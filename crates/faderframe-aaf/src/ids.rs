//! The identifiers AAF names things by: AUIDs (16 bytes: SMPTE labels and
//! UUIDs) and MobIDs (32-byte SMPTE UMIDs).

use std::fmt;

/// A 16-byte identifier, held in the order its text form reads
/// (`0d010101-0101-0100-060e-2b3402060101`). AAF files store it with the
/// first three fields little-endian ([`Auid::to_le`]), like a Windows GUID.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct Auid(pub [u8; 16]);

/// An [`Auid`] from its text form as a number (the generated tables use it).
pub const fn id(v: u128) -> Auid {
    Auid(v.to_be_bytes())
}

impl Auid {
    pub const NIL: Auid = Auid([0; 16]);

    /// The bytes as stored in a file.
    pub fn to_le(self) -> [u8; 16] {
        let b = self.0;
        [
            b[3], b[2], b[1], b[0], b[5], b[4], b[7], b[6], b[8], b[9], b[10], b[11], b[12], b[13],
            b[14], b[15],
        ]
    }

    /// From the bytes as stored in a file.
    pub fn from_le(b: &[u8]) -> Option<Auid> {
        let b: &[u8; 16] = b.get(..16)?.try_into().ok()?;
        Some(Auid([
            b[3], b[2], b[1], b[0], b[5], b[4], b[7], b[6], b[8], b[9], b[10], b[11], b[12], b[13],
            b[14], b[15],
        ]))
    }

    pub fn is_nil(&self) -> bool {
        self.0 == [0; 16]
    }
}

impl fmt::Debug for Auid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl fmt::Display for Auid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let b = &self.0;
        for (i, x) in b.iter().enumerate() {
            if matches!(i, 4 | 6 | 8 | 10) {
                f.write_str("-")?;
            }
            write!(f, "{x:02x}")?;
        }
        Ok(())
    }
}

/// A mob's identity: a basic SMPTE UMID (label, length, instance number)
/// whose material number is an [`Auid`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct MobId(pub [u8; 32]);

/// The UMID label AAF applications use for material numbers made from
/// UUIDs (SMPTE 330M: material type not identified, UUID/UL method).
const UMID_LABEL: [u8; 12] = [
    0x06, 0x0a, 0x2b, 0x34, 0x01, 0x01, 0x01, 0x05, 0x01, 0x01, 0x0f, 0x20,
];

impl MobId {
    pub const NIL: MobId = MobId([0; 32]);

    /// A basic UMID around `material`.
    pub fn new(material: Auid) -> MobId {
        let mut b = [0u8; 32];
        b[..12].copy_from_slice(&UMID_LABEL);
        b[12] = 0x13;
        b[16..].copy_from_slice(&material.to_le());
        MobId(b)
    }

    /// The bytes as stored in a file (the material number little-endian,
    /// like an [`Auid`]).
    pub fn to_le(self) -> [u8; 32] {
        self.0
    }

    pub fn from_le(b: &[u8]) -> Option<MobId> {
        Some(MobId(b.get(..32)?.try_into().ok()?))
    }

    pub fn material(&self) -> Auid {
        Auid::from_le(&self.0[16..]).unwrap_or_default()
    }

    pub fn is_nil(&self) -> bool {
        self.0 == [0; 32]
    }
}

impl fmt::Debug for MobId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "urn:smpte:umid:")?;
        for (i, x) in self.0.iter().enumerate() {
            if i > 0 && i % 4 == 0 {
                f.write_str(".")?;
            }
            write!(f, "{x:02x}")?;
        }
        Ok(())
    }
}

/// Where new identifiers come from: random ones (each export its own), or
/// a sequence from a seed (tests whose files must come out the same).
#[derive(Clone, Debug)]
pub struct IdSource {
    state: u64,
    random: bool,
}

impl IdSource {
    /// Identifiers no other file has.
    pub fn random() -> IdSource {
        use std::hash::{BuildHasher, Hasher};
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos()),
        );
        h.write_u32(std::process::id());
        IdSource {
            state: h.finish(),
            random: true,
        }
    }

    /// The same identifiers every time for the same seed.
    pub fn seeded(seed: u64) -> IdSource {
        IdSource {
            state: seed,
            random: false,
        }
    }

    pub fn is_random(&self) -> bool {
        self.random
    }

    fn next_u64(&mut self) -> u64 {
        // splitmix64
        self.state = self.state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// A version 4 UUID.
    pub fn uuid(&mut self) -> Auid {
        let mut b = [0u8; 16];
        b[..8].copy_from_slice(&self.next_u64().to_be_bytes());
        b[8..].copy_from_slice(&self.next_u64().to_be_bytes());
        b[6] = (b[6] & 0x0f) | 0x40;
        b[8] = (b[8] & 0x3f) | 0x80;
        Auid(b)
    }

    pub fn mob_id(&mut self) -> MobId {
        let material = self.uuid();
        MobId::new(material)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auids_print_and_store_like_guids() {
        let a = id(0x0d010101_0101_0100_060e_2b3402060101);
        assert_eq!(a.to_string(), "0d010101-0101-0100-060e-2b3402060101");
        assert_eq!(
            a.to_le(),
            [
                0x01, 0x01, 0x01, 0x0d, 0x01, 0x01, 0x00, 0x01, 0x06, 0x0e, 0x2b, 0x34, 0x02, 0x06,
                0x01, 0x01
            ]
        );
        assert_eq!(Auid::from_le(&a.to_le()), Some(a));
    }

    #[test]
    fn seeded_ids_repeat_and_random_ones_differ() {
        let (mut a, mut b) = (IdSource::seeded(7), IdSource::seeded(7));
        assert_eq!(a.mob_id(), b.mob_id());
        let m = a.mob_id();
        assert_eq!(m.0[12], 0x13);
        assert_eq!(MobId::new(m.material()), m);
        assert_ne!(IdSource::random().uuid(), IdSource::random().uuid());
    }
}
