//! The Preferences window (audio, editing, engine, project).

use crate::prefs::Preferences;
use crate::state::{AppState, BackendChoice};
use faderframe_audio::{STANDARD_BUFFER_SIZES, STANDARD_SAMPLE_RATES, format_sample_rate};
use faderframe_project::Command;
use faderframe_session::Action;
use gtk::glib;
use gtk::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

thread_local! {
    static OPEN: RefCell<Option<glib::WeakRef<gtk::Window>>> = const { RefCell::new(None) };
}

fn row(grid: &gtk::Grid, y: i32, label: &str, widget: &impl IsA<gtk::Widget>) {
    let l = gtk::Label::new(Some(label));
    l.set_xalign(1.0);
    l.add_css_class("dim-label");
    grid.attach(&l, 0, y, 1, 1);
    widget.set_hexpand(true);
    grid.attach(widget, 1, y, 1, 1);
}

fn form() -> gtk::Grid {
    let g = gtk::Grid::new();
    g.set_row_spacing(10);
    g.set_column_spacing(14);
    g.add_css_class("audio-settings");
    g
}

fn note(text: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    l.set_wrap(true);
    l.set_xalign(0.0);
    l.add_css_class("dim-label");
    l
}

fn audio_page(app: &Rc<AppState>, alive: &Rc<std::cell::Cell<bool>>) -> gtk::Widget {
    let g = form();
    let opts = app.options.borrow().clone();
    let backends = BackendChoice::available();
    let backend_names: Vec<&str> = backends.iter().map(|b| b.label()).collect();
    let backend = gtk::DropDown::from_strings(&backend_names);
    backend.set_selected(
        backends
            .iter()
            .position(|b| *b == opts.backend)
            .unwrap_or(0) as u32,
    );
    row(&g, 0, "Audio system", &backend);

    let mut rate_names = vec!["Device / server default".to_string()];
    rate_names.extend(STANDARD_SAMPLE_RATES.iter().map(|r| format_sample_rate(*r)));
    let rate_refs: Vec<&str> = rate_names.iter().map(String::as_str).collect();
    let rate = gtk::DropDown::from_strings(&rate_refs);
    rate.set_selected(
        opts.sample_rate
            .and_then(|r| STANDARD_SAMPLE_RATES.iter().position(|x| *x == r))
            .map_or(0, |i| i as u32 + 1),
    );
    row(&g, 1, "Sample rate", &rate);

    let mut buf_names = vec!["Device / server default".to_string()];
    buf_names.extend(STANDARD_BUFFER_SIZES.iter().map(|b| format!("{b} frames")));
    let buf_refs: Vec<&str> = buf_names.iter().map(String::as_str).collect();
    let buffer = gtk::DropDown::from_strings(&buf_refs);
    buffer.set_selected(
        opts.buffer_size
            .and_then(|b| STANDARD_BUFFER_SIZES.iter().position(|x| *x == b))
            .map_or(0, |i| i as u32 + 1),
    );
    row(&g, 2, "Buffer size", &buffer);

    // Processing threads: automatic (one per core) or a fixed count.
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
        "Threads that process tracks, buses and plugins in parallel, the audio thread included",
    ));
    row(&g, 3, "Processing threads", &threads);

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
    row(&g, 4, "Plugin precision", &precision);

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
    row(&g, 5, "Render ahead", &ahead_row);

    let status = gtk::Label::new(None);
    status.set_xalign(0.0);
    status.set_selectable(true);
    row(&g, 6, "Stream", &status);
    let stats = gtk::Label::new(None);
    stats.set_xalign(0.0);
    stats.add_css_class("monospace");
    row(&g, 7, "DSP load", &stats);

    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let apply = gtk::Button::with_label("Apply & Restart Audio");
    apply.add_css_class("suggested-action");
    let live = gtk::Button::with_label("Change Buffer Size Now");
    live.set_tooltip_text(Some(
        "Ask the running JACK/PipeWire graph for the selected buffer size without restarting",
    ));
    let reset = gtk::Button::with_label("Reset Statistics");
    buttons.append(&apply);
    buttons.append(&live);
    buttons.append(&reset);
    g.attach(&buttons, 1, 8, 1, 1);
    g.attach(
        &note(
            "JACK and PipeWire own the sample rate: FaderFrame follows whatever the server runs at \
             and rebuilds its engine automatically when it changes. Any buffer size from 16 to 8192 \
             frames works; larger device buffers are processed in internal blocks. Tracks, buses and \
             plugins are spread over the processing threads; with realtime permission they run at \
             the audio thread's priority.",
        ),
        0,
        9,
        2,
        1,
    );

    let selected = move |backend: &gtk::DropDown, rate: &gtk::DropDown, buffer: &gtk::DropDown| {
        let backends = BackendChoice::available();
        let b = backends[backend.selected() as usize % backends.len()];
        let r = (rate.selected() > 0).then(|| STANDARD_SAMPLE_RATES[rate.selected() as usize - 1]);
        let f =
            (buffer.selected() > 0).then(|| STANDARD_BUFFER_SIZES[buffer.selected() as usize - 1]);
        (b, r, f)
    };
    let weak = Rc::downgrade(app);
    apply.connect_clicked(glib::clone!(
        #[weak]
        backend,
        #[weak]
        rate,
        #[weak]
        buffer,
        #[weak]
        threads,
        move |_| {
            let Some(app) = weak.upgrade() else { return };
            let (b, r, f) = selected(&backend, &rate, &buffer);
            let t = (threads.selected() > 0).then(|| threads.selected() as u16);
            {
                let mut o = app.options.borrow_mut();
                o.backend = b;
                o.sample_rate = r;
                o.buffer_size = f;
                o.threads = t;
            }
            let mut prefs = Preferences::load();
            prefs.set_backend(b);
            prefs.sample_rate = r;
            prefs.buffer_size = f;
            prefs.threads = t;
            if let Err(e) = prefs.save() {
                tracing::warn!("cannot save preferences: {e}");
            }
            app.start_audio();
        }
    ));
    let weak = Rc::downgrade(app);
    live.connect_clicked(glib::clone!(
        #[weak]
        buffer,
        move |_| {
            let Some(app) = weak.upgrade() else { return };
            if buffer.selected() == 0 {
                return;
            }
            let frames = STANDARD_BUFFER_SIZES[buffer.selected() as usize - 1];
            app.with_session(|s| s.request_buffer_size(frames));
        }
    ));
    let weak = Rc::downgrade(app);
    reset.connect_clicked(move |_| {
        if let Some(app) = weak.upgrade() {
            app.session.borrow().engine().reset_metrics();
        }
    });

    let weak = Rc::downgrade(app);
    let alive = Rc::clone(alive);
    let refresh = move || {
        let Some(app) = weak.upgrade() else {
            return glib::ControlFlow::Break;
        };
        if !alive.get() {
            return glib::ControlFlow::Break;
        }
        let s = app.session.borrow();
        status.set_text(&match (s.stream_info(), s.stream_status()) {
            (Some(info), Some(st)) => format!(
                "{} · {} · {} · {} frames ({:.2} ms) · {} in / {} out",
                info.backend.to_uppercase(),
                info.device,
                format_sample_rate(st.sample_rate),
                st.buffer_size,
                st.buffer_size as f64 * 1000.0 / st.sample_rate.max(1) as f64,
                info.input_channels,
                info.output_channels
            ),
            _ => "No audio stream is running".into(),
        });
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
        stats.set_text(&format!(
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
            if s.worker_priority_failures() > 0 {
                " · no realtime permission for the workers"
            } else {
                ""
            }
        ));
        glib::ControlFlow::Continue
    };
    refresh();
    glib::timeout_add_local(Duration::from_millis(500), refresh);
    g.upcast()
}

fn general_page(app: &Rc<AppState>) -> gtk::Widget {
    use crate::recent::StartupProject;
    let g = form();
    let prefs = Preferences::load();
    let labels: Vec<&str> = StartupProject::ALL.iter().map(|s| s.label()).collect();
    let startup = gtk::DropDown::from_strings(&labels);
    let current = StartupProject::from_id(&prefs.startup_project).unwrap_or_default();
    startup.set_selected(
        StartupProject::ALL
            .iter()
            .position(|s| *s == current)
            .unwrap_or(0) as u32,
    );
    row(&g, 0, "On start-up", &startup);
    g.attach(
        &note("A project given on the command line always wins; --empty and --demo choose for one start."),
        0,
        1,
        2,
        1,
    );
    let recent = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    let count = gtk::Label::new(Some(&format!(
        "{} remembered (File → Open Recent)",
        prefs.recent_projects.len()
    )));
    count.set_xalign(0.0);
    count.set_hexpand(true);
    let clear = gtk::Button::with_label("Clear");
    clear.set_sensitive(!prefs.recent_projects.is_empty());
    recent.append(&count);
    recent.append(&clear);
    row(&g, 2, "Recent projects", &recent);
    let themes = faderframe_ui_canvas::Theme::all();
    let names: Vec<&str> = themes.iter().map(|t| t.name).collect();
    let theme = gtk::DropDown::from_strings(&names);
    let current = app.theme.borrow().id;
    theme.set_selected(themes.iter().position(|t| t.id == current).unwrap_or(0) as u32);
    row(&g, 3, "Theme", &theme);
    let storage = match faderframe_core::paths::portable_root() {
        Some(dir) => format!(
            "Portable: settings, caches, presets and unsaved recordings are kept in {}",
            dir.display()
        ),
        None => format!(
            "In your profile ({}). Put a folder named “{}” next to the program to make a copy portable.",
            crate::paths::config_dir().display(),
            faderframe_core::paths::PORTABLE_FOLDER
        ),
    };
    let storage = note(&storage);
    storage.set_wrap(true);
    row(&g, 4, "Data", &storage);
    let sandbox = gtk::CheckButton::with_label(
        "Run each plugin in its own process (a crashing plugin cannot take FaderFrame down)",
    );
    sandbox.set_active(prefs.sandbox_plugins && faderframe_plugin_sandbox::AVAILABLE);
    sandbox.set_sensitive(faderframe_plugin_sandbox::AVAILABLE);
    row(&g, 5, "Plugins", &sandbox);
    let sandbox_note = note(if faderframe_plugin_sandbox::AVAILABLE {
        if cfg!(target_os = "macos") {
            "CLAP, VST3 and Audio Unit plugins; their editors open in windows of their own. Changing this restarts the loaded plugins (their settings are kept)."
        } else {
            "CLAP and VST3 plugins. Changing this restarts the loaded plugins (their settings are kept)."
        }
    } else {
        "Not available on this platform yet: plugins run inside FaderFrame."
    });
    sandbox_note.set_wrap(true);
    g.attach(&sandbox_note, 1, 6, 1, 1);
    let gpu = gtk::CheckButton::with_label("Draw analysers, meters and curves on the GPU");
    gpu.set_active(app.gpu_painter.get() && crate::gpu::available());
    gpu.set_sensitive(crate::gpu::available());
    gpu.set_tooltip_text(Some(
        "The mixer, the mastering tools and device editors such as the EQ are drawn with \
         the graphics card (vello on wgpu) instead of GTK's renderer, which rasterises \
         every changing curve on the CPU. Without a usable GPU they stay as they are.",
    ));
    row(&g, 7, "Drawing", &gpu);
    {
        let weak = Rc::downgrade(app);
        gpu.connect_toggled(move |b| {
            let mut p = Preferences::load();
            p.gpu_painter = b.is_active();
            if let Err(e) = p.save() {
                tracing::warn!("cannot save preferences: {e}");
            }
            if let Some(app) = weak.upgrade() {
                app.gpu_painter.set(crate::gpu::wanted(b.is_active()));
                app.redraw_all();
            }
        });
    }
    {
        let weak = Rc::downgrade(app);
        sandbox.connect_toggled(move |b| {
            let mut p = Preferences::load();
            p.sandbox_plugins = b.is_active();
            if let Err(e) = p.save() {
                tracing::warn!("cannot save preferences: {e}");
            }
            faderframe_plugin_sandbox::set_enabled(b.is_active());
            if let Some(app) = weak.upgrade() {
                app.dispatch(faderframe_session::Action::ReloadAllPlugins);
            }
        });
    }
    {
        let weak = Rc::downgrade(app);
        theme.connect_selected_notify(move |d| {
            let themes = faderframe_ui_canvas::Theme::all();
            let t = &themes[(d.selected() as usize).min(themes.len() - 1)];
            if let Some(app) = weak.upgrade()
                && app.theme.borrow().id != t.id
            {
                app.set_theme(t.id);
            }
        });
    }
    startup.connect_selected_notify(|d| {
        let mut p = Preferences::load();
        let choice =
            StartupProject::ALL[(d.selected() as usize).min(StartupProject::ALL.len() - 1)];
        p.startup_project = choice.id().into();
        if let Err(e) = p.save() {
            tracing::warn!("cannot save preferences: {e}");
        }
    });
    let weak = Rc::downgrade(app);
    clear.connect_clicked(move |b| {
        if let Some(app) = weak.upgrade() {
            crate::recent::clear(&app);
            count.set_text("0 remembered (File → Open Recent)");
            b.set_sensitive(false);
        }
    });
    g.upcast()
}

fn editing_page(app: &Rc<AppState>) -> gtk::Widget {
    let g = form();
    let ed = app.session.borrow().editor;
    let snap = gtk::CheckButton::with_label("Snap to grid");
    snap.set_active(ed.snap);
    let follow = gtk::CheckButton::with_label("Follow the playhead while playing");
    follow.set_active(ed.follow_playhead);
    g.attach(&snap, 1, 0, 1, 1);
    g.attach(&follow, 1, 1, 1, 1);
    g.attach(
        &note("Grid resolution is chosen from the arranger's top-left corner. Hold Alt while dragging to bypass snapping; Shift or Ctrl give fine control on faders and knobs."),
        0,
        2,
        2,
        1,
    );
    let persist = |snap: bool, follow: bool| {
        let mut p = Preferences::load();
        p.snap = snap;
        p.follow_playhead = follow;
        let _ = p.save();
    };
    let weak = Rc::downgrade(app);
    snap.connect_toggled(move |b| {
        let Some(app) = weak.upgrade() else { return };
        let ed = app.session.borrow().editor;
        if ed.snap != b.is_active() {
            app.dispatch(Action::ToggleSnap);
        }
        persist(b.is_active(), ed.follow_playhead);
    });
    let weak = Rc::downgrade(app);
    follow.connect_toggled(move |b| {
        let Some(app) = weak.upgrade() else { return };
        let ed = app.session.borrow().editor;
        if ed.follow_playhead != b.is_active() {
            app.dispatch(Action::ToggleFollowPlayhead);
        }
        persist(ed.snap, b.is_active());
    });
    g.upcast()
}

/// Video: proxies (their size, or none) and the cache of indexes and
/// proxies (where, how big, clearing it).
fn video_page(app: &Rc<AppState>) -> gtk::Widget {
    use faderframe_session::video::{PROXY_HEIGHTS, video_cache_size};
    let g = form();
    let prefs = Preferences::load();
    let mut labels = vec!["No proxies".to_string()];
    labels.extend(PROXY_HEIGHTS.iter().map(|h| format!("{h}p (MJPEG)")));
    let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    let proxy = gtk::DropDown::from_strings(&refs);
    proxy.set_selected(
        PROXY_HEIGHTS
            .iter()
            .position(|h| *h == prefs.video_proxy_height)
            .map_or(0, |i| i as u32 + 1),
    );
    row(&g, 0, "Proxies", &proxy);
    g.attach(
        &note(
            "Long-GOP and large pictures get an all-intra copy at this height for scrubbing; \
             stopped, frames are decoded sharp from the original, and exports always use it.",
        ),
        1,
        1,
        1,
        1,
    );
    let folder = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let path = gtk::Label::new(Some(
        &faderframe_session::video::cache_dir().display().to_string(),
    ));
    path.set_xalign(0.0);
    path.set_hexpand(true);
    path.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    path.set_selectable(true);
    let choose = gtk::Button::with_label("Choose…");
    let reset = gtk::Button::with_label("Default");
    reset.set_sensitive(prefs.video_cache_dir.is_some());
    folder.append(&path);
    folder.append(&choose);
    folder.append(&reset);
    row(&g, 2, "Cache folder", &folder);
    let size_text = |bytes: u64| {
        if bytes >= 1_000_000_000 {
            format!("{:.1} GB of indexes and proxies", bytes as f64 / 1e9)
        } else {
            format!("{:.0} MB of indexes and proxies", bytes as f64 / 1e6)
        }
    };
    let clear_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let size = gtk::Label::new(Some(&size_text(video_cache_size())));
    size.set_xalign(0.0);
    size.set_hexpand(true);
    let clear = gtk::Button::with_label("Clear Cache");
    clear_box.append(&size);
    clear_box.append(&clear);
    row(&g, 3, "Cache", &clear_box);
    let zero_copy =
        gtk::CheckButton::with_label("Show frames straight from the decoder (zero-copy)");
    zero_copy.set_active(prefs.video_zero_copy);
    zero_copy.set_sensitive(cfg!(target_os = "linux"));
    row(&g, 4, "Display", &zero_copy);
    g.attach(
        &note(
            "VA-API decodes and scales the picture into video memory the display shows as it is \
             (Linux); without VA-API, or off, frames come through memory.",
        ),
        1,
        5,
        1,
        1,
    );
    // Full screen on a monitor of its own (a second screen as the
    // picture's).
    let mut monitors = vec!["Where the window is".to_string()];
    if let Some(display) = gtk::gdk::Display::default() {
        let list = display.monitors();
        for i in 0..list.n_items() {
            if let Some(m) = list.item(i).and_downcast::<gtk::gdk::Monitor>() {
                monitors.push(crate::video::monitor_name(&m));
            }
        }
    }
    let refs: Vec<&str> = monitors.iter().map(String::as_str).collect();
    let screen = gtk::DropDown::from_strings(&refs);
    screen.set_selected(
        prefs
            .video_fullscreen_monitor
            .as_ref()
            .and_then(|m| monitors.iter().position(|x| x == m))
            .unwrap_or(0) as u32,
    );
    row(&g, 6, "Full screen on", &screen);
    // A DeckLink card's SDI/HDMI output.
    let devices = faderframe_video::output::decklink_devices();
    let modes = faderframe_video::output::decklink_modes();
    let output_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let mut device_labels = vec!["Off".to_string()];
    device_labels.extend(devices.iter().map(|(_, n)| n.clone()));
    let refs: Vec<&str> = device_labels.iter().map(String::as_str).collect();
    let device = gtk::DropDown::from_strings(&refs);
    let mode_labels: Vec<&str> = modes.iter().map(|(_, l)| l.as_str()).collect();
    let mode = gtk::DropDown::from_strings(&mode_labels);
    let (cur_dev, cur_mode) = prefs
        .video_output
        .clone()
        .map_or((None, "1080p25".to_string()), |(d, m)| (Some(d), m));
    device.set_selected(
        cur_dev
            .and_then(|d| devices.iter().position(|(n, _)| *n == d))
            .map_or(0, |i| i as u32 + 1),
    );
    mode.set_selected(
        modes
            .iter()
            .position(|(id, _)| *id == cur_mode)
            .unwrap_or(0) as u32,
    );
    mode.set_sensitive(device.selected() > 0);
    output_box.append(&device);
    output_box.append(&mode);
    row(&g, 7, "Picture output", &output_box);
    g.attach(
        &note(if devices.is_empty() {
            "No Blackmagic DeckLink output found (its Desktop Video driver must be installed)."
        } else {
            "The picture on a Blackmagic DeckLink card's SDI or HDMI output, in step with the sound."
        }),
        1,
        8,
        1,
        1,
    );

    let apply = {
        let weak = Rc::downgrade(app);
        move |change: &dyn Fn(&mut Preferences)| {
            let mut p = Preferences::load();
            change(&mut p);
            if let Err(e) = p.save() {
                tracing::warn!("could not save the preferences: {e}");
            }
            if let Some(app) = weak.upgrade() {
                let settings = p.video_settings();
                app.with_session(|s| {
                    s.set_video_settings(settings);
                    Ok(())
                });
            }
        }
    };
    let apply = Rc::new(apply);
    {
        let apply = Rc::clone(&apply);
        proxy.connect_selected_notify(move |d| {
            let i = d.selected() as usize;
            let h = if i == 0 { 0 } else { PROXY_HEIGHTS[i - 1] };
            apply(&|p| p.video_proxy_height = h);
        });
    }
    {
        let names = monitors.clone();
        screen.connect_selected_notify(move |d| {
            let i = d.selected() as usize;
            let mut p = Preferences::load();
            p.video_fullscreen_monitor = (i > 0).then(|| names[i].clone());
            if let Err(e) = p.save() {
                tracing::warn!("could not save the preferences: {e}");
            }
        });
    }
    {
        let weak = Rc::downgrade(app);
        let choose = std::rc::Rc::new({
            let device = device.clone();
            let mode = mode.clone();
            move || {
                let d = device.selected() as usize;
                mode.set_sensitive(d > 0);
                let output = (d > 0).then(|| {
                    let m = modes
                        .get(mode.selected() as usize)
                        .map_or_else(|| "1080p25".to_string(), |(id, _)| id.clone());
                    (devices[d - 1].0, m)
                });
                let mut p = Preferences::load();
                p.video_output = output.clone();
                if let Err(e) = p.save() {
                    tracing::warn!("could not save the preferences: {e}");
                }
                if let Some(app) = weak.upgrade() {
                    let sink = output.map(|(device, mode)| {
                        faderframe_video::output::Sink::DeckLink { device, mode }
                    });
                    app.with_session(|s| s.set_picture_output(sink));
                }
            }
        });
        let c = std::rc::Rc::clone(&choose);
        device.connect_selected_notify(move |_| c());
        mode.connect_selected_notify(move |_| choose());
    }
    {
        let apply = Rc::clone(&apply);
        zero_copy.connect_toggled(move |b| {
            let on = b.is_active();
            apply(&|p| p.video_zero_copy = on);
            #[cfg(target_os = "linux")]
            faderframe_video::zero_copy::set_allowed(on);
        });
    }
    {
        let (apply, path, reset2, size2) =
            (Rc::clone(&apply), path.clone(), reset.clone(), size.clone());
        reset.connect_clicked(move |_| {
            apply(&|p| p.video_cache_dir = None);
            path.set_text(&faderframe_session::video::cache_dir().display().to_string());
            reset2.set_sensitive(false);
            size2.set_text(&size_text(video_cache_size()));
        });
    }
    {
        let (apply, path, reset, size2) =
            (Rc::clone(&apply), path.clone(), reset.clone(), size.clone());
        choose.connect_clicked(move |b| {
            let dialog = gtk::FileDialog::builder()
                .title("Video Cache Folder")
                .modal(true)
                .build();
            let window = b.root().and_downcast::<gtk::Window>();
            let (apply, path, reset, size2) = (
                Rc::clone(&apply),
                path.clone(),
                reset.clone(),
                size2.clone(),
            );
            dialog.select_folder(window.as_ref(), gtk::gio::Cancellable::NONE, move |res| {
                let Some(dir) = res.ok().and_then(|f| f.path()) else {
                    return;
                };
                let text = dir.display().to_string();
                apply(&|p| p.video_cache_dir = Some(text.clone()));
                path.set_text(&text);
                reset.set_sensitive(true);
                size2.set_text(&size_text(video_cache_size()));
            });
        });
    }
    {
        let (weak, size2) = (Rc::downgrade(app), size.clone());
        clear.connect_clicked(move |_| {
            let Some(app) = weak.upgrade() else { return };
            let mut freed = 0;
            app.with_session(|s| {
                freed = s.clear_video_cache();
                Ok(())
            });
            size2.set_text(&size_text(video_cache_size()));
            tracing::info!("video cache: {freed} bytes freed");
        });
    }
    g.upcast()
}

fn engine_page(app: &Rc<AppState>) -> gtk::Widget {
    let g = form();
    let s = app.session.borrow();
    let e = s.engine();
    let st = e.graph_stats();
    let sr = e.sample_rate() as f64;
    let lines = [
        (
            "Internal block size",
            format!("{} frames", e.config().max_block_size),
        ),
        (
            "Graph nodes / edges",
            format!("{} / {}", st.nodes, st.edges),
        ),
        (
            "Critical path",
            format!(
                "{} nodes deep, up to {} nodes in parallel",
                st.levels, st.max_width
            ),
        ),
        (
            "Delay compensation",
            format!(
                "{} samples ({:.2} ms) total, {} compensated connections",
                st.output_latency,
                st.output_latency as f64 * 1000.0 / sr.max(1.0),
                st.compensated_edges
            ),
        ),
        (
            "Scheduling",
            "single-threaded (dependency-aware multicore scheduler planned)".to_string(),
        ),
    ];
    for (i, (k, v)) in lines.iter().enumerate() {
        let l = gtk::Label::new(Some(v));
        l.set_xalign(0.0);
        l.set_selectable(true);
        row(&g, i as i32, k, &l);
    }
    let warnings = e.warnings().join("\n");
    if !warnings.is_empty() {
        g.attach(&note(&warnings), 0, lines.len() as i32, 2, 1);
    }
    g.upcast()
}

fn project_page(app: &Rc<AppState>) -> gtk::Widget {
    let g = form();
    let (name, bpm, rate, crosstalk) = {
        let s = app.session.borrow();
        let p = s.project();
        (
            p.name.clone(),
            p.timeline.tempo.points()[0].bpm,
            p.sample_rate,
            p.crosstalk,
        )
    };
    let name_entry = gtk::Entry::new();
    name_entry.set_text(&name);
    row(&g, 0, "Project name", &name_entry);
    let tempo = gtk::SpinButton::with_range(20.0, 400.0, 0.5);
    tempo.set_digits(2);
    tempo.set_value(bpm);
    row(&g, 1, "Tempo (BPM)", &tempo);
    let rate_label = gtk::Label::new(Some(&format_sample_rate(rate)));
    rate_label.set_xalign(0.0);
    row(&g, 2, "Project sample rate", &rate_label);
    g.attach(
        &note("Clip positions are stored in musical time; audio offsets are project-rate frames and are scaled when the engine runs at another rate."),
        0,
        3,
        2,
        1,
    );
    let leakage = gtk::CheckButton::with_label("Enable between neighbouring channels");
    leakage.set_active(crosstalk);
    leakage.set_tooltip_text(Some("Subtle analogue leakage between adjacent audio and instrument tracks in mixer order. Saved with this project."));
    row(&g, 4, "Analogue crosstalk", &leakage);
    let weak = Rc::downgrade(app);
    leakage.connect_toggled(move |button| {
        if let Some(app) = weak.upgrade() {
            app.dispatch(Action::Edit(Command::SetCrosstalk {
                enabled: button.is_active(),
            }));
        }
    });
    let weak = Rc::downgrade(app);
    name_entry.connect_activate(move |e| {
        if let Some(app) = weak.upgrade() {
            app.dispatch(Action::Edit(Command::RenameProject {
                name: e.text().to_string(),
            }));
        }
    });
    let weak = Rc::downgrade(app);
    tempo.connect_value_changed(move |t| {
        if let Some(app) = weak.upgrade() {
            app.dispatch(Action::Edit(Command::SetTempo { bpm: t.value() }));
        }
    });
    g.upcast()
}

/// Open (or raise) the preferences window, optionally on a given page.
pub fn open(app: &Rc<AppState>, page: Option<&str>) {
    if let Some(win) = OPEN.with(|o| o.borrow().as_ref().and_then(|w| w.upgrade())) {
        win.present();
        return;
    }
    let win = gtk::Window::builder()
        .application(&app.app)
        .title("Preferences — FaderFrame")
        .default_width(820)
        .default_height(640)
        .build();
    if let Some(main) = app.window.borrow().as_ref() {
        win.set_transient_for(Some(main));
    }
    crate::actions::install_window_keys(app, &win);
    let alive = Rc::new(std::cell::Cell::new(true));
    let stack = gtk::Stack::new();
    stack.set_transition_type(gtk::StackTransitionType::Crossfade);
    // Size by the visible page only (hidden pages are not measured).
    stack.set_hhomogeneous(false);
    stack.set_vhomogeneous(false);
    stack.add_titled(&general_page(app), Some("general"), "General");
    stack.add_titled(&audio_page(app, &alive), Some("audio"), "Audio");
    stack.add_titled(&editing_page(app), Some("editing"), "Editing");
    stack.add_titled(
        &crate::recording::page(app, row),
        Some("recording"),
        "Recording",
    );
    stack.add_titled(&crate::midi_prefs::page(app), Some("midi"), "MIDI");
    stack.add_titled(&video_page(app), Some("video"), "Video");
    stack.add_titled(&engine_page(app), Some("engine"), "Engine");
    stack.add_titled(&project_page(app), Some("project"), "Project");
    if let Some(p) = page {
        stack.set_visible_child_name(p);
    }
    let sidebar = gtk::StackSidebar::new();
    sidebar.set_stack(&stack);
    sidebar.set_size_request(150, -1);
    let body = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    body.append(&sidebar);
    body.append(&gtk::Separator::new(gtk::Orientation::Vertical));
    stack.set_hexpand(true);
    body.append(&stack);
    win.set_child(Some(&body));
    win.connect_close_request(move |_| {
        alive.set(false);
        glib::Propagation::Proceed
    });
    OPEN.with(|o| *o.borrow_mut() = Some(win.downgrade()));
    win.present();
}
