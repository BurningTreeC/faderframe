//! XML to the model, leniently: elements in any order, unknown elements
//! and attributes that do not read noted (once each) and left out, never
//! an error unless the document is not XML or not a project.

use super::*;
use crate::{InterchangeError, Result};
use roxmltree::Node;
use std::collections::BTreeSet;

#[derive(Default)]
struct Noted(BTreeSet<String>);

impl Noted {
    fn note(&mut self, text: String) {
        self.0.insert(text);
    }

    fn unknown(&mut self, parent: Node, child: Node) {
        self.note(format!(
            "<{}> in <{}> was left out",
            child.tag_name().name(),
            parent.tag_name().name()
        ));
    }
}

fn elements<'a, 'i>(n: Node<'a, 'i>) -> impl Iterator<Item = Node<'a, 'i>> {
    n.children().filter(|c| c.is_element())
}

fn child<'a, 'i>(n: Node<'a, 'i>, name: &str) -> Option<Node<'a, 'i>> {
    elements(n).find(|c| c.has_tag_name(name))
}

fn text<'a>(n: Node<'a, '_>, key: &str) -> Option<&'a str> {
    n.attribute(key)
}

fn string(n: Node, key: &str) -> Option<String> {
    n.attribute(key).map(str::to_string)
}

/// A number as the schema writes doubles (and DAWproject's `inf`).
pub(crate) fn parse_number(s: &str) -> Option<f64> {
    let s = s.trim();
    match s {
        "inf" | "INF" | "Infinity" | "+inf" => Some(f64::INFINITY),
        "-inf" | "-INF" | "-Infinity" => Some(f64::NEG_INFINITY),
        "NaN" | "nan" | "null" | "" => None,
        _ => s.parse::<f64>().ok(),
    }
}

impl Noted {
    fn number(&mut self, n: Node, key: &str) -> Option<f64> {
        let s = n.attribute(key)?;
        let v = parse_number(s);
        if v.is_none() && !matches!(s.trim(), "NaN" | "nan" | "null" | "") {
            self.note(format!(
                "<{} {key}=\"{s}\"> is no number",
                n.tag_name().name()
            ));
        }
        v
    }

    fn integer(&mut self, n: Node, key: &str) -> Option<i64> {
        let s = n.attribute(key)?;
        let v = s.trim().parse::<i64>().ok().or_else(|| {
            parse_number(s)
                .filter(|f| f.is_finite())
                .map(|f| f.round() as i64)
        });
        if v.is_none() {
            self.note(format!(
                "<{} {key}=\"{s}\"> is no whole number",
                n.tag_name().name()
            ));
        }
        v
    }

    fn boolean(&mut self, n: Node, key: &str) -> Option<bool> {
        let s = n.attribute(key)?;
        match s.trim() {
            "true" | "1" => Some(true),
            "false" | "0" => Some(false),
            _ => {
                self.note(format!(
                    "<{} {key}=\"{s}\"> is neither true nor false",
                    n.tag_name().name()
                ));
                None
            }
        }
    }

    fn named<T>(&mut self, n: Node, key: &str, parse: fn(&str) -> Option<T>) -> Option<T> {
        let s = n.attribute(key)?;
        let v = parse(s);
        if v.is_none() {
            self.note(format!(
                "<{} {key}=\"{s}\"> is not understood",
                n.tag_name().name()
            ));
        }
        v
    }

    fn meta(&mut self, n: Node) -> Meta {
        Meta {
            id: string(n, "id"),
            name: string(n, "name"),
            color: string(n, "color"),
            comment: string(n, "comment"),
        }
    }

    fn parameter_id(&mut self, n: Node) -> Option<i32> {
        // VST3 and CLAP ids are unsigned: files may write them either way.
        self.integer(n, "parameterID").map(|v| v as i32)
    }

    fn real(&mut self, n: Node) -> RealParameter {
        let unit = self.named(n, "unit", Unit::parse).unwrap_or(Unit::Linear);
        RealParameter {
            meta: self.meta(n),
            parameter_id: self.parameter_id(n),
            unit,
            min: self.number(n, "min"),
            max: self.number(n, "max"),
            value: self.number(n, "value"),
        }
    }

    fn bool_param(&mut self, n: Node) -> BoolParameter {
        BoolParameter {
            meta: self.meta(n),
            parameter_id: self.parameter_id(n),
            value: self.boolean(n, "value"),
        }
    }

    fn time_signature(&mut self, n: Node) -> TimeSignatureParameter {
        TimeSignatureParameter {
            meta: self.meta(n),
            parameter_id: self.parameter_id(n),
            numerator: self.integer(n, "numerator").unwrap_or(4),
            denominator: self.integer(n, "denominator").unwrap_or(4),
        }
    }

    /// A parameter element by its type (`kind` = the element's own name,
    /// or the type it stands for when its name is a role like `Volume`).
    fn parameter(&mut self, n: Node, kind: &str) -> Option<Parameter> {
        Some(match kind {
            "RealParameter" => Parameter::Real(self.real(n)),
            "BoolParameter" => Parameter::Bool(self.bool_param(n)),
            "IntegerParameter" => Parameter::Integer(IntegerParameter {
                meta: self.meta(n),
                parameter_id: self.parameter_id(n),
                min: self.integer(n, "min"),
                max: self.integer(n, "max"),
                value: self.integer(n, "value"),
            }),
            "EnumParameter" => Parameter::Enum(EnumParameter {
                meta: self.meta(n),
                parameter_id: self.parameter_id(n),
                count: self.integer(n, "count").unwrap_or(0),
                labels: text(n, "labels")
                    .map(|l| l.split_whitespace().map(str::to_string).collect())
                    .unwrap_or_default(),
                value: self.integer(n, "value"),
            }),
            "TimeSignatureParameter" => Parameter::TimeSignature(self.time_signature(n)),
            _ => return None,
        })
    }

    fn file_reference(&mut self, n: Node) -> FileReference {
        FileReference {
            path: string(n, "path").unwrap_or_default(),
            external: self.boolean(n, "external"),
        }
    }

    fn device(&mut self, n: Node, kind: DeviceKind) -> Device {
        let role = self
            .named(n, "deviceRole", DeviceRole::parse)
            .unwrap_or(DeviceRole::AudioFx);
        let mut d = Device::new(
            kind,
            self.meta(n),
            role,
            string(n, "deviceName").unwrap_or_default(),
        );
        d.loaded = self.boolean(n, "loaded");
        d.device_id = string(n, "deviceID");
        d.vendor = string(n, "deviceVendor");
        d.plugin_version = string(n, "pluginVersion");
        for c in elements(n) {
            let name = c.tag_name().name();
            match name {
                "Parameters" => {
                    for p in elements(c) {
                        match self.parameter(p, p.tag_name().name()) {
                            Some(x) => d.parameters.push(x),
                            None => self.unknown(c, p),
                        }
                    }
                }
                "Enabled" => d.enabled = Some(self.bool_param(c)),
                "State" => d.state = Some(self.file_reference(c)),
                "Band" if kind == DeviceKind::Equalizer => {
                    let Some(freq) = child(c, "Freq") else {
                        self.note("an equalizer band without a frequency was left out".into());
                        continue;
                    };
                    let band = EqBand {
                        kind: self
                            .named(c, "type", EqBandType::parse)
                            .unwrap_or(EqBandType::Bell),
                        order: self.integer(c, "order"),
                        freq: self.real(freq),
                        gain: child(c, "Gain").map(|g| self.real(g)),
                        q: child(c, "Q").map(|q| self.real(q)),
                        enabled: child(c, "Enabled").map(|e| self.bool_param(e)),
                    };
                    d.bands.push(band);
                }
                own if kind.own_parameters().contains(&own) => {
                    let p = if own == "AutoMakeup" {
                        Parameter::Bool(self.bool_param(c))
                    } else {
                        Parameter::Real(self.real(c))
                    };
                    d.own.push((own.to_string(), p));
                }
                _ => self.unknown(n, c),
            }
        }
        d
    }

    fn channel(&mut self, n: Node) -> Channel {
        let mut c = Channel::new(self.meta(n));
        c.role = self.named(n, "role", MixerRole::parse);
        c.audio_channels = self.integer(n, "audioChannels");
        c.solo = self.boolean(n, "solo");
        c.destination = string(n, "destination");
        for e in elements(n) {
            match e.tag_name().name() {
                "Devices" => {
                    for d in elements(e) {
                        match DeviceKind::of_element(d.tag_name().name()) {
                            Some(k) => c.devices.push(self.device(d, k)),
                            None => self.unknown(e, d),
                        }
                    }
                }
                "Mute" => c.mute = Some(self.bool_param(e)),
                "Pan" => c.pan = Some(self.real(e)),
                "Volume" => c.volume = Some(self.real(e)),
                "Sends" => {
                    for s in elements(e) {
                        if !s.has_tag_name("Send") {
                            self.unknown(e, s);
                            continue;
                        }
                        let volume =
                            child(s, "Volume").map(|v| self.real(v)).unwrap_or_else(|| {
                                RealParameter::new(Meta::default(), Unit::Linear, 1.0)
                            });
                        c.sends.push(Send {
                            meta: self.meta(s),
                            destination: string(s, "destination"),
                            kind: self.named(s, "type", SendType::parse),
                            enable: child(s, "Enable").map(|x| self.bool_param(x)),
                            pan: child(s, "Pan").map(|x| self.real(x)),
                            volume,
                        });
                    }
                }
                _ => self.unknown(n, e),
            }
        }
        c
    }

    fn track(&mut self, n: Node) -> Track {
        let content_types = text(n, "contentType")
            .map(|s| {
                s.split(|c: char| c.is_whitespace() || c == ',')
                    .filter(|t| !t.is_empty())
                    .filter_map(|t| {
                        let v = ContentType::parse(t);
                        if v.is_none() {
                            self.note(format!("track content type \"{t}\" is not understood"));
                        }
                        v
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut t = Track {
            meta: self.meta(n),
            content_types,
            loaded: self.boolean(n, "loaded"),
            channel: None,
            tracks: Vec::new(),
        };
        for e in elements(n) {
            match e.tag_name().name() {
                "Channel" => t.channel = Some(self.channel(e)),
                "Track" => t.tracks.push(self.track(e)),
                _ => self.unknown(n, e),
            }
        }
        t
    }

    fn timeline_meta(&mut self, n: Node) -> TimelineMeta {
        TimelineMeta {
            meta: self.meta(n),
            time_unit: self.named(n, "timeUnit", TimeUnit::parse),
            track: string(n, "track"),
        }
    }

    fn clip(&mut self, n: Node) -> Clip {
        let mut content = None;
        for e in elements(n) {
            match self.timeline(e) {
                Some(t) if content.is_none() => content = Some(Box::new(t)),
                Some(_) => self.note("a clip with more than one timeline kept its first".into()),
                None => self.unknown(n, e),
            }
        }
        Clip {
            meta: self.meta(n),
            time: self.number(n, "time").unwrap_or(0.0),
            duration: self.number(n, "duration"),
            content_time_unit: self.named(n, "contentTimeUnit", TimeUnit::parse),
            play_start: self.number(n, "playStart"),
            play_stop: self.number(n, "playStop"),
            loop_start: self.number(n, "loopStart"),
            loop_end: self.number(n, "loopEnd"),
            fade_time_unit: self.named(n, "fadeTimeUnit", TimeUnit::parse),
            fade_in_time: self.number(n, "fadeInTime"),
            fade_out_time: self.number(n, "fadeOutTime"),
            enable: self.boolean(n, "enable"),
            reference: string(n, "reference"),
            content,
        }
    }

    fn points(&mut self, n: Node) -> Points {
        let mut p = Points {
            common: self.timeline_meta(n),
            unit: self.named(n, "unit", Unit::parse),
            ..Points::default()
        };
        for e in elements(n) {
            let time = self.number(e, "time").unwrap_or(0.0);
            match e.tag_name().name() {
                "Target" => {
                    p.target = AutomationTarget {
                        parameter: string(e, "parameter"),
                        expression: self.named(e, "expression", ExpressionType::parse),
                        channel: self.integer(e, "channel"),
                        key: self.integer(e, "key"),
                        controller: self.integer(e, "controller"),
                    }
                }
                "RealPoint" => {
                    if let Some(value) = self.number(e, "value") {
                        p.points.push(Point::Real {
                            time,
                            value,
                            interpolation: self.named(e, "interpolation", Interpolation::parse),
                        });
                    }
                }
                "BoolPoint" => {
                    if let Some(value) = self.boolean(e, "value") {
                        p.points.push(Point::Bool { time, value });
                    }
                }
                "IntegerPoint" => {
                    if let Some(value) = self.integer(e, "value") {
                        p.points.push(Point::Integer { time, value });
                    }
                }
                "EnumPoint" => {
                    if let Some(value) = self.integer(e, "value") {
                        p.points.push(Point::Enum { time, value });
                    }
                }
                "TimeSignaturePoint" => p.points.push(Point::TimeSignature {
                    time,
                    numerator: self.integer(e, "numerator").unwrap_or(4),
                    denominator: self.integer(e, "denominator").unwrap_or(4),
                }),
                _ => self.unknown(n, e),
            }
        }
        p
    }

    fn markers(&mut self, n: Node) -> Markers {
        let mut m = Markers {
            common: self.timeline_meta(n),
            markers: Vec::new(),
        };
        for e in elements(n) {
            if e.has_tag_name("Marker") {
                m.markers.push(Marker {
                    meta: self.meta(e),
                    time: self.number(e, "time").unwrap_or(0.0),
                });
            } else {
                self.unknown(n, e);
            }
        }
        m
    }

    fn media(&mut self, n: Node) -> Option<MediaFile> {
        let Some(file) = child(n, "File") else {
            self.note(format!(
                "<{}> without a file was left out",
                n.tag_name().name()
            ));
            return None;
        };
        Some(MediaFile {
            common: self.timeline_meta(n),
            duration: self.number(n, "duration").unwrap_or(0.0),
            file: self.file_reference(file),
            algorithm: string(n, "algorithm"),
            channels: self.integer(n, "channels"),
            sample_rate: self.integer(n, "sampleRate"),
        })
    }

    fn lanes(&mut self, n: Node) -> Lanes {
        let mut l = Lanes {
            common: self.timeline_meta(n),
            lanes: Vec::new(),
        };
        for e in elements(n) {
            match self.timeline(e) {
                Some(t) => l.lanes.push(t),
                None => self.unknown(n, e),
            }
        }
        l
    }

    /// A timeline element (`None`: not one).
    fn timeline(&mut self, n: Node) -> Option<Timeline> {
        Some(match n.tag_name().name() {
            "Lanes" => Timeline::Lanes(self.lanes(n)),
            "Clips" => {
                let mut c = Clips {
                    common: self.timeline_meta(n),
                    clips: Vec::new(),
                };
                for e in elements(n) {
                    if e.has_tag_name("Clip") {
                        c.clips.push(self.clip(e));
                    } else {
                        self.unknown(n, e);
                    }
                }
                Timeline::Clips(c)
            }
            "Notes" => {
                let mut notes = Notes {
                    common: self.timeline_meta(n),
                    notes: Vec::new(),
                };
                for e in elements(n) {
                    if !e.has_tag_name("Note") {
                        self.unknown(n, e);
                        continue;
                    }
                    let mut content = None;
                    for x in elements(e) {
                        match self.timeline(x) {
                            Some(t) if content.is_none() => content = Some(Box::new(t)),
                            _ => self.unknown(e, x),
                        }
                    }
                    let Some(key) = self.integer(e, "key") else {
                        continue;
                    };
                    notes.notes.push(Note {
                        time: self.number(e, "time").unwrap_or(0.0),
                        duration: self.number(e, "duration").unwrap_or(0.0),
                        channel: self.integer(e, "channel").unwrap_or(0),
                        key,
                        vel: self.number(e, "vel"),
                        rel: self.number(e, "rel"),
                        content,
                    });
                }
                Timeline::Notes(notes)
            }
            "Points" => Timeline::Points(self.points(n)),
            "Warps" => {
                let mut content = None;
                let mut warps = Vec::new();
                for e in elements(n) {
                    if e.has_tag_name("Warp") {
                        if let (Some(time), Some(content_time)) =
                            (self.number(e, "time"), self.number(e, "contentTime"))
                        {
                            warps.push(Warp { time, content_time });
                        }
                    } else {
                        match self.timeline(e) {
                            Some(t) if content.is_none() => content = Some(Box::new(t)),
                            _ => self.unknown(n, e),
                        }
                    }
                }
                Timeline::Warps(Warps {
                    common: self.timeline_meta(n),
                    content_time_unit: self
                        .named(n, "contentTimeUnit", TimeUnit::parse)
                        .unwrap_or(TimeUnit::Seconds),
                    content,
                    warps,
                })
            }
            "Audio" => Timeline::Audio(self.media(n)?),
            "Video" => Timeline::Video(self.media(n)?),
            "Markers" | "markers" => Timeline::Markers(self.markers(n)),
            "ClipSlot" => Timeline::ClipSlot(ClipSlot {
                common: self.timeline_meta(n),
                has_stop: self.boolean(n, "hasStop"),
                clip: child(n, "Clip").map(|c| self.clip(c)),
            }),
            _ => return None,
        })
    }
}

fn parse(xml: &str) -> Result<roxmltree::Document<'_>> {
    roxmltree::Document::parse_with_options(
        xml,
        roxmltree::ParsingOptions {
            allow_dtd: true,
            ..Default::default()
        },
    )
    .map_err(|e| InterchangeError::Xml(e.to_string()))
}

/// Read `project.xml`; the notes say what was left out.
pub fn read_project(xml: &str) -> Result<(Project, Vec<String>)> {
    let doc = parse(xml)?;
    let root = doc.root_element();
    if !root.has_tag_name("Project") {
        return Err(InterchangeError::Format(format!(
            "<{}> is not a DAWproject project",
            root.tag_name().name()
        )));
    }
    let mut r = Noted::default();
    let mut p = Project {
        version: string(root, "version").unwrap_or_default(),
        application: Application {
            name: String::new(),
            version: String::new(),
        },
        transport: None,
        structure: Vec::new(),
        arrangement: None,
        scenes: Vec::new(),
    };
    if !p.version.starts_with("1.") {
        r.note(format!(
            "made for DAWproject {} (this reads 1.x)",
            if p.version.is_empty() {
                "?"
            } else {
                &p.version
            }
        ));
    }
    for e in elements(root) {
        match e.tag_name().name() {
            "Application" => {
                p.application = Application {
                    name: string(e, "name").unwrap_or_default(),
                    version: string(e, "version").unwrap_or_default(),
                }
            }
            "Transport" => {
                let mut t = Transport::default();
                for x in elements(e) {
                    match x.tag_name().name() {
                        "Tempo" => t.tempo = Some(r.real(x)),
                        "TimeSignature" => t.time_signature = Some(r.time_signature(x)),
                        _ => r.unknown(e, x),
                    }
                }
                p.transport = Some(t);
            }
            "Structure" => {
                for x in elements(e) {
                    match x.tag_name().name() {
                        "Track" => p.structure.push(Lane::Track(r.track(x))),
                        "Channel" => p.structure.push(Lane::Channel(r.channel(x))),
                        _ => r.unknown(e, x),
                    }
                }
            }
            "Arrangement" => {
                let mut a = Arrangement {
                    meta: r.meta(e),
                    ..Arrangement::default()
                };
                for x in elements(e) {
                    match x.tag_name().name() {
                        "Lanes" => a.lanes = Some(r.lanes(x)),
                        "Markers" | "markers" => a.markers = Some(r.markers(x)),
                        "TempoAutomation" => a.tempo_automation = Some(r.points(x)),
                        "TimeSignatureAutomation" => {
                            a.time_signature_automation = Some(r.points(x));
                        }
                        _ => r.unknown(e, x),
                    }
                }
                p.arrangement = Some(a);
            }
            "Scenes" => {
                for x in elements(e) {
                    if !x.has_tag_name("Scene") {
                        r.unknown(e, x);
                        continue;
                    }
                    let content = elements(x).find_map(|c| r.timeline(c));
                    p.scenes.push(Scene {
                        meta: r.meta(x),
                        content,
                    });
                }
            }
            _ => r.unknown(root, e),
        }
    }
    Ok((p, r.0.into_iter().collect()))
}

/// Read `metadata.xml`.
pub fn read_metadata(xml: &str) -> Result<MetaData> {
    let doc = parse(xml)?;
    let mut m = MetaData::default();
    for e in elements(doc.root_element()) {
        let value = e.text().unwrap_or("").trim().to_string();
        if let Some(f) = m.field_mut(e.tag_name().name())
            && !value.is_empty()
        {
            *f = Some(value);
        }
    }
    Ok(m)
}
