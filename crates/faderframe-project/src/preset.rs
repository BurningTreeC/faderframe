//! Track presets: a track's channel settings — format, input, monitoring,
//! instrument and inserts (with plugin parameters and state, containers
//! with their chains), modulators, fader, pan, polarity, sends, output and
//! colour — saved to a `.fftrack` file and recalled as a new track or onto
//! an existing one. Clips and automation are not part of a preset.
//!
//! Plugins get new ids when a preset is used: a modulator's route to a
//! plugin keeps the plugin's place among the track's devices (preamp,
//! instrument, inserts, each container followed by what its chains hold —
//! the order of `Track::slots`) and finds it there again.
//!
//! Sends and outputs to other tracks are stored by the target's *name*
//! (ids are project-local) and resolved when the preset is used; targets
//! that do not exist are reported and left out (an output falls back to
//! the master).

use crate::container::Chain;
use crate::modulation::{FollowSource, ModRoute, ModSource, ModTarget, Modulator};
use crate::{
    AuxSend, Command, EditError, InputRouting, MonitorMode, OutputRouting, PluginRef, PluginSlot,
    Project, SavedParameter, SendTap, Track, TrackColor, TrackKind,
};
use faderframe_core::{ChannelLayout, PluginInstanceId, TrackId};
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
    /// A container's chains.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chains: Vec<PresetChain>,
}

/// A container's chain in a preset.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PresetChain {
    pub name: String,
    #[serde(default)]
    pub plugins: Vec<PresetPlugin>,
    #[serde(default)]
    pub gain_db: f32,
    #[serde(default)]
    pub pan: f32,
    #[serde(default)]
    pub mute: bool,
    #[serde(default)]
    pub solo: bool,
    #[serde(default)]
    pub key_low: u8,
    #[serde(default = "top_key")]
    pub key_high: u8,
}

fn top_key() -> u8 {
    127
}

/// What a preset's modulator route moves.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum PresetTarget {
    Volume,
    Pan,
    /// The plugin at `index` of the track's devices (`Track::slots`).
    Plugin {
        index: usize,
        parameter: faderframe_core::ParameterId,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PresetModulator {
    pub name: String,
    pub source: ModSource,
    /// A follower's source track, by name (the id in `source` means
    /// nothing elsewhere).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follows: Option<String>,
    #[serde(default)]
    pub routes: Vec<(PresetTarget, f32)>,
    #[serde(default = "yes")]
    pub enabled: bool,
}

/// The plugins built from a preset, with fresh ids.
struct Built {
    preamp: Option<PluginSlot>,
    instrument: Option<PluginSlot>,
    inserts: Vec<PluginSlot>,
    containers: Vec<(PluginInstanceId, Vec<Chain>)>,
    /// Every slot's id, in the order of `Track::slots`.
    order: Vec<PluginInstanceId>,
}

impl PresetPlugin {
    fn capture(track: &Track, slot: &PluginSlot) -> Self {
        let chains = track
            .containers
            .get(&slot.id)
            .filter(|_| slot.plugin.is_container())
            .map(|chains| {
                chains
                    .iter()
                    .map(|c| PresetChain {
                        name: c.name.clone(),
                        plugins: c.inserts.iter().map(|s| Self::capture(track, s)).collect(),
                        gain_db: c.gain_db,
                        pan: c.pan,
                        mute: c.mute,
                        solo: c.solo,
                        key_low: c.key_low,
                        key_high: c.key_high,
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self {
            plugin: slot.plugin.clone(),
            bypass: slot.bypass,
            parameters: slot.parameters.clone(),
            state: slot.state.clone(),
            chains,
        }
    }

    /// The slot with a fresh id (and its chains into `built`).
    fn build(&self, p: &mut Project, built: &mut Built) -> PluginSlot {
        let slot = PluginSlot {
            id: p.ids.allocate(),
            plugin: self.plugin.clone(),
            bypass: self.bypass,
            parameters: self.parameters.clone(),
            state: self.state.clone(),
            sidechain: None,
        };
        built.order.push(slot.id);
        if self.plugin.is_container() && !self.chains.is_empty() {
            let chains = self
                .chains
                .iter()
                .map(|c| Chain {
                    name: c.name.clone(),
                    inserts: c.plugins.iter().map(|x| x.build(p, built)).collect(),
                    gain_db: c.gain_db,
                    pan: c.pan,
                    mute: c.mute,
                    solo: c.solo,
                    key_low: c.key_low,
                    key_high: c.key_high,
                })
                .collect();
            built.containers.push((slot.id, chains));
        }
        slot
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modulators: Vec<PresetModulator>,
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
            instrument: track
                .instrument
                .as_ref()
                .map(|s| PresetPlugin::capture(track, s)),
            preamp: track
                .preamp
                .as_ref()
                .map(|s| PresetPlugin::capture(track, s)),
            inserts: track
                .inserts
                .iter()
                .map(|s| PresetPlugin::capture(track, s))
                .collect(),
            modulators: Self::capture_modulators(project, track),
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

    fn capture_modulators(project: &Project, track: &Track) -> Vec<PresetModulator> {
        let order: Vec<PluginInstanceId> = track.slots().iter().map(|s| s.id).collect();
        track
            .modulators
            .iter()
            .map(|m| PresetModulator {
                name: m.name.clone(),
                source: m.source.clone(),
                follows: match m.source {
                    ModSource::Follower {
                        source: FollowSource::Track { track },
                        ..
                    } => project.track(track).map(|t| t.name.clone()),
                    _ => None,
                },
                routes: m
                    .routes
                    .iter()
                    .filter_map(|r| {
                        let target = match r.target {
                            ModTarget::Volume => PresetTarget::Volume,
                            ModTarget::Pan => PresetTarget::Pan,
                            ModTarget::Plugin { plugin, parameter } => PresetTarget::Plugin {
                                index: order.iter().position(|id| *id == plugin)?,
                                parameter,
                            },
                        };
                        Some((target, r.depth))
                    })
                    .collect(),
                enabled: m.enabled,
            })
            .collect()
    }

    /// The preset's plugins with fresh ids, in `Track::slots` order.
    fn build(&self, p: &mut Project) -> Built {
        let mut built = Built {
            preamp: None,
            instrument: None,
            inserts: Vec::new(),
            containers: Vec::new(),
            order: Vec::new(),
        };
        built.preamp = self.preamp.as_ref().map(|i| i.build(p, &mut built));
        built.instrument = self.instrument.as_ref().map(|i| i.build(p, &mut built));
        let inserts: Vec<PluginSlot> = self
            .inserts
            .iter()
            .map(|i| i.build(p, &mut built))
            .collect();
        built.inserts = inserts;
        built
    }

    /// The preset's modulators for a track whose devices are `order`.
    fn modulators(
        &self,
        p: &mut Project,
        own: Option<TrackId>,
        order: &[PluginInstanceId],
        notes: &mut Vec<String>,
    ) -> Vec<Modulator> {
        let mut out = Vec::new();
        for m in &self.modulators {
            let mut source = m.source.clone();
            if let (ModSource::Follower { source: follow, .. }, Some(name)) =
                (&mut source, &m.follows)
            {
                *follow = match Self::find(p, name, own) {
                    Some(track) => FollowSource::Track { track },
                    None => {
                        notes.push(format!(
                            "‘{}’ follows '{name}', which does not exist here: it follows the track's input",
                            m.name
                        ));
                        FollowSource::Input
                    }
                };
            }
            let mut modulator = Modulator::new(p.ids.allocate(), source);
            modulator.name = m.name.clone();
            modulator.enabled = m.enabled;
            modulator.routes = m
                .routes
                .iter()
                .filter_map(|(target, depth)| {
                    let target = match *target {
                        PresetTarget::Volume => ModTarget::Volume,
                        PresetTarget::Pan => ModTarget::Pan,
                        PresetTarget::Plugin { index, parameter } => ModTarget::Plugin {
                            plugin: *order.get(index)?,
                            parameter,
                        },
                    };
                    Some(ModRoute {
                        target,
                        depth: *depth,
                    })
                })
                .collect();
            out.push(modulator);
        }
        out
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
        let built = self.build(p);
        t.instrument = built.instrument;
        t.preamp = built.preamp;
        t.inserts = built.inserts;
        t.containers = built.containers.into_iter().collect();
        t.modulators = self.modulators(p, Some(id), &built.order, &mut notes);
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
        let built = self.build(p);
        c.push(Command::SetPreamp {
            track,
            slot: built.preamp,
        });
        for slot in &t.inserts {
            c.push(Command::RemovePlugin {
                track,
                plugin: slot.id,
            });
        }
        for (index, slot) in built.inserts.into_iter().enumerate() {
            c.push(Command::InsertPlugin { track, index, slot });
        }
        if t.kind == TrackKind::Instrument {
            c.push(Command::SetInstrument {
                track,
                slot: built.instrument,
            });
        }
        for (container, chains) in built.containers {
            c.push(Command::SetContainer {
                track,
                container,
                chains: Some(chains),
            });
        }
        let modulators = self.modulators(p, Some(track), &built.order, &mut notes);
        if !modulators.is_empty() || !t.modulators.is_empty() {
            c.push(Command::SetModulators { track, modulators });
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

    #[test]
    fn containers_and_modulators_travel_with_a_preset() {
        let (mut p, vox, aux) = project();
        // A container whose chain holds an echo, an LFO on that echo and
        // on the pan, and a follower of the Reverb aux.
        let container = PluginSlot {
            id: p.ids.allocate(),
            plugin: PluginRef::builtin(builtin::CONTAINER, "Container"),
            bypass: false,
            parameters: Vec::new(),
            state: None,
            sidechain: None,
        };
        let echo = PluginSlot {
            id: p.ids.allocate(),
            plugin: PluginRef::builtin(builtin::ECHO, "Echo"),
            bypass: false,
            parameters: Vec::new(),
            state: None,
            sidechain: None,
        };
        let mut wet = Chain::new("Wet");
        wet.gain_db = -6.0;
        wet.key_high = 80;
        wet.inserts.push(echo.clone());
        let param = faderframe_core::ParameterId(1);
        let mut lfo = Modulator::new(p.ids.allocate(), ModSource::defaults()[0].clone());
        lfo.routes = vec![
            ModRoute {
                target: ModTarget::Plugin {
                    plugin: echo.id,
                    parameter: param,
                },
                depth: 0.3,
            },
            ModRoute {
                target: ModTarget::Pan,
                depth: -0.2,
            },
        ];
        let follower = Modulator::new(
            p.ids.allocate(),
            ModSource::Follower {
                source: FollowSource::Track { track: aux },
                attack_ms: 5.0,
                release_ms: 100.0,
                gain_db: 0.0,
            },
        );
        let t = p.track_mut(vox).unwrap();
        t.containers
            .insert(container.id, vec![Chain::new("Dry"), wet.clone()]);
        t.inserts.push(container);
        t.modulators = vec![lfo, follower];
        let preset = TrackPreset::capture(&p, p.track(vox).unwrap());

        // Elsewhere: new ids, the same structure; routes find the echo in
        // the chain, the follower the other project's Reverb.
        let (mut other, _, other_aux) = project();
        let (t, notes) = preset.instantiate(&mut other, None);
        assert!(notes.is_empty(), "{notes:?}");
        let c = t.inserts.iter().find(|s| s.plugin.is_container()).unwrap();
        let chains = &t.containers[&c.id];
        assert_eq!(chains[1].gain_db, -6.0);
        assert_eq!(chains[1].key_high, 80);
        let new_echo = chains[1].inserts[0].id;
        assert_ne!(new_echo, echo.id);
        assert_eq!(
            t.modulators[0].routes[0].target,
            ModTarget::Plugin {
                plugin: new_echo,
                parameter: param
            }
        );
        assert_eq!(t.modulators[0].routes[1].target, ModTarget::Pan);
        assert!(matches!(
            t.modulators[1].source,
            ModSource::Follower {
                source: FollowSource::Track { track },
                ..
            } if track == other_aux
        ));

        // Onto an existing track, in one undoable step.
        let plain = other.tracks.iter().find(|t| t.name == "Vocal").unwrap().id;
        let (commands, _) = preset.apply_commands(&mut other, plain).unwrap();
        let mut h = History::default();
        h.apply(
            &mut other,
            Command::Batch {
                label: "Apply Preset".into(),
                commands,
            },
        )
        .unwrap();
        let t = other.track(plain).unwrap();
        let c = t.inserts.iter().find(|s| s.plugin.is_container()).unwrap();
        let echo_now = t.containers[&c.id][1].inserts[0].id;
        assert_eq!(
            t.modulators[0].routes[0].target,
            ModTarget::Plugin {
                plugin: echo_now,
                parameter: param
            }
        );
        h.undo(&mut other).unwrap();
        assert!(other.track(plain).unwrap().modulators.is_empty());
    }
}
