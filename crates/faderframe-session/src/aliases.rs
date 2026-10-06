//! Clip aliases: clips that share their content. An edit that changes one
//! alias's content (notes, controllers, length, gain, fades, warp …) is
//! made to every alias in the same undo step; each keeps its own position,
//! name, colour and mute. "Duplicate as Alias" makes one, "Make Unique"
//! ends it; splitting an alias makes it its own clip first. Copies and
//! pastes are never aliases.

use crate::{Result, SelectMode, Session};
use faderframe_core::{ClipId, ClipLinkId};
use faderframe_project::{Clip, ClipContent, Command, Impact};
use faderframe_timeline::MusicalTime;

/// The clips whose content a command changes (or may).
fn content_edits(cmd: &Command, out: &mut Vec<ClipId>) {
    match cmd {
        Command::SetClipContent { clip, .. }
        | Command::AddNote { clip, .. }
        | Command::RemoveNote { clip, .. }
        | Command::UpdateNote { clip, .. } => out.push(*clip),
        Command::Batch { commands, .. } => {
            for c in commands {
                content_edits(c, out);
            }
        }
        _ => {}
    }
}

impl Session {
    /// Is the clip an alias (does it share its content with others)?
    pub fn is_alias(&self, clip: ClipId) -> bool {
        !self.project.linked_clips(clip).is_empty()
    }

    /// An alias of each clip right after it; returns them.
    pub(crate) fn duplicate_as_alias(&mut self, clips: &[ClipId]) -> Result<Vec<ClipId>> {
        let rate = self.project.sample_rate;
        let mut commands = Vec::new();
        let mut made = Vec::new();
        for id in clips {
            let Some(c) = self.project.clip(*id).cloned() else {
                continue;
            };
            let end = c.end(&self.project.timeline, rate);
            let link = match self.project.clip_links.get(id) {
                Some(l) => *l,
                None => {
                    let l: ClipLinkId = self.project.ids.allocate();
                    commands.push(Command::SetClipLink {
                        clip: *id,
                        link: Some(l),
                    });
                    l
                }
            };
            let alias: ClipId = self.project.ids.allocate();
            commands.push(Command::AddClip {
                clip: Box::new(Clip {
                    id: alias,
                    start: end,
                    ..c
                }),
            });
            commands.push(Command::SetClipLink {
                clip: alias,
                link: Some(link),
            });
            made.push(alias);
        }
        if made.is_empty() {
            return Ok(made);
        }
        self.batch("Duplicate as Alias", commands)?;
        self.selection.select_clips(&made, SelectMode::Replace);
        Ok(made)
    }

    /// The clips are their own again (no longer aliases).
    pub(crate) fn make_unique(&mut self, clips: &[ClipId]) -> Result<()> {
        let commands: Vec<Command> = clips
            .iter()
            .filter(|c| self.project.clip_links.contains_key(c))
            .map(|&clip| Command::SetClipLink { clip, link: None })
            .collect();
        if commands.is_empty() {
            return Ok(());
        }
        self.batch("Make Unique", commands)
    }

    /// Splitting an alias makes it its own clip first (the others keep
    /// their whole content).
    pub(crate) fn unlink_split_aliases(&self, cmd: Command) -> Command {
        match cmd {
            Command::SplitClip { clip, at, new_clip } if self.is_alias(clip) => Command::Batch {
                label: "Split Clip".into(),
                commands: vec![
                    Command::SetClipLink { clip, link: None },
                    Command::SplitClip { clip, at, new_clip },
                ],
            },
            Command::Batch { label, commands } => Command::Batch {
                label,
                commands: commands
                    .into_iter()
                    .map(|c| self.unlink_split_aliases(c))
                    .collect(),
            },
            c => c,
        }
    }

    /// The aliases a command changes the content of, as they are before it.
    pub(crate) fn aliases_before(&self, cmd: &Command) -> Vec<(ClipId, MusicalTime, ClipContent)> {
        let mut clips = Vec::new();
        content_edits(cmd, &mut clips);
        clips.sort();
        clips.dedup();
        clips
            .into_iter()
            .filter(|c| self.is_alias(*c))
            .filter_map(|c| {
                let clip = self.project.clip(c)?;
                Some((c, clip.start, clip.content.clone()))
            })
            .collect()
    }

    /// Make the aliases of the clips that changed the same (inside the
    /// open undo step). A start that moved (a front trim) moves theirs as
    /// much.
    pub(crate) fn mirror_aliases(
        &mut self,
        before: Vec<(ClipId, MusicalTime, ClipContent)>,
    ) -> Result<Impact> {
        let mut impact = Impact::None;
        let mut done: Vec<ClipId> = Vec::new();
        for (id, start, content) in before {
            if done.contains(&id) {
                continue;
            }
            let Some(now) = self.project.clip(id) else {
                continue;
            };
            if now.content == content && now.start == start {
                continue;
            }
            let (moved, new) = (now.start - start, now.content.clone());
            for other in self.project.linked_clips(id) {
                let Some(o) = self.project.clip(other) else {
                    continue;
                };
                let frozen = self
                    .project
                    .track(o.track)
                    .is_some_and(|t| t.freeze.is_some());
                if frozen {
                    continue;
                }
                let cmd = Command::SetClipContent {
                    clip: other,
                    start: (o.start + moved).max(MusicalTime::ZERO),
                    content: Box::new(new.clone()),
                };
                impact = impact.max(self.history.apply(&mut self.project, cmd)?);
                done.push(other);
            }
            done.push(id);
        }
        Ok(impact)
    }
}
