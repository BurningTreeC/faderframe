//! Track header layout (pure geometry).

use faderframe_ui_canvas::Rect;

pub const STRIPE_W: f32 = 5.0;
const METER_W: f32 = 9.0;
const BUTTON_W: f32 = 21.0;
const BUTTON_H: f32 = 15.0;
const GAP: f32 = 3.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HeaderLayout {
    pub row: Rect,
    pub stripe: Rect,
    pub name: Rect,
    pub info: Option<Rect>,
    pub mute: Rect,
    pub solo: Rect,
    pub record: Rect,
    pub monitor: Rect,
    /// Show/hide the track's automation lanes.
    pub automation: Rect,
    pub pan: Option<Rect>,
    pub volume: Rect,
    pub volume_text: Rect,
    pub meter: Rect,
}

impl HeaderLayout {
    pub fn new(row: Rect) -> Self {
        let stripe = Rect::new(row.x, row.y, STRIPE_W, row.h);
        let meter = Rect::new(
            row.right() - METER_W - 6.0,
            row.y + 6.0,
            METER_W,
            (row.h - 12.0).max(4.0),
        );
        let inner = Rect::new(
            row.x + STRIPE_W + 8.0,
            row.y + 6.0,
            meter.x - (row.x + STRIPE_W + 8.0) - 8.0,
            row.h - 12.0,
        );
        let buttons_w = 5.0 * BUTTON_W + 4.0 * GAP;
        let bx = inner.right() - buttons_w;
        let b = |i: f32| Rect::new(bx + i * (BUTTON_W + GAP), inner.y, BUTTON_W, BUTTON_H);
        let name = Rect::new(inner.x, inner.y, (bx - inner.x - 6.0).max(10.0), 16.0);
        let roomy = row.h >= 54.0;
        let info = roomy.then(|| Rect::new(inner.x, inner.y + 18.0, name.w, 12.0));
        let pan = roomy.then(|| {
            let s = 22.0;
            Rect::new(inner.right() - s, inner.y + BUTTON_H + 4.0, s, s)
        });
        let vol_y = inner.bottom() - 11.0;
        let text_w = 42.0;
        let vol_right = match pan {
            Some(p) => p.x - 6.0 - text_w,
            None => inner.right() - text_w,
        };
        let volume = Rect::new(inner.x, vol_y, (vol_right - inner.x).max(20.0), 11.0);
        let volume_text = Rect::new(volume.right() + 4.0, vol_y - 1.0, text_w - 4.0, 13.0);
        Self {
            row,
            stripe,
            name,
            info,
            mute: b(0.0),
            solo: b(1.0),
            record: b(2.0),
            monitor: b(3.0),
            automation: b(4.0),
            pan,
            volume,
            volume_text,
            meter,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controls_stay_inside_the_row_and_do_not_overlap() {
        let l = HeaderLayout::new(Rect::new(0.0, 100.0, 252.0, 72.0));
        for r in [
            l.name,
            l.mute,
            l.solo,
            l.record,
            l.monitor,
            l.automation,
            l.volume,
            l.volume_text,
            l.meter,
        ] {
            assert!(
                r.x >= 0.0 && r.right() <= 252.0 && r.y >= 100.0 && r.bottom() <= 172.0,
                "{r:?}"
            );
        }
        assert!(!l.name.intersects(&l.mute));
        assert!(!l.volume.intersects(&l.pan.unwrap()));
        assert!(
            HeaderLayout::new(Rect::new(0.0, 0.0, 252.0, 40.0))
                .pan
                .is_none()
        );
    }
}
