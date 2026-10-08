//! The built-in skins besides "Studio": each is built from a compact
//! [`Spec`] — the colours and looks that make it distinct — and everything
//! else is derived consistently.

use crate::Color;
use crate::theme::*;

fn c(hex: u32) -> Color {
    Color::hex(hex)
}

/// What distinguishes a skin.
struct Spec {
    id: &'static str,
    name: &'static str,
    dark: bool,
    fonts: Typography,
    // Shell.
    bg: u32,
    surface: u32,
    surface_alt: u32,
    border: u32,
    text: u32,
    text_dim: u32,
    text_faint: u32,
    accent: u32,
    selection: u32,
    /// LCD background, text, unlit segments.
    lcd: (u32, u32, u32),
    // Console.
    panel: (u32, u32),
    master_panel: (u32, u32),
    panel_label: u32,
    /// Bevel highlight and shadow alphas.
    edges: (f32, f32),
    well: u32,
    well_text: (u32, u32),
    /// Body dark/light, cap top/bottom, pointer, ring track.
    knob: [u32; 6],
    /// Send, pan, trim knob caps.
    knob_caps: [u32; 3],
    /// Fader slot, cap top, cap bottom, scale legends.
    fader: [u32; 4],
    /// Fader caps of audio tracks, buses, auxes, the master.
    fader_caps: [u32; 4],
    /// Meter background, green, yellow, orange, red; unlit brightness.
    meter: ([u32; 5], f32),
    /// Mute, solo, record, monitor, phase.
    leds: [u32; 5],
    /// LED off, its label, the bezel.
    led_off: [u32; 3],
    scribble: (u32, u32),
    // Editors.
    editor_bg: u32,
    lanes: (u32, u32, u32),
    header: (u32, u32, u32),
    ruler: (u32, u32),
    /// Base colour of grid lines and faint overlays, and their strength.
    line: (u32, f32),
    clip_text: u32,
    outline: u32,
    /// White key, its shade, black key, key names.
    keys: [u32; 4],
    look: ConsoleLook,
}

fn build(s: Spec) -> Theme {
    let accent = c(s.accent);
    let line = |a: f32| c(s.line.0).with_alpha((a * s.line.1).min(1.0));
    let k = s.knob;
    let m = s.meter.0;
    let editor = c(s.editor_bg);
    let surface = c(s.surface);
    Theme {
        id: s.id,
        name: s.name,
        dark: s.dark,
        fonts: s.fonts,
        ui: UiPalette {
            background: c(s.bg),
            surface,
            surface_alt: c(s.surface_alt),
            border: c(s.border),
            text: c(s.text),
            text_dim: c(s.text_dim),
            text_faint: c(s.text_faint),
            accent,
            selection: c(s.selection),
            lcd_bg: c(s.lcd.0),
            lcd_text: c(s.lcd.1),
            lcd_dim: c(s.lcd.2),
        },
        console: ConsoleTheme {
            strip_width: 92.0,
            master_width: 112.0,
            strip_gap: 2.0,
            panel_top: c(s.panel.0),
            panel_bottom: c(s.panel.1),
            panel_edge_light: Color::rgba(1.0, 1.0, 1.0, s.edges.0),
            panel_edge_dark: Color::rgba(0.0, 0.0, 0.0, s.edges.1),
            panel_label: c(s.panel_label),
            section_line: Color::rgba(0.0, 0.0, 0.0, s.edges.1 * 0.8),
            master_panel_top: c(s.master_panel.0),
            master_panel_bottom: c(s.master_panel.1),
            well: c(s.well),
            well_text: c(s.well_text.0),
            well_text_empty: c(s.well_text.1),
            knob: KnobStyle {
                body_dark: c(k[0]),
                body_light: c(k[1]),
                cap_top: c(k[2]),
                cap_bottom: c(k[3]),
                pointer: c(k[4]),
                ring_track: c(k[5]),
                shadow: Color::rgba(0.0, 0.0, 0.0, if s.dark { 0.6 } else { 0.35 }),
            },
            send_cap: c(s.knob_caps[0]),
            pan_cap: c(s.knob_caps[1]),
            trim_cap: c(s.knob_caps[2]),
            fader: FaderStyle {
                slot: c(s.fader[0]),
                slot_edge: Color::rgba(1.0, 1.0, 1.0, s.edges.0 * 0.7),
                cap_top: c(s.fader[1]),
                cap_bottom: c(s.fader[2]),
                cap_line: c(0x161616),
                cap_grip: Color::rgba(0.0, 0.0, 0.0, 0.22),
                scale_text: c(s.fader[3]),
                scale_tick: c(s.fader[3]).with_alpha(0.45),
                cap_width: 30.0,
                cap_height: 48.0,
            },
            fader_cap_audio: c(s.fader_caps[0]),
            fader_cap_bus: c(s.fader_caps[1]),
            fader_cap_aux: c(s.fader_caps[2]),
            fader_cap_master: c(s.fader_caps[3]),
            meter: MeterStyle {
                background: c(m[0]),
                green: c(m[1]),
                yellow: c(m[2]),
                orange: c(m[3]),
                red: c(m[4]),
                unlit: s.meter.1,
                peak: c(s.text),
                clip: c(m[4]),
                segment: 2.0,
                gap: 1.0,
                vu_face: c(0xf3dfa8),
                vu_needle: c(0x1a1410),
            },
            led: LedStyle {
                mute: c(s.leds[0]),
                solo: c(s.leds[1]),
                record: c(s.leds[2]),
                monitor: c(s.leds[3]),
                phase: c(s.leds[4]),
                off: c(s.led_off[0]),
                bezel: c(s.led_off[2]),
                label_off: c(s.led_off[1]),
                label_on: c(0x141414),
            },
            scribble_bg: c(s.scribble.0),
            scribble_text: c(s.scribble.1),
            selected_glow: c(s.selection).with_alpha(0.55),
            look: s.look,
        },
        arranger: ArrangerTheme {
            background: editor,
            lane_a: c(s.lanes.0),
            lane_b: c(s.lanes.1),
            lane_selected: c(s.lanes.2),
            header_bg: c(s.header.0),
            header_bg_selected: c(s.header.1),
            header_border: c(s.header.2),
            ruler_bg: c(s.ruler.0),
            ruler_text: c(s.ruler.1),
            bar_line: line(0.10),
            beat_line: line(0.045),
            sub_line: line(0.022),
            loop_on: accent.with_alpha(0.22),
            loop_off: line(0.07),
            playhead: accent,
            record: c(s.leds[2]),
            automation: c(s.leds[1]),
            launch_playing: c(m[1]),
            launch_queued: c(m[2]),
            clip_radius: 4.0,
            clip_header: 22.0,
            clip_text: c(s.clip_text),
            selection_outline: c(s.outline),
            track_height: 72.0,
            header_width: 252.0,
            ruler_height: 30.0,
        },
        piano: PianoRollTheme {
            background: editor,
            white_row: c(s.lanes.0),
            black_row: c(s.lanes.1),
            octave_line: line(0.09),
            bar_line: line(0.12),
            beat_line: line(0.05),
            sub_line: line(0.025),
            key_white: c(s.keys[0]),
            key_white_shade: c(s.keys[1]),
            key_black: c(s.keys[2]),
            key_text: c(s.keys[3]),
            velocity_bg: editor.darken(0.12),
            toolbar: c(s.header.0),
            button: c(s.surface_alt),
            button_active: accent.mix(surface, 0.62),
            off_scale: Color::rgba(0.0, 0.0, 0.0, if s.dark { 0.32 } else { 0.08 }),
            root_row: accent.with_alpha(0.07),
            ghost_note: line(0.16),
            step_cursor: c(s.leds[0]),
            lane_curve: c(s.selection),
            rubber_band: c(s.selection).with_alpha(0.18),
            keyboard_width: 66.0,
            row_height: 13.0,
            velocity_height: 96.0,
            ruler_height: 24.0,
            toolbar_height: 32.0,
        },
        perf: PerformanceTheme {
            background: c(s.bg),
            panel: surface,
            header: c(s.header.0),
            row_a: c(s.lanes.0),
            row_b: c(s.lanes.1),
            row_hover: c(s.lanes.2),
            plugin_row: editor,
            bar_track: c(s.well),
            load_ok: c(m[1]),
            load_warn: c(m[2]),
            load_high: c(m[3]),
            load_critical: c(m[4]),
            peak_mark: c(s.text),
            graph_grid: line(0.06),
            graph_avg: c(m[1]),
            graph_peak: accent,
            engine_share: c(s.selection),
            row_height: 30.0,
            plugin_row_height: 24.0,
            summary_height: 152.0,
        },
        tools: ToolsTheme {
            background: c(s.bg),
            panel: surface,
            header: c(s.header.0),
            well: c(s.well),
            grid: line(0.07),
            level_ok: c(m[1]),
            level_warn: c(m[2]),
            level_over: c(m[4]),
            rms: c(m[1]).darken(0.35),
            hold: c(s.text),
            spectrum: c(s.selection),
            spectrum_peak: accent,
            target: accent,
            goniometer: c(m[1]),
            readout: c(s.text),
            toolbar_height: 32.0,
        },
        device: DeviceTheme {
            deck: c(s.bg),
            section: surface,
            section_edge: c(s.border),
            reduction: c(m[3]),
            wave: c(s.selection),
            accents: DEVICE_ACCENTS,
            display: c(s.well),
            grid: line(0.06),
            grid_strong: line(0.18),
            curve: accent,
            left: c(s.text),
            right: c(s.leds[2]),
            mid: c(m[1]),
            side: c(s.selection),
            pre: c(s.text_dim),
            post: c(s.text),
            external: c(s.leds[2]).mix(c(s.text), 0.35),
            collision: c(m[4]),
            dyn_range: c(m[4]),
            dyn_live: accent,
            panel: surface,
            panel_edge: c(s.border),
            node_text: if s.dark { c(s.bg) } else { c(s.text) },
            key_white: c(s.keys[0]),
            key_black: c(s.keys[2]),
        },
    }
}

fn fonts() -> Typography {
    Typography {
        tiny: 8.5,
        small: 10.0,
        normal: 11.5,
        large: 13.5,
        display: 20.0,
    }
}

/// "Daylight": a bright studio — light grey console, white editors.
pub fn daylight() -> Theme {
    build(Spec {
        id: "daylight",
        name: "Daylight",
        dark: false,
        fonts: fonts(),
        bg: 0xe7e5e1,
        surface: 0xf3f2ef,
        surface_alt: 0xdad8d3,
        border: 0xb5b2ab,
        text: 0x1d1e20,
        text_dim: 0x54575c,
        text_faint: 0x8c8f94,
        accent: 0xe2541f,
        selection: 0x2f7fd6,
        lcd: (0xc6d1b2, 0x1c2516, 0xa6b192),
        panel: (0xdedcd7, 0xc6c4be),
        master_panel: (0xd8d0c3, 0xbdb4a5),
        panel_label: 0x3a3a38,
        edges: (0.65, 0.28),
        well: 0x2a2c2f,
        well_text: (0xece5d0, 0x6c6f74),
        knob: [0x2c2d30, 0x585b61, 0xf2f1ed, 0xb6b5b0, 0x1c1c1c, 0xa5a39d],
        knob_caps: [0x2f7fd6, 0x6c7077, 0xc0392b],
        fader: [0x3a3b3e, 0xfbfbf9, 0xa8a7a2, 0x45464a],
        fader_caps: [0xe9e7e1, 0x5f8fd0, 0x59a87a, 0xd4473c],
        meter: ([0x1a1b1c, 0x32c060, 0xe2c13a, 0xf08a3a, 0xff3d3d], 0.16),
        leds: [0xf2a51e, 0x3fbf5a, 0xf03c3c, 0x2f8fe6, 0xa86be8],
        led_off: [0xc9c7c1, 0x3a3a38, 0x8f8d88],
        scribble: (0xfffdf6, 0x1f1d19),
        editor_bg: 0xf0efec,
        lanes: (0xf7f6f3, 0xeeede9, 0xe1e7ef),
        header: (0xe5e3de, 0xd6dfea, 0xbcb9b3),
        ruler: (0xe0ded9, 0x45474b),
        line: (0x000000, 1.3),
        clip_text: 0x141414,
        outline: 0x1d1e20,
        keys: [0xfcfbf8, 0xd8d6d0, 0x2c2d30, 0x7a7d82],
        look: ConsoleLook {
            sheen: 0.6,
            engrave: Color::rgba(1.0, 1.0, 1.0, 0.7),
            ..ConsoleLook::default()
        },
    })
}

/// "Vintage": a 1970s console — enamel panels, bakelite skirted knobs,
/// plasma bar-graph meters, walnut cheeks, amber displays.
pub fn vintage() -> Theme {
    build(Spec {
        id: "vintage",
        name: "Vintage Console",
        dark: true,
        fonts: fonts(),
        bg: 0x1c1814,
        surface: 0x26211b,
        surface_alt: 0x302921,
        border: 0x0e0b08,
        text: 0xefe6d2,
        text_dim: 0xa89c86,
        text_faint: 0x6b6152,
        accent: 0xe0a040,
        selection: 0xd9b46a,
        lcd: (0x120c05, 0xffb547, 0x5c3c12),
        panel: (0x5d6670, 0x48505a),
        master_panel: (0x6b2f2c, 0x4f1f1c),
        panel_label: 0xefe3c4,
        edges: (0.14, 0.55),
        well: 0x0f0d0a,
        well_text: (0xf2c46b, 0x5a4d38),
        knob: [0x17130f, 0x3a322a, 0xe9e2d3, 0xa89f8e, 0xf7f0de, 0x1a1612],
        knob_caps: [0x3f6ea8, 0x9a9488, 0xa8352d],
        fader: [0x0a0807, 0xf3efe6, 0x9b9385, 0xe6dcc3],
        fader_caps: [0xefe7d6, 0x3f6ea8, 0x5f9a6a, 0xb53a31],
        // Neon-orange plasma: amber low, brighter towards 0 dBFS, red over.
        meter: ([0x100806, 0xe9732a, 0xff9a3c, 0xffc35e, 0xff3b24], 0.13),
        leds: [0xf0b33a, 0x69c46b, 0xff4b3b, 0x6fb1e8, 0xcf9be8],
        led_off: [0x3b342c, 0xe8dcc0, 0x0c0a08],
        scribble: (0xefe2bd, 0x2a2114),
        editor_bg: 0x1d1915,
        lanes: (0x221d18, 0x1e1a16, 0x2c261f),
        header: (0x2a241e, 0x352d25, 0x0f0c0a),
        ruler: (0x26201a, 0xc9bb9f),
        line: (0xfff0d0, 1.0),
        clip_text: 0x15110d,
        outline: 0xf5ead2,
        keys: [0xefe6d2, 0xc9bfa9, 0x1a1612, 0x7a6e5a],
        look: ConsoleLook {
            knob_skirt: Some(c(0x1d1915)),
            knob_ring: false,
            sheen: 0.3,
            brushed: 0.45,
            screws: true,
            wood: Some((c(0x8a5530), c(0x4f2d17))),
            meter: MeterKind::Plasma,
            engrave: Color::rgba(0.0, 0.0, 0.0, 0.6),
            flat: false,
        },
    })
}

/// "Midnight": near-black, flat controls, electric blue.
pub fn midnight() -> Theme {
    build(Spec {
        id: "midnight",
        name: "Midnight",
        dark: true,
        fonts: fonts(),
        bg: 0x050607,
        surface: 0x0c0e11,
        surface_alt: 0x13161a,
        border: 0x000000,
        text: 0xe8edf2,
        text_dim: 0x8a929c,
        text_faint: 0x4a5058,
        accent: 0x3da5ff,
        selection: 0x3da5ff,
        lcd: (0x000000, 0x5ec8ff, 0x12384f),
        panel: (0x101317, 0x0a0c0e),
        master_panel: (0x121a24, 0x0b1017),
        panel_label: 0x9aa6b2,
        edges: (0.05, 0.8),
        well: 0x000000,
        well_text: (0x9fd8ff, 0x2f3740),
        knob: [0x15191e, 0x1f242b, 0x2a3038, 0x1c2127, 0xe8edf2, 0x0f1215],
        knob_caps: [0x3da5ff, 0x8a929c, 0xff5470],
        fader: [0x000000, 0x2b3139, 0x1a1e23, 0x6c7680],
        fader_caps: [0xc8d2dc, 0x3da5ff, 0x3ddc97, 0xff5470],
        meter: ([0x000000, 0x2bd9a0, 0xe6e15a, 0xff9e3d, 0xff4d6a], 0.08),
        leds: [0xffb02e, 0x2bd9a0, 0xff4d6a, 0x3da5ff, 0xb48cff],
        led_off: [0x14181d, 0x8a929c, 0x000000],
        scribble: (0x1a1f26, 0xe8edf2),
        editor_bg: 0x07080a,
        lanes: (0x0b0d10, 0x08090b, 0x111821),
        header: (0x0e1115, 0x15202c, 0x000000),
        ruler: (0x0c0e11, 0x9aa6b2),
        line: (0xffffff, 1.0),
        clip_text: 0x050607,
        outline: 0xe8edf2,
        keys: [0xcfd6dd, 0x9ea8b2, 0x0b0d10, 0x4a5058],
        look: ConsoleLook {
            sheen: 0.0,
            meter: MeterKind::Bar,
            engrave: Color::TRANSPARENT,
            flat: true,
            ..ConsoleLook::default()
        },
    })
}

/// "Frost": cool slate blues, soft and calm.
pub fn frost() -> Theme {
    build(Spec {
        id: "frost",
        name: "Frost",
        dark: true,
        fonts: fonts(),
        bg: 0x222833,
        surface: 0x2a313c,
        surface_alt: 0x333b47,
        border: 0x181d25,
        text: 0xe6eaf1,
        text_dim: 0xa2acbc,
        text_faint: 0x697384,
        accent: 0x8cc6d8,
        selection: 0x86a6c6,
        lcd: (0x1a2028, 0x92d4e2, 0x37525d),
        panel: (0x3a4351, 0x2e3541),
        master_panel: (0x3c4959, 0x303b49),
        panel_label: 0xc8d2e0,
        edges: (0.10, 0.5),
        well: 0x1a1f27,
        well_text: (0xd8e3ef, 0x4c5666),
        knob: [0x1e242d, 0x47515e, 0x6a7585, 0x39424f, 0xedf0f5, 0x1a1f27],
        knob_caps: [0x86a6c6, 0xa2acbc, 0xc2666f],
        fader: [0x141920, 0xe6eaf1, 0x8c96a5, 0xa2acbc],
        fader_caps: [0xd9dfea, 0x86a6c6, 0xa6c290, 0xc2666f],
        meter: ([0x11151b, 0xa6c290, 0xeccf8f, 0xd38b74, 0xc2666f], 0.14),
        leds: [0xeccf8f, 0xa6c290, 0xc2666f, 0x8cc6d8, 0xb792b1],
        led_off: [0x2d3441, 0xc8d2e0, 0x151a21],
        scribble: (0xedf0f5, 0x2d3340),
        editor_bg: 0x252b35,
        lanes: (0x29303a, 0x262c36, 0x303947),
        header: (0x2d3441, 0x364153, 0x181d25),
        ruler: (0x2a303b, 0xc8d2e0),
        line: (0xedf0f5, 1.0),
        clip_text: 0x1c2028,
        outline: 0xedf0f5,
        keys: [0xe6eaf1, 0xb8c0cd, 0x242a34, 0x697384],
        look: ConsoleLook {
            sheen: 0.4,
            engrave: Color::rgba(0.0, 0.0, 0.0, 0.4),
            ..ConsoleLook::default()
        },
    })
}

/// "Neon": synthwave night — violet panels, magenta and cyan light.
pub fn neon() -> Theme {
    build(Spec {
        id: "neon",
        name: "Neon",
        dark: true,
        fonts: fonts(),
        bg: 0x120d1f,
        surface: 0x1a1430,
        surface_alt: 0x231b3d,
        border: 0x07040f,
        text: 0xf5e9ff,
        text_dim: 0xb39ccc,
        text_faint: 0x6d5a87,
        accent: 0xff3ea5,
        selection: 0x36e6ff,
        lcd: (0x0a0614, 0x36e6ff, 0x1d4a5c),
        panel: (0x2a1f47, 0x1c1533),
        master_panel: (0x3a1838, 0x26102a),
        panel_label: 0xe0c8ff,
        edges: (0.08, 0.6),
        well: 0x0a0614,
        well_text: (0xff8ad0, 0x4a3a63),
        knob: [0x0f0a1c, 0x3a2c5c, 0x5a4590, 0x2a1f47, 0x36e6ff, 0x0a0614],
        knob_caps: [0x36e6ff, 0xb39ccc, 0xff3ea5],
        fader: [0x07040f, 0xf0d8ff, 0x8a6fb3, 0xb39ccc],
        fader_caps: [0xe8d8ff, 0x36e6ff, 0x7dffb2, 0xff3ea5],
        meter: ([0x07040f, 0x36e6ff, 0xb388ff, 0xff6ad5, 0xff2d55], 0.12),
        leds: [0xffd23f, 0x7dffb2, 0xff2d55, 0x36e6ff, 0xc77dff],
        led_off: [0x2a1f47, 0xe0c8ff, 0x07040f],
        scribble: (0x1f1636, 0xff8ad0),
        editor_bg: 0x140e24,
        lanes: (0x19122d, 0x150f27, 0x24183f),
        header: (0x1c1533, 0x2a1f4d, 0x07040f),
        ruler: (0x1a1430, 0xe0c8ff),
        line: (0xf0d8ff, 1.1),
        clip_text: 0x0d0818,
        outline: 0x36e6ff,
        keys: [0xeadcff, 0xb8a2d8, 0x120d1f, 0x6d5a87],
        look: ConsoleLook {
            sheen: 0.25,
            engrave: Color::rgba(0.0, 0.0, 0.0, 0.5),
            flat: true,
            ..ConsoleLook::default()
        },
    })
}

/// "Fjord": dusk over deep water — dark slate with a hint of teal, petrol
/// console panels, a soft coral and old-gold light; calm contrast for long
/// sessions.
pub fn fjord() -> Theme {
    build(Spec {
        id: "fjord",
        name: "Fjord",
        dark: true,
        fonts: fonts(),
        bg: 0x1d2427,
        surface: 0x242c30,
        surface_alt: 0x2e373c,
        border: 0x13181b,
        text: 0xdedcd4,
        text_dim: 0xa3a8a5,
        text_faint: 0x6b7476,
        accent: 0xe98a6d,
        selection: 0x82aadb,
        lcd: (0x111618, 0xe6c27a, 0x3b3a2e),
        panel: (0x34494b, 0x283b3d),
        master_panel: (0x3f4741, 0x2f3632),
        panel_label: 0xd5dfda,
        edges: (0.08, 0.5),
        well: 0x111518,
        well_text: (0xe8d8b0, 0x4b5557),
        knob: [0x161b1d, 0x3b4a4c, 0x5e7072, 0x2f3c3e, 0xf1ede3, 0x141a1c],
        knob_caps: [0x82aadb, 0xa3a8a5, 0xe98a6d],
        fader: [0x0e1214, 0xebe8df, 0x8f9891, 0xa3a8a5],
        fader_caps: [0xdcd9cf, 0x82aadb, 0x93be86, 0xe98a6d],
        meter: ([0x0b0e0f, 0x93c27a, 0xe3c35e, 0xea955b, 0xec6b5f], 0.12),
        leds: [0xe4b155, 0x93c27a, 0xec6b5f, 0x82aadb, 0xc49ad5],
        led_off: [0x263133, 0xd5dfda, 0x0e1214],
        scribble: (0xe9e4d6, 0x23292b),
        editor_bg: 0x20272b,
        lanes: (0x242c30, 0x21282c, 0x2b3640),
        header: (0x283135, 0x324049, 0x161b1e),
        ruler: (0x252d31, 0xc9cfcc),
        line: (0xe6efe9, 1.0),
        clip_text: 0x15191b,
        outline: 0xf1ede3,
        keys: [0xe3e0d7, 0xb8bab3, 0x1c2124, 0x6b7476],
        look: ConsoleLook {
            sheen: 0.3,
            engrave: Color::rgba(0.0, 0.0, 0.0, 0.45),
            ..ConsoleLook::default()
        },
    })
}

/// "High Contrast": black and white with strong colours and larger type.
pub fn high_contrast() -> Theme {
    build(Spec {
        id: "contrast",
        name: "High Contrast",
        dark: true,
        fonts: Typography {
            tiny: 9.5,
            small: 11.0,
            normal: 12.5,
            large: 14.5,
            display: 22.0,
        },
        bg: 0x000000,
        surface: 0x000000,
        surface_alt: 0x1a1a1a,
        border: 0xffffff,
        text: 0xffffff,
        text_dim: 0xe6e6e6,
        text_faint: 0xbdbdbd,
        accent: 0xffd400,
        selection: 0x00e5ff,
        lcd: (0x000000, 0xffd400, 0x5c4d00),
        panel: (0x141414, 0x0a0a0a),
        master_panel: (0x1c1a00, 0x0f0e00),
        panel_label: 0xffffff,
        edges: (0.55, 0.9),
        well: 0x000000,
        well_text: (0xffffff, 0x9e9e9e),
        knob: [0x000000, 0x000000, 0xffffff, 0xd0d0d0, 0x000000, 0x5c5c5c],
        knob_caps: [0x00e5ff, 0xffffff, 0xff5252],
        fader: [0x000000, 0xffffff, 0xd0d0d0, 0xffffff],
        fader_caps: [0xffffff, 0x00e5ff, 0x69f0ae, 0xffd400],
        meter: ([0x000000, 0x00e676, 0xffea00, 0xff9100, 0xff1744], 0.22),
        leds: [0xffd400, 0x00e676, 0xff1744, 0x00e5ff, 0xe040fb],
        led_off: [0x2b2b2b, 0xffffff, 0xffffff],
        scribble: (0xffffff, 0x000000),
        editor_bg: 0x000000,
        lanes: (0x0a0a0a, 0x000000, 0x1f2a33),
        header: (0x0d0d0d, 0x1f2a33, 0xffffff),
        ruler: (0x000000, 0xffffff),
        line: (0xffffff, 2.0),
        clip_text: 0x000000,
        outline: 0xffffff,
        keys: [0xffffff, 0xcfcfcf, 0x000000, 0x000000],
        look: ConsoleLook {
            sheen: 0.0,
            engrave: Color::TRANSPARENT,
            flat: true,
            ..ConsoleLook::default()
        },
    })
}

/// A console family's mixer look (`faderframe_project::console::FAMILIES`
/// order), in FaderFrame's own design language: the console section's
/// panels, knobs, faders, meters and finish change, everything else stays
/// the skin's. Not any real console's livery.
struct Family {
    panel: (u32, u32),
    master_panel: (u32, u32),
    label: u32,
    /// Body dark/light, cap top/bottom, pointer, ring track.
    knob: [u32; 6],
    /// Send, pan, trim caps.
    caps: [u32; 3],
    /// Audio, bus, aux, master fader caps.
    faders: [u32; 4],
    scribble: (u32, u32),
    well: u32,
    /// Its wood (light, dark): its cheeks when the look has them, or when
    /// the skin has wooden cheeks of its own (Vintage Console).
    wood: (u32, u32),
    look: ConsoleLook,
}

fn family(f: usize) -> Family {
    match f {
        // Graphite and brass: brushed dark metal, brass pointers, moving
        // coil meters.
        0 => Family {
            panel: (0x36393e, 0x27292d),
            master_panel: (0x2b2d31, 0x1d1f22),
            label: 0xe9e1cc,
            knob: [0x111214, 0x2d2f33, 0x3b3d41, 0x1b1c1f, 0xd9b56b, 0x101113],
            caps: [0x3a3d42, 0x8c7a52, 0x7d322b],
            faders: [0x1d1e21, 0x8c7a52, 0x4f6b57, 0x9a3b2f],
            scribble: (0xe4d9bd, 0x221d14),
            well: 0x101113,
            // Walnut.
            wood: (0x5e3d26, 0x301e12),
            look: ConsoleLook {
                knob_skirt: Some(c(0x1b1c1f)),
                knob_ring: false,
                sheen: 0.25,
                brushed: 0.55,
                screws: true,
                wood: None,
                meter: MeterKind::Ladder,
                engrave: Color::rgba(0.0, 0.0, 0.0, 0.6),
                flat: false,
            },
        },
        // Slate and colour: light caps, colour-coded sections, bar meters.
        1 => Family {
            panel: (0x4b5158, 0x3c4147),
            master_panel: (0x3c4147, 0x2f3338),
            label: 0xf1f2ef,
            knob: [0x1f2225, 0x3d4247, 0xdcddd8, 0xa6a8a4, 0x1b1d1f, 0x1a1c1e],
            caps: [0x2f6f8f, 0xc9a227, 0xa83a32],
            faders: [0xdcddd8, 0x2f6f8f, 0x4f8a55, 0xa83a32],
            scribble: (0xf3f1e8, 0x1d2024),
            well: 0x16181b,
            // Ash.
            wood: (0xb4925f, 0x735634),
            look: ConsoleLook {
                knob_skirt: None,
                knob_ring: false,
                sheen: 0.35,
                brushed: 0.0,
                screws: false,
                wood: None,
                meter: MeterKind::Bar,
                engrave: Color::rgba(0.0, 0.0, 0.0, 0.45),
                flat: false,
            },
        },
        // Taupe and Bakelite: warm plates, dark brown knobs with printed
        // scales, mahogany cheeks, a plasma column.
        3 => Family {
            panel: (0x6e675c, 0x575148),
            master_panel: (0x4a3628, 0x35261c),
            label: 0xf4ecd8,
            knob: [0x1a120d, 0x3a2a20, 0x5a3e2c, 0x2e2018, 0xeee2c4, 0x140e0a],
            caps: [0x4a3424, 0x8a6a48, 0x8e3a28],
            faders: [0xece2cc, 0x7a5a3c, 0x5e7058, 0x8e3a28],
            scribble: (0xf0e6cc, 0x2a1f14),
            well: 0x16110c,
            // Mahogany.
            wood: (0x7a3b26, 0x3f1c12),
            look: ConsoleLook {
                knob_skirt: Some(c(0x2c2620)),
                knob_ring: false,
                sheen: 0.3,
                brushed: 0.0,
                screws: true,
                wood: Some((c(0x7a3b26), c(0x3f1c12))),
                meter: MeterKind::Plasma,
                engrave: Color::rgba(0.0, 0.0, 0.0, 0.6),
                flat: false,
            },
        },
        // Hammertone green-grey: textured plates, black knobs, oak cheeks
        // with the skin's, an LED ladder.
        4 => Family {
            panel: (0x55605a, 0x434c47),
            master_panel: (0x3f4843, 0x2f3632),
            label: 0xeef1ea,
            knob: [0x121413, 0x2e3330, 0x343936, 0x161917, 0xe8ecdf, 0x0f1110],
            caps: [0x2e3330, 0x7d8a72, 0x8a3a30],
            faders: [0xe4e8dc, 0x5e7a68, 0x6a7f86, 0x8a3a30],
            scribble: (0xeef0e4, 0x1c211e),
            well: 0x101211,
            // Oak.
            wood: (0x9a7444, 0x5a4022),
            look: ConsoleLook {
                knob_skirt: None,
                knob_ring: false,
                sheen: 0.2,
                brushed: 0.35,
                screws: true,
                wood: None,
                meter: MeterKind::Ladder,
                engrave: Color::rgba(0.0, 0.0, 0.0, 0.55),
                flat: false,
            },
        },
        // Light enamel grey: broadcast-plain plates, dark legends, black
        // knobs, beech cheeks with the skin's, a bar.
        5 => Family {
            panel: (0x9ea19b, 0x878a84),
            master_panel: (0x7c7f7a, 0x666964),
            label: 0x171816,
            knob: [0x0f1010, 0x2a2c2b, 0x2e302f, 0x121313, 0xf0f0ea, 0x0c0d0d],
            caps: [0x2e302f, 0x5d6a74, 0x9a3a2c],
            faders: [0x242625, 0x4f6878, 0x5d7a5e, 0x9a3a2c],
            scribble: (0xf4f4ee, 0x1a1b19),
            well: 0x1a1b1a,
            // Beech.
            wood: (0xb98b5c, 0x7a5636),
            look: ConsoleLook {
                knob_skirt: None,
                knob_ring: false,
                sheen: 0.45,
                brushed: 0.0,
                screws: true,
                wood: None,
                meter: MeterKind::Bar,
                engrave: Color::rgba(1.0, 1.0, 1.0, 0.25),
                flat: false,
            },
        },
        // Pewter and walnut: warm grey-green plates, printed knob scales,
        // wooden cheeks, moving coil meters.
        _ => Family {
            panel: (0x5a5e57, 0x474a44),
            master_panel: (0x4c3a2c, 0x382a1f),
            label: 0xf1ead8,
            knob: [0x1c1a17, 0x3b3833, 0x8e8c86, 0x5f5d58, 0xf3eee0, 0x171512],
            caps: [0x5f5d58, 0x8e8c86, 0x8d2f27],
            faders: [0xe8e2d3, 0x6c7a5d, 0x5d7a78, 0x8d2f27],
            scribble: (0xeee6d0, 0x241f17),
            well: 0x14130f,
            // Teak.
            wood: (0x8a5a32, 0x4a2c18),
            look: ConsoleLook {
                knob_skirt: Some(c(0x2a2925)),
                knob_ring: false,
                sheen: 0.3,
                brushed: 0.2,
                screws: true,
                wood: Some((c(0x8a5a32), c(0x4a2c18))),
                meter: MeterKind::Plasma,
                engrave: Color::rgba(0.0, 0.0, 0.0, 0.6),
                flat: false,
            },
        },
    }
}

impl Theme {
    /// This skin with console family `f`'s mixer look (see [`Family`]).
    pub fn with_console_family(&self, f: usize) -> Theme {
        let s = family(f);
        let mut t = self.clone();
        let k = &mut t.console;
        (k.panel_top, k.panel_bottom) = (c(s.panel.0), c(s.panel.1));
        (k.master_panel_top, k.master_panel_bottom) = (c(s.master_panel.0), c(s.master_panel.1));
        k.panel_label = c(s.label);
        k.panel_edge_light = Color::rgba(1.0, 1.0, 1.0, 0.12);
        k.panel_edge_dark = Color::rgba(0.0, 0.0, 0.0, 0.55);
        k.section_line = Color::rgba(0.0, 0.0, 0.0, 0.44);
        k.well = c(s.well);
        k.knob.body_dark = c(s.knob[0]);
        k.knob.body_light = c(s.knob[1]);
        k.knob.cap_top = c(s.knob[2]);
        k.knob.cap_bottom = c(s.knob[3]);
        k.knob.pointer = c(s.knob[4]);
        k.knob.ring_track = c(s.knob[5]);
        (k.send_cap, k.pan_cap, k.trim_cap) = (c(s.caps[0]), c(s.caps[1]), c(s.caps[2]));
        k.fader_cap_audio = c(s.faders[0]);
        k.fader_cap_bus = c(s.faders[1]);
        k.fader_cap_aux = c(s.faders[2]);
        k.fader_cap_master = c(s.faders[3]);
        (k.scribble_bg, k.scribble_text) = (c(s.scribble.0), c(s.scribble.1));
        let skin_wood = k.look.wood.is_some();
        k.look = s.look;
        if skin_wood {
            k.look.wood = Some((c(s.wood.0), c(s.wood.1)));
        }
        t
    }

    /// Every built-in skin, the default first.
    pub fn all() -> Vec<Theme> {
        vec![
            Theme::studio(),
            daylight(),
            vintage(),
            midnight(),
            frost(),
            fjord(),
            neon(),
            high_contrast(),
        ]
    }

    /// The skin with this id (the default if unknown).
    pub fn by_id(id: &str) -> Theme {
        Theme::all()
            .into_iter()
            .find(|t| t.id == id)
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skins_are_distinct_and_found_by_id() {
        let all = Theme::all();
        assert_eq!(all.len(), 8);
        let mut ids: Vec<_> = all.iter().map(|t| t.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), all.len());
        assert!(all.iter().any(|t| !t.dark), "a light skin");
        assert_eq!(Theme::by_id("vintage").name, "Vintage Console");
        assert_eq!(Theme::by_id("nope").id, "studio");
        // The console looks change the console section only, each its
        // own, its labels readable on its panels.
        let base = Theme::studio();
        for f in 0..6 {
            let t = base.with_console_family(f);
            assert_eq!(t.ui.text, base.ui.text);
            assert_ne!(t.console.panel_top, base.console.panel_top);
            let (a, b) = (
                t.console.panel_label.luminance(),
                t.console.panel_top.luminance(),
            );
            assert!((a - b).abs() > 0.4, "family {f}: label contrast");
            // Meters that show the level (a VU face fills a narrow strip).
            assert_ne!(t.console.look.meter, MeterKind::Edgewise, "family {f}");
            // A skin with wooden cheeks keeps them, in the family's wood.
            assert!(
                Theme::by_id("vintage")
                    .with_console_family(f)
                    .console
                    .look
                    .wood
                    .is_some()
            );
        }
        // Text stands out from the background in every skin.
        for t in &all {
            let (a, b) = (t.ui.text.luminance(), t.ui.background.luminance());
            assert!((a - b).abs() > 0.5, "{}: text contrast", t.id);
        }
    }
}
