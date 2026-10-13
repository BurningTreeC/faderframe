//! Controllers FaderFrame knows by name: keyboards and surfaces with a DAW
//! mode, each a [`Controller`] with its protocol (a [`MackieProfile`] for
//! those that speak Mackie Control, a [`Profile`](crate::mapped::Profile)
//! for vendor protocols), what to do on the device, and how to find its
//! ports ([`detect`]): by the device's (ALSA client's) name and the order
//! of its ports, since port names are cut to 31 characters and differ
//! between kernels.
//!
//! Only what sources document went in: vendors' manuals and programmer's
//! guides, else open-source integrations that agree (each module names
//! its own). Controllers that speak plain Mackie Control or HUI otherwise
//! (Oxygen Pro and Keystation in their Mackie modes, nanoKONTROL2's DAW
//! modes, X-Touch extenders past one) work as "Mackie Control".

pub mod akai;
pub mod arturia;
pub mod korg;
pub mod maudio;
pub mod ni;
pub mod novation;
pub mod presonus;

use crate::Protocol;
use crate::mackie::{Display, MCU, Mackie, MackieProfile, XT};
use crate::mapped::Mapped;
use serde::{Deserialize, Serialize};

/// Arturia's SysEx header (keyboards).
const ARTURIA: [u8; 6] = [0xF0, 0x00, 0x20, 0x6B, 0x7F, 0x42];

/// KeyLab Essential (mkI): Mackie on its DAW port. FaderFrame selects the
/// Mackie DAW mode (`… 02 00 40 51 00`) and recalls the DAW map (`… 05
/// 02`); the pads (channel 11) are left alone; its 2 × 16 LCD shows the
/// selected track. Sources: Bitwig's `essential` extension, Ableton's
/// `KeyLab_Essential`, MiniDexed's DAW controller.
pub const KEYLAB_ESSENTIAL: MackieProfile = MackieProfile {
    hello: &[
        &[
            ARTURIA[0], ARTURIA[1], ARTURIA[2], ARTURIA[3], ARTURIA[4], ARTURIA[5], 0x02, 0x00,
            0x40, 0x51, 0x00, 0xF7,
        ],
        &[
            ARTURIA[0], ARTURIA[1], ARTURIA[2], ARTURIA[3], ARTURIA[4], ARTURIA[5], 0x05, 0x02,
            0xF7,
        ],
    ],
    channel_1: true,
    display: Display::Arturia,
    timecode: false,
    meters: false,
    ..MackieProfile::plain(MCU)
};

/// KeyLab mkII: its "Standard MCU" DAW map on the DAW port (`… 02 00 40
/// 52 00`), then DAW mode (`… 05 02`); the same LCD. Sources: Bitwig's
/// `keylab/mk2`, Ableton's `KeyLab_mkII`, Ardour's `arturiaMkII.device`.
pub const KEYLAB_MK2: MackieProfile = MackieProfile {
    hello: &[
        &[
            ARTURIA[0], ARTURIA[1], ARTURIA[2], ARTURIA[3], ARTURIA[4], ARTURIA[5], 0x02, 0x00,
            0x40, 0x52, 0x00, 0xF7,
        ],
        &[
            ARTURIA[0], ARTURIA[1], ARTURIA[2], ARTURIA[3], ARTURIA[4], ARTURIA[5], 0x05, 0x02,
            0xF7,
        ],
    ],
    channel_1: true,
    display: Display::Arturia,
    timecode: false,
    meters: false,
    ..MackieProfile::plain(MCU)
};

/// BeatStep Pro, its second port with KNOBS on the MCU/HUI layer: the top
/// encoders V-pots, the bottom ones faders (manual §11.2).
pub const BEATSTEP_PRO: MackieProfile = MackieProfile {
    display: Display::None,
    timecode: false,
    meters: false,
    ..MackieProfile::plain(MCU)
};

/// X-Touch in MC mode: the Mackie Control, scribble strips in the tracks'
/// colours (`F0 00 00 66 14 72 …`, Ardour `display_colors_on_xtouch`).
pub const X_TOUCH: MackieProfile = MackieProfile {
    colours: true,
    ..MackieProfile::plain(MCU)
};

/// X-Touch Extender in MC mode (the extender's id, coloured too).
pub const X_TOUCH_EXTENDER: MackieProfile = MackieProfile {
    colours: true,
    ..MackieProfile::plain(XT)
};

/// X-Touch One in MC Std mode: one strip, the selected track; its SCRUB
/// key sends F8 (midizap's `XTouchONE` example).
pub const X_TOUCH_ONE: MackieProfile = MackieProfile {
    strips: 1,
    remap: &[(0x3D, 0x65)],
    ..MackieProfile::plain(MCU)
};

/// X-Touch Mini, switched into MC mode (CC 127 = 1 on its global channel
/// 1, `B0 7F 01`; back with `… 00`): eight V-pots, the fader the master's
/// (pitch bend channel 9), no displays. Sources: the X-TOUCH MINI quick
/// start guide (MC map, RX data), measured by github.com/jskeet/DemoCode.
pub const X_TOUCH_MINI: MackieProfile = MackieProfile {
    hello: &[&[0xB0, 0x7F, 0x01]],
    goodbye: &[&[0xB0, 0x7F, 0x00]],
    display: Display::None,
    timecode: false,
    meters: false,
    ..MackieProfile::plain(MCU)
};

/// iCON Platform M+ in its Mackie mode: Read and Write swapped, the Mixer
/// button flips (Ardour's `platform_m+.device`, Bitwig's
/// `IconPlatformMExtensionDefinition`); no time display or meters.
pub const PLATFORM_M: MackieProfile = MackieProfile {
    remap: &[(0x4A, 0x4B), (0x4B, 0x4A), (0x54, 0x32)],
    timecode: false,
    meters: false,
    ..MackieProfile::plain(MCU)
};

/// A controller FaderFrame knows (see the module docs).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Controller {
    KeyLabEssential,
    KeyLabEssentialMk3,
    KeyLabMk2,
    MiniLab3,
    BeatStepPro,
    LaunchkeyMk3,
    Launchkey88Mk3,
    LaunchkeyMiniMk3,
    LaunchkeyMk4,
    LaunchkeyMiniMk4,
    SlMk3,
    NanoKontrol2,
    KompleteKontrolMk3,
    OxygenPro,
    OxygenMkv,
    MpkMiniPlus,
    XTouch,
    XTouchExtender,
    XTouchOne,
    XTouchMini,
    FaderPort8,
    FaderPort16,
    FaderPort,
    PlatformM,
}

/// Which of a device's ports.
#[derive(Clone, Copy, Debug)]
enum Pick {
    /// The n-th (from 0) in the order they are listed.
    Nth(usize),
    /// The first whose name holds this, else the n-th.
    Named(&'static str, usize),
}

impl Controller {
    pub const ALL: [Controller; 24] = [
        Controller::KeyLabEssential,
        Controller::KeyLabEssentialMk3,
        Controller::KeyLabMk2,
        Controller::MiniLab3,
        Controller::BeatStepPro,
        Controller::LaunchkeyMk3,
        Controller::Launchkey88Mk3,
        Controller::LaunchkeyMiniMk3,
        Controller::LaunchkeyMk4,
        Controller::LaunchkeyMiniMk4,
        Controller::SlMk3,
        Controller::NanoKontrol2,
        Controller::KompleteKontrolMk3,
        Controller::OxygenPro,
        Controller::OxygenMkv,
        Controller::MpkMiniPlus,
        Controller::XTouch,
        Controller::XTouchExtender,
        Controller::XTouchOne,
        Controller::XTouchMini,
        Controller::FaderPort8,
        Controller::FaderPort16,
        Controller::FaderPort,
        Controller::PlatformM,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Controller::KeyLabEssential => "Arturia KeyLab Essential",
            Controller::KeyLabEssentialMk3 => "Arturia KeyLab Essential mk3",
            Controller::KeyLabMk2 => "Arturia KeyLab mkII",
            Controller::MiniLab3 => "Arturia MiniLab 3",
            Controller::BeatStepPro => "Arturia BeatStep Pro (MCU knobs)",
            Controller::LaunchkeyMk3 => "Novation Launchkey MK3",
            Controller::Launchkey88Mk3 => "Novation Launchkey 88 MK3",
            Controller::LaunchkeyMiniMk3 => "Novation Launchkey Mini MK3",
            Controller::LaunchkeyMk4 => "Novation Launchkey MK4",
            Controller::LaunchkeyMiniMk4 => "Novation Launchkey Mini MK4",
            Controller::SlMk3 => "Novation SL MkIII",
            Controller::NanoKontrol2 => "Korg nanoKONTROL2",
            Controller::KompleteKontrolMk3 => "NI Komplete Kontrol S MK3",
            Controller::OxygenPro => "M-Audio Oxygen Pro",
            Controller::OxygenMkv => "M-Audio Oxygen MKV",
            Controller::MpkMiniPlus => "Akai MPK mini Plus",
            Controller::XTouch => "Behringer X-Touch",
            Controller::XTouchExtender => "Behringer X-Touch Extender",
            Controller::XTouchOne => "Behringer X-Touch One",
            Controller::XTouchMini => "Behringer X-Touch Mini",
            Controller::FaderPort8 => "PreSonus FaderPort 8",
            Controller::FaderPort16 => "PreSonus FaderPort 16",
            Controller::FaderPort => "PreSonus FaderPort (2018, one fader)",
            Controller::PlatformM => "iCON Platform M+",
        }
    }

    /// What to do on the device (and which port it is).
    pub fn setup(self) -> &'static str {
        match self {
            Controller::KeyLabEssential => {
                "Its DAW port (the second); FaderFrame selects Mackie mode and the DAW map"
            }
            Controller::KeyLabEssentialMk3 => {
                "Its MIDI port (the keys keep playing); FaderFrame connects and selects the DAW program"
            }
            Controller::KeyLabMk2 => {
                "Its DAW port (the second); FaderFrame selects the MCU map and DAW mode"
            }
            Controller::MiniLab3 => {
                "Its MIDI port (the keys keep playing); select the DAW program (Shift + pad 3)"
            }
            Controller::BeatStepPro => {
                "Its second port; press KNOBS for the MCU/HUI layer (upper encoders pan, lower ones are faders)"
            }
            Controller::LaunchkeyMk3
            | Controller::Launchkey88Mk3
            | Controller::LaunchkeyMiniMk3
            | Controller::LaunchkeyMk4
            | Controller::LaunchkeyMiniMk4 => {
                "Its DAW port (the second); FaderFrame turns DAW mode on — the pads play the clip launcher"
            }
            Controller::SlMk3 => "Its InControl port; press InControl on the keyboard",
            Controller::NanoKontrol2 => {
                "FaderFrame switches it to its native mode (global MIDI channel 1, the factory setting)"
            }
            Controller::KompleteKontrolMk3 => "Its DAW port (the second)",
            Controller::OxygenPro => {
                "Its third port (Mackie/HUI); FaderFrame switches it to DAW mode — the pads play the clip launcher"
            }
            Controller::OxygenMkv => "Its second port; FaderFrame switches it to DAW mode",
            Controller::MpkMiniPlus => {
                "Its MIDI port (the keys keep playing); transport set to CC (\"Trpt\" On/Off)"
            }
            Controller::XTouch | Controller::XTouchExtender => {
                "MC mode (hold CH1 SELECT while switching it on)"
            }
            Controller::XTouchOne => {
                "MC Std mode (hold the encoder while switching it on); its fader follows the selected track"
            }
            Controller::XTouchMini => "FaderFrame switches it to MC mode",
            Controller::FaderPort8 | Controller::FaderPort16 => {
                "Studio One mode (hold Select 1 and 2 while switching it on)"
            }
            Controller::FaderPort => {
                "Studio One mode (hold Next while switching it on, then Solo); its fader follows the selected track, the mode buttons choose what the encoder and Prev/Next do"
            }
            Controller::PlatformM => "Its Mackie mode",
        }
    }

    pub fn protocol(self) -> Box<dyn Protocol> {
        let mackie = |p: MackieProfile| -> Box<dyn Protocol> { Box::new(Mackie::with_profile(p)) };
        let mapped =
            |p: &'static crate::mapped::Profile| -> Box<dyn Protocol> { Box::new(Mapped::new(p)) };
        match self {
            Controller::KeyLabEssential => mackie(KEYLAB_ESSENTIAL),
            Controller::KeyLabEssentialMk3 => mapped(&arturia::KEYLAB_ESSENTIAL_MK3),
            Controller::KeyLabMk2 => mackie(KEYLAB_MK2),
            Controller::MiniLab3 => mapped(&arturia::MINILAB_3),
            Controller::BeatStepPro => mackie(BEATSTEP_PRO),
            Controller::LaunchkeyMk3 => mapped(&novation::LAUNCHKEY_MK3),
            Controller::Launchkey88Mk3 => mapped(&novation::LAUNCHKEY_MK3_88),
            Controller::LaunchkeyMiniMk3 => mapped(&novation::LAUNCHKEY_MINI_MK3),
            Controller::LaunchkeyMk4 => mapped(&novation::LAUNCHKEY_MK4),
            Controller::LaunchkeyMiniMk4 => mapped(&novation::LAUNCHKEY_MINI_MK4),
            Controller::SlMk3 => mapped(&novation::SL_MK3),
            Controller::NanoKontrol2 => mapped(&korg::NANOKONTROL2),
            Controller::KompleteKontrolMk3 => mapped(&ni::KOMPLETE_KONTROL_MK3),
            Controller::OxygenPro => mapped(&maudio::OXYGEN_PRO),
            Controller::OxygenMkv => mapped(&maudio::OXYGEN_MKV),
            Controller::MpkMiniPlus => mapped(&akai::MPK_MINI_PLUS),
            Controller::XTouch => mackie(X_TOUCH),
            Controller::XTouchExtender => mackie(X_TOUCH_EXTENDER),
            Controller::XTouchOne => mackie(X_TOUCH_ONE),
            Controller::XTouchMini => mackie(X_TOUCH_MINI),
            Controller::FaderPort8 => mapped(&presonus::FADERPORT_8),
            Controller::FaderPort16 => mapped(&presonus::FADERPORT_16),
            Controller::FaderPort => Box::new(presonus::FaderPortOne::new()),
            Controller::PlatformM => mackie(PLATFORM_M),
        }
    }

    /// Whether a device (client) name is this controller's, and which of
    /// its ports (inputs and outputs alike) is the one.
    fn rule(self, client: &str) -> Option<Pick> {
        let c = client.to_lowercase();
        let has = |s: &str| c.contains(s);
        let ok = match self {
            Controller::KeyLabEssential => has("keylab essential") && !has("mk3"),
            Controller::KeyLabEssentialMk3 => {
                (has("kl essential") || has("keylab essential")) && has("mk3")
            }
            Controller::KeyLabMk2 => has("keylab mkii"),
            Controller::MiniLab3 => has("minilab3") || has("minilab 3"),
            Controller::BeatStepPro => has("beatstep pro"),
            Controller::LaunchkeyMk3 => has("launchkey mk3") && !has("mini") && !has("88"),
            Controller::Launchkey88Mk3 => has("launchkey mk3") && has("88"),
            Controller::LaunchkeyMiniMk3 => has("launchkey mini mk3"),
            Controller::LaunchkeyMk4 => has("launchkey mk4") && !has("mini"),
            Controller::LaunchkeyMiniMk4 => has("launchkey mini mk4"),
            Controller::SlMk3 => has("sl mkiii"),
            Controller::NanoKontrol2 => has("nanokontrol2"),
            Controller::KompleteKontrolMk3 => has("kontrol s") && has("mk3"),
            Controller::OxygenPro => has("oxygen pro"),
            // Its device name says nothing apart: chosen by hand.
            Controller::OxygenMkv => false,
            Controller::MpkMiniPlus => has("mpk mini plus"),
            Controller::XTouch => c == "x-touch",
            Controller::XTouchExtender => has("x-touch-ext"),
            Controller::XTouchOne => has("x-touch one"),
            Controller::XTouchMini => has("x-touch mini"),
            Controller::FaderPort8 => has("fp8"),
            Controller::FaderPort16 => has("fp16"),
            Controller::FaderPort => has("fp2"),
            Controller::PlatformM => has("platform m+"),
        };
        if !ok {
            return None;
        }
        Some(match self {
            Controller::KeyLabEssential
            | Controller::KeyLabMk2
            | Controller::BeatStepPro
            | Controller::LaunchkeyMk3
            | Controller::Launchkey88Mk3
            | Controller::LaunchkeyMiniMk3
            | Controller::LaunchkeyMk4
            | Controller::LaunchkeyMiniMk4
            | Controller::KompleteKontrolMk3
            | Controller::OxygenMkv => Pick::Nth(1),
            Controller::KeyLabEssentialMk3 => Pick::Named("MIDI", 0),
            Controller::SlMk3 => Pick::Named("InCo", 1),
            Controller::OxygenPro => Pick::Nth(2),
            _ => Pick::Nth(0),
        })
    }
}

/// A controller found among the ports, with the ports to use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Detected {
    pub controller: Controller,
    pub input: String,
    pub output: String,
}

/// The device (sequencer client) of a port key ("Client:Port").
fn client_of(key: &str) -> &str {
    key.split_once(':').map_or(key, |(c, _)| c)
}

/// Ports grouped by device, in listing order.
fn by_client(keys: &[String]) -> Vec<(&str, Vec<&str>)> {
    let mut groups: Vec<(&str, Vec<&str>)> = Vec::new();
    for k in keys {
        let c = client_of(k);
        match groups.iter_mut().find(|(g, _)| *g == c) {
            Some((_, v)) => v.push(k),
            None => groups.push((c, vec![k])),
        }
    }
    groups
}

fn pick<'a>(ports: &[&'a str], p: Pick) -> Option<&'a str> {
    match p {
        Pick::Nth(n) => ports.get(n).copied(),
        Pick::Named(name, n) => ports
            .iter()
            .find(|k| {
                k.split_once(':')
                    .is_some_and(|(_, port)| port.contains(name))
            })
            .copied()
            .or_else(|| ports.get(n).copied()),
    }
}

/// Controllers among the input and output ports (keys as the MIDI hub
/// gives them, listed in order), with the ports each should use.
pub fn detect(inputs: &[String], outputs: &[String]) -> Vec<Detected> {
    let ins = by_client(inputs);
    let outs = by_client(outputs);
    let mut found = Vec::new();
    for (client, ports) in &ins {
        let Some((controller, p)) = Controller::ALL
            .iter()
            .find_map(|c| c.rule(client).map(|p| (*c, p)))
        else {
            continue;
        };
        let Some(input) = pick(ports, p) else {
            continue;
        };
        let output = outs
            .iter()
            .find(|(c, _)| c == client)
            .and_then(|(_, ports)| pick(ports, p))
            .unwrap_or_default();
        found.push(Detected {
            controller,
            input: input.to_string(),
            output: output.to_string(),
        });
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{StripState, SurfaceInput, SurfaceState};

    fn keys(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn controllers_are_found_by_device_and_port_order() {
        let inputs = keys(&[
            "Midi Through:Midi Through Port-0",
            "Arturia KeyLab Essential 61:Arturia KeyLab Essential 61 MID",
            "Arturia KeyLab Essential 61:Arturia KeyLab Essential 61 MID #2",
            "KL Essential 49 mk3:KL Essential 49 mk3 MIDI",
            "KL Essential 49 mk3:KL Essential 49 mk3 DINTHRU",
            "KL Essential 49 mk3:KL Essential 49 mk3 MCU/HUI",
            "Launchkey MK4 49:Launchkey MK4 49 MIDI In",
            "Launchkey MK4 49:Launchkey MK4 49 DAW In",
            "nanoKONTROL2:nanoKONTROL2 MIDI 1",
            "X-Touch:X-Touch X-TOUCH_INT",
            "Novation SL MkIII:Novation SL MkIII SL MkIII MIDI",
            "Novation SL MkIII:Novation SL MkIII SL MkIII InCo",
        ]);
        let found = detect(&inputs, &inputs);
        let summary: Vec<(Controller, &str)> = found
            .iter()
            .map(|d| (d.controller, d.input.as_str()))
            .collect();
        assert_eq!(
            summary,
            [
                (
                    Controller::KeyLabEssential,
                    "Arturia KeyLab Essential 61:Arturia KeyLab Essential 61 MID #2"
                ),
                (
                    Controller::KeyLabEssentialMk3,
                    "KL Essential 49 mk3:KL Essential 49 mk3 MIDI"
                ),
                (
                    Controller::LaunchkeyMk4,
                    "Launchkey MK4 49:Launchkey MK4 49 DAW In"
                ),
                (Controller::NanoKontrol2, "nanoKONTROL2:nanoKONTROL2 MIDI 1"),
                (Controller::XTouch, "X-Touch:X-Touch X-TOUCH_INT"),
                (
                    Controller::SlMk3,
                    "Novation SL MkIII:Novation SL MkIII SL MkIII InCo"
                ),
            ]
        );
        assert_eq!(found[2].output, "Launchkey MK4 49:Launchkey MK4 49 DAW In");
    }

    #[test]
    fn every_controller_has_a_protocol_and_a_setup() {
        for c in Controller::ALL {
            assert!(!c.setup().is_empty());
            let p = c.protocol();
            assert!(p.strips() >= 1, "{}", c.label());
        }
        // Single strips follow the selection.
        assert!(Controller::XTouchOne.protocol().follows_selection());
        assert!(Controller::FaderPort.protocol().follows_selection());
        // Keyboards sharing their port claim their controls only.
        assert!(Controller::KeyLabEssentialMk3.protocol().claims().is_some());
        assert!(Controller::LaunchkeyMk3.protocol().claims().is_none());
    }

    #[test]
    fn the_keylab_essential_speaks_mackie_and_leaves_its_pads() {
        let mut p = Controller::KeyLabEssential.protocol();
        let mut out = Vec::new();
        let state = SurfaceState {
            strips: vec![
                StripState {
                    present: true,
                    name: "Lead".into(),
                    selected: true,
                    ..StripState::default()
                };
                8
            ],
            ..SurfaceState::default()
        };
        p.update(&state, 0.0, &mut out);
        // Mackie mode, the DAW map, then the LCD (Arturia's).
        assert_eq!(out[0][6..11], [0x02, 0x00, 0x40, 0x51, 0x00]);
        assert_eq!(out[1][6..8], [0x05, 0x02]);
        assert!(out.iter().any(
            |m| m.starts_with(&[0xF0, 0x00, 0x20, 0x6B, 0x7F, 0x42, 0x04])
                && m.windows(4).any(|w| w == b"Lead")
        ));
        assert!(
            !out.iter().any(|m| m.starts_with(&[0xF0, 0x00, 0x00, 0x66])),
            "no Mackie LCD"
        );
        let mut inputs = Vec::new();
        // A pad (note 36, channel 11): not a Mackie button.
        p.receive(&[0x9A, 36, 100], &mut inputs);
        assert!(inputs.is_empty());
        p.receive(&[0x90, 0x5E, 0x7F], &mut inputs);
        p.receive(&[0xB0, 0x10, 0x41], &mut inputs);
        assert_eq!(
            inputs,
            [
                SurfaceInput::Button {
                    button: crate::Button::Play,
                    pressed: true
                },
                SurfaceInput::Pot {
                    strip: 0,
                    delta: -1
                },
            ]
        );
    }

    #[test]
    fn the_x_touch_colours_its_scribble_strips() {
        let mut p = Controller::XTouch.protocol();
        let mut strips = vec![StripState::default(); 8];
        strips[0] = StripState {
            present: true,
            color: [255, 0, 0],
            ..StripState::default()
        };
        strips[1] = StripState {
            present: true,
            color: [0, 0, 0],
            ..StripState::default()
        };
        let state = SurfaceState {
            strips,
            ..SurfaceState::default()
        };
        let mut out = Vec::new();
        p.update(&state, 0.0, &mut out);
        assert!(out.contains(&vec![
            0xF0, 0x00, 0x00, 0x66, 0x14, 0x72, 1, 7, 0, 0, 0, 0, 0, 0, 0xF7
        ]));
    }

    #[test]
    fn the_platform_m_swaps_read_and_write_both_ways() {
        let mut p = Controller::PlatformM.protocol();
        let mut inputs = Vec::new();
        p.receive(&[0x90, 0x4A, 0x7F], &mut inputs);
        assert_eq!(
            inputs,
            [SurfaceInput::Button {
                button: crate::Button::Automation(crate::AutomationButton::Write),
                pressed: true
            }]
        );
        let state = SurfaceState {
            automation: Some(crate::AutomationButton::Write),
            strips: vec![StripState::default(); 8],
            ..SurfaceState::default()
        };
        let mut out = Vec::new();
        p.update(&state, 0.0, &mut out);
        assert!(
            out.contains(&vec![0x90, 0x4A, 0x7F]),
            "Write lit on its own note"
        );
    }
}
