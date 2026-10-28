//! The user's own themes as files: `<settings>/themes/<name>.json`, each a
//! built-in skin and the colours changed —
//!
//! ```json
//! { "name": "Night Shift", "base": "studio",
//!   "colors": { "ui.accent": "#3fa9f5", "arranger.background": "#101418" } }
//! ```
//!
//! (keys are `faderframe_ui_canvas::custom::COLORS` ids; hand-edited files
//! load as well). Loaded before the first window, registered with the
//! canvas toolkit, listed under View → Theme after the built-in skins.

use faderframe_ui_canvas::custom::{self, CustomTheme};
use gtk::gio;
use gtk::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Serialize, Deserialize)]
struct ThemeFile {
    name: String,
    #[serde(default = "default_base")]
    base: String,
    #[serde(default)]
    colors: BTreeMap<String, String>,
}

fn default_base() -> String {
    "studio".into()
}

/// Where the user's themes are.
pub fn dir() -> PathBuf {
    crate::paths::config_dir().join("themes")
}

fn file_of(slug: &str) -> PathBuf {
    dir().join(format!("{slug}.json"))
}

/// Every theme file (unreadable ones skipped with a warning), by name.
pub fn load() -> Vec<CustomTheme> {
    let Ok(entries) = std::fs::read_dir(dir()) else {
        return Vec::new();
    };
    let mut out: Vec<CustomTheme> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .filter_map(|path| {
            let slug = path.file_stem()?.to_string_lossy().into_owned();
            let text = std::fs::read_to_string(&path)
                .map_err(|e| tracing::warn!("theme {}: {e}", path.display()))
                .ok()?;
            let file: ThemeFile = serde_json::from_str(&text)
                .map_err(|e| tracing::warn!("theme {}: {e}", path.display()))
                .ok()?;
            let colors = file
                .colors
                .iter()
                .filter_map(
                    |(k, v)| match (custom::color_key(k), custom::parse_hex(v)) {
                        (Some(_), Some(c)) => Some((k.clone(), c)),
                        _ => {
                            tracing::warn!("theme {}: '{k}': '{v}' ignored", path.display());
                            None
                        }
                    },
                )
                .collect();
            Some(CustomTheme {
                slug,
                name: file.name,
                base: file.base,
                colors,
            })
        })
        .collect();
    out.sort_by_key(|t| t.name.to_lowercase());
    out
}

/// Load the files and register them (start-up, and after every change).
pub fn install() -> Vec<CustomTheme> {
    let all = load();
    custom::set_custom_themes(&all);
    all
}

/// Write a theme's file (and register them all again).
pub fn save(theme: &CustomTheme) -> std::io::Result<()> {
    std::fs::create_dir_all(dir())?;
    let file = ThemeFile {
        name: theme.name.clone(),
        base: theme.base.clone(),
        colors: theme
            .colors
            .iter()
            .map(|(k, c)| (k.clone(), custom::to_hex(*c)))
            .collect(),
    };
    let text = serde_json::to_string_pretty(&file).map_err(std::io::Error::other)?;
    std::fs::write(file_of(&theme.slug), text)?;
    install();
    Ok(())
}

/// Delete a theme's file (and register the rest again).
pub fn delete(slug: &str) -> std::io::Result<()> {
    std::fs::remove_file(file_of(slug))?;
    install();
    Ok(())
}

/// A file name for a theme called `name`, not taken yet.
pub fn new_slug(name: &str) -> String {
    let base: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let base = if base.is_empty() {
        "theme".into()
    } else {
        base
    };
    let mut slug = base.clone();
    let mut n = 2;
    while file_of(&slug).exists() {
        slug = format!("{base}-{n}");
        n += 1;
    }
    slug
}

/// A name for a new theme, not taken yet ("My Theme", "My Theme 2", …).
pub fn new_name(from: &str) -> String {
    let taken: Vec<String> = load().into_iter().map(|t| t.name).collect();
    let first = format!("My {from}");
    if !taken.contains(&first) {
        return first;
    }
    (2..)
        .map(|n| format!("My {from} {n}"))
        .find(|n| !taken.contains(n))
        .unwrap_or(first)
}

/// View → Theme's section of the user's themes.
pub fn rebuild_menu(menu: &gio::Menu) {
    menu.remove_all();
    for t in custom::custom_themes() {
        let item = gio::MenuItem::new(Some(&t.name.replace('_', "__")), None);
        item.set_action_and_target_value(Some("app.theme"), Some(&t.id.to_variant()));
        menu.append_item(&item);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_files_read_what_was_written() {
        let text = r##"{ "name": "Night", "base": "midnight",
            "colors": { "ui.accent": "#3fa9f5", "nonsense": "#ffffff", "ui.text": "red" } }"##;
        let file: ThemeFile = serde_json::from_str(text).unwrap();
        assert_eq!(file.base, "midnight");
        assert_eq!(file.colors.len(), 3);
        let minimal: ThemeFile = serde_json::from_str(r#"{ "name": "Plain" }"#).unwrap();
        assert_eq!(minimal.base, "studio");
        assert!(minimal.colors.is_empty());
    }
}
