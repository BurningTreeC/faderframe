//! The Tools view for screen readers and the keyboard: the toolbar (the
//! source, target and scale menus — Enter opens them —, Reset on Play and
//! Announce Limits as toggles, Reset), then each pane's readings as meters
//! the keyboard visits (Tab hears each one): loudness and dynamics, the
//! levels, the correlation, the spectrum summed up (where it is strongest),
//! and the codec monitor's pane with its buttons while it is on.
//!
//! With Announce Limits on, a reading crossing its limit is said when it
//! happens ([`Limits`]): the true peak over −1 dBTP, the integrated
//! loudness reaching the target (within 1 LU) or leaving it, the
//! correlation going below zero (and back), the codec's decoded peaks
//! going over full scale.

use crate::{
    CodecLayout, LEVEL_FLOOR, Layout, SPECTRUM_HI, SPECTRUM_LO, ToolsView, format_hz, format_lufs,
};
use faderframe_session::analysis::LevelScale;
use faderframe_session::codec::{CodecOp, CodecStatus};
use faderframe_session::{Action, Session};
use faderframe_ui_canvas::{
    AccessNode, AccessPress, AccessRole, CanvasView, EventCx, HostRequest, Point, Rect, Size,
    access_id,
};

/// Kinds of node.
const TOOLBAR: u64 = 0x7001;
const SOURCE: u64 = 0x7002;
const TARGET: u64 = 0x7003;
const SCALE: u64 = 0x7004;
const RESET_ON_PLAY: u64 = 0x7005;
const ANNOUNCE: u64 = 0x7006;
const RESET: u64 = 0x7007;
const LOUDNESS: u64 = 0x7010;
const LEVEL: u64 = 0x7011;
const PHASE: u64 = 0x7012;
const SPECTRUM: u64 = 0x7013;
const CODEC: u64 = 0x7014;
const READING: u64 = 0x7020;
const CODEC_HEAR: u64 = 0x7030;
const CODEC_CHOOSE: u64 = 0x7031;
const CODEC_RESET: u64 = 0x7032;

/// The true peak a delivery should stay under (dBTP).
pub(crate) const PEAK_LIMIT: f64 = -1.0;
/// The integrated loudness counts as on target within this (LU).
pub(crate) const ON_TARGET: f64 = 1.0;
/// The correlation is out of phase below this, back in phase above
/// `IN_PHASE` (apart: no chatter around zero).
pub(crate) const OUT_OF_PHASE: f32 = -0.1;
pub(crate) const IN_PHASE: f32 = 0.1;

/// Which side of its limit each reading is on.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Limits {
    /// The true peak is over [`PEAK_LIMIT`].
    pub peak_over: bool,
    /// The integrated loudness: −1 under the target, 0 on it, 1 over
    /// (`None`: nothing measured yet).
    pub loudness: Option<i8>,
    pub out_of_phase: bool,
    /// The codec's decoded peaks went over full scale.
    pub codec_over: bool,
}

/// The readings the limits look at.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Readings {
    pub true_peak: f64,
    pub integrated: f64,
    pub target: f64,
    pub correlation: f32,
    /// The codec monitor's frames over full scale (`None`: it is off).
    pub codec_overs: Option<u64>,
}

impl Limits {
    /// The sides `r` is on, from where `self` was (the correlation keeps
    /// its side between the two thresholds).
    pub fn of(&self, r: &Readings) -> Limits {
        let loudness = r.integrated.is_finite().then(|| {
            let delta = r.integrated - r.target;
            if delta.abs() <= ON_TARGET {
                0
            } else if delta > 0.0 {
                1
            } else {
                -1
            }
        });
        Limits {
            peak_over: r.true_peak.is_finite() && r.true_peak > PEAK_LIMIT,
            loudness,
            out_of_phase: if self.out_of_phase {
                r.correlation < IN_PHASE
            } else {
                r.correlation < OUT_OF_PHASE
            },
            codec_over: r.codec_overs.is_some_and(|n| n > 0),
        }
    }

    /// What crossing from `self` to `now` says.
    pub fn crossed(&self, now: &Limits, r: &Readings) -> Vec<String> {
        let mut out = Vec::new();
        if now.peak_over && !self.peak_over {
            out.push(format!(
                "True peak over {} dBTP: {} dBTP",
                format_lufs(PEAK_LIMIT),
                format_lufs(r.true_peak)
            ));
        }
        if now.loudness != self.loudness
            && let Some(side) = now.loudness
        {
            let delta = r.integrated - r.target;
            out.push(match side {
                0 => format!(
                    "Integrated loudness on target: {} LUFS",
                    format_lufs(r.integrated)
                ),
                1 => format!("Integrated loudness {:.1} LU over the target", delta),
                _ => format!("Integrated loudness {:.1} LU under the target", -delta),
            });
        }
        if now.out_of_phase != self.out_of_phase {
            out.push(if now.out_of_phase {
                format!(
                    "Correlation below zero: {}, out of phase",
                    correlation_text(r.correlation)
                )
            } else {
                "Correlation positive again".into()
            });
        }
        if now.codec_over && !self.codec_over {
            out.push("Codec: decoded peaks over full scale".into());
        }
        out
    }
}

fn correlation_text(c: f32) -> String {
    format!("{c:+.2}").replace('-', "−")
}

/// A level in dB as read ("−14.2 LUFS", "no reading").
fn reading(v: f64, unit: &str) -> String {
    if v.is_finite() && v > -200.0 {
        format!("{} {unit}", format_lufs(v))
    } else {
        "no reading".into()
    }
}

/// A meter node the keyboard visits.
fn meter(
    id: u64,
    label: &str,
    r: Rect,
    v: f64,
    lo: f64,
    hi: f64,
    text: String,
) -> AccessNode<Action> {
    let now = if v.is_finite() { v.clamp(lo, hi) } else { lo };
    AccessNode::new(id, AccessRole::Meter, label)
        .at(r)
        .value(now, lo, hi, text)
        .tab_stop()
        .fixed()
}

impl ToolsView {
    fn readings(model: &Session) -> Readings {
        let l = model.analyzer().loudness.read();
        let st = model.codec_status();
        Readings {
            true_peak: l.true_peak,
            integrated: l.integrated,
            target: f64::from(model.analysis_settings().target_lufs),
            correlation: model.analyzer().phase.correlation(),
            codec_overs: st.on.then_some(st.overs),
        }
    }

    /// Turn Announce Limits on or off (said either way).
    pub(crate) fn toggle_announce(&mut self, cx: &mut EventCx<'_, Action>) {
        self.announce = !self.announce;
        cx.request(HostRequest::Announce(
            if self.announce {
                "Announcing limits"
            } else {
                "Not announcing limits"
            }
            .into(),
        ));
        cx.redraw();
    }

    /// Readings crossing their limits since the last call (said with
    /// Announce Limits on; followed either way, so turning it on says
    /// only what crosses afterwards).
    pub(crate) fn news(&mut self, model: &Session) -> Vec<String> {
        let r = Self::readings(model);
        let before = self.limits.unwrap_or_default();
        let now = before.of(&r);
        let first = self.limits.replace(now).is_none();
        if first || !self.announce {
            return Vec::new();
        }
        before.crossed(&now, &r)
    }

    fn toolbar(&self, l: &Layout, size: Size, model: &Session) -> AccessNode<Action> {
        let settings = model.analysis_settings();
        let source = model
            .analysis_source()
            .and_then(|s| model.project().track(s))
            .map_or_else(|| "none".to_string(), |t| t.name.clone());
        let scale = match settings.scale {
            LevelScale::Digital => "dBFS".to_string(),
            LevelScale::K(n) => format!("K-{n}"),
        };
        let tip = |pos: Point| self.tooltip(pos, size, model);
        let button = |id: u64, label: String, r: Rect| {
            let n = AccessNode::new(access_id(&[id]), AccessRole::Button, label)
                .at(r)
                .fixed();
            match tip(r.center()) {
                Some(t) => n.described(t),
                None => n,
            }
        };
        let mut reset_on_play = button(RESET_ON_PLAY, "Reset on play".into(), l.reset_on_play)
            .checked(settings.reset_on_play)
            .on_activate(Action::SetResetOnPlay(!settings.reset_on_play));
        reset_on_play.role = AccessRole::ToggleButton;
        let mut announce = button(ANNOUNCE, "Announce limits".into(), l.announce)
            .checked(self.announce)
            .on_press();
        announce.role = AccessRole::ToggleButton;
        AccessNode::new(access_id(&[TOOLBAR]), AccessRole::Toolbar, "Tools toolbar")
            .at(l.toolbar)
            .fixed()
            .with_children([
                button(SOURCE, format!("Source: {source}"), l.source).with_menu(),
                button(
                    TARGET,
                    format!(
                        "Loudness target: {} LUFS",
                        format_lufs(f64::from(settings.target_lufs))
                    ),
                    l.target,
                )
                .with_menu(),
                button(SCALE, format!("Level scale: {scale}"), l.scale).with_menu(),
                reset_on_play,
                announce,
                button(RESET, "Reset the measurement".into(), l.reset)
                    .on_activate(Action::ResetAnalysis),
            ])
    }

    fn loudness_pane(&self, r: Rect, model: &Session) -> AccessNode<Action> {
        let l = model.analyzer().loudness.read();
        let d = model.analyzer().dynamics.read();
        let target = f64::from(model.analysis_settings().target_lufs);
        let body = r.split_top(20.0).1.inset_xy(8.0, 4.0);
        let (big, rest) = body.split_top(46.0);
        let row = |i: usize| Rect::new(rest.x, rest.y + i as f32 * 15.0, rest.w, 15.0);
        let id = |k: u64| access_id(&[READING, LOUDNESS, k]);
        let mut integrated = reading(l.integrated, "LUFS");
        if l.integrated.is_finite() {
            let delta = l.integrated - target;
            integrated.push_str(&if delta.abs() < 0.05 {
                ", on the target".to_string()
            } else if delta > 0.0 {
                format!(", {delta:.1} LU over the target")
            } else {
                format!(", {:.1} LU under the target", -delta)
            });
        }
        let lu = |v: f64| {
            if v.is_finite() {
                format!("{v:.1} LU")
            } else {
                "no reading".into()
            }
        };
        let measured = l.integrated.is_finite();
        let plr = l.true_peak - l.integrated;
        AccessNode::new(
            access_id(&[LOUDNESS]),
            AccessRole::Group,
            "Loudness, EBU R128",
        )
        .at(r)
        .fixed()
        .with_children([
            meter(
                id(0),
                "Integrated loudness",
                big,
                l.integrated,
                -70.0,
                0.0,
                integrated,
            ),
            meter(
                id(1),
                "Short-term loudness",
                row(0),
                l.short_term,
                -70.0,
                0.0,
                reading(l.short_term, "LUFS"),
            ),
            meter(
                id(2),
                "Momentary loudness",
                row(1),
                l.momentary,
                -70.0,
                0.0,
                reading(l.momentary, "LUFS"),
            ),
            meter(
                id(3),
                "Maximum short-term loudness",
                row(2),
                l.max_short_term,
                -70.0,
                0.0,
                reading(l.max_short_term, "LUFS"),
            ),
            meter(
                id(4),
                "True peak",
                row(3),
                l.true_peak,
                -70.0,
                6.0,
                reading(l.true_peak, "dBTP"),
            ),
            meter(
                id(5),
                "Loudness range",
                row(5),
                if measured { l.range } else { f64::NAN },
                0.0,
                30.0,
                if measured {
                    lu(l.range)
                } else {
                    "no reading".into()
                },
            ),
            meter(
                id(6),
                "Peak to loudness ratio",
                row(6),
                if measured { plr } else { f64::NAN },
                0.0,
                30.0,
                if measured && plr.is_finite() {
                    lu(plr)
                } else {
                    "no reading".into()
                },
            ),
            meter(
                id(7),
                "Crest factor",
                row(7),
                d.crest.map_or(f64::NAN, f64::from),
                0.0,
                30.0,
                d.crest
                    .map_or_else(|| "no reading".into(), |c| format!("{c:.1} dB")),
            ),
            meter(
                id(8),
                "Dynamic range",
                row(8),
                d.dr_value().map_or(f64::NAN, f64::from),
                0.0,
                20.0,
                match (d.dr_value(), d.dr) {
                    (Some(v), Some(x)) => format!("DR{v}, {x:.1} dB"),
                    _ => "no reading".into(),
                },
            ),
        ])
    }

    fn level_pane(&self, r: Rect, model: &Session) -> AccessNode<Action> {
        let scale = model.analysis_settings().scale;
        let zero = scale.zero_db();
        let levels = model.analysis_levels();
        let body = r.split_top(20.0).1.inset_xy(8.0, 4.0);
        let w = body.w / 2.0;
        let id = |k: u64| access_id(&[READING, LEVEL, k]);
        let in_scale = |db: f32| match scale {
            LevelScale::Digital => reading(f64::from(db), "dBFS"),
            LevelScale::K(n) if db > LEVEL_FLOOR => {
                format!("{} on K-{n}", format_lufs(f64::from(db - zero)))
            }
            LevelScale::K(_) => "no reading".into(),
        };
        let mut nodes = Vec::new();
        for (c, (lv, side)) in levels.iter().zip(["Left", "Right"]).enumerate() {
            let bar = Rect::new(body.x + c as f32 * w, body.y, w, body.h);
            let k = c as u64 * 2;
            nodes.push(meter(
                id(k),
                &format!("{side} peak"),
                bar,
                f64::from(lv.peak),
                f64::from(LEVEL_FLOOR),
                0.0,
                in_scale(lv.peak),
            ));
            nodes.push(meter(
                id(k + 1),
                &format!("{side} RMS"),
                bar,
                f64::from(lv.rms),
                f64::from(LEVEL_FLOOR),
                0.0,
                in_scale(lv.rms),
            ));
        }
        let max = model.analyzer().level.max_peak();
        nodes.push(meter(
            id(4),
            "Highest peak",
            body.split_bottom(30.0).1,
            f64::from(max),
            f64::from(LEVEL_FLOOR),
            0.0,
            reading(f64::from(max), "dBFS"),
        ));
        AccessNode::new(
            access_id(&[LEVEL]),
            AccessRole::Group,
            format!("Level, {}", scale.label()),
        )
        .at(r)
        .fixed()
        .with_children(nodes)
    }

    fn phase_pane(&self, r: Rect, model: &Session) -> AccessNode<Action> {
        let c = model.analyzer().phase.correlation();
        let words = if c < OUT_OF_PHASE {
            "out of phase"
        } else if c > 0.9 {
            "nearly mono"
        } else if c.abs() <= 0.1 {
            "wide or silent"
        } else {
            "in phase"
        };
        AccessNode::new(access_id(&[PHASE]), AccessRole::Group, "Phase")
            .at(r)
            .fixed()
            .child(meter(
                access_id(&[READING, PHASE, 0]),
                "Correlation",
                r.split_bottom(34.0).1,
                f64::from(c),
                -1.0,
                1.0,
                format!("{}, {words}", correlation_text(c)),
            ))
    }

    /// The spectrum summed up: where it is strongest.
    fn spectrum_pane(&self, r: Rect, model: &Session) -> AccessNode<Action> {
        const POINTS: usize = 64;
        let (curve, _) = model
            .analyzer()
            .spectrum
            .curve(POINTS, SPECTRUM_LO, SPECTRUM_HI);
        let strongest = curve
            .iter()
            .enumerate()
            .filter(|(_, v)| v.is_finite() && **v > -90.0)
            .max_by(|a, b| a.1.total_cmp(b.1));
        let text = match strongest {
            Some((i, v)) => {
                let hz =
                    SPECTRUM_LO * (SPECTRUM_HI / SPECTRUM_LO).powf(i as f64 / (POINTS - 1) as f64);
                format!(
                    "Strongest around {} Hz, {} dBFS",
                    format_hz(hz),
                    format_lufs(f64::from(*v))
                )
            }
            None => "Silent".into(),
        };
        AccessNode::new(
            access_id(&[SPECTRUM]),
            AccessRole::Group,
            "Spectrum of the mid signal",
        )
        .at(r)
        .fixed()
        .child(
            AccessNode::new(access_id(&[READING, SPECTRUM, 0]), AccessRole::Label, text)
                .at(r)
                .tab_stop()
                .fixed(),
        )
    }

    fn codec_pane(&self, c: &CodecLayout, st: &CodecStatus) -> AccessNode<Action> {
        let id = |k: u64| access_id(&[READING, CODEC, k]);
        let db = |v: Option<f64>, unit: &str| match v {
            Some(v) if v.is_finite() => format!("{} {unit}", format!("{v:+.1}").replace('-', "−")),
            Some(_) => format!("−∞ {unit}"),
            None => "no reading".into(),
        };
        let mut nodes: Vec<AccessNode<Action>> = c
            .hear
            .iter()
            .map(|(h, r)| {
                AccessNode::new(
                    access_id(&[CODEC_HEAR, *h as u64]),
                    AccessRole::ToggleButton,
                    format!("Hear the {}", h.label().to_lowercase()),
                )
                .at(*r)
                .fixed()
                .checked(st.hear == *h)
                .on_activate(Action::Codec(CodecOp::Hear(*h)))
            })
            .collect();
        let row = |i: usize| Rect::new(c.rows.x, c.rows.y + i as f32 * 15.0, c.rows.w, 15.0);
        nodes.push(
            AccessNode::new(id(0), AccessRole::Label, st.title.clone())
                .at(row(0))
                .described(st.gain_text.clone())
                .tab_stop()
                .fixed(),
        );
        nodes.push(meter(
            id(1),
            "Platform gain",
            row(1),
            st.gain_db,
            -30.0,
            30.0,
            db(Some(st.gain_db), "dB"),
        ));
        nodes.push(meter(
            id(2),
            "True peak decoded",
            row(3),
            st.true_peak.unwrap_or(f64::NAN),
            -70.0,
            6.0,
            db(st.true_peak, "dBTP"),
        ));
        nodes.push(meter(
            id(3),
            "Frames over full scale",
            row(4),
            st.overs as f64,
            0.0,
            st.overs.max(1) as f64,
            format!("{}", st.overs),
        ));
        nodes.push(meter(
            id(4),
            "Artefacts in the last 3 seconds",
            row(5),
            st.artefacts_now_db.unwrap_or(f64::NAN),
            -120.0,
            0.0,
            db(st.artefacts_now_db, "dB"),
        ));
        nodes.push(meter(
            id(5),
            "Artefacts since the reset",
            row(6),
            st.artefacts_db.unwrap_or(f64::NAN),
            -120.0,
            0.0,
            db(st.artefacts_db, "dB"),
        ));
        nodes.push(
            AccessNode::new(
                access_id(&[CODEC_CHOOSE]),
                AccessRole::Button,
                match st.platform {
                    Some(pl) => format!("Codec: {}", pl.name),
                    None => format!("Codec: Opus {} kb/s", st.kbps),
                },
            )
            .at(c.choose)
            .fixed()
            .with_menu(),
        );
        nodes.push(
            AccessNode::new(
                access_id(&[CODEC_RESET]),
                AccessRole::Button,
                "Reset the codec's measurements",
            )
            .at(c.reset)
            .fixed()
            .on_activate(Action::Codec(CodecOp::ResetMeters)),
        );
        if let Some(e) = &st.error {
            nodes.push(
                AccessNode::new(id(6), AccessRole::Label, e.clone())
                    .at(c.rows)
                    .tab_stop()
                    .fixed(),
            );
        }
        AccessNode::new(access_id(&[CODEC]), AccessRole::Group, "Codec monitor")
            .at(c.pane)
            .fixed()
            .with_children(nodes)
    }

    pub(crate) fn access_nodes(&self, size: Size, model: &Session) -> Vec<AccessNode<Action>> {
        // The codec pane as it is now (painted when the monitor is on).
        let l = self.layout_with(size, model.codec_monitor_on());
        let mut out = vec![self.toolbar(&l, size, model)];
        if l.loudness.w > 0.0 {
            out.push(self.loudness_pane(l.loudness, model));
        }
        if l.level.w > 0.0 {
            out.push(self.level_pane(l.level, model));
        }
        if l.phase.w > 0.0 {
            out.push(self.phase_pane(l.phase, model));
        }
        if let Some(c) = &l.codec {
            out.push(self.codec_pane(c, &model.codec_status()));
        }
        if l.spectrum.w > 60.0 {
            out.push(self.spectrum_pane(l.spectrum, model));
        }
        out
    }

    pub(crate) fn press(
        &mut self,
        id: u64,
        press: AccessPress,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        self.codec_shown = model.codec_monitor_on();
        let l = self.layout(size);
        let under = |r: Rect| Point::new(r.x, r.bottom());
        let menu = matches!(press, AccessPress::Menu | AccessPress::Activate);
        if id == access_id(&[ANNOUNCE]) && press == AccessPress::Activate {
            self.toggle_announce(cx);
        } else if id == access_id(&[SOURCE]) && menu {
            cx.request(Self::source_menu(model, under(l.source)));
        } else if id == access_id(&[TARGET]) && menu {
            cx.request(Self::target_menu(model, under(l.target)));
        } else if id == access_id(&[SCALE]) && menu {
            cx.request(Self::scale_menu(model, under(l.scale)));
        } else if id == access_id(&[CODEC_CHOOSE])
            && menu
            && let Some(c) = &l.codec
        {
            cx.request(Self::codec_menu(model, under(c.choose)));
        } else {
            return false;
        }
        true
    }
}
