//! Stem separation's placement, on a stand-in separator (the model is not
//! in the repository; `faderframe-separate`'s tests check it against the
//! reference): the stems land on new tracks where the separated audio
//! was, in a folder when there are several, add up to what was separated,
//! and come with the originals muted, all in one undo step.
#![allow(clippy::unwrap_used)]

use faderframe_audio_files::{WavFormat, read_wav, write_wav};
use faderframe_core::{ClipId, TrackId};
use faderframe_engine::EngineConfig;
use faderframe_project::{Project, SourceSpec, TrackKind};
use faderframe_session::separate::{Stem, StemSeparator, StemTarget};
use faderframe_session::{Action, Session};
use faderframe_timeline::MusicalTime;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

const SR: u32 = 48_000;

/// Splits by fixed shares (vocals ½, drums ¼, bass ⅛, other ⅛) at
/// 44.1 kHz, as the model would hand them back.
struct Shares;

impl StemSeparator for Shares {
    fn sample_rate(&self) -> u32 {
        44_100
    }

    fn sources(&self) -> Vec<String> {
        ["bass", "drums", "vocals", "other"]
            .map(String::from)
            .to_vec()
    }

    fn separate(
        &self,
        left: &[f32],
        right: &[f32],
        progress: &mut dyn FnMut(f32) -> bool,
    ) -> Result<Vec<[Vec<f32>; 2]>, String> {
        progress(0.5);
        let part = |k: f32| {
            [
                left.iter().map(|v| v * k).collect(),
                right.iter().map(|v| v * k).collect(),
            ]
        };
        Ok(vec![part(0.125), part(0.25), part(0.5), part(0.125)])
    }
}

/// Waits until told to stop (a long separation).
struct Endless(Arc<AtomicBool>);

impl StemSeparator for Endless {
    fn sample_rate(&self) -> u32 {
        44_100
    }

    fn sources(&self) -> Vec<String> {
        Shares.sources()
    }

    fn separate(
        &self,
        _left: &[f32],
        _right: &[f32],
        progress: &mut dyn FnMut(f32) -> bool,
    ) -> Result<Vec<[Vec<f32>; 2]>, String> {
        self.0.store(true, Ordering::Relaxed);
        while progress(0.1) {
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        Err("cancelled".into())
    }
}

/// A stereo phrase: a chord left, a bass note right, 1.5 s.
fn phrase() -> [Vec<f32>; 2] {
    let n = (1.5 * f64::from(SR)) as usize;
    let tau = std::f64::consts::TAU;
    let mut l = Vec::with_capacity(n);
    let mut r = Vec::with_capacity(n);
    for i in 0..n {
        let t = i as f64 / f64::from(SR);
        let env = (t / 0.02).min(1.0) * ((1.5 - t) / 0.05).clamp(0.0, 1.0);
        l.push((env * 0.3 * ((tau * 330.0 * t).sin() + (tau * 440.0 * t).sin())) as f32);
        r.push((env * 0.4 * (tau * 110.0 * t).sin()) as f32);
    }
    [l, r]
}

fn session(name: &str, files: &[(&str, [Vec<f32>; 2], f64)]) -> Session {
    let dir = std::env::temp_dir().join(format!("ff-session-stems-{}-{name}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let mut s = Session::new(Project::new("Stems", SR), None, EngineConfig::default()).unwrap();
    s.set_stem_separator(Some(Arc::new(Shares)));
    for (i, (file, audio, at)) in files.iter().enumerate() {
        let path = dir.join(format!("{file}.wav"));
        write_wav(&path, audio, SR, WavFormat::Float32, false).unwrap();
        let track = s
            .project()
            .tracks
            .iter()
            .find(|t| t.kind == TrackKind::Audio)
            .map(|t| t.id)
            .filter(|_| i > 0);
        s.dispatch(Action::ImportFiles {
            files: vec![path],
            track,
            at: MusicalTime::from_quarters(*at),
        })
        .unwrap();
        s.wait_for_imports();
    }
    s
}

fn audio_track(s: &Session) -> TrackId {
    s.project()
        .tracks
        .iter()
        .find(|t| t.kind == TrackKind::Audio)
        .unwrap()
        .id
}

fn first_clip(s: &Session) -> ClipId {
    s.project().clips_of(audio_track(s))[0].id
}

/// A clip's audio, from its source file.
fn audio_of(s: &Session, clip: ClipId) -> Vec<Vec<f32>> {
    let p = s.project();
    let a = p.clip(clip).unwrap().as_audio().unwrap().clone();
    match &p.sources[&a.source].spec {
        SourceSpec::File { path, .. } => read_wav(path).unwrap().channels,
        other => panic!("{other:?}"),
    }
}

fn separate(s: &mut Session, target: StemTarget, stems: Vec<Stem>) {
    s.dispatch(Action::SeparateStems {
        target,
        model: None,
        stems,
    })
    .unwrap();
    assert!(s.separating());
    s.wait_for_separations();
}

#[test]
fn a_clip_separates_into_four_stems_in_a_folder() {
    let mut s = session("four", &[("Song", phrase(), 4.0)]);
    let clip = first_clip(&s);
    let source = audio_track(&s);
    let tracks = s.project().tracks.len();
    separate(&mut s, StemTarget::Clip(clip), Stem::SOURCES.to_vec());
    let p = s.project();
    assert_eq!(p.tracks.len(), tracks + 5, "a folder and four tracks");
    let at = p.track_index(source).unwrap();
    let folder = &p.tracks[at + 1];
    assert!(folder.is_folder());
    assert_eq!(folder.name, "Song Stems");
    let names: Vec<&str> = p.tracks[at + 2..at + 6]
        .iter()
        .map(|t| t.name.as_str())
        .collect();
    assert_eq!(names, ["Vocals", "Drums", "Bass", "Other"]);
    let original = p.clip(clip).unwrap();
    assert!(original.muted, "the original is muted");
    let length = original.as_audio().unwrap().length;
    let mix = audio_of(&s, clip);
    let mut sum = vec![vec![0.0f32; mix[0].len()]; 2];
    for t in &p.tracks[at + 2..at + 6] {
        assert_eq!(t.folder, Some(folder.id));
        assert_eq!(t.output, p.track(source).unwrap().output);
        let clips = p.clips_of(t.id);
        assert_eq!(clips.len(), 1);
        let c = clips[0];
        assert_eq!(c.start, original.start, "where the audio was");
        assert_eq!(c.as_audio().unwrap().length, length);
        let stem = audio_of(&s, c.id);
        assert_eq!(stem.len(), 2);
        assert_eq!(stem[0].len(), mix[0].len());
        for (acc, ch) in sum.iter_mut().zip(&stem) {
            for (a, v) in acc.iter_mut().zip(ch) {
                *a += v;
            }
        }
        if t.name == "Vocals" {
            // Half the mix, through 44.1 kHz and back.
            let worst = (2_000..mix[0].len() - 2_000)
                .map(|i| (stem[0][i] - 0.5 * mix[0][i]).abs())
                .fold(0.0f32, f32::max);
            assert!(worst < 2e-3, "vocals: {worst}");
        }
    }
    // "Other" takes what the rest leave: they add up to the mix.
    for (c, ch) in sum.iter().enumerate() {
        let worst = ch
            .iter()
            .zip(&mix[c])
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        assert!(worst < 1e-5, "channel {c}: {worst}");
    }
    // One undo step, and back.
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(s.project().tracks.len(), tracks);
    assert!(!s.project().clip(clip).unwrap().muted);
    s.dispatch(Action::Redo).unwrap();
    assert_eq!(s.project().tracks.len(), tracks + 5);
    assert_eq!(s.history_steps().0.last().unwrap(), "Separate Stems");
}

#[test]
fn vocals_are_extracted_and_removed_on_tracks_of_their_own() {
    let mut s = session("vocals", &[("Song", phrase(), 0.0)]);
    let clip = first_clip(&s);
    let tracks = s.project().tracks.len();
    separate(&mut s, StemTarget::Clip(clip), vec![Stem::Vocals]);
    let p = s.project();
    assert_eq!(p.tracks.len(), tracks + 1, "no folder for one stem");
    let t = p.tracks.iter().find(|t| t.name == "Song Vocals").unwrap();
    assert!(!t.is_folder());
    let vocals = p.clips_of(t.id)[0].id;
    // Remove Vocals on the original, unmuted again by undoing.
    s.dispatch(Action::Undo).unwrap();
    separate(&mut s, StemTarget::Clip(clip), vec![Stem::Instrumental]);
    let p = s.project();
    assert!(p.clip(vocals).is_none());
    let t = p
        .tracks
        .iter()
        .find(|t| t.name == "Song Instrumental")
        .unwrap();
    let inst = audio_of(&s, p.clips_of(t.id)[0].id);
    let mix = audio_of(&s, clip);
    let worst = (2_000..mix[1].len() - 2_000)
        .map(|i| (inst[1][i] - 0.5 * mix[1][i]).abs())
        .fold(0.0f32, f32::max);
    assert!(worst < 2e-3, "the mix less the vocals: {worst}");
    assert_eq!(s.history_steps().0.last().unwrap(), "Remove Vocals");
}

#[test]
fn a_track_separates_its_clips_as_they_play() {
    let mut s = session("track", &[("A", phrase(), 0.0), ("B", phrase(), 8.0)]);
    let track = audio_track(&s);
    let clips: Vec<ClipId> = s.project().clips_of(track).iter().map(|c| c.id).collect();
    assert_eq!(clips.len(), 2);
    let name = s.project().track(track).unwrap().name.clone();
    separate(
        &mut s,
        StemTarget::Track(track),
        vec![Stem::Drums, Stem::Bass],
    );
    let p = s.project();
    for c in &clips {
        assert!(p.clip(*c).unwrap().muted);
    }
    let folder = p
        .tracks
        .iter()
        .find(|t| t.name == format!("{name} Stems"))
        .unwrap();
    let drums = p
        .tracks
        .iter()
        .find(|t| t.folder == Some(folder.id) && t.name == "Drums")
        .unwrap();
    let c = p.clips_of(drums.id)[0];
    // From the first clip's start to the end of the last.
    assert_eq!(c.start, MusicalTime::from_quarters(0.0));
    let last = p.clip(clips[1]).unwrap();
    assert_eq!(
        c.end(&p.timeline, p.sample_rate),
        last.end(&p.timeline, p.sample_rate)
    );
}

#[test]
fn a_mono_track_gets_mono_stems() {
    let mut s = session("mono", &[("Voice", phrase(), 0.0)]);
    let track = audio_track(&s);
    s.dispatch(Action::Edit(faderframe_project::Command::SetTrackLayout {
        track,
        layout: faderframe_core::ChannelLayout::Mono,
    }))
    .unwrap_or_else(|_| panic!("a mono track"));
    separate(&mut s, StemTarget::Track(track), vec![Stem::Vocals]);
    let p = s.project();
    let t = p
        .tracks
        .iter()
        .find(|t| t.name.ends_with(" Vocals"))
        .unwrap();
    assert_eq!(t.layout, faderframe_core::ChannelLayout::Mono);
    assert_eq!(audio_of(&s, p.clips_of(t.id)[0].id).len(), 1);
}

#[test]
fn a_separation_can_be_cancelled() {
    let mut s = session("cancel", &[("Song", phrase(), 0.0)]);
    let started = Arc::new(AtomicBool::new(false));
    s.set_stem_separator(Some(Arc::new(Endless(Arc::clone(&started)))));
    let tracks = s.project().tracks.len();
    let clip = first_clip(&s);
    s.dispatch(Action::SeparateStems {
        target: StemTarget::Clip(clip),
        model: None,
        stems: Stem::SOURCES.to_vec(),
    })
    .unwrap();
    while !started.load(Ordering::Relaxed) {
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    let status = s.separations();
    assert_eq!(status.len(), 1);
    assert_eq!(status[0].name, "Song");
    s.dispatch(Action::CancelSeparations).unwrap();
    s.wait_for_separations();
    assert_eq!(s.project().tracks.len(), tracks);
    assert!(!s.project().clip(clip).unwrap().muted);
}

#[test]
fn the_menus_offer_stems_for_audio_only() {
    let mut s = session("menu", &[("Song", phrase(), 0.0)]);
    let clip = first_clip(&s);
    let entries = s.stems_menu(StemTarget::Clip(clip));
    assert!(entries.iter().any(|e| e.label == "Extract Vocals"));
    assert!(entries.iter().any(|e| e.action
        == Some(Action::SeparateStems {
            target: StemTarget::Clip(clip),
            model: None,
            stems: vec![Stem::Instrumental],
        })));
    assert!(!s.stems_menu(StemTarget::Track(audio_track(&s))).is_empty());
    s.dispatch(Action::AddTrack(TrackKind::Instrument)).unwrap();
    let inst = s
        .project()
        .tracks
        .iter()
        .find(|t| t.kind == TrackKind::Instrument)
        .unwrap()
        .id;
    assert!(s.stems_menu(StemTarget::Track(inst)).is_empty());
    // An unknown model is refused before anything starts.
    assert!(
        s.dispatch(Action::SeparateStems {
            target: StemTarget::Clip(clip),
            model: Some("no-such-model".into()),
            stems: vec![Stem::Vocals],
        })
        .is_err()
    );
    assert!(!s.separating());
}
