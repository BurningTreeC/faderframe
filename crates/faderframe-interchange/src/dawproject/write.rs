//! The model as XML, in the schema's element order (an extension's
//! elements after its base's).

use super::*;
use crate::xml::{Element, document};

/// A double as `xs:double` writes it (the shortest text that reads back
/// the same; infinities as the schema spells them).
pub(crate) fn num(v: f64) -> String {
    if v.is_nan() {
        "NaN".into()
    } else if v.is_infinite() {
        if v > 0.0 { "INF".into() } else { "-INF".into() }
    } else if v == 0.0 {
        // No "-0".
        "0".into()
    } else {
        format!("{v}")
    }
}

/// A real parameter's value (`xs:string`: DAWproject writes infinities as
/// `inf`).
fn value_text(v: f64) -> String {
    if v.is_infinite() {
        if v > 0.0 { "inf".into() } else { "-inf".into() }
    } else {
        num(v)
    }
}

fn boolean(b: bool) -> &'static str {
    if b { "true" } else { "false" }
}

fn with_meta(e: Element, m: &Meta, id: bool) -> Element {
    let e = if id {
        e.attr_opt("id", m.id.as_deref())
    } else {
        e
    };
    e.attr_opt("name", m.name.as_deref())
        .attr_opt("color", m.color.as_deref())
        .attr_opt("comment", m.comment.as_deref())
}

fn real(name: &str, p: &RealParameter) -> Element {
    with_meta(Element::new(name), &p.meta, true)
        .attr_opt("parameterID", p.parameter_id)
        .attr("unit", p.unit)
        .attr_opt("min", p.min.map(num))
        .attr_opt("max", p.max.map(num))
        .attr_opt("value", p.value.map(value_text))
}

fn boolean_param(name: &str, p: &BoolParameter) -> Element {
    with_meta(Element::new(name), &p.meta, true)
        .attr_opt("parameterID", p.parameter_id)
        .attr_opt("value", p.value.map(boolean))
}

fn time_signature(name: &str, p: &TimeSignatureParameter) -> Element {
    with_meta(Element::new(name), &p.meta, true)
        .attr_opt("parameterID", p.parameter_id)
        .attr("numerator", p.numerator)
        .attr("denominator", p.denominator)
}

fn parameter(name: Option<&str>, p: &Parameter) -> Element {
    match p {
        Parameter::Real(r) => real(name.unwrap_or("RealParameter"), r),
        Parameter::Bool(b) => boolean_param(name.unwrap_or("BoolParameter"), b),
        Parameter::Integer(i) => with_meta(
            Element::new(name.unwrap_or("IntegerParameter")),
            &i.meta,
            true,
        )
        .attr_opt("parameterID", i.parameter_id)
        .attr_opt("min", i.min)
        .attr_opt("max", i.max)
        .attr_opt("value", i.value),
        Parameter::Enum(e) => {
            with_meta(Element::new(name.unwrap_or("EnumParameter")), &e.meta, true)
                .attr_opt("parameterID", e.parameter_id)
                .attr("count", e.count)
                .attr_opt(
                    "labels",
                    (!e.labels.is_empty()).then(|| {
                        // A list of tokens: spaces inside a label cannot be kept.
                        e.labels
                            .iter()
                            .map(|l| l.replace(char::is_whitespace, "_"))
                            .collect::<Vec<_>>()
                            .join(" ")
                    }),
                )
                .attr_opt("value", e.value)
        }
        Parameter::TimeSignature(t) => time_signature(name.unwrap_or("TimeSignatureParameter"), t),
    }
}

fn file_reference(name: &str, f: &FileReference) -> Element {
    Element::new(name)
        .attr("path", &f.path)
        .attr_opt("external", f.external.map(boolean))
}

fn device(d: &Device) -> Element {
    let mut e = with_meta(Element::new(d.kind.element()), &d.meta, true)
        .attr("deviceRole", d.role)
        .attr_opt("loaded", d.loaded.map(boolean))
        .attr("deviceName", &d.device_name)
        .attr_opt("deviceID", d.device_id.as_deref())
        .attr_opt("deviceVendor", d.vendor.as_deref());
    if d.kind.is_plugin() {
        e = e.attr_opt("pluginVersion", d.plugin_version.as_deref());
    }
    if !d.parameters.is_empty() {
        let mut ps = Element::new("Parameters");
        for p in &d.parameters {
            ps.push(parameter(None, p));
        }
        e.push(ps);
    }
    if let Some(en) = &d.enabled {
        e.push(boolean_param("Enabled", en));
    }
    if let Some(s) = &d.state {
        e.push(file_reference("State", s));
    }
    if d.kind == DeviceKind::Equalizer {
        for b in &d.bands {
            let band = Element::new("Band")
                .attr("type", b.kind)
                .attr_opt("order", b.order)
                .child(real("Freq", &b.freq))
                .child_opt(b.gain.as_ref().map(|g| real("Gain", g)))
                .child_opt(b.q.as_ref().map(|q| real("Q", q)))
                .child_opt(b.enabled.as_ref().map(|x| boolean_param("Enabled", x)));
            e.push(band);
        }
    }
    for name in d.kind.own_parameters() {
        if let Some(p) = d.own(name) {
            e.push(parameter(Some(name), p));
        }
    }
    e
}

fn channel(c: &Channel) -> Element {
    let mut e = with_meta(Element::new("Channel"), &c.meta, true)
        .attr_opt("role", c.role)
        .attr_opt("audioChannels", c.audio_channels)
        .attr_opt("solo", c.solo.map(boolean))
        .attr_opt("destination", c.destination.as_deref());
    if !c.devices.is_empty() {
        let mut ds = Element::new("Devices");
        for d in &c.devices {
            ds.push(device(d));
        }
        e.push(ds);
    }
    if let Some(m) = &c.mute {
        e.push(boolean_param("Mute", m));
    }
    if let Some(p) = &c.pan {
        e.push(real("Pan", p));
    }
    if !c.sends.is_empty() {
        let mut ss = Element::new("Sends");
        for s in &c.sends {
            ss.push(
                with_meta(Element::new("Send"), &s.meta, true)
                    .attr_opt("destination", s.destination.as_deref())
                    .attr_opt("type", s.kind)
                    .child_opt(s.enable.as_ref().map(|x| boolean_param("Enable", x)))
                    .child_opt(s.pan.as_ref().map(|x| real("Pan", x)))
                    .child(real("Volume", &s.volume)),
            );
        }
        e.push(ss);
    }
    if let Some(v) = &c.volume {
        e.push(real("Volume", v));
    }
    e
}

fn track(t: &Track) -> Element {
    let mut e = with_meta(Element::new("Track"), &t.meta, true)
        .attr_opt(
            "contentType",
            (!t.content_types.is_empty()).then(|| {
                t.content_types
                    .iter()
                    .map(|c| c.as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
            }),
        )
        .attr_opt("loaded", t.loaded.map(boolean));
    if let Some(c) = &t.channel {
        e.push(channel(c));
    }
    for c in &t.tracks {
        e.push(track(c));
    }
    e
}

fn timeline_attrs(e: Element, c: &TimelineMeta) -> Element {
    with_meta(e, &c.meta, true)
        .attr_opt("timeUnit", c.time_unit)
        .attr_opt("track", c.track.as_deref())
}

fn clip(c: &Clip) -> Element {
    let e = with_meta(Element::new("Clip"), &c.meta, false)
        .attr("time", num(c.time))
        .attr_opt("duration", c.duration.map(num))
        .attr_opt("contentTimeUnit", c.content_time_unit)
        .attr_opt("playStart", c.play_start.map(num))
        .attr_opt("playStop", c.play_stop.map(num))
        .attr_opt("loopStart", c.loop_start.map(num))
        .attr_opt("loopEnd", c.loop_end.map(num))
        .attr_opt("fadeTimeUnit", c.fade_time_unit)
        .attr_opt("fadeInTime", c.fade_in_time.map(num))
        .attr_opt("fadeOutTime", c.fade_out_time.map(num))
        .attr_opt("enable", c.enable.map(boolean))
        .attr_opt("reference", c.reference.as_deref());
    e.child_opt(c.content.as_deref().and_then(timeline))
}

fn points(name: &str, p: &Points) -> Element {
    let t = &p.target;
    let mut e = timeline_attrs(Element::new(name), &p.common)
        .attr_opt("unit", p.unit)
        .child(
            Element::new("Target")
                .attr_opt("parameter", t.parameter.as_deref())
                .attr_opt("expression", t.expression)
                .attr_opt("channel", t.channel)
                .attr_opt("key", t.key)
                .attr_opt("controller", t.controller),
        );
    for pt in &p.points {
        e.push(match *pt {
            Point::Real {
                time,
                value,
                interpolation,
            } => Element::new("RealPoint")
                .attr("time", num(time))
                .attr("value", value_text(value))
                .attr_opt("interpolation", interpolation),
            Point::Bool { time, value } => Element::new("BoolPoint")
                .attr("time", num(time))
                .attr("value", boolean(value)),
            Point::Integer { time, value } => Element::new("IntegerPoint")
                .attr("time", num(time))
                .attr("value", value),
            Point::Enum { time, value } => Element::new("EnumPoint")
                .attr("time", num(time))
                .attr("value", value),
            Point::TimeSignature {
                time,
                numerator,
                denominator,
            } => Element::new("TimeSignaturePoint")
                .attr("time", num(time))
                .attr("numerator", numerator)
                .attr("denominator", denominator),
        });
    }
    e
}

fn markers(name: &str, m: &Markers) -> Option<Element> {
    // The schema wants at least one marker.
    if m.markers.is_empty() {
        return None;
    }
    let mut e = timeline_attrs(Element::new(name), &m.common);
    for k in &m.markers {
        e.push(with_meta(Element::new("Marker"), &k.meta, false).attr("time", num(k.time)));
    }
    Some(e)
}

fn media(name: &str, m: &MediaFile) -> Element {
    timeline_attrs(Element::new(name), &m.common)
        .attr("duration", num(m.duration))
        .attr_opt("algorithm", m.algorithm.as_deref())
        .attr_opt("channels", m.channels)
        .attr_opt("sampleRate", m.sample_rate)
        .child(file_reference("File", &m.file))
}

fn lanes(name: &str, l: &Lanes) -> Element {
    let mut e = timeline_attrs(Element::new(name), &l.common);
    for t in &l.lanes {
        if let Some(c) = timeline(t) {
            e.push(c);
        }
    }
    e
}

fn timeline(t: &Timeline) -> Option<Element> {
    Some(match t {
        Timeline::Lanes(l) => lanes("Lanes", l),
        Timeline::Clips(c) => {
            let mut e = timeline_attrs(Element::new("Clips"), &c.common);
            for k in &c.clips {
                e.push(clip(k));
            }
            e
        }
        Timeline::Notes(n) => {
            let mut e = timeline_attrs(Element::new("Notes"), &n.common);
            for k in &n.notes {
                e.push(
                    Element::new("Note")
                        .attr("time", num(k.time))
                        .attr("duration", num(k.duration))
                        .attr("channel", k.channel)
                        .attr("key", k.key)
                        .attr_opt("vel", k.vel.map(num))
                        .attr_opt("rel", k.rel.map(num))
                        .child_opt(k.content.as_deref().and_then(timeline)),
                );
            }
            e
        }
        Timeline::Points(p) => points("Points", p),
        Timeline::Warps(w) => {
            let mut e = timeline_attrs(Element::new("Warps"), &w.common)
                .attr("contentTimeUnit", w.content_time_unit)
                .child_opt(w.content.as_deref().and_then(timeline));
            for k in &w.warps {
                e.push(
                    Element::new("Warp")
                        .attr("time", num(k.time))
                        .attr("contentTime", num(k.content_time)),
                );
            }
            e
        }
        Timeline::Audio(m) => media("Audio", m),
        Timeline::Video(m) => media("Video", m),
        Timeline::Markers(m) => markers("markers", m)?,
        Timeline::ClipSlot(s) => timeline_attrs(Element::new("ClipSlot"), &s.common)
            .attr_opt("hasStop", s.has_stop.map(boolean))
            .child_opt(s.clip.as_ref().map(clip)),
    })
}

/// `project.xml`.
pub fn write_project(p: &Project) -> String {
    let mut root = Element::new("Project").attr("version", &p.version).child(
        Element::new("Application")
            .attr("name", &p.application.name)
            .attr("version", &p.application.version),
    );
    if let Some(t) = &p.transport {
        root.push(
            Element::new("Transport")
                .child_opt(t.tempo.as_ref().map(|x| real("Tempo", x)))
                .child_opt(
                    t.time_signature
                        .as_ref()
                        .map(|x| time_signature("TimeSignature", x)),
                ),
        );
    }
    let mut structure = Element::new("Structure");
    for l in &p.structure {
        structure.push(match l {
            Lane::Track(t) => track(t),
            Lane::Channel(c) => channel(c),
        });
    }
    root.push(structure);
    if let Some(a) = &p.arrangement {
        root.push(
            with_meta(Element::new("Arrangement"), &a.meta, true)
                .child_opt(a.lanes.as_ref().map(|l| lanes("Lanes", l)))
                .child_opt(a.markers.as_ref().and_then(|m| markers("Markers", m)))
                .child_opt(
                    a.tempo_automation
                        .as_ref()
                        .map(|t| points("TempoAutomation", t)),
                )
                .child_opt(
                    a.time_signature_automation
                        .as_ref()
                        .map(|t| points("TimeSignatureAutomation", t)),
                ),
        );
    }
    let mut scenes = Element::new("Scenes");
    for s in &p.scenes {
        scenes.push(
            with_meta(Element::new("Scene"), &s.meta, true)
                .child_opt(s.content.as_ref().and_then(timeline)),
        );
    }
    root.push(scenes);
    document(&root)
}

/// `metadata.xml`.
pub fn write_metadata(m: &MetaData) -> String {
    let mut root = Element::new("MetaData");
    for (name, value) in m.fields() {
        if let Some(v) = value {
            root.push(Element::new(name).text(v.clone()));
        }
    }
    document(&root)
}
