#![allow(clippy::unwrap_used)]
//! The Wavetable, FM and Granular in a session: a user wavetable copied
//! in and read at the frame size the document gives (one undo step each),
//! saved relative and reopened; DX7 voices read into the FM's bank and
//! loaded as one undo step; the Granular's sample; the three played live.

use faderframe_audio::dummy::DummyBackend;
use faderframe_core::{PluginInstanceId, builtin};
use faderframe_engine::EngineConfig;
use faderframe_plugin_host::devices::fm;
use faderframe_plugin_host::devices::samples::Contents;
use faderframe_project::{PluginRef, Project, TrackKind};
use faderframe_session::{Action, AudioPreferences, PluginTarget, Session};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn run(s: &mut Session, d: Duration) {
    let start = Instant::now();
    while start.elapsed() < d {
        std::thread::sleep(Duration::from_millis(5));
        s.tick(0.005);
    }
}

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ff-instruments-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn with(id: &str) -> (Session, PluginInstanceId) {
    let mut s = Session::new(
        Project::new("Synths", 48_000),
        None,
        EngineConfig::default(),
    )
    .unwrap();
    let t = s.add_track(TrackKind::Instrument).unwrap();
    s.place_plugin(t, PluginTarget::Instrument, PluginRef::builtin(id, id))
        .unwrap();
    let plugin = s.project().track(t).unwrap().inserts[0].id;
    (s, plugin)
}

fn wait(s: &mut Session, plugin: PluginInstanceId) {
    for _ in 0..1_000 {
        s.tick(0.01);
        if !s.loading_samples(plugin) {
            s.wait_for_samples();
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    panic!("the samples never loaded");
}

/// Eight frames of 1024 samples: a sine growing a fifth harmonic.
fn table_wav(path: &Path) {
    let x: Vec<f32> = (0..8 * 1024)
        .map(|i| {
            let (f, n) = (i / 1024, i % 1024);
            let p = std::f64::consts::TAU * n as f64 / 1024.0;
            (0.4 * (p.sin() + f as f64 / 8.0 * (5.0 * p).sin())) as f32
        })
        .collect();
    faderframe_audio_files::write_wav(
        path,
        &[x],
        48_000,
        faderframe_audio_files::WavFormat::Float32,
        false,
    )
    .unwrap();
}

fn frames(s: &Session, plugin: PluginInstanceId, slot: usize) -> Option<usize> {
    s.plugin_tap(plugin)
        .and_then(|t| t.assets::<Contents>())
        .and_then(|c| c.set.tables.get(slot).cloned().flatten())
        .map(|t| t.frames)
}

#[test]
fn a_user_wavetable_loads_by_its_frame_size_saves_and_reopens() {
    let outside = scratch("wt-src");
    table_wav(&outside.join("growl.wav"));
    let (mut s, plugin) = with(builtin::WAVETABLE);
    s.dispatch(Action::LoadDeviceSamples {
        plugin,
        slot: 1,
        files: vec![outside.join("growl.wav")],
    })
    .unwrap();
    wait(&mut s, plugin);
    // 8192 samples, no chunk: frames of 2048 guessed.
    assert_eq!(frames(&s, plugin, 1), Some(4));
    assert_eq!(s.wavetable_cycle(plugin, 1), 0);
    s.dispatch(Action::SetWavetableCycle {
        plugin,
        slot: 1,
        samples: 1024,
    })
    .unwrap();
    wait(&mut s, plugin);
    assert_eq!(frames(&s, plugin, 1), Some(8), "read in frames of 1024");
    assert_eq!(s.wavetable_cycle(plugin, 1), 1024);
    assert_eq!(
        s.history_steps().0.last().map(String::as_str),
        Some("Wavetable Frame Size")
    );
    s.dispatch(Action::Undo).unwrap();
    wait(&mut s, plugin);
    assert_eq!(frames(&s, plugin, 1), Some(4));
    s.dispatch(Action::Redo).unwrap();
    wait(&mut s, plugin);
    // Saved, the table's path is relative to the project; reopened, it
    // loads again with its frame size.
    let dir = scratch("wt-project");
    let path = dir.join("Synths.ffproj");
    s.save_as(&path).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(!text.contains(&outside.display().to_string()));
    let mut s = Session::new(Project::new("Other", 48_000), None, EngineConfig::default()).unwrap();
    s.open(&path).unwrap();
    let plugin = s.project().tracks[0].inserts[0].id;
    wait(&mut s, plugin);
    assert_eq!(s.wavetable_cycle(plugin, 1), 1024);
    assert_eq!(frames(&s, plugin, 1), Some(8));
    let _ = std::fs::remove_dir_all(&outside);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A bulk dump of 32 packed voices, each with its algorithm.
fn bank(path: &Path) {
    let mut dump = vec![0xF0, 0x43, 0x00, 0x09, 0x20, 0x00];
    let mut sum = 0u32;
    for v in 0..32u8 {
        let mut d = [0u8; 128];
        for block in 0..6 {
            let o = &mut d[block * 17..block * 17 + 17];
            o[0..4].copy_from_slice(&[90, 40, 30, 60]);
            o[4..8].copy_from_slice(&[99, 85, 70, 0]);
            o[8] = 39;
            o[14] = if block == 5 { 99 } else { 70 };
            o[15] = 1 << 1;
        }
        d[106..110].copy_from_slice(&[50, 50, 50, 50]);
        d[110] = v;
        d[117] = 24;
        let name = format!("VOICE {v:02}");
        d[118..128].copy_from_slice(format!("{name:<10}").as_bytes());
        sum += d.iter().map(|b| u32::from(*b)).sum::<u32>();
        dump.extend_from_slice(&d);
    }
    dump.push(((128 - (sum & 0x7f)) & 0x7f) as u8);
    dump.push(0xF7);
    std::fs::write(path, dump).unwrap();
}

#[test]
fn dx7_voices_load_into_the_fm_as_one_undo_step() {
    let dir = scratch("dx7");
    bank(&dir.join("bank.syx"));
    let (mut s, plugin) = with(builtin::FM);
    s.dispatch(Action::ImportDx7 {
        plugin,
        path: dir.join("bank.syx"),
    })
    .unwrap();
    let b = s.dx7_bank(plugin).unwrap();
    assert_eq!(b.voices.len(), 32);
    assert_eq!(b.names()[7], "VOICE 07");
    let steps = s.history_steps().0.len();
    s.dispatch(Action::LoadDx7Voice { plugin, index: 7 })
        .unwrap();
    assert_eq!(s.history_steps().0.len(), steps + 1);
    assert_eq!(
        s.history_steps().0.last().map(String::as_str),
        Some("Load DX7 Voice \"VOICE 07\"")
    );
    let value =
        |s: &Session, id: u32| f64::from(s.plugin_tap(plugin).unwrap().params.get(id as usize));
    assert_eq!(value(&s, fm::id::ALGORITHM), 7.0, "algorithm 8");
    assert_eq!(value(&s, fm::id::op(1, fm::id::LEVEL)), 70.0);
    assert_eq!(s.dx7_bank(plugin).unwrap().loaded, Some(7));
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(value(&s, fm::id::ALGORITHM), 0.0);
    assert_eq!(value(&s, fm::id::op(1, fm::id::LEVEL)), 0.0);
    // Not a dump: a notice, the bank as it was.
    std::fs::write(dir.join("junk.syx"), [1u8, 2, 3]).unwrap();
    s.dispatch(Action::ImportDx7 {
        plugin,
        path: dir.join("junk.syx"),
    })
    .unwrap();
    assert_eq!(s.dx7_bank(plugin).unwrap().voices.len(), 32);
    // Only an FM takes them.
    let (mut other, synth) = with(builtin::SYNTH);
    assert!(
        other
            .dispatch(Action::ImportDx7 {
                plugin: synth,
                path: dir.join("bank.syx"),
            })
            .is_err()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_granular_takes_its_sample_and_the_three_play_live() {
    let dir = scratch("granular");
    table_wav(&dir.join("texture.wav"));
    let (mut s, plugin) = with(builtin::GRANULAR);
    s.dispatch(Action::LoadDeviceSamples {
        plugin,
        slot: 0,
        files: vec![dir.join("texture.wav")],
    })
    .unwrap();
    wait(&mut s, plugin);
    assert!(
        s.plugin_tap(plugin)
            .and_then(|t| t.assets::<Contents>())
            .is_some_and(|c| c.set.slot(0).is_some())
    );
    // Each instrument plays a note through the engine, live on a device.
    for id in [builtin::WAVETABLE, builtin::FM, builtin::GRANULAR] {
        let (mut s, _) = with(id);
        let track = s.project().tracks[0].id;
        s.start_audio(
            vec![Box::new(DummyBackend::default())],
            &AudioPreferences::default(),
        )
        .unwrap();
        run(&mut s, Duration::from_millis(80));
        s.dispatch(Action::Audition {
            track,
            key: 60,
            velocity: 110,
            channel: 0,
        })
        .unwrap();
        run(&mut s, Duration::from_millis(300));
        let m = s.meter(track);
        let level = m.left.level_db.max(m.right.level_db);
        assert!(level > -50.0, "{id} sounds: {level}");
        s.dispatch(Action::AuditionOff).unwrap();
        s.stop_audio();
    }
    let _ = std::fs::remove_dir_all(&dir);
}
