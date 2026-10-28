use super::*;
use crate::pedal::{PedalStage, Stomp, StompCircuit, StompSettings};
use crate::voice::{Pedal, GUITAR_VOLTS, NOMINAL_DBFS};
use std::f64::consts::TAU;

// --- the CD4047 and D5's network, stepped -------------------------------------------

/// The 4047's two drivers as ideal inverters behind `DRIVER`, flipping
/// `PROPAGATION` after the timing node crosses half the supply, into the
/// traced network: R (`timing`) from pin 2 and C14 from pin 1 to the timing
/// node (pin 3, with the 4047's 5 pF input), C13 from there to D5's cathode,
/// C15 from pin 1 to its anode, R14 from the cathode to node A (held at
/// `node_a` by C3, 2.2 µF, far stiffer than anything at the clock), R15 from
/// the anode to the LFO's bias line. Backward Euler, `steps_per_half` steps a
/// half period, D5 (a 1N4148) by Newton. Without `network`, C13 and C15 are
/// left out and the 4047 runs on R and C14 alone.
///
/// What comes back: the clock at the BBDs (the 4047's Q, half its
/// oscillator), and the average current through R14 into node A.
fn transient(timing: f64, node_a: f64, steps_per_half: usize, network: bool) -> (f64, f64) {
    const VDD: f64 = BATTERY;
    const THRESHOLD: f64 = 0.5 * BATTERY;
    const INPUT: f64 = 5e-12;
    let is = DiodeSpec::SILICON.saturation;
    let nvt = DiodeSpec::SILICON.emission * 0.025852;
    let (c13, c15) = if network { (C13, C15) } else { (0.0, 0.0) };
    let half = 1.1 * (timing + DRIVER) * (C14 + 0.25 * (c13 + c15)) + PROPAGATION;
    let h = half / steps_per_half as f64;
    let r_pin2 = timing + DRIVER;

    // pin 1, pin 3, D5's cathode, its anode
    let mut v = [0.0, 0.0, node_a, LFO_BIAS];
    let mut high = true;
    let mut pending: Option<f64> = None;
    let mut t = 0.0;
    let mut flips: Vec<f64> = Vec::new();
    let wanted = 44;
    let (mut charge, mut span) = (0.0, 0.0);
    let mut vd = v[3] - v[2];
    while flips.len() < wanted {
        let (s2, s1) = if high { (VDD, 0.0) } else { (0.0, VDD) };
        let old = v;
        let (g14, g13, g15, gin) = (C14 / h, c13 / h, c15 / h, INPUT / h);
        let mut x = v;
        for _ in 0..40 {
            let e = (vd / nvt).min(80.0).exp();
            let id = is * (e - 1.0);
            let gd = is * e / nvt;
            let ieq = id - gd * vd;
            let mut a = [[0.0f64; 4]; 4];
            let mut r = [0.0f64; 4];
            a[0][0] += 1.0 / DRIVER + g14 + g15;
            a[0][1] -= g14;
            a[0][3] -= g15;
            r[0] += s1 / DRIVER + g14 * (old[0] - old[1]) + g15 * (old[0] - old[3]);
            a[1][1] += 1.0 / r_pin2 + g14 + g13 + gin;
            a[1][0] -= g14;
            a[1][2] -= g13;
            r[1] += s2 / r_pin2 + g14 * (old[1] - old[0]) + g13 * (old[1] - old[2]) + gin * old[1];
            a[2][2] += g13 + 1.0 / R14 + gd;
            a[2][1] -= g13;
            a[2][3] -= gd;
            r[2] += g13 * (old[2] - old[1]) + node_a / R14 + ieq;
            a[3][3] += g15 + 1.0 / R15 + gd;
            a[3][0] -= g15;
            a[3][2] -= gd;
            r[3] += g15 * (old[3] - old[0]) + LFO_BIAS / R15 - ieq;
            x = solve4(a, r);
            let mut next = x[3] - x[2];
            if next - vd > 0.1 {
                next = vd + 0.1;
            }
            let done = (next - vd).abs() < 1e-10;
            vd = next;
            if done {
                break;
            }
        }
        v = x;
        t += h;
        if flips.len() + 20 >= wanted {
            charge += (v[2] - node_a) / R14 * h;
            span += h;
        }
        if pending.is_none() && ((high && v[1] > THRESHOLD) || (!high && v[1] < THRESHOLD)) {
            pending = Some(t + PROPAGATION);
        }
        if pending.is_some_and(|due| t >= due) {
            high = !high;
            pending = None;
            flips.push(t);
        }
    }
    let n = flips.len();
    let halves = (flips[n - 1] - flips[n - 21]) / 20.0;
    (1.0 / (4.0 * halves), charge / span)
}

fn solve4(mut a: [[f64; 4]; 4], mut b: [f64; 4]) -> [f64; 4] {
    for c in 0..4 {
        let p = (c..4)
            .max_by(|&i, &j| a[i][c].abs().total_cmp(&a[j][c].abs()))
            .unwrap();
        a.swap(c, p);
        b.swap(c, p);
        for r in c + 1..4 {
            let f = a[r][c] / a[c][c];
            for k in c..4 {
                a[r][k] -= f * a[c][k];
            }
            b[r] -= f * b[c];
        }
    }
    let mut x = [0.0; 4];
    for r in (0..4).rev() {
        let s: f64 = (r + 1..4).map(|k| a[r][k] * x[k]).sum();
        x[r] = (b[r] - s) / a[r][r];
    }
    x
}

/// Prints `CLOCK_RATIO`, `PUMP_LOW` and `PUMP_HIGH` as source. Run in
/// release with --ignored --nocapture and paste.
#[test]
#[ignore]
fn print_the_clock_tables() {
    let mut ratio = [[0.0; NODE_POINTS]; TIMING_POINTS];
    let mut low = [0.0; TIMING_POINTS];
    let mut high = [0.0; TIMING_POINTS];
    std::thread::scope(|scope| {
        let rows: Vec<_> = (0..TIMING_POINTS)
            .map(|i| {
                scope.spawn(move || {
                    let r = timing_at(i);
                    let mut row = [0.0; NODE_POINTS];
                    for (j, cell) in row.iter_mut().enumerate() {
                        let (hz, _) = transient(r, node_at(j), 2000, true);
                        *cell = hz / plain_clock_hz(r);
                    }
                    let (_, a) = transient(r, PUMP_LOW_V, 2000, true);
                    let (_, b) = transient(r, PUMP_HIGH_V, 2000, true);
                    (row, a, b)
                })
            })
            .collect();
        for (i, row) in rows.into_iter().enumerate() {
            let (r, a, b) = row.join().unwrap();
            ratio[i] = r;
            low[i] = a;
            high[i] = b;
        }
    });
    println!("const CLOCK_RATIO: [[f64; NODE_POINTS]; TIMING_POINTS] = [");
    for row in &ratio {
        let cells: Vec<String> = row.iter().map(|x| format!("{x:.5}")).collect();
        println!("    [{}],", cells.join(", "));
    }
    println!("];");
    let fmt = |r: &[f64]| {
        r.iter()
            .map(|x| format!("{:.3}e-6", x * 1e6))
            .collect::<Vec<_>>()
            .join(", ")
    };
    println!("const PUMP_LOW: [f64; TIMING_POINTS] = [{}];", fmt(&low));
    println!("const PUMP_HIGH: [f64; TIMING_POINTS] = [{}];", fmt(&high));
}

/// The tables are the transient model's, between their points too: where
/// the knob's travel and the triangle put the clock.
#[test]
fn the_clock_law_is_the_transient_models() {
    for (timing, node) in [
        (2_300.0, 4.9),
        (9_000.0, 4.35),
        (31_000.0, 4.1),
        (61_000.0, 3.8),
    ] {
        let (hz, amps) = transient(timing, node, 1000, true);
        let law = clock_hz(timing, node);
        assert!(
            (law / hz - 1.0).abs() < 0.006,
            "{timing} ohm, {node} V: the law says {law:.0} Hz, the model {hz:.0}"
        );
        let (volts, ohms) = pump(timing);
        let pumped = (volts - node) / ohms;
        assert!(
            (pumped - amps).abs() < 4e-6,
            "{timing} ohm, {node} V: the pump {:.1} µA, the model {:.1}",
            pumped * 1e6,
            amps * 1e6
        );
    }
}

/// Without D5's network the transient model is the 4047 sheet's `4.40 R C`
/// with the driver and the delays, so what the tables add is the network's
/// and not the model's. (Within 1.5 %: the formula leaves out the 4047's
/// 5 pF input and the C pin's driver charging C14, which the model steps;
/// they matter only where R is a couple of kilohms.)
#[test]
fn without_its_network_the_clock_is_the_sheets_four_point_four_rc() {
    for timing in [2_000.0, 20_000.0, 61_000.0] {
        let (hz, _) = transient(timing, 4.0, 1000, false);
        let sheet = plain_clock_hz(timing);
        assert!(
            (hz / sheet - 1.0).abs() < 0.015,
            "{timing} ohm: {hz:.0} Hz stepped, {sheet:.0} Hz by the formula"
        );
    }
}

// --- the pedal on a line ------------------------------------------------------------

const SR: f64 = 48_000.0;

/// The line's digital units for a guitar's volts, and back.
fn digital(volts: f64) -> f64 {
    volts / GUITAR_VOLTS * 10f64.powf(NOMINAL_DBFS / 20.0)
}

fn volts_out(x: f64) -> f64 {
    x * GUITAR_VOLTS / 10f64.powf(NOMINAL_DBFS / 20.0)
}

fn knobs(delay: f64, mix: f64, regen: f64, modulation: bool) -> StompSettings {
    StompSettings {
        stomp: Stomp::Pedal(Pedal::CarbonEcho),
        drive: delay,
        level: mix,
        tone: [regen, if modulation { 1.0 } else { 0.0 }, 0.5, 0.5, 0.5],
        ..StompSettings::default()
    }
}

struct Rig {
    stage: PedalStage,
}

impl Rig {
    fn new(s: &StompSettings) -> Self {
        Self::at(s, SR)
    }

    /// Built, at its operating point, and a tenth of a second of silence on:
    /// the switch's fade done and the clock board running, so the delay it
    /// reports is the one it plays.
    fn at(s: &StompSettings, rate: f64) -> Self {
        let mut stage = PedalStage::new(rate, 1);
        let mut circuit = StompCircuit::build(s.stomp, rate, 1).unwrap();
        stage.swap(&mut circuit);
        stage.apply(s);
        stage.reset();
        stage.find_operating_point();
        for _ in 0..(rate * 0.1) as usize {
            stage.process(0.0);
        }
        Self { stage }
    }

    fn run(&mut self, n: usize, input: impl Fn(usize) -> f64) -> Vec<f64> {
        (0..n).map(|k| self.stage.process(input(k))).collect()
    }

    fn echo(&self) -> &Echo {
        self.stage.circuit().unwrap().echo().unwrap()
    }

    fn volts(&self, node: &str) -> f64 {
        let sim = self.stage.circuit().unwrap().simulation();
        sim.voltage_at(unknown(node))
    }
}

fn unknown(node: &str) -> usize {
    build(10_000.0, 470_000.0)
        .unwrap()
        .unknown_named(node)
        .unwrap_or_else(|| panic!("no node {node}"))
}

fn rms(x: &[f64]) -> f64 {
    (x.iter().map(|v| v * v).sum::<f64>() / x.len().max(1) as f64).sqrt()
}

fn db(x: f64) -> f64 {
    20.0 * x.max(1e-12).log10()
}

/// Power at `hz`, averaged over Hann windows of 4096 with half overlap
/// (Goertzel).
fn power_at(x: &[f64], hz: f64, rate: f64) -> f64 {
    let n = 4096;
    let coeff = 2.0 * (TAU * hz / rate).cos();
    let mut total = 0.0;
    let mut count = 0;
    let mut start = 0;
    while start + n <= x.len() {
        let (mut s1, mut s2) = (0.0, 0.0);
        for (k, &v) in x[start..start + n].iter().enumerate() {
            let hann = 0.5 - 0.5 * (TAU * k as f64 / n as f64).cos();
            let s0 = v * hann + coeff * s1 - s2;
            s2 = s1;
            s1 = s0;
        }
        total += s1 * s1 + s2 * s2 - coeff * s1 * s2;
        count += 1;
        start += n / 2;
    }
    total / count.max(1) as f64
}

/// White noise from a fixed seed, `level` volts RMS, as line samples.
fn white(n: usize, level: f64, seed: u64) -> Vec<f64> {
    let mut s = seed | 1;
    (0..n)
        .map(|_| {
            let mut sum = 0.0;
            for _ in 0..4 {
                s ^= s >> 12;
                s ^= s << 25;
                s ^= s >> 27;
                sum += (s.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64 / (1u64 << 53) as f64;
            }
            digital(level * (sum - 2.0) * 3f64.sqrt())
        })
        .collect()
}

/// A plucked note's worth: 330 Hz under a 50 ms decay, at a guitar's peak.
fn pluck(k: usize) -> f64 {
    let t = k as f64 / SR;
    digital(GUITAR_VOLTS * 2.0) * (TAU * 330.0 * t).sin() * (-t / 0.05).exp()
}

/// A note with nothing after it: 30 ms of 440 Hz under a Hann window, at a
/// guitar's level.
const BURST: usize = (0.03 * SR) as usize;
fn burst(k: usize) -> f64 {
    if k < BURST {
        let w = 0.5 - 0.5 * (TAU * k as f64 / BURST as f64).cos();
        digital(GUITAR_VOLTS * 2f64.sqrt()) * w * (TAU * 440.0 * k as f64 / SR).sin()
    } else {
        0.0
    }
}

/// What the wet path alone puts out: the mixer is linear and the dry signal
/// does not pass the MIX pot, so the difference between full and no MIX is
/// the echo.
fn wet(delay: f64, n: usize, input: &dyn Fn(usize) -> f64) -> Vec<f64> {
    let full = Rig::new(&knobs(delay, 1.0, 0.0, false)).run(n, input);
    let none = Rig::new(&knobs(delay, 0.0, 0.0, false)).run(n, input);
    full.iter().zip(&none).map(|(a, b)| a - b).collect()
}

/// The manual's range: 20 ms to 600 ms. The bottom comes out of the trace
/// and the 4047's sheet with nothing set to it; the top is TR3's.
#[test]
fn the_delay_runs_from_twenty_to_six_hundred_milliseconds() {
    let mut last = 0.0;
    let mut range = Vec::new();
    for k in 0..=10 {
        let p = k as f64 / 10.0;
        let mut rig = Rig::new(&knobs(p, 1.0, 0.0, false));
        rig.run(4_800, |_| 0.0);
        let delay = rig.echo().delay_seconds();
        assert!(
            delay > last,
            "DELAY {p}: {:.1} ms after {:.1}",
            delay * 1e3,
            last * 1e3
        );
        last = delay;
        range.push(delay);
    }
    let (shortest, longest) = (range[0], range[10]);
    assert!(
        (0.017..0.021).contains(&shortest),
        "the shortest is {:.1} ms",
        shortest * 1e3
    );
    assert!(
        (longest - 0.6).abs() < 0.006,
        "the longest is {:.1} ms",
        longest * 1e3
    );
}

/// A burst comes back when the clock says and, at full MIX, about as loud
/// as it went in: the manual's "half wet / half dry", which is TR8's.
#[test]
fn an_echo_comes_back_after_the_delay_as_loud_as_the_dry() {
    let (burst, input) = (BURST, burst);
    for p in [0.3, 0.8] {
        let mut rig = Rig::new(&knobs(p, 1.0, 0.0, false));
        let delay = rig.echo().delay_seconds();
        let d = (delay * SR) as usize;
        let out = rig.run(d * 3 / 2 + burst, input);
        let reference: Vec<f64> = (0..burst).map(input).collect();
        let (mut best, mut at) = (0.0, 0);
        for lag in d * 3 / 4..d * 5 / 4 {
            let c: f64 = reference.iter().zip(&out[lag..]).map(|(a, b)| a * b).sum();
            if c > best {
                best = c;
                at = lag;
            }
        }
        let heard = at as f64 / SR;
        assert!(
            (heard - delay).abs() < 1.5e-3,
            "DELAY {p}: the echo at {:.1} ms, the clock's {:.1} ms",
            heard * 1e3,
            delay * 1e3
        );
        let dry = db(rms(&out[..burst]));
        let echo = db(rms(&out[at..at + burst]));
        assert!(
            (echo - dry).abs() < 1.5,
            "DELAY {p}: the echo {echo:.1} dB, the dry {dry:.1} dB"
        );
    }
}

/// MIX from none of the echo to as much of it as of the guitar. Half way is
/// 10 dB down, not 6: a linear 100 k track whose wiper drives R38's 43 k
/// into U8B's summing node, 25 k of source at the middle.
#[test]
fn mix_runs_from_dry_to_half_and_half() {
    let level = |mix: f64| {
        let mut rig = Rig::new(&knobs(0.15, mix, 0.0, false));
        let d = (rig.echo().delay_seconds() * SR) as usize;
        let out = rig.run(d * 2, burst);
        (db(rms(&out[..BURST])), db(rms(&out[d..d + BURST])))
    };
    let (dry, none) = level(0.0);
    assert!(
        none < dry - 50.0,
        "no MIX: the echo {none:.1} dB, the dry {dry:.1}"
    );
    let (_, half) = level(0.5);
    assert!(
        (dry - 11.5..dry - 8.5).contains(&half),
        "half MIX: the echo {half:.1} dB, the dry {dry:.1}"
    );
    let (dry_full, full) = level(1.0);
    assert!((dry_full - dry).abs() < 0.1, "MIX moves the dry signal");
    assert!(
        (full - dry).abs() < 2.0,
        "full MIX: the echo {full:.1} dB, the dry {dry:.1}"
    );
}

/// The repeats of a note, each in dB: the windows where the clock puts them
/// (the stage's latency and the line's are under a tenth of the note).
fn repeats(delay: f64, regen: f64, count: usize) -> Vec<f64> {
    let mut rig = Rig::new(&knobs(delay, 1.0, regen, false));
    let d = (rig.echo().delay_seconds() * SR) as usize;
    let out = rig.run(d * (count + 1), burst);
    (1..=count)
        .map(|k| db(rms(&out[k * d..k * d + BURST])))
        .collect()
}

/// REGEN takes the echo back round: with it low each repeat is well under
/// the last, and at full the loop holds its own -- the compander makes the
/// loop's gain the same at every level, so it runs away until something
/// clips -- as the box does.
#[test]
fn regen_decays_below_the_top_and_sustains_at_it() {
    let low = repeats(0.25, 0.4, 5);
    for w in low.windows(2) {
        assert!(w[1] < w[0] - 6.0, "REGEN 0.4: {low:.1?}");
    }
    let top = repeats(0.25, 1.0, 6);
    assert!(
        top[5] > top[0] - 4.0,
        "REGEN full: the sixth repeat {:.1} dB, the first {:.1} dB",
        top[5],
        top[0]
    );
}

/// And what it runs away to is bounded: the expander's op-amp against its
/// rails, each pass through the filters rounding it, never a number that
/// grows.
#[test]
fn regen_at_full_runs_away_to_a_bounded_level() {
    let mut rig = Rig::new(&knobs(0.05, 1.0, 1.0, false));
    let out = rig.run((SR * 4.0) as usize, pluck);
    assert!(out.iter().all(|x| x.is_finite()));
    let last = &out[out.len() - (SR as usize)..];
    let peak = last.iter().fold(0f64, |m, v| m.max(v.abs()));
    assert!(
        volts_out(peak) < 4.5,
        "{:.2} V at the output",
        volts_out(peak)
    );
    assert!(
        db(rms(last)) > db(rms(&out[..(0.05 * SR) as usize])) - 12.0,
        "it is still sounding: {:.1} dB",
        db(rms(last))
    );
}

/// The clock board on its own at its own rate, its triangle's rate and node
/// A's swing, with SPEED at `speed`.
fn triangle(speed: f64, seconds: f64) -> (f64, f64) {
    triangle_at(CLOCK_BOARD_RATE, speed, seconds)
}

fn triangle_at(rate: f64, speed: f64, seconds: f64) -> (f64, f64) {
    let board = build_clock(rate).unwrap();
    let a = board.unknown_named(NODE_A).unwrap();
    let mut sim = Simulation::new(board, rate);
    sim.set_control(SPEED_TRIM, speed);
    let (volts, ohms) = pump(timing_resistance(0.5));
    sim.set_value(PUMP_RESISTOR, ohms);
    sim.set_aux_input(PUMP_SOURCE, volts);
    let n = (seconds * rate) as usize;
    let trace: Vec<f64> = (0..n)
        .map(|_| {
            sim.set_aux_input(PUMP_SOURCE, volts);
            sim.process(0.0);
            sim.voltage_at(a)
        })
        .collect();
    let settled = &trace[n / 4..];
    let mean = settled.iter().sum::<f64>() / settled.len() as f64;
    let ups: Vec<usize> = settled
        .windows(2)
        .enumerate()
        .filter(|(_, w)| w[0] < mean && w[1] >= mean)
        .map(|(i, _)| i)
        .collect();
    let period = (ups[ups.len() - 1] - ups[0]) as f64 / (ups.len() - 1) as f64 / rate;
    let swing = settled.iter().fold(0f64, |m, v| m.max((v - mean).abs()));
    (1.0 / period, swing)
}

/// MOD: the triangle at TR1's factory setting runs inside the manual's
/// 0.2 to 2.2 Hz, and SPEED's ends reach about its ends.
#[test]
fn the_triangle_runs_at_the_manuals_rates() {
    let (factory, swing) = triangle(TR1_REST, 6.0);
    assert!(
        (1.0..1.6).contains(&factory),
        "{factory:.2} Hz at TR1's rest"
    );
    assert!((0.1..0.25).contains(&swing), "node A swings {swing:.3} V");
    let (slowest, _) = triangle(0.0, 24.0);
    assert!(
        (0.15..0.3).contains(&slowest),
        "{slowest:.2} Hz with SPEED down"
    );
    let (fastest, _) = triangle(1.0, 3.0);
    assert!(
        (2.0..4.0).contains(&fastest),
        "{fastest:.2} Hz with SPEED up"
    );
}

/// MOD sweeps the clock by about a per cent, which is more pitch the longer
/// the delay; off, it holds still.
#[test]
fn the_mod_switch_sweeps_the_clock_and_holds_it_off() {
    let sweep = |modulation: bool| {
        let mut rig = Rig::new(&knobs(0.5, 1.0, 0.0, modulation));
        rig.run((SR * 0.5) as usize, |_| 0.0);
        let (mut lo, mut hi) = (f64::MAX, 0f64);
        for _ in 0..(SR * 1.6) as usize {
            rig.stage.process(0.0);
            lo = lo.min(rig.echo().clock_hz());
            hi = hi.max(rig.echo().clock_hz());
        }
        (hi - lo) / (hi + lo)
    };
    let on = sweep(true);
    assert!(
        (0.003..0.012).contains(&on),
        "±{:.2} % with MOD on",
        on * 100.0
    );
    let off = sweep(false);
    assert!(off < 1e-4, "±{:.4} % with MOD off", off * 100.0);
}

/// The wet path's response to white noise at both ends of DELAY, against
/// the write-up's measurement of a real M169 (read off its plot, against
/// 100 Hz): the filters' peak at 1.5 kHz with the clock high, and the long
/// delay's slow clock taking the top away.
#[test]
fn the_wet_response_is_the_measured_units() {
    let noise = white((SR * 2.5) as usize, 0.1, 7);
    let input = |k: usize| noise[k];
    for (delay, measured) in [
        (
            0.0,
            [(1000.0, 3.3), (1500.0, 5.0), (2000.0, 2.3), (3000.0, -10.3)],
        ),
        (
            1.0,
            [
                (1000.0, -1.3),
                (1500.0, -3.4),
                (2000.0, -9.2),
                (3000.0, -24.2),
            ],
        ),
    ] {
        let wet = wet(delay, noise.len(), &input);
        let wet = &wet[(SR * 0.7) as usize..];
        let reference = power_at(wet, 100.0, SR);
        for (hz, want) in measured {
            let got = 10.0 * (power_at(wet, hz, SR) / reference).log10();
            assert!(
                (got - want).abs() < 4.5,
                "DELAY {delay}, {hz} Hz: {got:.1} dB re 100 Hz, measured {want:.1}"
            );
        }
    }
}

/// THD of a steady stretch, harmonics 2 to 5.
fn thd(x: &[f64], hz: f64) -> f64 {
    let h: Vec<f64> = (1..=5)
        .map(|k| power_at(x, hz * k as f64, SR).sqrt())
        .collect();
    h[1..].iter().map(|v| v * v).sum::<f64>().sqrt() / h[0]
}

/// At the manual's test point, 1 kHz at -5 dBV, the four BBDs add under
/// half a per cent between their input and their output. The wet path as a
/// whole is rated under 1 % there and this model is not: the Sallen-Key
/// followers either side (Q7, Q14, Q9: emitter followers on 10 k, in loops
/// whose capacitors stand 250 and 120 to 1) cannot quite keep up with a
/// kilohertz at a volt and more, and their second harmonic falls on the
/// filters' peak. At a guitar's level and pitch the echo is clean.
#[test]
fn the_filters_followers_are_where_a_hot_echo_frays() {
    let amp = digital(10f64.powf(-5.0 / 20.0) * 2f64.sqrt());
    let n = (SR * 1.5) as usize;
    let mut rig = Rig::new(&knobs(0.5, 1.0, 0.0, false));
    let (mut sent, mut back) = (Vec::new(), Vec::new());
    for k in 0..n {
        rig.stage
            .process(amp * (TAU * 1000.0 * k as f64 / SR).sin());
        if k >= n / 2 {
            sent.push(rig.volts(BBD_IN));
            back.push(rig.volts("tp10"));
        }
    }
    let centre = |x: &mut Vec<f64>| {
        let mean = x.iter().sum::<f64>() / x.len() as f64;
        x.iter_mut().for_each(|v| *v -= mean);
    };
    centre(&mut sent);
    centre(&mut back);
    let (into, out_of) = (thd(&sent, 1000.0), thd(&back, 1000.0));
    assert!(
        out_of - into < 0.005,
        "the BBDs take {:.2} % to {:.2} %",
        into * 100.0,
        out_of * 100.0
    );
    let nominal = digital(GUITAR_VOLTS * 2f64.sqrt());
    let echo = wet(0.5, n, &|k| nominal * (TAU * 220.0 * k as f64 / SR).sin());
    let clean = thd(&echo[n / 2..], 220.0);
    assert!(
        clean < 0.02,
        "a guitar's 220 Hz echoes with {:.2} % THD",
        clean * 100.0
    );
}

/// With nothing playing the expander shuts the BBDs' hiss and clock away.
#[test]
fn nothing_in_is_nearly_nothing_out() {
    for p in [0.0, 1.0] {
        let out = Rig::new(&knobs(p, 1.0, 0.0, false)).run((SR * 1.0) as usize, |_| 0.0);
        let tail: Vec<f64> = out[out.len() / 2..].iter().map(|&v| volts_out(v)).collect();
        assert!(
            db(rms(&tail)) < -110.0,
            "DELAY {p}: {:.1} dBV at rest",
            db(rms(&tail))
        );
    }
}

/// The delay is the clock's, whatever the rate the stage runs at.
#[test]
fn the_delay_is_the_same_at_every_rate() {
    for rate in [44_100.0, 96_000.0] {
        let mut rig = Rig::at(&knobs(0.4, 1.0, 0.0, false), rate);
        rig.run((rate * 0.1) as usize, |_| 0.0);
        let mut here = Rig::new(&knobs(0.4, 1.0, 0.0, false));
        here.run(4_800, |_| 0.0);
        let (a, b) = (rig.echo().delay_seconds(), here.echo().delay_seconds());
        assert!(
            (a / b - 1.0).abs() < 0.003,
            "{rate} Hz: {a} s against {b} s"
        );
    }
}

/// What each part costs one channel at 48 kHz, as a share of real time: the
/// whole stage, the clock board alone at its own rate, the line alone; and
/// the Blue Chorus beside it. Run in release with --ignored --nocapture,
/// nothing else running.
#[test]
#[ignore]
fn cost_by_part() {
    use std::time::Instant;
    let seconds = 3.0;
    let n = (SR * seconds) as usize;
    let a = digital(GUITAR_VOLTS * 1.414);
    let input: Vec<f64> = (0..n)
        .map(|k| {
            let t = k as f64 / SR;
            a * (TAU * 196.0 * t).sin() * (-3.0 * (t % 1.0)).exp()
        })
        .collect();
    let share = |secs: f64| 100.0 * secs / seconds;
    for s in [
        knobs(0.5, 0.6, 0.4, true),
        knobs(1.0, 0.6, 0.4, true),
        knobs(0.0, 0.6, 0.4, true),
    ] {
        let mut rig = Rig::new(&s);
        let p0 = rig.stage.circuit().unwrap().simulation().newton_passes();
        let t = Instant::now();
        for &x in &input {
            std::hint::black_box(rig.stage.process(x));
        }
        let secs = t.elapsed().as_secs_f64();
        let sim = rig.stage.circuit().unwrap().simulation();
        println!(
            "stage, DELAY {:.1}: {:5.1} %, {:.2} passes a sample, {:?}",
            s.drive,
            share(secs),
            (sim.newton_passes() - p0) as f64 / n as f64,
            sim.statistics()
        );
    }
    let mut clock = Simulation::new(build_clock(CLOCK_BOARD_RATE).unwrap(), CLOCK_BOARD_RATE);
    clock.set_aux_input(PUMP_SOURCE, pump(timing_resistance(0.5)).0);
    clock.find_operating_point();
    let steps = (CLOCK_BOARD_RATE * seconds) as usize;
    let t = Instant::now();
    for _ in 0..steps {
        std::hint::black_box(clock.process(0.0));
    }
    println!(
        "clock board alone: {:5.1} %, {:.2} passes a step",
        share(t.elapsed().as_secs_f64()),
        clock.newton_passes() as f64 / steps as f64
    );
    let mut line = Clocked::new(LINE, 1);
    for hz in [6_800.0, 11_400.0, 220_000.0] {
        let t = Instant::now();
        for &x in &input {
            std::hint::black_box(line.process(x, hz, SR));
        }
        println!(
            "line alone at {hz} Hz: {:5.1} %",
            share(t.elapsed().as_secs_f64())
        );
    }
    let chorus = StompSettings {
        stomp: Stomp::Pedal(Pedal::BlueChorus),
        drive: 0.5,
        ..StompSettings::default()
    };
    let mut rig = Rig::new(&chorus);
    let t = Instant::now();
    for &x in &input {
        std::hint::black_box(rig.stage.process(x));
    }
    println!(
        "Blue Chorus stage: {:5.1} %",
        share(t.elapsed().as_secs_f64())
    );
}

/// The numbers the module's documentation quotes. Run with --ignored
/// --nocapture.
#[test]
#[ignore]
fn measurements() {
    for k in 0..=10 {
        let p = k as f64 / 10.0;
        let mut rig = Rig::new(&knobs(p, 1.0, 0.0, false));
        rig.run(4_800, |_| 0.0);
        println!(
            "DELAY {p:.1}: {:6.1} ms, clock {:6.0} Hz, node A {:.3} V",
            rig.echo().delay_seconds() * 1e3,
            rig.echo().clock_hz(),
            rig.echo().node_a()
        );
    }
    for delay in [0.1, 0.25, 0.6, 1.0] {
        for regen in [0.5, 0.6, 0.7, 0.8, 0.9, 1.0] {
            let r = repeats(delay, regen, 8);
            println!(
                "DELAY {delay}, REGEN {regen}: {:?}",
                r.iter()
                    .map(|v| (v * 10.0).round() / 10.0)
                    .collect::<Vec<_>>()
            );
        }
    }
    for speed in [0.0, 0.25, 0.5, 0.75, 1.0] {
        let seconds = if speed < 0.2 { 24.0 } else { 8.0 };
        let (hz, swing) = triangle(speed, seconds);
        let (full, _) = triangle_at(48_000.0, speed, seconds);
        println!("SPEED {speed}: {hz:.3} Hz, node A ±{swing:.3} V; stepped at 48 kHz {full:.3} Hz");
    }
    for p in [0.0, 0.5] {
        for dbv in [-20.0, -10.0, -5.0] {
            let amp = digital(10f64.powf(dbv / 20.0) * 2f64.sqrt());
            let n = (SR * 1.5) as usize;
            for hz in [220.0, 1000.0] {
                let w = wet(p, n, &|k| amp * (TAU * hz * k as f64 / SR).sin());
                println!(
                    "DELAY {p}, {dbv} dBV, {hz} Hz: wet THD {:.2} %",
                    thd(&w[n / 2..], hz) * 100.0
                );
            }
        }
    }
    let noise = white((SR * 2.5) as usize, 0.1, 7);
    for delay in [0.0, 0.5, 1.0] {
        let w = wet(delay, noise.len(), &|k| noise[k]);
        let w = &w[(SR * 0.7) as usize..];
        let reference = power_at(w, 100.0, SR);
        let row: Vec<String> = [
            500.0, 1000.0, 1500.0, 2000.0, 2500.0, 3000.0, 4000.0, 5000.0, 6000.0,
        ]
        .iter()
        .map(|&f| {
            format!(
                "{f}: {:.1}",
                10.0 * (power_at(w, f, SR) / reference).log10()
            )
        })
        .collect();
        println!("DELAY {delay}: wet re 100 Hz: {}", row.join(", "));
    }
}
