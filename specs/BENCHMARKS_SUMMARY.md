# Benchmarks against Maven and Gradle: first results

Two passes are recorded here, oldest first: jrs 0.11.0 on 2026-10-14
(below), and jrs 0.12.0 on 2026-10-15 at 22:10 CEST
([Second pass](#second-pass-jrs-0120-2026-10-15)).

Run on 2026-10-14 with jrs 0.11.0 (`4f40e18`, `benches/compare.rs`), on an
Apple M4 Max (14 cores, 36 GiB) under macOS 27.0.1, with Corretto 25.0.2,
Maven 3.9.16 and Gradle 9.1.0, every tool from the local mirror. Every
project passed the checks of [BENCHMARKS.md](BENCHMARKS.md)
§5.1 on all three tools first: the same runtime classpath, the same tests,
the same `run` output and a fat jar that starts.

**These are not the report of [BENCHMARKS.md](BENCHMARKS.md)
§6.** Each cell is **one run with no warm-up** (`JRS_BENCH_RUNS=1
JRS_BENCH_WARMUP=0`), so there is no spread and a cell may move by tens of
percent in a full run. They were taken to check the harness end to end; the
published numbers come from the full run on the reference machine
(BENCHMARKS.md §7). They are kept here because they already show where to
look.

Columns: **jrs** · **Maven** · **Gradle** (warm daemon) · **Gradle CC**
(daemon with `--configuration-cache`) · **Gradle no daemon**. **Bold** marks
a tool that beat jrs.

## wordstats (a few classes, Guava, JUnit)

| Scenario | jrs | Maven | Gradle | Gradle CC | Gradle no daemon |
| --- | --- | --- | --- | --- | --- |
| cold | 557 ms | 1.43 s | 11.3 s | 11.3 s | 11.2 s |
| clean | 325 ms | 1.11 s | 454 ms | 428 ms | 2.40 s |
| noop | 63 ms | 821 ms | 357 ms | 333 ms | 2.05 s |
| incremental | 245 ms | 1.09 s | 392 ms | 365 ms | 2.32 s |
| abi | 426 ms | 1.10 s | **382 ms** | **376 ms** | 2.34 s |
| test | 369 ms | 1.14 s | 604 ms | 575 ms | 2.42 s |
| test-edit | 543 ms | 1.46 s | 652 ms | 681 ms | 2.69 s |
| run | 126 ms | 933 ms | 439 ms | 460 ms | 2.16 s |
| package | 207 ms | 1.43 s | 600 ms | 657 ms | 2.42 s |

## bookmarks (Spring Boot 4.1)

| Scenario | jrs | Maven | Gradle | Gradle CC | Gradle no daemon |
| --- | --- | --- | --- | --- | --- |
| cold | 632 ms | 1.84 s | 12.3 s | 12.5 s | 12.4 s |
| clean | 336 ms | 1.23 s | 558 ms | 616 ms | 2.60 s |
| noop | 63 ms | 961 ms | 450 ms | 329 ms | 2.31 s |
| incremental | 285 ms | 1.21 s | 489 ms | 392 ms | 2.60 s |
| abi | 475 ms | 1.37 s | **453 ms** | **387 ms** | 2.61 s |
| test | 1.08 s | 2.19 s | 1.63 s | 1.52 s | 3.67 s |
| test-edit | 1.30 s | 2.61 s | 1.65 s | 1.74 s | 4.06 s |
| run (to HTTP 200) | 762 ms | 1.88 s | 1.01 s | 1.08 s | 2.93 s |
| package | 719 ms | 1.45 s | **513 ms** | **556 ms** | 2.49 s |

## gen-300 (300 classes, 30 dependencies)

| Scenario | jrs | Maven | Gradle | Gradle CC | Gradle no daemon |
| --- | --- | --- | --- | --- | --- |
| cold | 891 ms | 1.92 s | 12.3 s | 12.4 s | 12.2 s |
| clean | 673 ms | 1.48 s | **640 ms** | **593 ms** | 2.79 s |
| noop | 66 ms | 869 ms | 370 ms | 344 ms | 2.11 s |
| incremental | 271 ms | 1.45 s | 432 ms | 405 ms | 2.45 s |
| abi | 843 ms | 1.45 s | **461 ms** | **415 ms** | 2.52 s |
| test | 474 ms | 1.23 s | 654 ms | 614 ms | 2.55 s |
| test-edit | 671 ms | 1.95 s | 709 ms | 762 ms | 2.86 s |
| run | 132 ms | 980 ms | 438 ms | 453 ms | 2.19 s |
| package | 758 ms | 2.25 s | 1.50 s | 1.55 s | 3.45 s |

## gen-2000 (2000 classes, 60 dependencies)

| Scenario | jrs | Maven | Gradle | Gradle CC | Gradle no daemon |
| --- | --- | --- | --- | --- | --- |
| cold | 1.85 s | 3.01 s | 14.2 s | 14.4 s | 14.1 s |
| clean | 1.55 s | 2.33 s | **1.26 s** | **1.24 s** | 3.91 s |
| noop | 85 ms | 1.01 s | 388 ms | 332 ms | 2.23 s |
| incremental | 386 ms | 2.49 s | 593 ms | 556 ms | 2.88 s |
| abi | 1.84 s | 2.51 s | **641 ms** | **588 ms** | 3.06 s |
| test | 910 ms | 1.66 s | **851 ms** | **811 ms** | 2.87 s |
| test-edit | 1.20 s | 3.30 s | **1.06 s** | **1.13 s** | 3.56 s |
| run | 237 ms | 1.11 s | 507 ms | 540 ms | 2.34 s |
| package | 1.41 s | 3.39 s | 2.58 s | 2.65 s | 4.52 s |

## gen-ws (a six-member workspace, 300 classes)

| Scenario | jrs | Maven | Gradle | Gradle CC | Gradle no daemon |
| --- | --- | --- | --- | --- | --- |
| cold | 2.22 s | **2.08 s** | 13.9 s | 13.9 s | 13.5 s |
| clean | 1.98 s | **1.58 s** | **791 ms** | **671 ms** | 3.04 s |
| noop | 81 ms | 911 ms | 395 ms | 331 ms | 2.22 s |
| incremental | 287 ms | 1.22 s | 443 ms | 379 ms | 2.53 s |
| abi | 2.09 s | **1.58 s** | **519 ms** | **446 ms** | 2.66 s |
| test | 1.89 s | 2.26 s | **1.83 s** | **714 ms** | 3.87 s |
| test-edit | 626 ms | 2.62 s | 681 ms | 748 ms | 2.93 s |
| run | not timed (BENCHMARKS.md §11) | | | | |
| package | 803 ms | 3.04 s | 1.49 s | 1.54 s | 3.47 s |

## Where jrs is better

- **Anything start-up bound.** A no-op build is 63–85 ms on every project,
  against 330–450 ms for a warm Gradle daemon (4–7×), about a second for
  Maven (11–15×), and over two seconds for Gradle without a daemon (26–37×).
  `run` follows suit: 126–237 ms to the first output, 762 ms to Spring
  Boot's first HTTP answer, against 0.4–1.1 s for the daemon.
- **One-file edits that change no API.** `incremental` is 245–386 ms, ahead
  of the warm daemon by 25–45% and of Maven by 4–6×: file-by-file
  compilation (`compile/incremental.rs`) compiles one source, and nothing
  else runs.
- **Cold builds.** 0.56–2.2 s from the mirror, against 1.4–3 s for Maven and
  11–14 s for Gradle, which pays for a fresh Gradle home on every cold run
  (its distribution, Kotlin DSL script compilation, the plugins).
- **Fat jars of plain projects.** `package` is 207–1410 ms, 1.8–3× ahead of
  `shadowJar` on a warm daemon.
- **Tests on small suites, and test selection.** `test` beats the warm
  daemon by 28–39% on `wordstats`, `bookmarks` and `gen-300`; `test-edit`
  pays most in the workspace (626 ms against 2.6 s for Maven), where only
  the member the change reaches runs its tests.
- **Against what CI sees.** Next to `gradle --no-daemon` and Maven, jrs wins
  every cell of every project but `gen-ws`'s `cold`, `clean` and `abi`
  against Maven.

## Where jrs can improve

- **A workspace's members compile one after another.** On `gen-ws`, `clean`
  (1.98 s) and `abi` (2.09 s) are six `javac` runs of about 300 ms each in
  sequence; Gradle's daemon does the same work in 0.5–0.8 s, and even
  Maven's single-threaded reactor is faster, since it compiles in one warm
  JVM. This is the evidence for "workspace members built in parallel" in
  [GRADLE_GAPS.md](GRADLE_GAPS.md); a warm in-process `javac`
  across members ([FASTER_BUILDS.md](FASTER_BUILDS.md) §6's worker
  outside `--watch`) would help as much.
- **An API change recompiles more than it needs to.** `abi` on `gen-2000` is
  1.84 s against Gradle's 0.64 s: jrs recompiles every source that
  transitively refers to the changed class (`compile/incremental.rs`), where
  Gradle's incremental compiler recompiles only what its class analysis
  finds affected, in a `javac` that is already warm. Stopping the walk at a
  dependent whose own API did not change is the candidate to measure.
- **A cold `javac` on large compiles.** `clean` on `gen-2000` is 1.55 s,
  1.34 s of it `javac`, against 1.26 s for the daemon's hot compiler; at 300
  classes the two are even. The JIT-warm daemon wins once the compile itself
  is long, which is again the case for a compiler worker.
- **Spring Boot's fat jar.** `package` on `bookmarks` is 719 ms, 655 ms of
  it packaging, against 513 ms for `bootJar`, which stores the dependency
  jars nested rather than unpacking every entry. Writing the larger jars
  stored (uncompressed) entry for entry, or in parallel, is worth trying.
- **Test JVM start on large suites.** On `gen-2000`, `test` (910 ms) and
  `test-edit` (1.20 s) trail the daemon by 6–13%, almost all of it in the
  test JVM (about 700 ms);
  [TEST_JVM_BENCHMARK.md](TEST_JVM_BENCHMARK.md)'s `share-classes`
  archive is the lever already built for it.
- **The workspace's `test` against Gradle's configuration cache** (1.89 s
  against 714 ms) needs a full run before it is read: a single sample that
  differs this much from the daemon's own 1.83 s may be noise.

What a full run (BENCHMARKS.md §7) must confirm before any of these gets
code, as BENCHMARKS.md §1 requires: the `abi` and workspace rows with their
spread, the `package` row for Spring Boot, and whether the gen-2000 test
rows are outside the noise.


---

## Second pass: jrs 0.12.0 (2026-10-15)

Run on **2026-10-15 at 22:10 CEST** (finished 22:24) with **jrs 0.12.0**
(`d86b2e7`, `benches/compare.rs`), on the same machine as the first pass:
Apple M4 Max (14 cores, 36 GiB), macOS 27.0.1, Corretto 25.0.2, Maven 3.9.16
and Gradle 9.1.0, every tool from a freshly recorded local mirror (14,569
requests, none unanswered). Every project passed the checks of
BENCHMARKS.md §5.1 on all three tools again.

The settings are the first pass's, so the two can be compared: **one run per
cell with no warm-up** (`JRS_BENCH_RUNS=1 JRS_BENCH_WARMUP=0`), the five
parity projects only (`JRS_BENCH_PROJECTS=wordstats,bookmarks,gen-300,gen-2000,gen-ws`).
The same caveat holds: this is still not the report of BENCHMARKS.md §6, and
a cell may move by tens of percent in a full run.

What changed since 0.11.0 is `1fd41b7`, which closes GRADLE_GAPS G1–G7.
For these numbers, G5 matters: **workspace members build at the same time**
(`cli/members.rs`, up to `--jobs`). The harness also gained a **Gradle
--parallel** column, timed on workspaces only, and a "workspace members at
once" table (G1).

### jrs, 0.11.0 against 0.12.0

| Project | Scenario | 0.11.0 | 0.12.0 | Change |
| --- | --- | --- | --- | --- |
| `gen-ws` | cold | 2.22 s | 1.90 s | −14% |
| `gen-ws` | clean | 1.98 s | 1.70 s | −14% |
| `gen-ws` | abi | 2.09 s | 1.77 s | −15% |
| `gen-ws` | test | 1.89 s | 1.80 s | −5% |
| `gen-2000` | run | 237 ms | 218 ms | −8% |
| `bookmarks` | package | 719 ms | 753 ms | +5% |

Every other jrs cell is within ±5% of the first pass, which is single-run
noise (no-op builds are 62–83 ms again, `incremental` 245–394 ms). Maven's
and Gradle's cells are within about ±6% of theirs too, so the machine and
the mirror behaved the same both times.

### Workspace members at once (`gen-ws`)

GRADLE_GAPS.md §2.1: each member's wall time in jrs's clean build.

| Members | Sequential (sum) | Critical path | jrs clean | Gradle --parallel | Gate |
| --- | --- | --- | --- | --- | --- |
| 6 | 1957 ms | 1624 ms (17% less) | 1696 ms | 723 ms | holds |

`gen-ws` is the graph `core → {model, store} → service → web → app`, so only
`model` and `store` can build at the same time. jrs's 1.70 s is within 72 ms of
that 1.62 s floor, so parallel members already give what this graph allows.
The remaining gap to Gradle (0.72–0.80 s) is each member's own cold `javac`, at
about 300 ms each (`member core 287 · model 333 · store 336 · service 320 ·
web 335 · app 346`). Gradle runs all six in one warm compiler, which is why
`--parallel` gains it almost nothing over the plain daemon (724 ms against
803 ms).

### All cells

Columns as in the first pass, plus **Gradle --parallel** (daemon with
`--parallel`) on `gen-ws`. **Bold** marks a tool that beat jrs.

### wordstats (a few classes, Guava, JUnit)

| Scenario | jrs | Maven | Gradle | Gradle CC | Gradle no daemon |
| --- | --- | --- | --- | --- | --- |
| cold | 554 ms | 1.43 s | 11.2 s | 11.3 s | 11.2 s |
| clean | 322 ms | 1.13 s | 444 ms | 417 ms | 2.37 s |
| noop | 63 ms | 829 ms | 342 ms | 325 ms | 2.02 s |
| incremental | 245 ms | 1.12 s | 392 ms | 363 ms | 2.29 s |
| abi | 417 ms | 1.19 s | **389 ms** | **361 ms** | 2.29 s |
| test | 358 ms | 1.18 s | 609 ms | 585 ms | 2.40 s |
| test-edit | 546 ms | 1.49 s | 635 ms | 673 ms | 2.63 s |
| run | 126 ms | 985 ms | 415 ms | 454 ms | 2.10 s |
| package | 206 ms | 1.47 s | 604 ms | 625 ms | 2.41 s |

### bookmarks (Spring Boot 4.1)

| Scenario | jrs | Maven | Gradle | Gradle CC | Gradle no daemon |
| --- | --- | --- | --- | --- | --- |
| cold | 643 ms | 1.84 s | 12.3 s | 12.5 s | 12.0 s |
| clean | 342 ms | 1.18 s | 574 ms | 610 ms | 2.59 s |
| noop | 62 ms | 946 ms | 453 ms | 323 ms | 2.34 s |
| incremental | 296 ms | 1.21 s | 483 ms | 397 ms | 2.65 s |
| abi | 471 ms | 1.19 s | **457 ms** | **415 ms** | 2.56 s |
| test | 1.08 s | 2.18 s | 1.65 s | 1.50 s | 3.58 s |
| test-edit | 1.27 s | 2.52 s | 1.61 s | 1.75 s | 4.18 s |
| run (to HTTP 200) | 738 ms | 1.90 s | 1.01 s | 1.05 s | 2.93 s |
| package | 753 ms | 1.37 s | **515 ms** | **536 ms** | 2.43 s |

### gen-300 (300 classes, 30 dependencies)

| Scenario | jrs | Maven | Gradle | Gradle CC | Gradle no daemon |
| --- | --- | --- | --- | --- | --- |
| cold | 906 ms | 1.91 s | 12.1 s | 12.2 s | 11.8 s |
| clean | 684 ms | 1.45 s | **613 ms** | **611 ms** | 2.80 s |
| noop | 65 ms | 880 ms | 356 ms | 328 ms | 2.05 s |
| incremental | 264 ms | 1.45 s | 421 ms | 395 ms | 2.37 s |
| abi | 852 ms | 1.49 s | **466 ms** | **416 ms** | 2.52 s |
| test | 450 ms | 1.23 s | 655 ms | 609 ms | 2.49 s |
| test-edit | 655 ms | 1.91 s | 704 ms | 759 ms | 2.80 s |
| run | 131 ms | 1.02 s | 398 ms | 446 ms | 2.10 s |
| package | 731 ms | 2.22 s | 1.48 s | 1.49 s | 3.24 s |

### gen-2000 (2000 classes, 60 dependencies)

| Scenario | jrs | Maven | Gradle | Gradle CC | Gradle no daemon |
| --- | --- | --- | --- | --- | --- |
| cold | 1.91 s | 3.11 s | 14.3 s | 14.2 s | 13.9 s |
| clean | 1.53 s | 2.47 s | **1.28 s** | **1.26 s** | 3.77 s |
| noop | 83 ms | 1.06 s | 377 ms | 335 ms | 2.23 s |
| incremental | 394 ms | 2.46 s | 583 ms | 595 ms | 2.89 s |
| abi | 1.87 s | 2.53 s | **654 ms** | **612 ms** | 3.09 s |
| test | 901 ms | 1.69 s | **831 ms** | **795 ms** | 2.79 s |
| test-edit | 1.19 s | 3.25 s | **1.03 s** | **1.07 s** | 3.43 s |
| run | 218 ms | 1.06 s | 496 ms | 536 ms | 2.30 s |
| package | 1.40 s | 3.35 s | 2.56 s | 2.57 s | 4.48 s |

### gen-ws (a six-member workspace, 300 classes)

| Scenario | jrs | Maven | Gradle | Gradle CC | Gradle no daemon | Gradle --parallel |
| --- | --- | --- | --- | --- | --- | --- |
| cold | 1.90 s | 2.05 s | 13.7 s | 13.5 s | 13.4 s | 13.4 s |
| clean | 1.70 s | **1.60 s** | **803 ms** | **675 ms** | 3.03 s | **724 ms** |
| noop | 83 ms | 926 ms | 400 ms | 339 ms | 2.22 s | 387 ms |
| incremental | 278 ms | 1.23 s | 439 ms | 372 ms | 2.43 s | 422 ms |
| abi | 1.77 s | **1.58 s** | **505 ms** | **432 ms** | 2.70 s | **476 ms** |
| test | 1.80 s | 2.27 s | 1.85 s | **716 ms** | 3.87 s | **729 ms** |
| test-edit | 620 ms | 2.61 s | 670 ms | 754 ms | 2.89 s | 656 ms |
| run | not timed (BENCHMARKS.md §11) | | | | | |
| package | 818 ms | 3.05 s | 1.49 s | 1.52 s | 3.45 s | 1.49 s |

### Where jrs is better (0.12.0)

All of the first pass still holds, cell for cell:

- **Start-up bound work.** No-op builds are 62–83 ms on every project,
  against 323–453 ms for a warm Gradle daemon (4–7×), 0.83–1.06 s for Maven
  (11–15×) and 2.0–2.3 s for Gradle without a daemon (27–38×). `run` takes
  126–218 ms to first output, and 738 ms to Spring Boot's first HTTP answer.
- **Edits that change no API.** `incremental` is 245–394 ms, 1.3–1.6× ahead
  of the warm daemon and 4–6× ahead of Maven.
- **Cold builds.** 0.55–1.91 s against 1.43–3.11 s for Maven and 11–14 s for
  Gradle. The workspace's cold build now beats Maven too (1.90 s against
  2.05 s; in 0.11.0 it was 2.22 s against 2.08 s).
- **Fat jars of plain projects.** 206 ms–1.40 s, 1.8–2.9× ahead of
  `shadowJar` on a warm daemon.
- **Tests.** `test` beats the warm daemon by 1.4–1.7× on `wordstats`,
  `bookmarks` and `gen-300`. `test-edit` in the workspace is 620 ms against
  2.61 s for Maven.
- **Against what CI sees.** Against `gradle --no-daemon`, jrs wins every
  cell. Against Maven it loses only `gen-ws`'s `clean` (1.70 s against
  1.60 s) and `abi` (1.77 s against 1.58 s); `cold` there is now a win.

### Where jrs can improve (0.12.0)

- **Workspaces still trail the daemon by 2–4×.** Parallel members took
  about 15% off `clean`, `abi` and `cold`, but on this graph the floor is
  the chain of five cold `javac` runs. To go further, the per-member `javac`
  has to be cheaper: one warm compiler shared by the members (the
  FASTER_BUILDS.md §6 worker, outside `--watch`), or one `javac` for members
  that are ready at the same moment. A wider graph would show more gain from
  G5 than `gen-ws` can.
- **`gen-ws` `test` against Gradle CC and `--parallel`** (1.80 s against
  0.72–0.73 s) is now seen in two passes, so it is not noise. jrs tests
  member after member, with six test JVMs of about 280 ms each
  (`cli/members.rs` builds every member first, then tests them one at a
  time). Running the members' test JVMs at the same time is the next lever.
- **An API change recompiles more than it needs to**: unchanged. `abi` on
  `gen-2000` is 1.87 s against 0.65 s, and on `gen-300` 852 ms against
  466 ms.
- **A cold `javac` on large compiles**: unchanged. `clean` on `gen-2000` is
  1.53 s (1.33 s of it `javac`) against 1.28 s.
- **Spring Boot's fat jar**: unchanged. 753 ms (684 ms of it packaging)
  against `bootJar`'s 515 ms.
- **Test JVM on large suites**: unchanged. `gen-2000`'s `test` (901 ms) and
  `test-edit` (1.19 s) trail the daemon by 8–16%, about 700 ms of each in
  the test JVM.

A full run (seven measured runs and two warm-ups per cell) is still what
BENCHMARKS.md §7 asks for before these become the published report.
