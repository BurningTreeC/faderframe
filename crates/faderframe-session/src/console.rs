//! Console summing (see `faderframe_project::console`): switching the
//! console is one undo step that sets it and places its bus amplifier in
//! the input stage of every bus, return and the master (a built-in device,
//! the circuit solved like the preamps, its latency compensated); the
//! channels' line amplifiers are the engine's. Switching families keeps the
//! bus amplifiers' drive and output; a bus whose input stage holds a
//! microphone preamp keeps it. Buses added while a console is set get its
//! amplifier as they are added.
//!
//! Each bus's amplifier is its own: it follows the console (has the
//! console's family), or runs through another console's bus amplifier
//! (hybrid summing: the drums through one console, the mix through
//! another), or is in the box. Switching the console moves the buses that
//! follow it and leaves the others; the master's amplifier is always the
//! console's.

use crate::{Action, InputChoice, NoticeLevel, Result, Session};
use faderframe_core::builtin::{CONSOLE_BUSES, console_bus_index};
use faderframe_project::console::{Console, has_bus_stage};
use faderframe_project::{Command, PluginFormat, PluginRef, PluginSlot, Track};

impl Session {
    /// The console the mix runs through.
    pub fn console(&self) -> Option<Console> {
        self.project.console
    }

    /// The mixer's Console menu: off or a family, the channel drive, the
    /// look.
    pub fn console_choices(&self) -> Vec<InputChoice> {
        let now = self.project.console;
        let mut out = vec![InputChoice {
            label: "Off (in the Box)".into(),
            action: Action::SetConsole { family: None },
            checked: now.is_none(),
            group_start: false,
        }];
        for (f, name) in faderframe_project::console::FAMILIES.iter().enumerate() {
            out.push(InputChoice {
                label: (*name).into(),
                action: Action::SetConsole {
                    family: Some(f as u8),
                },
                checked: now.is_some_and(|c| usize::from(c.family) == f),
                group_start: false,
            });
        }
        let Some(c) = now else {
            return out;
        };
        for (i, db) in [-6.0, -3.0, 0.0, 3.0, 6.0, 9.0, 12.0]
            .into_iter()
            .enumerate()
        {
            out.push(InputChoice {
                label: format!("Channel Drive {db:+} dB"),
                action: Action::Edit(Command::SetConsoleDrive { drive_db: db }),
                checked: (c.drive_db - db).abs() < 0.05,
                group_start: i == 0,
            });
        }
        out.push(InputChoice {
            label: "Mixer Takes the Console's Look".into(),
            action: Action::Edit(Command::SetConsoleLook { look: !c.look }),
            checked: c.look,
            group_start: true,
        });
        out
    }

    /// Family `family`'s bus amplifier, with `from`'s settings (the drive
    /// and output of another family's).
    fn console_bus_slot(&mut self, family: u8, from: Option<&PluginSlot>) -> PluginSlot {
        let (id, name, _) = CONSOLE_BUSES[usize::from(family).min(CONSOLE_BUSES.len() - 1)];
        PluginSlot {
            id: self.project.ids.allocate(),
            plugin: PluginRef {
                format: PluginFormat::Builtin,
                id: id.into(),
                name: name.into(),
            },
            bypass: from.is_some_and(|s| s.bypass),
            parameters: from.map(|s| s.parameters.clone()).unwrap_or_default(),
            state: None,
            sidechain: None,
        }
    }

    /// Run the mix through console `family` (None: in the box).
    pub(crate) fn set_console(&mut self, family: Option<u8>) -> Result<()> {
        let old = self.project.console;
        let console = family.map(|f| Console {
            family: f.min(CONSOLE_BUSES.len() as u8 - 1),
            ..old.unwrap_or_else(|| Console::new(f))
        });
        if console == old {
            return Ok(());
        }
        let mut commands = vec![Command::SetConsole { console }];
        let mut kept = Vec::new();
        let buses: Vec<(faderframe_core::TrackId, String, Option<PluginSlot>)> = self
            .project
            .tracks
            .iter()
            .filter(|t| has_bus_stage(t.kind))
            .map(|t| (t.id, t.name.clone(), t.preamp.clone()))
            .collect();
        let master = self.project.master_id();
        for (track, name, current) in buses {
            let ours = current
                .as_ref()
                .and_then(|s| console_bus_index(&s.plugin.id));
            // A bus follows the console when it has the console's amplifier
            // (or, with no console before, nothing); the master always.
            let follows = Some(track) == master
                || match (old, ours) {
                    (Some(o), Some(i)) => i == usize::from(o.family),
                    (None, None) => current.is_none(),
                    _ => false,
                };
            let slot = match (console, &current, ours) {
                (_, Some(_), None) if console.is_some() => {
                    kept.push(name);
                    None
                }
                _ if !follows => None,
                (Some(c), None, _) => Some(Some(self.console_bus_slot(c.family, None))),
                (Some(c), Some(s), Some(i)) if i != usize::from(c.family) => {
                    Some(Some(self.console_bus_slot(c.family, Some(s))))
                }
                (None, Some(_), Some(_)) => Some(None),
                _ => None,
            };
            if let Some(slot) = slot {
                commands.push(Command::SetPreamp { track, slot });
            }
        }
        let label = commands[0].label();
        self.batch(&label, commands)?;
        if !kept.is_empty() {
            self.notify(
                NoticeLevel::Info,
                format!(
                    "{} kept {} microphone preamp: no console bus amplifier there",
                    kept.join(", "),
                    if kept.len() == 1 { "its" } else { "their" }
                ),
            );
        }
        Ok(())
    }

    /// Run bus `track` through family `family`'s bus amplifier (None: in
    /// the box on this bus), whatever the console. Its drive and output
    /// stay.
    pub(crate) fn set_bus_amplifier(
        &mut self,
        track: faderframe_core::TrackId,
        family: Option<u8>,
    ) -> Result<()> {
        let t = self
            .project
            .track(track)
            .ok_or(crate::SessionError::Other("no such track".into()))?;
        if t.kind == faderframe_project::TrackKind::Master {
            return Err(crate::SessionError::Other(
                "the master's bus amplifier is the console's: choose the console".into(),
            ));
        }
        if !has_bus_stage(t.kind) {
            return Err(crate::SessionError::Other(
                "bus amplifiers go on buses and returns".into(),
            ));
        }
        let current = t.preamp.clone();
        let now = current
            .as_ref()
            .and_then(|s| console_bus_index(&s.plugin.id));
        if family.map(usize::from) == now && (family.is_some() || current.is_none()) {
            return Ok(());
        }
        let from = current
            .as_ref()
            .filter(|s| console_bus_index(&s.plugin.id).is_some());
        let slot = family.map(|f| self.console_bus_slot(f, from));
        self.batch("Bus Amplifier", vec![Command::SetPreamp { track, slot }])
    }

    /// Buses, returns (and a master) a command adds while a console is set
    /// come with its bus amplifier.
    pub(crate) fn console_buses_for_new_tracks(&mut self, cmd: Command) -> Command {
        let Some(console) = self.project.console else {
            return cmd;
        };
        self.give_console_buses(cmd, console.family)
    }

    fn give_console_buses(&mut self, cmd: Command, family: u8) -> Command {
        match cmd {
            Command::AddTrack { mut track, index } => {
                if needs_console_bus(&track) {
                    track.preamp = Some(self.console_bus_slot(family, None));
                }
                Command::AddTrack { track, index }
            }
            Command::Batch { label, commands } => Command::Batch {
                label,
                commands: commands
                    .into_iter()
                    .map(|c| self.give_console_buses(c, family))
                    .collect(),
            },
            other => other,
        }
    }
}

fn needs_console_bus(t: &Track) -> bool {
    has_bus_stage(t.kind) && t.preamp.is_none()
}
