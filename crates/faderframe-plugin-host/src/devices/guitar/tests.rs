use super::processor::GuitarProcessor;
use super::stages::Run;
use super::*;
use crate::tap::AnalysisTap;
use crate::{PluginProcessContext, PluginProcessor, ProcessConfig, ProcessStatus};
use faderframe_audio_graph::{AudioBuffer, NodeIo};
use faderframe_automation::ParameterEvent;
use faderframe_core::{ChannelLayout, ParameterId};
use faderframe_transport::TransportInfo;
use std::sync::Arc;

const SR: f64 = 48_000.0;
const MAX_BLOCK: usize = 512;

fn config() -> ProcessConfig {
    ProcessConfig {
        sample_rate: SR,
        max_block_size: MAX_BLOCK as u32,
        sidechain: false,
        double_precision: false,
    }
}

/// A plucked note at about the level a guitar is tracked at.
fn pluck(n: usize) -> f32 {
    let t = n as f64 / SR;
    let since = (t * 3.0).fract() / 3.0;
    let env = (-since * 7.0).exp();
    (0.2 * env
        * ((std::f64::consts::TAU * 110.0 * t).sin()
            + 0.4 * (std::f64::consts::TAU * 220.0 * t).sin()
            + 0.2 * (std::f64::consts::TAU * 330.0 * t).sin())) as f32
}

fn params(set: &[(u32, f64)]) -> ParamValues {
    let p = ParamValues::new(parameters());
    for (id, v) in set {
        p.set_by_id(ParameterId(*id), *v).unwrap();
    }
    p
}

fn stomp(s: Stomp) -> f64 {
    s.index() as f64
}

struct Line {
    p: GuitarProcessor,
    params: ParamValues,
    tap: Arc<AnalysisTap>,
    ins: Vec<AudioBuffer>,
    outs: Vec<AudioBuffer>,
    at: usize,
}

impl Line {
    fn new(set: &[(u32, f64)], channels: usize, run: Run, device_block: usize) -> Self {
        let params = params(set);
        let tap = Arc::new(AnalysisTap::new(params.clone(), TAP_VALUES));
        let p = GuitarProcessor::with_run(
            params.clone(),
            Some(Arc::clone(&tap)),
            &config(),
            channels,
            run,
            device_block,
        )
        .unwrap();
        let layout = ChannelLayout::from_channel_count(channels);
        Self {
            p,
            params,
            tap,
            ins: vec![AudioBuffer::new(layout, MAX_BLOCK)],
            outs: vec![
                AudioBuffer::new(layout, MAX_BLOCK),
                AudioBuffer::new(layout, MAX_BLOCK),
            ],
            at: 0,
        }
    }

    /// One block of `signal(frame, channel)`; the main and DI outputs.
    fn block(
        &mut self,
        len: usize,
        events: &[ParameterEvent],
        signal: impl Fn(usize, usize) -> f32,
    ) -> (Vec<Vec<f32>>, Vec<Vec<f32>>) {
        for b in self.ins.iter_mut().chain(self.outs.iter_mut()) {
            b.set_len(len);
        }
        let channels = self.ins[0].num_channels();
        for c in 0..channels {
            for (i, x) in self.ins[0].channel_mut(c).iter_mut().enumerate() {
                *x = signal(self.at + i, c);
            }
        }
        let transport = TransportInfo::default();
        let ctx = PluginProcessContext {
            transport: &transport,
            param_events: events,
            harmony: &crate::NO_HARMONY,
            param_mods: &[],
            note_mods: &[],
        };
        let status = self.p.process(
            &ctx,
            &mut NodeIo {
                frames: len,
                audio_in: &self.ins,
                audio_out: &mut self.outs,
                events_in: &[],
                events_out: &mut [],
            },
        );
        assert_eq!(status, ProcessStatus::Continue);
        self.at += len;
        let take = |b: &AudioBuffer| (0..channels).map(|c| b.channel(c).to_vec()).collect();
        (take(&self.outs[0]), take(&self.outs[1]))
    }

    /// `seconds` of the pluck on every channel, in 256-frame blocks.
    fn play(&mut self, seconds: f64) -> (Vec<Vec<f32>>, Vec<Vec<f32>>) {
        let channels = self.ins[0].num_channels();
        let (mut main, mut di) = (vec![Vec::new(); channels], vec![Vec::new(); channels]);
        let blocks = (seconds * SR / 256.0).ceil() as usize;
        for _ in 0..blocks {
            let (m, d) = self.block(256, &[], |n, _| pluck(n));
            for c in 0..channels {
                main[c].extend_from_slice(&m[c]);
                di[c].extend_from_slice(&d[c]);
            }
        }
        (main, di)
    }
}

fn rms_db(x: &[f32]) -> f64 {
    let ms = x.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / x.len().max(1) as f64;
    10.0 * ms.max(1e-30).log10()
}

#[test]
fn every_added_pedal_is_a_stage_of_reported_latency() {
    let empty = params(&[]);
    assert_eq!(pedal_count(&empty), 0);
    assert_eq!(latency(&empty, 256), 256, "the amplifier's buffer");
    let two = params(&[
        (
            id::slot(0, id::STOMP),
            stomp(Stomp::Pedal(faderframe_guitar::voice::Pedal::Green808)),
        ),
        (
            id::slot(3, id::STOMP),
            stomp(Stomp::Wah(faderframe_guitar::circuits::wah::Build::CryBaby)),
        ),
        // A footswitch does not change it.
        (id::slot(3, id::ON), 0.0),
    ]);
    assert_eq!(pedal_count(&two), 2);
    assert_eq!(latency(&two, 256), 3 * 256);
    // Small device blocks: at least the reservoir's least.
    assert_eq!(latency(&two, 32), 3 * 128);
    // 2x adds the oversampler's round trip to every stage.
    let hq = params(&[(id::QUALITY, 1.0)]);
    assert_eq!(
        latency(&hq, 256),
        256 + faderframe_guitar::pedal::stage_latency(2)
    );
    let line = Line::new(&[(id::slot(0, id::STOMP), 1.0)], 1, Run::Inline, 256);
    assert_eq!(line.p.pedals(), 1);
}

#[test]
fn every_amplifier_and_pedal_makes_sound() {
    use faderframe_guitar::chain::AMPS;
    let mut line = Line::new(&[], 1, Run::Inline, 128);
    for (i, _) in AMPS.iter().enumerate() {
        line.params
            .set_by_id(ParameterId(id::AMP), i as f64)
            .unwrap();
        let (main, _) = line.play(0.25);
        let tail = &main[0][main[0].len() / 2..];
        assert!(tail.iter().all(|x| x.is_finite()));
        let level = rms_db(tail);
        assert!(
            level > -60.0 && level < 6.0,
            "{}: {level:.1} dB",
            faderframe_guitar::chain::amp_name(AMPS[i])
        );
    }
    // The pedals, one place on the line, changed while playing: the
    // workshop builds each (offline the stage waits for it).
    let mut line = Line::new(&[(id::slot(0, id::STOMP), 1.0)], 1, Run::Inline, 128);
    for s in Stomp::ALL.into_iter().skip(1) {
        line.params
            .set_by_id(ParameterId(id::slot(0, id::STOMP)), stomp(s))
            .unwrap();
        let (main, _) = line.play(0.25);
        assert_eq!(line.p.installed(), vec![s]);
        let tail = &main[0][main[0].len() / 2..];
        assert!(tail.iter().all(|x| x.is_finite()), "{}", s.name());
        let level = rms_db(tail);
        assert!(level > -60.0 && level < 6.0, "{}: {level:.1} dB", s.name());
    }
}

#[test]
fn workers_match_inline_with_pedals_automation_and_resets() {
    use faderframe_guitar::circuits::wah;
    use faderframe_guitar::voice::Pedal;
    let set = [
        (id::slot(0, id::STOMP), stomp(Stomp::Wah(wah::Build::V847))),
        (id::slot(0, id::AUTO), 1.0),
        (id::slot(1, id::STOMP), stomp(Stomp::Pedal(Pedal::Green808))),
        (id::slot(1, id::P_DRIVE), 0.7),
        (id::AMP, 0.0),
        (id::MIC_B, 6.0),
        (id::B_PAN, 0.5),
        (id::DI_SOURCE, 2.0),
        (id::MIX, 0.8),
    ];
    for channels in [1, 2] {
        let mut worker = Line::new(&set, channels, Run::Deterministic, 128);
        let mut inline = Line::new(&set, channels, Run::Inline, 128);
        let stage = stage_latency(1, 128) as usize;
        for _ in 0..2 {
            worker.p.reset();
            inline.p.reset();
            worker.at = 0;
            inline.at = 0;
            for (block, len) in [31, 64, 128, 511, 97, 128, 256, 511, 200, 128]
                .into_iter()
                .enumerate()
            {
                // The drive, the footswitch, the treadle and the master,
                // inside segments and at their edges.
                let events: Vec<_> = [0, 17, 64, 255, 400]
                    .into_iter()
                    .filter(|&i| i < len)
                    .flat_map(|i| {
                        let flip = (block + i) % 2 == 0;
                        [
                            ParameterEvent {
                                sample_offset: i as u32,
                                parameter: ParameterId(id::DRIVE),
                                value: if flip { 0.3 } else { 0.8 },
                            },
                            ParameterEvent {
                                sample_offset: i as u32,
                                parameter: ParameterId(id::slot(1, id::ON)),
                                value: if block == 4 { 0.0 } else { 1.0 },
                            },
                            ParameterEvent {
                                sample_offset: i as u32,
                                parameter: ParameterId(id::slot(0, id::TREADLE)),
                                value: if flip { 0.2 } else { 0.9 },
                            },
                        ]
                    })
                    .collect();
                let signal =
                    |n: usize, c: usize| pluck(n) * if c == 1 && block >= 6 { 0.5 } else { 1.0 };
                let at = worker.at;
                let a = worker.block(len, &events, signal);
                let b = inline.block(len, &events, signal);
                assert_eq!(a, b, "{channels} channels, block {block}");
                // Silence until the amplifier's own reservoir has primed
                // (after it, its circuits at rest are not exactly zero).
                if at + len <= stage {
                    assert!(a.0.iter().flatten().all(|&x| x == 0.0), "priming");
                }
                assert_eq!(worker.p.take_underruns(), 0);
            }
        }
    }
}

#[test]
fn a_bypassed_pedal_is_only_its_stage_delay() {
    use faderframe_guitar::voice::Pedal;
    let block = 128;
    let mut with = Line::new(
        &[
            (id::slot(0, id::STOMP), stomp(Stomp::Pedal(Pedal::Rodent))),
            (id::slot(0, id::ON), 0.0),
        ],
        1,
        Run::Inline,
        block,
    );
    let mut without = Line::new(&[], 1, Run::Inline, block);
    let (a, _) = with.play(0.5);
    let (b, _) = without.play(0.5);
    // The amplifier behind it starts a stage later, so the same within the
    // solver's rounding.
    let stage = stage_latency(1, block) as usize;
    let peak = b[0].iter().fold(0f32, |m, x| m.max(x.abs()));
    let worst = a[0][stage..]
        .iter()
        .zip(&b[0][..b[0].len() - stage])
        .fold(0f32, |m, (x, y)| m.max((x - y).abs()));
    assert!(
        peak > 1e-3 && worst <= peak * 1e-5,
        "worst {worst}, peak {peak}"
    );
}

#[test]
fn the_di_is_the_input_in_step_with_the_amplifier() {
    use faderframe_guitar::voice::Pedal;
    let mut line = Line::new(
        &[(id::slot(0, id::STOMP), stomp(Stomp::Pedal(Pedal::BigMuff)))],
        2,
        Run::Inline,
        128,
    );
    let total = latency(&line.params, 128) as usize;
    let (main, di) = line.play(0.5);
    for (c, di) in di.iter().enumerate() {
        for (n, &x) in di.iter().enumerate() {
            let expected = if n < total { 0.0 } else { pluck(n - total) };
            assert_eq!(x, expected, "channel {c}, frame {n}");
        }
    }
    assert!(main[0] != di[0], "the amplifier is not the DI");
    // With Mix at zero the main output is the DI as well.
    line.params.set_by_id(ParameterId(id::MIX), 0.0).unwrap();
    let _ = line.play(0.2);
    let (main, di) = line.play(0.2);
    assert_eq!(main, di);
}

#[test]
fn duplicated_mono_wakes_to_stereo_without_changing_audio() {
    use faderframe_guitar::voice::Pedal;
    let set = [
        (id::slot(0, id::STOMP), stomp(Stomp::Pedal(Pedal::Green808))),
        (id::AMP, 8.0),
        (id::REVERB, 0.3),
    ];
    let mut shared = Line::new(&set, 2, Run::Inline, 128);
    let mut independent = Line::new(&set, 2, Run::Inline, 128);
    independent.p.force_stereo();
    for block in 0..40 {
        let signal = |n: usize, c: usize| {
            let x = pluck(n);
            if c == 1 && block == 20 && n % 256 == 255 {
                x + 0.1
            } else {
                x
            }
        };
        assert_eq!(
            shared.block(256, &[], signal),
            independent.block(256, &[], signal),
            "block {block}"
        );
    }
}

#[test]
fn a_mono_source_on_a_stereo_bus_places_the_microphones() {
    // One amplifier, two microphones panned apart: two different sides.
    let mut line = Line::new(
        &[(id::MIC_B, 6.0), (id::A_PAN, -0.8), (id::B_PAN, 0.8)],
        2,
        Run::Inline,
        128,
    );
    let (main, _) = line.play(0.5);
    assert!(main[0] != main[1]);
    assert!(rms_db(&main[0][12_000..]) > -60.0 && rms_db(&main[1][12_000..]) > -60.0);
    assert!(line.tap.value(value::UNDERRUNS) == 0.0);
}
