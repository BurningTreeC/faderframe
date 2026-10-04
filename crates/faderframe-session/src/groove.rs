//! Quantize and Humanize for whole clips: audio clips through their
//! transients (warp markers pin each detected hit where it should play),
//! MIDI clips through their notes. One undo step for every clip; take
//! folders and clips without material are left alone.

use crate::notes::NoteOp;
use crate::{Result, Session};
use faderframe_core::ClipId;
use faderframe_project::midi_ops::Rng;
use faderframe_project::{ClipContent, Warp, WarpMarker};
use faderframe_timeline::MusicalTime;

/// How far Humanize moves things.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HumanizeSettings {
    /// Largest timing offset either way (milliseconds).
    pub timing_ms: f32,
    /// Largest velocity change either way (MIDI notes).
    pub velocity: u8,
}

impl Default for HumanizeSettings {
    fn default() -> Self {
        Self {
            timing_ms: 10.0,
            velocity: 8,
        }
    }
}

/// Choices the menus offer.
pub const HUMANIZE_TIMINGS: [f32; 5] = [0.0, 5.0, 10.0, 20.0, 40.0];
pub const HUMANIZE_VELOCITIES: [u8; 5] = [0, 4, 8, 16, 24];
pub const QUANTIZE_STRENGTHS: [f32; 4] = [1.0, 0.75, 0.5, 0.25];
pub const QUANTIZE_SWINGS: [f32; 4] = [0.0, 0.25, 0.5, 0.66];

/// A menu entry of the Quantize / Humanize settings (views turn these
/// into their menu items).
#[derive(Clone, Debug, PartialEq)]
pub struct SettingsEntry {
    pub label: String,
    pub action: crate::Action,
    pub checked: bool,
    /// A separator goes above it.
    pub separated: bool,
}

enum Kind {
    Audio,
    Midi(MusicalTime),
    Other,
}

impl Session {
    /// Quantize strength, swing and note ends; Humanize timing and
    /// velocity.
    pub fn groove_settings_menu(&self) -> Vec<SettingsEntry> {
        use crate::Action;
        use faderframe_project::midi_ops::QuantizeSettings;
        let q = self.editor.quantize;
        let h = self.editor.humanize;
        let entry = |label: String, action: Action, checked: bool, separated: bool| SettingsEntry {
            label,
            action,
            checked,
            separated,
        };
        let mut items = Vec::new();
        for s in QUANTIZE_STRENGTHS {
            items.push(entry(
                format!("Quantize strength {:.0} %", s * 100.0),
                Action::SetQuantize(QuantizeSettings { strength: s, ..q }),
                (q.strength - s).abs() < 1e-3,
                false,
            ));
        }
        for (i, s) in QUANTIZE_SWINGS.into_iter().enumerate() {
            items.push(entry(
                format!("Swing {:.0} %", s * 100.0),
                Action::SetQuantize(QuantizeSettings { swing: s, ..q }),
                (q.swing - s).abs() < 1e-3,
                i == 0,
            ));
        }
        items.push(entry(
            "Quantize note ends too".into(),
            Action::SetQuantize(QuantizeSettings { ends: !q.ends, ..q }),
            q.ends,
            true,
        ));
        for (i, t) in HUMANIZE_TIMINGS.into_iter().enumerate() {
            items.push(entry(
                if t == 0.0 {
                    "Humanize timing off".into()
                } else {
                    format!("Humanize timing ±{t:.0} ms")
                },
                Action::SetHumanize(HumanizeSettings { timing_ms: t, ..h }),
                (h.timing_ms - t).abs() < 1e-3,
                i == 0,
            ));
        }
        for (i, v) in HUMANIZE_VELOCITIES.into_iter().enumerate() {
            items.push(entry(
                if v == 0 {
                    "Humanize velocity off".into()
                } else {
                    format!("Humanize velocity ±{v}")
                },
                Action::SetHumanize(HumanizeSettings { velocity: v, ..h }),
                h.velocity == v,
                i == 0,
            ));
        }
        items
    }

    fn clip_kind(&self, clip: ClipId) -> Kind {
        match self.project.clips.get(&clip) {
            Some(c) => match &c.content {
                ClipContent::Audio(_) => Kind::Audio,
                ClipContent::Midi(_) => Kind::Midi(c.start),
                _ => Kind::Other,
            },
            None => Kind::Other,
        }
    }

    /// The humanize operation for notes of a clip at `at` (the timing in
    /// milliseconds turned into musical time at the tempo there).
    pub fn humanize_op(&self, at: MusicalTime) -> NoteOp {
        let h = self.editor.humanize;
        let tempo = &self.project.timeline.tempo;
        let secs = tempo.musical_to_seconds(at);
        let later = tempo.seconds_to_musical(secs + h.timing_ms.max(0.0) as f64 / 1000.0);
        NoteOp::Humanize {
            timing: (later - at).max(MusicalTime::ZERO),
            velocity: h.velocity,
        }
    }

    /// Run `edit` for every clip inside one gesture (one undo step); a
    /// failing clip does not stop the others, the first error is returned.
    fn each_clip(
        &mut self,
        label: &str,
        clips: &[ClipId],
        mut edit: impl FnMut(&mut Session, ClipId) -> Result<()>,
    ) -> Result<()> {
        let own = !self.history.in_gesture();
        if own {
            self.dispatch(crate::Action::BeginGesture(label.into()))?;
        }
        let mut first_error = None;
        for &c in clips {
            if let Err(e) = edit(self, c) {
                first_error.get_or_insert(e);
            }
        }
        if own {
            self.dispatch(crate::Action::EndGesture)?;
        }
        first_error.map_or(Ok(()), Err)
    }

    pub(crate) fn quantize_clips(&mut self, clips: &[ClipId]) -> Result<()> {
        let q = self.editor.quantize_settings();
        self.each_clip("Quantize", clips, |s, c| match s.clip_kind(c) {
            Kind::Audio => s.quantize_warp(c),
            Kind::Midi(_) => s.note_operation(c, &[], &NoteOp::Quantize(q)),
            Kind::Other => Ok(()),
        })
    }

    pub(crate) fn humanize_clips(&mut self, clips: &[ClipId]) -> Result<()> {
        self.each_clip("Humanize", clips, |s, c| match s.clip_kind(c) {
            Kind::Audio => s.humanize_warp(c),
            Kind::Midi(start) => {
                let op = s.humanize_op(start);
                s.note_operation(c, &[], &op)
            }
            Kind::Other => Ok(()),
        })
    }

    /// Move the clip's transients by random amounts up to the humanize
    /// timing (where they play now, not their grid positions).
    pub fn humanize_warp(&mut self, clip: ClipId) -> Result<()> {
        let c = self.gesture_clip(clip)?;
        let mut a = crate::warping::audio(&c)?;
        let (off, len) = (a.source_offset, a.length);
        let w = crate::warping::warp_of(&a);
        let hits = self.transients_for_warp(a.source)?;
        let rate = self.project.sample_rate as f64;
        let spread = self.editor.humanize.timing_ms.max(0.0) as f64 / 1000.0 * rate;
        if spread < 1.0 {
            return Ok(());
        }
        let mut rng =
            Rng::new(self.history.revision() ^ clip.0.wrapping_mul(0x9e37_79b9_7f4a_7c15));
        let mut h = Warp {
            source_length: w.source_length,
            markers: Vec::new(),
            algorithm: w.algorithm,
        };
        let mut last = 0;
        for s in hits
            .into_iter()
            .filter(|s| w.source_range(off).contains(s) && *s > off)
        {
            let at = w.output_of(off, len, s) + (rng.signed() * spread).round() as i64;
            if at > last && at < len {
                h.markers.push(WarpMarker { at, source: s });
                last = at;
            }
        }
        h.normalize(off, len);
        a.warp = Some(h);
        self.set_audio("Humanize Transients", &c, c.start, a)
    }
}
