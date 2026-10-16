"""Turn one run's raw samples into the tables the summary and the website use.

    python3 -I .claude/skills/run-benchmarks/tables.py <raw.tsv> [website/src/benchmarks.ts]

<raw.tsv> is what `cargo bench --bench compare` writes beside its report
(`project scenario tool run ms exit misses`). The optional second argument is
the website's data file as it stands, i.e. the previous pass: its jrs cells
are compared with this run's.

Prints four sections: the Markdown tables for specs/BENCHMARKS_SUMMARY.md,
the `rows` for RESULTS in website/src/benchmarks.ts, the jrs cells that moved
by 5% or more, and every sample that failed or missed the mirror. Every value
is rounded to what the summary shows, so the page, its bars and the summary
hold the same numbers.
"""

import csv
import re
import statistics
import sys

SCENARIOS = ["cold", "clean", "noop", "incremental", "abi", "test", "test-edit", "run", "package"]
COLUMNS = [
    ("jrs", "jrs"),
    ("maven", "Maven"),
    ("gradle-daemon", "Gradle"),
    ("gradle-daemon-cc", "Gradle CC"),
    ("gradle-no-daemon", "Gradle no daemon"),
    ("gradle-daemon-parallel", "Gradle --parallel"),
]


def rounded(ms):
    """The value the summary prints, back in milliseconds."""
    if ms < 1000:
        return int(round(ms))
    if ms < 10000:
        return int(round(ms / 10)) * 10
    return int(round(ms / 100)) * 100


def shown(ms):
    if ms < 1000:
        return f"{ms} ms"
    if ms < 10000:
        return f"{ms / 1000:.2f} s"
    return f"{ms / 1000:.1f} s"


def load(path):
    samples, problems, order = {}, [], []
    with open(path, newline="") as f:
        for row in csv.DictReader(f, delimiter="\t"):
            project = row["project"]
            if project not in order:
                order.append(project)
            if row["exit"] != "0" or row["mirror misses"] not in ("", "0"):
                problems.append(row)
                continue
            key = (project, row["scenario"], row["tool"])
            samples.setdefault(key, []).append(int(row["ms"]))
    cells = {k: rounded(statistics.median(v)) for k, v in samples.items()}
    return order, cells, problems


def previous_jrs(path):
    text = open(path).read()
    found = {}
    for block in re.finditer(r'id:\s*"([^"]+)".*?rows:\s*\[(.*?)\n\s*\],', text, re.S):
        for row in re.finditer(r'\["([^"]+)",\s*(null|\d+)', block.group(2)):
            if row.group(2) != "null":
                found[(block.group(1), row.group(1))] = int(row.group(2))
    return found


def main():
    order, cells, problems = load(sys.argv[1])
    previous = previous_jrs(sys.argv[2]) if len(sys.argv) > 2 else {}

    print("## Markdown (specs/BENCHMARKS_SUMMARY.md)\n")
    for project in order:
        columns = [c for c in COLUMNS if any((project, s, c[0]) in cells for s in SCENARIOS)]
        print(f"### {project}\n")
        print("| Scenario | " + " | ".join(name for _, name in columns) + " |")
        print("| --- |" + " --- |" * len(columns))
        for scenario in SCENARIOS:
            jrs = cells.get((project, scenario, "jrs"))
            if jrs is None and not any((project, scenario, c) in cells for c, _ in columns):
                print(f"| {scenario} | not timed (BENCHMARKS.md §11) |" + " |" * (len(columns) - 1))
                continue
            out = []
            for tool, _ in columns:
                value = cells.get((project, scenario, tool))
                if value is None:
                    out.append("—")
                elif tool != "jrs" and jrs is not None and value < jrs:
                    out.append(f"**{shown(value)}**")
                else:
                    out.append(shown(value))
            print(f"| {scenario} | " + " | ".join(out) + " |")
        print()

    print("## RESULTS rows (website/src/benchmarks.ts)\n")
    for project in order:
        columns = [c for c, _ in COLUMNS if any((project, s, c) in cells for s in SCENARIOS)]
        print(f"// {project}")
        for scenario in SCENARIOS:
            values = [cells.get((project, scenario, c)) for c in columns]
            if all(v is None for v in values):
                continue
            rendered = ", ".join("null" if v is None else str(v) for v in values)
            print(f'      ["{scenario}", {rendered}],')
        print()

    print("## jrs cells that moved by 5% or more\n")
    print("| Project | Scenario | Previous | This run | Change |")
    print("| --- | --- | --- | --- | --- |")
    for project in order:
        for scenario in SCENARIOS:
            now = cells.get((project, scenario, "jrs"))
            then = previous.get((project, scenario))
            if now is None or then is None:
                continue
            change = (now - then) / then
            if abs(change) >= 0.05:
                sign = "+" if change > 0 else "−"
                print(f"| `{project}` | {scenario} | {shown(then)} | {shown(now)} | {sign}{abs(change):.0%} |")
    new = [p for p in order if not any(k[0] == p for k in previous)]
    if previous and new:
        print(f"\nNot in the previous pass: {', '.join(new)}")

    print("\n## Failed or incomplete samples\n")
    if not problems:
        print("none")
    for row in problems:
        print(
            f"{row['project']} {row['scenario']} {row['tool']} run {row['run']}: "
            f"exit {row['exit']}, mirror misses {row['mirror misses']}"
        )


if __name__ == "__main__":
    main()
