//! IAMF masters (Render → "IAMF · Opus/FLAC/LPCM"): the master — mono,
//! stereo or a bed — rendered at a rate the codec carries, finished
//! (loudness measured on IAMF's stereo down-mix, which is what most
//! listeners hear), measured on Stereo and on its own layout, coded and
//! written as MP4 (`.mp4`) or a standalone IA Sequence (`.iamf`). See
//! `faderframe_iamf`.
//!
//! Formats IAMF has no layout for are carried in the smallest one that
//! holds them, the missing speakers silent: LCR and 5.0 as 5.1, quad (its
//! rears behind the listener) and 7.0 as 7.1. FaderFrame's x.1.2 heights
//! (top middle) are IAMF's x.1.2 top front pair.

use crate::delivery::Finished;
use crate::render::{RenderError, RenderJob, RenderProgress, RenderSettings, Rendered};
use faderframe_core::{ChannelLayout, SurroundFormat};
use faderframe_iamf::{Codec, Container, Layout, Loudness, Master};
use faderframe_project::Project;
use std::sync::Arc;
use std::sync::atomic::Ordering;

/// How the master goes into IAMF: the layout, and for each of its channels
/// (in IAMF's order) the master channel it carries (`None`: silent).
#[derive(Clone, Debug, PartialEq)]
pub struct IamfPlan {
    pub layout: Layout,
    pub channels: Vec<Option<usize>>,
    /// The master's channels.
    pub outputs: usize,
    /// The master's format ("LCR", "Stereo").
    pub master: &'static str,
}

impl IamfPlan {
    /// "7.1.4", "5.1 (LCR in it)".
    pub fn describe(&self) -> String {
        if self.master == self.layout.name() {
            self.master.to_string()
        } else {
            format!("{} ({} in it)", self.layout.name(), self.master)
        }
    }
}

/// The layout for a master format.
fn layout_for(layout: ChannelLayout) -> Layout {
    match layout {
        ChannelLayout::Mono => Layout::Mono,
        ChannelLayout::Surround(f) => match f {
            SurroundFormat::Lcr | SurroundFormat::S50 | SurroundFormat::S51 => Layout::S51,
            SurroundFormat::Quad | SurroundFormat::S70 | SurroundFormat::S71 => Layout::S71,
            SurroundFormat::S512 => Layout::S512,
            SurroundFormat::S514 => Layout::S514,
            SurroundFormat::S712 => Layout::S712,
            SurroundFormat::S714 => Layout::S714,
        },
        _ => Layout::Stereo,
    }
}

pub fn plan(project: &Project) -> IamfPlan {
    let master = project.master().map_or(ChannelLayout::Stereo, |m| m.layout);
    let layout = layout_for(master);
    let labels: Vec<&str> = match master {
        ChannelLayout::Surround(f) => f.speakers().iter().map(|s| s.label).collect(),
        ChannelLayout::Mono => vec!["C"],
        _ => vec!["L", "R"],
    };
    let quad = master == ChannelLayout::Surround(SurroundFormat::Quad);
    let find = |names: &[&str]| {
        names
            .iter()
            .find_map(|n| labels.iter().position(|l| l == n))
    };
    let channels = layout
        .channels()
        .iter()
        .map(|name| match *name {
            // Quad's rears stand behind the listener.
            "Lrs" if quad => find(&["Ls"]),
            "Rrs" if quad => find(&["Rs"]),
            "Ltf" => find(&["Ltf", "Ltm"]),
            "Rtf" => find(&["Rtf", "Rtm"]),
            "Ltb" => find(&["Ltr"]),
            "Rtb" => find(&["Rtr"]),
            other => find(&[other]),
        })
        .collect();
    IamfPlan {
        layout,
        channels,
        outputs: labels.len(),
        master: match master {
            ChannelLayout::Surround(f) => f.name(),
            ChannelLayout::Mono => "Mono",
            _ => "Stereo",
        },
    }
}

/// The loudness IAMF carries: on Stereo (IAMF's down-mix) and on the
/// layout itself (BS.1770 weights), with sample and true peaks.
pub fn measure(layout: Layout, audio: &[&[f32]], rate: u32) -> Vec<Loudness> {
    let peak = |chs: &[&[f32]]| {
        let p = chs
            .iter()
            .flat_map(|c| c.iter())
            .fold(0.0f32, |m, v| m.max(v.abs()));
        if p > 0.0 {
            20.0 * f64::from(p).log10()
        } else {
            f64::NEG_INFINITY
        }
    };
    let [l, r] = faderframe_iamf::stereo_downmix(layout, audio);
    let stereo: [&[f32]; 2] = [&l, &r];
    let (integrated, true_peak) =
        faderframe_analysis::integrated_weighted(&stereo, &[1.0, 1.0], rate);
    let mut out = vec![Loudness {
        layout: Layout::Stereo,
        integrated,
        digital_peak: peak(&stereo),
        true_peak,
    }];
    // Mono's highest loudness layout is Stereo (§ 3.7).
    if !matches!(layout, Layout::Stereo | Layout::Mono) {
        let (integrated, true_peak) = faderframe_analysis::integrated_weighted(
            audio,
            &faderframe_iamf::loudness_weights(layout),
            rate,
        );
        out.push(Loudness {
            layout,
            integrated,
            digital_peak: peak(audio),
            true_peak,
        });
    }
    out
}

/// Render `project` as an IAMF master on a worker thread.
pub fn start(
    project: Project,
    settings: RenderSettings,
    codec: Codec,
) -> Result<RenderJob, RenderError> {
    let mut project = project;
    project.loop_enabled = false;
    let plan = plan(&project);
    let rate = codec.rate_for(settings.sample_rate);
    let (a, b) = crate::render::resolve_range(&project, settings.range)?;
    let sr = f64::from(rate);
    let start = project.timeline.to_samples(a, sr);
    let end =
        project.timeline.to_samples(b, sr) + (settings.tail_seconds.max(0.0) as f64 * sr) as i64;
    let frames = (end - start).max(0) as usize;
    if frames == 0 {
        return Err(RenderError::EmptyRange);
    }
    let progress = Arc::new(RenderProgress::default());
    // Rendering, then coding.
    progress.total.store(2 * frames as u64, Ordering::Relaxed);
    let p = Arc::clone(&progress);
    let path = settings.output.clone();
    let label = project.name.clone();
    RenderJob::spawn(progress, move || {
        let mut sources = faderframe_engine::render_generated_sources(&project, rate);
        for (_, file, e) in crate::media::open_file_sources(&project, None, &mut sources) {
            tracing::warn!("render: {}: {e}", file.display());
        }
        let outputs = plan.outputs.max(2);
        let rendered = crate::render::render_one(
            &project, rate, &sources, start, frames, &p, outputs, None, false,
        )?;
        let silence = vec![0.0f32; frames];
        let mut audio: Vec<Vec<f32>> = plan
            .channels
            .iter()
            .map(|c| {
                c.and_then(|c| rendered.get(c))
                    .map_or_else(|| silence.clone(), Clone::clone)
            })
            .collect();
        drop(rendered);
        let finished = finish(&mut audio, &settings, plan.layout, rate);
        if p.cancel.load(Ordering::Relaxed) {
            return Err(RenderError::Cancelled);
        }
        let refs: Vec<&[f32]> = audio.iter().map(Vec::as_slice).collect();
        let mut master = Master {
            layout: plan.layout,
            codec,
            sample_rate: rate,
            pre_skip: 0,
            label,
            loudness: measure(plan.layout, &refs, rate),
        };
        let done = p.done.load(Ordering::Relaxed);
        faderframe_iamf::write_file(&path, &mut master, &refs, Container::for_path(&path), |n| {
            p.done.store(done + n, Ordering::Relaxed)
        })
        .map_err(|e| match e {
            faderframe_iamf::IamfError::Io(source) => RenderError::Io {
                path: path.clone(),
                source,
            },
            other => RenderError::Iamf(other.to_string()),
        })?;
        Ok(vec![Rendered {
            path: path.clone(),
            finished,
        }])
    })
}

/// The render's finish for an IAMF master: a loudness target reached on
/// the stereo down-mix (all channels moved alike), the ceiling kept on
/// every channel; the report on the stereo down-mix.
fn finish(
    audio: &mut [Vec<f32>],
    settings: &RenderSettings,
    layout: Layout,
    rate: u32,
) -> Option<Finished> {
    use faderframe_analysis::delivery::{
        LOOKAHEAD_MS, RELEASE_MS, apply_gain, limit_true_peak, measure,
    };
    let stereo = |audio: &[Vec<f32>]| {
        let refs: Vec<&[f32]> = audio.iter().map(Vec::as_slice).collect();
        let [l, r] = faderframe_iamf::stereo_downmix(layout, &refs);
        vec![l, r]
    };
    let mut gain_db = 0.0;
    let mut limited_db = 0.0;
    if let Some(target) = settings.finish.loudness {
        let before = measure(&stereo(audio), rate);
        if before.integrated.is_finite() {
            gain_db = f64::from(target) - before.integrated;
            apply_gain(audio, gain_db);
        }
    } else if let Some(target) = settings.normalize_db {
        let peak = audio.iter().flatten().fold(0.0f32, |m, s| m.max(s.abs()));
        if peak > 1e-9 {
            gain_db = f64::from(target) - 20.0 * f64::from(peak).log10();
            apply_gain(audio, gain_db);
        }
    }
    if let Some(ceiling) = settings.finish.ceiling {
        limited_db = limit_true_peak(audio, rate, f64::from(ceiling), LOOKAHEAD_MS, RELEASE_MS);
    }
    settings.report.then(|| Finished {
        gain_db,
        limited_db,
        report: measure(&stereo(audio), rate),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_master(layout: ChannelLayout) -> Project {
        let mut p = Project::new("P", 48_000);
        let m = p.master_id().unwrap();
        p.track_mut(m).unwrap().layout = layout;
        p
    }

    #[test]
    fn every_master_format_finds_its_layout() {
        let pl = plan(&with_master(ChannelLayout::Surround(SurroundFormat::S714)));
        assert_eq!(pl.layout, Layout::S714);
        // IAMF: L R Lss Rss Lrs Rrs Ltf Rtf Ltb Rtb C LFE; ours (WAVE
        // order): L R C LFE Lrs Rrs Lss Rss Ltf Rtf Ltr Rtr.
        assert_eq!(
            pl.channels,
            [0, 1, 6, 7, 4, 5, 8, 9, 10, 11, 2, 3].map(Some).to_vec()
        );
        let quad = plan(&with_master(ChannelLayout::Surround(SurroundFormat::Quad)));
        assert_eq!(quad.layout, Layout::S71);
        // Its rears are 7.1's rears; sides, centre and LFE silent.
        assert_eq!(
            quad.channels,
            [Some(0), Some(1), None, None, Some(2), Some(3), None, None]
        );
        let s512 = plan(&with_master(ChannelLayout::Surround(SurroundFormat::S512)));
        assert_eq!(s512.layout, Layout::S512);
        assert_eq!(
            s512.channels[4..6],
            [Some(6), Some(7)],
            "top middles as the top pair"
        );
        assert_eq!(plan(&with_master(ChannelLayout::Mono)).channels, [Some(0)]);
        assert_eq!(
            plan(&with_master(ChannelLayout::Stereo)).layout,
            Layout::Stereo
        );
    }
}
