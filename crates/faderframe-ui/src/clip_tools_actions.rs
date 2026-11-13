//! Clip envelopes and Strip Silence from menus and scripts
//! (`FADERFRAME_STARTUP_ACTIONS`):
//!
//! * `app.strip-silence` (Edit → Strip Silence…; target `""`: the
//!   dialog on the selected audio clips). In scripts
//!   `strip-silence:<track>@<dB>/<ms>` opens it on the track's audio clips
//!   with that threshold and minimum length (the regions drawn on them),
//!   `strip-silence:apply[=strip|separate|new-clips]` does it,
//!   `strip-silence:close` closes it;
//! * `clip-envelope:<track>[@<take n>]=[pan:]<t>/<value>[|…]` — the
//!   track's first audio clip's (or take n's of its first take folder)
//!   volume (dB) or pan (−1…1) envelope, seconds from its start, and that
//!   lane shown;
//! * Edit → Clip Envelopes (`show-clip-envelope:<hide|volume|pan>`, a
//!   choice of `crate::toggles`).

use crate::state::AppState;
use faderframe_project::clip_envelope::EnvelopeLane;
use faderframe_session::Action;
use faderframe_session::strip_silence::{StripMode, StripSilenceOp};
use gtk::gio;
use gtk::prelude::*;
use std::rc::Rc;

pub(crate) fn install(app: &Rc<AppState>) {
    let named = |name: &'static str, f: fn(&Rc<AppState>, &str)| {
        let weak = Rc::downgrade(app);
        gio::ActionEntry::builder(name)
            .parameter_type(Some(&String::static_variant_type()))
            .activate(move |_, _, param| {
                if let (Some(a), Some(arg)) =
                    (weak.upgrade(), param.and_then(|p| p.get::<String>()))
                {
                    f(&a, &arg);
                }
            })
            .build()
    };
    app.app.add_action_entries([
        named("strip-silence", strip_silence),
        named("clip-envelope", clip_envelope),
    ]);
}

fn track_named(a: &Rc<AppState>, name: &str) -> Option<faderframe_core::TrackId> {
    let s = a.session.borrow();
    s.project()
        .tracks
        .iter()
        .find(|t| t.name == name.trim())
        .map(|t| t.id)
}

/// The id of the Edit → Clip Envelopes choice now.
pub(crate) fn shown_lane(s: &faderframe_session::Session) -> &'static str {
    match s.clip_envelope_shown() {
        None => "hide",
        Some(EnvelopeLane::Volume) => "volume",
        Some(EnvelopeLane::Pan) => "pan",
    }
}

/// Choose the shown lane by its id.
pub(crate) fn show_lane(a: &Rc<AppState>, id: &str) {
    let lane = match id {
        "hide" => None,
        "volume" => Some(EnvelopeLane::Volume),
        "pan" => Some(EnvelopeLane::Pan),
        _ => return tracing::warn!("show-clip-envelope: unknown '{id}'"),
    };
    a.dispatch(Action::ShowClipEnvelope(lane));
}

fn strip_silence(a: &Rc<AppState>, arg: &str) {
    let arg = arg.trim();
    let op = |op| a.dispatch(Action::StripSilence(op));
    if arg.is_empty() {
        return op(StripSilenceOp::Open { clips: Vec::new() });
    }
    if arg == "close" {
        return op(StripSilenceOp::Close);
    }
    if let Some(mode) = arg.strip_prefix("apply") {
        let mode = match mode.trim_start_matches('=') {
            "" | "strip" => StripMode::Strip,
            "separate" => StripMode::Separate,
            "new-clips" => StripMode::NewClips,
            m => return tracing::warn!("strip-silence: unknown mode '{m}'"),
        };
        return op(StripSilenceOp::Apply(mode));
    }
    let Some((track, rest)) = arg.rsplit_once('@') else {
        return tracing::warn!("strip-silence: '{arg}' is not <track>@<dB>/<ms>");
    };
    let Some(track) = track_named(a, track) else {
        return tracing::warn!("strip-silence: no track '{track}'");
    };
    let (db, ms) = rest.split_once('/').unwrap_or((rest, ""));
    let (Ok(db), ms) = (db.trim().parse::<f32>(), ms.trim().parse::<f64>().ok()) else {
        return tracing::warn!("strip-silence: '{rest}' is not <dB>/<ms>");
    };
    let clips: Vec<faderframe_core::ClipId> = {
        let s = a.session.borrow();
        s.project()
            .clips_of(track)
            .into_iter()
            .filter(|c| c.as_audio().is_some())
            .map(|c| c.id)
            .collect()
    };
    let mut settings = a.session.borrow().strip_silence_settings();
    settings.threshold_db = db;
    if let Some(ms) = ms {
        settings.min_length = ms / 1000.0;
    }
    op(StripSilenceOp::Settings(settings));
    op(StripSilenceOp::Open { clips });
    let measured = a
        .session
        .borrow_mut()
        .wait_for_strip_silence(std::time::Duration::from_secs(30));
    if let Some(p) = a.session.borrow().strip_silence_preview() {
        let (regions, clips) = p.counts();
        tracing::info!("strip-silence: {regions} regions in {clips} clips (measured: {measured})");
    }
}

fn clip_envelope(a: &Rc<AppState>, arg: &str) {
    let Some((track, spec)) = arg.split_once('=') else {
        return tracing::warn!("clip-envelope: '{arg}' is not <track>[@<take n>]=<t>/<dB>[|…]");
    };
    let (lane, spec) = match spec.trim().strip_prefix("pan:") {
        Some(rest) => (EnvelopeLane::Pan, rest),
        None => (
            EnvelopeLane::Volume,
            spec.trim().strip_prefix("volume:").unwrap_or(spec.trim()),
        ),
    };
    let (audio, rate) = {
        let s = a.session.borrow();
        (
            crate::actions::audio_target(&s, track),
            s.project().sample_rate,
        )
    };
    let Some(audio) = audio else {
        return tracing::warn!("clip-envelope: no audio clip or take '{track}'");
    };
    let Some(points) = faderframe_session::clip_envelope::parse_points(spec, rate) else {
        return tracing::warn!("clip-envelope: '{spec}' is not <t>/<value>[|…]");
    };
    a.dispatch(Action::ShowClipEnvelope(Some(lane)));
    a.dispatch(Action::SetClipEnvelope {
        audio,
        lane,
        points,
    });
}
