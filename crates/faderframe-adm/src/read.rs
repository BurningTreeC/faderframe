//! Reading an ADM BWF file's `axml` and `chna` chunks into a [`Scene`]:
//! the bed's speakers and the objects with their blocks, each by the file
//! channel that carries it.
//!
//! Dolby Atmos masters read exactly (Cartesian places, the profile's
//! labels); other ADM files as far as they are beds and objects: BS.2051
//! speaker labels, the common definitions of 5.1, polar object positions
//! mapped into the room (azimuth through the speakers' angles — ±30° the
//! front corners, ±90° the sides, ±135° the rear corners — and elevation up
//! to 30° as the height). HOA, matrix and binaural channels are left out
//! with a note.

use crate::{BedChannel, seconds};
use std::collections::HashMap;

#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    #[error("the ADM document is not XML: {0}")]
    Xml(String),
    #[error("the chna chunk is malformed")]
    Chna,
}

/// A bed speaker of a file.
#[derive(Clone, Debug, PartialEq)]
pub struct SceneSpeaker {
    /// The file channel (0-based).
    pub track: usize,
    pub label: String,
    pub channel: Option<BedChannel>,
    pub position: [f32; 3],
}

/// One block of an object, in seconds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SceneBlock {
    pub start: f64,
    /// `None`: to the end.
    pub length: Option<f64>,
    pub position: [f32; 3],
    pub size: f32,
    pub gain: f32,
    /// The place jumps at the block's start (over `interpolation`
    /// seconds); otherwise it moves there over the whole block.
    pub jump: bool,
    pub interpolation: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SceneObject {
    pub track: usize,
    pub name: String,
    pub blocks: Vec<SceneBlock>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Scene {
    pub programme: String,
    pub bed: Vec<SceneSpeaker>,
    pub objects: Vec<SceneObject>,
    /// What was left out, for the user.
    pub notes: Vec<String>,
}

/// `chna` entries: (file channel 0-based, track UID, track format ref).
fn parse_chna(chna: &[u8]) -> Result<Vec<(usize, String, String)>, ReadError> {
    if chna.len() < 4 {
        return Err(ReadError::Chna);
    }
    let uids = u16::from_le_bytes([chna[2], chna[3]]) as usize;
    let mut out = Vec::new();
    for e in chna[4..].as_chunks::<40>().0.iter().take(uids) {
        let index = u16::from_le_bytes([e[0], e[1]]) as usize;
        if index == 0 {
            continue;
        }
        let text = |b: &[u8]| {
            String::from_utf8_lossy(b)
                .trim_end_matches('\0')
                .trim()
                .to_string()
        };
        out.push((index - 1, text(&e[2..14]), text(&e[14..28])));
    }
    Ok(out)
}

/// The room place of a polar position (degrees, azimuth positive to the
/// left).
fn polar(azimuth: f32, elevation: f32) -> [f32; 3] {
    // (azimuth, x, y) around the room, left half; the right mirrors it.
    const ANCHORS: [(f32, f32, f32); 5] = [
        (0.0, 0.0, 1.0),
        (30.0, -1.0, 1.0),
        (90.0, -1.0, 0.0),
        (135.0, -1.0, -1.0),
        (180.0, 0.0, -1.0),
    ];
    let mut az = azimuth % 360.0;
    if az > 180.0 {
        az -= 360.0;
    } else if az < -180.0 {
        az += 360.0;
    }
    let a = az.abs();
    let (mut x, mut y) = (0.0, -1.0);
    for w in ANCHORS.windows(2) {
        let ((a0, x0, y0), (a1, x1, y1)) = (w[0], w[1]);
        if a >= a0 && a <= a1 {
            let t = (a - a0) / (a1 - a0);
            x = x0 + (x1 - x0) * t;
            y = y0 + (y1 - y0) * t;
            break;
        }
    }
    if az < 0.0 {
        x = -x;
    }
    [x, y, (elevation / 30.0).clamp(0.0, 1.0)]
}

/// The first six common definitions (5.1), by channel format ID.
fn common(id: &str) -> Option<BedChannel> {
    Some(match id.to_ascii_uppercase().as_str() {
        "AC_00010001" => BedChannel::L,
        "AC_00010002" => BedChannel::R,
        "AC_00010003" => BedChannel::C,
        "AC_00010004" => BedChannel::Lfe,
        "AC_00010005" => BedChannel::Ls,
        "AC_00010006" => BedChannel::Rs,
        _ => return None,
    })
}

#[derive(Default)]
struct Channel<'a> {
    name: String,
    kind: String,
    blocks: Vec<roxmltree::Node<'a, 'a>>,
}

fn text<'a>(n: roxmltree::Node<'a, 'a>, name: &str) -> Option<&'a str> {
    n.children()
        .find(|c| c.tag_name().name() == name)
        .and_then(|c| c.text())
}

fn number(n: roxmltree::Node<'_, '_>, name: &str) -> Option<f32> {
    text(n, name)?.trim().parse().ok()
}

/// A block's place (Cartesian, or polar mapped into the room).
fn place(block: roxmltree::Node<'_, '_>) -> [f32; 3] {
    let mut c: HashMap<String, f32> = HashMap::new();
    for p in block
        .children()
        .filter(|c| c.tag_name().name() == "position")
    {
        if p.attribute("bound").is_some() {
            continue;
        }
        if let (Some(k), Some(v)) = (
            p.attribute("coordinate"),
            p.text().and_then(|t| t.trim().parse().ok()),
        ) {
            c.insert(k.to_string(), v);
        }
    }
    let cartesian = text(block, "cartesian").map(str::trim) == Some("1")
        || (c.contains_key("X") && !c.contains_key("azimuth"));
    if cartesian {
        [
            c.get("X").copied().unwrap_or(0.0),
            c.get("Y").copied().unwrap_or(0.0),
            c.get("Z").copied().unwrap_or(0.0),
        ]
    } else {
        polar(
            c.get("azimuth").copied().unwrap_or(0.0),
            c.get("elevation").copied().unwrap_or(0.0),
        )
    }
}

/// Read a file's ADM document and `chna` chunk.
pub fn parse(axml: &str, chna: &[u8]) -> Result<Scene, ReadError> {
    let doc = roxmltree::Document::parse(axml).map_err(|e| ReadError::Xml(e.to_string()))?;
    let entries = parse_chna(chna)?;
    let by = |name: &'static str| {
        doc.descendants()
            .filter(move |n| n.is_element() && n.tag_name().name() == name)
    };
    let mut scene = Scene {
        programme: by("audioProgramme")
            .next()
            .and_then(|p| p.attribute("audioProgrammeName"))
            .unwrap_or_default()
            .to_string(),
        ..Scene::default()
    };
    let mut channels: HashMap<String, Channel<'_>> = HashMap::new();
    for c in by("audioChannelFormat") {
        let Some(id) = c.attribute("audioChannelFormatID") else {
            continue;
        };
        let kind = c
            .attribute("typeDefinition")
            .map(str::to_string)
            .or_else(|| {
                c.attribute("typeLabel").map(|l| match l {
                    "0001" => "DirectSpeakers".into(),
                    "0003" => "Objects".into(),
                    other => other.to_string(),
                })
            })
            .unwrap_or_default();
        channels.insert(
            id.to_ascii_uppercase(),
            Channel {
                name: c
                    .attribute("audioChannelFormatName")
                    .unwrap_or(id)
                    .to_string(),
                kind,
                blocks: c
                    .children()
                    .filter(|b| b.tag_name().name() == "audioBlockFormat")
                    .collect(),
            },
        );
    }
    let refs = |kind: &'static str, attr: &str, child: &str| -> HashMap<String, String> {
        by(kind)
            .filter_map(|n| {
                Some((
                    n.attribute(attr)?.to_ascii_uppercase(),
                    text(n, child)?.trim().to_ascii_uppercase(),
                ))
            })
            .collect()
    };
    let track_stream = refs(
        "audioTrackFormat",
        "audioTrackFormatID",
        "audioStreamFormatIDRef",
    );
    let stream_channel = refs(
        "audioStreamFormat",
        "audioStreamFormatID",
        "audioChannelFormatIDRef",
    );
    let uid_track = refs("audioTrackUID", "UID", "audioTrackFormatIDRef");
    let uid_channel = refs("audioTrackUID", "UID", "audioChannelFormatIDRef");
    // Objects name their tracks.
    let mut uid_object: HashMap<String, String> = HashMap::new();
    for o in by("audioObject") {
        let name = o.attribute("audioObjectName").unwrap_or_default();
        for r in o
            .children()
            .filter(|c| c.tag_name().name() == "audioTrackUIDRef")
        {
            if let Some(t) = r.text() {
                uid_object.insert(t.trim().to_ascii_uppercase(), name.to_string());
            }
        }
    }
    for (track, uid, track_ref) in entries {
        let uid = uid.to_ascii_uppercase();
        let track_format = uid_track
            .get(&uid)
            .cloned()
            .unwrap_or(track_ref.to_ascii_uppercase());
        let channel_id = uid_channel
            .get(&uid)
            .cloned()
            .or_else(|| {
                track_stream
                    .get(&track_format)
                    .and_then(|s| stream_channel.get(s))
                    .cloned()
            })
            // By convention AT_yyyyxxxx_zz carries AC_yyyyxxxx.
            .or_else(|| {
                track_format
                    .strip_prefix("AT_")
                    .and_then(|r| r.get(..8))
                    .map(|yx| format!("AC_{yx}"))
            });
        let Some(channel_id) = channel_id else {
            scene
                .notes
                .push(format!("track {}: no channel format", track + 1));
            continue;
        };
        let Some(ch) = channels.get(&channel_id) else {
            match common(&channel_id) {
                Some(c) => scene.bed.push(SceneSpeaker {
                    track,
                    label: c.itu().into(),
                    channel: Some(c),
                    position: c.position(),
                }),
                None => scene.notes.push(format!(
                    "track {}: {channel_id} is not described",
                    track + 1
                )),
            }
            continue;
        };
        match ch.kind.as_str() {
            "DirectSpeakers" => {
                let block = ch.blocks.first().copied();
                let label = block
                    .and_then(|b| text(b, "speakerLabel"))
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                let channel = BedChannel::from_label(&label).or_else(|| common(&channel_id));
                let position = match (block, channel) {
                    (Some(b), _) if b.children().any(|c| c.tag_name().name() == "position") => {
                        place(b)
                    }
                    (_, Some(c)) => c.position(),
                    _ => [0.0, 1.0, 0.0],
                };
                scene.bed.push(SceneSpeaker {
                    track,
                    label,
                    channel,
                    position,
                });
            }
            "Objects" => {
                let mut blocks = Vec::new();
                for b in &ch.blocks {
                    let cartesian = text(*b, "cartesian").map(str::trim) == Some("1");
                    let extent = ["width", "depth", "height"]
                        .iter()
                        .filter_map(|e| number(*b, e))
                        .fold(0.0f32, f32::max);
                    let size = if cartesian { extent } else { extent / 180.0 };
                    let gain = b
                        .children()
                        .find(|c| c.tag_name().name() == "gain")
                        .and_then(|g| {
                            let v: f32 = g.text()?.trim().parse().ok()?;
                            Some(if g.attribute("gainUnit") == Some("dB") {
                                10f32.powf(v / 20.0)
                            } else {
                                v
                            })
                        })
                        .unwrap_or(1.0);
                    let jump = b.children().find(|c| c.tag_name().name() == "jumpPosition");
                    blocks.push(SceneBlock {
                        start: b.attribute("rtime").and_then(seconds).unwrap_or(0.0),
                        length: b.attribute("duration").and_then(seconds),
                        position: place(*b),
                        size: size.clamp(0.0, 1.0),
                        gain,
                        jump: jump.and_then(|j| j.text()).map(str::trim) == Some("1"),
                        interpolation: jump
                            .and_then(|j| j.attribute("interpolationLength"))
                            .and_then(|v| v.parse().ok())
                            .unwrap_or(0.0),
                    });
                }
                scene.objects.push(SceneObject {
                    track,
                    name: uid_object
                        .get(&uid)
                        .cloned()
                        .unwrap_or_else(|| ch.name.clone()),
                    blocks,
                });
            }
            other => scene.notes.push(format!(
                "track {}: {} channels are not supported",
                track + 1,
                if other.is_empty() { "untyped" } else { other }
            )),
        }
    }
    scene.bed.sort_by_key(|s| s.track);
    scene.objects.sort_by_key(|o| o.track);
    Ok(scene)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BEDS, Block, Master, Object, axml, chna, validate};

    fn master() -> Master {
        let block = |start, length, x, y, z| Block {
            start,
            length,
            position: [x, y, z],
            size: 0.25,
            gain: 1.0,
        };
        Master {
            name: "Song".into(),
            profile: crate::Profile::DolbyAtmos,
            sample_rate: 48_000,
            frames: 96_000,
            bed: BEDS[7].to_vec(),
            objects: vec![
                Object {
                    name: "Lead & <Vox>".into(),
                    blocks: vec![
                        block(0, 48_000, -1.0, 1.0, 0.0),
                        block(48_000, 48_000, 0.5, -0.5, 1.0),
                    ],
                },
                Object {
                    name: "Pad".into(),
                    blocks: vec![block(0, 96_000, 0.0, 0.0, 0.5)],
                },
            ],
        }
    }

    #[test]
    fn a_master_reads_back() {
        let m = master();
        validate(&m).unwrap();
        let scene = parse(&axml(&m), &chna(&m)).unwrap();
        assert_eq!(scene.programme, crate::PROGRAMME);
        assert!(scene.notes.is_empty(), "{:?}", scene.notes);
        let bed: Vec<_> = scene.bed.iter().map(|s| s.channel.unwrap()).collect();
        assert_eq!(bed, BEDS[7]);
        assert_eq!(scene.bed[8].position, [-1.0, 0.0, 1.0]);
        assert_eq!(scene.objects.len(), 2);
        let o = &scene.objects[0];
        assert_eq!((o.track, o.name.as_str()), (10, "Lead & <Vox>"));
        assert_eq!(o.blocks.len(), 2);
        assert_eq!(o.blocks[1].start, 1.0);
        assert_eq!(o.blocks[1].length, Some(1.0));
        assert_eq!(o.blocks[1].position, [0.5, -0.5, 1.0]);
        assert!(o.blocks[1].jump && (o.blocks[1].interpolation - 0.005208).abs() < 1e-9);
        assert_eq!(o.blocks[0].interpolation, 0.0);
        assert!((o.blocks[0].size - 0.25).abs() < 1e-6);
        assert_eq!(scene.objects[1].track, 11);
    }

    #[test]
    fn the_profile_is_kept() {
        let mut m = master();
        m.sample_rate = 44_100;
        assert!(validate(&m).is_err());
        m.sample_rate = 96_000;
        m.bed = vec![BedChannel::L, BedChannel::C];
        assert_eq!(validate(&m), Err(crate::AdmError::Bed));
        m.bed.clear();
        m.objects[1].blocks[0].length = 10;
        assert!(matches!(validate(&m), Err(crate::AdmError::Blocks(_))));
        let x = axml(&master());
        // IDs as the profile wants them.
        for id in [
            "APR_1001", "ACO_1001", "ACO_1003", "AO_1001", "AO_100B", "AO_100C",
        ] {
            assert!(x.contains(id), "{id}");
        }
        assert!(x.contains("audioChannelFormatID=\"AC_0003100B\""));
        assert!(x.contains("<jumpPosition interpolationLength=\"0.005208\">1</jumpPosition>"));
        assert!(!x.contains("AO_1002"));
        assert!(x.contains("audioProgrammeName=\"Atmos_Master\""));
        assert!(!x.contains("<frequency"));
        // The ITU flavour: named after the master, the LFE by its low-pass,
        // streams naming only their channel; it reads back the same.
        let mut itu = master();
        itu.profile = crate::Profile::Itu;
        let x = axml(&itu);
        assert!(x.contains("audioProgrammeName=\"Song\""));
        assert!(x.contains("<frequency typeDefinition=\"lowPass\">120</frequency>"));
        let stream = x.split("<audioStreamFormat ").nth(1).unwrap();
        assert!(
            !stream
                .split("</audioStreamFormat>")
                .next()
                .unwrap()
                .contains("audioPackFormatIDRef")
        );
        let scene = parse(&x, &chna(&itu)).unwrap();
        assert_eq!(scene.objects.len(), 2);
        assert_eq!(scene.bed[3].channel, Some(BedChannel::Lfe));
    }

    #[test]
    fn polar_places_map_into_the_room() {
        assert_eq!(polar(30.0, 0.0), [-1.0, 1.0, 0.0]);
        assert_eq!(polar(-90.0, 30.0), [1.0, 0.0, 1.0]);
        assert_eq!(polar(180.0, 0.0), [0.0, -1.0, 0.0]);
        let p = polar(110.0, 15.0);
        assert!(p[0] == -1.0 && p[1] < 0.0 && p[1] > -1.0 && (p[2] - 0.5).abs() < 1e-6);
    }
}
