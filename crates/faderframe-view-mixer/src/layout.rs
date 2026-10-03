//! Channel strip layout (pure geometry).

use faderframe_ui_canvas::{Rect, Theme};

pub const INSERT_SLOTS: usize = 4;
pub const SEND_SLOTS: usize = 2;

/// Rectangles of every control on one strip. Optional sections are `None`
/// when the strip is too short to show them.
#[derive(Clone, Debug, PartialEq)]
pub struct StripLayout {
    pub strip: Rect,
    pub color_bar: Rect,
    pub header: Rect,
    pub input: Option<InputRow>,
    pub inserts: Option<[Rect; INSERT_SLOTS]>,
    pub inserts_label: Option<Rect>,
    pub sends: Option<[SendSlot; SEND_SLOTS]>,
    pub sends_label: Option<Rect>,
    pub pan_knob: Rect,
    pub pan_readout: Rect,
    pub mute: Rect,
    pub solo: Rect,
    pub record: Rect,
    pub level_readout: Rect,
    pub fader: Rect,
    pub meter: Rect,
    pub output: Rect,
    pub scribble: Rect,
    /// Lines between sections (y coordinates).
    pub dividers: Vec<f32>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InputRow {
    pub input: Rect,
    pub phase: Rect,
    pub monitor: Rect,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SendSlot {
    pub knob: Rect,
    pub label: Rect,
}

const MARGIN: f32 = 5.0;
const HEADER_H: f32 = 15.0;
const INPUT_H: f32 = 17.0;
const LABEL_H: f32 = 11.0;
const SLOT_H: f32 = 15.0;
const SLOT_GAP: f32 = 3.0;
const SEND_H: f32 = 44.0;
const PAN_H: f32 = 52.0;
const BUTTONS_H: f32 = 18.0;
const READOUT_H: f32 = 15.0;
const OUTPUT_H: f32 = 16.0;
const SCRIBBLE_H: f32 = 24.0;
const SECTION_GAP: f32 = 7.0;
const MIN_FADER_H: f32 = 110.0;

impl StripLayout {
    pub fn new(strip: Rect, theme: &Theme, has_input: bool, has_sends: bool) -> Self {
        let _ = theme;
        let mut r = strip.inset_xy(MARGIN, 0.0);
        let color_bar = Rect::new(strip.x, strip.y, strip.w, 3.0);
        r.take_top(6.0);
        let header = r.take_top(HEADER_H);

        // Bottom-up fixed parts.
        let mut bottom = r;
        bottom.take_bottom(MARGIN);
        let scribble = bottom.take_bottom(SCRIBBLE_H);
        bottom.take_bottom(4.0);
        let output = bottom.take_bottom(OUTPUT_H);
        r = bottom;

        // Decide which optional sections fit, in priority order.
        let essential = PAN_H + BUTTONS_H + READOUT_H + MIN_FADER_H + 3.0 * SECTION_GAP;
        let mut budget = r.h - essential;
        let mut take = |h: f32| {
            if budget >= h {
                budget -= h;
                true
            } else {
                false
            }
        };
        let show_input = has_input && take(INPUT_H + SECTION_GAP);
        let inserts_h = LABEL_H + INSERT_SLOTS as f32 * (SLOT_H + SLOT_GAP) + SECTION_GAP;
        let show_inserts = take(inserts_h);
        let show_sends = has_sends && take(LABEL_H + SEND_H + SECTION_GAP);

        let mut dividers = Vec::new();
        let input = show_input.then(|| {
            let row = r.take_top(INPUT_H);
            let third = (row.w - 4.0) / 3.0;
            let input = Rect::new(row.x, row.y, third * 1.4, row.h);
            let phase = Rect::new(input.right() + 2.0, row.y, third * 0.8, row.h);
            let monitor = Rect::new(
                phase.right() + 2.0,
                row.y,
                row.right() - phase.right() - 2.0,
                row.h,
            );
            dividers.push(r.y + SECTION_GAP * 0.5);
            r.take_top(SECTION_GAP);
            InputRow {
                input,
                phase,
                monitor,
            }
        });

        let (inserts_label, inserts) = if show_inserts {
            let label = r.take_top(LABEL_H);
            let mut slots = [Rect::default(); INSERT_SLOTS];
            for s in &mut slots {
                *s = r.take_top(SLOT_H);
                r.take_top(SLOT_GAP);
            }
            dividers.push(r.y + SECTION_GAP * 0.5);
            r.take_top(SECTION_GAP);
            (Some(label), Some(slots))
        } else {
            (None, None)
        };

        let (sends_label, sends) = if show_sends {
            let label = r.take_top(LABEL_H);
            let row = r.take_top(SEND_H);
            let half = row.w / 2.0;
            let mut slots = [SendSlot {
                knob: Rect::default(),
                label: Rect::default(),
            }; SEND_SLOTS];
            for (i, s) in slots.iter_mut().enumerate() {
                let col = Rect::new(row.x + half * i as f32, row.y, half, row.h);
                let size = (col.w - 4.0).min(32.0);
                s.knob = Rect::new(col.center().x - size / 2.0, col.y, size, size);
                s.label = Rect::new(col.x, col.y + size + 1.0, col.w, row.h - size - 1.0);
            }
            dividers.push(r.y + SECTION_GAP * 0.5);
            r.take_top(SECTION_GAP);
            (Some(label), Some(slots))
        } else {
            (None, None)
        };

        let pan_area = r.take_top(PAN_H);
        let knob = (pan_area.h - 14.0).min(pan_area.w - 10.0).min(40.0);
        let pan_knob = Rect::new(pan_area.center().x - knob / 2.0, pan_area.y, knob, knob);
        let pan_readout = Rect::new(
            pan_area.x + 8.0,
            pan_area.bottom() - 13.0,
            pan_area.w - 16.0,
            13.0,
        );
        dividers.push(r.y + SECTION_GAP * 0.5);
        r.take_top(SECTION_GAP);

        let buttons = r.take_top(BUTTONS_H);
        let bw = (buttons.w - 6.0) / 3.0;
        let mute = Rect::new(buttons.x, buttons.y, bw, buttons.h);
        let solo = Rect::new(mute.right() + 3.0, buttons.y, bw, buttons.h);
        let record = Rect::new(solo.right() + 3.0, buttons.y, bw, buttons.h);
        r.take_top(SECTION_GAP);

        let level_readout = r.take_top(READOUT_H).inset_xy(6.0, 0.0);
        r.take_top(4.0);
        let fader_zone = r;
        let meter_w = (fader_zone.w * 0.26).clamp(10.0, 18.0);
        let (fader, meter) = fader_zone.split_right(meter_w);
        Self {
            strip,
            color_bar,
            header,
            input,
            inserts,
            inserts_label,
            sends,
            sends_label,
            pan_knob,
            pan_readout,
            mute,
            solo,
            record,
            level_readout,
            fader,
            meter: meter.inset_xy(0.0, 2.0),
            output,
            scribble,
            dividers,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tall_strips_show_everything_short_strips_collapse() {
        let theme = Theme::default();
        let tall = StripLayout::new(Rect::new(0.0, 0.0, 92.0, 760.0), &theme, true, true);
        assert!(tall.input.is_some() && tall.inserts.is_some() && tall.sends.is_some());
        assert!(tall.fader.h >= MIN_FADER_H);
        let short = StripLayout::new(Rect::new(0.0, 0.0, 92.0, 330.0), &theme, true, true);
        assert!(short.inserts.is_none() && short.sends.is_none());
        assert!(short.fader.h >= MIN_FADER_H - 1.0);
        // Nothing overlaps the scribble strip.
        assert!(tall.output.bottom() <= tall.scribble.y);
        assert!(tall.fader.bottom() <= tall.output.y);
    }
}
