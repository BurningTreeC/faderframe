use super::psola::PsolaVoice;
use crate::context::EngineContext;
use crate::snapshot::{AudioRegion, Source, WarpMode, WarpedRegion};
use faderframe_audio_graph::{
    AudioBuffer, NodeIo, ProcessContext, Processor, for_each_channel_route,
};
use faderframe_core::TrackId;
use faderframe_stretch::{MAX_CHANNELS, Preset, Stretcher};

/// Plays the audio regions of one track's lane from the timeline snapshot.
///
/// In-memory sources are read directly. Streamed sources are read from
/// their resident pages; a page the disk loader has not provided yet plays
/// as silence and is counted as a miss, the realtime thread never waits for
/// the disk. Silent while the transport is stopped.
///
/// Warped clips play through their time map: varispeed by interpolation,
/// pitch-preserving modes through [`Stretcher`] voices preallocated when
/// the node is built (the voice count is part of the node's key, so a
/// track that newly needs voices gets a rebuilt node). A voice stays bound
/// to its clip while playback continues seamlessly and is primed again
/// after any jump; with no voice free, a clip falls back to varispeed.
pub struct AudioClipPlayer {
    track: TrackId,
    latency: u32,
    voices: Vec<StretchVoice>,
    /// For pitch-edited clips.
    psola: Vec<PsolaVoice>,
    /// Blocks processed (voices free up when their clip stopped playing).
    cycle: u64,
}

/// Voices a track's clip player gets.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct StretchVoices {
    pub polyphonic: usize,
    pub rhythmic: usize,
    /// PSOLA voices (pitch-edited clips).
    pub psola: usize,
    /// Channels per voice.
    pub channels: usize,
}

impl StretchVoices {
    pub fn is_empty(&self) -> bool {
        self.polyphonic + self.rhythmic + self.psola == 0
    }

    /// For the node key.
    pub fn key(&self) -> u64 {
        (self.polyphonic as u64)
            | (self.rhythmic as u64) << 8
            | (self.channels as u64) << 16
            | (self.psola as u64) << 24
    }
}

/// Fastest warp the stretcher follows (faster plays varispeed).
const MAX_RATE: f64 = 8.0;

struct StretchVoice {
    stretcher: Stretcher,
    /// Clip key and the next timeline frame this voice will produce.
    bound: Option<(u64, i64)>,
    /// Next source frame to feed.
    feed: i64,
    last_cycle: u64,
    input: Vec<Vec<f32>>,
    output: Vec<Vec<f32>>,
}

impl StretchVoice {
    fn new(channels: usize, sample_rate: f64, preset: Preset, max_block: usize) -> Option<Self> {
        let stretcher = Stretcher::new(channels, sample_rate, preset)?;
        let out_len = max_block.max(stretcher.output_latency()).max(1);
        let in_len = stretcher
            .seek_length()
            .max((out_len as f64 * MAX_RATE) as usize + 64);
        Some(Self {
            input: vec![vec![0.0; in_len]; channels],
            output: vec![vec![0.0; out_len]; channels],
            stretcher,
            bound: None,
            feed: 0,
            last_cycle: 0,
        })
    }

    /// Fill the input buffers with source frames `[from, from + n)`.
    fn read(&mut self, w: &WarpedRegion, from: i64, n: usize) {
        read_source(w, from, n, &mut self.input);
    }

    /// Run the stretcher for `outputs` frames, feeding the source up to
    /// `target` (input frames are capped by the buffer; beyond that the
    /// feed skips ahead).
    fn run(&mut self, w: &WarpedRegion, target: f64, outputs: usize) {
        let target = target.floor() as i64;
        let cap = self.input[0].len();
        let n = (target - self.feed).clamp(0, cap as i64) as usize;
        self.read(w, self.feed, n);
        self.feed = target.max(self.feed + n as i64);
        let ins: [&[f32]; MAX_CHANNELS] =
            std::array::from_fn(|c| self.input.get(c).map_or(&[][..], |v| &v[..n]));
        let channels = self.stretcher.channels();
        let mut outs: [&mut [f32]; MAX_CHANNELS] = Default::default();
        for (o, v) in outs.iter_mut().zip(self.output.iter_mut()) {
            *o = &mut v[..outputs];
        }
        self.stretcher
            .process(&ins[..channels], n, &mut outs[..channels], outputs);
    }

    /// Prepare to produce output frame `rel` of `w` next: pre-roll the
    /// analysis and run (and discard) the output latency, so the next
    /// output is exactly `w.source_at(rel)` (the stretcher's contract:
    /// output at `u` comes from the analysis at `source_at(u + Lo)`, which
    /// is fed up to `+ Li`).
    fn prime(&mut self, w: &WarpedRegion, rel: i64) {
        let li = self.stretcher.input_latency() as f64;
        let lo = self.stretcher.output_latency() as i64;
        self.stretcher.reset();
        self.stretcher.set_transpose(w.transpose);
        let end = (w.source_at(rel as f64) + li).floor() as i64;
        let pre = self.stretcher.seek_length().min(self.input[0].len());
        self.read(w, end - pre as i64, pre);
        let ins: [&[f32]; MAX_CHANNELS] =
            std::array::from_fn(|c| self.input.get(c).map_or(&[][..], |v| &v[..pre]));
        let channels = self.stretcher.channels();
        self.stretcher.seek(
            &ins[..channels],
            w.rate_at(rel as f64).clamp(1.0 / MAX_RATE, MAX_RATE),
        );
        self.feed = end;
        let mut done = 0;
        let chunk = self.output[0].len() as i64;
        while done < lo {
            let n = chunk.min(lo - done);
            let target = w.source_at((rel + done + n) as f64) + li;
            self.run(w, target, n as usize);
            done += n;
        }
    }
}

/// Fill `bufs` with source frames `[from, from + n)` of `w` (zero outside
/// the clip's source range or a missing page). Realtime-safe.
pub(crate) fn read_source(w: &WarpedRegion, from: i64, n: usize, bufs: &mut [Vec<f32>]) {
    let lo = w.source_lo.floor() as i64;
    let hi = (w.source_hi.ceil() as i64).min(w.region.source.frames());
    let src_ch = w.region.source.channels();
    for (c, buf) in bufs.iter_mut().enumerate() {
        let n = n.min(buf.len());
        let buf = &mut buf[..n];
        buf.fill(0.0);
        if c >= src_ch {
            continue;
        }
        let a = from.max(lo).max(0);
        let b = (from + n as i64).min(hi);
        if b <= a {
            continue;
        }
        let off = (a - from) as usize;
        let len = (b - a) as usize;
        match &w.region.source {
            Source::Memory(d) => {
                let ch = d.channel(c);
                buf[off..off + len].copy_from_slice(&ch[a as usize..a as usize + len]);
            }
            Source::Stream(s) => s.read_segments(c, a, len, |o, k, seg| {
                if let Some(seg) = seg {
                    buf[off + o..off + o + k].copy_from_slice(&seg[..k]);
                }
            }),
        }
    }
}

impl AudioClipPlayer {
    pub fn new(track: TrackId) -> Self {
        Self {
            track,
            voices: Vec::new(),
            psola: Vec::new(),
            cycle: 0,
            latency: 0,
        }
    }

    /// Report delay already baked into frozen audio; do not delay it again.
    pub fn with_latency(mut self, latency: u32) -> Self {
        self.latency = latency;
        self
    }

    /// With stretcher voices (allocates; control thread).
    pub fn with_voices(
        track: TrackId,
        v: StretchVoices,
        sample_rate: f64,
        max_block: usize,
    ) -> Self {
        let mut voices = Vec::with_capacity(v.polyphonic + v.rhythmic);
        let channels = v.channels.clamp(1, MAX_CHANNELS);
        for (preset, n) in [
            (Preset::Polyphonic, v.polyphonic),
            (Preset::Rhythmic, v.rhythmic),
        ] {
            for _ in 0..n {
                if let Some(voice) = StretchVoice::new(channels, sample_rate, preset, max_block) {
                    voices.push(voice);
                }
            }
        }
        let psola = (0..v.psola)
            .map(|_| PsolaVoice::new(channels, sample_rate, max_block))
            .collect();
        Self {
            track,
            voices,
            psola,
            cycle: 0,
            latency: 0,
        }
    }

    /// The PSOLA voice playing `w` (or a free one).
    fn psola_for(&mut self, w: &WarpedRegion) -> Option<usize> {
        let fits = |v: &PsolaVoice| v.channels() >= w.region.source.channels();
        if let Some(i) = self
            .psola
            .iter()
            .position(|v| v.bound.is_some_and(|(k, _)| k == w.key) && fits(v))
        {
            return Some(i);
        }
        let cycle = self.cycle;
        let i = self
            .psola
            .iter()
            .position(|v| fits(v) && (v.bound.is_none() || v.last_cycle + 1 < cycle))?;
        self.psola[i].bound = None;
        Some(i)
    }

    /// The voice playing `w` (or a free one of its preset).
    fn voice_for(&mut self, w: &WarpedRegion, preset: Preset) -> Option<usize> {
        let fits = |v: &StretchVoice| {
            v.stretcher.preset() == preset && v.stretcher.channels() >= w.region.source.channels()
        };
        if let Some(i) = self
            .voices
            .iter()
            .position(|v| v.bound.is_some_and(|(k, _)| k == w.key) && fits(v))
        {
            return Some(i);
        }
        let cycle = self.cycle;
        let i = self
            .voices
            .iter()
            .position(|v| fits(v) && (v.bound.is_none() || v.last_cycle + 1 < cycle))?;
        self.voices[i].bound = None;
        Some(i)
    }
}

/// Mix `region` into `out` for timeline samples `[a, b)` of the block
/// starting at `pos`.
#[inline]
fn play_region(region: &AudioRegion, out: &mut AudioBuffer, pos: i64, a: i64, b: i64) {
    let out_ch = out.num_channels();
    let src_ch = region.source.channels();
    let len_src = region.source.frames();
    match &region.source {
        Source::Memory(data) if region.step == 1.0 => {
            for_each_channel_route(src_ch, out_ch, |s, d, w| {
                let src = data.channel(s);
                let dst = out.channel_mut(d);
                for t in a..b {
                    let rel = t - region.start;
                    let frame = if region.reversed {
                        region.source_start + (region.end - region.start) - 1 - rel
                    } else {
                        region.source_start + rel
                    };
                    if frame < 0 || frame >= len_src {
                        continue;
                    }
                    dst[(t - pos) as usize] += src[frame as usize] * region.gain_at(t) * w;
                }
            });
        }
        Source::Stream(stream) if region.step == 1.0 && !region.reversed => {
            for_each_channel_route(src_ch, out_ch, |s, d, w| {
                let dst = out.channel_mut(d);
                stream.read_segments(
                    s,
                    region.source_start + (a - region.start),
                    (b - a) as usize,
                    |off, n, seg| {
                        if let Some(seg) = seg {
                            for (k, v) in seg[..n].iter().enumerate() {
                                let t = a + (off + k) as i64;
                                dst[(t - pos) as usize] += v * region.gain_at(t) * w;
                            }
                        }
                    },
                );
            });
        }
        _ => {
            // The source rate differs from the engine rate (or reversed):
            // linear interpolation between resident samples.
            let span = ((region.end - region.start) as f64 * region.step).ceil() as i64;
            for_each_channel_route(src_ch, out_ch, |s, d, w| {
                let dst = out.channel_mut(d);
                for t in a..b {
                    let x = (t - region.start) as f64 * region.step;
                    let x = if region.reversed {
                        (span - 1) as f64 - x
                    } else {
                        x
                    };
                    let i0 = x.floor() as i64;
                    let frac = (x - i0 as f64) as f32;
                    let f0 = region.source_start + i0;
                    let (Some(s0), s1) = (
                        sample(&region.source, s, f0),
                        sample(&region.source, s, f0 + 1),
                    ) else {
                        continue;
                    };
                    let v = s0 + (s1.unwrap_or(s0) - s0) * frac;
                    dst[(t - pos) as usize] += v * region.gain_at(t) * w;
                }
            });
        }
    }
}

/// One source sample (`None` outside the source or on a missing page).
#[inline]
fn sample(source: &Source, ch: usize, frame: i64) -> Option<f32> {
    match source {
        Source::Memory(d) => {
            (frame >= 0 && frame < d.frames() as i64).then(|| d.channel(ch)[frame as usize])
        }
        Source::Stream(s) => s.sample(ch, frame),
    }
}

/// A warped clip resampled along its time map (pitch follows the speed).
fn play_varispeed(w: &WarpedRegion, out: &mut AudioBuffer, pos: i64, a: i64, b: i64) {
    let region = &w.region;
    let (lo, hi) = (w.source_lo, w.source_hi);
    for_each_channel_route(region.source.channels(), out.num_channels(), |s, d, g| {
        let dst = out.channel_mut(d);
        for t in a..b {
            let x = w.source_at((t - region.start) as f64);
            if x < lo || x >= hi {
                continue;
            }
            let i0 = x.floor() as i64;
            let frac = (x - i0 as f64) as f32;
            let Some(s0) = sample(&region.source, s, i0) else {
                continue;
            };
            let s1 = sample(&region.source, s, i0 + 1).unwrap_or(s0);
            dst[(t - pos) as usize] += (s0 + (s1 - s0) * frac) * region.gain_at(t) * g;
        }
    });
}

impl Processor<EngineContext> for AudioClipPlayer {
    fn latency(&self) -> u32 {
        self.latency
    }
    fn process(&mut self, cx: &ProcessContext<'_, EngineContext>, io: &mut NodeIo<'_>) {
        let Some(out) = io.audio_out.first_mut() else {
            return;
        };
        out.clear();
        self.cycle += 1;
        let t = &cx.data.transport;
        if !t.playing {
            return;
        }
        let Some(lane) = cx.data.timeline.lane(self.track) else {
            return;
        };
        let pos = t.sample_position;
        let end = pos + io.frames as i64;
        // Regions are sorted by start; everything starting at/after `end` is
        // irrelevant for this block.
        let upto = lane.audio.partition_point(|r| r.start < end);
        for region in lane.audio[..upto].iter().filter(|r| r.end > pos) {
            let a = pos.max(region.start);
            let b = end.min(region.end);
            play_region(region, out, pos, a, b);
        }
        let upto = lane.warped.partition_point(|w| w.region.start < end);
        for w in lane.warped[..upto].iter().filter(|w| w.region.end > pos) {
            let a = pos.max(w.region.start);
            let b = end.min(w.region.end);
            if let (WarpMode::Psola, Some(curve)) = (w.mode, &w.pitch) {
                let Some(vi) = self.psola_for(w) else {
                    play_varispeed(w, out, pos, a, b);
                    continue;
                };
                let cycle = self.cycle;
                let v = &mut self.psola[vi];
                let n = (b - a) as usize;
                v.process(w, curve, a - w.region.start, n);
                v.last_cycle = cycle;
                let src_ch = w.region.source.channels().min(v.channels());
                let output = &v.output;
                for_each_channel_route(src_ch, out.num_channels(), |s, d, g| {
                    let dst = out.channel_mut(d);
                    for (k, x) in output[s][..n].iter().enumerate() {
                        let t = a + k as i64;
                        dst[(t - pos) as usize] += x * w.region.gain_at(t) * g;
                    }
                });
                continue;
            }
            let voice = match w.mode {
                WarpMode::Stretch(preset) => self.voice_for(w, preset),
                WarpMode::Varispeed | WarpMode::Psola => None,
            };
            let Some(vi) = voice else {
                play_varispeed(w, out, pos, a, b);
                continue;
            };
            let cycle = self.cycle;
            let v = &mut self.voices[vi];
            let rel_a = a - w.region.start;
            if v.bound != Some((w.key, a)) {
                v.prime(w, rel_a);
            }
            let n = (b - a) as usize;
            let li = v.stretcher.input_latency() as f64;
            let lo = v.stretcher.output_latency() as f64;
            let target = w.source_at((b - w.region.start) as f64 + lo) + li;
            v.run(w, target, n);
            v.bound = Some((w.key, b));
            v.last_cycle = cycle;
            let src_ch = w.region.source.channels().min(v.stretcher.channels());
            let output = &v.output;
            for_each_channel_route(src_ch, out.num_channels(), |s, d, g| {
                let dst = out.channel_mut(d);
                for (k, x) in output[s][..n].iter().enumerate() {
                    let t = a + k as i64;
                    dst[(t - pos) as usize] += x * w.region.gain_at(t) * g;
                }
            });
        }
    }
}
