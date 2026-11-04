//! Scripts and macros: Lua 5.4 over the session (docs/SCRIPTING.md is
//! the API's reference).
//!
//! The session keeps one sandboxed Lua state ([`faderframe_script`]) and
//! puts the `ff` table into it ([`api`]). A script runs as a coroutine on
//! the UI thread, never the audio thread:
//!
//! * One run = one undo step named after the script: the run is a
//!   gesture (`BeginGesture`/`EndGesture`). Every edit goes through the
//!   session's actions and commands, each worked out from the project as
//!   it is then (drag bases and ripple marks are forgotten between them:
//!   [`History::unmark`](faderframe_project::History::unmark)). A run that
//!   fails takes its edits back (the gesture is cancelled) and says where
//!   it failed (script and line) in the console and the status bar.
//! * `ff.prompt` asks through [`UiRequest::ScriptPrompt`]: the coroutine
//!   waits (its step so far committed) until
//!   [`ScriptOp::Answer`] resumes it; what it does then joins the same
//!   step while nothing else was done in between
//!   ([`History::amend_on_end`](faderframe_project::History::amend_on_end)).
//! * Timers (`ff.after`, `ff.every`) call back from [`Session::tick`],
//!   each call a run of its own (a step of its own when it edits), never
//!   while a gesture is open or another script runs.
//! * Scripts never start scripts while running (`ff.run` calls one inside
//!   the run instead); MIDI controllers, surfaces and timers wait.
//! * The console ([`ScriptOp::Console`]) runs lines in an environment
//!   that lasts; what a line returns is printed.
//! * Macros ([`record`]): while recording, every action the user
//!   dispatches that a script can do is written as Lua; stopping keeps
//!   the macro as a numbered script in the macros folder. Replaying it is
//!   running it: one undo step.

mod api;
pub mod library;
mod record;

pub use faderframe_script::{ErrorKind, Limits, ScriptError};
pub use library::{PROJECT_FOLDER, ScriptEntry, ScriptOrigin, macro_dir, set_user_dir, user_dir};

use crate::{Action, NoticeLevel, Result, Session, SessionError, UiRequest};
use faderframe_script::mlua::{Function, MultiValue, Table, Thread, Value};
use faderframe_script::{Folders, Resumed, Sandbox};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// What the Scripts menu, the console, the macro recorder, controllers
/// and prompts ask of the session.
#[derive(Clone, Debug, PartialEq)]
pub enum ScriptOp {
    /// Run a Lua file (anywhere; its folder joins the ones it may use).
    RunFile(PathBuf),
    /// Run the library's script with this key ([`ScriptEntry::key`]).
    Run(String),
    /// Run the macro (or script) with this number.
    RunMacro(u16),
    /// Run Lua source under a name (the name labels the undo step and its
    /// errors).
    RunSource {
        name: String,
        source: String,
    },
    /// A line typed into the console.
    Console(String),
    ClearConsole,
    /// The answer to prompt `prompt` (`None`: cancelled; the script's
    /// `ff.prompt` returns nil).
    Answer {
        prompt: u64,
        values: Option<Vec<String>>,
    },
    /// Stop every timer and every script waiting for an answer.
    StopAll,
    /// Start recording a macro.
    RecordMacro,
    /// Stop recording: keep the macro (`keep`) or throw it away.
    StopRecording {
        keep: bool,
    },
    /// Give a macro (or numbered script) another name.
    RenameMacro {
        number: u16,
        name: String,
    },
    /// Delete a recorded macro's file.
    DeleteMacro(u16),
    /// Read the scripts folders again.
    Rescan,
}

/// A question a script asks: the shell shows it and answers with
/// [`ScriptOp::Answer`].
#[derive(Clone, Debug, PartialEq)]
pub struct Prompt {
    pub id: u64,
    /// The script asking.
    pub script: String,
    pub title: String,
    /// More words under the title.
    pub text: Option<String>,
    pub fields: Vec<PromptField>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PromptField {
    pub label: String,
    pub kind: FieldKind,
    /// As text (a check: "true" or "false"; a choice: one of them).
    pub default: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum FieldKind {
    Text,
    Number,
    Check,
    Choice(Vec<String>),
}

/// A line of the console.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConsoleLine {
    pub kind: ConsoleKind,
    pub text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConsoleKind {
    /// What was typed.
    Input,
    /// What scripts printed.
    Output,
    /// What a console line returned.
    Result,
    Error,
    /// What the session says (macro saved, timer stopped).
    Info,
}

/// The macro being recorded, as the shell shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct MacroRecording {
    /// Lines written so far.
    pub steps: usize,
    /// What could not be written (names of the actions left out).
    pub skipped: Vec<String>,
    pub seconds: f64,
}

/// A script's render, as `ff.render_status` reports it.
#[derive(Clone, Debug, PartialEq)]
pub struct RenderOutcome {
    pub done: bool,
    pub progress: f64,
    pub files: Vec<PathBuf>,
    pub error: Option<String>,
}

/// Lines the console keeps.
const CONSOLE_KEPT: usize = 2000;
/// Timers a session keeps at most.
pub const MAX_TIMERS: usize = 64;
/// Calls of timers in one tick at most.
const TIMERS_A_TICK: usize = 8;
/// The shortest timer interval.
pub const MIN_INTERVAL: Duration = Duration::from_millis(50);
/// How often the scripts folders are read again.
const RESCAN: Duration = Duration::from_secs(2);
/// The console's undo steps.
pub const CONSOLE_LABEL: &str = "Console";

/// A script waiting for a prompt's answer.
struct Waiting {
    id: u64,
    label: String,
    thread: Thread,
    /// The history's revision right after the step its first part made
    /// (`None`: it made none, so nothing to join).
    token: Option<u64>,
    console: bool,
    fields: Vec<PromptField>,
}

struct Timer {
    id: u64,
    label: String,
    due: Instant,
    every: Option<Duration>,
    function: Function,
}

struct ScriptRender {
    id: u64,
    label: String,
    job: Option<crate::render::RenderJob>,
    outcome: RenderOutcome,
}

/// The session's scripting state.
#[derive(Default)]
pub struct ScriptState {
    sandbox: Option<Arc<Sandbox>>,
    limits: Limits,
    /// The run under way (its label).
    running: Option<String>,
    /// A prompt `ff.prompt` asked for in the run under way.
    pending_prompt: Option<Prompt>,
    waiting: Vec<Waiting>,
    timers: Vec<Timer>,
    next_id: u64,
    console: VecDeque<ConsoleLine>,
    console_env: Option<Table>,
    console_revision: u64,
    library: Vec<ScriptEntry>,
    library_revision: u64,
    pub(crate) scanned: Option<Instant>,
    recorder: Option<record::Recorder>,
    /// A dispatch the recorder is hearing is under way (its own nested
    /// dispatches are not heard).
    hearing: bool,
    renders: Vec<ScriptRender>,
    /// Folders a script run from a file may use besides the usual ones.
    extra_folders: Vec<PathBuf>,
    /// Answers that came while a drag was under way (given once it ends).
    answers: Vec<(u64, Option<Vec<String>>)>,
}

impl ScriptState {
    fn fresh_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }
}

/// How a run went.
enum Done {
    Finished(MultiValue),
    Waiting,
}

/// A run to drive.
struct Drive {
    label: String,
    thread: Thread,
    args: MultiValue,
    amend: Option<u64>,
    console: bool,
}

fn other(e: impl std::fmt::Display) -> SessionError {
    SessionError::Other(e.to_string())
}

impl Session {
    // --- reading -------------------------------------------------------

    /// The scripts of the library (user, project, macros), by name.
    pub fn script_library(&self) -> &[ScriptEntry] {
        &self.scripts.library
    }

    /// Changes whenever the library does.
    pub fn script_library_revision(&self) -> u64 {
        self.scripts.library_revision
    }

    /// The console's lines, oldest first.
    pub fn script_console(&self) -> impl Iterator<Item = &ConsoleLine> {
        self.scripts.console.iter()
    }

    /// Changes whenever a console line comes.
    pub fn script_console_revision(&self) -> u64 {
        self.scripts.console_revision
    }

    /// The macro being recorded, if one is.
    pub fn macro_recording(&self) -> Option<MacroRecording> {
        self.scripts.recorder.as_ref().map(record::Recorder::status)
    }

    /// The script running now (only ever seen from its own calls).
    pub fn script_running(&self) -> Option<&str> {
        self.scripts.running.as_deref()
    }

    /// Scripts waiting for a prompt's answer (their names).
    pub fn scripts_waiting(&self) -> Vec<String> {
        self.scripts
            .waiting
            .iter()
            .map(|w| w.label.clone())
            .collect()
    }

    /// Timers scripts set (how many).
    pub fn script_timers(&self) -> usize {
        self.scripts.timers.len()
    }

    pub fn script_limits(&self) -> Limits {
        self.scripts.limits
    }

    /// How long a run may compute and how much memory scripts may hold.
    pub fn set_script_limits(&mut self, limits: Limits) -> Result<()> {
        self.scripts.limits = limits;
        if let Some(sb) = &self.scripts.sandbox {
            sb.set_limits(limits).map_err(other)?;
        }
        Ok(())
    }

    /// "Macro 3 · Fade Out" (MIDI learn and surfaces).
    pub fn macro_label(&self, number: u16) -> String {
        match self
            .scripts
            .library
            .iter()
            .find(|e| e.number == Some(number))
        {
            Some(e) => format!("Macro {number} · {}", e.name),
            None => format!("Macro {number}"),
        }
    }

    /// The library entry with this key or number.
    pub fn script_entry(&self, key: &str) -> Option<&ScriptEntry> {
        self.scripts.library.iter().find(|e| e.key == key)
    }

    // --- ops -----------------------------------------------------------

    pub(crate) fn script_op(&mut self, op: ScriptOp) -> Result<()> {
        match op {
            ScriptOp::RunFile(path) => {
                let source = std::fs::read_to_string(&path)
                    .map_err(|e| other(format!("{}: {e}", path.display())))?;
                let info = faderframe_script::read_header(&source);
                let file = path
                    .file_name()
                    .map(|f| f.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "script.lua".into());
                let label = info
                    .name
                    .unwrap_or_else(|| file.trim_end_matches(".lua").to_string());
                self.scripts.extra_folders = path
                    .parent()
                    .map(|p| vec![p.to_path_buf()])
                    .unwrap_or_default();
                let r = self.run_source(&label, &file, &source);
                self.scripts.extra_folders.clear();
                r
            }
            ScriptOp::Run(key) => {
                self.rescan_scripts(false);
                let entry = self
                    .script_entry(&key)
                    .cloned()
                    .ok_or_else(|| other(format!("no script {key}")))?;
                self.run_entry(&entry)
            }
            ScriptOp::RunMacro(n) => {
                self.rescan_scripts(false);
                let entry = self
                    .scripts
                    .library
                    .iter()
                    .filter(|e| e.number == Some(n))
                    .min_by_key(|e| e.origin != ScriptOrigin::Macro)
                    .cloned()
                    .ok_or_else(|| other(format!("no macro {n}")))?;
                self.run_entry(&entry)
            }
            ScriptOp::RunSource { name, source } => {
                let chunk = format!("{name}.lua");
                self.run_source(&name, &chunk, &source)
            }
            ScriptOp::Console(line) => self.console_line(&line),
            ScriptOp::ClearConsole => {
                self.scripts.console.clear();
                self.scripts.console_revision += 1;
                Ok(())
            }
            ScriptOp::Answer { prompt, values } => self.answer_prompt(prompt, values),
            ScriptOp::StopAll => {
                let timers = self.scripts.timers.len();
                let waiting = self.scripts.waiting.len();
                self.scripts.timers.clear();
                self.scripts.waiting.clear();
                self.scripts.answers.clear();
                for r in &mut self.scripts.renders {
                    if let Some(j) = &r.job {
                        j.cancel();
                    }
                }
                self.console_say(
                    ConsoleKind::Info,
                    format!("stopped {timers} timer(s) and {waiting} waiting script(s)"),
                );
                Ok(())
            }
            ScriptOp::RecordMacro => {
                if self.scripts.recorder.is_none() {
                    self.scripts.recorder = Some(record::Recorder::new());
                    self.console_say(ConsoleKind::Info, "recording a macro".to_string());
                    self.revision += 1;
                }
                Ok(())
            }
            ScriptOp::StopRecording { keep } => self.stop_macro(keep),
            ScriptOp::RenameMacro { number, name } => {
                let name = name.trim().to_string();
                if name.is_empty() {
                    return Err(other("a macro needs a name"));
                }
                let entry = self
                    .scripts
                    .library
                    .iter()
                    .find(|e| e.number == Some(number))
                    .cloned()
                    .ok_or_else(|| other(format!("no macro {number}")))?;
                library::rename(&entry, &name).map_err(other)?;
                self.rescan_scripts(true);
                Ok(())
            }
            ScriptOp::DeleteMacro(number) => {
                let entry = self
                    .scripts
                    .library
                    .iter()
                    .find(|e| e.number == Some(number) && e.origin == ScriptOrigin::Macro)
                    .cloned()
                    .ok_or_else(|| other(format!("no recorded macro {number}")))?;
                std::fs::remove_file(&entry.path).map_err(other)?;
                self.rescan_scripts(true);
                Ok(())
            }
            ScriptOp::Rescan => {
                self.rescan_scripts(true);
                Ok(())
            }
        }
    }

    /// Read the scripts folders again (`now`: whatever the time since the
    /// last look).
    pub(crate) fn rescan_scripts(&mut self, now: bool) {
        if !now && self.scripts.scanned.is_some_and(|t| t.elapsed() < RESCAN) {
            return;
        }
        self.scripts.scanned = Some(Instant::now());
        let found = library::scan(self.project_dir().as_deref());
        if found != self.scripts.library {
            self.scripts.library = found;
            self.scripts.library_revision += 1;
            self.revision += 1;
        }
    }

    fn run_entry(&mut self, entry: &ScriptEntry) -> Result<()> {
        let source = std::fs::read_to_string(&entry.path)
            .map_err(|e| other(format!("{}: {e}", entry.path.display())))?;
        let file = entry
            .path
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_else(|| "script.lua".into());
        self.run_source(&entry.name, &file, &source)
    }

    /// The sandbox, made on first use with the `ff` table in it.
    fn script_sandbox(&mut self) -> Result<Arc<Sandbox>> {
        if let Some(sb) = &self.scripts.sandbox {
            return Ok(Arc::clone(sb));
        }
        let sb = Sandbox::new(self.scripts.limits).map_err(other)?;
        api::install(&sb).map_err(other)?;
        let sb = Arc::new(sb);
        self.scripts.sandbox = Some(Arc::clone(&sb));
        Ok(sb)
    }

    /// The folders scripts reach now: the project's (once saved), the
    /// user's scripts folder, a file's own folder.
    fn script_folders(&self) -> Folders {
        let mut files = Vec::new();
        let mut modules = Vec::new();
        if let Some(p) = self.project_dir() {
            files.push(p.clone());
            modules.push(p.join(library::PROJECT_FOLDER));
        }
        let user = user_dir();
        let _ = std::fs::create_dir_all(&user);
        files.push(user.clone());
        modules.push(user);
        for f in &self.scripts.extra_folders {
            files.push(f.clone());
            modules.push(f.clone());
        }
        Folders { files, modules }
    }

    /// Compile and run `source` as `label` (`chunk` names it in errors).
    pub(crate) fn run_source(&mut self, label: &str, chunk: &str, source: &str) -> Result<()> {
        self.refuse_nested()?;
        let sb = self.script_sandbox()?;
        let env = sb.environment().map_err(other)?;
        let f = match sb.compile(source, chunk, &env) {
            Ok(f) => f,
            Err(e) => return Err(self.script_failed(label, &e)),
        };
        let thread = sb.lua().create_thread(f).map_err(other)?;
        self.drive(Drive {
            label: label.to_string(),
            thread,
            args: MultiValue::new(),
            amend: None,
            console: false,
        })
        .map(|_| ())
    }

    fn refuse_nested(&self) -> Result<()> {
        if let Some(r) = &self.scripts.running {
            return Err(other(format!(
                "“{r}” is running: a script cannot start another one (use ff.run)"
            )));
        }
        if self.history.in_gesture() {
            return Err(other(
                "an edit is under way (a drag): scripts run once it ends",
            ));
        }
        Ok(())
    }

    /// Run (or resume) a script's coroutine as one undo step.
    fn drive(&mut self, d: Drive) -> Result<Done> {
        self.refuse_nested()?;
        let sb = self.script_sandbox()?;
        sb.set_folders(self.script_folders());
        // Running from here on: the macro recorder hears none of it.
        self.scripts.running = Some(d.label.clone());
        let r = self.drive_running(d, &sb);
        self.scripts.running = None;
        r
    }

    fn drive_running(&mut self, d: Drive, sb: &Arc<Sandbox>) -> Result<Done> {
        let revision = self.history.revision();
        self.dispatch(Action::BeginGesture(d.label.clone()))?;
        if let Some(t) = d.amend {
            self.history.amend_on_end(t);
        }
        self.scripts.pending_prompt = None;
        let lua = sb.lua().clone();
        let thread = d.thread.clone();
        let args = d.args;
        let outcome = lua.scope(|scope| {
            let ud = scope.create_any_userdata_ref_mut(&mut *self)?;
            lua.set_named_registry_value(api::SESSION_KEY, ud)?;
            Ok(sb.resume(&thread, args))
        });
        let _ = lua.unset_named_registry_value(api::SESSION_KEY);
        self.take_script_output(sb);
        let outcome = outcome.unwrap_or_else(|e| Err(ScriptError::from_lua(&e)));
        let prompt = self.scripts.pending_prompt.take();
        match outcome {
            Ok(Resumed::Finished(values)) => {
                self.dispatch(Action::EndGesture)?;
                if prompt.is_some() {
                    self.console_say(
                        ConsoleKind::Error,
                        format!(
                            "{}: ff.prompt works in the script itself, not in coroutines it makes",
                            d.label
                        ),
                    );
                }
                self.revision += 1;
                Ok(Done::Finished(values))
            }
            Ok(Resumed::Yielded(_)) => match prompt {
                Some(mut p) => {
                    self.dispatch(Action::EndGesture)?;
                    let id = self.scripts.fresh_id();
                    p.id = id;
                    p.script = d.label.clone();
                    // The step to join: the one this part made, or the
                    // one it joined.
                    let token = (self.history.revision() != revision)
                        .then(|| self.history.revision())
                        .or(d.amend.filter(|t| self.history.still_last(*t, &d.label)));
                    self.scripts.waiting.push(Waiting {
                        id,
                        label: d.label,
                        thread: d.thread,
                        token,
                        console: d.console,
                        fields: p.fields.clone(),
                    });
                    self.ui_requests.push(UiRequest::ScriptPrompt(p));
                    self.revision += 1;
                    Ok(Done::Waiting)
                }
                None => {
                    self.take_back(&d.label, d.amend)?;
                    let e = ScriptError::new(
                        ErrorKind::Runtime,
                        "coroutine.yield at the script's top is only for ff.prompt",
                    );
                    Err(self.script_failed(&d.label, &e))
                }
            },
            Err(e) => {
                self.take_back(&d.label, d.amend)?;
                Err(self.script_failed(&d.label, &e))
            }
        }
    }

    /// A failed run leaves the project as it was: its gesture cancelled,
    /// and the part before a prompt undone while it is still the last
    /// step.
    fn take_back(&mut self, label: &str, amend: Option<u64>) -> Result<()> {
        self.dispatch(Action::CancelGesture)?;
        if let Some(t) = amend
            && self.history.still_last(t, label)
            && let Some(r) = self.history.undo(&mut self.project)?
        {
            self.sync(r.impact)?;
        }
        self.revision += 1;
        Ok(())
    }

    fn script_failed(&mut self, label: &str, e: &ScriptError) -> SessionError {
        let text = format!("{label}: {e}");
        self.console_say(ConsoleKind::Error, text.clone());
        SessionError::Other(text)
    }

    pub(crate) fn console_say(&mut self, kind: ConsoleKind, text: String) {
        for line in text.lines() {
            if self.scripts.console.len() >= CONSOLE_KEPT {
                self.scripts.console.pop_front();
            }
            self.scripts.console.push_back(ConsoleLine {
                kind,
                text: line.to_string(),
            });
        }
        self.scripts.console_revision += 1;
    }

    fn take_script_output(&mut self, sb: &Sandbox) {
        for line in sb.take_output() {
            self.console_say(ConsoleKind::Output, line);
        }
    }

    fn console_line(&mut self, line: &str) -> Result<()> {
        self.console_say(ConsoleKind::Input, format!("> {line}"));
        let line = line.trim();
        if line.is_empty() {
            return Ok(());
        }
        self.refuse_nested()?;
        let sb = self.script_sandbox()?;
        let env = match &self.scripts.console_env {
            Some(e) => e.clone(),
            None => {
                let e = sb.environment().map_err(other)?;
                self.scripts.console_env = Some(e.clone());
                e
            }
        };
        // An expression's value is printed; else the line is a statement.
        let f = match sb.compile(&format!("return {line}"), "console", &env) {
            Ok(f) => f,
            Err(_) => match sb.compile(line, "console", &env) {
                Ok(f) => f,
                Err(e) => return Err(self.script_failed(CONSOLE_LABEL, &e)),
            },
        };
        let thread = sb.lua().create_thread(f).map_err(other)?;
        let done = self.drive(Drive {
            label: CONSOLE_LABEL.into(),
            thread,
            args: MultiValue::new(),
            amend: None,
            console: true,
        })?;
        if let Done::Finished(values) = done {
            self.print_values(&values);
        }
        Ok(())
    }

    fn print_values(&mut self, values: &MultiValue) {
        if values.is_empty() {
            return;
        }
        let shown: Vec<String> = values.iter().map(|v| api::show(v, 0)).collect();
        self.console_say(ConsoleKind::Result, shown.join("\t"));
    }

    fn answer_prompt(&mut self, prompt: u64, values: Option<Vec<String>>) -> Result<()> {
        // A drag under way: the script goes on once it ends.
        if self.history.in_gesture() || self.scripts.running.is_some() {
            self.scripts.answers.push((prompt, values));
            return Ok(());
        }
        let Some(i) = self.scripts.waiting.iter().position(|w| w.id == prompt) else {
            return Ok(());
        };
        let w = self.scripts.waiting.remove(i);
        let sb = self.script_sandbox()?;
        let args = match values {
            None => MultiValue::from_iter([Value::Nil]),
            Some(v) => {
                let t = api::answers(sb.lua(), &w.fields, &v).map_err(other)?;
                MultiValue::from_iter([Value::Table(t)])
            }
        };
        let done = self.drive(Drive {
            label: w.label,
            thread: w.thread,
            args,
            amend: w.token,
            console: w.console,
        })?;
        if let (Done::Finished(values), true) = (done, w.console) {
            self.print_values(&values);
        }
        Ok(())
    }

    fn stop_macro(&mut self, keep: bool) -> Result<()> {
        let Some(rec) = self.scripts.recorder.take() else {
            return Ok(());
        };
        self.revision += 1;
        if !keep {
            self.console_say(ConsoleKind::Info, "macro thrown away".into());
            return Ok(());
        }
        if rec.is_empty() {
            self.notify(
                NoticeLevel::Info,
                "nothing to keep: the macro recorded no steps",
            );
            return Ok(());
        }
        self.rescan_scripts(true);
        let number = library::next_number(&self.scripts.library);
        let name = format!("Macro {number}");
        let (description, body) = rec.script();
        let path = library::save_macro(&name, number, &description, &body).map_err(other)?;
        self.rescan_scripts(true);
        let skipped = rec.status().skipped;
        let mut text = format!("{name} recorded ({})", path.display());
        if !skipped.is_empty() {
            text.push_str(&format!("; not recorded: {}", skipped.join(", ")));
        }
        self.console_say(ConsoleKind::Info, text.clone());
        self.notify(NoticeLevel::Info, text);
        Ok(())
    }

    /// Timers due, renders finished, the library's folders (every
    /// [`RESCAN`]). From [`Session::tick`].
    pub(crate) fn tick_scripts(&mut self) {
        self.rescan_scripts(false);
        self.poll_script_renders();
        if self.scripts.running.is_some() || self.history.in_gesture() {
            return;
        }
        for (prompt, values) in std::mem::take(&mut self.scripts.answers) {
            if let Err(e) = self.answer_prompt(prompt, values) {
                self.notify(NoticeLevel::Warning, e.to_string());
            }
        }
        if self.scripts.timers.is_empty() {
            return;
        }
        let now = Instant::now();
        for _ in 0..TIMERS_A_TICK {
            let Some(i) = self.scripts.timers.iter().position(|t| t.due <= now) else {
                break;
            };
            let t = self.scripts.timers.remove(i);
            let thread = match self
                .script_sandbox()
                .and_then(|sb| sb.lua().create_thread(t.function.clone()).map_err(other))
            {
                Ok(th) => th,
                Err(e) => {
                    self.notify(NoticeLevel::Warning, format!("{}: {e}", t.label));
                    continue;
                }
            };
            let r = self.drive(Drive {
                label: t.label.clone(),
                thread,
                args: MultiValue::new(),
                amend: None,
                console: false,
            });
            match r {
                Ok(Done::Finished(values)) => {
                    let stop = matches!(values.front(), Some(Value::Boolean(false)));
                    if let (Some(every), false) = (t.every, stop) {
                        self.scripts.timers.push(Timer {
                            due: now + every,
                            ..t
                        });
                    }
                }
                Ok(Done::Waiting) => {
                    if let Some(every) = t.every {
                        self.scripts.timers.push(Timer {
                            due: now + every,
                            ..t
                        });
                    }
                }
                Err(e) => {
                    self.notify(
                        NoticeLevel::Warning,
                        format!("a timer of {} stopped: {e}", t.label),
                    );
                }
            }
        }
    }

    fn poll_script_renders(&mut self) {
        let mut said = Vec::new();
        for r in &mut self.scripts.renders {
            let finished = r.job.as_ref().is_some_and(|j| j.is_finished());
            if let Some(j) = &r.job {
                r.outcome.progress = j.progress.fraction();
            }
            if !finished {
                continue;
            }
            let Some(job) = r.job.take() else {
                continue;
            };
            r.outcome.done = true;
            r.outcome.progress = 1.0;
            match job.join() {
                Ok(files) => {
                    r.outcome.files = files.into_iter().map(|f| f.path).collect();
                    said.push((
                        NoticeLevel::Info,
                        format!("{}: rendered {} file(s)", r.label, r.outcome.files.len()),
                    ));
                }
                Err(e) => {
                    r.outcome.error = Some(e.to_string());
                    said.push((NoticeLevel::Warning, format!("{}: render: {e}", r.label)));
                }
            }
        }
        for (level, text) in said {
            self.console_say(ConsoleKind::Info, text.clone());
            self.notify(level, text);
        }
        // Finished renders nobody asked about are forgotten after a while.
        if self.scripts.renders.len() > 32 {
            self.scripts.renders.retain(|r| !r.outcome.done);
        }
    }

    /// Is a dispatch of `action` one for the macro recorder to hear?
    pub(crate) fn macro_hears(&self, action: &Action) -> bool {
        self.scripts.recorder.is_some()
            && self.scripts.running.is_none()
            && !self.scripts.hearing
            && !matches!(action, Action::Script(_))
    }

    /// Dispatch `action` and write what it did into the macro.
    pub(crate) fn dispatch_heard(&mut self, action: Action) -> Result<()> {
        let before = record::Before::of(self);
        self.scripts.hearing = true;
        let r = self.dispatch(action.clone());
        self.scripts.hearing = false;
        if r.is_ok() {
            record::note(self, &before, &action);
        }
        r
    }
}
