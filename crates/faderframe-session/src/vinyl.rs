//! The album on vinyl: how its songs fall onto sides, what a lathe will
//! make of them, and the cutting sheet that goes with the premaster.
//!
//! Sides keep the album's order. Split automatically, the album takes as
//! few sides as fit the format's recommended side length (an odd count
//! gets the record's other side too: it costs nothing and shortens the
//! sides), as evenly as the songs allow (the longest side as short as
//! can be). Split by hand, a side starts at every song marked
//! [`Song::side_break`]. A side's time is its songs with the pauses (or
//! less the crossfades) between them; a crossfade into the first song of
//! a side is dropped, the record has to be turned over there.

use crate::album::SongAnalysis;
use faderframe_analysis::delivery::LoudnessReport;
use faderframe_analysis::vinyl::{LOW_SPLIT_HZ, VinylReport};
use faderframe_core::SongId;
use faderframe_project::album::{Album, Song, VinylFormat, VinylSettings};
use std::ops::Range;

/// One side of the record.
#[derive(Clone, Debug, PartialEq)]
pub struct Side {
    /// The songs on it (album indexes).
    pub songs: Range<usize>,
    /// Its playing time (seconds).
    pub seconds: f64,
}

/// A side's letter: A, B, … Z, then AA, AB, ….
pub fn side_name(i: usize) -> String {
    let letter = |k: usize| char::from(b'A' + (k % 26) as u8);
    if i < 26 {
        letter(i).to_string()
    } else {
        format!("{}{}", letter(i / 26 - 1), letter(i))
    }
}

/// A song's place on the record: "A1", "B3".
pub fn track_label(side: usize, k: usize) -> String {
    format!("{}{}", side_name(side), k + 1)
}

/// What a song adds after the one before it on the same side: its pause,
/// or less its crossfade.
fn gap(song: &Song) -> f64 {
    if song.crossfade > 0.0 {
        -f64::from(song.crossfade)
    } else {
        f64::from(song.pause.max(0.0))
    }
}

/// The playing time of songs `range`.
fn span(songs: &[Song], lengths: &[f64], range: Range<usize>) -> f64 {
    let start = range.start;
    range
        .map(|i| lengths[i] + if i > start { gap(&songs[i]) } else { 0.0 })
        .sum::<f64>()
        .max(0.0)
}

/// The sides the album's songs fall onto (`lengths`: each song's seconds).
pub fn plan_sides(songs: &[Song], lengths: &[f64], settings: &VinylSettings) -> Vec<Side> {
    let n = songs.len().min(lengths.len());
    if n == 0 {
        return Vec::new();
    }
    let side = |r: Range<usize>| Side {
        seconds: span(songs, lengths, r.clone()),
        songs: r,
    };
    if !settings.auto_sides {
        let mut starts: Vec<usize> = std::iter::once(0)
            .chain((1..n).filter(|&i| songs[i].side_break))
            .collect();
        starts.push(n);
        return starts.windows(2).map(|w| side(w[0]..w[1])).collect();
    }
    // best[k][i]: the shortest longest side for the first i songs on k
    // sides; cut[k][i]: where the last of those sides starts.
    let mut best = vec![vec![f64::INFINITY; n + 1]; n + 1];
    let mut cut = vec![vec![0usize; n + 1]; n + 1];
    best[0][0] = 0.0;
    for k in 1..=n {
        for i in k..=n {
            for j in (k - 1)..i {
                let v = best[k - 1][j].max(span(songs, lengths, j..i));
                if v < best[k][i] {
                    best[k][i] = v;
                    cut[k][i] = j;
                }
            }
        }
    }
    let (recommended, maximum) = settings.format.side_seconds();
    let fewest = |limit: f64| (1..=n).find(|&k| best[k][n] <= limit + 1e-9);
    let mut k = fewest(recommended).or_else(|| fewest(maximum)).unwrap_or(n);
    if k % 2 == 1 && k < n {
        k += 1;
    }
    let mut ends = vec![n];
    let mut i = n;
    for kk in (1..=k).rev() {
        i = cut[kk][i];
        ends.push(i);
    }
    ends.reverse();
    ends.windows(2).map(|w| side(w[0]..w[1])).collect()
}

/// The side each song is on, and its place there.
pub fn placement(sides: &[Side], song: usize) -> Option<(usize, usize)> {
    sides
        .iter()
        .position(|s| s.songs.contains(&song))
        .map(|s| (s, song - sides[s].songs.start))
}

/// How much a check matters.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// Worth knowing.
    Note,
    /// The cut will suffer (lower level, less bass, some distortion).
    Warning,
    /// It cannot be cut as it is.
    Problem,
}

/// One finding about the album on vinyl.
#[derive(Clone, Debug, PartialEq)]
pub struct VinylNote {
    pub severity: Severity,
    /// The song it is about (`None`: a side or the record).
    pub song: Option<SongId>,
    /// The side it is about.
    pub side: Option<usize>,
    pub text: String,
}

/// A song as the checks see it.
#[derive(Clone, Copy, Debug)]
pub struct SongFacts<'a> {
    pub loudness: &'a LoudnessReport,
    pub vinyl: &'a VinylReport,
    /// The premaster's gain for it (dB).
    pub gain: f64,
}

/// Over this in the premaster, esses and highs distort (dBFS).
const SIBILANCE_PROBLEM: f64 = -6.0;
const SIBILANCE_WARNING: f64 = -12.0;
/// Side energy this close to the mid's below 150 Hz is wide bass (dB).
const WIDE_BASS: f64 = -12.0;
/// Left and right pulling this much against each other in the bass.
const OPPOSED_BASS: f64 = -0.2;
/// A peak-to-loudness ratio under this: limited hard (LU).
const HARD_LIMITED: f64 = 8.0;
/// How much brighter or louder than the rest of its side the last song
/// must be to be worth a note (dB / LU).
const INNER_MARGIN: f64 = 3.0;

/// m:ss.
pub fn clock(seconds: f64) -> String {
    let s = seconds.max(0.0).round() as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

fn level(db: f64) -> String {
    format!("{db:.1}").replace('-', "−")
}

/// What a lathe will make of the album: side lengths, then each song's
/// bass, highs and limiting, then the inner grooves.
pub fn check(
    album: &Album,
    format: VinylFormat,
    sides: &[Side],
    facts: &[Option<SongFacts<'_>>],
) -> Vec<VinylNote> {
    let mut notes = Vec::new();
    let (recommended, maximum) = format.side_seconds();
    for (k, side) in sides.iter().enumerate() {
        let name = side_name(k);
        let note = |severity, text: String| VinylNote {
            severity,
            song: None,
            side: Some(k),
            text,
        };
        if side.seconds > maximum {
            notes.push(note(
                Severity::Problem,
                format!(
                    "Side {name} plays {} — longer than a {} can be cut ({}). Split it or add sides.",
                    clock(side.seconds),
                    format.name(),
                    clock(maximum)
                ),
            ));
        } else if side.seconds > recommended {
            notes.push(note(
                Severity::Warning,
                format!(
                    "Side {name} plays {} — over the {} a {} takes at full level: it will be cut quieter, with less bass.",
                    clock(side.seconds),
                    clock(recommended),
                    format.name()
                ),
            ));
        }
    }
    for (i, song) in album.songs.iter().enumerate() {
        let Some(f) = facts.get(i).copied().flatten() else {
            continue;
        };
        let title = &song.title;
        let side = placement(sides, i).map(|p| p.0);
        let mut push = |severity, text: String| {
            notes.push(VinylNote {
                severity,
                song: Some(song.id),
                side,
                text,
            });
        };
        let v = f.vinyl;
        if v.low_correlation < OPPOSED_BASS {
            push(
                Severity::Problem,
                format!(
                    "“{title}”: the bass below {LOW_SPLIT_HZ:.0} Hz is out of phase (correlation {}). A lathe cuts that vertically and cannot hold it at full level — make the lows mono (Utility: Vinyl Mono Bass).",
                    level(v.low_correlation)
                ),
            );
        } else if v.low_side_db > WIDE_BASS {
            push(
                Severity::Warning,
                format!(
                    "“{title}”: wide bass below {LOW_SPLIT_HZ:.0} Hz (the side only {} dB under the mid). Make the lows mono (Utility: Vinyl Mono Bass) or the cutting engineer will.",
                    level(-v.low_side_db)
                ),
            );
        }
        let sibilance = v.sibilance_db + f.gain;
        if sibilance > SIBILANCE_PROBLEM {
            push(
                Severity::Problem,
                format!(
                    "“{title}”: esses and highs peak at {} dBFS in the premaster — they will distort on vinyl. De-ess (De-esser) or soften the highs.",
                    level(sibilance)
                ),
            );
        } else if sibilance > SIBILANCE_WARNING {
            push(
                Severity::Warning,
                format!(
                    "“{title}”: strong esses and highs (peaking at {} dBFS in the premaster) may distort, most near the inner groove. Consider de-essing.",
                    level(sibilance)
                ),
            );
        }
        let plr = f.loudness.plr();
        if plr.is_finite() && plr < HARD_LIMITED {
            push(
                Severity::Warning,
                format!(
                    "“{title}” is limited hard (peak to loudness {} LU). A lathe needs no brickwall limiting: if there is an unlimited master, use that.",
                    level(plr)
                ),
            );
        }
    }
    // The last song of a side plays at the inner groove, where the groove
    // runs slowest: highs and loud passages suffer first there.
    for (k, side) in sides.iter().enumerate() {
        if side.songs.len() < 2 {
            continue;
        }
        let last = side.songs.end - 1;
        let Some(f) = facts.get(last).copied().flatten() else {
            continue;
        };
        let others: Vec<SongFacts<'_>> = side
            .songs
            .clone()
            .filter(|&i| i != last)
            .filter_map(|i| facts.get(i).copied().flatten())
            .collect();
        if others.is_empty() {
            continue;
        }
        let max = |g: &dyn Fn(&SongFacts<'_>) -> f64| {
            others
                .iter()
                .map(g)
                .filter(|v| v.is_finite())
                .fold(f64::NEG_INFINITY, f64::max)
        };
        let bright = |x: &SongFacts<'_>| x.vinyl.highs_db;
        let loud = |x: &SongFacts<'_>| x.loudness.integrated + x.gain;
        let brighter = bright(&f) - max(&bright) >= INNER_MARGIN;
        let louder = loud(&f) - max(&loud) >= INNER_MARGIN;
        if brighter || louder {
            let song = &album.songs[last];
            let what = match (brighter, louder) {
                (true, true) => "brightest and loudest",
                (true, false) => "brightest",
                _ => "loudest",
            };
            notes.push(VinylNote {
                severity: Severity::Note,
                song: Some(song.id),
                side: Some(k),
                text: format!(
                    "“{}” ends side {} at the inner groove and is the side's {what} song. The inner groove loses highs and distorts first — a calmer song sits better there.",
                    song.title,
                    side_name(k)
                ),
            });
        }
    }
    notes.sort_by_key(|n| std::cmp::Reverse(n.severity));
    notes
}

/// The premaster's gain per song: the digital release's balance between
/// the songs, brought (as one) to the vinyl peak without limiting; or,
/// limiting, the digital gains themselves.
pub fn premaster_gains(settings: &VinylSettings, peaks: &[f64], digital: &[f64]) -> Vec<f64> {
    if settings.limit {
        return digital.to_vec();
    }
    let highest = peaks
        .iter()
        .zip(digital)
        .map(|(p, g)| p + g)
        .filter(|v| v.is_finite())
        .fold(f64::NEG_INFINITY, f64::max);
    let shift = if highest.is_finite() {
        f64::from(settings.peak) - highest
    } else {
        0.0
    };
    digital.iter().map(|g| g + shift).collect()
}

/// A song on the cutting sheet.
#[derive(Clone, Debug, PartialEq)]
pub struct SheetSong {
    pub title: String,
    pub isrc: String,
    /// Where it starts on its side and how long it plays (seconds).
    pub start: f64,
    pub length: f64,
    /// In the premaster: true peak (dBTP) and loudness (LUFS).
    pub true_peak: f64,
    pub loudness: f64,
    /// Its own file (with track files).
    pub file: Option<String>,
}

/// A side on the cutting sheet.
#[derive(Clone, Debug, PartialEq)]
pub struct SheetSide {
    pub seconds: f64,
    pub file: String,
    pub songs: Vec<SheetSong>,
}

/// The cutting sheet: what the cutting engineer needs to know, side by
/// side, as plain text.
pub fn cutting_sheet(
    album: &Album,
    project: &str,
    rate: u32,
    sides: &[SheetSide],
    notes: &[VinylNote],
) -> String {
    let v = &album.settings.vinyl;
    let info = &album.info;
    let title = if info.title.trim().is_empty() {
        project
    } else {
        info.title.trim()
    };
    let mut out = String::new();
    let mut line = |s: String| {
        out.push_str(&s);
        out.push('\n');
    };
    line("VINYL CUTTING SHEET".into());
    line(String::new());
    line(format!("Title       {title}"));
    if !info.credits.performer.trim().is_empty() {
        line(format!("Artist      {}", info.credits.performer.trim()));
    }
    if !info.upc.trim().is_empty() {
        line(format!("UPC/EAN     {}", info.upc.trim()));
    }
    let (recommended, maximum) = v.format.side_seconds();
    line(format!(
        "Format      {} — {} side{} on {} record{}",
        v.format.name(),
        sides.len(),
        if sides.len() == 1 { "" } else { "s" },
        sides.len().div_ceil(2),
        if sides.len() > 2 { "s" } else { "" }
    ));
    line(format!(
        "Side time   {} recommended, {} at most",
        clock(recommended),
        clock(maximum)
    ));
    line(format!(
        "Premaster   24-bit WAV, {} Hz, one continuous file per side; peaks at {} dBTP{}",
        rate,
        level(f64::from(v.peak)),
        if v.limit {
            ", limited"
        } else {
            ", gain only (no limiting)"
        }
    ));
    line("Gaps        the silence between songs is in the side files (cut as bands)".into());
    for (k, side) in sides.iter().enumerate() {
        line(String::new());
        let over = if side.seconds > maximum {
            "  — TOO LONG TO CUT"
        } else if side.seconds > recommended {
            "  — over the recommended length"
        } else {
            ""
        };
        line(format!(
            "SIDE {}   {}{over}   file: {}",
            side_name(k),
            clock(side.seconds),
            side.file
        ));
        line(format!(
            "  {:<4} {:>6} {:>7}  {:<36} {:<13} {:>9} {:>10}",
            "#", "Start", "Length", "Title", "ISRC", "Peak", "Loudness"
        ));
        for (j, s) in side.songs.iter().enumerate() {
            let title: String = s.title.chars().take(36).collect();
            line(format!(
                "  {:<4} {:>6} {:>7}  {:<36} {:<13} {:>9} {:>10}",
                track_label(k, j),
                clock(s.start),
                clock(s.length),
                title,
                if s.isrc.is_empty() { "—" } else { &s.isrc },
                format!("{} dBTP", level(s.true_peak)),
                format!("{} LUFS", level(s.loudness)),
            ));
            if let Some(f) = &s.file {
                line(format!("       file: {f}"));
            }
        }
    }
    line(String::new());
    if notes.is_empty() {
        line("NOTES       none: the checks found nothing to worry about".into());
    } else {
        line("NOTES".into());
        for n in notes {
            let mark = match n.severity {
                Severity::Problem => "PROBLEM",
                Severity::Warning => "WARNING",
                Severity::Note => "NOTE",
            };
            line(format!("  {mark:<8} {}", n.text));
        }
    }
    out
}

/// The checks' view of an analysed song.
pub(crate) fn facts<'a>(a: &'a SongAnalysis, gain: f64) -> SongFacts<'a> {
    SongFacts {
        loudness: &a.report,
        vinyl: &a.vinyl,
        gain,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use faderframe_project::album::SongSource;

    fn songs(n: usize) -> Vec<Song> {
        (0..n)
            .map(|i| {
                Song::new(
                    SongId(i as u64 + 1),
                    format!("S{i}"),
                    SongSource::ThisProject,
                )
            })
            .collect()
    }

    fn auto(format: VinylFormat) -> VinylSettings {
        VinylSettings {
            enabled: true,
            format,
            ..VinylSettings::default()
        }
    }

    #[test]
    fn sides_are_as_few_and_as_even_as_the_format_allows() {
        // Ten four-minute songs with 2 s pauses: 40 min — two sides of
        // 20:08 are over 18:00 recommended, so four sides of 3, 3, 2, 2.
        let s = songs(10);
        let lengths = vec![240.0; 10];
        let sides = plan_sides(&s, &lengths, &auto(VinylFormat::Lp12At33));
        let counts: Vec<usize> = sides.iter().map(|x| x.songs.len()).collect();
        assert_eq!(sides.len(), 4, "{counts:?}");
        assert_eq!(counts.iter().sum::<usize>(), 10);
        assert!(sides.iter().all(|x| x.seconds <= 18.0 * 60.0));
        assert_eq!(sides[0].songs.start, 0);
        for side in &sides {
            let k = side.songs.len() as f64;
            assert!((2.0..=3.0).contains(&k), "{sides:?}");
            assert!((side.seconds - (k * 240.0 + (k - 1.0) * 2.0)).abs() < 1e-9);
        }
        // Eight songs, 32 min: two even sides of four.
        let sides = plan_sides(&s[..8], &lengths[..8], &auto(VinylFormat::Lp12At33));
        assert_eq!(sides.len(), 2);
        assert_eq!(sides[0].songs, 0..4);
        assert!((sides[0].seconds - sides[1].seconds).abs() < 1e-9);
        // Three short songs fit one side, but the record has two.
        let sides = plan_sides(&s[..3], &[120.0; 3], &auto(VinylFormat::Lp12At33));
        assert_eq!(sides.len(), 2);
        // Uneven songs: the longest side as short as can be.
        let lengths = [600.0, 60.0, 60.0, 60.0, 600.0];
        let sides = plan_sides(&s[..5], &lengths, &auto(VinylFormat::Lp12At33));
        assert_eq!(sides.len(), 2);
        let longest = sides.iter().map(|x| x.seconds).fold(0.0, f64::max);
        assert!(longest < 790.0, "{sides:?}");
        // A single on a 7″: a side each.
        let sides = plan_sides(&s[..2], &[200.0, 190.0], &auto(VinylFormat::Single7At45));
        assert_eq!(sides.len(), 2);
        // Crossfades shorten a side; one into a new side is dropped.
        let mut s = songs(2);
        s[1].crossfade = 5.0;
        let sides = plan_sides(
            &s,
            &[100.0, 100.0],
            &VinylSettings {
                auto_sides: false,
                ..auto(VinylFormat::Lp12At33)
            },
        );
        assert_eq!(sides.len(), 1);
        assert!((sides[0].seconds - 195.0).abs() < 1e-9);
        s[1].side_break = true;
        let sides = plan_sides(
            &s,
            &[100.0, 100.0],
            &VinylSettings {
                auto_sides: false,
                ..auto(VinylFormat::Lp12At33)
            },
        );
        assert_eq!(sides.len(), 2);
        assert_eq!(sides[1].seconds, 100.0);
        assert_eq!(track_label(1, 0), "B1");
        assert_eq!(side_name(27), "AB");
        assert_eq!(placement(&sides, 1), Some((1, 0)));
    }

    fn report(integrated: f64, true_peak: f64) -> LoudnessReport {
        LoudnessReport {
            integrated,
            range: 6.0,
            true_peak,
            sample_peak: true_peak,
            max_short_term: integrated + 3.0,
        }
    }

    fn vinyl(
        low_side_db: f64,
        low_correlation: f64,
        sibilance_db: f64,
        highs_db: f64,
    ) -> VinylReport {
        VinylReport {
            low_side_db,
            low_correlation,
            sibilance_db,
            highs_db,
        }
    }

    #[test]
    fn the_checks_find_what_a_lathe_cannot_cut() {
        let album = Album {
            songs: songs(4),
            ..Album::default()
        };
        let clean = vinyl(-30.0, 0.9, -24.0, -20.0);
        let reports = [
            report(-14.0, -1.0),
            report(-8.0, -1.0),
            report(-14.0, -1.0),
            report(-14.0, -1.0),
        ];
        let vinyls = [
            vinyl(-8.0, 0.5, -24.0, -20.0),  // wide bass
            vinyl(-30.0, -0.6, -4.0, -20.0), // opposed bass, harsh esses
            clean,
            vinyl(-30.0, 0.9, -24.0, -12.0), // bright, last on its side
        ];
        let facts: Vec<Option<SongFacts<'_>>> = reports
            .iter()
            .zip(&vinyls)
            .map(|(r, v)| {
                Some(SongFacts {
                    loudness: r,
                    vinyl: v,
                    gain: -1.0,
                })
            })
            .collect();
        let sides = vec![
            Side {
                songs: 0..2,
                seconds: 25.0 * 60.0,
            },
            Side {
                songs: 2..4,
                seconds: 19.0 * 60.0,
            },
        ];
        let notes = check(&album, VinylFormat::Lp12At33, &sides, &facts);
        let find = |needle: &str| notes.iter().find(|n| n.text.contains(needle));
        assert_eq!(find("Side A plays").unwrap().severity, Severity::Problem);
        assert_eq!(find("Side B plays").unwrap().severity, Severity::Warning);
        assert_eq!(find("“S0”: wide bass").unwrap().severity, Severity::Warning);
        assert_eq!(find("out of phase").unwrap().song, Some(SongId(2)));
        assert_eq!(find("“S1”: esses").unwrap().severity, Severity::Problem);
        assert!(find("“S1” is limited hard").is_some());
        let inner = find("“S3” ends side B at the inner groove").unwrap();
        assert_eq!((inner.song, inner.side), (Some(SongId(4)), Some(1)));
        assert!(inner.text.contains("brightest song"), "{}", inner.text);
        // S1 ends side A and is 6 LU louder than S0.
        let loud = find("“S1” ends side A").unwrap();
        assert!(loud.text.contains("loudest song"), "{}", loud.text);
        // Worst first.
        assert_eq!(notes[0].severity, Severity::Problem);
        assert!(notes.windows(2).all(|w| w[0].severity >= w[1].severity));
        // A clean album on short sides: nothing.
        let facts: Vec<Option<SongFacts<'_>>> = (0..4)
            .map(|_| {
                Some(SongFacts {
                    loudness: &reports[0],
                    vinyl: &clean,
                    gain: 0.0,
                })
            })
            .collect();
        let short = vec![
            Side {
                songs: 0..2,
                seconds: 600.0,
            },
            Side {
                songs: 2..4,
                seconds: 600.0,
            },
        ];
        assert!(check(&album, VinylFormat::Lp12At33, &short, &facts).is_empty());
    }

    #[test]
    fn the_premaster_keeps_the_balance_and_reaches_the_peak() {
        let s = VinylSettings::default();
        // The loudest peak after the digital gains lands on −3 dBTP.
        let g = premaster_gains(&s, &[-1.0, -6.0], &[-2.0, 1.0]);
        assert!(
            (g[0] - -2.0).abs() < 1e-9 && (g[1] - 1.0).abs() < 1e-9,
            "{g:?}"
        );
        let g = premaster_gains(&s, &[-10.0, -12.0], &[0.0, 0.0]);
        assert!((g[0] - 7.0).abs() < 1e-9 && (g[1] - 7.0).abs() < 1e-9);
        let limited = VinylSettings { limit: true, ..s };
        assert_eq!(premaster_gains(&limited, &[0.0], &[4.0]), vec![4.0]);
    }
}
