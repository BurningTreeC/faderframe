/// Linear-blend RGBA colour with components in 0..=1 (sRGB encoded).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Color {
    pub const TRANSPARENT: Color = Color::rgba(0.0, 0.0, 0.0, 0.0);
    pub const BLACK: Color = Color::rgb(0.0, 0.0, 0.0);
    pub const WHITE: Color = Color::rgb(1.0, 1.0, 1.0);

    pub const fn rgb(r: f32, g: f32, b: f32) -> Self {
        Self { r, g, b, a: 1.0 }
    }

    pub const fn rgba(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self { r, g, b, a }
    }

    pub fn rgb8(r: u8, g: u8, b: u8) -> Self {
        Self::rgb(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0)
    }

    /// `0xRRGGBB`.
    pub const fn hex(v: u32) -> Self {
        Self::rgb(
            ((v >> 16) & 0xff) as f32 / 255.0,
            ((v >> 8) & 0xff) as f32 / 255.0,
            (v & 0xff) as f32 / 255.0,
        )
    }

    pub fn with_alpha(self, a: f32) -> Self {
        Self { a, ..self }
    }

    pub fn mix(self, other: Color, t: f32) -> Self {
        let t = t.clamp(0.0, 1.0);
        Self {
            r: self.r + (other.r - self.r) * t,
            g: self.g + (other.g - self.g) * t,
            b: self.b + (other.b - self.b) * t,
            a: self.a + (other.a - self.a) * t,
        }
    }

    pub fn lighten(self, t: f32) -> Self {
        self.mix(Color::WHITE.with_alpha(self.a), t)
    }

    pub fn darken(self, t: f32) -> Self {
        self.mix(Color::BLACK.with_alpha(self.a), t)
    }

    /// Perceived luminance (for choosing text colour on top).
    pub fn luminance(self) -> f32 {
        0.2126 * self.r + 0.7152 * self.g + 0.0722 * self.b
    }

    /// WCAG 2 relative luminance (linear light; alpha ignored).
    pub fn relative_luminance(self) -> f32 {
        let lin = |c: f32| {
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * lin(self.r) + 0.7152 * lin(self.g) + 0.0722 * lin(self.b)
    }

    /// WCAG 2 contrast ratio with `other` (1 … 21; AA asks 4.5 for text,
    /// 3 for large text and the parts of controls).
    pub fn contrast(self, other: Color) -> f32 {
        let (a, b) = (self.relative_luminance(), other.relative_luminance());
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    /// This colour (with its alpha) laid over an opaque `below`.
    pub fn over(self, below: Color) -> Color {
        below.mix(self.with_alpha(1.0), self.a).with_alpha(1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_and_mixing() {
        let c = Color::hex(0xff8000);
        assert_eq!(c.r, 1.0);
        assert!((c.g - 0.502).abs() < 0.01);
        let m = Color::BLACK.mix(Color::WHITE, 0.5);
        assert!((m.r - 0.5).abs() < 1e-6);
        assert!(Color::WHITE.luminance() > Color::hex(0x202020).luminance());
        assert!((Color::WHITE.contrast(Color::BLACK) - 21.0).abs() < 1e-3);
        assert!((Color::hex(0x777777).contrast(Color::WHITE) - 4.48).abs() < 0.01);
        assert_eq!(Color::BLACK.contrast(Color::BLACK), 1.0);
        let half = Color::WHITE.with_alpha(0.5).over(Color::BLACK);
        assert!((half.r - 0.5).abs() < 1e-6 && half.a == 1.0);
    }
}
