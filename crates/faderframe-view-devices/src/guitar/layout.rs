//! What the panel shows, read from the parameters, and where every control
//! sits on it (panel coordinates). Painting and hit testing both go by it.

use super::{AMP_H, AMP_Y, BOARD_Y, CAB_H, CAB_Y, HEADER_H, OUT_H, OUT_Y, PANEL_W, Travel};
use faderframe_guitar::chain::AMPS;
use faderframe_guitar::lists;
use faderframe_guitar::pedal::Stomp;
use faderframe_guitar::voice::{Gain, PEDAL_TONES, PowerAmp};
use faderframe_plugin_host::devices::guitar::{MAX_PEDALS, id};
use faderframe_plugin_host::tap::AnalysisTap;
use faderframe_ui_canvas::{Point, Rect};

// --- the pedalboard ------------------------------------------------------------

pub const PEDAL_W: f32 = 126.0;
pub const PEDAL_H: f32 = 202.0;
pub const PEDAL_STEP: f32 = 136.0;
pub const PEDAL_X0: f32 = 82.0;
pub const PEDAL_Y: f32 = BOARD_Y + 34.0;

pub fn pedal_rect(i: usize) -> Rect {
    Rect::new(PEDAL_X0 + i as f32 * PEDAL_STEP, PEDAL_Y, PEDAL_W, PEDAL_H)
}

/// Which place of `n` a pedal whose left edge is at `x` would land in.
pub fn place_at(x: f32, n: usize) -> usize {
    let i = ((x - PEDAL_X0) / PEDAL_STEP).round().max(0.0) as usize;
    i.min(n.saturating_sub(1))
}

// --- the amplifier -------------------------------------------------------------

pub fn amp_rect() -> Rect {
    Rect::new(20.0, AMP_Y + 12.0, PANEL_W - 40.0, AMP_H - 24.0)
}

pub fn amp_plate() -> Rect {
    amp_rect().inset(16.0)
}

/// Where a new group starts in the amplifier menu.
pub fn amp_group_starts(i: usize) -> bool {
    // Brit | American (Fender, Roland) | Cali, 5150, Oregon | bass amplifiers
    matches!(i, 8 | 12 | 16)
}

/// What an amplifier's panel looks like.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Finish {
    /// Gold brushed, black lettering.
    Brit,
    /// Copper top panel, cream lettering.
    Copper,
    /// Black panel, white lettering (blackface).
    Blackface,
    /// Brushed aluminium, black lettering.
    Silver,
    /// Black with chrome, red accents.
    Modern,
    /// Black and silver with a blue line.
    Bass,
}

pub fn finish(amp: Gain) -> Finish {
    match amp {
        Gain::Plexi
        | Gain::Brit45
        | Gain::PlexiBass
        | Gain::Brit800
        | Gain::Brit2205
        | Gain::DR103
        | Gain::Brum100 => Finish::Brit,
        Gain::AC30 => Finish::Copper,
        Gain::Twin | Gain::Deluxe | Gain::DeluxeNormal => Finish::Blackface,
        Gain::Jazz120 | Gain::OregonT => Finish::Silver,
        Gain::Boogie | Gain::Recto | Gain::Peavey => Finish::Modern,
        _ => Finish::Bass,
    }
}

// --- the cabinet ------------------------------------------------------------------

pub fn cab_rect() -> Rect {
    Rect::new(20.0, CAB_Y + 10.0, PANEL_W - 40.0, CAB_H - 20.0)
}

/// The cabinet's front, drawn to scale inside this.
pub fn cab_area() -> Rect {
    Rect::new(44.0, CAB_Y + 26.0, 392.0, 230.0)
}

pub const CONE_X: f32 = 590.0;
pub const CONE_Y: f32 = CAB_Y + 100.0;
pub const CONE_R: f32 = 70.0;

/// The side view: the cone's profile on the left, the microphones in front.
pub fn side_rect() -> Rect {
    Rect::new(456.0, CAB_Y + 212.0, 286.0, 52.0)
}
/// Panel pixels for the distances' whole (logarithmic) range.
pub const SIDE_TRAVEL: f32 = 214.0;
/// Panel pixels of vertical drag for 90 degrees.
pub const ANGLE_TRAVEL: f32 = 80.0;

/// Where on the side view a distance (normalised) is.
pub fn side_x(t: f64) -> f32 {
    let r = side_rect();
    r.x + 52.0 + t as f32 * SIDE_TRAVEL
}

// --- the output strip -----------------------------------------------------------

pub fn out_rect() -> Rect {
    Rect::new(20.0, OUT_Y + 6.0, PANEL_W - 40.0, OUT_H - 12.0)
}

pub fn quality_rects() -> [Rect; 2] {
    let w = 36.0;
    let x0 = PANEL_W - 22.0 - 2.0 * (w + 4.0);
    std::array::from_fn(|i| {
        Rect::new(
            x0 + i as f32 * (w + 4.0),
            -HEADER_H + 7.0,
            w,
            HEADER_H - 14.0,
        )
    })
}

// --- what is hit -------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Target {
    Knob {
        id: u32,
        travel: Travel,
        bipolar: bool,
    },
    Slider {
        id: u32,
    },
    Toggle {
        id: u32,
    },
    /// A switch of `positions`: a click throws it to `value`, or to the next.
    Switch {
        id: u32,
        positions: usize,
        value: Option<usize>,
    },
    Segment {
        id: u32,
        value: usize,
    },
    Menu {
        id: u32,
    },
    Footswitch {
        place: usize,
    },
    StompPlate {
        place: usize,
    },
    PedalBody {
        place: usize,
    },
    Remove {
        place: usize,
    },
    Add,
    Treadle {
        id: u32,
        place: usize,
    },
    MicFront {
        mic: usize,
    },
    MicSide {
        mic: usize,
    },
    Meter {
        output: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hot {
    pub rect: Rect,
    pub target: Target,
}

fn around(c: Point, r: f32) -> Rect {
    Rect::new(c.x - r, c.y - r, 2.0 * r, 2.0 * r)
}

/// A knob on the panel.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct KnobAt {
    pub id: u32,
    pub at: Point,
    pub r: f32,
    pub label: &'static str,
    pub bipolar: bool,
}

/// A switch on the amplifier: its positions' names.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SwitchAt {
    pub id: u32,
    pub rect: Rect,
    pub caption: &'static str,
    pub labels: [&'static str; 4],
    pub positions: usize,
}

/// A pedal on the line.
#[derive(Clone, Debug, PartialEq)]
pub struct PedalLook {
    pub slot: usize,
    pub stomp: Stomp,
    pub on: bool,
    pub rect: Rect,
    pub knobs: Vec<KnobAt>,
    pub led: Point,
    pub plate: Rect,
    pub switch: Point,
    pub remove: Rect,
    /// A wah's treadle and its Auto switch.
    pub treadle: Option<Rect>,
    pub auto: Option<Rect>,
}

pub const SWITCH_R: f32 = 15.0;

fn pedal_look(place: usize, slot: usize, stomp: Stomp, on: bool) -> PedalLook {
    let r = pedal_rect(place);
    let cx = r.center().x;
    let field = |f: u32| id::slot(slot, f);
    let mut knobs = Vec::new();
    let mut treadle = None;
    let mut auto = None;
    let knobs_spec = stomp.knobs();
    if knobs_spec.wah {
        knobs.push(KnobAt {
            id: field(id::SENSE),
            at: Point::new(r.x + 26.0, r.y + 30.0),
            r: 11.0,
            label: "SENSE",
            bipolar: false,
        });
        auto = Some(Rect::new(r.right() - 48.0, r.y + 20.0, 36.0, 20.0));
        treadle = Some(Rect::new(r.x + 12.0, r.y + 56.0, r.w - 24.0, 70.0));
    } else {
        // In the box's order: the drive, the tone controls, the level.
        let mut list: Vec<(u32, &'static str)> = Vec::new();
        if let Some(d) = knobs_spec.drive {
            list.push((id::P_DRIVE, d));
        }
        for (t, label) in knobs_spec.tones.iter().enumerate() {
            if let Some(l) = label {
                list.push((id::TONE + t as u32, l));
            }
        }
        if let Some(l) = knobs_spec.level {
            list.push((id::P_LEVEL, l));
        }
        let n = list.len();
        let rows: Vec<usize> = match n {
            0 => vec![],
            1..=3 => vec![n],
            4 => vec![2, 2],
            5 => vec![3, 2],
            6 => vec![3, 3],
            _ => vec![3, n - 3],
        };
        let mut k = 0;
        let top = r.y + 14.0;
        let height = 92.0;
        for (row, &count) in rows.iter().enumerate() {
            let rows_n = rows.len() as f32;
            let y = top + height * (row as f32 + 0.5) / rows_n;
            let radius = match (rows.len(), count) {
                (1, 1) => 18.0,
                (1, _) => 15.0,
                (_, c) if c >= 4 => 10.5,
                _ => 12.5,
            };
            for j in 0..count {
                let x = r.x + 8.0 + (r.w - 16.0) * (j as f32 + 0.5) / count as f32;
                let (f, label) = list[k];
                k += 1;
                knobs.push(KnobAt {
                    id: field(f),
                    at: Point::new(x, y - 4.0),
                    r: radius,
                    label,
                    bipolar: false,
                });
            }
        }
    }
    PedalLook {
        slot,
        stomp,
        on,
        rect: r,
        knobs,
        led: Point::new(cx, r.y + 116.0),
        plate: Rect::new(r.x + 8.0, r.y + 126.0, r.w - 16.0, 22.0),
        switch: Point::new(cx, r.y + 172.0),
        remove: Rect::new(r.right() - 18.0, r.y + 2.0, 16.0, 16.0),
        treadle,
        auto,
    }
}

/// The amplifier's controls for one amplifier and power selection.
#[derive(Clone, Debug, PartialEq)]
pub struct AmpLook {
    pub amp: Gain,
    pub power: PowerAmp,
    pub name: Rect,
    pub power_menu: Rect,
    pub mains: [Rect; 4],
    pub lamp: Point,
    pub knobs: Vec<KnobAt>,
    pub switches: Vec<SwitchAt>,
    pub graphic: Option<[Rect; 5]>,
}

pub const AMP_KNOB_R: f32 = 24.0;

fn amp_look(amp: Gain, power: PowerAmp) -> AmpLook {
    let plate = amp_plate();
    let mut knobs: Vec<(u32, &'static str, bool)> = vec![(id::DRIVE, amp.drive_name(), false)];
    let tone = amp.own_tone_knobs();
    for (on, (id, label)) in tone.iter().zip([
        (id::BASS, "BASS"),
        (id::MIDDLE, "MIDDLE"),
        (id::TREBLE, "TREBLE"),
    ]) {
        if *on {
            knobs.push((id, label, false));
        }
    }
    if let Some((_, name)) = amp.own_sweep() {
        knobs.push((id::SWEEP, name, false));
    }
    let resolved = power.resolved(amp);
    if amp.own_presence().is_some() {
        knobs.push((id::PRESENCE, "PRESENCE", false));
    } else if let Some(name) = resolved.and_then(|m| m.presence_name()) {
        knobs.push((id::PRESENCE, name, false));
    }
    let overridden = power != PowerAmp::Matched && resolved.is_some();
    if amp.level_control().is_some() || overridden {
        knobs.push((
            id::MASTER,
            if overridden {
                "MASTER"
            } else {
                amp.level_name()
            },
            false,
        ));
    }
    if amp.has_reverb() {
        knobs.push((id::REVERB, "REVERB", false));
    }
    if amp.has_tremolo() {
        knobs.push((id::SPEED, "SPEED", false));
        knobs.push((id::INTENSITY, "INTENSITY", false));
    }
    if amp.has_chorus() {
        knobs.push((id::CHORUS, "CHORUS", false));
    }
    let graphic = amp.has_graphic().then(|| {
        std::array::from_fn(|b| {
            Rect::new(
                plate.right() - 186.0 + b as f32 * 36.0,
                plate.y + 34.0,
                22.0,
                118.0,
            )
        })
    });
    let x0 = plate.x + 300.0;
    let x1 = if graphic.is_some() {
        plate.right() - 210.0
    } else {
        plate.right() - 20.0
    };
    let n = knobs.len().max(1) as f32;
    let step = ((x1 - x0) / n).min(112.0);
    let start = x0 + ((x1 - x0) - step * n) / 2.0;
    let knobs = knobs
        .into_iter()
        .enumerate()
        .map(|(i, (id, label, bipolar))| KnobAt {
            id,
            at: Point::new(start + step * (i as f32 + 0.5), plate.y + 84.0),
            r: AMP_KNOB_R,
            label,
            bipolar,
        })
        .collect();
    // The circuit's own switches, under the knobs.
    let mut switches = Vec::new();
    if let Some(b) = amp.bright_switch() {
        switches.push((id::BRIGHT, "BRIGHT", ["Off", b.on_label, "", ""], 2));
    }
    if let Some(j) = amp.input_jacks() {
        switches.push((
            id::LOW_INPUT,
            "INPUT",
            [j.high_label, j.low_label, "", ""],
            2,
        ));
    }
    for (id, caption, sw) in [
        (id::LOW_SWITCH, "LOW", amp.low_switch()),
        (id::MID_SWITCH, "MID", amp.mid_switch()),
    ] {
        if let Some(sw) = sw {
            let mut labels = ["", "", "", ""];
            for (l, s) in labels.iter_mut().zip(sw.labels) {
                *l = s;
            }
            switches.push((id, caption, labels, sw.labels.len().min(4)));
        }
    }
    let sw_w = 168.0;
    let switches = switches
        .into_iter()
        .enumerate()
        .map(|(i, (id, caption, labels, positions))| SwitchAt {
            id,
            rect: Rect::new(x0 + i as f32 * (sw_w + 12.0), plate.y + 160.0, sw_w, 26.0),
            caption,
            labels,
            positions,
        })
        .collect();
    let mains_w = 42.0;
    AmpLook {
        amp,
        power,
        name: Rect::new(plate.x + 18.0, plate.y + 16.0, 262.0, 58.0),
        power_menu: Rect::new(plate.x + 66.0, plate.y + 100.0, 214.0, 26.0),
        mains: std::array::from_fn(|i| {
            Rect::new(
                plate.x + 66.0 + i as f32 * (mains_w + 15.0),
                plate.y + 154.0,
                mains_w,
                22.0,
            )
        }),
        lamp: Point::new(plate.x + 36.0, plate.y + 128.0),
        knobs,
        switches,
        graphic,
    }
}

/// The cabinet section's controls.
#[derive(Clone, Debug, PartialEq)]
pub struct CabLook {
    pub cabinet: usize,
    pub physical: bool,
    pub menus: [(u32, Rect, &'static str); 4],
    pub knobs: Vec<KnobAt>,
    pub toggles: [(u32, Rect, &'static str, [&'static str; 2]); 2],
    pub mic_b: bool,
}

fn cab_look(cabinet: usize, mic_b: bool) -> CabLook {
    let entry = lists::cabinet(cabinet);
    let physical = !matches!(
        entry.choice,
        faderframe_guitar::voice::CabinetChoice::Legacy
    );
    let r = cab_rect();
    let col = |i: usize| r.x + 778.0 + i as f32 * 208.0;
    let menus = [
        (
            id::CABINET,
            Rect::new(col(0), r.y + 30.0, 196.0, 26.0),
            "CABINET",
        ),
        (
            id::SPEAKER,
            Rect::new(col(1), r.y + 30.0, 196.0, 26.0),
            "SPEAKER",
        ),
        (
            id::MIC_A,
            Rect::new(col(0), r.y + 86.0, 196.0, 26.0),
            "MIC A",
        ),
        (
            id::MIC_B,
            Rect::new(col(1), r.y + 86.0, 196.0, 26.0),
            "MIC B",
        ),
    ];
    let horn = match entry.choice {
        faderframe_guitar::voice::CabinetChoice::Model(p) => p.horn.is_some(),
        _ => false,
    };
    let mut list = vec![
        (id::BLEND, "BLEND", false),
        (id::A_PAN, "PAN A", true),
        (id::B_PAN, "PAN B", true),
    ];
    if horn {
        list.push((id::HORN, "HORN", false));
    }
    let knobs = list
        .into_iter()
        .enumerate()
        .map(|(i, (id, label, bipolar))| KnobAt {
            id,
            at: Point::new(col(0) + 40.0 + i as f32 * 98.0, r.y + 170.0),
            r: 19.0,
            label,
            bipolar,
        })
        .collect();
    CabLook {
        cabinet,
        physical,
        menus,
        knobs,
        toggles: [
            (
                id::B_INVERT,
                Rect::new(col(0), r.y + 222.0, 196.0, 24.0),
                "B POLARITY",
                ["Normal", "Inverted"],
            ),
            (
                id::ALIGN,
                Rect::new(col(1), r.y + 222.0, 196.0, 24.0),
                "TIME",
                ["Physical", "Aligned"],
            ),
        ],
        mic_b,
    }
}

/// The output strip's controls.
#[derive(Clone, Debug, PartialEq)]
pub struct OutLook {
    pub input: KnobAt,
    pub mix: KnobAt,
    pub output: KnobAt,
    pub meters: [Rect; 2],
    pub di: [Rect; 3],
    pub readout: Rect,
}

fn out_look() -> OutLook {
    let r = out_rect();
    let y = r.y + r.h / 2.0 + 4.0;
    let knob = |id, label, x| KnobAt {
        id,
        at: Point::new(x, y),
        r: 17.0,
        label,
        bipolar: false,
    };
    OutLook {
        input: knob(id::INPUT, "INPUT", r.x + 44.0),
        mix: knob(id::MIX, "MIX", r.x + 632.0),
        output: knob(id::OUTPUT, "OUTPUT", r.x + 716.0),
        meters: [
            Rect::new(r.x + 86.0, r.y + 26.0, 210.0, 26.0),
            Rect::new(r.x + 760.0, r.y + 26.0, 210.0, 26.0),
        ],
        di: std::array::from_fn(|i| {
            Rect::new(r.x + 340.0 + i as f32 * 82.0, r.y + 30.0, 78.0, 24.0)
        }),
        readout: Rect::new(r.x + 996.0, r.y + 12.0, 190.0, 50.0),
    }
}

/// Everything the panel shows.
#[derive(Clone, Debug, PartialEq)]
pub struct Look {
    pub pedals: Vec<PedalLook>,
    pub add: Option<Rect>,
    pub amp: AmpLook,
    pub cab: CabLook,
    pub out: OutLook,
}

impl Look {
    pub fn read(tap: &AnalysisTap) -> Self {
        let get = |id: u32| f64::from(tap.params.get(id as usize));
        let mut pedals = Vec::new();
        for slot in 0..MAX_PEDALS {
            let stomp = Stomp::from_index(get(id::slot(slot, id::STOMP)).round().max(0.0) as usize);
            if stomp == Stomp::Empty {
                continue;
            }
            let on = get(id::slot(slot, id::ON)) >= 0.5;
            pedals.push(pedal_look(pedals.len(), slot, stomp, on));
        }
        let add = (pedals.len() < MAX_PEDALS).then(|| pedal_rect(pedals.len()));
        let amp = AMPS[(get(id::AMP).round().max(0.0) as usize).min(AMPS.len() - 1)];
        let power =
            PowerAmp::ALL[(get(id::POWER).round().max(0.0) as usize).min(PowerAmp::ALL.len() - 1)];
        let cabinet = (get(id::CABINET).round().max(0.0) as usize).min(lists::CABINETS.len() - 1);
        let mic_b = get(id::MIC_B).round() as usize != 0;
        Self {
            pedals,
            add,
            amp: amp_look(amp, power),
            cab: cab_look(cabinet, mic_b),
            out: out_look(),
        }
    }

    /// Every control, in painting order (later ones are on top).
    pub fn hots(&self) -> Vec<Hot> {
        let mut h = Vec::new();
        let knob = |h: &mut Vec<Hot>, k: &KnobAt, travel| {
            h.push(Hot {
                rect: around(k.at, k.r + 4.0),
                target: Target::Knob {
                    id: k.id,
                    travel,
                    bipolar: k.bipolar,
                },
            })
        };
        // Header.
        for (i, r) in quality_rects().iter().enumerate() {
            h.push(Hot {
                rect: *r,
                target: Target::Segment {
                    id: id::QUALITY,
                    value: i,
                },
            });
        }
        // The line.
        for (place, p) in self.pedals.iter().enumerate() {
            h.push(Hot {
                rect: p.rect,
                target: Target::PedalBody { place },
            });
            for k in &p.knobs {
                knob(&mut h, k, Travel::Linear);
            }
            if let Some(t) = p.treadle {
                h.push(Hot {
                    rect: t,
                    target: Target::Treadle {
                        id: id::slot(p.slot, id::TREADLE),
                        place,
                    },
                });
            }
            if let Some(a) = p.auto {
                h.push(Hot {
                    rect: a,
                    target: Target::Toggle {
                        id: id::slot(p.slot, id::AUTO),
                    },
                });
            }
            h.push(Hot {
                rect: p.plate,
                target: Target::StompPlate { place },
            });
            h.push(Hot {
                rect: around(p.switch, SWITCH_R + 6.0),
                target: Target::Footswitch { place },
            });
            h.push(Hot {
                rect: p.remove,
                target: Target::Remove { place },
            });
        }
        if let Some(r) = self.add {
            h.push(Hot {
                rect: r,
                target: Target::Add,
            });
        }
        // The amplifier.
        let a = &self.amp;
        h.push(Hot {
            rect: a.name,
            target: Target::Menu { id: id::AMP },
        });
        h.push(Hot {
            rect: a.power_menu,
            target: Target::Menu { id: id::POWER },
        });
        for (i, r) in a.mains.iter().enumerate() {
            h.push(Hot {
                rect: *r,
                target: Target::Segment {
                    id: id::MAINS,
                    value: i,
                },
            });
        }
        for k in &a.knobs {
            knob(&mut h, k, Travel::Linear);
        }
        for s in &a.switches {
            h.push(Hot {
                rect: s.rect,
                target: Target::Switch {
                    id: s.id,
                    positions: s.positions,
                    value: None,
                },
            });
        }
        if let Some(g) = &a.graphic {
            for (b, r) in g.iter().enumerate() {
                h.push(Hot {
                    rect: *r,
                    target: Target::Slider {
                        id: id::GRAPHIC + b as u32,
                    },
                });
            }
        }
        // The cabinet.
        let c = &self.cab;
        for (id, r, _) in &c.menus {
            h.push(Hot {
                rect: *r,
                target: Target::Menu { id: *id },
            });
        }
        if c.physical {
            for k in &c.knobs {
                knob(&mut h, k, Travel::Linear);
            }
            for (id, r, _, _) in &c.toggles {
                h.push(Hot {
                    rect: *r,
                    target: Target::Toggle { id: *id },
                });
            }
        }
        // The output strip.
        let o = &self.out;
        for k in [&o.input, &o.mix, &o.output] {
            knob(&mut h, k, Travel::Linear);
        }
        for (i, r) in o.di.iter().enumerate() {
            h.push(Hot {
                rect: *r,
                target: Target::Segment {
                    id: id::DI_SOURCE,
                    value: i,
                },
            });
        }
        for (i, r) in o.meters.iter().enumerate() {
            h.push(Hot {
                rect: *r,
                target: Target::Meter { output: i == 1 },
            });
        }
        h
    }

    /// The microphones' markers, given the parameters (drawn and hit by the
    /// painter, which knows their values).
    pub fn mic_hots(&self, tap: &AnalysisTap) -> Vec<Hot> {
        if !self.cab.physical {
            return Vec::new();
        }
        let get = |id: u32| f64::from(tap.params.get(id as usize));
        let mut h = Vec::new();
        for mic in 0..2 {
            if mic == 1 && !self.cab.mic_b {
                continue;
            }
            let (p, d) = if mic == 0 {
                (id::A_POSITION, id::A_DISTANCE)
            } else {
                (id::B_POSITION, id::B_DISTANCE)
            };
            let front = mic_front(mic, get(p));
            h.push(Hot {
                rect: around(front, 13.0),
                target: Target::MicFront { mic },
            });
            let side = Point::new(
                side_x(distance_norm(get(d))),
                side_rect().y + 16.0 + 20.0 * mic as f32,
            );
            h.push(Hot {
                rect: around(side, 12.0),
                target: Target::MicSide { mic },
            });
        }
        h
    }
}

/// A microphone's place on the zoomed cone: A to the left of the dust cap,
/// B to the right (the cone is round; only the distance from its centre
/// matters).
pub fn mic_front(mic: usize, position: f64) -> Point {
    let sign = if mic == 0 { -1.0 } else { 1.0 };
    Point::new(CONE_X + sign * position as f32 * CONE_R, CONE_Y)
}

/// A distance's place on the side view's logarithmic travel.
pub fn distance_norm(d: f64) -> f64 {
    use faderframe_guitar::acoustics::mic::MicPlacement;
    let (lo, hi) = (MicPlacement::MIN_DISTANCE, MicPlacement::MAX_DISTANCE);
    ((d.max(lo) / lo).ln() / (hi / lo).ln()).clamp(0.0, 1.0)
}

/// The amplifier's nameplate (the menu), for tests.
#[cfg(test)]
pub fn amp_look_for_tests() -> Rect {
    amp_look(AMPS[0], PowerAmp::Matched).name
}

/// Tones a pedal panel names, for tests.
#[allow(dead_code)]
pub fn knob_count(stomp: Stomp) -> usize {
    let k = stomp.knobs();
    usize::from(k.drive.is_some())
        + usize::from(k.level.is_some())
        + k.tones
            .iter()
            .take(PEDAL_TONES)
            .filter(|t| t.is_some())
            .count()
        + if k.wah { 1 } else { 0 }
}
