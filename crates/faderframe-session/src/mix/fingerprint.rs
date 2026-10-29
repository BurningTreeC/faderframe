//! What an analysis depends on, as numbers: the project's fingerprint
//! (everything that can change what is heard) and, finer, each track's.
//!
//! **Why per track is valid.** The one-pass render taps every track where
//! it reaches its destination, *before* it is summed there. A tap's audio
//! therefore depends only on what flows into it — the track itself (its
//! clips and their sources, devices and their state, automation, fader,
//! pan, modulators) and, upstream, everything that reaches it: the tracks
//! routed or sent into it (a bus, a return), the MIDI tracks playing it,
//! the tracks keying its devices' sidechains or its modulators, the
//! plugin whose extra output it takes, the VCAs and folders scaling or
//! muting it — and on what is global (tempo, sample rate, pan law,
//! console, downmix, crosstalk …: the project without its tracks, clips
//! and sources). Nothing downstream changes a tap: a bus compressor, the
//! master chain or another track's fader leave a track's own signal as it
//! was. So a track's fingerprint is the global part plus its upstream
//! closure's tracks, clips and sources, and a change shows only in the
//! tracks it reaches (and the master, whose closure is everything).
//!
//! **The trade-off.** Per-track fingerprints let a later analysis reuse
//! the profiles of unchanged tracks (skipping their measurement) and let
//! findings stay valid when only other tracks changed (a kick/bass
//! finding is not made stale by a vocal edit). They cannot save the whole
//! render: the master's closure is the whole mix, so any change that needs
//! a new measurement renders the mix again — after a first look at the
//! changed tracks alone, a render of their [`upstream`] closures
//! (`super::capture::pruned`). The global part is conservative (anything not
//! in a track invalidates everything), and so is a send counted whether it
//! is enabled or not.

use faderframe_core::TrackId;
use faderframe_mix_intelligence::Fnv;
use faderframe_project::{OutputRouting, Project, TrackKind};
use std::collections::{BTreeSet, HashMap};

/// Top-level project fields that do not change what any track sounds like.
pub(super) const PRESENTATION: [&str; 20] = [
    "name",
    "markers",
    "loop_range",
    "loop_enabled",
    "punch_range",
    "punch_enabled",
    "play_start",
    "midi_mappings",
    "sections",
    "groups",
    "album",
    "lyrics",
    "adr",
    "setlist",
    "scratch_pads",
    "clip_links",
    "video",
    "timecode",
    "ids",
    // Reference tracks are heard instead of the mix, never in it (their
    // comparison has its own key).
    "references",
];
/// Track fields that do not change what it sounds like in a render.
pub(super) const TRACK_PRESENTATION: [&str; 7] = [
    "name",
    "color",
    "solo",
    "role",
    "record_arm",
    "monitor",
    "group",
];

/// The project's fingerprint and each track's (see the module docs).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Fingerprints {
    pub project: u64,
    pub tracks: HashMap<TrackId, u64>,
}

impl Fingerprints {
    /// What `track`'s signal depends on (the project's for one not known,
    /// the master).
    pub fn of(&self, track: TrackId) -> u64 {
        self.tracks.get(&track).copied().unwrap_or(self.project)
    }
}

/// How serde names an id as a map key.
fn key_of<T: serde::Serialize>(id: T) -> Option<String> {
    match serde_json::to_value(id).ok()? {
        serde_json::Value::String(s) => Some(s),
        other => Some(other.to_string()),
    }
}

fn hash_value(h: &mut Fnv, v: &serde_json::Value) {
    // `to_vec` of a value never fails; an empty write keeps it stable.
    let bytes = serde_json::to_vec(v).unwrap_or_default();
    h.write_u64(bytes.len() as u64);
    h.write(&bytes);
}

/// The tracks whose audio reaches `track`'s tap (itself included).
pub(super) fn upstream(project: &Project, track: TrackId) -> BTreeSet<TrackId> {
    let followers = faderframe_engine::modulation::shape(project);
    let mut seen = BTreeSet::new();
    let mut stack = vec![track];
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        let Some(t) = project.track(id) else {
            continue;
        };
        for f in &project.tracks {
            // Routed or sent into it (a send counted whether on or not),
            // playing it (a MIDI track).
            if project.output_target(f) == Some(id)
                || f.sends.iter().any(|s| s.target == id)
                || (f.kind == TrackKind::Midi && f.output == OutputRouting::Track { track: id })
            {
                stack.push(f.id);
            }
        }
        // Keys of its devices' sidechains (nested ones too).
        stack.extend(t.slots().iter().filter_map(|s| s.sidechain));
        // Tracks its modulators follow.
        if let Some((_, keys)) = followers.iter().find(|(x, _)| *x == id) {
            stack.extend(keys.iter().copied());
        }
        // The track hosting the plugin whose extra output it takes.
        stack.extend(project.plugin_output_source(t));
        // A VCA or folder scaling, muting or soloing it.
        stack.extend(t.vca);
        stack.extend(t.folder);
    }
    seen
}

/// The fingerprints of `project` (`None` when it cannot be serialised:
/// nothing is cached or compared then).
pub fn fingerprints(project: &Project) -> Option<Fingerprints> {
    let mut v = serde_json::to_value(project).ok()?;
    let o = v.as_object_mut()?;
    for k in PRESENTATION {
        o.remove(k);
    }
    if let Some(tracks) = o.get_mut("tracks").and_then(|t| t.as_array_mut()) {
        for t in tracks.iter_mut().filter_map(|t| t.as_object_mut()) {
            for k in TRACK_PRESENTATION {
                t.remove(k);
            }
        }
    }
    if let Some(clips) = o.get_mut("clips").and_then(|c| c.as_object_mut()) {
        for c in clips.values_mut().filter_map(|c| c.as_object_mut()) {
            c.remove("name");
            c.remove("color");
        }
    }
    // The whole: what the master (and the old single fingerprint) depends
    // on.
    let whole = {
        let bytes = serde_json::to_vec(&v).ok()?;
        let mut h = Fnv::new();
        h.write(&bytes);
        h.finish()
    };
    let o = v.as_object()?;
    // The global part.
    let global = {
        let mut h = Fnv::new();
        for (k, x) in o {
            if !matches!(k.as_str(), "tracks" | "clips" | "sources") {
                h.write(k.as_bytes());
                hash_value(&mut h, x);
            }
        }
        h.finish()
    };
    // Each track's own part: its JSON, its clips, their sources.
    let track_values: Vec<&serde_json::Value> = o
        .get("tracks")
        .and_then(|t| t.as_array())
        .map(|a| a.iter().collect())
        .unwrap_or_default();
    let clip_values = o.get("clips").and_then(|c| c.as_object());
    let source_values = o.get("sources").and_then(|c| c.as_object());
    let mut own: HashMap<TrackId, u64> = HashMap::new();
    for (t, tv) in project.tracks.iter().zip(&track_values) {
        let mut h = Fnv::new();
        hash_value(&mut h, tv);
        let mut clips: Vec<_> = project.clips.values().filter(|c| c.track == t.id).collect();
        clips.sort_by_key(|c| c.id);
        let mut used = BTreeSet::new();
        for c in clips {
            match key_of(c.id).and_then(|k| clip_values.and_then(|m| m.get(&k))) {
                Some(cv) => hash_value(&mut h, cv),
                None => {
                    if let Ok(cv) = serde_json::to_value(c) {
                        hash_value(&mut h, &cv);
                    }
                }
            }
            used.extend(c.content.referenced_sources());
        }
        if let Some(f) = &t.freeze {
            used.insert(f.source);
        }
        // The sources its clips (takes, its freeze) play: each by its own
        // entry.
        for s in used {
            match key_of(s).and_then(|k| source_values.and_then(|m| m.get(&k))) {
                Some(sv) => hash_value(&mut h, sv),
                None => {
                    if let Some(src) = project.sources.get(&s)
                        && let Ok(sv) = serde_json::to_value(src)
                    {
                        hash_value(&mut h, &sv);
                    }
                }
            }
        }
        own.insert(t.id, h.finish());
    }
    let mut tracks = HashMap::new();
    for t in &project.tracks {
        let mut h = Fnv::new();
        h.write_u64(global);
        for u in upstream(project, t.id) {
            h.write_u64(u.0);
            h.write_u64(own.get(&u).copied().unwrap_or(0));
        }
        tracks.insert(t.id, h.finish());
    }
    // The master's is the whole project's.
    if let Some(m) = project.master_id() {
        tracks.insert(m, whole);
    }
    Some(Fingerprints {
        project: whole,
        tracks,
    })
}

/// Pairs (track, bus or return) where the track's audio reaches the bus
/// by its output or a send: never compared for masking (the bus carries
/// the track, or its effect).
pub(super) fn related(project: &Project, tracks: &[TrackId]) -> Vec<(TrackId, TrackId)> {
    let edges = project.routing_edges();
    let reaches = |from: TrackId, to: TrackId| {
        let mut stack = vec![from];
        let mut seen = BTreeSet::new();
        while let Some(n) = stack.pop() {
            if n == to && n != from {
                return true;
            }
            if !seen.insert(n) {
                continue;
            }
            stack.extend(edges.iter().filter(|(a, _)| *a == n).map(|(_, b)| *b));
        }
        false
    };
    let mut out = Vec::new();
    for (i, &a) in tracks.iter().enumerate() {
        for &b in &tracks[i + 1..] {
            if reaches(a, b) || reaches(b, a) {
                out.push((a, b));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use faderframe_project::demo::demo_project;

    #[test]
    fn a_change_shows_in_the_tracks_it_reaches_only() {
        let p = demo_project(48_000);
        let fp = fingerprints(&p).unwrap_or_else(|| panic!("serialisable"));
        // Presentation changes nothing.
        let mut q = p.clone();
        q.tracks[0].name = "Renamed".into();
        q.tracks[0].solo = true;
        q.name = "Other".into();
        assert_eq!(fingerprints(&q), Some(fp.clone()));
        // A track's fader: it, what it reaches and the master change; a
        // track it does not reach keeps its fingerprint.
        let changed = p
            .tracks
            .iter()
            .find(|t| matches!(t.kind, TrackKind::Audio | TrackKind::Instrument))
            .map(|t| t.id)
            .unwrap_or_else(|| panic!("an audio track"));
        let mut q = p.clone();
        if let Some(t) = q.track_mut(changed) {
            t.volume_db += 1.0;
        }
        let after = fingerprints(&q).unwrap_or_else(|| panic!("serialisable"));
        assert_ne!(after.project, fp.project);
        assert_ne!(after.of(changed), fp.of(changed));
        for t in &p.tracks {
            let reached = upstream(&p, t.id).contains(&changed);
            assert_eq!(
                after.of(t.id) != fp.of(t.id),
                reached || Some(t.id) == p.master_id(),
                "{}",
                t.name
            );
        }
        assert!(
            p.tracks.iter().any(|t| after.of(t.id) == fp.of(t.id)),
            "some tracks are untouched"
        );
    }

    #[test]
    fn buses_carry_what_reaches_them() {
        let p = demo_project(48_000);
        let all: Vec<TrackId> = p.tracks.iter().map(|t| t.id).collect();
        let pairs = related(&p, &all);
        for (a, b) in &pairs {
            let edges = p.routing_edges();
            assert!(edges.iter().any(|(x, _)| x == a || x == b), "{a} {b}");
        }
        // Every track routed into a bus is related to it.
        for t in &p.tracks {
            if let Some(d) = p.output_target(t)
                && Some(d) != p.master_id()
            {
                assert!(pairs.contains(&(t.id, d)) || pairs.contains(&(d, t.id)));
            }
        }
    }
}
