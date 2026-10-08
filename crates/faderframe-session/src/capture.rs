//! Capture MIDI: whatever the tracks that play live are played is kept for
//! a while, recording or not, so a phrase played without recording can
//! still become a clip ("Capture MIDI", Ctrl+Shift+C).
//!
//! The session already sees a copy of every incoming message; each one
//! that reaches a live track is kept with the tracks it reached and — while
//! the transport runs — the timeline position where it was heard (as a
//! recording would place it: the processing position at its time, less the
//! output latency and the recording offset; loop wraps start a new pass).
//! Starting or stopping the transport begins a new run.
//!
//! A capture takes the newest run since the previous capture: played while
//! playing, the notes land where they were played (loop passes as the
//! loop-record setting says); played while stopped, the latest phrase (back
//! to a pause of [`PHRASE_GAP`]) starts on the bar at or before the
//! playhead, its timing kept. The messages go through the MIDI take
//! machinery that recording uses, so notes, controllers and the record mode
//! behave the same, and the clips are one undo step.

use crate::midi::MidiTake;
use crate::{NoticeLevel, Result, SelectMode, Session, SessionError};
use faderframe_core::TrackId;
use faderframe_engine::midi::RecordedMidi;
use faderframe_midi::{MidiEvent, MidiInputEvent};
use faderframe_project::{Command, InputRouting};
use std::collections::VecDeque;

/// How long played MIDI is kept (nanoseconds): ten minutes.
const KEEP_NS: u64 = 600 * 1_000_000_000;
/// At most this many messages are kept.
const KEEP_EVENTS: usize = 200_000;
/// A pause this long ends a phrase played while stopped (nanoseconds).
pub const PHRASE_GAP: u64 = 8 * 1_000_000_000;

/// One message a live track took.
#[derive(Clone, Debug)]
struct Played {
    time_ns: u64,
    event: MidiEvent,
    /// Timeline sample where it was heard (while playing).
    position: Option<i64>,
    /// Transport runs (a start or stop begins a new one).
    run: u32,
    /// Loop passes within a run.
    pass: u32,
    tracks: Vec<TrackId>,
}

/// The messages kept for capturing.
#[derive(Debug, Default)]
pub(crate) struct CaptureBuffer {
    played: VecDeque<Played>,
    run: u32,
    playing: bool,
    pass: u32,
    last_position: Option<i64>,
    /// Messages up to this time were captured (or recorded) already.
    taken_until: u64,
}

impl CaptureBuffer {
    /// The phrase a capture would take (oldest first).
    fn phrase(&self) -> Vec<&Played> {
        let Some(newest) = self.played.back() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut later = newest.time_ns;
        for p in self.played.iter().rev() {
            if p.run != newest.run || p.time_ns <= self.taken_until {
                break;
            }
            if p.position.is_none() && later.saturating_sub(p.time_ns) > PHRASE_GAP {
                break;
            }
            later = p.time_ns;
            out.push(p);
        }
        out.reverse();
        out
    }

    /// Everything kept so far counts as taken.
    pub(crate) fn mark_taken(&mut self) {
        if let Some(p) = self.played.back() {
            self.taken_until = self.taken_until.max(p.time_ns);
        }
    }
}

fn is_note_on(e: &MidiEvent) -> bool {
    matches!(e, MidiEvent::NoteOn { velocity, .. } if *velocity > 0)
}

impl Session {
    /// Keep what the live tracks were played (from the MIDI tick).
    pub(crate) fn feed_capture(&mut self, events: &[MidiInputEvent]) {
        let playing = self.transport.playing;
        let cap = &mut self.capture;
        if playing != cap.playing {
            cap.playing = playing;
            cap.run = cap.run.wrapping_add(1);
            cap.pass = 0;
            cap.last_position = None;
        }
        if events.is_empty() || self.midi.live().is_empty() {
            return;
        }
        let ports = self.midi.port_map();
        let rate = f64::from(self.project.sample_rate);
        let shift = i64::from(self.engine.output_latency()) + self.record.latency_offset;
        let looped = self
            .project
            .loop_range
            .filter(|_| self.project.loop_enabled)
            .map(|r| {
                (
                    self.project.timeline.to_samples(r.start, rate),
                    self.project.timeline.to_samples(r.end, rate),
                )
            })
            .filter(|(a, b)| b > a);
        let mut live: Vec<TrackId> = self.midi.live().iter().copied().collect();
        live.sort_by_key(|t| self.project.track_index(*t));
        for ev in events {
            let Some(event) = ev.event() else {
                continue;
            };
            if !matches!(
                event,
                MidiEvent::NoteOn { .. }
                    | MidiEvent::NoteOff { .. }
                    | MidiEvent::ControlChange { .. }
                    | MidiEvent::PitchBend { .. }
                    | MidiEvent::ChannelPressure { .. }
                    | MidiEvent::ProgramChange { .. }
                    | MidiEvent::PolyPressure { .. }
            ) {
                continue;
            }
            let tracks: Vec<TrackId> = live
                .iter()
                .copied()
                .filter(|t| {
                    self.project.track(*t).is_some_and(|t| match &t.input {
                        InputRouting::Midi { port, channel } => {
                            port.as_ref().is_none_or(|k| ports.get(k) == Some(&ev.port))
                                && channel.is_none_or(|c| c == event.channel())
                        }
                        _ => false,
                    })
                })
                .collect();
            if tracks.is_empty() {
                continue;
            }
            let position = if playing {
                self.engine.position_at(ev.time_ns).map(|p| {
                    let p = p - shift;
                    match looped {
                        Some((a, b)) if p >= b => a + (p - a) % (b - a),
                        _ => p,
                    }
                })
            } else {
                None
            };
            let cap = &mut self.capture;
            // A new pass when the loop wrapped (back by half the loop or
            // more); a smaller step back is the clock's (a dropout: the
            // engine's playhead lost time), kept in order instead.
            let position = position.map(|p| match cap.last_position {
                Some(last) if p < last => match looped {
                    Some((a, b)) if last - p >= (b - a) / 2 => {
                        cap.pass += 1;
                        p
                    }
                    _ => last,
                },
                _ => p,
            });
            if let Some(p) = position {
                cap.last_position = Some(p);
            }
            cap.played.push_back(Played {
                time_ns: ev.time_ns,
                event,
                position,
                run: cap.run,
                pass: cap.pass,
                tracks,
            });
        }
        let cap = &mut self.capture;
        if let Some(newest) = cap.played.back().map(|p| p.time_ns) {
            while cap
                .played
                .front()
                .is_some_and(|p| p.time_ns + KEEP_NS < newest)
            {
                cap.played.pop_front();
            }
        }
        while cap.played.len() > KEEP_EVENTS {
            cap.played.pop_front();
        }
    }

    /// Is there something played to capture (and nothing recording)?
    /// Cheap enough for every frame: it stops at the newest note.
    pub fn can_capture_midi(&self) -> bool {
        let cap = &self.capture;
        let Some(newest) = cap.played.back() else {
            return false;
        };
        let mut later = newest.time_ns;
        self.recording.is_none()
            && cap
                .played
                .iter()
                .rev()
                .take_while(|p| {
                    let inside = p.run == newest.run
                        && p.time_ns > cap.taken_until
                        && (p.position.is_some() || later.saturating_sub(p.time_ns) <= PHRASE_GAP);
                    later = p.time_ns;
                    inside
                })
                .any(|p| is_note_on(&p.event))
    }

    /// Turn what was played last into clips on the tracks that played it.
    pub(crate) fn capture_midi(&mut self) -> Result<()> {
        if self.recording.is_some() {
            return Err(SessionError::Other(
                "recording keeps what is played already".into(),
            ));
        }
        let phrase: Vec<Played> = self.capture.phrase().into_iter().cloned().collect();
        let Some(anchor) = phrase.iter().find(|p| is_note_on(&p.event)) else {
            self.notify(
                NoticeLevel::Info,
                "nothing played to capture: play a track live (select it) first",
            );
            return Ok(());
        };
        // Positions: as heard while playing; played while stopped, the
        // first note on the bar at or before the playhead.
        let stopped = phrase.iter().all(|p| p.position.is_none());
        let rate = f64::from(self.project.sample_rate);
        let positions: Vec<i64> = if stopped {
            let timeline = &self.project.timeline;
            let bar_start = timeline
                .meter
                .bar_start(timeline.meter.bar_at(self.playhead()));
            let mut base = timeline.to_samples(bar_start, rate);
            // The first sample that is on the bar (not a rounding before).
            while self.engine.samples_to_musical(&self.project, base) < bar_start {
                base += 1;
            }
            let t0 = anchor.time_ns as i128;
            phrase
                .iter()
                .map(|p| {
                    let dt = (p.time_ns as i128 - t0) as f64 / 1e9;
                    (base + (dt * rate).round() as i64).max(0)
                })
                .collect()
        } else {
            let mut last = 0;
            phrase
                .iter()
                .map(|p| {
                    last = p.position.unwrap_or(last);
                    last
                })
                .collect()
        };
        let mut tracks: Vec<TrackId> = phrase.iter().flat_map(|p| p.tracks.clone()).collect();
        tracks.sort_by_key(|t| self.project.track_index(*t));
        tracks.dedup();
        tracks.retain(|t| self.project.track(*t).is_some());
        if tracks.is_empty() {
            return Ok(());
        }
        let (mut tx, rx) = rtrb::RingBuffer::new(phrase.len() * tracks.len() + 1);
        for (p, &position) in phrase.iter().zip(&positions) {
            for t in &p.tracks {
                if let Some(target) = tracks.iter().position(|x| x == t) {
                    let _ = tx.push(RecordedMidi {
                        target: target as u16,
                        position,
                        pass: p.pass,
                        event: p.event,
                    });
                }
            }
        }
        let mut take = MidiTake::new(rx, tracks.clone(), 0);
        take.drain();
        // Keys still held end a moment after the last message.
        take.close_all(take.last + (rate * 0.25) as i64);
        let notes: usize = take.notes.iter().map(Vec::len).sum();
        let (commands, placed) = self.midi_take_commands(&take);
        if commands.is_empty() {
            self.notify(NoticeLevel::Info, "nothing played to capture");
            return Ok(());
        }
        self.edit(Command::Batch {
            label: "Capture MIDI".into(),
            commands,
        })?;
        self.selection.select_clips(&placed, SelectMode::Replace);
        self.capture.taken_until = phrase.last().map_or(0, |p| p.time_ns);
        let names: Vec<String> = tracks
            .iter()
            .filter_map(|t| self.project.track(*t).map(|t| format!("'{}'", t.name)))
            .collect();
        self.notify(
            NoticeLevel::Info,
            format!(
                "captured {notes} note{} on {}",
                if notes == 1 { "" } else { "s" },
                names.join(", ")
            ),
        );
        Ok(())
    }
}
