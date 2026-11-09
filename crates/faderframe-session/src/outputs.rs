//! Multi-output plugins (a drum instrument's kit pieces on outputs of
//! their own, the Guitar Station's amplifier and DI): tracks take a
//! plugin's output buses as their input (`InputRouting::Plugin`), and one
//! action makes a track for every one, the main one too, routed like the
//! plugin's track, in a folder under it; the plugin's own track then only
//! carries the plugin (its output off, so the main is not heard twice).
//! Removing the track that takes the main gives its routing back; removing
//! one that takes an extra bus puts that bus back in the plugin's main
//! where the plugin can (the Drum Sampler's pads), else it goes unheard.
//! A track may take one channel of a bus instead (`InputRouting::Plugin`'s
//! `channel`): "mono output tracks" make one per channel, whatever the
//! plugin's format grouped its channels into — a multi-microphone drum
//! instrument's microphones each on a track of their own, never two of
//! them hard left and right on one (#52).

use crate::{Result, Session, SessionError};
use faderframe_core::{ChannelLayout, PluginInstanceId, TrackId};
use faderframe_project::{Command, InputRouting, OutputRouting, Track, TrackKind};

/// One of a plugin's output buses and the tracks taking it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginOutput {
    pub bus: u16,
    pub name: String,
    pub channels: u16,
    /// The track that takes all of it.
    pub track: Option<TrackId>,
    /// Its channels' names (the plugin's, else after the bus: "Out 3 L").
    pub channel_names: Vec<String>,
    /// The plugin names its channels itself (LV2's ports).
    pub channels_named: bool,
    /// The tracks taking one channel of it, by channel.
    pub channel_tracks: Vec<Option<TrackId>>,
}

impl PluginOutput {
    /// Does a track take it, all of it or a channel?
    pub fn taken(&self) -> bool {
        self.track.is_some() || self.channel_tracks.iter().any(Option::is_some)
    }

    /// Channels a mono track could still take (none while a track takes
    /// the whole bus).
    pub fn free_channels(&self) -> Vec<u16> {
        if self.track.is_some() {
            return Vec::new();
        }
        (0..self.channels)
            .filter(|c| {
                self.channel_tracks
                    .get(usize::from(*c))
                    .is_none_or(Option::is_none)
            })
            .collect()
    }

    /// Is every channel of it taken (by one track or one each)?
    pub fn covered(&self) -> bool {
        self.track.is_some() || (self.channels > 0 && self.free_channels().is_empty())
    }
}

/// What the host calls channel `c` of a bus of `channels`: "L"/"R" of a
/// pair, else its number.
fn channel_suffix(channels: u16, c: u16) -> String {
    match (channels, c) {
        (2, 0) => "L".into(),
        (2, 1) => "R".into(),
        _ => (c + 1).to_string(),
    }
}

/// Is a bus name only a number ("Output 3", "Out 5/6", "Aux 2", "3-4")?
fn generic(name: &str) -> bool {
    let words = name
        .trim()
        .trim_start_matches(|c: char| c.is_alphabetic() || c == ' ')
        .trim();
    let word = name.trim()[..name.trim().len() - words.len()]
        .trim()
        .to_lowercase();
    words
        .chars()
        .all(|c| c.is_ascii_digit() || "/-–+ &".contains(c))
        && [
            "", "out", "output", "outs", "outputs", "aux", "bus", "main", "st", "stereo",
        ]
        .contains(&word.as_str())
}

impl Session {
    /// The top-level plugin slot `plugin` on its track (outputs of plugins
    /// inside containers are not taken).
    fn output_host(&self, plugin: PluginInstanceId) -> Option<&Track> {
        self.project.tracks.iter().find(|t| {
            t.inserts.iter().any(|s| s.id == plugin)
                || t.instrument.as_ref().is_some_and(|s| s.id == plugin)
        })
    }

    /// A plugin's output buses, main first, with the tracks taking them
    /// (empty until it is instantiated).
    pub fn plugin_output_buses(&self, plugin: PluginInstanceId) -> Vec<PluginOutput> {
        let Some(buses) = self.engine.plugin_outputs(plugin) else {
            return Vec::new();
        };
        let takers = self.project.plugin_output_tracks(plugin);
        buses
            .iter()
            .enumerate()
            .map(|(i, b)| {
                let bus = i as u16;
                let on_bus = |t: &&&Track| t.input.plugin_output() == Some((plugin, bus));
                // A one-channel bus taken "by channel 0" is taken whole.
                let whole = |t: &&&Track| {
                    on_bus(t) && (b.channels <= 1 || t.input.plugin_channel().is_none())
                };
                let named = !b.channel_names.is_empty();
                PluginOutput {
                    bus,
                    name: b.name.clone(),
                    channels: b.channels,
                    track: takers.iter().find(whole).map(|t| t.id),
                    channel_names: if named {
                        b.channel_names.clone()
                    } else {
                        (0..b.channels)
                            .map(|c| format!("{} {}", b.name, channel_suffix(b.channels, c)))
                            .collect()
                    },
                    channels_named: named,
                    channel_tracks: if b.channels > 1 {
                        (0..b.channels)
                            .map(|c| {
                                takers
                                    .iter()
                                    .find(|t| on_bus(t) && t.input.plugin_channel() == Some(c))
                                    .map(|t| t.id)
                            })
                            .collect()
                    } else {
                        Vec::new()
                    },
                }
            })
            .collect()
    }

    /// Does the plugin have extra outputs tracks could take?
    pub fn plugin_has_extra_outputs(&self, plugin: PluginInstanceId) -> bool {
        self.output_host(plugin).is_some()
            && self
                .engine
                .plugin_outputs(plugin)
                .is_some_and(|b| b.len() > 1)
    }

    /// The name a track taking `bus` gets: the plugin's own name for it,
    /// after the plugin's track's when that is only a number.
    fn output_track_name(host: &Track, name: &str) -> String {
        if generic(name) {
            format!("{} {}", host.name, name.trim())
        } else {
            name.trim().to_string()
        }
    }

    /// What a track taking a plugin's output takes, as the plugin names
    /// it: the bus ("Kick"), or one channel of it ("Out3", "Main L");
    /// `None` when the input is no plugin output or the plugin is not
    /// there.
    pub fn plugin_output_name(&self, input: &InputRouting) -> Option<String> {
        let (plugin, bus) = input.plugin_output()?;
        let o = self
            .plugin_output_buses(plugin)
            .into_iter()
            .find(|o| o.bus == bus)?;
        Some(match input.plugin_channel() {
            Some(c) if o.channels > 1 => o
                .channel_names
                .get(usize::from(c))
                .cloned()
                .unwrap_or_else(|| format!("{} {}", o.name, c + 1)),
            _ => o.name,
        })
    }

    /// Does any output bus of the plugin have more than one channel (mono
    /// output tracks would differ from the buses' own)?
    pub fn plugin_has_wide_outputs(&self, plugin: PluginInstanceId) -> bool {
        self.engine
            .plugin_outputs(plugin)
            .is_some_and(|b| b.iter().any(|o| o.channels > 1))
    }

    /// How many output tracks [`Action::CreateOutputTracks`] would make:
    /// buses no track takes, or (`mono`) channels no track takes.
    ///
    /// [`Action::CreateOutputTracks`]: crate::Action::CreateOutputTracks
    pub fn missing_output_tracks(&self, plugin: PluginInstanceId, mono: bool) -> usize {
        self.plugin_output_buses(plugin)
            .iter()
            .map(|o| {
                if mono {
                    o.free_channels().len()
                } else {
                    usize::from(!o.taken())
                }
            })
            .sum()
    }

    /// Tracks for the plugin's output buses (`buses`: those, else every
    /// one, the main too) that no track takes yet: Aux tracks fed by them,
    /// routed where the plugin's track went, in a folder right under it
    /// (the folder earlier ones are in, if they are). `mono`: a mono track
    /// per channel no track takes (each microphone of a drum instrument on
    /// its own, whatever the plugin grouped them into). A track for the
    /// main (all of its channels) switches the plugin's track's output
    /// off. One undo step; returns the new tracks.
    pub(crate) fn create_output_tracks(
        &mut self,
        plugin: PluginInstanceId,
        buses: Option<&[u16]>,
        mono: bool,
    ) -> Result<Vec<TrackId>> {
        let host = self
            .output_host(plugin)
            .ok_or_else(|| SessionError::Other("that plugin is not on a track".into()))?
            .clone();
        if host.freeze.is_some() {
            return Err(SessionError::Other(format!(
                "'{}' is frozen: unfreeze it to take its plugin's outputs",
                host.name
            )));
        }
        let all = self.plugin_output_buses(plugin);
        // What each new track takes: a bus (channel `None`) or one channel
        // of it, its name and its width.
        let mut wanted: Vec<(u16, Option<u16>, String, ChannelLayout)> = Vec::new();
        for o in all
            .iter()
            .filter(|o| buses.is_none_or(|b| b.contains(&o.bus)))
        {
            if !mono || o.channels <= 1 {
                if !o.taken() {
                    let width = usize::from(o.channels.max(1));
                    wanted.push((
                        o.bus,
                        None,
                        o.name.clone(),
                        ChannelLayout::from_channel_count(width),
                    ));
                }
                continue;
            }
            for c in o.free_channels() {
                // The plugin's name for it, else the bus's track name and
                // L/R ("Kit Aux 2 L").
                let name = match o.channel_names.get(usize::from(c)) {
                    Some(n) if o.channels_named => Self::output_track_name(&host, n),
                    _ => format!(
                        "{} {}",
                        Self::output_track_name(&host, &o.name),
                        channel_suffix(o.channels, c)
                    ),
                };
                wanted.push((o.bus, Some(c), name, ChannelLayout::Mono));
            }
        }
        if wanted.is_empty() {
            return Err(SessionError::Other(
                if self.plugin_has_extra_outputs(plugin) {
                    "every output of that plugin has its track already"
                } else {
                    "that plugin has no extra outputs"
                }
                .into(),
            ));
        }
        let p = &self.project;
        let host_index = p.track_index(host.id).unwrap_or(p.tracks.len());
        // Earlier output tracks' folder, else a new one under the track.
        let existing = p.plugin_output_tracks(plugin);
        // Where the outputs go: where the plugin's track went (or, once a
        // track takes its main and its own output is off, that track's).
        let route = existing
            .iter()
            .find(|t| t.input.plugin_output() == Some((plugin, 0)))
            .map_or(host.output, |t| t.output);
        let route = if route == OutputRouting::None && host.output == OutputRouting::None {
            OutputRouting::Master
        } else {
            route
        };
        let folder = existing
            .iter()
            .find_map(|t| t.folder)
            .filter(|f| Some(*f) != host.folder);
        let last = existing
            .iter()
            .filter_map(|t| p.track_index(t.id))
            .max()
            .unwrap_or(host_index);
        let mut commands = Vec::new();
        let (folder, mut index) = match folder {
            Some(f) => (f, last + 1),
            None => {
                let id: TrackId = self.project.ids.allocate();
                let slot_name = host
                    .inserts
                    .iter()
                    .chain(host.instrument.iter())
                    .find(|s| s.id == plugin)
                    .map(|s| host.name_of(s))
                    .unwrap_or_default();
                let mut f = Track::new(
                    id,
                    TrackKind::Folder,
                    if slot_name.is_empty() {
                        format!("{} Outputs", host.name)
                    } else {
                        format!("{} · {slot_name}", host.name)
                    },
                    host.color,
                );
                f.folder = host.folder;
                f.input = InputRouting::None;
                f.output = OutputRouting::None;
                commands.push(Command::AddTrack {
                    track: Box::new(f),
                    index: host_index + 1,
                });
                (id, host_index + 2)
            }
        };
        let mut made = Vec::new();
        for (bus, channel, name, layout) in &wanted {
            let id: TrackId = self.project.ids.allocate();
            let name = if channel.is_some() {
                name.clone()
            } else {
                Self::output_track_name(&host, name)
            };
            let mut t = Track::new(id, TrackKind::Aux, name, host.color).with_layout(*layout);
            t.input = InputRouting::Plugin {
                plugin,
                bus: *bus,
                channel: *channel,
            };
            t.output = route;
            t.folder = Some(folder);
            commands.push(Command::AddTrack {
                track: Box::new(t),
                index,
            });
            index += 1;
            made.push(id);
        }
        // The main has a track now (or a track per channel): the plugin's
        // track would play it twice.
        let main_covered = all.first().is_some_and(|main| {
            main.track.is_some()
                || wanted.iter().any(|w| w.0 == 0 && w.1.is_none())
                || (main.channels > 1
                    && main
                        .free_channels()
                        .iter()
                        .all(|c| wanted.iter().any(|w| w.0 == 0 && w.1 == Some(*c))))
        });
        if wanted.iter().any(|w| w.0 == 0) && main_covered && host.output != OutputRouting::None {
            commands.push(Command::SetTrackOutput {
                track: host.id,
                output: OutputRouting::None,
            });
        }
        self.batch("Create Output Tracks", commands)?;
        self.notify(
            crate::NoticeLevel::Info,
            format!(
                "{} output track{} for '{}'",
                made.len(),
                if made.len() == 1 { "" } else { "s" },
                host.name
            ),
        );
        Ok(made)
    }

    /// Remove the tracks taking `plugin`'s output `bus` (all of it, or a
    /// channel each), and the folder they are in when nothing else is (one
    /// undo step): the main gives the plugin's track its routing back, an
    /// extra bus goes back into the main (built-ins that route into buses
    /// fold untaken ones there).
    pub(crate) fn remove_output_track(&mut self, plugin: PluginInstanceId, bus: u16) -> Result<()> {
        let p = &self.project;
        let tracks: Vec<&Track> = p
            .plugin_output_tracks(plugin)
            .into_iter()
            .filter(|t| t.input.plugin_output() == Some((plugin, bus)))
            .collect();
        if tracks.is_empty() {
            return Err(SessionError::Other("no track takes that output".into()));
        }
        let ids: Vec<TrackId> = tracks.iter().map(|t| t.id).collect();
        let names: Vec<String> = tracks.iter().map(|t| format!("'{}'", t.name)).collect();
        let mut commands: Vec<Command> = ids
            .iter()
            .map(|id| Command::RemoveTrack { track: *id })
            .collect();
        let mut folders: Vec<TrackId> = tracks.iter().filter_map(|t| t.folder).collect();
        folders.sort();
        folders.dedup();
        for folder in folders {
            if !p
                .tracks
                .iter()
                .any(|t| !ids.contains(&t.id) && t.folder == Some(folder))
            {
                commands.push(Command::RemoveTrack { track: folder });
            }
        }
        let one = ids.len() == 1;
        self.batch(
            if one {
                "Remove Output Track"
            } else {
                "Remove Output Tracks"
            },
            commands,
        )?;
        self.notify(
            crate::NoticeLevel::Info,
            format!(
                "{} removed: {} output is back in the plugin's mix",
                names.join(", "),
                if one { "its" } else { "their" }
            ),
        );
        Ok(())
    }

    /// Removing the track that takes a plugin's main gives its routing back
    /// to the plugin's track, when that one's output is off (hooked into
    /// `Session::edit`): the plugin is heard again, where the track went.
    pub(crate) fn keep_main_heard(&self, cmd: Command) -> Command {
        fn removed(cmd: &Command, out: &mut Vec<TrackId>) {
            match cmd {
                Command::RemoveTrack { track } => out.push(*track),
                Command::Batch { commands, .. } => {
                    for c in commands {
                        removed(c, out);
                    }
                }
                _ => {}
            }
        }
        let mut gone = Vec::new();
        removed(&cmd, &mut gone);
        let p = &self.project;
        // One routing back per plugin track (its main may go as a track
        // per channel).
        let mut hosts = Vec::new();
        let back: Vec<Command> =
            gone.iter()
                .filter_map(|t| p.track(*t))
                .filter_map(|t| {
                    let (plugin, bus) = t.input.plugin_output()?;
                    if bus != 0 {
                        return None;
                    }
                    // Another track still takes (part of) the main.
                    if p.plugin_output_tracks(plugin).iter().any(|o| {
                        !gone.contains(&o.id) && o.input.plugin_output() == Some((plugin, 0))
                    }) {
                        return None;
                    }
                    let host = p
                        .tracks
                        .iter()
                        .find(|h| h.id != t.id && h.slots().iter().any(|s| s.id == plugin))?;
                    if hosts.contains(&host.id) {
                        return None;
                    }
                    hosts.push(host.id);
                    (host.output == OutputRouting::None && !gone.contains(&host.id)).then_some(
                        Command::SetTrackOutput {
                            track: host.id,
                            output: t.output,
                        },
                    )
                })
                .collect();
        if back.is_empty() {
            return cmd;
        }
        Command::Batch {
            label: cmd.label(),
            commands: std::iter::once(cmd).chain(back).collect(),
        }
    }

    /// Tracks taking outputs of `track`'s plugins.
    pub fn output_tracks_of(&self, track: TrackId) -> Vec<TrackId> {
        let Some(t) = self.project.track(track) else {
            return Vec::new();
        };
        t.slots()
            .iter()
            .flat_map(|s| self.project.plugin_output_tracks(s.id))
            .map(|t| t.id)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::generic;

    #[test]
    fn numbered_bus_names_are_generic() {
        for n in [
            "Output 3", "Out 5/6", "Aux 2", "3-4", "", "Out 1+2", "Stereo 2",
        ] {
            assert!(generic(n), "{n}");
        }
        for n in ["Kick", "Snare Top", "OH", "Room 2", "Toms"] {
            assert!(!generic(n), "{n}");
        }
    }
}
