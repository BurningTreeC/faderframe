// FaderFrame Remote. Talks to FaderFrame over one WebSocket: pairs with
// the code FaderFrame shows (or the one in the address a QR code opened),
// keeps the token it is given, then shows the state FaderFrame sends
// (whole, then patches) and sends what is done here. Faders and sliders
// are relative drags: a touch, the moves, a release — one undo step in
// FaderFrame. Text from the project is always set as text.
'use strict';

(() => {
  const TOKEN_KEY = 'faderframe.token';
  const SIZE_KEY = 'faderframe.lyricSize';
  const SCREEN_KEY = 'faderframe.screen';

  const store = {
    get(k) { try { return localStorage.getItem(k); } catch (e) { return null; } },
    set(k, v) { try { localStorage.setItem(k, v); } catch (e) { /* private mode */ } },
    del(k) { try { localStorage.removeItem(k); } catch (e) { /* private mode */ } },
  };

  const SCREENS = {
    full: ['transport', 'mixer', 'cues', 'launcher', 'setlist', 'lyrics'],
    performer: ['cues', 'lyrics', 'setlist'],
    viewer: ['lyrics', 'setlist', 'transport'],
  };
  const LABELS = {
    transport: 'Transport', mixer: 'Mixer', cues: 'Cue Mix', launcher: 'Launcher',
    setlist: 'Setlist', lyrics: 'Lyrics',
  };

  const S = {
    ws: null,
    retry: 0,
    joined: false,
    role: null,
    state: null,
    screen: null,
    pendingCode: null,
    cue: null,
    sends: false,
    posSec: 0,
    posAt: 0,
    meters: {},
    built: {},
    lyricShown: -2,
    wakeLock: null,
  };

  // --- small helpers ---------------------------------------------------

  const $ = (id) => document.getElementById(id);

  function h(tag, attrs, ...children) {
    const e = document.createElement(tag);
    for (const [k, v] of Object.entries(attrs || {})) {
      if (v == null || v === false) continue;
      if (k === 'class') e.className = v;
      else if (k === 'text') e.textContent = v;
      else if (k.startsWith('on')) e.addEventListener(k.slice(2), v);
      else e.setAttribute(k, v === true ? '' : String(v));
    }
    for (const c of children) if (c != null) e.append(c);
    return e;
  }

  function clamp(v, lo, hi) { return Math.min(hi, Math.max(lo, v)); }

  function setColor(el, color) {
    if (typeof color === 'string' && /^#[0-9a-f]{6}$/i.test(color)) el.style.setProperty('--c', color);
  }

  function clock(seconds) {
    if (!isFinite(seconds) || seconds < 0) seconds = 0;
    const m = Math.floor(seconds / 60);
    const s = Math.floor(seconds % 60);
    return m + ':' + String(s).padStart(2, '0');
  }

  let statusTimer = null;
  function status(text, sticky) {
    const el = $('status');
    el.textContent = text || '';
    el.classList.toggle('shown', !!text);
    clearTimeout(statusTimer);
    if (text && !sticky) statusTimer = setTimeout(() => el.classList.remove('shown'), 3000);
  }

  function deviceName() {
    const ua = navigator.userAgent || '';
    if (/iPad/.test(ua)) return 'iPad';
    if (/iPhone/.test(ua)) return 'iPhone';
    if (/Android/.test(ua)) return /Mobile/.test(ua) ? 'Android phone' : 'Android tablet';
    if (/Macintosh/.test(ua)) return 'Mac';
    if (/Windows/.test(ua)) return 'Windows';
    if (/Linux/.test(ua)) return 'Linux';
    return 'Browser';
  }

  // --- the connection --------------------------------------------------

  function send(m) {
    if (S.ws && S.ws.readyState === 1) S.ws.send(JSON.stringify(m));
  }

  function connect() {
    const url = (location.protocol === 'https:' ? 'wss://' : 'ws://') + location.host + '/ws';
    let ws;
    try {
      ws = new WebSocket(url);
    } catch (e) {
      retry();
      return;
    }
    S.ws = ws;
    ws.onopen = () => {
      S.retry = 0;
      status('');
      greet();
    };
    ws.onmessage = (ev) => {
      let m;
      try { m = JSON.parse(ev.data); } catch (e) { return; }
      receive(m);
    };
    ws.onclose = () => {
      S.ws = null;
      S.joined = false;
      if (!$('pair').hidden) return retry();
      status('Connecting to FaderFrame…', true);
      retry();
    };
    ws.onerror = () => { /* onclose follows */ };
  }

  function retry() {
    const delay = Math.min(8000, 400 * Math.pow(2, S.retry++));
    setTimeout(connect, delay);
  }

  function greet() {
    if (S.pendingCode) {
      send({ t: 'pair', code: S.pendingCode, name: $('pair-name').value || deviceName() });
      S.pendingCode = null;
      return;
    }
    const token = store.get(TOKEN_KEY);
    if (token) send({ t: 'hello', token });
    else showPair('');
  }

  function receive(m) {
    switch (m.t) {
      case 'paired':
        store.set(TOKEN_KEY, m.token);
        status('Paired as ' + m.name);
        break;
      case 'welcome':
        S.joined = true;
        S.role = m.role || { kind: 'viewer' };
        welcome();
        break;
      case 'refused':
        refused(m.why);
        break;
      case 'state':
        S.state = m.state;
        changed();
        break;
      case 'patch':
        if (S.state) {
          applyPatch(m.ops || []);
          changed();
        }
        break;
      case 'meters':
        S.meters = m.m || {};
        drawMeters();
        break;
      case 'notice':
        status(m.text);
        break;
      case 'bye':
        store.del(TOKEN_KEY);
        S.joined = false;
        showPair(m.why === 'forgotten'
          ? 'FaderFrame forgot this device. Pair it again.'
          : 'FaderFrame closed the connection.');
        break;
      default:
        break;
    }
  }

  function refused(why) {
    const say = {
      wrong_code: 'That is not the code FaderFrame shows.',
      no_code: 'FaderFrame offers no code now: ask for a new one in Preferences → Remote.',
      unknown_token: 'This device is no longer paired. Pair it again.',
      not_paired: '',
      error: 'FaderFrame could not pair this device.',
    };
    if (why === 'unknown_token') store.del(TOKEN_KEY);
    showPair(say[why] != null ? say[why] : 'Refused: ' + why);
  }

  // Patches from FaderFrame (as `faderframe_remote::diff::apply`).
  function applyPatch(ops) {
    for (const op of ops) {
      const p = op.p || [];
      if (p.length === 0) {
        if ('v' in op) S.state = op.v;
        continue;
      }
      let at = S.state;
      for (let i = 0; i < p.length - 1 && at != null; i++) at = typeof at === 'object' ? at[p[i]] : null;
      if (at == null || typeof at !== 'object') continue;
      const last = p[p.length - 1];
      if (op.d) {
        if (!Array.isArray(at)) delete at[last];
      } else if (Array.isArray(at)) {
        if (typeof last === 'number' && last < at.length) at[last] = op.v;
      } else {
        at[last] = op.v;
      }
    }
  }

  // --- pairing ---------------------------------------------------------

  function showPair(message) {
    for (const s of document.querySelectorAll('.screen')) s.hidden = true;
    $('pair').hidden = false;
    $('tabs').replaceChildren();
    document.body.classList.add('not-engineer');
    $('pair-message').textContent = message || '';
    if (!$('pair-name').value) $('pair-name').value = deviceName();
  }

  $('pair-form').addEventListener('submit', (e) => {
    e.preventDefault();
    const code = $('pair-code').value.replace(/\D/g, '');
    if (code.length !== 6) {
      $('pair-message').textContent = 'The code has six digits.';
      return;
    }
    $('pair-message').textContent = 'Pairing…';
    if (S.ws && S.ws.readyState === 1) send({ t: 'pair', code, name: $('pair-name').value || deviceName() });
    else S.pendingCode = code;
  });

  // --- roles and screens -----------------------------------------------

  function welcome() {
    $('pair').hidden = true;
    const kind = S.role.kind;
    document.body.classList.toggle('not-engineer', kind !== 'full');
    const screens = SCREENS[kind] || SCREENS.viewer;
    const tabs = $('tabs');
    tabs.replaceChildren(...screens.map((s) => h('button', {
      role: 'tab', 'data-screen': s, text: LABELS[s], onclick: () => show(s),
    })));
    const kept = store.get(SCREEN_KEY);
    show(screens.includes(S.screen) ? S.screen : screens.includes(kept) ? kept : screens[0]);
  }

  function show(screen) {
    S.screen = screen;
    store.set(SCREEN_KEY, screen);
    for (const b of $('tabs').children) b.classList.toggle('on', b.dataset.screen === screen);
    for (const s of document.querySelectorAll('.screen')) s.hidden = s.id !== 'screen-' + screen;
    send({ t: 'view', screen });
    keepAwake(screen === 'lyrics' || screen === 'setlist');
    S.lyricShown = -2;
    render();
  }

  function keepAwake(on) {
    if (!('wakeLock' in navigator)) return;
    if (on && !S.wakeLock) {
      navigator.wakeLock.request('screen').then((l) => { S.wakeLock = l; }, () => {});
    } else if (!on && S.wakeLock) {
      S.wakeLock.release().catch(() => {});
      S.wakeLock = null;
    }
  }

  function changed() {
    const t = S.state.transport || {};
    if (t.seconds !== S.lastSeconds || t.playing !== S.lastPlaying) {
      S.lastSeconds = t.seconds;
      S.lastPlaying = t.playing;
      S.posSec = t.seconds || 0;
      S.posAt = performance.now();
    }
    $('project').textContent = S.state.project || 'FaderFrame';
    $('bars').textContent = t.bars || '—';
    $('time').textContent = t.time || '';
    const play = $('play');
    play.classList.toggle('on', !!t.playing);
    play.textContent = t.playing ? '■' : '▶';
    $('rec').classList.toggle('on', !!t.recording);
    $('loop').classList.toggle('on', !!t.looping);
    if (S.joined && S.screen) render();
  }

  function render() {
    if (!S.state || !S.screen) return;
    switch (S.screen) {
      case 'transport': renderTransport(); break;
      case 'mixer': renderMixer(); break;
      case 'cues': renderCues(); break;
      case 'setlist': renderSetlist(); break;
      case 'lyrics': renderLyrics(); break;
      case 'launcher': renderLauncher(); break;
      default: break;
    }
  }

  /// Rebuild a screen when what it is made of changed (`sig`), else let
  /// it update in place.
  function rebuild(screen, sig) {
    if (S.built[screen] === sig) return false;
    S.built[screen] = sig;
    return true;
  }

  for (const b of document.querySelectorAll('#transport button')) {
    b.addEventListener('click', () => send({ t: 'transport', button: b.dataset.button }));
  }

  // --- controls ----------------------------------------------------------

  // A drag that is one gesture in FaderFrame.
  function touch(on) { send({ t: 'touch', on }); }

  function gesture(fn) {
    touch(true);
    fn();
    touch(false);
  }

  // A fader or slider: relative drags, double tap resets.
  function slider(o) {
    const el = h('div', { class: 'slider ' + (o.vertical ? 'v' : 'h') });
    setColor(el, o.color);
    const fill = h('div', { class: 'fill' });
    const cap = h('div', { class: 'cap' });
    el.append(fill);
    const span = o.max - o.min;
    if (o.unity != null) {
      const u = h('div', { class: 'unity' });
      const f = (o.unity - o.min) / span;
      if (o.vertical) u.style.bottom = (f * 100) + '%';
      else u.style.left = (f * 100) + '%';
      el.append(u);
    }
    el.append(cap);
    let value = o.min;
    let dragging = false;
    let start = 0;
    let startValue = 0;
    let lastTap = 0;
    let sentAt = 0;
    let pending = null;
    let timer = null;
    function draw(v) {
      value = v;
      const f = clamp((v - o.min) / span, 0, 1);
      const from = o.bipolar ? 0.5 : 0;
      const lo = Math.min(from, f);
      const hi = Math.max(from, f);
      if (o.vertical) {
        cap.style.top = ((1 - f) * 100) + '%';
        fill.style.bottom = (lo * 100) + '%';
        fill.style.height = ((hi - lo) * 100) + '%';
      } else {
        cap.style.left = (f * 100) + '%';
        fill.style.left = (lo * 100) + '%';
        fill.style.width = ((hi - lo) * 100) + '%';
      }
    }
    function flush() {
      timer = null;
      if (pending == null) return;
      const v = pending;
      pending = null;
      sentAt = performance.now();
      o.move(v);
    }
    function emit(v) {
      pending = v;
      if (timer) return;
      const wait = 30 - (performance.now() - sentAt);
      if (wait <= 0) flush();
      else timer = setTimeout(flush, wait);
    }
    function end() {
      if (!dragging) return;
      dragging = false;
      el.classList.remove('held');
      clearTimeout(timer);
      flush();
      touch(false);
    }
    el.addEventListener('pointerdown', (e) => {
      if (o.readOnly) return;
      e.preventDefault();
      const now = performance.now();
      if (now - lastTap < 320 && o.reset != null) {
        lastTap = 0;
        draw(o.reset);
        gesture(() => o.move(o.reset));
        return;
      }
      lastTap = now;
      try { el.setPointerCapture(e.pointerId); } catch (err) { /* old browsers */ }
      dragging = true;
      el.classList.add('held');
      start = o.vertical ? e.clientY : e.clientX;
      startValue = value;
      touch(true);
    });
    el.addEventListener('pointermove', (e) => {
      if (!dragging) return;
      const r = el.getBoundingClientRect();
      const length = Math.max(1, o.vertical ? r.height : r.width);
      const moved = o.vertical ? start - e.clientY : e.clientX - start;
      const v = clamp(startValue + (moved / length) * span * (e.shiftKey ? 0.25 : 1), o.min, o.max);
      draw(v);
      emit(v);
    });
    el.addEventListener('pointerup', end);
    el.addEventListener('pointercancel', end);
    el.addEventListener('lostpointercapture', end);
    draw(o.value != null ? o.value : o.min);
    return {
      el,
      set(v) { if (!dragging && typeof v === 'number') draw(v); },
    };
  }

  function meter(horizontal) {
    const el = h('div', { class: 'meter' + (horizontal ? ' h' : '') });
    const bars = [h('b'), h('b')];
    el.append(h('i', {}, bars[0]), h('i', {}, bars[1]));
    return {
      el,
      set(l, r) {
        [l, r].forEach((db, i) => {
          const f = clamp(((typeof db === 'number' ? db : -90) + 60) / 66, 0, 1) * 100 + '%';
          if (horizontal) bars[i].style.width = f;
          else bars[i].style.height = f;
        });
      },
    };
  }

  // Meters by strip or cue mix id, per screen.
  const meterSets = { mixer: new Map(), cues: new Map() };

  function drawMeters() {
    const meters = meterSets[S.screen];
    if (!meters) return;
    for (const [id, m] of meters) {
      const v = S.meters[id];
      if (v) m.set(v[0], v[1]);
      else m.set(-90, -90);
    }
  }

  function toggle(text, cls, on, click) {
    const b = h('button', { class: cls + (on ? ' on' : ''), text, onclick: click });
    return b;
  }

  // --- transport screen --------------------------------------------------

  function renderTransport() {
    const sec = $('screen-transport');
    const st = S.state;
    const full = S.role.kind === 'full';
    const sig = JSON.stringify([full, st.markers, st.sections, st.mix_scenes, st.macros]);
    if (rebuild('transport', sig)) {
      const big = h('div', { class: 'big', id: 't-bars' });
      const small = h('div', { class: 'small', id: 't-small' });
      const parts = [h('div', { class: 'counter' }, big, small)];
      if (full) {
        const b = (label, button, id) => h('button', {
          text: label, id, onclick: () => send({ t: 'transport', button }),
        });
        const step = (label, s) => h('button', { text: label, onclick: () => send({ t: 'step', step: s }) });
        parts.push(h('div', { class: 'grid' },
          b('⏮ Start', 'start'), b('⏪', 'rewind'), b('▶ Play', 'play', 't-play'),
          b('■ Stop', 'stop'), b('● Rec', 'record', 't-rec'), b('⏩', 'forward'),
          b('⏭ End', 'end'), b('⟳ Loop', 'loop', 't-loop'), b('Click', 'click', 't-click'),
          b('Undo', 'undo'), b('Redo', 'redo'), b('+ Marker', 'add_marker'),
          step('← Marker', 'marker_previous'), step('Marker →', 'marker_next'),
          step('← Section', 'section_previous'), step('Section →', 'section_next')));
      }
      const list = (title, items, row) => {
        parts.push(h('h2', { text: title }));
        parts.push(items.length ? h('div', { class: 'list' }, ...items.map(row)) : h('div', { class: 'empty', text: 'None' }));
      };
      list('Markers', st.markers || [], (m) => h('div', {
        class: 'row' + (full ? ' tap' : ''), onclick: full ? () => send({ t: 'locate_marker', id: m.id }) : null,
      }, h('span', { class: 'name', text: m.name || 'Marker' }), h('span', { class: 'meta', text: m.bars })));
      list('Sections', st.sections || [], (s) => {
        const r = h('div', {
          class: 'row' + (full ? ' tap' : ''), onclick: full ? () => send({ t: 'locate_section', id: s.id }) : null,
        }, h('span', { class: 'swatch' }), h('span', { class: 'name', text: s.name || 'Section' }),
        h('span', { class: 'meta', text: clock(s.start) }));
        setColor(r, s.color);
        return r;
      });
      if (full && (st.mix_scenes || []).length) {
        parts.push(h('h2', { text: 'Mix scenes' }));
        parts.push(h('div', { class: 'buttons' },
          h('button', { text: '←', onclick: () => send({ t: 'mix_scene', step: 'previous' }) }),
          ...st.mix_scenes.map((m) => h('button', {
            class: 'mix-scene' + (m.current ? ' on' : ''), 'data-id': m.id, text: m.name,
            onclick: () => send({ t: 'recall_scene', scene: m.id }),
          })),
          h('button', { text: '→', onclick: () => send({ t: 'mix_scene', step: 'next' }) })));
      }
      if (full && (st.macros || []).length) {
        parts.push(h('h2', { text: 'Macros' }));
        parts.push(h('div', { class: 'buttons' }, ...st.macros.map((m) => h('button', {
          text: m.number + ' · ' + m.name, onclick: () => send({ t: 'macro', number: m.number }),
        }))));
      }
      sec.replaceChildren(...parts);
    }
    const t = st.transport || {};
    $('t-bars').textContent = t.bars || '—';
    $('t-small').textContent = [t.time, t.bpm ? t.bpm.toFixed(t.bpm % 1 ? 2 : 0) + ' BPM' : '', t.signature]
      .filter(Boolean).join(' · ');
    const on = (id, v) => { const e = $(id); if (e) e.classList.toggle('on', !!v); };
    on('t-play', t.playing);
    on('t-rec', t.recording);
    on('t-loop', t.looping);
    on('t-click', t.click);
    for (const b of sec.querySelectorAll('.mix-scene')) {
      const m = (st.mix_scenes || []).find((x) => String(x.id) === b.dataset.id);
      b.classList.toggle('on', !!(m && m.current));
    }
  }

  // --- mixer -------------------------------------------------------------

  function strip(id, master) {
    const t = () => S.state.tracks[id] || {};
    const first = t();
    const el = h('div', { class: 'strip' + (master ? ' master' : '') });
    setColor(el, first.color);
    const label = h('div', { class: 'label', text: first.name });
    const mute = toggle('M', 'mute', false, () => send({ t: 'mute', track: id, on: !t().mute }));
    const solo = master ? null : toggle('S', 'solo', false, () => send({ t: 'solo', track: id, on: !t().solo }));
    const arm = first.armable ? toggle('R', 'arm', false, () => send({ t: 'arm', track: id, on: !t().arm })) : null;
    const pan = first.pans ? slider({
      min: -1, max: 1, bipolar: true, reset: 0, value: first.pan,
      move: (v) => send({ t: 'pan', track: id, pan: v }),
    }) : null;
    if (pan) pan.el.classList.add('pan');
    const sends = [];
    let sendBox = null;
    if (S.sends && !master) {
      sendBox = h('div', { class: 'sends' });
      for (const s of first.sends || []) {
        const sl = slider({
          min: 0, max: 1, unity: S.state.unity, reset: S.state.unity, value: s.travel,
          move: (v) => send({ t: 'send', track: id, send: s.id, travel: v }),
        });
        const name = h('span', {
          text: s.name, onclick: () => {
            const now = (t().sends || []).find((x) => x.id === s.id);
            send({ t: 'send_on', track: id, send: s.id, on: !(now && now.on) });
          },
        });
        const box = h('div', { class: 'send' }, name, sl.el);
        sendBox.append(box);
        sends.push({ id: s.id, box, sl });
      }
      if (!(first.sends || []).length) sendBox.append(h('div', { class: 'send', text: 'No sends' }));
    }
    const fader = slider({
      vertical: true, min: 0, max: 1, unity: S.state.unity, reset: S.state.unity, value: first.fader,
      move: (v) => send({ t: 'fader', track: id, travel: v }),
    });
    const m = meter(false);
    meterSets.mixer.set(String(id), m);
    const level = h('div', { class: 'level' });
    el.append(...[label, h('div', { class: 'msr' }, mute, solo, arm), pan ? pan.el : null, sendBox,
      h('div', { class: 'body' }, fader.el, m.el), level].filter((c) => c != null));
    return {
      el,
      update() {
        const v = t();
        mute.classList.toggle('on', !!v.mute);
        if (solo) solo.classList.toggle('on', !!v.solo);
        if (arm) arm.classList.toggle('on', !!v.arm);
        if (pan) pan.set(v.pan);
        fader.set(v.fader);
        level.textContent = v.level || '';
        for (const s of sends) {
          const now = (v.sends || []).find((x) => x.id === s.id);
          if (!now) continue;
          s.sl.set(now.travel);
          s.box.classList.toggle('off', !now.on);
        }
      },
    };
  }

  function renderMixer() {
    const sec = $('screen-mixer');
    const st = S.state;
    const order = (st.order || []).filter((id) => id !== st.master && st.tracks[id]);
    const shape = (id) => {
      const t = st.tracks[id] || {};
      return [id, t.name, t.color, t.pans, t.armable, (t.sends || []).map((s) => [s.id, s.name])];
    };
    const sig = JSON.stringify([order.map(shape), st.master, S.sends, st.unity]);
    if (rebuild('mixer', sig)) {
      meterSets.mixer.clear();
      S.strips = [];
      const box = h('div', { class: 'strips' });
      for (const id of order) {
        const s = strip(id, false);
        S.strips.push(s);
        box.append(s.el);
      }
      if (st.master != null && st.tracks[st.master]) {
        const s = strip(st.master, true);
        S.strips.push(s);
        box.append(s.el);
      }
      const tools = h('div', { class: 'mixer-tools' },
        toggle('Sends', '', S.sends, () => { S.sends = !S.sends; render(); }));
      sec.replaceChildren(tools, order.length || st.master != null ? box : h('div', { class: 'empty', text: 'No strips' }));
    }
    for (const s of S.strips || []) s.update();
    drawMeters();
  }

  // --- cue mixes ---------------------------------------------------------

  function renderCues() {
    const sec = $('screen-cues');
    const st = S.state;
    const cues = st.cues || [];
    if (!cues.length) {
      if (rebuild('cues', 'none')) {
        const role = st.role || {};
        sec.replaceChildren(h('div', {
          class: 'empty',
          text: role.kind === 'performer'
            ? 'Your cue mix "' + (role.cue_name || '') + '" is not in this project.'
            : 'This project has no cue mixes (make them in FaderFrame’s mixer).',
        }));
      }
      return;
    }
    if (!cues.some((c) => c.id === S.cue)) S.cue = cues[0].id;
    const cue = () => (S.state.cues || []).find((c) => c.id === S.cue) || {};
    const rows = (st.cue_order || []).filter((id) => st.tracks[id]);
    const sig = JSON.stringify([cues.map((c) => [c.id, c.name, c.color]), S.cue, st.unity,
      rows.map((id) => [id, st.tracks[id].name, st.tracks[id].color])]);
    if (rebuild('cues', sig)) {
      meterSets.cues.clear();
      const parts = [];
      if (cues.length > 1) {
        parts.push(h('div', { class: 'cue-tabs' }, ...cues.map((c) => {
          const b = toggle(c.name, '', c.id === S.cue, () => { S.cue = c.id; render(); });
          setColor(b, c.color);
          return b;
        })));
      }
      const c0 = cue();
      const master = h('div', { class: 'cue-master' });
      setColor(master, c0.color);
      const mute = toggle('Mute', 'mute', false, () => send({ t: 'cue_mute', cue: S.cue, on: !cue().mute }));
      const lvl = h('span', { class: 'meta' });
      const fader = slider({
        min: 0, max: 1, unity: st.unity, reset: st.unity, value: c0.travel, color: c0.color,
        move: (v) => send({ t: 'cue_level', cue: S.cue, travel: v }),
      });
      const m = meter(true);
      meterSets.cues.set(String(S.cue), m);
      master.append(h('div', { class: 'top' }, h('span', { class: 'name', text: c0.name }), lvl, mute), fader.el, m.el);
      parts.push(master);
      const lines = [];
      for (const id of rows) {
        const t = st.tracks[id];
        const level = h('span', { class: 'lvl' });
        const on = toggle('On', 'onoff', false, () => {
          const s = (cue().sends || {})[id] || {};
          send({ t: 'cue_on', cue: S.cue, track: id, on: !s.on });
        });
        const sl = slider({
          min: 0, max: 1, unity: st.unity, reset: st.unity, color: t.color,
          move: (v) => send({ t: 'cue_send', cue: S.cue, track: id, travel: v }),
        });
        const pan = slider({
          min: -1, max: 1, bipolar: true, reset: 0,
          move: (v) => send({ t: 'cue_pan', cue: S.cue, track: id, pan: v }),
        });
        pan.el.classList.add('pan');
        const row = h('div', { class: 'cue-row' }, h('span', { class: 'swatch' }),
          h('div', { class: 'head' }, h('span', { class: 'name', text: t.name }), level), on,
          h('div', { class: 'controls' }, sl.el, pan.el));
        setColor(row, t.color);
        parts.push(row);
        lines.push({ id, row, level, on, sl, pan });
      }
      if (!rows.length) parts.push(h('div', { class: 'empty', text: 'No tracks feed cue mixes.' }));
      sec.replaceChildren(...parts);
      S.cueView = { mute, lvl, fader, lines };
    }
    const c = cue();
    const v = S.cueView;
    v.mute.classList.toggle('on', !!c.mute);
    v.lvl.textContent = (c.level || '') + ' dB';
    v.fader.set(c.travel);
    for (const l of v.lines) {
      const s = (c.sends || {})[l.id];
      if (!s) continue;
      l.sl.set(s.travel);
      l.pan.set(s.pan);
      l.level.textContent = s.level;
      l.on.classList.toggle('on', !!s.on);
      l.row.classList.toggle('off', !s.on);
    }
    drawMeters();
  }

  // --- setlist -----------------------------------------------------------

  function renderSetlist() {
    const sec = $('screen-setlist');
    const st = S.state;
    const list = st.setlist || { songs: [] };
    const full = S.role.kind === 'full';
    const sig = JSON.stringify([full, list.songs]);
    if (rebuild('setlist', sig)) {
      const parts = [];
      if (full) {
        parts.push(h('div', { class: 'show-controls' },
          h('button', { id: 's-show', onclick: () => send({ t: 'show', control: S.state.setlist.show ? 'leave' : 'enter' }) }),
          h('button', { text: '⏮ Previous', onclick: () => send({ t: 'show', control: 'previous' }) }),
          h('button', { id: 's-play', onclick: () => send({ t: 'show', control: 'play_stop' }) }),
          h('button', { text: 'Next ⏭', onclick: () => send({ t: 'show', control: 'next' }) })));
      }
      parts.push(h('div', { class: 'countdown', id: 's-countdown', hidden: true }));
      parts.push(h('div', { class: 'notes', id: 's-notes', hidden: true }));
      if (!list.songs.length) {
        parts.push(h('div', { class: 'empty', text: 'The setlist is empty.' }));
      } else {
        parts.push(h('div', { class: 'list', id: 's-songs' }, ...list.songs.map((s, i) => h('div', {
          class: 'row song' + (full ? ' tap' : ''), 'data-i': i,
          onclick: full ? () => send({ t: 'show_song', song: i, go: false }) : null,
        }, h('span', { class: 'num', text: String(i + 1) }), h('span', { class: 'name', text: s.name }),
        h('span', { class: 'meta', text: clock(s.seconds) + (s.then ? ' · ' + s.then : '') }),
        full ? h('button', {
          class: 'go', text: '▶', 'aria-label': 'Play ' + s.name,
          onclick: (e) => { e.stopPropagation(); send({ t: 'show_song', song: i, go: true }); },
        }) : null))));
      }
      sec.replaceChildren(...parts);
    }
    const current = list.show && list.current != null ? list.current : null;
    const showBtn = $('s-show');
    if (showBtn) {
      showBtn.textContent = list.show ? 'Leave Show' : 'Show Mode';
      showBtn.classList.toggle('on', !!list.show);
    }
    const play = $('s-play');
    if (play) {
      const playing = st.transport && st.transport.playing;
      play.textContent = playing ? '■ Stop' : '▶ Play';
      play.classList.toggle('on', !!playing);
    }
    for (const r of sec.querySelectorAll('.song')) r.classList.toggle('now', Number(r.dataset.i) === current);
    const cd = $('s-countdown');
    const left = st.transport && st.transport.countdown;
    cd.hidden = left == null;
    if (left != null) cd.textContent = Math.ceil(left) + ' s';
    const notes = $('s-notes');
    const song = current != null ? list.songs[current] : null;
    notes.hidden = !(song && song.notes);
    notes.textContent = song && song.notes ? song.notes : '';
  }

  // --- lyrics (the teleprompter) ----------------------------------------

  function position() {
    const t = (S.state && S.state.transport) || {};
    return t.playing ? S.posSec + (performance.now() - S.posAt) / 1000 : S.posSec;
  }

  function lyricSize(step) {
    const now = Number(store.get(SIZE_KEY)) || 2;
    const next = clamp(now + step, 1, 5);
    store.set(SIZE_KEY, String(next));
    const p = document.querySelector('.prompter');
    if (p) p.style.setProperty('--lyric-size', next + 'em');
  }

  function renderLyrics() {
    const sec = $('screen-lyrics');
    const st = S.state;
    const lines = st.lyrics || [];
    const sig = JSON.stringify(lines);
    if (rebuild('lyrics', sig)) {
      const prompter = h('div', { class: 'prompter' });
      prompter.style.setProperty('--lyric-size', (Number(store.get(SIZE_KEY)) || 2) + 'em');
      if (!lines.length) prompter.append(h('p', { class: 'gap', text: 'No lyrics in this project.' }));
      lines.forEach((l, i) => prompter.append(h('p', { 'data-i': i, text: l.text })));
      sec.replaceChildren(
        h('div', { class: 'prompter-tools' }, h('span', { class: 'song', id: 'l-song' }),
          h('button', { text: 'A−', 'aria-label': 'Smaller', onclick: () => lyricSize(-0.25) }),
          h('button', { text: 'A+', 'aria-label': 'Larger', onclick: () => lyricSize(0.25) })),
        prompter);
      S.lyricShown = -2;
    }
    const list = st.setlist || { songs: [] };
    const song = list.show && list.current != null ? list.songs[list.current] : null;
    $('l-song').textContent = song ? song.name : (st.transport && st.transport.bars) || '';
    follow();
  }

  // Light the line being sung (a little early, to read ahead) and keep it
  // in the middle.
  function follow() {
    if (S.screen !== 'lyrics' || !S.state) return;
    const lines = S.state.lyrics || [];
    const at = position() + 0.15;
    let i = -1;
    for (let k = 0; k < lines.length; k++) {
      if (lines[k].start <= at) i = k;
      else break;
    }
    const sounding = i >= 0 && at < lines[i].end + 0.5;
    const shown = sounding ? i : i + 1;
    if (shown !== S.lyricShown) {
      S.lyricShown = shown;
      const ps = document.querySelectorAll('.prompter p[data-i]');
      for (const p of ps) {
        const k = Number(p.dataset.i);
        p.classList.toggle('now', sounding && k === i);
        p.classList.toggle('next', k === (sounding ? i + 1 : shown));
      }
      const target = ps[Math.max(0, Math.min(shown, ps.length - 1))];
      if (target) target.scrollIntoView({ block: 'center', behavior: 'smooth' });
    }
  }

  function tick() {
    follow();
    requestAnimationFrame(tick);
  }

  // --- launcher ----------------------------------------------------------

  function renderLauncher() {
    const sec = $('screen-launcher');
    const l = S.state.launcher || { scenes: [], columns: [] };
    const sig = JSON.stringify([l.scenes.map((s) => [s.id, s.name]), l.columns.map((c) => [c.track, c.name, c.color])]);
    if (rebuild('launcher', sig)) {
      const parts = [h('div', { class: 'launcher-tools' },
        h('button', { text: '■ Stop All', onclick: () => send({ t: 'stop_clips' }) }),
        h('button', { text: 'Back to Arrangement', onclick: () => send({ t: 'back_to_arrangement' }) }))];
      if (!l.columns.length || !l.scenes.length) {
        parts.push(h('div', { class: 'empty', text: 'The clip launcher is empty.' }));
      } else {
        const grid = h('div', { class: 'launch-grid' });
        grid.style.gridTemplateColumns = 'minmax(72px, 0.8fr) repeat(' + l.columns.length + ', minmax(64px, 1fr))';
        grid.append(h('span'));
        for (const c of l.columns) {
          const head = h('div', { class: 'head', text: c.name });
          setColor(head, c.color);
          grid.append(head);
        }
        l.scenes.forEach((s, row) => {
          grid.append(h('button', {
            class: 'scene', 'data-scene': row, text: '▶ ' + (s.name || 'Scene ' + (row + 1)),
            onclick: () => send({ t: 'launch_scene', scene: s.id }),
          }));
          for (const c of l.columns) {
            const press = (e) => { e.preventDefault(); send({ t: 'launch', track: c.track, scene: s.id }); };
            const release = () => send({ t: 'release', track: c.track, scene: s.id });
            grid.append(h('button', {
              class: 'slot', 'data-track': c.track, 'data-row': row,
              onpointerdown: press, onpointerup: release,
            }));
          }
        });
        grid.append(h('span'));
        for (const c of l.columns) {
          grid.append(h('button', {
            class: 'stop-track', 'data-track': c.track, text: '■',
            'aria-label': 'Stop ' + c.name, onclick: () => send({ t: 'stop_track', track: c.track }),
          }));
        }
        parts.push(grid);
      }
      sec.replaceChildren(...parts);
    }
    for (const b of sec.querySelectorAll('.slot')) {
      const c = l.columns.find((x) => String(x.track) === b.dataset.track);
      const slot = c && c.slots[Number(b.dataset.row)];
      if (!slot) continue;
      b.className = 'slot ' + slot.kind;
      b.textContent = slot.name;
      b.style.removeProperty('--c');
      if (slot.kind !== 'empty' && slot.kind !== 'armed') setColor(b, slot.color);
    }
    for (const b of sec.querySelectorAll('.scene')) {
      const s = l.scenes[Number(b.dataset.scene)];
      b.className = 'scene ' + (s ? s.light : '');
    }
    for (const b of sec.querySelectorAll('.stop-track')) {
      const c = l.columns.find((x) => String(x.track) === b.dataset.track);
      b.classList.toggle('playing', !!(c && c.playing));
    }
  }

  // --- start -------------------------------------------------------------

  const m = /(?:^#|&)pair=(\d{6})/.exec(location.hash);
  if (m) {
    S.pendingCode = m[1];
    try { history.replaceState(null, '', location.pathname); } catch (e) { /* file: */ }
  }
  $('pair-name').value = deviceName();
  document.addEventListener('visibilitychange', () => {
    if (document.visibilityState === 'visible' && (S.screen === 'lyrics' || S.screen === 'setlist')) {
      S.wakeLock = null;
      keepAwake(true);
    }
  });
  requestAnimationFrame(tick);
  connect();
})();
