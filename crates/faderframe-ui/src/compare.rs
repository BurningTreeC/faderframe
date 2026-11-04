//! A/B listening's actions: the Reference view, the flip between the mix
//! and what it is compared with (Ctrl+Alt+A: "Then" of an audition of the
//! undo history, a reference track, or Audio Bisect's A ⇄ B), Audio Bisect
//! from the Edit menu, and the development aids that drive them and the
//! undo tree from a script.

use crate::state::AppState;
use faderframe_session::bisect::{BisectAnswer, BisectOp, BisectSide};
use faderframe_session::compare::CompareOp;
use faderframe_session::references::ReferenceOp;
use faderframe_session::{Action, WorkspaceAction};
use faderframe_workspace::ViewId;
use gtk::gio;
use gtk::prelude::*;
use std::rc::Rc;

pub(crate) fn install(app: &Rc<AppState>) {
    let simple = |name: &'static str, action: Action| {
        let weak = Rc::downgrade(app);
        gio::ActionEntry::builder(name)
            .activate(move |_, _, _| {
                if let Some(a) = weak.upgrade() {
                    a.dispatch(action.clone());
                }
            })
            .build()
    };
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
        simple(
            "show-reference",
            Action::Workspace(WorkspaceAction::ShowView(ViewId::reference())),
        ),
        // Mix ⇄ the other side (Now ⇄ Then, Mix ⇄ Reference).
        simple("compare-flip", Action::Compare(CompareOp::Flip)),
        simple("compare-off", Action::Compare(CompareOp::Off)),
        // Audio Bisect: the History view, getting ready (the good state is
        // marked there).
        simple(
            "audio-bisect",
            Action::Several(vec![
                Action::Workspace(WorkspaceAction::ShowView(ViewId::history())),
                Action::Bisect(BisectOp::Setup),
            ]),
        ),
        // Development aid: `history-to-node:<id>` goes to a step of the
        // undo tree (0: the start), `history-branch:<n>` to the newest
        // step of branch n, `audition-node:<id>` auditions a step.
        named("history-to-node", |a, arg| {
            match arg.trim().parse::<u64>() {
                Ok(id) => a.dispatch(Action::HistoryToNode(id)),
                Err(_) => tracing::warn!("history-to-node: '{arg}' is not a step id"),
            }
        }),
        named("history-branch", |a, arg| match arg.trim().parse::<u32>() {
            Ok(n) => a.dispatch(Action::HistoryBranch(n)),
            Err(_) => tracing::warn!("history-branch: '{arg}' is not a branch number"),
        }),
        named("audition-node", |a, arg| match arg.trim().parse::<u64>() {
            Ok(id) => a.dispatch(Action::AuditionNode(id)),
            Err(_) => tracing::warn!("audition-node: '{arg}' is not a step id"),
        }),
        // Development aid: `bisect:<start=<good>[@<bad>]|answer=a|answer=b|
        // skip|cancel|setup|hear=a|hear=b|flip|match=on|match=off|
        // found=hear|found=undo|found=go|found=go-before>` (step ids as
        // `history-to-node` takes them; no bad: the project now; skip is
        // "Can't tell").
        named("bisect", |a, arg| {
            let Some(op) = bisect_op(arg.trim()) else {
                return tracing::warn!("bisect: '{arg}' is not understood");
            };
            a.dispatch(Action::Bisect(op));
        }),
        // Development aid: `history-audition:<steps back>` renders the mix
        // as it was that many steps ago beside the mix as it is (the loop,
        // the edit selection or the bars round the playhead).
        named("history-audition", |a, arg| {
            let Ok(back) = arg.trim().parse::<usize>() else {
                return tracing::warn!("history-audition: '{arg}' is not a number of steps");
            };
            let done = a.session.borrow().history_steps().0.len();
            a.dispatch(Action::AuditionHistory(done.saturating_sub(back)));
        }),
        // Development aid: `compare:<now|then|mix|reference|flip|off|match|
        // no-match>` (which side is heard; matching an audition's
        // loudness).
        named("compare", |a, arg| {
            let op = match arg.trim() {
                "now" | "mix" => CompareOp::Hear(false),
                "then" | "reference" => CompareOp::Hear(true),
                "flip" => CompareOp::Flip,
                "off" => CompareOp::Off,
                "match" => CompareOp::MatchLoudness(true),
                "no-match" => CompareOp::MatchLoudness(false),
                _ => {
                    return tracing::warn!(
                        "compare: '{arg}' is not now|then|mix|reference|flip|off|match|no-match"
                    );
                }
            };
            a.dispatch(Action::Compare(op));
        }),
        // Development aid: `add-reference:<path>` imports a file as a
        // reference track (`|` between several).
        named("add-reference", |a, arg| {
            let files = arg
                .split('|')
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .map(std::path::PathBuf::from)
                .collect();
            a.dispatch(Action::Reference(ReferenceOp::Add(files)));
        }),
        // Development aid: `reference:<n|off|follow|follow-here|loop|
        // measure|eq|eq-selected|region=<s>-<s>|whole>`: compare with
        // reference n (1-based), how the compared one plays, measuring it
        // again, sending it to EQ Match (the master's EQ, or the selected
        // track's), the seconds of it that play.
        named("reference", |a, arg| {
            let (current, first, playhead, selected) = {
                let s = a.session.borrow();
                let p = s.project();
                let here = p
                    .timeline
                    .to_samples(s.playhead(), f64::from(p.sample_rate));
                (
                    s.current_reference(),
                    s.references().tracks.first().map(|r| r.id),
                    here,
                    s.selection.tracks.iter().next().copied(),
                )
            };
            let arg = arg.trim();
            if let Ok(n) = arg.parse::<usize>() {
                let id = a
                    .session
                    .borrow()
                    .references()
                    .tracks
                    .get(n.saturating_sub(1))
                    .map(|r| r.id);
                return match id {
                    Some(id) => a.dispatch(Action::Reference(ReferenceOp::Choose(Some(id)))),
                    None => tracing::warn!("reference: no reference {n}"),
                };
            }
            if arg == "off" {
                return a.dispatch(Action::Reference(ReferenceOp::Choose(None)));
            }
            let Some(id) = current.or(first) else {
                return tracing::warn!("reference: there is no reference");
            };
            use faderframe_project::reference::ReferenceSync;
            let rate = f64::from(a.session.borrow().project().sample_rate);
            let op = match arg {
                "follow" => ReferenceOp::SetSync(id, ReferenceSync::Follow { at: 0 }),
                "follow-here" => ReferenceOp::SetSync(
                    id,
                    ReferenceSync::Follow {
                        at: playhead.max(0),
                    },
                ),
                "loop" => ReferenceOp::SetSync(id, ReferenceSync::Loop),
                "measure" => ReferenceOp::Measure(id),
                "eq" => ReferenceOp::SendToEqMatch {
                    reference: id,
                    track: None,
                },
                "eq-selected" => ReferenceOp::SendToEqMatch {
                    reference: id,
                    track: selected,
                },
                "whole" => ReferenceOp::SetRegion(id, None),
                other => match other
                    .strip_prefix("region=")
                    .and_then(|r| r.split_once('-'))
                    .and_then(|(x, y)| Some((x.parse::<f64>().ok()?, y.parse::<f64>().ok()?)))
                {
                    Some((x, y)) => ReferenceOp::SetRegion(
                        id,
                        Some(((x * rate).round() as i64, (y * rate).round() as i64)),
                    ),
                    None => return tracing::warn!("reference: unknown '{other}'"),
                },
            };
            a.dispatch(Action::Reference(op));
        }),
    ]);
}

/// A `bisect:` development action's argument as an operation.
fn bisect_op(arg: &str) -> Option<BisectOp> {
    if let Some(ends) = arg.strip_prefix("start=") {
        let (good, bad) = match ends.split_once('@') {
            Some((g, b)) => (g, Some(b)),
            None => (ends, None),
        };
        let good = good.trim().parse().ok()?;
        let bad = match bad {
            Some(b) => Some(b.trim().parse().ok()?),
            None => None,
        };
        return Some(BisectOp::Start { good, bad });
    }
    Some(match arg {
        "answer=a" => BisectOp::Answer(BisectAnswer::A),
        "answer=b" => BisectOp::Answer(BisectAnswer::B),
        "skip" | "answer=skip" => BisectOp::Answer(BisectAnswer::CantTell),
        "cancel" => BisectOp::Cancel,
        "setup" => BisectOp::Setup,
        "hear=a" => BisectOp::Hear(BisectSide::A),
        "hear=b" => BisectOp::Hear(BisectSide::B),
        "flip" => BisectOp::Flip,
        "match=on" => BisectOp::MatchLoudness(true),
        "match=off" => BisectOp::MatchLoudness(false),
        "found=hear" => BisectOp::AuditionFound,
        "found=undo" => BisectOp::UndoFound,
        "found=go" => BisectOp::GoToFound { after: true },
        "found=go-before" => BisectOp::GoToFound { after: false },
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bisect_arguments_read_as_operations() {
        assert_eq!(
            bisect_op("start=3@9"),
            Some(BisectOp::Start {
                good: 3,
                bad: Some(9)
            })
        );
        assert_eq!(
            bisect_op("start=0"),
            Some(BisectOp::Start { good: 0, bad: None })
        );
        assert_eq!(
            bisect_op("skip"),
            Some(BisectOp::Answer(BisectAnswer::CantTell))
        );
        assert_eq!(
            bisect_op("found=go-before"),
            Some(BisectOp::GoToFound { after: false })
        );
        assert_eq!(bisect_op("start=x"), None);
        assert_eq!(bisect_op("answer=c"), None);
    }
}
