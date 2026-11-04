//! Retuning: how a track's tuning (`faderframe_project::tuning`) reaches
//! what plays its notes.
//!
//! The timeline snapshot carries each retuned track's [`Retuning`] — its
//! key table and that table as MIDI Tuning Standard SysEx, made on the
//! control thread. Each node that plays notes has a [`Retuner`] with the
//! [`Method`] chosen when the graph was built:
//!
//! * [`Method::Builtin`]: the processor is given the table
//!   (`PluginProcessor::set_tuning`) when it changes.
//! * [`Method::Mts`]: a bulk tuning dump goes to the plugin before the
//!   first notes, real-time single note changes when the tuning changes.
//!   What the plugin was last told is the instance's ([`TunedPlugin`]), so
//!   a rebuilt graph does not tell it again, and a plugin that no longer
//!   takes MTS (another method, retuning off) is told twelve-tone equal
//!   temperament once.
//! * [`Method::Expression`]: every note-on is followed by a tuning note
//!   expression (the key's offset in semitones); the note's own tuning
//!   expressions (pitch curves) get the offset added. External MIDI 2.0
//!   ports send it as per-note pitch; MIDI 1.0 ports drop it.
//! * [`Method::Mpe`]: each note's member-channel pitch bend carries its
//!   key's offset — before its note-on, and on every bend of the channel
//!   while its key is the channel's.
//!
//! Keys a tuning leaves unmapped play nothing: their note-ons are dropped
//! (MTS cannot silence a key; they go out at their own pitch there).
//! Everything on the audio thread is copying and arithmetic.

use faderframe_core::TrackId;
use faderframe_midi::tuning::{TuningTable, mts};
use faderframe_midi::{ExpressionValue, MidiBuffer, MidiEvent, NoteExpressionKind, TimedMidiEvent};
use faderframe_project::tuning::Retune;
use faderframe_project::{MpeConfig, PluginFormat, Project, Track};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// The smallest offset worth a note expression (a thousandth of a cent).
const NEGLIGIBLE: f64 = 1e-5;

/// A track's tuning as the audio thread plays it.
#[derive(Debug)]
pub struct Retuning {
    pub table: TuningTable,
    pub fingerprint: u64,
    /// Twelve-tone equal temperament at 440 Hz (nothing to do).
    pub standard: bool,
    /// The table as a bulk tuning dump (program 0).
    pub bulk: Box<[u8]>,
    /// The table as real-time single note changes.
    pub changes: Box<[Box<[u8]>]>,
    /// The track's MPE zone.
    pub mpe: Option<MpeConfig>,
}

impl Retuning {
    /// Made on the control thread (allocates the SysEx).
    pub fn new(table: TuningTable, name: &str, mpe: Option<MpeConfig>) -> Self {
        Self {
            fingerprint: table.fingerprint(),
            standard: table.is_standard(),
            bulk: mts::bulk_dump(&table, 0, name).into_boxed_slice(),
            changes: mts::note_changes(&table, 0)
                .into_iter()
                .map(Vec::into_boxed_slice)
                .collect(),
            table,
            mpe,
        }
    }

    /// Twelve-tone equal temperament at 440 Hz.
    pub fn standard() -> Self {
        Self::new(TuningTable::STANDARD, "12-TET", None)
    }
}

/// The tunings of a timeline snapshot: the retuned tracks', by track.
#[derive(Debug)]
pub struct Tunings {
    tracks: Vec<(TrackId, Arc<Retuning>)>,
    standard: Arc<Retuning>,
}

impl Default for Tunings {
    fn default() -> Self {
        Self {
            tracks: Vec::new(),
            standard: Arc::new(Retuning::standard()),
        }
    }
}

impl Tunings {
    /// Every track of `project` that plays a tuning (control thread).
    pub fn build(project: &Project) -> Self {
        let mut tracks: Vec<(TrackId, Arc<Retuning>)> = project
            .tracks
            .iter()
            .filter_map(|t| {
                let table = project.tuning_table(t.id);
                (!table.is_standard()).then(|| {
                    let name = project.tuning_name(t.id);
                    (t.id, Arc::new(Retuning::new(table, &name, t.mpe)))
                })
            })
            .collect();
        tracks.sort_by_key(|(t, _)| *t);
        Self {
            tracks,
            ..Self::default()
        }
    }

    /// What `track` plays (twelve-tone equal temperament when it is not
    /// retuned). Realtime-safe: a binary search.
    #[inline]
    pub fn of(&self, track: TrackId) -> &Retuning {
        match self.tracks.binary_search_by_key(&track, |(t, _)| *t) {
            Ok(i) => &self.tracks[i].1,
            Err(_) => &self.standard,
        }
    }

    /// Twelve-tone equal temperament at 440 Hz.
    #[inline]
    pub fn standard(&self) -> &Retuning {
        &self.standard
    }

    /// The tracks that play a tuning.
    pub fn tracks(&self) -> impl Iterator<Item = TrackId> + '_ {
        self.tracks.iter().map(|(t, _)| *t)
    }
}

/// How a node passes its track's tuning on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Method {
    /// Not at all (it takes no notes).
    #[default]
    None,
    /// The processor reads the table.
    Builtin,
    /// MIDI Tuning Standard SysEx.
    Mts,
    /// A tuning note expression per note.
    Expression,
    /// Member-channel pitch bend.
    Mpe,
}

impl Method {
    /// For node keys (a method change is a different node).
    pub fn key(self) -> u64 {
        match self {
            Self::None => 0,
            Self::Builtin => 1,
            Self::Mts => 2,
            Self::Expression => 3,
            Self::Mpe => 4,
        }
    }

    /// Does it rewrite the events (rather than only adding SysEx)?
    fn rewrites(self) -> bool {
        matches!(self, Self::Expression | Self::Mpe)
    }
}

/// How a plugin that takes notes on `track` is retuned: built-ins read
/// the table (whatever [`Track::retune`] says); hosted plugins as the
/// track says — automatically MPE on MPE tracks, else note expressions for
/// plugins that take tuning per note (CLAP; VST3 listing it), else MTS.
pub fn method_for(
    track: &Track,
    format: PluginFormat,
    expressions: Option<&[NoteExpressionKind]>,
) -> Method {
    if format == PluginFormat::Builtin {
        return Method::Builtin;
    }
    let takes_tuning = match expressions {
        Some(kinds) => kinds.contains(&NoteExpressionKind::Tuning),
        // CLAP has no list: its note dialect carries tuning expressions.
        None => format == PluginFormat::Clap,
    };
    match track.retune {
        Retune::Off => Method::None,
        Retune::Mts => Method::Mts,
        Retune::Expression => Method::Expression,
        Retune::Mpe if track.mpe.is_some() => Method::Mpe,
        Retune::Auto if track.mpe.is_some() => Method::Mpe,
        Retune::Auto | Retune::Mpe if takes_tuning => Method::Expression,
        Retune::Auto | Retune::Mpe => Method::Mts,
    }
}

/// What a track's external MIDI output gets in the graph: MPE bends on MPE
/// tracks, else tuning expressions (MIDI 2.0 ports send them as per-note
/// pitch). MTS goes to MIDI 1.0 ports from the control side
/// (`faderframe_session`), not through the graph.
pub fn output_method(track: &Track) -> Method {
    match track.retune {
        Retune::Off | Retune::Mts => Method::None,
        Retune::Expression => Method::Expression,
        Retune::Auto | Retune::Mpe if track.mpe.is_some() => Method::Mpe,
        Retune::Auto | Retune::Mpe => Method::Expression,
    }
}

/// What MIDI Tuning Standard told a plugin instance last (shared by its
/// nodes in every graph).
#[derive(Debug, Default)]
pub struct TunedPlugin {
    /// The fingerprint of the table sent (0: none yet).
    mts: AtomicU64,
}

impl TunedPlugin {
    /// The fingerprint of the table MTS sent it last (0: none).
    pub fn mts_sent(&self) -> u64 {
        self.mts.load(Ordering::Acquire)
    }

    /// The instance was made again (its tuning is its own once more).
    pub fn forget(&self) {
        self.mts.store(0, Ordering::Release);
    }
}

/// A node's retuning (see the module documentation).
pub struct Retuner {
    method: Method,
    shared: Arc<TunedPlugin>,
    /// The fingerprint of the table given to a built-in (0: none yet).
    given: u64,
    /// MPE: per channel the key its notes play (the sounding one, else
    /// the last), the bend as it came in, and where in this block a bend
    /// already went out.
    keys: [Option<u8>; 16],
    bends: [u16; 16],
    bent: [Option<u32>; 16],
    /// The events as they came in, while rewriting them.
    scratch: MidiBuffer,
}

impl Retuner {
    /// Made with the node (control thread).
    pub fn new(method: Method, shared: Arc<TunedPlugin>) -> Self {
        let (events, sysex) = if method.rewrites() {
            (
                MidiBuffer::DEFAULT_CAPACITY,
                MidiBuffer::DEFAULT_SYSEX_CAPACITY,
            )
        } else {
            (1, 0)
        };
        Self {
            method,
            shared,
            given: 0,
            keys: [None; 16],
            bends: [MidiEvent::PITCH_BEND_CENTRE; 16],
            bent: [None; 16],
            scratch: MidiBuffer::with_capacities(events, sysex),
        }
    }

    /// A retuner that does nothing.
    pub fn none() -> Self {
        Self::new(Method::None, Arc::default())
    }

    pub fn method(&self) -> Method {
        self.method
    }

    /// The table a built-in is to be given now (`None`: it has it).
    pub fn to_give<'a>(&mut self, tuning: &'a Retuning) -> Option<&'a TuningTable> {
        if self.method != Method::Builtin || self.given == tuning.fingerprint {
            return None;
        }
        self.given = tuning.fingerprint;
        Some(&tuning.table)
    }

    /// Do this block's events need [`Self::apply`]?
    pub fn wants(&self, tuning: &Retuning, standard: &Retuning) -> bool {
        let sent = self.shared.mts_sent();
        match self.method {
            Method::Mts => sent != tuning.fingerprint && !(sent == 0 && tuning.standard),
            m => (m.rewrites() && !tuning.standard) || (sent != 0 && sent != standard.fingerprint),
        }
    }

    /// Retune a block's `events` in place: MTS first (at its start), then
    /// the notes. Realtime-safe; what does not fit is dropped (and an MTS
    /// message that did not fit is tried again next block).
    pub fn apply(&mut self, tuning: &Retuning, standard: &Retuning, events: &mut MidiBuffer) {
        if self.method.rewrites() && !tuning.standard {
            self.scratch.clear();
            self.scratch.merge_from(events, 0);
            events.clear();
            match self.method {
                Method::Mpe => self.rewrite_mpe(tuning, events),
                _ => self.rewrite_expression(tuning, events),
            }
        }
        let sent = self.shared.mts_sent();
        let told = match self.method {
            Method::Mts if sent != tuning.fingerprint && !(sent == 0 && tuning.standard) => {
                let ok = if sent == 0 {
                    events.push_sysex(0, &tuning.bulk).is_ok()
                } else {
                    tuning
                        .changes
                        .iter()
                        .all(|m| events.push_sysex(0, m).is_ok())
                };
                ok.then_some(tuning.fingerprint)
            }
            Method::Mts => None,
            // No longer retuned by MTS: back to equal temperament once.
            _ if sent != 0 && sent != standard.fingerprint => standard
                .changes
                .iter()
                .all(|m| events.push_sysex(0, m).is_ok())
                .then_some(standard.fingerprint),
            _ => None,
        };
        if let Some(fp) = told {
            self.shared.mts.store(fp, Ordering::Release);
        }
    }

    /// Note-ons followed by their key's offset as a tuning expression; the
    /// notes' own tuning expressions moved by it.
    fn rewrite_expression(&mut self, tuning: &Retuning, out: &mut MidiBuffer) {
        let table = &tuning.table;
        for e in self.scratch.iter() {
            match e.event {
                MidiEvent::NoteOn { key, .. } if !table.is_mapped(key) => {}
                MidiEvent::NoteOn { channel, key, .. } => {
                    let _ = out.push(*e);
                    let offset = table.offset(key);
                    if offset.abs() > NEGLIGIBLE {
                        let _ = out.push(TimedMidiEvent::new(
                            e.sample_offset,
                            MidiEvent::NoteExpression {
                                channel,
                                key,
                                kind: NoteExpressionKind::Tuning,
                                value: ExpressionValue::new(offset),
                            },
                        ));
                    }
                }
                MidiEvent::NoteExpression {
                    channel,
                    key,
                    kind: NoteExpressionKind::Tuning,
                    value,
                } => {
                    let _ = out.push(TimedMidiEvent::new(
                        e.sample_offset,
                        MidiEvent::NoteExpression {
                            channel,
                            key,
                            kind: NoteExpressionKind::Tuning,
                            value: ExpressionValue::new(value.get() + table.offset(key)),
                        },
                    ));
                }
                _ => {
                    let _ = out.push_from(&self.scratch, *e);
                }
            }
        }
    }

    /// Member channels' bends carrying their key's offset.
    fn rewrite_mpe(&mut self, tuning: &Retuning, out: &mut MidiBuffer) {
        let cfg = tuning.mpe.unwrap_or_default();
        let members = usize::from(cfg.members.clamp(1, 15));
        let range = f64::from(cfg.bend_range.max(1));
        let table = &tuning.table;
        let bend = |value: u16, key: u8| -> u16 {
            (f64::from(value) + table.offset(key) / range * 8192.0)
                .round()
                .clamp(0.0, 16383.0) as u16
        };
        self.bent = [None; 16];
        let events = self.scratch.as_slice();
        for (i, e) in events.iter().enumerate() {
            let member = |c: u8| (1..=members).contains(&usize::from(c));
            match e.event {
                MidiEvent::PitchBend { channel, value } if member(channel) => {
                    let c = usize::from(channel);
                    self.bends[c] = value;
                    // The note it is for: one starting on the channel right
                    // now (bends come before note-ons), else the channel's.
                    let starting = events[i + 1..]
                        .iter()
                        .take_while(|n| n.sample_offset == e.sample_offset)
                        .find_map(|n| match n.event {
                            MidiEvent::NoteOn {
                                channel: c2, key, ..
                            } if c2 == channel => Some(key),
                            _ => None,
                        });
                    let value = match starting.or(self.keys[c]) {
                        Some(key) => bend(value, key),
                        None => value,
                    };
                    self.bent[c] = Some(e.sample_offset);
                    let _ = out.push(TimedMidiEvent::new(
                        e.sample_offset,
                        MidiEvent::PitchBend { channel, value },
                    ));
                }
                MidiEvent::NoteOn { key, .. } if !table.is_mapped(key) => {}
                MidiEvent::NoteOn { channel, key, .. } if member(channel) => {
                    let c = usize::from(channel);
                    if self.bent[c] != Some(e.sample_offset) {
                        // Sorted before the note-on (same offset).
                        let _ = out.push(TimedMidiEvent::new(
                            e.sample_offset,
                            MidiEvent::PitchBend {
                                channel,
                                value: bend(self.bends[c], key),
                            },
                        ));
                    }
                    self.keys[c] = Some(key);
                    let _ = out.push(*e);
                }
                _ => {
                    let _ = out.push_from(&self.scratch, *e);
                }
            }
        }
    }

    /// Forget the channels' notes (a reset).
    pub fn reset(&mut self) {
        self.keys = [None; 16];
        self.bends = [MidiEvent::PITCH_BEND_CENTRE; 16];
        self.bent = [None; 16];
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use faderframe_midi::tuning::Tuning;

    fn block(events: &[(u32, MidiEvent)]) -> MidiBuffer {
        let mut b = MidiBuffer::with_capacity(64);
        for (at, e) in events {
            b.push(TimedMidiEvent::new(*at, *e)).unwrap();
        }
        b
    }

    fn on(channel: u8, key: u8) -> MidiEvent {
        MidiEvent::NoteOn {
            channel,
            key,
            velocity: 100,
        }
    }

    fn listed(b: &MidiBuffer) -> Vec<(u32, MidiEvent)> {
        b.iter().map(|e| (e.sample_offset, e.event)).collect()
    }

    #[test]
    fn note_ons_get_their_offset_as_a_tuning_expression() {
        let tuning = Retuning::new(Tuning::equal(24).table(440.0), "24", None);
        let standard = Retuning::standard();
        let mut r = Retuner::new(Method::Expression, Arc::default());
        assert!(r.wants(&tuning, &standard));
        assert!(!r.wants(&standard, &standard), "nothing to do in 12-TET");
        // Key 61 is a quarter tone above middle C: 0.5 − 1 semitones off.
        let mut b = block(&[
            (3, on(0, 61)),
            (
                9,
                MidiEvent::NoteExpression {
                    channel: 0,
                    key: 61,
                    kind: NoteExpressionKind::Tuning,
                    value: ExpressionValue::new(0.25),
                },
            ),
        ]);
        r.apply(&tuning, &standard, &mut b);
        let expression = |at, v: f64| {
            (
                at,
                MidiEvent::NoteExpression {
                    channel: 0,
                    key: 61,
                    kind: NoteExpressionKind::Tuning,
                    value: ExpressionValue::new(v),
                },
            )
        };
        assert_eq!(
            listed(&b),
            [(3, on(0, 61)), expression(3, -0.5), expression(9, -0.25)]
        );
    }

    #[test]
    fn mpe_bends_carry_the_offset_of_their_channels_key() {
        let mpe = MpeConfig {
            members: 15,
            bend_range: 48,
        };
        let tuning = Retuning::new(Tuning::equal(24).table(440.0), "24", Some(mpe));
        let standard = Retuning::standard();
        let mut r = Retuner::new(Method::Mpe, Arc::default());
        // Key 61 (−0.5 st): its initial bend (centre) before its note-on,
        // then a bend up a semitone; key 62 (−1 st) without a bend first.
        let step = 8192.0 / 48.0;
        let at = |semis: f64| (8192.0 + semis * step).round() as u16;
        let moved = |v: u16, semis: f64| (f64::from(v) + semis * step).round() as u16;
        let mut b = block(&[
            (
                0,
                MidiEvent::PitchBend {
                    channel: 1,
                    value: 8192,
                },
            ),
            (0, on(1, 61)),
            (0, on(2, 62)),
            (
                40,
                MidiEvent::PitchBend {
                    channel: 1,
                    value: at(1.0),
                },
            ),
            // The master channel's bend is everyone's: left alone.
            (
                50,
                MidiEvent::PitchBend {
                    channel: 0,
                    value: 9000,
                },
            ),
        ]);
        r.apply(&tuning, &standard, &mut b);
        let bend = |channel, value| MidiEvent::PitchBend { channel, value };
        assert_eq!(
            listed(&b),
            [
                (0, bend(1, at(-0.5))),
                (0, bend(2, at(-1.0))),
                (0, on(1, 61)),
                (0, on(2, 62)),
                (40, bend(1, moved(at(1.0), -0.5))),
                (50, bend(0, 9000)),
            ]
        );
        // The next block: a bend on channel 2 is for key 62 still.
        let mut b = block(&[(5, bend(2, at(2.0)))]);
        r.apply(&tuning, &standard, &mut b);
        assert_eq!(listed(&b), [(5, bend(2, moved(at(2.0), -1.0)))]);
    }

    #[test]
    fn unmapped_keys_play_nothing() {
        let mut map = faderframe_midi::tuning::KeyboardMap::standard(12);
        map.size = 2;
        map.map = vec![Some(0), None];
        map.octave_degree = 1;
        let t = Tuning::equal(12).with_mapping(Some(map)).unwrap();
        let tuning = Retuning::new(t.table(440.0), "x", None);
        let standard = Retuning::standard();
        for method in [Method::Expression, Method::Mpe] {
            let mut r = Retuner::new(method, Arc::default());
            let mut b = block(&[(0, on(1, 61)), (0, on(1, 62))]);
            r.apply(&tuning, &standard, &mut b);
            assert!(
                listed(&b)
                    .iter()
                    .all(|(_, e)| !matches!(e, MidiEvent::NoteOn { key: 61, .. })),
                "{method:?}"
            );
            assert!(listed(&b).contains(&(0, on(1, 62))));
        }
    }

    #[test]
    fn mts_is_told_once_then_by_changes_and_reset_by_another_method() {
        let shared = Arc::new(TunedPlugin::default());
        let tuning = Retuning::new(Tuning::equal(19).table(440.0), "19-EDO", None);
        let other = Retuning::new(Tuning::equal(31).table(440.0), "31-EDO", None);
        let standard = Retuning::standard();
        let mut r = Retuner::new(Method::Mts, Arc::clone(&shared));
        assert!(!r.wants(&standard, &standard), "12-TET: nothing to tell");
        assert!(r.wants(&tuning, &standard));
        let mut b = block(&[(10, on(0, 60))]);
        r.apply(&tuning, &standard, &mut b);
        let sysex: Vec<Vec<u8>> = b
            .iter()
            .filter_map(|e| match e.event {
                MidiEvent::SysEx(s) => Some(b.sysex(&s).unwrap().to_vec()),
                _ => None,
            })
            .collect();
        assert_eq!(sysex, [tuning.bulk.to_vec()], "a bulk dump first");
        assert_eq!(b.as_slice()[0].sample_offset, 0, "before the notes");
        assert!(!r.wants(&tuning, &standard), "told");
        // Another graph's node of the same plugin knows it was told.
        let again = Retuner::new(Method::Mts, Arc::clone(&shared));
        assert!(!again.wants(&tuning, &standard));
        // A new tuning: real-time changes.
        let mut b = MidiBuffer::with_capacity(8);
        r.apply(&other, &standard, &mut b);
        assert_eq!(b.len(), other.changes.len());
        assert_eq!(shared.mts_sent(), other.fingerprint);
        // Retuned another way now: told 12-TET once.
        let mut e = Retuner::new(Method::Expression, Arc::clone(&shared));
        assert!(e.wants(&standard, &standard));
        let mut b = MidiBuffer::with_capacity(8);
        e.apply(&standard, &standard, &mut b);
        assert_eq!(b.len(), standard.changes.len());
        assert!(!e.wants(&standard, &standard));
    }

    #[test]
    fn built_ins_are_given_the_table_when_it_changes() {
        let tuning = Retuning::new(Tuning::equal(19).table(440.0), "19", None);
        let standard = Retuning::standard();
        let mut r = Retuner::new(Method::Builtin, Arc::default());
        assert!(r.to_give(&tuning).is_some());
        assert!(r.to_give(&tuning).is_none());
        assert_eq!(r.to_give(&standard), Some(&TuningTable::STANDARD));
        assert!(!r.wants(&tuning, &standard));
        let mut none = Retuner::none();
        assert!(none.to_give(&tuning).is_none());
    }

    #[test]
    fn methods_follow_the_track_and_the_plugin() {
        use faderframe_project::{TrackColor, TrackKind};
        let mut t = Track::new(
            TrackId(1),
            TrackKind::Instrument,
            "K",
            TrackColor::palette(0),
        );
        let all = NoteExpressionKind::ALL;
        let none: &[NoteExpressionKind] = &[];
        assert_eq!(method_for(&t, PluginFormat::Builtin, None), Method::Builtin);
        assert_eq!(method_for(&t, PluginFormat::Clap, None), Method::Expression);
        assert_eq!(
            method_for(&t, PluginFormat::Vst3, Some(&all)),
            Method::Expression
        );
        assert_eq!(method_for(&t, PluginFormat::Vst3, Some(none)), Method::Mts);
        assert_eq!(method_for(&t, PluginFormat::Lv2, None), Method::Mts);
        t.mpe = Some(MpeConfig::default());
        assert_eq!(method_for(&t, PluginFormat::Clap, None), Method::Mpe);
        assert_eq!(output_method(&t), Method::Mpe);
        t.retune = Retune::Mts;
        assert_eq!(method_for(&t, PluginFormat::Clap, None), Method::Mts);
        assert_eq!(output_method(&t), Method::None, "MTS goes from the session");
        t.retune = Retune::Off;
        assert_eq!(method_for(&t, PluginFormat::Clap, None), Method::None);
        assert_eq!(method_for(&t, PluginFormat::Builtin, None), Method::Builtin);
        t.mpe = None;
        t.retune = Retune::Mpe;
        assert_eq!(method_for(&t, PluginFormat::Vst3, Some(none)), Method::Mts);
        assert_eq!(output_method(&t), Method::Expression);
    }
}
