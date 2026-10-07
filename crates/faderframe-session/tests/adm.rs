//! An object-based master: a 7.1.4 mix delivered as an ADM BWF file (bed,
//! the 7.1.4's tops as fixed objects, object tracks with their movement).
#![allow(clippy::unwrap_used)]

use faderframe_audio_files::{WavFormat, write_wav};
use faderframe_automation::{AutomationCurve, AutomationPoint, CurveShape};
use faderframe_core::{ChannelLayout, SurroundFormat, SurroundPan, SurroundParam};
use faderframe_engine::EngineConfig;
use faderframe_project::{Command, Project, TrackKind};
use faderframe_session::render::{RenderChannels, RenderRange, RenderSettings, RenderSource};
use faderframe_session::{Action, AutomationTarget, ImportTarget, Session};
use faderframe_timeline::MusicalTime;

fn track_with_dc(
    s: &mut Session,
    dir: &std::path::Path,
    name: &str,
    layout: ChannelLayout,
    v: f32,
) -> faderframe_core::TrackId {
    let file = dir.join(format!("{name}.wav"));
    let channels = vec![vec![v; 48_000 * 4]; layout.channel_count()];
    write_wav(&file, &channels, 48_000, WavFormat::Float32, false).unwrap();
    let t = s.add_track(TrackKind::Audio).unwrap();
    s.dispatch(Action::Edit(Command::RenameTrack {
        track: t,
        name: name.into(),
    }))
    .unwrap();
    s.dispatch(Action::Edit(Command::SetTrackLayout { track: t, layout }))
        .unwrap();
    s.import_audio(
        vec![file],
        ImportTarget {
            track: Some(t),
            at: MusicalTime::ZERO,
        },
    );
    s.wait_for_imports();
    t
}

#[test]
fn a_714_mix_becomes_an_atmos_master() {
    let dir = std::env::temp_dir().join(format!("ff-adm-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut s = Session::new(
        Project::new("Objects", 48_000),
        None,
        EngineConfig::default(),
    )
    .unwrap();
    let master = s.project().master_id().unwrap();
    s.dispatch(Action::Edit(Command::SetTrackLayout {
        track: master,
        layout: ChannelLayout::Surround(SurroundFormat::S714),
    }))
    .unwrap();
    s.dispatch(Action::Edit(Command::SetTrackVolume {
        track: master,
        db: -6.0,
    }))
    .unwrap();
    let bed = track_with_dc(&mut s, &dir, "Bed", ChannelLayout::Mono, 0.5);
    let mover = track_with_dc(&mut s, &dir, "Mover", ChannelLayout::Mono, 0.5);
    let pair = track_with_dc(&mut s, &dir, "Pair", ChannelLayout::Stereo, 0.25);
    s.dispatch(Action::Edit(Command::SetTrackVolume {
        track: mover,
        db: -6.0,
    }))
    .unwrap();
    for t in [mover, pair] {
        s.dispatch(Action::Edit(Command::SetTrackObject { track: t, on: true }))
            .unwrap();
    }
    s.dispatch(Action::Edit(Command::SetTrackSurround {
        track: pair,
        pan: SurroundPan {
            y: -1.0,
            width: 0.5,
            z: 1.0,
            ..SurroundPan::default()
        },
    }))
    .unwrap();
    // The mover crosses from left to right over the second beat.
    s.dispatch(Action::ShowAutomation {
        track: mover,
        target: AutomationTarget::Surround(SurroundParam::X),
    })
    .unwrap();
    let lane = s.shown_lanes(mover)[0].clone();
    let point = |q: f64, v: f64| AutomationPoint {
        time: MusicalTime::from_quarters(q),
        value: v,
        shape: CurveShape::Linear,
    };
    s.dispatch(Action::Edit(Command::SetAutomationLane {
        track: mover,
        lane: Box::new(faderframe_automation::AutomationLane {
            curve: AutomationCurve::from_points(vec![
                point(0.0, -1.0),
                point(1.0, -1.0),
                point(2.0, 1.0),
            ]),
            ..lane
        }),
    }))
    .unwrap();
    let plan = faderframe_session::adm::plan(s.project()).unwrap();
    assert_eq!(plan.describe(), "7.1 bed and 7 objects");
    let out = std::env::var("FADERFRAME_ADM_SESSION_OUT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| dir.join("master.wav"));
    // `FADERFRAME_ADM_SESSION_PROFILE=itu` for the EBU renderer's reading.
    let profile = match std::env::var("FADERFRAME_ADM_SESSION_PROFILE").as_deref() {
        Ok("itu") => faderframe_adm::Profile::Itu,
        _ => faderframe_adm::Profile::DolbyAtmos,
    };
    let job = s
        .render(RenderSettings {
            range: RenderRange::Bars { start: 0, end: 1 },
            source: RenderSource::Master,
            channels: RenderChannels::Adm(profile),
            tail_seconds: 0.0,
            dither: faderframe_audio_files::Dither::Off,
            ..RenderSettings::defaults_for(s.project(), out.clone())
        })
        .unwrap();
    job.join().unwrap();
    let axml = faderframe_audio_files::wavstream::read_chunk(&out, b"axml")
        .unwrap()
        .unwrap();
    let chna = faderframe_audio_files::wavstream::read_chunk(&out, b"chna")
        .unwrap()
        .unwrap();
    let scene = faderframe_adm::parse(std::str::from_utf8(&axml).unwrap(), &chna).unwrap();
    if profile == faderframe_adm::Profile::DolbyAtmos {
        assert_eq!(scene.programme, "Atmos_Master");
        // Dolby's metadata chunk: the bed's LFE off for headphones.
        let dbmd = faderframe_audio_files::wavstream::read_chunk(&out, b"dbmd")
            .unwrap()
            .unwrap();
        let modes = faderframe_adm::binaural_modes(&dbmd).unwrap();
        assert_eq!(modes.len(), 15);
        assert_eq!(modes[3], Some(faderframe_adm::BinauralMode::Off));
    }
    assert_eq!(scene.bed.len(), 8);
    let names: Vec<&str> = scene.objects.iter().map(|o| o.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "Bed Ltf", "Bed Rtf", "Bed Ltr", "Bed Rtr", "Mover", "Pair L", "Pair R"
        ]
    );
    // The mover's blocks follow its lane.
    let mover_blocks = &scene.objects[4].blocks;
    assert!(mover_blocks.len() > 10, "{}", mover_blocks.len());
    assert!((mover_blocks[0].position[0] + 1.0).abs() < 1e-6);
    assert!((mover_blocks.last().unwrap().position[0] - 1.0).abs() < 0.02);
    // The pair: width apart, at the back, overhead.
    assert_eq!(scene.objects[5].blocks[0].position, [-0.5, -1.0, 1.0]);
    assert_eq!(scene.objects[6].blocks[0].position, [0.5, -1.0, 1.0]);
    // Audio: the bed track in C through the master fader, the objects
    // unpanned at their own levels (no master fader).
    let f = faderframe_audio_files::wavstream::WavFile::open(&out).unwrap();
    assert_eq!(f.channels(), 15);
    assert_eq!(f.format(), WavFormat::Pcm24);
    let mut chans = vec![vec![0.0f32; 1]; 15];
    let mut refs: Vec<&mut [f32]> = chans.iter_mut().map(|c| &mut c[..]).collect();
    f.read(24_000, &mut refs, &mut Vec::new()).unwrap();
    let g = faderframe_core::db_to_gain(-6.0);
    let near = |a: f32, b: f32| (a - b).abs() < 2e-3;
    assert!(near(chans[2][0], 0.5 * g), "C {}", chans[2][0]);
    assert!(near(chans[12][0], 0.5 * g), "Mover {}", chans[12][0]);
    assert!(near(chans[13][0], 0.25) && near(chans[14][0], 0.25));
    assert!(
        chans[0][0].abs() < 1e-4,
        "nothing of the objects in the bed"
    );
    let _ = bed;

    // Read back into a new project: the bed as a 7.1 track, the tops and
    // the tracks as objects, the mover's crossing as automation, the master
    // in a format with heights.
    let mut back =
        Session::new(Project::new("Back", 48_000), None, EngineConfig::default()).unwrap();
    back.dispatch(Action::ImportAdm(out.clone())).unwrap();
    back.wait_for_adm_imports();
    let p = back.project();
    let master = p.track(p.master_id().unwrap()).unwrap();
    assert_eq!(master.layout, ChannelLayout::Surround(SurroundFormat::S714));
    let bed_track = p.tracks.iter().find(|t| t.name == "Bed").unwrap();
    assert_eq!(
        bed_track.layout,
        ChannelLayout::Surround(SurroundFormat::S71)
    );
    let objects: Vec<&str> = p.objects().map(|t| t.name.as_str()).collect();
    assert_eq!(
        objects,
        [
            "Bed Ltf", "Bed Rtf", "Bed Ltr", "Bed Rtr", "Mover", "Pair L", "Pair R"
        ]
    );
    let mover_back = p.tracks.iter().find(|t| t.name == "Mover").unwrap();
    assert_eq!(mover_back.surround.x, -1.0);
    let lane = mover_back
        .automation
        .lane(AutomationTarget::Surround(SurroundParam::X))
        .unwrap();
    let at = |q: f64| lane.curve.value_at(MusicalTime::from_quarters(q)).unwrap();
    // Where the original lane is, within half a block's movement.
    for (q, want) in [(0.5, -1.0), (1.25, -0.5), (1.5, 0.0), (1.98, 0.96)] {
        assert!((at(q) - want).abs() < 0.05, "{q}: {} for {want}", at(q));
    }
    let pair_back = p.tracks.iter().find(|t| t.name == "Pair L").unwrap();
    assert_eq!((pair_back.surround.x, pair_back.surround.z), (-0.5, 1.0));
    std::fs::remove_dir_all(&dir).unwrap();
}
