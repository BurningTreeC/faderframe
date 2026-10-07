//! Listening on headphones: whose ears (a built-in head or a SOFA file)
//! and the headphone correction (an EqualizerAPO/AutoEq text or an
//! impulse response). Listening only — renders stay as mixed unless they
//! ask for a binaural render. See `faderframe_binaural`.

use crate::{Action, InputChoice, Result, Session, SessionError};
use faderframe_binaural::{BUILTIN_HEADS, Correction, Head};
use faderframe_project::Impact;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

/// A head by its id: one of [`BUILTIN_HEADS`], or `sofa:<path>`.
pub fn load_head(id: &str) -> Result<Head> {
    let head = match id.strip_prefix("sofa:") {
        Some(path) => Head::from_sofa(Path::new(path)),
        None => Head::builtin(id),
    };
    head.map_err(|e| SessionError::Other(e.to_string()))
}

/// A headphone correction from a file: audio (an impulse response, one
/// channel for both ears or one per ear) or EqualizerAPO's text.
pub fn load_correction(path: &Path) -> Result<Correction> {
    let name = path
        .file_stem()
        .map_or_else(|| "correction".into(), |s| s.to_string_lossy().into_owned());
    let err = |e: &dyn std::fmt::Display| SessionError::Other(format!("{}: {e}", path.display()));
    if faderframe_audio_files::decode::is_supported(path) {
        let mut channels: Vec<Vec<f32>> = Vec::new();
        let info = faderframe_audio_files::decode::decode_file(
            path,
            &AtomicBool::new(false),
            |_| Ok(()),
            |block, frames| {
                channels.resize(block.len(), Vec::new());
                for (c, b) in channels.iter_mut().zip(block) {
                    c.extend_from_slice(&b[..frames.min(b.len())]);
                }
                Ok(())
            },
        )
        .map_err(|e| err(&e))?;
        return Correction::from_ir(&name, info.sample_rate, &channels).map_err(|e| err(&e));
    }
    let text = std::fs::read_to_string(path).map_err(|e| err(&e))?;
    Correction::parse(&name, &text).map_err(|e| err(&e))
}

impl Session {
    /// Listen through `head`'s ears (a rebuild when on headphones).
    pub fn use_head(&mut self, head: Head) -> Result<()> {
        if let Some(note) = head.note() {
            self.notify(
                crate::NoticeLevel::Warning,
                format!("{}: {note}", head.name()),
            );
        }
        self.engine.set_head(head);
        if self.engine.binaural().is_some() {
            self.sync(Impact::Graph)?;
        }
        self.revision += 1;
        Ok(())
    }

    /// Listen through the head `id` (built in, or `sofa:<path>`).
    pub fn set_head(&mut self, id: &str) -> Result<()> {
        if id == self.head().id() {
            return Ok(());
        }
        let head = load_head(id)?;
        self.use_head(head)
    }

    pub fn head(&self) -> &Head {
        self.engine.head()
    }

    /// Even out the headphones with the correction in `path` (`None`: no
    /// correction).
    pub fn set_headphone_correction(&mut self, path: Option<&Path>) -> Result<()> {
        let correction = path.map(load_correction).transpose()?;
        if let Some(c) = &correction {
            self.notify(
                crate::NoticeLevel::Info,
                format!("headphone correction: {} ({})", c.name(), c.describe()),
            );
        }
        self.engine
            .set_headphone_correction(correction.map(Arc::new));
        self.correction_path = path.map(Path::to_path_buf);
        if self.engine.binaural().is_some() {
            self.sync(Impact::Graph)?;
        }
        self.revision += 1;
        Ok(())
    }

    /// The headphone correction in use and its file.
    pub fn headphone_correction(&self) -> Option<(&Path, &Correction)> {
        Some((
            self.correction_path.as_deref()?,
            self.engine.headphone_correction()?,
        ))
    }

    /// The heads to listen through (checked: the one in use).
    pub fn head_choices(&self) -> Vec<InputChoice> {
        let now = self.head().id().to_string();
        let mut out: Vec<InputChoice> = BUILTIN_HEADS
            .iter()
            .enumerate()
            .map(|(i, h)| InputChoice {
                label: h.name.into(),
                action: Action::SetHead(h.id.into()),
                checked: now == h.id,
                group_start: i == 2,
            })
            .collect();
        if now.starts_with("sofa:") {
            out.push(InputChoice {
                label: format!("{} (SOFA)", self.head().name()),
                action: Action::SetHead(now.clone()),
                checked: true,
                group_start: true,
            });
        }
        out
    }
}

/// `sofa:<path>` for a SOFA file.
pub fn sofa_id(path: &Path) -> String {
    format!("sofa:{}", path.display())
}

/// The file a `sofa:` id names.
pub fn sofa_path(id: &str) -> Option<PathBuf> {
    id.strip_prefix("sofa:").map(PathBuf::from)
}
