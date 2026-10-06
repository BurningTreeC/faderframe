#![allow(clippy::unwrap_used)]
mod common;
use common::TestProject;
use faderframe_core::{ChannelLayout, FaderLaw, ModulatorId, ParameterId, builtin, db_to_gain};
use faderframe_engine::{EngineConfig, offline::render_project};
use faderframe_project::modulation::{
    FollowSource, LfoShape, ModRate, ModRoute, ModSource, ModTarget, Modulator,
};
use faderframe_project::{OutputRouting, PluginRef, PluginSlot, TrackKind};
use faderframe_timeline::MusicalTime;

fn render(tp: &TestProject, frames: usize) -> Vec<Vec<f32>> {
    render_project(
        &tp.project,
        &tp.sources,
        EngineConfig::default(),
        256,
        0,
        frames,
    )
    .unwrap()
}

fn modulator(tp: &mut TestProject, source: ModSource, routes: &[(ModTarget, f32)]) -> Modulator {
    let id: ModulatorId = tp.project.ids.allocate();
    let mut m = Modulator::new(id, source);
    m.routes = routes
        .iter()
        .map(|&(target, depth)| ModRoute { target, depth })
        .collect();
    m
}

/// A stereo track playing a constant 0.5 for `frames`, to the master.
fn dc_track(tp: &mut TestProject, name: &str, frames: usize) -> faderframe_core::TrackId {
    let t = tp.track(TrackKind::Audio, name, ChannelLayout::Stereo);
    let src = tp.dc(2, 0.5, frames);
    tp.clip(t, src, MusicalTime::ZERO, frames as i64);
    t
}

#[test]
fn an_lfo_moves_the_fader_in_travel_on_top_of_its_value() {
    let mut tp = TestProject::new(48_000);
    let t = dc_track(&mut tp, "Tone", 48_000);
    // A square, a cycle a quarter (24000 frames at 120 bpm): up for the
    // first half of each quarter, down for the second.
    let m = modulator(
        &mut tp,
        ModSource::Lfo {
            shape: LfoShape::Square,
            rate: ModRate::Sync { beats: 1.0 },
            phase: 0.0,
        },
        &[(ModTarget::Volume, 0.1)],
    );
    tp.project.track_mut(t).unwrap().modulators.push(m);
    let out = render(&tp, 48_000);
    let law = FaderLaw::console();
    let unity = law.db_to_position(0.0);
    let up = 0.5 * db_to_gain(law.position_to_db(unity + 0.1));
    let down = 0.5 * db_to_gain(law.position_to_db(unity - 0.1));
    for (at, want) in [(6_000, up), (18_000, down), (30_000, up), (42_000, down)] {
        let got = out[0][at];
        assert!((got - want).abs() < 1e-3, "{at}: {got} vs {want}");
    }
    // The fader itself is where it was.
    assert_eq!(tp.project.track(t).unwrap().volume_db, 0.0);
    // Off: no modulation.
    tp.project.track_mut(t).unwrap().modulators[0].enabled = false;
    let out = render(&tp, 24_000);
    assert!((out[0][18_000] - 0.5).abs() < 1e-4);
}

#[test]
fn a_macro_moves_a_device_parameter_and_routes_to_one_parameter_add_up() {
    let mut tp = TestProject::new(48_000);
    let t = dc_track(&mut tp, "Tone", 48_000);
    let slot = PluginSlot {
        id: tp.project.ids.allocate(),
        plugin: PluginRef::builtin(builtin::GAIN, "Utility"),
        bypass: false,
        parameters: vec![],
        state: None,
        sidechain: None,
    };
    let gain = ModTarget::Plugin {
        plugin: slot.id,
        parameter: ParameterId(0),
    };
    tp.project.track_mut(t).unwrap().inserts.push(slot);
    // −60..+24 dB: two routes of −0.05 at full output, −8.4 dB together.
    let a = modulator(&mut tp, ModSource::Macro { value: 1.0 }, &[(gain, -0.05)]);
    let b = modulator(&mut tp, ModSource::Macro { value: 1.0 }, &[(gain, -0.05)]);
    tp.project.track_mut(t).unwrap().modulators = vec![a, b];
    let out = render(&tp, 48_000);
    let want = 0.5 * db_to_gain(-8.4);
    let got = out[0][47_000];
    assert!((got - want).abs() < 2e-3, "{got} vs {want}");
    // Half the macro: half the offset.
    for m in &mut tp.project.track_mut(t).unwrap().modulators {
        m.source = ModSource::Macro { value: 0.5 };
    }
    let out = render(&tp, 48_000);
    let want = 0.5 * db_to_gain(-4.2);
    assert!((out[0][47_000] - want).abs() < 2e-3);
}

#[test]
fn a_follower_hears_another_track_before_its_fader() {
    let mut tp = TestProject::new(48_000);
    // The key: loud for the first half second, then silent; not heard.
    let key = dc_track(&mut tp, "Key", 24_000);
    {
        let k = tp.project.track_mut(key).unwrap();
        k.output = OutputRouting::None;
        k.volume_db = -80.0;
    }
    let t = dc_track(&mut tp, "Pad", 96_000);
    let m = modulator(
        &mut tp,
        ModSource::Follower {
            source: FollowSource::Track { track: key },
            attack_ms: 5.0,
            release_ms: 30.0,
            gain_db: 0.0,
        },
        &[(ModTarget::Volume, -0.3)],
    );
    tp.project.track_mut(t).unwrap().modulators.push(m);
    let out = render(&tp, 96_000);
    // Ducked while the key plays (−6 dB peak key: 0.9 of the follower's
    // range), back to 0.5 once it has gone.
    let law = FaderLaw::console();
    let unity = law.db_to_position(0.0);
    let level = (faderframe_core::gain_to_db(0.5) + 60.0) / 60.0;
    let ducked = 0.5 * db_to_gain(law.position_to_db(unity - 0.3 * level));
    assert!(
        (out[0][20_000] - ducked).abs() < 5e-3,
        "{} vs {ducked}",
        out[0][20_000]
    );
    assert!((out[0][90_000] - 0.5).abs() < 1e-3, "{}", out[0][90_000]);
}

#[test]
fn tracks_following_each_other_still_build_and_renders_repeat() {
    let mut tp = TestProject::new(48_000);
    let a = dc_track(&mut tp, "A", 48_000);
    let b = dc_track(&mut tp, "B", 48_000);
    for (t, other) in [(a, b), (b, a)] {
        let follower = modulator(
            &mut tp,
            ModSource::Follower {
                source: FollowSource::Track { track: other },
                attack_ms: 10.0,
                release_ms: 100.0,
                gain_db: 0.0,
            },
            &[(ModTarget::Pan, 0.5)],
        );
        let random = modulator(
            &mut tp,
            ModSource::Random {
                rate: ModRate::Sync { beats: 0.25 },
                smooth: 0.5,
            },
            &[(ModTarget::Volume, 0.2)],
        );
        tp.project.track_mut(t).unwrap().modulators = vec![follower, random];
    }
    let first = render(&tp, 48_000);
    assert!(first[0].iter().any(|x| x.abs() > 0.1));
    assert_eq!(first, render(&tp, 48_000), "the same on every render");
}

/// An instrument track whose synth (a sine at −6 dB) plays A4 at
/// `velocity` for two quarters.
fn synth_track(tp: &mut TestProject, velocity: u8) -> (faderframe_core::TrackId, PluginSlot) {
    use faderframe_plugin_host::devices::synth::id::*;
    use faderframe_project::{Clip, ClipContent, MidiClip, MidiNote, SavedParameter};
    let t = tp.track(TrackKind::Instrument, "Keys", ChannelLayout::Stereo);
    let set = [
        (OSC1_WAVE, 3.0),
        (OSC2_LEVEL, 0.0),
        (DETUNE, 0.0),
        (CUTOFF, 20_000.0),
        (RESONANCE, 0.0),
        (ENV_AMOUNT, 0.0),
        (ATTACK, 1.0),
        (SUSTAIN, 1.0),
        (RELEASE, 5.0),
        (WIDTH, 0.0),
        (VELOCITY_AMP, 0.0),
        (VELOCITY_CUTOFF, 0.0),
    ];
    let slot = PluginSlot {
        id: tp.project.ids.allocate(),
        plugin: PluginRef::builtin(builtin::SYNTH, "Synth"),
        bypass: false,
        parameters: set
            .iter()
            .map(|(id, value)| SavedParameter {
                id: ParameterId(*id),
                value: *value,
            })
            .collect(),
        state: None,
        sidechain: None,
    };
    tp.project.track_mut(t).unwrap().inserts.push(slot.clone());
    let note = MidiNote {
        id: tp.project.ids.allocate(),
        start: MusicalTime::ZERO,
        length: MusicalTime::from_quarters(2.0),
        key: 69,
        velocity,
        channel: 0,
        muted: false,
    };
    let id = tp.project.ids.allocate();
    tp.project.clips.insert(
        id,
        Clip {
            id,
            track: t,
            name: "Note".into(),
            color: None,
            start: MusicalTime::ZERO,
            muted: false,
            content: ClipContent::Midi(MidiClip {
                length: MusicalTime::from_quarters_i(4),
                notes: vec![note],
                controllers: Vec::new(),
                expressions: Vec::new(),
                sysex: Vec::new(),
            }),
        },
    );
    tp.project.track_mut(t).unwrap().clips.push(id);
    (t, slot)
}

fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt()
}

#[test]
fn a_note_source_moves_the_instrument_by_the_note() {
    let level = |velocity: u8, depth: Option<f32>| {
        let mut tp = TestProject::new(48_000);
        let (t, slot) = synth_track(&mut tp, velocity);
        if let Some(depth) = depth {
            let volume = ModTarget::Plugin {
                plugin: slot.id,
                parameter: ParameterId(faderframe_plugin_host::devices::synth::id::VOLUME),
            };
            let m = modulator(&mut tp, ModSource::Velocity, &[(volume, depth)]);
            tp.project.track_mut(t).unwrap().modulators.push(m);
        }
        let out = render(&tp, 24_000);
        rms(&out[0][8_000..20_000])
    };
    let plain = level(127, None);
    assert!(plain > 0.01, "{plain}");
    // −0.2 of the volume's 54 dB at full velocity: −10.8 dB.
    let hard = level(127, Some(-0.2));
    let db = 20.0 * (hard / plain).log10();
    assert!((db + 10.8).abs() < 0.5, "{db}");
    // Played half as hard: half of that.
    let soft = level(64, Some(-0.2));
    let db = 20.0 * (soft / plain).log10();
    assert!((db + 10.8 * 64.0 / 127.0).abs() < 0.5, "{db}");
}
