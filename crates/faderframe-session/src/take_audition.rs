//! Auditioning a take: a take folder plays one of its takes whole instead
//! of the comp (the arranger's ▶ on a take lane), without editing the
//! project. It ends when asked, when the folder's comp or takes change, or
//! when another project opens.

use crate::{Action, Result, Session, TransportAction};
use faderframe_core::ClipId;
use faderframe_project::{CompSegment, Impact, Project};

/// The take a folder plays instead of its comp.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TakeAudition {
    pub clip: ClipId,
    pub take: usize,
    /// The folder's comp and take count when it began (a change ends it).
    comp: Vec<CompSegment>,
    takes: usize,
}

impl TakeAudition {
    /// The folder is as it was when the audition began.
    pub(crate) fn holds(&self, project: &Project) -> bool {
        project
            .clip(self.clip)
            .and_then(|c| c.as_takes())
            .is_some_and(|f| f.comp == self.comp && f.takes.len() == self.takes)
    }
}

impl Session {
    /// The take a folder plays instead of its comp, if any.
    pub fn auditioned_take(&self) -> Option<(ClipId, usize)> {
        self.take_audition.as_ref().map(|a| (a.clip, a.take))
    }

    /// Play `take` of the folder `clip` instead of its comp (`None`: the
    /// comp). Stopped, playback starts at the folder (unless the playhead
    /// is in it already).
    pub(crate) fn audition_take(&mut self, clip: ClipId, take: Option<usize>) -> Result<()> {
        let folder = self.project.clip(clip).and_then(|c| {
            let f = c.as_takes()?;
            Some((
                c.start,
                c.end(&self.project.timeline, self.project.sample_rate),
                f,
            ))
        });
        let wanted = match (take, folder) {
            (Some(k), Some((_, _, f))) if k < f.takes.len() => Some(TakeAudition {
                clip,
                take: k,
                comp: f.comp.clone(),
                takes: f.takes.len(),
            }),
            _ => None,
        };
        let span = folder.map(|(a, b, _)| (a, b));
        let starting = wanted.is_some();
        self.take_audition = wanted;
        self.engine.set_take_audition(self.auditioned_take());
        self.sync(Impact::Timeline)?;
        if starting
            && !self.transport().playing
            && let Some((start, end)) = span
        {
            let here = self.playhead();
            if here < start || here >= end {
                self.dispatch(Action::Transport(TransportAction::Locate(start)))?;
            }
            self.dispatch(Action::Transport(TransportAction::Play))?;
        }
        self.shown_changed();
        Ok(())
    }

    /// Before the engine follows an edit: an audition whose folder changed
    /// ends (true: the timeline must be rebuilt).
    pub(crate) fn audition_still_holds(&mut self) -> bool {
        if self
            .take_audition
            .as_ref()
            .is_none_or(|a| a.holds(&self.project))
        {
            return true;
        }
        self.take_audition = None;
        self.engine.set_take_audition(None);
        false
    }
}
