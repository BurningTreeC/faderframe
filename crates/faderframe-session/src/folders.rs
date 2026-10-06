//! Folder tracks: they hold tracks (and folders) to organise a project.
//! The editors show a folder's tracks right under it (see
//! `Project::folder_order`), it opens and closes (a view setting, kept
//! with the layout), and its mute and solo reach what it holds. Removing
//! a folder keeps its tracks: they move up a level, in the same undo step.

use crate::{Action, Result, SelectMode, Session, SessionError};
use faderframe_core::TrackId;
use faderframe_project::{Command, OutputRouting, Track, TrackKind};
use std::collections::HashSet;

impl Session {
    /// Is the folder open (its tracks shown)?
    pub fn folder_open(&self, folder: TrackId) -> bool {
        !self.workspace.closed_folders.contains(&folder)
    }

    /// Is every folder `track` is in open?
    pub fn track_shown(&self, track: &Track) -> bool {
        self.project
            .folder_chain(track)
            .iter()
            .all(|f| self.folder_open(f.id))
    }

    /// The tracks a folder holds (all levels).
    pub fn folder_contents(&self, folder: TrackId) -> Vec<TrackId> {
        self.project
            .tracks
            .iter()
            .filter(|t| self.project.in_folder(t, folder))
            .map(|t| t.id)
            .collect()
    }

    pub(crate) fn toggle_folder(&mut self, folder: TrackId) {
        if !self.workspace.closed_folders.remove(&folder) {
            self.workspace.closed_folders.insert(folder);
        }
        self.revision += 1;
    }

    /// A new folder holding `tracks` (inside the folder they share, if
    /// they share one); returns it.
    pub(crate) fn new_folder(&mut self, tracks: &[TrackId]) -> Result<TrackId> {
        let p = &self.project;
        let chosen: HashSet<TrackId> = tracks.iter().copied().collect();
        // A track goes with its folder when that is chosen too.
        let moved: Vec<TrackId> = p
            .tracks
            .iter()
            .filter(|t| chosen.contains(&t.id) && t.kind != TrackKind::Master)
            .filter(|t| !p.folder_chain(t).iter().any(|f| chosen.contains(&f.id)))
            .map(|t| t.id)
            .collect();
        let parents: HashSet<Option<TrackId>> = moved
            .iter()
            .filter_map(|t| p.track(*t))
            .map(|t| p.folder_chain(t).first().map(|f| f.id))
            .collect();
        let parent = match parents.len() {
            1 => parents.into_iter().next().flatten(),
            _ => None,
        };
        let index = moved
            .iter()
            .filter_map(|t| p.track_index(*t))
            .min()
            .unwrap_or_else(|| {
                p.tracks
                    .iter()
                    .position(|t| t.kind == TrackKind::Master)
                    .unwrap_or(p.tracks.len())
            });
        let n = p
            .tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Folder)
            .count()
            + 1;
        let color = moved
            .first()
            .and_then(|t| p.track(*t))
            .map_or_else(|| faderframe_project::TrackColor::palette(n), |t| t.color);
        let id: TrackId = self.project.ids.allocate();
        let mut folder = Track::new(id, TrackKind::Folder, format!("Folder {n}"), color);
        folder.folder = parent;
        folder.input = faderframe_project::InputRouting::None;
        folder.output = OutputRouting::None;
        let mut commands = vec![Command::AddTrack {
            track: Box::new(folder),
            index,
        }];
        commands.extend(moved.iter().map(|&track| Command::SetTrackFolder {
            track,
            folder: Some(id),
        }));
        self.batch("New Folder", commands)?;
        self.workspace.closed_folders.remove(&id);
        self.selection.select_tracks(&[id], SelectMode::Replace);
        Ok(id)
    }

    /// Put `tracks` into `folder` (`None`: out of their folders, a level
    /// up). Tracks that cannot go there (the folder itself, a folder it is
    /// in, the master) stay.
    pub(crate) fn move_to_folder(
        &mut self,
        tracks: &[TrackId],
        folder: Option<TrackId>,
    ) -> Result<()> {
        let p = &self.project;
        if let Some(f) = folder
            && !p.track(f).is_some_and(|t| t.kind == TrackKind::Folder)
        {
            return Err(SessionError::Other("that track is not a folder".into()));
        }
        let commands: Vec<Command> = tracks
            .iter()
            .filter_map(|id| p.track(*id))
            .filter(|t| t.kind != TrackKind::Master)
            .filter(|t| match folder {
                // Not into itself or into a folder inside it.
                Some(f) => t.id != f && p.track(f).is_some_and(|ft| !p.in_folder(ft, t.id)),
                None => t.folder.is_some(),
            })
            .map(|t| Command::SetTrackFolder {
                track: t.id,
                folder: match folder {
                    Some(f) => Some(f),
                    // A level up: the folder its folder is in.
                    None => p.folder_chain(t).get(1).map(|f| f.id),
                },
            })
            .collect();
        if commands.is_empty() {
            return Ok(());
        }
        let label = if folder.is_some() {
            "Move to Folder"
        } else {
            "Move out of Folder"
        };
        self.batch(label, commands)
    }

    /// The folder entries of a track's menu: label, action, separator
    /// before. `selected` are the tracks a new folder would hold.
    pub fn folder_choices(&self, track: TrackId) -> Vec<(String, Action, bool)> {
        let p = &self.project;
        let Some(t) = p.track(track).filter(|t| t.kind != TrackKind::Master) else {
            return Vec::new();
        };
        let mut tracks: Vec<TrackId> = if self.selection.tracks.contains(&track) {
            self.selection.tracks.iter().copied().collect()
        } else {
            vec![track]
        };
        tracks.retain(|id| p.track(*id).is_some_and(|t| t.kind != TrackKind::Master));
        let what = if tracks.len() > 1 {
            "Selected Tracks"
        } else {
            "Track"
        };
        let mut out = vec![(
            format!("New Folder with the {what}"),
            Action::NewFolder {
                tracks: tracks.clone(),
            },
            true,
        )];
        for f in p
            .folder_order()
            .into_iter()
            .filter(|f| f.kind == TrackKind::Folder && f.id != track)
            .filter(|f| !p.in_folder(f, track) && t.folder != Some(f.id))
        {
            out.push((
                format!("Move into ‘{}’", f.name),
                Action::MoveToFolder {
                    tracks: tracks.clone(),
                    folder: Some(f.id),
                },
                false,
            ));
        }
        if let Some(f) = p.folder_chain(t).first() {
            out.push((
                format!("Move out of ‘{}’", f.name),
                Action::MoveToFolder {
                    tracks,
                    folder: None,
                },
                false,
            ));
        }
        out
    }

    /// Sum a folder's tracks in a new bus inside it: the tracks that went
    /// to the master go to the bus instead.
    pub(crate) fn sum_folder(&mut self, folder: TrackId) -> Result<TrackId> {
        let p = &self.project;
        let f = p
            .track(folder)
            .filter(|t| t.kind == TrackKind::Folder)
            .ok_or_else(|| SessionError::Other("that track is not a folder".into()))?;
        let name = format!("{} Bus", f.name);
        let color = f.color;
        let inside: Vec<TrackId> = self
            .folder_contents(folder)
            .into_iter()
            .filter(|id| {
                p.track(*id).is_some_and(|t| {
                    t.kind.has_audio()
                        && t.kind != TrackKind::Master
                        && t.output == OutputRouting::Master
                })
            })
            .collect();
        let index = p
            .tracks
            .iter()
            .position(|t| t.kind == TrackKind::Master)
            .unwrap_or(p.tracks.len());
        let id: TrackId = self.project.ids.allocate();
        let mut bus = Track::new(id, TrackKind::Bus, name, color);
        bus.folder = Some(folder);
        let mut commands = vec![Command::AddTrack {
            track: Box::new(bus),
            index,
        }];
        commands.extend(inside.into_iter().map(|track| Command::SetTrackOutput {
            track,
            output: OutputRouting::Track { track: id },
        }));
        self.batch("Sum Folder into a Bus", commands)?;
        Ok(id)
    }

    /// Removing a folder keeps what it holds: its tracks move up a level
    /// (in the same step).
    pub(crate) fn keep_folder_contents(&self, cmd: Command) -> Command {
        fn removed(cmd: &Command, out: &mut Vec<TrackId>) {
            match cmd {
                Command::RemoveTrack { track } => out.push(*track),
                Command::Batch { commands, .. } => {
                    for c in commands {
                        removed(c, out);
                    }
                }
                _ => {}
            }
        }
        let p = &self.project;
        let mut gone = Vec::new();
        removed(&cmd, &mut gone);
        let folders: HashSet<TrackId> = gone
            .iter()
            .copied()
            .filter(|t| p.track(*t).is_some_and(|t| t.kind == TrackKind::Folder))
            .collect();
        if folders.is_empty() {
            return cmd;
        }
        let gone: HashSet<TrackId> = gone.into_iter().collect();
        let moves: Vec<Command> = p
            .tracks
            .iter()
            .filter(|t| !gone.contains(&t.id))
            .filter(|t| t.folder.is_some_and(|f| folders.contains(&f)))
            .map(|t| Command::SetTrackFolder {
                track: t.id,
                // The nearest folder above that stays.
                folder: p
                    .folder_chain(t)
                    .iter()
                    .find(|f| !gone.contains(&f.id))
                    .map(|f| f.id),
            })
            .collect();
        if moves.is_empty() {
            return cmd;
        }
        Command::Batch {
            label: cmd.label(),
            commands: moves.into_iter().chain(std::iter::once(cmd)).collect(),
        }
    }
}
