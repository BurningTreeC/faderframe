#![allow(clippy::unwrap_used)]

use super::*;
use faderframe_core::ParameterId;
use faderframe_plugin_host::PluginFormat;
use faderframe_plugin_host::{
    AudioPortInfo, ParameterInfo, PluginCategory, PluginDescriptor, PluginError, PluginFactory,
    ProcessStatus, TailLength,
};
use std::alloc::{GlobalAlloc, Layout};
use std::cell::Cell;

// The C allocator's counter, as the program's: it sees every allocation:
// Rust's (they reach malloc) and a plugin's own (here: straight from the
// system allocator, as a C plugin's would be). Rust's also pass the
// prefaulting allocator, which counts them as the host's.
thread_local! {
    static ARMED: Cell<bool> = const { Cell::new(false) };
    static COUNT: Cell<u64> = const { Cell::new(0) };
}

fn note() {
    let _ = ARMED.try_with(|a| {
        if a.get() {
            let _ = COUNT.try_with(|c| c.set(c.get() + 1));
        }
    });
}

struct Counting;

// SAFETY: forwards to the prefaulting allocator.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        note();
        // SAFETY: the caller's contract.
        unsafe { faderframe_realtime::memory::Prefaulting.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        note();
        // SAFETY: the caller's contract.
        unsafe { faderframe_realtime::memory::Prefaulting.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        note();
        // SAFETY: the caller's contract.
        unsafe { faderframe_realtime::memory::Prefaulting.realloc(p, l, n) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

fn install_counter() {
    set_alloc_counter(AllocCounter {
        arm: |on| ARMED.with(|a| a.set(on)),
        count: || COUNT.with(Cell::get),
    });
}

fn quick() -> CheckOptions {
    CheckOptions {
        sample_rate: 48_000,
        block: 128,
        start_seconds: 0.1,
        seconds: 0.5,
        resets: 3,
        paced: false,
    }
}

/// A plugin that misbehaves as told.
struct Fake {
    descriptor: PluginDescriptor,
    allocate_every: usize,
    host_allocate_every: usize,
    sleep_every: usize,
    reset_ms: u64,
}

/// An allocation as a C plugin makes it: past Rust's allocator, seen only
/// by the C allocator's counter.
fn c_allocation() {
    let l = Layout::from_size_align(256, 8).unwrap();
    // SAFETY: a valid layout; freed at once.
    unsafe {
        let p = std::alloc::System.alloc(l);
        note();
        std::hint::black_box(p);
        std::alloc::System.dealloc(p, l);
        note();
    }
}

struct FakeProcessor {
    calls: usize,
    allocate_every: usize,
    host_allocate_every: usize,
    sleep_every: usize,
    reset_ms: u64,
}

impl PluginProcessor for FakeProcessor {
    fn process(&mut self, _ctx: &PluginProcessContext<'_>, io: &mut NodeIo<'_>) -> ProcessStatus {
        self.calls += 1;
        if self.allocate_every > 0 && self.calls.is_multiple_of(self.allocate_every) {
            c_allocation();
        }
        if self.host_allocate_every > 0 && self.calls.is_multiple_of(self.host_allocate_every) {
            // Rust code allocating: the host's.
            std::hint::black_box(vec![0u8; 256]);
        }
        if self.sleep_every > 0 && self.calls.is_multiple_of(self.sleep_every) {
            std::thread::sleep(Duration::from_micros(200));
        }
        for b in io.audio_out.iter_mut() {
            b.clear();
        }
        ProcessStatus::Continue
    }

    fn reset(&mut self) {
        let end = Instant::now() + Duration::from_millis(self.reset_ms);
        while Instant::now() < end {
            std::hint::spin_loop();
        }
    }
}

impl PluginInstance for Fake {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }
    fn parameters(&self) -> &[ParameterInfo] {
        &[]
    }
    fn parameter(&mut self, _id: ParameterId) -> Option<f64> {
        None
    }
    fn set_parameter(&mut self, _id: ParameterId, _value: f64) -> Result<(), PluginError> {
        Ok(())
    }
    fn latency_samples(&self) -> u32 {
        0
    }
    fn tail(&self) -> TailLength {
        TailLength::None
    }
    fn save_state(&mut self) -> Result<Vec<u8>, PluginError> {
        Ok(Vec::new())
    }
    fn load_state(&mut self, _data: &[u8]) -> Result<(), PluginError> {
        Ok(())
    }
    fn create_processor(
        &mut self,
        _config: &ProcessConfig,
    ) -> Result<Box<dyn PluginProcessor>, PluginError> {
        Ok(Box::new(FakeProcessor {
            calls: 0,
            allocate_every: self.allocate_every,
            host_allocate_every: self.host_allocate_every,
            sleep_every: self.sleep_every,
            reset_ms: self.reset_ms,
        }))
    }
}

fn fake(allocate_every: usize, sleep_every: usize, reset_ms: u64) -> Fake {
    let port = |channels| AudioPortInfo {
        channels,
        is_main: true,
    };
    Fake {
        descriptor: PluginDescriptor {
            format: PluginFormat::Clap,
            id: "test.fake".into(),
            name: "Fake".into(),
            vendor: "Tests".into(),
            version: "1".into(),
            category: PluginCategory::Effect,
            audio_inputs: vec![port(2)],
            audio_outputs: vec![port(2)],
            note_inputs: 0,
            note_outputs: 0,
        },
        allocate_every,
        host_allocate_every: 0,
        sleep_every,
        reset_ms,
    }
}

/// What the check finds is what the plugin does: allocations and waits
/// while playing (how often), a slow reset; the verdict and advice follow.
#[test]
fn a_misbehaving_plugin_is_caught_doing_what_it_does() {
    install_counter();
    let report = check(&mut fake(10, 25, 3), &quick());
    let f = report.findings();
    assert!(
        f.iter().any(
            |f| matches!(f, Finding::Allocates { calls, of } if *calls > 0 && calls * 8 < *of)
        ),
        "{f:?}"
    );
    #[cfg(target_os = "linux")]
    assert!(
        f.iter().any(|f| matches!(f, Finding::Waits { .. })),
        "{f:?}"
    );
    assert!(
        f.iter().any(|f| matches!(f, Finding::SlowReset { .. })),
        "{f:?}"
    );
    assert_eq!(report.verdict(), Verdict::Problem);
    assert!(report.verdict().advice().contains("sandboxed"));
    // The report survives the trip from the helper process.
    let json = serde_json::to_string(&report).unwrap();
    assert_eq!(serde_json::from_str::<CheckReport>(&json).unwrap(), report);
}

/// What the host's own code allocates is told apart from the plugin's: a
/// FaderFrame bug, not the plugin's.
#[test]
fn the_hosts_allocations_are_told_apart() {
    install_counter();
    let mut f = fake(0, 0, 0);
    f.host_allocate_every = 7;
    let report = check(&mut f, &quick());
    assert_eq!(report.steady.calls_allocating, 0, "{report:?}");
    assert!(report.steady.calls_host_allocating > 0, "{report:?}");
    assert!(
        report
            .findings()
            .iter()
            .any(|f| matches!(f, Finding::HostAllocates { .. })),
        "{:?}",
        report.findings()
    );
}

#[test]
fn a_well_behaved_plugin_comes_out_clean() {
    install_counter();
    let report = check(&mut fake(0, 0, 0), &quick());
    assert_eq!(report.steady.allocations, Some(0), "{report:?}");
    assert_eq!(report.steady.calls_waiting, 0, "{report:?}");
    assert_eq!(report.verdict(), Verdict::Clean, "{:?}", report.findings());
}

/// FaderFrame's own devices keep the rules: the EQ checks clean.
#[test]
fn a_built_in_device_is_realtime_safe() {
    install_counter();
    let mut eq = faderframe_plugin_host::builtin::BuiltinFactory
        .instantiate(faderframe_core::builtin::EQ)
        .unwrap();
    let report = check(eq.as_mut(), &quick());
    assert_eq!(report.steady.calls_allocating, 0, "{report:?}");
    assert_eq!(report.steady.calls_waiting, 0, "{report:?}");
    assert_eq!(report.reset.calls_allocating, 0, "{report:?}");
    assert!(
        report.verdict() <= Verdict::Caution,
        "{:?}",
        report.findings()
    );
}

#[test]
fn findings_follow_the_numbers() {
    let mut r = CheckReport {
        sample_rate: 48_000,
        block: 64,
        ..CheckReport::default()
    };
    assert_eq!(r.verdict(), Verdict::Clean);
    // A block has 1333 µs: a median of 900 µs is heavy.
    r.steady.cpu_median_us = 900.0;
    assert!(matches!(r.findings()[0], Finding::Heavy { .. }));
    // One slow block among quick ones: uneven.
    r.steady.cpu_median_us = 100.0;
    r.steady.cpu_max_us = 1200.0;
    assert!(matches!(r.findings()[0], Finding::Uneven { .. }));
    assert_eq!(r.verdict(), Verdict::Caution);
    // Only the first blocks allocate: harmless.
    r.steady.cpu_max_us = 120.0;
    r.start.calls_allocating = 3;
    assert_eq!(r.findings(), vec![Finding::StartsLazily]);
    assert_eq!(r.verdict(), Verdict::Clean);
    assert_eq!(CheckReport::failed("crashed").verdict(), Verdict::Failed);
}
