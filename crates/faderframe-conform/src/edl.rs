//! CMX3600 EDLs, as the picture editors write them:
//!
//! ```text
//! TITLE: REEL 1 V3
//! FCM: NON-DROP FRAME
//!
//! 001  A001C003 V     C        01:00:10:00 01:00:15:00 01:00:00:00 01:00:05:00
//! * FROM CLIP NAME: A001C003.MOV
//! 002  A002C001 AA/V  D    012 01:20:00:00 01:20:04:00 01:00:05:00 01:00:09:00
//! M2   A002C001       050.0                01:20:00:00
//! ```
//!
//! Events are number, reel, channels (V, A, A2, AA, AA/V, B…), transition
//! (C cut, D dissolve, Wnnn wipe, K key; with a duration), source in and
//! out, record in and out. A dissolve's incoming event starts where the
//! transition does (the outgoing clip's line before it ends there). `M2`
//! lines give an event's speed; `* FROM CLIP NAME:` and `* SOURCE FILE:`
//! comments name its clip. Frames count at the rate the caller gives
//! (EDLs do not say it); `FCM: DROP FRAME` makes it drop-frame.

use crate::{ConformError, CutList, Event, Kind};
use faderframe_core::timecode::{FrameRate, Timecode};

/// Read an EDL whose timecodes count at `rate`.
pub fn parse(text: &str, rate: FrameRate) -> Result<CutList, ConformError> {
    let mut list = CutList::default();
    let mut rate = rate;
    let seconds = |tc: &str, rate: FrameRate, line: usize| -> Result<f64, ConformError> {
        let t = Timecode::parse(tc, rate).ok_or_else(|| ConformError::Edl {
            line,
            message: format!("not a timecode: {tc}"),
        })?;
        Ok(rate.seconds_of(t.total_frames(rate)))
    };
    // The events of the line being read (one per channel kind).
    let mut last: Vec<usize> = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let line = i + 1;
        let l = raw.trim();
        if l.is_empty() {
            continue;
        }
        if let Some(t) = l.strip_prefix("TITLE:") {
            list.title = Some(t.trim().to_string());
            continue;
        }
        if let Some(f) = l.strip_prefix("FCM:") {
            let drop = f.trim().to_ascii_uppercase().starts_with("DROP");
            rate = rate.with_drop(drop);
            continue;
        }
        if let Some(c) = l.strip_prefix('*') {
            let c = c.trim();
            let named = c
                .strip_prefix("FROM CLIP NAME:")
                .or_else(|| c.strip_prefix("SOURCE FILE:"))
                .map(str::trim);
            if let Some(name) = named {
                for &e in &last {
                    let ev: &mut Event = &mut list.events[e];
                    if c.starts_with("SOURCE FILE:") {
                        ev.source = name.to_string();
                    } else if ev.name.is_none() {
                        ev.name = Some(name.to_string());
                    }
                }
            }
            continue;
        }
        let fields: Vec<&str> = l.split_whitespace().collect();
        if fields.first() == Some(&"M2") {
            // M2 reel speed(fps) source-in: the event's speed.
            if let Some(fps) = fields.get(2).and_then(|f| f.parse::<f64>().ok()) {
                let speed = fps / rate.fps();
                for &e in &last {
                    let ev: &mut Event = &mut list.events[e];
                    ev.speed = speed;
                    ev.source_out = ev.source_in + ev.length() * speed;
                }
            }
            continue;
        }
        if !fields
            .first()
            .is_some_and(|f| f.chars().all(|c| c.is_ascii_digit()))
        {
            // Other notes (SPLIT:, AUD, …) are left alone.
            continue;
        }
        // number reel channels transition [duration] 4 timecodes
        if fields.len() < 8 {
            return Err(ConformError::Edl {
                line,
                message: "an event needs reel, channels, transition and four timecodes".into(),
            });
        }
        let tcs = &fields[fields.len() - 4..];
        let (reel, channels) = (fields[1], fields[2].to_ascii_uppercase());
        let source_in = seconds(tcs[0], rate, line)?;
        let source_out = seconds(tcs[1], rate, line)?;
        let record_in = seconds(tcs[2], rate, line)?;
        let record_out = seconds(tcs[3], rate, line)?;
        last.clear();
        if record_out <= record_in {
            // The outgoing side of a dissolve: nothing of its own.
            continue;
        }
        for (kind, track) in tracks(&channels) {
            last.push(list.events.len());
            list.events.push(Event {
                kind,
                track,
                source: reel.to_string(),
                name: None,
                source_in,
                source_out,
                record_in,
                record_out,
                speed: 1.0,
            });
        }
    }
    Ok(list)
}

/// The tracks a channels field names: "V", "A", "A2", "AA" (A1 and A2),
/// "AA/V", "B" (V and A1), "A3"…
fn tracks(channels: &str) -> Vec<(Kind, usize)> {
    let mut out = Vec::new();
    for part in channels.split('/') {
        match part {
            "V" => out.push((Kind::Video, 0)),
            "B" => {
                out.push((Kind::Video, 0));
                out.push((Kind::Audio, 0));
            }
            "A" => out.push((Kind::Audio, 0)),
            "AA" => {
                out.push((Kind::Audio, 0));
                out.push((Kind::Audio, 1));
            }
            p if p.starts_with('A') => {
                if let Ok(n) = p[1..].parse::<usize>()
                    && n >= 1
                {
                    out.push((Kind::Audio, n - 1));
                }
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const EDL: &str = "TITLE: SCENE 12 V4
FCM: NON-DROP FRAME

001  A001C003 V     C        01:00:10:00 01:00:15:00 01:00:00:00 01:00:05:00
* FROM CLIP NAME: A001C003.MOV
002  A002C001 AA/V  C        01:20:00:00 01:20:04:12 01:00:05:00 01:00:09:12
003  A002C001 V     C        01:20:04:12 01:20:04:12 01:00:09:12 01:00:09:12
003  A003C007 V     D    012 00:10:00:00 00:10:02:00 01:00:09:12 01:00:11:12
M2   A003C007       050.0                00:10:00:00
";

    #[test]
    fn events_tracks_names_dissolves_and_speed() {
        let l = parse(EDL, FrameRate::Fps25).unwrap();
        assert_eq!(l.title.as_deref(), Some("SCENE 12 V4"));
        let v = l.track(Kind::Video, 0);
        assert_eq!(v.len(), 3, "the dissolve's outgoing side has no length");
        assert_eq!(v[0].name.as_deref(), Some("A001C003.MOV"));
        assert_eq!(v[0].record_in, 3600.0);
        assert_eq!(v[0].source_in, 3610.0);
        assert!((v[1].record_out - (3609.0 + 12.0 / 25.0)).abs() < 1e-9);
        // The dissolve's incoming shot at double speed.
        assert_eq!(v[2].source, "A003C007");
        assert!((v[2].speed - 2.0).abs() < 1e-9);
        assert!((v[2].source_out - v[2].source_in - 4.0).abs() < 1e-9);
        assert_eq!(l.track(Kind::Audio, 0).len(), 1);
        assert_eq!(l.track(Kind::Audio, 1).len(), 1);
    }

    #[test]
    fn drop_frame_counts_drop_frame() {
        let l = parse(
            "FCM: DROP FRAME\n001  R1 V C 00:01:00;02 00:01:00;12 01:00:00;00 01:00:00;10\n",
            FrameRate::Fps2997,
        )
        .unwrap();
        let e = &l.events[0];
        // 00:01:00;02 is frame 1800 in drop-frame counting.
        assert!((e.source_in - 1800.0 * 1001.0 / 30_000.0).abs() < 1e-9);
    }

    #[test]
    fn a_broken_event_says_where() {
        let e = parse("001 R1 V C 01:00:00:00\n", FrameRate::Fps25).unwrap_err();
        assert!(e.to_string().contains("line 1"), "{e}");
    }
}
