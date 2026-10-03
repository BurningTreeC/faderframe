use crate::Color;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FontFamily {
    /// UI sans-serif (the desktop font).
    #[default]
    Sans,
    /// Tabular figures for time/level readouts.
    Mono,
    /// Narrow face for scribble strips and dense labels.
    Condensed,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FontWeight {
    #[default]
    Normal,
    Medium,
    Bold,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Align {
    #[default]
    Start,
    Center,
    End,
}

/// Single-line text style. Text is laid out inside a rectangle with the
/// given alignment and ellipsised when it does not fit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextStyle {
    /// Size in logical pixels.
    pub size: f32,
    pub family: FontFamily,
    pub weight: FontWeight,
    pub color: Color,
    pub align: Align,
    pub valign: Align,
    /// Extra letter spacing in pixels (engraved panel labels).
    pub tracking: f32,
}

impl TextStyle {
    pub fn new(size: f32, color: Color) -> Self {
        Self {
            size,
            family: FontFamily::Sans,
            weight: FontWeight::Normal,
            color,
            align: Align::Start,
            valign: Align::Center,
            tracking: 0.0,
        }
    }

    pub fn family(mut self, f: FontFamily) -> Self {
        self.family = f;
        self
    }

    pub fn weight(mut self, w: FontWeight) -> Self {
        self.weight = w;
        self
    }

    pub fn bold(self) -> Self {
        self.weight(FontWeight::Bold)
    }

    pub fn align(mut self, a: Align) -> Self {
        self.align = a;
        self
    }

    pub fn center(self) -> Self {
        self.align(Align::Center)
    }

    pub fn right(self) -> Self {
        self.align(Align::End)
    }

    pub fn valign(mut self, a: Align) -> Self {
        self.valign = a;
        self
    }

    pub fn color(mut self, c: Color) -> Self {
        self.color = c;
        self
    }

    pub fn tracking(mut self, t: f32) -> Self {
        self.tracking = t;
        self
    }
}
