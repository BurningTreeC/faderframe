//! A small but complete demo session in A minor (Am – F – C – G on the
//! chord track, an Intro and a Verse): drums through a drum bus with a
//! parallel-compression container, bass through a preamp, compressor and
//! saturator, plucks through the program EQ, a chorused pad that ducks
//! under the drums and pans with an LFO, a synth playing a melody (its
//! filter following velocity and a macro), a MIDI track whose chords
//! (from the chord track) an arpeggiated synth plays, echo and reverb
//! auxes fed by sends, and a limiter on the master. All audio is
//! generated, so the demo needs no files.

use crate::container::Chain;
use crate::harmony::{Chord, Key, Quality, Scale};
use crate::modulation::{
    FollowSource, LfoShape, ModRate, ModRoute, ModSource, ModTarget, Modulator,
};
use crate::{
    AudioClip, AudioSource, AuxSend, ChordEvent, Clip, ClipContent, ClipFades, KeyChange, Marker,
    MidiClip, MidiNote, MusicalRange, OutputRouting, PluginRef, PluginSlot, Project,
    SavedParameter, Section, SendTap, SourceSpec, StretchSettings, Track, TrackColor, TrackKind,
};
use faderframe_audio_files::GeneratorSpec;
use faderframe_core::{AudioSourceId, ChannelLayout, ParameterId, TrackId, builtin};
use faderframe_timeline::MusicalTime;

/// A built-in device with some of its parameters set.
fn device(p: &mut Project, id: &str, name: &str, set: &[(u32, f64)]) -> PluginSlot {
    PluginSlot {
        id: p.ids.allocate(),
        plugin: PluginRef::builtin(id, name),
        bypass: false,
        parameters: set
            .iter()
            .map(|(id, value)| SavedParameter {
                id: ParameterId(*id),
                value: *value,
            })
            .collect(),
        state: None,
        sidechain: None,
    }
}

/// A modulator routed to `routes` (target, depth).
fn modulator(
    p: &mut Project,
    name: &str,
    source: ModSource,
    routes: &[(ModTarget, f32)],
) -> Modulator {
    let mut m = Modulator::new(p.ids.allocate(), source);
    m.name = name.into();
    m.routes = routes
        .iter()
        .map(|&(target, depth)| ModRoute { target, depth })
        .collect();
    m
}

/// A parameter of a device as a modulation target.
fn param(slot: &PluginSlot, id: u32) -> ModTarget {
    ModTarget::Plugin {
        plugin: slot.id,
        parameter: ParameterId(id),
    }
}

const BPM: f64 = 112.0;
const BARS: u32 = 8;

/// A-minor progression Am – F – C – G.
fn chords() -> Vec<Vec<u8>> {
    vec![
        vec![57, 60, 64],
        vec![53, 57, 60],
        vec![55, 60, 64],
        vec![55, 59, 62],
    ]
}

pub fn demo_project(sample_rate: u32) -> Project {
    let mut p = Project::new("Demo Session", sample_rate);
    p.timeline.tempo.set_initial_bpm(BPM);
    let master = p.master_id().unwrap_or(TrackId(0));
    let bar = |n: i64| p.timeline.meter.bar_start(n as i32);
    let (bar0, bar2, bar4, bar8) = (bar(0), bar(2), bar(4), bar(8));
    let bars: Vec<MusicalTime> = (0..=BARS as i64).map(bar).collect();
    p.loop_range = MusicalRange::new(bar0, bar8);
    p.loop_enabled = true;

    let source = |p: &mut Project, name: &str, generator: GeneratorSpec| -> (AudioSourceId, i64) {
        let id: AudioSourceId = p.ids.allocate();
        let src = AudioSource {
            id,
            name: name.into(),
            spec: SourceSpec::Generated { generator },
        };
        let frames = src.frames(sample_rate);
        p.sources.insert(id, src);
        (id, frames)
    };
    let drums_src = source(
        &mut p,
        "Drum Loop",
        GeneratorSpec::DrumLoop {
            bpm: BPM,
            bars: BARS,
            seed: 11,
        },
    );
    let bass_src = source(
        &mut p,
        "Bassline",
        GeneratorSpec::Bassline {
            bpm: BPM,
            bars: BARS,
            roots: vec![45, 41, 48, 43],
        },
    );
    let pluck_src = source(
        &mut p,
        "Pluck Arp",
        GeneratorSpec::PluckArpeggio {
            bpm: BPM,
            bars: BARS,
            chords: chords(),
            seed: 5,
        },
    );
    let pad_src = source(
        &mut p,
        "Pad",
        GeneratorSpec::Pad {
            bpm: BPM,
            bars: BARS,
            chords: chords(),
        },
    );

    // Tracks.
    let drum_bus: TrackId = p.ids.allocate();
    let echo: TrackId = p.ids.allocate();
    let mut tracks = Vec::new();
    let new_track = |p: &mut Project, kind, name: &str, layout, color: usize| {
        let id: TrackId = p.ids.allocate();
        Track::new(id, kind, name, TrackColor::palette(color)).with_layout(layout)
    };

    let mut drums = new_track(&mut p, TrackKind::Audio, "Drums", ChannelLayout::Stereo, 0);
    drums.output = OutputRouting::Track { track: drum_bus };
    let mut bass = new_track(&mut p, TrackKind::Audio, "Bass", ChannelLayout::Mono, 1);
    bass.volume_db = -3.0;
    let mut pluck = new_track(&mut p, TrackKind::Audio, "Pluck", ChannelLayout::Mono, 4);
    pluck.pan = -0.35;
    pluck.volume_db = -6.0;
    let mut pad = new_track(&mut p, TrackKind::Audio, "Pad", ChannelLayout::Stereo, 6);
    pad.volume_db = -8.0;
    let mut keys = new_track(
        &mut p,
        TrackKind::Instrument,
        "Lead Synth",
        ChannelLayout::Stereo,
        7,
    );
    keys.volume_db = -7.0;
    keys.pan = 0.25;
    let synth = device(&mut p, builtin::SYNTH, "FaderFrame Synth", &[]);
    keys.inserts.push(synth.clone());
    // The lead's filter opens with how hard a note is played, and a
    // Brightness macro opens it further and adds resonance.
    keys.modulators = vec![
        modulator(
            &mut p,
            "Velocity",
            ModSource::Velocity,
            &[(param(&synth, 1), 0.12)],
        ),
        modulator(
            &mut p,
            "Brightness",
            ModSource::Macro { value: 0.35 },
            &[(param(&synth, 1), 0.18), (param(&synth, 2), 0.15)],
        ),
    ];

    // The bass: a preamp, then compressor and saturator.
    bass.preamp = Some(device(
        &mut p,
        builtin::PREAMPS[1].0,
        builtin::PREAMPS[1].1,
        &[],
    ));
    bass.inserts.push(device(
        &mut p,
        builtin::COMPRESSOR,
        "FaderFrame Compressor",
        &[(0, -18.0), (1, 3.0), (2, 15.0), (3, 120.0)],
    ));
    bass.inserts.push(device(
        &mut p,
        builtin::SATURATOR,
        "FaderFrame Saturator",
        &[(0, 4.0)],
    ));
    // The plucks through the program EQ; the pad chorused.
    pluck.inserts.push(device(
        &mut p,
        builtin::PROGRAM_EQ,
        "FaderFrame Program EQ",
        &[],
    ));
    pad.inserts.push(device(
        &mut p,
        builtin::MODULATION,
        "FaderFrame Modulation",
        &[(12, 0.35)],
    ));

    let space: TrackId = p.ids.allocate();
    for (t, echo_db, space_db) in [
        (&mut pluck, -10.0, -14.0),
        (&mut pad, -14.0, -10.0),
        (&mut keys, -8.0, -12.0),
    ] {
        for (target, level_db) in [(echo, echo_db), (space, space_db)] {
            t.sends.push(AuxSend {
                id: p.ids.allocate(),
                target,
                level_db,
                tap: SendTap::PostFader,
                enabled: true,
            });
        }
    }
    // The pad ducks under the drums and drifts across the stereo field.
    let drums_id = drums.id;
    pad.modulators = vec![
        modulator(
            &mut p,
            "Duck",
            ModSource::Follower {
                source: FollowSource::Track { track: drums_id },
                attack_ms: 5.0,
                release_ms: 180.0,
                gain_db: 0.0,
            },
            &[(ModTarget::Volume, -0.12)],
        ),
        modulator(
            &mut p,
            "Auto Pan",
            ModSource::Lfo {
                shape: LfoShape::Sine,
                rate: ModRate::Sync { beats: 8.0 },
                phase: 0.0,
            },
            &[(ModTarget::Pan, 0.3)],
        ),
    ];

    let mut bus = Track::new(drum_bus, TrackKind::Bus, "Drum Bus", TrackColor::palette(9))
        .with_layout(ChannelLayout::Stereo);
    bus.volume_db = -2.0;
    // Parallel compression: the drums dry, and squashed and driven below.
    let squash = device(&mut p, builtin::CONTAINER, "Parallel Squash", &[]);
    let mut crushed = Chain::new("Squash");
    crushed.gain_db = -8.0;
    crushed.inserts = vec![
        device(
            &mut p,
            builtin::COMPRESSOR,
            "FaderFrame Compressor",
            &[(0, -32.0), (1, 10.0), (2, 3.0), (3, 80.0), (8, 1.0)],
        ),
        device(
            &mut p,
            builtin::SATURATOR,
            "FaderFrame Saturator",
            &[(0, 9.0)],
        ),
    ];
    bus.containers
        .insert(squash.id, vec![Chain::new("Dry"), crushed]);
    bus.inserts.push(squash);
    let mut echo_track = Track::new(echo, TrackKind::Aux, "Echo", TrackColor::palette(5))
        .with_layout(ChannelLayout::Stereo);
    echo_track.inserts.push(PluginSlot {
        id: p.ids.allocate(),
        plugin: PluginRef::builtin(builtin::ECHO, "FaderFrame Echo"),
        bypass: false,
        parameters: Vec::new(),
        state: None,
        sidechain: None,
    });
    echo_track.volume_db = -4.0;
    let mut space_track = Track::new(space, TrackKind::Aux, "Space", TrackColor::palette(3))
        .with_layout(ChannelLayout::Stereo);
    space_track.inserts.push(device(
        &mut p,
        builtin::REVERB,
        "FaderFrame Reverb",
        &[(2, 2.6), (13, 1.0)],
    ));
    space_track.volume_db = -6.0;

    // Arpeggios: a MIDI track whose notes become the chord track's chords
    // (its Chord effect), played by an instrument track that arpeggiates
    // them, both in a folder.
    let folder: TrackId = p.ids.allocate();
    let arp_folder = Track::new(
        folder,
        TrackKind::Folder,
        "Arpeggios",
        TrackColor::palette(8),
    );
    let arp_synth_id: TrackId = p.ids.allocate();
    let mut arp_notes = new_track(&mut p, TrackKind::Midi, "Chords", ChannelLayout::Stereo, 8);
    arp_notes.folder = Some(folder);
    arp_notes.output = OutputRouting::Track {
        track: arp_synth_id,
    };
    arp_notes.inserts.push(device(
        &mut p,
        builtin::CHORD,
        "FaderFrame Chord",
        &[(0, 3.0)],
    ));
    let mut arp_synth = Track::new(
        arp_synth_id,
        TrackKind::Instrument,
        "Arp Synth",
        TrackColor::palette(8),
    )
    .with_layout(ChannelLayout::Stereo);
    arp_synth.folder = Some(folder);
    arp_synth.volume_db = -13.0;
    arp_synth.pan = -0.3;
    arp_synth.inserts.push(device(
        &mut p,
        builtin::ARPEGGIATOR,
        "FaderFrame Arpeggiator",
        &[(0, 2.0), (3, 2.0), (2, 0.6)],
    ));
    let arp_voice = device(
        &mut p,
        builtin::SYNTH,
        "FaderFrame Synth",
        &[
            (9, 2.0),
            (1, 1_800.0),
            (4, 2.0),
            (5, 180.0),
            (6, 0.2),
            (7, 120.0),
        ],
    );
    arp_synth.inserts.push(arp_voice.clone());
    arp_synth.modulators = vec![modulator(
        &mut p,
        "Filter Sweep",
        ModSource::Lfo {
            shape: LfoShape::Triangle,
            rate: ModRate::Sync { beats: 16.0 },
            phase: 0.0,
        },
        &[(param(&arp_voice, 1), 0.15)],
    )];

    // Clips.
    let audio_clip = |p: &mut Project,
                      track: &mut Track,
                      name: &str,
                      src: (AudioSourceId, i64),
                      start: MusicalTime,
                      offset_bars: i64| {
        let sr = sample_rate as f64;
        let offset = p
            .timeline
            .to_samples(p.timeline.meter.bar_start(offset_bars as i32), sr);
        let clip = Clip {
            id: p.ids.allocate(),
            track: track.id,
            name: name.into(),
            color: None,
            start,
            muted: false,
            content: ClipContent::Audio(AudioClip {
                source: src.0,
                source_offset: offset,
                length: src.1 - offset,
                gain_db: 0.0,
                fades: ClipFades {
                    fade_in: 64,
                    fade_out: (sample_rate / 50) as i64,
                    ..ClipFades::default()
                },
                stretch: StretchSettings::Off,
                reversed: false,
                warp: None,
                pitch: None,
                effects: None,
                spectral: None,
            }),
        };
        track.clips.push(clip.id);
        p.clips.insert(clip.id, clip);
    };
    audio_clip(&mut p, &mut drums, "Drum Loop", drums_src, bar0, 0);
    audio_clip(&mut p, &mut bass, "Bassline", bass_src, bar2, 2);
    audio_clip(&mut p, &mut pluck, "Pluck Arp", pluck_src, bar0, 0);
    audio_clip(&mut p, &mut pad, "Pad", pad_src, bar4, 4);
    // The pad swells in and dips before the loop point.
    {
        use faderframe_automation::{
            AutomationCurve, AutomationLane, AutomationMode, AutomationPoint, AutomationTarget,
            CurveShape,
        };
        let at = |q: f64, value: f64, shape| AutomationPoint {
            time: MusicalTime::from_quarters(q),
            value,
            shape,
        };
        let id = p.ids.allocate();
        pad.automation.lanes.push(AutomationLane {
            id,
            target: AutomationTarget::TrackVolume,
            curve: AutomationCurve::from_points(vec![
                at(16.0, -30.0, CurveShape::Smooth),
                at(22.0, -8.0, CurveShape::Linear),
                at(28.0, -8.0, CurveShape::Exponential),
                at(31.5, -20.0, CurveShape::Linear),
            ]),
            mode: AutomationMode::Read,
            visible: true,
        });
    }

    // Melody for the instrument track (A minor pentatonic phrases).
    let phrases: [&[(f64, f64, u8)]; 4] = [
        &[
            (0.0, 1.0, 76),
            (1.0, 0.5, 74),
            (1.5, 0.5, 72),
            (2.0, 1.5, 69),
            (3.5, 0.5, 72),
        ],
        &[
            (0.0, 1.5, 72),
            (1.5, 0.5, 69),
            (2.0, 1.0, 67),
            (3.0, 1.0, 69),
        ],
        &[
            (0.0, 0.5, 67),
            (0.5, 0.5, 69),
            (1.0, 1.0, 72),
            (2.0, 1.0, 76),
            (3.0, 1.0, 74),
        ],
        &[
            (0.0, 2.0, 74),
            (2.0, 0.5, 72),
            (2.5, 0.5, 71),
            (3.0, 1.0, 67),
        ],
    ];
    let mut notes = Vec::new();
    for b in 0..4i64 {
        let bar_q = (b * 4) as f64;
        for &(start, len, key) in phrases[b as usize % 4] {
            notes.push(MidiNote {
                id: p.ids.allocate(),
                start: MusicalTime::from_quarters(bar_q + start),
                length: MusicalTime::from_quarters(len * 0.95),
                key,
                velocity: if start.fract() == 0.0 { 104 } else { 86 },
                channel: 0,
                muted: false,
                release: None,
            });
        }
    }
    let melody = Clip {
        id: p.ids.allocate(),
        track: keys.id,
        name: "Melody".into(),
        color: None,
        start: bar4,
        muted: false,
        content: ClipContent::Midi(MidiClip {
            length: bar4,
            notes,
            controllers: Vec::new(),
            expressions: Vec::new(),
            sysex: Vec::new(),
        }),
    };
    keys.clips.push(melody.id);
    p.clips.insert(melody.id, melody);

    // The chords' MIDI: an A3 a bar, which the Chord effect turns into the
    // chord track's chord.
    let chord_notes = (0..BARS as i64)
        .map(|b| MidiNote {
            id: p.ids.allocate(),
            start: MusicalTime::from_quarters((b * 4) as f64),
            length: MusicalTime::from_quarters(3.9),
            key: 57,
            velocity: 92,
            channel: 0,
            muted: false,
            release: None,
        })
        .collect();
    let chord_clip = Clip {
        id: p.ids.allocate(),
        track: arp_notes.id,
        name: "Chords".into(),
        color: None,
        start: bar0,
        muted: false,
        content: ClipContent::Midi(MidiClip {
            length: bar8,
            notes: chord_notes,
            controllers: Vec::new(),
            expressions: Vec::new(),
            sysex: Vec::new(),
        }),
    };
    arp_notes.clips.push(chord_clip.id);
    p.clips.insert(chord_clip.id, chord_clip);

    // The key, the chord track (a chord a bar), the song's sections and a
    // marker where the lead comes in.
    p.keys = vec![KeyChange {
        at: bar0,
        key: Key::new(9, Scale::Minor),
    }];
    let progression = [
        (9, Quality::Minor),
        (5, Quality::Major),
        (0, Quality::Major),
        (7, Quality::Major),
    ];
    p.chords = (0..BARS as i64)
        .map(|b| {
            let (root, quality) = progression[b as usize % 4];
            ChordEvent {
                start: bars[b as usize],
                end: bars[b as usize + 1],
                chord: Chord::new(root, quality),
            }
        })
        .collect();
    p.sections = vec![
        Section {
            id: p.ids.allocate(),
            name: "Intro".into(),
            start: bar0,
            end: bar4,
            color: TrackColor::palette(2),
        },
        Section {
            id: p.ids.allocate(),
            name: "Verse".into(),
            start: bar4,
            end: bar8,
            color: TrackColor::palette(5),
        },
    ];
    p.markers = vec![Marker {
        id: p.ids.allocate(),
        position: bar4,
        name: "Lead In".into(),
    }];

    // The master: a true-peak limiter.
    let limiter = device(&mut p, builtin::LIMITER, "FaderFrame Limiter", &[(1, -1.0)]);
    if let Some(m) = p.track_mut(master) {
        m.inserts.push(limiter);
    }

    tracks.extend([
        drums,
        bass,
        pluck,
        pad,
        keys,
        bus,
        echo_track,
        space_track,
        arp_folder,
        arp_notes,
        arp_synth,
    ]);
    let master_index = p.track_index(master).unwrap_or(0);
    for (i, t) in tracks.into_iter().enumerate() {
        p.tracks.insert(master_index + i, t);
    }
    p
}
