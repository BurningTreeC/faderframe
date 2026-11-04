//! The Interactive Music view: the interactive score (music for a game;
//! see `faderframe_session::adaptive`) edited and played.
//!
//! The header starts and stops the preview, adds states (from sections or
//! scenes), rules, layers (of the selected tracks), parameters and
//! stingers (launcher slots), and exports the score into a folder; the
//! status on its right says which state plays, which one waits and when.
//! Below, three columns: the states (a click goes there while the
//! preview plays — a double click starts it there — the playing one marked
//! and the waiting one outlined; a right click edits loop, entry, cues,
//! once/then, start, name), the rules in their precedence (a click opens
//! everything a rule sets: ends, sync point, fades, transition segment,
//! entry, order), and the layers, the parameters (sliders that move the
//! layers in the preview; a right click sets each layer's curve) and the
//! stingers (a click plays one on its sync point).

#![forbid(unsafe_code)]

use faderframe_core::{MusicStateId, ScoreParamId};
use faderframe_project::interactive::{
    Entry, LayerCurve, MusicState, RuleEnd, StateCue, StateSource, SyncPoint, TransitionRule,
};
use faderframe_project::scenes::FadeLength;
use faderframe_session::adaptive::AdaptiveOp;
use faderframe_session::{Action, Session};
use faderframe_timeline::MusicalTime;
use faderframe_ui_canvas::{
    CanvasView, Cursor, EventCx, FileChoice, HostRequest, MenuItem, Paint, Painter, Point,
    PointerButton, Rect, Size, TextStyle, Theme, ViewEvent,
};

const HEADER_H: f32 = 38.0;
const FOOTER_H: f32 = 22.0;
const ROW_H: f32 = 34.0;
const SECTION_H: f32 = 22.0;
const GAP: f32 = 8.0;

/// Fade lengths the menus offer.
const FADES: [FadeLength; 7] = [
    FadeLength::Instant,
    FadeLength::Millis { ms: 250.0 },
    FadeLength::Millis { ms: 500.0 },
    FadeLength::Millis { ms: 1000.0 },
    FadeLength::Millis { ms: 2000.0 },
    FadeLength::Beats { beats: 1.0 },
    FadeLength::Beats { beats: 4.0 },
];

/// Entry cues the state menu offers (bars into the state).
const ENTRIES: [f64; 5] = [0.0, 1.0, 2.0, 4.0, 8.0];

fn add_op(action: AdaptiveOp) -> Action {
    Action::Adaptive(action)
}

/// What a click hits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Preview,
    AddState,
    AddRule,
    AddLayer,
    AddParameter,
    AddStinger,
    Export,
    State(usize),
    Rule(usize),
    Layer(usize),
    Parameter(usize),
    Stinger(usize),
}

pub struct AdaptiveView {
    theme: Theme,
    hover: Option<Hit>,
    /// A parameter's slider held.
    dragging: Option<ScoreParamId>,
}

impl AdaptiveView {
    pub fn new(theme: Theme) -> Self {
        Self {
            theme,
            hover: None,
            dragging: None,
        }
    }

    /// The header's buttons.
    pub fn buttons(size: Size, model: &Session) -> Vec<(Hit, Rect, String)> {
        let previewing = model.adaptive_status().previewing;
        let y = 8.0;
        let h = HEADER_H - 16.0;
        let mut x = 150.0;
        let mut out = Vec::new();
        let list = [
            (
                Hit::Preview,
                if previewing {
                    "■ Stop".to_string()
                } else {
                    "▶ Preview".to_string()
                },
                84.0,
            ),
            (Hit::AddState, "+ State ▾".into(), 76.0),
            (Hit::AddRule, "+ Rule ▾".into(), 70.0),
            (Hit::AddLayer, "+ Layer".into(), 64.0),
            (Hit::AddParameter, "+ Parameter".into(), 92.0),
            (Hit::AddStinger, "+ Stinger ▾".into(), 86.0),
            (Hit::Export, "Export…".into(), 70.0),
        ];
        for (hit, label, w) in list {
            out.push((hit, Rect::new(x, y, w, h), label));
            x += w + 6.0;
        }
        out.retain(|(_, r, _)| r.right() <= size.w - 4.0);
        out
    }

    fn body(size: Size) -> Rect {
        Rect::new(
            0.0,
            HEADER_H,
            size.w,
            (size.h - HEADER_H - FOOTER_H).max(0.0),
        )
    }

    /// The three columns: states, rules, the rest.
    pub fn columns(size: Size) -> [Rect; 3] {
        let b = Self::body(size);
        let w = (b.w - 4.0 * GAP) / 3.0;
        let col = |i: f32| {
            Rect::new(
                b.x + GAP + i * (w + GAP),
                b.y + GAP,
                w.max(0.0),
                (b.h - 2.0 * GAP).max(0.0),
            )
        };
        [col(0.0), col(1.0), col(2.0)]
    }

    pub fn state_rect(i: usize, size: Size) -> Rect {
        let c = Self::columns(size)[0];
        Rect::new(c.x, c.y + SECTION_H + i as f32 * ROW_H, c.w, ROW_H - 2.0)
    }

    pub fn rule_rect(i: usize, size: Size) -> Rect {
        let c = Self::columns(size)[1];
        Rect::new(c.x, c.y + SECTION_H + i as f32 * ROW_H, c.w, ROW_H - 2.0)
    }

    /// The right column's rows: layers, parameters, stingers, each list
    /// under its heading.
    pub fn right_rows(size: Size, model: &Session) -> Vec<(Hit, Rect)> {
        let c = Self::columns(size)[2];
        let score = model.interactive();
        let mut y = c.y + SECTION_H;
        let mut out = Vec::new();
        for i in 0..score.layers.len() {
            out.push((Hit::Layer(i), Rect::new(c.x, y, c.w, ROW_H - 2.0)));
            y += ROW_H;
        }
        y += SECTION_H;
        for i in 0..score.parameters.len() {
            out.push((Hit::Parameter(i), Rect::new(c.x, y, c.w, ROW_H - 2.0)));
            y += ROW_H;
        }
        y += SECTION_H;
        for i in 0..score.stingers.len() {
            out.push((Hit::Stinger(i), Rect::new(c.x, y, c.w, ROW_H - 2.0)));
            y += ROW_H;
        }
        out.retain(|(_, r)| r.bottom() <= c.bottom() + 0.5);
        out
    }

    pub fn hit(&self, pos: Point, size: Size, model: &Session) -> Option<Hit> {
        if let Some((h, _, _)) = Self::buttons(size, model)
            .into_iter()
            .find(|(_, r, _)| r.contains(pos))
        {
            return Some(h);
        }
        let score = model.interactive();
        if let Some(i) = (0..score.states.len()).find(|i| Self::state_rect(*i, size).contains(pos))
        {
            return Some(Hit::State(i));
        }
        if let Some(i) = (0..score.rules.len()).find(|i| Self::rule_rect(*i, size).contains(pos)) {
            return Some(Hit::Rule(i));
        }
        Self::right_rows(size, model)
            .into_iter()
            .find(|(_, r)| r.contains(pos))
            .map(|(h, _)| h)
    }

    /// The slider of a parameter's row.
    fn slider(row: Rect) -> Rect {
        Rect::new(
            row.x + row.w * 0.4,
            row.y + 8.0,
            row.w * 0.6 - 10.0,
            row.h - 16.0,
        )
    }

    fn value_at(x: f32, row: Rect, min: f32, max: f32) -> f32 {
        let s = Self::slider(row);
        let t = ((x - s.x) / s.w.max(1.0)).clamp(0.0, 1.0);
        min + t * (max - min)
    }

    // --- menus ---------------------------------------------------------

    fn add_state_menu(model: &Session) -> Vec<MenuItem<Action>> {
        let p = model.project();
        let mut items = vec![
            MenuItem::new(
                "A State for Every Section",
                add_op(AdaptiveOp::StatesFromSections),
            ),
            MenuItem::new(
                "A State for Every Scene",
                add_op(AdaptiveOp::StatesFromScenes),
            ),
        ];
        let sections: Vec<MenuItem<Action>> = p
            .sections
            .iter()
            .map(|s| {
                MenuItem::new(
                    s.name.clone(),
                    add_op(AdaptiveOp::AddState(StateSource::Section { section: s.id })),
                )
            })
            .collect();
        if !sections.is_empty() {
            items.push(MenuItem::submenu("Section", sections).separated());
        }
        let scenes: Vec<MenuItem<Action>> = p
            .launcher
            .scenes
            .iter()
            .enumerate()
            .filter(|(_, s)| p.scene_length(s.id).is_some())
            .map(|(i, s)| {
                let label = if s.name.is_empty() {
                    format!("Scene {}", i + 1)
                } else {
                    s.name.clone()
                };
                MenuItem::new(
                    label,
                    add_op(AdaptiveOp::AddState(StateSource::Scene { scene: s.id })),
                )
            })
            .collect();
        if !scenes.is_empty() {
            items.push(MenuItem::submenu("Launcher Scene", scenes));
        }
        items
    }

    fn end_items(
        model: &Session,
        now: RuleEnd,
        make: impl Fn(RuleEnd) -> Action,
    ) -> Vec<MenuItem<Action>> {
        let mut items =
            vec![MenuItem::new("Any State", make(RuleEnd::Any)).checked(now == RuleEnd::Any)];
        for (i, s) in model.interactive().states.iter().enumerate() {
            let end = RuleEnd::State { state: s.id };
            let item = MenuItem::new(s.name.clone(), make(end)).checked(now == end);
            items.push(if i == 0 { item.separated() } else { item });
        }
        items
    }

    fn add_rule_menu(model: &Session) -> Vec<MenuItem<Action>> {
        let mut items = vec![MenuItem::new(
            "Any State → Any State",
            add_op(AdaptiveOp::AddRule {
                from: RuleEnd::Any,
                to: RuleEnd::Any,
            }),
        )];
        let states = &model.interactive().states;
        for s in states {
            let from = RuleEnd::State { state: s.id };
            let mut sub = vec![MenuItem::new(
                "Any State",
                add_op(AdaptiveOp::AddRule {
                    from,
                    to: RuleEnd::Any,
                }),
            )];
            for d in states.iter().filter(|d| d.id != s.id) {
                sub.push(MenuItem::new(
                    d.name.clone(),
                    add_op(AdaptiveOp::AddRule {
                        from,
                        to: RuleEnd::State { state: d.id },
                    }),
                ));
            }
            items.push(MenuItem::submenu(format!("From ‘{}’ to", s.name), sub));
        }
        items
    }

    fn add_stinger_menu(model: &Session) -> Vec<MenuItem<Action>> {
        let p = model.project();
        let mut items: Vec<MenuItem<Action>> = p
            .launcher
            .slots
            .iter()
            .filter_map(|(key, clip)| {
                let clip = p.clips.get(clip)?;
                let track = p.track(key.track)?;
                let scene = p.launcher.scene_index(key.scene)? + 1;
                Some(MenuItem::new(
                    format!("{} ({}, scene {scene})", clip.name, track.name),
                    add_op(AdaptiveOp::AddStinger { slot: *key }),
                ))
            })
            .collect();
        if items.is_empty() {
            items.push(MenuItem::disabled("No launcher clips: a stinger plays one"));
        }
        items
    }

    fn sync_items(
        now: &SyncPoint,
        cues: &[StateCue],
        make: impl Fn(SyncPoint) -> Action,
    ) -> Vec<MenuItem<Action>> {
        let mut items: Vec<MenuItem<Action>> = SyncPoint::COMMON
            .iter()
            .map(|s| MenuItem::new(s.label(), make(s.clone())).checked(s == now))
            .collect();
        let any = SyncPoint::Cue {
            name: String::new(),
        };
        items.push(MenuItem::new(any.label(), make(any.clone())).checked(*now == any));
        let mut names: Vec<&str> = cues.iter().map(|c| c.name.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        for (i, n) in names.into_iter().enumerate() {
            let s = SyncPoint::Cue { name: n.into() };
            let item = MenuItem::new(s.label(), make(s.clone())).checked(*now == s);
            items.push(if i == 0 { item.separated() } else { item });
        }
        items
    }

    fn fade_items(now: FadeLength, make: impl Fn(FadeLength) -> Action) -> Vec<MenuItem<Action>> {
        FADES
            .iter()
            .map(|f| MenuItem::new(f.label(), make(*f)).checked(now.clamped() == *f))
            .collect()
    }

    /// Everything a rule sets.
    fn rule_menu(model: &Session, rule: &TransitionRule, index: usize) -> Vec<MenuItem<Action>> {
        let set = |f: &dyn Fn(&mut TransitionRule)| {
            let mut r = rule.clone();
            f(&mut r);
            add_op(AdaptiveOp::SetRule(r))
        };
        let score = model.interactive();
        let cues: Vec<StateCue> = match rule.from.state() {
            Some(s) => score.state(s).map(|s| s.cues.clone()).unwrap_or_default(),
            None => score.states.iter().flat_map(|s| s.cues.clone()).collect(),
        };
        let mut segment =
            vec![MenuItem::new("None", set(&|r| r.segment = None)).checked(rule.segment.is_none())];
        for (i, s) in score.states.iter().enumerate() {
            if !matches!(s.source, StateSource::Section { .. }) {
                continue;
            }
            let id = s.id;
            let item = MenuItem::new(s.name.clone(), set(&|r| r.segment = Some(id)))
                .checked(rule.segment == Some(id));
            segment.push(if i == 0 { item.separated() } else { item });
        }
        let entries: Vec<MenuItem<Action>> = Entry::ALL
            .iter()
            .map(|e| MenuItem::new(e.label(), set(&|r| r.entry = *e)).checked(rule.entry == *e))
            .collect();
        let mut items = vec![
            MenuItem::submenu(
                "From",
                Self::end_items(model, rule.from, |e| set(&|r| r.from = e)),
            ),
            MenuItem::submenu(
                "To",
                Self::end_items(model, rule.to, |e| set(&|r| r.to = e)),
            ),
            MenuItem::submenu(
                "Leave On",
                Self::sync_items(&rule.sync, &cues, |s| set(&|r| r.sync = s.clone())),
            )
            .separated(),
            MenuItem::submenu(
                "Fade Out",
                Self::fade_items(rule.fade_out, |f| set(&|r| r.fade_out = f)),
            ),
            MenuItem::submenu(
                "Fade In",
                Self::fade_items(rule.fade_in, |f| set(&|r| r.fade_in = f)),
            ),
            MenuItem::submenu("Transition Segment", segment),
            MenuItem::submenu("Destination Starts", entries),
        ];
        if index > 0 {
            items.push(
                MenuItem::new(
                    "Move Up (Wins over the Next)",
                    add_op(AdaptiveOp::MoveRule {
                        rule: rule.id,
                        to: index - 1,
                    }),
                )
                .separated(),
            );
        }
        if index + 1 < score.rules.len() {
            items.push(MenuItem::new(
                "Move Down",
                add_op(AdaptiveOp::MoveRule {
                    rule: rule.id,
                    to: index + 1,
                }),
            ));
        }
        items.push(
            MenuItem::new("Remove Rule", add_op(AdaptiveOp::RemoveRule(rule.id))).separated(),
        );
        items
    }

    fn state_menu(model: &Session, state: &MusicState) -> Vec<MenuItem<Action>> {
        let score = model.interactive();
        let set = |f: &dyn Fn(&mut MusicState)| {
            let mut s = state.clone();
            f(&mut s);
            add_op(AdaptiveOp::SetState(s))
        };
        let bars = |n: f64| {
            let tl = &model.project().timeline;
            let start = model
                .project()
                .state_span(state)
                .map_or(MusicalTime::ZERO, |s| s.start);
            let bar = tl.meter.bar_at(start);
            tl.meter.bar_start(bar + n as i32) - tl.meter.bar_start(bar)
        };
        let entries: Vec<MenuItem<Action>> = ENTRIES
            .iter()
            .map(|n| {
                let at = bars(*n);
                let label = match *n {
                    0.0 => "At Its Start (No Pickup)".to_string(),
                    1.0 => "After 1 Bar".to_string(),
                    n => format!("After {n:.0} Bars"),
                };
                MenuItem::new(label, set(&|s| s.entry = at)).checked(state.entry == at)
            })
            .collect();
        let mut then =
            vec![MenuItem::new("Stop", set(&|s| s.then = None)).checked(state.then.is_none())];
        for (i, other) in score.states.iter().filter(|o| o.id != state.id).enumerate() {
            let id = other.id;
            let item = MenuItem::new(other.name.clone(), set(&|s| s.then = Some(id)))
                .checked(state.then == Some(id));
            then.push(if i == 0 { item.separated() } else { item });
        }
        let id = state.id;
        let mut items = vec![
            MenuItem::new("Go Here (Preview)", add_op(AdaptiveOp::Go(id))),
            MenuItem::new(
                "The Music Starts Here",
                add_op(AdaptiveOp::SetStart(Some(id))),
            )
            .checked(score.first_state() == Some(id)),
            MenuItem::new("Loops", set(&|s| s.looping = true))
                .checked(state.looping)
                .separated(),
            MenuItem::new("Plays Once", set(&|s| s.looping = false)).checked(!state.looping),
            MenuItem::submenu("Then (After Playing Once)", then),
            MenuItem::submenu("Loop Begins (Entry Cue)", entries).separated(),
            MenuItem::new("Remove State", add_op(AdaptiveOp::RemoveState(id))).separated(),
        ];
        if !state.cues.is_empty() {
            let cues = state
                .cues
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    MenuItem::new(
                        format!("Remove ‘{}’", c.name),
                        set(&move |s: &mut MusicState| {
                            s.cues.remove(i);
                        }),
                    )
                })
                .collect();
            items.insert(6, MenuItem::submenu("Cues", cues));
        }
        items
    }

    fn parameter_menu(model: &Session, index: usize) -> Vec<MenuItem<Action>> {
        let score = model.interactive();
        let Some(p) = score.parameters.get(index) else {
            return Vec::new();
        };
        let mut items = Vec::new();
        for layer in &score.layers {
            let curve = p.curve(layer.id);
            let set_curve = |c: Option<LayerCurve>| {
                let mut np = p.clone();
                np.curves.retain(|x| x.layer != layer.id);
                if let Some(c) = c {
                    np.curves.push(c);
                }
                add_op(AdaptiveOp::SetParameter(np))
            };
            let (lo, hi) = (p.min, p.max);
            let span = hi - lo;
            let choices = [
                ("Not Moved by It", None),
                (
                    "Fades In over Its Range",
                    Some(LayerCurve::fade_in(layer.id, lo, hi)),
                ),
                (
                    "Fades In over the Lower Half",
                    Some(LayerCurve::fade_in(layer.id, lo, lo + span * 0.5)),
                ),
                (
                    "Fades In over the Upper Half",
                    Some(LayerCurve::fade_in(layer.id, lo + span * 0.5, hi)),
                ),
                (
                    "Fades Out over Its Range",
                    Some(LayerCurve::fade_in(layer.id, lo, hi)).map(|mut c| {
                        for pt in &mut c.points {
                            pt.value = lo + hi - pt.value;
                        }
                        c.points.reverse();
                        c
                    }),
                ),
            ];
            let sub = choices
                .into_iter()
                .map(|(label, c)| {
                    let on = curve == c.as_ref();
                    MenuItem::new(label, set_curve(c)).checked(on)
                })
                .collect();
            items.push(MenuItem::submenu(format!("Layer ‘{}’", layer.name), sub));
        }
        if items.is_empty() {
            items.push(MenuItem::disabled(
                "No layers yet: + Layer makes one of the selected tracks",
            ));
        }
        items.push(
            MenuItem::new(
                "Remove Parameter",
                add_op(AdaptiveOp::RemoveParameter(p.id)),
            )
            .separated(),
        );
        items
    }

    fn layer_menu(model: &Session, index: usize) -> Vec<MenuItem<Action>> {
        let Some(l) = model.interactive().layers.get(index) else {
            return Vec::new();
        };
        let mut selected: Vec<_> = model.selection.tracks.iter().copied().collect();
        selected.sort_by_key(|t| model.project().tracks.iter().position(|x| x.id == *t));
        let mut with = l.clone();
        with.tracks = selected;
        vec![
            MenuItem::new(
                "Make It the Selected Tracks",
                add_op(AdaptiveOp::SetLayer(with)),
            ),
            MenuItem::new("Remove Layer", add_op(AdaptiveOp::RemoveLayer(l.id))).separated(),
        ]
    }

    fn stinger_menu(model: &Session, index: usize) -> Vec<MenuItem<Action>> {
        let Some(st) = model.interactive().stingers.get(index) else {
            return Vec::new();
        };
        let cues: Vec<StateCue> = model
            .interactive()
            .states
            .iter()
            .flat_map(|s| s.cues.clone())
            .collect();
        let sync = Self::sync_items(&st.sync, &cues, |s| {
            let mut n = st.clone();
            n.sync = s;
            add_op(AdaptiveOp::SetStinger(n))
        });
        vec![
            MenuItem::new("Play", add_op(AdaptiveOp::PlayStinger(st.id))),
            MenuItem::submenu("Plays On", sync).separated(),
            MenuItem::new("Remove Stinger", add_op(AdaptiveOp::RemoveStinger(st.id))).separated(),
        ]
    }

    /// The prompt renaming a state, layer or parameter (F2 over it).
    pub fn rename_request(hit: Hit, model: &Session, at: Rect) -> Option<HostRequest<Action>> {
        let score = model.interactive();
        let (initial, commit): (String, faderframe_ui_canvas::TextCommit<Action>) = match hit {
            Hit::State(i) => {
                let s = score.states.get(i)?.clone();
                (
                    s.name.clone(),
                    Box::new(move |t: &str| {
                        let t = t.trim();
                        (!t.is_empty()).then(|| {
                            let mut n = s.clone();
                            n.name = t.into();
                            add_op(AdaptiveOp::SetState(n))
                        })
                    }),
                )
            }
            Hit::Layer(i) => {
                let l = score.layers.get(i)?.clone();
                (
                    l.name.clone(),
                    Box::new(move |t: &str| {
                        let t = t.trim();
                        (!t.is_empty()).then(|| {
                            let mut n = l.clone();
                            n.name = t.into();
                            add_op(AdaptiveOp::SetLayer(n))
                        })
                    }),
                )
            }
            Hit::Parameter(i) => {
                let p = score.parameters.get(i)?.clone();
                (
                    p.name.clone(),
                    Box::new(move |t: &str| {
                        let t = t.trim();
                        (!t.is_empty()).then(|| {
                            let mut n = p.clone();
                            n.name = t.into();
                            add_op(AdaptiveOp::SetParameter(n))
                        })
                    }),
                )
            }
            _ => return None,
        };
        Some(HostRequest::TextInput {
            at,
            initial,
            commit,
        })
    }

    // --- painting ------------------------------------------------------

    fn paint_header(&self, p: &mut dyn Painter, size: Size, model: &Session, th: &Theme) {
        let r = Rect::new(0.0, 0.0, size.w, HEADER_H);
        p.fill(r, th.ui.surface);
        p.hline(0.0, size.w, HEADER_H - 0.5, th.ui.border);
        p.text(
            "Interactive Music",
            Rect::new(12.0, 0.0, 136.0, HEADER_H),
            &TextStyle::new(th.fonts.small, th.ui.text).bold(),
        );
        let status = model.adaptive_status();
        let mut right = 0.0f32;
        for (hit, rect, label) in Self::buttons(size, model) {
            let lit = hit == Hit::Preview && status.previewing;
            let fill = if lit {
                th.ui.accent.with_alpha(0.5)
            } else if self.hover == Some(hit) {
                th.ui.surface_alt.with_alpha(0.8)
            } else {
                th.ui.surface_alt
            };
            p.fill_rounded(rect, 4.0, &Paint::Solid(fill));
            p.stroke_rounded(rect, 4.0, 1.0, th.ui.border);
            p.text(
                &label,
                rect,
                &TextStyle::new(th.fonts.small, th.ui.text).center(),
            );
            right = rect.right();
        }
        let score = model.interactive();
        let name = |id: Option<MusicStateId>| {
            id.and_then(|i| score.state(i))
                .map_or("—".to_string(), |s| s.name.clone())
        };
        let mut text = if status.previewing {
            format!("Playing ‘{}’", name(status.current))
        } else {
            format!(
                "{} states · {} rules",
                score.states.len(),
                score.rules.len()
            )
        };
        if let Some(g) = status.segment {
            text.push_str(&format!(" (via ‘{}’)", name(Some(g))));
        }
        if let Some((to, secs, sync)) = &status.queued {
            text.push_str(&format!(" → ‘{}’ in {secs:.1} s ({sync})", name(Some(*to))));
        }
        if let Some(f) = status.exporting {
            text = format!("Exporting… {:.0} %", f * 100.0);
        }
        p.text(
            &text,
            Rect::new(
                right + 12.0,
                0.0,
                (size.w - right - 24.0).max(0.0),
                HEADER_H,
            ),
            &TextStyle::new(th.fonts.small, th.ui.text_dim).right(),
        );
    }

    fn heading(p: &mut dyn Painter, r: Rect, text: &str, th: &Theme) {
        p.text(
            text,
            Rect::new(r.x, r.y, r.w, SECTION_H - 4.0),
            &TextStyle::new(th.fonts.small, th.ui.text_dim).bold(),
        );
    }

    fn row(&self, p: &mut dyn Painter, r: Rect, hit: Hit, th: &Theme) {
        let fill = if self.hover == Some(hit) {
            th.ui.surface_alt.with_alpha(0.9)
        } else {
            th.ui.surface_alt.with_alpha(0.55)
        };
        p.fill_rounded(r, 3.0, &Paint::Solid(fill));
    }

    fn paint_states(&self, p: &mut dyn Painter, size: Size, model: &Session, th: &Theme) {
        let col = Self::columns(size)[0];
        Self::heading(p, col, "States", th);
        let status = model.adaptive_status();
        let score = model.interactive();
        let project = model.project();
        let queued = status.queued.as_ref().map(|q| q.0);
        p.push_clip(col);
        for (i, s) in score.states.iter().enumerate() {
            let r = Self::state_rect(i, size);
            self.row(p, r, Hit::State(i), th);
            if status.current == Some(s.id) {
                p.fill(Rect::new(r.x, r.y, 3.0, r.h), th.ui.accent);
            }
            if queued == Some(s.id) {
                p.stroke_rounded(r, 3.0, 1.5, th.ui.accent);
            }
            let start = if score.first_state() == Some(s.id) {
                " ★"
            } else {
                ""
            };
            p.text(
                &format!("{}{start}", s.name),
                Rect::new(r.x + 10.0, r.y + 2.0, r.w - 16.0, 16.0),
                &TextStyle::new(th.fonts.small, th.ui.text).bold(),
            );
            let mut detail = vec![project.state_source_name(s.source)];
            match project.state_span(s) {
                Some(span) if span.loop_start > span.start => {
                    detail.push("pickup".into());
                }
                Some(_) => {}
                None => detail.push("plays nothing".into()),
            }
            detail.push(if s.looping {
                "loops".into()
            } else {
                "once".into()
            });
            if !s.cues.is_empty() {
                detail.push(format!("{} cues", s.cues.len()));
            }
            p.text(
                &detail.join(" · "),
                Rect::new(r.x + 10.0, r.y + 17.0, r.w - 16.0, 14.0),
                &TextStyle::new(th.fonts.tiny + 0.5, th.ui.text_dim),
            );
        }
        if score.states.is_empty() {
            p.text(
                "No states yet: + State makes them of sections or scenes",
                Rect::new(col.x, col.y + SECTION_H, col.w, 18.0),
                &TextStyle::new(th.fonts.small, th.ui.text_faint),
            );
        }
        p.pop_clip();
    }

    fn paint_rules(&self, p: &mut dyn Painter, size: Size, model: &Session, th: &Theme) {
        let col = Self::columns(size)[1];
        Self::heading(p, col, "Transition Rules (most specific wins)", th);
        let score = model.interactive();
        let name = |e: RuleEnd| match e.state().and_then(|s| score.state(s)) {
            Some(s) => s.name.clone(),
            None => "Any".into(),
        };
        p.push_clip(col);
        for (i, r) in score.rules.iter().enumerate() {
            let rect = Self::rule_rect(i, size);
            self.row(p, rect, Hit::Rule(i), th);
            p.text(
                &format!("{} → {}", name(r.from), name(r.to)),
                Rect::new(rect.x + 10.0, rect.y + 2.0, rect.w - 16.0, 16.0),
                &TextStyle::new(th.fonts.small, th.ui.text).bold(),
            );
            let mut detail = vec![r.sync.label()];
            if !r.fade_out.is_instant() {
                detail.push(format!("out {}", r.fade_out.label()));
            }
            if !r.fade_in.is_instant() {
                detail.push(format!("in {}", r.fade_in.label()));
            }
            if let Some(g) = r.segment.and_then(|g| score.state(g)) {
                detail.push(format!("via {}", g.name));
            }
            if r.entry != Entry::Start {
                detail.push(r.entry.label().to_lowercase());
            }
            p.text(
                &detail.join(" · "),
                Rect::new(rect.x + 10.0, rect.y + 17.0, rect.w - 16.0, 14.0),
                &TextStyle::new(th.fonts.tiny + 0.5, th.ui.text_dim),
            );
        }
        if score.rules.is_empty() {
            p.text(
                "No rules: every move waits for the next bar",
                Rect::new(col.x, col.y + SECTION_H, col.w, 18.0),
                &TextStyle::new(th.fonts.small, th.ui.text_faint),
            );
        }
        p.pop_clip();
    }

    fn paint_right(&self, p: &mut dyn Painter, size: Size, model: &Session, th: &Theme) {
        let col = Self::columns(size)[2];
        let score = model.interactive();
        let rows = Self::right_rows(size, model);
        let top = |pred: &dyn Fn(Hit) -> bool, fallback: f32| {
            rows.iter()
                .find(|(h, _)| pred(*h))
                .map_or(fallback, |(_, r)| r.y - SECTION_H)
        };
        let layers_y = col.y;
        let params_y = top(
            &|h| matches!(h, Hit::Parameter(_)),
            layers_y + SECTION_H + score.layers.len() as f32 * ROW_H,
        );
        let stingers_y = top(
            &|h| matches!(h, Hit::Stinger(_)),
            params_y + SECTION_H + score.parameters.len() as f32 * ROW_H,
        );
        p.push_clip(col);
        Self::heading(
            p,
            Rect::new(col.x, layers_y, col.w, SECTION_H),
            "Layers",
            th,
        );
        Self::heading(
            p,
            Rect::new(col.x, params_y, col.w, SECTION_H),
            "Parameters (the preview's values)",
            th,
        );
        Self::heading(
            p,
            Rect::new(col.x, stingers_y, col.w, SECTION_H),
            "Stingers",
            th,
        );
        for (hit, r) in &rows {
            self.row(p, *r, *hit, th);
            match *hit {
                Hit::Layer(i) => {
                    let l = &score.layers[i];
                    let tracks: Vec<String> = l
                        .tracks
                        .iter()
                        .filter_map(|t| model.project().track(*t))
                        .map(|t| t.name.clone())
                        .collect();
                    p.text(
                        &l.name,
                        Rect::new(r.x + 10.0, r.y + 2.0, r.w - 16.0, 16.0),
                        &TextStyle::new(th.fonts.small, th.ui.text).bold(),
                    );
                    p.text(
                        &if tracks.is_empty() {
                            "no tracks".into()
                        } else {
                            tracks.join(", ")
                        },
                        Rect::new(r.x + 10.0, r.y + 17.0, r.w - 16.0, 14.0),
                        &TextStyle::new(th.fonts.tiny + 0.5, th.ui.text_dim),
                    );
                }
                Hit::Parameter(i) => {
                    let par = &score.parameters[i];
                    let v = model.score_value(par.id);
                    p.text(
                        &format!("{} {v:.2}", par.name),
                        Rect::new(r.x + 10.0, r.y, r.w * 0.4 - 12.0, r.h),
                        &TextStyle::new(th.fonts.small, th.ui.text),
                    );
                    let s = Self::slider(*r);
                    let mid = s.y + s.h / 2.0;
                    p.fill_rounded(
                        Rect::new(s.x, mid - 2.0, s.w, 4.0),
                        2.0,
                        &Paint::Solid(th.ui.border),
                    );
                    let t = ((v - par.min) / (par.max - par.min).max(f32::EPSILON)).clamp(0.0, 1.0);
                    let x = s.x + t * s.w;
                    p.fill_rounded(
                        Rect::new(s.x, mid - 2.0, x - s.x, 4.0),
                        2.0,
                        &Paint::Solid(th.ui.accent.with_alpha(0.8)),
                    );
                    let knob = Rect::new(x - 6.0, mid - 8.0, 12.0, 16.0);
                    p.fill_rounded(knob, 3.0, &Paint::Solid(th.ui.text));
                    p.stroke_rounded(knob, 3.0, 1.0, th.ui.border);
                }
                Hit::Stinger(i) => {
                    let st = &score.stingers[i];
                    p.text(
                        &format!("♪ {}", st.name),
                        Rect::new(r.x + 10.0, r.y + 2.0, r.w - 16.0, 16.0),
                        &TextStyle::new(th.fonts.small, th.ui.text).bold(),
                    );
                    p.text(
                        &format!("on {}", st.sync.label().to_lowercase()),
                        Rect::new(r.x + 10.0, r.y + 17.0, r.w - 16.0, 14.0),
                        &TextStyle::new(th.fonts.tiny + 0.5, th.ui.text_dim),
                    );
                }
                _ => {}
            }
        }
        p.pop_clip();
    }

    fn paint_footer(&self, p: &mut dyn Painter, size: Size, model: &Session, th: &Theme) {
        let f = Rect::new(0.0, size.h - FOOTER_H, size.w, FOOTER_H);
        p.fill(f, th.ui.surface);
        p.hline(0.0, size.w, f.y + 0.5, th.ui.border);
        let text = match model.adaptive_status().last_export {
            Some(Ok(r)) => format!("Exported into {} ({} files)", r.folder.display(), r.files.len()),
            Some(Err(e)) => format!("The export failed: {e}"),
            None => "Click a state to go there as the game would · right-click to edit · Export writes stems, a manifest, FMOD and Wwise imports".into(),
        };
        p.text(
            &text,
            Rect::new(12.0, f.y, size.w - 24.0, FOOTER_H),
            &TextStyle::new(th.fonts.tiny + 0.5, th.ui.text_dim),
        );
    }

    fn click(
        &mut self,
        hit: Hit,
        pos: Point,
        size: Size,
        model: &Session,
        clicks: u32,
        cx: &mut EventCx<'_, Action>,
    ) {
        let score = model.interactive();
        match hit {
            Hit::Preview => cx.emit(add_op(AdaptiveOp::Preview(
                !model.adaptive_status().previewing,
            ))),
            Hit::AddState => cx.request(HostRequest::ContextMenu {
                at: pos,
                items: Self::add_state_menu(model),
            }),
            Hit::AddRule => cx.request(HostRequest::ContextMenu {
                at: pos,
                items: Self::add_rule_menu(model),
            }),
            Hit::AddLayer => cx.emit(add_op(AdaptiveOp::AddLayer {
                name: None,
                tracks: Vec::new(),
            })),
            Hit::AddParameter => cx.emit(add_op(AdaptiveOp::AddParameter { name: None })),
            Hit::AddStinger => cx.request(HostRequest::ContextMenu {
                at: pos,
                items: Self::add_stinger_menu(model),
            }),
            Hit::Export => cx.request(HostRequest::ChooseFiles {
                choice: FileChoice::Folder {
                    title: "Export the Interactive Score into".into(),
                    initial: None,
                },
                commit: Box::new(|paths| {
                    paths
                        .into_iter()
                        .next()
                        .map(|dir| add_op(AdaptiveOp::Export(dir)))
                }),
            }),
            // A state's button: the music goes there (the preview starts
            // there when it is not running).
            Hit::State(i) => {
                if let Some(s) = score.states.get(i)
                    && clicks == 1
                {
                    cx.emit(add_op(AdaptiveOp::Go(s.id)));
                }
            }
            Hit::Rule(i) => {
                if let Some(r) = score.rules.get(i) {
                    cx.request(HostRequest::ContextMenu {
                        at: pos,
                        items: Self::rule_menu(model, r, i),
                    });
                }
            }
            Hit::Layer(_) => {}
            Hit::Parameter(i) => {
                let Some(par) = score.parameters.get(i) else {
                    return;
                };
                let Some((_, row)) = Self::right_rows(size, model)
                    .into_iter()
                    .find(|(h, _)| *h == hit)
                else {
                    return;
                };
                if Self::slider(row).inset_xy(-6.0, -4.0).contains(pos) {
                    self.dragging = Some(par.id);
                    cx.emit(add_op(AdaptiveOp::SetValue {
                        parameter: par.id,
                        value: Self::value_at(pos.x, row, par.min, par.max),
                    }));
                }
            }
            Hit::Stinger(i) => {
                if let Some(st) = score.stingers.get(i) {
                    cx.emit(add_op(AdaptiveOp::PlayStinger(st.id)));
                }
            }
        }
    }
}

impl CanvasView<Session, Action> for AdaptiveView {
    fn set_theme(&mut self, theme: &Theme) {
        self.theme = theme.clone();
    }

    fn accessible_name(&self) -> Option<String> {
        Some("Interactive Music".into())
    }

    fn accessible(
        &self,
        size: Size,
        model: &Session,
    ) -> Vec<faderframe_ui_canvas::AccessNode<Action>> {
        use faderframe_ui_canvas::{AccessNode, AccessRole, access_id};
        let status = model.adaptive_status();
        let states = model.interactive().states.iter().enumerate().map(|(i, s)| {
            AccessNode::new(
                access_id(&[71, s.id.raw()]),
                AccessRole::ListItem,
                format!("State {}: {}", i + 1, s.name),
            )
            .at(Self::state_rect(i, size))
            .selected(status.current == Some(s.id))
            .on_activate(add_op(AdaptiveOp::Go(s.id)))
        });
        vec![
            AccessNode::new(
                access_id(&[70]),
                AccessRole::Button,
                if status.previewing {
                    "Stop the Preview"
                } else {
                    "Preview"
                },
            )
            .on_activate(add_op(AdaptiveOp::Preview(!status.previewing))),
            AccessNode::new(access_id(&[72]), AccessRole::List, "States")
                .at(Self::columns(size)[0])
                .with_children(states),
        ]
    }

    fn wants_frames(&self, model: &Session) -> bool {
        model.adaptive_previewing() || model.adaptive_status().exporting.is_some()
    }

    fn paint(&mut self, p: &mut dyn Painter, size: Size, model: &Session, theme: &Theme) {
        let th = theme;
        p.fill(Rect::from_size(size), th.ui.background);
        self.paint_states(p, size, model, th);
        self.paint_rules(p, size, model, th);
        self.paint_right(p, size, model, th);
        self.paint_header(p, size, model, th);
        self.paint_footer(p, size, model, th);
    }

    fn event(
        &mut self,
        ev: &ViewEvent,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        match *ev {
            ViewEvent::PointerDown {
                pos,
                button: PointerButton::Primary,
                clicks,
                ..
            } => {
                let Some(hit) = self.hit(pos, size, model) else {
                    return false;
                };
                self.click(hit, pos, size, model, clicks, cx);
                true
            }
            ViewEvent::PointerDown {
                pos,
                button: PointerButton::Secondary,
                ..
            } => {
                let Some(hit) = self.hit(pos, size, model) else {
                    return false;
                };
                let score = model.interactive();
                let (items, at) = match hit {
                    Hit::State(i) => match score.states.get(i) {
                        Some(s) => (Self::state_menu(model, s), Self::state_rect(i, size)),
                        None => return false,
                    },
                    Hit::Rule(i) => match score.rules.get(i) {
                        Some(r) => (Self::rule_menu(model, r, i), Self::rule_rect(i, size)),
                        None => return false,
                    },
                    Hit::Layer(i) | Hit::Parameter(i) | Hit::Stinger(i) => {
                        let row = Self::right_rows(size, model)
                            .into_iter()
                            .find(|(h, _)| *h == hit)
                            .map_or(Rect::new(pos.x, pos.y, 120.0, 24.0), |(_, r)| r);
                        let items = match hit {
                            Hit::Layer(_) => Self::layer_menu(model, i),
                            Hit::Parameter(_) => Self::parameter_menu(model, i),
                            _ => Self::stinger_menu(model, i),
                        };
                        (items, row)
                    }
                    _ => return false,
                };
                cx.request(HostRequest::ContextMenu {
                    at: Point::new(at.x + 12.0, at.bottom()),
                    items,
                });
                true
            }
            ViewEvent::PointerMove { pos, dragging, .. } => {
                if dragging && let Some(id) = self.dragging {
                    let score = model.interactive();
                    if let Some(i) = score.parameters.iter().position(|p| p.id == id)
                        && let Some((_, row)) = Self::right_rows(size, model)
                            .into_iter()
                            .find(|(h, _)| *h == Hit::Parameter(i))
                    {
                        let p = &score.parameters[i];
                        cx.emit(add_op(AdaptiveOp::SetValue {
                            parameter: id,
                            value: Self::value_at(pos.x, row, p.min, p.max),
                        }));
                    }
                    return true;
                }
                let hover = self.hit(pos, size, model);
                if hover != self.hover {
                    self.hover = hover;
                    cx.redraw();
                }
                if matches!(hover, Some(Hit::Parameter(_))) {
                    cx.set_cursor(Cursor::ResizeHorizontal);
                }
                false
            }
            ViewEvent::PointerUp { .. } => self.dragging.take().is_some(),
            ViewEvent::PointerLeave => {
                if self.hover.take().is_some() {
                    cx.redraw();
                }
                false
            }
            // F2 renames the state, layer or parameter under the pointer.
            ViewEvent::Key {
                key: faderframe_ui_canvas::Key::F(2),
                ..
            } => {
                let Some(hit) = self.hover else {
                    return false;
                };
                let at = match hit {
                    Hit::State(i) => Self::state_rect(i, size),
                    _ => match Self::right_rows(size, model)
                        .into_iter()
                        .find(|(h, _)| *h == hit)
                    {
                        Some((_, r)) => r,
                        None => return false,
                    },
                };
                match Self::rename_request(hit, model, at) {
                    Some(req) => {
                        cx.request(req);
                        true
                    }
                    None => false,
                }
            }
            _ => false,
        }
    }

    fn tooltip(&self, pos: Point, size: Size, model: &Session) -> Option<String> {
        Some(match self.hit(pos, size, model)? {
            Hit::Preview => "Play the score as the game would: click states to move between them".into(),
            Hit::AddState => "A state of a song section or a launcher scene".into(),
            Hit::AddRule => "How the music moves from a state to another".into(),
            Hit::AddLayer => "A layer of the selected tracks (faded by parameters)".into(),
            Hit::AddParameter => "A value the game sets (intensity): it fades layers in and out".into(),
            Hit::AddStinger => "A short phrase (a launcher clip) played over the music on a sync point".into(),
            Hit::Export => {
                "Stems per state and layer with loop points, a JSON manifest, an FMOD Studio script, Wwise importer list and WAAPI calls".into()
            }
            Hit::State(_) => {
                "Click: the music goes there on its rule (the preview starts there) · right-click: edit · F2: rename".into()
            }
            Hit::Rule(_) => "Click: what the rule does".into(),
            Hit::Layer(_) => "Right-click: its tracks · F2: rename".into(),
            Hit::Parameter(_) => {
                "Drag: the value in the preview · right-click: what it does to each layer · F2: rename".into()
            }
            Hit::Stinger(_) => "Click: play it on its sync point · right-click: when".into(),
        })
    }

    fn min_size(&self) -> Size {
        Size::new(640.0, 220.0)
    }
}

#[cfg(test)]
mod tests;
