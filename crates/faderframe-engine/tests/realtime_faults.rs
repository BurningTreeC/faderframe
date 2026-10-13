//! The realtime path must not take page faults: memory the audio thread
//! writes is touched (and so mapped) before it gets it — after a graph
//! swap too, whose buffers and delay lines are new. A minor fault is the
//! kernel zeroing and mapping a page (around a microsecond, more under
//! memory pressure); thousands of them make a callback late.
#![cfg(target_os = "linux")]

mod common;

use faderframe_audio::OwnedBuffers;
use faderframe_engine::offline::OfflineRenderer;
use faderframe_engine::{EngineConfig, render_generated_sources};
use faderframe_project::demo::demo_project;
use faderframe_project::{Command, History, PluginSlot};

// As FaderFrame itself runs.
#[global_allocator]
static GLOBAL: faderframe_realtime::memory::Prefaulting = faderframe_realtime::memory::Prefaulting;

/// Minor and major page faults of the calling thread so far.
fn faults() -> (i64, i64) {
    // SAFETY: getrusage fills the struct it is given.
    let mut u: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: a valid pointer to a rusage.
    unsafe { libc::getrusage(libc::RUSAGE_THREAD, &mut u) };
    (u.ru_minflt, u.ru_majflt)
}

fn faults_in(f: impl FnOnce()) -> i64 {
    let (a, b) = faults();
    f();
    let (c, d) = faults();
    (c - a) + (d - b)
}

#[test]
fn processing_takes_no_page_faults() {
    const SR: u32 = 48_000;
    const BLOCK: usize = 256;
    let mut project = demo_project(SR);
    let sources = render_generated_sources(&project, SR);
    let config = EngineConfig {
        sample_rate: SR,
        ..EngineConfig::default()
    };
    let mut r = OfflineRenderer::new(&project, &sources, config, BLOCK, 2).unwrap();
    // Code and constants mapped, as the programs do at start.
    faderframe_realtime::memory::prefault_code();
    let mut bufs = OwnedBuffers::new(2, 2, BLOCK);
    r.play_from(0).unwrap();
    for _ in 0..8 {
        r.processor.process_device(&mut bufs);
    }
    let steady = faults_in(|| {
        for _ in 0..400 {
            r.processor.process_device(&mut bufs);
        }
    });
    // A graph with new buffers and long delay lines: devices with
    // latency on two tracks (the others wait for them).
    let mut h = History::default();
    for name in ["Bass", "Pluck"] {
        let track = project.tracks.iter().find(|t| t.name == name).unwrap().id;
        let id = project.ids.allocate();
        let slot = PluginSlot {
            id,
            plugin: faderframe_project::PluginRef::builtin(
                faderframe_core::builtin::LATENCY_PROBE,
                "Latency",
            ),
            bypass: false,
            parameters: vec![faderframe_project::SavedParameter {
                id: faderframe_core::ParameterId(0),
                value: 6400.0,
            }],
            state: None,
            sidechain: None,
        };
        let index = project.track(track).unwrap().inserts.len();
        let impact = h
            .apply(&mut project, Command::InsertPlugin { track, index, slot })
            .unwrap();
        r.controller.sync(&project, &sources, impact).unwrap();
    }
    let swapped = faults_in(|| {
        for _ in 0..200 {
            r.processor.process_device(&mut bufs);
        }
    });
    eprintln!("page faults: steady {steady}, after the swap {swapped}");
    assert_eq!(steady, 0, "faults while playing");
    assert_eq!(swapped, 0, "faults after a graph swap");
}

/// Recording: the capture rings (seconds of every armed input, made when
/// recording begins) are mapped before the audio thread fills them.
#[test]
fn recording_takes_no_page_faults() {
    use faderframe_core::ChannelLayout;
    use faderframe_engine::RecordTarget;
    use faderframe_project::TrackKind;
    use faderframe_transport::TransportCommand;
    const SR: u32 = 48_000;
    const BLOCK: usize = 256;
    let mut tp = common::TestProject::new(SR);
    let tracks: Vec<_> = (0..8)
        .map(|i| tp.track(TrackKind::Audio, &format!("In {i}"), ChannelLayout::Stereo))
        .collect();
    let config = EngineConfig {
        sample_rate: SR,
        ..EngineConfig::default()
    };
    let mut r = OfflineRenderer::new(&tp.project, &tp.sources, config, BLOCK, 2).unwrap();
    faderframe_realtime::memory::prefault_code();
    let targets = tracks
        .iter()
        .enumerate()
        .map(|(i, &track)| RecordTarget {
            track,
            first_channel: (i * 2) as u16 % 2,
            channels: 2,
        })
        .collect();
    r.play_from(0).unwrap();
    let mut bufs = OwnedBuffers::new(2, 2, BLOCK);
    for _ in 0..8 {
        r.processor.process_device(&mut bufs);
    }
    let mut streams = r
        .controller
        .begin_recording(targets, 0, i64::MAX, 4.0)
        .unwrap();
    r.controller
        .transport(TransportCommand::SetRecording(true))
        .unwrap();
    // Two seconds: half of every ring written for the first time.
    let faults = faults_in(|| {
        for _ in 0..(2 * SR as usize / BLOCK) {
            r.processor.process_device(&mut bufs);
            // The writer's side keeps up (as its thread would).
            while streams.headers.pop().is_ok() {}
            let n = streams.data.slots();
            if let Ok(chunk) = streams.data.read_chunk(n) {
                chunk.commit_all();
            }
        }
    });
    eprintln!("page faults while recording: {faults}");
    assert_eq!(faults, 0, "faults while recording");
}
