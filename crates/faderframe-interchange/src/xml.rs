//! A small XML writer: elements built as a tree, written indented with
//! attributes and text escaped (characters XML 1.0 cannot carry left out).

use std::fmt::Write;

/// An element with its attributes (in the order given) and children.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Element {
    pub name: String,
    pub attrs: Vec<(String, String)>,
    pub children: Vec<Node>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    Element(Element),
    Text(String),
}

impl Element {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Self::default()
        }
    }

    /// With attribute `key`.
    pub fn attr(mut self, key: &str, value: impl std::fmt::Display) -> Self {
        self.set(key, value);
        self
    }

    /// With attribute `key` when there is a value.
    pub fn attr_opt<T: std::fmt::Display>(mut self, key: &str, value: Option<T>) -> Self {
        if let Some(v) = value {
            self.set(key, v);
        }
        self
    }

    /// Set attribute `key` (replacing one of that name).
    pub fn set(&mut self, key: &str, value: impl std::fmt::Display) {
        let v = value.to_string();
        match self.attrs.iter_mut().find(|(k, _)| k == key) {
            Some(a) => a.1 = v,
            None => self.attrs.push((key.to_string(), v)),
        }
    }

    pub fn child(mut self, e: Element) -> Self {
        self.push(e);
        self
    }

    pub fn child_opt(mut self, e: Option<Element>) -> Self {
        if let Some(e) = e {
            self.push(e);
        }
        self
    }

    pub fn push(&mut self, e: Element) {
        self.children.push(Node::Element(e));
    }

    pub fn text(mut self, t: impl Into<String>) -> Self {
        self.children.push(Node::Text(t.into()));
        self
    }

    fn write(&self, out: &mut String, depth: usize) {
        for _ in 0..depth {
            out.push_str("  ");
        }
        out.push('<');
        out.push_str(&self.name);
        for (k, v) in &self.attrs {
            let _ = write!(out, " {k}=\"{}\"", escape(v, true));
        }
        if self.children.is_empty() {
            out.push_str("/>\n");
            return;
        }
        out.push('>');
        // Text content stays on the line (no whitespace added to it).
        if self.children.iter().all(|c| matches!(c, Node::Text(_))) {
            for c in &self.children {
                if let Node::Text(t) = c {
                    out.push_str(&escape(t, false));
                }
            }
        } else {
            out.push('\n');
            for c in &self.children {
                match c {
                    Node::Element(e) => e.write(out, depth + 1),
                    Node::Text(t) => {
                        for _ in 0..=depth {
                            out.push_str("  ");
                        }
                        out.push_str(&escape(t, false));
                        out.push('\n');
                    }
                }
            }
            for _ in 0..depth {
                out.push_str("  ");
            }
        }
        out.push_str("</");
        out.push_str(&self.name);
        out.push_str(">\n");
    }
}

/// A document of `root` (UTF-8, standalone).
pub fn document(root: &Element) -> String {
    let mut out = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n");
    root.write(&mut out, 0);
    out
}

/// Can XML 1.0 carry `c`?
fn allowed(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\r' | '\u{20}'..='\u{D7FF}' | '\u{E000}'..='\u{FFFD}' | '\u{10000}'..)
}

/// `s` escaped for text, or for an attribute (`attr`: quotes and white
/// space as references, so they survive normalisation).
pub fn escape(s: &str, attr: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars().filter(|c| allowed(*c)) {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' if attr => out.push_str("&quot;"),
            '\t' if attr => out.push_str("&#9;"),
            '\n' if attr => out.push_str("&#10;"),
            '\r' => out.push_str("&#13;"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn documents_escape_and_nest() {
        let root = Element::new("Project")
            .attr("version", "1.0")
            .child(Element::new("Title").text("A & B <c>"))
            .child(
                Element::new("Track")
                    .attr("name", "Say \"hi\"\n\u{1}")
                    .attr_opt::<i32>("missing", None),
            );
        let x = document(&root);
        assert!(x.starts_with("<?xml version=\"1.0\""));
        assert!(x.contains("<Title>A &amp; B &lt;c&gt;</Title>"));
        assert!(x.contains("name=\"Say &quot;hi&quot;&#10;\""));
        assert!(!x.contains("missing"));
        let doc = roxmltree::Document::parse(&x).unwrap();
        let t = doc.descendants().find(|n| n.has_tag_name("Track")).unwrap();
        assert_eq!(t.attribute("name"), Some("Say \"hi\"\n"));
    }
}
