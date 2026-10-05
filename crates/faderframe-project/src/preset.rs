//! Track presets: a track's channel settings — format, input, monitoring,
//! instrument and inserts (with plugin parameters and state), fader, pan,
//! polarity, sends, output and colour — saved to a `.fftrack` file and
//! recalled as a new track or onto an existing one. Clips and automation
//! are not part of a preset.
//!
//! Sends and outputs to other tracks are stored by the target's *name*
//! (ids are project-local) and resolved when the preset is used; targets
//! that do not exist are reported and left out (an output falls back to
//! the master).

use crate::{
    AuxSend, Command, EditError, InputRouting, MonitorMode, OutputRouting, PluginRef, PluginSlot,
    Project, SavedParameter, SendTap, Track, TrackColor, TrackKind,
};
use faderframe_core::{ChannelLayout, TrackId};
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const PRESET_EXTENSION: &str = "fftrack";
const FORMAT: &str = "faderframe-track-preset";
pub const PRESET_VERSION: u32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum PresetError {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("not a FaderFrame track preset: {0}")]
    Format(String),
    #[error("track preset version {0} is newer than this FaderFrame supports")]
    TooNew(u32),
    #[error("a {preset} preset cannot be applied to a {track} track")]
    Kind { preset: String, track: String },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PresetPlugin {
    pub plugin: PluginRef,
    #[serde(default)]
    pub bypass: bool,
    #[serde(default)]
    pub parameters: Vec<SavedParameter>,
    #[serde(default)]
    pub state: Option<String>,
}

impl PresetPlugin {
    fn capture(slot: &PluginSlot) -> Self {
        Self {
            plugin: slot.plugin.clone(),
            bypass: slot.bypass,
            parameters: slot.parameters.clone(),
            state: slot.state.clone(),
        }
    }

    fn slot(&self, p: &mut Project) -> PluginSlot {
        PluginSlot {
            id: p.ids.allocate(),
            plugin: self.plugin.clone(),
            bypass: self.bypass,
            parameters: self.parameters.clone(),
            state: self.state.clone(),
            sidechain: None,
        }
    }
}

/// Where the track's output goes, by name.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum PresetOutput {
    Master,
    Track { name: String },
    Hardware { first_channel: u16 },
    None,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PresetSend {
    /// Name of the bus/aux the send feeds.
    pub target: String,
    pub level_db: f32,
    #[serde(default)]
    pub tap: SendTap,
    #[serde(default = "yes")]
    pub enabled: bool,
}

fn yes() -> bool {
    true
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TrackPreset {
    pub format: String,
    pub version: u32,
    pub name: String,
    pub kind: TrackKind,
    pub layout: ChannelLayout,
    pub color: TrackColor,
    #[serde(default)]
    pub input: InputRouting,
    pub output: PresetOutput,
    pub volume_db: f32,
    pub pan: f32,
    #[serde(default)]
    pub phase_invert: bool,
    #[serde(default)]
    pub monitor: MonitorMode,
    #[serde(default)]
    pub instrument: Option<PresetPlugin>,
    #[serde(default)]
    pub preamp: Option<PresetPlugin>,
    #[serde(default)]
    pub inserts: Vec<PresetPlugin>,
    #[serde(default)]
    pub sends: Vec<PresetSend>,
}

impl TrackPreset {
    /// The settings of `track` in `project`.
    pub fn capture(project: &Project, track: &Track) -> Self {
        let name_of = |id: TrackId| project.track(id).map(|t| t.name.clone());
        let output = match track.output {
            OutputRouting::Master => PresetOutput::Master,
            OutputRouting::Track { track } => {
                name_of(track).map_or(PresetOutput::Master, |name| PresetOutput::Track { name })
            }
            OutputRouting::Hardware { first_channel } => PresetOutput::Hardware { first_channel },
            OutputRouting::None => PresetOutput::None,
        };
        Self {
            format: FORMAT.into(),
            version: PRESET_VERSION,
            name: track.name.clone(),
            kind: track.kind,
            layout: track.layout,
            color: track.color,
            input: track.input.clone(),
            output,
            volume_db: track.volume_db,
            pan: track.pan,
            phase_invert: track.phase_invert,
            monitor: track.monitor,
            instrument: track.instrument.as_ref().map(PresetPlugin::capture),
            preamp: track.preamp.as_ref().map(PresetPlugin::capture),
            inserts: track.inserts.iter().map(PresetPlugin::capture).collect(),
            sends: track
                .sends
                .iter()
                .filter_map(|s| {
                    Some(PresetSend {
                        target: name_of(s.target)?,
                        level_db: s.level_db,
                        tap: s.tap,
                        enabled: s.enabled,
                    })
                })
                .collect(),
        }
    }

    fn find(project: &Project, name: &str, not: Option<TrackId>) -> Option<TrackId> {
        project
            .tracks
            .iter()
            .find(|t| t.name == name && Some(t.id) != not)
            .map(|t| t.id)
    }

    fn resolve_output(
        &self,
        project: &Project,
        own: Option<TrackId>,
        notes: &mut Vec<String>,
    ) -> OutputRouting {
        match &self.output {
            PresetOutput::Master => OutputRouting::Master,
            PresetOutput::Hardware { first_channel } => OutputRouting::Hardware {
                first_channel: *first_channel,
            },
            PresetOutput::None => OutputRouting::None,
            PresetOutput::Track { name } => match Self::find(project, name, own) {
                Some(track) => OutputRouting::Track { track },
                None => {
                    notes.push(format!(
                        "output '{name}' does not exist here; routed to the master"
                    ));
                    OutputRouting::Master
                }
            },
        }
    }

    fn resolve_sends(
        &self,
        p: &mut Project,
        own: Option<TrackId>,
        notes: &mut Vec<String>,
    ) -> Vec<AuxSend> {
        let mut out = Vec::new();
        for s in &self.sends {
            match Self::find(p, &s.target, own) {
                Some(target) => out.push(AuxSend {
                    id: p.ids.allocate(),
                    target,
                    level_db: s.level_db,
                    tap: s.tap,
                    enabled: s.enabled,
                }),
                None => notes.push(format!("send to '{}' left out: no such track", s.target)),
            }
        }
        out
    }

    /// A new track built from the preset (fresh ids). Returns the track and
    /// notes about anything that could not be resolved.
    pub fn instantiate(&self, p: &mut Project, name: Option<&str>) -> (Track, Vec<String>) {
        let mut notes = Vec::new();
        let id: TrackId = p.ids.allocate();
        let mut t = Track::new(id, self.kind, name.unwrap_or(&self.name), self.color);
        t.layout = self.layout;
        t.input = self.input.clone();
        t.volume_db = self.volume_db;
        t.pan = self.pan;
        t.phase_invert = self.phase_invert;
        t.monitor = self.monitor;
        t.instrument = self.instrument.as_ref().map(|i| i.slot(p));
        t.preamp = self.preamp.as_ref().map(|i| i.slot(p));
        t.inserts = self.inserts.iter().map(|i| i.slot(p)).collect();
        t.output = self.resolve_output(p, None, &mut notes);
        t.sends = self.resolve_sends(p, None, &mut notes);
        (t, notes)
    }

    /// Commands that give an existing track the preset's settings (its
    /// name, clips, arm, mute and solo are kept).
    pub fn apply_commands(
        &self,
        p: &mut Project,
        track: TrackId,
    ) -> Result<(Vec<Command>, Vec<String>), PresetError> {
        let t = p
            .track(track)
            .cloned()
            .ok_or_else(|| PresetError::Format(EditError::UnknownTrack(track).to_string()))?;
        let compatible = t.kind == self.kind
            || (t.kind.is_summing()
                && self.kind.is_summing()
                && t.kind != TrackKind::Master
                && self.kind != TrackKind::Master);
        if !compatible {
            return Err(PresetError::Kind {
                preset: self.kind.label().to_lowercase(),
                track: t.kind.label().to_lowercase(),
            });
        }
        let mut notes = Vec::new();
        let mut c = vec![
            Command::SetTrackLayout {
                track,
                layout: self.layout,
            },
            Command::SetTrackColor {
                track,
                color: self.color,
            },
            Command::SetTrackVolume {
                track,
                db: self.volume_db,
            },
            Command::SetTrackPan {
                track,
                pan: self.pan,
            },
            Command::SetTrackPhaseInvert {
                track,
                on: self.phase_invert,
            },
        ];
        if t.kind == TrackKind::Audio {
            c.push(Command::SetTrackInput {
                track,
                input: self.input.clone(),
            });
            c.push(Command::SetTrackMonitor {
                track,
                mode: self.monitor,
            });
        }
        for s in &t.sends {
            c.push(Command::RemoveSend { track, send: s.id });
        }
        c.push(Command::SetPreamp {
            track,
            slot: self.preamp.as_ref().map(|i| i.slot(p)),
        });
        for slot in &t.inserts {
            c.push(Command::RemovePlugin {
                track,
                plugin: slot.id,
            });
        }
        for (index, ins) in self.inserts.iter().enumerate() {
            c.push(Command::InsertPlugin {
                track,
                index,
                slot: ins.slot(p),
            });
        }
        if t.kind == TrackKind::Instrument {
            c.push(Command::SetInstrument {
                track,
                slot: self.instrument.as_ref().map(|i| i.slot(p)),
            });
        }
        if t.kind != TrackKind::Master {
            c.push(Command::SetTrackOutput {
                track,
                output: self.resolve_output(p, Some(track), &mut notes),
            });
        }
        for send in self.resolve_sends(p, Some(track), &mut notes) {
            c.push(Command::AddSend {
                track,
                send,
                index: None,
            });
        }
        Ok((c, notes))
    }
}

pub fn save(path: &Path, preset: &TrackPreset) -> Result<(), PresetError> {
    let text =
        serde_json::to_string_pretty(preset).map_err(|e| PresetError::Format(e.to_string()))?;
    let tmp = path.with_extension("fftrack.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

pub fn load(path: &Path) -> Result<TrackPreset, PresetError> {
    let text = std::fs::read_to_string(path)?;
    let preset: TrackPreset =
        serde_json::from_str(&text).map_err(|e| PresetError::Format(e.to_string()))?;
    if preset.format != FORMAT {
        return Err(PresetError::Format(format!(
            "{} has format '{}'",
            path.display(),
            preset.format
        )));
    }
    if preset.version > PRESET_VERSION {
        return Err(PresetError::TooNew(preset.version));
    }
    Ok(preset)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::History;
    use faderframe_core::builtin;

    fn project() -> (Project, TrackId, TrackId) {
        let mut p = Project::new("P", 48_000);
        let aux: TrackId = p.ids.allocate();
        p.tracks.insert(
            0,
            Track::new(aux, TrackKind::Aux, "Reverb", TrackColor::palette(1)),
        );
        let vox: TrackId = p.ids.allocate();
        let mut t = Track::new(vox, TrackKind::Audio, "Vocal", TrackColor::palette(3))
            .with_layout(ChannelLayout::Stereo);
        t.input = InputRouting::Hardware { first_channel: 2 };
        t.volume_db = -4.5;
        t.pan = 0.25;
        t.monitor = MonitorMode::Auto;
        t.inserts.push(PluginSlot {
            id: p.ids.allocate(),
            plugin: PluginRef::builtin(builtin::ECHO, "Echo"),
            bypass: true,
            parameters: vec![SavedParameter {
                id: faderframe_core::ParameterId(1),
                value: 0.4,
            }],
            state: Some("c3RhdGU=".into()),
            sidechain: None,
        });
        t.sends.push(AuxSend {
            id: p.ids.allocate(),
            target: aux,
            level_db: -12.0,
            tap: SendTap::PreFader,
            enabled: true,
        });
        p.tracks.insert(0, t);
        (p, vox, aux)
    }

    #[test]
    fn capture_save_load_and_instantiate_elsewhere() {
        let (p, vox, _) = project();
        let preset = TrackPreset::capture(&p, p.track(vox).unwrap());
        let path = std::env::temp_dir().join(format!("ff-preset-{}.fftrack", std::process::id()));
        save(&path, &preset).unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(loaded, preset);
        std::fs::remove_file(&path).unwrap();

        // In a project with a "Reverb" aux the send is restored, by name.
        let (mut other, _, other_aux) = project();
        let (t, notes) = loaded.instantiate(&mut other, Some("Lead Vocal"));
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(t.name, "Lead Vocal");
        assert_eq!(t.layout, ChannelLayout::Stereo);
        assert_eq!(t.input, InputRouting::Hardware { first_channel: 2 });
        assert_eq!(t.inserts[0].state.as_deref(), Some("c3RhdGU="));
        assert!(t.inserts[0].bypass);
        assert_eq!(t.sends[0].target, other_aux);
        assert_eq!(t.sends[0].tap, SendTap::PreFader);

        // Without it, the send is dropped and reported.
        let mut bare = Project::new("B", 48_000);
        let (t, notes) = loaded.instantiate(&mut bare, None);
        assert!(t.sends.is_empty());
        assert_eq!(notes.len(), 1);
        let mut h = History::default();
        h.apply(
            &mut bare,
            Command::AddTrack {
                track: Box::new(t),
                index: 0,
            },
        )
        .unwrap();
    }

    #[test]
    fn apply_onto_existing_track_is_one_undo_step() {
        let (mut p, vox, _) = project();
        let preset = TrackPreset::capture(&p, p.track(vox).unwrap());
        let plain: TrackId = p.ids.allocate();
        p.tracks.insert(
            0,
            Track::new(plain, TrackKind::Audio, "Guitar", TrackColor::palette(0)),
        );
        let (commands, notes) = preset.apply_commands(&mut p, plain).unwrap();
        assert!(notes.is_empty());
        let mut h = History::default();
        h.apply(
            &mut p,
            Command::Batch {
                label: "Apply Track Preset".into(),
                commands,
            },
        )
        .unwrap();
        let g = p.track(plain).unwrap();
        assert_eq!(g.name, "Guitar", "name kept");
        assert_eq!(g.volume_db, -4.5);
        assert_eq!(g.layout, ChannelLayout::Stereo);
        assert_eq!(g.inserts.len(), 1);
        assert_eq!(g.sends.len(), 1);
        h.undo(&mut p).unwrap();
        let g = p.track(plain).unwrap();
        assert!(g.inserts.is_empty() && g.sends.is_empty());
        assert_eq!(g.layout, ChannelLayout::Mono);

        let bus: TrackId = p.ids.allocate();
        p.tracks.insert(
            0,
            Track::new(bus, TrackKind::Bus, "Bus", TrackColor::palette(0)),
        );
        assert!(matches!(
            preset.apply_commands(&mut p, bus),
            Err(PresetError::Kind { .. })
        ));
    }
}
