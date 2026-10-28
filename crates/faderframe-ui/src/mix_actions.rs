//! Development aids for Mix Intelligence (`FADERFRAME_STARTUP_ACTIONS`):
//!
//! * `mix-analyse:<project|selection|tracks>[/source]` — start an analysis
//!   (processed unless `/source`); the results come in later (`wait:`);
//! * `mix-cancel:x`;
//! * `mix-finding:<n>` — select the n-th listed finding (1 = the most
//!   relevant) and show it on the arranger;
//! * `mix-audition:<n>` — audition the selected finding's n-th experiment;
//!   `mix-compare:<a|b>`, `mix-match:<on|off>`, `mix-apply:x` (the one
//!   auditioned, else the first that can be applied), `mix-reject:x`,
//!   `mix-dismiss:x`;
//! * `track-role:<track>=<role|none>` (a role by its name: `kick`,
//!   `lead-vocal`, `rhythm guitar` …);
//! * `mix-log:x` — log the findings (scripted checks).

use crate::state::AppState;
use faderframe_core::TrackRole;
use faderframe_mix_intelligence::SignalPoint;
use faderframe_project::Command;
use faderframe_session::Action;
use faderframe_session::mix::{MixOp, MixScope};
use std::rc::Rc;

fn op(a: &Rc<AppState>, op: MixOp) {
    a.dispatch(Action::Mix(op));
}

pub(crate) fn analyse(a: &Rc<AppState>, arg: &str) {
    let (scope, signal) = arg.split_once('/').unwrap_or((arg, ""));
    let scope = match scope {
        "selection" => MixScope::Selection,
        "tracks" => MixScope::SelectedTracks,
        _ => MixScope::Project,
    };
    let signal = if signal == "source" {
        SignalPoint::Source
    } else {
        SignalPoint::Processed
    };
    op(a, MixOp::Analyse { scope, signal });
}

pub(crate) fn cancel(a: &Rc<AppState>, _: &str) {
    op(a, MixOp::Cancel);
}

pub(crate) fn finding(a: &Rc<AppState>, arg: &str) {
    let id = {
        let s = a.session.borrow();
        let n = arg.parse::<usize>().unwrap_or(1).max(1);
        s.mix_findings().get(n - 1).map(|f| f.id)
    };
    match id {
        Some(id) => {
            op(a, MixOp::Select(Some(id)));
            op(a, MixOp::Show(id));
        }
        None => tracing::warn!("mix-finding: no finding {arg}"),
    }
}

pub(crate) fn audition(a: &Rc<AppState>, arg: &str) {
    let finding = a.session.borrow().mix_selected_id();
    let n = arg.parse::<usize>().unwrap_or(1).max(1);
    match finding {
        Some(finding) => op(
            a,
            MixOp::Audition {
                finding,
                recommendation: n - 1,
            },
        ),
        None => tracing::warn!("mix-audition: no finding selected"),
    }
}

pub(crate) fn compare(a: &Rc<AppState>, arg: &str) {
    op(a, MixOp::Compare(arg != "a"));
}

pub(crate) fn matching(a: &Rc<AppState>, arg: &str) {
    op(a, MixOp::LoudnessMatch(arg != "off"));
}

pub(crate) fn apply(a: &Rc<AppState>, _: &str) {
    let target = {
        let s = a.session.borrow();
        s.mix_audition()
            .map(|x| (x.finding, x.recommendation))
            .or_else(|| {
                let f = s.mix_selected()?;
                let i = (0..f.recommendations.len()).find(|i| s.mix_can_try(f.id, *i).is_ok())?;
                Some((f.id, i))
            })
    };
    match target {
        Some((finding, recommendation)) => op(
            a,
            MixOp::Apply {
                finding,
                recommendation,
            },
        ),
        None => tracing::warn!("mix-apply: nothing to apply"),
    }
}

pub(crate) fn reject(a: &Rc<AppState>, _: &str) {
    op(a, MixOp::Reject);
}

pub(crate) fn dismiss(a: &Rc<AppState>, _: &str) {
    let id = a.session.borrow().mix_selected_id();
    if let Some(id) = id {
        op(a, MixOp::Dismiss(id));
    }
}

/// A role by any of its names ("lead-vocal", "Lead Vocal", "pads" …).
fn role_named(name: &str) -> Option<TrackRole> {
    let key = |s: &str| {
        s.chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect::<String>()
    };
    let want = key(name);
    TrackRole::ALL
        .into_iter()
        .find(|r| key(r.label()) == want || key(r.short()) == want)
}

pub(crate) fn track_role(a: &Rc<AppState>, arg: &str) {
    let Some((track, role)) = arg.split_once('=') else {
        return;
    };
    let id = a
        .session
        .borrow()
        .project()
        .tracks
        .iter()
        .find(|t| t.name == track)
        .map(|t| t.id);
    let role = if role == "none" {
        None
    } else {
        match role_named(role) {
            Some(r) => Some(r),
            None => {
                tracing::warn!("track-role: no role '{role}'");
                return;
            }
        }
    };
    match id {
        Some(track) => a.dispatch(Action::Edit(Command::SetTrackRole { track, role })),
        None => tracing::warn!("track-role: no track '{track}'"),
    }
}

pub(crate) fn log(a: &Rc<AppState>, _: &str) {
    let s = a.session.borrow();
    let Some(r) = s.mix_results() else {
        tracing::info!("mix: no results ({:?})", s.mix_status());
        return;
    };
    let name = |t| s.mix_track_name(t);
    tracing::info!(
        "mix: {} findings, {} · stale: {:?}",
        r.report.findings.len(),
        r.signal.label(),
        s.mix_stale()
    );
    for (i, f) in s.mix_findings().iter().enumerate() {
        tracing::info!(
            "mix: {} {} — severity {:.2}, confidence {:.2}, {} experiments",
            i + 1,
            f.title(&name),
            f.severity,
            f.confidence,
            f.recommendations.len()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_are_found_by_any_of_their_names() {
        assert_eq!(role_named("lead-vocal"), Some(TrackRole::LeadVocal));
        assert_eq!(role_named("Lead Vocal"), Some(TrackRole::LeadVocal));
        assert_eq!(role_named("pads"), Some(TrackRole::Pads));
        assert_eq!(role_named("rhythm_guitar"), Some(TrackRole::RhythmGuitar));
        assert_eq!(role_named("kazoo"), None);
    }
}
