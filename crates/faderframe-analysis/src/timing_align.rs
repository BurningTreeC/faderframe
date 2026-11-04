//! Timing alignment of a double to a guide: where in a dub (a doubled
//! vocal, a backing line sung with the lead, an ADR line against the
//! production take) each moment of the guide was sung, as a smooth time
//! map the dub can be warped by.
//!
//! Both are described every 5 ms ([`HOP_SECONDS`]) by what they share
//! however different the voices are: the spectral envelope as cepstral
//! coefficients of third-octave band levels (vowels and consonants; bands
//! at least 450 Hz wide, so where another singer's harmonics fall does not
//! count; the mean of each coefficient taken off, so microphones and
//! voices differ less), the onset strength (the rise of the band levels:
//! syllable starts) and the level against the recording's loudest.
//! Dynamic time warping finds the cheapest path through their distances
//! with the symmetric slope constraint (steps of one or two frames: the
//! dub is at most twice as fast or as slow anywhere; a step off the
//! diagonal costs a little more, so a held vowel gives the path no reason
//! to wander), first on 40 ms frames within
//! [`Settings::max_offset`] of the best overall lag (the onset envelopes'
//! cross-correlation), then on the 5 ms frames in a band around that
//! path. Its start and end are open: a dub that begins late or ends early
//! is followed where both sound.
//!
//! The path is made a map (the dub frame for each guide frame), its offset
//! smoothed over a Gaussian whose width follows [`Settings::tightness`]
//! (300 ms when loose: phrases; 10 ms when tight: syllables) with only
//! the frames where something sounds counting (silences take the offsets
//! around them), the stretch limited (at most 1.25 times faster or slower
//! when loose, twice when tight) and, with
//! [`Settings::protect_consonants`], every sibilant of the dub played at
//! its own speed from where its onset lands (noise smears when stretched;
//! the time is taken from the vowels after it).

use crate::fft;

/// Frames of the analysis (seconds).
pub const HOP_SECONDS: f64 = 0.005;
/// The analysis window (seconds; a Hann window this long or the next power
/// of two of samples).
const WINDOW_SECONDS: f64 = 0.032;
/// Cepstral coefficients compared (the first, the level, apart).
const CEPS: usize = 12;
/// Third-octave bands from 100 Hz (to about 7 kHz).
const LOWEST_BAND_HZ: f64 = 100.0;
const BANDS: usize = 19;
/// The narrowest band (Hz): wider than a voice's harmonics are apart.
const ENVELOPE_HZ: f64 = 450.0;
/// Sibilance: the share of the energy above this, and how much of it.
const SIBILANT_HZ: f64 = 4_000.0;
const SIBILANT_SHARE: f32 = 0.45;
/// Zero crossings per sample of a sibilant (noise; a vowel crosses at
/// its first formant's rate, far fewer).
const SIBILANT_CROSSINGS: f32 = 0.2;
/// Sibilants longer than this are not protected (a held "sss" stretches
/// as well as any noise).
const LONGEST_SIBILANT_SECONDS: f64 = 0.15;
const SHORTEST_SIBILANT_FRAMES: usize = 3;
/// A frame sounds when it is within this of the recording's loudest
/// (dB) and above an absolute floor.
const ACTIVE_RANGE_DB: f32 = 45.0;
const FLOOR_DB: f32 = -80.0;
/// Coarse frames (fine frames each).
const COARSE: usize = 8;
/// The fine band either side of the coarse path (fine frames).
const FINE_RADIUS: usize = 24;
/// What a step off the diagonal costs beyond its frames' distances: inside
/// a held vowel every frame is like the next, and the path would wander
/// there for nothing.
const BENT: f32 = 0.15;
/// Smoothing at tightness 0 and 1 (frames).
const LOOSE_SIGMA: f64 = 60.0;
const TIGHT_SIGMA: f64 = 2.0;
/// Stretch limits at tightness 0 and 1 (speed ratio either way).
const LOOSE_STRETCH: f64 = 1.25;
const TIGHT_STRETCH: f64 = 2.0;

/// How a dub is aligned.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    /// 0 (loose: phrases move, syllables keep their own timing) … 1
    /// (tight: every syllable follows the guide).
    pub tightness: f64,
    /// Sibilants keep their own length (the time is found in the vowels).
    pub protect_consonants: bool,
    /// The furthest the dub is searched from the guide (seconds).
    pub max_offset: f64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            tightness: 0.5,
            protect_consonants: true,
            max_offset: 0.5,
        }
    }
}

/// Where the dub plays each moment of the guide: `dub[i]` is the dub's
/// time (seconds) heard at guide time `i × hop`. Strictly increasing.
#[derive(Clone, Debug, PartialEq)]
pub struct TimeMap {
    pub hop: f64,
    pub dub: Vec<f64>,
}

impl TimeMap {
    /// The dub's time at guide time `t` (seconds; extrapolated at speed 1
    /// outside).
    pub fn at(&self, t: f64) -> f64 {
        let n = self.dub.len();
        if n == 0 {
            return t;
        }
        let x = t / self.hop;
        if x <= 0.0 {
            return self.dub[0] + t;
        }
        let i = x.floor() as usize;
        if i + 1 >= n {
            return self.dub[n - 1] + (t - (n - 1) as f64 * self.hop);
        }
        let f = x - i as f64;
        self.dub[i] + (self.dub[i + 1] - self.dub[i]) * f
    }

    /// The map as few (guide, dub) points as keep it within `tolerance`
    /// seconds (Ramer–Douglas–Peucker on the dub's time), the ends
    /// included.
    pub fn points(&self, tolerance: f64) -> Vec<(f64, f64)> {
        let n = self.dub.len();
        if n == 0 {
            return Vec::new();
        }
        let pt = |i: usize| (i as f64 * self.hop, self.dub[i]);
        if n == 1 {
            return vec![pt(0)];
        }
        let mut keep = vec![false; n];
        keep[0] = true;
        keep[n - 1] = true;
        let mut stack = vec![(0usize, n - 1)];
        while let Some((a, b)) = stack.pop() {
            if b <= a + 1 {
                continue;
            }
            let (ta, da) = pt(a);
            let (tb, db) = pt(b);
            let (mut worst, mut at) = (0.0, a);
            for i in a + 1..b {
                let (t, d) = pt(i);
                let line = da + (db - da) * (t - ta) / (tb - ta);
                let e = (d - line).abs();
                if e > worst {
                    worst = e;
                    at = i;
                }
            }
            if worst > tolerance {
                keep[at] = true;
                stack.push((a, at));
                stack.push((at, b));
            }
        }
        (0..n).filter(|i| keep[*i]).map(pt).collect()
    }

    /// The largest distance (seconds) between the guide's time and the
    /// dub's.
    pub fn largest_move(&self) -> f64 {
        self.dub
            .iter()
            .enumerate()
            .map(|(i, d)| (d - i as f64 * self.hop).abs())
            .fold(0.0, f64::max)
    }
}

/// What the analysis hears every 5 ms.
#[derive(Clone, Debug, Default)]
pub struct Frames {
    /// Cepstra of the band levels (mean taken off over the active frames).
    cep: Vec<[f32; CEPS]>,
    /// Onset strength (scaled to its 95th percentile).
    onset: Vec<f32>,
    /// Level (dB against the loudest, −ACTIVE_RANGE_DB floor).
    level: Vec<f32>,
    /// Sibilant frames.
    sibilant: Vec<bool>,
    active: Vec<bool>,
}

impl Frames {
    pub fn len(&self) -> usize {
        self.level.len()
    }

    pub fn is_empty(&self) -> bool {
        self.level.is_empty()
    }

    /// Frames where something sounds.
    pub fn active_count(&self) -> usize {
        self.active.iter().filter(|a| **a).count()
    }

    /// `factor` frames averaged into one (the coarse search).
    fn coarse(&self, factor: usize) -> Frames {
        let n = self.len().div_ceil(factor);
        let mut out = Frames {
            cep: vec![[0.0; CEPS]; n],
            onset: vec![0.0; n],
            level: vec![0.0; n],
            sibilant: vec![false; n],
            active: vec![false; n],
        };
        for k in 0..n {
            let range = k * factor..((k + 1) * factor).min(self.len());
            let count = range.len().max(1) as f32;
            for i in range {
                for c in 0..CEPS {
                    out.cep[k][c] += self.cep[i][c] / count;
                }
                out.onset[k] = out.onset[k].max(self.onset[i]);
                out.level[k] += self.level[i] / count;
                out.active[k] |= self.active[i];
            }
        }
        out
    }
}

/// The analysis of `x` at `rate` (frame `i` centred on `i × hop`).
pub fn frames(x: &[f32], rate: f64) -> Frames {
    let hop = ((rate * HOP_SECONDS).round() as usize).max(1);
    let size = ((rate * WINDOW_SECONDS).round() as usize)
        .next_power_of_two()
        .max(64);
    let count = x.len().div_ceil(hop);
    let window: Vec<f64> = (0..size)
        .map(|k| 0.5 - 0.5 * (std::f64::consts::TAU * k as f64 / size as f64).cos())
        .collect();
    // Bins of each band (at least one). A band is at least
    // `ENVELOPE_HZ` wide: the spectral envelope, not where a voice's
    // harmonics happen to fall (another singer, another pitch).
    let bin_hz = rate / size as f64;
    let bands: Vec<(usize, usize)> = (0..BANDS)
        .map(|b| {
            let centre = LOWEST_BAND_HZ * 2f64.powf(b as f64 / 3.0);
            let half = (centre * (2f64.powf(1.0 / 6.0) - 2f64.powf(-1.0 / 6.0)) / 2.0)
                .max(ENVELOPE_HZ / 2.0);
            let (lo, hi) = ((centre - half).max(bin_hz), centre + half);
            let a = ((lo / bin_hz).ceil() as usize).min(size / 2);
            let z = ((hi / bin_hz).floor() as usize).clamp(a, size / 2);
            (a, z.max(a))
        })
        .collect();
    let sibilant_bin = ((SIBILANT_HZ / bin_hz) as usize).min(size / 2);
    // The DCT-II basis over the bands.
    let basis: Vec<[f32; BANDS]> = (1..=CEPS)
        .map(|c| {
            let mut row = [0.0f32; BANDS];
            for (b, v) in row.iter_mut().enumerate() {
                *v = (std::f64::consts::PI * c as f64 * (b as f64 + 0.5) / BANDS as f64).cos()
                    as f32;
            }
            row
        })
        .collect();
    let mut re = vec![0.0f64; size];
    let mut im = vec![0.0f64; size];
    let mut out = Frames {
        cep: vec![[0.0; CEPS]; count],
        onset: vec![0.0; count],
        level: vec![0.0; count],
        sibilant: vec![false; count],
        active: vec![false; count],
    };
    let mut crossings = vec![0.0f32; count];
    let mut share = vec![0.0f32; count];
    let mut last = [0.0f32; BANDS];
    let floor = 10f64.powf(f64::from(FLOOR_DB) / 10.0);
    for i in 0..count {
        let centre = (i * hop) as isize;
        let from = centre - (size / 2) as isize;
        let mut zc = 0usize;
        let mut prev = 0.0f32;
        for k in 0..size {
            let at = from + k as isize;
            let v = if at >= 0 && (at as usize) < x.len() {
                x[at as usize]
            } else {
                0.0
            };
            re[k] = f64::from(v) * window[k];
            im[k] = 0.0;
            if k > 0 && (v >= 0.0) != (prev >= 0.0) {
                zc += 1;
            }
            prev = v;
        }
        crossings[i] = zc as f32 / size as f32;
        fft(&mut re, &mut im);
        let power = |k: usize| re[k] * re[k] + im[k] * im[k];
        let norm = 4.0 / (size as f64 * size as f64);
        let mut total = 0.0;
        let mut high = 0.0;
        for k in 1..size / 2 {
            let p = power(k);
            total += p;
            if k >= sibilant_bin {
                high += p;
            }
        }
        share[i] = if total > 0.0 {
            (high / total) as f32
        } else {
            0.0
        };
        out.level[i] = (10.0 * (total * norm + floor).log10()) as f32;
        let mut levels = [0.0f32; BANDS];
        for (b, &(a, z)) in bands.iter().enumerate() {
            let e: f64 = (a..=z).map(power).sum::<f64>() / (z + 1 - a) as f64;
            levels[b] = (10.0 * (e * norm + floor).log10()) as f32;
        }
        for (c, row) in basis.iter().enumerate() {
            out.cep[i][c] = row.iter().zip(&levels).map(|(w, l)| w * l).sum::<f32>() / BANDS as f32;
        }
        if i > 0 {
            out.onset[i] = levels
                .iter()
                .zip(&last)
                .map(|(l, p)| (l - p).max(0.0))
                .sum::<f32>()
                / BANDS as f32;
        }
        last = levels;
    }
    // Levels against the loudest; what sounds.
    let loudest = out.level.iter().copied().fold(FLOOR_DB, f32::max);
    for i in 0..count {
        out.active[i] = out.level[i] > loudest - ACTIVE_RANGE_DB && out.level[i] > FLOOR_DB + 20.0;
        out.level[i] = (out.level[i] - loudest).max(-ACTIVE_RANGE_DB);
    }
    // Onsets only count where something sounds, scaled to their 95th
    // percentile.
    for i in 0..count {
        if !out.active[i] {
            out.onset[i] = 0.0;
        }
    }
    let mut sorted: Vec<f32> = out
        .onset
        .iter()
        .zip(&out.active)
        .filter(|(_, a)| **a)
        .map(|(o, _)| *o)
        .collect();
    if !sorted.is_empty() {
        sorted.sort_by(f32::total_cmp);
        let p95 = sorted[(sorted.len() * 95 / 100).min(sorted.len() - 1)].max(1e-3);
        for o in &mut out.onset {
            *o = (*o / p95).min(2.0);
        }
    }
    // The cepstra's means over the active frames off.
    let active = out.active.iter().filter(|a| **a).count();
    if active > 0 {
        let mut mean = [0.0f32; CEPS];
        for (c, a) in out.cep.iter().zip(&out.active) {
            if *a {
                for k in 0..CEPS {
                    mean[k] += c[k] / active as f32;
                }
            }
        }
        for c in &mut out.cep {
            for k in 0..CEPS {
                c[k] -= mean[k];
            }
        }
    }
    // Sibilants: noisy, bright, sounding.
    for i in 0..count {
        out.sibilant[i] =
            out.active[i] && share[i] >= SIBILANT_SHARE && crossings[i] >= SIBILANT_CROSSINGS;
    }
    out
}

/// How unlike guide frame `i` and dub frame `j` are.
fn distance(g: &Frames, i: usize, d: &Frames, j: usize) -> f32 {
    let mut cep = 0.0;
    for k in 0..CEPS {
        let e = g.cep[i][k] - d.cep[j][k];
        cep += e * e;
    }
    // Band levels are dB: a few dB per coefficient is a different sound.
    let cep = (cep / CEPS as f32).sqrt() / 4.0;
    let onset = (g.onset[i] - d.onset[j]).abs();
    let level = (g.level[i] - d.level[j]).abs() / 15.0;
    // Silence against silence is alike whatever its noise is.
    let cep = if g.active[i] || d.active[j] { cep } else { 0.0 };
    cep + onset + 0.5 * level
}

/// The lag (dub frames after guide frames) that lines up the two's onset
/// and level envelopes best, within `reach` frames.
fn best_lag(g: &Frames, d: &Frames, reach: usize) -> isize {
    let env = |f: &Frames, i: usize| {
        f.onset[i] + if f.active[i] { 0.5 } else { 0.0 } + (f.level[i] + ACTIVE_RANGE_DB) / 90.0
    };
    let mean_g = (0..g.len()).map(|i| env(g, i)).sum::<f32>() / g.len().max(1) as f32;
    let mean_d = (0..d.len()).map(|i| env(d, i)).sum::<f32>() / d.len().max(1) as f32;
    let reach = reach as isize;
    let mut best = (0isize, f32::MIN);
    for lag in -reach..=reach {
        let (mut sum, mut gg, mut dd) = (0.0f32, 0.0f32, 0.0f32);
        for i in 0..g.len() {
            let j = i as isize + lag;
            if j < 0 || j as usize >= d.len() {
                continue;
            }
            let a = env(g, i) - mean_g;
            let b = env(d, j as usize) - mean_d;
            sum += a * b;
            gg += a * a;
            dd += b * b;
        }
        let r = sum / (gg * dd).sqrt().max(1e-9);
        // Nearer lags win ties.
        let r = r - 1e-4 * lag.unsigned_abs() as f32 / reach.max(1) as f32;
        if r > best.1 {
            best = (lag, r);
        }
    }
    best.0
}

/// Dynamic time warping of `g` against `d` inside a band: row `i` holds
/// dub frames `centre[i] − radius ..= centre[i] + radius`. The path (guide
/// frame, dub frame), start to end; open at both ends.
fn dtw(g: &Frames, d: &Frames, centre: &[isize], radius: usize) -> Vec<(usize, usize)> {
    let (n, m) = (g.len(), d.len());
    if n == 0 || m == 0 {
        return Vec::new();
    }
    let w = 2 * radius + 1;
    let inf = f32::INFINITY;
    // Accumulated cost, the step taken (0 diagonal, 1 from (i−2, j−1),
    // 2 from (i−1, j−2), 3 a start) and the path's length so far.
    let mut cost = vec![inf; n * w];
    let mut step = vec![3u8; n * w];
    let mut len = vec![0u32; n * w];
    let lo = |i: usize| centre[i] - radius as isize;
    let slot = |i: usize, j: isize| -> Option<usize> {
        let k = j - lo(i);
        (j >= 0 && (j as usize) < m && k >= 0 && (k as usize) < w).then(|| i * w + k as usize)
    };
    let local = |i: usize, j: usize| distance(g, i, d, j);
    for i in 0..n {
        for k in 0..w {
            let j = lo(i) + k as isize;
            if j < 0 || j as usize >= m {
                continue;
            }
            let ju = j as usize;
            let here = local(i, ju);
            let at = i * w + k;
            // A start: the first row or column.
            let mut best = if i == 0 || ju == 0 {
                (2.0 * here, 3u8, 2u32)
            } else {
                (inf, 3, 0)
            };
            // Paths of different lengths meet here (open starts): the
            // cheapest per step wins.
            let mut consider = |c: f32, s: u8, l: u32| {
                let mean_old = if best.0 == inf {
                    inf
                } else {
                    best.0 / best.2.max(1) as f32
                };
                if c / (l.max(1) as f32) < mean_old {
                    best = (c, s, l);
                }
            };
            if i >= 1
                && let Some(p) = slot(i - 1, j - 1)
                && cost[p] < inf
            {
                consider(cost[p] + 2.0 * here, 0, len[p] + 2);
            }
            if i >= 2
                && ju >= 1
                && let Some(p) = slot(i - 2, j - 1)
                && cost[p] < inf
            {
                consider(
                    cost[p] + 2.0 * local(i - 1, ju) + here + BENT,
                    1,
                    len[p] + 3,
                );
            }
            if i >= 1
                && ju >= 2
                && let Some(p) = slot(i - 1, j - 2)
                && cost[p] < inf
            {
                consider(
                    cost[p] + 2.0 * local(i, ju - 1) + here + BENT,
                    2,
                    len[p] + 3,
                );
            }
            cost[at] = best.0;
            step[at] = best.1;
            len[at] = best.2;
        }
    }
    // The end: the last row or column, cheapest per step.
    let mut end: Option<(usize, usize, f32)> = None;
    let mut look = |i: usize, j: isize| {
        if let Some(s) = slot(i, j)
            && cost[s] < inf
        {
            let mean = cost[s] / len[s].max(1) as f32;
            if end.is_none_or(|e| mean < e.2) {
                end = Some((i, j as usize, mean));
            }
        }
    };
    for k in 0..w {
        look(n - 1, lo(n - 1) + k as isize);
    }
    for i in 0..n {
        look(i, m as isize - 1);
    }
    let Some((mut i, mut j, _)) = end else {
        return Vec::new();
    };
    let mut path = vec![(i, j)];
    while let Some(s) = slot(i, j as isize) {
        match step[s] {
            0 => {
                i -= 1;
                j -= 1;
            }
            1 => {
                path.push((i - 1, j));
                i -= 2;
                j -= 1;
            }
            2 => {
                path.push((i, j - 1));
                i -= 1;
                j -= 2;
            }
            _ => break,
        }
        path.push((i, j));
    }
    path.reverse();
    path
}

/// The dub frame for each of `n` guide frames from a path (the mean of the
/// dub frames it pairs a guide frame with; at speed 1 beyond the path's
/// ends).
fn path_map(path: &[(usize, usize)], n: usize) -> Vec<f64> {
    let mut sum = vec![0.0f64; n];
    let mut count = vec![0u32; n];
    for &(i, j) in path {
        if i < n {
            sum[i] += j as f64;
            count[i] += 1;
        }
    }
    let mut map = vec![f64::NAN; n];
    for i in 0..n {
        if count[i] > 0 {
            map[i] = sum[i] / f64::from(count[i]);
        }
    }
    fill_gaps(&mut map);
    map
}

/// NaNs filled: between known values linearly, outside them at speed 1.
fn fill_gaps(map: &mut [f64]) {
    let known: Vec<usize> = (0..map.len()).filter(|i| !map[*i].is_nan()).collect();
    let (Some(&first), Some(&last)) = (known.first(), known.last()) else {
        for (i, v) in map.iter_mut().enumerate() {
            *v = i as f64;
        }
        return;
    };
    for i in 0..first {
        map[i] = map[first] - (first - i) as f64;
    }
    for i in last + 1..map.len() {
        map[i] = map[last] + (i - last) as f64;
    }
    for pair in known.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        for i in a + 1..b {
            map[i] = map[a] + (map[b] - map[a]) * (i - a) as f64 / (b - a) as f64;
        }
    }
}

/// The offset `map[i] − i` smoothed over a Gaussian of `sigma` frames,
/// only `weight`ed frames counting; where none is near, filled between.
fn smooth(map: &mut [f64], weight: &[bool], sigma: f64) {
    let n = map.len();
    let reach = (3.0 * sigma).ceil() as isize;
    let kernel: Vec<f64> = (-reach..=reach)
        .map(|k| (-0.5 * (k as f64 / sigma).powi(2)).exp())
        .collect();
    let full: f64 = kernel.iter().sum();
    let offset: Vec<f64> = map.iter().enumerate().map(|(i, v)| v - i as f64).collect();
    let mut out = vec![f64::NAN; n];
    for (i, o) in out.iter_mut().enumerate() {
        let (mut s, mut w) = (0.0, 0.0);
        for (k, g) in kernel.iter().enumerate() {
            let j = i as isize + k as isize - reach;
            if j < 0 || j as usize >= n || !weight[j as usize] {
                continue;
            }
            s += g * offset[j as usize];
            w += g;
        }
        if w > 0.05 * full {
            *o = s / w;
        }
    }
    // Silences: the offsets around them, linearly.
    let mut filled: Vec<f64> = out
        .iter()
        .enumerate()
        .map(|(i, o)| if o.is_nan() { f64::NAN } else { o + i as f64 })
        .collect();
    let any = filled.iter().any(|v| !v.is_nan());
    if any {
        // Outside the known ones the offset holds (not speed 1 from the
        // path's frame: an offset, not a frame).
        let known: Vec<usize> = (0..n).filter(|i| !filled[*i].is_nan()).collect();
        let (first, last) = (known[0], known[known.len() - 1]);
        for i in 0..first {
            filled[i] = filled[first] - (first - i) as f64;
        }
        for i in last + 1..n {
            filled[i] = filled[last] + (i - last) as f64;
        }
        fill_gaps(&mut filled);
        map.copy_from_slice(&filled);
    }
}

/// Every step between `1/stretch` and `stretch` frames (strictly
/// increasing).
fn limit_stretch(map: &mut [f64], stretch: f64) {
    let (lo, hi) = (1.0 / stretch, stretch);
    for i in 1..map.len() {
        map[i] = map[i].clamp(map[i - 1] + lo, map[i - 1] + hi);
    }
    for i in (0..map.len().saturating_sub(1)).rev() {
        map[i] = map[i].clamp(map[i + 1] - hi, map[i + 1] - lo);
    }
}

/// The dub's sibilants (frame ranges) short enough to protect.
fn sibilants(d: &Frames) -> Vec<(usize, usize)> {
    let longest = (LONGEST_SIBILANT_SECONDS / HOP_SECONDS) as usize;
    let mut runs = Vec::new();
    let mut i = 0;
    while i < d.len() {
        if !d.sibilant[i] {
            i += 1;
            continue;
        }
        let start = i;
        // A single frame's gap does not end one.
        while i < d.len() && (d.sibilant[i] || (i + 1 < d.len() && d.sibilant[i + 1])) {
            i += 1;
        }
        let len = i - start;
        if (SHORTEST_SIBILANT_FRAMES..=longest).contains(&len) {
            runs.push((start.saturating_sub(1), (i + 1).min(d.len())));
        }
    }
    runs
}

/// Play each of `runs` (dub frames) at speed 1 from where its start lands,
/// joining the map again within the stretch allowed.
fn protect(map: &mut [f64], runs: &[(usize, usize)], stretch: f64) {
    let n = map.len();
    for &(a, b) in runs {
        // The guide frame where the dub reaches the sibilant.
        let Some(start) = (0..n).find(|&i| map[i] >= a as f64) else {
            continue;
        };
        let length = b - a;
        let base = map[start];
        let held_to = (start + length).min(n - 1);
        for (k, v) in map[start..=held_to].iter_mut().enumerate() {
            *v = base + k as f64;
        }
        // Rejoin: the gap spread over enough frames.
        let after = held_to + 1;
        if after >= n {
            continue;
        }
        let held = |k: usize| base + (k - start) as f64;
        let room = (stretch - 1.0).min(1.0 - 1.0 / stretch).max(0.05);
        let gap = (map[after] - held(after)).abs();
        let ramp = ((gap / room * 1.5).ceil() as usize).clamp(2, n - after);
        for (r, i) in (after..(after + ramp).min(n)).enumerate() {
            let t = (r + 1) as f64 / ramp as f64;
            let w = t * t * (3.0 - 2.0 * t);
            map[i] = held(i) + w * (map[i] - held(i));
        }
    }
}

/// Align `dub` to `guide` (both mono at `rate`, starting at the same
/// moment): the dub's time for each moment of the guide. `None` when
/// either holds nothing to align by.
pub fn align(guide: &[f32], dub: &[f32], rate: f64, settings: &Settings) -> Option<TimeMap> {
    let g = frames(guide, rate);
    let d = frames(dub, rate);
    align_frames(&g, &d, settings)
}

/// [`align`] on frames already made.
pub fn align_frames(g: &Frames, d: &Frames, settings: &Settings) -> Option<TimeMap> {
    let least = (0.1 / HOP_SECONDS) as usize;
    if g.active_count() < least || d.active_count() < least {
        return None;
    }
    let tight = settings.tightness.clamp(0.0, 1.0);
    // Coarse: the best lag, then a path within the reach around it.
    let (gc, dc) = (g.coarse(COARSE), d.coarse(COARSE));
    let reach = ((settings.max_offset.max(0.05) / HOP_SECONDS) as usize / COARSE).max(2);
    let lag = best_lag(&gc, &dc, reach);
    let centre: Vec<isize> = (0..gc.len()).map(|i| i as isize + lag).collect();
    let coarse = dtw(&gc, &dc, &centre, reach);
    if coarse.is_empty() {
        return None;
    }
    let coarse_map = path_map(&coarse, gc.len());
    // Fine: a band around the coarse path.
    let centre: Vec<isize> = (0..g.len())
        .map(|i| {
            let x = i as f64 / COARSE as f64;
            let k = (x.floor() as usize).min(gc.len() - 1);
            let next = (k + 1).min(gc.len() - 1);
            let f = x - k as f64;
            let v = coarse_map[k] + (coarse_map[next] - coarse_map[k]) * f;
            (v * COARSE as f64).round() as isize
        })
        .collect();
    let path = dtw(g, d, &centre, FINE_RADIUS);
    if path.is_empty() {
        return None;
    }
    let mut map = path_map(&path, g.len());
    // Smoothed where something sounds (in either).
    let weight: Vec<bool> = (0..g.len())
        .map(|i| {
            let j = (map[i].round().max(0.0) as usize).min(d.len().saturating_sub(1));
            g.active[i] || d.active.get(j).copied().unwrap_or(false)
        })
        .collect();
    let sigma = LOOSE_SIGMA * (TIGHT_SIGMA / LOOSE_SIGMA).powf(tight);
    smooth(&mut map, &weight, sigma);
    let stretch = LOOSE_STRETCH + (TIGHT_STRETCH - LOOSE_STRETCH) * tight;
    limit_stretch(&mut map, stretch);
    if settings.protect_consonants {
        protect(&mut map, &sibilants(d), stretch);
        limit_stretch(&mut map, stretch.max(1.0 + 1e-3));
    }
    Some(TimeMap {
        hop: HOP_SECONDS,
        dub: map.into_iter().map(|j| j * HOP_SECONDS).collect(),
    })
}

/// Voice-like test material: syllables of a glottal pulse through vowel
/// formants with sibilant and plosive consonants, placed where asked.
#[doc(hidden)]
pub mod synth {
    use std::f64::consts::{PI, TAU};

    /// A syllable: its consonant (0 none, 1 "s", 2 "t"), the vowel (0–4)
    /// and when it starts and how long it lasts (seconds).
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct Syllable {
        pub consonant: u8,
        pub vowel: u8,
        pub start: f64,
        pub length: f64,
        /// The sung note (MIDI, fractional).
        pub note: f64,
    }

    /// Formants (Hz) of a, e, i, o, u.
    const VOWELS: [[f64; 3]; 5] = [
        [730.0, 1090.0, 2440.0],
        [530.0, 1840.0, 2480.0],
        [270.0, 2290.0, 3010.0],
        [570.0, 840.0, 2410.0],
        [300.0, 870.0, 2240.0],
    ];

    struct Resonator {
        a: f64,
        b: f64,
        c: f64,
        y1: f64,
        y2: f64,
    }

    impl Resonator {
        fn new(f: f64, bw: f64, rate: f64) -> Self {
            let t = 1.0 / rate;
            let c = -(-TAU * bw * t).exp();
            let b = 2.0 * (-PI * bw * t).exp() * (TAU * f * t).cos();
            Self {
                a: 1.0 - b - c,
                b,
                c,
                y1: 0.0,
                y2: 0.0,
            }
        }

        fn run(&mut self, x: f64) -> f64 {
            let y = self.a * x + self.b * self.y1 + self.c * self.y2;
            self.y2 = self.y1;
            self.y1 = y;
            y
        }
    }

    /// Hashed noise in −1…1 for sample `n` (`seed` picks the stream).
    pub fn noise(n: u64, seed: u64) -> f64 {
        let mut z = n
            .wrapping_mul(0x9e37_79b9_7f4a_7c15)
            .wrapping_add(seed.wrapping_mul(0xbf58_476d_1ce4_e5b9));
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^= z >> 31;
        (z >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0
    }

    /// `seconds` of the syllables sung at `rate` (`brightness` moves the
    /// formants: another singer; `seed` the breath noise).
    pub fn phrase(
        syllables: &[Syllable],
        seconds: f64,
        rate: f64,
        brightness: f64,
        seed: u64,
    ) -> Vec<f32> {
        let mut out = vec![0.0f32; (seconds * rate) as usize];
        for (k, s) in syllables.iter().enumerate() {
            let consonant = match s.consonant {
                1 => 0.07,
                2 => 0.02,
                _ => 0.0,
            };
            // The consonant before the vowel.
            if consonant > 0.0 {
                let from = (s.start * rate) as usize;
                let n = (consonant * rate) as usize;
                let (f, bw) = if s.consonant == 1 {
                    (6_500.0, 2_500.0)
                } else {
                    (3_000.0, 3_000.0)
                };
                let mut r1 = Resonator::new(f, bw, rate);
                let mut r2 = Resonator::new(f, bw, rate);
                let mut last = 0.0;
                for i in 0..n {
                    let at = from + i;
                    if at >= out.len() {
                        break;
                    }
                    let x = noise(at as u64, seed ^ k as u64);
                    let d = x - last;
                    last = x;
                    let t = i as f64 / n as f64;
                    let env = if s.consonant == 1 {
                        (PI * t).sin().powf(0.5)
                    } else {
                        (-t * 6.0).exp()
                    };
                    out[at] += (r2.run(r1.run(d)) * env * 0.5) as f32;
                }
            }
            let from = ((s.start + consonant) * rate) as usize;
            let n = ((s.length - consonant).max(0.02) * rate) as usize;
            let formants = VOWELS[usize::from(s.vowel) % 5];
            let mut res: Vec<Resonator> = formants
                .iter()
                .map(|f| Resonator::new(f * brightness, 60.0 + f * 0.06, rate))
                .collect();
            let mut phase = 0.0;
            for i in 0..n {
                let at = from + i;
                if at >= out.len() {
                    break;
                }
                let t = i as f64 / rate;
                let hz = 440.0
                    * 2f64.powf((s.note - 69.0) / 12.0)
                    * (1.0 + 0.004 * (TAU * 5.5 * t).sin());
                phase += hz / rate;
                // A glottal pulse: a sawtooth's falling edge, softened.
                let p = phase.fract();
                let pulse = if p < 0.6 {
                    (PI * p / 0.6).sin().powi(2)
                } else {
                    0.0
                } - 0.3;
                let mut y = 0.0;
                for (r, w) in res.iter_mut().zip([1.0, 0.6, 0.3]) {
                    y += w * r.run(pulse);
                }
                let edge = (i as f64 / (0.015 * rate)).min(1.0)
                    * ((n - i) as f64 / (0.03 * rate)).min(1.0);
                out[at] += (y * edge * 0.6) as f32;
            }
        }
        out
    }
}

#[cfg(test)]
mod tests;
