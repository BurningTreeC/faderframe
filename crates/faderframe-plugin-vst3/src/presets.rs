//! VST3 preset files (`.vstpreset`): found in the standard folders and
//! read into FaderFrame's VST3 state container.
//!
//! Layout: `VST3`, version (i32 LE), class id (32 ASCII hex), offset of the
//! chunk list (i64 LE); the list is `List`, count (i32 LE) and entries of
//! id (4 bytes), offset and size (i64 LE). `Comp` holds the component
//! state, `Cont` the controller state.

use faderframe_plugin_host::PluginError;
use faderframe_plugin_host::scan::ScannedPlugin;
use std::path::{Path, PathBuf};

/// Preset folders for a plugin (user first, then system).
fn folders(p: &ScannedPlugin) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        roots.push(PathBuf::from(home).join(".vst3/presets"));
    }
    roots.push(PathBuf::from("/usr/local/share/vst3/presets"));
    roots.push(PathBuf::from("/usr/share/vst3/presets"));
    roots
        .into_iter()
        .map(|r| r.join(&p.vendor).join(&p.name))
        .collect()
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let path = e.path();
        if path.is_dir() && depth < 4 {
            walk(&path, out, depth + 1);
        } else if path
            .extension()
            .is_some_and(|x| x.eq_ignore_ascii_case("vstpreset"))
        {
            out.push(path);
        }
    }
}

/// Class id written in a preset's header.
fn class_id(data: &[u8]) -> Option<&str> {
    (data.get(..4)? == b"VST3").then_some(())?;
    std::str::from_utf8(data.get(8..40)?).ok()
}

/// The plugin's `.vstpreset` files, by name.
pub fn files(p: &ScannedPlugin) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for dir in folders(p) {
        walk(&dir, &mut out, 0);
    }
    // Only presets of this class (header check; small reads).
    out.retain(|f| {
        std::fs::read(f)
            .ok()
            .is_some_and(|d| class_id(&d).is_some_and(|c| c.eq_ignore_ascii_case(&p.id)))
    });
    out.sort_by_key(|f| f.file_stem().map(|s| s.to_string_lossy().to_lowercase()));
    out.dedup();
    out
}

fn i64_at(d: &[u8], at: usize) -> Option<i64> {
    Some(i64::from_le_bytes(d.get(at..at + 8)?.try_into().ok()?))
}

fn chunk<'a>(d: &'a [u8], id: &[u8; 4]) -> Option<&'a [u8]> {
    let list = usize::try_from(i64_at(d, 40)?).ok()?;
    (d.get(list..list + 4)? == b"List").then_some(())?;
    let count = i32::from_le_bytes(d.get(list + 4..list + 8)?.try_into().ok()?);
    for k in 0..usize::try_from(count).ok()? {
        let e = list + 8 + k * 20;
        if d.get(e..e + 4)? == id {
            let off = usize::try_from(i64_at(d, e + 4)?).ok()?;
            let size = usize::try_from(i64_at(d, e + 12)?).ok()?;
            return d.get(off..off.checked_add(size)?);
        }
    }
    None
}

/// FaderFrame VST3 state (`FFV3`, component, controller) of a preset for
/// class `class`.
pub fn state(data: &[u8], class: &str) -> Result<Vec<u8>, PluginError> {
    let bad = |why: &str| PluginError::InvalidState(format!("VST3 preset: {why}"));
    let cid = class_id(data).ok_or_else(|| bad("not a .vstpreset file"))?;
    if !cid.eq_ignore_ascii_case(class) {
        return Err(bad("made for another plugin"));
    }
    let comp = chunk(data, b"Comp").ok_or_else(|| bad("no component state"))?;
    let cont = chunk(data, b"Cont").unwrap_or(&[]);
    let mut out = Vec::with_capacity(12 + comp.len() + cont.len());
    out.extend_from_slice(b"FFV3");
    out.extend_from_slice(&(comp.len() as u32).to_le_bytes());
    out.extend_from_slice(comp);
    out.extend_from_slice(&(cont.len() as u32).to_le_bytes());
    out.extend_from_slice(cont);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A preset as Steinberg's SDK writes it.
    pub(crate) fn preset(class: &str, comp: &[u8], cont: &[u8]) -> Vec<u8> {
        let mut d = b"VST3".to_vec();
        d.extend_from_slice(&1i32.to_le_bytes());
        d.extend_from_slice(class.as_bytes());
        d.extend_from_slice(&0i64.to_le_bytes()); // list offset, patched
        let comp_at = d.len() as i64;
        d.extend_from_slice(comp);
        let cont_at = d.len() as i64;
        d.extend_from_slice(cont);
        let list = d.len() as i64;
        d[40..48].copy_from_slice(&list.to_le_bytes());
        d.extend_from_slice(b"List");
        d.extend_from_slice(&2i32.to_le_bytes());
        for (id, at, n) in [
            (b"Comp", comp_at, comp.len()),
            (b"Cont", cont_at, cont.len()),
        ] {
            d.extend_from_slice(id);
            d.extend_from_slice(&at.to_le_bytes());
            d.extend_from_slice(&(n as i64).to_le_bytes());
        }
        d
    }

    #[test]
    fn reads_component_and_controller_chunks() {
        let class = "D39D5B69D6AF42FA1234567844695661";
        let d = preset(class, b"component!", b"ctl");
        let s = state(&d, &class.to_lowercase()).unwrap();
        assert_eq!(&s[..4], b"FFV3");
        assert_eq!(&s[8..18], b"component!");
        assert_eq!(&s[22..25], b"ctl");
        assert!(state(&d, "00000000000000000000000000000000").is_err());
        assert!(state(b"RIFF", class).is_err());
    }
}
