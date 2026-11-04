//! DX7 voices from SysEx: a 32-voice bulk dump (packed VMEM, 4104 bytes
//! with its header, or the bare 4096) or a single voice (VCED, 163 bytes,
//! or the bare 155), parsed from the published format description, and
//! turned into the FM instrument's parameters. Envelope rates become times
//! by the original's timing (the level falls at a rate doubling every 6¼
//! steps; an attack climbs about eight times faster); levels keep their
//! 0–99 scale; keyboard scaling depths of 99 are ±24 dB an octave.

use super::id;
use crate::ParameterInfo;
use faderframe_core::ParameterId;

/// A voice: its name and every parameter of the FM instrument.
#[derive(Clone, Debug, PartialEq)]
pub struct Dx7Voice {
    pub name: String,
    pub values: Vec<(ParameterId, f64)>,
}

/// One operator's settings as the dump has them.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct RawOp {
    rates: [u8; 4],
    levels: [u8; 4],
    break_point: u8,
    left_depth: u8,
    right_depth: u8,
    left_curve: u8,
    right_curve: u8,
    rate_scale: u8,
    amp_mod: u8,
    velocity: u8,
    level: u8,
    fixed: bool,
    coarse: u8,
    fine: u8,
    detune: u8,
}

/// A voice as the dump has it (operators 1–6 at index 0–5).
#[derive(Clone, Debug, Default, PartialEq)]
struct RawVoice {
    ops: [RawOp; 6],
    pitch_rates: [u8; 4],
    pitch_levels: [u8; 4],
    algorithm: u8,
    feedback: u8,
    key_sync: bool,
    lfo_speed: u8,
    lfo_pitch_depth: u8,
    lfo_amp_depth: u8,
    lfo_wave: u8,
    pitch_sensitivity: u8,
    transpose: u8,
    name: String,
}

fn name_of(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| match b & 0x7f {
            c @ 0x20..=0x7e => c as char,
            _ => ' ',
        })
        .collect::<String>()
        .trim()
        .to_string()
}

/// A packed voice (128 bytes).
fn packed(d: &[u8]) -> RawVoice {
    let mut v = RawVoice::default();
    for block in 0..6 {
        let o = &d[block * 17..block * 17 + 17];
        // Operator 6 first.
        v.ops[5 - block] = RawOp {
            rates: [o[0], o[1], o[2], o[3]],
            levels: [o[4], o[5], o[6], o[7]],
            break_point: o[8],
            left_depth: o[9],
            right_depth: o[10],
            left_curve: o[11] & 3,
            right_curve: (o[11] >> 2) & 3,
            rate_scale: o[12] & 7,
            detune: (o[12] >> 3) & 15,
            amp_mod: o[13] & 3,
            velocity: (o[13] >> 2) & 7,
            level: o[14],
            fixed: o[15] & 1 == 1,
            coarse: (o[15] >> 1) & 31,
            fine: o[16],
        };
    }
    v.pitch_rates = [d[102], d[103], d[104], d[105]];
    v.pitch_levels = [d[106], d[107], d[108], d[109]];
    v.algorithm = d[110] & 31;
    v.feedback = d[111] & 7;
    v.key_sync = (d[111] >> 3) & 1 == 1;
    v.lfo_speed = d[112];
    v.lfo_pitch_depth = d[114];
    v.lfo_amp_depth = d[115];
    v.lfo_wave = (d[116] >> 1) & 7;
    v.pitch_sensitivity = (d[116] >> 4) & 7;
    v.transpose = d[117];
    v.name = name_of(&d[118..128]);
    v
}

/// An unpacked single voice (155 bytes).
fn unpacked(d: &[u8]) -> RawVoice {
    let mut v = RawVoice::default();
    for block in 0..6 {
        let o = &d[block * 21..block * 21 + 21];
        v.ops[5 - block] = RawOp {
            rates: [o[0], o[1], o[2], o[3]],
            levels: [o[4], o[5], o[6], o[7]],
            break_point: o[8],
            left_depth: o[9],
            right_depth: o[10],
            left_curve: o[11] & 3,
            right_curve: o[12] & 3,
            rate_scale: o[13] & 7,
            amp_mod: o[14] & 3,
            velocity: o[15] & 7,
            level: o[16],
            fixed: o[17] & 1 == 1,
            coarse: o[18] & 31,
            fine: o[19],
            detune: o[20] & 15,
        };
    }
    v.pitch_rates = [d[126], d[127], d[128], d[129]];
    v.pitch_levels = [d[130], d[131], d[132], d[133]];
    v.algorithm = d[134] & 31;
    v.feedback = d[135] & 7;
    v.key_sync = d[136] & 1 == 1;
    v.lfo_speed = d[137];
    v.lfo_pitch_depth = d[139];
    v.lfo_amp_depth = d[140];
    v.lfo_wave = d[142];
    v.pitch_sensitivity = d[143] & 7;
    v.transpose = d[144];
    v.name = name_of(&d[145..155]);
    v
}

/// The voices of a SysEx file (or a bare voice block). Checksums are not
/// required to hold: many dumps in the wild have them wrong.
pub fn parse(bytes: &[u8]) -> Result<Vec<Dx7Voice>, String> {
    let raws: Vec<RawVoice> =
        if bytes.len() >= 4104 && bytes[0] == 0xF0 && bytes[1] == 0x43 && bytes[3] == 0x09 {
            let data = &bytes[6..6 + 4096];
            (0..32)
                .map(|i| packed(&data[i * 128..i * 128 + 128]))
                .collect()
        } else if bytes.len() >= 163 && bytes[0] == 0xF0 && bytes[1] == 0x43 && bytes[3] == 0x00 {
            vec![unpacked(&bytes[6..6 + 155])]
        } else if bytes.len() == 4096 {
            (0..32)
                .map(|i| packed(&bytes[i * 128..i * 128 + 128]))
                .collect()
        } else if bytes.len() == 155 {
            vec![unpacked(bytes)]
        } else {
            return Err("not a DX7 voice dump (32 voices or one)".into());
        };
    let infos = super::parameters();
    Ok(raws.iter().map(|r| voice(r, &infos)).collect())
}

/// Seconds a level step takes at rate `r` (0–99) falling: the original
/// moves its level in 6 dB steps that halve in time every 6¼ rate steps.
fn fall_seconds_per_step(rate: u8) -> f64 {
    2.97 * 2f64.powf(-0.16 * f64::from(rate.min(99)))
}

/// A falling (or rising, slower) stage's time from level `a` to `b`.
fn stage_ms(rate: u8, a: u8, b: u8) -> f64 {
    let steps = f64::from(a.abs_diff(b));
    steps * fall_seconds_per_step(rate) * 1000.0
}

/// An attack's time to level `to` from silence (about eight times as
/// fast as a fall, and from where its jump lands).
fn attack_ms(rate: u8, to: u8) -> f64 {
    f64::from(to) * fall_seconds_per_step(rate) * 1000.0 / 8.0
}

/// The pitch envelope's levels: 50 is the key, ±4 octaves at the ends.
fn pitch_semis(level: u8) -> f64 {
    (f64::from(level.min(99)) - 50.0) / 50.0 * 48.0
}

/// The FM instrument's values for a voice.
fn voice(r: &RawVoice, infos: &[ParameterInfo]) -> Dx7Voice {
    use id::*;
    let mut set: Vec<(u32, f64)> = vec![
        (ALGORITHM, f64::from(r.algorithm.min(31))),
        (FEEDBACK, f64::from(r.feedback) / 7.0),
        (TRANSPOSE, f64::from(r.transpose.min(48)) - 24.0),
        (KEY_SYNC, if r.key_sync { 1.0 } else { 0.0 }),
        // The voice's own amplifier stays out of the way: the operators
        // shape it.
        (id::ATTACK, 0.5),
        (id::DECAY, 10_000.0),
        (id::SUSTAIN, 1.0),
        (id::RELEASE, 10_000.0),
        (FILTER_TYPE, 0.0),
        (VELOCITY_AMP, 0.0),
        (VOLUME, -10.0),
    ];
    for (k, o) in r.ops.iter().enumerate() {
        let [r1, r2, r3, r4] = o.rates;
        let [l1, l2, l3, _] = o.levels;
        let depth = |d: u8, curve: u8| {
            // Curves 0 and 1 fall away from the break, 2 and 3 rise.
            let sign = if curve < 2 { -1.0 } else { 1.0 };
            sign * f64::from(d.min(99)) / 99.0
        };
        let freq = 10f64
            .powf(f64::from(o.coarse & 3) + f64::from(o.fine.min(99)) * 9.772f64.log10() / 99.0);
        set.extend([
            (op(k, RATIO), f64::from(o.coarse.min(31))),
            (op(k, FINE), f64::from(o.fine.min(99)) / 100.0),
            (op(k, FIXED), if o.fixed { 1.0 } else { 0.0 }),
            (op(k, FREQ), freq),
            (op(k, DETUNE), (f64::from(o.detune.min(14)) - 7.0) * 1.5),
            (op(k, id::LEVEL), f64::from(o.level.min(99))),
            (op(k, VELOCITY), f64::from(o.velocity) / 7.0),
            (op(k, AMP_MOD), f64::from(o.amp_mod) / 3.0),
            (op(k, id::OP_ATTACK), attack_ms(r1, l1)),
            (op(k, PEAK), f64::from(l1.min(99))),
            (op(k, id::OP_DECAY), stage_ms(r2, l1, l2)),
            (op(k, BREAK), f64::from(l2.min(99))),
            (op(k, SLOPE), stage_ms(r3, l2, l3)),
            (op(k, id::OP_SUSTAIN), f64::from(l3.min(99))),
            // A release is timed from full level (its rate is the same
            // whatever the sustain).
            (op(k, id::OP_RELEASE), stage_ms(r4, 99, 0)),
            (op(k, RATE_SCALE), f64::from(o.rate_scale) / 7.0),
            (op(k, KEY_BREAK), f64::from(o.break_point.min(99)) + 21.0),
            (op(k, KEY_LEFT), depth(o.left_depth, o.left_curve)),
            (op(k, KEY_RIGHT), depth(o.right_depth, o.right_curve)),
        ]);
    }
    // The pitch envelope, as far as two segments carry it: from where it
    // starts to where it holds, and on release to where it ends.
    let [pr1, pr2, pr3, pr4] = r.pitch_rates;
    let [pl1, pl2, pl3, pl4] = r.pitch_levels;
    let hold = pitch_semis(pl3);
    let start = (pitch_semis(pl4) - hold).clamp(-24.0, 24.0);
    let time = stage_ms(pr1, pl4, pl1) + stage_ms(pr2, pl1, pl2) + stage_ms(pr3, pl2, pl3);
    set.extend([
        (PITCH_START, start),
        (PITCH_TIME, time),
        (PITCH_END, start),
        (PITCH_RELEASE, stage_ms(pr4, pl3, pl4)),
    ]);
    // A held offset becomes part of the transpose.
    if hold.abs() >= 0.5 {
        let t = (f64::from(r.transpose.min(48)) - 24.0 + hold.round()).clamp(-24.0, 24.0);
        set.push((TRANSPOSE, t));
    }
    // LFO 1: speed, shape, depth to pitch (by the pitch sensitivity) and
    // to amplitude.
    const SENSITIVITY_CENTS: [f64; 8] = [0.0, 10.0, 20.0, 33.0, 55.0, 92.0, 153.0, 254.0];
    const SHAPE: [f64; 6] = [1.0, 2.0, 2.0, 3.0, 0.0, 4.0];
    set.extend([
        (
            LFO_RATE,
            (0.062 * 1.069f64.powf(f64::from(r.lfo_speed.min(99)))).min(20.0),
        ),
        (LFO_SYNC, 0.0),
        (LFO_SHAPE, SHAPE[usize::from(r.lfo_wave.min(5))]),
        (
            LFO_PITCH,
            SENSITIVITY_CENTS[usize::from(r.pitch_sensitivity.min(7))]
                * f64::from(r.lfo_pitch_depth.min(99))
                / 99.0,
        ),
        (LFO_AMP, f64::from(r.lfo_amp_depth.min(99)) / 99.0),
    ]);
    let values = infos
        .iter()
        .map(|p| {
            let v = set
                .iter()
                .rev()
                .find(|(i, _)| *i == p.id.0)
                .map_or(p.default, |(_, v)| *v);
            (p.id, p.clamp(v))
        })
        .collect();
    Dx7Voice {
        name: r.name.clone(),
        values,
    }
}

/// A single voice dump (VCED with its header and checksum) of `voice`'s
/// raw form: what [`parse`] reads back (for tests and round trips).
#[cfg(test)]
pub(crate) fn single_voice_dump(data: &[u8; 155]) -> Vec<u8> {
    let mut out = vec![0xF0, 0x43, 0x00, 0x00, 0x01, 0x1B];
    out.extend_from_slice(data);
    let sum: u32 = data.iter().map(|b| u32::from(*b)).sum();
    out.push(((128 - (sum & 0x7f)) & 0x7f) as u8);
    out.push(0xF7);
    out
}

/// A bulk dump of 32 packed voices (VMEM with its header and checksum).
#[cfg(test)]
pub(crate) fn bulk_dump(voices: &[[u8; 128]; 32]) -> Vec<u8> {
    let mut out = vec![0xF0, 0x43, 0x00, 0x09, 0x20, 0x00];
    let mut sum = 0u32;
    for v in voices {
        out.extend_from_slice(v);
        sum += v.iter().map(|b| u32::from(*b)).sum::<u32>();
    }
    out.push(((128 - (sum & 0x7f)) & 0x7f) as u8);
    out.push(0xF7);
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    /// A packed voice: an electric-piano-like pair (algorithm 5).
    fn packed_voice(name: &str) -> [u8; 128] {
        let mut d = [0u8; 128];
        for block in 0..6 {
            let o = &mut d[block * 17..block * 17 + 17];
            let op = 6 - block;
            o[0..4].copy_from_slice(&[96, 25, 25, 67]);
            o[4..8].copy_from_slice(&[99, 75, 0, 0]);
            o[8] = 39;
            o[9] = 0;
            o[10] = 30;
            o[11] = 0b11 << 2;
            o[12] = 3 | (7 << 3);
            o[13] = (2 << 2) | 1;
            o[14] = if op % 2 == 1 { 99 } else { 78 };
            o[15] = (if op == 2 { 14 } else { 1 }) << 1;
            o[16] = 0;
        }
        d[102..106].copy_from_slice(&[99, 99, 99, 99]);
        d[106..110].copy_from_slice(&[50, 50, 50, 50]);
        d[110] = 4;
        d[111] = 6 | (1 << 3);
        d[112] = 35;
        d[116] = (3 << 4) | (4 << 1);
        d[117] = 24;
        let bytes = name.as_bytes();
        for i in 0..10 {
            d[118 + i] = *bytes.get(i).unwrap_or(&b' ');
        }
        d
    }

    #[test]
    fn a_bulk_dump_gives_32_voices() {
        let mut voices = [[0u8; 128]; 32];
        for (i, v) in voices.iter_mut().enumerate() {
            *v = packed_voice(&format!("E.PIANO {i}"));
        }
        let dump = bulk_dump(&voices);
        assert_eq!(dump.len(), 4104);
        let got = parse(&dump).unwrap();
        assert_eq!(got.len(), 32);
        assert_eq!(got[3].name, "E.PIANO 3");
        let get = |v: &Dx7Voice, id: u32| v.values.iter().find(|(p, _)| p.0 == id).unwrap().1;
        let v = &got[0];
        assert_eq!(get(v, id::ALGORITHM), 4.0, "algorithm 5");
        assert!((get(v, id::FEEDBACK) - 6.0 / 7.0).abs() < 1e-9);
        assert_eq!(get(v, id::KEY_SYNC), 1.0);
        assert_eq!(get(v, id::TRANSPOSE), 0.0);
        // Operator 2's coarse ratio 14, the carriers at 99.
        assert_eq!(get(v, id::op(1, id::RATIO)), 14.0);
        assert_eq!(get(v, id::op(0, id::LEVEL)), 99.0);
        assert_eq!(get(v, id::op(1, id::LEVEL)), 78.0);
        assert_eq!(get(v, id::op(0, id::KEY_BREAK)), 60.0);
        assert!(get(v, id::op(0, id::KEY_RIGHT)) > 0.0, "a + curve");
        assert!((get(v, id::op(0, id::VELOCITY)) - 2.0 / 7.0).abs() < 1e-9);
        // A fast attack, a decay of seconds.
        assert!(get(v, id::op(0, id::OP_ATTACK)) < 5.0);
        let decay = get(v, id::op(0, id::OP_DECAY));
        assert!((100.0..30_000.0).contains(&decay), "{decay}");
        // The bare 4096 bytes read the same.
        let bare = parse(&dump[6..6 + 4096]).unwrap();
        assert_eq!(bare, got);
        assert!(parse(&[0xF0, 0x43, 0, 9]).is_err());
    }

    #[test]
    fn a_single_voice_dump_reads() {
        let mut d = [0u8; 155];
        for block in 0..6 {
            let o = &mut d[block * 21..block * 21 + 21];
            o[0..4].copy_from_slice(&[99, 50, 40, 60]);
            o[4..8].copy_from_slice(&[99, 80, 70, 0]);
            o[8] = 39;
            o[16] = if block == 5 { 99 } else { 0 };
            o[18] = 1;
            o[20] = 7;
        }
        d[130..134].copy_from_slice(&[50, 50, 50, 50]);
        d[134] = 31;
        d[135] = 0;
        d[144] = 36;
        d[145..155].copy_from_slice(b"INIT VOICE");
        let got = parse(&single_voice_dump(&d)).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].name, "INIT VOICE");
        let get = |id: u32| got[0].values.iter().find(|(p, _)| p.0 == id).unwrap().1;
        assert_eq!(get(id::ALGORITHM), 31.0);
        assert_eq!(get(id::TRANSPOSE), 12.0);
        assert_eq!(
            get(id::op(0, id::LEVEL)),
            99.0,
            "operator 1 is the last block"
        );
        assert_eq!(get(id::op(5, id::LEVEL)), 0.0);
        assert_eq!(get(id::op(0, id::DETUNE)), 0.0);
        // Every parameter of the instrument, once.
        assert_eq!(got[0].values.len(), super::super::parameters().len());
    }
}
