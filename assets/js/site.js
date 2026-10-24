// FaderFrame's page: picks the build for the visitor's system, keeps the
// downloads and the release log on the latest release, and runs the
// workspace tabs. Without JavaScript the page links the release it was
// published with and shows every workspace in turn.
(() => {
  "use strict";

  const REPO = "BurningTreeC/faderframe";
  const API = `https://api.github.com/repos/${REPO}/releases?per_page=6`;
  const RELEASES = `https://github.com/${REPO}/releases/`;
  const CACHE_KEY = "faderframe-releases";
  const CACHE_MS = 10 * 60 * 1000;
  const LOG_ROWS = 5;

  // Each build: how its asset is named, its system, and how the hero names it.
  const BUILDS = {
    "linux-tar": { test: /-linux-x86_64\.tar\.xz$/, os: "linux", what: "Portable tarball for x86_64" },
    "linux-flatpak": { test: /-linux-x86_64\.flatpak$/, os: "linux", what: "Flatpak for x86_64" },
    "mac-arm": { test: /-macos-arm64\.dmg$/, os: "mac", what: "Disk image for Apple silicon" },
    "mac-intel": { test: /-macos-x86_64\.dmg$/, os: "mac", what: "Disk image for Intel Macs" },
    "win-setup": { test: /-windows-x64-setup\.exe$/, os: "windows", what: "Installer for 64-bit Windows" },
    "win-zip": { test: /-windows-x64-portable\.zip$/, os: "windows", what: "Portable zip for 64-bit Windows" },
  };
  const SYSTEMS = {
    linux: { name: "Linux", build: "linux-tar" },
    mac: { name: "macOS", build: "mac-arm" },
    windows: { name: "Windows", build: "win-setup" },
  };

  const $ = (sel, root = document) => root.querySelector(sel);
  const $$ = (sel, root = document) => [...root.querySelectorAll(sel)];
  const megabytes = (bytes) => `${(bytes / 1048576).toFixed(1)} MB`;
  const dateIn = (month) => (iso) =>
    new Intl.DateTimeFormat("en-GB", { day: "numeric", month, year: "numeric", timeZone: "UTC" }).format(new Date(iso));
  const longDate = dateIn("long");
  const shortDate = dateIn("short");

  // What the page was published with, read back from its own links.
  const builds = {};
  for (const a of $$("a[data-build]")) {
    builds[a.dataset.build] = { url: a.href, size: $(`[data-size="${a.dataset.build}"]`)?.textContent ?? "" };
  }

  // ---------- The visitor's system ----------

  async function detect() {
    const hints = navigator.userAgentData;
    const ua = navigator.userAgent || "";
    const platform = (hints && hints.platform) || navigator.platform || "";
    const touchMac = /Macintosh/.test(ua) && navigator.maxTouchPoints > 1;
    if ((hints && hints.mobile) || /Android|iPhone|iPad|iPod/.test(ua) || touchMac) return { os: null, build: null };
    if (/win/i.test(platform) || /Windows/.test(ua)) return { os: "windows", build: "win-setup" };
    if (/mac/i.test(platform) || /Mac OS X/.test(ua)) {
      let build = "mac-arm";
      try {
        if (hints && hints.getHighEntropyValues) {
          const values = await hints.getHighEntropyValues(["architecture"]);
          if (values.architecture === "x86") build = "mac-intel";
        }
      } catch (_) {
        // Safari and Firefox do not say; Apple silicon is the default guess.
      }
      return { os: "mac", build };
    }
    if (/linux|x11|cros/i.test(`${platform} ${ua}`)) {
      // There is no ARM build of FaderFrame yet: mark the system, offer nothing new.
      return { os: "linux", build: /aarch64|arm/i.test(platform) ? null : "linux-tar" };
    }
    return { os: null, build: null };
  }

  let system = { os: null, build: null };

  function applySystem() {
    const os = system.build ? system.os : "linux";
    const key = system.build || "linux-tar";
    const name = SYSTEMS[os].name;

    const get = $("[data-get]");
    if (get) {
      for (const a of $$("a[data-os]", get)) {
        const mine = a.dataset.os === os;
        const build = mine ? key : SYSTEMS[a.dataset.os].build;
        a.href = builds[build].url;
        a.textContent = mine ? `Download for ${name}` : SYSTEMS[a.dataset.os].name;
        a.classList.toggle("btn--primary", mine);
        a.classList.toggle("btn--ghost", !mine);
        if (mine) get.prepend(a);
      }
    }

    const meta = $("[data-get-meta]");
    if (meta) {
      const size = document.createElement("span");
      size.dataset.size = key;
      size.textContent = builds[key].size;
      meta.replaceChildren(`${BUILDS[key].what}, `, size, ".");
    }

    const close = $("[data-close-get]");
    if (close) {
      close.href = builds[key].url;
      close.textContent = `Download for ${name}`;
    }

    for (const group of $$("tbody[data-platform]")) {
      group.toggleAttribute("data-detected", group.dataset.platform === system.os);
    }
  }

  // ---------- The latest release and the log ----------

  async function releases() {
    try {
      const cached = JSON.parse(sessionStorage.getItem(CACHE_KEY) || "null");
      if (cached && Date.now() - cached.at < CACHE_MS) return cached.data;
    } catch (_) {
      // Storage may be blocked; ask GitHub.
    }
    const response = await fetch(API, { headers: { Accept: "application/vnd.github+json" } });
    if (!response.ok) throw new Error(`GitHub answered ${response.status}`);
    const data = (await response.json())
      .filter((r) => !r.draft && !r.prerelease)
      .map((r) => ({
        tag: String(r.tag_name || ""),
        url: String(r.html_url || ""),
        published: String(r.published_at || ""),
        assets: (r.assets || []).map((a) => ({ name: String(a.name), url: String(a.browser_download_url), size: Number(a.size) })),
      }))
      .filter((r) => /^v?\d+\.\d+\.\d+/.test(r.tag) && r.url.startsWith(RELEASES));
    try {
      sessionStorage.setItem(CACHE_KEY, JSON.stringify({ at: Date.now(), data }));
    } catch (_) {
      // Not cached; fine.
    }
    return data;
  }

  function applyLatest(release) {
    const version = release.tag.replace(/^v/, "");
    for (const el of $$("[data-version]")) el.textContent = version;
    if (release.published) {
      for (const el of $$("[data-released]")) {
        el.textContent = longDate(release.published);
        el.setAttribute("datetime", release.published.slice(0, 10));
      }
    }
    for (const [key, spec] of Object.entries(BUILDS)) {
      const asset = release.assets.find((a) => spec.test.test(a.name) && a.url.startsWith(RELEASES));
      if (!asset) continue;
      builds[key] = { url: asset.url, size: megabytes(asset.size) };
      for (const a of $$(`a[data-build="${key}"]`)) a.href = asset.url;
      for (const el of $$(`[data-size="${key}"]`)) el.textContent = builds[key].size;
    }
    applySystem();
  }

  // Releases newer than the page get a row that links to their changes.
  function applyLog(list) {
    const body = $("[data-log] tbody");
    if (!body) return;
    const parts = (tag) => tag.replace(/^v/, "").split(".").map((n) => parseInt(n, 10) || 0);
    const newer = (a, b) => {
      const [x, y] = [parts(a), parts(b)];
      for (let i = 0; i < 3; i++) if (x[i] !== y[i]) return x[i] > y[i];
      return false;
    };
    const top = $("tr[data-tag]", body)?.dataset.tag || "v0.0.0";
    const fresh = list.filter((r) => newer(r.tag, top)).slice(0, LOG_ROWS).reverse();
    for (const release of fresh) {
      const row = document.createElement("tr");
      row.dataset.tag = release.tag;
      const version = document.createElement("th");
      version.scope = "row";
      version.className = "figure";
      version.textContent = release.tag.replace(/^v/, "");
      const when = document.createElement("td");
      const time = document.createElement("time");
      time.dateTime = release.published.slice(0, 10);
      time.textContent = shortDate(release.published);
      when.append(time);
      const what = document.createElement("td");
      const link = document.createElement("a");
      link.className = "link";
      link.href = release.url;
      link.textContent = "The changes on GitHub";
      what.append(link);
      row.append(version, when, what);
      body.prepend(row);
    }
    for (const extra of $$("tr", body).slice(LOG_ROWS)) extra.remove();
  }

  // ---------- Workspace tabs ----------

  for (const list of $$("[data-tabs]")) {
    const tabs = $$('[role="tab"]', list);
    const panelOf = (tab) => document.getElementById(tab.getAttribute("aria-controls"));
    const select = (tab, focus) => {
      for (const t of tabs) {
        const on = t === tab;
        t.setAttribute("aria-selected", String(on));
        t.tabIndex = on ? 0 : -1;
        const panel = panelOf(t);
        if (panel) panel.hidden = !on;
      }
      if (focus) tab.focus({ preventScroll: true });
    };
    for (const tab of tabs) {
      const panel = panelOf(tab);
      if (panel) panel.tabIndex = 0;
    }
    list.addEventListener("click", (event) => {
      const tab = event.target.closest('[role="tab"]');
      if (tab) select(tab, false);
    });
    list.addEventListener("keydown", (event) => {
      const i = tabs.indexOf(document.activeElement);
      if (i < 0) return;
      const next = {
        ArrowRight: (i + 1) % tabs.length,
        ArrowLeft: (i - 1 + tabs.length) % tabs.length,
        Home: 0,
        End: tabs.length - 1,
      }[event.key];
      if (next === undefined) return;
      event.preventDefault();
      select(tabs[next], true);
    });
  }

  // ---------- Start ----------

  detect()
    .then((found) => {
      system = found;
      applySystem();
    })
    .catch(() => {})
    .then(releases)
    .then((list) => {
      if (!list.length) return;
      applyLatest(list[0]);
      applyLog(list);
    })
    .catch(() => {
      // Offline or rate-limited: the links the page was published with stay.
    });
})();
