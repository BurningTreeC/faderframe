#![allow(clippy::unwrap_used)]
//! Multi-output plugins: one action makes tracks for a plugin's extra
//! outputs (routed like its track, in a folder, one undo step); a pad
//! sent to an extra output sounds on its track and not on the plugin's.

use faderframe_audio::dummy::DummyBackend;
use faderframe_core::{ParameterId, PluginInstanceId, TrackId, builtin};
use faderframe_engine::EngineConfig;
use faderframe_plugin_host::devices::drums;
use faderframe_project::{Command, InputRouting, PluginRef, Project, TrackKind};
use faderframe_session::{Action, AudioPreferences, PluginTarget, SelectMode, Session};
use std::path::Path;
use std::time::{Duration, Instant};

fn tone(path: &Path, freq: f64) {
    let x: Vec<f32> = (0..24_000)
        .map(|n| (0.5 * (std::f64::consts::TAU * freq * n as f64 / 48_000.0).sin()) as f32)
        .collect();
    faderframe_audio_files::write_wav(
        path,
        &[x],
        48_000,
        faderframe_audio_files::WavFormat::Pcm16,
        false,
    )
    .unwrap();
}

fn run(s: &mut Session, seconds: f64) {
    let end = Instant::now() + Duration::from_secs_f64(seconds);
    while Instant::now() < end {
        s.tick(0.016);
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// A kit on an instrument track, pad 1 loaded.
fn kit() -> (Session, TrackId, PluginInstanceId) {
    let dir = std::env::temp_dir().join(format!("ff-plugin-outputs-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    tone(&dir.join("kick.wav"), 80.0);
    let mut s = Session::new(Project::new("Kit", 48_000), None, EngineConfig::default()).unwrap();
    let t = s.add_track(TrackKind::Instrument).unwrap();
    s.place_plugin(
        t,
        PluginTarget::Instrument,
        PluginRef::builtin(builtin::DRUMS, "Drums"),
    )
    .unwrap();
    let plugin = s.project().track(t).unwrap().inserts[0].id;
    s.dispatch(Action::LoadDeviceSamples {
        plugin,
        slot: 0,
        files: vec![dir.join("kick.wav")],
    })
    .unwrap();
    for _ in 0..500 {
        s.tick(0.01);
        if !s.loading_samples(plugin) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    (s, t, plugin)
}

#[test]
fn output_tracks_are_made_in_one_step_and_carry_their_pads() {
    let (mut s, kit_track, plugin) = kit();
    let buses = s.plugin_output_buses(plugin);
    assert_eq!(buses.len(), 1 + drums::AUX);
    assert_eq!(buses[0].name, "Main");
    assert!(s.plugin_has_extra_outputs(plugin));
    let before = s.project().tracks.len();
    s.dispatch(Action::CreateOutputTracks {
        plugin,
        buses: None,
    })
    .unwrap();
    let p = s.project();
    assert_eq!(
        p.tracks.len(),
        before + 1 + drums::AUX,
        "a folder and a track each"
    );
    let outs = p.plugin_output_tracks(plugin);
    assert_eq!(outs.len(), drums::AUX);
    let host = p.track(kit_track).unwrap();
    let folder = outs[0].folder.unwrap();
    assert_eq!(p.track(folder).unwrap().kind, TrackKind::Folder);
    for (i, t) in outs.iter().enumerate() {
        assert_eq!(t.kind, TrackKind::Aux);
        assert_eq!(
            t.input,
            InputRouting::Plugin {
                plugin,
                bus: i as u16 + 1
            }
        );
        assert_eq!(t.output, host.output);
        assert_eq!(t.folder, Some(folder));
        // "Out 3" is only a number: the kit's track name goes first.
        assert_eq!(t.name, format!("{} Out {}", host.name, i + 1));
    }
    assert_eq!(s.input_label(outs[2]), "Drums · Out 3");
    // Again: nothing left to make.
    assert!(
        s.dispatch(Action::CreateOutputTracks {
            plugin,
            buses: None
        })
        .is_err()
    );
    // One undo step.
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(s.project().tracks.len(), before);
    s.dispatch(Action::Redo).unwrap();
    let out3 = s.project().plugin_output_tracks(plugin)[2].id;
    let out1 = s.project().plugin_output_tracks(plugin)[0].id;
    // Freezing the kit would silence them: refused.
    assert!(s.dispatch(Action::FreezeTrack(kit_track)).is_err());

    // Pad 1 to output 3: it plays there, not on the kit's own track.
    s.dispatch(Action::Edit(Command::SetPluginParameter {
        track: kit_track,
        plugin,
        parameter: ParameterId(drums::id::pad(0) + drums::id::OUTPUT),
        value: Some(3.0),
    }))
    .unwrap();
    s.start_audio(
        vec![Box::new(DummyBackend::default())],
        &AudioPreferences::default(),
    )
    .unwrap();
    s.dispatch(Action::SelectTracks {
        tracks: vec![kit_track],
        mode: SelectMode::Replace,
    })
    .unwrap();
    run(&mut s, 0.3);
    s.midi_keyboard().send(&[0x90, 36, 120]);
    run(&mut s, 0.15);
    let on3 = s.meter(out3).left.level_db;
    let on_kit = s.meter(kit_track).left.level_db;
    let on1 = s.meter(out1).left.level_db;
    eprintln!("out 3 {on3:.1} dBFS, kit {on_kit:.1}, out 1 {on1:.1}");
    assert!(on3 > -30.0, "the pad sounds on its output's track");
    assert!(
        on_kit < -70.0,
        "not on the kit's own (the meter's floor is -72)"
    );
    assert!(on1 < -70.0, "nor on another output");
    s.midi_keyboard().send(&[0x80, 36, 0]);
}

#[test]
fn a_track_can_take_one_output_by_hand() {
    let (mut s, kit_track, plugin) = kit();
    let aux = s.add_track(TrackKind::Aux).unwrap();
    let choices = s.input_choices(aux);
    let pick = choices
        .iter()
        .find(|c| c.label.ends_with("Out 2"))
        .unwrap_or_else(|| {
            panic!(
                "{:#?}",
                choices.iter().map(|c| &c.label).collect::<Vec<_>>()
            )
        });
    s.dispatch(pick.action.clone()).unwrap();
    let t = s.project().track(aux).unwrap();
    assert_eq!(t.input, InputRouting::Plugin { plugin, bus: 2 });
    // The kit's own track cannot take its outputs (a loop).
    assert!(
        !s.input_choices(kit_track)
            .iter()
            .any(|c| matches!(c.action, Action::Edit(_)) && c.label.contains("Out 2"))
    );
    // Only the missing ones are made now.
    s.dispatch(Action::CreateOutputTracks {
        plugin,
        buses: None,
    })
    .unwrap();
    assert_eq!(s.project().plugin_output_tracks(plugin).len(), drums::AUX);
}
