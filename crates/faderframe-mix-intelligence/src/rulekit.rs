//! Small pieces the phase, width and dynamics rules share: stable ids
//! (the same scheme as the first rules), an empty finding to fill, terms.

use crate::finding::{Finding, RuleId, SignalPoint, Subject, Term, TimeSpan};
use faderframe_core::TrackId;

/// FNV-1a of the rule, the signal and the parts: a finding's id (stable
/// across analyses of the same thing).
pub(crate) fn id(rule: RuleId, signal: SignalPoint, parts: &[u64]) -> u64 {
    let mut h = crate::Fnv::new();
    h.write_u64(rule as u64);
    h.write_u64(signal as u64);
    for p in parts {
        h.write_u64(*p);
    }
    h.finish()
}

/// A finding of `rule` about `subject` with nothing measured yet.
pub(crate) fn blank(rule: RuleId, signal: SignalPoint, subject: Subject, parts: &[u64]) -> Finding {
    let tracks = match subject {
        Subject::Track(t) | Subject::Master(Some(t)) => vec![t],
        Subject::Pair { target, masker } => vec![target, masker],
        Subject::Tracks { a, b } => vec![a, b],
        Subject::Master(None) => Vec::new(),
    };
    Finding {
        id: id(rule, signal, parts),
        rule,
        signal,
        subject,
        tracks,
        time: None,
        events: Vec::new(),
        pattern: None,
        frequency: None,
        severity: 0.0,
        severity_terms: Vec::new(),
        confidence: 0.0,
        confidence_terms: Vec::new(),
        evidence: Vec::new(),
        spectrum: None,
        sections: Vec::new(),
        recommendations: Vec::new(),
        caveats: Vec::new(),
    }
}

pub(crate) fn term(name: &'static str, value: f64) -> Term {
    Term { name, value }
}

/// Severity and confidence as products of their terms (clamped 0 to 1).
pub(crate) fn score(f: &mut Finding, severity: Vec<Term>, confidence: Vec<Term>) {
    f.severity = severity
        .iter()
        .map(|t| t.value)
        .product::<f64>()
        .clamp(0.0, 1.0);
    f.confidence = confidence
        .iter()
        .map(|t| t.value)
        .product::<f64>()
        .clamp(0.0, 1.0);
    f.severity_terms = severity;
    f.confidence_terms = confidence;
}

pub(crate) fn round_to(v: f64, step: f64) -> f64 {
    (v / step).round() * step
}

/// The track a subject names (the master's when known).
pub(crate) fn track_of(s: Subject) -> Option<TrackId> {
    match s {
        Subject::Track(t) | Subject::Master(Some(t)) => Some(t),
        _ => None,
    }
}

/// A key for a subject in ids.
pub(crate) fn key_of(s: Subject) -> u64 {
    match s {
        Subject::Track(t) | Subject::Master(Some(t)) => t.0,
        Subject::Master(None) => u64::MAX,
        Subject::Pair { target, .. } => target.0,
        Subject::Tracks { a, .. } => a.0,
    }
}

/// Spans with gaps up to `gap` seconds merged, at most
/// [`crate::finding::MAX_EVENTS`].
pub(crate) fn merge_spans(mut spans: Vec<TimeSpan>, gap: f64) -> Vec<TimeSpan> {
    spans.sort_by(|a, b| a.start.total_cmp(&b.start));
    let mut out: Vec<TimeSpan> = Vec::new();
    for s in spans {
        match out.last_mut() {
            Some(l) if s.start - l.end <= gap => l.end = l.end.max(s.end),
            _ => out.push(s),
        }
    }
    if out.len() > crate::finding::MAX_EVENTS {
        let end = out.last().map_or(0.0, |e| e.end);
        out.truncate(crate::finding::MAX_EVENTS);
        if let Some(l) = out.last_mut() {
            l.end = end;
        }
    }
    out
}

/// The span from the first event's start to the last's end.
pub(crate) fn span_of(events: &[TimeSpan]) -> Option<TimeSpan> {
    Some(TimeSpan::new(events.first()?.start, events.last()?.end))
}
