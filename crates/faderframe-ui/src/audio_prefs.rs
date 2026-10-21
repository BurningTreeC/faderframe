//! Preferences → Audio: the audio interface (audio system, output and
//! input devices, sample rate, buffer size, channels and what the audio
//! system itself offers), what is running (asked for against actual,
//! latency, where each channel goes, a device that went away) and the
//! processing settings.
//!
//! Only what the selected audio system supports is shown
//! ([`faderframe_audio::Capabilities`]); its devices are listed on a
//! thread of its own. Apply passes what the running stream can take to it
//! (playback goes on) and reopens the device for the rest, saying which
//! before it is pressed.

use crate::prefs::Preferences;
use crate::state::{AppState, BackendChoice};
use faderframe_audio::{
    Capabilities, DeviceInfo, RateControl, STANDARD_BUFFER_SIZES, STANDARD_SAMPLE_RATES,
    format_sample_rate,
};
use faderframe_session::{Applied, AudioHealth, AudioPreferences, DeviceChoice};
use gtk::glib;
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::time::Duration;

/// Channel counts offered (besides Automatic).
const CHANNEL_COUNTS: [u16; 16] = [1, 2, 4, 6, 8, 10, 12, 16, 18, 20, 24, 26, 32, 48, 56, 64];

/// One audio system's devices, listed on a thread.
#[derive(Clone, Debug, Default)]
struct Scan {
    /// The backend listed (`None`: none of the choice's is available).
    backend: Option<&'static str>,
    name: &'static str,
    caps: Capabilities,
    devices: Vec<DeviceInfo>,
    error: Option<String>,
}

/// List the devices of the first available backend of `choice` (the
/// running one first, for Automatic).
fn scan(choice: BackendChoice, running: Option<&'static str>) -> Scan {
    let mut backends = choice.backends();
    if let Some(i) = running.and_then(|id| backends.iter().position(|b| b.id() == id)) {
        let b = backends.remove(i);
        backends.insert(0, b);
    }
    let mut out = Scan {
        error: Some("no audio system of this choice is available".into()),
        ..Scan::default()
    };
    for b in backends {
        if !b.is_available() {
            continue;
        }
        let (devices, error) = match b.enumerate_devices() {
            Ok(d) => (d, None),
            Err(e) => (Vec::new(), Some(e.to_string())),
        };
        return Scan {
            backend: Some(b.id()),
            name: b.display_name(),
            caps: b.capabilities(),
            devices,
            error,
        };
    }
    out.error = Some(match choice {
        BackendChoice::Auto => "no audio system is available".into(),
        c => format!("{} is not available", c.label()),
    });
    out
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

/// A labelled row; the label is returned (to hide the row with it).
fn row(
    grid: &gtk::Grid,
    y: i32,
    label: &str,
    widget: &(impl IsA<gtk::Widget> + IsA<gtk::Accessible>),
) -> gtk::Label {
    let l = gtk::Label::new(Some(label));
    l.set_xalign(1.0);
    l.add_css_class("dim-label");
    grid.attach(&l, 0, y, 1, 1);
    widget.set_hexpand(true);
    widget.update_relation(&[gtk::accessible::Relation::LabelledBy(&[l.upcast_ref()])]);
    grid.attach(widget, 1, y, 1, 1);
    l
}

fn set_items(dd: &gtk::DropDown, items: &[String], selected: usize) {
    let refs: Vec<&str> = items.iter().map(String::as_str).collect();
    dd.set_model(Some(&gtk::StringList::new(&refs)));
    dd.set_selected(selected.min(items.len().saturating_sub(1)) as u32);
}

fn ms(frames: u32, rate: u32) -> String {
    format!(
        "{:.2} ms",
        f64::from(frames) * 1000.0 / f64::from(rate.max(1))
    )
}

/// The controls and what they map to.
struct Page {
    app: Weak<AppState>,
    backend: gtk::DropDown,
    /// Output devices (main, also) and input devices (main, also).
    devices: [gtk::DropDown; 4],
    device_rows: [gtk::Label; 4],
    /// What each device entry stands for (`None`: the default / none).
    ids: RefCell<[Vec<Option<String>>; 4]>,
    /// The output device whose rates and buffer sizes are shown.
    shown_output: RefCell<Option<String>>,
    clock_note: gtk::Label,
    rate: gtk::DropDown,
    rates: RefCell<Vec<Option<u32>>>,
    server_rate: gtk::Label,
    buffer: gtk::DropDown,
    buffers: RefCell<Vec<Option<u32>>>,
    inputs: gtk::DropDown,
    outputs: gtk::DropDown,
    force: gtk::CheckButton,
    system_note: gtk::Label,
    threads: gtk::DropDown,
    apply: gtk::Button,
    plan: gtk::Label,
    scan: RefCell<Option<Scan>>,
    scanning: RefCell<Option<std::sync::mpsc::Receiver<Scan>>>,
    /// The stored settings the controls change (read when devices are
    /// listed and after Apply).
    base: RefCell<AudioPreferences>,
    /// Models are being rebuilt: selections are not the user's.
    building: Cell<bool>,
    // Status.
    running: gtk::Label,
    asked: gtk::Label,
    latency: gtk::Label,
    mapping: gtk::Label,
    health: gtk::Label,
}

impl Page {
    fn choice(&self) -> BackendChoice {
        let all = BackendChoice::available();
        all[self.backend.selected() as usize % all.len()]
    }

    /// Start listing the selected audio system's devices.
    fn rescan(self: &Rc<Self>) {
        let choice = self.choice();
        let running = self
            .app
            .upgrade()
            .and_then(|a| a.session.borrow().backend_name());
        for (dd, l) in self.devices.iter().zip(&self.device_rows) {
            dd.set_sensitive(false);
            l.set_visible(true);
            dd.set_visible(true);
        }
        let (tx, rx) = std::sync::mpsc::channel();
        *self.scanning.borrow_mut() = Some(rx);
        // The placeholder is not the user's choice of output device (its
        // handler would show the previous audio system's scan again).
        self.building.set(true);
        set_items(&self.devices[0], &["Looking for devices…".into()], 0);
        self.building.set(false);
        let spawned = std::thread::Builder::new()
            .name("audio-devices".into())
            .spawn(move || {
                let _ = tx.send(scan(choice, running));
            });
        if let Err(e) = spawned {
            tracing::warn!("cannot list audio devices: {e}");
            return;
        }
        let weak = Rc::downgrade(self);
        glib::timeout_add_local(Duration::from_millis(50), move || {
            let Some(page) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            let got = page.scanning.borrow().as_ref().map(|rx| rx.try_recv());
            match got {
                Some(Ok(scan)) => {
                    page.scanning.borrow_mut().take();
                    page.show_scan(scan, None);
                    glib::ControlFlow::Break
                }
                Some(Err(std::sync::mpsc::TryRecvError::Empty)) => glib::ControlFlow::Continue,
                _ => glib::ControlFlow::Break,
            }
        });
    }

    /// What is stored (and what the session runs with), as the base the
    /// controls change.
    fn stored(&self) -> AudioPreferences {
        self.base.borrow().clone()
    }

    /// Read the stored settings again.
    fn reload(&self) {
        if let Some(app) = self.app.upgrade() {
            let o = app.options.borrow();
            *self.base.borrow_mut() = Preferences::load().audio_preferences(&o);
        }
    }

    /// Fill the controls for a scanned audio system: as stored, or as
    /// `keep` has them.
    fn show_scan(self: &Rc<Self>, scan: Scan, keep: Option<Keep>) {
        self.building.set(true);
        self.reload();
        let stored = self.stored();
        let shown = keep.as_ref().map_or(&stored, |k| &k.settings);
        let choice = scan
            .backend
            .map(|id| stored.devices_for(id))
            .unwrap_or_default();
        let caps = scan.caps;
        let name = |d: &DeviceInfo, output: bool| {
            let n = if output {
                d.output_channels
            } else {
                d.input_channels
            };
            format!("{} — {n} {}", d.name, if output { "out" } else { "in" })
        };
        let default_out = scan
            .devices
            .iter()
            .find(|d| d.is_default && d.output_channels > 0)
            .or_else(|| scan.devices.iter().find(|d| d.output_channels > 0));
        let default_in = default_out.filter(|d| d.input_channels > 0).or_else(|| {
            scan.devices
                .iter()
                .find(|d| d.is_default && d.input_channels > 0)
        });
        let mut ids: [Vec<Option<String>>; 4] = Default::default();
        for (slot, dd) in self.devices.iter().enumerate() {
            let output = slot < 2;
            let first = slot % 2 == 0;
            let max = if output {
                caps.output_devices
            } else {
                caps.input_devices
            };
            let shown = max > slot % 2 && scan.backend.is_some();
            dd.set_visible(shown);
            self.device_rows[slot].set_visible(shown);
            dd.set_sensitive(true);
            let mut items = vec![if first {
                let d = if output { default_out } else { default_in };
                match d {
                    Some(d) => format!("Default ({})", d.name),
                    None => "Default".into(),
                }
            } else {
                "None".into()
            }];
            let mut list = vec![None];
            for d in &scan.devices {
                let has = if output {
                    d.output_channels > 0
                } else {
                    d.input_channels > 0
                };
                if has {
                    items.push(name(d, output));
                    list.push(Some(d.id.clone()));
                }
            }
            let chosen = if output {
                &choice.outputs
            } else {
                &choice.inputs
            };
            let wanted = match &keep {
                Some(k) => k.devices[slot].clone(),
                None => chosen.get(slot % 2).cloned(),
            };
            let mut selected = 0;
            if let Some(id) = wanted {
                match list.iter().position(|x| x.as_deref() == Some(id.as_str())) {
                    Some(i) => selected = i,
                    None => {
                        // Chosen, not connected now: kept and shown.
                        items.push(format!("{id} (not connected)"));
                        list.push(Some(id));
                        selected = list.len() - 1;
                    }
                }
            }
            set_items(dd, &items, selected);
            ids[slot] = list;
        }
        *self.ids.borrow_mut() = ids;
        *self.shown_output.borrow_mut() = self.selected_output();
        if caps.input_devices == 0 && scan.backend.is_some() {
            self.device_rows[2].set_visible(true);
            self.devices[2].set_visible(true);
            set_items(
                &self.devices[2],
                &["From the output device (the driver records too)".into()],
                0,
            );
            self.devices[2].set_sensitive(false);
        }
        // Sample rates: the device's where it says, else the standard ones.
        let device = self
            .selected_output()
            .and_then(|id| faderframe_audio::find_device(&scan.devices, &id).cloned())
            .or_else(|| default_out.cloned());
        let rates: Vec<u32> = match (&device, caps.rate) {
            (Some(d), RateControl::Device) if !d.sample_rates.is_empty() => d.sample_rates.clone(),
            _ => STANDARD_SAMPLE_RATES.to_vec(),
        };
        let mut rate_list: Vec<Option<u32>> = vec![None];
        rate_list.extend(rates.iter().map(|r| Some(*r)));
        if let Some(r) = shown.sample_rate
            && !rate_list.contains(&Some(r))
        {
            rate_list.push(Some(r));
        }
        let rate_items: Vec<String> = rate_list
            .iter()
            .map(|r| match r {
                None => "Device / server default".to_string(),
                Some(r) if rates.contains(r) => format_sample_rate(*r),
                Some(r) => format!("{} (not supported here)", format_sample_rate(*r)),
            })
            .collect();
        let at = rate_list
            .iter()
            .position(|r| *r == shown.sample_rate)
            .unwrap_or(0);
        set_items(&self.rate, &rate_items, at);
        *self.rates.borrow_mut() = rate_list;
        let server = caps.rate == RateControl::Server;
        self.rate.set_visible(!server);
        self.server_rate.set_visible(server);
        self.server_rate
            .set_text(&match device.as_ref().and_then(|d| d.current_sample_rate) {
                Some(r) => format!(
                    "{} — the server's own (set it in the server)",
                    format_sample_rate(r)
                ),
                None => "The server's own (set it in the server)".into(),
            });
        // Buffer sizes: within what the device takes, where it says (an
        // audio server's limits are told in the note instead).
        let range = (caps.rate == RateControl::Device)
            .then(|| device.as_ref().and_then(|d| d.buffer_sizes))
            .flatten();
        let mut buffer_list: Vec<Option<u32>> = vec![None];
        buffer_list.extend(
            STANDARD_BUFFER_SIZES
                .iter()
                .filter(|b| range.is_none_or(|(lo, hi)| (lo..=hi).contains(*b)))
                .map(|b| Some(*b)),
        );
        if let Some(b) = shown.buffer_size
            && !buffer_list.contains(&Some(b))
        {
            buffer_list.push(Some(b));
        }
        let buffer_items: Vec<String> = buffer_list
            .iter()
            .map(|b| match b {
                None => "Device / server default".to_string(),
                Some(b) => format!("{b} frames"),
            })
            .collect();
        let at = buffer_list
            .iter()
            .position(|b| *b == shown.buffer_size)
            .unwrap_or(0);
        set_items(&self.buffer, &buffer_items, at);
        *self.buffers.borrow_mut() = buffer_list;
        let count_at = |c: Option<u16>| {
            c.and_then(|c| CHANNEL_COUNTS.iter().position(|x| *x == c))
                .map_or(0, |i| i + 1)
        };
        self.inputs
            .set_selected(count_at(shown.input_channels) as u32);
        self.outputs
            .set_selected(count_at(shown.output_channels) as u32);
        self.force.set_visible(caps.force_format);
        self.force.set_active(shown.force_format);
        self.system_note
            .set_text(&system_note(&scan, device.as_ref()));
        *self.scan.borrow_mut() = Some(scan);
        self.building.set(false);
        self.update_clock_note();
        self.update_plan();
    }

    fn selected_id(&self, slot: usize) -> Option<String> {
        let i = self.devices[slot].selected() as usize;
        self.ids.borrow()[slot].get(i).cloned().flatten()
    }

    fn selected_output(&self) -> Option<String> {
        self.selected_id(0)
    }

    /// Settings as the controls show them.
    fn wanted(&self) -> (BackendChoice, AudioPreferences) {
        let mut prefs = self.stored();
        let pick = |dd: &gtk::DropDown, list: &[Option<u32>]| {
            list.get(dd.selected() as usize).copied().flatten()
        };
        // A server that owns the rate: the setting stays as it was (for
        // the other audio systems).
        if self.rate.is_visible() {
            prefs.sample_rate = pick(&self.rate, &self.rates.borrow());
        }
        prefs.buffer_size = pick(&self.buffer, &self.buffers.borrow());
        let count = |dd: &gtk::DropDown| {
            let i = dd.selected() as usize;
            (i > 0).then(|| CHANNEL_COUNTS[(i - 1).min(CHANNEL_COUNTS.len() - 1)])
        };
        prefs.input_channels = count(&self.inputs);
        prefs.output_channels = count(&self.outputs);
        prefs.threads = (self.threads.selected() > 0).then(|| self.threads.selected() as u16);
        if let Some(scan) = self.scan.borrow().as_ref() {
            prefs.force_format = scan.caps.force_format && self.force.is_active();
            if let Some(id) = scan.backend {
                let take = |slots: [usize; 2]| -> Vec<String> {
                    let mut v: Vec<String> = Vec::new();
                    for s in slots {
                        if self.devices[s].is_visible()
                            && self.devices[s].is_sensitive()
                            && let Some(id) = self.selected_id(s)
                            && !v.contains(&id)
                        {
                            v.push(id);
                        }
                    }
                    v
                };
                // The default and a second device: the default by name
                // (an empty list could only mean the default alone).
                let with_default = |mut list: Vec<String>, main: usize, output: bool| {
                    let has = |d: &&DeviceInfo| {
                        if output {
                            d.output_channels > 0
                        } else {
                            d.input_channels > 0
                        }
                    };
                    if self.selected_id(main).is_none()
                        && !list.is_empty()
                        && let Some(d) = scan
                            .devices
                            .iter()
                            .filter(has)
                            .find(|d| d.is_default)
                            .or_else(|| scan.devices.iter().find(has))
                        && !list.contains(&d.id)
                    {
                        list.insert(0, d.id.clone());
                    }
                    list
                };
                let outputs = with_default(take([0, 1]), 0, true);
                let inputs = with_default(take([2, 3]), 2, false);
                prefs
                    .devices
                    .insert(id.to_string(), DeviceChoice { outputs, inputs });
            }
        }
        (self.choice(), prefs)
    }

    /// What Apply would do, said before it is pressed.
    fn update_plan(&self) {
        if self.building.get() {
            return;
        }
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let (choice, prefs) = self.wanted();
        let now = app.options.borrow().backend;
        let (text, can) = if self.scanning.borrow().is_some() {
            ("Looking for devices…".to_string(), false)
        } else if choice != now {
            (
                format!(
                    "Apply switches to {}: the audio device reopens and playback stops (the \
                     playhead stays)",
                    choice.label()
                ),
                true,
            )
        } else {
            match app.session.borrow().plan_audio(&prefs) {
                Ok(Applied::Unchanged) => {
                    ("Nothing to apply: this is what runs".to_string(), false)
                }
                Ok(Applied::Live) => (
                    "Apply passes this to the running stream: playback goes on".to_string(),
                    true,
                ),
                Ok(Applied::Restarted) => (
                    "Apply reopens the audio device: playback stops for a moment (the playhead \
                     stays)"
                        .to_string(),
                    true,
                ),
                Err(e) => (e, false),
            }
        };
        self.plan.set_text(&text);
        self.apply.set_sensitive(can);
    }

    /// Devices of different clocks chosen together: what that means here.
    fn update_clock_note(&self) {
        let text = (|| {
            let scan = self.scan.borrow();
            let scan = scan.as_ref()?;
            let (_, prefs) = self.wanted();
            let choice = prefs.devices_for(scan.backend?);
            let clocks: Vec<Option<String>> = choice
                .outputs
                .iter()
                .chain(&choice.inputs)
                .filter_map(|id| faderframe_audio::find_device(&scan.devices, id))
                .map(|d| d.clock.clone())
                .collect();
            let mut distinct = clocks.clone();
            distinct.dedup();
            let several = clocks.len() > 1 && clocks.iter().any(|c| *c != clocks[0]);
            if !several {
                return None;
            }
            Some(if scan.caps.clocked_by_server {
                "These devices run on clocks of their own: the audio server keeps them in step \
                 (PipeWire resamples the ones that follow the graph's driver — a little latency \
                 and processing, no drift)."
                    .to_string()
            } else {
                "Input and output on different devices run on two clocks and nothing here \
                 resamples between them: the input drifts, with a dropout now and then (counted \
                 as xruns). Record through the output device itself, or make one aggregate device \
                 of them in the system (macOS: Audio MIDI Setup)."
                    .to_string()
            })
        })();
        self.clock_note.set_visible(text.is_some());
        self.clock_note.set_text(text.as_deref().unwrap_or(""));
    }

    /// The running state (twice a second).
    fn refresh(&self) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let s = app.session.borrow();
        let status = s.audio_status();
        let (info, st) = (s.stream_info(), s.stream_status());
        let set = |l: &gtk::Label, t: &str| {
            if l.text() != t {
                l.set_text(t);
            }
        };
        match (&info, &st) {
            (Some(info), Some(st)) => {
                set(
                    &self.running,
                    &format!(
                        "{} · {} · {} · {} frames ({}) · {} in / {} out",
                        info.backend.to_uppercase(),
                        info.device,
                        format_sample_rate(st.sample_rate),
                        st.buffer_size,
                        ms(st.buffer_size, st.sample_rate),
                        info.input_channels,
                        info.output_channels
                    ),
                );
                let mut asked = Vec::new();
                if let Some(r) = status.requested.sample_rate
                    && r != st.sample_rate
                {
                    asked.push(format!(
                        "{} asked for, running at {}",
                        format_sample_rate(r),
                        format_sample_rate(st.sample_rate)
                    ));
                }
                if let Some(b) = status.requested.buffer_size
                    && b != st.buffer_size
                {
                    asked.push(format!(
                        "{b} frames asked for, running at {}",
                        st.buffer_size
                    ));
                }
                let why = match status.capabilities.map(|c| c.rate) {
                    Some(RateControl::Requested) => {
                        " — the PipeWire graph keeps its own (other streams, or its \
                         clock.allowed-rates and quantum limits)"
                    }
                    Some(RateControl::Server) => " — the server owns them",
                    _ => " — the device's choice",
                };
                set(
                    &self.asked,
                    &if asked.is_empty() {
                        "Running as asked for".to_string()
                    } else {
                        format!("{}{why}", asked.join("; "))
                    },
                );
                let rate = st.sample_rate;
                let (input, output) = (info.input_latency, info.output_latency);
                let graph = s.engine().graph_latency();
                set(
                    &self.latency,
                    &format!(
                        "Input {input} fr ({}) · output {output} fr ({}) beyond the buffer, as the \
                         audio system reports them\nHeard {} after processing (buffer + output) · \
                         recordings move back {} (input + output){}",
                        ms(input, rate),
                        ms(output, rate),
                        ms(st.buffer_size + output, rate),
                        ms(input + output, rate),
                        if graph > 0 {
                            format!(" + {} of plugin delay", ms(graph, rate))
                        } else {
                            String::new()
                        }
                    ),
                );
            }
            _ => {
                set(&self.running, "No audio stream is running");
                set(&self.asked, "");
                set(&self.latency, "");
            }
        }
        // Where each channel goes.
        let r = &status.routing;
        let mut lines = Vec::new();
        if let Some(info) = &info {
            for c in 0..info.input_channels as usize {
                lines.push(format!(
                    "In {:<3} ← {}",
                    c + 1,
                    r.input(c).unwrap_or("not connected")
                ));
            }
            for c in 0..info.output_channels as usize {
                lines.push(format!(
                    "Out {:<2} → {}",
                    c + 1,
                    r.output(c).unwrap_or("not connected")
                ));
            }
            if r.inputs.is_empty() && r.outputs.is_empty() {
                lines = vec!["This audio system does not say where the channels go".into()];
            }
        }
        set(&self.mapping, &lines.join("\n"));
        let mut health = match &status.health {
            AudioHealth::Stopped => String::new(),
            AudioHealth::Running => String::new(),
            AudioHealth::Fallback { missing } => format!(
                "Not connected: {} — playing on the default device until it is back (FaderFrame \
                 moves back to it by itself, once playback stops)",
                missing.join(", ")
            ),
            AudioHealth::Reconnecting { since, device } => format!(
                "{device} went away {} s ago: FaderFrame opens it again as soon as it is back, \
                 else the default device",
                since.elapsed().as_secs()
            ),
        };
        if !r.missing.is_empty() && matches!(status.health, AudioHealth::Running) {
            health = format!(
                "Not connected now: {} — its channels reconnect by themselves when it is back",
                r.missing.join(", ")
            );
        }
        for n in &status.notes {
            if !health.is_empty() {
                health.push('\n');
            }
            health.push_str(n);
        }
        self.health.set_visible(!health.is_empty());
        set(&self.health, &health);
        drop(s);
        self.update_plan();
    }

    fn do_apply(self: &Rc<Self>) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let (choice, prefs) = self.wanted();
        let switched = choice != app.options.borrow().backend;
        {
            let mut o = app.options.borrow_mut();
            o.backend = choice;
            o.sample_rate = prefs.sample_rate;
            o.buffer_size = prefs.buffer_size;
            o.threads = prefs.threads;
        }
        let mut p = Preferences::load();
        p.set_audio_preferences(choice, &prefs);
        if let Err(e) = p.save() {
            tracing::warn!("cannot save preferences: {e}");
        }
        *self.base.borrow_mut() = prefs.clone();
        let text = if switched {
            app.start_audio();
            format!("Switched to {}", choice.label())
        } else {
            match app.with_session(|s| s.apply_audio(prefs)) {
                Some(Applied::Live) => "Applied to the running stream".into(),
                Some(Applied::Restarted) => "The audio device was reopened".into(),
                Some(Applied::Unchanged) => "Nothing changed".into(),
                None => "Not applied (see the message)".into(),
            }
        };
        self.refresh();
        self.plan.set_text(&text);
        if switched {
            self.rescan();
        }
    }
}

/// What the audio system itself decides, said once under its settings.
fn system_note(scan: &Scan, device: Option<&DeviceInfo>) -> String {
    if let Some(e) = &scan.error {
        return format!("{e}.");
    }
    let rates = |d: &DeviceInfo| {
        d.sample_rates
            .iter()
            .map(|r| format_sample_rate(*r))
            .collect::<Vec<_>>()
            .join(", ")
    };
    match scan.backend {
        Some("pipewire") => {
            let mut t = "PipeWire's graph runs every program: FaderFrame asks it for this rate \
                         and buffer size and follows what it runs at."
                .to_string();
            if let Some(d) = device {
                if !d.sample_rates.is_empty() {
                    t.push_str(&format!(" It allows {}", rates(d)));
                }
                if let Some((lo, hi)) = d.buffer_sizes {
                    t.push_str(&format!(" and {lo}–{hi} frames"));
                }
                t.push('.');
            }
            t
        }
        Some("jack") => "The JACK server owns the rate and the buffer size: a buffer size is \
                         asked of it (for every JACK program) and applies at once; set the rate \
                         in the server. Devices are its clients with physical ports."
            .into(),
        Some("asio") => "ASIO: one driver plays and records; set its own options in the \
                         driver's control panel (from the interface maker)."
            .into(),
        Some("dummy") => "No device: the engine runs on a timer, silently.".into(),
        Some(_) => {
            let mut t = format!(
                "{} opens the device at the rate and buffer size chosen, if it takes them \
                 (else at its own, with a notice); a change reopens it.",
                scan.name
            );
            if let Some(d) = device
                && !d.sample_rates.is_empty()
            {
                t.push_str(&format!(" {} takes {}.", d.name, rates(d)));
            }
            t
        }
        None => String::new(),
    }
}

/// The Audio page's rows from `y` on in `g`: the interface, its state
/// and processing; returns the next free row.
pub fn rows(app: &Rc<AppState>, alive: &Rc<Cell<bool>>, g: &gtk::Grid, mut y: i32) -> i32 {
    let opts = app.options.borrow().clone();
    let stored = Preferences::load().audio_preferences(&opts);
    let next = |y: &mut i32| {
        let v = *y;
        *y += 1;
        v
    };
    g.attach(&heading("AUDIO INTERFACE"), 0, next(&mut y), 2, 1);
    let backends = BackendChoice::available();
    let names: Vec<String> = backends.iter().map(|b| b.label().to_string()).collect();
    let backend =
        gtk::DropDown::from_strings(&names.iter().map(String::as_str).collect::<Vec<_>>());
    backend.set_tooltip_text(Some(if cfg!(target_os = "linux") {
        "PipeWire: one graph for every program; FaderFrame asks it for a rate and buffer size, \
         and devices with clocks of their own can be combined (PipeWire resamples the ones \
         that follow). JACK: the server owns the rate and buffer size. ALSA: one device \
         opened directly."
    } else {
        "The system's own API opens one device at the rate and buffer size chosen; input from \
         another device runs on another clock."
    }));
    backend.set_selected(
        backends
            .iter()
            .position(|b| *b == opts.backend)
            .unwrap_or(0) as u32,
    );
    row(g, next(&mut y), "Audio system", &backend);
    let dd = || gtk::DropDown::from_strings(&[""]);
    let devices = [dd(), dd(), dd(), dd()];
    let labels = [
        "Output device",
        "Also play on",
        "Input device",
        "Also record from",
    ];
    let device_rows: [gtk::Label; 4] =
        std::array::from_fn(|i| row(g, next(&mut y), labels[i], &devices[i]));
    devices[1].set_tooltip_text(Some(
        "A second interface: its outputs follow the first one's (with two of 2 outputs, \
         outputs 3–4 are the second's)",
    ));
    devices[3].set_tooltip_text(Some(
        "A second interface: its inputs follow the first one's",
    ));
    let clock_note = note("");
    clock_note.set_visible(false);
    g.attach(&clock_note, 1, next(&mut y), 1, 1);
    let rate = dd();
    let server_rate = gtk::Label::new(None);
    server_rate.set_xalign(0.0);
    let rate_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    rate_box.append(&rate);
    rate_box.append(&server_rate);
    rate.set_hexpand(true);
    row(g, next(&mut y), "Sample rate", &rate_box);
    let buffer = dd();
    row(g, next(&mut y), "Buffer size", &buffer);
    let mut counts = vec!["Automatic: as the devices have".to_string()];
    counts.extend(CHANNEL_COUNTS.iter().map(|c| c.to_string()));
    let refs: Vec<&str> = counts.iter().map(String::as_str).collect();
    let inputs = gtk::DropDown::from_strings(&refs);
    let outputs = gtk::DropDown::from_strings(&refs);
    let channels = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let l_in = gtk::Label::new(Some("Inputs"));
    let l_out = gtk::Label::new(Some("Outputs"));
    inputs.update_relation(&[gtk::accessible::Relation::LabelledBy(&[l_in.upcast_ref()])]);
    outputs.update_relation(&[gtk::accessible::Relation::LabelledBy(&[l_out.upcast_ref()])]);
    channels.append(&l_in);
    channels.append(&inputs);
    channels.append(&l_out);
    channels.append(&outputs);
    channels.set_tooltip_text(Some(
        "Device channels FaderFrame opens. The project can need more — a track on an \
         interface output, a hardware insert, cue mixes, a surround master — and gets them",
    ));
    row(g, next(&mut y), "Channels", &channels);
    let force = gtk::CheckButton::with_label(
        "Hold the PipeWire graph at this rate and buffer size while FaderFrame runs",
    );
    force.set_tooltip_text(Some(
        "node.force-rate / node.force-quantum: every program in the graph runs at them until \
         FaderFrame closes its stream (nothing is changed in PipeWire's own settings)",
    ));
    force.set_active(stored.force_format);
    g.attach(&force, 1, next(&mut y), 1, 1);
    let system_note = note("");
    g.attach(&system_note, 1, next(&mut y), 1, 1);

    // Apply, and what it will do.
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let apply = gtk::Button::with_label("Apply");
    apply.add_css_class("suggested-action");
    let refresh = gtk::Button::with_label("Look for Devices Again");
    let reset = gtk::Button::with_label("Reset Statistics");
    buttons.append(&apply);
    buttons.append(&refresh);
    buttons.append(&reset);
    g.attach(&buttons, 1, next(&mut y), 1, 1);
    let plan = note("");
    g.attach(&plan, 1, next(&mut y), 1, 1);

    // What runs.
    g.attach(&heading("RUNNING"), 0, next(&mut y), 2, 1);
    let label = || {
        let l = gtk::Label::new(None);
        l.set_xalign(0.0);
        l.set_wrap(true);
        l.set_selectable(true);
        l
    };
    let running = label();
    row(g, next(&mut y), "Stream", &running);
    let asked = label();
    asked.add_css_class("dim-label");
    g.attach(&asked, 1, next(&mut y), 1, 1);
    let latency = label();
    row(g, next(&mut y), "Latency", &latency);
    let health = label();
    health.add_css_class("warning");
    health.set_visible(false);
    g.attach(&health, 1, next(&mut y), 1, 1);
    let mapping = label();
    mapping.add_css_class("monospace");
    let expander = gtk::Expander::new(Some("Where each channel goes"));
    expander.set_child(Some(&mapping));
    expander.set_expanded(true);
    g.attach(&expander, 1, next(&mut y), 1, 1);
    let stats = gtk::Label::new(None);
    stats.set_xalign(0.0);
    stats.add_css_class("monospace");
    row(g, next(&mut y), "DSP load", &stats);

    // Processing.
    g.attach(&heading("PROCESSING"), 0, next(&mut y), 2, 1);
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    let mut thread_names = vec![format!("Automatic ({cores})")];
    thread_names.extend((1..=cores).map(|n| match n {
        1 => "1 (audio thread only)".to_string(),
        n => n.to_string(),
    }));
    let thread_refs: Vec<&str> = thread_names.iter().map(String::as_str).collect();
    let threads = gtk::DropDown::from_strings(&thread_refs);
    threads.set_selected(opts.threads.map_or(0, |t| (t as usize).min(cores) as u32));
    threads.set_tooltip_text(Some(
        "Threads that process tracks, buses and plugins in parallel, the audio thread included \
         (applied with Apply: the audio device reopens)",
    ));
    row(g, next(&mut y), "Processing threads", &threads);
    y = processing_rows(app, g, y);

    let page = Rc::new(Page {
        app: Rc::downgrade(app),
        backend,
        devices,
        device_rows,
        ids: RefCell::default(),
        shown_output: RefCell::default(),
        clock_note,
        rate,
        rates: RefCell::default(),
        server_rate,
        buffer,
        buffers: RefCell::default(),
        inputs,
        outputs,
        force,
        system_note,
        threads,
        apply: apply.clone(),
        plan,
        scan: RefCell::default(),
        scanning: RefCell::default(),
        base: RefCell::new(stored),
        building: Cell::new(false),
        running,
        asked,
        latency,
        mapping,
        health,
    });
    {
        let weak = Rc::downgrade(&page);
        page.backend.connect_selected_notify(move |_| {
            if let Some(p) = weak.upgrade()
                && !p.building.get()
            {
                p.rescan();
                p.update_plan();
            }
        });
    }
    let changed = {
        let weak = Rc::downgrade(&page);
        move || {
            if let Some(p) = weak.upgrade()
                && !p.building.get()
            {
                p.update_clock_note();
                p.update_plan();
            }
        }
    };
    for dd in page.devices.iter().chain([
        &page.rate,
        &page.buffer,
        &page.inputs,
        &page.outputs,
        &page.threads,
    ]) {
        let changed = changed.clone();
        dd.connect_selected_notify(move |_| changed());
    }
    {
        let changed = changed.clone();
        page.force.connect_toggled(move |_| changed());
    }
    // A new output device: its rates and buffer sizes.
    {
        let weak = Rc::downgrade(&page);
        page.devices[0].connect_selected_notify(move |_| {
            if weak.upgrade().is_none_or(|p| p.building.get()) {
                return;
            }
            // Not inside this signal: filling the page replaces this
            // drop-down's model, and with it the selection still emitting.
            let weak = weak.clone();
            glib::idle_add_local_once(move || {
                let Some(p) = weak.upgrade() else {
                    return;
                };
                // Read out first: `show_scan` stores the scan again.
                let scan = p.scan.borrow().clone();
                let scanning = p.scanning.borrow().is_some();
                let other = p.selected_output() != *p.shown_output.borrow();
                if !scanning
                    && other
                    && let Some(scan) = scan
                {
                    p.show_scan_keeping(scan);
                }
            });
        });
    }
    {
        let weak = Rc::downgrade(&page);
        apply.connect_clicked(move |_| {
            if let Some(p) = weak.upgrade() {
                p.do_apply();
            }
        });
    }
    {
        let weak = Rc::downgrade(&page);
        refresh.connect_clicked(move |_| {
            if let Some(p) = weak.upgrade() {
                p.rescan();
            }
        });
    }
    {
        let weak = Rc::downgrade(app);
        reset.connect_clicked(move |_| {
            if let Some(app) = weak.upgrade() {
                app.session.borrow().engine().reset_metrics();
            }
        });
    }
    page.rescan();
    // The refresh owns the page (the controls' handlers hold it weakly)
    // until the window closes.
    let app_weak = Rc::downgrade(app);
    let alive = Rc::clone(alive);
    let tick = move || {
        let Some(app) = app_weak.upgrade() else {
            return glib::ControlFlow::Break;
        };
        if !alive.get() {
            return glib::ControlFlow::Break;
        }
        page.refresh();
        stats.set_text(&dsp_load(&app));
        glib::ControlFlow::Continue
    };
    tick();
    glib::timeout_add_local(Duration::from_millis(500), tick);
    y
}

/// The page's unapplied choices, kept when it is filled again.
struct Keep {
    /// Each device slot's pick (`None`: the default / none).
    devices: [Option<String>; 4],
    /// Rate, buffer size, channels and format as the controls show them.
    settings: AudioPreferences,
}

impl Page {
    /// Show `scan` again keeping the choices made (an output device
    /// changed: the rates and sizes it takes).
    fn show_scan_keeping(self: &Rc<Self>, scan: Scan) {
        let devices = std::array::from_fn(|s| self.selected_id(s));
        let (_, settings) = self.wanted();
        self.show_scan(scan, Some(Keep { devices, settings }));
    }
}

/// The processing settings that apply at once (plugin precision, render
/// ahead); returns the next free row.
fn processing_rows(app: &Rc<AppState>, g: &gtk::Grid, mut y: i32) -> i32 {
    // Plugin precision: applies at once (the plugins are reactivated).
    let precision = gtk::CheckButton::with_label(
        "Process plugins in 64-bit floating point where they support it",
    );
    precision.set_active(app.session.borrow().plugin_double_precision());
    precision.set_tooltip_text(Some(
        "VST3 and CLAP plugins that offer double precision get 64-bit buffers; \
         FaderFrame's own mixing stays 32-bit",
    ));
    {
        let weak = Rc::downgrade(app);
        precision.connect_toggled(move |b| {
            let on = b.is_active();
            let mut p = Preferences::load();
            p.plugin_double_precision = on;
            if let Err(e) = p.save() {
                tracing::warn!("cannot save preferences: {e}");
            }
            if let Some(app) = weak.upgrade() {
                app.with_session(|s| s.set_plugin_double_precision(on));
            }
        });
    }
    row(g, y, "Plugin precision", &precision);
    y += 1;

    // Render ahead: applies at once.
    const AHEAD: [(u32, &str); 4] = [
        (0, "Off: every track on the audio thread"),
        (100, "100 ms"),
        (200, "200 ms (recommended)"),
        (500, "500 ms"),
    ];
    let ahead_names: Vec<&str> = AHEAD.iter().map(|(_, n)| *n).collect();
    let ahead = gtk::DropDown::from_strings(&ahead_names);
    let now_ms = app
        .session
        .borrow()
        .render_ahead()
        .map_or(0, |d| d.as_millis() as u32);
    ahead.set_selected(AHEAD.iter().position(|(ms, _)| *ms == now_ms).unwrap_or(0) as u32);
    ahead.set_tooltip_text(Some(
        "Tracks nobody plays live are rendered this far ahead on threads of their own, so \
         heavy plugins need not finish within one buffer. Armed and live tracks and tracks \
         with a plugin editor open play immediately; changes to the others' plugins and clips \
         are heard after this time.",
    ));
    {
        let weak = Rc::downgrade(app);
        ahead.connect_selected_notify(move |d| {
            let ms = AHEAD[(d.selected() as usize).min(AHEAD.len() - 1)].0;
            let mut p = Preferences::load();
            p.render_ahead_ms = ms;
            if let Err(e) = p.save() {
                tracing::warn!("cannot save preferences: {e}");
            }
            if let Some(app) = weak.upgrade() {
                let lookahead = (ms > 0).then(|| Duration::from_millis(u64::from(ms)));
                app.with_session(|s| s.set_render_ahead(lookahead));
            }
        });
    }
    let buses = gtk::CheckButton::with_label("Buses too");
    buses.set_active(app.session.borrow().render_ahead_buses());
    buses.set_tooltip_text(Some(
        "Also render buses, auxes and the master's devices ahead when everything reaching \
         them is: their plugins leave the audio thread too. They and the faders reaching \
         them run only an audio buffer and a few milliseconds ahead, so faders still \
         answer at once; automation stays exact, meters in time.",
    ));
    {
        let weak = Rc::downgrade(app);
        buses.connect_toggled(move |b| {
            let on = b.is_active();
            let mut p = Preferences::load();
            p.render_ahead_buses = on;
            if let Err(e) = p.save() {
                tracing::warn!("cannot save preferences: {e}");
            }
            if let Some(app) = weak.upgrade() {
                app.with_session(|s| s.set_render_ahead_buses(on));
            }
        });
    }
    let ahead_row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    ahead_row.append(&ahead);
    ahead_row.append(&buses);
    row(g, y, "Render ahead", &ahead_row);
    y += 1;
    g.attach(
        &note(
            "Any buffer size from 16 to 8192 frames works; larger device buffers are processed \
             in internal blocks. Tracks, buses and plugins are spread over the processing \
             threads; with realtime permission they run at the audio thread's priority.",
        ),
        0,
        y,
        2,
        1,
    );
    y + 1
}

/// The DSP load and stream statistics (as the old page showed them).
fn dsp_load(app: &Rc<AppState>) -> String {
    let s = app.session.borrow();
    let m = s.metrics();
    let us = |ns: u64| ns as f64 / 1000.0;
    let (resident, disk_misses) = s.streaming_stats();
    let (ahead_tracks, ahead_late) = s.render_ahead_status();
    let ahead = if s.render_ahead().is_some() {
        format!(
            "\nrendered ahead: {ahead_tracks} track{} · {ahead_late} late block{}",
            if ahead_tracks == 1 { "" } else { "s" },
            if ahead_late == 1 { "" } else { "s" }
        )
    } else {
        String::new()
    };
    format!(
        "p50 {:.0} µs · p95 {:.0} µs · p99 {:.0} µs · max {:.0} µs\n{} callbacks · {} deadline misses · {} xruns · budget {:.0} µs\ndisk: {:.1} MiB resident · {} late reads\n{} processing thread{}{}{ahead}",
        us(m.p50_ns),
        us(m.p95_ns),
        us(m.p99_ns),
        us(m.max_ns),
        m.callbacks,
        m.deadline_misses,
        s.stream_status().map_or(0, |st| st.xruns),
        us(m.last_budget_ns),
        resident as f64 / (1 << 20) as f64,
        disk_misses,
        s.processing_threads(),
        if s.processing_threads() == 1 { "" } else { "s" },
        match s.workers_without_realtime() {
            0 => String::new(),
            n => format!(" · {n} without realtime priority"),
        }
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::state::RunOptions;
    use std::time::Instant;

    /// With a display: choosing an output device, then another audio
    /// system, rebuilds the page from inside its own handlers (both
    /// panicked with "RefCell already borrowed" in 0.23 and 0.24).
    #[test]
    #[ignore = "requires a GTK display"]
    fn the_output_device_and_the_audio_system_can_be_changed() {
        gtk::init().expect("GTK display");
        let session = faderframe_session::Session::new(
            faderframe_project::Project::new("Audio", 48_000),
            None,
            faderframe_engine::EngineConfig::default(),
        )
        .unwrap();
        let options = RunOptions {
            backend: BackendChoice::Dummy,
            ..RunOptions::default()
        };
        let app = AppState::new(
            &gtk::Application::new(None::<&str>, Default::default()),
            session,
            options,
        );
        let alive = Rc::new(Cell::new(true));
        let grid = gtk::Grid::new();
        rows(&app, &alive, &grid, 0);
        let dropdown = |y| grid.child_at(1, y).and_downcast::<gtk::DropDown>().unwrap();
        let (backend, output) = (dropdown(1), dropdown(2));
        // Run what is due; a handler that keeps itself going fails.
        let pump = |start: Instant| {
            while glib::MainContext::default().iteration(false) {
                assert!(start.elapsed() < Duration::from_secs(20), "busy");
            }
        };
        // The devices are listed on a thread: wait until they are shown.
        let listed = || {
            let start = Instant::now();
            while !output.is_sensitive() {
                assert!(start.elapsed() < Duration::from_secs(20), "no devices");
                pump(start);
                std::thread::sleep(Duration::from_millis(10));
            }
        };
        listed();
        let n = output.model().unwrap().n_items();
        assert!(n >= 2, "the default and the dummy device");
        output.set_selected(1);
        pump(Instant::now());
        assert_eq!(output.selected(), 1, "the device chosen stays chosen");
        let auto = BackendChoice::available()
            .iter()
            .position(|b| *b == BackendChoice::Auto)
            .unwrap();
        backend.set_selected(auto as u32);
        listed();
        alive.set(false);
    }
}
