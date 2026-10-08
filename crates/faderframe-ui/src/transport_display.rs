//! The LCD-style position/tempo display in the header bar: position (a
//! big and a small counter in any unit: right-click to choose, a click on
//! the small one steps it), tempo (double-click to type, the TAP pad to
//! tap it in), time signature (click to type, right-click for common
//! meters and meter changes), loop and record flags.

use faderframe_project::Command;
use faderframe_session::{Action, CounterUnit, Session, TransportAction, format_position};
use faderframe_timeline::TimeSignature;
use faderframe_ui_canvas::{
    Align, CanvasView, Color, EventCx, FontFamily, HostRequest, MenuItem, Paint, Painter, Point,
    PointerButton, Rect, Size, TextStyle, Theme, ViewEvent,
};
use std::time::{Duration, Instant};

/// Taps further apart than this start a new measurement.
const TAP_RESET: Duration = Duration::from_millis(2500);
/// Taps averaged.
const TAP_WINDOW: usize = 8;
const TAP_FLASH: Duration = Duration::from_millis(120);

/// Tempo from tap times (at least two, oldest first): the mean interval of
/// the last few taps.
pub fn tap_tempo(taps: &[Instant]) -> Option<f64> {
    let taps = &taps[taps.len().saturating_sub(TAP_WINDOW)..];
    if taps.len() < 2 {
        return None;
    }
    let span = taps[taps.len() - 1].duration_since(taps[0]).as_secs_f64();
    let interval = span / (taps.len() - 1) as f64;
    (interval > 0.0).then(|| ((60.0 / interval).clamp(20.0, 400.0) * 100.0).round() / 100.0)
}

#[derive(Default)]
pub struct TransportDisplay {
    taps: Vec<Instant>,
}

impl TransportDisplay {
    pub fn new() -> Self {
        Self::default()
    }

    fn tap(&mut self, now: Instant) -> Option<f64> {
        if self
            .taps
            .last()
            .is_some_and(|t| now.duration_since(*t) > TAP_RESET)
        {
            self.taps.clear();
        }
        self.taps.push(now);
        if self.taps.len() > TAP_WINDOW {
            self.taps.remove(0);
        }
        tap_tempo(&self.taps)
    }

    fn flashing(&self) -> bool {
        self.taps.last().is_some_and(|t| t.elapsed() < TAP_FLASH)
    }
}

struct Zones {
    bbt: Rect,
    time: Rect,
    tempo: Rect,
    bpm: Rect,
    tap: Rect,
    meter: Rect,
    meter_label: Rect,
    flags: Rect,
}

fn zones(size: Size) -> Zones {
    let r = Rect::from_size(size).inset_xy(10.0, 3.0);
    let (left, rest) = r.split_left(124.0);
    let (bbt, time) = left.split_top(left.h * 0.62);
    let (tempo_col, rest) = rest.split_left(70.0);
    let (tempo, bpm) = tempo_col.split_top(tempo_col.h * 0.62);
    let (tap, rest) = rest.split_left(40.0);
    let tap = tap.inset_xy(5.0, 2.0);
    let (meter_col, flags) = rest.split_left((rest.w - 42.0).max(44.0));
    let (meter, meter_label) = meter_col.split_top(meter_col.h * 0.62);
    Zones {
        bbt,
        time,
        tempo,
        bpm,
        tap,
        meter,
        meter_label,
        flags,
    }
}

/// The meter change in effect at the playhead (its bar).
fn current_change_bar(s: &Session) -> i32 {
    let meter = &s.project().timeline.meter;
    let bar = meter.bar_at(s.playhead());
    meter
        .changes()
        .iter()
        .rev()
        .find(|c| c.bar <= bar)
        .map_or(0, |c| c.bar)
}

fn set_meter(bar: i32, signature: TimeSignature) -> Action {
    Action::Edit(Command::SetTimeSignature {
        bar,
        signature: Some(signature),
    })
}

const COMMON_METERS: [(u8, u8); 8] = [
    (2, 4),
    (3, 4),
    (4, 4),
    (5, 4),
    (6, 8),
    (7, 8),
    (9, 8),
    (12, 8),
];

fn heading(label: String) -> MenuItem<Action> {
    MenuItem::disabled(label).separated()
}

/// The small counter's unit (automatic: timecode for picture work).
fn sub_unit(s: &Session) -> CounterUnit {
    s.editor.sub_counter.unwrap_or({
        let p = s.project();
        if p.timecode.is_some() || !p.video.tracks.is_empty() {
            CounterUnit::Timecode
        } else {
            CounterUnit::MinSecs
        }
    })
}

/// The playhead in `unit`.
fn readout(s: &Session, unit: CounterUnit) -> String {
    match unit {
        CounterUnit::BarsBeats => s.project().timeline.format_bbt(s.playhead()),
        _ => format_position(s.project(), s.transport().position, unit),
    }
}

fn counter_menu(s: &Session, at: Point) -> HostRequest<Action> {
    let e = &s.editor;
    let mut items = vec![heading("Main Counter".into())];
    for u in CounterUnit::ALL {
        items.push(
            MenuItem::new(
                u.label(),
                Action::SetTransportCounter {
                    sub: false,
                    unit: Some(u),
                },
            )
            .checked(e.main_counter == u),
        );
    }
    items.push(heading("Sub Counter".into()));
    items.push(
        MenuItem::new(
            "Automatic (Timecode with Picture)",
            Action::SetTransportCounter {
                sub: true,
                unit: None,
            },
        )
        .checked(e.sub_counter.is_none()),
    );
    for u in CounterUnit::ALL {
        items.push(
            MenuItem::new(
                u.label(),
                Action::SetTransportCounter {
                    sub: true,
                    unit: Some(u),
                },
            )
            .checked(e.sub_counter == Some(u)),
        );
    }
    HostRequest::ContextMenu { at, items }
}

fn meter_menu(s: &Session, at: Point) -> HostRequest<Action> {
    let meter = &s.project().timeline.meter;
    let change_bar = current_change_bar(s);
    let here = meter.bar_at(s.playhead()).max(0);
    let current = meter.signature_of_bar(change_bar);
    let mut items = vec![heading(format!("Meter from Bar {}", change_bar + 1))];
    let section = |items: &mut Vec<MenuItem<Action>>, bar: i32, checked: Option<TimeSignature>| {
        for sig in COMMON_METERS
            .into_iter()
            .filter_map(|(n, d)| TimeSignature::new(n, d))
        {
            items.push(
                MenuItem::new(sig.to_string(), set_meter(bar, sig)).checked(Some(sig) == checked),
            );
        }
    };
    section(&mut items, change_bar, Some(current));
    if here > change_bar {
        items.push(heading(format!("New Meter at Bar {}", here + 1)));
        section(&mut items, here, None);
    }
    if change_bar > 0 {
        items.push(
            MenuItem::new(
                format!("Remove Meter Change at Bar {}", change_bar + 1),
                Action::Edit(Command::SetTimeSignature {
                    bar: change_bar,
                    signature: None,
                }),
            )
            .separated(),
        );
    }
    HostRequest::ContextMenu { at, items }
}

impl CanvasView<Session, Action> for TransportDisplay {
    /// The position (both counters), the tempo (the arrows change it by a
    /// BPM) and the meter.
    fn accessible(&self, size: Size, s: &Session) -> Vec<faderframe_ui_canvas::AccessNode<Action>> {
        use faderframe_ui_canvas::{AccessNode, AccessRole, access_id};
        let z = zones(size);
        let pos = s.playhead();
        let project = s.project();
        let bpm = project.timeline.tempo.bpm_at(pos);
        let sig = project.timeline.meter.signature_at(pos);
        let set = |bpm: f64| {
            Action::Edit(Command::SetTempo {
                bpm: bpm.clamp(20.0, 999.0),
            })
        };
        vec![
            AccessNode::new(
                access_id(&[61]),
                AccessRole::Label,
                format!(
                    "Position {}, {}",
                    readout(s, s.editor.main_counter),
                    readout(s, sub_unit(s))
                ),
            )
            .at(z.bbt.union(&z.time)),
            AccessNode::new(access_id(&[62]), AccessRole::SpinButton, "Tempo")
                .at(z.tempo.union(&z.bpm))
                .value(bpm, 20.0, 999.0, format!("{bpm:.2} BPM"))
                .on_step(set(bpm.floor() + 1.0), set(bpm.ceil() - 1.0)),
            AccessNode::new(
                access_id(&[63]),
                AccessRole::Label,
                format!("Meter {}/{}", sig.numerator, sig.denominator),
            )
            .at(z.meter.union(&z.meter_label)),
        ]
    }

    fn accessible_name(&self) -> Option<String> {
        Some("Transport".into())
    }

    fn paint(&mut self, p: &mut dyn Painter, size: Size, s: &Session, theme: &Theme) {
        let (lcd_bg, lcd_text, lcd_dim) = (theme.ui.lcd_bg, theme.ui.lcd_text, theme.ui.lcd_dim);
        let r = Rect::from_size(size);
        p.fill_rounded(r, 6.0, &Paint::vertical(r, lcd_bg.lighten(0.04), lcd_bg));
        p.inset_shadow(r, 6.0, Color::rgba(0.0, 0.0, 0.0, 0.9), 0.0, 1.5, 4.0);
        p.stroke_rounded(r, 6.0, 1.0, Color::rgba(1.0, 1.0, 1.0, 0.06));
        let z = zones(size);
        let project = s.project();
        let pos = s.playhead();
        let mono = |size: f32, c: Color| TextStyle::new(size, c).family(FontFamily::Mono);
        let small = |c: Color| TextStyle::new(theme.fonts.tiny + 0.5, c).bold();
        // Long readouts (timecode, samples) shrink to fit.
        let fit = |text: &str, size: f32, room: usize| {
            let n = text.chars().count().max(1);
            if n > room {
                size * room as f32 / n as f32
            } else {
                size
            }
        };
        let main = readout(s, s.editor.main_counter);
        p.text(
            &main,
            z.bbt,
            &mono(fit(&main, theme.fonts.display - 2.0, 9), lcd_text).bold(),
        );
        let sub = readout(s, sub_unit(s));
        p.text(
            &sub,
            z.time,
            &mono(fit(&sub, theme.fonts.small, 14), lcd_text.with_alpha(0.75)),
        );
        let bpm = project.timeline.tempo.bpm_at(pos);
        p.text(
            &format!("{bpm:.2}"),
            z.tempo,
            &mono(theme.fonts.large, lcd_text).align(Align::End),
        );
        // Varispeed: the speed in place of "BPM".
        let speed = s.engine().speed();
        if let Some(v) = s.shuttle_speed() {
            let arrows = if v < 0.0 { "◀◀" } else { "▶▶" };
            p.text(
                &format!("{arrows} {}×", v.abs()),
                z.bpm,
                &small(Color::hex(0x6fd0ff)).align(Align::End),
            );
        } else if (speed - 1.0).abs() > 1e-6 {
            p.text(
                &format!("VARI {:+.1}%", (speed - 1.0) * 100.0),
                z.bpm,
                &small(Color::hex(0xffc24b)).align(Align::End),
            );
        } else {
            p.text(
                "BPM",
                z.bpm,
                &small(lcd_text.with_alpha(0.7)).align(Align::End),
            );
        }
        // The tap pad.
        let lit = self.flashing();
        p.fill_rounded(
            z.tap,
            3.0,
            &Paint::Solid(if lit {
                lcd_text.with_alpha(0.85)
            } else {
                lcd_text.with_alpha(0.08)
            }),
        );
        p.stroke_rounded(z.tap, 3.0, 1.0, lcd_text.with_alpha(0.35));
        p.text(
            "TAP",
            z.tap,
            &small(if lit {
                lcd_bg
            } else {
                lcd_text.with_alpha(0.8)
            })
            .center(),
        );
        let sig = project.timeline.meter.signature_at(pos);
        p.text(
            &sig.to_string(),
            z.meter,
            &mono(theme.fonts.large, lcd_text).align(Align::Center),
        );
        p.text(
            "METER",
            z.meter_label,
            &small(lcd_text.with_alpha(0.7)).center(),
        );
        let t = s.transport();
        let flag = |p: &mut dyn Painter, rect: Rect, label: &str, on: bool, color: Color| {
            p.text(
                label,
                rect,
                &small(if on { color } else { lcd_dim }).center(),
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
        let ViewEvent::PointerDown {
            pos,
            button,
            clicks,
            ..
        } = *ev
        else {
            return false;
        };
        let z = zones(size);
        if z.tap.inset_xy(-4.0, -2.0).contains(pos) && button == PointerButton::Primary {
            // Every press is a tap, however fast (multi-click counts too).
            if let Some(bpm) = self.tap(Instant::now()) {
                cx.emit(Action::Edit(Command::SetTempo { bpm }));
            }
            cx.redraw();
            return true;
        }
        let counters = z.bbt.union(&z.time);
        if counters.contains(pos) {
            match button {
                PointerButton::Secondary => cx.request(counter_menu(s, pos)),
                // The small counter steps through the units.
                PointerButton::Primary if z.time.contains(pos) => {
                    let now = sub_unit(s);
                    let i = CounterUnit::ALL.iter().position(|u| *u == now).unwrap_or(0);
                    cx.emit(Action::SetTransportCounter {
                        sub: true,
                        unit: Some(CounterUnit::ALL[(i + 1) % CounterUnit::ALL.len()]),
                    });
                }
                _ => return false,
            }
            return true;
        }
        let meter_zone = z.meter.union(&z.meter_label);
        if meter_zone.contains(pos) {
            match button {
                PointerButton::Primary => {
                    let bar = current_change_bar(s);
                    let sig = s.project().timeline.meter.signature_of_bar(bar);
                    cx.request(HostRequest::TextInput {
                        at: meter_zone,
                        initial: sig.to_string(),
                        commit: Box::new(move |t| {
                            TimeSignature::parse(t).map(|sig| set_meter(bar, sig))
                        }),
                    });
                }
                PointerButton::Secondary => cx.request(meter_menu(s, pos)),
                _ => {}
            }
            return true;
        }
        if button != PointerButton::Primary {
            return false;
        }
        if z.tempo.union(&z.bpm).contains(pos) && clicks >= 2 {
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
                TransportAction::ToggleLoop
            } else {
                TransportAction::ToggleRecord
            }));
            return true;
        }
        false
    }

    fn wants_frames(&self, s: &Session) -> bool {
        s.transport().playing || self.flashing()
    }

    fn tooltip(&self, pos: Point, size: Size, _s: &Session) -> Option<String> {
        let z = zones(size);
        Some(if z.tap.inset_xy(-4.0, -2.0).contains(pos) {
            "Tap tempo: click in time with the music".into()
        } else if z.tempo.union(&z.bpm).contains(pos) {
            "Double-click to type a tempo".into()
        } else if z.meter.union(&z.meter_label).contains(pos) {
            "Time signature: click to type (e.g. 7/8) · right-click for common meters".into()
        } else if z.flags.contains(pos) {
            "Toggle loop / record mode".into()
        } else {
            "Position: right-click to choose the counters · click the small one to step it".into()
        })
    }

    fn min_size(&self) -> Size {
        Size::new(370.0, 38.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn taps_average_their_intervals() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        assert_eq!(tap_tempo(&[at(0)]), None);
        assert_eq!(tap_tempo(&[at(0), at(500)]), Some(120.0));
        // Jitter averages out.
        assert_eq!(
            tap_tempo(&[at(0), at(490), at(1010), at(1500)]),
            Some(120.0)
        );
        let mut d = TransportDisplay::new();
        d.tap(at(0));
        assert_eq!(d.tap(at(600)), Some(100.0));
        // A long pause starts over.
        assert_eq!(d.tap(at(5000)), None);
        assert_eq!(d.tap(at(5400)), Some(150.0));
    }
}
