//! A/B listening: the undo history auditioned (a render of the project as
//! undoing would make it, nothing undone) and reference tracks (imported,
//! saved with the project, loudness-matched to the mix, handed to EQ
//! Match).
#![allow(clippy::unwrap_used)]

use faderframe_audio_files::{WavFormat, read_wav, write_wav};
use faderframe_core::db_to_gain;
use faderframe_engine::EngineConfig;
use faderframe_project::Command;
use faderframe_project::reference::ReferenceSync;
use faderframe_session::compare::{CompareOp, Comparing, compare_render_settings};
use faderframe_session::references::{
    MATCH_FROM_REFERENCE, MATCH_REFERENCE_KEY, MATCH_TRACK_KEY, ReferenceOp,
};
use faderframe_session::render;
use faderframe_session::{Action, Session, UiRequest};
use faderframe_timeline::MusicalTime;
use std::time::Duration;

const WAIT: Duration = Duration::from_secs(300);

fn temp(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("ff-session-compare-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn track(s: &Session, name: &str) -> faderframe_core::TrackId {
    s.project()
        .tracks
        .iter()
        .find(|t| t.name == name)
        .unwrap()
        .id
}

/// Integrated loudness of a stereo mix.
fn lufs(channels: &[Vec<f32>], rate: u32) -> f64 {
    let views: Vec<&[f32]> = channels.iter().map(Vec::as_slice).collect();
    faderframe_analysis::integrated_weighted(&views, &[1.0, 1.0], rate).0
}

#[test]
fn the_history_auditioned_is_the_project_as_undoing_makes_it() {
    let dir = temp("history");
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let bass = track(&s, "Bass");
    let drums = s.project().tracks[0].id;
    s.dispatch(Action::Edit(Command::SetTrackVolume {
        track: bass,
        db: -14.0,
    }))
    .unwrap();
    s.dispatch(Action::Edit(Command::SetTrackMute {
        track: drums,
        on: true,
    }))
    .unwrap();
    s.dispatch(Action::Edit(Command::RenameTrack {
        track: bass,
        name: "Low".into(),
    }))
    .unwrap();
    // The loop off (a step too): the edit selection is auditioned.
    let range = s.project().loop_range;
    s.dispatch(Action::Edit(Command::SetLoop {
        range,
        enabled: false,
    }))
    .unwrap();
    // Two bars of the song, selected.
    let meter = s.project().timeline.meter.clone();
    s.selection.range = Some(faderframe_session::editing::EditRange::new(
        meter.bar_start(4),
        meter.bar_start(6),
    ));
    assert_eq!(s.audition_range(), (meter.bar_start(4), meter.bar_start(6)));
    let before = s.project().clone();
    let steps = s.history_steps();

    // Then: just after the first step (the bass turned down, the drums
    // still playing).
    s.dispatch(Action::AuditionHistory(1)).unwrap();
    assert!(s.audition_info().unwrap().progress.is_some(), "rendering");
    assert!(s.wait_for_compare(WAIT));
    let info = s.audition_info().unwrap();
    assert_eq!(info.error, None);
    assert_eq!((info.done, info.label.as_str()), (1, "Change Volume"));
    assert!(info.path.exists());
    // Nothing was undone.
    assert_eq!(s.project(), &before);
    assert_eq!(s.history_steps(), steps);
    // Matched: the louder Then (the drums play) is brought to Now.
    let (then, now) = (info.then_lufs.unwrap(), info.now_lufs.unwrap());
    assert!(then > now + 0.5, "then {then} now {now}");
    let status = s.compare_status().unwrap();
    assert_eq!(status.what, Comparing::History { done: 1 });
    assert!(status.ready && status.matched && !status.hearing_other);
    let shared = std::sync::Arc::clone(s.compare_link().shared());
    assert!((shared.gain() - db_to_gain((now - then) as f32)).abs() < 1e-4);
    // Flip: Then is heard; unmatched: at its own level.
    s.dispatch(Action::Compare(CompareOp::Flip)).unwrap();
    assert!(shared.hearing() && s.compare_status().unwrap().hearing_other);
    assert!(
        s.compare_status()
            .unwrap()
            .text
            .starts_with("Hearing: Then")
    );
    s.dispatch(Action::Compare(CompareOp::MatchLoudness(false)))
        .unwrap();
    assert_eq!(shared.gain(), 1.0);
    s.dispatch(Action::Compare(CompareOp::Flip)).unwrap();
    assert!(!shared.hearing());

    // The render equals one of the project after really undoing the steps.
    let then_audio = read_wav(&info.path).unwrap();
    s.dispatch(Action::HistoryTo(1)).unwrap();
    let undone = s.project().clone();
    let out = dir.join("undone.wav");
    render::start(
        undone.clone(),
        compare_render_settings(&undone, info.span, info.rate, out.clone()),
    )
    .unwrap()
    .join()
    .unwrap();
    let really = read_wav(&out).unwrap();
    assert_eq!(then_audio.channels.len(), really.channels.len());
    for (a, b) in then_audio.channels.iter().zip(&really.channels) {
        assert_eq!(a.len(), b.len());
        // Bit for bit.
        assert!(
            a == b,
            "differs by {}",
            a.iter()
                .zip(b)
                .map(|(x, y)| (x - y).abs())
                .fold(0.0f32, f32::max)
        );
    }
    assert!(then_audio.channels[0].iter().any(|v| v.abs() > 1e-3));

    // Off: the mix alone, the files gone.
    s.dispatch(Action::Compare(CompareOp::Off)).unwrap();
    assert!(s.compare_status().is_none());
    assert!(!info.path.exists());
    // The present is not auditioned.
    let done = s.history_steps().0.len();
    assert!(s.dispatch(Action::AuditionHistory(done)).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn references_import_persist_match_the_mix_and_feed_eq_match() {
    let dir = temp("references");
    let rate = 48_000;
    // A loud record: noise with a strong low end, 8 seconds.
    let mut x = 777u32;
    let mut lp = 0.0f32;
    let noise: Vec<f32> = (0..rate as usize * 8)
        .map(|_| {
            x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let w = (x >> 8) as f32 / (1u32 << 24) as f32 - 0.5;
            lp += 0.05 * (w - lp);
            0.8 * w + 4.0 * lp
        })
        .collect();
    let file = dir.join("Record.wav");
    write_wav(
        &file,
        &[noise.clone(), noise.iter().map(|v| v * 0.9).collect()],
        rate,
        WavFormat::Float32,
        false,
    )
    .unwrap();

    let mut s = Session::demo(EngineConfig::default()).unwrap();
    s.dispatch(Action::Reference(ReferenceOp::Add(vec![file.clone()])))
        .unwrap();
    assert!(s.importing_references());
    assert!(s.wait_for_compare(WAIT));
    let r = s.references().tracks[0].clone();
    assert_eq!(r.name, "Record");
    assert_eq!(s.history_steps().0.last().unwrap(), "Add Reference");
    // Chosen (nothing was compared) and measured.
    assert_eq!(s.current_reference(), Some(r.id));
    let m = s.reference_measure(r.id).unwrap().clone();
    let (mix, reference) = (m.mix_lufs.unwrap(), m.reference_lufs.unwrap());
    assert!(reference > mix, "a loud record: {reference} vs {mix}");
    assert!(m.reference.is_some() && m.mix.is_some());
    assert!(!s.reference_measure_stale(r.id));
    // Not part of the mix: no track plays it; its media is used.
    assert!(s.project().used_sources().contains(&r.source));
    assert!(
        s.project()
            .clips
            .values()
            .all(|c| !c.content.referenced_sources().contains(&r.source))
    );

    // Level-matched within 0.1 LU: the record at the gain applied against
    // the mix rendered over the same stretch (independently here).
    let shared = std::sync::Arc::clone(s.compare_link().shared());
    let gain = shared.gain();
    assert!(gain < 1.0 && s.compare_status().unwrap().matched);
    let media = match &s.project().sources[&r.source].spec {
        faderframe_project::SourceSpec::File { path, .. } => path.clone(),
        _ => unreachable!(),
    };
    let heard: Vec<Vec<f32>> = read_wav(&media)
        .unwrap()
        .channels
        .into_iter()
        .map(|c| c.into_iter().map(|v| v * gain).collect())
        .collect();
    let project = s.project().clone();
    let out = dir.join("mix.wav");
    render::start(
        project.clone(),
        compare_render_settings(&project, m.mix_span, rate, out.clone()),
    )
    .unwrap()
    .join()
    .unwrap();
    let mix_audio = read_wav(&out).unwrap();
    let a = lufs(&heard, rate);
    let b = lufs(&mix_audio.channels, rate);
    assert!(
        (a - b).abs() < 0.1,
        "reference {a} LUFS against the mix {b} LUFS"
    );

    // Flip to it and back.
    s.dispatch(Action::Compare(CompareOp::Flip)).unwrap();
    assert!(shared.hearing());
    // Monitoring only: the session's render while it is heard is the mix.
    let again = dir.join("mix-again.wav");
    s.render(compare_render_settings(
        &project,
        m.mix_span,
        rate,
        again.clone(),
    ))
    .unwrap()
    .join()
    .unwrap();
    assert_eq!(read_wav(&again).unwrap().channels, mix_audio.channels);
    assert!(
        s.compare_status()
            .unwrap()
            .text
            .starts_with("Hearing: Reference — ‘Record’")
    );
    s.dispatch(Action::Compare(CompareOp::Hear(false))).unwrap();
    assert!(!shared.hearing());

    // Looping its own region: measured again (the mix's stretch moved).
    s.dispatch(Action::Reference(ReferenceOp::SetRegion(
        r.id,
        Some((rate as i64, rate as i64 * 3)),
    )))
    .unwrap();
    s.dispatch(Action::Reference(ReferenceOp::SetSync(
        r.id,
        ReferenceSync::Loop,
    )))
    .unwrap();
    s.tick(0.016);
    assert!(s.wait_for_compare(WAIT));
    let m2 = s.reference_measure(r.id).unwrap();
    assert_ne!(m2.reference_lufs, m.reference_lufs);
    // The loop (the demo's first eight bars) instead of where it played.
    assert_eq!(m2.mix_span.0, MusicalTime::ZERO);
    assert_ne!(m2.mix_span, m.mix_span);

    // Send to EQ Match: the master gets an EQ whose match reference is it.
    s.take_ui_requests();
    s.dispatch(Action::Reference(ReferenceOp::SendToEqMatch {
        reference: r.id,
        track: None,
    }))
    .unwrap();
    let master = s.project().master_id().unwrap();
    let eq = s
        .project()
        .track(master)
        .unwrap()
        .slots()
        .into_iter()
        .find(|p| p.plugin.id == faderframe_core::builtin::EQ)
        .unwrap()
        .id;
    assert_eq!(
        s.device_view(eq, MATCH_REFERENCE_KEY),
        Some(MATCH_FROM_REFERENCE)
    );
    assert_eq!(s.device_view(eq, MATCH_TRACK_KEY), Some(r.id.raw() as f64));
    assert!(s.reference_spectrum(r.id).is_some());
    assert!(
        s.take_ui_requests()
            .iter()
            .any(|u| matches!(u, UiRequest::PluginEditor { plugin, .. } if *plugin == eq))
    );

    // Saved and opened again: the list, the file in the project's media.
    let path = dir.join("Song").join("Song.ffproj");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    s.save_as(&path).unwrap();
    let saved = s.references().clone();
    let mut t = Session::demo(EngineConfig::default()).unwrap();
    t.open(&path).unwrap();
    assert_eq!(t.references(), &saved);
    let r2 = t.references().tracks[0].clone();
    match &t.project().sources[&r2.source].spec {
        faderframe_project::SourceSpec::File { path: p, .. } => {
            assert!(p.starts_with(dir.join("Song")), "{}", p.display());
            assert!(p.exists());
        }
        _ => panic!("not a file"),
    }
    // Nothing compared in the project opened until one is chosen.
    t.tick(0.016);
    assert_eq!(t.current_reference(), None);
    t.dispatch(Action::Reference(ReferenceOp::Choose(Some(r2.id))))
        .unwrap();
    assert!(t.wait_for_compare(WAIT));
    assert!(t.compare_status().unwrap().ready);

    // Removing it (an undoable step) stops comparing.
    s.dispatch(Action::Reference(ReferenceOp::Remove(r.id)))
        .unwrap();
    assert!(s.references().is_empty() && s.compare_status().is_none());
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(s.references().tracks.len(), 1);
    drop(s);
    drop(t);
    let _ = std::fs::remove_dir_all(&dir);
}
