//! Favorite plugins and the user's categories for them (issue #15).
//!
//! A user preference, not part of any project: changing favorites never
//! touches the project, its history or a plugin's state. The shell keeps
//! them in a file of their own in the configuration folder
//! ([`PluginFavorites::load`] / [`save`](PluginFavorites::save)) and hands
//! them to the [`Session`](crate::Session), whose views read them (the
//! plugin browser, the insert menus) and change them with
//! [`Action::PluginFavorites`](crate::Action::PluginFavorites).
//!
//! * A favorite is keyed by what never changes for an installed plugin:
//!   its format and the format's id (CLAP id, VST3 class id, LV2 URI,
//!   built-in id) — not its name. Each format of a plugin is a favorite of
//!   its own; [`PluginFavorites::other_formats`] finds the same plugin in
//!   other formats.
//! * Its name and vendor are remembered too: a favorite whose plugin is
//!   missing from the catalogue (uninstalled, moved, failed its scan)
//!   still shows by name — as missing — and stays a favorite until the
//!   user removes it. Rescans change nothing; renamed plugins update the
//!   remembered name ([`PluginFavorites::note_names`]).
//! * Categories have stable ids (renaming keeps the plugins in them); a
//!   plugin can be in several. Putting a plugin in a category makes it a
//!   favorite; removing a category leaves its plugins favorites;
//!   unfavoriting a plugin takes it out of every category.

use crate::AvailablePlugin;
use faderframe_project::{PluginFormat, PluginRef};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::Path;

/// The file's format version.
pub const VERSION: u32 = 1;

/// A category's id (stable across renames).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CategoryId(pub u32);

/// What identifies a plugin across rescans and renames.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FavoriteKey {
    pub format: PluginFormat,
    pub id: String,
}

impl FavoriteKey {
    pub fn of(plugin: &PluginRef) -> Self {
        Self {
            format: plugin.format,
            id: plugin.id.clone(),
        }
    }

    pub fn is(&self, plugin: &PluginRef) -> bool {
        self.format == plugin.format && self.id == plugin.id
    }
}

/// A plugin as an operation names it: the key, and the name and vendor
/// to remember (shown while the plugin is missing).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FavoritePlugin {
    pub key: FavoriteKey,
    pub name: String,
    pub vendor: String,
}

impl FavoritePlugin {
    pub fn of(p: &AvailablePlugin) -> Self {
        Self {
            key: FavoriteKey::of(&p.plugin),
            name: p.plugin.name.clone(),
            vendor: p.vendor.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Favorite {
    #[serde(flatten)]
    pub key: FavoriteKey,
    /// As last seen in the catalogue.
    pub name: String,
    #[serde(default)]
    pub vendor: String,
    #[serde(default)]
    pub categories: BTreeSet<CategoryId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Category {
    pub id: CategoryId,
    pub name: String,
}

/// Every favorite and category.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PluginFavorites {
    pub version: u32,
    favorites: Vec<Favorite>,
    categories: Vec<Category>,
    /// The next category's id.
    next_category: u32,
}

impl Default for PluginFavorites {
    fn default() -> Self {
        Self {
            version: VERSION,
            favorites: Vec::new(),
            categories: Vec::new(),
            next_category: 1,
        }
    }
}

/// Favorites as a menu shows them ([`PluginFavorites::menu`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FavoriteMenu {
    pub all: Vec<MenuEntry>,
    /// Category name → its favorites.
    pub categories: Vec<(String, Vec<MenuEntry>)>,
}

impl FavoriteMenu {
    pub fn is_empty(&self) -> bool {
        self.all.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum MenuEntry {
    Available(AvailablePlugin),
    /// Not in the catalogue now.
    Missing(Favorite),
}

impl MenuEntry {
    pub fn name(&self) -> &str {
        match self {
            Self::Available(p) => &p.plugin.name,
            Self::Missing(f) => &f.name,
        }
    }
}

/// A change to the favorites.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FavoriteOp {
    /// Make a plugin a favorite, or no longer one (out of every category
    /// too; missing plugins are removed this way).
    Set {
        plugin: FavoritePlugin,
        favorite: bool,
    },
    /// Put a plugin into a category (it becomes a favorite) or take it out
    /// (it stays a favorite).
    Assign {
        plugin: FavoritePlugin,
        category: CategoryId,
        member: bool,
    },
    /// A new category (an existing one of that name is used instead);
    /// `with` goes into it.
    NewCategory {
        name: String,
        with: Option<FavoritePlugin>,
    },
    RenameCategory {
        category: CategoryId,
        name: String,
    },
    /// Remove a category; its plugins stay favorites.
    RemoveCategory(CategoryId),
}

/// Why an operation was refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum FavoriteError {
    #[error("a category needs a name")]
    EmptyName,
    #[error("there is a category called “{0}” already")]
    NameTaken(String),
    #[error("that category no longer exists")]
    NoCategory,
}

fn same_name(a: &str, b: &str) -> bool {
    a.trim().to_lowercase() == b.trim().to_lowercase()
}

impl PluginFavorites {
    /// Favorites in the order they were made.
    pub fn favorites(&self) -> &[Favorite] {
        &self.favorites
    }

    /// Categories by name.
    pub fn categories(&self) -> Vec<&Category> {
        let mut c: Vec<&Category> = self.categories.iter().collect();
        c.sort_by_key(|c| c.name.to_lowercase());
        c
    }

    pub fn category(&self, id: CategoryId) -> Option<&Category> {
        self.categories.iter().find(|c| c.id == id)
    }

    pub fn favorite(&self, plugin: &PluginRef) -> Option<&Favorite> {
        self.favorites.iter().find(|f| f.key.is(plugin))
    }

    pub fn is_favorite(&self, plugin: &PluginRef) -> bool {
        self.favorite(plugin).is_some()
    }

    pub fn in_category(&self, plugin: &PluginRef, category: CategoryId) -> bool {
        self.favorite(plugin)
            .is_some_and(|f| f.categories.contains(&category))
    }

    /// The names of a plugin's categories (by name).
    pub fn category_names(&self, plugin: &PluginRef) -> Vec<String> {
        let Some(f) = self.favorite(plugin) else {
            return Vec::new();
        };
        self.categories()
            .into_iter()
            .filter(|c| f.categories.contains(&c.id))
            .map(|c| c.name.clone())
            .collect()
    }

    /// How many favorites a category holds.
    pub fn count_in(&self, category: CategoryId) -> usize {
        self.favorites
            .iter()
            .filter(|f| f.categories.contains(&category))
            .count()
    }

    /// Favorites whose plugin is not in `available` (uninstalled, moved,
    /// not scanned): shown as missing, never dropped by themselves.
    pub fn missing<'a>(&'a self, available: &[AvailablePlugin]) -> Vec<&'a Favorite> {
        self.favorites
            .iter()
            .filter(|f| !available.iter().any(|p| f.key.is(&p.plugin)))
            .collect()
    }

    /// The same plugin (name and vendor) in other formats than `key`'s.
    pub fn other_formats(
        key: &FavoriteKey,
        name: &str,
        vendor: &str,
        available: &[AvailablePlugin],
    ) -> Vec<PluginRef> {
        available
            .iter()
            .filter(|p| p.plugin.format != key.format)
            .filter(|p| same_name(&p.plugin.name, name) && same_name(&p.vendor, vendor))
            .map(|p| p.plugin.clone())
            .collect()
    }

    /// Take the catalogue's current names and vendors for favorites found
    /// in it (a renamed plugin); whether anything changed.
    pub fn note_names(&mut self, available: &[AvailablePlugin]) -> bool {
        let mut changed = false;
        for f in &mut self.favorites {
            if let Some(p) = available.iter().find(|p| f.key.is(&p.plugin))
                && (f.name != p.plugin.name || f.vendor != p.vendor)
            {
                f.name = p.plugin.name.clone();
                f.vendor = p.vendor.clone();
                changed = true;
            }
        }
        changed
    }

    /// What a menu of plugins `accept`ed shows as favorites: all of them
    /// by name, then each category with any (by name). Missing ones are
    /// listed (as missing) whatever `accept` says — what they are is not
    /// known any more.
    pub fn menu(
        &self,
        available: &[AvailablePlugin],
        accept: impl Fn(&AvailablePlugin) -> bool,
    ) -> FavoriteMenu {
        let entry = |f: &Favorite| match available.iter().find(|p| f.key.is(&p.plugin)) {
            Some(p) if accept(p) => Some(MenuEntry::Available(p.clone())),
            Some(_) => None,
            None => Some(MenuEntry::Missing(f.clone())),
        };
        let mut all: Vec<MenuEntry> = self.favorites.iter().filter_map(entry).collect();
        all.sort_by_key(|e| e.name().to_lowercase());
        let categories = self
            .categories()
            .into_iter()
            .filter_map(|c| {
                let mut members: Vec<MenuEntry> = self
                    .favorites
                    .iter()
                    .filter(|f| f.categories.contains(&c.id))
                    .filter_map(entry)
                    .collect();
                members.sort_by_key(|e| e.name().to_lowercase());
                (!members.is_empty()).then(|| (c.name.clone(), members))
            })
            .collect();
        FavoriteMenu { all, categories }
    }

    /// Apply `op`; whether anything changed.
    pub fn apply(&mut self, op: FavoriteOp) -> Result<bool, FavoriteError> {
        match op {
            FavoriteOp::Set { plugin, favorite } => Ok(if favorite {
                self.ensure(&plugin).1
            } else {
                let before = self.favorites.len();
                self.favorites.retain(|f| f.key != plugin.key);
                self.favorites.len() != before
            }),
            FavoriteOp::Assign {
                plugin,
                category,
                member,
            } => {
                if self.category(category).is_none() {
                    return Err(FavoriteError::NoCategory);
                }
                if member {
                    let (i, made) = self.ensure(&plugin);
                    Ok(self.favorites[i].categories.insert(category) || made)
                } else {
                    Ok(self
                        .favorites
                        .iter_mut()
                        .find(|f| f.key == plugin.key)
                        .is_some_and(|f| f.categories.remove(&category)))
                }
            }
            FavoriteOp::NewCategory { name, with } => {
                let name = name.trim().to_string();
                if name.is_empty() {
                    return Err(FavoriteError::EmptyName);
                }
                let (id, made) = match self.categories.iter().find(|c| same_name(&c.name, &name)) {
                    Some(c) => (c.id, false),
                    None => {
                        let id = CategoryId(self.next_category.max(1));
                        self.next_category = id.0 + 1;
                        self.categories.push(Category { id, name });
                        (id, true)
                    }
                };
                let assigned = match with {
                    Some(plugin) => self.apply(FavoriteOp::Assign {
                        plugin,
                        category: id,
                        member: true,
                    })?,
                    None => false,
                };
                Ok(made || assigned)
            }
            FavoriteOp::RenameCategory { category, name } => {
                let name = name.trim().to_string();
                if name.is_empty() {
                    return Err(FavoriteError::EmptyName);
                }
                if self
                    .categories
                    .iter()
                    .any(|c| c.id != category && same_name(&c.name, &name))
                {
                    return Err(FavoriteError::NameTaken(name));
                }
                let c = self
                    .categories
                    .iter_mut()
                    .find(|c| c.id == category)
                    .ok_or(FavoriteError::NoCategory)?;
                if c.name == name {
                    return Ok(false);
                }
                c.name = name;
                Ok(true)
            }
            FavoriteOp::RemoveCategory(category) => {
                let before = self.categories.len();
                self.categories.retain(|c| c.id != category);
                if self.categories.len() == before {
                    return Ok(false);
                }
                for f in &mut self.favorites {
                    f.categories.remove(&category);
                }
                Ok(true)
            }
        }
    }

    /// The plugin's favorite (made if needed): its index, and whether it
    /// was made.
    fn ensure(&mut self, plugin: &FavoritePlugin) -> (usize, bool) {
        if let Some(i) = self.favorites.iter().position(|f| f.key == plugin.key) {
            return (i, false);
        }
        self.favorites.push(Favorite {
            key: plugin.key.clone(),
            name: plugin.name.clone(),
            vendor: plugin.vendor.clone(),
            categories: BTreeSet::new(),
        });
        (self.favorites.len() - 1, true)
    }

    /// Read the favorites file. No file: none. A file that cannot be read
    /// is moved aside (`<name>.unreadable`, never overwritten unseen) and
    /// the reason returned with empty favorites.
    pub fn load(path: &Path) -> (Self, Option<String>) {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (Self::default(), None),
            Err(e) => return (Self::default(), Some(format!("{}: {e}", path.display()))),
        };
        match serde_json::from_str::<Self>(&text) {
            Ok(mut f) => {
                // Ids are never given twice, whatever the file says.
                let top = f.categories.iter().map(|c| c.id.0).max().unwrap_or(0);
                f.next_category = f.next_category.max(top + 1);
                // Assignments to categories that are gone.
                let known: BTreeSet<CategoryId> = f.categories.iter().map(|c| c.id).collect();
                for fav in &mut f.favorites {
                    fav.categories.retain(|c| known.contains(c));
                }
                (f, None)
            }
            Err(e) => {
                let aside = path.with_extension("json.unreadable");
                let _ = std::fs::rename(path, &aside);
                (
                    Self::default(),
                    Some(format!(
                        "{} could not be read ({e}); kept as {}",
                        path.display(),
                        aside.display()
                    )),
                )
            }
        }
    }

    /// Write the favorites file (whole or not at all: a temporary file
    /// renamed over it).
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn available(format: PluginFormat, id: &str, name: &str, vendor: &str) -> AvailablePlugin {
        AvailablePlugin {
            plugin: PluginRef {
                format,
                id: id.into(),
                name: name.into(),
            },
            vendor: vendor.into(),
            version: "1".into(),
            instrument: false,
            midi_effect: false,
            audio_inputs: 2,
            audio_outputs: 2,
            note_inputs: 0,
        }
    }

    fn catalogue() -> Vec<AvailablePlugin> {
        vec![
            available(
                PluginFormat::Clap,
                "com.tone3000.plugin",
                "TONE3000",
                "TONE3000",
            ),
            available(
                PluginFormat::Vst3,
                "ABCDEF019182FAEB5446544654334B30",
                "TONE3000",
                "TONE3000",
            ),
            available(PluginFormat::Clap, "com.u-he.Satin", "Satin", "u-he"),
            available(PluginFormat::Builtin, "faderframe.eq", "EQ", "FaderFrame"),
        ]
    }

    fn fav(p: &AvailablePlugin) -> FavoritePlugin {
        FavoritePlugin::of(p)
    }

    fn new_category(f: &mut PluginFavorites, name: &str) -> CategoryId {
        f.apply(FavoriteOp::NewCategory {
            name: name.into(),
            with: None,
        })
        .unwrap();
        f.categories()
            .into_iter()
            .find(|c| c.name == name)
            .unwrap()
            .id
    }

    #[test]
    fn favorites_are_set_and_unset_by_format_and_id() {
        let cat = catalogue();
        let mut f = PluginFavorites::default();
        assert!(
            f.apply(FavoriteOp::Set {
                plugin: fav(&cat[0]),
                favorite: true
            })
            .unwrap()
        );
        // Again: nothing changes.
        assert!(
            !f.apply(FavoriteOp::Set {
                plugin: fav(&cat[0]),
                favorite: true
            })
            .unwrap()
        );
        assert!(f.is_favorite(&cat[0].plugin));
        // The VST3 of the same plugin is a favorite of its own.
        assert!(!f.is_favorite(&cat[1].plugin));
        // A ref with another name (a renamed plugin) is the same favorite.
        let renamed = PluginRef {
            name: "TONE3000 Pro".into(),
            ..cat[0].plugin.clone()
        };
        assert!(f.is_favorite(&renamed));
        assert!(
            f.apply(FavoriteOp::Set {
                plugin: fav(&cat[0]),
                favorite: false
            })
            .unwrap()
        );
        assert!(!f.is_favorite(&cat[0].plugin));
    }

    #[test]
    fn categories_hold_several_plugins_and_a_plugin_several_categories() {
        let cat = catalogue();
        let mut f = PluginFavorites::default();
        let amps = new_category(&mut f, "Amps");
        let mix = new_category(&mut f, "Mixing");
        assert_ne!(amps, mix);
        // Into a category: a favorite at once.
        for c in [amps, mix] {
            f.apply(FavoriteOp::Assign {
                plugin: fav(&cat[0]),
                category: c,
                member: true,
            })
            .unwrap();
        }
        f.apply(FavoriteOp::Assign {
            plugin: fav(&cat[2]),
            category: mix,
            member: true,
        })
        .unwrap();
        assert!(f.is_favorite(&cat[0].plugin));
        assert_eq!(f.category_names(&cat[0].plugin), ["Amps", "Mixing"]);
        assert_eq!(f.count_in(mix), 2);
        assert_eq!(f.count_in(amps), 1);
        // Out of one: still in the other, still a favorite.
        f.apply(FavoriteOp::Assign {
            plugin: fav(&cat[0]),
            category: amps,
            member: false,
        })
        .unwrap();
        assert!(!f.in_category(&cat[0].plugin, amps));
        assert!(f.in_category(&cat[0].plugin, mix));
        assert!(f.is_favorite(&cat[0].plugin));
        // Renamed: same id, same members.
        f.apply(FavoriteOp::RenameCategory {
            category: mix,
            name: "Mix Bus".into(),
        })
        .unwrap();
        assert_eq!(f.category(mix).unwrap().name, "Mix Bus");
        assert_eq!(f.count_in(mix), 2);
        // Removed: its plugins stay favorites.
        assert!(f.apply(FavoriteOp::RemoveCategory(mix)).unwrap());
        assert!(f.category(mix).is_none());
        assert!(f.is_favorite(&cat[2].plugin));
        assert!(f.category_names(&cat[2].plugin).is_empty());
        // Ids are never given twice.
        let next = new_category(&mut f, "Later");
        assert!(next.0 > mix.0);
        // Unfavorited: out of every category.
        f.apply(FavoriteOp::Assign {
            plugin: fav(&cat[2]),
            category: amps,
            member: true,
        })
        .unwrap();
        f.apply(FavoriteOp::Set {
            plugin: fav(&cat[2]),
            favorite: false,
        })
        .unwrap();
        assert_eq!(f.count_in(amps), 0);
    }

    #[test]
    fn category_names_are_checked() {
        let cat = catalogue();
        let mut f = PluginFavorites::default();
        assert_eq!(
            f.apply(FavoriteOp::NewCategory {
                name: "  ".into(),
                with: None
            }),
            Err(FavoriteError::EmptyName)
        );
        let a = new_category(&mut f, "Amps");
        let b = new_category(&mut f, "Drums");
        // The same name (any case) is the same category; `with` goes in.
        f.apply(FavoriteOp::NewCategory {
            name: " amps ".into(),
            with: Some(fav(&cat[0])),
        })
        .unwrap();
        assert_eq!(f.categories().len(), 2);
        assert!(f.in_category(&cat[0].plugin, a));
        assert_eq!(
            f.apply(FavoriteOp::RenameCategory {
                category: b,
                name: "AMPS".into()
            }),
            Err(FavoriteError::NameTaken("AMPS".into()))
        );
        assert_eq!(
            f.apply(FavoriteOp::Assign {
                plugin: fav(&cat[0]),
                category: CategoryId(99),
                member: true
            }),
            Err(FavoriteError::NoCategory)
        );
        // Case changes of its own name are fine.
        assert!(
            f.apply(FavoriteOp::RenameCategory {
                category: a,
                name: "AMPS".into()
            })
            .unwrap()
        );
    }

    #[test]
    fn missing_plugins_stay_favorites_and_name_their_other_formats() {
        let cat = catalogue();
        let mut f = PluginFavorites::default();
        for p in [&cat[0], &cat[2]] {
            f.apply(FavoriteOp::Set {
                plugin: fav(p),
                favorite: true,
            })
            .unwrap();
        }
        assert!(f.missing(&cat).is_empty());
        // A rescan without the CLAP of TONE3000 (uninstalled).
        let rescanned: Vec<AvailablePlugin> = cat[1..].to_vec();
        assert!(!f.note_names(&rescanned));
        let missing = f.missing(&rescanned);
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].name, "TONE3000");
        assert!(f.is_favorite(&cat[0].plugin), "kept, not dropped");
        let others = PluginFavorites::other_formats(
            &missing[0].key,
            &missing[0].name,
            &missing[0].vendor,
            &rescanned,
        );
        assert_eq!(others.len(), 1);
        assert_eq!(others[0].format, PluginFormat::Vst3);
        // Back after the next rescan.
        assert!(f.missing(&cat).is_empty());
    }

    #[test]
    fn menus_list_favorites_by_name_and_category() {
        let mut cat = catalogue();
        cat[2].instrument = true;
        let mut f = PluginFavorites::default();
        let amps = new_category(&mut f, "Amps");
        let _empty = new_category(&mut f, "Empty");
        for p in [&cat[2], &cat[0], &cat[3]] {
            f.apply(FavoriteOp::Set {
                plugin: fav(p),
                favorite: true,
            })
            .unwrap();
        }
        f.apply(FavoriteOp::Assign {
            plugin: fav(&cat[0]),
            category: amps,
            member: true,
        })
        .unwrap();
        // An effects menu, with the CLAP of TONE3000 uninstalled since.
        let rescanned: Vec<AvailablePlugin> = cat[1..].to_vec();
        let menu = f.menu(&rescanned, |p| !p.instrument);
        let names: Vec<&str> = menu.all.iter().map(MenuEntry::name).collect();
        assert_eq!(
            names,
            ["EQ", "TONE3000"],
            "by name, the instrument left out"
        );
        assert!(matches!(menu.all[1], MenuEntry::Missing(_)));
        assert_eq!(menu.categories.len(), 1, "empty categories are left out");
        assert_eq!(menu.categories[0].0, "Amps");
        assert_eq!(menu.categories[0].1.len(), 1);
        assert!(PluginFavorites::default().menu(&cat, |_| true).is_empty());
    }

    #[test]
    fn renamed_plugins_update_the_remembered_name() {
        let mut cat = catalogue();
        let mut f = PluginFavorites::default();
        f.apply(FavoriteOp::Set {
            plugin: fav(&cat[2]),
            favorite: true,
        })
        .unwrap();
        cat[2].plugin.name = "Satin 2".into();
        assert!(f.note_names(&cat));
        assert_eq!(f.favorite(&cat[2].plugin).unwrap().name, "Satin 2");
        assert!(!f.note_names(&cat));
    }

    #[test]
    fn favorites_survive_a_save_and_a_load() {
        let dir = std::env::temp_dir().join(format!(
            "faderframe-favorites-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let path = dir.join("plugin-favorites.json");
        let _ = std::fs::remove_dir_all(&dir);
        // No file: no favorites, no complaint.
        let (none, warning) = PluginFavorites::load(&path);
        assert_eq!(none, PluginFavorites::default());
        assert!(warning.is_none());
        let cat = catalogue();
        let mut f = PluginFavorites::default();
        let amps = new_category(&mut f, "Amps");
        f.apply(FavoriteOp::Assign {
            plugin: fav(&cat[1]),
            category: amps,
            member: true,
        })
        .unwrap();
        f.apply(FavoriteOp::Set {
            plugin: fav(&cat[3]),
            favorite: true,
        })
        .unwrap();
        f.save(&path).unwrap();
        let (back, warning) = PluginFavorites::load(&path);
        assert!(warning.is_none());
        assert_eq!(back, f);
        assert!(back.in_category(&cat[1].plugin, amps));
        // Keyed by format and id in the file (readable, stable).
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"format\": \"vst3\""), "{text}");
        assert!(text.contains("ABCDEF019182FAEB5446544654334B30"));
        // An unreadable file is kept aside, not overwritten unseen.
        std::fs::write(&path, "{ not json").unwrap();
        let (empty, warning) = PluginFavorites::load(&path);
        assert_eq!(empty, PluginFavorites::default());
        assert!(warning.is_some());
        assert!(path.with_extension("json.unreadable").exists());
        // Files naming categories that are gone, or old id counters.
        std::fs::write(
            &path,
            r#"{"favorites":[{"format":"clap","id":"x","name":"X","categories":[7,9]}],
                "categories":[{"id":7,"name":"Seven"}],"next_category":2}"#,
        )
        .unwrap();
        let (mut odd, warning) = PluginFavorites::load(&path);
        assert!(warning.is_none());
        let x = PluginRef {
            format: PluginFormat::Clap,
            id: "x".into(),
            name: String::new(),
        };
        assert_eq!(odd.category_names(&x), ["Seven"]);
        let next = new_category(&mut odd, "Eight");
        assert_eq!(next, CategoryId(8));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
