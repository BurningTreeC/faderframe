//! What a track's chain output depends on (see the module docs of
//! [`super`]): the tracks reaching its chain, a view of everything of
//! theirs that shapes it, compared with the project in place on every
//! sync, and a key (a hash of that view) naming the cache file.
//!
//! **What reaches a chain.** The track itself (its clips and their media,
//! its devices and their state, the automation and modulators driving
//! them), the MIDI tracks playing it (their notes, MIDI effects, mute —
//! folders and solo included — and its automation), and the tracks
//! keying its devices' sidechains or its modulators' followers — those
//! tap a source after its devices, so their chains, recursively, but
//! never their faders. Globally: the tempo map and signatures, the key
//! and chord track (MIDI effects follow them), each track's tuning (the
//! key table it plays and how its plugins are retuned), the project rate
//! (clip positions are in its frames), the engine rate (the cache is
//! rendered at it) and, where a third-party plug-in is among the chains,
//! the real time renders give such plug-ins before their first frame
//! (`faderframe_engine::offline::plugin_warm_up_ms`: what one preparing
//! its state in the background plays first). Nothing after the chain
//! counts: fader, pan, sends, mute, solo and polarity of the track stay
//! live.
//!
//! Every comparison destructures [`Track`] and [`Clip`] field by field,
//! so a field added later must be placed on one side or the other before
//! this compiles.

use faderframe_automation::AutomationTarget;
use faderframe_core::TrackId;
use faderframe_mix_intelligence::Fnv;
use faderframe_project::modulation::{FollowSource, ModSource};
use faderframe_project::{
    AudioSource, ChordEvent, Clip, KeyChange, OutputRouting, PluginFormat, Project, SourceSpec,
    Track, TrackColor, TrackKind,
};
use faderframe_timeline::Timeline;
use serde::Serialize;
use std::collections::{BTreeSet, HashSet};

/// Changes when what a key covers (or how a cache is made) changes:
/// older caches are not taken for newer ones. 2: third-party plug-ins
/// get a warm-up before renders (caches made without one may start
/// silent).
pub(crate) const KEY_VERSION: u64 = 2;

/// The tracks whose signal reaches `track`'s chain (itself included),
/// sorted by id.
pub(crate) fn chain_closure(project: &Project, track: TrackId) -> Vec<TrackId> {
    let mut seen = BTreeSet::new();
    let mut stack = vec![track];
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        let Some(t) = project.track(id) else {
            continue;
        };
        // MIDI tracks playing it.
        stack.extend(
            project
                .tracks
                .iter()
                .filter(|f| {
                    f.kind == TrackKind::Midi && f.output == OutputRouting::Track { track: id }
                })
                .map(|f| f.id),
        );
        // The keys of its devices' sidechains (nested ones too).
        stack.extend(t.slots().iter().filter_map(|s| s.sidechain));
        // The tracks its modulators follow.
        for m in &t.modulators {
            if let ModSource::Follower {
                source: FollowSource::Track { track },
                ..
            } = m.source
            {
                stack.push(track);
            }
        }
    }
    seen.into_iter().collect()
}

/// Whether a lane changes what a chain makes (`midi`: of a MIDI track,
/// whose mute decides which notes reach the instrument).
fn drives_chain(target: &AutomationTarget, midi: bool) -> bool {
    use AutomationTarget as A;
    match target {
        A::PluginParameter { .. } | A::PluginBypass(_) => true,
        A::TrackMute => midi,
        A::TrackVolume
        | A::TrackPan
        | A::SendLevel(_)
        | A::Surround(_)
        | A::DualPanLeft
        | A::DualPanRight
        | A::CueSend { .. }
        | A::CueLevel { .. } => midi,
    }
}

/// A MIDI track's notes are held back (its mute, its folders', solo).
pub(crate) fn midi_muted(project: &Project, t: &Track, solo: Option<&HashSet<TrackId>>) -> bool {
    t.kind == TrackKind::Midi
        && (t.mute
            || project.folder_chain(t).iter().any(|f| f.mute)
            || solo.is_some_and(|s| !s.contains(&t.id)))
}

/// `t` with only what shapes its chain's output kept (the rest as a new
/// track has it).
fn normalized(t: &Track, midi: bool) -> Track {
    let Track {
        id,
        kind,
        name: _,
        color: _,
        layout,
        clips: _,
        instrument,
        preamp,
        inserts,
        sends: _,
        cue_sends: _,
        input,
        output: _,
        volume_db: _,
        pan: _,
        pan_mode: _,
        dual_pan: _,
        surround: _,
        object: _,
        binaural: _,
        phase_invert: _,
        delay_ms: _,
        mute: _,
        solo: _,
        record_arm: _,
        midi_output: _,
        mpe,
        monitor: _,
        automation,
        freeze,
        vca: _,
        group: _,
        folder: _,
        folder_bus: _,
        folder_group: _,
        ripple_excluded: _,
        time_base: _,
        role: _,
        modulators,
        containers,
        no_self_freeze: _,
        // Kept as the maps' contents in `TrackPart`: what they send.
        articulation_map: _,
        drum_map: _,
        // The table played is the part's `tuning` (its own or the
        // project's, with the reference pitch).
        tuning: _,
        retune,
    } = t;
    let mut n = Track::new(*id, *kind, "", TrackColor::palette(0));
    n.layout = *layout;
    n.instrument = instrument.clone();
    n.preamp = preamp.clone();
    n.inserts = inserts.clone();
    n.input = input.clone();
    n.mpe = *mpe;
    n.automation.lanes = automation
        .lanes
        .iter()
        .filter(|l| drives_chain(&l.target, midi))
        .cloned()
        .collect();
    // A key source's freeze is what it plays.
    n.freeze = freeze.clone();
    n.modulators = modulators.clone();
    n.containers = containers.clone();
    n.retune = *retune;
    n
}

/// Whether `t` shapes its chain as `kept` (from [`normalized`]) does.
fn same_track(kept: &Track, t: &Track, midi: bool) -> bool {
    let Track {
        id,
        kind,
        name: _,
        color: _,
        layout,
        clips: _,
        instrument,
        preamp,
        inserts,
        sends: _,
        cue_sends: _,
        input,
        output: _,
        volume_db: _,
        pan: _,
        pan_mode: _,
        dual_pan: _,
        surround: _,
        object: _,
        binaural: _,
        phase_invert: _,
        delay_ms: _,
        mute: _,
        solo: _,
        record_arm: _,
        midi_output: _,
        mpe,
        monitor: _,
        automation,
        freeze,
        vca: _,
        group: _,
        folder: _,
        folder_bus: _,
        folder_group: _,
        ripple_excluded: _,
        time_base: _,
        role: _,
        modulators,
        containers,
        no_self_freeze: _,
        // Kept as the maps' contents in `TrackPart`: what they send.
        articulation_map: _,
        drum_map: _,
        // The table played is the part's `tuning` (its own or the
        // project's, with the reference pitch).
        tuning: _,
        retune,
    } = t;
    let lanes = automation
        .lanes
        .iter()
        .filter(|l| drives_chain(&l.target, midi));
    kept.id == *id
        && kept.kind == *kind
        && kept.layout == *layout
        && kept.instrument == *instrument
        && kept.preamp == *preamp
        && kept.inserts == *inserts
        && kept.input == *input
        && kept.mpe == *mpe
        && kept.freeze == *freeze
        && kept.modulators == *modulators
        && kept.containers == *containers
        && kept.retune == *retune
        && kept.automation.lanes.len() == lanes.clone().count()
        && kept.automation.lanes.iter().zip(lanes).all(|(a, b)| a == b)
}

fn normalized_clip(c: &Clip) -> Clip {
    let Clip {
        id,
        track,
        name: _,
        color: _,
        start,
        muted,
        content,
    } = c;
    Clip {
        id: *id,
        track: *track,
        name: String::new(),
        color: None,
        start: *start,
        muted: *muted,
        content: content.clone(),
    }
}

fn same_clip(kept: &Clip, c: &Clip) -> bool {
    let Clip {
        id,
        track,
        name: _,
        color: _,
        start,
        muted,
        content,
    } = c;
    kept.id == *id
        && kept.track == *track
        && kept.start == *start
        && kept.muted == *muted
        && kept.content == *content
}

/// A source as its audio is: a file by its name (a save moves media into
/// the project folder; names there are never reused).
fn normalized_source(s: &AudioSource) -> AudioSource {
    AudioSource {
        id: s.id,
        name: String::new(),
        spec: match &s.spec {
            SourceSpec::File {
                path,
                channels,
                frames,
                sample_rate,
            } => SourceSpec::File {
                path: path.file_name().map(Into::into).unwrap_or_default(),
                channels: *channels,
                frames: *frames,
                sample_rate: *sample_rate,
            },
            g @ SourceSpec::Generated { .. } => g.clone(),
        },
    }
}

fn same_source(kept: &AudioSource, s: &AudioSource) -> bool {
    kept.id == s.id
        && match (&kept.spec, &s.spec) {
            (
                SourceSpec::File {
                    path: a,
                    channels: ca,
                    frames: fa,
                    sample_rate: ra,
                },
                SourceSpec::File {
                    path: b,
                    channels: cb,
                    frames: fb,
                    sample_rate: rb,
                },
            ) => Some(a.as_os_str()) == b.file_name() && ca == cb && fa == fb && ra == rb,
            (a @ SourceSpec::Generated { .. }, b @ SourceSpec::Generated { .. }) => a == b,
            _ => false,
        }
}

/// One track of a chain's closure as it was.
#[derive(Clone, Debug, PartialEq, Serialize)]
struct TrackPart {
    track: Track,
    midi_muted: bool,
    /// In the track's order.
    clips: Vec<Clip>,
    sources: Vec<AudioSource>,
    /// The maps its notes play through (keyswitches, remapped keys).
    articulation_map: Option<faderframe_project::articulation::ArticulationMap>,
    drum_map: Option<faderframe_project::drum_map::DrumMap>,
    /// The fingerprint of the key table it plays (see
    /// `faderframe_project::tuning`).
    tuning: u64,
}

/// Everything a track's chain output depends on, as it was when its cache
/// was made (see the module docs).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct ChainView {
    rate: u32,
    project_rate: u32,
    timeline: Timeline,
    keys: Vec<KeyChange>,
    chords: Vec<ChordEvent>,
    tracks: Vec<TrackPart>,
    /// The plug-ins' warm-up a render of these chains takes, in
    /// milliseconds (0 without third-party plug-ins: they never wait).
    warm_up_ms: u32,
}

/// The warm-up a render of `tracks`' chains takes (see [`ChainView`]).
fn warm_up_of<'a>(tracks: impl IntoIterator<Item = &'a Track>, ms: u32) -> u32 {
    let third_party = tracks.into_iter().any(|t| {
        t.slots()
            .iter()
            .any(|s| s.plugin.format != PluginFormat::Builtin)
    });
    if third_party { ms } else { 0 }
}

impl ChainView {
    /// `track`'s chain in `project`, rendered at `rate`.
    pub(crate) fn of(project: &Project, track: TrackId, rate: u32) -> Self {
        let solo = project.solo_audible();
        let closure: Vec<&Track> = chain_closure(project, track)
            .into_iter()
            .filter_map(|id| project.track(id))
            .collect();
        let warm_up_ms = warm_up_of(
            closure.iter().copied(),
            faderframe_engine::offline::plugin_warm_up_ms(),
        );
        let tracks = closure
            .into_iter()
            .map(|t| {
                let midi = t.kind == TrackKind::Midi;
                let clips: Vec<Clip> = t
                    .clips
                    .iter()
                    .filter_map(|c| project.clips.get(c))
                    .map(normalized_clip)
                    .collect();
                let mut used: Vec<_> = clips
                    .iter()
                    .flat_map(|c| c.content.referenced_sources())
                    .chain(t.freeze.as_ref().map(|f| f.source))
                    .collect();
                used.sort();
                used.dedup();
                TrackPart {
                    track: normalized(t, midi),
                    midi_muted: midi_muted(project, t, solo.as_ref()),
                    tuning: project.tuning_table(t.id).fingerprint(),
                    clips,
                    sources: used
                        .iter()
                        .filter_map(|s| project.sources.get(s))
                        .map(normalized_source)
                        .collect(),
                    articulation_map: project.track_articulation_map(t.id).cloned(),
                    drum_map: project.track_drum_map(t.id).cloned(),
                }
            })
            .collect();
        Self {
            rate,
            project_rate: project.sample_rate,
            timeline: project.timeline.clone(),
            keys: project.keys.clone(),
            chords: project.chords.clone(),
            tracks,
            warm_up_ms,
        }
    }

    /// Whether `project` (at engine rate `rate`) would still make what
    /// this view made: compared in place, nothing cloned.
    pub(crate) fn holds(&self, project: &Project, track: TrackId, rate: u32) -> bool {
        if self.rate != rate
            || self.project_rate != project.sample_rate
            || self.timeline != project.timeline
            || self.keys != project.keys
            || self.chords != project.chords
        {
            return false;
        }
        let closure = chain_closure(project, track);
        if closure.len() != self.tracks.len()
            || closure
                .iter()
                .zip(&self.tracks)
                .any(|(id, p)| *id != p.track.id)
        {
            return false;
        }
        let warm_up = warm_up_of(
            closure.iter().filter_map(|id| project.track(*id)),
            faderframe_engine::offline::plugin_warm_up_ms(),
        );
        if self.warm_up_ms != warm_up {
            return false;
        }
        let solo = project.solo_audible();
        self.tracks.iter().all(|p| {
            let Some(t) = project.track(p.track.id) else {
                return false;
            };
            let midi = t.kind == TrackKind::Midi;
            same_track(&p.track, t, midi)
                && p.midi_muted == midi_muted(project, t, solo.as_ref())
                && p.articulation_map.as_ref() == project.track_articulation_map(t.id)
                && p.drum_map.as_ref() == project.track_drum_map(t.id)
                && p.tuning == project.tuning_table(t.id).fingerprint()
                && t.clips.len() == p.clips.len()
                && t.clips
                    .iter()
                    .zip(&p.clips)
                    .all(|(id, kept)| project.clips.get(id).is_some_and(|c| same_clip(kept, c)))
                && p.sources.iter().all(|kept| {
                    project
                        .sources
                        .get(&kept.id)
                        .is_some_and(|s| same_source(kept, s))
                })
        })
    }

    /// The cache's name: a hash of the view.
    pub(crate) fn key(&self) -> u64 {
        struct Hashing(Fnv);
        impl std::io::Write for Hashing {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.write(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut h = Hashing(Fnv::new());
        h.0.write_u64(KEY_VERSION);
        // Serialising plain data into a hasher does not fail; were it to,
        // the key would still be a key (of what was written).
        let _ = serde_json::to_writer(&mut h, self);
        h.0.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use faderframe_project::demo::demo_project;

    fn audio_track(p: &Project) -> TrackId {
        p.tracks
            .iter()
            .find(|t| t.kind == TrackKind::Audio && !t.clips.is_empty())
            .map(|t| t.id)
            .unwrap_or_else(|| panic!("an audio track with clips"))
    }

    #[test]
    fn faders_and_names_leave_a_chain_alone() {
        let p = demo_project(48_000);
        let t = audio_track(&p);
        let view = ChainView::of(&p, t, 48_000);
        let key = view.key();
        let mut q = p.clone();
        if let Some(tr) = q.track_mut(t) {
            tr.volume_db -= 6.0;
            tr.pan = 0.3;
            tr.mute = true;
            tr.name = "Renamed".into();
            tr.phase_invert = true;
        }
        assert!(view.holds(&q, t, 48_000));
        assert_eq!(ChainView::of(&q, t, 48_000).key(), key);
        // Another rate, another cache.
        assert!(!view.holds(&q, t, 44_100));
        assert_ne!(ChainView::of(&q, t, 44_100).key(), key);
    }

    #[test]
    fn the_warm_up_counts_where_a_third_party_plug_in_plays() {
        let p = demo_project(48_000);
        let t = audio_track(&p);
        let mut track = p.track(t).cloned().unwrap_or_else(|| panic!("the track"));
        track
            .inserts
            .retain(|s| s.plugin.format == PluginFormat::Builtin);
        assert_eq!(warm_up_of([&track], 2_000), 0, "built-ins never wait");
        track.inserts.push(faderframe_project::PluginSlot {
            id: faderframe_core::PluginInstanceId(9_999),
            plugin: faderframe_project::PluginRef {
                format: PluginFormat::Vst3,
                id: "an.amp".into(),
                name: "Amp".into(),
            },
            bypass: false,
            parameters: Vec::new(),
            state: None,
            sidechain: None,
            mono_to_stereo: false,
            mix_origin: None,
        });
        assert_eq!(warm_up_of([&track], 2_000), 2_000);
        assert_eq!(warm_up_of([&track], 0), 0);
    }

    #[test]
    fn clips_devices_and_tempo_change_it() {
        let p = demo_project(48_000);
        let t = audio_track(&p);
        let view = ChainView::of(&p, t, 48_000);
        let mut q = p.clone();
        let clip = q.track(t).and_then(|t| t.clips.first().copied());
        if let Some(c) = clip.and_then(|c| q.clips.get_mut(&c)) {
            c.muted = !c.muted;
        }
        assert!(!view.holds(&q, t, 48_000));
        let mut q = p.clone();
        q.timeline.tempo.set_initial_bpm(97.0);
        assert!(!view.holds(&q, t, 48_000));
        let mut q = p.clone();
        if let Some(tr) = q.track_mut(t)
            && let Some(slot) = tr.inserts.first_mut()
        {
            slot.bypass = !slot.bypass;
            assert!(!view.holds(&q, t, 48_000));
        }
    }

    #[test]
    fn a_midi_track_playing_it_is_part_of_it() {
        let p = demo_project(48_000);
        let Some((midi, inst)) = p.tracks.iter().find_map(|m| match m.output {
            OutputRouting::Track { track } if m.kind == TrackKind::Midi => Some((m.id, track)),
            _ => None,
        }) else {
            return;
        };
        assert!(chain_closure(&p, inst).contains(&midi));
        let view = ChainView::of(&p, inst, 48_000);
        let mut q = p.clone();
        if let Some(m) = q.track_mut(midi) {
            m.mute = true;
        }
        assert!(!view.holds(&q, inst, 48_000), "its notes no longer come");
    }

    #[test]
    fn a_drum_map_and_what_it_says_change_it() {
        use faderframe_project::drum_map::DrumMap;
        let p = demo_project(48_000);
        let t = audio_track(&p);
        let mut with_map = p.clone();
        let id = with_map.ids.allocate();
        with_map.drum_maps.push(DrumMap::gm(id));
        let view = ChainView::of(&with_map, t, 48_000);
        let mut q = with_map.clone();
        if let Some(tr) = q.track_mut(t) {
            tr.drum_map = Some(id);
        }
        assert!(!view.holds(&q, t, 48_000), "a map chosen");
        let view = ChainView::of(&q, t, 48_000);
        assert!(view.holds(&q, t, 48_000));
        if let Some(m) = q.drum_maps.iter_mut().find(|m| m.id == id)
            && let Some(sound) = m.sounds.first_mut()
        {
            sound.output_note = Some(sound.pitch.wrapping_add(1) & 127);
        }
        assert!(!view.holds(&q, t, 48_000), "the map plays another note");
        assert_ne!(ChainView::of(&q, t, 48_000).key(), view.key());
    }

    #[test]
    fn a_clip_envelope_changes_it() {
        use faderframe_project::clip_envelope::{ClipEnvelope, EnvelopePoint};
        let p = demo_project(48_000);
        let t = audio_track(&p);
        let view = ChainView::of(&p, t, 48_000);
        let mut q = p.clone();
        let clip = q.track(t).and_then(|t| t.clips.first().copied());
        if let Some(faderframe_project::ClipContent::Audio(a)) = clip
            .and_then(|c| q.clips.get_mut(&c))
            .map(|c| &mut c.content)
        {
            a.envelope = ClipEnvelope {
                volume: vec![EnvelopePoint::new(0, -6.0)],
                pan: Vec::new(),
            }
            .boxed();
        }
        assert!(!view.holds(&q, t, 48_000), "the clip plays quieter");
        assert_ne!(ChainView::of(&q, t, 48_000).key(), view.key());
    }
}
