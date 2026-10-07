//! An ADM BWF master for outside checkers (MediaInfo's Dolby Atmos profile
//! checks, the EBU ADM renderer): `FADERFRAME_ADM_OUT=<path> cargo test -p
//! faderframe-adm --test sample -- --ignored`.
#![allow(clippy::unwrap_used)]

use faderframe_adm::{BEDS, Block, Master, Object, axml, chna, validate};
use faderframe_audio_files::Dither;
use faderframe_audio_files::WavFormat;
use faderframe_audio_files::bw64::Bw64Writer;

#[test]
#[ignore = "writes a file for outside checkers"]
fn write_a_sample_master() {
    let path = std::env::var("FADERFRAME_ADM_OUT").unwrap();
    let rate = 48_000u32;
    let frames = 4 * rate as u64;
    // An object circling the room once over four seconds, in 64 blocks.
    let circle: Vec<Block> = (0..64u64)
        .map(|i| {
            let a = i as f32 / 64.0 * std::f32::consts::TAU;
            Block {
                start: i * frames / 64,
                length: (i + 1) * frames / 64 - i * frames / 64,
                position: [a.sin(), a.cos(), 0.0],
                size: 0.0,
                gain: 1.0,
            }
        })
        .collect();
    let profile = match std::env::var("FADERFRAME_ADM_PROFILE").as_deref() {
        Ok("itu") => faderframe_adm::Profile::Itu,
        _ => faderframe_adm::Profile::DolbyAtmos,
    };
    let m = Master {
        name: "Sample".into(),
        profile,
        sample_rate: rate,
        frames,
        bed: BEDS[7].to_vec(),
        objects: vec![
            Object {
                name: "Circle".into(),
                blocks: circle,
            },
            Object {
                name: "Overhead".into(),
                blocks: vec![Block {
                    start: 0,
                    length: frames,
                    position: [0.0, 0.0, 1.0],
                    size: 0.3,
                    gain: 1.0,
                }],
            },
        ],
    };
    validate(&m).unwrap();
    let mut w = Bw64Writer::create(
        std::path::Path::new(&path),
        m.channels() as u16,
        rate,
        WavFormat::Pcm24,
        Dither::Off,
        &[(*b"chna", chna(&m))],
    )
    .unwrap();
    // A tone on the centre and on each object.
    let n = frames as usize;
    let tone = |f: f32| -> Vec<f32> {
        (0..n)
            .map(|i| 0.25 * (i as f32 * f * std::f32::consts::TAU / rate as f32).sin())
            .collect()
    };
    let silent = vec![0.0f32; n];
    let (c, a, b) = (tone(440.0), tone(660.0), tone(880.0));
    let mut chans: Vec<&[f32]> = vec![&silent; m.channels()];
    chans[2] = &c;
    chans[10] = &a;
    chans[11] = &b;
    w.write_planar(&chans, n).unwrap();
    w.finish(&[(*b"axml", axml(&m).into_bytes())]).unwrap();
}
