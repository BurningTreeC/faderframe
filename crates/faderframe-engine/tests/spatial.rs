//! VBAP into 5.1 and 7.1 through the engine, stereo sources' ways into a
//! bed, the LFE send, azimuth automation across speaker regions, the
//! project's downmix (live), and switching to headphones without a
//! rebuild (crossfaded).
#![allow(clippy::unwrap_used)]
mod common;
use common::{TestProject, approx};
use faderframe_audio_files::AudioData;
use faderframe_automation::{
    AutomationCurve, AutomationLane, AutomationMode, AutomationPoint, AutomationTarget, CurveShape,
};
use faderframe_core::TrackId;
use faderframe_core::surround::{Downmix, DownmixMethod, StereoSource, SurroundPanner};
use faderframe_core::vbap::Vbap;
use faderframe_core::{AudioSourceId, ChannelLayout, SurroundFormat, SurroundPan, SurroundParam};
use faderframe_engine::EngineConfig;
use faderframe_engine::offline::OfflineRenderer;
use faderframe_project::{Command, History, Impact, OutputRouting, TrackKind};
use faderframe_timeline::MusicalTime;

const SR: u32 = 48_000;
/// One quarter note at 120 bpm.
const Q: usize = 24_000;

fn render(tp: &TestProject, block: usize, outputs: usize, frames: usize) -> Vec<Vec<f32>> {
    let mut r = OfflineRenderer::new(
        &tp.project,
        &tp.sources,
        EngineConfig::default(),
        block,
        outputs,
    )
    .unwrap();
    r.play_from(0).unwrap();
    r.render(frames)
}

fn vbap(azimuth: f32) -> SurroundPan {
    SurroundPan {
        panner: SurroundPanner::Vbap,
        azimuth,
        ..SurroundPan::default()
    }
}

/// A project whose master is `format`, with a track of `layout` playing
/// `source`.
fn into(
    format: SurroundFormat,
    layout: ChannelLayout,
    source: AudioData,
) -> (TestProject, TrackId) {
    let mut tp = TestProject::new(SR);
    let master = tp.master();
    tp.project.track_mut(master).unwrap().layout = ChannelLayout::Surround(format);
    let t = tp.track(TrackKind::Audio, "Source", layout);
    let frames = source.frames();
    let src = tp.source(source);
    tp.clip(t, src, MusicalTime::ZERO, frames as i64);
    (tp, t)
}

fn dc(channels: &[f32], frames: usize) -> AudioData {
    AudioData::from_channels(SR, channels.iter().map(|v| vec![*v; frames]).collect())
}

fn label(f: SurroundFormat, l: &str) -> usize {
    f.speakers().iter().position(|s| s.label == l).unwrap()
}

#[test]
fn a_mono_track_plays_where_vbap_puts_it_in_51_and_71() {
    for f in [SurroundFormat::S51, SurroundFormat::S71] {
        for az in [0.0f32, 30.0, 60.0, 90.0, 110.0, 150.0, 180.0, -45.0, -135.0] {
            let (mut tp, t) = into(f, ChannelLayout::Mono, dc(&[0.5], SR as usize));
            tp.project.track_mut(t).unwrap().surround = vbap(az);
            let out = render(&tp, 256, f.channels(), 4096);
            let mut want = vec![0.0; f.channels()];
            Vbap::of(f).gains(az, 0.0, &mut want);
            for (c, w) in want.iter().enumerate() {
                assert!(
                    approx(out[c][4000], 0.5 * w, 1e-5),
                    "{} {az}° ch {c}: {} vs {}",
                    f.name(),
                    out[c][4000],
                    0.5 * w
                );
            }
        }
    }
    // At a speaker: that speaker only, the rest (the LFE too) silent.
    let f = SurroundFormat::S71;
    let (mut tp, t) = into(f, ChannelLayout::Mono, dc(&[0.5], SR as usize));
    tp.project.track_mut(t).unwrap().surround = vbap(90.0);
    let out = render(&tp, 256, 8, 4096);
    for (c, ch) in out.iter().enumerate() {
        let want = if c == label(f, "Lss") { 0.5 } else { 0.0 };
        assert!(ch[1000..].iter().all(|v| (v - want).abs() < 1e-6), "ch {c}");
    }
}

#[test]
fn the_lfe_takes_only_its_send() {
    let f = SurroundFormat::S51;
    for az in [0.0f32, 180.0, -110.0] {
        let (mut tp, t) = into(f, ChannelLayout::Mono, dc(&[0.5], SR as usize));
        tp.project.track_mut(t).unwrap().surround = vbap(az);
        let out = render(&tp, 256, 6, 4096);
        assert!(out[label(f, "LFE")].iter().all(|v| *v == 0.0), "{az}");
        tp.project.track_mut(t).unwrap().surround = SurroundPan {
            lfe_db: -6.0206,
            ..vbap(az)
        };
        let sent = render(&tp, 256, 6, 4096);
        assert!(approx(sent[label(f, "LFE")][4000], 0.25, 1e-5));
        // The mains as they were.
        for c in (0..6).filter(|c| *c != label(f, "LFE")) {
            assert!(approx(sent[c][4000], out[c][4000], 1e-6), "{az} {c}");
        }
    }
}

#[test]
fn stereo_goes_into_a_bed_as_an_image_balanced_or_dual() {
    let f = SurroundFormat::S51;
    let (l, r, c, ls, rs) = (
        label(f, "L"),
        label(f, "R"),
        label(f, "C"),
        label(f, "Ls"),
        label(f, "Rs"),
    );
    let (mut tp, t) = into(f, ChannelLayout::Stereo, dc(&[0.8, -0.2], SR as usize));
    // An image at its default: the format's L and R, nothing else.
    tp.project.track_mut(t).unwrap().surround = vbap(0.0);
    let out = render(&tp, 256, 6, 4096);
    assert!(approx(out[l][4000], 0.8, 1e-5) && approx(out[r][4000], -0.2, 1e-5));
    for ch in [c, 3, ls, rs] {
        assert!(out[ch][4000].abs() < 1e-6, "ch {ch}");
    }
    // Balanced at R50 by the strip: left halved, no crossfeed.
    let tr = tp.project.track_mut(t).unwrap();
    tr.surround = SurroundPan {
        stereo: StereoSource::Balance,
        spread: 1.0,
        ..vbap(150.0)
    };
    tr.pan = 0.5;
    let out = render(&tp, 256, 6, 4096);
    assert!(approx(out[l][4000], 0.4, 1e-5) && approx(out[r][4000], -0.2, 1e-5));
    for ch in [c, 3, ls, rs] {
        assert!(out[ch][4000].abs() < 1e-6, "ch {ch}");
    }
    // Dual: each channel on its own surround.
    let tr = tp.project.track_mut(t).unwrap();
    tr.pan = 0.0;
    tr.surround = SurroundPan {
        stereo: StereoSource::Dual,
        left: 110.0,
        right: -110.0,
        ..vbap(0.0)
    };
    let out = render(&tp, 256, 6, 4096);
    assert!(approx(out[ls][4000], 0.8, 1e-5) && approx(out[rs][4000], -0.2, 1e-5));
    assert!(out[l][4000].abs() < 1e-6 && out[r][4000].abs() < 1e-6);
    // Both channels on one direction: they add there (the antiphase part
    // cancels, as in any fold to one place).
    tp.project.track_mut(t).unwrap().surround = SurroundPan {
        stereo: StereoSource::Dual,
        left: 0.0,
        right: 0.0,
        ..vbap(0.0)
    };
    let out = render(&tp, 256, 6, 4096);
    assert!(approx(out[c][4000], 0.6, 1e-5), "{}", out[c][4000]);
}

fn lane(tp: &mut TestProject, track: TrackId, target: AutomationTarget, points: &[(f64, f64)]) {
    let curve = AutomationCurve::from_points(
        points
            .iter()
            .map(|&(q, value)| AutomationPoint {
                time: MusicalTime::from_quarters(q),
                value,
                shape: CurveShape::Linear,
            })
            .collect(),
    );
    let id = tp.project.ids.allocate();
    tp.project
        .track_mut(track)
        .unwrap()
        .automation
        .lanes
        .push(AutomationLane {
            id,
            target,
            curve,
            mode: AutomationMode::Read,
            visible: true,
        });
}

#[test]
fn an_azimuth_lane_turns_the_track_through_every_region() {
    let f = SurroundFormat::S71;
    let n = Q + 4096;
    let (mut tp, t) = into(f, ChannelLayout::Mono, dc(&[0.5], n));
    tp.project.track_mut(t).unwrap().surround = vbap(0.0);
    // Once round to the left in a quarter: C, L, Lss, Lrs, behind.
    lane(
        &mut tp,
        t,
        AutomationTarget::Surround(SurroundParam::Azimuth),
        &[(0.0, 0.0), (1.0, 180.0)],
    );
    let out = render(&tp, 256, 8, n);
    let v = Vbap::of(f);
    for frame in [
        1000usize,
        4000,
        6000,
        9000,
        12_000,
        15_000,
        18_000,
        21_000,
        Q - 64,
    ] {
        let az = 180.0 * (frame + 1) as f32 / Q as f32;
        let mut want = vec![0.0; 8];
        v.gains(az, 0.0, &mut want);
        for c in 0..8 {
            assert!(
                approx(out[c][frame], 0.5 * want[c], 2e-3),
                "{frame} ({az:.1}°) ch {c}: {} vs {}",
                out[c][frame],
                0.5 * want[c]
            );
        }
    }
    // Through every edge between regions: no jumps, and the power holds.
    for (c, ch) in out.iter().enumerate() {
        let worst = ch[..Q]
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0f32, f32::max);
        assert!(worst < 2e-3, "ch {c}: {worst}");
    }
    for i in (256..Q).step_by(97) {
        let p: f32 = out.iter().map(|ch| ch[i] * ch[i]).sum();
        assert!(approx(p, 0.25, 2e-3), "{i}: {p}");
    }
    // Any block size that is a multiple of the automation step: the same.
    for block in [64usize, 128, 512] {
        assert_eq!(render(&tp, block, 8, n), out, "block {block}");
    }
}

#[test]
fn heights_pan_in_3d_and_elevation_is_automated() {
    let f = SurroundFormat::S714;
    let n = Q + 2048;
    let (mut tp, t) = into(f, ChannelLayout::Mono, dc(&[0.5], n));
    tp.project.track_mut(t).unwrap().surround = vbap(45.0);
    lane(
        &mut tp,
        t,
        AutomationTarget::Surround(SurroundParam::Elevation),
        &[(0.0, 0.0), (1.0, 90.0)],
    );
    let out = render(&tp, 256, 12, n);
    let v = Vbap::of(f);
    for frame in [1000usize, 8000, 16_000, Q - 64] {
        let el = 90.0 * (frame + 1) as f32 / Q as f32;
        let mut want = vec![0.0; 12];
        v.gains(45.0, el, &mut want);
        for c in 0..12 {
            assert!(approx(out[c][frame], 0.5 * want[c], 2e-3), "{frame} ch {c}");
        }
    }
    // Overhead at the end: the four tops alike, nothing on the floor.
    let end = n - 1;
    for l in ["Ltf", "Rtf", "Ltr", "Rtr"] {
        assert!(approx(out[label(f, l)][end], 0.25, 1e-4), "{l}");
    }
    assert!(out[label(f, "L")][end].abs() < 1e-6);
}

/// A 5.1 bus (a track at its left surround) into a stereo master: folded
/// by the project's downmix, and a new downmix applies live, ramped.
#[test]
fn a_bed_folds_into_stereo_by_the_projects_downmix() {
    let mut tp = TestProject::new(SR);
    let bus = tp.track(
        TrackKind::Bus,
        "Bed",
        ChannelLayout::Surround(SurroundFormat::S51),
    );
    let t = tp.track(TrackKind::Audio, "Rear", ChannelLayout::Mono);
    let src: AudioSourceId = tp.dc(1, 0.5, SR as usize);
    tp.clip(t, src, MusicalTime::ZERO, SR as i64);
    tp.route(t, bus);
    let tr = tp.project.track_mut(t).unwrap();
    tr.surround = SurroundPan {
        lfe_db: 0.0,
        ..vbap(110.0)
    };
    let h = std::f32::consts::FRAC_1_SQRT_2;
    // A new project: −3 dB surrounds, no LFE.
    let out = render(&tp, 256, 2, 4096);
    assert!(approx(out[0][4000], 0.5 * h, 1e-5) && out[1][4000].abs() < 1e-6);
    // Older projects fold by place: the surround at unity.
    tp.project.downmix = Downmix::BY_PLACE;
    let out = render(&tp, 256, 2, 4096);
    assert!(approx(out[0][4000], 0.5, 1e-5));
    // Live: custom coefficients with the LFE in, as one edit, no rebuild.
    tp.project.downmix = Downmix::STANDARD;
    let mut r =
        OfflineRenderer::new(&tp.project, &tp.sources, EngineConfig::default(), 256, 2).unwrap();
    r.play_from(0).unwrap();
    r.render(2048);
    let mut hist = History::default();
    let impact = hist
        .apply(
            &mut tp.project,
            Command::SetDownmix {
                downmix: Downmix {
                    method: DownmixMethod::Coefficients,
                    surround: 0.5,
                    lfe: 0.25,
                    ..Downmix::STANDARD
                },
            },
        )
        .unwrap();
    assert_eq!(impact, Impact::Params);
    r.controller.sync(&tp.project, &tp.sources, impact).unwrap();
    let out = r.render(512);
    // Ramped across the block: no step.
    let worst = out[0][..256]
        .windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0f32, f32::max);
    assert!(worst < 0.01, "{worst}");
    // Ls 0.5 × 0.5 on the left, the LFE (0.5 sent) × 0.25 on both.
    assert!(approx(out[0][500], 0.25 + 0.125, 1e-5), "{}", out[0][500]);
    assert!(approx(out[1][500], 0.125, 1e-5), "{}", out[1][500]);
    let _ = OutputRouting::Master;
}

/// Headphones on and off while it plays: no rebuild, a crossfade — no
/// sample jumps beyond what the signals do themselves; back on the
/// speakers it is the speakers' signal exactly.
#[test]
fn switching_to_headphones_crossfades_in_place() {
    let n = SR as usize;
    let sine: Vec<f32> = (0..n)
        .map(|i| 0.5 * (i as f32 * 200.0 * std::f32::consts::TAU / SR as f32).sin())
        .collect();
    let (mut tp, t) = into(
        SurroundFormat::S51,
        ChannelLayout::Mono,
        AudioData::from_channels(SR, vec![sine]),
    );
    tp.project.track_mut(t).unwrap().surround = vbap(30.0);
    let mut r =
        OfflineRenderer::new(&tp.project, &tp.sources, EngineConfig::default(), 256, 6).unwrap();
    r.play_from(0).unwrap();
    let before = r.render(4096);
    let jump = |x: &[f32]| {
        x.windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0f32, f32::max)
    };
    let natural = jump(&before[0][1024..]);
    r.controller
        .set_binaural(Some(faderframe_binaural::Room::Near));
    r.controller.relisten(&tp.project);
    let on = r.render(8192);
    for (c, ear) in on.iter().enumerate().take(2) {
        assert!(
            jump(ear) < 3.0 * natural,
            "ch {c}: {} vs {natural}",
            jump(ear)
        );
    }
    // Headphones: both ears hear the front left, the other channels are
    // quiet.
    let rms = |x: &[f32]| (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt();
    assert!(rms(&on[0][4096..]) > 0.05 && rms(&on[1][4096..]) > 0.01);
    assert!(
        on[2..]
            .iter()
            .all(|ch| ch[4096..].iter().all(|v| *v == 0.0))
    );
    r.controller.set_binaural(None);
    r.controller.relisten(&tp.project);
    let off = r.render(8192);
    for (c, ch) in off.iter().enumerate().take(2) {
        assert!(jump(ch) < 3.0 * natural, "ch {c}");
    }
    // Back to the speakers exactly.
    let speakers = render(&tp, 256, 6, 4096 + 8192 + 8192);
    for c in 0..6 {
        for i in 4096..8192 {
            assert!(
                (off[c][i] - speakers[c][4096 + 8192 + i]).abs() < 1e-6,
                "ch {c} at {i}"
            );
        }
    }
    // The renderers that went out come back to this thread.
    r.controller
        .sync(&tp.project, &tp.sources, Impact::None)
        .unwrap();
}

/// Bass management on the speakers: the centre's lows go to the LFE
/// output (−10 dB), its highs stay; switched live, no rebuild; a render
/// never has it.
#[test]
fn bass_management_moves_the_mains_lows_to_the_sub() {
    use faderframe_core::surround::BassManagement;
    let n = SR as usize;
    let tone = |hz: f32| -> Vec<f32> {
        (0..n)
            .map(|i| 0.5 * (i as f32 * hz * std::f32::consts::TAU / SR as f32).sin())
            .collect()
    };
    let mut both = tone(30.0);
    for (b, h) in both.iter_mut().zip(tone(1000.0)) {
        *b += h;
    }
    let (mut tp, t) = into(
        SurroundFormat::S51,
        ChannelLayout::Mono,
        AudioData::from_channels(SR, vec![both]),
    );
    tp.project.track_mut(t).unwrap().surround = vbap(0.0);
    let mut r =
        OfflineRenderer::new(&tp.project, &tp.sources, EngineConfig::default(), 256, 6).unwrap();
    r.play_from(0).unwrap();
    let plain = r.render(8192);
    let (c, lfe) = (
        label(SurroundFormat::S51, "C"),
        label(SurroundFormat::S51, "LFE"),
    );
    assert!(plain[lfe].iter().all(|v| *v == 0.0));
    r.controller
        .set_bass_management(BassManagement {
            enabled: true,
            ..BassManagement::default()
        })
        .unwrap();
    let managed = r.render(24_000);
    let rms = |x: &[f32]| (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt();
    // The sub: the 30 Hz part at −10 dB (a little under: LR4 at 80 Hz).
    let sub = rms(&managed[lfe][12_000..]);
    let want = 0.5 * std::f32::consts::FRAC_1_SQRT_2 * faderframe_core::db_to_gain(-10.0);
    assert!(sub > 0.85 * want && sub < 1.02 * want, "{sub} vs {want}");
    // The centre keeps the 1 kHz part (its 30 Hz is gone).
    let centre = rms(&managed[c][12_000..]);
    let high = 0.5 * std::f32::consts::FRAC_1_SQRT_2;
    assert!((centre - high).abs() < 0.03 * high, "{centre}");
    // Switched on without a jump.
    let jump = |x: &[f32]| {
        x.windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0f32, f32::max)
    };
    assert!(jump(&managed[c][..2048]) < 1.2 * jump(&plain[c][1024..]));
    // A render of the same project has none of it.
    let rendered = render(&tp, 256, 6, 8192);
    assert!(rendered[lfe].iter().all(|v| *v == 0.0));
}

fn hexagon() -> SurroundFormat {
    use faderframe_core::surround::{LayoutDef, SpeakerDef};
    let spk = |label: &str, azimuth: f32| SpeakerDef {
        label: label.into(),
        azimuth,
        elevation: 0.0,
        lfe: false,
    };
    LayoutDef {
        name: "Engine Hexagon".into(),
        speakers: vec![
            spk("F", 0.0),
            spk("FL", 60.0),
            spk("BL", 120.0),
            spk("B", 180.0),
            spk("BR", -120.0),
            spk("FR", -60.0),
        ],
    }
    .intern()
    .unwrap()
}

/// A custom layout as the master: VBAP places on its speakers, its
/// channels reach the device in their order; a bus in it folds into 5.1
/// by direction; on a stereo device it folds by the downmix; on
/// headphones every speaker is heard.
#[test]
fn a_custom_layout_is_a_bed_like_the_others() {
    let hex = hexagon();
    let (mut tp, t) = into(hex, ChannelLayout::Mono, dc(&[0.5], SR as usize));
    tp.project.track_mut(t).unwrap().surround = vbap(60.0);
    let out = render(&tp, 256, 6, 4096);
    for (c, ch) in out.iter().enumerate() {
        let want = if c == 1 { 0.5 } else { 0.0 };
        assert!((ch[4000] - want).abs() < 1e-5, "ch {c}: {}", ch[4000]);
    }
    // Between two of its speakers: both, at constant power.
    tp.project.track_mut(t).unwrap().surround = vbap(150.0);
    let out = render(&tp, 256, 6, 4096);
    assert!((out[2][4000] - out[3][4000]).abs() < 1e-5 && out[2][4000] > 0.3);
    // On a stereo device: folded by the downmix (B is a centre there).
    tp.project.track_mut(t).unwrap().surround = vbap(180.0);
    let folded = render(&tp, 256, 2, 4096);
    let h = std::f32::consts::FRAC_1_SQRT_2;
    assert!(approx(folded[0][4000], 0.5 * h, 1e-5) && approx(folded[1][4000], 0.5 * h, 1e-5));
    // A hexagon bus into a 5.1 master: B lands between Ls and Rs.
    let mut tp = TestProject::new(SR);
    let master = tp.master();
    tp.project.track_mut(master).unwrap().layout = ChannelLayout::Surround(SurroundFormat::S51);
    let bus = tp.track(TrackKind::Bus, "Hex", ChannelLayout::Surround(hex));
    let m = tp.track(TrackKind::Audio, "Back", ChannelLayout::Mono);
    let src = tp.dc(1, 0.5, SR as usize);
    tp.clip(m, src, MusicalTime::ZERO, SR as i64);
    tp.route(m, bus);
    tp.project.track_mut(m).unwrap().surround = vbap(180.0);
    let out = render(&tp, 256, 6, 4096);
    let f = SurroundFormat::S51;
    let (ls, rs) = (out[label(f, "Ls")][4000], out[label(f, "Rs")][4000]);
    assert!(approx(ls, rs, 1e-5) && ls > 0.3, "{ls} {rs}");
    assert!(out[label(f, "C")][4000].abs() < 1e-6);
    // Headphones: a speaker between the measured directions is heard.
    let (mut tp, t) = into(hex, ChannelLayout::Mono, dc(&[0.5], SR as usize));
    tp.project.track_mut(t).unwrap().surround = vbap(60.0);
    let mut r =
        OfflineRenderer::new(&tp.project, &tp.sources, EngineConfig::default(), 256, 2).unwrap();
    r.controller
        .set_binaural(Some(faderframe_binaural::Room::Near));
    r.controller
        .sync(&tp.project, &tp.sources, Impact::Graph)
        .unwrap();
    r.play_from(0).unwrap();
    let ears = r.render(8192);
    let rms = |x: &[f32]| (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt();
    assert!(rms(&ears[0][4096..]) > 0.05, "{}", rms(&ears[0][4096..]));
}

/// A diffuse spread: the point keeps √(1 − spread), every main speaker
/// gets its own decorrelated copy of the rest — the power as a coherent
/// spread's, the speakers no longer alike; switching it rebuilds the
/// graph (its filters), no added latency.
#[test]
fn a_diffuse_spread_decorrelates_the_speakers() {
    let n = SR as usize;
    let mut seed = 0x9E37_79B9u32;
    let noise: Vec<f32> = (0..n)
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            (seed as f32 / u32::MAX as f32 - 0.5) * 0.5
        })
        .collect();
    let (mut tp, t) = into(
        SurroundFormat::S51,
        ChannelLayout::Mono,
        AudioData::from_channels(SR, vec![noise.clone()]),
    );
    let spread = SurroundPan {
        spread: 1.0,
        ..vbap(0.0)
    };
    tp.project.track_mut(t).unwrap().surround = spread;
    let coherent = render(&tp, 256, 6, 24_000);
    tp.project.track_mut(t).unwrap().surround = SurroundPan {
        diffuse: true,
        ..spread
    };
    let diffuse = render(&tp, 256, 6, 24_000);
    let rms = |x: &[f32]| (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt();
    let corr = |a: &[f32], b: &[f32]| {
        a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>() / a.len() as f32 / (rms(a) * rms(b))
    };
    let f = SurroundFormat::S51;
    let mains = [
        label(f, "L"),
        label(f, "R"),
        label(f, "C"),
        label(f, "Ls"),
        label(f, "Rs"),
    ];
    let tail = 6000..24_000;
    // Coherent: the same signal on all five; diffuse: unalike.
    let (a, b) = (mains[0], mains[3]);
    assert!(corr(&coherent[a][tail.clone()], &coherent[b][tail.clone()]) > 0.99);
    assert!(corr(&diffuse[a][tail.clone()], &diffuse[b][tail.clone()]).abs() < 0.2);
    // The same power on each speaker, the LFE untouched.
    for &c in &mains {
        let db = 20.0 * (rms(&diffuse[c][tail.clone()]) / rms(&coherent[c][tail.clone()])).log10();
        assert!(db.abs() < 0.5, "ch {c}: {db} dB");
    }
    assert!(diffuse[label(f, "LFE")].iter().all(|v| *v == 0.0));
    // Switched live (a parameter edit): the graph is rebuilt with the
    // filters and plays the diffuse field.
    tp.project.track_mut(t).unwrap().surround = spread;
    let mut r =
        OfflineRenderer::new(&tp.project, &tp.sources, EngineConfig::default(), 256, 6).unwrap();
    r.play_from(0).unwrap();
    r.render(2048);
    let mut h = History::default();
    let impact = h
        .apply(
            &mut tp.project,
            Command::SetTrackSurround {
                track: t,
                pan: SurroundPan {
                    diffuse: true,
                    ..spread
                },
            },
        )
        .unwrap();
    assert_eq!(impact, Impact::Params);
    r.controller.sync(&tp.project, &tp.sources, impact).unwrap();
    let live = r.render(16_000);
    assert!(corr(&live[a][4000..], &live[b][4000..]).abs() < 0.2);
    // Half diffuse: the point (C) answers an impulse at once.
    let mut imp = vec![0.0f32; 8000];
    imp[2000] = 1.0;
    let (mut tp, t) = into(
        SurroundFormat::S51,
        ChannelLayout::Mono,
        AudioData::from_channels(SR, vec![imp]),
    );
    tp.project.track_mut(t).unwrap().surround = SurroundPan {
        spread: 0.5,
        diffuse: true,
        ..vbap(0.0)
    };
    let out = render(&tp, 256, 6, 8000);
    let c = label(f, "C");
    let first = out[c].iter().position(|v| v.abs() > 1e-6).unwrap();
    assert_eq!(first, 2000, "no delay");
    assert!(
        approx(out[c][2000], 0.5f32.sqrt() + (0.1f32).sqrt() * 0.0625, 1e-3),
        "{}",
        out[c][2000]
    );
}

/// Speakers below ear level take sources below it; a built-in format
/// keeps them at ear level.
#[test]
fn a_source_below_ear_level_plays_on_the_lower_speakers() {
    use faderframe_core::surround::{LayoutDef, SpeakerDef};
    let spk = |label: &str, azimuth: f32, elevation: f32| SpeakerDef {
        label: label.into(),
        azimuth,
        elevation,
        lfe: false,
    };
    let mut speakers: Vec<SpeakerDef> = [30.0f32, 90.0, 150.0, -150.0, -90.0, -30.0]
        .iter()
        .enumerate()
        .map(|(i, az)| spk(&format!("M{i}"), *az, 0.0))
        .collect();
    for (i, el) in [40.0f32, -40.0].iter().enumerate() {
        for az in [45.0f32, 135.0, -135.0, -45.0] {
            speakers.push(spk(&format!("{}{az}", ["U", "D"][i]), az, *el));
        }
    }
    let sphere = LayoutDef {
        name: "Engine Sphere".into(),
        speakers,
    }
    .intern()
    .unwrap();
    let (mut tp, t) = into(sphere, ChannelLayout::Mono, dc(&[0.5], SR as usize));
    tp.project.track_mut(t).unwrap().surround = SurroundPan {
        elevation: -40.0,
        ..vbap(45.0)
    };
    let out = render(&tp, 256, 14, 4096);
    for (c, ch) in out.iter().enumerate() {
        let want = if c == 10 { 0.5 } else { 0.0 };
        assert!((ch[4000] - want).abs() < 1e-5, "ch {c}: {}", ch[4000]);
    }
    // 7.1.4 has nothing below: at ear level.
    let (mut tp, t) = into(
        SurroundFormat::S714,
        ChannelLayout::Mono,
        dc(&[0.5], SR as usize),
    );
    tp.project.track_mut(t).unwrap().surround = SurroundPan {
        elevation: -40.0,
        ..vbap(30.0)
    };
    let out = render(&tp, 256, 12, 4096);
    assert!((out[label(SurroundFormat::S714, "L")][4000] - 0.5).abs() < 1e-5);
}
