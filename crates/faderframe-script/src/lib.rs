//! Sandboxed Lua 5.4 for FaderFrame's scripts and macros.
//!
//! FaderFrame runs the user's scripts (and the macros it records, which
//! are scripts too) in one Lua 5.4 state per session — the vendored
//! reference interpreter (MIT) through `mlua`. This crate makes that state
//! safe to hand to scripts and knows nothing of the session; the session's
//! `script` module puts the `ff` table (the API over the session) into it.
//!
//! * [`Sandbox`]: the state with the libraries scripts may use. Base
//!   functions without `dofile`/`loadfile` (and `load` for text only),
//!   `string` without `dump`, `table`, `math`, `utf8`, `coroutine`, `os`
//!   with only `clock`, `date`, `difftime` and `time` — nothing runs
//!   programs, exits, reads the environment or removes files —, and an
//!   `io` that opens files only inside the folders the session allows
//!   ([`Folders::files`]: the project's folder and the scripts folders).
//!   `require` loads Lua modules (text, never binaries) from the scripts
//!   folders. `print` and `io.write` go to the console
//!   ([`Sandbox::take_output`]).
//! * Limits ([`Limits`]): every run (a script, a timer's call, a console
//!   line, the part of a script after a prompt was answered) may compute
//!   for so long, and the state may hold so much memory. A run over its
//!   time is stopped where it is — yielded out of when it can be, else
//!   failed —, whatever `pcall` it is inside: the sandbox's `pcall`,
//!   `xpcall` and `coroutine.resume` pass the stop on.
//! * Each run gets its own global environment ([`Sandbox::environment`])
//!   over the shared libraries, so scripts never see each other's globals.
//! * Errors ([`ScriptError`]) carry the script and line they happened at
//!   (from the message, else from the traceback's first Lua frame).
//! * [`header`]: the `--!` lines at the top of a script file (its name,
//!   description, a macro's number); [`literal`]: Lua source for values
//!   (macros are written as Lua).

#![forbid(unsafe_code)]

mod error;
pub mod header;
pub mod literal;
mod sandbox;

pub use error::{ErrorKind, ScriptError};
pub use header::{ScriptInfo, read_header, write_header};
pub use mlua;
pub use sandbox::{Folders, Limits, Resumed, Sandbox, environment_of};

#[cfg(test)]
mod tests;
