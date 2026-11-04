//! What the Wavetable's, the FM's and the Granular's faces share: two
//! pages (the instrument's own sound, and its modulation) switched by tabs
//! in the display's corner, three rows of sections each, the last row —
//! filter, its envelope, the amplifier envelope, voicing and the output —
//! on both; the modulation page's LFOs, mod envelope, wheels, the
//! eight-slot matrix and the effects, laid out alike for all three
//! (`plugin_host::devices::poly`'s shared block).

use crate::kit::{self, Ctl, KNOB, Meter, MeterKind, Panel, SMALL, Scale, Section};
use crate::synth::smalls;
use faderframe_core::ParameterId;
use faderframe_plugin_host::devices::poly::{self as shared, MOD_SLOTS, WHEEL_SLOTS, id};
use faderframe_ui_canvas::{Color, Paint, Painter, Point, Rect, Size, TextStyle, Theme};

pub(crate) fn pid(i: u32) -> ParameterId {
    ParameterId(i)
}

/// A row's height and the gap between rows.
pub(crate) const ROW: f32 = 178.0;
pub(crate) const ROW_GAP: f32 = 8.0;
/// The deck: three rows.
pub(crate) const DECK_H: f32 = 3.0 * ROW + 2.0 * ROW_GAP;
/// The faces' smallest size (the editor windows open at it).
pub(crate) const MIN_SIZE: (f32, f32) = (1600.0, 840.0);

/// Which page the deck shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Page {
    /// The instrument's own sound (oscillators, operators, grains).
    Sound,
    Modulation,
}

/// The display, the deck's three rows and the meter.
pub(crate) struct Frame {
    pub display: Rect,
    pub rows: [Rect; 3],
    pub meter: Rect,
}

pub(crate) fn frame(size: Size) -> Frame {
    let (display, deck, meter) = kit::frame(size, DECK_H, 30.0);
    let row = |n: f32| Rect::new(deck.x, deck.y + n * (ROW + ROW_GAP), deck.w, ROW);
    Frame {
        display,
        rows: [row(0.0), row(1.0), row(2.0)],
        meter,
    }
}

/// The page tabs at the display's top right: the sound page's (named
/// `first`) and the modulation page's.
pub(crate) fn tabs(display: Rect) -> [(Rect, Page); 2] {
    let w = 118.0;
    let y = display.y + 6.0;
    let x = display.right() - 8.0 - 2.0 * w - 4.0;
    [
        (Rect::new(x, y, w, 22.0), Page::Sound),
        (Rect::new(x + w + 4.0, y, w, 22.0), Page::Modulation),
    ]
}

/// The page a click at `pos` in the display chooses.
pub(crate) fn tab_at(display: Rect, pos: Point) -> Option<Page> {
    tabs(display)
        .into_iter()
        .find(|(r, _)| r.contains(pos))
        .map(|(_, p)| p)
}

pub(crate) fn paint_tabs(
    p: &mut dyn Painter,
    display: Rect,
    first: &str,
    page: Page,
    accent: Color,
    th: &Theme,
) {
    for (r, which) in tabs(display) {
        let on = which == page;
        p.fill_rounded(
            r,
            4.0,
            &Paint::Solid(if on {
                accent.with_alpha(0.35)
            } else {
                th.ui.surface_alt
            }),
        );
        p.stroke_rounded(r, 4.0, 1.0, if on { accent } else { th.ui.border });
        let label = match which {
            Page::Sound => first,
            Page::Modulation => "MODULATION",
        };
        p.text(
            label,
            r,
            &TextStyle::new(th.fonts.tiny + 0.5, th.ui.text)
                .bold()
                .tracking(0.5)
                .center(),
        );
    }
}

/// The display's area left of the tabs' row (what the face draws in).
pub(crate) fn content(display: Rect) -> Rect {
    Rect::new(
        display.x + 8.0,
        display.y + 34.0,
        display.w - 16.0,
        display.h - 42.0,
    )
}

/// A box in the display (a darker panel with a title).
pub(crate) fn display_box(p: &mut dyn Painter, r: Rect, title: &str, th: &Theme) {
    p.fill_rounded(r, 4.0, &Paint::Solid(th.device.display.darken(0.12)));
    p.text(
        title,
        Rect::new(r.x + 8.0, r.y + 6.0, r.w - 16.0, 12.0),
        &TextStyle::new(th.fonts.tiny, th.ui.text_dim)
            .bold()
            .tracking(0.6),
    );
}

/// The segments with a switch at their right.
fn seg(r: Rect) -> Rect {
    Rect::new(r.x + 6.0, r.y, r.w - 12.0, 24.0)
}

/// An ADSR's four small knobs (ids in order).
fn envelope(c: &mut Vec<Ctl>, r: Rect, ids: [u32; 4]) {
    let ms = Some(Scale::Log);
    smalls(
        c,
        Rect::new(r.x, r.y + 20.0, r.w, r.h - 20.0),
        &[
            (ids[0], "A", ms, false),
            (ids[1], "D", ms, false),
            (ids[2], "S", None, false),
            (ids[3], "R", ms, false),
        ],
    );
}

/// The row both pages end with: filter, filter envelope, amplifier
/// envelope, voicing and the output.
pub(crate) fn shape_row(r: Rect, s: &mut Vec<Section>, c: &mut Vec<Ctl>) {
    let at = s.len();
    s.extend(kit::sections(
        r,
        &[
            ("FILTER", 8.8),
            ("FILTER ENVELOPE", 4.8),
            ("AMP ENVELOPE", 4.8),
            ("VOICE", 5.4),
            ("OUT", 2.2),
        ],
    ));
    let s = &s[at..];
    // Filter: the type (or off), cutoff and resonance, then how it moves.
    let r = kit::inside(&s[0]);
    c.push(Ctl::segments(pid(id::FILTER_TYPE), "", seg(r)));
    let k = kit::row(
        Rect::new(r.x, r.y + 30.0, r.w, KNOB.1),
        &[KNOB.0, KNOB.0, SMALL.0, SMALL.0, SMALL.0, SMALL.0],
    );
    c.push(Ctl::knob(pid(id::CUTOFF), "CUTOFF", k[0]).scaled(Scale::Log));
    c.push(Ctl::knob(pid(id::RESONANCE), "RESO", k[1]));
    let small_at = |r: Rect| kit::at(r, 0.0, 6.0, SMALL.0, SMALL.1);
    c.push(Ctl::small(pid(id::ENV_AMOUNT), "ENV", small_at(k[2])).bipolar());
    c.push(Ctl::small(pid(id::DRIVE), "DRIVE", small_at(k[3])));
    c.push(Ctl::small(pid(id::KEY_TRACK), "KEY", small_at(k[4])));
    c.push(Ctl::small(pid(id::VELOCITY_CUTOFF), "VEL", small_at(k[5])));
    envelope(
        c,
        kit::inside(&s[1]),
        [id::F_ATTACK, id::F_DECAY, id::F_SUSTAIN, id::F_RELEASE],
    );
    envelope(
        c,
        kit::inside(&s[2]),
        [id::ATTACK, id::DECAY, id::SUSTAIN, id::RELEASE],
    );
    // Voice: the mode, then glide, voices, velocity and the bend range,
    // then how glide and stealing behave.
    let r = kit::inside(&s[3]);
    c.push(Ctl::segments(pid(id::VOICE_MODE), "", seg(r)));
    smalls(
        c,
        Rect::new(r.x, r.y + 30.0, r.w, SMALL.1),
        &[
            (id::GLIDE, "GLIDE", Some(Scale::Skew(2.5)), false),
            (id::POLYPHONY, "VOICES", None, false),
            (id::VELOCITY_AMP, "VELOCITY", None, false),
            (id::BEND_RANGE, "BEND", None, false),
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
    let r = kit::inside(&s[4]);
    c.push(Ctl::knob(
        pid(id::VOLUME),
        "VOLUME",
        Rect::new(r.x + (r.w - KNOB.0) / 2.0, r.y + 30.0, KNOB.0, KNOB.1),
    ));
}

/// An LFO's shape, sync and note division across the top of `r`.
fn lfo_head(c: &mut Vec<Ctl>, r: Rect, shape: u32, sync: u32, division: u32) {
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
}

/// LFO 2 (per voice) in a section.
pub(crate) fn lfo2(c: &mut Vec<Ctl>, s: &Section) {
    let r = kit::inside(s);
    lfo_head(c, r, id::LFO2_SHAPE, id::LFO2_SYNC, id::LFO2_DIVISION);
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
}

/// The mod envelope in a section.
pub(crate) fn mod_envelope(c: &mut Vec<Ctl>, s: &Section) {
    envelope(
        c,
        kit::inside(s),
        [id::M_ATTACK, id::M_DECAY, id::M_SUSTAIN, id::M_RELEASE],
    );
}

/// The modulation page's first two rows: LFO 1, LFO 2, the mod envelope
/// and the wheels; the matrix and the effects.
pub(crate) fn modulation_rows(r0: Rect, r1: Rect, s: &mut Vec<Section>, c: &mut Vec<Ctl>) {
    let at = s.len();
    s.extend(kit::sections(
        r0,
        &[
            ("LFO 1", 4.8),
            ("LFO 2", 5.6),
            ("MOD ENVELOPE", 4.8),
            ("WHEELS", 4.8),
        ],
    ));
    s.extend(kit::sections(
        r1,
        &[
            ("MATRIX", 12.0),
            ("CHORUS", 4.4),
            ("DELAY", 6.2),
            ("REVERB", 5.6),
        ],
    ));
    let s = &s[at..];
    let r = kit::inside(&s[0]);
    lfo_head(c, r, id::LFO_SHAPE, id::LFO_SYNC, id::LFO_DIVISION);
    smalls(
        c,
        Rect::new(r.x, r.y + 40.0, r.w, SMALL.1),
        &[(id::LFO_RATE, "RATE", Some(Scale::Log), false)],
    );
    lfo2(c, &s[1]);
    mod_envelope(c, &s[2]);
    // The pitch-bend range is the voice's; the wheel's three slots here.
    let r = kit::inside(&s[3]);
    for k in 0..WHEEL_SLOTS {
        let y = r.y + 6.0 + 40.0 * k as f32;
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
    // The matrix: two columns of four slots — source, target, amount.
    let r = kit::inside(&s[4]);
    let col = (r.w - 8.0) / 2.0;
    for k in 0..MOD_SLOTS {
        let (cx, row) = ((k / 4) as f32, (k % 4) as f32);
        let x = r.x + cx * (col + 8.0);
        let y = r.y + 4.0 + 34.0 * row;
        let w = col - 8.0;
        let (src_w, tgt_w) = (w * 0.3, w * 0.38);
        c.push(Ctl::choice(
            pid(id::mod_source(k)),
            "",
            Rect::new(x, y + 2.0, src_w, 22.0),
        ));
        c.push(Ctl::choice(
            pid(id::mod_target(k)),
            "",
            Rect::new(x + 4.0 + src_w, y + 2.0, tgt_w, 22.0),
        ));
        c.push(
            Ctl::slider(
                pid(id::mod_amount(k)),
                "",
                Rect::new(x + 8.0 + src_w + tgt_w, y, w - 8.0 - src_w - tgt_w, 26.0),
            )
            .bipolar(),
        );
    }
    // Chorus.
    let r = kit::inside(&s[5]);
    c.push(Ctl::choice(
        pid(id::CHORUS_MODE),
        "",
        Rect::new(r.x + 6.0, r.y, r.w - 12.0, 22.0),
    ));
    smalls(
        c,
        Rect::new(r.x, r.y + 30.0, r.w, SMALL.1),
        &[
            (id::CHORUS_RATE, "RATE", Some(Scale::Log), false),
            (id::CHORUS_DEPTH, "DEPTH", None, false),
            (id::CHORUS_MIX, "MIX", None, false),
        ],
    );
    // Delay.
    let r = kit::inside(&s[6]);
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
    let ms = Some(Scale::Log);
    smalls(
        c,
        Rect::new(r.x, r.y + 30.0, r.w, SMALL.1),
        &[
            (id::DELAY_TIME, "TIME", ms, false),
            (id::DELAY_FEEDBACK, "FEEDBACK", None, false),
            (id::DELAY_TONE, "TONE", Some(Scale::Log), false),
            (id::DELAY_MIX, "MIX", None, false),
        ],
    );
    // Reverb.
    let r = kit::inside(&s[7]);
    smalls(
        c,
        Rect::new(r.x, r.y + 20.0, r.w, SMALL.1),
        &[
            (id::REVERB_SIZE, "SIZE", None, false),
            (id::REVERB_DECAY, "DECAY", ms, false),
            (id::REVERB_TONE, "TONE", Some(Scale::Log), false),
            (id::REVERB_MIX, "MIX", None, false),
        ],
    );
}

/// The panel of a page: its sections and controls, the output meter.
pub(crate) fn panel(f: &Frame, sections: Vec<Section>, controls: Vec<Ctl>) -> Panel {
    Panel {
        display: Some(f.display),
        sections,
        controls,
        meters: vec![Meter {
            rect: f.meter,
            kind: MeterKind::Output,
            label: "OUT",
        }],
    }
}

/// The modulation page, whatever the instrument.
pub(crate) fn modulation_page(f: &Frame) -> Panel {
    let (mut s, mut c) = (Vec::new(), Vec::new());
    modulation_rows(f.rows[0], f.rows[1], &mut s, &mut c);
    shape_row(f.rows[2], &mut s, &mut c);
    panel(f, s, c)
}

/// What a shared control does.
pub(crate) fn tip(pid: ParameterId) -> Option<&'static str> {
    shared::tip(pid)
}

/// A cycle drawn across `r` from `samples` (one period), as a polyline of
/// at most `points`.
pub(crate) fn cycle(
    p: &mut dyn Painter,
    r: Rect,
    samples: &[f32],
    points: usize,
    width: f32,
    color: Color,
) {
    if samples.is_empty() {
        return;
    }
    let mid = r.y + r.h / 2.0;
    let pts: Vec<Point> = (0..=points)
        .map(|i| {
            let k = (i * samples.len() / points).min(samples.len() - 1);
            Point::new(
                r.x + r.w * i as f32 / points as f32,
                mid - samples[k].clamp(-1.0, 1.0) * r.h * 0.45,
            )
        })
        .collect();
    p.stroke_path(&faderframe_ui_canvas::Path::polyline(&pts), width, color);
}
