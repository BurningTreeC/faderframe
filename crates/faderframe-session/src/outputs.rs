//! Multi-output plugins (a drum instrument's kit pieces on outputs of
//! their own): tracks take a plugin's extra output buses as their input
//! (`InputRouting::Plugin`), and one action makes them all, routed like the
//! plugin's track, in a folder under it.

use crate::{Result, Session, SessionError};
use faderframe_core::{ChannelLayout, PluginInstanceId, TrackId};
use faderframe_project::{Command, InputRouting, OutputRouting, Track, TrackKind};

/// One of a plugin's output buses and the track taking it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginOutput {
    pub bus: u16,
    pub name: String,
    pub channels: u16,
    /// The track that takes it (the main bus: none; it is the plugin's
    /// own track's signal).
    pub track: Option<TrackId>,
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
                PluginOutput {
                    bus,
                    name: b.name.clone(),
                    channels: b.channels,
                    track: takers
                        .iter()
                        .find(|t| t.input.plugin_output() == Some((plugin, bus)) && bus > 0)
                        .map(|t| t.id),
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

    /// Tracks for the plugin's extra output buses (`buses`: those, else
    /// every one) that no track takes yet: Aux tracks fed by them, routed
    /// where the plugin's track goes, in a folder right under it (the
    /// folder earlier ones are in, if they are). One undo step; returns
    /// the new tracks.
    pub(crate) fn create_output_tracks(
        &mut self,
        plugin: PluginInstanceId,
        buses: Option<&[u16]>,
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
        let wanted: Vec<PluginOutput> = all
            .into_iter()
            .filter(|o| o.bus > 0 && o.track.is_none())
            .filter(|o| buses.is_none_or(|b| b.contains(&o.bus)))
            .collect();
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
                    .map(|s| s.plugin.name.clone())
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
        for o in &wanted {
            let id: TrackId = self.project.ids.allocate();
            let mut t = Track::new(
                id,
                TrackKind::Aux,
                Self::output_track_name(&host, &o.name),
                host.color,
            )
            .with_layout(ChannelLayout::from_channel_count(usize::from(
                o.channels.max(1),
            )));
            t.input = InputRouting::Plugin { plugin, bus: o.bus };
            t.output = host.output;
            t.folder = Some(folder);
            commands.push(Command::AddTrack {
                track: Box::new(t),
                index,
            });
            index += 1;
            made.push(id);
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
