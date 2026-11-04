//! `file:` URLs, as locators hold them: written from paths, read back
//! into paths (also the forms Windows and macOS applications write:
//! `file:///C:/…`, `file://localhost/…`, `file:///Volumes/…`).

use std::path::{Path, PathBuf};

/// A path as a `file://` URL (bytes outside the unreserved set and `/`
/// percent-encoded).
pub fn from_path(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let mut out = String::from("file://");
    if !text.starts_with('/') {
        out.push('/');
    }
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/:".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn unhex(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// The path a `file:` URL names (`None` for other schemes).
pub fn to_path(url: &str) -> Option<PathBuf> {
    let rest = url
        .strip_prefix("file://")
        .or_else(|| url.strip_prefix("file:"))?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    let mut bytes = Vec::with_capacity(rest.len());
    let b = rest.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let (Some(h), Some(l)) = (
                b.get(i + 1).copied().and_then(unhex),
                b.get(i + 2).copied().and_then(unhex),
            )
        {
            bytes.push(h * 16 + l);
            i += 3;
            continue;
        }
        bytes.push(b[i]);
        i += 1;
    }
    let text = String::from_utf8_lossy(&bytes).into_owned();
    // "/C:/…" is a Windows drive.
    let t = text.as_bytes();
    let text = if t.len() > 2 && t[0] == b'/' && t[1].is_ascii_alphabetic() && t[2] == b':' {
        text[1..].to_string()
    } else {
        text
    };
    Some(PathBuf::from(text))
}

/// The file name a URL ends in (what to look for when the path is not
/// here).
pub fn file_name(url: &str) -> Option<String> {
    let p = to_path(url)?;
    let s = p.to_string_lossy().replace('\\', "/");
    s.rsplit('/')
        .next()
        .filter(|n| !n.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_and_urls_round_trip() {
        let p = Path::new("/home/me/Reel 1 Media/Dia Ø.A1.wav");
        let u = from_path(p);
        assert_eq!(u, "file:///home/me/Reel%201%20Media/Dia%20%C3%98.A1.wav");
        assert_eq!(to_path(&u).as_deref(), Some(p));
        assert_eq!(
            to_path("file://localhost/Volumes/Audio/a.wav").as_deref(),
            Some(Path::new("/Volumes/Audio/a.wav"))
        );
        assert_eq!(
            to_path("file:///C:/Media/b.wav").as_deref(),
            Some(Path::new("C:/Media/b.wav"))
        );
        assert_eq!(
            file_name("file:///C:/Media/b%20c.wav").as_deref(),
            Some("b c.wav")
        );
        assert_eq!(to_path("http://example.com/a.wav"), None);
    }
}
