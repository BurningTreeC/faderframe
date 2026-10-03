//! The LCD-style position/tempo display in the header bar.

use faderframe_project::Command;
use faderframe_session::{Action, Session};
use faderframe_timeline::format_seconds;
use faderframe_ui_canvas::{
    Align, CanvasView, Color, EventCx, FontFamily, HostRequest, Paint, Painter, Point,
    PointerButton, Rect, Size, TextStyle, Theme, ViewEvent,
};

pub struct TransportDisplay;

const LCD_BG: Color = Color::hex(0x0d100e);
const LCD_TEXT: Color = Color::hex(0xf0c46a);
const LCD_DIM: Color = Color::hex(0x6f5a33);

struct Zones {
    bbt: Rect,
    time: Rect,
    tempo: Rect,
    meter: Rect,
    flags: Rect,
}

fn zones(size: Size) -> Zones {
    let r = Rect::from_size(size).inset_xy(10.0, 3.0);
    let (left, rest) = r.split_left(124.0);
    let (bbt, time) = left.split_top(left.h * 0.62);
    let (tempo_col, flags) = rest.split_left((rest.w - 70.0).max(60.0));
    let (tempo, meter) = tempo_col.split_top(tempo_col.h * 0.62);
    Zones {
        bbt,
        time,
        tempo,
        meter,
        flags,
    }
}

impl CanvasView<Session, Action> for TransportDisplay {
    fn paint(&mut self, p: &mut dyn Painter, size: Size, s: &Session, theme: &Theme) {
        let r = Rect::from_size(size);
        p.fill_rounded(r, 6.0, &Paint::vertical(r, LCD_BG.lighten(0.04), LCD_BG));
        p.inset_shadow(r, 6.0, Color::rgba(0.0, 0.0, 0.0, 0.9), 0.0, 1.5, 4.0);
        p.stroke_rounded(r, 6.0, 1.0, Color::rgba(1.0, 1.0, 1.0, 0.06));
        let z = zones(size);
        let project = s.project();
        let pos = s.playhead();
        let mono = |size: f32, c: Color| TextStyle::new(size, c).family(FontFamily::Mono);
        p.text(
            &project.timeline.format_bbt(pos),
            z.bbt,
            &mono(theme.fonts.display - 2.0, LCD_TEXT).bold(),
        );
        let secs = s.transport().position as f64 / s.sample_rate().max(1) as f64;
        p.text(
            &format_seconds(secs),
            z.time,
            &mono(theme.fonts.small, LCD_TEXT.with_alpha(0.75)),
        );
        let bpm = project.timeline.tempo.bpm_at(pos);
        p.text(
            &format!("{bpm:.2}"),
            z.tempo,
            &mono(theme.fonts.large, LCD_TEXT).align(Align::End),
        );
        let sig = project.timeline.meter.signature_at(pos);
        p.text(
            &format!("BPM · {sig}"),
            z.meter,
            &mono(theme.fonts.tiny + 0.5, LCD_TEXT.with_alpha(0.7)).align(Align::End),
        );
        let t = s.transport();
        let flag = |p: &mut dyn Painter, rect: Rect, label: &str, on: bool, color: Color| {
            p.text(
                label,
                rect,
                &TextStyle::new(theme.fonts.tiny + 0.5, if on { color } else { LCD_DIM })
                    .bold()
                    .center(),
            );
        };
        let (f1, f2) = z.flags.split_top(z.flags.h / 2.0);
        flag(p, f1, "LOOP", project.loop_enabled, Color::hex(0xff8a5c));
        flag(p, f2, "REC", t.recording, Color::hex(0xff4b4b));
    }

    fn event(
        &mut self,
        ev: &ViewEvent,
        size: Size,
        s: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        if let ViewEvent::PointerDown {
            pos,
            button: PointerButton::Primary,
            clicks,
            ..
        } = *ev
        {
            let z = zones(size);
            if z.tempo.contains(pos) && clicks >= 2 {
                let bpm = s.project().timeline.tempo.points()[0].bpm;
                cx.request(HostRequest::TextInput {
                    at: z.tempo,
                    initial: format!("{bpm:.2}"),
                    commit: Box::new(|t| {
                        t.trim()
                            .trim_end_matches("BPM")
                            .trim()
                            .parse::<f64>()
                            .ok()
                            .filter(|v| v.is_finite())
                            .map(|bpm| Action::Edit(Command::SetTempo { bpm }))
                    }),
                });
                return true;
            }
            if z.flags.contains(pos) {
                cx.emit(Action::Transport(if pos.y < z.flags.center().y {
                    faderframe_session::TransportAction::ToggleLoop
                } else {
                    faderframe_session::TransportAction::ToggleRecord
                }));
                return true;
            }
        }
        false
    }

    fn wants_frames(&self, s: &Session) -> bool {
        s.transport().playing
    }

    fn tooltip(&self, pos: Point, size: Size, _s: &Session) -> Option<String> {
        let z = zones(size);
        if z.tempo.contains(pos) {
            Some("Double-click to type a tempo".into())
        } else if z.flags.contains(pos) {
            Some("Toggle loop / record mode".into())
        } else {
            Some("Bars.Beats.Ticks and time".into())
        }
    }

    fn min_size(&self) -> Size {
        Size::new(290.0, 38.0)
    }
}
