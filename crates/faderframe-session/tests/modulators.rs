//! Modulators through the session: added and named, changed in one undo
//! step per gesture, mapped by touching a parameter, and what they offer
//! to move.
#![allow(clippy::unwrap_used)]

use faderframe_core::{ParameterId, TrackId};
use faderframe_engine::EngineConfig;
use faderframe_plugin_host::devices::synth::id as synth;
use faderframe_project::Command;
use faderframe_project::modulation::{LfoShape, ModRate, ModRoute, ModSource, ModTarget};
use faderframe_session::{Action, Session};

fn track(s: &Session, name: &str) -> TrackId {
    s.project()
        .tracks
        .iter()
        .find(|t| t.name == name)
        .unwrap()
        .id
}

/// The demo, its lead synth without the modulators it comes with.
fn demo() -> Session {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let lead = track(&s, "Lead Synth");
    s.dispatch(Action::Edit(Command::SetModulators {
        track: lead,
        modulators: Vec::new(),
    }))
    .unwrap();
    s
}

fn lfo() -> ModSource {
    ModSource::Lfo {
        shape: LfoShape::Sine,
        rate: ModRate::Sync { beats: 1.0 },
        phase: 0.0,
    }
}

fn names(s: &Session, t: TrackId) -> Vec<String> {
    s.project()
        .track(t)
        .unwrap()
        .modulators
        .iter()
        .map(|m| m.name.clone())
        .collect()
}

#[test]
fn modulators_are_added_named_changed_in_one_step_and_undone() {
    let mut s = demo();
    let lead = track(&s, "Lead Synth");
    for source in [lfo(), lfo(), ModSource::Macro { value: 0.0 }] {
        s.dispatch(Action::AddModulator {
            track: lead,
            source,
        })
        .unwrap();
    }
    assert_eq!(names(&s, lead), ["LFO", "LFO 2", "Macro"]);
    // A folder has no audio of its own to modulate.
    let bass = track(&s, "Bass");
    s.dispatch(Action::NewFolder { tracks: vec![bass] })
        .unwrap();
    let folder = s.project().track(bass).unwrap().folder.unwrap();
    assert!(
        s.dispatch(Action::AddModulator {
            track: folder,
            source: lfo()
        })
        .is_err()
    );
    // A depth dragged: one undo step.
    let mut m = s.project().track(lead).unwrap().modulators[0].clone();
    m.routes.push(ModRoute {
        target: ModTarget::Volume,
        depth: 0.0,
    });
    s.dispatch(Action::BeginGesture("Modulation Depth".into()))
        .unwrap();
    for d in [0.1, 0.2, 0.3] {
        m.routes[0].depth = d;
        s.dispatch(Action::SetModulator {
            track: lead,
            modulator: m.clone(),
        })
        .unwrap();
    }
    s.dispatch(Action::EndGesture).unwrap();
    assert_eq!(
        s.project().track(lead).unwrap().modulators[0].routes[0].depth,
        0.3
    );
    s.dispatch(Action::Undo).unwrap();
    assert!(
        s.project().track(lead).unwrap().modulators[0]
            .routes
            .is_empty()
    );
    // Removed, and back with undo.
    let id = m.id;
    s.dispatch(Action::RemoveModulator {
        track: lead,
        modulator: id,
    })
    .unwrap();
    assert_eq!(names(&s, lead), ["LFO 2", "Macro"]);
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(names(&s, lead), ["LFO", "LFO 2", "Macro"]);
}

#[test]
fn touching_a_parameter_maps_it_and_leaves_its_value() {
    let mut s = demo();
    let lead = track(&s, "Lead Synth");
    let plugin = s.project().track(lead).unwrap().inserts[0].id;
    let cutoff = ParameterId(synth::CUTOFF);
    s.dispatch(Action::AddModulator {
        track: lead,
        source: lfo(),
    })
    .unwrap();
    let id = s.project().track(lead).unwrap().modulators[0].id;
    // What it can move: the fader, the pan and the synth's continuous
    // parameters (not a waveform switch).
    let targets: Vec<ModTarget> = s
        .modulation_targets(lead)
        .into_iter()
        .map(|c| c.target)
        .collect();
    let param = |p: u32| ModTarget::Plugin {
        plugin,
        parameter: ParameterId(p),
    };
    assert_eq!(targets[..2], [ModTarget::Volume, ModTarget::Pan]);
    assert!(targets.contains(&param(synth::CUTOFF)));
    assert!(!targets.contains(&param(synth::OSC1_WAVE)));
    assert_eq!(
        s.modulation_target_name(lead, param(synth::CUTOFF))
            .unwrap(),
        "FaderFrame Synth · Cutoff"
    );

    let before = s.plugin_parameter_value(plugin, cutoff).unwrap();
    s.dispatch(Action::LearnModulation(Some((lead, id))))
        .unwrap();
    assert_eq!(s.modulation_learning(), Some((lead, id)));
    s.dispatch(Action::BeginGesture("Cutoff".into())).unwrap();
    for v in [3_000.0, 4_000.0] {
        s.dispatch(Action::Edit(Command::SetPluginParameter {
            track: lead,
            plugin,
            parameter: cutoff,
            value: Some(v),
        }))
        .unwrap();
    }
    s.dispatch(Action::EndGesture).unwrap();
    let m = &s.project().track(lead).unwrap().modulators[0];
    assert_eq!(
        m.routes,
        [ModRoute {
            target: param(synth::CUTOFF),
            depth: 0.25
        }]
    );
    assert_eq!(s.plugin_parameter_value(plugin, cutoff).unwrap(), before);
    assert!(s.is_modulated(plugin, cutoff));
    // Mapping is over: the next move moves the value.
    assert_eq!(s.modulation_learning(), None);
    s.dispatch(Action::Edit(Command::SetPluginParameter {
        track: lead,
        plugin,
        parameter: cutoff,
        value: Some(3_000.0),
    }))
    .unwrap();
    assert_eq!(s.plugin_parameter_value(plugin, cutoff).unwrap(), 3_000.0);
}

#[test]
fn modulator_values_follow_the_list() {
    let mut s = demo();
    let lead = track(&s, "Lead Synth");
    assert!(s.modulator_values(lead).is_empty());
    for value in [0.7, 0.2] {
        s.dispatch(Action::AddModulator {
            track: lead,
            source: ModSource::Macro { value },
        })
        .unwrap();
    }
    // Without a running stream nothing has played yet (the engine's tests
    // check what they put out).
    let ids: Vec<_> = s
        .project()
        .track(lead)
        .unwrap()
        .modulators
        .iter()
        .map(|m| m.id)
        .collect();
    let values = s.modulator_values(lead);
    assert_eq!(values.iter().map(|v| v.0).collect::<Vec<_>>(), ids);
}

#[test]
fn note_modulators_map_only_to_devices_that_get_the_notes() {
    let mut s = demo();
    let lead = track(&s, "Lead Synth");
    let synth_slot = s.project().track(lead).unwrap().inserts[0].id;
    s.dispatch(Action::InsertPlugin {
        track: lead,
        index: 1,
        plugin: faderframe_project::PluginRef::builtin(faderframe_core::builtin::GAIN, "Utility"),
    })
    .unwrap();
    let utility = s.project().track(lead).unwrap().inserts[1].id;
    s.dispatch(Action::AddModulator {
        track: lead,
        source: ModSource::Velocity,
    })
    .unwrap();
    let id = s.project().track(lead).unwrap().modulators[0].id;
    // The synth gets the notes (its parameters move as the newest note
    // has it: built-ins take no per-voice modulation); the utility not.
    let targets = s.modulation_targets(lead);
    let of = |plugin| {
        targets
            .iter()
            .find(|c| matches!(c.target, ModTarget::Plugin { plugin: p, .. } if p == plugin))
            .unwrap()
            .clone()
    };
    assert!(of(synth_slot).takes_notes && !of(synth_slot).per_note);
    assert!(!of(utility).takes_notes);
    s.dispatch(Action::LearnModulation(Some((lead, id))))
        .unwrap();
    s.dispatch(Action::Edit(Command::SetPluginParameter {
        track: lead,
        plugin: utility,
        parameter: ParameterId(0),
        value: Some(-3.0),
    }))
    .unwrap();
    assert!(
        s.project().track(lead).unwrap().modulators[0]
            .routes
            .is_empty()
    );
    assert_eq!(
        s.plugin_parameter_value(utility, ParameterId(0)),
        Some(-3.0)
    );
    s.dispatch(Action::Edit(Command::SetPluginParameter {
        track: lead,
        plugin: synth_slot,
        parameter: ParameterId(synth::CUTOFF),
        value: Some(3_000.0),
    }))
    .unwrap();
    assert_eq!(
        s.project().track(lead).unwrap().modulators[0].routes.len(),
        1
    );
}
