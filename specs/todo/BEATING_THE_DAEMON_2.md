# jrs — Beating Gradle's warm daemon, step 2

What is left after [BEATING_THE_DAEMON.md](BEATING_THE_DAEMON.md), whose
§2–§7 are built, and what would close each remaining gap. Everything here
stays within SPEC §1.2: no process outlives the command. The numbers are
the third pass of [BENCHMARKS_SUMMARY.md](../BENCHMARKS_SUMMARY.md) (jrs
0.13.0, `f52f4d6`, 2026-10-16, one run per cell, no warm-up).

Status: **§2–§5 and §7 built, not yet benchmarked; §6 not built.** The
expected figures below are still estimates until a benchmark pass reads
them. What was built, and what was measured on the way (JDK 25, macOS,
one-file compiles, five runs each, outside the benchmark):

- **§2.** Quick flags against the full JIT, both with a cache: 74–78 ms
  against 83–87 ms for one file, 99–101 ms against 105–111 ms for 20. The
  quick flags still pay, so they stay; but the cache the full flags
  recorded maps under the quick flags too (76–77 ms, its heap region
  loaded), so there is one cache, keyed by the JDK alone (the key the
  full-flags cache already had). Only a whole-unit compile writes it, and
  only on the second command that finds none: the first leaves
  `<archive>.seen` (option 3). Before JDK 25 the dynamic archive follows
  the same two rules.
- **§3.** A worker with no cache answers its first one-file request
  176 ms after it is spawned and later ones in 13–16 ms; with an AOT cache
  of its own, 80 ms and 11–14 ms; the forked `javac`'s cache does nothing
  for it (200 ms), since the worker runs another main. So the worker gets
  its own AOT cache (`Share::for_worker`), recorded over its life by the
  second command that finds none and assembled when it exits. A workspace
  command starts its first worker when it reads the graph
  (`Driver::warm_up`); one that compiled nothing is killed at the end, and
  so is one with no archive to write, rather than waited for. A few files
  go to a forked `javac` instead of a one-command worker that would start
  cold, when the fork maps its cache (proposal 3).
- **§4.** `gen-ws`'s generated `jrs.toml` sets `parallel-tests = true`;
  BENCHMARKS.md §5 records why.
- **§5.** A unit without a worker of its own sends its rounds after the
  first to one (`Worker::for_rounds`), but only when that worker's AOT
  cache is already there: a cold one costs its first round about twice a
  forked `javac`. It never records, since its exit is on the edit's path,
  so a single-module project gets it once any workspace command, or a
  `--watch` session, has made the worker's cache in the shared cache.
  The number of rounds `abi` takes on `gen-300` and `gen-2000` is still to
  be counted (`-v` prints it).
- **§6.** Not built: whether splitting pays depends on how independent
  `gen-2000`'s packages are, which nothing has measured yet.
- **§7.** `test.share-classes = true` is now the AOT cache from JDK 25, and
  a passing run whose record cannot be assembled leaves
  `<cache>.unassembled`, after which that key gets the dynamic archive.
  `"archive"` is new and keeps the dynamic archive on every JDK. The
  benchmark's generated projects do not set `share-classes`, so the
  `gen-2000` cells only move if they do, which is a §5 fairness decision
  of its own (BENCHMARKS.md), like §4's.

---

## 1. Where the daemon still wins

jrs beats every Gradle column in 30 of the 44 cells. A warm daemon (plain,
with the configuration cache, or with `--parallel`) wins these 14. The
Gradle figure is the fastest of those columns.

| Gap | Cell | jrs | Gradle | Part |
| --- | --- | --- | --- | --- |
| The `javac` AOT cache is made inside an edit | `wordstats` incremental | 849 ms | 370 ms (CC) | §2 |
| | `bookmarks` incremental | 880 ms | 386 ms (CC) | §2 |
| | `gen-300` incremental | 844 ms | 391 ms (CC) | §2 |
| | `gen-2000` incremental | 1.11 s | 563 ms (CC) | §2 |
| The first members compile in cold workers | `gen-ws` clean | 1.13 s | 693 ms (CC) | §3 |
| | `gen-ws` abi | 667 ms | 449 ms (CC) | §3 |
| Members' tests run one after another | `gen-ws` test | 1.81 s | 714 ms (CC) | §4 |
| Each round of an API change forks a `javac` | `gen-2000` abi | 822 ms | 590 ms (CC) | §5 |
| | `gen-300` abi | 501 ms | 404 ms (CC) | §5 |
| | `bookmarks` abi | 404 ms | 382 ms (CC) | §5 |
| A long compile in a cold `javac` | `gen-2000` clean | 1.45 s | 1.24 s | §6 |
| | `gen-300` clean | 604 ms | 588 ms (CC) | §6 |
| The test JVM on a large suite | `gen-2000` test | 856 ms | 802 ms (CC) | §7 |
| | `gen-2000` test-edit | 1.17 s | 1.06 s | §7 |

Recommended order: §2 first (small, contained, four cells and `cold`),
then §3 and §4, then §5. §6 and §7 narrow gaps rather than close them. With
§2–§4 in, the daemon would keep about 5–7 cells, all compile- or test-bound
work on `gen-2000` and `gen-300`.

## 2. Make the `javac` AOT cache off the edit's critical path ✅
**Now.** In the second pass `incremental` was 245–394 ms and ahead of the
daemon on every project; in the third it is 844 ms–1.11 s on the four
single-module projects. The extra time is all in one forked `javac`
(750–921 ms), while `test-edit` compiles the same edit in 137–166 ms.
Reproduced on `wordstats` with a fresh `JRS_CACHE_DIR`:

| Build | `javac` | AOT caches in `cds/` |
| --- | --- | --- |
| cold | 876 ms | the full-flags cache is made |
| clean | 202 ms | maps it |
| first edit | 710 ms | a second cache is made |
| second and third edit | 136 ms, 137 ms | maps it |
| clean again | 190 ms | maps the first |

A compile of `QUICK_SOURCES` (100) files or fewer forks `javac` with
`QUICK_JVM_FLAGS` (`-XX:TieredStopAtLevel=1 -XX:+UseSerialGC`,
`compile/javac.rs`), and `Share::for_javac` (`compile/share.rs`) keys the
cache by the JVM flags. So the first small compile records and assembles a
cache of its own, about 570 ms once. With both caches in place an edit is
136 ms, well ahead of the daemon's 370–387 ms: this scenario is a win as
soon as the cache stops being made inside it.

`cold` carries the same cost for the full-flags cache: `javac` 876–1957 ms
in `cold` against 182–1136 ms in `clean`, which is why `cold` went 19–76%
slower between the second and third passes. On CI with an empty cache that
cost comes back on every run and is never paid back.

**Proposal**, cheapest first:

1. **Measure whether the quick flags still pay once there is a cache.**
   They were tuned (`compile/incremental.rs`, `QUICK_SOURCES`) before the
   AOT cache existed. If a one-file compile with the full flags and the
   full cache comes close to 136 ms, drop the quick flags whenever a cache
   is in use: one cache, one key, no second training run.
2. **Otherwise, borrow the full cache.** When the quick cache is missing
   but the full one exists, compile with the full flags and that cache
   rather than recording. A whole clean compile with it is 202 ms, so a
   one-file compile should land well under the daemon. Record the quick
   cache only where it holds up no edit, or never.
3. **Record on the second build that finds no cache, not the first.** A
   marker file in `cds/` says a JDK and key have been seen once; the next
   forked run records. A one-off build, and every CI run with an empty
   cache, then pays nothing.

**Correctness.** None of this changes what `javac` writes; the
`reproducible` job still checks it.

**Expected.** `incremental` back to 136–200 ms on the single-module
projects, ahead of every Gradle column; `cold` back to the second pass's
0.55–1.91 s.

**Measured by.** `incremental` and `cold` on `wordstats`, `bookmarks`,
`gen-300` and `gen-2000`; `cargo bench --bench incremental`.

## 3. Warm the workspace's first members ✅
**Now.** With the `javac` worker pool (BEATING_THE_DAEMON.md §6), the
members after the first two take 123–147 ms each in `gen-ws`'s clean
build, but `core` (285 ms) and `model` (316 ms) are still the pool's first
compiles. They sit on the chain `core → model → service → web → app`, so
they are most of the 995 ms critical path; jrs's clean build is 1.13 s
against Gradle `--parallel`'s 738 ms.

The worker JVM maps a dynamic CDS archive (`Share::new` in
`compile/worker.rs`, `Worker::start`), not the JEP 483 AOT cache with the
method profiles of JEP 515 that a forked `javac` gets from
`Share::for_javac`.

**Proposal.**

1. **Give the worker the AOT cache** on JDK 25 and later, made the way
   `Share::for_javac` makes the forked `javac`'s, keyed by the worker
   program as well as the JDK and the flags.
2. **Start the pool when the command starts**, not on the first member's
   compile, so the JVMs start while the lockfile is read and the
   classpath assembled. Check first where `Worker::javac_only`
   (`cli/members.rs`) starts its JVMs today.
3. **Look at `gen-ws` `incremental`** while here: BEATING_THE_DAEMON.md
   notes it went from 276 to 305 ms because a one-file compile in a worker
   costs more than a forked `javac` with the quick flags. After §2 a
   forked `javac` with a cache is 136 ms; a member with one stale source
   may do better forked.

**Expected.** `core` and `model` near 150 ms each would put `clean` at about
0.7–0.8 s, level with Gradle `--parallel`, and take `abi` (667 ms, whose
longest phase is `core` at 240 ms) towards the daemon's 449–492 ms.

**Measured by.** `clean`, `abi` and `incremental` on `gen-ws`; the
"workspace members at once" table.

## 4. `parallel-tests` in the benchmark's workspace ✅
**Now.** `[workspace] parallel-tests = true` is built
(BEATING_THE_DAEMON.md §4) but off by default, and `gen-ws` does not set
it, so `test` is still six test JVMs of 273–283 ms each in sequence: 1.81 s,
against 714 ms with Gradle's configuration cache and 756 ms with
`--parallel`, both of which run the members' test tasks at once.

**Proposal.** A decision rather than code: set `parallel-tests = true` in
`gen-ws`'s generated `jrs.toml`, since the Gradle columns it is compared
with already run members' tests at once. BENCHMARKS.md's fairness rules
(§5) should record the setting and why. Making it the default
instead stays out of scope: suites that share a port, a database or a fixed
file would race.

**Expected.** About one test JVM plus scheduling, 350–450 ms, ahead of
every Gradle column.

**Measured by.** `test` on `gen-ws`.

## 5. One worker for the rounds of a single-module build ✅
**Now.** An API change compiles in rounds (BEATING_THE_DAEMON.md §2), which
took `abi` from 1.87 s to 822 ms on `gen-2000` and from 852 to 501 ms on
`gen-300`. But a single-module build forks a `javac` per round; only a
workspace command holds a worker. `gen-2000`'s `abi` spends 586 ms in
`javac` against the daemon's 590–623 ms in all.

**Proposal.** When a build takes a second round, start a worker for the
rest of the command, as a workspace command holds one, so later rounds
skip the JVM start. It dies with the command and any failure falls back to
a fork, as now. Count the rounds `abi` takes on `gen-300` and `gen-2000`
first: if it is one round and the transitive run, the gain is in §2's
cache instead.

**Expected.** `gen-2000` `abi` below the daemon's 590 ms if it takes two or
more rounds; `gen-300` and `bookmarks` (404 ms against 382 ms) level or
ahead.

**Measured by.** `abi` on `bookmarks`, `gen-300` and `gen-2000`.

## 6. Large clean compiles

**Now.** `gen-2000`'s `clean` is 1.45 s, 1136 ms of it `javac`, against the
daemon's 1.24 s; `gen-300`'s is 604 ms against 588 ms with the
configuration cache. The daemon's `javac` has been JIT-warmed by earlier
builds, which only a process kept between commands gives; that is
BEATING_THE_DAEMON.md §9, behind its gate.

**Proposal**, within SPEC §1.2:

1. **Split a large unit across `javac` runs at once**, where the package
   graph has independent parts: each run compiles its part against the
   classes of the parts it depends on. This is only worth it above a size
   threshold, and the parts must be ordered, so it pays on wide graphs, not
   on a chain.
2. **Otherwise accept these cells** as the cost of having no daemon.

**Expected.** Unclear without measuring how independent `gen-2000`'s
packages are.

**Measured by.** `clean` on `gen-300` and `gen-2000`.

## 7. The test JVM on large suites ✅
**Now.** `gen-2000`'s `test` is 856 ms (643 ms test JVM) against 802 ms,
and `test-edit` 1.17 s (703 ms test JVM) against 1.06 s: 2–10% behind.

**Proposal.** [TEST_JVM_BENCHMARK.md](../TEST_JVM_BENCHMARK.md) finds
`test.share-classes = "aot"` 17–20% faster than the archive wherever it can
be assembled, and it stays opt-in only because it never assembles on the
Spring Boot projects. Use `"aot"` where it assembles and fall back to the
archive, or nothing, where it does not, so a project gets it without
being able to lose by it.

**Expected.** A 17–20% faster test JVM would take about 110–140 ms off
both cells, enough to lead.

**Measured by.** `test` and `test-edit` on `gen-2000` and `bookmarks`.

## 8. Out of scope

- **A compiler daemon**, opt-in or not: BEATING_THE_DAEMON.md §9 and its
  gate, which should read a full run taken after §2–§5 here.
- **`parallel-tests` on by default** (§4).
- **The benchmark's settings.** A full run (seven measured runs, two
  warm-ups) would hide §2's one-off cost in the warm-ups; it should be
  fixed rather than averaged away, since a user's first edit and every CI
  run pay it.
