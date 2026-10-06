#![allow(clippy::unwrap_used)]
//! MIDI time code output: a full frame when playback starts, then quarter
//! frames that spell the timecode (from the project start's timecode at
//! the chosen rate), on the outputs that have MTC on.

use faderframe_audio::dummy::DummyBackend;
use faderframe_engine::EngineConfig;
use faderframe_project::Project;
use faderframe_session::{
    Action, AudioPreferences, MtcRate, Session, SyncSettings, Timecode, TransportAction,
};
use std::time::{Duration, Instant};

#[test]
fn mtc_goes_out_from_the_project_start_timecode() {
    let mut s = Session::new(Project::new("MTC", 48_000), None, EngineConfig::default()).unwrap();
    s.start_audio(
        vec![Box::new(DummyBackend::default())],
        &AudioPreferences::default(),
    )
    .unwrap();
    let out = s.add_virtual_midi_output("MTC Out");
    s.set_midi_mtc_output("virtual:MTC Out", true);
    s.set_sync_settings(SyncSettings {
        mtc_out_offset: Timecode::parse("01:00:00:00").unwrap(),
        mtc_out_rate: MtcRate::Fps25,
        ..SyncSettings::default()
    });
    s.tick(0.01);
    out.lock().unwrap().clear();
    s.dispatch(Action::Transport(TransportAction::Play))
        .unwrap();
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(400) {
        std::thread::sleep(Duration::from_millis(5));
        s.tick(0.005);
    }
    s.dispatch(Action::Transport(TransportAction::Stop))
        .unwrap();
    std::thread::sleep(Duration::from_millis(50));
    let got: Vec<Vec<u8>> = out.lock().unwrap().iter().map(|(_, b)| b.clone()).collect();
    // The full frame at the start: 01:00:00:00 at 25 fps (rate bits 01).
    let full = got.iter().find(|m| m[0] == 0xF0).unwrap();
    assert_eq!(&full[..9], &[0xF0, 0x7F, 0x7F, 0x01, 0x01, 0x21, 0, 0, 0]);
    // Quarter frames: whole sequences spell 01:00:00:ff with ff even and
    // rising.
    let qf: Vec<u8> = got.iter().filter(|m| m[0] == 0xF1).map(|m| m[1]).collect();
    assert!(qf.len() >= 24, "{} quarter frames", qf.len());
    let first = qf.iter().position(|b| b >> 4 == 0).unwrap();
    let mut last_frame = None;
    for seq in qf[first..].as_chunks::<8>().0 {
        let pieces: Vec<u8> = seq.iter().map(|b| b >> 4).collect();
        assert_eq!(pieces, (0..8).collect::<Vec<u8>>());
        let n = |k: usize| seq[k] & 0x0F;
        let frames = n(0) | n(1) << 4;
        let seconds = n(2) | n(3) << 4;
        let minutes = n(4) | n(5) << 4;
        let hours = n(6) | (n(7) & 1) << 4;
        assert_eq!((hours, minutes), (1, 0));
        assert_eq!(n(7) >> 1, 1, "25 fps");
        let at = u32::from(seconds) * 25 + u32::from(frames);
        assert_eq!(at % 2, 0, "sequences start on even frames");
        if let Some(prev) = last_frame {
            assert_eq!(at, prev + 2, "two frames a sequence");
        }
        last_frame = Some(at);
    }
}
