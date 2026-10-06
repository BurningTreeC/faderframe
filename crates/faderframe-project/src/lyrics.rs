//! The lyrics (or any words spoken or sung) along the timeline: lines of
//! text with where they start and end, edited whole
//! (`Command::SetLyrics`), shown in the arranger's Lyrics lane and moved
//! with section edits ([`crate::arrange`]). Transcribing a clip puts its
//! words here.

use faderframe_timeline::MusicalTime;
use serde::{Deserialize, Serialize};

/// A line of words over `start..end`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LyricLine {
    pub start: MusicalTime,
    pub end: MusicalTime,
    pub text: String,
}

/// Sorted by start, no empty lines, none ending before it starts.
pub fn normalize(lines: &mut Vec<LyricLine>) {
    for l in lines.iter_mut() {
        l.text = l.text.trim().to_string();
        if l.end < l.start {
            l.end = l.start;
        }
    }
    lines.retain(|l| !l.text.is_empty());
    lines.sort_by(|a, b| a.start.cmp(&b.start).then(a.end.cmp(&b.end)));
}

/// `lines` with those starting inside `start..end` replaced by `new`.
pub fn replace_span(
    lines: &[LyricLine],
    start: MusicalTime,
    end: MusicalTime,
    new: Vec<LyricLine>,
) -> Vec<LyricLine> {
    let mut out: Vec<LyricLine> = lines
        .iter()
        .filter(|l| l.start < start || l.start >= end)
        .cloned()
        .chain(new)
        .collect();
    normalize(&mut out);
    out
}

/// The line sounding at `t`, if any.
pub fn line_at(lines: &[LyricLine], t: MusicalTime) -> Option<usize> {
    lines.iter().position(|l| l.start <= t && t < l.end)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(a: f64, b: f64, t: &str) -> LyricLine {
        LyricLine {
            start: MusicalTime::from_quarters(a),
            end: MusicalTime::from_quarters(b),
            text: t.into(),
        }
    }

    #[test]
    fn lines_sort_trim_and_replace_by_span() {
        let mut l = vec![
            line(4.0, 6.0, " two "),
            line(0.0, 2.0, "one"),
            line(8.0, 7.0, "x"),
            line(9.0, 10.0, "  "),
        ];
        normalize(&mut l);
        assert_eq!(
            l.iter().map(|x| x.text.as_str()).collect::<Vec<_>>(),
            ["one", "two", "x"]
        );
        assert_eq!(l[2].end, l[2].start);
        let q = MusicalTime::from_quarters;
        let r = replace_span(&l, q(3.0), q(8.5), vec![line(5.0, 7.0, "new")]);
        assert_eq!(
            r.iter().map(|x| x.text.as_str()).collect::<Vec<_>>(),
            ["one", "new"]
        );
        assert_eq!(line_at(&r, q(1.0)), Some(0));
        assert_eq!(line_at(&r, q(3.0)), None);
    }
}
