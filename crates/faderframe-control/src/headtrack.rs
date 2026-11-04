//! Head trackers over OSC: the listener's head orientation, as the common
//! head trackers and the ambisonic tools they feed send it.
//!
//! | In | |
//! |---|---|
//! | `/SceneRotator/ypr f f f`, `/ypr f f f` | yaw, pitch and roll together (IEM SceneRotator; SPARTA's rotator and binaural decoders) |
//! | `/SceneRotator/{yaw,pitch,roll} f`, `/{yaw,pitch,roll} f` | one of them |
//! | `/SceneRotator/quaternions f f f f`, `/quaternion f f f f` | a quaternion, `w x y z` |
//! | `/SceneRotator/{qw,qx,qy,qz} f` | one of its components |
//!
//! Angles are degrees (radians when the tracker is set so); yaw is
//! positive to the left, pitch positive up, roll positive towards the
//! right shoulder, composed yaw, then pitch, then roll — the ambisonic
//! frame (`x` front, `y` left, `z` up) quaternions are taken in. Trackers
//! that count an axis the other way are set to invert it (as SceneRotator
//! offers): its angle is negated, or its quaternion component. Any app
//! that can send one of these addresses (phone apps that send their
//! orientation over OSC can usually be told which) works as a tracker.
//! Several messages in a bundle are taken in order; the last complete
//! orientation wins.

use crate::osc::{Arg, Message};

/// What a tracker says about the head.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum HeadReading {
    /// Yaw, pitch and roll (as sent; see [`HeadInput::angles_in_radians`]).
    Angles { yaw: f32, pitch: f32, roll: f32 },
    /// A quaternion `w x y z` (not yet normalised).
    Quaternion { w: f32, x: f32, y: f32, z: f32 },
}

/// The axes a tracker counts the other way, and its angles' unit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HeadInput {
    pub invert_yaw: bool,
    pub invert_pitch: bool,
    pub invert_roll: bool,
    pub angles_in_radians: bool,
}

/// Reads head-tracker messages, keeping the parts trackers send one at a
/// time (`/yaw`, `/qw` …) until the next.
#[derive(Clone, Debug)]
pub struct HeadTracker {
    input: HeadInput,
    angles: [f32; 3],
    quaternion: [f32; 4],
}

impl Default for HeadTracker {
    fn default() -> Self {
        Self::new(HeadInput::default())
    }
}

impl HeadTracker {
    pub fn new(input: HeadInput) -> Self {
        Self {
            input,
            angles: [0.0; 3],
            quaternion: [1.0, 0.0, 0.0, 0.0],
        }
    }

    pub fn set_input(&mut self, input: HeadInput) {
        self.input = input;
    }

    /// Whether `addr` is one a head tracker sends (see the module docs).
    pub fn understands(addr: &str) -> bool {
        Self::part(addr).is_some()
    }

    /// The address without the SceneRotator prefix, if it is one of ours.
    fn part(addr: &str) -> Option<&str> {
        let a = addr.strip_prefix("/SceneRotator").unwrap_or(addr);
        let a = a.strip_prefix('/')?;
        matches!(
            a,
            "ypr"
                | "yaw"
                | "pitch"
                | "roll"
                | "quaternion"
                | "quaternions"
                | "qw"
                | "qx"
                | "qy"
                | "qz"
        )
        .then_some(a)
    }

    /// The orientation `m` gives (degrees, inverts applied), if it is a
    /// head tracker's message.
    pub fn read(&mut self, m: &Message) -> Option<HeadReading> {
        let part = Self::part(&m.addr)?;
        let numbers: Vec<f32> = m.args.iter().filter_map(Arg::number).collect();
        let first = numbers.first().copied().filter(|v| v.is_finite());
        let unit = if self.input.angles_in_radians {
            180.0 / std::f32::consts::PI
        } else {
            1.0
        };
        let sign = |invert: bool| if invert { -1.0 } else { 1.0 };
        let angles = match part {
            "ypr" => {
                let [y, p, r] = [0, 1, 2].map(|i| numbers.get(i).copied());
                let (Some(y), Some(p), Some(r)) = (y, p, r) else {
                    return None;
                };
                if !(y.is_finite() && p.is_finite() && r.is_finite()) {
                    return None;
                }
                self.angles = [y * unit, p * unit, r * unit];
                true
            }
            "yaw" => {
                self.angles[0] = first? * unit;
                true
            }
            "pitch" => {
                self.angles[1] = first? * unit;
                true
            }
            "roll" => {
                self.angles[2] = first? * unit;
                true
            }
            "quaternion" | "quaternions" => {
                if numbers.len() < 4 || numbers[..4].iter().any(|v| !v.is_finite()) {
                    return None;
                }
                self.quaternion.copy_from_slice(&numbers[..4]);
                false
            }
            _ => {
                let i = match part {
                    "qw" => 0,
                    "qx" => 1,
                    "qy" => 2,
                    _ => 3,
                };
                self.quaternion[i] = first?;
                false
            }
        };
        let i = self.input;
        Some(if angles {
            HeadReading::Angles {
                yaw: self.angles[0] * sign(i.invert_yaw),
                pitch: self.angles[1] * sign(i.invert_pitch),
                roll: self.angles[2] * sign(i.invert_roll),
            }
        } else {
            // Mirroring a rotation about an axis negates the other two
            // components; inverting yaw (about z) negates z, pitch (about
            // y) y, roll (about x) x.
            let [w, x, y, z] = self.quaternion;
            HeadReading::Quaternion {
                w,
                x: x * sign(i.invert_roll),
                y: y * sign(i.invert_pitch),
                z: z * sign(i.invert_yaw),
            }
        })
    }

    /// Every head reading in an OSC packet (bundles opened), in order.
    pub fn read_packet(&mut self, packet: &[u8]) -> Vec<HeadReading> {
        crate::osc::decode(packet)
            .iter()
            .filter_map(|m| self.read(m))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::osc::encode;

    fn msg(addr: &str, args: &[f32]) -> Message {
        Message::new(addr, args.iter().map(|v| Arg::Float(*v)).collect())
    }

    #[test]
    fn scene_rotator_and_sparta_angles_are_read() {
        let mut t = HeadTracker::default();
        for addr in ["/SceneRotator/ypr", "/ypr"] {
            assert_eq!(
                t.read(&msg(addr, &[30.0, -10.0, 5.0])),
                Some(HeadReading::Angles {
                    yaw: 30.0,
                    pitch: -10.0,
                    roll: 5.0
                })
            );
        }
        // One at a time: the others kept.
        assert_eq!(
            t.read(&msg("/SceneRotator/pitch", &[20.0])),
            Some(HeadReading::Angles {
                yaw: 30.0,
                pitch: 20.0,
                roll: 5.0
            })
        );
        assert_eq!(
            t.read(&msg("/yaw", &[-90.0])),
            Some(HeadReading::Angles {
                yaw: -90.0,
                pitch: 20.0,
                roll: 5.0
            })
        );
        // Integers count too.
        let ints = Message::new("/ypr", vec![Arg::Int(1), Arg::Int(2), Arg::Int(3)]);
        assert!(t.read(&ints).is_some());
    }

    #[test]
    fn quaternions_whole_or_by_component() {
        let mut t = HeadTracker::default();
        assert_eq!(
            t.read(&msg("/SceneRotator/quaternions", &[0.9, 0.1, 0.2, 0.3])),
            Some(HeadReading::Quaternion {
                w: 0.9,
                x: 0.1,
                y: 0.2,
                z: 0.3
            })
        );
        assert_eq!(
            t.read(&msg("/SceneRotator/qz", &[-0.3])),
            Some(HeadReading::Quaternion {
                w: 0.9,
                x: 0.1,
                y: 0.2,
                z: -0.3
            })
        );
        assert!(t.read(&msg("/quaternion", &[1.0, 0.0])).is_none());
    }

    #[test]
    fn inverted_axes_and_radians() {
        let mut t = HeadTracker::new(HeadInput {
            invert_yaw: true,
            invert_roll: true,
            angles_in_radians: true,
            ..HeadInput::default()
        });
        let Some(HeadReading::Angles { yaw, pitch, roll }) =
            t.read(&msg("/ypr", &[std::f32::consts::FRAC_PI_2, 0.5, 0.25]))
        else {
            panic!("no reading");
        };
        assert!((yaw + 90.0).abs() < 1e-4);
        assert!((pitch - 0.5f32.to_degrees()).abs() < 1e-4);
        assert!((roll + 0.25f32.to_degrees()).abs() < 1e-4);
        assert_eq!(
            t.read(&msg("/quaternion", &[0.5, 0.5, 0.5, 0.5])),
            Some(HeadReading::Quaternion {
                w: 0.5,
                x: -0.5,
                y: 0.5,
                z: -0.5
            })
        );
    }

    #[test]
    fn other_messages_and_broken_ones_are_left_alone() {
        let mut t = HeadTracker::default();
        assert!(t.read(&msg("/strip/1/fader", &[0.5])).is_none());
        assert!(t.read(&msg("/ypr", &[1.0, 2.0])).is_none());
        assert!(t.read(&msg("/ypr", &[f32::NAN, 2.0, 3.0])).is_none());
        assert!(t.read(&msg("/yaw", &[])).is_none());
        assert!(HeadTracker::understands("/SceneRotator/roll"));
        assert!(!HeadTracker::understands("/SceneRotator/other"));
        // A bundle: both, in order.
        let mut bundle = b"#bundle\0".to_vec();
        bundle.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 1]);
        for m in [msg("/yaw", &[10.0]), msg("/pitch", &[5.0])] {
            let p = encode(&m);
            bundle.extend_from_slice(&(p.len() as i32).to_be_bytes());
            bundle.extend_from_slice(&p);
        }
        let got = t.read_packet(&bundle);
        assert_eq!(got.len(), 2);
        assert_eq!(
            got[1],
            HeadReading::Angles {
                yaw: 10.0,
                pitch: 5.0,
                roll: 0.0
            }
        );
    }
}
