//! The null test of a stems render, and stems that null.
//!
//! Which stems make up the mix ([`stems_summed`]): the tracks with audio
//! no other track with audio feeds. A bus's, an aux return's or a folder
//! bus's stem (solo in place) holds its sources again, so it is left out
//! of the sum; a MIDI track feeds its instrument no audio.
//!
//! The test renders the mix with the same range and settings (format,
//! dither, level processing; every solo off — the stems were made from
//! the whole mix), writes it beside the stems as they are written and
//! reads both back: the residual of the stems' sum against the mix
//! (`faderframe_analysis::qc::NullMeter`), an offset in time where it does
//! not null (`best_lag` at its worst place), and the causes the project
//! shows ([`causes`]: non-linear devices several stems pass through,
//! sidechains keyed by a bus, level processing per stem, dither) — warned
//! of before the render too. A key is taken before its track's fader, mute
//! and solo: a track playing its own clips keys a stem as it keys the mix;
//! a bus or return (whose sources the solo silences) does not.
//!
//! Stems that null ([`super::StemOptions::shared_stem`]): the mix's level
//! processing is worked out on the mix and its gain applied to every stem
//! alike, and what still differs — the shared processing's doing, the
//! limiting of the finish — is written as one more stem, the mix minus
//! the stems' sum in the format's codes (no dither), so the files sum to
//! the mix as written to the last bit (where the codes fit).

use super::files::StagedFile;
use super::stats::{self, Written};
use super::{RenderError, RenderProgress, RenderSettings};
use faderframe_analysis::qc::{NullMeter, best_lag};
use faderframe_audio_files::Dither;
use faderframe_audio_files::wavstream::{WavFile, WavWriter};
use faderframe_core::TrackId;
use faderframe_project::{PluginFormat, Project, TrackKind};
use faderframe_qc::null::{Cause, DitherKind, LeftOut, NullTest, Precision};
use faderframe_qc::{QcFile, Role, Spec, Verdict};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

/// The nulling stem's name ("<project> - Shared Processing.wav").
pub const SHARED_NAME: &str = "Shared Processing";
/// Frames read at a time.
const CHUNK: usize = 65_536;
/// The offset searched for (frames either way) and the excerpt it is
/// searched in.
const MAX_LAG: usize = 2048;
const EXCERPT: usize = 16_384;

/// Audio routing edges whose source makes audio (a MIDI track playing an
/// instrument track feeds it notes, not audio).
fn audio_edges(project: &Project) -> Vec<(TrackId, TrackId)> {
    let audio: HashSet<TrackId> = project
        .tracks
        .iter()
        .filter(|t| t.kind.has_audio())
        .map(|t| t.id)
        .collect();
    project
        .routing_edges()
        .into_iter()
        .filter(|(a, b)| a != b && audio.contains(a))
        .collect()
}

/// The stems summed to make the mix and those left out with the reason
/// (see the module): `(summed, left out)` in track order.
pub fn stems_summed(project: &Project) -> (Vec<TrackId>, Vec<(TrackId, String)>) {
    let edges = audio_edges(project);
    let mut summed = Vec::new();
    let mut left = Vec::new();
    for t in project
        .tracks
        .iter()
        .filter(|t| t.kind != TrackKind::Master && t.kind.has_audio())
    {
        let feeders: Vec<&str> = edges
            .iter()
            .filter(|(_, b)| *b == t.id)
            .filter_map(|(a, _)| project.track(*a).map(|x| x.name.as_str()))
            .collect();
        if feeders.is_empty() {
            summed.push(t.id);
            continue;
        }
        let what = if t.is_folder() {
            "a folder bus"
        } else if t.kind == TrackKind::Bus {
            "a bus"
        } else {
            "fed by other tracks"
        };
        let names = match feeders.as_slice() {
            [one] => one.to_string(),
            [a, b] => format!("{a} and {b}"),
            more => format!("{} tracks", more.len()),
        };
        left.push((
            t.id,
            format!("{what}: its stem holds what {names} send it again"),
        ));
    }
    (summed, left)
}

/// What a built-in device does that keeps stems from summing to the mix
/// where several pass through it (`None`: it is linear — the same to a
/// sum as to its parts).
pub fn nonlinearity(id: &str) -> Option<&'static str> {
    use faderframe_core::builtin as b;
    Some(match id {
        b::COMPRESSOR => "a compressor",
        b::LIMITER => "a limiter",
        b::GATE => "a gate",
        b::DEESSER => "a de-esser",
        b::CHANNEL_STRIP => "a channel strip (gate, compressor and drive)",
        b::SATURATOR => "a saturator",
        b::COMPRESSOR_76 => "a FET compressor",
        b::LEVELER_2A => "an optical leveler",
        b::TAPE_MACHINE => "a tape machine (saturation, and hiss of its own)",
        b::GUITAR_STATION => "an amplifier model",
        b::PROGRAM_EQ => "a valve equaliser (its make-up stage saturates)",
        b::MULTIBAND_DYNAMICS => "a multiband compressor",
        b::VOCODER => "a vocoder (its bands follow the whole signal's envelope)",
        id if b::preamp_index(id).is_some() => "a modelled preamp",
        id if b::console_bus_index(id).is_some() => "a console bus amplifier",
        _ => return None,
    })
}

/// For every track, how many of `summed` reach it (along the audio
/// routing; a source reaches itself).
fn reach(project: &Project, summed: &[TrackId]) -> HashMap<TrackId, usize> {
    let edges = audio_edges(project);
    let mut count: HashMap<TrackId, usize> = HashMap::new();
    for &s in summed {
        let mut seen = HashSet::new();
        let mut stack = vec![s];
        while let Some(n) = stack.pop() {
            if !seen.insert(n) {
                continue;
            }
            *count.entry(n).or_default() += 1;
            stack.extend(edges.iter().filter(|(a, _)| *a == n).map(|(_, b)| *b));
        }
    }
    count
}

/// Why stems of `project` rendered with `settings` would not sum to the
/// mix, as far as the project shows it (before rendering: the Render
/// window warns of them).
pub fn causes(project: &Project, settings: &RenderSettings) -> Vec<Cause> {
    let (summed, _) = stems_summed(project);
    let reached = reach(project, &summed);
    let edges = audio_edges(project);
    let mut out = Vec::new();
    let master = project.master_id();
    let place = |t: &faderframe_project::Track| {
        if Some(t.id) == master {
            "the master".to_string()
        } else {
            format!("“{}”", t.name)
        }
    };
    for t in &project.tracks {
        let shared = reached.get(&t.id).copied().unwrap_or(0);
        for slot in t.slots().into_iter().filter(|s| !s.bypass) {
            let device = format!("“{}”", t.name_of(slot));
            // The key is taken before its track's fader, mute and solo: a
            // track of its own keys every stem alike, one fed by others
            // (silenced by the solo) does not.
            if let Some(key) = slot
                .sidechain
                .filter(|k| *k != t.id && edges.iter().any(|(_, b)| b == k))
                && shared > 0
            {
                let key = project
                    .track(key)
                    .map_or_else(|| "another track".into(), |k| format!("“{}”", k.name));
                out.push(Cause::sidechain(&device, &place(t), &key));
                continue;
            }
            if shared < 2 {
                continue;
            }
            match slot.plugin.format {
                PluginFormat::Builtin => {
                    if let Some(what) = nonlinearity(&slot.plugin.id) {
                        out.push(Cause::non_linear(&device, what, &place(t), shared));
                    }
                }
                _ => out.push(Cause::maybe_non_linear(&device, &place(t), shared)),
            }
        }
    }
    if !settings.stems.shared_stem {
        if !settings.finish.is_none() {
            out.push(Cause::level_per_stem(
                "brought to the loudness target and limited",
            ));
        } else if settings.normalize_db.is_some() {
            out.push(Cause::level_per_stem("normalised"));
        }
    }
    for t in &project.tracks {
        if t.kind.has_audio()
            && t.kind != TrackKind::Master
            && edges.iter().any(|(_, b)| *b == t.id)
            && !project.clips_of(t.id).is_empty()
        {
            out.push(Cause::fed_track(&format!("“{}”", t.name)));
        }
    }
    out
}

/// The mix the stems are compared with: the project with every solo off.
pub(crate) fn reference_project(project: &Project) -> Project {
    let mut p = project.clone();
    for t in &mut p.tracks {
        t.solo = false;
    }
    p
}

/// The dither the files carry as the null test reckons it.
fn dither_kind(settings: &RenderSettings) -> DitherKind {
    if !settings.format.is_integer() {
        return DitherKind::Off;
    }
    match settings.dither {
        Dither::Off => DitherKind::Off,
        Dither::Tpdf => DitherKind::Tpdf,
        Dither::Shaped if matches!(settings.sample_rate, 44_100 | 48_000) => DitherKind::Shaped,
        Dither::Shaped => DitherKind::Tpdf,
    }
}

fn name_of(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// What [`compare`] needs to know of the files.
pub(crate) struct Compared<'a> {
    /// The mix as written (read back).
    pub mix: &'a Path,
    /// The stems summed: their names and where to read them.
    pub stems: Vec<(String, PathBuf)>,
    pub left_out: Vec<LeftOut>,
    /// Stems that null: the shared stem is written here.
    pub shared: Option<&'a StagedFile>,
    /// The range's length without the tail (frames).
    pub range_frames: u64,
    /// The master's bed, for the mix's measurement.
    pub bed: Option<faderframe_core::SurroundFormat>,
    /// Said of the mix in the report.
    pub reference: String,
}

/// Read the stems and the mix back and compare them (writing the shared
/// stem when asked); the null test with the project's causes.
pub(crate) fn compare(
    project: &Project,
    settings: &RenderSettings,
    files: Compared<'_>,
    progress: &RenderProgress,
) -> Result<NullTest, RenderError> {
    let io = |path: &Path, source: std::io::Error| RenderError::Io {
        path: path.to_path_buf(),
        source,
    };
    let mix = WavFile::open(files.mix).map_err(|e| io(files.mix, e))?;
    let channels = mix.channels();
    let rate = mix.sample_rate();
    let format = mix.format();
    let stems: Vec<WavFile> = files
        .stems
        .iter()
        .map(|(_, p)| WavFile::open(p).map_err(|e| io(p, e)))
        .collect::<Result<_, _>>()?;
    let frames = stems
        .iter()
        .map(WavFile::frames)
        .chain(std::iter::once(mix.frames()))
        .max()
        .unwrap_or(0);
    let half = (1u64 << (format.bits().clamp(2, 32) - 1)) as f64;
    let mut writer = match files.shared {
        Some(f) => Some(
            WavWriter::create_with_mask(
                f.temp(),
                channels as u16,
                rate,
                format,
                Dither::Off,
                files.bed.map(faderframe_core::SurroundFormat::channel_mask),
            )
            .map_err(|e| io(f.dest(), e))?,
        ),
        None => None,
    };
    let precision = Precision {
        files: stems.len() + usize::from(writer.is_some()) + 1,
        bits: format.is_integer().then(|| format.bits()),
        dither: dither_kind(settings),
    };
    // Where the residual first rises above what rounding and dither
    // leave.
    let (_, floor) = precision.bound();
    let mut meter = NullMeter::new(floor, files.range_frames);
    let mut scratch = Vec::new();
    let mut piece = vec![vec![0.0f32; CHUNK]; channels];
    let mut mix_piece = vec![vec![0.0f32; CHUNK]; channels];
    let mut sum = vec![vec![0.0f64; CHUNK]; channels];
    let mut codes = vec![vec![0i32; CHUNK]; channels];
    let mut diff = vec![vec![0.0f32; CHUNK]; channels];
    let mut at = 0u64;
    progress.null_testing.store(true, Ordering::Relaxed);
    while at < frames {
        if progress.cancel.load(Ordering::Relaxed) {
            return Err(RenderError::Cancelled);
        }
        let n = (frames - at).min(CHUNK as u64) as usize;
        let mut out: Vec<&mut [f32]> = mix_piece.iter_mut().map(|b| &mut b[..n]).collect();
        mix.read(at, &mut out, &mut scratch)
            .map_err(|e| io(files.mix, e))?;
        for s in sum.iter_mut() {
            s[..n].fill(0.0);
        }
        for (stem, (_, path)) in stems.iter().zip(&files.stems) {
            let mut out: Vec<&mut [f32]> = piece.iter_mut().map(|b| &mut b[..n]).collect();
            stem.read(at, &mut out, &mut scratch)
                .map_err(|e| io(path, e))?;
            for (s, p) in sum.iter_mut().zip(&piece) {
                for (a, b) in s[..n].iter_mut().zip(&p[..n]) {
                    *a += f64::from(*b);
                }
            }
        }
        if let Some(w) = &mut writer {
            // What is left of the mix, as the file will hold it: added to
            // the sum, it is the mix.
            for c in 0..channels {
                for i in 0..n {
                    let rest = f64::from(mix_piece[c][i]) - sum[c][i];
                    let held = if format.is_integer() {
                        let code = (rest * half).round().clamp(-half, half - 1.0);
                        codes[c][i] = code as i32;
                        code / half
                    } else {
                        diff[c][i] = rest as f32;
                        f64::from(diff[c][i])
                    };
                    sum[c][i] += held;
                }
            }
            let path = files.shared.map_or(files.mix, StagedFile::dest);
            if format.is_integer() {
                let parts: Vec<&[i32]> = codes.iter().map(|c| &c[..n]).collect();
                w.write_codes(&parts, n).map_err(|e| io(path, e))?;
            } else {
                let parts: Vec<&[f32]> = diff.iter().map(|c| &c[..n]).collect();
                w.write_planar(&parts, n).map_err(|e| io(path, e))?;
            }
        }
        let s: Vec<&[f64]> = sum.iter().map(|c| &c[..n]).collect();
        let m: Vec<&[f32]> = mix_piece.iter().map(|c| &c[..n]).collect();
        meter.add(&s, &m);
        at += n as u64;
        progress.done.fetch_add(n as u64, Ordering::Relaxed);
    }
    if let Some(w) = writer {
        let path = files.shared.map_or(files.mix, StagedFile::dest);
        w.finish().map_err(|e| io(path, e))?;
    }
    let levels = meter.finish();
    let outcome = faderframe_qc::null::outcome(&levels, &precision);
    // Where it does not null: would an offset in time do better?
    let lag = if outcome == faderframe_qc::NullOutcome::Exact || stems.is_empty() {
        None
    } else {
        let start = levels
            .peak_at
            .saturating_sub(EXCERPT as u64 / 2)
            .min(frames.saturating_sub(EXCERPT as u64));
        let c = levels.peak_channel.min(channels.saturating_sub(1));
        let mut excerpt_sum = vec![0.0f64; EXCERPT];
        let mut buf = vec![vec![0.0f32; EXCERPT]; channels];
        for stem in &stems {
            let mut out: Vec<&mut [f32]> = buf.iter_mut().map(|b| &mut b[..]).collect();
            stem.read(start, &mut out, &mut scratch)
                .map_err(|e| io(stem.path(), e))?;
            for (a, b) in excerpt_sum.iter_mut().zip(&buf[c]) {
                *a += f64::from(*b);
            }
        }
        let mut out: Vec<&mut [f32]> = buf.iter_mut().map(|b| &mut b[..]).collect();
        mix.read(start, &mut out, &mut scratch)
            .map_err(|e| io(files.mix, e))?;
        best_lag(&excerpt_sum, &buf[c], MAX_LAG)
    };
    let mut summed: Vec<String> = files.stems.iter().map(|(n, _)| n.clone()).collect();
    let shared_name = files.shared.map(|f| name_of(f.dest()));
    summed.extend(shared_name.clone());
    let mut test = NullTest::new(
        &levels,
        rate,
        precision,
        files.range_frames,
        lag,
        causes(project, settings),
        summed,
        files.left_out,
        files.reference,
    );
    test.shared_stem = shared_name;
    // The mix measured as the programme (checked against the report's
    // spec when one is written).
    let measured = stats::measure(
        files.mix,
        files.mix,
        &Written::Wav { bed: files.bed },
        0.0,
        &progress.cancel,
    );
    test.mix = measured.ok().map(|st| {
        let facts = st.facts();
        let levels = st.qc_levels();
        QcFile {
            path: String::new(),
            name: "The mix (rendered for the test, not delivered)".into(),
            role: Role::Programme,
            sha256: None,
            checks: faderframe_qc::checks(&facts, levels.as_ref(), Spec::none(), Role::Programme),
            facts: Some(facts),
            levels,
            verdict: Verdict::Info,
            notes: st.notes,
        }
    });
    progress.null_testing.store(false, Ordering::Relaxed);
    Ok(test)
}

/// The text the report says of the mix it compared with.
pub(crate) fn reference_text(project: &Project, settings: &RenderSettings) -> String {
    let soloed = project.tracks.iter().any(|t| t.solo);
    format!(
        "the mix of the same range rendered with the same settings ({}, {}{}){}",
        settings.format.label(),
        faderframe_audio::format_sample_rate(settings.sample_rate),
        if settings.format.is_integer() && settings.dither != Dither::Off {
            format!(", {} dither", settings.dither.label())
        } else {
            String::new()
        },
        if soloed {
            "; every solo off, as the stems make up the whole mix"
        } else {
            ""
        }
    )
}

/// A null test of stems written before (`stems`: track and file, the
/// tracks the render made them of), rendering the mix from `project` as
/// the stems were rendered (`settings`): on the calling thread (a job).
pub(crate) fn test_written(
    project: &Project,
    settings: &RenderSettings,
    stems: &[(TrackId, PathBuf)],
    progress: &RenderProgress,
) -> Result<NullTest, RenderError> {
    let mut project = project.clone();
    project.loop_enabled = false;
    let (a, b) = super::resolve_range(&project, settings.range)?;
    let sr = f64::from(settings.sample_rate);
    let start = project.timeline.to_samples(a, sr);
    let range_frames = (project.timeline.to_samples(b, sr) - start).max(0) as u64;
    let frames = range_frames as usize + (settings.tail_seconds.max(0.0) as f64 * sr) as usize;
    let (summed, left) = stems_summed(&project);
    let by_track: HashMap<TrackId, &PathBuf> = stems.iter().map(|(t, p)| (*t, p)).collect();
    let mut chosen = Vec::new();
    let mut left_out: Vec<LeftOut> = Vec::new();
    for t in &summed {
        match by_track.get(t) {
            Some(p) if p.exists() => chosen.push((name_of(p), (*p).clone())),
            Some(p) => left_out.push(LeftOut {
                name: name_of(p),
                why: "the file is gone".into(),
            }),
            None => {}
        }
    }
    for (t, why) in left {
        if let Some(p) = by_track.get(&t) {
            left_out.push(LeftOut {
                name: name_of(p),
                why,
            });
        }
    }
    let dir = stems
        .first()
        .and_then(|(_, p)| p.parent())
        .map(Path::to_path_buf)
        .unwrap_or_else(std::env::temp_dir);
    let mut sources = faderframe_engine::render_generated_sources(&project, settings.sample_rate);
    for (_, path, e) in crate::media::open_file_sources(&project, None, &mut sources) {
        tracing::warn!("null test: {}: {e}", path.display());
    }
    let bed = (settings.channels == super::RenderChannels::Master)
        .then(|| super::master_bed(&project))
        .flatten();
    let outputs = bed.map_or(2, |f| f.channels());
    let binaural = match settings.channels {
        super::RenderChannels::Binaural(room) => Some((room, settings.head.as_ref())),
        _ => None,
    };
    progress.total.store(2 * frames as u64, Ordering::Relaxed);
    let reference = reference_project(&project);
    let audio = super::render_one(
        &reference,
        settings.sample_rate,
        &sources,
        start,
        frames,
        progress,
        outputs,
        binaural,
        settings.keep_latency,
    )?;
    // Written as a stem is, beside them; never moved into place (gone
    // when it is dropped).
    let file = StagedFile::new(&dir.join("Null test mix.wav"))?;
    super::finish(
        audio,
        settings,
        &file,
        bed.map(faderframe_core::SurroundFormat::channel_mask),
    )?;
    compare(
        &project,
        settings,
        Compared {
            mix: file.temp(),
            stems: chosen,
            left_out,
            shared: None,
            range_frames,
            bed,
            reference: reference_text(&project, settings),
        },
        progress,
    )
}
