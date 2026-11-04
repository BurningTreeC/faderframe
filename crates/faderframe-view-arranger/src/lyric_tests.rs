//! Comping by lyric in the arranger: the words under each take's
//! waveform (a click uses that take for the word, Shift+click a run of
//! words, a double-click the line) and the lyrics comp row under the
//! lanes (the take chosen per word; a click takes the suggestion,
//! right-click lists the takes).

use super::*;
use crate::lyric_lane::LYRIC_ROW_H;
use faderframe_engine::EngineConfig;
use faderframe_project::lyric_comp::{SourceWords, SungWord};
use faderframe_session::lyric_comp::LyricCompOp;
use faderframe_ui_canvas::RecordingPainter;
use faderframe_workspace::TAKE_LANE_HEIGHT;

const SIZE: Size = Size::new(1400.0, 900.0);
const WORDS: [&str; 4] = ["one", "two", "three", "four"];

/// The demo's "Bass" clip as a folder of three takes of its source (the
/// third comped), the words of its part of the source heard.
fn session() -> (Session, ClipId, usize) {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let tracks = ArrangerView::lane_tracks(&s);
    let row = tracks.iter().position(|t| t.name == "Bass").unwrap();
    let clip = s.project().track(tracks[row].id).unwrap().clips[0];
    let c = s.project().clip(clip).unwrap().clone();
    let a = c.as_audio().unwrap().clone();
    let mut f = TakeFolder::new(a.length);
    for i in 0..3 {
        f.add_take(faderframe_project::Take {
            name: format!("Take {}", i + 1),
            source: a.source,
            source_offset: a.source_offset,
            start: 0,
            end: a.length,
            gain_db: 0.0,
            rating: 0,
        });
    }
    f.use_take(2);
    s.edit(Command::SetClipContent {
        clip,
        start: c.start,
        content: Box::new(ClipContent::Takes(f)),
    })
    .unwrap();
    let rate = s.project().sample_rate;
    let at = |secs: f64| a.source_offset + (secs * f64::from(rate)) as i64;
    assert!(
        a.length > (2.0 * f64::from(rate)) as i64,
        "room for the words"
    );
    let mut words = SourceWords::new(rate);
    words.add(
        (a.source_offset, a.source_offset + a.length),
        WORDS
            .iter()
            .enumerate()
            .map(|(k, w)| SungWord {
                start: at(0.25 + 0.45 * k as f64),
                end: at(0.25 + 0.45 * k as f64 + 0.3),
                text: (*w).into(),
                line: k == 0,
                confidence: 90,
            })
            .collect(),
    );
    s.edit(Command::SetSourceWords {
        source: a.source,
        words: Some(Box::new(words)),
    })
    .unwrap();
    (s, clip, row)
}

fn run(
    view: &mut ArrangerView,
    ev: ViewEvent,
    s: &Session,
) -> (Vec<Action>, Vec<HostRequest<Action>>) {
    let mut actions = Vec::new();
    let mut requests = Vec::new();
    let mut cx = EventCx::new(&mut actions, &mut requests);
    view.event(&ev, SIZE, s, &mut cx);
    (actions, requests)
}

fn press(pos: Point, clicks: u32, modifiers: Modifiers, button: PointerButton) -> ViewEvent {
    ViewEvent::PointerDown {
        pos,
        button,
        modifiers,
        clicks,
    }
}

fn texts(view: &mut ArrangerView, s: &Session) -> Vec<String> {
    let mut p = RecordingPainter::new();
    view.paint(&mut p, SIZE, s, &Theme::default());
    assert!(p.balanced_clips());
    p.texts().into_iter().map(str::to_string).collect()
}

/// The middle of word `w`'s chip in take lane `k`.
fn word_point(
    view: &ArrangerView,
    s: &Session,
    clip: ClipId,
    row: usize,
    k: usize,
    w: usize,
) -> Point {
    let c = s.project().clip(clip).unwrap();
    let comp = s.lyric_comp(clip).unwrap();
    let (a, b) = comp.alignment.span(w, k).unwrap();
    let row = view.row_rect(row, SIZE);
    let lane_h = s.take_lane_height(c.track);
    let lane = Rect::new(
        0.0,
        row.y + view.row_h() + k as f32 * lane_h,
        SIZE.w,
        lane_h - 1.0,
    );
    let strip = ArrangerView::word_strip(lane).unwrap();
    let x = (view.x_of_folder(s, c, a) + view.x_of_folder(s, c, b)) / 2.0;
    Point::new(x, strip.y + strip.h / 2.0)
}

#[test]
fn each_take_shows_its_words_and_a_click_uses_it_for_one() {
    let (mut s, clip, row) = session();
    let mut view = ArrangerView::new(Theme::default());
    s.dispatch(Action::ToggleTakeLanes(clip)).unwrap();
    let shown = texts(&mut view, &s);
    // Every lane's words, and the lyrics comp row with the take chosen.
    assert_eq!(shown.iter().filter(|t| *t == "two").count(), 3, "{shown:?}");
    assert!(shown.iter().any(|t| t == "two 3"), "{shown:?}");
    assert_eq!(
        view.row_rect(row, SIZE).h,
        view.row_h() + 3.0 * TAKE_LANE_HEIGHT + LYRIC_ROW_H
    );
    // A click on take 1's "two": that take for that word.
    let pos = word_point(&view, &s, clip, row, 0, 1);
    assert_eq!(
        view.hit_test(pos, SIZE, &s),
        Some(Hit::TakeWord {
            clip,
            take: 0,
            word: 1
        })
    );
    let (a, _) = run(
        &mut view,
        press(pos, 1, Modifiers::NONE, PointerButton::Primary),
        &s,
    );
    assert_eq!(
        a,
        vec![Action::LyricComp(LyricCompOp::Comp {
            clip,
            from: 1,
            to: 1,
            take: 0
        })]
    );
    s.dispatch(a[0].clone()).unwrap();
    let f = s.project().clip(clip).unwrap().as_takes().unwrap().clone();
    let comp = s.lyric_comp(clip).unwrap();
    assert_eq!(comp.alignment.chosen(1, &f), Some(0));
    assert_eq!(comp.alignment.chosen(0, &f), Some(2));
    assert_eq!(comp.alignment.chosen(2, &f), Some(2));
    assert!(texts(&mut view, &s).iter().any(|t| t == "two 1"));
    // Shift+click on take 1's "four": the words from "two" on.
    let pos = word_point(&view, &s, clip, row, 0, 3);
    let shift = Modifiers {
        shift: true,
        ..Modifiers::NONE
    };
    let (a, _) = run(&mut view, press(pos, 1, shift, PointerButton::Primary), &s);
    assert_eq!(
        a,
        vec![Action::LyricComp(LyricCompOp::Comp {
            clip,
            from: 1,
            to: 3,
            take: 0
        })]
    );
    // A double-click: the line.
    let pos = word_point(&view, &s, clip, row, 1, 2);
    let (a, _) = run(
        &mut view,
        press(pos, 2, Modifiers::NONE, PointerButton::Primary),
        &s,
    );
    assert_eq!(
        a,
        vec![Action::LyricComp(LyricCompOp::CompLine {
            clip,
            word: 2,
            take: 1
        })]
    );
    // Right-click on a word: its take, the line, the folder's entries.
    let (_, req) = run(
        &mut view,
        press(pos, 1, Modifiers::NONE, PointerButton::Secondary),
        &s,
    );
    let Some(HostRequest::ContextMenu { items, .. }) = req.into_iter().next() else {
        panic!("no menu");
    };
    let labels: Vec<String> = items.iter().map(|i| i.label.clone()).collect();
    assert!(
        labels.contains(&"Use “three” from Take 2".to_string()),
        "{labels:?}"
    );
    assert!(
        labels.contains(&"Use the Line from Take 2".to_string()),
        "{labels:?}"
    );
    assert!(
        labels.contains(&"Use All Suggested Takes".to_string()),
        "{labels:?}"
    );
}

#[test]
fn the_lyrics_comp_row_takes_suggestions_and_lists_the_takes() {
    let (mut s, clip, row) = session();
    let mut view = ArrangerView::new(Theme::default());
    s.dispatch(Action::ToggleTakeLanes(clip)).unwrap();
    texts(&mut view, &s);
    let c = s.project().clip(clip).unwrap().clone();
    let comp = s.lyric_comp(clip).unwrap();
    let (a, b) = comp.alignment.span(2, 2).unwrap();
    let r = view.row_rect(row, SIZE);
    let y = r.y + view.row_h() + 3.0 * TAKE_LANE_HEIGHT + LYRIC_ROW_H / 2.0;
    let pos = Point::new(
        (view.x_of_folder(&s, &c, a) + view.x_of_folder(&s, &c, b)) / 2.0,
        y,
    );
    assert_eq!(
        view.hit_test(pos, SIZE, &s),
        Some(Hit::LyricWord { clip, word: 2 })
    );
    let (acts, _) = run(
        &mut view,
        press(pos, 1, Modifiers::NONE, PointerButton::Primary),
        &s,
    );
    assert_eq!(
        acts,
        vec![Action::LyricComp(LyricCompOp::UseBest {
            clip,
            words: Some((2, 2))
        })]
    );
    let (_, req) = run(
        &mut view,
        press(pos, 1, Modifiers::NONE, PointerButton::Secondary),
        &s,
    );
    let Some(HostRequest::ContextMenu { items, .. }) = req.into_iter().next() else {
        panic!("no menu");
    };
    let labels: Vec<String> = items.iter().map(|i| i.label.clone()).collect();
    for t in 1..=3 {
        assert!(
            labels
                .iter()
                .any(|l| l.starts_with(&format!("{t} · Take {t}"))),
            "{labels:?}"
        );
    }
    let chosen: Vec<bool> = items
        .iter()
        .take(3)
        .map(|i| i.checked == Some(true))
        .collect();
    assert_eq!(chosen, [false, false, true], "take 3 is in the comp");
    assert!(labels.contains(&"Use the Suggestions for This Line".to_string()));
    // Between the words: the row itself.
    let gap = Point::new(view.x_of_folder(&s, &c, b) + 4.0, y);
    assert_eq!(view.hit_test(gap, SIZE, &s), Some(Hit::LyricRow(clip)));
}

/// A folder with nothing heard offers Comp by Lyrics; with the lanes
/// closed there is no lyrics row.
#[test]
fn comp_by_lyrics_is_offered_until_the_words_are_heard() {
    let (mut s, clip, row) = session();
    let source = s.project().clip(clip).unwrap().as_takes().unwrap().takes[0].source;
    s.edit(Command::SetSourceWords {
        source,
        words: None,
    })
    .unwrap();
    let mut view = ArrangerView::new(Theme::default());
    texts(&mut view, &s);
    assert_eq!(view.row_rect(row, SIZE).h, view.row_h());
    let c = s.project().clip(clip).unwrap().clone();
    let HostRequest::ContextMenu { items, .. } =
        ArrangerView::clip_menu(&s, &c, Point::new(0.0, 0.0))
    else {
        panic!("no menu");
    };
    let hear = items.iter().find(|i| i.label == "Comp by Lyrics").unwrap();
    assert_eq!(
        hear.action,
        Some(Action::Several(vec![
            Action::LyricComp(LyricCompOp::Hear { clip }),
            Action::ToggleTakeLanes(clip),
        ]))
    );
    s.dispatch(Action::ToggleTakeLanes(clip)).unwrap();
    texts(&mut view, &s);
    assert_eq!(
        view.row_rect(row, SIZE).h,
        view.row_h() + 3.0 * TAKE_LANE_HEIGHT,
        "no words: no lyrics row"
    );
    let HostRequest::ContextMenu { items, .. } =
        ArrangerView::take_menu(&s, &c, 0, Point::new(0.0, 0.0))
    else {
        panic!("no menu");
    };
    assert!(
        items
            .iter()
            .any(|i| i.label.starts_with("Comp by Lyrics (Hear the Words")),
        "{:?}",
        items.iter().map(|i| &i.label).collect::<Vec<_>>()
    );
}
