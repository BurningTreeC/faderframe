//! Development aid for the interactive score (`FADERFRAME_STARTUP_ACTIONS`):
//! `adaptive:<op>` with
//!
//! * `from-sections` / `from-scenes` — a state for every section or scene;
//! * `state=<n|name>` — go to a state (the preview starts there when it is
//!   not running); `preview=<on|off>`;
//! * `rule=<from>><to>@<sync>[/<fade out ms>[/<fade in ms>]]` — a rule
//!   (`*` = any state; sync as `SyncPoint::parse` reads it: `now`, `beat`,
//!   `bar`, `4bars`, `marker`, `cue`, `cue=Name`, `exit`);
//! * `layer=<name>=<track>[|<track>…]`, `param=<name>` (a parameter),
//!   `value=<parameter>=<v>` (its value in the preview);
//! * `stinger=<track>@<scene n>` (a stinger of that slot), `sting=<n|name>`
//!   (play it);
//! * `export=<dir>` — export the score there; `log` — log the preview.

use crate::state::AppState;
use faderframe_project::interactive::{RuleEnd, SyncPoint};
use faderframe_project::scenes::FadeLength;
use faderframe_session::Action;
use faderframe_session::adaptive::AdaptiveOp;
use std::rc::Rc;

fn op(a: &Rc<AppState>, op: AdaptiveOp) {
    a.dispatch(Action::Adaptive(op));
}

fn state_end(a: &Rc<AppState>, text: &str) -> Option<RuleEnd> {
    if text.trim() == "*" {
        return Some(RuleEnd::Any);
    }
    let s = a.session.borrow();
    s.interactive()
        .state_named(text)
        .map(|st| RuleEnd::State { state: st.id })
}

pub(crate) fn adaptive(a: &Rc<AppState>, arg: &str) {
    let (verb, rest) = arg.split_once('=').unwrap_or((arg, ""));
    match verb {
        "from-sections" => op(a, AdaptiveOp::StatesFromSections),
        "from-scenes" => op(a, AdaptiveOp::StatesFromScenes),
        "preview" => op(a, AdaptiveOp::Preview(rest != "off")),
        "state" => {
            let id = a
                .session
                .borrow()
                .interactive()
                .state_named(rest)
                .map(|s| s.id);
            match id {
                Some(id) => op(a, AdaptiveOp::Go(id)),
                None => tracing::warn!("adaptive: no state '{rest}'"),
            }
        }
        "rule" => {
            let (ends, tail) = rest.split_once('@').unwrap_or((rest, "bar"));
            let mut parts = tail.split('/');
            let sync = parts.next().and_then(SyncPoint::parse).unwrap_or_default();
            let ms = |p: Option<&str>| {
                p.and_then(|t| t.trim().parse::<f64>().ok())
                    .map_or(FadeLength::Instant, |ms| FadeLength::Millis { ms })
                    .clamped()
            };
            let (fade_out, fade_in) = (ms(parts.next()), ms(parts.next()));
            let Some((from, to)) = ends.split_once('>') else {
                tracing::warn!("adaptive: rule=<from>><to>@<sync>, got '{rest}'");
                return;
            };
            let (Some(from), Some(to)) = (state_end(a, from), state_end(a, to)) else {
                tracing::warn!("adaptive: no such state in '{ends}'");
                return;
            };
            op(a, AdaptiveOp::AddRule { from, to });
            let rule = a.session.borrow().interactive().rules.last().cloned();
            if let Some(mut r) = rule {
                r.sync = sync;
                r.fade_out = fade_out;
                r.fade_in = fade_in;
                op(a, AdaptiveOp::SetRule(r));
            }
        }
        "layer" => {
            let (name, tracks) = rest.split_once('=').unwrap_or((rest, ""));
            let ids: Vec<_> = {
                let s = a.session.borrow();
                tracks
                    .split('|')
                    .filter_map(|n| s.project().tracks.iter().find(|t| t.name == n.trim()))
                    .map(|t| t.id)
                    .collect()
            };
            op(
                a,
                AdaptiveOp::AddLayer {
                    name: Some(name.to_string()),
                    tracks: ids,
                },
            );
        }
        "param" => op(
            a,
            AdaptiveOp::AddParameter {
                name: (!rest.is_empty()).then(|| rest.to_string()),
            },
        ),
        "value" => {
            let (name, v) = rest.split_once('=').unwrap_or((rest, "0"));
            let id = a
                .session
                .borrow()
                .interactive()
                .parameters
                .iter()
                .find(|p| p.name.eq_ignore_ascii_case(name))
                .map(|p| p.id);
            match (id, v.trim().parse::<f32>()) {
                (Some(parameter), Ok(value)) => op(a, AdaptiveOp::SetValue { parameter, value }),
                _ => tracing::warn!("adaptive: value=<parameter>=<v>, got '{rest}'"),
            }
        }
        "stinger" => {
            let (track, scene) = rest.split_once('@').unwrap_or((rest, "1"));
            let slot = {
                let s = a.session.borrow();
                let p = s.project();
                let track = p.tracks.iter().find(|t| t.name == track).map(|t| t.id);
                let scene = scene
                    .trim()
                    .parse::<usize>()
                    .ok()
                    .and_then(|n| n.checked_sub(1))
                    .and_then(|i| p.launcher.scenes.get(i))
                    .map(|s| s.id);
                track
                    .zip(scene)
                    .map(|(track, scene)| faderframe_project::launcher::SlotKey { track, scene })
            };
            match slot {
                Some(slot) => op(a, AdaptiveOp::AddStinger { slot }),
                None => tracing::warn!("adaptive: stinger=<track>@<scene n>, got '{rest}'"),
            }
        }
        "sting" => {
            let id = {
                let s = a.session.borrow();
                let list = &s.interactive().stingers;
                match rest.trim().parse::<usize>() {
                    Ok(n) => n.checked_sub(1).and_then(|i| list.get(i)),
                    Err(_) => list.iter().find(|x| x.name.eq_ignore_ascii_case(rest)),
                }
                .map(|x| x.id)
            };
            match id {
                Some(id) => op(a, AdaptiveOp::PlayStinger(id)),
                None => tracing::warn!("adaptive: no stinger '{rest}'"),
            }
        }
        "export" => op(a, AdaptiveOp::Export(std::path::PathBuf::from(rest))),
        "log" => {
            let s = a.session.borrow();
            tracing::info!(
                "adaptive: {} states, {} rules; {:?}",
                s.interactive().states.len(),
                s.interactive().rules.len(),
                s.adaptive_status()
            );
        }
        _ => tracing::warn!("adaptive: unknown '{arg}'"),
    }
}
