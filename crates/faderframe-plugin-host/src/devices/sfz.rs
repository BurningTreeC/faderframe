//! SFZ instruments: the text (comments, `#define`, `#include`), the
//! headers (`<control>`, `<global>`, `<master>`, `<group>`, `<region>`,
//! `<curve>`; `<effect>` and `<midi>` are read past) and the regions'
//! opcodes, inherited down the headers, into [`Zone`]s the sampler plays.
//!
//! What a zone holds follows SFZ 1.0 and the common SFZ 2 additions:
//! sample playback (offset, end, count, loops with a crossfade, direction,
//! delay — some by random, some by controller), the conditions a note must
//! meet (channel, key, velocity, controller, bend and aftertouch ranges,
//! random and tempo ranges, round robins, keyswitches with `sw_last`,
//! `sw_down`, `sw_up` and `sw_previous`, triggers on attack, release,
//! first, legato, `release_key` and on controllers), voice control
//! (`group`, `off_by`, `off_mode`, `polyphony`, `note_polyphony`), the
//! amplifier (volume, amplitude, pan, width, position, key and velocity
//! tracking, velocity curves, randomness, release decay, key, velocity and
//! controller crossfades), pitch (key centre and tracking, velocity
//! tracking, randomness, transpose, tune, bend range), two filters (low,
//! high, band pass and band reject with 1, 2, 4 and 6 poles, peak and
//! shelves; cutoff, resonance and gain with key, velocity, random,
//! envelope, LFO, controller and aftertouch modulation), a three-band EQ,
//! three envelopes (amplifier, filter, pitch: delay, start, attack, hold,
//! decay, sustain, release, depth, their velocity scaling and controller
//! modulation) and three LFOs (amplifier, filter, pitch: delay, fade,
//! frequency, depth, by controller and aftertouch). Controller modulation
//! is written `name_ccN` or `name_onccN` and may name a `<curve>`
//! (`name_curveccN`); the extended controllers 128 (pitch bend), 129
//! (channel aftertouch), 130 (polyphonic aftertouch), 131 (velocity), 133
//! (key), 135/136 (random) are understood. `sample=*sine` (`*saw`,
//! `*square`, `*triangle`, `*noise`, `*silence`) generates its waveform.

use super::samples::{LoopMode, Sample, SampleSet, load_cached};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Controllers a zone may follow: MIDI's 0–127 and SFZ 2's extended ones.
/// (sfizz's 512: the ones above the extended controllers are set by
/// `set_ccN` and automation.)
pub const CONTROLLERS: usize = 512;
pub const CC_BEND: usize = 128;
pub const CC_CHANAFT: usize = 129;
pub const CC_POLYAFT: usize = 130;
pub const CC_VELOCITY: usize = 131;
pub const CC_KEY: usize = 133;
pub const CC_RANDOM_UNI: usize = 135;
pub const CC_RANDOM_BI: usize = 136;

/// One controller's modulation of a value (`amount` at the controller's
/// full value, through `curve` when it names one).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CcMod {
    pub cc: usize,
    pub amount: f32,
    pub curve: Option<usize>,
}

/// What an envelope stage's controller modulation changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EgParam {
    Delay,
    Start,
    Attack,
    Hold,
    Decay,
    Sustain,
    Release,
    Depth,
}

/// An envelope (times in seconds, levels 0–1, depth in cents).
#[derive(Clone, Debug, PartialEq)]
pub struct EgSpec {
    pub delay: f32,
    pub start: f32,
    pub attack: f32,
    pub hold: f32,
    pub decay: f32,
    pub sustain: f32,
    pub release: f32,
    pub depth: f32,
    pub vel2delay: f32,
    pub vel2attack: f32,
    pub vel2hold: f32,
    pub vel2decay: f32,
    pub vel2sustain: f32,
    pub vel2release: f32,
    pub vel2depth: f32,
    pub cc: Vec<(EgParam, CcMod)>,
    /// The file says anything about it.
    pub set: bool,
}

impl Default for EgSpec {
    fn default() -> Self {
        Self {
            delay: 0.0,
            start: 0.0,
            attack: 0.0,
            hold: 0.0,
            decay: 0.0,
            sustain: 1.0,
            release: 0.0,
            depth: 0.0,
            vel2delay: 0.0,
            vel2attack: 0.0,
            vel2hold: 0.0,
            vel2decay: 0.0,
            vel2sustain: 0.0,
            vel2release: 0.0,
            vel2depth: 0.0,
            cc: Vec::new(),
            set: false,
        }
    }
}

/// A (sine) LFO: depth in dB (amplifier) or cents (filter, pitch).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LfoSpec {
    pub delay: f32,
    pub fade: f32,
    pub freq: f32,
    pub depth: f32,
    pub depth_cc: Vec<CcMod>,
    pub freq_cc: Vec<CcMod>,
}

impl LfoSpec {
    pub fn active(&self) -> bool {
        self.depth != 0.0 || !self.depth_cc.is_empty()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FilterKind {
    Lp1,
    Hp1,
    #[default]
    Lp2,
    Hp2,
    Bp2,
    Br2,
    Lp4,
    Hp4,
    Lp6,
    Hp6,
    Bp1,
    Br1,
    Peak,
    LowShelf,
    HighShelf,
}

impl FilterKind {
    fn parse(v: &str) -> Option<Self> {
        Some(match v {
            "lpf_1p" => Self::Lp1,
            "hpf_1p" => Self::Hp1,
            "lpf_2p" | "lpf_2p_sv" => Self::Lp2,
            "hpf_2p" | "hpf_2p_sv" => Self::Hp2,
            "bpf_2p" | "bpf_2p_sv" => Self::Bp2,
            "brf_2p" | "brf_2p_sv" => Self::Br2,
            "lpf_4p" => Self::Lp4,
            "hpf_4p" => Self::Hp4,
            "lpf_6p" => Self::Lp6,
            "hpf_6p" => Self::Hp6,
            "bpf_1p" => Self::Bp1,
            "brf_1p" => Self::Br1,
            "pkf_2p" | "peq" => Self::Peak,
            "lsh" => Self::LowShelf,
            "hsh" => Self::HighShelf,
            _ => return None,
        })
    }
}

/// One of a zone's two filters.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FilterSpec {
    pub kind: FilterKind,
    /// Hz.
    pub cutoff: f32,
    /// dB.
    pub resonance: f32,
    /// dB (peak and shelves).
    pub gain: f32,
    /// Cents per key from `keycenter`.
    pub keytrack: f32,
    pub keycenter: u8,
    /// Cents at full velocity.
    pub veltrack: f32,
    /// Cents (random, up to).
    pub random: f32,
    pub cutoff_cc: Vec<CcMod>,
    pub resonance_cc: Vec<CcMod>,
    pub gain_cc: Vec<CcMod>,
}

/// One band of a zone's equaliser.
#[derive(Clone, Debug, PartialEq)]
pub struct EqBand {
    pub freq: f32,
    /// Octaves.
    pub bw: f32,
    pub gain: f32,
    pub vel2freq: f32,
    pub vel2gain: f32,
    pub freq_cc: Vec<CcMod>,
    pub bw_cc: Vec<CcMod>,
    pub gain_cc: Vec<CcMod>,
}

impl EqBand {
    fn new(freq: f32) -> Self {
        Self {
            freq,
            bw: 1.0,
            gain: 0.0,
            vel2freq: 0.0,
            vel2gain: 0.0,
            freq_cc: Vec::new(),
            bw_cc: Vec::new(),
            gain_cc: Vec::new(),
        }
    }

    pub fn active(&self) -> bool {
        self.gain != 0.0 || self.vel2gain != 0.0 || !self.gain_cc.is_empty()
    }
}

/// When a zone sounds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Trigger {
    #[default]
    Attack,
    /// On the note's release (after the sustain pedal).
    Release,
    /// Only when no other key is held.
    First,
    /// Only while another key is held.
    Legato,
    /// On the key's release, pedal or not.
    ReleaseKey,
}

/// A crossfade: in (rising from `lo` to `hi`) or out (falling).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Xfade {
    pub cc: usize,
    pub lo: f32,
    pub hi: f32,
    pub fade_in: bool,
}

/// One region of an instrument (see the module docs; units as SFZ's).
#[derive(Clone, Debug, PartialEq)]
pub struct Zone {
    pub sample: usize,
    // Playback.
    pub offset: usize,
    pub offset_random: usize,
    pub offset_cc: Vec<CcMod>,
    pub end: Option<usize>,
    /// Plays this many times (0: once).
    pub count: u32,
    pub loop_mode: Option<LoopMode>,
    pub loop_start: Option<usize>,
    pub loop_end: Option<usize>,
    /// Seconds.
    pub loop_crossfade: Option<f32>,
    pub reverse: bool,
    /// `oscillator=on`: the sample is one cycle at the root key's pitch.
    pub oscillator: bool,
    pub delay: f32,
    pub delay_random: f32,
    pub delay_cc: Vec<CcMod>,
    // Conditions (channels 0-based).
    pub lochan: u8,
    pub hichan: u8,
    pub lokey: u8,
    pub hikey: u8,
    pub lovel: u8,
    pub hivel: u8,
    pub cc_ranges: Vec<(usize, u8, u8)>,
    pub lobend: i32,
    pub hibend: i32,
    pub lochanaft: u8,
    pub hichanaft: u8,
    pub lopolyaft: u8,
    pub hipolyaft: u8,
    pub lorand: f64,
    pub hirand: f64,
    pub lobpm: f32,
    pub hibpm: f32,
    pub seq_length: u32,
    pub seq_position: u32,
    pub sw_lokey: Option<u8>,
    pub sw_hikey: Option<u8>,
    pub sw_last: Option<u8>,
    pub sw_down: Option<u8>,
    pub sw_up: Option<u8>,
    pub sw_previous: Option<u8>,
    pub sw_default: Option<u8>,
    /// Velocity of the previous note instead of this one's.
    pub sw_vel_previous: bool,
    pub trigger: Trigger,
    /// Triggered when the controller moves into the range.
    pub on_cc: Vec<(usize, u8, u8)>,
    // Voices.
    pub group: u32,
    pub off_by: u32,
    /// `off_mode=normal`: the release envelope (else quickly, or in
    /// `off_time`).
    pub off_normal: bool,
    pub off_time: Option<f32>,
    pub polyphony: Option<u32>,
    pub note_polyphony: Option<u32>,
    // Amplifier.
    /// dB.
    pub volume: f32,
    /// 0–1.
    pub amplitude: f32,
    /// −1…1.
    pub pan: f32,
    /// −1…1 (1: as recorded).
    pub width: f32,
    /// −1…1.
    pub position: f32,
    pub amp_keycenter: u8,
    /// dB per key.
    pub amp_keytrack: f32,
    /// −1…1.
    pub veltrack: f32,
    /// Amplitude (0–1) per velocity when the file draws a curve.
    pub velcurve: Option<Box<[f32; 128]>>,
    /// dB (random, up to).
    pub amp_random: f32,
    /// Release triggers lose this much (dB) per second the note was held.
    pub rt_decay: f32,
    pub xfades: Vec<Xfade>,
    /// Crossfades by gain (else by power).
    pub xf_key_gain: bool,
    pub xf_vel_gain: bool,
    pub xf_cc_gain: bool,
    pub volume_cc: Vec<CcMod>,
    pub amplitude_cc: Vec<CcMod>,
    pub pan_cc: Vec<CcMod>,
    pub width_cc: Vec<CcMod>,
    pub position_cc: Vec<CcMod>,
    // Pitch.
    pub root: f64,
    /// Cents per key.
    pub keytrack: f64,
    /// Cents at full velocity.
    pub pitch_veltrack: f32,
    /// Cents (random, up to).
    pub pitch_random: f32,
    /// Cents (with `transpose`).
    pub tune: f64,
    /// Cents at full bend up and down.
    pub bend_up: f32,
    pub bend_down: f32,
    /// Cents the bend moves in.
    pub bend_step: f32,
    pub pitch_cc: Vec<CcMod>,
    // Filters, EQ, envelopes, LFOs.
    pub filters: [Option<FilterSpec>; 2],
    pub eq: [EqBand; 3],
    pub ampeg: EgSpec,
    pub fileg: EgSpec,
    pub pitcheg: EgSpec,
    pub amplfo: LfoSpec,
    pub fillfo: LfoSpec,
    pub pitchlfo: LfoSpec,
}

impl Zone {
    /// A zone playing `sample` across every key and velocity.
    pub fn whole(sample: usize) -> Self {
        Self {
            sample,
            offset: 0,
            offset_random: 0,
            offset_cc: Vec::new(),
            end: None,
            count: 0,
            loop_mode: None,
            loop_start: None,
            loop_end: None,
            loop_crossfade: None,
            reverse: false,
            oscillator: false,
            delay: 0.0,
            delay_random: 0.0,
            delay_cc: Vec::new(),
            lochan: 0,
            hichan: 15,
            lokey: 0,
            hikey: 127,
            lovel: 0,
            hivel: 127,
            cc_ranges: Vec::new(),
            lobend: -8192,
            hibend: 8192,
            lochanaft: 0,
            hichanaft: 127,
            lopolyaft: 0,
            hipolyaft: 127,
            lorand: 0.0,
            hirand: 1.0,
            lobpm: 0.0,
            hibpm: 500.0,
            seq_length: 1,
            seq_position: 1,
            sw_lokey: None,
            sw_hikey: None,
            sw_last: None,
            sw_down: None,
            sw_up: None,
            sw_previous: None,
            sw_default: None,
            sw_vel_previous: false,
            trigger: Trigger::Attack,
            on_cc: Vec::new(),
            group: 0,
            off_by: 0,
            off_normal: false,
            off_time: None,
            polyphony: None,
            note_polyphony: None,
            volume: 0.0,
            amplitude: 1.0,
            pan: 0.0,
            width: 1.0,
            position: 0.0,
            amp_keycenter: 60,
            amp_keytrack: 0.0,
            veltrack: 1.0,
            velcurve: None,
            amp_random: 0.0,
            rt_decay: 0.0,
            xfades: Vec::new(),
            xf_key_gain: false,
            xf_vel_gain: false,
            xf_cc_gain: false,
            volume_cc: Vec::new(),
            amplitude_cc: Vec::new(),
            pan_cc: Vec::new(),
            width_cc: Vec::new(),
            position_cc: Vec::new(),
            root: 60.0,
            keytrack: 100.0,
            pitch_veltrack: 0.0,
            pitch_random: 0.0,
            tune: 0.0,
            bend_up: 200.0,
            bend_down: -200.0,
            bend_step: 1.0,
            pitch_cc: Vec::new(),
            filters: [None, None],
            eq: [EqBand::new(50.0), EqBand::new(500.0), EqBand::new(5_000.0)],
            ampeg: EgSpec::default(),
            fileg: EgSpec::default(),
            pitcheg: EgSpec::default(),
            amplfo: LfoSpec::default(),
            fillfo: LfoSpec::default(),
            pitchlfo: LfoSpec::default(),
        }
    }

    /// The key and velocity ranges take the note.
    pub fn takes(&self, key: u8, velocity: u8) -> bool {
        (self.lokey..=self.hikey).contains(&key) && (self.lovel..=self.hivel).contains(&velocity)
    }

    /// The key is a keyswitch of this zone.
    pub fn is_keyswitch(&self, key: u8) -> bool {
        match (self.sw_lokey, self.sw_hikey) {
            (Some(lo), Some(hi)) => (lo..=hi).contains(&key),
            (Some(lo), None) => key >= lo,
            (None, Some(hi)) => key <= hi,
            (None, None) => self.sw_last == Some(key),
        }
    }

    pub fn eq_active(&self) -> bool {
        self.eq.iter().any(EqBand::active)
    }
}

/// A `<curve>`: a value (0–1) per controller value.
pub type Curve = [f32; 128];

/// What the `<control>` header and the curves say for the whole file.
#[derive(Clone, Debug)]
pub struct Instrument {
    /// Controllers' values before any arrives (0–1).
    pub initial_cc: Box<[f32; CONTROLLERS]>,
    pub curves: Vec<Curve>,
}

impl Default for Instrument {
    fn default() -> Self {
        let mut cc = Box::new([0.0f32; CONTROLLERS]);
        // Volume and expression start full, pan centred (SFZ's defaults).
        cc[7] = 100.0 / 127.0;
        cc[10] = 0.5;
        cc[11] = 1.0;
        Self {
            initial_cc: cc,
            curves: default_curves(),
        }
    }
}

/// SFZ 2's predefined curves 0–6 (linear, bipolar, inverted, …).
fn default_curves() -> Vec<Curve> {
    let lin = |f: &dyn Fn(f32) -> f32| -> Curve { std::array::from_fn(|i| f(i as f32 / 127.0)) };
    vec![
        lin(&|x| x),
        lin(&|x| 2.0 * x - 1.0),
        lin(&|x| 1.0 - x),
        lin(&|x| 1.0 - 2.0 * x),
        lin(&|x| x * x),
        lin(&|x| x.sqrt()),
        lin(&|x| (1.0 - x).sqrt()),
    ]
}

// --- the text -------------------------------------------------------------------

/// The opcodes of one header, as written.
pub type Opcodes = Vec<(String, String)>;

/// The text with `#include`s inlined (relative to the instrument's folder,
/// else to the including file's; a file including itself again is
/// skipped), `#define`d names replaced and comments removed, and the
/// samples embedded in `<sample>` headers (name, audio file bytes).
pub fn preprocess(text: &str, file: &Path) -> (String, Vec<(String, Vec<u8>)>) {
    let dir = file.parent().unwrap_or(Path::new("."));
    let mut cx = Expand {
        root: dir.to_path_buf(),
        defines: Vec::new(),
        out: String::new(),
        stack: vec![file.canonicalize().unwrap_or_else(|_| file.to_path_buf())],
        embedded: Vec::new(),
    };
    cx.expand(text, dir);
    (cx.out, cx.embedded)
}

struct Expand {
    root: PathBuf,
    defines: Vec<(String, String)>,
    out: String,
    stack: Vec<PathBuf>,
    embedded: Vec<(String, Vec<u8>)>,
}

impl Expand {
    fn expand(&mut self, text: &str, here: &Path) {
        let text = take_embedded(text, &mut self.embedded);
        let text = strip_block_comments(&text);
        for raw in text.lines() {
            let line = match raw.find("//") {
                Some(i) => &raw[..i],
                None => raw,
            };
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix("#define") {
                let mut parts = rest.split_whitespace();
                if let (Some(name), Some(value)) = (parts.next(), parts.next())
                    && name.starts_with('$')
                {
                    let value = substitute(value, &self.defines);
                    self.defines.retain(|(n, _)| n != name);
                    self.defines.push((name.to_string(), value));
                    // Longest names first, so `$AB` is not read as `$A`.
                    self.defines
                        .sort_by_key(|(n, _)| std::cmp::Reverse(n.len()));
                }
                continue;
            }
            if let Some(rest) = trimmed.strip_prefix("#include") {
                let name = substitute(rest.trim(), &self.defines);
                let name = name.trim().trim_matches('"');
                let mut path = resolve(&self.root, name);
                if !path.exists() {
                    path = resolve(here, name);
                }
                let key = path.canonicalize().unwrap_or_else(|_| path.clone());
                if self.stack.len() < 32
                    && !self.stack.contains(&key)
                    && let Ok(bytes) = std::fs::read(&path)
                {
                    let included = String::from_utf8_lossy(&bytes).into_owned();
                    let sub = path.parent().unwrap_or(here).to_path_buf();
                    self.stack.push(key);
                    self.expand(&included, &sub);
                    self.stack.pop();
                }
                continue;
            }
            self.out.push_str(&substitute(line, &self.defines));
            self.out.push('\n');
        }
    }
}

/// `rel` under `dir` (`\\` read as `/`), its parts matched without regard
/// to case where the exact names are not there (libraries made on other
/// systems).
pub fn resolve(dir: &Path, rel: &str) -> PathBuf {
    let rel = rel.replace('\\', "/");
    let exact = dir.join(&rel);
    if exact.exists() || Path::new(&rel).is_absolute() {
        return exact;
    }
    let mut at = dir.to_path_buf();
    for part in rel.split('/').filter(|p| !p.is_empty()) {
        let next = at.join(part);
        if part == "." || part == ".." || next.exists() {
            at = next;
            continue;
        }
        let found = std::fs::read_dir(&at).ok().and_then(|entries| {
            entries
                .flatten()
                .find(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case(part))
                .map(|e| e.path())
        });
        match found {
            Some(p) => at = p,
            None => return exact,
        }
    }
    at
}

/// Take sfizz's `<sample>` headers (`name=…`, then the file as base64 in
/// `data=` or `base64data=` up to the next header) out of the text: their
/// data may hold `//`.
fn take_embedded(text: &str, out: &mut Vec<(String, Vec<u8>)>) -> String {
    if !text.contains("<sample>") {
        return text.to_string();
    }
    let mut rest = text;
    let mut kept = String::with_capacity(text.len());
    while let Some(i) = rest.find("<sample>") {
        kept.push_str(&rest[..i]);
        let block = &rest[i + "<sample>".len()..];
        let end = block.find('<').unwrap_or(block.len());
        let body = &block[..end];
        if let Some(d) = body.find("data=") {
            let start = if body[..d].ends_with("base64") {
                d - 6
            } else {
                d
            };
            let meta = headers(&format!("<sample>{}", &body[..start]));
            let name = meta
                .first()
                .and_then(|(_, ops)| ops.iter().find(|(k, _)| k == "name"))
                .map(|(_, v)| v.replace('\\', "/"));
            if let (Some(name), Some(bytes)) = (name, base64(&body[d + 5..])) {
                out.push((name, bytes));
            }
        }
        // Keep the line count.
        kept.extend(body.chars().filter(|c| *c == '\n'));
        rest = &block[end..];
    }
    kept.push_str(rest);
    kept
}

/// Standard base64 (whitespace and padding ignored).
fn base64(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in text.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            c if c.is_ascii_whitespace() => continue,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    (!out.is_empty()).then_some(out)
}

fn strip_block_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find("/*") {
        out.push_str(&rest[..i]);
        match rest[i + 2..].find("*/") {
            Some(j) => {
                // Keep the line count (no headers glued together).
                out.extend(rest[i..i + 2 + j + 2].chars().filter(|c| *c == '\n'));
                rest = &rest[i + 2 + j + 2..];
            }
            None => {
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

fn substitute(line: &str, defines: &[(String, String)]) -> String {
    if !line.contains('$') {
        return line.to_string();
    }
    let mut s = line.to_string();
    for (name, value) in defines {
        s = s.replace(name.as_str(), value);
    }
    s
}

/// Split (preprocessed) SFZ text into headers and their opcodes.
pub fn headers(text: &str) -> Vec<(String, Opcodes)> {
    let mut out: Vec<(String, Opcodes)> = Vec::new();
    for line in text.lines() {
        let mut rest = line.trim();
        while !rest.is_empty() {
            if let Some(after) = rest.strip_prefix('<') {
                let Some(end) = after.find('>') else { break };
                out.push((after[..end].trim().to_ascii_lowercase(), Vec::new()));
                rest = after[end + 1..].trim_start();
                continue;
            }
            let Some(eq) = rest.find('=') else { break };
            let key = rest[..eq].trim().to_ascii_lowercase();
            let after = &rest[eq + 1..];
            // The value (spaces and all: sample paths have them) runs to
            // the next opcode (`name=`) or header.
            let end = {
                let mut end = after.len();
                let bytes = after.as_bytes();
                let mut i = 0;
                while i < bytes.len() {
                    if bytes[i] == b'<' {
                        end = i;
                        break;
                    }
                    if bytes[i].is_ascii_whitespace() {
                        let tail = after[i..].trim_start();
                        let word_end =
                            tail.find(|c: char| c == '=' || c.is_whitespace() || c == '<');
                        if let Some(w) = word_end
                            && tail[w..].starts_with('=')
                        {
                            end = i;
                            break;
                        }
                    }
                    i += 1;
                }
                end
            };
            let value = after[..end].trim().to_string();
            if out.is_empty() {
                out.push(("global".into(), Vec::new()));
            }
            if let Some(last) = out.last_mut() {
                last.1.push((key, value));
            }
            rest = after[end..].trim_start();
        }
    }
    out
}

/// A key: a number or a note name (`c4` = 60, sharps `#`, flats `b`).
pub fn parse_key(v: &str) -> Option<u8> {
    if let Ok(n) = v.trim().parse::<i32>() {
        return u8::try_from(n.clamp(0, 127)).ok();
    }
    let v = v.trim().to_ascii_lowercase();
    let mut chars = v.chars();
    let base = match chars.next()? {
        'c' => 0,
        'd' => 2,
        'e' => 4,
        'f' => 5,
        'g' => 7,
        'a' => 9,
        'b' => 11,
        _ => return None,
    };
    let rest: String = chars.collect();
    let (shift, octave) = if let Some(o) = rest.strip_prefix('#') {
        (1, o)
    } else if let Some(o) = rest.strip_prefix('b').filter(|o| !o.is_empty()) {
        (-1, o)
    } else {
        (0, rest.as_str())
    };
    let octave: i32 = octave.parse().ok()?;
    u8::try_from((12 * (octave + 1) + base + shift).clamp(0, 127)).ok()
}

/// `name_ccN`, `name_onccN`, `nameccN` or `name_curveccN`: the name, the
/// controller and whether it names a curve.
fn split_cc(key: &str) -> Option<(&str, usize, bool)> {
    let digits = key.len() - key.bytes().rev().take_while(u8::is_ascii_digit).count();
    if digits == key.len() {
        return None;
    }
    let cc: usize = key[digits..].parse().ok()?;
    let stem = &key[..digits];
    let (name, curve) = if let Some(n) = stem.strip_suffix("_curvecc") {
        (n, true)
    } else if let Some(n) = stem.strip_suffix("_oncc") {
        (n, false)
    } else if let Some(n) = stem.strip_suffix("_cc") {
        (n, false)
    } else {
        (stem.strip_suffix("cc")?, false)
    };
    (cc < CONTROLLERS).then_some((name, cc, curve))
}

/// The `<control>` settings the regions' opcodes are read with.
#[derive(Default)]
struct Control {
    default_path: String,
    note_offset: i32,
    octave_offset: i32,
}

impl Control {
    fn key(&self, v: &str) -> Option<u8> {
        let k = i32::from(parse_key(v)?) + self.note_offset + 12 * self.octave_offset;
        u8::try_from(k.clamp(0, 127)).ok()
    }
}

/// Read an SFZ instrument (the text of `file`) into `set`: its samples
/// (relative to the file, or to `default_path`), zones, curves and
/// controllers' initial values.
pub fn load(set: &mut SampleSet, text: &str, file: &Path) {
    let dir = file.parent().unwrap_or(Path::new("."));
    let (text, embedded) = preprocess(text, file);
    let mut inst = Instrument::default();
    let mut control = Control::default();
    let mut global: Opcodes = Vec::new();
    let mut master: Opcodes = Vec::new();
    let mut group: Opcodes = Vec::new();
    let mut loaded: HashMap<PathBuf, Option<usize>> = HashMap::new();
    // Curves first (regions may name them before they are drawn).
    let all = headers(&text);
    for (header, ops) in &all {
        if header == "curve" {
            read_curve(&mut inst, ops);
        }
    }
    let mut in_effect = false;
    for (header, ops) in all {
        match header.as_str() {
            "control" => {
                in_effect = false;
                // Each control header starts a new default path.
                control.default_path.clear();
                for (k, v) in &ops {
                    match k.as_str() {
                        "default_path" => control.default_path = v.replace('\\', "/"),
                        "note_offset" => control.note_offset = v.parse().unwrap_or(0),
                        "octave_offset" => control.octave_offset = v.parse().unwrap_or(0),
                        _ => {
                            if let Some(n) = k.strip_prefix("set_cc")
                                && let (Ok(n), Ok(value)) = (n.parse::<usize>(), v.parse::<f32>())
                                && n < CONTROLLERS
                            {
                                inst.initial_cc[n] = (value / 127.0).clamp(0.0, 1.0);
                            }
                            if let Some(n) = k
                                .strip_prefix("set_hdcc")
                                .or_else(|| k.strip_prefix("set_realcc"))
                                && let (Ok(n), Ok(value)) = (n.parse::<usize>(), v.parse::<f32>())
                                && n < CONTROLLERS
                            {
                                inst.initial_cc[n] = value.clamp(0.0, 1.0);
                            }
                        }
                    }
                }
            }
            "global" => {
                in_effect = false;
                global = ops;
                master.clear();
                group.clear();
            }
            "master" => {
                in_effect = false;
                master = ops;
                group.clear();
            }
            "group" => {
                in_effect = false;
                group = ops;
            }
            "effect" | "midi" | "curve" => in_effect = true,
            "region" if !in_effect => {
                let ops: Vec<&(String, String)> = global
                    .iter()
                    .chain(&master)
                    .chain(&group)
                    .chain(&ops)
                    .collect();
                let Some(sample) = ops
                    .iter()
                    .rev()
                    .find(|(k, _)| k == "sample")
                    .map(|(_, v)| v.replace('\\', "/"))
                else {
                    continue;
                };
                let index = if let Some(kind) = sample.strip_prefix('*') {
                    let key = PathBuf::from(format!("*{kind}"));
                    *loaded.entry(key).or_insert_with(|| {
                        generated(kind).map(|s| {
                            set.samples.push(Arc::new(s));
                            set.samples.len() - 1
                        })
                    })
                } else if let Some((name, bytes)) = embedded.iter().find(|(n, _)| *n == sample) {
                    let key = PathBuf::from(format!("<sample>{name}"));
                    *loaded.entry(key).or_insert_with(|| {
                        match super::samples::load_audio_bytes(name, bytes.clone()) {
                            Ok(s) => {
                                set.samples.push(Arc::new(s));
                                Some(set.samples.len() - 1)
                            }
                            Err(e) => {
                                set.errors.push(e);
                                None
                            }
                        }
                    })
                } else {
                    // The default path is a prefix (`…/sample` + `1.wav`).
                    let path = resolve(dir, &format!("{}{sample}", control.default_path));
                    *loaded
                        .entry(path.clone())
                        .or_insert_with(|| match load_cached(&path) {
                            Ok(s) => {
                                set.samples.push(s);
                                Some(set.samples.len() - 1)
                            }
                            Err(e) => {
                                set.errors.push(e);
                                None
                            }
                        })
                };
                let Some(index) = index else { continue };
                let mut z = zone(index, &ops, &control, &inst);
                if sample == "*noise" {
                    z.keytrack = 0.0;
                }
                if sample.starts_with('*') && z.loop_mode.is_none() {
                    z.loop_mode = Some(LoopMode::Continuous);
                }
                // A wavetable: the whole sample one cycle, looped.
                if z.oscillator {
                    z.loop_mode = Some(LoopMode::Continuous);
                    (z.loop_start, z.loop_end, z.loop_crossfade) = (Some(0), None, Some(0.0));
                }
                set.zones.push(z);
            }
            _ => {}
        }
    }
    set.instrument = Some(Arc::new(inst));
}

fn read_curve(inst: &mut Instrument, ops: &Opcodes) {
    let mut index = None;
    let mut points: Vec<(usize, f32)> = Vec::new();
    for (k, v) in ops {
        if k == "curve_index" {
            index = v.parse::<usize>().ok();
        } else if let Some(n) = k.strip_prefix('v')
            && let (Ok(n), Ok(value)) = (n.parse::<usize>(), v.parse::<f32>())
            && n < 128
        {
            points.push((n, value));
        }
    }
    let Some(index) = index.filter(|i| *i < 256) else {
        return;
    };
    points.sort_by_key(|(n, _)| *n);
    let curve = interpolate(&points, |x| x as f32 / 127.0);
    while inst.curves.len() <= index {
        inst.curves.push(std::array::from_fn(|i| i as f32 / 127.0));
    }
    inst.curves[index] = curve;
}

/// 128 values through `points` (linear between them; `fallback` without
/// any).
fn interpolate(points: &[(usize, f32)], fallback: impl Fn(usize) -> f32) -> [f32; 128] {
    std::array::from_fn(|x| {
        if points.is_empty() {
            return fallback(x);
        }
        let after = points.iter().position(|(n, _)| *n >= x);
        match after {
            Some(0) => points[0].1,
            Some(i) => {
                let (x0, y0) = points[i - 1];
                let (x1, y1) = points[i];
                if x1 == x0 {
                    y1
                } else {
                    y0 + (y1 - y0) * (x - x0) as f32 / (x1 - x0) as f32
                }
            }
            None => points[points.len() - 1].1,
        }
    })
}

/// A generated waveform: one cycle at middle C (or a second of noise).
fn generated(kind: &str) -> Option<Sample> {
    const N: usize = 2048;
    let middle_c = 440.0 * 2f64.powf(-9.0 / 12.0);
    let cycle =
        |f: &dyn Fn(f64) -> f32| -> Vec<f32> { (0..N).map(|i| f(i as f64 / N as f64)).collect() };
    let (data, rate) = match kind {
        "sine" => (
            cycle(&|t| (std::f64::consts::TAU * t).sin() as f32),
            middle_c * N as f64,
        ),
        "saw" => (cycle(&|t| (2.0 * t - 1.0) as f32), middle_c * N as f64),
        "square" => (
            cycle(&|t| if t < 0.5 { 1.0 } else { -1.0 }),
            middle_c * N as f64,
        ),
        "triangle" | "tri" => (
            cycle(&|t| (1.0 - 4.0 * (t - 0.5).abs()) as f32),
            middle_c * N as f64,
        ),
        "silence" => (vec![0.0; N], middle_c * N as f64),
        "noise" => {
            let mut x = 0x9E37_79B9u32;
            let data = (0..48_000)
                .map(|_| {
                    x ^= x << 13;
                    x ^= x >> 17;
                    x ^= x << 5;
                    (x as f32 / u32::MAX as f32) * 2.0 - 1.0
                })
                .collect();
            (data, 48_000.0)
        }
        _ => return None,
    };
    Some(Sample::from_channels(format!("*{kind}"), rate, vec![data]))
}

/// A zone of `sample` from the opcodes in effect for a region.
fn zone(sample: usize, ops: &[&(String, String)], control: &Control, inst: &Instrument) -> Zone {
    let mut z = Zone::whole(sample);
    let mut key_set = false;
    let (mut extra_db, mut extra_amp) = (0.0f32, 1.0f32);
    let mut velcurve: Vec<(usize, f32)> = Vec::new();
    let mut filters: [Option<FilterSpec>; 2] = [None, None];
    // Curves named for controller modulation: (name, cc) → curve.
    let mut curves: Vec<(String, usize, usize)> = Vec::new();
    for (k, v) in ops {
        let f = || v.parse::<f32>().ok();
        let key = || control.key(v);
        match k.as_str() {
            // Playback.
            "offset" => z.offset = v.parse().unwrap_or(0),
            "offset_random" => z.offset_random = v.parse().unwrap_or(0),
            "end" => z.end = v.parse::<i64>().ok().map(|e| e.max(0) as usize),
            "count" => z.count = v.parse().unwrap_or(0),
            "loop_mode" | "loopmode" => {
                z.loop_mode = match v.as_str() {
                    "no_loop" => Some(LoopMode::NoLoop),
                    "one_shot" => Some(LoopMode::OneShot),
                    "loop_continuous" => Some(LoopMode::Continuous),
                    "loop_sustain" => Some(LoopMode::Sustain),
                    _ => z.loop_mode,
                }
            }
            "loop_start" | "loopstart" => z.loop_start = v.parse().ok(),
            "loop_end" | "loopend" => z.loop_end = v.parse().ok(),
            "loop_crossfade" => z.loop_crossfade = f(),
            "direction" => z.reverse = v == "reverse",
            "oscillator" => z.oscillator = v == "on",
            "delay" => z.delay = f().unwrap_or(0.0),
            "delay_random" => z.delay_random = f().unwrap_or(0.0),
            // Conditions.
            "lochan" => z.lochan = v.parse::<u8>().unwrap_or(1).clamp(1, 16) - 1,
            "hichan" => z.hichan = v.parse::<u8>().unwrap_or(16).clamp(1, 16) - 1,
            "lokey" => z.lokey = key().unwrap_or(z.lokey),
            "hikey" => z.hikey = key().unwrap_or(z.hikey),
            "key" => {
                if let Some(n) = key() {
                    (z.lokey, z.hikey, z.root) = (n, n, f64::from(n));
                    key_set = true;
                }
            }
            "pitch_keycenter" => {
                if let Some(n) = key() {
                    z.root = f64::from(n);
                    key_set = true;
                }
            }
            "lovel" => z.lovel = v.parse().unwrap_or(z.lovel),
            "hivel" => z.hivel = v.parse().unwrap_or(z.hivel),
            "lobend" => z.lobend = v.parse().unwrap_or(z.lobend),
            "hibend" => z.hibend = v.parse().unwrap_or(z.hibend),
            "lochanaft" => z.lochanaft = v.parse().unwrap_or(0),
            "hichanaft" => z.hichanaft = v.parse().unwrap_or(127),
            "lopolyaft" => z.lopolyaft = v.parse().unwrap_or(0),
            "hipolyaft" => z.hipolyaft = v.parse().unwrap_or(127),
            "lorand" => z.lorand = f().map_or(0.0, f64::from),
            "hirand" => z.hirand = f().map_or(1.0, f64::from),
            "lobpm" => z.lobpm = f().unwrap_or(0.0),
            "hibpm" => z.hibpm = f().unwrap_or(500.0),
            "seq_length" => z.seq_length = v.parse::<u32>().unwrap_or(1).max(1),
            "seq_position" => z.seq_position = v.parse::<u32>().unwrap_or(1).max(1),
            "sw_lokey" => z.sw_lokey = key(),
            "sw_hikey" => z.sw_hikey = key(),
            "sw_last" => z.sw_last = key(),
            "sw_down" => z.sw_down = key(),
            "sw_up" => z.sw_up = key(),
            "sw_previous" => z.sw_previous = key(),
            "sw_default" => z.sw_default = key(),
            "sw_vel" => z.sw_vel_previous = v == "previous",
            "trigger" => {
                z.trigger = match v.as_str() {
                    "release" => Trigger::Release,
                    "first" => Trigger::First,
                    "legato" => Trigger::Legato,
                    "release_key" => Trigger::ReleaseKey,
                    _ => Trigger::Attack,
                }
            }
            "group" => z.group = v.parse().unwrap_or(0),
            "off_by" => z.off_by = v.parse().unwrap_or(0),
            "off_mode" => z.off_normal = v == "normal",
            "off_time" => z.off_time = f(),
            "polyphony" => z.polyphony = v.parse().ok(),
            "note_polyphony" => z.note_polyphony = v.parse().ok(),
            // Amplifier.
            "volume" | "gain" => z.volume = f().unwrap_or(0.0),
            // ARIA's levels per header, on top of the region's.
            "group_volume" | "master_volume" | "global_volume" => extra_db += f().unwrap_or(0.0),
            "group_amplitude" | "master_amplitude" | "global_amplitude" => {
                extra_amp *= f().unwrap_or(100.0) / 100.0
            }
            "amplitude" => z.amplitude = f().unwrap_or(100.0) / 100.0,
            "pan" => z.pan = (f().unwrap_or(0.0) / 100.0).clamp(-1.0, 1.0),
            "width" => z.width = (f().unwrap_or(100.0) / 100.0).clamp(-1.0, 1.0),
            "position" => z.position = (f().unwrap_or(0.0) / 100.0).clamp(-1.0, 1.0),
            "amp_keycenter" => z.amp_keycenter = key().unwrap_or(60),
            "amp_keytrack" => z.amp_keytrack = f().unwrap_or(0.0),
            "amp_veltrack" => z.veltrack = f().unwrap_or(100.0) / 100.0,
            "amp_random" => z.amp_random = f().unwrap_or(0.0),
            "rt_decay" => z.rt_decay = f().unwrap_or(0.0),
            "xfin_lokey" | "xfin_hikey" | "xfout_lokey" | "xfout_hikey" => {
                xfade(&mut z, k, CC_KEY, key().map_or(0.0, f32::from));
            }
            "xfin_lovel" | "xfin_hivel" | "xfout_lovel" | "xfout_hivel" => {
                xfade(&mut z, k, CC_VELOCITY, f().unwrap_or(0.0));
            }
            "xf_keycurve" => z.xf_key_gain = v == "gain",
            "xf_velcurve" => z.xf_vel_gain = v == "gain",
            "xf_cccurve" => z.xf_cc_gain = v == "gain",
            // Pitch.
            "pitch_keytrack" => z.keytrack = f().map_or(100.0, f64::from),
            "pitch_veltrack" => z.pitch_veltrack = f().unwrap_or(0.0),
            "pitch_random" => z.pitch_random = f().unwrap_or(0.0),
            "transpose" => z.tune += 100.0 * f().map_or(0.0, f64::from),
            "tune" | "pitch" => z.tune += f().map_or(0.0, f64::from),
            "bend_up" | "bendup" => z.bend_up = f().unwrap_or(200.0),
            "bend_down" | "benddown" => z.bend_down = f().unwrap_or(-200.0),
            "bend_step" => z.bend_step = f().unwrap_or(1.0).max(1.0),
            // Filters.
            "fil_type" | "filtype" | "fil1_type" => {
                filter(&mut filters, 0).kind = FilterKind::parse(v).unwrap_or_default()
            }
            "fil2_type" => filter(&mut filters, 1).kind = FilterKind::parse(v).unwrap_or_default(),
            "cutoff" | "cutoff1" => filter(&mut filters, 0).cutoff = f().unwrap_or(0.0),
            "cutoff2" => filter(&mut filters, 1).cutoff = f().unwrap_or(0.0),
            "resonance" | "resonance1" => filter(&mut filters, 0).resonance = f().unwrap_or(0.0),
            "resonance2" => filter(&mut filters, 1).resonance = f().unwrap_or(0.0),
            "fil_gain" | "fil1_gain" => filter(&mut filters, 0).gain = f().unwrap_or(0.0),
            "fil2_gain" => filter(&mut filters, 1).gain = f().unwrap_or(0.0),
            "fil_keytrack" | "fil1_keytrack" => {
                filter(&mut filters, 0).keytrack = f().unwrap_or(0.0)
            }
            "fil2_keytrack" => filter(&mut filters, 1).keytrack = f().unwrap_or(0.0),
            "fil_keycenter" | "fil1_keycenter" => {
                filter(&mut filters, 0).keycenter = key().unwrap_or(60)
            }
            "fil2_keycenter" => filter(&mut filters, 1).keycenter = key().unwrap_or(60),
            "fil_veltrack" | "fil1_veltrack" => {
                filter(&mut filters, 0).veltrack = f().unwrap_or(0.0)
            }
            "fil2_veltrack" => filter(&mut filters, 1).veltrack = f().unwrap_or(0.0),
            "fil_random" | "fil1_random" => filter(&mut filters, 0).random = f().unwrap_or(0.0),
            "fil2_random" => filter(&mut filters, 1).random = f().unwrap_or(0.0),
            "cutoff_chanaft" => filter(&mut filters, 0)
                .cutoff_cc
                .push(cc_mod(CC_CHANAFT, f())),
            "cutoff_polyaft" => filter(&mut filters, 0)
                .cutoff_cc
                .push(cc_mod(CC_POLYAFT, f())),
            // Envelopes and LFOs.
            _ if eg_opcode(&mut z, k, f()) => {}
            _ if lfo_opcode(&mut z, k, f()) => {}
            _ if eq_opcode(&mut z, k, f()) => {}
            _ => {
                if let Some(n) = k.strip_prefix("amp_velcurve_")
                    && let (Ok(n), Some(value)) = (n.parse::<usize>(), f())
                    && n < 128
                {
                    velcurve.push((n, value));
                } else if let Some((name, cc, is_curve)) = split_cc(k) {
                    if is_curve {
                        if let Some(c) = v.parse::<usize>().ok().filter(|c| *c < inst.curves.len())
                        {
                            curves.push((name.to_string(), cc, c));
                        }
                    } else {
                        cc_opcode(&mut z, &mut filters, name, cc, v, f(), control);
                    }
                }
            }
        }
    }
    z.volume += extra_db;
    z.amplitude *= extra_amp;
    // `count` plays the sample through (that many times).
    if z.count > 0 && z.loop_mode.is_none() {
        z.loop_mode = Some(LoopMode::OneShot);
    }
    if !key_set && z.lokey == z.hikey {
        z.root = f64::from(z.lokey);
    }
    if !velcurve.is_empty() {
        velcurve.sort_by_key(|(n, _)| *n);
        // The curve runs from 0 at velocity 0 to 1 at 127 unless drawn.
        let mut points = velcurve;
        if points.first().is_some_and(|(n, _)| *n > 0) {
            points.insert(0, (0, 0.0));
        }
        if points.last().is_some_and(|(n, _)| *n < 127) {
            points.push((127, 1.0));
        }
        z.velcurve = Some(Box::new(interpolate(&points, |x| x as f32 / 127.0)));
    }
    // Filters with a type but no cutoff do nothing.
    for f in &mut filters {
        if f.as_ref().is_some_and(|s| s.cutoff <= 0.0) {
            *f = None;
        }
    }
    z.filters = filters;
    // Curves named for modulations.
    for (name, cc, curve) in curves {
        for m in mods_named(&mut z, &name) {
            if m.cc == cc {
                m.curve = Some(curve);
            }
        }
    }
    z
}

/// Filter `i` (made when an opcode first names it).
fn filter(filters: &mut [Option<FilterSpec>; 2], i: usize) -> &mut FilterSpec {
    filters[i].get_or_insert_with(|| FilterSpec {
        keycenter: 60,
        ..FilterSpec::default()
    })
}

fn cc_mod(cc: usize, amount: Option<f32>) -> CcMod {
    CcMod {
        cc,
        amount: amount.unwrap_or(0.0),
        curve: None,
    }
}

/// `xfin_*`/`xfout_*` (key and velocity): one end of a crossfade.
fn xfade(z: &mut Zone, k: &str, cc: usize, value: f32) {
    let fade_in = k.starts_with("xfin");
    let lo = k.ends_with("lokey") || k.ends_with("lovel");
    let i = match z
        .xfades
        .iter()
        .position(|x| x.cc == cc && x.fade_in == fade_in)
    {
        Some(i) => i,
        None => {
            z.xfades.push(Xfade {
                cc,
                lo: if fade_in { 0.0 } else { 127.0 },
                hi: if fade_in { 0.0 } else { 127.0 },
                fade_in,
            });
            z.xfades.len() - 1
        }
    };
    if lo {
        z.xfades[i].lo = value;
    } else {
        z.xfades[i].hi = value;
    }
}

/// The modulation lists a controller opcode's name feeds.
fn mods_named<'a>(z: &'a mut Zone, name: &str) -> Vec<&'a mut CcMod> {
    let list: Option<&mut Vec<CcMod>> = match name {
        "volume" | "gain" => Some(&mut z.volume_cc),
        "amplitude" => Some(&mut z.amplitude_cc),
        "pan" => Some(&mut z.pan_cc),
        "width" => Some(&mut z.width_cc),
        "position" => Some(&mut z.position_cc),
        "pitch" | "tune" => Some(&mut z.pitch_cc),
        "delay" => Some(&mut z.delay_cc),
        "offset" => Some(&mut z.offset_cc),
        "cutoff" | "cutoff1" => z.filters[0].as_mut().map(|f| &mut f.cutoff_cc),
        "cutoff2" => z.filters[1].as_mut().map(|f| &mut f.cutoff_cc),
        _ => None,
    };
    list.map(|l| l.iter_mut().collect()).unwrap_or_default()
}

/// A controller opcode (`name` by `cc`).
fn cc_opcode(
    z: &mut Zone,
    filters: &mut [Option<FilterSpec>; 2],
    name: &str,
    cc: usize,
    v: &str,
    value: Option<f32>,
    control: &Control,
) {
    let m = cc_mod(cc, value);
    let pct = |m: CcMod| CcMod {
        amount: m.amount / 100.0,
        ..m
    };
    match name {
        "lo" => {
            // `loccN`
            let lo = v.parse::<f32>().unwrap_or(0.0).clamp(0.0, 127.0) as u8;
            match z.cc_ranges.iter_mut().find(|r| r.0 == cc) {
                Some(r) => r.1 = lo,
                None => z.cc_ranges.push((cc, lo, 127)),
            }
        }
        "hi" => {
            let hi = v.parse::<f32>().unwrap_or(127.0).clamp(0.0, 127.0) as u8;
            match z.cc_ranges.iter_mut().find(|r| r.0 == cc) {
                Some(r) => r.2 = hi,
                None => z.cc_ranges.push((cc, 0, hi)),
            }
        }
        "lohd" | "hihd" => {
            let x = (v
                .parse::<f32>()
                .unwrap_or(if name == "lohd" { 0.0 } else { 1.0 })
                * 127.0)
                .round()
                .clamp(0.0, 127.0) as u8;
            match z.cc_ranges.iter_mut().find(|r| r.0 == cc) {
                Some(r) if name == "lohd" => r.1 = x,
                Some(r) => r.2 = x,
                None if name == "lohd" => z.cc_ranges.push((cc, x, 127)),
                None => z.cc_ranges.push((cc, 0, x)),
            }
        }
        "on_lo" | "on_hi" => {
            let x = v.parse::<f32>().unwrap_or(0.0).clamp(0.0, 127.0) as u8;
            match z.on_cc.iter_mut().find(|r| r.0 == cc) {
                Some(r) if name == "on_lo" => r.1 = x,
                Some(r) => r.2 = x,
                None if name == "on_lo" => z.on_cc.push((cc, x, 127)),
                None => z.on_cc.push((cc, 0, x)),
            }
        }
        "xfin_lo" | "xfin_hi" | "xfout_lo" | "xfout_hi" => {
            let fade_in = name.starts_with("xfin");
            let lo = name.ends_with("lo");
            let value = v.parse::<f32>().unwrap_or(0.0);
            let i = match z
                .xfades
                .iter()
                .position(|x| x.cc == cc && x.fade_in == fade_in)
            {
                Some(i) => i,
                None => {
                    z.xfades.push(Xfade {
                        cc,
                        lo: if fade_in { 0.0 } else { 127.0 },
                        hi: if fade_in { 0.0 } else { 127.0 },
                        fade_in,
                    });
                    z.xfades.len() - 1
                }
            };
            if lo {
                z.xfades[i].lo = value;
            } else {
                z.xfades[i].hi = value;
            }
        }
        "volume" | "gain" => z.volume_cc.push(m),
        "amplitude" => z.amplitude_cc.push(pct(m)),
        "pan" => z.pan_cc.push(pct(m)),
        "width" => z.width_cc.push(pct(m)),
        "position" => z.position_cc.push(pct(m)),
        "pitch" | "tune" => z.pitch_cc.push(m),
        "delay" => z.delay_cc.push(m),
        "offset" => z.offset_cc.push(m),
        "cutoff" | "cutoff1" => {
            let f = filter(filters, 0);
            f.cutoff_cc.push(m);
        }
        "cutoff2" => {
            let f = filter(filters, 1);
            f.cutoff_cc.push(m);
        }
        "resonance" | "resonance1" => {
            let f = filter(filters, 0);
            f.resonance_cc.push(m);
        }
        "resonance2" => {
            let f = filter(filters, 1);
            f.resonance_cc.push(m);
        }
        "fil_gain" | "fil1_gain" => {
            let f = filter(filters, 0);
            f.gain_cc.push(m);
        }
        "fil2_gain" => {
            let f = filter(filters, 1);
            f.gain_cc.push(m);
        }
        _ => {
            let _ = control;
            // Envelopes (`ampeg_attackccN`, `fileg_depth_onccN`, …).
            for (prefix, eg) in [
                ("ampeg_", &mut z.ampeg),
                ("fileg_", &mut z.fileg),
                ("pitcheg_", &mut z.pitcheg),
            ] {
                if let Some(stage) = name.strip_prefix(prefix) {
                    let param = match stage {
                        "delay" => EgParam::Delay,
                        "start" => EgParam::Start,
                        "attack" => EgParam::Attack,
                        "hold" => EgParam::Hold,
                        "decay" => EgParam::Decay,
                        "sustain" => EgParam::Sustain,
                        "release" => EgParam::Release,
                        "depth" => EgParam::Depth,
                        _ => return,
                    };
                    let m = if matches!(param, EgParam::Start | EgParam::Sustain) {
                        pct(m)
                    } else {
                        m
                    };
                    eg.cc.push((param, m));
                    eg.set = true;
                    return;
                }
            }
            // LFOs (`amplfo_depthccN`, `pitchlfo_freq_onccN`, …).
            for (prefix, lfo) in [
                ("amplfo_", &mut z.amplfo),
                ("fillfo_", &mut z.fillfo),
                ("pitchlfo_", &mut z.pitchlfo),
            ] {
                match name.strip_prefix(prefix) {
                    Some("depth") => {
                        lfo.depth_cc.push(m);
                        return;
                    }
                    Some("freq") => {
                        lfo.freq_cc.push(m);
                        return;
                    }
                    _ => {}
                }
            }
            // EQ (`eq1_gainccN`, …).
            for (b, band) in z.eq.iter_mut().enumerate() {
                let p = format!("eq{}_", b + 1);
                match name.strip_prefix(p.as_str()) {
                    Some("freq") => band.freq_cc.push(m),
                    Some("bw") => band.bw_cc.push(m),
                    Some("gain") => band.gain_cc.push(m),
                    _ => {}
                }
            }
        }
    }
}

/// `ampeg_*`, `fileg_*`, `pitcheg_*` (and SFZ 1's `amp_*` aliases).
fn eg_opcode(z: &mut Zone, k: &str, value: Option<f32>) -> bool {
    let (eg, stage) = if let Some(s) = k.strip_prefix("ampeg_") {
        (&mut z.ampeg, s)
    } else if let Some(s) = k.strip_prefix("fileg_") {
        (&mut z.fileg, s)
    } else if let Some(s) = k.strip_prefix("pitcheg_") {
        (&mut z.pitcheg, s)
    } else {
        return false;
    };
    let Some(v) = value else {
        return split_cc(k).is_none();
    };
    let field: &mut f32 = match stage {
        "delay" => &mut eg.delay,
        "start" => {
            eg.start = v / 100.0;
            eg.set = true;
            return true;
        }
        "attack" => &mut eg.attack,
        "hold" => &mut eg.hold,
        "decay" => &mut eg.decay,
        "sustain" => {
            eg.sustain = v / 100.0;
            eg.set = true;
            return true;
        }
        "release" => &mut eg.release,
        "depth" => &mut eg.depth,
        "vel2delay" => &mut eg.vel2delay,
        "vel2attack" => &mut eg.vel2attack,
        "vel2hold" => &mut eg.vel2hold,
        "vel2decay" => &mut eg.vel2decay,
        "vel2sustain" => {
            eg.vel2sustain = v / 100.0;
            eg.set = true;
            return true;
        }
        "vel2release" => &mut eg.vel2release,
        "vel2depth" => &mut eg.vel2depth,
        _ => return false,
    };
    *field = v;
    eg.set = true;
    true
}

/// `amplfo_*`, `fillfo_*`, `pitchlfo_*` (their `chanaft`/`polyaft` forms
/// as controllers 129/130).
fn lfo_opcode(z: &mut Zone, k: &str, value: Option<f32>) -> bool {
    let (lfo, what) = if let Some(s) = k.strip_prefix("amplfo_") {
        (&mut z.amplfo, s)
    } else if let Some(s) = k.strip_prefix("fillfo_") {
        (&mut z.fillfo, s)
    } else if let Some(s) = k.strip_prefix("pitchlfo_") {
        (&mut z.pitchlfo, s)
    } else {
        return false;
    };
    let v = value.unwrap_or(0.0);
    match what {
        "delay" => lfo.delay = v,
        "fade" => lfo.fade = v,
        "freq" => lfo.freq = v,
        "depth" => lfo.depth = v,
        "depthchanaft" => lfo.depth_cc.push(cc_mod(CC_CHANAFT, value)),
        "depthpolyaft" => lfo.depth_cc.push(cc_mod(CC_POLYAFT, value)),
        "freqchanaft" => lfo.freq_cc.push(cc_mod(CC_CHANAFT, value)),
        "freqpolyaft" => lfo.freq_cc.push(cc_mod(CC_POLYAFT, value)),
        _ => return false,
    }
    true
}

/// `eqN_freq`, `eqN_bw`, `eqN_gain`, `eqN_vel2freq`, `eqN_vel2gain`.
fn eq_opcode(z: &mut Zone, k: &str, value: Option<f32>) -> bool {
    let Some(rest) = k.strip_prefix("eq") else {
        return false;
    };
    let Some((n, what)) = rest.split_once('_') else {
        return false;
    };
    let Some(band) = n
        .parse::<usize>()
        .ok()
        .and_then(|n| n.checked_sub(1))
        .and_then(|i| z.eq.get_mut(i))
    else {
        return false;
    };
    let v = value.unwrap_or(0.0);
    match what {
        "freq" => band.freq = v,
        "bw" => band.bw = v,
        "gain" => band.gain = v,
        "vel2freq" => band.vel2freq = v,
        "vel2gain" => band.vel2gain = v,
        _ => return false,
    }
    true
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn defines_includes_and_comments() {
        let d = std::env::temp_dir().join(format!("ff-sfz-pre-{}", std::process::id()));
        std::fs::create_dir_all(d.join("inc")).unwrap();
        std::fs::write(d.join("inc/part.sfz"), "<region> sample=$S key=$K\n").unwrap();
        let text = "/* a block\ncomment */ #define $S tone.wav\n#define $K 62\n// a line\n#include \"inc/part.sfz\"\n<region>sample=x.wav /* inline */ key=c4";
        let (pre, _) = preprocess(text, &d.join("main.sfz"));
        let h = headers(&pre);
        assert_eq!(h.len(), 2);
        assert_eq!(
            h[0].1,
            vec![
                ("sample".to_string(), "tone.wav".to_string()),
                ("key".to_string(), "62".to_string())
            ]
        );
        assert_eq!(h[1].1[1], ("key".to_string(), "c4".to_string()));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn controller_opcodes_and_curves() {
        assert_eq!(split_cc("cutoff_oncc74"), Some(("cutoff", 74, false)));
        assert_eq!(split_cc("cutoff_cc1"), Some(("cutoff", 1, false)));
        assert_eq!(
            split_cc("amplfo_depthcc1"),
            Some(("amplfo_depth", 1, false))
        );
        assert_eq!(split_cc("volume_curvecc7"), Some(("volume", 7, true)));
        assert_eq!(split_cc("loop_end"), None);
        let control = Control::default();
        let mut inst = Instrument::default();
        read_curve(
            &mut inst,
            &vec![
                ("curve_index".into(), "7".into()),
                ("v000".into(), "0".into()),
                ("v127".into(), "0.5".into()),
            ],
        );
        assert!((inst.curves[7][127] - 0.5).abs() < 1e-6);
        let ops: Vec<(String, String)> = [
            ("fil_type", "hpf_2p"),
            ("cutoff", "500"),
            ("cutoff_oncc74", "1200"),
            ("cutoff_curvecc74", "7"),
            ("volume_oncc7", "-12"),
            ("ampeg_attack", "0.2"),
            ("ampeg_attack_oncc1", "1.0"),
            ("fileg_depth", "2400"),
            ("pitchlfo_freq", "5"),
            ("pitchlfo_depth", "50"),
            ("amp_velcurve_64", "1"),
            ("eq2_gain", "6"),
            ("sw_lokey", "c1"),
            ("sw_hikey", "b1"),
            ("sw_last", "c1"),
            ("lohdcc1", "0.5"),
            ("locc64", "64"),
            ("trigger", "release_key"),
            ("xfin_lovel", "0"),
            ("xfin_hivel", "64"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let refs: Vec<&(String, String)> = ops.iter().collect();
        let z = zone(0, &refs, &control, &inst);
        let f = z.filters[0].as_ref().unwrap();
        assert_eq!((f.kind, f.cutoff), (FilterKind::Hp2, 500.0));
        assert_eq!(
            f.cutoff_cc,
            vec![CcMod {
                cc: 74,
                amount: 1200.0,
                curve: Some(7)
            }]
        );
        assert_eq!(z.volume_cc[0].amount, -12.0);
        assert_eq!(z.ampeg.attack, 0.2);
        assert_eq!(z.ampeg.cc, vec![(EgParam::Attack, cc_mod(1, Some(1.0)))]);
        assert_eq!(z.fileg.depth, 2400.0);
        assert!(z.pitchlfo.active());
        let curve = z.velcurve.as_ref().unwrap();
        assert!((curve[32] - 0.5).abs() < 0.01 && (curve[100] - 1.0).abs() < 1e-6);
        assert!(z.eq_active());
        assert!(z.is_keyswitch(24) && !z.is_keyswitch(36));
        assert_eq!(z.sw_last, Some(24));
        assert_eq!(z.cc_ranges, vec![(1, 64, 127), (64, 64, 127)]);
        assert_eq!(z.trigger, Trigger::ReleaseKey);
        assert_eq!(
            z.xfades,
            vec![Xfade {
                cc: CC_VELOCITY,
                lo: 0.0,
                hi: 64.0,
                fade_in: true
            }]
        );
    }

    /// Opt-in: every `.sfz` under `FADERFRAME_TEST_SFZ_DIR` reads without
    /// a panic (missing samples are reported).
    #[test]
    #[ignore]
    fn real_world_files_read() {
        let Ok(dir) = std::env::var("FADERFRAME_TEST_SFZ_DIR") else {
            return;
        };
        let mut stack = vec![PathBuf::from(dir)];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().is_some_and(|x| x == "sfz") {
                    let text = String::from_utf8_lossy(&std::fs::read(&p).unwrap()).into_owned();
                    let mut set = SampleSet::default();
                    load(&mut set, &text, &p);
                    println!(
                        "{}: {} zones, {} samples, {} errors",
                        p.display(),
                        set.zones.len(),
                        set.samples.len(),
                        set.errors.len()
                    );
                }
            }
        }
    }

    #[test]
    fn generators_make_waveforms() {
        let s = generated("sine").unwrap();
        assert_eq!(s.frames, 2048);
        let c4 = s.rate / 2048.0;
        assert!((c4 - 261.6256).abs() < 0.01);
        assert!(generated("noise").unwrap().peak > 0.9);
        assert!(generated("bogus").is_none());
    }
}
