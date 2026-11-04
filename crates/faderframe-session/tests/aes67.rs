#![allow(clippy::unwrap_used)]
//! AES67 as the session's audio system, on the loopback interface (TTL 0:
//! nothing leaves the machine): the stream opens with the received
//! streams as inputs and the sent ones as outputs, lists the PTP clock and
//! the streams as its devices, and a received stream that stops arriving
//! is said to have gone — and to be back when it comes again — while the
//! audio plays on.

use faderframe_audio::AudioBackend;
use faderframe_audio_aes67::settings::{
    Aes67Settings, NetSettings, PtpSettings, ReceiveSettings, SendSettings,
};
use faderframe_audio_aes67::stream::{SendFormat, Sender};
use faderframe_audio_aes67::{Aes67Backend, rtp::Encoding};
use faderframe_engine::EngineConfig;
use faderframe_project::Project;
use faderframe_session::{AudioHealth, AudioPreferences, BackendFactory, Session};
use std::net::{Ipv4Addr, SocketAddrV4};
use std::sync::Arc;
use std::time::{Duration, Instant};

const SR: u32 = 48_000;

fn settings() -> Aes67Settings {
    let group = Ipv4Addr::new(239, 69, 255, 7);
    let sdp = format!(
        "v=0\r\no=- 91 1 IN IP4 127.0.0.1\r\ns=Stage box\r\nc=IN IP4 {group}/0\r\nt=0 0\r\n\
         m=audio 28504 RTP/AVP 96\r\na=rtpmap:96 L24/48000/2\r\na=ptime:1\r\n\
         a=mediaclk:direct=0\r\n"
    );
    Aes67Settings {
        interface: Some(Ipv4Addr::LOCALHOST),
        ptp: PtpSettings {
            lead: false,
            ..PtpSettings::default()
        },
        sends: vec![SendSettings {
            name: "Mix".into(),
            channels: 2,
            address: Some(Ipv4Addr::new(239, 69, 255, 8)),
            port: 28_506,
            enabled: true,
        }],
        receives: vec![ReceiveSettings {
            key: "stage".into(),
            name: "Stage box".into(),
            sdp,
            enabled: true,
        }],
        announce: false,
        net: NetSettings {
            ptp_group: Ipv4Addr::new(239, 69, 255, 250),
            ptp_event_port: 28_500,
            ptp_general_port: 28_501,
            ttl: 0,
            ..NetSettings::default()
        },
        ..Aes67Settings::default()
    }
}

fn wait_until(s: &mut Session, what: &str, most: Duration, mut done: impl FnMut(&Session) -> bool) {
    let deadline = Instant::now() + most;
    while !done(s) {
        assert!(Instant::now() < deadline, "{what}");
        s.tick(0.02);
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_stream_that_stops_arriving_goes_and_comes_back() {
    let backend_settings = settings();
    let factory: BackendFactory = Arc::new(move || {
        vec![Box::new(Aes67Backend::new(backend_settings.clone())) as Box<dyn AudioBackend>]
    });
    let mut s = Session::new(Project::new("Network", SR), None, EngineConfig::default()).unwrap();
    let info = s
        .start_audio_with(
            factory,
            &AudioPreferences {
                sample_rate: Some(SR),
                buffer_size: Some(128),
                ..AudioPreferences::default()
            },
        )
        .unwrap();
    assert_eq!(info.backend, "aes67");
    assert_eq!(info.sample_rate, SR);
    assert!(info.input_channels >= 2 && info.output_channels >= 2);
    assert_eq!(info.input_latency, 96, "the link offset, 2 ms");
    let devices = s.stream_devices();
    assert!(devices[0].master && devices[0].name.starts_with("PTP clock"));
    assert!(
        devices
            .iter()
            .any(|d| d.name == "Mix" && d.outputs == vec![0, 1])
    );
    assert_eq!(
        s.input_channel_name(1).as_deref(),
        Some("Stage box · 2"),
        "channels named by their streams"
    );
    // Nobody sends the stage box's stream: it goes away.
    wait_until(&mut s, "the notice", Duration::from_secs(10), |s| {
        s.notices().any(|n| n.text.contains("Stage box went away"))
    });
    assert_eq!(s.audio_status().health, AudioHealth::Running);
    // Now someone does (silence, on the same clock).
    let clock = faderframe_audio_aes67::services(&settings()).ptp.clock();
    let sender = Sender::start(
        SocketAddrV4::new(Ipv4Addr::new(239, 69, 255, 7), 28_504),
        Ipv4Addr::LOCALHOST,
        &SendFormat {
            encoding: Encoding::L24,
            sample_rate: SR,
            channels: 2,
            frames_per_packet: 48,
            ttl: 0,
            multicast_loop: true,
        },
        clock,
    )
    .unwrap();
    let before = s.stream_status().unwrap().callbacks;
    wait_until(&mut s, "back", Duration::from_secs(10), |s| {
        s.notices().any(|n| n.text.contains("Stage box is back"))
    });
    assert!(s.stream_devices().iter().all(|d| d.present));
    let status = s.stream_status().unwrap();
    assert!(status.callbacks > before && !status.shut_down);
    drop(sender);
    s.stop_audio();
}
