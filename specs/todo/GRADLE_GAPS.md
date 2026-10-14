# jrs — Closing the gaps where Gradle wins

Four places where a Gradle build does something a jrs build does not, and
what jrs would do instead: workspace members built in parallel, the cost of
starting `kotlinc`, a pinned JDK that is not installed, and a pinned jrs.
This is [ROADMAP §3](../../ROADMAP.md#3-closing-the-gaps-where-gradle-wins).

Status: **proposed.** Nothing here is implemented. Parts A and B are speed,
and each waits for a number from the benchmark of
[BENCHMARKS.md](../BENCHMARKS.md) before it gets code (§1). Parts C and D are
not speed; they cross lines drawn in SPEC §1.2 and listed in
[ROADMAP_ARCHIVE §5](../ROADMAP_ARCHIVE.md#5-needs-a-spec-decision-first),
so each needs its SPEC.md edits (§6) agreed before code.

---

## 1. The gate

Every part says what has to be true before it is built. For A and B that is
a measurement; for C and D it is a decision. The rule from FASTER_BUILDS
holds: a gap is closed when the report says it is felt, not when Gradle has
a feature of the same name.

| Part | Gradle feature | Gate |
| --- | --- | --- |
| A | `--parallel` | §2.1: the critical-path estimate from `--timings` on the benchmark's workspaces |
| B | the Kotlin daemon | §3.1: `kotlinc`'s start-up share on the Ktor corpus projects |
| C | toolchain resolvers (foojay) | the SPEC §7.1 / §8.5 edits in §6 |
| D | `gradlew` | the SPEC §4.3 edit in §6, and the wrapper decision in §5.3 |

---

## 2. A — workspace members in parallel

Today `cli/members.rs` builds members one after another in
`Workspace::order`: `build_upstream` and `build` loop over
`with_upstream(..)`, each member a `Session` of its own, and
[WORKSPACES.md §9](../WORKSPACES.md#9-what-stays-out) keeps parallel members
out "to keep the phase lines in a fixed order without buffering a member's
output", calling it "the first thing to revisit if a large workspace shows
it". §9 also says each member's compile "is already parallel inside
`javac`"; it is not, much: `javac` does its attribution and code generation
on one thread, so a workspace of independent members leaves cores idle that
Gradle's `--parallel` uses.

### 2.1 What must be measured first

No code is needed to know the gain. A sequential `jrs build --timings` over
a workspace gives each member's wall time; the critical path through the
member graph (each member's time plus its slowest upstream member's path) is
what a perfect parallel build would take. The benchmark computes, on its
generated workspace (BENCHMARKS.md's many-module project) and on every
multi-module corpus project:

- `sequential` — the sum, which is what jrs takes now;
- `critical path` — the lower bound with unlimited cores;
- Gradle's clean build with `--parallel` and a warm daemon.

**Build A when** the critical path is at least 25% below the sequential sum
on a workspace of four or more members, **or** Gradle `--parallel` beats
jrs's clean build of it by more than 20%. If neither holds on any project,
WORKSPACES §9 stays as it is and records the numbers.

### 2.2 Scheduling

- **Units are members**, not compile steps. A member is ready when every
  member it depends on (`Workspace::upstream_of`) has built; ready members
  start in `Workspace::order`, so ties go to declaration order as today.
- **Threads, not `rayon` tasks.** A member's build blocks on subprocesses
  (`javac`, a compiler JVM, hooks), so each running member gets a scoped
  thread (`std::thread::scope`); at most `--jobs` members run at once,
  where `--jobs` already means "how much at once" for downloads. One member
  at a time is `--jobs 1`, which is how tests and users get today's
  behaviour back.
- **One session per thread.** `Session` holds `RefCell`s and `OnceCell`s and
  the driver hands it around as `Rc` (`Driver::sessions`), so it is `Send`
  at best, never `Sync`. Each member's session is built and used on its own
  thread; what crosses threads is already shareable: `&Cli`, `Ui` (an
  `Arc`), the resolved graph, the build cache (`Arc<BuildCache>`), and the
  upstream members' class directories, which a session only ever reads by
  path (`MemberContext::upstream`). `Driver::sessions` becomes a map of
  finished members' results, not of sessions.
- **Downloads first.** The aggregate's jars are fetched once before the
  members fan out, so two members never race for one jar. A race would be
  safe — cache writes are temp-file-then-rename (`resolve/cache.rs`) — but
  wasteful.
- **What stays sequential.** `run` (one member), `--watch` (one `javac`
  worker serves one request at a time, so parallel members would queue on
  it; §7, question 2), and any workspace where a member's hook or task
  depends on another member's task (`"<member>:<task>"`, WORKSPACES §6.3):
  that task runs in the other member's session, which under this design
  lives on another thread. Such a workspace builds as today and says why
  under `-v`. Lifting that is a later step, once a workspace needs it.
- **Tests** keep running member by member in the first version: a test JVM
  already forks per core (`test.forks`), and two members' suites at once
  would fight over the same cores and ports. The build part of `jrs test`,
  `package` and `doc` — the upstream builds — runs in parallel.

### 2.3 Output: buffered, released in build order

The transcript has to read the same whatever the timing, which is what
WORKSPACES §6.2 promises and `tests/workspace.rs` asserts. The rule:

> **The earliest unfinished member in build order writes to the terminal;
> every other member writes to a buffer.** When the writing member finishes,
> the next member in order is *released*: its buffer is replayed and it
> writes straight through from then on.

So the plain transcript is line for line the sequential one, and the member
at the head still streams `javac`'s diagnostics as they arrive.

In `ui/`, which is the only place that touches the terminal:

- `Ui::deferred()` returns a `Ui` for one member that shares the parent's
  renderer but holds a gate. While the gate is closed, `permanent` lines —
  `phase`, `status`, `warn`, `error`, `verbose`, `passthrough`,
  `passthrough_line` — go to an in-memory buffer of `(Stream, String)`,
  and its live scopes (`spinner`, `downloads`, `tests`) are inert.
  `release()` replays the buffer through the renderer in one lock and
  opens the gate. Nothing changes for code that takes a `&Ui`.
- The parent `Ui` owns the one live region: a spinner naming what is
  running, `Compiling core, api (2 of 5 modules)`, so the animated mode
  still moves while the head member is silent.
- Phase lines are still emitted by `cli.rs` (here `cli/members.rs` and the
  `Session` it drives), unconditionally; only where they land is deferred.
- A subprocess that would inherit the terminal (`run_task_inherited`,
  `run_inherited_in`) never runs in a deferred member: of the commands A
  touches, only `jrs task <name>` inherits, and it is not parallelised.
  A hook streams (`run_task_streamed`), which a buffer can hold.

**Failure.** When a member fails, no new member starts; the running ones
finish; then every finished member is released in build order, up to and
including the first failure, and the command fails with that member's
error. Members after it in order that also finished are released too, so
nothing that ran is hidden. `jrs test`'s "a failing member does not stop
the others" is unchanged, since tests stay sequential.

**`--timings`** gains one row per member (`member core`) beside the phase
rows, and the report's total is wall time, so the saving shows.

### 2.4 Tests

- A captured-UI test drives two independent fake members whose steps
  finish in reverse order and asserts the transcript equals `--jobs 1`'s.
- `tests/workspace.rs`: a three-member workspace (`a`, `b` independent,
  `c` on both) built with `--jobs 4` and `--jobs 1` gives the same plain
  output and byte-identical jars; a compile error in `b` reports `b` and
  still prints `a`'s lines first.
- `tests/output.rs`: the parent's live row naming two running members.

---

## 3. B — what `kotlinc` costs

A Kotlin unit's first step runs `org.jetbrains.kotlin.cli.jvm.K2JVMCompiler`
(`compile/lang.rs`) in a fresh JVM, from a class-data-sharing archive
(`compile/share.rs`). [JVM_LANGUAGES.md §14.1](../JVM_LANGUAGES.md#14-open-questions)
measured one small mixed unit at 1.8 s in `kotlinc` and decided "no daemon
for now"; FASTER_BUILDS §6.4 left a `kotlinc` worker out until "a
measurement on the corpus saying the second of JVM start-up is felt". Part
B is that measurement, and what follows from it. Ktor is one of the two
kinds of project the corpus is built around, so a second per edit is paid
on most edits of half the target audience.

### 3.1 What must be measured first

`cargo bench --bench kotlinc`, a harness in the shape of
`benches/test_jvm.rs`: `JRS_BENCH_PROJECT=<path>` copies a project, runs
`jrs build --timings --no-build-cache` once to warm everything, then for
`JRS_BENCH_RUNS` edits of one Kotlin source (a body change, then an API
change) records:

| Column | What it is |
| --- | --- |
| `kotlinc` | the `compile main: kotlinc` row of `--timings` |
| `floor` | the same JVM, archive and classpath compiling one empty `.kt` file |
| `warm` | the same compile run twice in one JVM by a throwaway Java driver calling `K2JVMCompiler().exec(..)`, second call timed |
| `javac` | the unit's `javac` row, for scale |

`floor` is what start-up and loading the compiler cost; `kotlinc − warm` is
what a worker would save. The driver is test code in `benches/`, not shipped.

**Build a worker when**, on the Ktor and Spring-Kotlin corpus projects,
`kotlinc − warm` is at least one second **and** at least 40% of `kotlinc`
for a body edit. Below that, record the numbers in JVM_LANGUAGES §14.1 and
stop. The benchmark report's "one-file incremental" scenario on a Kotlin
project is where the same gap shows against Gradle, whose Kotlin daemon is
warm after the first build.

### 3.2 The cheap step first

Before any worker: a `kotlinc` run over a few changed files gets the same
JVM flags a small `javac` run does (SPEC §7.2: `-XX:TieredStopAtLevel=1
-XX:+UseSerialGC` under 100 sources). It is one line in the unit's flags,
and the bench says whether it helps `kotlinc` as it helps `javac`. A Kotlin
unit compiles whole (no file-by-file Kotlin, SPEC §7.2), so the threshold is
the unit's source count, not the changed files'.

### 3.3 A `kotlinc` worker, under `--watch` only

If §3.1 says so: the `javac` worker's shape (`compile/worker.rs`), a second
time.

- `KotlincWorker.java`, embedded like `JavacWorker.java` and compiled on
  first use with the project's `javac` against the compiler's tool jars,
  into `<cache>/worker/<jrs version>-kotlin<version>-jdk<n>/`. It runs with
  the compiler's classpath and reads the same one-line requests.
- Each request names an argfile holding `kotlinc`'s own arguments. Today the
  whole invocation — JVM classpath, main class and compiler flags — is one
  `java @argfile`; the compiler's part moves into an argfile of its own,
  which the forked run passes to `K2JVMCompiler` as `@<file>`, so a forked
  and a worker run read the same bytes.
- The worker calls `new K2JVMCompiler().exec(err, args)` per request and
  answers as `JavacWorker` does: `jrs-worker <id> <exit> <out> <err>` and
  both streams, passed through verbatim.
- `kotlinc` holds more static state than `javac`, so `RESTART_AFTER` is
  per worker kind, and its default for `kotlinc` comes from the bench (heap
  after N compiles). Every rule of SPEC §7.5 stays: a child of the jrs
  process, reached only through its pipe, killed and replaced by a forked
  run on any failure, never outliving the command.
- The Kotlin Build Tools API and incremental Kotlin compilation stay out
  (ROADMAP_ARCHIVE §5): this is the same whole-unit compile, warm.
- Scala and Groovy wait for their own number; the worker is shaped so a
  third kind is a Java file and a main class, not a new mechanism.

---

## 4. C — downloading a pinned JDK

SPEC §7.1 finds a pinned JDK (`java.jdk`, `.java-version`, `.sdkmanrc`) in
the `[jdks]` table, at `JAVA_HOME`/`PATH`, or where installers and version
managers put them (`toolchain::installed_jdks`), and otherwise fails naming
the versions it did find. Gradle with a toolchain resolver downloads it.
ROADMAP_ARCHIVE §5 lists this as crossing two lines: a host that is not a
Maven repository, and tar.gz meaning new crates. The second is not true:
`selfupdate::unpack` already shells out to `tar` for jrs's own `.tar.gz`
("every macOS and Linux machine has a `tar` that reads gzip"), and the
`zip` crate already reads Windows `.zip` archives. The first has precedent:
`jrs audit` talks to OSV and `jrs self update` to GitHub. What is new is that
jrs would install a program it then runs. That is the decision.

No benchmark gates C. The report's "fresh machine" scenario (clone to green
build on a runner with no JDK of the pinned version) shows the time with and
without it, for information.

### 4.1 Behaviour

- **Opt-in in v1**, from the user config (SPEC §8.5), which belongs to the
  machine, not the project:

  ```toml
  [jdk]
  download = true          # download a pinned JDK that is not installed
  vendor = "temurin"       # the only vendor v1 knows
  url = "https://api.adoptium.net"   # an API-compatible mirror
  ```

  and `JRS_JDK_DOWNLOAD=1` for CI. Without it, the §7.1 error gains one
  line: `jrs jdk install 21` downloads it.
- **`jrs jdk install <n>`** downloads JDK `<n>` explicitly, whatever the
  config says; **`jrs jdk list`** prints the JDKs jrs can see, where each
  is, and which one the project would pick. Both are new commands (SPEC
  §5.1).
- **Only a pin triggers it.** An unpinned project with no JDK at all still
  fails as SPEC §7.1 says: a pin is the project saying which version it
  wants; `java.source` is a minimum, not a choice.
- **`--offline`** never downloads; the error says the JDK is not installed
  and `--offline` is set.

### 4.2 Download, verify, unpack

1. Ask the vendor's API for the latest GA build of feature version `<n>`
   for this OS and architecture (Adoptium's
   `/v3/assets/latest/<n>/hotspot?image_type=jdk&os=..&architecture=..`),
   parsed with `json.rs`'s reader. The answer names the archive's URL, its
   name and its SHA-256.
2. Stream the archive to a temp file in `<cache>/jdks/`, hashing as it goes,
   behind a `ui.downloads` live scope with a byte count. `selfupdate`'s
   `Releases::download` reads into memory with a 256 MiB cap; a JDK archive
   is close to that, so the download half is a streaming variant of it,
   sharing `agent()` and the proxy (`build_proxy`).
3. Compare the SHA-256 before anything is unpacked; a mismatch deletes the
   file and fails.
4. Unpack into `<cache>/jdks/.tmp-<random>/`: `tar -xzf` on macOS and
   Linux, as `selfupdate::unpack` does; the `zip` crate on Windows, refusing
   any entry whose `enclosed_name()` is `None`.
5. Find the home: the directory holding `release` within three levels
   (`jdk-21.0.5+11/` on Linux and Windows, `jdk-21.0.5+11/Contents/Home/` on
   macOS), and check it with `Toolchain::probe_home`, which runs `javac
   -version`.
6. Rename the temp directory to `<cache>/jdks/temurin-21.0.5+11-<os>-<arch>/`,
   so an interrupted install leaves nothing a later lookup could pick, the
   same rule as cache jars (`resolve/cache.rs`).

`installed_jdks` gains `<cache>/jdks` as one more root, so once installed,
the JDK is found by §7.1's lookup with no network and no config. `jrs cache
prune` (SPEC §8.6) leaves `jdks/` alone unless asked (`--jdks`), since a JDK
is slow to fetch again.

Phase lines, from `cli.rs`: `Downloading JDK 21.0.5+11 (temurin,
linux-x64)`, then `Installed JDK 21.0.5+11 in <path>`.

### 4.3 What it does not do

- No other vendors in v1, no GraalVM (`native-image` keeps SPEC §9.7's
  rule), no early-access builds, no JDK 8.
- No entry in `jrs.lock`. A JDK archive differs per OS and architecture, and
  the lockfile is the same on every machine. Pinning the JDK build is §7,
  question 4.
- No PATH or `JAVA_HOME` changes; the JDK is jrs's, used for the project
  that pins it.

---

## 5. D — pinning jrs itself

What `gradlew` gives a team: every checkout builds with the same Gradle
version, a fresh machine needs nothing installed, and
`distributionSha256Sum` can pin the bytes. jrs has the first half of the
first: `project.jrs-version` (SPEC §4.3) names the *oldest* jrs that may
build the project, and an older one stops with exit `2`. SPEC §4.3 says
"fetching the right jrs, as `gradlew` does, is not part of it". Exact
pinning and fetching are what is missing.

No benchmark gates D.

### 5.1 In CI: `setup-jrs` reads the manifest

`setup-jrs/action.yml` installs the release it is used at (`@v0.10.0`), or
`version:`, or the latest. A new value, `version: manifest`, reads
`project.jrs-version` from `jrs.toml` (or the `manifest:` input's path) and
installs the newest release that satisfies it: `"0.10"` → the newest
`0.10.x`; `"0.10.1"` → `0.10.1` exactly when the major is `0`, since
`0.x` minors may break. The value is found by a small awk over the
`[project]` table, since the key is a plain quoted string there; anything
it cannot read fails the step naming the line. `website/install.sh` gets
the same as `--version-from <jrs.toml>`.

### 5.2 Locally: `jrs self update --to <version>`

`jrs self update` only goes to the latest. `--to <version>` installs that
release, older included, through the same download, `SHA256SUMS` check and
`replace` (`selfupdate.rs`). The SPEC §4.3 error for a too-old jrs names it:
`jrs self update --to 0.10.0`.

### 5.3 A wrapper script (the decision)

The step that is `gradlew`: `jrs wrapper` writes `jrsw` (POSIX `sh`) and
`jrsw.cmd` (Windows), committed with the project, each holding one jrs
version and the SHA-256 of every release target's archive, read from that
release's `SHA256SUMS`. Run, the script looks for
`<cache>/dist/<version>/jrs`, downloads the target's asset if missing,
checks it against the embedded checksum, unpacks it with `tar` (or
PowerShell's `Expand-Archive`) and `exec`s it with its arguments.

That is stronger than `gradlew`'s default, which checks no checksum unless
asked. It also crosses ROADMAP_ARCHIVE §5's row "a wrapper that fetches the
pinned jrs": jrs itself does not fetch and run a program, a script the
project commits does, but the effect is the same. **Proposed: yes**, with
the scripts generated from `website/install.sh`'s target detection so there
is one copy of it, and `jrs wrapper --version <v>` to move the pin.
`project.jrs-version` stays the minimum; the wrapper is the exact pin.

---

## 6. SPEC.md edits

- **§1.2** — none for A and B. For B, the `--watch` sentence widens from
  "one `javac` worker" to "one worker per compiler", with the same lifetime
  rule. For C: no non-goal names it today, but ROADMAP_ARCHIVE §5 does;
  §1.2 gains "a JDK is downloaded only for a pinned version, only when the
  user's config allows it, and only from the vendor's API".
- **§4.3** — "fetching the right jrs is not part of it" becomes a pointer to
  `jrs wrapper` and `setup-jrs`'s `version: manifest`; the error names
  `jrs self update --to`.
- **§5.1** — `jrs jdk install`, `jrs jdk list`, `jrs wrapper`, `jrs self
  update --to`.
- **§5.2 / §5.3** — `--jobs` also bounds members built at once; the
  transcript rule of §2.3.
- **§7.1** — a fourth lookup place, `<cache>/jdks`, and the download step
  after it.
- **§7.5** — the `kotlinc` worker, if B is built.
- **§8.5** — the `[jdk]` table.
- **§13** — no new crate: C uses `ureq`, `sha2`, `zip`, `json.rs` and the
  system `tar`; D is shell.
- **WORKSPACES.md §6.2 and §9** — members in parallel, the transcript rule,
  and what stays sequential.

## 7. Open questions

1. **Parallel tests across members.** Tests stay sequential in A. Running
   two members' suites at once needs a fork budget shared across members and
   says nothing about ports and databases two suites both use. Proposed:
   measure after A, with the `test JVM` rows per member.
2. **`--watch` and parallel members.** One `javac` worker serialises them.
   One worker per running member multiplies JVMs; proposed: sequential under
   `--watch`, where a change usually touches one member anyway.
3. **Default for C.** Opt-in keeps jrs from installing programs nobody asked
   for; on by default is what makes "clone and `jrs run`" (SPEC §1.3) work
   on a machine without the pinned JDK. Proposed: opt-in until the corpus
   has run with it on, then reconsider.
4. **Pinning the JDK build.** `java.jdk = 21` takes the newest 21 found or
   downloaded, so two machines can build with 21.0.4 and 21.0.5. Byte-
   identical jars do not depend on it (classes are compiled `--release`), but
   tests may. A `java.jdk = "21.0.5"` form, and checksums per platform in
   `jrs.lock`, are a second step.
5. **Checksum trust for C.** The SHA-256 comes from the same API as the
   archive, so it catches a broken download, not a compromised host.
   Adoptium also publishes GPG signatures, which need an OpenPGP crate
   (ROADMAP_ARCHIVE §5). Proposed: accept, as `jrs self update` does for
   GitHub.
6. **Wrapper on Windows.** `jrsw.cmd` delegating to PowerShell is the usual
   shape; whether Windows ARM64 runners have everything it needs is to be
   checked against `setup-jrs`'s matrix.

## 8. Order of work

| Step | What | Depends on |
| --- | --- | --- |
| G1 | `benches/kotlinc.rs` and the critical-path figure in the benchmark harness | BENCHMARKS.md's harness |
| G2 | `setup-jrs` `version: manifest`, `install.sh --version-from`, `self update --to` | nothing |
| G3 | `jrs jdk list`, `jrs jdk install`, `<cache>/jdks` lookup | SPEC edits for C |
| G4 | `[jdk] download` in the build path | G3 |
| G5 | `Ui::deferred`, parallel members | G1's numbers |
| G6 | small-run JVM flags for `kotlinc`, then the worker | G1's numbers |
| G7 | `jrs wrapper` | the §5.3 decision |

G2 and G3 need no measurement and can start now; G5 and G6 start only if
G1's numbers clear §2.1 and §3.1.
