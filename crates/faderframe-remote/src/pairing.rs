//! Pairing: a phone or tablet becomes a known device by typing (or
//! scanning) the code FaderFrame shows. A code is drawn when the remote
//! starts and again after every pairing (each code pairs one device), and
//! names the role the device gets. Paired devices keep a token (32 random
//! bytes) and show it on every connection; FaderFrame keeps only its
//! SHA-256. Wrong codes are counted: after [`MAX_WRONG`] of them the code
//! is withdrawn until a new one is asked for — guessing one code in a
//! million ten times is all anyone gets.

use crate::secret;
use serde::{Deserialize, Serialize};

/// Wrong codes before the code is withdrawn.
pub const MAX_WRONG: u32 = 10;
/// The longest device name kept.
pub const MAX_NAME: usize = 40;

/// What a paired device may see and do.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Role {
    /// The engineer: the transport, the mixer, cue mixes, the launcher,
    /// the setlist, markers, scenes, macros.
    #[default]
    Full,
    /// A performer: only their cue mix (by its name), and the lyrics, the
    /// setlist and the position to read.
    Performer { cue: String },
    /// Reads the lyrics, the setlist and the position; changes nothing.
    Viewer,
}

impl Role {
    pub fn label(&self) -> String {
        match self {
            Role::Full => "Full control".into(),
            Role::Performer { cue } => format!("Performer: {cue}"),
            Role::Viewer => "Viewer (lyrics, setlist)".into(),
        }
    }

    /// The short name the web app keys its screens by.
    pub fn kind(&self) -> &'static str {
        match self {
            Role::Full => "full",
            Role::Performer { .. } => "performer",
            Role::Viewer => "viewer",
        }
    }
}

/// A device that paired.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PairedDevice {
    /// Its own id (random; never shown to other devices).
    pub id: u64,
    pub name: String,
    /// SHA-256 of its token, hexadecimal.
    pub token_hash: String,
    pub role: Role,
    /// When it paired (seconds since 1970).
    pub paired: u64,
}

impl Default for PairedDevice {
    fn default() -> Self {
        Self {
            id: 0,
            name: String::new(),
            token_hash: String::new(),
            role: Role::Viewer,
            paired: 0,
        }
    }
}

/// Why pairing failed.
#[derive(Debug, PartialEq, Eq)]
pub enum PairError {
    /// No code is offered (withdrawn after wrong ones).
    NoCode,
    WrongCode,
    /// The system gave no random numbers.
    Random(String),
}

impl PairError {
    /// The word the web app is told.
    pub fn word(&self) -> &'static str {
        match self {
            PairError::NoCode => "no_code",
            PairError::WrongCode => "wrong_code",
            PairError::Random(_) => "error",
        }
    }
}

/// A device name as kept: trimmed, without control characters, at most
/// [`MAX_NAME`] characters.
pub fn clean_name(name: &str) -> String {
    let n: String = name
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_NAME)
        .collect();
    let n = n.trim();
    if n.is_empty() {
        "Device".into()
    } else {
        n.to_string()
    }
}

/// The pairing state the server keeps.
#[derive(Debug)]
pub struct Pairing {
    code: Option<String>,
    role: Role,
    devices: Vec<PairedDevice>,
    wrong: u32,
    /// Bumped when the code or the devices change.
    revision: u64,
}

impl Pairing {
    /// The devices paired before, and a code for `role`.
    pub fn new(devices: Vec<PairedDevice>, role: Role) -> Self {
        let mut p = Self {
            code: None,
            role: Role::Full,
            devices,
            wrong: 0,
            revision: 0,
        };
        p.new_code(role);
        p
    }

    /// The code offered now.
    pub fn code(&self) -> Option<&str> {
        self.code.as_deref()
    }

    /// The role the code gives.
    pub fn code_role(&self) -> &Role {
        &self.role
    }

    /// Wrong codes tried since the code was drawn.
    pub fn wrong_codes(&self) -> u32 {
        self.wrong
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Draw a new code for `role` (the wrong ones forgotten).
    pub fn new_code(&mut self, role: Role) {
        self.code = match secret::pairing_code() {
            Ok(c) => Some(c),
            Err(e) => {
                tracing::warn!("remote pairing: {e}");
                None
            }
        };
        self.role = role;
        self.wrong = 0;
        self.revision += 1;
    }

    /// Pair a device by `code`: its record (kept here) and the token it
    /// keeps. Each code pairs one device; the next is drawn at once.
    pub fn pair(
        &mut self,
        code: &str,
        name: &str,
        now: u64,
    ) -> Result<(PairedDevice, String), PairError> {
        let Some(offered) = &self.code else {
            return Err(PairError::NoCode);
        };
        if !secret::same(offered.as_bytes(), code.trim().as_bytes()) {
            self.wrong += 1;
            if self.wrong >= MAX_WRONG {
                tracing::warn!("remote pairing: {MAX_WRONG} wrong codes, the code is withdrawn");
                self.code = None;
            }
            self.revision += 1;
            return Err(PairError::WrongCode);
        }
        let token = secret::token().map_err(|e| PairError::Random(e.to_string()))?;
        let mut id = secret::random_u64().map_err(|e| PairError::Random(e.to_string()))?;
        while id == 0 || self.devices.iter().any(|d| d.id == id) {
            id = id.wrapping_add(1);
        }
        let device = PairedDevice {
            id,
            name: clean_name(name),
            token_hash: secret::token_hash(&token),
            role: self.role.clone(),
            paired: now,
        };
        self.devices.push(device.clone());
        let role = self.role.clone();
        self.new_code(role);
        Ok((device, token))
    }

    /// The device a token belongs to.
    pub fn authenticate(&self, token: &str) -> Option<&PairedDevice> {
        let hash = secret::token_hash(token);
        self.devices
            .iter()
            .find(|d| secret::same(d.token_hash.as_bytes(), hash.as_bytes()))
    }

    pub fn devices(&self) -> &[PairedDevice] {
        &self.devices
    }

    pub fn device(&self, id: u64) -> Option<&PairedDevice> {
        self.devices.iter().find(|d| d.id == id)
    }

    /// The devices as the settings have them (renamed, roles changed,
    /// forgotten).
    pub fn set_devices(&mut self, devices: Vec<PairedDevice>) {
        if devices != self.devices {
            self.devices = devices;
            self.revision += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_code_pairs_one_device_and_its_token_lets_it_back() {
        let mut p = Pairing::new(Vec::new(), Role::Full);
        let code = p.code().unwrap().to_string();
        assert_eq!(p.pair("000000x", "Phone", 1), Err(PairError::WrongCode));
        let (device, token) = p.pair(&code, " Phone\u{7} ", 5).unwrap();
        assert_eq!(device.name, "Phone");
        assert_eq!(device.role, Role::Full);
        assert_eq!(device.paired, 5);
        assert_ne!(device.token_hash, token);
        assert_eq!(p.authenticate(&token).map(|d| d.id), Some(device.id));
        assert!(p.authenticate("nope").is_none());
        // Used: the next code is another.
        assert_ne!(p.code(), Some(code.as_str()));
        assert_eq!(p.wrong_codes(), 0);
        // A performer's code.
        p.new_code(Role::Performer {
            cue: "Drums".into(),
        });
        let code = p.code().unwrap().to_string();
        let (drummer, _) = p.pair(&code, "Drummer", 6).unwrap();
        assert_eq!(
            drummer.role,
            Role::Performer {
                cue: "Drums".into()
            }
        );
        assert_eq!(p.devices().len(), 2);
        // Forgotten: its token no longer counts.
        let kept: Vec<PairedDevice> = p
            .devices()
            .iter()
            .filter(|d| d.id != device.id)
            .cloned()
            .collect();
        p.set_devices(kept);
        assert!(p.authenticate(&token).is_none());
    }

    #[test]
    fn wrong_codes_withdraw_the_code() {
        let mut p = Pairing::new(Vec::new(), Role::Viewer);
        let code = p.code().unwrap().to_string();
        let wrong = if code == "123456" { "654321" } else { "123456" };
        for _ in 0..MAX_WRONG {
            assert_eq!(p.pair(wrong, "x", 0), Err(PairError::WrongCode));
        }
        assert!(p.code().is_none());
        // Even the right code no longer pairs.
        assert_eq!(p.pair(&code, "x", 0), Err(PairError::NoCode));
        assert!(p.devices().is_empty());
        p.new_code(Role::Viewer);
        assert!(p.code().is_some());
        assert_eq!(p.wrong_codes(), 0);
    }

    #[test]
    fn roles_and_devices_keep_in_the_settings_file() {
        let d = PairedDevice {
            id: 7,
            name: "Tablet".into(),
            token_hash: "ab".into(),
            role: Role::Performer { cue: "Vox".into() },
            paired: 3,
        };
        let json = serde_json::to_string(&d).unwrap();
        assert!(
            json.contains(r#""role":{"kind":"performer","cue":"Vox"}"#),
            "{json}"
        );
        let back: PairedDevice = serde_json::from_str(&json).unwrap();
        assert_eq!(back, d);
        let partial: PairedDevice = serde_json::from_str(r#"{"name":"x"}"#).unwrap();
        assert_eq!(partial.role, Role::Viewer);
        assert_eq!(clean_name(&"x".repeat(100)).len(), MAX_NAME);
        assert_eq!(clean_name("  "), "Device");
    }
}
