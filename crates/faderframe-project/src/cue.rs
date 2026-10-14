//! Cue mixes: the performers' headphone mixes, each on its own device
//! outputs, apart from the main mix (as a console's foldback sends or a
//! DAW's studio/cue sends).
//!
//! A cue mix ([`CueMix`]) has a name, a colour (what the mixer marks it
//! with), its outputs (a stereo pair or one, [`CueMix::first_channel`]), a
//! master level and mute, where its sends tap the channels (pre-fader by
//! default, post-fader on request — never before the inserts) and the
//! click it plays (on/off and level). Every audio, instrument, bus and
//! aux track feeds every cue mix through its own [`CueSend`] (level, pan,
//! on), kept in [`crate::Track::cue_sends`] so it comes and goes with the
//! track (removing a track and undoing it, duplicating it).
//!
//! A cue send is solo-safe: it taps the channel before its mute and the
//! solo, so what the engineer mutes or solos in the main mix never
//! changes what a performer hears — and pre-fader it ignores the main
//! fader too. A bus or a return carries what reaches it in the main mix
//! (its members after their faders, mutes and solos): that is what a bus
//! is, so a new cue mix leaves buses and returns off and takes every
//! source on its own (at the level it reaches the master with), and a cue
//! is solo-safe as long as its buses stay off.
//!
//! Cue mixes are monitoring: renders, exports, stems, bounces, freezes and
//! the album never contain them (their engines build none), and they never
//! pass the master's listening (headphone rendering, the mono check).

use crate::{OutputRouting, Project, SendTap, Track, TrackColor, TrackKind};
use faderframe_core::gain::SILENCE_DB;
use faderframe_core::{ChannelLayout, CueId, TrackId};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashSet};

/// Most cue mixes a project has (the engine's click destinations).
pub const MAX_CUE_MIXES: usize = 8;

/// Colours new cue mixes take, in turn: loud enough that a mixer showing a
/// cue mix is never mistaken for the main mix.
pub const CUE_COLORS: [TrackColor; 6] = [
    TrackColor::rgb(0x2f, 0xb5, 0xa8), // teal
    TrackColor::rgb(0xd9, 0x8e, 0x2b), // orange
    TrackColor::rgb(0x9a, 0x6a, 0xe0), // violet
    TrackColor::rgb(0x5f, 0xa8, 0x3c), // green
    TrackColor::rgb(0xd5, 0x5a, 0x8a), // pink
    TrackColor::rgb(0x3f, 0x8a, 0xd8), // blue
];

/// A performer's headphone mix on its own outputs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CueMix {
    pub id: CueId,
    pub name: String,
    /// What the mixer marks the cue mix (and its controls) with.
    pub color: TrackColor,
    /// Stereo or mono.
    pub layout: ChannelLayout,
    /// The first device output it plays on (0: output 1).
    pub first_channel: u16,
    /// The master level of the whole cue mix (the click too).
    #[serde(default)]
    pub level_db: f32,
    #[serde(default)]
    pub mute: bool,
    /// Where the channels' cue sends tap: before the fader (default) or
    /// after it (the cue follows the main faders; never before the
    /// inserts). Mutes and solos never reach either.
    #[serde(default = "pre_fader")]
    pub tap: SendTap,
    /// The metronome plays in this cue mix (when it clicks at all; a
    /// count-in reaches every cue mix).
    #[serde(default = "yes")]
    pub click: bool,
    /// The click's level here, relative to the metronome's volume.
    #[serde(default)]
    pub click_db: f32,
}

fn pre_fader() -> SendTap {
    SendTap::PreFader
}

fn yes() -> bool {
    true
}

impl CueMix {
    pub fn new(
        id: CueId,
        name: impl Into<String>,
        color: TrackColor,
        first_channel: u16,
        layout: ChannelLayout,
    ) -> Self {
        Self {
            id,
            name: name.into(),
            color,
            layout: cue_layout(layout),
            first_channel,
            level_db: 0.0,
            mute: false,
            tap: SendTap::PreFader,
            click: true,
            click_db: 0.0,
        }
    }

    /// The id the engine keys the cue mix's own channel by (its master
    /// strip, meters and nodes): ids are unique across kinds, so it is
    /// never a track's.
    pub fn channel(&self) -> TrackId {
        TrackId(self.id.raw())
    }

    /// Its sends tap after the fader.
    pub fn post_fader(&self) -> bool {
        self.tap == SendTap::PostFader
    }

    /// The device outputs it plays on.
    pub fn outputs(&self) -> std::ops::Range<u16> {
        let n = self.layout.channel_count().max(1) as u16;
        self.first_channel..self.first_channel.saturating_add(n)
    }

    /// "Out 3-4" (or "Out 3" for a mono cue mix), counting from 1.
    pub fn output_label(&self) -> String {
        output_label(self.first_channel, self.layout)
    }
}

/// "Out 3-4" for a stereo pair from `first` (0-based), "Out 3" mono.
pub fn output_label(first: u16, layout: ChannelLayout) -> String {
    let n = layout.channel_count().max(1) as u32;
    let a = u32::from(first) + 1;
    if n == 1 {
        format!("Out {a}")
    } else {
        format!("Out {a}-{}", a + n - 1)
    }
}

/// Cue mixes are mono or stereo.
pub fn cue_layout(layout: ChannelLayout) -> ChannelLayout {
    if layout.channel_count() == 1 {
        ChannelLayout::Mono
    } else {
        ChannelLayout::Stereo
    }
}

/// A track's feed of one cue mix.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct CueSend {
    pub cue: CueId,
    pub level_db: f32,
    /// −1…1: a mono source by the project's pan law, a stereo one's
    /// balance.
    #[serde(default)]
    pub pan: f32,
    /// Heard in the cue mix (off: kept, silent).
    #[serde(default = "yes")]
    pub on: bool,
}

impl CueSend {
    /// What a send created by a level, pan or switch edit starts from.
    pub fn new(cue: CueId) -> Self {
        Self {
            cue,
            level_db: 0.0,
            pan: 0.0,
            on: true,
        }
    }

    /// Clamped into range (NaN: unity, centre).
    pub fn clamped(self) -> Self {
        Self {
            level_db: crate::edit::clamp_level(self.level_db),
            pan: if self.pan.is_nan() {
                0.0
            } else {
                self.pan.clamp(-1.0, 1.0)
            },
            ..self
        }
    }
}

/// Tracks of this kind feed the cue mixes (audio, instrument, bus, aux).
pub fn feeds_cues(kind: TrackKind) -> bool {
    matches!(
        kind,
        TrackKind::Audio | TrackKind::Instrument | TrackKind::Bus | TrackKind::Aux
    )
}

impl Track {
    /// Its send to cue mix `cue`.
    pub fn cue_send(&self, cue: CueId) -> Option<&CueSend> {
        self.cue_sends.iter().find(|s| s.cue == cue)
    }

    pub fn cue_send_mut(&mut self, cue: CueId) -> Option<&mut CueSend> {
        self.cue_sends.iter_mut().find(|s| s.cue == cue)
    }
}

impl Project {
    pub fn cue(&self, id: CueId) -> Option<&CueMix> {
        self.cue_mixes.iter().find(|c| c.id == id)
    }

    pub fn cue_mut(&mut self, id: CueId) -> Option<&mut CueMix> {
        self.cue_mixes.iter_mut().find(|c| c.id == id)
    }

    pub fn cue_index(&self, id: CueId) -> Option<usize> {
        self.cue_mixes.iter().position(|c| c.id == id)
    }

    /// The buses (and returns) between `t` and the master, in order, when
    /// its output reaches the master (`None`: it does not — no output, a
    /// hardware output, a loop).
    pub fn path_to_master<'a>(&'a self, t: &'a Track) -> Option<Vec<&'a Track>> {
        let master = self.master_id()?;
        let mut path = Vec::new();
        let mut seen = HashSet::new();
        let mut at = t;
        loop {
            let next = self.output_target(at)?;
            if next == master {
                return Some(path);
            }
            if !seen.insert(next) {
                return None;
            }
            at = self.track(next)?;
            path.push(at);
        }
    }

    /// `t`'s send to `cue` as the main mix has it: the level it reaches
    /// the master with (its fader and VCAs for a pre-fader cue mix, which
    /// a post-fader one has already, plus the faders of the buses on its
    /// way), its pan (a dual-panned image's middle), on when the main mix
    /// hears it (not muted, reaching the master). Buses and returns are
    /// off: what they carry depends on the main mix's mutes and solos, and
    /// their sources are in the cue mix on their own.
    pub fn main_mix_send(&self, t: &Track, cue: &CueMix) -> CueSend {
        let path = self.path_to_master(t);
        let own = if cue.post_fader() {
            0.0
        } else {
            t.volume_db + self.vca_chain(t).iter().map(|v| v.volume_db).sum::<f32>()
        };
        let buses: f32 = path.iter().flatten().map(|b| b.volume_db).sum();
        let level = own + buses;
        let level = if level <= SILENCE_DB || t.volume_db <= SILENCE_DB {
            SILENCE_DB
        } else {
            crate::edit::clamp_level(level)
        };
        let pan = match t.pan_mode {
            faderframe_core::PanMode::Dual if t.chain_layout().channel_count() == 2 => {
                (t.dual_pan.left + t.dual_pan.right) * 0.5
            }
            _ => t.pan,
        };
        let muted = self.effectively_muted(t, None)
            || self.folder_chain(t).iter().any(|f| f.mute)
            || path.iter().flatten().any(|b| b.mute);
        CueSend {
            cue: cue.id,
            level_db: level,
            pan: pan.clamp(-1.0, 1.0),
            on: path.is_some() && !muted && !t.kind.is_summing(),
        }
    }

    /// Every track's send to `cue` as the main mix has it
    /// ([`Self::main_mix_send`]).
    pub fn copy_main_mix(&self, cue: &CueMix) -> Vec<(TrackId, CueSend)> {
        self.tracks
            .iter()
            .filter(|t| feeds_cues(t.kind))
            .map(|t| (t.id, self.main_mix_send(t, cue)))
            .collect()
    }

    /// Device outputs the project plays on besides cue mix `except`: the
    /// hardware outputs of tracks (the master's too), the other cue mixes
    /// and hardware inserts' sends (as saved; their default is output 3).
    pub fn used_outputs(&self, except: Option<CueId>) -> BTreeSet<u16> {
        let mut used = BTreeSet::new();
        for t in &self.tracks {
            if let OutputRouting::Hardware { first_channel } = t.output {
                let n = self.destination_layout(t).channel_count().max(1) as u16;
                used.extend(first_channel..first_channel.saturating_add(n));
            }
            for s in t.slots() {
                if s.plugin.id != faderframe_core::builtin::HARDWARE_INSERT {
                    continue;
                }
                let send = s
                    .parameters
                    .iter()
                    .find(|v| v.id == faderframe_core::ParameterId(0))
                    .map_or(2.0, |v| v.value)
                    .round()
                    .clamp(0.0, 255.0) as u16;
                let n = t.chain_layout().channel_count().max(1) as u16;
                used.extend(send..send.saturating_add(n));
            }
        }
        for c in self.cue_mixes.iter().filter(|c| Some(c.id) != except) {
            used.extend(c.outputs());
        }
        used
    }

    /// The first outputs free for a cue mix of `layout` (a stereo one on a
    /// pair: 1-2, 3-4 …), besides cue mix `except`.
    pub fn free_cue_outputs(&self, layout: ChannelLayout, except: Option<CueId>) -> u16 {
        let used = self.used_outputs(except);
        let n = cue_layout(layout).channel_count() as u16;
        let mut first = 0u16;
        loop {
            if (first..first + n).all(|c| !used.contains(&c)) {
                return first;
            }
            first = first.saturating_add(n);
            if first > 250 {
                return first;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Command, History};

    struct Mix {
        p: Project,
        vox: TrackId,
        kick: TrackId,
        drums: TrackId,
        gtr: TrackId,
        verb: TrackId,
        guide: TrackId,
    }

    /// Vox → master; Kick → Drums (a bus at −4 dB) → master; a muted
    /// guitar; a reverb return; a guide track with no output.
    fn mix() -> Mix {
        let mut p = Project::new("cues", 48_000);
        let add = |p: &mut Project, kind: TrackKind, name: &str| {
            let id: TrackId = p.ids.allocate();
            let at = p.tracks.len() - 1;
            p.tracks
                .insert(at, Track::new(id, kind, name, TrackColor::palette(at)));
            id
        };
        let vox = add(&mut p, TrackKind::Audio, "Vox");
        let kick = add(&mut p, TrackKind::Audio, "Kick");
        let drums = add(&mut p, TrackKind::Bus, "Drums");
        let gtr = add(&mut p, TrackKind::Audio, "Gtr");
        let verb = add(&mut p, TrackKind::Aux, "Verb");
        let guide = add(&mut p, TrackKind::Audio, "Guide");
        let t = p.track_mut(vox).unwrap();
        t.volume_db = -6.0;
        t.pan = -0.3;
        let t = p.track_mut(kick).unwrap();
        t.volume_db = -3.0;
        t.output = OutputRouting::Track { track: drums };
        p.track_mut(drums).unwrap().volume_db = -4.0;
        p.track_mut(gtr).unwrap().mute = true;
        p.track_mut(verb).unwrap().volume_db = -10.0;
        p.track_mut(guide).unwrap().output = OutputRouting::None;
        Mix {
            p,
            vox,
            kick,
            drums,
            gtr,
            verb,
            guide,
        }
    }

    fn sends(p: &Project, cue: &CueMix) -> std::collections::HashMap<TrackId, CueSend> {
        p.copy_main_mix(cue).into_iter().collect()
    }

    #[test]
    fn a_copy_of_the_main_mix_takes_every_source_as_the_master_hears_it() {
        let m = mix();
        let cue = CueMix::new(CueId(900), "Cue 1", CUE_COLORS[0], 2, ChannelLayout::Stereo);
        let s = sends(&m.p, &cue);
        assert_eq!(
            s[&m.vox],
            CueSend {
                cue: cue.id,
                level_db: -6.0,
                pan: -0.3,
                on: true
            }
        );
        // Through the bus: its fader too.
        assert_eq!(s[&m.kick].level_db, -7.0);
        assert!(s[&m.kick].on);
        // Buses and returns carry the main mix's mutes and solos: off.
        assert!(!s[&m.drums].on && !s[&m.verb].on);
        assert_eq!(s[&m.verb].level_db, -10.0);
        // Muted in the main mix, not heard there at all: off.
        assert!(!s[&m.gtr].on);
        assert!(!s[&m.guide].on);
        assert!(!s.contains_key(&m.p.master_id().unwrap()));
        // Post-fader: the fader is in the tap already.
        let post = CueMix {
            tap: SendTap::PostFader,
            ..cue
        };
        let s = sends(&m.p, &post);
        assert_eq!(s[&m.vox].level_db, 0.0);
        assert_eq!(s[&m.kick].level_db, -4.0);
    }

    #[test]
    fn cue_mix_edits_undo_exactly() {
        let mut m = mix();
        let mut h = History::default();
        let id: CueId = m.p.ids.allocate();
        let cue = CueMix::new(id, "Cue 1", CUE_COLORS[0], 2, ChannelLayout::Stereo);
        let all = m.p.copy_main_mix(&cue);
        let before = m.p.clone();
        h.apply(
            &mut m.p,
            Command::AddCueMix {
                cue: Box::new(cue),
                index: None,
                sends: all,
            },
        )
        .unwrap();
        assert_eq!(m.p.cue_mixes.len(), 1);
        assert!(m.p.track(m.vox).unwrap().cue_send(id).is_some());
        let added = m.p.clone();
        // A send made by a level edit, then a pan, a switch.
        m.p.track_mut(m.guide).unwrap().cue_sends.clear();
        let without_guide = m.p.clone();
        for cmd in [
            Command::SetCueSendLevel {
                track: m.guide,
                cue: id,
                db: -9.0,
            },
            Command::SetCueSendPan {
                track: m.vox,
                cue: id,
                pan: 0.8,
            },
            Command::SetCueSendOn {
                track: m.drums,
                cue: id,
                on: true,
            },
            Command::SetCueLevel { cue: id, db: -3.0 },
            Command::SetCueClick {
                cue: id,
                on: false,
                db: -6.0,
            },
        ] {
            h.apply(&mut m.p, cmd).unwrap();
        }
        let guide = m.p.track(m.guide).unwrap().cue_send(id).copied().unwrap();
        assert_eq!((guide.level_db, guide.on), (-9.0, true));
        assert_eq!(m.p.track(m.vox).unwrap().cue_send(id).unwrap().pan, 0.8);
        assert!(m.p.track(m.drums).unwrap().cue_send(id).unwrap().on);
        for _ in 0..5 {
            h.undo(&mut m.p).unwrap();
        }
        assert_eq!(m.p, without_guide);
        m.p = added.clone();
        // Removing it takes every send; undo puts them back.
        h.apply(&mut m.p, Command::RemoveCueMix { cue: id })
            .unwrap();
        assert!(m.p.cue_mixes.is_empty());
        assert!(m.p.tracks.iter().all(|t| t.cue_sends.is_empty()));
        h.undo(&mut m.p).unwrap();
        assert_eq!(m.p, added);
        h.undo(&mut m.p).unwrap();
        assert_eq!(m.p, before);
        // A track that cannot feed a cue mix is refused.
        h.redo(&mut m.p).unwrap();
        let master = m.p.master_id().unwrap();
        assert!(
            Command::SetCueSendLevel {
                track: master,
                cue: id,
                db: 0.0
            }
            .apply(&mut m.p)
            .is_err()
        );
    }

    #[test]
    fn files_keep_cue_mixes_and_older_files_have_none() {
        let mut m = mix();
        let id: CueId = m.p.ids.allocate();
        let mut cue = CueMix::new(id, "Drummer", CUE_COLORS[1], 4, ChannelLayout::Mono);
        cue.tap = SendTap::PostFader;
        cue.click_db = -4.0;
        let all = m.p.copy_main_mix(&cue);
        Command::AddCueMix {
            cue: Box::new(cue),
            index: None,
            sends: all,
        }
        .apply(&mut m.p)
        .unwrap();
        let text = crate::file::to_string(&m.p, None).unwrap();
        let back = crate::file::from_str(&text).unwrap().project;
        assert_eq!(back.cue_mixes, m.p.cue_mixes);
        assert_eq!(back.tracks, m.p.tracks);
        // A file from before cue mixes: none, and nothing written for them.
        let plain = mix().p;
        let text = crate::file::to_string(&plain, None).unwrap();
        assert!(!text.contains("cue_mixes") && !text.contains("cue_sends"));
        assert!(
            crate::file::from_str(&text)
                .unwrap()
                .project
                .cue_mixes
                .is_empty()
        );
        // Sends to a cue mix that is gone go when the file is read.
        let mut broken = m.p.clone();
        broken.cue_mixes.clear();
        let text = crate::file::to_string(&broken, None).unwrap();
        let back = crate::file::from_str(&text).unwrap().project;
        assert!(back.tracks.iter().all(|t| t.cue_sends.is_empty()));
    }

    #[test]
    fn a_new_cue_mix_takes_the_next_free_outputs() {
        let mut m = mix();
        // The master on 1-2: the first cue mix on 3-4.
        assert_eq!(m.p.free_cue_outputs(ChannelLayout::Stereo, None), 2);
        assert_eq!(m.p.free_cue_outputs(ChannelLayout::Mono, None), 2);
        // A direct out on 3-4, a cue mix on 5-6: the next pair is 7-8.
        let t = m.p.track_mut(m.guide).unwrap();
        t.output = OutputRouting::Hardware { first_channel: 2 };
        t.layout = ChannelLayout::Stereo;
        let id: CueId = m.p.ids.allocate();
        m.p.cue_mixes.push(CueMix::new(
            id,
            "Cue 1",
            CUE_COLORS[0],
            4,
            ChannelLayout::Stereo,
        ));
        assert_eq!(m.p.free_cue_outputs(ChannelLayout::Stereo, None), 6);
        // Its own outputs are free for itself.
        assert_eq!(m.p.free_cue_outputs(ChannelLayout::Stereo, Some(id)), 4);
        assert_eq!(m.p.cue(id).unwrap().output_label(), "Out 5-6");
        assert_eq!(output_label(6, ChannelLayout::Mono), "Out 7");
    }
}
