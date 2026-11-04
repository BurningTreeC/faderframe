//! Script errors as the console and the notices show them: what went
//! wrong, and in which script on which line.

use std::fmt;

/// What kind of failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    /// The script does not compile.
    Syntax,
    /// It failed while running (an error it raised, or one of the API's).
    Runtime,
    /// It ran longer than [`crate::Limits::time`] and was stopped.
    TimeLimit,
    /// The scripts' state reached [`crate::Limits::memory`].
    Memory,
}

/// A failed script, with where it failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScriptError {
    pub kind: ErrorKind,
    /// What went wrong, without the place ("attempt to index a nil
    /// value (global 'x')").
    pub message: String,
    /// The chunk it happened in (the script's file name, `console`).
    pub chunk: Option<String>,
    /// The line in that chunk (1-based).
    pub line: Option<u32>,
    /// Lua's stack traceback, when there is one.
    pub traceback: Option<String>,
}

impl ScriptError {
    /// An error of `kind` with no place.
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            chunk: None,
            line: None,
            traceback: None,
        }
    }

    /// From mlua's error, the place found in the message or the traceback.
    pub fn from_lua(e: &mlua::Error) -> Self {
        let mut out = Self::new(ErrorKind::Runtime, String::new());
        describe(e, &mut out);
        if out.line.is_none()
            && let Some(tb) = &out.traceback
            && let Some((chunk, line)) = first_lua_frame(tb)
        {
            out.chunk = Some(chunk);
            out.line = Some(line);
        }
        out
    }

    /// "tidy.lua:12: attempt to call a nil value", or the bare message.
    pub fn located(&self) -> String {
        self.to_string()
    }
}

impl fmt::Display for ScriptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (&self.chunk, self.line) {
            (Some(c), Some(l)) => write!(f, "{c}:{l}: {}", self.message),
            (Some(c), None) => write!(f, "{c}: {}", self.message),
            _ => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for ScriptError {}

impl From<mlua::Error> for ScriptError {
    fn from(e: mlua::Error) -> Self {
        Self::from_lua(&e)
    }
}

fn describe(e: &mlua::Error, out: &mut ScriptError) {
    match e {
        mlua::Error::SyntaxError { message, .. } => {
            out.kind = ErrorKind::Syntax;
            take_message(message, out);
        }
        mlua::Error::RuntimeError(m) => take_message(m, out),
        mlua::Error::MemoryError(m) => {
            out.kind = ErrorKind::Memory;
            take_message(m, out);
        }
        mlua::Error::CallbackError { traceback, cause } => {
            if out.traceback.is_none() {
                out.traceback = Some(traceback.clone());
            }
            describe(cause, out);
        }
        mlua::Error::WithContext { context, cause } => {
            describe(cause, out);
            out.message = format!("{context}: {}", out.message);
        }
        mlua::Error::ExternalError(x) => take_message(&x.to_string(), out),
        other => take_message(&other.to_string(), out),
    }
}

/// Split "chunk:12: message\nstack traceback:…" into its parts.
fn take_message(text: &str, out: &mut ScriptError) {
    let (message, traceback) = match text.split_once("\nstack traceback:") {
        Some((m, t)) => (m, Some(format!("stack traceback:{t}"))),
        None => (text, None),
    };
    if out.traceback.is_none() {
        out.traceback = traceback;
    }
    match split_place(message) {
        Some((chunk, line, rest)) => {
            out.chunk = Some(chunk);
            out.line = Some(line);
            out.message = rest.trim().to_string();
        }
        None => out.message = message.trim().to_string(),
    }
}

/// "name:12: rest" → ("name", 12, "rest"); also `[string "…"]:3:`.
pub(crate) fn split_place(text: &str) -> Option<(String, u32, &str)> {
    let first = text.lines().next()?;
    let mut from = 0;
    while let Some(i) = first[from..].find(':') {
        let at = from + i;
        let after = &first[at + 1..];
        let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
        if !digits.is_empty() && after[digits.len()..].starts_with(':') && at > 0 {
            let line = digits.parse().ok()?;
            let rest = &text[at + 1 + digits.len() + 1..];
            return Some((first[..at].to_string(), line, rest));
        }
        from = at + 1;
    }
    None
}

/// The first frame of a traceback that is Lua code ("\ttidy.lua:12: in …").
fn first_lua_frame(traceback: &str) -> Option<(String, u32)> {
    traceback
        .lines()
        .map(str::trim)
        .filter(|l| !l.starts_with("[C]") && !l.starts_with("stack traceback"))
        .find_map(|l| split_place(l).map(|(c, n, _)| (c, n)))
}
