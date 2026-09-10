# jrs website

The project's landing page: one static HTML page, bundled with [Bun](https://bun.sh).

```
bun install
bun run dev        # dev server with hot reload on http://localhost:3000
bun run build      # static site in dist/
bun run preview    # build, then serve dist/ on http://localhost:4173
```

`dist/` is self-contained and can be served from any static host (GitHub Pages,
Netlify, a bucket).

- `index.html` is the page and the build entry point. Bun follows its links to
  `src/style.css`, `src/main.ts` and the repository's own `../logo.png`, and
  writes them to `dist/` with hashed names.
- The version in the header is imported from `../Cargo.toml` at build time, so
  the site shows whatever the crate is at.
- The terminal replay in `src/main.ts` mirrors jrs's real output: 12-column
  right-aligned verbs and the summary frame from `ui::render_summary`. Keep it
  in step when the output layer changes.
- The fonts, Martian Mono and IBM Plex Mono, come from the fontsource packages
  and are inlined into the CSS: no requests to third-party font hosts.
