#![allow(clippy::unwrap_used)]

use super::*;

const RATE: f64 = 48_000.0;

fn hz(key: f64) -> f64 {
    440.0 * 2f64.powf((key - 69.0) / 12.0)
}

/// A plucked string: partials shaped by where it is plucked, the higher
/// ones dying sooner and running sharp (stiffness), a short noise burst.
fn pluck(x: &mut [f32], key: f64, at: f64, until: f64, gain: f64) {
    let f0 = hz(key);
    let (i0, i1) = ((at * RATE) as usize, ((until * RATE) as usize).min(x.len()));
    let mut seed = 0x2545_f491_u32.wrapping_add((key * 1000.0) as u32);
    for (j, o) in x.iter_mut().enumerate().take(i1).skip(i0) {
        let t = (j - i0) as f64 / RATE;
        let mut v = 0.0;
        for m in 1..=30 {
            let h = f64::from(m);
            let f = h * f0 * (1.0 + 1e-4 * h * h).sqrt();
            if f > 20_000.0 {
                break;
            }
            let shape = (std::f64::consts::PI * h * 0.18).sin().abs() / h;
            let decay = (-t * (0.8 + 0.25 * h)).exp();
            v += shape * decay * (2.0 * std::f64::consts::PI * f * t).sin();
        }
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let noise = (f64::from(seed >> 8) / f64::from(1u32 << 24) - 0.5) * (-t / 0.004).exp();
        let env = (t / 0.002).min(1.0) * ((until - at - t) / 0.01).clamp(0.0, 1.0);
        *o += ((v + 0.3 * noise) * env * gain) as f32;
    }
}

/// Events (decided at, placed at, event), absolute frames.
fn track(x: &[f32], config: VoiceConfig) -> Vec<(i64, i64, VoiceEvent)> {
    let mut t = PolyTracker::new(RATE, config);
    let mut out = Vec::new();
    for (b, chunk) in x.chunks(256).enumerate() {
        let base = (b * 256) as i64;
        t.process(chunk, |now, at, e| {
            out.push((base + now as i64, base + at as i64, e));
        });
    }
    t.reset(|e| out.push((x.len() as i64, x.len() as i64, e)));
    out
}

/// Notes: (key, decided s, placed s, end s).
fn notes(ev: &[(i64, i64, VoiceEvent)]) -> Vec<(u8, f64, f64, f64)> {
    let mut open: Vec<(u8, i64, i64)> = Vec::new();
    let mut out = Vec::new();
    for &(now, at, e) in ev {
        match e {
            VoiceEvent::NoteOn { key, .. } => {
                assert!(open.iter().all(|o| o.0 != key), "{key} on twice");
                open.push((key, now, at));
            }
            VoiceEvent::NoteOff { key } => {
                let i = open
                    .iter()
                    .position(|o| o.0 == key)
                    .unwrap_or_else(|| panic!("{key} off unopened"));
                let (k, n, a) = open.remove(i);
                out.push((k, n as f64 / RATE, a as f64 / RATE, at as f64 / RATE));
            }
            VoiceEvent::Bend { .. } => {}
        }
    }
    assert!(open.is_empty());
    out.sort_by(|a, b| a.2.total_cmp(&b.2).then(a.0.cmp(&b.0)));
    out
}

fn keys(n: &[(u8, f64, f64, f64)]) -> Vec<u8> {
    let mut k: Vec<u8> = n.iter().map(|n| n.0).collect();
    k.sort_unstable();
    k
}

#[test]
fn single_plucked_notes_are_heard_across_the_range() {
    for key in [40.0, 45.0, 52.0, 59.0, 64.0, 69.0, 76.0, 84.0] {
        let mut x = vec![0.0f32; (1.6 * RATE) as usize];
        pluck(&mut x, key, 0.2, 1.4, 0.3);
        let got = notes(&track(&x, VoiceConfig::default()));
        assert_eq!(keys(&got), [key as u8], "{key}: {got:?}");
        let (_, decided, placed, end) = got[0];
        eprintln!(
            "key {key}: decided {:.1} ms after, placed {:+.1} ms, ends {:+.1} ms",
            (decided - 0.2) * 1000.0,
            (placed - 0.2) * 1000.0,
            (end - 1.4) * 1000.0
        );
        assert!(
            decided - 0.2 < window_seconds(Responsiveness::Balanced),
            "{key}: {decided}"
        );
        assert!((placed - 0.2).abs() < 0.02, "{key}: placed {placed}");
    }
}

#[test]
fn chords_become_their_notes() {
    let chords: [&[f64]; 5] = [
        &[48.0, 52.0, 55.0],                   // C major
        &[45.0, 52.0, 57.0, 60.0, 64.0],       // A minor (open)
        &[40.0, 47.0, 52.0],                   // E5 power chord
        &[40.0, 47.0, 52.0, 56.0, 59.0, 64.0], // E major (open)
        &[43.0, 47.0, 50.0, 55.0, 59.0, 67.0], // G major (open)
    ];
    let mut results = Vec::new();
    for (response, chord) in Responsiveness::ALL
        .into_iter()
        .flat_map(|r| chords.map(|c| (r, c)))
    {
        let mut x = vec![0.0f32; (2.0 * RATE) as usize];
        for (i, k) in chord.iter().enumerate() {
            // Strummed: 8 ms between strings.
            pluck(&mut x, *k, 0.2 + 0.008 * i as f64, 1.7, 0.18);
        }
        let got = notes(&track(&x, VoiceConfig::default().with(response)));
        let want: Vec<u8> = chord.iter().map(|k| *k as u8).collect();
        let heard = keys(&got);
        eprintln!("{response:?} {want:?}: heard {heard:?}");
        for n in &got {
            eprintln!(
                "  {} decided {:.1} ms, placed {:+.1} ms, to {:.2} s",
                n.0,
                (n.1 - 0.2) * 1000.0,
                (n.2 - 0.2) * 1000.0,
                n.3
            );
        }
        results.push((response, want, heard, got));
    }
    for (response, want, heard, got) in results {
        // Every note of a chord without doublings; of one with octave
        // doublings, every pitch class (a doubling may hide in its note an
        // octave down), and never a note it does not have.
        let doubled = |k: &u8| want.iter().any(|o| o != k && o % 12 == k % 12);
        for k in &want {
            assert!(
                heard.contains(k) || (doubled(k) && heard.iter().any(|h| h % 12 == k % 12)),
                "{want:?}: {k} missing in {heard:?}"
            );
        }
        // Simultaneous octave doublings share their partials. The tracker
        // deliberately merges ambiguous doubles instead of inventing high
        // notes from a lower string's harmonics.
        let wrong: Vec<&u8> = heard.iter().filter(|k| !want.contains(k)).collect();
        assert!(wrong.is_empty(), "{want:?}: {heard:?}");
        // Each played once (no flicker).
        for k in &want {
            assert!(
                got.iter().filter(|n| n.0 == *k).count() <= 1,
                "{k} twice: {got:?}"
            );
        }
        // Placed where strummed.
        for n in &got {
            let tolerance = if doubled(&n.0) {
                1.5 * window_seconds(response)
            } else {
                0.5 * window_seconds(response)
            };
            let start = 0.2 + 0.008 * want.iter().position(|k| *k == n.0).unwrap() as f64;
            assert!(
                (n.2 - start).abs() < tolerance,
                "{response:?}: {} placed {:.3}",
                n.0,
                n.2
            );
            assert!(n.3 >= n.2, "{} ends before its start", n.0);
        }
    }
}

#[test]
fn notes_added_to_a_ringing_chord_are_heard_and_the_others_hold() {
    let mut x = vec![0.0f32; (2.4 * RATE) as usize];
    // An arpeggio: C3, E3, G3, C4, each ringing on.
    for (i, k) in [48.0, 52.0, 55.0, 60.0].iter().enumerate() {
        pluck(&mut x, *k, 0.2 + 0.4 * i as f64, 2.2, 0.25);
    }
    let got = notes(&track(&x, VoiceConfig::default()));
    eprintln!("{got:?}");
    assert_eq!(
        got.iter().map(|n| n.0).collect::<Vec<_>>(),
        [48, 52, 55, 60],
        "{got:?}"
    );
    for (n, start) in got.iter().zip([0.2, 0.6, 1.0, 1.4]) {
        // (C4, C3's octave, waits a little more: it could be C3's harmonic.)
        assert!(
            n.1 - start < window_seconds(Responsiveness::Balanced) + 0.04,
            "{} decided {:.3}",
            n.0,
            n.1
        );
        assert!((n.2 - start).abs() < 0.03, "{} placed {:.3}", n.0, n.2);
        assert!(n.3 > 2.0, "{} held to {:.2}", n.0, n.3);
    }
}

#[test]
fn a_string_plucked_again_plays_again() {
    let mut x = vec![0.0f32; (1.6 * RATE) as usize];
    pluck(&mut x, 57.0, 0.2, 0.7, 0.3);
    pluck(&mut x, 57.0, 0.7, 1.4, 0.3);
    let got = notes(&track(&x, VoiceConfig::default()));
    assert_eq!(keys(&got), [57, 57], "{got:?}");
    assert!((got[1].2 - 0.7).abs() < 0.03, "{got:?}");
}

#[test]
fn silence_and_noise_play_nothing() {
    let mut x = vec![0.0f32; RATE as usize];
    pluck(&mut x, 57.0, 0.2, 0.8, 0.0005);
    assert!(notes(&track(&x, VoiceConfig::default())).is_empty());
    let mut seed = 1u32;
    let noise: Vec<f32> = (0..RATE as usize)
        .map(|_| {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (f64::from(seed >> 8) / f64::from(1u32 << 24) - 0.5) as f32 * 0.1
        })
        .collect();
    let got = notes(&track(&noise, VoiceConfig::default()));
    assert!(got.len() <= 1, "noise: {got:?}");
}

#[test]
fn a_chord_moves_to_the_next() {
    // C major, then A minor: the common notes (C, E) are played again.
    let mut x = vec![0.0f32; (2.4 * RATE) as usize];
    for k in [48.0, 52.0, 55.0] {
        pluck(&mut x, k, 0.2, 1.1, 0.2);
    }
    for k in [45.0, 52.0, 57.0, 60.0] {
        pluck(&mut x, k, 1.1, 2.2, 0.2);
    }
    let got = notes(&track(&x, VoiceConfig::default()));
    eprintln!("{got:?}");
    let before: Vec<u8> = keys(
        &got.iter()
            .filter(|n| n.2 < 1.0)
            .copied()
            .collect::<Vec<_>>(),
    );
    let after: Vec<u8> = keys(
        &got.iter()
            .filter(|n| n.2 > 1.0)
            .copied()
            .collect::<Vec<_>>(),
    );
    assert_eq!(before, [48, 52, 55], "{got:?}");
    let right = [45u8, 52, 57, 60]
        .iter()
        .filter(|k| after.contains(k))
        .count();
    assert!(right >= 3, "{after:?}");
    // G stops with its chord.
    let g = got
        .iter()
        .find(|n| n.0 == 55)
        .unwrap_or_else(|| panic!("{got:?}"));
    assert!((g.3 - 1.1).abs() < 0.05, "G ends {:.3}", g.3);
}

#[test]
fn threshold_scale_and_reset_apply_to_every_note() {
    let mut x = vec![0.0; RATE as usize];
    for key in [49.0, 53.0, 56.0] {
        pluck(&mut x, key, 0.1, 0.95, 0.2);
    }
    let quiet = VoiceConfig {
        threshold_db: -10.0,
        ..VoiceConfig::default()
    };
    assert!(
        notes(&track(&x, quiet)).is_empty(),
        "the configured gate applies to notes"
    );
    let config = VoiceConfig {
        scale: 0b1010_1011_0101,
        ..VoiceConfig::default()
    }; // C major
    let mut t = PolyTracker::new(RATE, config);
    let mut events = Vec::new();
    t.process(&x, |_, _, e| events.push(e));
    let sounding: Vec<_> = t.sounding().collect();
    assert!(!sounding.is_empty());
    for &key in &sounding {
        assert_ne!(
            config.scale & (1 << (key % 12)),
            0,
            "key {key} outside scale"
        );
    }
    let mut released = Vec::new();
    t.reset(|e| {
        if let VoiceEvent::NoteOff { key } = e {
            released.push(key);
        }
    });
    assert_eq!(released, sounding);
    assert_eq!(t.sounding().count(), 0);
    t.process(&vec![0.0; RATE as usize], |_, _, e| {
        panic!("stale audio after reset: {e:?}")
    });
}

#[test]
fn callback_size_does_not_change_notes_or_timestamps() {
    let mut x = vec![0.0; RATE as usize];
    for key in [48.0, 52.0, 55.0] {
        pluck(&mut x, key, 0.123, 0.8, 0.2);
    }
    let expected = track(&x, VoiceConfig::default());
    for block in [1, 17, 64, 511, 2048] {
        let mut t = PolyTracker::new(RATE, VoiceConfig::default());
        let mut events = Vec::new();
        for (b, chunk) in x.chunks(block).enumerate() {
            let base = (b * block) as i64;
            t.process(chunk, |now, at, e| {
                events.push((base + now as i64, base + at as i64, e))
            });
        }
        t.reset(|e| events.push((x.len() as i64, x.len() as i64, e)));
        assert_eq!(events, expected, "{block}-frame callbacks");
    }
}

#[test]
fn device_sample_rates_find_the_same_chord() {
    let mut original = vec![0.0; RATE as usize];
    for key in [48.0, 52.0, 55.0] {
        pluck(&mut original, key, 0.15, 0.8, 0.2);
    }
    for rate in [44_100.0, 48_000.0, 88_200.0, 96_000.0, 192_000.0] {
        let x: Vec<_> = (0..rate as usize)
            .map(|i| {
                let at = i as f64 * RATE / rate;
                let n = at as usize;
                let a = original[n];
                let b = original.get(n + 1).copied().unwrap_or(a);
                a + (b - a) * at.fract() as f32
            })
            .collect();
        let mut t = PolyTracker::new(rate, VoiceConfig::default());
        let mut keys = Vec::new();
        t.process(&x, |_, _, e| {
            if let VoiceEvent::NoteOn { key, .. } = e {
                keys.push(key);
            }
        });
        keys.sort_unstable();
        assert_eq!(keys, [48, 52, 55], "sample rate {rate}");
        assert_eq!(t.sounding().count(), 0, "all notes released at {rate}");
    }
}

/// Run separately in release mode; synthesis is outside the measured region.
#[test]
#[ignore = "manual realtime performance measurement"]
fn callback_cost() {
    let mut x = vec![0.0; (2.0 * RATE) as usize];
    for (i, key) in [40.0, 47.0, 52.0, 56.0, 59.0, 64.0].into_iter().enumerate() {
        pluck(&mut x, key, 0.2 + 0.008 * i as f64, 1.7, 0.18);
    }
    for response in Responsiveness::ALL {
        let mut t = PolyTracker::new(RATE, VoiceConfig::default().with(response));
        let mut times = Vec::with_capacity(x.len().div_ceil(64));
        for chunk in x.chunks(64) {
            let start = std::time::Instant::now();
            t.process(chunk, |_, _, _| {});
            times.push(start.elapsed().as_secs_f64() * 1e6);
        }
        times.sort_by(f64::total_cmp);
        let mean = times.iter().sum::<f64>() / times.len() as f64;
        eprintln!(
            "{response:?}: mean {mean:.1} µs, p99 {:.1} µs, max {:.1} µs; 64-frame budget {:.1} µs",
            times[times.len() * 99 / 100],
            times[times.len() - 1],
            64e6 / RATE
        );
    }
}

#[test]
#[ignore]
fn probe() {
    let chord: Vec<f64> = std::env::var("CHORD")
        .map(|v| v.split(',').map(|k| k.parse().unwrap()).collect())
        .unwrap_or_else(|_| vec![48.0, 52.0, 55.0]);
    let when: f64 = std::env::var("WHEN").map_or(0.6, |v| v.parse().unwrap());
    let mut x = vec![0.0f32; (2.0 * RATE) as usize];
    for (i, k) in chord.iter().enumerate() {
        pluck(&mut x, *k, 0.2 + 0.008 * i as f64, 1.7, 0.18);
    }
    let mut t = PolyTracker::new(RATE, VoiceConfig::default());
    let at = (when * RATE) as usize;
    t.process(&x[..at], |_, _, _| {});
    DEBUG.with(|d| d.set(true));
    let step = t.hop * t.decimate;
    t.process(&x[at..at + step], |_, _, e| eprintln!("  event {e:?}"));
}

#[test]
#[ignore]
fn probe_frames() {
    let chord: Vec<f64> = std::env::var("CHORD")
        .map(|v| v.split(',').map(|k| k.parse().unwrap()).collect())
        .unwrap_or_else(|_| vec![48.0, 52.0, 55.0]);
    let mut x = vec![0.0f32; (2.0 * RATE) as usize];
    for (i, k) in chord.iter().enumerate() {
        pluck(&mut x, *k, 0.2 + 0.008 * i as f64, 1.7, 0.18);
    }
    let mut t = PolyTracker::new(RATE, VoiceConfig::default());
    let step = t.hop * t.decimate;
    for (n, chunk) in x.chunks(step).enumerate() {
        t.process(chunk, |_, _, _| {});
        let found: Vec<String> = t.found[..t.found_count]
            .iter()
            .map(|f| format!("{:.1}({:.0})", f.pitch, f.level_db))
            .collect();
        let time = (n * step) as f64 / RATE;
        if (0.18..1.8).contains(&time) {
            eprintln!("{time:.3} {}", found.join(" "));
        }
    }
}

#[test]
#[ignore]
fn probe_onsets() {
    let chord: Vec<f64> = std::env::var("CHORD")
        .map(|v| v.split(',').map(|k| k.parse().unwrap()).collect())
        .unwrap_or_else(|_| vec![40.0]);
    let mut x = vec![0.0f32; (2.4 * RATE) as usize];
    match std::env::var("NOTES") {
        // key@start-end,…
        Ok(v) => {
            for n in v.split(',') {
                let (k, span) = n.split_once('@').unwrap();
                let (a, b) = span.split_once('-').unwrap();
                pluck(
                    &mut x,
                    k.parse().unwrap(),
                    a.parse().unwrap(),
                    b.parse().unwrap(),
                    0.2,
                );
            }
        }
        Err(_) => {
            for (i, k) in chord.iter().enumerate() {
                pluck(&mut x, *k, 0.2 + 0.4 * i as f64, 2.2, 0.25);
            }
        }
    }
    let mut t = PolyTracker::new(RATE, VoiceConfig::default());
    let step = t.hop * t.decimate;
    let mut last = t.last_onset;
    let debug_at: Option<f64> = std::env::var("DEBUG_AT").ok().map(|v| v.parse().unwrap());
    for (n, chunk) in x.chunks(step).enumerate() {
        let time = (n * step) as f64 / RATE;
        DEBUG.with(|d| d.set(debug_at.is_some_and(|a| (time - a).abs() < 0.0001)));
        if debug_at.is_some_and(|a| (time - a).abs() < 0.0001) {
            eprintln!("--- frame {time:.3}");
        }
        t.process(chunk, |_, _, e| {
            eprintln!("  {:.3} {e:?}", (n * step) as f64 / RATE)
        });
        if t.last_onset != last {
            last = t.last_onset;
            eprintln!("onset at {:.4}", last as f64 / RATE);
        }
        let time = (n * step) as f64 / RATE;
        let range: Vec<f64> = std::env::var("HIST")
            .map(|v| v.split('-').map(|x| x.parse().unwrap()).collect())
            .unwrap_or_default();
        if range.len() == 2 && (range[0]..range[1]).contains(&time) {
            let slots: Vec<String> = t
                .slots
                .iter()
                .filter(|s| s.on)
                .map(|s| format!("{}:{:.1}", s.key, s.history[(s.heard.max(1) - 1) % HISTORY]))
                .collect();
            let found: Vec<String> = t.found[..t.found_count]
                .iter()
                .map(|f| format!("{:.2}({:.0})", f.pitch, f.level_db))
                .collect();
            eprintln!(
                "{time:.3} {} | {} | fast/slow {:.2} fast/peak {:.3} armed {}",
                slots.join(" "),
                found.join(" "),
                t.fast / t.slow.max(1e-30),
                t.fast / t.peak.max(1e-30),
                t.armed
            );
        }
    }
}
