//! A keyboard for the MIDI effects' displays: keys marked in colours,
//! the Cs named.

use crate::values::note_name;
use faderframe_ui_canvas::{Color, Paint, Painter, Rect, TextStyle, Theme};

fn is_black(key: u8) -> bool {
    matches!(key % 12, 1 | 3 | 6 | 8 | 10)
}

/// Keys `low..=high` (whole octaves look best) across `r`, each key's
/// colour from `mark` (`None`: plain).
pub(crate) fn keyboard(
    p: &mut dyn Painter,
    r: Rect,
    low: u8,
    high: u8,
    mark: &dyn Fn(u8) -> Option<Color>,
    th: &Theme,
) {
    let whites: Vec<u8> = (low..=high).filter(|k| !is_black(*k)).collect();
    if whites.is_empty() {
        return;
    }
    let w = r.w / whites.len() as f32;
    let white = th.ui.text.mix(th.device.display, 0.12);
    let black = th.device.display.darken(0.35);
    p.fill_rounded(
        r.inset(-1.0),
        3.0,
        &Paint::Solid(th.device.display.darken(0.3)),
    );
    for (i, k) in whites.iter().enumerate() {
        let key = Rect::new(r.x + i as f32 * w + 0.5, r.y, w - 1.0, r.h);
        let c = mark(*k).unwrap_or(white);
        p.fill_rounded(
            key,
            2.0,
            &Paint::vertical(key, c.lighten(0.06), c.darken(0.08)),
        );
        if k % 12 == 0 && w >= 12.0 {
            p.text(
                &note_name(i32::from(*k)),
                Rect::new(key.x, key.bottom() - 15.0, key.w, 12.0),
                &TextStyle::new(th.fonts.tiny, th.device.display).center(),
            );
        }
    }
    let bw = w * 0.6;
    let bh = r.h * 0.6;
    for (i, k) in whites.iter().enumerate() {
        let b = k + 1;
        if b > high || !is_black(b) {
            continue;
        }
        let x = r.x + (i + 1) as f32 * w - bw / 2.0;
        let key = Rect::new(x, r.y, bw, bh);
        let c = mark(b).unwrap_or(black);
        p.fill_rounded(
            key,
            2.0,
            &Paint::vertical(key, c.lighten(0.08), c.darken(0.1)),
        );
        p.stroke_rounded(key, 2.0, 1.0, th.device.display.darken(0.4));
    }
}

/// Whole octaves round `keys` (at least `octaves` of them, C to C).
pub(crate) fn span(keys: &[u8], octaves: u8) -> (u8, u8) {
    let lo = keys.iter().min().copied().unwrap_or(48);
    let hi = keys.iter().max().copied().unwrap_or(lo);
    let low = (lo / 12) * 12;
    let high = ((hi / 12 + 1) * 12)
        .max(low.saturating_add(12 * octaves))
        .min(120);
    (low.min(high.saturating_sub(12)), high)
}
