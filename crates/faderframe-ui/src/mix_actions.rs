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
//! * `mix-open:<n>` — the selected finding's n-th experiment that hands
//!   over (a reference heard A/B, EQ Match with it; 1 = the first such);
//! * `track-role:<track>=<role|none>` (a role by its name: `kick`,
//!   `lead-vocal`, `rhythm guitar` …);
//! * `mix-log:x` — log the findings (scripted checks);
//! * `mix-pairs:x` — log the pairs compared for phase and timing (delay,
//!   polarity, confidence, the sum against the energies, onsets) and those
//!   left out;
//! * `mix-dynamics:x` — log every subject's dynamics indicators and width
//!   by region.

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

pub(crate) fn open(a: &Rc<AppState>, arg: &str) {
    let target = {
        let s = a.session.borrow();
        let n = arg.parse::<usize>().unwrap_or(1).max(1);
        s.mix_selected().and_then(|f| {
            f.recommendations
                .iter()
                .enumerate()
                .filter(|(_, r)| r.experiment.hand_off().is_some())
                .nth(n - 1)
                .map(|(i, _)| (f.id, i))
        })
    };
    match target {
        Some((finding, recommendation)) => op(
            a,
            MixOp::Open {
                finding,
                recommendation,
            },
        ),
        None => tracing::warn!("mix-open: the selected finding hands nothing over"),
    }
}

pub(crate) fn reject(a: &Rc<AppState>, _: &str) {
    op(a, MixOp::Reject);
}

/// `mix-undo:x`: take back the newest experiment applied for the selected
/// finding (its Undo).
pub(crate) fn undo(a: &Rc<AppState>, _: &str) {
    let step = {
        let s = a.session.borrow();
        s.mix_selected_id()
            .and_then(|id| s.mix_applied_to(id).last().map(|x| x.step))
    };
    match step {
        Some(step) => op(a, MixOp::Undo(step)),
        None => tracing::warn!("mix-undo: nothing applied for the selected finding"),
    }
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
    let device = |d| s.mix_device_name(d);
    tracing::info!(
        "mix: {} findings, {} · {} render(s), {:.1} s · stale: {:?}",
        r.report.findings.len(),
        r.signal.label(),
        r.renders,
        r.elapsed,
        s.mix_stale()
    );
    for (i, f) in s.mix_findings().iter().enumerate() {
        tracing::info!(
            "mix: {} [{}] {} — severity {:.2}, confidence {:.2}, {} experiments{}",
            i + 1,
            f.category().label(),
            f.title_named(&name, &device),
            f.severity,
            f.confidence,
            f.recommendations.len(),
            if s.mix_finding_stale(f.id) {
                " (stale)"
            } else {
                ""
            }
        );
    }
    if let Some(c) = s.mix_reference() {
        tracing::info!(
            "mix: reference played at {:+.1} dB to match the mix's loudness",
            c.match_db
        );
    }
}

pub(crate) fn pairs(a: &Rc<AppState>, _: &str) {
    let s = a.session.borrow();
    let Some(r) = s.mix_results() else {
        tracing::info!("mix: no results ({:?})", s.mix_status());
        return;
    };
    let name = |t| s.mix_track_name(t);
    let db = |v: Option<f64>| v.map_or("—".to_string(), |v| format!("{v:+.1} dB"));
    tracing::info!(
        "mix: {} pairs compared ({} measured, {} reused, {} left out)",
        r.report.details.pairs.len(),
        r.extra.pairs_measured,
        r.extra.pairs_reused,
        r.extra.pairs_skipped
    );
    for p in &r.report.details.pairs {
        let lag = p.lag.map_or("none".to_string(), |l| {
            format!(
                "{:+.3} ms{} (confidence {:.2}, clarity {:.2}{})",
                l.ms,
                if l.inverted { " inverted" } else { "" },
                l.confidence,
                l.clarity,
                if l.related { "" } else { ", unrelated" }
            )
        });
        tracing::info!(
            "mix: pair {} × {} — lag {lag}; sum {} now, {} corrected, {} mono; onsets {} \
             paired, median {:?} ms, spread {:?} ms",
            name(p.a),
            name(p.b),
            db(p.sum_db),
            db(p.corrected_db),
            db(p.mono_db),
            p.onsets_paired,
            p.onset_median_ms,
            p.onset_spread_ms
        );
    }
    for (x, y, why) in &r.report.details.skipped_pairs {
        tracing::info!("mix: pair {} × {} left out: {why}", name(*x), name(*y));
    }
}

pub(crate) fn dynamics(a: &Rc<AppState>, _: &str) {
    use faderframe_mix_intelligence::Subject;
    let s = a.session.borrow();
    let Some(r) = s.mix_results() else {
        tracing::info!("mix: no results ({:?})", s.mix_status());
        return;
    };
    let label = |x: Subject| match x {
        Subject::Track(t) | Subject::Pair { target: t, .. } | Subject::Tracks { a: t, .. } => {
            s.mix_track_name(t)
        }
        Subject::Master(_) => "the master".to_string(),
    };
    for d in &r.report.details.dynamics {
        let x = &d.dynamics;
        tracing::info!(
            "mix: dynamics {} — crest {:?} dB, PLR {:?}, LRA {:?} LU, short-term {:?} LUFS, TSR \
             {:?} dB over {} events, source TSR {:?}, flattening {:?}",
            label(d.subject),
            x.crest_db,
            x.plr_db,
            x.lra_lu,
            x.short_term,
            x.tsr_db,
            x.events,
            d.source.as_ref().and_then(|s| s.tsr_db),
            d.flattening.map(|f| f.tsr_change_db)
        );
    }
    for w in &r.report.details.width {
        tracing::info!(
            "mix: width {} — side − mid by region {:?} dB, mono change {:?} dB",
            label(w.subject),
            w.broad_width_db,
            w.broad_mono_db
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
