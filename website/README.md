# jrs website

The project's website: a landing page and a documentation page, static HTML
bundled with [Bun](https://bun.sh).

```
bun install
bun run dev        # dev server with hot reload on http://localhost:3000
bun run build      # static site in dist/
bun run preview    # build, then serve dist/ on http://localhost:4173
```

`dist/` is self-contained, with relative links, so it can be served from any
static host and from any path.

It is published to [getjrs.dev](https://getjrs.dev), a static site on the
mikr.us VPS, by `.github/workflows/website.yml`, which rsyncs `dist/` into the
site's directory there: on every
push to `master` that touches `website/` or `logo.png`, and after each release,
when `rust.yml` calls it with the version commit so the header shows the new
version. It can also be run by hand from the Actions tab.

- `index.html` is the landing page and a build entry point. Bun follows its
  links to `src/style.css`, `src/main.ts`, the contact section's `avatar.jpg`
  and the repository's own `../logo.png`, and writes them to `dist/` with
  hashed names.
- `docs/index.html` is the documentation, served at `getjrs.dev/docs/`, and the
  second entry point. Its content is plain HTML: a `<div class="group">` per
  sidebar group, a `<section id>` per page, an `<h3>` per topic.
  `src/docs.ts` derives the sidebar, the search index, the heading anchors and
  the code-block chrome from that markup, so a new section is only written,
  never registered. `src/docs.css` imports `style.css` and adds the layout;
  each page needs a stylesheet of its own, since Bun refuses two entry points
  that bundle to the same CSS file.
- `src/shared.ts` holds what both pages use: the version badge, the TOML
  colouring and the copy buttons. A `<pre data-lang="toml">` is coloured as
  TOML, `plain` not at all, anything else by its `#` comments; in the docs a
  `data-file` attribute gives the block a caption.
- The docs follow the repository's `README.md` and `specs/`. A change to
  jrs's behaviour, a flag or a manifest key belongs in them too.
- `install.sh` is the installer behind `curl -fsSL https://getjrs.dev/install.sh | sh`.
  Bun does not bundle it; the build copies it into `dist/` as is, so
  `bun run preview` serves it and `bun run dev` does not. It downloads
  `jrs-<target>.tar.gz` and `SHA256SUMS` from the GitHub release, so it breaks
  if `rust.yml` renames those assets.
- The version in the header is imported from `../Cargo.toml` at build time, so
  the site shows whatever the crate is at.
- The terminal replay in `src/main.ts` mirrors jrs's real output: 12-column
  right-aligned verbs and the summary frame from `ui::render_summary`. Keep it
  in step when the output layer changes.
- The fonts, Martian Mono and IBM Plex Mono, plus IBM Plex Sans for the docs'
  prose, come from the fontsource packages and are inlined into the CSS: no
  requests to third-party font hosts.
