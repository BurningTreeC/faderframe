//! A sample composition written with seeded ids: its structure held to a
//! golden outline, and written out for external readers on request
//! (`FADERFRAME_AAF_OUT=<dir>`: sample-embedded.aaf, sample-linked.aaf and
//! its WAVE files; check them with pyaaf2).

use faderframe_aaf::ids::IdSource;
use faderframe_aaf::read::AafFile;
use faderframe_aaf::store::TimeStamp;
use faderframe_aaf::store::dump::dump;
use faderframe_aaf::timeline::*;
use faderframe_aaf::write::{Samples, WriteOptions, write};
use std::io::Cursor;

struct Sine;

impl Samples for Sine {
    fn read(
        &mut self,
        media: usize,
        channel: u16,
        at: u64,
        out: &mut [f32],
    ) -> std::io::Result<()> {
        let hz = 220.0 * (1 + media + usize::from(channel)) as f64;
        for (k, s) in out.iter_mut().enumerate() {
            let t = (at + k as u64) as f64 / 48_000.0;
            *s = (0.25 * (std::f64::consts::TAU * hz * t).sin()) as f32;
        }
        Ok(())
    }
}

fn composition(essence: impl Fn(&str, u16) -> Essence) -> Composition {
    let clip = |position, length, media, channel| Clip {
        name: String::new(),
        position,
        length,
        media,
        channel,
        offset: 0,
        gain_db: 0.0,
        envelope: Vec::new(),
        fade_in: None,
        fade_out: None,
    };
    let mut l = clip(48_000, 96_000, 0, 0);
    l.fade_in = Some(Fade {
        length: 4_800,
        curve: FadeCurve::EqualPower,
    });
    l.gain_db = -3.0;
    let mut r = l.clone();
    r.channel = 1;
    let mut fx = clip(24_000, 48_000, 1, 0);
    fx.fade_out = Some(Fade {
        length: 9_600,
        curve: FadeCurve::Linear,
    });
    let mut fx2 = clip(72_000 - 9_600, 48_000, 1, 0);
    fx2.offset = 12_000;
    fx2.fade_in = Some(Fade {
        length: 9_600,
        curve: FadeCurve::Linear,
    });
    fx2.envelope = vec![(0, 0.0), (24_000, -10.0), (48_000, -3.0)];
    Composition {
        name: "Sample".into(),
        rate: 48_000,
        timecode: Some(Timecode {
            start: 90_000,
            fps: 25,
            drop: false,
            rate: (25, 1),
        }),
        tracks: vec![
            Track {
                name: "Music.L".into(),
                clips: vec![l],
            },
            Track {
                name: "Music.R".into(),
                clips: vec![r],
            },
            Track {
                name: "FX".into(),
                clips: vec![fx, fx2],
            },
        ],
        markers: vec![
            Marker {
                position: 48_000,
                name: "Verse".into(),
                color: None,
            },
            Marker {
                position: 120_000,
                name: "Hit".into(),
                color: Some([0, 128, 255]),
            },
        ],
        media: vec![
            Media {
                name: "Music".into(),
                channels: 2,
                rate: 48_000,
                frames: 200_000,
                essence: vec![essence("Music L", 0), essence("Music R", 1)],
            },
            Media {
                name: "Door".into(),
                channels: 1,
                rate: 48_000,
                frames: 100_000,
                essence: vec![essence("Door", 0)],
            },
        ],
    }
}

fn options() -> WriteOptions {
    WriteOptions {
        ids: IdSource::seeded(7),
        version: "0.36.0".into(),
        when: TimeStamp {
            year: 2026,
            month: 11,
            day: 4,
            hour: 12,
            minute: 0,
            second: 0,
        },
        platform: "test".into(),
        ..WriteOptions::default()
    }
}

#[test]
fn the_sample_keeps_its_structure() {
    let comp = composition(|_, _| Essence::Embed { bits: 24 });
    let (out, _) = write(
        Cursor::new(Vec::new()),
        &comp,
        &mut Sine,
        options(),
        &mut |_, _| true,
    )
    .unwrap();
    let f = AafFile::open(Cursor::new(out.into_inner())).unwrap();
    let header = f
        .root()
        .strong(faderframe_aaf::model::pid::ROOT_HEADER)
        .unwrap();
    let mut text = String::new();
    dump(header, 0, &mut text);
    let golden = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/sample.txt");
    if std::env::var_os("FADERFRAME_AAF_BLESS").is_some() {
        std::fs::write(&golden, &text).unwrap();
    }
    let want = std::fs::read_to_string(&golden).unwrap_or_default();
    assert!(
        text == want,
        "the structure changed (FADERFRAME_AAF_BLESS=1 to accept):\n{text}"
    );
}

#[test]
fn the_sample_is_written_for_external_readers() {
    let Some(dir) = std::env::var_os("FADERFRAME_AAF_OUT") else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir).unwrap();
    let comp = composition(|_, _| Essence::Embed { bits: 24 });
    let file = std::fs::File::create(dir.join("sample-embedded.aaf")).unwrap();
    write(file, &comp, &mut Sine, options(), &mut |_, _| true).unwrap();
    // Linked: the WAVE files beside it.
    let media = dir.join("sample-linked Media");
    std::fs::create_dir_all(&media).unwrap();
    let mut links = Vec::new();
    for (m, name, frames, channel) in [
        (0, "Music L", 200_000u64, 0u16),
        (0, "Music R", 200_000, 1),
        (1, "Door", 100_000, 0),
    ] {
        let path = media.join(format!("{name}.wav"));
        let mut samples = vec![0f32; frames as usize];
        Sine.read(m, channel, 0, &mut samples).unwrap();
        let mut pcm = Vec::with_capacity(samples.len() * 3);
        for s in samples {
            let v = (f64::from(s) * 8_388_608.0).round() as i32;
            pcm.extend_from_slice(&v.to_le_bytes()[..3]);
        }
        let info = faderframe_aaf::wave::PcmInfo {
            channels: 1,
            rate: 48_000,
            bits: 24,
            float: false,
        };
        let (mut bytes, _) = faderframe_aaf::wave::header_for_pcm(info, false, pcm.len() as u64);
        bytes.extend_from_slice(&pcm);
        std::fs::write(&path, bytes).unwrap();
        links.push((name, path));
    }
    let comp = composition(|name, _| {
        let path = &links.iter().find(|l| l.0 == name).unwrap().1;
        Essence::Link {
            url: format!("file://{}", path.display()).replace(' ', "%20"),
            wave_header: faderframe_aaf::wave::wave_summary(path).unwrap(),
        }
    });
    let file = std::fs::File::create(dir.join("sample-linked.aaf")).unwrap();
    write(file, &comp, &mut Sine, options(), &mut |_, _| true).unwrap();
}
