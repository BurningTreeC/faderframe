//! FaderFrame.app on macOS. The program itself is the bundle's executable:
//! Launch Services reads the architecture of a bundle from its executable,
//! and a launcher script has none, so Apple silicon took the app for an
//! Intel one (Rosetta, "not supported"). The program points GLib, GTK,
//! GdkPixbuf and GStreamer at the data inside the bundle itself, before any
//! thread exists (the launcher script's work until 0.31).

#[cfg(any(test, target_os = "macos"))]
use std::ffi::OsString;
#[cfg(any(test, target_os = "macos"))]
use std::path::{Path, PathBuf};

/// The bundle's `Contents` folder when `exe` is the executable of an
/// `.app` bundle with its resources (`<x>.app/Contents/MacOS/<exe>`).
#[cfg(any(test, target_os = "macos"))]
fn bundle_contents(exe: &Path) -> Option<PathBuf> {
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    let app = contents.parent()?;
    let is_bundle = macos.file_name()? == "MacOS"
        && contents.file_name()? == "Contents"
        && app.extension()? == "app";
    (is_bundle && contents.join("Resources").join("share").is_dir()).then(|| contents.to_owned())
}

/// The environment the bundle's data needs: `(name, value)`, given the
/// `XDG_DATA_DIRS` inherited, the pixbuf loader cache written for this
/// bundle (if any) and the home folder.
#[cfg(any(test, target_os = "macos"))]
fn bundle_environment(
    contents: &Path,
    data_dirs: Option<OsString>,
    loader_cache: Option<PathBuf>,
    home: Option<&Path>,
) -> Vec<(&'static str, OsString)> {
    let res = contents.join("Resources");
    let mut dirs = vec![res.join("share")];
    match data_dirs.filter(|d| !d.is_empty()) {
        Some(d) => dirs.extend(std::env::split_paths(&d)),
        None => dirs.extend(["/usr/local/share", "/usr/share"].map(PathBuf::from)),
    }
    let mut env = vec![
        (
            "XDG_DATA_DIRS",
            std::env::join_paths(dirs).unwrap_or_else(|_| res.join("share").into_os_string()),
        ),
        (
            "GSETTINGS_SCHEMA_DIR",
            res.join("share/glib-2.0/schemas").into_os_string(),
        ),
        // Video: the bundle's GStreamer plugins (Homebrew's build looks in
        // its own prefix otherwise), their registry in the user's caches.
        (
            "GST_PLUGIN_SYSTEM_PATH_1_0",
            res.join("lib/gstreamer-1.0").into_os_string(),
        ),
        (
            "GST_PLUGIN_SCANNER_1_0",
            contents.join("MacOS/gst-plugin-scanner").into_os_string(),
        ),
    ];
    if let Some(home) = home {
        env.push((
            "GST_REGISTRY_1_0",
            home.join("Library/Caches/faderframe/gstreamer-registry.bin")
                .into_os_string(),
        ));
    }
    if let Some(cache) = loader_cache {
        env.push(("GDK_PIXBUF_MODULE_FILE", cache.into_os_string()));
    }
    env
}

/// The image loaders' cache with the bundle's path in it (the bundle
/// holds it with a placeholder: the app may be anywhere).
#[cfg(any(test, target_os = "macos"))]
fn loader_cache(contents: &Path, dir: &Path) -> Option<PathBuf> {
    let res = contents.join("Resources");
    let template = res.join("lib/gdk-pixbuf-2.0/2.10.0/loaders.cache.in");
    let text = std::fs::read_to_string(template).ok()?;
    let text = text.replace("@RESOURCES@", &res.to_string_lossy());
    let cache = dir.join("faderframe-pixbuf-loaders.cache");
    if std::fs::read_to_string(&cache).ok().as_deref() != Some(text.as_str()) {
        std::fs::write(&cache, text).ok()?;
    }
    Some(cache)
}

/// Point the libraries at the bundle's data when running from
/// FaderFrame.app. Call it first in `main`, before any thread exists.
pub fn prepare_environment() {
    #[cfg(target_os = "macos")]
    {
        let Some(contents) = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.canonicalize().ok())
            .and_then(|exe| bundle_contents(&exe))
        else {
            return;
        };
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let cache = loader_cache(&contents, &std::env::temp_dir());
        let env = bundle_environment(
            &contents,
            std::env::var_os("XDG_DATA_DIRS"),
            cache,
            home.as_deref(),
        );
        for (name, value) in env {
            // SAFETY: no other thread exists yet (the caller's promise), so
            // nothing reads the environment meanwhile.
            unsafe { std::env::set_var(name, value) };
        }
    }
}

/// Finder used to pass a process serial number (`-psn_…`) to the programs
/// it starts; it is no option of FaderFrame's.
pub fn is_launch_argument(arg: &str) -> bool {
    cfg!(target_os = "macos") && arg.starts_with("-psn_")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bundle_is_found_from_its_executable() {
        let dir = std::env::temp_dir().join(format!("ff-bundle-{}", std::process::id()));
        let contents = dir.join("FaderFrame.app/Contents");
        std::fs::create_dir_all(contents.join("Resources/share")).unwrap();
        std::fs::create_dir_all(contents.join("MacOS")).unwrap();
        let exe = contents.join("MacOS/FaderFrame");
        assert_eq!(bundle_contents(&exe), Some(contents.clone()));
        assert_eq!(
            bundle_contents(&dir.join("faderframe")),
            None,
            "not in a bundle"
        );
        assert_eq!(
            bundle_contents(&dir.join("Other.app/Contents/MacOS/FaderFrame")),
            None,
            "a bundle without resources"
        );

        let loaders = contents.join("Resources/lib/gdk-pixbuf-2.0/2.10.0");
        std::fs::create_dir_all(&loaders).unwrap();
        std::fs::write(
            loaders.join("loaders.cache.in"),
            "\"@RESOURCES@/lib/gdk-pixbuf-2.0/2.10.0/loaders/libpixbufloader-svg.so\"\n",
        )
        .unwrap();
        let cache = loader_cache(&contents, &dir).unwrap();
        let written = std::fs::read_to_string(&cache).unwrap();
        assert!(written.contains(&*contents.join("Resources/lib").to_string_lossy()));
        assert!(!written.contains("@RESOURCES@"));

        let env = bundle_environment(
            &contents,
            None,
            Some(cache.clone()),
            Some(Path::new("/Users/x")),
        );
        let get = |name: &str| {
            env.iter()
                .find(|(n, _)| *n == name)
                .map(|(_, v)| PathBuf::from(v))
        };
        let data: Vec<PathBuf> = std::env::split_paths(&get("XDG_DATA_DIRS").unwrap()).collect();
        assert_eq!(
            data[0],
            contents.join("Resources/share"),
            "the bundle's data first"
        );
        assert_eq!(data.len(), 3, "then the usual folders");
        assert_eq!(get("GDK_PIXBUF_MODULE_FILE"), Some(cache));
        assert_eq!(
            get("GST_PLUGIN_SCANNER_1_0"),
            Some(contents.join("MacOS/gst-plugin-scanner"))
        );
        assert_eq!(
            get("GST_REGISTRY_1_0"),
            Some(PathBuf::from(
                "/Users/x/Library/Caches/faderframe/gstreamer-registry.bin"
            ))
        );
        let inherited = bundle_environment(&contents, Some("/opt/share".into()), None, None);
        let data: Vec<PathBuf> = std::env::split_paths(&inherited[0].1).collect();
        assert_eq!(
            data,
            vec![
                contents.join("Resources/share"),
                PathBuf::from("/opt/share")
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn finder_serial_numbers_are_no_options() {
        assert_eq!(
            is_launch_argument("-psn_0_12345"),
            cfg!(target_os = "macos")
        );
        assert!(!is_launch_argument("--demo"));
    }
}
