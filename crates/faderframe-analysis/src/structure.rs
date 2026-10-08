//! Song structure: where a recording's parts begin and end, which parts
//! are the same, and what they likely are — the sections of a jam.
//!
//! The recording is cut into bars by its tempo ([`crate::tempo`]; two
//! seconds a unit without a clear beat). Each bar is described by its
//! harmony (a chroma vector of the averaged spectrum), its timbre (the
//! shape of 20 logarithmic band energies, level taken out) and its
//! loudness. Bars compared with every other give a self-similarity matrix,
//! smoothed along its diagonals so repeated sequences stand out. Part
//! boundaries are where a checkerboard kernel along the diagonal peaks —
//! the bars before alike, the bars after alike, the two unlike — with
//! loudness changes counting too and four-bar phrases preferred; silence
//! (a pause in a jam) is a gap. Parts are then compared along their
//! diagonals and the alike given one letter; the letters are named by
//! repetition, loudness and place: the loudest repeated part the Chorus,
//! another repeated one the Verse, a part only once at the start the
//! Intro, at the end the Outro, in the middle a Bridge (or a Solo when it
//! is the loudest of all).

use crate::tempo::{self, Tempo};

const ANALYSIS_RATE: f64 = 11_025.0;
const FRAME: usize = 2048;
const HOP: usize = 1024;
/// Bands of the timbre vector (50 Hz – 5 kHz).
const BANDS: usize = 20;
/// The checkerboard kernel's half size (units).
const KERNEL: usize = 4;
/// Shortest part (units).
const SHORTEST: usize = 3;
/// A unit quieter than this (dBFS RMS, or this far under the loudest) is
/// silence.
const SILENCE_DB: f64 = -60.0;
const SILENCE_BELOW_LOUDEST_DB: f64 = 45.0;

/// One part of a recording.
#[derive(Clone, Debug, PartialEq)]
pub struct Part {
    /// Seconds from the recording's start.
    pub start: f64,
    pub end: f64,
    /// Parts alike share a letter (0 = A).
    pub label: usize,
    /// What it likely is ("Verse 2", "Chorus", "Intro" …).
    pub name: String,
    /// RMS level (dBFS).
    pub loudness: f64,
}

/// A recording's structure.
#[derive(Clone, Debug, PartialEq)]
pub struct Structure {
    pub parts: Vec<Part>,
    /// The tempo the bars came from (`None`: two-second units).
    pub tempo: Option<Tempo>,
    /// Seconds a unit (a bar) and where the first one starts.
    pub unit: f64,
    pub origin: f64,
}

/// Features of one unit.
struct Unit {
    start: f64,
    end: f64,
    chroma: [f64; 12],
    timbre: [f64; BANDS],
    loudness: f64,
    silent: bool,
}

fn normalise(v: &mut [f64]) {
    let n = v.iter().map(|x| x * x).sum::<f64>().sqrt();
    if n > 1e-12 {
        for x in v {
            *x /= n;
        }
    }
}

fn dot(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// The structure of mono `x` at `rate`, bars `quarters_per_bar` quarter
/// notes long (4 for 4/4, 3 for 6/8).
pub fn analyse(x: &[f32], rate: f64, quarters_per_bar: f64) -> Structure {
    let seconds = x.len() as f64 / rate.max(1.0);
    let found = tempo::detect(x, rate).filter(|t| t.confidence >= 0.1);
    let (mut unit, origin) = match found {
        Some(t) => (60.0 / t.bpm * quarters_per_bar.max(0.5), t.first_beat),
        None => (2.0, 0.0),
    };
    // Fast tempos: two bars a unit.
    while unit < 1.2 {
        unit *= 2.0;
    }
    // The downbeat: of the beats the tempo found, the one whose bar lines
    // fall where the harmony changes most.
    let (origin, units) = match found {
        Some(t) => {
            let beat = 60.0 / t.bpm;
            let per_unit = (unit / beat).round().max(1.0) as usize;
            (0..per_unit)
                .map(|k| {
                    let o = origin + k as f64 * beat;
                    let u = describe(x, rate, unit, o, seconds);
                    let change = u
                        .windows(2)
                        .filter(|w| !w[0].silent && !w[1].silent)
                        .map(|w| 1.0 - dot(&w[0].chroma, &w[1].chroma))
                        .sum::<f64>()
                        / u.len().max(1) as f64;
                    (change, o, u)
                })
                .max_by(|a, b| a.0.total_cmp(&b.0))
                .map(|(_, o, u)| (o, u))
                .unwrap_or_else(|| (origin, describe(x, rate, unit, origin, seconds)))
        }
        None => (origin, describe(x, rate, unit, origin, seconds)),
    };
    if units.len() < 2 * SHORTEST {
        return Structure {
            parts: whole(&units, seconds),
            tempo: found,
            unit,
            origin,
        };
    }
    let ssm = similarity(&units);
    let bounds = boundaries(&units);
    let parts = label(&units, &ssm, &bounds);
    Structure {
        parts,
        tempo: found,
        unit,
        origin,
    }
}

/// The whole recording as one part (too short to tell its parts).
fn whole(units: &[Unit], seconds: f64) -> Vec<Part> {
    let loud: Vec<&Unit> = units.iter().filter(|u| !u.silent).collect();
    if loud.is_empty() {
        return Vec::new();
    }
    vec![Part {
        start: loud[0].start,
        end: loud.last().map_or(seconds, |u| u.end),
        label: 0,
        name: "Theme".into(),
        loudness: loud.iter().map(|u| u.loudness).sum::<f64>() / loud.len() as f64,
    }]
}

/// Cut into units and describe each.
fn describe(x: &[f32], rate: f64, unit: f64, origin: f64, seconds: f64) -> Vec<Unit> {
    let d = ((rate / ANALYSIS_RATE).floor() as usize).max(1);
    let low = crate::melody::decimate(x, d);
    let low_rate = rate / d as f64;
    let window: Vec<f64> = (0..FRAME)
        .map(|i| 0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / FRAME as f64).cos())
        .collect();
    // Each bin's pitch class (and weight) and band.
    let bin_hz = |b: usize| b as f64 * low_rate / FRAME as f64;
    let classes: Vec<Option<(usize, f64)>> = (0..FRAME / 2)
        .map(|b| {
            let f = bin_hz(b);
            if !(65.0..=2100.0).contains(&f) {
                return None;
            }
            let note = 69.0 + 12.0 * (f / 440.0).log2();
            let nearest = note.round();
            let off = (note - nearest).abs();
            (off < 0.5).then(|| ((nearest as i64).rem_euclid(12) as usize, 1.0 - 2.0 * off))
        })
        .collect();
    let (lo, hi) = (50f64.ln(), 5000f64.ln());
    let bands: Vec<Option<usize>> = (0..FRAME / 2)
        .map(|b| {
            let f = bin_hz(b);
            if !(50.0..5000.0).contains(&f) {
                return None;
            }
            Some((((f.ln() - lo) / (hi - lo)) * BANDS as f64).min(BANDS as f64 - 1.0) as usize)
        })
        .collect();
    // Unit edges: from the first beat back to the start, on to the end.
    let mut edges = Vec::new();
    let mut t = origin.rem_euclid(unit);
    if t > unit * 0.5 {
        edges.push(0.0);
    }
    if t < 1e-6 {
        t = 0.0;
    }
    while t < seconds - unit * 0.5 {
        edges.push(t);
        t += unit;
    }
    if edges.first().is_none_or(|e| *e > 0.0) {
        edges.insert(0, 0.0);
    }
    edges.push(seconds);
    let mut re = vec![0.0f64; FRAME];
    let mut im = vec![0.0f64; FRAME];
    let mut units = Vec::with_capacity(edges.len());
    for w in edges.windows(2) {
        let (a, b) = (w[0], w[1]);
        if b - a < 1e-3 {
            continue;
        }
        let (la, lb) = (
            ((a * low_rate) as usize).min(low.len()),
            ((b * low_rate) as usize).min(low.len()),
        );
        let mut spectrum = vec![0.0f64; FRAME / 2];
        let mut frames = 0;
        let mut f = la;
        while f + FRAME <= lb.max(la + FRAME).min(low.len()) {
            for i in 0..FRAME {
                re[i] = f64::from(low[f + i]) * window[i];
                im[i] = 0.0;
            }
            crate::fft(&mut re, &mut im);
            for (k, s) in spectrum.iter_mut().enumerate() {
                *s += (re[k] * re[k] + im[k] * im[k]).sqrt();
            }
            frames += 1;
            f += HOP;
            if f + FRAME > lb && frames > 0 {
                break;
            }
        }
        let mut chroma = [0.0f64; 12];
        let mut timbre = [1e-9f64; BANDS];
        if frames > 0 {
            for (k, s) in spectrum.iter().enumerate() {
                let m = s / frames as f64;
                if let Some((c, w)) = classes[k] {
                    chroma[c] += m.sqrt() * w;
                }
                if let Some(band) = bands[k] {
                    timbre[band] += m * m;
                }
            }
        }
        normalise(&mut chroma);
        let mut t: Vec<f64> = timbre.iter().map(|e| e.log10()).collect();
        let mean = t.iter().sum::<f64>() / BANDS as f64;
        for v in &mut t {
            *v -= mean;
        }
        normalise(&mut t);
        let mut timbre = [0.0f64; BANDS];
        timbre.copy_from_slice(&t);
        let (sa, sb) = (
            ((a * rate) as usize).min(x.len()),
            ((b * rate) as usize).min(x.len()),
        );
        let n = (sb - sa).max(1);
        let power = x[sa..sb]
            .iter()
            .map(|v| f64::from(*v) * f64::from(*v))
            .sum::<f64>()
            / n as f64;
        units.push(Unit {
            start: a,
            end: b,
            chroma,
            timbre,
            loudness: 10.0 * power.max(1e-14).log10(),
            silent: false,
        });
    }
    let loudest = units.iter().map(|u| u.loudness).fold(f64::MIN, f64::max);
    for u in &mut units {
        u.silent = u.loudness < SILENCE_DB || u.loudness < loudest - SILENCE_BELOW_LOUDEST_DB;
    }
    units
}

/// The self-similarity of the units (harmony and timbre), smoothed along
/// the diagonals over two units.
fn similarity(units: &[Unit]) -> Vec<Vec<f64>> {
    let n = units.len();
    let mut raw = vec![vec![0.0f64; n]; n];
    for i in 0..n {
        for j in i..n {
            let s = if units[i].silent || units[j].silent {
                0.0
            } else {
                0.6 * dot(&units[i].chroma, &units[j].chroma)
                    + 0.4 * dot(&units[i].timbre, &units[j].timbre)
            };
            raw[i][j] = s;
            raw[j][i] = s;
        }
    }
    let mut out = vec![vec![0.0f64; n]; n];
    for i in 0..n {
        for j in 0..n {
            let next = if i + 1 < n && j + 1 < n {
                raw[i + 1][j + 1]
            } else {
                raw[i][j]
            };
            out[i][j] = 0.5 * (raw[i][j] + next);
        }
    }
    out
}

/// Unit indices where parts begin (besides 0).
///
/// The novelty at a unit: how unlike the phrase before it (the mean
/// harmony and timbre of [`KERNEL`] units) is the phrase after it — means,
/// so a chord cycle inside a part does not count as change — plus the
/// change of loudness.
fn boundaries(units: &[Unit]) -> Vec<usize> {
    let n = units.len();
    let w = KERNEL;
    let mean_of = |a: usize, b: usize| -> Option<Vec<f64>> {
        let loud: Vec<&Unit> = units[a..b].iter().filter(|u| !u.silent).collect();
        if loud.is_empty() {
            return None;
        }
        let mut c = vec![0.0f64; 12];
        let mut t = vec![0.0f64; BANDS];
        for u in &loud {
            for (x, y) in c.iter_mut().zip(&u.chroma) {
                *x += y;
            }
            for (x, y) in t.iter_mut().zip(&u.timbre) {
                *x += y;
            }
        }
        normalise(&mut c);
        normalise(&mut t);
        // Harmony and timbre weighed as in the similarity.
        Some(
            c.iter()
                .map(|v| v * 0.6f64.sqrt())
                .chain(t.iter().map(|v| v * 0.4f64.sqrt()))
                .collect(),
        )
    };
    let mut novelty = vec![0.0f64; n];
    for (i, nv) in novelty.iter_mut().enumerate().skip(1) {
        let (a, b) = (i.saturating_sub(w), (i + w).min(n));
        *nv = match (mean_of(a, i), mean_of(i, b)) {
            (Some(x), Some(y)) => 1.0 - dot(&x, &y),
            _ => 0.0,
        };
    }
    // As z-scores (a recording in one key is alike everywhere; what
    // matters is more or less alike).
    let mu = novelty.iter().sum::<f64>() / n as f64;
    let sd = (novelty.iter().map(|v| (v - mu).powi(2)).sum::<f64>() / n as f64)
        .sqrt()
        .max(1e-9);
    for v in &mut novelty {
        *v = (*v - mu) / sd;
    }
    // Loudness changes count too (a drop, a break).
    let level = |a: usize, b: usize| -> f64 {
        let u = &units[a..b];
        u.iter().map(|u| u.loudness).sum::<f64>() / u.len().max(1) as f64
    };
    for (i, nv) in novelty
        .iter_mut()
        .enumerate()
        .take(n.saturating_sub(2))
        .skip(2)
    {
        let before = level(i.saturating_sub(2), i);
        let after = level(i, (i + 2).min(n));
        *nv += 0.15 * (after - before).abs().min(15.0);
    }
    let mean = novelty.iter().sum::<f64>() / n as f64;
    let std = (novelty.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n as f64).sqrt();
    let first = units.iter().position(|u| !u.silent).unwrap_or(0);
    let mut candidates: Vec<(f64, usize)> = (1..n)
        .filter(|&i| {
            let a = i.saturating_sub(2);
            let b = (i + 3).min(n);
            novelty[a..b].iter().all(|v| *v <= novelty[i]) && novelty[i] > mean + 0.4 * std
        })
        .map(|i| {
            // Four-bar phrases from where the music starts.
            let k = i.saturating_sub(first);
            let bonus = if k % 4 == 0 {
                1.15
            } else if k % 2 == 0 {
                1.07
            } else {
                1.0
            };
            (novelty[i] * bonus, i)
        })
        .collect();
    candidates.sort_by(|a, b| b.0.total_cmp(&a.0));
    let mut chosen: Vec<usize> = Vec::new();
    // Where the music ends (silence after it is a gap of its own).
    let end = units.iter().rposition(|u| !u.silent).map_or(n, |e| e + 1);
    for (_, i) in candidates {
        // No part shorter than the shortest at either end.
        if i < first + SHORTEST || i + SHORTEST > end {
            continue;
        }
        if chosen
            .iter()
            .all(|c| (*c as i64 - i as i64).unsigned_abs() as usize >= SHORTEST)
        {
            chosen.push(i);
        }
    }
    // Silence begins and ends parts.
    for i in 1..n {
        if units[i].silent != units[i - 1].silent && !chosen.contains(&i) {
            chosen.retain(|c| (*c as i64 - i as i64).unsigned_abs() >= 2);
            chosen.push(i);
        }
    }
    chosen.sort_unstable();
    chosen
}

/// Label and name the parts between `bounds`.
fn label(units: &[Unit], ssm: &[Vec<f64>], bounds: &[usize]) -> Vec<Part> {
    let n = units.len();
    let mut edges = vec![0];
    edges.extend(bounds.iter().copied().filter(|b| *b > 0 && *b < n));
    edges.push(n);
    // Segments (start, end) of units, silent ones dropped.
    let segs: Vec<(usize, usize)> = edges
        .windows(2)
        .map(|w| (w[0], w[1]))
        .filter(|(a, b)| {
            let silent = units[*a..*b].iter().filter(|u| u.silent).count();
            silent * 2 < b - a
        })
        .collect();
    if segs.is_empty() {
        return Vec::new();
    }
    // Two parts alike: the mean similarity along their diagonal, at the
    // best of three alignments; parts of very different lengths a little
    // less alike.
    let alike = |p: (usize, usize), q: (usize, usize)| -> f64 {
        let len = (p.1 - p.0).min(q.1 - q.0);
        let mut best = f64::MIN;
        for shift in -1i64..=1 {
            let mut sum = 0.0;
            let mut count = 0;
            for k in 0..len {
                let (i, j) = (p.0 + k, q.0 as i64 + k as i64 + shift);
                if j < q.0 as i64 || j >= q.1 as i64 {
                    continue;
                }
                sum += ssm[i][j as usize];
                count += 1;
            }
            if count > 0 {
                best = best.max(sum / count as f64);
            }
        }
        let (lp, lq) = ((p.1 - p.0) as f64, (q.1 - q.0) as f64);
        if lp.max(lq) > 1.5 * lp.min(lq) {
            best * 0.92
        } else {
            best
        }
    };
    let mut pairs = Vec::new();
    for i in 0..segs.len() {
        for j in i + 1..segs.len() {
            pairs.push(alike(segs[i], segs[j]));
        }
    }
    let threshold = if pairs.is_empty() {
        1.0
    } else {
        let mean = pairs.iter().sum::<f64>() / pairs.len() as f64;
        let std =
            (pairs.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / pairs.len() as f64).sqrt();
        (mean + 0.6 * std).clamp(0.6, 0.97)
    };
    let mut labels: Vec<usize> = Vec::with_capacity(segs.len());
    let mut count = 0;
    for i in 0..segs.len() {
        let best = (0..i)
            .map(|j| (alike(segs[j], segs[i]), labels[j]))
            .max_by(|a, b| a.0.total_cmp(&b.0));
        match best {
            Some((s, l)) if s >= threshold => labels.push(l),
            _ => {
                labels.push(count);
                count += 1;
            }
        }
    }
    // Neighbours with one letter are one part.
    let mut merged: Vec<((usize, usize), usize)> = Vec::new();
    for (seg, l) in segs.iter().zip(&labels) {
        match merged.last_mut() {
            Some((s, ml)) if *ml == *l && s.1 == seg.0 => s.1 = seg.1,
            _ => merged.push((*seg, *l)),
        }
    }
    let loudness = |s: (usize, usize)| {
        let u = &units[s.0..s.1];
        10.0 * (u.iter().map(|u| 10f64.powf(u.loudness / 10.0)).sum::<f64>() / u.len() as f64)
            .max(1e-14)
            .log10()
    };
    // Names by repetition, loudness and place.
    let letters = count;
    let mut times = vec![0usize; letters];
    let mut level = vec![0.0f64; letters];
    for (s, l) in &merged {
        times[*l] += 1;
        level[*l] += loudness(*s);
    }
    for l in 0..letters {
        if times[l] > 0 {
            level[l] /= times[l] as f64;
        }
    }
    // Repeated: twice or more, once at least between the first and the
    // last part (a letter only opening and closing is Intro and Outro).
    let last = merged.len() - 1;
    let inside = |l: usize| {
        merged
            .iter()
            .enumerate()
            .any(|(i, (_, m))| *m == l && i > 0 && i < last)
    };
    let is_repeated = |l: usize| times[l] > 1 && inside(l);
    let mut repeated: Vec<usize> = (0..letters).filter(|l| is_repeated(*l)).collect();
    repeated.sort_by(|a, b| level[*b].total_cmp(&level[*a]));
    let loudest = merged
        .iter()
        .map(|(s, _)| loudness(*s))
        .fold(f64::MIN, f64::max);
    let mut role: Vec<String> = vec![String::new(); letters];
    match repeated.len() {
        0 => {}
        1 => role[repeated[0]] = "Theme".into(),
        _ => {
            role[repeated[0]] = "Chorus".into();
            role[repeated[1]] = "Verse".into();
            for (k, l) in repeated.iter().enumerate().skip(2) {
                role[*l] = if k == 2 {
                    "Pre-Chorus".into()
                } else {
                    format!("Part {}", (b'A' + *l as u8) as char)
                };
            }
        }
    }
    let first_repeat = merged.iter().position(|(_, l)| is_repeated(*l));
    let last_repeat = merged.iter().rposition(|(_, l)| is_repeated(*l));
    let mut used = vec![0usize; letters];
    let mut parts = Vec::with_capacity(merged.len());
    for (i, (s, l)) in merged.iter().enumerate() {
        let level = loudness(*s);
        let base = if is_repeated(*l) {
            role[*l].clone()
        } else if first_repeat.is_none_or(|f| i < f) && i == 0 {
            "Intro".into()
        } else if last_repeat.is_none_or(|r| i > r) && i + 1 == merged.len() {
            "Outro".into()
        } else if level >= loudest - 0.5 && repeated.len() > 1 {
            "Solo".into()
        } else {
            "Bridge".into()
        };
        used[*l] += 1;
        let name = if is_repeated(*l) {
            format!("{base} {}", used[*l])
        } else {
            base
        };
        parts.push(Part {
            start: units[s.0].start,
            end: units[s.1 - 1].end,
            label: *l,
            name,
            loudness: level,
        });
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f64 = 44_100.0;
    /// 120 BPM, 4/4: two seconds a bar.
    const BAR: f64 = 2.0;

    struct Noise(u32);

    impl Noise {
        fn next(&mut self) -> f32 {
            self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (self.0 >> 8) as f32 / (1 << 24) as f32 * 2.0 - 1.0
        }
    }

    fn hz(midi: i32) -> f64 {
        440.0 * 2f64.powf((f64::from(midi) - 69.0) / 12.0)
    }

    /// One part: `bars` bars cycling through `chords` (a bar each), a
    /// voice with `harmonics` partials, drums or not, at `gain`.
    #[allow(clippy::too_many_arguments)]
    fn part(
        out: &mut Vec<f32>,
        bars: usize,
        chords: &[[i32; 3]],
        harmonics: usize,
        drums: bool,
        hats: bool,
        gain: f32,
        noise: &mut Noise,
    ) {
        let bar = (BAR * SR) as usize;
        let beat = bar / 4;
        for b in 0..bars {
            let chord = chords[b % chords.len()];
            for i in 0..bar {
                let t = (out.len()) as f64 / SR;
                let in_beat = (i % beat) as f64 / SR;
                let mut v = 0.0f64;
                for &note in &chord {
                    for h in 1..=harmonics {
                        v += (std::f64::consts::TAU * hz(note) * h as f64 * t).sin() / h as f64;
                    }
                }
                v *= 0.08 * (0.6 + 0.4 * (-in_beat * 3.0).exp());
                // Bass: the chord's root two octaves down.
                v += 0.15 * (std::f64::consts::TAU * hz(chord[0] - 24) * t).sin();
                if drums {
                    let k = i % (beat * 2);
                    if k < (0.12 * SR) as usize {
                        let kt = k as f64 / SR;
                        v += 0.5
                            * (std::f64::consts::TAU * (50.0 + 70.0 * (-kt * 30.0).exp()) * kt)
                                .sin()
                            * (-kt * 18.0).exp();
                    }
                    let s = (i + beat) % (beat * 2);
                    if s < (0.1 * SR) as usize {
                        v += 0.25 * f64::from(noise.next()) * (-(s as f64 / SR) * 30.0).exp();
                    }
                }
                if hats {
                    let s = i % (beat / 2);
                    if s < (0.03 * SR) as usize {
                        v += 0.12 * f64::from(noise.next()) * (-(s as f64 / SR) * 120.0).exp();
                    }
                }
                out.push((v as f32) * gain);
            }
        }
    }

    /// Intro, Verse, Chorus, Verse, Chorus, Bridge, Chorus, Outro (bars:
    /// 4 8 8 8 8 4 8 4).
    fn song() -> (Vec<f32>, Vec<f64>) {
        let mut x = Vec::new();
        let mut noise = Noise(7);
        let am = [57, 60, 64];
        let f = [53, 57, 60];
        let c = [48, 52, 55];
        let g = [55, 59, 62];
        let dm = [50, 53, 57];
        let em = [52, 55, 59];
        let verse = [am, f, c, g];
        let chorus = [f, g, c, am];
        part(&mut x, 4, &[am], 1, false, false, 0.5, &mut noise);
        for _ in 0..2 {
            part(&mut x, 8, &verse, 2, true, false, 0.8, &mut noise);
            part(&mut x, 8, &chorus, 5, true, true, 1.0, &mut noise);
        }
        part(&mut x, 4, &[dm, em], 3, false, true, 0.7, &mut noise);
        part(&mut x, 8, &chorus, 5, true, true, 1.0, &mut noise);
        part(&mut x, 4, &[am], 1, false, false, 0.4, &mut noise);
        let bounds = [4.0, 12.0, 20.0, 28.0, 36.0, 40.0, 48.0]
            .iter()
            .map(|b| b * BAR)
            .collect();
        (x, bounds)
    }

    #[test]
    fn a_song_is_cut_into_its_parts_and_they_are_named() {
        let (x, truth) = song();
        let s = analyse(&x, SR, 4.0);
        let t = s.tempo.expect("a tempo");
        assert!((t.bpm - 120.0).abs() < 1.0, "{} BPM", t.bpm);
        let starts: Vec<f64> = s.parts.iter().skip(1).map(|p| p.start).collect();
        let found = truth
            .iter()
            .filter(|b| starts.iter().any(|s| (s - **b).abs() <= BAR + 0.1))
            .count();
        let names: Vec<&str> = s.parts.iter().map(|p| p.name.as_str()).collect();
        assert!(found >= 6, "boundaries {starts:?} vs {truth:?} ({names:?})");
        assert!(starts.len() <= truth.len() + 2, "{starts:?}");
        // The part at each true part's middle.
        let at = |bar: f64| {
            s.parts
                .iter()
                .find(|p| p.start <= bar * BAR && p.end > bar * BAR)
                .unwrap_or_else(|| panic!("nothing at bar {bar}: {:?}", s.parts))
        };
        // Intro 0–4, Verse 4–12, Chorus 12–20, Verse 20–28, Chorus 28–36,
        // Bridge 36–40, Chorus 40–48, Outro 48–52 (bars).
        let (v1, v2) = (at(8.0), at(24.0));
        let (c1, c2, c3) = (at(16.0), at(32.0), at(44.0));
        assert_eq!(v1.label, v2.label, "{names:?}");
        assert_eq!(c1.label, c2.label, "{names:?}");
        assert_eq!(c1.label, c3.label, "{names:?}");
        assert_ne!(v1.label, c1.label, "{names:?}");
        assert!(c1.name.starts_with("Chorus"), "{names:?}");
        assert!(v1.name.starts_with("Verse"), "{names:?}");
        assert_eq!(at(1.0).name, "Intro", "{names:?}");
        assert_eq!(at(50.0).name, "Outro", "{names:?}");
        assert!(at(38.0).label != c1.label && at(38.0).label != v1.label);
    }

    #[test]
    fn silence_in_a_jam_is_a_gap() {
        let (mut x, _) = song();
        let mut gap = vec![0.0f32; (8.0 * SR) as usize];
        let (y, _) = song();
        x.append(&mut gap);
        x.extend_from_slice(&y);
        let s = analyse(&x, SR, 4.0);
        let quiet = (104.0 + 2.0, 104.0 + 6.0);
        assert!(
            s.parts
                .iter()
                .all(|p| p.end <= quiet.0 + BAR || p.start >= quiet.1 - BAR),
            "{:?}",
            s.parts
        );
    }

    #[test]
    fn a_short_or_silent_recording_has_little_structure() {
        assert!(analyse(&vec![0.0; 44_100 * 10], SR, 4.0).parts.is_empty());
        let (x, _) = song();
        let s = analyse(&x[..(6.0 * SR) as usize], SR, 4.0);
        assert!(s.parts.len() <= 1);
    }
}
