//! Visual themes ("skins").
//!
//! All colours, sizes and typographic choices of the custom surfaces live
//! here instead of being scattered through the views; [`ConsoleLook`] also
//! decides how console controls are drawn (knob skirts, brushed panels,
//! wooden cheeks, VU needles, flat controls). [`Theme::all`] lists the
//! built-in skins; each is an original design — no brand is imitated.

use crate::Color;

#[derive(Clone, Debug)]
pub struct Typography {
    pub tiny: f32,
    pub small: f32,
    pub normal: f32,
    pub large: f32,
    pub display: f32,
}

#[derive(Clone, Debug)]
pub struct UiPalette {
    pub background: Color,
    pub surface: Color,
    pub surface_alt: Color,
    pub border: Color,
    pub text: Color,
    pub text_dim: Color,
    pub text_faint: Color,
    /// Playhead / record-ready accent.
    pub accent: Color,
    /// Selection highlight.
    pub selection: Color,
    /// LCD-style displays (transport counter, edit counters).
    pub lcd_bg: Color,
    pub lcd_text: Color,
    pub lcd_dim: Color,
}

#[derive(Clone, Debug)]
pub struct KnobStyle {
    pub body_dark: Color,
    pub body_light: Color,
    pub cap_top: Color,
    pub cap_bottom: Color,
    pub pointer: Color,
    pub ring_track: Color,
    pub shadow: Color,
}

#[derive(Clone, Debug)]
pub struct FaderStyle {
    pub slot: Color,
    pub slot_edge: Color,
    pub cap_top: Color,
    pub cap_bottom: Color,
    pub cap_line: Color,
    pub cap_grip: Color,
    pub scale_text: Color,
    pub scale_tick: Color,
    pub cap_width: f32,
    pub cap_height: f32,
}

#[derive(Clone, Debug)]
pub struct MeterStyle {
    pub background: Color,
    pub green: Color,
    pub yellow: Color,
    pub orange: Color,
    pub red: Color,
    /// Brightness of unlit segments relative to lit ones.
    pub unlit: f32,
    pub peak: Color,
    pub clip: Color,
    pub segment: f32,
    pub gap: f32,
    /// Edgewise VU meters: the backlit scale and the needle.
    pub vu_face: Color,
    pub vu_needle: Color,
}

/// How meters are drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MeterKind {
    /// Segmented LED ladder.
    Ladder,
    /// Continuous bar.
    Bar,
    /// Edgewise moving-coil VU meter (0 VU = −18 dBFS).
    Edgewise,
}

/// How console controls are drawn beyond their colours.
#[derive(Clone, Debug)]
pub struct ConsoleLook {
    /// Knobs sit on a skirt of this colour with a printed scale.
    pub knob_skirt: Option<Color>,
    /// Knobs show their value as an illuminated ring.
    pub knob_ring: bool,
    /// Strength of the panels' sheen bands (0: none).
    pub sheen: f32,
    /// Strength of brushed-metal grain on panels (0: none).
    pub brushed: f32,
    /// Panel screws (on the mixer's cheeks and the master section).
    pub screws: bool,
    /// Wooden end cheeks around the mixer (light, dark).
    pub wood: Option<(Color, Color)>,
    pub meter: MeterKind,
    /// The shadow under engraved legends (transparent: none).
    pub engrave: Color,
    /// Flat controls: no bevels or gradients.
    pub flat: bool,
}

impl Default for ConsoleLook {
    fn default() -> Self {
        Self {
            knob_skirt: None,
            knob_ring: true,
            sheen: 1.0,
            brushed: 0.0,
            screws: false,
            wood: None,
            meter: MeterKind::Ladder,
            engrave: Color::rgba(0.0, 0.0, 0.0, 0.55),
            flat: false,
        }
    }
}

#[derive(Clone, Debug)]
pub struct LedStyle {
    pub mute: Color,
    pub solo: Color,
    pub record: Color,
    pub monitor: Color,
    pub phase: Color,
    pub off: Color,
    pub bezel: Color,
    pub label_off: Color,
    pub label_on: Color,
}

#[derive(Clone, Debug)]
pub struct ConsoleTheme {
    pub strip_width: f32,
    pub master_width: f32,
    pub strip_gap: f32,
    pub panel_top: Color,
    pub panel_bottom: Color,
    pub panel_edge_light: Color,
    pub panel_edge_dark: Color,
    pub panel_label: Color,
    pub section_line: Color,
    pub master_panel_top: Color,
    pub master_panel_bottom: Color,
    pub well: Color,
    pub well_text: Color,
    pub well_text_empty: Color,
    pub knob: KnobStyle,
    pub send_cap: Color,
    pub pan_cap: Color,
    pub trim_cap: Color,
    pub fader: FaderStyle,
    pub fader_cap_audio: Color,
    pub fader_cap_bus: Color,
    pub fader_cap_aux: Color,
    pub fader_cap_master: Color,
    pub meter: MeterStyle,
    pub led: LedStyle,
    pub scribble_bg: Color,
    pub scribble_text: Color,
    pub selected_glow: Color,
    pub look: ConsoleLook,
}

#[derive(Clone, Debug)]
pub struct ArrangerTheme {
    pub background: Color,
    pub lane_a: Color,
    pub lane_b: Color,
    pub lane_selected: Color,
    pub header_bg: Color,
    pub header_bg_selected: Color,
    pub header_border: Color,
    pub ruler_bg: Color,
    pub ruler_text: Color,
    pub bar_line: Color,
    pub beat_line: Color,
    pub sub_line: Color,
    pub loop_on: Color,
    pub loop_off: Color,
    pub playhead: Color,
    /// Recording region and punch range.
    pub record: Color,
    /// Automation button and lanes.
    pub automation: Color,
    pub clip_radius: f32,
    pub clip_header: f32,
    pub clip_text: Color,
    pub selection_outline: Color,
    pub track_height: f32,
    pub header_width: f32,
    pub ruler_height: f32,
}

/// The performance meter: load bars, history graph, table.
#[derive(Clone, Debug)]
pub struct PerformanceTheme {
    pub background: Color,
    pub panel: Color,
    pub header: Color,
    pub row_a: Color,
    pub row_b: Color,
    pub row_hover: Color,
    pub plugin_row: Color,
    pub bar_track: Color,
    /// Load colours: below half the budget, below 3/4, below 9/10, above.
    pub load_ok: Color,
    pub load_warn: Color,
    pub load_high: Color,
    pub load_critical: Color,
    pub peak_mark: Color,
    pub graph_grid: Color,
    pub graph_avg: Color,
    pub graph_peak: Color,
    pub engine_share: Color,
    pub row_height: f32,
    pub plugin_row_height: f32,
    pub summary_height: f32,
}

/// The Tools view (mastering meters).
#[derive(Clone, Debug)]
pub struct ToolsTheme {
    pub background: Color,
    pub panel: Color,
    pub header: Color,
    /// Meter and graph backgrounds.
    pub well: Color,
    pub grid: Color,
    /// Levels: safe, near the top, over.
    pub level_ok: Color,
    pub level_warn: Color,
    pub level_over: Color,
    /// RMS inside the peak bar.
    pub rms: Color,
    pub hold: Color,
    pub spectrum: Color,
    pub spectrum_peak: Color,
    pub target: Color,
    pub goniometer: Color,
    /// Big readouts.
    pub readout: Color,
    pub toolbar_height: f32,
}

impl PerformanceTheme {
    /// Colour for a load (share of the callback budget).
    pub fn load_color(&self, load: f64) -> Color {
        if load < 0.5 {
            self.load_ok
        } else if load < 0.75 {
            self.load_warn
        } else if load < 0.9 {
            self.load_high
        } else {
            self.load_critical
        }
    }
}

#[derive(Clone, Debug)]
pub struct PianoRollTheme {
    pub background: Color,
    pub white_row: Color,
    pub black_row: Color,
    pub octave_line: Color,
    pub bar_line: Color,
    pub beat_line: Color,
    pub sub_line: Color,
    pub key_white: Color,
    pub key_white_shade: Color,
    pub key_black: Color,
    pub key_text: Color,
    pub velocity_bg: Color,
    pub toolbar: Color,
    pub button: Color,
    pub button_active: Color,
    /// Rows outside the scale are darkened by this.
    pub off_scale: Color,
    /// The scale's root rows.
    pub root_row: Color,
    pub ghost_note: Color,
    pub step_cursor: Color,
    pub lane_curve: Color,
    pub rubber_band: Color,
    pub keyboard_width: f32,
    pub row_height: f32,
    pub velocity_height: f32,
    pub ruler_height: f32,
    pub toolbar_height: f32,
}

#[derive(Clone, Debug)]
pub struct Theme {
    /// Stable identifier (saved in the preferences).
    pub id: &'static str,
    pub name: &'static str,
    /// Dark surfaces (GTK uses its dark variant).
    pub dark: bool,
    pub fonts: Typography,
    pub ui: UiPalette,
    pub console: ConsoleTheme,
    pub arranger: ArrangerTheme,
    pub piano: PianoRollTheme,
    pub perf: PerformanceTheme,
    pub tools: ToolsTheme,
}

impl Default for Theme {
    fn default() -> Self {
        Self::studio()
    }
}

impl Theme {
    /// "Studio": dark anodised console, warm cream legends, modern editors.
    pub fn studio() -> Self {
        let accent = Color::hex(0xff6a3d);
        Self {
            id: "studio",
            name: "Studio",
            dark: true,
            fonts: Typography {
                tiny: 8.5,
                small: 10.0,
                normal: 11.5,
                large: 13.5,
                display: 20.0,
            },
            ui: UiPalette {
                background: Color::hex(0x17181b),
                surface: Color::hex(0x1e2024),
                surface_alt: Color::hex(0x25282d),
                border: Color::hex(0x0d0e10),
                text: Color::hex(0xe6e3dc),
                text_dim: Color::hex(0x9a9c9f),
                text_faint: Color::hex(0x5f6266),
                accent,
                selection: Color::hex(0x6fc3ff),
                lcd_bg: Color::hex(0x0d100e),
                lcd_text: Color::hex(0xf0c46a),
                lcd_dim: Color::hex(0x6f5a33),
            },
            console: ConsoleTheme {
                strip_width: 92.0,
                master_width: 112.0,
                strip_gap: 2.0,
                panel_top: Color::hex(0x3b3e43),
                panel_bottom: Color::hex(0x2b2d31),
                panel_edge_light: Color::rgba(1.0, 1.0, 1.0, 0.10),
                panel_edge_dark: Color::rgba(0.0, 0.0, 0.0, 0.55),
                panel_label: Color::hex(0xbdb6a5),
                section_line: Color::rgba(0.0, 0.0, 0.0, 0.45),
                master_panel_top: Color::hex(0x43403b),
                master_panel_bottom: Color::hex(0x302e2b),
                well: Color::hex(0x111214),
                well_text: Color::hex(0xdcd4bf),
                well_text_empty: Color::hex(0x4a4d52),
                knob: KnobStyle {
                    body_dark: Color::hex(0x141517),
                    body_light: Color::hex(0x3c3e43),
                    cap_top: Color::hex(0x5a5d63),
                    cap_bottom: Color::hex(0x26282c),
                    pointer: Color::hex(0xf4ecd9),
                    ring_track: Color::hex(0x0c0d0f),
                    shadow: Color::rgba(0.0, 0.0, 0.0, 0.6),
                },
                send_cap: Color::hex(0x3d7fb5),
                pan_cap: Color::hex(0x8d9096),
                trim_cap: Color::hex(0xb5443c),
                fader: FaderStyle {
                    slot: Color::hex(0x08090a),
                    slot_edge: Color::rgba(1.0, 1.0, 1.0, 0.07),
                    cap_top: Color::hex(0xe4e2dc),
                    cap_bottom: Color::hex(0x8f8d88),
                    cap_line: Color::hex(0x161616),
                    cap_grip: Color::rgba(0.0, 0.0, 0.0, 0.22),
                    scale_text: Color::hex(0x9d978a),
                    scale_tick: Color::rgba(0.85, 0.82, 0.74, 0.35),
                    cap_width: 30.0,
                    cap_height: 48.0,
                },
                fader_cap_audio: Color::hex(0xdcd9d2),
                fader_cap_bus: Color::hex(0x6f93c0),
                fader_cap_aux: Color::hex(0x6fae88),
                fader_cap_master: Color::hex(0xc9483f),
                meter: MeterStyle {
                    background: Color::hex(0x060707),
                    green: Color::hex(0x3fd16b),
                    yellow: Color::hex(0xe6cb4a),
                    orange: Color::hex(0xf08a3a),
                    red: Color::hex(0xff3d3d),
                    unlit: 0.13,
                    peak: Color::hex(0xf6f2e8),
                    clip: Color::hex(0xff2a2a),
                    segment: 2.0,
                    gap: 1.0,
                    vu_face: Color::hex(0xf3dfa8),
                    vu_needle: Color::hex(0x1a1410),
                },
                led: LedStyle {
                    mute: Color::hex(0xf2b134),
                    solo: Color::hex(0x5ad66b),
                    record: Color::hex(0xff4b4b),
                    monitor: Color::hex(0x4fb3ff),
                    phase: Color::hex(0xc58bff),
                    off: Color::hex(0x2c2e33),
                    bezel: Color::hex(0x0f1012),
                    label_off: Color::hex(0xa9a497),
                    label_on: Color::hex(0x141414),
                },
                scribble_bg: Color::hex(0xe9e3d1),
                scribble_text: Color::hex(0x1f1d19),
                selected_glow: Color::hex(0x6fc3ff).with_alpha(0.55),
                look: ConsoleLook::default(),
            },
            arranger: ArrangerTheme {
                background: Color::hex(0x191a1d),
                lane_a: Color::hex(0x1d1f23),
                lane_b: Color::hex(0x1a1c1f),
                lane_selected: Color::hex(0x23272d),
                header_bg: Color::hex(0x25272c),
                header_bg_selected: Color::hex(0x2e333a),
                header_border: Color::hex(0x101113),
                ruler_bg: Color::hex(0x202226),
                ruler_text: Color::hex(0xb3b0a9),
                bar_line: Color::rgba(1.0, 1.0, 1.0, 0.10),
                beat_line: Color::rgba(1.0, 1.0, 1.0, 0.045),
                sub_line: Color::rgba(1.0, 1.0, 1.0, 0.022),
                loop_on: accent.with_alpha(0.22),
                loop_off: Color::rgba(1.0, 1.0, 1.0, 0.07),
                playhead: accent,
                record: Color::hex(0xff4b4b),
                automation: Color::hex(0x5fc27a),
                clip_radius: 4.0,
                // Tall enough for the clip gain knob.
                clip_header: 22.0,
                clip_text: Color::hex(0x121212),
                selection_outline: Color::hex(0xf5f5f5),
                track_height: 72.0,
                header_width: 252.0,
                ruler_height: 30.0,
            },
            piano: PianoRollTheme {
                background: Color::hex(0x18191c),
                white_row: Color::hex(0x202226),
                black_row: Color::hex(0x1a1b1e),
                octave_line: Color::rgba(1.0, 1.0, 1.0, 0.09),
                bar_line: Color::rgba(1.0, 1.0, 1.0, 0.12),
                beat_line: Color::rgba(1.0, 1.0, 1.0, 0.05),
                sub_line: Color::rgba(1.0, 1.0, 1.0, 0.025),
                key_white: Color::hex(0xe8e5de),
                key_white_shade: Color::hex(0xc4c1ba),
                key_black: Color::hex(0x1b1c1f),
                key_text: Color::hex(0x6d7177),
                velocity_bg: Color::hex(0x141518),
                toolbar: Color::hex(0x202227),
                button: Color::hex(0x2a2d33),
                button_active: Color::hex(0x5b3a2c),
                off_scale: Color::rgba(0.0, 0.0, 0.0, 0.32),
                root_row: Color::rgba(1.0, 0.42, 0.24, 0.07),
                ghost_note: Color::rgba(1.0, 1.0, 1.0, 0.16),
                step_cursor: Color::hex(0xffb347),
                lane_curve: Color::hex(0x7fd1ff),
                rubber_band: Color::rgba(0.55, 0.75, 1.0, 0.18),
                keyboard_width: 66.0,
                row_height: 13.0,
                velocity_height: 96.0,
                ruler_height: 24.0,
                toolbar_height: 32.0,
            },
            perf: PerformanceTheme {
                background: Color::hex(0x17181b),
                panel: Color::hex(0x1e2024),
                header: Color::hex(0x24262b),
                row_a: Color::hex(0x1b1d20),
                row_b: Color::hex(0x191a1d),
                row_hover: Color::hex(0x262a30),
                plugin_row: Color::hex(0x16171a),
                bar_track: Color::hex(0x101114),
                load_ok: Color::hex(0x4fc36b),
                load_warn: Color::hex(0xe1c14b),
                load_high: Color::hex(0xf08a3c),
                load_critical: Color::hex(0xf04a3c),
                peak_mark: Color::hex(0xf2efe8),
                graph_grid: Color::rgba(1.0, 1.0, 1.0, 0.06),
                graph_avg: Color::hex(0x4fc36b),
                graph_peak: Color::hex(0xffb38f),
                engine_share: Color::hex(0x6f8fd8),
                row_height: 30.0,
                plugin_row_height: 24.0,
                summary_height: 152.0,
            },
            tools: ToolsTheme {
                background: Color::hex(0x17181b),
                panel: Color::hex(0x1e2024),
                header: Color::hex(0x24262b),
                well: Color::hex(0x101114),
                grid: Color::rgba(1.0, 1.0, 1.0, 0.07),
                level_ok: Color::hex(0x4fc36b),
                level_warn: Color::hex(0xe1c14b),
                level_over: Color::hex(0xf04a3c),
                rms: Color::hex(0x2f8a47),
                hold: Color::hex(0xf2efe8),
                spectrum: Color::hex(0x5fb0e8),
                spectrum_peak: Color::hex(0xffb38f),
                target: accent,
                goniometer: Color::hex(0x7fe0a0),
                readout: Color::hex(0xf2efe8),
                toolbar_height: 32.0,
            },
        }
    }
}
