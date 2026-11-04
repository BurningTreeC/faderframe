//! Choosing a colour: GTK's colour chooser (its palette, and the editor
//! with the plane, hue strip, hex entry and eyedropper) in a window of
//! FaderFrame's own, for track and section colours and the theme editor.
//!
//! `gtk::ColorDialog` shows the same chooser in a dialog that holds its
//! content to 262 px high and cannot be resized, while the editor needs
//! about 350 px: on desktops that keep the dialog at the size it first
//! asked for, the editor's colour plane was cut off and could not be
//! reached (#40). This window has room for the larger of the two pages
//! from the start (switching pages never needs the desktop to grow it),
//! and it can be made larger.
//!
//! `gtk::ColorChooserWidget` is deprecated since GTK 4.10 in favour of
//! `ColorDialog`, which is itself built on it; it stays for all of GTK 4.

#![allow(deprecated)]

use gtk::prelude::*;
use gtk::{gdk, glib};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// The page the chooser opens on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    /// GTK's palette (its "+" goes on to the editor).
    Palette,
    /// The editor, on the colour given.
    Editor,
}

/// `#RRGGBB` of `c`.
pub fn hex(c: &gdk::RGBA) -> String {
    let to8 = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!(
        "#{:02X}{:02X}{:02X}",
        to8(c.red()),
        to8(c.green()),
        to8(c.blue())
    )
}

/// Choose a colour over `parent`, starting at `initial` on `page`;
/// `chosen` gets the colour selected (it is not called when the chooser
/// is cancelled or closed).
pub fn choose(
    parent: Option<&gtk::Window>,
    title: &str,
    initial: gdk::RGBA,
    page: Page,
    chosen: impl Fn(gdk::RGBA) + 'static,
) {
    let win = gtk::Window::builder()
        .title(title)
        .modal(true)
        .destroy_with_parent(true)
        .build();
    win.set_transient_for(parent);
    let body = gtk::Box::new(gtk::Orientation::Vertical, 10);
    body.set_margin_top(14);
    body.set_margin_bottom(14);
    body.set_margin_start(14);
    body.set_margin_end(14);
    let chooser = gtk::ColorChooserWidget::new();
    chooser.set_use_alpha(false);
    chooser.set_rgba(&initial);
    chooser.set_hexpand(true);
    chooser.set_vexpand(true);
    body.append(&chooser);
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label("Cancel");
    let select = gtk::Button::with_label("Select");
    select.add_css_class("suggested-action");
    buttons.append(&cancel);
    buttons.append(&select);
    body.append(&buttons);
    win.set_child(Some(&body));
    win.set_default_widget(Some(&select));

    // Room for either page: the palette is the wider, the editor the
    // taller.
    let mut room = (0, 0);
    for editor in [false, true] {
        chooser.set_show_editor(editor);
        let (_, w, _, _) = chooser.measure(gtk::Orientation::Horizontal, -1);
        let (_, h, _, _) = chooser.measure(gtk::Orientation::Vertical, -1);
        room = (room.0.max(w), room.1.max(h));
    }
    chooser.set_size_request(room.0, room.1);
    chooser.set_show_editor(page == Page::Editor);

    let chosen = Rc::new(chosen);
    let w = win.downgrade();
    cancel.connect_clicked(move |_| {
        if let Some(w) = w.upgrade() {
            w.close();
        }
    });
    let (w, c, f) = (win.downgrade(), chooser.downgrade(), Rc::clone(&chosen));
    select.connect_clicked(move |_| {
        let (Some(w), Some(c)) = (w.upgrade(), c.upgrade()) else {
            return;
        };
        let rgba = c.rgba();
        // Set again, the chooser keeps it among its custom colours (as
        // GTK's own dialog does when a colour is selected).
        c.set_rgba(&rgba);
        w.close();
        f(rgba);
    });
    // A double-click on a swatch selects it.
    let (w, f) = (win.downgrade(), chosen);
    chooser.connect_color_activated(move |c, rgba| {
        c.set_rgba(rgba);
        if let Some(w) = w.upgrade() {
            w.close();
        }
        f(*rgba);
    });
    // Escape closes it.
    let keys = gtk::EventControllerKey::new();
    let w = win.downgrade();
    keys.connect_key_pressed(move |_, key, _, _| {
        if key == gdk::Key::Escape
            && let Some(w) = w.upgrade()
        {
            w.close();
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    win.add_controller(keys);
    win.present();
}

/// What a [`ColorButton`] calls with each colour chosen with it.
type Chosen = Rc<RefCell<Option<Rc<dyn Fn(gdk::RGBA)>>>>;

/// A button showing a colour; a click opens the chooser on it (what
/// `gtk::ColorDialogButton` did, with the chooser above).
#[derive(Clone)]
pub struct ColorButton {
    button: gtk::Button,
    swatch: gtk::DrawingArea,
    title: Rc<str>,
    color: Rc<Cell<gdk::RGBA>>,
    chosen: Chosen,
}

impl ColorButton {
    /// A button for the colour of `title` (the chooser's title), whose
    /// chooser opens on `page`.
    pub fn new(title: &str, page: Page) -> Self {
        let color = Rc::new(Cell::new(gdk::RGBA::BLACK));
        let swatch = gtk::DrawingArea::new();
        swatch.set_content_width(32);
        swatch.set_content_height(16);
        let c = Rc::clone(&color);
        swatch.set_draw_func(move |area, cr, w, h| {
            let (w, h) = (f64::from(w) - 1.0, f64::from(h) - 1.0);
            let r = 3.0_f64.min(w / 2.0).min(h / 2.0);
            let (x, y) = (0.5, 0.5);
            let quarter = std::f64::consts::FRAC_PI_2;
            cr.new_sub_path();
            cr.arc(x + w - r, y + r, r, -quarter, 0.0);
            cr.arc(x + w - r, y + h - r, r, 0.0, quarter);
            cr.arc(x + r, y + h - r, r, quarter, 2.0 * quarter);
            cr.arc(x + r, y + r, r, 2.0 * quarter, 3.0 * quarter);
            cr.close_path();
            let v = c.get();
            cr.set_source_rgb(v.red().into(), v.green().into(), v.blue().into());
            let _ = cr.fill_preserve();
            // Its edge in the text colour, faint: seen on any background.
            let e = area.color();
            cr.set_source_rgba(e.red().into(), e.green().into(), e.blue().into(), 0.35);
            cr.set_line_width(1.0);
            let _ = cr.stroke();
        });
        let button = gtk::Button::new();
        button.set_child(Some(&swatch));
        let this = Self {
            button,
            swatch,
            title: Rc::from(title),
            color,
            chosen: Rc::new(RefCell::new(None)),
        };
        // The closure holds no reference to the button itself (it gets
        // it), so nothing keeps the widgets alive past their window.
        let (title, color, chosen, swatch) = (
            Rc::clone(&this.title),
            Rc::clone(&this.color),
            Rc::clone(&this.chosen),
            this.swatch.downgrade(),
        );
        this.button.connect_clicked(move |b| {
            let parent = b.root().and_downcast::<gtk::Window>();
            let (title2, color2, chosen, swatch, button) = (
                Rc::clone(&title),
                Rc::clone(&color),
                Rc::clone(&chosen),
                swatch.clone(),
                b.downgrade(),
            );
            choose(parent.as_ref(), &title, color.get(), page, move |c| {
                color2.set(c);
                if let Some(s) = swatch.upgrade() {
                    s.queue_draw();
                }
                if let Some(b) = button.upgrade() {
                    describe(&b, &title2, &c);
                }
                let f = chosen.borrow().clone();
                if let Some(f) = f {
                    f(c);
                }
            });
        });
        this.set_rgba(&gdk::RGBA::BLACK);
        this
    }

    pub fn widget(&self) -> &gtk::Button {
        &self.button
    }

    /// Show `c` (the chosen callback is not called).
    pub fn set_rgba(&self, c: &gdk::RGBA) {
        self.color.set(*c);
        self.swatch.queue_draw();
        describe(&self.button, &self.title, c);
    }

    /// Call `f` with each colour chosen with the button.
    pub fn connect_chosen(&self, f: impl Fn(gdk::RGBA) + 'static) {
        *self.chosen.borrow_mut() = Some(Rc::new(f));
    }
}

/// The button's tooltip and accessible name: what it is and its colour.
fn describe(button: &gtk::Button, title: &str, c: &gdk::RGBA) {
    let hex = hex(c);
    button.set_tooltip_text(Some(&hex));
    button.update_property(&[gtk::accessible::Property::Label(&format!("{title}: {hex}"))]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_are_written_as_hex() {
        assert_eq!(hex(&gdk::RGBA::new(0.75, 0.25, 0.25, 1.0)), "#BF4040");
        assert_eq!(hex(&gdk::RGBA::new(0.0, 1.0, 2.0, 1.0)), "#00FFFF");
    }
}
