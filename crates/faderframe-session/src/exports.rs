//! Exports to places the user chose — the album (its song files, the
//! album file and cue sheet, the DDP fileset, the vinyl premaster), a
//! movie, a sample, a lead sheet, lyrics, MIDI and MIDI 2.0 files, a
//! track preset — written the way renders are ([`crate::render::files`]):
//! to temporary files beside their destinations, moved into place once
//! everything is written, never replacing what is there without consent.
//!
//! Before an export starts, the destinations there now are asked about,
//! all in one question ([`UiRequest::AskReplace`], Overwrite / Cancel;
//! answered with [`crate::Action::AnswerReplace`]) — except the ones the
//! user agreed to replace in a save dialog that asked already (GTK's do:
//! [`Session::dispatch_agreeing`]). Right before the files move, the
//! destinations are checked again: one that appeared meanwhile is asked
//! about while the export waits beside it, and Cancel throws the export
//! away. Cancel, a failure or quitting leave what was there as it was.

use crate::render::files::{self, Overwrite, Staged};
use crate::render::{RenderError, Rendered};
use crate::{Action, NoticeLevel, Result, Session, SessionError, UiRequest};
use std::path::{Path, PathBuf};

/// May these files be replaced? (The shell asks: Overwrite / Cancel.)
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplaceQuestion {
    /// Answer with [`crate::Action::AnswerReplace`] and this.
    pub id: u64,
    /// What is being written ("Album", "MIDI file", "Movie").
    pub what: String,
    /// The destinations something is at now (a folder is replaced as a
    /// whole).
    pub paths: Vec<PathBuf>,
    /// They appeared while the export was being written: it is finished
    /// and waits beside them.
    pub appeared: bool,
}

type Answer = Box<dyn FnOnce(&mut Session, bool) -> Result<()>>;

/// Questions waiting for an answer, and what the user agreed to replace
/// for the action being dispatched.
#[derive(Default)]
pub(crate) struct Exports {
    next: u64,
    waiting: Vec<(u64, Answer)>,
    agreed: Vec<PathBuf>,
}

/// `overwrite` allowing `more` as well.
fn allowing(overwrite: &Overwrite, more: &[PathBuf]) -> Overwrite {
    match overwrite {
        Overwrite::Always => Overwrite::Always,
        Overwrite::Never => Overwrite::Confirmed(more.to_vec()),
        Overwrite::Confirmed(v) => {
            let mut v = v.clone();
            v.extend(
                more.iter()
                    .filter(|p| !v.contains(p))
                    .cloned()
                    .collect::<Vec<_>>(),
            );
            Overwrite::Confirmed(v)
        }
    }
}

/// "“Mix.mid”" / "“a.lrc” and “a.srt”" / "3 files".
fn names(paths: &[PathBuf]) -> String {
    let name = |p: &PathBuf| {
        p.file_name().map_or_else(
            || p.display().to_string(),
            |n| format!("“{}”", n.to_string_lossy()),
        )
    };
    match paths {
        [one] => name(one),
        [a, b] => format!("{} and {}", name(a), name(b)),
        more => format!("{} files", more.len()),
    }
}

impl Session {
    /// The user agreed to replace `paths` (a save dialog that asked): the
    /// export the next action starts replaces them without asking again.
    /// [`Session::forget_agreement`] ends it.
    pub fn agree_to_replace(&mut self, paths: impl IntoIterator<Item = PathBuf>) {
        self.exports.agreed.extend(paths);
    }

    pub fn forget_agreement(&mut self) {
        self.exports.agreed.clear();
    }

    /// Dispatch `action` with `paths` agreed to be replaced (a save
    /// dialog asked about them).
    pub fn dispatch_agreeing(&mut self, action: Action, paths: Vec<PathBuf>) -> Result<()> {
        self.agree_to_replace(paths);
        let r = self.dispatch(action);
        self.forget_agreement();
        r
    }

    /// The questions waiting for an answer (their ids, oldest first).
    pub fn replace_questions(&self) -> Vec<u64> {
        self.exports.waiting.iter().map(|(id, _)| *id).collect()
    }

    /// Ask the shell whether `paths` may be replaced; `answer` runs with
    /// the reply.
    pub(crate) fn ask_replace(
        &mut self,
        what: &str,
        paths: Vec<PathBuf>,
        appeared: bool,
        answer: impl FnOnce(&mut Session, bool) -> Result<()> + 'static,
    ) {
        self.exports.next += 1;
        let id = self.exports.next;
        self.exports.waiting.push((id, Box::new(answer)));
        self.ui_requests
            .push(UiRequest::AskReplace(ReplaceQuestion {
                id,
                what: what.to_string(),
                paths,
                appeared,
            }));
        self.revision += 1;
    }

    /// The user's answer to question `id` (one answered already, or
    /// thrown away, is ignored).
    pub(crate) fn answer_replace(&mut self, id: u64, replace: bool) -> Result<()> {
        let Some(i) = self.exports.waiting.iter().position(|(q, _)| *q == id) else {
            return Ok(());
        };
        let (_, answer) = self.exports.waiting.remove(i);
        self.revision += 1;
        answer(self, replace)
    }

    /// Start an export writing `dests`: the ones there now (but those the
    /// user agreed to replace) are asked about first; `start` runs with
    /// what the export may replace — at once when nothing is in the way.
    pub(crate) fn export_to(
        &mut self,
        what: &str,
        dests: Vec<PathBuf>,
        start: impl FnOnce(&mut Session, Overwrite) -> Result<()> + 'static,
    ) -> Result<()> {
        let there = files::existing(&dests);
        let (agreed, ask): (Vec<PathBuf>, Vec<PathBuf>) = there
            .into_iter()
            .partition(|p| self.exports.agreed.contains(p));
        if ask.is_empty() {
            return start(self, Overwrite::Confirmed(agreed));
        }
        let what = what.to_string();
        self.ask_replace(&what.clone(), ask.clone(), false, move |s, yes| {
            if yes {
                start(s, Overwrite::Confirmed([agreed, ask].concat()))
            } else {
                s.notify(
                    NoticeLevel::Info,
                    format!(
                        "{what} not written: {} kept as {}",
                        names(&ask),
                        if ask.len() == 1 {
                            "it was"
                        } else {
                            "they were"
                        }
                    ),
                );
                Ok(())
            }
        });
        Ok(())
    }

    /// Move a finished export's files into place as `overwrite` allows;
    /// destinations that appeared meanwhile are asked about (the files
    /// wait beside them; Cancel throws them away). `done` runs once they
    /// are in place.
    pub(crate) fn commit_export(
        &mut self,
        what: &str,
        staged: Staged,
        overwrite: &Overwrite,
        done: impl FnOnce(&mut Session, Vec<Rendered>) -> Result<()> + 'static,
    ) -> Result<()> {
        match staged.commit(overwrite) {
            Ok(placed) => done(self, placed),
            Err(RenderError::Exists(staged)) => {
                let conflicts = staged.conflicts();
                let overwrite = overwrite.clone();
                let what = what.to_string();
                self.ask_replace(&what.clone(), conflicts.clone(), true, move |s, yes| {
                    if yes {
                        let overwrite = allowing(&overwrite, &conflicts);
                        s.commit_export(&what, *staged, &overwrite, done)
                    } else {
                        staged.discard();
                        s.notify(
                            NoticeLevel::Info,
                            format!(
                                "{what} thrown away: {}, which appeared while it was written, \
                                 kept as it is",
                                names(&conflicts)
                            ),
                        );
                        Ok(())
                    }
                });
                Ok(())
            }
            Err(e) => {
                let text = format!("{what}: {e} (nothing was replaced)");
                self.notify(NoticeLevel::Error, text.clone());
                Err(SessionError::Other(text))
            }
        }
    }

    /// Write small files (destination, contents) here and now for `what`
    /// — asked first, staged, moved into place together; `done` says what
    /// was written.
    pub(crate) fn write_export(
        &mut self,
        what: &str,
        files: Vec<(PathBuf, Vec<u8>)>,
        done: impl FnOnce(&mut Session, Vec<Rendered>) + 'static,
    ) -> Result<()> {
        let dests: Vec<PathBuf> = files.iter().map(|(p, _)| p.clone()).collect();
        files_only(&dests)?;
        let name = what.to_string();
        self.export_to(what, dests, move |s, overwrite| {
            let mut staged = Vec::with_capacity(files.len());
            for (dest, bytes) in &files {
                let file = files::stage_bytes(dest, bytes)
                    .map_err(|e| SessionError::Other(format!("{name}: {e}")))?;
                staged.push(files::written(file));
            }
            s.commit_export(&name, Staged::new(staged), &overwrite, move |s, placed| {
                done(s, placed);
                Ok(())
            })
        })
    }
}

/// None of `files` (file destinations) may be a folder: no file replaces
/// one, so the export is refused before anything is written or asked.
pub(crate) fn files_only(files: &[PathBuf]) -> Result<()> {
    match files::folder_in_the_way(files) {
        Some(folder) => Err(SessionError::Other(
            RenderError::NotAFile(folder).to_string(),
        )),
        None => Ok(()),
    }
}

/// The folder `path` is written to must be there (an export into a new
/// folder makes it).
pub(crate) fn make_parent(path: &Path) -> Result<()> {
    match path.parent().filter(|p| !p.as_os_str().is_empty()) {
        Some(dir) => std::fs::create_dir_all(dir)
            .map_err(|e| SessionError::Other(format!("{}: {e}", dir.display()))),
        None => Ok(()),
    }
}
