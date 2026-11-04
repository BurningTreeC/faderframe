//! AAF out and back in: a project with a stereo and a mono track, fades,
//! a crossfade, clip gain, a volume and pan envelope, a reversed clip
//! (rendered) and a marker, exported with its audio inside and as linked
//! WAVE files, read back into an empty project — tracks per channel,
//! clips where they were, the audio as it was at 24 bits; linked media
//! moved away are found again in a folder the user names, or left
//! offline. `FADERFRAME_AAF_SESSION_OUT=<dir>` keeps the exported files.
#![allow(clippy::unwrap_used)]

use faderframe_aaf::read::AafFile;
use faderframe_aaf::timeline::Essence;
use faderframe_audio_files::wavstream::WavFile;
use faderframe_audio_files::{WavFormat, write_wav};
use faderframe_core::ClipId;
use faderframe_engine::EngineConfig;
use faderframe_project::clip_envelope::{ClipEnvelope, EnvelopePoint};
use faderframe_project::{ClipContent, Command, FadeShape, Project, SourceSpec};
use faderframe_session::aaf::AafMedia;
use faderframe_session::{Action, Session, UiRequest};
use faderframe_timeline::MusicalTime;
use std::path::{Path, PathBuf};

const SR: u32 = 48_000;

fn tone(hz: f64, secs: f64, level: f32) -> Vec<f32> {
    (0..(secs * f64::from(SR)) as usize)
        .map(|n| level * (std::f64::consts::TAU * hz * n as f64 / f64::from(SR)).sin() as f32)
        .collect()
}

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ff-aaf-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// The project: "Stereo" (one clip, fades, gain, envelope), "Mono" (two
/// clips crossfading, the second reversed), a marker.
fn project(dir: &Path) -> (Session, ClipId, ClipId, ClipId) {
    // The recordings elsewhere: never mistaken for the exported media.
    let dir = &dir.with_extension("sources");
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir).unwrap();
    let stereo = dir.join("Stereo.wav");
    write_wav(
        &stereo,
        &[tone(220.0, 3.0, 0.5), tone(330.0, 3.0, 0.4)],
        SR,
        WavFormat::Float32,
        false,
    )
    .unwrap();
    let mono = dir.join("Mono.wav");
    write_wav(
        &mono,
        &[tone(440.0, 2.0, 0.6)],
        SR,
        WavFormat::Float32,
        false,
    )
    .unwrap();
    let mut s = Session::new(Project::new("Reel", SR), None, EngineConfig::default()).unwrap();
    for f in [stereo, mono.clone(), mono] {
        s.dispatch(Action::ImportFiles {
            files: vec![f],
            track: None,
            at: MusicalTime::ZERO,
        })
        .unwrap();
        s.wait_for_imports();
    }
    let mut clips: Vec<ClipId> = s.project().clips.keys().copied().collect();
    clips.sort();
    let (a, b, c) = (clips[0], clips[1], clips[2]);
    // The second mono clip onto the first one's track, crossfading.
    let mono_track = s.project().clip(b).unwrap().track;
    let edit =
        |s: &mut Session, id: ClipId, at: i64, f: &dyn Fn(&mut faderframe_project::AudioClip)| {
            let clip = s.project().clip(id).unwrap().clone();
            let mut content = clip.content.clone();
            let ClipContent::Audio(au) = &mut content else {
                panic!()
            };
            f(au);
            let start = s.project().timeline.at_sample(at, f64::from(SR));
            s.dispatch(Action::Edit(Command::SetClipContent {
                clip: id,
                start,
                content: Box::new(content),
            }))
            .unwrap();
        };
    edit(&mut s, a, 48_000, &|au| {
        au.gain_db = -3.0;
        au.fades.fade_in = 4_800;
        au.fades.fade_in_shape = FadeShape::EqualPower;
        au.fades.fade_out = 9_600;
        au.envelope = Some(Box::new(ClipEnvelope {
            volume: vec![
                EnvelopePoint::new(0, 0.0),
                EnvelopePoint::new(96_000, -12.0),
            ],
            pan: vec![EnvelopePoint::new(0, 0.0), EnvelopePoint::new(96_000, 0.5)],
        }));
    });
    edit(&mut s, b, 0, &|au| {
        au.length = 72_000;
        au.fades.fade_out = 24_000;
    });
    let c_track = s.project().clip(c).unwrap().track;
    s.dispatch(Action::Edit(Command::MoveClip {
        clip: c,
        track: mono_track,
        start: s.project().timeline.at_sample(48_000, f64::from(SR)),
    }))
    .unwrap();
    edit(&mut s, c, 48_000, &|au| {
        au.source_offset = 12_000;
        au.length = 48_000;
        au.fades.fade_in = 24_000;
        au.reversed = true;
    });
    s.dispatch(Action::Edit(Command::RemoveTrack { track: c_track }))
        .unwrap();
    let at = s.project().timeline.at_sample(72_000, f64::from(SR));
    s.dispatch(Action::AddMarker(at)).unwrap();
    (s, a, b, c)
}

fn export(s: &mut Session, path: &Path, media: AafMedia) {
    s.dispatch(Action::ExportAaf {
        path: path.to_path_buf(),
        media,
    })
    .unwrap();
    s.wait_for_aaf();
    assert!(
        path.is_file(),
        "{:?}",
        s.notices().map(|n| n.text.clone()).collect::<Vec<_>>()
    );
    // For outside readers (pyaaf2): `FADERFRAME_AAF_SESSION_OUT=<dir>`.
    if let Some(out) = std::env::var_os("FADERFRAME_AAF_SESSION_OUT") {
        let out = PathBuf::from(out);
        std::fs::create_dir_all(&out).unwrap();
        std::fs::copy(path, out.join(path.file_name().unwrap())).unwrap();
    }
}

fn import(path: &Path) -> Session {
    let mut s = Session::new(Project::new("Back", SR), None, EngineConfig::default()).unwrap();
    s.dispatch(Action::ImportAaf(path.to_path_buf())).unwrap();
    s.wait_for_aaf();
    s
}

/// (track name, start sample, length, gain, fade in, fade out) by track
/// order.
fn layout(s: &Session) -> Vec<(String, i64, i64, f32, i64, i64)> {
    let p = s.project();
    let mut out = Vec::new();
    for t in p.folder_order() {
        for c in p.clips_of(t.id) {
            let ClipContent::Audio(a) = &c.content else {
                continue;
            };
            out.push((
                t.name.clone(),
                p.timeline.to_samples(c.start, f64::from(SR)),
                a.length,
                a.gain_db,
                a.fades.fade_in,
                a.fades.fade_out,
            ));
        }
    }
    out.sort_by(|x, y| x.0.cmp(&y.0).then(x.1.cmp(&y.1)));
    out
}

/// A clip's audio as played (no fades): `n` frames from `from`.
fn audio(s: &Session, track: &str, n: usize, from: i64) -> Vec<f32> {
    let p = s.project();
    let t = p.tracks.iter().find(|t| t.name == track).unwrap();
    let c = p.clips_of(t.id)[0];
    let ClipContent::Audio(a) = &c.content else {
        panic!()
    };
    let SourceSpec::File { path, .. } = &p.sources[&a.source].spec else {
        panic!()
    };
    let f = WavFile::open(path).unwrap();
    let mut buf = vec![0f32; n];
    f.read(
        (a.source_offset + from) as u64,
        &mut [&mut buf[..]],
        &mut Vec::new(),
    )
    .unwrap();
    buf
}

#[test]
fn a_project_goes_out_as_aaf_and_comes_back() {
    let d = dir("embed");
    let (mut s, _, _, _) = project(&d);
    let plan = s.aaf_export_plan(AafMedia::Embed).unwrap();
    // A track per channel; the reversed clip rendered.
    let names: Vec<_> = plan
        .composition()
        .tracks
        .iter()
        .map(|t| t.name.clone())
        .collect();
    assert_eq!(names, ["Stereo.L", "Stereo.R", "Mono"]);
    assert!(
        plan.notes().iter().any(|n| n.contains("rendered")),
        "{:?}",
        plan.notes()
    );
    let left = &plan.composition().tracks[0].clips[0];
    let right = &plan.composition().tracks[1].clips[0];
    // The pan's balance: the left side fades by half at the end.
    let (l, r) = (
        left.envelope.last().unwrap().1,
        right.envelope.last().unwrap().1,
    );
    assert!((l - r + 6.02).abs() < 0.1, "{l} {r}");

    let path = d.join("Reel.aaf");
    export(&mut s, &path, AafMedia::Embed);
    // pyaaf2-free check of what is inside: three tracks, media embedded.
    let read = AafFile::open_path(&path).unwrap().composition(SR).unwrap();
    assert_eq!(read.composition.tracks.len(), 3);
    assert!(
        read.composition
            .media
            .iter()
            .all(|m| m.essence.iter().all(|e| matches!(e, Essence::Embedded(_))))
    );

    let back = import(&path);
    let got = layout(&back);
    assert_eq!(
        got,
        vec![
            ("Mono".into(), 0, 72_000, 0.0, 0, 24_000),
            ("Mono".into(), 48_000, 48_000, 0.0, 24_000, 0),
            // With an envelope the clip gain travels inside it (one
            // Amplitude per gain operation).
            ("Stereo.L".into(), 48_000, 144_000, 0.0, 4_800, 9_600),
            ("Stereo.R".into(), 48_000, 144_000, 0.0, 4_800, 9_600),
        ]
    );
    let p = back.project();
    // A project without a timecode takes the AAF's.
    assert!(p.timecode.is_some());
    assert_eq!(p.markers.len(), 1);
    assert_eq!(
        p.timeline.to_samples(p.markers[0].position, f64::from(SR)),
        72_000
    );
    // The fade-in curve and the envelope came along.
    let stereo_l = p.tracks.iter().find(|t| t.name == "Stereo.L").unwrap();
    let ClipContent::Audio(a) = &p.clips_of(stereo_l.id)[0].content else {
        panic!()
    };
    assert_eq!(a.fades.fade_in_shape, FadeShape::EqualPower);
    let env = a.envelope.as_ref().unwrap();
    let at = |x: f64| f64::from(env.gain_at(x)).log10() * 20.0;
    assert!((at(0.0) + 3.0).abs() < 0.05, "{}", at(0.0));
    assert!(
        (at(96_000.0) - (-3.0 - 12.0 - 6.02)).abs() < 0.2,
        "{}",
        at(96_000.0)
    );
    // Halfway: volume −6 dB, pan 0.25 (the left at 0.75).
    assert!((at(48_000.0) - (-3.0 - 6.0 + 20.0 * 0.75f64.log10())).abs() < 0.2);
    // The audio, at 24 bits: the left channel is the 220 Hz tone.
    let want = tone(220.0, 3.0, 0.5);
    let heard = audio(&back, "Stereo.L", 4_800, 0);
    for (k, v) in heard.iter().enumerate() {
        assert!((v - want[k]).abs() < 1e-6, "{k}");
    }
    let want = tone(330.0, 3.0, 0.4);
    let heard = audio(&back, "Stereo.R", 4_800, 1000);
    for (k, v) in heard.iter().enumerate() {
        assert!((v - want[1000 + k]).abs() < 1e-6, "{k}");
    }
    // The reversed clip came back rendered: the source's frames backwards.
    let src = tone(440.0, 2.0, 0.6);
    let p = back.project();
    let mono = p.tracks.iter().find(|t| t.name == "Mono").unwrap();
    let clips = p.clips_of(mono.id);
    let rev = clips
        .iter()
        .find(|c| p.timeline.to_samples(c.start, f64::from(SR)) == 48_000)
        .unwrap();
    let ClipContent::Audio(a) = &rev.content else {
        panic!()
    };
    let SourceSpec::File { path: rp, .. } = &p.sources[&a.source].spec else {
        panic!()
    };
    let f = WavFile::open(rp).unwrap();
    let mut buf = vec![0f32; 1000];
    f.read(
        (a.source_offset + 30_000) as u64,
        &mut [&mut buf[..]],
        &mut Vec::new(),
    )
    .unwrap();
    // Played backwards from the clip's end: frame k is source frame
    // offset + length − 1 − k.
    for (k, v) in buf.iter().enumerate() {
        let at = 12_000 + 48_000 - 1 - (30_000 + k);
        assert!((v - src[at]).abs() < 2e-4, "{k}: {v} vs {}", src[at]);
    }
    let _ = std::fs::remove_dir_all(&d);
    let _ = std::fs::remove_dir_all(d.with_extension("sources"));
}

#[test]
fn linked_media_moved_away_are_found_in_the_folder_named() {
    let d = dir("link");
    let (mut s, ..) = project(&d);
    let path = d.join("Reel Linked.aaf");
    export(&mut s, &path, AafMedia::Link);
    let media = d.join("Reel Linked Media");
    let files: Vec<_> = std::fs::read_dir(&media).unwrap().flatten().collect();
    assert_eq!(files.len(), 4, "{files:?}");
    // Each a mono 24-bit WAVE file.
    for f in &files {
        let w = WavFile::open(&f.path()).unwrap();
        assert_eq!((w.channels(), w.format()), (1, WavFormat::Pcm24));
    }
    // Moved into a folder beside the AAF: found without asking.
    let near = d.join("near");
    std::fs::rename(&media, &near).unwrap();
    let mut found = Session::new(Project::new("Found", SR), None, EngineConfig::default()).unwrap();
    found.dispatch(Action::ImportAaf(path.clone())).unwrap();
    found.wait_for_aaf();
    assert!(!found.aaf_waiting_for_media());
    assert_eq!(layout(&found).len(), 4);
    let moved = d.join("elsewhere").join("a").join("b").join("deeper");
    std::fs::create_dir_all(moved.parent().unwrap()).unwrap();
    std::fs::rename(&near, &moved).unwrap();

    // Not where the AAF says, nor near it: asked about.
    let mut back = Session::new(Project::new("Back", SR), None, EngineConfig::default()).unwrap();
    back.dispatch(Action::ImportAaf(path.clone())).unwrap();
    back.wait_for_aaf();
    let asked = back.take_ui_requests().into_iter().find_map(|r| match r {
        UiRequest::RelinkAaf { missing, .. } => Some(missing),
        _ => None,
    });
    let missing = asked.expect("asked for the missing media");
    assert_eq!(missing.len(), 4);
    assert!(back.aaf_waiting_for_media());
    assert!(back.project().clips.is_empty());
    back.dispatch(Action::RelinkAaf(Some(d.join("elsewhere"))))
        .unwrap();
    back.wait_for_aaf();
    assert_eq!(layout(&back).len(), 4);
    let want = tone(220.0, 3.0, 0.5);
    let heard = audio(&back, "Stereo.L", 480, 0);
    for (k, v) in heard.iter().enumerate() {
        assert!((v - want[k]).abs() < 1e-6, "{k}");
    }

    // Imported without them: the clips stay, offline.
    let mut offline =
        Session::new(Project::new("Offline", SR), None, EngineConfig::default()).unwrap();
    offline.dispatch(Action::ImportAaf(path)).unwrap();
    offline.wait_for_aaf();
    offline.dispatch(Action::RelinkAaf(None)).unwrap();
    assert_eq!(layout(&offline).len(), 4);
    let p = offline.project();
    assert!(
        p.sources
            .values()
            .all(|s| matches!(&s.spec, SourceSpec::File { path, .. } if !path.exists()))
    );
    let _ = std::fs::remove_dir_all(&d);
    let _ = std::fs::remove_dir_all(d.with_extension("sources"));
}

#[test]
fn nothing_to_export_is_said() {
    let mut s = Session::new(Project::new("Empty", SR), None, EngineConfig::default()).unwrap();
    let r = s.dispatch(Action::ExportAaf {
        path: std::env::temp_dir().join("ff-aaf-nothing.aaf"),
        media: AafMedia::Embed,
    });
    assert!(r.is_err());
}
