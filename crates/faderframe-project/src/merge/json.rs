//! The three-way merge on the project's JSON form (see [`super`]).
//!
//! Values are merged where they sit: objects key by key, lists of things
//! with ids by id (with their order merged too), lists of things placed in
//! time by the stretches each side changed, everything else whole. What
//! must not be combined from two sides is taken whole: a changed enum
//! variant, a replaced thing (another id), a range (start and end move
//! together), a plug-in's settings (its state blob and its values), take
//! folders and the clip edits whose parts depend on each other.

use super::{ConflictKind, Resolution};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// A step on the way to a value.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Seg {
    /// A field of an object.
    Field(String),
    /// A thing in a list or map, by its key (id).
    Key(String),
}

impl Seg {
    pub(crate) fn text(&self) -> &str {
        match self {
            Seg::Field(s) | Seg::Key(s) => s,
        }
    }
}

/// A path's text: its steps joined by `/`.
pub(crate) fn path_key(path: &[Seg]) -> String {
    path.iter().map(Seg::text).collect::<Vec<_>>().join("/")
}

/// What a conflict is resolved as a whole: a thing with an id (a track, a
/// clip, a note, a plug-in…), else a field of the project.
#[derive(Clone, Debug)]
pub(crate) struct Unit {
    pub key: String,
    pub path: Vec<Seg>,
    /// Keeping both makes sense (a copy of theirs beside ours).
    pub both: bool,
}

impl Unit {
    fn new(path: &[Seg], both: bool) -> Self {
        Self {
            key: path_key(path),
            path: path.to_vec(),
            both,
        }
    }
}

/// A value as a conflict shows it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Brief {
    Absent,
    Scalar(Value),
    /// A structure or a long text (a plug-in's state).
    Complex,
}

pub(crate) fn brief(v: Option<&Value>) -> Brief {
    match v {
        None => Brief::Absent,
        Some(Value::String(s)) if s.len() > 80 => Brief::Complex,
        Some(v @ (Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_))) => {
            Brief::Scalar(v.clone())
        }
        Some(_) => Brief::Complex,
    }
}

/// One conflicting value.
#[derive(Clone, Debug)]
pub(crate) struct Record {
    pub unit: Unit,
    pub kind: ConflictKind,
    pub at: Vec<Seg>,
    pub ours: Brief,
    pub theirs: Brief,
    /// The stretch both changed (lists placed in time).
    pub span: Option<(i64, i64)>,
}

/// Fields always taken whole.
const WHOLE: &[&str] = &[
    "warp",
    "pitch",
    "spectral",
    "effects",
    "stash",
    "plugin",
    "signature",
    "color",
    "key",
    "chord",
    "tuning",
    "layout",
    "freeze",
    "timecode",
    "mapping",
    "source",
    "target",
];

/// Lists whose things are parts of the thing holding them (their
/// conflicts are its own).
const PARTS: &[&str] = &["parameters"];

/// Enum variants (`"type"`) always taken whole: take folders (their comp
/// names takes by place).
const WHOLE_TYPES: &[&str] = &["takes"];

/// Fields naming a place in time (lists of things placed by them merge by
/// stretches).
const PLACES: &[&str] = &["time", "position", "at", "bar", "start"];

/// Fields keying lists of things.
const KEYS: &[&[&str]] = &[
    &["id"],
    &["note"],
    &["controller", "channel"],
    &["track", "scene"],
];

/// Lists whose things may be kept twice ("both").
const BOTH: &[&str] = &["clips", "notes", "markers", "sections"];

pub(crate) struct Merger<'a> {
    pub resolutions: &'a BTreeMap<String, Resolution>,
    pub records: Vec<Record>,
}

impl<'a> Merger<'a> {
    pub fn new(resolutions: &'a BTreeMap<String, Resolution>) -> Self {
        Self {
            resolutions,
            records: Vec::new(),
        }
    }

    /// The side a unit's conflicts take (ours unless chosen; "both" keeps
    /// ours in place, theirs is copied beside it afterwards).
    fn takes_theirs(&self, unit: &Unit) -> bool {
        self.resolutions.get(&unit.key) == Some(&Resolution::Theirs)
    }

    fn record(
        &mut self,
        unit: &Unit,
        kind: ConflictKind,
        at: &[Seg],
        o: Option<&Value>,
        t: Option<&Value>,
        span: Option<(i64, i64)>,
    ) {
        self.records.push(Record {
            unit: unit.clone(),
            kind,
            at: at.to_vec(),
            ours: brief(o),
            theirs: brief(t),
            span,
        });
    }

    /// A conflict over a value taken whole: recorded, resolved.
    fn conflict(
        &mut self,
        path: &[Seg],
        unit: &Unit,
        kind: ConflictKind,
        o: Option<&Value>,
        t: Option<&Value>,
    ) -> Option<Value> {
        self.record(unit, kind, path, o, t, None);
        if self.takes_theirs(unit) {
            t.cloned()
        } else {
            o.cloned()
        }
    }

    /// The project: each top-level field its own unit unless something
    /// with an id inside is.
    pub fn merge_project(&mut self, b: &Value, o: &Value, t: &Value) -> Value {
        let empty = Map::new();
        let (bm, om, tm) = (
            b.as_object().unwrap_or(&empty),
            o.as_object().unwrap_or(&empty),
            t.as_object().unwrap_or(&empty),
        );
        let keys: BTreeSet<&String> = bm.keys().chain(om.keys()).chain(tm.keys()).collect();
        let mut out = Map::new();
        for k in keys {
            let mut path = vec![Seg::Field(k.clone())];
            let unit = Unit::new(&path, false);
            let v = if k == "ids" {
                // The allocator is set past both sides afterwards.
                om.get(k).cloned()
            } else {
                self.merge(&mut path, &unit, bm.get(k), om.get(k), tm.get(k))
            };
            if let Some(v) = v {
                out.insert(k.clone(), v);
            }
        }
        Value::Object(out)
    }

    pub fn merge(
        &mut self,
        path: &mut Vec<Seg>,
        unit: &Unit,
        b: Option<&Value>,
        o: Option<&Value>,
        t: Option<&Value>,
    ) -> Option<Value> {
        if o == t {
            return o.cloned();
        }
        if b == o {
            return t.cloned();
        }
        if b == t {
            return o.cloned();
        }
        // Both changed it, each its own way.
        let field = path.last().map_or("", Seg::text).to_string();
        let derived = field == "clips" && path.len() == 3 && path[0] == Seg::Field("tracks".into());
        if derived {
            // A track's clip list follows the clips (made again after).
            return o.cloned();
        }
        let (Some(ov), Some(tv)) = (o, t) else {
            let kind = if o.is_none() {
                ConflictKind::RemovedHere
            } else {
                ConflictKind::RemovedThere
            };
            return self.conflict(path, unit, kind, o, t);
        };
        if matches!(path.last(), Some(Seg::Field(f)) if WHOLE.contains(&f.as_str())) {
            return self.conflict(path, unit, ConflictKind::BothChanged, o, t);
        }
        match (b, ov, tv) {
            (_, Value::Object(om), Value::Object(tm))
                if b.is_none_or(Value::is_object)
                    && combinable(b.and_then(Value::as_object), om, tm) =>
            {
                Some(self.merge_object(path, unit, b.and_then(Value::as_object), om, tm))
            }
            (_, Value::Array(oa), Value::Array(ta)) if b.is_none_or(Value::is_array) => {
                let empty = Vec::new();
                let ba = b.and_then(Value::as_array).unwrap_or(&empty);
                Some(self.merge_array(path, unit, ba, oa, ta))
            }
            _ => self.conflict(path, unit, ConflictKind::BothChanged, o, t),
        }
    }

    fn merge_object(
        &mut self,
        path: &mut Vec<Seg>,
        unit: &Unit,
        b: Option<&Map<String, Value>>,
        o: &Map<String, Value>,
        t: &Map<String, Value>,
    ) -> Value {
        let empty = Map::new();
        let bm = b.unwrap_or(&empty);
        let keys: BTreeSet<&String> = bm.keys().chain(o.keys()).chain(t.keys()).collect();
        // A map by id (numeric keys): every entry its own unit.
        let by_id = !keys.is_empty() && keys.iter().all(|k| k.parse::<u64>().is_ok());
        let mut out = Map::new();
        let mut done: BTreeSet<&str> = BTreeSet::new();
        // A plug-in's settings move together when either side loaded a new
        // state.
        if is_plugin_slot(o) && is_plugin_slot(t) {
            let settings = |m: &Map<String, Value>| {
                Value::Array(vec![
                    m.get("state").cloned().unwrap_or(Value::Null),
                    m.get("parameters").cloned().unwrap_or(Value::Null),
                ])
            };
            let (sb, so, st) = (settings(bm), settings(o), settings(t));
            let new_state = so[0] != sb[0] || st[0] != sb[0];
            if so != sb && st != sb && so != st && new_state {
                path.push(Seg::Field("settings".into()));
                self.record(
                    unit,
                    ConflictKind::BothChanged,
                    path,
                    Some(&so),
                    Some(&st),
                    None,
                );
                path.pop();
                let from = if self.takes_theirs(unit) { t } else { o };
                for k in ["state", "parameters"] {
                    done.insert(k);
                    if let Some(v) = from.get(k) {
                        out.insert(k.into(), v.clone());
                    }
                }
            }
        }
        for k in keys {
            if done.contains(k.as_str()) {
                continue;
            }
            if by_id {
                path.push(Seg::Key(k.clone()));
                let child = Unit::new(path, both_for(path));
                let v = self.merge(path, &child, bm.get(k), o.get(k), t.get(k));
                path.pop();
                if let Some(v) = v {
                    out.insert(k.clone(), v);
                }
            } else {
                path.push(Seg::Field(k.clone()));
                let v = self.merge(path, unit, bm.get(k), o.get(k), t.get(k));
                path.pop();
                if let Some(v) = v {
                    out.insert(k.clone(), v);
                }
            }
        }
        Value::Object(out)
    }

    fn merge_array(
        &mut self,
        path: &mut Vec<Seg>,
        unit: &Unit,
        b: &[Value],
        o: &[Value],
        t: &[Value],
    ) -> Value {
        if let Some(fields) = keyed_by(b, o, t) {
            return Value::Array(self.merge_keyed(path, unit, fields, b, o, t));
        }
        if let Some(field) = placed_by(b, o, t) {
            return Value::Array(self.merge_placed(path, unit, field, b, o, t));
        }
        let (ov, tv) = (Value::Array(o.to_vec()), Value::Array(t.to_vec()));
        self.conflict(path, unit, ConflictKind::BothChanged, Some(&ov), Some(&tv))
            .unwrap_or(ov)
    }

    /// A list of things with ids: each merged as itself, the order merged.
    fn merge_keyed(
        &mut self,
        path: &mut Vec<Seg>,
        unit: &Unit,
        fields: &[&str],
        b: &[Value],
        o: &[Value],
        t: &[Value],
    ) -> Vec<Value> {
        let index = |list: &'_ [Value]| -> Vec<(String, Value)> {
            list.iter()
                .filter_map(|v| Some((key_of(v, fields)?, v.clone())))
                .collect()
        };

        let (bi, oi, ti) = (index(b), index(o), index(t));
        let map = |l: &'_ [(String, Value)]| -> HashMap<String, Value> {
            l.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
        };
        let (bmap, omap, tmap) = (map(&bi), map(&oi), map(&ti));
        let mut merged: HashMap<String, Value> = HashMap::new();
        let all: BTreeSet<&String> = bmap.keys().chain(omap.keys()).chain(tmap.keys()).collect();
        let parts = matches!(path.last(), Some(Seg::Field(f)) if PARTS.contains(&f.as_str()));
        for k in all {
            path.push(Seg::Key(k.clone()));
            let child = if parts {
                unit.clone()
            } else {
                Unit::new(path, both_for(path))
            };
            let v = self.merge(path, &child, bmap.get(k), omap.get(k), tmap.get(k));
            path.pop();
            if let Some(v) = v {
                merged.insert(k.clone(), v);
            }
        }
        let keep: BTreeSet<String> = merged.keys().cloned().collect();
        let keys =
            |l: &[(String, Value)]| -> Vec<String> { l.iter().map(|(k, _)| k.clone()).collect() };
        let order = self.merge_order(path, unit, &keys(&bi), &keys(&oi), &keys(&ti), &keep);
        order
            .into_iter()
            .filter_map(|k| merged.remove(&k))
            .collect()
    }

    /// The order of a list of things with ids: one side's reordering (or
    /// neither's) taken with the other's additions in place; both
    /// reordering it their own ways is a conflict.
    fn merge_order(
        &mut self,
        path: &[Seg],
        _unit: &Unit,
        b: &[String],
        o: &[String],
        t: &[String],
        keep: &BTreeSet<String>,
    ) -> Vec<String> {
        let (bs, os, ts): (BTreeSet<&String>, BTreeSet<&String>, BTreeSet<&String>) =
            (b.iter().collect(), o.iter().collect(), t.iter().collect());
        let common = |l: &[String]| -> Vec<String> {
            l.iter()
                .filter(|k| bs.contains(k) && os.contains(k) && ts.contains(k) && keep.contains(*k))
                .cloned()
                .collect()
        };
        let (bo, oo, to) = (common(b), common(o), common(t));
        let theirs_first = if to == bo {
            false
        } else if oo == bo {
            true
        } else {
            let mut at = path.to_vec();
            at.push(Seg::Field("order".into()));
            let unit = Unit {
                key: format!("{}#order", path_key(path)),
                path: at.clone(),
                both: false,
            };
            let ov = Value::Array(oo.iter().map(|k| Value::String(k.clone())).collect());
            let tv = Value::Array(to.iter().map(|k| Value::String(k.clone())).collect());
            self.record(&unit, ConflictKind::Order, &at, Some(&ov), Some(&tv), None);
            self.takes_theirs(&unit)
        };
        let (first, second) = if theirs_first { (t, o) } else { (o, t) };
        let mut out: Vec<String> = first
            .iter()
            .filter(|k| keep.contains(*k))
            .cloned()
            .collect();
        for (i, k) in second.iter().enumerate() {
            if !keep.contains(k) || out.contains(k) {
                continue;
            }
            // After what comes before it there, else before what comes
            // after it there, else at the end.
            let after = second[..i]
                .iter()
                .rev()
                .find_map(|p| out.iter().position(|x| x == p));
            let before = || {
                second[i + 1..]
                    .iter()
                    .find_map(|n| out.iter().position(|x| x == n))
            };
            match (after, before()) {
                (Some(p), _) => out.insert(p + 1, k.clone()),
                (None, Some(n)) => out.insert(n, k.clone()),
                (None, None) => out.push(k.clone()),
            }
        }
        for k in keep {
            if !out.contains(k) {
                out.push(k.clone());
            }
        }
        out
    }

    /// A list of things placed in time (automation points, tempo changes,
    /// chords…): the stretches each side changed come from that side;
    /// where both changed the same stretch, the unit's choice.
    fn merge_placed(
        &mut self,
        path: &[Seg],
        unit: &Unit,
        field: &str,
        b: &[Value],
        o: &[Value],
        t: &[Value],
    ) -> Vec<Value> {
        let extent = |v: &Value| -> (i64, i64) {
            let start = v.get(field).and_then(Value::as_i64).unwrap_or(0);
            let end = v
                .get("end")
                .and_then(Value::as_i64)
                .or_else(|| v.get("length").and_then(Value::as_i64).map(|l| start + l))
                .unwrap_or(start)
                .max(start);
            (start, end)
        };
        let unchanged_base = |side: &[Value]| -> (Vec<bool>, Vec<bool>) {
            // Base elements kept as they were, side elements that are such.
            let mut pool: HashMap<String, Vec<usize>> = HashMap::new();
            for (i, v) in side.iter().enumerate() {
                pool.entry(v.to_string()).or_default().push(i);
            }
            let mut base_kept = vec![false; b.len()];
            let mut side_kept = vec![false; side.len()];
            for (i, v) in b.iter().enumerate() {
                if let Some(list) = pool.get_mut(&v.to_string())
                    && let Some(j) = list.pop()
                {
                    base_kept[i] = true;
                    side_kept[j] = true;
                }
            }
            (base_kept, side_kept)
        };
        let (bko, sko) = unchanged_base(o);
        let (bkt, skt) = unchanged_base(t);
        // Places of base elements both kept: changed stretches never join
        // across them.
        let anchors: Vec<i64> = b
            .iter()
            .enumerate()
            .filter(|(i, _)| bko[*i] && bkt[*i])
            .map(|(_, v)| extent(v).0)
            .collect();
        let stretches =
            |side: &[Value], base_kept: &[bool], side_kept: &[bool]| -> Vec<(i64, i64)> {
                let mut spans: Vec<(i64, i64)> = b
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| !base_kept[*i])
                    .map(|(_, v)| extent(v))
                    .chain(
                        side.iter()
                            .enumerate()
                            .filter(|(i, _)| !side_kept[*i])
                            .map(|(_, v)| extent(v)),
                    )
                    .collect();
                spans.sort();
                let mut out: Vec<(i64, i64)> = Vec::new();
                for s in spans {
                    if let Some(last) = out.last_mut() {
                        let between = anchors.iter().any(|a| *a > last.1 && *a < s.0);
                        if s.0 <= last.1 || !between {
                            last.1 = last.1.max(s.1);
                            continue;
                        }
                    }
                    out.push(s);
                }
                out
            };
        let so = stretches(o, &bko, &sko);
        let st = stretches(t, &bkt, &skt);
        // Clusters of overlapping stretches, and who changed each.
        let mut all: Vec<(i64, i64, bool)> = so
            .iter()
            .map(|s| (s.0, s.1, true))
            .chain(st.iter().map(|s| (s.0, s.1, false)))
            .collect();
        all.sort();
        let mut clusters: Vec<(i64, i64, bool, bool)> = Vec::new();
        for (a, z, ours) in all {
            if let Some(c) = clusters.last_mut()
                && a <= c.1
            {
                c.1 = c.1.max(z);
                c.2 |= ours;
                c.3 |= !ours;
                continue;
            }
            clusters.push((a, z, ours, !ours));
        }
        let inside = |v: &Value, c: &(i64, i64, bool, bool)| {
            let s = extent(v).0;
            s >= c.0 && s <= c.1
        };
        let mut out: Vec<(i64, usize, Value)> = Vec::new();
        for (i, v) in b.iter().enumerate() {
            if !clusters.iter().any(|c| inside(v, c)) {
                out.push((extent(v).0, i, v.clone()));
            }
        }
        let mut seq = b.len();
        for c in &clusters {
            let pick = |side: &[Value]| -> Vec<Value> {
                side.iter().filter(|v| inside(v, c)).cloned().collect()
            };
            let from_theirs = match (c.2, c.3) {
                (true, false) => false,
                (false, true) => true,
                _ => {
                    let (ov, tv) = (pick(o), pick(t));
                    if ov == tv {
                        false
                    } else {
                        let (ov, tv) = (Value::Array(ov), Value::Array(tv));
                        self.record(
                            unit,
                            ConflictKind::SamePlace,
                            path,
                            Some(&ov),
                            Some(&tv),
                            Some((c.0, c.1)),
                        );
                        self.takes_theirs(unit)
                    }
                }
            };
            for v in pick(if from_theirs { t } else { o }) {
                out.push((extent(&v).0, seq, v));
                seq += 1;
            }
        }
        out.sort_by_key(|e| (e.0, e.1));
        out.into_iter().map(|e| e.2).collect()
    }
}

/// Can the three objects be merged field by field, or must the value be
/// taken whole (another enum variant, another thing, a range)?
fn combinable(
    b: Option<&Map<String, Value>>,
    o: &Map<String, Value>,
    t: &Map<String, Value>,
) -> bool {
    let present: Vec<&Map<String, Value>> = b.into_iter().chain([o, t]).collect();
    // The same variant of an internally tagged enum, not one taken whole.
    let tag = |m: &Map<String, Value>| m.get("type").cloned();
    if present.iter().any(|m| tag(m) != tag(o))
        || tag(o).is_some_and(|t| t.as_str().is_some_and(|t| WHOLE_TYPES.contains(&t)))
    {
        return false;
    }
    // An externally tagged variant: one key each, the same.
    if present.iter().all(|m| m.len() == 1) {
        let first = o.keys().next();
        if present.iter().any(|m| m.keys().next() != first) {
            return false;
        }
    }
    // The same thing (a replaced one is another).
    let id = |m: &Map<String, Value>| m.get("id").cloned();
    if present.iter().any(|m| id(m) != id(o)) {
        return false;
    }
    // A range moves as one.
    !present
        .iter()
        .any(|m| m.contains_key("start") && m.contains_key("end"))
}

fn is_plugin_slot(m: &Map<String, Value>) -> bool {
    m.contains_key("plugin") && m.contains_key("id")
}

/// Keeping both copies makes sense for things in these lists.
fn both_for(path: &[Seg]) -> bool {
    let n = path.len();
    n >= 2
        && matches!(&path[n - 1], Seg::Key(_))
        && matches!(&path[n - 2], Seg::Field(f) if BOTH.contains(&f.as_str()))
}

/// A list element's key (`None`: it has none).
fn key_of(v: &Value, fields: &[&str]) -> Option<String> {
    let m = v.as_object()?;
    let mut parts = Vec::with_capacity(fields.len());
    for f in fields {
        let part = m.get(*f)?;
        parts.push(match part {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        });
    }
    Some(parts.join(":"))
}

/// The fields keying all three lists (every element has one, none twice).
fn keyed_by(b: &[Value], o: &[Value], t: &[Value]) -> Option<&'static [&'static str]> {
    KEYS.iter().copied().find(|fields| {
        let mut any = false;
        let ok = [b, o, t].iter().all(|list| {
            let mut seen = BTreeSet::new();
            list.iter().all(|v| {
                any = true;
                key_of(v, fields).is_some_and(|k| seen.insert(k))
            })
        });
        ok && any
    })
}

/// The field placing all three lists' elements in time.
fn placed_by(b: &[Value], o: &[Value], t: &[Value]) -> Option<&'static str> {
    PLACES.iter().copied().find(|f| {
        let mut any = false;
        let ok = [b, o, t].iter().all(|list| {
            list.iter().all(|v| {
                any = true;
                v.get(*f).and_then(Value::as_i64).is_some()
            })
        });
        ok && any
    })
}
