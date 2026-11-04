//! Plug-in states in the formats DAWproject keeps them in.
//!
//! * `.clap-preset` (Bitwig's, as DAWproject asks for CLAP): `clap`, the
//!   plug-in's id as a big-endian `u32` length and its bytes, then the
//!   state exactly as the plug-in's state extension saved it.
//! * `.vstpreset` (Steinberg's): `VST3`, version 1 (`i32` LE), the class
//!   id as 32 hex digits, the offset of the chunk list (`i64` LE); the
//!   component state (`Comp`) and controller state (`Cont`); the list =
//!   `List`, a count (`i32` LE) and per chunk its id, offset and size
//!   (`i64` LE each).
//!
//! VST3 class ids are written as Steinberg's `FUID::toString` writes them:
//! the four 32-bit words in order. DAWproject's `deviceID` is the same
//! digits as a UUID (8-4-4-4-12).

/// A `.clap-preset` of plug-in `id`'s `state`.
pub fn clap_preset(id: &str, state: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + id.len() + state.len());
    out.extend_from_slice(b"clap");
    out.extend_from_slice(&(id.len() as u32).to_be_bytes());
    out.extend_from_slice(id.as_bytes());
    out.extend_from_slice(state);
    out
}

/// The plug-in id and state of a `.clap-preset` (`None`: not one).
pub fn read_clap_preset(data: &[u8]) -> Option<(String, &[u8])> {
    if data.get(..4)? != b"clap" {
        return None;
    }
    let n = u32::from_be_bytes(data.get(4..8)?.try_into().ok()?) as usize;
    let id = std::str::from_utf8(data.get(8..8usize.checked_add(n)?)?).ok()?;
    Some((id.to_string(), data.get(8 + n..)?))
}

/// What a `.vstpreset` holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VstPreset {
    /// 32 hex digits (as written; upper case when written here).
    pub class: String,
    pub component: Vec<u8>,
    pub controller: Vec<u8>,
}

/// A `.vstpreset` for class `class` (32 hex digits).
pub fn vst_preset(class: &str, component: &[u8], controller: &[u8]) -> Vec<u8> {
    let mut class = class.to_ascii_uppercase().into_bytes();
    class.resize(32, b'0');
    let mut d = Vec::with_capacity(48 + component.len() + controller.len() + 48);
    d.extend_from_slice(b"VST3");
    d.extend_from_slice(&1i32.to_le_bytes());
    d.extend_from_slice(&class);
    d.extend_from_slice(&0i64.to_le_bytes());
    let mut chunks: Vec<(&[u8; 4], i64, usize)> = Vec::new();
    let at = d.len() as i64;
    d.extend_from_slice(component);
    chunks.push((b"Comp", at, component.len()));
    if !controller.is_empty() {
        let at = d.len() as i64;
        d.extend_from_slice(controller);
        chunks.push((b"Cont", at, controller.len()));
    }
    let list = d.len() as i64;
    d[40..48].copy_from_slice(&list.to_le_bytes());
    d.extend_from_slice(b"List");
    d.extend_from_slice(&(chunks.len() as i32).to_le_bytes());
    for (id, at, n) in chunks {
        d.extend_from_slice(id);
        d.extend_from_slice(&at.to_le_bytes());
        d.extend_from_slice(&(n as i64).to_le_bytes());
    }
    d
}

fn i64_at(d: &[u8], at: usize) -> Option<i64> {
    Some(i64::from_le_bytes(d.get(at..at + 8)?.try_into().ok()?))
}

/// Read a `.vstpreset` (`None`: not one, or its component chunk is
/// missing).
pub fn read_vst_preset(d: &[u8]) -> Option<VstPreset> {
    if d.get(..4)? != b"VST3" {
        return None;
    }
    let class = std::str::from_utf8(d.get(8..40)?).ok()?.to_string();
    let list = usize::try_from(i64_at(d, 40)?).ok()?;
    if d.get(list..list + 4)? != b"List" {
        return None;
    }
    let count = usize::try_from(i32::from_le_bytes(
        d.get(list + 4..list + 8)?.try_into().ok()?,
    ))
    .ok()?;
    let mut component = None;
    let mut controller = Vec::new();
    for k in 0..count.min(64) {
        let e = list + 8 + k * 20;
        let id = d.get(e..e + 4)?;
        let off = usize::try_from(i64_at(d, e + 4)?).ok()?;
        let size = usize::try_from(i64_at(d, e + 12)?).ok()?;
        let body = d.get(off..off.checked_add(size)?)?;
        match id {
            b"Comp" => component = Some(body.to_vec()),
            b"Cont" => controller = body.to_vec(),
            _ => {}
        }
    }
    Some(VstPreset {
        class,
        component: component?,
        controller,
    })
}

/// 32 hex digits as a UUID (8-4-4-4-12, upper case).
pub fn uuid_of_class(class: &str) -> Option<String> {
    let c = class.trim();
    if c.len() != 32 || !c.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return None;
    }
    let c = c.to_ascii_uppercase();
    Some(format!(
        "{}-{}-{}-{}-{}",
        &c[..8],
        &c[8..12],
        &c[12..16],
        &c[16..20],
        &c[20..]
    ))
}

/// A UUID (with or without dashes and braces) as 32 hex digits (upper
/// case).
pub fn class_of_uuid(uuid: &str) -> Option<String> {
    let c: String = uuid
        .chars()
        .filter(|ch| !matches!(ch, '-' | '{' | '}' | ' '))
        .collect();
    (c.len() == 32 && c.chars().all(|ch| ch.is_ascii_hexdigit())).then(|| c.to_ascii_uppercase())
}

/// A class id's bytes as COM lays out a GUID in memory (the first three
/// words little-endian) and back: VST3 on Windows keeps its ids that way.
pub fn swap_com_order(class: &str) -> Option<String> {
    let c = class_of_uuid(class)?;
    let b: Vec<&str> = (0..16).map(|i| &c[2 * i..2 * i + 2]).collect();
    let order = [3, 2, 1, 0, 5, 4, 7, 6, 8, 9, 10, 11, 12, 13, 14, 15];
    Some(order.iter().map(|&i| b[i]).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clap_presets_carry_the_id_and_the_state() {
        let p = clap_preset("org.surge-synth-team.surge-xt", b"\x00state\xff");
        assert_eq!(&p[..4], b"clap");
        assert_eq!(&p[4..8], &[0, 0, 0, 29]);
        let (id, state) = read_clap_preset(&p).unwrap();
        assert_eq!(id, "org.surge-synth-team.surge-xt");
        assert_eq!(state, b"\x00state\xff");
        assert!(read_clap_preset(b"clap\0\0\0\x09short").is_none());
        assert!(read_clap_preset(b"VST3").is_none());
    }

    #[test]
    fn vst_presets_hold_both_states() {
        let class = "d39d5b69d6af42fa1234567844695661";
        let p = vst_preset(class, b"component!", b"ctl");
        let back = read_vst_preset(&p).unwrap();
        assert_eq!(back.class, class.to_ascii_uppercase());
        assert_eq!(back.component, b"component!");
        assert_eq!(back.controller, b"ctl");
        let no_controller = read_vst_preset(&vst_preset(class, b"c", b"")).unwrap();
        assert!(no_controller.controller.is_empty());
        assert_eq!(
            uuid_of_class(class).as_deref(),
            Some("D39D5B69-D6AF-42FA-1234-567844695661")
        );
        assert_eq!(
            class_of_uuid("{d39d5b69-d6af-42fa-1234-567844695661}").as_deref(),
            Some("D39D5B69D6AF42FA1234567844695661")
        );
        let swapped = swap_com_order(class).unwrap();
        assert_eq!(swapped, "695B9DD3AFD6FA421234567844695661");
        assert_eq!(
            swap_com_order(&swapped).unwrap(),
            class.to_ascii_uppercase()
        );
    }
}
