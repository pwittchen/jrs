// What the landing page and the docs page have in common: the version badge,
// the code colouring and the copy buttons.

import cargo from "../../Cargo.toml";

export const $ = <T extends Element>(sel: string, root: ParentNode = document) => root.querySelector<T>(sel);
export const $$ = <T extends Element>(sel: string, root: ParentNode = document) => [...root.querySelectorAll<T>(sel)];

export const esc = (s: string) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");

// The version comes from the crate itself, so the site never drifts from a release.
export function stampVersion() {
  for (const el of $$<HTMLElement>("[data-version]")) el.textContent = `v${cargo.package.version}`;
}

// ---- syntax colouring -----------------------------------------------------

export function highlightToml(src: string): string {
  return src
    .split("\n")
    .map((line) => {
      const header = /^(\[\[?[^\]]+\]\]?)(.*)$/.exec(line);
      if (header) return `<span class="t-table">${esc(header[1]!)}</span>${rest(header[2]!)}`;
      return rest(line);
    })
    .join("\n");

  function rest(s: string): string {
    const token = /("(?:[^"\\]|\\.)*")|(#.*$)|(\b\d+(?:\.\d+)*\b)|([A-Za-z][\w-]*(?=\s*=))/g;
    let out = "";
    let last = 0;
    for (const m of s.matchAll(token)) {
      out += esc(s.slice(last, m.index));
      const [text, str, com, num] = m;
      const cls = str ? "t-str" : com ? "t-com" : num ? "t-num" : "t-key";
      out += `<span class="${cls}">${esc(text)}</span>`;
      last = m.index! + text.length;
    }
    return out + esc(s.slice(last));
  }
}

export function highlightComments(src: string): string {
  return src
    .split("\n")
    .map((line) => {
      const i = line.indexOf("#");
      return i < 0 ? esc(line) : `${esc(line.slice(0, i))}<span class="t-com">${esc(line.slice(i))}</span>`;
    })
    .join("\n");
}

/** Colours a `<pre data-lang>`: TOML fully, `plain` not at all, anything else by its `#` comments. */
export function highlight(pre: HTMLPreElement) {
  const src = pre.textContent ?? "";
  const lang = pre.dataset.lang;
  pre.innerHTML = lang === "toml" ? highlightToml(src) : lang === "plain" ? esc(src) : highlightComments(src);
}

// ---- copy buttons ---------------------------------------------------------

export function setupCopy(root: ParentNode = document) {
  for (const button of $$<HTMLButtonElement>(".copy", root)) {
    button.addEventListener("click", async () => {
      const text = button.closest(".shell, .code")?.querySelector("pre")?.textContent ?? "";
      try {
        await navigator.clipboard.writeText(text.trim());
        button.textContent = "Copied";
      } catch {
        button.textContent = "Select and copy";
      }
      setTimeout(() => (button.textContent = "Copy"), 1600);
    });
  }
}
