# jrs — Benchmarks against Maven and Gradle

The plan for [ROADMAP §2](../ROADMAP.md#2-benchmarks-against-maven-and-gradle):
jrs, Maven and Gradle building, testing, running and packaging the same
projects, checked to be the same build before anything is timed, with the
results written up in a report anyone can regenerate.

Status: **the harness is implemented**; the first published report, and
the links of §9 to it, are not. The Kotlin projects of §2.1 are planned,
not implemented. It needs no SPEC.md change and adds no
crate: the harness is `benches/compare.rs`, a `cargo bench` target beside
the others, and drives Maven and Gradle as subprocesses. Where the
implementation settles something this plan left open, or departs from it,
§11 says so.

---

## 1. Why, and what it is not

The three benchmarks jrs has measure it against itself:
`benches/resolution.rs` against the network floor (SPEC §12, M5),
`benches/incremental.rs` against `javac`'s share of a rebuild (SPEC §7.2),
`benches/test_jvm.rs` against the test JVM's other modes (SPEC §10.2). None
says how jrs compares to the tools a user would otherwise run, which is the
claim the Why page and the README make ("fast start, no daemon") and leave
unbacked: `website/why/index.html` says the timings are "on the roadmap".

This benchmark is the evidence. It is also the judge of
[GRADLE_GAPS.md](GRADLE_GAPS.md): each gap there gets code only when a
scenario here shows it, and FASTER_BUILDS.md §9.1's "measured by" column
names scenarios defined in §4 below.

It is not a marketing number. The report gives every scenario, every tool's
median and spread, and a section of the cases where jrs is slower (§6).

## 2. The projects

Two sets, kept apart in the report because they prove different things.

**The parity set** — one build described three times. Each project has a
`jrs.toml`, a `pom.xml` and a `build.gradle.kts` in
`benchmarks/projects/<name>/`, written by hand to the same build: the same
sources, the same dependencies at the same versions, the same `--release`,
JUnit 5 for the tests, a fat jar as the package (`maven-shade-plugin`,
Gradle's `shadow`, `jrs package --fat`). Its sources are shared with
`examples/` by a relative path where the example exists, or generated:

| Project | Shape | What it shows |
| --- | --- | --- |
| `wordstats` | `examples/wordstats`, a few classes, JUnit | start-up cost alone |
| `bookmarks` | `examples/bookmarks`, Spring Boot | a framework's graph |
| `gen-300` | generated: 300 classes in 20 packages, 30 dependencies | throughput |
| `gen-2000` | generated: 2000 classes, 60 dependencies | where the JVM work dominates |
| `gen-ws` | generated: a 6-member workspace (Maven reactor, Gradle `include`) | multi-module, and §3.1 of GRADLE_GAPS |
| `orders` | `examples/orders`, Kotlin with a Java class, coroutines | `kotlinc`'s start-up cost |
| `gen-kt` | generated: `gen-300`'s shape in Kotlin | Kotlin throughput, and §3.1 of GRADLE_GAPS |

The generator is part of the harness (§3), seeded and deterministic: each
class has a body, a few calls into its package's neighbours and one into a
dependency, and a test class per ten classes. It writes all three build files
from one description, so they cannot drift.

### 2.1 Kotlin

The first five projects are Java only, so the comparison says nothing about
the cost GRADLE_GAPS §3.1 asks about: a cold `kotlinc` per build in jrs
against the Kotlin daemon Gradle keeps warm. `orders` and `gen-kt` add it to
the parity set.

- **`orders`** is the example as it is: Kotlin main and test sources, one
  Java class compiled after them in the same unit, `kotlinx-coroutines-core`
  (a multiplatform library, reached through its `-jvm` artifact) and
  `kotlin-test-junit5`. Its Maven build uses `kotlin-maven-plugin` with
  the `compile` goal bound before `maven-compiler-plugin`'s; its Gradle
  build uses `kotlin("jvm")`. Both use `[kotlin] version` and its
  `kotlinc-args`, and `jvmTarget` at `[java] source`.
- **`gen-kt`** is `gen-300` written as Kotlin by the same generator: 300
  classes in 20 packages, the same 30 dependencies, a Kotlin test class per
  ten classes on `kotlin-test-junit5`. It has no Java source, so its
  `incremental` and `abi` cells time `kotlinc`, not `javac`.
- **The stdlib** is an explicit dependency in the Maven build only. The
  Kotlin Gradle plugin adds it, and jrs implies it (the `[kotlin]` runtime
  library). §5.1's classpath check holds all three to one `kotlin-stdlib`
  version.
- **The corpus set's Kotlin projects** (`petclinic-kotlin`, `ktor-serialization`,
  `ktor-h2`) join the benchmark like any other, once their corpus row passes
  every step (§11). They are what §3.1 of GRADLE_GAPS names. A run limited to
  the parity set, as both passes in BENCHMARKS_SUMMARY.md were, leaves them
  out. The next full run should not.

`orders` is a command-line project. Its `run` cell is timed to a fixed line
of the receipt `Main.kt` prints (a `Probe::Stdout` in `benches/compare.rs`,
as `wordstats` has), and §5.1 checks that every tool prints it.

**The corpus set** — real projects, each against its own build. The projects
of CORPUS.md whose `jrs migrate` output passes every step there, each
benchmarked against the build it ships (`pom.xml`, `build.gradle`, or both,
as `spring-petclinic` has). jrs builds the migrated manifest. These are the
numbers a user would see when trying jrs on their project; the parity set is
the controlled comparison.

## 3. The harness

`benches/compare.rs`, a `[[bench]]` with `harness = false` in `Cargo.toml`
like the other three, built in release by `cargo bench`:

```text
cargo bench --bench compare
JRS_BENCH_RECORD=1 cargo bench --bench compare        # fill the mirror from Maven Central
JRS_BENCH_TOOLS=jrs,gradle JRS_BENCH_PROJECTS=gen-300 \
  JRS_BENCH_SCENARIOS=noop,incremental JRS_BENCH_RUNS=11 cargo bench --bench compare
```

| Variable | Default | Meaning |
| --- | --- | --- |
| `JRS_BENCH_TOOLS` | `jrs,maven,gradle` | tools to run; `gradle` covers the Gradle columns (§5) |
| `JRS_BENCH_PROJECTS` | all | a comma list of names from §2 |
| `JRS_BENCH_SCENARIOS` | all | a comma list of the ids in §4 |
| `JRS_BENCH_RUNS` | `7` | measured runs per cell |
| `JRS_BENCH_WARMUP` | `2` | untimed runs per cell before them |
| `JRS_BENCH_MIRROR` | `~/.cache/jrs-bench/mirror` | the mirror directory (§3.1) |
| `JRS_BENCH_LATENCY_MS` | `0` | fixed latency per mirror response, as in `resolution.rs` |
| `JRS_BENCH_REPORT` | `benchmarks/REPORT.md` | where the report is written |

`mvn` and `gradle` are found on `PATH`, or as the project's `mvnw` and
`gradlew` for a corpus project that ships them. A tool that is not there is
skipped loudly — a `SKIPPED maven: mvn not on PATH` line, as `require_jdk!`
does — and its column says so in the report. The corpus projects are cloned
at their pinned commits by CORPUS.md's harness; this one reads the list and
the clone directory from it rather than keeping a second list.

Every run happens in a scratch copy of the project (as `test_jvm.rs`'s
`copy_project` does, without `target/`, `build/`, `.gradle/` and `.git/`),
with its own caches: `JRS_CACHE_DIR`, `-Dmaven.repo.local`, and
`GRADLE_USER_HOME`. `JRS_CONFIG` points at a config file the harness writes
(§3.1), so the user's own `~/.config/jrs/config.toml` never applies.

### 3.1 The local mirror

Maven Central's latency swamps the signal and varies from run to run, so no
measured run touches the network. The harness serves `JRS_BENCH_MIRROR` from
a local HTTP server on `127.0.0.1`, the std-only server of
`benches/resolution.rs` (`Server`, `serve`) grown a second root:

- `/central/` — Maven Central's layout;
- `/plugins/` — the Gradle Plugin Portal's (`plugins.gradle.org/m2`), which
  Gradle's `plugins { }` block resolves from.

**Recording.** With `JRS_BENCH_RECORD=1` a miss is fetched from the real
host with `ureq` (already a jrs dependency, so no crate is added), written
into the mirror and served. The harness then runs every tool once over every
project with a cold cache, so the mirror ends up holding exactly the union of
what the three tools ask for — POMs, jars, `.module` files, checksums,
`maven-metadata.xml`, Maven's own plugins.

**Measuring.** Without it, a miss is a 404 and is counted. A cell with a miss
is marked `incomplete` in the report rather than timed against a tool that
failed fast: the mirror has to be re-recorded.

Each tool is pointed at the mirror the way its users would point it at a
corporate one:

- **jrs**: the config file of `JRS_CONFIG` holds `[mirrors] "*" =
  "http://127.0.0.1:<port>/central/"` (DOCS.md, "User configuration").
- **Maven**: `-s <scratch>/settings.xml` with one `<mirror>` whose
  `<mirrorOf>*</mirrorOf>` is the same URL, and `-B` so it never prompts.
- **Gradle**: `--init-script <scratch>/mirror.gradle.kts`, which in
  `beforeSettings` replaces `pluginManagement.repositories` with `/plugins/`
  and `/central/`, and `dependencyResolutionManagement.repositories` and
  every project's `repositories` with `/central/`
  (`allowInsecureProtocol = true`), and sets
  `org.gradle.java.installations.auto-download=false`.

A Gradle distribution is not a Maven artifact. The harness downloads each
Gradle version it needs once, outside any timing, and seeds
`GRADLE_USER_HOME/wrapper/dists` with it before every run, cold or warm.
Maven's own distribution is the installed one, or `mvnw`'s, seeded likewise
in `~/.m2/wrapper`.

### 3.2 One JDK

All three tools run on one JDK: `JAVA_HOME` set for every subprocess, and
`java -version` recorded in the report. A corpus project whose Gradle
toolchain asks for another version is run on that version, installed, for
all three; one that cannot be is left out with the reason. CI (§7) runs the
matrix on 21 and 25.

## 4. Scenarios

Each scenario is a state the harness puts the scratch copy in, untimed, then
one timed command per tool. The time is the wall time from spawn to exit,
with stdout and stderr to a file — never a terminal, whose own rendering
would be measured. Output modes are each tool's plain one: jrs
`--progress never`, Maven `-B`, Gradle `--console=plain`.

| Id | Preparation (untimed) | jrs | Maven | Gradle |
| --- | --- | --- | --- | --- |
| `cold` | empty caches, clean tree | `build` | `compile` | `classes` |
| `clean` | caches warm from a previous run, clean tree | `build` | `compile` | `classes` |
| `noop` | the same command just succeeded | `build` | `compile` | `classes` |
| `incremental` | as `noop`, then one method body changed in a leaf class | `build` | `compile` | `classes` |
| `abi` | as `noop`, then a public method added to a class many others use | `build` | `compile` | `classes` |
| `test` | main and test classes compiled, no test results | `test --all` | `test` | `test --rerun` |
| `test-edit` | as `test` after a run, then one method body changed | `test` | `test` | `test` |
| `run` | main classes compiled | `run` | `exec:java` or `spring-boot:run` | `run` or `bootRun` |
| `package` | main and test classes compiled | `package --fat` | `package -DskipTests` | `shadowJar` or `bootJar` |

Notes on the rows that need them:

- **`cold`** includes resolution and every download, from the mirror. It is
  also where Maven downloads its own plugins and Gradle its plugins; that is
  part of each tool's cold cost, not taken out.
- **`incremental` and `abi`** pick their file by the same rule in every
  project — the generator marks them; for a corpus project the harness takes
  the source with the most and the fewest dependents by a grep for its simple
  name — and restore it before the next run, so every run makes the same
  edit. They are where `compile/incremental.rs` and Gradle's incremental Java
  compilation meet. In a Kotlin project the edit is always in a `.kt` source,
  so the cell times the Kotlin compiler on every tool. There jrs compiles the
  unit whole (SPEC §7.2), Gradle runs Kotlin's incremental compilation in
  its daemon, and Maven uses `kotlin-maven-plugin` as configured.
- **`test`** runs the whole suite on every tool. jrs's `--all` and Gradle's
  `--rerun` turn off the selection and up-to-date checks that would
  otherwise run nothing.
- **`test-edit`** is the same command without them: jrs runs the tests the
  change reaches (`compile/impact.rs`), Gradle and Maven the module's whole
  suite. It is where jrs's selection pays, and the report labels it as
  such, next to `test`, so the gain is not mistaken for a faster test JVM.
- **`run`** is timed to the first line of expected output for a command-line
  project, and to the first successful HTTP answer on the port CORPUS.md
  records for a service, after which the process tree is killed. Its
  preparation leaves the classes compiled, so it measures start-up, not
  compilation.
- **`package`** builds the jar the project ships. For the parity set that is
  the fat jar of §2; for a corpus project, the deployable CORPUS.md's package
  step checks.

`jrs build` compiles the main sources only, so `cold` to `abi` compare it
with Maven's `compile` and Gradle's `classes`, not with `package`. The
`--timings` flag is passed on every jrs run: it costs one file write, and
`target/.jrs/timings.txt` (`timings.rs`, `phase\tms` rows and a `total`) is
read back after each run to fill §6's phase table.

## 5. Fairness

The rules, each one written into the report's header so a reader can check
them:

- **Default configuration.** Each tool runs the build as written: no tuning
  flag jrs has no counterpart for. A project's own `gradle.properties`
  (`org.gradle.parallel`, `org.gradle.jvmargs`) and `.mvn/jvm.config` are
  kept, since its users get them.
- **Members' tests at once in `gen-ws`.** Its generated `jrs.toml` sets
  `[workspace] parallel-tests = true`, which jrs leaves off by default since
  suites that share a port, a database or a fixed file would race. The
  generated suites share nothing, and the Gradle columns that lead this
  cell — the configuration cache, which runs tasks of different projects at
  once, and `--parallel` — already run the members' `test` tasks at once,
  so leaving it off would time jrs's suites one after another against
  Gradle's run together
  ([BEATING_THE_DAEMON_2.md §4](todo/BEATING_THE_DAEMON_2.md#4-parallel-tests-in-the-benchmarks-workspace-)).
- **The test JVM's class-data cache in the generated projects.** Their
  generated `jrs.toml`s set `[test] share-classes = true`, which jrs leaves
  off by default until the corpus has been run under it. A warm daemon's
  test task starts from a worker whose JDK and JUnit classes are loaded
  already; `share-classes` is jrs's counterpart, the same classes mapped
  from an AOT cache the first `jrs test` writes, and leaving it off would
  time a test JVM that loads them all again against one that does not
  ([BEATING_THE_DAEMON_3.md §6](todo/BEATING_THE_DAEMON_3.md#6-the-test-jvm-on-large-suites-)).
  The hand-written projects (`wordstats`, `bookmarks`) keep their own
  manifests.
- **No build cache on either side.** jrs runs with `--no-build-cache`; Gradle
  with `--no-build-cache` and `-Dorg.gradle.caching=false`, overriding a
  project that turns it on; Maven with `-Dmaven.build.cache.enabled=false`
  where the project carries the build-cache extension. A cache hit compares
  cache lookups, not builds. A separate report section may compare the
  caches later (open question 3).
- **Gradle three ways.** `gradle (daemon)`: a daemon started by the warm-up
  runs and reused by the measured ones, which is what a developer at a
  desk sees. `gradle (daemon, CC)`: the same with `--configuration-cache`,
  Gradle's own answer to configuration cost on `noop`. `gradle (no daemon)`:
  `--no-daemon`, what a CI job or a first command sees. On a multi-module
  project a fourth, `gradle (daemon, --parallel)`, is what a team that turns
  on `org.gradle.parallel` sees, and what jrs's members built at once answer
  ([GRADLE_GAPS.md §2.1](GRADLE_GAPS.md#21-what-must-be-measured-first));
  the report's "Workspace members at once" section sets it beside the
  critical path through jrs's members. `gradle --stop` runs between columns
  and between projects, so no daemon outlives its column.
- **The Kotlin daemon as Gradle ships it.** By default Gradle compiles
  Kotlin in a Kotlin daemon (`KotlinCompileDaemon`), a process of its own
  that `gradle --stop` does not end and that outlives `--no-daemon`. The
  daemon columns keep it, since that is what a developer gets. The harness
  stops every Kotlin daemon (found by its main class) between columns and
  projects, and before each run of `gradle (no daemon)` and of every `cold`
  cell. Otherwise a warm compiler left over from another column would be
  timed as a cold one. jrs runs `kotlinc` as it always does, one forked
  compiler per build, since its `--watch` worker is not a build mode.
  Maven runs `kotlin-maven-plugin` in its own JVM, as shipped.
- **Maven as shipped.** `mvn` with no `-T`, unless the project's
  `.mvn/maven.config` sets one. `mvnd` is not a column (open question 1).
- **Interleaved runs.** The measured runs go round-robin over the tools —
  jrs, Maven, Gradle, jrs, … — rather than one tool's seven in a row, so
  thermal throttling and background load spread over every column.
- **Same outcome or no time.** A cell is timed only if §5.1 passed for the
  project and the timed command exited 0. A failing run is reported as
  failing, not as fast.

### 5.1 Equivalence before timing

Before any timed run of a project, the harness checks that the tools build
the same thing, and refuses to time a project that fails the check:

- **The runtime classpath**, as a set of `group:artifact:version`. jrs:
  `jrs classpath --runtime`. Maven: `dependency:build-classpath
  -Dmdep.includeScope=runtime -Dmdep.outputFile=<file>`. Gradle: a
  `jrsBenchClasspath` task the init script adds to every project, writing
  `runtimeClasspath`'s resolved module ids. Paths are mapped back to
  coordinates by each cache's layout. The two sets must be equal; for the
  parity set exactly; for the corpus set up to the version differences
  CORPUS.md §4.1 records as expected for a Gradle project (highest-wins
  against nearest-wins), which the report lists.
- **The tests**: the totals of tests, failures, errors and skipped summed
  from the JUnit XML in `target/test-reports` (jrs),
  `target/surefire-reports` (Maven) and `build/test-results/test` (Gradle)
  after a full `test` run. They must be equal and have no failures.
- **The run**: the same output for a command-line project; an answer on the
  port for a service.
- **The package**: `java -jar` on what each tool built starts and passes
  the same run check.

## 6. The report

The harness writes `benchmarks/REPORT.md` and, beside it,
`benchmarks/raw/<date>-<host>.tsv` with every sample (`project`,
`scenario`, `tool`, `run`, `ms`, `exit`, `mirror misses`), so the tables can
be recomputed or charted without rerunning. The report is generated, never
edited; a published one is committed by hand from the reference machine
(§7).

**Header.** Date; jrs version and commit; the machine (CPU model, cores,
memory), the OS and its version; `java -version`; Maven's and every Gradle
version used; runs and warm-ups; mirror latency; the fairness rules of §5.

**Statistics.** Per cell, the median of the measured runs, and the spread as
min–max. A cell whose max exceeds 1.5× its median is marked `noisy`. The
ratio column is each tool's median over jrs's, so `2.4×` means jrs was 2.4
times faster and `0.8×` that it was slower.

**One table per scenario**, projects as rows:

```text
### noop — the same `build` again, nothing changed

| Project | jrs | Maven | Gradle (daemon) | Gradle (daemon, CC) | Gradle (no daemon) |
| --- | --- | --- | --- | --- | --- |
| gen-300 | 41 ms (38–45) | 2.31 s (2.25–2.40) 56× | 0.41 s (0.39–0.47) 10× | 0.19 s (0.18–0.22) 4.6× | 3.9 s (3.8–4.1) 95× |
```

**Where jrs is slower.** Every cell where some tool's median beats jrs's by
more than the spread, gathered in one section with the project, the scenario
and the jrs phase that took the time.

**jrs's phases.** For each project and scenario, the median of each
`--timings` row, so a slow jrs cell is explained by its phases — resolution,
`compile main: javac`, the test JVM — rather than by one total.

**Kotlin.** For each Kotlin project and each of `clean`, `incremental` and
`abi`, a table with jrs's `compile main: kotlinc` median, its share of jrs's
total, and Gradle's daemon column beside it. This is the number
GRADLE_GAPS §3.1 decides on. `benches/kotlinc.rs` (the compiler's floor and
a warm second compile) splits it further; the report links that bench's
command and does not run it.

**The test JVM.** TEST_JVM_BENCHMARK.md §4 sends its results to "a section
of the ROADMAP §2 benchmark report"; that section is a copy of its latest
§7 table, kept here so the two reports do not need reading together.

## 7. Where it runs

Not in `cargo test`, and not in the push CI of `rust.yml`: it needs Maven,
Gradle, a JDK and minutes per project.

**Locally**, from the command in §3. The published report comes from one
reference machine, named in its header, so successive reports compare.

**In CI**, `.github/workflows/benchmarks.yml`, `workflow_dispatch` only, on
`ubuntu-latest`: `actions/setup-java` with Temurin 21 and 25, a pinned
Maven and Gradle (by version, in the workflow), the mirror restored with
`actions/cache` keyed on the project list's hash and recorded on a miss
(the only step that reaches the network), then `cargo bench --bench
compare`. It uploads `REPORT.md` and the raw TSV as an artifact and commits
nothing. A shared runner is noisy, so the CI report is for spotting a
regression between two jrs commits, not for the Why page.

## 8. Implementation

- `benches/compare.rs` (new): the harness, the generator of §2, the
  equivalence checks of §5.1, the report of §6. The mirror server is lifted
  from `benches/resolution.rs` into `benches/support/mirror.rs` and shared
  by both.
- `benchmarks/projects/` (new): the parity set's hand-written build files;
  `benchmarks/REPORT.md` and `benchmarks/raw/` once a report is published.
- For Kotlin (§2.1): `benchmarks/projects/orders/` with `pom.xml`,
  `build.gradle.kts` and `settings.gradle.kts`. In `benches/compare.rs`, the
  generator gains a language, so `gen-kt` writes `.kt` sources, a `[kotlin]`
  table, `kotlin-maven-plugin` and `kotlin("jvm")`; the Kotlin daemon stop
  of §5; and the report section of §6. The Kotlin Gradle plugin and the
  compiler's artifacts reach the mirror by recording, like everything else,
  so the recorded mirror and CI's `actions/cache` key change once.
- `Cargo.toml`: one `[[bench]]` entry. No dependency.
- No change to `src/`. If the harness needs something jrs does not print
  (a coordinate list for `jrs classpath`, say), that is a finding with its
  own change, not a benchmark-only flag.

## 9. Documentation

- **ROADMAP.md** §2 points at the report and is marked done once the first
  one is published.
- **README.md** "Why jrs": the "one native binary, no daemon" bullet links
  to the report.
- **website/why/index.html**: the note under the comparison ("Timings
  against Maven and Gradle … are on the roadmap") becomes a link to the
  report with its headline table, the slower cases included, and
  `website/index.html` may quote one row.
- **DOCS.md**, Development: how to run the harness and record the mirror.
- **CLAUDE.md**, Commands: `cargo bench --bench compare` beside the other
  three benches.
- **FASTER_BUILDS.md §9.1** and **TEST_JVM_BENCHMARK.md §4**: their
  "ROADMAP §2" references point at this document's scenario ids.

## 10. Open questions

1. **`mvnd`.** The Maven Daemon is Maven's answer to start-up cost, as the
   Gradle daemon is Gradle's. A fourth column is cheap; is it fair to leave
   it out when Gradle's daemon is in?
2. **The reference machine.** macOS on Apple silicon is where the maintainer
   works; Linux x86_64 is where most CI builds run. One of each, or Linux
   only for the published report?
3. **Build caches compared.** jrs's build cache (SPEC §7.8) against Gradle's
   local cache on "clean build after a branch switch" is a fair second
   report, kept separate from §5's no-cache rule. In this one or later?
4. **Memory.** Peak resident memory per run (`wait4`'s `ru_maxrss`, through
   `libc` on unix) would show what the Gradle daemon holds between commands.
   A column, or a sentence?
5. **Gradle's configuration cache by default.** If a Gradle release in the
   pinned range turns it on by default, `gradle (daemon)` and `gradle
   (daemon, CC)` become one column, and the report says which Gradle did so.
6. **Windows.** Process start-up costs differ most there, and jrs ships a
   Windows binary. The harness is portable; the reference report is not.
7. **A Spring Boot project in Kotlin in the parity set.** `bookmarks`
   written in Kotlin (with `allopen`/`spring` as `[kotlin] plugins`) would
   test compiler plugins under control. `petclinic-kotlin` in the corpus set
   covers the same ground against a real build. Is one enough?
8. **A `kotlinc` worker column.** If GRADLE_GAPS §3.3's worker is built, it
   runs under `--watch` only, which no scenario here times. A
   `jrs build --watch` column that times one edit to its rebuild would be
   the fair answer to the Kotlin daemon, but a new kind of measurement.

## 11. As implemented

- **The init script is Groovy**, `mirror.gradle`, not `mirror.gradle.kts`:
  it is compiled again in every cold Gradle home, and a Kotlin script's
  compile would be the harness's cost counted as Gradle's. It rewrites every
  Maven repository a build declares (`repositories.all`), so a settings or
  build script's `mavenCentral()` or `gradlePluginPortal()` lands on the
  mirror too. `org.gradle.java.installations.auto-download=false` is in the
  home's `gradle.properties`.
- **The classpaths** are read as coordinates, not mapped back from paths:
  `jrs metadata`'s `classpaths.runtime`, Maven's `dependency:list` (the
  corpus harness's pinned plugin), and the init script's task over
  `runtimeClasspath`'s module ids.
- **The generated projects** draw from a list of real jars with no runtime
  dependency of their own and no signature, so the three tools' classpaths
  are one set by construction; each class refers to one of them by a class
  literal, which the fat jar's run then loads. `gen-ws` is six members
  (`core`, `model`, `store`, `service`, `web`, `app`), Gradle's `api` to
  match the transitive compile classpath Maven and jrs give a member.
  Its `run` is not timed: `exec:java` cannot run one member of a reactor
  without installing the others. Its `abi` edit is in `core`, so every
  other member sees it.
- **The edits** outside the generator are a private method added to the
  source with the fewest dependents (`incremental`, `test-edit`) and a public
  one to the source with the most (`abi`): a method body chosen by a grep
  could not be changed safely. The report names each file.
- **The states.** `cold` also deletes `jrs.lock`, which would otherwise spare
  jrs the resolution the others do; each cold run of a daemon column stops
  its daemon first, since its home is emptied. `clean` removes `target/` and
  `build/` but keeps `.gradle/`, as `mvn clean`, `gradle clean` and `jrs
  clean` leave each tool's state outside them. `test` and `package` start
  from a full `test` run, which is what compiles the test classes on every
  tool.
- **The corpus set** is the projects whose row in the latest
  `target/corpus/<run>/REPORT.md` has no failing, unrun, unselected or
  skipped step, each from that run's clone, against the one build its
  `corpus.toml` entry names.
- **A recording run** (`JRS_BENCH_RECORD=1`) runs the checks of §5.1 alone:
  each tool once, from cold caches, which fills the mirror. A 404 from
  upstream is kept as a `.404` marker, so the measuring run answers it the
  same way without counting a miss.
- **Exit status.** The run exits 1 when a project fails its checks or a
  cell fails, so a CI job shows red; an `incomplete` cell does not.
