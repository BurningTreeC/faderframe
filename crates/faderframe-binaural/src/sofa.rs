//! Heads from SOFA files (AES69): HRIR sets measured or simulated for a
//! person (Mesh2HRTF from a scan of the ears, a lab measurement) or taken
//! from a database. Read with `hdf5-reader` (pure Rust): the conventions
//! SimpleFreeFieldHRIR and GeneralFIR, impulse responses `Data.IR`
//! (measurements × 2 ears × taps), `Data.SamplingRate`, `Data.Delay`
//! (per ear, added in front) and `SourcePosition` (spherical in degrees or
//! Cartesian). Each speaker direction takes the nearest measurement; the
//! responses are prepared as `scripts/binaural_hrirs.py` bakes the built-in
//! heads (trimmed by their common onset, faded, the front centre at unity
//! at 1 kHz), so heads compare at the same level.

use crate::head::Head;
use crate::{BinauralError, DIRECTIONS, Hrirs, Pair, magnitude_at};
use hdf5_reader::{Dataset, Hdf5File};
use std::path::Path;

/// Longest response kept (taps at 48 kHz; twice that above 50 kHz).
const MAX_TAPS: usize = 1024;

fn err(path: &Path, what: impl std::fmt::Display) -> BinauralError {
    BinauralError::Sofa(format!("{}: {what}", path.display()))
}

/// A dataset's values as f64 (stored as 64- or 32-bit floats) and its
/// shape.
fn values(d: &Dataset) -> Result<(Vec<f64>, Vec<usize>), String> {
    let shape: Vec<usize> = d.shape().iter().map(|&n| n as usize).collect();
    let v = match d.read_array::<f64>() {
        Ok(a) => a.iter().copied().collect(),
        Err(_) => d
            .read_array::<f32>()
            .map_err(|e| e.to_string())?
            .iter()
            .map(|&x| f64::from(x))
            .collect(),
    };
    Ok((v, shape))
}

fn unit(az: f64, el: f64) -> [f64; 3] {
    let (a, e) = (az.to_radians(), el.to_radians());
    [e.cos() * a.cos(), e.cos() * a.sin(), e.sin()]
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

pub(crate) fn read(path: &Path) -> Result<Head, BinauralError> {
    let f = Hdf5File::open(path).map_err(|e| err(path, e))?;
    let root = f.root_group().map_err(|e| err(path, e))?;
    let attr = |name: &str| {
        root.attributes()
            .ok()?
            .into_iter()
            .find(|a| a.name == name)
            .and_then(|a| a.read_string().ok())
    };
    let conventions = attr("SOFAConventions").unwrap_or_default();
    if !conventions.is_empty()
        && !["SimpleFreeFieldHRIR", "GeneralFIR", "SimpleFreeFieldHRIR "]
            .contains(&conventions.as_str())
    {
        return Err(err(
            path,
            format!("{conventions} is not a set of head-related impulse responses"),
        ));
    }
    if let Some(t) = attr("DataType")
        && !t.starts_with("FIR")
    {
        return Err(err(
            path,
            format!("data type {t} (impulse responses wanted)"),
        ));
    }
    let ds = |name: &str| {
        f.dataset(name)
            .map_err(|e| err(path, format!("{name}: {e}")))
    };
    let (ir, shape) = values(&ds("Data.IR")?).map_err(|e| err(path, e))?;
    let [m, r, n] = shape[..] else {
        return Err(err(path, "Data.IR is not measurements × receivers × taps"));
    };
    if r < 2 || m == 0 || n == 0 {
        return Err(err(path, "Data.IR needs two ears"));
    }
    let rate = values(&ds("Data.SamplingRate")?)
        .map_err(|e| err(path, e))?
        .0
        .first()
        .copied()
        .unwrap_or(0.0);
    if !(8_000.0..=384_000.0).contains(&rate) {
        return Err(err(path, format!("sampling rate {rate}")));
    }
    // Delays per ear: one pair for all, or one per measurement.
    let delays = f
        .dataset("Data.Delay")
        .ok()
        .and_then(|d| values(&d).ok())
        .unwrap_or((vec![0.0, 0.0], vec![1, 2]));
    let delay = |i: usize, ear: usize| {
        let (v, s) = &delays;
        let rows = s.first().copied().unwrap_or(1);
        let cols = s.get(1).copied().unwrap_or(1).max(1);
        let row = if rows > 1 { i.min(rows - 1) } else { 0 };
        v.get(row * cols + ear.min(cols - 1))
            .copied()
            .unwrap_or(0.0)
            .max(0.0)
            .round() as usize
    };
    let pos_ds = ds("SourcePosition")?;
    let cartesian = pos_ds
        .attribute("Type")
        .ok()
        .and_then(|a| a.read_string().ok())
        .is_some_and(|t| t.eq_ignore_ascii_case("cartesian"));
    let (pos, pshape) = values(&pos_ds).map_err(|e| err(path, e))?;
    if pshape.get(1) != Some(&3) || pshape[0] != m {
        return Err(err(path, "SourcePosition does not list every measurement"));
    }
    let dirs: Vec<[f64; 3]> = pos
        .as_chunks::<3>()
        .0
        .iter()
        .map(|p| {
            if cartesian {
                let len = (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt().max(1e-12);
                [p[0] / len, p[1] / len, p[2] / len]
            } else {
                unit(p[0], p[1])
            }
        })
        .collect();
    let block = if rate > 50_000.0 { 128 } else { 64 };
    let max_taps = if rate > 50_000.0 {
        2 * MAX_TAPS
    } else {
        MAX_TAPS
    };
    let mut worst = 0.0f64;
    let mut cuts = Vec::with_capacity(DIRECTIONS.len());
    for (az, el) in DIRECTIONS {
        let want = unit(f64::from(az), f64::from(el));
        let (i, d) = dirs
            .iter()
            .enumerate()
            .map(|(i, v)| (i, dot(*v, want)))
            .fold((0, f64::MIN), |b, x| if x.1 > b.1 { x } else { b });
        worst = worst.max(d.clamp(-1.0, 1.0).acos().to_degrees());
        let ear = |e: usize| -> Vec<f32> {
            let at = (i * r + e) * n;
            let mut v = vec![0.0f32; delay(i, e)];
            v.extend(ir[at..at + n].iter().map(|&x| x as f32));
            v
        };
        cuts.push([ear(0), ear(1)]);
    }
    // Trimmed by each pair's common onset (the interaural delay stays),
    // cut to whole blocks and faded.
    let mut pairs = Vec::with_capacity(cuts.len());
    for [l, rr] in cuts {
        let peak = l.iter().chain(&rr).fold(0.0f32, |p, v| p.max(v.abs()));
        let first = |x: &[f32]| x.iter().position(|v| v.abs() > peak * 0.01).unwrap_or(0);
        let start = first(&l).min(first(&rr)).saturating_sub(8);
        let len = l.len().max(rr.len()).saturating_sub(start);
        let taps = ((len / block) * block).clamp(block, max_taps);
        let cut = |x: &[f32]| {
            let mut v: Vec<f32> = x.iter().skip(start).take(taps).copied().collect();
            v.resize(taps, 0.0);
            // A quarter cosine² down to zero, as the bake script fades.
            let fade = 16.min(taps);
            for k in 0..fade {
                let t = k as f64 / (fade - 1).max(1) as f64;
                let g = (std::f64::consts::FRAC_PI_2 * t).cos().powi(2) as f32;
                v[taps - fade + k] *= g;
            }
            v
        };
        pairs.push(Pair {
            left: cut(&l),
            right: cut(&rr),
        });
    }
    // Every pair as long as the longest; the front centre at unity at 1 kHz.
    let taps = pairs.iter().map(|p| p.left.len()).max().unwrap_or(block);
    // At the bin nearest 1 kHz of a 16384-point transform, as baked.
    let at = (1000.0 / rate * 16_384.0).round() * rate / 16_384.0;
    let norm =
        0.5 * (magnitude_at(&pairs[0].left, at, rate) + magnitude_at(&pairs[0].right, at, rate));
    if norm.is_nan() || norm <= 1e-9 {
        return Err(err(path, "the front direction is silent"));
    }
    let g = (1.0 / norm) as f32;
    for p in &mut pairs {
        for x in [&mut p.left, &mut p.right] {
            x.resize(taps, 0.0);
            x.iter_mut().for_each(|v| *v *= g);
        }
    }
    let name = attr("Title")
        .filter(|t| !t.trim().is_empty() && t.trim() != "HRTF")
        .or_else(|| path.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "SOFA".into());
    let note = (worst > 10.0)
        .then(|| format!("its measurements are up to {worst:.0}° from a speaker's direction"));
    Ok(Head::new(
        format!("sofa:{}", path.display()),
        name,
        vec![Hrirs {
            rate: rate.round() as u32,
            taps,
            pairs,
        }],
        note,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data(name: &str) -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/data")
            .join(name)
    }

    /// A small SOFA file (`tests/data/tiny.sofa`, made by
    /// `tests/data/make_tiny_sofa.py`): every direction's left ear an
    /// impulse whose position names the measurement, the right ear one
    /// sample later at half the level, with a delay of 3 samples.
    #[test]
    fn a_sofa_file_gives_the_nearest_measurement_for_each_speaker() {
        let h = Head::from_sofa(&data("tiny.sofa")).unwrap();
        assert_eq!(h.rates(), [48_000]);
        let s = h.at(48_000).unwrap();
        assert_eq!(s.pairs.len(), DIRECTIONS.len());
        assert_eq!(s.taps, 64);
        // Left: the impulse after its delay of 3 (less than the onset
        // margin, so nothing is trimmed); right: one sample later, half as
        // loud; the two ears' mean at unity.
        for p in &s.pairs {
            let l = p.left.iter().position(|v| v.abs() > 0.5).unwrap();
            let r = p.right.iter().position(|v| v.abs() > 0.1).unwrap();
            assert_eq!((l, r), (3, 4));
            assert!((p.right[4] / p.left[3] - 0.5).abs() < 1e-6);
            assert!((p.left[3] - 4.0 / 3.0).abs() < 1e-3, "{}", p.left[3]);
        }
        assert!(h.id().starts_with("sofa:"));
        assert_eq!(h.name(), "tiny");
        assert!(h.note().is_some(), "its few directions are far apart");
        // Resampled for other rates.
        assert_eq!(h.at(44_100).unwrap().rate, 44_100);
    }

    #[test]
    fn files_that_are_not_hrirs_are_refused() {
        let e = Head::from_sofa(&data("missing.sofa")).unwrap_err();
        assert!(e.to_string().contains("missing.sofa"), "{e}");
        let e = Head::from_sofa(&data("make_tiny_sofa.py")).unwrap_err();
        assert!(matches!(e, BinauralError::Sofa(_)));
    }

    /// `FADERFRAME_TEST_SOFA=<file.sofa>`: reads a real file (SADIE II's
    /// D1 48 kHz file gives exactly the baked KU100).
    #[test]
    #[ignore = "needs a SOFA file"]
    fn a_real_sofa_file_reads() {
        let Ok(path) = std::env::var("FADERFRAME_TEST_SOFA") else {
            return;
        };
        let h = Head::from_sofa(Path::new(&path)).unwrap();
        let rate = h.rates()[0];
        println!(
            "{}: {} at {rate} Hz, {} taps, note {:?}",
            h.id(),
            h.name(),
            h.at(rate).unwrap().taps,
            h.note()
        );
        if path.contains("D1_48K") {
            let ours = h.at(48_000).unwrap();
            let baked = Head::builtin("ku100").unwrap().at(48_000).unwrap();
            assert_eq!(ours.taps, baked.taps);
            for (a, b) in ours.pairs.iter().zip(&baked.pairs) {
                for (x, y) in a
                    .left
                    .iter()
                    .zip(&b.left)
                    .chain(a.right.iter().zip(&b.right))
                {
                    assert!((x - y).abs() < 1e-5, "{x} {y}");
                }
            }
        }
    }
}
