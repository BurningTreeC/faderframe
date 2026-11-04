//! MusicXML scores into the project ([`faderframe_interchange::musicxml`]
//! reads them): a part becomes an instrument track with one clip of its
//! notes (its MIDI program at the start, percussion on channel 10), the
//! score's tempo and metre the project's (imported at the start), its
//! keys the key lane, its chord symbols the chord track and the words of
//! the first part that has them the lyrics — all one undo step. A
//! tablature part under a part with the same notes (a lead sheet's Notes
//! + Tab) is left out: it would play twice.

use crate::{NoticeLevel, Result, Session, SessionError};
use faderframe_core::TrackId;
use faderframe_interchange::musicxml::{self, Part, Score};
use faderframe_project::harmony::{ChordEvent, KeyChange, normalize_chords, normalize_keys};
use faderframe_project::lyrics::LyricLine;
use faderframe_project::{
    Clip, ClipContent, Command, ControllerLane, ControllerPoint, MidiClip, MidiController,
    MidiNote, Track, TrackColor, TrackKind,
};
use faderframe_timeline::{MusicalTime, TimeSignature};
use std::path::Path;

/// Is `path` a MusicXML score (by its extension)?
pub fn is_musicxml_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "musicxml" | "mxl" | "xml"))
}

/// A part's notes, rounded to the grid MusicXML's durations make
/// (quarters) for comparison.
fn note_set(p: &Part) -> Vec<(i64, i64, u8)> {
    let mut v: Vec<(i64, i64, u8)> = p
        .notes
        .iter()
        .map(|n| {
            (
                (n.start * 1e4).round() as i64,
                ((n.start + n.length) * 1e4).round() as i64,
                n.key,
            )
        })
        .collect();
    v.sort_unstable();
    v
}

impl Session {
    /// Import a MusicXML score (`.musicxml`, `.xml` or compressed `.mxl`)
    /// at `at`; with `tempo` (and at the start) the score's tempo and
    /// metre become the project's. Returns the new tracks.
    pub fn import_musicxml(
        &mut self,
        path: &Path,
        at: MusicalTime,
        tempo: bool,
    ) -> Result<Vec<TrackId>> {
        let score = musicxml::read_file(path)
            .map_err(|e| SessionError::Other(format!("MusicXML: {}: {e}", path.display())))?;
        self.import_score(&score, path, at, tempo)
    }

    pub(crate) fn import_score(
        &mut self,
        score: &Score,
        path: &Path,
        at: MusicalTime,
        tempo: bool,
    ) -> Result<Vec<TrackId>> {
        let mut notes: Vec<String> = score.notes.clone();
        // Tablature repeating the part above it plays once.
        let mut parts: Vec<&Part> = Vec::new();
        for (i, p) in score.parts.iter().enumerate() {
            let repeats = p.tab
                && i > 0
                && !p.notes.is_empty()
                && note_set(&score.parts[i - 1]) == note_set(p);
            if repeats {
                notes.push(format!(
                    "the tablature of {} (it repeats the part above)",
                    score.parts[i - 1].name
                ));
            } else {
                parts.push(p);
            }
        }
        if parts.iter().all(|p| p.notes.is_empty()) && score.chord_spans().is_empty() {
            return Err(SessionError::Other(format!(
                "MusicXML: {} holds no notes or chords",
                path.display()
            )));
        }
        let q = |x: f64| at + MusicalTime::from_quarters(x.max(0.0));
        let mut cmds = Vec::new();
        let at_start = at == MusicalTime::ZERO;
        if tempo && at_start {
            // A tempo marked at the first note (a pickup's) is the
            // piece's from its first bar line.
            let first = score.measures.first().map_or(0.0, |m| m.start);
            let mut tempos: Vec<(MusicalTime, f64)> = score
                .tempos
                .iter()
                .map(|t| (if t.at <= first + 1e-9 { at } else { q(t.at) }, t.bpm))
                .collect();
            if tempos.is_empty() {
                // No tempo marked: the project's stays.
                tempos.push((
                    MusicalTime::ZERO,
                    self.project.timeline.tempo.bpm_at(MusicalTime::ZERO),
                ));
            }
            let meters: Vec<(MusicalTime, TimeSignature)> = score
                .meter_changes()
                .into_iter()
                .filter_map(|(bar, (n, d))| {
                    let m = score.measures.get(bar)?;
                    let start = if bar == 0 { 0.0 } else { m.start };
                    Some((q(start), TimeSignature::new(n, d)?))
                })
                .collect();
            cmds.push(self.imported_timeline(tempos, meters));
        } else if !score.tempos.is_empty() || score.meter_changes().len() > 1 {
            notes.push("the score's tempo and metre (imported only at the start of a project without clips)".into());
        }
        // The score's length, to the bar line after it.
        let length = MusicalTime::from_quarters(score.length().max(1.0));
        let p = &mut self.project;
        let mut index = p
            .tracks
            .iter()
            .rposition(|t| t.kind.has_clips())
            .map_or(0, |i| i + 1);
        let mut created = Vec::new();
        let base = path
            .file_stem()
            .map_or_else(|| "Score".to_string(), |s| s.to_string_lossy().to_string());
        for (k, part) in parts.iter().enumerate() {
            if part.notes.is_empty() {
                continue;
            }
            let name = if part.name.trim().is_empty() {
                format!("{base} {}", k + 1)
            } else {
                part.name.trim().to_string()
            };
            let channel = if part.percussion {
                9
            } else {
                part.channel.unwrap_or(0).min(15)
            };
            let mut clip = MidiClip {
                length,
                ..MidiClip::default()
            };
            for n in &part.notes {
                let start = MusicalTime::from_quarters(n.start.max(0.0));
                let end = MusicalTime::from_quarters((n.start + n.length).max(0.0));
                clip.notes.push(MidiNote {
                    id: p.ids.allocate(),
                    start,
                    length: (end - start).max(MusicalTime(1)),
                    key: n.key,
                    velocity: n.velocity.max(1),
                    channel: n.channel.unwrap_or(channel).min(15),
                    muted: false,
                    release: None,
                    time_locked: false,
                    articulation: None,
                    fine: None,
                });
            }
            clip.sort_notes();
            if let Some(program) = part.program.filter(|_| !part.percussion) {
                let mut lane = ControllerLane::new(MidiController::Program, channel);
                lane.points
                    .push(ControllerPoint::new(MusicalTime::ZERO, u16::from(program)));
                clip.controllers.push(lane);
            }
            let id: TrackId = p.ids.allocate();
            let track = Track::new(
                id,
                TrackKind::Instrument,
                name.clone(),
                TrackColor::palette(p.tracks.len() + created.len()),
            );
            cmds.push(Command::AddTrack {
                track: Box::new(track),
                index,
            });
            index += 1;
            cmds.push(Command::AddClip {
                clip: Box::new(Clip {
                    id: p.ids.allocate(),
                    track: id,
                    name,
                    color: None,
                    start: at,
                    muted: false,
                    content: ClipContent::Midi(clip),
                }),
            });
            created.push(id);
        }
        // Keys, chords and words over the score's span.
        let end = at + length;
        if !score.keys.is_empty() {
            let mut keys: Vec<KeyChange> = p
                .keys
                .iter()
                .copied()
                .filter(|k| k.at < at || k.at >= end)
                .collect();
            let first = score.measures.first().map_or(0.0, |m| m.start);
            keys.extend(score.keys.iter().map(|k| KeyChange {
                at: if k.at <= first + 1e-9 { at } else { q(k.at) },
                key: k.key,
            }));
            normalize_keys(&mut keys);
            cmds.push(Command::SetKeys { keys });
        }
        let spans = score.chord_spans();
        if !spans.is_empty() {
            let mut chords: Vec<ChordEvent> = p
                .chords
                .iter()
                .copied()
                .filter(|c| c.end <= at || c.start >= end)
                .collect();
            chords.extend(spans.iter().map(|&(a, b, chord)| ChordEvent {
                start: q(a),
                end: q(b),
                chord,
            }));
            normalize_chords(&mut chords);
            cmds.push(Command::SetChords { chords });
        }
        let sung: Vec<(usize, &Part)> = parts
            .iter()
            .enumerate()
            .filter(|(_, p)| !p.lyrics.is_empty())
            .map(|(i, p)| (i, *p))
            .collect();
        if let Some((_, part)) = sung.first() {
            let lines: Vec<LyricLine> = part
                .lyric_lines(&score.measures)
                .into_iter()
                .map(|l| LyricLine {
                    start: q(l.start),
                    end: q(l.end),
                    text: l.text,
                })
                .collect();
            let lyrics = faderframe_project::lyrics::replace_span(&p.lyrics, at, end, lines);
            cmds.push(Command::SetLyrics { lyrics });
            if sung.len() > 1 {
                notes.push(format!(
                    "the words of {} more part(s) (the lyrics lane holds one part's)",
                    sung.len() - 1
                ));
            }
        }
        self.batch("Import MusicXML", cmds)?;
        self.selection
            .select_tracks(&created, crate::SelectMode::Replace);
        let n = created.len();
        let mut text = format!(
            "imported {n} part{} from {} — choose an instrument for {}",
            if n == 1 { "" } else { "s" },
            path.file_name()
                .map_or_else(String::new, |f| f.to_string_lossy().to_string()),
            if n == 1 { "it" } else { "each" }
        );
        if !notes.is_empty() {
            text.push_str("; not carried: ");
            text.push_str(&notes.join("; "));
        }
        self.notify(
            if notes.is_empty() {
                NoticeLevel::Info
            } else {
                NoticeLevel::Warning
            },
            text,
        );
        Ok(created)
    }
}
