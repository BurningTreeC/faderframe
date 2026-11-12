//! Self-freezing in a session: a heavy track becomes cached after its
//! background render; an edit sends it back to its chain in the same
//! sync and undo finds the old cache again; what the cache plays is what
//! the chain plays, from the start and across a switch while playing;
//! touching a track live, keeping it live and the cache folder.
#![allow(clippy::unwrap_used)]

use faderframe_audio::dummy::DummyBackend;
use faderframe_audio_files::GeneratorSpec;
use faderframe_core::{ChannelLayout, ParameterId, PluginInstanceId, TrackId, builtin};
use faderframe_engine::cache::CachedChain;
use faderframe_engine::offline::OfflineRenderer;
use faderframe_engine::{EngineConfig, EngineController, NodeWork, Source};
use faderframe_project::{
    AudioClip, AudioSource, Clip, ClipContent, ClipFades, Command, Impact, PluginRef, PluginSlot,
    Project, SourceSpec, StretchSettings, Track, TrackColor, TrackKind,
};
use faderframe_session::self_freeze::{Live, SelfFreezeSettings, TrackCache};
use faderframe_session::{Action, AudioPreferences, Session, TransportAction};
use faderframe_timeline::MusicalTime;
use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;

const SR: u32 = 48_000;
const BLOCK: usize = 256;
const WAIT: Duration = Duration::from_secs(120);

fn slot(p: &mut Project, id: &str) -> PluginSlot {
    PluginSlot {
        id: p.ids.allocate(),
        plugin: PluginRef::builtin(id, id),
        bypass: false,
        parameters: Vec::new(),
        state: None,
        sidechain: None,
        mono_to_stereo: false,
        mix_origin: None,
    }
}

/// A drum loop through an echo and a reverb, and a plain track.
fn project() -> (Project, TrackId, TrackId) {
    let mut p = Project::new("Self-Freeze", SR);
    let source = p.ids.allocate();
    p.sources.insert(
        source,
        AudioSource {
            id: source,
            name: "Drums".into(),
            spec: SourceSpec::Generated {
                generator: GeneratorSpec::DrumLoop {
                    bpm: 120.0,
                    bars: 2,
                    seed: 7,
                },
            },
        },
    );
    let add = |p: &mut Project, name: &str, devices: &[&str]| -> TrackId {
        let id: TrackId = p.ids.allocate();
        let clip = p.ids.allocate();
        let mut t = Track::new(id, TrackKind::Audio, name, TrackColor::palette(0))
            .with_layout(ChannelLayout::Stereo);
        for d in devices {
            let s = slot(p, d);
            t.inserts.push(s);
        }
        t.clips.push(clip);
        let at = p.tracks.len() - 1;
        p.tracks.insert(at, t);
        p.clips.insert(
            clip,
            Clip {
                id: clip,
                track: id,
                name: name.into(),
                color: None,
                start: MusicalTime::ZERO,
                muted: false,
                content: ClipContent::Audio(AudioClip {
                    source,
                    source_offset: 0,
                    length: 4 * SR as i64,
                    gain_db: 0.0,
                    fades: ClipFades::default(),
                    stretch: StretchSettings::Off,
                    reversed: false,
                    warp: None,
                    pitch: None,
                    effects: None,
                    spectral: None,
                    envelope: None,
                }),
            },
        );
        id
    };
    let heavy = add(&mut p, "Heavy", &[builtin::ECHO, builtin::REVERB]);
    let plain = add(&mut p, "Plain", &[]);
    (p, heavy, plain)
}

/// A saved session (its cache folder beside its file, not in the user's
/// data) with self-freezing on for every chain.
fn session(name: &str) -> (Session, TrackId, TrackId, PathBuf) {
    let dir = std::env::temp_dir().join(format!("ff-self-freeze-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (p, heavy, plain) = project();
    let config = EngineConfig {
        sample_rate: SR,
        ..EngineConfig::default()
    };
    let mut s = Session::new(p, None, config).unwrap();
    s.save_as(&dir.join("song.ffproj")).unwrap();
    s.set_self_freeze(SelfFreezeSettings {
        min_cost_percent: 0.0,
        mode: faderframe_session::self_freeze::SelfFreezeMode::Always,
        ..SelfFreezeSettings::default()
    });
    s.self_freeze_now();
    (s, heavy, plain, dir)
}

fn cached(s: &Session, t: TrackId) -> bool {
    matches!(s.self_freeze_track(t), TrackCache::Cached { .. })
}

fn devices_of(e: &EngineController, t: TrackId) -> usize {
    e.graph_profile()
        .unwrap()
        .owners
        .iter()
        .flatten()
        .filter(|o| o.track == t && o.work == NodeWork::Insert)
        .count()
}

fn first_plugin(s: &Session, t: TrackId) -> PluginInstanceId {
    s.project().track(t).unwrap().inserts[0].id
}

#[test]
fn a_heavy_track_becomes_cached_after_its_background_render() {
    let (mut s, heavy, plain, dir) = session("cached");
    assert!(devices_of(s.engine(), heavy) > 0);
    assert!(s.wait_for_self_freeze(WAIT));
    assert!(cached(&s, heavy), "{:?}", s.self_freeze_track(heavy));
    assert_eq!(
        devices_of(s.engine(), heavy),
        0,
        "its devices no longer run"
    );
    assert_eq!(
        s.self_freeze_track(plain),
        TrackCache::Live(Live::NoDevices)
    );
    let summary = s.self_freeze_summary();
    assert_eq!(summary.cached, vec![heavy]);
    assert_eq!(summary.files, 1);
    assert!(
        summary.bytes > 4 * u64::from(SR) * 2 * 4,
        "the clip and the tail"
    );
    assert!(dir.join("Cache").join("index.json").is_file());
    // The devices are still loaded: their editors open at once.
    assert!(s.engine().plugin_latency(first_plugin(&s, heavy)).is_some());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_edit_sends_a_cached_track_back_at_once_and_undo_finds_its_cache() {
    let (mut s, heavy, _, dir) = session("edit");
    assert!(s.wait_for_self_freeze(WAIT));
    assert!(cached(&s, heavy));
    let plugin = first_plugin(&s, heavy);
    s.dispatch(Action::Edit(Command::SetPluginParameter {
        track: heavy,
        plugin,
        parameter: ParameterId(0),
        value: Some(250.0),
    }))
    .unwrap();
    // In the same sync: the chain is back in the graph.
    assert!(!cached(&s, heavy));
    assert!(devices_of(s.engine(), heavy) > 0);
    // A new cache for the new sound …
    s.self_freeze_now();
    assert!(s.wait_for_self_freeze(WAIT));
    assert!(cached(&s, heavy));
    assert_eq!(s.self_freeze_summary().files, 2);
    // … and the old one again after undo, without rendering.
    s.dispatch(Action::Undo).unwrap();
    assert!(!cached(&s, heavy));
    s.self_freeze_now();
    s.tick(0.01);
    s.tick(0.01);
    assert!(s.self_freeze_summary().rendering.is_none(), "no render");
    assert!(s.wait_for_self_freeze(WAIT));
    assert!(cached(&s, heavy));
    assert_eq!(s.self_freeze_summary().files, 2);
    // Fader, pan and mute stay live: nothing changes.
    for cmd in [
        Command::SetTrackVolume {
            track: heavy,
            db: -6.0,
        },
        Command::SetTrackMute {
            track: heavy,
            on: true,
        },
    ] {
        s.dispatch(Action::Edit(cmd)).unwrap();
        assert!(cached(&s, heavy));
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// The session's cache played by an engine of its own in place of the
/// chain: exactly what the chain makes, cached from the start and after a
/// switch while playing.
#[test]
fn what_the_cache_plays_is_what_the_chain_plays() {
    let (mut s, heavy, _, dir) = session("exact");
    assert!(s.wait_for_self_freeze(WAIT));
    let chain = s.self_freeze_chain(heavy).unwrap();
    let project = s.project().clone();
    let config = EngineConfig {
        sample_rate: SR,
        ..EngineConfig::default()
    };
    let sources = faderframe_engine::render_generated_sources(&project, SR);
    let fresh = || -> CachedChain {
        let Source::Stream(st) = &chain.source else {
            panic!("a cache file")
        };
        CachedChain {
            source: Source::Stream(faderframe_audio_files::StreamSource::open(st.path()).unwrap()),
            ..(*chain).clone()
        }
    };
    let total = BLOCK * 600;
    let mut live = OfflineRenderer::new(&project, &sources, config, BLOCK, 2).unwrap();
    live.play_from(0).unwrap();
    let want = live.render(total);
    assert!(want[0].iter().any(|v| v.abs() > 0.01), "something to hear");
    // Switched while playing.
    let mut r = OfflineRenderer::new(&project, &sources, config, BLOCK, 2).unwrap();
    r.play_from(0).unwrap();
    let mut got = r.render(BLOCK * 200);
    r.controller.set_cached(heavy, Some(fresh()));
    r.controller
        .sync(&project, &sources, Impact::Graph)
        .unwrap();
    let mut settled = false;
    while got[0].len() < total {
        for (g, m) in got.iter_mut().zip(r.render(BLOCK)) {
            g.extend(m);
        }
        if !settled && r.controller.settle_cache() {
            settled = true;
            r.controller
                .sync(&project, &sources, Impact::Graph)
                .unwrap();
        }
    }
    assert!(settled, "the handover ended");
    for c in 0..2 {
        if let Some(i) = got[c].iter().zip(&want[c]).position(|(a, b)| a != b) {
            panic!(
                "channel {c} differs from frame {i}: {} vs {}",
                got[c][i], want[c][i]
            );
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn touching_a_track_live_or_keeping_it_live_plays_its_chain() {
    let (mut s, heavy, _, dir) = session("live");
    assert!(s.wait_for_self_freeze(WAIT));
    assert!(cached(&s, heavy));
    // An editor open: back (gently), then cached again once it closes.
    let plugin = first_plugin(&s, heavy);
    s.set_plugins_being_edited(&HashSet::from([plugin]));
    s.tick(0.01);
    assert_eq!(
        s.self_freeze_track(heavy),
        TrackCache::ToLive,
        "back gently"
    );
    s.tick(0.01);
    assert_eq!(s.self_freeze_track(heavy), TrackCache::Live(Live::Editing));
    assert!(devices_of(s.engine(), heavy) > 0);
    s.set_plugins_being_edited(&HashSet::new());
    s.self_freeze_now();
    assert!(s.wait_for_self_freeze(WAIT));
    assert!(cached(&s, heavy));
    // Armed.
    s.dispatch(Action::Edit(Command::SetTrackRecordArm {
        track: heavy,
        on: true,
    }))
    .unwrap();
    assert!(!cached(&s, heavy));
    s.tick(0.01);
    assert_eq!(s.self_freeze_track(heavy), TrackCache::Live(Live::Armed));
    s.dispatch(Action::Edit(Command::SetTrackRecordArm {
        track: heavy,
        on: false,
    }))
    .unwrap();
    // Kept live by the user.
    s.dispatch(Action::Edit(Command::SetTrackSelfFreeze {
        track: heavy,
        off: true,
    }))
    .unwrap();
    s.self_freeze_now();
    assert!(s.wait_for_self_freeze(WAIT));
    assert_eq!(s.self_freeze_track(heavy), TrackCache::Live(Live::KeptLive));
    assert!(devices_of(s.engine(), heavy) > 0);
    s.dispatch(Action::Undo).unwrap();
    s.self_freeze_now();
    assert!(s.wait_for_self_freeze(WAIT));
    assert!(cached(&s, heavy));
    // Self-freezing off: every track plays its chain.
    s.set_self_freeze(SelfFreezeSettings {
        on: false,
        mode: faderframe_session::self_freeze::SelfFreezeMode::Always,
        ..SelfFreezeSettings::default()
    });
    assert!(s.wait_for_self_freeze(WAIT));
    assert_eq!(s.self_freeze_track(heavy), TrackCache::Live(Live::Off));
    assert!(devices_of(s.engine(), heavy) > 0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn chains_not_worth_it_stay_live_and_the_folder_moves_with_the_project() {
    let (mut s, heavy, _, dir) = session("folder");
    // An impossible minimum: judged cheap, nothing kept.
    s.set_self_freeze(SelfFreezeSettings {
        min_cost_percent: 10_000.0,
        mode: faderframe_session::self_freeze::SelfFreezeMode::Always,
        ..SelfFreezeSettings::default()
    });
    s.self_freeze_now();
    assert!(s.wait_for_self_freeze(WAIT));
    assert_eq!(s.self_freeze_track(heavy), TrackCache::Live(Live::Cheap));
    assert_eq!(s.self_freeze_summary().files, 0);
    // Worth it after all.
    s.set_self_freeze(SelfFreezeSettings {
        min_cost_percent: 0.0,
        mode: faderframe_session::self_freeze::SelfFreezeMode::Always,
        ..SelfFreezeSettings::default()
    });
    s.self_freeze_now();
    assert!(s.wait_for_self_freeze(WAIT));
    assert!(cached(&s, heavy));
    // Saved elsewhere: the folder goes along and keeps playing.
    let other = dir.join("elsewhere");
    std::fs::create_dir_all(&other).unwrap();
    s.save_as(&other.join("song.ffproj")).unwrap();
    assert!(other.join("Cache").join("index.json").is_file());
    assert!(!dir.join("Cache").join("index.json").exists());
    s.tick(0.01);
    assert!(cached(&s, heavy));
    // Opened again: played from the folder without rendering.
    s.open(&other.join("song.ffproj")).unwrap();
    s.set_self_freeze(SelfFreezeSettings {
        min_cost_percent: 0.0,
        mode: faderframe_session::self_freeze::SelfFreezeMode::Always,
        ..SelfFreezeSettings::default()
    });
    s.self_freeze_now();
    s.tick(0.01);
    s.tick(0.01);
    assert!(s.self_freeze_summary().rendering.is_none(), "no render");
    assert!(s.wait_for_self_freeze(WAIT));
    assert!(cached(&s, heavy));
    // A limit of nothing: what plays stays, nothing else.
    s.set_self_freeze(SelfFreezeSettings {
        min_cost_percent: 0.0,
        max_megabytes: 0,
        mode: faderframe_session::self_freeze::SelfFreezeMode::Always,
        ..SelfFreezeSettings::default()
    });
    assert_eq!(s.self_freeze_summary().files, 1);
    let _ = std::fs::remove_dir_all(&dir);
}

/// With a device running: the background render finishes while playing,
/// the track fades to its cache on the audio thread, and back when it is
/// armed.
#[test]
fn a_track_fades_to_its_cache_while_playing() {
    let (mut s, heavy, _, dir) = session("playing");
    s.start_audio(
        vec![Box::new(DummyBackend::default())],
        &AudioPreferences::default(),
    )
    .unwrap();
    s.dispatch(Action::Transport(TransportAction::Play))
        .unwrap();
    let run_until = |s: &mut Session, what: &dyn Fn(&Session) -> bool| {
        let end = std::time::Instant::now() + WAIT;
        while !what(s) {
            assert!(std::time::Instant::now() < end, "timed out");
            std::thread::sleep(Duration::from_millis(5));
            s.tick(0.005);
        }
    };
    run_until(&mut s, &|s| cached(s, heavy));
    assert!(s.transport().playing);
    assert_eq!(devices_of(s.engine(), heavy), 0);
    s.dispatch(Action::Edit(Command::SetTrackRecordArm {
        track: heavy,
        on: true,
    }))
    .unwrap();
    run_until(&mut s, &|s| {
        s.self_freeze_track(heavy) == TrackCache::Live(Live::Armed)
    });
    assert!(devices_of(s.engine(), heavy) > 0);
    s.stop_audio();
    let _ = std::fs::remove_dir_all(&dir);
}

/// An instrument played by a MIDI track: the MIDI track's notes and mute
/// are part of the cache; what the cache plays is what the synth plays.
#[test]
fn an_instrument_played_by_a_midi_track_is_cached_with_its_notes() {
    use faderframe_project::{MidiClip, MidiNote, OutputRouting};
    let dir = std::env::temp_dir().join(format!("ff-self-freeze-{}-midi", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut p = Project::new("MIDI", SR);
    let inst: TrackId = p.ids.allocate();
    let mut t = Track::new(inst, TrackKind::Instrument, "Keys", TrackColor::palette(0))
        .with_layout(ChannelLayout::Stereo);
    let synth = slot(&mut p, builtin::SYNTH);
    t.inserts.push(synth);
    let at = p.tracks.len() - 1;
    p.tracks.insert(at, t);
    let midi: TrackId = p.ids.allocate();
    let clip = p.ids.allocate();
    let mut m = Track::new(midi, TrackKind::Midi, "Notes", TrackColor::palette(1));
    m.output = OutputRouting::Track { track: inst };
    m.clips.push(clip);
    let at = p.tracks.len() - 1;
    p.tracks.insert(at, m);
    let notes = (0..4)
        .map(|i| MidiNote {
            articulation: None,
            id: p.ids.allocate(),
            start: MusicalTime::from_quarters_i(i),
            length: MusicalTime::from_quarters(0.5),
            key: 60 + 4 * i as u8,
            velocity: 100,
            channel: 0,
            muted: false,
            release: None,
            time_locked: false,
            fine: None,
        })
        .collect();
    p.clips.insert(
        clip,
        Clip {
            id: clip,
            track: midi,
            name: "Notes".into(),
            color: None,
            start: MusicalTime::ZERO,
            muted: false,
            content: ClipContent::Midi(MidiClip {
                articulations: Vec::new(),
                length: MusicalTime::from_quarters_i(4),
                notes,
                controllers: Vec::new(),
                expressions: Vec::new(),
                sysex: Vec::new(),
            }),
        },
    );
    let config = EngineConfig {
        sample_rate: SR,
        ..EngineConfig::default()
    };
    let mut s = Session::new(p, None, config).unwrap();
    s.save_as(&dir.join("midi.ffproj")).unwrap();
    // Not live: nothing selected.
    s.dispatch(Action::SelectTracks {
        tracks: Vec::new(),
        mode: faderframe_session::SelectMode::Replace,
    })
    .unwrap();
    s.set_self_freeze(SelfFreezeSettings {
        min_cost_percent: 0.0,
        mode: faderframe_session::self_freeze::SelfFreezeMode::Always,
        ..SelfFreezeSettings::default()
    });
    s.self_freeze_now();
    assert!(s.wait_for_self_freeze(WAIT));
    assert!(cached(&s, inst), "{:?}", s.self_freeze_track(inst));
    // The cache against the synth, played by an engine of its own.
    let chain = s.self_freeze_chain(inst).unwrap();
    let project = s.project().clone();
    let sources = faderframe_engine::render_generated_sources(&project, SR);
    let Source::Stream(st) = &chain.source else {
        panic!("a cache file")
    };
    let own = CachedChain {
        source: Source::Stream(faderframe_audio_files::StreamSource::open(st.path()).unwrap()),
        ..(*chain).clone()
    };
    let total = BLOCK * 500;
    let mut live = OfflineRenderer::new(&project, &sources, config, BLOCK, 2).unwrap();
    live.play_from(0).unwrap();
    let want = live.render(total);
    assert!(want[0].iter().any(|v| v.abs() > 0.01), "the synth plays");
    let mut r = OfflineRenderer::new(&project, &sources, config, BLOCK, 2).unwrap();
    r.controller.set_cached(inst, Some(own));
    r.controller
        .sync(&project, &sources, Impact::Graph)
        .unwrap();
    r.controller.skip_cache_fades();
    if r.controller.settle_cache() {
        r.controller
            .sync(&project, &sources, Impact::Graph)
            .unwrap();
    }
    r.play_from(0).unwrap();
    let got = r.render(total);
    for c in 0..2 {
        if let Some(i) = got[c].iter().zip(&want[c]).position(|(a, b)| a != b) {
            panic!(
                "channel {c} differs from frame {i}: {} vs {}",
                got[c][i], want[c][i]
            );
        }
    }
    // The MIDI track muted: its notes no longer reach the synth.
    s.dispatch(Action::Edit(Command::SetTrackMute {
        track: midi,
        on: true,
    }))
    .unwrap();
    assert!(!cached(&s, inst));
    let _ = std::fs::remove_dir_all(&dir);
}

/// When Needed (the default): a machine that copes hears every chain; a
/// struggling one gets caches, which stay when the load is low again (no
/// flipping back and forth), while nothing new is made then.
#[test]
fn when_needed_caches_only_while_the_machine_struggles() {
    use faderframe_session::self_freeze::SelfFreezeMode;
    let (mut s, heavy, _, dir) = session("when-needed");
    s.set_self_freeze(SelfFreezeSettings {
        min_cost_percent: 0.0,
        ..SelfFreezeSettings::default()
    });
    assert_eq!(s.self_freeze_settings().mode, SelfFreezeMode::WhenNeeded);
    // The helper asked for caches now: a load that copes takes that back.
    s.simulate_self_freeze_load(0.2);
    assert!(s.wait_for_self_freeze(WAIT));
    assert!(!s.self_freeze_needed());
    assert_eq!(
        s.self_freeze_track(heavy),
        TrackCache::Live(Live::NotNeeded)
    );
    assert!(!s.self_freeze_summary().needed);
    assert!(s.self_freeze_summary().cached.is_empty());
    // The machine struggles: the heavy track is cached.
    s.simulate_self_freeze_load(0.9);
    assert!(s.self_freeze_needed());
    assert!(s.wait_for_self_freeze(WAIT));
    assert!(cached(&s, heavy));
    assert!(s.self_freeze_summary().needed);
    // The load is low again: the cache stays.
    s.simulate_self_freeze_load(0.2);
    assert!(s.wait_for_self_freeze(WAIT));
    assert!(cached(&s, heavy));
    // An edit sends it back to its chain; with the machine coping it is
    // not cached again.
    let echo = s
        .project()
        .track(heavy)
        .unwrap()
        .inserts
        .first()
        .unwrap()
        .id;
    s.dispatch(Action::Edit(Command::SetPluginBypass {
        track: heavy,
        plugin: echo,
        bypass: true,
    }))
    .unwrap();
    assert!(s.wait_for_self_freeze(WAIT));
    assert_eq!(
        s.self_freeze_track(heavy),
        TrackCache::Live(Live::NotNeeded)
    );
    // Always: cached whatever the load.
    s.set_self_freeze(SelfFreezeSettings {
        min_cost_percent: 0.0,
        mode: SelfFreezeMode::Always,
        ..SelfFreezeSettings::default()
    });
    s.self_freeze_now();
    assert!(s.wait_for_self_freeze(WAIT));
    assert!(cached(&s, heavy));
    let _ = std::fs::remove_dir_all(&dir);
}
