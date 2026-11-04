//! Conflicts in words: what a unit is ("Track ‘Bass’", "Clip ‘Melody’ on
//! ‘Lead’", "Note E4 at 2.1.000 in ‘Keys’") and how the two sides differ
//! ("volume: −6.0 dB here, −3.0 dB there").

use super::json::{Brief, Record, Seg};
use super::{ConflictKind, Difference};
use crate::Project;
use faderframe_core::gain::format_db;
use faderframe_timeline::MusicalTime;

const TICKS_PER_QUARTER: i64 = MusicalTime::QUARTER.0;
use serde_json::Value;

/// The projects names are looked up in (theirs with its ids as merged).
pub(crate) struct Names<'a> {
    pub base: &'a Project,
    pub ours: &'a Project,
    pub theirs: &'a Project,
    /// Their JSON: ours, theirs, the base.
    pub json: [&'a Value; 3],
}

/// A field name in words.
pub(crate) fn humanize(field: &str) -> String {
    let words = match field {
        "volume_db" => "volume",
        "gain_db" => "gain",
        "level_db" => "level",
        "source_offset" => "start in the file",
        "start" => "position",
        "phase_invert" => "polarity",
        "record_arm" => "record arm",
        "delay_ms" => "delay",
        "timeline" => "tempo and time signatures",
        "loop_range" | "loop_enabled" => "loop",
        "punch_range" | "punch_enabled" => "punch",
        "play_start" => "play start marker",
        "keys" => "key track",
        "chords" => "chord track",
        "midi_mappings" => "MIDI mappings",
        "adr" => "ADR cue list",
        "ara" => "ARA",
        "pan_law" => "pan law",
        "points" => "points",
        "settings" => "settings",
        other => other,
    };
    words.replace('_', " ")
}

fn capitalised(s: &str) -> String {
    let mut c = s.chars();
    c.next().map_or_else(String::new, |f| {
        f.to_uppercase().collect::<String>() + c.as_str()
    })
}

/// A collection's one thing.
fn singular(collection: &str) -> String {
    match collection {
        "cue_mixes" => "cue mix".into(),
        "pads" => "scratch pad".into(),
        "sources" => "audio file".into(),
        "lanes" => "automation".into(),
        "inserts" => "plug-in".into(),
        "midi_mappings" => "MIDI mapping".into(),
        "articulation_maps" => "articulation map".into(),
        "drum_maps" => "drum map".into(),
        other => {
            let h = humanize(other);
            h.strip_suffix('s').map_or(h.clone(), str::to_string)
        }
    }
}

/// C4 = 60.
pub(crate) fn note_name(key: u64) -> String {
    const NAMES: [&str; 12] = [
        "C", "C♯", "D", "D♯", "E", "F", "F♯", "G", "G♯", "A", "A♯", "B",
    ];
    format!("{}{}", NAMES[(key % 12) as usize], key as i64 / 12 - 1)
}

/// The value at `path` in a project's JSON.
pub(crate) fn lookup<'v>(root: &'v Value, path: &[Seg]) -> Option<&'v Value> {
    let mut at = root;
    for seg in path {
        at = match (seg, at) {
            (Seg::Field(f), Value::Object(m)) => m.get(f)?,
            (Seg::Key(k), Value::Object(m)) => m.get(k)?,
            (Seg::Key(k), Value::Array(list)) => list.iter().find(|v| {
                ["id", "note"].iter().any(|f| {
                    v.get(*f).is_some_and(|x| match x {
                        Value::String(s) => s == k,
                        Value::Number(n) => n.as_u64().is_some_and(|n| k.parse() == Ok(n)),
                        _ => false,
                    })
                })
            })?,
            _ => return None,
        };
    }
    Some(at)
}

impl Names<'_> {
    fn track_name(&self, id: u64) -> Option<String> {
        [self.ours, self.theirs, self.base]
            .iter()
            .find_map(|p| p.track(id.into()).map(|t| format!("‘{}’", t.name)))
    }

    fn bar(&self, ticks: i64) -> String {
        self.ours
            .timeline
            .meter
            .to_bbt(MusicalTime(ticks))
            .to_string()
    }

    /// What the unit at `path` is.
    pub fn what(&self, path: &[Seg], key: &str) -> String {
        let find = |p: &[Seg]| self.json.iter().find_map(|j| lookup(j, p));
        let order = key.ends_with("#order");
        let path = if order {
            &path[..path.len().saturating_sub(1)]
        } else {
            path
        };
        let mut parts: Vec<String> = Vec::new();
        let mut i = 0;
        while i < path.len() {
            match (&path[i], path.get(i + 1)) {
                (Seg::Field(f), Some(Seg::Key(k))) => {
                    let elem = find(&path[..=i + 1]);
                    let name = elem
                        .and_then(|e| e.get("name"))
                        .and_then(Value::as_str)
                        .map(|n| format!("‘{n}’"));
                    let text = match f.as_str() {
                        "tracks" => format!("Track {}", name.unwrap_or_else(|| k.clone())),
                        "clips" => {
                            let on = elem
                                .and_then(|e| e.get("track"))
                                .and_then(Value::as_u64)
                                .and_then(|t| self.track_name(t))
                                .map_or_else(String::new, |t| format!(" on {t}"));
                            format!("Clip {}{on}", name.unwrap_or_default())
                        }
                        "notes" => {
                            let key = elem.and_then(|e| e.get("key")).and_then(Value::as_u64);
                            let start = elem.and_then(|e| e.get("start")).and_then(Value::as_i64);
                            format!(
                                "Note {}{}",
                                key.map_or_else(String::new, note_name),
                                start.map_or_else(String::new, |s| format!(
                                    " {:.2} beats in",
                                    s as f64 / TICKS_PER_QUARTER as f64
                                ))
                            )
                        }
                        "inserts" => elem
                            .and_then(|e| e.pointer("/plugin/name"))
                            .and_then(Value::as_str)
                            .map_or_else(
                                || "Plug-in".into(),
                                |n| n.trim_start_matches("FaderFrame ").to_string(),
                            ),
                        "sends" => {
                            let to = elem
                                .and_then(|e| e.get("target"))
                                .and_then(Value::as_u64)
                                .and_then(|t| self.track_name(t))
                                .unwrap_or_default();
                            format!("Send to {to}")
                        }
                        "lanes" => {
                            let target = elem.and_then(|e| e.get("target"));
                            let what = match target {
                                Some(Value::String(s)) => humanize(s.trim_start_matches("track_")),
                                Some(Value::Object(m)) => {
                                    m.keys().next().map_or_else(String::new, |k| humanize(k))
                                }
                                _ => String::new(),
                            };
                            format!("Automation of {what}")
                        }
                        other => {
                            let one = capitalised(&singular(other));
                            name.map_or_else(|| one.clone(), |n| format!("{one} {n}"))
                        }
                    };
                    parts.push(text);
                    i += 2;
                    continue;
                }
                (Seg::Field(f), _) => {
                    // Fields between things are left out once a thing is
                    // named ("content", "automation").
                    if parts.is_empty() || i + 1 == path.len() {
                        parts.push(capitalised(&humanize(f)));
                    }
                }
                (Seg::Key(k), _) => parts.push(format!("#{k}")),
            }
            i += 1;
        }
        let text = parts.join(" › ");
        if order {
            format!(
                "Order of {}",
                if text.is_empty() {
                    "things".into()
                } else {
                    text
                }
            )
        } else {
            text
        }
    }

    /// One record of a unit as a difference.
    pub fn difference(&self, unit: &[Seg], r: &Record) -> Difference {
        let rest: Vec<&Seg> = r.at.iter().skip(unit.len()).collect();
        let field_name = rest
            .iter()
            .rev()
            .find_map(|s| match s {
                Seg::Field(f) => Some(f.as_str()),
                Seg::Key(_) => None,
            })
            .unwrap_or("");
        let in_notes = r.at.iter().any(|s| *s == Seg::Field("notes".into()));
        let field = match r.kind {
            ConflictKind::Order => "order".to_string(),
            ConflictKind::SamePlace => match r.span {
                Some((a, b)) if a == b => format!("{} at {}", humanize(field_name), self.bar(a)),
                Some((a, b)) => format!(
                    "{} from {} to {}",
                    humanize(field_name),
                    self.bar(a),
                    self.bar(b)
                ),
                None => humanize(field_name),
            },
            _ if rest.is_empty() => "it".to_string(),
            _ => humanize(field_name),
        };
        let say = |b: &Brief, removed: &str| -> String {
            match b {
                Brief::Absent => removed.to_string(),
                Brief::Complex => match r.kind {
                    ConflictKind::Order => "its own order".into(),
                    _ => "changed".into(),
                },
                Brief::Scalar(v) => self.value(field_name, v, in_notes),
            }
        };
        Difference {
            field,
            ours: say(&r.ours, if rest.is_empty() { "removed" } else { "none" }),
            theirs: say(&r.theirs, if rest.is_empty() { "removed" } else { "none" }),
        }
    }

    fn value(&self, field: &str, v: &Value, in_notes: bool) -> String {
        match v {
            Value::Bool(b) => if *b { "on" } else { "off" }.into(),
            Value::Null => "none".into(),
            Value::String(s) => format!("‘{s}’"),
            Value::Number(n) => {
                let f = n.as_f64().unwrap_or(0.0);
                if field.ends_with("_db") {
                    format!("{} dB", format_db(f as f32))
                } else if field == "pan" {
                    faderframe_core::pan::format_pan(f as f32)
                } else if field == "key" && in_notes {
                    note_name(f as u64)
                } else if matches!(field, "start" | "position" | "at" | "time" | "length")
                    && n.is_i64()
                {
                    let ticks = n.as_i64().unwrap_or(0);
                    if in_notes || field == "length" {
                        format!("{:.2} beats", ticks as f64 / TICKS_PER_QUARTER as f64)
                    } else {
                        self.bar(ticks)
                    }
                } else if n.is_f64() {
                    format!("{f:.3}")
                        .trim_end_matches('0')
                        .trim_end_matches('.')
                        .to_string()
                } else {
                    n.to_string()
                }
            }
            _ => "changed".into(),
        }
    }
}
