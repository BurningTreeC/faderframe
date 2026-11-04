//! Preferences → Network Audio: AES67 streams (what FaderFrame sends and
//! receives), the network interface, the PTP clock and its state, and
//! whether the streams are the audio system or join the device chosen on
//! the Audio page. Changes are kept on the page until Apply, which saves
//! them and reopens the audio stream when the streams are in use.
//!
//! Development aids (`FADERFRAME_STARTUP_ACTIONS`): `aes67:<…>` —
//! `send=<name>@<channels>[@<group>]`, `receive=<announced stream name>`,
//! `receive-sdp=<file>`, `drop=<name>`, `link-offset=<ms>`,
//! `ptime=<125us|250us|333us|1ms|4ms>`, `format=<l16|l24>`, `rate=<Hz>`,
//! `domain=<n>`, `profile=<media|default>`, `lead=<on|off>`,
//! `clock=<ptp|system>`, `alongside=<on|off>`, `announce=<on|off>`,
//! `interface=<address|auto>` (each saved at once), `apply` (reopens the
//! audio stream) and `status` (logs the clock, the streams heard and the
//! settings).

use crate::prefs::Preferences;
use crate::state::{AppState, BackendChoice};
use faderframe_audio_aes67::ptp::port::Profile;
use faderframe_audio_aes67::rtp::{Encoding, PacketTime};
use faderframe_audio_aes67::sdp::StreamDescription;
use faderframe_audio_aes67::settings::{LINK_OFFSETS_MS, SAMPLE_RATES};
use faderframe_audio_aes67::{Aes67Settings, ClockSource, ReceiveSettings, SendSettings};
use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::net::Ipv4Addr;
use std::rc::Rc;
use std::time::Duration;

/// Save `settings` and hand them to the backends made from now on.
fn keep(settings: &Aes67Settings) {
    faderframe_audio_aes67::configure(settings.clone());
    let mut p = Preferences::load();
    p.aes67 = settings.clone();
    if let Err(e) = p.save() {
        tracing::warn!("cannot save preferences: {e}");
    }
}

/// Whether the running audio uses the streams (as the audio system, or
/// beside its device).
fn streams_in_use(app: &Rc<AppState>, settings: &Aes67Settings, before: &Aes67Settings) -> bool {
    app.options.borrow().backend == BackendChoice::Aes67 || settings.alongside || before.alongside
}

/// Reopen the audio stream with the settings kept (refused while
/// recording, which reopening would end).
fn reopen(app: &Rc<AppState>) -> Result<(), String> {
    if app.session.borrow().transport().recording {
        return Err("Stop recording first: reopening the audio stream would end the take".into());
    }
    app.start_audio();
    Ok(())
}

fn heading(text: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    l.add_css_class("sidebar-heading");
    l.set_xalign(0.0);
    l
}

fn note(text: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    l.set_wrap(true);
    l.set_xalign(0.0);
    l.add_css_class("dim-label");
    l
}

/// The packet time's id in scripts.
fn ptime_id(p: PacketTime) -> &'static str {
    match p {
        PacketTime::Us125 => "125us",
        PacketTime::Us250 => "250us",
        PacketTime::Us333 => "333us",
        PacketTime::Ms1 => "1ms",
        PacketTime::Ms4 => "4ms",
    }
}

/// A list rebuilt from the settings being edited (its rows can ask for
/// it again: a row removed).
type Rebuild = Rc<dyn Fn()>;

/// A received stream's line in the list.
fn receive_label(d: &StreamDescription, heard: bool) -> String {
    format!(
        "{} — {}{}",
        d.name,
        d.summary(),
        if heard { "" } else { " (not announced now)" }
    )
}

/// The page (see the module docs).
pub fn page(app: &Rc<AppState>, row: impl Fn(&gtk::Grid, i32, &str, &gtk::Widget)) -> gtk::Widget {
    let g = gtk::Grid::new();
    g.set_row_spacing(10);
    g.set_column_spacing(14);
    g.add_css_class("audio-settings");
    let stored = Preferences::load().aes67;
    let work = Rc::new(RefCell::new(stored.clone()));
    let before = Rc::new(RefCell::new(stored.clone()));
    let mut y = 0;
    let mut next = || {
        y += 1;
        y - 1
    };

    g.attach(&heading("NETWORK"), 0, next(), 2, 1);
    let addresses = faderframe_audio_aes67::net::local_addresses();
    let routed = faderframe_audio_aes67::net::route_to(Ipv4Addr::new(239, 69, 0, 1));
    let mut names = vec![match routed {
        Some(a) => format!("Automatic ({a})"),
        None => "Automatic".to_string(),
    }];
    names.extend(addresses.iter().map(|a| a.to_string()));
    let iface = gtk::DropDown::from_strings(&names.iter().map(String::as_str).collect::<Vec<_>>());
    iface.set_selected(
        stored
            .interface
            .and_then(|i| addresses.iter().position(|a| *a == i))
            .map_or(0, |k| k + 1) as u32,
    );
    iface.set_tooltip_text(Some(
        "The network interface the streams and the PTP clock use (an AES67 network is often \
         a card of its own)",
    ));
    row(&g, next(), "Interface", iface.upcast_ref());

    let rates: Vec<String> = SAMPLE_RATES
        .iter()
        .map(|r| faderframe_audio::format_sample_rate(*r))
        .collect();
    let rate = gtk::DropDown::from_strings(&rates.iter().map(String::as_str).collect::<Vec<_>>());
    rate.set_selected(
        SAMPLE_RATES
            .iter()
            .position(|r| *r == stored.sample_rate)
            .unwrap_or(1) as u32,
    );
    row(&g, next(), "Sample rate", rate.upcast_ref());

    let offsets: Vec<String> = LINK_OFFSETS_MS.iter().map(|m| format!("{m} ms")).collect();
    let link = gtk::DropDown::from_strings(&offsets.iter().map(String::as_str).collect::<Vec<_>>());
    link.set_selected(
        LINK_OFFSETS_MS
            .iter()
            .position(|m| (*m - stored.link_offset_ms).abs() < 1e-6)
            .unwrap_or(2) as u32,
    );
    link.set_tooltip_text(Some(
        "How long after its timestamp a received frame is played: long enough for the \
         sender's packets, the network and its jitter (the Running list below shows how \
         late packets come). The same is assumed of the receivers of FaderFrame's streams",
    ));
    row(&g, next(), "Link offset", link.upcast_ref());

    let ptimes: Vec<&str> = PacketTime::ALL.iter().map(|p| p.label()).collect();
    let ptime = gtk::DropDown::from_strings(&ptimes);
    ptime.set_selected(
        PacketTime::ALL
            .iter()
            .position(|p| *p == stored.packet_time)
            .unwrap_or(3) as u32,
    );
    let format = gtk::DropDown::from_strings(&["24-bit (L24)", "16-bit (L16)"]);
    format.set_selected(u32::from(stored.encoding == Encoding::L16));
    let send_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    send_box.append(&ptime);
    send_box.append(&format);
    send_box.set_tooltip_text(Some(
        "Packet time and sample format of the streams FaderFrame sends (1 ms of 24-bit is \
         AES67's own; every AES67 receiver takes it)",
    ));
    row(&g, next(), "Sending", send_box.upcast_ref());

    g.attach(&heading("CLOCK (PTP)"), 0, next(), 2, 1);
    let source = gtk::DropDown::from_strings(&[
        "FaderFrame's own PTP clock",
        "The system clock, kept on PTP by ptp4l and phc2sys",
    ]);
    source.set_selected(u32::from(stored.ptp.clock == ClockSource::System));
    source.set_tooltip_text(Some(
        "FaderFrame's clock follows the network's grandmaster with software timestamps \
         (ports 319 and 320). Where the network card stamps packets itself, linuxptp keeps \
         the system clock closer: run ptp4l and phc2sys and choose the system clock",
    ));
    row(&g, next(), "Clock", source.upcast_ref());
    let domain = gtk::SpinButton::with_range(0.0, 127.0, 1.0);
    domain.set_value(f64::from(stored.ptp.domain));
    let profile = gtk::DropDown::from_strings(&[Profile::Media.label(), Profile::Default.label()]);
    profile.set_selected(u32::from(stored.ptp.profile == Profile::Default));
    let clock_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    clock_box.append(&gtk::Label::new(Some("Domain")));
    clock_box.append(&domain);
    clock_box.append(&gtk::Label::new(Some("Profile")));
    clock_box.append(&profile);
    row(&g, next(), "PTP", clock_box.upcast_ref());
    let lead = gtk::CheckButton::with_label(
        "Lead the network's clock when no grandmaster is there (any real grandmaster wins)",
    );
    lead.set_active(stored.ptp.lead);
    g.attach(&lead, 1, next(), 1, 1);
    let clock_status = note("");
    clock_status.set_selectable(true);
    g.attach(&clock_status, 1, next(), 1, 1);

    g.attach(&heading("SEND"), 0, next(), 2, 1);
    let sends = gtk::ListBox::new();
    sends.set_selection_mode(gtk::SelectionMode::None);
    sends.add_css_class("boxed-list");
    g.attach(&sends, 0, next(), 2, 1);
    let add_send = gtk::Button::with_label("Add a Stream to Send");
    add_send.set_halign(gtk::Align::Start);
    g.attach(&add_send, 1, next(), 1, 1);

    g.attach(&heading("RECEIVE"), 0, next(), 2, 1);
    let receives = gtk::ListBox::new();
    receives.set_selection_mode(gtk::SelectionMode::None);
    receives.add_css_class("boxed-list");
    g.attach(&receives, 0, next(), 2, 1);
    let heard_note = note("");
    g.attach(&heard_note, 1, next(), 1, 1);
    let sdp_view = gtk::TextView::new();
    sdp_view.set_monospace(true);
    sdp_view.set_size_request(-1, 120);
    let sdp_add = gtk::Button::with_label("Receive This Stream");
    let sdp_said = note("");
    let sdp_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
    sdp_box.append(&note(
        "Paste a stream's description (SDP) from a device's web page or a Dante \
         controller's AES67 flow:",
    ));
    sdp_box.append(&sdp_view);
    sdp_box.append(&sdp_add);
    sdp_box.append(&sdp_said);
    let expander = gtk::Expander::new(Some("A stream not announced (by its description)"));
    expander.set_child(Some(&sdp_box));
    g.attach(&expander, 1, next(), 1, 1);

    g.attach(&heading("USE"), 0, next(), 2, 1);
    let alongside = gtk::CheckButton::with_label(
        "Beside the audio system chosen on the Audio page (its device's clock drives; the \
         streams are resampled onto it) — else choose \"AES67 network audio\" there",
    );
    alongside.set_active(stored.alongside);
    g.attach(&alongside, 1, next(), 1, 1);
    let announce =
        gtk::CheckButton::with_label("Announce FaderFrame's streams (SAP) and list the network's");
    announce.set_active(stored.announce);
    g.attach(&announce, 1, next(), 1, 1);
    g.attach(
        &note(
            "Dante devices take part in AES67 mode (Dante Controller: enable AES67, then \
             their AES67 flows are announced here). NDI is not offered: its SDK's licence \
             does not allow FaderFrame's.",
        ),
        1,
        next(),
        1,
        1,
    );
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let apply = gtk::Button::with_label("Apply");
    apply.add_css_class("suggested-action");
    buttons.append(&apply);
    g.attach(&buttons, 1, next(), 1, 1);
    let said = note("");
    g.attach(&said, 1, next(), 1, 1);

    // The discovery the page lists from, while it is open.
    let discovery = Rc::new(RefCell::new(faderframe_audio_aes67::discovery_for(&stored)));
    let shown_changes = Rc::new(Cell::new(u64::MAX));

    let rebuild_sends: Rebuild = {
        let (sends, work) = (sends.clone(), Rc::clone(&work));
        let me: Rc<RefCell<Option<Rebuild>>> = Rc::new(RefCell::new(None));
        let f: Rebuild = {
            let me = Rc::clone(&me);
            Rc::new(move || {
                while let Some(c) = sends.first_child() {
                    sends.remove(&c);
                }
                let n = work.borrow().sends.len();
                let most = f64::from(work.borrow().max_send_channels().max(1));
                for k in 0..n {
                    let s = work.borrow().sends[k].clone();
                    let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                    line.set_margin_top(4);
                    line.set_margin_bottom(4);
                    line.set_margin_start(6);
                    line.set_margin_end(6);
                    let on = gtk::CheckButton::new();
                    on.set_active(s.enabled);
                    on.set_tooltip_text(Some("Send it"));
                    let name = gtk::Entry::new();
                    name.set_text(&s.name);
                    name.set_hexpand(true);
                    name.set_tooltip_text(Some("The stream's name on the network"));
                    let channels = gtk::SpinButton::with_range(1.0, most, 1.0);
                    channels.set_value(f64::from(s.channels));
                    channels.set_tooltip_text(Some(
                        "Channels (the engine's outputs in order, after the streams above)",
                    ));
                    let group = gtk::Entry::new();
                    group.set_placeholder_text(Some("Group: automatic"));
                    group.set_text(&s.address.map_or(String::new(), |a| a.to_string()));
                    group.set_width_chars(16);
                    group.set_tooltip_text(Some(
                        "The multicast group (239.x.y.z); empty: one of 239.69.0.0/16 from \
                         this computer's address",
                    ));
                    let remove = gtk::Button::from_icon_name("list-remove-symbolic");
                    remove.set_tooltip_text(Some("Remove this stream"));
                    for w in [
                        on.upcast_ref::<gtk::Widget>(),
                        name.upcast_ref(),
                        channels.upcast_ref(),
                        group.upcast_ref(),
                        remove.upcast_ref(),
                    ] {
                        line.append(w);
                    }
                    {
                        let work = Rc::clone(&work);
                        on.connect_toggled(move |b| {
                            if let Some(s) = work.borrow_mut().sends.get_mut(k) {
                                s.enabled = b.is_active();
                            }
                        });
                    }
                    {
                        let work = Rc::clone(&work);
                        name.connect_changed(move |e| {
                            if let Some(s) = work.borrow_mut().sends.get_mut(k) {
                                s.name = e.text().trim().to_string();
                            }
                        });
                    }
                    {
                        let work = Rc::clone(&work);
                        channels.connect_value_changed(move |c| {
                            if let Some(s) = work.borrow_mut().sends.get_mut(k) {
                                s.channels = c.value() as u16;
                            }
                        });
                    }
                    {
                        let work = Rc::clone(&work);
                        group.connect_changed(move |e| {
                            if let Some(s) = work.borrow_mut().sends.get_mut(k) {
                                s.address = e
                                    .text()
                                    .trim()
                                    .parse::<Ipv4Addr>()
                                    .ok()
                                    .filter(Ipv4Addr::is_multicast);
                            }
                        });
                    }
                    {
                        let (work, me) = (Rc::clone(&work), Rc::clone(&me));
                        remove.connect_clicked(move |_| {
                            if k < work.borrow().sends.len() {
                                work.borrow_mut().sends.remove(k);
                            }
                            let again = me.borrow().clone();
                            if let Some(f) = again {
                                glib::idle_add_local_once(move || f());
                            }
                        });
                    }
                    sends.append(&line);
                }
            })
        };
        *me.borrow_mut() = Some(Rc::clone(&f));
        f
    };
    rebuild_sends();
    {
        let (work, rebuild) = (Rc::clone(&work), Rc::clone(&rebuild_sends));
        add_send.connect_clicked(move |_| {
            {
                let mut w = work.borrow_mut();
                let n = w.sends.len() + 1;
                w.sends.push(SendSettings {
                    name: format!("FaderFrame {n}"),
                    ..SendSettings::default()
                });
            }
            rebuild();
        });
    }

    let rebuild_receives: Rebuild = {
        let (receives, work, discovery, heard_note) = (
            receives.clone(),
            Rc::clone(&work),
            Rc::clone(&discovery),
            heard_note.clone(),
        );
        Rc::new(move || {
            while let Some(c) = receives.first_child() {
                receives.remove(&c);
            }
            let heard = discovery
                .borrow()
                .as_ref()
                .map(|d| d.streams())
                .unwrap_or_default();
            // Heard ones first, then the ones kept that are not heard now.
            let mut lines: Vec<(String, String, String, bool)> = heard
                .iter()
                .map(|h| {
                    (
                        h.description.key(),
                        receive_label(&h.description, true),
                        h.sdp.clone(),
                        true,
                    )
                })
                .collect();
            for r in &work.borrow().receives {
                if !lines.iter().any(|l| l.0 == r.key) {
                    let label = StreamDescription::parse(&r.sdp).map_or_else(
                        |e| format!("{} — {e}", r.name),
                        |d| receive_label(&d, false),
                    );
                    lines.push((r.key.clone(), label, r.sdp.clone(), false));
                }
            }
            heard_note.set_text(&match discovery.borrow().as_ref() {
                None => "Announcements are off: streams are added by their description.".into(),
                Some(d) => match d.error() {
                    Some(e) => format!("Cannot listen for announcements: {e}"),
                    None if heard.is_empty() => {
                        "No streams announced yet (senders announce every 30 s).".into()
                    }
                    None => format!(
                        "{} stream{} announced on the network. Ticked ones are the engine's \
                         inputs, in this order.",
                        heard.len(),
                        if heard.len() == 1 { "" } else { "s" }
                    ),
                },
            });
            for (key, label, sdp, _) in lines {
                let on = gtk::CheckButton::with_label(&label);
                on.set_margin_top(4);
                on.set_margin_bottom(4);
                on.set_margin_start(6);
                on.set_active(
                    work.borrow()
                        .receives
                        .iter()
                        .any(|r| r.key == key && r.enabled),
                );
                let work = Rc::clone(&work);
                on.connect_toggled(move |b| {
                    let mut w = work.borrow_mut();
                    match w.receives.iter_mut().find(|r| r.key == key) {
                        Some(r) => {
                            r.enabled = b.is_active();
                            r.sdp = sdp.clone();
                        }
                        None if b.is_active() => {
                            if let Ok(d) = StreamDescription::parse(&sdp) {
                                let mut r = ReceiveSettings::from_description(&d, &sdp);
                                r.key = key.clone();
                                w.receives.push(r);
                            }
                        }
                        None => {}
                    }
                });
                receives.append(&on);
            }
        })
    };
    rebuild_receives();
    {
        let (work, rebuild, said) = (
            Rc::clone(&work),
            Rc::clone(&rebuild_receives),
            sdp_said.clone(),
        );
        sdp_add.connect_clicked(move |_| {
            let buf = sdp_view.buffer();
            let text = buf
                .text(&buf.start_iter(), &buf.end_iter(), false)
                .to_string();
            match StreamDescription::parse(&text) {
                Ok(d) => {
                    let mut w = work.borrow_mut();
                    w.receives.retain(|r| r.key != d.key());
                    w.receives
                        .push(ReceiveSettings::from_description(&d, &text));
                    said.set_text(&format!("Added {} — {}", d.name, d.summary()));
                    buf.set_text("");
                    drop(w);
                    rebuild();
                }
                Err(e) => said.set_text(&format!("Not a stream description FaderFrame reads: {e}")),
            }
        });
    }

    // The settings widgets → the working copy.
    {
        let (work, addresses) = (Rc::clone(&work), addresses.clone());
        iface.connect_selected_notify(move |d| {
            let k = d.selected() as usize;
            work.borrow_mut().interface = k.checked_sub(1).and_then(|i| addresses.get(i).copied());
        });
    }
    {
        let work = Rc::clone(&work);
        rate.connect_selected_notify(move |d| {
            if let Some(r) = SAMPLE_RATES.get(d.selected() as usize) {
                work.borrow_mut().sample_rate = *r;
            }
        });
    }
    {
        let work = Rc::clone(&work);
        link.connect_selected_notify(move |d| {
            if let Some(m) = LINK_OFFSETS_MS.get(d.selected() as usize) {
                work.borrow_mut().link_offset_ms = *m;
            }
        });
    }
    {
        let work = Rc::clone(&work);
        ptime.connect_selected_notify(move |d| {
            if let Some(p) = PacketTime::ALL.get(d.selected() as usize) {
                work.borrow_mut().packet_time = *p;
            }
        });
    }
    {
        let work = Rc::clone(&work);
        format.connect_selected_notify(move |d| {
            work.borrow_mut().encoding = if d.selected() == 1 {
                Encoding::L16
            } else {
                Encoding::L24
            };
        });
    }
    {
        let work = Rc::clone(&work);
        source.connect_selected_notify(move |d| {
            work.borrow_mut().ptp.clock = if d.selected() == 1 {
                ClockSource::System
            } else {
                ClockSource::Ptp
            };
        });
    }
    {
        let work = Rc::clone(&work);
        domain.connect_value_changed(move |s| {
            work.borrow_mut().ptp.domain = s.value() as u8;
        });
    }
    {
        let work = Rc::clone(&work);
        profile.connect_selected_notify(move |d| {
            work.borrow_mut().ptp.profile = if d.selected() == 1 {
                Profile::Default
            } else {
                Profile::Media
            };
        });
    }
    {
        let work = Rc::clone(&work);
        lead.connect_toggled(move |b| work.borrow_mut().ptp.lead = b.is_active());
    }
    {
        let work = Rc::clone(&work);
        alongside.connect_toggled(move |b| work.borrow_mut().alongside = b.is_active());
    }
    {
        let work = Rc::clone(&work);
        announce.connect_toggled(move |b| work.borrow_mut().announce = b.is_active());
    }
    {
        let (app, work, before, said, discovery, rebuild) = (
            Rc::clone(app),
            Rc::clone(&work),
            Rc::clone(&before),
            said.clone(),
            Rc::clone(&discovery),
            Rc::clone(&rebuild_receives),
        );
        apply.connect_clicked(move |_| {
            let now = work.borrow().clone();
            let was = before.replace(now.clone());
            keep(&now);
            *discovery.borrow_mut() = faderframe_audio_aes67::discovery_for(&now);
            rebuild();
            if !streams_in_use(&app, &now, &was) {
                said.set_text(
                    "Saved. The streams open when \"AES67 network audio\" is the audio system \
                     (Audio page) or the streams join it (above).",
                );
                return;
            }
            said.set_text(&match reopen(&app) {
                Ok(()) => "Saved; the audio stream was reopened with them.".to_string(),
                Err(e) => e,
            });
        });
    }

    // The clock and the network, while the page is shown.
    let weak = clock_status.downgrade();
    glib::timeout_add_local(Duration::from_millis(500), move || {
        let Some(label) = weak.upgrade() else {
            return glib::ControlFlow::Break;
        };
        label.set_text(&match faderframe_audio_aes67::ptp_status() {
            Some(s) => s.summary(),
            None => "The PTP clock runs while AES67 streams are open.".into(),
        });
        let changes = discovery.borrow().as_ref().map_or(0, |d| d.changes());
        if shown_changes.replace(changes) != changes {
            rebuild_receives();
        }
        glib::ControlFlow::Continue
    });

    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&g)
        .build()
        .upcast()
}

/// Change the stored settings by a script's `what=value` and say how.
fn script_change(settings: &mut Aes67Settings, what: &str, value: &str) -> Result<String, String> {
    let on = |v: &str| match v {
        "on" | "true" | "1" => Ok(true),
        "off" | "false" | "0" => Ok(false),
        _ => Err(format!("'{v}' is not on or off")),
    };
    match what {
        "send" => {
            // <name>@<channels>[@<group>]
            let mut parts = value.split('@');
            let name = parts.next().unwrap_or("").trim().to_string();
            let channels: u16 = parts
                .next()
                .and_then(|c| c.trim().parse().ok())
                .ok_or("send=<name>@<channels>[@<group>]")?;
            let address = match parts.next() {
                Some(a) => Some(
                    a.trim()
                        .parse::<Ipv4Addr>()
                        .ok()
                        .filter(Ipv4Addr::is_multicast)
                        .ok_or_else(|| format!("'{a}' is not a multicast group"))?,
                ),
                None => None,
            };
            let s = SendSettings {
                name: name.clone(),
                channels: channels.clamp(1, settings.max_send_channels().max(1)),
                address,
                ..SendSettings::default()
            };
            match settings.sends.iter_mut().find(|x| x.name == name) {
                Some(x) => *x = s,
                None => settings.sends.push(s),
            }
            Ok(format!("sends {name} ({channels} ch)"))
        }
        "receive" => {
            let heard = faderframe_audio_aes67::discovered();
            let h = heard
                .iter()
                .find(|h| h.description.name == value.trim())
                .ok_or_else(|| format!("no stream '{value}' announced (yet)"))?;
            settings.receives.retain(|r| r.key != h.description.key());
            settings
                .receives
                .push(ReceiveSettings::from_description(&h.description, &h.sdp));
            Ok(format!("receives {}", h.description.summary()))
        }
        "receive-sdp" => {
            let text = std::fs::read_to_string(value.trim()).map_err(|e| e.to_string())?;
            let d = StreamDescription::parse(&text).map_err(|e| e.to_string())?;
            settings.receives.retain(|r| r.key != d.key());
            settings
                .receives
                .push(ReceiveSettings::from_description(&d, &text));
            Ok(format!("receives {} — {}", d.name, d.summary()))
        }
        "drop" => {
            let before = settings.sends.len() + settings.receives.len();
            settings.sends.retain(|s| s.name != value.trim());
            settings.receives.retain(|r| r.name != value.trim());
            if settings.sends.len() + settings.receives.len() == before {
                return Err(format!("no stream '{value}'"));
            }
            Ok(format!("dropped {value}"))
        }
        "link-offset" => {
            let ms: f64 = value.trim().parse().map_err(|_| "link-offset=<ms>")?;
            settings.link_offset_ms =
                ms.clamp(0.0, faderframe_audio_aes67::settings::MAX_LINK_OFFSET_MS);
            Ok(format!("link offset {} ms", settings.link_offset_ms))
        }
        "ptime" => {
            settings.packet_time = PacketTime::ALL
                .into_iter()
                .find(|p| ptime_id(*p) == value.trim())
                .ok_or("ptime=<125us|250us|333us|1ms|4ms>")?;
            Ok(format!("packet time {}", settings.packet_time.label()))
        }
        "format" => {
            settings.encoding = Encoding::parse(value).ok_or("format=<l16|l24>")?;
            Ok(format!("format {}", settings.encoding))
        }
        "rate" => {
            let r: u32 = value.trim().parse().map_err(|_| "rate=<Hz>")?;
            if !SAMPLE_RATES.contains(&r) {
                return Err(format!("{r} Hz is not one of {SAMPLE_RATES:?}"));
            }
            settings.sample_rate = r;
            Ok(format!("rate {r} Hz"))
        }
        "domain" => {
            settings.ptp.domain = value.trim().parse().map_err(|_| "domain=<0…127>")?;
            Ok(format!("PTP domain {}", settings.ptp.domain))
        }
        "profile" => {
            settings.ptp.profile = match value.trim() {
                "media" | "aes67" => Profile::Media,
                "default" => Profile::Default,
                _ => return Err("profile=<media|default>".into()),
            };
            Ok(format!("PTP profile {}", settings.ptp.profile.label()))
        }
        "lead" => {
            settings.ptp.lead = on(value)?;
            Ok(format!("may lead: {}", settings.ptp.lead))
        }
        "clock" => {
            settings.ptp.clock = match value.trim() {
                "ptp" => ClockSource::Ptp,
                "system" => ClockSource::System,
                _ => return Err("clock=<ptp|system>".into()),
            };
            Ok(format!("clock {:?}", settings.ptp.clock))
        }
        "alongside" => {
            settings.alongside = on(value)?;
            Ok(format!("alongside: {}", settings.alongside))
        }
        "announce" => {
            settings.announce = on(value)?;
            Ok(format!("announce: {}", settings.announce))
        }
        "interface" => {
            settings.interface = match value.trim() {
                "auto" | "" => None,
                a => Some(
                    a.parse()
                        .map_err(|_| format!("'{a}' is not an IPv4 address"))?,
                ),
            };
            Ok(format!("interface {:?}", settings.interface))
        }
        _ => Err(format!("unknown '{what}'")),
    }
}

/// `aes67:<…>` (see the module docs).
fn dev_aes67(a: &Rc<AppState>, arg: &str) {
    let arg = arg.trim();
    match arg {
        "apply" => {
            let settings = Preferences::load().aes67;
            faderframe_audio_aes67::configure(settings);
            match reopen(a) {
                Ok(()) => tracing::info!("aes67: the audio stream was reopened"),
                Err(e) => tracing::warn!("aes67: {e}"),
            }
        }
        "status" => {
            match faderframe_audio_aes67::ptp_status() {
                Some(s) => tracing::info!("aes67: {}", s.summary().replace('\n', " · ")),
                None => tracing::info!("aes67: the PTP clock is not running"),
            }
            for h in faderframe_audio_aes67::discovered() {
                tracing::info!(
                    "aes67: heard {} — {}",
                    h.description.name,
                    h.description.summary()
                );
            }
            let s = faderframe_audio_aes67::settings();
            tracing::info!(
                "aes67: {} send(s), {} receive(s), {} Hz, link offset {} ms, alongside {}",
                s.active_sends().count(),
                s.active_receives().count(),
                s.sample_rate,
                s.link_offset_ms,
                s.alongside
            );
        }
        _ => {
            let (what, value) = arg.split_once('=').unwrap_or((arg, ""));
            let mut settings = Preferences::load().aes67;
            match script_change(&mut settings, what.trim(), value) {
                Ok(said) => {
                    keep(&settings);
                    tracing::info!("aes67: {said}");
                }
                Err(e) => tracing::warn!("aes67: {e}"),
            }
        }
    }
}

/// The development aids.
pub(crate) fn install(app: &Rc<AppState>) {
    let weak = Rc::downgrade(app);
    let entry = gio::ActionEntry::builder("aes67")
        .parameter_type(Some(&String::static_variant_type()))
        .activate(move |_, _, param| {
            if let (Some(a), Some(arg)) = (weak.upgrade(), param.and_then(|p| p.get::<String>())) {
                dev_aes67(&a, &arg);
            }
        })
        .build();
    app.app.add_action_entries([entry]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripts_change_the_settings() {
        let mut s = Aes67Settings::default();
        script_change(&mut s, "send", "Monitors@8@239.69.3.4").unwrap();
        assert_eq!(s.sends.len(), 2);
        assert_eq!(s.sends[1].channels, 8);
        assert_eq!(s.sends[1].address, Some(Ipv4Addr::new(239, 69, 3, 4)));
        // The same name again replaces it.
        script_change(&mut s, "send", "Monitors@2").unwrap();
        assert_eq!(s.sends.len(), 2);
        assert_eq!(s.sends[1].address, None);
        assert!(script_change(&mut s, "send", "Bad@2@10.0.0.1").is_err());
        script_change(&mut s, "ptime", "125us").unwrap();
        assert_eq!(s.packet_time, PacketTime::Us125);
        script_change(&mut s, "format", "l16").unwrap();
        assert_eq!(s.encoding, Encoding::L16);
        script_change(&mut s, "link-offset", "5").unwrap();
        assert_eq!(s.link_offset_ms, 5.0);
        script_change(&mut s, "lead", "off").unwrap();
        assert!(!s.ptp.lead);
        script_change(&mut s, "clock", "system").unwrap();
        assert_eq!(s.ptp.clock, ClockSource::System);
        script_change(&mut s, "alongside", "on").unwrap();
        assert!(s.alongside);
        script_change(&mut s, "drop", "Monitors").unwrap();
        assert_eq!(s.sends.len(), 1);
        assert!(script_change(&mut s, "drop", "Nobody").is_err());
        assert!(script_change(&mut s, "rate", "22050").is_err());
        assert!(script_change(&mut s, "nonsense", "1").is_err());
    }

    #[test]
    fn stream_lines_say_what_and_where() {
        let d = StreamDescription::parse(
            "v=0\no=- 5 1 IN IP4 10.0.0.2\ns=Stage\nc=IN IP4 239.69.1.2/32\nt=0 0\n\
             m=audio 5004 RTP/AVP 96\na=rtpmap:96 L24/48000/8\na=ptime:1\n",
        )
        .unwrap();
        assert_eq!(
            receive_label(&d, false),
            "Stage — 8 ch · L24 · 48 kHz · 1 ms · 239.69.1.2:5004 from 10.0.0.2 (not announced now)"
        );
    }
}
