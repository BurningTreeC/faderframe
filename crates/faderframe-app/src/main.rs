//! `faderframe` — the FaderFrame DAW.

#![forbid(unsafe_code)]

use faderframe_ui::{BackendChoice, RunOptions};
use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "\
FaderFrame — multitrack DAW

USAGE:
    faderframe [OPTIONS] [PROJECT.ffproj]

OPTIONS:
    --backend <auto|jack|dummy>   Audio system (default: from preferences, else auto)
    --sample-rate <HZ>            Requested sample rate (44100, 48000, 96000, 192000, …)
    --buffer-size <FRAMES>        Requested buffer size (32, 64, 128, 256, 512, …)
    --empty                       Start with an empty project instead of the demo session
    -h, --help                    Show this help
    -V, --version                 Show the version

ENVIRONMENT:
    RUST_LOG                      Log filter, e.g. RUST_LOG=faderframe=debug
";

fn parse(args: impl Iterator<Item = String>) -> Result<Option<RunOptions>, String> {
    let mut o = RunOptions::default();
    let mut args = args.peekable();
    while let Some(a) = args.next() {
        let mut value = |name: &str| args.next().ok_or_else(|| format!("{name} needs a value"));
        match a.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(None);
            }
            "-V" | "--version" => {
                println!("faderframe {}", env!("CARGO_PKG_VERSION"));
                return Ok(None);
            }
            "--backend" => {
                o.backend = match value("--backend")?.as_str() {
                    "auto" => BackendChoice::Auto,
                    "jack" => BackendChoice::Jack,
                    "dummy" | "none" => BackendChoice::Dummy,
                    other => return Err(format!("unknown backend '{other}'")),
                }
            }
            "--sample-rate" => {
                let v = value("--sample-rate")?;
                o.sample_rate = Some(
                    v.parse()
                        .map_err(|_| format!("invalid sample rate '{v}'"))?,
                );
            }
            "--buffer-size" => {
                let v = value("--buffer-size")?;
                o.buffer_size = Some(
                    v.parse()
                        .map_err(|_| format!("invalid buffer size '{v}'"))?,
                );
            }
            "--empty" => o.empty = true,
            s if s.starts_with('-') => return Err(format!("unknown option '{s}'")),
            path => o.project = Some(PathBuf::from(path)),
        }
    }
    if let (Some(sr), Some(bs)) = (o.sample_rate, o.buffer_size) {
        faderframe_audio::validate_format(sr, bs).map_err(|e| e.to_string())?;
    }
    Ok(Some(o))
}

fn main() -> ExitCode {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();

    match parse(std::env::args().skip(1)) {
        Ok(Some(options)) => {
            let code = faderframe_ui::run(options);
            if code == gtk_exit_success() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Ok(None) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("faderframe: {e}\n\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn gtk_exit_success() -> faderframe_ui::ExitCode {
    faderframe_ui::ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> impl Iterator<Item = String> {
        a.iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>()
            .into_iter()
    }

    #[test]
    fn parses_options() {
        let o = parse(args(&[
            "--backend",
            "dummy",
            "--sample-rate",
            "96000",
            "--buffer-size",
            "64",
            "song.ffproj",
        ]))
        .unwrap()
        .unwrap();
        assert_eq!(o.backend, BackendChoice::Dummy);
        assert_eq!(o.sample_rate, Some(96_000));
        assert_eq!(o.buffer_size, Some(64));
        assert_eq!(o.project, Some(PathBuf::from("song.ffproj")));
        assert!(parse(args(&["--backend", "asio"])).is_err());
        assert!(
            parse(args(&["--sample-rate", "12"])).is_ok(),
            "rate alone is validated by the backend"
        );
        assert!(parse(args(&["--sample-rate", "1000", "--buffer-size", "64"])).is_err());
        assert!(parse(args(&["--bogus"])).is_err());
    }
}
