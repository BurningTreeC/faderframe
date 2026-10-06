//! FaderFrame's own icons: the symbolic ones and the application icon. They
//! are written into a `hicolor` theme folder in a cache directory that is
//! added to the icon theme's search path, so GTK recolours the symbolic ones
//! like the stock icons and windows show the application icon even when
//! FaderFrame is not installed. (GTK before 4.16 does not recolour loose
//! `-symbolic` files on the search path: they must be in a theme.)

use gtk::gdk;
use gtk::prelude::*;

const METRONOME: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16">
<path fill="#2e3436" fill-rule="evenodd" d="M5.6 1h4.8l3 14H2.6zM6.6 2.6 4.4 13.5h7.2L9.4 2.6z"/>
<path fill="#2e3436" d="M4.8 10.9h6.4v1.3H4.8z"/>
<path fill="#2e3436" d="M7.4 11.2 12.1 3.3l1.1.65-4.7 7.9z"/>
<circle fill="#2e3436" cx="10.8" cy="6.4" r="1.25"/>
</svg>
"##;

/// Capture MIDI: an arrow circling back around a record dot.
const CAPTURE: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16">
<path fill="#2e3436" d="M2.28 4.70A6.6 6.6 0 1 1 1.50 9.15L3.08 8.87A5.0 5.0 0 1 0 3.67 5.50z"/>
<path fill="#2e3436" d="M0.90 3.90L2.20 7.80L5.06 6.30z"/>
<circle fill="#2e3436" cx="8" cy="8" r="2.4"/>
</svg>
"##;

const APP_ICON: &str =
    include_str!("../../../packaging/icons/io.github.BurningTreeC.FaderFrame.svg");

const ICONS: &[(&str, &str)] = &[
    ("faderframe-metronome-symbolic", METRONOME),
    ("faderframe-capture-symbolic", CAPTURE),
    (crate::APP_ID, APP_ICON),
];

/// Used only where the system has no hicolor theme: search paths added by
/// the program come last, so an installed theme's index wins.
const HICOLOR_INDEX: &str = "[Icon Theme]
Name=Hicolor
Comment=Fallback icon theme
Directories=scalable/apps

[scalable/apps]
Size=16
MinSize=1
MaxSize=512
Type=Scalable
";

/// Write `contents` to `path` unless it holds them already.
fn write_if_changed(path: &std::path::Path, contents: &str) {
    if std::fs::read_to_string(path).ok().as_deref() != Some(contents)
        && let Err(e) = std::fs::write(path, contents)
    {
        tracing::warn!("cannot write {}: {e}", path.display());
    }
}

/// Write the icons and register their directory (once, at start-up).
pub fn install() {
    let Some(display) = gdk::Display::default() else {
        return;
    };
    let dir = crate::paths::cache_dir().join("icons");
    let apps = dir.join("hicolor").join("scalable").join("apps");
    if let Err(e) = std::fs::create_dir_all(&apps) {
        tracing::warn!("cannot create {}: {e}", apps.display());
        return;
    }
    write_if_changed(&dir.join("hicolor").join("index.theme"), HICOLOR_INDEX);
    for (name, svg) in ICONS {
        write_if_changed(&apps.join(format!("{name}.svg")), svg);
        // Earlier versions wrote them loose into the search path.
        let _ = std::fs::remove_file(dir.join(format!("{name}.svg")));
    }
    gtk::IconTheme::for_display(&display).add_search_path(&dir);
    gtk::Window::set_default_icon_name(crate::APP_ID);
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
