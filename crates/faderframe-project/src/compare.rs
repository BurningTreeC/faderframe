//! What changed between two versions of a project, in words: tracks added,
//! removed, renamed or moved; their mixer settings, plugins and routing;
//! their clips (added, removed, moved, edited); and the project's tempo,
//! time signatures, key, chords, markers, sections, loop and album.
//! Used to compare a saved version with the project as it is now.

use crate::{Project, Track};
use faderframe_core::gain::format_db;
use std::collections::BTreeSet;

/// What `now` has that differs from `then`, one line each (empty: the
/// same).
pub fn differences(then: &Project, now: &Project) -> Vec<String> {
    let mut out = Vec::new();
    let name = |p: &Project, id| {
        p.track(id)
            .map_or_else(|| "?".to_string(), |t| format!("‘{}’", t.name))
    };
    // Tracks.
    for t in &now.tracks {
        if then.track(t.id).is_none() {
            out.push(format!("Track ‘{}’ added ({})", t.name, t.kind.label()));
        }
    }
    for t in &then.tracks {
        if now.track(t.id).is_none() {
            out.push(format!("Track ‘{}’ removed", t.name));
        }
    }
    let order = |p: &Project| -> Vec<_> {
        p.tracks
            .iter()
            .map(|t| t.id)
            .filter(|id| then.track(*id).is_some() && now.track(*id).is_some())
            .collect()
    };
    if order(then) != order(now) {
        out.push("Tracks reordered".into());
    }
    for a in &then.tracks {
        let Some(b) = now.track(a.id) else { continue };
        track_differences(a, b, now, &mut out);
    }
    // Clips.
    for t in &now.tracks {
        let (mut added, mut removed, mut moved, mut edited) = (0, 0, 0, 0);
        for c in now.clips.values().filter(|c| c.track == t.id) {
            match then.clips.get(&c.id) {
                None => added += 1,
                Some(old) if old.track != c.track => added += 1,
                Some(old) => {
                    if old.start != c.start {
                        moved += 1;
                    }
                    if old.content != c.content || old.muted != c.muted {
                        edited += 1;
                    }
                }
            }
        }
        for c in then.clips.values().filter(|c| c.track == t.id) {
            if now.clips.get(&c.id).is_none_or(|n| n.track != c.track) {
                removed += 1;
            }
        }
        let parts: Vec<String> = [
            (added, "added"),
            (removed, "removed"),
            (moved, "moved"),
            (edited, "edited"),
        ]
        .iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, what)| format!("{n} {what}"))
        .collect();
        if !parts.is_empty() {
            out.push(format!("{}: clips {}", name(now, t.id), parts.join(", ")));
        }
    }
    // The project.
    if then.name != now.name {
        out.push(format!("Project renamed ‘{}’ → ‘{}’", then.name, now.name));
    }
    if then.timeline.tempo != now.timeline.tempo {
        out.push("Tempo changed".into());
    }
    if then.timeline.meter != now.timeline.meter {
        out.push("Time signatures changed".into());
    }
    if then.keys != now.keys {
        out.push("Key track changed".into());
    }
    if then.chords != now.chords {
        out.push("Chord track changed".into());
    }
    if then.markers != now.markers {
        out.push(format!(
            "Markers changed ({} → {})",
            then.markers.len(),
            now.markers.len()
        ));
    }
    if then.sections != now.sections {
        out.push(format!(
            "Sections changed ({} → {})",
            then.sections.len(),
            now.sections.len()
        ));
    }
    if then.loop_range != now.loop_range || then.loop_enabled != now.loop_enabled {
        out.push("Loop changed".into());
    }
    if then.groups != now.groups {
        out.push("Groups changed".into());
    }
    if then.album != now.album {
        out.push("Album changed".into());
    }
    if then.launcher != now.launcher {
        out.push("Clip launcher changed".into());
    }
    if then.video != now.video {
        let clips = |p: &Project| p.video.tracks.iter().map(|t| t.clips.len()).sum::<usize>();
        out.push(format!(
            "Video changed ({} → {} clips)",
            clips(then),
            clips(now)
        ));
    }
    if then.timecode != now.timecode {
        out.push("Timecode changed".into());
    }
    out
}

fn track_differences(a: &Track, b: &Track, now: &Project, out: &mut Vec<String>) {
    let n = format!("‘{}’", b.name);
    let mut push = |what: String| out.push(format!("{n}: {what}"));
    if a.name != b.name {
        push(format!("renamed from ‘{}’", a.name));
    }
    if (a.volume_db - b.volume_db).abs() > 0.01 {
        push(format!(
            "volume {} → {} dB",
            format_db(a.volume_db),
            format_db(b.volume_db)
        ));
    }
    if (a.pan - b.pan).abs() > 0.001 {
        push(format!(
            "pan {} → {}",
            faderframe_core::pan::format_pan(a.pan),
            faderframe_core::pan::format_pan(b.pan)
        ));
    }
    for (was, is, what) in [
        (a.mute, b.mute, "muted"),
        (a.solo, b.solo, "soloed"),
        (a.phase_invert, b.phase_invert, "polarity inverted"),
    ] {
        if was != is {
            push(format!("{}{what}", if is { "" } else { "no longer " }));
        }
    }
    if a.output != b.output {
        let to = match b.output {
            crate::OutputRouting::Track { track } => now
                .track(track)
                .map_or_else(|| "?".into(), |t| format!("‘{}’", t.name)),
            crate::OutputRouting::Master => "the master".into(),
            crate::OutputRouting::Hardware { first_channel } => {
                format!("output {}", first_channel + 1)
            }
            crate::OutputRouting::None => "nothing".into(),
        };
        push(format!("now goes to {to}"));
    }
    if a.color != b.color {
        push("colour changed".into());
    }
    if a.folder != b.folder {
        push("moved to another folder".into());
    }
    // Plugins: by instance.
    let slots = |t: &Track| -> Vec<(faderframe_core::PluginInstanceId, String)> {
        t.preamp
            .iter()
            .chain(t.instrument.iter())
            .chain(&t.inserts)
            .map(|s| {
                (
                    s.id,
                    s.plugin.name.trim_start_matches("FaderFrame ").to_string(),
                )
            })
            .collect()
    };
    let (sa, sb) = (slots(a), slots(b));
    let ids_a: BTreeSet<_> = sa.iter().map(|s| s.0).collect();
    let ids_b: BTreeSet<_> = sb.iter().map(|s| s.0).collect();
    for (id, name) in &sb {
        if !ids_a.contains(id) {
            push(format!("{name} added"));
        }
    }
    for (id, name) in &sa {
        if !ids_b.contains(id) {
            push(format!("{name} removed"));
        }
    }
    let all = |t: &Track| -> Vec<crate::PluginSlot> {
        t.preamp
            .iter()
            .chain(t.instrument.iter())
            .chain(&t.inserts)
            .cloned()
            .collect()
    };
    for old in all(a) {
        if let Some(new) = all(b).into_iter().find(|s| s.id == old.id) {
            let name = new.plugin.name.trim_start_matches("FaderFrame ");
            if old.parameters != new.parameters || old.state != new.state {
                push(format!("{name} settings changed"));
            }
            if old.bypass != new.bypass {
                push(format!(
                    "{name} {}",
                    if new.bypass { "bypassed" } else { "on again" }
                ));
            }
        }
    }
    let order_a: Vec<_> = sa
        .iter()
        .map(|s| s.0)
        .filter(|i| ids_b.contains(i))
        .collect();
    let order_b: Vec<_> = sb
        .iter()
        .map(|s| s.0)
        .filter(|i| ids_a.contains(i))
        .collect();
    if order_a != order_b {
        push("plugins reordered".into());
    }
    if a.sends != b.sends {
        push("sends changed".into());
    }
    if a.automation != b.automation {
        push("automation changed".into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TrackKind;

    #[test]
    fn the_same_project_has_no_differences() {
        let p = crate::demo::demo_project(48_000);
        assert!(differences(&p, &p.clone()).is_empty());
    }

    #[test]
    fn changes_are_named() {
        let then = crate::demo::demo_project(48_000);
        let mut now = then.clone();
        let bass = now.tracks.iter().position(|t| t.name == "Bass").unwrap();
        now.tracks[bass].volume_db = -6.0;
        now.tracks[bass].name = "Low End".into();
        let drums = now.tracks.iter().find(|t| t.name == "Drums").unwrap().id;
        now.tracks.retain(|t| t.id != drums);
        now.clips.retain(|_, c| c.track != drums);
        let id = now.ids.allocate();
        now.tracks.push(Track::new(
            id,
            TrackKind::Audio,
            "Vocals",
            crate::TrackColor::palette(0),
        ));
        let melody = now.clips.values_mut().find(|c| c.name == "Melody").unwrap();
        melody.start += faderframe_timeline::MusicalTime::from_quarters(4.0);
        let d = differences(&then, &now);
        for want in [
            "Track ‘Vocals’ added (Audio)",
            "Track ‘Drums’ removed",
            "‘Low End’: renamed from ‘Bass’",
            "‘Low End’: volume -3.0 → -6.0 dB",
            "‘Lead Synth’: clips 1 moved",
        ] {
            assert!(
                d.iter().any(|l| l.replace('−', "-") == want),
                "{want} in {d:#?}"
            );
        }
    }
}
