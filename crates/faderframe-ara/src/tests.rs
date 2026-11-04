//! The host against the mock plug-in: model sync in edit cycles,
//! rendering, undo-safe deletes, archives, content, updates.
#![allow(clippy::unwrap_used)]

use crate::content::{Context, Note, OrderedQuarters, TempoEntry};
use crate::mock::{self, Instance};
use crate::{
    Archive, Assignment, BarSignature, Document, Extension, Model, Modification, PlaybackRequest,
    Region, Renderer, Roles, Samples, Sequence, Source, ffi,
};
use std::collections::BTreeSet;
use std::sync::Arc;

const RATE: f64 = 48_000.0;

struct Memory(Vec<Vec<f32>>);

impl Samples for Memory {
    fn read(&self, start: i64, out: &mut [&mut [f32]]) -> bool {
        for (c, o) in out.iter_mut().enumerate() {
            let src = &self.0[c.min(self.0.len() - 1)];
            for (i, v) in o.iter_mut().enumerate() {
                let k = start + i as i64;
                *v = if k >= 0 && (k as usize) < src.len() {
                    src[k as usize]
                } else {
                    0.0
                };
            }
        }
        true
    }
}

/// A second of A440, then half a second of silence.
fn tone() -> Arc<Memory> {
    let mut x: Vec<f32> = (0..RATE as usize)
        .map(|i| 0.5 * (2.0 * std::f64::consts::PI * 440.0 * i as f64 / RATE).sin() as f32)
        .collect();
    x.extend(std::iter::repeat_n(0.0, RATE as usize / 2));
    Arc::new(Memory(vec![x.clone(), x]))
}

fn model(samples: &Arc<Memory>, at: f64) -> Model {
    Model {
        name: Some("Song".into()),
        context: Context {
            tempo: Context::tempo_entries(&[], 0.5),
            bars: vec![BarSignature {
                numerator: 4,
                denominator: 4,
                quarters: OrderedQuarters::new(0.0),
            }],
            ..Context::default()
        },
        sequences: vec![Sequence {
            key: "t:1".into(),
            name: Some("Vocals".into()),
            order: 0,
            color: Some([0.8, 0.2, 0.2]),
        }],
        sources: vec![Source {
            key: "src:1".into(),
            name: Some("Take 1".into()),
            frames: (RATE * 1.5) as i64,
            sample_rate: RATE,
            channels: 2,
            samples: samples.clone(),
        }],
        modifications: vec![Modification {
            key: "mod:1:1".into(),
            source: "src:1".into(),
            name: None,
        }],
        regions: vec![Region {
            key: "r:1@mod:1:1".into(),
            modification: "mod:1:1".into(),
            sequence: "t:1".into(),
            start_in_modification: 0.0,
            duration_in_modification: 1.5,
            start_in_playback: at,
            duration_in_playback: 1.5,
            transformation: 0,
            name: Some("Take 1".into()),
            color: None,
        }],
    }
}

fn keys(k: &[&str]) -> BTreeSet<String> {
    k.iter().map(|s| s.to_string()).collect()
}

struct Rig {
    doc: Document,
    instance: Instance,
    renderer: Renderer,
}

fn rig() -> Rig {
    let doc = Document::new(mock::factory(), Some("Song")).unwrap();
    let mut instance = Instance::new();
    let raw = instance.bind(doc.controller(), Roles::ALL, Roles::ALL);
    // SAFETY: the instance outlives the renderer in every test.
    let ext = unsafe { Extension::from_raw(raw) }.unwrap();
    assert!(ext.plays() && ext.previews() && ext.views());
    Rig {
        doc,
        instance,
        renderer: Renderer::new(ext),
    }
}

impl Rig {
    fn sync(
        &mut self,
        m: &Model,
        regions: &[&str],
        archive: Option<&Archive>,
    ) -> crate::SyncReport {
        let sequences = m.sequences.iter().map(|s| s.key.clone()).collect();
        let mut a = [Assignment {
            renderer: &mut self.renderer,
            regions: keys(regions),
            sequences,
        }];
        self.doc.sync(m, &mut a, archive)
    }

    /// One block of `frames` from song second `at`.
    fn render(&self, at: f64, frames: usize) -> Vec<f32> {
        let mut l = vec![0.0; frames];
        let mut r = vec![0.0; frames];
        self.instance
            .renderer()
            .render((at * RATE) as i64, RATE, true, &mut [&mut l, &mut r]);
        l
    }
}

#[test]
fn the_factory_describes_itself() {
    let f = mock::factory();
    assert_eq!(f.id(), mock::FACTORY_ID);
    assert_eq!(f.name(), "ARA Mock");
    assert_eq!(f.vendor(), "FaderFrame");
    assert_eq!(f.archive_id(), mock::ARCHIVE_ID);
    assert_eq!(f.compatible_archive_ids(), vec![mock::OLD_ARCHIVE_ID]);
    assert!(f.reads_archive(mock::OLD_ARCHIVE_ID));
    assert!(!f.reads_archive("org.other.archive"));
    assert!(f.stretches());
    assert_eq!(f.analyzable(), vec![ffi::kARAContentTypeNotes]);
}

#[test]
fn a_document_holds_the_model_and_renders_its_regions() {
    let samples = tone();
    let mut r = rig();
    let m = model(&samples, 2.0);
    let report = r.sync(&m, &["r:1@mod:1:1"], None);
    assert!(report.edited);
    assert!(mock::initialised());
    let s = r.instance.summary();
    assert_eq!(s.sources, vec![("src:1".into(), true, true)], "analysed");
    assert_eq!(s.modifications, vec![("mod:1:1".into(), 1.0, true)]);
    assert_eq!(s.regions, 1);
    assert_eq!(s.sequences, vec!["Vocals".to_string()]);
    assert_eq!(s.tempo.len(), 2, "the host's tempo map was read");
    assert_eq!(s.tempo[1].seconds, 0.5);
    assert_eq!(r.instance.region_count(), 1);
    assert_eq!(r.instance.probe().sequence_count(), 1);
    // Nothing changed: no edit cycle.
    assert!(!r.sync(&m, &["r:1@mod:1:1"], None).edited);
    r.instance.set_active(true);
    let out = r.render(2.0, 256);
    let want = &samples.0[0][..256];
    assert_eq!(out, want, "the region plays its source from second 2");
    assert!(
        r.render(1.0, 256).iter().all(|v| *v == 0.0),
        "silent before"
    );
    assert!(
        r.instance.violations().is_empty(),
        "{:?}",
        r.instance.violations()
    );
}

#[test]
fn a_moved_clip_moves_its_region_while_playing() {
    let samples = tone();
    let mut r = rig();
    r.sync(&model(&samples, 2.0), &["r:1@mod:1:1"], None);
    r.instance.set_active(true);
    // Properties change in an edit cycle; the renderer keeps its regions,
    // so it may stay active.
    assert!(r.sync(&model(&samples, 4.0), &["r:1@mod:1:1"], None).edited);
    assert!(r.render(2.0, 64).iter().all(|v| *v == 0.0));
    assert_eq!(r.render(4.0, 64), samples.0[0][..64].to_vec());
    assert!(
        r.instance.violations().is_empty(),
        "{:?}",
        r.instance.violations()
    );
}

#[test]
fn a_deleted_clip_keeps_its_edits_for_undo() {
    let samples = tone();
    let mut r = rig();
    let m = model(&samples, 0.0);
    r.sync(&m, &["r:1@mod:1:1"], None);
    assert!(r.instance.edit("mod:1:1", 0.5), "edited in the plug-in");
    // The clip goes: its region leaves the (inactive) renderer and is
    // destroyed; the modification and source are only deactivated.
    let empty = Model {
        sources: Vec::new(),
        modifications: Vec::new(),
        regions: Vec::new(),
        ..m.clone()
    };
    assert!(r.renderer.changes(&keys(&[])));
    r.sync(&empty, &[], None);
    let s = r.instance.summary();
    assert_eq!(s.regions, 0);
    assert_eq!(s.modifications, vec![("mod:1:1".into(), 0.5, false)]);
    assert_eq!(s.sources, vec![("src:1".into(), false, true)]);
    // The store leaves deactivated objects out.
    let archive = r.doc.store().unwrap();
    let text = String::from_utf8(archive.data.clone()).unwrap();
    assert!(!text.contains("mod:1:1"), "{text}");
    // Undo: back with its edit.
    r.sync(&m, &["r:1@mod:1:1"], None);
    r.instance.set_active(true);
    let out = r.render(0.0, 32);
    for (o, s) in out.iter().zip(&samples.0[0]) {
        assert!((o - s * 0.5).abs() < 1e-7);
    }
    assert!(
        r.instance.violations().is_empty(),
        "{:?}",
        r.instance.violations()
    );
}

#[test]
fn an_archive_carries_the_edits_into_a_new_document() {
    let samples = tone();
    let m = model(&samples, 0.0);
    let archive = {
        let mut r = rig();
        r.sync(&m, &["r:1@mod:1:1"], None);
        r.instance.edit("mod:1:1", 0.25);
        r.instance.set_document_data("chords: C F G");
        r.doc.store().unwrap()
    };
    assert_eq!(archive.id, mock::ARCHIVE_ID);
    let mut r = rig();
    let report = r.sync(&m, &["r:1@mod:1:1"], Some(&archive));
    assert_eq!(report.restored, Some(true));
    let s = r.instance.summary();
    assert_eq!(s.restored, 1);
    assert_eq!(s.modifications, vec![("mod:1:1".into(), 0.25, true)]);
    assert_eq!(s.document_data, "chords: C F G");
    // Another plug-in's archive is not handed over.
    let mut other = rig();
    let foreign = Archive {
        id: "com.vendor.other".into(),
        data: archive.data.clone(),
    };
    assert_eq!(
        other.sync(&m, &["r:1@mod:1:1"], Some(&foreign)).restored,
        Some(false)
    );
    assert_eq!(other.instance.summary().restored, 0);
    // An older format it reads is.
    let old = Archive {
        id: mock::OLD_ARCHIVE_ID.into(),
        data: archive.data,
    };
    let mut third = rig();
    assert_eq!(
        third.sync(&m, &["r:1@mod:1:1"], Some(&old)).restored,
        Some(true)
    );
    for x in [&r, &other, &third] {
        assert!(
            x.instance.violations().is_empty(),
            "{:?}",
            x.instance.violations()
        );
    }
}

#[test]
fn notes_are_read_back_in_song_time() {
    let samples = tone();
    let mut r = rig();
    r.sync(&model(&samples, 2.0), &["r:1@mod:1:1"], None);
    let notes: Vec<Note> = r.doc.region_content("r:1@mod:1:1").unwrap();
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert_eq!(notes[0].pitch, Some(69));
    assert!((notes[0].start - 2.0).abs() < 1e-3);
    assert!((notes[0].length - 1.0).abs() < 0.01);
    let source: Vec<Note> = r.doc.source_content("src:1").unwrap();
    assert!(source[0].start.abs() < 1e-3, "source time");
    assert!(r.doc.modification_content::<Note>("mod:1:1").is_none());
    let tempo: Option<Vec<TempoEntry>> = r.doc.region_content("r:1@mod:1:1");
    assert!(tempo.is_none(), "only notes");
}

#[test]
fn the_plugin_reports_its_changes_and_requests() {
    let samples = tone();
    let mut r = rig();
    r.sync(&model(&samples, 0.0), &["r:1@mod:1:1"], None);
    let u = r.doc.poll();
    assert_eq!(u.sources, vec!["src:1".to_string()], "analysis done");
    assert!(u.changes_state());
    assert_eq!(u.analysis.len(), 1);
    assert_eq!(u.analysis[0].state, crate::AnalysisState::Completed);
    assert!(r.doc.poll().is_empty());
    r.instance.edit("mod:1:1", 2.0);
    let u = r.doc.poll();
    assert_eq!(u.modifications, vec!["mod:1:1".to_string()]);
    r.instance.request(PlaybackRequest::Locate(3.5));
    r.instance.request(PlaybackRequest::Start);
    r.instance.request(PlaybackRequest::CycleRange {
        start: 1.0,
        duration: 2.0,
    });
    let u = r.doc.poll();
    assert_eq!(
        u.playback,
        vec![
            PlaybackRequest::Locate(3.5),
            PlaybackRequest::Start,
            PlaybackRequest::CycleRange {
                start: 1.0,
                duration: 2.0
            }
        ]
    );
    assert!(!u.changes_state());
}

#[test]
fn a_reshaped_source_is_updated_without_access() {
    let samples = tone();
    let mut r = rig();
    let mut m = model(&samples, 0.0);
    r.sync(&m, &["r:1@mod:1:1"], None);
    m.sources[0].frames = RATE as i64;
    assert!(r.sync(&m, &["r:1@mod:1:1"], None).edited);
    assert!(
        r.instance.violations().is_empty(),
        "{:?}",
        r.instance.violations()
    );
}

#[test]
fn the_editor_view_hears_the_selection() {
    let samples = tone();
    let mut r = rig();
    r.sync(&model(&samples, 0.0), &["r:1@mod:1:1"], None);
    let ext = *r.renderer.extension();
    r.doc
        .select(&ext, &["r:1@mod:1:1".into()], &["t:1".into()], None);
    assert_eq!(r.instance.probe().selected(), 1);
    r.doc.select(&ext, &[], &[], Some((0.0, 1.0)));
    assert_eq!(r.instance.probe().selected(), 0);
}

#[test]
fn a_document_is_emptied_before_its_controller_goes() {
    let samples = tone();
    let r = rig();
    let Rig {
        mut doc,
        instance,
        mut renderer,
    } = r;
    let m = model(&samples, 0.0);
    let mut a = [Assignment {
        renderer: &mut renderer,
        regions: keys(&["r:1@mod:1:1"]),
        sequences: keys(&["t:1"]),
    }];
    doc.sync(&m, &mut a, None);
    let probe = instance.probe();
    // The instance goes first (it lets go of its regions), then the
    // document with everything in it.
    drop(renderer);
    drop(instance);
    drop(doc);
    let s = probe.summary();
    assert!(s.destroyed);
    assert!(probe.violations().is_empty(), "{:?}", probe.violations());
}

#[test]
fn a_track_moving_gives_its_region_to_another_renderer() {
    let samples = tone();
    let mut doc = Document::new(mock::factory(), None).unwrap();
    let mut a = Instance::new();
    let mut b = Instance::new();
    let ra = a.bind(doc.controller(), Roles::ALL, Roles::ALL);
    let rb = b.bind(doc.controller(), Roles::ALL, Roles::ALL);
    // SAFETY: both instances outlive their renderers.
    let (ea, eb) = unsafe {
        (
            Extension::from_raw(ra).unwrap(),
            Extension::from_raw(rb).unwrap(),
        )
    };
    let (mut ra, mut rb) = (Renderer::new(ea), Renderer::new(eb));
    let mut m = model(&samples, 0.0);
    m.sequences.push(Sequence {
        key: "t:2".into(),
        name: Some("Doubles".into()),
        order: 1,
        color: None,
    });
    doc.sync(
        &m,
        &mut [
            Assignment {
                renderer: &mut ra,
                regions: keys(&["r:1@mod:1:1"]),
                sequences: keys(&["t:1"]),
            },
            Assignment {
                renderer: &mut rb,
                regions: keys(&[]),
                sequences: keys(&["t:2"]),
            },
        ],
        None,
    );
    // The clip moves to track 2: same modification here (the host keys
    // modifications per track and source; this one keeps it to check the
    // hand-over), the region changes sequence and renderer.
    m.regions[0].sequence = "t:2".into();
    doc.sync(
        &m,
        &mut [
            Assignment {
                renderer: &mut ra,
                regions: keys(&[]),
                sequences: keys(&["t:1"]),
            },
            Assignment {
                renderer: &mut rb,
                regions: keys(&["r:1@mod:1:1"]),
                sequences: keys(&["t:2"]),
            },
        ],
        None,
    );
    assert_eq!(a.region_count(), 0);
    assert_eq!(b.region_count(), 1);
    assert!(a.violations().is_empty() && b.violations().is_empty());
}
