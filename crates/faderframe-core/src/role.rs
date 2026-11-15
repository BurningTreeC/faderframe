//! What a track plays in the mix, as the user says it does.
//!
//! Mix Intelligence (`faderframe-mix-intelligence`) reads roles to know
//! which source of a pair is the one that should come through (a lead
//! vocal over its accompaniment, a kick against its bass). A role is only
//! ever assigned by the user: track names are not read as instruments.
//! [`TrackRole::suggest`] guesses one from a name for the user to confirm —
//! a suggestion only, never applied by itself. Tracks without a role are
//! analysed neutrally.

use serde::{Deserialize, Serialize};

/// A track's part in the mix.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackRole {
    Kick,
    Snare,
    /// The rest of a drum kit: toms, overheads, room microphones.
    Drums,
    Percussion,
    Bass,
    LeadVocal,
    BackingVocals,
    /// A lead guitar, synth or solo instrument (in front, like a voice).
    LeadInstrument,
    RhythmGuitar,
    Keys,
    /// Pads, strings, sustained beds.
    Pads,
    /// Effects, atmospheres, risers.
    Effects,
    Other,
    /// Not analysed by Mix Intelligence (a click, a guide or scratch track,
    /// a bed meant to cover the rest): it still plays in the mix and in
    /// what the master and its buses measure.
    Ignore,
}

/// Where a role belongs (what the masking rules weigh pairs by).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RoleFamily {
    /// Parts of one drum kit (picked up by several microphones).
    DrumKit,
    /// The low end: the bass (the kick is also in the kit).
    LowEnd,
    /// What should stand in front: lead vocal, lead instrument.
    Foreground,
    /// Everything that accompanies.
    Accompaniment,
    /// Effects and atmospheres (masking is often what they are for).
    Effects,
}

impl TrackRole {
    pub const ALL: [TrackRole; 14] = [
        TrackRole::Kick,
        TrackRole::Snare,
        TrackRole::Drums,
        TrackRole::Percussion,
        TrackRole::Bass,
        TrackRole::LeadVocal,
        TrackRole::BackingVocals,
        TrackRole::LeadInstrument,
        TrackRole::RhythmGuitar,
        TrackRole::Keys,
        TrackRole::Pads,
        TrackRole::Effects,
        TrackRole::Other,
        TrackRole::Ignore,
    ];

    pub fn label(self) -> &'static str {
        match self {
            TrackRole::Kick => "Kick",
            TrackRole::Snare => "Snare",
            TrackRole::Drums => "Drums (Kit, Overheads, Room)",
            TrackRole::Percussion => "Percussion",
            TrackRole::Bass => "Bass",
            TrackRole::LeadVocal => "Lead Vocal",
            TrackRole::BackingVocals => "Backing Vocals",
            TrackRole::LeadInstrument => "Lead Instrument",
            TrackRole::RhythmGuitar => "Rhythm Guitar",
            TrackRole::Keys => "Keys",
            TrackRole::Pads => "Pads and Strings",
            TrackRole::Effects => "Effects",
            TrackRole::Other => "Other",
            TrackRole::Ignore => "Ignore (Not Analysed)",
        }
    }

    /// Mix Intelligence leaves the track out ([`TrackRole::Ignore`]).
    pub fn ignored(role: Option<TrackRole>) -> bool {
        role == Some(TrackRole::Ignore)
    }

    /// A short name (lists, menus of several tracks).
    pub fn short(self) -> &'static str {
        match self {
            TrackRole::Drums => "Drums",
            TrackRole::Pads => "Pads",
            TrackRole::Ignore => "Ignored",
            _ => self.label(),
        }
    }

    /// A role a track named `name` probably plays (English and Polish
    /// words for the usual instruments and microphones: "Kick In", "SN
    /// Top", "OH L", "Bass DI", "Lead Vox", "Gitara 2", "Chórki"), for the
    /// user to confirm; `None` when the name says nothing.
    pub fn suggest(name: &str) -> Option<TrackRole> {
        let lower = name.to_lowercase();
        let words: Vec<&str> = lower
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .collect();
        let word = |list: &[&str]| words.iter().any(|w| list.contains(w));
        let starts = |list: &[&str]| words.iter().any(|w| list.iter().any(|p| w.starts_with(p)));
        let has = |list: &[&str]| list.iter().any(|p| lower.contains(p));
        // Tracks that help recording, not the mix: offered to be left out.
        if starts(&["click", "metronom", "guide", "scratch", "klik"])
            || word(&["ref", "reference", "referencja", "cue"])
        {
            return Some(TrackRole::Ignore);
        }
        // Most specific first: a bass drum is a kick, a lead vocal not a
        // backing one.
        if has(&["bass drum", "kick"]) || word(&["bd", "kik", "stopa", "centrala"]) {
            return Some(TrackRole::Kick);
        }
        if starts(&["snare", "werbel"]) || word(&["sn", "sd"]) {
            return Some(TrackRole::Snare);
        }
        if starts(&[
            "overhead", "tom", "hihat", "cymbal", "talerz", "blach", "drum", "perkus",
        ]) || word(&[
            "oh", "hh", "hat", "ride", "crash", "china", "room", "rooms", "kit", "floor",
        ]) {
            return Some(TrackRole::Drums);
        }
        if starts(&[
            "perc", "shaker", "conga", "bongo", "tamb", "clap", "cowbell", "cajon",
        ]) {
            return Some(TrackRole::Percussion);
        }
        if starts(&["bass", "bas"]) || word(&["sub", "808"]) {
            return Some(TrackRole::Bass);
        }
        if starts(&["backing", "chór", "chor", "choir", "harmon", "dubel"])
            || word(&["bv", "bvs", "bgv", "bgvs"])
        {
            return Some(TrackRole::BackingVocals);
        }
        if starts(&["vox", "vocal", "voc", "wokal", "śpiew", "spiew"]) || word(&["lv"]) {
            return Some(TrackRole::LeadVocal);
        }
        if word(&["solo", "lead"]) {
            return Some(TrackRole::LeadInstrument);
        }
        if starts(&["guitar", "gtr", "gitar", "rhythm", "acoustic", "akust"])
            || word(&["gt", "egt", "agt"])
        {
            return Some(TrackRole::RhythmGuitar);
        }
        if starts(&[
            "key", "piano", "pian", "organ", "organy", "rhodes", "wurli", "clav", "klawisz",
        ]) || word(&["epiano", "ep"])
        {
            return Some(TrackRole::Keys);
        }
        if starts(&[
            "pad", "string", "smycz", "skrzyp", "violin", "cello", "wiolon",
        ]) {
            return Some(TrackRole::Pads);
        }
        if starts(&[
            "fx", "riser", "sweep", "atmo", "ambien", "noise", "impact", "efekt",
        ]) {
            return Some(TrackRole::Effects);
        }
        None
    }

    pub fn family(self) -> RoleFamily {
        match self {
            TrackRole::Kick | TrackRole::Snare | TrackRole::Drums => RoleFamily::DrumKit,
            TrackRole::Bass => RoleFamily::LowEnd,
            TrackRole::LeadVocal | TrackRole::LeadInstrument => RoleFamily::Foreground,
            // Never analysed; weighed like effects should one be compared.
            TrackRole::Effects | TrackRole::Ignore => RoleFamily::Effects,
            TrackRole::Percussion
            | TrackRole::BackingVocals
            | TrackRole::RhythmGuitar
            | TrackRole::Keys
            | TrackRole::Pads
            | TrackRole::Other => RoleFamily::Accompaniment,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_are_suggested_from_the_usual_names() {
        use TrackRole::*;
        for (name, role) in [
            ("Kick In", Some(Kick)),
            ("BD out", Some(Kick)),
            ("Bass Drum", Some(Kick)),
            ("SN Top", Some(Snare)),
            ("Snare Bottom", Some(Snare)),
            ("Werbel", Some(Snare)),
            ("OH L", Some(Drums)),
            ("Overheads", Some(Drums)),
            ("Tom 1", Some(Drums)),
            ("Room", Some(Drums)),
            ("HiHat", Some(Drums)),
            ("Shaker", Some(Percussion)),
            ("Bass DI", Some(Bass)),
            ("Bas", Some(Bass)),
            ("Lead Vox", Some(LeadVocal)),
            ("Wokal", Some(LeadVocal)),
            ("Vocal 2", Some(LeadVocal)),
            ("BV 1", Some(BackingVocals)),
            ("Chórki", Some(BackingVocals)),
            ("Guitar Solo", Some(LeadInstrument)),
            ("Gitara L", Some(RhythmGuitar)),
            ("GTR 2", Some(RhythmGuitar)),
            ("Piano", Some(Keys)),
            ("Organy", Some(Keys)),
            ("Strings", Some(Pads)),
            ("Riser FX", Some(Effects)),
            ("Click", Some(Ignore)),
            ("Guide Vox", Some(Ignore)),
            ("Scratch Gtr", Some(Ignore)),
            ("Ref Mix", Some(Ignore)),
            ("Napierdol", None),
            ("Audio 1", None),
        ] {
            assert_eq!(TrackRole::suggest(name), role, "{name}");
        }
    }

    #[test]
    fn roles_round_trip_and_have_families() {
        for r in TrackRole::ALL {
            let s = serde_json::to_string(&r).unwrap_or_default();
            let back: TrackRole = serde_json::from_str(&s).unwrap_or(TrackRole::Other);
            assert_eq!(back, r, "{s}");
            assert!(!r.label().is_empty());
        }
        assert_eq!(TrackRole::Kick.family(), RoleFamily::DrumKit);
        assert_eq!(TrackRole::LeadVocal.family(), RoleFamily::Foreground);
        assert_eq!(
            serde_json::to_string(&TrackRole::LeadVocal).unwrap_or_default(),
            "\"lead_vocal\""
        );
    }
}
