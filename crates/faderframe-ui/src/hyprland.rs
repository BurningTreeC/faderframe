//! Hyprland's own window controls over its IPC socket. Hyprland honours
//! no X11 or Wayland request to keep a window above the others (its X11
//! window manager reads no `_NET_WM_STATE_ABOVE`, and Wayland has no such
//! request yet); its "pin" does that — a pinned floating window is drawn
//! above every other one (and shown on every workspace).
//!
//! FaderFrame's windows are found by process id and title in
//! `j/clients`; commands go as `dispatch <command> address:<window>`.

use serde_json::Value;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

/// The running Hyprland's command socket.
fn socket() -> Option<PathBuf> {
    let signature = std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE")?;
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from);
    // Since 0.40 under $XDG_RUNTIME_DIR, before in /tmp.
    runtime
        .into_iter()
        .map(|r| r.join("hypr"))
        .chain(Some(PathBuf::from("/tmp/hypr")))
        .map(|d| d.join(&signature).join(".socket.sock"))
        .find(|p| p.exists())
}

/// Is FaderFrame running under Hyprland (its socket there)?
pub fn available() -> bool {
    socket().is_some()
}

/// One request, its whole answer.
fn request(command: &str) -> Result<String, String> {
    let path = socket().ok_or("no Hyprland socket")?;
    let mut s = UnixStream::connect(&path).map_err(|e| format!("Hyprland: {e}"))?;
    let _ = s.set_read_timeout(Some(Duration::from_millis(500)));
    let _ = s.set_write_timeout(Some(Duration::from_millis(500)));
    s.write_all(command.as_bytes())
        .map_err(|e| format!("Hyprland: {e}"))?;
    let mut answer = String::new();
    s.read_to_string(&mut answer)
        .map_err(|e| format!("Hyprland: {e}"))?;
    Ok(answer)
}

/// What one of our windows needs: floating first (only floating windows
/// pin), its pin changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub address: String,
    pub float: bool,
    pub pin: bool,
}

/// The windows of processes `ours` takes titled `title` in Hyprland's
/// `clients` list and what each needs to be pinned (`on`) or not. `None`:
/// no such window (not mapped yet).
pub fn changes(
    clients: &Value,
    ours: &dyn Fn(u32) -> bool,
    title: &str,
    on: bool,
) -> Option<Vec<Change>> {
    let ours: Vec<&Value> = clients
        .as_array()?
        .iter()
        .filter(|c| {
            c.get("pid")
                .and_then(Value::as_u64)
                .and_then(|p| u32::try_from(p).ok())
                .is_some_and(ours)
                && c.get("title").and_then(Value::as_str) == Some(title)
        })
        .collect();
    if ours.is_empty() {
        return None;
    }
    Some(
        ours.into_iter()
            .filter_map(|c| {
                let flag = |name: &str| c.get(name).and_then(Value::as_bool).unwrap_or(false);
                Some(Change {
                    address: c.get("address").and_then(Value::as_str)?.to_string(),
                    float: on && !flag("floating"),
                    pin: flag("pinned") != on,
                })
            })
            .filter(|c| c.float || c.pin)
            .collect(),
    )
}

/// The requests for a change: Hyprland 0.56's Lua dispatchers (`lua`),
/// else the older ones (where `pin` toggles).
pub fn commands(c: &Change, on: bool, lua: bool) -> Vec<String> {
    let window = format!("address:{}", c.address);
    let mut out = Vec::new();
    if c.float {
        out.push(if lua {
            format!(r#"dispatch hl.dsp.window.float({{ action = "on", window = "{window}" }})"#)
        } else {
            format!("dispatch setfloating {window}")
        });
    }
    if c.pin {
        out.push(if lua {
            let action = if on { "on" } else { "off" };
            format!(r#"dispatch hl.dsp.window.pin({{ action = "{action}", window = "{window}" }})"#)
        } else {
            format!("dispatch pin {window}")
        });
    }
    out
}

/// Which dispatchers this Hyprland takes: 0 unknown, 1 Lua, 2 the older.
static SYNTAX: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// Pin (or unpin) the windows titled `title` of processes `ours` takes;
/// `Ok(false)` while Hyprland does not know such a window yet.
pub fn set_pinned(title: &str, on: bool, ours: &dyn Fn(u32) -> bool) -> Result<bool, String> {
    use std::sync::atomic::Ordering;
    let clients: Value = serde_json::from_str(&request("j/clients")?)
        .map_err(|e| format!("Hyprland's window list: {e}"))?;
    let Some(changes) = changes(&clients, ours, title, on) else {
        return Ok(false);
    };
    for c in &changes {
        let known = SYNTAX.load(Ordering::Relaxed);
        let tries: &[bool] = match known {
            1 => &[true],
            2 => &[false],
            _ => &[true, false],
        };
        let mut last = String::new();
        let done = tries.iter().any(|&lua| {
            let all = commands(c, on, lua)
                .iter()
                .all(|command| match request(command) {
                    Ok(answer) if answer.trim() == "ok" => true,
                    Ok(answer) => {
                        last = format!("{command}: {}", answer.trim());
                        false
                    }
                    Err(e) => {
                        last = e;
                        false
                    }
                });
            if all {
                SYNTAX.store(if lua { 1 } else { 2 }, Ordering::Relaxed);
            }
            all
        });
        if !done {
            return Err(format!("Hyprland: {last}"));
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clients() -> Value {
        serde_json::json!([
            { "address": "0x1", "pid": 7, "title": "Drums — EQ", "floating": true, "pinned": false },
            { "address": "0x2", "pid": 7, "title": "Bass — EQ", "floating": true, "pinned": true },
            { "address": "0x3", "pid": 9, "title": "Drums — EQ", "floating": true, "pinned": false },
            { "address": "0x4", "pid": 7, "title": "Keys — Synth", "floating": false, "pinned": false },
        ])
    }

    #[test]
    fn only_our_windows_with_that_title_change() {
        let change = |address: &str, float, pin| Change {
            address: address.into(),
            float,
            pin,
        };
        assert_eq!(
            changes(&clients(), &|p| p == 7, "Drums — EQ", true),
            Some(vec![change("0x1", false, true)])
        );
        // Already as wanted: nothing to do.
        assert_eq!(
            changes(&clients(), &|p| p == 7, "Drums — EQ", false),
            Some(vec![])
        );
        assert_eq!(
            changes(&clients(), &|p| p == 7, "Bass — EQ", false),
            Some(vec![change("0x2", false, true)])
        );
        // A tiled one floats first.
        assert_eq!(
            changes(&clients(), &|p| p == 7, "Keys — Synth", true),
            Some(vec![change("0x4", true, true)])
        );
        // Not mapped yet.
        assert_eq!(changes(&clients(), &|p| p == 7, "Lead — Synth", true), None);
    }

    #[test]
    fn both_dispatcher_syntaxes_are_written() {
        let c = Change {
            address: "0x4".into(),
            float: true,
            pin: true,
        };
        assert_eq!(
            commands(&c, true, true),
            vec![
                r#"dispatch hl.dsp.window.float({ action = "on", window = "address:0x4" })"#,
                r#"dispatch hl.dsp.window.pin({ action = "on", window = "address:0x4" })"#,
            ]
        );
        assert_eq!(
            commands(&c, true, false),
            vec![
                "dispatch setfloating address:0x4",
                "dispatch pin address:0x4"
            ]
        );
        let off = Change { float: false, ..c };
        assert_eq!(
            commands(&off, false, true),
            vec![r#"dispatch hl.dsp.window.pin({ action = "off", window = "address:0x4" })"#]
        );
    }
}
