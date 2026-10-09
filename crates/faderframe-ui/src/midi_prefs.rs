//! Preferences → MIDI: input devices (on/off, activity), controller
//! mappings (MIDI learn) and transport functions to learn.

use crate::prefs::Preferences;
use crate::state::AppState;
use faderframe_project::{Command, MappingTarget, TransportControl};
use faderframe_session::{Action, MidiPortStatus};
use gtk::glib;
use gtk::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

/// Persist which devices are used (and which get clock).
fn save_disabled(app: &AppState) {
    let m = app.session.borrow().midi_preferences();
    let mut p = Preferences::load();
    p.midi_disabled_inputs = m.disabled_inputs;
    p.midi_disabled_outputs = m.disabled_outputs;
    p.midi_clock_outputs = m.clock_outputs;
    p.midi_mtc_outputs = m.mtc_outputs;
    if let Err(e) = p.save() {
        tracing::warn!("cannot save preferences: {e}");
    }
}

fn heading(text: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    l.add_css_class("sidebar-heading");
    l.set_xalign(0.0);
    l
}

struct Ports {
    list: gtk::ListBox,
    shown: RefCell<Vec<(String, bool, bool)>>,
    dots: RefCell<Vec<(String, gtk::Label)>>,
}

impl Ports {
    fn rebuild(&self, app: &Rc<AppState>, ports: &[MidiPortStatus]) {
        while let Some(c) = self.list.first_child() {
            self.list.remove(&c);
        }
        let mut dots = Vec::new();
        // Voice ports have their own section (Voice to MIDI).
        for p in ports
            .iter()
            .filter(|p| faderframe_session::voice::voice_channel(&p.key).is_none())
        {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
            row.add_css_class("midi-port");
            let dot = gtk::Label::new(Some("●"));
            dot.add_css_class("midi-led");
            row.append(&dot);
            let names = gtk::Box::new(gtk::Orientation::Vertical, 0);
            let name = gtk::Label::new(Some(&p.name));
            name.set_xalign(0.0);
            names.append(&name);
            let detail = gtk::Label::new(Some(&if p.is_virtual {
                "built-in (on-screen and computer keyboard)".to_string()
            } else if !p.enabled {
                "off".to_string()
            } else {
                // The device ("MPK mini 3") of "MPK mini 3:MPK mini 3 MIDI 1".
                let device = p.key.split_once(':').map_or(p.key.as_str(), |(d, _)| d);
                if p.connected {
                    device.to_string()
                } else {
                    format!("{device} · not connected")
                }
            }));
            detail.add_css_class("dim-label");
            detail.set_xalign(0.0);
            detail.set_ellipsize(gtk::pango::EllipsizeMode::End);
            names.append(&detail);
            names.set_hexpand(true);
            row.append(&names);
            if !p.is_virtual {
                let sw = gtk::Switch::new();
                sw.set_active(p.enabled);
                sw.set_valign(gtk::Align::Center);
                sw.set_tooltip_text(Some("Use this input"));
                let weak = Rc::downgrade(app);
                let key = p.key.clone();
                sw.connect_state_set(move |_, on| {
                    if let Some(a) = weak.upgrade() {
                        a.session.borrow_mut().set_midi_input_enabled(&key, on);
                        save_disabled(&a);
                    }
                    glib::Propagation::Proceed
                });
                row.append(&sw);
            }
            self.list.append(&row);
            dots.push((p.key.clone(), dot));
        }
        *self.dots.borrow_mut() = dots;
        *self.shown.borrow_mut() = ports
            .iter()
            .map(|p| (p.key.clone(), p.enabled, p.connected))
            .collect();
    }
}

fn output_rows(
    app: &Rc<AppState>,
    list: &gtk::ListBox,
    outs: &[faderframe_session::MidiOutputStatus],
) {
    while let Some(c) = list.first_child() {
        list.remove(&c);
    }
    if outs.is_empty() {
        let l = gtk::Label::new(Some("No MIDI outputs found."));
        l.add_css_class("dim-label");
        l.set_margin_top(6);
        l.set_margin_bottom(6);
        list.append(&l);
        return;
    }
    for o in outs {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        row.add_css_class("midi-port");
        let names = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let name = gtk::Label::new(Some(&o.name));
        name.set_xalign(0.0);
        names.append(&name);
        let device = o.key.split_once(':').map_or(o.key.as_str(), |(d, _)| d);
        let detail = gtk::Label::new(Some(&if !o.enabled {
            "off".to_string()
        } else if o.connected {
            device.to_string()
        } else {
            format!("{device} · not connected")
        }));
        detail.add_css_class("dim-label");
        detail.set_xalign(0.0);
        names.append(&detail);
        names.set_hexpand(true);
        row.append(&names);
        let clock = gtk::CheckButton::with_label("Clock");
        clock.set_active(o.clock);
        clock.set_tooltip_text(Some("Send MIDI clock (24 ppqn, start/stop, song position)"));
        clock.set_sensitive(o.enabled);
        {
            let weak = Rc::downgrade(app);
            let key = o.key.clone();
            clock.connect_toggled(move |b| {
                if let Some(a) = weak.upgrade() {
                    a.session
                        .borrow_mut()
                        .set_midi_clock_output(&key, b.is_active());
                    save_disabled(&a);
                }
            });
        }
        row.append(&clock);
        let mtc = gtk::CheckButton::with_label("MTC");
        mtc.set_active(o.mtc);
        mtc.set_tooltip_text(Some(
            "Send MIDI time code (quarter frames while playing, full frames on start and locate; rate and start time under Sync)",
        ));
        mtc.set_sensitive(o.enabled);
        {
            let weak = Rc::downgrade(app);
            let key = o.key.clone();
            mtc.connect_toggled(move |b| {
                if let Some(a) = weak.upgrade() {
                    a.session
                        .borrow_mut()
                        .set_midi_mtc_output(&key, b.is_active());
                    save_disabled(&a);
                }
            });
        }
        row.append(&mtc);
        let syx = gtk::Button::with_label("Send .syx…");
        syx.add_css_class("flat");
        syx.set_sensitive(o.enabled && o.connected);
        syx.set_tooltip_text(Some("Send the SysEx messages of a file to this device"));
        {
            let weak = Rc::downgrade(app);
            let key = o.key.clone();
            syx.connect_clicked(move |_| {
                if let Some(a) = weak.upgrade() {
                    crate::dialogs::send_sysex_file(&a, key.clone());
                }
            });
        }
        row.append(&syx);
        let sw = gtk::Switch::new();
        sw.set_active(o.enabled);
        sw.set_valign(gtk::Align::Center);
        sw.set_tooltip_text(Some("Use this output"));
        {
            let weak = Rc::downgrade(app);
            let key = o.key.clone();
            sw.connect_state_set(move |_, on| {
                if let Some(a) = weak.upgrade() {
                    a.session.borrow_mut().set_midi_output_enabled(&key, on);
                    save_disabled(&a);
                }
                glib::Propagation::Proceed
            });
        }
        row.append(&sw);
        list.append(&row);
    }
}

fn mappings_rows(app: &Rc<AppState>, list: &gtk::ListBox) {
    while let Some(c) = list.first_child() {
        list.remove(&c);
    }
    let s = app.session.borrow();
    let maps = s.project().midi_mappings.clone();
    if maps.is_empty() {
        let l = gtk::Label::new(Some(
            "No controller mappings. Right-click a fader, knob, mute button, send, automation lane or plugin parameter and choose MIDI Learn, then move a control on your device.",
        ));
        l.set_wrap(true);
        l.set_natural_wrap_mode(gtk::NaturalWrapMode::Word);
        l.set_xalign(0.0);
        l.add_css_class("dim-label");
        l.set_margin_top(6);
        l.set_margin_bottom(6);
        list.append(&l);
        return;
    }
    for m in maps {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        row.add_css_class("midi-port");
        let src = gtk::Label::new(Some(&m.source.label()));
        src.set_xalign(0.0);
        src.set_width_chars(26);
        src.set_ellipsize(gtk::pango::EllipsizeMode::End);
        let arrow = gtk::Label::new(Some("→"));
        arrow.add_css_class("dim-label");
        let dst = gtk::Label::new(Some(&s.mapping_target_label(&m.target)));
        dst.set_xalign(0.0);
        dst.set_hexpand(true);
        dst.set_ellipsize(gtk::pango::EllipsizeMode::End);
        let remove = gtk::Button::from_icon_name("user-trash-symbolic");
        remove.add_css_class("flat");
        remove.set_tooltip_text(Some("Remove this mapping"));
        let weak = Rc::downgrade(app);
        let id = m.id;
        remove.connect_clicked(move |_| {
            if let Some(a) = weak.upgrade() {
                a.dispatch(Action::Edit(Command::RemoveMidiMapping { mapping: id }));
            }
        });
        row.append(&src);
        row.append(&arrow);
        row.append(&dst);
        row.append(&remove);
        list.append(&row);
    }
}

struct SyncWidgets {
    grid: gtk::Grid,
    status: gtk::Label,
}

/// One line about the external timing source.
pub fn sync_status_text(st: &faderframe_session::SyncStatus) -> String {
    use faderframe_session::SyncSource;
    match st.source {
        SyncSource::Internal => "FaderFrame is the timing master".into(),
        _ if !st.receiving => "Waiting for the master…".into(),
        source => {
            let what = match source {
                SyncSource::MidiClock => st
                    .tempo
                    .map_or("MIDI clock".into(), |t| format!("MIDI clock at {t:.1} BPM")),
                _ => st.timecode.map_or("MTC".into(), |(tc, rate)| {
                    format!("MTC {tc} ({})", rate.label())
                }),
            };
            let tempo = if st.tempo_differs {
                " · tempo differs from the project's (followed at the next start or stop)"
            } else {
                ""
            };
            let speed = if (st.speed - 1.0).abs() > 1e-6 {
                format!(" · speed {:+.3} %", (st.speed - 1.0) * 100.0)
            } else {
                String::new()
            };
            format!(
                "{what} · {} · off by {:.1} ms{speed} · {} re-lock{}{tempo}",
                if st.running { "following" } else { "stopped" },
                st.error_ms,
                st.relocks,
                if st.relocks == 1 { "" } else { "s" }
            )
        }
    }
}

/// Source, input and MTC start; changes apply at once and are saved.
/// Voice to MIDI: how the voice ports hear.
fn voice_section(app: &Rc<AppState>) -> gtk::Grid {
    use faderframe_session::voice::{VoiceGlide, VoiceSettings, VoiceSpeed};
    let grid = gtk::Grid::new();
    grid.set_row_spacing(8);
    grid.set_column_spacing(12);
    let label = |t: &str| {
        let l = gtk::Label::new(Some(t));
        l.set_xalign(1.0);
        l.add_css_class("dim-label");
        l
    };
    let now = app.session.borrow().voice_settings();
    let save = |app: &Rc<AppState>, s: VoiceSettings| {
        app.session.borrow_mut().set_voice_settings(s);
        let mut p = crate::prefs::Preferences::load();
        p.voice = s;
        if let Err(e) = p.save() {
            tracing::warn!("cannot save preferences: {e}");
        }
    };
    let threshold = gtk::SpinButton::with_range(-80.0, -10.0, 1.0);
    threshold.set_value(f64::from(now.threshold_db));
    threshold.set_tooltip_text(Some(
        "Quieter input is silence (dBFS): lower it for a soft voice, raise it in a noisy room",
    ));
    {
        let weak = Rc::downgrade(app);
        threshold.connect_value_changed(move |b| {
            if let Some(app) = weak.upgrade() {
                let mut s = app.session.borrow().voice_settings();
                s.threshold_db = b.value() as f32;
                save(&app, s);
            }
        });
    }
    let names: Vec<&str> = VoiceGlide::ALL.iter().map(|g| g.label()).collect();
    let glide = gtk::DropDown::from_strings(&names);
    glide.set_selected(
        VoiceGlide::ALL
            .iter()
            .position(|g| *g == now.glide)
            .unwrap_or(0) as u32,
    );
    {
        let weak = Rc::downgrade(app);
        glide.connect_selected_notify(move |d| {
            if let Some(app) = weak.upgrade() {
                let mut s = app.session.borrow().voice_settings();
                s.glide = VoiceGlide::ALL
                    .get(d.selected() as usize)
                    .copied()
                    .unwrap_or_default();
                save(&app, s);
            }
        });
    }
    let names: Vec<&str> = VoiceSpeed::ALL.iter().map(|g| g.label()).collect();
    let speed = gtk::DropDown::from_strings(&names);
    speed.set_tooltip_text(Some(
        "How soon a sung note sounds: Fast a couple of its periods after it begins (a few milliseconds; for playing), Stable a little later and surer (for recording clean takes)",
    ));
    speed.set_selected(
        VoiceSpeed::ALL
            .iter()
            .position(|g| *g == now.speed)
            .unwrap_or(1) as u32,
    );
    {
        let weak = Rc::downgrade(app);
        speed.connect_selected_notify(move |d| {
            if let Some(app) = weak.upgrade() {
                let mut s = app.session.borrow().voice_settings();
                s.speed = VoiceSpeed::ALL
                    .get(d.selected() as usize)
                    .copied()
                    .unwrap_or_default();
                save(&app, s);
            }
        });
    }
    let in_key = gtk::CheckButton::with_label("Snap notes to the project's key");
    in_key.set_active(now.in_key);
    {
        let weak = Rc::downgrade(app);
        in_key.connect_toggled(move |b| {
            if let Some(app) = weak.upgrade() {
                let mut s = app.session.borrow().voice_settings();
                s.in_key = b.is_active();
                save(&app, s);
            }
        });
    }
    let hint = gtk::Label::new(Some(
        "Sing (or play a monophonic instrument) into an audio input and choose “Voice · In n” as an instrument or MIDI track's MIDI input: the track plays and records the notes you sing. For the least delay, use a small audio buffer (Preferences → Audio).",
    ));
    hint.set_wrap(true);
    hint.set_natural_wrap_mode(gtk::NaturalWrapMode::Word);
    hint.set_xalign(0.0);
    hint.add_css_class("dim-label");
    let l = label("Sensitivity (dB)");
    threshold.update_relation(&[gtk::accessible::Relation::LabelledBy(&[l.upcast_ref()])]);
    grid.attach(&l, 0, 0, 1, 1);
    grid.attach(&threshold, 1, 0, 1, 1);
    let l = label("Pitch");
    glide.update_relation(&[gtk::accessible::Relation::LabelledBy(&[l.upcast_ref()])]);
    grid.attach(&l, 0, 1, 1, 1);
    grid.attach(&glide, 1, 1, 1, 1);
    let l = label("Response");
    speed.update_relation(&[gtk::accessible::Relation::LabelledBy(&[l.upcast_ref()])]);
    grid.attach(&l, 0, 2, 1, 1);
    grid.attach(&speed, 1, 2, 1, 1);
    grid.attach(&in_key, 1, 3, 1, 1);
    grid.attach(&hint, 0, 4, 2, 1);
    grid
}

fn sync_section(app: &Rc<AppState>) -> SyncWidgets {
    use faderframe_session::{SyncSource, Timecode};
    let grid = gtk::Grid::new();
    grid.set_row_spacing(8);
    grid.set_column_spacing(12);
    let label = |t: &str| {
        let l = gtk::Label::new(Some(t));
        l.set_xalign(1.0);
        l.add_css_class("dim-label");
        l
    };
    let current = app.session.borrow().sync_settings().clone();
    let names: Vec<&str> = SyncSource::ALL.iter().map(|s| s.label()).collect();
    let source = gtk::DropDown::from_strings(&names);
    source.set_selected(
        SyncSource::ALL
            .iter()
            .position(|s| *s == current.source)
            .unwrap_or(0) as u32,
    );
    let ports: Vec<(String, String)> = app
        .session
        .borrow()
        .midi_ports()
        .into_iter()
        .filter(|p| !p.is_virtual)
        .map(|p| (p.key, p.name))
        .collect();
    let mut port_names = vec!["Any input".to_string()];
    port_names.extend(ports.iter().map(|(_, n)| n.clone()));
    if let Some(k) = &current.port
        && !ports.iter().any(|(key, _)| key == k)
    {
        port_names.push(format!("{k} (absent)"));
    }
    let refs: Vec<&str> = port_names.iter().map(String::as_str).collect();
    let port = gtk::DropDown::from_strings(&refs);
    port.set_selected(match &current.port {
        None => 0,
        Some(k) => ports
            .iter()
            .position(|(key, _)| key == k)
            .map_or(port_names.len() - 1, |i| i + 1) as u32,
    });
    let offset = gtk::Entry::new();
    offset.set_text(&current.offset.to_string());
    offset.set_max_width_chars(12);
    offset.set_tooltip_text(Some(
        "Timecode at the project start (hh:mm:ss:ff), e.g. 01:00:00:00",
    ));
    let out_offset = gtk::Entry::new();
    out_offset.set_text(&current.mtc_out_offset.to_string());
    out_offset.set_max_width_chars(12);
    out_offset.set_tooltip_text(Some(
        "The timecode FaderFrame sends at the project start (hh:mm:ss:ff)",
    ));
    let status = gtk::Label::new(None);
    status.set_xalign(0.0);
    status.add_css_class("dim-label");
    grid.attach(&label("Follow"), 0, 0, 1, 1);
    grid.attach(&source, 1, 0, 1, 1);
    grid.attach(&label("From"), 0, 1, 1, 1);
    grid.attach(&port, 1, 1, 1, 1);
    grid.attach(&label("MTC at project start"), 0, 2, 1, 1);
    grid.attach(&offset, 1, 2, 1, 1);
    let rates: Vec<&str> = faderframe_session::MtcRate::ALL
        .iter()
        .map(|r| r.label())
        .collect();
    let out_rate = gtk::DropDown::from_strings(&rates);
    out_rate.set_selected(
        faderframe_session::MtcRate::ALL
            .iter()
            .position(|r| *r == current.mtc_out_rate)
            .unwrap_or(1) as u32,
    );
    out_rate.set_tooltip_text(Some(
        "The frame rate of the MIDI time code FaderFrame sends",
    ));
    grid.attach(&label("MTC sent at"), 0, 3, 1, 1);
    let out_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    out_row.append(&out_rate);
    out_row.append(&label("from"));
    out_row.append(&out_offset);
    grid.attach(&out_row, 1, 3, 1, 1);
    let varispeed = gtk::CheckButton::with_label("Follow by varispeed (no jumps)");
    varispeed.set_active(current.varispeed);
    varispeed.set_tooltip_text(Some(
        "Play a little faster or slower to stay with the master (within 1 %), instead of jumping back into step when the clocks drift apart",
    ));
    grid.attach(&varispeed, 1, 4, 1, 1);
    grid.attach(&status, 1, 5, 1, 1);

    let apply = {
        let weak = Rc::downgrade(app);
        let (source, port, offset, out_rate, vs, out_offset) = (
            source.clone(),
            port.clone(),
            offset.clone(),
            out_rate.clone(),
            varispeed.clone(),
            out_offset.clone(),
        );
        let ports = ports.clone();
        let absent = current.port.clone();
        move || {
            let Some(app) = weak.upgrade() else { return };
            let mut s = app.session.borrow().sync_settings().clone();
            s.source = SyncSource::ALL[source.selected() as usize % SyncSource::ALL.len()];
            let rates = faderframe_session::MtcRate::ALL;
            s.mtc_out_rate = rates[out_rate.selected() as usize % rates.len()];
            s.varispeed = vs.is_active();
            s.port = match port.selected() as usize {
                0 => None,
                i => ports.get(i - 1).map(|(k, _)| k.clone()).or(absent.clone()),
            };
            match Timecode::parse(&offset.text()) {
                Some(tc) => {
                    s.offset = tc;
                    offset.remove_css_class("error");
                }
                None => offset.add_css_class("error"),
            }
            match Timecode::parse(&out_offset.text()) {
                Some(tc) => {
                    s.mtc_out_offset = tc;
                    out_offset.remove_css_class("error");
                }
                None => out_offset.add_css_class("error"),
            }
            app.session.borrow_mut().set_sync_settings(s.clone());
            let mut p = Preferences::load();
            p.set_sync_settings(&s);
            if let Err(e) = p.save() {
                tracing::warn!("cannot save preferences: {e}");
            }
        }
    };
    let apply = Rc::new(apply);
    {
        let apply = Rc::clone(&apply);
        varispeed.connect_toggled(move |_| apply());
    }
    for d in [&source, &port, &out_rate] {
        let apply = Rc::clone(&apply);
        d.connect_selected_notify(move |_| apply());
    }
    for e in [&offset, &out_offset] {
        {
            let apply = Rc::clone(&apply);
            e.connect_activate(move |_| apply());
        }
        let apply = Rc::clone(&apply);
        let focus = gtk::EventControllerFocus::new();
        focus.connect_leave(move |_| apply());
        e.add_controller(focus);
    }
    SyncWidgets { grid, status }
}

pub fn page(app: &Rc<AppState>) -> gtk::Widget {
    let body = gtk::Box::new(gtk::Orientation::Vertical, 6);
    body.set_margin_start(18);
    body.set_margin_end(18);
    body.set_margin_top(12);
    body.set_margin_bottom(12);

    body.append(&heading("MIDI INPUTS"));
    let ports = Rc::new(Ports {
        list: gtk::ListBox::new(),
        shown: RefCell::new(Vec::new()),
        dots: RefCell::new(Vec::new()),
    });
    ports.list.set_selection_mode(gtk::SelectionMode::None);
    ports.list.add_css_class("boxed-list");
    body.append(&ports.list);
    let hint = gtk::Label::new(Some(
        "Instrument tracks play what you play while they are armed or selected (track menu: MIDI In, Play Live).",
    ));
    hint.set_wrap(true);
    hint.set_natural_wrap_mode(gtk::NaturalWrapMode::Word);
    hint.set_xalign(0.0);
    hint.add_css_class("dim-label");
    body.append(&hint);

    body.append(&heading("VOICE TO MIDI"));
    body.append(&voice_section(app));

    body.append(&heading("MIDI OUTPUTS"));
    let outputs = gtk::ListBox::new();
    outputs.set_selection_mode(gtk::SelectionMode::None);
    outputs.add_css_class("boxed-list");
    body.append(&outputs);
    let out_hint = gtk::Label::new(Some(
        "MIDI tracks play external instruments (track menu: MIDI Out). Clock sends tempo, start, stop and song position.",
    ));
    out_hint.set_wrap(true);
    out_hint.set_natural_wrap_mode(gtk::NaturalWrapMode::Word);
    out_hint.set_xalign(0.0);
    out_hint.add_css_class("dim-label");
    body.append(&out_hint);

    body.append(&heading("SYNC"));
    let sync = sync_section(app);
    body.append(&sync.grid);

    body.append(&heading("CONTROL SURFACES"));
    body.append(&crate::surface_prefs::section(app));

    body.append(&heading("CONTROLLER MAPPINGS"));
    let maps = gtk::ListBox::new();
    maps.set_selection_mode(gtk::SelectionMode::None);
    maps.add_css_class("boxed-list");
    body.append(&maps);

    body.append(&heading("TRANSPORT"));
    // (A grid: FlowBox mis-measures inside scrolled pages.)
    let transport = gtk::Grid::new();
    transport.set_row_spacing(6);
    transport.set_column_spacing(6);
    // Each control's learn button, and beside it (while it is mapped) the
    // mapping's removal.
    let mut unlearn: Vec<(TransportControl, gtk::Button)> = Vec::new();
    for (i, control) in TransportControl::ALL.into_iter().enumerate() {
        let b = gtk::Button::with_label(&format!("Learn {}", control.label()));
        b.set_tooltip_text(Some("Then press a pad or button on your device"));
        let weak = Rc::downgrade(app);
        b.connect_clicked(move |_| {
            if let Some(a) = weak.upgrade() {
                a.dispatch(Action::MidiLearn(MappingTarget::Transport { control }));
            }
        });
        let remove = gtk::Button::from_icon_name("user-trash-symbolic");
        remove.add_css_class("flat");
        remove.set_visible(false);
        let weak = Rc::downgrade(app);
        remove.connect_clicked(move |_| {
            let Some(a) = weak.upgrade() else { return };
            let ids: Vec<_> = a
                .session
                .borrow()
                .midi_mappings_for(MappingTarget::Transport { control })
                .into_iter()
                .map(|m| m.id)
                .collect();
            for mapping in ids {
                a.dispatch(Action::Edit(
                    faderframe_project::Command::RemoveMidiMapping { mapping },
                ));
            }
        });
        let cell = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        b.set_hexpand(true);
        cell.append(&b);
        cell.append(&remove);
        transport.attach(&cell, (i % 3) as i32, (i / 3) as i32, 1, 1);
        unlearn.push((control, remove));
    }
    body.append(&transport);

    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&body)
        .build();

    // Outputs: rebuilt when the set or its state changes.
    let outputs_seen: RefCell<Option<Vec<faderframe_session::MidiOutputStatus>>> =
        RefCell::new(None);
    // Live state: ports (hotplug, activity) and mappings.
    let weak = Rc::downgrade(app);
    let maps_seen: RefCell<Option<Vec<faderframe_core::MidiMappingId>>> = RefCell::new(None);
    let alive = scroller.downgrade();
    let refresh = move || {
        let (Some(a), Some(_)) = (weak.upgrade(), alive.upgrade()) else {
            return glib::ControlFlow::Break;
        };
        let current = a.session.borrow().midi_ports();
        let shape: Vec<(String, bool, bool)> = current
            .iter()
            .map(|p| (p.key.clone(), p.enabled, p.connected))
            .collect();
        if *ports.shown.borrow() != shape {
            ports.rebuild(&a, &current);
        }
        for (key, dot) in ports.dots.borrow().iter() {
            let on = current.iter().any(|p| &p.key == key && p.active);
            if on != dot.has_css_class("active") {
                if on {
                    dot.add_css_class("active");
                } else {
                    dot.remove_css_class("active");
                }
            }
        }
        let outs: Vec<_> = a
            .session
            .borrow()
            .midi_outputs()
            .into_iter()
            .filter(|o| !o.is_virtual)
            .collect();
        for (control, remove) in &unlearn {
            let sources: Vec<String> = a
                .session
                .borrow()
                .midi_mappings_for(MappingTarget::Transport { control: *control })
                .iter()
                .map(|m| m.source.label())
                .collect();
            remove.set_visible(!sources.is_empty());
            let tip = format!("Remove MIDI Mapping ({})", sources.join(", "));
            if remove.tooltip_text().as_deref() != Some(tip.as_str()) {
                remove.set_tooltip_text(Some(&tip));
            }
        }
        if outputs_seen.borrow().as_ref() != Some(&outs) {
            *outputs_seen.borrow_mut() = Some(outs.clone());
            output_rows(&a, &outputs, &outs);
        }
        let ids: Vec<_> = a
            .session
            .borrow()
            .project()
            .midi_mappings
            .iter()
            .map(|m| m.id)
            .collect();
        if maps_seen.borrow().as_ref() != Some(&ids) {
            *maps_seen.borrow_mut() = Some(ids);
            mappings_rows(&a, &maps);
        }
        sync.status
            .set_text(&sync_status_text(&a.session.borrow().sync_status()));
        glib::ControlFlow::Continue
    };
    refresh();
    glib::timeout_add_local(Duration::from_millis(120), refresh);
    scroller.upcast()
}
