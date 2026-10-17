# jrs — Beating Gradle's warm daemon

Where a warm Gradle daemon still beats jrs in the benchmark of
[BENCHMARKS.md](../BENCHMARKS.md), why, and what would close each gap. §2–§7
stay within SPEC §1.2, which rules out a compiler daemon; §9 is the opt-in
daemon to reach for only if they are not enough, and it needs a SPEC
decision first. The numbers are the second pass of
[BENCHMARKS_SUMMARY.md](../BENCHMARKS_SUMMARY.md) (jrs 0.12.0, 2026-10-15, one
run per cell).

Status: **§2–§7 built, not yet measured in a full run; §9 proposed.** §9
needs a SPEC.md decision first, and its gate (§9.1) reads the numbers of a
full run (seven measured runs, two warm-ups) over what §2–§6 built. Every
expected gain below is still an estimate from the phase timings until that
run; each part names the cells that measure it, and each says under
**Built** what the code does where it differs from the proposal.

A first jrs-only pass (five runs, two warm-ups, JDK 25, M4 Max; not the
full run §1 asks for) against the commit before: `abi` 856 ms → 501 ms on
`gen-300`, 1.80 s → 827 ms on `gen-2000`, 1.78 s → 676 ms on `gen-ws`;
`clean` 661 → 527 ms, 1.49 → 1.29 s, 1.66 → 1.03 s. `gen-ws`
`incremental` went from 276 to 305 ms: a one-file compile in the
workspace's worker JVM costs more than a forked `javac` with the quick
flags. `test` did not move, since `parallel-tests` is off in the
benchmark's projects.

---

## 1. Where the daemon wins

| Gap | Cell | jrs | Gradle (daemon) | Part |
| --- | --- | --- | --- | --- |
| An API change recompiles too much | `gen-2000` abi | 1.87 s | 654 ms | §2 |
| | `gen-300` abi | 852 ms | 466 ms | §2 |
| A member recompiles whole after an upstream API change | `gen-ws` abi | 1.77 s | 505 ms | §3 |
| | `gen-ws` clean | 1.70 s | 803 ms | §3, §6 |
| Members' tests run one after another | `gen-ws` test | 1.80 s | 716 ms (CC) | §4 |
| Spring Boot's fat jar | `bookmarks` package | 753 ms | 515 ms | §5 |
| A long compile in a cold `javac` | `gen-2000` clean | 1.53 s | 1.28 s | §6 |
| The test JVM on a large suite | `gen-2000` test | 901 ms | 831 ms | §7 |

Recommended order: §4 first (small and self-contained, one large win), then
§2, which §3 builds on. §5 is independent. §6 and §7 narrow gaps rather
than close them. §9 comes last, and only if its gate (§9.1) holds once the
rest are measured.

## 2. Stop the recompile walk at an unchanged API ✅

**Now.** `compile/incremental.rs` compiles a changed source, and when its
API changed, everything that transitively refers to it. On `gen-2000` a
public method added to `C0000`, which every package reaches, recompiles
1583 ms of `javac` for what is a one-line change.

**Gradle.** Its incremental compiler recompiles the classes that refer to
the changed class directly and does not follow the chain further when their
own API is unchanged.

**Proposal.** Compile in rounds. Round one: the changed sources and their
direct dependents. After each round, compute the `compile::api_digest` of
each recompiled class (`compile/abi.rs`) and compare it with the index.
Only classes whose API changed add their own direct dependents to the next
round. Stop when a round changes no API.

**Correctness.** This keeps "errs towards recompiling". Anything the index
cannot account for still compiles the unit whole: a changed constant
(inlined by `javac`), a processor, another language, an added or deleted
source. Adding a method can change overload resolution in a direct
dependent, which round one compiles anyway. Whether a removed or narrowed
member needs more than direct dependents should be checked against the JLS
§13 binary-compatibility rules before this is built; if in doubt, keep the
transitive walk for removals and use rounds only for additions.

**Cost.** Each round is a `javac` invocation. A worker that stays alive for
the command (§6) keeps the later rounds cheap. Without one, cap the number
of rounds and fall back to the transitive set.

**Expected.** Around the cost of a few small compiles. A one-file edit on
`gen-2000` is 394 ms, so `abi` plausibly lands below the daemon's 654 ms.

**Measured by.** `abi` on `gen-300` and `gen-2000`.

**Built.** `compile/incremental.rs`, index format 2. Each class's API is
recorded as a header digest (flags, supertypes, signature, annotations,
nesting) and a digest per visible member (`abi::ClassInfo::header`,
`members`), with its supertypes, which is what tells an addition from the
rest. An addition takes rounds: the next round compiles the sources that
refer to the class or to a subtype of it, since a subtype inherits what was
added and a new overload can change what its callers pick. A removal, a
changed member or header, or a fifth round compiles the transitive set in
one more run, as before. The rounds use the command's worker when it has
one (§6); a single-module build forks each.

## 3. Compile avoidance across workspace members ✅

**Now.** A member's main unit fingerprints its upstream members'
`api_digest`, so an API change in `core` recompiles every downstream member
whole. The `gen-ws` `abi` phases show it: `core` 368 ms, then `model`,
`store`, `service`, `web` and `app` each about 310 ms of `javac`. That is
1.77 s for one added method.

**Proposal.** Extend §2 across the member boundary. Record in each member's
index which upstream classes its sources refer to (the constant pools
already hold this, as `compile/impact.rs` reads them for test selection).
When an upstream member's API changes, a downstream member compiles only
the sources that refer to the classes whose API changed, then applies §2's
rounds inside itself. A member none of whose sources refers to a changed
class is fresh, and its own `api_digest` is unchanged, so its downstream
members are unaffected by it.

**Correctness.** As §2. In addition, a downstream member whose index is
missing or older than the upstream's must compile whole.

**Expected.** On `gen-ws`, where `core`'s `C0000` is reached by every
package, most members still compile something, but a few files each
rather than 300 classes. Plausibly under 0.8 s, against the daemon's
505 ms. A graph whose leaves do not touch the changed class gains more.

**Measured by.** `abi` and `test-edit` on `gen-ws`.

**Built.** `CompileUnit::tracked`: a member's main unit tracks its upstream
members' class directories, and the test unit tracks `target/classes` and
the upstream ones too, so a main API change no longer compiles the tests
whole either. `main_api` stays in the fingerprint, which makes the unit
stale, but not in the settings the index is taken under. The index records
each tracked class the sources refer to and its supertypes there (`seen`),
and a digest of the tracked directories' top-level classes: one appearing or
going away compiles the unit whole, as does a changed constant there or a
processor or Groovy AST transformation among them. A missing or older index
compiles whole, since it holds no `seen` entries to compare.

## 4. Members' test JVMs at once ✅

**Now.** `cli/members.rs` builds every member at once, then tests member by
member. On `gen-ws`, `test` is six test JVMs of about 280 ms each in
sequence: 1.80 s, against 1.85 s for the daemon but 716 ms with its
configuration cache and 729 ms with `--parallel`.

**Proposal.** Run the members' test JVMs at once, up to `--jobs`, through
the same `Ui::deferred` gating builds use, so the transcript stays the one
`--jobs 1` prints. A failing member's output is released in build order
like any other. `--fail-fast` stops scheduling new members once one fails.

**Open.** Tests that share an external resource (a port, a database, a
fixed file) across members would start racing. Gradle has the same problem
with `--parallel` and `maxParallelForks`. Either accept it with a
per-workspace opt-out (`[workspace] parallel-tests = false`), or default
to sequential and make it opt-in. The second is the safer first step.

**Expected.** About one test JVM plus scheduling: 350–450 ms on `gen-ws`,
ahead of every Gradle column.

**Measured by.** `test` on `gen-ws`.

**Built** as the opt-in: `[workspace] parallel-tests = true`, off by
default (`cli/members.rs`, `Driver::at_once_until`). The builds still go
first, at once; the tests then run at once through the same gates, so the
transcript is the one member-by-member testing prints.

## 5. Spring Boot's fat jar ✅

**Now.** `package` on `bookmarks` is 753 ms, 684 ms of it packaging:
`package.rs` inflates and deflates every entry of every dependency jar.
`bootJar` takes 515 ms because it stores the dependency jars nested, as
they are.

**Proposal**, cheapest first:

1. **Raw copy of deflated entries.** An entry that is copied unchanged (not
   a merged `META-INF/services/*` or Spring registry file, not relocated)
   keeps its compressed bytes, CRC and sizes, written through the `zip`
   crate's raw-copy API. Determinism holds: the timestamp and permissions
   are still set by jrs, and the bytes are those of a pinned, checksummed
   jar.
2. **Compress in parallel.** Deflate the entries that must be rewritten on
   `rayon`, then write them in sorted order.
3. **Nested jars**, as Boot does. This changes what `package --fat`
   produces and needs a launcher, so it is a SPEC decision, not an
   optimisation. Not proposed here.

**Expected.** 1 alone should take most of the 684 ms away for a project
whose dependencies dominate the jar. It also helps `package` on every other
project, where jrs already wins.

**Measured by.** `package` on all five projects; the `reproducible` CI job
must still pass.

**Built**: 1 and 2 (`package.rs`, `write_jar`). A dependency's entry that
is deflated, not relocated and not renamed is copied raw, with jrs's
timestamp and permissions. Every other entry is read in plan order, deflated
on `rayon` into a one-entry zip, and copied raw from that in sorted order,
which is the thin jar's path too. A stored entry is deflated as before.

## 6. A warmer `javac` without a daemon ✅

**Now.** Every `javac` starts cold: on `gen-2000`, `clean` spends 1331 ms
in it against the daemon's 1.28 s total, and on `gen-ws` each member pays
the start and the JIT warm-up again.

**What cannot be done here.** The daemon's real advantage is a compiler
JIT-warmed by earlier builds. Keeping one between commands is the compiler
daemon SPEC §1.2 rules out. This section stays within that rule; §9 is what
it would take to cross it.

**What can.**

1. **One compiler per command.** The `--watch` worker
   (`compile/worker.rs`) already compiles in a JVM jrs holds by a pipe and
   that cannot outlive the command. Use it outside `--watch` whenever a
   command compiles more than once: workspace members, and §2's rounds.
   The JIT warmed by the first member then serves the rest. Any failure
   falls back to a forked `javac`, as it does today.
2. **An AOT cache for `javac` itself** on JDK 25+ (JEP 483, JEP 515): an
   archive of `jdk.compiler`'s loaded and linked classes and, with JEP 515,
   its method profiles, written once per JDK into jrs's cache and passed
   with `-XX:AOTCache`. It is keyed by the JDK's version and path, holds no
   project input, and is disposable. It must not change `javac`'s output,
   which the `reproducible` job checks.

**Expected.** 1 cuts `gen-ws` `clean` towards the cost of one warm JVM
compiling six members, though the chain of five members still runs in
order. 2 narrows `gen-2000` `clean`. Neither is likely to beat a daemon
that has been warm for a whole working day.

**Measured by.** `clean` on `gen-2000` and `gen-ws`; `cargo bench --bench
incremental` for `javac`'s share.

**Built.** 1: `Worker` is a pool of JVMs per compiler, and a workspace
command that builds more than one member holds a `Worker::javac_only` for
its members' sessions; members built at once no longer wait on one JVM, so
`--watch` builds them at once too. `kotlinc` keeps its worker for `--watch`
alone. 2: `Share::for_javac`, from JDK 25, records on the first forked run
and assembles `cds/javac-jdk<n>-<key>.aot` after it, keyed by the JDK and
the JVM flags (the quick flags of a small file-by-file run make another
cache). The assembling JVM names `javac`'s main module
(`--add-modules ALL-DEFAULT -m jdk.compiler/…`), without which the cache
maps no module classes. On a 60-file compile on JDK 25 it took a forked
`javac` from 0.28 s to 0.17 s.

## 7. The test JVM on large suites ✅

**Now.** `gen-2000`'s `test` is 901 ms, 713 ms of it the test JVM, against
831 ms for the daemon, which also starts a test JVM but has its test
framework classes warm.

**Proposal.** None new. `test.share-classes` and `"aot"`
([TEST_JVM_BENCHMARK.md](../TEST_JVM_BENCHMARK.md)) are the levers already
built, held off by that document's findings 1–3. Fixing finding 3 (forked
runs never write an archive) is what would make them reach a suite of this
size.

**Measured by.** `test` and `test-edit` on `gen-2000`.

**Built**: finding 3's fix. When the tests fork and there is no archive
yet, the first fork dumps it while the others map nothing, and it is put in
place once that fork has run; a fork that never got to the launcher runs
the forks again without it. `share-classes` itself stays off by default,
for findings 1 and 2.

## 8. What stays out of scope

- **A daemon on by default**, or one that holds anything but compilers
  (§9.3): even if §9 is accepted, the default stays a daemonless jrs.
- **Build caches** in the comparison: BENCHMARKS.md §5 times without them,
  and jrs's build cache (SPEC §7.8) against Gradle's is BENCHMARKS.md §10's
  open question 3.
- **Nested Spring Boot jars** (§5, option 3): an output format change, not
  a speed one.

## 9. The last resort: an opt-in compiler daemon

If §2–§6 land and a warm Gradle daemon still wins the compile-bound cells,
the remaining gap is the one thing only a long-lived process gives: a
`javac` (and `kotlinc`) whose JIT has been warmed by earlier builds. This
section is what a jrs daemon would be if that turns out to be the only way
to win those cells. It **reverses a SPEC §1.2 non-goal** and is not
proposed for code until §9.1 holds and the SPEC edit of §9.6 is agreed.

### 9.1 The gate

All of these, in a full run (seven measured runs, two warm-ups):

1. §2, §3 and §6 are built, and the daemon still beats jrs by more than 20%
   on `clean` or `abi` of `gen-2000` or `gen-ws`.
2. The ceiling is real. The `--watch` worker already is a warm `javac` held
   by jrs, so it can measure the gain before anything is built: in
   `jrs build --watch`, the compile after a dozen edits approximates what a
   daemon would give. If that is not at least 25% faster than a forked
   `javac` on those cells, a daemon cannot pay for itself.
3. The cost is stated: the daemon's resident memory at idle, after a
   `gen-2000` build, as the `ru_maxrss` column BENCHMARKS.md §10 question 4
   proposes, beside Gradle's.

### 9.2 What it is

- **Opt-in, per user.** Off unless the user's config (not `jrs.toml`) says
  `[daemon] enabled = true`, or `JRS_DAEMON=1`. Never on when `CI` is set,
  so CI keeps the daemonless build whatever a developer's config says. A
  project cannot turn it on for its contributors.
- **Only compilers.** It holds the existing worker programs (`javac` and
  `KotlincWorker.java`, `compile/worker.rs`) and speaks their protocol,
  over a Unix domain socket (a named pipe on Windows) instead of a pipe.
  jrs itself stays one native process per command: resolution, the
  fingerprints, the output layer and the phase lines do not move.
- **One per key.** A daemon is keyed by the JDK's path and version, the jrs
  version, and the compiler's coordinate (for `kotlinc`). A command whose
  key matches no running daemon starts one; a different JDK gets its own.
- **Bounded life.** It exits after an idle timeout (default 30 minutes,
  `[daemon] idle-timeout`), restarts after 50 compilations as the worker
  does today, and `jrs daemon status` / `jrs daemon stop` list and stop
  them. `jrs clean` does not touch it, since it holds nothing of a
  project's.
- **Private.** The socket lives in a per-user directory under jrs's cache,
  mode `0700`, and the daemon checks the peer's user id. It accepts
  compile requests only, from jrs, never a command line.

### 9.3 What it does not hold

- **Test JVMs.** A test run gets a fresh JVM every time. Reusing one would
  leak static state between runs and break the isolation `jrs test`
  promises. §7 is the lever for the test JVM.
- **Tasks, resolution or anything of jrs's own.** No user code runs in it
  (SPEC §1.2's other rule), and nothing jrs computes is cached in it:
  every fingerprint and lockfile decision is still made by the command.
- **Annotation processors across builds** without a fresh class loader per
  request, so a processor's static state cannot carry from one build to the
  next.

### 9.4 Correctness

- **The same classes.** A request carries the whole invocation, as the
  argfile does today, and the compiler gets a fresh file manager per
  request. `javac`'s output must not depend on its JIT state; the
  `reproducible` CI job builds once with and once without the daemon and
  compares the bytes.
- **Falls back, never fails.** A daemon that is missing, busy past a
  timeout, on the wrong key, or answering garbage is skipped: the step runs
  as a forked `javac`, as the worker's failures do today. No daemon
  failure fails a build.
- **Concurrent commands.** Two jrs commands sharing a daemon get separate
  requests; the daemon compiles them on separate threads with separate
  file managers, or serves one and makes the other fall back.
- **Output.** The compiler's diagnostics still reach the terminal verbatim
  through jrs's output layer, never from the daemon directly.

### 9.5 In the benchmark

A separate column, `jrs (daemon)`, as Gradle has its own. The headline
numbers, the website and the cold, CI-shaped comparisons stay daemonless,
since that is what jrs is by default. The report says how warm the daemon
was, as it does for Gradle's.

### 9.6 SPEC.md edits

- **§1.2.** The compiler-daemon non-goal becomes: "A compiler daemon by
  default. An opt-in, per-user compiler daemon (§7.5) may hold the `javac`
  and `kotlinc` workers between commands; it never runs on CI, holds no
  test JVM and no user code, and any failure of it falls back to a forked
  compiler."
- **§7.5.** The worker section gains the daemon's key, socket, lifetime
  and fallback, and the sentence "This is not a compiler daemon" is
  rewritten to say when it is one.
- **CLAUDE.md**, "The `--watch` workers never outlive the command": becomes
  "never outlive the command unless the user opted into the daemon", with
  the same fallback rule.
- **Commands:** `jrs daemon status` and `jrs daemon stop`; `daemon`
  becomes a reserved task name.

### 9.7 Expected

Unmeasured until §9.1's second check, which is the number to read first.
If a warm `javac` compiles these sources as fast as Gradle's does, the
daemon's own figures are the target: `gen-2000` `clean` level with its
1.28 s, and `gen-ws` `clean` near its 0.8 s, since the compile itself is
then the same work in the same kind of process. With §2 and §3 in place, `abi` would then lead on
both. The start-up cells, where jrs already wins, are unaffected.
