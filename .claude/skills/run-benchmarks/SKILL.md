---
name: run-benchmarks
description: Run jrs's benchmark against Maven and Gradle (`cargo bench --bench compare`) the way the recorded passes were run, record the result as a new pass in specs/BENCHMARKS_SUMMARY.md with its date and time, the jrs version and commit, the machine, the OS and the tool versions, and update the website's benchmarks page (website/benchmarks/index.html, website/src/benchmarks.ts) to the new numbers. Leaves the changes uncommitted. Use when the user asks to run, rerun or refresh the benchmarks, record a new benchmark pass, or update the benchmarks page with new numbers.
argument-hint: "[optional: full | projects=a,b,c]"
allowed-tools: Bash(env JRS_BENCH_*), Bash(cargo build:*), Bash(git status:*), Bash(git log:*), Bash(git diff:*), Bash(git rev-parse:*), Bash(git show:*), Bash(date:*), Bash(sw_vers:*), Bash(system_profiler SPHardwareDataType:*), Bash(sysctl:*), Bash(pmset -g:*), Bash(java -version:*), Bash(mvn -v:*), Bash(gradle --version:*), Bash(ls:*), Bash(grep:*), Bash(python3 -I .claude/skills/run-benchmarks/tables.py:*), Bash(bun run build:*), Read, Edit, Grep, Glob, AskUserQuestion
---

# Run benchmarks

Run the comparison against Maven and Gradle as the recorded passes in
`specs/BENCHMARKS_SUMMARY.md` were run, add the result as the next pass, and
bring the website's benchmarks page up to it.

The plan, the scenarios and the fairness rules are `specs/BENCHMARKS.md`; the
harness is `benches/compare.rs`. Read the summary's latest pass before
starting: it is the template for everything this skill writes.

## Hard rules

- **Never change the harness or the projects** (`benches/`,
  `benchmarks/projects/`, `examples/`, `src/`). A failing check is a finding
  to report, not something to fix here.
- **Every number comes from this run.** Take each value from the raw samples
  through `tables.py`, never from memory or the previous pass, and never
  round differently from it. If a cell failed, say so; don't fill it in.
- **Don't publish a broken run.** If the measuring run exits non-zero, a
  project fails its checks, or any cell is `incomplete`, stop after step 3
  and report. Don't touch the summary or the website.
- **The run's outputs stay under `target/`.** The report, the raw samples, the
  logs and the mirror go to `target/bench/<stamp>/`, never to `benchmarks/`:
  BENCHMARKS.md §6 says a published `REPORT.md` is committed by hand from the
  reference machine, and none of the passes so far was such a report.
- **Don't commit and never push.** Leave the changes in the working tree; the
  user can run `/commit`.
- **Keep the voice** of the summary and the page: British spelling, plain
  declarative prose, the existing wrap width, table layout and number format.

## 1. Settings

The recorded passes used, and this skill uses by default:

- `JRS_BENCH_RUNS=1 JRS_BENCH_WARMUP=0`: one run per cell, no warm-up, so
  successive passes compare cell for cell;
- the parity set only, through `JRS_BENCH_PROJECTS`. Take its names from
  `benches/compare.rs` (the projects the `JRS_BENCH_PROJECTS` error message
  lists as the parity set) and from `specs/BENCHMARKS.md` §2, so a project
  added since the last pass (`orders` and `gen-kt` once implemented, say)
  is included;
- a mirror recorded fresh for this run.

Arguments (`$ARGUMENTS`) change that:

- `full`: drop `JRS_BENCH_RUNS` and `JRS_BENCH_WARMUP`, so the harness's
  defaults apply (7 measured, 2 warm-up). Say in the summary that this pass
  is not comparable cell for cell with the single-run passes.
- `projects=a,b,c`: run those instead. A corpus project needs a corpus run
  first (`specs/CORPUS.md`); if there is none under `target/corpus/`, stop
  and say so.

## 2. Preflight

Run these together and keep the output, which goes into the summary and the
page:

```
date '+%Y-%m-%d %H:%M %Z'
git status --short
git rev-parse --short HEAD
grep -m1 '^version' Cargo.toml
sw_vers
system_profiler SPHardwareDataType
pmset -g batt
java -version
mvn -v
gradle --version
```

- **Dirty tree.** If `git status` shows changes outside `.claude/`, ask the
  user (AskUserQuestion) whether to run anyway: the summary names a commit,
  and the run must be of that commit.
- **Tool versions.** Compare Java, Maven and Gradle with the versions the
  latest pass names. If any differs, ask the user whether to proceed with
  the new one (and name the change in the summary) or stop so they can
  switch. Gradle from SDKMAN! and Maven from Homebrew are what the passes
  used; `mvn` and `gradle` are on `PATH` for the harness, not shell aliases.
- **Power.** On battery, ask the user to plug in first; a throttled run is
  not comparable.
- Tell the user the run takes roughly 15–30 minutes and that the machine
  should stay otherwise idle.

## 3. Record, then measure

Let `<stamp>` be the date and time, `YYYY-MM-DD-HHMM`, and `<dir>` be
`target/bench/<stamp>`. Build first so compiling is not part of the run:
`cargo build --release --benches`.

Record the mirror (the only step that reaches the network), in the
background with a long timeout:

```
env JRS_BENCH_RECORD=1 JRS_BENCH_MIRROR=<dir>/mirror JRS_BENCH_REPORT=<dir>/REPORT.md \
    JRS_BENCH_PROJECTS=<names> cargo bench --bench compare
```

Its last lines say `recorded: N requests, M the mirror could not answer`.
Keep `N` for the summary. If `M` is not 0, the misses it lists must be
answered before measuring; stop and report them.

Then measure, again in the background, and note the time it finishes:

```
env JRS_BENCH_RUNS=1 JRS_BENCH_WARMUP=0 JRS_BENCH_MIRROR=<dir>/mirror \
    JRS_BENCH_REPORT=<dir>/REPORT.md JRS_BENCH_PROJECTS=<names> cargo bench --bench compare
```

It prints the report and the paths of the report, the raw samples
(`<dir>/raw/<date>-<host>.tsv`) and the logs. Exit 1 means a project failed
its checks or a cell failed: report which, with the lines from the report,
and stop.

## 4. Tables

```
python3 -I .claude/skills/run-benchmarks/tables.py <dir>/raw/<file>.tsv website/src/benchmarks.ts
```

It prints the per-project Markdown tables, the `rows` for the website, the
jrs cells that moved by 5% or more against the website's numbers (which are
the latest pass's), and any failed or incomplete sample. If that last section
is not `none`, stop as in step 3.

Read `<dir>/REPORT.md` too, for what the tables don't carry: the
"Workspace members at once" table, "Where jrs is slower", the jrs phases
(`--timings`) behind any cell you explain, and any section a newer harness
writes (a Kotlin section, say).

## 5. The summary

In `specs/BENCHMARKS_SUMMARY.md`:

1. **The opening paragraph** lists every pass, oldest first. Add this one,
   with its version, date and start time, and a link to its section.
2. **Append a section** after the last one, after a `---` rule, in the
   latest pass's shape, numbered on from it (`## Third pass: jrs 0.13.0
   (2026-11-02)`, and so on):
   - **The run**: date with start and finish time and the time zone; the
     jrs version and the commit; the machine (chip, cores, memory), macOS
     with its build, the JDK, Maven and Gradle versions, the mirror's
     request count; that every project passed BENCHMARKS.md §5.1.
   - **The settings**, with the exact `JRS_BENCH_*` values, and the caveat
     the earlier passes carry (single run, not the report of §6).
   - **What changed since the previous pass**: `git log --oneline
     <previous commit>..HEAD -- src/ benches/ Cargo.toml`, reduced to what
     can move a number. Name the commits. Don't claim a change caused a
     move the numbers don't show.
   - **jrs, previous against this pass**: the moved-cells table from
     `tables.py`. If nothing moved, say every jrs cell is within ±5%.
     Check Maven's and Gradle's cells against the previous pass too: if they
     moved as much as jrs's, the machine did, and the section says so.
   - **Workspace members at once**, from the report, if `gen-ws` ran.
   - **All cells**: the tables from `tables.py`, each under a heading with
     the project's description from the previous pass (or from
     BENCHMARKS.md §2 for a new project). A service's `run` row reads
     `run (to HTTP 200)`, as `bookmarks`'s does.
   - **Where jrs is better** and **Where jrs can improve**, rewritten from
     this pass's numbers on the previous pass's pattern: ranges and ratios
     recomputed, a point dropped when it no longer holds, a new one added
     when the numbers show it. Every figure in them must appear in the
     tables above.

Don't edit an earlier pass.

## 6. The website

The page shows the latest pass in full, and every pass's jrs and Gradle
daemon columns in the "Release by release" charts. `website/src/benchmarks.ts`
says to keep `RESULTS` and `HISTORY` in step with the summary.

**`website/src/benchmarks.ts`**: replace each project's `rows` with the
`tables.py` output. A project new to this pass gets an entry like its
neighbours (`id`, `name`, `what`); a column only some projects have
(`gradleParallel`) stays last in a row and is left out where it was not
timed. Append the new pass's jrs version to `RELEASES`, and to every
`HISTORY` trend its jrs and Gradle (daemon) milliseconds, in the same
order. A project new to this pass has no earlier cells: leave it out of
`HISTORY`, which keeps it out of the charts' tabs.
Change nothing else.

**`website/benchmarks/index.html`**, every place a number, version or date
appears:

- the `caveat` paragraph: which pass this is, its jrs version and date,
  that earlier passes are in the summary;
- the headline `stats` lists ("Where jrs is faster", "Where jrs is
  slower"): each value and its `versus` lines, for the same project and
  scenario as before. If a cell moved to the other list (jrs now faster
  where it was slower), move it and tell the user;
- the `verdicts` lists, from the summary's "better" and "improve" sections;
- "Five projects, nine scenarios each" and the `bars` lede, if the project
  count or the Gradle columns changed;
- the "Release by release" lede ("the six recorded passes, from 0.11.0 to
  0.14.2") and the note under its charts, if this pass adds a one-off to
  explain;
- the setup item of "How it was measured", and the machine `dl`, from
  `system_profiler` and `sw_vers` (computer, model identifier, chip and its
  cores, memory, macOS and its build, the JDK);
- the lede's claims ("about 60 ms", "five times faster", "up to four times
  sooner") and the `description`/`og:`/`twitter:` meta text: change them only
  if this pass's numbers no longer support them.

A chart's `data-project`/`data-scenario` must name a row that exists.

Then check it builds: `cd website && bun run build`. Remove nothing else
the build writes; `website/dist/` is what the deploy uses.

## 7. Report

Tell the user, briefly:

- when it ran, on which jrs, how long it took, and the mirror's request count;
- the cells that moved, and anything that changed side (jrs now faster or
  slower than a tool where it was not);
- what was edited (the summary's new section, the page, the data file);
- anything left for them: a tool version change they should know about, a
  claim on the page you were unsure of, a new project needing a chart.

Suggest `/commit` for the result. Don't commit.
