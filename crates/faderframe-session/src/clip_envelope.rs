//! Clip envelopes in the session ([`faderframe_project::clip_envelope`]):
//! which lane the arranger shows and edits on audio clips and on the takes
//! in take lanes, a lane's points replaced (drags inside a gesture
//! coalesce into one step), lanes cleared, and the parser of the
//! `clip-envelope:` dev action. A take's envelope is its own (a take is a
//! clip in its folder): it changes with the folder's content.

use crate::{Result, Session, SessionError};
use faderframe_core::{ClipId, TrackId};
use faderframe_project::clip_envelope::{ClipEnvelope, EnvelopeLane, EnvelopePoint};
use faderframe_project::{AudioClip, AudioRef, ClipContent, Command};

impl Session {
    /// The envelope lane the arranger draws and edits on audio clips
    /// (`None`: envelopes hidden, the clips edit as ever).
    pub fn clip_envelope_shown(&self) -> Option<EnvelopeLane> {
        self.editor.clip_envelope
    }

    /// The envelopes of an audio clip or take (`None`: none, or no audio).
    pub fn clip_envelope(&self, audio: AudioRef) -> Option<&ClipEnvelope> {
        self.project.audio(audio)?.envelope.as_deref()
    }

    /// Replace `lane` of `audio` (a clip or take) by `points` (its frames
    /// at the project rate; sorted and clamped here). Inside a gesture
    /// successive calls are one undo step.
    pub(crate) fn set_clip_envelope_lane(
        &mut self,
        audio: AudioRef,
        lane: EnvelopeLane,
        points: Vec<EnvelopePoint>,
    ) -> Result<()> {
        let Some(a) = self.project.audio(audio) else {
            return Err(SessionError::Other(match self.project.clip(audio.clip()) {
                Some(c) if c.as_takes().is_some() => format!(
                    "'{}' is a take folder: each of its takes has an envelope of its own \
                     (Show Takes)",
                    c.name
                ),
                Some(c) => format!(
                    "'{}' is no audio clip: only audio clips and takes have envelopes",
                    c.name
                ),
                None => format!("no clip {}", audio.clip()),
            }));
        };
        let envelope = ClipEnvelope::with_lane(a.envelope.as_deref(), lane, points);
        if envelope == a.envelope {
            return Ok(());
        }
        match audio {
            AudioRef::Clip(clip) => self.edit(Command::SetClipEnvelope { clip, envelope }),
            AudioRef::Take(t) => {
                let a = AudioClip {
                    envelope,
                    ..a.clone()
                };
                match self.project.set_take_audio(t, a) {
                    Some(cmd) => self.edit(cmd),
                    None => Ok(()),
                }
            }
        }
    }

    /// Clear `lane` (`None`: every lane) of the clips and takes among
    /// `audio` that have one, as one step.
    pub(crate) fn clear_clip_envelopes(
        &mut self,
        audio: &[AudioRef],
        lane: Option<EnvelopeLane>,
    ) -> Result<()> {
        let mut takes = Vec::new();
        let mut commands: Vec<Command> = Vec::new();
        for &r in audio {
            let Some(e) = self.clip_envelope(r) else {
                continue;
            };
            let envelope = match lane {
                None => None,
                Some(lane) if e.lane(lane).is_empty() => continue,
                Some(lane) => ClipEnvelope::with_lane(Some(e), lane, Vec::new()),
            };
            match r {
                AudioRef::Clip(clip) => commands.push(Command::SetClipEnvelope { clip, envelope }),
                AudioRef::Take(_) => {
                    if let Some(a) = self.project.audio(r) {
                        takes.push((
                            r,
                            AudioClip {
                                envelope,
                                ..a.clone()
                            },
                        ));
                    }
                }
            }
        }
        commands.extend(self.project.set_audio(takes));
        if commands.is_empty() {
            return Ok(());
        }
        let label = match lane {
            Some(EnvelopeLane::Pan) => "Clear Clip Pan",
            Some(EnvelopeLane::Volume) => "Clear Clip Volume",
            None => "Clear Clip Envelopes",
        };
        self.batch(label, commands)
    }

    /// The first audio clip of `track` in time (dev actions and tests).
    pub fn first_audio_clip(&self, track: TrackId) -> Option<ClipId> {
        self.project
            .clips_of(track)
            .into_iter()
            .filter(|c| matches!(c.content, ClipContent::Audio(_)))
            .min_by_key(|c| c.start)
            .map(|c| c.id)
    }
}

/// Points of `<t>/<value>[|<t>/<value>…]` (seconds from the clip start,
/// the lane's unit: dB or −1…1) as clip frames at `rate`.
pub fn parse_points(spec: &str, rate: u32) -> Option<Vec<EnvelopePoint>> {
    spec.split('|')
        .map(|p| {
            let (t, v) = p.trim().split_once('/')?;
            let t: f64 = t.trim().parse().ok()?;
            let v: f64 = match v.trim() {
                "-inf" | "−inf" => f64::NEG_INFINITY,
                v => v.trim_end_matches("dB").trim().parse().ok()?,
            };
            (t.is_finite() && t >= 0.0)
                .then(|| EnvelopePoint::new((t * f64::from(rate)).round() as i64, v))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn points_parse_in_seconds_and_db() {
        let p = parse_points("0/-6|0.5/0 | 1/-inf", 48_000).unwrap_or_default();
        assert_eq!(p.len(), 3);
        assert_eq!(p[1], EnvelopePoint::new(24_000, 0.0));
        assert_eq!(p[2].value, f64::NEG_INFINITY);
        assert!(parse_points("x/1", 48_000).is_none());
    }
}
