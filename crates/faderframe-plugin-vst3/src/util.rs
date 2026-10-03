//! String and id conversions between VST3 and Rust.

use std::ffi::c_char;
use vst3::Steinberg::Vst::TChar;
use vst3::Steinberg::{FUnknown, TUID, tresult};

/// A class id as 32 upper-case hex digits (the id FaderFrame stores).
pub fn tuid_hex(t: &TUID) -> String {
    t.iter().map(|b| format!("{:02X}", *b as u8)).collect()
}

pub fn parse_tuid(s: &str) -> Option<TUID> {
    if s.len() != 32 || !s.is_ascii() {
        return None;
    }
    let mut out: TUID = [0; 16];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok()? as c_char;
    }
    Some(out)
}

/// A TUID as the byte array `Interface::IID` uses.
pub fn guid(t: &TUID) -> [u8; 16] {
    t.map(|b| b as u8)
}

pub fn tuid(g: &[u8; 16]) -> TUID {
    g.map(|b| b as c_char)
}

/// A zero-terminated 8-bit string (UTF-8 in practice).
pub fn cstr(bytes: &[c_char]) -> String {
    let b: Vec<u8> = bytes
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    String::from_utf8_lossy(&b).trim().to_string()
}

/// A zero-terminated UTF-16 string.
pub fn wstr(chars: &[TChar]) -> String {
    let n = chars.iter().position(|&c| c == 0).unwrap_or(chars.len());
    String::from_utf16_lossy(&chars[..n]).trim().to_string()
}

pub fn write_wstr(dst: &mut [TChar], s: &str) {
    let Some(room) = dst.len().checked_sub(1) else {
        return;
    };
    let mut n = 0;
    for (d, c) in dst.iter_mut().zip(s.encode_utf16().take(room)) {
        *d = c as TChar;
        n += 1;
    }
    dst[n] = 0;
}

/// `queryInterface` on any interface pointer.
///
/// # Safety
/// `unknown` must be a live COM object.
pub unsafe fn query_raw(
    unknown: *mut FUnknown,
    iid: &[u8; 16],
    obj: *mut *mut std::ffi::c_void,
) -> tresult {
    let iid = tuid(iid);
    // SAFETY: every COM interface starts with the FUnknown vtable.
    unsafe { ((*(*unknown).vtbl).queryInterface)(unknown, &iid, obj) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_and_strings_round_trip() {
        let t: TUID = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, -1, 12, 13, 14, 15, 16];
        let s = tuid_hex(&t);
        assert_eq!(s, "0102030405060708090AFF0C0D0E0F10");
        assert_eq!(parse_tuid(&s), Some(t));
        assert_eq!(parse_tuid("xyz"), None);
        let mut w = [0 as TChar; 8];
        write_wstr(&mut w, "Grüße und mehr");
        assert_eq!(wstr(&w), "Grüße u");
    }
}
