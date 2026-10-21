//! A track's devices in signal-flow order, as the Inspector and the
//! Channel Overview show them, and what their controls do.
//!
//! Read straight from the project (the order of [`Track::slots`]: the
//! input stage, a legacy instrument slot, the inserts and, after each
//! container, what its chains hold) and the engine (latency, a failed
//! plugin, gain reduction): nothing is kept here. Every control is an
//! existing session action — the same the mixer's insert slots send —
//! so the overview is another face of the one processing chain.

use faderframe_core::{PluginInstanceId, TrackId};
use faderframe_project::{Command, PluginFormat, PluginSlot, Track, TrackKind};
use faderframe_session::{Action, PluginTarget, Session};

/// What a device is in the chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// The input stage (a preamp, the console's bus amplifier).
    InputStage,
    Instrument,
    MidiEffect,
    /// A container: parallel chains of devices.
    Container,
    Effect,
}

impl Role {
    /// Its tag on a card.
    pub fn tag(self) -> &'static str {
        match self {
            Role::InputStage => "INPUT STAGE",
            Role::Instrument => "INSTRUMENT",
            Role::MidiEffect => "MIDI FX",
            Role::Container => "CONTAINER",
            Role::Effect => "EFFECT",
        }
    }
}

/// One device of a track's chain.
#[derive(Clone, Debug, PartialEq)]
pub struct Device {
    pub plugin: PluginInstanceId,
    /// As every view names it ("Delay (2)"), without "FaderFrame ".
    pub name: String,
    pub role: Role,
    /// 0: on the track; 1 and more: inside a container's chain.
    pub depth: usize,
    pub bypass: bool,
    /// The plugin stopped working (a sandboxed one crashed).
    pub failed: bool,
    /// Samples (hosted plugins that report it).
    pub latency: Option<u32>,
    /// Its place among the track's inserts (top-level inserts only: they
    /// are the ones that move).
    pub insert: Option<usize>,
    /// The track keying it.
    pub sidechain: Option<String>,
    /// The gain it takes off now (dB), when it reports one.
    pub reduction: Option<f32>,
    /// "Built-in", "CLAP", "VST3", "AU", "LV2".
    pub format: &'static str,
    /// Its current program, when it has a list of them.
    pub program: Option<String>,
    /// How many chains a container has.
    pub chains: usize,
}

fn format_name(f: PluginFormat) -> &'static str {
    match f {
        PluginFormat::Builtin => "Built-in",
        PluginFormat::Clap => "CLAP",
        PluginFormat::Vst3 => "VST3",
        PluginFormat::AudioUnit => "AU",
        PluginFormat::Lv2 => "LV2",
    }
}

/// `t`'s devices in signal-flow order.
pub fn devices(model: &Session, t: &Track) -> Vec<Device> {
    let make = |s: &PluginSlot, role: Role, depth: usize, insert: Option<usize>| {
        let named = t.name_of(s);
        let program = model
            .plugin_current_program(s.id)
            .and_then(|i| model.plugin_programs(s.id).get(i).cloned());
        Device {
            plugin: s.id,
            name: named.trim_start_matches("FaderFrame ").to_string(),
            role,
            depth,
            bypass: s.bypass,
            failed: model.plugin_failed(s.id),
            latency: model.engine().plugin_latency(s.id).filter(|l| *l > 0),
            insert,
            sidechain: s
                .sidechain
                .and_then(|k| model.project().track(k))
                .map(|k| k.name.clone()),
            reduction: model.engine().plugin_reduction(s.id),
            format: format_name(s.plugin.format),
            program,
            chains: t.containers.get(&s.id).map_or(0, Vec::len),
        }
    };
    let role_of = |s: &PluginSlot| {
        if s.plugin.is_container() {
            Role::Container
        } else if model.engine_plugin_is_instrument(s.id) {
            Role::Instrument
        } else if model.is_midi_effect(&s.plugin) {
            Role::MidiEffect
        } else {
            Role::Effect
        }
    };
    fn walk(t: &Track, s: &PluginSlot, depth: usize, out: &mut Vec<(PluginInstanceId, usize)>) {
        out.push((s.id, depth));
        if depth >= faderframe_project::container::MAX_DEPTH {
            return;
        }
        if let Some(chains) = t.containers.get(&s.id) {
            for c in chains {
                for x in &c.inserts {
                    walk(t, x, depth + 1, out);
                }
            }
        }
    }
    let mut out = Vec::new();
    if let Some(s) = &t.preamp {
        out.push(make(s, Role::InputStage, 0, None));
    }
    if let Some(s) = &t.instrument {
        out.push(make(s, Role::Instrument, 0, None));
    }
    for (i, s) in t.inserts.iter().enumerate() {
        let mut nested = Vec::new();
        walk(t, s, 0, &mut nested);
        for (id, depth) in nested {
            let Some(slot) = t.plugin(id) else { continue };
            let insert = (depth == 0).then_some(i);
            out.push(make(slot, role_of(slot), depth, insert));
        }
    }
    out
}

/// Whether `t` takes devices (the "+" is offered).
pub fn holds_devices(t: &Track) -> bool {
    t.kind.has_audio() || t.kind == TrackKind::Midi
}

/// The device a new one goes after: the plugin browser for the next
/// insert (an instrument track without an instrument: its instrument).
pub fn add(model: &Session, t: &Track) -> Action {
    let target = if t.kind == TrackKind::Instrument && model.instrument_slot(t).is_none() {
        PluginTarget::Instrument
    } else {
        PluginTarget::Insert(t.inserts.len())
    };
    Action::OpenPluginBrowser {
        track: t.id,
        target,
    }
}

/// Its editor (a built-in's, a hosted plugin's own window).
pub fn open(track: TrackId, d: &Device) -> Action {
    Action::OpenPluginEditor {
        track,
        plugin: d.plugin,
        generic: false,
    }
}

/// Switch it off or on.
pub fn toggle_bypass(track: TrackId, d: &Device) -> Action {
    Action::Edit(Command::SetPluginBypass {
        track,
        plugin: d.plugin,
        bypass: !d.bypass,
    })
}

/// Take it out (as the mixer does: an input stage by the input-stage
/// action — the master's is the console's —, a device in a container out
/// of its chain). `None`: a legacy instrument slot, which the session
/// converts on its own.
pub fn remove(t: &Track, d: &Device) -> Option<Action> {
    if d.role == Role::InputStage {
        return Some(if t.kind == TrackKind::Master {
            Action::SetConsole { family: None }
        } else {
            Action::SetPreamp {
                track: t.id,
                model: None,
            }
        });
    }
    if d.depth > 0 {
        return Some(Action::RemoveFromChain {
            track: t.id,
            plugin: d.plugin,
        });
    }
    d.insert.map(|_| {
        Action::Edit(Command::RemovePlugin {
            track: t.id,
            plugin: d.plugin,
        })
    })
}

/// Move a top-level insert to insert slot `index` (the session keeps MIDI
/// effects before the instrument). `None` when it does not move.
pub fn move_to(track: TrackId, d: &Device, index: usize) -> Option<Action> {
    let from = d.insert?;
    (from != index).then_some(Action::MovePlugin {
        track,
        plugin: d.plugin,
        to: track,
        index,
    })
}

/// The insert slot a device dropped into gap `gap` (0: before the first
/// device shown) lands in: gaps count devices shown, inserts move by
/// their own indices.
pub fn drop_index(devices: &[Device], dragged: &Device, gap: usize) -> Option<usize> {
    dragged.insert?;
    // The top-level inserts before the gap, the dragged one left out.
    Some(
        devices[..gap.min(devices.len())]
            .iter()
            .filter(|d| d.insert.is_some() && d.plugin != dragged.plugin)
            .count(),
    )
}

/// Its latency as it reads ("2.7 ms").
pub fn latency_text(samples: u32, rate: u32) -> String {
    let ms = f64::from(samples) * 1000.0 / f64::from(rate.max(1));
    if ms >= 10.0 {
        format!("{ms:.0} ms")
    } else {
        format!("{ms:.1} ms")
    }
}

/// The whole chain's latency (samples): its devices in series, not
/// bypassed (a container's chains run side by side: its own report).
pub fn total_latency(devices: &[Device]) -> u32 {
    devices
        .iter()
        .filter(|d| d.depth == 0 && !d.bypass)
        .filter_map(|d| d.latency)
        .sum()
}

/// The track a view of one track follows: the one clicked last among the
/// selected tracks, else the first selected in the order the arranger
/// lists them (the master last). Tracks removed since are skipped.
pub fn inspected_track(model: &Session) -> Option<TrackId> {
    let sel = &model.selection.tracks;
    let p = model.project();
    if let Some(a) = model.selection.anchor_track
        && sel.contains(&a)
        && p.track(a).is_some()
    {
        return Some(a);
    }
    p.folder_order()
        .into_iter()
        .chain(p.master())
        .find(|t| sel.contains(&t.id))
        .map(|t| t.id)
}

/// How many of the selected tracks still exist.
pub fn selected_count(model: &Session) -> usize {
    model
        .selection
        .tracks
        .iter()
        .filter(|t| model.project().track(**t).is_some())
        .count()
}

/// What a list of session choices has chosen: its checked entries' labels
/// ("All MIDI Inputs · Channel 1"), "—" when none is.
pub fn checked_label(choices: &[faderframe_session::InputChoice]) -> String {
    let on: Vec<&str> = choices
        .iter()
        .filter(|c| c.checked)
        .map(|c| c.label.as_str())
        .collect();
    if on.is_empty() {
        "—".into()
    } else {
        on.join(" · ")
    }
}

/// Whether `t` has an input of its own to choose (audio from the device,
/// notes from a MIDI input, a plugin's extra output).
pub fn has_input(t: &Track) -> bool {
    matches!(
        t.kind,
        TrackKind::Audio | TrackKind::Instrument | TrackKind::Midi
    ) || t.input.plugin_output().is_some()
}

fn plural(n: usize, one: &str) -> String {
    format!("{n} {one}{}", if n == 1 { "" } else { "s" })
}

/// What a folder (or folder bus) holds: "3 tracks" (nested folders'
/// tracks too).
pub fn contents_label(model: &Session, t: &Track) -> String {
    plural(model.folder_contents(t.id).len(), "track")
}

/// What feeds `t`, as the input menus name it; a bus, an aux or the
/// master: how many tracks (as they are routed: a folder bus's tracks
/// with the default output play through it) and sends reach it.
pub fn input_label(model: &Session, t: &Track) -> String {
    if let Some((plugin, bus)) = t.input.plugin_output() {
        return model
            .plugin_output_buses(plugin)
            .into_iter()
            .find(|o| o.bus == bus)
            .map_or_else(|| "a plugin output".into(), |o| o.name);
    }
    match t.kind {
        TrackKind::Instrument | TrackKind::Midi => checked_label(&model.midi_input_choices(t.id)),
        TrackKind::Audio => checked_label(&model.input_choices(t.id)),
        TrackKind::Folder => contents_label(model, t),
        TrackKind::Vca => format!(
            "controls {}",
            plural(model.project().vca_members(t.id).len(), "track")
        ),
        _ => {
            let p = model.project();
            let routed = p
                .tracks
                .iter()
                .filter(|x| x.id != t.id && x.kind.has_audio())
                .filter(|x| p.output_target(x) == Some(t.id))
                .count();
            let sends = p
                .tracks
                .iter()
                .flat_map(|x| &x.sends)
                .filter(|s| s.target == t.id)
                .count();
            match (routed, sends) {
                (0, 0) => "nothing routed here".into(),
                (r, 0) => plural(r, "track"),
                (0, s) => plural(s, "send"),
                (r, s) => format!("{} · {}", plural(r, "track"), plural(s, "send")),
            }
        }
    }
}

/// Where `t` goes: the master, a bus or aux, hardware outputs, nothing;
/// a MIDI track: the instrument track it plays.
pub fn output_label(model: &Session, t: &Track) -> String {
    use faderframe_project::OutputRouting;
    if t.kind == TrackKind::Midi {
        return checked_label(&model.midi_instrument_choices(t.id));
    }
    match t.output {
        OutputRouting::Master => "Master".into(),
        OutputRouting::Track { track } => model.project().track(track).map_or_else(
            || "?".into(),
            |d| format!("{} ({})", d.name, d.kind.label()),
        ),
        OutputRouting::Hardware { first_channel } => {
            let first = usize::from(first_channel);
            let n = t.layout.channel_count().max(2);
            format!("Hardware Out {}-{}", first + 1, first + n)
        }
        OutputRouting::None => "Not connected".into(),
    }
}
