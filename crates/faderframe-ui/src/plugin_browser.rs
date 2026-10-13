//! The plugin browser: every available plugin (built-in and CLAP), with
//! search, an Effects/Instruments switch, format and vendor filters, a
//! detail panel, and one-click insertion into the track it was opened for.
//! Plugins from other makers show how they did in the realtime check
//! (`crate::plugin_check`): a badge per row, findings and advice in the
//! detail panel, a filter for those with findings.

use crate::state::AppState;
use faderframe_core::TrackId;
use faderframe_project::{PluginFormat, PluginRef};
use faderframe_session::{AvailablePlugin, PluginTarget};
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
}

impl Entry {
    /// How it did in the realtime check (this version).
    fn check(&self) -> Option<faderframe_plugin_check::CheckReport> {
        crate::plugin_check::report(&self.plugin, &self.version)
    }

    /// Checked (built-ins keep the rules by their own tests).
    fn checkable(&self) -> bool {
        self.plugin.format != PluginFormat::Builtin
    }

    fn kind_label(&self) -> &'static str {
        if self.instrument {
            "Instrument"
        } else if self.midi_effect {
            "MIDI Effect"
        } else {
            "Effect"
        }
    }

    fn format_label(&self) -> &'static str {
        match self.plugin.format {
            PluginFormat::Builtin => "Built-in",
            PluginFormat::Clap => "CLAP",
            PluginFormat::Vst3 => "VST3",
            PluginFormat::AudioUnit => "AU",
            PluginFormat::Lv2 => "LV2",
        }
    }

    fn matches(&self, text: &str) -> bool {
        if text.is_empty() {
            return true;
        }
        let hay = format!(
            "{} {} {} {}",
            self.plugin.name,
            self.vendor,
            self.format_label(),
            self.features.join(" ")
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
}

struct Filter {
    text: String,
    kind: Kind,
    scope: Scope,
}

impl Filter {
    fn accepts(&self, e: &Entry) -> bool {
        let kind = match self.kind {
            Kind::All => true,
            Kind::Effects => !e.instrument && !e.midi_effect,
            Kind::Instruments => e.instrument,
            Kind::MidiEffects => e.midi_effect,
        };
        let scope = match &self.scope {
            Scope::All => true,
            Scope::Findings => e
                .check()
                .is_some_and(|r| r.verdict() >= faderframe_plugin_check::Verdict::Caution),
            Scope::Unchecked => e.checkable() && e.check().is_none(),
            Scope::Format(f) => e.plugin.format == *f,
            Scope::Vendor(v) => &e.vendor == v,
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

fn entries(app: &AppState) -> Vec<Entry> {
    let mut out: Vec<Entry> = app
        .session
        .borrow()
        .available_plugins()
        .into_iter()
        .map(|p: AvailablePlugin| {
            let ch = |n: u16| match n {
                0 => "none".to_string(),
                1 => "mono".to_string(),
                2 => "stereo".to_string(),
                n => format!("{n} channels"),
            };
            let scanned = crate::plugins::scanned(p.plugin.format, &p.plugin.id);
            Entry {
                vendor: if p.vendor.is_empty() {
                    "Unknown vendor".into()
                } else {
                    p.vendor.clone()
                },
                version: p.version.clone(),
                instrument: p.instrument,
                midi_effect: p.midi_effect,
                features: scanned
                    .as_ref()
                    .map(|c| c.features.clone())
                    .unwrap_or_default(),
                audio: format!("{} in · {} out", ch(p.audio_inputs), ch(p.audio_outputs)),
                midi: if p.note_inputs > 0 {
                    "Note / MIDI input".into()
                } else {
                    "—".into()
                },
                bundle: scanned.map(|c| c.bundle.display().to_string()),
                plugin: p.plugin,
            }
        })
        .collect();
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

fn row_widget(e: &Entry) -> gtk::Widget {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    row.add_css_class("plugin-row");
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

impl Browser {
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
    fn refresh_list(&self) {
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
        let (all, scope) = item("All plugins", entries.len(), Scope::All);
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
            .filter(|(e, r)| e.checkable() && r.is_none())
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
            let n = entries.iter().filter(|e| e.plugin.format == f).count();
            if n == 0 && matches!(f, PluginFormat::Lv2 | PluginFormat::AudioUnit) {
                continue;
            }
            let (r, scope) = item(label, n, Scope::Format(f));
            self.sidebar.append(&r);
            scopes.push(Some(scope));
        }
        let mut vendors: Vec<(String, usize)> = Vec::new();
        for e in entries.iter() {
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

    fn show_detail(&self, e: Option<&Entry>) {
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
        self.detail
            .append(&check_section(e, crate::plugin_check::progress().is_some()));
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
        place.set_sensitive(!wrong_kind);
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
        let (track, target) = (*self.track.borrow(), *self.target.borrow());
        if app
            .with_session(|s| s.place_plugin(track, target, plugin))
            .is_some()
        {
            self.window.close();
        }
    }

    /// Show the check under way (and new reports) as they come.
    fn follow_checks(&self) {
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
    fn check_all(&self) {
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

    fn set_kind(&self, kind: Kind) {
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

    fn retarget(&self, track: TrackId, target: PluginTarget) {
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
