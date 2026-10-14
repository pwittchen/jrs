# Benchmarks against Maven and Gradle: first results

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
  [GRADLE_GAPS.md](todo/GRADLE_GAPS.md); a warm in-process `javac`
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
