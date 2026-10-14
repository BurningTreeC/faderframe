//! Duplicate Track: a copy of a track right after it, with its settings,
//! devices (as they sound now), sends, automation and modulators under
//! fresh ids; with its content, its clips too. A folder brings the tracks
//! it holds (copied into the new folder) when its content is copied.

use crate::{Action, GroupMenuEntry, Result, SelectMode, Session};
use faderframe_automation::AutomationTarget;
use faderframe_core::{ClipId, PluginInstanceId, TrackId};
use faderframe_project::modulation::ModTarget;
use faderframe_project::{Clip, Command, InputRouting, Project, Track, TrackKind};
use std::collections::HashMap;

/// A name like `name` no track has yet: "Bass" → "Bass 2", "Audio 1" →
/// "Audio 2" (counting on from a number at its end).
pub(crate) fn copy_name(p: &Project, name: &str) -> String {
    let (base, n) = match name.rsplit_once(' ') {
        Some((b, n)) if !b.is_empty() => match n.parse::<u32>() {
            Ok(n) => (b, n),
            Err(_) => (name, 1),
        },
        _ => (name, 1),
    };
    (n + 1..)
        .map(|k| format!("{base} {k}"))
        .find(|c| p.tracks.iter().all(|t| t.name != *c))
        .unwrap_or_else(|| format!("{name} copy"))
}

impl Session {
    /// The track menus' Duplicate items for `track` (the selected tracks
    /// when it is one of them).
    pub fn duplicate_menu(&self, track: TrackId) -> Vec<GroupMenuEntry> {
        let Some(t) = self.project.track(track) else {
            return Vec::new();
        };
        if t.kind == TrackKind::Master {
            return Vec::new();
        }
        let tracks: Vec<TrackId> = if self.selection.tracks.contains(&track) {
            self.selection.tracks.iter().copied().collect()
        } else {
            vec![track]
        };
        let several = tracks.len() > 1;
        let has_content = tracks.iter().any(|id| {
            self.project
                .track(*id)
                .is_some_and(|t| t.kind.has_clips() || t.kind == TrackKind::Folder)
        });
        let mut out = vec![GroupMenuEntry {
            label: if several {
                "Duplicate Tracks"
            } else {
                "Duplicate Track"
            }
            .into(),
            action: Some(Action::DuplicateTracks {
                tracks: tracks.clone(),
                content: false,
            }),
            checked: None,
            separated: true,
        }];
        if has_content {
            out.push(GroupMenuEntry {
                label: if several {
                    "Duplicate Tracks with Content"
                } else {
                    "Duplicate Track with Content"
                }
                .into(),
                action: Some(Action::DuplicateTracks {
                    tracks,
                    content: true,
                }),
                checked: None,
                separated: false,
            });
        }
        out
    }

    /// Duplicate the tracks (each right after itself; a folder after its
    /// contents), with their clips when `content`. One undo step; the
    /// copies end up selected.
    pub(crate) fn duplicate_tracks(&mut self, tracks: &[TrackId], content: bool) -> Result<()> {
        // The slots carry the devices' current state into the copies.
        self.capture_plugin_states();
        let mut order: Vec<TrackId> = tracks
            .iter()
            .copied()
            .filter(|t| {
                self.project
                    .track(*t)
                    .is_some_and(|t| t.kind != TrackKind::Master)
            })
            .collect();
        // A folder's contents come along with it (when content is copied);
        // tracks listed as well as their folder are copied once.
        if content {
            let folders: Vec<TrackId> = order
                .iter()
                .copied()
                .filter(|t| {
                    self.project
                        .track(*t)
                        .is_some_and(|t| t.kind == TrackKind::Folder)
                })
                .collect();
            for f in folders {
                for t in self.folder_contents(f) {
                    if !order.contains(&t) {
                        order.push(t);
                    }
                }
            }
        }
        if order.is_empty() {
            return Ok(());
        }
        // Copy in project order, so folders come before what they hold.
        order.sort_by_key(|t| self.project.track_index(*t));
        let mut new_ids: HashMap<TrackId, TrackId> = HashMap::new();
        // A copied folder → the copy placed last inside it (or itself).
        let mut last_in: HashMap<TrackId, TrackId> = HashMap::new();
        let mut parent: HashMap<TrackId, Option<TrackId>> = HashMap::new();
        let mut commands = Vec::new();
        let mut made = Vec::new();
        let mut names: Vec<String> = Vec::new();
        // The order the tracks will have, as the copies go in.
        let mut placed: Vec<TrackId> = self.project.tracks.iter().map(|t| t.id).collect();
        for id in &order {
            let Some(original) = self.project.track(*id).cloned() else {
                continue;
            };
            let (mut track, clips) = copy_track(&mut self.project, &original, content);
            track.name = copy_name_avoiding(&self.project, &original.name, &names);
            names.push(track.name.clone());
            let in_copy = original.folder.and_then(|f| new_ids.get(&f).copied());
            if let Some(f) = in_copy {
                track.folder = Some(f);
            }
            new_ids.insert(original.id, track.id);
            // Inside a copied folder: after what was copied into it so far.
            // Else after the original (past its contents, for a folder).
            let after = match in_copy {
                Some(f) => last_in.get(&f).copied().unwrap_or(f),
                None => {
                    let p = &self.project;
                    let at = placed
                        .iter()
                        .position(|t| t == id)
                        .unwrap_or(placed.len() - 1);
                    let inside = |t: &TrackId| p.track(*t).is_some_and(|x| p.in_folder(x, *id));
                    let mut end = at;
                    while end + 1 < placed.len() && inside(&placed[end + 1]) {
                        end += 1;
                    }
                    placed[end]
                }
            };
            let index = placed
                .iter()
                .position(|t| *t == after)
                .map_or(placed.len(), |i| i + 1);
            placed.insert(index, track.id);
            // The copy is now the last of every copied folder around it.
            parent.insert(track.id, in_copy);
            let mut up = in_copy;
            while let Some(f) = up {
                last_in.insert(f, track.id);
                up = parent.get(&f).copied().flatten();
            }
            made.push(track.id);
            commands.push(Command::AddTrack {
                track: Box::new(track),
                index,
            });
            commands.extend(clips.into_iter().map(|clip| Command::AddClip {
                clip: Box::new(clip),
            }));
        }
        let label = if content {
            "Duplicate Track with Content"
        } else {
            "Duplicate Track"
        };
        self.batch(label, commands)?;
        self.selection.select_tracks(&made, SelectMode::Replace);
        Ok(())
    }
}

fn copy_name_avoiding(p: &Project, name: &str, taken: &[String]) -> String {
    let mut name = copy_name(p, name);
    while taken.contains(&name) {
        name = copy_name(p, &name);
    }
    name
}

/// A copy of `t` under fresh ids (track, devices, sends, lanes,
/// modulators) and, when `content`, copies of its clips.
fn copy_track(p: &mut Project, t: &Track, content: bool) -> (Track, Vec<Clip>) {
    let mut c = t.clone();
    c.id = p.ids.allocate();
    c.clips.clear();
    c.record_arm = false;
    c.solo = false;
    // A frozen render belongs to the original; the copy plays its devices.
    c.freeze = None;
    // Another track's plugin output has one taker.
    if matches!(c.input, InputRouting::Plugin { .. }) {
        c.input = InputRouting::None;
    }
    // Devices (nested ones too) and the chains of containers.
    let mut plugins: HashMap<PluginInstanceId, PluginInstanceId> = HashMap::new();
    for s in c.slots_mut() {
        let new: PluginInstanceId = p.ids.allocate();
        plugins.insert(s.id, new);
        s.id = new;
    }
    c.containers = std::mem::take(&mut c.containers)
        .into_iter()
        .map(|(k, v)| (plugins.get(&k).copied().unwrap_or(k), v))
        .collect();
    let mut sends = HashMap::new();
    for s in &mut c.sends {
        let new = p.ids.allocate();
        sends.insert(s.id, new);
        s.id = new;
    }
    for lane in &mut c.automation.lanes {
        lane.id = p.ids.allocate();
        match &mut lane.target {
            AutomationTarget::PluginParameter { plugin, .. }
            | AutomationTarget::PluginBypass(plugin) => {
                if let Some(n) = plugins.get(plugin) {
                    *plugin = *n;
                }
            }
            AutomationTarget::SendLevel(s) => {
                if let Some(n) = sends.get(s) {
                    *s = *n;
                }
            }
            _ => {}
        }
    }
    for m in &mut c.modulators {
        m.id = p.ids.allocate();
        for r in &mut m.routes {
            if let ModTarget::Plugin { plugin, .. } = &mut r.target
                && let Some(n) = plugins.get(plugin)
            {
                *plugin = *n;
            }
        }
    }
    let clips = if content {
        let originals: Vec<Clip> = t
            .clips
            .iter()
            .filter_map(|id| p.clip(*id).cloned())
            .collect();
        originals
            .into_iter()
            .map(|clip| {
                let id: ClipId = p.ids.allocate();
                Clip {
                    id,
                    track: c.id,
                    ..clip
                }
            })
            .collect()
    } else {
        Vec::new()
    };
    (c, clips)
}
