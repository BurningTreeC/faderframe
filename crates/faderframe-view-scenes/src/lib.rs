//! Mix scenes: console snapshots stored, recalled, crossfaded, morphed
//! and compared (see `faderframe_session::scenes`).
//!
//! The header stores the console as a new scene (with the devices'
//! parameters and states, or without), updates and recalls the scene
//! chosen in the list (Stop while a crossfade runs), steps to the
//! previous and next scene, and sets how recalls go unless a scene says
//! otherwise: the crossfade's length (instant, milliseconds, seconds or
//! beats) and where switched values change. The list (left) holds the
//! scenes: a click chooses one, a double click recalls it, the context
//! menu recalls it at once or as it says, updates, renames, sets its own
//! crossfade, makes it A or B, compares it with the mix, moves, learns a
//! MIDI control and deletes it; a crossfade's progress runs along its
//! row. Right: what the chosen scene recalls (a chip per part of the
//! console), the tracks it never touches (a chip per track) and its own
//! crossfade. The strip at the bottom picks scenes A and B, compares
//! them (or the mix with B) — Keep makes the side heard one undo step,
//! Back returns to the mix as it was — and morphs between them with a
//! slider whose drag is one undo step (right-click: MIDI learn).

#![forbid(unsafe_code)]

use faderframe_core::{MixSceneId, TrackId};
use faderframe_project::scenes::{FadeLength, MixScene, SceneFade, ScopePart, SwitchPoint};
use faderframe_project::{MappingTarget, SceneControl};
use faderframe_session::scenes::{SceneActivity, SceneOp, SceneSide};
use faderframe_session::{Action, Session};
use faderframe_ui_canvas::{
    CanvasView, Cursor, EventCx, HostRequest, Key, MenuItem, Paint, Painter, Point, PointerButton,
    Rect, Size, TextStyle, Theme, ViewEvent,
};

const HEADER_H: f32 = 38.0;
const STRIP_H: f32 = 46.0;
const FOOTER_H: f32 = 22.0;
const LIST_W: f32 = 270.0;
const ROW_H: f32 = 40.0;
const GAP: f32 = 8.0;
const CHIP_H: f32 = 22.0;
const SECTION_H: f32 = 20.0;

/// Crossfade lengths the menus offer.
const LENGTHS: [FadeLength; 10] = [
    FadeLength::Instant,
    FadeLength::Millis { ms: 250.0 },
    FadeLength::Millis { ms: 500.0 },
    FadeLength::Millis { ms: 1000.0 },
    FadeLength::Millis { ms: 2000.0 },
    FadeLength::Millis { ms: 5000.0 },
    FadeLength::Beats { beats: 1.0 },
    FadeLength::Beats { beats: 2.0 },
    FadeLength::Beats { beats: 4.0 },
    FadeLength::Beats { beats: 8.0 },
];

/// What a click hits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Store,
    StorePlugins,
    Update,
    Recall,
    Stop,
    Previous,
    Next,
    /// The project's crossfade and switch point.
    Fade,
    Switch,
    Row(usize),
    Scope(ScopePart),
    Safe(TrackId),
    /// The chosen scene's own crossfade.
    OwnFade,
    PickA,
    PickB,
    HearA,
    HearB,
    Keep,
    Back,
    Morph,
}

pub struct ScenesView {
    theme: Theme,
    /// The scene chosen in the list (presentation only).
    chosen: Option<MixSceneId>,
    hover: Option<Hit>,
    /// The morph slider is held.
    morphing: bool,
    /// Where the slider stood last (shown between morphs).
    morph_at: f64,
}

impl ScenesView {
    pub fn new(theme: Theme) -> Self {
        Self {
            theme,
            chosen: None,
            hover: None,
            morphing: false,
            morph_at: 0.0,
        }
    }

    /// The scene the panel is about: the one chosen, else the one
    /// recalled last, else the first.
    pub fn shown(&self, model: &Session) -> Option<MixSceneId> {
        let scenes = model.mix_scenes();
        self.chosen
            .filter(|id| scenes.get(*id).is_some())
            .or_else(|| model.current_mix_scene())
            .filter(|id| scenes.get(*id).is_some())
            .or_else(|| scenes.scenes.first().map(|s| s.id))
    }

    fn shown_scene<'a>(&self, model: &'a Session) -> Option<&'a MixScene> {
        self.shown(model).and_then(|id| model.mix_scenes().get(id))
    }

    fn fading(model: &Session) -> bool {
        matches!(model.scene_activity(), Some(SceneActivity::Fading { .. }))
    }

    /// The header's buttons.
    pub fn buttons(size: Size, model: &Session) -> Vec<(Hit, Rect)> {
        let y = 8.0;
        let h = HEADER_H - 16.0;
        let mut x = 112.0;
        let mut out = Vec::new();
        let mut push = |hit: Hit, w: f32, gap: f32| {
            out.push((hit, Rect::new(x, y, w, h)));
            x += w + gap;
        };
        push(Hit::Store, 64.0, 4.0);
        push(Hit::StorePlugins, 118.0, 10.0);
        push(Hit::Update, 62.0, 4.0);
        push(
            if Self::fading(model) {
                Hit::Stop
            } else {
                Hit::Recall
            },
            62.0,
            10.0,
        );
        push(Hit::Previous, 28.0, 2.0);
        push(Hit::Next, 28.0, 10.0);
        push(Hit::Fade, 150.0, 4.0);
        push(Hit::Switch, 130.0, 0.0);
        out.retain(|(_, r)| r.right() <= size.w - 4.0);
        out
    }

    fn list_rect(size: Size) -> Rect {
        Rect::new(
            0.0,
            HEADER_H,
            LIST_W.min(size.w * 0.45),
            (size.h - HEADER_H - STRIP_H - FOOTER_H).max(0.0),
        )
    }

    pub fn row_rect(i: usize, size: Size) -> Rect {
        let l = Self::list_rect(size);
        Rect::new(l.x, l.y + i as f32 * ROW_H, l.w, ROW_H)
    }

    fn panel_rect(size: Size) -> Rect {
        let l = Self::list_rect(size);
        let x = l.right() + GAP;
        Rect::new(
            x,
            l.y + GAP,
            (size.w - x - GAP).max(0.0),
            (l.h - 2.0 * GAP).max(0.0),
        )
    }

    fn strip_rect(size: Size) -> Rect {
        Rect::new(0.0, size.h - FOOTER_H - STRIP_H, size.w, STRIP_H)
    }

    /// Chips laid out in rows from `top` across `panel` (labels' widths
    /// estimated, so hits need no painter).
    fn flow(panel: Rect, top: f32, labels: Vec<(Hit, String)>) -> (Vec<(Hit, Rect, String)>, f32) {
        let mut x = panel.x;
        let mut y = top;
        let mut out = Vec::new();
        for (hit, label) in labels {
            let w = (label.chars().count() as f32 * 6.6 + 18.0).min(panel.w);
            if x + w > panel.right() && x > panel.x {
                x = panel.x;
                y += CHIP_H + 4.0;
            }
            out.push((hit, Rect::new(x, y, w, CHIP_H), label));
            x += w + 4.0;
        }
        (out, y + CHIP_H)
    }

    /// The panel's chips: what the scene recalls, the tracks safe from it,
    /// its own crossfade.
    pub fn chips(&self, size: Size, model: &Session) -> Vec<(Hit, Rect, String)> {
        let panel = Self::panel_rect(size);
        let Some(scene) = self.shown_scene(model) else {
            return Vec::new();
        };
        let parts = ScopePart::ALL
            .iter()
            .map(|p| (Hit::Scope(*p), p.label().to_string()))
            .collect();
        let (mut out, bottom) = Self::flow(panel, panel.y + SECTION_H, parts);
        let tracks = model
            .project()
            .folder_order()
            .into_iter()
            .filter(|t| scene.mix.tracks.contains_key(&t.id))
            .map(|t| (Hit::Safe(t.id), t.name.clone()))
            .collect();
        let (safe, bottom) = Self::flow(panel, bottom + GAP + SECTION_H, tracks);
        out.extend(safe);
        let own = match scene.fade {
            Some(f) => format!("Own crossfade: {} ▾", f.label()),
            None => "Own crossfade: the project's ▾".to_string(),
        };
        let w = (own.chars().count() as f32 * 6.6 + 18.0).min(panel.w);
        out.push((
            Hit::OwnFade,
            Rect::new(panel.x, bottom + GAP + SECTION_H, w, CHIP_H),
            own,
        ));
        out.retain(|(_, r, _)| r.bottom() <= panel.bottom() + 0.5);
        out
    }

    /// The bottom strip's controls.
    pub fn strip(size: Size, model: &Session) -> Vec<(Hit, Rect)> {
        let s = Self::strip_rect(size);
        let y = s.y + 10.0;
        let h = s.h - 20.0;
        let mut x = 12.0;
        let mut out = Vec::new();
        let mut push = |hit: Hit, w: f32, gap: f32| {
            out.push((hit, Rect::new(x, y, w, h)));
            x += w + gap;
        };
        push(Hit::PickA, 120.0, 4.0);
        push(Hit::PickB, 120.0, 12.0);
        push(Hit::HearA, 40.0, 0.0);
        push(Hit::HearB, 40.0, 8.0);
        if matches!(
            model.scene_activity(),
            Some(SceneActivity::Comparing { .. })
        ) {
            push(Hit::Keep, 54.0, 4.0);
            push(Hit::Back, 54.0, 12.0);
        }
        let rest = (s.right() - 12.0 - x).max(0.0);
        if rest >= 80.0 {
            out.push((Hit::Morph, Rect::new(x, y, rest, h)));
        }
        out
    }

    pub fn hit(&self, pos: Point, size: Size, model: &Session) -> Option<Hit> {
        if let Some((h, _)) = Self::buttons(size, model)
            .into_iter()
            .chain(Self::strip(size, model))
            .find(|(_, r)| r.contains(pos))
        {
            return Some(h);
        }
        let list = Self::list_rect(size);
        if list.contains(pos) {
            let i = ((pos.y - list.y) / ROW_H).floor() as usize;
            return (i < model.mix_scenes().scenes.len()).then_some(Hit::Row(i));
        }
        self.chips(size, model)
            .into_iter()
            .find(|(_, r, _)| r.contains(pos))
            .map(|(h, _, _)| h)
    }

    /// The morph position under `x` on the slider.
    fn position_at(x: f32, slider: Rect) -> f64 {
        let track = slider.inset_xy(28.0, 0.0);
        f64::from(((x - track.x) / track.w.max(1.0)).clamp(0.0, 1.0))
    }

    /// Where the slider stands: the morph's position while it runs, else
    /// where it was left.
    fn morph_position(&self, model: &Session) -> f64 {
        match model.scene_activity() {
            Some(SceneActivity::Morphing { position, .. }) => position,
            _ => self.morph_at,
        }
    }

    // --- menus ---------------------------------------------------------------

    fn length_items(
        current: FadeLength,
        make: impl Fn(FadeLength) -> Action,
    ) -> Vec<MenuItem<Action>> {
        LENGTHS
            .iter()
            .map(|l| MenuItem::new(l.label(), make(*l)).checked(*l == current))
            .collect()
    }

    fn switch_items(
        current: SwitchPoint,
        make: impl Fn(SwitchPoint) -> Action,
    ) -> Vec<MenuItem<Action>> {
        SwitchPoint::ALL
            .iter()
            .map(|s| MenuItem::new(s.label(), make(*s)).checked(*s == current))
            .collect()
    }

    /// The project's crossfade: lengths, then typed.
    fn fade_menu(model: &Session) -> Vec<MenuItem<Action>> {
        let fade = model.mix_scenes().fade;
        Self::length_items(fade.length, |length| {
            Action::MixScene(SceneOp::SetFade {
                scene: None,
                fade: Some(SceneFade { length, ..fade }),
            })
        })
    }

    fn fade_input(model: &Session, scene: Option<MixSceneId>, at: Rect) -> HostRequest<Action> {
        let fade = match scene.and_then(|id| model.mix_scenes().get(id)) {
            Some(s) => model.mix_scenes().fade_of(s),
            None => model.mix_scenes().fade,
        };
        HostRequest::TextInput {
            at,
            initial: fade.length.label(),
            commit: Box::new(move |text| {
                let length = FadeLength::parse(text)?;
                Some(Action::MixScene(SceneOp::SetFade {
                    scene,
                    fade: Some(SceneFade { length, ..fade }),
                }))
            }),
        }
    }

    /// A scene's own crossfade: the project's, or a length and switch of
    /// its own.
    fn own_fade_menu(model: &Session, scene: &MixScene) -> Vec<MenuItem<Action>> {
        let id = scene.id;
        let base = model.mix_scenes().fade_of(scene);
        let mut items = vec![
            MenuItem::new(
                format!("The Project's ({})", model.mix_scenes().fade.label()),
                Action::MixScene(SceneOp::SetFade {
                    scene: Some(id),
                    fade: None,
                }),
            )
            .checked(scene.fade.is_none()),
        ];
        let own = scene.fade.map(|f| f.length);
        items.extend(
            LENGTHS
                .iter()
                .map(|l| {
                    MenuItem::new(
                        l.label(),
                        Action::MixScene(SceneOp::SetFade {
                            scene: Some(id),
                            fade: Some(SceneFade { length: *l, ..base }),
                        }),
                    )
                    .checked(own == Some(*l))
                })
                .enumerate()
                .map(|(i, m)| if i == 0 { m.separated() } else { m }),
        );
        let switches = Self::switch_items(base.switch, |switch| {
            Action::MixScene(SceneOp::SetFade {
                scene: Some(id),
                fade: Some(SceneFade { switch, ..base }),
            })
        });
        items.push(MenuItem::submenu("Switch", switches).separated());
        items
    }

    fn pick_menu(model: &Session, b: bool) -> Vec<MenuItem<Action>> {
        let m = model.mix_scenes();
        let now = if b { m.b } else { m.a };
        let set = |id: Option<MixSceneId>| {
            Action::MixScene(if b {
                SceneOp::SetAB { a: m.a, b: id }
            } else {
                SceneOp::SetAB { a: id, b: m.b }
            })
        };
        let mut items = vec![
            MenuItem::new(
                if b {
                    "None"
                } else {
                    "The Mix as It Is (compare)"
                },
                set(None),
            )
            .checked(now.is_none()),
        ];
        for (i, s) in m.scenes.iter().enumerate() {
            let item = MenuItem::new(s.name.clone(), set(Some(s.id))).checked(now == Some(s.id));
            items.push(if i == 0 { item.separated() } else { item });
        }
        items
    }

    fn row_menu(model: &Session, scene: &MixScene, index: usize) -> Vec<MenuItem<Action>> {
        let id = scene.id;
        let m = model.mix_scenes();
        let n = m.scenes.len();
        let keeps = scene.mix.keeps_plugins();
        let mut items = vec![
            MenuItem::new(
                format!("Recall ({})", m.fade_of(scene).label()),
                Action::MixScene(SceneOp::Recall {
                    scene: id,
                    fade: None,
                }),
            ),
            MenuItem::new(
                "Recall at Once",
                Action::MixScene(SceneOp::Recall {
                    scene: id,
                    fade: Some(SceneFade::default()),
                }),
            ),
            MenuItem::new("Rename…", Action::MixScene(SceneOp::PromptRename(id))).separated(),
            MenuItem::new(
                "Update from the Console",
                Action::MixScene(SceneOp::Update {
                    scene: id,
                    plugins: None,
                }),
            ),
            MenuItem::new(
                if keeps {
                    "Update without the Plugins"
                } else {
                    "Update with the Plugins"
                },
                Action::MixScene(SceneOp::Update {
                    scene: id,
                    plugins: Some(!keeps),
                }),
            ),
            MenuItem::submenu("Crossfade", Self::own_fade_menu(model, scene)),
            MenuItem::new(
                "Make It A",
                Action::MixScene(SceneOp::SetAB {
                    a: Some(id),
                    b: m.b,
                }),
            )
            .checked(m.a == Some(id))
            .separated(),
            MenuItem::new(
                "Make It B",
                Action::MixScene(SceneOp::SetAB {
                    a: m.a,
                    b: Some(id),
                }),
            )
            .checked(m.b == Some(id)),
            MenuItem::new(
                "Compare with the Mix",
                Action::MixScene(SceneOp::CompareStart { a: None, b: id }),
            ),
        ];
        if index > 0 {
            items.push(
                MenuItem::new(
                    "Move Up",
                    Action::MixScene(SceneOp::Move {
                        scene: id,
                        to: index - 1,
                    }),
                )
                .separated(),
            );
        }
        if index + 1 < n {
            let item = MenuItem::new(
                "Move Down",
                Action::MixScene(SceneOp::Move {
                    scene: id,
                    to: index + 1,
                }),
            );
            items.push(if index == 0 { item.separated() } else { item });
        }
        let learn = model
            .midi_learn_menu(MappingTarget::MixScene {
                control: SceneControl::Recall { scene: id },
            })
            .into_iter()
            .map(|(label, action)| MenuItem::new(label, action))
            .collect();
        items.push(MenuItem::submenu("MIDI Control", learn).separated());
        items
            .push(MenuItem::new("Delete Scene", Action::MixScene(SceneOp::Delete(id))).separated());
        items
    }

    fn learn_menu(model: &Session, control: SceneControl) -> Vec<MenuItem<Action>> {
        model
            .midi_learn_menu(MappingTarget::MixScene { control })
            .into_iter()
            .map(|(label, action)| MenuItem::new(label, action))
            .collect()
    }

    fn rename_request(scene: &MixScene, at: Rect) -> HostRequest<Action> {
        let id = scene.id;
        HostRequest::TextInput {
            at,
            initial: scene.name.clone(),
            commit: Box::new(move |text| {
                let name = text.trim();
                (!name.is_empty()).then(|| Action::MixScene(SceneOp::Rename(id, name.to_string())))
            }),
        }
    }

    // --- painting ------------------------------------------------------------

    fn button_label(&self, hit: Hit, model: &Session) -> String {
        let m = model.mix_scenes();
        let name = |id: Option<MixSceneId>| id.and_then(|id| m.get(id)).map(|s| s.name.clone());
        match hit {
            Hit::Store => "+ Store".into(),
            Hit::StorePlugins => "+ Store with Plugins".into(),
            Hit::Update => "Update".into(),
            Hit::Recall => "Recall".into(),
            Hit::Stop => "Stop".into(),
            Hit::Previous => "◀".into(),
            Hit::Next => "▶".into(),
            Hit::Fade => format!("Crossfade: {} ▾", m.fade.length.label()),
            Hit::Switch => format!("{} ▾", m.fade.switch.label()),
            Hit::PickA => format!("A: {} ▾", name(m.a).unwrap_or_else(|| "the mix".into())),
            Hit::PickB => format!("B: {} ▾", name(m.b).unwrap_or_else(|| "–".into())),
            Hit::HearA => "A".into(),
            Hit::HearB => "B".into(),
            Hit::Keep => "Keep".into(),
            Hit::Back => "Back".into(),
            _ => String::new(),
        }
    }

    fn enabled(&self, hit: Hit, model: &Session) -> bool {
        let m = model.mix_scenes();
        let has = self.shown(model).is_some();
        match hit {
            Hit::Update | Hit::Recall | Hit::Previous | Hit::Next => has,
            Hit::HearA | Hit::HearB => m.b.is_some(),
            Hit::Morph => m.a.is_some() && m.b.is_some(),
            _ => true,
        }
    }

    /// A button: lit (on), hovered, greyed when it can do nothing.
    fn paint_button(
        &self,
        p: &mut dyn Painter,
        r: Rect,
        hit: Hit,
        lit: bool,
        model: &Session,
        th: &Theme,
    ) {
        let label = self.button_label(hit, model);
        let label = label.as_str();
        let hot = self.hover == Some(hit);
        let enabled = self.enabled(hit, model);
        let fill = if lit {
            th.ui.accent.with_alpha(0.55)
        } else if hot && enabled {
            th.ui.surface_alt.with_alpha(0.8)
        } else {
            th.ui.surface_alt
        };
        p.fill_rounded(r, 4.0, &Paint::Solid(fill));
        p.stroke_rounded(r, 4.0, 1.0, th.ui.border);
        p.text(
            label,
            r,
            &TextStyle::new(
                th.fonts.small,
                if enabled {
                    th.ui.text
                } else {
                    th.ui.text_faint
                },
            )
            .center(),
        );
    }

    fn paint_header(&self, p: &mut dyn Painter, size: Size, model: &Session, th: &Theme) {
        let header = Rect::new(0.0, 0.0, size.w, HEADER_H);
        p.fill(header, th.ui.surface);
        p.hline(0.0, size.w, HEADER_H - 0.5, th.ui.border);
        p.text(
            "Mix Scenes",
            Rect::new(12.0, 0.0, 96.0, HEADER_H),
            &TextStyle::new(th.fonts.normal, th.ui.text).bold(),
        );
        let mut right = 112.0;
        for (hit, r) in Self::buttons(size, model) {
            right = r.right();
            let lit = hit == Hit::Stop;
            self.paint_button(p, r, hit, lit, model, th);
        }
        let status = match model.scene_activity() {
            Some(SceneActivity::Fading { scene, progress }) => format!(
                "Crossfading to ‘{}’ · {:.0} %",
                model.mix_scene_name(scene),
                progress * 100.0
            ),
            Some(SceneActivity::Morphing { a, b, position }) => format!(
                "Morphing ‘{}’ → ‘{}’ · {:.0} %",
                model.mix_scene_name(a),
                model.mix_scene_name(b),
                position * 100.0
            ),
            Some(SceneActivity::Comparing { a, b, hearing }) => {
                let side = match hearing {
                    SceneSide::A => a.map_or_else(
                        || "the mix as it was".to_string(),
                        |a| format!("‘{}’", model.mix_scene_name(a)),
                    ),
                    SceneSide::B => format!("‘{}’", model.mix_scene_name(b)),
                };
                format!("Comparing · hearing {side}")
            }
            None => match model.current_mix_scene() {
                Some(id) if model.mix_scene_matches(id) => {
                    format!("‘{}’ is on the console", model.mix_scene_name(id))
                }
                Some(id) => format!(
                    "‘{}’ recalled last (changed since)",
                    model.mix_scene_name(id)
                ),
                None => String::new(),
            },
        };
        p.text(
            &status,
            Rect::new(
                right + 12.0,
                0.0,
                (size.w - right - 24.0).max(0.0),
                HEADER_H,
            ),
            &TextStyle::new(th.fonts.small, th.ui.text_dim).right(),
        );
    }

    fn paint_list(&self, p: &mut dyn Painter, size: Size, model: &Session, th: &Theme) {
        let list = Self::list_rect(size);
        p.fill(list, th.ui.background);
        p.vline(list.right() - 0.5, list.y, list.bottom(), th.ui.border);
        let m = model.mix_scenes();
        let shown = self.shown(model);
        let current = model.current_mix_scene();
        let fading = match model.scene_activity() {
            Some(SceneActivity::Fading { scene, progress }) => Some((scene, progress)),
            _ => None,
        };
        p.push_clip(list);
        for (i, s) in m.scenes.iter().enumerate() {
            let row = Self::row_rect(i, size);
            if row.y > list.bottom() {
                break;
            }
            if shown == Some(s.id) {
                p.fill(row, th.ui.selection.with_alpha(0.35));
            } else if self.hover == Some(Hit::Row(i)) {
                p.fill(row, th.ui.text.with_alpha(0.06));
            }
            if current == Some(s.id) {
                p.fill(Rect::new(row.x, row.y, 3.0, row.h), th.ui.accent);
            }
            if let Some((id, progress)) = fading
                && id == s.id
            {
                let bar = Rect::new(row.x, row.bottom() - 3.0, row.w * progress as f32, 3.0);
                p.fill(bar, th.ui.accent);
            }
            p.text(
                &format!("{}", i + 1),
                Rect::new(row.x + 8.0, row.y, 20.0, row.h),
                &TextStyle::new(th.fonts.small, th.ui.text_faint),
            );
            let mut tags = String::new();
            if m.a == Some(s.id) {
                tags.push_str(" A");
            }
            if m.b == Some(s.id) {
                tags.push_str(" B");
            }
            p.text(
                &s.name,
                Rect::new(row.x + 28.0, row.y + 3.0, row.w - 70.0, 18.0),
                &TextStyle::new(th.fonts.small, th.ui.text).bold(),
            );
            if !tags.is_empty() {
                p.text(
                    tags.trim(),
                    Rect::new(row.right() - 40.0, row.y + 3.0, 32.0, 18.0),
                    &TextStyle::new(th.fonts.small, th.ui.accent).bold().right(),
                );
            }
            let parts = ScopePart::ALL.iter().filter(|p| s.scope.has(**p)).count();
            let mut detail = vec![if parts == ScopePart::ALL.len() {
                "everything".to_string()
            } else {
                format!("{parts} of {} parts", ScopePart::ALL.len())
            }];
            if s.mix.keeps_plugins() {
                detail.push("plugins".into());
            }
            if !s.safe.is_empty() {
                detail.push(format!("{} safe", s.safe.len()));
            }
            detail.push(match s.fade {
                Some(f) => f.label(),
                None => format!("{} (project)", m.fade.length.label()),
            });
            p.text(
                &detail.join(" · "),
                Rect::new(row.x + 28.0, row.y + 21.0, row.w - 36.0, 16.0),
                &TextStyle::new(th.fonts.tiny + 0.5, th.ui.text_dim),
            );
            p.hline(
                row.x,
                row.right(),
                row.bottom() - 0.5,
                th.ui.border.with_alpha(0.5),
            );
        }
        if m.scenes.is_empty() {
            p.text(
                "No scenes yet: + Store keeps the console as it is",
                Rect::new(list.x + 12.0, list.y + 8.0, list.w - 20.0, 20.0),
                &TextStyle::new(th.fonts.small, th.ui.text_faint),
            );
        }
        p.pop_clip();
    }

    fn paint_panel(&self, p: &mut dyn Painter, size: Size, model: &Session, th: &Theme) {
        let panel = Self::panel_rect(size);
        if panel.w < 40.0 || panel.h < 30.0 {
            return;
        }
        let Some(scene) = self.shown_scene(model) else {
            return;
        };
        let label = TextStyle::new(th.fonts.small, th.ui.text_dim).bold();
        let chips = self.chips(size, model);
        let first = |pred: &dyn Fn(Hit) -> bool| {
            chips
                .iter()
                .find(|(h, _, _)| pred(*h))
                .map(|(_, r, _)| r.y - SECTION_H)
        };
        p.push_clip(panel);
        p.text(
            &format!("‘{}’ recalls", scene.name),
            Rect::new(panel.x, panel.y, panel.w, SECTION_H - 2.0),
            &label,
        );
        if let Some(y) = first(&|h| matches!(h, Hit::Safe(_))) {
            p.text(
                "Safe tracks (never touched by it)",
                Rect::new(panel.x, y, panel.w, SECTION_H - 2.0),
                &label,
            );
        }
        if let Some(y) = first(&|h| h == Hit::OwnFade) {
            p.text(
                "Its recall",
                Rect::new(panel.x, y, panel.w, SECTION_H - 2.0),
                &label,
            );
        }
        let keeps = scene.mix.keeps_plugins();
        for (hit, r, text) in &chips {
            let (on, enabled) = match *hit {
                Hit::Scope(ScopePart::Plugins) => (scene.scope.plugins && keeps, keeps),
                Hit::Scope(part) => (scene.scope.has(part), true),
                Hit::Safe(t) => (scene.safe.contains(&t), true),
                _ => (false, true),
            };
            let fill = if on {
                th.ui.accent.with_alpha(0.45)
            } else if self.hover == Some(*hit) {
                th.ui.surface_alt.with_alpha(0.8)
            } else {
                th.ui.surface_alt
            };
            p.fill_rounded(*r, CHIP_H / 2.0, &Paint::Solid(fill));
            p.stroke_rounded(*r, CHIP_H / 2.0, 1.0, th.ui.border);
            p.text(
                text,
                *r,
                &TextStyle::new(
                    th.fonts.small,
                    if enabled {
                        th.ui.text
                    } else {
                        th.ui.text_faint
                    },
                )
                .center(),
            );
        }
        p.pop_clip();
    }

    fn paint_strip(&self, p: &mut dyn Painter, size: Size, model: &Session, th: &Theme) {
        let s = Self::strip_rect(size);
        p.fill(s, th.ui.surface);
        p.hline(0.0, size.w, s.y + 0.5, th.ui.border);
        let comparing = match model.scene_activity() {
            Some(SceneActivity::Comparing { hearing, .. }) => Some(hearing),
            _ => None,
        };
        for (hit, r) in Self::strip(size, model) {
            if hit == Hit::Morph {
                self.paint_morph(p, r, model, th);
                continue;
            }
            let lit = match hit {
                Hit::HearA => comparing == Some(SceneSide::A),
                Hit::HearB => comparing == Some(SceneSide::B),
                _ => false,
            };
            self.paint_button(p, r, hit, lit, model, th);
        }
    }

    fn paint_morph(&self, p: &mut dyn Painter, r: Rect, model: &Session, th: &Theme) {
        let enabled = self.enabled(Hit::Morph, model);
        let small = TextStyle::new(th.fonts.small, th.ui.text_dim);
        p.text("A", Rect::new(r.x, r.y, 20.0, r.h), &small.center());
        p.text(
            "B",
            Rect::new(r.right() - 20.0, r.y, 20.0, r.h),
            &small.center(),
        );
        let track = r.inset_xy(28.0, 0.0);
        let mid = track.y + track.h / 2.0;
        let groove = Rect::new(track.x, mid - 2.0, track.w, 4.0);
        p.fill_rounded(groove, 2.0, &Paint::Solid(th.ui.border));
        let pos = self.morph_position(model) as f32;
        let x = track.x + pos * track.w;
        p.fill_rounded(
            Rect::new(track.x, mid - 2.0, x - track.x, 4.0),
            2.0,
            &Paint::Solid(th.ui.accent.with_alpha(if enabled { 0.8 } else { 0.3 })),
        );
        let knob = Rect::new(x - 7.0, mid - 9.0, 14.0, 18.0);
        p.fill_rounded(
            knob,
            3.0,
            &Paint::Solid(if enabled {
                th.ui.text
            } else {
                th.ui.text_faint
            }),
        );
        p.stroke_rounded(knob, 3.0, 1.0, th.ui.border);
    }

    fn paint_footer(&self, p: &mut dyn Painter, size: Size, model: &Session, th: &Theme) {
        let f = Rect::new(0.0, size.h - FOOTER_H, size.w, FOOTER_H);
        p.fill(f, th.ui.surface);
        p.hline(0.0, size.w, f.y + 0.5, th.ui.border);
        let text = match self.shown_scene(model) {
            Some(s) => format!(
                "‘{}’ holds {} tracks{} · double-click recalls, right-click for more",
                s.name,
                s.mix.tracks.len(),
                if s.mix.keeps_plugins() {
                    ", their devices' parameters and states"
                } else {
                    ""
                }
            ),
            None => "A scene keeps faders, pans, mutes, solos, sends, cue sends, bypass and (optionally) the plugins".into(),
        };
        p.text(
            &text,
            Rect::new(12.0, f.y, size.w - 24.0, FOOTER_H),
            &TextStyle::new(th.fonts.tiny + 0.5, th.ui.text_dim),
        );
    }

    // --- clicks --------------------------------------------------------------

    fn click(
        &mut self,
        hit: Hit,
        pos: Point,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) {
        let shown = self.shown(model);
        let m = model.mix_scenes();
        let rect_of = |hit: Hit| {
            Self::buttons(size, model)
                .into_iter()
                .chain(Self::strip(size, model))
                .find(|(h, _)| *h == hit)
                .map_or(Rect::new(pos.x, pos.y, 160.0, 24.0), |(_, r)| r)
        };
        match hit {
            Hit::Store => cx.emit(Action::MixScene(SceneOp::Store {
                name: None,
                plugins: false,
            })),
            Hit::StorePlugins => cx.emit(Action::MixScene(SceneOp::Store {
                name: None,
                plugins: true,
            })),
            Hit::Update => {
                if let Some(scene) = shown {
                    cx.emit(Action::MixScene(SceneOp::Update {
                        scene,
                        plugins: None,
                    }));
                }
            }
            Hit::Recall => {
                if let Some(scene) = shown {
                    cx.emit(Action::MixScene(SceneOp::Recall { scene, fade: None }));
                }
            }
            Hit::Stop => cx.emit(Action::MixScene(SceneOp::StopFade)),
            Hit::Previous => cx.emit(Action::MixScene(SceneOp::Previous)),
            Hit::Next => cx.emit(Action::MixScene(SceneOp::Next)),
            Hit::Fade => cx.request(HostRequest::ContextMenu {
                at: pos,
                items: Self::fade_menu(model),
            }),
            Hit::Switch => {
                let fade = m.fade;
                cx.request(HostRequest::ContextMenu {
                    at: pos,
                    items: Self::switch_items(fade.switch, |switch| {
                        Action::MixScene(SceneOp::SetFade {
                            scene: None,
                            fade: Some(SceneFade { switch, ..fade }),
                        })
                    }),
                });
            }
            Hit::Row(i) => {
                if let Some(s) = m.scenes.get(i) {
                    self.chosen = Some(s.id);
                    cx.redraw();
                }
            }
            Hit::Scope(part) => {
                if let Some(s) = self.shown_scene(model)
                    && (part != ScopePart::Plugins || s.mix.keeps_plugins())
                {
                    cx.emit(Action::MixScene(SceneOp::SetScope {
                        scene: s.id,
                        part,
                        on: !s.scope.has(part),
                    }));
                }
            }
            Hit::Safe(track) => {
                if let Some(s) = self.shown_scene(model) {
                    cx.emit(Action::MixScene(SceneOp::SetSafe {
                        scene: s.id,
                        track,
                        safe: !s.safe.contains(&track),
                    }));
                }
            }
            Hit::OwnFade => {
                if let Some(s) = self.shown_scene(model) {
                    cx.request(HostRequest::ContextMenu {
                        at: pos,
                        items: Self::own_fade_menu(model, s),
                    });
                }
            }
            Hit::PickA | Hit::PickB => cx.request(HostRequest::ContextMenu {
                at: pos,
                items: Self::pick_menu(model, hit == Hit::PickB),
            }),
            Hit::HearA | Hit::HearB => {
                let side = if hit == Hit::HearA {
                    SceneSide::A
                } else {
                    SceneSide::B
                };
                let Some(b) = m.b else { return };
                match model.scene_activity() {
                    Some(SceneActivity::Comparing { .. }) => {
                        cx.emit(Action::MixScene(SceneOp::CompareHear(side)));
                    }
                    _ => {
                        cx.emit(Action::MixScene(SceneOp::CompareStart { a: m.a, b }));
                        if side == SceneSide::A {
                            cx.emit(Action::MixScene(SceneOp::CompareHear(SceneSide::A)));
                        }
                    }
                }
            }
            Hit::Keep => cx.emit(Action::MixScene(SceneOp::CompareEnd { keep: true })),
            Hit::Back => cx.emit(Action::MixScene(SceneOp::CompareEnd { keep: false })),
            Hit::Morph => {
                if self.enabled(Hit::Morph, model) {
                    let position = Self::position_at(pos.x, rect_of(Hit::Morph));
                    self.morphing = true;
                    self.morph_at = position;
                    cx.emit(Action::MixScene(SceneOp::Morph { position }));
                }
            }
        }
    }
}

impl CanvasView<Session, Action> for ScenesView {
    fn set_theme(&mut self, theme: &Theme) {
        self.theme = theme.clone();
    }

    fn accessible_name(&self) -> Option<String> {
        Some("Mix Scenes".into())
    }

    fn accessible(
        &self,
        size: Size,
        model: &Session,
    ) -> Vec<faderframe_ui_canvas::AccessNode<Action>> {
        use faderframe_ui_canvas::{AccessNode, AccessRole, access_id};
        let current = model.current_mix_scene();
        let items = model.mix_scenes().scenes.iter().enumerate().map(|(i, s)| {
            AccessNode::new(
                access_id(&[61, s.id.raw()]),
                AccessRole::ListItem,
                format!("Scene {}: {}", i + 1, s.name),
            )
            .at(Self::row_rect(i, size))
            .selected(current == Some(s.id))
            .on_activate(Action::MixScene(SceneOp::Recall {
                scene: s.id,
                fade: None,
            }))
        });
        vec![
            AccessNode::new(access_id(&[60]), AccessRole::Button, "Store Scene").on_activate(
                Action::MixScene(SceneOp::Store {
                    name: None,
                    plugins: false,
                }),
            ),
            AccessNode::new(access_id(&[62]), AccessRole::List, "Mix Scenes")
                .at(Self::list_rect(size))
                .with_children(items),
        ]
    }

    fn wants_frames(&self, model: &Session) -> bool {
        model.scenes_moving()
    }

    fn paint(&mut self, p: &mut dyn Painter, size: Size, model: &Session, theme: &Theme) {
        let th = theme;
        p.fill(Rect::from_size(size), th.ui.background);
        self.paint_list(p, size, model, th);
        self.paint_panel(p, size, model, th);
        self.paint_header(p, size, model, th);
        self.paint_strip(p, size, model, th);
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
                if let Hit::Row(i) = hit
                    && clicks >= 2
                {
                    if let Some(s) = model.mix_scenes().scenes.get(i) {
                        cx.emit(Action::MixScene(SceneOp::Recall {
                            scene: s.id,
                            fade: None,
                        }));
                    }
                    return true;
                }
                if hit == Hit::Fade && clicks >= 2 {
                    let r = Self::buttons(size, model)
                        .into_iter()
                        .find(|(h, _)| *h == Hit::Fade)
                        .map_or(Rect::new(pos.x, pos.y, 150.0, 24.0), |(_, r)| r);
                    cx.request(Self::fade_input(model, None, r));
                    return true;
                }
                self.click(hit, pos, size, model, cx);
                true
            }
            ViewEvent::PointerDown {
                pos,
                button: PointerButton::Secondary,
                ..
            } => {
                let items = match self.hit(pos, size, model) {
                    Some(Hit::Row(i)) => {
                        let Some(s) = model.mix_scenes().scenes.get(i) else {
                            return false;
                        };
                        self.chosen = Some(s.id);
                        cx.redraw();
                        Self::row_menu(model, s, i)
                    }
                    Some(Hit::Morph) => Self::learn_menu(model, SceneControl::Morph),
                    Some(Hit::Previous) => Self::learn_menu(model, SceneControl::Previous),
                    Some(Hit::Next) => Self::learn_menu(model, SceneControl::Next),
                    Some(Hit::Fade) => {
                        let r = Self::buttons(size, model)
                            .into_iter()
                            .find(|(h, _)| *h == Hit::Fade)
                            .map_or(Rect::new(pos.x, pos.y, 150.0, 24.0), |(_, r)| r);
                        cx.request(Self::fade_input(model, None, r));
                        return true;
                    }
                    _ => return false,
                };
                cx.request(HostRequest::ContextMenu { at: pos, items });
                true
            }
            ViewEvent::PointerMove { pos, dragging, .. } => {
                if dragging && self.morphing {
                    if let Some((_, r)) = Self::strip(size, model)
                        .into_iter()
                        .find(|(h, _)| *h == Hit::Morph)
                    {
                        let position = Self::position_at(pos.x, r);
                        if (position - self.morph_at).abs() > 1e-4 {
                            self.morph_at = position;
                            cx.emit(Action::MixScene(SceneOp::Morph { position }));
                        }
                    }
                    return true;
                }
                let hover = self.hit(pos, size, model);
                if hover != self.hover {
                    self.hover = hover;
                    cx.redraw();
                }
                if hover == Some(Hit::Morph) {
                    cx.set_cursor(Cursor::ResizeHorizontal);
                }
                false
            }
            ViewEvent::PointerUp { .. } => {
                if std::mem::take(&mut self.morphing) {
                    cx.emit(Action::MixScene(SceneOp::EndMorph));
                    return true;
                }
                false
            }
            ViewEvent::PointerLeave => {
                if self.hover.take().is_some() {
                    cx.redraw();
                }
                false
            }
            // F2 renames the chosen scene, Enter recalls it, the arrows
            // choose another.
            ViewEvent::Key { key, .. } => {
                let scenes = &model.mix_scenes().scenes;
                let at = self
                    .shown(model)
                    .and_then(|id| model.mix_scenes().index_of(id));
                match (key, at) {
                    (Key::F(2), Some(i)) => {
                        cx.request(Self::rename_request(&scenes[i], Self::row_rect(i, size)));
                    }
                    (Key::Enter, Some(i)) => cx.emit(Action::MixScene(SceneOp::Recall {
                        scene: scenes[i].id,
                        fade: None,
                    })),
                    (Key::Up, Some(i)) if i > 0 => {
                        self.chosen = Some(scenes[i - 1].id);
                        cx.redraw();
                    }
                    (Key::Down, Some(i)) if i + 1 < scenes.len() => {
                        self.chosen = Some(scenes[i + 1].id);
                        cx.redraw();
                    }
                    _ => return false,
                }
                true
            }
            _ => false,
        }
    }

    fn cancel_drag(&mut self, cx: &mut EventCx<'_, Action>) {
        if std::mem::take(&mut self.morphing) {
            cx.emit(Action::MixScene(SceneOp::EndMorph));
        }
    }

    fn tooltip(&self, pos: Point, size: Size, model: &Session) -> Option<String> {
        Some(match self.hit(pos, size, model)? {
            Hit::Store => "Keep the console as it is as a new scene".into(),
            Hit::StorePlugins => {
                "Keep the console and every device's parameters and state as a new scene".into()
            }
            Hit::Update => "Store the console into the chosen scene again".into(),
            Hit::Recall => "Recall the chosen scene (with its crossfade)".into(),
            Hit::Stop => "Stop the crossfade where it is".into(),
            Hit::Previous | Hit::Next => {
                "Recall the scene before or after the one recalled last (right-click: MIDI learn)"
                    .into()
            }
            Hit::Fade => {
                "How long recalls take unless a scene has its own (double- or right-click to type: \
                 350 ms, 2 s, 4 beats)"
                    .into()
            }
            Hit::Switch => {
                "Where in a crossfade mutes, bypass, pan modes and other switches change".into()
            }
            Hit::Row(_) => "Click to choose, double-click to recall, right-click for more".into(),
            Hit::Scope(part) => format!("Recall {} (on or off)", part.label().to_lowercase()),
            Hit::Safe(_) => "A safe track is never touched by this scene's recalls".into(),
            Hit::OwnFade => "This scene's own crossfade and switch point".into(),
            Hit::PickA | Hit::PickB => "The scenes compared and morphed between".into(),
            Hit::HearA | Hit::HearB => {
                "Hear A or B (A without a scene: the mix as it is) — nothing is recorded until Keep"
                    .into()
            }
            Hit::Keep => "Keep the side heard (one undo step)".into(),
            Hit::Back => "Back to the mix as it was".into(),
            Hit::Morph => {
                "Morph between A and B; the drag is one undo step (right-click: MIDI learn)".into()
            }
        })
    }

    fn min_size(&self) -> Size {
        Size::new(560.0, 220.0)
    }
}

#[cfg(test)]
mod tests;
