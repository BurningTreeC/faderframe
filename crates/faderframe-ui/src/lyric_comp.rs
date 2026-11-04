//! Comping by lyric from a script (see `faderframe_session::lyric_comp`;
//! the arranger's take lanes and lyrics comp row are the way in by hand):
//!
//! * `comp-lyrics:<track>` hears every take of the track's first take
//!   folder (its lanes opened);
//! * `comp-word:<track>=<word>[-<word>]@<take n>` plays take n (from 1)
//!   for a word (from 0, the order of the lyrics comp row) or a run;
//! * `comp-line:<track>=<word>@<take n>` the line holding the word;
//! * `comp-best:<track>[=<word>-<word>]` the suggested takes;
//! * `log-lyric-comp:<track>` logs the line: per word the take chosen,
//!   the take suggested and why.

use crate::state::AppState;
use faderframe_core::ClipId;
use faderframe_session::Action;
use faderframe_session::lyric_comp::LyricCompOp;
use gtk::gio;
use gtk::prelude::*;
use std::rc::Rc;

pub(crate) fn install(app: &Rc<AppState>) {
    let named = |name: &'static str, f: fn(&Rc<AppState>, &str)| {
        let weak = Rc::downgrade(app);
        gio::ActionEntry::builder(name)
            .parameter_type(Some(&String::static_variant_type()))
            .activate(move |_, _, param| {
                if let (Some(a), Some(arg)) =
                    (weak.upgrade(), param.and_then(|p| p.get::<String>()))
                {
                    f(&a, &arg);
                }
            })
            .build()
    };
    app.app.add_action_entries([
        named("comp-lyrics", |a, arg| {
            let Some(clip) = folder(a, arg.trim()) else {
                return tracing::warn!("comp-lyrics: no take folder on '{arg}'");
            };
            if !a.session.borrow().takes_open(clip) {
                a.dispatch(Action::ToggleTakeLanes(clip));
            }
            a.dispatch(Action::LyricComp(LyricCompOp::Hear { clip }));
        }),
        named("comp-word", |a, arg| {
            let Some((clip, words, take)) = word_and_take(a, arg) else {
                return tracing::warn!(
                    "comp-word: '{arg}' is not <track>=<word>[-<word>]@<take n>"
                );
            };
            a.dispatch(Action::LyricComp(LyricCompOp::Comp {
                clip,
                from: words.0,
                to: words.1,
                take,
            }));
        }),
        named("comp-line", |a, arg| {
            let Some((clip, words, take)) = word_and_take(a, arg) else {
                return tracing::warn!("comp-line: '{arg}' is not <track>=<word>@<take n>");
            };
            a.dispatch(Action::LyricComp(LyricCompOp::CompLine {
                clip,
                word: words.0,
                take,
            }));
        }),
        named("comp-best", |a, arg| {
            let (name, range) = match arg.split_once('=') {
                Some((n, r)) => (n.trim(), parse_words(r)),
                None => (arg.trim(), None),
            };
            let Some(clip) = folder(a, name) else {
                return tracing::warn!("comp-best: no take folder on '{name}'");
            };
            a.dispatch(Action::LyricComp(LyricCompOp::UseBest {
                clip,
                words: range,
            }));
        }),
        named("log-lyric-comp", |a, arg| {
            let Some(clip) = folder(a, arg.trim()) else {
                return tracing::warn!("log-lyric-comp: no take folder on '{arg}'");
            };
            let s = a.session.borrow();
            let (Some(comp), Some(f)) = (
                s.lyric_comp(clip),
                s.project().clip(clip).and_then(|c| c.as_takes()),
            ) else {
                return tracing::info!("lyric comp: nothing heard in '{arg}' yet");
            };
            for (i, c) in comp.alignment.columns.iter().enumerate() {
                let chosen = comp.alignment.chosen(i, f).map(|t| t + 1);
                let best = comp.best[i];
                tracing::info!(
                    "lyric comp: {i} “{}” chosen {chosen:?} suggested {:?} ({})",
                    c.text,
                    best.map(|t| t + 1),
                    best.map(|t| comp.reasons(i, t, 4).join(", "))
                        .unwrap_or_default()
                );
            }
        }),
    ]);
}

/// The first take folder on the track named `name`.
fn folder(a: &Rc<AppState>, name: &str) -> Option<ClipId> {
    let s = a.session.borrow();
    let p = s.project();
    let t = p.tracks.iter().find(|t| t.name == name)?;
    p.clips_of(t.id)
        .into_iter()
        .find(|c| c.as_takes().is_some())
        .map(|c| c.id)
}

/// `<word>` or `<word>-<word>` (from 0).
fn parse_words(text: &str) -> Option<(usize, usize)> {
    let text = text.trim();
    match text.split_once('-') {
        Some((x, y)) => Some((x.trim().parse().ok()?, y.trim().parse().ok()?)),
        None => text.parse().ok().map(|w| (w, w)),
    }
}

/// `<track>=<word>[-<word>]@<take n>`: the folder, the words, the take
/// (from 0).
fn word_and_take(a: &Rc<AppState>, arg: &str) -> Option<(ClipId, (usize, usize), usize)> {
    let (name, rest) = arg.split_once('=')?;
    let (words, take) = rest.split_once('@')?;
    let take = take.trim().parse::<usize>().ok()?.checked_sub(1)?;
    Some((folder(a, name.trim())?, parse_words(words)?, take))
}
