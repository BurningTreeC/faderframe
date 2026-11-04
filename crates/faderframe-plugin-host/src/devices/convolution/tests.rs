#![allow(clippy::unwrap_used)]

use super::*;
use crate::devices::rig::{Rig, SR, silence};
use crate::devices::samples::{self, Loaded, SampleDoc};
use faderframe_realtime::TryCell;

fn rig_with(set: &[(u32, f64)], files: samples::Shared, now: bool) -> Rig<ConvolutionProcessor> {
    Rig::with(parameters(), TAP_VALUES, set, |p, t, c| {
        ConvolutionProcessor::new(p, t, c, files, now)
    })
}

fn rig(set: &[(u32, f64)]) -> Rig<ConvolutionProcessor> {
    rig_with(set, samples::empty(), true)
}

fn impulse(at: usize) -> impl Fn(usize) -> (f32, f32) {
    move |n| if n == at { (1.0, 1.0) } else { (0.0, 0.0) }
}

fn energy(x: &[f32]) -> f64 {
    x.iter().map(|v| f64::from(*v).powi(2)).sum()
}

/// Run blocks of silence until the designer has made what was asked (it
/// works on a thread of its own) and the processor has taken it in.
fn settle(r: &mut Rig<ConvolutionProcessor>) {
    r.p.idle.store(false, Ordering::Release);
    let quiet = |r: &Rig<ConvolutionProcessor>| {
        r.p.old.is_none() && !r.p.engine.switching() && r.p.held.iter().all(Option::is_none)
    };
    for _ in 0..400 {
        std::thread::sleep(std::time::Duration::from_millis(5));
        let idle = r.p.idle.load(Ordering::Acquire);
        // A block takes whatever was sent before the designer went idle.
        r.run(0.02, silence, silence);
        if idle && quiet(r) {
            return;
        }
    }
    panic!("the designer never finished");
}

/// A file with four channels: each path an impulse at its own place.
fn true_stereo_file(name: &str) -> samples::Shared {
    let dir = samples::fixtures::dir(name);
    let mut ch = vec![vec![0.0f32; 2_000]; 4];
    ch[0][100] = 1.0;
    ch[1][200] = 0.5;
    ch[2][300] = 0.25;
    ch[3][400] = 0.125;
    let path = dir.join("room.wav");
    faderframe_audio_files::write_wav(
        &path,
        &ch,
        48_000,
        faderframe_audio_files::WavFormat::Float32,
        false,
    )
    .unwrap();
    let mut doc = SampleDoc::default();
    doc.set(0, Some(path.to_string_lossy().into_owned()));
    let set = samples::load(&doc);
    assert!(set.errors.is_empty(), "{:?}", set.errors);
    Arc::new(TryCell::new(Loaded {
        generation: 1,
        set: Some(Arc::new(set)),
    }))
}

#[test]
fn a_built_in_space_rings_on_without_latency() {
    // The plate (no direct sound): wet only.
    let plate = spaces::SPACES.len() as f64;
    let mut r = rig(&[(id::SPACE, plate), (id::MIX, 1.0)]);
    let (l, rr) = r.run(3.0, impulse(1_000), silence);
    assert!(l[..1_000].iter().all(|v| *v == 0.0), "nothing before");
    // At once (within a millisecond: the plate's onset).
    assert!(energy(&l[1_000..1_048]) > 0.0, "no latency");
    // Ringing for its decay: still there a second on, much quieter two
    // seconds on.
    let at = |s: f64| energy(&l[1_000 + (s * SR) as usize..1_000 + (s * SR) as usize + 4_800]);
    assert!(at(1.0) > 0.0 && at(0.1) > 10.0 * at(1.0) && at(1.0) > 10.0 * at(2.2));
    // Decorrelated sides.
    assert!(l.iter().zip(&rr).any(|(a, b)| (a - b).abs() > 1e-3));
    assert_eq!(r.tap.value(value::CHANNELS), 4.0);
    let decay = r.tap.value(value::DECAY);
    assert!((decay - 1.9).abs() < 0.6, "{decay}");
    let view = r.tap.assets::<ImpulseView>().unwrap();
    assert_eq!(view.name, "Plate");
    assert!(view.envelope.len() > 100 && view.decay_db.len() == view.envelope.len());
}

#[test]
fn a_true_stereo_file_plays_its_four_paths() {
    let mut r = rig_with(
        &[(id::SPACE, 0.0), (id::MIX, 1.0), (id::NORMALISE, 0.0)],
        true_stereo_file("ir-paths"),
        true,
    );
    assert_eq!(r.tap.value(value::CHANNELS), 4.0);
    // The left input alone: left out at 100, right out at 200.
    let (l, rr) = r.run(
        0.1,
        |n| if n == 0 { (1.0, 0.0) } else { (0.0, 0.0) },
        silence,
    );
    let peak = |x: &[f32]| {
        x.iter()
            .enumerate()
            .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
            .map(|(i, v)| (i, *v))
            .unwrap()
    };
    let (i, v) = peak(&l);
    assert_eq!(i, 100);
    assert!((v - 1.0).abs() < 1e-4);
    let (i, v) = peak(&rr);
    assert_eq!(i, 200);
    assert!((v - 0.5).abs() < 1e-4);
    // The right input alone: left out at 300, right out at 400.
    let start = r.at;
    let (l, rr) = r.run(
        0.1,
        move |n| if n == start { (0.0, 1.0) } else { (0.0, 0.0) },
        silence,
    );
    let (i, v) = peak(&l);
    assert_eq!(i, 300);
    assert!((v - 0.25).abs() < 1e-4);
    let (i, v) = peak(&rr);
    assert_eq!(i, 400);
    assert!((v - 0.125).abs() < 1e-4);
}

#[test]
fn without_a_file_the_wet_signal_is_silent_and_the_dry_passes() {
    let mut r = rig(&[(id::SPACE, 0.0), (id::MIX, 0.5)]);
    let (l, _) = r.run(0.2, crate::devices::rig::tone(440.0, 0.5), silence);
    let x = crate::devices::rig::tone(440.0, 0.5);
    for (n, v) in l.iter().enumerate() {
        assert!((v - 0.5 * x(n).0).abs() < 1e-6, "at {n}");
    }
    let view = r.tap.assets::<ImpulseView>().unwrap();
    assert!(view.problem.is_some());
    // No mix: the input exactly.
    let mut r = rig(&[(id::MIX, 0.0)]);
    let (l, _) = r.run(0.2, crate::devices::rig::tone(440.0, 0.5), silence);
    for (n, v) in l.iter().enumerate() {
        assert!((v - x(n).0).abs() < 1e-6);
    }
}

#[test]
fn pre_delay_waits_and_the_cuts_and_width_shape_the_wet_signal() {
    let space = space_named("Small Room").unwrap();
    let mut r = rig(&[(id::SPACE, space), (id::MIX, 1.0), (id::PRE_DELAY, 100.0)]);
    let (l, _) = r.run(0.5, impulse(0), silence);
    let first = l.iter().position(|v| v.abs() > 1e-6).unwrap();
    // 100 ms, then the direct sound's way through the room (a few ms).
    assert!(
        (4_800..4_800 + 600).contains(&first),
        "the wet signal from {first}"
    );
    // Mono width: both sides alike.
    let mut r = rig(&[(id::SPACE, space), (id::MIX, 1.0), (id::WIDTH, 0.0)]);
    let (l, rr) = r.run(0.5, impulse(0), silence);
    for (a, b) in l.iter().zip(&rr) {
        assert!((a - b).abs() < 1e-6);
    }
    // A high cut darkens the tail: white noise through the room, the
    // highs measured.
    let noise = |n: usize| {
        let v = crate::devices::rig::white(n, 3) as f32 * 0.1;
        (v, v)
    };
    let mut open = rig(&[(id::SPACE, space), (id::MIX, 1.0)]);
    let mut dark = rig(&[(id::SPACE, space), (id::MIX, 1.0), (id::HIGH_CUT, 2_000.0)]);
    let (a, _) = open.run(1.0, noise, silence);
    let (b, _) = dark.run(1.0, noise, silence);
    let high = |x: &[f32]| {
        // A first difference: the highs.
        x.windows(2)
            .map(|w| f64::from(w[1] - w[0]).powi(2))
            .sum::<f64>()
    };
    assert!(high(&b) < 0.2 * high(&a));
}

#[test]
fn shaping_the_response_takes_effect_while_it_plays() {
    let space = space_named("Hall").unwrap();
    let mut r = rig(&[(id::SPACE, space), (id::MIX, 1.0)]);
    let long = f64::from(r.tap.value(value::SECONDS));
    assert!(long > 2.0, "{long}");
    let (_, _) = r.run(0.5, crate::devices::rig::tone(300.0, 0.3), silence);
    // Shorter, reversed and stretched: the designer makes it on its
    // thread; the engine takes it in without a new engine (the same
    // source), the history kept.
    r.set(id::LENGTH, 0.25);
    settle(&mut r);
    let short = f64::from(r.tap.value(value::SECONDS));
    assert!((short / long - 0.25).abs() < 0.02, "{short} of {long}");
    assert_eq!(r.p.engine.tag(), 0, "the same engine");
    r.set(id::STRETCH, 2.0);
    settle(&mut r);
    let stretched = f64::from(r.tap.value(value::SECONDS));
    assert!((stretched / short - 2.0).abs() < 0.02, "{stretched}");
    // Another space: a new engine, crossfaded in.
    r.set(id::SPACE, space_named("Plate").unwrap());
    settle(&mut r);
    assert!(r.p.engine.tag() > 0);
    assert_eq!(r.tap.assets::<ImpulseView>().unwrap().name, "Plate");
    // And it plays: an impulse rings.
    let at = r.at;
    let (l, _) = r.run(
        0.5,
        move |n| if n == at { (1.0, 1.0) } else { (0.0, 0.0) },
        silence,
    );
    assert!(energy(&l[..4_800]) > 1e-3);
}

#[test]
fn a_live_processor_makes_its_first_response_in_the_background() {
    let space = space_named("Studio").unwrap();
    let mut r = rig_with(
        &[(id::SPACE, space), (id::MIX, 1.0)],
        samples::empty(),
        false,
    );
    // Made silent at once (nothing waited for).
    assert_eq!(r.p.engine.capacity(), 0);
    settle(&mut r);
    assert!(r.p.engine.capacity() > 0);
    let at = r.at;
    let (l, _) = r.run(
        0.3,
        move |n| if n == at { (1.0, 1.0) } else { (0.0, 0.0) },
        silence,
    );
    assert!(energy(&l) > 1e-3);
}

#[test]
fn a_reset_leaves_no_tail() {
    let mut r = rig(&[
        (id::SPACE, space_named("Cathedral").unwrap()),
        (id::MIX, 1.0),
    ]);
    r.run(0.5, crate::devices::rig::tone(200.0, 0.5), silence);
    r.p.reset();
    let (l, rr) = r.run(2.0, silence, silence);
    assert!(l.iter().chain(&rr).all(|v| *v == 0.0));
}

#[test]
fn values_read_as_they_should() {
    assert_eq!(format(ParameterId(id::SPACE), 0.0).unwrap(), "Impulse File");
    assert_eq!(format(ParameterId(id::SPACE), 5.0).unwrap(), "Hall");
    assert_eq!(format(ParameterId(id::LOW_CUT), 20.0).unwrap(), "Off");
    assert_eq!(format(ParameterId(id::STRETCH), 1.5).unwrap(), "1.50×");
    // Ids are their positions; what reshapes is not automatable.
    for (i, p) in parameters().iter().enumerate() {
        assert_eq!(p.id.0 as usize, i);
        let reshapes = p.id.0 == id::SPACE || p.id.0 >= id::START;
        assert_eq!(p.automatable, !reshapes, "{}", p.name);
    }
}

/// What the three devices cost (run by hand in release:
/// `cargo test -p faderframe-plugin-host --release --lib device_costs -- --ignored --nocapture`).
#[test]
#[ignore]
fn device_costs() {
    use crate::devices::{multiband, vocoder};
    let seconds = 10.0;
    let x = crate::devices::rig::tone(220.0, 0.3);
    let time = |name: &str, f: &mut dyn FnMut()| {
        let t = std::time::Instant::now();
        f();
        let s = t.elapsed().as_secs_f64();
        println!("{name:40} {:6.2} % of a core", 100.0 * s / seconds);
    };
    for space in ["Small Room", "Cathedral"] {
        let mut r = rig(&[(id::SPACE, space_named(space).unwrap()), (id::MIX, 0.5)]);
        time(&format!("convolution, {space} (true stereo)"), &mut || {
            r.run(seconds, &x, silence);
        });
    }
    let mut r = Rig::with(
        multiband::parameters(),
        multiband::TAP_VALUES,
        &[(multiband::id::BANDS, 6.0)],
        multiband::MultibandProcessor::new,
    );
    time("multiband, 6 bands, minimum phase", &mut || {
        r.run(seconds, &x, silence);
    });
    let mut r = Rig::with(
        multiband::parameters(),
        multiband::TAP_VALUES,
        &[(multiband::id::BANDS, 6.0), (multiband::id::PHASE, 1.0)],
        multiband::MultibandProcessor::new,
    );
    time("multiband, 6 bands, linear phase", &mut || {
        r.run(seconds, &x, silence);
    });
    for (bands, label) in [(2.0, "16"), (6.0, "40")] {
        let mut r = Rig::with(
            vocoder::parameters(),
            vocoder::TAP_VALUES,
            &[(vocoder::id::BANDS, bands)],
            vocoder::VocoderProcessor::new,
        );
        time(&format!("vocoder, {label} bands, chord"), &mut || {
            r.run(seconds, &x, silence);
        });
    }
}
