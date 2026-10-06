//! Realises the toolkit-independent [`WorkspaceLayout`] as GTK widgets.
//!
//! * splits → `gtk::Paned`, tab groups → `gtk::Notebook` (or a bare holder
//!   when a group shows a single view without tabs);
//! * detached groups → their own `gtk::ApplicationWindow` (so application
//!   shortcuts keep working there);
//! * each view has exactly one persistent [`ViewHost`]; rebuilding the
//!   layout only re-parents those widgets, so scroll positions, zoom and
//!   in-progress state survive docking, tabbing and detaching.
//!
//! User changes (divider drags, tab switches, window sizes) are written back
//! into the session's layout model; structural changes go through
//! `WorkspaceAction`s and trigger a rebuild.

use crate::canvas::ViewHost;
use crate::state::AppState;
use faderframe_session::{Action, WorkspaceAction};
use faderframe_workspace::{
    Axis, DockAreaId, DockNode, TabBar, TabGroup, ViewId, ViewKind, WindowGeometry, WindowId,
    WindowRef,
};
use gtk::glib;
use gtk::prelude::*;
use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

#[derive(Default)]
pub struct DockState {
    pub hosts: HashMap<ViewId, (ViewKind, ViewHost)>,
    pub main_slot: Option<gtk::Box>,
    /// The master strip at the window's right edge (shown per workspace).
    pub master_panel: Option<gtk::Widget>,
    pub floating: HashMap<WindowId, gtk::ApplicationWindow>,
    pub rebuilding: Rc<Cell<bool>>,
}

fn create_view(app: &Rc<AppState>, kind: ViewKind) -> ViewHost {
    let theme = app.theme.borrow().clone();
    let host = match kind {
        ViewKind::Arranger => ViewHost::overlaid(
            app,
            Box::new(faderframe_view_arranger::ArrangerView::new(theme)),
        ),
        ViewKind::Mixer => ViewHost::new(
            app,
            Box::new(faderframe_view_mixer::MixerView::new(theme)),
            true,
            false,
        ),
        ViewKind::PianoRoll => ViewHost::new(
            app,
            Box::new(faderframe_view_pianoroll::PianoRollView::new(theme)),
            true,
            true,
        ),
        ViewKind::Automation => ViewHost::new(
            app,
            Box::new(faderframe_view_automation::AutomationView::new(
                theme.clone(),
            )),
            false,
            false,
        ),
        ViewKind::Performance => ViewHost::new(
            app,
            Box::new(faderframe_view_performance::PerformanceView::new(theme)),
            false,
            true,
        ),
        ViewKind::Album => ViewHost::new(
            app,
            Box::new(faderframe_view_album::AlbumView::new(theme)),
            false,
            false,
        ),
        ViewKind::Tools => ViewHost::new(
            app,
            Box::new(faderframe_view_tools::ToolsView::new(theme)),
            false,
            false,
        ),
        ViewKind::History => ViewHost::new(
            app,
            Box::new(faderframe_view_history::HistoryView::new(theme)),
            false,
            true,
        ),
        ViewKind::Modulators => ViewHost::new(
            app,
            Box::new(faderframe_view_modulators::ModulatorsView::new(theme)),
            true,
            false,
        ),
        ViewKind::Pitch => ViewHost::new(
            app,
            Box::new(faderframe_view_pitch::PitchView::new(theme)),
            true,
            true,
        ),
    };
    app.register_canvas(&host.canvas);
    host
}

/// Make sure a host exists for every view of the layout.
fn ensure_hosts(app: &Rc<AppState>, views: &[(ViewId, ViewKind)]) {
    for (id, kind) in views {
        let exists = app
            .dock
            .borrow()
            .hosts
            .get(id)
            .is_some_and(|(k, _)| k == kind);
        if !exists {
            let host = create_view(app, *kind);
            app.dock
                .borrow_mut()
                .hosts
                .insert(id.clone(), (*kind, host));
        }
    }
}

fn host_widget(app: &Rc<AppState>, view: &ViewId) -> Option<gtk::Widget> {
    app.dock
        .borrow()
        .hosts
        .get(view)
        .map(|(_, h)| h.root.clone().upcast())
}

/// A box that holds exactly one view host (the only parent type hosts ever
/// get, which makes detaching them trivial and safe).
fn holder(app: &Rc<AppState>, view: &ViewId) -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 0);
    b.set_hexpand(true);
    b.set_vexpand(true);
    if let Some(w) = host_widget(app, view) {
        b.append(&w);
    }
    b
}

fn title_of(app: &Rc<AppState>, view: &ViewId) -> String {
    app.dock
        .borrow()
        .hosts
        .get(view)
        .map_or_else(|| view.to_string(), |(k, _)| k.title().to_string())
}

fn tab_label(app: &Rc<AppState>, view: &ViewId, window: WindowRef) -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    b.append(&gtk::Label::new(Some(&title_of(app, view))));
    let (icon, tip, action) = match window {
        WindowRef::Main => (
            "window-new-symbolic",
            "Detach to its own window",
            WorkspaceAction::Detach(view.clone()),
        ),
        WindowRef::Floating(_) => (
            "go-bottom-symbolic",
            "Dock back into the main window",
            WorkspaceAction::Attach(view.clone()),
        ),
    };
    let btn = gtk::Button::from_icon_name(icon);
    btn.add_css_class("flat");
    btn.add_css_class("tab-button");
    btn.set_tooltip_text(Some(tip));
    let weak = Rc::downgrade(app);
    btn.connect_clicked(move |_| {
        if let Some(app) = weak.upgrade() {
            app.dispatch(Action::Workspace(action.clone()));
        }
    });
    b.append(&btn);
    b
}

fn placeholder() -> gtk::Widget {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 0);
    b.set_hexpand(true);
    b.set_vexpand(true);
    b.upcast()
}

fn build_tabs(app: &Rc<AppState>, g: &TabGroup, window: WindowRef, path: &[u8]) -> gtk::Widget {
    if g.views.is_empty() {
        return placeholder();
    }
    if g.tab_bar == TabBar::Auto && g.views.len() == 1 {
        return holder(app, &g.views[0]).upcast();
    }
    let nb = gtk::Notebook::new();
    nb.set_scrollable(true);
    nb.set_show_border(false);
    nb.add_css_class("dock-tabs");
    nb.set_hexpand(true);
    nb.set_vexpand(true);
    for v in &g.views {
        let page = holder(app, v);
        nb.append_page(&page, Some(&tab_label(app, v, window)));
    }
    nb.set_current_page(Some(g.active.min(g.views.len() - 1) as u32));
    if g.area == Some(DockAreaId::bottom()) && window == WindowRef::Main {
        let hide = gtk::Button::from_icon_name("pan-down-symbolic");
        hide.add_css_class("flat");
        hide.set_tooltip_text(Some("Hide the bottom dock (F2)"));
        let weak = Rc::downgrade(app);
        hide.connect_clicked(move |_| {
            if let Some(app) = weak.upgrade() {
                app.dispatch(Action::Workspace(WorkspaceAction::ToggleArea(
                    DockAreaId::bottom(),
                )));
            }
        });
        nb.set_action_widget(&hide, gtk::PackType::End);
    }
    let weak = Rc::downgrade(app);
    let path = path.to_vec();
    let rebuilding = app.dock.borrow().rebuilding.clone();
    nb.connect_switch_page(move |_, _, index| {
        if rebuilding.get() {
            return;
        }
        if let Some(app) = weak.upgrade()
            && let Ok(mut s) = app.session.try_borrow_mut()
        {
            s.workspace_mut()
                .active_layout_mut()
                .set_active(window, &path, index as usize);
        }
    });
    nb.upcast()
}

fn build_node(
    app: &Rc<AppState>,
    node: &DockNode,
    window: WindowRef,
    path: Vec<u8>,
) -> gtk::Widget {
    match node {
        DockNode::Tabs(g) => build_tabs(app, g, window, &path),
        DockNode::Split {
            axis,
            ratio,
            first,
            second,
        } => {
            let child = |i: u8| {
                let mut p = path.clone();
                p.push(i);
                p
            };
            match (first.is_visible(), second.is_visible()) {
                (true, false) => return build_node(app, first, window, child(0)),
                (false, true) => return build_node(app, second, window, child(1)),
                (false, false) => return placeholder(),
                (true, true) => {}
            }
            let orientation = match axis {
                Axis::Horizontal => gtk::Orientation::Horizontal,
                Axis::Vertical => gtk::Orientation::Vertical,
            };
            let paned = gtk::Paned::new(orientation);
            paned.set_wide_handle(true);
            paned.set_resize_start_child(true);
            paned.set_resize_end_child(true);
            paned.set_shrink_start_child(false);
            paned.set_shrink_end_child(false);
            paned.set_start_child(Some(&build_node(app, first, window, child(0))));
            paned.set_end_child(Some(&build_node(app, second, window, child(1))));
            let applied = Rc::new(Cell::new(false));
            let ratio = *ratio;
            let extent = move |p: &gtk::Paned| match orientation {
                gtk::Orientation::Horizontal => p.width(),
                _ => p.height(),
            };
            // Positions are pixels; apply the stored ratio once allocated.
            paned.add_tick_callback(glib::clone!(
                #[strong]
                applied,
                move |p, _| {
                    let total = extent(p);
                    if total > 0 {
                        p.set_position((total as f32 * ratio) as i32);
                        applied.set(true);
                        return glib::ControlFlow::Break;
                    }
                    glib::ControlFlow::Continue
                }
            ));
            let weak = Rc::downgrade(app);
            let rebuilding = app.dock.borrow().rebuilding.clone();
            paned.connect_position_notify(move |p| {
                if rebuilding.get() || !applied.get() {
                    return;
                }
                let total = extent(p);
                if total <= 0 {
                    return;
                }
                if let Some(app) = weak.upgrade()
                    && let Ok(mut s) = app.session.try_borrow_mut()
                {
                    s.workspace_mut().active_layout_mut().set_ratio(
                        window,
                        &path,
                        p.position() as f32 / total as f32,
                    );
                }
            });
            paned.upcast()
        }
    }
}

fn floating_window(
    app: &Rc<AppState>,
    id: WindowId,
    geometry: &WindowGeometry,
    title: &str,
) -> gtk::ApplicationWindow {
    let win = gtk::ApplicationWindow::builder()
        .application(&app.app)
        .title(title)
        .default_width(geometry.width.max(320))
        .default_height(geometry.height.max(200))
        .show_menubar(false)
        .build();
    win.add_css_class("ff-window");
    let header = gtk::HeaderBar::new();
    let dock = gtk::Button::with_label("Dock");
    dock.set_tooltip_text(Some("Return this view to the main window"));
    let weak = Rc::downgrade(app);
    dock.connect_clicked(move |_| {
        if let Some(app) = weak.upgrade() {
            app.dispatch(Action::Workspace(WorkspaceAction::CloseWindow(id)));
        }
    });
    header.pack_start(&dock);
    win.set_titlebar(Some(&header));
    if geometry.maximized {
        win.maximize();
    }
    let weak = Rc::downgrade(app);
    win.connect_close_request(move |w| {
        let Some(app) = weak.upgrade() else {
            return glib::Propagation::Proceed;
        };
        if app.dock.borrow().rebuilding.get() {
            return glib::Propagation::Proceed;
        }
        // Remember the size, then dock the views back.
        if let Ok(mut s) = app.session.try_borrow_mut()
            && let Some(f) = s
                .workspace_mut()
                .active_layout_mut()
                .floating_window_mut(id)
        {
            let (width, height) = w.default_size();
            f.geometry.width = width;
            f.geometry.height = height;
            f.geometry.maximized = w.is_maximized();
        }
        app.dispatch(Action::Workspace(WorkspaceAction::CloseWindow(id)));
        glib::Propagation::Stop
    });
    win
}

/// Rebuild the widget tree from the session's active layout.
pub fn realize(app: &Rc<AppState>) {
    let layout = app.session.borrow().workspace().active_layout().clone();
    let views: Vec<(ViewId, ViewKind)> =
        layout.views.iter().map(|(k, v)| (k.clone(), *v)).collect();
    ensure_hosts(app, &views);
    let rebuilding = app.dock.borrow().rebuilding.clone();
    rebuilding.set(true);

    // 1. Detach every view host from its current holder.
    let hosts: Vec<gtk::Widget> = app
        .dock
        .borrow()
        .hosts
        .values()
        .map(|(_, h)| h.root.clone().upcast())
        .collect();
    for w in &hosts {
        if let Some(parent) = w.parent() {
            match parent.downcast::<gtk::Box>() {
                Ok(b) => b.remove(w),
                Err(_) => w.unparent(),
            }
        }
    }

    // 2. Main window content.
    if let Some(slot) = app.dock.borrow().main_slot.clone() {
        while let Some(child) = slot.first_child() {
            slot.remove(&child);
        }
        let tree = build_node(app, &layout.main, WindowRef::Main, Vec::new());
        tree.set_hexpand(true);
        tree.set_vexpand(true);
        slot.append(&tree);
    }

    // 3. Floating windows.
    let wanted: Vec<WindowId> = layout.floating.iter().map(|f| f.id).collect();
    let stale: Vec<(WindowId, gtk::ApplicationWindow)> = app
        .dock
        .borrow()
        .floating
        .iter()
        .filter(|(id, _)| !wanted.contains(id))
        .map(|(id, w)| (*id, w.clone()))
        .collect();
    for (id, w) in stale {
        app.dock.borrow_mut().floating.remove(&id);
        w.destroy();
    }
    for f in &layout.floating {
        let title = {
            let mut names = Vec::new();
            f.root.for_each_group(&mut Vec::new(), &mut |_, g| {
                names.extend(g.views.iter().map(|v| title_of(app, v)));
            });
            format!("{} — FaderFrame", names.join(" · "))
        };
        let existing = app.dock.borrow().floating.get(&f.id).cloned();
        let win = match existing {
            Some(w) => w,
            None => {
                let w = floating_window(app, f.id, &f.geometry, &title);
                app.dock.borrow_mut().floating.insert(f.id, w.clone());
                w
            }
        };
        win.set_title(Some(&title));
        win.set_child(None::<&gtk::Widget>);
        win.set_child(Some(&build_node(
            app,
            &f.root,
            WindowRef::Floating(f.id),
            Vec::new(),
        )));
        win.present();
    }
    rebuilding.set(false);
    // The master panel follows the workspace.
    let panel = app.dock.borrow().master_panel.clone();
    if let Some(panel) = panel
        && panel.is_visible() != layout.master_panel
    {
        panel.set_visible(layout.master_panel);
    }
    if let Some(a) = app
        .app
        .lookup_action("master-panel")
        .and_then(|a| a.downcast::<gtk::gio::SimpleAction>().ok())
    {
        a.set_state(&layout.master_panel.to_variant());
    }
    app.redraw_all();
}
