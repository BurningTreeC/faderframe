//! The lines at the top of a script file that say what it is:
//!
//! ```lua
//! --! name: Tidy Up the Drums
//! --! description: Colours, names and folders for a multitrack kit.
//! --! macro: 3
//! ```
//!
//! Each is `--! key: value`. They come first (blank lines and a `#!` line
//! may precede them); the first other line ends them. `name` is what the
//! Scripts menu shows (else the file's name), `description` its tooltip
//! and the console's listing, `macro` the number of a recorded macro (the
//! one MIDI controllers and surfaces run, `MappingTarget::RunMacro`).
//! Other keys are kept as they are.

/// What a script's header says.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScriptInfo {
    pub name: Option<String>,
    pub description: Option<String>,
    /// A recorded macro's number (from 1).
    pub macro_number: Option<u16>,
    /// Other `--!` lines, in order.
    pub other: Vec<(String, String)>,
}

/// The header of `source` (nothing set when it has none).
pub fn read_header(source: &str) -> ScriptInfo {
    let mut info = ScriptInfo::default();
    for (i, line) in source.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || (i == 0 && line.starts_with("#!")) {
            continue;
        }
        let Some(rest) = line.strip_prefix("--!") else {
            break;
        };
        let Some((key, value)) = rest.split_once(':') else {
            continue;
        };
        let (key, value) = (key.trim().to_lowercase(), value.trim().to_string());
        match key.as_str() {
            "name" if !value.is_empty() => info.name = Some(value),
            "description" if !value.is_empty() => info.description = Some(value),
            "macro" => info.macro_number = value.parse().ok().filter(|n| *n > 0),
            _ => info.other.push((key, value)),
        }
    }
    info
}

/// `info` as header lines (each ends with a newline).
pub fn write_header(info: &ScriptInfo) -> String {
    let one_line = |s: &str| s.replace(['\n', '\r'], " ");
    let mut out = String::new();
    if let Some(n) = &info.name {
        out.push_str(&format!("--! name: {}\n", one_line(n)));
    }
    if let Some(d) = &info.description {
        out.push_str(&format!("--! description: {}\n", one_line(d)));
    }
    if let Some(m) = info.macro_number {
        out.push_str(&format!("--! macro: {m}\n"));
    }
    for (k, v) in &info.other {
        out.push_str(&format!("--! {}: {}\n", one_line(k), one_line(v)));
    }
    out
}

/// `source` with its header replaced by `info`'s (the rest kept as it
/// is).
pub fn replace_header(source: &str, info: &ScriptInfo) -> String {
    let mut body = Vec::new();
    let mut in_header = true;
    let mut shebang = None;
    for (i, line) in source.lines().enumerate() {
        let t = line.trim();
        if in_header {
            if i == 0 && t.starts_with("#!") {
                shebang = Some(line);
                continue;
            }
            if t.starts_with("--!") {
                continue;
            }
            if t.is_empty() && body.is_empty() {
                continue;
            }
            in_header = false;
        }
        body.push(line);
    }
    let mut out = String::new();
    if let Some(s) = shebang {
        out.push_str(s);
        out.push('\n');
    }
    out.push_str(&write_header(info));
    if !body.is_empty() {
        out.push('\n');
        out.push_str(&body.join("\n"));
        out.push('\n');
    }
    out
}
