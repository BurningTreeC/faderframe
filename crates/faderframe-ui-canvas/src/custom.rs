//! Themes of the user's own: a built-in skin with colours changed. [`COLORS`]
//! names every colour a user may set — each sets all the theme's fields
//! that show that colour — and the themes made from them are registered
//! here ([`set_custom_themes`]), so [`Theme::all`] and [`Theme::by_id`]
//! find them like the built-in ones.

use crate::{Color, Theme};
use std::sync::RwLock;

/// Ids of custom themes start with this.
pub const CUSTOM_PREFIX: &str = "custom:";

/// A colour a user may set.
pub struct ColorKey {
    /// Stable (saved in theme files), e.g. `ui.background`.
    pub id: &'static str,
    /// Which part of the app it belongs to (the editor's sections).
    pub group: &'static str,
    pub label: &'static str,
    get: fn(&Theme) -> Color,
    set: fn(&mut Theme, Color),
}

impl ColorKey {
    pub fn get(&self, theme: &Theme) -> Color {
        (self.get)(theme)
    }

    /// Set it everywhere the theme shows it.
    pub fn set(&self, theme: &mut Theme, color: Color) {
        (self.set)(theme, color);
    }
}

/// The sections of [`COLORS`], in order.
pub const GROUPS: [&str; 4] = ["Window", "Editors", "Mixer", "Meters and lights"];

macro_rules! key {
    ($id:literal, $group:literal, $label:literal, |$t:ident| $get:expr, |$s:ident, $c:ident| $set:block) => {
        ColorKey {
            id: $id,
            group: $group,
            label: $label,
            get: |$t: &Theme| $get,
            set: |$s: &mut Theme, $c: Color| $set,
        }
    };
}

/// Every colour a user may set, by section.
pub static COLORS: [ColorKey; 56] = [
    // --- the window ------------------------------------------------------------
    key!(
        "ui.background",
        "Window",
        "Background",
        |t| t.ui.background,
        |t, c| {
            t.ui.background = c;
            t.perf.background = c;
            t.tools.background = c;
            t.device.deck = c;
        }
    ),
    key!(
        "ui.surface",
        "Window",
        "Panels",
        |t| t.ui.surface,
        |t, c| {
            t.ui.surface = c;
            t.perf.panel = c;
            t.tools.panel = c;
            t.device.section = c;
            t.device.panel = c;
        }
    ),
    key!(
        "ui.surface_alt",
        "Window",
        "Buttons",
        |t| t.ui.surface_alt,
        |t, c| {
            t.ui.surface_alt = c;
            t.piano.button = c;
        }
    ),
    key!("ui.border", "Window", "Borders", |t| t.ui.border, |t, c| {
        t.ui.border = c;
        t.device.section_edge = c;
        t.device.panel_edge = c;
    }),
    key!("ui.text", "Window", "Text", |t| t.ui.text, |t, c| {
        t.ui.text = c;
        t.perf.peak_mark = c;
        t.tools.hold = c;
        t.tools.readout = c;
        t.device.post = c;
        t.device.left = c;
    }),
    key!(
        "ui.text_dim",
        "Window",
        "Dim text",
        |t| t.ui.text_dim,
        |t, c| {
            t.ui.text_dim = c;
            t.device.pre = c;
        }
    ),
    key!(
        "ui.text_faint",
        "Window",
        "Faint text",
        |t| t.ui.text_faint,
        |t, c| {
            t.ui.text_faint = c;
        }
    ),
    key!("ui.accent", "Window", "Accent", |t| t.ui.accent, |t, c| {
        t.ui.accent = c;
        t.ui.accent_2 = c;
        t.arranger.loop_on = c.with_alpha(t.arranger.loop_on.a);
        t.piano.root_row = c.with_alpha(t.piano.root_row.a);
        t.piano.button_active = c.mix(t.ui.surface, 0.62);
        t.tools.target = c;
        t.tools.spectrum_peak = c;
        t.perf.graph_peak = c;
        t.device.curve = c;
        t.device.dyn_live = c;
    }),
    key!(
        "ui.selection",
        "Window",
        "Selection",
        |t| t.ui.selection,
        |t, c| {
            t.ui.selection = c;
            t.arranger.snap_guide = c;
            t.arranger.time_lock = c;
            t.piano.lane_curve = c;
            t.piano.rubber_band = c.with_alpha(t.piano.rubber_band.a);
            t.console.selected_glow = c.with_alpha(t.console.selected_glow.a);
            t.tools.spectrum = c;
            t.perf.engine_share = c;
            t.device.wave = c;
            t.device.side = c;
        }
    ),
    key!(
        "ui.lcd_bg",
        "Window",
        "Display background",
        |t| t.ui.lcd_bg,
        |t, c| {
            t.ui.lcd_bg = c;
        }
    ),
    key!(
        "ui.lcd_text",
        "Window",
        "Display text",
        |t| t.ui.lcd_text,
        |t, c| {
            t.ui.lcd_text = c;
        }
    ),
    key!(
        "ui.lcd_dim",
        "Window",
        "Display unlit segments",
        |t| t.ui.lcd_dim,
        |t, c| {
            t.ui.lcd_dim = c;
        }
    ),
    // --- the editors -----------------------------------------------------------
    key!(
        "arranger.background",
        "Editors",
        "Background",
        |t| t.arranger.background,
        |t, c| {
            t.arranger.background = c;
            t.piano.background = c;
            t.piano.velocity_bg = c.darken(0.12);
            t.perf.plugin_row = c;
        }
    ),
    key!(
        "arranger.lane_a",
        "Editors",
        "Lanes",
        |t| t.arranger.lane_a,
        |t, c| {
            t.arranger.lane_a = c;
            t.piano.white_row = c;
            t.perf.row_a = c;
        }
    ),
    key!(
        "arranger.lane_b",
        "Editors",
        "Alternate lanes",
        |t| t.arranger.lane_b,
        |t, c| {
            t.arranger.lane_b = c;
            t.piano.black_row = c;
            t.perf.row_b = c;
        }
    ),
    key!(
        "arranger.lane_selected",
        "Editors",
        "Selected lane",
        |t| t.arranger.lane_selected,
        |t, c| {
            t.arranger.lane_selected = c;
            t.perf.row_hover = c;
        }
    ),
    key!(
        "arranger.header_bg",
        "Editors",
        "Track headers and toolbars",
        |t| t.arranger.header_bg,
        |t, c| {
            t.arranger.header_bg = c;
            t.piano.toolbar = c;
            t.perf.header = c;
            t.tools.header = c;
        }
    ),
    key!(
        "arranger.header_bg_selected",
        "Editors",
        "Selected track header",
        |t| t.arranger.header_bg_selected,
        |t, c| {
            t.arranger.header_bg_selected = c;
        }
    ),
    key!(
        "arranger.header_border",
        "Editors",
        "Header borders",
        |t| t.arranger.header_border,
        |t, c| {
            t.arranger.header_border = c;
        }
    ),
    key!(
        "arranger.ruler_bg",
        "Editors",
        "Ruler",
        |t| t.arranger.ruler_bg,
        |t, c| {
            t.arranger.ruler_bg = c;
        }
    ),
    key!(
        "arranger.ruler_text",
        "Editors",
        "Ruler text",
        |t| t.arranger.ruler_text,
        |t, c| {
            t.arranger.ruler_text = c;
        }
    ),
    key!(
        "arranger.clip_text",
        "Editors",
        "Clip names",
        |t| t.arranger.clip_text,
        |t, c| {
            t.arranger.clip_text = c;
        }
    ),
    key!(
        "arranger.selection_outline",
        "Editors",
        "Selected clips' outline",
        |t| t.arranger.selection_outline,
        |t, c| {
            t.arranger.selection_outline = c;
        }
    ),
    key!(
        "arranger.playhead",
        "Editors",
        "Playhead",
        |t| t.arranger.playhead,
        |t, c| {
            t.arranger.playhead = c;
        }
    ),
    key!(
        "piano.key_white",
        "Editors",
        "White keys",
        |t| t.piano.key_white,
        |t, c| {
            t.piano.key_white = c;
            t.device.key_white = c;
        }
    ),
    key!(
        "piano.key_black",
        "Editors",
        "Black keys",
        |t| t.piano.key_black,
        |t, c| {
            t.piano.key_black = c;
            t.device.key_black = c;
        }
    ),
    key!(
        "piano.key_text",
        "Editors",
        "Key names",
        |t| t.piano.key_text,
        |t, c| {
            t.piano.key_text = c;
        }
    ),
    // --- the mixer -------------------------------------------------------------
    key!(
        "console.panel_top",
        "Mixer",
        "Channel strips (top)",
        |t| t.console.panel_top,
        |t, c| {
            t.console.panel_top = c;
        }
    ),
    key!(
        "console.panel_bottom",
        "Mixer",
        "Channel strips (bottom)",
        |t| t.console.panel_bottom,
        |t, c| {
            t.console.panel_bottom = c;
        }
    ),
    key!(
        "console.master_panel_top",
        "Mixer",
        "Master strip (top)",
        |t| t.console.master_panel_top,
        |t, c| {
            t.console.master_panel_top = c;
        }
    ),
    key!(
        "console.master_panel_bottom",
        "Mixer",
        "Master strip (bottom)",
        |t| t.console.master_panel_bottom,
        |t, c| {
            t.console.master_panel_bottom = c;
        }
    ),
    key!(
        "console.panel_label",
        "Mixer",
        "Strip labels",
        |t| t.console.panel_label,
        |t, c| {
            t.console.panel_label = c;
        }
    ),
    key!(
        "console.well",
        "Mixer",
        "Insert and send slots",
        |t| t.console.well,
        |t, c| {
            t.console.well = c;
            t.perf.bar_track = c;
            t.tools.well = c;
            t.device.display = c;
        }
    ),
    key!(
        "console.well_text",
        "Mixer",
        "Slot text",
        |t| t.console.well_text,
        |t, c| {
            t.console.well_text = c;
        }
    ),
    key!(
        "console.scribble_bg",
        "Mixer",
        "Scribble strips",
        |t| t.console.scribble_bg,
        |t, c| {
            t.console.scribble_bg = c;
        }
    ),
    key!(
        "console.scribble_text",
        "Mixer",
        "Scribble strip text",
        |t| t.console.scribble_text,
        |t, c| {
            t.console.scribble_text = c;
        }
    ),
    key!(
        "console.fader_cap_audio",
        "Mixer",
        "Fader caps: tracks",
        |t| t.console.fader_cap_audio,
        |t, c| {
            t.console.fader_cap_audio = c;
        }
    ),
    key!(
        "console.fader_cap_bus",
        "Mixer",
        "Fader caps: buses",
        |t| t.console.fader_cap_bus,
        |t, c| {
            t.console.fader_cap_bus = c;
        }
    ),
    key!(
        "console.fader_cap_aux",
        "Mixer",
        "Fader caps: auxes",
        |t| t.console.fader_cap_aux,
        |t, c| {
            t.console.fader_cap_aux = c;
        }
    ),
    key!(
        "console.fader_cap_master",
        "Mixer",
        "Fader cap: master",
        |t| t.console.fader_cap_master,
        |t, c| {
            t.console.fader_cap_master = c;
        }
    ),
    key!(
        "console.send_cap",
        "Mixer",
        "Send knobs",
        |t| t.console.send_cap,
        |t, c| {
            t.console.send_cap = c;
        }
    ),
    key!(
        "console.pan_cap",
        "Mixer",
        "Pan knobs",
        |t| t.console.pan_cap,
        |t, c| {
            t.console.pan_cap = c;
        }
    ),
    key!(
        "console.trim_cap",
        "Mixer",
        "Trim knobs",
        |t| t.console.trim_cap,
        |t, c| {
            t.console.trim_cap = c;
        }
    ),
    key!(
        "console.knob.pointer",
        "Mixer",
        "Knob pointers",
        |t| t.console.knob.pointer,
        |t, c| {
            t.console.knob.pointer = c;
        }
    ),
    // --- meters and lights -----------------------------------------------------
    key!(
        "meter.background",
        "Meters and lights",
        "Meter background",
        |t| t.console.meter.background,
        |t, c| {
            t.console.meter.background = c;
        }
    ),
    key!(
        "meter.green",
        "Meters and lights",
        "Meters: low",
        |t| t.console.meter.green,
        |t, c| {
            t.console.meter.green = c;
            t.perf.load_ok = c;
            t.perf.graph_avg = c;
            t.tools.level_ok = c;
            t.tools.goniometer = c;
            t.tools.rms = c.darken(0.35);
            t.device.mid = c;
            t.arranger.launch_playing = c;
        }
    ),
    key!(
        "meter.yellow",
        "Meters and lights",
        "Meters: middle",
        |t| t.console.meter.yellow,
        |t, c| {
            t.console.meter.yellow = c;
            t.perf.load_warn = c;
            t.tools.level_warn = c;
            t.arranger.launch_queued = c;
        }
    ),
    key!(
        "meter.orange",
        "Meters and lights",
        "Meters: high",
        |t| t.console.meter.orange,
        |t, c| {
            t.console.meter.orange = c;
            t.perf.load_high = c;
            t.device.reduction = c;
        }
    ),
    key!(
        "meter.red",
        "Meters and lights",
        "Meters: over",
        |t| t.console.meter.red,
        |t, c| {
            t.console.meter.red = c;
            t.console.meter.clip = c;
            t.perf.load_critical = c;
            t.tools.level_over = c;
            t.device.collision = c;
            t.device.dyn_range = c;
        }
    ),
    key!(
        "led.mute",
        "Meters and lights",
        "Mute",
        |t| t.console.led.mute,
        |t, c| {
            t.console.led.mute = c;
            t.piano.step_cursor = c;
        }
    ),
    key!(
        "led.solo",
        "Meters and lights",
        "Solo",
        |t| t.console.led.solo,
        |t, c| {
            t.console.led.solo = c;
            t.arranger.automation = c;
        }
    ),
    key!(
        "led.record",
        "Meters and lights",
        "Record",
        |t| t.console.led.record,
        |t, c| {
            t.console.led.record = c;
            t.arranger.record = c;
            t.device.right = c;
        }
    ),
    key!(
        "led.monitor",
        "Meters and lights",
        "Monitor",
        |t| t.console.led.monitor,
        |t, c| {
            t.console.led.monitor = c;
        }
    ),
    key!(
        "led.phase",
        "Meters and lights",
        "Polarity",
        |t| t.console.led.phase,
        |t, c| {
            t.console.led.phase = c;
        }
    ),
    key!(
        "led.off",
        "Meters and lights",
        "Lights off",
        |t| t.console.led.off,
        |t, c| {
            t.console.led.off = c;
        }
    ),
    key!(
        "led.label_off",
        "Meters and lights",
        "Button labels",
        |t| t.console.led.label_off,
        |t, c| {
            t.console.led.label_off = c;
        }
    ),
];

/// The colour key with this id.
pub fn color_key(id: &str) -> Option<&'static ColorKey> {
    COLORS.iter().find(|k| k.id == id)
}

/// A theme of the user's own: a built-in skin with colours changed.
#[derive(Clone, Debug, PartialEq)]
pub struct CustomTheme {
    /// Unique among the user's themes (its file's name).
    pub slug: String,
    pub name: String,
    /// The built-in skin it starts from (its looks: knobs, meters, fonts).
    pub base: String,
    /// Colours changed, by [`ColorKey::id`], in the order set.
    pub colors: Vec<(String, Color)>,
}

impl CustomTheme {
    /// Its id as [`Theme::id`] and the preferences have it.
    pub fn id(&self) -> String {
        format!("{CUSTOM_PREFIX}{}", self.slug)
    }

    /// The built-in skin it starts from (Studio, for one not known).
    pub fn base_theme(&self) -> Theme {
        Theme::builtin()
            .into_iter()
            .find(|t| t.id == self.base)
            .unwrap_or_default()
    }

    /// The theme: its base with its colours (dark when its background is).
    pub fn build(&self) -> Theme {
        let mut t = self.base_theme();
        for (id, c) in &self.colors {
            if let Some(k) = color_key(id) {
                k.set(&mut t, *c);
            }
        }
        t.id = intern(self.id());
        t.name = intern(self.name.clone());
        if self.colors.iter().any(|(id, _)| id == "ui.background") {
            t.dark = t.ui.background.luminance() < 0.5;
        }
        t
    }

    /// Set a colour (as the base has it: forgotten).
    pub fn set_color(&mut self, id: &str, color: Color) {
        self.colors.retain(|(k, _)| k != id);
        let base = color_key(id).map(|k| k.get(&self.base_theme()));
        if base != Some(color) {
            self.colors.push((id.to_string(), color));
        }
    }

    /// The colour this theme has for `id`.
    pub fn color(&self, id: &str) -> Option<Color> {
        color_key(id).map(|k| k.get(&self.build()))
    }
}

static CUSTOM: RwLock<Vec<Theme>> = RwLock::new(Vec::new());
static INTERNED: RwLock<Vec<&'static str>> = RwLock::new(Vec::new());

/// A `'static` copy of `s`, made once per distinct text (theme ids and
/// names are `'static`; a user renaming themes makes a few more).
fn intern(s: String) -> &'static str {
    if let Ok(list) = INTERNED.read()
        && let Some(found) = list.iter().find(|x| **x == s)
    {
        return found;
    }
    let leaked: &'static str = Box::leak(s.into_boxed_str());
    if let Ok(mut list) = INTERNED.write() {
        list.push(leaked);
    }
    leaked
}

/// The user's themes from now on (all of them, replacing those before).
pub fn set_custom_themes(themes: &[CustomTheme]) {
    let built: Vec<Theme> = themes.iter().map(CustomTheme::build).collect();
    if let Ok(mut list) = CUSTOM.write() {
        *list = built;
    }
}

/// The user's themes as registered.
pub fn custom_themes() -> Vec<Theme> {
    CUSTOM.read().map(|l| l.clone()).unwrap_or_default()
}

/// `#rrggbb` (or `#rrggbbaa` when not opaque).
pub fn to_hex(c: Color) -> String {
    let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    if c.a >= 0.999 {
        format!("#{:02x}{:02x}{:02x}", b(c.r), b(c.g), b(c.b))
    } else {
        format!("#{:02x}{:02x}{:02x}{:02x}", b(c.r), b(c.g), b(c.b), b(c.a))
    }
}

/// `#rrggbb` or `#rrggbbaa` (the `#` optional).
pub fn parse_hex(s: &str) -> Option<Color> {
    let h = s.trim().trim_start_matches('#');
    if !h.is_ascii() || !(h.len() == 6 || h.len() == 8) {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
    let (r, g, b) = (byte(0)?, byte(2)?, byte(4)?);
    let a = if h.len() == 8 { byte(6)? } else { 255 };
    Some(Color::rgb8(r, g, b).with_alpha(f32::from(a) / 255.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colour_keys_are_unique_grouped_and_read_back_what_they_set() {
        let mut ids: Vec<_> = COLORS.iter().map(|k| k.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), COLORS.len(), "unique ids");
        let probe = Color::rgb8(0x12, 0x34, 0x56);
        for k in &COLORS {
            assert!(GROUPS.contains(&k.group), "{}: {}", k.id, k.group);
            let mut t = Theme::default();
            k.set(&mut t, probe);
            assert_eq!(k.get(&t), probe, "{}", k.id);
        }
    }

    #[test]
    fn a_custom_theme_is_its_base_with_its_colours() {
        let red = Color::rgb8(200, 30, 30);
        let mut c = CustomTheme {
            slug: "test-theme".into(),
            name: "Test Theme".into(),
            base: "daylight".into(),
            colors: Vec::new(),
        };
        c.set_color("ui.accent", red);
        let base = Theme::by_id("daylight");
        // As the base has it: not kept.
        c.set_color("ui.text", base.ui.text);
        assert_eq!(c.colors.len(), 1);
        let t = c.build();
        assert_eq!(t.id, "custom:test-theme");
        assert_eq!(t.name, "Test Theme");
        assert_eq!(t.ui.accent, red);
        assert_eq!(t.arranger.playhead, base.arranger.playhead, "its own key");
        assert_eq!(t.ui.background, base.ui.background);
        assert!(!t.dark, "the base's");
        // A dark background makes it a dark theme.
        c.set_color("ui.background", Color::rgb8(10, 10, 12));
        assert!(c.build().dark);
        assert_eq!(c.color("ui.accent"), Some(red));
    }

    #[test]
    fn hex_colours_round_trip() {
        let c = Color::rgb8(0xab, 0xcd, 0xef);
        assert_eq!(to_hex(c), "#abcdef");
        assert_eq!(parse_hex("#abcdef"), Some(c));
        assert_eq!(
            parse_hex("abcdef80").map(|c| (c.a * 255.0).round()),
            Some(128.0)
        );
        assert_eq!(parse_hex("#abc"), None);
        assert_eq!(parse_hex("#gggggg"), None);
    }
}
