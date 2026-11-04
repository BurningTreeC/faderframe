//! A project as a DAWproject document: tracks (folders nesting what they
//! hold) with their channels, devices and sends, the arrangement's clips
//! and automation per track, markers (sections as markers saying where
//! they end), tempo and metre. Times are beats (quarter notes) on the
//! arrangement; audio inside its clips is in seconds, so a clip keeps its
//! samples whatever the tempo does — as FaderFrame plays it.

use super::{Media, ids};
use faderframe_automation::{AutomationLane, AutomationTarget, CurveShape, interpolate};
use faderframe_core::gain::db_to_gain;
use faderframe_interchange::dawproject as dp;
use faderframe_interchange::presets;
use faderframe_project::{
    AudioClip, Clip, ClipContent, ExpressionKind, MAX_LEVEL_DB, MidiClip, MidiController,
    OutputRouting, PluginFormat, PluginSlot, Project, SendTap, SourceSpec, TakeFolder, Track,
    TrackKind,
};
use faderframe_timeline::{MusicalTime, TempoCurve};
use std::collections::{BTreeMap, HashMap};

/// What the export needs to know of a device beyond the project.
#[derive(Clone, Debug, Default)]
pub(crate) struct DeviceInfo {
    pub vendor: Option<String>,
    pub version: Option<String>,
    pub instrument: bool,
    pub note_effect: bool,
    /// Its parameters (for names and ranges).
    pub parameters: Vec<faderframe_plugin_host::ParameterInfo>,
}

/// A project as a DAWproject: the documents, the files they name, and
/// what could not be carried.
pub(crate) struct Exported {
    pub project: dp::Project,
    pub metadata: dp::MetaData,
    pub files: Vec<(String, Media)>,
    pub notes: Vec<String>,
}

/// Counts what was left out, by what it was.
#[derive(Default)]
struct Left(BTreeMap<String, usize>);

impl Left {
    fn add(&mut self, what: impl Into<String>, n: usize) {
        if n > 0 {
            *self.0.entry(what.into()).or_default() += n;
        }
    }

    fn notes(self) -> Vec<String> {
        self.0
            .into_iter()
            .map(|(what, n)| {
                if n == 1 {
                    what
                } else {
                    format!("{what} (×{n})")
                }
            })
            .collect()
    }
}

/// A file-system-safe name for media in the container.
fn file_name(name: &str) -> String {
    let n: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.' | '(' | ')') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let n = n.trim().trim_matches('.').to_string();
    if n.is_empty() { "audio".into() } else { n }
}

fn real(id: String, name: &str, unit: dp::Unit, value: f64) -> dp::RealParameter {
    dp::RealParameter::new(dp::Meta::named(id, name), unit, value)
}

/// The fader's top as a gain.
fn max_gain() -> f64 {
    f64::from(db_to_gain(MAX_LEVEL_DB))
}

fn volume_param(id: String, db: f32) -> dp::RealParameter {
    real(id, "Volume", dp::Unit::Linear, f64::from(db_to_gain(db))).range(0.0, max_gain())
}

fn pan_param(id: String, pan: f32) -> dp::RealParameter {
    real(
        id,
        "Pan",
        dp::Unit::Normalized,
        f64::from((pan.clamp(-1.0, 1.0) + 1.0) / 2.0),
    )
    .range(0.0, 1.0)
}

struct Exporter<'a> {
    p: &'a Project,
    rate: f64,
    info: &'a dyn Fn(&PluginSlot) -> DeviceInfo,
    left: Left,
    files: Vec<(String, Media)>,
    /// Source → its path in the container.
    media: HashMap<faderframe_core::AudioSourceId, String>,
    taken_names: std::collections::HashSet<String>,
    next_timeline: u64,
}

impl Exporter<'_> {
    fn timeline_id(&mut self) -> String {
        self.next_timeline += 1;
        format!("timeline{}", self.next_timeline)
    }

    fn timeline_meta(&mut self) -> dp::TimelineMeta {
        dp::TimelineMeta::id(self.timeline_id())
    }

    fn beats(t: MusicalTime) -> f64 {
        t.quarters()
    }

    /// The source's file in the container (written once).
    fn media(&mut self, source: faderframe_core::AudioSourceId) -> Option<dp::MediaFile> {
        let s = self.p.sources.get(&source)?;
        let (channels, frames, rate) = match &s.spec {
            SourceSpec::File {
                channels,
                frames,
                sample_rate,
                ..
            } => (
                i64::from(*channels),
                *frames as f64,
                f64::from(*sample_rate),
            ),
            SourceSpec::Generated { generator } => (
                generator.channels() as i64,
                generator.frames(self.p.sample_rate) as f64,
                self.rate,
            ),
        };
        let path = match self.media.get(&source) {
            Some(p) => p.clone(),
            None => {
                let base = file_name(&s.name);
                let base = base.trim_end_matches(".wav").to_string();
                let mut name = format!("audio/{base}.wav");
                let mut n = 2;
                while !self.taken_names.insert(name.to_lowercase()) {
                    name = format!("audio/{base} ({n}).wav");
                    n += 1;
                }
                let media = match &s.spec {
                    SourceSpec::File { path, .. } => Media::File(path.clone()),
                    SourceSpec::Generated { generator } => Media::Generate(generator.clone()),
                };
                self.files.push((name.clone(), media));
                self.media.insert(source, name.clone());
                name
            }
        };
        let id = self.timeline_id();
        Some(dp::MediaFile {
            common: dp::TimelineMeta {
                meta: dp::Meta::named(id, s.name.clone()),
                time_unit: Some(dp::TimeUnit::Seconds),
                track: None,
            },
            duration: frames / rate.max(1.0),
            file: dp::FileReference::inside(path),
            algorithm: None,
            channels: Some(channels),
            sample_rate: Some(rate as i64),
        })
    }

    /// An audio clip's content: its source (warped when it is), with its
    /// gain as a gain expression beside it. Content time is seconds from
    /// the source's start (a warped clip's: from the clip's start).
    fn audio_content(&mut self, a: &AudioClip) -> Option<(dp::Timeline, f64, f64)> {
        let media = self.media(a.source)?;
        let r = self.rate;
        let len = a.length as f64 / r;
        let (content, play_start) = match &a.warp {
            Some(w) => {
                let mut warps: Vec<dp::Warp> = w
                    .points(a.source_offset, a.length)
                    .iter()
                    .map(|m| dp::Warp {
                        time: m.at as f64 / r,
                        content_time: m.source as f64 / r,
                    })
                    .collect();
                warps.dedup_by(|b, a| (a.time - b.time).abs() < 1e-12);
                let id = self.timeline_id();
                (
                    dp::Timeline::Warps(dp::Warps {
                        common: dp::TimelineMeta {
                            meta: dp::Meta::id(id),
                            time_unit: Some(dp::TimeUnit::Seconds),
                            track: None,
                        },
                        content_time_unit: dp::TimeUnit::Seconds,
                        content: Some(Box::new(dp::Timeline::Audio(media))),
                        warps,
                    }),
                    0.0,
                )
            }
            None => (dp::Timeline::Audio(media), a.source_offset as f64 / r),
        };
        if a.reversed {
            self.left
                .add("reversed audio clips (exported playing forwards)", 1);
        }
        if a.pitch.is_some() {
            self.left
                .add("pitch edits of audio clips (exported as recorded)", 1);
        }
        let content = if a.gain_db.abs() > 1e-4 {
            let gain = self.gain_points(f64::from(db_to_gain(a.gain_db)), play_start);
            let id = self.timeline_meta();
            dp::Timeline::Lanes(dp::Lanes {
                common: id,
                lanes: vec![content, gain],
            })
        } else {
            content
        };
        Some((content, play_start, play_start + len))
    }

    /// A clip gain as a gain expression (one point at the clip's start).
    fn gain_points(&mut self, gain: f64, at: f64) -> dp::Timeline {
        dp::Timeline::Points(dp::Points {
            common: self.timeline_meta(),
            unit: Some(dp::Unit::Linear),
            target: dp::AutomationTarget {
                expression: Some(dp::ExpressionType::Gain),
                ..dp::AutomationTarget::default()
            },
            points: vec![dp::Point::Real {
                time: at,
                value: gain,
                interpolation: None,
            }],
        })
    }

    /// Fades of a clip (seconds).
    fn fades(c: &mut dp::Clip, fades: &faderframe_project::ClipFades, rate: f64) {
        if fades.fade_in > 0 || fades.fade_out > 0 {
            c.fade_time_unit = Some(dp::TimeUnit::Seconds);
            c.fade_in_time = Some(fades.fade_in as f64 / rate);
            c.fade_out_time = Some(fades.fade_out as f64 / rate);
        }
    }

    fn audio_clip(&mut self, clip: &Clip, a: &AudioClip) -> Option<dp::Clip> {
        let (content, play_start, play_stop) = self.audio_content(a)?;
        let end = self
            .p
            .timeline
            .end_of_sample_span(clip.start, a.length, self.rate);
        let mut c = dp::Clip {
            time: Self::beats(clip.start),
            duration: Some(Self::beats(end) - Self::beats(clip.start)),
            content_time_unit: Some(dp::TimeUnit::Seconds),
            play_start: Some(play_start),
            play_stop: Some(play_stop),
            content: Some(Box::new(content)),
            ..dp::Clip::default()
        };
        Self::fades(&mut c, &a.fades, self.rate);
        Some(c)
    }

    /// A take folder as what it plays: its comp's pieces, a clip each
    /// inside the folder's clip.
    fn takes_clip(&mut self, clip: &Clip, f: &TakeFolder) -> Option<dp::Clip> {
        let r = self.rate;
        let mut inner = Vec::new();
        for piece in f.pieces() {
            let take = &f.takes[piece.take];
            let Some(media) = self.media(take.source) else {
                continue;
            };
            let start = (take.source_offset + piece.start) as f64 / r;
            let gain = take.gain_db + f.gain_db;
            let content = if gain.abs() > 1e-4 {
                let points = self.gain_points(f64::from(db_to_gain(gain)), start);
                dp::Timeline::Lanes(dp::Lanes {
                    common: self.timeline_meta(),
                    lanes: vec![dp::Timeline::Audio(media), points],
                })
            } else {
                dp::Timeline::Audio(media)
            };
            let fade = f.crossfade as f64 / r;
            inner.push(dp::Clip {
                meta: dp::Meta {
                    name: Some(take.name.clone()),
                    ..dp::Meta::default()
                },
                time: piece.start as f64 / r,
                duration: Some((piece.end - piece.start) as f64 / r),
                content_time_unit: Some(dp::TimeUnit::Seconds),
                play_start: Some(start),
                fade_time_unit: (fade > 0.0).then_some(dp::TimeUnit::Seconds),
                fade_in_time: (fade > 0.0).then_some(fade),
                fade_out_time: (fade > 0.0).then_some(fade),
                content: Some(Box::new(content)),
                ..dp::Clip::default()
            });
        }
        if f.takes.len() > 1 {
            self.left.add(
                "takes not in the comp (take folders carry what they play)",
                f.takes.len() - 1,
            );
        }
        let end = self
            .p
            .timeline
            .end_of_sample_span(clip.start, f.length, self.rate);
        let len = f.length as f64 / r;
        let mut c = dp::Clip {
            time: Self::beats(clip.start),
            duration: Some(Self::beats(end) - Self::beats(clip.start)),
            content_time_unit: Some(dp::TimeUnit::Seconds),
            play_start: Some(0.0),
            play_stop: Some(len),
            content: Some(Box::new(dp::Timeline::Clips(dp::Clips {
                common: dp::TimelineMeta {
                    meta: dp::Meta::id(self.timeline_id()),
                    time_unit: Some(dp::TimeUnit::Seconds),
                    track: None,
                },
                clips: inner,
            }))),
            ..dp::Clip::default()
        };
        Self::fades(&mut c, &f.fades, r);
        Some(c)
    }

    fn controller_points(&mut self, lane: &faderframe_project::ControllerLane) -> dp::Points {
        use dp::ExpressionType as E;
        let c = lane.controller;
        let mut target = dp::AutomationTarget {
            channel: Some(i64::from(lane.channel)),
            ..dp::AutomationTarget::default()
        };
        target.expression = Some(match c {
            MidiController::Cc { number } => {
                target.controller = Some(i64::from(number));
                E::ChannelController
            }
            MidiController::PitchBend => E::PitchBend,
            MidiController::ChannelPressure => E::ChannelPressure,
            MidiController::Program => E::ProgramChange,
            MidiController::PolyPressure { key } => {
                target.key = Some(i64::from(key));
                E::PolyPressure
            }
        });
        let program = c == MidiController::Program;
        dp::Points {
            common: self.timeline_meta(),
            unit: (!program).then_some(dp::Unit::Normalized),
            target,
            points: lane
                .points
                .iter()
                .map(|p| {
                    let time = Self::beats(p.time);
                    if program {
                        dp::Point::Integer {
                            time,
                            value: i64::from(p.value),
                        }
                    } else {
                        dp::Point::Real {
                            time,
                            value: p.unit(c),
                            interpolation: None,
                        }
                    }
                })
                .collect(),
        }
    }

    /// A note's expression curves (times from the note's start).
    fn note_expression(&mut self, e: &faderframe_project::NoteExpression) -> Option<dp::Timeline> {
        use dp::ExpressionType as E;
        let mut lanes = Vec::new();
        for kind in ExpressionKind::ALL {
            let curve = e.curve(kind);
            if curve.is_empty() {
                continue;
            }
            let (expression, unit, map): (E, dp::Unit, fn(f32) -> f64) = match kind {
                ExpressionKind::Pitch => (E::Transpose, dp::Unit::Semitones, |v| f64::from(v)),
                ExpressionKind::Pressure => (E::Pressure, dp::Unit::Normalized, |v| f64::from(v)),
                ExpressionKind::Timbre => (E::Timbre, dp::Unit::Normalized, |v| f64::from(v)),
                ExpressionKind::Volume => (E::Gain, dp::Unit::Linear, |v| f64::from(db_to_gain(v))),
                ExpressionKind::Pan => (E::Pan, dp::Unit::Normalized, |v| {
                    f64::from((v.clamp(-1.0, 1.0) + 1.0) / 2.0)
                }),
                ExpressionKind::Vibrato | ExpressionKind::Expression => {
                    self.left.add(
                        format!(
                            "note {} expression (no DAWproject expression)",
                            kind.label().to_lowercase()
                        ),
                        1,
                    );
                    continue;
                }
            };
            lanes.push(dp::Timeline::Points(dp::Points {
                common: self.timeline_meta(),
                unit: Some(unit),
                target: dp::AutomationTarget {
                    expression: Some(expression),
                    ..dp::AutomationTarget::default()
                },
                points: curve
                    .iter()
                    .map(|p| dp::Point::Real {
                        time: Self::beats(p.time),
                        value: map(p.value),
                        interpolation: Some(dp::Interpolation::Linear),
                    })
                    .collect(),
            }));
        }
        match lanes.len() {
            0 => None,
            1 => lanes.pop(),
            _ => Some(dp::Timeline::Lanes(dp::Lanes {
                common: self.timeline_meta(),
                lanes,
            })),
        }
    }

    fn midi_content(&mut self, m: &MidiClip) -> dp::Timeline {
        let mut notes = Vec::with_capacity(m.notes.len());
        let up = |v: u8| faderframe_midi::ump::scale_up(u32::from(v), 7, 16) as u16;
        for n in &m.notes {
            if n.muted {
                self.left.add("muted notes", 1);
                continue;
            }
            if n.time_locked {
                self.left
                    .add("notes locked to time (exported with the beats)", 1);
            }
            if n.articulation.is_some() {
                self.left.add("note articulations", 1);
            }
            let vel = if n.has_fine_velocity() {
                f64::from(n.velocity16()) / 65535.0
            } else {
                f64::from(n.velocity) / 127.0
            };
            let rel = n.release.map(|r| match n.release16() {
                Some(v) if v != up(r) => f64::from(v) / 65535.0,
                _ => f64::from(r) / 127.0,
            });
            let content = m
                .expression(n.id)
                .and_then(|e| self.note_expression(e))
                .map(Box::new);
            notes.push(dp::Note {
                time: Self::beats(n.start),
                duration: Self::beats(n.length),
                channel: i64::from(n.channel),
                key: i64::from(n.key),
                vel: Some(vel),
                rel,
                content,
            });
        }
        self.left.add("SysEx messages in clips", m.sysex.len());
        self.left
            .add("articulation directions in clips", m.articulations.len());
        let notes = dp::Timeline::Notes(dp::Notes {
            common: self.timeline_meta(),
            notes,
        });
        if m.controllers.is_empty() {
            return notes;
        }
        let mut lanes = vec![notes];
        for l in &m.controllers {
            if !l.points.is_empty() {
                let p = self.controller_points(l);
                lanes.push(dp::Timeline::Points(p));
            }
        }
        dp::Timeline::Lanes(dp::Lanes {
            common: self.timeline_meta(),
            lanes,
        })
    }

    fn clip(&mut self, clip: &Clip) -> Option<dp::Clip> {
        let mut c = match &clip.content {
            ClipContent::Midi(m) => dp::Clip {
                time: Self::beats(clip.start),
                duration: Some(Self::beats(m.length)),
                play_start: Some(0.0),
                content: Some(Box::new(self.midi_content(m))),
                ..dp::Clip::default()
            },
            ClipContent::Audio(a) => self.audio_clip(clip, a)?,
            ClipContent::Takes(f) => self.takes_clip(clip, f)?,
        };
        c.meta.name = Some(clip.name.clone());
        c.meta.color = clip.color.map(|c| c.to_hex());
        if clip.muted {
            c.enable = Some(false);
        }
        Some(c)
    }

    /// A device: its plug-in or built-in, its state in the plug-in's own
    /// format, the parameters it has values or automation for.
    fn device(&mut self, t: &Track, slot: &PluginSlot, automated: &[u32]) -> Option<dp::Device> {
        let info = (self.info)(slot);
        let role = if t.instrument.as_ref().is_some_and(|i| i.id == slot.id) || info.instrument {
            dp::DeviceRole::Instrument
        } else if info.note_effect {
            dp::DeviceRole::NoteFx
        } else {
            dp::DeviceRole::AudioFx
        };
        let pid = slot.id.raw();
        let name = t.name_of(slot);
        let state = slot
            .state
            .as_deref()
            .and_then(faderframe_engine::decode_state);
        let (kind, device_id, state_file) = match slot.plugin.format {
            PluginFormat::Clap => (
                dp::DeviceKind::Clap,
                slot.plugin.id.clone(),
                state.map(|s| {
                    (
                        format!("plugins/{pid}.clap-preset"),
                        presets::clap_preset(&slot.plugin.id, &s),
                    )
                }),
            ),
            PluginFormat::Vst3 => {
                let class = ids::vst3_canonical(&slot.plugin.id);
                let uuid = presets::uuid_of_class(&class).unwrap_or_else(|| slot.plugin.id.clone());
                let file = state.map(|s| {
                    let (comp, cont) = ids::split_vst3_state(&s);
                    (
                        format!("plugins/{pid}.vstpreset"),
                        presets::vst_preset(&class, comp, cont),
                    )
                });
                (dp::DeviceKind::Vst3, uuid, file)
            }
            PluginFormat::AudioUnit => {
                self.left.add(
                    "Audio Unit states (written in FaderFrame's form, not as .aupreset)",
                    usize::from(state.is_some()),
                );
                (
                    dp::DeviceKind::Au,
                    slot.plugin.id.clone(),
                    state.map(|s| (format!("plugins/{pid}.ffstate"), s)),
                )
            }
            PluginFormat::Lv2 => {
                self.left.add(
                    "LV2 plug-ins (no DAWproject format: written as generic devices)",
                    1,
                );
                (
                    dp::DeviceKind::Device,
                    format!("{}{}", ids::LV2_PREFIX, slot.plugin.id),
                    state.map(|s| (format!("plugins/{pid}.ffstate"), s)),
                )
            }
            PluginFormat::Builtin => (
                dp::DeviceKind::Builtin,
                slot.plugin.id.clone(),
                state.map(|s| (format!("plugins/{pid}.ffstate"), s)),
            ),
        };
        let mut d = dp::Device::new(
            kind,
            dp::Meta::named(ids::device(slot.id), name),
            role,
            slot.plugin.name.clone(),
        );
        d.device_id = Some(device_id);
        d.vendor = if slot.plugin.format == PluginFormat::Builtin {
            Some(ids::VENDOR.into())
        } else {
            info.vendor.clone()
        };
        d.plugin_version = info.version.clone().filter(|v| !v.is_empty());
        d.enabled = Some(dp::BoolParameter::new(
            dp::Meta::named(ids::device_on(slot.id), "On/Off"),
            !slot.bypass,
        ));
        if let Some((path, bytes)) = state_file {
            d.state = Some(dp::FileReference::inside(path.clone()));
            self.files.push((path, Media::Bytes(bytes)));
        }
        // Parameters: what the slot keeps values of, and what is automated.
        let mut ids_wanted: Vec<u32> = slot.parameters.iter().map(|p| p.id.0).collect();
        ids_wanted.extend_from_slice(automated);
        ids_wanted.sort_unstable();
        ids_wanted.dedup();
        for id in ids_wanted {
            let pi = info.parameters.iter().find(|p| p.id.0 == id);
            let value = slot
                .saved(faderframe_core::ParameterId(id))
                .or(pi.map(|p| p.default));
            let mut rp = dp::RealParameter {
                meta: dp::Meta::named(
                    ids::device_param(slot.id, id),
                    pi.map_or_else(|| format!("Parameter {id}"), |p| p.name.clone()),
                ),
                parameter_id: Some(id as i32),
                unit: dp::Unit::Linear,
                min: pi.map(|p| p.min),
                max: pi.map(|p| p.max),
                value,
            };
            if let Some(p) = pi
                && p.unit == faderframe_plugin_host::ParameterUnit::Decibels
            {
                rp.unit = dp::Unit::Decibel;
            }
            d.parameters.push(dp::Parameter::Real(rp));
        }
        if slot.sidechain.is_some() {
            self.left.add("sidechain inputs", 1);
        }
        Some(d)
    }

    fn channel(&mut self, t: &Track) -> dp::Channel {
        let n = t.id;
        let mut c = dp::Channel::new(dp::Meta::id(ids::channel(n)));
        c.role = Some(match t.kind {
            TrackKind::Master => dp::MixerRole::Master,
            TrackKind::Aux => dp::MixerRole::Effect,
            TrackKind::Bus => dp::MixerRole::Submix,
            TrackKind::Vca => dp::MixerRole::Vca,
            _ => dp::MixerRole::Regular,
        });
        if t.kind.has_audio() {
            c.audio_channels = Some(t.layout.channel_count() as i64);
        }
        c.solo = Some(t.solo);
        c.destination = match t.output {
            OutputRouting::Hardware { .. } | OutputRouting::None => None,
            _ => self
                .p
                .output_target(t)
                .filter(|d| {
                    self.p
                        .track(*d)
                        .is_some_and(|d| d.kind != TrackKind::Folder)
                })
                .map(ids::channel),
        };
        if t.output == OutputRouting::None {
            self.left
                .add("disconnected outputs (they play to the master)", 1);
        }
        c.mute = Some(dp::BoolParameter::new(
            dp::Meta::named(ids::mute(n), "Mute"),
            t.mute,
        ));
        if t.kind != TrackKind::Midi {
            c.volume = Some(volume_param(ids::volume(n), t.volume_db));
            c.pan = Some(pan_param(ids::pan(n), t.pan));
        }
        // Devices in the order they play.
        let automated = |slot: faderframe_core::PluginInstanceId| -> Vec<u32> {
            t.automation
                .lanes
                .iter()
                .filter_map(|l| match l.target {
                    AutomationTarget::PluginParameter { plugin, parameter } if plugin == slot => {
                        Some(parameter.0)
                    }
                    _ => None,
                })
                .collect()
        };
        let slots: Vec<&PluginSlot> = t
            .instrument
            .iter()
            .chain(t.preamp.iter())
            .chain(t.inserts.iter())
            .collect();
        for slot in slots {
            if slot.plugin.is_container() {
                let inside: usize = t
                    .containers
                    .get(&slot.id)
                    .map_or(0, |chains| chains.iter().map(|c| c.inserts.len()).sum());
                self.left.add(
                    format!(
                        "containers of parallel chains (no DAWproject form; {inside} device(s) inside)"
                    ),
                    1,
                );
                continue;
            }
            if let Some(d) = self.device(t, slot, &automated(slot.id)) {
                c.devices.push(d);
            }
        }
        for s in &t.sends {
            if s.tap == SendTap::PreFx {
                self.left.add("pre-effects sends (written as pre-fader)", 1);
            }
            c.sends.push(dp::Send {
                meta: dp::Meta::id(ids::send(s.id)),
                destination: Some(ids::channel(s.target)),
                kind: Some(if s.tap == SendTap::PostFader {
                    dp::SendType::Post
                } else {
                    dp::SendType::Pre
                }),
                enable: Some(dp::BoolParameter::new(
                    dp::Meta::named(ids::send_on(s.id), "On"),
                    s.enabled,
                )),
                pan: s.pan.map(|p| pan_param(ids::send_pan(s.id), p)),
                volume: volume_param(ids::send_volume(s.id), s.level_db),
            });
        }
        self.left.add("cue mix sends", t.cue_sends.len());
        if t.phase_invert {
            self.left.add("polarity inversions", 1);
        }
        if t.delay_ms != 0.0 {
            self.left.add("track delays", 1);
        }
        if t.vca.is_some() {
            self.left.add("VCA assignments", 1);
        }
        if !t.modulators.is_empty() {
            self.left.add("modulators", t.modulators.len());
        }
        c
    }

    /// The points of one automation lane in the target's unit (linear
    /// segments only: curves and dB faders are followed by more points).
    fn lane_points(
        &mut self,
        lane: &AutomationLane,
        map: fn(f64) -> f64,
        nonlinear: bool,
    ) -> Vec<dp::Point> {
        let pts = lane.curve.points();
        let mut out = Vec::with_capacity(pts.len());
        for (i, a) in pts.iter().enumerate() {
            let next = pts.get(i + 1);
            let step = a.shape == CurveShape::Step;
            out.push(dp::Point::Real {
                time: Self::beats(a.time),
                value: map(a.value),
                interpolation: Some(if step {
                    dp::Interpolation::Hold
                } else {
                    dp::Interpolation::Linear
                }),
            });
            // Between this point and the next: enough points for the
            // shape (or a fader's dB line) to stay within reach of a line.
            let Some(b) = next else { continue };
            if step || b.time <= a.time {
                continue;
            }
            let pieces = match a.shape {
                CurveShape::Linear if nonlinear => {
                    ((a.value - b.value).abs() / 1.5).ceil() as usize
                }
                CurveShape::Linear => 1,
                _ => 16,
            }
            .clamp(1, 64);
            for k in 1..pieces {
                let f = k as f64 / pieces as f64;
                let t = a.time.ticks() as f64 + (b.time.ticks() - a.time.ticks()) as f64 * f;
                out.push(dp::Point::Real {
                    time: t / faderframe_timeline::TICKS_PER_QUARTER as f64,
                    value: map(interpolate(a.value, b.value, f, a.shape)),
                    interpolation: Some(dp::Interpolation::Linear),
                });
            }
        }
        out
    }

    /// The track's automation lanes, as points targeting its parameters.
    fn automation(&mut self, t: &Track) -> Vec<dp::Timeline> {
        let mut out = Vec::new();
        for lane in &t.automation.lanes {
            if lane.curve.is_empty() {
                continue;
            }
            let (param, unit, points) = match lane.target {
                AutomationTarget::TrackVolume => (
                    ids::volume(t.id),
                    dp::Unit::Linear,
                    self.lane_points(lane, |db| f64::from(db_to_gain(db as f32)), true),
                ),
                AutomationTarget::TrackPan => (
                    ids::pan(t.id),
                    dp::Unit::Normalized,
                    self.lane_points(lane, |p| (p.clamp(-1.0, 1.0) + 1.0) / 2.0, false),
                ),
                AutomationTarget::TrackMute => {
                    (ids::mute(t.id), dp::Unit::Linear, bool_points(lane, false))
                }
                AutomationTarget::SendLevel(s) if t.send(s).is_some() => (
                    ids::send_volume(s),
                    dp::Unit::Linear,
                    self.lane_points(lane, |db| f64::from(db_to_gain(db as f32)), true),
                ),
                AutomationTarget::PluginParameter { plugin, parameter }
                    if t.plugin(plugin).is_some() =>
                {
                    (
                        ids::device_param(plugin, parameter.0),
                        dp::Unit::Linear,
                        self.lane_points(lane, |v| v, false),
                    )
                }
                AutomationTarget::PluginBypass(plugin) if t.plugin(plugin).is_some() => (
                    ids::device_on(plugin),
                    dp::Unit::Linear,
                    bool_points(lane, true),
                ),
                other => {
                    self.left.add(
                        format!("automation of {} (no DAWproject parameter)", other.label()),
                        1,
                    );
                    continue;
                }
            };
            let bool_target = points.iter().all(|p| matches!(p, dp::Point::Bool { .. }));
            out.push(dp::Timeline::Points(dp::Points {
                common: self.timeline_meta(),
                unit: (!bool_target).then_some(unit),
                target: dp::AutomationTarget {
                    parameter: Some(param),
                    ..dp::AutomationTarget::default()
                },
                points,
            }));
        }
        out
    }

    fn track(&mut self, t: &Track, children: Vec<dp::Track>) -> dp::Track {
        let content_types = match t.kind {
            TrackKind::Audio => vec![dp::ContentType::Audio],
            TrackKind::Instrument | TrackKind::Midi => vec![dp::ContentType::Notes],
            TrackKind::Master => vec![dp::ContentType::Audio, dp::ContentType::Notes],
            TrackKind::Vca => vec![dp::ContentType::Automation],
            TrackKind::Folder => vec![dp::ContentType::Tracks],
            TrackKind::Bus if t.is_folder_bus() => vec![dp::ContentType::Tracks],
            TrackKind::Bus | TrackKind::Aux => vec![dp::ContentType::Audio],
        };
        dp::Track {
            meta: dp::Meta {
                id: Some(ids::track(t.id)),
                name: Some(t.name.clone()),
                color: Some(t.color.to_hex()),
                comment: None,
            },
            content_types,
            loaded: Some(true),
            channel: (t.kind != TrackKind::Folder).then(|| self.channel(t)),
            tracks: children,
        }
    }
}

/// Points of a switch lane (`invert`: the lane is a bypass and the
/// parameter an "on").
fn bool_points(lane: &AutomationLane, invert: bool) -> Vec<dp::Point> {
    lane.curve
        .points()
        .iter()
        .map(|p| dp::Point::Bool {
            time: p.time.quarters(),
            value: (p.value >= 0.5) != invert,
        })
        .collect()
}

/// A section as a marker: its name, colour and where it ends.
pub(crate) fn section_comment(end: MusicalTime) -> String {
    format!("{}{}", ids::SECTION_COMMENT, end.quarters())
}

/// `project` as a DAWproject (`info` tells about its devices).
pub(crate) fn export(project: &Project, info: &dyn Fn(&PluginSlot) -> DeviceInfo) -> Exported {
    let mut x = Exporter {
        p: project,
        rate: f64::from(project.sample_rate),
        info,
        left: Left::default(),
        files: Vec::new(),
        media: HashMap::new(),
        taken_names: Default::default(),
        next_timeline: 0,
    };
    // The structure: each folder holding what is in it.
    let order = project.folder_order();
    let parent = |t: &Track| {
        t.folder
            .filter(|f| project.track(*f).is_some_and(Track::is_folder))
    };
    fn build(
        x: &mut Exporter,
        order: &[&Track],
        parent: &dyn Fn(&Track) -> Option<faderframe_core::TrackId>,
        under: Option<faderframe_core::TrackId>,
    ) -> Vec<dp::Track> {
        let mut out = Vec::new();
        for t in order.iter().filter(|t| parent(t) == under) {
            let children = if t.is_folder() {
                build(x, order, parent, Some(t.id))
            } else {
                Vec::new()
            };
            out.push(x.track(t, children));
        }
        out
    }
    let structure = build(&mut x, &order, &parent, None)
        .into_iter()
        .map(dp::Lane::Track)
        .collect();
    // The arrangement: a lane per track with its clips and automation.
    let mut lanes = Vec::new();
    for t in &order {
        let mut clips = Vec::new();
        for c in project.clips_of(t.id) {
            if let Some(clip) = x.clip(c) {
                clips.push(clip);
            }
        }
        let mut track_lanes = Vec::new();
        if !clips.is_empty() {
            track_lanes.push(dp::Timeline::Clips(dp::Clips {
                common: x.timeline_meta(),
                clips,
            }));
        }
        track_lanes.extend(x.automation(t));
        if t.freeze.is_some() {
            x.left
                .add("frozen tracks (exported with their clips and devices)", 1);
        }
        if !track_lanes.is_empty() {
            lanes.push(dp::Timeline::Lanes(dp::Lanes {
                common: dp::TimelineMeta {
                    meta: dp::Meta::id(x.timeline_id()),
                    time_unit: None,
                    track: Some(ids::track(t.id)),
                },
                lanes: track_lanes,
            }));
        }
    }
    // Markers, and sections as markers saying where they end.
    let mut markers: Vec<dp::Marker> = project
        .markers
        .iter()
        .map(|m| dp::Marker {
            meta: dp::Meta {
                name: Some(m.name.clone()),
                ..dp::Meta::default()
            },
            time: m.position.quarters(),
        })
        .collect();
    markers.extend(project.sections.iter().map(|s| dp::Marker {
        meta: dp::Meta {
            name: Some(s.name.clone()),
            color: Some(s.color.to_hex()),
            comment: Some(section_comment(s.end)),
            id: None,
        },
        time: s.start.quarters(),
    }));
    markers.sort_by(|a, b| a.time.total_cmp(&b.time));
    // Tempo and metre.
    let tempo = &project.timeline.tempo;
    let first = tempo.points().first().map_or(120.0, |p| p.bpm);
    let tempo_automation = (tempo.points().len() > 1
        || tempo.points().iter().any(|p| p.curve == TempoCurve::Linear))
    .then(|| dp::Points {
        common: x.timeline_meta(),
        unit: Some(dp::Unit::Bpm),
        target: dp::AutomationTarget {
            parameter: Some(ids::TEMPO.into()),
            ..dp::AutomationTarget::default()
        },
        points: tempo
            .points()
            .iter()
            .map(|p| dp::Point::Real {
                time: p.position.quarters(),
                value: p.bpm,
                interpolation: Some(if p.curve == TempoCurve::Linear {
                    dp::Interpolation::Linear
                } else {
                    dp::Interpolation::Hold
                }),
            })
            .collect(),
    });
    let meter = &project.timeline.meter;
    let sig = meter.signature_at(MusicalTime::ZERO);
    let time_signature_automation = (meter.changes().len() > 1).then(|| dp::Points {
        common: x.timeline_meta(),
        unit: None,
        target: dp::AutomationTarget {
            parameter: Some(ids::TIME_SIGNATURE.into()),
            ..dp::AutomationTarget::default()
        },
        points: meter
            .changes()
            .iter()
            .map(|c| dp::Point::TimeSignature {
                time: meter.bar_start(c.bar).quarters(),
                numerator: i64::from(c.signature.numerator),
                denominator: i64::from(c.signature.denominator),
            })
            .collect(),
    });
    // What has no place in DAWproject.
    x.left.add("key changes", project.keys.len());
    x.left
        .add("chords of the chord track", project.chords.len());
    x.left.add("lines of lyrics", project.lyrics.len());
    x.left
        .add("clip launcher scenes", project.launcher.scenes.len());
    x.left.add("cue mixes", project.cue_mixes.len());
    x.left.add("groups", project.groups.len());
    x.left.add("mix scenes", project.mix_scenes.scenes.len());
    if project.loop_range.is_some() {
        x.left.add("the loop range", 1);
    }
    if !project.video.is_empty() {
        x.left.add("video", 1);
    }
    let lanes = dp::Lanes {
        common: dp::TimelineMeta {
            meta: dp::Meta::id(x.timeline_id()),
            time_unit: Some(dp::TimeUnit::Beats),
            track: None,
        },
        lanes,
    };
    let doc = dp::Project {
        version: dp::VERSION.into(),
        application: dp::Application {
            name: "FaderFrame".into(),
            version: env!("CARGO_PKG_VERSION").into(),
        },
        transport: Some(dp::Transport {
            tempo: Some(
                real(ids::TEMPO.into(), "Tempo", dp::Unit::Bpm, first).range(
                    faderframe_timeline::TempoMap::MIN_BPM,
                    faderframe_timeline::TempoMap::MAX_BPM,
                ),
            ),
            time_signature: Some(dp::TimeSignatureParameter {
                meta: dp::Meta::named(ids::TIME_SIGNATURE, "Time Signature"),
                parameter_id: None,
                numerator: i64::from(sig.numerator),
                denominator: i64::from(sig.denominator),
            }),
        }),
        structure,
        arrangement: Some(dp::Arrangement {
            meta: dp::Meta::id("arrangement"),
            lanes: Some(lanes),
            markers: (!markers.is_empty()).then(|| dp::Markers {
                common: dp::TimelineMeta::id("markers"),
                markers,
            }),
            tempo_automation,
            time_signature_automation,
        }),
        scenes: Vec::new(),
    };
    Exported {
        project: doc,
        metadata: dp::MetaData {
            title: Some(project.name.clone()),
            comment: Some("Exported by FaderFrame".into()),
            ..dp::MetaData::default()
        },
        files: x.files,
        notes: x.left.notes(),
    }
}
