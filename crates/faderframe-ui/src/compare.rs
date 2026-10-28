//! A/B listening's actions: the Reference view, the flip between the mix
//! and what it is compared with (Ctrl+Alt+A: "Then" of an audition of the
//! undo history, or a reference track), and the development aids that
//! drive both from a script.

use crate::state::AppState;
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
