# jrs — Beating Gradle's warm daemon, step 3

What is left after [BEATING_THE_DAEMON_2.md](BEATING_THE_DAEMON_2.md),
whose §2–§5 and §7 are built, and what would close each remaining gap.
Everything here stays within SPEC §1.2: no process outlives the command.
The numbers are the fourth pass of
[BENCHMARKS_SUMMARY.md](BENCHMARKS_SUMMARY.md) (jrs 0.14.0, `cf0d36d`,
2026-10-17, one run per cell, no warm-up).

Status: **§2–§4 and §6 are built, and the fifth pass read them**
(BENCHMARKS_SUMMARY.md); what is left, §5 included, continues in
[BEATING_THE_DAEMON_4.md](BEATING_THE_DAEMON_4.md). The measurement §2
asks for first was not taken: the fourth pass's `javac` phase is about 600 ms longer in `clean` than in
`cold` on every project, whatever its size, which points at the assembly,
and the order of the benchmark's scenarios settled the rest:

- **§2.** Proposals 1–3 and 5 are built; proposal 4 is not, since the
  benchmark's `incremental`, the fourth command and a cell jrs leads by
  1.5–1.8×, would then run without the cache: no `test`, `run` or
  `package` comes before it, and its own quick compile never records.
  Proposal 5 is built as the main path rather than a fallback, and at a
  moment the plan did not list: the first command that finds no `javac`
  cache trains it on a thread of its own (`share::train_javac`), started
  before the resolution on a clean tree, over an embedded workload
  (`compile/Training.java` and the two workers' sources). The command
  joins it before exiting, so `cold`, which leads Maven by 1.7–3.8× and Gradle
  by 8–30×, pays up to the training's length beyond its own build,
  instead of `clean` paying the assembly after its compile. `jrs fetch` and
  `jrs jdk install` train both caches too, the moments §2.5 names; there is
  no `jrs warm`. A training that comes to nothing leaves the marker, and
  the next command records in a whole-unit compile, as before.
- **§2, the workers.** The first worker of a command that finds no worker
  cache records it, and no other worker in its pool (`share::claim`, one
  recorder per archive per command, which also covers forked `javac`s).
  Outside `--watch`, that worker stops after its first compile, its exit,
  dump and assembly on a background thread, and a new worker takes its
  place: in `gen-ws`'s `cold`, `core` trains the cache and the assembly
  overlaps the members after it. `jrs build` and `jrs run` retire every
  worker once the last member has compiled, and `Drop for Worker` stops
  the rest at once rather than in turn. Every AOT cache, the test JVM's
  included, is assembled on a thread that `cli::main` joins last.
- **§3.** The prestarted worker is primed with a one-class compile it
  answers while the graph is read, read back before its first real
  request. With the worker's cache made in `cold`, `clean` is the first
  command whose `core` and `model` compile in workers that map it.
- **§4.** The rounds were not counted. The rounds' worker now starts
  beside the first round when other sources name a class that round
  compiles, but only where its cache exists, which in a single-module
  project only `jrs fetch`, `jrs jdk install` or a workspace build on the
  same JDK makes: the benchmark's `gen-300` and `gen-2000` `abi` cells are
  unchanged by it.
- **§6.** Decided for the benchmark rather than the default: the
  generated projects set `share-classes = true`, recorded in BENCHMARKS.md
  §5. The default waits for the corpus check §6 asks for.

---

## 1. Where the daemon still wins

jrs beats every Gradle column in 34 of the 44 cells, and every
`--no-daemon` cell. A warm daemon (plain, with the configuration cache, or
with `--parallel`) wins these 10. The Gradle figure is the fastest of those
columns.

| Gap | Cell | jrs | Gradle | Part |
| --- | --- | --- | --- | --- |
| The caches are made inside a `build` | `wordstats` clean | 1.01 s | 412 ms (CC) | §2 |
| | `bookmarks` clean | 1.03 s | 569 ms | §2 |
| | `gen-300` clean | 1.35 s | 620 ms (CC) | §2, §5 |
| | `gen-2000` clean | 2.18 s | 1.19 s (CC) | §2, §5 |
| | `gen-ws` clean | 2.35 s | 690 ms (CC) | §2, §3 |
| The first members compile in cold workers | `gen-ws` abi | 511 ms | 454 ms (CC) | §3 |
| The first round of an API change forks a `javac` | `gen-2000` abi | 810 ms | 576 ms (CC) | §4 |
| | `gen-300` abi | 493 ms | 421 ms (CC) | §4 |
| The test JVM on a large suite | `gen-2000` test | 893 ms | 804 ms (CC) | §6 |
| | `gen-2000` test-edit | 1.19 s | 1.07 s | §6 |

`gen-ws`'s `clean` also loses to Maven (1.80 s), the only cell Maven wins.

BEATING_THE_DAEMON_2.md §2 took the cache out of the edit: `incremental` is
207–349 ms on every project, ahead of every Gradle column, and `cold` is
24–56% faster. But its option 3, recording on the second command that
finds no cache, moved the cost rather than removing it. In the benchmark
that second command is `clean`, a plain `build`, and every project's
`clean` is now 1.5–4.0× slower than in the third pass, where it led the
daemon on `wordstats` and `bookmarks`. BEATING_THE_DAEMON_2.md §8 already
says this cost should be fixed rather than averaged away by warm-ups: a
user's second build, and a CI job that runs `build` then `test` on an
empty cache, pay it.

Recommended order: §2 (one measurement, then the cheapest fixes that
measurement leaves standing), then §3 and §4. §5 and §6 narrow gaps rather
than close them. With §2 in, `clean` should return to the third pass's
250–604 ms on the small projects, and the daemon would keep about 5–6
cells, all on `gen-ws`, `gen-2000` and `gen-300`.

## 2. Make the caches where no build waits on them ✅

**Now.** The command that writes the `javac` AOT cache pays for it twice,
in series (`compile/share.rs`):

1. the training run carries `-XX:AOTMode=record`, which makes that
   `javac` slower;
2. `Share::finish` then runs `Assemble::run`, a second JVM
   (`-XX:AOTMode=create`), and waits for it.

In `clean`, `javac` takes 936–1,981 ms, more than in `cold` (334–1,366 ms),
where it compiles the same sources with nothing on disk.

The workspace has a worse version of this (`compile/worker.rs`):

- **Every pooled worker records.** `Worker::start` calls
  `Share::for_worker` per JVM, and in the recording command each worker
  gets recording flags of its own, though they all make the same cache,
  keyed by the JDK and the worker program.
- **The workers stop one after another.** `Drop for Worker` calls
  `running.stop()` in a loop, and each stop closes stdin, waits up to 10 s
  for the JVM to exit and dump, then assembles, before the next starts.

`gen-ws`'s members finish 1,247 ms into its `clean` (`--timings`), but the
command takes 2.35 s: 1.1 s after the last member, 1,174 ms above the
critical path, where the third pass was 136 ms above it.

**Measure first.** How the extra time splits between recording and
assembling decides which of the proposals below are needed. On `wordstats`
with a fresh `JRS_CACHE_DIR` and the `.seen` marker already in place, time
the recording `javac` against a plain one, and the `-XX:AOTMode=create` run
on its own. The same for one pooled worker on `gen-ws`. If assembly
dominates, proposals 1–4 take out nearly all of it; if recording does, only
proposal 5, or recording on a smaller compile, does.

**Proposal**, cheapest first:

1. **One recording worker per pool.** The first worker a command starts
   records; the others start with no recording flags, and any worker
   started after the cache exists maps it. Nothing is lost: they would all
   have written the same file.
2. **Stop workers at once, and early.** Stop the pool's workers in
   parallel rather than in turn. And stop a worker as soon as the command
   has nothing left for it to compile, the last member compiled, so its
   exit, dump and assembly overlap whatever the command still has to do:
   other members, resources, tests, packaging. The worker still never
   outlives the command (`Drop` joins it).
3. **Assemble on a thread, joined at the end of the command.** `finish`
   hands `Assemble::run` to a thread instead of running it inline, and the
   `Session` joins every such thread before the command returns. It is
   still done before jrs exits, so SPEC §1.2 holds, but it runs beside the
   rest of the command rather than in front of it.
4. **Write the cache only in a command with time to spare.** The second
   command that finds the marker records only if it is `test`, `run` or
   `package`, never `build`, where nothing comes after the compile for
   proposal 3 to hide behind. The project's first `jrs test` then makes the
   cache, its assembly overlapping a test JVM of 300–1,000 ms. A user who
   only ever runs `build` keeps the marker and records nothing, which costs
   them the cache's start-up saving and nothing else; if that matters,
   let the `n`th `build` record instead, for a small `n`.
5. **Train the cache outside any build.** `share.rs` says the `javac`
   cache holds no project's data and is keyed by the JDK alone, so jrs can
   train it on a small workload of its own, embedded as it embeds the
   worker program, at a moment the user already waits: `jrs jdk install`,
   the `setup-jrs` action, or an explicit `jrs warm`. Proposals 3 and 4
   stay as the fallback for anyone who never runs one. Whether a synthetic
   workload's method profiles (JEP 515) serve a real compile as well as
   the real compile's own is the open question; measure a one-file and a
   whole-unit compile under each cache before choosing.

**Not proposed.** Assembling in a detached process that jrs leaves running
when it exits would cost no command anything, but that process outlives
the command, which SPEC §1.2 rules out. It is a spec decision, not a code
change; it stays out unless SPEC §1.2 changes.

**Correctness.** None of this changes what `javac` writes; the
`reproducible` job still checks it. A cache is still put in place by
rename after its assembly succeeded, so a thread or a worker cut short
leaves no half-written file, and two commands assembling at once each
write their own temporary file.

**Expected.** `clean` back to the third pass's figures where nothing else
changed: 250 ms on `wordstats` and 329 ms on `bookmarks`, ahead of every
Gradle column; `gen-300` and `gen-2000` back to about 0.60 s and 1.45 s
(§5 is what is left there). `gen-ws`'s `clean` back to about its critical
path, 1.2–1.3 s, ahead of Maven again (§3 is what is left). `incremental`
and `cold` keep this pass's gains.

**Measured by.** `clean` on every project; `cold` and `incremental` must
not move; the "workspace members at once" table's distance from the
critical path. Also one CI-shaped run outside the benchmark: `build` then
`test` on an empty `JRS_CACHE_DIR`.

## 3. Warm the workspace's first members ✅

**Now.** BEATING_THE_DAEMON_2.md §3 gave the worker an AOT cache of its own
and starts the first worker while the graph is read. In this pass `core`
and `model` still take 383 ms and 439 ms of `gen-ws`'s clean build, about
100 ms more than in the third, while the members after them take
115–130 ms. They are on the chain `core → model → service → web → app`,
so they are most of the 1,178 ms critical path, against the daemon's
690–768 ms for the whole build. `abi` (511 ms, longest phase `core` at
186 ms) is 1–12% behind the daemon.

This pass's `clean` was also the command that recorded the workers'
cache, so `core` and `model` may have compiled in recording JVMs.

**Proposal.**

1. **Re-measure after §2** with the cache in place, member by member. If
   `core` and `model` fall to about the later members' 115–130 ms, this
   part is done.
2. **Otherwise, find what the first compile pays** that later ones do not:
   the worker's start, its first `JavacTool` and file manager, or the JIT.
   A first compile in a worker that maps its cache answered in 80 ms on
   one file (BEATING_THE_DAEMON_2.md, status); `core` is about 50 classes.
3. **Prime the first worker** while the graph is read: once it has started,
   send it a tiny compile (one embedded source, into a scratch directory
   jrs deletes) so the compiler's classes are initialised and its hot paths
   compiled before `core` arrives.

**Expected.** `core` and `model` near 150 ms each would put `clean` at
about 0.7–0.8 s, level with the daemon, and `abi` ahead of it.

**Measured by.** `clean` and `abi` on `gen-ws`; the "workspace members at
once" table.

## 4. The first round of an API change ✅

**Now.** `abi` on `gen-2000` is 810 ms (`javac` 580 ms) against 576–634 ms,
and on `gen-300` 493 ms (`javac` 389 ms) against 421–434 ms.
BEATING_THE_DAEMON_2.md §5 sends a single-module unit's rounds after the
first to a worker, but only once that worker's cache exists, and the
first round always forks. How many rounds `abi` takes on these projects
has still not been counted.

**Proposal.**

1. **Count the rounds** (`-v` prints them) on `gen-300` and `gen-2000`. If
   it is one round and then the transitive run, the time is in one forked
   `javac`, and only §2's cache or §5 below move it.
2. **If there are two or more**, start the rounds' worker at the start of
   the build, when the index shows an API change is likely (a changed
   source whose class others name), so its JVM starts while the first
   round's `javac` runs rather than after it.

**Expected.** `gen-300`'s `abi` level with the daemon; `gen-2000`'s closer,
by however much of its 580 ms of `javac` is JVM start rather than compile.

**Measured by.** `abi` on `bookmarks`, `gen-300` and `gen-2000`.

## 5. Large clean compiles

BEATING_THE_DAEMON_2.md §6, unchanged and still not built: whether
splitting a large unit across `javac` runs at once pays depends on how
independent `gen-2000`'s packages are, which nothing has measured. In the
third pass, before §2's cost moved into `clean`, `gen-2000`'s was 1.45 s
against the daemon's 1.24 s, and `gen-300`'s 604 ms against 588 ms with the
configuration cache. Re-read both after §2 before starting on it.

## 6. The test JVM on large suites ✅

**Now.** `gen-2000`'s `test` is 893 ms (699 ms of it the test JVM) against
804–842 ms, and `test-edit` 1.19 s (721 ms) against 1.07–1.12 s: 6–11%
behind. `test.share-classes = true` is the AOT cache from JDK 25
(BEATING_THE_DAEMON_2.md §7), but the benchmark's generated projects do not
set `share-classes`, so these cells do not use it.

**Proposal.** A decision rather than code, as `parallel-tests` was: either
set `share-classes = true` in the generated projects, recorded in
BENCHMARKS.md §5 with why, or make class-data sharing the test JVM's
default where it cannot lose (the AOT cache where it assembles, the archive
where it does not). The default is the better outcome, since users get it
too; it needs its own check that no suite in the corpus fails or slows
under it.

**Expected.** A 17–20% faster test JVM (TEST_JVM_BENCHMARK.md) would take
about 120–145 ms off both cells, enough to lead.

**Measured by.** `test` and `test-edit` on `gen-2000` and `bookmarks`.

## 7. Out of scope

- **A compiler daemon**, opt-in or not: BEATING_THE_DAEMON.md §9 and its
  gate, which should read a full run taken after §2–§4 here.
- **A detached assembler** (§2, not proposed).
- **The benchmark's settings.** A full run (seven measured runs, two
  warm-ups) would hide §2's cost in the warm-ups, as
  BEATING_THE_DAEMON_2.md §8 says; it should be fixed, not averaged away.
