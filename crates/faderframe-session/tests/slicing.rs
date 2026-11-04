//! Slicing, REX-style: a drum loop cut at its hits onto Drum Sampler pads
//! with a MIDI clip that plays them as they were, on the grid onto a
//! Sampler in slice mode, onto a kit's free pads, and a sampler's own
//! sample sliced.
#![allow(clippy::unwrap_used)]

use faderframe_audio_files::read_wav;
use faderframe_core::{PluginInstanceId, TrackId, builtin};
use faderframe_engine::EngineConfig;
use faderframe_engine::offline::OfflineRenderer;
use faderframe_plugin_host::devices::samples::{SampleDoc, unpack};
use faderframe_plugin_host::devices::{drums, sampler};
use faderframe_project::{ClipContent, MidiClip, OutputRouting, Project, TrackKind};
use faderframe_session::sampling::SampleTarget;
use faderframe_session::slicing::{SliceBy, SliceSource, SliceTarget};
use faderframe_session::{Action, Session};
use faderframe_timeline::MusicalTime;
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(120);

fn track(s: &Session, name: &str) -> TrackId {
    s.project()
        .tracks
        .iter()
        .find(|t| t.name == name)
        .unwrap_or_else(|| panic!("no track {name}"))
        .id
}

/// A sampler slot's state: its parameter block and document.
fn sampler_state(s: &Session, plugin: PluginInstanceId) -> (Vec<u8>, SampleDoc) {
    let (_, slot) = s.plugin_owner(plugin).unwrap();
    let state = faderframe_engine::decode_state(slot.state.as_deref().unwrap()).unwrap();
    let (params, doc) = unpack(&state).unwrap();
    (params.to_vec(), doc)
}

/// A value in a packed parameter block.
fn param(block: &[u8], id: u32) -> Option<f64> {
    block.as_chunks::<12>().0.iter().find_map(|c| {
        (u32::from_le_bytes(c[..4].try_into().unwrap()) == id)
            .then(|| f64::from_le_bytes(c[4..].try_into().unwrap()))
    })
}

fn instrument(s: &Session, t: TrackId) -> PluginInstanceId {
    let t = s.project().track(t).unwrap();
    s.instrument_slot(t).unwrap().id
}

/// The MIDI clip on a track.
fn midi_clip(s: &Session, t: TrackId) -> (MusicalTime, MidiClip) {
    let c = s
        .project()
        .clips_of(t)
        .into_iter()
        .find(|c| matches!(c.content, ClipContent::Midi(_)))
        .unwrap()
        .clone();
    let ClipContent::Midi(m) = c.content else {
        unreachable!()
    };
    (c.start, m)
}

fn samples_at(p: &Project, m: MusicalTime) -> i64 {
    p.timeline.to_samples(m, f64::from(p.sample_rate))
}

fn wait(s: &mut Session) {
    let end = Instant::now() + WAIT;
    while !s.sampling().is_empty() {
        assert!(Instant::now() < end, "the sample timed out");
        s.tick(0.016);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(s.wait_for_slicing(WAIT), "the slicing timed out");
}

/// `track` alone (no devices, at 0 dB, centred, into the master), the
/// master without devices: `frames` from `from`.
fn alone(p: &Project, keep: TrackId, from: i64, frames: usize) -> Vec<Vec<f32>> {
    let mut p = p.clone();
    let master = p.master_id().unwrap();
    for t in &mut p.tracks {
        if t.id == master {
            t.inserts.clear();
            t.volume_db = 0.0;
            continue;
        }
        // (Muting a folder would mute what is in it.)
        t.mute = t.id != keep && t.kind.has_audio();
        if t.id == keep {
            t.folder = None;
            t.inserts
                .retain(|s| s.plugin.id == builtin::DRUMS || s.plugin.id == builtin::SAMPLER);
            t.preamp = None;
            t.volume_db = 0.0;
            t.pan = 0.0;
            t.sends.clear();
            t.output = OutputRouting::Master;
            t.automation.lanes.clear();
        }
    }
    let rate = p.sample_rate;
    let mut sources = faderframe_engine::render_generated_sources(&p, rate);
    faderframe_session::media::open_file_sources(&p, None, &mut sources);
    let config = EngineConfig {
        sample_rate: rate,
        ..EngineConfig::default()
    };
    let mut r = OfflineRenderer::new(&p, &sources, config, 512, 2).unwrap();
    r.play_from(from).unwrap();
    r.render(frames)
}

fn energy(x: &[Vec<f32>]) -> f64 {
    x.iter().flatten().map(|v| f64::from(*v).powi(2)).sum()
}

/// A bar of the demo's drum loop cut at its hits: a Drum Sampler pad a
/// slice, a MIDI clip playing each slice where it was — the slices end to
/// end are the bar, and the new track plays what the drums played. One
/// undo step, the drums' clip muted in it.
#[test]
fn a_drum_loop_is_cut_at_its_hits_onto_pads_and_played_as_it_was() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let drums = track(&s, "Drums");
    let clip = s.project().clips_of(drums)[0].clone();
    let start = clip.start;
    let p = s.project();
    let clip_end = clip.end(&p.timeline, p.sample_rate);
    let end = clip_end.min(start + MusicalTime::from_quarters(8.0));
    // The menu offers it.
    let choices = s.slice_choices(drums, Some(clip.id));
    let labels: Vec<&str> = choices.iter().map(|c| c.0.as_str()).collect();
    assert!(
        labels.contains(&"Slice Clip at Transients to New Drum Sampler"),
        "{labels:?}"
    );
    assert!(labels.contains(&"Slice Clip on the Grid to New Sampler (a Key Each)"));
    let before = s.project().clone();
    let tracks = before.tracks.len();
    let steps = s.history_steps().0.len();
    s.dispatch(Action::Slice {
        source: SliceSource::Span {
            track: drums,
            start,
            end,
        },
        by: SliceBy::Transients,
        target: SliceTarget::NewDrums,
        midi: true,
    })
    .unwrap();
    assert!(s.slicing());
    wait(&mut s);
    assert_eq!(s.project().tracks.len(), tracks + 1);
    assert_eq!(s.history_steps().0.len(), steps + 1, "one step");
    let kit_track = track(&s, "Drums Slices");
    let t = s.project().track(kit_track).unwrap();
    assert_eq!(t.kind, TrackKind::Instrument);
    let kit = instrument(&s, kit_track);
    assert_eq!(s.plugin_owner(kit).unwrap().1.plugin.id, builtin::DRUMS);
    let (_, doc) = sampler_state(&s, kit);
    let files: Vec<String> = doc.files.iter().map(|f| f.clone().unwrap()).collect();
    assert!(
        (4..=drums::PADS).contains(&files.len()),
        "{} slices",
        files.len()
    );
    let (at, midi) = midi_clip(&s, kit_track);
    assert_eq!(at, start);
    assert_eq!(midi.notes.len(), files.len());
    // Note k plays pad k where its slice was, as long as it is.
    let p = s.project();
    let base = samples_at(p, start);
    let mut offset = 0i64;
    let mut whole: Vec<Vec<f32>> = Vec::new();
    for (k, (note, file)) in midi.notes.iter().zip(&files).enumerate() {
        assert_eq!(note.key as usize, 36 + k);
        assert_eq!(note.velocity, 127);
        let wav = read_wav(file.as_ref()).unwrap();
        let len = wav.channels[0].len() as i64;
        let on = samples_at(p, start + note.start) - base;
        let off = samples_at(p, start + note.start + note.length) - base;
        assert!(
            (on - offset).abs() <= 1,
            "slice {k} starts at {on}, not {offset}"
        );
        assert!((off - on - len).abs() <= 1, "slice {k} is {len} long");
        offset += len;
        if whole.is_empty() {
            whole = vec![Vec::new(); wav.channels.len()];
        }
        for (w, c) in whole.iter_mut().zip(&wav.channels) {
            w.extend_from_slice(c);
        }
    }
    let span = samples_at(p, end) - base;
    assert_eq!(offset, span, "the slices end to end are the span");
    // The drums' clip falls silent (the MIDI clip plays it now).
    assert!(s.project().clip(clip.id).unwrap().muted == (clip_end <= end));
    assert!(energy(&whole) > 1.0, "the slices hold the drums");

    // Played back, the kit plays what the drums played (at the kit's
    // −3 dB): heard against the drums before slicing.
    let frames = span as usize;
    let then = alone(&before, drums, base, frames);
    let now = alone(s.project(), kit_track, base, frames);
    let dot: f64 = then
        .iter()
        .zip(&now)
        .flat_map(|(a, b)| a.iter().zip(b))
        .map(|(a, b)| f64::from(*a) * f64::from(*b))
        .sum();
    let corr = dot / (energy(&then) * energy(&now)).sqrt();
    assert!(
        corr > 0.99,
        "the kit plays the loop (correlation {corr:.4})"
    );
    let gain_db = 10.0 * (energy(&now) / energy(&then)).log10();
    eprintln!(
        "{} slices; played back: correlation {corr:.5}, {gain_db:+.2} dB",
        files.len()
    );
    assert!(
        (gain_db + 3.0).abs() < 0.5,
        "at the kit's volume: {gain_db:.2} dB"
    );

    // One undo takes it all back.
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(s.project().tracks.len(), tracks);
    assert!(!s.project().clip(clip.id).unwrap().muted);
}

/// On the grid onto a new Sampler in slice mode: its sample is the whole
/// span, cut on every beat, a key each from C1; the clip plays the keys
/// a beat apart.
#[test]
fn a_bass_line_is_cut_on_the_grid_onto_a_sampler_a_key_each() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let bass = track(&s, "Bass");
    let clip = s.project().clips_of(bass)[0].clone();
    let start = clip.start;
    let end = start + MusicalTime::from_quarters(4.0);
    s.dispatch(Action::Slice {
        source: SliceSource::Span {
            track: bass,
            start,
            end,
        },
        by: SliceBy::Grid(MusicalTime::from_quarters(1.0)),
        target: SliceTarget::NewSampler,
        midi: true,
    })
    .unwrap();
    wait(&mut s);
    let t = track(&s, "Bass Sliced");
    let plugin = instrument(&s, t);
    let (block, doc) = sampler_state(&s, plugin);
    assert_eq!(param(&block, sampler::id::SLICES), Some(1.0));
    assert_eq!(param(&block, sampler::id::SLICE_KEY), Some(36.0));
    assert_eq!(doc.files.len(), 1);
    let p = s.project();
    let beat = |q: f64| {
        (samples_at(p, start + MusicalTime::from_quarters(q)) - samples_at(p, start)) as u64
    };
    assert_eq!(doc.slices, vec![beat(1.0), beat(2.0), beat(3.0)]);
    let wav = read_wav(doc.files[0].as_deref().unwrap().as_ref()).unwrap();
    assert_eq!(wav.channels[0].len() as u64, beat(4.0), "the whole span");
    let (_, midi) = midi_clip(&s, t);
    let keys: Vec<u8> = midi.notes.iter().map(|n| n.key).collect();
    assert_eq!(keys, [36, 37, 38, 39]);
    for (k, n) in midi.notes.iter().enumerate() {
        let on = samples_at(p, start + n.start) - samples_at(p, start);
        assert!((on - beat(k as f64) as i64).abs() <= 1);
    }
    // Nothing that does not fit: 40 parts onto 16 pads.
    let err = s
        .dispatch(Action::Slice {
            source: SliceSource::Span {
                track: bass,
                start,
                end,
            },
            by: SliceBy::Parts(40),
            target: SliceTarget::NewDrums,
            midi: false,
        })
        .unwrap_err();
    assert!(err.to_string().contains("do not fit"), "{err}");
}

/// A kit's free pads take the slices (pad 1 is in use), the clip goes on
/// the kit's track; a pad's own sample sliced keeps its pad for the
/// first slice.
#[test]
fn slices_go_to_a_kits_free_pads_and_a_pad_is_sliced_in_the_kit() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let bass = track(&s, "Bass");
    let clip = s.project().clips_of(bass)[0].clone();
    let start = clip.start;
    let end = start + MusicalTime::from_quarters(4.0);
    s.dispatch(Action::MakeSample {
        track: bass,
        start,
        end,
        target: SampleTarget::Drums,
    })
    .unwrap();
    wait(&mut s);
    let kit_track = track(&s, "Bass Drums");
    let kit = instrument(&s, kit_track);
    let first = sampler_state(&s, kit).1.files;
    assert_eq!(first.len(), 1);
    // The menu lists the kit.
    let choices = s.slice_choices(bass, Some(clip.id));
    let to_kit = choices
        .iter()
        .find(|c| c.0 == "Slice Clip at Transients to the Free Pads of ‘Bass Drums’")
        .unwrap();
    assert!(matches!(
        &to_kit.1,
        Action::Slice {
            target: SliceTarget::Pads(k),
            ..
        } if *k == kit
    ));
    s.dispatch(Action::Slice {
        source: SliceSource::Span {
            track: bass,
            start,
            end,
        },
        by: SliceBy::Grid(MusicalTime::from_quarters(2.0)),
        target: SliceTarget::Pads(kit),
        midi: true,
    })
    .unwrap();
    wait(&mut s);
    let files = sampler_state(&s, kit).1.files;
    assert_eq!(files.len(), 3);
    assert_eq!(files[0], first[0], "pad 1 kept");
    assert!(files[1].is_some() && files[2].is_some());
    let (_, midi) = midi_clip(&s, kit_track);
    let keys: Vec<u8> = midi.notes.iter().map(|n| n.key).collect();
    assert_eq!(keys, [37, 38], "pads 2 and 3 from the kit's first note");

    // Pad 1's sample into four parts: pad 1 takes the first, pads 4–6 the
    // rest.
    let menu = s.sample_slice_choices(kit, 0);
    assert_eq!(
        menu[1].0, "Slice Pad 1 into 4 Equal Parts on the Free Pads",
        "{menu:?}"
    );
    s.dispatch(menu[1].1.clone()).unwrap();
    wait(&mut s);
    let files = sampler_state(&s, kit).1.files;
    assert_eq!(files.len(), 6);
    assert_ne!(files[0], first[0], "pad 1 holds its first slice now");
    let whole = read_wav(first[0].as_deref().unwrap().as_ref()).unwrap();
    let part = read_wav(files[0].as_deref().unwrap().as_ref()).unwrap();
    let n = whole.channels[0].len();
    assert_eq!(part.channels[0].len(), n / 4);
    assert_eq!(part.channels[0][100], whole.channels[0][100]);
    assert!(files[3..6].iter().all(Option::is_some), "pads 4–6");
}

/// A Sampler's own sample cut into equal parts plays a key each; the
/// points are set by hand and cleared (each one step), and a MIDI clip
/// plays the slices one after another.
#[test]
fn a_samplers_own_sample_is_sliced_in_place() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let bass = track(&s, "Bass");
    let clip = s.project().clips_of(bass)[0].clone();
    let start = clip.start;
    let end = start + MusicalTime::from_quarters(4.0);
    s.dispatch(Action::MakeSample {
        track: bass,
        start,
        end,
        target: SampleTarget::Sampler,
    })
    .unwrap();
    wait(&mut s);
    let t = track(&s, "Bass Sample");
    let plugin = instrument(&s, t);
    let n = {
        let (_, doc) = sampler_state(&s, plugin);
        read_wav(doc.files[0].as_deref().unwrap().as_ref())
            .unwrap()
            .channels[0]
            .len() as u64
    };
    let menu = s.sample_slice_choices(plugin, 0);
    let labels: Vec<&str> = menu.iter().map(|c| c.0.as_str()).collect();
    assert_eq!(
        labels[..5],
        [
            "Slice at Transients (a Key Each)",
            "Slice into 4 Equal Parts",
            "Slice into 8 Equal Parts",
            "Slice into 16 Equal Parts",
            "Slice into 32 Equal Parts",
        ]
    );
    // Grid and clips mean nothing to a sample.
    assert!(
        s.dispatch(Action::Slice {
            source: SliceSource::Sample { plugin, slot: 0 },
            by: SliceBy::Grid(MusicalTime::from_quarters(1.0)),
            target: SliceTarget::InPlace,
            midi: false,
        })
        .is_err()
    );
    let steps = s.history_steps().0.len();
    s.dispatch(menu[1].1.clone()).unwrap();
    wait(&mut s);
    assert_eq!(s.history_steps().0.len(), steps + 1);
    let (block, doc) = sampler_state(&s, plugin);
    assert_eq!(doc.slices, vec![n / 4, n / 2, 3 * n / 4]);
    assert_eq!(param(&block, sampler::id::SLICES), Some(1.0));
    // The menu now offers the clip and clearing.
    let labels: Vec<String> = s
        .sample_slice_choices(plugin, 0)
        .into_iter()
        .map(|c| c.0)
        .collect();
    assert!(labels.contains(&"Slices to a MIDI Clip at the Playhead".to_string()));
    // By hand.
    s.dispatch(Action::SetSampleSlices {
        plugin,
        slices: vec![n / 3, 2 * n / 3],
    })
    .unwrap();
    assert_eq!(sampler_state(&s, plugin).1.slices, vec![n / 3, 2 * n / 3]);
    assert_eq!(s.history_steps().0.last().unwrap(), "Slice Points");
    s.dispatch(Action::SlicesToMidi(plugin)).unwrap();
    let (_, midi) = midi_clip(&s, t);
    let keys: Vec<u8> = midi.notes.iter().map(|n| n.key).collect();
    assert_eq!(keys, [36, 37, 38]);
    s.dispatch(Action::SetSampleSlices {
        plugin,
        slices: Vec::new(),
    })
    .unwrap();
    assert!(sampler_state(&s, plugin).1.slices.is_empty());
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(sampler_state(&s, plugin).1.slices.len(), 2);
}
