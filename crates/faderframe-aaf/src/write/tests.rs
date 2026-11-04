use super::*;
use crate::read::AafFile;
use crate::timeline::{EssenceFormat, Marker, Media, Timecode, Track};
use std::io::Cursor;

fn clip(position: i64, length: i64) -> Clip {
    Clip {
        name: "c".into(),
        position,
        length,
        media: 0,
        channel: 0,
        offset: 0,
        gain_db: 0.0,
        envelope: Vec::new(),
        fade_in: None,
        fade_out: None,
    }
}

fn fade(length: i64, curve: FadeCurve) -> Option<Fade> {
    Some(Fade { length, curve })
}

#[test]
fn fades_become_transitions_against_filler() {
    let mut a = clip(1000, 500);
    a.fade_in = fade(100, FadeCurve::Linear);
    a.fade_out = fade(50, FadeCurve::EqualPower);
    let mut b = clip(1500, 400);
    b.fade_in = fade(80, FadeCurve::Linear);
    let clips = [a, b];
    let (items, length) = sequence(&clips, &[0, 1]);
    use Item::*;
    assert_eq!(
        items,
        vec![
            Filler(1100),
            Transition {
                length: 100,
                cut: 0,
                level: Level::In(FadeCurve::Linear)
            },
            Clip(0),
            Transition {
                length: 50,
                cut: 50,
                level: Level::Out(FadeCurve::EqualPower)
            },
            // Abutting: the filler lies wholly under both transitions.
            Filler(130),
            Transition {
                length: 80,
                cut: 0,
                level: Level::In(FadeCurve::Linear)
            },
            Clip(1),
        ]
    );
    assert_eq!(length, 1900);
    // Every clip starts where it is: segments less transitions.
    let mut t = 0;
    for i in &items {
        match i {
            Filler(n) => t += n,
            Clip(k) => {
                assert_eq!(t, clips[*k].position);
                t += clips[*k].length;
            }
            Transition { length, .. } => t -= length,
        }
    }
}

#[test]
fn overlaps_crossfade_or_take_another_lane() {
    let mut a = clip(0, 1000);
    a.fade_out = fade(200, FadeCurve::EqualPower);
    let mut b = clip(800, 1000);
    b.fade_in = fade(200, FadeCurve::EqualPower);
    // Overlaps without matching fades: a lane of its own.
    let c = clip(900, 100);
    let clips = [a, b, c];
    let lanes = lanes(&clips);
    assert_eq!(lanes, vec![vec![0, 1], vec![2]]);
    let (items, length) = sequence(&clips, &lanes[0]);
    assert_eq!(
        items,
        vec![
            Item::Clip(0),
            Item::Transition {
                length: 200,
                cut: 100,
                level: Level::In(FadeCurve::EqualPower)
            },
            Item::Clip(1),
        ]
    );
    assert_eq!(length, 1800);
}

#[test]
fn fades_longer_than_the_clip_are_shortened_to_fit() {
    let mut a = clip(0, 100);
    a.fade_in = fade(80, FadeCurve::Linear);
    a.fade_out = fade(80, FadeCurve::Linear);
    let (i, o) = fitted(&a);
    assert_eq!(i.map(|f| f.length), Some(50));
    assert_eq!(o.map(|f| f.length), Some(50));
}

struct Ramp;

impl Samples for Ramp {
    fn read(&mut self, media: usize, channel: u16, at: u64, out: &mut [f32]) -> io::Result<()> {
        for (k, s) in out.iter_mut().enumerate() {
            let n = at + k as u64;
            *s = ((n % 1000) as f32 / 1000.0 - 0.5) * if channel == 0 { 1.0 } else { -1.0 }
                + media as f32 * 0.01;
        }
        Ok(())
    }
}

pub(crate) fn sample_composition() -> Composition {
    let mut a = clip(48_000, 96_000);
    a.name = "Dialogue".into();
    a.offset = 1000;
    a.gain_db = -6.0;
    a.fade_in = fade(4800, FadeCurve::EqualPower);
    a.fade_out = fade(9600, FadeCurve::Linear);
    let mut b = clip(144_000 - 9600, 48_000);
    b.channel = 1;
    b.fade_in = fade(9600, FadeCurve::Linear);
    b.envelope = vec![(0, 0.0), (24_000, -12.0), (48_000, 0.0)];
    let mut c = clip(10_000, 20_000);
    c.media = 1;
    Composition {
        name: "Reel 1".into(),
        rate: 48_000,
        timecode: Some(Timecode {
            start: 90_000,
            fps: 25,
            drop: false,
            rate: (25, 1),
        }),
        tracks: vec![
            Track {
                name: "Dia".into(),
                clips: vec![a, b],
            },
            Track {
                name: "FX".into(),
                clips: vec![c],
            },
        ],
        markers: vec![Marker {
            position: 96_000,
            name: "Scene 2".into(),
            color: Some([255, 0, 0]),
        }],
        media: vec![
            Media {
                name: "Boom".into(),
                channels: 2,
                rate: 48_000,
                frames: 200_000,
                essence: vec![Essence::Embed { bits: 24 }, Essence::Embed { bits: 24 }],
            },
            Media {
                name: "Door".into(),
                channels: 1,
                rate: 48_000,
                frames: 30_000,
                essence: vec![Essence::Embed { bits: 16 }],
            },
        ],
    }
}

pub(crate) fn written(comp: &Composition) -> Vec<u8> {
    let options = WriteOptions {
        ids: IdSource::seeded(42),
        version: "0.36.0".into(),
        when: TimeStamp {
            year: 2026,
            month: 11,
            day: 4,
            ..TimeStamp::default()
        },
        ..WriteOptions::default()
    };
    let (out, report) = write(
        Cursor::new(Vec::new()),
        comp,
        &mut Ramp,
        options,
        &mut |_, _| true,
    )
    .unwrap();
    assert_eq!(report.slots, comp.tracks.len());
    out.into_inner()
}

#[test]
fn a_composition_reads_back_as_it_was_written() {
    let comp = sample_composition();
    let bytes = written(&comp);
    let mut f = AafFile::open(Cursor::new(bytes)).unwrap();
    let back = f.composition(48_000).unwrap();
    assert!(back.notes.is_empty(), "{:?}", back.notes);
    let got = back.composition;
    assert_eq!(got.name, "Reel 1");
    assert_eq!(got.timecode, comp.timecode);
    assert_eq!(got.markers, comp.markers);
    assert_eq!(got.tracks.len(), 2);
    assert_eq!(got.tracks[0].name, "Dia");
    let (a, b) = (&got.tracks[0].clips[0], &got.tracks[0].clips[1]);
    assert_eq!((a.position, a.length, a.offset), (48_000, 96_000, 1000));
    assert!((a.gain_db + 6.0).abs() < 1e-4, "{}", a.gain_db);
    assert_eq!(a.fade_in, fade(4800, FadeCurve::EqualPower));
    // A crossfade: both sides take the dissolve's curve.
    assert_eq!(a.fade_out, fade(9600, FadeCurve::Linear));
    assert_eq!(b.fade_in, fade(9600, FadeCurve::Linear));
    // Each channel is a mono file source of its own: the second channel of
    // "Boom" reads as channel 0 of another medium.
    assert_eq!((b.position, b.length, b.channel), (134_400, 48_000, 0));
    assert_eq!(b.envelope.len(), 3);
    assert!((b.envelope[1].1 + 12.0).abs() < 1e-3);
    assert_eq!(b.envelope[1].0, 24_000);
    let c = &got.tracks[1].clips[0];
    assert_eq!(got.media[c.media].name, "Door");
    // The media: a medium per channel (mono file source mobs).
    let boom = &got.media[a.media];
    assert_eq!(boom.name, "Boom");
    assert_eq!((boom.rate, boom.frames), (48_000, 200_000));
    let Essence::Embedded(e) = boom.essence[0].clone() else {
        panic!("not embedded: {:?}", boom.essence[0]);
    };
    assert_eq!(
        e.format,
        EssenceFormat::Pcm {
            bits: 24,
            channels: 1,
            rate: 48_000,
            big_endian: false,
            float: false
        }
    );
    let mut pcm = Vec::new();
    f.copy_essence(&e, &mut pcm, &mut |_| {}).unwrap();
    assert_eq!(pcm.len(), 600_000);
    let at = |n: usize| {
        let v = i32::from_le_bytes([0, pcm[3 * n], pcm[3 * n + 1], pcm[3 * n + 2]]) >> 8;
        v as f64 / 8_388_608.0
    };
    assert!((at(250) - (0.25 - 0.5)).abs() < 1e-6);
    assert!((at(1999) - (0.999 - 0.5)).abs() < 1e-6);
    // The other channel is another medium of the same master: the clip on
    // channel 1 of "Boom" plays it.
    assert_eq!(got.media[b.media].name, "Boom");
    assert_ne!(b.media, a.media);
}

#[test]
fn linked_media_carry_their_url_and_header() {
    let mut comp = sample_composition();
    comp.media.truncate(1);
    comp.tracks.truncate(1);
    let header = crate::wave::header_for_pcm(
        crate::wave::PcmInfo {
            channels: 1,
            rate: 48_000,
            bits: 24,
            float: false,
        },
        false,
        600_000,
    )
    .0[..36]
        .to_vec();
    comp.media[0].essence = vec![
        Essence::Link {
            url: "file:///media/Boom%20L.wav".into(),
            wave_header: header.clone(),
        },
        Essence::Link {
            url: "file:///media/Boom%20R.wav".into(),
            wave_header: header,
        },
    ];
    let bytes = written(&comp);
    let got = AafFile::open(Cursor::new(bytes))
        .unwrap()
        .composition(48_000)
        .unwrap()
        .composition;
    let urls: Vec<_> = got
        .media
        .iter()
        .map(|m| match &m.essence[0] {
            Essence::Linked { urls, format, .. } => {
                assert_eq!(*format, EssenceFormat::Wave);
                urls[0].clone()
            }
            e => panic!("{e:?}"),
        })
        .collect();
    assert_eq!(
        urls,
        ["file:///media/Boom%20L.wav", "file:///media/Boom%20R.wav"]
    );
}

#[test]
fn a_composition_read_at_another_rate_is_rescaled() {
    let got = AafFile::open(Cursor::new(written(&sample_composition())))
        .unwrap()
        .composition(96_000)
        .unwrap()
        .composition;
    let a = &got.tracks[0].clips[0];
    assert_eq!((a.position, a.length, a.offset), (96_000, 192_000, 2000));
}

#[test]
fn cancelling_stops_the_essence() {
    let comp = sample_composition();
    let r = write(
        Cursor::new(Vec::new()),
        &comp,
        &mut Ramp,
        WriteOptions {
            ids: IdSource::seeded(1),
            ..WriteOptions::default()
        },
        &mut |done, _| done < 100_000,
    );
    assert!(matches!(r, Err(WriteError::Cancelled)));
}

#[test]
fn bad_compositions_are_refused() {
    let mut comp = sample_composition();
    comp.tracks[0].clips[0].media = 7;
    let r = write(
        Cursor::new(Vec::new()),
        &comp,
        &mut Ramp,
        WriteOptions::default(),
        &mut |_, _| true,
    );
    assert!(matches!(r, Err(WriteError::Invalid(_))));
}
