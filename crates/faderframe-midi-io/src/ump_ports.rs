//! MIDI 2.0 ports: what FaderFrame sends them (MIDI 2.0 protocol, group 1),
//! and which sequencer clients are left to them.

use faderframe_midi::MidiOutputEvent;
use faderframe_midi::ump::{Message, Midi1ToMidi2, per_note_message, sysex7};

/// The sequencer client number at the end of a port's full name
/// ("MPK mini 3:MPK mini 3 MIDI 1 32:0" → 32).
pub fn sequencer_client(full: &str) -> Option<u8> {
    let (_, tail) = full.rsplit_once(' ')?;
    let (c, p) = tail.split_once(':')?;
    p.parse::<u32>().ok()?;
    c.parse().ok()
}

/// An output event's packets: channel messages translated to MIDI 2.0,
/// per-note expressions as per-note controllers, system messages as they
/// are.
pub fn event_words(t: &mut Midi1ToMidi2, ev: &MidiOutputEvent, out: &mut Vec<u32>) {
    let message = if let Some((channel, key, kind, value)) = ev.expression {
        Message::Midi2 {
            group: 0,
            channel,
            voice: per_note_message(key, kind, value.get()),
        }
    } else {
        let b = ev.bytes();
        let Some(&status) = b.first() else {
            return;
        };
        if status >= 0xF0 {
            Message::System {
                group: 0,
                status,
                data: [
                    b.get(1).copied().unwrap_or(0),
                    b.get(2).copied().unwrap_or(0),
                ],
            }
        } else {
            let mut m = [0u8; 3];
            m[..b.len()].copy_from_slice(b);
            let Some(voice) = t.translate(m) else {
                return;
            };
            Message::Midi2 {
                group: 0,
                channel: status & 0xF,
                voice,
            }
        }
    };
    out.extend_from_slice(message.to_ump().words());
}

/// Bytes sent as they are (SysEx `F0 … F7`, or a channel or system
/// message) as packets.
pub fn bytes_words(t: &mut Midi1ToMidi2, bytes: &[u8], out: &mut Vec<u32>) {
    match bytes {
        [0xF0, rest @ ..] => {
            let data = rest.strip_suffix(&[0xF7]).unwrap_or(rest);
            for p in sysex7(0, data) {
                out.extend_from_slice(p.words());
            }
        }
        _ => {
            if let Some(ev) = MidiOutputEvent::new(0, 0, bytes) {
                event_words(t, &ev, out);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use faderframe_midi::ump::{Voice2, packets};
    use faderframe_midi::{ExpressionValue, NoteExpressionKind};

    #[test]
    fn events_become_midi2_packets() {
        let mut t = Midi1ToMidi2::new();
        let mut w = Vec::new();
        let on = MidiOutputEvent::new(0, 0, &[0x92, 60, 127]).unwrap();
        event_words(&mut t, &on, &mut w);
        let bend = MidiOutputEvent::expression(
            0,
            0,
            2,
            60,
            NoteExpressionKind::Tuning,
            ExpressionValue::new(1.5),
        );
        event_words(&mut t, &bend, &mut w);
        bytes_words(&mut t, &[0xF0, 0x7E, 0x7F, 0x06, 0x01, 0xF7], &mut w);
        bytes_words(&mut t, &[0xF8], &mut w);
        let got: Vec<Message> = packets(&w).map(|p| Message::parse(&p)).collect();
        assert_eq!(got.len(), 4);
        assert!(matches!(
            got[0],
            Message::Midi2 {
                channel: 2,
                voice: Voice2::NoteOn {
                    note: 60,
                    velocity: 0xFFFF,
                    ..
                },
                ..
            }
        ));
        assert!(matches!(
            got[1],
            Message::Midi2 {
                voice: Voice2::PerNotePitchBend { note: 60, .. },
                ..
            }
        ));
        assert!(matches!(got[2], Message::Sysex7 { len: 4, .. }));
        assert!(matches!(got[3], Message::System { status: 0xF8, .. }));
        assert_eq!(
            sequencer_client("MPK mini 3:MPK mini 3 MIDI 1 32:0"),
            Some(32)
        );
        assert_eq!(sequencer_client("Plain"), None);
    }
}
