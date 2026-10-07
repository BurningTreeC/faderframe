//! Plugin state: the control ports' values (by symbol) and the properties
//! a plugin stores through the state extension, kept with URIs instead of
//! URIDs (those are only valid in one process).
//!
//! Saved form (little endian): `FFL2`, version (u32), the ports (count,
//! then symbol + f32 each), the properties (count, then key URI, type
//! URI, flags (u32), value bytes each); strings and byte runs are a u32
//! length and the bytes. A URID value is stored as its URI.

use crate::sys::{self, LV2_Handle, LV2_State_Interface};
use crate::ttl::{self, Graph, Node};
use crate::urid::{self, Urids};
use std::ffi::{CStr, CString, c_char, c_void};

const MAGIC: &[u8; 4] = b"FFL2";
const VERSION: u32 = 1;

const XSD: &str = "http://www.w3.org/2001/XMLSchema#";
const ATOM: &str = "http://lv2plug.in/ns/ext/atom#";

/// One stored property.
#[derive(Clone, Debug, PartialEq)]
pub struct Property {
    pub key: String,
    /// The value's atom type URI.
    pub type_uri: String,
    pub flags: u32,
    pub value: Vec<u8>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct State {
    pub ports: Vec<(String, f32)>,
    pub properties: Vec<Property>,
}

fn put_bytes(out: &mut Vec<u8>, b: &[u8]) {
    out.extend_from_slice(&(b.len() as u32).to_le_bytes());
    out.extend_from_slice(b);
}

struct Reader<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.at.checked_add(n).filter(|e| *e <= self.data.len());
        let end = end.ok_or("the state ends early")?;
        let s = &self.data[self.at..end];
        self.at = end;
        Ok(s)
    }

    fn u32(&mut self) -> Result<u32, String> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn bytes(&mut self) -> Result<&'a [u8], String> {
        let n = self.u32()? as usize;
        self.take(n)
    }

    fn text(&mut self) -> Result<String, String> {
        Ok(String::from_utf8_lossy(self.bytes()?).into_owned())
    }
}

impl State {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&VERSION.to_le_bytes());
        out.extend_from_slice(&(self.ports.len() as u32).to_le_bytes());
        for (symbol, v) in &self.ports {
            put_bytes(&mut out, symbol.as_bytes());
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&(self.properties.len() as u32).to_le_bytes());
        for p in &self.properties {
            put_bytes(&mut out, p.key.as_bytes());
            put_bytes(&mut out, p.type_uri.as_bytes());
            out.extend_from_slice(&p.flags.to_le_bytes());
            put_bytes(&mut out, &p.value);
        }
        out
    }

    pub fn from_bytes(data: &[u8]) -> Result<State, String> {
        if data.get(..4) != Some(MAGIC) {
            return Err("not an LV2 state of FaderFrame".into());
        }
        let mut r = Reader { data, at: 4 };
        let version = r.u32()?;
        if version > VERSION {
            return Err(format!("state version {version} is newer than this host"));
        }
        let mut s = State::default();
        for _ in 0..r.u32()? {
            let symbol = r.text()?;
            let b = r.take(4)?;
            s.ports
                .push((symbol, f32::from_le_bytes([b[0], b[1], b[2], b[3]])));
        }
        for _ in 0..r.u32()? {
            let key = r.text()?;
            let type_uri = r.text()?;
            let flags = r.u32()?;
            let value = r.bytes()?.to_vec();
            s.properties.push(Property {
                key,
                type_uri,
                flags,
                value,
            });
        }
        Ok(s)
    }
}

/// A null-terminated string's bytes (atom strings and paths include it).
fn c_bytes(s: &str) -> Vec<u8> {
    let mut v = s.as_bytes().to_vec();
    v.push(0);
    v
}

/// The properties of a `state:state` node in a plugin's or preset's data
/// (`base`: files relative to it are paths).
pub fn properties_of(g: &Graph, node: &Node) -> Vec<Property> {
    let mut out = Vec::new();
    let (Node::Blank(_) | Node::Iri(_)) = node else {
        return out;
    };
    for (key, value) in g.pairs(node) {
        let (type_uri, bytes) = match value {
            Node::Iri(iri) => match ttl::url_path(iri) {
                Some(path) => (format!("{ATOM}Path"), c_bytes(&path.to_string_lossy())),
                None => (format!("{ATOM}URID"), c_bytes(iri)),
            },
            Node::Literal {
                value, datatype, ..
            } => {
                let local = datatype.strip_prefix(XSD).unwrap_or("");
                match local {
                    "float" => match value.trim().parse::<f32>() {
                        Ok(f) => (format!("{ATOM}Float"), f.to_le_bytes().to_vec()),
                        Err(_) => continue,
                    },
                    "double" | "decimal" => match value.trim().parse::<f64>() {
                        Ok(f) => (format!("{ATOM}Double"), f.to_le_bytes().to_vec()),
                        Err(_) => continue,
                    },
                    "int" | "integer" | "short" | "byte" => match value.trim().parse::<i32>() {
                        Ok(i) => (format!("{ATOM}Int"), i.to_le_bytes().to_vec()),
                        Err(_) => continue,
                    },
                    "long" => match value.trim().parse::<i64>() {
                        Ok(i) => (format!("{ATOM}Long"), i.to_le_bytes().to_vec()),
                        Err(_) => continue,
                    },
                    "boolean" => {
                        let b = i32::from(value == "true" || value == "1");
                        (format!("{ATOM}Bool"), b.to_le_bytes().to_vec())
                    }
                    "base64Binary" => match base64_decode(value) {
                        Some(b) => (format!("{ATOM}Chunk"), b),
                        None => continue,
                    },
                    _ => (format!("{ATOM}String"), c_bytes(value)),
                }
            }
            Node::Blank(_) => continue,
        };
        out.push(Property {
            key: key.to_string(),
            type_uri,
            flags: sys::LV2_STATE_IS_POD | sys::LV2_STATE_IS_PORTABLE,
            value: bytes,
        });
    }
    out
}

fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut acc = 0u32;
    let mut bits = 0;
    for c in text.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            c if c.is_ascii_whitespace() => continue,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

/// What a store callback collects.
struct Store {
    properties: Vec<Property>,
}

unsafe extern "C" fn store(
    handle: *mut c_void,
    key: u32,
    value: *const c_void,
    size: usize,
    type_: u32,
    flags: u32,
) -> u32 {
    // SAFETY: `save` passes our `Store`.
    let s = unsafe { &mut *handle.cast::<Store>() };
    let (Some(key), Some(type_uri)) = (urid::unmap(key), urid::unmap(type_)) else {
        return 1;
    };
    let bytes = if size == 0 || value.is_null() {
        Vec::new()
    } else {
        // SAFETY: the plugin hands `size` readable bytes.
        unsafe { std::slice::from_raw_parts(value.cast::<u8>(), size) }.to_vec()
    };
    let bytes = if type_ == Urids::get().atom_urid && bytes.len() == 4 {
        let id = u32::from_ne_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        c_bytes(&urid::unmap(id).unwrap_or_default())
    } else {
        bytes
    };
    s.properties.push(Property {
        key,
        type_uri,
        flags,
        value: bytes,
    });
    sys::LV2_STATE_SUCCESS
}

/// What a retrieve callback hands out (alive during `restore`).
struct Retrieve {
    entries: Vec<(u32, u32, u32, Vec<u8>)>,
}

unsafe extern "C" fn retrieve(
    handle: *mut c_void,
    key: u32,
    size: *mut usize,
    type_: *mut u32,
    flags: *mut u32,
) -> *const c_void {
    // SAFETY: `restore` passes our `Retrieve`.
    let r = unsafe { &*handle.cast::<Retrieve>() };
    let Some((_, t, f, v)) = r.entries.iter().find(|e| e.0 == key) else {
        return std::ptr::null();
    };
    // SAFETY: out parameters the plugin passes (may be null).
    unsafe {
        if !size.is_null() {
            *size = v.len();
        }
        if !type_.is_null() {
            *type_ = *t;
        }
        if !flags.is_null() {
            *flags = *f;
        }
    }
    v.as_ptr().cast()
}

// --- paths: absolute paths stay as they are ---------------------------------------------

unsafe extern "C" fn abstract_path(_: *mut c_void, path: *const c_char) -> *mut c_char {
    if path.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: a null-terminated path; the copy is freed by the plugin
    // through `free_path` (or libc's free).
    unsafe { libc::strdup(path) }
}

unsafe extern "C" fn absolute_path(_: *mut c_void, path: *const c_char) -> *mut c_char {
    // SAFETY: as in `abstract_path`.
    unsafe { abstract_path(std::ptr::null_mut(), path) }
}

unsafe extern "C" fn free_path(_: *mut c_void, path: *mut c_char) {
    // SAFETY: a string from `strdup` above.
    unsafe { libc::free(path.cast()) }
}

/// The features `save`/`restore` get: map and free paths, plus the URID
/// maps.
struct StateFeatures {
    _map_path: Box<sys::LV2_State_Map_Path>,
    _free_path: Box<sys::LV2_State_Free_Path>,
    _map: Box<sys::LV2_URID_Map>,
    _schedule: Option<Box<sys::LV2_Worker_Schedule>>,
    _uris: Vec<CString>,
    _list: Vec<sys::LV2_Feature>,
    pointers: Vec<*const sys::LV2_Feature>,
}

impl StateFeatures {
    fn new(schedule: Option<sys::LV2_Worker_Schedule>) -> StateFeatures {
        let map_path = Box::new(sys::LV2_State_Map_Path {
            handle: std::ptr::null_mut(),
            abstract_path: Some(abstract_path),
            absolute_path: Some(absolute_path),
        });
        let free_path = Box::new(sys::LV2_State_Free_Path {
            handle: std::ptr::null_mut(),
            free_path: Some(free_path),
        });
        let map = Box::new(urid::map_feature());
        let schedule = schedule.map(Box::new);
        let mut entries: Vec<(&str, *mut c_void)> = vec![
            (
                sys::uri::STATE_MAP_PATH,
                (&*map_path as *const sys::LV2_State_Map_Path)
                    .cast_mut()
                    .cast(),
            ),
            (
                sys::uri::STATE_FREE_PATH,
                (&*free_path as *const sys::LV2_State_Free_Path)
                    .cast_mut()
                    .cast(),
            ),
            (
                sys::uri::URID_MAP,
                (&*map as *const sys::LV2_URID_Map).cast_mut().cast(),
            ),
        ];
        if let Some(s) = &schedule {
            entries.push((
                sys::uri::WORKER_SCHEDULE,
                (&**s as *const sys::LV2_Worker_Schedule).cast_mut().cast(),
            ));
        }
        let uris: Vec<CString> = entries
            .iter()
            .map(|(u, _)| CString::new(*u).unwrap_or_default())
            .collect();
        let list: Vec<sys::LV2_Feature> = uris
            .iter()
            .zip(&entries)
            .map(|(u, (_, d))| sys::LV2_Feature {
                uri: u.as_ptr(),
                data: *d,
            })
            .collect();
        let mut pointers: Vec<*const sys::LV2_Feature> =
            list.iter().map(|f| f as *const sys::LV2_Feature).collect();
        pointers.push(std::ptr::null());
        StateFeatures {
            _map_path: map_path,
            _free_path: free_path,
            _map: map,
            _schedule: schedule,
            _uris: uris,
            _list: list,
            pointers,
        }
    }
}

/// The properties the plugin stores.
///
/// # Safety
/// `iface` is `handle`'s state interface; `save` may run alongside `run`
/// (the extension's own threading class) but not alongside instantiation
/// functions.
pub unsafe fn save(handle: LV2_Handle, iface: *const LV2_State_Interface) -> Vec<Property> {
    let mut s = Store {
        properties: Vec::new(),
    };
    let features = StateFeatures::new(None);
    // SAFETY: as documented; our store and features live through the call.
    unsafe {
        if let Some(f) = (*iface).save {
            f(
                handle,
                store,
                (&mut s as *mut Store).cast(),
                sys::LV2_STATE_IS_POD | sys::LV2_STATE_IS_PORTABLE,
                features.pointers.as_ptr(),
            );
        }
    }
    s.properties
}

/// Hand `properties` to the plugin.
///
/// # Safety
/// `iface` is `handle`'s state interface, and nothing else runs on the
/// instance during the call (unless it declares thread-safe restore).
pub unsafe fn restore(
    handle: LV2_Handle,
    iface: *const LV2_State_Interface,
    properties: &[Property],
    schedule: Option<sys::LV2_Worker_Schedule>,
) -> Result<(), String> {
    let u = Urids::get();
    let entries = properties
        .iter()
        .map(|p| {
            let t = urid::map(&p.type_uri);
            let value = if t == u.atom_urid {
                let text = CStr::from_bytes_until_nul(&p.value)
                    .map(|c| c.to_string_lossy().into_owned())
                    .unwrap_or_else(|_| String::from_utf8_lossy(&p.value).into_owned());
                urid::map(&text).to_ne_bytes().to_vec()
            } else {
                p.value.clone()
            };
            (urid::map(&p.key), t, p.flags, value)
        })
        .collect();
    let r = Retrieve { entries };
    let features = StateFeatures::new(schedule);
    // SAFETY: as documented; our retrieve data and features live through
    // the call.
    let status = unsafe {
        match (*iface).restore {
            Some(f) => f(
                handle,
                retrieve,
                (&r as *const Retrieve).cast_mut().cast(),
                0,
                features.pointers.as_ptr(),
            ),
            None => sys::LV2_STATE_SUCCESS,
        }
    };
    if status == sys::LV2_STATE_SUCCESS {
        Ok(())
    } else {
        Err(format!("the plugin refused its state ({status})"))
    }
}

/// A preset's state: port values (by symbol) and properties.
pub fn preset(g: &Graph, preset: &Node) -> State {
    let ports = g
        .objects(preset, &format!("{}port", ttl::LV2))
        .filter_map(|port| {
            Some((
                g.text(port, &format!("{}symbol", ttl::LV2))?,
                g.number(port, &format!("{}value", ttl::PSET))? as f32,
            ))
        })
        .collect();
    let properties = g
        .object(preset, &format!("{}state", ttl::STATE))
        .map(|s| properties_of(g, s))
        .unwrap_or_default();
    State { ports, properties }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_round_trips_through_bytes() {
        let s = State {
            ports: vec![("gain".into(), -3.5), ("on".into(), 1.0)],
            properties: vec![Property {
                key: "urn:x#sample".into(),
                type_uri: format!("{ATOM}Path"),
                flags: 3,
                value: c_bytes("/a/b.wav"),
            }],
        };
        let b = s.to_bytes();
        assert_eq!(State::from_bytes(&b).unwrap(), s);
        assert!(State::from_bytes(b"nope").is_err());
        assert!(State::from_bytes(&b[..b.len() - 2]).is_err());
    }

    #[test]
    fn turtle_state_becomes_typed_properties() {
        let mut g = Graph::new();
        g.load_bytes(
            br#"@prefix state: <http://lv2plug.in/ns/ext/state#> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
@prefix pset: <http://lv2plug.in/ns/ext/presets#> .
@prefix lv2: <http://lv2plug.in/ns/lv2core#> .
<urn:p> state:state [
    <urn:p#sample> <click.wav> ;
    <urn:p#gain> "0.5"^^xsd:float ;
    <urn:p#name> "hi" ;
    <urn:p#data> "AQID"^^xsd:base64Binary
] ;
    lv2:port [ lv2:symbol "gain" ; pset:value 2 ] ."#,
            "file:///b/x.lv2/p.ttl",
        )
        .unwrap();
        let s = preset(&g, &Node::Iri("urn:p".into()));
        assert_eq!(s.ports, [("gain".to_string(), 2.0)]);
        let by = |k: &str| s.properties.iter().find(|p| p.key == k).unwrap();
        assert_eq!(by("urn:p#sample").type_uri, format!("{ATOM}Path"));
        assert_eq!(by("urn:p#sample").value, c_bytes("/b/x.lv2/click.wav"));
        assert_eq!(by("urn:p#gain").value, 0.5f32.to_le_bytes());
        assert_eq!(by("urn:p#name").value, b"hi\0");
        assert_eq!(by("urn:p#data").value, [1, 2, 3]);
    }
}
