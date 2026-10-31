# Benchmarks against Maven and Gradle: first results

Twelve passes are recorded here, oldest first: jrs 0.11.0 on 2026-10-14
(below), jrs 0.12.0 on 2026-10-15 at 22:10 CEST
([Second pass](#second-pass-jrs-0120-2026-10-15)), jrs 0.13.0 on
2026-10-16 at 22:38 CEST ([Third pass](#third-pass-jrs-0130-2026-10-16)),
jrs 0.14.0 on 2026-10-17 at 13:05 CEST
([Fourth pass](#fourth-pass-jrs-0140-2026-10-17)), jrs 0.14.1 on
2026-10-17 at 17:18 CEST ([Fifth pass](#fifth-pass-jrs-0141-2026-10-17)),
jrs 0.14.2 on 2026-10-17 at 19:03 CEST
([Sixth pass](#sixth-pass-jrs-0142-2026-10-17)), jrs 0.14.3 on
2026-10-17 at 20:42 CEST
([Seventh pass](#seventh-pass-jrs-0143-2026-10-17)), jrs 0.14.4 on
2026-10-18 at 09:58 CEST
([Eighth pass](#eighth-pass-jrs-0144-2026-10-18)), jrs 0.14.5 on
2026-10-18 at 13:10 CEST
([Ninth pass](#ninth-pass-jrs-0145-2026-10-18)), jrs 0.14.6 on
2026-10-18 at 13:57 CEST
([Tenth pass](#tenth-pass-jrs-0146-2026-10-18)), jrs 0.14.7 on
2026-10-18 at 19:04 CEST
([Eleventh pass](#eleventh-pass-jrs-0147-2026-10-18)), and jrs 0.16.0 on
2026-10-31 at 20:22 CET
([Twelfth pass](#twelfth-pass-jrs-0160-2026-10-31)).

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

---

## Third pass: jrs 0.13.0 (2026-10-16)

Run on **2026-10-16 at 22:38 CEST** (finished 22:52) with **jrs 0.13.0**
(`f52f4d6`, `benches/compare.rs`), on the same machine as the earlier
passes: Apple M4 Max (14 cores: 10 performance and 4 efficiency, 36 GiB),
macOS 27.0.1 (26A434), Corretto 25.0.2, Maven 3.9.16 and Gradle 9.1.0,
every tool from a freshly recorded local mirror (8,138 requests, none
unanswered). Every project passed the checks of BENCHMARKS.md §5.1 on all
three tools again.

The settings are the earlier passes', so the three can be compared: **one
run per cell with no warm-up** (`JRS_BENCH_RUNS=1 JRS_BENCH_WARMUP=0`), the
five parity projects only
(`JRS_BENCH_PROJECTS=wordstats,bookmarks,gen-300,gen-2000,gen-ws`). The same
caveat holds: this is still not the report of BENCHMARKS.md §6, and a cell
may move by tens of percent in a full run.

What changed since `d86b2e7` that can move a number:

- `9b17c6d` copies a dependency's deflated jar entries as they are, instead
  of inflating and deflating each one again: `package`.
- `a7e8f26` gives a forked `javac` a JEP 483 AOT cache on JDK 25, made in
  two steps by the first runs that find none: every forked `javac`.
- `76ac2ce` follows an API change in rounds, compiling a changed class's
  users and stopping when a round changes no API, and tracks upstream
  class directories class by class: `abi`, and every workspace member
  downstream of a change.
- `3df72e4` compiles workspace members in a pool of warm `javac` workers:
  `gen-ws`'s builds.
- `89adc5f` can run members' test JVMs at once, with `[workspace]
  parallel-tests = true`. It is off by default and `gen-ws` does not set
  it, so it is not in these numbers.
- `342cfbb` lets the first test fork dump the class-data archive, which
  only matters to suites large enough to fork.

### jrs, 0.12.0 against 0.13.0

| Project | Scenario | 0.12.0 | 0.13.0 | Change |
| --- | --- | --- | --- | --- |
| `wordstats` | cold | 554 ms | 973 ms | +76% |
| `wordstats` | clean | 322 ms | 250 ms | −22% |
| `wordstats` | incremental | 245 ms | 849 ms | +247% |
| `wordstats` | abi | 417 ms | 367 ms | −12% |
| `wordstats` | test-edit | 546 ms | 501 ms | −8% |
| `wordstats` | package | 206 ms | 88 ms | −57% |
| `bookmarks` | cold | 643 ms | 1.06 s | +65% |
| `bookmarks` | incremental | 296 ms | 880 ms | +197% |
| `bookmarks` | abi | 471 ms | 404 ms | −14% |
| `bookmarks` | package | 753 ms | 201 ms | −73% |
| `gen-300` | cold | 906 ms | 1.36 s | +50% |
| `gen-300` | clean | 684 ms | 604 ms | −12% |
| `gen-300` | incremental | 264 ms | 844 ms | +220% |
| `gen-300` | abi | 852 ms | 501 ms | −41% |
| `gen-300` | test-edit | 655 ms | 593 ms | −9% |
| `gen-300` | package | 731 ms | 242 ms | −67% |
| `gen-2000` | cold | 1.91 s | 2.28 s | +19% |
| `gen-2000` | clean | 1.53 s | 1.45 s | −5% |
| `gen-2000` | incremental | 394 ms | 1.11 s | +182% |
| `gen-2000` | abi | 1.87 s | 822 ms | −56% |
| `gen-2000` | run | 218 ms | 237 ms | +9% |
| `gen-2000` | package | 1.40 s | 486 ms | −65% |
| `gen-ws` | clean | 1.70 s | 1.13 s | −34% |
| `gen-ws` | incremental | 278 ms | 303 ms | +9% |
| `gen-ws` | abi | 1.77 s | 667 ms | −62% |
| `gen-ws` | package | 818 ms | 309 ms | −62% |

Every other jrs cell is within ±5% of 0.12.0. Maven's and
Gradle's cells are within about ±12% of theirs, most within ±5%, so the
machine and the mirror behaved the same; the jrs moves above are jrs's.

Three groups stand out:

- **`package` is 2.3–3.7× faster everywhere** (packaging 27–403 ms in
  `--timings`; `bookmarks`'s was 684 ms), which is `9b17c6d`.
- **`abi` is 12–62% faster**, most on the projects where one class reaches
  everything: `gen-2000` 1.87 s to 822 ms, `gen-ws` 1.77 s to 667 ms.
  `gen-ws`'s `clean` fell from 1.70 s to 1.13 s.
- **`incremental` is 2.8–3.5× slower on the four single-module projects,
  and `cold` 19–76% slower.** In both, the forked `javac` is far slower
  than in the cells around it: 750–921 ms for one edit in `incremental`,
  where `test-edit` compiles the same edit in 137–166 ms;
  879–1,957 ms in `cold` against 182–1,136 ms in `clean`. That is the
  shape of the `javac` AOT cache of `a7e8f26` being recorded and assembled
  in those cells, a one-off that a warm-up run would take out of them, and
  `gen-ws`, which compiles in warm workers rather than a forked `javac`,
  shows neither. This pass does not show that it is the cause; it needs a
  look before the next pass.

### Workspace members at once (`gen-ws`)

GRADLE_GAPS.md §2.1: each member's wall time in jrs's clean build.

| Members | Sequential (sum) | Critical path | jrs clean | Gradle --parallel | Gate |
| --- | --- | --- | --- | --- | --- |
| 6 | 1119 ms | 995 ms (11% less) | 1131 ms | 738 ms | holds |

The warm `javac` pool took the members after the first two from about
330 ms each to 123–147 ms (`member core 285 · model 316 · store 124 ·
service 147 · web 123 · app 124`). `core` and `model` still take about
300 ms each, the pool's first compiles, and being on the chain
`core → model → service → web → app` they are most of the 995 ms floor. jrs's
1.13 s is 136 ms above that floor and 393 ms behind Gradle `--parallel`.

### All cells

Columns as in the second pass. **Bold** marks a tool that beat jrs.

### wordstats (a few classes, Guava, JUnit)

| Scenario | jrs | Maven | Gradle | Gradle CC | Gradle no daemon |
| --- | --- | --- | --- | --- | --- |
| cold | 973 ms | 1.46 s | 11.2 s | 11.4 s | 11.2 s |
| clean | 250 ms | 1.12 s | 450 ms | 426 ms | 2.35 s |
| noop | 62 ms | 847 ms | 350 ms | 322 ms | 2.02 s |
| incremental | 849 ms | 1.12 s | **387 ms** | **370 ms** | 2.30 s |
| abi | 367 ms | 1.15 s | 391 ms | 373 ms | 2.30 s |
| test | 354 ms | 1.16 s | 614 ms | 573 ms | 2.40 s |
| test-edit | 501 ms | 1.48 s | 643 ms | 676 ms | 2.62 s |
| run | 123 ms | 949 ms | 421 ms | 459 ms | 2.10 s |
| package | 88 ms | 1.46 s | 607 ms | 646 ms | 2.36 s |

### bookmarks (Spring Boot 4.1)

| Scenario | jrs | Maven | Gradle | Gradle CC | Gradle no daemon |
| --- | --- | --- | --- | --- | --- |
| cold | 1.06 s | 1.90 s | 12.1 s | 12.5 s | 12.2 s |
| clean | 329 ms | 1.32 s | 578 ms | 615 ms | 2.62 s |
| noop | 62 ms | 946 ms | 426 ms | 315 ms | 2.23 s |
| incremental | 880 ms | 1.18 s | **492 ms** | **386 ms** | 2.54 s |
| abi | 404 ms | 1.19 s | 453 ms | **382 ms** | 2.57 s |
| test | 1.06 s | 2.16 s | 1.60 s | 1.49 s | 3.58 s |
| test-edit | 1.28 s | 2.53 s | 1.64 s | 1.71 s | 3.84 s |
| run (to HTTP 200) | 758 ms | 1.94 s | 1.01 s | 1.05 s | 2.86 s |
| package | 201 ms | 1.46 s | 494 ms | 529 ms | 2.46 s |

### gen-300 (300 classes, 30 dependencies)

| Scenario | jrs | Maven | Gradle | Gradle CC | Gradle no daemon |
| --- | --- | --- | --- | --- | --- |
| cold | 1.36 s | 1.90 s | 12.0 s | 12.1 s | 11.9 s |
| clean | 604 ms | 1.57 s | 651 ms | **588 ms** | 2.71 s |
| noop | 65 ms | 885 ms | 357 ms | 317 ms | 2.03 s |
| incremental | 844 ms | 1.45 s | **433 ms** | **391 ms** | 2.37 s |
| abi | 501 ms | 1.47 s | **432 ms** | **404 ms** | 2.42 s |
| test | 446 ms | 1.23 s | 642 ms | 603 ms | 2.41 s |
| test-edit | 593 ms | 1.89 s | 688 ms | 752 ms | 2.77 s |
| run | 126 ms | 997 ms | 396 ms | 445 ms | 2.09 s |
| package | 242 ms | 2.32 s | 1.46 s | 1.50 s | 3.25 s |

### gen-2000 (2000 classes, 60 dependencies)

| Scenario | jrs | Maven | Gradle | Gradle CC | Gradle no daemon |
| --- | --- | --- | --- | --- | --- |
| cold | 2.28 s | 3.02 s | 14.0 s | 14.2 s | 14.7 s |
| clean | 1.45 s | 2.60 s | **1.24 s** | **1.27 s** | 3.84 s |
| noop | 85 ms | 993 ms | 364 ms | 339 ms | 2.19 s |
| incremental | 1.11 s | 2.47 s | **575 ms** | **563 ms** | 2.81 s |
| abi | 822 ms | 2.49 s | **623 ms** | **590 ms** | 2.92 s |
| test | 856 ms | 1.76 s | **837 ms** | **802 ms** | 2.80 s |
| test-edit | 1.17 s | 3.32 s | **1.06 s** | **1.11 s** | 3.54 s |
| run | 237 ms | 1.15 s | 495 ms | 541 ms | 2.30 s |
| package | 486 ms | 3.47 s | 2.55 s | 2.63 s | 4.52 s |

### gen-ws (a six-member workspace, 300 classes)

| Scenario | jrs | Maven | Gradle | Gradle CC | Gradle no daemon | Gradle --parallel |
| --- | --- | --- | --- | --- | --- | --- |
| cold | 1.82 s | 2.06 s | 13.8 s | 14.1 s | 13.9 s | 13.7 s |
| clean | 1.13 s | 1.70 s | **800 ms** | **693 ms** | 3.10 s | **738 ms** |
| noop | 81 ms | 955 ms | 391 ms | 328 ms | 2.16 s | 390 ms |
| incremental | 303 ms | 1.20 s | 434 ms | 365 ms | 2.43 s | 413 ms |
| abi | 667 ms | 1.61 s | **492 ms** | **449 ms** | 2.64 s | **473 ms** |
| test | 1.81 s | 2.26 s | 1.92 s | **714 ms** | 3.80 s | **756 ms** |
| test-edit | 642 ms | 2.60 s | 692 ms | 732 ms | 2.85 s | 644 ms |
| run | not timed (BENCHMARKS.md §11) | | | | | |
| package | 309 ms | 3.15 s | 1.54 s | 1.60 s | 3.51 s | 1.54 s |

### Where jrs is better (0.13.0)

- **Start-up bound work.** No-op builds are 62–85 ms on every project,
  against 315–426 ms for a warm Gradle daemon (4–7×), 0.85–0.99 s for Maven
  (12–15×) and 2.0–2.2 s for Gradle without a daemon (26–36×). `run` takes
  123–237 ms to first output, and 758 ms to Spring Boot's first HTTP answer.
- **Fat jars, Spring Boot's now included.** 88–486 ms, 5.0–6.9× ahead of
  `shadowJar` on a warm daemon, and 201 ms against `bootJar`'s 494 ms on
  `bookmarks` (2.5×), where the second pass lost by 753 ms to 515 ms.
- **Cold builds.** 0.97–2.28 s against 1.46–3.02 s for Maven and 11–15 s
  for Gradle, though by less than in the second pass (see above).
- **Tests.** `test` beats the warm daemon by 1.4–1.7× on `wordstats`,
  `bookmarks` and `gen-300`. `test-edit` in the workspace is 642 ms against
  2.60 s for Maven.
- **API changes on small projects.** `abi` now beats the warm daemon on
  `wordstats` (367 ms against 391 ms) and `bookmarks` (404 ms against
  453 ms), both lost in the second pass.
- **Against what CI sees.** Against `gradle --no-daemon` and against Maven,
  jrs wins every cell. `gen-ws`'s `clean` (1.13 s against 1.70 s) and `abi`
  (667 ms against 1.61 s), its two losses to Maven in the second pass, are
  now wins.

### Where jrs can improve (0.13.0)

- **One-file edits outside a workspace (new).** `incremental` is 844 ms–
  1.11 s on the four single-module projects, against 370–575 ms for the
  warm daemon; it was 245–394 ms and ahead of the daemon in the second
  pass. Almost all of it is one slow forked `javac` (750–921 ms), and
  `cold` carries the same kind of excess. If it is the `javac` AOT cache
  being made, the cache should be made where it costs no measured build,
  or not on an edit's critical path; either way it needs confirming first.
  It is still ahead of Maven (1.3–2.2×).
- **Workspaces still trail the daemon, now by 1.4–1.6×.** `clean` is 1.13 s
  against 693–800 ms, and `abi` 667 ms against 449–492 ms. With warm
  workers, the floor is now `core` and `model` at about 300 ms each, the
  pool's first compiles.
- **`gen-ws` `test` against Gradle CC and `--parallel`** (1.81 s against
  714–756 ms), in a third pass. `parallel-tests` exists now but is off by
  default and `gen-ws` does not set it, so the six test JVMs (273–283 ms
  each) still run one after another here.
- **An API change on a large project**, much closer: `abi` on `gen-2000` is
  822 ms against 590–623 ms (it was 1.87 s), and on `gen-300` 501 ms against
  404–432 ms (it was 852 ms).
- **A cold `javac` on large compiles**: unchanged. `clean` on `gen-2000` is
  1.45 s (1,136 ms of it `javac`) against 1.24–1.27 s.
- **Test JVM on large suites**: closer. `gen-2000`'s `test` (856 ms) and
  `test-edit` (1.17 s) trail the daemon by 2–10%, 643 ms and 703 ms of them
  in the test JVM.

A full run (seven measured runs and two warm-ups per cell) is still what
BENCHMARKS.md §7 asks for before these become the published report.

---

## Fourth pass: jrs 0.14.0 (2026-10-17)

Run on **2026-10-17 at 13:05 CEST** (finished 13:19) with **jrs 0.14.0**
(`cf0d36d`, `benches/compare.rs`), on the same machine as the earlier
passes: Apple M4 Max (14 cores: 10 performance and 4 efficiency, 36 GiB),
macOS 27.0.1 (26A434), Corretto 25.0.2, Maven 3.9.16 and Gradle 9.1.0,
every tool from a freshly recorded local mirror (8,144 requests, none
unanswered). Every project passed the checks of BENCHMARKS.md §5.1 on all
three tools again.

The settings are the earlier passes', so the four can be compared: **one
run per cell with no warm-up** (`JRS_BENCH_RUNS=1 JRS_BENCH_WARMUP=0`), the
five parity projects only
(`JRS_BENCH_PROJECTS=wordstats,bookmarks,gen-300,gen-2000,gen-ws`). The same
caveat holds: this is still not the report of BENCHMARKS.md §6, and a cell
may move by tens of percent in a full run.

What changed since `f52f4d6` that can move a number:

- `fd29d54` keeps one `javac` AOT cache per JDK, which only a whole-unit
  compile writes and which the second command to find none writes, not
  the first; gives the `javac` worker a cache of its own, starts a
  workspace's first worker while the graph is read, and sends a
  single-module unit's later rounds to a worker: every `javac`, forked or
  warm.
- `899262c` makes `share-classes = true` use the AOT cache on JDK 25: the
  test JVM of a project that sets it.
- `a6185bd` sets `parallel-tests = true` in the generated workspace, so
  `gen-ws`'s six test JVMs now run at once, as Gradle's CC and `--parallel`
  columns run their test tasks (BENCHMARKS.md §5): `gen-ws`'s `test` and
  `test-edit`.

### jrs, 0.13.0 against 0.14.0

| Project | Scenario | 0.13.0 | 0.14.0 | Change |
| --- | --- | --- | --- | --- |
| `wordstats` | cold | 973 ms | 424 ms | −56% |
| `wordstats` | clean | 250 ms | 1.01 s | +304% |
| `wordstats` | incremental | 849 ms | 215 ms | −75% |
| `wordstats` | run | 123 ms | 132 ms | +7% |
| `bookmarks` | cold | 1.06 s | 513 ms | −52% |
| `bookmarks` | clean | 329 ms | 1.03 s | +213% |
| `bookmarks` | incremental | 880 ms | 252 ms | −71% |
| `bookmarks` | package | 201 ms | 173 ms | −14% |
| `gen-300` | cold | 1.36 s | 767 ms | −44% |
| `gen-300` | clean | 604 ms | 1.35 s | +124% |
| `gen-300` | incremental | 844 ms | 242 ms | −71% |
| `gen-300` | test-edit | 593 ms | 687 ms | +16% |
| `gen-300` | package | 242 ms | 190 ms | −21% |
| `gen-2000` | cold | 2.28 s | 1.67 s | −27% |
| `gen-2000` | clean | 1.45 s | 2.18 s | +50% |
| `gen-2000` | incremental | 1.11 s | 349 ms | −69% |
| `gen-2000` | package | 486 ms | 362 ms | −26% |
| `gen-ws` | cold | 1.82 s | 1.38 s | −24% |
| `gen-ws` | clean | 1.13 s | 2.35 s | +108% |
| `gen-ws` | noop | 81 ms | 86 ms | +6% |
| `gen-ws` | incremental | 303 ms | 207 ms | −32% |
| `gen-ws` | abi | 667 ms | 511 ms | −23% |
| `gen-ws` | test | 1.81 s | 574 ms | −68% |
| `gen-ws` | test-edit | 642 ms | 540 ms | −16% |
| `gen-ws` | package | 309 ms | 258 ms | −17% |

Every other jrs cell is within ±5% of 0.13.0. Gradle's cells are within
−6% and +16% of theirs, most within ±5%. **Maven's are not:** with the same
Maven 3.9.16, they are slower in almost every cell, by 11% at the median and
by up to 31% (`gen-300`'s `noop`, 885 ms to 1.16 s). jrs's start-up bound
cells (`noop`, `test`, `run`) and Gradle's say the machine behaved the same,
so the move is Maven's own; its ratios to jrs below are about a tenth
larger than the third pass's for that reason alone.

Four groups stand out:

- **`incremental` is 3.2–3.9× faster on the four single-module projects**,
  and back ahead of the warm daemon: 215–349 ms, against 245–394 ms in the
  second pass. The forked `javac` takes 146–169 ms for the edit, where it
  took 750–921 ms in the third pass.
- **`cold` is 24–56% faster**: `javac` takes 334–1,366 ms in it, against
  879–1,957 ms in the third pass.
- **`clean` is 1.5–4.0× slower.** `javac` takes 936–1,981 ms in it, more
  than in `cold` (334–1,366 ms), where it compiles the same sources with
  nothing on disk. `clean` is the second command each project's cells run on
  an empty cache, which is the command `fd29d54` has write the `javac` AOT
  cache, so the cost the third pass saw in `cold` and `incremental` is now
  in `clean`; a warm-up run would take it out, as it would have from the
  third pass's cells. In `gen-ws`, the members are done 1,247 ms in
  (`--timings`) but the command takes 2.35 s: the warm workers are waited
  for at the end, and in this cell they have caches to write. This pass
  shows the shape, not the cause; the archive's timings would.
- **`gen-ws`'s `test` is 3.2× faster** (1.81 s to 574 ms), since its six
  test JVMs (429–462 ms each) now run at once (`a6185bd`). Its `abi`,
  `incremental` and `test-edit` are 16–32% faster too, with warm workers
  that now map caches of their own.

`package` is 14–26% faster on four projects, which no commit in the log
above touches; this pass does not explain it.

### Workspace members at once (`gen-ws`)

GRADLE_GAPS.md §2.1: each member's wall time in jrs's clean build.

| Members | Sequential (sum) | Critical path | jrs clean | Gradle --parallel | Gate |
| --- | --- | --- | --- | --- | --- |
| 6 | 1308 ms | 1178 ms (10% less) | 2352 ms | 744 ms | holds |

The members took `core 383 · model 439 · store 130 · service 123 · web 115 ·
app 118` ms: `core` and `model` about 100 ms longer than in the third pass,
the rest about the same, and the critical path is 1,178 ms. jrs's clean build is 1,174 ms above that floor, where it
was 136 ms above it in the third pass: the time after the last member,
described above.

### All cells

Columns as in the second pass. **Bold** marks a tool that beat jrs.

### wordstats (a few classes, Guava, JUnit)

| Scenario | jrs | Maven | Gradle | Gradle CC | Gradle no daemon |
| --- | --- | --- | --- | --- | --- |
| cold | 424 ms | 1.62 s | 11.7 s | 12.1 s | 11.7 s |
| clean | 1.01 s | 1.30 s | **455 ms** | **412 ms** | 2.39 s |
| noop | 63 ms | 1.02 s | 347 ms | 326 ms | 2.05 s |
| incremental | 215 ms | 1.28 s | 389 ms | 359 ms | 2.31 s |
| abi | 364 ms | 1.27 s | 389 ms | 369 ms | 2.32 s |
| test | 360 ms | 1.32 s | 611 ms | 571 ms | 2.42 s |
| test-edit | 515 ms | 1.62 s | 650 ms | 671 ms | 2.64 s |
| run | 132 ms | 1.14 s | 433 ms | 447 ms | 2.10 s |
| package | 89 ms | 1.62 s | 589 ms | 679 ms | 2.42 s |

### bookmarks (Spring Boot 4.1)

| Scenario | jrs | Maven | Gradle | Gradle CC | Gradle no daemon |
| --- | --- | --- | --- | --- | --- |
| cold | 513 ms | 2.06 s | 13.0 s | 13.2 s | 12.5 s |
| clean | 1.03 s | 1.38 s | **569 ms** | **636 ms** | 2.63 s |
| noop | 63 ms | 1.09 s | 448 ms | 322 ms | 2.26 s |
| incremental | 252 ms | 1.34 s | 498 ms | 385 ms | 2.57 s |
| abi | 406 ms | 1.34 s | 461 ms | 444 ms | 2.64 s |
| test | 1.09 s | 2.37 s | 1.63 s | 1.53 s | 3.64 s |
| test-edit | 1.29 s | 2.74 s | 1.65 s | 1.73 s | 3.93 s |
| run (to HTTP 200) | 756 ms | 2.02 s | 1.00 s | 1.07 s | 2.88 s |
| package | 173 ms | 1.62 s | 512 ms | 553 ms | 2.47 s |

### gen-300 (300 classes, 30 dependencies)

| Scenario | jrs | Maven | Gradle | Gradle CC | Gradle no daemon |
| --- | --- | --- | --- | --- | --- |
| cold | 767 ms | 2.06 s | 12.5 s | 12.8 s | 12.9 s |
| clean | 1.35 s | 1.74 s | **668 ms** | **620 ms** | 2.92 s |
| noop | 66 ms | 1.16 s | 373 ms | 338 ms | 2.14 s |
| incremental | 242 ms | 1.68 s | 435 ms | 399 ms | 2.42 s |
| abi | 493 ms | 1.67 s | **434 ms** | **421 ms** | 2.47 s |
| test | 452 ms | 1.46 s | 715 ms | 624 ms | 2.53 s |
| test-edit | 687 ms | 2.14 s | 706 ms | 779 ms | 2.93 s |
| run | 126 ms | 1.22 s | 416 ms | 469 ms | 2.29 s |
| package | 190 ms | 2.49 s | 1.48 s | 1.52 s | 3.31 s |

### gen-2000 (2000 classes, 60 dependencies)

| Scenario | jrs | Maven | Gradle | Gradle CC | Gradle no daemon |
| --- | --- | --- | --- | --- | --- |
| cold | 1.67 s | 3.24 s | 14.8 s | 14.7 s | 14.1 s |
| clean | 2.18 s | 2.56 s | **1.23 s** | **1.19 s** | 3.83 s |
| noop | 89 ms | 1.21 s | 362 ms | 324 ms | 2.17 s |
| incremental | 349 ms | 2.70 s | 600 ms | 567 ms | 2.86 s |
| abi | 810 ms | 2.69 s | **634 ms** | **576 ms** | 2.95 s |
| test | 893 ms | 2.02 s | **842 ms** | **804 ms** | 2.84 s |
| test-edit | 1.19 s | 3.53 s | **1.07 s** | **1.12 s** | 3.50 s |
| run | 232 ms | 1.30 s | 497 ms | 534 ms | 2.40 s |
| package | 362 ms | 3.58 s | 2.54 s | 2.61 s | 4.48 s |

### gen-ws (a six-member workspace, 300 classes)

| Scenario | jrs | Maven | Gradle | Gradle CC | Gradle no daemon | Gradle --parallel |
| --- | --- | --- | --- | --- | --- | --- |
| cold | 1.38 s | 2.28 s | 14.1 s | 14.1 s | 13.9 s | 14.4 s |
| clean | 2.35 s | **1.80 s** | **768 ms** | **690 ms** | 3.10 s | **744 ms** |
| noop | 86 ms | 1.16 s | 388 ms | 340 ms | 2.17 s | 396 ms |
| incremental | 207 ms | 1.44 s | 435 ms | 378 ms | 2.45 s | 420 ms |
| abi | 511 ms | 1.88 s | **506 ms** | **454 ms** | 2.58 s | **485 ms** |
| test | 574 ms | 2.51 s | 1.81 s | 668 ms | 3.75 s | 744 ms |
| test-edit | 540 ms | 3.04 s | 703 ms | 770 ms | 2.83 s | 639 ms |
| run | not timed (BENCHMARKS.md §11) | | | | | |
| package | 258 ms | 3.29 s | 1.49 s | 1.54 s | 3.41 s | 1.50 s |

### Where jrs is better (0.14.0)

- **Start-up bound work.** No-op builds are 63–89 ms on every project,
  against 322–448 ms for a warm Gradle daemon (4–7×), 1.02–1.21 s for Maven
  (14–18×) and 2.05–2.26 s for Gradle without a daemon (24–36×). `run`
  takes 126–232 ms to first output, and 756 ms to Spring Boot's first HTTP
  answer.
- **One-file edits, again.** `incremental` is 207–349 ms on all five
  projects, 1.5–1.8× ahead of the faster warm-daemon column (359–600 ms)
  and 5–8× ahead of Maven. It was 844 ms–1.11 s and behind the daemon on
  the single-module projects in the third pass.
- **Fat jars, Spring Boot's included.** 89–362 ms, 5.8–7.8× ahead of
  `shadowJar` on a warm daemon, and 173 ms against `bootJar`'s 512 ms on
  `bookmarks` (3.0×).
- **Cold builds.** 424 ms–1.67 s against 1.62–3.24 s for Maven and
  12–15 s for Gradle, by more than in the third pass.
- **Tests.** `test` beats the warm daemon by 1.4–1.7× on `wordstats`,
  `bookmarks` and `gen-300`, and now on `gen-ws` too: 574 ms against 668 ms
  for Gradle CC and 744 ms for `--parallel`, both of which it lost to in
  the third pass. `test-edit` in the workspace is 540 ms against 3.04 s for
  Maven.
- **API changes on small projects.** `abi` beats every Gradle column on
  `wordstats` (364 ms against 369–389 ms) and `bookmarks` (406 ms against
  444–461 ms), where Gradle CC won the third pass.
- **Against what CI sees.** Against `gradle --no-daemon` jrs wins every
  cell, and against Maven every cell but one: `gen-ws`'s `clean`, 2.35 s
  against 1.80 s, a loss again after the third pass's win.

### Where jrs can improve (0.14.0)

- **Clean builds (new).** `clean` is 1.01–2.35 s, 1.6–3.4× behind the warm
  daemon (412 ms–1.23 s), where the third pass led on `wordstats` and
  `bookmarks`, and on `gen-300` against all but Gradle CC. `javac` takes 936–1,981 ms in it,
  more than in `cold`, and `gen-ws` spends 1.1 s after its last member.
  If this is the `javac` and worker caches being written, as their timing
  suggests, the one-off moved from `cold` and `incremental` to `clean`
  rather than out of the measured builds; writing them where no command
  waits would take it out of all three. Either way it needs confirming
  first.
- **Workspaces.** `gen-ws`'s `clean` is 2.35 s against 690–768 ms for the
  daemon and 1.80 s for Maven. Its `abi` is nearly level: 511 ms against
  454–506 ms (it was 667 ms).
- **An API change on a large project**, unchanged: `abi` on `gen-2000` is
  810 ms against 576–634 ms, and on `gen-300` 493 ms against 421–434 ms.
- **Test JVM on large suites**: unchanged. `gen-2000`'s `test` (893 ms) and
  `test-edit` (1.19 s) trail the daemon by 6–11%, 699 ms and 721 ms of them
  in the test JVM.

A full run (seven measured runs and two warm-ups per cell) is still what
BENCHMARKS.md §7 asks for before these become the published report.

---

## Fifth pass: jrs 0.14.1 (2026-10-17)

Run on **2026-10-17 at 17:18 CEST** (finished 17:32) with **jrs 0.14.1**
(`56c0d97`, `benches/compare.rs`), on the same machine as the earlier
passes: Apple M4 Max (14 cores: 10 performance and 4 efficiency, 36 GiB),
macOS 27.0.1 (26A434), Corretto 25.0.2, Maven 3.9.16 and Gradle 9.1.0,
every tool from a freshly recorded local mirror (8,138 requests, none
unanswered). Every project passed the checks of BENCHMARKS.md §5.1 on all
three tools again.

The binary was built from `56c0d97` and calls itself 0.14.0: the release
commit `c79ceb3` (`Release 0.14.1`), which changes only the version in
`Cargo.toml` and `Cargo.lock`, was pulled while the run was under way, so
the report names it. What was measured is 0.14.1's source.

The settings are the earlier passes', so the five can be compared: **one
run per cell with no warm-up** (`JRS_BENCH_RUNS=1 JRS_BENCH_WARMUP=0`), the
five parity projects only
(`JRS_BENCH_PROJECTS=wordstats,bookmarks,gen-300,gen-2000,gen-ws`). The same
caveat holds: this is still not the report of BENCHMARKS.md §6, and a cell
may move by tens of percent in a full run.

What changed since `cf0d36d` that can move a number:

- `56c0d97` makes the `javac` AOT cache where no build waits on it: the
  first command that finds none trains it on a thread of its own, beside
  the downloads and the compile, and the command joins that thread last.
  Each worker cache is recorded by the first worker of a command, which
  then stops and is replaced, and a prestarted worker is primed with a
  one-class compile. The trade-off it names is that the first command on
  an empty cache, `cold` here, waits for the training: every `javac`, and
  `cold` most.
- `4dbd982` sets `[test] share-classes = true` in the generated projects'
  `jrs.toml`, so their test JVM maps the JDK's and JUnit's classes from an
  AOT cache: `test` and `test-edit` on `gen-300`, `gen-2000` and `gen-ws`.
  `wordstats` and `bookmarks` keep their own manifests.

### jrs, fourth pass against fifth

| Project | Scenario | Fourth pass | Fifth pass | Change |
| --- | --- | --- | --- | --- |
| `wordstats` | cold | 424 ms | 980 ms | +131% |
| `wordstats` | clean | 1.01 s | 340 ms | −66% |
| `wordstats` | incremental | 215 ms | 229 ms | +7% |
| `wordstats` | test-edit | 515 ms | 556 ms | +8% |
| `wordstats` | package | 89 ms | 108 ms | +21% |
| `bookmarks` | cold | 513 ms | 1.03 s | +101% |
| `bookmarks` | clean | 1.03 s | 346 ms | −66% |
| `bookmarks` | noop | 63 ms | 68 ms | +8% |
| `bookmarks` | abi | 406 ms | 431 ms | +6% |
| `bookmarks` | run | 756 ms | 802 ms | +6% |
| `bookmarks` | package | 173 ms | 211 ms | +22% |
| `gen-300` | cold | 767 ms | 1.05 s | +37% |
| `gen-300` | clean | 1.35 s | 588 ms | −56% |
| `gen-300` | noop | 66 ms | 71 ms | +8% |
| `gen-300` | test | 452 ms | 315 ms | −30% |
| `gen-300` | test-edit | 687 ms | 486 ms | −29% |
| `gen-300` | package | 190 ms | 207 ms | +9% |
| `gen-2000` | cold | 1.67 s | 1.76 s | +5% |
| `gen-2000` | clean | 2.18 s | 1.54 s | −29% |
| `gen-2000` | noop | 89 ms | 84 ms | −6% |
| `gen-2000` | incremental | 349 ms | 380 ms | +9% |
| `gen-2000` | abi | 810 ms | 895 ms | +10% |
| `gen-2000` | test | 893 ms | 763 ms | −15% |
| `gen-2000` | test-edit | 1.19 s | 1.03 s | −13% |
| `gen-ws` | cold | 1.38 s | 1.49 s | +8% |
| `gen-ws` | clean | 2.35 s | 817 ms | −65% |
| `gen-ws` | test | 574 ms | 419 ms | −27% |
| `gen-ws` | test-edit | 540 ms | 424 ms | −21% |

Every other jrs cell is within ±5% of the fourth pass. Maven's and
Gradle's are not far off it either: within about 1% at the median in every
column, and within −9% and +15% at the extremes, so the machine behaved the
same and the moves above are jrs's own.

Three groups stand out:

- **`clean` is 29–66% faster on every project**, and leads the warm daemon
  again on `wordstats`, `bookmarks` and `gen-300`. `javac` takes
  236–1,338 ms in it, against 936–1,981 ms in the fourth pass: the cost of
  writing its cache is no longer in the second command. In `gen-ws` the
  command no longer waits for its workers after the last member: it is
  77 ms above its members' critical path, where it was 1,174 ms above it.
- **`cold` is 37–131% slower on the three small projects**, the trade-off
  `56c0d97` names. Their `--timings` totals are 451, 544 and 840 ms,
  against 421, 509 and 763 ms in the fourth pass, but the commands take
  980 ms, 1.03 s and 1.05 s: the rest falls after the build, where
  `56c0d97` has the command wait for the training. On `gen-2000` and `gen-ws` the build is long enough to hide
  it (+5% and +8%).
- **`test` and `test-edit` are 13–30% faster on the generated projects**,
  with their test JVMs mapping classes from the AOT cache (`4dbd982`):
  `gen-2000`'s test JVM takes 569 ms in `test` and 522 ms in `test-edit`,
  against 699 ms and 721 ms, and `gen-ws`'s six take 285–302 ms each,
  against 429–462 ms. `wordstats` and `bookmarks`, which do not set it,
  are within 1% in `test`.

`package` is 21–22% slower on `wordstats` and `bookmarks` and 9% on
`gen-300`, giving back part of the fourth pass's unexplained gain; `abi` and
`incremental` are 6–10% slower on `bookmarks` and `gen-2000`. No commit in
the log above touches packaging or the incremental rounds, and this pass
does not explain either.

### Workspace members at once (`gen-ws`)

GRADLE_GAPS.md §2.1: each member's wall time in jrs's clean build.

| Members | Sequential (sum) | Critical path | jrs clean | Gradle --parallel | Gate |
| --- | --- | --- | --- | --- | --- |
| 6 | 832 ms | 740 ms (11% less) | 817 ms | 725 ms | does not hold |

The members took `core 204 · model 230 · store 92 · service 106 · web 100 ·
app 100` ms, about half of the fourth pass's each, and jrs's clean build is
77 ms above the critical path, where it was 1,174 ms above it. The gate no
longer holds: Gradle `--parallel` beats jrs by 11%, not by more than 20%.

### All cells

Columns as in the second pass. **Bold** marks a tool that beat jrs.

### wordstats (a few classes, Guava, JUnit)

| Scenario | jrs | Maven | Gradle | Gradle CC | Gradle no daemon |
| --- | --- | --- | --- | --- | --- |
| cold | 980 ms | 1.62 s | 11.5 s | 12.5 s | 11.6 s |
| clean | 340 ms | 1.41 s | 480 ms | 428 ms | 2.38 s |
| noop | 64 ms | 1.05 s | 373 ms | 326 ms | 2.07 s |
| incremental | 229 ms | 1.27 s | 416 ms | 383 ms | 2.32 s |
| abi | 381 ms | 1.30 s | 401 ms | **374 ms** | 2.45 s |
| test | 361 ms | 1.38 s | 598 ms | 627 ms | 2.47 s |
| test-edit | 556 ms | 1.65 s | 662 ms | 698 ms | 2.69 s |
| run | 126 ms | 1.14 s | 426 ms | 445 ms | 2.10 s |
| package | 108 ms | 1.72 s | 614 ms | 669 ms | 2.42 s |

### bookmarks (Spring Boot 4.1)

| Scenario | jrs | Maven | Gradle | Gradle CC | Gradle no daemon |
| --- | --- | --- | --- | --- | --- |
| cold | 1.03 s | 2.08 s | 13.4 s | 13.3 s | 12.9 s |
| clean | 346 ms | 1.58 s | 656 ms | 678 ms | 2.68 s |
| noop | 68 ms | 1.13 s | 453 ms | 344 ms | 2.36 s |
| incremental | 261 ms | 1.39 s | 493 ms | 388 ms | 2.64 s |
| abi | 431 ms | 1.42 s | 464 ms | **403 ms** | 2.60 s |
| test | 1.08 s | 2.44 s | 1.63 s | 1.54 s | 3.83 s |
| test-edit | 1.30 s | 2.75 s | 1.67 s | 1.79 s | 4.02 s |
| run (to HTTP 200) | 802 ms | 2.15 s | 1.01 s | 1.08 s | 2.91 s |
| package | 211 ms | 1.66 s | 514 ms | 586 ms | 2.46 s |

### gen-300 (300 classes, 30 dependencies)

| Scenario | jrs | Maven | Gradle | Gradle CC | Gradle no daemon |
| --- | --- | --- | --- | --- | --- |
| cold | 1.05 s | 2.20 s | 12.7 s | 12.7 s | 12.5 s |
| clean | 588 ms | 1.76 s | 616 ms | 601 ms | 2.80 s |
| noop | 71 ms | 1.15 s | 372 ms | 323 ms | 2.24 s |
| incremental | 253 ms | 1.70 s | 440 ms | 413 ms | 2.42 s |
| abi | 517 ms | 1.68 s | **434 ms** | **415 ms** | 2.52 s |
| test | 315 ms | 1.45 s | 722 ms | 623 ms | 2.47 s |
| test-edit | 486 ms | 2.12 s | 698 ms | 766 ms | 2.81 s |
| run | 132 ms | 1.23 s | 415 ms | 454 ms | 2.18 s |
| package | 207 ms | 2.47 s | 1.56 s | 1.53 s | 3.34 s |

### gen-2000 (2000 classes, 60 dependencies)

| Scenario | jrs | Maven | Gradle | Gradle CC | Gradle no daemon |
| --- | --- | --- | --- | --- | --- |
| cold | 1.76 s | 3.27 s | 14.7 s | 14.9 s | 14.5 s |
| clean | 1.54 s | 2.59 s | **1.27 s** | **1.21 s** | 3.78 s |
| noop | 84 ms | 1.21 s | 363 ms | 319 ms | 2.17 s |
| incremental | 380 ms | 2.74 s | 578 ms | 558 ms | 2.81 s |
| abi | 895 ms | 2.68 s | **638 ms** | **595 ms** | 2.96 s |
| test | 763 ms | 1.86 s | 879 ms | 799 ms | 2.83 s |
| test-edit | 1.03 s | 3.54 s | 1.07 s | 1.10 s | 3.46 s |
| run | 226 ms | 1.28 s | 494 ms | 539 ms | 2.28 s |
| package | 356 ms | 3.52 s | 2.53 s | 2.62 s | 4.46 s |

### gen-ws (a six-member workspace, 300 classes)

| Scenario | jrs | Maven | Gradle | Gradle CC | Gradle no daemon | Gradle --parallel |
| --- | --- | --- | --- | --- | --- | --- |
| cold | 1.49 s | 2.28 s | 14.2 s | 14.3 s | 13.8 s | 13.9 s |
| clean | 817 ms | 1.73 s | **746 ms** | **664 ms** | 2.99 s | **725 ms** |
| noop | 82 ms | 1.14 s | 394 ms | 339 ms | 2.25 s | 402 ms |
| incremental | 210 ms | 1.42 s | 433 ms | 370 ms | 2.42 s | 417 ms |
| abi | 512 ms | 1.77 s | **504 ms** | **422 ms** | 2.64 s | **474 ms** |
| test | 419 ms | 2.42 s | 1.79 s | 672 ms | 3.77 s | 735 ms |
| test-edit | 424 ms | 2.82 s | 658 ms | 732 ms | 2.92 s | 658 ms |
| run | not timed (BENCHMARKS.md §11) | | | | | |
| package | 246 ms | 3.26 s | 1.48 s | 1.52 s | 3.45 s | 1.49 s |

### Where jrs is better (0.14.1)

- **Start-up bound work.** No-op builds are 64–84 ms on every project,
  against 319–453 ms for a warm Gradle daemon (4–7×), 1.05–1.21 s for Maven
  (14–17×) and 2.07–2.36 s for Gradle without a daemon (26–35×). `run`
  takes 126–226 ms to first output, and 802 ms to Spring Boot's first HTTP
  answer.
- **One-file edits.** `incremental` is 210–380 ms on all five projects,
  1.5–1.8× ahead of the faster warm-daemon column (370–558 ms) and 5–7×
  ahead of Maven.
- **Tests, on every project now.** `test` beats every warm-daemon column on
  all five: by 1.4–2.3× on `wordstats`, `bookmarks` and `gen-300`, on
  `gen-2000` with 763 ms against 799–879 ms, where it trailed in the fourth
  pass, and on `gen-ws` with 419 ms against 672 ms for Gradle CC and 735 ms
  for `--parallel`. `test-edit` on `gen-2000` is 1.03 s against 1.07–1.10 s,
  and in the workspace 424 ms against 2.82 s for Maven.
- **Clean builds of small projects, again.** `clean` is 340 ms on
  `wordstats` against 428–480 ms for the daemon, 346 ms on `bookmarks`
  against 656–678 ms (1.9–2.0×), and 588 ms on `gen-300` against
  601–616 ms. In the fourth pass it trailed on all three.
- **Fat jars, Spring Boot's included.** 108–356 ms, 5.7–7.5× ahead of
  `shadowJar` on a warm daemon, and 211 ms against `bootJar`'s 514 ms on
  `bookmarks` (2.4×).
- **Cold builds**, by less than in the fourth pass: 980 ms–1.76 s against
  1.62–3.27 s for Maven (1.5–2.1×) and 11.5–14.9 s for Gradle.
- **Against what CI sees.** Against `gradle --no-daemon` and against Maven
  jrs wins every cell, `gen-ws`'s `clean` included: 817 ms against 1.73 s
  for Maven, which it lost in the fourth pass.

### Where jrs can improve (0.14.1)

- **Cold builds of small projects (new).** `cold` is 980 ms–1.05 s on
  `wordstats`, `bookmarks` and `gen-300`, 1.4–2.3× its fourth-pass time,
  since the first command on an empty cache waits for the `javac` training
  after its own build. It still leads Maven by 1.5–2.1×; a training that
  finished with the build, or one the command did not wait for, would give
  the lead back.
- **Clean builds of a large project or a workspace.** `clean` on
  `gen-2000` is 1.54 s against 1.21–1.27 s for the daemon, nearly all of it
  `javac`, and on `gen-ws` 817 ms against 664–746 ms, 77 ms above its
  members' critical path. Both are much closer than in the fourth pass
  (2.18 s and 2.35 s).
- **An API change.** `abi` on `gen-2000` is 895 ms against 595–638 ms, on
  `gen-300` 517 ms against 415–434 ms, and on `gen-ws` 512 ms against
  422–504 ms. On `wordstats` (381 ms against 374 ms) and `bookmarks`
  (431 ms against 403 ms) Gradle CC wins it again, by 2% and 7%, where the
  fourth pass led.

A full run (seven measured runs and two warm-ups per cell) is still what
BENCHMARKS.md §7 asks for before these become the published report.

---

## Sixth pass: jrs 0.14.2 (2026-10-17)

Run on **2026-10-17 at 19:03 CEST** (finished 19:17) with **jrs 0.14.2**
(`6f57634`, `benches/compare.rs`), on the same machine as the earlier
passes: Apple M4 Max (14 cores: 10 performance and 4 efficiency, 36 GiB),
macOS 27.0.1 (26A434), Corretto 25.0.2, Maven 3.9.16 and Gradle 9.1.0,
every tool from a freshly recorded local mirror (8,146 requests, none
unanswered). Every project passed the checks of BENCHMARKS.md §5.1 on all
three tools again.

The settings are the earlier passes', so the six can be compared: **one
run per cell with no warm-up** (`JRS_BENCH_RUNS=1 JRS_BENCH_WARMUP=0`), the
five parity projects only
(`JRS_BENCH_PROJECTS=wordstats,bookmarks,gen-300,gen-2000,gen-ws`). The same
caveat holds: this is still not the report of BENCHMARKS.md §6, and a cell
may move by tens of percent in a full run.

What changed since `56c0d97` that can move a number:

- `7d24fb5` trains the `javac` worker's AOT cache beside a single-module
  build, on a thread the command joins at its end, where before only
  `jrs fetch` and `jrs jdk install` made it in a single-module project. A
  unit sends its later rounds of `javac` to a worker only once that cache
  exists, so it is aimed at `abi`, which is two rounds. The trade-off it
  names is `cold`, where the command waits for the worker's training.

`c79ceb3` and `6f57634` change only the version.

### jrs, fifth pass against sixth

| Project | Scenario | Fifth pass | Sixth pass | Change |
| --- | --- | --- | --- | --- |
| `wordstats` | cold | 980 ms | 1.30 s | +33% |
| `wordstats` | clean | 340 ms | 287 ms | −16% |
| `wordstats` | incremental | 229 ms | 245 ms | +7% |
| `wordstats` | abi | 381 ms | 304 ms | −20% |
| `wordstats` | test-edit | 556 ms | 524 ms | −6% |
| `wordstats` | package | 108 ms | 219 ms | +103% |
| `bookmarks` | cold | 1.03 s | 1.34 s | +30% |
| `bookmarks` | clean | 346 ms | 282 ms | −18% |
| `bookmarks` | noop | 68 ms | 62 ms | −9% |
| `bookmarks` | abi | 431 ms | 338 ms | −22% |
| `bookmarks` | run | 802 ms | 740 ms | −8% |
| `bookmarks` | package | 211 ms | 184 ms | −13% |
| `gen-300` | cold | 1.05 s | 1.34 s | +28% |
| `gen-300` | noop | 71 ms | 64 ms | −10% |
| `gen-300` | abi | 517 ms | 424 ms | −18% |
| `gen-300` | package | 207 ms | 182 ms | −12% |
| `gen-2000` | abi | 895 ms | 817 ms | −9% |
| `gen-2000` | test | 763 ms | 720 ms | −6% |

Every other jrs cell is within ±5% of the fifth pass, `gen-ws`'s all of
them. The machine was a little faster for the others: Maven by about 7% at
the median and Gradle's columns by 1–2%, within −20% and +6% at the
extremes, with `bookmarks`'s `clean` the fastest move in Maven's column
(−20%) and Gradle's (−14%) as in jrs's. The smaller jrs moves above, in
`clean`, `noop` and `run`, are as much the machine's as jrs's.

Two groups stand out:

- **`abi` is 9–22% faster on the four single-module projects**, the aim
  of `7d24fb5`. `javac` takes 232, 255, 322 and 571 ms in it, against 312,
  347, 411 and 659 ms in the fifth pass. jrs now leads every Gradle column
  in `abi` on `wordstats` and `bookmarks`, and the warm daemon without the
  configuration cache on `gen-300`. `gen-ws`, whose workspace pool already
  had a worker, did not move (516 ms).
- **`cold` is 28–33% slower on the three small projects**, the trade-off
  `7d24fb5` names. Their `--timings` totals are 468, 569 and 855 ms,
  against 451, 544 and 840 ms in the fifth pass, but the commands take
  1.30, 1.34 and 1.34 s: 275–303 ms more than in the fifth pass falls after
  the build, where the command waits for the worker's training. On
  `gen-2000` and `gen-ws` `cold` did not move (1.76 s and 1.49 s).

`package` is 103% slower on `wordstats`, its packaging phase 157 ms
against 45 ms, while it is 12–13% faster on `bookmarks` and `gen-300`
(120 and 117 ms against 147 and 135 ms). No commit in the log above
touches packaging, and this pass does not explain either move.

### Workspace members at once (`gen-ws`)

GRADLE_GAPS.md §2.1: each member's wall time in jrs's clean build.

| Members | Sequential (sum) | Critical path | jrs clean | Gradle --parallel | Gate |
| --- | --- | --- | --- | --- | --- |
| 6 | 845 ms | 754 ms (11% less) | 826 ms | 727 ms | does not hold |

The members took `core 211 · model 231 · store 91 · service 108 · web 103 ·
app 101` ms, much as in the fifth pass, and jrs's clean build is 72 ms
above the critical path. The gate still does not hold: Gradle `--parallel`
beats jrs by 12%, not by more than 20%.

### All cells

Columns as in the second pass. **Bold** marks a tool that beat jrs.

### wordstats (a few classes, Guava, JUnit)

| Scenario | jrs | Maven | Gradle | Gradle CC | Gradle no daemon |
| --- | --- | --- | --- | --- | --- |
| cold | 1.30 s | 1.56 s | 11.2 s | 11.5 s | 11.0 s |
| clean | 287 ms | 1.19 s | 433 ms | 417 ms | 2.33 s |
| noop | 61 ms | 1.05 s | 358 ms | 312 ms | 2.12 s |
| incremental | 245 ms | 1.17 s | 392 ms | 369 ms | 2.30 s |
| abi | 304 ms | 1.26 s | 388 ms | 369 ms | 2.32 s |
| test | 354 ms | 1.24 s | 595 ms | 577 ms | 2.38 s |
| test-edit | 524 ms | 1.53 s | 645 ms | 689 ms | 2.61 s |
| run | 127 ms | 1.04 s | 429 ms | 443 ms | 2.10 s |
| package | 219 ms | 1.55 s | 606 ms | 646 ms | 2.37 s |

### bookmarks (Spring Boot 4.1)

| Scenario | jrs | Maven | Gradle | Gradle CC | Gradle no daemon |
| --- | --- | --- | --- | --- | --- |
| cold | 1.34 s | 1.94 s | 12.4 s | 12.6 s | 12.1 s |
| clean | 282 ms | 1.27 s | 565 ms | 601 ms | 2.58 s |
| noop | 62 ms | 1.02 s | 445 ms | 325 ms | 2.24 s |
| incremental | 256 ms | 1.26 s | 490 ms | 388 ms | 2.55 s |
| abi | 338 ms | 1.27 s | 447 ms | 373 ms | 2.53 s |
| test | 1.05 s | 2.29 s | 1.60 s | 1.53 s | 3.59 s |
| test-edit | 1.25 s | 2.64 s | 1.66 s | 1.72 s | 3.89 s |
| run (to HTTP 200) | 740 ms | 2.08 s | 1.00 s | 1.06 s | 2.92 s |
| package | 184 ms | 1.55 s | 521 ms | 536 ms | 2.43 s |

### gen-300 (300 classes, 30 dependencies)

| Scenario | jrs | Maven | Gradle | Gradle CC | Gradle no daemon |
| --- | --- | --- | --- | --- | --- |
| cold | 1.34 s | 1.98 s | 12.2 s | 12.6 s | 12.0 s |
| clean | 584 ms | 1.53 s | 605 ms | 598 ms | 2.70 s |
| noop | 64 ms | 964 ms | 356 ms | 311 ms | 2.05 s |
| incremental | 253 ms | 1.54 s | 413 ms | 386 ms | 2.34 s |
| abi | 424 ms | 1.54 s | 450 ms | **399 ms** | 2.43 s |
| test | 320 ms | 1.32 s | 640 ms | 609 ms | 2.42 s |
| test-edit | 505 ms | 2.00 s | 709 ms | 756 ms | 2.81 s |
| run | 127 ms | 1.08 s | 406 ms | 432 ms | 2.10 s |
| package | 182 ms | 2.32 s | 1.48 s | 1.52 s | 3.29 s |

### gen-2000 (2000 classes, 60 dependencies)

| Scenario | jrs | Maven | Gradle | Gradle CC | Gradle no daemon |
| --- | --- | --- | --- | --- | --- |
| cold | 1.76 s | 3.23 s | 14.4 s | 14.3 s | 13.8 s |
| clean | 1.48 s | 2.44 s | **1.25 s** | **1.21 s** | 3.81 s |
| noop | 82 ms | 1.09 s | 354 ms | 315 ms | 2.16 s |
| incremental | 382 ms | 2.63 s | 583 ms | 519 ms | 2.82 s |
| abi | 817 ms | 2.57 s | **622 ms** | **613 ms** | 2.94 s |
| test | 720 ms | 1.75 s | 804 ms | 802 ms | 2.79 s |
| test-edit | 1.03 s | 3.35 s | 1.05 s | 1.08 s | 3.43 s |
| run | 234 ms | 1.18 s | 501 ms | 520 ms | 2.29 s |
| package | 340 ms | 3.45 s | 2.53 s | 2.61 s | 4.44 s |

### gen-ws (a six-member workspace, 300 classes)

| Scenario | jrs | Maven | Gradle | Gradle CC | Gradle no daemon | Gradle --parallel |
| --- | --- | --- | --- | --- | --- | --- |
| cold | 1.49 s | 2.17 s | 13.9 s | 14.1 s | 13.7 s | 13.6 s |
| clean | 826 ms | 1.64 s | **780 ms** | **664 ms** | 2.98 s | **727 ms** |
| noop | 83 ms | 995 ms | 393 ms | 322 ms | 2.15 s | 389 ms |
| incremental | 205 ms | 1.30 s | 414 ms | 356 ms | 2.42 s | 407 ms |
| abi | 516 ms | 1.65 s | **488 ms** | **431 ms** | 2.59 s | **473 ms** |
| test | 438 ms | 2.29 s | 1.78 s | 673 ms | 3.72 s | 732 ms |
| test-edit | 411 ms | 2.64 s | 669 ms | 743 ms | 2.81 s | 647 ms |
| run | not timed (BENCHMARKS.md §11) | | | | | |
| package | 239 ms | 3.44 s | 1.50 s | 1.54 s | 3.42 s | 1.50 s |

### Where jrs is better (0.14.2)

- **Start-up bound work.** No-op builds are 61–83 ms on every project,
  against 311–445 ms for a warm Gradle daemon (4–7×), 964 ms–1.09 s for
  Maven (12–17×) and 2.05–2.24 s for Gradle without a daemon (26–36×).
  `run` takes 127–234 ms to first output, and 740 ms to Spring Boot's first
  HTTP answer.
- **One-file edits.** `incremental` is 205–382 ms on all five projects,
  1.4–1.7× ahead of the faster warm-daemon column (356–519 ms) and 5–7×
  ahead of Maven.
- **Tests, on every project.** `test` beats every warm-daemon column on
  all five: by 1.5–2.0× on `wordstats`, `bookmarks` and `gen-300`, on
  `gen-2000` with 720 ms against 802–804 ms, and on `gen-ws` with 438 ms
  against 673 ms for Gradle CC and 732 ms for `--parallel`. `test-edit` on
  `gen-2000` is 1.03 s against 1.05–1.08 s, and in the workspace 411 ms
  against 2.64 s for Maven.
- **An API change in a small project (new).** `abi` is 304 ms on
  `wordstats` against 369–388 ms for the daemon and 338 ms on `bookmarks`
  against 373–447 ms, where Gradle CC won both in the fifth pass.
- **Clean builds of small projects.** `clean` is 287 ms on `wordstats`
  against 417–433 ms for the daemon, 282 ms on `bookmarks` against
  565–601 ms (2.0–2.1×), and 584 ms on `gen-300` against 598–605 ms.
- **Fat jars, Spring Boot's included.** 182–340 ms, 2.8–8.1× ahead of
  `shadowJar` on a warm daemon (`wordstats` the low end, at 219 ms against
  606 ms), and 184 ms against `bootJar`'s 521 ms on `bookmarks` (2.8×).
- **Cold builds**, by less again: 1.30–1.76 s against 1.56–3.23 s for
  Maven (1.2–1.8×) and 11.0–14.4 s for Gradle.
- **Against what CI sees.** Against `gradle --no-daemon` and against Maven
  jrs wins every cell.

### Where jrs can improve (0.14.2)

- **Cold builds of small projects.** `cold` is 1.30–1.34 s on
  `wordstats`, `bookmarks` and `gen-300`, 28–33% slower than in the fifth
  pass, since the first command on an empty cache now waits for the
  worker's training as well as `javac`'s after its own build. Its lead over
  Maven there is down to 1.2–1.5× (1.56–1.98 s); a training that finished
  with the build, or one the command did not wait for, would give it back.
- **Clean builds of a large project or a workspace.** `clean` on
  `gen-2000` is 1.48 s against 1.21–1.25 s for the daemon, 1,280 ms of it
  `javac`, and on `gen-ws` 826 ms against 664–780 ms, 72 ms above its
  members' critical path.
- **An API change in a large project or a workspace.** `abi` on
  `gen-2000` is 817 ms against 613–622 ms, on `gen-ws` 516 ms against
  431–488 ms, and on `gen-300` 424 ms against Gradle CC's 399 ms (6%),
  though it now beats the daemon's 450 ms there.

A full run (seven measured runs and two warm-ups per cell) is still what
BENCHMARKS.md §7 asks for before these become the published report.

---

## Seventh pass: jrs 0.14.3 (2026-10-17)

Run on **2026-10-17 at 20:42 CEST** (finished 20:56) with **jrs 0.14.3**
(`8cb352b`, the release's code before `4d22f57` set its version;
`benches/compare.rs`), on the same machine as the earlier passes: Apple M4 Max (14 cores: 10
performance and 4 efficiency, 36 GiB), macOS 27.0.1 (26A434), Corretto
25.0.2, Maven 3.9.16 and Gradle 9.1.0, every tool from a freshly recorded
local mirror (8,146 requests, none unanswered). Every project passed the
checks of BENCHMARKS.md §5.1 on all three tools again.

The settings are the earlier passes', so the seven can be compared: **one
run per cell with no warm-up** (`JRS_BENCH_RUNS=1 JRS_BENCH_WARMUP=0`), the
five parity projects only
(`JRS_BENCH_PROJECTS=wordstats,bookmarks,gen-300,gen-2000,gen-ws`). The same
caveat holds: this is still not the report of BENCHMARKS.md §6, and a cell
may move by tens of percent in a full run.

What changed since `6f57634` that can move a number:

- `8cb352b` reads only what a round compiled into the incremental index,
  taking every other class from the index as it stands, where before each
  round walked and parsed the whole output directory; the whole-unit
  record and the API digest read their files on rayon's pool. It is aimed
  at `incremental` and `abi` on the larger projects. It also gives
  `--timings` a `compile <unit>: index` and a `compile <unit>: api digest`
  row, so `compile test (fresh)` no longer holds the digest.

### jrs, sixth pass against seventh

| Project | Scenario | Sixth pass | Seventh pass | Change |
| --- | --- | --- | --- | --- |
| `wordstats` | clean | 287 ms | 271 ms | −6% |
| `wordstats` | incremental | 245 ms | 224 ms | −9% |
| `wordstats` | package | 219 ms | 88 ms | −60% |
| `bookmarks` | cold | 1.34 s | 1.25 s | −7% |
| `bookmarks` | noop | 62 ms | 66 ms | +6% |
| `bookmarks` | incremental | 256 ms | 270 ms | +5% |
| `gen-300` | noop | 64 ms | 69 ms | +8% |
| `gen-300` | test | 320 ms | 345 ms | +8% |
| `gen-300` | package | 182 ms | 193 ms | +6% |
| `gen-2000` | clean | 1.48 s | 1.56 s | +5% |
| `gen-2000` | incremental | 382 ms | 332 ms | −13% |
| `gen-2000` | abi | 817 ms | 738 ms | −10% |
| `gen-2000` | test | 720 ms | 849 ms | +18% |
| `gen-2000` | package | 340 ms | 376 ms | +11% |
| `gen-ws` | clean | 826 ms | 944 ms | +14% |
| `gen-ws` | noop | 83 ms | 97 ms | +17% |
| `gen-ws` | incremental | 205 ms | 223 ms | +9% |
| `gen-ws` | test-edit | 411 ms | 436 ms | +6% |
| `gen-ws` | package | 239 ms | 318 ms | +33% |

Every other jrs cell is within ±5% of the sixth pass. The machine was much
where it was: Maven moved by +0.5% at the median and Gradle's columns by
+1–3%, within −15% and +17% at the extremes. `gen-ws`'s `clean` is the
largest move in Gradle CC's column (+17%), as it is among jrs's larger
ones, and Maven's `clean` on `gen-2000` moved by +14%, so jrs's +5% there
is the machine's as much as jrs's.

- **`incremental` and `abi` on `gen-2000` are 13% and 10% faster**, the aim
  of `8cb352b`. `javac` takes about what it did (183 and 567 ms, against
  186 and 571 ms), and what lies outside it fell from 192 to 144 ms in
  `incremental` and from 242 to 167 ms in `abi`; the index now takes 13
  and 26 ms of that. The smaller projects did not move in `abi`.
- **`test` on `gen-2000` is 18% slower**, 849 ms against 720 ms, and now
  behind both daemon columns (808–843 ms). The test JVM takes 611 ms
  against 523 ms, 88 ms of the 129; the API digest, `compile test
  (fresh)` and the test impact take 139 ms (74, 4 and 61) against 98 ms
  (47 and 51) before. This pass does not say how much of either is the
  machine's.
- **`gen-ws`'s `clean` is 14% slower** and every member took longer
  (below), as Gradle CC (+17%) and Maven (+8%) did in the same cell.

`package` moved again with no commit touching packaging: `wordstats`'s
packaging phase took 27 ms against 157 ms, back where the third and fourth
passes had it (88 and 89 ms for the command), while `gen-ws`'s `app` took
186 ms against 122 ms and `gen-2000`'s 293 ms against 257 ms. The sixth
pass's `wordstats` cell was the one-off; this pass does not explain the
other two.

### Workspace members at once (`gen-ws`)

GRADLE_GAPS.md §2.1: each member's wall time in jrs's clean build.

| Members | Sequential (sum) | Critical path | jrs clean | Gradle --parallel | Gate |
| --- | --- | --- | --- | --- | --- |
| 6 | 969 ms | 857 ms (12% less) | 944 ms | 778 ms | holds |

The members took `core 254 · model 250 · store 112 · service 125 · web
114 · app 114` ms, 11–43 ms more each than in the sixth pass, and jrs's
clean build is 87 ms above the critical path. The gate holds again, by a
hair: jrs's clean build takes 1.21× Gradle `--parallel`'s, where the gate
asks for more than 1.2×. In the sixth pass it was 1.14× and did not hold.

### All cells

Columns as in the second pass. **Bold** marks a tool that beat jrs.

### wordstats (a few classes, Guava, JUnit)

| Scenario | jrs | Maven | Gradle | Gradle CC | Gradle no daemon |
| --- | --- | --- | --- | --- | --- |
| cold | 1.34 s | 1.52 s | 11.2 s | 11.5 s | 11.0 s |
| clean | 271 ms | 1.17 s | 451 ms | 432 ms | 2.37 s |
| noop | 62 ms | 894 ms | 340 ms | 314 ms | 1.95 s |
| incremental | 224 ms | 1.18 s | 388 ms | 358 ms | 2.26 s |
| abi | 300 ms | 1.18 s | 386 ms | 359 ms | 2.31 s |
| test | 357 ms | 1.21 s | 607 ms | 561 ms | 2.36 s |
| test-edit | 520 ms | 1.53 s | 633 ms | 682 ms | 2.59 s |
| run | 125 ms | 1.01 s | 397 ms | 432 ms | 2.15 s |
| package | 88 ms | 1.54 s | 598 ms | 622 ms | 2.37 s |

### bookmarks (Spring Boot 4.1)

| Scenario | jrs | Maven | Gradle | Gradle CC | Gradle no daemon |
| --- | --- | --- | --- | --- | --- |
| cold | 1.25 s | 1.90 s | 12.5 s | 12.9 s | 12.5 s |
| clean | 291 ms | 1.26 s | 556 ms | 608 ms | 2.60 s |
| noop | 66 ms | 1.09 s | 476 ms | 348 ms | 2.33 s |
| incremental | 270 ms | 1.32 s | 505 ms | 391 ms | 2.56 s |
| abi | 343 ms | 1.29 s | 461 ms | 372 ms | 2.66 s |
| test | 1.09 s | 2.28 s | 1.70 s | 1.55 s | 3.64 s |
| test-edit | 1.28 s | 2.65 s | 1.63 s | 1.71 s | 3.97 s |
| run (to HTTP 200) | 759 ms | 1.97 s | 1.00 s | 1.04 s | 2.98 s |
| package | 188 ms | 1.54 s | 509 ms | 535 ms | 2.44 s |

### gen-300 (300 classes, 30 dependencies)

| Scenario | jrs | Maven | Gradle | Gradle CC | Gradle no daemon |
| --- | --- | --- | --- | --- | --- |
| cold | 1.30 s | 1.95 s | 12.2 s | 12.4 s | 12.2 s |
| clean | 583 ms | 1.62 s | 669 ms | 615 ms | 2.88 s |
| noop | 69 ms | 1.03 s | 373 ms | 347 ms | 2.09 s |
| incremental | 250 ms | 1.55 s | 424 ms | 399 ms | 2.41 s |
| abi | 424 ms | 1.56 s | 469 ms | 438 ms | 2.51 s |
| test | 345 ms | 1.33 s | 635 ms | 634 ms | 2.51 s |
| test-edit | 523 ms | 2.02 s | 697 ms | 761 ms | 2.87 s |
| run | 132 ms | 1.08 s | 425 ms | 446 ms | 2.14 s |
| package | 193 ms | 2.35 s | 1.54 s | 1.54 s | 3.41 s |

### gen-2000 (2000 classes, 60 dependencies)

| Scenario | jrs | Maven | Gradle | Gradle CC | Gradle no daemon |
| --- | --- | --- | --- | --- | --- |
| cold | 1.77 s | 3.61 s | 14.6 s | 14.8 s | 14.2 s |
| clean | 1.56 s | 2.79 s | **1.24 s** | **1.21 s** | 3.78 s |
| noop | 82 ms | 1.08 s | 357 ms | 321 ms | 2.16 s |
| incremental | 332 ms | 2.59 s | 583 ms | 535 ms | 2.81 s |
| abi | 738 ms | 2.60 s | **664 ms** | **578 ms** | 3.21 s |
| test | 849 ms | 1.75 s | **843 ms** | **808 ms** | 2.84 s |
| test-edit | 1.03 s | 3.33 s | 1.04 s | 1.07 s | 3.48 s |
| run | 232 ms | 1.16 s | 495 ms | 540 ms | 2.34 s |
| package | 376 ms | 3.47 s | 2.61 s | 2.66 s | 4.75 s |

### gen-ws (a six-member workspace, 300 classes)

| Scenario | jrs | Maven | Gradle | Gradle CC | Gradle no daemon | Gradle --parallel |
| --- | --- | --- | --- | --- | --- | --- |
| cold | 1.51 s | 2.43 s | 14.5 s | 14.4 s | 14.3 s | 14.2 s |
| clean | 944 ms | 1.77 s | **789 ms** | **777 ms** | 3.14 s | **778 ms** |
| noop | 97 ms | 1.01 s | 397 ms | 329 ms | 2.25 s | 406 ms |
| incremental | 223 ms | 1.32 s | 441 ms | 385 ms | 2.54 s | 434 ms |
| abi | 534 ms | 1.71 s | **499 ms** | **461 ms** | 2.66 s | **478 ms** |
| test | 456 ms | 2.39 s | 1.86 s | 711 ms | 3.80 s | 737 ms |
| test-edit | 436 ms | 2.70 s | 676 ms | 747 ms | 2.87 s | 645 ms |
| run | not timed (BENCHMARKS.md §11) | | | | | |
| package | 318 ms | 3.15 s | 1.48 s | 1.57 s | 3.43 s | 1.51 s |

### Where jrs is better (0.14.3)

- **Start-up bound work.** No-op builds are 62–97 ms on every project,
  against 314–476 ms for a warm Gradle daemon (3–7×), 894 ms–1.09 s for
  Maven (10–17×) and 1.95–2.33 s for Gradle without a daemon (23–35×).
  `run` takes 125–232 ms to first output, and 759 ms to Spring Boot's first
  HTTP answer.
- **One-file edits.** `incremental` is 223–332 ms on all five projects,
  1.4–1.7× ahead of the faster warm-daemon column (358–535 ms) and 5–8×
  ahead of Maven.
- **Tests, on four projects.** `test` beats every warm-daemon column on
  `wordstats`, `bookmarks` and `gen-300` by 1.4–1.8×, and on `gen-ws` with
  456 ms against 711 ms for Gradle CC and 737 ms for `--parallel`.
  `test-edit` on `gen-2000` is 1.03 s against 1.04–1.07 s, and in the
  workspace 436 ms against 2.70 s for Maven.
- **An API change in a small project.** `abi` is 300 ms on `wordstats`
  against 359–386 ms for the daemon, 343 ms on `bookmarks` against
  372–461 ms, and 424 ms on `gen-300` against 438–469 ms, where Gradle CC
  won in the sixth pass.
- **Clean builds of small projects.** `clean` is 271 ms on `wordstats`
  against 432–451 ms for the daemon, 291 ms on `bookmarks` against
  556–608 ms (1.9–2.1×), and 583 ms on `gen-300` against 615–669 ms.
- **Fat jars, Spring Boot's included.** 88–376 ms, 4.7–8.0× ahead of
  `shadowJar` on a warm daemon (`gen-ws` the low end, at 318 ms against
  1.48 s), and 188 ms against `bootJar`'s 509 ms on `bookmarks` (2.7×).
- **Cold builds**, by less: 1.25–1.77 s against 1.52–3.61 s for Maven
  (1.1–2.0×) and 11.0–14.8 s for Gradle.
- **Against what CI sees.** Against `gradle --no-daemon` and against Maven
  jrs wins every cell.

### Where jrs can improve (0.14.3)

- **Cold builds of small projects.** `cold` is 1.34 s on `wordstats`,
  1.25 s on `bookmarks` and 1.30 s on `gen-300`, against `--timings`
  totals of 508, 565 and 850 ms: the command still waits for the `javac`
  worker's training after its build. Its lead over Maven there is
  1.1–1.5× (1.52–1.95 s).
- **Clean builds of a large project or a workspace.** `clean` on
  `gen-2000` is 1.56 s against 1.21–1.24 s for the daemon, 1,336 ms of it
  `javac`, and on `gen-ws` 944 ms against 777–789 ms, 87 ms above its
  members' critical path.
- **An API change in a large project or a workspace.** `abi` on
  `gen-2000` is 738 ms against 578–664 ms, closer than in the sixth pass,
  and on `gen-ws` 534 ms against 461–499 ms.
- **The whole suite of a large project (new).** `test` on `gen-2000` is
  849 ms against 808–843 ms, 611 ms of it the test JVM.

A full run (seven measured runs and two warm-ups per cell) is still what
BENCHMARKS.md §7 asks for before these become the published report.

---

## Eighth pass: jrs 0.14.4 (2026-10-18)

Run on **2026-10-18 at 09:58 CEST** (finished 10:11) with **jrs 0.14.4**
(`246f30d`, the release's code with only documentation and a workflow
change after `4d75137` set its version; `benches/compare.rs`). The report
names `75ac95a`: `9a74f96` and `75ac95a` were committed while it ran, and
touch only `README.md` and `website/index.html`, so the binary measured is
`246f30d`'s. The same machine as the earlier passes: Apple M4 Max (14
cores: 10 performance and 4 efficiency, 36 GiB), macOS 27.0.1 (26A434),
Corretto 25.0.2, Maven 3.9.16 and Gradle 9.1.0, every tool from a freshly
recorded local mirror (8,152 requests, none unanswered). Every project
passed the checks of BENCHMARKS.md §5.1 on all three tools again.

The settings are the earlier passes', so the eight can be compared: **one
run per cell with no warm-up** (`JRS_BENCH_RUNS=1 JRS_BENCH_WARMUP=0`), the
five parity projects only
(`JRS_BENCH_PROJECTS=wordstats,bookmarks,gen-300,gen-2000,gen-ws`). The same
caveat holds: this is still not the report of BENCHMARKS.md §6, and a cell
may move by tens of percent in a full run.

What changed since `8cb352b` that can move a number:

- `7ad988a` prestarts a warm `javac` worker for each workspace member that
  can compile at once (the widest level of the member graph, up to
  `--jobs`), all started as the graph is read: the first primed with one
  class, the others with the embedded training workload. The pool hands a
  compile the warmest idle worker, and the ready member with the most
  members still to build after it starts first. It is aimed at `gen-ws`'s
  `clean`, where `model` took 249 ms on a cold JVM of its own. It changes
  nothing outside a workspace.

### jrs, seventh pass against eighth

| Project | Scenario | Seventh pass | Eighth pass | Change |
| --- | --- | --- | --- | --- |
| `wordstats` | cold | 1.34 s | 1.26 s | −6% |
| `bookmarks` | noop | 66 ms | 61 ms | −8% |
| `gen-300` | cold | 1.30 s | 1.37 s | +5% |
| `gen-300` | noop | 69 ms | 64 ms | −7% |
| `gen-300` | test | 345 ms | 318 ms | −8% |
| `gen-300` | test-edit | 523 ms | 499 ms | −5% |
| `gen-2000` | test | 849 ms | 713 ms | −16% |
| `gen-2000` | package | 376 ms | 411 ms | +9% |
| `gen-ws` | clean | 944 ms | 713 ms | −24% |
| `gen-ws` | abi | 534 ms | 465 ms | −13% |
| `gen-ws` | test | 456 ms | 536 ms | +18% |
| `gen-ws` | package | 318 ms | 247 ms | −22% |

Every other jrs cell is within ±5% of the seventh pass. The machine was
where it was: every column's median moved by less than 2% (Maven +0.6%,
Gradle's −0.4% to −1.9%), within −16% and +17% at the extremes. The −16% is
Gradle CC on `gen-ws`'s `clean`, the cell where jrs moved most, so some of
jrs's −24% there may be the machine's; the daemon (−3%) and `--parallel`
(−7%) moved less.

- **`gen-ws`'s `clean` is 24% faster**, 713 ms against 944 ms, the aim of
  `7ad988a`. `model` took 119 ms against 250 ms, and every other member
  less than before (below). jrs now beats the warm daemon (764 ms) and
  `--parallel` (722 ms) there; only Gradle CC is ahead, at 655 ms.
- **`gen-ws`'s `abi` is 13% faster**, 465 ms against 534 ms: `model` took
  79 ms against 141 ms and `app` 75 against 91. jrs is now ahead of the
  daemon (478 ms) and `--parallel` (472 ms), and behind Gradle CC's 446 ms.
- **`gen-2000`'s `test` is back**, 713 ms against 849 ms, where the sixth
  pass had it (720 ms): the test JVM took 520 ms against 611 ms, and the API
  digest and the test impact 39 and 52 ms against 74 and 61. No commit
  touched testing outside a workspace, so the seventh pass's cell was the
  one-off. jrs is ahead of both daemon columns there again (806–899 ms).
- **`gen-ws`'s `test` is 18% slower**, 536 ms against 456 ms, while Maven
  and Gradle's columns there moved by −3% to +1%. Each of the six test JVMs
  took 359–393 ms against 299–322 ms. This pass does not say why. A lead to
  check: `test` also runs `warm_up` in `cli/members.rs`, which since
  `7ad988a` starts the extra workers on the training workload as the graph
  is read, beside six test JVMs and with nothing for them to compile.

`package` moved again with no commit touching packaging: `gen-ws`'s `app`
took 135 ms against 202 ms, back towards the sixth pass's 239 ms for the
command, and `gen-2000`'s packaging phase 328 ms against 293 ms.

### Workspace members at once (`gen-ws`)

GRADLE_GAPS.md §2.1: each member's wall time in jrs's clean build.

| Members | Sequential (sum) | Critical path | jrs clean | Gradle --parallel | Gate |
| --- | --- | --- | --- | --- | --- |
| 6 | 727 ms | 631 ms (13% less) | 713 ms | 722 ms | does not hold |

The members took `core 222 · model 119 · store 96 · service 94 · web 95 ·
app 101` ms, against `254 · 250 · 112 · 125 · 114 · 114` in the seventh
pass, and jrs's clean build is 82 ms above the critical path. The gate does
not hold: the critical path is 13% below the sum, where the gate asks for
25%, and Gradle `--parallel` no longer beats jrs at all (722 ms against
713 ms), where in the seventh pass jrs took 1.21× its time.

### All cells

Columns as in the second pass. **Bold** marks a tool that beat jrs.

### wordstats (a few classes, Guava, JUnit)

| Scenario | jrs | Maven | Gradle | Gradle CC | Gradle no daemon |
| --- | --- | --- | --- | --- | --- |
| cold | 1.26 s | 1.63 s | 11.2 s | 11.4 s | 11.2 s |
| clean | 283 ms | 1.18 s | 472 ms | 406 ms | 2.39 s |
| noop | 61 ms | 908 ms | 347 ms | 323 ms | 2.05 s |
| incremental | 226 ms | 1.20 s | 386 ms | 362 ms | 2.28 s |
| abi | 301 ms | 1.17 s | 383 ms | 359 ms | 2.25 s |
| test | 361 ms | 1.22 s | 591 ms | 586 ms | 2.43 s |
| test-edit | 523 ms | 1.54 s | 636 ms | 682 ms | 2.58 s |
| run | 126 ms | 1.15 s | 437 ms | 470 ms | 2.16 s |
| package | 90 ms | 1.53 s | 609 ms | 636 ms | 2.44 s |

### bookmarks (Spring Boot 4.1)

| Scenario | jrs | Maven | Gradle | Gradle CC | Gradle no daemon |
| --- | --- | --- | --- | --- | --- |
| cold | 1.25 s | 1.96 s | 12.4 s | 12.8 s | 12.5 s |
| clean | 289 ms | 1.38 s | 584 ms | 602 ms | 2.60 s |
| noop | 61 ms | 1.05 s | 454 ms | 328 ms | 2.44 s |
| incremental | 258 ms | 1.32 s | 483 ms | 378 ms | 2.51 s |
| abi | 337 ms | 1.34 s | 448 ms | 370 ms | 2.56 s |
| test | 1.10 s | 2.50 s | 1.61 s | 1.52 s | 3.60 s |
| test-edit | 1.25 s | 2.68 s | 1.64 s | 1.71 s | 3.91 s |
| run (to HTTP 200) | 760 ms | 2.02 s | 1.00 s | 1.04 s | 2.90 s |
| package | 186 ms | 1.66 s | 533 ms | 543 ms | 2.47 s |

### gen-300 (300 classes, 30 dependencies)

| Scenario | jrs | Maven | Gradle | Gradle CC | Gradle no daemon |
| --- | --- | --- | --- | --- | --- |
| cold | 1.37 s | 2.10 s | 12.4 s | 12.9 s | 12.2 s |
| clean | 603 ms | 1.89 s | 640 ms | 612 ms | 2.80 s |
| noop | 64 ms | 980 ms | 348 ms | 301 ms | 2.05 s |
| incremental | 251 ms | 1.63 s | 436 ms | 397 ms | 2.36 s |
| abi | 422 ms | 1.57 s | 444 ms | **409 ms** | 2.45 s |
| test | 318 ms | 1.33 s | 641 ms | 602 ms | 2.45 s |
| test-edit | 499 ms | 1.96 s | 682 ms | 740 ms | 2.78 s |
| run | 132 ms | 1.05 s | 396 ms | 446 ms | 2.10 s |
| package | 191 ms | 2.33 s | 1.47 s | 1.55 s | 3.34 s |

### gen-2000 (2000 classes, 60 dependencies)

| Scenario | jrs | Maven | Gradle | Gradle CC | Gradle no daemon |
| --- | --- | --- | --- | --- | --- |
| cold | 1.78 s | 3.10 s | 14.2 s | 14.6 s | 14.1 s |
| clean | 1.54 s | 2.46 s | **1.22 s** | **1.19 s** | 3.87 s |
| noop | 85 ms | 1.13 s | 374 ms | 333 ms | 2.32 s |
| incremental | 335 ms | 2.58 s | 570 ms | 542 ms | 2.80 s |
| abi | 737 ms | 2.68 s | **629 ms** | **588 ms** | 2.92 s |
| test | 713 ms | 1.75 s | 899 ms | 806 ms | 2.79 s |
| test-edit | 988 ms | 3.35 s | 1.01 s | 1.08 s | 3.47 s |
| run | 226 ms | 1.20 s | 498 ms | 545 ms | 2.30 s |
| package | 411 ms | 3.51 s | 2.59 s | 2.65 s | 4.56 s |

### gen-ws (a six-member workspace, 300 classes)

| Scenario | jrs | Maven | Gradle | Gradle CC | Gradle no daemon | Gradle --parallel |
| --- | --- | --- | --- | --- | --- | --- |
| cold | 1.45 s | 2.36 s | 13.8 s | 14.1 s | 13.7 s | 13.6 s |
| clean | 713 ms | 1.66 s | 764 ms | **655 ms** | 2.98 s | 722 ms |
| noop | 97 ms | 1.02 s | 398 ms | 341 ms | 2.19 s | 396 ms |
| incremental | 231 ms | 1.31 s | 426 ms | 370 ms | 2.47 s | 418 ms |
| abi | 465 ms | 1.66 s | 478 ms | **446 ms** | 2.63 s | 472 ms |
| test | 536 ms | 2.39 s | 1.81 s | 691 ms | 3.76 s | 743 ms |
| test-edit | 452 ms | 2.67 s | 674 ms | 744 ms | 2.88 s | 662 ms |
| run | not timed (BENCHMARKS.md §11) | | | | | |
| package | 247 ms | 3.08 s | 1.50 s | 1.54 s | 3.41 s | 1.50 s |

### Where jrs is better (0.14.4)

- **Start-up bound work.** No-op builds are 61–97 ms on every project,
  against 301–454 ms for a warm Gradle daemon (3.5–7.4×), 908 ms–1.13 s for
  Maven (10–17×) and 2.05–2.44 s for Gradle without a daemon (23–40×).
  `run` takes 126–226 ms to first output, and 760 ms to Spring Boot's first
  HTTP answer.
- **One-file edits.** `incremental` is 226–335 ms on all five projects,
  1.5–1.6× ahead of the faster warm-daemon column (362–542 ms) and 5–8×
  ahead of Maven.
- **Tests, on all five projects.** `test` beats every warm-daemon column on
  `wordstats`, `bookmarks` and `gen-300` by 1.4–1.9×, on `gen-2000` with
  713 ms against 806–899 ms, and on `gen-ws` with 536 ms against 691 ms for
  Gradle CC and 743 ms for `--parallel`. `test-edit` on `gen-2000` is
  988 ms against 1.01–1.08 s, and in the workspace 452 ms against 2.67 s
  for Maven.
- **An API change in a small project.** `abi` is 301 ms on `wordstats`
  against 359–383 ms for the daemon and 337 ms on `bookmarks` against
  370–448 ms. `gen-300` left this list (below).
- **Clean builds of small projects.** `clean` is 283 ms on `wordstats`
  against 406–472 ms for the daemon, 289 ms on `bookmarks` against
  584–602 ms (2.0–2.1×), and 603 ms on `gen-300` against 612–640 ms.
- **Fat jars, Spring Boot's included.** 90–411 ms, 6.1–7.7× ahead of
  `shadowJar` on a warm daemon (`gen-ws` the low end, at 247 ms against
  1.50 s), and 186 ms against `bootJar`'s 533 ms on `bookmarks` (2.9×).
- **Cold builds**, by less: 1.25–1.78 s against 1.63–3.10 s for Maven
  (1.3–1.7×) and 11.2–14.6 s for Gradle.
- **Against what CI sees.** Against `gradle --no-daemon` and against Maven
  jrs wins every cell.

### Where jrs can improve (0.14.4)

- **Cold builds of small projects.** `cold` is 1.26 s on `wordstats`,
  1.25 s on `bookmarks` and 1.37 s on `gen-300`, against `--timings`
  totals of 465, 587 and 865 ms: the command still waits for the `javac`
  worker's training after its build. Its lead over Maven there is
  1.3–1.6× (1.63–2.10 s).
- **Clean builds of a large project or a workspace.** `clean` on
  `gen-2000` is 1.54 s against 1.19–1.22 s for the daemon, 1,336 ms of it
  `javac`, and on `gen-ws` 713 ms against Gradle CC's 655 ms, 82 ms above
  its members' critical path, though now ahead of the plain daemon and
  `--parallel`.
- **An API change in a large project or a workspace, and now in
  `gen-300`.** `abi` on `gen-2000` is 737 ms against 588–629 ms, on
  `gen-ws` 465 ms against Gradle CC's 446 ms, and on `gen-300` 422 ms
  against Gradle CC's 409 ms, where jrs was ahead in the seventh pass.
- **The workspace's test suite (new).** `test` on `gen-ws` is still ahead
  of every Gradle column, but 18% slower than in the seventh pass, with
  every member's test JVM slower.

A full run (seven measured runs and two warm-ups per cell) is still what
BENCHMARKS.md §7 asks for before these become the published report.

---

## Ninth pass: jrs 0.14.5 (2026-10-18)

Run on **2026-10-18 at 13:10 CEST** (finished 13:24) with **jrs 0.14.5**
(`2934af2`, the release commit, whose only code change since the eighth
pass is `50f1854`; `benches/compare.rs`). The same machine as the earlier
passes: Apple M4 Max (14 cores: 10 performance and 4 efficiency, 36 GiB),
macOS 27.0.1 (26A434), Corretto 25.0.2, Maven 3.9.16 and Gradle 9.1.0,
every tool from a freshly recorded local mirror (8,156 requests, none
unanswered). Every project passed the checks of BENCHMARKS.md §5.1 on all
three tools again.

The settings are the earlier passes', so the nine can be compared: **one
run per cell with no warm-up** (`JRS_BENCH_RUNS=1 JRS_BENCH_WARMUP=0`), the
five parity projects only
(`JRS_BENCH_PROJECTS=wordstats,bookmarks,gen-300,gen-2000,gen-ws`). The same
caveat holds: this is still not the report of BENCHMARKS.md §6, and a cell
may move by tens of percent in a full run.

What changed since `246f30d` that can move a number:

- `50f1854` trains `javac`'s own AOT cache on 300 generated classes, in
  packages of 20 and four shapes, beside the hand-written training file,
  so the cache carries JEP 515's method profiles for a large compile. It
  is written by the first command's training, `jrs fetch` and `jrs jdk
  install`; the `javac` worker's cache and its prime keep the hand-written
  part alone. The commit measured `gen-2000`'s `clean` going from 1.51 to
  1.33 s and `gen-300`'s from 599 to 552 ms (three runs, one warm-up), at
  the price of `cold`, which waits for a training that now records for
  1.6 s.

### jrs, eighth pass against ninth

| Project | Scenario | Eighth pass | Ninth pass | Change |
| --- | --- | --- | --- | --- |
| `wordstats` | cold | 1.26 s | 2.01 s | +60% |
| `wordstats` | test-edit | 523 ms | 575 ms | +10% |
| `bookmarks` | cold | 1.25 s | 2.07 s | +66% |
| `bookmarks` | package | 186 ms | 173 ms | −7% |
| `gen-300` | cold | 1.37 s | 2.13 s | +55% |
| `gen-300` | clean | 603 ms | 659 ms | +9% |
| `gen-300` | package | 191 ms | 298 ms | +56% |
| `gen-2000` | cold | 1.78 s | 2.44 s | +37% |
| `gen-2000` | test | 713 ms | 774 ms | +9% |
| `gen-2000` | test-edit | 988 ms | 1.09 s | +10% |
| `gen-2000` | run | 226 ms | 245 ms | +8% |
| `gen-2000` | package | 411 ms | 479 ms | +17% |
| `gen-ws` | cold | 1.45 s | 2.42 s | +67% |
| `gen-ws` | clean | 713 ms | 839 ms | +18% |
| `gen-ws` | test | 536 ms | 740 ms | +38% |
| `gen-ws` | test-edit | 452 ms | 535 ms | +18% |
| `gen-ws` | package | 247 ms | 404 ms | +64% |

Every other jrs cell is within ±5% of the eighth pass. The machine was
where it was: every column's median moved by less than 4% (jrs +1.5%,
Maven −2.6%, Gradle's +0.5% to +3.4%). The exceptions are single cells:
Maven's `test` and `test-edit` on `gen-2000` (+50% and +32%), the daemon's
`clean` there (+17%), and `gen-ws`'s `clean`, where every warm Gradle column
moved by +8% to +14%, so some of jrs's +18% there is the machine's.

- **`cold` is 0.66–0.97 s slower on every project**, the price `50f1854`
  names: 2.01–2.44 s against 1.25–1.78 s. The `--timings` totals barely
  moved (464 ms on `wordstats` against 465, 617 against 587 on
  `bookmarks`, 873 against 865 on `gen-300`): the time is the command
  waiting for the training after its build. **Maven now beats jrs** on
  `wordstats` (1.52 s), `bookmarks` (1.90 s) and `gen-300` (1.92 s), and is
  4% behind on `gen-ws` (2.51 s against 2.42 s).
- **The `clean` gain the commit measured does not show.** On `gen-2000`
  the `javac` phase took 1,221 ms against 1,336 ms, but the command took
  1.55 s against 1.54 s, its `--timings` total 1,538 ms against 1,537: the
  115 ms went outside the named phases, and this pass does not say where.
  On `gen-300` the `javac` phase took 509 ms against 500, and Gradle CC
  (613 ms) is now ahead of jrs's 659 ms.
- **`gen-ws`'s `clean` is 18% slower**, 839 ms against 713 ms: every member
  took longer (below), `core` 271 ms against 222. jrs is now behind all
  three warm Gradle columns there (723–824 ms), where the eighth pass had
  it ahead of the daemon and `--parallel`.
- **`gen-ws`'s `test` is 38% slower**, 740 ms against 536 ms, and Gradle
  CC (693 ms) is now ahead of it. Each member's test JVM took 538–600 ms,
  against 359–393 ms in the eighth pass and 299–322 ms in the seventh. The
  eighth pass's lead, the workers `warm_up` starts beside six test JVMs,
  is still unchecked.
- **`package` moved again** with no commit touching packaging: the
  packaging phase took 232 ms on `gen-300` against 127, 396 ms on
  `gen-2000` against 328, and 232 ms in `gen-ws`'s `app` against 135.
  `bookmarks`'s took 111 ms against 124.

`gen-2000`'s `test-edit` (1.09 s) is now 4% behind the daemon's 1.05 s,
with the test JVM at 591 ms against 509 ms.

### Workspace members at once (`gen-ws`)

GRADLE_GAPS.md §2.1: each member's wall time in jrs's clean build.

| Members | Sequential (sum) | Critical path | jrs clean | Gradle --parallel | Gate |
| --- | --- | --- | --- | --- | --- |
| 6 | 853 ms | 744 ms (13% less) | 839 ms | 824 ms | does not hold |

The members took `core 271 · model 139 · store 109 · service 106 · web 113 ·
app 115` ms, against `222 · 119 · 96 · 94 · 95 · 101` in the eighth pass,
and jrs's clean build is 95 ms above the critical path. The gate does not
hold: the critical path is 13% below the sum, where the gate asks for 25%,
and Gradle `--parallel` beats jrs by 2% (824 ms against 839 ms), where the
gate asks for 20%.

### All cells

Columns as in the second pass. **Bold** marks a tool that beat jrs.

### wordstats (a few classes, Guava, JUnit)

| Scenario | jrs | Maven | Gradle | Gradle CC | Gradle no daemon |
| --- | --- | --- | --- | --- | --- |
| cold | 2.01 s | **1.52 s** | 11.3 s | 11.8 s | 11.4 s |
| clean | 292 ms | 1.16 s | 451 ms | 425 ms | 2.36 s |
| noop | 61 ms | 868 ms | 344 ms | 321 ms | 2.02 s |
| incremental | 224 ms | 1.16 s | 372 ms | 372 ms | 2.27 s |
| abi | 303 ms | 1.15 s | 379 ms | 374 ms | 2.32 s |
| test | 355 ms | 1.18 s | 611 ms | 573 ms | 2.40 s |
| test-edit | 575 ms | 1.57 s | 619 ms | 666 ms | 2.57 s |
| run | 125 ms | 976 ms | 410 ms | 451 ms | 2.16 s |
| package | 90 ms | 1.49 s | 613 ms | 653 ms | 2.38 s |

### bookmarks (Spring Boot 4.1)

| Scenario | jrs | Maven | Gradle | Gradle CC | Gradle no daemon |
| --- | --- | --- | --- | --- | --- |
| cold | 2.07 s | **1.90 s** | 12.3 s | 12.5 s | 12.1 s |
| clean | 282 ms | 1.22 s | 574 ms | 615 ms | 2.60 s |
| noop | 62 ms | 967 ms | 432 ms | 333 ms | 2.30 s |
| incremental | 258 ms | 1.24 s | 507 ms | 388 ms | 2.59 s |
| abi | 343 ms | 1.23 s | 463 ms | 385 ms | 2.55 s |
| test | 1.06 s | 2.22 s | 1.61 s | 1.51 s | 3.60 s |
| test-edit | 1.27 s | 2.61 s | 1.64 s | 1.73 s | 3.94 s |
| run (to HTTP 200) | 745 ms | 1.93 s | 1.00 s | 1.04 s | 2.95 s |
| package | 173 ms | 1.56 s | 496 ms | 550 ms | 2.47 s |

### gen-300 (300 classes, 30 dependencies)

| Scenario | jrs | Maven | Gradle | Gradle CC | Gradle no daemon |
| --- | --- | --- | --- | --- | --- |
| cold | 2.13 s | **1.92 s** | 12.2 s | 12.4 s | 12.2 s |
| clean | 659 ms | 1.67 s | 682 ms | **613 ms** | 2.74 s |
| noop | 65 ms | 925 ms | 362 ms | 323 ms | 2.09 s |
| incremental | 252 ms | 1.51 s | 426 ms | 398 ms | 2.41 s |
| abi | 428 ms | 1.55 s | 450 ms | **420 ms** | 2.53 s |
| test | 318 ms | 1.26 s | 644 ms | 593 ms | 2.44 s |
| test-edit | 493 ms | 1.90 s | 688 ms | 734 ms | 2.85 s |
| run | 130 ms | 1.04 s | 419 ms | 440 ms | 2.12 s |
| package | 298 ms | 2.31 s | 1.48 s | 1.56 s | 3.35 s |

### gen-2000 (2000 classes, 60 dependencies)

| Scenario | jrs | Maven | Gradle | Gradle CC | Gradle no daemon |
| --- | --- | --- | --- | --- | --- |
| cold | 2.44 s | 3.58 s | 14.5 s | 14.6 s | 14.9 s |
| clean | 1.55 s | 2.85 s | **1.43 s** | **1.22 s** | 3.88 s |
| noop | 85 ms | 1.11 s | 372 ms | 314 ms | 2.19 s |
| incremental | 337 ms | 2.57 s | 586 ms | 558 ms | 2.84 s |
| abi | 759 ms | 2.58 s | **642 ms** | **603 ms** | 2.95 s |
| test | 774 ms | 2.62 s | 827 ms | 811 ms | 2.83 s |
| test-edit | 1.09 s | 4.41 s | **1.05 s** | 1.15 s | 3.56 s |
| run | 245 ms | 1.16 s | 499 ms | 528 ms | 2.29 s |
| package | 479 ms | 3.42 s | 2.54 s | 2.59 s | 4.48 s |

### gen-ws (a six-member workspace, 300 classes)

| Scenario | jrs | Maven | Gradle | Gradle CC | Gradle no daemon | Gradle --parallel |
| --- | --- | --- | --- | --- | --- | --- |
| cold | 2.42 s | 2.51 s | 14.1 s | 14.3 s | 14.7 s | 14.9 s |
| clean | 839 ms | 1.78 s | **824 ms** | **723 ms** | 3.20 s | **824 ms** |
| noop | 94 ms | 1.00 s | 395 ms | 340 ms | 2.26 s | 401 ms |
| incremental | 231 ms | 1.35 s | 432 ms | 374 ms | 2.52 s | 438 ms |
| abi | 464 ms | 1.69 s | 501 ms | **440 ms** | 2.68 s | 473 ms |
| test | 740 ms | 2.54 s | 1.87 s | **693 ms** | 3.86 s | 766 ms |
| test-edit | 535 ms | 2.70 s | 678 ms | 749 ms | 2.95 s | 687 ms |
| run | not timed (BENCHMARKS.md §11) | | | | | |
| package | 404 ms | 3.20 s | 1.49 s | 1.54 s | 3.45 s | 1.51 s |

### Where jrs is better (0.14.5)

- **Start-up bound work.** No-op builds are 61–94 ms on every project,
  against 314–432 ms for a warm Gradle daemon (3.6–7.0×), 868 ms–1.11 s for
  Maven (11–16×) and 2.02–2.30 s for Gradle without a daemon (24–37×).
  `run` takes 125–245 ms to first output, and 745 ms to Spring Boot's first
  HTTP answer.
- **One-file edits.** `incremental` is 224–337 ms on all five projects,
  1.5–1.7× ahead of the faster warm-daemon column (372–558 ms) and 5–8×
  ahead of Maven.
- **Tests, outside the workspace.** `test` beats every warm-daemon column
  on `wordstats`, `bookmarks` and `gen-300` by 1.4–2.0×, and on `gen-2000`
  with 774 ms against 811–827 ms. In the workspace it is ahead of the
  daemon (1.87 s) and `--parallel` (766 ms), behind Gradle CC (below).
  `test-edit` in the workspace is 535 ms against 2.70 s for Maven.
- **An API change in a small project.** `abi` is 303 ms on `wordstats`
  against 374–379 ms for the daemon and 343 ms on `bookmarks` against
  385–463 ms.
- **Clean builds of the smallest projects.** `clean` is 292 ms on
  `wordstats` against 425–451 ms for the daemon (1.5×), and 282 ms on
  `bookmarks` against 574–615 ms (2.0–2.2×). `gen-300` left this list
  (below).
- **Fat jars, Spring Boot's included.** 90–479 ms, 3.7–6.8× ahead of
  `shadowJar` on a warm daemon (`gen-ws` the low end, at 404 ms against
  1.49 s), and 173 ms against `bootJar`'s 496 ms on `bookmarks` (2.9×).
- **Cold builds against Gradle**, at 2.01–2.44 s against 11.3–14.9 s, and
  against Maven on the large projects: 2.44 s against 3.58 s on `gen-2000`
  (1.5×), 2.42 s against 2.51 s on `gen-ws`.
- **Against what CI sees.** Against `gradle --no-daemon` jrs wins every
  cell; against Maven every cell but `cold` on the three small projects.

### Where jrs can improve (0.14.5)

- **Cold builds of small projects (new).** `cold` is 2.01 s on
  `wordstats`, 2.07 s on `bookmarks` and 2.13 s on `gen-300`, behind
  Maven's 1.52, 1.90 and 1.92 s, against `--timings` totals of 464, 617
  and 873 ms: the command waits for `javac`'s training, which since
  `50f1854` takes 0.66–0.97 s longer.
- **Clean builds of a large project or a workspace, and now in
  `gen-300`.** `clean` on `gen-2000` is 1.55 s against 1.22–1.43 s for the
  daemon, 1,221 ms of it `javac`; on `gen-ws` 839 ms against 723–824 ms,
  behind all three warm columns and 95 ms above its members' critical path;
  on `gen-300` 659 ms against Gradle CC's 613 ms.
- **An API change in a large project or a workspace, and in `gen-300`.**
  `abi` on `gen-2000` is 759 ms against 603–642 ms, on `gen-ws` 464 ms
  against Gradle CC's 440 ms (ahead of the daemon and `--parallel`), and on
  `gen-300` 428 ms against Gradle CC's 420 ms.
- **The workspace's test suite.** `test` on `gen-ws` is 740 ms against
  Gradle CC's 693 ms, with every member's test JVM at 538–600 ms, slower
  in each of the last two passes. `test-edit` on `gen-2000` is 1.09 s
  against the daemon's 1.05 s.
- **Packaging (new).** Still 3.7–6.8× ahead of `shadowJar`, but `gen-300`
  (298 ms), `gen-2000` (479 ms) and `gen-ws` (404 ms) took 17–64% longer
  than in the eighth pass, with no change to packaging behind it.

A full run (seven measured runs and two warm-ups per cell) is still what
BENCHMARKS.md §7 asks for before these become the published report.

---

## Tenth pass: jrs 0.14.6 (2026-10-18)

Run on **2026-10-18 at 13:57 CEST** (finished 14:12) with **jrs 0.14.6**
(`4d4c795`, the release commit, whose only code change since the ninth
pass is `4d69d11`; `benches/compare.rs`). The same machine as the earlier
passes: Apple M4 Max (14 cores: 10 performance and 4 efficiency, 36 GiB),
macOS 27.0.1 (26A434), Corretto 25.0.2, Maven 3.9.16 and Gradle 9.1.0,
every tool from a freshly recorded local mirror (8,148 requests, none
unanswered). Every project passed the checks of BENCHMARKS.md §5.1 on all
three tools again.

The settings are the earlier passes', so the ten can be compared: **one
run per cell with no warm-up** (`JRS_BENCH_RUNS=1 JRS_BENCH_WARMUP=0`), the
five parity projects only
(`JRS_BENCH_PROJECTS=wordstats,bookmarks,gen-300,gen-2000,gen-ws`). The same
caveat holds: this is still not the report of BENCHMARKS.md §6, and a cell
may move by tens of percent in a full run.

What changed since `2934af2` that can move a number:

- `4d69d11` reverts `50f1854`, the ninth pass's training of `javac`'s AOT
  cache on 300 generated classes: the cache trains on the hand-written
  workload alone again, as the worker's does, so the first command no
  longer waits for the longer training.

### jrs, ninth pass against tenth

| Project | Scenario | Ninth pass | Tenth pass | Change |
| --- | --- | --- | --- | --- |
| `wordstats` | cold | 2.01 s | 1.35 s | −33% |
| `wordstats` | clean | 292 ms | 270 ms | −8% |
| `wordstats` | noop | 61 ms | 65 ms | +7% |
| `wordstats` | test-edit | 575 ms | 529 ms | −8% |
| `bookmarks` | cold | 2.07 s | 1.28 s | −38% |
| `bookmarks` | clean | 282 ms | 379 ms | +34% |
| `bookmarks` | package | 173 ms | 219 ms | +27% |
| `gen-300` | cold | 2.13 s | 1.33 s | −38% |
| `gen-300` | clean | 659 ms | 697 ms | +6% |
| `gen-300` | run | 130 ms | 138 ms | +6% |
| `gen-2000` | cold | 2.44 s | 1.79 s | −27% |
| `gen-2000` | clean | 1.55 s | 1.67 s | +8% |
| `gen-2000` | noop | 85 ms | 105 ms | +24% |
| `gen-2000` | incremental | 337 ms | 368 ms | +9% |
| `gen-2000` | test | 774 ms | 700 ms | −10% |
| `gen-2000` | run | 245 ms | 344 ms | +40% |
| `gen-2000` | package | 479 ms | 506 ms | +6% |
| `gen-ws` | cold | 2.42 s | 1.45 s | −40% |
| `gen-ws` | clean | 839 ms | 1.02 s | +22% |
| `gen-ws` | noop | 94 ms | 122 ms | +30% |
| `gen-ws` | incremental | 231 ms | 253 ms | +10% |
| `gen-ws` | abi | 464 ms | 497 ms | +7% |
| `gen-ws` | test | 740 ms | 496 ms | −33% |

Every other jrs cell is within ±5% of the ninth pass. The machine moved a
little: every column's median moved by less than 5% (jrs +1.9%, Maven
+4.3%, Gradle's +1.2% to +3.1%), but both warm Gradle columns moved by
+10% to +20% on `gen-2000`'s `incremental`, `test-edit` and `package`, and
Gradle CC's `clean` by +11% to +16% on every project but `wordstats`, so
some of jrs's `clean` moves are the machine's. Maven moved more than
either: +24% to +34% on `clean` in `bookmarks`, `gen-300` and `gen-ws`,
−33% on `gen-2000`'s `test`.

- **`cold` is back where the eighth pass had it**, the revert's doing:
  1.28–1.79 s against the ninth pass's 2.01–2.44 s and the eighth's
  1.25–1.78 s. jrs is ahead of Maven on every project again: 1.35 s
  against 1.52 s on `wordstats`, 1.28 s against 1.92 s on `bookmarks`,
  1.33 s against 1.92 s on `gen-300`.
- **`gen-ws`'s `test` is back too**, 496 ms against 740 ms, with every
  member's test JVM at 334–361 ms against 538–600 ms, and ahead of all
  three warm Gradle columns again (694 ms–1.79 s).
- **`clean` is slower on `bookmarks` and `gen-ws`.** On `bookmarks` the
  `javac` phase took 267 ms against 198, 379 ms in all; jrs is still ahead
  of the daemon (635–683 ms). On `gen-ws` every member took longer, `core`
  348 ms against 271 (below), 1.02 s in all. On `gen-2000` the `javac`
  phase took 1,365 ms against 1,221, near the eighth pass's 1,336, and the
  command 1.67 s.
- **`noop` on the two largest projects** took 105 ms on `gen-2000` and
  122 ms on `gen-ws`. On `gen-ws` the members' API digests took 13, 13 and
  23 ms in `model`, `store` and `service` against 3, 3 and 5; on
  `gen-2000` the `--timings` total was 88 ms against 81.
- **`gen-2000`'s `run`** took 344 ms against 245, with no change to
  running behind it and the daemon's 517 ms against 499.
- **`bookmarks`'s `package`** took 219 ms, its packaging phase 157 ms
  against 111. `gen-300`, `gen-2000` and `gen-ws` packaged in about the
  ninth pass's time (307, 506 and 403 ms), not the eighth's.

`gen-2000`'s `test-edit` (1.11 s) is ahead of the daemon's 1.24 s again,
and `gen-300`'s `abi` (428 ms) ahead of Gradle CC's 430 ms.

### Workspace members at once (`gen-ws`)

GRADLE_GAPS.md §2.1: each member's wall time in jrs's clean build.

| Members | Sequential (sum) | Critical path | jrs clean | Gradle --parallel | Gate |
| --- | --- | --- | --- | --- | --- |
| 6 | 1,053 ms | 918 ms (13% less) | 1,016 ms | 821 ms | holds |

The members took `core 348 · model 144 · store 135 · service 134 · web 147 ·
app 145` ms, against `271 · 139 · 109 · 106 · 113 · 115` in the ninth pass,
and jrs's clean build is 98 ms above the critical path. The gate holds
again, as in the seventh pass: the critical path is still 13% below the
sum, where the gate asks for 25%, but Gradle `--parallel` beats jrs by 24%
(821 ms against 1,016 ms), where the gate asks for 20%.

### All cells

Columns as in the second pass. **Bold** marks a tool that beat jrs.

### wordstats (a few classes, Guava, JUnit)

| Scenario | jrs | Maven | Gradle | Gradle CC | Gradle no daemon |
| --- | --- | --- | --- | --- | --- |
| cold | 1.35 s | 1.52 s | 11.3 s | 11.9 s | 11.5 s |
| clean | 270 ms | 1.21 s | 464 ms | 439 ms | 2.40 s |
| noop | 65 ms | 878 ms | 358 ms | 323 ms | 2.06 s |
| incremental | 223 ms | 1.22 s | 394 ms | 377 ms | 2.31 s |
| abi | 305 ms | 1.19 s | 399 ms | 367 ms | 2.34 s |
| test | 370 ms | 1.23 s | 610 ms | 571 ms | 2.40 s |
| test-edit | 529 ms | 1.54 s | 635 ms | 696 ms | 2.65 s |
| run | 126 ms | 1.44 s | 414 ms | 467 ms | 2.16 s |
| package | 88 ms | 1.48 s | 606 ms | 658 ms | 2.39 s |

### bookmarks (Spring Boot 4.1)

| Scenario | jrs | Maven | Gradle | Gradle CC | Gradle no daemon |
| --- | --- | --- | --- | --- | --- |
| cold | 1.28 s | 1.92 s | 12.7 s | 13.1 s | 13.1 s |
| clean | 379 ms | 1.57 s | 635 ms | 683 ms | 2.65 s |
| noop | 62 ms | 1.08 s | 466 ms | 342 ms | 2.29 s |
| incremental | 262 ms | 1.38 s | 481 ms | 388 ms | 2.59 s |
| abi | 348 ms | 1.36 s | 461 ms | 390 ms | 2.59 s |
| test | 1.08 s | 2.31 s | 1.60 s | 1.49 s | 3.60 s |
| test-edit | 1.31 s | 2.65 s | 1.67 s | 1.76 s | 3.94 s |
| run (to HTTP 200) | 773 ms | 2.16 s | 1.03 s | 1.07 s | 2.91 s |
| package | 219 ms | 1.53 s | 499 ms | 550 ms | 2.47 s |

### gen-300 (300 classes, 30 dependencies)

| Scenario | jrs | Maven | Gradle | Gradle CC | Gradle no daemon |
| --- | --- | --- | --- | --- | --- |
| cold | 1.33 s | 1.92 s | 12.9 s | 13.3 s | 12.9 s |
| clean | 697 ms | 2.24 s | **685 ms** | **686 ms** | 2.79 s |
| noop | 66 ms | 986 ms | 353 ms | 344 ms | 2.08 s |
| incremental | 254 ms | 1.83 s | 429 ms | 397 ms | 2.39 s |
| abi | 428 ms | 1.98 s | 441 ms | 430 ms | 2.51 s |
| test | 324 ms | 1.31 s | 648 ms | 632 ms | 2.48 s |
| test-edit | 509 ms | 2.26 s | 744 ms | 779 ms | 2.83 s |
| run | 138 ms | 1.11 s | 423 ms | 459 ms | 2.13 s |
| package | 307 ms | 2.38 s | 1.60 s | 1.54 s | 3.34 s |

### gen-2000 (2000 classes, 60 dependencies)

| Scenario | jrs | Maven | Gradle | Gradle CC | Gradle no daemon |
| --- | --- | --- | --- | --- | --- |
| cold | 1.79 s | 3.20 s | 14.7 s | 14.9 s | 14.6 s |
| clean | 1.67 s | 2.81 s | **1.46 s** | **1.41 s** | 4.53 s |
| noop | 105 ms | 1.09 s | 382 ms | 317 ms | 2.22 s |
| incremental | 368 ms | 3.25 s | 671 ms | 626 ms | 3.10 s |
| abi | 751 ms | 2.85 s | **650 ms** | **616 ms** | 3.03 s |
| test | 700 ms | 1.75 s | 877 ms | 805 ms | 2.86 s |
| test-edit | 1.11 s | 3.85 s | 1.24 s | 1.27 s | 3.74 s |
| run | 344 ms | 1.18 s | 517 ms | 552 ms | 2.40 s |
| package | 506 ms | 3.89 s | 3.06 s | 2.92 s | 5.50 s |

### gen-ws (a six-member workspace, 300 classes)

| Scenario | jrs | Maven | Gradle | Gradle CC | Gradle no daemon | Gradle --parallel |
| --- | --- | --- | --- | --- | --- | --- |
| cold | 1.45 s | 2.54 s | 14.9 s | 15.4 s | 16.0 s | 16.7 s |
| clean | 1.02 s | 2.20 s | **855 ms** | **822 ms** | 3.35 s | **821 ms** |
| noop | 122 ms | 1.13 s | 400 ms | 328 ms | 2.29 s | 384 ms |
| incremental | 253 ms | 1.51 s | 440 ms | 421 ms | 2.52 s | 455 ms |
| abi | 497 ms | 1.71 s | 514 ms | **488 ms** | 2.90 s | 528 ms |
| test | 496 ms | 2.43 s | 1.79 s | 694 ms | 3.84 s | 740 ms |
| test-edit | 558 ms | 2.97 s | 695 ms | 791 ms | 2.88 s | 638 ms |
| run | not timed (BENCHMARKS.md §11) | | | | | |
| package | 403 ms | 3.83 s | 1.61 s | 1.73 s | 3.70 s | 1.60 s |

### Where jrs is better (0.14.6)

- **Start-up bound work.** No-op builds are 62–122 ms on every project,
  against 317–466 ms for a warm Gradle daemon (2.7–7.5×), 878 ms–1.13 s for
  Maven (9.3–17×) and 2.06–2.29 s for Gradle without a daemon (19–37×).
  `run` takes 126–344 ms to first output, and 773 ms to Spring Boot's first
  HTTP answer.
- **One-file edits.** `incremental` is 223–368 ms on all five projects,
  1.5–1.7× ahead of the faster warm-daemon column (377–626 ms) and 5–9×
  ahead of Maven.
- **Tests, the workspace's again.** `test` beats every warm-daemon column
  on every project: by 1.4–2.0× on `wordstats`, `bookmarks` and `gen-300`,
  with 700 ms against 805–877 ms on `gen-2000`, and with 496 ms against
  694 ms–1.79 s on `gen-ws`. `test-edit` in the workspace is 558 ms
  against 2.97 s for Maven, and on `gen-2000` 1.11 s against 1.24–1.27 s.
- **An API change in a small project.** `abi` is 305 ms on `wordstats`
  against 367–399 ms for the daemon, 348 ms on `bookmarks` against
  390–461 ms, and 428 ms on `gen-300` against 430–441 ms.
- **Clean builds of the smallest projects.** `clean` is 270 ms on
  `wordstats` against 439–464 ms for the daemon (1.6–1.7×), and 379 ms on
  `bookmarks` against 635–683 ms (1.7–1.8×).
- **Fat jars, Spring Boot's included.** 88–506 ms, 4.0–6.9× ahead of
  `shadowJar` on a warm daemon (`gen-ws` the low end, at 403 ms against
  1.61 s), and 219 ms against `bootJar`'s 499 ms on `bookmarks` (2.3×).
- **Cold builds, against both.** 1.28–1.79 s against 11.3–16.7 s for
  Gradle, and ahead of Maven on every project again: by 1.1× on
  `wordstats` (1.35 s against 1.52 s), 1.4–1.5× on `bookmarks` and
  `gen-300`, and 1.8× on `gen-2000` and `gen-ws` (1.79 s against 3.20 s,
  1.45 s against 2.54 s).
- **Against what CI sees.** Against `gradle --no-daemon` and against Maven
  jrs wins every cell.

### Where jrs can improve (0.14.6)

- **Clean builds of a large project or a workspace, and of `gen-300`.**
  `clean` on `gen-2000` is 1.67 s against 1.41–1.46 s for the daemon,
  1,365 ms of it `javac`; on `gen-ws` 1.02 s against 821–855 ms, behind
  all three warm columns and 98 ms above its members' critical path, where
  the GRADLE_GAPS.md §2.1 gate now holds; on `gen-300` 697 ms against
  685–686 ms, now behind both warm columns.
- **An API change in a large project or a workspace.** `abi` on `gen-2000`
  is 751 ms against 616–650 ms, and on `gen-ws` 497 ms against Gradle CC's
  488 ms (ahead of the daemon and `--parallel`). `gen-300` left this list.
- **Packaging.** Still 4.0–6.9× ahead of `shadowJar`, but `gen-300`
  (307 ms), `gen-2000` (506 ms) and `gen-ws` (403 ms) stayed at the ninth
  pass's level, 23–63% above the eighth's, and `bookmarks` took 219 ms
  against 173, with no change to packaging behind either.

The ninth pass's cold builds of small projects and the workspace's test
suite left this list with the revert.

A full run (seven measured runs and two warm-ups per cell) is still what
BENCHMARKS.md §7 asks for before these become the published report.

---

## Eleventh pass: jrs 0.14.7 (2026-10-18)

Run on **2026-10-18 at 19:04 CEST** (finished 19:24) with **jrs 0.14.7**
ahead of its tag: `25736a0`, the commit 0.14.7 is to be tagged from, whose
`Cargo.toml` still reads 0.14.6. Its only code change since the tenth pass
is `0aa2cc3`; `d785f44` moves specs, and changes no more in `src/` than the
paths its comments cite. The same machine as the earlier passes: Apple M4
Max (14 cores: 10 performance and 4 efficiency, 36 GiB), macOS 27.0.1
(26A434), Corretto 25.0.2, Maven 3.9.16 and Gradle 9.1.0, every tool from
a freshly recorded local mirror (8,138 requests, none unanswered). Every
project passed the checks of BENCHMARKS.md §5.1 on all three tools again.

The settings are the earlier passes', so the eleven can be compared: **one
run per cell with no warm-up** (`JRS_BENCH_RUNS=1 JRS_BENCH_WARMUP=0`), the
five parity projects only
(`JRS_BENCH_PROJECTS=wordstats,bookmarks,gen-300,gen-2000,gen-ws`). The same
caveat holds: this is still not the report of BENCHMARKS.md §6, and a cell
may move by tens of percent in a full run.

What changed since `4d4c795` that can move a number:

- `0aa2cc3` trains one AOT cache for `javac` and the `javac` worker where
  there were two: from JDK 25 `javac`'s cache is recorded by `java` running
  `com.sun.tools.javac.Main` on an empty class-path directory, and the
  worker, started with that directory first, maps the same cache. The first
  command no longer waits for a second training beside its build.

### jrs, tenth pass against eleventh

| Project | Scenario | Tenth pass | Eleventh pass | Change |
| --- | --- | --- | --- | --- |
| `wordstats` | cold | 1.35 s | 1.11 s | −18% |
| `wordstats` | clean | 270 ms | 302 ms | +12% |
| `wordstats` | noop | 65 ms | 69 ms | +6% |
| `wordstats` | test-edit | 529 ms | 497 ms | −6% |
| `bookmarks` | cold | 1.28 s | 1.03 s | −20% |
| `bookmarks` | clean | 379 ms | 269 ms | −29% |
| `bookmarks` | abi | 348 ms | 330 ms | −5% |
| `bookmarks` | test | 1.08 s | 1.15 s | +6% |
| `bookmarks` | test-edit | 1.31 s | 1.23 s | −6% |
| `bookmarks` | run | 773 ms | 728 ms | −6% |
| `bookmarks` | package | 219 ms | 170 ms | −22% |
| `gen-300` | cold | 1.33 s | 1.05 s | −21% |
| `gen-300` | clean | 697 ms | 565 ms | −19% |
| `gen-300` | incremental | 254 ms | 235 ms | −7% |
| `gen-300` | test-edit | 509 ms | 479 ms | −6% |
| `gen-300` | run | 138 ms | 131 ms | −5% |
| `gen-300` | package | 307 ms | 180 ms | −41% |
| `gen-2000` | clean | 1.67 s | 1.79 s | +7% |
| `gen-2000` | noop | 105 ms | 90 ms | −14% |
| `gen-2000` | test | 700 ms | 911 ms | +30% |
| `gen-2000` | test-edit | 1.11 s | 989 ms | −11% |
| `gen-2000` | run | 344 ms | 232 ms | −33% |
| `gen-2000` | package | 506 ms | 449 ms | −11% |
| `gen-ws` | clean | 1.02 s | 784 ms | −23% |
| `gen-ws` | noop | 122 ms | 93 ms | −24% |
| `gen-ws` | incremental | 253 ms | 226 ms | −11% |
| `gen-ws` | abi | 497 ms | 456 ms | −8% |
| `gen-ws` | test-edit | 558 ms | 493 ms | −12% |
| `gen-ws` | package | 403 ms | 303 ms | −25% |

Every other jrs cell is within ±5% of the tenth pass. The machine moved
back some of the way it moved in the tenth pass: every other column's
median moved by less than 5% (Maven −1.3%, Gradle's −0.8% to −4.2%,
against jrs's −5.5%), but Gradle CC's `clean` fell by 11% to 17% on `bookmarks`,
`gen-300` and `gen-ws`, and both warm Gradle columns by 11% to 16% on
`gen-2000`'s `test-edit` and `package`, about what they rose by in the
tenth pass. So some of jrs's `clean` moves are the machine's again. Maven
moved more than either: +35% and +36% on `wordstats`'s `clean` and `noop`,
+51% on `gen-300`'s `noop`, −30% on `wordstats`'s `run`.

- **`cold` is faster on the three small projects**, `0aa2cc3`'s doing:
  1.11 s against 1.35 s on `wordstats`, 1.03 s against 1.28 s on
  `bookmarks`, 1.05 s against 1.33 s on `gen-300`. The `--timings` totals
  barely moved (461, 544 and 836 ms against 473, 569 and 853), so what went
  is the wait past the build: 649 ms against 877 on `wordstats`, 486
  against 711 on `bookmarks`, 214 against 477 on `gen-300`. `gen-2000`
  (1.78 s) and `gen-ws` (1.44 s), whose builds outlast the training, did
  not move. jrs is ahead of Maven by 1.5–2.0× on every project.
- **`clean` is faster on `bookmarks`, `gen-300` and `gen-ws`.** The `javac`
  phase took 186 ms against 267 on `bookmarks`, 469 against 543 on
  `gen-300`, and `gen-ws`'s `core` member 240 ms against 348 (below). Gradle
  CC fell as far on the same three, so this pass claims no cause for it.
  `gen-300`'s `clean` (565 ms) is ahead of both warm columns again
  (587–632 ms), and `gen-ws`'s (784 ms) ahead of the plain daemon's 789 ms.
  `wordstats` went the other way, its `javac` phase 230 ms against 202, and
  so did `gen-2000`, 1.79 s with its `javac` phase where it was (1,368 ms
  against 1,365).
- **Packaging is faster on every project but `wordstats`**, with no change
  to packaging behind it: `bookmarks` 170 ms with its packaging phase at
  108 ms against 157, `gen-300` 180 ms at 115 against 240, `gen-2000`
  449 ms at 364 against 423, `gen-ws` 303 ms.
- **`noop` on the two largest projects** is back: 90 ms on `gen-2000`, and
  93 ms on `gen-ws`, whose members' API digests took 3, 3 and 6 ms in
  `model`, `store` and `service` against 13, 13 and 23.
- **`gen-2000`'s `run`** took 232 ms, back near the ninth pass's 245, with
  no change to running behind it.
- **`gen-2000`'s `test`** took 911 ms against 700, its test JVM 682 ms
  against 506, with no change to testing behind it; jrs is behind both warm
  Gradle columns there (779–825 ms), where it was ahead.

### Workspace members at once (`gen-ws`)

GRADLE_GAPS.md §2.1: each member's wall time in jrs's clean build.

| Members | Sequential (sum) | Critical path | jrs clean | Gradle --parallel | Gate |
| --- | --- | --- | --- | --- | --- |
| 6 | 813 ms | 694 ms (15% less) | 784 ms | 775 ms | does not hold |

The members took `core 240 · model 121 · store 119 · service 107 · web 113 ·
app 113` ms, against `348 · 144 · 135 · 134 · 147 · 145` in the tenth pass,
and jrs's clean build is 90 ms above the critical path. The gate does not
hold: the critical path is 15% below the sum, where the gate asks for 25%,
and Gradle `--parallel` beats jrs by 1% (775 ms against 784 ms), where the
gate asks for 20%.

### All cells

Columns as in the second pass. **Bold** marks a tool that beat jrs.

### wordstats (a few classes, Guava, JUnit)

| Scenario | jrs | Maven | Gradle | Gradle CC | Gradle no daemon |
| --- | --- | --- | --- | --- | --- |
| cold | 1.11 s | 1.66 s | 12.4 s | 12.1 s | 12.3 s |
| clean | 302 ms | 1.63 s | 467 ms | 440 ms | 2.51 s |
| noop | 69 ms | 1.19 s | 400 ms | 351 ms | 2.17 s |
| incremental | 216 ms | 1.28 s | 396 ms | 370 ms | 2.34 s |
| abi | 293 ms | 1.28 s | 401 ms | 363 ms | 2.35 s |
| test | 357 ms | 1.21 s | 607 ms | 576 ms | 2.35 s |
| test-edit | 497 ms | 1.73 s | 652 ms | 671 ms | 2.57 s |
| run | 120 ms | 1.01 s | 397 ms | 435 ms | 2.10 s |
| package | 89 ms | 1.55 s | 606 ms | 641 ms | 2.38 s |

### bookmarks (Spring Boot 4.1)

| Scenario | jrs | Maven | Gradle | Gradle CC | Gradle no daemon |
| --- | --- | --- | --- | --- | --- |
| cold | 1.03 s | 2.08 s | 12.8 s | 12.8 s | 12.0 s |
| clean | 269 ms | 1.26 s | 585 ms | 608 ms | 2.60 s |
| noop | 62 ms | 1.07 s | 464 ms | 341 ms | 2.32 s |
| incremental | 251 ms | 1.42 s | 493 ms | 393 ms | 2.60 s |
| abi | 330 ms | 1.29 s | 465 ms | 403 ms | 2.66 s |
| test | 1.15 s | 2.34 s | 1.62 s | 1.52 s | 3.67 s |
| test-edit | 1.23 s | 2.85 s | 1.64 s | 1.74 s | 3.89 s |
| run (to HTTP 200) | 728 ms | 1.98 s | 1.00 s | 1.05 s | 3.00 s |
| package | 170 ms | 1.57 s | 499 ms | 514 ms | 2.44 s |

### gen-300 (300 classes, 30 dependencies)

| Scenario | jrs | Maven | Gradle | Gradle CC | Gradle no daemon |
| --- | --- | --- | --- | --- | --- |
| cold | 1.05 s | 2.09 s | 12.3 s | 12.3 s | 12.1 s |
| clean | 565 ms | 1.80 s | 632 ms | 587 ms | 2.74 s |
| noop | 65 ms | 1.49 s | 369 ms | 321 ms | 2.04 s |
| incremental | 235 ms | 1.56 s | 438 ms | 401 ms | 2.39 s |
| abi | 411 ms | 1.53 s | 436 ms | 412 ms | 2.63 s |
| test | 338 ms | 1.56 s | 649 ms | 628 ms | 2.45 s |
| test-edit | 479 ms | 2.02 s | 706 ms | 735 ms | 2.84 s |
| run | 131 ms | 1.09 s | 429 ms | 453 ms | 2.10 s |
| package | 180 ms | 2.47 s | 1.47 s | 1.50 s | 3.30 s |

### gen-2000 (2000 classes, 60 dependencies)

| Scenario | jrs | Maven | Gradle | Gradle CC | Gradle no daemon |
| --- | --- | --- | --- | --- | --- |
| cold | 1.78 s | 3.20 s | 14.4 s | 15.0 s | 15.0 s |
| clean | 1.79 s | 2.74 s | **1.50 s** | **1.56 s** | 4.49 s |
| noop | 90 ms | 1.21 s | 381 ms | 369 ms | 2.23 s |
| incremental | 383 ms | 2.63 s | 588 ms | 580 ms | 2.95 s |
| abi | 779 ms | 2.62 s | **640 ms** | **631 ms** | 2.99 s |
| test | 911 ms | 1.86 s | **825 ms** | **779 ms** | 2.81 s |
| test-edit | 989 ms | 3.40 s | 1.04 s | 1.08 s | 3.46 s |
| run | 232 ms | 1.41 s | 496 ms | 521 ms | 2.34 s |
| package | 449 ms | 3.58 s | 2.63 s | 2.61 s | 4.48 s |

### gen-ws (a six-member workspace, 300 classes)

| Scenario | jrs | Maven | Gradle | Gradle CC | Gradle no daemon | Gradle --parallel |
| --- | --- | --- | --- | --- | --- | --- |
| cold | 1.44 s | 2.22 s | 14.2 s | 14.3 s | 14.0 s | 13.9 s |
| clean | 784 ms | 1.75 s | 789 ms | **683 ms** | 2.95 s | **775 ms** |
| noop | 93 ms | 1.09 s | 398 ms | 336 ms | 2.18 s | 370 ms |
| incremental | 226 ms | 1.32 s | 435 ms | 373 ms | 2.47 s | 433 ms |
| abi | 456 ms | 1.75 s | 506 ms | **432 ms** | 2.56 s | 477 ms |
| test | 502 ms | 2.36 s | 1.79 s | 672 ms | 3.81 s | 1.04 s |
| test-edit | 493 ms | 2.70 s | 656 ms | 741 ms | 2.84 s | 670 ms |
| run | not timed (BENCHMARKS.md §11) | | | | | |
| package | 303 ms | 3.47 s | 1.57 s | 1.61 s | 3.57 s | 1.57 s |

### Where jrs is better (0.14.7)

- **Start-up bound work.** No-op builds are 62–93 ms on every project,
  against 321–464 ms for a warm Gradle daemon (3.6–7.5×), 1.07–1.49 s for
  Maven (12–23×) and 2.04–2.32 s for Gradle without a daemon (23–37×).
  `run` takes 120–232 ms to first output, and 728 ms to Spring Boot's first
  HTTP answer.
- **One-file edits.** `incremental` is 216–383 ms on all five projects,
  1.5–1.7× ahead of the faster warm-daemon column (370–580 ms) and 5.7–6.9×
  ahead of Maven.
- **Tests, but for `gen-2000`'s full suite.** `test` beats every
  warm-daemon column on four projects: by 1.3–1.9× on `wordstats`,
  `bookmarks` and `gen-300`, and with 502 ms against 672 ms–1.79 s on
  `gen-ws`. `test-edit` beats them on all five: in the workspace 493 ms
  against 2.70 s for Maven, and on `gen-2000` 989 ms against 1.04–1.08 s.
- **An API change in a small project.** `abi` is 293 ms on `wordstats`
  against 363–401 ms for the daemon, 330 ms on `bookmarks` against
  403–465 ms, and 411 ms on `gen-300` against 412–436 ms.
- **Clean builds of the small projects, `gen-300`'s again.** `clean` is
  302 ms on `wordstats` against 440–467 ms for the daemon (1.5×), 269 ms on
  `bookmarks` against 585–608 ms (2.2–2.3×), and 565 ms on `gen-300`
  against 587–632 ms.
- **Fat jars, Spring Boot's included.** 89–449 ms, 5.2–8.2× ahead of
  `shadowJar` on a warm daemon (`gen-ws` the low end, at 303 ms against
  1.57 s), and 170 ms against `bootJar`'s 499 ms on `bookmarks` (2.9×).
- **Cold builds, against both.** 1.03–1.78 s against 12.0–15.0 s for
  Gradle, and ahead of Maven on every project: by 1.5× on `wordstats` and
  `gen-ws` (1.11 s against 1.66 s, 1.44 s against 2.22 s), 1.8× on
  `gen-2000` (1.78 s against 3.20 s), and 2.0× on `bookmarks` and
  `gen-300`.
- **Against what CI sees.** Against `gradle --no-daemon` and against Maven
  jrs wins every cell.

### Where jrs can improve (0.14.7)

- **Clean builds of a large project or a workspace.** `clean` on
  `gen-2000` is 1.79 s against 1.50–1.56 s for the daemon, 1,368 ms of it
  `javac`; on `gen-ws` 784 ms against Gradle CC's 683 ms and `--parallel`'s
  775 ms (ahead of the plain daemon's 789 ms), 90 ms above its members'
  critical path, where the GRADLE_GAPS.md §2.1 gate no longer holds.
  `gen-300` left this list.
- **An API change in a large project or a workspace.** `abi` on `gen-2000`
  is 779 ms against 631–640 ms, and on `gen-ws` 456 ms against Gradle CC's
  432 ms (ahead of the daemon's 506 ms and `--parallel`'s 477 ms).
- **`gen-2000`'s full test suite.** `test` is 911 ms against 779–825 ms for
  the daemon, 682 ms of it the test JVM, with no change to testing since
  the tenth pass's 700 ms; one run per cell cannot yet say whether it is a
  one-off like the seventh pass's.

Packaging left this list: every project packages faster than in the tenth
pass, `bookmarks` and `gen-300` in 170 and 180 ms.

A full run (seven measured runs and two warm-ups per cell) is still what
BENCHMARKS.md §7 asks for before these become the published report.

---

## Twelfth pass: jrs 0.16.0 (2026-10-31)

Run on **2026-10-31 at 20:22 CET** (finished 20:38) with **jrs 0.16.0**:
`013bcad`, which the tag `v0.16.0` points at and which is not released yet.
The release writes the version into `Cargo.toml`, which still reads 0.15.0,
so the binary and the report call themselves 0.15.0. The same machine as the
earlier passes: Apple M4 Max (14 cores: 10 performance and 4 efficiency,
36 GiB), macOS 27.0.1 (26A434), Corretto 25.0.2 and Gradle 9.1.0, every tool
from a freshly recorded local mirror (7,868 requests, none unanswered).
**Maven is 3.10.0**, where every earlier pass had 3.9.16, so its column does
not compare with theirs. Every project passed the checks of BENCHMARKS.md
§5.1 on all three tools again.

The settings are the earlier passes', so the twelve can be compared: **one
run per cell with no warm-up** (`JRS_BENCH_RUNS=1 JRS_BENCH_WARMUP=0`), the
five parity projects only
(`JRS_BENCH_PROJECTS=wordstats,bookmarks,gen-300,gen-2000,gen-ws`). The same
caveat holds: this is still not the report of BENCHMARKS.md §6, and a cell
may move by tens of percent in a full run.

There is no pass of 0.14.8 or 0.15.0. What changed since `25736a0` that can
move a number:

- `013bcad` compiles, for a class that gained only methods with a body, the
  sources with a subtype of it and those with a new name in a constant pool,
  where a round compiled every user. It follows a tracked class the same way,
  for the tests and a workspace member: `abi`.
- `20af610` keeps the main classes' API digest under their sizes and times,
  stops the up-to-date check from reading the time of every class, and reads
  and hashes a clean tree's sources, the jars' processor registrations and
  the class directories on `rayon`'s pool: `test`, `noop`, `clean`.
- `b7b1c26` and `950ce3b` move to zip 9 and name the Unix system on every zip
  entry: `package`, though no number here ties a move to them.

### jrs, eleventh pass against twelfth

| Project | Scenario | Previous | This run | Change |
| --- | --- | --- | --- | --- |
| `wordstats` | cold | 1.11 s | 972 ms | −12% |
| `wordstats` | clean | 302 ms | 256 ms | −15% |
| `wordstats` | noop | 69 ms | 61 ms | −12% |
| `wordstats` | abi | 293 ms | 192 ms | −34% |
| `wordstats` | run | 120 ms | 126 ms | +5% |
| `wordstats` | package | 89 ms | 84 ms | −6% |
| `bookmarks` | abi | 330 ms | 224 ms | −32% |
| `bookmarks` | test | 1.15 s | 1.24 s | +8% |
| `bookmarks` | run | 728 ms | 779 ms | +7% |
| `bookmarks` | package | 170 ms | 155 ms | −9% |
| `gen-300` | cold | 1.05 s | 1.12 s | +7% |
| `gen-300` | clean | 565 ms | 606 ms | +7% |
| `gen-300` | abi | 411 ms | 234 ms | −43% |
| `gen-300` | test-edit | 479 ms | 531 ms | +11% |
| `gen-2000` | clean | 1.79 s | 1.55 s | −13% |
| `gen-2000` | noop | 90 ms | 81 ms | −10% |
| `gen-2000` | incremental | 383 ms | 347 ms | −9% |
| `gen-2000` | abi | 779 ms | 333 ms | −57% |
| `gen-2000` | test | 911 ms | 730 ms | −20% |
| `gen-2000` | test-edit | 989 ms | 1.19 s | +20% |
| `gen-2000` | package | 449 ms | 411 ms | −8% |
| `gen-ws` | clean | 784 ms | 715 ms | −9% |
| `gen-ws` | noop | 93 ms | 70 ms | −25% |
| `gen-ws` | abi | 456 ms | 274 ms | −40% |
| `gen-ws` | test | 502 ms | 435 ms | −13% |
| `gen-ws` | test-edit | 493 ms | 449 ms | −9% |
| `gen-ws` | package | 303 ms | 228 ms | −25% |

Every other jrs cell is within ±5% of the eleventh pass. The Gradle columns'
medians hardly moved (−0.2% to +2.3%), but two groups of their cells did:
`cold`, by up to +30% (`bookmarks`'s daemon 12.8 s to 16.4 s, `gen-ws`'s
`--no-daemon` 14.0 s to 18.3 s), and `gen-300`'s `clean`, +23% and +25% on the
daemon and the configuration cache, a cell where jrs rose by 7% too. So jrs's
rises in `gen-300`'s `clean` and `cold` may be the machine's. Gradle
`--parallel`'s `test` on `gen-ws` fell by 27% (1.04 s to 763 ms). Maven's
median rose by 8.2%, from −35% to +96% cell by cell (`gen-2000`'s
`incremental` 2.63 s to 5.16 s), which this pass cannot split between the new
version and one run per cell.

- **`abi` is 32–57% faster on all five projects**: 192, 224, 234, 333 and
  274 ms against 293, 330, 411, 779 and 456. On `wordstats`, `bookmarks` and
  `gen-2000` it is now below `incremental`, an edit of the same size. The benchmark's edit is a method
  nothing calls, so the second round is gone: `gen-2000`'s `javac` phase is
  190 ms, one run of one file, and in `gen-ws` `model` to `app` run no `javac`
  at all and take 15–23 ms each. An edit whose users call the new method
  still compiles them, which this benchmark does not time.
- **`gen-2000`'s `test` fell by 20%** to 730 ms, back under both warm Gradle
  columns (860–869 ms). Its `api digest` row is 7 ms, against the 55–64 ms
  BEATING_THE_DAEMON_4.md §3 recorded, but most of the fall is the test JVM,
  544 ms against 682, with no change to testing behind it. The eleventh pass
  could not say whether its 911 ms was a one-off, and the tenth had 700 ms.
- **`gen-2000`'s `test-edit` rose by 20%** to 1.19 s, behind both warm Gradle
  columns (1.06–1.16 s), where it was ahead. The test JVM took 715 ms, against
  544 in `test` here. `api digest` took 78 ms in this cell, where the main
  classes changed and the memo cannot answer. This pass shows no cause for
  the rest.
- **`gen-2000`'s `clean` fell by 13%** to 1.55 s, but its `javac` phase only
  from 1,368 to 1,334 ms; outside it, `index` is 49 ms and the total 1,537 ms.
  That is the same level as the passes before the eleventh, whose 1.79 s may
  have been the outlier. It is ahead of Gradle CC's 1.60 s and behind the
  plain daemon's 1.46 s.
- **`noop` fell on the two largest projects and on `wordstats`**, to 81 ms on
  `gen-2000`, 70 ms on `gen-ws` and 61 ms on `wordstats`. Its `compile main
  (fresh)` row on `gen-2000` is 6 ms.

### Workspace members at once (`gen-ws`)

GRADLE_GAPS.md §2.1: each member's wall time in jrs's clean build.

| Members | Sequential (sum) | Critical path | jrs clean | Gradle --parallel | Gate |
| --- | --- | --- | --- | --- | --- |
| 6 | 740 ms | 636 ms (14% less) | 715 ms | 756 ms | does not hold |

The members took `core 224 · model 116 · store 104 · service 105 · web 93 ·
app 98` ms, against `240 · 121 · 119 · 107 · 113 · 113` in the eleventh pass,
and jrs's clean build is 79 ms above the critical path. The gate does not
hold: the critical path is 14% below the sum, where the gate asks for 25%,
and jrs now beats Gradle `--parallel`.

### All cells

Columns as in the second pass. **Bold** marks a tool that beat jrs.

### wordstats (a few classes, Guava, JUnit)

| Scenario | jrs | Maven | Gradle | Gradle CC | Gradle no daemon |
| --- | --- | --- | --- | --- | --- |
| cold | 972 ms | 2.83 s | 13.1 s | 14.0 s | 13.9 s |
| clean | 256 ms | 1.91 s | 449 ms | 418 ms | 2.35 s |
| noop | 61 ms | 1.06 s | 346 ms | 321 ms | 2.03 s |
| incremental | 207 ms | 1.78 s | 384 ms | 369 ms | 2.23 s |
| abi | 192 ms | 1.99 s | 382 ms | 376 ms | 2.29 s |
| test | 356 ms | 1.28 s | 604 ms | 570 ms | 2.36 s |
| test-edit | 493 ms | 1.60 s | 629 ms | 691 ms | 2.63 s |
| run | 126 ms | 992 ms | 395 ms | 434 ms | 2.09 s |
| package | 84 ms | 1.28 s | 597 ms | 636 ms | 2.34 s |

### bookmarks (Spring Boot 4.1)

| Scenario | jrs | Maven | Gradle | Gradle CC | Gradle no daemon |
| --- | --- | --- | --- | --- | --- |
| cold | 1.00 s | 3.05 s | 16.4 s | 14.2 s | 13.4 s |
| clean | 282 ms | 1.21 s | 568 ms | 611 ms | 2.62 s |
| noop | 61 ms | 862 ms | 445 ms | 339 ms | 2.31 s |
| incremental | 242 ms | 943 ms | 474 ms | 379 ms | 2.54 s |
| abi | 224 ms | 984 ms | 456 ms | 375 ms | 2.54 s |
| test | 1.24 s | 2.27 s | 1.64 s | 1.52 s | 3.60 s |
| test-edit | 1.28 s | 3.11 s | 1.63 s | 1.76 s | 3.91 s |
| run (to HTTP 200) | 779 ms | 2.45 s | 974 ms | 1.06 s | 2.87 s |
| package | 155 ms | 1.98 s | 504 ms | 544 ms | 2.42 s |

### gen-300 (300 classes, 30 dependencies)

| Scenario | jrs | Maven | Gradle | Gradle CC | Gradle no daemon |
| --- | --- | --- | --- | --- | --- |
| cold | 1.12 s | 2.62 s | 14.3 s | 14.8 s | 15.5 s |
| clean | 606 ms | 1.95 s | 778 ms | 736 ms | 2.84 s |
| noop | 64 ms | 976 ms | 355 ms | 310 ms | 2.08 s |
| incremental | 233 ms | 1.57 s | 424 ms | 401 ms | 2.38 s |
| abi | 234 ms | 1.77 s | 453 ms | 403 ms | 2.56 s |
| test | 325 ms | 1.23 s | 677 ms | 658 ms | 2.59 s |
| test-edit | 531 ms | 2.38 s | 727 ms | 805 ms | 2.90 s |
| run | 131 ms | 1.34 s | 434 ms | 536 ms | 2.28 s |
| package | 184 ms | 2.71 s | 1.58 s | 1.51 s | 3.34 s |

### gen-2000 (2000 classes, 60 dependencies)

| Scenario | jrs | Maven | Gradle | Gradle CC | Gradle no daemon |
| --- | --- | --- | --- | --- | --- |
| cold | 1.71 s | 3.54 s | 16.4 s | 16.4 s | 17.0 s |
| clean | 1.55 s | 3.57 s | **1.46 s** | 1.60 s | 4.24 s |
| noop | 81 ms | 1.61 s | 381 ms | 328 ms | 2.28 s |
| incremental | 347 ms | 5.16 s | 589 ms | 547 ms | 2.81 s |
| abi | 333 ms | 2.84 s | 658 ms | 640 ms | 3.05 s |
| test | 730 ms | 3.22 s | 869 ms | 860 ms | 2.88 s |
| test-edit | 1.19 s | 3.74 s | **1.06 s** | **1.16 s** | 3.66 s |
| run | 239 ms | 917 ms | 516 ms | 537 ms | 2.55 s |
| package | 411 ms | 3.59 s | 2.52 s | 2.57 s | 4.58 s |

### gen-ws (a six-member workspace, 300 classes)

| Scenario | jrs | Maven | Gradle | Gradle CC | Gradle no daemon | Gradle --parallel |
| --- | --- | --- | --- | --- | --- | --- |
| cold | 1.48 s | 1.89 s | 13.5 s | 17.0 s | 18.3 s | 14.7 s |
| clean | 715 ms | 1.74 s | 795 ms | **686 ms** | 3.08 s | 756 ms |
| noop | 70 ms | 1.16 s | 397 ms | 338 ms | 2.19 s | 396 ms |
| incremental | 225 ms | 1.27 s | 434 ms | 372 ms | 2.48 s | 450 ms |
| abi | 274 ms | 1.40 s | 499 ms | 430 ms | 2.72 s | 494 ms |
| test | 435 ms | 3.26 s | 1.86 s | 690 ms | 3.98 s | 763 ms |
| test-edit | 449 ms | 2.92 s | 698 ms | 744 ms | 2.95 s | 677 ms |
| run | not timed (BENCHMARKS.md §11) | | | | | |
| package | 228 ms | 3.70 s | 1.48 s | 1.55 s | 3.44 s | 1.51 s |

### Where jrs is better (0.16.0)

- **Start-up bound work.** No-op builds are 61–81 ms on every project,
  against 310–445 ms for a warm Gradle daemon (4.0–7.3×), 862 ms–1.61 s for
  Maven (14–20×) and 2.03–2.31 s for Gradle without a daemon (28–38×). `run`
  takes 126–239 ms to first output, and 779 ms to Spring Boot's first HTTP
  answer.
- **An API change, on every project.** `abi` is 192–333 ms, 1.6–2.0× ahead of
  the fastest warm-daemon column on all five, the two it lost in the eleventh
  pass included: 333 ms against 640–658 ms on `gen-2000`, and 274 ms against
  430–499 ms on `gen-ws`.
- **One-file edits.** `incremental` is 207–347 ms on all five projects,
  1.6–1.8× ahead of the faster warm-daemon column (369–547 ms) and 3.9–15×
  ahead of Maven.
- **Tests, on all five.** `test` beats every warm-daemon column: by 1.2–2.1×
  on `wordstats`, `bookmarks`, `gen-300` and `gen-2000`, and with 435 ms
  against 690 ms–1.86 s on `gen-ws`. `test-edit` beats them on four, in the
  workspace 449 ms against 2.92 s for Maven.
- **Clean builds of the small projects.** `clean` is 256 ms on `wordstats`
  against 418–449 ms for the daemon (1.6–1.8×), 282 ms on `bookmarks` against
  568–611 ms (2.0–2.2×), and 606 ms on `gen-300` against 736–778 ms
  (1.2–1.3×).
- **Fat jars, Spring Boot's included.** 84–411 ms, 6.1–8.6× ahead of
  `shadowJar` on a warm daemon, and 155 ms against `bootJar`'s 504 ms on
  `bookmarks` (3.3×).
- **Cold builds, against both.** 972 ms–1.71 s against 13.1–18.3 s for
  Gradle, and ahead of Maven on every project: by 2.9× on `wordstats`, 3.0× on
  `bookmarks`, 2.3× on `gen-300`, 2.1× on `gen-2000` and 1.3× on `gen-ws`
  (1.48 s against 1.89 s).
- **Against what CI sees.** Against `gradle --no-daemon` and against Maven
  jrs wins every cell.

### Where jrs can improve (0.16.0)

A warm Gradle column beats jrs in three cells of the 44, by 29–130 ms:

- **`gen-2000`'s `clean`.** 1.55 s against the plain daemon's 1.46 s, 1,334 ms
  of it `javac`, ahead of Gradle CC's 1.60 s. BEATING_THE_DAEMON_4.md §5
  measured what is left: the JIT warming up, which a daemon never pays.
- **`gen-ws`'s `clean`.** 715 ms against Gradle CC's 686 ms, ahead of
  `--parallel`'s 756 ms and the plain daemon's 795 ms, 79 ms above its
  members' critical path. `core`, at 224 ms, is most of what is left.
- **`gen-2000`'s `test-edit`.** 1.19 s against 1.06–1.16 s, 715 ms of it the
  test JVM. `test` and `test-edit` have swapped sides between the eleventh
  pass and this one, with the same test JVM taking 544–715 ms, so a full run
  has to say which side either is on.

`abi` left this list, and so did `gen-2000`'s `test`.

A full run (seven measured runs and two warm-ups per cell) is still what
BENCHMARKS.md §7 asks for before these become the published report.
