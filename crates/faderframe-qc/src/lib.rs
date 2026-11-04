//! Delivery QC: every delivered file checked against a delivery spec
//! ([`spec::SPECS`]: EBU R128, ATSC A/85, Netflix, streaming targets, CD
//! Red Book, ARIB TR-B32, OP-59 …) with pass, warn or fail and the
//! measured value ([`check`]); a stems delivery's null test against the
//! mix ([`null`]); and the report of it all ([`report::QcReport`]) as JSON
//! and as a PDF ([`pdf`]), with each file's SHA-256 ([`sha256`]) as its
//! receipt.
//!
//! Pure: the measurements come from `faderframe_analysis::{levels, qc}`
//! (the files read back by the session), the PDF from the lead sheet's
//! writer.

#![forbid(unsafe_code)]

pub mod check;
pub mod date;
pub mod null;
pub mod pdf;
pub mod report;
pub mod sha256;
pub mod spec;

pub use check::checks;
pub use date::Utc;
pub use null::{Cause, CauseKind, DitherKind, NullOutcome, NullTest, Precision};
pub use report::{Check, Facts, Levels, QcFile, QcReport, Receipt, Role, Verdict};
pub use spec::{SPECS, Spec};

#[cfg(test)]
mod tests {
    use super::*;
    use faderframe_analysis::levels::{FileMeter, full_scale};
    use faderframe_analysis::qc::{NullMeter, QcMeter, silence_floor};

    /// A report of one measured stereo tone, a stem and a null test.
    fn sample() -> QcReport {
        let rate = 48_000;
        let tone: Vec<f32> = (0..rate * 6)
            .map(|i| 0.25 * (i as f32 * 997.0 * std::f32::consts::TAU / rate as f32).sin())
            .collect();
        let mut meter = FileMeter::new(rate, 2, Some(&[1.0, 1.0]), full_scale(24, false));
        let mut qc = QcMeter::new(rate, 2, full_scale(24, false), silence_floor(24, false));
        meter.add(&[&tone, &tone]);
        qc.add(&[&tone, &tone]);
        let levels = Levels::of(&meter.finish(), Some(&qc.finish()), rate);
        let facts = Facts {
            container: "WAV".into(),
            encoding: "24-bit PCM".into(),
            sample_rate: rate,
            bits: Some(24),
            float: false,
            channels: 2,
            layout: "Stereo".into(),
            frames: tone.len() as u64,
            seconds: 6.0,
            bytes: 1234,
            from_file: true,
        };
        let spec = Spec::by_id("ebu-r128").unwrap_or(Spec::none());
        let file = |name: &str, role: Role| QcFile {
            path: format!("/tmp/{name}"),
            name: name.into(),
            role,
            sha256: Some("ab".repeat(32)),
            facts: Some(facts.clone()),
            levels: Some(levels.clone()),
            checks: checks(&facts, Some(&levels), spec, role),
            verdict: Verdict::Info,
            notes: Vec::new(),
        };
        let mut r = QcReport::new(
            "Stems",
            report::ProjectInfo {
                name: "Song (demo)".into(),
                path: Some("/home/x/Song.ffproj".into()),
                program: "FaderFrame 0.36.0".into(),
                file_version: 12,
            },
            Utc(1_793_282_592),
            spec,
            vec![
                file("Song - Bass.wav", Role::Stem),
                file("Song - Keys.wav", Role::Stem),
            ],
        );
        let sum: Vec<f64> = tone.iter().map(|&v| f64::from(v)).collect();
        let mut nm = NullMeter::new(1e-5, tone.len() as u64);
        nm.add(&[&sum, &sum], &[&tone, &tone]);
        let mut n = NullTest::new(
            &nm.finish(),
            rate,
            Precision {
                files: 3,
                bits: Some(24),
                dither: DitherKind::Tpdf,
            },
            tone.len() as u64,
            None,
            Vec::new(),
            vec!["Song - Bass.wav".into(), "Song - Keys.wav".into()],
            Vec::new(),
            "the mix, rendered with the same settings".into(),
        );
        n.mix = Some(file("the mix", Role::Programme));
        r.null_test = Some(n);
        r.receipts.push(Receipt {
            path: "/tmp/Song.cue".into(),
            name: "Song.cue".into(),
            bytes: 10,
            sha256: Some("cd".repeat(32)),
        });
        r.tally();
        r
    }

    #[test]
    fn the_json_holds_the_values_and_reads_back() {
        let r = sample();
        // −12 dBFS-peak tone on both sides: about −12 LUFS, far from R128.
        let mix = r.null_test.as_ref().and_then(|n| n.mix.as_ref()).unwrap();
        let integrated = mix
            .checks
            .iter()
            .find(|c| c.id == "loudness.integrated")
            .unwrap();
        assert_eq!(integrated.verdict, Verdict::Fail);
        assert!((integrated.value.unwrap() + 12.0).abs() < 0.2);
        assert_eq!(r.verdict, Verdict::Fail);
        assert_eq!(
            r.null_test.as_ref().map(|n| n.outcome),
            Some(NullOutcome::Exact)
        );
        let json = r.to_json();
        // It reads back (numbers to their last bit or so: serde_json's
        // parser rounds some).
        let back: QcReport = serde_json::from_str(&json).unwrap();
        assert_eq!((back.verdict, back.counts), (r.verdict, r.counts));
        assert_eq!(back.files.len(), r.files.len());
        assert_eq!(back.files[1].checks.len(), r.files[1].checks.len());
        let near = |a: Option<f64>, b: Option<f64>| match (a, b) {
            (Some(a), Some(b)) => (a - b).abs() < 1e-9,
            (a, b) => a == b,
        };
        let (a, b) = (back.files[0].levels.as_ref(), r.files[0].levels.as_ref());
        assert!(near(
            a.and_then(|l| l.true_peak_dbtp),
            b.and_then(|l| l.true_peak_dbtp)
        ));
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["schema"], "faderframe.qc/1");
        assert_eq!(v["created"], "2026-10-29T14:03:12Z");
        assert_eq!(v["spec"]["id"], "ebu-r128");
        assert_eq!(v["files"][0]["sha256"], "ab".repeat(32));
        assert_eq!(v["null_test"]["outcome"], "exact");
        // Silence has no level: null, never a number.
        assert!(v["null_test"]["residual"]["peak_dbfs"].is_null());
        let tp = v["files"][0]["levels"]["true_peak_dbtp"].as_f64().unwrap();
        assert!((tp + 12.0).abs() < 0.1, "{tp}");
    }

    #[test]
    fn the_pdf_is_well_formed() {
        let r = sample();
        let pdf = pdf::write(&r);
        assert!(pdf.starts_with(b"%PDF-1.4") && pdf.ends_with(b"%%EOF\n"));
        let text = String::from_utf8_lossy(&pdf);
        assert!(text.contains("(Delivery QC Report)"));
        assert!(text.contains("Song - Bass.wav"));
        assert!(pdf::pages(&r).len() >= 2, "a page per few files");
        if let Ok(dir) = std::env::var("FADERFRAME_QC_OUT") {
            let dir = std::path::Path::new(&dir);
            let _ = std::fs::write(dir.join("sample-qc.pdf"), &pdf);
            let _ = std::fs::write(dir.join("sample-qc.json"), r.to_json());
        }
    }
}
