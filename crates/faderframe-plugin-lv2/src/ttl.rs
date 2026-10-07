//! LV2's metadata: Turtle files read into one graph (`oxttl`), with the
//! few queries the host needs. IRIs of files are `file://` URLs; relative
//! references resolve against the file they are in.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
pub const RDFS_SEE_ALSO: &str = "http://www.w3.org/2000/01/rdf-schema#seeAlso";
pub const RDFS_LABEL: &str = "http://www.w3.org/2000/01/rdf-schema#label";
pub const DOAP_NAME: &str = "http://usefulinc.com/ns/doap#name";
pub const LV2: &str = "http://lv2plug.in/ns/lv2core#";
pub const UI: &str = "http://lv2plug.in/ns/extensions/ui#";
pub const ATOM: &str = "http://lv2plug.in/ns/ext/atom#";
pub const PSET: &str = "http://lv2plug.in/ns/ext/presets#";
pub const STATE: &str = "http://lv2plug.in/ns/ext/state#";

/// A node of the graph.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Node {
    Iri(String),
    Blank(String),
    Literal {
        value: String,
        datatype: String,
        language: Option<String>,
    },
}

impl Node {
    pub fn iri(&self) -> Option<&str> {
        match self {
            Node::Iri(s) => Some(s),
            _ => None,
        }
    }

    pub fn literal(&self) -> Option<&str> {
        match self {
            Node::Literal { value, .. } => Some(value),
            _ => None,
        }
    }

    /// A number (an integer, decimal, double or boolean literal).
    pub fn number(&self) -> Option<f64> {
        match self {
            Node::Literal {
                value, datatype, ..
            } => {
                if datatype.ends_with("#boolean") {
                    return Some(if value == "true" || value == "1" {
                        1.0
                    } else {
                        0.0
                    });
                }
                value.trim().parse::<f64>().ok()
            }
            _ => None,
        }
    }
}

/// Triples, indexed by subject.
#[derive(Default)]
pub struct Graph {
    by_subject: HashMap<Node, Vec<(String, Node)>>,
    /// Files read (to read each once).
    files: Vec<PathBuf>,
}

/// `path` as a `file://` URL (bytes outside the unreserved set and `/`
/// percent-encoded).
pub fn file_url(path: &Path) -> String {
    let mut out = String::from("file://");
    for b in path.to_string_lossy().bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The path of a `file://` URL.
pub fn url_path(url: &str) -> Option<PathBuf> {
    let rest = url.strip_prefix("file://")?;
    // `file://localhost/…` or `file:///…`.
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(v) = bytes
                .get(i + 1..i + 3)
                .and_then(|h| std::str::from_utf8(h).ok())
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    Some(PathBuf::from(String::from_utf8_lossy(&out).into_owned()))
}

impl Graph {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add `path`'s triples (once).
    pub fn load(&mut self, path: &Path) -> Result<(), String> {
        let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        if self.files.contains(&path) {
            return Ok(());
        }
        self.files.push(path.clone());
        let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        self.load_bytes(&bytes, &file_url(&path))
            .map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Add Turtle text with `base` as its base IRI.
    pub fn load_bytes(&mut self, bytes: &[u8], base: &str) -> Result<(), String> {
        let parser = oxttl::TurtleParser::new()
            .with_base_iri(base)
            .map_err(|e| e.to_string())?
            .lenient();
        for triple in parser.for_slice(bytes) {
            let t = triple.map_err(|e| e.to_string())?;
            let subject = match t.subject {
                oxrdf::NamedOrBlankNode::NamedNode(n) => Node::Iri(n.into_string()),
                oxrdf::NamedOrBlankNode::BlankNode(b) => Node::Blank(b.into_string()),
            };
            let object = match t.object {
                oxrdf::Term::NamedNode(n) => Node::Iri(n.into_string()),
                oxrdf::Term::BlankNode(b) => Node::Blank(b.into_string()),
                oxrdf::Term::Literal(l) => {
                    let (value, datatype, language) = (
                        l.value().to_string(),
                        l.datatype().as_str().to_string(),
                        l.language().map(str::to_string),
                    );
                    Node::Literal {
                        value,
                        datatype,
                        language,
                    }
                }
                #[allow(unreachable_patterns)]
                _ => continue,
            };
            self.by_subject
                .entry(subject)
                .or_default()
                .push((t.predicate.into_string(), object));
        }
        Ok(())
    }

    /// Every object of `(subject, predicate, ·)`.
    pub fn objects<'a>(
        &'a self,
        subject: &Node,
        predicate: &str,
    ) -> impl Iterator<Item = &'a Node> + use<'a> {
        let predicate = predicate.to_string();
        self.by_subject
            .get(subject)
            .into_iter()
            .flatten()
            .filter(move |(p, _)| *p == predicate)
            .map(|(_, o)| o)
    }

    /// Every (predicate, object) of `subject`.
    pub fn pairs<'a>(
        &'a self,
        subject: &Node,
    ) -> impl Iterator<Item = (&'a str, &'a Node)> + use<'a> {
        self.by_subject
            .get(subject)
            .into_iter()
            .flatten()
            .map(|(p, o)| (p.as_str(), o))
    }

    pub fn object<'a>(&'a self, subject: &Node, predicate: &str) -> Option<&'a Node> {
        self.objects(subject, predicate).next()
    }

    /// Whether `(subject, predicate, <iri>)` holds.
    pub fn has(&self, subject: &Node, predicate: &str, iri: &str) -> bool {
        self.objects(subject, predicate)
            .any(|o| o.iri() == Some(iri))
    }

    /// Subjects of type `class`.
    pub fn of_type(&self, class: &str) -> Vec<Node> {
        let mut out: Vec<Node> = self
            .by_subject
            .iter()
            .filter(|(_, ps)| {
                ps.iter()
                    .any(|(p, o)| p == RDF_TYPE && o.iri() == Some(class))
            })
            .map(|(s, _)| s.clone())
            .collect();
        out.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
        out
    }

    /// A text: untagged first, then English, then any.
    pub fn text(&self, subject: &Node, predicate: &str) -> Option<String> {
        let all: Vec<&Node> = self.objects(subject, predicate).collect();
        let pick = |want: &dyn Fn(&Option<String>) -> bool| {
            all.iter().find_map(|n| match n {
                Node::Literal {
                    value, language, ..
                } if want(language) => Some(value.clone()),
                _ => None,
            })
        };
        pick(&|l| l.is_none())
            .or_else(|| pick(&|l| l.as_deref().is_some_and(|l| l.starts_with("en"))))
            .or_else(|| pick(&|_| true))
    }

    pub fn number(&self, subject: &Node, predicate: &str) -> Option<f64> {
        self.objects(subject, predicate).find_map(Node::number)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turtle_reads_with_relative_iris_and_languages() {
        let mut g = Graph::new();
        g.load_bytes(
            r#"@prefix lv2: <http://lv2plug.in/ns/lv2core#> .
@prefix doap: <http://usefulinc.com/ns/doap#> .
<http://example.org/amp> a lv2:Plugin ;
    lv2:binary <amp.so> ;
    doap:name "Verstärker"@de , "Amp" ;
    lv2:port [ lv2:index 0 ; lv2:default 0.5 ; lv2:toggled true ] ."#
                .as_bytes(),
            "file:///lv2/My%20Amp.lv2/manifest.ttl",
        )
        .unwrap();
        let p = Node::Iri("http://example.org/amp".into());
        assert_eq!(g.of_type(&format!("{LV2}Plugin")), std::slice::from_ref(&p));
        let binary = g
            .object(&p, &format!("{LV2}binary"))
            .unwrap()
            .iri()
            .unwrap();
        assert_eq!(binary, "file:///lv2/My%20Amp.lv2/amp.so");
        assert_eq!(
            url_path(binary).unwrap(),
            PathBuf::from("/lv2/My Amp.lv2/amp.so")
        );
        assert_eq!(g.text(&p, DOAP_NAME).as_deref(), Some("Amp"));
        let port = g.object(&p, &format!("{LV2}port")).unwrap().clone();
        assert_eq!(g.number(&port, &format!("{LV2}default")), Some(0.5));
        assert_eq!(g.number(&port, &format!("{LV2}toggled")), Some(1.0));
        assert_eq!(
            file_url(Path::new("/a b/c")),
            "file:///a%20b/c",
            "spaces are encoded"
        );
    }
}
