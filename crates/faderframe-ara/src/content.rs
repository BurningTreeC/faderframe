//! Content events in Rust form: what the host tells the plug-in about its
//! musical context (tempo, bars, keys, chords) and what it reads back
//! (notes, and the same kinds of events the plug-in detected).

use crate::ffi;
use std::ffi::{CStr, c_void};

/// A tempo sync point: song time ↔ musical time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TempoEntry {
    pub seconds: f64,
    pub quarters: f64,
}

/// A bar signature from `quarters` on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BarSignature {
    pub numerator: i32,
    pub denominator: i32,
    pub quarters: OrderedQuarters,
}

/// A position in quarter notes that compares exactly (bar signatures,
/// keys and chords sit on musical positions the project gives them).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OrderedQuarters(pub u64);

impl OrderedQuarters {
    pub fn new(q: f64) -> Self {
        Self(q.to_bits())
    }

    pub fn get(self) -> f64 {
        f64::from_bits(self.0)
    }
}

/// A key signature from `quarters` on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeySignature {
    /// The root as a circle-of-fifths index from C (G = 1, F = −1).
    pub root: i32,
    /// The scale's pitch classes from the root (index = semitones).
    pub intervals: [bool; 12],
    pub name: Option<String>,
    pub quarters: OrderedQuarters,
}

/// A chord (lead-sheet style) from `quarters` on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chord {
    /// Root and bass as circle-of-fifths indices from C.
    pub root: i32,
    pub bass: i32,
    /// Per semitone from the root: 0 unused, else the diatonic degree (or
    /// 0xFF: used, function unknown).
    pub intervals: [u8; 12],
    pub name: Option<String>,
    pub quarters: OrderedQuarters,
}

/// A note the plug-in found (song time in seconds for playback regions,
/// else relative to the source or modification).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Note {
    /// Average frequency in Hz (0: no defined pitch).
    pub frequency: f32,
    /// MIDI note number, `None` for percussive notes.
    pub pitch: Option<i32>,
    /// 0 (weak) … 1 (strong), on a perceptual scale.
    pub volume: f32,
    pub start: f64,
    /// Where the note's musical anchor is, from its start.
    pub attack: f64,
    /// Its length up to its release ("note off").
    pub length: f64,
    /// Its whole sound, release included.
    pub signal: f64,
}

/// A musical context's content as the host provides it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Context {
    /// At least two entries, one at quarter 0 (ARA's rules; see
    /// [`Context::tempo_entries`]).
    pub tempo: Vec<TempoEntry>,
    pub bars: Vec<BarSignature>,
    pub keys: Vec<KeySignature>,
    pub chords: Vec<Chord>,
}

impl Context {
    /// Tempo entries ARA accepts: sorted, one at quarter 0, at least two.
    /// Constant tempo before the first and after the last entry follows
    /// from the two nearest ones, so a constant map needs only a second
    /// point one quarter on.
    pub fn tempo_entries(points: &[TempoEntry], seconds_per_quarter: f64) -> Vec<TempoEntry> {
        let mut out: Vec<TempoEntry> = points
            .iter()
            .copied()
            .filter(|e| e.seconds.is_finite() && e.quarters.is_finite())
            .collect();
        out.sort_by(|a, b| a.quarters.total_cmp(&b.quarters));
        out.dedup_by(|a, b| a.quarters == b.quarters);
        if !out.iter().any(|e| e.quarters == 0.0) {
            out.insert(
                0,
                TempoEntry {
                    seconds: 0.0,
                    quarters: 0.0,
                },
            );
        }
        if out.len() < 2 {
            let last = out[out.len() - 1];
            out.push(TempoEntry {
                seconds: last.seconds + seconds_per_quarter.max(1e-6),
                quarters: last.quarters + 1.0,
            });
        }
        out
    }
}

/// The circle-of-fifths index of pitch class `pc` (C = 0): sharps up to F♯
/// (6), flats from D♭ (−5).
pub fn circle_of_fifths(pc: u8) -> i32 {
    let i = (i32::from(pc % 12) * 7).rem_euclid(12);
    if i > 6 { i - 12 } else { i }
}

/// The pitch class (C = 0) of a circle-of-fifths index.
pub fn pitch_class(fifths: i32) -> u8 {
    (fifths * 7).rem_euclid(12) as u8
}

/// Content the host reads from the plug-in.
pub trait PlugInContent: Sized {
    const TYPE: ffi::ContentType;
    /// # Safety
    /// `data` points to the event struct of [`Self::TYPE`], valid for the
    /// call.
    unsafe fn from_raw(data: *const c_void) -> Self;
}

/// A C string the plug-in owns, copied (None: null or empty).
///
/// # Safety
/// `s` is null or a valid NUL-terminated string.
pub(crate) unsafe fn text(s: *const std::ffi::c_char) -> Option<String> {
    if s.is_null() {
        return None;
    }
    // SAFETY: the caller's contract.
    let t = unsafe { CStr::from_ptr(s) }.to_string_lossy().into_owned();
    (!t.is_empty()).then_some(t)
}

impl PlugInContent for Note {
    const TYPE: ffi::ContentType = ffi::kARAContentTypeNotes;
    unsafe fn from_raw(data: *const c_void) -> Self {
        // SAFETY: the caller's contract; read unaligned (packed struct).
        let n = unsafe { (data as *const ffi::ContentNote).read_unaligned() };
        let pitch = n.pitchNumber;
        Note {
            frequency: n.frequency,
            pitch: (pitch != ffi::kARAInvalidPitchNumber).then_some(pitch),
            volume: n.volume,
            start: n.startPosition,
            attack: n.attackDuration,
            length: n.noteDuration,
            signal: n.signalDuration,
        }
    }
}

impl PlugInContent for TempoEntry {
    const TYPE: ffi::ContentType = ffi::kARAContentTypeTempoEntries;
    unsafe fn from_raw(data: *const c_void) -> Self {
        // SAFETY: the caller's contract.
        let e = unsafe { (data as *const ffi::ContentTempoEntry).read_unaligned() };
        TempoEntry {
            seconds: e.timePosition,
            quarters: e.quarterPosition,
        }
    }
}

impl PlugInContent for BarSignature {
    const TYPE: ffi::ContentType = ffi::kARAContentTypeBarSignatures;
    unsafe fn from_raw(data: *const c_void) -> Self {
        // SAFETY: the caller's contract.
        let b = unsafe { (data as *const ffi::ContentBarSignature).read_unaligned() };
        BarSignature {
            numerator: b.numerator,
            denominator: b.denominator,
            quarters: OrderedQuarters::new(b.position),
        }
    }
}

impl PlugInContent for KeySignature {
    const TYPE: ffi::ContentType = ffi::kARAContentTypeKeySignatures;
    unsafe fn from_raw(data: *const c_void) -> Self {
        // SAFETY: the caller's contract.
        let k = unsafe { (data as *const ffi::ContentKeySignature).read_unaligned() };
        let intervals = k.intervals;
        KeySignature {
            root: k.root,
            intervals: std::array::from_fn(|i| intervals[i] != 0),
            // SAFETY: valid as long as the event (the caller's contract).
            name: unsafe { text(k.name) },
            quarters: OrderedQuarters::new(k.position),
        }
    }
}

impl PlugInContent for Chord {
    const TYPE: ffi::ContentType = ffi::kARAContentTypeSheetChords;
    unsafe fn from_raw(data: *const c_void) -> Self {
        // SAFETY: the caller's contract.
        let c = unsafe { (data as *const ffi::ContentChord).read_unaligned() };
        Chord {
            root: c.root,
            bass: c.bass,
            intervals: c.intervals,
            // SAFETY: as above.
            name: unsafe { text(c.name) },
            quarters: OrderedQuarters::new(c.position),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pitch_classes_go_round_the_circle_of_fifths() {
        assert_eq!(circle_of_fifths(0), 0);
        assert_eq!(circle_of_fifths(7), 1, "G");
        assert_eq!(circle_of_fifths(5), -1, "F");
        assert_eq!(circle_of_fifths(10), -2, "B flat");
        assert_eq!(circle_of_fifths(6), 6, "F sharp");
        assert_eq!(circle_of_fifths(1), -5, "D flat");
        for pc in 0..12 {
            assert_eq!(pitch_class(circle_of_fifths(pc)), pc);
        }
        assert_eq!(pitch_class(7), 1, "C sharp");
        assert_eq!(pitch_class(11), 5, "E sharp");
    }

    #[test]
    fn tempo_entries_follow_aras_rules() {
        let one = Context::tempo_entries(&[], 0.5);
        assert_eq!(one.len(), 2);
        assert_eq!(one[0].quarters, 0.0);
        assert_eq!(one[1].seconds, 0.5);
        let map = Context::tempo_entries(
            &[
                TempoEntry {
                    seconds: 4.0,
                    quarters: 8.0,
                },
                TempoEntry {
                    seconds: 0.0,
                    quarters: 0.0,
                },
            ],
            0.5,
        );
        assert_eq!(map.len(), 2);
        assert_eq!(map[0].quarters, 0.0);
        assert_eq!(map[1].quarters, 8.0);
    }
}
