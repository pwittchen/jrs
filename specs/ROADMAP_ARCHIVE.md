# jrs — Roadmap archive

The roadmap as it stood on 2026-10-14, before [ROADMAP.md](../ROADMAP.md) was
narrowed to four items. Nothing here was dropped: what is still open below
stays open, and the four items of the new roadmap take sections 1, 2, 3 and 6
and parts of 4, 5 and 7 further, each with a spec in `specs/todo/`. Section
numbers cited elsewhere as "ROADMAP_ARCHIVE §n" refer to this document.

---

Every milestone in [SPEC §12](../SPEC.md#12-roadmap) has landed. This
document lists only what is still open. Much of it closes a gap with Gradle, and those items
name the Gradle feature they answer. jrs is not trying to become Gradle
(SPEC §1.2), so each one is the declarative version of what a
Gradle build gets from a plugin or a DSL block, and it arrives with its row in
`jrs migrate` and a fixture in `tests/fixtures/migrate/`, as `test.forks` did
for `maxParallelForks` (`gradle-forks`). Anything that touches a
[non-goal](../SPEC.md#12-non-goals) or adds a crate is a spec-level
decision (see the last section) — it needs a spec change before it needs code.

---

## Where jrs is aimed

jrs will not out-feature Gradle, and a smaller build tool for Java is a crowded
idea (Mill, bld, JeKa, Amper, jbang, mvnd, Declarative Gradle). What sets it
apart is a set of defaults the incumbents can only reach by configuration, or
not at all: a native binary with no daemon, a lockfile with a checksum for
every jar, byte-identical jars, and a build that runs no code of the
project's own inside the tool. The [Why jrs](https://getjrs.dev/why/) page and
the README's "Why jrs" section make that case; this section is the plan for
backing it with evidence and for aiming the work at the people it fits.

**Who it is for.** In order of how well jrs fits them today:

1. **Services, in one module or a few.** A Spring Boot or Ktor service in a
   repository of its own is the shape jrs was built for, and since workspaces
   (section 7) that includes one split into a `core`, an `api` and an
   application. Section 1 is about exactly these projects.
2. **Command-line tools and desktop apps.** The fat jar, `--jlink`,
   `--jpackage`, `--native-image` and `--obfuscate` are built in; in Maven or
   Gradle each is a plugin with its own configuration.
3. **Teams for whom the JVM is a guest language.** Teams working mostly in Rust,
   Go or TypeScript, with a Java or Kotlin service or two. They know Cargo, npm
   and `go.mod`, and have nobody who owns a Gradle build.
4. **Builds with supply-chain requirements.** A lockfile, checksums,
   reproducible jars and no build scripts are auditable by reading two files.
   `jrs package --sbom`, `jrs licenses` and `jrs audit` complete the
   picture.
5. **Coding agents.** A declarative TOML manifest, `jrs add` and `jrs
   remove`, plain output with `--progress never`, `jrs metadata` as JSON and
   a fast edit–build–test loop are what an agent needs from a build, and what
   a Gradle build makes slow and error-prone.
6. **Learning and first projects.** The step up from a single file or a jbang
   script to a real project, without learning Maven first.

It is not for libraries published to Maven Central, Android, or builds that
need Gradle's plugin ecosystem (section 5 says why). Multi-module builds it
takes as workspaces (section 7), declared rather than configured: a build
whose modules configure each other from `subprojects { }` code migrates with
that code reported, not translated.

**The claims, and the evidence each needs.** Every claim on the Why page has to
be one a sceptical reader can check. Where the evidence does not exist yet,
the page says what jrs does and leaves out how it compares:

| Claim | Evidence | Where |
| --- | --- | --- |
| Fast start, no daemon | No-op and incremental rebuild times against Maven and Gradle (warm daemon and `--no-daemon`) | Section 2 |
| It builds real projects | The corpus's compatibility table | Section 1 |
| Reproducible by default | Two builds of every example compared byte for byte in CI | The `reproducible` job in `rust.yml` |
| Auditable supply chain | `jrs package --sbom`, `jrs licenses`, `jrs audit` | Built in |
| Works in the IDE | The IntelliJ plugin | `plugins/intellij` |

**Order of work.** The plan follows from the table: first what stops people
from trying jrs at all, then what proves the claims, then what widens the
audience.

1. **Section 1, the real-project corpus.** A user's first project has to
   build; nothing else matters until it does.
2. **Section 2, benchmarks.** They turn "fast" from a promise into a table,
   and the Why page and README link to the report once it exists.
3. **Section 3, publishing to the package managers**, so trying jrs costs
   one command on every platform.
4. **One spec decision, taken early: `jrs install`** (section 5, publishing).
   Installing a jar into `~/.m2` lets a library be shared between projects in
   different repositories, which a workspace (section 7) does not reach:
   its members all live under one root.
5. **A guide for coding agents** in `DOCS.md` and on the website: which
   commands to run, which flags give stable output, and how to read
   `jrs metadata`. It needs no code, only documentation of what exists.

**How it is measured.** Adoption is not something this repository can count,
since jrs sends no telemetry. What it can track is the number
of corpus projects that build end to end, the benchmark report, and the rows
`jrs migrate` still reports as not migrated.

---

## 1. Hardening against real projects

The `examples/` projects and the `tests/fixtures/migrate/` fixtures are small
and written for jrs: each one shows a feature, and each one builds. Real
projects are not like that. Tried on the maintainer's own Java and Kotlin
projects, `jrs migrate` and the build that follows did not fully work. Before
more features are added, jrs has to migrate, build, test, run and package real
projects of the two kinds people are most likely to bring to it: **Java with
Spring Boot** and **Kotlin with Ktor**.

- **A corpus of real projects.** A fixed list of open-source projects, each
  pinned to a commit, from both kinds and both build tools: Spring Boot on
  Maven (`spring-boot-starter-parent`) and on Gradle (Groovy and Kotlin DSL),
  Spring Boot in Kotlin, Ktor on the Kotlin DSL, with and without
  `kotlinx.serialization`. Candidates: `spring-petclinic`, the Spring guides,
  the official Ktor samples, and a mid-sized service or two with a few dozen
  dependencies, a database driver, Flyway or Liquibase, Testcontainers and
  Mockito. The maintainer's projects that failed go in first, or stand-ins
  that reproduce the same failures if they cannot be published.
- **One pass, every step.** For each project: `jrs migrate`, then `jrs build`,
  `jrs test`, `jrs run` (the application starts and answers one request) and
  `jrs package` (the jar starts with `java -jar`). The reference is the
  project's own Maven or Gradle build: the same resolved runtime classpath, the
  same number of tests run and passed. Each step's outcome is recorded, so a
  project that migrates but does not build is a finding, not a pass.
- **Record the failures.** Every failure is written down with the project, the
  step, the error and its cause, then sorted into one of three bins:
  - *a jrs bug* — fixed, with a regression test built from a minimal
    reproduction: a POM published into `tests/fixtures/repo`, a project in
    `tests/build.rs`, never a test that reaches for Maven Central;
  - *a migration gap* — a new row in `jrs migrate` and a fixture in
    `tests/fixtures/migrate/`, as every other migration rule has;
  - *a non-goal in the way* — the project is cited in section 5, next to the
    idea it needs. Several rows there wait for exactly this evidence (the
    annotation-processor path, kapt and KSP, Gradle Module Metadata's rich
    versions and constraints, the compiler daemon).
- **Java and Spring Boot, what to check.** The Spring Boot BOM through
  `[managed]` and the starters' deep graphs, including optional and
  `provided` dependencies; Lombok, MapStruct and
  `spring-boot-configuration-processor` as processors on the compile
  classpath; `application.yml` and profile resources; `@SpringBootTest`
  contexts, Testcontainers; the fat jar's Spring registry merge against an
  application with more auto-configuration than `examples/bookmarks`, which
  shows a flat fat jar is enough for Boot — no `BOOT-INF/` layout — and
  Mockito loaded as an agent from `[test] java-agents`.
- **Kotlin and Ktor, what to check.** Multiplatform roots now resolve to
  their `-jvm` artifact through the `.module` file; what is left to check is
  the long tail of Ktor and kotlinx artifacts real projects use beyond
  `examples/orders`. `io.ktor.server.netty.EngineMain` as the main
  class with `application.conf` or `application.yaml`; `ktor-server-test-host`
  tests; Logback and `META-INF/services` in the fat jar; mixed Kotlin and Java
  sources; `kotlinc` arguments and `jvmToolchain` read from the Gradle build;
  `[kotlin] plugins` (`spring`, `serialization`) as `jrs migrate` writes them.
- **Keep it running.** The corpus needs the network and a JDK, so it lives
  outside the default `cargo test`, like `tests/network.rs`: a harness that
  clones the pinned commits into a scratch directory with its own
  `JRS_CACHE_DIR`, runs the pass and prints a table of project × step. A
  manually triggered CI workflow runs it on Linux. It adds no crate to jrs.
- **Say where jrs stands.** A compatibility table in `DOCS.md` (and on the
  website) lists which corpus projects build end to end and, for the ones that
  do not, why — so a user knows before trying their own project. The same
  projects then feed the benchmarks in section 2, which are only worth running
  on projects that build.

## 2. Benchmarks against Maven and Gradle

The M5 benchmark (`benches/resolution.rs`) measures jrs against itself and the
network floor. It does not say how jrs compares to the tools people would
otherwise use. The next benchmark does: jrs, Maven and Gradle building, running
and testing the same projects, with the results written up in a report.

- **Identical projects.** Each benchmark project has a `jrs.toml`, a `pom.xml`
  and a `build.gradle.kts` that describe the same build: the same sources, the
  same dependencies at the same versions, the same JDK and `--release`, and
  JUnit 5 for the tests. The set runs from the small `examples/` projects up to
  a generated project with a few hundred classes and a few dozen dependencies,
  so both start-up cost and throughput show. Before any timing, the harness
  checks that the three builds match: the same resolved classpath, the same
  number of tests run and passed, the same `run` output.
- **Scenarios.** A clean build with a cold dependency cache, a clean build with
  a warm cache, a no-op rebuild, an incremental rebuild after touching one
  source file, `test`, `run` and `package`, each timed as its own command.
- **A fair setup.** Pinned Maven and Gradle versions with a default
  configuration and no build cache or tuning that jrs has no counterpart for.
  Gradle is measured both with a warm daemon and with `--no-daemon`, since the
  daemon is its answer to start-up cost. Dependencies come from a local mirror
  (as in M5), not Maven Central, so network latency does not swamp the numbers.
  Every scenario has warm-up runs and repeated measured runs, and the report
  gives the median and the spread, not a single best time.
- **The report.** A generated `benchmarks/REPORT.md`, one table per scenario,
  with the machine, the OS, the JDK and every tool's version recorded next to
  the numbers. Where jrs is slower, the report says so. jrs's own totals are
  explained by its `--timings` report, whose copy in
  `target/.jrs/timings.txt` is tab-separated so the harness can read it back.
- **Where it runs.** Maven and Gradle are not something `cargo test` or `cargo
  bench` can assume, so the harness lives outside the default test run and skips
  a tool that is not installed, as `require_jdk!` does. It adds no crate to jrs.
  A manually triggered CI workflow can regenerate the report on a fixed runner.

## 3. Publishing to the package managers

Every release now builds a Windows ARM64 binary beside the others, and its
`release` job writes a Homebrew formula, a Scoop manifest and a winget
manifest from the release's checksums (`packaging/manifests.sh`, kept as the
run's `package-manifests` artifact). The `setup-jrs/` action installs a
release in GitHub Actions. What is left is putting the manifests where the
package managers look:

- **A Homebrew tap**, `pwittchen/homebrew-jrs`, holding `Formula/jrs.rb`, so
  that `brew install pwittchen/jrs/jrs` works; then a job that commits each
  release's formula there, with a token scoped to that repository.
- **A Scoop bucket** the same way, and a pull request to `winget-pkgs` per
  release (`wingetcreate` can open it from the manifests).
- **DOCS.md and the website's install section** list the three commands once
  they work, and `website/install.sh` mentions them for Windows.

## 4. The IntelliJ plugin

The plugin lives in `plugins/intellij` (its README says how to build and use
it), written in Kotlin against the IntelliJ Platform SDK. It imports a jrs
project from `jrs metadata`: source roots, outputs, the JDK, and one library
per classpath entry in its scope, with its sources jar. It shows "Load jrs
changes" when `jrs.toml` or `jrs.lock` changes, and delegates Build Project to
`jrs build`. It has run configurations for every command, with `jrs test` from
the gutter (`--filter`, `--method`), JUnit XML in the test tree and the
debugger on `--debug`. It also has a tool window, `jrs.toml` completion and
unknown-key warnings, `jrs add`/`remove`, `jrs migrate`, and the logo on both
files. What is left:

- **Kotlin, Scala and Groovy compiler settings.** The IDE's language plugins
  get the source roots and the runtime libraries, but not the compiler
  arguments or `[kotlin] plugins`. Those need new keys in `jrs metadata`,
  within its current `version` (SPEC §5.4), before the plugin can pass them on.
- **A file icon drawn for 16×16.** The plugin scales `logo.png` down. An SVG
  with the `_dark` variant the IntelliJ Platform expects would read better
  at that size.
- **Publishing to the JetBrains Marketplace.** CI already builds and tests
  the plugin, and each release ships it as `jrs-intellij.zip`; what is left is
  a release to the Marketplace.

## 5. Needs a spec decision first

These cross a line drawn in SPEC §1.2 or §13 (or the dependency list). They are
listed so the discussion has a home, not because they are planned.

| Idea | What it crosses |
| --- | --- |
| Composite builds (Gradle's `includeBuild`), dependency substitution with a local checkout | Non-goal: a workspace's members live under its root ([WORKSPACES.md §9](WORKSPACES.md#9-what-stays-out)). Without them, the way to try a change to a library in the project that uses it is a `-SNAPSHOT` in `~/.m2` through a `file://` repository |
| Publishing (Gradle's `maven-publish`, `publishToMavenLocal`) | Non-goal: SPEC §1.2 rules out `deploy`/`publish`. Done properly it means generating a POM from the manifest, sources and Javadoc jars (`jrs package --sources --javadoc` writes them), signing, and uploading with credentials. A `jrs install` into `~/.m2` is the smallest version, and it is the one that makes library development across projects bearable without a reactor |
| JDK auto-provisioning (Gradle's toolchain resolvers, foojay) | Downloads from a host that is not a Maven repository. Unix JDKs ship as tar.gz, which means new crates. Today a pinned JDK that is not installed is an error listing the ones that are (SPEC §7.1) |
| A wrapper that fetches the pinned jrs (Gradle's `gradlew`) | jrs would download and run executables. Today `project.jrs-version` (SPEC §4.3) makes an older jrs stop and name the version the project needs |
| Per-suite dependencies, `java-test-fixtures`, multi-release jars | `[test.suites]` (SPEC §10.2) gives a second test suite its own sources, JVM and `jrs test --suite` selection on the one test classpath. Dependencies of its own, as Gradle's JVM Test Suite plugin has them, would put a graph per suite in the lockfile; multi-release jars need a source root per Java release |
| Build variants (Gradle `-P` properties, Maven profiles) | The manifest is one fixed configuration. Conditional configuration is the start of a DSL |
| PGP signature verification (Gradle's dependency verification) | An OpenPGP crate and a trust store. `jrs.lock` already pins a checksum for every non-snapshot jar, which covers the "bytes changed under the same version" case |
| A compiler daemon (the Gradle daemon) | Non-goal: SPEC §1.2 rules out a compiler process that outlives the command that started it. A `--watch` session already keeps a `javac` worker for its own lifetime (SPEC §7.5). What is left is a daemon across commands, which jrs would have to find and manage, and a worker for kotlinc and scalac, which add a second or so of JVM start-up to a changed build ([JVM_LANGUAGES.md §14.1](JVM_LANGUAGES.md#14-open-questions)) and wait for a measurement saying it is felt |
| A Build Server Protocol server | A long-lived JSON-RPC process that IntelliJ, Metals and VS Code can talk to. `jrs metadata` (SPEC §5.4) is the first step and needs no daemon |
| Annotation-processor path in the manifest | Non-goal: no annotation-processor configuration. Processors on the compile classpath, as `compile-only` dependencies with `-proc:full`, work today. Error Prone, NullAway and other `javac` plugins need a processor path too |
| JPMS (`module-info.java`, module path) | Non-goal |
| JVM languages beyond Kotlin, Scala and Groovy; Kotlin Multiplatform | Non-goal (SPEC §1.2) |
| Kotlin compiler plugins with configuration, kapt/KSP | `[kotlin] plugins` turns on the plugins that need only their name (`serialization`, `spring`, `jpa`, `power-assert`, and `allopen`/`noarg` with their options in `kotlinc-args`). A plugin block of its own (`allOpen { annotation(...) }`) is plugin configuration ([JVM_LANGUAGES.md §14.2](JVM_LANGUAGES.md#14-open-questions)); kapt and KSP run processors over Kotlin and need a processor path |
| Dokka | A plugin host with its own configuration (JVM_LANGUAGES.md §14.4). `jrs doc` runs Scaladoc and Groovydoc, but a Kotlin unit's Kotlin sources are still left out of its `javadoc`, with a warning |
| sbt-style `%%` cross-version keys | New key syntax, touching `edit.rs`, the lockfile and migration (JVM_LANGUAGES.md §14.3) |
| TestNG | SPEC §13.7: the JUnit Platform only (Jupiter and Vintage) |
| Version ranges | SPEC §8.2 rejects them rather than guessing. The corpus's `microservices` (`spring-petclinic-microservices`) stops on one in a webjar's POM ([corpus finding 11](corpus/FINDINGS.md#11-a-version-range-in-a-transitive-pom)); a `[managed]` pin settles it, but migration cannot know the version without resolving |
| Container images (Jib, Boot's `bootBuildImage`) | A layered OCI image written without Docker, pushed to a registry: a host that is not a Maven repository, registry credentials, and tar layers. `jrs package --jlink` already builds the runtime such an image would hold |
| Incremental Kotlin compilation (the Kotlin Build Tools API) | `compile/incremental.rs` compiles a Java-only unit file by file; a Kotlin unit is compiled whole. Kotlin's own incremental compiler keeps caches of its own and wants to run in-process, which is close to the compiler-daemon non-goal |
| Highest-wins mediation (opt-in) | SPEC §13.6 chose nearest-wins. It is Gradle's default, so migrated Gradle builds can resolve to different versions; the migration report could flag where the two strategies disagree |
| Gradle Module Metadata (`.module` files) beyond the JVM variant | jrs reads a `.module` only to follow a Multiplatform library's root to its JVM artifact (`resolve/gradle_module.rs`). Its rich versions (`strictly`, `prefer`, `reject`, ranges), dependency constraints and capabilities do not fit nearest-wins and the no-ranges rule (SPEC §8.2); honouring them means a different resolver |

## 6. Faster than Gradle

jrs starts faster than Gradle: it is a native binary with no configuration
phase. Where Gradle still wins is the JVM work itself. Most of that gap is
closed now ([specs/FASTER_BUILDS.md](FASTER_BUILDS.md)): the compiler
JVMs start from class-data-sharing archives, and the test JVM can too
(`test.share-classes`, SPEC §10.2); a build cache restores compiled classes,
passing test runs and the outputs of tasks that opt in with `cache = true`
across branches, checkouts and CI machines, locally and from a remote
(SPEC §7.6, §7.8); `--watch` compiles in one warm `javac` worker
(SPEC §7.5); and a cold build downloads the test jars behind the main
compile (SPEC §8.3). What is left is below. The benchmarks in section 2 say
which gap is real, and every item here is judged by them.

- **`test.share-classes` on by default, and the AOT cache as what it means
  on JDK 24+.** Both modes are in, opt-in (`true`, `"aot"`; SPEC §10.2).
  The launcher's class loader changes which copy of a name a test finds
  first and registries cannot be checked by name, so the default waits for
  the corpus of section 1 to run green with it on.
  [specs/TEST_JVM_BENCHMARK.md](TEST_JVM_BENCHMARK.md) is the plan
  and its decision rules; `cargo bench --bench test_jvm` is the harness.
  Its first pass (§7 there) keeps both opt-in and names what blocks them: a
  jar deserialising a project class cannot see it under the shared layout,
  the AOT cache is never assembled after Mockito self-attaches, a forked run
  never writes an archive, and the test JVM does not run in the project
  directory.
- **Per-class test results in the build cache**, keyed by each class's
  reach, so that a branch switch reuses part of a suite's results rather
  than all or nothing: [specs/TEST_RESULT_CACHE.md](TEST_RESULT_CACHE.md),
  a proposal.

What stays out: a background daemon and an in-house compiler. Both would trade
away the "no daemon, just a driver" defaults the Why page rests on.

## 7. Multi-module projects

**Done.** Workspaces are designed in
[specs/WORKSPACES.md](WORKSPACES.md), which narrowed the "one module per
manifest" non-goal (SPEC §1.2) and reversed SPEC open question 11, and
`examples/ledger` is one. In short:

- **A workspace manifest.** A root `jrs.toml` with `[workspace] members`;
  `[java]`, `[repositories]`, `[managed]` and the language tables are written
  once there and inherited, a member's own key winning. A root without
  `[project]` is a pure aggregator; a root with one is a member too.
- **Module dependencies** by path (`core = { path = "../core" }`), in any
  scope; a member's own dependencies join the graph as a jar's transitive ones
  do, and a cycle is an error naming it.
- **One lockfile** at the root, `version = 3`, mediation once over every
  member's graph; a single-module project keeps its lockfile byte for byte.
- **Building the graph** in dependency order, with compile avoidance and
  `jrs test`'s selection carried across the module boundary, and phase lines
  naming their member.
- **Commands** act on every member at the root, and `-p <member>` or running
  from inside a member narrows them to it and the members it depends on;
  `jrs run` picks the only member with a main class; a fat jar carries its
  sibling members' classes; a task can depend on another member's as
  `"<member>:<task>"`.
- **Tooling.** `jrs metadata` lists every member with its module dependencies,
  and the IntelliJ plugin imports one module per member.
- **Migration.** A Maven aggregator (with its parent's settings,
  `<dependencyManagement>` and `<pluginManagement>`) and a Gradle build with
  `include` (with its version catalog and `subprojects { }`) become a
  workspace, with a report section per member; fixtures in
  `tests/fixtures/migrate/maven-workspace` and `gradle-workspace`.

What is left: building independent members in parallel, which the plan had
and the first version leaves out to keep the phase lines in a fixed order
without buffering a member's output (WORKSPACES.md §9); and multi-module
Spring Boot and Ktor projects joining the section 1 corpus, so that the
end-to-end pass judges the migration.

What stays out: a member configuring another member, build logic at the root
beyond shared keys, per-module lockfiles, and composite builds across
repositories. Each would bring back the scripted configuration phase jrs was
built without.
