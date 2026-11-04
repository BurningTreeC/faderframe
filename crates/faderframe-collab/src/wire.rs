//! What host and peers say to each other.
//!
//! Messages are MessagePack (struct fields by name, so optional fields can
//! be added later; floats exact). What a step changes, a participant's
//! presence and project snapshots are bytes the session writes and reads
//! (`faderframe-session`'s `collab`): this crate orders and carries them
//! without knowing projects.

use serde::{Deserialize, Serialize};

/// The version of this protocol: a host turns away other versions.
pub const PROTOCOL: u32 = 1;

/// A participant, by the number the host gave it (the host is 0).
pub type PeerId = u32;

/// What a joining session says first.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Hello {
    pub protocol: u32,
    /// The program and its version, for the host's notices.
    pub program: String,
    /// The name others see.
    pub name: String,
    /// Back after losing the connection: who it was and what it has.
    #[serde(default)]
    pub resume: Option<Resume>,
}

/// A peer coming back.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Resume {
    pub peer: PeerId,
    /// The secret the host gave it when it first joined.
    pub token: u64,
    /// The last entry it applied.
    pub seq: u64,
}

/// Someone in the session.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Participant {
    pub peer: PeerId,
    pub name: String,
    /// 0xRRGGBB.
    pub colour: u32,
    pub host: bool,
    /// False while a peer that lost its connection may still come back.
    pub connected: bool,
    /// Hears the host's mix.
    #[serde(default)]
    pub listening: bool,
}

/// One entry of the host's ordered log.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    /// Its place (1, 2, 3 …).
    pub seq: u64,
    pub author: PeerId,
    /// The author's own number for what it proposed (0 for the host's own
    /// entries made without a proposal).
    pub local: u64,
    pub body: EntryBody,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum EntryBody {
    /// Edits: the commands as applied, in the session's wire form.
    Step {
        label: String,
        #[serde(with = "serde_bytes")]
        step: Vec<u8>,
    },
    /// Ids `[start, end)` set aside for `peer`'s new things.
    Grant { peer: PeerId, start: u64, end: u64 },
}

/// The host's answer to a Hello.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Welcome {
    pub peer: PeerId,
    /// For coming back after losing the connection.
    pub token: u64,
    pub colour: u32,
    /// The session's project as of entry `seq` (`None` when `entries`
    /// bring a peer that came back up to date).
    #[serde(with = "serde_bytes")]
    pub snapshot: Option<Vec<u8>>,
    pub seq: u64,
    /// Entries after the peer's own `seq` (coming back).
    pub entries: Vec<Entry>,
    /// The newest of the peer's proposals the host has answered (coming
    /// back: later ones it must send again).
    pub answered: u64,
    pub participants: Vec<Participant>,
    /// The project's sample rate.
    pub sample_rate: u32,
    /// Ids set aside for the new peer (`[start, end)` ranges).
    #[serde(default)]
    pub lease: Vec<(u64, u64)>,
}

/// A piece of a media file on its way.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum MediaPart {
    /// The file is `len` bytes; chunks follow from `from` on.
    Begin { key: String, len: u64, from: u64 },
    Chunk {
        key: String,
        offset: u64,
        #[serde(with = "serde_bytes")]
        data: Vec<u8>,
    },
    /// All sent; the SHA-256 of the whole file.
    End { key: String, sha256: [u8; 32] },
    /// The host has no such file.
    Missing { key: String },
}

/// Peer → host.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ToHost {
    Hello(Hello),
    /// Edits made here, to be put in order.
    Propose {
        local: u64,
        label: String,
        #[serde(with = "serde_bytes")]
        step: Vec<u8>,
    },
    /// Where this participant is and what it has selected.
    Presence(#[serde(with = "serde_bytes")] Vec<u8>),
    Chat(String),
    /// More ids, please.
    WantLease,
    /// A media file, from byte `from` on (a part kept from before).
    WantMedia {
        key: String,
        from: u64,
    },
    /// A piece of a file the host asked for ([`ToPeer::WantMedia`]).
    Media(MediaPart),
    /// Hear the host's mix, or stop.
    Listen(bool),
    /// The project here no longer matches: send it whole.
    Resync,
    Ping,
    Bye,
}

/// Host → peer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ToPeer {
    Welcome(Box<Welcome>),
    Entry(Entry),
    /// A proposal that could not be applied.
    Reject {
        local: u64,
        reason: String,
    },
    Participants(Vec<Participant>),
    Presence {
        peer: PeerId,
        #[serde(with = "serde_bytes")]
        data: Vec<u8>,
    },
    Chat {
        peer: PeerId,
        text: String,
        /// Milliseconds since the Unix epoch, by the host's clock.
        at: u64,
    },
    /// The project as of entry `seq` (an answer to Resync).
    Snapshot {
        seq: u64,
        #[serde(with = "serde_bytes")]
        project: Vec<u8>,
    },
    /// The SHA-256 of the project's canonical form after entry `seq`.
    Checksum {
        seq: u64,
        hash: [u8; 32],
    },
    Media(MediaPart),
    /// A file this peer brought in (an import, a recording), from byte
    /// `from` on.
    WantMedia {
        key: String,
        from: u64,
    },
    /// A packet of the host's mix (Opus).
    Audio {
        seq: u64,
        #[serde(with = "serde_bytes")]
        packet: Vec<u8>,
    },
    Ping,
    Bye {
        reason: String,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum WireError {
    #[error("could not write a message: {0}")]
    Encode(#[from] rmp_serde::encode::Error),
    #[error("could not read a message: {0}")]
    Decode(#[from] rmp_serde::decode::Error),
}

pub fn encode<T: Serialize>(m: &T) -> Result<Vec<u8>, WireError> {
    Ok(rmp_serde::to_vec_named(m)?)
}

pub fn decode<T: serde::de::DeserializeOwned>(b: &[u8]) -> Result<T, WireError> {
    Ok(rmp_serde::from_slice(b)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_read_back_as_written() {
        let msgs = [
            ToPeer::Entry(Entry {
                seq: 7,
                author: 2,
                local: 3,
                body: EntryBody::Step {
                    label: "Volume".into(),
                    step: vec![1, 2, 3],
                },
            }),
            ToPeer::Entry(Entry {
                seq: 8,
                author: 0,
                local: 0,
                body: EntryBody::Grant {
                    peer: 2,
                    start: 100,
                    end: 200,
                },
            }),
            ToPeer::Media(MediaPart::End {
                key: "4:ab".into(),
                sha256: [9; 32],
            }),
            ToPeer::Welcome(Box::new(Welcome {
                peer: 1,
                token: 42,
                colour: 0xff8800,
                snapshot: Some(vec![0; 10]),
                seq: 3,
                entries: Vec::new(),
                answered: 0,
                participants: Vec::new(),
                sample_rate: 48_000,
                lease: vec![(10, 20)],
            })),
        ];
        for m in msgs {
            let b = encode(&m).unwrap();
            assert_eq!(decode::<ToPeer>(&b).unwrap(), m);
        }
        let h = ToHost::Hello(Hello {
            protocol: PROTOCOL,
            program: "FaderFrame".into(),
            name: "Ana".into(),
            resume: None,
        });
        assert_eq!(decode::<ToHost>(&encode(&h).unwrap()).unwrap(), h);
    }
}
