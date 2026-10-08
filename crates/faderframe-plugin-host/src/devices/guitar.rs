//! FaderFrame Guitar Station: a line of pedals, a modelled amplifier with
//! its power stage, a loudspeaker in a cabinet and two microphones, from
//! GainStageFx (`faderframe-guitar`), and a DI beside them on a second
//! output.
//!
//! Realtime like the microphone preamps ([`super::preamp`]): every stage of
//! the line -- each pedal, and the amplifier -- runs on a reservoir worker of
//! its own, one device callback behind the stage before it, so a stage's
//! hard samples cost its own thread time and the line never piles up on one
//! core. Each added pedal reports its buffer as latency, which the graph
//! compensates. A stereo signal is solved on two threads, one channel each,
//! and while it is exactly mono one circuit runs and the other wakes from
//! its history (`bank`). Offline the stages run inline with the same delays.

mod bank;
mod processor;
mod stages;
#[cfg(test)]
mod tests;

pub use processor::GuitarProcessor;

use crate::{ParamValues, ParameterInfo, ParameterUnit};
use faderframe_core::ParameterId;
use faderframe_guitar::acoustics::mic::MicPlacement;
use faderframe_guitar::chain::{self, AMPS};
use faderframe_guitar::lists;
use faderframe_guitar::pedal::{Stomp, StompSettings};
use faderframe_guitar::voice::{AcousticSettings, PEDAL_TONES, PowerAmp};

/// Places on the pedal line.
pub const MAX_PEDALS: usize = 8;

/// Stable parameter ids (`id == position` in [`parameters`]).
pub mod id {
    pub const AMP: u32 = 0;
    pub const POWER: u32 = 1;
    pub const DRIVE: u32 = 2;
    pub const MASTER: u32 = 3;
    pub const PRESENCE: u32 = 4;
    pub const BASS: u32 = 5;
    pub const MIDDLE: u32 = 6;
    pub const TREBLE: u32 = 7;
    /// A circuit's fourth tone control (the 800RB's high mid).
    pub const SWEEP: u32 = 8;
    pub const BRIGHT: u32 = 9;
    /// The amplifier's second input jack.
    pub const LOW_INPUT: u32 = 10;
    pub const LOW_SWITCH: u32 = 11;
    pub const MID_SWITCH: u32 = 12;
    pub const REVERB: u32 = 13;
    pub const SPEED: u32 = 14;
    pub const INTENSITY: u32 = 15;
    pub const CHORUS: u32 = 16;
    /// The Cali IIC+'s five graphic sliders: `GRAPHIC + band`.
    pub const GRAPHIC: u32 = 17;
    pub const MAINS: u32 = 22;
    /// Oversampling (changes the latency).
    pub const QUALITY: u32 = 23;
    pub const CABINET: u32 = 24;
    pub const SPEAKER: u32 = 25;
    pub const MIC_A: u32 = 26;
    pub const MIC_B: u32 = 27;
    pub const A_POSITION: u32 = 28;
    pub const A_DISTANCE: u32 = 29;
    pub const A_ANGLE: u32 = 30;
    pub const A_PAN: u32 = 31;
    pub const B_POSITION: u32 = 32;
    pub const B_DISTANCE: u32 = 33;
    pub const B_ANGLE: u32 = 34;
    pub const B_PAN: u32 = 35;
    pub const BLEND: u32 = 36;
    pub const B_INVERT: u32 = 37;
    pub const ALIGN: u32 = 38;
    pub const HORN: u32 = 39;
    pub const DI_SOURCE: u32 = 40;
    pub const MIX: u32 = 41;
    pub const OUTPUT: u32 = 42;
    pub const INPUT: u32 = 43;
    /// The first pedal's parameters; each place has `STRIDE`.
    pub const SLOTS: u32 = 44;
    pub const STRIDE: u32 = 12;
    // Within a place.
    /// What is in it (changes the latency when it empties or fills).
    pub const STOMP: u32 = 0;
    /// The footswitch.
    pub const ON: u32 = 1;
    pub const P_DRIVE: u32 = 2;
    pub const P_LEVEL: u32 = 3;
    /// `TONE + knob`, five of them.
    pub const TONE: u32 = 4;
    pub const TREADLE: u32 = 9;
    pub const AUTO: u32 = 10;
    pub const SENSE: u32 = 11;

    /// Parameter `field` of place `s`.
    pub const fn slot(s: usize, field: u32) -> u32 {
        SLOTS + STRIDE * s as u32 + field
    }

    /// GainStageFx's noise gate after the Input trim (after the places, so
    /// no earlier id moved).
    pub const NOISE_GATE: u32 = SLOTS + STRIDE * super::MAX_PEDALS as u32;
    /// Where it starts to close, dBFS.
    pub const NOISE_THRESHOLD: u32 = NOISE_GATE + 1;
}

/// Published values: the wahs' treadles (one per place), the line's
/// underruns, its latency in samples, and what each stage is doing (for
/// the trace, `FADERFRAME_TRACE_GUITAR=1`).
pub mod value {
    pub const TREADLE: usize = 0;
    pub const UNDERRUNS: usize = super::MAX_PEDALS;
    pub const LATENCY: usize = super::MAX_PEDALS + 1;
    /// 1 while the stages run on their workers (live), 0 inline.
    pub const LIVE: usize = super::MAX_PEDALS + 2;
    /// A stage's buffer, frames.
    pub const DELAY: usize = super::MAX_PEDALS + 3;
    /// Stage `k` (the pedals in line order, then the amplifier): field `f`
    /// at `STAGE + STAGE_STRIDE * k + f`.
    pub const STAGE: usize = super::MAX_PEDALS + 4;
    pub const STAGE_STRIDE: usize = 5;
    /// Frames played late (concealed) or abandoned, so far.
    pub const LATE: usize = 0;
    /// Callbacks that waited for the worker, so far.
    pub const WAITS: usize = 1;
    /// The least the reservoir held ahead of the callback, frames.
    pub const FILL_MIN: usize = 2;
    /// The worker's longest segment, microseconds.
    pub const SEGMENT_MAX: usize = 3;
    /// Samples whose solve the deadline cut short, so far.
    pub const ABORTS: usize = 4;
}
/// Stages a line has at most: the pedals and the amplifier.
pub const STAGES: usize = MAX_PEDALS + 1;
pub const TAP_VALUES: usize = value::STAGE + value::STAGE_STRIDE * STAGES;

/// Whether the line traces itself (`FADERFRAME_TRACE_GUITAR=1`).
pub fn tracing_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("FADERFRAME_TRACE_GUITAR").is_some_and(|v| v != "0"))
}

/// One line of the trace, from the published values.
pub fn trace_line(tap: &crate::tap::AnalysisTap, pedals: usize) -> String {
    use std::fmt::Write;
    let v = |i: usize| tap.value(i);
    let mut line = format!(
        "{} delay {} latency {} late total {}",
        if v(value::LIVE) >= 0.5 {
            "live"
        } else {
            "inline"
        },
        v(value::DELAY),
        v(value::LATENCY),
        v(value::UNDERRUNS)
    );
    for k in 0..=pedals.min(MAX_PEDALS) {
        let at = |f: usize| v(value::STAGE + value::STAGE_STRIDE * k + f);
        let name = if k == pedals {
            "amp".to_string()
        } else {
            format!("p{}", k + 1)
        };
        let _ = write!(
            line,
            " | {name}: late {} waits {} fill_min {} seg_max {} us aborts {}",
            at(value::LATE),
            at(value::WAITS),
            at(value::FILL_MIN),
            at(value::SEGMENT_MAX),
            at(value::ABORTS)
        );
    }
    line
}

fn percent(id: u32, name: &str, default: f64) -> ParameterInfo {
    super::param(id, name, 0.0, 1.0, default, ParameterUnit::Percent)
}

fn toggle(id: u32, name: &str, default: bool) -> ParameterInfo {
    super::stepped(id, name, 1.0, if default { 1.0 } else { 0.0 })
}

pub fn parameters() -> Vec<ParameterInfo> {
    use id::*;
    let unit = |id, name: &str, min, max, default| {
        super::param(id, name, min, max, default, ParameterUnit::None)
    };
    let mut p = vec![
        super::stepped(AMP, "Amplifier", (AMPS.len() - 1) as f64, 3.0),
        super::stepped(POWER, "Power Amp", (PowerAmp::ALL.len() - 1) as f64, 0.0),
        percent(DRIVE, "Drive", 0.5),
        percent(MASTER, "Master", 0.5),
        percent(PRESENCE, "Presence", 0.5),
        percent(BASS, "Bass", 0.5),
        percent(MIDDLE, "Middle", 0.5),
        percent(TREBLE, "Treble", 0.5),
        percent(SWEEP, "High Mid", 0.5),
        toggle(BRIGHT, "Bright", true),
        toggle(LOW_INPUT, "Low Input", false),
        super::stepped(LOW_SWITCH, "Low Switch", 3.0, 1.0),
        super::stepped(MID_SWITCH, "Mid Switch", 3.0, 1.0),
        percent(REVERB, "Reverb", 0.0),
        percent(SPEED, "Speed", 0.4),
        percent(INTENSITY, "Intensity", 0.0),
        percent(CHORUS, "Chorus", 0.0),
    ];
    for (b, name) in [
        "Graphic 80",
        "Graphic 240",
        "Graphic 750",
        "Graphic 2.2k",
        "Graphic 6.6k",
    ]
    .into_iter()
    .enumerate()
    {
        p.push(percent(GRAPHIC + b as u32, name, 0.5));
    }
    p.extend([
        super::stepped(MAINS, "Mains", (lists::MAINS.len() - 1) as f64, 0.0),
        super::fixed(super::stepped(
            QUALITY,
            "Quality",
            (lists::QUALITY.len() - 1) as f64,
            0.0,
        )),
        super::stepped(CABINET, "Cabinet", (lists::CABINETS.len() - 1) as f64, 0.0),
        super::stepped(SPEAKER, "Speaker", (lists::SPEAKERS.len() - 1) as f64, 0.0),
        super::stepped(MIC_A, "Mic A", (lists::MICS.len() - 1) as f64, 2.0),
        super::stepped(MIC_B, "Mic B", (lists::MICS.len() - 1) as f64, 0.0),
        percent(A_POSITION, "Mic A Position", 0.3),
        unit(
            A_DISTANCE,
            "Mic A Distance",
            MicPlacement::MIN_DISTANCE,
            MicPlacement::MAX_DISTANCE,
            0.025,
        ),
        unit(A_ANGLE, "Mic A Angle", 0.0, 90.0, 0.0),
        unit(A_PAN, "Mic A Pan", -1.0, 1.0, 0.0),
        percent(B_POSITION, "Mic B Position", 0.5),
        unit(
            B_DISTANCE,
            "Mic B Distance",
            MicPlacement::MIN_DISTANCE,
            MicPlacement::MAX_DISTANCE,
            0.05,
        ),
        unit(B_ANGLE, "Mic B Angle", 0.0, 90.0, 0.0),
        unit(B_PAN, "Mic B Pan", -1.0, 1.0, 0.0),
        percent(BLEND, "Mic Blend", 0.5),
        toggle(B_INVERT, "Mic B Polarity", false),
        toggle(ALIGN, "Mic Time Align", false),
        percent(HORN, "Horn", 0.5),
        super::stepped(
            DI_SOURCE,
            "DI From",
            (lists::DI_SOURCES.len() - 1) as f64,
            0.0,
        ),
        percent(MIX, "Mix", 1.0),
        super::param(OUTPUT, "Output", -24.0, 24.0, 0.0, ParameterUnit::Decibels),
        super::param(INPUT, "Input", -24.0, 24.0, 0.0, ParameterUnit::Decibels),
    ]);
    for s in 0..MAX_PEDALS {
        let n = |what: &str| format!("Pedal {} {what}", s + 1);
        p.push(super::fixed(super::stepped(
            slot(s, STOMP),
            &n("Model"),
            (Stomp::ALL.len() - 1) as f64,
            0.0,
        )));
        p.push(toggle(slot(s, ON), &n("On"), true));
        p.push(percent(slot(s, P_DRIVE), &n("Drive"), 0.5));
        p.push(percent(slot(s, P_LEVEL), &n("Level"), 0.5));
        for t in 0..PEDAL_TONES {
            p.push(percent(
                slot(s, TONE + t as u32),
                &n(&format!("Tone {}", t + 1)),
                0.5,
            ));
        }
        p.push(percent(slot(s, TREADLE), &n("Treadle"), 0.5));
        p.push(toggle(slot(s, AUTO), &n("Auto"), false));
        p.push(percent(slot(s, SENSE), &n("Sense"), 0.5));
    }
    p.push(toggle(NOISE_GATE, "Noise Gate", false));
    p.push(super::param(
        NOISE_THRESHOLD,
        "Gate Threshold",
        -90.0,
        -30.0,
        -60.0,
        ParameterUnit::Decibels,
    ));
    debug_assert!(
        p.iter()
            .enumerate()
            .all(|(i, info)| info.id.0 as usize == i)
    );
    p
}

fn get(p: &ParamValues, id: u32) -> f64 {
    f64::from(p.get(id as usize))
}

fn index(p: &ParamValues, id: u32) -> usize {
    get(p, id).round().max(0.0) as usize
}

fn on(p: &ParamValues, id: u32) -> bool {
    get(p, id) >= 0.5
}

/// Place `s`'s settings as the parameters have them.
pub fn stomp_settings(p: &ParamValues, s: usize) -> StompSettings {
    let at = |field| get(p, id::slot(s, field));
    let mut tone = [0.5; PEDAL_TONES];
    for (t, v) in tone.iter_mut().enumerate() {
        *v = at(id::TONE + t as u32);
    }
    StompSettings {
        stomp: Stomp::from_index(index(p, id::slot(s, id::STOMP))),
        engaged: on(p, id::slot(s, id::ON)),
        drive: at(id::P_DRIVE),
        level: at(id::P_LEVEL),
        tone,
        treadle: at(id::TREADLE),
        auto: on(p, id::slot(s, id::AUTO)),
        sense: at(id::SENSE),
    }
}

/// The pedals in the line, in order: the places that hold one.
pub fn pedals(p: &ParamValues) -> impl Iterator<Item = (usize, StompSettings)> + '_ {
    (0..MAX_PEDALS)
        .map(|s| (s, stomp_settings(p, s)))
        .filter(|(_, s)| s.stomp != Stomp::Empty)
}

pub fn pedal_count(p: &ParamValues) -> usize {
    pedals(p).count()
}

/// The `n`th pedal of the line (an empty place past the end).
pub(crate) fn nth_pedal(p: &ParamValues, n: usize) -> (usize, StompSettings) {
    pedals(p)
        .nth(n)
        .unwrap_or((MAX_PEDALS, StompSettings::default()))
}

/// The oversampling factor of the quality setting.
pub fn quality(p: &ParamValues) -> usize {
    lists::QUALITY.get(index(p, id::QUALITY)).map_or(1, |q| q.1)
}

/// The amplifier stage's settings as the parameters have them.
pub fn amp_settings(p: &ParamValues) -> chain::Settings {
    use id::*;
    let cab = lists::cabinet(index(p, CABINET));
    let mut graphic = [0.5; 5];
    for (b, g) in graphic.iter_mut().enumerate() {
        *g = get(p, GRAPHIC + b as u32);
    }
    let throw = |id| lists::THROWS[index(p, id).min(3)];
    chain::Settings {
        amp: AMPS[index(p, AMP).min(AMPS.len() - 1)],
        power_amp: PowerAmp::ALL[index(p, POWER).min(PowerAmp::ALL.len() - 1)],
        acoustic: AcousticSettings {
            cabinet: cab.choice,
            speaker: lists::speaker(index(p, SPEAKER)).choice,
            mic_a: lists::mic_slot(index(p, MIC_A), false),
            mic_b: lists::mic_slot(index(p, MIC_B), true),
            place_a: MicPlacement {
                position: get(p, A_POSITION),
                distance: get(p, A_DISTANCE),
                angle: get(p, A_ANGLE),
            },
            place_b: MicPlacement {
                position: get(p, B_POSITION),
                distance: get(p, B_DISTANCE),
                angle: get(p, B_ANGLE),
            },
            blend: get(p, BLEND),
            pan_a: get(p, A_PAN),
            pan_b: get(p, B_PAN),
            invert_b: on(p, B_INVERT),
            align: on(p, ALIGN),
            horn: get(p, HORN),
        },
        legacy_cabinet: cab.legacy,
        drive: get(p, DRIVE),
        master: get(p, MASTER),
        presence: get(p, PRESENCE),
        graphic,
        bass: get(p, BASS),
        mid: get(p, MIDDLE),
        treble: get(p, TREBLE),
        sweep: get(p, SWEEP),
        low_input: on(p, LOW_INPUT),
        bright: on(p, BRIGHT),
        low_switch: throw(LOW_SWITCH),
        mid_switch: throw(MID_SWITCH),
        reverb: get(p, REVERB),
        speed: get(p, SPEED),
        intensity: get(p, INTENSITY),
        chorus: get(p, CHORUS),
        mains: lists::MAINS.get(index(p, MAINS)).map_or(1.0, |m| m.1),
        dry_source: lists::DI_SOURCES
            .get(index(p, DI_SOURCE))
            .map_or(lists::DI_SOURCES[0].1, |d| d.1),
    }
}

/// The host samples one stage of the line delays by: its oversampler's
/// round trip and its reservoir's buffer (one device callback, at least
/// [`super::preamp::BUFFER_LATENCY`]).
pub fn stage_latency(quality: usize, device_block: usize) -> u32 {
    faderframe_guitar::pedal::stage_latency(quality)
        + super::preamp::buffer_delay(device_block) as u32
}

/// The whole line's latency: the amplifier and every added pedal.
pub fn latency(p: &ParamValues, device_block: usize) -> u32 {
    (pedal_count(p) as u32 + 1) * stage_latency(quality(p), device_block)
}

/// The output buses, main first.
pub fn output_bus_names() -> Vec<String> {
    vec![
        "Main".to_string(),
        "DI".to_string(),
        "Mic A".to_string(),
        "Mic B".to_string(),
    ]
}

fn pan_text(v: f64) -> String {
    match (v * 100.0).round() as i32 {
        0 => "C".into(),
        p if p < 0 => format!("L{}", -p),
        p => format!("R{p}"),
    }
}

/// A value as the panel shows it.
pub fn format(id: ParameterId, v: f64) -> Option<String> {
    use id::*;
    let i = v.round().max(0.0) as usize;
    let name = |n: &str| Some(n.to_string());
    match id.0 {
        AMP => name(chain::amp_name(AMPS[i.min(AMPS.len() - 1)])),
        POWER => name(chain::power_name(
            PowerAmp::ALL[i.min(PowerAmp::ALL.len() - 1)],
        )),
        MAINS => lists::MAINS.get(i).and_then(|m| name(m.0)),
        QUALITY => lists::QUALITY.get(i).and_then(|q| name(q.0)),
        CABINET => name(lists::cabinet(i).name),
        SPEAKER => name(lists::speaker(i).name),
        MIC_A | MIC_B => lists::MICS.get(i).and_then(|m| name(m.name)),
        A_DISTANCE | B_DISTANCE => Some(format!("{:.1} cm", v * 100.0)),
        A_ANGLE | B_ANGLE => Some(format!("{v:.0}°")),
        A_PAN | B_PAN => Some(pan_text(v)),
        DI_SOURCE => lists::DI_SOURCES.get(i).and_then(|d| name(d.0)),
        BRIGHT | LOW_INPUT | B_INVERT | ALIGN | NOISE_GATE => {
            name(if v >= 0.5 { "On" } else { "Off" })
        }
        NOISE_THRESHOLD => Some(format!("{v:.0} dBFS").replace('-', "−")),
        x if x >= SLOTS && x < SLOTS + STRIDE * MAX_PEDALS as u32 => match (x - SLOTS) % STRIDE {
            STOMP => name(Stomp::from_index(i).name()),
            ON => name(if v >= 0.5 { "On" } else { "Bypassed" }),
            AUTO => name(if v >= 0.5 { "Auto" } else { "Manual" }),
            _ => None,
        },
        _ => None,
    }
}
