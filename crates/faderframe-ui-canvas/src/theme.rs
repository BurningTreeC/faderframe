//! Visual themes ("skins").
//!
//! All colours, sizes and typographic choices of the custom surfaces live
//! here instead of being scattered through the views; [`ConsoleLook`] also
//! decides how console controls are drawn (knob skirts, brushed panels,
//! wooden cheeks, VU needles, flat controls — or the planes, halo knobs,
//! pill faders and buttons and gradient meters of a skin drawn with light
//! instead of hardware). [`Theme::all`] lists the built-in skins; each is
//! an original design — no brand is imitated.

use crate::{Color, FontFamily};

#[derive(Clone, Debug)]
pub struct Typography {
    pub tiny: f32,
    pub small: f32,
    pub normal: f32,
    pub large: f32,
    pub display: f32,
    /// The face of numeric readouts (levels, pan, the transport's
    /// counters): monospaced, or the UI face with tabular figures.
    pub numerals: FontFamily,
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
    /// Where the accent's gradient ends (the accent itself for a skin
    /// without one).
    pub accent_2: Color,
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
    /// Gas-plasma bar graph: a glowing column with fine lines across it.
    Plasma,
    /// Fine rounded bars, one smooth gradient from cool to hot, a hairline
    /// peak marker and a soft bloom at the top of the level.
    Gradient,
}

/// How knobs without a skirt are drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum KnobKind {
    /// A body with a cap and a pointer (inside the value ring when the
    /// look has one).
    #[default]
    Cap,
    /// A halo: a fine ring with the value as a glowing arc around a flat
    /// disc with a small indicator dot.
    Halo,
}

/// How faders are drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FaderKind {
    /// A slot and a moulded cap with grip ridges.
    #[default]
    Cap,
    /// A slim track lit up to the level and a pill-shaped thumb.
    Pill,
}

/// How switches (mute, solo, record …) are drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ButtonKind {
    /// Illuminated push buttons.
    #[default]
    Led,
    /// Ghost pills with a hairline edge that light from within when on.
    Pill,
}

/// How console panels are drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PanelKind {
    /// Bevelled plates (with sheen, grain and screws as the look says).
    #[default]
    Hardware,
    /// Flat rounded planes set apart by luminance steps: no bevels,
    /// sheen, grain or screws; hairline section rules; wells and scribble
    /// strips as darker insets.
    Plane,
}

/// How the arranger draws clips.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ClipStyle {
    /// A coloured name strip over a darker translucent body.
    #[default]
    Classic,
    /// Translucent rounded cards tinted with the clip's colour, a fine
    /// line of it along the top, a light name and a softer waveform.
    Card,
}

/// How the GTK shell around the views is styled.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Chrome {
    /// Adwaita in the skin's colours.
    #[default]
    Classic,
    /// Flat planes: pill buttons, a glass transport display, rounded
    /// floating menus and popovers, accent check marks and focus rings.
    Planes,
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
    pub knob_kind: KnobKind,
    pub fader_kind: FaderKind,
    pub button_kind: ButtonKind,
    pub panel_kind: PanelKind,
    /// Strength of the soft glow the newer kinds give lit things (value
    /// arcs, pills, meter blooms, the playhead, the transport's digits;
    /// 0: none).
    pub glow: f32,
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
            knob_kind: KnobKind::Cap,
            fader_kind: FaderKind::Cap,
            button_kind: ButtonKind::Led,
            panel_kind: PanelKind::Hardware,
            glow: 0.0,
        }
    }
}

impl ConsoleLook {
    /// Planes instead of plates (see [`PanelKind::Plane`]).
    pub fn planes(&self) -> bool {
        self.panel_kind == PanelKind::Plane
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
    /// Grid lines at the default strength (see [`GridLook`]): bars,
    /// beats, the grid between beats, finer lines nothing snaps to.
    pub bar_line: Color,
    pub beat_line: Color,
    pub sub_line: Color,
    pub fine_line: Color,
    /// The line at the object a drag snapped to (Snap to Objects).
    pub snap_guide: Color,
    /// The mark of a time-locked track and its clips (placed in time,
    /// not with the music).
    pub time_lock: Color,
    pub loop_on: Color,
    pub loop_off: Color,
    pub playhead: Color,
    /// Recording region and punch range.
    pub record: Color,
    /// Automation button and lanes.
    pub automation: Color,
    /// The clip launcher: a clip playing, one waiting to start or stop.
    pub launch_playing: Color,
    pub launch_queued: Color,
    pub clip_radius: f32,
    pub clip_header: f32,
    /// Opacity of a clip's name strip and of its body: translucent, so the
    /// grid lines show through.
    pub clip_header_alpha: f32,
    pub clip_alpha: f32,
    pub clip_style: ClipStyle,
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
    /// Grid lines at the default strength (see [`GridLook`]).
    pub bar_line: Color,
    pub beat_line: Color,
    pub sub_line: Color,
    pub fine_line: Color,
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

/// The devices' colours: dynamics, limiting, gating, de-essing,
/// saturation, space, time, modulation, instruments, samplers.
pub const DEVICE_ACCENTS: [Color; 11] = [
    Color::hex(0xf0a24a),
    Color::hex(0xf06a5a),
    Color::hex(0x6cc77a),
    Color::hex(0xe07aa8),
    Color::hex(0xf2c94c),
    Color::hex(0x6aa8f0),
    Color::hex(0x4fc3b8),
    Color::hex(0xa98af0),
    Color::hex(0x5fd0e4),
    Color::hex(0xe8925f),
    Color::hex(0xc6e05c),
];

/// Which of [`DeviceTheme::accents`] a device wears.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Accent {
    Dynamics,
    Limiter,
    Gate,
    DeEsser,
    Saturation,
    Space,
    Time,
    Modulation,
    Instrument,
    Sampler,
    /// MIDI effects.
    Midi,
}

impl DeviceTheme {
    pub fn accent(&self, a: Accent) -> Color {
        self.accents[a as usize]
    }
}

/// The built-in devices' editors (the EQ, dynamics, effects, instruments).
#[derive(Clone, Debug)]
pub struct DeviceTheme {
    /// The control deck under a device's display, the sections on it and
    /// their edges.
    pub deck: Color,
    pub section: Color,
    pub section_edge: Color,
    /// Gain reduction (meters, histories).
    pub reduction: Color,
    /// A signal's waveform or level history.
    pub wave: Color,
    /// Each device's own colour (knob rings, highlights), by
    /// [`DeviceTheme::accent`].
    pub accents: [Color; 11],
    /// The display behind the curves, and its grid.
    pub display: Color,
    pub grid: Color,
    pub grid_strong: Color,
    /// The overall response, and the result curves of the bands that work
    /// on the left, right, mid and side only.
    pub curve: Color,
    pub left: Color,
    pub right: Color,
    pub mid: Color,
    pub side: Color,
    /// The analyser: what goes in (a line), what comes out (filled), the
    /// external spectrum (outlined).
    pub pre: Color,
    pub post: Color,
    pub external: Color,
    /// Where the output and the external spectrum collide.
    pub collision: Color,
    /// A dynamic band's range, and how far it moves right now.
    pub dyn_range: Color,
    pub dyn_live: Color,
    /// The floating controls.
    pub panel: Color,
    pub panel_edge: Color,
    /// Text on a band's node.
    pub node_text: Color,
    /// The piano display's keys.
    pub key_white: Color,
    pub key_black: Color,
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
    pub device: DeviceTheme,
    /// How strongly the editors draw their time grid (a preference laid
    /// over every skin, [`Theme::with_grid_strength`]).
    pub grid: GridLook,
    /// How the GTK shell is styled.
    pub chrome: Chrome,
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
                numerals: FontFamily::Mono,
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
                accent_2: accent,
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
                bar_line: Color::rgba(1.0, 1.0, 1.0, GRID_ALPHAS[0]),
                beat_line: Color::rgba(1.0, 1.0, 1.0, GRID_ALPHAS[1]),
                sub_line: Color::rgba(1.0, 1.0, 1.0, GRID_ALPHAS[2]),
                fine_line: Color::rgba(1.0, 1.0, 1.0, GRID_ALPHAS[3]),
                snap_guide: Color::hex(0x6fc3ff),
                time_lock: Color::hex(0x6fc3ff),
                loop_on: accent.with_alpha(0.22),
                loop_off: Color::rgba(1.0, 1.0, 1.0, 0.07),
                playhead: accent,
                record: Color::hex(0xff4b4b),
                automation: Color::hex(0x5fc27a),
                launch_playing: Color::hex(0x4fc36b),
                launch_queued: Color::hex(0xe1c14b),
                clip_radius: 4.0,
                // Tall enough for the clip gain knob.
                clip_header: 22.0,
                clip_header_alpha: 0.82,
                clip_alpha: 0.5,
                clip_style: ClipStyle::Classic,
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
                bar_line: Color::rgba(1.0, 1.0, 1.0, PIANO_GRID_ALPHAS[0]),
                beat_line: Color::rgba(1.0, 1.0, 1.0, PIANO_GRID_ALPHAS[1]),
                sub_line: Color::rgba(1.0, 1.0, 1.0, PIANO_GRID_ALPHAS[2]),
                fine_line: Color::rgba(1.0, 1.0, 1.0, PIANO_GRID_ALPHAS[3]),
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
            device: DeviceTheme {
                deck: Color::hex(0x1c1e23),
                section: Color::hex(0x23262c),
                section_edge: Color::hex(0x30343b),
                reduction: Color::hex(0xf0784a),
                wave: Color::hex(0x8fb4d8),
                accents: DEVICE_ACCENTS,
                display: Color::hex(0x131519),
                grid: Color::rgba(1.0, 1.0, 1.0, 0.055),
                grid_strong: Color::rgba(1.0, 1.0, 1.0, 0.16),
                curve: Color::hex(0xf2c94c),
                left: Color::hex(0xece8df),
                right: Color::hex(0xf05a4f),
                mid: Color::hex(0x5fd07a),
                side: Color::hex(0x5aa8f0),
                pre: Color::hex(0x8796ad),
                post: Color::hex(0xc8d2df),
                external: Color::hex(0xf0806e),
                collision: Color::hex(0xff3b30),
                dyn_range: Color::hex(0xd8473b),
                dyn_live: Color::hex(0xf2c94c),
                panel: Color::hex(0x23262c),
                panel_edge: Color::hex(0x3c414a),
                node_text: Color::hex(0x101114),
                key_white: Color::hex(0xd9d6cf),
                key_black: Color::hex(0x24262b),
            },
            grid: GridLook::default(),
            chrome: Chrome::Classic,
        }
    }

    /// The ink of a name on a card ([`ClipStyle::Card`]) of `color`: the
    /// colour drawn towards the skin's text.
    pub fn card_ink(&self, color: Color) -> Color {
        color.mix(self.ui.text, 0.62)
    }

    /// This skin with its time grid drawn at `strength` (0…1, see
    /// [`GridLook`]).
    pub fn with_grid_strength(mut self, strength: f32) -> Self {
        self.grid = GridLook::new(strength);
        self
    }
}

/// Opacity of the arranger's grid lines at the default strength — bars,
/// beats, subdivisions, fine lines — for a white line on a dark skin
/// (skins scale them by their line strength).
pub const GRID_ALPHAS: [f32; 4] = [0.19, 0.11, 0.075, 0.055];
/// The piano roll's (its rows are lighter than the arranger's lanes).
pub const PIANO_GRID_ALPHAS: [f32; 4] = [0.21, 0.12, 0.08, 0.06];

/// The kinds of grid line, strongest first (an editor maps its timeline's
/// line kinds onto them).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GridEmphasis {
    Bar,
    Beat,
    Subdivision,
    Fine,
}

/// How the editors draw their time grid. The skin gives the colours at
/// the default strength (`ArrangerTheme::{bar,beat,sub,fine}_line`, the
/// piano roll's); `strength` — Preferences → Editing → Grid lines, the
/// same for every skin — scales their opacity and, when strong, widens
/// bar lines. Lines are whole device pixels on device-pixel edges, so they
/// stay crisp (not smeared over two pixels at half strength) on every
/// display, and bars and beats are a logical pixel wide on HiDPI screens.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GridLook {
    /// 0 (faintest) … 1 (strongest).
    pub strength: f32,
}

impl Default for GridLook {
    fn default() -> Self {
        Self {
            strength: Self::DEFAULT,
        }
    }
}

impl GridLook {
    /// The default strength (the middle of the slider).
    pub const DEFAULT: f32 = 0.5;
    /// From this strength on bar lines are a device pixel wider.
    pub const WIDE_FROM: f32 = 0.8;

    pub fn new(strength: f32) -> Self {
        Self {
            strength: if strength.is_finite() {
                strength.clamp(0.0, 1.0)
            } else {
                Self::DEFAULT
            },
        }
    }

    /// The factor on the skin's grid opacities: ⅓ at 0, 1 at the default,
    /// 3 at 1 (even steps of loudness between).
    pub fn gain(self) -> f32 {
        3f32.powf((self.strength - Self::DEFAULT) * 2.0)
    }

    /// `base` (a skin's grid colour) at this strength.
    pub fn color(self, base: Color) -> Color {
        base.with_alpha((base.a * self.gain()).min(1.0))
    }

    /// Width of a line in device pixels on a display with `scale` device
    /// pixels per logical pixel: a logical pixel for bars and beats (so
    /// HiDPI screens show them as plainly), a hairline for the rest; bars
    /// one device pixel more from [`GridLook::WIDE_FROM`].
    pub fn device_width(self, e: GridEmphasis, scale: f32) -> f32 {
        let scale = scale.max(1.0);
        let base = match e {
            GridEmphasis::Bar | GridEmphasis::Beat => scale.round(),
            GridEmphasis::Subdivision | GridEmphasis::Fine => 1.0,
        };
        let wide = e == GridEmphasis::Bar && self.strength >= Self::WIDE_FROM;
        base + if wide { 1.0 } else { 0.0 }
    }

    /// The rectangle of a vertical grid line at `x` (logical pixels) from
    /// `y0` to `y1`: [`GridLook::device_width`] wide, its left edge on a
    /// device pixel.
    pub fn line_rect(self, x: f32, y0: f32, y1: f32, e: GridEmphasis, scale: f32) -> crate::Rect {
        let scale = scale.max(1.0);
        let w = self.device_width(e, scale);
        let left = (x * scale - (w - 1.0) * 0.5).floor() / scale;
        crate::Rect::new(left, y0, w / scale, y1 - y0)
    }

    /// Draw a vertical grid line of colour `base` (the skin's, at the
    /// default strength).
    pub fn vline(
        self,
        p: &mut dyn crate::Painter,
        x: f32,
        y0: f32,
        y1: f32,
        base: Color,
        e: GridEmphasis,
    ) {
        let rect = self.line_rect(x, y0, y1, e, p.scale_factor());
        p.fill(rect, self.color(base));
    }
}

#[cfg(test)]
mod grid_look_tests {
    use super::*;

    #[test]
    fn the_grid_strength_scales_opacity_and_widens_bars_on_device_pixels() {
        let base = Color::rgba(1.0, 1.0, 1.0, GRID_ALPHAS[0]);
        let d = GridLook::default();
        assert_eq!(d.color(base), base, "the skin's colour at the default");
        let faint = GridLook::new(0.0).color(base).a;
        let strong = GridLook::new(1.0).color(base).a;
        assert!((faint - base.a / 3.0).abs() < 1e-4 && (strong - base.a * 3.0).abs() < 1e-4);
        assert!(GridLook::new(0.25).color(base).a < base.a);
        assert_eq!(GridLook::new(7.0).strength, 1.0);
        assert_eq!(GridLook::new(f32::NAN), d);
        // Hairlines stay one device pixel; bars and beats a logical pixel.
        assert_eq!(d.device_width(GridEmphasis::Fine, 2.0), 1.0);
        assert_eq!(d.device_width(GridEmphasis::Bar, 1.0), 1.0);
        assert_eq!(d.device_width(GridEmphasis::Beat, 2.0), 2.0);
        assert_eq!(GridLook::new(0.9).device_width(GridEmphasis::Bar, 1.0), 2.0);
        assert_eq!(
            GridLook::new(0.9).device_width(GridEmphasis::Beat, 1.0),
            1.0
        );
        // On device-pixel edges, whatever x is.
        for scale in [1.0f32, 2.0] {
            for x in [10.0f32, 10.3, 10.5, 10.75] {
                let r = d.line_rect(x, 0.0, 5.0, GridEmphasis::Fine, scale);
                assert_eq!((r.x * scale).fract(), 0.0);
                assert!((r.w * scale - 1.0).abs() < 1e-6);
                assert!(r.x <= x && x < r.x + 1.0 / scale + 1e-6, "{x} at {scale}");
            }
        }
        // Every skin starts at the default and keeps a set strength.
        for t in Theme::all() {
            assert_eq!(t.grid, d);
            assert_eq!(t.with_grid_strength(0.8).grid.strength, 0.8);
        }
    }
}
