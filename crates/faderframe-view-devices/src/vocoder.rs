//! The Vocoder's face: the bank across the spectrum — each band's level in
//! the voice now as a bar at its centre, where the carrier's bands sit
//! after Formant as ticks under them — what the carrier is playing (the
//! chord, the note followed, the notes held, or which input), the
//! unvoiced lamp lighting on consonants, the controls and meters.

use crate::kit::{self, Ctl, Ctx, Face, KNOB, Meter, MeterKind, Panel, SMALL, SWITCH_H, Scale};
use faderframe_core::ParameterId;
use faderframe_plugin_host::devices::note_name;
use faderframe_plugin_host::devices::vocoder::{self as voc, id, value};
use faderframe_ui_canvas::{Accent, Color, Paint, Painter, Point, Rect, Size, TextStyle};

fn pid(i: u32) -> ParameterId {
    ParameterId(i)
}

/// The bars' floor (dB).
const FLOOR: f32 = -72.0;

pub(crate) struct VocoderFace {
    accent: Color,
    spectrum: kit::Spectrum,
    /// Each band's bar, falling.
    bars: [f32; voc::MAX_BANDS],
}

impl VocoderFace {
    pub fn new(theme: &faderframe_ui_canvas::Theme) -> Self {
        Self {
            accent: theme.device.accent(Accent::Modulation),
            spectrum: kit::Spectrum::new(),
            bars: [FLOOR; voc::MAX_BANDS],
        }
    }

    /// The heading and the bank under it.
    pub(crate) fn layout(r: Rect) -> (Rect, Rect) {
        let head = Rect::new(r.x + 8.0, r.y + 6.0, r.w - 16.0, 20.0);
        let bank = Rect::new(r.x + 8.0, r.y + 32.0, r.w - 16.0, r.h - 40.0);
        (head, bank)
    }

    /// What the carrier is playing, in words.
    fn carrier_text(cx: &Ctx<'_>) -> String {
        let carrier = cx.value(pid(id::CARRIER)).round() as usize;
        let modulator = if cx.on(pid(id::MODULATOR)) {
            "the sidechain"
        } else {
            "the input"
        };
        let playing = match carrier {
            1 => "Noise".to_string(),
            2 => "The sidechain".to_string(),
            3 => "The input".to_string(),
            _ => {
                let pitch = f64::from(cx.published(value::PITCH));
                let voices = cx.published(value::VOICES) as usize;
                match cx.value(pid(id::PITCH)).round() as usize {
                    1 if voices == 0 => "The synth: play notes (MIDI)".to_string(),
                    1 => format!(
                        "The synth: {voices} notes, {}",
                        note_name(pitch.round() as i32)
                    ),
                    2 => format!(
                        "The synth following the voice: {}",
                        note_name(pitch.round() as i32)
                    ),
                    _ => {
                        let chord = voc::format(pid(id::CHORD), cx.value(pid(id::CHORD)))
                            .unwrap_or_default();
                        let root = note_name(cx.value(pid(id::ROOT)).round() as i32);
                        format!("The synth: {chord} on {root}")
                    }
                }
            }
        };
        format!("{playing} carries {modulator}")
    }
}

impl Face for VocoderFace {
    fn accent(&self) -> Color {
        self.accent
    }

    fn min_size(&self) -> Size {
        Size::new(1_100.0, 520.0)
    }

    fn panel(&self, size: Size) -> Panel {
        let (display, deck, meter) = kit::frame(size, 224.0, 48.0);
        let s = kit::sections(
            deck,
            &[
                ("CARRIER", 5.0),
                ("SYNTH", 4.2),
                ("BANK", 6.0),
                ("VOICE", 4.6),
                ("OUTPUT", 3.2),
            ],
        );
        let mut c = Vec::new();
        // The carrier and the modulator, the synth's pitch.
        let r = kit::inside(&s[0]);
        let half = (r.w - 6.0) / 2.0;
        c.push(Ctl::choice(
            pid(id::CARRIER),
            "CARRIER",
            Rect::new(r.x, r.y, half, 38.0),
        ));
        c.push(Ctl::choice(
            pid(id::MODULATOR),
            "VOICE IN",
            Rect::new(r.x + half + 6.0, r.y, half, 38.0),
        ));
        c.push(Ctl::segments(
            pid(id::PITCH),
            "PITCH",
            Rect::new(r.x, r.y + 44.0, r.w, 38.0),
        ));
        let k = kit::row(
            Rect::new(r.x, r.y + 88.0, r.w, SMALL.1),
            &[SMALL.0, 76.0, SMALL.0],
        );
        c.push(Ctl::small(pid(id::ROOT), "ROOT", k[0]));
        c.push(Ctl::choice(
            pid(id::CHORD),
            "CHORD",
            Rect::new(k[1].x, k[1].y + 18.0, k[1].w, 38.0),
        ));
        c.push(Ctl::small(pid(id::TRANSPOSE), "TRANSPOSE", k[2]).bipolar());
        // The synth's sound.
        let r = kit::inside(&s[1]);
        let k = kit::row(Rect::new(r.x, r.y, r.w, SMALL.1), &[SMALL.0, SMALL.0]);
        c.push(Ctl::small(pid(id::WAVE), "WAVE", k[0]));
        c.push(Ctl::small(pid(id::DETUNE), "DETUNE", k[1]));
        let k = kit::row(
            Rect::new(r.x, r.y + SMALL.1 + 8.0, r.w, SMALL.1),
            &[SMALL.0, SMALL.0],
        );
        c.push(Ctl::small(pid(id::GLIDE), "GLIDE", k[0]).scaled(Scale::Skew(2.5)));
        c.push(Ctl::small(pid(id::NOISE), "BREATH", k[1]));
        // The bank.
        let r = kit::inside(&s[2]);
        c.push(Ctl::segments(
            pid(id::BANDS),
            "BANDS",
            Rect::new(r.x, r.y, r.w, 38.0),
        ));
        c.push(Ctl::segments(
            pid(id::SPACING),
            "SPACING",
            Rect::new(r.x, r.y + 44.0, r.w, 38.0),
        ));
        let k = kit::row(
            Rect::new(r.x, r.y + 88.0, r.w, SMALL.1),
            &[SMALL.0, SMALL.0, SMALL.0, SMALL.0],
        );
        c.push(Ctl::small(pid(id::LOW), "LOW", k[0]).scaled(Scale::Log));
        c.push(Ctl::small(pid(id::HIGH), "HIGH", k[1]).scaled(Scale::Log));
        c.push(Ctl::small(pid(id::BANDWIDTH), "WIDTH", k[2]).scaled(Scale::Log));
        c.push(Ctl::small(pid(id::FORMANT), "FORMANT", k[3]).bipolar());
        // How the voice is followed.
        let r = kit::inside(&s[3]);
        let k = kit::row(Rect::new(r.x, r.y, r.w, KNOB.1), &[KNOB.0, KNOB.0]);
        c.push(Ctl::knob(pid(id::ATTACK), "ATTACK", k[0]).scaled(Scale::Log));
        c.push(Ctl::knob(pid(id::RELEASE), "RELEASE", k[1]).scaled(Scale::Log));
        let k = kit::row(
            Rect::new(r.x, r.y + KNOB.1 + 6.0, r.w, SMALL.1),
            &[SMALL.0, SMALL.0, 60.0],
        );
        c.push(Ctl::small(pid(id::UNVOICED), "UNVOICED", k[0]));
        c.push(Ctl::small(pid(id::SENSITIVITY), "SENS", k[1]));
        c.push(Ctl::toggle(
            pid(id::ENHANCE),
            "Enhance",
            Rect::new(k[2].x, k[2].y + 28.0, k[2].w, SWITCH_H),
        ));
        // Out.
        let r = kit::inside(&s[4]);
        c.push(Ctl::knob(
            pid(id::MIX),
            "MIX",
            Rect::new(r.x + (r.w - KNOB.0) / 2.0, r.y, KNOB.0, KNOB.1),
        ));
        let k = kit::row(
            Rect::new(r.x, r.y + KNOB.1 + 6.0, r.w, SMALL.1),
            &[SMALL.0, SMALL.0],
        );
        c.push(Ctl::small(pid(id::WIDTH), "STEREO", k[0]));
        c.push(Ctl::small(pid(id::OUTPUT), "OUTPUT", k[1]).bipolar());
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
        self.spectrum.update(cx);
        let th = cx.theme;
        let (head, bank) = Self::layout(r);
        // What plays.
        p.text(
            &Self::carrier_text(cx),
            Rect::new(head.x, head.y, head.w - 120.0, head.h),
            &TextStyle::new(th.fonts.small, th.ui.text).bold(),
        );
        let unvoiced = cx.published(value::UNVOICED).clamp(0.0, 1.0);
        let lamp = Point::new(head.right() - 104.0, head.center().y);
        p.circle(lamp, 6.0, th.ui.text_faint.with_alpha(0.4));
        p.circle(lamp, 6.0, self.accent.with_alpha(unvoiced));
        p.text(
            "UNVOICED",
            Rect::new(lamp.x + 10.0, head.y + 3.0, 90.0, 14.0),
            &TextStyle::new(th.fonts.tiny, th.ui.text_dim)
                .bold()
                .tracking(0.6),
        );
        // The spectrum behind, the bank's bands over it.
        self.spectrum.paint(p, bank, cx, -96.0, true);
        let n = voc::band_count(cx.value(pid(id::BANDS)));
        let bands = voc::bands(
            n,
            cx.value(pid(id::LOW)),
            cx.value(pid(id::HIGH)),
            cx.value(pid(id::SPACING)).round().max(0.0) as usize,
        );
        let shift = 2f64.powf(cx.value(pid(id::FORMANT)) / 12.0);
        let y_of = |d: f32| bank.bottom() - bank.h * ((d - FLOOR) / -FLOOR).clamp(0.0, 1.0);
        for (k, (f, w)) in bands.iter().enumerate() {
            let level = cx.published(value::BAND + k);
            let bar = &mut self.bars[k];
            *bar = if level > *bar {
                level
            } else {
                (*bar - 40.0 * cx.dt).max(FLOOR)
            };
            let a = kit::Spectrum::x_of(bank, f - 0.4 * w);
            let z = kit::Spectrum::x_of(bank, f + 0.4 * w);
            let x = kit::Spectrum::x_of(bank, *f);
            let bw = (z - a).clamp(2.0, 18.0);
            let top = y_of(*bar);
            p.fill_rounded(
                Rect::new(x - bw / 2.0, top, bw, bank.bottom() - top),
                2.0,
                &Paint::Solid(self.accent.with_alpha(0.75)),
            );
            // Where the carrier's band is (Formant).
            let sx = kit::Spectrum::x_of(bank, f * shift);
            p.vline(
                sx,
                bank.bottom() - 8.0,
                bank.bottom(),
                th.ui.text.with_alpha(0.6),
            );
        }
        if (shift - 1.0).abs() > 1e-6 {
            p.text(
                &format!(
                    "FORMANT {}",
                    voc::format(pid(id::FORMANT), cx.value(pid(id::FORMANT))).unwrap_or_default()
                ),
                Rect::new(bank.right() - 130.0, bank.y + 4.0, 124.0, 12.0),
                &TextStyle::new(th.fonts.tiny, self.accent).bold(),
            );
        }
    }

    fn format(&self, id: ParameterId, v: f64) -> Option<String> {
        voc::format(id, v)
    }

    fn tip(&self, pid: ParameterId) -> Option<&'static str> {
        Some(match pid.0 {
            id::CARRIER => "What sings: the built-in synth, noise, the sidechain or the input",
            id::MODULATOR => "Whose voice shapes it: the input or the sidechain",
            id::PITCH => {
                "Chord: a held chord on the root. MIDI: the notes played (an instrument track). Follow: the voice's own pitch"
            }
            id::FORMANT => "Moves the voice's formants up or down the carrier",
            id::UNVOICED => "Consonants' highs passed straight through",
            id::SENSITIVITY => "How readily a sound counts as a consonant",
            id::ENHANCE => "Every band of the carrier speaks alike (its tilt flattened)",
            id::WIDTH => "Bands spread left and right",
            id::NOISE => "Breath: noise in the synth",
            _ => return None,
        })
    }
}
