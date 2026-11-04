//! Show mode's gear changes (see [`faderframe_project::setlist::SongGear`]).
//!
//! * When the show stands on a song (cued: on its start, ready to play)
//!   its changes go to their outputs at once, each message after the one
//!   before it has gone down a MIDI cable and after its change's wait —
//!   so a synth has loaded its patch by the time the song is played.
//!   Standing on another song cancels what has not gone out yet; standing
//!   on the same one again (stopped back on its start) sends nothing new
//!   until the setlist is edited or the show entered again (Send Now
//!   sends at any time).
//! * A song timed for its first frame ([`GearTiming::Start`]), and a song
//!   played into straight on from the one before (no moment to stand on
//!   it), is scheduled ahead like clip SysEx (`session::sysex`): due when
//!   its first frame is heard (the engine's position at the callback plus
//!   the output latency). A locate or stop cancels what is scheduled; a
//!   song played from a stop gets its changes at once if its start has
//!   just passed.
//!
//! Gear changes go out on schedules of their own (`Schedule::ShowCue`,
//! `Schedule::ShowStart`), so clip SysEx cancelled on a jump never takes
//! them along, and the other way round.

use crate::{NoticeLevel, Session, SessionError};
use faderframe_midi_io::Schedule;
use faderframe_project::setlist::{AfterSong, GearChange, GearMessage, GearTiming, SongGear};
use faderframe_timeline::MusicalTime;

/// How far ahead first-frame changes are handed to the output sender.
const LOOKAHEAD_NS: f64 = 100_000_000.0;
/// A song whose start passed this little ago (the first tick after a
/// start from a stop) still gets its first-frame changes, at once.
const LATE_NS: f64 = 500_000_000.0;

/// First-frame scheduling (part of the show's state).
#[derive(Debug, Default)]
pub(crate) struct GearPlayback {
    /// The song whose changes went out when the show stood on it last.
    pub(crate) cued: Option<usize>,
    /// Songs scheduled for their first frame since the last jump.
    timed: Vec<usize>,
    /// The transport's jump count when they were.
    jumps: Option<u32>,
}

impl Session {
    /// Hand song `i`'s changes to the output sender from `due_ns` on (on
    /// the MIDI clock); how many messages went. Outputs that are not
    /// there are named in a notice.
    pub(crate) fn send_song_gear(&mut self, i: usize, schedule: Schedule, due_ns: u64) -> usize {
        let Some(song) = self.project.setlist.songs.get(i) else {
            return 0;
        };
        let mut missing: Vec<String> = Vec::new();
        let mut sends = Vec::new();
        for (output, after, bytes) in song.gear.schedule() {
            match self.midi.outputs.port_index(output) {
                Some(port) => sends.push((port, due_ns + after, bytes)),
                None if !missing.iter().any(|m| m == output) => missing.push(output.into()),
                None => {}
            }
        }
        let name = song.name.clone();
        let n = sends.len();
        for (port, due, bytes) in sends {
            self.midi.outputs.send_timed(schedule, port, due, bytes);
        }
        if !missing.is_empty() {
            self.notify(
                NoticeLevel::Warning,
                format!(
                    "show: '{name}' changes gear on {} — no such MIDI output",
                    missing.join(", ")
                ),
            );
        }
        n
    }

    /// The show stands on song `i`: its changes now (timed for the cue),
    /// what the song before had not sent yet cancelled.
    pub(crate) fn song_gear(&mut self, i: usize) {
        if self.show.gear.cued == Some(i) {
            return;
        }
        self.midi.outputs.cancel(Schedule::ShowCue);
        self.show.gear.cued = Some(i);
        let Some(song) = self.project.setlist.songs.get(i) else {
            return;
        };
        if song.gear.timing != GearTiming::Cue || song.gear.changes.is_empty() {
            return;
        }
        let now = self.midi.outputs.clock().now_ns();
        self.send_song_gear(i, Schedule::ShowCue, now);
    }

    /// Send song `i`'s changes now, whatever their timing (the setlist's
    /// Send Now).
    pub(crate) fn send_gear_now(&mut self, i: usize) -> crate::Result<()> {
        let Some(song) = self.project.setlist.songs.get(i) else {
            return Ok(());
        };
        if song.gear.changes.is_empty() {
            return Err(SessionError::Other(format!(
                "'{}' changes no gear",
                song.name
            )));
        }
        self.midi.outputs.cancel(Schedule::ShowCue);
        let now = self.midi.outputs.clock().now_ns();
        let n = self.send_song_gear(i, Schedule::ShowCue, now);
        self.notify(
            NoticeLevel::Info,
            format!("sent {n} message{}", if n == 1 { "" } else { "s" }),
        );
        Ok(())
    }

    /// Does song `j` get its changes on its first frame (its timing says
    /// so, or the song before plays straight on into it)?
    fn gear_on_first_frame(&self, j: usize) -> bool {
        let songs = &self.project.setlist.songs;
        let Some(song) = songs.get(j) else {
            return false;
        };
        if song.gear.changes.is_empty() {
            return false;
        }
        if song.gear.timing == GearTiming::Start {
            return true;
        }
        let Some(before) = j.checked_sub(1).and_then(|k| songs.get(k)) else {
            return false;
        };
        let rate = f64::from(self.project.sample_rate.max(1));
        let tl = &self.project.timeline;
        matches!(before.then, AfterSong::Continue)
            && (tl.to_samples(song.start, rate) - tl.to_samples(before.end, rate)).abs()
                <= (rate * 0.05) as i64
    }

    /// Schedule first-frame changes that are due within the next moments
    /// (from the session's MIDI tick).
    pub(crate) fn tick_show_gear(&mut self, now_ns: u64) {
        let playing = self.show.on && self.transport.playing;
        let at = if playing {
            self.engine.position_and_jumps_at(now_ns)
        } else {
            None
        };
        let Some((pos_now, jumps)) = at else {
            if self.show.gear.jumps.take().is_some() {
                self.midi.outputs.cancel(Schedule::ShowStart);
                self.show.gear.timed.clear();
            }
            return;
        };
        if self.show.gear.jumps != Some(jumps) {
            if self.show.gear.jumps.is_some() {
                self.midi.outputs.cancel(Schedule::ShowStart);
            }
            self.show.gear.timed.clear();
            self.show.gear.jumps = Some(jumps);
        }
        let rate = f64::from(self.engine.stream_sample_rate().max(1));
        let latency_ns = f64::from(self.engine.output_latency()) / rate * 1e9;
        for j in 0..self.project.setlist.songs.len() {
            if self.show.gear.timed.contains(&j) || !self.gear_on_first_frame(j) {
                continue;
            }
            let start = self.project.setlist.songs[j].start;
            let ahead_ns = (self.engine.musical_to_samples(&self.project, start) - pos_now) as f64
                / rate
                * 1e9;
            if ahead_ns > LOOKAHEAD_NS || ahead_ns < -LATE_NS {
                continue;
            }
            let due = now_ns + (ahead_ns + latency_ns).max(0.0) as u64;
            self.show.gear.timed.push(j);
            self.send_song_gear(j, Schedule::ShowStart, due);
        }
    }

    /// Gear messages handed to the output sender so far: (when the show
    /// stood on a song, on songs' first frames). Diagnostics and tests.
    pub fn gear_scheduled(&self) -> (u64, u64) {
        (
            self.midi.outputs.scheduled(Schedule::ShowCue),
            self.midi.outputs.scheduled(Schedule::ShowStart),
        )
    }

    /// The output gear changes of song `i` go to unless they name one:
    /// the song's last change's, any song's, else the first output that is
    /// on (and no control surface's).
    pub fn default_gear_output(&self, i: usize) -> Option<String> {
        let songs = &self.project.setlist.songs;
        songs
            .get(i)
            .and_then(|s| s.gear.changes.last())
            .or_else(|| songs.iter().find_map(|s| s.gear.changes.last()))
            .map(|c| c.output.clone())
            .or_else(|| {
                self.midi_outputs()
                    .into_iter()
                    .find(|o| o.enabled && !self.is_surface_port(&o.key))
                    .map(|o| o.key)
            })
    }

    /// Song `i`'s gear from text (see `SongGear::parse`).
    pub(crate) fn gear_from_text(&self, i: usize, text: &str) -> crate::Result<SongGear> {
        let Some(song) = self.project.setlist.songs.get(i) else {
            return Err(SessionError::Other(format!("no song {}", i + 1)));
        };
        let output = self.default_gear_output(i).unwrap_or_default();
        song.gear.parse(text, &output).map_err(SessionError::Other)
    }

    /// Song `i`'s gear with its program change (on the default output,
    /// channel 1) set to `program` (0-based) and `bank`.
    pub(crate) fn gear_with_program(
        &self,
        i: usize,
        program: u8,
        bank: Option<u16>,
    ) -> crate::Result<SongGear> {
        let Some(song) = self.project.setlist.songs.get(i) else {
            return Err(SessionError::Other(format!("no song {}", i + 1)));
        };
        let output = self.default_gear_output(i).ok_or_else(|| {
            SessionError::Other("there is no MIDI output to change gear on".into())
        })?;
        let mut gear = song.gear.clone();
        let message = GearMessage::Program {
            channel: 0,
            program: program.min(127),
            bank,
        };
        let same = gear.changes.iter_mut().find(|c| {
            c.output == output && matches!(c.message, GearMessage::Program { channel: 0, .. })
        });
        match same {
            Some(c) => c.message = message,
            None => gear.changes.push(GearChange {
                output,
                wait_ms: 0,
                message,
            }),
        }
        Ok(gear)
    }

    /// Song `i`'s gear with the SysEx messages of a `.syx` file added
    /// (named by the file).
    pub(crate) fn gear_with_sysex_file(
        &self,
        i: usize,
        path: &std::path::Path,
    ) -> crate::Result<SongGear> {
        let Some(song) = self.project.setlist.songs.get(i) else {
            return Err(SessionError::Other(format!("no song {}", i + 1)));
        };
        let bytes = std::fs::read(path)
            .map_err(|e| SessionError::Other(format!("{}: {e}", path.display())))?;
        let messages = faderframe_project::SysexEvent::split_messages(&bytes);
        if messages.is_empty() {
            return Err(SessionError::Other(format!(
                "{} holds no complete SysEx message",
                path.display()
            )));
        }
        let output = self.default_gear_output(i).ok_or_else(|| {
            SessionError::Other("there is no MIDI output to change gear on".into())
        })?;
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut gear = song.gear.clone();
        gear.changes
            .extend(messages.into_iter().map(|data| GearChange {
                output: output.clone(),
                wait_ms: 0,
                message: GearMessage::SysEx {
                    name: name.clone(),
                    data,
                },
            }));
        Ok(gear)
    }

    /// Song `i`'s gear with a controller snapshot of `track` added: the
    /// value each controller of its MIDI clips has at the song's start
    /// (sent to the track's MIDI output, else the default one).
    pub(crate) fn gear_with_snapshot(
        &self,
        i: usize,
        track: faderframe_core::TrackId,
    ) -> crate::Result<SongGear> {
        let Some(song) = self.project.setlist.songs.get(i) else {
            return Err(SessionError::Other(format!("no song {}", i + 1)));
        };
        let t = self
            .project
            .track(track)
            .ok_or_else(|| SessionError::Other(format!("no track {track}")))?;
        let output = t
            .midi_output
            .as_ref()
            .map(|o| o.port.clone())
            .or_else(|| self.default_gear_output(i))
            .ok_or_else(|| {
                SessionError::Other("there is no MIDI output to change gear on".into())
            })?;
        let values = controller_values_at(&self.project, track, song.start);
        if values.is_empty() {
            return Err(SessionError::Other(format!(
                "'{}' sets no controllers by the song's start",
                t.name
            )));
        }
        let mut gear = song.gear.clone();
        for ((channel, controller), value) in values {
            gear.changes.push(GearChange {
                output: output.clone(),
                wait_ms: 0,
                message: GearMessage::Control {
                    channel,
                    controller,
                    value,
                },
            });
        }
        Ok(gear)
    }
}

/// The value of each control change (channel, number) the track's clips
/// set at or before `at` (the latest clip starting before it wins).
fn controller_values_at(
    p: &faderframe_project::Project,
    track: faderframe_core::TrackId,
    at: MusicalTime,
) -> std::collections::BTreeMap<(u8, u8), u8> {
    let mut clips: Vec<&faderframe_project::Clip> = p
        .clips_of(track)
        .into_iter()
        .filter(|c| c.start <= at && !c.muted)
        .collect();
    clips.sort_by_key(|c| c.start);
    let mut out = std::collections::BTreeMap::new();
    for c in clips {
        let Some(m) = c.as_midi() else { continue };
        let rel = (at - c.start).min(m.length);
        for lane in &m.controllers {
            let faderframe_project::MidiController::Cc { number } = lane.controller else {
                continue;
            };
            if let Some(v) = lane.value_at(rel) {
                out.insert((lane.channel & 15, number & 127), v.min(127) as u8);
            }
        }
    }
    out
}

/// The `show-gear:<song n>=<program>[/<bank>]` development aid's argument:
/// (song index, program 0-based, bank). Songs and programs count from 1
/// as written.
pub fn parse_show_gear(arg: &str) -> Option<(usize, u8, Option<u16>)> {
    let (song, rest) = arg.split_once('=')?;
    let song = song.trim().parse::<usize>().ok()?.checked_sub(1)?;
    let (program, bank) = match rest.split_once('/') {
        Some((p, b)) => (
            p,
            Some(b.trim().parse::<u16>().ok().filter(|b| *b < 0x4000)?),
        ),
        None => (rest, None),
    };
    let program = program
        .trim()
        .parse::<u8>()
        .ok()
        .filter(|p| (1..=128).contains(p))?;
    Some((song, program - 1, bank))
}

#[cfg(test)]
mod tests {
    use super::parse_show_gear;

    #[test]
    fn the_dev_actions_argument_reads() {
        assert_eq!(parse_show_gear("2=5"), Some((1, 4, None)));
        assert_eq!(parse_show_gear("1=128/300"), Some((0, 127, Some(300))));
        for bad in ["0=5", "1=0", "1=129", "x=1", "1", "1=5/99999"] {
            assert_eq!(parse_show_gear(bad), None, "{bad}");
        }
    }
}
