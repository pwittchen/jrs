import cargo from "../../Cargo.toml";

const $ = <T extends Element>(sel: string, root: ParentNode = document) => root.querySelector<T>(sel);
const $$ = <T extends Element>(sel: string, root: ParentNode = document) => [...root.querySelectorAll<T>(sel)];

const esc = (s: string) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
const reducedMotion = matchMedia("(prefers-reduced-motion: reduce)").matches;

// The version comes from the crate itself, so the site never drifts from a release.
for (const el of $$<HTMLElement>("[data-version]")) el.textContent = `v${cargo.package.version}`;

// ---- terminal replay ------------------------------------------------------
//
// A `jrs package --fat` of examples/wordstats, as jrs prints it: the verb
// right-aligned in 12 columns, a live line that animates below the permanent
// ones, and the framed summary from `ui::render_summary`.

const SPINNER = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const TICK = 80;

const phase = (verb: string, msg: string) => `<span class="v">${esc(verb.padStart(12))}</span> ${esc(msg)}`;

function summary(rows: [string, string][]): string[] {
  const labelWidth = Math.max(...rows.map(([l]) => l.length));
  const body = rows.map(([l, v]) => `  ${l.padEnd(labelWidth)}  ${v}`);
  const inner = Math.max(14, ...body.map((b) => b.length)) + 2;
  const title = " jrs ";
  const dim = (s: string) => `<span class="d">${s}</span>`;
  return [
    dim(`  ┌─${title}${"─".repeat(inner - 1 - title.length)}┐`),
    ...body.map((b) => `  ${dim("│")}${esc(b)}${" ".repeat(inner - b.length)}${dim("│")}`),
    dim(`  └${"─".repeat(inner)}┘`),
  ];
}

function bar(pct: number, name: string, size: string): string {
  const width = 16;
  const full = Math.round((pct / 100) * width);
  const text = `${"█".repeat(full)}${"░".repeat(width - full)}`;
  return `             <span class="p">${text}</span> ${String(pct).padStart(3)}%  ${esc(name)}  <span class="d">${esc(size)}</span>`;
}

const COMMAND = "jrs package --fat";
const prompt = (typed: string, cursor: boolean) =>
  `<span class="p">$</span> ${esc(typed)}${cursor ? '<span class="cursor"></span>' : ""}`;

const DOWNLOADS: [string, number][] = [
  ["guava-33.3.1-jre.jar", 3.1],
  ["commons-lang3-3.17.0.jar", 0.7],
  ["junit-jupiter-api-5.11.4.jar", 0.2],
  ["failureaccess-1.0.2.jar", 0.1],
];

const SUMMARY = summary([
  ["build", "ok      7 classes"],
  ["deps", "16      16 downloaded"],
  ["jar", "wordstats-1.0.0.jar   3.8 MB"],
  ["time", "3.92s"],
]);

const FINAL = [
  prompt(COMMAND, false),
  phase("Resolving", "3 declared dependencies"),
  phase("Downloading", "16 artifacts"),
  phase("Compiling", "wordstats v1.0.0 (7 source files)"),
  phase("Packaging", "target/wordstats-1.0.0.jar (fat, 8 dependencies)"),
  phase("Finished", "build in 3.92s"),
  "",
  ...SUMMARY,
  "",
  prompt("", true),
];

function setupTerminal() {
  const body = $<HTMLPreElement>("[data-term]");
  const replay = $<HTMLButtonElement>("[data-replay]");
  if (!body || !replay) return;

  // Reserve the final height up front, so the page does not grow as it plays.
  body.innerHTML = FINAL.join("\n");
  body.style.minHeight = `${body.offsetHeight}px`;
  if (reducedMotion) return;

  let run = 0;

  async function play() {
    const id = ++run;
    const done: string[] = [];
    const draw = (live: string[] = []) => {
      if (id === run) body!.innerHTML = [...done, ...live].join("\n");
    };
    const alive = () => id === run;

    // The command, typed.
    for (let i = 0; i <= COMMAND.length && alive(); i++) {
      draw([prompt(COMMAND.slice(0, i), true)]);
      await sleep(i === 0 ? 500 : 45 + Math.random() * 45);
    }
    await sleep(300);
    done.push(prompt(COMMAND, false));

    const spin = async (verb: string, msg: string, ticks: number) => {
      for (let t = 0; t < ticks && alive(); t++) {
        draw([`<span class="v">${verb.padStart(12)}</span> <span class="p">${SPINNER[t % 10]}</span> ${esc(msg)}`]);
        await sleep(TICK);
      }
    };

    await spin("Resolving", "3 declared dependencies", 10);
    done.push(phase("Resolving", "3 declared dependencies"));

    // Downloads: a counter, and a bar per transfer in flight.
    let finished = 0;
    const progress = DOWNLOADS.map(() => 0);
    for (let t = 0; finished < 16 && alive(); t++) {
      progress.forEach((p, i) => (progress[i] = Math.min(100, p + 7 + ((i * 13 + t * 7) % 11))));
      finished = Math.min(16, Math.floor(t / 1.6));
      const rows = DOWNLOADS.map(([name, mb], i) =>
        bar(progress[i]!, name, `${((mb * progress[i]!) / 100).toFixed(1)}/${mb.toFixed(1)} MB`),
      );
      draw([`<span class="v">${"Downloading".padStart(12)}</span> <span class="p">${SPINNER[t % 10]}</span> ${finished}/16`, ...rows]);
      await sleep(TICK);
    }
    done.push(phase("Downloading", "16 artifacts"));

    await spin("Compiling", "7 source files", 14);
    done.push(phase("Compiling", "wordstats v1.0.0 (7 source files)"));
    draw();
    await sleep(350);

    done.push(phase("Packaging", "target/wordstats-1.0.0.jar (fat, 8 dependencies)"));
    draw();
    await sleep(450);

    done.push(phase("Finished", "build in 3.92s"), "");
    draw();
    await sleep(250);

    for (const line of SUMMARY) {
      if (!alive()) return;
      done.push(line);
      draw();
      await sleep(60);
    }
    done.push("", prompt("", true));
    draw();
  }

  replay.addEventListener("click", play);

  // Play once, the first time the terminal is on screen.
  const io = new IntersectionObserver(
    (entries) => {
      if (entries.some((e) => e.isIntersecting)) {
        io.disconnect();
        play();
      }
    },
    { threshold: 0.35 },
  );
  io.observe(body);
}

// ---- syntax colouring -----------------------------------------------------

function highlightToml(src: string): string {
  return src
    .split("\n")
    .map((line) => {
      const header = /^(\[[^\]]+\])(.*)$/.exec(line);
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

function highlightComments(src: string): string {
  return src
    .split("\n")
    .map((line) => {
      const i = line.indexOf("#");
      return i < 0 ? esc(line) : `${esc(line.slice(0, i))}<span class="t-com">${esc(line.slice(i))}</span>`;
    })
    .join("\n");
}

for (const pre of $$<HTMLPreElement>("pre[data-lang]")) {
  const src = pre.textContent ?? "";
  pre.innerHTML = pre.dataset.lang === "toml" ? highlightToml(src) : highlightComments(src);
}

// ---- install tabs ---------------------------------------------------------

function setupTabs() {
  const tabs = $$<HTMLButtonElement>('[role="tab"]');
  const select = (tab: HTMLButtonElement) => {
    for (const t of tabs) {
      const on = t === tab;
      t.setAttribute("aria-selected", String(on));
      t.tabIndex = on ? 0 : -1;
      const panel = document.getElementById(t.getAttribute("aria-controls")!);
      if (panel) panel.hidden = !on;
    }
  };
  tabs.forEach((tab, i) => {
    tab.addEventListener("click", () => select(tab));
    tab.addEventListener("keydown", (e) => {
      const step = e.key === "ArrowRight" ? 1 : e.key === "ArrowLeft" ? -1 : 0;
      if (!step) return;
      const next = tabs[(i + step + tabs.length) % tabs.length]!;
      select(next);
      next.focus();
    });
  });
  const ua = navigator.userAgent;
  const detected = /Windows/.test(ua)
    ? "tab-windows"
    : /Linux/.test(ua) && !/Android/.test(ua)
      ? "tab-linux"
      : null;
  const tab = detected && tabs.find((t) => t.id === detected);
  if (tab) select(tab);
}

function setupInstall() {
  const ua = navigator.userAgent;
  for (const panel of $$<HTMLElement>("[data-install]")) {
    const target = panel.querySelector<HTMLSelectElement>("[data-target]");
    const code = panel.querySelector<HTMLElement>("[data-install-command]");
    if (!target || !code) continue;

    if (panel.id === "panel-linux" && /aarch64|arm64/i.test(ua)) {
      target.value = "aarch64-unknown-linux-musl";
    }

    const render = () => {
      code.textContent = [
        `TARGET=${target.value}`,
        "mkdir -p ~/.local/bin",
        'curl -fsSL "https://github.com/pwittchen/jrs/releases/latest/download/jrs-$TARGET.tar.gz" | tar -xz -C ~/.local/bin jrs',
      ].join("\n");
    };
    target.addEventListener("change", render);
    render();
  }
}

function setupCopy() {
  for (const button of $$<HTMLButtonElement>(".copy")) {
    button.addEventListener("click", async () => {
      const text = button.parentElement?.querySelector("pre")?.textContent ?? "";
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

setupTerminal();
setupTabs();
setupInstall();
setupCopy();
