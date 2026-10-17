# jrs — Beating Gradle's warm daemon, step 4

What is left after [BEATING_THE_DAEMON_3.md](BEATING_THE_DAEMON_3.md),
whose §2–§4 and §6 are built and were read by the fifth pass, and what
would close each remaining gap. Everything here stays within SPEC §1.2: no
process outlives the command. The numbers are the fifth pass of
[BENCHMARKS_SUMMARY.md](../BENCHMARKS_SUMMARY.md) (jrs 0.14.1, `56c0d97`,
2026-10-17, one run per cell, no warm-up), and the measurements below,
taken the same evening on the same machine.

Status: **§2 built, not yet benchmarked; §3, §4 and §6 proposed; §5's
split dropped on its measurements, its training proposed.** What §2's
code does and what it measured are in §2, under **Built**.

Every "measured" figure but §2's **Built** ones was taken outside the
benchmark, with the jrs release binary of `56c0d97`, on the generated
projects the harness writes (copied from its `work/` directory) and on
`examples/wordstats` and `examples/bookmarks`, each with an empty
`JRS_CACHE_DIR` of its own, reading the fifth pass's recorded mirror, the
commands in the benchmark's order (`cold`, `clean`, `noop`, the body edit
and back, then `abi`), three samples of each `abi`. They measure what a
change would bring before it is made; the benchmark still decides.

---

## 1. Where the daemon still wins

jrs beats every warm-daemon column in 37 of the 44 cells, and every Maven
and `--no-daemon` cell. A warm daemon (plain, with the configuration
cache, or with `--parallel`) wins these 7. The Gradle figure is the
fastest of those columns.

| Gap | Cell | jrs | Gradle | Part |
| --- | --- | --- | --- | --- |
| The rounds of an API change fork cold `javac`s | `wordstats` abi | 381 ms | 374 ms (CC) | §2 (built) |
| | `bookmarks` abi | 431 ms | 403 ms (CC) | §2 (built) |
| | `gen-300` abi | 517 ms | 415 ms (CC) | §2 (built), §3 |
| | `gen-2000` abi | 895 ms | 595 ms (CC) | §2 (built), §3, §5 |
| A worker's first compile | `gen-ws` abi | 512 ms | 422 ms (CC) | §4 |
| | `gen-ws` clean | 817 ms | 664 ms (CC) | §4 |
| A large compile in a cold JIT | `gen-2000` clean | 1.54 s | 1.21 s (CC) | §3, §5 |

BEATING_THE_DAEMON_3.md §2 took the cost of making the caches out of
`clean`, which now leads the daemon on `wordstats`, `bookmarks` and
`gen-300`, and §6 took the test JVM's: `gen-2000`'s `test` and `test-edit`
now lead. Its §3 and §4 did not move their cells, for the reasons §2 and
§4 below find. The price of its §2 is `cold`, which §6 takes up.

Recommended order: §2 (one change, three cells), §3 (instrument, then the
cheapest fix), §4, then §5 and §6 together, since they pull the training
in opposite directions. With §2–§5 in, the daemon should keep one cell,
`gen-2000`'s `abi`, and lose it by less.

## 2. A worker cache for a single-module project ✅

**Now.** BEATING_THE_DAEMON_3.md §4 asked for the rounds to be counted.
Every `abi` cell is **two rounds**: the edited class alone, then the
sources that name it. Both rounds fork a `javac`, since the rounds'
worker is only used where its AOT cache exists (`worker_for_rounds`,
`compile/incremental.rs`), and in a single-module project nothing the
benchmark runs makes it: the first command's training
(`share::train_javac`) makes the `javac` cache only; `jrs fetch` and
`jrs jdk install` make both.

| `abi` | Round 1 | Round 2 | Total |
| --- | --- | --- | --- |
| `wordstats` | 128–130 ms | 174–179 ms | 368–377 ms |
| `bookmarks` | 148–152 ms | 181–200 ms | 409–432 ms |
| `gen-300` | 145–146 ms (1 file) | 243–248 ms (34 files) | 486–489 ms |
| `gen-2000` | 172–187 ms (1 file) | 456–480 ms (119 files) | 837–888 ms |

`gen-2000`'s second round is over `QUICK_SOURCES` (100), so it forks with
the full JIT.

**Measured.** The same commands with `jrs fetch` run before `abi`, so the
worker cache exists. The rounds' worker then starts beside the first
round (`is_named`), is primed, and answers the second:

| `abi` | No worker cache | Worker cache | Round 2 | Gradle's best |
| --- | --- | --- | --- | --- |
| `wordstats` | 368–377 ms | 297–299 ms | 93–94 ms | 374 ms |
| `bookmarks` | 409–432 ms | 333–336 ms | 98–99 ms | 403 ms |
| `gen-300` | 486–489 ms | 428–434 ms | 170–171 ms | 415 ms |
| `gen-2000` | 837–888 ms | 770–782 ms | 372–374 ms | 595 ms |

**Proposal.** Make the worker cache where the `javac` cache is made: the
first command's training trains both, as `jrs fetch` does, over the
workload it already embeds (the workers' sources are part of it). The
rounds' worker then qualifies from the second command on.

**Cost.** The training is longer, and `cold` waits for it (§6). Measure
the two trainings' length together and apart before choosing; if the
worker's adds to `cold`, train it on its own thread, joined last, as the
`javac` cache's is.

**Expected.** `wordstats` and `bookmarks` lead by 17–20%, `gen-300` level
(428–434 ms against 415–434 ms), `gen-2000` 10% closer.

**Measured by.** `abi` on every project; `cold` on the three small ones.

**Built.** `Worker::train_beside` (`compile/worker.rs`) runs the training
`Worker::train` does for `jrs fetch`, on a thread of `share::background`,
which the command joins at its end; `Session::train_javac` starts it
beside `javac`'s, in a session that holds no workers of its own (a
workspace's pool and `--watch` still record theirs). It is gated by
`share::worker_training_due`: no archive there, the `.seen` marker unless
`always`, and once per command, though a clean build asks twice. Until
the worker's program is built the archive's name holds none of its size
and time, so that first marker stands for that look alone.

Measured the same way as above, three samples each, before and after,
with `cold` timed from a cache that holds the jars but no compiler cache:

| | `abi` before | `abi` after | `cold` before | `cold` after |
| --- | --- | --- | --- | --- |
| `wordstats` | 371–375 ms | 300–301 ms | 1.01–1.03 s | 1.26–1.32 s |
| `bookmarks` | 412–416 ms | 337–342 ms | 0.99–1.06 s | 1.25–1.32 s |
| `gen-300` | 498–503 ms | 431–438 ms | 1.03–1.05 s | 1.28–1.35 s |
| `gen-2000` | 871–877 ms | 779–787 ms | 1.58–1.60 s | 1.63–1.64 s |

`abi` is what the experiment said; `clean` and `noop` do not move. The
price is `cold`, by 0.25–0.3 s on the three small projects; two first
samples of 3.2 s and 4.2 s did not come back in twelve more. The worker's
training is probably the longer chain because it compiles the worker's
program first; §6 takes that up. The benchmark has not read it yet.

## 3. Bookkeeping that reads every class

**Now.** After each round, `Incremental::record` (`compile/incremental.rs`)
walks the whole output directory and parses every `.class` file, one at a
time, to rebuild the index; `record_all` does the same after a whole-unit
compile. Its cost grows with the project, not with the change. From the
fifth pass's `--timings` (BENCHMARKS_SUMMARY.md and its report), the time
a build spends outside `javac`:

| | `noop` | `incremental` (1 round) | `abi` (2 rounds) | `clean` |
| --- | --- | --- | --- | --- |
| `gen-300` | 67 ms | 94 ms | 102 ms (with §2: about 105) | 96 ms |
| `gen-2000` | 80 ms | 194 ms | 232 ms (with §2: about 220) | 197 ms |

On `gen-2000` a one-round build spends about 110 ms more outside `javac`
than a `noop`, and `abi` about 150 ms. Not all of it is `record`: the
sweep, the index's save and the fingerprint's `api_digest`, which also
reads the classes, are in it too. Nothing splits it yet.

**Proposal.**

1. **Time it.** A `--timings` row for the index (and one for the digest)
   per unit, so the next pass shows the split.
2. **Read only what a round compiled.** The index already holds every
   other source's classes; a round replaces the entries of the sources it
   compiled, and of those whose classes it removed, and keeps the rest. A
   whole-unit compile still reads everything.
3. **Read in parallel**, with `rayon`, already a dependency, where a
   whole-unit compile has to read everything: parsing a class file is
   independent of every other.
4. **Share one read** between the index and `api_digest` where both run
   in one command.

**Expected.** Most of the 110–150 ms on `gen-2000`'s `incremental` and
`abi`, and a part of `clean`'s 197 ms; little on the small projects.

**Measured by.** The new timing rows first; then `incremental`, `abi` and
`clean` on `gen-2000`.

## 4. A worker's first compile

**Now.** `gen-ws`'s members are 50 sources each, but in every clean build
the first two take twice as long as the rest. The same on three clean
builds after `cold`, with both AOT caches present:

| `clean` | `core` | `model` | `store` | `service` | `web` | `app` | Total |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | 213 ms | 237 ms | 95 ms | 107 ms | 107 ms | 102 ms | 832 ms |
| 2 | 211 ms | 238 ms | 92 ms | 108 ms | 102 ms | 104 ms | 826 ms |
| 3 | 207 ms | 250 ms | 93 ms | 108 ms | 101 ms | 102 ms | 833 ms |

So it is not a missing cache. `-v` shows why:

- Worker 1 is prestarted and primed, compiles `core` (its first compile),
  then `store`, in about 92 ms.
- `model` and `store` both depend on `core` alone, so they are ready at
  once. `store` takes the idle, warm worker 1; `model` finds no worker
  idle and starts worker 2, unprimed, so it pays that JVM's start and its
  first compile. Worker 2 then compiles `service`, `web` and `app` in
  about 100 ms each.
- A worker's first real compile costs about twice its second, primed or
  not: the one-class prime (`PRIME`, `compile/worker.rs`) warms the
  compiler's classes but not its hot paths.

The chain `core → model → service → web → app` is 740 ms, against the
daemon's 664–746 ms for the whole build; `abi` is 512 ms, with `core`
184 ms of it.

**Proposal.**

1. **Prestart as many workers as the member graph's widest level** (two
   here), not one, and prime each, so no member waits for a JVM to start.
2. **Prime with real work.** Send the embedded training workload
   (`compile/Training.java`), not one class, to every worker but the one
   the first member needs at once. A worker started beside `core` has
   `core`'s whole compile to warm up in before `model` or `store` is
   ready.
3. **Hand ready members to the warmest worker**, longest remaining path
   first: a worker that has answered a compile before takes the member on
   the critical path.

**Expected.** `model` and `store` near 100 ms put the chain at about
620 ms and `clean` at about 0.7 s, level with the daemon's 664–746 ms;
`core` near 100 ms as well (point 2, if the first worker can be warmed
during resolution) puts it ahead. `abi` follows `core`.

**Measured by.** `clean` and `abi` on `gen-ws`; the members' times in the
report's "workspace members at once" table.

## 5. A large compile in a cold JIT

**Now.** `gen-2000`'s `clean` is one forked `javac` of 2,000 sources
(1,338 ms in the fifth pass) against the daemon's whole build in 1.21–1.27 s.
BEATING_THE_DAEMON_2.md §6 proposed splitting a unit across `javac` runs
at once, and `gen-2000` is the best case for it: its 100 packages are
`p00` and 99 that depend on `p00` alone.

**Measured: splitting does not pay.** jrs's own argfile, its sources
split by package into _k_ groups run at once, each with `-sourcepath` and
`-implicit:none`, on the same AOT cache:

| _k_ | 1 | 2 | 3 | 4 | 6 | 8 |
| --- | --- | --- | --- | --- | --- | --- |
| Wall time | 1.28–1.33 s | 1.22–1.23 s | 1.22–1.26 s | 1.32–1.35 s | 1.47–1.53 s | 1.80–1.87 s |

One `javac` already keeps about 3.3 cores busy (4.2–4.5 s of CPU in
1.26–1.34 s), most of it the JIT's compiler threads; more runs means more
JITs warming up at once, on the same cores. Fewer compiler threads
(`CICompilerCount=2`, 1.33 s), C1 only with the serial collector (1.49 s)
or the parallel collector (1.26 s) do not help either. The time is the
JIT warming up, which is the one thing a daemon never pays.

**Measured: a richer training does.** An AOT cache trained on
`gen-2000`'s own compile (`-XX:AOTCacheOutput`), against jrs's, trained on
the embedded workload:

| Whole-unit `javac` | jrs's cache | Trained on `gen-2000` |
| --- | --- | --- |
| `gen-2000` | 1.27–1.34 s | 1.04–1.06 s |
| `gen-300` | 0.48 s | 0.42 s |

The gain holds on a project the cache was not trained on (if one from the
same generator), so it is not the cache knowing the sources: it is the
method profiles (JDK 25, JEP 515) of a compile large enough to have hot
paths.

**Proposal.**

1. ✅ **Drop the split** (BEATING_THE_DAEMON_2.md §6) on these numbers.
   Done: §7 lists it as out of scope.
2. **Make the training workload a real compile's size**: a few hundred
   generated classes rather than one file and the workers' sources, the
   generator embedded in jrs as `Training.java` is. Measure the cache it
   makes against the one trained on `gen-2000`, and how long it takes.

**Expected.** About 0.25 s off `gen-2000`'s `clean` (1.54 s to about
1.3 s, level with the daemon's 1.21–1.27 s), more with §3; about 60 ms off
`gen-300`'s, and some of `abi`'s and `cold`'s `javac` everywhere.

**Measured by.** `clean` and `abi` on `gen-2000` and `gen-300`; `cold` for
the training's length (§6).

## 6. `cold`

Not a daemon cell, which `cold` leads by 8–13×, but the price of
BEATING_THE_DAEMON_3.md §2: on `wordstats`, `bookmarks` and `gen-300` the
command takes 980 ms–1.05 s where its `--timings` total is 451–840 ms,
because it waits for the training it started. It still leads Maven there
by 1.7–2.1×; it led by 2.7–4.0× in the fourth pass.

§2 and §5 both make the training longer. Before choosing:

1. **Measure the training alone**: the embedded workload, §2's worker
   training, and §5's larger workload, each on its own thread.
2. **Start it sooner, or make it less**: it starts before resolution on
   a clean tree; on a project whose own compile is shorter than the
   training, a smaller first training and a richer one in a later command
   that has its own compile to hide it behind (the second `build`, as
   BEATING_THE_DAEMON_3.md §2 proposal 4 had it) may cost no command
   anything. That moved the cost into `clean` once; measure before
   repeating it.

**Measured by.** `cold` on every project, against `clean` and `abi`.

## 7. Out of scope

- **A compiler daemon**, opt-in or not: BEATING_THE_DAEMON.md §9. §5's
  numbers are the clearest case for one yet; they are also the case for
  the training, which needs no process that outlives the command.
- **Splitting a unit across `javac` runs**: measured in §5, and dropped.
- **The benchmark's settings.** Two of the seven cells (`wordstats` and
  `bookmarks` `abi`) are 2% and 7% behind in one run each; a full run
  (seven measured runs, two warm-ups) says whether they are losses before
  §2 is credited with fixing them.
