//! Others in a live session shown in the arranger: a named line at their
//! playhead, their selected clips outlined.

use super::*;
use faderframe_session::collab::CollabOp;
use faderframe_ui_canvas::{DrawOp, RecordingPainter};
use std::time::{Duration, Instant};

const SIZE: Size = Size::new(1400.0, 700.0);

fn pump(a: &mut Session, b: &mut Session, done: impl Fn(&Session, &Session) -> bool) {
    let t = Instant::now();
    while !done(a, b) {
        a.tick(0.005);
        b.tick(0.005);
        assert!(t.elapsed() < Duration::from_secs(20), "did not settle");
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn the_others_are_drawn_where_they_are_in_their_colours() {
    let mut a = Session::demo(faderframe_engine::EngineConfig::default()).unwrap();
    a.dispatch(Action::Collab(CollabOp::Host {
        bind: Some(std::net::SocketAddr::from(([127, 0, 0, 1], 0))),
        name: "Ana".into(),
    }))
    .unwrap();
    let invite = a.collaboration().unwrap().invite.unwrap();
    let mut b = Session::new(
        faderframe_project::Project::new("Empty", 48_000),
        None,
        faderframe_engine::EngineConfig::default(),
    )
    .unwrap();
    b.dispatch(Action::Collab(CollabOp::Join {
        address: invite.addresses[0].clone(),
        code: invite.code,
        name: "Bo".into(),
    }))
    .unwrap();
    pump(&mut a, &mut b, |a, b| {
        a.collab_settled() && b.collab_settled()
    });
    // Bo selects a clip and moves the playhead.
    let clip = *b.project().clips.keys().next().unwrap();
    b.dispatch(Action::SelectClips {
        clips: vec![clip],
        mode: SelectMode::Replace,
    })
    .unwrap();
    b.dispatch(Action::Transport(TransportAction::Locate(
        MusicalTime::from_quarters(8.0),
    )))
    .unwrap();
    pump(&mut a, &mut b, |a, _| {
        a.collab_presences()
            .first()
            .is_some_and(|o| o.presence.clips == [clip] && o.presence.playhead.quarters() > 7.9)
    });
    let mut view = ArrangerView::new(Theme::default());
    let mut p = RecordingPainter::new();
    view.paint(&mut p, SIZE, &a, &Theme::default());
    let tab = p.ops.iter().find_map(|o| match o {
        DrawOp::Text(t, r) if t == "Bo" => Some(*r),
        _ => None,
    });
    let tab = tab.expect("Bo's name at Bo's playhead");
    let x = view.x_of(MusicalTime::from_quarters(8.0));
    assert!((tab.x - x).abs() < 1.0, "{} vs {x}", tab.x);
    // Alone again: nothing of the others.
    b.dispatch(Action::Collab(CollabOp::Leave)).unwrap();
    pump(&mut a, &mut b, |a, _| a.collab_presences().is_empty());
    let mut p = RecordingPainter::new();
    view.paint(&mut p, SIZE, &a, &Theme::default());
    assert!(!p.texts().contains(&"Bo"));
}
