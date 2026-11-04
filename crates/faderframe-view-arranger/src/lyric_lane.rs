//! Comping by lyric in the arranger (see `faderframe_session::lyric_comp`):
//! the words each take sings, under its waveform in the take lanes — a
//! click uses that take for the word, Shift+click for the words from the
//! last one clicked, a double-click for the whole line — and the lyrics
//! comp row under the lanes: the line word by word, each in the colour of
//! the take chosen for it with that take's number, a star where another
//! take is suggested (a click uses the suggestion; right-click lists every
//! take with its reasons).

use crate::{ArrangerView, Hit};
use faderframe_core::ClipId;
use faderframe_project::{Clip, TakeFolder, Track};
use faderframe_session::lyric_comp::{LyricComp, LyricCompOp};
use faderframe_session::{Action, Session};
use faderframe_ui_canvas::{
    Color, EventCx, HostRequest, MenuItem, Modifiers, Paint, Painter, Point, Rect, TextStyle,
};

/// Height of the lyrics comp row under an open folder's take lanes.
pub(crate) const LYRIC_ROW_H: f32 = 22.0;
/// Height of the word strip at the bottom of a take lane.
const STRIP_H: f32 = 13.0;
/// Lanes lower than this show no words.
const MIN_LANE_H: f32 = 18.0;

/// The last word clicked in a take lane (Shift+click comps from it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct WordAnchor {
    pub clip: ClipId,
    pub word: usize,
}

impl ArrangerView {
    /// Is the lyrics comp row shown under this folder's lanes?
    pub(crate) fn has_lyric_row(model: &Session, clip: ClipId) -> bool {
        model.takes_open(clip)
            && (model.hearing_words(clip).is_some() || model.lyric_comp(clip).is_some())
    }

    /// Height of the lyrics comp row under a track's take lanes.
    pub(crate) fn lyric_row_h(t: &Track, model: &Session) -> f32 {
        let shown = model
            .project()
            .clips_of(t.id)
            .into_iter()
            .any(|c| c.as_takes().is_some() && Self::has_lyric_row(model, c.id));
        if shown { LYRIC_ROW_H } else { 0.0 }
    }

    /// The word strip of a take lane.
    pub(crate) fn word_strip(lane: Rect) -> Option<Rect> {
        (lane.h >= MIN_LANE_H).then(|| {
            let h = STRIP_H.min(lane.h - 4.0);
            Rect::new(lane.x, lane.bottom() - h - 1.0, lane.w, h)
        })
    }

    /// x of a folder frame of `clip`.
    pub(crate) fn x_of_folder(&self, model: &Session, clip: &Clip, frame: i64) -> f32 {
        let base = Self::clip_start_sample(model, clip);
        self.x_of_sample(model, base + Self::engine_frames(model, frame))
    }

    /// Take `take`'s words in its lane: (column, chip).
    fn word_chips(
        &self,
        model: &Session,
        clip: &Clip,
        comp: &LyricComp,
        take: usize,
        strip: Rect,
    ) -> Vec<(usize, Rect)> {
        (0..comp.alignment.columns.len())
            .filter_map(|c| {
                let (a, b) = comp.alignment.span(c, take)?;
                let x0 = self.x_of_folder(model, clip, a);
                let x1 = self.x_of_folder(model, clip, b).max(x0 + 3.0);
                Some((c, Rect::new(x0, strip.y, x1 - x0, strip.h)))
            })
            .collect()
    }

    /// The word under `pos` in take lane `take` (lane rect `lane`).
    pub(crate) fn take_word_at(
        &self,
        model: &Session,
        clip: &Clip,
        take: usize,
        lane: Rect,
        pos: Point,
    ) -> Option<usize> {
        let strip = Self::word_strip(lane)?;
        if !strip.contains(pos) {
            return None;
        }
        let comp = model.lyric_comp(clip.id)?;
        self.word_chips(model, clip, &comp, take, strip)
            .into_iter()
            .find(|(_, r)| r.x <= pos.x && pos.x < r.right())
            .map(|(c, _)| c)
    }

    /// Paint take `take`'s words in its lane.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn paint_take_words(
        &self,
        p: &mut dyn Painter,
        lane: Rect,
        vis: (f32, f32),
        clip: &Clip,
        f: &TakeFolder,
        take: usize,
        model: &Session,
    ) {
        let (Some(strip), Some(comp)) = (Self::word_strip(lane), model.lyric_comp(clip.id)) else {
            return;
        };
        let ui = &self.theme.ui;
        let style = TextStyle::new(self.theme.fonts.small, ui.text_dim);
        let chosen_style = TextStyle::new(self.theme.fonts.small, ui.background);
        for (c, r) in self.word_chips(model, clip, &comp, take, strip) {
            if r.right() < vis.0 || r.x > vis.1 {
                continue;
            }
            let chosen = comp.alignment.chosen(c, f) == Some(take);
            let fill = if chosen {
                ui.accent.with_alpha(0.85)
            } else {
                ui.surface.with_alpha(0.8)
            };
            p.fill_rounded(r, 3.0, &Paint::Solid(fill));
            if comp.best.get(c).copied().flatten() == Some(take) && !chosen {
                p.stroke_rounded(r, 3.0, 1.0, ui.accent);
            }
            let text = &comp.alignment.columns[c].text;
            let word = comp
                .alignment
                .word(c, take)
                .map_or(text.as_str(), |w| w.text.as_str());
            if r.w > 8.0 {
                p.push_clip(r);
                p.text(
                    word,
                    Rect::new(r.x + 3.0, r.y, r.w.max(40.0), r.h),
                    if chosen { &chosen_style } else { &style },
                );
                p.pop_clip();
            }
        }
    }

    /// The lyrics comp row's word chips: (column, chip, chosen take).
    fn lyric_chips(
        &self,
        model: &Session,
        clip: &Clip,
        f: &TakeFolder,
        comp: &LyricComp,
        row: Rect,
    ) -> Vec<(usize, Rect, Option<usize>)> {
        (0..comp.alignment.columns.len())
            .filter_map(|c| {
                let (a, b) = comp.alignment.shown_span(c, f)?;
                let x0 = self.x_of_folder(model, clip, a);
                let x1 = self.x_of_folder(model, clip, b).max(x0 + 3.0);
                Some((
                    c,
                    Rect::new(x0, row.y + 3.0, x1 - x0, row.h - 6.0),
                    comp.alignment.chosen(c, f),
                ))
            })
            .collect()
    }

    /// The word under `pos` in a folder's lyrics comp row (`row`: the row).
    pub(crate) fn lyric_word_at(
        &self,
        model: &Session,
        clip: &Clip,
        row: Rect,
        pos: Point,
    ) -> Option<usize> {
        let f = clip.as_takes()?;
        let comp = model.lyric_comp(clip.id)?;
        self.lyric_chips(model, clip, f, &comp, row)
            .into_iter()
            .find(|(_, r, _)| r.x <= pos.x && pos.x < r.right())
            .map(|(c, _, _)| c)
    }

    /// Paint a folder's lyrics comp row.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn paint_lyric_row(
        &self,
        p: &mut dyn Painter,
        clip_rect: Rect,
        row: Rect,
        lanes: Rect,
        clip: &Clip,
        f: &TakeFolder,
        model: &Session,
        color: Color,
    ) {
        let vis = (
            clip_rect.x.max(lanes.x),
            clip_rect.right().min(lanes.right()),
        );
        if vis.1 <= vis.0 {
            return;
        }
        let ui = &self.theme.ui;
        p.fill(
            Rect::new(vis.0, row.y, vis.1 - vis.0, row.h - 1.0),
            ui.surface_alt.with_alpha(0.9),
        );
        let small = TextStyle::new(self.theme.fonts.small, ui.text_dim);
        if let Some(progress) = model.hearing_words(clip.id) {
            let label = if progress > 0.0 {
                format!("Listening to the words… {:.0} %", progress * 100.0)
            } else {
                "Listening to the words…".to_string()
            };
            p.text(&label, Rect::new(vis.0 + 6.0, row.y, 300.0, row.h), &small);
            return;
        }
        let Some(comp) = model.lyric_comp(clip.id) else {
            return;
        };
        let text = TextStyle::new(self.theme.fonts.small, ui.text);
        let star = TextStyle::new(self.theme.fonts.small, ui.accent);
        for (c, r, chosen) in self.lyric_chips(model, clip, f, &comp, row) {
            if r.right() < vis.0 || r.x > vis.1 {
                continue;
            }
            // Each take its shade of the clip's colour.
            let fill = match chosen {
                Some(t) => take_shade(color, t),
                None => ui.surface.with_alpha(0.6),
            };
            p.fill_rounded(r, 3.0, &Paint::Solid(fill));
            if comp.alignment.columns[c].line {
                p.vline(r.x, row.y + 1.0, row.bottom() - 1.0, ui.text_faint);
            }
            let best = comp.best.get(c).copied().flatten();
            let mut label = comp.alignment.columns[c].text.clone();
            if let Some(t) = chosen {
                label = format!("{label} {}", t + 1);
            }
            p.push_clip(r);
            p.text(&label, Rect::new(r.x + 3.0, r.y, r.w.max(60.0), r.h), &text);
            p.pop_clip();
            if best.is_some() && best != chosen {
                // Another take is suggested: a star at the chip's end.
                let w = p.text_width("★", &star);
                p.text(
                    "★",
                    Rect::new(r.right() - w - 2.0, r.y, w + 2.0, r.h),
                    &star,
                );
            }
        }
    }

    /// A click on a take's word: that take for it (Shift: from the last
    /// word clicked; double-click: the line).
    pub(crate) fn press_take_word(
        &mut self,
        clip: ClipId,
        take: usize,
        word: usize,
        clicks: u32,
        mods: Modifiers,
        cx: &mut EventCx<'_, Action>,
    ) {
        let op = if clicks >= 2 {
            LyricCompOp::CompLine { clip, word, take }
        } else if mods.shift
            && let Some(a) = self.word_anchor.filter(|a| a.clip == clip)
        {
            LyricCompOp::Comp {
                clip,
                from: a.word.min(word),
                to: a.word.max(word),
                take,
            }
        } else {
            LyricCompOp::Comp {
                clip,
                from: word,
                to: word,
                take,
            }
        };
        if !mods.shift {
            self.word_anchor = Some(WordAnchor { clip, word });
        }
        cx.emit(Action::LyricComp(op));
    }

    /// Menu of a word in a take lane.
    pub(crate) fn take_word_menu(
        model: &Session,
        clip: ClipId,
        take: usize,
        word: usize,
        at: Point,
    ) -> HostRequest<Action> {
        let mut items = Vec::new();
        if let Some(comp) = model.lyric_comp(clip) {
            let text = comp
                .alignment
                .columns
                .get(word)
                .map_or(String::new(), |c| c.text.clone());
            items.push(MenuItem::new(
                format!("Use “{text}” from Take {}", take + 1),
                Action::LyricComp(LyricCompOp::Comp {
                    clip,
                    from: word,
                    to: word,
                    take,
                }),
            ));
            items.push(MenuItem::new(
                format!("Use the Line from Take {}", take + 1),
                Action::LyricComp(LyricCompOp::CompLine { clip, word, take }),
            ));
            let why = comp.reasons(word, take, 4);
            if !why.is_empty() {
                items.push(MenuItem::disabled(why.join(" · ")).separated());
            }
        }
        items.extend(Self::lyric_items(model, clip));
        HostRequest::ContextMenu { at, items }
    }

    /// Menu of a word in the lyrics comp row: every take that sings it,
    /// with its reasons.
    pub(crate) fn lyric_word_menu(
        model: &Session,
        clip: ClipId,
        word: usize,
        at: Point,
    ) -> HostRequest<Action> {
        let mut items = Vec::new();
        if let (Some(comp), Some(f)) = (
            model.lyric_comp(clip),
            model.project().clip(clip).and_then(|c| c.as_takes()),
        ) {
            let chosen = comp.alignment.chosen(word, f);
            let best = comp.best.get(word).copied().flatten();
            let n = comp
                .alignment
                .columns
                .get(word)
                .map_or(0, |c| c.takes.len());
            for t in (0..n).filter(|&t| comp.alignment.span(word, t).is_some()) {
                let name = f
                    .takes
                    .get(t)
                    .map_or_else(|| format!("Take {}", t + 1), |x| x.name.clone());
                let mut label = format!("{} · {name}", t + 1);
                if best == Some(t) {
                    label.push_str(" ★ suggested");
                }
                let why = comp.reasons(word, t, 3);
                if !why.is_empty() {
                    label.push_str(&format!(" — {}", why.join(", ")));
                }
                items.push(
                    MenuItem::new(
                        label,
                        Action::LyricComp(LyricCompOp::Comp {
                            clip,
                            from: word,
                            to: word,
                            take: t,
                        }),
                    )
                    .checked(chosen == Some(t)),
                );
            }
            let line = comp.alignment.line(word);
            items.push(
                MenuItem::new(
                    "Use the Suggestions for This Line",
                    Action::LyricComp(LyricCompOp::UseBest {
                        clip,
                        words: Some((line.start, line.end.saturating_sub(1))),
                    }),
                )
                .separated(),
            );
        }
        items.extend(Self::lyric_items(model, clip));
        HostRequest::ContextMenu { at, items }
    }

    /// The folder-wide lyric entries (take lane and clip menus).
    pub(crate) fn lyric_items(model: &Session, clip: ClipId) -> Vec<MenuItem<Action>> {
        let heard = model.lyric_comp(clip).is_some();
        let mut items = Vec::new();
        if heard {
            items.push(
                MenuItem::new(
                    "Use All Suggested Takes",
                    Action::LyricComp(LyricCompOp::UseBest { clip, words: None }),
                )
                .separated(),
            );
            items.push(MenuItem::new(
                "Hear the Words Again",
                Action::LyricComp(LyricCompOp::HearAgain { clip }),
            ));
            items.push(MenuItem::new(
                "Forget the Words",
                Action::LyricComp(LyricCompOp::Forget { clip }),
            ));
        } else if model.hearing_words(clip).is_some() {
            items.push(MenuItem::disabled("Listening to the words…").separated());
        } else {
            items.push(
                MenuItem::new(
                    "Comp by Lyrics (Hear the Words of Every Take)",
                    Action::LyricComp(LyricCompOp::Hear { clip }),
                )
                .separated(),
            );
        }
        items
    }

    /// The tooltip of a lyric hit.
    pub(crate) fn lyric_tooltip(model: &Session, hit: &Hit) -> Option<String> {
        match *hit {
            Hit::TakeWord { clip, take, word } => {
                let comp = model.lyric_comp(clip)?;
                let w = comp.alignment.word(word, take)?;
                let why = comp.reasons(word, take, 3);
                Some(format!(
                    "“{}” from Take {} — click: use it here · Shift+click: from the last word · \
                     double-click: the line{}",
                    w.text,
                    take + 1,
                    if why.is_empty() {
                        String::new()
                    } else {
                        format!("\n{}", why.join(" · "))
                    }
                ))
            }
            Hit::LyricWord { clip, word } => {
                let comp = model.lyric_comp(clip)?;
                let text = &comp.alignment.columns.get(word)?.text;
                Some(match comp.best.get(word).copied().flatten() {
                    Some(t) => format!(
                        "“{text}”: Take {} is suggested ({}) — click to use it · right-click: \
                         every take",
                        t + 1,
                        comp.reasons(word, t, 3).join(", ")
                    ),
                    None => format!("“{text}” — right-click: every take"),
                })
            }
            _ => None,
        }
    }
}

/// Take `t`'s shade of the folder's colour (alternating lighter and
/// darker steps, so neighbours differ).
fn take_shade(color: Color, t: usize) -> Color {
    let step = 0.12 * ((t / 2) as f32 + 1.0);
    if t.is_multiple_of(2) {
        color.lighten(step.min(0.5))
    } else {
        color.darken(step.min(0.5))
    }
}
