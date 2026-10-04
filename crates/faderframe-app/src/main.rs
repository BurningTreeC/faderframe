//! `faderframe` — the FaderFrame DAW.

#![forbid(unsafe_code)]
// Release builds on Windows are GUI programs: no console window (also none
// for the plugin-scan helper processes).
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use faderframe_ui::{BackendChoice, RunOptions};
use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "\
FaderFrame — multitrack DAW

USAGE:
    faderframe [OPTIONS] [PROJECT.ffproj]

OPTIONS:
    --backend <auto|pipewire|jack|system|asio|dummy>
                                 Audio system (default: from preferences, else auto;
                                 system: ALSA, WASAPI or CoreAudio; asio: Windows
                                 builds with the `asio` feature)
    --sample-rate <HZ>            Requested sample rate (44100, 48000, 96000, 192000, …)
    --buffer-size <FRAMES>        Requested buffer size (32, 64, 128, 256, 512, …)
    --threads <N>                 Processing threads incl. the audio thread (default: one per core)
    --empty                       Start with a new, empty project
    --demo                        Start with the demo session
                                  (without these or a PROJECT: Preferences → General → On start-up)
    --import <FILE>               Import an audio file on start-up (repeatable)
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
                    "pipewire" | "pw" => BackendChoice::PipeWire,
                    "system" | "alsa" | "wasapi" | "coreaudio" => BackendChoice::System,
                    "asio" if BackendChoice::available().contains(&BackendChoice::Asio) => {
                        BackendChoice::Asio
                    }
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
            "--threads" => {
                let v = value("--threads")?;
                o.threads = Some(
                    v.parse::<u16>()
                        .ok()
                        .filter(|t| (1..=256).contains(t))
                        .ok_or_else(|| format!("invalid thread count '{v}'"))?,
                );
            }
            "--empty" => o.empty = true,
            "--demo" => o.demo = true,
            "--import" => o.import.push(PathBuf::from(value("--import")?)),
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
    // Plugin scan helper (run by FaderFrame itself; a crashing plugin only
    // takes this process down).
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--scan-clap")
        && let Some(bundle) = args.get(2)
    {
        let code = faderframe_plugin_clap::scan::run_scan_subprocess(std::path::Path::new(bundle));
        return ExitCode::from(code.clamp(0, 255) as u8);
    }
    if args.get(1).map(String::as_str) == Some("--scan-vst3")
        && let Some(bundle) = args.get(2)
    {
        let code = faderframe_plugin_vst3::scan::run_scan_subprocess(std::path::Path::new(bundle));
        return ExitCode::from(code.clamp(0, 255) as u8);
    }

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
        let o = parse(args(&["--import", "a.wav", "--import", "b.flac"]))
            .unwrap()
            .unwrap();
        assert_eq!(
            o.import,
            vec![PathBuf::from("a.wav"), PathBuf::from("b.flac")]
        );
        assert_eq!(
            parse(args(&["--backend", "asio"])).is_ok(),
            BackendChoice::available().contains(&BackendChoice::Asio),
            "ASIO only where the build has it"
        );
        assert!(parse(args(&["--backend", "directsound"])).is_err());
        assert!(
            parse(args(&["--sample-rate", "12"])).is_ok(),
            "rate alone is validated by the backend"
        );
        assert!(parse(args(&["--sample-rate", "1000", "--buffer-size", "64"])).is_err());
        assert!(parse(args(&["--bogus"])).is_err());
    }
}
