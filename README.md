# FaderFrame's website

This branch is the GitHub Pages site for FaderFrame, published from the
branch root. It is plain HTML, CSS and a small script: no build step.

- `index.html`: the page.
- `assets/css/site.css`: the page's styles. `tokens.css` and `base.css` were
  written by Hallmark's scaffold (Kiln theme, the accent taken from the
  app icon's amber fader cap).
- `assets/js/site.js`: picks the build for the visitor's system, and asks the
  GitHub releases API for the latest release. It updates the version, the
  date, every download link and size, and adds newer releases to the log.
  Without the script, or when the API does not answer, the page links the
  release it was published with (0.30.0).
- `assets/img`: WebP copies of `docs/screenshots` on `main`, plus crops of
  `mixing.png` for the hero (`console-*`) and the strips close up (`strips-*`).
- `assets/fonts`: Boska (Indian Type Foundry, ITF Free Font License) and Geist
  (SIL Open Font License), served from here so the page makes no requests to
  font services.

Preview: `python3 -m http.server` in this folder, then open
http://127.0.0.1:8000/. Opening `index.html` as a file works too, but Chrome
then blocks the fonts.

When a release changes what the page shows, update the fallback values in
`index.html` (version, date, sizes, links) and give the new release a line in
the "Is it finished?" table: the script can add a row for a newer release but
not say what changed in it.
