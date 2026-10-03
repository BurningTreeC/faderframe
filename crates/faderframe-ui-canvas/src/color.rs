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
    }
}
