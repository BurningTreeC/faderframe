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

impl Rig {
    fn new() -> Self {
        let params = ParamValues::new(parameters());
        let tap = Arc::new(AnalysisTap::new(params.clone(), BANDS));
        let config = ProcessConfig {
            sample_rate: SR,
            max_block_size: BLOCK as u32,
            sidechain: true,
            double_precision: false,
        };
        let eq = EqProcessor::new(params.clone(), Arc::clone(&tap), &config);
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

    fn set_band(&self, band: usize, field: Field, value: f64) {
        self.params.set_by_id(band_id(band, field), value).unwrap();
    }

    fn set_global(&self, g: usize, value: f64) {
        self.params.set_by_id(global_id(g), value).unwrap();
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
}

fn sine(f: f64) -> impl Fn(usize) -> (f32, f32) {
    move |n| {
        let v = (0.25 * (std::f64::consts::TAU * f * n as f64 / SR).sin()) as f32;
        (v, v)
    }
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

fn db(x: f64) -> f64 {
    20.0 * x.log10()
}

#[test]
fn a_band_does_what_its_design_says() {
    let mut rig = Rig::new();
    rig.set_band(0, Field::Enabled, 1.0);
    rig.set_band(0, Field::Freq, 1_000.0);
    rig.set_band(0, Field::Gain, 9.0);
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
                slope: 12,
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
    rig.set_band(0, Field::Slope, 7.0); // 96 dB/oct
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
        rig.set_band(0, Field::Enabled, 1.0);
        rig.set_band(0, Field::Freq, 1_000.0);
        rig.set_band(0, Field::Gain, 6.0);
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
    rig.set_band(0, Field::Enabled, 1.0);
    rig.set_band(0, Field::Freq, 1_000.0);
    rig.set_band(0, Field::Gain, -12.0);
    rig.set_band(0, Field::Placement, Placement::Left.index() as f64);
    let (l, r) = rig.run(40, 0, sine(1_000.0), silence, &[]);
    assert!((db(peak(&l[5000..]) / 0.25) + 12.0).abs() < 0.05);
    assert!(db(peak(&r[5000..]) / 0.25).abs() < 0.01);
}

#[test]
fn a_dynamic_band_moves_with_its_key() {
    let mut rig = Rig::new();
    rig.set_band(0, Field::Enabled, 1.0);
    rig.set_band(0, Field::Freq, 2_000.0);
    rig.set_band(0, Field::Gain, 0.0);
    rig.set_band(0, Field::Range, -9.0);
    rig.set_band(0, Field::Threshold, -40.0);
    rig.set_global(global::ATTACK, 1.0);
    rig.set_global(global::RELEASE, 20.0);
    // Loud at the band's frequency: it cuts by the full range.
    let (l, _) = rig.run(60, 0, sine(2_000.0), silence, &[]);
    let level = db(peak(&l[l.len() - 4096..]) / 0.25);
    assert!((level + 9.0).abs() < 0.3, "{level}");
    assert!((f64::from(rig.tap.value(0)) + 9.0).abs() < 0.3);
    // Quiet: it lets go.
    let quiet = |n: usize| {
        let v = (0.001 * (std::f64::consts::TAU * 2_000.0 * n as f64 / SR).sin()) as f32;
        (v, v)
    };
    let (l, _) = rig.run(60, 0, quiet, silence, &[]);
    assert!(db(peak(&l[l.len() - 4096..]) / 0.001).abs() < 0.2);
    // Keyed from the sidechain instead: the signal is quiet, the key loud.
    rig.set_global(global::SIDECHAIN, 1.0);
    let (l, _) = rig.run(60, 0, quiet, sine(2_000.0), &[]);
    assert!((db(peak(&l[l.len() - 4096..]) / 0.001) + 9.0).abs() < 0.3);
    assert_eq!(dynamic_gain(-6.0, -20.0, -30.0), 0.0);
    assert_eq!(dynamic_gain(-6.0, -20.0, -17.0), -3.0);
    assert_eq!(dynamic_gain(6.0, -20.0, 0.0), 6.0);
}

#[test]
fn auto_gain_keeps_pink_noise_level() {
    let mut bands = [BandParams::read(&ParamValues::new(parameters()), 0); BANDS];
    bands[0].enabled = true;
    bands[0].kind = BandType::HighShelf;
    bands[0].freq = 3_000.0;
    bands[0].gain = 6.0;
    let change = loudness_change(&bands, 1.0, SR);
    // A +6 dB shelf over the top 2.4 of 9 octaves: about +1.6 dB of
    // pink noise power... in that region weighted by octaves.
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
fn listening_to_a_band_plays_its_region() {
    let mut rig = Rig::new();
    rig.set_band(2, Field::Enabled, 1.0);
    rig.set_band(2, Field::Freq, 500.0);
    rig.set_band(2, Field::Gain, 6.0);
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
fn automation_lands_inside_the_block() {
    let mut rig = Rig::new();
    rig.set_band(0, Field::Enabled, 1.0);
    rig.set_band(0, Field::Freq, 1_000.0);
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
    rig.run(4, 0, sine(1_000.0), silence, &[]);
    assert_eq!(rig.tap.output.written(), 0);
    rig.tap.watch();
    rig.run(4, 0, sine(1_000.0), silence, &[]);
    assert_eq!(rig.tap.input.written(), 4 * BLOCK as u64);
    let (mut a, mut b) = (vec![0.0; 1024], vec![0.0; 1024]);
    rig.tap.output.latest(&mut a, &mut b);
    assert!(peak(&a) > 0.2);
    const { assert!(RING_FRAMES >= 4 * BLOCK) };
    // The meters see the input and output.
    assert!(rig.tap.meter_in.held(0) > 0.2);
}

#[test]
fn the_formatting_names_choices() {
    assert_eq!(format(band_id(3, Field::Type), 3.0).unwrap(), "Low Cut");
    assert_eq!(format(band_id(3, Field::Slope), 5.0).unwrap(), "48 dB/oct");
    assert_eq!(format(band_id(3, Field::Placement), 4.0).unwrap(), "Side");
    assert_eq!(format(band_id(3, Field::Freq), 1234.0).unwrap(), "1.23 kHz");
    assert_eq!(format(band_id(3, Field::Range), 0.0).unwrap(), "Off");
    assert_eq!(
        format(global_id(global::PHASE), 1.0).unwrap(),
        "Linear Phase"
    );
    assert!(format(global_id(global::OUTPUT), 1.0).is_none());
    assert_eq!(parameters().len(), GLOBALS + BANDS * FIELDS);
    // Indexes and ids agree.
    let ps = parameters();
    for b in [0, 7, 23] {
        for f in Field::ALL {
            assert_eq!(ps[band_index(b, f)].id, band_id(b, f));
        }
    }
}

mod linear_phase {
    use super::*;
    use crate::eq::linear;

    fn linear_rig(quality: f64) -> Rig {
        let rig = Rig::new();
        rig.set_global(global::PHASE, 1.0);
        rig.set_global(global::QUALITY, quality);
        // The mode is fixed when the processor is made.
        let config = ProcessConfig {
            sample_rate: SR,
            max_block_size: BLOCK as u32,
            sidechain: true,
            double_precision: false,
        };
        let eq = EqProcessor::new(rig.params.clone(), Arc::clone(&rig.tap), &config);
        Rig { eq, ..rig }
    }

    #[test]
    fn an_impulse_comes_out_at_the_latency_and_symmetric() {
        let mut rig = linear_rig(0.0);
        rig.set_band(0, Field::Enabled, 1.0);
        rig.set_band(0, Field::Freq, 1_000.0);
        rig.set_band(0, Field::Gain, 6.0);
        rig = {
            let config = ProcessConfig {
                sample_rate: SR,
                max_block_size: BLOCK as u32,
                sidechain: true,
                double_precision: false,
            };
            let eq = EqProcessor::new(rig.params.clone(), Arc::clone(&rig.tap), &config);
            Rig { eq, ..rig }
        };
        let lat = linear::latency(0) as usize;
        let impulse = |n: usize| if n == 0 { (1.0, 1.0) } else { (0.0, 0.0) };
        let (l, _) = rig.run(lat * 2 / BLOCK + 2, 0, impulse, silence, &[]);
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
        assert_eq!(rig.eq.linear.as_ref().unwrap().latency() as usize, lat);
    }

    #[test]
    fn it_follows_the_analog_curve_to_nyquist() {
        // Where minimum phase cannot quite: a broad 12 kHz bell.
        const SR44: f64 = 44_100.0;
        let params = ParamValues::new(parameters());
        params.set_by_id(global_id(global::PHASE), 1.0).unwrap();
        params.set_by_id(global_id(global::QUALITY), 1.0).unwrap();
        params.set_by_id(band_id(0, Field::Enabled), 1.0).unwrap();
        params.set_by_id(band_id(0, Field::Freq), 12_000.0).unwrap();
        params.set_by_id(band_id(0, Field::Gain), 9.0).unwrap();
        params.set_by_id(band_id(0, Field::Q), 0.7).unwrap();
        let shape = BandParams::read(&params, 0).shape(1.0);
        let mut planner = realfft::RealFftPlanner::<f64>::new();
        let n = linear::LENGTHS[1];
        // The kernel's response: transform the first path's FIR again.
        let tap = Arc::new(AnalysisTap::new(params.clone(), BANDS));
        let config = ProcessConfig {
            sample_rate: SR44,
            max_block_size: BLOCK as u32,
            sidechain: false,
            double_precision: false,
        };
        let mut eq = EqProcessor::new(params.clone(), tap, &config);
        let lat = linear::latency(1) as usize;
        let mut ins = vec![AudioBuffer::new(ChannelLayout::Stereo, BLOCK)];
        let mut outs = vec![AudioBuffer::new(ChannelLayout::Stereo, BLOCK)];
        ins[0].set_len(BLOCK);
        outs[0].set_len(BLOCK);
        let mut ir = Vec::new();
        let transport = TransportInfo::default();
        for b in 0..(lat + n) / BLOCK + 2 {
            for i in 0..BLOCK {
                let v = if b == 0 && i == 0 { 1.0 } else { 0.0 };
                ins[0].channel_mut(0)[i] = v;
                ins[0].channel_mut(1)[i] = v;
            }
            let ctx = PluginProcessContext {
                transport: &transport,
                param_events: &[],
            };
            let mut io = NodeIo {
                frames: BLOCK,
                audio_in: &ins,
                audio_out: &mut outs,
                events_in: &[],
                events_out: &mut [],
            };
            eq.process(&ctx, &mut io);
            ir.extend(outs[0].channel(0).iter().map(|v| f64::from(*v)));
        }
        let mag = |f: f64| {
            let w = std::f64::consts::TAU * f / SR44;
            let (mut re, mut im) = (0.0, 0.0);
            for (k, x) in ir.iter().enumerate() {
                re += x * (w * k as f64).cos();
                im -= x * (w * k as f64).sin();
            }
            20.0 * re.hypot(im).log10()
        };
        for f in [1_000.0, 6_000.0, 12_000.0, 16_000.0, 19_000.0, 20_000.0] {
            let got = mag(f);
            let want = design::analog_db(&shape, f);
            assert!(
                (got - want).abs() < 0.1,
                "{f} Hz: {got:.3} vs analog {want:.3}"
            );
        }
        let _ = &mut planner;
    }

    #[test]
    fn mid_and_side_stay_apart() {
        let base = Rig::new();
        base.set_global(global::PHASE, 1.0);
        base.set_global(global::QUALITY, 0.0);
        base.set_band(0, Field::Enabled, 1.0);
        base.set_band(0, Field::Freq, 1_000.0);
        base.set_band(0, Field::Gain, 6.0);
        base.set_band(0, Field::Placement, Placement::Mid.index() as f64);
        let config = ProcessConfig {
            sample_rate: SR,
            max_block_size: BLOCK as u32,
            sidechain: true,
            double_precision: false,
        };
        let eq = EqProcessor::new(base.params.clone(), Arc::clone(&base.tap), &config);
        let mut rig = Rig { eq, ..base };
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
        rig.set_band(0, Field::Enabled, 1.0);
        rig.set_band(0, Field::Freq, 1_000.0);
        rig.set_band(0, Field::Gain, -12.0);
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
        assert!(!parameters()[global::PHASE].automatable);
    }
}
