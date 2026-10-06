#![allow(clippy::unwrap_used)]

use super::*;
use crate::PluginFactory;
use crate::tap::RING_FRAMES;
use faderframe_audio_graph::AudioBuffer;
use faderframe_automation::ParameterEvent;
use faderframe_core::ChannelLayout;
use faderframe_transport::TransportInfo;

const SR: f64 = 48_000.0;
const BLOCK: usize = 256;

struct Rig {
    params: ParamValues,
    tap: Arc<AnalysisTap>,
    eq: EqProcessor,
    ins: Vec<AudioBuffer>,
    outs: Vec<AudioBuffer>,
}

fn config() -> ProcessConfig {
    ProcessConfig {
        sample_rate: SR,
        max_block_size: BLOCK as u32,
        sidechain: true,
        double_precision: false,
    }
}

impl Rig {
    fn new() -> Self {
        let params = ParamValues::new(parameters());
        let tap = Arc::new(AnalysisTap::new(params.clone(), TAP_VALUES));
        let eq = EqProcessor::new(params.clone(), Arc::clone(&tap), &config());
        let mut ins = vec![
            AudioBuffer::new(ChannelLayout::Stereo, BLOCK),
            AudioBuffer::new(ChannelLayout::Stereo, BLOCK),
        ];
        let mut outs = vec![AudioBuffer::new(ChannelLayout::Stereo, BLOCK)];
        for b in ins.iter_mut().chain(outs.iter_mut()) {
            b.set_len(BLOCK);
        }
        Self {
            params,
            tap,
            eq,
            ins,
            outs,
        }
    }

    /// A fresh processor for the parameters as they are (the mode, the
    /// resolution and spectral bands are fixed when it is made).
    fn rebuild(self) -> Self {
        let eq = EqProcessor::new(self.params.clone(), Arc::clone(&self.tap), &config());
        Self { eq, ..self }
    }

    fn set_band(&self, band: usize, field: Field, value: f64) {
        self.params.set_by_id(band_id(band, field), value).unwrap();
    }

    fn set_global(&self, g: usize, value: f64) {
        self.params.set_by_id(global_id(g), value).unwrap();
    }

    /// A bell (on) at `freq` with `gain`.
    fn bell(&self, band: usize, freq: f64, gain: f64) {
        self.set_band(band, Field::Enabled, 1.0);
        self.set_band(band, Field::Freq, freq);
        self.set_band(band, Field::Gain, gain);
    }

    /// Run `blocks` blocks of `signal(frame) -> (left, right)` (and a key);
    /// returns the output.
    fn run(
        &mut self,
        blocks: usize,
        start: usize,
        signal: impl Fn(usize) -> (f32, f32),
        key: impl Fn(usize) -> (f32, f32),
        events: &[ParameterEvent],
    ) -> (Vec<f32>, Vec<f32>) {
        let (mut l, mut r) = (Vec::new(), Vec::new());
        let transport = TransportInfo::default();
        for b in 0..blocks {
            for i in 0..BLOCK {
                let n = start + b * BLOCK + i;
                let (a, c) = signal(n);
                self.ins[0].channel_mut(0)[i] = a;
                self.ins[0].channel_mut(1)[i] = c;
                let (ka, kc) = key(n);
                self.ins[1].channel_mut(0)[i] = ka;
                self.ins[1].channel_mut(1)[i] = kc;
            }
            let ctx = PluginProcessContext {
                transport: &transport,
                param_events: if b == 0 { events } else { &[] },
                harmony: &crate::NO_HARMONY,
                param_mods: &[],
                note_mods: &[],
            };
            let mut io = NodeIo {
                frames: BLOCK,
                audio_in: &self.ins,
                audio_out: &mut self.outs,
                events_in: &[],
                events_out: &mut [],
            };
            self.eq.process(&ctx, &mut io);
            l.extend_from_slice(self.outs[0].channel(0));
            r.extend_from_slice(self.outs[0].channel(1));
        }
        (l, r)
    }

    /// The impulse response of the left channel (`len` samples).
    fn impulse(&mut self, len: usize) -> Vec<f64> {
        let (l, _) = self.run(
            len.div_ceil(BLOCK),
            0,
            |n| if n == 0 { (1.0, 1.0) } else { (0.0, 0.0) },
            silence,
            &[],
        );
        l.iter().map(|v| f64::from(*v)).collect()
    }
}

fn tone(f: f64, amp: f64) -> impl Fn(usize) -> (f32, f32) {
    move |n| {
        let v = (amp * (std::f64::consts::TAU * f * n as f64 / SR).sin()) as f32;
        (v, v)
    }
}

fn sine(f: f64) -> impl Fn(usize) -> (f32, f32) {
    tone(f, 0.25)
}

fn silence(_: usize) -> (f32, f32) {
    (0.0, 0.0)
}

fn peak(x: &[f32]) -> f64 {
    x.iter().fold(0.0f64, |m, v| m.max(f64::from(v.abs())))
}

/// A sine's amplitude from its RMS (the sample peak misses the crest).
fn amplitude(x: &[f32]) -> f64 {
    let ms: f64 = x.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / x.len() as f64;
    (2.0 * ms).sqrt()
}

/// The level (relative to `amp`) of the `f` Hz component of `x`.
fn component(x: &[f32], f: f64, amp: f64) -> f64 {
    let w = std::f64::consts::TAU * f / SR;
    let (mut re, mut im) = (0.0, 0.0);
    for (k, v) in x.iter().enumerate() {
        re += f64::from(*v) * (w * k as f64).cos();
        im += f64::from(*v) * (w * k as f64).sin();
    }
    db(2.0 * re.hypot(im) / x.len() as f64 / amp)
}

/// The response of an impulse response at `f` (complex).
fn response(ir: &[f64], f: f64) -> design::C64 {
    let w = std::f64::consts::TAU * f / SR;
    ir.iter()
        .enumerate()
        .fold(design::C64::new(0.0, 0.0), |acc, (k, x)| {
            acc + design::C64::from_polar(*x, -w * k as f64)
        })
}

fn db(x: f64) -> f64 {
    20.0 * x.log10()
}

#[test]
fn a_band_does_what_its_design_says() {
    let mut rig = Rig::new();
    rig.bell(0, 1_000.0, 9.0);
    rig.set_band(0, Field::Q, 1.4);
    for f in [250.0, 1_000.0, 3_000.0] {
        let (l, r) = rig.run(80, 0, sine(f), silence, &[]);
        let got = db(amplitude(&l[l.len() / 2..]) / 0.25);
        let want = design::band_db(
            &BandShape {
                kind: BandType::Bell,
                freq: 1_000.0,
                gain: 9.0,
                q: 1.4,
                slope: 12.0,
            },
            SR,
            f,
        );
        assert!((got - want).abs() < 0.05, "{f} Hz: {got:.3} vs {want:.3}");
        assert_eq!(l, r, "both channels alike for a stereo band");
    }
}

#[test]
fn switching_a_band_fades_instead_of_clicking() {
    let mut rig = Rig::new();
    rig.set_band(0, Field::Freq, 100.0);
    rig.set_band(0, Field::Type, BandType::LowCut.index() as f64);
    rig.set_band(0, Field::Slope, 96.0);
    let low = sine(40.0);
    rig.run(20, 0, &low, silence, &[]);
    rig.set_band(0, Field::Enabled, 1.0);
    let (l, _) = rig.run(20, 20 * BLOCK, &low, silence, &[]);
    // The largest step between neighbouring samples stays near the sine's.
    let step = l
        .windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0f32, f32::max);
    let sine_step = (0.25 * std::f64::consts::TAU * 40.0 / SR) as f32;
    assert!(step < sine_step * 2.5, "a click: {step} vs {sine_step}");
    // And the cut is in by the end.
    assert!(db(peak(&l[l.len() - 2048..]) / 0.25) < -40.0);
    // A type change mid-signal fades too.
    rig.set_band(0, Field::Type, BandType::Bell.index() as f64);
    rig.set_band(0, Field::Gain, 12.0);
    let (l, _) = rig.run(20, 40 * BLOCK, &low, silence, &[]);
    let step = l
        .windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0f32, f32::max);
    assert!(step < sine_step * 5.0, "a click on the type change: {step}");
}

#[test]
fn a_fractional_slope_glides_without_a_fade() {
    let mut rig = Rig::new();
    rig.set_band(0, Field::Enabled, 1.0);
    rig.set_band(0, Field::Type, BandType::HighCut.index() as f64);
    rig.set_band(0, Field::Freq, 1_000.0);
    rig.set_band(0, Field::Slope, 13.0);
    rig.run(40, 0, sine(4_000.0), silence, &[]);
    rig.set_band(0, Field::Slope, 16.0);
    let (l, _) = rig.run(40, 40 * BLOCK, sine(4_000.0), silence, &[]);
    let want = design::band_db(
        &BandShape {
            kind: BandType::HighCut,
            freq: 1_000.0,
            gain: 0.0,
            q: std::f64::consts::FRAC_1_SQRT_2,
            slope: 16.0,
        },
        SR,
        4_000.0,
    );
    let got = db(amplitude(&l[l.len() / 2..]) / 0.25);
    assert!((got - want).abs() < 0.1, "{got:.2} vs {want:.2}");
    assert!(want < -26.0 && want > -34.0, "{want}");
}

#[test]
fn placements_reach_only_their_part_of_the_signal() {
    let mid = |n: usize| {
        let v = (0.25 * (std::f64::consts::TAU * 1_000.0 * n as f64 / SR).sin()) as f32;
        (v, v)
    };
    let side = |n: usize| {
        let v = (0.25 * (std::f64::consts::TAU * 1_000.0 * n as f64 / SR).sin()) as f32;
        (v, -v)
    };
    for (placement, mid_db, side_db) in [
        (Placement::Mid, 6.0, 0.0),
        (Placement::Side, 0.0, 6.0),
        (Placement::Stereo, 6.0, 6.0),
    ] {
        let mut rig = Rig::new();
        rig.bell(0, 1_000.0, 6.0);
        rig.set_band(0, Field::Placement, placement.index() as f64);
        let (l, _) = rig.run(40, 0, mid, silence, &[]);
        assert!(
            (db(peak(&l[5000..]) / 0.25) - mid_db).abs() < 0.05,
            "{placement:?} on the mid"
        );
        let (l, r) = rig.run(40, 0, side, silence, &[]);
        assert!(
            (db(peak(&l[5000..]) / 0.25) - side_db).abs() < 0.05,
            "{placement:?} on the side"
        );
        assert!((peak(&l[5000..]) - peak(&r[5000..])).abs() < 1e-4);
    }
    // Left only.
    let mut rig = Rig::new();
    rig.bell(0, 1_000.0, -12.0);
    rig.set_band(0, Field::Placement, Placement::Left.index() as f64);
    let (l, r) = rig.run(40, 0, sine(1_000.0), silence, &[]);
    assert!((db(peak(&l[5000..]) / 0.25) + 12.0).abs() < 0.05);
    assert!(db(peak(&r[5000..]) / 0.25).abs() < 0.01);
}

/// A dynamic bell at 2 kHz cutting by up to 9 dB over −40 dBFS.
fn dynamic_rig() -> Rig {
    let rig = Rig::new();
    rig.bell(0, 2_000.0, 0.0);
    rig.set_band(0, Field::Range, -9.0);
    rig.set_band(0, Field::Dynamics, 1.0);
    rig.set_band(0, Field::Threshold, -40.0);
    rig
}

#[test]
fn a_dynamic_band_moves_with_its_key() {
    let mut rig = dynamic_rig();
    // Loud at the band's frequency: it cuts by the full range.
    let (l, _) = rig.run(60, 0, sine(2_000.0), silence, &[]);
    let level = db(peak(&l[l.len() - 4096..]) / 0.25);
    assert!((level + 9.0).abs() < 0.3, "{level}");
    assert!((f64::from(rig.tap.value(value::DYN)) + 9.0).abs() < 0.3);
    // The trigger's level and the threshold are published.
    assert!((f64::from(rig.tap.value(value::THRESHOLD)) + 40.0).abs() < 1e-3);
    assert!(rig.tap.value(value::KEY) > -20.0);
    // Quiet: it lets go.
    let quiet = tone(2_000.0, 0.001);
    let (l, _) = rig.run(60, 0, &quiet, silence, &[]);
    assert!(db(peak(&l[l.len() - 4096..]) / 0.001).abs() < 0.2);
    // Keyed from the sidechain instead: the signal is quiet, the key loud.
    rig.set_band(0, Field::Key, 1.0);
    let (l, _) = rig.run(60, 0, &quiet, sine(2_000.0), &[]);
    assert!((db(peak(&l[l.len() - 4096..]) / 0.001) + 9.0).abs() < 0.3);
    // Back in auto mode the sidechain is not in effect.
    rig.set_band(0, Field::Dynamics, 0.0);
    let (l, _) = rig.run(60, 0, &quiet, sine(2_000.0), &[]);
    assert!(db(peak(&l[l.len() - 4096..]) / 0.001).abs() < 0.5);
    // The dynamics bypassed: a static band.
    rig.set_band(0, Field::Dynamics, 1.0);
    rig.set_band(0, Field::DynBypass, 1.0);
    let (l, _) = rig.run(60, 0, &quiet, sine(2_000.0), &[]);
    assert!(db(peak(&l[l.len() - 4096..]) / 0.001).abs() < 0.2);
}

#[test]
fn the_knee_is_soft_and_the_range_is_reached() {
    assert_eq!(dynamic_gain(-6.0, -20.0, -30.0), 0.0);
    assert_eq!(dynamic_gain(-6.0, -20.0, -17.0), -3.0);
    assert_eq!(dynamic_gain(6.0, -20.0, 0.0), 6.0);
    // Inside the knee it starts gently.
    let at = dynamic_gain(-6.0, -20.0, -20.0);
    assert!(at < 0.0 && at > -1.0, "{at}");
    // Continuous through the knee.
    let mut last = 0.0;
    for i in 0..200 {
        let g = dynamic_gain(-6.0, -20.0, -26.0 + i as f64 * 0.05);
        assert!(g <= last + 1e-12 && last - g < 0.1);
        last = g;
    }
}

#[test]
fn a_free_trigger_hears_only_its_own_range() {
    let mut rig = dynamic_rig();
    rig.set_band(0, Field::Key, 1.0);
    rig.set_band(0, Field::Trigger, 1.0);
    rig.set_band(0, Field::TriggerLow, 5_000.0);
    let quiet = tone(2_000.0, 0.001);
    // A key well below the free range does nothing.
    let (l, _) = rig.run(60, 0, &quiet, sine(1_000.0), &[]);
    assert!(db(peak(&l[l.len() - 4096..]) / 0.001).abs() < 0.3);
    // One inside it triggers.
    let (l, _) = rig.run(60, 0, &quiet, sine(8_000.0), &[]);
    assert!((db(peak(&l[l.len() - 4096..]) / 0.001) + 9.0).abs() < 0.4);
}

#[test]
fn an_auto_threshold_follows_the_trigger() {
    let mut rig = dynamic_rig();
    rig.set_band(0, Field::Dynamics, 0.0);
    // A steady tone: the threshold settles just over it, little moves.
    let (l, _) = rig.run(800, 0, sine(2_000.0), silence, &[]);
    let steady = db(peak(&l[l.len() - 4096..]) / 0.25);
    assert!(steady > -2.0, "steady {steady}");
    let threshold = f64::from(rig.tap.value(value::THRESHOLD));
    let key = f64::from(rig.tap.value(value::KEY));
    assert!(
        (threshold - key - dynamics::AUTO_MARGIN).abs() < 1.0,
        "{threshold} vs {key}"
    );
    // A burst 12 dB over it is pulled down.
    let (l, _) = rig.run(8, 800 * BLOCK, tone(2_000.0, 1.0), silence, &[]);
    let burst = db(peak(&l[l.len() - 512..]) / 1.0);
    assert!(burst < -5.0, "burst {burst}");
}

#[test]
fn custom_times_slow_the_band_down() {
    let settle = |attack: f64| {
        let mut rig = dynamic_rig();
        // Near the threshold, where the attack decides how soon it acts.
        rig.set_band(0, Field::Threshold, -20.0);
        rig.set_band(0, Field::Attack, attack);
        let (l, _) = rig.run(40, 0, sine(2_000.0), silence, &[]);
        // How far it has cut 5 ms in.
        db(peak(&l[200..240]) / 0.25)
    };
    let fast = settle(0.0);
    let slow = settle(1.0);
    assert!(fast < slow - 1.0, "fast {fast} slow {slow}");
}

#[test]
fn auto_gain_keeps_pink_noise_level() {
    let mut bands = [BandParams::read(&ParamValues::new(parameters()), 0); BANDS];
    bands[0].enabled = true;
    bands[0].kind = BandType::HighShelf;
    bands[0].freq = 3_000.0;
    bands[0].gain = 6.0;
    let change = loudness_change(&bands, 1.0, false, SR);
    assert!(change > 1.0 && change < 3.0, "{change}");
    let mut rig = Rig::new();
    rig.set_band(0, Field::Enabled, 1.0);
    rig.set_band(0, Field::Type, BandType::HighShelf.index() as f64);
    rig.set_band(0, Field::Freq, 3_000.0);
    rig.set_band(0, Field::Gain, 6.0);
    rig.set_global(global::AUTO_GAIN, 1.0);
    let (l, _) = rig.run(60, 0, sine(100.0), silence, &[]);
    // Below the shelf the level drops by the compensation.
    let level = db(peak(&l[l.len() - 4096..]) / 0.25);
    assert!((level + change).abs() < 0.1, "{level} vs {change}");
    // The gain scale halves every band.
    rig.set_global(global::GAIN_SCALE, 0.5);
    rig.set_global(global::AUTO_GAIN, 0.0);
    let (l, _) = rig.run(60, 0, sine(15_000.0), silence, &[]);
    assert!((db(peak(&l[l.len() - 4096..]) / 0.25) - 3.0).abs() < 0.1);
}

#[test]
fn gain_q_interaction_narrows_a_boost() {
    let (g, q) = gain_q(BandType::Bell, 12.0, 1.0);
    assert!(q > 1.3 && q < 1.5 && g == 12.0, "{g} {q}");
    let (g, _) = gain_q(BandType::Bell, 6.0, 8.0);
    assert!(g > 6.0, "a narrow bell gains a little: {g}");
    assert_eq!(gain_q(BandType::LowShelf, 12.0, 1.0), (12.0, 1.0));
    let mut rig = Rig::new();
    rig.bell(0, 1_000.0, 12.0);
    rig.set_band(0, Field::Q, 1.0);
    let (l, _) = rig.run(60, 0, sine(1_400.0), silence, &[]);
    let wide = db(amplitude(&l[l.len() / 2..]) / 0.25);
    rig.set_global(global::GAIN_Q, 1.0);
    let (l, _) = rig.run(60, 0, sine(1_400.0), silence, &[]);
    let narrow = db(amplitude(&l[l.len() / 2..]) / 0.25);
    assert!(narrow < wide - 0.5, "{narrow} vs {wide}");
}

#[test]
fn listening_to_a_band_plays_its_region() {
    let mut rig = Rig::new();
    rig.bell(2, 500.0, 6.0);
    rig.set_band(2, Field::Q, 4.0);
    rig.tap.set_listen(Some(2));
    let (l, _) = rig.run(40, 0, sine(500.0), silence, &[]);
    assert!(
        db(peak(&l[5000..]) / 0.25).abs() < 0.05,
        "the centre passes"
    );
    let (l, _) = rig.run(40, 0, sine(5_000.0), silence, &[]);
    assert!(db(peak(&l[5000..]) / 0.25) < -25.0, "far away is gone");
    rig.tap.set_listen(None);
    let (l, _) = rig.run(40, 0, sine(5_000.0), silence, &[]);
    assert!(db(peak(&l[5000..]) / 0.25).abs() < 0.05);
}

#[test]
fn listening_to_a_trigger_plays_the_sidechain_through_its_filter() {
    let mut rig = dynamic_rig();
    rig.set_band(0, Field::Key, 1.0);
    rig.tap.set_listen(Some(listen_key(0)));
    // The input is silent; what is heard is the key at the band.
    let (l, _) = rig.run(40, 0, silence, sine(2_000.0), &[]);
    assert!(db(peak(&l[5000..]) / 0.25).abs() < 0.1);
    let (l, _) = rig.run(40, 0, silence, sine(200.0), &[]);
    assert!(db(peak(&l[5000..]) / 0.25) < -15.0);
}

#[test]
fn automation_lands_inside_the_block() {
    let mut rig = Rig::new();
    rig.bell(0, 1_000.0, 0.0);
    let ev = ParameterEvent {
        parameter: band_id(0, Field::Gain),
        value: 12.0,
        sample_offset: 100,
    };
    rig.run(1, 0, silence, silence, &[ev]);
    let (l, _) = rig.run(60, BLOCK, sine(1_000.0), silence, &[]);
    assert!((db(peak(&l[l.len() - 4096..]) / 0.25) - 12.0).abs() < 0.05);
}

#[test]
fn the_analyser_rings_fill_only_while_watched() {
    let mut rig = Rig::new();
    rig.run(4, 0, sine(1_000.0), sine(300.0), &[]);
    assert_eq!(rig.tap.output.written(), 0);
    rig.tap.watch();
    rig.run(4, 0, sine(1_000.0), sine(300.0), &[]);
    assert_eq!(rig.tap.input.written(), 4 * BLOCK as u64);
    assert_eq!(rig.tap.sidechain.written(), 4 * BLOCK as u64);
    let (mut a, mut b) = (vec![0.0; 1024], vec![0.0; 1024]);
    rig.tap.output.latest(&mut a, &mut b);
    assert!(peak(&a) > 0.2);
    rig.tap.sidechain.latest(&mut a, &mut b);
    assert!(peak(&a) > 0.2);
    const { assert!(RING_FRAMES >= 4 * BLOCK) };
    // The meters see the input and output.
    assert!(rig.tap.meter_in.held(0) > 0.2);
}

#[test]
fn output_pan_phase_and_bypass() {
    let mut rig = Rig::new();
    rig.bell(0, 1_000.0, 6.0);
    rig.set_global(global::PAN, 0.5);
    let (l, r) = rig.run(40, 0, sine(1_000.0), silence, &[]);
    assert!((db(peak(&l[5000..]) / 0.25) - (6.0 + db(0.5))).abs() < 0.05);
    assert!((db(peak(&r[5000..]) / 0.25) - 6.0).abs() < 0.05);
    // Mid/side: towards the side halves the mid.
    rig.set_global(global::PAN_MODE, 1.0);
    let (l, r) = rig.run(40, 0, sine(1_000.0), silence, &[]);
    assert!((db(peak(&l[5000..]) / 0.25) - (6.0 + db(0.5))).abs() < 0.05);
    assert!((peak(&l[5000..]) - peak(&r[5000..])).abs() < 1e-4);
    rig.set_global(global::PAN, 0.0);
    rig.set_global(global::INVERT, 1.0);
    let (l, _) = rig.run(40, 0, sine(1_000.0), silence, &[]);
    let (clean, _) = {
        let mut other = Rig::new();
        other.bell(0, 1_000.0, 6.0);
        other.run(40, 0, sine(1_000.0), silence, &[])
    };
    let tail = l.len() - 2048;
    for (i, (a, b)) in l[tail..].iter().zip(&clean[tail..]).enumerate() {
        assert!((a + b).abs() < 1e-4, "inverted at {i}: {a} {b}");
    }
    // Bypassed: the input, exactly.
    rig.set_global(global::INVERT, 0.0);
    rig.set_global(global::BYPASS, 1.0);
    let input = sine(1_000.0);
    let (l, _) = rig.run(40, 0, &input, silence, &[]);
    for (i, v) in l.iter().enumerate().skip(l.len() - 2048) {
        assert!(
            (v - input(i).0).abs() < 1e-6,
            "at {i}: {v} vs {}",
            input(i).0
        );
    }
}

#[test]
fn flat_tilts_and_all_passes_in_the_processor() {
    let mut rig = Rig::new();
    rig.set_band(0, Field::Enabled, 1.0);
    rig.set_band(0, Field::Type, BandType::AllPass.index() as f64);
    rig.set_band(0, Field::Freq, 1_000.0);
    for f in [100.0, 1_000.0, 10_000.0] {
        let (l, _) = rig.run(40, 0, sine(f), silence, &[]);
        // Whole periods of every probe.
        assert!(
            db(amplitude(&l[l.len() - 4_800..]) / 0.25).abs() < 0.02,
            "{f}"
        );
    }
    let mut rig = Rig::new();
    rig.set_band(0, Field::Enabled, 1.0);
    rig.set_band(0, Field::Type, BandType::FlatTilt.index() as f64);
    rig.set_band(0, Field::Freq, 1_000.0);
    rig.set_band(0, Field::Gain, 10.0);
    let (l, _) = rig.run(40, 0, sine(4_000.0), silence, &[]);
    let got = db(amplitude(&l[l.len() - 4_800..]) / 0.25);
    let want = 10.0 / design::FLAT_TILT_OCTAVES * 2.0;
    assert!((got - want).abs() < 0.25, "{got} vs {want}");
}

#[test]
fn the_character_colours_loud_signals_only() {
    let mut rig = Rig::new();
    rig.set_global(global::CHARACTER, 2.0);
    // Whole periods: no leakage between the harmonics.
    let (l, _) = rig.run(80, 0, tone(100.0, 0.9), silence, &[]);
    let tail = &l[l.len() - 9_600..];
    assert!(component(tail, 200.0, 0.9) > -60.0, "even harmonics");
    let (l, _) = rig.run(80, 0, tone(100.0, 0.01), silence, &[]);
    let tail = &l[l.len() - 9_600..];
    assert!(component(tail, 200.0, 0.01) < -60.0);
}

#[test]
fn the_formatting_names_choices() {
    assert_eq!(format(band_id(3, Field::Type), 3.0).unwrap(), "Low Cut");
    assert_eq!(format(band_id(3, Field::Slope), 48.0).unwrap(), "48 dB/oct");
    assert_eq!(
        format(band_id(3, Field::Slope), 100.0).unwrap(),
        "Brickwall"
    );
    assert_eq!(format(band_id(3, Field::Placement), 4.0).unwrap(), "Side");
    assert_eq!(format(band_id(3, Field::Freq), 1234.0).unwrap(), "1.23 kHz");
    assert_eq!(format(band_id(3, Field::Range), 0.0).unwrap(), "Off");
    assert_eq!(format(band_id(3, Field::Threshold), 0.0).unwrap(), "Auto");
    assert_eq!(format(band_id(3, Field::Key), 1.0).unwrap(), "Sidechain");
    assert_eq!(
        format(global_id(global::PHASE), 1.0).unwrap(),
        "Linear Phase"
    );
    assert_eq!(
        format(global_id(global::PHASE), 2.0).unwrap(),
        "Natural Phase"
    );
    assert_eq!(format(global_id(global::CHARACTER), 1.0).unwrap(), "Subtle");
    assert_eq!(format(global_id(global::PAN), -0.25).unwrap(), "L25");
    assert!(format(global_id(global::OUTPUT), 1.0).is_none());
    assert_eq!(parameters().len(), GLOBALS + BANDS * FIELDS);
    // Indexes and ids agree, ids are unique and decode back.
    let ps = parameters();
    for b in 0..BANDS {
        for f in Field::ALL {
            assert_eq!(ps[band_index(b, f)].id, band_id(b, f));
            assert_eq!(field_of(band_id(b, f)), Some((b, f)));
        }
    }
    for (g, p) in ps.iter().enumerate().take(GLOBALS) {
        assert_eq!(p.id, global_id(g));
        assert_eq!(global_of(p.id), Some(g));
    }
    let mut ids: Vec<u32> = ps.iter().map(|p| p.id.0).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), ps.len(), "ids are unique");
    // The first version's ids keep their meaning.
    assert_eq!(band_id(1, Field::Gain).0, 100 + 16 + 3);
    assert_eq!(global_id(global::QUALITY).0, 4);
}

mod linear_phase {
    use super::*;

    fn linear_rig(quality: f64) -> Rig {
        let rig = Rig::new();
        rig.set_global(global::PHASE, PhaseMode::Linear.value());
        rig.set_global(global::QUALITY, quality);
        rig.rebuild()
    }

    #[test]
    fn an_impulse_comes_out_at_the_latency_and_symmetric() {
        let rig = linear_rig(0.0);
        rig.bell(0, 1_000.0, 6.0);
        let mut rig = rig.rebuild();
        let lat = linear::latency(0) as usize;
        let l = rig.impulse(lat * 2 + 2 * BLOCK);
        let peak_at = l
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
            .unwrap()
            .0;
        assert_eq!(peak_at, lat, "the peak is the latency");
        for k in 1..1500 {
            let (a, b) = (l[lat - k], l[lat + k]);
            assert!((a - b).abs() < 1e-5, "asymmetric at ±{k}: {a} {b}");
        }
        assert_eq!(rig.eq.latency() as usize, lat);
    }

    #[test]
    fn it_follows_the_analog_curve_to_nyquist() {
        // Where minimum phase cannot quite: a broad 12 kHz bell.
        let rig = linear_rig(1.0);
        rig.bell(0, 12_000.0, 9.0);
        rig.set_band(0, Field::Q, 0.7);
        let shape = BandParams::read(&rig.params, 0).shape(1.0, false);
        let mut rig = rig.rebuild();
        let ir = rig.impulse(linear::latency(1) as usize + linear::LENGTHS[1]);
        for f in [1_000.0, 6_000.0, 12_000.0, 16_000.0, 19_000.0, 20_000.0] {
            let got = 20.0 * response(&ir, f).norm().log10();
            let want = design::analog_db(&shape, f);
            assert!(
                (got - want).abs() < 0.1,
                "{f} Hz: {got:.3} vs analog {want:.3}"
            );
        }
    }

    #[test]
    fn mid_and_side_stay_apart() {
        let base = linear_rig(0.0);
        base.bell(0, 1_000.0, 6.0);
        base.set_band(0, Field::Placement, Placement::Mid.index() as f64);
        let mut rig = base.rebuild();
        let side = |n: usize| {
            let v = (0.25 * (std::f64::consts::TAU * 1_000.0 * n as f64 / SR).sin()) as f32;
            (v, -v)
        };
        let (l, r) = rig.run(80, 0, side, silence, &[]);
        assert!(
            db(amplitude(&l[l.len() / 2..]) / 0.25).abs() < 0.05,
            "the side is untouched"
        );
        assert!((amplitude(&l[l.len() / 2..]) - amplitude(&r[r.len() / 2..])).abs() < 1e-4);
        let (l, _) = rig.run(80, 0, sine(1_000.0), silence, &[]);
        assert!(
            (db(amplitude(&l[l.len() / 2..]) / 0.25) - 6.0).abs() < 0.05,
            "the mid gets +6"
        );
    }

    #[test]
    fn new_settings_reach_the_audio_through_the_design_thread() {
        let mut rig = linear_rig(0.0);
        rig.bell(0, 1_000.0, -12.0);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let (l, _) = rig.run(40, 0, sine(1_000.0), silence, &[]);
            let level = db(amplitude(&l[l.len() / 2..]) / 0.25);
            if (level + 12.0).abs() < 0.1 {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "still {level:.2} dB");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    #[test]
    fn the_instance_asks_for_a_rebuild_when_the_latency_changes() {
        let mut inst = crate::builtin::BuiltinFactory
            .instantiate(faderframe_core::builtin::EQ)
            .unwrap();
        assert_eq!(inst.latency_samples(), 0);
        assert!(!inst.poll().restart);
        inst.set_parameter(global_id(global::PHASE), 1.0).unwrap();
        assert_eq!(inst.latency_samples(), linear::latency(1));
        assert!(inst.poll().restart, "the latency changed");
        assert!(!inst.poll().restart);
        inst.set_parameter(global_id(global::QUALITY), 3.0).unwrap();
        assert!(inst.poll().restart);
        assert_eq!(inst.latency_samples(), 32_768 / 2 + 256);
        inst.set_parameter(global_id(global::PHASE), 2.0).unwrap();
        assert!(inst.poll().restart);
        assert_eq!(inst.latency_samples(), natural::LATENCY);
        // A spectral band adds its frames.
        inst.set_parameter(band_id(0, Field::Enabled), 1.0).unwrap();
        inst.set_parameter(band_id(0, Field::Spectral), 1.0)
            .unwrap();
        assert!(inst.poll().restart);
        assert_eq!(
            inst.latency_samples(),
            natural::LATENCY + spectral::frame(3) as u32
        );
        assert!(!parameters()[global::PHASE].automatable);
    }
}

mod natural_phase {
    use super::*;

    fn natural_rig() -> Rig {
        let rig = Rig::new();
        rig.set_global(global::PHASE, PhaseMode::Natural.value());
        rig
    }

    /// The response of the bands after taking off the latency, against the
    /// analog filters': magnitude (dB) and phase (degrees) at `f`.
    fn error_at(ir: &[f64], analog: &[BandShape], f: f64) -> (f64, f64) {
        let lat = natural::LATENCY as f64;
        let got =
            response(ir, f) * design::C64::from_polar(1.0, std::f64::consts::TAU * f / SR * lat);
        let want = analog.iter().fold(design::C64::new(1.0, 0.0), |acc, s| {
            acc * design::analog_response(s, f)
        });
        let ratio = got / want;
        (20.0 * ratio.norm().log10(), ratio.arg().to_degrees())
    }

    #[test]
    fn it_matches_the_analog_magnitude_and_phase() {
        for (kind, freq, gain, q) in [
            (BandType::Bell, 15_000.0, 9.0, 1.0),
            (BandType::Bell, 1_000.0, -12.0, 3.0),
            (BandType::HighShelf, 8_000.0, 6.0, 0.707),
            (BandType::LowCut, 80.0, 0.0, 0.707),
        ] {
            let rig = natural_rig();
            rig.bell(0, freq, gain);
            rig.set_band(0, Field::Type, kind.index() as f64);
            rig.set_band(0, Field::Q, q);
            let shape = BandParams::read(&rig.params, 0).shape(1.0, false);
            let mut rig = rig.rebuild();
            assert_eq!(rig.eq.latency(), natural::LATENCY);
            let ir = rig.impulse(1 << 15);
            for f in [50.0, 200.0, 1_000.0, 5_000.0, 12_000.0, 16_000.0, 19_000.0] {
                if design::analog_db(&shape, f) < -30.0 {
                    continue;
                }
                let (mag, phase) = error_at(&ir, &[shape], f);
                assert!(mag.abs() < 0.1, "{kind:?} at {f}: {mag:.3} dB");
                assert!(phase.abs() < 2.0, "{kind:?} at {f}: {phase:.2}°");
            }
        }
    }

    #[test]
    fn zero_latency_would_miss_where_natural_does_not() {
        // The top of a high bell: the minimum phase sections drift from
        // the analog phase there.
        let rig = Rig::new();
        rig.bell(0, 15_000.0, 9.0);
        let shape = BandParams::read(&rig.params, 0).shape(1.0, false);
        let mut rig = rig.rebuild();
        let ir = rig.impulse(1 << 14);
        let got = response(&ir, 19_000.0);
        let want = design::analog_response(&shape, 19_000.0);
        let phase = (got / want).arg().to_degrees().abs();
        assert!(phase > 2.0, "zero latency already matches: {phase}");
    }
}

mod spectral_dynamics {
    use super::*;

    /// A spectral bell at 3 kHz cutting by up to 12 dB over −30 dBFS.
    fn spectral_rig() -> Rig {
        let rig = Rig::new();
        rig.bell(0, 3_000.0, 0.0);
        rig.set_band(0, Field::Q, 0.7);
        rig.set_band(0, Field::Range, -12.0);
        rig.set_band(0, Field::Spectral, 1.0);
        rig.set_band(0, Field::Dynamics, 1.0);
        rig.set_band(0, Field::Threshold, -30.0);
        rig.set_band(0, Field::SpectralTilt, 0.0);
        rig.set_band(0, Field::Density, 0.9);
        rig.set_global(global::QUALITY, 1.0);
        rig.rebuild()
    }

    #[test]
    fn unity_gains_rebuild_the_signal_after_its_latency() {
        let mut rig = spectral_rig();
        // Quiet: nothing triggers, the band is flat.
        let n = spectral::frame(1);
        assert_eq!(rig.eq.latency() as usize, n);
        let input = tone(1_234.0, 0.001);
        let (l, _) = rig.run(64, 0, &input, silence, &[]);
        for (i, v) in l.iter().enumerate().skip(2 * n) {
            let want = input(i - n).0;
            assert!((v - want).abs() < 1e-6, "at {i}: {v} vs {want}");
        }
    }

    #[test]
    fn only_the_loud_frequency_is_pulled_down() {
        let mut rig = spectral_rig();
        // A loud 2.6 kHz and a quiet 3.4 kHz, both in the band.
        let both = |n: usize| {
            let t = n as f64 / SR;
            let v = 0.5 * (std::f64::consts::TAU * 2_600.0 * t).sin()
                + 0.01 * (std::f64::consts::TAU * 3_400.0 * t).sin();
            (v as f32, v as f32)
        };
        let (l, _) = rig.run(160, 0, both, silence, &[]);
        let tail = &l[l.len() / 2..];
        let loud = component(tail, 2_600.0, 0.5);
        let quiet = component(tail, 3_400.0, 0.01);
        assert!(loud < -6.0, "the loud one: {loud:.2} dB");
        assert!(quiet.abs() < 1.0, "the quiet one: {quiet:.2} dB");
        // The editor sees the movement per frequency.
        let at = |f: f64| {
            let i = (0..SPECTRAL_POINTS)
                .min_by(|a, b| {
                    (spectral_point_hz(*a) / f)
                        .ln()
                        .abs()
                        .total_cmp(&(spectral_point_hz(*b) / f).ln().abs())
                })
                .unwrap();
            rig.tap.value(value::SPECTRAL + i)
        };
        assert!(at(2_600.0) < -6.0);
        assert!(at(500.0).abs() < 0.5);
        assert!(rig.tap.value(value::DYN) < -6.0);
    }

    #[test]
    fn a_spectral_band_can_be_keyed_from_the_sidechain() {
        let mut rig = spectral_rig();
        rig.set_band(0, Field::Key, 1.0);
        let quiet = tone(2_600.0, 0.01);
        let (l, _) = rig.run(160, 0, &quiet, tone(2_600.0, 0.5), &[]);
        let level = component(&l[l.len() / 2..], 2_600.0, 0.01);
        assert!(level < -6.0, "{level:.2}");
    }
}

/// How fast the EQ runs (`cargo test -p faderframe-plugin-host --release
/// eq_speed -- --ignored --nocapture`): seconds of stereo audio processed
/// per second, for a few set-ups.
#[test]
#[ignore = "a measurement, not a check"]
fn eq_speed() {
    type Setup<'a> = (&'a str, &'a dyn Fn(&Rig));
    let setups: [Setup<'_>; 7] = [
        ("no bands (the harness)", &|_| {}),
        ("8 bells, zero latency", &|r| {
            for b in 0..8 {
                r.bell(b, 100.0 * 1.6f64.powi(b as i32), 3.0);
            }
        }),
        ("24 bands, 8 dynamic", &|r| {
            for b in 0..24 {
                r.bell(b, 40.0 * 1.3f64.powi(b as i32), 2.0);
                if b % 3 == 0 {
                    r.set_band(b, Field::Range, -6.0);
                }
            }
        }),
        ("8 bells, natural phase", &|r| {
            r.set_global(global::PHASE, PhaseMode::Natural.value());
            for b in 0..8 {
                r.bell(b, 100.0 * 1.6f64.powi(b as i32), 3.0);
            }
        }),
        ("8 bells, linear phase medium", &|r| {
            r.set_global(global::PHASE, PhaseMode::Linear.value());
            for b in 0..8 {
                r.bell(b, 100.0 * 1.6f64.powi(b as i32), 3.0);
            }
        }),
        ("2 spectral bands", &|r| {
            for b in 0..2 {
                r.bell(b, 2_000.0 * 2f64.powi(b as i32), 0.0);
                r.set_band(b, Field::Range, -6.0);
                r.set_band(b, Field::Spectral, 1.0);
            }
        }),
        ("brickwall + warm character", &|r| {
            r.set_band(0, Field::Enabled, 1.0);
            r.set_band(0, Field::Type, BandType::HighCut.index() as f64);
            r.set_band(0, Field::Slope, design::BRICKWALL);
            r.set_global(global::CHARACTER, 2.0);
        }),
    ];
    for (name, setup) in setups {
        let rig = Rig::new();
        setup(&rig);
        let mut rig = rig.rebuild();
        let blocks = (10.0 * SR) as usize / BLOCK;
        let t = std::time::Instant::now();
        rig.run(blocks, 0, sine(1_000.0), silence, &[]);
        let secs = t.elapsed().as_secs_f64();
        println!("{name:>32}: {:>7.0}× real time", 10.0 / secs);
    }
}

#[test]
fn mono_parametric_eq_publishes_both_meter_channels() {
    let rig = Rig::new();
    let mut eq = rig.eq;
    let mut input = AudioBuffer::new(ChannelLayout::Mono, BLOCK);
    let mut output = AudioBuffer::new(ChannelLayout::Mono, BLOCK);
    input.set_len(BLOCK);
    output.set_len(BLOCK);
    input.channel_mut(0).fill(0.25);
    let transport = TransportInfo::default();
    for _ in 0..40 {
        eq.process(
            &PluginProcessContext {
                transport: &transport,
                param_events: &[],
                harmony: &crate::NO_HARMONY,
                param_mods: &[],
                note_mods: &[],
            },
            &mut NodeIo {
                frames: BLOCK,
                audio_in: std::slice::from_ref(&input),
                audio_out: std::slice::from_mut(&mut output),
                events_in: &[],
                events_out: &mut [],
            },
        );
    }
    for meter in [&rig.tap.meter_in, &rig.tap.meter_out] {
        assert!(meter.held(0) > 0.2);
        assert_eq!(meter.held(0), meter.held(1));
        assert_eq!(meter.mean_square(0), meter.mean_square(1));
        assert_eq!(meter.figure(0), meter.figure(1));
    }
}
