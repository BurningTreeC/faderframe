#![allow(clippy::unwrap_used)]

use super::*;

/// The project.xml Bitwig Studio 5.0 saved (DAWproject's README).
pub(crate) const BITWIG: &str = r##"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Project version="1.0">
  <Application name="Bitwig Studio" version="5.0"/>
  <Transport>
    <Tempo max="666.000000" min="20.000000" unit="bpm" value="149.000000" id="id0" name="Tempo"/>
    <TimeSignature denominator="4" numerator="4" id="id1"/>
  </Transport>
  <Structure>
    <Track contentType="notes" loaded="true" id="id2" name="Bass" color="#a2eabf">
      <Channel audioChannels="2" destination="id15" role="regular" solo="false" id="id3">
        <Devices>
          <ClapPlugin deviceID="org.surge-synth-team.surge-xt" deviceName="Surge XT" deviceRole="instrument" loaded="true" id="id7" name="Surge XT">
            <Parameters/>
            <Enabled value="true" id="id8" name="On/Off"/>
            <State path="plugins/d19b1f6e-bbb6-42fe-a6c9-54b41d97a05d.clap-preset"/>
          </ClapPlugin>
        </Devices>
        <Mute value="false" id="id6" name="Mute"/>
        <Pan max="1.000000" min="0.000000" unit="normalized" value="0.500000" id="id5" name="Pan"/>
        <Volume max="2.000000" min="0.000000" unit="linear" value="0.659140" id="id4" name="Volume"/>
      </Channel>
    </Track>
    <Track contentType="audio" loaded="true" id="id9" name="Drumloop" color="#b53bba">
      <Channel audioChannels="2" destination="id15" role="regular" solo="false" id="id10">
        <Mute value="false" id="id13" name="Mute"/>
        <Pan max="1.000000" min="0.000000" unit="normalized" value="0.500000" id="id12" name="Pan"/>
        <Volume max="2.000000" min="0.000000" unit="linear" value="0.177125" id="id11" name="Volume"/>
      </Channel>
    </Track>
    <Track contentType="audio notes" loaded="true" id="id14" name="Master">
      <Channel audioChannels="2" role="master" solo="false" id="id15">
        <Mute value="false" id="id18" name="Mute"/>
        <Pan max="1.000000" min="0.000000" unit="normalized" value="0.500000" id="id17" name="Pan"/>
        <Volume max="2.000000" min="0.000000" unit="linear" value="1.000000" id="id16" name="Volume"/>
      </Channel>
    </Track>
  </Structure>
  <Arrangement id="id19">
    <Lanes timeUnit="beats" id="id20">
      <Lanes track="id2" id="id21">
        <Clips id="id22">
          <Clip time="0.0" duration="8.0" playStart="0.0">
            <Notes id="id23">
              <Note time="0.000000" duration="0.250000" channel="0" key="65" vel="0.787402" rel="0.787402"/>
              <Note time="1.000000" duration="0.250000" channel="0" key="65" vel="0.787402" rel="0.787402"/>
              <Note time="1.500000" duration="2.500000" channel="0" key="53" vel="0.787402" rel="0.787402"/>
            </Notes>
          </Clip>
        </Clips>
      </Lanes>
      <Lanes track="id9" id="id24">
        <Clips id="id25">
          <Clip time="0.0" duration="8.00003433227539" playStart="0.0" loopStart="0.0" loopEnd="8.00003433227539" fadeTimeUnit="beats" fadeInTime="0.0" fadeOutTime="0.0" name="Drumfunk3 170bpm">
            <Clips id="id26">
              <Clip time="0.0" duration="8.00003433227539" contentTimeUnit="beats" playStart="0.0" fadeTimeUnit="beats" fadeInTime="0.0" fadeOutTime="0.0">
                <Warps contentTimeUnit="seconds" timeUnit="beats" id="id28">
                  <Audio algorithm="stretch" channels="2" duration="2.823541666666667" sampleRate="48000" id="id27">
                    <File path="audio/Drumfunk3 170bpm.wav"/>
                  </Audio>
                  <Warp time="0.0" contentTime="0.0"/>
                  <Warp time="8.00003433227539" contentTime="2.823541666666667"/>
                </Warps>
              </Clip>
            </Clips>
          </Clip>
        </Clips>
      </Lanes>
      <Lanes track="id14" id="id29">
        <Clips id="id30"/>
      </Lanes>
    </Lanes>
  </Arrangement>
  <Scenes/>
</Project>
"##;

#[test]
fn bitwigs_example_reads() {
    let (p, notes) = read_project(BITWIG).unwrap();
    assert!(notes.is_empty(), "{notes:?}");
    assert_eq!(p.application.name, "Bitwig Studio");
    let t = p.transport.as_ref().unwrap();
    assert_eq!(t.tempo.as_ref().unwrap().value, Some(149.0));
    assert_eq!(t.tempo.as_ref().unwrap().unit, Unit::Bpm);
    let tracks = p.tracks();
    assert_eq!(tracks.len(), 3);
    assert_eq!(tracks[0].content_types, [ContentType::Notes]);
    assert_eq!(
        tracks[2].content_types,
        [ContentType::Audio, ContentType::Notes]
    );
    let bass = tracks[0].channel.as_ref().unwrap();
    assert_eq!(bass.destination.as_deref(), Some("id15"));
    let d = &bass.devices[0];
    assert_eq!((d.kind, d.role), (DeviceKind::Clap, DeviceRole::Instrument));
    assert_eq!(
        d.device_id.as_deref(),
        Some("org.surge-synth-team.surge-xt")
    );
    assert_eq!(
        d.state.as_ref().unwrap().path,
        "plugins/d19b1f6e-bbb6-42fe-a6c9-54b41d97a05d.clap-preset"
    );
    assert_eq!(bass.volume.as_ref().unwrap().value, Some(0.65914));
    let lanes = p.arrangement.as_ref().unwrap().lanes.as_ref().unwrap();
    assert_eq!(lanes.common.time_unit, Some(TimeUnit::Beats));
    let Timeline::Lanes(drums) = &lanes.lanes[1] else {
        panic!()
    };
    assert_eq!(drums.common.track.as_deref(), Some("id9"));
    let Timeline::Clips(outer) = &drums.lanes[0] else {
        panic!()
    };
    let Some(Timeline::Clips(inner)) = outer.clips[0].content.as_deref() else {
        panic!()
    };
    let Some(Timeline::Warps(w)) = inner.clips[0].content.as_deref() else {
        panic!()
    };
    assert_eq!(w.warps.len(), 2);
    let Some(Timeline::Audio(a)) = w.content.as_deref() else {
        panic!()
    };
    assert_eq!(a.file.path, "audio/Drumfunk3 170bpm.wav");
    assert_eq!((a.channels, a.sample_rate), (Some(2), Some(48_000)));
}

/// A project using every element the model has.
pub(crate) fn sample() -> Project {
    let pan = |id: &str| {
        RealParameter::new(Meta::named(id, "Pan"), Unit::Normalized, 0.25).range(0.0, 1.0)
    };
    let volume = |id: &str, v: f64| {
        RealParameter::new(Meta::named(id, "Volume"), Unit::Linear, v).range(0.0, 2.0)
    };
    let mut synth = Channel::new(Meta::id("ch1"));
    synth.role = Some(MixerRole::Regular);
    synth.audio_channels = Some(2);
    synth.solo = Some(false);
    synth.destination = Some("ch3".into());
    let mut clap = Device::new(
        DeviceKind::Clap,
        Meta::named("dev1", "Surge XT"),
        DeviceRole::Instrument,
        "Surge XT",
    );
    clap.device_id = Some("org.surge-synth-team.surge-xt".into());
    clap.vendor = Some("Surge Synth Team".into());
    clap.plugin_version = Some("1.3".into());
    clap.enabled = Some(BoolParameter::new(Meta::named("dev1on", "On/Off"), true));
    clap.state = Some(FileReference::inside("plugins/1.clap-preset"));
    clap.parameters.push(Parameter::Real(RealParameter {
        meta: Meta::named("p1", "Cutoff"),
        parameter_id: Some(-2_147_483_647),
        unit: Unit::Linear,
        min: Some(-60.0),
        max: Some(70.0),
        value: Some(f64::NEG_INFINITY),
    }));
    clap.parameters.push(Parameter::Enum(EnumParameter {
        meta: Meta::named("p2", "Mode"),
        parameter_id: Some(3),
        count: 3,
        labels: vec!["Poly".into(), "Mono".into(), "Legato".into()],
        value: Some(1),
    }));
    clap.parameters.push(Parameter::Integer(IntegerParameter {
        meta: Meta::named("p3", "Voices"),
        parameter_id: Some(4),
        min: Some(1),
        max: Some(64),
        value: Some(16),
    }));
    let mut comp = Device::new(
        DeviceKind::Compressor,
        Meta::named("dev2", "Compressor"),
        DeviceRole::AudioFx,
        "Compressor",
    );
    // (In the schema's order: they are written in it.)
    comp.own.push((
        "Attack".into(),
        Parameter::Real(RealParameter::new(Meta::id("c2"), Unit::Seconds, 0.01)),
    ));
    comp.own.push((
        "AutoMakeup".into(),
        Parameter::Bool(BoolParameter::new(Meta::id("c3"), true)),
    ));
    comp.own.push((
        "Threshold".into(),
        Parameter::Real(RealParameter::new(Meta::id("c1"), Unit::Decibel, -18.0)),
    ));
    let mut eq = Device::new(
        DeviceKind::Equalizer,
        Meta::named("dev3", "EQ"),
        DeviceRole::AudioFx,
        "EQ",
    );
    eq.bands.push(EqBand {
        kind: EqBandType::Bell,
        order: Some(2),
        freq: RealParameter::new(Meta::id("b1f"), Unit::Hertz, 1000.0),
        gain: Some(RealParameter::new(Meta::id("b1g"), Unit::Decibel, 3.0)),
        q: Some(RealParameter::new(Meta::id("b1q"), Unit::Linear, 0.7)),
        enabled: Some(BoolParameter::new(Meta::id("b1e"), true)),
    });
    eq.own.push((
        "OutputGain".into(),
        Parameter::Real(RealParameter::new(Meta::id("e1"), Unit::Decibel, -1.0)),
    ));
    synth.devices = vec![clap, comp, eq];
    synth.mute = Some(BoolParameter::new(Meta::named("m1", "Mute"), false));
    synth.pan = Some(pan("pan1"));
    synth.volume = Some(volume("vol1", 0.5));
    synth.sends.push(Send {
        meta: Meta::id("send1"),
        destination: Some("ch2".into()),
        kind: Some(SendType::Post),
        enable: Some(BoolParameter::new(Meta::id("send1on"), true)),
        pan: Some(pan("send1pan")),
        volume: volume("send1vol", 0.3),
    });
    let mut aux = Channel::new(Meta::named("ch2", "Reverb"));
    aux.role = Some(MixerRole::Effect);
    aux.destination = Some("ch3".into());
    let mut master = Channel::new(Meta::id("ch3"));
    master.role = Some(MixerRole::Master);
    master.volume = Some(volume("vol3", 1.0));
    let lead = Track {
        meta: Meta {
            id: Some("t1".into()),
            name: Some("Lead <synth> & \"co\"".into()),
            color: Some("#a2eabf".into()),
            comment: Some("two\nlines".into()),
        },
        content_types: vec![ContentType::Notes],
        loaded: Some(true),
        channel: Some(synth),
        tracks: Vec::new(),
    };
    let folder = Track {
        meta: Meta::named("t0", "Folder"),
        content_types: vec![ContentType::Tracks],
        loaded: None,
        channel: None,
        tracks: vec![lead],
    };
    let returns = Track {
        meta: Meta::named("t2", "Reverb"),
        content_types: vec![ContentType::Audio],
        loaded: Some(true),
        channel: Some(aux),
        tracks: Vec::new(),
    };
    let master_track = Track {
        meta: Meta::named("t3", "Master"),
        content_types: vec![ContentType::Audio, ContentType::Notes],
        loaded: Some(true),
        channel: Some(master),
        tracks: Vec::new(),
    };
    let expression = Points {
        common: TimelineMeta::id("x1"),
        unit: Some(Unit::Semitones),
        target: AutomationTarget {
            expression: Some(ExpressionType::Transpose),
            ..AutomationTarget::default()
        },
        points: vec![
            Point::Real {
                time: 0.0,
                value: 0.0,
                interpolation: Some(Interpolation::Linear),
            },
            Point::Real {
                time: 0.5,
                value: 2.0,
                interpolation: None,
            },
        ],
    };
    let notes = Notes {
        common: TimelineMeta::id("n1"),
        notes: vec![
            Note {
                time: 0.0,
                duration: 0.25,
                channel: 0,
                key: 60,
                vel: Some(100.0 / 127.0),
                rel: Some(0.5),
                content: Some(Box::new(Timeline::Points(expression))),
            },
            Note {
                time: 1.0 / 3.0,
                duration: 0.125,
                channel: 9,
                key: 36,
                vel: Some(1.0),
                rel: None,
                content: None,
            },
        ],
    };
    let cc = Points {
        common: TimelineMeta::id("cc1"),
        unit: Some(Unit::Normalized),
        target: AutomationTarget {
            expression: Some(ExpressionType::ChannelController),
            channel: Some(0),
            controller: Some(1),
            ..AutomationTarget::default()
        },
        points: vec![Point::Real {
            time: 2.0,
            value: 0.5,
            interpolation: None,
        }],
    };
    let midi_clip = Clip {
        meta: Meta {
            name: Some("Riff".into()),
            color: Some("#112233".into()),
            ..Meta::default()
        },
        time: 4.0,
        duration: Some(8.0),
        play_start: Some(0.0),
        play_stop: Some(8.0),
        loop_start: Some(0.0),
        loop_end: Some(4.0),
        enable: Some(false),
        content: Some(Box::new(Timeline::Lanes(Lanes {
            common: TimelineMeta::id("l9"),
            lanes: vec![Timeline::Notes(notes), Timeline::Points(cc)],
        }))),
        ..Clip::default()
    };
    let audio_clip = Clip {
        meta: Meta::default(),
        time: 16.0,
        duration: Some(4.0),
        content_time_unit: Some(TimeUnit::Seconds),
        play_start: Some(0.5),
        fade_time_unit: Some(TimeUnit::Seconds),
        fade_in_time: Some(0.01),
        fade_out_time: Some(0.1),
        content: Some(Box::new(Timeline::Warps(Warps {
            common: TimelineMeta {
                meta: Meta::id("w1"),
                time_unit: Some(TimeUnit::Seconds),
                track: None,
            },
            content_time_unit: TimeUnit::Seconds,
            content: Some(Box::new(Timeline::Audio(MediaFile {
                common: TimelineMeta::id("a1"),
                duration: 2.5,
                file: FileReference::inside("audio/loop.wav"),
                algorithm: Some("stretch".into()),
                channels: Some(2),
                sample_rate: Some(48_000),
            }))),
            warps: vec![
                Warp {
                    time: 0.0,
                    content_time: 0.0,
                },
                Warp {
                    time: 2.0,
                    content_time: 2.5,
                },
            ],
        }))),
        ..Clip::default()
    };
    let alias = Clip {
        time: 32.0,
        duration: Some(8.0),
        reference: Some("l9".into()),
        ..Clip::default()
    };
    let volume_automation = Points {
        common: TimelineMeta::id("auto1"),
        unit: Some(Unit::Linear),
        target: AutomationTarget {
            parameter: Some("vol1".into()),
            ..AutomationTarget::default()
        },
        points: vec![
            Point::Real {
                time: 0.0,
                value: 0.5,
                interpolation: Some(Interpolation::Linear),
            },
            Point::Real {
                time: 8.0,
                value: 1.0,
                interpolation: Some(Interpolation::Hold),
            },
        ],
    };
    let mute_automation = Points {
        common: TimelineMeta::id("auto2"),
        unit: None,
        target: AutomationTarget {
            parameter: Some("m1".into()),
            ..AutomationTarget::default()
        },
        points: vec![
            Point::Bool {
                time: 0.0,
                value: false,
            },
            Point::Bool {
                time: 4.0,
                value: true,
            },
            Point::Enum {
                time: 6.0,
                value: 2,
            },
            Point::Integer {
                time: 7.0,
                value: -3,
            },
        ],
    };
    let arrangement = Arrangement {
        meta: Meta::id("arr"),
        lanes: Some(Lanes {
            common: TimelineMeta {
                meta: Meta::id("lanes"),
                time_unit: Some(TimeUnit::Beats),
                track: None,
            },
            lanes: vec![Timeline::Lanes(Lanes {
                common: TimelineMeta {
                    meta: Meta::id("lt1"),
                    time_unit: None,
                    track: Some("t1".into()),
                },
                lanes: vec![
                    Timeline::Clips(Clips {
                        common: TimelineMeta::id("clips1"),
                        clips: vec![midi_clip, audio_clip, alias],
                    }),
                    Timeline::Points(volume_automation),
                    Timeline::Points(mute_automation),
                    Timeline::Markers(Markers {
                        common: TimelineMeta::id("lm"),
                        markers: vec![Marker {
                            meta: Meta {
                                name: Some("Cue".into()),
                                ..Meta::default()
                            },
                            time: 1.0,
                        }],
                    }),
                ],
            })],
        }),
        markers: Some(Markers {
            common: TimelineMeta::id("markers"),
            markers: vec![Marker {
                meta: Meta {
                    name: Some("Verse".into()),
                    color: Some("#ff0000".into()),
                    comment: Some("section end=16".into()),
                    id: None,
                },
                time: 0.0,
            }],
        }),
        tempo_automation: Some(Points {
            common: TimelineMeta::id("tempo-auto"),
            unit: Some(Unit::Bpm),
            target: AutomationTarget {
                parameter: Some("tempo".into()),
                ..AutomationTarget::default()
            },
            points: vec![
                Point::Real {
                    time: 0.0,
                    value: 120.0,
                    interpolation: Some(Interpolation::Linear),
                },
                Point::Real {
                    time: 16.0,
                    value: 90.0,
                    interpolation: None,
                },
            ],
        }),
        time_signature_automation: Some(Points {
            common: TimelineMeta::id("ts-auto"),
            unit: None,
            target: AutomationTarget {
                parameter: Some("ts".into()),
                ..AutomationTarget::default()
            },
            points: vec![Point::TimeSignature {
                time: 0.0,
                numerator: 7,
                denominator: 8,
            }],
        }),
    };
    Project {
        version: VERSION.into(),
        application: Application {
            name: "FaderFrame".into(),
            version: "0.36.0".into(),
        },
        transport: Some(Transport {
            tempo: Some(
                RealParameter::new(Meta::named("tempo", "Tempo"), Unit::Bpm, 120.0)
                    .range(1.0, 999.0),
            ),
            time_signature: Some(TimeSignatureParameter {
                meta: Meta::id("ts"),
                parameter_id: None,
                numerator: 7,
                denominator: 8,
            }),
        }),
        structure: vec![
            Lane::Track(folder),
            Lane::Track(returns),
            Lane::Track(master_track),
        ],
        arrangement: Some(arrangement),
        scenes: vec![Scene {
            meta: Meta::named("s1", "Scene 1"),
            content: Some(Timeline::Lanes(Lanes {
                common: TimelineMeta::id("sl"),
                lanes: vec![Timeline::ClipSlot(ClipSlot {
                    common: TimelineMeta {
                        meta: Meta::id("slot1"),
                        time_unit: None,
                        track: Some("t1".into()),
                    },
                    has_stop: Some(true),
                    clip: Some(Clip {
                        time: 0.0,
                        duration: Some(4.0),
                        reference: Some("n1".into()),
                        ..Clip::default()
                    }),
                })],
            })),
        }],
    }
}

#[test]
fn a_project_reads_back_as_written() {
    let p = sample();
    let xml = write_project(&p);
    let (back, notes) = read_project(&xml).unwrap();
    assert!(notes.is_empty(), "{notes:?}");
    // Enum labels are tokens.
    assert_eq!(back, p);
    let m = MetaData {
        title: Some("Song & Dance".into()),
        artist: Some("Someone".into()),
        year: Some("2026".into()),
        ..MetaData::default()
    };
    assert_eq!(read_metadata(&write_metadata(&m)).unwrap(), m);
}

#[test]
fn numbers_read_as_written() {
    for v in [0.0, -0.0, 1.0 / 3.0, 1e-9, 123_456_789.123, 0.787_402] {
        let s = write::num(v);
        assert!(!s.contains('e'), "{s}: no exponents in xs:double text");
        assert_eq!(read::parse_number(&s), Some(v + 0.0));
    }
    assert_eq!(read::parse_number("inf"), Some(f64::INFINITY));
    assert_eq!(read::parse_number("-INF"), Some(f64::NEG_INFINITY));
    assert_eq!(read::parse_number("8.000000"), Some(8.0));
}

#[test]
fn unknown_things_are_noted_not_fatal() {
    let xml = r#"<Project version="2.0"><Application name="X" version="1"/>
        <Structure><Track id="t" name="A" contentType="audio wobble"><Gizmo/></Track></Structure>
        <Arrangement><Lanes><Clips><Clip time="zero"/></Clips><Thing/></Lanes></Arrangement></Project>"#;
    let (p, notes) = read_project(xml).unwrap();
    assert_eq!(p.tracks().len(), 1);
    let text = notes.join("\n");
    assert!(text.contains("<Gizmo> in <Track>"), "{text}");
    assert!(text.contains("wobble"), "{text}");
    assert!(text.contains("<Thing> in <Lanes>"), "{text}");
    assert!(text.contains("time=\"zero\""), "{text}");
    assert!(text.contains("DAWproject 2.0"), "{text}");
    assert!(read_project("<Score/>").is_err());
    assert!(read_project("not xml").is_err());
}

/// `xmllint` with the vendored schema, when it is installed.
pub(crate) fn validate(xml: &str, schema: &str) -> Option<Result<(), String>> {
    let lint = std::process::Command::new("xmllint")
        .arg("--version")
        .output()
        .ok()?;
    if !lint.status.success() {
        return None;
    }
    let dir = std::env::temp_dir().join(format!(
        "ff-dawproject-xsd-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos())
    ));
    std::fs::create_dir_all(&dir).ok()?;
    let file = dir.join("doc.xml");
    std::fs::write(&file, xml).ok()?;
    let xsd = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("data/dawproject")
        .join(schema);
    let out = std::process::Command::new("xmllint")
        .arg("--noout")
        .arg("--schema")
        .arg(&xsd)
        .arg(&file)
        .output()
        .ok()?;
    let _ = std::fs::remove_dir_all(&dir);
    Some(if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).into_owned())
    })
}

#[test]
fn the_written_documents_keep_to_the_schema() {
    let p = sample();
    match validate(&write_project(&p), "Project.xsd") {
        None => eprintln!("xmllint not installed: schema check skipped"),
        Some(r) => r.unwrap(),
    }
    let m = MetaData {
        title: Some("T".into()),
        comment: Some("c".into()),
        ..MetaData::default()
    };
    if let Some(r) = validate(&write_metadata(&m), "MetaData.xsd") {
        r.unwrap();
    }
    // Bitwig's own file is valid too (the check itself works).
    if let Some(r) = validate(BITWIG, "Project.xsd") {
        r.unwrap();
    }
}
