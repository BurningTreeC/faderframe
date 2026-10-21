#![allow(clippy::unwrap_used)]

use crate::chain::{self, Role};
use crate::{ChannelOverviewView, Field, InspectorHit, InspectorView, OverviewHit, Section};
use faderframe_core::TrackId;
use faderframe_engine::EngineConfig;
use faderframe_project::{Command, MonitorMode};
use faderframe_session::{Action, SelectMode, Session};
use faderframe_ui_canvas::{
    CanvasView, EventCx, HostRequest, Modifiers, Point, PointerButton, RecordingPainter, Size,
    Theme, ViewEvent, access,
};

fn session() -> Session {
    Session::demo(EngineConfig::default()).unwrap()
}

fn id(s: &Session, name: &str) -> TrackId {
    s.project()
        .tracks
        .iter()
        .find(|t| t.name == name)
        .unwrap_or_else(|| panic!("no track {name}"))
        .id
}

fn select(s: &mut Session, names: &[&str]) {
    let tracks = names.iter().map(|n| id(s, n)).collect();
    s.dispatch(Action::SelectTracks {
        tracks,
        mode: SelectMode::Replace,
    })
    .unwrap();
}

/// Send `ev` to a view; dispatch what it emits; hand back its requests.
fn send(
    view: &mut dyn CanvasView<Session, Action>,
    ev: ViewEvent,
    size: Size,
    s: &mut Session,
) -> Vec<HostRequest<Action>> {
    let mut actions = Vec::new();
    let mut requests = Vec::new();
    let mut cx = EventCx::new(&mut actions, &mut requests);
    view.event(&ev, size, s, &mut cx);
    for a in actions {
        s.dispatch(a).unwrap();
    }
    requests
}

/// What a view emits for `ev` (not dispatched).
fn emitted(
    view: &mut dyn CanvasView<Session, Action>,
    ev: ViewEvent,
    size: Size,
    s: &Session,
) -> (Vec<Action>, Vec<HostRequest<Action>>) {
    let mut actions = Vec::new();
    let mut requests = Vec::new();
    let mut cx = EventCx::new(&mut actions, &mut requests);
    view.event(&ev, size, s, &mut cx);
    (actions, requests)
}

fn down(pos: Point) -> ViewEvent {
    ViewEvent::PointerDown {
        pos,
        button: PointerButton::Primary,
        modifiers: Modifiers::NONE,
        clicks: 1,
    }
}

fn right(pos: Point) -> ViewEvent {
    ViewEvent::PointerDown {
        pos,
        button: PointerButton::Secondary,
        modifiers: Modifiers::NONE,
        clicks: 1,
    }
}

fn drag_to(pos: Point) -> ViewEvent {
    ViewEvent::PointerMove {
        pos,
        modifiers: Modifiers::NONE,
        dragging: true,
    }
}

fn up(pos: Point) -> ViewEvent {
    ViewEvent::PointerUp {
        pos,
        button: PointerButton::Primary,
        modifiers: Modifiers::NONE,
    }
}

fn texts(view: &mut dyn CanvasView<Session, Action>, size: Size, s: &Session) -> Vec<String> {
    let mut p = RecordingPainter::new();
    view.paint(&mut p, size, s, &Theme::default());
    p.texts().iter().map(|t| t.to_string()).collect()
}

/// The first point of `size` (in steps) where `want` holds.
fn find(size: Size, step: f32, want: impl Fn(Point) -> bool) -> Option<Point> {
    let mut y = step / 2.0;
    while y < size.h {
        let mut x = step / 2.0;
        while x < size.w {
            let p = Point::new(x, y);
            if want(p) {
                return Some(p);
            }
            x += step;
        }
        y += step;
    }
    None
}

fn menu_labels(r: &HostRequest<Action>) -> Vec<(String, Option<bool>)> {
    match r {
        HostRequest::ContextMenu { items, .. } => {
            items.iter().map(|i| (i.label.clone(), i.checked)).collect()
        }
        _ => panic!("a menu"),
    }
}

// --- the chain ---------------------------------------------------------------

/// Devices come in signal-flow order — the input stage, then the inserts,
/// a container followed by what its chains hold — each with its role.
#[test]
fn the_chain_lists_devices_in_signal_flow_order() {
    let s = session();
    let bass = s.project().track(id(&s, "Bass")).unwrap();
    let d = chain::devices(&s, bass);
    let roles: Vec<Role> = d.iter().map(|d| d.role).collect();
    assert_eq!(roles, [Role::InputStage, Role::Effect, Role::Effect]);
    assert_eq!(d[1].name, "Compressor");
    assert_eq!(
        (d[0].insert, d[1].insert, d[2].insert),
        (None, Some(0), Some(1))
    );
    assert!(d.iter().all(|d| d.depth == 0 && !d.bypass && !d.failed));
    // The drum bus: a container, then its chains' devices one level in.
    let bus = s.project().track(id(&s, "Drum Bus")).unwrap();
    let d = chain::devices(&s, bus);
    assert_eq!(d[0].role, Role::Container);
    assert!(d[0].chains >= 1);
    assert!(d.len() > 1 && d[1..].iter().all(|x| x.depth == 1 && x.insert.is_none()));
    assert_eq!(
        d.len(),
        bus.slots().len(),
        "every slot, as Track::slots lists them"
    );
    // A MIDI effect before the instrument.
    let arp = s.project().track(id(&s, "Arp Synth")).unwrap();
    let roles: Vec<Role> = chain::devices(&s, arp).iter().map(|d| d.role).collect();
    assert_eq!(roles.first(), Some(&Role::MidiEffect));
    assert!(roles.contains(&Role::Instrument), "{roles:?}");
    // Removing goes the way the mixer goes: the input stage by its own
    // action, a device in a container out of its chain.
    let bass_devices = chain::devices(&s, bass);
    assert!(matches!(
        chain::remove(bass, &bass_devices[0]),
        Some(Action::SetPreamp { model: None, .. })
    ));
    assert!(matches!(
        chain::remove(bass, &bass_devices[2]),
        Some(Action::Edit(Command::RemovePlugin { .. }))
    ));
    let bus_devices = chain::devices(&s, bus);
    assert!(matches!(
        chain::remove(bus, &bus_devices[1]),
        Some(Action::RemoveFromChain { .. })
    ));
    // Dropping: the slot among the other inserts before the gap.
    assert_eq!(
        chain::drop_index(&bass_devices, &bass_devices[2], 1),
        Some(0)
    );
    assert_eq!(
        chain::drop_index(&bass_devices, &bass_devices[1], 3),
        Some(1)
    );
    assert_eq!(chain::drop_index(&bass_devices, &bass_devices[0], 3), None);
}

/// The track shown: the one clicked last among the selected, else the
/// first selected in the arranger's order; removed tracks are skipped.
#[test]
fn the_inspected_track_follows_the_selection() {
    let mut s = session();
    assert_eq!(chain::inspected_track(&s), None);
    select(&mut s, &["Pad"]);
    assert_eq!(chain::inspected_track(&s), Some(id(&s, "Pad")));
    // Several: the anchor (the single click) stays the one shown.
    s.dispatch(Action::SelectTracks {
        tracks: vec![id(&s, "Bass")],
        mode: SelectMode::Add,
    })
    .unwrap();
    assert_eq!(chain::selected_count(&s), 2);
    assert_eq!(chain::inspected_track(&s), Some(id(&s, "Pad")));
    // The shown track removed: the other selected one.
    let pad = id(&s, "Pad");
    s.dispatch(Action::Edit(Command::RemoveTrack { track: pad }))
        .unwrap();
    let now = chain::inspected_track(&s);
    assert!(now.is_none() || now == Some(id(&s, "Bass")), "{now:?}");
    assert!(now.is_none_or(|t| s.project().track(t).is_some()));
}

// --- the Channel Overview ------------------------------------------------------

const ROW: Size = Size::new(1400.0, 190.0);

#[test]
fn the_overview_shows_the_signal_path_of_the_selected_track() {
    let mut s = session();
    let mut view = ChannelOverviewView::new(Theme::default());
    let t = texts(&mut view, ROW, &s);
    assert!(t.iter().any(|t| t.starts_with("Select a track")), "{t:?}");
    select(&mut s, &["Bass"]);
    let t = texts(&mut view, ROW, &s);
    for want in [
        "INPUT",
        "Compressor",
        "Saturator",
        "CHANNEL",
        "SENDS",
        "OUTPUT",
        "Master",
    ] {
        assert!(t.iter().any(|x| x.contains(want)), "{want} in {t:?}");
    }
    assert!(t.iter().any(|x| x.contains("INPUT STAGE")), "{t:?}");
    // A MIDI track: its MIDI input, its MIDI effect, the instrument it
    // plays, no channel.
    select(&mut s, &["Chords"]);
    let t = texts(&mut view, ROW, &s);
    assert!(t.iter().any(|x| x.contains("Chord")), "{t:?}");
    assert!(t.iter().any(|x| x == "PLAYS"), "{t:?}");
    assert!(t.iter().any(|x| x.contains("Arp Synth")), "{t:?}");
    assert!(!t.iter().any(|x| x == "CHANNEL"), "{t:?}");
    // Several selected: it says which one it shows.
    select(&mut s, &["Bass", "Pad"]);
    let t = texts(&mut view, ROW, &s);
    assert!(t.iter().any(|x| x.contains("1 of 2 selected")), "{t:?}");
}

#[test]
fn overview_cards_bypass_open_remove_and_move() {
    let mut s = session();
    select(&mut s, &["Bass"]);
    let bass = id(&s, "Bass");
    let mut view = ChannelOverviewView::new(Theme::default());
    texts(&mut view, ROW, &s);
    let at = |view: &ChannelOverviewView, s: &Session, h: OverviewHit| {
        find(ROW, 4.0, |p| view.hit(p, ROW, s) == Some(h)).unwrap()
    };
    // The power button bypasses the compressor (card 1; 0 is the preamp).
    let power = at(&view, &s, OverviewHit::Bypass(1));
    send(&mut view, down(power), ROW, &mut s);
    assert!(s.project().track(bass).unwrap().inserts[0].bypass);
    send(&mut view, down(power), ROW, &mut s);
    assert!(!s.project().track(bass).unwrap().inserts[0].bypass);
    // A click on the card opens its editor.
    let card = at(&view, &s, OverviewHit::Device(1));
    send(&mut view, down(card), ROW, &mut s);
    let (actions, _) = emitted(&mut view, up(card), ROW, &s);
    assert!(
        matches!(
            actions.as_slice(),
            [Action::OpenPluginEditor { generic: false, .. }]
        ),
        "{}",
        actions.len()
    );
    // Dragging the saturator before the compressor moves it there.
    let names = |s: &Session| -> Vec<String> {
        s.project()
            .track(bass)
            .unwrap()
            .inserts
            .iter()
            .map(|i| i.plugin.name.clone())
            .collect()
    };
    let before = names(&s);
    let saturator = at(&view, &s, OverviewHit::Device(2));
    let compressor = at(&view, &s, OverviewHit::Device(1));
    send(&mut view, down(saturator), ROW, &mut s);
    let target = Point::new(compressor.x - 40.0, compressor.y);
    send(&mut view, drag_to(target), ROW, &mut s);
    texts(&mut view, ROW, &s);
    send(&mut view, up(target), ROW, &mut s);
    let after = names(&s);
    assert_eq!(after, [before[1].clone(), before[0].clone()]);
    // The input stage does not move (the drag is a no-op).
    let preamp = at(&view, &s, OverviewHit::Device(0));
    send(&mut view, down(preamp), ROW, &mut s);
    send(
        &mut view,
        drag_to(Point::new(preamp.x + 400.0, preamp.y)),
        ROW,
        &mut s,
    );
    let (actions, _) = emitted(
        &mut view,
        up(Point::new(preamp.x + 400.0, preamp.y)),
        ROW,
        &s,
    );
    assert!(actions.is_empty());
    // × removes a device; undo brings it back.
    let remove = at(&view, &s, OverviewHit::Remove(2));
    send(&mut view, down(remove), ROW, &mut s);
    assert_eq!(s.project().track(bass).unwrap().inserts.len(), 1);
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(s.project().track(bass).unwrap().inserts.len(), 2);
    // "+" opens the plugin browser for the next insert; right-click: the
    // mixer's quick list.
    texts(&mut view, ROW, &s);
    let add = at(&view, &s, OverviewHit::Add);
    let (actions, _) = emitted(&mut view, down(add), ROW, &s);
    assert!(matches!(
        actions.as_slice(),
        [Action::OpenPluginBrowser {
            target: faderframe_session::PluginTarget::Insert(2),
            ..
        }]
    ));
    let (_, requests) = emitted(&mut view, right(add), ROW, &s);
    assert!(
        menu_labels(&requests[0])
            .iter()
            .any(|(l, _)| l == "Browse Plugins…")
    );
    // Right-click on a card: the mixer's insert menu.
    let card = at(&view, &s, OverviewHit::Device(1));
    let (_, requests) = emitted(&mut view, right(card), ROW, &s);
    let labels = menu_labels(&requests[0]);
    assert!(labels.iter().any(|(l, _)| l == "Bypass"), "{labels:?}");
    assert!(labels.iter().any(|(l, _)| l == "Remove"), "{labels:?}");
}

#[test]
fn the_overview_routes_and_mixes_through_the_session() {
    let mut s = session();
    select(&mut s, &["Bass"]);
    let bass = id(&s, "Bass");
    let mut view = ChannelOverviewView::new(Theme::default());
    texts(&mut view, ROW, &s);
    let at = |view: &ChannelOverviewView, s: &Session, h: OverviewHit| {
        find(ROW, 3.0, |p| view.hit(p, ROW, s) == Some(h)).unwrap()
    };
    // Output: the mixer's output menu, Master checked.
    let pt = at(&view, &s, OverviewHit::Output);
    let (_, r) = emitted(&mut view, down(pt), ROW, &s);
    let labels = menu_labels(&r[0]);
    assert!(
        labels.contains(&("Master".to_string(), Some(true))),
        "{labels:?}"
    );
    // Input: the track's input choices.
    let pt = at(&view, &s, OverviewHit::Input);
    let (_, r) = emitted(&mut view, down(pt), ROW, &s);
    assert!(!menu_labels(&r[0]).is_empty());
    // Mute and solo.
    let pt = at(&view, &s, OverviewHit::Mute);
    send(&mut view, down(pt), ROW, &mut s);
    assert!(s.project().track(bass).unwrap().mute);
    let pt = at(&view, &s, OverviewHit::Solo);
    send(&mut view, down(pt), ROW, &mut s);
    assert!(s.project().track(bass).unwrap().solo);
    // The level typed.
    let pt = at(&view, &s, OverviewHit::Volume);
    let r = send(&mut view, down(pt), ROW, &mut s);
    let Some(HostRequest::TextInput { commit, .. }) = r.into_iter().next() else {
        panic!("a text entry");
    };
    s.dispatch(commit("-6.5").unwrap()).unwrap();
    assert!((s.project().track(bass).unwrap().volume_db + 6.5).abs() < 1e-6);
    // Screen readers: the devices with their power buttons.
    let nodes = view.accessible(ROW, &s);
    let order = access::focus_order(&nodes);
    assert!(
        order
            .iter()
            .any(|n| n.label.starts_with("Device 2: Compressor"))
    );
    let power = order.iter().find(|n| n.label == "Compressor on").unwrap();
    assert_eq!(power.checked, Some(true));
    s.dispatch(power.activate.clone().unwrap()).unwrap();
    assert!(s.project().track(bass).unwrap().inserts[0].bypass);
}

// --- the Inspector ---------------------------------------------------------------

const COLUMN: Size = Size::new(300.0, 1300.0);

fn rows_of(view: &mut InspectorView, s: &Session) -> Vec<String> {
    texts(view, COLUMN, s)
}

/// Each kind of track shows its own settings.
#[test]
fn the_inspector_shows_what_each_kind_of_track_has() {
    let mut s = session();
    let mut view = InspectorView::new(Theme::default());
    let t = rows_of(&mut view, &s);
    assert!(
        t.iter().any(|x| x.starts_with("No track selected")),
        "{t:?}"
    );
    // An audio track: input, output, monitoring, format, record, the
    // devices, its channel strip.
    select(&mut s, &["Bass"]);
    let t = rows_of(&mut view, &s);
    for want in [
        "Bass",
        "Input",
        "Output",
        "Master",
        "Monitor",
        "Format",
        "Mono",
        "Record",
        "Freeze",
        "Compressor",
        "Saturator",
    ] {
        assert!(
            t.iter().any(|x| x == want || x.contains(want)),
            "{want} in {t:?}"
        );
    }
    assert!(t.iter().any(|x| x.starts_with("▾ DEVICES (3)")), "{t:?}");
    assert!(t.iter().any(|x| x == "▾ CHANNEL"), "{t:?}");
    // The hosted strip paints the track's console strip (its name on the
    // scribble strip, its kind on the header).
    assert!(t.iter().any(|x| x == "AUDIO"), "{t:?}");
    // A MIDI track: MIDI input, the instrument it plays, its MIDI device,
    // live play, MPE; no format.
    select(&mut s, &["Chords"]);
    let t = rows_of(&mut view, &s);
    for want in [
        "MIDI In",
        "Plays",
        "MIDI Out",
        "Play Live",
        "MPE",
        "Arp Synth",
    ] {
        assert!(t.iter().any(|x| x.contains(want)), "{want} in {t:?}");
    }
    assert!(!t.iter().any(|x| x == "Format"), "{t:?}");
    // An aux: what reaches it (no input to choose), its output.
    select(&mut s, &["Space"]);
    let t = rows_of(&mut view, &s);
    assert!(t.iter().any(|x| x.contains("send")), "{t:?}");
    assert!(!t.iter().any(|x| x == "Monitor"), "{t:?}");
    // The folder: its contents, open or closed.
    select(&mut s, &["Arpeggios"]);
    let t = rows_of(&mut view, &s);
    assert!(t.iter().any(|x| x.contains("2 tracks")), "{t:?}");
    assert!(
        t.iter().any(|x| x == "This kind of track holds no devices"),
        "{t:?}"
    );
    // The master: listening and the console.
    let master = s.project().master().unwrap().id;
    s.dispatch(Action::SelectTracks {
        tracks: vec![master],
        mode: SelectMode::Replace,
    })
    .unwrap();
    let t = rows_of(&mut view, &s);
    for want in ["Listen", "Console", "Speakers", "MASTER"] {
        assert!(t.iter().any(|x| x.contains(want)), "{want} in {t:?}");
    }
}

/// A folder (and a folder bus) shows its Group switch: pressing it groups
/// what the folder holds and back; the row says so, and is a check box for
/// screen readers.
#[test]
fn a_folder_shows_its_group_switch() {
    let mut s = session();
    select(&mut s, &["Arpeggios"]);
    let folder = id(&s, "Arpeggios");
    let mut view = InspectorView::new(Theme::default());
    let t = rows_of(&mut view, &s);
    assert!(t.iter().any(|x| x == "Grouped"), "{t:?}");
    let at = |view: &InspectorView, s: &Session| {
        find(COLUMN, 3.0, |p| {
            view.hit(p, COLUMN, s) == Some(InspectorHit::Field(Field::FolderGroup))
        })
        .unwrap()
    };
    let pt = at(&view, &s);
    let (actions, _) = emitted(&mut view, down(pt), COLUMN, &s);
    assert_eq!(actions, vec![Action::SetFolderGroup { folder, on: true }]);
    send(&mut view, down(pt), COLUMN, &mut s);
    assert!(s.folder_grouped(folder));
    let t = rows_of(&mut view, &s);
    assert!(t.iter().any(|x| x == "edited and armed as one"), "{t:?}");
    let nodes = view.accessible(COLUMN, &s);
    let order = access::focus_order(&nodes);
    let check = order
        .iter()
        .find(|n| n.label == "Grouped")
        .expect("its check box");
    assert_eq!(check.checked, Some(true));
    // A folder bus has it too.
    s.dispatch(Action::SetFolderBus { folder, on: true })
        .unwrap();
    let t = rows_of(&mut view, &s);
    assert!(t.iter().any(|x| x == "Grouped"), "{t:?}");
    let pt = at(&view, &s);
    send(&mut view, down(pt), COLUMN, &mut s);
    assert!(!s.folder_grouped(folder));
}

#[test]
fn inspector_fields_send_session_actions() {
    let mut s = session();
    select(&mut s, &["Bass"]);
    let bass = id(&s, "Bass");
    let mut view = InspectorView::new(Theme::default());
    rows_of(&mut view, &s);
    let at = |view: &InspectorView, s: &Session, h: InspectorHit| {
        find(COLUMN, 3.0, |p| view.hit(p, COLUMN, s) == Some(h)).unwrap()
    };
    // Monitoring: Auto.
    let pt = at(&view, &s, InspectorHit::Segment(Field::Monitor, 1));
    send(&mut view, down(pt), COLUMN, &mut s);
    assert_eq!(s.project().track(bass).unwrap().monitor, MonitorMode::Auto);
    // Record arm.
    let pt = at(&view, &s, InspectorHit::Field(Field::Record));
    send(&mut view, down(pt), COLUMN, &mut s);
    assert!(s.project().track(bass).unwrap().record_arm);
    // Output: the mixer's own menu.
    let pt = at(&view, &s, InspectorHit::Field(Field::Output));
    let (_, r) = emitted(&mut view, down(pt), COLUMN, &s);
    assert!(menu_labels(&r[0]).contains(&("Master".to_string(), Some(true))));
    // Format: the session's choices, Mono checked; Stereo switches.
    let pt = at(&view, &s, InspectorHit::Field(Field::Format));
    let r = send(&mut view, down(pt), COLUMN, &mut s);
    let HostRequest::ContextMenu { items, .. } = &r[0] else {
        panic!("a menu");
    };
    let stereo = items.iter().find(|i| i.label == "Stereo").unwrap();
    assert_eq!(stereo.checked, Some(false));
    s.dispatch(stereo.action.clone().unwrap()).unwrap();
    assert_eq!(
        s.project().track(bass).unwrap().layout,
        faderframe_core::ChannelLayout::Stereo
    );
    // The name: a text entry renaming it.
    let pt = at(&view, &s, InspectorHit::Name);
    let r = send(&mut view, down(pt), COLUMN, &mut s);
    let Some(HostRequest::TextInput { commit, .. }) = r.into_iter().next() else {
        panic!("a text entry");
    };
    s.dispatch(commit("Bass DI").unwrap()).unwrap();
    assert_eq!(s.project().track(bass).unwrap().name, "Bass DI");
    // The header right-clicked: the strip's track menu.
    let (_, r) = emitted(&mut view, right(Point::new(200.0, 40.0)), COLUMN, &s);
    assert!(menu_labels(&r[0]).iter().any(|(l, _)| l == "Remove Track"));
    // A section folds by its title (the devices go, the others move up).
    view.toggle_section(Section::Devices);
    let t = rows_of(&mut view, &s);
    assert!(t.iter().any(|x| x.starts_with("▸ DEVICES")));
    assert!(!t.iter().any(|x| x == "Compressor"));
    let title = at(&view, &s, InspectorHit::Section(Section::Devices));
    send(&mut view, down(title), COLUMN, &mut s);
    assert!(!view.is_folded(Section::Devices));
}

#[test]
fn the_inspector_device_list_works_like_the_overview() {
    let mut s = session();
    select(&mut s, &["Bass"]);
    let bass = id(&s, "Bass");
    let mut view = InspectorView::new(Theme::default());
    rows_of(&mut view, &s);
    let at = |view: &InspectorView, s: &Session, h: InspectorHit| {
        find(COLUMN, 3.0, |p| view.hit(p, COLUMN, s) == Some(h)).unwrap()
    };
    let pt = at(&view, &s, InspectorHit::Power(1));
    send(&mut view, down(pt), COLUMN, &mut s);
    assert!(s.project().track(bass).unwrap().inserts[0].bypass);
    // Drag the saturator above the compressor.
    let sat = at(&view, &s, InspectorHit::Device(2));
    let comp = at(&view, &s, InspectorHit::Device(1));
    send(&mut view, down(sat), COLUMN, &mut s);
    let to = Point::new(comp.x, comp.y - 8.0);
    send(&mut view, drag_to(to), COLUMN, &mut s);
    send(&mut view, up(to), COLUMN, &mut s);
    let first = &s.project().track(bass).unwrap().inserts[0];
    assert!(
        first.plugin.name.contains("Saturator"),
        "{}",
        first.plugin.name
    );
    // The input stage's × takes the preamp out.
    let pt = at(&view, &s, InspectorHit::Remove(0));
    send(&mut view, down(pt), COLUMN, &mut s);
    assert!(s.project().track(bass).unwrap().preamp.is_none());
    // "+" opens the plugin browser.
    let pt = at(&view, &s, InspectorHit::AddDevice);
    let (actions, _) = emitted(&mut view, down(pt), COLUMN, &s);
    assert!(matches!(
        actions.as_slice(),
        [Action::OpenPluginBrowser { .. }]
    ));
}

/// The channel section is the mixer's strip: its controls work here, and
/// what it asks for (menus, text entry) lands where it was pressed.
#[test]
fn the_inspector_hosts_the_tracks_channel_strip() {
    let mut s = session();
    select(&mut s, &["Pad"]);
    let pad = id(&s, "Pad");
    let mut view = InspectorView::new(Theme::default());
    rows_of(&mut view, &s);
    let nodes = view.accessible(COLUMN, &s);
    let all = access::focus_order(&nodes);
    let mute = all.iter().find(|n| n.label == "Mute").unwrap();
    assert!(view.hit(mute.bounds.center(), COLUMN, &s) == Some(InspectorHit::Strip));
    // A click on the strip's mute (forwarded in the strip's coordinates).
    send(&mut view, down(mute.bounds.center()), COLUMN, &mut s);
    send(&mut view, up(mute.bounds.center()), COLUMN, &mut s);
    assert!(s.project().track(pad).unwrap().mute);
    // Its fader right-clicked: MIDI learn, the menu where the pointer is.
    let volume = all.iter().find(|n| n.label == "Volume").unwrap();
    let at = volume.bounds.center();
    let (_, r) = emitted(&mut view, right(at), COLUMN, &s);
    let Some(HostRequest::ContextMenu { at: shown, .. }) = r.first() else {
        panic!("a menu");
    };
    assert!(shown.distance(at) < 0.5, "{shown:?} vs {at:?}");
    // Dragging the fader is one gesture (begun and ended there).
    let (actions, _) = emitted(&mut view, down(at), COLUMN, &s);
    assert!(matches!(actions.first(), Some(Action::BeginGesture(_))));
    s.dispatch(actions[0].clone()).unwrap();
    for a in actions.into_iter().skip(1) {
        s.dispatch(a).unwrap();
    }
    let before = s.project().track(pad).unwrap().volume_db;
    send(
        &mut view,
        drag_to(Point::new(at.x, at.y - 40.0)),
        COLUMN,
        &mut s,
    );
    let (actions, _) = emitted(&mut view, up(Point::new(at.x, at.y - 40.0)), COLUMN, &s);
    assert!(matches!(actions.as_slice(), [Action::EndGesture]));
    s.dispatch(Action::EndGesture).unwrap();
    assert!(s.project().track(pad).unwrap().volume_db > before);
    // The strip shows no inserts of its own: the device list does.
    let t = rows_of(&mut view, &s);
    assert!(!t.iter().any(|x| x == "INSERTS"), "{t:?}");
}

/// Screen readers get every line: a menu's choices as toggles under it,
/// switches as check boxes, the strip's controls under Channel.
#[test]
fn the_inspector_is_accessible() {
    let mut s = session();
    select(&mut s, &["Bass"]);
    let bass = id(&s, "Bass");
    let mut view = InspectorView::new(Theme::default());
    rows_of(&mut view, &s);
    let nodes = view.accessible(COLUMN, &s);
    assert!(nodes[0].label.starts_with("Inspector: Bass"));
    let order = access::focus_order(&nodes);
    let stereo = order
        .iter()
        .find(|n| n.label == "Stereo" && n.checked.is_some())
        .unwrap();
    s.dispatch(stereo.activate.clone().unwrap()).unwrap();
    assert_eq!(
        s.project().track(bass).unwrap().layout,
        faderframe_core::ChannelLayout::Stereo
    );
    let record = order.iter().find(|n| n.label == "Record").unwrap();
    assert_eq!(record.checked, Some(false));
    let auto = order.iter().find(|n| n.label == "Monitor: Auto").unwrap();
    s.dispatch(auto.activate.clone().unwrap()).unwrap();
    assert_eq!(s.project().track(bass).unwrap().monitor, MonitorMode::Auto);
    assert!(
        order.iter().any(|n| n.label == "Volume"),
        "the strip's fader"
    );
    // Nothing selected: one label says so.
    s.dispatch(Action::ClearSelection).unwrap();
    let nodes = view.accessible(COLUMN, &s);
    assert_eq!(nodes.len(), 1);
    assert!(nodes[0].label.contains("no track selected"));
}

/// A folder's Folder Bus switch: on, the folder sums what it holds and
/// the Inspector shows the bus's controls (what feeds it, its output,
/// format, devices and a full strip); off, a plain folder again — each
/// one undo step, as the folder's menu's Folder Is a Bus.
#[test]
fn a_folders_bus_switch_makes_it_a_folder_bus_with_its_controls() {
    let mut s = session();
    select(&mut s, &["Arpeggios"]);
    let folder = id(&s, "Arpeggios");
    let mut view = InspectorView::new(Theme::default());
    let t = rows_of(&mut view, &s);
    assert!(t.iter().any(|x| x == "Folder Bus"), "{t:?}");
    assert!(t.iter().any(|x| x.contains("a plain folder")), "{t:?}");
    assert!(!t.iter().any(|x| x == "Output"), "a plain folder: {t:?}");
    let switch = |view: &InspectorView, s: &Session| {
        find(COLUMN, 3.0, |p| {
            view.hit(p, COLUMN, s) == Some(InspectorHit::Field(Field::FolderBus))
        })
        .unwrap()
    };
    let at = switch(&view, &s);
    send(&mut view, down(at), COLUMN, &mut s);
    assert!(s.project().track(folder).unwrap().is_folder_bus());
    let t = rows_of(&mut view, &s);
    for want in [
        "Folder Bus · Stereo",
        "sums its tracks",
        "2 tracks · open",
        "Output",
        "Master",
        "Format",
        "Group",
        "Automation",
        "FOLDER BUS",
    ] {
        assert!(t.iter().any(|x| x.contains(want)), "{want} in {t:?}");
    }
    // What feeds it: the tracks it holds that play audio through it (the
    // instrument; its MIDI track plays the instrument).
    let input = s.project().track(folder).unwrap();
    assert_eq!(chain::input_label(&s, input), "1 track");
    // Its devices: a bus takes inserts.
    assert!(
        !t.iter().any(|x| x == "This kind of track holds no devices"),
        "{t:?}"
    );
    // The switch for screen readers: a check box, ticked.
    let nodes = view.accessible(COLUMN, &s);
    let order = access::focus_order(&nodes);
    let node = order.iter().find(|n| n.label == "Folder Bus").unwrap();
    assert_eq!(node.checked, Some(true));
    // Off again: a plain folder (one undo step each way).
    let at = switch(&view, &s);
    send(&mut view, down(at), COLUMN, &mut s);
    let f = s.project().track(folder).unwrap();
    assert!(!f.is_folder_bus() && f.is_folder());
    s.dispatch(Action::Undo).unwrap();
    assert!(s.project().track(folder).unwrap().is_folder_bus());
}

/// A track's Ripple line: pressing it leaves the track out of ripples
/// (edits elsewhere never move it) and back.
#[test]
fn a_track_can_be_left_out_of_ripples() {
    let mut s = session();
    select(&mut s, &["Bass"]);
    let bass = id(&s, "Bass");
    let mut view = InspectorView::new(Theme::default());
    let t = rows_of(&mut view, &s);
    assert!(t.iter().any(|x| x == "follows edits"), "{t:?}");
    let pt = find(COLUMN, 3.0, |p| {
        view.hit(p, COLUMN, &s) == Some(InspectorHit::Field(Field::Ripple))
    })
    .unwrap();
    let (actions, _) = emitted(&mut view, down(pt), COLUMN, &s);
    assert_eq!(
        actions,
        vec![Action::Ripple(faderframe_session::RippleOp::Exclude {
            tracks: vec![bass],
            excluded: true,
        })]
    );
    send(&mut view, down(pt), COLUMN, &mut s);
    assert!(s.project().track(bass).unwrap().ripple_excluded);
    let t = rows_of(&mut view, &s);
    assert!(t.iter().any(|x| x == "left out · stays put"), "{t:?}");
}
