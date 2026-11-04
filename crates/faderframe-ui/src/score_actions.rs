//! The score editor from scripts (`FADERFRAME_STARTUP_ACTIONS`; the View
//! menu's Score and the arranger's "Edit as Notation" entries open it):
//!
//! * `show-score:<track>[|<track>…]` — those tracks' MIDI clips, a staff
//!   each; `show-score:clip=<track>` its first clip; `show-score:editor`
//!   the clip in the piano roll;
//! * `score-enter:<text>` — keys typed (`faderframe_session::score::parse_keys`:
//!   `n` note input, `a`–`g` notes (upper case: added to the chord),
//!   `1 2 4 8 16` values, `.` dot, `t` triplet, `r` rest, `~` tie, `^`/`v`
//!   semitones, `'`/`,` octaves, `j` respell, `v1`/`v2` voices, `>`/`<`,
//!   `x` delete, `esc`), e.g. `score-enter:n 4 c d e f 2 g G`;
//! * `score-select:<n>[|<n>…]` — the score's notes by place (from 0, in
//!   time order);
//! * `score-layout:<pages|strip>`, `score-lyrics:<words>` (from the first
//!   selected note), `score-chord:<symbol>` (at the selection or cursor),
//!   `score-dynamic:<ppp…fff|none>`, `score-mark:<staccato|accent|tenuto|marcato>`
//!   (toggled on the selection), `score-display:<auto|straight|triplets>[/legato|/exact]`,
//!   `score-quantize:x`, `export-score-to:<path.pdf|.musicxml>`.

use crate::state::AppState;
use faderframe_core::NoteId;
use faderframe_project::notation::{Dynamic, Mark, ScoreGrid};
use faderframe_session::score::{ScoreLayout, ScoreOf, ScoreOp, ScoreSettings, parse_keys};
use faderframe_session::{Action, SelectMode};
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
        named("show-score", show),
        named("score-enter", enter),
        named("score-select", select),
        named("score-layout", layout),
        named("score-lyrics", lyrics),
        named("score-chord", chord),
        named("score-dynamic", dynamic),
        named("score-mark", mark),
        named("score-display", display),
        named("score-quantize", quantize),
        named("export-score-to", export),
    ]);
}

fn op(a: &Rc<AppState>, op: ScoreOp) {
    a.dispatch(Action::Score(op));
}

fn show(a: &Rc<AppState>, arg: &str) {
    let arg = arg.trim();
    let of = {
        let s = a.session.borrow();
        let track = |name: &str| s.project().tracks.iter().find(|t| t.name == name.trim());
        if arg.is_empty() || arg == "editor" {
            Some(ScoreOf::Editor)
        } else if let Some(name) = arg.strip_prefix("clip=") {
            track(name)
                .and_then(|t| {
                    s.project()
                        .clips_of(t.id)
                        .into_iter()
                        .find(|c| c.as_midi().is_some())
                })
                .map(|c| ScoreOf::Clip(c.id))
        } else {
            let tracks: Vec<_> = arg
                .split('|')
                .filter_map(|n| track(n).map(|t| t.id))
                .collect();
            (!tracks.is_empty()).then_some(ScoreOf::Tracks(tracks))
        }
    };
    match of {
        Some(of) => op(a, ScoreOp::Show(of)),
        None => tracing::warn!("show-score: nothing to show for '{arg}'"),
    }
}

fn enter(a: &Rc<AppState>, arg: &str) {
    for k in parse_keys(arg) {
        op(a, ScoreOp::Key(k));
    }
    let s = a.session.borrow();
    if let Some(doc) = s.score() {
        tracing::info!(
            "score-enter: {} notes written, cursor {:?}",
            doc.written.len(),
            s.score_cursor()
        );
    }
}

/// The score's notes in time order (a chord's from the lowest).
fn notes_in_order(a: &Rc<AppState>) -> Vec<NoteId> {
    let s = a.session.borrow();
    let Some(doc) = s.score() else {
        return Vec::new();
    };
    let mut w = doc.written.clone();
    w.sort_by(|x, y| x.start.total_cmp(&y.start).then(x.part.cmp(&y.part)));
    w.into_iter().map(|w| NoteId(w.id)).collect()
}

fn select(a: &Rc<AppState>, arg: &str) {
    let all = notes_in_order(a);
    let notes: Vec<NoteId> = arg
        .split('|')
        .filter_map(|n| n.trim().parse::<usize>().ok())
        .filter_map(|i| all.get(i).copied())
        .collect();
    a.dispatch(Action::SelectNotes {
        notes,
        mode: SelectMode::Replace,
    });
}

fn selected(a: &Rc<AppState>) -> Vec<NoteId> {
    let order = notes_in_order(a);
    let s = a.session.borrow();
    order
        .into_iter()
        .filter(|n| s.selection.notes.contains(n))
        .collect()
}

fn layout(a: &Rc<AppState>, arg: &str) {
    let settings = a.session.borrow().score_settings();
    let layout = match arg.trim() {
        "strip" => ScoreLayout::Strip,
        _ => ScoreLayout::Pages,
    };
    op(a, ScoreOp::Settings(ScoreSettings { layout, ..settings }));
}

fn lyrics(a: &Rc<AppState>, arg: &str) {
    let from = selected(a)
        .first()
        .copied()
        .or_else(|| notes_in_order(a).first().copied());
    match from {
        Some(from) => op(
            a,
            ScoreOp::Lyrics {
                from,
                text: arg.to_string(),
            },
        ),
        None => tracing::warn!("score-lyrics: the score has no notes"),
    }
}

fn chord(a: &Rc<AppState>, arg: &str) {
    let at = {
        let s = a.session.borrow();
        let Some(doc) = s.score() else {
            return tracing::warn!("score-chord: no score");
        };
        s.selection
            .notes
            .iter()
            .filter_map(|n| doc.written(*n))
            .map(|w| w.start)
            .reduce(f64::min)
            .map(faderframe_timeline::MusicalTime::from_quarters)
            .or_else(|| s.score_cursor().map(|c| c.at))
            .unwrap_or_else(|| doc.start())
    };
    op(
        a,
        ScoreOp::ChordSymbol {
            at,
            text: arg.to_string(),
        },
    );
}

fn dynamic(a: &Rc<AppState>, arg: &str) {
    let dynamic = match arg.trim() {
        "none" => None,
        d => match Dynamic::of_name(d) {
            Some(d) => Some(d),
            None => return tracing::warn!("score-dynamic: no dynamic '{d}'"),
        },
    };
    op(
        a,
        ScoreOp::Dynamic {
            notes: selected(a),
            dynamic,
        },
    );
}

fn mark(a: &Rc<AppState>, arg: &str) {
    let Some(mark) = Mark::ALL
        .into_iter()
        .find(|m| m.label().eq_ignore_ascii_case(arg.trim()))
    else {
        return tracing::warn!("score-mark: no articulation '{arg}'");
    };
    let notes = selected(a);
    let on = {
        let s = a.session.borrow();
        let Some(doc) = s.score() else { return };
        !notes.iter().all(|n| {
            doc.clips
                .get(n)
                .is_some_and(|c| mark.of(&s.project().clip_notation(*c).note(*n).marks))
        })
    };
    op(a, ScoreOp::Mark { notes, mark, on });
}

fn display(a: &Rc<AppState>, arg: &str) {
    let (grid, rest) = arg.split_once('/').unwrap_or((arg, ""));
    let grid = match grid.trim() {
        "straight" => ScoreGrid::Straight,
        "triplets" => ScoreGrid::Triplets,
        _ => ScoreGrid::Auto,
    };
    let clips = {
        let s = a.session.borrow();
        let Some(doc) = s.score() else { return };
        doc.parts
            .iter()
            .flat_map(|p| p.clips.iter().copied())
            .collect()
    };
    op(
        a,
        ScoreOp::Display {
            clips,
            grid,
            legato: rest.trim() != "exact",
        },
    );
}

fn quantize(a: &Rc<AppState>, _arg: &str) {
    op(a, ScoreOp::Quantize(selected(a)));
}

fn export(a: &Rc<AppState>, arg: &str) {
    op(a, ScoreOp::Export(std::path::PathBuf::from(arg.trim())));
}
