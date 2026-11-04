//! Scripts and macros against a session: the API reads and edits the
//! project as one undo step named after the script, a failing script
//! leaves the project as it was and says where it failed, the sandbox
//! refuses what scripts may not do, prompts wait for their answer (the
//! rest joining the same step), timers call back from the tick, the
//! console keeps its variables, and a recorded macro replays as one step
//! — also from a MIDI controller.
#![allow(clippy::unwrap_used)]

use faderframe_core::TrackId;
use faderframe_engine::EngineConfig;
use faderframe_project::{Command, MappingTarget, Project, TrackKind};
use faderframe_session::script::{self, ConsoleKind, FieldKind, ScriptOp};
use faderframe_session::{Action, Session, TransportAction, UiRequest};
use faderframe_timeline::MusicalTime;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, OnceLock};

/// The scripts folder is the process's: one test at a time.
static TURN: Mutex<()> = Mutex::new(());

fn turn() -> MutexGuard<'static, ()> {
    let g = TURN.lock().unwrap_or_else(|e| e.into_inner());
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    let dir = DIR.get_or_init(|| {
        let d = std::env::temp_dir().join(format!("faderframe-scripts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    });
    script::set_user_dir(Some(dir.clone()));
    g
}

fn session() -> Session {
    Session::new(
        Project::new("Scripts", 48_000),
        None,
        EngineConfig::default(),
    )
    .unwrap()
}

fn run(s: &mut Session, name: &str, source: &str) -> faderframe_session::Result<()> {
    s.dispatch(Action::Script(ScriptOp::RunSource {
        name: name.into(),
        source: source.into(),
    }))
}

fn track_named(s: &Session, name: &str) -> Option<TrackId> {
    s.project()
        .tracks
        .iter()
        .find(|t| t.name == name)
        .map(|t| t.id)
}

fn undo_labels(s: &Session) -> Vec<String> {
    s.history_steps().0
}

#[test]
fn a_script_reads_the_project_and_edits_it_in_one_undo_step() {
    let _t = turn();
    let mut s = session();
    let tracks_before = s.project().tracks.len();
    run(
        &mut s,
        "Build",
        r##"
        local p = ff.project()
        assert(p.rate == 48000 and p.signature == "4/4")
        local drums = ff.add_track{ kind = "instrument", name = "Drums" }
        ff.set_track(drums, { volume = -6, pan = -0.25, mute = true, color = "#336699" })
        local clip = ff.add_midi_clip(drums, ff.bar(2), 4, "Beat")
        local ids = ff.add_notes(clip, {
            { key = 36, start = 0, length = 0.5, velocity = 120 },
            { key = 38, start = 1, length = 0.5 },
            { key = 42, start = 1.5, length = 0.25, channel = 10 },
        })
        assert(#ids == 3)
        ff.set_note(clip, ids[2], { velocity = 64 })
        ff.add_marker(ff.bar(3), "Verse")
        ff.add_section(0, 16, "Intro")
        ff.set_tempo(96)
        ff.set_loop(4, 8)
        local seen = ff.track("Drums")
        assert(seen.volume == -6 and seen.mute and seen.kind == "instrument")
        assert(#ff.notes(clip) == 3 and ff.notes(clip)[2].velocity == 64)
        print("made", seen.name)
        "##,
    )
    .unwrap();
    let drums = track_named(&s, "Drums").unwrap();
    let t = s.project().track(drums).unwrap();
    assert_eq!(t.volume_db, -6.0);
    assert!(t.mute);
    assert_eq!(t.color.to_hex(), "#336699");
    let clips = s.project().clips_of(drums);
    assert_eq!(clips.len(), 1);
    assert_eq!(clips[0].start, MusicalTime::from_quarters(4.0));
    assert_eq!(clips[0].as_midi().unwrap().notes.len(), 3);
    assert!(s.project().markers.iter().any(|m| m.name == "Verse"));
    assert!(s.project().sections.iter().any(|x| x.name == "Intro"));
    assert_eq!(s.project().timeline.tempo.bpm_at(MusicalTime::ZERO), 96.0);
    // One step, named after the script.
    assert_eq!(undo_labels(&s), vec!["Build".to_string()]);
    assert!(
        s.script_console()
            .any(|l| l.kind == ConsoleKind::Output && l.text == "made\tDrums")
    );
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(s.project().tracks.len(), tracks_before);
    assert!(s.project().markers.is_empty());
    assert_eq!(s.project().timeline.tempo.bpm_at(MusicalTime::ZERO), 120.0);
}

#[test]
fn a_failing_script_leaves_the_project_as_it_was_and_says_where() {
    let _t = turn();
    let mut s = session();
    let before = s.project().tracks.len();
    let e = run(
        &mut s,
        "Broken",
        "ff.add_track('audio')\nff.add_marker(4)\nff.set_track('No Such Track', { mute = true })\n",
    )
    .unwrap_err()
    .to_string();
    assert!(e.contains("Broken.lua:3:"), "{e}");
    assert!(e.contains("no track named 'No Such Track'"), "{e}");
    assert_eq!(s.project().tracks.len(), before);
    assert!(s.project().markers.is_empty());
    assert!(undo_labels(&s).is_empty());
    assert!(
        s.script_console()
            .any(|l| l.kind == ConsoleKind::Error && l.text.contains("Broken.lua:3:"))
    );
    // A syntax error names its line too.
    let e = run(&mut s, "Typo", "local a = 1\nlocal = 2\n")
        .unwrap_err()
        .to_string();
    assert!(e.contains("Typo.lua:2:"), "{e}");
    // A runtime error of Lua's own.
    let e = run(&mut s, "Nil", "\n\nlocal t = nil\nprint(t.x)\n")
        .unwrap_err()
        .to_string();
    assert!(
        e.contains("Nil.lua:4:") && e.contains("attempt to index"),
        "{e}"
    );
}

#[test]
fn the_sandbox_refuses_what_scripts_may_not_do() {
    let _t = turn();
    let mut s = session();
    for (code, line) in [
        ("os.execute('touch /tmp/x')", 1),
        ("\nio.popen('ls')", 2),
        ("\n\nio.open('/etc/passwd')", 3),
        ("require('../../etc/passwd')", 1),
        ("ff.play = nil", 1),
    ] {
        let e = run(&mut s, "Bad", code).unwrap_err().to_string();
        assert!(e.contains(&format!("Bad.lua:{line}:")), "{code}: {e}");
    }
    // The scripts folder is open to them.
    run(
        &mut s,
        "Notes",
        "local f = assert(io.open('notes.txt', 'w')) f:write('hi') f:close()",
    )
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(script::user_dir().join("notes.txt")).unwrap(),
        "hi"
    );
    // A run over its time is stopped (and its edits taken back).
    s.set_script_limits(script::Limits {
        time: std::time::Duration::from_millis(200),
        ..script::Limits::default()
    })
    .unwrap();
    let e = run(&mut s, "Forever", "ff.add_marker(1)\nwhile true do end")
        .unwrap_err()
        .to_string();
    assert!(e.contains("time limit"), "{e}");
    assert!(s.project().markers.is_empty());
    s.set_script_limits(script::Limits::default()).unwrap();
}

fn take_prompt(s: &mut Session) -> script::Prompt {
    s.take_ui_requests()
        .into_iter()
        .find_map(|r| match r {
            UiRequest::ScriptPrompt(p) => Some(p),
            _ => None,
        })
        .expect("a prompt")
}

#[test]
fn a_prompt_waits_for_its_answer_and_the_rest_joins_the_step() {
    let _t = turn();
    let mut s = session();
    run(
        &mut s,
        "Ask",
        r##"
        local t = ff.add_track("audio")
        local a = ff.prompt{
            title = "Name the track",
            fields = {
                { label = "Name", default = "Vox" },
                { label = "Level", default = -3 },
                { label = "Muted", kind = "check" },
                { label = "Colour", choices = { "#aa0000", "#00aa00" } },
            },
        }
        if a then
            ff.set_track(t, { name = a.Name, volume = a.Level, mute = a.Muted, color = a[4] })
        end
        "##,
    )
    .unwrap();
    let p = take_prompt(&mut s);
    assert_eq!(p.script, "Ask");
    assert_eq!(p.title, "Name the track");
    assert_eq!(p.fields.len(), 4);
    assert_eq!(p.fields[1].kind, FieldKind::Number);
    assert_eq!(p.fields[2].kind, FieldKind::Check);
    assert_eq!(p.fields[3].default, "#aa0000");
    assert_eq!(s.scripts_waiting(), vec!["Ask".to_string()]);
    s.dispatch(Action::Script(ScriptOp::Answer {
        prompt: p.id,
        values: Some(vec![
            "Lead".into(),
            "-4.5".into(),
            "true".into(),
            "#00aa00".into(),
        ]),
    }))
    .unwrap();
    let t = s.project().track(track_named(&s, "Lead").unwrap()).unwrap();
    assert_eq!(t.volume_db, -4.5);
    assert!(t.mute);
    assert_eq!(t.color.to_hex(), "#00aa00");
    assert!(s.scripts_waiting().is_empty());
    assert_eq!(undo_labels(&s), vec!["Ask".to_string()], "one step");
    // Cancelled: the prompt returns nil.
    run(
        &mut s,
        "Ask Again",
        "local a = ff.prompt('Name?', 'x') ff.add_marker(1, a == nil and 'cancelled' or a[1])",
    )
    .unwrap();
    let p = take_prompt(&mut s);
    s.dispatch(Action::Script(ScriptOp::Answer {
        prompt: p.id,
        values: None,
    }))
    .unwrap();
    assert!(s.project().markers.iter().any(|m| m.name == "cancelled"));
    // Something done in between: the rest is a step of its own.
    run(
        &mut s,
        "Third",
        "ff.add_marker(2) local a = ff.prompt('?') ff.add_marker(3)",
    )
    .unwrap();
    let p = take_prompt(&mut s);
    let t = s.add_track(TrackKind::Bus).unwrap();
    s.dispatch(Action::Edit(Command::SetTrackMute { track: t, on: true }))
        .unwrap();
    s.dispatch(Action::Script(ScriptOp::Answer {
        prompt: p.id,
        values: Some(vec!["ok".into()]),
    }))
    .unwrap();
    let labels = undo_labels(&s);
    assert_eq!(labels.last().map(String::as_str), Some("Third"));
    assert_eq!(
        labels.iter().filter(|l| *l == "Third").count(),
        2,
        "{labels:?}"
    );
}

#[test]
fn timers_call_back_from_the_tick() {
    let _t = turn();
    let mut s = session();
    run(
        &mut s,
        "Clock",
        r#"
        ff.after(0, function() ff.add_marker(1, "later") end)
        local n = 0
        ff.every(0, function()
            n = n + 1
            ff.add_marker(n + 1, "tick " .. n)
            if n == 3 then return false end
        end)
        "#,
    )
    .unwrap();
    assert!(s.project().markers.is_empty(), "nothing yet");
    for _ in 0..20 {
        s.tick(0.0);
        std::thread::sleep(std::time::Duration::from_millis(60));
    }
    let names: Vec<String> = s.project().markers.iter().map(|m| m.name.clone()).collect();
    assert!(names.contains(&"later".to_string()), "{names:?}");
    assert_eq!(
        names.iter().filter(|n| n.starts_with("tick")).count(),
        3,
        "{names:?}"
    );
    assert_eq!(s.script_timers(), 0);
    // Each call was a step named after the script.
    assert!(undo_labels(&s).iter().all(|l| l == "Clock"));
    // A timer can be cancelled, and Stop All clears them.
    run(
        &mut s,
        "Clock 2",
        "local id = ff.after(10, function() end) ff.cancel(id) ff.every(5, function() end)",
    )
    .unwrap();
    assert_eq!(s.script_timers(), 1);
    s.dispatch(Action::Script(ScriptOp::StopAll)).unwrap();
    assert_eq!(s.script_timers(), 0);
}

#[test]
fn the_console_keeps_its_variables_and_prints_results() {
    let _t = turn();
    let mut s = session();
    let line = |s: &mut Session, l: &str| s.dispatch(Action::Script(ScriptOp::Console(l.into())));
    line(&mut s, "x = 20").unwrap();
    line(&mut s, "x + 1").unwrap();
    line(&mut s, "ff.add_marker(4, 'here')").unwrap();
    line(&mut s, "{ a = 1, 'one' }").unwrap();
    let results: Vec<String> = s
        .script_console()
        .filter(|l| l.kind == ConsoleKind::Result)
        .map(|l| l.text.clone())
        .collect();
    assert_eq!(results[0], "21");
    assert!(
        results.iter().any(|r| r == "{ \"one\", a = 1 }"),
        "{results:?}"
    );
    assert_eq!(undo_labels(&s), vec![script::CONSOLE_LABEL.to_string()]);
    assert!(line(&mut s, "nope(").is_err());
    assert!(
        s.script_console()
            .any(|l| l.kind == ConsoleKind::Error && l.text.contains("console:1:"))
    );
}

#[test]
fn a_recorded_macro_replays_as_one_step() {
    let _t = turn();
    let mut s = session();
    let master = s.project().master_id().unwrap();
    s.dispatch(Action::Script(ScriptOp::RecordMacro)).unwrap();
    assert!(s.macro_recording().is_some());
    s.dispatch(Action::AddTrack(TrackKind::Audio)).unwrap();
    let made = *s.selection.tracks.iter().next().unwrap();
    // A fader drag: one line, the last value.
    s.dispatch(Action::BeginGesture("Volume".into())).unwrap();
    for db in [-1.0, -2.0, -3.5] {
        s.dispatch(Action::Edit(Command::SetTrackVolume { track: made, db }))
            .unwrap();
    }
    s.dispatch(Action::EndGesture).unwrap();
    // A cancelled drag: nothing.
    s.dispatch(Action::BeginGesture("Pan".into())).unwrap();
    s.dispatch(Action::Edit(Command::SetTrackPan {
        track: made,
        pan: 0.5,
    }))
    .unwrap();
    s.dispatch(Action::CancelGesture).unwrap();
    s.dispatch(Action::AddMarker(MusicalTime::from_quarters(8.0)))
        .unwrap();
    s.dispatch(Action::Edit(Command::SetTrackMute {
        track: master,
        on: true,
    }))
    .unwrap();
    s.dispatch(Action::Transport(TransportAction::Locate(
        MusicalTime::from_quarters(4.0),
    )))
    .unwrap();
    // Only the view: left out silently.
    s.dispatch(Action::Zoom(faderframe_session::ZoomRequest::In))
        .unwrap();
    let status = s.macro_recording().unwrap();
    assert_eq!(status.steps, 5, "{status:?}");
    s.dispatch(Action::Script(ScriptOp::StopRecording { keep: true }))
        .unwrap();
    assert!(s.macro_recording().is_none());
    let entry = s
        .script_library()
        .iter()
        .find(|e| e.origin == script::ScriptOrigin::Macro)
        .cloned()
        .unwrap();
    let number = entry.number.unwrap();
    let source = std::fs::read_to_string(&entry.path).unwrap();
    assert!(
        source.contains("local t1 = ff.add_track{ kind = \"audio\" }"),
        "{source}"
    );
    assert!(
        source.contains("ff.set_track(t1, { volume = -3.5 })"),
        "{source}"
    );
    assert!(!source.contains("pan"), "{source}");
    assert!(
        source.contains("ff.set_track(\"Master\", { mute = true })"),
        "{source}"
    );
    // Replayed on a fresh project: one step, named after the macro.
    let mut fresh = session();
    fresh.dispatch(Action::Script(ScriptOp::Rescan)).unwrap();
    fresh
        .dispatch(Action::Script(ScriptOp::RunMacro(number)))
        .unwrap();
    let audio: Vec<_> = fresh
        .project()
        .tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Audio)
        .collect();
    assert_eq!(audio.len(), 1);
    assert_eq!(audio[0].volume_db, -3.5);
    assert_eq!(fresh.project().markers.len(), 1);
    assert!(fresh.project().master().unwrap().mute);
    assert_eq!(undo_labels(&fresh), vec![entry.name.clone()]);
    // Renamed, it keeps its number; MIDI learn names it.
    fresh
        .dispatch(Action::Script(ScriptOp::RenameMacro {
            number,
            name: "Set Up a Take".into(),
        }))
        .unwrap();
    assert_eq!(
        fresh.macro_label(number),
        format!("Macro {number} · Set Up a Take")
    );
    // A controller's button runs it.
    fresh
        .dispatch(Action::MidiLearn(MappingTarget::RunMacro { number }))
        .unwrap();
    fresh.midi_keyboard().send(&[0x90, 40, 100]);
    fresh.tick(0.0);
    assert!(fresh.midi_learning().is_none());
    fresh.midi_keyboard().send(&[0x90, 40, 100]);
    fresh.tick(0.0);
    let audio = fresh
        .project()
        .tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Audio)
        .count();
    assert_eq!(audio, 2, "the button ran it again");
    fresh
        .dispatch(Action::Script(ScriptOp::DeleteMacro(number)))
        .unwrap();
    assert!(
        !entry.path.exists()
            || fresh
                .script_library()
                .iter()
                .all(|e| e.number != Some(number))
    );
}

#[test]
fn scripts_from_the_library_and_scripts_calling_scripts() {
    let _t = turn();
    let dir = script::user_dir();
    std::fs::write(
        dir.join("helper.lua"),
        "--! name: Helper\n--! macro: 900\nff.add_marker(1, 'helped')\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("lib")).unwrap();
    std::fs::write(dir.join("lib/names.lua"), "return { drums = 'Drums' }\n").unwrap();
    let mut s = session();
    s.dispatch(Action::Script(ScriptOp::Rescan)).unwrap();
    let helper = s
        .script_library()
        .iter()
        .find(|e| e.name == "Helper")
        .cloned()
        .unwrap();
    assert_eq!(helper.key, "user/helper.lua");
    s.dispatch(Action::Script(ScriptOp::Run(helper.key.clone())))
        .unwrap();
    assert_eq!(undo_labels(&s), vec!["Helper".to_string()]);
    run(
        &mut s,
        "Caller",
        "local names = require('lib.names') ff.add_track{ kind = 'audio', name = names.drums } ff.run('Helper')",
    )
    .unwrap();
    assert!(track_named(&s, "Drums").is_some());
    assert_eq!(s.project().markers.len(), 2);
    assert_eq!(undo_labels(&s).last().map(String::as_str), Some("Caller"));
    // A file run by path: its folder is open to it.
    let file = dir.join("by-path.lua");
    std::fs::write(&file, "ff.set_tempo(100)\n").unwrap();
    s.dispatch(Action::Script(ScriptOp::RunFile(file))).unwrap();
    assert_eq!(s.project().timeline.tempo.bpm_at(MusicalTime::ZERO), 100.0);
    let _ = std::fs::remove_file(dir.join("helper.lua"));
}
