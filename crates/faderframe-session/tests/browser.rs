#![allow(clippy::unwrap_used)]
//! The media browser: files listed with what they are (read and analysed
//! once, kept in an index until they change), filtered, sorted, rated,
//! tagged and found by likeness; previews made in the project's tempo and
//! key; files placed on the arranger and in launcher slots with that match
//! applied as a warp (Follow Tempo) and a transposition, in one step.

use faderframe_engine::EngineConfig;
use faderframe_project::harmony::{Key, KeyChange, Scale};
use faderframe_project::{ClipContent, Command, Project};
use faderframe_session::browser::{
    self, BrowserFilter, BrowserOp, KeyFilter, KindFilter, PreviewSettings, RowItem, SortBy,
    TempoFilter,
};
use faderframe_session::{Action, Session};
use faderframe_timeline::MusicalTime;
use std::path::{Path, PathBuf};
use std::time::Duration;

const RATE: u32 = 48_000;
const WAIT: Duration = Duration::from_secs(300);

fn temp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ff-session-browser-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn write(path: &Path, x: Vec<f32>) {
    faderframe_audio_files::write_wav(
        path,
        &[x.clone(), x],
        RATE,
        faderframe_audio_files::WavFormat::Pcm24,
        false,
    )
    .unwrap();
}

/// A drum loop at `bpm`: a falling-sine kick on every beat, a noise hat
/// between, `beats` long.
fn drum_loop(bpm: f64, beats: usize, pitch: f64) -> Vec<f32> {
    let beat = (f64::from(RATE) * 60.0 / bpm) as usize;
    let n = beat * beats;
    let mut seed = 7u32;
    (0..n)
        .map(|i| {
            let t = (i % beat) as f64 / f64::from(RATE);
            let f = pitch * (1.0 + 2.0 * (-t * 40.0).exp());
            let kick = (std::f64::consts::TAU * f * t).sin() * (-t * 14.0).exp() * 0.8;
            let h = ((i + beat / 2) % beat) as f64 / f64::from(RATE);
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            let noise = f64::from(seed >> 9) / f64::from(1u32 << 23) * 2.0 - 1.0;
            let hat = noise * (-h * 80.0).exp() * 0.25;
            (kick + hat) as f32
        })
        .collect()
}

/// A held chord of these frequencies.
fn chord(hz: &[f64], seconds: f64) -> Vec<f32> {
    let n = (f64::from(RATE) * seconds) as usize;
    (0..n)
        .map(|i| {
            let t = i as f64 / f64::from(RATE);
            hz.iter()
                .map(|f| (std::f64::consts::TAU * f * t).sin() * 0.2)
                .sum::<f64>() as f32
        })
        .collect()
}

/// A MIDI file: an A minor arpeggio, two bars at 90 BPM (written through
/// `midly`).
fn midi_file(path: &Path) {
    use midly::num::{u4, u7, u15, u24, u28};
    use midly::{
        Format, Header, MetaMessage, MidiMessage, Smf, Timing, TrackEvent, TrackEventKind,
    };
    let ppq = 480u32;
    let mut events = vec![TrackEvent {
        delta: u28::new(0),
        kind: TrackEventKind::Meta(MetaMessage::Tempo(u24::new(666_667))),
    }];
    for bar in 0..2 {
        for (i, k) in [57u8, 60, 64, 69].iter().enumerate() {
            let on = |vel: u8| TrackEventKind::Midi {
                channel: u4::new(0),
                message: if vel > 0 {
                    MidiMessage::NoteOn {
                        key: u7::new(*k),
                        vel: u7::new(vel),
                    }
                } else {
                    MidiMessage::NoteOff {
                        key: u7::new(*k),
                        vel: u7::new(0),
                    }
                },
            };
            let _ = (bar, i);
            events.push(TrackEvent {
                delta: u28::new(0),
                kind: on(100),
            });
            events.push(TrackEvent {
                delta: u28::new(ppq * 9 / 10),
                kind: on(0),
            });
            events.push(TrackEvent {
                delta: u28::new(ppq / 10),
                kind: TrackEventKind::Meta(MetaMessage::Marker(b"")),
            });
        }
    }
    events.push(TrackEvent {
        delta: u28::new(0),
        kind: TrackEventKind::Meta(MetaMessage::EndOfTrack),
    });
    let smf = Smf {
        header: Header::new(Format::SingleTrack, Timing::Metrical(u15::new(ppq as u16))),
        tracks: vec![events],
    };
    let mut out = Vec::new();
    smf.write_std(&mut out).unwrap();
    std::fs::write(path, out).unwrap();
}

/// A folder of sounds: two drum loops (120 BPM, two bars), a D major pad,
/// a short hit, a MIDI file.
fn library(dir: &Path) -> PathBuf {
    let lib = dir.join("Library");
    std::fs::create_dir_all(lib.join("More")).unwrap();
    write(&lib.join("Beat 120.wav"), drum_loop(120.0, 8, 55.0));
    write(&lib.join("Other Beat 120.wav"), drum_loop(120.0, 8, 62.0));
    write(&lib.join("Pad D.wav"), chord(&[293.66, 369.99, 440.0], 4.0));
    write(&lib.join("More/Hit.wav"), drum_loop(120.0, 1, 70.0));
    midi_file(&lib.join("Arp.mid"));
    lib
}

/// A session at 100 BPM in C major whose browser keeps its index in `dir`.
fn session(dir: &Path) -> Session {
    let mut s = Session::new(Project::new("Browse", RATE), None, EngineConfig::default()).unwrap();
    s.set_browser_dir(dir.join("index"));
    s.dispatch(Action::Edit(Command::SetTempo { bpm: 100.0 }))
        .unwrap();
    s.dispatch(Action::Edit(Command::SetKeys {
        keys: vec![KeyChange {
            at: MusicalTime::ZERO,
            key: Key::new(0, Scale::Major),
        }],
    }))
    .unwrap();
    s
}

fn op(s: &mut Session, op: BrowserOp) {
    s.dispatch(Action::Browser(op)).unwrap();
}

fn names(s: &Session) -> Vec<String> {
    s.browser().rows().iter().map(|r| r.name.clone()).collect()
}

fn row<'a>(s: &'a Session, name: &str) -> &'a browser::BrowserRow {
    s.browser().rows().iter().find(|r| r.name == name).unwrap()
}

#[test]
fn files_are_read_analysed_and_kept_until_they_change() {
    let dir = temp("index");
    let lib = library(&dir);
    let mut s = session(&dir);
    op(&mut s, BrowserOp::AddFolder(lib.clone()));
    assert!(s.wait_for_browser(WAIT));
    assert_eq!(
        names(&s),
        vec![
            "More",
            "Arp.mid",
            "Beat 120.wav",
            "Other Beat 120.wav",
            "Pad D.wav"
        ]
    );
    let beat = row(&s, "Beat 120.wav");
    assert_eq!(beat.format, "WAV 24-bit");
    assert_eq!((beat.channels, beat.rate), (2, RATE));
    assert!((beat.seconds.unwrap() - 4.0).abs() < 1e-3);
    assert!((beat.bpm.unwrap() - 120.0).abs() < 0.5, "{:?}", beat.bpm);
    assert!(beat.is_loop && !beat.pending);
    let pad = row(&s, "Pad D.wav");
    let key = pad.key.expect("the pad's key");
    assert_eq!(
        browser::transposition(key, Key::new(2, Scale::Major)),
        0,
        "{}",
        key.name()
    );
    assert!(!pad.is_loop);
    let arp = row(&s, "Arp.mid");
    assert_eq!(arp.item, RowItem::Midi);
    assert!((arp.bpm.unwrap() - 90.0).abs() < 0.01);
    assert_eq!(arp.key.unwrap().root, 9);
    // The index is written; another session reads it and analyses nothing
    // again.
    assert!(dir.join("index/index.json").is_file());
    drop(s);
    let mut again = session(&dir);
    op(
        &mut again,
        BrowserOp::Open(browser::Place::Folder(lib.clone())),
    );
    // (The listing comes from the worker; nothing waits to be analysed.)
    assert!(again.wait_for_browser(WAIT));
    assert!(!row(&again, "Beat 120.wav").pending);
    assert_eq!(again.browser().pending(), (0, 0));
    // A file changed on disk is read again.
    std::thread::sleep(Duration::from_millis(30));
    write(&lib.join("Pad D.wav"), chord(&[293.66, 369.99, 440.0], 2.0));
    op(&mut again, BrowserOp::Refresh);
    assert!(again.wait_for_browser(WAIT));
    assert!((row(&again, "Pad D.wav").seconds.unwrap() - 2.0).abs() < 1e-3);
    // Folders open and the way back up.
    op(&mut again, BrowserOp::Enter(lib.join("More")));
    assert!(again.wait_for_browser(WAIT));
    assert_eq!(names(&again), vec!["..", "Hit.wav"]);
    op(&mut again, BrowserOp::Up);
    assert!(again.wait_for_browser(WAIT));
    assert_eq!(again.browser().folder(), Some(lib.as_path()));
}

#[test]
fn searches_filters_ratings_tags_and_likeness() {
    let dir = temp("filters");
    let lib = library(&dir);
    let mut s = session(&dir);
    op(&mut s, BrowserOp::AddFolder(lib.clone()));
    assert!(s.wait_for_browser(WAIT));
    // A search goes through the folders inside.
    op(&mut s, BrowserOp::Search("hit".into()));
    assert!(s.wait_for_browser(WAIT));
    assert_eq!(names(&s), vec!["Hit.wav"]);
    op(&mut s, BrowserOp::Deep(false));
    assert!(s.wait_for_browser(WAIT));
    assert!(names(&s).is_empty());
    op(&mut s, BrowserOp::Deep(true));
    op(&mut s, BrowserOp::Search(String::new()));
    assert!(s.wait_for_browser(WAIT));
    let filter = |f: BrowserFilter| Action::Browser(BrowserOp::Filter(f));
    s.dispatch(filter(BrowserFilter {
        kind: KindFilter::Loops,
        ..BrowserFilter::default()
    }))
    .unwrap();
    assert!(s.wait_for_browser(WAIT));
    assert_eq!(names(&s), vec!["Beat 120.wav", "Other Beat 120.wav"]);
    s.dispatch(filter(BrowserFilter {
        kind: KindFilter::Midi,
        ..BrowserFilter::default()
    }))
    .unwrap();
    assert!(s.wait_for_browser(WAIT));
    assert_eq!(names(&s), vec!["Arp.mid"]);
    // Keys that fit C major: the A minor arpeggio, not the D major pad.
    s.dispatch(filter(BrowserFilter {
        key: KeyFilter::Project,
        ..BrowserFilter::default()
    }))
    .unwrap();
    assert!(s.wait_for_browser(WAIT));
    assert_eq!(names(&s), vec!["Arp.mid"]);
    s.dispatch(filter(BrowserFilter {
        tempo: TempoFilter::Range(110.0, 130.0),
        ..BrowserFilter::default()
    }))
    .unwrap();
    assert!(s.wait_for_browser(WAIT));
    assert_eq!(names(&s), vec!["Beat 120.wav", "Other Beat 120.wav"]);
    // Stars and tags, kept in the user's file.
    s.dispatch(filter(BrowserFilter::default())).unwrap();
    let pad = lib.join("Pad D.wav");
    op(
        &mut s,
        BrowserOp::Rate {
            paths: vec![pad.clone()],
            stars: 4,
        },
    );
    op(
        &mut s,
        BrowserOp::Tag {
            paths: vec![pad.clone(), lib.join("Arp.mid")],
            tag: "Warm".into(),
            on: true,
        },
    );
    s.dispatch(filter(BrowserFilter {
        rating: 3,
        ..BrowserFilter::default()
    }))
    .unwrap();
    assert!(s.wait_for_browser(WAIT));
    assert_eq!(names(&s), vec!["Pad D.wav"]);
    s.dispatch(filter(BrowserFilter {
        tag: Some("Warm".into()),
        ..BrowserFilter::default()
    }))
    .unwrap();
    assert!(s.wait_for_browser(WAIT));
    assert_eq!(names(&s), vec!["Arp.mid", "Pad D.wav"]);
    s.dispatch(filter(BrowserFilter::default())).unwrap();
    // A search finds tags too; sorting by rating puts the stars first.
    op(&mut s, BrowserOp::Search("warm".into()));
    assert!(s.wait_for_browser(WAIT));
    assert_eq!(names(&s), vec!["Arp.mid", "Pad D.wav"]);
    op(&mut s, BrowserOp::Search(String::new()));
    op(&mut s, BrowserOp::Sort(SortBy::Rating));
    assert!(s.wait_for_browser(WAIT));
    assert_eq!(s.browser().rows()[1].name, "Pad D.wav");
    let user = std::fs::read_to_string(dir.join("index/user.json")).unwrap();
    assert!(user.contains("Warm"), "{user}");
    // Find Similar: the other beat is nearer the beat than the pad is.
    op(&mut s, BrowserOp::FindSimilar(lib.join("Beat 120.wav")));
    assert!(s.wait_for_browser(WAIT));
    let similar = names(&s);
    assert_eq!(
        similar.first().map(String::as_str),
        Some("Other Beat 120.wav"),
        "{similar:?}"
    );
    let pad_at = similar.iter().position(|n| n == "Pad D.wav").unwrap();
    assert!(pad_at > 0);
    assert!(s.browser().rows()[0].likeness.unwrap() > s.browser().rows()[pad_at].likeness.unwrap());
    op(&mut s, BrowserOp::ClearSimilar);
    assert!(s.wait_for_browser(WAIT));
    assert!(names(&s).contains(&"More".to_string()));
}

#[test]
fn previews_are_made_in_the_projects_tempo_and_key() {
    let dir = temp("preview");
    let lib = library(&dir);
    let mut s = session(&dir);
    op(&mut s, BrowserOp::AddFolder(lib.clone()));
    assert!(s.wait_for_browser(WAIT));
    let settings = |sync: bool, key: bool| {
        Action::Browser(BrowserOp::Settings(PreviewSettings {
            sync,
            key,
            looped: true,
            auto: true,
            volume_db: -6.0,
        }))
    };
    // As it is: four seconds.
    s.dispatch(settings(false, false)).unwrap();
    op(&mut s, BrowserOp::Preview(lib.join("Beat 120.wav")));
    assert!(s.wait_for_preview_audio(WAIT));
    let p = s.browser_preview().unwrap();
    assert_eq!(p.frames, Some(i64::from(RATE) * 4));
    assert!(p.fit.is_empty() && !p.synced);
    // In the project's tempo (100 BPM): 4.8 s, looping two bars.
    s.dispatch(settings(true, false)).unwrap();
    op(&mut s, BrowserOp::Preview(lib.join("Beat 120.wav")));
    assert!(s.wait_for_preview_audio(WAIT));
    let p = s.browser_preview().unwrap();
    assert_eq!(p.fit, "120.0 → 100.0 BPM");
    assert!((p.frames.unwrap() - 230_400).abs() <= 1, "{:?}", p.frames);
    assert_eq!(p.loop_frames, 230_400);
    // In the key: the D major pad down a tone to C, its length kept; a
    // selection with Auto on hears it.
    s.dispatch(settings(true, true)).unwrap();
    op(&mut s, BrowserOp::Select(vec![lib.join("Pad D.wav")]));
    assert!(s.wait_for_preview_audio(WAIT));
    let p = s.browser_preview().unwrap();
    assert_eq!(p.name, "Pad D.wav");
    assert_eq!(p.fit, "-2 st");
    assert_eq!(p.frames, Some(i64::from(RATE) * 4));
    op(&mut s, BrowserOp::StopPreview);
    assert!(s.browser_preview().is_none());
    // A MIDI file needs an instrument to play through.
    let err = s.dispatch(Action::Browser(BrowserOp::Preview(lib.join("Arp.mid"))));
    assert!(err.is_err());
    let mut demo = Session::demo(EngineConfig::default()).unwrap();
    demo.set_browser_dir(dir.join("index"));
    demo.dispatch(Action::Browser(BrowserOp::Preview(lib.join("Arp.mid"))))
        .unwrap();
    let p = demo.browser_preview().unwrap();
    assert!(p.midi);
    // Recent files list what was heard, newest first.
    op(&mut s, BrowserOp::Open(browser::Place::Recent));
    assert!(s.wait_for_browser(WAIT));
    assert_eq!(names(&s), vec!["Pad D.wav", "Beat 120.wav"]);
}

#[test]
fn files_placed_from_the_browser_are_matched_to_the_project() {
    let dir = temp("place");
    let lib = library(&dir);
    let mut s = session(&dir);
    op(&mut s, BrowserOp::AddFolder(lib.clone()));
    assert!(s.wait_for_browser(WAIT));
    s.dispatch(Action::Browser(BrowserOp::Settings(PreviewSettings {
        sync: true,
        key: true,
        ..PreviewSettings::default()
    })))
    .unwrap();
    let at = s.project().timeline.meter.bar_start(2);
    let before = s.project().tracks.len();
    op(
        &mut s,
        BrowserOp::Place {
            files: vec![lib.join("Beat 120.wav"), lib.join("Pad D.wav")],
            track: None,
            at,
        },
    );
    assert!(s.wait_for_browser(WAIT));
    assert_eq!(s.project().tracks.len(), before + 2);
    let clip = |name: &str| {
        s.project()
            .clips
            .values()
            .find(|c| c.name.starts_with(name))
            .cloned()
            .unwrap()
    };
    let beat = clip("Beat 120");
    let ClipContent::Audio(a) = &beat.content else {
        panic!("audio");
    };
    assert_eq!(beat.start, at);
    let w = a.warp.as_ref().expect("warped to the project's tempo");
    assert_eq!(w.source_length, 4 * i64::from(RATE));
    assert_eq!(a.length, 230_400, "two bars at 100 BPM");
    assert_eq!(a.stretch.semitones(), 0.0);
    let pad = clip("Pad D");
    let ClipContent::Audio(a) = &pad.content else {
        panic!("audio");
    };
    assert!(a.warp.is_none(), "not a loop: its own time");
    assert_eq!(a.stretch.semitones(), -2.0);
    // One step: undone at once.
    assert_eq!(
        s.history_steps().0.last().map(String::as_str),
        Some("Place Files from Browser")
    );
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(s.project().tracks.len(), before);
}

#[test]
fn a_loop_dropped_in_a_launcher_slot_follows_the_tempo() {
    use faderframe_project::launcher::SlotKey;
    use faderframe_session::launcher::LauncherOp;
    let dir = temp("launcher");
    let lib = library(&dir);
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    s.set_browser_dir(dir.join("index"));
    op(&mut s, BrowserOp::AddFolder(lib.clone()));
    assert!(s.wait_for_browser(WAIT));
    s.dispatch(Action::Launcher(LauncherOp::AddScene { after: None }))
        .unwrap();
    let scene = s.project().launcher.scenes[0].id;
    let drums = s
        .project()
        .tracks
        .iter()
        .find(|t| t.name == "Drums")
        .unwrap()
        .id;
    op(
        &mut s,
        BrowserOp::ToLauncher {
            files: vec![lib.join("Beat 120.wav")],
            track: drums,
            scene,
        },
    );
    assert!(s.wait_for_browser(WAIT));
    let key = SlotKey {
        track: drums,
        scene,
    };
    let id = *s
        .project()
        .launcher
        .slots
        .get(&key)
        .expect("the slot holds it");
    let c = s.project().clips.get(&id).unwrap();
    let ClipContent::Audio(a) = &c.content else {
        panic!("audio");
    };
    assert!(a.warp.is_none(), "the launcher stretches it itself");
    let tempo = s.project().launcher.launch_of(key).tempo.unwrap();
    assert!((tempo - 120.0).abs() < 0.5, "{tempo}");
    assert!(!s.project().clips_of(drums).iter().any(|x| x.id == id));
    assert_eq!(
        s.history_steps().0.last().map(String::as_str),
        Some("Browser to Launcher")
    );
}

/// Self-freezing keeps its caches in an unsaved project's media folder:
/// the browser's Project Media leaves them out.
#[test]
fn project_media_leaves_the_self_freeze_cache_out() {
    let dir = temp("cache");
    let mut s = session(&dir);
    let media = s.media_dir().to_path_buf();
    let cache = media.join(faderframe_session::self_freeze::CACHE_FOLDER);
    std::fs::create_dir_all(&cache).unwrap();
    write(&cache.join("abc.wav"), chord(&[220.0], 0.5));
    write(&media.join("Kept.wav"), chord(&[330.0], 0.5));
    op(&mut s, BrowserOp::Open(browser::Place::ProjectMedia));
    assert!(s.wait_for_browser(WAIT));
    let shown = names(&s);
    assert!(shown.iter().any(|n| n.starts_with("Kept")), "{shown:?}");
    assert!(
        !shown.iter().any(|n| n == "Cache" || n.starts_with("abc")),
        "{shown:?}"
    );
    // The session's scratch media folder: take back what was put there.
    let _ = std::fs::remove_dir_all(&cache);
    let _ = std::fs::remove_file(media.join("Kept.wav"));
    let _ = std::fs::remove_dir_all(&dir);
}
