//! Tempo through hit points: the tempo map that lands every hit (a time in
//! seconds: a cut in the picture, a marker) exactly on a beat — or on a bar
//! line — with the tempo as steady as it can be inside a range.
//!
//! Between two hits the music plays a whole number of beats, so each
//! stretch has a tempo of `60 · beats / seconds`. Choosing the beats is a
//! shortest path: every stretch's choices are the beat counts whose tempo
//! falls in the range, a step costs the squared change of tempo (in log, so
//! 100 → 110 counts as 120 → 132) plus how far it strays from the preferred
//! tempo, and dynamic programming finds the cheapest sequence. The stretch
//! before the first hit may take any tempo in the range (the music starts
//! somewhere); hits too close to the one before to be a beat apart at the
//! fastest tempo are left out and reported.

use crate::{MusicalTime, TempoCurve, TempoMap, TempoPoint};

/// What a hit lands on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum HitGrid {
    /// Any beat (quarter note).
    Beats,
    /// Any eighth.
    Eighths,
    /// A bar line, bars of this many quarters.
    Bars(f64),
}

impl HitGrid {
    /// The step in quarters.
    pub fn quarters(self) -> f64 {
        match self {
            HitGrid::Beats => 1.0,
            HitGrid::Eighths => 0.5,
            HitGrid::Bars(q) => q.max(0.25),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HitSettings {
    pub min_bpm: f64,
    pub max_bpm: f64,
    /// A tempo to keep near (None: only steadiness counts).
    pub preferred: Option<f64>,
    pub grid: HitGrid,
}

impl Default for HitSettings {
    fn default() -> Self {
        Self {
            min_bpm: 70.0,
            max_bpm: 160.0,
            preferred: None,
            grid: HitGrid::Beats,
        }
    }
}

/// The answer: each kept hit's position (quarters from the start), the
/// tempo map, the hits left out (their indices), and the stretches'
/// tempos.
#[derive(Clone, Debug, PartialEq)]
pub struct HitSolution {
    pub positions: Vec<(usize, f64)>,
    pub tempo: TempoMap,
    pub dropped: Vec<usize>,
    pub tempos: Vec<f64>,
}

impl HitSolution {
    /// The largest tempo change from one stretch to the next (a share:
    /// 0.05 = 5 %), the stretch before the first hit not counted.
    pub fn largest_change(&self) -> f64 {
        self.tempos
            .windows(2)
            .skip(1)
            .map(|w| (w[1] / w[0] - 1.0).abs())
            .fold(0.0, f64::max)
    }
}

/// How much more steadiness counts than being near the preferred tempo.
const PREFER: f64 = 0.15;

/// Solve for `hits` (seconds from the project's start, any order); None
/// when no hit can be placed.
pub fn solve(hits: &[f64], settings: &HitSettings) -> Option<HitSolution> {
    let (lo, hi) = (
        settings.min_bpm.clamp(TempoMap::MIN_BPM, TempoMap::MAX_BPM),
        settings
            .max_bpm
            .clamp(TempoMap::MIN_BPM, TempoMap::MAX_BPM)
            .max(settings.min_bpm),
    );
    let step = settings.grid.quarters();
    // Hits in order; those too close to the one before dropped.
    let mut order: Vec<usize> = (0..hits.len())
        .filter(|i| hits[*i].is_finite() && hits[*i] > 0.0)
        .collect();
    order.sort_by(|a, b| hits[*a].total_cmp(&hits[*b]));
    let shortest = 60.0 * step / hi;
    let mut kept: Vec<usize> = Vec::new();
    let mut dropped: Vec<usize> = (0..hits.len())
        .filter(|i| !(hits[*i].is_finite() && hits[*i] > 0.0))
        .collect();
    let mut last = 0.0;
    for i in order {
        if hits[i] - last >= shortest * 0.999 {
            kept.push(i);
            last = hits[i];
        } else {
            dropped.push(i);
        }
    }
    if kept.is_empty() {
        return None;
    }
    // The stretches: from 0 to the first hit, then hit to hit.
    let spans: Vec<f64> = kept
        .iter()
        .scan(0.0, |prev, i| {
            let d = hits[*i] - *prev;
            *prev = hits[*i];
            Some(d)
        })
        .collect();
    // Each stretch's choices: steps n with a tempo in the range.
    let choices: Vec<Vec<(u32, f64)>> = spans
        .iter()
        .map(|d| {
            let tempo = |n: u32| 60.0 * f64::from(n) * step / d;
            let a = ((lo * d / (60.0 * step)).ceil() as u32).max(1);
            let b = (hi * d / (60.0 * step)).floor() as u32;
            let mut c: Vec<(u32, f64)> = (a..=b.max(a)).map(|n| (n, tempo(n))).collect();
            // A stretch too short or too long for the range keeps the
            // count nearest to it.
            c.retain(|(_, t)| *t >= lo * 0.999 && *t <= hi * 1.001);
            if c.is_empty() {
                let n = ((lo + hi) / 2.0 * d / (60.0 * step)).round().max(1.0) as u32;
                c.push((n, tempo(n)));
            }
            c
        })
        .collect();
    let prefer = |t: f64| {
        settings
            .preferred
            .map_or(0.0, |p| PREFER * (t.ln() - p.max(1.0).ln()).powi(2))
    };
    // cost[i][j]: the cheapest way to choice j of stretch i.
    let mut cost: Vec<Vec<f64>> = Vec::with_capacity(choices.len());
    let mut from: Vec<Vec<usize>> = Vec::with_capacity(choices.len());
    // The first stretch is free in the range; the second starts the
    // steadiness.
    cost.push(choices[0].iter().map(|(_, t)| prefer(*t) * 0.1).collect());
    from.push(vec![0; choices[0].len()]);
    for i in 1..choices.len() {
        let mut c = Vec::with_capacity(choices[i].len());
        let mut f = Vec::with_capacity(choices[i].len());
        for (_, t) in &choices[i] {
            let (best, at) = choices[i - 1]
                .iter()
                .enumerate()
                .map(|(k, (_, p))| {
                    let change = if i == 1 {
                        0.0
                    } else {
                        (t.ln() - p.ln()).powi(2)
                    };
                    (cost[i - 1][k] + change, k)
                })
                .fold((f64::INFINITY, 0), |a, b| if b.0 < a.0 { b } else { a });
            c.push(best + prefer(*t));
            f.push(at);
        }
        cost.push(c);
        from.push(f);
    }
    // Back from the cheapest end.
    let last = choices.len() - 1;
    let mut j = cost[last]
        .iter()
        .enumerate()
        .fold(
            (0, f64::INFINITY),
            |a, (k, c)| if *c < a.1 { (k, *c) } else { a },
        )
        .0;
    let mut picked = vec![0usize; choices.len()];
    for i in (0..choices.len()).rev() {
        picked[i] = j;
        j = from[i][j];
    }
    // The map: a step of tempo at the start of every stretch.
    let mut positions = Vec::with_capacity(kept.len());
    let mut tempos = Vec::with_capacity(kept.len());
    let mut at = 0.0;
    let mut map: Option<TempoMap> = None;
    for (i, &k) in picked.iter().enumerate() {
        let (n, t) = choices[i][k];
        let point = TempoPoint {
            position: MusicalTime::from_quarters(at),
            bpm: t,
            curve: TempoCurve::Constant,
        };
        match &mut map {
            None => map = Some(TempoMap::new(t)),
            Some(m) => m.set_point(point),
        }
        tempos.push(t);
        at += f64::from(n) * step;
        positions.push((kept[i], at));
    }
    dropped.sort_unstable();
    Some(HitSolution {
        positions,
        tempo: map.unwrap_or_else(|| TempoMap::new(120.0)),
        dropped,
        tempos,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lands(s: &HitSolution, hits: &[f64]) {
        for (i, q) in &s.positions {
            let t = s.tempo.musical_to_seconds(MusicalTime::from_quarters(*q));
            assert!((t - hits[*i]).abs() < 1e-6, "hit {i}: {t} vs {}", hits[*i]);
            assert!((q - q.round()).abs() < 1e-9 || (q * 2.0 - (q * 2.0).round()).abs() < 1e-9);
        }
    }

    #[test]
    fn hits_on_a_steady_beat_keep_it() {
        // Every 2 s at 120: four beats apart.
        let hits = [1.0, 3.0, 5.0, 7.0, 9.0];
        let s = solve(&hits, &HitSettings::default()).unwrap();
        lands(&s, &hits);
        for t in &s.tempos[1..] {
            assert!((t - s.tempos[1]).abs() < 1e-9, "{:?}", s.tempos);
        }
        assert!(s.largest_change() < 1e-9);
        assert!(s.dropped.is_empty());
    }

    #[test]
    fn uneven_hits_get_the_steadiest_tempo_that_lands_them() {
        // Cuts a film editor made: no tempo hits them all on its own.
        let hits = [2.13, 4.71, 6.02, 9.38, 12.4, 13.9, 17.25];
        let settings = HitSettings {
            min_bpm: 80.0,
            max_bpm: 140.0,
            ..HitSettings::default()
        };
        let s = solve(&hits, &settings).unwrap();
        lands(&s, &hits);
        for t in &s.tempos {
            assert!((80.0..=140.0).contains(t), "{t}");
        }
        // Steadier than any one tempo would need: under 15 % between
        // stretches.
        assert!(s.largest_change() < 0.15, "{:?}", s.tempos);
        // On bars, each stretch a whole number of bars.
        let bars = solve(
            &hits,
            &HitSettings {
                grid: HitGrid::Bars(4.0),
                min_bpm: 40.0,
                max_bpm: 240.0,
                ..settings
            },
        )
        .unwrap();
        lands(&bars, &hits);
        for (_, q) in &bars.positions {
            assert!((q / 4.0 - (q / 4.0).round()).abs() < 1e-9, "{q}");
        }
    }

    #[test]
    fn a_preferred_tempo_is_kept_near_and_too_close_hits_are_left_out() {
        let hits = [2.0, 4.0, 4.05, 6.0];
        let s = solve(
            &hits,
            &HitSettings {
                preferred: Some(90.0),
                ..HitSettings::default()
            },
        )
        .unwrap();
        assert_eq!(s.dropped, [2], "50 ms after a hit: no beat fits");
        lands(&s, &hits);
        // 2 s stretches near 90: three beats (90 BPM) rather than four (120).
        assert!((s.tempos[1] - 90.0).abs() < 1e-9, "{:?}", s.tempos);
        assert!(solve(&[], &HitSettings::default()).is_none());
    }
}
