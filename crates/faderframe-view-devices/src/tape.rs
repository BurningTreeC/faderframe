//! The Tape Machine's face: two VU meters reading the tape level (0 VU =
//! the operating level), between them what the machine does at its
//! settings — its small-signal response (the head bump, the top the speed
//! leaves) and the tape's magnetisation against the recording field (the
//! loop closed when the bias is aligned, opening as it is turned under),
//! with the machine's line: speed, equalisation, level, the weighted wow
//! and flutter — and the deck: input and output, the tape, the alignment
//! and the transport.

use crate::fet76::Needle;
use crate::kit::{self, Ctl, Ctx, Face, KNOB, Panel, SMALL, SWITCH_H};
use faderframe_core::ParameterId;
use faderframe_plugin_host::devices::tape::{
    self as tape, FORMULATIONS, LEVEL_NAMES, SPEED_NAMES, id, value,
};
use faderframe_ui_canvas::{
    Accent, Color, FontWeight, Paint, Painter, Path, Point, Rect, Size, TextStyle, controls,
};

fn pid(i: u32) -> ParameterId {
    ParameterId(i)
}

pub(crate) struct TapeFace {
    accent: Color,
    pub(crate) needles: [Needle; 2],
    /// The response (dB) on a log grid and the loop, for `drawn`.
    response: Vec<f64>,
    hysteresis: Vec<(f64, f64)>,
    /// What the curves were drawn for.
    drawn: Option<tape::Response>,
}

/// The response's frequencies: 20 Hz to 20 kHz, log spaced.
const POINTS: usize = 96;

fn freq(i: usize) -> f64 {
    20.0 * 1000f64.powf(i as f64 / (POINTS - 1) as f64)
}

/// The response's range (dB each way).
const RANGE: f64 = 9.0;
/// The caption line under each VU meter.
const CAPTION_H: f32 = 12.0;

impl TapeFace {
    pub fn new(theme: &faderframe_ui_canvas::Theme) -> Self {
        Self {
            accent: theme.device.accent(Accent::Saturation),
            needles: [Needle::default(); 2],
            response: Vec::new(),
            hysteresis: Vec::new(),
            drawn: None,
        }
    }

    fn settings(cx: &Ctx<'_>) -> tape::Response {
        let pick = |id: u32, n: usize| (cx.value(pid(id)).round().max(0.0) as usize).min(n - 1);
        tape::Response {
            speed: pick(id::SPEED, 4),
            iec: cx.on(pid(id::EQ)),
            tape: pick(id::TAPE, 3),
            format: pick(id::FORMAT, 2),
            bias: cx.value(pid(id::BIAS)),
            factor: tape::factor(cx.value(pid(id::OVERSAMPLING))),
        }
    }

    /// Work the curves out again when the settings moved.
    fn follow(&mut self, d: tape::Response) {
        if self.drawn == Some(d) {
            return;
        }
        self.drawn = Some(d);
        self.response = (0..POINTS)
            .map(|i| tape::response_db(&d, freq(i)))
            .collect();
        self.hysteresis = tape::hysteresis_loop(d.tape, d.bias, 0.0, 160);
    }

    /// The display's parts: the meters either side, the response and the
    /// loop between them under the machine's line.
    fn split(r: Rect) -> (Rect, Rect, Rect, Rect, Rect) {
        let vu_w = (r.w * 0.24).clamp(120.0, 230.0);
        let vu_h = (vu_w * 0.72).min(r.h - 16.0);
        let top = r.y + (r.h - vu_h) / 2.0;
        let left = Rect::new(r.x + 10.0, top, vu_w, vu_h);
        let right = Rect::new(r.right() - 10.0 - vu_w, top, vu_w, vu_h);
        let mid_x = left.right() + 14.0;
        let mid_w = right.x - 14.0 - mid_x;
        let line = Rect::new(mid_x, r.y + 8.0, mid_w, 16.0);
        let body_y = line.bottom() + 6.0;
        let body_h = r.bottom() - 10.0 - body_y;
        let curve_w = mid_w * 0.6 - 6.0;
        let response = Rect::new(mid_x, body_y, curve_w, body_h);
        let bh = Rect::new(
            response.right() + 12.0,
            body_y,
            mid_w - curve_w - 12.0,
            body_h,
        );
        (left, right, line, response, bh)
    }

    /// Text on the display: the theme's text or background colour,
    /// whichever stands out from the display (a light theme keeps a dark
    /// display), and a dimmer one for labels.
    fn ink(th: &faderframe_ui_canvas::Theme) -> (Color, Color) {
        let display = th.device.display;
        let ink = if th.ui.text.contrast(display) >= th.ui.background.contrast(display) {
            th.ui.text
        } else {
            th.ui.background
        };
        (ink, ink.mix(display, 0.35))
    }

    fn paint_response(&self, p: &mut dyn Painter, r: Rect, cx: &Ctx<'_>) {
        let th = cx.theme;
        p.fill_rounded(r, 3.0, &Paint::Solid(th.device.display.darken(0.12)));
        let x_of = |f: f64| r.x + r.w * ((f / 20.0).log10() / 3.0) as f32;
        let y_of = |db: f64| r.y + r.h * (0.5 - (db / (2.0 * RANGE)) as f32).clamp(0.0, 1.0);
        for f in [100.0, 1000.0, 10_000.0] {
            p.vline(x_of(f), r.y, r.bottom(), th.device.grid_strong);
        }
        for f in [50.0, 200.0, 500.0, 2000.0, 5000.0] {
            p.vline(x_of(f), r.y, r.bottom(), th.device.grid);
        }
        for db in [-6.0, -3.0, 3.0, 6.0] {
            p.hline(r.x, r.right(), y_of(db), th.device.grid);
        }
        p.hline(r.x, r.right(), y_of(0.0), th.device.grid_strong);
        let (_, dim) = Self::ink(th);
        let small = TextStyle::new(th.fonts.tiny - 0.5, dim);
        for (f, label) in [(100.0, "100"), (1000.0, "1k"), (10_000.0, "10k")] {
            p.text(
                label,
                Rect::new(x_of(f) + 3.0, r.bottom() - 13.0, 30.0, 11.0),
                &small,
            );
        }
        if self.response.len() == POINTS {
            let pts: Vec<Point> = self
                .response
                .iter()
                .enumerate()
                .map(|(i, db)| Point::new(x_of(freq(i)), y_of(*db)))
                .collect();
            p.stroke_path(&Path::polyline(&pts), 2.0, self.accent);
        }
        p.text(
            "RESPONSE",
            Rect::new(r.x + 8.0, r.y + 5.0, r.w - 16.0, 12.0),
            &TextStyle::new(th.fonts.tiny, dim)
                .weight(FontWeight::Bold)
                .tracking(0.8),
        );
    }

    fn paint_loop(&self, p: &mut dyn Painter, r: Rect, cx: &Ctx<'_>) {
        let th = cx.theme;
        p.fill_rounded(r, 3.0, &Paint::Solid(th.device.display.darken(0.12)));
        let c = r.center();
        let (sx, sy) = (r.w * 0.42, r.h * 0.4);
        p.vline(c.x, r.y + 18.0, r.bottom() - 4.0, th.device.grid_strong);
        p.hline(r.x + 4.0, r.right() - 4.0, c.y, th.device.grid_strong);
        // Saturation.
        for s in [-1.0f32, 1.0] {
            p.hline(r.x + 4.0, r.right() - 4.0, c.y - s * sy, th.device.grid);
        }
        if !self.hysteresis.is_empty() {
            let mut pts: Vec<Point> = self
                .hysteresis
                .iter()
                .map(|(h, m)| Point::new(c.x + *h as f32 * sx, c.y - *m as f32 * sy))
                .collect();
            if let Some(first) = pts.first().copied() {
                pts.push(first);
            }
            p.stroke_path(&Path::polyline(&pts), 1.8, self.accent);
        }
        // How far the tape is driven now.
        let sat = cx.published(value::SATURATION).clamp(0.0, 1.05);
        if sat > 0.001 {
            let y = c.y - sat * sy;
            p.hline(r.x + 4.0, r.right() - 4.0, y, self.accent.with_alpha(0.55));
            p.circle(Point::new(r.right() - 8.0, y), 3.0, th.ui.text);
        }
        p.text(
            "TAPE  M–H",
            Rect::new(r.x + 8.0, r.y + 5.0, r.w - 16.0, 12.0),
            &TextStyle::new(th.fonts.tiny, Self::ink(th).1)
                .weight(FontWeight::Bold)
                .tracking(0.8),
        );
    }
}

impl Face for TapeFace {
    fn accent(&self) -> Color {
        self.accent
    }

    fn min_size(&self) -> Size {
        Size::new(980.0, 500.0)
    }

    fn panel(&self, size: Size) -> Panel {
        let (display, deck, _) = kit::frame(size, 206.0, 0.0);
        let s = kit::sections(
            deck,
            &[
                ("INPUT · OUTPUT", 4.3),
                ("TAPE", 4.4),
                ("ALIGNMENT", 3.6),
                ("TRANSPORT", 3.6),
            ],
        );
        let mut c = Vec::new();
        // Input and output.
        let r = kit::inside(&s[0]);
        let k = kit::row(Rect::new(r.x, r.y, r.w, KNOB.1), &[KNOB.0, KNOB.0, SMALL.0]);
        c.push(Ctl::knob(pid(id::INPUT), "INPUT", k[0]).bipolar());
        c.push(Ctl::knob(pid(id::OUTPUT), "OUTPUT", k[1]).bipolar());
        c.push(Ctl::small(
            pid(id::MIX),
            "MIX",
            kit::at(k[2], 0.0, 6.0, SMALL.0, SMALL.1),
        ));
        c.push(Ctl::toggle(
            pid(id::AUTO_OUTPUT),
            "Auto Output",
            Rect::new(
                r.x + (r.w - 150.0) / 2.0,
                r.bottom() - SWITCH_H - 4.0,
                150.0,
                SWITCH_H,
            ),
        ));
        // The tape: formulation, level, speed, equalisation.
        let r = kit::inside(&s[1]);
        let half = (r.w - 10.0) / 2.0;
        c.push(Ctl::choice(
            pid(id::TAPE),
            "TAPE",
            Rect::new(r.x, r.y, half, 38.0),
        ));
        c.push(Ctl::choice(
            pid(id::LEVEL),
            "OPERATING LEVEL",
            Rect::new(r.x + half + 10.0, r.y, half, 38.0),
        ));
        c.push(Ctl::segments(
            pid(id::SPEED),
            "SPEED",
            Rect::new(r.x, r.y + 50.0, r.w, 38.0),
        ));
        c.push(Ctl::segments(
            pid(id::EQ),
            "EQUALISATION",
            Rect::new(r.x, r.y + 100.0, r.w, 38.0),
        ));
        // The alignment: bias and azimuth, the heads.
        let r = kit::inside(&s[2]);
        let k = kit::row(Rect::new(r.x, r.y, r.w, KNOB.1), &[KNOB.0, SMALL.0]);
        c.push(Ctl::knob(pid(id::BIAS), "BIAS", k[0]).bipolar());
        c.push(
            Ctl::small(
                pid(id::AZIMUTH),
                "AZIMUTH",
                kit::at(k[1], 0.0, 6.0, SMALL.0, SMALL.1),
            )
            .bipolar(),
        );
        c.push(Ctl::segments(
            pid(id::FORMAT),
            "HEADS",
            Rect::new(r.x, r.bottom() - 42.0, r.w, 38.0),
        ));
        // The transport.
        let r = kit::inside(&s[3]);
        let k = kit::row(Rect::new(r.x, r.y, r.w * 0.6, KNOB.1), &[KNOB.0]);
        c.push(Ctl::knob(pid(id::FLUTTER), "WOW & FLUTTER", k[0]));
        c.push(Ctl::toggle(
            pid(id::HISS),
            "Hiss",
            Rect::new(
                r.x + r.w * 0.6 + 4.0,
                r.y + (KNOB.1 - SWITCH_H) / 2.0,
                r.w * 0.4 - 8.0,
                SWITCH_H,
            ),
        ));
        c.push(Ctl::segments(
            pid(id::OVERSAMPLING),
            "OVERSAMPLING",
            Rect::new(r.x, r.bottom() - 42.0, r.w, 38.0),
        ));
        Panel {
            display: Some(display),
            sections: s,
            controls: c,
            meters: Vec::new(),
        }
    }

    fn paint_display(&mut self, p: &mut dyn Painter, r: Rect, cx: &Ctx<'_>) {
        let th = cx.theme;
        let d = Self::settings(cx);
        self.follow(d);
        let (left, right, line, response, bh) = Self::split(r);
        // The meters: the tape level, 0 VU at the operating level; the lamp
        // when the tape nears saturation.
        let hot = cx.published(value::SATURATION) > 0.75;
        for (i, (rect, caption)) in [(left, "L"), (right, "R")].into_iter().enumerate() {
            let ms = cx.published(if i == 0 { value::VU_L } else { value::VU_R });
            let vu = 10.0 * ms.max(1e-12).log10();
            self.needles[i].follow(controls::vu_deflection(vu), cx.dt);
            // The caption under the meter in the display's own ink.
            let face = Rect::new(rect.x, rect.y, rect.w, rect.h - CAPTION_H);
            controls::vu_arc(p, face, self.needles[i].pos, hot, "", th);
            p.text(
                caption,
                Rect::new(rect.x, rect.bottom() - CAPTION_H, rect.w, CAPTION_H),
                &TextStyle::new(th.fonts.tiny, Self::ink(th).1).center(),
            );
        }
        // The machine's line.
        let depth = cx.value(pid(id::FLUTTER));
        let level = (cx.value(pid(id::LEVEL)).round().max(0.0) as usize).min(4);
        let text = format!(
            "{}  ·  {}  ·  {}  ·  {}  ·  W&F {:.2} % wtd",
            FORMULATIONS[d.tape].name,
            SPEED_NAMES[d.speed],
            tape::standard_name(d.speed, d.iec),
            LEVEL_NAMES[level],
            tape::flutter_figure(d.speed, depth) * 100.0,
        );
        p.text(
            &text,
            line,
            &TextStyle::new(th.fonts.tiny + 0.5, Self::ink(th).0).center(),
        );
        self.paint_response(p, response, cx);
        self.paint_loop(p, bh, cx);
    }

    fn format(&self, id: ParameterId, v: f64) -> Option<String> {
        tape::format(id, v)
    }

    fn tip(&self, pid: ParameterId) -> Option<&'static str> {
        Some(match pid.0 {
            id::INPUT => "How hard the tape is driven (the record level)",
            id::OUTPUT => "The level after the machine",
            id::AUTO_OUTPUT => {
                "Take the input trim back off at the output: driving the tape saturates it without making it louder"
            }
            id::MIX => "The machine against the dry signal (delayed to match)",
            id::SPEED => {
                "Tape speed: slower drives the treble harder into the tape and moves the head bump down"
            }
            id::TAPE => {
                "Standard (456-type), High Output (GP9-type, 3 dB more headroom), Vintage (thin, older, softer)"
            }
            id::LEVEL => {
                "The flux the operating level records: higher is hotter on the tape and further above the hiss"
            }
            id::EQ => "NAB or IEC record/reproduce curves (at 30 ips both are AES 17.5 µs)",
            id::BIAS => {
                "Bias current from the alignment: under is brighter, louder and grittier, over is darker and cleaner"
            }
            id::FORMAT => "¼″ or ½″ two-track heads: the bump, the hiss and the crosstalk",
            id::FLUTTER => "Wow and flutter, times a good machine's (×1)",
            id::HISS => "The tape's own noise at its measured level",
            id::AZIMUTH => {
                "The reproduce head's tilt (arc minutes): the tracks drift apart in the treble"
            }
            id::OVERSAMPLING => "The tape solved faster to keep its harmonics from folding back",
            _ => return None,
        })
    }
}
