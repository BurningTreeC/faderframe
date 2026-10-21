//! The arranger's scratch pad controls: the switcher in the corner (it
//! names the arrangement shown — the song or a pad — and its menu shows
//! another, makes, renames, duplicates and deletes pads and brings one
//! back), the entries the section and clip menus get, the tint of the
//! ruler while a pad is shown, and each arrangement's own scroll and zoom.

use crate::ArrangerView;
use faderframe_core::ScratchPadId;
use faderframe_session::scratch::{PadMaterial, ScratchOp, SongPlacement};
use faderframe_session::{Action, Session};
use faderframe_timeline::MusicalTime;
use faderframe_ui_canvas::{
    AccessNode, AccessRole, Align, FontWeight, MenuItem, Paint, Painter, Rect, TextStyle, access_id,
};

/// Access ids of the switcher's tabs.
const SWITCHER: u64 = 18;

fn pad(op: ScratchOp) -> Action {
    Action::ScratchPad(op)
}

/// `len` in bars from `from` in the song ("2 bars", "1.5 bars").
fn bars_text(model: &Session, from: MusicalTime, len: MusicalTime) -> String {
    let meter = &model.song_timeline().meter;
    let sig = meter.signature_of_bar(meter.bar_at(from));
    let per = f64::from(sig.numerator) * 4.0 / f64::from(sig.denominator);
    let bars = len.quarters() / per;
    let n = (bars * 100.0).round() / 100.0;
    let text = if n.fract().abs() < 1e-9 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    };
    format!(
        "{text} bar{}",
        if (n - 1.0).abs() < 1e-9 { "" } else { "s" }
    )
}

/// The song's bar number (from 1) at `at`.
fn bar_number(model: &Session, at: MusicalTime) -> i32 {
    model.song_timeline().meter.bar_at(at) + 1
}

/// "Copy to Scratch Pad ▸": into a new pad (shown), or after what another
/// pad holds.
pub(crate) fn copy_to_pad_menu(model: &Session, what: PadMaterial) -> MenuItem<Action> {
    let mut items = vec![MenuItem::new(
        "New Scratch Pad",
        pad(ScratchOp::New {
            name: None,
            from: Some(what.clone()),
        }),
    )];
    let shown = model.shown_pad();
    for (i, p) in model
        .scratch_pads()
        .iter()
        .filter(|p| Some(p.id) != shown)
        .enumerate()
    {
        let item = MenuItem::new(
            p.name.clone(),
            pad(ScratchOp::CopyTo {
                pad: p.id,
                what: what.clone(),
            }),
        );
        items.push(if i == 0 { item.separated() } else { item });
    }
    MenuItem::submenu("Copy to Scratch Pad", items)
}

/// The entries bringing `what` of `id` into the song: inserted or laid
/// over the song at its playhead's bar, or replacing a section — when
/// the lengths differ, both ways of doing that are offered by name.
pub(crate) fn to_song_items(
    model: &Session,
    id: ScratchPadId,
    what: PadMaterial,
) -> Vec<MenuItem<Action>> {
    let Some(len) = model.pad_material_length(id, &what) else {
        return vec![MenuItem::disabled("Nothing to bring")];
    };
    let at = model.song_bar_line(model.song_position());
    let n = bar_number(model, at);
    let to_song = |place| {
        pad(ScratchOp::ToSong {
            pad: id,
            what: what.clone(),
            place,
        })
    };
    let mut items = vec![
        MenuItem::new(
            format!("Insert at Bar {n} (What Follows Moves Later)"),
            to_song(SongPlacement::Insert { at }),
        ),
        MenuItem::new(
            format!("Overwrite from Bar {n} (Time Stays)"),
            to_song(SongPlacement::Overwrite { at }),
        ),
    ];
    let sections = model.song_sections();
    if sections.is_empty() {
        items.push(MenuItem::disabled("Replace Section (the Song Has None)").separated());
        return items;
    }
    let ours = bars_text(model, at, len);
    let replace = sections
        .iter()
        .map(|s| {
            let theirs = s.end - s.start;
            let place = |keep_length| SongPlacement::Replace {
                section: s.id,
                keep_length,
            };
            if theirs == len {
                return MenuItem::new(
                    format!("{} ({})", s.name, bars_text(model, s.start, theirs)),
                    to_song(place(false)),
                );
            }
            let theirs_text = bars_text(model, s.start, theirs);
            let (diff, later) = if len > theirs {
                (len - theirs, "Later")
            } else {
                (theirs - len, "Earlier")
            };
            MenuItem::submenu(
                format!("{} ({theirs_text}; the Material Is {ours})", s.name),
                vec![
                    MenuItem::new(
                        format!(
                            "Take the Material's Length (What Follows Moves {} {later})",
                            bars_text(model, s.start, diff)
                        ),
                        to_song(place(false)),
                    ),
                    MenuItem::new(
                        format!(
                            "Keep the Section's Length ({} the Material to {theirs_text})",
                            if len > theirs { "Cut" } else { "Pad" }
                        ),
                        to_song(place(true)),
                    ),
                ],
            )
        })
        .collect();
    items.push(MenuItem::submenu("Replace Section", replace).separated());
    items
}

/// "Bring to Song ▸".
pub(crate) fn to_song_menu(
    model: &Session,
    id: ScratchPadId,
    what: PadMaterial,
) -> MenuItem<Action> {
    MenuItem::submenu("Bring to Song", to_song_items(model, id, what))
}

/// Something to copy: clips selected or a range with a length.
fn has_selection(model: &Session) -> bool {
    model.selection.range.is_some_and(|r| !r.is_empty()) || !model.selection.clips.is_empty()
}

/// The switcher's menu: the arrangements to show, new pads, and what the
/// shown pad (or, from the song, any pad) can do.
pub(crate) fn arrangement_menu(model: &Session) -> Vec<MenuItem<Action>> {
    let shown = model.shown_pad();
    let mut items =
        vec![MenuItem::new("Song", pad(ScratchOp::Show(None))).checked(shown.is_none())];
    for p in model.scratch_pads() {
        items.push(
            MenuItem::new(p.name.clone(), pad(ScratchOp::Show(Some(p.id))))
                .checked(shown == Some(p.id)),
        );
    }
    items.push(
        MenuItem::new(
            "New Scratch Pad",
            pad(ScratchOp::New {
                name: None,
                from: None,
            }),
        )
        .separated(),
    );
    items.push(if has_selection(model) {
        MenuItem::new(
            "New Scratch Pad from Selection",
            pad(ScratchOp::New {
                name: None,
                from: Some(PadMaterial::Selection),
            }),
        )
    } else {
        MenuItem::disabled("New Scratch Pad from Selection")
    });
    let sections = &model.project().sections;
    items.push(if sections.is_empty() {
        MenuItem::disabled("New Scratch Pad from Section")
    } else {
        MenuItem::submenu(
            "New Scratch Pad from Section",
            sections
                .iter()
                .map(|s| {
                    MenuItem::new(
                        s.name.clone(),
                        pad(ScratchOp::New {
                            name: None,
                            from: Some(PadMaterial::Section(s.id)),
                        }),
                    )
                })
                .collect(),
        )
    });
    match shown {
        Some(id) => {
            let name = model.shown_arrangement_name();
            items.push(
                MenuItem::new(
                    format!("Rename ‘{name}’…"),
                    pad(ScratchOp::PromptRename(id)),
                )
                .separated(),
            );
            items.push(MenuItem::new(
                format!("Duplicate ‘{name}’"),
                pad(ScratchOp::Duplicate(id)),
            ));
            items.push(MenuItem::new(
                format!("Delete ‘{name}’"),
                pad(ScratchOp::Delete(id)),
            ));
            items.push(MenuItem::submenu(
                format!("Bring ‘{name}’ to Song"),
                to_song_items(model, id, PadMaterial::All),
            ));
        }
        None if !model.scratch_pads().is_empty() => {
            let pads: Vec<MenuItem<Action>> = model
                .scratch_pads()
                .iter()
                .map(|p| MenuItem::submenu(p.name.clone(), pad_items(model, p.id)))
                .collect();
            items.push(MenuItem::submenu("Scratch Pads", pads).separated());
        }
        None => {}
    }
    items
}

/// What a pad put aside can do (from the song).
fn pad_items(model: &Session, id: ScratchPadId) -> Vec<MenuItem<Action>> {
    vec![
        MenuItem::new("Show", pad(ScratchOp::Show(Some(id)))),
        MenuItem::new("Rename…", pad(ScratchOp::PromptRename(id))),
        MenuItem::new("Duplicate", pad(ScratchOp::Duplicate(id))),
        MenuItem::new("Delete", pad(ScratchOp::Delete(id))),
        MenuItem::submenu("Bring to Song", to_song_items(model, id, PadMaterial::All)),
    ]
}

/// The section and clip menus' scratch pad entries for `what`: copying it
/// to a pad, and in a pad bringing it to the song.
pub(crate) fn material_items(model: &Session, what: PadMaterial) -> Vec<MenuItem<Action>> {
    let mut items = vec![copy_to_pad_menu(model, what.clone()).separated()];
    if let Some(id) = model.shown_pad() {
        items.push(to_song_menu(model, id, what));
    }
    items
}

impl ArrangerView {
    /// The switcher in the corner, at its left.
    pub(crate) fn arrangement_rect(&self, model: &Session) -> Rect {
        let h = self.base_ruler_h();
        let chars = model.shown_arrangement_name().chars().count().min(18) as f32;
        let w = (chars * 6.5 + 30.0).clamp(58.0, self.header_w() * 0.45);
        Rect::new(6.0, 4.0, w, (h - 8.0).max(12.0))
    }

    /// The switcher: the arrangement's name, an accent while a pad is
    /// shown.
    pub(crate) fn paint_arrangement(&self, p: &mut dyn Painter, model: &Session) {
        let th = &self.theme;
        let r = self.arrangement_rect(model);
        let in_pad = model.shown_pad().is_some();
        let (bg, fg) = if in_pad {
            (th.ui.accent, th.ui.text)
        } else {
            (th.arranger.ruler_bg.lighten(0.14), th.ui.text_dim)
        };
        p.fill_rounded(r, 4.0, &Paint::Solid(bg));
        let style = TextStyle::new(th.fonts.small, fg)
            .weight(if in_pad {
                FontWeight::Bold
            } else {
                FontWeight::Normal
            })
            .align(Align::Start);
        let mut name = model.shown_arrangement_name();
        let room = ((r.w - 22.0) / 6.0).max(3.0) as usize;
        if name.chars().count() > room {
            name = name
                .chars()
                .take(room.saturating_sub(1))
                .collect::<String>()
                + "…";
        }
        p.text(&name, r.inset_xy(7.0, 0.0), &style);
        p.text(
            "▾",
            Rect::new(r.right() - 14.0, r.y, 12.0, r.h),
            &TextStyle::new(th.fonts.small, fg).center(),
        );
    }

    /// While a pad is shown, the ruler says so.
    pub(crate) fn paint_pad_tint(&self, p: &mut dyn Painter, ruler: Rect, model: &Session) {
        if model.shown_pad().is_none() {
            return;
        }
        let accent = self.theme.ui.accent;
        p.fill(ruler, accent.with_alpha(0.10));
        p.fill(
            Rect::new(0.0, ruler.bottom() - 2.0, ruler.right(), 2.0),
            accent,
        );
    }

    /// Keep each arrangement's scroll and zoom: showing another puts back
    /// where it was left (the start, for one never shown).
    pub(crate) fn follow_arrangement(&mut self, model: &Session) {
        let now = model.shown_pad();
        if let Some(before) = self.arrangement_seen
            && before != now
        {
            self.places
                .insert(before, (self.scroll_x, self.ppq, self.scroll_y));
            let (x, ppq, y) =
                self.places
                    .get(&now)
                    .copied()
                    .unwrap_or((0.0, self.ppq, self.scroll_y));
            self.scroll_x = x;
            self.ppq = ppq;
            self.scroll_y = y;
        }
        self.arrangement_seen = Some(now);
    }

    /// The switcher for screen readers: a tab per arrangement.
    pub(crate) fn arrangement_access(&self, model: &Session) -> AccessNode<Action> {
        let shown = model.shown_pad();
        let r = self.arrangement_rect(model);
        let tab = |id: u64, name: String, pad_id: Option<ScratchPadId>| {
            AccessNode::new(access_id(&[SWITCHER, id]), AccessRole::Tab, name)
                .at(r)
                .selected(shown == pad_id)
                .on_activate(pad(ScratchOp::Show(pad_id)))
        };
        let mut node = AccessNode::new(
            access_id(&[SWITCHER, 0]),
            AccessRole::TabList,
            "Arrangement shown",
        )
        .at(r)
        .described("The song or a scratch pad")
        .child(tab(1, "Song".into(), None));
        for p in model.scratch_pads() {
            node = node.child(tab(p.id.raw() + 1, p.name.clone(), Some(p.id)));
        }
        node
    }
}
