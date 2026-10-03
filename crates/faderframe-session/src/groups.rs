//! Track groups and multi-track edits: edits of a member's linked controls
//! (volume relatively, mute, solo and record arm alike) and its selection
//! reach the whole group while the group is active, and a control changed
//! on one of several selected tracks changes on all of them (levels and pan
//! relatively). VCAs are plain tracks of kind [`TrackKind::Vca`]; the
//! engine applies them.

use crate::{Action, Result, SelectMode, Session, SessionError};
use faderframe_core::gain::SILENCE_DB;
use faderframe_core::{GroupId, TrackId};
use faderframe_project::{
    Command, GroupLink, MAX_LEVEL_DB, Track, TrackColor, TrackGroup, TrackKind,
};
use std::collections::HashSet;

/// One switch of a [`GroupLink`].
type LinkField = fn(&mut GroupLink) -> &mut bool;

/// A value a relative move of several tracks starts from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum FollowKey {
    Volume(TrackId),
    Pan(TrackId),
    Send(faderframe_core::SendId),
}

impl Session {
    /// The other members of `track`'s active group, if it links `what`.
    fn linked(&self, track: TrackId, what: impl Fn(&GroupLink) -> bool) -> Vec<TrackId> {
        let Some(g) = self
            .project
            .track(track)
            .and_then(|t| t.group)
            .and_then(|g| self.project.group(g))
            .filter(|g| g.active && what(&g.link))
        else {
            return Vec::new();
        };
        self.project
            .group_members(g.id)
            .into_iter()
            .filter(|m| *m != track)
            .collect()
    }

    /// Tracks that follow an edit of `track`: the members of its active
    /// group when the group links the control (`link`), and, for the user's
    /// own edits, the other selected tracks when `track` is one of several
    /// selected. The master never follows; `ok` says which tracks have the
    /// control.
    fn followers(
        &self,
        track: TrackId,
        link: Option<fn(&GroupLink) -> bool>,
        ok: impl Fn(&Track) -> bool,
    ) -> Vec<&Track> {
        let mut ids = link.map_or_else(Vec::new, |l| self.linked(track, l));
        let sel = &self.selection.tracks;
        if self.user_edit && sel.len() > 1 && sel.contains(&track) {
            ids.extend(sel.iter().copied().filter(|t| *t != track));
        }
        let mut seen = HashSet::new();
        ids.into_iter()
            .filter(|t| seen.insert(*t))
            .filter_map(|t| self.project.track(t))
            .filter(|t| t.kind != TrackKind::Master && ok(t))
            .collect()
    }

    /// The value a relative move starts from: within a gesture, the value
    /// when the gesture first moved it (so balances survive clamping),
    /// else the current one.
    fn follow_base(&mut self, key: FollowKey, current: f32) -> f32 {
        if self.history.in_gesture() {
            *self.follow_base.entry(key).or_insert(current)
        } else {
            current
        }
    }

    /// The edits that follow `cmd` (group members, other selected tracks).
    pub(crate) fn group_edits(&mut self, cmd: &Command) -> Vec<Command> {
        let strip = |t: &Track| t.kind.has_audio() || t.kind == TrackKind::Vca;
        match *cmd {
            Command::SetTrackVolume { track, db } => {
                let Some(old) = self.project.track(track).map(|t| t.volume_db) else {
                    return Vec::new();
                };
                let base = self.follow_base(FollowKey::Volume(track), old);
                let others: Vec<(TrackId, f32)> = self
                    .followers(track, Some(|l| l.volume), strip)
                    .into_iter()
                    .map(|t| (t.id, t.volume_db))
                    .collect();
                others
                    .into_iter()
                    .map(|(id, v)| {
                        let from = self.follow_base(FollowKey::Volume(id), v);
                        let db = if db <= SILENCE_DB {
                            SILENCE_DB
                        } else if base <= SILENCE_DB {
                            // From -inf there is no offset to keep.
                            db
                        } else {
                            (from + db - base).clamp(SILENCE_DB, MAX_LEVEL_DB)
                        };
                        Command::SetTrackVolume { track: id, db }
                    })
                    .collect()
            }
            Command::SetTrackPan { track, pan } => {
                let Some(old) = self.project.track(track).map(|t| t.pan) else {
                    return Vec::new();
                };
                let base = self.follow_base(FollowKey::Pan(track), old);
                let others: Vec<(TrackId, f32)> = self
                    .followers(track, None, |t| t.kind.has_audio())
                    .into_iter()
                    .map(|t| (t.id, t.pan))
                    .collect();
                others
                    .into_iter()
                    .map(|(id, v)| {
                        let from = self.follow_base(FollowKey::Pan(id), v);
                        Command::SetTrackPan {
                            track: id,
                            pan: (from + pan - base).clamp(-1.0, 1.0),
                        }
                    })
                    .collect()
            }
            Command::SetSendLevel { track, send, db } => {
                // Other tracks' sends to the same destination.
                let Some((target, old)) = self
                    .project
                    .track(track)
                    .and_then(|t| t.sends.iter().find(|s| s.id == send))
                    .map(|s| (s.target, s.level_db))
                else {
                    return Vec::new();
                };
                let base = self.follow_base(FollowKey::Send(send), old);
                let others: Vec<(TrackId, faderframe_core::SendId, f32)> = self
                    .followers(track, None, |t| t.kind.has_audio())
                    .into_iter()
                    .filter_map(|t| {
                        let s = t.sends.iter().find(|s| s.target == target)?;
                        Some((t.id, s.id, s.level_db))
                    })
                    .collect();
                others
                    .into_iter()
                    .map(|(id, s, v)| {
                        let from = self.follow_base(FollowKey::Send(s), v);
                        let db = if db <= SILENCE_DB {
                            SILENCE_DB
                        } else if base <= SILENCE_DB {
                            db
                        } else {
                            (from + db - base).clamp(SILENCE_DB, MAX_LEVEL_DB)
                        };
                        Command::SetSendLevel {
                            track: id,
                            send: s,
                            db,
                        }
                    })
                    .collect()
            }
            Command::SetTrackMute { track, on } => self
                .followers(track, Some(|l| l.mute), |_| true)
                .into_iter()
                .map(|t| Command::SetTrackMute { track: t.id, on })
                .collect(),
            Command::SetTrackSolo { track, on } => self
                .followers(track, Some(|l| l.solo), |_| true)
                .into_iter()
                .map(|t| Command::SetTrackSolo { track: t.id, on })
                .collect(),
            Command::SetTrackRecordArm { track, on } => self
                .followers(track, Some(|l| l.arm), |t| t.kind.has_clips())
                .into_iter()
                .map(|t| Command::SetTrackRecordArm { track: t.id, on })
                .collect(),
            Command::SetTrackPhaseInvert { track, on } => self
                .followers(track, None, |t| t.kind.has_audio())
                .into_iter()
                .map(|t| Command::SetTrackPhaseInvert { track: t.id, on })
                .collect(),
            Command::SetTrackMonitor { track, mode } => self
                .followers(track, None, |t| t.kind == TrackKind::Audio)
                .into_iter()
                .map(|t| Command::SetTrackMonitor { track: t.id, mode })
                .collect(),
            Command::SetTrackColor { track, color } => self
                .followers(track, None, |_| true)
                .into_iter()
                .map(|t| Command::SetTrackColor { track: t.id, color })
                .collect(),
            Command::SetTrackOutput { track, ref output } => {
                let Some(kind) = self.project.track(track).map(|t| t.kind) else {
                    return Vec::new();
                };
                let p = &self.project;
                // Same kind of signal, and no feedback loops.
                let valid = |t: &Track| {
                    t.kind.has_audio() == kind.has_audio()
                        && t.kind != TrackKind::Vca
                        && match output {
                            faderframe_project::OutputRouting::Track { track: dst } => {
                                t.id != *dst && !p.would_cycle(t.id, *dst)
                            }
                            _ => true,
                        }
                };
                self.followers(track, None, valid)
                    .into_iter()
                    .map(|t| Command::SetTrackOutput {
                        track: t.id,
                        output: *output,
                    })
                    .collect()
            }
            Command::SetTrackVca { track, vca } => {
                let p = &self.project;
                let valid = |t: &Track| {
                    (t.kind.has_audio() || t.kind == TrackKind::Vca)
                        && vca.is_none_or(|v| {
                            v != t.id
                                && !p
                                    .track(v)
                                    .is_some_and(|v| p.vca_chain(v).iter().any(|c| c.id == t.id))
                        })
                };
                self.followers(track, None, valid)
                    .into_iter()
                    .map(|t| Command::SetTrackVca { track: t.id, vca })
                    .collect()
            }
            _ => Vec::new(),
        }
    }

    /// `tracks` plus the members of their active selection-linked groups.
    pub(crate) fn with_group_selection(&self, tracks: &[TrackId]) -> Vec<TrackId> {
        let mut out = tracks.to_vec();
        for t in tracks {
            for m in self.linked(*t, |l| l.selection) {
                if !out.contains(&m) {
                    out.push(m);
                }
            }
        }
        out
    }

    /// Group `tracks` (one undo step); they leave their old groups.
    pub(crate) fn create_group(&mut self, tracks: &[TrackId]) -> Result<GroupId> {
        let tracks: Vec<TrackId> = tracks
            .iter()
            .copied()
            .filter(|t| {
                self.project
                    .track(*t)
                    .is_some_and(|t| t.kind != TrackKind::Master)
            })
            .collect();
        if tracks.is_empty() {
            return Err(SessionError::Other("select the tracks to group".into()));
        }
        let id: GroupId = self.project.ids.allocate();
        let n = self.project.groups.len();
        let mut name = format!("Group {}", n + 1);
        let mut k = n + 1;
        while self.project.groups.iter().any(|g| g.name == name) {
            k += 1;
            name = format!("Group {k}");
        }
        let color = self
            .project
            .track(tracks[0])
            .map_or_else(|| TrackColor::palette(n), |t| t.color);
        let mut commands = vec![Command::AddGroup {
            group: Box::new(TrackGroup {
                id,
                name: name.clone(),
                color,
                active: true,
                link: GroupLink::default(),
            }),
        }];
        commands.extend(tracks.iter().map(|&track| Command::SetTrackGroup {
            track,
            group: Some(id),
        }));
        self.edit(Command::Batch {
            label: "Create Group".into(),
            commands,
        })?;
        self.notify(
            crate::NoticeLevel::Info,
            format!("grouped {} tracks as '{name}'", tracks.len()),
        );
        Ok(id)
    }

    /// Release a group's members and delete it (one undo step).
    pub(crate) fn delete_group(&mut self, group: GroupId) -> Result<()> {
        let mut commands: Vec<Command> = self
            .project
            .group_members(group)
            .into_iter()
            .map(|track| Command::SetTrackGroup { track, group: None })
            .collect();
        commands.push(Command::RemoveGroup { group });
        self.edit(Command::Batch {
            label: "Delete Group".into(),
            commands,
        })
    }

    /// Change a group's settings.
    pub(crate) fn update_group(
        &mut self,
        group: GroupId,
        change: impl FnOnce(&mut TrackGroup),
    ) -> Result<()> {
        let mut g = self
            .project
            .group(group)
            .cloned()
            .ok_or_else(|| SessionError::Other(format!("no group {group}")))?;
        change(&mut g);
        self.edit(Command::UpdateGroup { group: Box::new(g) })
    }
}

/// One entry of a track's group/VCA menu (views turn these into their
/// menu items).
#[derive(Clone, Debug, PartialEq)]
pub struct GroupMenuEntry {
    pub label: String,
    /// `None`: a heading.
    pub action: Option<Action>,
    pub checked: Option<bool>,
    /// Starts a new section.
    pub separated: bool,
}

impl GroupMenuEntry {
    fn new(label: impl Into<String>, action: Action) -> Self {
        Self {
            label: label.into(),
            action: Some(action),
            checked: None,
            separated: false,
        }
    }

    fn heading(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            action: None,
            checked: None,
            separated: false,
        }
    }

    fn checked(mut self, on: bool) -> Self {
        self.checked = Some(on);
        self
    }

    fn separated(mut self) -> Self {
        self.separated = true;
        self
    }
}

impl Session {
    /// Group and VCA entries of a track's menu.
    pub fn group_menu(&self, track: TrackId) -> Vec<GroupMenuEntry> {
        let p = &self.project;
        let Some(t) = p.track(track) else {
            return Vec::new();
        };
        if t.kind == TrackKind::Master {
            return Vec::new();
        }
        let mut out = Vec::new();
        let selected = self.selection.tracks.len();
        out.push(
            GroupMenuEntry::new(
                if selected > 1 {
                    format!("Group {selected} Selected Tracks")
                } else {
                    "Group Selected Track".to_string()
                },
                Action::GroupSelectedTracks,
            )
            .separated(),
        );
        if let Some(g) = t.group.and_then(|g| p.group(g)) {
            out.push(GroupMenuEntry::heading(format!("Group '{}'", g.name)).separated());
            out.push(
                GroupMenuEntry::new(
                    "Group Active",
                    Action::SetGroupActive {
                        group: g.id,
                        active: !g.active,
                    },
                )
                .checked(g.active),
            );
            let links: [(&str, LinkField); 5] = [
                ("Link Volume", |l| &mut l.volume),
                ("Link Mute", |l| &mut l.mute),
                ("Link Solo", |l| &mut l.solo),
                ("Link Record Arm", |l| &mut l.arm),
                ("Link Selection", |l| &mut l.selection),
            ];
            for (label, field) in links {
                let mut link = g.link;
                let on = *field(&mut link);
                *field(&mut link) = !on;
                out.push(
                    GroupMenuEntry::new(label, Action::SetGroupLink { group: g.id, link })
                        .checked(on),
                );
            }
            out.push(GroupMenuEntry::new(
                "Rename Group…",
                Action::PromptRenameGroup(g.id),
            ));
            out.push(GroupMenuEntry::new(
                "Remove from Group",
                Action::Edit(Command::SetTrackGroup { track, group: None }),
            ));
            out.push(GroupMenuEntry::new(
                format!("Delete Group '{}'", g.name),
                Action::DeleteGroup(g.id),
            ));
        }
        for (i, g) in p
            .groups
            .iter()
            .filter(|g| Some(g.id) != t.group)
            .enumerate()
        {
            let e = GroupMenuEntry::new(
                format!("Add to Group '{}'", g.name),
                Action::Edit(Command::SetTrackGroup {
                    track,
                    group: Some(g.id),
                }),
            );
            out.push(if i == 0 { e.separated() } else { e });
        }
        // VCA assignment (tracks with audio and VCAs).
        if t.kind.has_audio() || t.kind == TrackKind::Vca {
            let vcas: Vec<&faderframe_project::Track> = p
                .tracks
                .iter()
                .filter(|v| {
                    v.kind == TrackKind::Vca
                        && v.id != track
                        && !p.vca_chain(v).iter().any(|c| c.id == track)
                })
                .collect();
            if !vcas.is_empty() || t.vca.is_some() {
                out.push(
                    GroupMenuEntry::new(
                        "VCA: None",
                        Action::Edit(Command::SetTrackVca { track, vca: None }),
                    )
                    .checked(t.vca.is_none())
                    .separated(),
                );
                for v in vcas {
                    out.push(
                        GroupMenuEntry::new(
                            format!("VCA: {}", v.name),
                            Action::Edit(Command::SetTrackVca {
                                track,
                                vca: Some(v.id),
                            }),
                        )
                        .checked(t.vca == Some(v.id)),
                    );
                }
            }
        }
        if t.kind == TrackKind::Vca {
            let members = p.vca_members(track);
            out.push(
                GroupMenuEntry::new("Assign Selected Tracks", Action::AssignSelectedToVca(track))
                    .separated(),
            );
            if !members.is_empty() {
                out.push(GroupMenuEntry::new(
                    format!("Select {} Assigned Tracks", members.len()),
                    Action::SelectTracks {
                        tracks: members,
                        mode: SelectMode::Replace,
                    },
                ));
            }
        }
        out
    }

    /// Assign the selected tracks (with audio) to `vca` (one undo step).
    pub(crate) fn assign_selected_to_vca(&mut self, vca: TrackId) -> Result<()> {
        let p = &self.project;
        let commands: Vec<Command> = self
            .selection
            .tracks
            .iter()
            .filter_map(|id| p.track(*id))
            .filter(|t| {
                t.id != vca
                    && t.kind != TrackKind::Master
                    && (t.kind.has_audio() || t.kind == TrackKind::Vca)
                    && t.vca != Some(vca)
            })
            .map(|t| Command::SetTrackVca {
                track: t.id,
                vca: Some(vca),
            })
            .collect();
        if commands.is_empty() {
            return Err(SessionError::Other(
                "select the tracks to assign to the VCA".into(),
            ));
        }
        self.edit(Command::Batch {
            label: "Assign VCA".into(),
            commands,
        })
    }
}
