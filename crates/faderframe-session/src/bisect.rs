//! Audio Bisect: a guided search for the edit that made the mix worse.
//!
//! The user marks a state of the undo history that sounded good and one
//! that sounds bad (by default the project now). Between them lies a path
//! through the history tree (up to their common ancestor, then down: the
//! bad state may be on another branch); every move along it is a step
//! done or undone, and one of them did the damage. As `git bisect` does
//! with commits, the search halves the path until one step is left:
//!
//! * **A round** plays the state in the middle of what is still in
//!   question (the *candidate*) against the last state judged good, blind:
//!   which one is "A" and which "B" is drawn at random, nothing on screen
//!   tells, and both are brought to the same loudness unless the user
//!   turns that off (the quieter one's: nothing gets louder). The user
//!   answers "A is better", "B is better" or "Can't tell".
//! * **The verdict.** The candidate better (or as good) → it is good, the
//!   damage is after it; the other one better → the candidate is bad, the
//!   damage is at or before it. "Can't tell" against the good state asks
//!   again against the first bad one (a candidate that sounds like the
//!   good state is told apart from the bad one, and the other way round);
//!   "Can't tell" there too skips the candidate (as `git bisect skip`):
//!   the next one is the nearest to the middle not skipped. When no
//!   candidate is left the step is found — or, with skipped ones in the
//!   way, the few steps it is among.
//! * **The step found** shows its label and when it was made, and offers:
//!   hear it (the states just before and after it, A/B, named), take just
//!   that step back (its inverse as a new step, as Mix Intelligence's Undo
//!   does when it is not the last; a step undone on the way is brought
//!   back the same way), or go there.
//!
//! The states are rendered as an audition of the history is
//! ([`crate::compare`]): made on a copy of the project
//! ([`faderframe_project::History::project_at_node`]), rendered offline
//! over [`Session::audition_range`] with the run-up, each once per search
//! (a state rendered for one round serves the next). A round's two states
//! are joined into one file — A, then B, each at its fairness gain — that
//! plays through the compare source in the monitor path with `Follow`
//! sync: switching A ⇄ B is a crossfade between two regions of one file,
//! in step with the transport, and the disk loader keeps the side not
//! heard resident too. The project and the history are never touched by
//! the search itself: cancelling leaves everything as it was (a reference
//! that was being compared is chosen again).

use crate::compare::{self, CompareOp, Comparing, StateFile, StateRender, Stretch, render_state};
use crate::render::RenderJob;
use crate::{NoticeLevel, Result, Session, SessionError};
use faderframe_audio_files::{StreamSource, WavFormat};
use faderframe_engine::nodes::{CompareSettings, CompareSource, CompareSync};
use faderframe_project::History;
use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime};

/// One side of a comparison.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BisectSide {
    A,
    B,
}

impl BisectSide {
    pub fn other(self) -> Self {
        match self {
            Self::A => Self::B,
            Self::B => Self::A,
        }
    }

    /// Its name: "A"/"B" in a blind round, "Before"/"After" when the step
    /// found is heard.
    pub fn label(self, blind: bool) -> &'static str {
        match (self, blind) {
            (Self::A, true) => "A",
            (Self::B, true) => "B",
            (Self::A, false) => "Before",
            (Self::B, false) => "After",
        }
    }
}

/// The user's answer to a round.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BisectAnswer {
    /// A sounds better.
    A,
    /// B sounds better.
    B,
    /// No telling which is better.
    CantTell,
}

/// What a round decided about its candidate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BisectVerdict {
    /// Good: the damage comes after it.
    Good,
    /// Bad: the damage is in it already.
    Bad,
    /// Asked again, against the first bad state.
    Undecided,
    /// Not to be told: skipped.
    Skipped,
}

/// Audio Bisect's actions.
#[derive(Clone, Debug, PartialEq)]
pub enum BisectOp {
    /// Get ready: the user marks the good state (and another bad one than
    /// the project now) in the History view.
    Setup,
    /// Mark a step good or bad (getting ready).
    Mark { node: u64, good: bool },
    /// Search between the good state and the bad one (`None`: the project
    /// now).
    Start { good: u64, bad: Option<u64> },
    /// Hear A or B (or, the step found heard, Before or After).
    Hear(BisectSide),
    /// From one side to the other.
    Flip,
    /// Both sides as loud (EBU R128), or each as it is.
    MatchLoudness(bool),
    /// Which one is better.
    Answer(BisectAnswer),
    /// The step found: hear the states just before and after it.
    AuditionFound,
    /// The step found: take just it back (a new step).
    UndoFound,
    /// The step found: go to the state just after it (`false`: just
    /// before it, the last good one).
    GoToFound { after: bool },
    /// End the search; nothing changed.
    Cancel,
}

/// Where the search is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BisectStage {
    /// The good (and bad) state being marked.
    Setup,
    /// Rounds being asked.
    Comparing,
    /// The step (or the few steps) found.
    Found,
}

/// A round answered (shown after the answer: which side was which).
#[derive(Clone, Debug, PartialEq)]
pub struct BisectLog {
    pub round: usize,
    /// The candidate.
    pub node: u64,
    pub label: String,
    /// Compared with the last good state (else the first bad one).
    pub against_good: bool,
    /// The side the candidate played on.
    pub was: BisectSide,
    pub answer: BisectAnswer,
    pub verdict: BisectVerdict,
}

/// A step the damage may be in: a move along the path.
#[derive(Clone, Debug, PartialEq)]
pub struct FoundStep {
    /// The step (a node of the history).
    pub node: u64,
    pub label: String,
    pub made: SystemTime,
    /// The path undoes it (the bad state lacks it) rather than doing it.
    pub undone: bool,
}

/// What the search found.
#[derive(Clone, Debug, PartialEq)]
pub struct BisectFound {
    /// The step (`exact`) or the steps it is among.
    pub steps: Vec<FoundStep>,
    pub exact: bool,
    /// The states either side: the last good one and the first bad one.
    pub before: u64,
    pub after: u64,
    /// Taking the step back: the label of the new step, or why not.
    pub take_back: std::result::Result<String, String>,
}

/// What the views show of Audio Bisect.
#[derive(Clone, Debug, PartialEq)]
pub struct BisectStatus {
    pub stage: BisectStage,
    /// Getting ready: the steps marked (`bad` `None`: the project now).
    /// Searching: the last good state and the first bad one so far.
    pub good: Option<u64>,
    pub bad: Option<u64>,
    /// The whole path, good to bad.
    pub path: Vec<u64>,
    /// The part of it still in question (the last good state to the first
    /// bad one).
    pub range: Vec<u64>,
    /// Candidates skipped ("Can't tell" twice).
    pub skipped: Vec<u64>,
    /// The comparison asked now (1-based), and about how many more.
    pub round: usize,
    pub rounds_left: usize,
    /// This comparison is with the last good state (else the first bad
    /// one).
    pub against_good: bool,
    /// The sides are blind (A/B), not the step found's Before/After.
    pub blind: bool,
    /// The side heard (`None`: the mix — getting ready, found).
    pub hearing: Option<BisectSide>,
    /// The two sides are ready to be heard.
    pub ready: bool,
    pub progress: Option<f64>,
    pub error: Option<String>,
    pub match_loudness: bool,
    pub found: Option<BisectFound>,
    pub log: Vec<BisectLog>,
    /// Where the search is, in words.
    pub text: String,
}

/// A state of the history rendered for the search.
enum NodeRender {
    Rendering(StateRender),
    Done(StateFile),
    Failed(String),
}

/// Two states joined into one file: A's region, then B's.
#[derive(Clone, Debug)]
struct Joined {
    path: PathBuf,
    a: CompareSettings,
    b: CompareSettings,
    /// File frames from where A plays to where B plays at the same moment.
    a_to_b: i64,
}

/// The two states compared now, being joined or ready.
struct Pair {
    a: u64,
    b: u64,
    matched: bool,
    /// The joined file.
    path: PathBuf,
    job: Option<JoinHandle<std::result::Result<Joined, String>>>,
    ready: Option<(Arc<StreamSource>, Joined)>,
    error: Option<String>,
    /// Heard at once when ready (a new pair; not one joined anew at other
    /// gains).
    hear_when_ready: bool,
}

/// A round: the candidate (an index into the path) against an end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Round {
    number: usize,
    candidate: usize,
    against_good: bool,
    candidate_side: BisectSide,
}

struct Search {
    path: Vec<u64>,
    /// `path[lo]` is the last state judged good, `path[hi]` the first bad.
    lo: usize,
    hi: usize,
    skipped: BTreeSet<usize>,
    /// The round asked (`None`: found).
    round: Option<Round>,
    asked: usize,
    log: Vec<BisectLog>,
    stretch: Stretch,
    renders: HashMap<u64, NodeRender>,
    pair: Option<Pair>,
    /// The step found's before and after are heard.
    auditioning_found: bool,
    side: BisectSide,
    /// A reference compared before the search (chosen again after it).
    before: Option<Comparing>,
    /// The project it belongs to.
    serial: u64,
    /// Every file made (deleted at the end).
    files: Vec<PathBuf>,
}

impl Search {
    /// The candidate nearest the middle of what is in question, not
    /// skipped (ties: the earlier).
    fn pick(&self) -> Option<usize> {
        let mid2 = self.lo + self.hi;
        (self.lo + 1..self.hi)
            .filter(|i| !self.skipped.contains(i))
            .min_by_key(|i| ((2 * i) as i64 - mid2 as i64).unsigned_abs())
    }

    /// Candidates left (not skipped).
    fn open(&self) -> usize {
        (self.lo + 1..self.hi)
            .filter(|i| !self.skipped.contains(i))
            .count()
    }

    /// The two states compared now and whether blind.
    fn wanted(&self) -> Option<(u64, u64, bool)> {
        if self.auditioning_found {
            return Some((self.path[self.lo], self.path[self.hi], false));
        }
        let r = self.round?;
        let candidate = self.path[r.candidate];
        let other = if r.against_good {
            self.path[self.lo]
        } else {
            self.path[self.hi]
        };
        Some(match r.candidate_side {
            BisectSide::A => (candidate, other, true),
            BisectSide::B => (other, candidate, true),
        })
    }

    fn ready(&self) -> Option<&(Arc<StreamSource>, Joined)> {
        let (a, b, _) = self.wanted()?;
        self.pair
            .as_ref()
            .filter(|p| p.a == a && p.b == b)
            .and_then(|p| p.ready.as_ref())
    }
}

#[derive(Default)]
enum Phase {
    #[default]
    Off,
    Setup {
        good: Option<u64>,
        bad: Option<u64>,
    },
    Search(Box<Search>),
}

/// Something a search left running: its file goes once it is done.
enum Leftover {
    Render(RenderJob, PathBuf),
    Join(JoinHandle<std::result::Result<Joined, String>>, PathBuf),
}

pub(crate) struct BisectState {
    phase: Phase,
    match_loudness: bool,
    /// Draws the blind assignment.
    rng: u64,
    seeded: bool,
    leftovers: Vec<Leftover>,
}

impl Default for BisectState {
    fn default() -> Self {
        Self {
            phase: Phase::Off,
            match_loudness: true,
            rng: 0,
            seeded: false,
            leftovers: Vec::new(),
        }
    }
}

impl BisectState {
    /// A fair coin (splitmix64).
    fn coin(&mut self) -> BisectSide {
        self.rng = self.rng.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.rng;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^= z >> 31;
        if z & 1 == 0 {
            BisectSide::A
        } else {
            BisectSide::B
        }
    }

    fn search(&self) -> Option<&Search> {
        match &self.phase {
            Phase::Search(s) => Some(s),
            _ => None,
        }
    }

    fn search_mut(&mut self) -> Option<&mut Search> {
        match &mut self.phase {
            Phase::Search(s) => Some(s),
            _ => None,
        }
    }

    /// A pair done with: its file goes (once its join is done, if it still
    /// runs). Returns the file to delete now.
    fn retire(&mut self, pair: Pair) -> Option<PathBuf> {
        match pair.job {
            Some(job) => {
                self.leftovers.push(Leftover::Join(job, pair.path));
                None
            }
            None => Some(pair.path),
        }
    }
}

/// About how many comparisons find one of `open` candidates (and the step
/// after them).
fn rounds_for(open: usize) -> usize {
    let mut n = 0;
    while (1usize << n) < open + 1 {
        n += 1;
    }
    n
}

/// Join two rendered states into one file at `out`: A's frames, then B's,
/// each at its gain (matched: both at the quieter one's loudness over the
/// stretch heard).
fn join_states(
    a: &StateFile,
    b: &StateFile,
    matched: bool,
    out: PathBuf,
) -> std::result::Result<Joined, String> {
    let (ca, rate) = compare::read_file(&a.path)?;
    let (cb, _) = compare::read_file(&b.path)?;
    let heard = |c: &[Vec<f32>], f: &StateFile| -> Vec<Vec<f32>> {
        let (from, to) = (f.from.max(0) as usize, f.to.max(0) as usize);
        c.iter()
            .map(|ch| ch[from.min(ch.len())..to.min(ch.len())].to_vec())
            .collect()
    };
    let (mut ga, mut gb) = (1.0f32, 1.0f32);
    if matched {
        let la = compare::integrated(&heard(&ca, a), a.layout, rate);
        let lb = compare::integrated(&heard(&cb, b), b.layout, rate);
        if la.is_finite() && lb.is_finite() {
            let target = la.min(lb);
            ga = faderframe_core::db_to_gain((target - la) as f32);
            gb = faderframe_core::db_to_gain((target - lb) as f32);
        }
    }
    let frames = |c: &[Vec<f32>]| c.first().map_or(0, Vec::len);
    let (na, nb) = (frames(&ca), frames(&cb));
    let channels = ca.len().max(cb.len()).max(1);
    let side = |c: &[Vec<f32>], ch: usize, n: usize, g: f32, v: &mut Vec<f32>| match c.get(ch) {
        Some(x) => v.extend(x.iter().map(|s| s * g)),
        None => v.resize(v.len() + n, 0.0),
    };
    let joined: Vec<Vec<f32>> = (0..channels)
        .map(|ch| {
            let mut v = Vec::with_capacity(na + nb);
            side(&ca, ch, na, ga, &mut v);
            side(&cb, ch, nb, gb, &mut v);
            v
        })
        .collect();
    faderframe_audio_files::write_wav(&out, &joined, rate, WavFormat::Float32, false)
        .map_err(|e| format!("{}: {e}", out.display()))?;
    let off = na as i64;
    Ok(Joined {
        path: out,
        a: CompareSettings {
            sync: CompareSync::Follow { anchor: a.anchor },
            from: a.from,
            to: a.to,
        },
        b: CompareSettings {
            sync: CompareSync::Follow {
                anchor: b.anchor - off,
            },
            from: b.from + off,
            to: b.to + off,
        },
        a_to_b: a.anchor - b.anchor + off,
    })
}

static NEXT_FILE: AtomicUsize = AtomicUsize::new(0);

impl Session {
    // --- what the views read ---------------------------------------------------

    /// Audio Bisect is getting ready, searching or showing what it found.
    pub fn bisecting(&self) -> bool {
        !matches!(self.bisect.phase, Phase::Off)
    }

    /// Where Audio Bisect is (`None`: not on). Never tells which side of a
    /// round is the candidate until it is answered.
    pub fn bisect_status(&self) -> Option<BisectStatus> {
        let matched = self.bisect.match_loudness;
        let s = match &self.bisect.phase {
            Phase::Off => return None,
            Phase::Setup { good, bad } => {
                let text = match good {
                    None => "Audio Bisect: click the last step that sounded good (right-click \
                             a step to mark another bad one than the project now)"
                        .to_string(),
                    Some(g) => format!(
                        "Good: ‘{}’ · Bad: {} — press Start",
                        self.history_step_label(*g),
                        bad.map_or_else(
                            || "the project now".to_string(),
                            |b| format!("‘{}’", self.history_step_label(b))
                        )
                    ),
                };
                return Some(BisectStatus {
                    stage: BisectStage::Setup,
                    good: *good,
                    bad: *bad,
                    path: Vec::new(),
                    range: Vec::new(),
                    skipped: Vec::new(),
                    round: 0,
                    rounds_left: 0,
                    against_good: true,
                    blind: true,
                    hearing: None,
                    ready: false,
                    progress: None,
                    error: None,
                    match_loudness: matched,
                    found: None,
                    log: Vec::new(),
                    text,
                });
            }
            Phase::Search(s) => s,
        };
        let ready = s.ready().is_some() && self.compare.source.is_some();
        let blind = !s.auditioning_found;
        let hearing = (ready && self.compare.hear).then_some(s.side);
        let found = s.round.is_none().then(|| self.bisect_found(s));
        let error = self.bisect_error(s);
        let progress = self.bisect_progress();
        let open = s.open();
        let text = if let Some(f) = &found {
            let what = if f.exact {
                let step = &f.steps[0];
                format!(
                    "Found: {}‘{}’ (step {}, {})",
                    if step.undone { "undoing " } else { "" },
                    step.label,
                    step.node,
                    crate::undo_tree::ago(step.made, SystemTime::now())
                )
            } else {
                format!(
                    "It is one of {} steps (skipped ones kept it from narrowing)",
                    f.steps.len()
                )
            };
            match hearing {
                Some(side) => format!("{what} · Hearing {}", side.label(false)),
                None => what,
            }
        } else if let Some(e) = &error {
            format!("Could not prepare: {e}")
        } else if !ready {
            format!(
                "Round {} — preparing A and B… {:.0} %",
                s.round.map_or(0, |r| r.number),
                progress.unwrap_or(0.0) * 100.0
            )
        } else {
            format!(
                "Round {} · about {} more · {} — which is better?",
                s.round.map_or(0, |r| r.number),
                rounds_for(open).saturating_sub(1),
                match hearing {
                    Some(side) => format!("Hearing {}", side.label(true)),
                    None => "Hearing the mix".into(),
                }
            )
        };
        Some(BisectStatus {
            stage: if found.is_some() {
                BisectStage::Found
            } else {
                BisectStage::Comparing
            },
            good: Some(s.path[s.lo]),
            bad: Some(s.path[s.hi]),
            path: s.path.clone(),
            range: s.path[s.lo..=s.hi].to_vec(),
            skipped: s.skipped.iter().map(|i| s.path[*i]).collect(),
            round: s.round.map_or(s.asked, |r| r.number),
            rounds_left: rounds_for(open),
            against_good: s.round.is_none_or(|r| r.against_good),
            blind,
            hearing,
            ready,
            progress,
            error,
            match_loudness: matched,
            found,
            log: s.log.clone(),
            text,
        })
    }

    /// The step (or steps) found.
    fn bisect_found(&self, s: &Search) -> BisectFound {
        let step = |i: usize| -> FoundStep {
            // The move from path[i - 1] to path[i]: down does a step, up
            // undoes the one left.
            let (from, to) = (s.path[i - 1], s.path[i]);
            let down = self.history.step(to).and_then(|t| t.parent) == Some(from);
            let node = if down { to } else { from };
            let info = self.history.step(node);
            FoundStep {
                node,
                label: info
                    .as_ref()
                    .map_or_else(|| format!("step {node}"), |i| i.label.clone()),
                made: info.map_or(SystemTime::UNIX_EPOCH, |i| i.made),
                undone: !down,
            }
        };
        let steps: Vec<FoundStep> = (s.lo + 1..=s.hi).map(step).collect();
        let exact = steps.len() == 1;
        let take_back = match (exact, steps.first()) {
            (true, Some(f)) if !f.undone && self.history.is_done(f.node) => {
                Ok(format!("Undo ‘{}’", f.label))
            }
            (true, Some(f)) if !f.undone => Err(format!(
                "‘{}’ is not in the project now (undone, or on another branch)",
                f.label
            )),
            (true, Some(f)) if self.history.knows(f.node) && !self.history.is_done(f.node) => {
                Ok(format!("Bring Back ‘{}’", f.label))
            }
            (true, Some(f)) => Err(format!("‘{}’ is in the project now", f.label)),
            _ => Err("the search could not narrow it down to one step".into()),
        };
        BisectFound {
            steps,
            exact,
            before: s.path[s.lo],
            after: s.path[s.hi],
            take_back,
        }
    }

    fn bisect_error(&self, s: &Search) -> Option<String> {
        let (a, b, _) = s.wanted()?;
        for n in [a, b] {
            if let Some(NodeRender::Failed(e)) = s.renders.get(&n) {
                return Some(e.clone());
            }
        }
        s.pair
            .as_ref()
            .filter(|p| p.a == a && p.b == b)
            .and_then(|p| p.error.clone())
    }

    /// How far the two sides wanted now are prepared (`None`: ready or
    /// nothing wanted).
    pub(crate) fn bisect_progress(&self) -> Option<f64> {
        let s = self.bisect.search()?;
        let (a, b, _) = s.wanted()?;
        if s.ready().is_some() {
            return None;
        }
        let part = |n: u64| match s.renders.get(&n) {
            Some(NodeRender::Rendering(r)) => r.job.progress.fraction(),
            Some(NodeRender::Done(_)) | Some(NodeRender::Failed(_)) => 1.0,
            None => 0.0,
        };
        Some((0.5 * (part(a) + part(b))).min(0.99))
    }

    /// The compare path's other side, as [`Session::compare_status`] names
    /// it: the side heard and what it is.
    pub(crate) fn bisect_heard(&self) -> (&'static str, String) {
        let Some(s) = self.bisect.search() else {
            return ("A", String::new());
        };
        if s.auditioning_found {
            let name = |n: u64| {
                if n == History::ROOT {
                    "the start".to_string()
                } else {
                    format!("just after ‘{}’", self.history_step_label(n))
                }
            };
            let node = match s.side {
                BisectSide::A => s.path[s.lo],
                BisectSide::B => s.path[s.hi],
            };
            return (s.side.label(false), name(node));
        }
        (
            s.side.label(true),
            "a state of the history (blind)".to_string(),
        )
    }

    // --- actions ------------------------------------------------------------------

    /// From now on the blind assignment is drawn from `seed` (scripted
    /// runs and tests repeat it).
    pub fn set_bisect_seed(&mut self, seed: u64) {
        self.bisect.rng = seed;
        self.bisect.seeded = true;
    }

    pub(crate) fn bisect_op(&mut self, op: BisectOp) -> Result<()> {
        self.compare_follows_project();
        match op {
            BisectOp::Setup => {
                self.end_bisect(None);
                self.bisect.phase = Phase::Setup {
                    good: None,
                    bad: None,
                };
            }
            BisectOp::Mark { node, good } => {
                if node != History::ROOT && !self.history.knows(node) {
                    return Err(SessionError::Other(format!(
                        "the history has no step {node}"
                    )));
                }
                if !matches!(self.bisect.phase, Phase::Setup { .. }) {
                    self.end_bisect(None);
                    self.bisect.phase = Phase::Setup {
                        good: None,
                        bad: None,
                    };
                }
                if let Phase::Setup { good: g, bad: b } = &mut self.bisect.phase {
                    if good {
                        *g = Some(node);
                    } else {
                        *b = (node != self.history.current()).then_some(node);
                    }
                }
            }
            BisectOp::Start { good, bad } => self.start_bisect(good, bad)?,
            BisectOp::Hear(side) => {
                let s = self.searching()?;
                s.side = side;
                self.compare.hear = true;
                self.hear_bisect_side();
            }
            BisectOp::Flip => self.bisect_compare_op(CompareOp::Flip)?,
            BisectOp::MatchLoudness(on) => self.bisect_compare_op(CompareOp::MatchLoudness(on))?,
            BisectOp::Answer(answer) => self.answer_bisect(answer)?,
            BisectOp::AuditionFound => {
                let s = self.searching()?;
                if s.round.is_some() {
                    return Err(SessionError::Other("nothing is found yet".into()));
                }
                s.auditioning_found = true;
                s.side = BisectSide::A;
                let nodes = [s.path[s.lo], s.path[s.hi]];
                self.compare.hear = true;
                self.set_compare_source(None);
                self.render_nodes(&nodes);
            }
            BisectOp::UndoFound => self.take_back_found()?,
            BisectOp::GoToFound { after } => {
                let s = self.searching()?;
                if s.round.is_some() {
                    return Err(SessionError::Other("nothing is found yet".into()));
                }
                let node = if after { s.path[s.hi] } else { s.path[s.lo] };
                self.end_bisect(None);
                self.history_to_node(node)?;
            }
            BisectOp::Cancel => self.end_bisect(Some("Audio Bisect cancelled — nothing changed")),
        }
        self.revision += 1;
        Ok(())
    }

    /// The compare path's own controls while bisecting: the mix or a side,
    /// A ⇄ B, the loudness match, off (cancel).
    pub(crate) fn bisect_compare_op(&mut self, op: CompareOp) -> Result<()> {
        match op {
            CompareOp::Hear(on) => {
                self.searching()?;
                self.compare.hear = on;
                self.hear_bisect_side();
            }
            CompareOp::Flip => {
                let hearing = self.compare.hear;
                let s = self.searching()?;
                if hearing {
                    s.side = s.side.other();
                }
                self.compare.hear = true;
                self.hear_bisect_side();
            }
            CompareOp::MatchLoudness(on) => {
                // Kept for the next search too.
                self.bisect.match_loudness = on;
                if self.bisect.search().is_some() && self.compare.what == Some(Comparing::Bisect) {
                    self.compare.matched = on;
                    // Joined anew at the new gains (the side heard stays).
                    self.set_compare_source(None);
                }
            }
            CompareOp::Off => self.end_bisect(Some("Audio Bisect cancelled — nothing changed")),
        }
        self.send_compare();
        self.revision += 1;
        Ok(())
    }

    fn searching(&mut self) -> Result<&mut Search> {
        self.bisect
            .search_mut()
            .ok_or_else(|| SessionError::Other("Audio Bisect is not searching".into()))
    }

    fn start_bisect(&mut self, good: u64, bad: Option<u64>) -> Result<()> {
        let bad = bad.unwrap_or(self.history.current());
        for n in [good, bad] {
            if n != History::ROOT && !self.history.knows(n) {
                return Err(SessionError::Other(format!("the history has no step {n}")));
            }
        }
        if good == bad {
            return Err(SessionError::Other(
                "the good and the bad state are the same: mark an earlier step good".into(),
            ));
        }
        let path = self
            .history
            .path_between(good, bad)
            .ok_or_else(|| SessionError::Other("no way between the two states".into()))?;
        let now = self.absolute_copy();
        let stretch = self.audition_stretch(&now);
        if stretch.b <= stretch.a {
            return Err(SessionError::Other(
                "nothing to compare: the range to hear is empty".into(),
            ));
        }
        self.end_bisect(None);
        if !self.bisect.seeded {
            let nanos = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos() as u64);
            self.bisect.rng = nanos ^ u64::from(std::process::id()).rotate_left(32);
            self.bisect.seeded = true;
        }
        let before = self
            .compare
            .what
            .filter(|w| matches!(w, Comparing::Reference(_)));
        self.compare_with(Comparing::Bisect);
        self.set_compare_gain(0.0, self.bisect.match_loudness);
        let steps = path.len() - 1;
        let hi = steps;
        self.bisect.phase = Phase::Search(Box::new(Search {
            path,
            lo: 0,
            hi,
            skipped: BTreeSet::new(),
            round: None,
            asked: 0,
            log: Vec::new(),
            stretch,
            renders: HashMap::new(),
            pair: None,
            auditioning_found: false,
            side: BisectSide::A,
            before,
            serial: self.project_serial,
            files: Vec::new(),
        }));
        self.notify(
            NoticeLevel::Info,
            format!(
                "Audio Bisect: {steps} step{} between the good and the bad state — about {} \
                 comparisons",
                if steps == 1 { "" } else { "s" },
                rounds_for(steps.saturating_sub(1))
            ),
        );
        self.next_bisect_round(None);
        Ok(())
    }

    /// The next round (the same candidate against the first bad state when
    /// `again`), or the step found.
    fn next_bisect_round(&mut self, again: Option<usize>) {
        let side = self.bisect.coin();
        let old = self.bisect.search_mut().and_then(|s| s.pair.take());
        if let Some(path) = old.and_then(|p| self.bisect.retire(p)) {
            self.compare.throw_away(path);
        }
        let Some(s) = self.bisect.search_mut() else {
            return;
        };
        s.auditioning_found = false;
        s.side = BisectSide::A;
        let candidate = again.or_else(|| s.pick());
        let Some(candidate) = candidate else {
            s.round = None;
            let found = self.bisect.search().map(|s| self.bisect_found(s));
            self.set_compare_source(None);
            self.compare.hear = false;
            self.send_compare();
            if let Some(f) = found {
                let text = if f.exact {
                    format!(
                        "Audio Bisect found it: {}‘{}’",
                        if f.steps[0].undone { "undoing " } else { "" },
                        f.steps[0].label
                    )
                } else {
                    format!(
                        "Audio Bisect: it is one of {} steps (skipped ones kept it from \
                         narrowing)",
                        f.steps.len()
                    )
                };
                self.notify(NoticeLevel::Info, text);
            }
            return;
        };
        s.asked += 1;
        let round = Round {
            number: s.asked,
            candidate,
            against_good: again.is_none(),
            candidate_side: side,
        };
        s.round = Some(round);
        // The candidate and the end it is compared with; the other end
        // too (asked for when the answer is "Can't tell").
        let nodes = [s.path[candidate], s.path[s.lo], s.path[s.hi]];
        self.set_compare_source(None);
        self.render_nodes(&nodes);
    }

    /// Start rendering the states not rendered yet.
    fn render_nodes(&mut self, nodes: &[u64]) {
        let missing: Vec<u64> = match self.bisect.search() {
            Some(s) => nodes
                .iter()
                .copied()
                .filter(|n| !s.renders.contains_key(n))
                .collect(),
            None => return,
        };
        if missing.is_empty() {
            return;
        }
        let now = self.absolute_copy();
        let dir = self.compare.dir();
        let made = std::fs::create_dir_all(&dir)
            .map_err(|e| format!("{}: {e}", dir.display()))
            .err();
        let Some(stretch) = self.bisect.search().map(|s| s.stretch) else {
            return;
        };
        let mut started = Vec::new();
        for node in missing {
            let n = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
            let path = dir.join(format!("bisect-{n}-state-{node}.wav"));
            let r = match &made {
                Some(e) => NodeRender::Failed(e.clone()),
                None => match self.history.project_at_node(&now, node) {
                    Ok(p) => match render_state(p, stretch, path.clone()) {
                        Ok(r) => NodeRender::Rendering(r),
                        Err(e) => NodeRender::Failed(e.to_string()),
                    },
                    Err(e) => NodeRender::Failed(e.to_string()),
                },
            };
            started.push((node, path, r));
        }
        if let Some(s) = self.bisect.search_mut() {
            for (node, path, r) in started {
                s.files.push(path);
                s.renders.insert(node, r);
            }
        }
    }

    fn answer_bisect(&mut self, answer: BisectAnswer) -> Result<()> {
        let s = self.searching()?;
        let Some(r) = s.round else {
            return Err(SessionError::Other(
                "nothing is asked: the search is done".into(),
            ));
        };
        if s.ready().is_none() {
            return Err(SessionError::Other(
                "A and B are still being prepared".into(),
            ));
        }
        let candidate_better = match answer {
            BisectAnswer::A => Some(r.candidate_side == BisectSide::A),
            BisectAnswer::B => Some(r.candidate_side == BisectSide::B),
            BisectAnswer::CantTell => None,
        };
        let verdict = match (candidate_better, r.against_good) {
            (Some(true), _) => BisectVerdict::Good,
            (Some(false), _) => BisectVerdict::Bad,
            (None, true) => BisectVerdict::Undecided,
            (None, false) => BisectVerdict::Skipped,
        };
        let node = s.path[r.candidate];
        match verdict {
            BisectVerdict::Good => s.lo = r.candidate,
            BisectVerdict::Bad => s.hi = r.candidate,
            BisectVerdict::Skipped => {
                s.skipped.insert(r.candidate);
            }
            BisectVerdict::Undecided => {}
        }
        let label = self.history_step_label(node);
        if let Some(s) = self.bisect.search_mut() {
            s.log.push(BisectLog {
                round: r.number,
                node,
                label,
                against_good: r.against_good,
                was: r.candidate_side,
                answer,
                verdict,
            });
        }
        let again = (verdict == BisectVerdict::Undecided).then_some(r.candidate);
        self.next_bisect_round(again);
        Ok(())
    }

    /// Take the step found back alone (a new step), and end the search.
    fn take_back_found(&mut self) -> Result<()> {
        let s = self.searching()?;
        if s.round.is_some() {
            return Err(SessionError::Other("nothing is found yet".into()));
        }
        let found = self
            .bisect
            .search()
            .map(|s| self.bisect_found(s))
            .ok_or_else(|| SessionError::Other("Audio Bisect is not searching".into()))?;
        let label = found.take_back.clone().map_err(SessionError::Other)?;
        let step = &found.steps[0];
        let node = step.node;
        let cmd = if step.undone {
            self.history.again_of(node, &label)
        } else if self.history.last_step() == Some(node) && !self.history.in_gesture() {
            None
        } else {
            self.history.inverse_of(node, &label)
        };
        self.end_bisect(None);
        match cmd {
            None if !step.undone => {
                if let Some(r) = self.history.undo(&mut self.project)? {
                    self.sync(r.impact)?;
                }
            }
            None => return Err(SessionError::Other("its step is gone".into())),
            Some(cmd) => self.edit(cmd).map_err(|e| {
                SessionError::Other(format!(
                    "it cannot be taken back alone (a later edit changed what it changed): {e}"
                ))
            })?,
        }
        self.notify(NoticeLevel::Info, format!("Audio Bisect: {label}"));
        Ok(())
    }

    /// Play the side wanted (once the two are joined).
    fn hear_bisect_side(&mut self) {
        let Some(s) = self.bisect.search() else {
            return;
        };
        let Some((source, joined)) = s.ready() else {
            self.send_compare();
            return;
        };
        let (settings, also) = match s.side {
            BisectSide::A => (joined.a, joined.a_to_b),
            BisectSide::B => (joined.b, -joined.a_to_b),
        };
        let source = Arc::clone(source);
        self.compare.also = vec![also];
        self.set_compare_source(Some(CompareSource { source, settings }));
        self.send_compare();
    }

    /// End the search (anything it left running finishes on its own; its
    /// files go). The project and the history are as they were.
    pub(crate) fn end_bisect(&mut self, notice: Option<&str>) {
        let phase = std::mem::take(&mut self.bisect.phase);
        let Phase::Search(s) = phase else {
            if notice.is_some() && !matches!(phase, Phase::Off) {
                self.notify(NoticeLevel::Info, notice.unwrap_or_default());
            }
            return;
        };
        let s = *s;
        let mut files = s.files;
        for (_, r) in s.renders {
            if let NodeRender::Rendering(r) = r {
                r.job.cancel();
                self.bisect
                    .leftovers
                    .push(Leftover::Render(r.job, r.file.path));
            }
        }
        if let Some(path) = s.pair.and_then(|p| self.bisect.retire(p)) {
            files.push(path);
        }
        if self.compare.what == Some(Comparing::Bisect) {
            self.compare_off();
        }
        for f in files {
            self.compare.throw_away(f);
        }
        if let Some(Comparing::Reference(id)) = s.before
            && self.project.references.get(id).is_some()
        {
            let _ = self.choose_reference(Some(id));
        }
        if let Some(n) = notice {
            self.notify(NoticeLevel::Info, n);
        }
        self.revision += 1;
    }

    /// The tick: renders done, the two states joined, the side handed to
    /// the engine; the search ends when something else takes the compare
    /// path or another project the session's place.
    pub(crate) fn poll_bisect(&mut self) {
        // What earlier rounds or searches left running: files go when done.
        let mut gone = Vec::new();
        self.bisect.leftovers.retain(|l| {
            let (finished, path) = match l {
                Leftover::Render(j, p) => (j.is_finished(), p),
                Leftover::Join(j, p) => (j.is_finished(), p),
            };
            if finished {
                gone.push(path.clone());
            }
            !finished
        });
        for path in gone {
            self.compare.throw_away(path);
        }
        let Some(s) = self.bisect.search() else {
            return;
        };
        if s.serial != self.project_serial {
            return self.end_bisect(Some("Audio Bisect ended: another project took its place"));
        }
        if self.compare.what != Some(Comparing::Bisect) {
            return self.end_bisect(Some("Audio Bisect ended: something else is compared now"));
        }
        let matched = self.bisect.match_loudness;
        let dir = self.compare.dir();
        let mut changed = false;
        let mut opened = None;
        let mut delete = Vec::new();
        let BisectState {
            phase, leftovers, ..
        } = &mut self.bisect;
        let Phase::Search(s) = phase else {
            return;
        };
        // Renders done.
        for r in s.renders.values_mut() {
            let finished = matches!(r, NodeRender::Rendering(st) if st.job.is_finished());
            if finished
                && let NodeRender::Rendering(st) =
                    std::mem::replace(r, NodeRender::Failed(String::new()))
            {
                *r = match st.job.join() {
                    Ok(_) => NodeRender::Done(st.file),
                    Err(e) => NodeRender::Failed(e.to_string()),
                };
                changed = true;
            }
        }
        // The two states wanted joined (anew when they or the gains
        // changed).
        if let Some((a, b, _)) = s.wanted() {
            let current = s
                .pair
                .as_ref()
                .is_some_and(|p| p.a == a && p.b == b && p.matched == matched);
            if !current
                && let (Some(NodeRender::Done(fa)), Some(NodeRender::Done(fb))) =
                    (s.renders.get(&a), s.renders.get(&b))
            {
                let (fa, fb) = (fa.clone(), fb.clone());
                let again = s.pair.as_ref().is_some_and(|p| p.a == a && p.b == b);
                if let Some(old) = s.pair.take() {
                    match old.job {
                        Some(job) => leftovers.push(Leftover::Join(job, old.path)),
                        None => delete.push(old.path),
                    }
                }
                let n = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
                let out = dir.join(format!("bisect-{n}-pair.wav"));
                s.files.push(out.clone());
                let path = out.clone();
                let job = std::thread::Builder::new()
                    .name("faderframe-bisect".into())
                    .spawn(move || join_states(&fa, &fb, matched, out));
                let (job, error) = match job {
                    Ok(j) => (Some(j), None),
                    Err(e) => (None, Some(e.to_string())),
                };
                s.pair = Some(Pair {
                    a,
                    b,
                    matched,
                    path,
                    job,
                    ready: None,
                    error,
                    hear_when_ready: !again,
                });
                changed = true;
            }
        }
        // Joined: opened and handed to the engine.
        if let Some(p) = s.pair.as_mut()
            && p.job.as_ref().is_some_and(JoinHandle::is_finished)
            && let Some(job) = p.job.take()
        {
            match job.join() {
                Ok(Ok(joined)) => match StreamSource::open(&joined.path) {
                    Ok(source) => {
                        p.ready = Some((source, joined));
                        opened = Some(p.hear_when_ready);
                    }
                    Err(e) => p.error = Some(format!("{}: {e}", joined.path.display())),
                },
                Ok(Err(e)) => p.error = Some(e),
                Err(_) => p.error = Some("joining the two states failed".into()),
            }
            changed = true;
        }
        let busy = s
            .renders
            .values()
            .any(|r| matches!(r, NodeRender::Rendering(_)))
            || s.pair.as_ref().is_some_and(|p| p.job.is_some());
        for path in delete {
            self.compare.throw_away(path);
        }
        if let Some(hear) = opened {
            // A new pair is heard at once (A first).
            if hear {
                self.compare.hear = true;
            }
            self.hear_bisect_side();
        }
        if changed || busy {
            self.revision += 1;
        }
    }

    /// Wait until the two sides wanted are ready (or failed, or nothing is
    /// wanted). Tests and scripts.
    pub fn wait_for_bisect(&mut self, timeout: Duration) -> bool {
        let until = std::time::Instant::now() + timeout;
        loop {
            self.poll_compare();
            let settled = match self.bisect.search() {
                None => true,
                Some(s) => match s.wanted() {
                    None => true,
                    Some(_) => {
                        s.ready().is_some() && self.compare.source.is_some()
                            || self.bisect_error(s).is_some()
                    }
                },
            };
            if settled {
                return true;
            }
            if std::time::Instant::now() > until {
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounds_halve_what_is_open() {
        assert_eq!(rounds_for(0), 0);
        assert_eq!(rounds_for(1), 1);
        assert_eq!(rounds_for(3), 2);
        assert_eq!(rounds_for(7), 3);
        assert_eq!(rounds_for(8), 4);
    }

    #[test]
    fn the_coin_is_fair_and_repeats_from_its_seed() {
        let mut a = BisectState {
            rng: 42,
            ..Default::default()
        };
        let mut b = BisectState {
            rng: 42,
            ..Default::default()
        };
        let draws: Vec<BisectSide> = (0..400).map(|_| a.coin()).collect();
        let again: Vec<BisectSide> = (0..400).map(|_| b.coin()).collect();
        assert_eq!(draws, again);
        let heads = draws.iter().filter(|s| **s == BisectSide::A).count();
        assert!((150..250).contains(&heads), "{heads}");
    }

    #[test]
    fn the_candidate_is_nearest_the_middle_and_never_skipped() {
        let s = |lo, hi, skipped: &[usize]| Search {
            path: (0..20).collect(),
            lo,
            hi,
            skipped: skipped.iter().copied().collect(),
            round: None,
            asked: 0,
            log: Vec::new(),
            stretch: Stretch {
                range: Default::default(),
                a: 0,
                b: 1,
                start: 0,
                rate: 48_000,
            },
            renders: HashMap::new(),
            pair: None,
            auditioning_found: false,
            side: BisectSide::A,
            before: None,
            serial: 0,
            files: Vec::new(),
        };
        assert_eq!(s(0, 8, &[]).pick(), Some(4));
        assert_eq!(s(0, 8, &[4]).pick(), Some(3));
        assert_eq!(s(0, 8, &[3, 4]).pick(), Some(5));
        assert_eq!(s(3, 5, &[4]).pick(), None);
        assert_eq!(s(4, 5, &[]).pick(), None);
        assert_eq!(s(0, 8, &[4]).open(), 6);
    }
}
