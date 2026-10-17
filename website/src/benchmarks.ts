// The benchmarks page: the latest pass of `cargo bench --bench compare`
// (specs/BENCHMARKS_SUMMARY.md), drawn from one table of numbers so the bars
// and the tables cannot disagree. Keep RESULTS in step with that file.

import { $, $$, setupCopy, stampVersion } from "./shared";

type Tool = "jrs" | "maven" | "gradle" | "gradleCc" | "gradleNoDaemon" | "gradleParallel";

/** `short` names a bar, where the value beside it leaves less room. */
const TOOLS: { id: Tool; name: string; short: string }[] = [
  { id: "jrs", name: "jrs", short: "jrs" },
  { id: "maven", name: "Maven", short: "Maven" },
  { id: "gradle", name: "Gradle (daemon)", short: "Gradle daemon" },
  { id: "gradleCc", name: "Gradle (daemon, CC)", short: "Gradle CC" },
  { id: "gradleNoDaemon", name: "Gradle (no daemon)", short: "Gradle no daemon" },
  { id: "gradleParallel", name: "Gradle (daemon, --parallel)", short: "Gradle --parallel" },
];

/**
 * Milliseconds per tool; `null` when the scenario was not timed. Gradle
 * `--parallel` is timed on workspaces only, so other rows leave it out.
 */
type Row = [
  scenario: string,
  jrs: number | null,
  maven: number,
  gradle: number,
  gradleCc: number,
  gradleNoDaemon: number,
  gradleParallel?: number,
];

interface Project {
  id: string;
  name: string;
  what: string;
  rows: Row[];
}

const RESULTS: Project[] = [
  {
    id: "wordstats",
    name: "wordstats",
    what: "a few classes, Guava, JUnit",
    rows: [
      ["cold", 424, 1620, 11700, 12100, 11700],
      ["clean", 1010, 1300, 455, 412, 2390],
      ["noop", 63, 1020, 347, 326, 2050],
      ["incremental", 215, 1280, 389, 359, 2310],
      ["abi", 364, 1270, 389, 369, 2320],
      ["test", 360, 1320, 611, 571, 2420],
      ["test-edit", 515, 1620, 650, 671, 2640],
      ["run", 132, 1140, 433, 447, 2100],
      ["package", 89, 1620, 589, 679, 2420],
    ],
  },
  {
    id: "bookmarks",
    name: "bookmarks",
    what: "Spring Boot 4.1, MockMvc tests",
    rows: [
      ["cold", 513, 2060, 13000, 13200, 12500],
      ["clean", 1030, 1380, 569, 636, 2630],
      ["noop", 63, 1090, 448, 322, 2260],
      ["incremental", 252, 1340, 498, 385, 2570],
      ["abi", 406, 1340, 461, 444, 2640],
      ["test", 1090, 2370, 1630, 1530, 3640],
      ["test-edit", 1290, 2740, 1650, 1730, 3930],
      ["run", 756, 2020, 1000, 1070, 2880],
      ["package", 173, 1620, 512, 553, 2470],
    ],
  },
  {
    id: "gen-300",
    name: "gen-300",
    what: "300 classes, 30 dependencies",
    rows: [
      ["cold", 767, 2060, 12500, 12800, 12900],
      ["clean", 1350, 1740, 668, 620, 2920],
      ["noop", 66, 1160, 373, 338, 2140],
      ["incremental", 242, 1680, 435, 399, 2420],
      ["abi", 493, 1670, 434, 421, 2470],
      ["test", 452, 1460, 715, 624, 2530],
      ["test-edit", 687, 2140, 706, 779, 2930],
      ["run", 126, 1220, 416, 469, 2290],
      ["package", 190, 2490, 1480, 1520, 3310],
    ],
  },
  {
    id: "gen-2000",
    name: "gen-2000",
    what: "2000 classes, 60 dependencies",
    rows: [
      ["cold", 1670, 3240, 14800, 14700, 14100],
      ["clean", 2180, 2560, 1230, 1190, 3830],
      ["noop", 89, 1210, 362, 324, 2170],
      ["incremental", 349, 2700, 600, 567, 2860],
      ["abi", 810, 2690, 634, 576, 2950],
      ["test", 893, 2020, 842, 804, 2840],
      ["test-edit", 1190, 3530, 1070, 1120, 3500],
      ["run", 232, 1300, 497, 534, 2400],
      ["package", 362, 3580, 2540, 2610, 4480],
    ],
  },
  {
    id: "gen-ws",
    name: "gen-ws",
    what: "a six-member workspace, 300 classes",
    rows: [
      ["cold", 1380, 2280, 14100, 14100, 13900, 14400],
      ["clean", 2350, 1800, 768, 690, 3100, 744],
      ["noop", 86, 1160, 388, 340, 2170, 396],
      ["incremental", 207, 1440, 435, 378, 2450, 420],
      ["abi", 511, 1880, 506, 454, 2580, 485],
      ["test", 574, 2510, 1810, 668, 3750, 744],
      ["test-edit", 540, 3040, 703, 770, 2830, 639],
      ["package", 258, 3290, 1490, 1540, 3410, 1500],
    ],
  },
];

// The Maven and Gradle marks, from Simple Icons (simpleicons.org, CC0), on a
// 24-unit square. jrs's own is the logo the top bar already loads.
const MARKS: Partial<Record<Tool, string>> = {
  maven: "M4.237.001c-.312-.013-.665.072-.828.457-.158.374-.283 1.188-.34 2.276l1.223.591c-.02-.737.007-1.43.076-2.066-.026.299-.056.96.006 2.039.019.342.049.725.088 1.15.002.024.002.047.007.069a45.485 45.485 0 0 0 .309 2.412c.057.368.126.752.195 1.16l-.01.01c.014.01.015.018.014.023l.03.16c.03.162.06.328.093.494l.108.553.056.289a61.72 61.72 0 0 0 .457 2.068c.09.382.186.78.287 1.186.098.386.199.783.309 1.193.096.362.199.735.303 1.117.003.018.012.036.015.055a145.826 145.826 0 0 0 .34 1.185l.049.174c.078.261.158.533.242.805a4.2 4.2 0 0 1-.293-.135l-.19-.654c-.02-.077-.042-.148-.062-.225l-.002-.004-.004-.002c-.087-.3-.17-.607-.257-.916-.023-.087-.044-.173-.069-.263l-.314-1.178c-.1-.381-.194-.765-.29-1.154-.094-.39-.185-.78-.277-1.172-.093-.401-.181-.8-.265-1.203-.085-.396-.161-.798-.24-1.193a50.315 50.315 0 0 1-.211-1.17c-.004-.013-.006-.03-.01-.041l.004-.002c-.057-.386-.116-.77-.174-1.15a60.905 60.905 0 0 1-.154-1.204 27.447 27.447 0 0 1-.172-2.41l-1.22-.59c-.004.074-.01.15-.013.23-.012.294-.02.605-.023.93a45.3 45.3 0 0 0 .006 1.157c.009.37.025.755.045 1.148.02.336.042.675.07 1.022l.002.039.006.004c.003.023.007.05.006.076.033.368.064.739.107 1.115a34.493 34.493 0 0 0 .303 2.125c.01.064.024.131.035.195a23.418 23.418 0 0 0 .547 2.32c.07.237.14.464.21.68.063.182.13.365.194.545.155.422.327.832.512 1.232l.006.004a.318.318 0 0 0 .02.05c.225.485.475.95.755 1.395.01.013.02.033.03.047-.455-.183-1.259-.098-1.253-.097.83.288 1.557.64 2.016 1.175-.183.2-.523.352-.953.477.594.064.924-.039 1.045-.092-.31.26-.483.732-.635 1.24.35-.57.696-.949 1.033-1.094.078.258.162.524.244.788A147.532 147.532 0 0 0 5.157 24a.56.56 0 0 0 .43-.312c.13-.282.83-1.775 1.908-3.875.413 1.303.88 2.679 1.386 4.109a.494.494 0 0 0 .076-.465 103.735 103.735 0 0 1-1.308-3.945c.154-.299.316-.612.484-.932.125.04.255.094.389.155.203.186.352.491.482.84a1.515 1.515 0 0 0-.334-1.098c1.335.258 2.547.09 3.287-.81a3.97 3.97 0 0 0 .192-.258c-.325.304-.682.404-1.313.273.996-.281 1.523-.617 2.035-1.22.12-.145.244-.303.371-.48-.943.722-1.927.822-2.9.493l-.045-.018c.914.02 2.203-.474 3.092-1.189.41-.33.796-.73 1.17-1.21.28-.359.55-.76.82-1.216.234-.393.468-.824.7-1.293a2.83 2.83 0 0 1-.74.137l-.144.008c-.048.002-.093 0-.146.002.885-.198 1.5-.74 1.994-1.447-.24.117-.628.262-1.07.297-.058.006-.12.006-.182.006-.013-.002-.028 0-.047-.002.306-.078.574-.178.81-.309a3.363 3.363 0 0 0 .358-.236c.044-.037.088-.07.13-.106.099-.086.193-.18.28-.287.028-.034.056-.063.08-.098.036-.05.073-.098.104-.146a8.388 8.388 0 0 0 .51-.828c.015-.031.032-.057.046-.088.04-.084.08-.16.11-.227.042-.099.074-.179.092-.238a.515.515 0 0 1-.108.051c-.273.112-.727.187-1.086.201-.004 0-.008 0-.013.004h-.067c.72-.214 1.067-.45 1.422-.818a13.883 13.883 0 0 0 1.154-1.428c.264-.37.505-.738.692-1.072a6.5 6.5 0 0 0 .298-.592c.066-.157.122-.305.172-.45-.466.01-.986.011-1.48 0 .495.01 1.015.007 1.484-.005.5-1.485.063-2.262.063-2.262s-.526-1.212-1.4-.851c-.426.175-1.172.73-2.083 1.56l.514 1.45a17.561 17.561 0 0 1 1.703-1.602c-.257.22-.807.726-1.615 1.644-.256.29-.537.624-.844.997-.017.02-.035.038-.047.06a51.435 51.435 0 0 0-1.666 2.187c-.248.34-.498.704-.765 1.088h-.016c.002.02-.004.028-.01.032l-.101.152c-.104.155-.213.31-.318.47l-.352.534c-.061.09-.124.181-.186.277-.184.282-.367.573-.558.873a97.351 97.351 0 0 0-1.428 2.338 96.866 96.866 0 0 0-1.341 2.343c-.012.017-.02.04-.034.057a197.256 197.256 0 0 0-.668 1.223l-.097.181c-.17.318-.346.642-.52.979 0 .004-.005.008-.006.013-.026.048-.05.093-.072.141-.117.222-.218.424-.45.87a1.352 1.352 0 0 0-.233-.182l.345-.65c.047-.089.096-.177.143-.27l.04-.077.546-1.001.13-.233v-.006l-.001-.006c.169-.31.345-.62.52-.94.051-.087.102-.173.153-.265.224-.395.454-.794.684-1.197a91.685 91.685 0 0 1 2.135-3.504c.247-.386.503-.77.754-1.152.092-.138.182-.272.279-.41a72.9 72.9 0 0 1 .48-.701c.007-.012.019-.024.026-.037h.006c.26-.356.517-.713.773-1.065.278-.373.554-.735.83-1.09a31.075 31.075 0 0 1 1.777-2.075l-.515-1.446c-.06.057-.126.116-.192.178a32.37 32.37 0 0 0-.758.729c-.295.294-.597.606-.912.935a46.032 46.032 0 0 0-1.632 1.838l-.03.033.002.008c-.017.02-.033.044-.054.064-.266.323-.538.649-.801.985a39.105 39.105 0 0 0-1.445 1.95c-.043.06-.085.126-.127.186a26.458 26.458 0 0 0-1.403 2.303c-.13.247-.256.485-.37.715-.096.195-.187.395-.278.591-.21.463-.398.93-.566 1.399l.002.006a.36.36 0 0 0-.026.058c-.108.303-.203.608-.29.914-.14.174-.302.325-.483.46a3.505 3.505 0 0 0-.131-.153 5.148 5.148 0 0 0 .824-2.211 6.4 6.4 0 0 0-.016-1.488c-.046-.4-.126-.82-.238-1.274-.097-.393-.217-.81-.363-1.248-.091.185-.22.367-.379.545l-.086.094c-.029.032-.06.06-.092.094.434-.674.486-1.397.358-2.148a2.722 2.722 0 0 1-.49.85c-.033.038-.072.077-.11.116-.01.007-.019.018-.033.028.144-.24.25-.467.318-.698a1.29 1.29 0 0 0 .04-.146 2.85 2.85 0 0 0 .038-.225l.018-.146a2.11 2.11 0 0 0-.002-.354c-.003-.04-.004-.076-.01-.113-.01-.055-.016-.105-.027-.154a7.416 7.416 0 0 0-.193-.84c-.01-.028-.015-.056-.026-.084-.027-.079-.048-.149-.072-.209a2.1 2.1 0 0 0-.09-.209.455.455 0 0 1-.035.1c-.102.24-.34.57-.557.8-.003.003-.007.005-.007.01l-.04.043c.318-.58.39-.946.385-1.398a12.274 12.274 0 0 0-.16-1.615 10.68 10.68 0 0 0-.232-1.104 5.853 5.853 0 0 0-.18-.558 6.337 6.337 0 0 0-.172-.391 26.18 26.18 0 0 0 .002-.004C5.576.341 4.82.124 4.82.124s-.27-.11-.582-.123zm3.38 15.783l.032.082v.002c-.06.033-.116.067-.178.097-.012.004-.024.012-.039.018a2.41 2.41 0 0 0 .186-.2zm-.603 1.626c.13.136.25.242.354.32l.07.227a1.866 1.866 0 0 0-.246.053l-.03-.098c-.024-.084-.048-.17-.076-.257l-.021-.073zm.26.875a2.34 2.34 0 0 1 .271.01l.07.229a.778.778 0 0 1 .247-.004l-.326.627a127.643 127.643 0 0 1-.262-.862z",
  gradle: "M22.695 4.297a3.807 3.807 0 0 0-5.29-.09.368.368 0 0 0 0 .533l.46.47a.363.363 0 0 0 .474.032 2.182 2.182 0 0 1 2.86 3.291c-3.023 3.02-7.056-5.447-16.211-1.083a1.24 1.24 0 0 0-.534 1.745l1.571 2.713a1.238 1.238 0 0 0 1.681.461l.037-.02-.029.02.688-.384a16.083 16.083 0 0 0 2.193-1.635.384.384 0 0 1 .499-.016.357.357 0 0 1 .016.534 16.435 16.435 0 0 1-2.316 1.741H8.77l-.696.39a1.958 1.958 0 0 1-.963.25 1.987 1.987 0 0 1-1.726-.989L3.9 9.696C1.06 11.72-.686 15.603.26 20.522a.363.363 0 0 0 .354.296h1.675a.363.363 0 0 0 .37-.331 2.478 2.478 0 0 1 4.915 0 .36.36 0 0 0 .357.317h1.638a.363.363 0 0 0 .357-.317 2.478 2.478 0 0 1 4.914 0 .363.363 0 0 0 .358.317h1.627a.363.363 0 0 0 .363-.357c.037-2.294.656-4.93 2.42-6.25 6.108-4.57 4.502-8.486 3.088-9.9zm-6.229 6.901l-1.165-.584a.73.73 0 1 1 1.165.587z",
};
MARKS.gradleCc = MARKS.gradle;
MARKS.gradleNoDaemon = MARKS.gradle;
MARKS.gradleParallel = MARKS.gradle;

const SVG = "http://www.w3.org/2000/svg";

/** A table header: the tool's mark, then its name. */
function toolHead(tool: { id: Tool; name: string }): HTMLElement {
  const th = el("th", tool.id === "jrs" ? "jrs" : undefined);
  const head = el("span", "tool-head");
  if (tool.id === "jrs") {
    const logo = $<HTMLImageElement>(".brand img");
    if (logo) {
      const img = el("img", "tool-mark");
      img.src = logo.src;
      img.alt = "";
      head.append(img);
    }
  } else if (MARKS[tool.id]) {
    const svg = document.createElementNS(SVG, "svg");
    svg.setAttribute("viewBox", "0 0 24 24");
    svg.setAttribute("aria-hidden", "true");
    svg.classList.add("tool-mark");
    const path = document.createElementNS(SVG, "path");
    path.setAttribute("d", MARKS[tool.id]!);
    svg.append(path);
    head.append(svg);
  }
  head.append(document.createTextNode(tool.name));
  th.append(head);
  return th;
}

const SCENARIOS: Record<string, string> = {
  cold: "build, empty caches",
  clean: "build, warm caches, clean tree",
  noop: "build again, nothing changed",
  incremental: "build after a private edit",
  abi: "build after a public API change",
  test: "the whole suite",
  "test-edit": "test after an edit",
  run: "to the first output",
  package: "the fat jar",
};

function format(ms: number): string {
  if (ms < 1000) return `${ms} ms`;
  if (ms < 10000) return `${(ms / 1000).toFixed(2)} s`;
  return `${(ms / 1000).toFixed(1)} s`;
}

function ratio(theirs: number, jrs: number): string {
  const r = theirs / jrs;
  return r >= 10 ? `${r.toFixed(0)}×` : `${r.toFixed(1)}×`;
}

/** How much faster another tool was than jrs, where it was. */
function faster(theirs: number, jrs: number): string {
  const r = jrs / theirs;
  return r < 1.1 ? `${Math.round((1 - theirs / jrs) * 100)}% faster` : `${r.toFixed(1)}× faster`;
}

function el<K extends keyof HTMLElementTagNameMap>(tag: K, cls?: string, text?: string): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  if (cls) node.className = cls;
  if (text !== undefined) node.textContent = text;
  return node;
}

/** A horizontal bar per tool for one project's scenario, on a linear scale from zero. */
function renderBars(host: HTMLElement): void {
  const project = RESULTS.find((p) => p.id === host.dataset.project);
  const row = project?.rows.find((r) => r[0] === host.dataset.scenario);
  if (!project || !row) return;
  const values = row.slice(1) as (number | null)[];
  const max = Math.max(...values.map((v) => v ?? 0));
  const jrs = values[0];
  const list = el("ol", "bars");
  TOOLS.forEach((tool, i) => {
    const value = values[i];
    if (value === null || value === undefined) return;
    const item = el("li", tool.id === "jrs" ? "bar jrs" : "bar");
    const compared = jrs && tool.id !== "jrs" ? (value < jrs ? faster(value, jrs) : ratio(value, jrs)) : "";
    const versus = !compared ? "" : value < jrs! ? `, ${compared} than jrs` : `, ${compared} jrs's time`;
    item.tabIndex = 0;
    item.setAttribute("aria-label", `${tool.name}: ${format(value)}${versus}`);
    item.append(el("span", "bar-name", tool.short));
    const track = el("span", "bar-track");
    const fill = el("span", "bar-fill");
    fill.style.width = `${Math.max((value / max) * 100, 0.6)}%`;
    track.append(fill);
    const shown = el("span", "bar-value", format(value));
    if (compared) shown.append(el("span", value < jrs! ? "x ahead" : "x", compared));
    item.append(track, shown);
    item.append(el("span", "bar-tip", `${tool.name} · ${format(value)}${versus}`));
    list.append(item);
  });
  host.append(list);
}

/** One table per project: every scenario, every tool, the ratio to jrs. */
function renderTables(host: HTMLElement): void {
  for (const project of RESULTS) {
    const figure = el("figure", "results");
    const caption = el("figcaption");
    caption.append(el("strong", undefined, project.name), document.createTextNode(` · ${project.what}`));
    figure.append(caption);
    const scroll = el("div", "table-scroll");
    const table = el("table");
    const head = el("tr");
    head.append(el("th", "scenario", "Scenario"));
    // The tools this project was timed with: a column only some rows have
    // is still a column for all of them.
    const tools = TOOLS.slice(0, Math.max(...project.rows.map((r) => r.length - 1)));
    for (const tool of tools) head.append(toolHead(tool));
    const thead = el("thead");
    thead.append(head);
    table.append(thead);
    const body = el("tbody");
    for (const [scenario, ...values] of project.rows) {
      const tr = el("tr");
      const th = el("th", "scenario");
      th.append(el("code", undefined, scenario), el("span", "what", SCENARIOS[scenario] ?? ""));
      tr.append(th);
      const jrs = values[0];
      const best = Math.min(...values.filter((v): v is number => v != null));
      tools.forEach((_, i) => {
        const value = values[i];
        const td = el("td", i === 0 ? "jrs" : undefined);
        if (value === null || value === undefined) {
          td.textContent = "—";
        } else if (i === 0 || jrs === null) {
          td.textContent = format(value);
        } else {
          td.append(document.createTextNode(format(value)), el("span", "x", value < jrs ? faster(value, jrs) : ratio(value, jrs)));
          if (value < jrs) td.classList.add("ahead");
        }
        // The fastest tool in the row, jrs or not: in colour on the page,
        // and named for a screen reader, which cannot see it.
        if (value === best) {
          td.classList.add("best");
          td.append(el("span", "visually-hidden", " (fastest)"));
        }
        tr.append(td);
      });
      body.append(tr);
    }
    table.append(body);
    scroll.append(table);
    figure.append(scroll);
    host.append(figure);
  }
}

stampVersion();
setupCopy();
for (const host of $$<HTMLElement>("[data-bars]")) renderBars(host);
const tables = $<HTMLElement>("[data-tables]");
if (tables) renderTables(tables);
