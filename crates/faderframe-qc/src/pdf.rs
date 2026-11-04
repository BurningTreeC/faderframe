//! The QC report laid out on A4 pages and written as a PDF by the lead
//! sheet's writer (`faderframe_leadsheet::pdf`: text in the standard
//! Helvetica, lines as vector paths — nothing embedded, every reader has
//! it). Black on white, so it prints and copies as it reads.
//!
//! The standard fonts speak WinAnsi: characters outside it are written as
//! their nearest ASCII ("−" as "-", "≤" as "<=").

use crate::report::{Check, QcFile, QcReport, Verdict, level, time};
use faderframe_leadsheet::engrave::{Anchor, Font, Ink, Page, cp1252, text_width};

const WIDTH: f32 = 595.0;
const HEIGHT: f32 = 842.0;
const LEFT: f32 = 48.0;
const RIGHT: f32 = WIDTH - 48.0;
const TOP: f32 = 56.0;
const BOTTOM: f32 = HEIGHT - 52.0;
const BODY: f32 = 8.5;
const SMALL: f32 = 7.5;
const LEADING: f32 = 1.32;

/// `text` in characters the standard fonts have.
pub fn safe(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if cp1252(c).is_some() {
            out.push(c);
            continue;
        }
        out.push_str(match c {
            '−' | '‐' | '‑' => "-",
            '≤' => "<=",
            '≥' => ">=",
            '∞' => "inf",
            '→' => "->",
            '←' => "<-",
            '′' => "'",
            '″' => "\"",
            '⅓' => "1/3",
            '✓' => "ok",
            '\u{a0}' | '\u{2009}' | '\u{202f}' => " ",
            _ => "?",
        });
    }
    out
}

struct Writer {
    pages: Vec<Page>,
    y: f32,
}

impl Writer {
    fn new() -> Self {
        let mut w = Self {
            pages: Vec::new(),
            y: TOP,
        };
        w.new_page();
        w
    }

    fn new_page(&mut self) {
        self.pages.push(Page {
            width: WIDTH,
            height: HEIGHT,
            ink: Vec::new(),
        });
        self.y = TOP;
    }

    fn ink(&mut self, i: Ink) {
        if let Some(p) = self.pages.last_mut() {
            p.ink.push(i);
        }
    }

    /// Room for `h` points more on this page, else a new one.
    fn room(&mut self, h: f32) {
        if self.y + h > BOTTOM {
            self.new_page();
        }
    }

    fn text_at(&mut self, x: f32, y: f32, text: &str, font: Font, size: f32, anchor: Anchor) {
        self.ink(Ink::Text {
            text: safe(text),
            x,
            y,
            size,
            font,
            anchor,
        });
    }

    fn rule(&mut self, width: f32) {
        let y = self.y;
        self.ink(Ink::Line {
            a: (LEFT, y),
            b: (RIGHT, y),
            width,
        });
    }

    /// `text` wrapped to `width` points: its lines.
    fn wrap(text: &str, font: Font, size: f32, width: f32) -> Vec<String> {
        let text = safe(text);
        let mut lines = Vec::new();
        for para in text.split('\n') {
            let mut line = String::new();
            for word in para.split(' ') {
                let candidate = if line.is_empty() {
                    word.to_string()
                } else {
                    format!("{line} {word}")
                };
                if text_width(&candidate, font, size) <= width {
                    line = candidate;
                    continue;
                }
                if !line.is_empty() {
                    lines.push(std::mem::take(&mut line));
                }
                // A word longer than the line (a hash, a path): broken.
                let mut part = String::new();
                for c in word.chars() {
                    part.push(c);
                    if text_width(&part, font, size) > width {
                        part.pop();
                        lines.push(std::mem::take(&mut part));
                        part.push(c);
                    }
                }
                line = part;
            }
            lines.push(line);
        }
        lines
    }

    /// A paragraph at `x`, `width` wide.
    fn para(&mut self, x: f32, width: f32, text: &str, font: Font, size: f32) {
        for line in Self::wrap(text, font, size, width) {
            self.room(size * LEADING);
            self.y += size * LEADING;
            let y = self.y;
            self.text_at(x, y, &line, font, size, Anchor::Start);
        }
    }

    fn gap(&mut self, h: f32) {
        self.y += h;
    }

    fn heading(&mut self, text: &str, size: f32) {
        self.room(size * 3.0);
        self.gap(size * 0.6);
        self.para(LEFT, RIGHT - LEFT, text, Font::SansBold, size);
        self.gap(3.0);
    }

    /// Name / value lines.
    fn pairs(&mut self, rows: &[(String, String)], bold_first: bool) {
        let key_w = 110.0;
        for (i, (k, v)) in rows.iter().enumerate() {
            let font = if bold_first && i == 0 {
                Font::SansBold
            } else {
                Font::Sans
            };
            let lines = Self::wrap(v, font, BODY, RIGHT - LEFT - key_w);
            self.room(lines.len() as f32 * BODY * LEADING);
            self.y += BODY * LEADING;
            let y = self.y;
            self.text_at(LEFT, y, k, Font::SansBold, BODY, Anchor::Start);
            for (n, line) in lines.iter().enumerate() {
                if n > 0 {
                    self.y += BODY * LEADING;
                }
                let y = self.y;
                self.text_at(LEFT + key_w, y, line, font, BODY, Anchor::Start);
            }
        }
    }

    /// A table: column widths (the last takes the rest), a header and
    /// rows; `strong(row)`: the row is set bold (a failed check).
    fn table(
        &mut self,
        widths: &[f32],
        header: &[&str],
        rows: &[Vec<String>],
        strong: &dyn Fn(usize) -> bool,
    ) {
        let total = RIGHT - LEFT;
        let mut cols: Vec<f32> = widths.to_vec();
        let used: f32 = cols.iter().take(cols.len().saturating_sub(1)).sum();
        if let Some(last) = cols.last_mut() {
            *last = total - used;
        }
        let pad = 4.0;
        let draw_row = |w: &mut Writer, cells: &[String], font: Font, size: f32| {
            let wrapped: Vec<Vec<String>> = cells
                .iter()
                .zip(&cols)
                .map(|(c, cw)| Self::wrap(c, font, size, cw - pad))
                .collect();
            let lines = wrapped.iter().map(Vec::len).max().unwrap_or(1).max(1);
            let h = lines as f32 * size * LEADING + 2.0;
            w.room(h + size);
            let top = w.y;
            let mut x = LEFT;
            for (cell, cw) in wrapped.iter().zip(&cols) {
                for (n, line) in cell.iter().enumerate() {
                    let y = top + (n + 1) as f32 * size * LEADING;
                    w.text_at(x, y, line, font, size, Anchor::Start);
                }
                x += cw;
            }
            w.y = top + h;
        };
        self.room(BODY * 4.0);
        draw_row(
            self,
            &header.iter().map(|h| h.to_string()).collect::<Vec<_>>(),
            Font::SansBold,
            SMALL,
        );
        self.rule(0.6);
        for (i, r) in rows.iter().enumerate() {
            let font = if strong(i) {
                Font::SansBold
            } else {
                Font::Sans
            };
            draw_row(self, r, font, SMALL);
            self.ink(Ink::Line {
                a: (LEFT, self.y),
                b: (RIGHT, self.y),
                width: 0.2,
            });
        }
        self.gap(4.0);
    }

    /// Footers on every page: what, and page n of m.
    fn footers(&mut self, left: &str) {
        let n = self.pages.len();
        for (i, p) in self.pages.iter_mut().enumerate() {
            p.ink.push(Ink::Line {
                a: (LEFT, HEIGHT - 40.0),
                b: (RIGHT, HEIGHT - 40.0),
                width: 0.4,
            });
            p.ink.push(Ink::Text {
                text: safe(left),
                x: LEFT,
                y: HEIGHT - 30.0,
                size: SMALL,
                font: Font::Sans,
                anchor: Anchor::Start,
            });
            p.ink.push(Ink::Text {
                text: format!("page {} of {n}", i + 1),
                x: RIGHT,
                y: HEIGHT - 30.0,
                size: SMALL,
                font: Font::Sans,
                anchor: Anchor::End,
            });
        }
    }
}

fn check_rows(checks: &[Check]) -> Vec<Vec<String>> {
    checks
        .iter()
        .map(|c| {
            vec![
                c.label.clone(),
                c.measured.clone(),
                c.requirement.clone(),
                c.verdict.label().to_string(),
            ]
        })
        .collect()
}

const CHECK_COLUMNS: [f32; 4] = [118.0, 196.0, 140.0, 0.0];

fn file_section(w: &mut Writer, n: usize, f: &QcFile) {
    w.heading(&format!("{n}. {} — {}", f.name, f.verdict.label()), 10.5);
    let mut rows = vec![("File".to_string(), f.path.clone())];
    if let Some(x) = &f.facts {
        rows.push((
            "Format".into(),
            format!(
                "{} · {}{} · {} Hz · {} channel{} ({})",
                x.container,
                x.encoding,
                if x.from_file { "" } else { " (as encoded)" },
                x.sample_rate,
                x.channels,
                if x.channels == 1 { "" } else { "s" },
                x.layout
            ),
        ));
        rows.push((
            "Duration".into(),
            format!(
                "{} ({} frames) · {} bytes",
                time(x.seconds),
                x.frames,
                x.bytes
            ),
        ));
    }
    rows.push((
        "SHA-256".into(),
        f.sha256.clone().unwrap_or_else(|| "not computed".into()),
    ));
    rows.push((
        "Role".into(),
        match f.role {
            crate::report::Role::Programme => "programme (judged against the spec)",
            crate::report::Role::Stem => "stem (part of the programme)",
            crate::report::Role::Part => "part of the programme",
        }
        .into(),
    ));
    w.pairs(&rows, false);
    if let Some(l) = &f.levels
        && l.channels.len() > 1
        && l.channels.len() <= 16
    {
        let per: Vec<String> = l
            .channels
            .iter()
            .map(|c| level(c.true_peak_dbtp, "").trim_end().to_string())
            .collect();
        w.pairs(
            &[("True peak / ch.".to_string(), per.join(" · ") + " dBTP")],
            false,
        );
    }
    w.gap(4.0);
    let checks = f.checks.clone();
    w.table(
        &CHECK_COLUMNS,
        &["Check", "Measured", "Requirement", "Verdict"],
        &check_rows(&checks),
        &|i| checks.get(i).is_some_and(|c| c.verdict == Verdict::Fail),
    );
    for note in &f.notes {
        w.para(
            LEFT,
            RIGHT - LEFT,
            &format!("Note: {note}"),
            Font::Sans,
            SMALL,
        );
    }
}

/// The report's pages (A4).
pub fn pages(r: &QcReport) -> Vec<Page> {
    let mut w = Writer::new();
    w.y += 18.0;
    let y = w.y;
    w.text_at(
        LEFT,
        y,
        "Delivery QC Report",
        Font::SansBold,
        18.0,
        Anchor::Start,
    );
    w.gap(6.0);
    w.para(
        LEFT,
        RIGHT - LEFT,
        &format!("{} — {}", r.title, r.project.name),
        Font::Sans,
        11.0,
    );
    w.gap(8.0);
    w.rule(0.8);
    w.gap(2.0);
    let mut rows = vec![
        ("Verdict".to_string(), r.summary()),
        ("Spec".to_string(), r.spec.name.clone()),
        ("Created".to_string(), r.created_text.clone()),
        ("Program".to_string(), r.project.program.clone()),
        (
            "Project".to_string(),
            match &r.project.path {
                Some(p) => format!(
                    "{} ({p}, file format {})",
                    r.project.name, r.project.file_version
                ),
                None => format!("{} (not saved)", r.project.name),
            },
        ),
        (
            "Files".to_string(),
            format!(
                "{} audio file{}{}",
                r.files.len(),
                if r.files.len() == 1 { "" } else { "s" },
                if r.receipts.is_empty() {
                    String::new()
                } else {
                    format!(", {} other", r.receipts.len())
                }
            ),
        ),
    ];
    for s in &r.settings {
        rows.push((s.name.clone(), s.value.clone()));
    }
    w.pairs(&rows, true);
    if !r.spec.source.is_empty() {
        w.gap(4.0);
        w.para(LEFT, RIGHT - LEFT, &r.spec.source, Font::Sans, SMALL);
    }
    for note in &r.spec.notes {
        w.para(LEFT, RIGHT - LEFT, note, Font::Sans, SMALL);
    }

    // Overview: a line per file.
    w.heading("Files", 12.0);
    let overview: Vec<Vec<String>> = r
        .files
        .iter()
        .map(|f| {
            let l = f.levels.as_ref();
            vec![
                f.name.clone(),
                l.and_then(|l| l.loudness)
                    .map_or_else(|| "—".into(), |x| level(x.integrated_lufs, "LUFS")),
                l.map_or_else(|| "—".into(), |l| level(l.true_peak_dbtp, "dBTP")),
                f.facts
                    .as_ref()
                    .map_or_else(|| "—".into(), |x| time(x.seconds)),
                f.verdict.label().to_string(),
            ]
        })
        .collect();
    let files = r.files.clone();
    w.table(
        &[190.0, 80.0, 80.0, 80.0, 0.0],
        &["File", "Loudness", "True peak", "Duration", "Verdict"],
        &overview,
        &|i| files.get(i).is_some_and(|f| f.verdict == Verdict::Fail),
    );

    if let Some(n) = &r.null_test {
        w.heading(&format!("Null test — {}", n.verdict.label()), 12.0);
        w.para(LEFT, RIGHT - LEFT, &n.summary, Font::Sans, BODY);
        w.gap(3.0);
        let f = &n.residual;
        let mut rows = vec![
            ("Reference".to_string(), n.reference.clone()),
            (
                "Residual".to_string(),
                format!(
                    "peak {}, RMS {}{}",
                    level(f.peak_dbfs, "dBFS"),
                    level(f.rms_dbfs, "dBFS"),
                    f.depth_db
                        .map_or_else(String::new, |d| format!(" ({d:.1} dB under the mix)"))
                ),
            ),
            (
                "Where".to_string(),
                match (f.first_at, f.worst_at) {
                    (Some(a), Some(b)) => format!(
                        "first above the floor at {}, worst at {} (channel {})",
                        time(a),
                        time(b),
                        f.worst_channel
                    ),
                    (None, Some(b)) => {
                        format!("worst at {} (channel {})", time(b), f.worst_channel)
                    }
                    _ => "nowhere: it nulls".into(),
                },
            ),
            (
                "In range / tail".to_string(),
                format!(
                    "{} / {}",
                    level(f.range_peak_dbfs, "dBFS"),
                    level(f.tail_peak_dbfs, "dBFS")
                ),
            ),
            (
                "Floor".to_string(),
                format!(
                    "{} (what rounding and dither alone leave at most)",
                    level(n.floor_dbfs, "dBFS")
                ),
            ),
            ("Summed".to_string(), n.summed.join(", ")),
        ];
        if let Some(s) = &n.shared_stem {
            rows.push(("Nulling stem".to_string(), s.clone()));
        }
        for l in &n.left_out {
            rows.push(("Left out".to_string(), format!("{} — {}", l.name, l.why)));
        }
        if let Some(lag) = n.lag_frames {
            rows.push(("Offset".to_string(), format!("{lag:+} frames")));
        }
        w.pairs(&rows, false);
        if !n.causes.is_empty() {
            w.gap(4.0);
            let why = if n.problems().next().is_some() {
                "Why:"
            } else {
                "Notes:"
            };
            w.para(LEFT, RIGHT - LEFT, why, Font::SansBold, BODY);
            for c in &n.causes {
                w.para(
                    LEFT + 8.0,
                    RIGHT - LEFT - 8.0,
                    &format!("• {}.", c.text),
                    Font::Sans,
                    BODY,
                );
                if let Some(fix) = &c.fix {
                    w.para(
                        LEFT + 16.0,
                        RIGHT - LEFT - 16.0,
                        &format!("{fix}."),
                        Font::Sans,
                        SMALL,
                    );
                }
            }
        }
        if let Some(m) = &n.mix {
            w.gap(4.0);
            w.para(
                LEFT,
                RIGHT - LEFT,
                &format!("The mix (the programme) — {}", m.verdict.label()),
                Font::SansBold,
                BODY,
            );
            let checks = m.checks.clone();
            w.table(
                &CHECK_COLUMNS,
                &["Check", "Measured", "Requirement", "Verdict"],
                &check_rows(&checks),
                &|i| checks.get(i).is_some_and(|c| c.verdict == Verdict::Fail),
            );
        }
    }

    for (i, f) in r.files.iter().enumerate() {
        file_section(&mut w, i + 1, f);
    }

    if !r.receipts.is_empty() {
        w.heading("Other delivered files", 12.0);
        let rows: Vec<Vec<String>> = r
            .receipts
            .iter()
            .map(|x| {
                vec![
                    x.name.clone(),
                    format!("{} bytes", x.bytes),
                    x.sha256.clone().unwrap_or_else(|| "not computed".into()),
                ]
            })
            .collect();
        w.table(
            &[170.0, 70.0, 0.0],
            &["File", "Size", "SHA-256"],
            &rows,
            &|_| false,
        );
    }

    w.heading("Method", 12.0);
    for m in &r.method {
        w.para(
            LEFT + 8.0,
            RIGHT - LEFT - 8.0,
            &format!("• {m}"),
            Font::Sans,
            SMALL,
        );
    }
    w.footers(&format!(
        "{} · {} · {} · {}",
        r.project.program, r.project.name, r.title, r.created_text
    ));
    w.pages
}

/// The report as a PDF file's bytes.
pub fn write(r: &QcReport) -> Vec<u8> {
    // The document's title is read as PDFDocEncoding, not WinAnsi: plain
    // characters only.
    let title: String = safe(&format!(
        "Delivery QC Report: {}, {}",
        r.title, r.project.name
    ))
    .chars()
    .map(|c| if c.is_ascii() { c } else { '-' })
    .collect();
    faderframe_leadsheet::pdf::write(&pages(r), &title)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_outside_winansi_is_spelled_out() {
        assert_eq!(
            safe("−23.0 ±0.5 LUFS ≤ −1 dBTP"),
            "-23.0 ±0.5 LUFS <= -1 dBTP"
        );
        assert_eq!(safe("12″ 33⅓"), "12\" 331/3");
        assert_eq!(safe("“Live” — ok…"), "“Live” — ok…");
    }

    #[test]
    fn long_words_break_and_lines_wrap() {
        let hash = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        let lines = Writer::wrap(hash, Font::Sans, 8.0, 100.0);
        assert!(lines.len() > 1);
        assert_eq!(lines.concat(), hash);
        let lines = Writer::wrap("a b c d e f g h", Font::Sans, 8.0, 20.0);
        assert!(lines.len() > 2 && lines.iter().all(|l| text_width(l, Font::Sans, 8.0) <= 20.0));
    }
}
