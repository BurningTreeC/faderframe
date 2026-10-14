//! Calibrating Voice to MIDI to a room and a performer: a few seconds of
//! the room alone, then the performer singing or playing as they will
//! (soft and loud), both counted by a listener's [`Meter`](super::Meter);
//! from them the gate (over the room's noise and any hum, under the soft
//! notes) and the dynamics velocities span.

use super::Dynamics;
use super::level::Levels;

/// The gate sits at least this far (dB) over the room's louder frames (its
/// noise has no clear pitch: a little over it is enough), and this over
/// pitched noise heard in it (hum, a fan's tone).
const OVER_ROOM_DB: f32 = 3.0;
const OVER_PITCHED_ROOM_DB: f32 = 4.0;
/// ... and this far under the performance's soft notes where it can.
const UNDER_SOFT_DB: f32 = 6.0;
/// The room's frames counted as its noise (the share under), and the
/// pitched frames that make it a room with a tone.
const ROOM_SHARE: f64 = 0.95;
const PITCHED_ROOM_SHARE: f64 = 0.02;
/// The performance's soft and loud levels (shares of its voiced frames).
const SOFT_SHARE: f64 = 0.1;
const LOUD_SHARE: f64 = 0.97;
/// Least voiced share of the performance's frames.
const VOICED_SHARE: f64 = 0.05;
/// Dynamics at least this wide (dB).
const NARROWEST_DB: f32 = 12.0;
/// Louder than this (dBFS, the loud notes' frames) is close to clipping;
/// quieter than this, the input's gain is low.
const HOT_DB: f32 = -6.0;
const QUIET_DB: f32 = -50.0;

/// What a calibration found and the settings it gives.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Calibration {
    /// The room's noise (its louder frames, dBFS).
    pub room_db: f32,
    /// Pitched noise in the room (its louder pitched frames), if any.
    pub hum_db: Option<f32>,
    /// The performance's soft and loud levels.
    pub dynamics: Dynamics,
    /// The gate to use (dBFS).
    pub threshold_db: f32,
    /// Something to tell the performer.
    pub advice: Option<Advice>,
}

/// Advice from a calibration that still worked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Advice {
    /// The loud notes come close to full scale: lower the input gain.
    TooHot,
    /// The performance is quiet: raise the input gain.
    TooQuiet,
    /// The soft notes are hardly over the room: raise the gain, come
    /// closer to the microphone, or quieten the room.
    CloseToRoom,
}

impl Advice {
    pub fn text(self) -> &'static str {
        match self {
            Advice::TooHot => {
                "The loudest notes come close to clipping: lower the input gain a little."
            }
            Advice::TooQuiet => "The input is quiet: raising its gain makes the soft notes surer.",
            Advice::CloseToRoom => {
                "The soft notes are hardly louder than the room: raise the input gain, come closer to the microphone or make the room quieter."
            }
        }
    }
}

/// Why a calibration did not work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CalibrationError {
    /// Nothing was counted (no input).
    NoInput,
    /// Too little with a clear pitch was heard during the performance (in
    /// a room as loud as the performance too).
    NothingPlayed,
}

impl CalibrationError {
    pub fn text(self) -> &'static str {
        match self {
            CalibrationError::NoInput => {
                "Nothing reached the input: check that the audio device is running and the input is the one you sing or play into."
            }
            CalibrationError::NothingPlayed => {
                "Too little with a clear pitch was heard: sing or play for most of the time, a little louder or nearer the microphone, and check the input."
            }
        }
    }
}

/// Calibrate from what a listener counted in the room alone and during a
/// performance.
pub fn calibrate(room: &Levels, performance: &Levels) -> Result<Calibration, CalibrationError> {
    let frames = performance.frames(false);
    if frames == 0 || room.frames(false) == 0 {
        return Err(CalibrationError::NoInput);
    }
    let voiced = performance.frames(true);
    if f64::from(voiced) < VOICED_SHARE * f64::from(frames) || voiced < 20 {
        return Err(CalibrationError::NothingPlayed);
    }
    let room_db = room
        .percentile(false, ROOM_SHARE)
        .ok_or(CalibrationError::NoInput)?;
    let hum_db = (f64::from(room.frames(true))
        >= PITCHED_ROOM_SHARE * f64::from(room.frames(false)))
    .then(|| room.percentile(true, ROOM_SHARE))
    .flatten();
    let soft = performance
        .percentile(true, SOFT_SHARE)
        .ok_or(CalibrationError::NothingPlayed)?;
    let loud = performance
        .percentile(true, LOUD_SHARE)
        .ok_or(CalibrationError::NothingPlayed)?;
    // Over the room (and its tone), under the soft notes: nearer the room
    // (quiet notes still count). Where they overlap the soft notes win —
    // the trackers' own floor and their ear for pitch keep the room out.
    let lowest =
        (room_db + OVER_ROOM_DB).max(hum_db.map_or(f32::MIN, |h| h + OVER_PITCHED_ROOM_DB));
    let highest = soft - UNDER_SOFT_DB;
    let close = lowest > highest;
    let threshold_db = if close {
        highest
    } else {
        lowest + 0.35 * (highest - lowest)
    }
    .clamp(-100.0, -10.0);
    let advice = if loud > HOT_DB {
        Some(Advice::TooHot)
    } else if close {
        Some(Advice::CloseToRoom)
    } else if loud < QUIET_DB {
        Some(Advice::TooQuiet)
    } else {
        None
    };
    Ok(Calibration {
        room_db,
        hum_db,
        dynamics: Dynamics {
            soft_db: soft.min(loud - NARROWEST_DB),
            loud_db: loud,
        },
        threshold_db,
        advice,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn levels(spec: &[(f32, u32, bool)]) -> Levels {
        let mut l = Levels::default();
        for &(db, n, voiced) in spec {
            for _ in 0..n {
                l.count(db, voiced);
            }
        }
        l
    }

    #[test]
    fn the_gate_sits_between_the_room_and_the_soft_notes() {
        let room = levels(&[(-78.0, 900, false), (-72.0, 100, false)]);
        let sung = levels(&[
            (-75.0, 400, false),
            (-40.0, 100, true),
            (-30.0, 300, true),
            (-18.0, 100, true),
        ]);
        let c = calibrate(&room, &sung).unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(c.room_db, -72.0);
        assert_eq!(c.hum_db, None);
        assert_eq!(c.dynamics.soft_db, -40.0);
        assert_eq!(c.dynamics.loud_db, -18.0);
        // Over the room, under the soft notes, nearer the room.
        assert!((-69.0..=-58.0).contains(&c.threshold_db), "{c:?}");
        assert_eq!(c.advice, None);
    }

    #[test]
    fn hum_in_the_room_raises_the_gate_over_it() {
        let room = levels(&[(-80.0, 800, false), (-55.0, 200, true)]);
        let sung = levels(&[(-30.0, 500, true), (-22.0, 100, true)]);
        let c = calibrate(&room, &sung).unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(c.hum_db, Some(-55.0));
        assert!(c.threshold_db >= -51.0, "{c:?}");
    }

    #[test]
    fn a_quiet_or_hot_input_is_told_and_nothing_played_fails() {
        let room = levels(&[(-90.0, 1000, false)]);
        let quiet = levels(&[(-62.0, 300, true), (-56.0, 100, true)]);
        let c = calibrate(&room, &quiet).unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(c.advice, Some(Advice::TooQuiet));
        assert!(c.threshold_db < -62.0, "{c:?}");
        let hot = levels(&[(-20.0, 300, true), (-3.0, 100, true)]);
        assert_eq!(
            calibrate(&room, &hot).map(|c| c.advice),
            Ok(Some(Advice::TooHot))
        );
        // Soft notes in the room's noise: they count all the same.
        let close = levels(&[(-80.0, 300, true), (-70.0, 100, true)]);
        let noisy = levels(&[(-84.0, 1000, false)]);
        let c = calibrate(&noisy, &close).unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(c.advice, Some(Advice::CloseToRoom));
        assert!(c.threshold_db <= -80.0, "{c:?}");
        let breath = levels(&[(-40.0, 1000, false), (-35.0, 10, true)]);
        assert_eq!(
            calibrate(&room, &breath),
            Err(CalibrationError::NothingPlayed)
        );
        assert_eq!(
            calibrate(&Levels::default(), &quiet),
            Err(CalibrationError::NoInput)
        );
        let loud_room = levels(&[(-30.0, 1000, false)]);
        assert_eq!(
            calibrate(&loud_room, &quiet).map(|c| c.advice),
            Ok(Some(Advice::CloseToRoom))
        );
    }
}
