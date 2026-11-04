//! The web app: plain HTML, CSS and JavaScript, built into the program (no
//! build step, nothing fetched from elsewhere).

/// A file the server serves: its type and bytes.
pub struct Asset {
    pub content_type: &'static str,
    pub body: &'static [u8],
}

const INDEX: &str = include_str!("../web/index.html");
const APP_JS: &str = include_str!("../web/app.js");
const APP_CSS: &str = include_str!("../web/app.css");
const ICON: &str = include_str!("../web/icon.svg");
const MANIFEST: &str = include_str!("../web/manifest.webmanifest");

/// The page's content security policy: its own files only, the WebSocket
/// back to where it came from.
pub const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self'; \
                       manifest-src 'self'; connect-src 'self' ws: wss:; base-uri 'none'; \
                       form-action 'none'; frame-ancestors 'none'";

/// The file at `path`, if there is one.
pub fn asset(path: &str) -> Option<Asset> {
    let (content_type, body) = match path {
        "/" | "/index.html" => ("text/html; charset=utf-8", INDEX),
        "/app.js" => ("text/javascript; charset=utf-8", APP_JS),
        "/app.css" => ("text/css; charset=utf-8", APP_CSS),
        "/icon.svg" | "/favicon.ico" => ("image/svg+xml", ICON),
        "/manifest.webmanifest" => ("application/manifest+json", MANIFEST),
        _ => return None,
    };
    Some(Asset {
        content_type,
        body: body.as_bytes(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_app_is_built_in_and_names_only_its_own_files() {
        let index = std::str::from_utf8(asset("/").unwrap().body).unwrap();
        assert!(index.contains("app.js"));
        assert!(index.contains("app.css"));
        // Nothing from elsewhere: no scheme in any reference.
        for file in ["/", "/app.js", "/app.css", "/manifest.webmanifest"] {
            let text = std::str::from_utf8(asset(file).unwrap().body).unwrap();
            assert!(!text.contains("https://"), "{file}");
            assert!(!text.contains("http://"), "{file}");
        }
        assert!(asset("/../secret").is_none());
        assert!(
            asset("/app.js")
                .unwrap()
                .content_type
                .starts_with("text/javascript")
        );
    }

    #[test]
    fn the_script_handles_every_message_the_server_sends() {
        let js = std::str::from_utf8(asset("/app.js").unwrap().body).unwrap();
        for t in [
            "paired", "welcome", "refused", "state", "patch", "meters", "notice", "bye",
        ] {
            assert!(js.contains(&format!("case '{t}'")), "{t}");
        }
        // Text from the project goes in as text, never as markup.
        assert!(!js.contains("innerHTML"));
    }
}
