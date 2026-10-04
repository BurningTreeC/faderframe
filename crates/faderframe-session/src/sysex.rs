//! System exclusive (SysEx) messages.
//!
//! SysEx is for external devices (patch dumps, device set-up), so it never
//! enters the realtime path: variable-length data stays on the control
//! side.
//!
//! * Recording: SysEx arriving on a recording MIDI track's input (the
//!   system feed, timestamped on the MIDI clock) is placed at the position
//!   heard when it arrived and becomes part of the take.
//! * Playback: SysEx in the clips of tracks with an external MIDI output is
//!   scheduled ahead (100 ms) to the output sender with exact due times,
//!   derived from the engine's position at each callback. Loop wraps are
//!   followed; a locate or stop is noticed (the engine is not where it was
//!   predicted to be) and cancels what was scheduled.
//! * Editing: messages can be added (e.g. from a `.syx` file), removed and
//!   sent straight to an output.

use crate::{Result, Session, SessionError};
use faderframe_core::ClipId;
use faderframe_midi::{MidiSystemEvent, SystemMessage};
use faderframe_project::{InputRouting, SysexEvent};
use faderframe_timeline::MusicalTime;

/// How far ahead SysEx is handed to the output sender.
const LOOKAHEAD_NS: u64 = 100_000_000;

/// Playback scheduling state.
#[derive(Debug, Default)]
pub(crate) struct SysexPlayback {
    /// Real time (MIDI clock) up to which messages have been scheduled.
    until_ns: u64,
    /// (time, engine position) at the last tick, to notice jumps.
    last: Option<(u64, i64)>,
}

impl Session {
    /// Put incoming SysEx into the MIDI take of the tracks listening to its
    /// port.
    pub(crate) fn record_sysex(&mut self, events: &[MidiSystemEvent]) {
        let Some(r) = self.recording.as_ref() else {
            return;
        };
        let (Some(take), true) = (r.midi.as_ref(), self.transport.playing) else {
            return;
        };
        let (from, to) = (r.from, r.to);
        let latency = self.engine.output_latency() as i64;
        let mut placed = Vec::new();
        for ev in events {
            let SystemMessage::SysEx(data) = &ev.message else {
                continue;
            };
            // Where the player was in what they heard.
            let Some(pos) = self.engine.position_at(ev.time_ns).map(|p| p - latency) else {
                continue;
            };
            if pos < from || pos >= to {
                continue;
            }
            let key = self.midi.hub.port_key(ev.port);
            for (i, track) in take.tracks.iter().enumerate() {
                let listens = self.project.track(*track).is_some_and(|t| match &t.input {
                    InputRouting::Midi { port, .. } => {
                        port.as_deref().is_none_or(|p| Some(p) == key)
                    }
                    _ => false,
                });
                if listens {
                    placed.push((i, pos, data.clone()));
                }
            }
        }
        if let Some(take) = self.recording.as_mut().and_then(|r| r.midi.as_mut()) {
            for (i, pos, data) in placed {
                take.sysex[i].push((pos, data));
            }
        }
    }

    /// Schedule the clips' SysEx that plays within the next moments.
    pub(crate) fn tick_sysex(&mut self, now_ns: u64) {
        let pos_now = self.engine.position_at(now_ns);
        let rate = self.engine.stream_sample_rate().max(1) as f64;
        let (Some(pos_now), true) = (pos_now, self.transport.playing) else {
            if self.midi.sysex.last.take().is_some() {
                self.midi.outputs.cancel_sysex();
            }
            return;
        };
        let loop_range = self
            .project
            .loop_range
            .filter(|_| self.project.loop_enabled)
            .map(|r| {
                (
                    self.engine.musical_to_samples(&self.project, r.start),
                    self.engine.musical_to_samples(&self.project, r.end),
                )
            })
            .filter(|(a, b)| b > a);
        let wrap = |p: i64| match loop_range {
            Some((a, b)) if p >= b => a + (p - b) % (b - a),
            _ => p,
        };
        // Did playback jump since the last tick?
        let jumped = match self.midi.sysex.last {
            None => true,
            Some((t, p)) => {
                let predicted = wrap(p + ((now_ns - t) as f64 * rate / 1e9) as i64);
                (predicted - pos_now).abs() as f64 > rate * 0.02
            }
        };
        if jumped {
            if self.midi.sysex.last.is_some() {
                self.midi.outputs.cancel_sysex();
            }
            self.midi.sysex.until_ns = now_ns;
        }
        self.midi.sysex.last = Some((now_ns, pos_now));
        let from_ns = self.midi.sysex.until_ns.max(now_ns);
        let to_ns = now_ns + LOOKAHEAD_NS;
        if to_ns <= from_ns {
            return;
        }
        self.midi.sysex.until_ns = to_ns;
        // The window in playback order, unwrapped: [p0, p1).
        let to_pos = |t: u64| pos_now + ((t as f64 - now_ns as f64) * rate / 1e9) as i64;
        let (p0, p1) = (to_pos(from_ns), to_pos(to_ns));
        let latency_ns = self.engine.output_latency() as f64 / rate * 1e9;
        let due = |unwrapped: i64| {
            now_ns.saturating_add_signed(
                ((unwrapped - pos_now) as f64 / rate * 1e9 + latency_ns) as i64,
            )
        };
        // Segments of the timeline played in the window: (start, end,
        // offset from timeline to unwrapped positions).
        let mut segments = Vec::new();
        match loop_range {
            Some((a, b)) if pos_now < b && p1 > b => {
                segments.push((wrap(p0), b, p0 - wrap(p0)));
                segments.push((a, a + (p1 - b), b - a));
            }
            _ => segments.push((wrap(p0), wrap(p0) + (p1 - p0), p0 - wrap(p0))),
        }
        let mut sends = Vec::new();
        for t in &self.project.tracks {
            let Some(out) = &t.midi_output else { continue };
            let Some(port) = self.midi.outputs.port_index(&out.port) else {
                continue;
            };
            for clip in self.project.clips_of(t.id) {
                let Some(m) = clip.as_midi().filter(|_| !clip.muted) else {
                    continue;
                };
                for e in &m.sysex {
                    let at = clip.start + e.time;
                    if e.time >= m.length {
                        continue;
                    }
                    let pos = self.engine.musical_to_samples(&self.project, at);
                    for &(a, b, offset) in &segments {
                        if pos >= a && pos < b {
                            sends.push((port, due(pos + offset), e.data.clone()));
                        }
                    }
                }
            }
        }
        for (port, due, data) in sends {
            self.midi.outputs.send_sysex(port, due, data);
        }
    }

    /// Add SysEx messages to a MIDI clip at `at` (clip-relative).
    pub fn add_sysex(
        &mut self,
        clip: ClipId,
        at: MusicalTime,
        messages: Vec<Vec<u8>>,
    ) -> Result<()> {
        let c = self
            .project
            .clip(clip)
            .ok_or_else(|| SessionError::Other(format!("no clip {clip}")))?;
        let (start, mut m) = (
            c.start,
            c.as_midi()
                .ok_or_else(|| SessionError::Other("not a MIDI clip".into()))?
                .clone(),
        );
        let valid: Vec<Vec<u8>> = messages
            .into_iter()
            .filter(|d| d.len() >= 2 && d[0] == 0xF0 && d[d.len() - 1] == 0xF7)
            .collect();
        if valid.is_empty() {
            return Err(SessionError::Other("no complete SysEx message".into()));
        }
        for data in valid {
            m.sysex.push(SysexEvent {
                time: at.max(MusicalTime::ZERO),
                data,
            });
        }
        m.sysex.sort_by_key(|e| e.time);
        self.edit(faderframe_project::Command::Batch {
            label: "Add SysEx".into(),
            commands: vec![faderframe_project::Command::SetClipContent {
                clip,
                start,
                content: Box::new(faderframe_project::ClipContent::Midi(m)),
            }],
        })
    }

    /// Remove the `index`th SysEx message of a clip.
    pub fn remove_sysex(&mut self, clip: ClipId, index: usize) -> Result<()> {
        let c = self
            .project
            .clip(clip)
            .ok_or_else(|| SessionError::Other(format!("no clip {clip}")))?;
        let (start, mut m) = (
            c.start,
            c.as_midi()
                .ok_or_else(|| SessionError::Other("not a MIDI clip".into()))?
                .clone(),
        );
        if index >= m.sysex.len() {
            return Ok(());
        }
        m.sysex.remove(index);
        self.edit(faderframe_project::Command::Batch {
            label: "Delete SysEx".into(),
            commands: vec![faderframe_project::Command::SetClipContent {
                clip,
                start,
                content: Box::new(faderframe_project::ClipContent::Midi(m)),
            }],
        })
    }

    /// Send SysEx to an output port now (e.g. a `.syx` file to a synth).
    /// SysEx messages handed to the output sender so far (diagnostics).
    pub fn sysex_scheduled(&self) -> u64 {
        self.midi.outputs.sysex_scheduled()
    }

    pub fn send_sysex(&mut self, output: &str, messages: Vec<Vec<u8>>) -> Result<()> {
        let port = self
            .midi
            .outputs
            .port_index(output)
            .ok_or_else(|| SessionError::Other(format!("no MIDI output {output}")))?;
        let now = self.midi.outputs.clock().now_ns();
        let n = messages.len();
        for data in messages {
            self.midi.outputs.send_sysex(port, now, data);
        }
        self.notify(
            crate::NoticeLevel::Info,
            format!("sent {n} SysEx message{}", if n == 1 { "" } else { "s" }),
        );
        Ok(())
    }
}
