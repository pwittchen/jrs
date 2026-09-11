# jrs website

The project's landing page: one static HTML page, bundled with [Bun](https://bun.sh).

```
bun install
bun run dev        # dev server with hot reload on http://localhost:3000
bun run build      # static site in dist/
bun run preview    # build, then serve dist/ on http://localhost:4173
```

`dist/` is self-contained, with relative links, so it can be served from any
static host and from any path.

It is published to GitHub Pages by `.github/workflows/website.yml`: on every
push to `master` that touches `website/` or `logo.png`, and after each release,
when `rust.yml` calls it with the version commit so the header shows the new
version. It can also be run by hand from the Actions tab.

- `index.html` is the page and the build entry point. Bun follows its links to
  `src/style.css`, `src/main.ts`, the contact section's `avatar.jpg` and the
  repository's own `../logo.png`, and writes them to `dist/` with hashed names.
- The version in the header is imported from `../Cargo.toml` at build time, so
  the site shows whatever the crate is at.
- The terminal replay in `src/main.ts` mirrors jrs's real output: 12-column
  right-aligned verbs and the summary frame from `ui::render_summary`. Keep it
  in step when the output layer changes.
- The fonts, Martian Mono and IBM Plex Mono, come from the fontsource packages
  and are inlined into the CSS: no requests to third-party font hosts.
