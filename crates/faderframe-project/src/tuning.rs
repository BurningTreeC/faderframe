//! Microtonal tuning of a project.
//!
//! The project has a tuning ([`Project::tuning`], `None` = twelve-tone
//! equal temperament) and a reference pitch ([`Project::reference_pitch`],
//! A4 in Hz); a track may play a tuning of its own ([`Track::tuning`]).
//! The tunings themselves (Scala scales and keyboard mappings, equal
//! temperaments, the key table) are `faderframe_midi::tuning`'s.
//!
//! What plays them: built-in instruments read the key table directly;
//! hosted plugins and MIDI outputs are retuned as [`Track::retune`] says
//! (see [`Retune`]). The key and chord track stay in twelve-tone pitch
//! classes whatever the tuning: they name keys of the MIDI keyboard, not
//! pitches.

use crate::{Project, Track};
use faderframe_core::TrackId;
use faderframe_midi::tuning::reference_or_standard;
pub use faderframe_midi::tuning::{
    A4_HZ, KeyDegree, KeyboardMap, REFERENCE_RANGE, ScalaScale, ScaleSource, Tuning, TuningError,
    TuningTable, mts,
};
use serde::{Deserialize, Serialize};

/// How a track's hosted plugins and MIDI outputs are told its tuning
/// (built-in instruments always read the table).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Retune {
    /// MPE on tracks with [`Track::mpe`]; else note expressions for
    /// plugins that take tuning per note (CLAP, VST3 listing it) and MIDI
    /// 2.0 ports, MTS SysEx for the others and MIDI 1.0 ports.
    #[default]
    Auto,
    /// MIDI Tuning Standard SysEx only (a bulk dump, then real-time
    /// single note changes when the tuning changes).
    Mts,
    /// A tuning note expression after every note-on (per-note pitch on
    /// MIDI 2.0 ports).
    Expression,
    /// Each note's member-channel pitch bend carries its key's offset
    /// (tracks with [`Track::mpe`] only).
    Mpe,
    /// Not retuned: plugins and devices play their own tuning.
    Off,
}

impl Retune {
    pub const ALL: [Retune; 5] = [
        Self::Auto,
        Self::Mts,
        Self::Expression,
        Self::Mpe,
        Self::Off,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Automatic",
            Self::Mts => "MIDI Tuning Standard (SysEx)",
            Self::Expression => "Note Expression",
            Self::Mpe => "MPE Pitch Bend",
            Self::Off => "Off (the Plugin's Own Tuning)",
        }
    }

    /// The word dev actions and files use.
    pub fn id(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Mts => "mts",
            Self::Expression => "expression",
            Self::Mpe => "mpe",
            Self::Off => "off",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|r| r.id() == id)
    }

    pub fn is_auto(&self) -> bool {
        *self == Self::Auto
    }
}

/// The reference pitch of new projects (A4 = 440 Hz).
pub fn default_reference() -> f64 {
    A4_HZ
}

pub(crate) fn is_default_reference(hz: &f64) -> bool {
    *hz == A4_HZ
}

/// The equal temperaments menus offer (steps to the octave).
pub const EQUAL_TEMPERAMENTS: [u32; 10] = [5, 7, 10, 12, 15, 17, 19, 22, 24, 31];
/// More of them, in a submenu.
pub const MORE_EQUAL_TEMPERAMENTS: [u32; 8] = [11, 13, 16, 26, 34, 41, 53, 72];

impl Project {
    /// The tuning `track` plays: its own, else the project's (`None`:
    /// twelve-tone equal temperament).
    pub fn tuning_of(&self, track: TrackId) -> Option<&Tuning> {
        self.track(track)
            .and_then(|t| t.tuning.as_ref())
            .or(self.tuning.as_ref())
    }

    /// The reference pitch in use (A4, Hz).
    pub fn reference_hz(&self) -> f64 {
        reference_or_standard(self.reference_pitch)
    }

    /// What each key of `track` sounds.
    pub fn tuning_table(&self, track: TrackId) -> TuningTable {
        let a4 = self.reference_hz();
        match self.tuning_of(track) {
            Some(t) => t.table(a4),
            None if a4 == A4_HZ => TuningTable::STANDARD,
            None => Tuning::twelve_tet().table(a4),
        }
    }

    /// Does `track` play anything but twelve-tone equal temperament at
    /// 440 Hz?
    pub fn is_retuned(&self, track: TrackId) -> bool {
        !self.tuning_table(track).is_standard()
    }

    /// The name of the tuning `track` plays, with the reference pitch when
    /// it is not 440 Hz ("19-EDO", "12-TET · A = 432 Hz").
    pub fn tuning_name(&self, track: TrackId) -> String {
        let name = self
            .tuning_of(track)
            .map_or_else(|| "12-TET".to_string(), |t| t.name.clone());
        let a4 = self.reference_hz();
        if a4 == A4_HZ {
            name
        } else {
            format!("{name} · A = {} Hz", trim_hz(a4))
        }
    }
}

impl Track {
    /// Does the track play notes (so a tuning means something to it)?
    pub fn takes_tuning(&self) -> bool {
        matches!(
            self.kind,
            crate::TrackKind::Instrument | crate::TrackKind::Midi
        )
    }
}

/// "440", "432.5": a frequency without trailing zeros.
pub fn trim_hz(hz: f64) -> String {
    let s = format!("{hz:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::{Command, TrackColor, TrackKind};

    fn project() -> (Project, TrackId) {
        let mut p = Project::new("t", 48_000);
        let id = p.ids.allocate();
        let t = Track::new(id, TrackKind::Instrument, "Keys", TrackColor::palette(0));
        Command::AddTrack {
            track: Box::new(t),
            index: 0,
        }
        .apply(&mut p)
        .unwrap();
        (p, id)
    }

    #[test]
    fn a_track_follows_the_project_until_it_has_its_own() {
        let (mut p, keys) = project();
        assert!(!p.is_retuned(keys));
        assert_eq!(p.tuning_name(keys), "12-TET");
        let undo = Command::SetTuning {
            track: None,
            tuning: Some(Tuning::equal(19)),
        }
        .apply(&mut p)
        .unwrap();
        assert!(p.is_retuned(keys));
        assert_eq!(p.tuning_name(keys), "19-EDO");
        let own = Command::SetTuning {
            track: Some(keys),
            tuning: Some(Tuning::equal(31)),
        }
        .apply(&mut p)
        .unwrap();
        assert_eq!(p.tuning_name(keys), "31-EDO");
        own.apply(&mut p).unwrap();
        assert_eq!(p.tuning_name(keys), "19-EDO");
        undo.apply(&mut p).unwrap();
        assert!(!p.is_retuned(keys));
    }

    #[test]
    fn the_reference_pitch_is_checked_coalesced_and_named() {
        let (mut p, keys) = project();
        let set = |hz| Command::SetReferencePitch { hz };
        assert!(set(100.0).apply(&mut p).is_err());
        assert!(set(f64::NAN).apply(&mut p).is_err());
        let undo = set(432.0).apply(&mut p).unwrap();
        assert_eq!(p.reference_hz(), 432.0);
        assert!(p.is_retuned(keys), "12-TET at 432 Hz is a retuning");
        assert_eq!(p.tuning_name(keys), "12-TET · A = 432 Hz");
        assert!((p.tuning_table(keys).frequency(69).unwrap() - 432.0).abs() < 1e-9);
        assert_eq!(set(431.0).coalesce_key(), set(433.0).coalesce_key());
        assert!(set(431.0).coalesce_key().is_some());
        assert_eq!(set(431.0).impact(), crate::Impact::Timeline);
        undo.apply(&mut p).unwrap();
        assert_eq!(p.reference_pitch, 440.0);
    }

    #[test]
    fn retuning_settings_are_undone_and_rebuild_the_graph() {
        let (mut p, keys) = project();
        let cmd = Command::SetTrackRetune {
            track: keys,
            retune: Retune::Mts,
        };
        assert_eq!(cmd.impact(), crate::Impact::Graph);
        assert_eq!(cmd.label(), "Retune: MIDI Tuning Standard (SysEx)");
        let undo = cmd.apply(&mut p).unwrap();
        assert_eq!(p.track(keys).unwrap().retune, Retune::Mts);
        undo.apply(&mut p).unwrap();
        assert_eq!(p.track(keys).unwrap().retune, Retune::Auto);
        let bad = Command::SetTuning {
            track: Some(TrackId(999_999)),
            tuning: None,
        };
        assert!(bad.apply(&mut p).is_err());
        for r in Retune::ALL {
            assert_eq!(Retune::from_id(r.id()), Some(r));
        }
    }

    #[test]
    fn tunings_are_saved_with_their_degrees_and_old_files_read() {
        use faderframe_midi::tuning::{KeyboardMap, ScalaScale};
        let (mut p, keys) = project();
        let scale = ScalaScale::parse("just\n3\n5/4\n3/2\n2/1\n").unwrap();
        let mut map = KeyboardMap::standard(3);
        map.reference_hz = 256.0;
        let t = Tuning::scala("Just triad", scale, Some(map)).unwrap();
        Command::SetTuning {
            track: Some(keys),
            tuning: Some(t.clone()),
        }
        .apply(&mut p)
        .unwrap();
        Command::SetReferencePitch { hz: 415.0 }
            .apply(&mut p)
            .unwrap();
        Command::SetTuning {
            track: None,
            tuning: Some(Tuning::equal(24)),
        }
        .apply(&mut p)
        .unwrap();
        let json = serde_json::to_string(&p).unwrap();
        assert!(json.contains("\"reference_pitch\":415.0"));
        assert!(json.contains("\"type\":\"equal\""));
        assert!(json.contains("\"5/4\""));
        let back: Project = serde_json::from_str(&json).unwrap();
        assert_eq!(back.tuning_of(keys), Some(&t));
        assert_eq!(back.tuning, Some(Tuning::equal(24)));
        assert_eq!(back.reference_pitch, 415.0);
        // A project without them is in 12-TET at 440 Hz and says nothing
        // about it.
        let plain = Project::new("plain", 48_000);
        let json = serde_json::to_string(&plain).unwrap();
        assert!(!json.contains("tuning"));
        assert!(!json.contains("reference_pitch"));
        assert!(!json.contains("retune"));
        let back: Project = serde_json::from_str(&json).unwrap();
        assert_eq!(back.reference_pitch, 440.0);
        assert_eq!(back.tuning, None);
    }
}
