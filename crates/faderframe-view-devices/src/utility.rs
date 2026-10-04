//! The Utility's face: a vectorscope of what comes out (mid up, side
//! across), its correlation and balance, the controls and meters.

use crate::kit::{self, Ctl, Ctx, Face, KNOB, Meter, MeterKind, Panel, SMALL, SWITCH_H, Scale};
use faderframe_core::ParameterId;
use faderframe_plugin_host::devices::utility::{self as util, id};
use faderframe_ui_canvas::{Color, Paint, Painter, Path, Point, Rect, Size, TextStyle};

fn pid(i: u32) -> ParameterId {
    ParameterId(i)
}

/// Frames the scope shows.
const SCOPE: usize = 2048;

pub(crate) struct UtilityFace {
    accent: Color,
    left: Vec<f32>,
    right: Vec<f32>,
    /// The scope's zoom, following the level.
    zoom: f32,
    correlation: f32,
    balance: f32,
    seen: u64,
}

impl UtilityFace {
    pub fn new(theme: &faderframe_ui_canvas::Theme) -> Self {
        Self {
            accent: theme.device.wave,
            left: vec![0.0; SCOPE],
            right: vec![0.0; SCOPE],
            zoom: 1.0,
            correlation: 1.0,
            balance: 0.0,
            seen: 0,
        }
    }

    fn split(r: Rect) -> (Rect, Rect) {
        let side = r.h - 16.0;
        let scope = Rect::new(r.x + 8.0, r.y + 8.0, side, side);
        let bars = Rect::new(
            scope.right() + 18.0,
            r.y + 14.0,
            r.right() - scope.right() - 30.0,
            side - 12.0,
        );
        (scope, bars)
    }

    /// A bar from −1 to +1 with the value marked.
    fn bar(
        p: &mut dyn Painter,
        r: Rect,
        v: f32,
        caption: &str,
        ends: (&str, &str),
        color: Color,
        cx: &Ctx<'_>,
    ) {
        let th = cx.theme;
        p.text(
            caption,
            Rect::new(r.x, r.y, r.w, 12.0),
            &TextStyle::new(th.fonts.tiny, th.ui.text_dim)
                .bold()
                .tracking(0.6),
        );
        let track = Rect::new(r.x, r.y + 16.0, r.w, 10.0);
        p.fill_rounded(track, 3.0, &Paint::Solid(th.device.display.darken(0.2)));
        let mid = track.x + track.w / 2.0;
        let x = mid + v.clamp(-1.0, 1.0) * track.w / 2.0;
        let (a, b) = if x < mid { (x, mid) } else { (mid, x) };
        p.fill_rounded(
            Rect::new(a, track.y, (b - a).max(2.0), track.h),
            2.0,
            &Paint::Solid(color.with_alpha(0.8)),
        );
        p.vline(
            mid,
            track.y - 2.0,
            track.bottom() + 2.0,
            th.device.grid_strong,
        );
        let small = TextStyle::new(th.fonts.tiny, th.ui.text_faint);
        p.text(
            ends.0,
            Rect::new(track.x, track.bottom() + 2.0, 40.0, 12.0),
            &small,
        );
        p.text(
            ends.1,
            Rect::new(track.right() - 40.0, track.bottom() + 2.0, 40.0, 12.0),
            &small.right(),
        );
        p.text(
            &format!("{v:+.2}").replace('-', "−"),
            Rect::new(track.x, track.bottom() + 2.0, track.w, 12.0),
            &TextStyle::new(th.fonts.tiny + 0.5, th.ui.text)
                .bold()
                .center(),
        );
    }
}

impl Face for UtilityFace {
    fn accent(&self) -> Color {
        self.accent
    }

    fn min_size(&self) -> Size {
        Size::new(760.0, 470.0)
    }

    fn panel(&self, size: Size) -> Panel {
        let (display, deck, meter) = kit::frame(size, 180.0, 48.0);
        let s = kit::sections(
            deck,
            &[
                ("GAIN", 3.2),
                ("STEREO", 5.0),
                ("POLARITY", 2.4),
                ("MONO BASS", 2.6),
            ],
        );
        let mut c = Vec::new();
        let r = kit::inside(&s[0]);
        c.push(
            Ctl::knob(
                pid(id::GAIN),
                "GAIN",
                Rect::new(r.x + (r.w - KNOB.0) / 2.0, r.y, KNOB.0, KNOB.1),
            )
            .bipolar(),
        );
        let b = kit::row(
            Rect::new(r.x, r.bottom() - SWITCH_H, r.w, SWITCH_H),
            &[r.w / 2.0 - 8.0, r.w / 2.0 - 8.0],
        );
        c.push(Ctl::toggle(pid(id::MUTE), "Mute", b[0]));
        c.push(Ctl::toggle(pid(id::DC), "DC", b[1]));
        let r = kit::inside(&s[1]);
        let k = kit::row(Rect::new(r.x, r.y, r.w * 0.62, KNOB.1), &[KNOB.0, KNOB.0]);
        c.push(Ctl::knob(pid(id::PAN), "BALANCE", k[0]).bipolar());
        c.push(Ctl::knob(pid(id::WIDTH), "WIDTH", k[1]));
        c.push(Ctl::choice(
            pid(id::CHANNELS),
            "CHANNELS",
            Rect::new(r.x + r.w * 0.62 + 4.0, r.y + 10.0, r.w * 0.38 - 10.0, 38.0),
        ));
        let r = kit::inside(&s[2]);
        c.push(Ctl::toggle(
            pid(id::INVERT_L),
            "Ø Left",
            Rect::new(r.x + 6.0, r.y + 8.0, r.w - 12.0, SWITCH_H),
        ));
        c.push(Ctl::toggle(
            pid(id::INVERT_R),
            "Ø Right",
            Rect::new(r.x + 6.0, r.y + 40.0, r.w - 12.0, SWITCH_H),
        ));
        let r = kit::inside(&s[3]);
        c.push(Ctl::toggle(
            pid(id::MONO_BASS),
            "Mono Bass",
            Rect::new(r.x + 6.0, r.y + 2.0, r.w - 12.0, SWITCH_H),
        ));
        c.push(
            Ctl::small(
                pid(id::BASS_FREQ),
                "BELOW",
                Rect::new(r.x + (r.w - SMALL.0) / 2.0, r.y + 32.0, SMALL.0, SMALL.1),
            )
            .scaled(Scale::Log),
        );
        let w = (meter.w - 4.0) / 2.0;
        let meters = vec![
            Meter {
                rect: Rect::new(meter.x, meter.y, w, meter.h),
                kind: MeterKind::Input,
                label: "IN",
            },
            Meter {
                rect: Rect::new(meter.x + w + 4.0, meter.y, w, meter.h),
                kind: MeterKind::Output,
                label: "OUT",
            },
        ];
        Panel {
            display: Some(display),
            sections: s,
            controls: c,
            meters,
        }
    }

    fn paint_display(&mut self, p: &mut dyn Painter, r: Rect, cx: &Ctx<'_>) {
        cx.tap.watch();
        let th = cx.theme;
        let written = cx.tap.output.latest(&mut self.left, &mut self.right);
        let fresh = written != self.seen;
        self.seen = written;
        let (scope, bars) = Self::split(r);
        p.fill_rounded(scope, 4.0, &Paint::Solid(th.device.display.darken(0.12)));
        let c = Point::new(scope.x + scope.w / 2.0, scope.y + scope.h / 2.0);
        let rad = scope.w / 2.0 - 10.0;
        // Guides: the L and R axes (diagonals), mid (up) and side.
        p.stroke_path(&Path::circle(c, rad), 1.0, th.device.grid);
        p.stroke_path(&Path::circle(c, rad * 0.5), 1.0, th.device.grid);
        p.vline(
            c.x,
            scope.y + 8.0,
            scope.bottom() - 8.0,
            th.device.grid_strong,
        );
        p.hline(scope.x + 8.0, scope.right() - 8.0, c.y, th.device.grid);
        let d = rad * std::f32::consts::FRAC_1_SQRT_2;
        p.line(
            Point::new(c.x - d, c.y - d),
            Point::new(c.x + d, c.y + d),
            1.0,
            th.device.grid,
        );
        p.line(
            Point::new(c.x + d, c.y - d),
            Point::new(c.x - d, c.y + d),
            1.0,
            th.device.grid,
        );
        let label = TextStyle::new(th.fonts.tiny, th.ui.text_faint);
        p.text(
            "L",
            Rect::new(c.x - d - 14.0, c.y - d - 14.0, 12.0, 12.0),
            &label,
        );
        p.text(
            "R",
            Rect::new(c.x + d + 4.0, c.y - d - 14.0, 12.0, 12.0),
            &label,
        );
        p.text("M", Rect::new(c.x + 4.0, scope.y + 6.0, 12.0, 12.0), &label);
        p.text(
            "S",
            Rect::new(scope.right() - 18.0, c.y + 2.0, 12.0, 12.0),
            &label,
        );
        // The level, correlation and balance of what is shown.
        let (mut ll, mut rr, mut lr, mut peak) = (0.0f64, 0.0f64, 0.0f64, 0.0f32);
        for (a, b) in self.left.iter().zip(&self.right) {
            ll += f64::from(*a) * f64::from(*a);
            rr += f64::from(*b) * f64::from(*b);
            lr += f64::from(*a) * f64::from(*b);
            // The point's distance from the centre (full scale mono: √2).
            peak = peak.max(a.hypot(*b));
        }
        let k = (1.0 - 4.0 * cx.dt).clamp(0.0, 1.0);
        if fresh && ll + rr > 1e-12 {
            let corr = (lr / (ll * rr).sqrt().max(1e-20)) as f32;
            let bal = ((rr - ll) / (rr + ll)) as f32;
            self.correlation = self.correlation * k + corr * (1.0 - k);
            self.balance = self.balance * k + bal * (1.0 - k);
        }
        let want = 0.9 / peak.max(0.03);
        self.zoom = if want < self.zoom {
            want
        } else {
            self.zoom + (want - self.zoom) * (1.0 - k)
        };
        if peak > 1e-5 {
            let s = rad * self.zoom * std::f32::consts::FRAC_1_SQRT_2;
            // Short paths (one long self-crossing path is too much for the
            // renderer), without points on top of each other.
            let mut pts: Vec<Point> = Vec::with_capacity(SCOPE);
            for (a, b) in self.left.iter().zip(&self.right) {
                let q = Point::new(c.x + (b - a) * s, c.y - (a + b) * s);
                if pts
                    .last()
                    .is_none_or(|l| (l.x - q.x).abs() + (l.y - q.y).abs() > 0.4)
                {
                    pts.push(q);
                }
            }
            for chunk in pts.chunks(64) {
                if chunk.len() > 1 {
                    p.stroke_path(&Path::polyline(chunk), 1.0, self.accent.with_alpha(0.55));
                }
            }
        }
        p.text(
            &format!("circle {:.1} dB", 20.0 * (0.9 / self.zoom).log10()).replace('-', "−"),
            Rect::new(scope.x + 8.0, scope.bottom() - 18.0, 80.0, 12.0),
            &label,
        );
        // Correlation and balance.
        let corr_color = if self.correlation < 0.0 {
            th.tools.level_over
        } else {
            self.accent
        };
        Self::bar(
            p,
            Rect::new(bars.x, bars.y, bars.w, 44.0),
            self.correlation,
            "CORRELATION",
            ("−1", "+1"),
            corr_color,
            cx,
        );
        Self::bar(
            p,
            Rect::new(bars.x, bars.y + 60.0, bars.w, 44.0),
            self.balance,
            "BALANCE",
            ("L", "R"),
            self.accent,
            cx,
        );
        let w = cx.value(pid(id::WIDTH));
        let mode =
            util::CHANNEL_MODES[cx.value(pid(id::CHANNELS)).round().clamp(0.0, 5.0) as usize];
        kit::readout(
            p,
            Rect::new(bars.x, bars.y + 124.0, (bars.w - 10.0) / 2.0, 46.0),
            "WIDTH",
            &format!("{:.0} %", w * 100.0),
            self.accent,
            th,
        );
        kit::readout(
            p,
            Rect::new(
                bars.x + (bars.w + 10.0) / 2.0,
                bars.y + 124.0,
                (bars.w - 10.0) / 2.0,
                46.0,
            ),
            "CHANNELS",
            mode,
            self.accent,
            th,
        );
    }

    fn format(&self, id: ParameterId, v: f64) -> Option<String> {
        util::format(id, v)
    }

    fn tip(&self, pid: ParameterId) -> Option<&'static str> {
        Some(match pid.0 {
            id::PAN => "Turn one side down (centre: both untouched)",
            id::WIDTH => "0 % mono, 100 % as it is, 200 % twice the side",
            id::CHANNELS => "Stereo, left or right on both, swapped, the mid or the side",
            id::MONO_BASS => "Fold the side to mono below the frequency",
            id::DC => "Take DC off (5 Hz high pass)",
            _ => return None,
        })
    }
}
