//! Running a check in a helper process: a plugin that crashes, hangs or
//! prints garbage costs only that process, and comes back as a failed
//! report saying so.

use crate::CheckReport;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The line a helper puts its report on (after this; a plugin may print
/// to standard output too).
pub const MARKER: &str = "FADERFRAME-CHECK ";

/// Print `report` for the parent (the helper's side).
pub fn print_report(report: &CheckReport) -> Result<(), String> {
    let json = serde_json::to_string(report).map_err(|e| e.to_string())?;
    println!("{MARKER}{json}");
    Ok(())
}

/// Run `exe` with `args` and read the report it prints; past `timeout`
/// it is stopped.
pub fn in_subprocess(exe: &Path, args: &[String], timeout: Duration) -> CheckReport {
    let mut child = match Command::new(exe)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return CheckReport::failed(format!("cannot start the check: {e}")),
    };
    // Drained while waiting: a chatty plugin must not block on a full pipe.
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut s = String::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_string(&mut s);
            }
            s
        })
    };
    let out = drain(
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let err = drain(
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if start.elapsed() > timeout => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => return CheckReport::failed(e.to_string()),
        }
    };
    let stdout = out.join().unwrap_or_default();
    let stderr = err.join().unwrap_or_default();
    let Some(status) = status else {
        return CheckReport::failed(format!("it did not finish within {} s", timeout.as_secs()));
    };
    if let Some(report) = stdout
        .lines()
        .rev()
        .find_map(|l| l.strip_prefix(MARKER))
        .and_then(|json| serde_json::from_str::<CheckReport>(json).ok())
    {
        return report;
    }
    let last: Vec<&str> = stderr.lines().rev().take(2).collect();
    let tail = last.into_iter().rev().collect::<Vec<_>>().join(" / ");
    CheckReport::failed(if status.success() {
        format!("no report ({tail})")
    } else {
        format!(
            "it crashed the check ({status}){}",
            if tail.is_empty() {
                String::new()
            } else {
                format!(": {tail}")
            }
        )
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn sh(script: &str, timeout: Duration) -> CheckReport {
        in_subprocess(Path::new("/bin/sh"), &["-c".into(), script.into()], timeout)
    }

    #[test]
    fn the_report_is_found_among_what_the_plugin_printed() {
        let report = CheckReport {
            name: "Thing".into(),
            sample_rate: 48_000,
            block: 64,
            ..CheckReport::default()
        };
        let json = serde_json::to_string(&report).unwrap();
        let got = sh(
            &format!("echo plugin chatter; echo '{MARKER}{json}'; echo more chatter"),
            Duration::from_secs(10),
        );
        assert_eq!(got, report);
    }

    #[test]
    fn a_crash_or_a_hang_is_a_failed_check() {
        let crash = sh("echo dying >&2; kill -SEGV $$", Duration::from_secs(10));
        assert!(
            crash
                .error
                .as_deref()
                .is_some_and(|e| e.contains("crashed") && e.contains("dying")),
            "{crash:?}"
        );
        let hang = sh("sleep 5", Duration::from_millis(200));
        assert!(
            hang.error
                .as_deref()
                .is_some_and(|e| e.contains("did not finish")),
            "{hang:?}"
        );
    }
}
