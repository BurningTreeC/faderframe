//! Channel strip layout (pure geometry).

use faderframe_ui_canvas::{Rect, Theme};

/// Height of one insert slot including its gap (the grip moves in these
/// steps).
pub const INSERT_SLOT_STEP: f32 = SLOT_H + SLOT_GAP;
/// Send knobs per row.
pub const SENDS_PER_ROW: usize = 2;
/// Most send rows a strip shows at once (more sends are paged in banks).
pub const MAX_SEND_ROWS: usize = 4;

/// Rectangles of every control on one strip. Optional sections are `None`
/// when the strip is too short to show them.
#[derive(Clone, Debug, PartialEq)]
pub struct StripLayout {
    pub strip: Rect,
    pub color_bar: Rect,
    pub header: Rect,
    pub input: Option<InputRow>,
    pub preamp: Option<Rect>,
    /// Insert slots shown (as many as wanted and fit).
    pub inserts: Option<Vec<Rect>>,
    pub inserts_label: Option<Rect>,
    /// The rule under the inserts: drag it to show more or fewer slots.
    pub inserts_grip: Option<Rect>,
    /// Visible send slots (rows × [`SENDS_PER_ROW`]).
    pub sends: Option<Vec<SendSlot>>,
    pub sends_label: Option<Rect>,
    /// Bank arrows in the SENDS label (previous / next page of sends).
    pub send_prev: Option<Rect>,
    pub send_next: Option<Rect>,
    pub pan_knob: Rect,
    pub pan_readout: Rect,
    pub mute: Rect,
    pub solo: Rect,
    pub record: Rect,
    pub level_readout: Rect,
    pub fader: Rect,
    pub meter: Rect,
    pub output: Rect,
    /// Group and VCA tags (when the project has groups or VCAs).
    pub tags: Option<Rect>,
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
const TAGS_H: f32 = 14.0;
const SECTION_GAP: f32 = 7.0;
const MIN_FADER_H: f32 = 110.0;

impl StripLayout {
    /// `send_rows` / `insert_slots`: how many rows of send knobs and insert
    /// slots are wanted (fewer are shown when the strip is short).
    pub fn new(
        strip: Rect,
        theme: &Theme,
        has_input: bool,
        has_sends: bool,
        send_rows: usize,
        insert_slots: usize,
        tags: bool,
    ) -> Self {
        Self::with_preamp(
            strip,
            theme,
            has_input,
            has_sends,
            send_rows,
            insert_slots,
            tags,
            0.0,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn with_preamp(
        strip: Rect,
        theme: &Theme,
        has_input: bool,
        has_sends: bool,
        send_rows: usize,
        insert_slots: usize,
        tags: bool,
        preamp_height: f32,
    ) -> Self {
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
        let tags = tags.then(|| {
            let t = bottom.take_bottom(TAGS_H);
            bottom.take_bottom(3.0);
            t
        });
        let output = bottom.take_bottom(OUTPUT_H);
        r = bottom;

        // Decide which optional sections fit, in priority order.
        let fixed = PAN_H + BUTTONS_H + READOUT_H + 3.0 * SECTION_GAP;
        // Keep an installed input stage usable in the docked mixer. Shorten
        // the fader before hiding its Gain and Master controls.
        let min_fader = if preamp_height >= 80.0 {
            (r.h - fixed - preamp_height - SECTION_GAP).clamp(36.0, MIN_FADER_H)
        } else {
            MIN_FADER_H
        };
        let essential = fixed + min_fader;
        let mut budget = r.h - essential;
        let mut take = |h: f32| {
            if budget >= h {
                budget -= h;
                true
            } else {
                false
            }
        };
        let show_preamp = preamp_height > 0.0 && take(preamp_height + SECTION_GAP);
        let show_input = has_input && take(INPUT_H + SECTION_GAP);
        // 0: no inserts (VCAs).
        let insert_slots = (1..=insert_slots)
            .rev()
            .find(|&n| take(LABEL_H + n as f32 * INSERT_SLOT_STEP + SECTION_GAP))
            .unwrap_or(0);
        let show_inserts = insert_slots > 0;
        let send_rows = if has_sends {
            (1..=send_rows.clamp(1, MAX_SEND_ROWS))
                .rev()
                .find(|&n| take(LABEL_H + n as f32 * SEND_H + SECTION_GAP))
                .unwrap_or(0)
        } else {
            0
        };
        let show_sends = send_rows > 0;

        let mut dividers = Vec::new();
        let preamp = show_preamp.then(|| {
            let area = r.take_top(preamp_height);
            r.take_top(SECTION_GAP);
            area
        });
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

        let (inserts_label, inserts, inserts_grip) = if show_inserts {
            let label = r.take_top(LABEL_H);
            let slots = (0..insert_slots)
                .map(|_| {
                    let s = r.take_top(SLOT_H);
                    r.take_top(SLOT_GAP);
                    s
                })
                .collect();
            let y = r.y + SECTION_GAP * 0.5;
            dividers.push(y);
            r.take_top(SECTION_GAP);
            let grip = Rect::new(strip.x, y - 3.5, strip.w, 7.0);
            (Some(label), Some(slots), Some(grip))
        } else {
            (None, None, None)
        };

        let (sends_label, sends, send_prev, send_next) = if show_sends {
            let label = r.take_top(LABEL_H);
            let mut slots = Vec::with_capacity(send_rows * SENDS_PER_ROW);
            for _ in 0..send_rows {
                let row = r.take_top(SEND_H);
                let col_w = row.w / SENDS_PER_ROW as f32;
                for i in 0..SENDS_PER_ROW {
                    let col = Rect::new(row.x + col_w * i as f32, row.y, col_w, row.h);
                    let size = (col.w - 4.0).min(32.0);
                    slots.push(SendSlot {
                        knob: Rect::new(col.center().x - size / 2.0, col.y, size, size),
                        label: Rect::new(col.x, col.y + size + 1.0, col.w, row.h - size - 1.0),
                    });
                }
            }
            let prev = Rect::new(label.x, label.y, 12.0, label.h);
            let next = Rect::new(label.right() - 12.0, label.y, 12.0, label.h);
            dividers.push(r.y + SECTION_GAP * 0.5);
            r.take_top(SECTION_GAP);
            (Some(label), Some(slots), Some(prev), Some(next))
        } else {
            (None, None, None, None)
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
            preamp,
            header,
            input,
            inserts,
            inserts_label,
            inserts_grip,
            sends,
            sends_label,
            send_prev,
            send_next,
            pan_knob,
            pan_readout,
            mute,
            solo,
            record,
            level_readout,
            fader,
            meter: meter.inset_xy(0.0, 2.0),
            output,
            tags,
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
        let tall = StripLayout::new(
            Rect::new(0.0, 0.0, 92.0, 760.0),
            &theme,
            true,
            true,
            1,
            5,
            false,
        );
        assert!(tall.input.is_some() && tall.inserts.is_some() && tall.sends.is_some());
        assert_eq!(tall.sends.as_ref().map(Vec::len), Some(SENDS_PER_ROW));
        assert!(tall.fader.h >= MIN_FADER_H);
        let short = StripLayout::new(
            Rect::new(0.0, 0.0, 92.0, 330.0),
            &theme,
            true,
            true,
            4,
            5,
            false,
        );
        assert!(short.inserts.is_none() && short.sends.is_none());
        assert!(short.fader.h >= MIN_FADER_H - 1.0);
        // More send rows when asked for and there is room; fewer when not.
        let many = StripLayout::new(
            Rect::new(0.0, 0.0, 92.0, 900.0),
            &theme,
            true,
            true,
            3,
            5,
            false,
        );
        assert_eq!(many.sends.as_ref().map(Vec::len), Some(3 * SENDS_PER_ROW));
        let squeezed = StripLayout::new(
            Rect::new(0.0, 0.0, 92.0, 560.0),
            &theme,
            true,
            true,
            4,
            5,
            false,
        );
        let n = squeezed.sends.as_ref().map_or(0, Vec::len);
        assert!((SENDS_PER_ROW..4 * SENDS_PER_ROW).contains(&n), "{n}");
        assert!(squeezed.fader.h >= MIN_FADER_H - 1.0);
        // Nothing overlaps the scribble strip.
        assert!(tall.output.bottom() <= tall.scribble.y);
        assert!(tall.fader.bottom() <= tall.output.y);
    }

    #[test]
    fn insert_slots_follow_the_wanted_count_and_the_space() {
        let theme = Theme::default();
        let strip = Rect::new(0.0, 0.0, 92.0, 900.0);
        let five = StripLayout::new(strip, &theme, true, true, 1, 5, false);
        assert_eq!(five.inserts.as_ref().map(Vec::len), Some(5));
        let grip = five.inserts_grip.unwrap();
        assert!(grip.y > five.inserts.as_ref().unwrap()[4].bottom() - 1.0);
        let nine = StripLayout::new(strip, &theme, true, true, 1, 9, false);
        assert_eq!(nine.inserts.as_ref().map(Vec::len), Some(9));
        assert!(
            (nine.inserts_grip.unwrap().y - grip.y - 4.0 * INSERT_SLOT_STEP).abs() < 0.01,
            "the grip moves down by the added slots"
        );
        // A short strip shows fewer, never squeezing the fader.
        let short = StripLayout::new(
            Rect::new(0.0, 0.0, 92.0, 450.0),
            &theme,
            true,
            true,
            1,
            12,
            false,
        );
        let n = short.inserts.as_ref().map_or(0, Vec::len);
        assert!((1..12).contains(&n), "{n}");
        assert!(short.fader.h >= MIN_FADER_H - 1.0);
    }
}
