//! A small but complete demo session: drums through a drum bus, bass,
//! plucks, pad, an instrument track playing a melody, and an echo aux fed by
//! sends. All audio is generated, so the demo needs no files.

use crate::{
    AudioClip, AudioSource, AuxSend, Clip, ClipContent, ClipFades, MidiClip, MidiNote,
    MusicalRange, OutputRouting, PluginRef, PluginSlot, Project, SendTap, SourceSpec,
    StretchSettings, Track, TrackColor, TrackKind,
};
use faderframe_audio_files::GeneratorSpec;
use faderframe_core::{AudioSourceId, ChannelLayout, TrackId, builtin};
use faderframe_timeline::MusicalTime;

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
    keys.instrument = Some(PluginSlot {
        id: p.ids.allocate(),
        plugin: PluginRef::builtin(builtin::SYNTH, "FaderFrame Synth"),
        bypass: false,
        parameters: Vec::new(),
        state: None,
    });

    for (t, level) in [(&mut pluck, -10.0), (&mut pad, -14.0), (&mut keys, -8.0)] {
        t.sends.push(AuxSend {
            id: p.ids.allocate(),
            target: echo,
            level_db: level,
            tap: SendTap::PostFader,
            enabled: true,
        });
    }

    let mut bus = Track::new(drum_bus, TrackKind::Bus, "Drum Bus", TrackColor::palette(9))
        .with_layout(ChannelLayout::Stereo);
    bus.volume_db = -2.0;
    let mut echo_track = Track::new(echo, TrackKind::Aux, "Echo", TrackColor::palette(5))
        .with_layout(ChannelLayout::Stereo);
    echo_track.inserts.push(PluginSlot {
        id: p.ids.allocate(),
        plugin: PluginRef::builtin(builtin::ECHO, "FaderFrame Echo"),
        bypass: false,
        parameters: Vec::new(),
        state: None,
    });
    echo_track.volume_db = -4.0;

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
        }),
    };
    keys.clips.push(melody.id);
    p.clips.insert(melody.id, melody);

    tracks.extend([drums, bass, pluck, pad, keys, bus, echo_track]);
    let master_index = p.track_index(master).unwrap_or(0);
    for (i, t) in tracks.into_iter().enumerate() {
        p.tracks.insert(master_index + i, t);
    }
    p
}
