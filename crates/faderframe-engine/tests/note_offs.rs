#![allow(clippy::unwrap_used)]
//! Every note an instrument is given ends (issue #27: a note stuck after a
//! plugin's editor opened while the arpeggiator played). The instrument
//! here keeps one processor across graphs, as hosted plugins do: what it
//! was given in one graph it still has in the next.

mod common;

use common::TestProject;
use faderframe_audio_graph::NodeIo;
use faderframe_core::{ChannelLayout, ParameterId, TrackId, builtin};
use faderframe_engine::EngineConfig;
use faderframe_engine::offline::OfflineRenderer;
use faderframe_midi::MidiEvent;
use faderframe_plugin_host::devices::arpeggiator;
use faderframe_plugin_host::{
    AudioPortInfo, ParameterInfo, PluginCategory, PluginDescriptor, PluginError, PluginFactory,
    PluginFormat, PluginInstance, PluginProcessContext, PluginProcessor, PluginRegistry,
    ProcessConfig, ProcessStatus, TailLength,
};
use faderframe_project::{
    Clip, ClipContent, ControllerLane, ControllerPoint, Impact, MidiClip, MidiController, MidiNote,
    PluginRef, PluginSlot, SavedParameter, TrackColor, TrackKind,
};
use faderframe_timeline::MusicalTime;
use faderframe_transport::TransportCommand;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

const SR: u32 = 48_000;
const BLOCK: usize = 256;
/// A sixteenth at 120 BPM.
const SIXTEENTH: i64 = 6000;
const BAR: i64 = 16 * SIXTEENTH;

// --- an instrument that tells what it has sounding ----------------------------------

/// What a test instrument (by plugin id) has sounding: keys of channel 0
/// (and its sustain pedal); while `miss` is set, blocks miss it.
struct Heard {
    on: [AtomicBool; 128],
    ons: AtomicUsize,
    pedal: AtomicBool,
    miss: AtomicBool,
}

impl Heard {
    fn sounding(&self) -> Vec<u8> {
        (0..128u8)
            .filter(|k| self.on[usize::from(*k)].load(Ordering::SeqCst))
            .collect()
    }

    fn ons(&self) -> usize {
        self.ons.load(Ordering::SeqCst)
    }
}

fn heard(id: &str) -> &'static Heard {
    static ALL: OnceLock<Mutex<HashMap<String, &'static Heard>>> = OnceLock::new();
    let mut all = ALL
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    all.entry(id.to_string()).or_insert_with(|| {
        Box::leak(Box::new(Heard {
            on: std::array::from_fn(|_| AtomicBool::new(false)),
            ons: AtomicUsize::new(0),
            pedal: AtomicBool::new(false),
            miss: AtomicBool::new(false),
        }))
    })
}

fn descriptor(id: &str) -> PluginDescriptor {
    PluginDescriptor {
        format: PluginFormat::Clap,
        id: id.into(),
        name: "Voices".into(),
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
    }
}

struct Factory;

impl PluginFactory for Factory {
    fn format(&self) -> PluginFormat {
        PluginFormat::Clap
    }
    fn scan(&self) -> Vec<PluginDescriptor> {
        Vec::new()
    }
    fn instantiate(&self, id: &str) -> Result<Box<dyn PluginInstance>, PluginError> {
        Ok(Box::new(Instance {
            descriptor: descriptor(id),
            heard: heard(id),
            turn: Arc::new(Mutex::new(())),
        }))
    }
}

struct Instance {
    descriptor: PluginDescriptor,
    heard: &'static Heard,
    /// One processor, whichever graph has it (a hosted plugin's).
    turn: Arc<Mutex<()>>,
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
    fn create_processor(
        &mut self,
        _c: &ProcessConfig,
    ) -> Result<Box<dyn PluginProcessor>, PluginError> {
        Ok(Box::new(Voices {
            heard: self.heard,
            turn: Arc::clone(&self.turn),
        }))
    }
}

struct Voices {
    heard: &'static Heard,
    turn: Arc<Mutex<()>>,
}

impl PluginProcessor for Voices {
    fn process(&mut self, _ctx: &PluginProcessContext<'_>, io: &mut NodeIo<'_>) -> ProcessStatus {
        for out in io.audio_out.iter_mut() {
            out.clear();
        }
        // Another graph's turn: silence, as hosted plugins play then.
        let Ok(_turn) = self.turn.try_lock() else {
            return ProcessStatus::Missed;
        };
        if self.heard.miss.load(Ordering::SeqCst) {
            return ProcessStatus::Missed;
        }
        let Some(midi) = io.events_in.first() else {
            return ProcessStatus::Continue;
        };
        for e in midi.iter() {
            match e.event {
                MidiEvent::NoteOn {
                    channel: 0,
                    key,
                    velocity,
                } if velocity > 0 => {
                    self.heard.on[usize::from(key & 127)].store(true, Ordering::SeqCst);
                    self.heard.ons.fetch_add(1, Ordering::SeqCst);
                }
                MidiEvent::NoteOn {
                    channel: 0, key, ..
                }
                | MidiEvent::NoteOff {
                    channel: 0, key, ..
                } => self.heard.on[usize::from(key & 127)].store(false, Ordering::SeqCst),
                MidiEvent::ControlChange {
                    channel: 0,
                    controller: 64,
                    value,
                } => self.heard.pedal.store(value >= 64, Ordering::SeqCst),
                _ => {}
            }
        }
        ProcessStatus::Continue
    }

    fn reset(&mut self) {
        for on in &self.heard.on {
            on.store(false, Ordering::SeqCst);
        }
    }
}

fn install() {
    faderframe_plugin_host::set_default_registry(|| {
        let mut r = PluginRegistry::with_builtins();
        r.add_factory(Box::new(Factory));
        r
    });
}

// --- the rig ---------------------------------------------------------------------------

fn builtin_slot(tp: &mut TestProject, id: &str, set: &[(u32, f64)]) -> PluginSlot {
    PluginSlot {
        id: tp.project.ids.allocate(),
        plugin: PluginRef::builtin(id, id),
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

fn voices_slot(tp: &mut TestProject, id: &str) -> PluginSlot {
    PluginSlot {
        id: tp.project.ids.allocate(),
        plugin: PluginRef {
            format: faderframe_project::PluginFormat::Clap,
            id: id.into(),
            name: "Voices".into(),
        },
        bypass: false,
        parameters: Vec::new(),
        state: None,
        sidechain: None,
    }
}

/// Chords as (start, length in quarters, keys), in one clip.
fn chords(tp: &mut TestProject, track: TrackId, chords: &[(f64, f64, &[u8])]) {
    let mut notes = Vec::new();
    for (start, length, keys) in chords {
        for k in *keys {
            notes.push(MidiNote {
                id: tp.project.ids.allocate(),
                start: MusicalTime::from_quarters(*start),
                length: MusicalTime::from_quarters(*length),
                key: *k,
                velocity: 100,
                channel: 0,
                muted: false,
                release: None,
            });
        }
    }
    let id = tp.project.ids.allocate();
    tp.project.clips.insert(
        id,
        Clip {
            id,
            track,
            name: "Chords".into(),
            color: Some(TrackColor::palette(1)),
            start: MusicalTime::ZERO,
            muted: false,
            content: ClipContent::Midi(MidiClip {
                length: MusicalTime::from_quarters_i(16),
                notes,
                controllers: Vec::new(),
                expressions: Vec::new(),
                sysex: Vec::new(),
            }),
        },
    );
    tp.project.track_mut(track).unwrap().clips.push(id);
}

fn renderer(tp: &TestProject) -> OfflineRenderer {
    let config = EngineConfig {
        sample_rate: SR,
        max_block_size: BLOCK,
        ..EngineConfig::default()
    };
    OfflineRenderer::new(&tp.project, &tp.sources, config, BLOCK, 2).unwrap()
}

/// Callbacks, about at the device's pace (the anticipator renders
/// alongside), until `done` (or ten seconds: false).
fn run_until(r: &mut OfflineRenderer, mut done: impl FnMut(&OfflineRenderer) -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(10) {
        if done(r) {
            return true;
        }
        r.step();
        std::thread::sleep(Duration::from_millis(4));
    }
    done(r)
}

fn position(r: &OfflineRenderer) -> i64 {
    r.controller.transport_snapshot().position
}

// --- the tests -------------------------------------------------------------------------

/// Opening a device's editor takes its track out of the render-ahead graph
/// while playing (and closing it, once stopped, puts it back): the
/// arpeggiator moves with it and ends the notes it left sounding.
#[test]
fn an_arpeggiator_moving_between_the_graphs_ends_its_notes() {
    install();
    let id = "test.voices.ahead";
    let mut tp = TestProject::new(SR);
    tp.project.timeline.tempo = faderframe_timeline::TempoMap::new(120.0);
    let t = tp.track(TrackKind::Instrument, "Arp", ChannelLayout::Stereo);
    // Legato: one of its notes always sounds.
    let arp = builtin_slot(
        &mut tp,
        builtin::ARPEGGIATOR,
        &[(arpeggiator::id::GATE, 1.5)],
    );
    let voices = voices_slot(&mut tp, id);
    tp.project.track_mut(t).unwrap().inserts = vec![arp, voices];
    chords(
        &mut tp,
        t,
        &[(0.0, 4.0, &[60, 64, 67]), (4.0, 4.0, &[62, 65, 69])],
    );
    let mut r = renderer(&tp);
    // As in a live session: devices are made for the audio thread there.
    r.controller.plugins().set_realtime(true);
    r.controller
        .set_render_ahead(Some(Duration::from_millis(100)), 1);
    r.controller
        .sync(&tp.project, &tp.sources, Impact::Graph)
        .unwrap();
    assert!(r.controller.ahead_tracks().contains(&t));
    r.play_from(0).unwrap();
    let heard = heard(id);
    // Rendered ahead, it plays the first chord.
    assert!(run_until(&mut r, |_| heard.ons() >= 2));
    assert!(position(&r) < BAR / 2, "{}", position(&r));
    // An editor of the track opens: it plays on the audio thread now.
    r.controller.set_edited_tracks(HashSet::from([t]));
    r.controller.update_params(&tp.project).unwrap();
    assert!(!r.controller.ahead_tracks().contains(&t));
    for _ in 0..8 {
        r.step();
    }
    assert!(position(&r) < BAR, "{}", position(&r));
    assert_eq!(
        heard.sounding(),
        [] as [u8; 0],
        "the first chord's notes ended"
    );
    // The second chord, played live.
    assert!(run_until(&mut r, |r| position(r) > BAR + 3 * SIXTEENTH));
    assert!(!heard.sounding().is_empty());
    r.controller.transport(TransportCommand::Stop).unwrap();
    assert!(run_until(&mut r, |r| !r
        .controller
        .transport_snapshot()
        .playing));
    // Closed while stopped: rendered ahead again, its last note ends there.
    r.controller.set_edited_tracks(HashSet::new());
    assert!(r.controller.ahead_wants_rebuild(&tp.project));
    r.controller.update_params(&tp.project).unwrap();
    assert!(r.controller.ahead_tracks().contains(&t));
    assert!(
        run_until(&mut r, |_| heard.sounding().is_empty()),
        "stuck: {:?}",
        heard.sounding()
    );
}

/// Blocks that never reach the instrument (another graph's turn with it, a
/// sandbox helper still busy with an earlier block) lose no note-off and no
/// pedal let go: they reach it with its next block.
#[test]
fn what_missed_blocks_end_reaches_the_instrument_with_the_next() {
    install();
    let id = "test.voices.missed";
    let mut tp = TestProject::new(SR);
    tp.project.timeline.tempo = faderframe_timeline::TempoMap::new(120.0);
    let t = tp.track(TrackKind::Instrument, "Keys", ChannelLayout::Stereo);
    let voices = voices_slot(&mut tp, id);
    tp.project.track_mut(t).unwrap().inserts = vec![voices];
    // A note for a sixteenth, the pedal down from the start and up with
    // the note's end.
    chords(&mut tp, t, &[(0.0, 0.25, &[60])]);
    let clip = tp.project.track(t).unwrap().clips[0];
    if let Some(ClipContent::Midi(m)) = tp.project.clips.get_mut(&clip).map(|c| &mut c.content) {
        let point = |q: f64, value: u16| ControllerPoint {
            time: MusicalTime::from_quarters(q),
            value,
        };
        m.controllers.push(ControllerLane {
            controller: MidiController::Cc { number: 64 },
            channel: 0,
            points: vec![point(0.0, 127), point(0.25, 0)],
        });
    }
    let mut r = renderer(&tp);
    r.play_from(0).unwrap();
    let heard = heard(id);
    r.step();
    assert_eq!(heard.sounding(), [60]);
    assert!(heard.pedal.load(Ordering::SeqCst));
    // The blocks with the note's end and the pedal's never reach it.
    while position(&r) < SIXTEENTH - BLOCK as i64 {
        r.step();
    }
    heard.miss.store(true, Ordering::SeqCst);
    for _ in 0..4 {
        r.step();
    }
    assert!(position(&r) > SIXTEENTH);
    assert_eq!(heard.sounding(), [60], "missed");
    heard.miss.store(false, Ordering::SeqCst);
    r.step();
    assert_eq!(heard.sounding(), [] as [u8; 0], "ended with the next block");
    assert!(!heard.pedal.load(Ordering::SeqCst), "the pedal let go too");
}

/// An arpeggiator playing a held chord into the test instrument, at 120
/// BPM, legato (one of its notes always sounds).
fn arp_rig(id: &str) -> (TestProject, TrackId, faderframe_core::PluginInstanceId) {
    install();
    let mut tp = TestProject::new(SR);
    tp.project.timeline.tempo = faderframe_timeline::TempoMap::new(120.0);
    let t = tp.track(TrackKind::Instrument, "Arp", ChannelLayout::Stereo);
    let arp = builtin_slot(
        &mut tp,
        builtin::ARPEGGIATOR,
        &[(arpeggiator::id::GATE, 1.5)],
    );
    let arp_id = arp.id;
    let voices = voices_slot(&mut tp, id);
    tp.project.track_mut(t).unwrap().inserts = vec![arp, voices];
    chords(&mut tp, t, &[(0.0, 12.0, &[60, 64, 67])]);
    (tp, t, arp_id)
}

/// Steps until late in arpeggiator step `k` (its notes overlap the next
/// step's first half: then only one sounds).
fn late_in_step(r: &mut OfflineRenderer, k: i64) {
    while position(r) < k * SIXTEENTH + SIXTEENTH * 3 / 4 {
        r.step();
    }
}

/// Bypassing the arpeggiator while it plays (issue #27) ends the note it
/// had sounding, and the held chord passes it by; playing it again ends
/// the chord and arpeggiates the keys held; nothing stays after stopping.
#[test]
fn bypassing_an_arpeggiator_while_it_plays_leaves_no_note_sounding() {
    let id = "test.voices.bypass";
    let (mut tp, t, _) = arp_rig(id);
    let mut r = renderer(&tp);
    r.play_from(0).unwrap();
    let heard = heard(id);
    late_in_step(&mut r, 8);
    assert_eq!(heard.sounding().len(), 1, "{:?}", heard.sounding());
    let bypass = |tp: &mut TestProject, r: &mut OfflineRenderer, on: bool| {
        tp.project.track_mut(t).unwrap().inserts[0].bypass = on;
        r.controller
            .sync(&tp.project, &tp.sources, Impact::Graph)
            .unwrap();
        r.step();
        r.step();
    };
    bypass(&mut tp, &mut r, true);
    assert_eq!(heard.sounding(), [60, 64, 67], "the chord as it is");
    late_in_step(&mut r, 14);
    assert_eq!(heard.sounding(), [60, 64, 67]);
    bypass(&mut tp, &mut r, false);
    assert!(
        heard.sounding().len() <= 1,
        "the chord ended: {:?}",
        heard.sounding()
    );
    // It arpeggiates the held chord again.
    let ons = heard.ons();
    late_in_step(&mut r, 16);
    assert!(heard.ons() > ons);
    assert_eq!(heard.sounding().len(), 1, "{:?}", heard.sounding());
    late_in_step(&mut r, 17);
    assert_eq!(heard.sounding().len(), 1, "{:?}", heard.sounding());
    // Bypassed and stopped at once.
    bypass(&mut tp, &mut r, true);
    r.controller.transport(TransportCommand::Stop).unwrap();
    assert!(
        run_until(&mut r, |_| heard.sounding().is_empty()),
        "stuck: {:?}",
        heard.sounding()
    );
}

/// Automated bypass (the arpeggiator keeps running, its notes pass it by
/// while the lane says so) ends what the other side left sounding at each
/// switch.
#[test]
fn an_automated_arpeggiator_bypass_leaves_no_note_sounding() {
    use faderframe_automation::{
        AutomationCurve, AutomationLane, AutomationMode, AutomationPoint, AutomationTarget,
        CurveShape,
    };
    let id = "test.voices.automated";
    let (mut tp, t, arp) = arp_rig(id);
    let point = |q: f64, value: f64| AutomationPoint {
        time: MusicalTime::from_quarters(q),
        value,
        shape: CurveShape::Step,
    };
    let lane = tp.project.ids.allocate();
    tp.project
        .track_mut(t)
        .unwrap()
        .automation
        .lanes
        .push(AutomationLane {
            id: lane,
            target: AutomationTarget::PluginBypass(arp),
            // Bypassed from the middle of step 4 to that of step 8.
            curve: AutomationCurve::from_points(vec![
                point(0.0, 0.0),
                point(1.1, 1.0),
                point(2.1, 0.0),
            ]),
            mode: AutomationMode::Read,
            visible: true,
        });
    let mut r = renderer(&tp);
    r.play_from(0).unwrap();
    let heard = heard(id);
    late_in_step(&mut r, 3);
    assert_eq!(heard.sounding().len(), 1, "arpeggiated");
    late_in_step(&mut r, 6);
    assert_eq!(heard.sounding(), [60, 64, 67], "passed by");
    late_in_step(&mut r, 10);
    assert_eq!(heard.sounding().len(), 1, "arpeggiated again");
    r.controller.transport(TransportCommand::Stop).unwrap();
    assert!(
        run_until(&mut r, |_| heard.sounding().is_empty()),
        "stuck: {:?}",
        heard.sounding()
    );
}

/// Taking the arpeggiator out while it plays ends the note it had
/// sounding at the instrument (the instrument, fed by something else now,
/// ends what it was given), and putting it back arpeggiates again.
#[test]
fn removing_an_arpeggiator_while_it_plays_leaves_no_note_sounding() {
    let id = "test.voices.removed";
    let (mut tp, t, _) = arp_rig(id);
    let mut r = renderer(&tp);
    r.play_from(0).unwrap();
    let heard = heard(id);
    late_in_step(&mut r, 6);
    assert_eq!(heard.sounding().len(), 1, "{:?}", heard.sounding());
    let arp = tp.project.track_mut(t).unwrap().inserts.remove(0);
    r.controller
        .sync(&tp.project, &tp.sources, Impact::Graph)
        .unwrap();
    r.step();
    r.step();
    assert_eq!(
        heard.sounding(),
        [] as [u8; 0],
        "the arpeggiated note ended"
    );
    late_in_step(&mut r, 10);
    assert_eq!(heard.sounding(), [] as [u8; 0], "nothing stuck");
    tp.project.track_mut(t).unwrap().inserts.insert(0, arp);
    r.controller
        .sync(&tp.project, &tp.sources, Impact::Graph)
        .unwrap();
    late_in_step(&mut r, 13);
    r.controller.transport(TransportCommand::Stop).unwrap();
    assert!(
        run_until(&mut r, |_| heard.sounding().is_empty()),
        "stuck: {:?}",
        heard.sounding()
    );
}

/// A MIDI track playing an instrument track, taken away while its note
/// sounds: the note ends.
#[test]
fn a_midi_track_taken_away_while_it_plays_leaves_no_note_sounding() {
    install();
    let id = "test.voices.midi-track";
    let mut tp = TestProject::new(SR);
    tp.project.timeline.tempo = faderframe_timeline::TempoMap::new(120.0);
    let keys = tp.track(TrackKind::Instrument, "Keys", ChannelLayout::Stereo);
    let voices = voices_slot(&mut tp, id);
    tp.project.track_mut(keys).unwrap().inserts = vec![voices];
    let m = tp.track(TrackKind::Midi, "Notes", ChannelLayout::Stereo);
    tp.project.track_mut(m).unwrap().output =
        faderframe_project::OutputRouting::Track { track: keys };
    chords(&mut tp, m, &[(0.0, 8.0, &[64])]);
    let mut r = renderer(&tp);
    r.play_from(0).unwrap();
    let heard = heard(id);
    late_in_step(&mut r, 2);
    assert_eq!(heard.sounding(), [64]);
    tp.project.track_mut(m).unwrap().output = faderframe_project::OutputRouting::Master;
    r.controller
        .sync(&tp.project, &tp.sources, Impact::Graph)
        .unwrap();
    r.step();
    r.step();
    assert_eq!(heard.sounding(), [] as [u8; 0]);
}
