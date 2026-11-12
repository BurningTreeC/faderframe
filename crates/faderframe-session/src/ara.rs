//! ARA in the session: opening a clip with an ARA plug-in (Melodyne,
//! SpectraLayers, VocAlign…), the notes it found as a MIDI clip, the
//! transport requests it makes, and its document's archive kept in the
//! project.
//!
//! "Open with ARA" puts the plug-in first in the clip's track inserts and
//! binds it (`Command::SetAraSlot`, one undo step with the insert): from
//! then on it renders every audio clip of the track from its own model —
//! clips moved, trimmed, split or removed follow at once — and its editor
//! (the ordinary plugin window) shows the clip. Clip gain, fades and
//! reversing do not reach an ARA plug-in (it reads the files); bypassing
//! it plays the clips as they are. The engine side is
//! `faderframe_engine::ara`; ARA plug-ins never run in a helper process.

use crate::{Action, GroupMenuEntry, NoticeLevel, Result, Session, SessionError, UiRequest};
use faderframe_core::{ClipId, TrackId};
use faderframe_plugin_host::ara::PlaybackRequest;
use faderframe_project::{
    Clip, ClipContent, Command, MidiClip, MidiNote, MusicalRange, PluginRef, PluginSlot, Track,
    TrackColor, TrackKind,
};
use faderframe_timeline::MusicalTime;

impl Session {
    /// The ARA plug-ins installed.
    pub fn ara_plugins(&self) -> Vec<PluginRef> {
        use faderframe_plugin_host::PluginFormat as F;
        let ara = self.engine.ara_plugins();
        self.available_plugins()
            .into_iter()
            .filter(|p| {
                let format = match p.plugin.format {
                    faderframe_project::PluginFormat::Builtin => F::Builtin,
                    faderframe_project::PluginFormat::Clap => F::Clap,
                    faderframe_project::PluginFormat::Vst3 => F::Vst3,
                    faderframe_project::PluginFormat::AudioUnit => F::AudioUnit,
                    faderframe_project::PluginFormat::Lv2 => F::Lv2,
                };
                ara.iter().any(|(f, id)| *f == format && *id == p.plugin.id)
            })
            .map(|p| p.plugin)
            .collect()
    }

    /// The ARA entries of an audio clip's menu: show it in its track's ARA
    /// plug-in, or open it with one of those installed; the notes the
    /// plug-in found as MIDI.
    pub fn ara_menu(&self, clip: ClipId) -> Vec<GroupMenuEntry> {
        let p = &self.project;
        let Some(c) = p.clip(clip) else {
            return Vec::new();
        };
        if c.as_midi().is_some() || p.is_launcher_clip(clip) {
            return Vec::new();
        }
        let entry = |label: String, action: Option<Action>| GroupMenuEntry {
            label,
            action,
            checked: None,
            separated: false,
        };
        if let Some(slot) = p.ara_slot_of(c.track) {
            let name = slot.plugin.name.clone();
            return vec![
                entry(
                    format!("Edit in {name} (ARA)"),
                    Some(Action::OpenWithAra { clip, plugin: None }),
                ),
                entry(
                    format!("Notes Found by {name} to MIDI"),
                    Some(Action::AraNotesToMidi(clip)),
                ),
            ];
        }
        let plugins = self.ara_plugins();
        if plugins.is_empty() {
            return vec![entry(
                "Open with ARA (no ARA plug-in installed)".into(),
                None,
            )];
        }
        plugins
            .into_iter()
            .map(|plugin| {
                entry(
                    format!("Open with {} (ARA)", plugin.name),
                    Some(Action::OpenWithAra {
                        clip,
                        plugin: Some(plugin),
                    }),
                )
            })
            .collect()
    }

    /// Open a clip with an ARA plug-in (`None`: its track's), and show the
    /// plug-in's editor on it.
    pub(crate) fn open_with_ara(&mut self, clip: ClipId, plugin: Option<PluginRef>) -> Result<()> {
        let c = self
            .project
            .clip(clip)
            .ok_or_else(|| SessionError::Other(format!("no clip {clip}")))?;
        if c.as_midi().is_some() {
            return Err(SessionError::Other(
                "ARA plug-ins edit audio clips, not MIDI".into(),
            ));
        }
        let track = c.track;
        let t = self
            .project
            .track(track)
            .ok_or_else(|| SessionError::Other("no track".into()))?;
        if t.kind != TrackKind::Audio {
            return Err(SessionError::Other(format!(
                "‘{}’ is not an audio track: ARA plug-ins render audio tracks",
                t.name
            )));
        }
        let slot = match (self.project.ara_slot_of(track).cloned(), plugin) {
            (Some(s), None) => s.id,
            (Some(s), Some(p)) if s.plugin.format == p.format && s.plugin.id == p.id => s.id,
            (Some(s), Some(p)) => {
                return Err(SessionError::Other(format!(
                    "‘{}’ is rendered by {} already: remove it (or stop its ARA) before opening the clip with {}",
                    t.name, s.plugin.name, p.name
                )));
            }
            (None, None) => {
                return Err(SessionError::Other(
                    "the clip's track has no ARA plug-in".into(),
                ));
            }
            (None, Some(plugin)) => {
                let slot = PluginSlot {
                    id: self.project.ids.allocate(),
                    plugin,
                    bypass: false,
                    parameters: Vec::new(),
                    state: None,
                    sidechain: None,
                    mono_to_stereo: false,
                    mix_origin: None,
                };
                let id = slot.id;
                let name = slot.plugin.name.clone();
                // First in the chain: nothing before it delays what it
                // renders (the engine lines it up behind latency anyway).
                self.edit(Command::Batch {
                    label: format!("Open with {name}"),
                    commands: vec![
                        Command::InsertPlugin {
                            track,
                            index: 0,
                            slot,
                        },
                        Command::SetAraSlot { slot: id, on: true },
                    ],
                })?;
                if !self.engine.is_ara_renderer(id) {
                    self.notify(
                        NoticeLevel::Warning,
                        format!("{name} did not take ARA: it plays as an ordinary insert"),
                    );
                }
                id
            }
        };
        self.engine.ara_select(slot, &[clip]);
        self.ui_requests.push(UiRequest::PluginEditor {
            track,
            plugin: slot,
            generic: false,
        });
        self.revision += 1;
        Ok(())
    }

    /// The notes the track's ARA plug-in found in a clip, as a MIDI clip
    /// on a new instrument track (the FaderFrame Synth) under it.
    pub(crate) fn ara_notes_to_midi(&mut self, clip: ClipId) -> Result<TrackId> {
        let c = self
            .project
            .clip(clip)
            .cloned()
            .ok_or_else(|| SessionError::Other(format!("no clip {clip}")))?;
        let plugin = self
            .project
            .ara_slot_of(c.track)
            .map(|s| s.plugin.name.clone())
            .ok_or_else(|| {
                SessionError::Other("the clip is not rendered by an ARA plug-in".into())
            })?;
        let found = self.engine.ara_notes(clip, c.track).unwrap_or_default();
        if found.is_empty() {
            return Err(SessionError::Other(format!(
                "{plugin} has no notes for ‘{}’ (yet: it may still be analysing)",
                c.name
            )));
        }
        let p = &mut self.project;
        let tempo = &p.timeline.tempo;
        let min = MusicalTime::from_quarters(1.0 / 32.0);
        let mut notes: Vec<MidiNote> = found
            .iter()
            .filter_map(|n| {
                let key = n.pitch?.clamp(0, 127) as u8;
                let start = tempo.seconds_to_musical(n.start.max(0.0));
                let end = tempo.seconds_to_musical((n.start + n.length).max(0.0));
                Some((key, start, end, n.volume))
            })
            .map(|(key, start, end, volume)| MidiNote {
                fine: Default::default(),
                articulation: None,
                time_locked: false,
                id: p.ids.allocate(),
                start: start - c.start,
                length: (end - start).max(min),
                key,
                velocity: (volume.clamp(0.0, 1.0) * 126.0).round() as u8 + 1,
                channel: 0,
                muted: false,
                release: None,
            })
            .filter(|n| n.start >= MusicalTime::ZERO)
            .collect();
        notes.sort_by_key(|n| (n.start, n.key));
        if notes.is_empty() {
            return Err(SessionError::Other(format!(
                "{plugin} found no pitched notes in ‘{}’",
                c.name
            )));
        }
        let length = c.end(&p.timeline, p.sample_rate) - c.start;
        let track_id: TrackId = p.ids.allocate();
        let color = p.track(c.track).map(|t| t.color);
        let index = p
            .tracks
            .iter()
            .position(|t| t.id == c.track)
            .map_or(p.tracks.len(), |i| i + 1);
        let name = format!("{} ({plugin} Notes)", c.name);
        let track = Track::new(
            track_id,
            TrackKind::Instrument,
            name.clone(),
            color.unwrap_or_else(|| TrackColor::palette(p.tracks.len())),
        );
        let slot = PluginSlot {
            id: p.ids.allocate(),
            plugin: PluginRef::builtin(faderframe_core::builtin::SYNTH, "FaderFrame Synth"),
            bypass: false,
            parameters: Vec::new(),
            state: None,
            sidechain: None,
            mono_to_stereo: false,
            mix_origin: None,
        };
        let clip_id: ClipId = p.ids.allocate();
        let count = notes.len();
        self.batch(
            "ARA Notes to MIDI",
            vec![
                Command::AddTrack {
                    track: Box::new(track),
                    index,
                },
                Command::InsertPlugin {
                    track: track_id,
                    index: 0,
                    slot,
                },
                Command::AddClip {
                    clip: Box::new(Clip {
                        id: clip_id,
                        track: track_id,
                        name: name.clone(),
                        color: None,
                        start: c.start,
                        muted: false,
                        content: ClipContent::Midi(MidiClip {
                            articulations: Vec::new(),
                            length,
                            notes,
                            controllers: Vec::new(),
                            expressions: Vec::new(),
                            sysex: Vec::new(),
                        }),
                    }),
                },
            ],
        )?;
        self.selection
            .select_tracks(&[track_id], crate::SelectMode::Replace);
        self.notify(NoticeLevel::Info, format!("‘{name}’: {count} notes"));
        Ok(track_id)
    }

    /// Store every ARA document's archive in the project (bookkeeping, no
    /// undo step: part of capturing plugin states).
    pub(crate) fn capture_ara_archives(&mut self) {
        for a in self.engine.capture_ara_archives() {
            self.project.ara.set_archive(a);
        }
    }

    /// What ARA plug-ins asked the transport for (their editors' play,
    /// stop, locate and cycle buttons).
    pub(crate) fn tick_ara(&mut self) {
        use crate::TransportAction as T;
        for r in self.engine.take_ara_requests() {
            let tempo = &self.project.timeline.tempo;
            let action = match r {
                PlaybackRequest::Start if !self.transport.playing => T::Play,
                PlaybackRequest::Stop if self.transport.playing => T::Stop,
                PlaybackRequest::Locate(s) => T::Locate(tempo.seconds_to_musical(s.max(0.0))),
                PlaybackRequest::CycleRange { start, duration } => T::SetLoop(Some(MusicalRange {
                    start: tempo.seconds_to_musical(start.max(0.0)),
                    end: tempo.seconds_to_musical((start + duration).max(0.0)),
                })),
                PlaybackRequest::EnableCycle(on) if on != self.project.loop_enabled => {
                    T::ToggleLoop
                }
                _ => continue,
            };
            if let Err(e) = self.dispatch(Action::Transport(action)) {
                self.notify(NoticeLevel::Warning, e.to_string());
            }
        }
    }

    /// Tracks an ARA plug-in renders now (their clips play through it).
    pub fn ara_rendered_tracks(&self) -> Vec<TrackId> {
        let mut v: Vec<TrackId> = self.engine.ara_rendered_tracks().into_iter().collect();
        v.sort();
        v
    }

    /// What ARA plug-ins are analysing (plug-in, file, done share), for a
    /// status line.
    pub fn ara_analysis(&self) -> Vec<(String, String, f32)> {
        self.engine
            .ara_analysis()
            .into_iter()
            .filter(|(_, a)| a.state != faderframe_plugin_host::ara::AnalysisState::Completed)
            .map(|(plugin, a)| {
                let file = a
                    .source
                    .strip_prefix("src:")
                    .and_then(|id| id.parse::<u64>().ok())
                    .and_then(|id| {
                        self.project
                            .sources
                            .get(&faderframe_core::AudioSourceId(id))
                    })
                    .map_or(a.source.clone(), |s| s.name.clone());
                (plugin, file, a.progress)
            })
            .collect()
    }
}
