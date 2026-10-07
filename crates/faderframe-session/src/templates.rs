//! Project templates: a project's set-up without its content
//! ([`faderframe_project::template`]), kept in the data folder's
//! `templates` folder, one folder per template (`<name>/<name>.ffproj`, the
//! samplers' samples in `<name>/Samples`), so a template stands on its own.
//!
//! A new project from a template is unsaved, like a new project: its
//! samples are copied into the new project's scratch media, so nothing it
//! does reaches the template.

use crate::{NoticeLevel, Result, SessionError, media, samples};
use faderframe_project::{file, template};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// A saved template.
#[derive(Clone, Debug, PartialEq)]
pub struct ProjectTemplate {
    pub name: String,
    /// Its project file.
    pub path: PathBuf,
    /// When it was saved (the file's time).
    pub saved: Option<SystemTime>,
}

/// The folder that holds the templates.
pub fn templates_dir() -> PathBuf {
    media::data_dir().join("templates")
}

/// The templates, by name.
pub fn templates() -> Vec<ProjectTemplate> {
    templates_in(&templates_dir())
}

fn templates_in(dir: &Path) -> Vec<ProjectTemplate> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<ProjectTemplate> = entries
        .flatten()
        .filter_map(|e| {
            let folder = e.path();
            let name = folder.file_name()?.to_str()?.to_string();
            if name.starts_with('.') {
                return None;
            }
            let path = folder.join(format!("{name}.{}", file::FILE_EXTENSION));
            let saved = std::fs::metadata(&path).ok()?.modified().ok();
            Some(ProjectTemplate { name, path, saved })
        })
        .collect();
    out.sort_by_key(|t| t.name.to_lowercase());
    out
}

/// The template a name would replace (names match whatever their case).
pub fn existing_template(name: &str) -> Option<ProjectTemplate> {
    let name = clean(name).to_lowercase();
    templates()
        .into_iter()
        .find(|t| t.name.to_lowercase() == name)
}

/// A name that can be a folder's.
fn clean(name: &str) -> String {
    let c: String = name
        .trim()
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '-',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect();
    let c = c.trim().trim_matches('.').to_string();
    if c.is_empty() {
        "Template".into()
    } else {
        c.chars().take(80).collect()
    }
}

/// An I/O error, naming the file.
fn io(path: &Path) -> impl Fn(std::io::Error) -> SessionError + '_ {
    move |e| SessionError::Other(format!("{}: {e}", path.display()))
}

/// Copy `from` to `to`, making its folder.
fn copy_file(from: &Path, to: &Path) -> std::io::Result<()> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(from, to).map(|_| ())
}

/// Delete a template (only one in the templates folder).
pub fn delete_template(path: &Path) -> Result<()> {
    let dir = templates_dir();
    let folder = path
        .parent()
        .filter(|f| f.parent() == Some(dir.as_path()))
        .ok_or_else(|| SessionError::Other(format!("{} is not a template", path.display())))?;
    std::fs::remove_dir_all(folder).map_err(io(folder))?;
    Ok(())
}

impl crate::Session {
    /// Keep the project's set-up (everything but its content) as the
    /// template `name`, replacing one of that name. Returns its file.
    pub fn save_template(&mut self, name: &str) -> Result<PathBuf> {
        self.save_template_in(&templates_dir(), name)
    }

    pub(crate) fn save_template_in(&mut self, root: &Path, name: &str) -> Result<PathBuf> {
        let name = match templates_in(root)
            .into_iter()
            .find(|t| t.name.to_lowercase() == clean(name).to_lowercase())
        {
            // Replacing keeps the name as it was spelt.
            Some(old) => old.name,
            None => clean(name),
        };
        self.capture_plugin_states();
        let mut t = template::without_content(&self.project);
        t.name = name.clone();
        // Written beside, then put in place: a failed save leaves the old
        // template as it was.
        let partial = root.join(format!(".{name}.saving"));
        if partial.exists() {
            std::fs::remove_dir_all(&partial).map_err(io(&partial))?;
        }
        std::fs::create_dir_all(&partial).map_err(io(&partial))?;
        // The samplers' samples in the project's media go with it (stored
        // relative to the template); others (SFZ read in place) stay where
        // they are.
        let media = self.media_dir.clone();
        let failed = RefCell::new(Vec::new());
        samples::map_states(&mut t, |p| {
            let stored = media::to_stored(p, Some(&media));
            if stored.is_absolute() {
                return stored;
            }
            if let Err(e) = copy_file(p, &partial.join(&stored)) {
                failed.borrow_mut().push(format!("{}: {e}", p.display()));
            }
            stored
        });
        let file = partial.join(format!("{name}.{}", file::FILE_EXTENSION));
        file::save(&file, &t, Some(&self.workspace))?;
        let folder = root.join(&name);
        if folder.exists() {
            std::fs::remove_dir_all(&folder).map_err(io(&folder))?;
        }
        std::fs::rename(&partial, &folder).map_err(io(&folder))?;
        for f in failed.into_inner() {
            self.notify(
                NoticeLevel::Warning,
                format!("a sample did not go into the template: {f}"),
            );
        }
        self.notify(NoticeLevel::Info, format!("saved the template ‘{name}’"));
        Ok(folder.join(format!("{name}.{}", file::FILE_EXTENSION)))
    }

    /// A new, unsaved project set up as the template (or any project file:
    /// its content is left out) at `path`.
    pub fn new_from_template(&mut self, path: &Path) -> Result<()> {
        let loaded = file::load(path)?;
        let dir = path.parent().map(Path::to_path_buf);
        let mut project = template::without_content(&loaded.project);
        let from = loaded.project.name.clone();
        project.name = "Untitled".into();
        // Nothing in it counts samples: it plays at the device's rate.
        project.sample_rate = self.engine.sample_rate();
        // The template's samples are copied into the new project's
        // scratch media (a save moves them into its folder).
        let scratch = media::new_unsaved_media_dir();
        let failed = RefCell::new(Vec::new());
        samples::map_states(&mut project, |p| {
            let at = media::resolve(p, dir.as_deref());
            if p.is_absolute() {
                return at;
            }
            let to = scratch.join(p);
            match copy_file(&at, &to) {
                Ok(()) => to,
                Err(e) => {
                    failed.borrow_mut().push(format!("{}: {e}", at.display()));
                    at
                }
            }
        });
        let mut adopted = Vec::new();
        for t in &mut project.tracks {
            adopted.extend(self.adopt_instrument(t));
        }
        self.discard_unsaved_media();
        self.path = None;
        self.media_dir = scratch;
        self.unsaved_media = true;
        self.replace_project(project, loaded.workspace)?;
        for n in loaded.notes.iter().chain(&adopted) {
            self.notify(NoticeLevel::Warning, n.clone());
        }
        for f in failed.into_inner() {
            self.notify(
                NoticeLevel::Warning,
                format!("a sample of the template could not be copied: {f}"),
            );
        }
        self.notify(NoticeLevel::Info, format!("new project from ‘{from}’"));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use faderframe_engine::EngineConfig;

    #[test]
    fn names_keep_to_what_a_folder_takes() {
        assert_eq!(clean(" Band: live/4 "), "Band- live-4");
        assert_eq!(clean("..."), "Template");
    }

    #[test]
    fn a_template_is_listed_replaced_and_starts_a_project() {
        let root = std::env::temp_dir().join(format!("ff-templates-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let mut s = crate::Session::demo(EngineConfig::default()).unwrap();
        let tracks = s.project().tracks.len();
        let path = s.save_template_in(&root, "Band").unwrap();
        assert!(path.is_file());
        let listed = templates_in(&root);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "Band");
        // Saving under the same name (any case) replaces it, keeping its
        // spelling.
        s.save_template_in(&root, "band").unwrap();
        let listed = templates_in(&root);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "Band");
        // A project from it: the set-up without content, unsaved.
        s.new_from_template(&path).unwrap();
        assert!(s.path().is_none());
        assert_eq!(s.project().tracks.len(), tracks);
        assert!(s.project().clips.is_empty());
        assert_eq!(s.project().name, "Untitled");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A sampler's samples go into the template and are copied out of it
    /// into a new project, which then plays them from its own media.
    #[test]
    fn a_templates_samples_go_with_it() {
        use crate::{Action, PluginTarget};
        use faderframe_core::builtin;
        use faderframe_project::{PluginRef, Project, TrackKind};
        let root = std::env::temp_dir().join(format!("ff-template-samples-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let wav = root.join("kick.wav");
        let x: Vec<f32> = (0..4800).map(|n| (n as f32 * 0.05).sin() * 0.5).collect();
        faderframe_audio_files::write_wav(
            &wav,
            &[x],
            48_000,
            faderframe_audio_files::WavFormat::Pcm16,
            false,
        )
        .unwrap();
        let mut s = crate::Session::new(Project::new("Kit", 48_000), None, EngineConfig::default())
            .unwrap();
        let t = s.add_track(TrackKind::Instrument).unwrap();
        s.place_plugin(
            t,
            PluginTarget::Instrument,
            PluginRef::builtin(builtin::DRUMS, "Drums"),
        )
        .unwrap();
        let plugin = s.project().track(t).unwrap().inserts[0].id;
        s.dispatch(Action::LoadDeviceSamples {
            plugin,
            slot: 0,
            files: vec![wav],
        })
        .unwrap();
        let file = |s: &crate::Session| {
            let (_, slot) = s.plugin_owner(plugin).unwrap();
            samples::doc_of(slot)
                .files
                .first()
                .cloned()
                .flatten()
                .map(PathBuf::from)
        };
        for _ in 0..500 {
            s.tick(0.01);
            if file(&s).is_some() && !s.loading_samples(plugin) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let in_project = file(&s).unwrap();
        let templates = root.join("templates");
        let path = s.save_template_in(&templates, "Kit").unwrap();
        let kept = templates.join("Kit").join(samples::SAMPLES_FOLDER);
        assert!(
            kept.join("kick.wav").is_file(),
            "the sample is in the template"
        );
        s.new_from_template(&path).unwrap();
        let now = file(&s).unwrap();
        assert!(now.is_absolute() && now.is_file());
        assert!(
            !now.starts_with(&templates),
            "copied out of the template: {now:?}"
        );
        assert_ne!(now, in_project);
        let _ = std::fs::remove_dir_all(&root);
        s.discard_unsaved_media();
    }
}
