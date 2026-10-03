use faderframe_session::{Action, Session};
use faderframe_ui_canvas::{CanvasView, EventCx, Painter, Rect, Size, TextStyle, Theme, ViewEvent};

/// A clearly labelled stand-in for editors that are not implemented yet.
pub struct PlaceholderView {
    title: &'static str,
    message: &'static str,
}

impl PlaceholderView {
    pub fn new(title: &'static str, message: &'static str) -> Self {
        Self { title, message }
    }
}

impl CanvasView<Session, Action> for PlaceholderView {
    fn paint(&mut self, p: &mut dyn Painter, size: Size, _model: &Session, theme: &Theme) {
        p.fill(Rect::from_size(size), theme.ui.surface);
        let c = Rect::from_size(size).centered(size.w.min(720.0), 60.0);
        let (top, bottom) = c.split_top(28.0);
        p.text(
            self.title,
            top,
            &TextStyle::new(theme.fonts.large, theme.ui.text_dim)
                .bold()
                .center(),
        );
        p.text(
            self.message,
            bottom,
            &TextStyle::new(theme.fonts.normal, theme.ui.text_faint).center(),
        );
    }

    fn event(
        &mut self,
        _ev: &ViewEvent,
        _size: Size,
        _model: &Session,
        _cx: &mut EventCx<'_, Action>,
    ) -> bool {
        false
    }
}
