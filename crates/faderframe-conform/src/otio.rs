//! OpenTimelineIO timelines (`.otio`, JSON): a `Timeline` holds a `Stack`
//! of `Track`s (kind "Video" or "Audio"), each a sequence of `Clip`s,
//! `Gap`s and `Transition`s. A clip's record position is where the items
//! before it end (transitions take no time of their own), from the
//! timeline's `global_start_time`; its source span is its `source_range`;
//! its source is its media reference's file (else the clip's name).
//! Times are `RationalTime`s (`value` at `rate`).

use crate::{ConformError, CutList, Event, Kind};
use serde_json::Value;

fn err(m: impl Into<String>) -> ConformError {
    ConformError::Otio(m.into())
}

/// Seconds of a `RationalTime`.
fn seconds(t: &Value) -> Option<f64> {
    let rate = t.get("rate")?.as_f64()?;
    let value = t.get("value")?.as_f64()?;
    (rate > 0.0).then(|| value / rate)
}

/// A `TimeRange`'s start and duration (seconds).
fn range(r: &Value) -> Option<(f64, f64)> {
    Some((seconds(r.get("start_time")?)?, seconds(r.get("duration")?)?))
}

fn schema(v: &Value) -> &str {
    v.get("OTIO_SCHEMA")
        .and_then(Value::as_str)
        .and_then(|s| s.split('.').next())
        .unwrap_or("")
}

/// The file a clip's media reference names (its last path part, decoded
/// from a URL), if any.
fn media_file(clip: &Value) -> Option<String> {
    let reference = match clip.get("media_references") {
        // Clip.2: several references, one active.
        Some(Value::Object(refs)) => {
            let key = clip
                .get("active_media_reference_key")
                .and_then(Value::as_str)
                .unwrap_or("DEFAULT_MEDIA");
            refs.get(key).or_else(|| refs.values().next())?
        }
        _ => clip.get("media_reference")?,
    };
    let url = reference.get("target_url")?.as_str()?;
    let path = url.strip_prefix("file://").unwrap_or(url);
    let name = path.rsplit(['/', '\\']).next()?.to_string();
    Some(percent_decoded(&name))
}

fn percent_decoded(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Read an OpenTimelineIO timeline.
pub fn parse(text: &str) -> Result<CutList, ConformError> {
    let root: Value = serde_json::from_str(text).map_err(|e| err(e.to_string()))?;
    if schema(&root) != "Timeline" {
        return Err(err("not a Timeline"));
    }
    let start = root
        .get("global_start_time")
        .and_then(seconds)
        .unwrap_or(0.0);
    let stack = root
        .get("tracks")
        .ok_or_else(|| err("a timeline without tracks"))?;
    let tracks = stack
        .get("children")
        .and_then(Value::as_array)
        .ok_or_else(|| err("tracks without children"))?;
    let mut list = CutList {
        title: root.get("name").and_then(Value::as_str).map(str::to_string),
        events: Vec::new(),
    };
    let (mut videos, mut audios) = (0, 0);
    for t in tracks {
        if schema(t) != "Track" {
            continue;
        }
        let kind = match t.get("kind").and_then(Value::as_str) {
            Some("Audio") => Kind::Audio,
            _ => Kind::Video,
        };
        let track = match kind {
            Kind::Video => {
                videos += 1;
                videos - 1
            }
            Kind::Audio => {
                audios += 1;
                audios - 1
            }
        };
        let mut at = start;
        for item in t
            .get("children")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            match schema(item) {
                "Transition" => {}
                "Gap" => {
                    if let Some((_, d)) = item.get("source_range").and_then(range) {
                        at += d;
                    }
                }
                // Clips, and nested stacks or tracks taken whole.
                _ => {
                    let Some((src_in, d)) = item.get("source_range").and_then(range) else {
                        continue;
                    };
                    let name = item.get("name").and_then(Value::as_str).map(str::to_string);
                    let source = media_file(item)
                        .or_else(|| name.clone())
                        .unwrap_or_default();
                    if d > 0.0 {
                        list.events.push(Event {
                            kind,
                            track,
                            source,
                            name,
                            source_in: src_in,
                            source_out: src_in + d,
                            record_in: at,
                            record_out: at + d,
                            speed: 1.0,
                        });
                    }
                    at += d;
                }
            }
        }
    }
    Ok(list)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rt(v: f64) -> String {
        format!(r#"{{"OTIO_SCHEMA":"RationalTime.1","rate":24.0,"value":{v}}}"#)
    }

    fn tr(start: f64, d: f64) -> String {
        format!(
            r#"{{"OTIO_SCHEMA":"TimeRange.1","start_time":{},"duration":{}}}"#,
            rt(start),
            rt(d)
        )
    }

    #[test]
    fn clips_gaps_and_transitions() {
        let text = format!(
            r#"{{"OTIO_SCHEMA":"Timeline.1","name":"Reel 2","global_start_time":{},
              "tracks":{{"OTIO_SCHEMA":"Stack.1","children":[
                {{"OTIO_SCHEMA":"Track.1","kind":"Video","children":[
                  {{"OTIO_SCHEMA":"Clip.1","name":"shot 1","source_range":{},
                    "media_reference":{{"OTIO_SCHEMA":"ExternalReference.1","target_url":"file:///media/A001%20C003.mov"}}}},
                  {{"OTIO_SCHEMA":"Transition.1","in_offset":{},"out_offset":{}}},
                  {{"OTIO_SCHEMA":"Gap.1","source_range":{}}},
                  {{"OTIO_SCHEMA":"Clip.2","name":"shot 2","source_range":{},
                    "media_references":{{"DEFAULT_MEDIA":{{"OTIO_SCHEMA":"ExternalReference.1","target_url":"file:///media/B.mov"}}}},
                    "active_media_reference_key":"DEFAULT_MEDIA"}}
                ]}},
                {{"OTIO_SCHEMA":"Track.1","kind":"Audio","children":[
                  {{"OTIO_SCHEMA":"Clip.1","name":"dialogue","source_range":{}}}
                ]}}
              ]}}}}"#,
            rt(86_400.0),
            tr(240.0, 48.0),
            rt(6.0),
            rt(6.0),
            tr(0.0, 24.0),
            tr(1000.0, 72.0),
            tr(0.0, 120.0),
        );
        let l = parse(&text).unwrap();
        assert_eq!(l.title.as_deref(), Some("Reel 2"));
        let v = l.track(Kind::Video, 0);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].source, "A001 C003.mov");
        assert_eq!(
            (v[0].source_in, v[0].record_in, v[0].record_out),
            (10.0, 3600.0, 3602.0)
        );
        // After the gap (a second); the transition took no time.
        assert_eq!(v[1].source, "B.mov");
        assert_eq!(v[1].record_in, 3603.0);
        assert_eq!(l.track(Kind::Audio, 0)[0].source, "dialogue");
    }

    #[test]
    fn not_a_timeline() {
        assert!(parse(r#"{"OTIO_SCHEMA":"Clip.1"}"#).is_err());
        assert!(parse("{").is_err());
    }
}
