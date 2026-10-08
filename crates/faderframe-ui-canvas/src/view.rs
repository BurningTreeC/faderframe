use crate::{Cursor, Painter, Point, Rect, Size, Theme, ViewEvent};

/// An entry of a context menu requested by a view.
#[derive(Clone)]
pub struct MenuItem<A> {
    pub label: String,
    /// `None` renders a disabled entry (unless it opens `children`).
    pub action: Option<A>,
    pub checked: Option<bool>,
    /// Draw a separator before this entry.
    pub separator_before: bool,
    /// A submenu (the entry opens it instead of acting).
    pub children: Vec<MenuItem<A>>,
}

impl<A> MenuItem<A> {
    pub fn new(label: impl Into<String>, action: A) -> Self {
        Self {
            label: label.into(),
            action: Some(action),
            checked: None,
            separator_before: false,
            children: Vec::new(),
        }
    }

    pub fn disabled(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            action: None,
            checked: None,
            separator_before: false,
            children: Vec::new(),
        }
    }

    /// An entry that opens `children`.
    pub fn submenu(label: impl Into<String>, children: Vec<MenuItem<A>>) -> Self {
        Self {
            label: label.into(),
            action: None,
            checked: None,
            separator_before: false,
            children,
        }
    }

    pub fn checked(mut self, on: bool) -> Self {
        self.checked = Some(on);
        self
    }

    pub fn separated(mut self) -> Self {
        self.separator_before = true;
        self
    }
}

/// Turns committed text into an action (or nothing).
pub type TextCommit<A> = Box<dyn Fn(&str) -> Option<A>>;

/// Turns chosen paths into an action (or nothing).
pub type FilesCommit<A> = Box<dyn Fn(Vec<std::path::PathBuf>) -> Option<A>>;

/// What a file chooser asks for.
#[derive(Clone, Debug, PartialEq)]
pub enum FileChoice {
    /// Existing files (several), offered by filters: (name, patterns like
    /// `*.wav`).
    Open {
        title: String,
        filters: Vec<(String, Vec<String>)>,
    },
    /// A folder.
    Folder {
        title: String,
        initial: Option<std::path::PathBuf>,
    },
    /// A file to write, `name` offered.
    Save {
        title: String,
        name: String,
        filters: Vec<(String, Vec<String>)>,
    },
}

/// Things only the toolkit host can do on a view's behalf (native popovers,
/// text entry). Keeps views free of toolkit types.
pub enum HostRequest<A> {
    ContextMenu {
        at: Point,
        items: Vec<MenuItem<A>>,
    },
    /// Inline text/number entry (rename, numeric value entry).
    TextInput {
        at: Rect,
        initial: String,
        commit: TextCommit<A>,
    },
    GrabFocus,
    /// A native file or folder chooser.
    ChooseFiles {
        choice: FileChoice,
        commit: FilesCommit<A>,
    },
}

/// Collects what a view wants done in response to an event.
pub struct EventCx<'a, A> {
    actions: &'a mut Vec<A>,
    requests: &'a mut Vec<HostRequest<A>>,
    redraw: bool,
    cursor: Option<Cursor>,
}

impl<'a, A> EventCx<'a, A> {
    pub fn new(actions: &'a mut Vec<A>, requests: &'a mut Vec<HostRequest<A>>) -> Self {
        Self {
            actions,
            requests,
            redraw: false,
            cursor: None,
        }
    }

    /// Ask the session to do something.
    pub fn emit(&mut self, action: A) {
        self.actions.push(action);
        self.redraw = true;
    }

    pub fn request(&mut self, req: HostRequest<A>) {
        self.requests.push(req);
    }

    pub fn redraw(&mut self) {
        self.redraw = true;
    }

    pub fn set_cursor(&mut self, c: Cursor) {
        self.cursor = Some(c);
    }

    pub fn wants_redraw(&self) -> bool {
        self.redraw
    }

    pub fn cursor(&self) -> Option<Cursor> {
        self.cursor
    }
}

/// Scroll state along one axis, for hosts that show native scrollbars.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScrollInfo {
    /// Total content extent in pixels.
    pub content: f32,
    /// Visible extent in pixels.
    pub viewport: f32,
    /// Current offset in pixels.
    pub offset: f32,
    /// Where the scrolled area begins and ends, in pixels from the view's
    /// leading and trailing edges (track headers, a ruler, a pinned
    /// section): the host's scrollbar spans only the scrolled part.
    pub start: f32,
    pub end: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrollAxis {
    Horizontal,
    Vertical,
}

/// A custom-rendered editor surface (arranger, mixer, piano roll ...).
///
/// `M` is the shared model (the session) read during painting and event
/// handling; `A` is the action type views emit to change it. A view owns
/// only presentation state (scroll, zoom, hover, drag) — never project data —
/// so the same instance can be re-hosted in any dock or window.
pub trait CanvasView<M, A> {
    fn paint(&mut self, p: &mut dyn Painter, size: Size, model: &M, theme: &Theme);

    /// Handle an event; return `true` if it was consumed.
    fn event(&mut self, ev: &ViewEvent, size: Size, model: &M, cx: &mut EventCx<'_, A>) -> bool;

    /// The skin changed (views that keep a copy of the theme replace it).
    fn set_theme(&mut self, _theme: &Theme) {}

    /// Before each paint: how far ahead (ns) the frame being drawn is
    /// expected on screen, from the toolkit's frame clock and its
    /// presentation feedback (video shows the picture for that moment), and
    /// the device pixels per logical pixel.
    fn frame_timing(&mut self, _lead_ns: i64, _scale: f32) {}

    /// Should the host keep redrawing every frame (meters, playhead)?
    fn wants_frames(&self, _model: &M) -> bool {
        false
    }

    /// Is the view's geometry dense and changing every frame (analysers,
    /// meters, curves)? A host with a GPU painter draws such views with it
    /// (the toolkit's renderer rasterises every changed path on the CPU).
    fn dense(&self) -> bool {
        false
    }

    fn tooltip(&self, _pos: Point, _size: Size, _model: &M) -> Option<String> {
        None
    }

    /// Minimum useful size.
    fn min_size(&self) -> Size {
        Size::new(100.0, 60.0)
    }

    fn scroll_info(&self, _axis: ScrollAxis, _size: Size, _model: &M) -> Option<ScrollInfo> {
        None
    }

    fn set_scroll(&mut self, _axis: ScrollAxis, _offset: f32) {}

    /// The pointer holds one of the view's scrollbars (`held`), or let go
    /// of it: a view that scrolls by itself (following the playhead)
    /// leaves the scrolling to the hand meanwhile.
    fn scroll_held(&mut self, _axis: ScrollAxis, _held: bool) {}

    /// Files are dragged over the view at `pos` (`None`: the drag left).
    /// Return whether dropping there would be accepted (views typically
    /// remember the position to paint a drop indicator).
    fn drag_files(&mut self, _pos: Option<Point>, _size: Size, _model: &M) -> bool {
        false
    }

    /// Files were dropped at `pos`.
    fn drop_files(
        &mut self,
        _files: &[std::path::PathBuf],
        _pos: Point,
        _size: Size,
        _model: &M,
    ) -> Option<A> {
        None
    }

    /// What the drag in progress carries to other views when it leaves
    /// this one (an opaque payload, e.g. `clips:1,2`); `None`: nothing.
    fn drag_payload(&self, _model: &M) -> Option<String> {
        None
    }

    /// The drag left this view and another took its payload: undo what the
    /// drag did here (and forget it).
    fn cancel_drag(&mut self, _cx: &mut EventCx<'_, A>) {}

    /// Another view's payload hovers at `pos` (`None`: it left); true to
    /// redraw.
    fn hover_payload(&mut self, _payload: Option<(&str, Point)>, _size: Size, _model: &M) -> bool {
        false
    }

    /// Another view's payload was dropped at `pos`.
    fn drop_payload(&mut self, _payload: &str, _pos: Point, _size: Size, _model: &M) -> Option<A> {
        None
    }
}
