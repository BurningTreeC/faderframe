//! A control surface's own MIDI connection: its input port is read here
//! (the hub leaves it alone, so it never plays tracks or gets recorded)
//! and its output port written at once, not through the engine's timed
//! queue (LEDs, faders and displays follow the session, not the audio).

use crate::{CLIENT_NAME, Captured, port_identity};
use std::sync::mpsc::{Receiver, Sender, channel};

enum Out {
    Device(midir::MidiOutputConnection),
    Capture(Captured),
}

/// The ports of one surface (see the module docs).
pub struct SurfacePorts {
    rx: Receiver<Vec<u8>>,
    _input: Option<midir::MidiInputConnection<()>>,
    output: Option<Out>,
}

/// The other end of [`SurfacePorts::virtual_pair`] (tests).
pub struct VirtualSurface {
    tx: Sender<Vec<u8>>,
    captured: Captured,
}

impl VirtualSurface {
    /// A message from the surface.
    pub fn send(&self, bytes: &[u8]) {
        let _ = self.tx.send(bytes.to_vec());
    }

    /// What was sent to the surface since the last call.
    pub fn take(&self) -> Vec<Vec<u8>> {
        self.captured
            .lock()
            .map(|mut v| v.drain(..).map(|(_, b)| b).collect())
            .unwrap_or_default()
    }
}

/// Splits running bytes into messages (`midir` hands over whole
/// messages, but a status may carry several data pairs).
fn messages(bytes: &[u8], mut each: impl FnMut(&[u8])) {
    if bytes.first() == Some(&0xF0) {
        each(bytes);
        return;
    }
    let mut i = 0;
    let mut status = 0u8;
    while i < bytes.len() {
        if bytes[i] & 0x80 != 0 {
            status = bytes[i];
            i += 1;
        }
        let len = match status & 0xF0 {
            0xC0 | 0xD0 => 1,
            0x80..=0xE0 => 2,
            _ => {
                each(&bytes[i.saturating_sub(1)..]);
                return;
            }
        };
        let Some(data) = bytes.get(i..i + len) else {
            return;
        };
        let mut msg = [0u8; 3];
        msg[0] = status;
        msg[1..=len].copy_from_slice(data);
        each(&msg[..=len]);
        i += len;
    }
}

impl SurfacePorts {
    /// Connect to the input and output ports with these keys (as
    /// `MidiPort::key` names them; an empty key: none).
    pub fn open(input_key: &str, output_key: &str) -> Result<Self, String> {
        let (tx, rx) = channel();
        let input = if input_key.is_empty() {
            None
        } else {
            let mut input = midir::MidiInput::new(CLIENT_NAME).map_err(|e| e.to_string())?;
            input.ignore(midir::Ignore::All);
            let port = input
                .ports()
                .into_iter()
                .find(|p| {
                    input
                        .port_name(p)
                        .is_ok_and(|full| port_identity(&full).0 == input_key)
                })
                .ok_or_else(|| format!("no MIDI input '{input_key}'"))?;
            let conn = input
                .connect(
                    &port,
                    &format!("{CLIENT_NAME} surface in: {input_key}"),
                    move |_, bytes, _| messages(bytes, |m| drop(tx.send(m.to_vec()))),
                    (),
                )
                .map_err(|e| e.to_string())?;
            Some(conn)
        };
        let output = if output_key.is_empty() {
            None
        } else {
            let out = midir::MidiOutput::new(CLIENT_NAME).map_err(|e| e.to_string())?;
            let port = out
                .ports()
                .into_iter()
                .find(|p| {
                    out.port_name(p)
                        .is_ok_and(|full| port_identity(&full).0 == output_key)
                })
                .ok_or_else(|| format!("no MIDI output '{output_key}'"))?;
            let conn = out
                .connect(&port, &format!("{CLIENT_NAME} surface out: {output_key}"))
                .map_err(|e| e.to_string())?;
            Some(Out::Device(conn))
        };
        Ok(Self {
            rx,
            _input: input,
            output,
        })
    }

    /// Ports joined to a [`VirtualSurface`] (tests).
    pub fn virtual_pair() -> (Self, VirtualSurface) {
        let (tx, rx) = channel();
        let captured = Captured::default();
        (
            Self {
                rx,
                _input: None,
                output: Some(Out::Capture(captured.clone())),
            },
            VirtualSurface { tx, captured },
        )
    }

    /// The next message from the surface.
    pub fn recv(&self) -> Option<Vec<u8>> {
        self.rx.try_recv().ok()
    }

    /// Send a message (or several in running status) to the surface.
    pub fn send(&mut self, bytes: &[u8]) {
        match &mut self.output {
            Some(Out::Device(c)) => {
                if let Err(e) = c.send(bytes) {
                    tracing::debug!("control surface output: {e}");
                }
            }
            Some(Out::Capture(c)) => {
                if let Ok(mut v) = c.lock() {
                    v.push((0, bytes.to_vec()));
                }
            }
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn running_status_is_split() {
        let mut got = Vec::new();
        messages(&[0xB0, 0x0F, 0x04, 0x2F, 0x43], |m| got.push(m.to_vec()));
        messages(&[0xD0, 0x15], |m| got.push(m.to_vec()));
        messages(&[0xF0, 0x00, 0x66, 0xF7], |m| got.push(m.to_vec()));
        assert_eq!(
            got,
            vec![
                vec![0xB0, 0x0F, 0x04],
                vec![0xB0, 0x2F, 0x43],
                vec![0xD0, 0x15],
                vec![0xF0, 0x00, 0x66, 0xF7],
            ]
        );
        let (mut ports, surface) = SurfacePorts::virtual_pair();
        surface.send(&[0x90, 0x5E, 0x7F]);
        assert_eq!(ports.recv(), Some(vec![0x90, 0x5E, 0x7F]));
        ports.send(&[0x90, 0x5E, 0x7F]);
        assert_eq!(surface.take(), vec![vec![0x90, 0x5E, 0x7F]]);
    }
}
