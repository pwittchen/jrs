# jrs — The real-project corpus

The plan for [ROADMAP §1](../../ROADMAP.md#1-hardening-against-real-projects):
a fixed list of open-source Spring Boot and Ktor projects, a harness that
takes each one through `jrs migrate`, `build`, `test`, `run` and `package`
and checks the result against the project's own Maven or Gradle build, and
the rules for what happens to every failure it finds.

Status: **harness landed, findings open**. `benches/corpus.rs`,
`docs/corpus/corpus.toml`, `docs/corpus/FINDINGS.md` and `.github/workflows/corpus.yml`
exist, and the first run is in `DOCS.md`'s Compatibility section; §9's
criteria are not met yet. Nothing here changes how jrs builds; the harness
is a new `[[bench]]` and adds no crate. The first pass of
[TEST_JVM_BENCHMARK.md §7](../TEST_JVM_BENCHMARK.md#7-results-first-pass)
already ran jrs on three petclinics and found five things; this document
turns that one-off into something that runs on every release and keeps
its findings.

---

## 1. Problem

Every project jrs is tested on was written for it. `examples/` shows one
feature per project, and the `tests/fixtures/migrate/` fixtures each carry
one construct `jrs migrate` translates. Real builds carry dozens of them at
once, and the ones nobody wrote a fixture for are the ones that fail: the
Kotlin petclinic lost its whole `[kotlin]` table to a `val` inside
`plugins { }`, and its main class went unset because only `.java` sources
were searched (TEST_JVM_BENCHMARK §7.1, finding 5). Tried on the
maintainer's own projects, migration and the build after it did not fully
work either.

A user's first project decides whether they try a second. So the question
this answers is narrow and measurable: **of a pinned set of real projects,
how many go from `jrs migrate` to a running fat jar with no hand edits, and
why do the others not?**

## 2. The corpus

### 2.1 What goes in

Two kinds of project, the two jrs is aimed at, on both build tools:

| Kind | Projects (first set) |
| --- | --- |
| Spring Boot, Java, Maven | `spring-projects/spring-petclinic` (`pom.xml`), `spring-guides/gs-rest-service`, `spring-guides/gs-accessing-data-jpa` |
| Spring Boot, Java, Gradle | `spring-petclinic` again from `build.gradle`, the same guides from their `build.gradle` |
| Spring Boot, Kotlin | `spring-petclinic/spring-petclinic-kotlin` (`build.gradle.kts`) |
| Ktor | two from `ktorio/ktor-samples`: one with `kotlinx.serialization`, one without |
| Mid-sized service | one with a few dozen dependencies, a database driver, Flyway or Liquibase, Testcontainers, Mockito and an annotation processor (MapStruct or Lombok); `spring-petclinic-rest` is the first candidate |
| Multi-module | `spring-petclinic-microservices` (a Maven aggregator), to judge workspaces (WORKSPACES.md §8) |
| Stand-ins | the maintainer's projects that failed, or minimal public reproductions of them if they cannot be published |

A project built by both tools (petclinic, the guides) is two corpus
entries: the migration is different code (`migrate/maven.rs` against
`migrate/gradle.rs`) and so is the reference.

What stays out: libraries published to Central, Android, and builds whose
point is a Gradle plugin jrs has no counterpart for. Those are non-goals
already (ROADMAP_ARCHIVE §5), and a corpus full of them would measure
nothing jrs means to fix.

### 2.2 The list

`docs/corpus/corpus.toml`, read with the `toml` crate jrs
already depends on:

```toml
[[project]]
name = "petclinic-maven"
repo = "https://github.com/spring-projects/spring-petclinic"
commit = "500158f…"               # full 40-character hash
dir = "."                         # where the build file is, inside the clone
build = "maven"                   # or "gradle"; what `jrs migrate --from` gets
kind = "spring-boot"              # spring-boot | ktor | service | workspace
jdk = 25                          # the release it is run on, at least
run = { probe = "http://localhost:8080/", expect = 200 }
package = { fat = true, probe = "http://localhost:8080/", expect = 200 }
needs = ["docker"]                # steps that need it are skipped, not failed, without it
```

- **`commit`** is always a full hash, never a branch or tag: a moving
  project would change the outcome under the harness. Moving a pin is a
  commit of its own, with the outcome before and after in its message.
- **`run`** says how to tell the application started: an HTTP probe (with
  the status it should answer) for a service, or `stdout = "<substring>"`
  for a command-line program. A project without one has `run` skipped.
- **`needs`** names what the host must have; today only `docker`, for
  Testcontainers. A missing need is a `skip`, so a laptop without Docker
  still gets a table: the tests are skipped on both sides (the reference
  runs with `-DskipTests` or without `test`), and `test` shows `skip`.
- **`member`**, in a workspace, names the member `run` and `package`
  start (`jrs -p <member>`).
- **`exclude`** lists test classes left out of the comparison on both
  sides, each with its reason in `FINDINGS.md` (§4.2).
- **`jdk`**: a host JDK older than it skips the whole row.

## 3. The harness

### 3.1 Where it lives

`benches/corpus.rs`, a `[[bench]]` with `harness = false`, as
`benches/test_jvm.rs` is. A bench and not a test because its output is a
table for a person to read, it takes minutes per project, and it needs
`git`, the network, a JDK and, for the reference, Maven or Gradle — none of
which `cargo test` may assume. It does nothing unless asked:

```text
JRS_CORPUS=all cargo bench --bench corpus
JRS_CORPUS=petclinic-maven,ktor-serialization cargo bench --bench corpus
JRS_CORPUS=all JRS_CORPUS_STEPS=migrate,build cargo bench --bench corpus
```

Without `JRS_CORPUS` it prints one `SKIPPED` line, so `cargo bench` stays
what it is. It drives the release `jrs` binary as a user would — never the
library in-process — so what it finds is what a user hits.

### 3.2 One pass

Everything happens under one scratch directory, kept at the end:
`target/corpus/<run>/`, with `cache/` as `JRS_CACHE_DIR` (shared by the
run's projects, never the user's own cache) and one directory per project.
For each project, in order, each step timed and its full output logged:

| Step | What runs | Passes when |
| --- | --- | --- |
| `clone` | `git init`, `git fetch --depth 1 <repo> <commit>`, `git checkout FETCH_HEAD` | the tree is at `commit` |
| `reference` | the project's own build: its wrapper (`./mvnw`, `./gradlew`) if it has one, else `mvn`/`gradle` on `PATH`, running the tests and dumping the runtime classpath (§4) | it exits 0; a project whose own build fails is fixed in the list, not counted |
| `migrate` | `jrs migrate --from <build>` | exit 0 and a `jrs.toml` that `jrs metadata` accepts |
| `build` | `jrs build`, then `jrs metadata` | exit 0, and the classpath check of §4.1 |
| `test` | `jrs test --all` | exit 0, and the count check of §4.2 |
| `run` | `jrs run` in the background, then the probe | the probe answers within 120 s |
| `package` | `jrs package --fat`, then `java -jar` on it, then the probe | as for `run` |

The reference runs first and in a copy of the clone, so its `target/` or
`build/` cannot leak into jrs's. A step after one whose command failed is
not run, and its cell says so (`·`). A step whose command succeeded but
whose comparison with the reference does not hold (§4) fails its cell and
lets the next step run: a classpath difference says nothing of whether the
tests pass. `run` and `package` kill what they started, its
whole process group, on every path out.

Nothing is edited by hand between steps. That is the point: the outcome is
what a user who runs `jrs migrate` gets. A `[corpus.project.patch]` key is
deliberately not offered — a patch would hide exactly the finding the
corpus exists to make.

## 4. Comparing with the reference

### 4.1 The runtime classpath

The reference dumps its runtime classpath as resolved coordinates:

- **Maven**: `dependency:list -DincludeScope=runtime -DoutputFile=…
  -DexcludeTransitive=false`, pinned to one `maven-dependency-plugin`
  version on the command line, so the project's own pin cannot change the
  output format.
- **Gradle**: an init script the harness writes to the scratch directory and
  passes with `--init-script`, adding a task that prints
  `configurations.runtimeClasspath.resolvedConfiguration.resolvedArtifacts`
  as `group:artifact:version[:classifier]`, one per line.

jrs's side is `jrs metadata`'s `classpaths.runtime` (SPEC §5.4): every jar
the application gets, each with its `coordinate`, from the same function
`jrs run` uses. A local jar has a `null` coordinate and is compared by file
name. Both become sets of `group:artifact` → version and are compared:

- **missing** — on the reference's classpath, not jrs's: a finding.
- **extra** — on jrs's, not the reference's: a finding, unless it is an
  implied language runtime (`Manifest::effective_dependencies`) the
  reference also has under another name.
- **version** — on both at different versions. For a Maven project this is
  a finding: both mediate nearest-wins. For a Gradle project it is recorded
  but expected, since Gradle picks the highest version (SPEC §13.6); the
  report lists them, and they feed the open "highest-wins, opt-in" row of
  ROADMAP_ARCHIVE §5 with real numbers.

The `build` cell shows `ok`, or `cp −2 +1 ~5` for two missing, one extra and
five version differences.

### 4.2 The tests

Both sides write JUnit XML: Surefire to `target/surefire-reports`, Gradle
to `build/test-results/test`, jrs to `target/test-reports`. The harness
reads `tests`, `failures`, `errors` and `skipped` from each `<testsuite>`,
keyed by class name, and compares:

- the totals, which must be equal: the same tests run, passed and skipped;
- per class, so a difference names the class: a class only one side ran
  (discovery), a class whose counts differ (a failing test under jrs).

A failure that also fails under the reference is not jrs's; a project with
any is fixed in the list (§2.2) or the test noted as excluded in
`corpus.toml`, never silently.

## 5. The report

At the end the harness prints, and writes to `target/corpus/<run>/REPORT.md`:

```text
jrs corpus — jrs 0.11.0 (abc1234), JDK 25.0.2, macOS 27.0.1, 2026-10-20

project              build   reference  migrate  build      test        run   package  time
petclinic-maven      maven   ok         ok       ok         ok 81/81    ok    ok       3m12s
petclinic-gradle     gradle  ok         ok       cp ~4      ok 81/81    ok    ok       3m40s
petclinic-kotlin     gradle  ok         F5       ·          ·           ·     ·        0m41s
ktor-serialization   gradle  ok         ok       ok         ok 12/12    ok    skip     1m05s
service-rest         maven   ok         ok       ok         F9 140/142  ok    ok       4m50s

end to end: 3 of 5; migrate first try: 4 of 5
```

A cell is `ok`, `skip` (a missing need, or no probe), `·` (an earlier
step's command failed), `-` (a step `JRS_CORPUS_STEPS` left out), or
`F<n>` naming the finding (§6) the failure belongs to; a failure the
findings do not explain yet is `FAIL` and the run exits 1. A failed
`reference` is `FAIL` too, and leaves the row out of the headline numbers:
the list needs fixing, not jrs. A step `known` maps that now passes is `ok`,
and the report's notes say to close its finding. The two headline
numbers at the bottom are the measures ROADMAP §1 tracks: **end to end**
(every step `ok` or `skip`) and **migrate first try** (`migrate` and `build`
both `ok`). Every step's log and both classpath lists stay under the run's
directory for whoever triages.

## 6. Findings and triage

`docs/corpus/FINDINGS.md` is the ledger: one numbered entry per cause, not per
project, carrying the project and step it was seen in, the error, the cause
once known, its bin and its state. Its first entries are the five of
TEST_JVM_BENCHMARK §7.1, renumbered and cited back. `corpus.toml` maps
each known failure to its entry (`known = { test = 9 }`), which is how the
table prints `F9` instead of `FAIL`.

Every finding goes in one of three bins, as ROADMAP_ARCHIVE §1 set out:

1. **A jrs bug.** Fixed, with a regression test from a minimal reproduction:
   a POM published into `tests/fixtures/repo`, a project in
   `tests/build.rs`, never a test that reaches for Maven Central. The entry
   names the test.
2. **A migration gap.** A new rule in `src/migrate/` and a fixture in
   `tests/fixtures/migrate/` with its `expected-jrs.toml`, as every other
   migration rule has. Finding 5's two halves (`version kotlinVersion`,
   a Kotlin `@SpringBootApplication`) are the first two.
3. **A non-goal in the way.** The project is cited in ROADMAP_ARCHIVE §5
   beside the idea it needs, and its entry in `corpus.toml` keeps its
   `known` mapping. That row is the evidence the table there waits for
   (kapt and KSP, the annotation-processor path, Gradle Module Metadata).

A finding is closed when the corpus run after the fix shows the cell `ok`;
its `known` mapping is then removed, so a regression shows as `FAIL`.
Finding 4 of §7.1 (the test JVM does not run in the project directory) is a
SPEC §10.2 change and goes through bin 1 with that change made first.

## 7. CI

`.github/workflows/corpus.yml`, `workflow_dispatch` only — it takes too long
and needs too much of the network for every push — on `ubuntu-latest`,
which has Docker for Testcontainers:

- inputs: `projects` (default `all`) and `jdk` (default `25`);
- installs that JDK with `actions/setup-java`, plus pinned Maven and Gradle
  for projects without a wrapper;
- builds jrs in release mode and runs `cargo bench --bench corpus` with
  `JRS_CORPUS` from the input;
- uploads `target/corpus/` as the run's artifact — the logs, the
  comparisons and `REPORT.md`, without the clones and the cache — and
  writes `REPORT.md` to the job summary.

A run on JDK 17 and 21 is a matter of the input; the matrix is not built in
until a finding shows the JDK matters. `rust.yml`'s `release` job does not
call it: a release is not blocked on a third party's repository, but the
compatibility table (§8) is refreshed from a run on the release commit.

## 8. The compatibility table

A "Compatibility" section in `DOCS.md`, and the same table on the website's
docs page, generated from a CI run's `REPORT.md` and committed by hand:
one row per corpus project with its build tool, kind, pinned commit, each
step's cell, and for every non-`ok` cell a sentence on why and the finding
it links. It names the jrs version and date it was measured on. A user
reads it before trying their own project; the Why page's "It builds real
projects" row links to it, replacing what the page says today.

## 9. Done

ROADMAP §1 is done when:

- every corpus project is `ok` end to end, or each non-`ok` cell is a
  bin-3 finding cited in ROADMAP_ARCHIVE §5;
- `migrate first try` is every project not blocked by a bin-3 finding;
- the compatibility table is published, and the corpus has run green on a
  release commit.

The corpus is not frozen there: new projects are added when a user reports
one that fails, which is cheaper than any other way of finding the next
bug. The ROADMAP §2 benchmarks take their projects from it, and
TEST_JVM_BENCHMARK §3's corpus becomes this one.

## 10. Changes this implies

1. **`Cargo.toml`**: a `[[bench]] name = "corpus"`, `harness = false`.
2. **`docs/corpus/corpus.toml`, `docs/corpus/FINDINGS.md`**: new. Neither is
   read by jrs.
3. **`.github/workflows/corpus.yml`**: new, manual only.
4. **`DOCS.md`**: the Compatibility section (§8); the Development section
   gains the `cargo bench --bench corpus` command beside the other benches.
5. **`CLAUDE.md`**: the command in its bench list, and a line that only the
   corpus and `tests/network.rs` may reach Maven Central.
6. **`website/docs/index.html`** gets the table; **`website/why/`** links it.
7. **`TEST_JVM_BENCHMARK.md` §3** points to this corpus instead of
   ROADMAP_ARCHIVE §1.
8. **SPEC.md**: nothing for the harness. Each bin-1 finding that changes
   behaviour carries its own SPEC edit.

## 11. Open questions

1. ~~**Which Ktor samples, and which mid-sized service?**~~ Decided with
   the list: each `ktor-samples` sample is a standalone build with its own
   wrapper, so two are entries with `dir` pointing into the clone —
   `httpbin` (kotlinx.serialization) and `h2` (none). The service is
   `spring-petclinic-rest`: MapStruct, Spring Security, springdoc and
   OpenAPI Generator, H2 and PostgreSQL. It has no Testcontainers; a service
   that does is still wanted.
2. **Is the reference's test run worth its time?** It doubles each
   project's minutes. Recording the reference counts in `corpus.toml` at
   pin time and only re-running the reference with `JRS_CORPUS_REFERENCE=1`
   would halve the run, at the risk of a stale count after a pin moves.
3. **Gradle version differences.** If they turn out to be the largest class
   of difference, is a migration that writes `[managed]` pins for them the
   answer, or the opt-in highest-wins mediation of ROADMAP_ARCHIVE §5?
4. **The maintainer's private projects.** If no public stand-in reproduces
   their failures, they could run through the same harness from a private
   `corpus.local.toml` that is never committed, with only their findings
   (not their names) entering the ledger.
