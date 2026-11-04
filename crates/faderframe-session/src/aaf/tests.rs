use super::*;
use faderframe_automation::CurveShape;

#[test]
fn a_volume_ride_and_a_pan_become_a_level_per_side() {
    let env = ClipEnvelope {
        volume: vec![
            EnvelopePoint::new(0, 0.0),
            EnvelopePoint::new(48_000, -12.0),
        ],
        pan: vec![EnvelopePoint::new(0, 0.0), EnvelopePoint::new(48_000, 0.5)],
    };
    let left = channel_envelope(Some(&env), 48_000, Some(0));
    let right = channel_envelope(Some(&env), 48_000, Some(1));
    // The far side fades out, the near one keeps the volume alone.
    assert!((left.last().unwrap().1 - (-12.0 + 20.0 * 0.5f64.log10())).abs() < 0.05);
    assert!((right.last().unwrap().1 + 12.0).abs() < 0.05);
    // Points enough to follow the dB ramp within a tenth of a dB.
    let amp = |d: f64| 10f64.powf(d / 20.0);
    for w in right.windows(2) {
        let mid = (w[0].0 + w[1].0) / 2;
        let straight = amp(w[0].1) + (amp(w[1].1) - amp(w[0].1)) * 0.5;
        let want = env.gain_at(mid as f64) as f64;
        assert!((amp_db(straight) - amp_db(want)).abs() < 0.15, "{w:?}");
    }
    // A mono track leaves the pan out; no envelope, nothing.
    assert_eq!(
        channel_envelope(Some(&env), 48_000, None).len(),
        right.len()
    );
    assert!(channel_envelope(None, 48_000, Some(0)).is_empty());
    let flat = ClipEnvelope {
        volume: vec![EnvelopePoint::new(0, 0.0)],
        pan: Vec::new(),
    };
    assert!(channel_envelope(Some(&flat), 1000, None).is_empty());
}

#[test]
fn levels_straight_in_amplitude_become_volume_points_straight_in_db() {
    let pts = volume_points(&[(0, 0.0), (48_000, -40.0)]);
    assert!(pts.len() > 2);
    assert!(pts.iter().all(|p| p.shape == CurveShape::Linear));
    let env = ClipEnvelope {
        volume: pts,
        pan: Vec::new(),
    };
    let amp = |d: f64| 10f64.powf(d / 20.0);
    for x in [6_000, 24_000, 40_000] {
        let want = amp_db(1.0 + (amp(-40.0) - 1.0) * x as f64 / 48_000.0);
        let got = f64::from(env.gain_at(x as f64)).log10() * 20.0;
        assert!((got - want).abs() < 0.2, "{x}: {got} vs {want}");
    }
}

#[test]
fn fade_shapes_map_both_ways() {
    for s in FadeShape::ALL {
        assert_eq!(shape(curve(s)), s);
    }
}
