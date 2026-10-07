#![allow(clippy::unwrap_used)]
//! The Guitar Station live: every stage on its reservoir worker, a stereo
//! signal on two threads each, pedals changed and switched, the amplifier,
//! cabinet and microphones changed, a reset -- and no thread but the pedal
//! workshop's (which builds circuits by design) allocates or frees.

use faderframe_audio_graph::{AudioBuffer, NodeIo};
use faderframe_automation::ParameterEvent;
use faderframe_core::{ChannelLayout, ParameterId};
use faderframe_guitar::pedal::Stomp;
use faderframe_guitar::voice::Pedal;
use faderframe_plugin_host::devices::guitar::{self, GuitarProcessor, id};
use faderframe_plugin_host::tap::AnalysisTap;
use faderframe_plugin_host::{
    NO_HARMONY, ParamValues, PluginProcessContext, PluginProcessor, ProcessConfig,
};
use faderframe_transport::TransportInfo;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

struct Counting;
static ARMED: AtomicBool = AtomicBool::new(false);
static EVENTS: AtomicUsize = AtomicUsize::new(0);
/// The workshop's, to show it did build the pedals changed while armed.
static WORKSHOP: AtomicUsize = AtomicUsize::new(0);

fn note() {
    if ARMED.load(Ordering::Relaxed) {
        let workshop = std::thread::current()
            .name()
            .is_some_and(|n| n == "faderframe-pedals");
        if workshop {
            WORKSHOP.fetch_add(1, Ordering::Relaxed);
        } else {
            EVENTS.fetch_add(1, Ordering::Relaxed);
        }
    }
}

// SAFETY: forwards to the system allocator; the counter is an atomic.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        note();
        // SAFETY: the caller's layout, unchanged.
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        note();
        // SAFETY: `p` came from `alloc` with this layout.
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, size: usize) -> *mut u8 {
        note();
        // SAFETY: as `alloc`/`dealloc`.
        unsafe { System.realloc(p, l, size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

const SR: f64 = 48_000.0;
const BLOCK: usize = 128;

#[test]
fn the_guitar_station_does_not_allocate_live() {
    let params = ParamValues::new(guitar::parameters());
    let set = |id: u32, v: f64| params.set_by_id(ParameterId(id), v).unwrap();
    set(
        id::slot(0, id::STOMP),
        Stomp::Pedal(Pedal::BlueChorus).index() as f64,
    );
    set(
        id::slot(1, id::STOMP),
        Stomp::Pedal(Pedal::Green808).index() as f64,
    );
    set(
        id::slot(2, id::STOMP),
        Stomp::Wah(faderframe_guitar::circuits::wah::Build::CryBaby).index() as f64,
    );
    set(id::slot(2, id::AUTO), 1.0);
    set(id::MIC_B, 6.0);
    set(id::B_PAN, 0.6);
    let tap = Arc::new(AnalysisTap::new(params.clone(), guitar::TAP_VALUES));
    let config = ProcessConfig {
        sample_rate: SR,
        max_block_size: BLOCK as u32,
        sidechain: false,
        double_precision: false,
    };
    let mut p = GuitarProcessor::new(
        params.clone(),
        Some(Arc::clone(&tap)),
        &config,
        2,
        true,
        BLOCK,
    )
    .unwrap();
    let mut ins = [AudioBuffer::new(ChannelLayout::Stereo, BLOCK)];
    let mut outs = [
        AudioBuffer::new(ChannelLayout::Stereo, BLOCK),
        AudioBuffer::new(ChannelLayout::Stereo, BLOCK),
    ];
    for b in ins.iter_mut().chain(outs.iter_mut()) {
        b.set_len(BLOCK);
    }
    let transport = TransportInfo::default();
    let events: Vec<Vec<ParameterEvent>> = (0..4)
        .map(|k| {
            vec![
                ParameterEvent {
                    sample_offset: 10,
                    parameter: ParameterId(id::DRIVE),
                    value: 0.2 + 0.2 * k as f32,
                },
                ParameterEvent {
                    sample_offset: 70,
                    parameter: ParameterId(id::slot(2, id::TREADLE)),
                    value: 0.25 * k as f32,
                },
                ParameterEvent {
                    sample_offset: 90,
                    parameter: ParameterId(id::slot(1, id::ON)),
                    value: (k % 2) as f32,
                },
            ]
        })
        .collect();
    let mut at = 0usize;
    // Blocks that sounded, and whether every sample was finite.
    let (sounded, finite) = (std::cell::Cell::new(0usize), std::cell::Cell::new(true));
    let mut run =
        |p: &mut GuitarProcessor, blocks: usize, stereo: bool, events: &[ParameterEvent]| {
            for _ in 0..blocks {
                for c in 0..2 {
                    for (i, x) in ins[0].channel_mut(c).iter_mut().enumerate() {
                        let t = (at + i) as f64 / SR;
                        let v = 0.2 * (std::f64::consts::TAU * 110.0 * t).sin();
                        *x = if stereo && c == 1 {
                            (v * 0.7) as f32
                        } else {
                            v as f32
                        };
                    }
                }
                let ctx = PluginProcessContext {
                    transport: &transport,
                    param_events: events,
                    harmony: &NO_HARMONY,
                    param_mods: &[],
                    note_mods: &[],
                };
                let started = Instant::now();
                p.set_callback_deadline(Some(started + Duration::from_secs_f64(BLOCK as f64 / SR)));
                p.process(
                    &ctx,
                    &mut NodeIo {
                        frames: BLOCK,
                        audio_in: &ins,
                        audio_out: &mut outs,
                        events_in: &[],
                        events_out: &mut [],
                    },
                );
                at += BLOCK;
                let main = outs[0].channel(0);
                sounded.set(sounded.get() + usize::from(main.iter().any(|x| *x != 0.0)));
                finite.set(finite.get() && main.iter().all(|x| x.is_finite()));
                // Paced like a device: the workers have their callback's time.
                if let Some(rest) =
                    Duration::from_secs_f64(BLOCK as f64 / SR).checked_sub(started.elapsed())
                {
                    std::thread::sleep(rest);
                }
            }
        };
    // Every thread started, every circuit settled once.
    run(&mut p, 200, false, &[]);
    ARMED.store(true, Ordering::SeqCst);
    run(&mut p, 100, false, &events[0]);
    // The second channel wakes from the first.
    run(&mut p, 100, true, &events[1]);
    // A different pedal in place 2 (built by the workshop, swapped in on
    // the stage's worker, the old circuits handed back), another amplifier,
    // cabinet, speaker and microphone.
    set(
        id::slot(1, id::STOMP),
        Stomp::Pedal(Pedal::ModernPurple).index() as f64,
    );
    set(id::AMP, 12.0);
    set(id::CABINET, 6.0);
    set(id::SPEAKER, 5.0);
    set(id::MIC_A, 9.0);
    run(&mut p, 400, true, &events[2]);
    set(
        id::slot(1, id::STOMP),
        Stomp::Pedal(Pedal::Rodent).index() as f64,
    );
    set(id::AMP, 8.0);
    set(id::REVERB, 0.4);
    set(id::INTENSITY, 0.5);
    run(&mut p, 400, true, &events[3]);
    p.reset();
    sounded.set(0);
    finite.set(true);
    run(&mut p, 100, true, &[]);
    let allocations = EVENTS.load(Ordering::SeqCst);
    // The counter counts (a deliberate allocation while armed).
    let probe = std::hint::black_box(vec![1u8; 16]);
    drop(probe);
    ARMED.store(false, Ordering::SeqCst);
    assert!(
        EVENTS.load(Ordering::SeqCst) > allocations,
        "the counter works"
    );
    assert_eq!(
        allocations, 0,
        "allocations on the audio thread or the stages' workers"
    );
    assert!(
        WORKSHOP.load(Ordering::SeqCst) > 0,
        "the changed pedals were built while armed"
    );
    // After the reset it plays again -- anywhere in the run: a machine too
    // busy for the workers conceals late blocks (silence), most of them
    // when heavily overloaded.
    let sounded = sounded.get();
    assert!(
        sounded > 0,
        "it plays after the reset: {sounded} of 100 blocks"
    );
    assert!(finite.get());
}
