//! The plugin browser: every available plugin (built-in and CLAP), with
//! search, an Effects/Instruments switch, format and vendor filters, a
//! detail panel, and one-click insertion into the track it was opened for.
//! Plugins from other makers show how they did in the realtime check
//! (`crate::plugin_check`): a badge per row, findings and advice in the
//! detail panel, a filter for those with findings.
//!
//! Favorites (issue #15): a star on every row and in the detail panel,
//! the user's categories (made in the sidebar or the detail panel, renamed
//! and removed from a category's right-click), and a sidebar section to
//! browse them. Favorites whose plugin is gone show as missing. They are
//! the user's (`faderframe_session::plugin_favorites`, saved by
//! `crate::favorites`), never the project's.

use crate::state::AppState;
use faderframe_core::TrackId;
use faderframe_project::{PluginFormat, PluginRef};
use faderframe_session::plugin_favorites::{
    CategoryId, FavoriteKey, FavoriteOp, FavoritePlugin, PluginFavorites,
};
use faderframe_session::{Action, AvailablePlugin, PluginTarget};
use gtk::glib;
use gtk::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

thread_local! {
    static OPEN: RefCell<Option<Rc<Browser>>> = const { RefCell::new(None) };
}

#[derive(Clone, Debug)]
struct Entry {
    plugin: PluginRef,
    vendor: String,
    version: String,
    instrument: bool,
    midi_effect: bool,
    features: Vec<String>,
    audio: String,
    midi: String,
    bundle: Option<String>,
    /// One of the user's favorites, in these categories.
    favorite: bool,
    categories: Vec<CategoryId>,
    category_names: Vec<String>,
    /// A favorite whose plugin is not in the catalogue (uninstalled,
    /// moved): shown by its remembered name; it cannot be inserted.
    missing: bool,
    /// The same plugin in other formats (named for missing favorites).
    other_formats: Vec<PluginRef>,
}

impl Entry {
    /// The plugin as favorite operations name it.
    fn favorite_plugin(&self) -> FavoritePlugin {
        FavoritePlugin {
            key: FavoriteKey::of(&self.plugin),
            name: self.plugin.name.clone(),
            vendor: if self.vendor == UNKNOWN_VENDOR {
                String::new()
            } else {
                self.vendor.clone()
            },
        }
    }

    /// How it did in the realtime check (this version).
    fn check(&self) -> Option<faderframe_plugin_check::CheckReport> {
        crate::plugin_check::report(&self.plugin, &self.version)
    }

    /// Checked (built-ins keep the rules by their own tests).
    fn checkable(&self) -> bool {
        !self.missing && self.plugin.format != PluginFormat::Builtin
    }

    fn kind_label(&self) -> &'static str {
        if self.missing {
            "—"
        } else if self.instrument {
            "Instrument"
        } else if self.midi_effect {
            "MIDI Effect"
        } else {
            "Effect"
        }
    }

    fn format_label(&self) -> &'static str {
        format_name(self.plugin.format)
    }

    fn matches(&self, text: &str) -> bool {
        if text.is_empty() {
            return true;
        }
        let hay = format!(
            "{} {} {} {} {}",
            self.plugin.name,
            self.vendor,
            self.format_label(),
            self.features.join(" "),
            self.category_names.join(" ")
        )
        .to_lowercase();
        text.to_lowercase()
            .split_whitespace()
            .all(|w| hay.contains(w))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    All,
    Effects,
    Instruments,
    MidiEffects,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Scope {
    All,
    /// Checked, with something found (or the check failed).
    Findings,
    /// Not checked yet (this version).
    Unchecked,
    Format(PluginFormat),
    Vendor(String),
    /// The user's favorites (missing ones too).
    Favorites,
    Category(CategoryId),
}

struct Filter {
    text: String,
    kind: Kind,
    scope: Scope,
}

impl Filter {
    fn accepts(&self, e: &Entry) -> bool {
        // What a missing favorite is, is not known any more.
        let kind = e.missing
            || match self.kind {
                Kind::All => true,
                Kind::Effects => !e.instrument && !e.midi_effect,
                Kind::Instruments => e.instrument,
                Kind::MidiEffects => e.midi_effect,
            };
        let scope = match &self.scope {
            // Missing favorites only where favorites are listed.
            _ if e.missing && !matches!(self.scope, Scope::Favorites | Scope::Category(_)) => false,
            Scope::All => true,
            Scope::Findings => e
                .check()
                .is_some_and(|r| r.verdict() >= faderframe_plugin_check::Verdict::Caution),
            Scope::Unchecked => e.checkable() && e.check().is_none(),
            Scope::Format(f) => e.plugin.format == *f,
            Scope::Vendor(v) => &e.vendor == v,
            Scope::Favorites => e.favorite,
            Scope::Category(c) => e.categories.contains(c),
        };
        kind && scope && e.matches(&self.text)
    }
}

struct Browser {
    window: gtk::Window,
    app: std::rc::Weak<AppState>,
    track: RefCell<TrackId>,
    target: RefCell<PluginTarget>,
    entries: RefCell<Vec<Entry>>,
    filter: RefCell<Filter>,
    list: gtk::ListBox,
    sidebar: gtk::ListBox,
    detail: gtk::Box,
    count: gtk::Label,
    subtitle: gtk::Label,
    /// The realtime check under way.
    check_status: gtk::Label,
    check_button: gtk::Button,
    /// The check reports last shown.
    check_generation: std::cell::Cell<u64>,
    search: gtk::SearchEntry,
    kind_buttons: [gtk::ToggleButton; 4],
    generation: std::cell::Cell<u64>,
    /// Scope of each sidebar row (`None` for headings).
    scopes: RefCell<Vec<Option<Scope>>>,
}

const UNKNOWN_VENDOR: &str = "Unknown vendor";

fn entries(app: &AppState) -> Vec<Entry> {
    let s = app.session.borrow();
    build_entries(s.available_plugins(), s.plugin_favorites(), |p| {
        crate::plugins::scanned(p.format, &p.id)
            .map(|c| (c.features, c.bundle.display().to_string()))
    })
}

/// The browser's rows: every available plugin, then the favorites missing
/// from the catalogue. `scanned` gives a hosted plugin's features and
/// bundle.
fn build_entries(
    available: Vec<AvailablePlugin>,
    favorites: &PluginFavorites,
    scanned: impl Fn(&PluginRef) -> Option<(Vec<String>, String)>,
) -> Vec<Entry> {
    let ch = |n: u16| match n {
        0 => "none".to_string(),
        1 => "mono".to_string(),
        2 => "stereo".to_string(),
        n => format!("{n} channels"),
    };
    let favorite_of = |plugin: &PluginRef| {
        let f = favorites.favorite(plugin);
        (
            f.is_some(),
            f.map(|f| f.categories.iter().copied().collect())
                .unwrap_or_default(),
            favorites.category_names(plugin),
        )
    };
    let mut out: Vec<Entry> = available
        .iter()
        .map(|p| {
            let scanned = scanned(&p.plugin);
            let (favorite, categories, category_names) = favorite_of(&p.plugin);
            Entry {
                vendor: if p.vendor.is_empty() {
                    UNKNOWN_VENDOR.into()
                } else {
                    p.vendor.clone()
                },
                version: p.version.clone(),
                instrument: p.instrument,
                midi_effect: p.midi_effect,
                features: scanned.as_ref().map(|(f, _)| f.clone()).unwrap_or_default(),
                audio: format!("{} in · {} out", ch(p.audio_inputs), ch(p.audio_outputs)),
                midi: if p.note_inputs > 0 {
                    "Note / MIDI input".into()
                } else {
                    "—".into()
                },
                bundle: scanned.map(|(_, b)| b),
                plugin: p.plugin.clone(),
                favorite,
                categories,
                category_names,
                missing: false,
                other_formats: Vec::new(),
            }
        })
        .collect();
    for f in favorites.missing(&available) {
        let plugin = PluginRef {
            format: f.key.format,
            id: f.key.id.clone(),
            name: f.name.clone(),
        };
        let (_, categories, category_names) = favorite_of(&plugin);
        out.push(Entry {
            vendor: if f.vendor.is_empty() {
                UNKNOWN_VENDOR.into()
            } else {
                f.vendor.clone()
            },
            version: String::new(),
            instrument: false,
            midi_effect: false,
            features: Vec::new(),
            audio: "—".into(),
            midi: "—".into(),
            bundle: None,
            other_formats: PluginFavorites::other_formats(&f.key, &f.name, &f.vendor, &available),
            plugin,
            favorite: true,
            categories,
            category_names,
            missing: true,
        });
    }
    out.sort_by(|a, b| {
        (
            a.plugin.format != PluginFormat::Builtin,
            a.plugin.name.to_lowercase(),
        )
            .cmp(&(
                b.plugin.format != PluginFormat::Builtin,
                b.plugin.name.to_lowercase(),
            ))
    });
    out
}

fn format_name(f: PluginFormat) -> &'static str {
    match f {
        PluginFormat::Builtin => "Built-in",
        PluginFormat::Clap => "CLAP",
        PluginFormat::Vst3 => "VST3",
        PluginFormat::AudioUnit => "AU",
        PluginFormat::Lv2 => "LV2",
    }
}

/// A colour for a vendor's avatar, stable per name.
fn avatar_class(vendor: &str) -> &'static str {
    let h = vendor
        .bytes()
        .fold(7u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32));
    ["av0", "av1", "av2", "av3", "av4", "av5"][(h % 6) as usize]
}

fn initials(name: &str) -> String {
    let mut s: String = name
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .take(2)
        .filter_map(|w| w.chars().next())
        .collect();
    if s.is_empty() {
        s.push('?');
    }
    s.to_uppercase()
}

fn badge(text: &str, class: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    l.add_css_class("badge");
    l.add_css_class(class);
    l.set_valign(gtk::Align::Center);
    l
}

/// The open browser (for buttons made inside its rows and panels).
fn open_browser() -> Option<std::rc::Weak<Browser>> {
    OPEN.with(|o| o.borrow().as_ref().map(Rc::downgrade))
}

/// A star that makes the plugin a favorite or no longer one.
fn star_button(e: &Entry) -> gtk::Button {
    let star = gtk::Button::with_label(if e.favorite { "★" } else { "☆" });
    star.add_css_class("flat");
    star.add_css_class("favorite-star");
    if e.favorite {
        star.add_css_class("starred");
    }
    star.set_valign(gtk::Align::Center);
    let tip = if e.favorite {
        "Remove from Favorites"
    } else {
        "Add to Favorites"
    };
    star.set_tooltip_text(Some(tip));
    star.update_property(&[gtk::accessible::Property::Label(tip)]);
    let (plugin, on) = (e.favorite_plugin(), !e.favorite);
    let weak = open_browser();
    star.connect_clicked(move |_| {
        if let Some(b) = weak.as_ref().and_then(|w| w.upgrade()) {
            b.change_favorites(FavoriteOp::Set {
                plugin: plugin.clone(),
                favorite: on,
            });
        }
    });
    star
}

fn row_widget(e: &Entry) -> gtk::Widget {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    row.add_css_class("plugin-row");
    if e.missing {
        row.add_css_class("plugin-missing");
    }
    row.append(&star_button(e));
    let avatar = gtk::Label::new(Some(&initials(&e.plugin.name)));
    avatar.add_css_class("avatar");
    avatar.add_css_class(avatar_class(&e.vendor));
    avatar.set_valign(gtk::Align::Center);
    row.append(&avatar);
    let text = gtk::Box::new(gtk::Orientation::Vertical, 1);
    text.set_hexpand(true);
    text.set_valign(gtk::Align::Center);
    let name = gtk::Label::new(Some(&e.plugin.name));
    name.add_css_class("plugin-name");
    name.set_xalign(0.0);
    name.set_ellipsize(gtk::pango::EllipsizeMode::End);
    let sub = gtk::Label::new(Some(&if e.version.is_empty() {
        e.vendor.clone()
    } else {
        format!("{} · {}", e.vendor, e.version)
    }));
    sub.add_css_class("plugin-sub");
    sub.set_xalign(0.0);
    sub.set_ellipsize(gtk::pango::EllipsizeMode::End);
    text.append(&name);
    text.append(&sub);
    row.append(&text);
    if e.missing {
        let b = badge("Missing", "badge-missing");
        b.set_tooltip_text(Some("Not found in the last scan: uninstalled or moved"));
        row.append(&b);
    } else {
        row.append(&badge(
            e.kind_label(),
            if e.instrument {
                "badge-instrument"
            } else if e.midi_effect {
                "badge-midi"
            } else {
                "badge-effect"
            },
        ));
    }
    row.append(&badge(
        e.format_label(),
        match e.plugin.format {
            PluginFormat::Builtin => "badge-builtin",
            PluginFormat::Clap => "badge-clap",
            PluginFormat::Vst3 => "badge-vst3",
            PluginFormat::Lv2 => "badge-lv2",
            _ => "badge-other",
        },
    ));
    if let Some(r) = e.check() {
        let v = r.verdict();
        let b = badge(verdict_mark(v), verdict_class(v));
        b.set_tooltip_text(Some(v.label()));
        row.append(&b);
    }
    row.upcast()
}

/// A verdict as a badge.
fn verdict_mark(v: faderframe_plugin_check::Verdict) -> &'static str {
    use faderframe_plugin_check::Verdict as V;
    match v {
        V::Clean => "✓ RT",
        V::Caution => "! RT",
        V::Problem => "✗ RT",
        V::Failed => "? RT",
    }
}

fn verdict_class(v: faderframe_plugin_check::Verdict) -> &'static str {
    use faderframe_plugin_check::Verdict as V;
    match v {
        V::Clean => "badge-rt-ok",
        V::Caution => "badge-rt-warn",
        V::Problem => "badge-rt-bad",
        V::Failed => "badge-rt-failed",
    }
}

/// The detail panel's realtime check: verdict, findings, advice, numbers.
fn check_section(e: &Entry, running: bool) -> gtk::Box {
    let section = gtk::Box::new(gtk::Orientation::Vertical, 6);
    section.add_css_class("check-section");
    let heading = gtk::Label::new(Some("Realtime check"));
    heading.add_css_class("check-heading");
    heading.set_xalign(0.0);
    section.append(&heading);
    let text = |t: &str, class: Option<&str>| {
        let l = gtk::Label::new(Some(t));
        l.set_xalign(0.0);
        l.set_wrap(true);
        // As wide as the panel (it must not widen it).
        l.set_width_chars(1);
        l.set_max_width_chars(30);
        if let Some(c) = class {
            l.add_css_class(c);
        }
        l
    };
    match e.check() {
        Some(r) => {
            let v = r.verdict();
            let verdict = text(v.label(), Some(verdict_class(v)));
            verdict.add_css_class("check-verdict");
            section.append(&verdict);
            for f in r.findings() {
                section.append(&text(&format!("• {}", f.text()), None));
            }
            section.append(&text(v.advice(), Some("dim-label")));
            if r.error.is_none() {
                let s = &r.steady;
                for line in [
                    format!(
                        "{} frames at {} Hz: {:.0} µs a block",
                        r.block,
                        r.sample_rate,
                        r.period_us()
                    ),
                    format!(
                        "Typically {:.0} µs, slowest {:.0} µs",
                        s.cpu_median_us, s.cpu_max_us
                    ),
                    format!(
                        "Slowest reset {:.0} µs",
                        r.reset.cpu_max_us.max(r.reset.wall_max_us)
                    ),
                ] {
                    section.append(&text(&line, Some("check-numbers")));
                }
                if s.allocations.is_none() || s.waits.is_none() {
                    section.append(&text(
                        "Allocations, waits and page faults are counted on Linux only: \
                         here the check measures the time per block and the reset.",
                        Some("check-numbers"),
                    ));
                }
            }
        }
        None if !e.checkable() => {
            section.append(&text(
                "FaderFrame's own devices keep the realtime rules (their tests prove it).",
                Some("dim-label"),
            ));
        }
        None => {
            section.append(&text(
                "Not checked yet. The check plays it for a few seconds in a helper process and \
                 counts what it does on the audio thread.",
                Some("dim-label"),
            ));
        }
    }
    if e.checkable() {
        let again = e.check().is_some();
        let button = gtk::Button::with_label(if running {
            "Checking…"
        } else if again {
            "Check Again"
        } else {
            "Check"
        });
        button.set_sensitive(!running);
        button.set_halign(gtk::Align::Start);
        let (plugin, version) = (e.plugin.clone(), e.version.clone());
        let weak = OPEN.with(|o| o.borrow().as_ref().map(Rc::downgrade));
        button.connect_clicked(move |_| {
            if let Some(b) = weak.as_ref().and_then(|w| w.upgrade())
                && let Some(app) = b.app.upgrade()
            {
                crate::plugin_check::check(&app, vec![(plugin.clone(), version.clone())]);
                b.follow_checks();
            }
        });
        section.append(&button);
    }
    section
}

/// Ask for a name in a popover under `parent` (made when shown, gone when
/// closed); Enter or the button hands it to `done`.
fn ask_name(
    parent: &impl IsA<gtk::Widget>,
    initial: &str,
    action: &str,
    extra: Option<gtk::Widget>,
    done: impl Fn(String) + 'static,
) -> gtk::Popover {
    let pop = gtk::Popover::new();
    pop.set_parent(parent);
    let column = gtk::Box::new(gtk::Orientation::Vertical, 6);
    column.set_margin_top(4);
    column.set_margin_bottom(4);
    column.set_margin_start(4);
    column.set_margin_end(4);
    let line = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let entry = gtk::Entry::new();
    entry.set_text(initial);
    entry.set_placeholder_text(Some("Category name"));
    entry.set_width_chars(18);
    let ok = gtk::Button::with_label(action);
    ok.add_css_class("suggested-action");
    line.append(&entry);
    line.append(&ok);
    column.append(&line);
    if let Some(w) = extra {
        column.append(&w);
    }
    pop.set_child(Some(&column));
    let done = Rc::new(done);
    let go = {
        let (entry, pop) = (entry.clone(), pop.clone());
        move || {
            let name = entry.text().to_string();
            pop.popdown();
            let done = Rc::clone(&done);
            // After the popover is gone (the sidebar is rebuilt).
            glib::idle_add_local_once(move || done(name));
        }
    };
    let again = go.clone();
    entry.connect_activate(move |_| again());
    ok.connect_clicked(move |_| go());
    pop.connect_closed(|p| {
        let p = p.clone();
        glib::idle_add_local_once(move || p.unparent());
    });
    pop.popup();
    entry.grab_focus();
    pop
}

impl Browser {
    /// Change the favorites (through the session, which the shell saves)
    /// and show them; the selected plugin stays selected.
    fn change_favorites(self: &Rc<Self>, op: FavoriteOp) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let keep = self.selected().map(|e| FavoriteKey::of(&e.plugin));
        app.dispatch(Action::PluginFavorites(op));
        *self.entries.borrow_mut() = entries(&app);
        self.refresh_sidebar();
        self.refresh_list_keeping(keep);
    }

    /// The rows again, the plugin `keep` selected if it is still shown.
    fn refresh_list_keeping(self: &Rc<Self>, keep: Option<FavoriteKey>) {
        self.refresh_list();
        let Some(keep) = keep else { return };
        let at = self
            .visible_entries()
            .iter()
            .position(|e| keep.is(&e.plugin));
        if let Some(row) = at.and_then(|i| self.list.row_at_index(i as i32)) {
            self.list.select_row(Some(&row));
        }
    }

    /// A category's sidebar row: right-click renames or removes it.
    fn category_menu(self: &Rc<Self>, row: &gtk::ListBoxRow, id: CategoryId, name: &str) {
        let click = gtk::GestureClick::new();
        click.set_button(gtk::gdk::BUTTON_SECONDARY);
        let weak = Rc::downgrade(self);
        let (row_w, name) = (row.downgrade(), name.to_string());
        click.connect_pressed(move |_, _, _, _| {
            let (Some(b), Some(row)) = (weak.upgrade(), row_w.upgrade()) else {
                return;
            };
            let remove = gtk::Button::with_label("Remove Category");
            remove.add_css_class("destructive-action");
            remove.set_tooltip_text(Some("Its plugins stay favorites"));
            let rename = Rc::downgrade(&b);
            let pop = ask_name(
                &row,
                &name,
                "Rename",
                Some(remove.clone().upcast()),
                move |n| {
                    if let Some(b) = rename.upgrade() {
                        b.change_favorites(FavoriteOp::RenameCategory {
                            category: id,
                            name: n,
                        });
                    }
                },
            );
            let weak = Rc::downgrade(&b);
            remove.connect_clicked(move |_| {
                pop.popdown();
                let weak = weak.clone();
                glib::idle_add_local_once(move || {
                    if let Some(b) = weak.upgrade() {
                        b.change_favorites(FavoriteOp::RemoveCategory(id));
                    }
                });
            });
        });
        row.add_controller(click);
    }

    /// The detail panel's favorites: the star, the categories, a new one.
    fn favorites_section(self: &Rc<Self>, e: &Entry) -> gtk::Box {
        let section = gtk::Box::new(gtk::Orientation::Vertical, 4);
        section.add_css_class("favorites-section");
        let plugin = e.favorite_plugin();
        let toggle = gtk::Button::with_label(if e.favorite {
            "★ Favorite"
        } else {
            "☆ Add to Favorites"
        });
        toggle.add_css_class("favorite-toggle");
        if e.favorite {
            toggle.add_css_class("starred");
            toggle.set_tooltip_text(Some("Remove from Favorites"));
        }
        toggle.set_halign(gtk::Align::Start);
        let weak = Rc::downgrade(self);
        let (p, on) = (plugin.clone(), !e.favorite);
        toggle.connect_clicked(move |_| {
            if let Some(b) = weak.upgrade() {
                b.change_favorites(FavoriteOp::Set {
                    plugin: p.clone(),
                    favorite: on,
                });
            }
        });
        section.append(&toggle);
        if e.missing {
            let text = if e.other_formats.is_empty() {
                "Not found in the last scan (uninstalled, moved, or it failed its scan). It \
                 stays a favorite until you remove it."
                    .to_string()
            } else {
                format!(
                    "Not found in the last scan; installed as {}. It stays a favorite until \
                     you remove it.",
                    e.other_formats
                        .iter()
                        .map(|p| format_name(p.format))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            let l = gtk::Label::new(Some(&text));
            l.set_xalign(0.0);
            l.set_wrap(true);
            l.set_width_chars(1);
            l.set_max_width_chars(30);
            l.add_css_class("dim-label");
            section.append(&l);
        }
        let favorites = self
            .app
            .upgrade()
            .map(|a| a.session.borrow().plugin_favorites().clone())
            .unwrap_or_default();
        for c in favorites.categories() {
            let check = gtk::CheckButton::with_label(&c.name);
            check.set_active(e.categories.contains(&c.id));
            let weak = Rc::downgrade(self);
            let (p, id) = (plugin.clone(), c.id);
            check.connect_toggled(move |c| {
                let member = c.is_active();
                let (weak, p) = (weak.clone(), p.clone());
                // After the toggle's own handling (the panel is rebuilt).
                glib::idle_add_local_once(move || {
                    if let Some(b) = weak.upgrade() {
                        b.change_favorites(FavoriteOp::Assign {
                            plugin: p,
                            category: id,
                            member,
                        });
                    }
                });
            });
            section.append(&check);
        }
        let new = gtk::Button::with_label("+ New Category…");
        new.add_css_class("flat");
        new.set_halign(gtk::Align::Start);
        new.set_tooltip_text(Some("A category of your own, with this plugin in it"));
        let weak = Rc::downgrade(self);
        new.connect_clicked(move |button| {
            let (weak, p) = (weak.clone(), plugin.clone());
            ask_name(button, "", "Create", None, move |name| {
                if let Some(b) = weak.upgrade() {
                    b.change_favorites(FavoriteOp::NewCategory {
                        name,
                        with: Some(p.clone()),
                    });
                }
            });
        });
        section.append(&new);
        section
    }

    fn selected(&self) -> Option<Entry> {
        let row = self.list.selected_row()?;
        let i = row.index();
        self.visible_entries().get(i as usize).cloned()
    }

    fn visible_entries(&self) -> Vec<Entry> {
        let f = self.filter.borrow();
        self.entries
            .borrow()
            .iter()
            .filter(|e| f.accepts(e))
            .cloned()
            .collect()
    }

    /// Rebuild the list from the filter (cheap: a few hundred rows at most).
    fn refresh_list(self: &Rc<Self>) {
        while let Some(c) = self.list.first_child() {
            self.list.remove(&c);
        }
        let visible = self.visible_entries();
        for e in &visible {
            self.list.append(&row_widget(e));
        }
        self.count.set_text(&match visible.len() {
            0 => "No plugins match".into(),
            1 => "1 plugin".into(),
            n => format!("{n} plugins"),
        });
        if let Some(first) = self.list.row_at_index(0) {
            self.list.select_row(Some(&first));
        } else {
            self.show_detail(None);
        }
    }

    fn refresh_sidebar(self: &Rc<Self>) {
        while let Some(c) = self.sidebar.first_child() {
            self.sidebar.remove(&c);
        }
        let entries = self.entries.borrow();
        let mut scopes = self.scopes.borrow_mut();
        scopes.clear();
        let heading = |t: &str| {
            let l = gtk::Label::new(Some(t));
            l.add_css_class("sidebar-heading");
            l.set_xalign(0.0);
            let r = gtk::ListBoxRow::new();
            r.set_child(Some(&l));
            r.set_selectable(false);
            r.set_activatable(false);
            r
        };
        let item = |label: &str, n: usize, scope: Scope| {
            let b = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            let l = gtk::Label::new(Some(label));
            l.set_xalign(0.0);
            l.set_hexpand(true);
            l.set_ellipsize(gtk::pango::EllipsizeMode::End);
            let c = gtk::Label::new(Some(&n.to_string()));
            c.add_css_class("sidebar-count");
            b.append(&l);
            b.append(&c);
            b.add_css_class("sidebar-item");
            let r = gtk::ListBoxRow::new();
            r.set_child(Some(&b));
            (r, scope)
        };
        self.sidebar.append(&heading("LIBRARY"));
        scopes.push(None);
        let installed = entries.iter().filter(|e| !e.missing).count();
        let (all, scope) = item("All plugins", installed, Scope::All);
        self.sidebar.append(&all);
        scopes.push(Some(scope));
        let checked: Vec<_> = entries.iter().map(|e| (e, e.check())).collect();
        let findings = checked
            .iter()
            .filter(|(_, r)| {
                r.as_ref()
                    .is_some_and(|r| r.verdict() >= faderframe_plugin_check::Verdict::Caution)
            })
            .count();
        let unchecked = checked
            .iter()
            .filter(|(e, r)| !e.missing && e.checkable() && r.is_none())
            .count();
        let (r, scope) = item("With findings", findings, Scope::Findings);
        self.sidebar.append(&r);
        scopes.push(Some(scope));
        let (r, scope) = item("Not checked", unchecked, Scope::Unchecked);
        self.sidebar.append(&r);
        scopes.push(Some(scope));
        for (f, label) in [
            (PluginFormat::Builtin, "Built-in"),
            (PluginFormat::Clap, "CLAP"),
            (PluginFormat::Vst3, "VST3"),
            (PluginFormat::Lv2, "LV2"),
            (PluginFormat::AudioUnit, "AU"),
        ] {
            let n = entries
                .iter()
                .filter(|e| !e.missing && e.plugin.format == f)
                .count();
            if n == 0 && matches!(f, PluginFormat::Lv2 | PluginFormat::AudioUnit) {
                continue;
            }
            let (r, scope) = item(label, n, Scope::Format(f));
            self.sidebar.append(&r);
            scopes.push(Some(scope));
        }
        // The user's favorites and categories.
        self.sidebar.append(&heading("FAVORITES"));
        scopes.push(None);
        let n = entries.iter().filter(|e| e.favorite).count();
        let (r, scope) = item("All favorites", n, Scope::Favorites);
        self.sidebar.append(&r);
        scopes.push(Some(scope));
        let favorites = self
            .app
            .upgrade()
            .map(|a| a.session.borrow().plugin_favorites().clone())
            .unwrap_or_default();
        for c in favorites.categories() {
            let n = entries
                .iter()
                .filter(|e| e.categories.contains(&c.id))
                .count();
            let (r, scope) = item(&c.name, n, Scope::Category(c.id));
            r.set_tooltip_text(Some("Right-click to rename or remove"));
            self.category_menu(&r, c.id, &c.name);
            self.sidebar.append(&r);
            scopes.push(Some(scope));
        }
        let new = gtk::Button::with_label("+ New Category…");
        new.add_css_class("flat");
        new.add_css_class("sidebar-new");
        new.set_halign(gtk::Align::Start);
        let weak = Rc::downgrade(self);
        new.connect_clicked(move |button| {
            let weak = weak.clone();
            ask_name(button, "", "Create", None, move |name| {
                if let Some(b) = weak.upgrade() {
                    b.change_favorites(FavoriteOp::NewCategory { name, with: None });
                }
            });
        });
        let r = gtk::ListBoxRow::new();
        r.set_child(Some(&new));
        r.set_selectable(false);
        r.set_activatable(false);
        self.sidebar.append(&r);
        scopes.push(None);
        let mut vendors: Vec<(String, usize)> = Vec::new();
        for e in entries.iter().filter(|e| !e.missing) {
            match vendors.iter_mut().find(|(v, _)| *v == e.vendor) {
                Some((_, n)) => *n += 1,
                None => vendors.push((e.vendor.clone(), 1)),
            }
        }
        vendors.sort_by_key(|(v, _)| v.to_lowercase());
        if !vendors.is_empty() {
            self.sidebar.append(&heading("VENDORS"));
            scopes.push(None);
        }
        for (v, n) in vendors {
            let (r, scope) = item(&v, n, Scope::Vendor(v.clone()));
            self.sidebar.append(&r);
            scopes.push(Some(scope));
        }
        // The scope shown stays selected (its count may have changed).
        let current = self.filter.borrow().scope.clone();
        let at = scopes.iter().position(|s| s.as_ref() == Some(&current));
        drop(scopes);
        drop(entries);
        match at.and_then(|i| self.sidebar.row_at_index(i as i32)) {
            Some(row) => self.sidebar.select_row(Some(&row)),
            None => self.sidebar.select_row(Some(&all)),
        }
    }

    fn show_detail(self: &Rc<Self>, e: Option<&Entry>) {
        while let Some(c) = self.detail.first_child() {
            self.detail.remove(&c);
        }
        let Some(e) = e else {
            let l = gtk::Label::new(Some("Select a plugin"));
            l.add_css_class("dim-label");
            l.set_vexpand(true);
            self.detail.append(&l);
            return;
        };
        let avatar = gtk::Label::new(Some(&initials(&e.plugin.name)));
        avatar.add_css_class("avatar");
        avatar.add_css_class("avatar-large");
        avatar.add_css_class(avatar_class(&e.vendor));
        avatar.set_halign(gtk::Align::Start);
        self.detail.append(&avatar);
        let title = gtk::Label::new(Some(&e.plugin.name));
        title.add_css_class("detail-title");
        title.set_xalign(0.0);
        title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        title.set_max_width_chars(22);
        self.detail.append(&title);
        let vendor = gtk::Label::new(Some(&e.vendor));
        vendor.add_css_class("plugin-sub");
        vendor.set_xalign(0.0);
        self.detail.append(&vendor);
        self.detail.append(&self.favorites_section(e));
        if !e.features.is_empty() {
            // (A grid: FlowBox mis-measures in this pane.)
            let chips = gtk::Grid::new();
            chips.set_row_spacing(4);
            chips.set_column_spacing(4);
            for (i, f) in e.features.iter().enumerate() {
                let c = gtk::Label::new(Some(f));
                c.add_css_class("chip");
                chips.attach(&c, (i % 3) as i32, (i / 3) as i32, 1, 1);
            }
            self.detail.append(&chips);
        }
        let grid = gtk::Grid::new();
        grid.set_row_spacing(6);
        grid.set_column_spacing(12);
        grid.add_css_class("detail-grid");
        let mut y = 0;
        let mut row = |k: &str, v: &str| {
            let kl = gtk::Label::new(Some(k));
            kl.add_css_class("dim-label");
            kl.set_xalign(0.0);
            let vl = gtk::Label::new(Some(v));
            vl.set_xalign(0.0);
            vl.set_selectable(true);
            vl.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
            vl.set_max_width_chars(32);
            vl.set_tooltip_text(Some(v));
            grid.attach(&kl, 0, y, 1, 1);
            grid.attach(&vl, 1, y, 1, 1);
            y += 1;
        };
        row("Format", e.format_label());
        if e.missing {
            row("ID", &e.plugin.id);
        }
        if !e.version.is_empty() {
            row("Version", &e.version);
        }
        row("Type", e.kind_label());
        row("Audio", &e.audio);
        row("MIDI", &e.midi);
        if let Some(b) = &e.bundle {
            row("Bundle", b);
        }
        self.detail.append(&grid);
        if !e.missing {
            self.detail
                .append(&check_section(e, crate::plugin_check::progress().is_some()));
        }
        let spacer = gtk::Box::new(gtk::Orientation::Vertical, 0);
        spacer.set_vexpand(true);
        self.detail.append(&spacer);
        let target = *self.target.borrow();
        let track_name = self
            .app
            .upgrade()
            .map(|a| target_name(&a, *self.track.borrow(), target))
            .unwrap_or_default();
        let label = match target {
            PluginTarget::Instrument => format!("Use as Instrument on {track_name}"),
            PluginTarget::Insert(_) | PluginTarget::Song(_) => format!("Insert on {track_name}"),
        };
        let place = gtk::Button::with_label(&label);
        place.add_css_class("suggested-action");
        place.add_css_class("place-button");
        let wrong_kind = matches!(target, PluginTarget::Instrument) && !e.instrument;
        place.set_sensitive(!wrong_kind && !e.missing);
        if e.missing {
            place.set_label("Not installed");
        }
        let plugin = e.plugin.clone();
        let weak = OPEN.with(|o| o.borrow().as_ref().map(Rc::downgrade));
        place.connect_clicked(move |_| {
            if let Some(b) = weak.as_ref().and_then(|w| w.upgrade()) {
                b.place(plugin.clone());
            }
        });
        self.detail.append(&place);
    }

    fn place(&self, plugin: PluginRef) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let missing = self
            .entries
            .borrow()
            .iter()
            .any(|e| e.missing && e.plugin == plugin);
        if missing {
            return;
        }
        let (track, target) = (*self.track.borrow(), *self.target.borrow());
        if app
            .with_session(|s| s.place_plugin(track, target, plugin))
            .is_some()
        {
            self.window.close();
        }
    }

    /// Show the check under way (and new reports) as they come.
    fn follow_checks(self: &Rc<Self>) {
        let g = crate::plugin_check::generation();
        if g != self.check_generation.get() {
            self.check_generation.set(g);
            self.refresh_sidebar_counts();
            // Rows and the detail show the new reports; the selection stays.
            let selected = self.list.selected_row().map(|r| r.index());
            let visible = self.visible_entries();
            while let Some(c) = self.list.first_child() {
                self.list.remove(&c);
            }
            for e in &visible {
                self.list.append(&row_widget(e));
            }
            if let Some(r) = selected.and_then(|i| self.list.row_at_index(i)) {
                self.list.select_row(Some(&r));
            }
            let e = self.selected();
            self.show_detail(e.as_ref());
        }
        match crate::plugin_check::progress() {
            Some((done, total, current)) => {
                self.check_status.set_text(&format!(
                    "Checking {} of {total}{}",
                    (done + 1).min(total),
                    if current.is_empty() {
                        String::new()
                    } else {
                        format!(": {current}")
                    }
                ));
                self.check_status.set_visible(true);
                self.check_button.set_sensitive(false);
            }
            None => {
                self.check_status.set_visible(false);
                self.check_button.set_sensitive(true);
            }
        }
    }

    /// The sidebar again (its counts), keeping what it shows.
    fn refresh_sidebar_counts(&self) {
        if let Some(b) = OPEN.with(|o| o.borrow().clone()) {
            b.refresh_sidebar();
        }
    }

    /// Check every plugin from another maker not checked in its version.
    fn check_all(self: &Rc<Self>) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let todo: Vec<_> = self
            .entries
            .borrow()
            .iter()
            .filter(|e| e.checkable() && e.check().is_none())
            .map(|e| (e.plugin.clone(), e.version.clone()))
            .collect();
        if todo.is_empty() {
            self.check_status.set_text("Every plugin is checked");
            self.check_status.set_visible(true);
            return;
        }
        crate::plugin_check::check(&app, todo);
        self.follow_checks();
    }

    fn set_kind(self: &Rc<Self>, kind: Kind) {
        self.filter.borrow_mut().kind = kind;
        for (b, k) in self.kind_buttons.iter().zip([
            Kind::All,
            Kind::Effects,
            Kind::Instruments,
            Kind::MidiEffects,
        ]) {
            b.set_active(k == kind);
        }
        self.refresh_list();
    }

    fn retarget(self: &Rc<Self>, track: TrackId, target: PluginTarget) {
        *self.track.borrow_mut() = track;
        *self.target.borrow_mut() = target;
        let name = self
            .app
            .upgrade()
            .map(|a| target_name(&a, track, target))
            .unwrap_or_default();
        self.subtitle.set_text(&match target {
            PluginTarget::Instrument => format!("Choose an instrument for {name}"),
            PluginTarget::Insert(i) => format!("Insert slot {} on {name}", i + 1),
            PluginTarget::Song(_) => format!("Add an insert to {name}"),
        });
        // MIDI effects on a MIDI track, and in a slot before an instrument
        // (an audio effect there would be replaced by the instrument).
        let midi = match target {
            PluginTarget::Insert(i) => self.app.upgrade().is_some_and(|a| {
                let s = a.session.borrow();
                s.project().track(track).is_some_and(|t| match t.kind {
                    faderframe_project::TrackKind::Midi => true,
                    faderframe_project::TrackKind::Instrument => t
                        .inserts
                        .iter()
                        .position(|x| s.engine_plugin_is_instrument(x.id))
                        .is_some_and(|first| i <= first),
                    _ => false,
                })
            }),
            _ => false,
        };
        self.set_kind(match target {
            PluginTarget::Instrument => Kind::Instruments,
            PluginTarget::Insert(_) if midi => Kind::MidiEffects,
            PluginTarget::Insert(_) | PluginTarget::Song(_) => Kind::Effects,
        });
    }
}

/// What a plugin goes onto: the track's name, or the album song's.
fn target_name(app: &AppState, track: TrackId, target: PluginTarget) -> String {
    let Ok(s) = app.session.try_borrow() else {
        return String::new();
    };
    match target {
        PluginTarget::Song(song) => s
            .project()
            .album
            .song(song)
            .map(|x| format!("the song “{}”", x.title))
            .unwrap_or_default(),
        _ => s
            .project()
            .track(track)
            .map(|t| t.name.clone())
            .unwrap_or_default(),
    }
}

/// Search the open browser for `text` (scripts).
pub fn search(text: &str) {
    if let Some(b) = OPEN.with(|o| o.borrow().clone()) {
        b.search.set_text(text);
    }
}

/// Show a sidebar entry of the open browser by its label ("all",
/// "favorites", a category's or vendor's name; scripts).
pub fn show_scope(name: &str) {
    let Some(b) = OPEN.with(|o| o.borrow().clone()) else {
        return;
    };
    let wanted = name.trim().to_lowercase();
    let favorites = b
        .app
        .upgrade()
        .map(|a| a.session.borrow().plugin_favorites().clone())
        .unwrap_or_default();
    let at = b.scopes.borrow().iter().position(|s| match s {
        Some(Scope::All) => wanted == "all",
        Some(Scope::Favorites) => wanted == "favorites",
        Some(Scope::Category(c)) => favorites
            .category(*c)
            .is_some_and(|c| c.name.to_lowercase() == wanted),
        Some(Scope::Vendor(v)) => v.to_lowercase() == wanted,
        _ => false,
    });
    match at.and_then(|i| b.sidebar.row_at_index(i as i32)) {
        Some(row) => b.sidebar.select_row(Some(&row)),
        None => tracing::warn!("plugin-browser-scope: no '{name}'"),
    }
}

/// Open (or bring up and retarget) the plugin browser.
pub fn open(app: &Rc<AppState>, track: TrackId, target: PluginTarget) {
    if let Some(b) = OPEN.with(|o| o.borrow().clone()) {
        b.retarget(track, target);
        b.window.present();
        b.search.grab_focus();
        return;
    }
    let window = gtk::Window::builder()
        .application(&app.app)
        .title("Plugins — FaderFrame")
        .default_width(1020)
        .default_height(640)
        .build();
    window.add_css_class("plugin-browser");
    if let Some(main) = app.window.borrow().as_ref() {
        window.set_transient_for(Some(main));
    }

    let header = gtk::HeaderBar::new();
    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some("Search plugins, vendors, features…"));
    search.set_width_request(380);
    header.set_title_widget(Some(&search));
    let kinds = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    kinds.add_css_class("linked");
    let kind_buttons = ["All", "Effects", "Instruments", "MIDI Effects"].map(|l| {
        let b = gtk::ToggleButton::with_label(l);
        kinds.append(&b);
        b
    });
    header.pack_start(&kinds);
    let rescan = gtk::Button::from_icon_name("view-refresh-symbolic");
    rescan.set_tooltip_text(Some(if cfg!(target_os = "linux") {
        "Rescan the CLAP, VST3 and LV2 folders"
    } else {
        "Rescan the CLAP and VST3 folders"
    }));
    header.pack_end(&rescan);
    let check_button = gtk::Button::with_label("Check");
    check_button.set_tooltip_text(Some(
        "Check the plugins not checked yet: each plays for a few seconds in a helper process \
         while what it does on the audio thread is counted (allocations, waits, page faults, \
         time per block, reset)",
    ));
    header.pack_end(&check_button);
    window.set_titlebar(Some(&header));

    let sidebar = gtk::ListBox::new();
    sidebar.add_css_class("plugin-sidebar");
    sidebar.set_selection_mode(gtk::SelectionMode::Single);
    let side_scroll = gtk::ScrolledWindow::builder()
        .child(&sidebar)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .width_request(210)
        .build();

    let list = gtk::ListBox::new();
    list.add_css_class("plugin-list");
    list.set_selection_mode(gtk::SelectionMode::Single);
    list.set_activate_on_single_click(false);
    let list_scroll = gtk::ScrolledWindow::builder()
        .child(&list)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .hexpand(true)
        .build();
    let subtitle = gtk::Label::new(None);
    subtitle.add_css_class("browser-subtitle");
    subtitle.set_xalign(0.0);
    let count = gtk::Label::new(None);
    count.add_css_class("dim-label");
    count.set_xalign(1.0);
    let check_status = gtk::Label::new(None);
    check_status.add_css_class("check-status");
    check_status.set_visible(false);
    check_status.set_ellipsize(gtk::pango::EllipsizeMode::End);
    check_status.set_width_chars(26);
    check_status.set_xalign(1.0);
    let top = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    top.add_css_class("browser-top");
    subtitle.set_hexpand(true);
    top.append(&subtitle);
    top.append(&check_status);
    top.append(&count);
    let center = gtk::Box::new(gtk::Orientation::Vertical, 0);
    center.append(&top);
    center.append(&list_scroll);
    list_scroll.set_vexpand(true);

    let detail = gtk::Box::new(gtk::Orientation::Vertical, 10);
    detail.add_css_class("plugin-detail");
    detail.set_width_request(310);

    let body = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    body.append(&side_scroll);
    body.append(&gtk::Separator::new(gtk::Orientation::Vertical));
    body.append(&center);
    body.append(&gtk::Separator::new(gtk::Orientation::Vertical));
    body.append(&detail);
    window.set_child(Some(&body));

    let b = Rc::new(Browser {
        window: window.clone(),
        app: Rc::downgrade(app),
        track: RefCell::new(track),
        target: RefCell::new(target),
        entries: RefCell::new(entries(app)),
        filter: RefCell::new(Filter {
            text: String::new(),
            kind: Kind::Effects,
            scope: Scope::All,
        }),
        list: list.clone(),
        sidebar: sidebar.clone(),
        detail,
        count,
        subtitle,
        check_status,
        check_button: check_button.clone(),
        check_generation: std::cell::Cell::new(crate::plugin_check::generation()),
        search: search.clone(),
        kind_buttons: kind_buttons.clone(),
        generation: std::cell::Cell::new(crate::plugins::catalog_generation()),
        scopes: RefCell::new(Vec::new()),
    });
    OPEN.with(|o| *o.borrow_mut() = Some(Rc::clone(&b)));

    // Wiring.
    let weak = Rc::downgrade(&b);
    search.connect_search_changed(move |s| {
        if let Some(b) = weak.upgrade() {
            b.filter.borrow_mut().text = s.text().to_string();
            b.refresh_list();
        }
    });
    let weak = Rc::downgrade(&b);
    search.connect_activate(move |_| {
        if let Some(b) = weak.upgrade()
            && let Some(e) = b.selected()
        {
            b.place(e.plugin);
        }
    });
    for (button, kind) in kind_buttons.iter().zip([
        Kind::All,
        Kind::Effects,
        Kind::Instruments,
        Kind::MidiEffects,
    ]) {
        let weak = Rc::downgrade(&b);
        button.connect_clicked(move |btn| {
            if let Some(b) = weak.upgrade() {
                if !btn.is_active() && b.filter.borrow().kind == kind {
                    btn.set_active(true);
                    return;
                }
                if b.filter.borrow().kind != kind {
                    b.set_kind(kind);
                }
            }
        });
    }
    let weak = Rc::downgrade(&b);
    sidebar.connect_row_selected(move |_, row| {
        let (Some(b), Some(row)) = (weak.upgrade(), row) else {
            return;
        };
        let scope = b
            .scopes
            .borrow()
            .get(row.index() as usize)
            .cloned()
            .flatten();
        if let Some(scope) = scope {
            b.filter.borrow_mut().scope = scope;
            b.refresh_list();
        }
    });
    let weak = Rc::downgrade(&b);
    list.connect_row_selected(move |_, _| {
        if let Some(b) = weak.upgrade() {
            let e = b.selected();
            b.show_detail(e.as_ref());
        }
    });
    let weak = Rc::downgrade(&b);
    list.connect_row_activated(move |_, _| {
        if let Some(b) = weak.upgrade()
            && let Some(e) = b.selected()
        {
            b.place(e.plugin);
        }
    });
    let weak = Rc::downgrade(&b);
    check_button.connect_clicked(move |_| {
        if let Some(b) = weak.upgrade() {
            b.check_all();
        }
    });
    let weak_app = Rc::downgrade(app);
    rescan.connect_clicked(move |_| {
        if let Some(app) = weak_app.upgrade() {
            *app.plugin_scan.borrow_mut() = Some(crate::plugins::scan_in_background());
            app.session
                .borrow_mut()
                .notify(faderframe_session::NoticeLevel::Info, "rescanning plugins…");
        }
    });
    // Escape closes; arrow down from the search moves into the list.
    let keys = gtk::EventControllerKey::new();
    let weak = Rc::downgrade(&b);
    keys.connect_key_pressed(move |_, key, _, _| {
        let Some(b) = weak.upgrade() else {
            return glib::Propagation::Proceed;
        };
        match key {
            gtk::gdk::Key::Escape => {
                b.window.close();
                glib::Propagation::Stop
            }
            gtk::gdk::Key::Down if b.search.has_focus() => {
                if let Some(r) = b.list.selected_row().or_else(|| b.list.row_at_index(0)) {
                    r.grab_focus();
                }
                glib::Propagation::Stop
            }
            _ => glib::Propagation::Proceed,
        }
    });
    window.add_controller(keys);
    // Pick up a finished rescan.
    let weak = Rc::downgrade(&b);
    glib::timeout_add_local(std::time::Duration::from_millis(500), move || {
        let Some(b) = weak.upgrade() else {
            return glib::ControlFlow::Break;
        };
        let g = crate::plugins::catalog_generation();
        if g != b.generation.get()
            && let Some(app) = b.app.upgrade()
        {
            b.generation.set(g);
            // Favorites take renamed plugins' new names (saved).
            if let Ok(mut s) = app.session.try_borrow_mut() {
                s.note_favorite_names();
            }
            crate::favorites::follow(&app);
            *b.entries.borrow_mut() = entries(&app);
            b.refresh_sidebar();
            b.refresh_list();
        }
        b.follow_checks();
        glib::ControlFlow::Continue
    });
    window.connect_close_request(|_| {
        OPEN.with(|o| o.borrow_mut().take());
        glib::Propagation::Proceed
    });

    b.refresh_sidebar();
    b.retarget(track, target);
    window.present();
    search.grab_focus();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plugin(format: PluginFormat, id: &str, name: &str, vendor: &str) -> AvailablePlugin {
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
        let mut diva = plugin(PluginFormat::Clap, "com.u-he.Diva", "Diva", "u-he");
        diva.instrument = true;
        vec![
            plugin(
                PluginFormat::Clap,
                "com.tone3000.plugin",
                "TONE3000",
                "TONE3000",
            ),
            plugin(PluginFormat::Vst3, "ABCD", "TONE3000", "TONE3000"),
            plugin(PluginFormat::Clap, "com.u-he.Satin", "Satin", "u-he"),
            diva,
        ]
    }

    fn filter(scope: Scope, kind: Kind, text: &str) -> Filter {
        Filter {
            text: text.into(),
            kind,
            scope,
        }
    }

    fn names(entries: &[Entry], f: &Filter) -> Vec<String> {
        entries
            .iter()
            .filter(|e| f.accepts(e))
            .map(|e| format!("{} {}", e.plugin.name, e.format_label()))
            .collect()
    }

    #[test]
    fn favorites_and_categories_filter_the_list_and_missing_ones_show_there_only() {
        let cat = catalogue();
        let mut favs = PluginFavorites::default();
        favs.apply(FavoriteOp::NewCategory {
            name: "Amps".into(),
            with: Some(FavoritePlugin::of(&cat[0])),
        })
        .unwrap();
        let amps = favs.categories()[0].id;
        for p in [&cat[2], &cat[3]] {
            favs.apply(FavoriteOp::Set {
                plugin: FavoritePlugin::of(p),
                favorite: true,
            })
            .unwrap();
        }
        // The CLAP of TONE3000 uninstalled since: its VST3 is still there.
        let installed: Vec<AvailablePlugin> = cat[1..].to_vec();
        let entries = build_entries(installed, &favs, |_| None);
        assert_eq!(entries.len(), 4, "three installed and one missing");
        let missing: Vec<&Entry> = entries.iter().filter(|e| e.missing).collect();
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].plugin.name, "TONE3000");
        assert_eq!(missing[0].other_formats.len(), 1, "named: the VST3");
        assert_eq!(missing[0].category_names, ["Amps"]);

        // The library leaves out what is not installed…
        assert_eq!(
            names(&entries, &filter(Scope::All, Kind::All, "")),
            ["Diva CLAP", "Satin CLAP", "TONE3000 VST3"]
        );
        assert_eq!(
            names(
                &entries,
                &filter(Scope::Format(PluginFormat::Clap), Kind::All, "")
            ),
            ["Diva CLAP", "Satin CLAP"]
        );
        // … favorites show it, as missing; the kind filter still applies to
        // the others.
        assert_eq!(
            names(&entries, &filter(Scope::Favorites, Kind::Effects, "")),
            ["Satin CLAP", "TONE3000 CLAP"]
        );
        assert_eq!(
            names(&entries, &filter(Scope::Favorites, Kind::All, "")),
            ["Diva CLAP", "Satin CLAP", "TONE3000 CLAP"]
        );
        assert_eq!(
            names(&entries, &filter(Scope::Category(amps), Kind::All, "")),
            ["TONE3000 CLAP"]
        );
        // Search works in favorites, and finds category names.
        assert_eq!(
            names(&entries, &filter(Scope::Favorites, Kind::All, "u-he sat")),
            ["Satin CLAP"]
        );
        assert!(
            names(&entries, &filter(Scope::All, Kind::All, "amps")).is_empty(),
            "the installed VST3 is not in Amps"
        );
        assert_eq!(
            names(&entries, &filter(Scope::Favorites, Kind::All, "amps")),
            ["TONE3000 CLAP"]
        );
        // A star on an installed row names the plugin by format and id.
        let satin = entries
            .iter()
            .find(|e| e.plugin.id == "com.u-he.Satin")
            .unwrap();
        assert!(satin.favorite);
        let f = satin.favorite_plugin();
        assert_eq!(f.key, FavoriteKey::of(&cat[2].plugin));
        assert_eq!(f.vendor, "u-he");
    }

    #[test]
    fn without_favorites_the_browser_lists_as_before() {
        let entries = build_entries(catalogue(), &PluginFavorites::default(), |_| None);
        assert_eq!(entries.len(), 4);
        assert!(entries.iter().all(|e| !e.favorite && !e.missing));
        assert_eq!(
            names(&entries, &filter(Scope::All, Kind::Effects, "tone")),
            ["TONE3000 CLAP", "TONE3000 VST3"]
        );
        assert_eq!(
            names(
                &entries,
                &filter(Scope::Vendor("u-he".into()), Kind::Instruments, "")
            ),
            ["Diva CLAP"]
        );
        assert!(names(&entries, &filter(Scope::Favorites, Kind::All, "")).is_empty());
    }
}
