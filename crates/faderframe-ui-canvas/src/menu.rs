//! Long context menus without the toolkit: views group their entries into
//! submenus with [`grouped`]; the host puts a search field over menus that
//! are [`searchable`] and lists what [`search`] finds.

use crate::MenuItem;

/// More entries than this at one level are split into ranges.
pub const LONG_LEVEL: usize = 64;
/// Entries per range, about.
const RANGE: usize = 32;
/// Menus with this many entries (in all their submenus) get a search field.
pub const SEARCH_FROM: usize = 30;
/// Characters of an entry's label a range's label keeps.
const RANGE_LABEL: usize = 22;

enum Node<A> {
    Entry(MenuItem<A>),
    Group(String, Vec<Node<A>>),
}

/// A menu of `entries`, each in the submenus its path names (outermost
/// first; empty = the top level). A submenu stands where its first entry
/// came, with a separator where entries and submenus meet; a submenu that
/// holds only one submenu shows that one's entries; a submenu shows a check
/// when an entry in it is checked (unchecked when its entries have check
/// boxes); and a level longer than [`LONG_LEVEL`] is split into ranges.
pub fn grouped<A>(
    entries: impl IntoIterator<Item = (Vec<String>, MenuItem<A>)>,
) -> Vec<MenuItem<A>> {
    let mut top = Vec::new();
    for (path, item) in entries {
        insert(&mut top, &path, item);
    }
    level(top)
}

fn insert<A>(nodes: &mut Vec<Node<A>>, path: &[String], item: MenuItem<A>) {
    let Some((first, rest)) = path.split_first() else {
        nodes.push(Node::Entry(item));
        return;
    };
    let i = match nodes
        .iter()
        .position(|n| matches!(n, Node::Group(label, _) if label == first))
    {
        Some(i) => i,
        None => {
            nodes.push(Node::Group(first.clone(), Vec::new()));
            nodes.len() - 1
        }
    };
    if let Node::Group(_, children) = &mut nodes[i] {
        insert(children, rest, item);
    }
}

fn level<A>(nodes: Vec<Node<A>>) -> Vec<MenuItem<A>> {
    let mut items = Vec::with_capacity(nodes.len());
    let mut last_was_group = None;
    for node in nodes {
        let (mut item, group) = match node {
            Node::Entry(item) => (item, false),
            Node::Group(label, mut children) => {
                while matches!(children.as_slice(), [Node::Group(..)]) {
                    if let Some(Node::Group(_, inner)) = children.pop() {
                        children = inner;
                    }
                }
                let children = level(children);
                let mut item = MenuItem::submenu(label, children);
                item.checked = checks(&item.children);
                (item, true)
            }
        };
        if last_was_group.is_some_and(|was| was != group) {
            item.separator_before = true;
        }
        last_was_group = Some(group);
        items.push(item);
    }
    split_long(items)
}

/// A submenu's check: any of its entries checked, else unchecked if they
/// have check boxes.
fn checks<A>(items: &[MenuItem<A>]) -> Option<bool> {
    items.iter().fold(None, |acc, i| match (acc, i.checked) {
        (Some(true), _) | (_, Some(true)) => Some(true),
        (Some(false), _) | (_, Some(false)) => Some(false),
        _ => None,
    })
}

/// The entries of a long level in ranges, each where its first entry stood
/// (submenus stay as they are).
fn split_long<A>(items: Vec<MenuItem<A>>) -> Vec<MenuItem<A>> {
    let entries = items.iter().filter(|i| i.children.is_empty()).count();
    if entries <= LONG_LEVEL {
        return items;
    }
    let per = entries.div_ceil(entries.div_ceil(RANGE));
    let mut out: Vec<MenuItem<A>> = Vec::new();
    let mut range: Vec<MenuItem<A>> = Vec::new();
    let mut slot = 0;
    let close = |out: &mut Vec<MenuItem<A>>, slot: usize, range: Vec<MenuItem<A>>| {
        let label = match (range.first(), range.last()) {
            (Some(a), Some(b)) => format!("{} – {}", shortened(&a.label), shortened(&b.label)),
            _ => String::new(),
        };
        let separated = range.first().is_some_and(|i| i.separator_before);
        let mut item = MenuItem::submenu(label, range);
        if let Some(first) = item.children.first_mut() {
            first.separator_before = false;
        }
        item.checked = checks(&item.children);
        item.separator_before = separated;
        item.range = true;
        out[slot] = item;
    };
    for item in items {
        if !item.children.is_empty() {
            out.push(item);
            continue;
        }
        if range.is_empty() {
            slot = out.len();
            out.push(MenuItem::disabled(""));
        }
        range.push(item);
        if range.len() == per {
            close(&mut out, slot, std::mem::take(&mut range));
        }
    }
    if !range.is_empty() {
        close(&mut out, slot, range);
    }
    out
}

fn shortened(label: &str) -> String {
    if label.chars().count() <= RANGE_LABEL {
        return label.to_string();
    }
    let mut s: String = label.chars().take(RANGE_LABEL - 1).collect();
    s.truncate(s.trim_end().len());
    s.push('…');
    s
}

/// The entries of `items` and all their submenus.
pub fn entry_count<A>(items: &[MenuItem<A>]) -> usize {
    items
        .iter()
        .map(|i| {
            if i.children.is_empty() {
                1
            } else {
                entry_count(&i.children)
            }
        })
        .sum()
}

/// Does the menu get a search field?
pub fn searchable<A>(items: &[MenuItem<A>]) -> bool {
    entry_count(items) >= SEARCH_FROM
}

/// An entry [`search`] found.
#[derive(Clone, Debug, PartialEq)]
pub struct MenuMatch {
    /// Indices down the submenus to the entry.
    pub path: Vec<usize>,
    /// The entry's label after the submenus it is in ("Surge XT · Osc 1 ·
    /// Pitch"; ranges left out).
    pub label: String,
}

/// The entries in `items` and all their submenus whose label, with the
/// submenus they are in, holds every word of `query` (any case): those
/// whose own label does first, each kind in menu order. Nothing for an
/// empty query.
pub fn search<A>(items: &[MenuItem<A>], query: &str) -> Vec<MenuMatch> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    if words.is_empty() {
        return Vec::new();
    }
    let mut found = Vec::new();
    let mut path = Vec::new();
    let mut context = Vec::new();
    collect(items, &words, &mut path, &mut context, &mut found);
    // Stable: menu order within each kind.
    found.sort_by_key(|(own, _)| !own);
    found.into_iter().map(|(_, m)| m).collect()
}

fn collect<A>(
    items: &[MenuItem<A>],
    words: &[String],
    path: &mut Vec<usize>,
    context: &mut Vec<String>,
    found: &mut Vec<(bool, MenuMatch)>,
) {
    for (i, item) in items.iter().enumerate() {
        path.push(i);
        if item.children.is_empty() {
            let mut label = context.join(" · ");
            if !label.is_empty() {
                label.push_str(" · ");
            }
            label.push_str(&item.label);
            let all = label.to_lowercase();
            if words.iter().all(|w| all.contains(w.as_str())) {
                let own = item.label.to_lowercase();
                found.push((
                    words.iter().all(|w| own.contains(w.as_str())),
                    MenuMatch {
                        path: path.clone(),
                        label,
                    },
                ));
            }
        } else {
            if !item.range {
                context.push(item.label.clone());
            }
            collect(&item.children, words, path, context, found);
            if !item.range {
                context.pop();
            }
        }
        path.pop();
    }
}

/// The entry at `path` (indices down the submenus).
pub fn entry_at<'a, A>(items: &'a [MenuItem<A>], path: &[usize]) -> Option<&'a MenuItem<A>> {
    let (first, rest) = path.split_first()?;
    let item = items.get(*first)?;
    if rest.is_empty() {
        Some(item)
    } else {
        entry_at(&item.children, rest)
    }
}

/// The entry labelled by the last of `labels` in the submenus the others
/// name, looking into ranges as if they were not there.
pub fn find<'a, A>(items: &'a [MenuItem<A>], labels: &[&str]) -> Option<&'a MenuItem<A>> {
    let (first, rest) = labels.split_first()?;
    items.iter().find_map(|item| {
        if item.range {
            find(&item.children, labels)
        } else if item.label == *first {
            if rest.is_empty() {
                Some(item)
            } else {
                find(&item.children, rest)
            }
        } else {
            None
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &[&str], label: &str, n: u32) -> (Vec<String>, MenuItem<u32>) {
        (
            path.iter().map(|s| s.to_string()).collect(),
            MenuItem::new(label, n),
        )
    }

    fn labels<A>(items: &[MenuItem<A>]) -> Vec<&str> {
        items.iter().map(|i| i.label.as_str()).collect()
    }

    #[test]
    fn entries_go_into_their_submenus_where_their_first_came() {
        let menu = grouped([
            entry(&[], "Volume", 0),
            entry(&[], "Pan", 2),
            entry(&["Synth"], "Cutoff", 1),
            entry(&["Synth", "Osc 1"], "Pitch", 3),
            entry(&["EQ"], "Gain", 4),
            entry(&["Synth"], "Bypass", 5),
            entry(&["Synth", "Osc 1"], "Shape", 6),
        ]);
        assert_eq!(labels(&menu), ["Volume", "Pan", "Synth", "EQ"]);
        // A separator where entries and submenus meet.
        assert!(!menu[1].separator_before && menu[2].separator_before);
        assert!(!menu[3].separator_before);
        let synth = &menu[2].children;
        assert_eq!(labels(synth), ["Cutoff", "Osc 1", "Bypass"]);
        assert!(synth[1].separator_before && synth[2].separator_before);
        assert_eq!(labels(&synth[1].children), ["Pitch", "Shape"]);
        assert_eq!(synth[1].children[1].action, Some(6));
        assert_eq!(
            find(&menu, &["Synth", "Osc 1", "Shape"]).unwrap().action,
            Some(6)
        );
        assert!(find(&menu, &["Synth", "Shape"]).is_none());
    }

    #[test]
    fn a_submenu_of_one_submenu_shows_its_entries() {
        let menu = grouped([
            entry(&["Plugin", "Main"], "A", 0),
            entry(&["Plugin", "Main"], "B", 1),
        ]);
        assert_eq!(labels(&menu), ["Plugin"]);
        assert_eq!(labels(&menu[0].children), ["A", "B"]);
    }

    #[test]
    fn submenus_are_checked_when_something_in_them_is() {
        let item = |path: &[&str], label: &str, on: bool| {
            let (p, i) = entry(path, label, 0);
            (p, i.checked(on))
        };
        let menu = grouped([
            item(&[], "Volume", false),
            item(&["Synth", "Osc"], "Pitch", false),
            item(&["Synth", "Osc"], "Shape", true),
            item(&["Synth"], "Cutoff", false),
            item(&["EQ"], "Gain", false),
            (vec!["Echo".into()], MenuItem::new("Time", 0)),
        ]);
        assert_eq!(menu[1].checked, Some(true));
        assert_eq!(menu[1].children[0].checked, Some(true));
        assert_eq!(menu[2].checked, Some(false));
        assert_eq!(menu[3].checked, None);
    }

    #[test]
    fn long_levels_are_split_into_ranges() {
        let many: Vec<_> = (0..100)
            .map(|n| entry(&["Big"], &format!("Parameter {n}"), n))
            .chain([entry(&["Big", "Group"], "Inside", 1000)])
            .collect();
        let menu = grouped(many);
        let big = &menu[0].children;
        // 100 entries: four ranges of 25, the group where it came.
        assert_eq!(
            labels(big),
            [
                "Parameter 0 – Parameter 24",
                "Parameter 25 – Parameter 49",
                "Parameter 50 – Parameter 74",
                "Parameter 75 – Parameter 99",
                "Group"
            ]
        );
        assert!(big[..4].iter().all(|r| r.range && r.children.len() == 25));
        assert!(!big[4].range && big[4].separator_before);
        // Found as if the ranges were not there.
        assert_eq!(
            find(&menu, &["Big", "Parameter 60"]).unwrap().action,
            Some(60)
        );
        // A short level stays as it is.
        let short = grouped((0..LONG_LEVEL as u32).map(|n| entry(&[], &format!("{n}"), n)));
        assert_eq!(short.len(), LONG_LEVEL);
        assert!(shortened("A very long parameter name indeed").ends_with('…'));
        assert_eq!(
            shortened("A very long parameter name indeed")
                .chars()
                .count(),
            22
        );
    }

    #[test]
    fn search_finds_entries_in_every_submenu() {
        let mut entries = vec![
            entry(&[], "Volume", 0),
            entry(&[], "Pan", 1),
            entry(&["Surge XT", "Osc 1"], "Pitch", 2),
            entry(&["Surge XT", "Osc 2"], "Pitch", 3),
            entry(&["Surge XT", "Filter 1"], "Cutoff", 4),
            entry(&["EQ"], "Band 1 Gain", 5),
            entry(&["EQ"], "Filter", 86),
        ];
        entries
            .extend((0..80).map(|n| entry(&["Surge XT", "Osc 2"], &format!("Macro {n}"), 6 + n)));
        let menu = grouped(entries);
        let found = search(&menu, "pitch");
        let names: Vec<&str> = found.iter().map(|m| m.label.as_str()).collect();
        assert_eq!(
            names,
            ["Surge XT · Osc 1 · Pitch", "Surge XT · Osc 2 · Pitch"]
        );
        assert_eq!(entry_at(&menu, &found[1].path).unwrap().action, Some(3));
        // Every word, in any case, anywhere in the context.
        let found = search(&menu, "OSC 2 pit");
        assert_eq!(found.len(), 1);
        assert_eq!(entry_at(&menu, &found[0].path).unwrap().action, Some(3));
        // Entries called so first, then what only sits in a submenu called so.
        let names: Vec<String> = search(&menu, "filter")
            .into_iter()
            .map(|m| m.label)
            .collect();
        assert_eq!(names, ["EQ · Filter", "Surge XT · Filter 1 · Cutoff"]);
        // Ranges are left out of the context.
        let found = search(&menu, "macro 79");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].label, "Surge XT · Osc 2 · Macro 79");
        assert_eq!(entry_at(&menu, &found[0].path).unwrap().action, Some(85));
        assert!(search(&menu, "  ").is_empty());
        assert!(search(&menu, "nothing like it").is_empty());
        assert_eq!(entry_count(&menu), 87);
        assert!(searchable(&menu));
        assert!(!searchable(&menu[..2]));
    }
}
