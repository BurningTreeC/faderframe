#![allow(clippy::unwrap_used)]
//! DAWproject files: FaderFrame → DAWproject → FaderFrame keeps the
//! tracks, mix, devices, clips, automation, markers and tempo; the file
//! keeps to the schema; another program's file (Bitwig's shape: clips in
//! clips, warps, a CLAP state) reads.

use faderframe_audio_files::{WavFormat, write_wav};
use faderframe_automation::{
    AutomationCurve, AutomationLane, AutomationMode, AutomationPoint, AutomationTarget, CurveShape,
};
use faderframe_core::{ParameterId, PluginInstanceId};
use faderframe_engine::EngineConfig;
use faderframe_interchange::dawproject as dp;
use faderframe_interchange::presets;
use faderframe_interchange::zip::{ZipReader, ZipWriter};
use faderframe_project::{
    AudioClip, AudioSource, AuxSend, Clip, ClipContent, ClipFades, ControllerLane, ControllerPoint,
    ExpressionKind, ExpressionPoint, MidiClip, MidiController, MidiNote, NoteExpression,
    PluginFormat, PluginRef, PluginSlot, Project, SavedParameter, SendTap, SourceSpec, Track,
    TrackColor, TrackKind, Warp, WarpMarker,
};
use faderframe_session::{Action, Session};
use faderframe_timeline::{MusicalTime, TempoCurve, TempoPoint};
use std::path::{Path, PathBuf};

/// A device as compared: its plug-in, bypass and kept values.
type Device = (String, bool, Vec<(u32, i64)>);

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ff-dawproject-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn empty() -> Session {
    Session::new(Project::new("Empty", 48_000), None, EngineConfig::default()).unwrap()
}

fn export(s: &mut Session, path: &Path) {
    s.dispatch(Action::ExportDawProject(path.to_path_buf()))
        .unwrap();
    s.wait_for_dawproject();
    assert!(path.is_file(), "{} written", path.display());
}

fn import(path: &Path) -> Session {
    let mut s = empty();
    s.dispatch(Action::ImportDawProject(path.to_path_buf()))
        .unwrap();
    s.wait_for_dawproject();
    s
}

/// The tracks by name in editor order, with their kinds.
fn shape(p: &Project) -> Vec<(String, TrackKind, Option<String>)> {
    p.folder_order()
        .iter()
        .map(|t| {
            (
                t.name.clone(),
                t.kind,
                t.folder.and_then(|f| p.track(f)).map(|f| f.name.clone()),
            )
        })
        .collect()
}

fn by_name<'a>(p: &'a Project, name: &str) -> &'a Track {
    p.tracks.iter().find(|t| t.name == name).unwrap()
}

/// `project.xml` of a written file, checked against the schema when
/// xmllint is there.
fn check_schema(path: &Path) -> String {
    let mut z = ZipReader::open(path).unwrap();
    let xml = String::from_utf8(z.read("project.xml").unwrap()).unwrap();
    let meta = String::from_utf8(z.read("metadata.xml").unwrap()).unwrap();
    let schemas =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../faderframe-interchange/data/dawproject");
    let Ok(v) = std::process::Command::new("xmllint")
        .arg("--version")
        .output()
    else {
        eprintln!("xmllint not installed: schema check skipped");
        return xml;
    };
    if !v.status.success() {
        return xml;
    }
    let dir = path.parent().unwrap();
    for (name, text, schema) in [
        ("project.xml", &xml, "Project.xsd"),
        ("metadata.xml", &meta, "MetaData.xsd"),
    ] {
        let f = dir.join(format!("check-{name}"));
        std::fs::write(&f, text).unwrap();
        let out = std::process::Command::new("xmllint")
            .arg("--noout")
            .arg("--schema")
            .arg(schemas.join(schema))
            .arg(&f)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    xml
}

#[test]
fn the_demo_goes_out_and_comes_back() {
    let dir = tmp("demo");
    let path = dir.join("Demo.dawproject");
    let mut demo = Session::demo(EngineConfig::default()).unwrap();
    export(&mut demo, &path);
    let report = demo.interchange_report().unwrap();
    assert!(report.title.starts_with("Exported"));
    let left = report.notes.join("\n");
    assert!(left.contains("chords of the chord track"), "{left}");
    let xml = check_schema(&path);
    assert!(xml.contains("<Application name=\"FaderFrame\""));

    let back = import(&path);
    let (a, b) = (demo.project(), back.project());
    assert!(
        back.interchange_report()
            .unwrap()
            .title
            .starts_with("Imported")
    );
    // The same tracks in the same places, folders holding the same.
    assert_eq!(shape(a), shape(b));
    assert_eq!(b.tracks.last().unwrap().kind, TrackKind::Master);
    for t in &a.tracks {
        let u = by_name(b, &t.name);
        if t.kind.has_audio() && t.kind != TrackKind::Midi {
            assert!((t.volume_db - u.volume_db).abs() < 0.01, "{}", t.name);
            assert!((t.pan - u.pan).abs() < 1e-4, "{}", t.name);
        }
        assert_eq!(
            (t.mute, t.solo, t.color),
            (u.mute, u.solo, u.color),
            "{}",
            t.name
        );
        assert_eq!(t.sends.len(), u.sends.len(), "{}", t.name);
        for (x, y) in t.sends.iter().zip(&u.sends) {
            assert_eq!(
                a.track(x.target).unwrap().name,
                b.track(y.target).unwrap().name
            );
            assert!((x.level_db - y.level_db).abs() < 0.01);
        }
        // Devices: built-ins with their parameters and bypass.
        let devices = |t: &Track| -> Vec<Device> {
            t.preamp
                .iter()
                .chain(t.instrument.iter())
                .chain(t.inserts.iter())
                .filter(|s| !s.plugin.is_container())
                .map(|s| {
                    let mut ps: Vec<(u32, i64)> = s
                        .parameters
                        .iter()
                        .map(|p| (p.id.0, (p.value * 1e6).round() as i64))
                        .collect();
                    ps.sort_unstable();
                    (s.plugin.id.clone(), s.bypass, ps)
                })
                .collect()
        };
        assert_eq!(devices(t), devices(u), "{}", t.name);
        // Clips at their places, of their lengths.
        let ca = a.clips_of(t.id);
        let cb = b.clips_of(u.id);
        assert_eq!(ca.len(), cb.len(), "{}", t.name);
        for (x, y) in ca.iter().zip(&cb) {
            assert_eq!(x.name, y.name);
            assert_eq!(x.muted, y.muted);
            match (&x.content, &y.content) {
                (ClipContent::Midi(m), ClipContent::Midi(n)) => {
                    assert_eq!(x.start, y.start, "{}", x.name);
                    assert_eq!(m.length, n.length);
                    let notes = |c: &MidiClip| -> Vec<(i64, i64, u8, u8, u8)> {
                        c.notes
                            .iter()
                            .map(|n| {
                                (
                                    n.start.ticks(),
                                    n.length.ticks(),
                                    n.key,
                                    n.velocity,
                                    n.channel,
                                )
                            })
                            .collect()
                    };
                    assert_eq!(notes(m), notes(n), "{}", x.name);
                }
                (ClipContent::Audio(m), ClipContent::Audio(n)) => {
                    let rate = f64::from(a.sample_rate);
                    assert_eq!(
                        a.timeline.to_samples(x.start, rate),
                        b.timeline.to_samples(y.start, rate),
                        "{}",
                        x.name
                    );
                    assert_eq!(
                        (m.source_offset, m.length),
                        (n.source_offset, n.length),
                        "{}",
                        x.name
                    );
                    assert!((m.gain_db - n.gain_db).abs() < 0.01);
                    assert_eq!(
                        (m.fades.fade_in, m.fades.fade_out),
                        (n.fades.fade_in, n.fades.fade_out)
                    );
                    assert_eq!(m.warp.is_some(), n.warp.is_some());
                }
                (ClipContent::Takes(_), ClipContent::Audio(_)) => {}
                other => panic!("{} came back as another kind: {other:?}", x.name),
            }
        }
        // Automation follows the same lines.
        for l in &t.automation.lanes {
            let target = match l.target {
                AutomationTarget::PluginParameter { plugin, parameter } => {
                    let i = t.inserts.iter().position(|s| s.id == plugin).unwrap();
                    AutomationTarget::PluginParameter {
                        plugin: u.inserts[i].id,
                        parameter,
                    }
                }
                other => other,
            };
            let m = u
                .automation
                .lanes
                .iter()
                .find(|x| x.target == target)
                .unwrap_or_else(|| panic!("{}: {:?} came back", t.name, l.target));
            for q in 0..64 {
                let at = MusicalTime::from_quarters(f64::from(q) * 0.5);
                let (v, w) = (l.curve.value_at(at).unwrap(), m.curve.value_at(at).unwrap());
                assert!(
                    (v - w).abs() < 0.06,
                    "{} {:?} at {q}: {v} vs {w}",
                    t.name,
                    l.target
                );
            }
        }
    }
    assert_eq!(a.timeline.tempo.points(), b.timeline.tempo.points());
    assert_eq!(a.timeline.meter.changes(), b.timeline.meter.changes());
    let names = |p: &Project| -> Vec<(String, i64)> {
        p.markers
            .iter()
            .map(|m| (m.name.clone(), m.position.ticks()))
            .collect()
    };
    assert_eq!(names(a), names(b));
    let sections = |p: &Project| -> Vec<(String, i64, i64)> {
        p.sections
            .iter()
            .map(|s| (s.name.clone(), s.start.ticks(), s.end.ticks()))
            .collect()
    };
    assert_eq!(sections(a), sections(b));
    // A new project: unsaved, every source a file in its media folder.
    assert!(back.path().is_none());
    assert!(b.sources.values().all(|s| match &s.spec {
        SourceSpec::File { path, .. } => path.is_file(),
        SourceSpec::Generated { .. } => false,
    }));
}

fn tone(path: &Path, rate: u32, seconds: f64, channels: usize) {
    let n = (f64::from(rate) * seconds) as usize;
    let ch: Vec<f32> = (0..n).map(|i| ((i as f32) * 0.031).sin() * 0.4).collect();
    write_wav(path, &vec![ch; channels], rate, WavFormat::Float32, false).unwrap();
}

/// A project with what the demo has not: a warped clip, a CLAP and a
/// VST3 state, plug-in and fader automation, MIDI 2.0 values, note
/// expression, a tempo ramp.
fn rich(dir: &Path) -> Project {
    let mut p = Project::new("Rich", 48_000);
    p.timeline.tempo.set_initial_bpm(100.0);
    p.timeline.tempo.set_point(TempoPoint {
        position: MusicalTime::from_quarters(16.0),
        bpm: 100.0,
        curve: TempoCurve::Linear,
    });
    p.timeline.tempo.set_point(TempoPoint {
        position: MusicalTime::from_quarters(32.0),
        bpm: 140.0,
        curve: TempoCurve::Constant,
    });
    let wav = dir.join("loop.wav");
    tone(&wav, 48_000, 3.0, 2);
    let source = AudioSource {
        id: p.ids.allocate(),
        name: "loop".into(),
        spec: SourceSpec::File {
            path: wav,
            channels: 2,
            frames: 144_000,
            sample_rate: 48_000,
        },
    };
    let sid = source.id;
    p.sources.insert(sid, source);
    let master = p.master_id().unwrap();
    // An audio track: a warped clip with gain and fades, a send.
    let audio_id = p.ids.allocate();
    let aux_id = p.ids.allocate();
    let mut audio = Track::new(audio_id, TrackKind::Audio, "Loop", TrackColor::palette(1))
        .with_layout(faderframe_core::ChannelLayout::Stereo);
    audio.volume_db = -3.5;
    audio.pan = -0.4;
    audio.sends.push(AuxSend {
        id: p.ids.allocate(),
        target: aux_id,
        level_db: -12.0,
        tap: SendTap::PreFader,
        enabled: true,
        pan: Some(0.25),
    });
    let clap: PluginInstanceId = p.ids.allocate();
    audio.inserts.push(PluginSlot {
        id: clap,
        plugin: PluginRef {
            format: PluginFormat::Clap,
            id: "com.example.delay".into(),
            name: "Example Delay".into(),
        },
        bypass: true,
        parameters: vec![SavedParameter {
            id: ParameterId(7),
            value: 0.25,
        }],
        state: Some(faderframe_engine::encode_state(b"clap state \x00\xff")),
        sidechain: None,
        mono_to_stereo: false,
        mix_origin: None,
    });
    let vst3: PluginInstanceId = p.ids.allocate();
    let mut ffv3 = b"FFV3".to_vec();
    ffv3.extend_from_slice(&4u32.to_le_bytes());
    ffv3.extend_from_slice(b"comp");
    ffv3.extend_from_slice(&3u32.to_le_bytes());
    ffv3.extend_from_slice(b"ctl");
    audio.inserts.push(PluginSlot {
        id: vst3,
        plugin: PluginRef {
            format: PluginFormat::Vst3,
            id: "D39D5B69D6AF42FA1234567844695661".into(),
            name: "Example EQ".into(),
        },
        bypass: false,
        parameters: Vec::new(),
        state: Some(faderframe_engine::encode_state(&ffv3)),
        sidechain: None,
        mono_to_stereo: false,
        mix_origin: None,
    });
    let lane = |p: &mut Project, target, pts: &[(f64, f64, CurveShape)]| AutomationLane {
        id: p.ids.allocate(),
        target,
        curve: AutomationCurve::from_points(
            pts.iter()
                .map(|&(q, value, shape)| AutomationPoint {
                    time: MusicalTime::from_quarters(q),
                    value,
                    shape,
                })
                .collect(),
        ),
        mode: AutomationMode::Read,
        visible: false,
    };
    let vol = lane(
        &mut p,
        AutomationTarget::TrackVolume,
        &[
            (0.0, -20.0, CurveShape::Linear),
            (8.0, 0.0, CurveShape::Step),
            (12.0, -6.0, CurveShape::Linear),
        ],
    );
    let param = lane(
        &mut p,
        AutomationTarget::PluginParameter {
            plugin: clap,
            parameter: ParameterId(7),
        },
        &[
            (0.0, 0.1, CurveShape::Linear),
            (4.0, 0.9, CurveShape::Linear),
        ],
    );
    let bypass = lane(
        &mut p,
        AutomationTarget::PluginBypass(vst3),
        &[(0.0, 0.0, CurveShape::Step), (6.0, 1.0, CurveShape::Step)],
    );
    audio.automation.lanes = vec![vol, param, bypass];
    let clip = Clip {
        id: p.ids.allocate(),
        track: audio_id,
        name: "Loop".into(),
        color: Some(TrackColor::rgb(1, 2, 3)),
        start: MusicalTime::from_quarters(4.0),
        muted: false,
        content: ClipContent::Audio(AudioClip {
            envelope: None,
            source: sid,
            source_offset: 4_800,
            length: 96_000,
            gain_db: -4.0,
            fades: ClipFades {
                fade_in: 480,
                fade_out: 960,
                ..ClipFades::default()
            },
            stretch: Default::default(),
            reversed: false,
            warp: Some(Warp {
                source_length: 120_000,
                markers: vec![WarpMarker {
                    at: 48_000,
                    source: 70_000,
                }],
                algorithm: Default::default(),
            }),
            pitch: None,
            effects: None,
            spectral: None,
        }),
    };
    audio.clips.push(clip.id);
    p.clips.insert(clip.id, clip);
    // An instrument track: notes at MIDI 2.0's resolution, a controller
    // lane at 32 bits, a note's pitch curve.
    let keys_id = p.ids.allocate();
    let mut keys = Track::new(
        keys_id,
        TrackKind::Instrument,
        "Keys",
        TrackColor::palette(2),
    );
    keys.inserts.push(PluginSlot {
        id: p.ids.allocate(),
        plugin: PluginRef::builtin(faderframe_core::builtin::SYNTH, "FaderFrame Synth"),
        bypass: false,
        parameters: vec![SavedParameter {
            id: ParameterId(3),
            value: 0.5,
        }],
        state: None,
        sidechain: None,
        mono_to_stereo: false,
        mix_origin: None,
    });
    let mut n1 = MidiNote {
        id: p.ids.allocate(),
        start: MusicalTime::ZERO,
        length: MusicalTime::QUARTER,
        key: 60,
        velocity: 100,
        channel: 0,
        muted: false,
        release: Some(40),
        time_locked: false,
        articulation: None,
        fine: None,
    };
    let mut n2 = n1;
    n2.id = p.ids.allocate();
    n2.start = MusicalTime::from_quarters(1.5);
    n2.key = 64;
    n2.release = None;
    n2.set_velocity16(0x1234);
    n1.channel = 2;
    let mut expr = NoteExpression::new(n1.id);
    expr.curve_mut(ExpressionKind::Pitch).extend([
        ExpressionPoint {
            time: MusicalTime::ZERO,
            value: 0.0,
        },
        ExpressionPoint {
            time: MusicalTime::from_quarters(0.5),
            value: 2.0,
        },
    ]);
    let mut cc = ControllerLane::new(MidiController::MOD_WHEEL, 0);
    cc.points = vec![
        ControllerPoint::new(MusicalTime::ZERO, 10),
        ControllerPoint::of_steps(MusicalTime::QUARTER, MidiController::MOD_WHEEL, 64.5),
    ];
    let mut bend = ControllerLane::new(MidiController::PitchBend, 0);
    bend.points = vec![ControllerPoint::new(
        MusicalTime::from_quarters(2.0),
        12_000,
    )];
    let midi = Clip {
        id: p.ids.allocate(),
        track: keys_id,
        name: "Riff".into(),
        color: None,
        start: MusicalTime::from_quarters(8.0),
        muted: true,
        content: ClipContent::Midi(MidiClip {
            length: MusicalTime::from_quarters(4.0),
            notes: vec![n1, n2],
            controllers: vec![cc, bend],
            expressions: vec![expr],
            ..MidiClip::default()
        }),
    };
    keys.clips.push(midi.id);
    p.clips.insert(midi.id, midi);
    let aux = Track::new(aux_id, TrackKind::Aux, "Reverb", TrackColor::palette(3));
    let at = p.tracks.iter().position(|t| t.id == master).unwrap();
    p.tracks.insert(at, aux);
    p.tracks.insert(at, keys);
    p.tracks.insert(at, audio);
    p
}

#[test]
fn what_fader_frame_holds_comes_back_through_the_file() {
    let dir = tmp("rich");
    let mut s = Session::new(rich(&dir), None, EngineConfig::default()).unwrap();
    let path = dir.join("Rich.dawproject");
    export(&mut s, &path);
    let xml = check_schema(&path);
    // Plug-in states in their own formats.
    let mut z = ZipReader::open(&path).unwrap();
    let names: Vec<String> = z.entries().iter().map(|e| e.name.clone()).collect();
    let clap = names.iter().find(|n| n.ends_with(".clap-preset")).unwrap();
    let (id, state) = {
        let b = z.read(clap).unwrap();
        let (id, s) = presets::read_clap_preset(&b).unwrap();
        (id, s.to_vec())
    };
    assert_eq!(
        (id.as_str(), state.as_slice()),
        ("com.example.delay", &b"clap state \x00\xff"[..])
    );
    let vst = names.iter().find(|n| n.ends_with(".vstpreset")).unwrap();
    let preset = presets::read_vst_preset(&z.read(vst).unwrap()).unwrap();
    assert_eq!(
        (preset.component.as_slice(), preset.controller.as_slice()),
        (&b"comp"[..], &b"ctl"[..])
    );
    assert!(xml.contains("deviceID=\"D39D5B69-D6AF-42FA-1234-567844695661\""));
    assert!(names.iter().any(|n| n == "audio/loop.wav"));

    let back = import(&path);
    let (a, b) = (s.project(), back.project());
    let loop_a = by_name(a, "Loop");
    let loop_b = by_name(b, "Loop");
    assert!((loop_b.volume_db + 3.5).abs() < 1e-3 && (loop_b.pan + 0.4).abs() < 1e-5);
    assert_eq!(loop_b.layout, faderframe_core::ChannelLayout::Stereo);
    let send = &loop_b.sends[0];
    assert_eq!(b.track(send.target).unwrap().name, "Reverb");
    assert_eq!(send.tap, SendTap::PreFader);
    assert!((send.level_db + 12.0).abs() < 1e-3);
    assert!((send.pan.unwrap() - 0.25).abs() < 1e-5);
    // The plug-ins with their states and values.
    let c = &loop_b.inserts[0];
    assert_eq!(
        (c.plugin.format, c.plugin.id.as_str(), c.bypass),
        (PluginFormat::Clap, "com.example.delay", true)
    );
    assert_eq!(c.state, loop_a.inserts[0].state);
    assert_eq!(c.saved(ParameterId(7)), Some(0.25));
    let v = &loop_b.inserts[1];
    assert_eq!(
        (v.plugin.format, v.plugin.id.as_str()),
        (PluginFormat::Vst3, "D39D5B69D6AF42FA1234567844695661")
    );
    assert_eq!(v.state, loop_a.inserts[1].state);
    // The warped clip, sample for sample.
    let ca = a.clips_of(loop_a.id)[0];
    let cb = b.clips_of(loop_b.id)[0];
    let (ClipContent::Audio(x), ClipContent::Audio(y)) = (&ca.content, &cb.content) else {
        panic!()
    };
    let rate = 48_000.0;
    assert_eq!(
        a.timeline.to_samples(ca.start, rate),
        b.timeline.to_samples(cb.start, rate)
    );
    assert_eq!((x.source_offset, x.length), (y.source_offset, y.length));
    assert_eq!(x.warp, y.warp);
    assert!((x.gain_db - y.gain_db).abs() < 1e-4);
    assert_eq!((y.fades.fade_in, y.fades.fade_out), (480, 960));
    assert_eq!(cb.color, Some(TrackColor::rgb(1, 2, 3)));
    // Automation: the fader's line in dB, the plug-in's, the bypass.
    let lane = |t: &Track, target: AutomationTarget| {
        t.automation
            .lanes
            .iter()
            .find(|l| l.target == target)
            .cloned()
            .unwrap()
    };
    let va = lane(loop_a, AutomationTarget::TrackVolume);
    let vb = lane(loop_b, AutomationTarget::TrackVolume);
    for k in 0..=56 {
        let at = MusicalTime::from_quarters(f64::from(k) * 0.25);
        let (p, q) = (
            va.curve.value_at(at).unwrap(),
            vb.curve.value_at(at).unwrap(),
        );
        assert!(
            (p - q).abs() < 0.06,
            "volume at {}: {p} vs {q}",
            k as f64 * 0.25
        );
    }
    assert!(
        vb.curve.points().len() <= 6,
        "the fader's line thinned back: {:?}",
        vb.curve.points()
    );
    let pb = lane(
        loop_b,
        AutomationTarget::PluginParameter {
            plugin: c.id,
            parameter: ParameterId(7),
        },
    );
    assert_eq!(pb.curve.points().len(), 2);
    assert!((pb.curve.value_at(MusicalTime::from_quarters(2.0)).unwrap() - 0.5).abs() < 1e-9);
    let bb = lane(loop_b, AutomationTarget::PluginBypass(v.id));
    assert_eq!(
        bb.curve.value_at(MusicalTime::from_quarters(7.0)),
        Some(1.0)
    );
    assert_eq!(
        bb.curve.value_at(MusicalTime::from_quarters(1.0)),
        Some(0.0)
    );
    // The notes at full resolution, the controllers, the expression.
    let kb = by_name(b, "Keys");
    assert_eq!(kb.kind, TrackKind::Instrument);
    assert_eq!(kb.inserts[0].plugin.id, faderframe_core::builtin::SYNTH);
    assert_eq!(kb.inserts[0].saved(ParameterId(3)), Some(0.5));
    let mb = b.clips_of(kb.id)[0];
    assert!(mb.muted);
    let ma = a.clips_of(by_name(a, "Keys").id)[0];
    let (ClipContent::Midi(x), ClipContent::Midi(y)) = (&ma.content, &mb.content) else {
        panic!()
    };
    assert_eq!(ma.start, mb.start);
    assert_eq!(x.length, y.length);
    for (m, n) in x.notes.iter().zip(&y.notes) {
        assert_eq!(
            (m.start, m.length, m.key, m.channel),
            (n.start, n.length, n.key, n.channel)
        );
        assert_eq!(
            (m.velocity16(), m.release16()),
            (n.velocity16(), n.release16())
        );
    }
    assert!(y.notes[1].has_fine_velocity());
    let lane_b = y.lane(MidiController::MOD_WHEEL, 0).unwrap();
    assert_eq!(lane_b.points[0].fine, None);
    assert_eq!(
        lane_b.points[1].steps_text(MidiController::MOD_WHEEL),
        "64.50"
    );
    assert_eq!(
        y.lane(MidiController::PitchBend, 0).unwrap().points[0].value,
        12_000
    );
    let e = y.expression(y.notes[0].id).unwrap();
    assert_eq!(e.pitch.len(), 2);
    assert!(
        (e.value_at(ExpressionKind::Pitch, MusicalTime::from_quarters(0.25)) - 1.0).abs() < 1e-4
    );
    // Tempo, ramps and all.
    assert_eq!(a.timeline.tempo.points(), b.timeline.tempo.points());
}

/// A file as another program writes one (Bitwig's shape, from the
/// format's README): an instrument with a CLAP state, an audio clip in a
/// clip warped onto 8 beats, notes straight in a clip.
#[test]
fn another_programs_file_reads() {
    let dir = tmp("bitwig");
    let wav = dir.join("drums.wav");
    tone(&wav, 48_000, 2.823_541_666_666_667, 2);
    let xml = r##"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Project version="1.0">
  <Application name="Bitwig Studio" version="5.0"/>
  <Transport>
    <Tempo max="666.000000" min="20.000000" unit="bpm" value="149.000000" id="id0" name="Tempo"/>
    <TimeSignature denominator="4" numerator="4" id="id1"/>
  </Transport>
  <Structure>
    <Track contentType="notes" loaded="true" id="id2" name="Bass" color="#a2eabf">
      <Channel audioChannels="2" destination="id15" role="regular" solo="false" id="id3">
        <Devices>
          <ClapPlugin deviceID="org.surge-synth-team.surge-xt" deviceName="Surge XT" deviceRole="instrument" loaded="true" id="id7" name="Surge XT">
            <Parameters/>
            <Enabled value="true" id="id8" name="On/Off"/>
            <State path="plugins/surge.clap-preset"/>
          </ClapPlugin>
        </Devices>
        <Mute value="false" id="id6" name="Mute"/>
        <Pan max="1.000000" min="0.000000" unit="normalized" value="0.500000" id="id5" name="Pan"/>
        <Volume max="2.000000" min="0.000000" unit="linear" value="0.659140" id="id4" name="Volume"/>
      </Channel>
    </Track>
    <Track contentType="audio" loaded="true" id="id9" name="Drumloop" color="#b53bba">
      <Channel audioChannels="2" destination="id15" role="regular" solo="false" id="id10">
        <Mute value="true" id="id13" name="Mute"/>
        <Pan max="1.000000" min="0.000000" unit="normalized" value="0.250000" id="id12" name="Pan"/>
        <Volume max="2.000000" min="0.000000" unit="linear" value="0.177125" id="id11" name="Volume"/>
      </Channel>
    </Track>
    <Track contentType="audio notes" loaded="true" id="id14" name="Master">
      <Channel audioChannels="2" role="master" solo="false" id="id15">
        <Volume max="2.000000" min="0.000000" unit="linear" value="1.000000" id="id16" name="Volume"/>
      </Channel>
    </Track>
  </Structure>
  <Arrangement id="id19">
    <Lanes timeUnit="beats" id="id20">
      <Lanes track="id2" id="id21">
        <Clips id="id22">
          <Clip time="0.0" duration="8.0" playStart="0.0" loopStart="0.0" loopEnd="4.0">
            <Notes id="id23">
              <Note time="0.000000" duration="0.250000" channel="0" key="65" vel="0.787402" rel="0.787402"/>
              <Note time="1.500000" duration="2.500000" channel="0" key="53" vel="0.787402" rel="0.787402"/>
            </Notes>
          </Clip>
        </Clips>
        <Points id="id40" unit="linear">
          <Target parameter="id4"/>
          <RealPoint time="0" value="0.659140" interpolation="linear"/>
          <RealPoint time="8" value="1.0" interpolation="hold"/>
        </Points>
      </Lanes>
      <Lanes track="id9" id="id24">
        <Clips id="id25">
          <Clip time="4.0" duration="8.00003433227539" playStart="0.0" fadeTimeUnit="beats" fadeInTime="0.5" fadeOutTime="0.0" name="Drumfunk3 170bpm">
            <Clips id="id26">
              <Clip time="0.0" duration="8.00003433227539" contentTimeUnit="beats" playStart="0.0">
                <Warps contentTimeUnit="seconds" timeUnit="beats" id="id28">
                  <Audio algorithm="stretch" channels="2" duration="2.823541666666667" sampleRate="48000" id="id27">
                    <File path="audio/Drumfunk3 170bpm.wav"/>
                  </Audio>
                  <Warp time="0.0" contentTime="0.0"/>
                  <Warp time="8.00003433227539" contentTime="2.823541666666667"/>
                </Warps>
              </Clip>
            </Clips>
          </Clip>
        </Clips>
      </Lanes>
    </Lanes>
    <Markers><Marker time="8" name="Drop" color="#ff0000"/></Markers>
  </Arrangement>
  <Scenes/>
</Project>
"##;
    let path = dir.join("Bitwig.dawproject");
    let mut w = ZipWriter::create(&path).unwrap();
    w.add("project.xml", xml.as_bytes(), true).unwrap();
    w.add(
        "metadata.xml",
        b"<MetaData><Title>Funk</Title><Artist>Someone</Artist></MetaData>",
        true,
    )
    .unwrap();
    w.add(
        "plugins/surge.clap-preset",
        &presets::clap_preset("org.surge-synth-team.surge-xt", b"SURGE"),
        false,
    )
    .unwrap();
    w.add_file("audio/Drumfunk3 170bpm.wav", &wav).unwrap();
    w.finish().unwrap();
    // The vendored schema takes it.
    let (doc, notes) = dp::read_project(xml).unwrap();
    assert!(notes.is_empty(), "{notes:?}");
    assert_eq!(doc.tracks().len(), 3);

    let s = import(&path);
    let p = s.project();
    assert_eq!(p.name, "Funk");
    assert!((p.timeline.tempo.points()[0].bpm - 149.0).abs() < 1e-9);
    let bass = by_name(p, "Bass");
    assert_eq!(bass.kind, TrackKind::Instrument);
    assert_eq!(bass.color, TrackColor::from_hex("#a2eabf").unwrap());
    assert!((bass.volume_db - 20.0 * 0.659_14f32.log10()).abs() < 1e-3);
    let surge = &bass.inserts[0];
    assert_eq!(surge.plugin.format, PluginFormat::Clap);
    assert_eq!(
        surge
            .state
            .as_deref()
            .and_then(faderframe_engine::decode_state),
        Some(b"SURGE".to_vec())
    );
    // The looped clip plays its 4 beats twice.
    let c = p.clips_of(bass.id)[0];
    let m = c.as_midi().unwrap();
    assert_eq!(m.length, MusicalTime::from_quarters(8.0));
    let starts: Vec<f64> = m.notes.iter().map(|n| n.start.quarters()).collect();
    assert_eq!(starts, [0.0, 1.5, 4.0, 5.5]);
    assert_eq!(m.notes[0].velocity, 100);
    // The second loop's long note stops where the clip does.
    assert_eq!(m.notes[3].end(), MusicalTime::from_quarters(8.0));
    // The fader's automation (gain → dB).
    let lane = &bass.automation.lanes[0];
    assert_eq!(lane.target, AutomationTarget::TrackVolume);
    assert!(
        (lane
            .curve
            .value_at(MusicalTime::from_quarters(8.0))
            .unwrap())
        .abs()
            < 1e-3
    );
    // The audio clip warped onto 8 beats at 149 BPM.
    let drums = by_name(p, "Drumloop");
    assert_eq!(drums.kind, TrackKind::Audio);
    assert!(drums.mute);
    assert!((drums.pan + 0.5).abs() < 1e-5);
    let d = p.clips_of(drums.id)[0];
    assert_eq!(d.name, "Drumfunk3 170bpm");
    assert_eq!(d.start, MusicalTime::from_quarters(4.0), "on its beat");
    let a = d.as_audio().unwrap();
    let eight_beats = (8.000_034_332_275_39 * 60.0 / 149.0 * 48_000.0f64).round() as i64;
    assert!(
        (a.length - eight_beats).abs() <= 1,
        "{} vs {eight_beats}",
        a.length
    );
    let w = a.warp.as_ref().unwrap();
    assert!(
        (w.source_length - 135_530).abs() <= 1,
        "{}",
        w.source_length
    );
    assert!(w.markers.is_empty());
    assert_eq!(
        a.fades.fade_in,
        (0.5 * 60.0 / 149.0 * 48_000.0f64).round() as i64
    );
    assert_eq!(p.markers[0].name, "Drop");
    let report = s.interchange_report().unwrap();
    assert!(report.notes.is_empty(), "{:?}", report.notes);
    // One more look: a missing file is reported, not fatal.
    let mut broken = empty();
    broken
        .dispatch(Action::ImportDawProject(dir.join("missing.dawproject")))
        .unwrap_err();
}

#[test]
fn an_import_cancelled_leaves_the_project() {
    let dir = tmp("cancel");
    let path = dir.join("Demo.dawproject");
    let mut demo = Session::demo(EngineConfig::default()).unwrap();
    export(&mut demo, &path);
    let mut s = empty();
    let before = s.project().tracks.len();
    s.dispatch(Action::ImportDawProject(path)).unwrap();
    s.dispatch(Action::CancelDawProject).unwrap();
    s.wait_for_dawproject();
    // Cancelled before the media were read, or read whole: never half.
    let after = s.project().tracks.len();
    assert!(after == before || after > 5, "{before} → {after}");
}
