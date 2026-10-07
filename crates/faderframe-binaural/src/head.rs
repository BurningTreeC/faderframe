//! Heads: whose ears the mix is heard through.

use crate::{BinauralError, DIRECTIONS, Hrirs, Pair, resample};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// A head baked into FaderFrame (SADIE II, University of York).
pub struct BuiltinHead {
    /// Its stable id (preferences, scripts).
    pub id: &'static str,
    pub name: &'static str,
    data: &'static [u8],
}

/// The built-in heads: the KU100 (the default) and KEMAR dummy heads, then
/// SADIE II's eighteen listeners — to choose by ear the one that places
/// sounds best for you.
pub static BUILTIN_HEADS: &[BuiltinHead] = &[
    BuiltinHead {
        id: "ku100",
        name: "KU100 dummy head",
        data: include_bytes!("../data/sadie-d1.ffhr"),
    },
    BuiltinHead {
        id: "kemar",
        name: "KEMAR dummy head",
        data: include_bytes!("../data/sadie-d2.ffhr"),
    },
    BuiltinHead {
        id: "sadie-h3",
        name: "Listener H3",
        data: include_bytes!("../data/sadie-h3.ffhr"),
    },
    BuiltinHead {
        id: "sadie-h4",
        name: "Listener H4",
        data: include_bytes!("../data/sadie-h4.ffhr"),
    },
    BuiltinHead {
        id: "sadie-h5",
        name: "Listener H5",
        data: include_bytes!("../data/sadie-h5.ffhr"),
    },
    BuiltinHead {
        id: "sadie-h6",
        name: "Listener H6",
        data: include_bytes!("../data/sadie-h6.ffhr"),
    },
    BuiltinHead {
        id: "sadie-h7",
        name: "Listener H7",
        data: include_bytes!("../data/sadie-h7.ffhr"),
    },
    BuiltinHead {
        id: "sadie-h8",
        name: "Listener H8",
        data: include_bytes!("../data/sadie-h8.ffhr"),
    },
    BuiltinHead {
        id: "sadie-h9",
        name: "Listener H9",
        data: include_bytes!("../data/sadie-h9.ffhr"),
    },
    BuiltinHead {
        id: "sadie-h10",
        name: "Listener H10",
        data: include_bytes!("../data/sadie-h10.ffhr"),
    },
    BuiltinHead {
        id: "sadie-h11",
        name: "Listener H11",
        data: include_bytes!("../data/sadie-h11.ffhr"),
    },
    BuiltinHead {
        id: "sadie-h12",
        name: "Listener H12",
        data: include_bytes!("../data/sadie-h12.ffhr"),
    },
    BuiltinHead {
        id: "sadie-h13",
        name: "Listener H13",
        data: include_bytes!("../data/sadie-h13.ffhr"),
    },
    BuiltinHead {
        id: "sadie-h14",
        name: "Listener H14",
        data: include_bytes!("../data/sadie-h14.ffhr"),
    },
    BuiltinHead {
        id: "sadie-h15",
        name: "Listener H15",
        data: include_bytes!("../data/sadie-h15.ffhr"),
    },
    BuiltinHead {
        id: "sadie-h16",
        name: "Listener H16",
        data: include_bytes!("../data/sadie-h16.ffhr"),
    },
    BuiltinHead {
        id: "sadie-h17",
        name: "Listener H17",
        data: include_bytes!("../data/sadie-h17.ffhr"),
    },
    BuiltinHead {
        id: "sadie-h18",
        name: "Listener H18",
        data: include_bytes!("../data/sadie-h18.ffhr"),
    },
    BuiltinHead {
        id: "sadie-h19",
        name: "Listener H19",
        data: include_bytes!("../data/sadie-h19.ffhr"),
    },
    BuiltinHead {
        id: "sadie-h20",
        name: "Listener H20",
        data: include_bytes!("../data/sadie-h20.ffhr"),
    },
];

/// Whose ears: the impulse responses of the fifteen directions at one or
/// more sample rates. Cheap to clone; two heads are equal when they are
/// the same load.
#[derive(Clone)]
pub struct Head(Arc<HeadData>);

struct HeadData {
    id: String,
    name: String,
    sets: Vec<Hrirs>,
    /// What to know about it (a SOFA file's directions far from a speaker).
    note: Option<String>,
    serial: u64,
}

static SERIAL: AtomicU64 = AtomicU64::new(1);

impl Head {
    /// A head of [`BUILTIN_HEADS`].
    pub fn builtin(id: &str) -> Result<Head, BinauralError> {
        let b = BUILTIN_HEADS
            .iter()
            .find(|h| h.id == id)
            .ok_or_else(|| BinauralError::NoHead(id.to_string()))?;
        Ok(Head::new(b.id.into(), b.name.into(), parse(b.data)?, None))
    }

    pub(crate) fn new(id: String, name: String, sets: Vec<Hrirs>, note: Option<String>) -> Head {
        Head(Arc::new(HeadData {
            id,
            name,
            sets,
            note,
            serial: SERIAL.fetch_add(1, Ordering::Relaxed),
        }))
    }

    /// A head from a SOFA file (AES69: SimpleFreeFieldHRIR or GeneralFIR):
    /// for each speaker the nearest measured direction.
    pub fn from_sofa(path: &std::path::Path) -> Result<Head, BinauralError> {
        crate::sofa::read(path)
    }

    /// `ku100`, `kemar`, `sadie-h3` … or `sofa:<path>`.
    pub fn id(&self) -> &str {
        &self.0.id
    }

    pub fn name(&self) -> &str {
        &self.0.name
    }

    pub fn note(&self) -> Option<&str> {
        self.0.note.as_deref()
    }

    /// Different for every load (graph node keys).
    pub fn serial(&self) -> u64 {
        self.0.serial
    }

    /// The rates it was measured or baked at.
    pub fn rates(&self) -> Vec<u32> {
        self.0.sets.iter().map(|s| s.rate).collect()
    }

    /// Its directions at `rate`: as stored, or resampled from the nearest
    /// rate (88.2/176.4 kHz from 96 kHz where it has one).
    pub fn at(&self, rate: u32) -> Result<Hrirs, BinauralError> {
        let sets = &self.0.sets;
        if let Some(s) = sets.iter().find(|s| s.rate == rate) {
            return Ok(s.clone());
        }
        let source = sets
            .iter()
            .filter(|s| s.rate >= rate.min(96_000))
            .min_by_key(|s| s.rate)
            .or_else(|| sets.iter().max_by_key(|s| s.rate))
            .ok_or(BinauralError::Data)?;
        let ratio = f64::from(rate) / f64::from(source.rate);
        let pairs: Vec<Pair> = source
            .pairs
            .iter()
            .map(|p| Pair {
                left: resample(&p.left, ratio),
                right: resample(&p.right, ratio),
            })
            .collect();
        let taps = pairs.first().map_or(0, |p| p.left.len());
        Ok(Hrirs { rate, taps, pairs })
    }
}

impl Default for Head {
    /// The KU100 (an empty head, rendering nothing, if its data were
    /// damaged — the tests make sure it is not).
    fn default() -> Self {
        Head::builtin("ku100")
            .unwrap_or_else(|_| Head::new("ku100".into(), "KU100".into(), Vec::new(), None))
    }
}

impl PartialEq for Head {
    fn eq(&self, other: &Self) -> bool {
        self.0.serial == other.0.serial
    }
}

impl std::fmt::Debug for Head {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Head({})", self.0.id)
    }
}

fn read_u32(b: &[u8], at: &mut usize) -> Option<u32> {
    let v = u32::from_le_bytes(b.get(*at..*at + 4)?.try_into().ok()?);
    *at += 4;
    Some(v)
}

fn read_f32s(b: &[u8], at: &mut usize, n: usize) -> Option<Vec<f32>> {
    let bytes = b.get(*at..*at + 4 * n)?;
    *at += 4 * n;
    Some(
        bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| f32::from_le_bytes(*c))
            .collect(),
    )
}

/// A baked file ("FFHR" v1, `scripts/binaural_hrirs.py`).
fn parse(b: &[u8]) -> Result<Vec<Hrirs>, BinauralError> {
    if b.get(..4) != Some(b"FFHR") {
        return Err(BinauralError::Data);
    }
    let mut at = 4;
    let _version = read_u32(b, &mut at).ok_or(BinauralError::Data)?;
    let sets = read_u32(b, &mut at).ok_or(BinauralError::Data)?;
    let mut out = Vec::new();
    for _ in 0..sets {
        let rate = read_u32(b, &mut at).ok_or(BinauralError::Data)?;
        let taps = read_u32(b, &mut at).ok_or(BinauralError::Data)? as usize;
        let n = read_u32(b, &mut at).ok_or(BinauralError::Data)? as usize;
        if n != DIRECTIONS.len() {
            return Err(BinauralError::Data);
        }
        let mut pairs = Vec::with_capacity(n);
        for _ in 0..n {
            let _dir = read_f32s(b, &mut at, 2).ok_or(BinauralError::Data)?;
            let left = read_f32s(b, &mut at, taps).ok_or(BinauralError::Data)?;
            let right = read_f32s(b, &mut at, taps).ok_or(BinauralError::Data)?;
            pairs.push(Pair { left, right });
        }
        out.push(Hrirs { rate, taps, pairs });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_builtin_head_holds_every_direction_at_three_rates() {
        assert_eq!(BUILTIN_HEADS.len(), 20);
        for b in BUILTIN_HEADS {
            let h = Head::builtin(b.id).unwrap();
            assert_eq!(h.rates(), [44_100, 48_000, 96_000], "{}", b.id);
            for rate in h.rates() {
                let s = h.at(rate).unwrap();
                assert_eq!(s.pairs.len(), DIRECTIONS.len());
                // Level-matched: the front centre at unity at 1 kHz.
                let m = 0.5
                    * (crate::magnitude_at(&s.pairs[0].left, 1000.0, f64::from(rate))
                        + crate::magnitude_at(&s.pairs[0].right, 1000.0, f64::from(rate)));
                assert!((m - 1.0).abs() < 0.02, "{} at {rate}: {m}", b.id);
            }
        }
        // 88.2 kHz from 96 kHz.
        let r = Head::default().at(88_200).unwrap();
        assert_eq!(r.rate, 88_200);
        assert!((r.taps as f64 - 384.0 * 88.2 / 96.0).abs() < 2.0);
        assert!(Head::builtin("nobody").is_err());
        assert_ne!(Head::default(), Head::default(), "every load is its own");
    }
}
