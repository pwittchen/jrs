// The docs page. Its content is plain HTML in docs/index.html: a group per
// `<div class="group">`, a page per `<section id>`, its topics as `<h3>`s. The
// sidebar, the search index, the anchors and the code-block chrome are all
// derived from that markup here, so adding a section is only writing it.

import { $, $$, esc, highlight, setupCopy, stampVersion } from "./shared";

stampVersion();

const article = $<HTMLElement>(".doc-body")!;
const toc = $<HTMLElement>("[data-toc]")!;
const results = $<HTMLElement>("[data-results]")!;
const input = $<HTMLInputElement>("[data-search]")!;
const reducedMotion = matchMedia("(prefers-reduced-motion: reduce)").matches;

const slug = (s: string) =>
  s
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-|-$/g, "");
const label = (h: HTMLElement) => h.dataset.nav ?? h.textContent?.trim() ?? "";

// ---- structure ------------------------------------------------------------

interface Page {
  group: string;
  section: HTMLElement;
  h2: HTMLElement;
  topics: HTMLElement[];
}

const pages: Page[] = [];
const taken = new Set($$("[id]").map((e) => e.id));

for (const group of $$<HTMLElement>(":scope > .group", article)) {
  for (const section of $$<HTMLElement>(":scope > section[id]", group)) {
    const h2 = $<HTMLElement>(":scope > h2", section)!;
    const topics = $$<HTMLElement>(":scope > h3", section);
    // Pin every label now, before the anchors add their `#` to the text.
    for (const h of [h2, ...topics]) h.dataset.nav = label(h);
    // An h3 without an id gets one from its page's and its own name.
    for (const h of topics) {
      if (h.id) continue;
      const base = `${section.id}-${slug(label(h))}`;
      let id = base;
      for (let n = 2; taken.has(id); n++) id = `${base}-${n}`;
      taken.add(id);
      h.id = id;
    }
    pages.push({ group: group.dataset.title ?? "", section, h2, topics });
  }
}

const idOf = (h: HTMLElement) => h.id || h.closest("section")!.id;

// ---- search index ---------------------------------------------------------
//
// One entry per page introduction (the text before its first h3) and one per
// topic. Built before any decoration, so copy buttons and anchors stay out.

interface Entry {
  id: string;
  title: string;
  crumb: string;
  text: string;
  lower: string;
  els: Element[];
  order: number;
}

const entries: Entry[] = [];

for (const page of pages) {
  const make = (heading: HTMLElement, crumb: string): Entry => {
    const e = { id: idOf(heading), title: label(heading), crumb, text: "", lower: "", els: [heading], order: entries.length };
    entries.push(e);
    return e;
  };
  let current = make(page.h2, page.group);
  for (const el of page.section.children) {
    if (el === page.h2) continue;
    if (el.tagName === "H3") current = make(el as HTMLElement, label(page.h2));
    else current.els.push(el);
  }
}

// A table's cells sit side by side in the markup, so `textContent` would run
// them together ("FlagEffect"); join them with a space instead.
const textOf = (el: Element) =>
  el.tagName === "TABLE"
    ? $$("th, td", el)
        .map((cell) => cell.textContent ?? "")
        .join(" ")
    : (el.textContent ?? "");

for (const e of entries) {
  e.text = e.els
    .slice(1)
    .map(textOf)
    .join(" ")
    .replace(/\s+/g, " ")
    .trim();
  e.lower = e.text.toLowerCase();
}

// ---- decoration -----------------------------------------------------------

for (const pre of $$<HTMLPreElement>("pre", article)) {
  highlight(pre);
  const copy = document.createElement("button");
  copy.className = "copy";
  copy.type = "button";
  copy.textContent = "Copy";
  const file = pre.dataset.file;
  const wrap = document.createElement(file ? "figure" : "div");
  wrap.className = file ? "code" : "shell";
  pre.before(wrap);
  if (file) {
    const caption = document.createElement("figcaption");
    const name = document.createElement("span");
    name.textContent = file;
    caption.append(name, copy);
    wrap.append(caption, pre);
  } else {
    wrap.append(pre, copy);
  }
}
setupCopy(article);

for (const table of $$<HTMLTableElement>("table", article)) {
  const wrap = document.createElement("div");
  wrap.className = "table-wrap";
  table.before(wrap);
  wrap.append(table);
}

for (const page of pages) {
  const eyebrow = document.createElement("p");
  eyebrow.className = "eyebrow";
  eyebrow.textContent = page.group;
  page.h2.before(eyebrow);
  for (const h of [page.h2, ...page.topics]) {
    const a = document.createElement("a");
    a.className = "anchor";
    a.href = `#${idOf(h)}`;
    a.textContent = "#";
    a.setAttribute("aria-label", `Link to ${label(h)}`);
    h.append(a);
  }
}

// ---- sidebar --------------------------------------------------------------

const links = new Map<string, HTMLAnchorElement>();
const pageOf = new Map<string, string>();

function link(id: string, text: string): HTMLAnchorElement {
  const a = document.createElement("a");
  a.href = `#${id}`;
  a.textContent = text;
  links.set(id, a);
  return a;
}

{
  const frag = document.createDocumentFragment();
  let list: HTMLUListElement | null = null;
  let group = "";
  for (const page of pages) {
    if (!list || page.group !== group) {
      group = page.group;
      const title = document.createElement("p");
      title.className = "toc-group";
      title.textContent = group;
      list = document.createElement("ul");
      frag.append(title, list);
    }
    const li = document.createElement("li");
    li.append(link(page.section.id, label(page.h2)));
    pageOf.set(page.section.id, page.section.id);
    if (page.topics.length) {
      const sub = document.createElement("ul");
      sub.className = "sub";
      for (const h of page.topics) {
        const item = document.createElement("li");
        item.append(link(h.id, label(h)));
        sub.append(item);
        pageOf.set(h.id, page.section.id);
      }
      li.append(sub);
    }
    list.append(li);
  }
  toc.append(frag);
}

// ---- where the reader is --------------------------------------------------

const headings = pages.flatMap((p) => [p.h2, ...p.topics]);
let active = "";

function setActive(id: string) {
  if (id === active) return;
  links.get(active)?.classList.remove("active");
  const previousPage = pageOf.get(active);
  const page = pageOf.get(id);
  if (previousPage !== page) links.get(previousPage ?? "")?.parentElement?.classList.remove("open");
  active = id;
  const a = links.get(id);
  if (!a) return;
  a.classList.add("active");
  links.get(page ?? "")?.parentElement?.classList.add("open");
  // Keep the active entry in view, without touching the window's scroll.
  const top = a.offsetTop;
  if (top < toc.scrollTop + 40 || top > toc.scrollTop + toc.clientHeight - 60) {
    toc.scrollTop = top - toc.clientHeight / 3;
  }
}

function spy() {
  const doc = document.documentElement;
  if (innerHeight + scrollY >= doc.scrollHeight - 4) {
    setActive(idOf(headings[headings.length - 1]!));
    return;
  }
  let current = headings[0]!;
  for (const h of headings) {
    if (h.getBoundingClientRect().top <= 120) current = h;
    else break;
  }
  setActive(idOf(current));
}

let ticking = false;
addEventListener(
  "scroll",
  () => {
    if (ticking) return;
    ticking = true;
    requestAnimationFrame(() => {
      ticking = false;
      spy();
    });
  },
  { passive: true },
);

// ---- drawer (narrow screens) ----------------------------------------------

const menu = $<HTMLButtonElement>("[data-menu]")!;
const scrim = $<HTMLElement>("[data-scrim]")!;
const narrow = matchMedia("(max-width: 52rem)");

function setDrawer(open: boolean) {
  document.body.classList.toggle("nav-open", open);
  menu.setAttribute("aria-expanded", String(open));
}

menu.addEventListener("click", () => setDrawer(!document.body.classList.contains("nav-open")));
scrim.addEventListener("click", () => setDrawer(false));
toc.addEventListener("click", (e) => {
  if ((e.target as Element).closest("a")) setDrawer(false);
});
addEventListener("keydown", (e) => {
  if (e.key === "Escape" && document.body.classList.contains("nav-open") && document.activeElement !== input) {
    setDrawer(false);
  }
});

// ---- search ---------------------------------------------------------------

const LIMIT = 50;
let hits: Entry[] = [];
let tokens: string[] = [];
let selected = 0;

const escRe = (s: string) => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
const pattern = (ts: string[]) => new RegExp(ts.map(escRe).join("|"), "gi");

function count(hay: string, needle: string): number {
  let n = 0;
  for (let i = hay.indexOf(needle); i !== -1; i = hay.indexOf(needle, i + needle.length)) n++;
  return n;
}

function markText(s: string): string {
  let out = "";
  let last = 0;
  for (const m of s.matchAll(pattern(tokens))) {
    out += `${esc(s.slice(last, m.index))}<mark>${esc(m[0])}</mark>`;
    last = m.index! + m[0].length;
  }
  return out + esc(s.slice(last));
}

function snippet(e: Entry, query: string): string {
  let at = e.lower.indexOf(query);
  if (at < 0) {
    for (const t of tokens) {
      const i = e.lower.indexOf(t);
      if (i >= 0 && (at < 0 || i < at)) at = i;
    }
  }
  if (at < 0) return esc(e.text.slice(0, 130)) + (e.text.length > 130 ? "…" : "");
  const start = Math.max(0, at - 45);
  const end = Math.min(e.text.length, at + 115);
  return (start > 0 ? "…" : "") + markText(e.text.slice(start, end)) + (end < e.text.length ? "…" : "");
}

function search(raw: string) {
  clearMarks();
  const query = raw.trim().toLowerCase().replace(/\s+/g, " ");
  if (!query) {
    hits = [];
    results.hidden = true;
    toc.hidden = false;
    return;
  }
  tokens = [...new Set(query.split(" "))];
  const scored: { e: Entry; score: number }[] = [];
  for (const e of entries) {
    const title = e.title.toLowerCase();
    const hay = `${title} ${e.crumb.toLowerCase()} ${e.lower}`;
    if (!tokens.every((t) => hay.includes(t))) continue;
    let score = 0;
    if (title === query) score += 100;
    if (title.includes(query)) score += 60;
    for (const t of tokens) if (title.includes(t)) score += 12;
    if (e.lower.includes(query)) score += 10;
    score += Math.min(count(e.lower, tokens[0]!), 8);
    scored.push({ e, score });
  }
  scored.sort((a, b) => b.score - a.score || a.e.order - b.e.order);
  hits = scored.slice(0, LIMIT).map((s) => s.e);
  selected = 0;

  toc.hidden = true;
  results.hidden = false;
  results.scrollTop = 0;
  if (!hits.length) {
    results.innerHTML = `<p class="results-empty">No results for “${esc(raw.trim())}”.</p>`;
    return;
  }
  const n = scored.length > LIMIT ? `${LIMIT}+` : String(hits.length);
  results.innerHTML =
    `<p class="results-count">${n} result${hits.length === 1 ? "" : "s"} · ↑↓ to move, ↵ to open</p>` +
    hits
      .map(
        (e, i) =>
          `<a href="#${e.id}" data-i="${i}" role="option" aria-selected="${i === selected}">` +
          `<span class="crumb">${esc(e.crumb)}</span>` +
          `<span class="title">${markText(e.title)}</span>` +
          `<span class="snip">${snippet(e, query)}</span></a>`,
      )
      .join("");
}

function select(i: number) {
  selected = i;
  for (const a of $$<HTMLAnchorElement>("a[data-i]", results)) {
    const on = Number(a.dataset.i) === i;
    a.setAttribute("aria-selected", String(on));
    if (on) {
      const top = a.offsetTop;
      if (top < results.scrollTop || top + a.offsetHeight > results.scrollTop + results.clientHeight) {
        results.scrollTop = top - results.clientHeight / 3;
      }
    }
  }
}

// Marks every occurrence of the query in the opened topic, until the next search.
function markEntry(e: Entry) {
  const re = pattern(tokens);
  for (const el of e.els) {
    const walker = document.createTreeWalker(el, NodeFilter.SHOW_TEXT, {
      acceptNode: (n) =>
        n.parentElement?.closest("button, .anchor") ? NodeFilter.FILTER_REJECT : NodeFilter.FILTER_ACCEPT,
    });
    const nodes: Text[] = [];
    while (walker.nextNode()) nodes.push(walker.currentNode as Text);
    for (const node of nodes) {
      const text = node.data;
      const found = [...text.matchAll(re)];
      if (!found.length) continue;
      const frag = document.createDocumentFragment();
      let last = 0;
      for (const m of found) {
        frag.append(text.slice(last, m.index));
        const mark = document.createElement("mark");
        mark.className = "hit";
        mark.textContent = m[0];
        frag.append(mark);
        last = m.index! + m[0].length;
      }
      frag.append(text.slice(last));
      node.replaceWith(frag);
    }
  }
}

function clearMarks() {
  const parents = new Set<Node>();
  for (const m of $$("mark.hit", article)) {
    parents.add(m.parentNode!);
    m.replaceWith(m.textContent ?? "");
  }
  for (const p of parents) p.normalize();
}

function open(e: Entry) {
  clearMarks();
  markEntry(e);
  history.pushState(null, "", `#${e.id}`);
  document.getElementById(e.id)?.scrollIntoView({ behavior: reducedMotion ? "auto" : "smooth", block: "start" });
  setDrawer(false);
}

input.addEventListener("input", () => search(input.value));

input.addEventListener("keydown", (e) => {
  if (e.key === "ArrowDown" || e.key === "ArrowUp") {
    if (!hits.length) return;
    e.preventDefault();
    select((selected + (e.key === "ArrowDown" ? 1 : -1) + hits.length) % hits.length);
  } else if (e.key === "Enter") {
    const hit = hits[selected];
    if (hit) {
      e.preventDefault();
      open(hit);
    }
  } else if (e.key === "Escape") {
    if (input.value) {
      input.value = "";
      search("");
    } else {
      input.blur();
    }
  }
});

results.addEventListener("click", (e) => {
  const a = (e.target as Element).closest<HTMLAnchorElement>("a[data-i]");
  const hit = a && hits[Number(a.dataset.i)];
  if (!hit) return;
  e.preventDefault();
  select(Number(a.dataset.i));
  open(hit);
});

// `/` or Ctrl/Cmd+K focuses the search from anywhere.
addEventListener("keydown", (e) => {
  const typing = e.target instanceof HTMLInputElement || e.target instanceof HTMLTextAreaElement;
  if ((e.key === "/" && !typing) || (e.key.toLowerCase() === "k" && (e.metaKey || e.ctrlKey))) {
    e.preventDefault();
    if (narrow.matches) setDrawer(true);
    input.focus();
    input.select();
  }
});

// ---- start ----------------------------------------------------------------

// The h3 ids did not exist when the browser looked for the fragment.
if (location.hash) {
  document.getElementById(decodeURIComponent(location.hash.slice(1)))?.scrollIntoView();
}

// docs/?q=… opens with a search already made, for links that point at one.
const q = new URLSearchParams(location.search).get("q");
if (q) {
  input.value = q;
  search(q);
}

spy();
