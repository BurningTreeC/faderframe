//! Engraved pages as a PDF 1.4 file: the outlines and lines as vector
//! paths, the text in the five standard fonts (Helvetica and Times, in
//! WinAnsi; nothing embedded, every reader has them).

use crate::assets::FONT_NAMES;
use crate::engrave::{Anchor, Ink, Page, cp1252, text_width};
use crate::glyph::Seg;
use std::fmt::Write;

fn num(v: f32) -> String {
    let s = format!("{:.3}", v);
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" || s.is_empty() {
        "0".into()
    } else {
        s.to_string()
    }
}

/// A PDF string in parentheses (WinAnsi bytes, escaped).
fn pdf_string(text: &str) -> Vec<u8> {
    let mut out = vec![b'('];
    for c in text.chars() {
        let b = cp1252(c).unwrap_or(b'?');
        match b {
            b'(' | b')' | b'\\' => {
                out.push(b'\\');
                out.push(b);
            }
            0x20..=0x7E => out.push(b),
            _ => out.extend(format!("\\{b:03o}").bytes()),
        }
    }
    out.push(b')');
    out
}

fn content(page: &Page) -> Vec<u8> {
    let h = page.height;
    let y = |v: f32| num(h - v);
    let mut s: Vec<u8> = Vec::new();
    let mut ops = String::from("0 g 0 G 0 J 0 j\n");
    for i in &page.ink {
        match i {
            Ink::Fill(path) => {
                for seg in path {
                    match *seg {
                        Seg::M(a, b) => {
                            let _ = writeln!(ops, "{} {} m", num(a), y(b));
                        }
                        Seg::L(a, b) => {
                            let _ = writeln!(ops, "{} {} l", num(a), y(b));
                        }
                        Seg::C(a, b, c, d, e, f) => {
                            let _ = writeln!(
                                ops,
                                "{} {} {} {} {} {} c",
                                num(a),
                                y(b),
                                num(c),
                                y(d),
                                num(e),
                                y(f)
                            );
                        }
                        Seg::Z => ops.push_str("h\n"),
                    }
                }
                ops.push_str("f\n");
            }
            Ink::Line { a, b, width } => {
                let _ = writeln!(
                    ops,
                    "{} w {} {} m {} {} l S",
                    num(*width),
                    num(a.0),
                    y(a.1),
                    num(b.0),
                    y(b.1)
                );
            }
            Ink::Text {
                text,
                x,
                y: ty,
                size,
                font,
                anchor,
            } => {
                let w = text_width(text, *font, *size);
                let x = match anchor {
                    Anchor::Start => *x,
                    Anchor::Middle => x - w / 2.0,
                    Anchor::End => x - w,
                };
                let _ = write!(
                    ops,
                    "BT /F{} {} Tf {} {} Td ",
                    font.index(),
                    num(*size),
                    num(x),
                    y(*ty)
                );
                s.extend(ops.as_bytes());
                ops.clear();
                s.extend(pdf_string(text));
                ops.push_str(" Tj ET\n");
            }
        }
    }
    s.extend(ops.as_bytes());
    s
}

/// The file's bytes.
pub fn write(pages: &[Page], title: &str) -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let mut offsets: Vec<usize> = Vec::new();
    let object = |out: &mut Vec<u8>, offsets: &mut Vec<usize>, body: &[u8]| {
        offsets.push(out.len());
        let n = offsets.len();
        out.extend(format!("{n} 0 obj\n").bytes());
        out.extend(body);
        out.extend(b"\nendobj\n");
        n
    };
    // 1 catalogue, 2 pages, 3.. fonts, then info, then each page and its
    // contents.
    let fonts = FONT_NAMES.len();
    let first_page = 3 + fonts + 1;
    object(&mut out, &mut offsets, b"<< /Type /Catalog /Pages 2 0 R >>");
    let kids: Vec<String> = (0..pages.len())
        .map(|i| format!("{} 0 R", first_page + 2 * i))
        .collect();
    object(
        &mut out,
        &mut offsets,
        format!(
            "<< /Type /Pages /Kids [{}] /Count {} >>",
            kids.join(" "),
            pages.len()
        )
        .as_bytes(),
    );
    for name in FONT_NAMES {
        object(
            &mut out,
            &mut offsets,
            format!(
                "<< /Type /Font /Subtype /Type1 /BaseFont /{name} /Encoding /WinAnsiEncoding >>"
            )
            .as_bytes(),
        );
    }
    let mut info = b"<< /Title ".to_vec();
    info.extend(pdf_string(title));
    info.extend(b" /Producer (FaderFrame) /Creator (FaderFrame) >>");
    let info_n = object(&mut out, &mut offsets, &info);
    let font_dict: String = (0..fonts)
        .map(|i| format!("/F{i} {} 0 R", 3 + i))
        .collect::<Vec<_>>()
        .join(" ");
    for (i, page) in pages.iter().enumerate() {
        let contents = first_page + 2 * i + 1;
        object(
            &mut out,
            &mut offsets,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {} {}] /Resources << /Font << {font_dict} >> >> /Contents {contents} 0 R >>",
                num(page.width),
                num(page.height)
            )
            .as_bytes(),
        );
        let stream = content(page);
        let mut body = format!("<< /Length {} >>\nstream\n", stream.len()).into_bytes();
        body.extend(&stream);
        body.extend(b"\nendstream");
        object(&mut out, &mut offsets, &body);
    }
    let xref = out.len();
    out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len() + 1).bytes());
    for o in &offsets {
        out.extend(format!("{o:010} 00000 n \n").bytes());
    }
    out.extend(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R /Info {info_n} 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            offsets.len() + 1
        )
        .bytes(),
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engrave::{Layout, engrave};

    #[test]
    fn the_file_has_its_objects_where_the_table_says() {
        let sheet = crate::musicxml::tests::sample();
        let pages = engrave(&sheet, &Layout::default());
        let pdf = write(&pages, "A (lead) sheet");
        assert!(pdf.starts_with(b"%PDF-1.4"));
        assert!(pdf.ends_with(b"%%EOF\n"));
        // The cross-reference table points at each object's start (byte
        // offsets: the header's binary marker counts).
        let find = |what: &[u8]| pdf.windows(what.len()).rposition(|w| w == what);
        let sx = find(b"startxref\n").unwrap_or(0) + 10;
        let tail = std::str::from_utf8(&pdf[sx..]).unwrap_or("");
        let xref: usize = tail
            .lines()
            .next()
            .and_then(|l| l.trim().parse().ok())
            .unwrap_or(0);
        assert!(pdf[xref..].starts_with(b"xref"));
        let table = std::str::from_utf8(&pdf[xref..]).unwrap_or("");
        let mut n = 0;
        for line in table.lines().skip(3).take_while(|l| l.ends_with(" n ")) {
            n += 1;
            let at: usize = line[..10].parse().unwrap_or(0);
            assert!(
                pdf[at..].starts_with(format!("{n} 0 obj").as_bytes()),
                "object {n}"
            );
        }
        assert!(n > 8);
        let text = String::from_utf8_lossy(&pdf);
        assert!(text.contains("/BaseFont /Times-Bold"));
        assert!(text.contains("(A \\(lead\\) sheet)"));
        if let Ok(dir) = std::env::var("FADERFRAME_LEADSHEET_OUT") {
            let _ = std::fs::write(std::path::Path::new(&dir).join("sample.pdf"), &pdf);
        }
    }
}
