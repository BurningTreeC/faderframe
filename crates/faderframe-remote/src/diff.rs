//! State diffs: what changed between two JSON states as a list of
//! operations, each a path (object keys and array indices) with the value
//! set there, or a removal. Objects are compared key by key, arrays of the
//! same length element by element; an array that changed length is sent
//! whole. The web app applies them in order (`app.js`'s `applyPatch`),
//! as [`apply`] does here.

use serde::Serialize;
use serde_json::{Map, Value};

/// One step of a path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum Key {
    Field(String),
    Index(usize),
}

/// One change.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Op {
    /// Where (from the state's root).
    pub p: Vec<Key>,
    /// The value set there (absent: removed).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub v: Option<Value>,
    /// Removed (an object's key).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub d: bool,
}

/// What turns `old` into `new`.
pub fn diff(old: &Value, new: &Value) -> Vec<Op> {
    let mut ops = Vec::new();
    let mut path = Vec::new();
    walk(old, new, &mut path, &mut ops);
    ops
}

fn walk(old: &Value, new: &Value, path: &mut Vec<Key>, ops: &mut Vec<Op>) {
    match (old, new) {
        (Value::Object(a), Value::Object(b)) => objects(a, b, path, ops),
        (Value::Array(a), Value::Array(b)) if a.len() == b.len() => {
            for (i, (x, y)) in a.iter().zip(b).enumerate() {
                if x != y {
                    path.push(Key::Index(i));
                    walk(x, y, path, ops);
                    path.pop();
                }
            }
        }
        (a, b) if a == b => {}
        (_, b) => ops.push(Op {
            p: path.clone(),
            v: Some(b.clone()),
            d: false,
        }),
    }
}

fn objects(a: &Map<String, Value>, b: &Map<String, Value>, path: &mut Vec<Key>, ops: &mut Vec<Op>) {
    for (k, x) in a {
        match b.get(k) {
            Some(y) if x == y => {}
            Some(y) => {
                path.push(Key::Field(k.clone()));
                walk(x, y, path, ops);
                path.pop();
            }
            None => {
                let mut p = path.clone();
                p.push(Key::Field(k.clone()));
                ops.push(Op {
                    p,
                    v: None,
                    d: true,
                });
            }
        }
    }
    for (k, y) in b {
        if !a.contains_key(k) {
            let mut p = path.clone();
            p.push(Key::Field(k.clone()));
            ops.push(Op {
                p,
                v: Some(y.clone()),
                d: false,
            });
        }
    }
}

/// Apply `ops` to `state` (as the web app does). A path that does not
/// lead anywhere is skipped.
pub fn apply(state: &mut Value, ops: &[Op]) {
    for op in ops {
        let Some((last, parents)) = op.p.split_last() else {
            if let Some(v) = &op.v {
                *state = v.clone();
            }
            continue;
        };
        let Some(at) = walk_to(state, parents) else {
            continue;
        };
        match (at, last, &op.v) {
            (Value::Object(m), Key::Field(f), _) if op.d => {
                m.remove(f);
            }
            (Value::Object(m), Key::Field(f), Some(v)) => {
                m.insert(f.clone(), v.clone());
            }
            (Value::Array(a), Key::Index(i), Some(v)) if *i < a.len() => a[*i] = v.clone(),
            _ => {}
        }
    }
}

/// The value at `path`, if the path leads anywhere.
fn walk_to<'a>(state: &'a mut Value, path: &[Key]) -> Option<&'a mut Value> {
    let mut at = state;
    for k in path {
        at = match (at, k) {
            (Value::Object(m), Key::Field(f)) => m.get_mut(f)?,
            (Value::Array(a), Key::Index(i)) => a.get_mut(*i)?,
            _ => return None,
        };
    }
    Some(at)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn round(a: Value, b: Value) -> Vec<Op> {
        let ops = diff(&a, &b);
        let mut c = a.clone();
        apply(&mut c, &ops);
        assert_eq!(c, b, "{ops:?}");
        ops
    }

    #[test]
    fn equal_states_need_nothing() {
        let s = json!({"a": 1, "b": [1, 2, {"c": null}]});
        assert!(diff(&s, &s).is_empty());
    }

    #[test]
    fn only_what_changed_is_sent() {
        let a = json!({
            "transport": {"playing": false, "bars": "1.1.1"},
            "tracks": {"12": {"name": "Bass", "fader": 0.7, "mute": false},
                       "14": {"name": "Drums", "fader": 0.5, "mute": true}},
            "order": [12, 14],
            "lyrics": []
        });
        let b = json!({
            "transport": {"playing": true, "bars": "1.1.1"},
            "tracks": {"12": {"name": "Bass", "fader": 0.75, "mute": false},
                       "15": {"name": "Keys", "fader": 0.5, "mute": false}},
            "order": [12, 15],
            "lyrics": [{"text": "la"}]
        });
        let ops = round(a, b);
        let text = serde_json::to_string(&ops).unwrap();
        assert!(
            text.contains(r#"{"p":["transport","playing"],"v":true}"#),
            "{text}"
        );
        assert!(
            text.contains(r#"{"p":["tracks","12","fader"],"v":0.75}"#),
            "{text}"
        );
        assert!(text.contains(r#"{"p":["tracks","14"],"d":true}"#), "{text}");
        assert!(text.contains(r#"{"p":["order",1],"v":15}"#), "{text}");
        // A list that grew is sent whole.
        assert!(
            text.contains(r#"{"p":["lyrics"],"v":[{"text":"la"}]}"#),
            "{text}"
        );
        assert!(!text.contains("Bass"), "{text}");
    }

    #[test]
    fn nulls_and_kinds_change_like_values() {
        round(json!({"a": null}), json!({"a": 3}));
        round(json!({"a": 3}), json!({"a": null}));
        round(json!({"a": {"b": 1}}), json!({"a": [1]}));
        round(json!([1, {"x": 1}]), json!([1, {"x": 2, "y": 3}]));
        round(json!(1), json!("one"));
        // A patch that lost its way is skipped, not a panic.
        let mut s = json!({"a": 1});
        apply(
            &mut s,
            &[Op {
                p: vec![Key::Field("x".into()), Key::Field("y".into())],
                v: Some(json!(1)),
                d: false,
            }],
        );
        assert_eq!(s, json!({"a": 1}));
    }
}
