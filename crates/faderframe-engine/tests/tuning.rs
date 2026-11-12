#![allow(clippy::unwrap_used)]
//! Tunings reach what plays the notes: built-in instruments read the key
//! table, hosted plugins get tuning note expressions, MIDI Tuning Standard
//! SysEx or MPE bends (as the track says), external MIDI outputs MPE bends
//! or per-note pitch.

mod common;

use common::TestProject;
use faderframe_audio_graph::NodeIo;
use faderframe_core::{ChannelLayout, ParameterId, TrackId, builtin};
use faderframe_engine::offline::OfflineRenderer;
use faderframe_engine::{EngineConfig, PluginHost};
use faderframe_midi::tuning::mts::{self, Mts};
use faderframe_midi::tuning::{Tuning, TuningTable};
use faderframe_midi::{MidiEvent, NoteExpressionKind};
use faderframe_plugin_host::{
    AudioPortInfo, ParameterInfo, PluginCategory, PluginDescriptor, PluginError, PluginFactory,
    PluginFormat, PluginInstance, PluginProcessContext, PluginProcessor, PluginRegistry,
    ProcessConfig, ProcessStatus, TailLength,
};
use faderframe_project::tuning::Retune;
use faderframe_project::{
    Clip, ClipContent, Command, Impact, MidiClip, MidiNote, MidiOutputRouting, MpeConfig,
    PluginRef, PluginSlot, TrackColor, TrackKind,
};
use faderframe_timeline::MusicalTime;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

const SR: u32 = 48_000;
const BLOCK: usize = 256;

// --- an instrument that writes down what it is given ------------------------------------

/// What reached it: a MIDI event, or a SysEx message's bytes.
#[derive(Clone, Debug, PartialEq)]
enum Got {
    Event(u32, MidiEvent),
    SysEx(u32, Vec<u8>),
}

/// What each listener (by plugin id) was given.
type Logs = Mutex<HashMap<String, &'static Mutex<Vec<Got>>>>;

fn log(id: &str) -> &'static Mutex<Vec<Got>> {
    static ALL: OnceLock<Logs> = OnceLock::new();
    let mut all = ALL
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    all.entry(id.to_string())
        .or_insert_with(|| Box::leak(Box::new(Mutex::new(Vec::new()))))
}

fn take(id: &str) -> Vec<Got> {
    std::mem::take(&mut *log(id).lock().unwrap())
}

struct Factory;

struct Instance {
    descriptor: PluginDescriptor,
}

struct Listener {
    id: String,
}

impl PluginFactory for Factory {
    fn format(&self) -> PluginFormat {
        PluginFormat::Clap
    }
    fn scan(&self) -> Vec<PluginDescriptor> {
        Vec::new()
    }
    fn instantiate(&self, id: &str) -> Result<Box<dyn PluginInstance>, PluginError> {
        Ok(Box::new(Instance {
            descriptor: PluginDescriptor {
                format: PluginFormat::Clap,
                id: id.into(),
                name: "Listener".into(),
                vendor: "FaderFrame".into(),
                version: "1".into(),
                category: PluginCategory::Instrument,
                audio_inputs: Vec::new(),
                audio_outputs: vec![AudioPortInfo {
                    channels: 2,
                    is_main: true,
                }],
                note_inputs: 1,
                note_outputs: 0,
            },
        }))
    }
}

impl PluginInstance for Instance {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }
    fn parameters(&self) -> &[ParameterInfo] {
        &[]
    }
    fn parameter(&mut self, _id: ParameterId) -> Option<f64> {
        None
    }
    fn set_parameter(&mut self, id: ParameterId, _v: f64) -> Result<(), PluginError> {
        Err(PluginError::UnknownParameter(id))
    }
    fn latency_samples(&self) -> u32 {
        0
    }
    fn tail(&self) -> TailLength {
        TailLength::None
    }
    fn save_state(&mut self) -> Result<Vec<u8>, PluginError> {
        Ok(Vec::new())
    }
    fn load_state(&mut self, _data: &[u8]) -> Result<(), PluginError> {
        Ok(())
    }
    /// "…plain" ids list no note expressions (as a VST3 instrument without
    /// tuning would); others leave it open, as CLAP does.
    fn note_expressions(&self) -> Option<Vec<NoteExpressionKind>> {
        self.descriptor.id.ends_with("plain").then(Vec::new)
    }
    fn create_processor(
        &mut self,
        _c: &ProcessConfig,
    ) -> Result<Box<dyn PluginProcessor>, PluginError> {
        Ok(Box::new(Listener {
            id: self.descriptor.id.clone(),
        }))
    }
}

impl PluginProcessor for Listener {
    fn process(&mut self, _ctx: &PluginProcessContext<'_>, io: &mut NodeIo<'_>) -> ProcessStatus {
        for out in io.audio_out.iter_mut() {
            out.clear();
        }
        if let Some(midi) = io.events_in.first() {
            let mut log = log(&self.id).lock().unwrap();
            for e in midi.iter() {
                log.push(match e.event {
                    MidiEvent::SysEx(s) => {
                        Got::SysEx(e.sample_offset, midi.sysex(&s).unwrap().to_vec())
                    }
                    ev => Got::Event(e.sample_offset, ev),
                });
            }
        }
        ProcessStatus::Continue
    }
    fn reset(&mut self) {}
}

// --- the rig ---------------------------------------------------------------------------

fn note(tp: &mut TestProject, start: f64, key: u8) -> MidiNote {
    MidiNote {
        fine: Default::default(),
        articulation: None,
        id: tp.project.ids.allocate(),
        start: MusicalTime::from_quarters(start),
        length: MusicalTime::from_quarters(0.5),
        key,
        velocity: 100,
        channel: 0,
        muted: false,
        release: None,
        time_locked: false,
    }
}

fn clip(tp: &mut TestProject, track: TrackId, keys: &[(f64, u8)]) {
    let notes = keys.iter().map(|(s, k)| note(tp, *s, *k)).collect();
    let id = tp.project.ids.allocate();
    tp.project.clips.insert(
        id,
        Clip {
            id,
            track,
            name: "Notes".into(),
            color: Some(TrackColor::palette(1)),
            start: MusicalTime::ZERO,
            muted: false,
            content: ClipContent::Midi(MidiClip {
                articulations: Vec::new(),
                length: MusicalTime::from_quarters_i(4),
                notes,
                controllers: Vec::new(),
                expressions: Vec::new(),
                sysex: Vec::new(),
            }),
        },
    );
    tp.project.track_mut(track).unwrap().clips.push(id);
}

/// An instrument track playing `plugin` (a listener id or a built-in).
fn instrument(tp: &mut TestProject, plugin: PluginRef) -> TrackId {
    let t = tp.track(TrackKind::Instrument, "Keys", ChannelLayout::Stereo);
    let slot = PluginSlot {
        id: tp.project.ids.allocate(),
        plugin,
        bypass: false,
        parameters: Vec::new(),
        state: None,
        sidechain: None,
        mono_to_stereo: false,
        mix_origin: None,
    };
    tp.project.track_mut(t).unwrap().inserts = vec![slot];
    t
}

fn listener(id: &str) -> PluginRef {
    PluginRef {
        format: faderframe_project::PluginFormat::Clap,
        id: id.into(),
        name: "Listener".into(),
    }
}

fn renderer(tp: &TestProject) -> OfflineRenderer {
    let config = EngineConfig {
        sample_rate: SR,
        max_block_size: BLOCK,
        ..EngineConfig::default()
    };
    let mut r = OfflineRenderer::new(&tp.project, &tp.sources, config, BLOCK, 2).unwrap();
    let mut registry = PluginRegistry::with_builtins();
    registry.add_factory(Box::new(Factory));
    *r.controller.plugins() = PluginHost::new(registry);
    r.controller
        .sync(&tp.project, &tp.sources, Impact::Graph)
        .unwrap();
    r
}

fn set(tp: &mut TestProject, cmd: Command) -> Impact {
    let impact = cmd.impact();
    cmd.apply(&mut tp.project).unwrap();
    impact
}

fn steps(r: &mut OfflineRenderer, n: usize) {
    for _ in 0..n {
        r.step();
    }
}

fn events(got: &[Got]) -> Vec<(u32, MidiEvent)> {
    got.iter()
        .filter_map(|g| match g {
            Got::Event(at, e) => Some((*at, *e)),
            Got::SysEx(..) => None,
        })
        .collect()
}

fn sysex(got: &[Got]) -> Vec<Vec<u8>> {
    got.iter()
        .filter_map(|g| match g {
            Got::SysEx(_, b) => Some(b.clone()),
            Got::Event(..) => None,
        })
        .collect()
}

// --- the tests -------------------------------------------------------------------------

/// A CLAP-like instrument (it takes tuning per note): each note-on is
/// followed by its key's offset.
#[test]
fn hosted_instruments_get_each_notes_offset_as_a_tuning_expression() {
    let id = "test.tuning.expression";
    let mut tp = TestProject::new(SR);
    let t = instrument(&mut tp, listener(id));
    clip(&mut tp, t, &[(0.0, 60), (1.0, 61)]);
    set(
        &mut tp,
        Command::SetTuning {
            track: None,
            tuning: Some(Tuning::equal(24)),
        },
    );
    let mut r = renderer(&tp);
    take(id);
    r.play_from(0).unwrap();
    steps(&mut r, 200);
    let got = events(&take(id));
    let tuning = |key: u8| {
        got.iter().find_map(|(_, e)| match *e {
            MidiEvent::NoteExpression {
                key: k,
                kind: NoteExpressionKind::Tuning,
                value,
                ..
            } if k == key => Some(value.get()),
            _ => None,
        })
    };
    assert_eq!(tuning(60), None, "middle C is where 12-TET has it");
    assert!((tuning(61).unwrap() + 0.5).abs() < 1e-6, "{got:?}");
    let on = got
        .iter()
        .position(|(_, e)| matches!(e, MidiEvent::NoteOn { key: 61, .. }))
        .unwrap();
    assert!(matches!(
        got[on + 1].1,
        MidiEvent::NoteExpression { key: 61, .. }
    ));
    assert_eq!(got[on].0, got[on + 1].0, "at the note-on");
    assert!(
        got.iter().all(|(_, e)| !matches!(e, MidiEvent::SysEx(_))),
        "no MTS"
    );
}

/// An instrument without tuning expressions gets MTS: a bulk dump before
/// its first notes, real-time changes when the tuning changes, nothing
/// again when the graph is rebuilt; a track tuned back to 12-TET tells it
/// so.
#[test]
fn plugins_without_tuning_expressions_are_told_by_mts_once() {
    let id = "test.tuning.plain";
    let mut tp = TestProject::new(SR);
    let t = instrument(&mut tp, listener(id));
    clip(&mut tp, t, &[(0.0, 61)]);
    set(
        &mut tp,
        Command::SetTuning {
            track: Some(t),
            tuning: Some(Tuning::equal(19)),
        },
    );
    let mut r = renderer(&tp);
    take(id);
    r.play_from(0).unwrap();
    steps(&mut r, 20);
    let got = take(id);
    let dumps = sysex(&got);
    assert_eq!(dumps.len(), 1, "one bulk dump");
    let Some(Mts::Bulk { pitches, .. }) = mts::parse(&dumps[0]) else {
        panic!("not a bulk dump")
    };
    let table = Tuning::equal(19).table(440.0);
    for (k, p) in pitches.iter().enumerate() {
        assert!((p.unwrap() - table.pitch(k as u8).unwrap()).abs() < 1e-4);
    }
    assert!(matches!(got[0], Got::SysEx(0, _)), "before the notes");
    assert!(
        events(&got)
            .iter()
            .all(|(_, e)| !matches!(e, MidiEvent::NoteExpression { .. })),
        "no expressions"
    );
    // Rebuilt: told already.
    r.controller
        .sync(&tp.project, &tp.sources, Impact::Graph)
        .unwrap();
    steps(&mut r, 10);
    assert!(sysex(&take(id)).is_empty());
    // Another tuning: real-time changes, all 128 keys.
    let impact = set(
        &mut tp,
        Command::SetTuning {
            track: Some(t),
            tuning: Some(Tuning::equal(31)),
        },
    );
    r.controller.sync(&tp.project, &tp.sources, impact).unwrap();
    steps(&mut r, 4);
    let changes = sysex(&take(id));
    assert_eq!(
        changes.len(),
        mts::note_changes(&TuningTable::STANDARD, 0).len()
    );
    let thirty_one = Tuning::equal(31).table(440.0);
    for m in &changes {
        let Some(Mts::NoteChange {
            realtime, changes, ..
        }) = mts::parse(m)
        else {
            panic!("not a note change")
        };
        assert!(realtime);
        for (k, p) in changes {
            assert!((p.unwrap() - thirty_one.pitch(k).unwrap()).abs() < 1e-4);
        }
    }
    // Back to 12-TET: told once.
    let impact = set(
        &mut tp,
        Command::SetTuning {
            track: Some(t),
            tuning: None,
        },
    );
    r.controller.sync(&tp.project, &tp.sources, impact).unwrap();
    steps(&mut r, 4);
    let back = sysex(&take(id));
    assert_eq!(back.len(), changes.len());
    steps(&mut r, 4);
    assert!(sysex(&take(id)).is_empty());
    // Retuned another way: the plugin is told 12-TET (here: it is).
    let impact = set(
        &mut tp,
        Command::SetTrackRetune {
            track: t,
            retune: Retune::Off,
        },
    );
    assert_eq!(impact, Impact::Graph);
}

/// An MPE track: every note's member-channel bend carries its key's
/// offset, from before its note-on.
#[test]
fn mpe_tracks_carry_the_offset_in_their_member_bends() {
    let id = "test.tuning.mpe";
    let mut tp = TestProject::new(SR);
    let t = instrument(&mut tp, listener(id));
    let mpe = MpeConfig {
        members: 15,
        bend_range: 48,
    };
    tp.project.track_mut(t).unwrap().mpe = Some(mpe);
    clip(&mut tp, t, &[(0.0, 61), (0.0, 64)]);
    set(
        &mut tp,
        Command::SetTuning {
            track: None,
            tuning: Some(Tuning::equal(24)),
        },
    );
    let mut r = renderer(&tp);
    take(id);
    r.play_from(0).unwrap();
    steps(&mut r, 40);
    let got = events(&take(id));
    for (key, offset) in [(61u8, -0.5), (64u8, -2.0)] {
        let on = got
            .iter()
            .position(|(_, e)| matches!(e, MidiEvent::NoteOn { key: k, .. } if *k == key))
            .unwrap();
        let MidiEvent::NoteOn { channel, .. } = got[on].1 else {
            unreachable!()
        };
        assert_ne!(channel, 0, "a member channel");
        let bend = got[..on]
            .iter()
            .rev()
            .find_map(|(_, e)| match *e {
                MidiEvent::PitchBend { channel: c, value } if c == channel => Some(value),
                _ => None,
            })
            .unwrap();
        let want = (8192.0 + offset / 48.0 * 8192.0_f64).round() as u16;
        assert_eq!(bend, want, "key {key}");
    }
}

/// The built-in synth plays the table: A4 at a reference of 432 Hz.
#[test]
fn the_built_in_synth_plays_the_reference_pitch() {
    let level_at = |reference: f64| {
        let mut tp = TestProject::new(SR);
        let t = instrument(&mut tp, PluginRef::builtin(builtin::SYNTH, "Synth"));
        clip(&mut tp, t, &[(0.0, 69)]);
        set(&mut tp, Command::SetReferencePitch { hz: reference });
        let mut r = renderer(&tp);
        r.play_from(0).unwrap();
        let out = r.render(SR as usize / 4);
        let l = &out[0][2_000..];
        // Energy at 432 and 440 Hz (correlation with each).
        let at = |hz: f64| {
            let (mut s, mut c) = (0.0, 0.0);
            for (i, v) in l.iter().enumerate() {
                let ph = 2.0 * std::f64::consts::PI * hz * i as f64 / f64::from(SR);
                s += f64::from(*v) * ph.sin();
                c += f64::from(*v) * ph.cos();
            }
            (s * s + c * c).sqrt()
        };
        (at(432.0), at(440.0))
    };
    let (a432, a440) = level_at(432.0);
    assert!(a432 > 2.0 * a440, "432: {a432} vs {a440}");
    let (a432, a440) = level_at(440.0);
    assert!(a440 > 2.0 * a432, "440: {a440} vs {a432}");
}

/// A MIDI track's external output: per-note pitch after each note-on (MIDI
/// 2.0 ports send it, MIDI 1.0 ports drop it), MPE bends on MPE tracks.
#[test]
fn midi_outputs_carry_per_note_pitch_or_mpe_bends() {
    use std::collections::HashSet;
    let run = |mpe: Option<MpeConfig>| {
        let mut tp = TestProject::new(SR);
        let t = tp.track(TrackKind::Midi, "Ext", ChannelLayout::Stereo);
        tp.project.track_mut(t).unwrap().midi_output = Some(MidiOutputRouting {
            port: "Synth:Synth In".into(),
            channel: None,
        });
        tp.project.track_mut(t).unwrap().mpe = mpe;
        clip(&mut tp, t, &[(0.0, 61)]);
        set(
            &mut tp,
            Command::SetTuning {
                track: None,
                tuning: Some(Tuning::equal(24)),
            },
        );
        let mut r = renderer(&tp);
        r.controller
            .set_midi_output_ports(HashMap::from([("Synth:Synth In".to_string(), 2)]));
        r.controller.set_midi_live(HashSet::new());
        r.controller
            .sync(&tp.project, &tp.sources, Impact::Graph)
            .unwrap();
        let (_tx, q, _feed) = faderframe_midi::midi_input_queue(64);
        let clock = _tx.clock();
        r.controller.set_midi_input(q).unwrap();
        let (oq, mut orx) = faderframe_midi::midi_output_queue(1024, clock);
        r.controller.set_midi_output(oq).unwrap();
        r.play_from(0).unwrap();
        steps(&mut r, 40);
        std::iter::from_fn(|| orx.pop().ok())
            .filter(|e| e.port == 2)
            .filter_map(|e| e.event())
            .collect::<Vec<_>>()
    };
    let got = run(None);
    let on = got
        .iter()
        .position(|e| matches!(e, MidiEvent::NoteOn { key: 61, .. }))
        .unwrap();
    match got[on + 1] {
        MidiEvent::NoteExpression {
            key: 61,
            kind: NoteExpressionKind::Tuning,
            value,
            ..
        } => assert!((value.get() + 0.5).abs() < 1e-6),
        e => panic!("{e:?}"),
    }
    let got = run(Some(MpeConfig::default()));
    let on = got
        .iter()
        .position(|e| matches!(e, MidiEvent::NoteOn { key: 61, .. }))
        .unwrap();
    let MidiEvent::NoteOn { channel, .. } = got[on] else {
        unreachable!()
    };
    let bend = got[..on]
        .iter()
        .rev()
        .find_map(|e| match *e {
            MidiEvent::PitchBend { channel: c, value } if c == channel => Some(value),
            _ => None,
        })
        .unwrap();
    assert_eq!(bend, (8192.0 - 0.5 / 48.0 * 8192.0_f64).round() as u16);
    assert!(
        got.iter()
            .all(|e| !matches!(e, MidiEvent::NoteExpression { .. })),
        "MPE carries it"
    );
}
