//! The Tuner's face. The pitch is McLeod's (`faderframe_analysis::pitch`)
//! over the last 4096 input frames; a clarity below 0.8, or a signal under
//! −60 dBFS, shows no note. Estimates are taken as a median of five and
//! smoothed, so the needle moves calmly and a strobe drifts at the rate the
//! string is out.

use crate::kit::{self, Ctl, Ctx, Face, KNOB, Meter, MeterKind, Panel, SWITCH_H};
use crate::values::note_name;
use faderframe_analysis::pitch::{Detector, FRAMES, note_at};
use faderframe_core::ParameterId;
use faderframe_plugin_host::devices::tuner::{self as tun, id};
use faderframe_ui_canvas::{Color, Paint, Painter, Path, Point, Rect, Size, TextStyle};

fn pid(i: u32) -> ParameterId {
    ParameterId(i)
}

pub(crate) struct TunerFace {
    accent: Color,
    detector: Detector,
    left: Vec<f32>,
    right: Vec<f32>,
    seen: u64,
    recent: [f64; 5],
    count: usize,
    /// The note shown, its smoothed cents, and since when nothing was heard.
    note: Option<i32>,
    cents: f64,
    freq: f64,
    silent: f32,
    strobe: f64,
}

impl TunerFace {
    pub fn new(theme: &faderframe_ui_canvas::Theme) -> Self {
        Self {
            accent: theme.device.accent(faderframe_ui_canvas::Accent::Gate),
            detector: Detector::new(),
            left: vec![0.0; FRAMES],
            right: vec![0.0; FRAMES],
            seen: 0,
            recent: [0.0; 5],
            count: 0,
            note: None,
            cents: 0.0,
            freq: 0.0,
            silent: 10.0,
            strobe: 0.0,
        }
    }

    fn listen(&mut self, cx: &Ctx<'_>) {
        cx.tap.watch();
        let written = cx.tap.input.latest(&mut self.left, &mut self.right);
        self.silent += cx.dt;
        if written == self.seen {
            return;
        }
        self.seen = written;
        for (l, r) in self.left.iter_mut().zip(&self.right) {
            *l = 0.5 * (*l + r);
        }
        let rate = f64::from(cx.model.sample_rate().max(8_000));
        let Some(p) = self.detector.detect(&self.left, rate) else {
            return;
        };
        if p.clarity < 0.8 {
            return;
        }
        let reference = cx.value(pid(id::REFERENCE));
        self.recent[self.count % 5] = note_at(p.freq, reference);
        self.count += 1;
        let mut sorted = self.recent;
        let have = self.count.min(5);
        sorted[..have].sort_by(f64::total_cmp);
        let median = sorted[have / 2];
        let near = median.round();
        let cents = (median - near) * 100.0;
        if self.note != Some(near as i32) || self.silent > 0.5 {
            self.note = Some(near as i32);
            self.cents = cents;
        } else {
            self.cents += (cents - self.cents) * 0.35;
        }
        self.freq = reference * 2f64.powf((median - 69.0) / 12.0);
        self.silent = 0.0;
    }
}

impl Face for TunerFace {
    fn accent(&self) -> Color {
        self.accent
    }

    fn min_size(&self) -> Size {
        Size::new(560.0, 420.0)
    }

    fn panel(&self, size: Size) -> Panel {
        let (display, deck, meter) = kit::frame(size, 130.0, 30.0);
        let s = kit::sections(
            deck,
            &[("REFERENCE", 3.0), ("DISPLAY", 3.0), ("OUTPUT", 2.6)],
        );
        let mut c = Vec::new();
        let r = kit::inside(&s[0]);
        c.push(Ctl::knob(
            pid(id::REFERENCE),
            "A4",
            Rect::new(r.x + (r.w - KNOB.0) / 2.0, r.y, KNOB.0, KNOB.1),
        ));
        let r = kit::inside(&s[1]);
        c.push(Ctl::segments(
            pid(id::DISPLAY),
            "",
            Rect::new(r.x + 6.0, r.y + 30.0, r.w - 12.0, 26.0),
        ));
        let r = kit::inside(&s[2]);
        c.push(Ctl::toggle(
            pid(id::MUTE),
            "Mute",
            Rect::new(r.x + 8.0, r.y + 32.0, r.w - 16.0, SWITCH_H),
        ));
        let meters = vec![Meter {
            rect: meter,
            kind: MeterKind::Input,
            label: "IN",
        }];
        Panel {
            display: Some(display),
            sections: s,
            controls: c,
            meters,
        }
    }

    fn paint_display(&mut self, p: &mut dyn Painter, r: Rect, cx: &Ctx<'_>) {
        self.listen(cx);
        let th = cx.theme;
        p.fill_rounded(r, 4.0, &Paint::Solid(th.device.display.darken(0.12)));
        let heard = self.silent < 0.6 && self.note.is_some();
        let cents = if heard { self.cents } else { 0.0 };
        let in_tune = heard && cents.abs() < 3.0;
        let color = if !heard {
            th.ui.text_faint
        } else if in_tune {
            self.accent
        } else if cents.abs() < 15.0 {
            th.tools.level_warn
        } else {
            th.tools.level_over
        };
        // The note.
        let (name, octave) = match self.note {
            Some(n) => {
                let full = note_name(n);
                let split = full
                    .find(|c: char| c.is_ascii_digit() || c == '-')
                    .unwrap_or(full.len());
                (full[..split].to_string(), full[split..].to_string())
            }
            None => ("–".into(), String::new()),
        };
        let cx_mid = r.x + r.w / 2.0;
        p.text(
            &name,
            Rect::new(cx_mid - 80.0, r.y + 16.0, 160.0, 64.0),
            &TextStyle::new(
                th.fonts.large * 3.0,
                if heard { th.ui.text } else { th.ui.text_faint },
            )
            .bold()
            .center(),
        );
        // The octave beside the letter (and its sharp).
        let letter = th.fonts.large * 3.0;
        p.text(
            &octave,
            Rect::new(
                cx_mid + name.chars().count() as f32 * letter * 0.31 + 2.0,
                r.y + 50.0,
                40.0,
                24.0,
            ),
            &TextStyle::new(th.fonts.large, th.ui.text_dim).bold(),
        );
        let reading = if heard {
            format!(
                "{:.1} Hz   {}{:.0} ¢",
                self.freq,
                if cents >= 0.0 { "+" } else { "−" },
                cents.abs()
            )
        } else {
            format!("A4 = {:.1} Hz", cx.value(pid(id::REFERENCE)))
        };
        p.text(
            &reading,
            Rect::new(r.x, r.y + 84.0, r.w, 16.0),
            &TextStyle::new(th.fonts.tiny + 2.0, color).bold().center(),
        );
        let strobe = cx.value(pid(id::DISPLAY)) >= 0.5;
        let band = Rect::new(r.x + 30.0, r.y + 112.0, r.w - 60.0, r.h - 124.0);
        if strobe {
            // Bars drifting at the rate the note is off (still when in tune).
            self.strobe += cents * f64::from(cx.dt) * 0.08;
            p.fill_rounded(band, 4.0, &Paint::Solid(th.device.display));
            let rows = 3;
            let h = band.h / rows as f32;
            for row in 0..rows {
                let period = 40.0 / (row + 1) as f32;
                let off =
                    ((self.strobe * f64::from(row as u32 + 1)).rem_euclid(1.0) as f32) * period;
                let y = band.y + row as f32 * h + 3.0;
                let mut x = band.x - period + off;
                while x < band.right() {
                    let a = x.max(band.x);
                    let b = (x + period / 2.0).min(band.right());
                    if b > a {
                        p.fill(
                            Rect::new(a, y, b - a, h - 6.0),
                            color.with_alpha(if heard { 0.85 } else { 0.25 }),
                        );
                    }
                    x += period;
                }
            }
            p.vline(cx_mid, band.y - 4.0, band.bottom() + 4.0, th.ui.text_dim);
        } else {
            // A needle over ±50 cents.
            let centre = Point::new(cx_mid, band.bottom() + band.h * 0.55);
            let rad = (band.h * 1.35).min(band.w * 0.5);
            let angle = |c: f64| -std::f64::consts::FRAC_PI_2 + (c / 50.0).clamp(-1.0, 1.0) * 0.9;
            let at = |c: f64, k: f32| {
                let a = angle(c);
                Point::new(
                    centre.x + rad * k * a.cos() as f32,
                    centre.y + rad * k * a.sin() as f32,
                )
            };
            let arc: Vec<Point> = (-50..=50).map(|c| at(f64::from(c), 1.0)).collect();
            p.stroke_path(&Path::polyline(&arc), 2.0, th.device.grid_strong);
            let good: Vec<Point> = (-3..=3).map(|c| at(f64::from(c), 1.0)).collect();
            p.stroke_path(&Path::polyline(&good), 6.0, self.accent.with_alpha(0.7));
            for c in (-50..=50).step_by(10) {
                let k = if c == 0 { 0.86 } else { 0.92 };
                p.line(
                    at(f64::from(c), k),
                    at(f64::from(c), 1.0),
                    1.4,
                    th.ui.text_faint,
                );
                if c % 50 == 0 || c == 0 {
                    let pt = at(f64::from(c), 0.78);
                    p.text(
                        &format!("{c:+}").replace("+0", "0").replace('-', "−"),
                        Rect::new(pt.x - 20.0, pt.y - 7.0, 40.0, 14.0),
                        &TextStyle::new(th.fonts.tiny, th.ui.text_faint).center(),
                    );
                }
            }
            p.line(at(cents, 0.15), at(cents, 1.04), 3.0, color);
            p.circle(at(cents, 0.15), 5.0, color);
        }
        let _ = tun::DISPLAYS;
    }

    fn format(&self, id: ParameterId, v: f64) -> Option<String> {
        tun::format(id, v)
    }

    fn tip(&self, pid: ParameterId) -> Option<&'static str> {
        Some(match pid.0 {
            id::REFERENCE => "The frequency of A above middle C",
            id::MUTE => "Silence the output while tuning",
            id::DISPLAY => "A needle, or strobe bars that stand still when in tune",
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(f: f64, harmonics: &[f64], rate: f64) -> Vec<f32> {
        (0..FRAMES)
            .map(|n| {
                let t = n as f64 / rate;
                harmonics
                    .iter()
                    .enumerate()
                    .map(|(k, a)| a * (std::f64::consts::TAU * f * (k + 1) as f64 * t).sin())
                    .sum::<f64>() as f32
            })
            .collect()
    }

    #[test]
    fn it_finds_low_and_high_notes_within_a_cent() {
        let mut d = Detector::new();
        for (f, rate) in [
            (82.407, 48_000.0),
            (110.0, 44_100.0),
            (440.0, 48_000.0),
            (1_318.51, 96_000.0),
            (61.735, 48_000.0),
        ] {
            // A guitar-like spectrum: the fundamental weaker than the
            // second harmonic.
            let x = tone(f, &[0.4, 0.6, 0.3, 0.2, 0.1], rate);
            let p = d.detect(&x, rate).expect("a pitch");
            let cents = 1200.0 * (p.freq / f).log2();
            assert!(
                cents.abs() < 1.0,
                "{f} Hz read as {:.3} ({cents:.2} ¢)",
                p.freq
            );
            assert!(p.clarity > 0.9);
        }
        // Noise and silence have none worth showing.
        let mut state = 0x1234_5678u32;
        let noise: Vec<f32> = (0..FRAMES)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                (f64::from(state) / f64::from(u32::MAX) - 0.5) as f32
            })
            .collect();
        assert!(d.detect(&noise, 48_000.0).is_none_or(|p| p.clarity < 0.8));
        assert!(d.detect(&vec![0.0; FRAMES], 48_000.0).is_none());
        assert!((note_at(440.0, 440.0) - 69.0).abs() < 1e-9);
        assert!((note_at(432.0, 432.0) - 69.0).abs() < 1e-9);
    }
}
