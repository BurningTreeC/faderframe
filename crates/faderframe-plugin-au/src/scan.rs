//! Listing the installed Audio Units. The component registry is read
//! without instantiating anything, so this runs in-process (fast, and no
//! third-party code is loaded).

use crate::ffi::*;
use faderframe_plugin_host::scan::ScannedPlugin;
use std::path::PathBuf;

/// The kinds of units the host offers.
const TYPES: [OSType; 3] = [
    kAudioUnitType_Effect,
    kAudioUnitType_MusicEffect,
    kAudioUnitType_MusicDevice,
];

/// One four-character code as text: the characters when printable (and
/// not the separator), else `0x` and eight hex digits.
fn code_text(c: OSType) -> String {
    let b = c.to_be_bytes();
    if b.iter().all(|&x| (0x20..0x7f).contains(&x) && x != b':') {
        String::from_utf8_lossy(&b).into_owned()
    } else {
        format!("0x{c:08x}")
    }
}

fn code_parse(s: &str) -> Option<OSType> {
    if let Some(hex) = s.strip_prefix("0x")
        && hex.len() == 8
    {
        return u32::from_str_radix(hex, 16).ok();
    }
    let b: [u8; 4] = s.as_bytes().try_into().ok()?;
    Some(u32::from_be_bytes(b))
}

/// The plugin id of a component: `type:subtype:manufacturer`, e.g.
/// `aufx:dely:appl` for Apple's AUDelay.
pub fn id_of(d: &AudioComponentDescription) -> String {
    format!(
        "{}:{}:{}",
        code_text(d.componentType),
        code_text(d.componentSubType),
        code_text(d.componentManufacturer)
    )
}

/// The component description an id names.
pub fn parse_id(id: &str) -> Option<AudioComponentDescription> {
    let mut parts = id.split(':');
    let d = AudioComponentDescription {
        componentType: code_parse(parts.next()?)?,
        componentSubType: code_parse(parts.next()?)?,
        componentManufacturer: code_parse(parts.next()?)?,
        componentFlags: 0,
        componentFlagsMask: 0,
    };
    parts.next().is_none().then_some(d)
}

/// The component with exactly this description.
pub(crate) fn find(d: &AudioComponentDescription) -> Option<AudioComponent> {
    // SAFETY: plain registry query.
    let c = unsafe { AudioComponentFindNext(std::ptr::null_mut(), d) };
    (!c.is_null()).then_some(c)
}

fn version_text(v: u32) -> String {
    format!("{}.{}.{}", v >> 16, (v >> 8) & 0xff, v & 0xff)
}

/// Every effect and instrument unit installed.
pub fn scan() -> Vec<ScannedPlugin> {
    let mut out = Vec::new();
    for ty in TYPES {
        let wanted = AudioComponentDescription {
            componentType: ty,
            ..Default::default()
        };
        let mut c: AudioComponent = std::ptr::null_mut();
        loop {
            // SAFETY: walks the registry; `c` is the previous result.
            c = unsafe { AudioComponentFindNext(c, &wanted) };
            if c.is_null() {
                break;
            }
            let mut d = AudioComponentDescription::default();
            let mut name: CFStringRef = std::ptr::null();
            let mut version = 0u32;
            // SAFETY: `c` is a registered component; outputs are written.
            unsafe {
                if AudioComponentGetDescription(c, &mut d) != 0 {
                    continue;
                }
                if AudioComponentCopyName(c, &mut name) != 0 {
                    name = std::ptr::null();
                }
                AudioComponentGetVersion(c, &mut version);
            }
            let full = cf_string(name);
            release(name);
            // "Manufacturer: Name" by convention.
            let (vendor, plugin) = match full.split_once(": ") {
                Some((v, n)) => (v.trim().to_string(), n.trim().to_string()),
                None => (code_text(d.componentManufacturer), full.clone()),
            };
            let instrument = ty == kAudioUnitType_MusicDevice;
            let mut features = vec![if instrument {
                "instrument".to_string()
            } else {
                "audio-effect".to_string()
            }];
            if ty == kAudioUnitType_MusicEffect {
                features.push("note-effect".into());
            }
            out.push(ScannedPlugin {
                id: id_of(&d),
                name: if plugin.is_empty() { id_of(&d) } else { plugin },
                vendor,
                version: version_text(version),
                features,
                bundle: PathBuf::new(),
                audio_inputs: if instrument { Vec::new() } else { vec![2] },
                audio_outputs: vec![2],
                note_inputs: u16::from(ty != kAudioUnitType_Effect),
                note_outputs: 0,
            });
        }
    }
    out.sort_by(|a, b| (&a.vendor, &a.name).cmp(&(&b.vendor, &b.name)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip() {
        let d = AudioComponentDescription {
            componentType: fourcc(b"aufx"),
            componentSubType: fourcc(b"dely"),
            componentManufacturer: fourcc(b"appl"),
            ..Default::default()
        };
        assert_eq!(id_of(&d), "aufx:dely:appl");
        assert_eq!(parse_id("aufx:dely:appl"), Some(d));
        // Unprintable or separator characters as hex.
        let odd = AudioComponentDescription {
            componentSubType: u32::from_be_bytes(*b"a:b\x01"),
            ..d
        };
        let id = id_of(&odd);
        assert_eq!(id, "aufx:0x613a6201:appl");
        assert_eq!(parse_id(&id), Some(odd));
        assert_eq!(parse_id("aufx:dely"), None);
        assert_eq!(parse_id("aufx:dely:appl:x"), None);
        assert_eq!(version_text(0x0001_0203), "1.2.3");
    }
}
