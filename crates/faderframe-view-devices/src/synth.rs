//! The Synth's face: one cycle of each oscillator as set, the filter's
//! response (its reach under the envelope shaded, where the newest voice
//! has it now marked) and the envelopes with their levels now; below,
//! three rows of sections — the oscillators (with sync, ring, FM and
//! phase reset), mix (sub wave and key tracking, noise colour) and
//! voicing (stealing, glide mode, drift), the wheels (pitch-bend range,
//! what the mod wheel does); the filter (six types, filter FM), the
//! envelopes, LFO 1 and the output; LFO 2, the mod envelope, the
//! modulation matrix and the effects (chorus, delay, reverb).

use crate::kit::{self, Ctl, Ctx, Face, KNOB, Meter, MeterKind, Panel, SMALL, Scale, Section};
use crate::values::note_name;
use faderframe_core::ParameterId;
use faderframe_plugin_host::devices::synth::{self as syn, id, value};
use faderframe_ui_canvas::{Accent, Color, Paint, Painter, Path, Point, Rect, Size, TextStyle};

fn pid(i: u32) -> ParameterId {
    ParameterId(i)
}

pub(crate) struct SynthFace {
    accent: Color,
}

impl SynthFace {
    pub fn new(theme: &faderframe_ui_canvas::Theme) -> Self {
        Self {
            accent: theme.device.accent(Accent::Instrument),
        }
    }
}

/// Small knobs in a row across `r` (from the top).
fn smalls(c: &mut Vec<Ctl>, r: Rect, items: &[(u32, &'static str, Option<Scale>, bool)]) {
    let widths: Vec<f32> = items.iter().map(|_| SMALL.0).collect();
    for (k, &(i, label, scale, bipolar)) in kit::row(Rect::new(r.x, r.y, r.w, SMALL.1), &widths)
        .iter()
        .zip(items)
    {
        let mut ctl = Ctl::small(pid(i), label, *k);
        if let Some(s) = scale {
            ctl = ctl.scaled(s);
        }
        if bipolar {
            ctl = ctl.bipolar();
        }
        c.push(ctl);
    }
}

/// An ADSR's shape across `r` (attack, decay, a fixed sustain stretch,
/// release; times on a square-root scale so short ones show).
fn adsr(
    p: &mut dyn Painter,
    r: Rect,
    times: [f64; 3],
    sustain: f64,
    color: Color,
    now: Option<f32>,
) {
    let w = |ms: f64| (ms.max(0.5) / 10_000.0).sqrt() as f32;
    let (a, d, rel) = (w(times[0]), w(times[1]), w(times[2]));
    let hold = 0.25;
    let total = a + d + hold + rel;
    let x = |t: f32| r.x + r.w * t / total;
    let y = |v: f64| r.bottom() - r.h * v.clamp(0.0, 1.0) as f32;
    let mut pts = vec![Point::new(r.x, r.bottom()), Point::new(x(a), r.y)];
    for i in 1..=16 {
        let t = i as f32 / 16.0;
        let v = sustain + (1.0 - sustain) * f64::from((-6.9 * t).exp());
        pts.push(Point::new(x(a + d * t), y(v)));
    }
    pts.push(Point::new(x(a + d + hold), y(sustain)));
    for i in 1..=16 {
        let t = i as f32 / 16.0;
        pts.push(Point::new(
            x(a + d + hold + rel * t),
            y(sustain * f64::from((-6.9 * t).exp())),
        ));
    }
    let mut fill = Path::polyline(&pts);
    fill.line_to(Point::new(r.right(), r.bottom())).close();
    p.fill_path(&fill, color.with_alpha(0.12));
    p.stroke_path(&Path::polyline(&pts), 1.8, color);
    if let Some(v) = now.filter(|v| *v > 0.001) {
        let yy = y(f64::from(v));
        p.hline(r.x, r.right(), yy, color.with_alpha(0.35));
        p.circle(Point::new(r.x + 4.0, yy), 3.5, color);
    }
}

/// A section's row height and the gap between rows.
const ROW: f32 = 178.0;
const ROW_GAP: f32 = 8.0;

impl Face for SynthFace {
    fn accent(&self) -> Color {
        self.accent
    }

    fn min_size(&self) -> Size {
        Size::new(1600.0, 840.0)
    }

    fn panel(&self, size: Size) -> Panel {
        let deck_h = 3.0 * ROW + 2.0 * ROW_GAP;
        let (display, deck, meter) = kit::frame(size, deck_h, 30.0);
        let row = |n: f32| Rect::new(deck.x, deck.y + n * (ROW + ROW_GAP), deck.w, ROW);
        let mut s: Vec<Section> = kit::sections(
            row(0.0),
            &[
                ("OSC 1", 5.0),
                ("OSC 2", 5.0),
                ("MIX", 8.6),
                ("VOICE", 5.4),
                ("WHEELS", 4.8),
            ],
        );
        s.extend(kit::sections(
            row(1.0),
            &[
                ("FILTER", 8.8),
                ("FILTER ENVELOPE", 4.6),
                ("AMP ENVELOPE", 4.4),
                ("LFO 1", 5.8),
                ("OUT", 2.2),
            ],
        ));
        s.extend(kit::sections(
            row(2.0),
            &[
                ("LFO 2", 5.6),
                ("MOD ENVELOPE", 5.6),
                ("MATRIX", 8.0),
                ("CHORUS", 4.4),
                ("DELAY", 6.2),
                ("REVERB", 5.6),
            ],
        ));
        let mut c = Vec::new();
        let seg = |r: Rect| Rect::new(r.x + 6.0, r.y, r.w - 12.0, 24.0);
        // The segments with a switch at their right.
        let seg_and = |r: Rect| {
            let w = (r.w - 12.0) * 0.64;
            (
                Rect::new(r.x + 6.0, r.y, w, 24.0),
                Rect::new(r.x + 12.0 + w, r.y, r.w - 18.0 - w, 24.0),
            )
        };
        let below = |r: Rect| Rect::new(r.x, r.y + 34.0, r.w, r.h - 34.0);
        let ms = Some(Scale::Log);
        // Oscillators.
        let r = kit::inside(&s[0]);
        let (a, b) = seg_and(r);
        c.push(Ctl::segments(pid(id::OSC1_WAVE), "", a));
        c.push(Ctl::toggle(pid(id::PHASE_RESET), "Reset", b));
        smalls(
            &mut c,
            below(r),
            &[
                (id::OSC1_OCTAVE, "OCTAVE", None, true),
                (id::OSC1_PW, "PW", None, false),
                (id::OSC1_LEVEL, "LEVEL", None, false),
                (id::FM, "FM", None, false),
            ],
        );
        let r = kit::inside(&s[1]);
        let (a, b) = seg_and(r);
        c.push(Ctl::segments(pid(id::OSC2_WAVE), "", a));
        c.push(Ctl::toggle(pid(id::SYNC), "Sync", b));
        smalls(
            &mut c,
            below(r),
            &[
                (id::OSC2_OCTAVE, "OCTAVE", None, true),
                (id::OSC2_SEMI, "SEMI", None, true),
                (id::OSC2_LEVEL, "LEVEL", None, false),
                (id::RING, "RING", None, false),
            ],
        );
        // Mix: the sub's wave and the noise's colour over the levels.
        let r = kit::inside(&s[2]);
        let third = (r.w - 16.0) / 3.0;
        c.push(Ctl::choice(
            pid(id::SUB_WAVE),
            "SUB WAVE",
            Rect::new(r.x + 4.0, r.y - 6.0, third, 38.0),
        ));
        c.push(Ctl::choice(
            pid(id::NOISE_COLOUR),
            "NOISE",
            Rect::new(r.x + 12.0 + 2.0 * third, r.y - 6.0, third, 38.0),
        ));
        smalls(
            &mut c,
            Rect::new(r.x, r.y + 40.0, r.w, r.h - 40.0),
            &[
                (id::SUB, "SUB", None, false),
                (id::SUB_KEY, "SUB KEY", None, true),
                (id::NOISE, "NOISE", None, false),
                (id::DETUNE, "DETUNE", None, false),
                (id::UNISON, "UNISON", None, false),
                (id::UNISON_SPREAD, "SPREAD", None, false),
                (id::WIDTH, "WIDTH", None, false),
            ],
        );
        // Voice: the mode, glide and voices, then how glide and stealing
        // behave.
        let r = kit::inside(&s[3]);
        c.push(Ctl::segments(pid(id::VOICE_MODE), "", seg(r)));
        smalls(
            &mut c,
            Rect::new(r.x, r.y + 30.0, r.w, r.h - 30.0),
            &[
                (id::GLIDE, "GLIDE", Some(Scale::Skew(2.5)), false),
                (id::POLYPHONY, "VOICES", None, false),
                (id::VELOCITY_AMP, "VELOCITY", None, false),
                (id::DRIFT, "DRIFT", None, false),
            ],
        );
        let half = (r.w - 12.0) / 2.0;
        c.push(Ctl::choice(
            pid(id::GLIDE_MODE),
            "GLIDE",
            Rect::new(r.x + 4.0, r.y + 112.0, half, 38.0),
        ));
        c.push(Ctl::choice(
            pid(id::STEAL),
            "STEAL",
            Rect::new(r.x + 8.0 + half, r.y + 112.0, half, 38.0),
        ));
        // The pitch-bend range, then the mod wheel's slots: what each
        // drives and how much.
        let r = kit::inside(&s[4]);
        c.push(Ctl::slider(
            pid(id::BEND_RANGE),
            "PITCH BEND",
            Rect::new(r.x + 4.0, r.y, r.w - 8.0, 26.0),
        ));
        for k in 0..syn::WHEEL_SLOTS {
            let y = r.y + 34.0 + 34.0 * k as f32;
            let split = (r.w - 12.0) * 0.52;
            c.push(Ctl::choice(
                pid(id::wheel_target(k)),
                "",
                Rect::new(r.x + 4.0, y + 4.0, split, 22.0),
            ));
            c.push(
                Ctl::slider(
                    pid(id::wheel_amount(k)),
                    ["MOD WHEEL 1", "MOD WHEEL 2", "MOD WHEEL 3"][k],
                    Rect::new(r.x + 10.0 + split, y, r.w - 14.0 - split, 26.0),
                )
                .bipolar(),
            );
        }
        // Filter.
        let r = kit::inside(&s[5]);
        c.push(Ctl::segments(pid(id::FILTER_TYPE), "", seg(r)));
        let k = kit::row(
            Rect::new(r.x, r.y + 30.0, r.w, KNOB.1),
            &[KNOB.0, KNOB.0, SMALL.0, SMALL.0, SMALL.0, SMALL.0, SMALL.0],
        );
        c.push(Ctl::knob(pid(id::CUTOFF), "CUTOFF", k[0]).scaled(Scale::Log));
        c.push(Ctl::knob(pid(id::RESONANCE), "RESO", k[1]));
        let small_at = |r: Rect| kit::at(r, 0.0, 6.0, SMALL.0, SMALL.1);
        c.push(Ctl::small(pid(id::ENV_AMOUNT), "ENV", small_at(k[2])).bipolar());
        c.push(Ctl::small(pid(id::DRIVE), "DRIVE", small_at(k[3])));
        c.push(Ctl::small(pid(id::KEY_TRACK), "KEY", small_at(k[4])));
        c.push(Ctl::small(pid(id::VELOCITY_CUTOFF), "VEL", small_at(k[5])));
        c.push(Ctl::small(pid(id::FILTER_FM), "OSC 2 FM", small_at(k[6])));
        // Envelopes.
        let r = kit::inside(&s[6]);
        c.push(Ctl::segments(pid(id::FILTER_ENV), "", seg(r)));
        smalls(
            &mut c,
            below(r),
            &[
                (id::F_ATTACK, "A", ms, false),
                (id::F_DECAY, "D", ms, false),
                (id::F_SUSTAIN, "S", None, false),
                (id::F_RELEASE, "R", ms, false),
            ],
        );
        let r = kit::inside(&s[7]);
        smalls(
            &mut c,
            below(r),
            &[
                (id::ATTACK, "A", ms, false),
                (id::DECAY, "D", ms, false),
                (id::SUSTAIN, "S", None, false),
                (id::RELEASE, "R", ms, false),
            ],
        );
        // LFO 1.
        let lfo_head = |c: &mut Vec<Ctl>, r: Rect, shape: u32, sync: u32, division: u32| {
            let third = (r.w - 16.0) / 3.0;
            c.push(Ctl::choice(
                pid(shape),
                "SHAPE",
                Rect::new(r.x + 4.0, r.y - 6.0, third, 38.0),
            ));
            c.push(Ctl::toggle(
                pid(sync),
                "Sync",
                Rect::new(r.x + 8.0 + third, r.y + 8.0, third, 24.0),
            ));
            c.push(Ctl::choice(
                pid(division),
                "NOTE",
                Rect::new(r.x + 12.0 + 2.0 * third, r.y - 6.0, third, 38.0),
            ));
        };
        let r = kit::inside(&s[8]);
        lfo_head(&mut c, r, id::LFO_SHAPE, id::LFO_SYNC, id::LFO_DIVISION);
        smalls(
            &mut c,
            Rect::new(r.x, r.y + 40.0, r.w, r.h - 40.0),
            &[
                (id::LFO_RATE, "RATE", Some(Scale::Log), false),
                (id::LFO_PITCH, "PITCH", None, false),
                (id::LFO_CUTOFF, "CUTOFF", None, false),
                (id::LFO_AMP, "LEVEL", None, false),
                (id::LFO_PW, "PW", None, false),
            ],
        );
        let r = kit::inside(&s[9]);
        c.push(Ctl::knob(
            pid(id::VOLUME),
            "VOLUME",
            Rect::new(r.x + (r.w - KNOB.0) / 2.0, r.y + 30.0, KNOB.0, KNOB.1),
        ));
        // LFO 2: per voice, so it can restart with the note and fade in.
        let r = kit::inside(&s[10]);
        lfo_head(&mut c, r, id::LFO2_SHAPE, id::LFO2_SYNC, id::LFO2_DIVISION);
        let k = kit::row(
            Rect::new(r.x, r.y + 40.0, r.w, SMALL.1),
            &[SMALL.0, SMALL.0, 72.0],
        );
        c.push(Ctl::small(pid(id::LFO2_RATE), "RATE", k[0]).scaled(Scale::Log));
        c.push(Ctl::small(pid(id::LFO2_FADE), "FADE IN", k[1]).scaled(Scale::Skew(2.5)));
        c.push(Ctl::toggle(
            pid(id::LFO2_RETRIG),
            "Retrig",
            Rect::new(k[2].x, k[2].y + 16.0, k[2].w, 24.0),
        ));
        // The mod envelope.
        let r = kit::inside(&s[11]);
        smalls(
            &mut c,
            Rect::new(r.x, r.y + 20.0, r.w, r.h - 20.0),
            &[
                (id::M_ATTACK, "A", ms, false),
                (id::M_DECAY, "D", ms, false),
                (id::M_SUSTAIN, "S", None, false),
                (id::M_RELEASE, "R", ms, false),
            ],
        );
        // The matrix: a row per slot — source, target, amount.
        let r = kit::inside(&s[12]);
        for k in 0..syn::MOD_SLOTS {
            let y = r.y + 25.0 * k as f32;
            let w = r.w - 16.0;
            let (src_w, tgt_w) = (w * 0.28, w * 0.36);
            c.push(Ctl::choice(
                pid(id::mod_source(k)),
                "",
                Rect::new(r.x + 4.0, y, src_w, 22.0),
            ));
            c.push(Ctl::choice(
                pid(id::mod_target(k)),
                "",
                Rect::new(r.x + 8.0 + src_w, y, tgt_w, 22.0),
            ));
            c.push(
                Ctl::slider(
                    pid(id::mod_amount(k)),
                    "",
                    Rect::new(r.x + 12.0 + src_w + tgt_w, y - 2.0, w - src_w - tgt_w, 25.0),
                )
                .bipolar(),
            );
        }
        // Chorus.
        let r = kit::inside(&s[13]);
        c.push(Ctl::choice(
            pid(id::CHORUS_MODE),
            "",
            Rect::new(r.x + 6.0, r.y, r.w - 12.0, 22.0),
        ));
        smalls(
            &mut c,
            Rect::new(r.x, r.y + 30.0, r.w, r.h - 30.0),
            &[
                (id::CHORUS_RATE, "RATE", Some(Scale::Log), false),
                (id::CHORUS_DEPTH, "DEPTH", None, false),
                (id::CHORUS_MIX, "MIX", None, false),
            ],
        );
        // Delay.
        let r = kit::inside(&s[14]);
        let third = (r.w - 16.0) / 3.0;
        c.push(Ctl::toggle(
            pid(id::DELAY_SYNC),
            "Sync",
            Rect::new(r.x + 4.0, r.y, third, 24.0),
        ));
        c.push(Ctl::choice(
            pid(id::DELAY_DIVISION),
            "",
            Rect::new(r.x + 8.0 + third, r.y + 1.0, third, 22.0),
        ));
        c.push(Ctl::toggle(
            pid(id::DELAY_PINGPONG),
            "Ping-Pong",
            Rect::new(r.x + 12.0 + 2.0 * third, r.y, third, 24.0),
        ));
        smalls(
            &mut c,
            Rect::new(r.x, r.y + 30.0, r.w, r.h - 30.0),
            &[
                (id::DELAY_TIME, "TIME", ms, false),
                (id::DELAY_FEEDBACK, "FEEDBACK", None, false),
                (id::DELAY_TONE, "TONE", Some(Scale::Log), false),
                (id::DELAY_MIX, "MIX", None, false),
            ],
        );
        // Reverb.
        let r = kit::inside(&s[15]);
        smalls(
            &mut c,
            Rect::new(r.x, r.y + 20.0, r.w, r.h - 20.0),
            &[
                (id::REVERB_SIZE, "SIZE", None, false),
                (id::REVERB_DECAY, "DECAY", ms, false),
                (id::REVERB_TONE, "TONE", Some(Scale::Log), false),
                (id::REVERB_MIX, "MIX", None, false),
            ],
        );
        let meters = vec![Meter {
            rect: meter,
            kind: MeterKind::Output,
            label: "OUT",
        }];
        Panel {
            display: Some(display),
            sections: s,
            controls: c,
            meters,
        }
    }

    fn paint_display(&mut self, p: &mut dyn Painter, r: Rect, cx: &Ctx<'_>) {
        let th = cx.theme;
        let w = (r.w - 32.0) / 3.0;
        let boxes = [0, 1, 2]
            .map(|i| Rect::new(r.x + 8.0 + i as f32 * (w + 8.0), r.y + 8.0, w, r.h - 16.0));
        let title = TextStyle::new(th.fonts.tiny, th.ui.text_dim)
            .bold()
            .tracking(0.6);
        let label = TextStyle::new(th.fonts.tiny, th.ui.text_faint);
        for (b, t) in boxes.iter().zip(["OSCILLATORS", "FILTER", "ENVELOPES"]) {
            p.fill_rounded(*b, 4.0, &Paint::Solid(th.device.display.darken(0.12)));
            p.text(t, Rect::new(b.x + 8.0, b.y + 6.0, b.w, 12.0), &title);
        }
        let second = th.device.accent(Accent::Modulation);
        let third = th.ui.text_dim;
        // One cycle of each oscillator (osc 2 at its pitch relative to 1).
        let plot = Rect::new(
            boxes[0].x + 10.0,
            boxes[0].y + 26.0,
            boxes[0].w - 20.0,
            boxes[0].h - 40.0,
        );
        let mid = plot.y + plot.h / 2.0;
        p.hline(plot.x, plot.right(), mid, th.device.grid_strong);
        let ratio = 2f64.powf(
            cx.value(pid(id::OSC2_OCTAVE)).round() - cx.value(pid(id::OSC1_OCTAVE)).round()
                + cx.value(pid(id::OSC2_SEMI)).round() / 12.0,
        );
        let pw = cx.value(pid(id::OSC1_PW));
        let shape = |wave: f64, t: f64| -> f64 {
            let t = t.rem_euclid(1.0);
            match wave.round() as i64 {
                1 => {
                    if t < pw {
                        1.0
                    } else {
                        -1.0
                    }
                }
                2 => 1.0 - 4.0 * (t - 0.5).abs(),
                3 => (std::f64::consts::TAU * t).sin(),
                _ => 2.0 * t - 1.0,
            }
        };
        let synced = cx.on(pid(id::SYNC));
        for (o, cycles, color) in [(0, 2.0, self.accent), (1, 2.0 * ratio, second)] {
            let level = cx.value(pid(if o == 0 {
                id::OSC1_LEVEL
            } else {
                id::OSC2_LEVEL
            }));
            let wave = cx.value(pid(if o == 0 { id::OSC1_WAVE } else { id::OSC2_WAVE }));
            if level <= 0.001 {
                continue;
            }
            let pts: Vec<Point> = (0..=240)
                .map(|i| {
                    let t = f64::from(i) / 240.0;
                    // Synced: oscillator 2 restarts with every cycle of 1.
                    let phase = if o == 1 && synced {
                        (t * 2.0).rem_euclid(1.0) * ratio
                    } else {
                        t * cycles
                    };
                    let v = shape(wave, phase) * level;
                    Point::new(plot.x + plot.w * t as f32, mid - (v * 0.42) as f32 * plot.h)
                })
                .collect();
            p.stroke_path(
                &Path::polyline(&pts),
                if o == 0 { 2.0 } else { 1.4 },
                color.with_alpha(if o == 0 { 1.0 } else { 0.8 }),
            );
        }
        let unison = cx.value(pid(id::UNISON)).round();
        let voices = cx.published(value::VOICES);
        let mut notes: Vec<String> = Vec::new();
        if unison > 1.0 {
            notes.push(format!("{unison:.0}× unison"));
        }
        if synced {
            notes.push("sync".into());
        }
        if cx.value(pid(id::RING)) > 0.001 {
            notes.push("ring".into());
        }
        if cx.value(pid(id::FM)) > 0.001 {
            notes.push("FM".into());
        }
        if voices > 0.0 {
            notes.push(format!(
                "{voices:.0} playing · {}",
                note_name(cx.published(value::NOTE) as i32)
            ));
        }
        p.text(
            &notes.join("  "),
            Rect::new(
                boxes[0].x + 8.0,
                boxes[0].bottom() - 18.0,
                boxes[0].w - 16.0,
                12.0,
            ),
            &label.right(),
        );
        // The filter: its response, and its reach under the envelope.
        let fp = Rect::new(
            boxes[1].x + 10.0,
            boxes[1].y + 26.0,
            boxes[1].w - 20.0,
            boxes[1].h - 40.0,
        );
        let kind = cx.value(pid(id::FILTER_TYPE)).round() as i64;
        let resonance = cx.value(pid(id::RESONANCE));
        let k = 2.0 - 1.9 * resonance;
        let cutoff = cx.value(pid(id::CUTOFF));
        let amount = cx.value(pid(id::ENV_AMOUNT)) * 6.0;
        let response = |f: f64, fc: f64| -> f64 {
            let x = f / fc;
            let den = |kk: f64| ((1.0 - x * x).powi(2) + (kk * x).powi(2)).sqrt();
            let m = match kind {
                1 => 1.0 / den(std::f64::consts::SQRT_2) / den(k),
                2 => k * x / den(k),
                3 => x * x / den(k),
                4 => {
                    // Four poles with feedback: 1 / |(1 + jx)^4 + k|.
                    let kl = 4.0 * resonance;
                    let re = (1.0 - x * x).powi(2) - 4.0 * x * x + kl;
                    let im = 4.0 * x * (1.0 - x * x);
                    (1.0 + 0.5 * kl) / re.hypot(im)
                }
                5 => (1.0 - x * x).abs() / den(k),
                _ => 1.0 / den(k),
            };
            20.0 * m.max(1e-6).log10()
        };
        let to_y = |d: f64| fp.y + fp.h * ((18.0 - d) / 66.0).clamp(0.0, 1.0) as f32;
        p.hline(fp.x, fp.right(), to_y(0.0), th.device.grid_strong);
        let curve = |fc: f64| -> Vec<Point> {
            (0..=120)
                .map(|i| {
                    let x = fp.x + fp.w * i as f32 / 120.0;
                    Point::new(x, to_y(response(kit::Spectrum::freq_at(fp, x), fc)))
                })
                .collect()
        };
        let reach = (cutoff * 2f64.powf(amount)).clamp(20.0, 20_000.0);
        if (reach - cutoff).abs() > 1.0 {
            let (a, b) = (
                kit::Spectrum::x_of(fp, cutoff.min(reach)),
                kit::Spectrum::x_of(fp, cutoff.max(reach)),
            );
            p.fill(
                Rect::new(a, fp.y, b - a, fp.h),
                self.accent.with_alpha(0.08),
            );
            p.stroke_path(
                &Path::polyline(&curve(reach)),
                1.0,
                self.accent.with_alpha(0.35),
            );
        }
        let main = curve(cutoff);
        let mut fill = Path::polyline(&main);
        fill.line_to(Point::new(fp.right(), fp.bottom()))
            .line_to(Point::new(fp.x, fp.bottom()))
            .close();
        p.fill_path(&fill, self.accent.with_alpha(0.12));
        p.stroke_path(&Path::polyline(&main), 2.0, self.accent);
        if voices > 0.0 {
            let now = f64::from(cx.published(value::CUTOFF));
            if now > 20.0 {
                let x = kit::Spectrum::x_of(fp, now);
                p.vline(x, fp.y, fp.bottom(), th.ui.text.with_alpha(0.5));
                p.circle(Point::new(x, to_y(response(now, now))), 4.0, th.ui.text);
            }
        }
        p.text(
            &format!(
                "{} · {}",
                syn::FILTERS[kind.clamp(0, syn::FILTERS.len() as i64 - 1) as usize],
                faderframe_plugin_host::eq::format_hz(cutoff)
            ),
            Rect::new(boxes[1].x + 8.0, boxes[1].y + 6.0, boxes[1].w - 16.0, 12.0),
            &TextStyle::new(th.fonts.tiny, self.accent).bold().right(),
        );
        // The envelopes: the mod envelope faint behind, the filter's and
        // the amplifier's.
        let ep = Rect::new(
            boxes[2].x + 10.0,
            boxes[2].y + 26.0,
            boxes[2].w - 20.0,
            boxes[2].h - 40.0,
        );
        let mod_used = (0..syn::MOD_SLOTS).any(|k| {
            cx.value(pid(id::mod_source(k))).round() as usize == syn::source::MOD_ENV
                && cx.value(pid(id::mod_target(k))).round() as usize != syn::target::OFF
        });
        if mod_used {
            adsr(
                p,
                ep,
                [
                    cx.value(pid(id::M_ATTACK)),
                    cx.value(pid(id::M_DECAY)),
                    cx.value(pid(id::M_RELEASE)),
                ],
                cx.value(pid(id::M_SUSTAIN)),
                third,
                Some(cx.published(value::MOD_ENV)),
            );
        }
        let own = cx.value(pid(id::FILTER_ENV)) >= 0.5;
        if own {
            adsr(
                p,
                ep,
                [
                    cx.value(pid(id::F_ATTACK)),
                    cx.value(pid(id::F_DECAY)),
                    cx.value(pid(id::F_RELEASE)),
                ],
                cx.value(pid(id::F_SUSTAIN)),
                second,
                Some(cx.published(value::FILTER_ENV)),
            );
        }
        adsr(
            p,
            ep,
            [
                cx.value(pid(id::ATTACK)),
                cx.value(pid(id::DECAY)),
                cx.value(pid(id::RELEASE)),
            ],
            cx.value(pid(id::SUSTAIN)),
            self.accent,
            Some(cx.published(value::AMP_ENV)),
        );
        p.text(
            &format!(
                "amp{}{}",
                if own { " · filter" } else { " (and filter)" },
                if mod_used { " · mod" } else { "" }
            ),
            Rect::new(boxes[2].x + 8.0, boxes[2].y + 6.0, boxes[2].w - 16.0, 12.0),
            &TextStyle::new(th.fonts.tiny, th.ui.text_faint).right(),
        );
    }

    fn format(&self, id: ParameterId, v: f64) -> Option<String> {
        syn::format(id, v)
    }

    fn tip(&self, pid: ParameterId) -> Option<&'static str> {
        Some(match pid.0 {
            id::DETUNE => "How far the two oscillators are apart",
            id::UNISON => "Copies of the oscillators per voice",
            id::UNISON_SPREAD => "How far the unison copies are detuned",
            id::WIDTH => "How far the oscillators and copies spread across the sides",
            id::FILTER_ENV => "The filter follows the amp envelope, or its own",
            id::KEY_TRACK => "The cutoff follows the keys",
            id::VELOCITY_CUTOFF => "How much velocity opens the filter envelope",
            id::VELOCITY_AMP => "How much velocity sets the level",
            id::VOICE_MODE => "Poly, Mono (always retriggers), Legato (slides between held keys)",
            id::GLIDE => "Slide from the last note",
            id::GLIDE_MODE => "Glide on every note, or only onto a note played over a held key",
            id::STEAL => {
                "Which voice a new note takes when all are busy: the oldest, or the quietest"
            }
            id::DRIFT => "A little analogue drift: each voice a touch off, slowly wandering",
            id::BEND_RANGE => {
                "Semitones a full pitch-wheel throw bends (a controller setting it with RPN 0 wins on its channel)"
            }
            44 | 46 | 48 => "What the mod wheel drives in this slot",
            45 | 47 | 49 => {
                "How much (at full wheel): negative takes away; for LFO Amount and Volume, positive brings it in as the wheel goes up"
            }
            id::SYNC => {
                "Oscillator 2 restarts whenever oscillator 1 wraps: tune 2 above 1 and sweep it"
            }
            id::RING => "The product of the two oscillators, mixed in",
            id::FM => "Oscillator 2 bending oscillator 1's phase",
            id::PHASE_RESET => "The oscillators start every note from zero (off: free-running)",
            id::SUB_KEY => {
                "The sub's level across the keys: positive louder low down, ±6 dB an octave"
            }
            id::NOISE_COLOUR => "White, or pink (3 dB an octave less going up)",
            id::FILTER_FM => "Oscillator 2 moving the cutoff at audio rate",
            id::LFO2_RETRIG => "LFO 2 restarts with every note",
            id::LFO2_FADE => "LFO 2 comes in over this time after the note",
            x if (60..60 + 3 * syn::MOD_SLOTS as u32).contains(&x) => match (x - 60) % 3 {
                0 => "What drives this slot",
                1 => "What it moves",
                _ => "How much: negative turns it the other way",
            },
            id::CHORUS_MODE => "Chorus: one voice a side; Ensemble: three with a fast shimmer",
            id::DELAY_PINGPONG => "Repeats alternate between the sides",
            id::DELAY_TONE => "The repeats lose highs above this",
            id::REVERB_DECAY => "How long the tail takes to fade by 60 dB",
            _ => return None,
        })
    }
}
