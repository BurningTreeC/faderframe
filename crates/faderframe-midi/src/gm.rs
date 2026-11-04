//! General MIDI names: the 128 programs of the GM sound set, the drum
//! kits GM 2 places on channel 10 (by program number, 0-based) and the
//! percussion sounds of the GM key map (keys 35–81 on channel 10).

/// The GM programs, 0-based.
pub const PROGRAMS: [&str; 128] = [
    "Acoustic Grand Piano",
    "Bright Acoustic Piano",
    "Electric Grand Piano",
    "Honky-tonk Piano",
    "Electric Piano 1",
    "Electric Piano 2",
    "Harpsichord",
    "Clavi",
    "Celesta",
    "Glockenspiel",
    "Music Box",
    "Vibraphone",
    "Marimba",
    "Xylophone",
    "Tubular Bells",
    "Dulcimer",
    "Drawbar Organ",
    "Percussive Organ",
    "Rock Organ",
    "Church Organ",
    "Reed Organ",
    "Accordion",
    "Harmonica",
    "Tango Accordion",
    "Acoustic Guitar (nylon)",
    "Acoustic Guitar (steel)",
    "Electric Guitar (jazz)",
    "Electric Guitar (clean)",
    "Electric Guitar (muted)",
    "Overdriven Guitar",
    "Distortion Guitar",
    "Guitar Harmonics",
    "Acoustic Bass",
    "Electric Bass (finger)",
    "Electric Bass (pick)",
    "Fretless Bass",
    "Slap Bass 1",
    "Slap Bass 2",
    "Synth Bass 1",
    "Synth Bass 2",
    "Violin",
    "Viola",
    "Cello",
    "Contrabass",
    "Tremolo Strings",
    "Pizzicato Strings",
    "Orchestral Harp",
    "Timpani",
    "String Ensemble 1",
    "String Ensemble 2",
    "Synth Strings 1",
    "Synth Strings 2",
    "Choir Aahs",
    "Voice Oohs",
    "Synth Voice",
    "Orchestra Hit",
    "Trumpet",
    "Trombone",
    "Tuba",
    "Muted Trumpet",
    "French Horn",
    "Brass Section",
    "Synth Brass 1",
    "Synth Brass 2",
    "Soprano Sax",
    "Alto Sax",
    "Tenor Sax",
    "Baritone Sax",
    "Oboe",
    "English Horn",
    "Bassoon",
    "Clarinet",
    "Piccolo",
    "Flute",
    "Recorder",
    "Pan Flute",
    "Blown Bottle",
    "Shakuhachi",
    "Whistle",
    "Ocarina",
    "Lead 1 (square)",
    "Lead 2 (sawtooth)",
    "Lead 3 (calliope)",
    "Lead 4 (chiff)",
    "Lead 5 (charang)",
    "Lead 6 (voice)",
    "Lead 7 (fifths)",
    "Lead 8 (bass + lead)",
    "Pad 1 (new age)",
    "Pad 2 (warm)",
    "Pad 3 (polysynth)",
    "Pad 4 (choir)",
    "Pad 5 (bowed)",
    "Pad 6 (metallic)",
    "Pad 7 (halo)",
    "Pad 8 (sweep)",
    "FX 1 (rain)",
    "FX 2 (soundtrack)",
    "FX 3 (crystal)",
    "FX 4 (atmosphere)",
    "FX 5 (brightness)",
    "FX 6 (goblins)",
    "FX 7 (echoes)",
    "FX 8 (sci-fi)",
    "Sitar",
    "Banjo",
    "Shamisen",
    "Koto",
    "Kalimba",
    "Bagpipe",
    "Fiddle",
    "Shanai",
    "Tinkle Bell",
    "Agogo",
    "Steel Drums",
    "Woodblock",
    "Taiko Drum",
    "Melodic Tom",
    "Synth Drum",
    "Reverse Cymbal",
    "Guitar Fret Noise",
    "Breath Noise",
    "Seashore",
    "Bird Tweet",
    "Telephone Ring",
    "Helicopter",
    "Applause",
    "Gunshot",
];

/// GM 2's drum kits (channel 10), by 0-based program.
const KITS: [(u8, &str); 9] = [
    (0, "Standard Kit"),
    (8, "Room Kit"),
    (16, "Power Kit"),
    (24, "Electronic Kit"),
    (25, "Analog Kit"),
    (32, "Jazz Kit"),
    (40, "Brush Kit"),
    (48, "Orchestra Kit"),
    (56, "SFX Kit"),
];

/// The GM percussion key map (channel 10): key and sound.
pub const DRUMS: [(u8, &str); 47] = [
    (35, "Acoustic Bass Drum"),
    (36, "Bass Drum 1"),
    (37, "Side Stick"),
    (38, "Acoustic Snare"),
    (39, "Hand Clap"),
    (40, "Electric Snare"),
    (41, "Low Floor Tom"),
    (42, "Closed Hi-Hat"),
    (43, "High Floor Tom"),
    (44, "Pedal Hi-Hat"),
    (45, "Low Tom"),
    (46, "Open Hi-Hat"),
    (47, "Low-Mid Tom"),
    (48, "Hi-Mid Tom"),
    (49, "Crash Cymbal 1"),
    (50, "High Tom"),
    (51, "Ride Cymbal 1"),
    (52, "Chinese Cymbal"),
    (53, "Ride Bell"),
    (54, "Tambourine"),
    (55, "Splash Cymbal"),
    (56, "Cowbell"),
    (57, "Crash Cymbal 2"),
    (58, "Vibraslap"),
    (59, "Ride Cymbal 2"),
    (60, "Hi Bongo"),
    (61, "Low Bongo"),
    (62, "Mute Hi Conga"),
    (63, "Open Hi Conga"),
    (64, "Low Conga"),
    (65, "High Timbale"),
    (66, "Low Timbale"),
    (67, "High Agogo"),
    (68, "Low Agogo"),
    (69, "Cabasa"),
    (70, "Maracas"),
    (71, "Short Whistle"),
    (72, "Long Whistle"),
    (73, "Short Guiro"),
    (74, "Long Guiro"),
    (75, "Claves"),
    (76, "Hi Wood Block"),
    (77, "Low Wood Block"),
    (78, "Mute Cuica"),
    (79, "Open Cuica"),
    (80, "Mute Triangle"),
    (81, "Open Triangle"),
];

/// The GM percussion sound on `key` (channel 10).
pub fn drum_name(key: u8) -> Option<&'static str> {
    DRUMS.iter().find(|(k, _)| *k == key).map(|(_, n)| *n)
}

/// The GM name of `program` (0-based) on `channel` (0-based; 9 = drums).
pub fn program_name(channel: u8, program: u8) -> Option<&'static str> {
    if channel == 9 {
        KITS.iter().find(|(p, _)| *p == program).map(|(_, n)| *n)
    } else {
        PROGRAMS.get(usize::from(program)).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(program_name(0, 0), Some("Acoustic Grand Piano"));
        assert_eq!(program_name(3, 127), Some("Gunshot"));
        assert_eq!(program_name(9, 25), Some("Analog Kit"));
        assert_eq!(program_name(9, 1), None);
        assert_eq!(program_name(0, 128), None);
        assert_eq!(drum_name(36), Some("Bass Drum 1"));
        assert_eq!(drum_name(81), Some("Open Triangle"));
        assert_eq!(drum_name(34), None);
        assert!(DRUMS.windows(2).all(|w| w[1].0 == w[0].0 + 1));
    }
}
