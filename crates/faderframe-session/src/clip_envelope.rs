//! Clip envelopes in the session ([`faderframe_project::clip_envelope`]):
//! which lane the arranger shows and edits on audio clips, a lane's
//! points replaced (drags inside a gesture coalesce into one step), lanes
//! cleared, and the parser of the `clip-envelope:` dev action.

use crate::{Result, Session, SessionError};
use faderframe_core::{ClipId, TrackId};
use faderframe_project::clip_envelope::{ClipEnvelope, EnvelopeLane, EnvelopePoint};
use faderframe_project::{ClipContent, Command};

impl Session {
    /// The envelope lane the arranger draws and edits on audio clips
    /// (`None`: envelopes hidden, the clips edit as ever).
    pub fn clip_envelope_shown(&self) -> Option<EnvelopeLane> {
        self.editor.clip_envelope
    }

    /// An audio clip's envelopes (`None`: none, or no audio clip).
    pub fn clip_envelope(&self, clip: ClipId) -> Option<&ClipEnvelope> {
        match &self.project.clip(clip)?.content {
            ClipContent::Audio(a) => a.envelope.as_deref(),
            _ => None,
        }
    }

    /// Replace `lane` of `clip` by `points` (clip frames at the project
    /// rate; sorted and clamped here). Inside a gesture successive calls
    /// are one undo step.
    pub(crate) fn set_clip_envelope_lane(
        &mut self,
        clip: ClipId,
        lane: EnvelopeLane,
        points: Vec<EnvelopePoint>,
    ) -> Result<()> {
        let Some(c) = self.project.clip(clip) else {
            return Err(SessionError::Other(format!("no clip {clip}")));
        };
        let ClipContent::Audio(a) = &c.content else {
            return Err(SessionError::Other(format!(
                "'{}' is no audio clip: only audio clips have envelopes",
                c.name
            )));
        };
        let envelope = ClipEnvelope::with_lane(a.envelope.as_deref(), lane, points);
        if envelope == a.envelope {
            return Ok(());
        }
        self.edit(Command::SetClipEnvelope { clip, envelope })
    }

    /// Clear `lane` (`None`: every lane) of the audio clips among `clips`
    /// that have one, as one step.
    pub(crate) fn clear_clip_envelopes(
        &mut self,
        clips: &[ClipId],
        lane: Option<EnvelopeLane>,
    ) -> Result<()> {
        let commands: Vec<Command> = clips
            .iter()
            .filter_map(|&clip| {
                let e = self.clip_envelope(clip)?;
                let envelope = match lane {
                    None => None,
                    Some(lane) if e.lane(lane).is_empty() => return None,
                    Some(lane) => ClipEnvelope::with_lane(Some(e), lane, Vec::new()),
                };
                Some(Command::SetClipEnvelope { clip, envelope })
            })
            .collect();
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
