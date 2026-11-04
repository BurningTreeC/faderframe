//! What a capture takes from the store: the newest stretch of sound on
//! the kept tracks that no recording or earlier capture took.
//!
//! Frames are input frames: what came in at frame `f` was played while
//! the player heard what the engine processed at `f − latency` (the
//! latency recording compensates), so `f` belongs to the run and pass of
//! the span holding `f − latency`.
//!
//! * The newest sound decides: heard in a playing run, the run is taken
//!   (back to the transport start); in a stopped run, the phrase back to a
//!   pause of [`PHRASE_GAP_SECONDS`]. A run the recorder took is not
//!   offered again.
//! * A playing run's passes are taken as they were heard, from a moment
//!   ([`LEAD_IN_SECONDS`]) before the first sound to a second after the
//!   last; passes with no sound are left out.
//! * Sound in a stopped run that carries on from the run before it (a
//!   chord still ringing when Stop was pressed, played on for less than
//!   [`TAIL_SECONDS`]) is that run's tail: the run before is taken.
//!   Played on longer, the phrase starts at the first sound after a pause
//!   of [`TAIL_QUIET_SECONDS`] in the stopped run.

use super::store::{Channel, GROUP, MARK, Span, Store};
use faderframe_core::TrackId;

/// A pause this long ends a phrase played while stopped (as Capture
/// MIDI's).
pub const PHRASE_GAP_SECONDS: f64 = crate::capture::PHRASE_GAP as f64 / 1e9;
/// Kept before the first sound (a breath, a pick's noise).
pub const LEAD_IN_SECONDS: f64 = 0.2;
/// Kept after the last sound of a stopped phrase or a playing run.
pub const TAIL_AFTER_SECONDS: f64 = 1.0;
/// Sound in a stopped run that started in the run before and lasts at
/// most this long is the earlier run's tail.
pub const TAIL_SECONDS: f64 = 10.0;
/// The pause that ends such a tail.
pub const TAIL_QUIET_SECONDS: f64 = 0.25;

/// A track kept and its device channels.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Kept {
    pub track: TrackId,
    pub channels: Vec<u16>,
}

/// One stretch of a playing run's pass, as heard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Heard {
    pub pass: u32,
    /// The engine frame processed at `position`: its input is read
    /// `latency` frames later.
    pub from: u64,
    /// Timeline position (engine samples).
    pub position: i64,
    pub frames: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Taken {
    /// Played while stopped: input frames `start..end`, the first sound
    /// at `onset`.
    Stopped { start: u64, onset: u64, end: u64 },
    /// Played while playing: the stretches of the run's passes heard,
    /// read `latency` frames later than processed.
    Playing { pieces: Vec<Heard>, latency: u64 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Phrase {
    pub taken: Taken,
    /// The tracks that sounded in it (in the order asked).
    pub tracks: Vec<TrackId>,
    /// The input frame after the last one taken (later captures start
    /// there).
    pub until: u64,
}

/// What a run is.
#[derive(Clone, Copy, Debug)]
struct Run {
    run: u32,
    /// Its first and after its last processed frame.
    start: u64,
    end: u64,
    playing: bool,
    recorded: bool,
}

struct Finder<'a> {
    store: &'a Store,
    /// The kept channels and their thresholds.
    channels: Vec<(&'a Channel, f32)>,
    /// Each track and its channels (indexes into `channels`).
    tracks: Vec<(TrackId, Vec<usize>)>,
    all: Vec<usize>,
    rate: f64,
}

impl Finder<'_> {
    /// Mark `m` sounds on one of `set`'s channels.
    fn sounds_in(&self, set: &[usize], m: u64) -> bool {
        set.iter().any(|&i| {
            let (c, t) = self.channels[i];
            c.mark(m).is_some_and(|p| p >= t)
        })
    }

    fn sound(&self, m: u64) -> bool {
        self.sounds_in(&self.all, m)
    }

    /// No mark of group `g` sounds on `set`'s channels.
    fn quiet_group(&self, set: &[usize], g: u64) -> bool {
        set.iter().all(|&i| {
            let (c, t) = self.channels[i];
            c.group(g).is_none_or(|p| p < t)
        })
    }

    /// The first (or with `newest`, the last) mark of `range` that sounds
    /// on `set`'s channels, passing over quiet groups whole.
    fn find_sound(&self, set: &[usize], range: std::ops::Range<u64>, newest: bool) -> Option<u64> {
        if range.is_empty() {
            return None;
        }
        if newest {
            let mut m = range.end - 1;
            loop {
                let g = m / GROUP;
                if m % GROUP == GROUP - 1 && g * GROUP >= range.start && self.quiet_group(set, g) {
                    m = (g * GROUP).checked_sub(1)?;
                } else if self.sounds_in(set, m) {
                    return Some(m);
                } else {
                    m = m.checked_sub(1)?;
                }
                if m < range.start {
                    return None;
                }
            }
        }
        let mut m = range.start;
        while m < range.end {
            let g = m / GROUP;
            if m.is_multiple_of(GROUP) && (g + 1) * GROUP <= range.end && self.quiet_group(set, g) {
                m = (g + 1) * GROUP;
            } else if self.sounds_in(set, m) {
                return Some(m);
            } else {
                m += 1;
            }
        }
        None
    }

    /// The tracks that sound anywhere in input frames `ranges`.
    fn sounding(&self, ranges: &[(u64, u64)]) -> Vec<TrackId> {
        self.tracks
            .iter()
            .filter(|(_, set)| {
                ranges.iter().any(|&(from, to)| {
                    self.find_sound(set, from / MARK..to.div_ceil(MARK), false)
                        .is_some()
                })
            })
            .map(|(t, _)| *t)
            .collect()
    }

    fn frames(&self, seconds: f64) -> u64 {
        (seconds * self.rate).round() as u64
    }

    fn run_of(&self, processed: u64) -> Option<Run> {
        let span = self.store.span_at(processed)?;
        Some(self.run(span.run))
    }

    fn run(&self, run: u32) -> Run {
        let spans = self.store.spans().iter().filter(|s| s.run == run);
        let mut r = Run {
            run,
            start: u64::MAX,
            end: 0,
            playing: false,
            recorded: false,
        };
        for s in spans {
            r.start = r.start.min(s.start);
            r.end = r.end.max(s.end());
            r.playing |= s.position.is_some();
            r.recorded |= s.recording;
        }
        r
    }

    /// The first sample at or after `from` in mark `m` above its
    /// channel's threshold.
    fn onset(&self, m: u64, from: u64) -> u64 {
        let a = (m * MARK).max(from);
        for f in a..(m + 1) * MARK {
            if self
                .channels
                .iter()
                .any(|(c, t)| c.sample(f).is_some_and(|s| s.abs() >= *t))
            {
                return f;
            }
        }
        a
    }
}

/// The phrase a capture takes now (see the module docs): `tracks` with
/// their channels, input before `taken` already taken, `latency_at(f)`
/// the latency in effect at input frame `f`.
pub fn find(
    store: &Store,
    tracks: &[Kept],
    taken: u64,
    latency_at: impl Fn(u64) -> u64,
) -> Option<Phrase> {
    let mut channels: Vec<(&Channel, f32)> = Vec::new();
    let mut sets = Vec::new();
    for t in tracks {
        let mut set = Vec::new();
        for &ch in &t.channels {
            let Some(c) = store.channel(ch) else { continue };
            let i = match channels.iter().position(|(x, _)| x.channel == ch) {
                Some(i) => i,
                None => {
                    channels.push((c, c.threshold()));
                    channels.len() - 1
                }
            };
            set.push(i);
        }
        sets.push((t.track, set));
    }
    let kept_from = channels.iter().map(|(c, _)| c.start()).min()?;
    let finder = Finder {
        store,
        all: (0..channels.len()).collect(),
        channels,
        tracks: sets,
        rate: f64::from(store.rate),
    };
    let lo = taken.max(kept_from);
    let hi = store.newest();
    if lo >= hi {
        return None;
    }
    // Marks wholly after what was taken.
    let m_lo = lo.div_ceil(MARK);
    let m_hi = (hi - 1) / MARK;
    if m_lo > m_hi {
        return None;
    }
    let newest = finder.find_sound(&finder.all, m_lo..m_hi + 1, true)?;
    let at = (newest * MARK).max(lo);
    let latency = latency_at(at);
    let run = finder.run_of(at.saturating_sub(latency));
    match run {
        Some(r) if r.recorded => None,
        Some(r) if r.playing => playing(&finder, r, lo, latency_at),
        _ => stopped(&finder, run, newest, lo, m_lo, latency, latency_at),
    }
}

fn stopped(
    f: &Finder,
    run: Option<Run>,
    newest: u64,
    lo: u64,
    m_lo: u64,
    latency: u64,
    latency_at: impl Fn(u64) -> u64,
) -> Option<Phrase> {
    // The run's first input frame (heard).
    let run_from = run.map_or(0, |r| r.start + latency);
    let gap = f.frames(PHRASE_GAP_SECONDS).max(MARK);
    let m_run = run_from.div_ceil(MARK).max(m_lo);
    let mut first = newest;
    let mut quiet = 0;
    let mut gapped = false;
    let mut m = newest;
    while m > m_run {
        m -= 1;
        if f.sound(m) {
            first = m;
            quiet = 0;
        } else {
            quiet += MARK;
            if quiet >= gap {
                gapped = true;
                break;
            }
        }
    }
    // Sound right at the run's start, carrying on from before it: the
    // run before's tail, unless sound comes after a pause.
    let carried = !gapped && first == m_run && run_from > lo && m_run > 0 && f.sound(m_run - 1);
    if let Some(r) = run.filter(|_| carried) {
        let pause = f.frames(TAIL_QUIET_SECONDS).max(MARK);
        let mut quiet = 0;
        let mut after = None;
        for m in m_run..=newest {
            if f.sound(m) {
                if quiet >= pause {
                    after = Some(m);
                    break;
                }
                quiet = 0;
            } else {
                quiet += MARK;
            }
        }
        match after {
            Some(m) => first = m,
            None => {
                let before = (r.start > 0).then(|| f.run_of(r.start - 1)).flatten();
                let short = (newest + 1) * MARK - run_from <= f.frames(TAIL_SECONDS);
                match before {
                    Some(b) if short && b.recorded => return None,
                    Some(b) if short && b.playing => return playing(f, b, lo, latency_at),
                    // Played on and on: from the run's start.
                    _ => {}
                }
            }
        }
    }
    let onset = f.onset(first, lo);
    let start = onset
        .saturating_sub(f.frames(LEAD_IN_SECONDS))
        .max(lo)
        .max(run_from.min(onset));
    let end = ((newest + 1) * MARK + f.frames(TAIL_AFTER_SECONDS)).min(f.store.newest());
    let tracks = f.sounding(&[(onset, end)]);
    Some(Phrase {
        taken: Taken::Stopped { start, onset, end },
        tracks,
        until: end,
    })
}

fn playing(f: &Finder, run: Run, lo: u64, latency_at: impl Fn(u64) -> u64) -> Option<Phrase> {
    let latency = latency_at(run.start);
    let spans: Vec<Span> = f
        .store
        .spans()
        .iter()
        .filter(|s| s.run == run.run && s.position.is_some())
        .copied()
        .collect();
    // The run as heard, less what was taken.
    let a = (run.start + latency).max(lo);
    let b = (run.end + latency).min(f.store.newest());
    if a >= b {
        return None;
    }
    let first = f.find_sound(&f.all, a.div_ceil(MARK)..b.div_ceil(MARK), false)?;
    let last = f.find_sound(&f.all, a / MARK..b.div_ceil(MARK), true)?;
    let from = (first * MARK)
        .max(a)
        .saturating_sub(f.frames(LEAD_IN_SECONDS))
        .max(a);
    let to = ((last + 1) * MARK + f.frames(TAIL_AFTER_SECONDS)).min(b);
    let mut pieces = Vec::new();
    for s in spans {
        let heard_from = (s.start + latency).max(from);
        let heard_to = (s.end() + latency).min(to);
        if heard_from >= heard_to {
            continue;
        }
        let sounds = f
            .find_sound(&f.all, heard_from / MARK..heard_to.div_ceil(MARK), false)
            .is_some();
        if !sounds {
            continue;
        }
        let processed = heard_from - latency;
        let Some(position) = s.position_at(processed) else {
            continue;
        };
        pieces.push(Heard {
            pass: s.pass,
            from: processed,
            position,
            frames: heard_to - heard_from,
        });
    }
    if pieces.is_empty() {
        return None;
    }
    let until = pieces
        .iter()
        .map(|p| p.from + p.frames + latency)
        .max()
        .unwrap_or(to);
    let heard: Vec<(u64, u64)> = pieces
        .iter()
        .map(|p| (p.from + latency, p.from + latency + p.frames))
        .collect();
    let tracks = f.sounding(&heard);
    Some(Phrase {
        taken: Taken::Playing { pieces, latency },
        tracks,
        until,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    const SR: u32 = 1_000 * MARK as u32 / 10; // 25.6 kHz: a mark is 10 ms

    /// Builds a store from a script of blocks: (frames, sound, position).
    struct Script {
        store: Store,
        frame: u64,
    }

    impl Script {
        fn new() -> Self {
            let mut store = Store::new(SR, 600.0, 1 << 30);
            store.set_channels(&[0, 1]);
            Self { store, frame: 0 }
        }

        /// `seconds` of input: `level` on channel 0 (channel 1 silent),
        /// stopped or playing from `position`.
        fn add(&mut self, seconds: f64, level: f32, position: Option<i64>) -> &mut Self {
            self.add_on(seconds, [level, 0.0], position, false)
        }

        fn add_on(
            &mut self,
            seconds: f64,
            levels: [f32; 2],
            position: Option<i64>,
            recording: bool,
        ) -> &mut Self {
            let n = (seconds * f64::from(SR)).round() as usize;
            let block = 256;
            let mut done = 0;
            while done < n {
                let k = block.min(n - done);
                let mut data = vec![levels[0]; k];
                data.extend(std::iter::repeat_n(levels[1], k));
                self.store.push(
                    self.frame,
                    k,
                    position.map(|p| p + done as i64),
                    recording,
                    &[0, 1],
                    Some(&data),
                    &mut Vec::new(),
                );
                self.frame += k as u64;
                done += k;
            }
            self
        }
    }

    const A: TrackId = TrackId(1);
    const B: TrackId = TrackId(2);

    fn tracks() -> Vec<Kept> {
        vec![
            Kept {
                track: A,
                channels: vec![0],
            },
            Kept {
                track: B,
                channels: vec![1],
            },
        ]
    }

    fn secs(f: u64) -> f64 {
        f as f64 / f64::from(SR)
    }

    #[test]
    fn nothing_sounding_is_nothing_to_capture() {
        let mut s = Script::new();
        s.add(5.0, 0.0, None).add(3.0, 0.0, Some(0));
        assert_eq!(find(&s.store, &tracks(), 0, |_| 0), None);
    }

    #[test]
    fn a_phrase_played_while_stopped_goes_back_to_a_pause() {
        let mut s = Script::new();
        // An old phrase, a long pause, then a phrase with short rests.
        s.add(2.0, 0.0, None)
            .add(1.0, 0.5, None)
            .add(9.0, 0.0, None)
            .add(1.0, 0.5, None)
            .add(3.0, 0.0, None)
            .add(1.0, 0.5, None)
            .add(2.0, 0.0, None);
        let p = find(&s.store, &tracks(), 0, |_| 0).unwrap();
        let Taken::Stopped { start, onset, end } = p.taken else {
            panic!("{p:?}")
        };
        assert_eq!(secs(onset), 12.0, "the phrase after the long pause");
        assert_eq!(secs(start), 12.0 - LEAD_IN_SECONDS);
        assert!(
            (secs(end) - 17.0 - TAIL_AFTER_SECONDS).abs() < 0.011,
            "{}",
            secs(end)
        );
        assert_eq!(p.tracks, [A], "B was silent");
        // Taken: the older phrase is not offered after it.
        assert_eq!(find(&s.store, &tracks(), p.until, |_| 0), None);
    }

    #[test]
    fn a_playing_run_is_taken_as_heard_with_its_latency() {
        let mut s = Script::new();
        // Four marks.
        let latency = 1_024;
        let n = |seconds: f64| (seconds * f64::from(SR)).round() as i64;
        let lat = latency as f64 / f64::from(SR);
        // Stopped, then playing from 48 000 for 4 s: the input sounds from
        // 1 s to 3 s of what was heard (arriving `latency` later).
        s.add(1.0, 0.0, None);
        let run_start = s.frame;
        s.add(1.0 + lat, 0.0, Some(48_000))
            .add(2.0, 0.5, Some(48_000 + n(1.0 + lat)))
            .add(1.0 - lat, 0.0, Some(48_000 + n(3.0 + lat)))
            .add(3.0, 0.0, None);
        let p = find(&s.store, &tracks(), 0, |_| latency).unwrap();
        let Taken::Playing { pieces, latency: l } = &p.taken else {
            panic!("{p:?}")
        };
        assert_eq!(*l, latency);
        assert_eq!(pieces.len(), 1, "{pieces:?}");
        let h = pieces[0];
        // From the lead-in before the sound heard 1 s in.
        let sound = run_start + n(1.0 + lat) as u64;
        assert_eq!(h.from + latency, sound - n(LEAD_IN_SECONDS) as u64);
        assert_eq!(h.position, 48_000 + (h.from - run_start) as i64);
        // To a second after it stopped, which is where the run ended as
        // heard.
        assert_eq!(h.from + h.frames, run_start + n(4.0) as u64);
        assert_eq!(p.until, h.from + h.frames + latency);
        assert_eq!(p.tracks, [A]);
    }

    #[test]
    fn passes_without_sound_are_left_out_and_each_keeps_its_place() {
        let mut s = Script::new();
        s.add(0.5, 0.0, None);
        // A loop of 1 s at timeline 0: four passes; sound in passes 2 and 3.
        s.add(1.0, 0.0, Some(0))
            .add(1.0, 0.5, Some(0))
            .add(1.0, 0.5, Some(0))
            .add(1.0, 0.0, Some(0))
            .add(2.0, 0.0, None);
        let p = find(&s.store, &tracks(), 0, |_| 0).unwrap();
        let Taken::Playing { pieces, .. } = &p.taken else {
            panic!("{p:?}")
        };
        let passes: Vec<u32> = pieces.iter().map(|h| h.pass).collect();
        // The lead-in at the end of the first pass and the second after
        // the last sound in the fourth are silent.
        assert_eq!(passes, [1, 2]);
        for h in pieces {
            // Each piece's position is its place in the loop.
            assert!(h.position >= 0 && h.position < i64::from(SR), "{h:?}");
            let span = s.store.span_at(h.from).unwrap();
            assert_eq!(span.position_at(h.from), Some(h.position));
        }
    }

    #[test]
    fn a_tail_ringing_into_the_stop_belongs_to_the_run_before() {
        let mut s = Script::new();
        s.add(1.0, 0.0, None)
            .add(2.0, 0.5, Some(0))
            // Still sounding after Stop, then quiet.
            .add(0.5, 0.5, None)
            .add(1.0, 0.0, None);
        let p = find(&s.store, &tracks(), 0, |_| 0).unwrap();
        assert!(matches!(p.taken, Taken::Playing { .. }), "{p:?}");
        // Played on after Stop for longer than a tail: a phrase of its own
        // from the first sound after a pause.
        let mut s = Script::new();
        s.add(1.0, 0.0, None)
            .add(2.0, 0.5, Some(0))
            .add(0.5, 0.5, None)
            .add(0.5, 0.0, None)
            .add(11.0, 0.5, None);
        let p = find(&s.store, &tracks(), 0, |_| 0).unwrap();
        let Taken::Stopped { onset, .. } = p.taken else {
            panic!("{p:?}")
        };
        assert_eq!(secs(onset), 4.0);
    }

    #[test]
    fn a_recorded_run_is_not_offered_again() {
        let mut s = Script::new();
        s.add(1.0, 0.0, None)
            .add_on(2.0, [0.5, 0.5], Some(0), true)
            .add(1.0, 0.0, None);
        assert_eq!(find(&s.store, &tracks(), 0, |_| 0), None);
        // Its tail after Stop neither.
        let mut s = Script::new();
        s.add(1.0, 0.0, None)
            .add_on(2.0, [0.5, 0.0], Some(0), true)
            .add(0.3, 0.5, None)
            .add(1.0, 0.0, None);
        assert_eq!(find(&s.store, &tracks(), 0, |_| 0), None);
        // Played while stopped after it: that phrase.
        s.add(1.0, 0.5, None);
        let p = find(&s.store, &tracks(), 0, |_| 0).unwrap();
        assert!(matches!(p.taken, Taken::Stopped { .. }), "{p:?}");
    }

    #[test]
    fn quiet_minutes_are_passed_over_and_a_short_sound_found_to_the_frame() {
        let n = |seconds: f64| (seconds * f64::from(SR)).round() as u64;
        // Minutes of silence round a 20 ms sound, stopped.
        let mut s = Script::new();
        s.add(123.45, 0.0, None)
            .add(0.02, 0.5, None)
            .add(176.53, 0.0, None);
        let p = find(&s.store, &tracks(), 0, |_| 0).unwrap();
        let Taken::Stopped { onset, end, .. } = p.taken else {
            panic!("{p:?}")
        };
        assert_eq!(onset, n(123.45));
        assert_eq!(end, n(123.47) + n(TAIL_AFTER_SECONDS));
        // And playing: the run from its first sound to a second after its
        // last, whole minutes in.
        let mut s = Script::new();
        s.add(1.0, 0.0, None)
            .add(200.0, 0.0, Some(0))
            .add(0.02, 0.5, Some(n(200.0) as i64))
            .add(99.98, 0.0, Some(n(200.02) as i64))
            .add(1.0, 0.0, None);
        let p = find(&s.store, &tracks(), 0, |_| 0).unwrap();
        let Taken::Playing { pieces, .. } = &p.taken else {
            panic!("{p:?}")
        };
        assert_eq!(pieces.len(), 1);
        let h = pieces[0];
        assert_eq!(h.position, n(200.0 - LEAD_IN_SECONDS) as i64);
        assert_eq!(h.frames, n(LEAD_IN_SECONDS + 0.02 + TAIL_AFTER_SECONDS));
    }

    #[test]
    fn the_tracks_are_those_that_sounded() {
        let mut s = Script::new();
        s.add(1.0, 0.0, None)
            .add_on(1.0, [0.0, 0.5], None, false)
            .add(1.0, 0.0, None);
        let p = find(&s.store, &tracks(), 0, |_| 0).unwrap();
        assert_eq!(p.tracks, [B]);
        let both = find(
            &s.store,
            &[Kept {
                track: A,
                channels: vec![0, 1],
            }],
            0,
            |_| 0,
        )
        .unwrap();
        assert_eq!(both.tracks, [A], "a stereo track sounding on its right");
    }
}
