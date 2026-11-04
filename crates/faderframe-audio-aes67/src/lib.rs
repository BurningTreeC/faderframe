//! AES67 network audio: PCM streams over RTP timed by a PTPv2 clock —
//! what Dante devices (in AES67 mode), RAVENNA, Livewire+ and most
//! broadcast gear exchange.
//!
//! * [`ptp`] — the clock: IEEE 1588-2008 messages, an ordinary clock's
//!   port (best master, end-to-end delay, a PI servo on a software clock
//!   over the process's monotonic time), the default and AES67 media
//!   profiles, leading when no grandmaster is there; or the system clock
//!   followed when linuxptp keeps it.
//! * [`rtp`], [`sdp`], [`sap`] — L16/L24 packets, stream descriptions,
//!   announcements; [`discovery`] listens and announces on the network.
//! * [`ring`] and [`stream`] — receivers write frames at their media
//!   time, senders send at the media clock's pace; the audio side meets
//!   them in [`ring::MediaRing`]s on the media clock.
//! * [`backend::Aes67Backend`] — AES67 as the audio system: the engine's
//!   callback on a timer locked to PTP. [`alongside::Alongside`] — the
//!   streams beside another backend's device, joined through
//!   `faderframe_audio::follow` (resampled onto its clock).
//!
//! Settings come from Preferences ([`settings::Aes67Settings`],
//! [`configure`]); the PTP clock and SAP discovery run while something
//! holds them ([`services`]: a stream, the Preferences page) and are
//! shared by everything in the process.

#![forbid(unsafe_code)]

pub mod alongside;
pub mod backend;
pub mod discovery;
pub mod net;
pub mod ptp;
pub mod ring;
pub mod rtp;
pub mod sap;
pub mod sdp;
pub mod settings;
pub mod stream;

pub use alongside::Alongside;
pub use backend::{Aes67Backend, Aes67Stream, BACKEND_ID};
pub use discovery::{Discovery, DiscoveryConfig};
pub use ptp::service::{PtpService, PtpStatus, ServiceConfig};
pub use settings::{
    Aes67Settings, ClockSource, NetSettings, PtpSettings, ReceiveSettings, SendSettings,
};

use std::sync::{Arc, Mutex, MutexGuard, Weak};

fn locked<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

static SETTINGS: Mutex<Option<Aes67Settings>> = Mutex::new(None);
/// Clocks a stream let go of lately, kept a while (a reopened stream finds
/// its clock still locked).
static LINGER: Mutex<Vec<(std::time::Instant, Arc<PtpService>)>> = Mutex::new(Vec::new());
/// How long a clock nobody holds keeps running.
pub const LINGER_SECONDS: u64 = 30;
static PTP: Mutex<Option<Weak<PtpService>>> = Mutex::new(None);
static SAP: Mutex<Option<Weak<Discovery>>> = Mutex::new(None);

/// The settings backends made from now on use (the shell sets them at
/// start-up and when Preferences changes them).
pub fn configure(settings: Aes67Settings) {
    *locked(&SETTINGS) = Some(settings);
}

/// The settings as configured (defaults before).
pub fn settings() -> Aes67Settings {
    locked(&SETTINGS).clone().unwrap_or_default()
}

/// The clock and discovery for some settings, shared by everyone asking
/// with the same interface and clock settings; they run while one of
/// these is held.
#[derive(Clone, Debug)]
pub struct Services {
    pub ptp: Arc<PtpService>,
    /// `None` where announcements are off.
    pub discovery: Option<Arc<Discovery>>,
}

/// The running services for `settings` (started if none runs, or if the
/// one running has other settings: streams holding the old one keep it
/// until they close).
pub fn services(settings: &Aes67Settings) -> Services {
    let interface = net::interface(settings.interface);
    let config = ServiceConfig {
        interface,
        ptp: settings.ptp.clone(),
        net: settings.net.clone(),
    };
    let ptp = {
        let mut g = locked(&PTP);
        match g.as_ref().and_then(Weak::upgrade) {
            Some(p) if *p.config() == config => p,
            _ => {
                let p = Arc::new(PtpService::start(config));
                *g = Some(Arc::downgrade(&p));
                p
            }
        }
    };
    let discovery = discovery_for(settings);
    Services { ptp, discovery }
}

/// Keep `ptp` running for [`LINGER_SECONDS`] after its last holder lets
/// go: reopening the audio stream (new settings elsewhere, a device back)
/// then finds the clock locked instead of locking anew. A thread drops
/// it after that.
pub(crate) fn linger(ptp: Arc<PtpService>) {
    static REAPER: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    locked(&LINGER).push((std::time::Instant::now(), ptp));
    REAPER.get_or_init(|| {
        let spawned = std::thread::Builder::new()
            .name("faderframe-ptp-linger".into())
            .spawn(|| {
                loop {
                    std::thread::sleep(std::time::Duration::from_secs(1));
                    let gone: Vec<Arc<PtpService>> = {
                        let mut g = locked(&LINGER);
                        let (old, keep): (Vec<_>, Vec<_>) = g
                            .drain(..)
                            .partition(|(at, _)| at.elapsed().as_secs() >= LINGER_SECONDS);
                        *g = keep;
                        old.into_iter().map(|(_, p)| p).collect()
                    };
                    // Stopped outside the lock (joining its threads).
                    drop(gone);
                }
            });
        if let Err(e) = spawned {
            tracing::warn!("PTP: cannot keep clocks lingering: {e}");
        }
    });
}

/// SAP discovery for `settings` (shared like [`services`]; `None` where
/// announcements are off). It listens while the handle is held — the
/// Preferences page holds one to list the streams on the network.
pub fn discovery_for(settings: &Aes67Settings) -> Option<Arc<Discovery>> {
    if !settings.announce {
        return None;
    }
    let config = DiscoveryConfig {
        interface: net::interface(settings.interface),
        group: settings.net.sap_group,
        port: settings.net.sap_port,
        ttl: settings.net.ttl,
        multicast_loop: settings.net.multicast_loop,
    };
    let mut g = locked(&SAP);
    Some(match g.as_ref().and_then(Weak::upgrade) {
        Some(d) if *d.config() == config => d,
        _ => {
            let d = Arc::new(Discovery::start(config));
            *g = Some(Arc::downgrade(&d));
            d
        }
    })
}

/// The clock's state if it runs (nothing is started to answer).
pub fn ptp_status() -> Option<PtpStatus> {
    locked(&PTP)
        .as_ref()
        .and_then(Weak::upgrade)
        .map(|p| p.status())
}

/// The streams heard announced if discovery runs.
pub fn discovered() -> Vec<sap::Heard> {
    locked(&SAP)
        .as_ref()
        .and_then(Weak::upgrade)
        .map(|d| d.streams())
        .unwrap_or_default()
}
