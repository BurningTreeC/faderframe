//! Cubase expression maps (`.expressionmap`, XML) read into articulation
//! maps.
//!
//! The file lists articulations as *slot visuals* (`USlotVisuals`: text,
//! description, attribute or direction, group) and *sound slots*
//! (`PSoundSlot`: a name, the visuals it stands for, its output — MIDI
//! messages (`POutputEvent`: note-ons are keyswitches, control changes,
//! program changes) and a note changer (`PSlotNoteChanger`: channel,
//! transpose, velocity and length factors)). A slot standing for one
//! articulation gives that articulation its output; a slot standing for
//! several is a [`Combination`]; a slot standing for none (the "no
//! articulation" sound Cubase plays by default) becomes an articulation
//! of its own — the first one of group 1 when it is the first slot, so it
//! stays the default. Remote keys and colours are left out.

use faderframe_core::ArticulationMapId;
use faderframe_project::articulation::{
    ArticulationKind, ArticulationMap, ArticulationOutput, Combination, ControlValue, GROUPS,
    Keyswitch,
};
use roxmltree::{Document, Node};

fn attr<'a>(n: Node<'a, '_>, key: &str) -> Option<&'a str> {
    n.attribute(key)
}

/// A typed value child (`<int name="x" value="…"/>`, `<string …>`,
/// `<float …>`) of `n`.
fn value<'a>(n: Node<'a, '_>, name: &str) -> Option<&'a str> {
    n.children()
        .find(|c| c.is_element() && attr(*c, "name") == Some(name))
        .and_then(|c| attr(c, "value"))
}

fn int(n: Node, name: &str) -> Option<i64> {
    value(n, name).and_then(|v| v.trim().parse().ok())
}

fn float(n: Node, name: &str) -> Option<f64> {
    value(n, name).and_then(|v| v.trim().parse().ok())
}

/// A `<member name="…">` child of `n`.
fn member<'a, 'i>(n: Node<'a, 'i>, name: &str) -> Option<Node<'a, 'i>> {
    n.children()
        .find(|c| c.has_tag_name("member") && attr(*c, "name") == Some(name))
}

fn objs<'a, 'i>(n: Node<'a, 'i>, class: &'a str) -> impl Iterator<Item = Node<'a, 'i>> + 'a {
    n.descendants()
        .filter(move |c| c.has_tag_name("obj") && attr(*c, "class") == Some(class))
}

/// The output a sound slot sends.
fn slot_output(slot: Node) -> ArticulationOutput {
    let mut out = ArticulationOutput::default();
    for ev in objs(slot, "POutputEvent") {
        let status = int(ev, "status").unwrap_or(0);
        let d1 = int(ev, "data1").unwrap_or(-1);
        let d2 = int(ev, "data2").unwrap_or(-1);
        if !(0..=127).contains(&d1) {
            continue;
        }
        match status & 0xF0 {
            0x90 => out.keyswitches.push(Keyswitch {
                key: d1 as u8,
                velocity: if (1..=127).contains(&d2) {
                    d2 as u8
                } else {
                    100
                },
            }),
            0xB0 => out.controls.push(ControlValue {
                controller: d1 as u8,
                value: d2.clamp(0, 127) as u8,
            }),
            0xC0 => out.program = Some(d1 as u8),
            _ => {}
        }
    }
    if let Some(nc) = objs(slot, "PSlotNoteChanger").next() {
        if let Some(c) = int(nc, "channel").filter(|c| (0..16).contains(c)) {
            out.channel = Some(c as u8);
        }
        out.transpose = int(nc, "transpose").unwrap_or(0).clamp(-48, 48) as i8;
        let percent = |f: Option<f64>| (f.unwrap_or(1.0) * 100.0).round().clamp(1.0, 250.0) as u8;
        out.velocity = percent(float(nc, "velocityFact"));
        out.length = percent(float(nc, "lengthFact"));
    }
    out
}

/// Read a Cubase expression map; the map gets `id`.
pub fn parse(id: ArticulationMapId, text: &str) -> Result<ArticulationMap, String> {
    let doc = Document::parse(text).map_err(|e| format!("not an expression map: {e}"))?;
    let root = doc.root_element();
    if !root.has_tag_name("InstrumentMap") {
        return Err(format!(
            "not an expression map (<{}>)",
            root.tag_name().name()
        ));
    }
    let mut map = ArticulationMap::new(
        id,
        value(root, "name")
            .filter(|n| !n.trim().is_empty())
            .unwrap_or("Expression Map")
            .to_string(),
    );
    // The articulations, by the visuals' ids.
    let mut by_visual: Vec<(String, faderframe_project::articulation::ArticulationId)> = Vec::new();
    if let Some(visuals) = member(root, "slotvisuals") {
        for v in objs(visuals, "USlotVisuals") {
            let Some(vid) = attr(v, "ID") else { continue };
            let text = value(v, "text").unwrap_or("").trim().to_string();
            let description = value(v, "description").unwrap_or("").trim().to_string();
            let name = if description.is_empty() {
                text.clone()
            } else {
                description
            };
            let kind = if int(v, "articulationtype") == Some(1) {
                ArticulationKind::Direction
            } else {
                ArticulationKind::Attribute
            };
            let group = int(v, "group").unwrap_or(0).clamp(0, GROUPS as i64 - 1) as u8;
            let a = map.add(
                if name.is_empty() {
                    "Articulation"
                } else {
                    &name
                },
                text,
                group,
                kind,
                ArticulationOutput::default(),
            );
            by_visual.push((vid.to_string(), a));
        }
    }
    let Some(slots) = member(root, "slots") else {
        map.tidy();
        if map.articulations.is_empty() {
            return Err("the expression map has no articulations".into());
        }
        return Ok(map);
    };
    let mut given = std::collections::BTreeSet::new();
    for (k, slot) in objs(slots, "PSoundSlot").enumerate() {
        let name = member(slot, "name")
            .and_then(|m| value(m, "s"))
            .unwrap_or("")
            .trim()
            .to_string();
        let mut refs = Vec::new();
        for v in objs(slot, "USlotVisuals") {
            let Some(vid) = attr(v, "ID") else { continue };
            if let Some((_, a)) = by_visual.iter().find(|(id, _)| id == vid)
                && !refs.contains(a)
            {
                refs.push(*a);
            }
        }
        let output = slot_output(slot);
        match refs.as_slice() {
            [] => {
                let a = map.add(
                    if name.is_empty() { "Default" } else { &name },
                    name.clone(),
                    0,
                    ArticulationKind::Direction,
                    output,
                );
                if k == 0
                    && let Some(i) = map.articulations.iter().position(|x| x.id == a)
                {
                    let first = map.articulations.remove(i);
                    map.articulations.insert(0, first);
                }
            }
            [one] => {
                if given.insert(*one)
                    && let Some(a) = map.get_mut(*one)
                {
                    a.output = output;
                }
            }
            several => map.combinations.push(Combination {
                articulations: several.to_vec(),
                output,
            }),
        }
    }
    map.tidy();
    if map.articulations.is_empty() {
        return Err("the expression map has no articulations".into());
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A map in the format's shape: a default slot without articulation,
    /// staccato (attribute) and pizzicato (direction) on keyswitches, a
    /// legato direction in group 2 on CC 68, a combination of pizzicato
    /// and legato on its own channel.
    const SAMPLE: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<InstrumentMap>
   <string name="name" value="Strings Test"/>
   <member name="slotvisuals">
      <int name="ownership" value="1"/>
      <list name="obj" type="obj">
         <obj class="USlotVisuals" ID="11">
            <int name="displaytype" value="1"/>
            <int name="articulationtype" value="0"/>
            <int name="symbol" value="0"/>
            <string name="text" value="stacc."/>
            <string name="description" value="Staccato"/>
            <int name="group" value="0"/>
         </obj>
         <obj class="USlotVisuals" ID="12">
            <int name="articulationtype" value="1"/>
            <string name="text" value="pizz."/>
            <string name="description" value="Pizzicato"/>
            <int name="group" value="0"/>
         </obj>
         <obj class="USlotVisuals" ID="13">
            <int name="articulationtype" value="1"/>
            <string name="text" value="leg."/>
            <string name="description" value="Legato"/>
            <int name="group" value="1"/>
         </obj>
      </list>
   </member>
   <member name="slots">
      <int name="ownership" value="1"/>
      <list name="obj" type="obj">
         <obj class="PSoundSlot" ID="20">
            <obj class="PSlotMidiAction" name="action" ID="21">
               <member name="midiMessages">
                  <list name="obj" type="obj">
                     <obj class="POutputEvent" ID="22">
                        <int name="status" value="144"/>
                        <int name="data1" value="24"/>
                        <int name="data2" value="120"/>
                     </obj>
                  </list>
               </member>
            </obj>
            <member name="sv"><list name="s" type="obj"/></member>
            <member name="name"><string name="s" value="Sustain"/></member>
         </obj>
         <obj class="PSoundSlot" ID="30">
            <obj class="PSlotMidiAction" name="action" ID="31">
               <member name="noteChanger">
                  <list name="obj" type="obj">
                     <obj class="PSlotNoteChanger" ID="32">
                        <int name="channel" value="-1"/>
                        <float name="velocityFact" value="0.8"/>
                        <float name="lengthFact" value="0.5"/>
                        <int name="transpose" value="0"/>
                     </obj>
                  </list>
               </member>
               <member name="midiMessages">
                  <list name="obj" type="obj">
                     <obj class="POutputEvent" ID="33">
                        <int name="status" value="144"/>
                        <int name="data1" value="25"/>
                        <int name="data2" value="100"/>
                     </obj>
                  </list>
               </member>
            </obj>
            <member name="sv"><list name="s" type="obj"><obj class="USlotVisuals" ID="11"/></list></member>
            <member name="name"><string name="s" value="Staccato"/></member>
         </obj>
         <obj class="PSoundSlot" ID="40">
            <obj class="PSlotMidiAction" name="action" ID="41">
               <member name="midiMessages">
                  <list name="obj" type="obj">
                     <obj class="POutputEvent" ID="42">
                        <int name="status" value="144"/>
                        <int name="data1" value="26"/>
                     </obj>
                     <obj class="POutputEvent" ID="43">
                        <int name="status" value="192"/>
                        <int name="data1" value="5"/>
                     </obj>
                  </list>
               </member>
            </obj>
            <member name="sv"><list name="s" type="obj"><obj class="USlotVisuals" ID="12"/></list></member>
            <member name="name"><string name="s" value="Pizzicato"/></member>
         </obj>
         <obj class="PSoundSlot" ID="50">
            <obj class="PSlotMidiAction" name="action" ID="51">
               <member name="midiMessages">
                  <list name="obj" type="obj">
                     <obj class="POutputEvent" ID="52">
                        <int name="status" value="176"/>
                        <int name="data1" value="68"/>
                        <int name="data2" value="127"/>
                     </obj>
                  </list>
               </member>
            </obj>
            <member name="sv"><list name="s" type="obj"><obj class="USlotVisuals" ID="13"/></list></member>
            <member name="name"><string name="s" value="Legato"/></member>
         </obj>
         <obj class="PSoundSlot" ID="60">
            <obj class="PSlotMidiAction" name="action" ID="61">
               <member name="noteChanger">
                  <list name="obj" type="obj">
                     <obj class="PSlotNoteChanger" ID="62">
                        <int name="channel" value="3"/>
                        <float name="velocityFact" value="1"/>
                        <float name="lengthFact" value="1"/>
                        <int name="transpose" value="-12"/>
                     </obj>
                  </list>
               </member>
            </obj>
            <member name="sv"><list name="s" type="obj">
               <obj class="USlotVisuals" ID="12"/><obj class="USlotVisuals" ID="13"/>
            </list></member>
            <member name="name"><string name="s" value="Pizz Legato"/></member>
         </obj>
      </list>
   </member>
</InstrumentMap>
"#;

    #[test]
    fn an_expression_map_reads_into_an_articulation_map() {
        let m = parse(ArticulationMapId(5), SAMPLE).unwrap();
        assert_eq!(m.name, "Strings Test");
        let names: Vec<&str> = m.articulations.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["Sustain", "Staccato", "Pizzicato", "Legato"]);
        // The slot without articulation is group 1's first: the default.
        let sus = m.find("Sustain").unwrap();
        assert_eq!(sus.kind, ArticulationKind::Direction);
        assert_eq!(m.default_state()[0], Some(sus.id));
        assert_eq!(
            sus.output.keyswitches,
            vec![Keyswitch {
                key: 24,
                velocity: 120
            }]
        );
        let stacc = m.find("stacc.").unwrap();
        assert_eq!(stacc.kind, ArticulationKind::Attribute);
        assert_eq!((stacc.output.velocity, stacc.output.length), (80, 50));
        let pizz = m.find("Pizzicato").unwrap();
        assert_eq!(pizz.output.program, Some(5));
        assert_eq!(pizz.output.keyswitches[0].velocity, 100);
        let legato = m.find("Legato").unwrap();
        assert_eq!(legato.group, 1);
        assert_eq!(
            legato.output.controls,
            vec![ControlValue {
                controller: 68,
                value: 127
            }]
        );
        assert_eq!(m.combinations.len(), 1);
        let r = m.resolve(&[Some(pizz.id), Some(legato.id), None, None]);
        assert_eq!((r.channel, r.transpose), (Some(3), -12));
        assert!(r.keyswitches.is_empty());
    }

    #[test]
    fn other_files_are_refused() {
        assert!(parse(ArticulationMapId(1), "<Other/>").is_err());
        assert!(parse(ArticulationMapId(1), "not xml").is_err());
        assert!(parse(ArticulationMapId(1), "<InstrumentMap/>").is_err());
    }
}
