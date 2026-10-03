//! FaderFrame's own symbolic icons. They are written to a cache directory
//! that is added to the icon theme's search path, so GTK recolours them
//! like the stock symbolic icons.

use gtk::prelude::*;
use gtk::{gdk, glib};

const METRONOME: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16">
<path fill="#2e3436" fill-rule="evenodd" d="M5.6 1h4.8l3 14H2.6zM6.6 2.6 4.4 13.5h7.2L9.4 2.6z"/>
<path fill="#2e3436" d="M4.8 10.9h6.4v1.3H4.8z"/>
<path fill="#2e3436" d="M7.4 11.2 12.1 3.3l1.1.65-4.7 7.9z"/>
<circle fill="#2e3436" cx="10.8" cy="6.4" r="1.25"/>
</svg>
"##;

const ICONS: &[(&str, &str)] = &[("faderframe-metronome-symbolic", METRONOME)];

/// Write the icons and register their directory (once, at start-up).
pub fn install() {
    let Some(display) = gdk::Display::default() else {
        return;
    };
    let dir = glib::user_cache_dir().join("faderframe").join("icons");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        tracing::warn!("cannot create {}: {e}", dir.display());
        return;
    }
    for (name, svg) in ICONS {
        let path = dir.join(format!("{name}.svg"));
        if std::fs::read_to_string(&path).ok().as_deref() != Some(*svg)
            && let Err(e) = std::fs::write(&path, svg)
        {
            tracing::warn!("cannot write {}: {e}", path.display());
        }
    }
    gtk::IconTheme::for_display(&display).add_search_path(&dir);
}

/// An icon, or `fallback` text when it is not available.
pub fn image(name: &str, fallback: &str) -> gtk::Widget {
    let found =
        gdk::Display::default().is_some_and(|d| gtk::IconTheme::for_display(&d).has_icon(name));
    if found {
        gtk::Image::from_icon_name(name).upcast()
    } else {
        gtk::Label::new(Some(fallback)).upcast()
    }
}
