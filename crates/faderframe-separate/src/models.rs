//! The separation models FaderFrame can download: where each is
//! published, its checksum, licence and attribution. No weights are in
//! the repository or the packages; the session downloads a model when
//! asked and checks it against the checksum its publisher gives.

use md5::{Digest, Md5};
use std::io::Read;
use std::path::Path;

/// A downloadable model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModelInfo {
    /// Stable id (preferences, dev actions, the folder it lives in).
    pub id: &'static str,
    pub name: &'static str,
    /// The published file's name.
    pub file: &'static str,
    pub url: &'static str,
    pub bytes: u64,
    /// The publisher's MD5 checksum (lowercase hex).
    pub md5: &'static str,
    pub licence: &'static str,
    pub licence_url: &'static str,
    /// Who made it and where it is published (what the licence asks to
    /// be credited).
    pub attribution: &'static str,
    /// What it was trained on.
    pub training: &'static str,
    /// A caveat to show beside it (the training data's own terms), if any.
    pub caveat: Option<&'static str>,
    /// The sources it separates, in its output order.
    pub sources: &'static [&'static str],
}

/// Sony's CrossNet-Open-Unmix trained on its internal data set.
pub const X_UMXL: ModelInfo = ModelInfo {
    id: "x-umxl",
    name: "X-UMXL (CrossNet-Open-Unmix Large)",
    file: "pretrained_xumxl.pth",
    url: "https://zenodo.org/records/7128659/files/pretrained_xumxl.pth?download=1",
    bytes: 142_514_830,
    md5: "492f8a2f4cd094b6bf97d32b67906d44",
    licence: "CC BY 4.0",
    licence_url: "https://creativecommons.org/licenses/by/4.0/",
    attribution: "CrossNet-Open-Unmix (X-UMXL) weights by Ryosuke Sawata, Stefan Uhlich, \
                  Shusuke Takahashi and Yuki Mitsufuji (Sony), \
                  https://doi.org/10.5281/zenodo.7128659, licensed CC BY 4.0; used unmodified",
    training: "about 100 hours (1505 songs) of Sony's own music; no MUSDB18",
    caveat: None,
    sources: &["bass", "drums", "vocals", "other"],
};

/// Every model, the default first.
pub const MODELS: &[ModelInfo] = &[X_UMXL];

pub fn by_id(id: &str) -> Option<&'static ModelInfo> {
    MODELS.iter().find(|m| m.id == id)
}

/// The default model.
pub fn default_model() -> &'static ModelInfo {
    &MODELS[0]
}

/// A file's MD5 checksum (lowercase hex).
pub fn md5_of(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Md5::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// Is `path` the model's published file (size and checksum)?
pub fn verify(path: &Path, model: &ModelInfo) -> Result<(), String> {
    let len = std::fs::metadata(path)
        .map_err(|e| format!("{}: {e}", path.display()))?
        .len();
    if len != model.bytes {
        return Err(format!(
            "{} has {len} bytes, not the {} published",
            path.display(),
            model.bytes
        ));
    }
    let sum = md5_of(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if sum != model.md5 {
        return Err(format!(
            "{} is not the published file (MD5 {sum}, not {})",
            path.display(),
            model.md5
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksums_are_md5() {
        let dir = std::env::temp_dir().join(format!("ff-separate-md5-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("abc");
        std::fs::write(&p, b"abc").unwrap();
        assert_eq!(md5_of(&p).unwrap(), "900150983cd24fb0d6963f7d28e17f72");
        assert!(verify(&p, &X_UMXL).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_default_model_is_the_one_with_clean_data() {
        let m = default_model();
        assert_eq!(m.id, "x-umxl");
        assert!(m.caveat.is_none());
        assert_eq!(by_id("x-umxl"), Some(m));
    }
}
