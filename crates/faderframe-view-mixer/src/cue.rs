//! The mixer's cue mode: a bar over the strips (while the project has cue
//! mixes) chooses what the faders set — the main mix or a cue mix. In a
//! cue mix every strip's fader, pan and mute set that channel's send to it
//! (the mute: the send off), drawn in the cue mix's colour so nobody takes
//! them for the main mix; solo stays the main mix's (dimmed); the master
//! section becomes the cue mix's own: its level, mute, click (switch and
//! level on the pan knob), meters and outputs.

use crate::{MixerView, track_color};
use faderframe_core::CueId;
use faderframe_core::gain::format_db;
use faderframe_project::cue::{CueMix, CueSend, feeds_cues};
use faderframe_project::{Command, SendTap, Track, TrackKind};
use faderframe_session::cues::CueOp;
use faderframe_session::{Action, Session};
use faderframe_ui_canvas::{
    Color, EventCx, HostRequest, MenuItem, Painter, Point, PointerButton, Rect, TextStyle, Theme,
};

/// The bar's height (only while the project has cue mixes).
pub(crate) const CUE_BAR_H: f32 = 26.0;

/// What the bar has under the pointer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BarHit {
    /// Back to the main mix.
    Main,
    Cue(CueId),
    /// A new cue mix.
    New,
    /// The shown cue mix: every send as the main mix has it.
    CopyMain,
    /// The shown cue mix: pre- or post-fader.
    Tap,
}

/// What a strip's fader, pan and mute set now.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Sets {
    Main,
    /// The track's send to this cue mix.
    Send(CueId),
    /// The master section: this cue mix's own level, mute and click.
    CueMaster(CueId),
    /// Nothing in cue mode (a VCA, a folder): it has no cue send.
    Nothing,
}

pub(crate) fn sets(model: &Session, t: &Track) -> Sets {
    match model.mixer_cue() {
        None => Sets::Main,
        Some(c) if t.kind == TrackKind::Master => Sets::CueMaster(c.id),
        Some(c) if feeds_cues(t.kind) => Sets::Send(c.id),
        Some(_) => Sets::Nothing,
    }
}

pub(crate) fn cue_color(c: &CueMix) -> Color {
    track_color(c.color)
}

/// "Pre-fader" or "post-fader".
fn tap_name(c: &CueMix) -> &'static str {
    if c.post_fader() {
        "post-fader"
    } else {
        "pre-fader"
    }
}

impl MixerView {
    /// The level a strip's fader shows (dB).
    pub(crate) fn fader_db(model: &Session, t: &Track) -> f32 {
        match sets(model, t) {
            Sets::Main | Sets::Nothing => model.shown_volume_db(t),
            Sets::Send(cue) => model.cue_send(t, cue).level_db,
            Sets::CueMaster(cue) => model.project().cue(cue).map_or(0.0, |c| c.level_db),
        }
    }

    /// The edit a strip's fader at `db` makes (`None`: it sets nothing).
    pub(crate) fn fader_command(model: &Session, t: &Track, db: f32) -> Option<Command> {
        Some(match sets(model, t) {
            Sets::Main => Command::SetTrackVolume { track: t.id, db },
            Sets::Send(cue) => Command::SetCueSendLevel {
                track: t.id,
                cue,
                db,
            },
            Sets::CueMaster(cue) => Command::SetCueLevel { cue, db },
            Sets::Nothing => return None,
        })
    }

    /// The pan a strip's knob shows (the master's in a cue mix: none).
    pub(crate) fn pan_shown(model: &Session, t: &Track) -> f32 {
        match sets(model, t) {
            Sets::Send(cue) => model.cue_send(t, cue).pan,
            _ => model.shown_pan(t),
        }
    }

    /// The edit a strip's (single) pan knob at `pan` makes.
    pub(crate) fn pan_command(model: &Session, t: &Track, pan: f32) -> Option<Command> {
        Some(match sets(model, t) {
            Sets::Main => Command::SetTrackPan { track: t.id, pan },
            Sets::Send(cue) => Command::SetCueSendPan {
                track: t.id,
                cue,
                pan,
            },
            Sets::CueMaster(_) | Sets::Nothing => return None,
        })
    }

    /// Whether a strip's mute button is lit (a cue send: off).
    pub(crate) fn mute_shown(model: &Session, t: &Track) -> bool {
        match sets(model, t) {
            Sets::Main | Sets::Nothing => model.shown_mute(t),
            Sets::Send(cue) => !model.cue_send(t, cue).on,
            Sets::CueMaster(cue) => model.project().cue(cue).is_some_and(|c| c.mute),
        }
    }

    /// The edit a press of a strip's mute button makes.
    pub(crate) fn mute_command(model: &Session, t: &Track) -> Option<Command> {
        Some(match sets(model, t) {
            Sets::Main => Command::SetTrackMute {
                track: t.id,
                on: !t.mute,
            },
            Sets::Send(cue) => Command::SetCueSendOn {
                track: t.id,
                cue,
                on: !model.cue_send(t, cue).on,
            },
            Sets::CueMaster(cue) => Command::SetCueMute {
                cue,
                on: !model.project().cue(cue)?.mute,
            },
            Sets::Nothing => return None,
        })
    }

    /// The click's switch in the shown cue mix (its master's MONO place).
    pub(crate) fn click_command(
        model: &Session,
        cue: CueId,
        on: Option<bool>,
        db: Option<f32>,
    ) -> Option<Command> {
        let c = model.project().cue(cue)?;
        Some(Command::SetCueClick {
            cue,
            on: on.unwrap_or(c.click),
            db: db.unwrap_or(c.click_db),
        })
    }

    /// A strip in cue mode: framed in the cue mix's colour (it sets the
    /// cue mix), dimmed when it sets nothing there.
    pub(crate) fn paint_cue_mark(
        &self,
        p: &mut dyn Painter,
        rect: Rect,
        t: &Track,
        model: &Session,
    ) {
        let Some(cue) = model.mixer_cue() else {
            return;
        };
        let th = &self.theme;
        let col = cue_color(cue);
        match sets(model, t) {
            Sets::Main => {}
            Sets::Send(_) => {
                p.stroke_rounded(rect.inset(1.0), 3.0, 2.0, col.with_alpha(0.85));
                // The cue mix's name over the strip's number.
                let tag = Rect::new(rect.x + 3.0, rect.y + 3.0, rect.w - 6.0, 12.0);
                p.fill_rounded(
                    tag,
                    3.0,
                    &faderframe_ui_canvas::Paint::Solid(col.with_alpha(0.8)),
                );
                let style = TextStyle::new(th.fonts.tiny, th.ui.text).bold().center();
                p.push_clip(tag);
                p.text(&cue.name.to_uppercase(), tag, &style);
                p.pop_clip();
            }
            Sets::CueMaster(_) => {
                p.stroke_rounded(rect.inset(1.0), 3.0, 3.0, col);
            }
            Sets::Nothing => {
                p.fill(rect, th.ui.background.with_alpha(0.6));
                let style = TextStyle::new(th.fonts.tiny, th.ui.text_dim).center();
                let r = Rect::new(rect.x, rect.center().y - 20.0, rect.w, 40.0);
                p.push_clip(rect);
                p.text("not in", Rect::new(r.x, r.y, r.w, 14.0), &style);
                p.text("cue mixes", Rect::new(r.x, r.y + 14.0, r.w, 14.0), &style);
                p.pop_clip();
            }
        }
    }

    /// The master section in a cue mix: the cue mix's own channel — its
    /// name, how it taps and where it plays, the click (switch on the MONO
    /// place, level on the pan knob), mute, level fader and meters.
    pub(crate) fn paint_cue_master(
        &self,
        p: &mut dyn Painter,
        rect: Rect,
        l: &crate::StripLayout,
        cue: CueId,
        model: &Session,
    ) {
        use faderframe_ui_canvas::Align;
        use faderframe_ui_canvas::controls::{self, FaderGeometry, KnobLook};
        let Some(c) = model.project().cue(cue) else {
            return;
        };
        let th = &self.theme;
        let k = &th.console;
        let col = cue_color(c);
        controls::panel(p, rect, k.master_panel_top, k.master_panel_bottom, th);
        p.fill(l.color_bar, col);
        for &y in &l.dividers {
            controls::section_line(p, rect.x + 4.0, rect.right() - 4.0, y, th);
        }
        controls::engraved(p, &c.name.to_uppercase(), l.header, th, Align::Center);
        // Where the inserts are: what it is.
        if let Some(slots) = &l.inserts {
            let lines = [
                "CUE MIX".to_string(),
                if c.post_fader() {
                    "POST-FADER".into()
                } else {
                    "PRE-FADER".into()
                },
                c.output_label().to_uppercase(),
            ];
            for (r, text) in slots.iter().zip(lines) {
                controls::well_label(p, *r, &text, false, th);
            }
        }
        // The click: its level on the pan knob, its switch below.
        controls::knob(
            p,
            l.pan_knob,
            self.law.db_to_position(c.click_db),
            false,
            KnobLook {
                cap: col,
                ring: k.panel_label,
            },
            th,
        );
        controls::readout(
            p,
            l.pan_readout,
            &format!("CLK {}", format_db(c.click_db)),
            th,
        );
        controls::led_button(p, l.mute, "M", c.mute, k.led.mute, th);
        controls::led_button(p, l.solo, "S", false, k.led.solo, th);
        controls::led_button_fit(p, l.record, &["CLICK", "CL"], c.click, k.led.monitor, th);
        controls::readout(p, l.level_readout, &format_db(c.level_db), th);
        let geo = FaderGeometry::new(l.fader, th);
        controls::fader(
            p,
            &geo,
            self.law.db_to_position(c.level_db),
            col,
            &self.fader_scale(),
            th,
        );
        let m = model.cue_meter(cue);
        let channels = vec![m.left, m.right];
        crate::paint_meter(
            p,
            l.meter,
            &channels,
            model.meter_mode(c.channel()),
            model.vu_reference(),
            th,
        );
        controls::well_label(p, l.output, &format!("→ {}", c.output_label()), false, th);
        controls::scribble(p, l.scribble, &c.name, col, th);
    }

    /// The bar's height in this view now.
    pub(crate) fn cue_bar_h(&self, model: &Session) -> f32 {
        if self.master_only || model.cue_mixes().is_empty() {
            0.0
        } else {
            CUE_BAR_H
        }
    }

    /// The bar's buttons, left to right: Main Mix, the cue mixes, "+";
    /// on the right (a cue mix shown) Copy Main Mix and Pre/Post.
    pub(crate) fn bar_items(&self, width: f32, model: &Session) -> Vec<(Rect, BarHit, String)> {
        let h = CUE_BAR_H - 6.0;
        let mut out = Vec::new();
        let mut x = 6.0;
        let mut push = |out: &mut Vec<(Rect, BarHit, String)>, label: String, hit: BarHit| {
            let w = (label.chars().count() as f32 * 6.4 + 22.0).clamp(28.0, 190.0);
            out.push((Rect::new(x, 3.0, w, h), hit, label));
            x += w + 4.0;
        };
        push(&mut out, "MAIN MIX".into(), BarHit::Main);
        for c in model.cue_mixes() {
            push(&mut out, c.name.to_uppercase(), BarHit::Cue(c.id));
        }
        if model.cue_mixes().len() < faderframe_project::cue::MAX_CUE_MIXES {
            push(&mut out, "+".into(), BarHit::New);
        }
        if let Some(c) = model.mixer_cue() {
            let tap = if c.post_fader() { "POST" } else { "PRE" };
            let right = [
                ("COPY MAIN MIX", BarHit::CopyMain, 108.0),
                (tap, BarHit::Tap, 44.0),
            ];
            let mut rx = width - 6.0;
            for (label, hit, w) in right.into_iter().rev() {
                rx -= w;
                if rx > x + 40.0 {
                    out.push((Rect::new(rx, 3.0, w, h), hit, label.into()));
                }
                rx -= 4.0;
            }
        }
        out
    }

    pub(crate) fn bar_hit(&self, pos: Point, width: f32, model: &Session) -> Option<BarHit> {
        self.bar_items(width, model)
            .into_iter()
            .find(|(r, _, _)| r.contains(pos))
            .map(|(_, h, _)| h)
    }

    /// The bar: which mix the faders set, and (a cue mix shown) what it
    /// is, in its colour.
    pub(crate) fn paint_cue_bar(&self, p: &mut dyn Painter, width: f32, model: &Session) {
        let th: &Theme = &self.theme;
        let band = Rect::new(0.0, 0.0, width, CUE_BAR_H);
        let shown = model.mixer_cue();
        p.fill(band, th.ui.surface);
        if let Some(c) = shown {
            // The whole bar in the cue mix's colour: cue mode at a glance.
            p.fill(band, cue_color(c).with_alpha(0.28));
        }
        p.hline(0.0, width, CUE_BAR_H - 0.5, th.ui.border);
        let style = TextStyle::new(th.fonts.small, th.ui.text).bold().center();
        let items = self.bar_items(width, model);
        let mut end = 0.0f32;
        for (r, hit, label) in &items {
            let (fill, edge) = match hit {
                BarHit::Main if shown.is_none() => (th.ui.accent.with_alpha(0.45), th.ui.accent),
                BarHit::Cue(id) => {
                    let col = model.project().cue(*id).map_or(th.ui.accent, cue_color);
                    if shown.is_some_and(|c| c.id == *id) {
                        (col.with_alpha(0.75), col)
                    } else {
                        (col.with_alpha(0.18), col.with_alpha(0.7))
                    }
                }
                _ => (th.ui.background.with_alpha(0.6), th.ui.border),
            };
            let hot = self.bar_hover == Some(*hit);
            p.fill_rounded(
                *r,
                4.0,
                &faderframe_ui_canvas::Paint::Solid(if hot { fill.lighten(0.15) } else { fill }),
            );
            p.stroke_rounded(*r, 4.0, 1.0, edge);
            p.text(label, *r, &style);
            if !matches!(hit, BarHit::CopyMain | BarHit::Tap) {
                end = end.max(r.right());
            }
        }
        // What the faders do now.
        let first_right = items
            .iter()
            .filter(|(_, h, _)| matches!(h, BarHit::CopyMain | BarHit::Tap))
            .map(|(r, _, _)| r.x)
            .fold(width, f32::min);
        let caption = match shown {
            Some(c) => format!(
                "Faders, pans and mutes set {}'s sends ({}) · {}",
                c.name,
                tap_name(c),
                c.output_label()
            ),
            None => {
                "Faders set the main mix · choose a cue mix to set a performer's headphones".into()
            }
        };
        let area = Rect::new(
            end + 10.0,
            0.0,
            (first_right - end - 20.0).max(0.0),
            CUE_BAR_H,
        );
        if area.w > 60.0 {
            p.push_clip(area);
            let dim = TextStyle::new(th.fonts.small, th.ui.text_dim);
            p.text(&caption, area, &dim);
            p.pop_clip();
        }
    }

    /// A press in the bar.
    pub(crate) fn bar_press(
        &mut self,
        pos: Point,
        button: PointerButton,
        width: f32,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        let Some(hit) = self.bar_hit(pos, width, model) else {
            return true;
        };
        if button == PointerButton::Secondary {
            if let BarHit::Cue(id) = hit {
                cx.request(HostRequest::ContextMenu {
                    at: pos,
                    items: Self::cue_menu(model, id),
                });
            }
            return true;
        }
        let shown = model.mixer_cue().map(|c| c.id);
        match hit {
            BarHit::Main => cx.emit(Action::Cue(CueOp::Show(None))),
            BarHit::Cue(id) => cx.emit(Action::Cue(CueOp::Show(Some(id)))),
            BarHit::New => cx.emit(Action::Cue(CueOp::New)),
            BarHit::CopyMain => {
                if let Some(id) = shown {
                    cx.emit(Action::Cue(CueOp::CopyMainMix(id)));
                }
            }
            BarHit::Tap => {
                if let Some(c) = model.mixer_cue() {
                    let tap = if c.post_fader() {
                        SendTap::PreFader
                    } else {
                        SendTap::PostFader
                    };
                    cx.emit(Action::Cue(CueOp::SetTap { cue: c.id, tap }));
                }
            }
        }
        true
    }

    pub(crate) fn bar_tooltip(&self, pos: Point, width: f32, model: &Session) -> Option<String> {
        Some(match self.bar_hit(pos, width, model)? {
            BarHit::Main => "The faders set the main mix".into(),
            BarHit::Cue(id) => {
                let c = model.project().cue(id)?;
                format!(
                    "{}: the faders set its sends ({}) · {} · Right-click: rename, outputs, click, remove",
                    c.name,
                    tap_name(c),
                    c.output_label()
                )
            }
            BarHit::New => "New cue mix: a copy of the main mix on the next free outputs".into(),
            BarHit::CopyMain => {
                "Copy Main Mix: every send as the main mix has it (levels, pans, mutes)".into()
            }
            BarHit::Tap => {
                "Pre-fader (independent of the main faders) or post-fader (follows them); mutes and solos never reach a cue mix".into()
            }
        })
    }

    /// A cue mix's menu (the bar's tab, the cue master's strip).
    pub(crate) fn cue_menu(model: &Session, id: CueId) -> Vec<MenuItem<Action>> {
        let Some(c) = model.project().cue(id) else {
            return Vec::new();
        };
        let outputs: Vec<MenuItem<Action>> = model
            .cue_output_choices(id)
            .into_iter()
            .map(|o| {
                let item = MenuItem::new(o.label, o.action).checked(o.checked);
                if o.group_start {
                    item.separated()
                } else {
                    item
                }
            })
            .collect();
        let mut items = vec![
            MenuItem::new(
                format!("Show {} in the Mixer", c.name),
                Action::Cue(CueOp::Show(Some(id))),
            )
            .checked(model.mixer_cue().is_some_and(|m| m.id == id)),
            MenuItem::new("Show the Main Mix", Action::Cue(CueOp::Show(None))),
            MenuItem::submenu(format!("Outputs ({})", c.output_label()), outputs).separated(),
            MenuItem::new(
                "Pre-Fader",
                Action::Cue(CueOp::SetTap {
                    cue: id,
                    tap: SendTap::PreFader,
                }),
            )
            .checked(!c.post_fader()),
            MenuItem::new(
                "Post-Fader",
                Action::Cue(CueOp::SetTap {
                    cue: id,
                    tap: SendTap::PostFader,
                }),
            )
            .checked(c.post_fader()),
            MenuItem::new(
                "Click",
                Action::Edit(Command::SetCueClick {
                    cue: id,
                    on: !c.click,
                    db: c.click_db,
                }),
            )
            .checked(c.click)
            .separated(),
            MenuItem::new(
                "Mute",
                Action::Edit(Command::SetCueMute {
                    cue: id,
                    on: !c.mute,
                }),
            )
            .checked(c.mute),
            MenuItem::new("Copy Main Mix", Action::Cue(CueOp::CopyMainMix(id))).separated(),
        ];
        items.push(MenuItem::new("Remove Cue Mix", Action::Cue(CueOp::Remove(id))).separated());
        items
    }

    /// A strip's controls right-clicked in a cue mix: its send there.
    pub(crate) fn cue_send_menu(model: &Session, t: &Track, cue: CueId) -> Vec<MenuItem<Action>> {
        let Some(c) = model.project().cue(cue) else {
            return Vec::new();
        };
        let send = model.cue_send(t, cue);
        let main = model.project().main_mix_send(t, c);
        let set = |s: CueSend| {
            Action::Edit(Command::SetCueSend {
                track: t.id,
                cue,
                send: Some(s),
            })
        };
        vec![
            MenuItem::new(
                format!("In {}", c.name),
                Action::Edit(Command::SetCueSendOn {
                    track: t.id,
                    cue,
                    on: !send.on,
                }),
            )
            .checked(send.on),
            MenuItem::new(
                format!("Level 0 dB (now {})", format_db(send.level_db)),
                set(CueSend {
                    level_db: 0.0,
                    ..send
                }),
            ),
            MenuItem::new("Pan Centre", set(CueSend { pan: 0.0, ..send })),
            MenuItem::new("As in the Main Mix", set(main)).separated(),
        ]
    }
}
