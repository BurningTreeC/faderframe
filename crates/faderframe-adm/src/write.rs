//! Writing a [`Master`]'s metadata: the `axml` chunk (the ADM document)
//! and the `chna` chunk (which track carries which channel).

use crate::{
    BEDS, Block, INTERPOLATION, MAX_CHANNELS, MAX_OBJECTS, Master, PROGRAMME, Profile, time, units,
};
use std::fmt::Write;

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum AdmError {
    #[error("the bed must be one of the profile's (2.0, 3.0, 5.0, 5.1, 7.0, 7.1, 7.0.2, 7.1.2)")]
    Bed,
    #[error("{0} objects: the profile allows {MAX_OBJECTS}")]
    Objects(usize),
    #[error("{0} channels: the profile allows {MAX_CHANNELS}")]
    Channels(usize),
    #[error("{0} Hz: the profile allows 48 kHz and 96 kHz")]
    SampleRate(u32),
    #[error("nothing to write")]
    Empty,
    #[error("the object \"{0}\" has no metadata covering the master")]
    Blocks(String),
}

/// Whether `m` can be written to the profile.
pub fn validate(m: &Master) -> Result<(), AdmError> {
    if !m.bed.is_empty() && !BEDS.contains(&m.bed.as_slice()) {
        return Err(AdmError::Bed);
    }
    if m.objects.len() > MAX_OBJECTS {
        return Err(AdmError::Objects(m.objects.len()));
    }
    if m.channels() > MAX_CHANNELS {
        return Err(AdmError::Channels(m.channels()));
    }
    if !matches!(m.sample_rate, 48_000 | 96_000) {
        return Err(AdmError::SampleRate(m.sample_rate));
    }
    if m.channels() == 0 || m.frames == 0 {
        return Err(AdmError::Empty);
    }
    for o in &m.objects {
        let covers = o.blocks.first().is_some_and(|b| b.start == 0)
            && o.blocks
                .windows(2)
                .all(|w| w[0].start + w[0].length == w[1].start)
            && o.blocks
                .last()
                .is_some_and(|b| b.start + b.length == m.frames);
        if !covers {
            return Err(AdmError::Blocks(o.name.clone()));
        }
    }
    Ok(())
}

/// XML text with the five special characters escaped.
fn esc(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '&' => "&amp;".to_string(),
            '<' => "&lt;".to_string(),
            '>' => "&gt;".to_string(),
            '"' => "&quot;".to_string(),
            '\'' => "&apos;".to_string(),
            c if (c as u32) < 0x20 && !matches!(c, '\t' | '\n' | '\r') => " ".to_string(),
            c => c.to_string(),
        })
        .collect()
}

/// The IDs of channel `i` of the file (bed channels first).
struct Ids {
    /// `yyyyxxxx`: type (0001 DirectSpeakers, 0003 Objects) and number.
    channel: String,
    pack: String,
    uid: String,
}

fn ids(m: &Master, i: usize) -> Ids {
    let bed = i < m.bed.len();
    let ty = if bed { "0001" } else { "0003" };
    // One counter for channels, streams and track formats; packs count the
    // bed's (one) and then each object's.
    let pack = if bed {
        0x1001
    } else {
        0x1001 + usize::from(!m.bed.is_empty()) + (i - m.bed.len())
    };
    Ids {
        channel: format!("{ty}{:04X}", 0x1001 + i),
        pack: format!("AP_{ty}{pack:04X}"),
        uid: format!("ATU_{:08X}", i + 1),
    }
}

fn pos(out: &mut String, indent: &str, p: [f32; 3]) {
    for (c, v) in ['X', 'Y', 'Z'].iter().zip(p) {
        let _ = writeln!(
            out,
            "{indent}<position coordinate=\"{c}\">{v:.6}</position>"
        );
    }
}

/// The `axml` chunk: the ADM document of `m`.
pub fn axml(m: &Master) -> String {
    let rate = m.sample_rate;
    let end = time(units(m.frames, rate));
    let mut o = String::with_capacity(
        4096 + m
            .objects
            .iter()
            .map(|o| o.blocks.len() * 420)
            .sum::<usize>(),
    );
    o.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    o.push_str(
        "<ebuCoreMain xmlns:dc=\"http://purl.org/dc/elements/1.1/\" \
         xmlns=\"urn:ebu:metadata-schema:ebuCore_2014\" \
         xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" \
         xsi:schemaLocation=\"urn:ebu:metadata-schema:ebuCore_2014 \
         http://www.ebu.ch/metadata/schemas/EBUCore/20140318/EBU_CORE_20140318.xsd\" \
         xml:lang=\"en\">\n",
    );
    o.push_str("  <coreMetadata>\n    <format>\n      <audioFormatExtended>\n");
    let i5 = "          ";
    // The contents: the bed's, then one per object.
    let contents = usize::from(!m.bed.is_empty()) + m.objects.len();
    let _ = writeln!(
        o,
        "        <audioProgramme audioProgrammeID=\"APR_1001\" audioProgrammeName=\"{}\" start=\"00:00:00.00000\" end=\"{end}\">",
        match m.profile {
            Profile::DolbyAtmos => PROGRAMME.to_string(),
            Profile::Itu => esc(&m.name),
        }
    );
    for c in 0..contents {
        let _ = writeln!(
            o,
            "{i5}<audioContentIDRef>ACO_{:04X}</audioContentIDRef>",
            0x1001 + c
        );
    }
    o.push_str("        </audioProgramme>\n");
    // (content, object id, name, file channels)
    let mut groups: Vec<(String, String, std::ops::Range<usize>)> = Vec::new();
    if !m.bed.is_empty() {
        groups.push(("AO_1001".into(), "Bed".into(), 0..m.bed.len()));
    }
    for (k, ob) in m.objects.iter().enumerate() {
        let i = m.bed.len() + k;
        groups.push((format!("AO_{:04X}", 0x100B + k), ob.name.clone(), i..i + 1));
    }
    for (c, (object, name, _)) in groups.iter().enumerate() {
        let _ = writeln!(
            o,
            "        <audioContent audioContentID=\"ACO_{:04X}\" audioContentName=\"{}\">",
            0x1001 + c,
            esc(name)
        );
        let _ = writeln!(o, "{i5}<audioObjectIDRef>{object}</audioObjectIDRef>");
        let _ = writeln!(o, "{i5}<dialogue mixedContentKind=\"0\">2</dialogue>");
        o.push_str("        </audioContent>\n");
    }
    for (object, name, channels) in &groups {
        let _ = writeln!(
            o,
            "        <audioObject audioObjectID=\"{object}\" audioObjectName=\"{}\" start=\"00:00:00.00000\" duration=\"{end}\">",
            esc(name)
        );
        let _ = writeln!(
            o,
            "{i5}<audioPackFormatIDRef>{}</audioPackFormatIDRef>",
            ids(m, channels.start).pack
        );
        for i in channels.clone() {
            let _ = writeln!(
                o,
                "{i5}<audioTrackUIDRef>{}</audioTrackUIDRef>",
                ids(m, i).uid
            );
        }
        o.push_str("        </audioObject>\n");
    }
    // Packs.
    for (_, name, channels) in &groups {
        let bed = channels.start < m.bed.len();
        let (label, def) = if bed {
            ("0001", "DirectSpeakers")
        } else {
            ("0003", "Objects")
        };
        let _ = writeln!(
            o,
            "        <audioPackFormat audioPackFormatID=\"{}\" audioPackFormatName=\"{}\" typeLabel=\"{label}\" typeDefinition=\"{def}\">",
            ids(m, channels.start).pack,
            esc(name)
        );
        for i in channels.clone() {
            let _ = writeln!(
                o,
                "{i5}<audioChannelFormatIDRef>AC_{}</audioChannelFormatIDRef>",
                ids(m, i).channel
            );
        }
        o.push_str("        </audioPackFormat>\n");
    }
    // Channels: the bed's at their places, the objects' blocks.
    for (i, c) in m.bed.iter().enumerate() {
        let id = ids(m, i).channel;
        let _ = writeln!(
            o,
            "        <audioChannelFormat audioChannelFormatID=\"AC_{id}\" audioChannelFormatName=\"{}\" typeLabel=\"0001\" typeDefinition=\"DirectSpeakers\">",
            c.name()
        );
        if c.is_lfe() && m.profile == Profile::Itu {
            // Renderers that go by BS.2076 know an LFE by its frequency.
            let _ = writeln!(
                o,
                "{i5}<frequency typeDefinition=\"lowPass\">120</frequency>"
            );
        }
        let _ = writeln!(
            o,
            "{i5}<audioBlockFormat audioBlockFormatID=\"AB_{id}_00000001\">"
        );
        let i6 = "            ";
        let _ = writeln!(o, "{i6}<cartesian>1</cartesian>");
        // Dolby's labels, or BS.2051's for renderers that go by those.
        let label = match m.profile {
            Profile::DolbyAtmos => c.label(),
            Profile::Itu => c.itu(),
        };
        let _ = writeln!(o, "{i6}<speakerLabel>{label}</speakerLabel>");
        pos(&mut o, i6, c.position());
        let _ = writeln!(o, "{i5}</audioBlockFormat>");
        o.push_str("        </audioChannelFormat>\n");
    }
    for (k, ob) in m.objects.iter().enumerate() {
        let id = ids(m, m.bed.len() + k).channel;
        let _ = writeln!(
            o,
            "        <audioChannelFormat audioChannelFormatID=\"AC_{id}\" audioChannelFormatName=\"{}\" typeLabel=\"0003\" typeDefinition=\"Objects\">",
            esc(&ob.name)
        );
        for (j, b) in ob.blocks.iter().enumerate() {
            block(&mut o, &id, j, b, rate);
        }
        o.push_str("        </audioChannelFormat>\n");
    }
    // Streams, track formats and track UIDs.
    for i in 0..m.channels() {
        let id = ids(m, i);
        let ch = &id.channel;
        let name = if i < m.bed.len() {
            m.bed[i].name().to_string()
        } else {
            esc(&m.objects[i - m.bed.len()].name)
        };
        let _ = writeln!(
            o,
            "        <audioStreamFormat audioStreamFormatID=\"AS_{ch}\" audioStreamFormatName=\"PCM_{name}\" formatLabel=\"0001\" formatDefinition=\"PCM\">"
        );
        let _ = writeln!(
            o,
            "{i5}<audioChannelFormatIDRef>AC_{ch}</audioChannelFormatIDRef>"
        );
        // Dolby's profile wants the pack too (BS.2076 one or the other).
        if m.profile == Profile::DolbyAtmos {
            let _ = writeln!(
                o,
                "{i5}<audioPackFormatIDRef>{}</audioPackFormatIDRef>",
                id.pack
            );
        }
        let _ = writeln!(
            o,
            "{i5}<audioTrackFormatIDRef>AT_{ch}_01</audioTrackFormatIDRef>"
        );
        o.push_str("        </audioStreamFormat>\n");
        let _ = writeln!(
            o,
            "        <audioTrackFormat audioTrackFormatID=\"AT_{ch}_01\" audioTrackFormatName=\"PCM_{name}\" formatLabel=\"0001\" formatDefinition=\"PCM\">"
        );
        let _ = writeln!(
            o,
            "{i5}<audioStreamFormatIDRef>AS_{ch}</audioStreamFormatIDRef>"
        );
        o.push_str("        </audioTrackFormat>\n");
    }
    for i in 0..m.channels() {
        let id = ids(m, i);
        let _ = writeln!(
            o,
            "        <audioTrackUID UID=\"{}\" sampleRate=\"{rate}\" bitDepth=\"24\">",
            id.uid
        );
        let _ = writeln!(
            o,
            "{i5}<audioTrackFormatIDRef>AT_{}_01</audioTrackFormatIDRef>",
            id.channel
        );
        let _ = writeln!(
            o,
            "{i5}<audioPackFormatIDRef>{}</audioPackFormatIDRef>",
            id.pack
        );
        o.push_str("        </audioTrackUID>\n");
    }
    o.push_str("      </audioFormatExtended>\n    </format>\n  </coreMetadata>\n</ebuCoreMain>\n");
    o
}

fn block(o: &mut String, id: &str, j: usize, b: &Block, rate: u32) {
    let (a, z) = (units(b.start, rate), units(b.start + b.length, rate));
    let i5 = "          ";
    let i6 = "            ";
    let _ = writeln!(
        o,
        "{i5}<audioBlockFormat audioBlockFormatID=\"AB_{id}_{:08X}\" rtime=\"{}\" duration=\"{}\">",
        j + 1,
        time(a),
        time(z - a)
    );
    let _ = writeln!(o, "{i6}<cartesian>1</cartesian>");
    let p = [
        b.position[0].clamp(-1.0, 1.0),
        b.position[1].clamp(-1.0, 1.0),
        b.position[2].clamp(0.0, 1.0),
    ];
    pos(o, i6, p);
    let size = b.size.clamp(0.0, 1.0);
    for e in ["width", "depth", "height"] {
        let _ = writeln!(o, "{i6}<{e}>{size:.6}</{e}>");
    }
    let _ = writeln!(o, "{i6}<gain>{:.6}</gain>", b.gain.max(0.0));
    let length = if j == 0 {
        "0".to_string()
    } else {
        format!("{INTERPOLATION:.6}")
    };
    let _ = writeln!(
        o,
        "{i6}<jumpPosition interpolationLength=\"{length}\">1</jumpPosition>"
    );
    let _ = writeln!(o, "{i5}</audioBlockFormat>");
}

/// The `chna` chunk: for each file channel (1-based) its track UID, track
/// format and pack format.
pub fn chna(m: &Master) -> Vec<u8> {
    let n = m.channels() as u16;
    let mut out = Vec::with_capacity(4 + 40 * n as usize);
    out.extend_from_slice(&n.to_le_bytes());
    out.extend_from_slice(&n.to_le_bytes());
    for i in 0..m.channels() {
        let id = ids(m, i);
        out.extend_from_slice(&((i + 1) as u16).to_le_bytes());
        out.extend_from_slice(id.uid.as_bytes());
        out.extend_from_slice(format!("AT_{}_01", id.channel).as_bytes());
        out.extend_from_slice(id.pack.as_bytes());
        out.push(0);
    }
    out
}
