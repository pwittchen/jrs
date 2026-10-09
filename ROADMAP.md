# jrs — Roadmap

Every milestone in [SPEC §12](SPEC.md#12-roadmap) has landed. This
document lists only what is still open. Much of it closes a gap with Gradle, and those items
name the Gradle feature they answer. jrs is not trying to become Gradle
(SPEC §1.2), so each one is the single-module, declarative version of what a
Gradle build gets from a plugin or a DSL block, and it arrives with its row in
`jrs migrate` and a fixture in `tests/fixtures/migrate/`, as `test.forks` did
for `maxParallelForks` (`gradle-forks`). Anything that touches a
[non-goal](SPEC.md#12-non-goals) or adds a crate is a spec-level
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

1. **Single-module services.** A Spring Boot or Ktor service in a repository
   of its own is the shape jrs was built for. Section 1 is about exactly these
   projects.
2. **Command-line tools and desktop apps.** The fat jar, `--jlink`,
   `--jpackage`, `--native-image` and `--obfuscate` are built in; in Maven or
   Gradle each is a plugin with its own configuration.
3. **Teams for whom the JVM is a guest language.** Teams working mostly in Rust,
   Go or TypeScript, with a Java or Kotlin service or two. They know Cargo, npm
   and `go.mod`, and have nobody who owns a Gradle build.
4. **Builds with supply-chain requirements.** A lockfile, checksums,
   reproducible jars and no build scripts are auditable by reading two files.
   `jrs package --sbom`, `jrs licenses` and a vulnerability audit
   (section 5) complete the picture.
5. **Coding agents.** A declarative TOML manifest, `jrs add` and `jrs
   remove`, plain output with `--progress never`, `jrs metadata` as JSON and
   a fast edit–build–test loop are what an agent needs from a build, and what
   a Gradle build makes slow and error-prone.
6. **Learning and first projects.** The step up from a single file or a jbang
   script to a real project, without learning Maven first.

It is not for libraries published to Maven Central, Android, or builds that
need Gradle's plugin ecosystem (section 5 says why). It is not for
multi-module builds yet either; section 7 is the plan for them.

**The claims, and the evidence each needs.** Every claim on the Why page has to
be one a sceptical reader can check. Where the evidence does not exist yet,
the page says what jrs does and leaves out how it compares:

| Claim | Evidence | Where |
| --- | --- | --- |
| Fast start, no daemon | No-op and incremental rebuild times against Maven and Gradle (warm daemon and `--no-daemon`) | Section 2 |
| It builds real projects | The corpus's compatibility table | Section 1 |
| Reproducible by default | Two builds of every example compared byte for byte in CI | The `reproducible` job in `rust.yml` |
| Auditable supply chain | `jrs package --sbom`, `jrs licenses` | Built in |
| Works in the IDE | The IntelliJ plugin | Section 4 |

**Order of work.** The plan follows from the table: first what stops people
from trying jrs at all, then what proves the claims, then what widens the
audience.

1. **Section 1, the real-project corpus.** A user's first project has to
   build; nothing else matters until it does.
2. **Section 4, the IntelliJ plugin.** Without it, a jrs project is a folder of
   files in the IDE most JVM developers use, and nobody works that way every day.
3. **Section 2, benchmarks.** They turn "fast" from a promise into a table,
   and the Why page and README link to the report once it exists.
4. **Section 3, publishing to the package managers**, so trying jrs costs
   one command on every platform.
5. **One spec decision, taken early: `jrs install`** (section 5, publishing).
   Installing a jar into `~/.m2` is the smallest step past one module: it
   lets a team split a library out of a service without a reactor, which is
   the most common reason a project cannot try jrs.
   Multi-module builds (section 7) are the full answer, and come after it.
6. **A guide for coding agents** in `DOCS.md` and on the website: which
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

## 4. An IntelliJ plugin

Today IntelliJ IDEA does not recognise a jrs project. Someone who opens one
gets a folder of `.java` files with no source roots, no JDK and no libraries,
unless they keep a `pom.xml` or a Gradle build beside `jrs.toml` for the IDE
alone. A plugin that imports a jrs project the way IntelliJ imports a Maven or
Gradle one closes that gap. It needs no daemon: `jrs metadata` (SPEC §5.4) is
already the project model an editor plugin needs, and was written for this.

- **Import and sync.** Opening a directory with a `jrs.toml` offers to import
  it. The plugin runs `jrs metadata` and builds one IntelliJ module from the
  JSON: the main and test source roots for each language, the generated
  sources and resources, the output directories, the JDK from `jdk.home`,
  and one library per classpath entry, with its `-sources.jar` attached when
  `jrs fetch --sources` has cached it. Compile-only and runtime-only
  dependencies get the matching IntelliJ scope, and the test classpath goes to
  the test scope. A change to `jrs.toml` or `jrs.lock` shows the usual "Load
  jrs changes" prompt, and a `jrs metadata --no-deps` pass sets up the source
  roots before the first resolution has finished.
- **Kotlin, Scala and Groovy.** The module's language levels and compiler
  settings come from the metadata's `languages` and the `[kotlin]`,
  `[scala]` and `[groovy]` tables, so the IDE's own Kotlin, Scala and Groovy
  plugins analyse the code with the compiler version the build uses. Where
  `jrs metadata` does not yet carry a setting the IDE needs, such as the
  compiler arguments or `[kotlin] plugins`, the key is added to the JSON
  within its current `version`, as SPEC §5.4 allows.
- **Build and run through jrs.** Build Project delegates to `jrs build`, as
  IntelliJ delegates to Gradle, so the IDE and the terminal produce the same
  `target/classes`. Run configurations cover `jrs run`, `jrs test` (a
  class run from the gutter becomes `--filter`; a single method needs a
  selector jrs does not have yet), `jrs package` and `jrs task
  <name>`, with `--debug` wired to the IDE's debugger. Test results come from
  the JUnit XML in `target/test-reports` into the test runner tree, and the
  build's output keeps jrs's plain transcript (`--progress never`), with
  `javac`'s diagnostics linked back to the source.
- **A jrs tool window.** The tasks from `jrs task --list` and the hooks that run
  them, the dependency graph from `jrs tree`, and actions for `jrs update`,
  `jrs outdated`, `jrs verify` and `jrs clean`.
- **Editing `jrs.toml`.** Completion and validation for the manifest's tables
  and keys, and the same unknown-key warnings the manifest parser gives. Adding
  or removing a dependency goes through `jrs add` and `jrs remove`, so
  `edit.rs` stays the one place that rewrites the manifest.
- **A file icon for `jrs.toml` and `jrs.lock`.** Both files show the jrs icon
  in the project view and the editor tabs, as `pom.xml` and `build.gradle`
  show Maven's and Gradle's. The icon is drawn from `logo.png` and checked in
  to this repository before the plugin work starts — an SVG, with the 16×16
  light and `_dark` variants the IntelliJ Platform expects — so the plugin
  takes it from here rather than keeping a copy of its own.
- **Migration from the IDE.** An action on a Maven or Gradle project runs
  `jrs migrate`, shows its report, and imports the result.
- **Where it lives.** A separate repository, written in Kotlin against the
  IntelliJ Platform SDK and built with the IntelliJ Platform Gradle Plugin,
  since that is the only supported way to build one. It is published to the
  JetBrains Marketplace and tested with the platform's own test framework,
  against a `jrs` binary on `PATH`. It adds no crate to jrs: what the plugin
  needs from jrs goes into `jrs metadata` and the commands' existing flags.
  If a long-lived process turns out to be needed after all, that is the Build
  Server Protocol row in section 5.

## 5. Needs a spec decision first

These cross a line drawn in SPEC §1.2 or §13 (or the dependency list). They are
listed so the discussion has a home, not because they are planned.

| Idea | What it crosses |
| --- | --- |
| Multi-module builds / workspaces | Non-goal: one module per manifest. Planned in section 7, which starts with the spec change |
| Composite builds (Gradle's `includeBuild`), dependency substitution with a local checkout | Non-goal: one module per manifest. Without them, the way to try a change to a library in the project that uses it is a `-SNAPSHOT` in `~/.m2` through a `file://` repository |
| Publishing (Gradle's `maven-publish`, `publishToMavenLocal`) | Non-goal: SPEC §1.2 rules out `deploy`/`publish`. Done properly it means generating a POM from the manifest, sources and Javadoc jars (`jrs package --sources --javadoc` writes them), signing, and uploading with credentials. A `jrs install` into `~/.m2` is the smallest version, and it is the one that makes library development across projects bearable without a reactor |
| JDK auto-provisioning (Gradle's toolchain resolvers, foojay) | Downloads from a host that is not a Maven repository. Unix JDKs ship as tar.gz, which means new crates. Today a pinned JDK that is not installed is an error listing the ones that are (SPEC §7.1) |
| A wrapper that fetches the pinned jrs (Gradle's `gradlew`) | jrs would download and run executables. Today `project.jrs-version` (SPEC §4.3) makes an older jrs stop and name the version the project needs |
| Per-suite dependencies, `java-test-fixtures`, multi-release jars | `[test.suites]` (SPEC §10.2) gives a second test suite its own sources, JVM and `jrs test --suite` selection on the one test classpath. Dependencies of its own, as Gradle's JVM Test Suite plugin has them, would put a graph per suite in the lockfile; multi-release jars need a source root per Java release |
| Build variants (Gradle `-P` properties, Maven profiles) | The manifest is one fixed configuration. Conditional configuration is the start of a DSL |
| PGP signature verification (Gradle's dependency verification) | An OpenPGP crate and a trust store. `jrs.lock` already pins a checksum for every non-snapshot jar, which covers the "bytes changed under the same version" case |
| A compiler daemon (the Gradle daemon) | Non-goal: SPEC §1.2 rules out a compiler process that outlives the command that started it. A `--watch` session already keeps a `javac` worker for its own lifetime (SPEC §7.5). What is left is a daemon across commands, which jrs would have to find and manage, and a worker for kotlinc and scalac, which add a second or so of JVM start-up to a changed build ([JVM_LANGUAGES.md §14.1](specs/JVM_LANGUAGES.md#14-open-questions)) and wait for a measurement saying it is felt |
| A Build Server Protocol server | A long-lived JSON-RPC process that IntelliJ, Metals and VS Code can talk to. `jrs metadata` (SPEC §5.4) is the first step and needs no daemon |
| Annotation-processor path in the manifest | Non-goal: no annotation-processor configuration. Processors on the compile classpath, as `compile-only` dependencies with `-proc:full`, work today. Error Prone, NullAway and other `javac` plugins need a processor path too |
| JPMS (`module-info.java`, module path) | Non-goal |
| JVM languages beyond Kotlin, Scala and Groovy; Kotlin Multiplatform | Non-goal (SPEC §1.2) |
| Kotlin compiler plugins with configuration, kapt/KSP | `[kotlin] plugins` turns on the plugins that need only their name (`serialization`, `spring`, `jpa`, `power-assert`, and `allopen`/`noarg` with their options in `kotlinc-args`). A plugin block of its own (`allOpen { annotation(...) }`) is plugin configuration ([JVM_LANGUAGES.md §14.2](specs/JVM_LANGUAGES.md#14-open-questions)); kapt and KSP run processors over Kotlin and need a processor path |
| Dokka | A plugin host with its own configuration (JVM_LANGUAGES.md §14.4). `jrs doc` runs Scaladoc and Groovydoc, but a Kotlin unit's Kotlin sources are still left out of its `javadoc`, with a warning |
| sbt-style `%%` cross-version keys | New key syntax, touching `edit.rs`, the lockfile and migration (JVM_LANGUAGES.md §14.3) |
| TestNG | SPEC §13.7: the JUnit Platform only (Jupiter and Vintage) |
| Version ranges | SPEC §8.2 rejects them rather than guessing |
| Container images (Jib, Boot's `bootBuildImage`) | A layered OCI image written without Docker, pushed to a registry: a host that is not a Maven repository, registry credentials, and tar layers. `jrs package --jlink` already builds the runtime such an image would hold |
| A vulnerability audit (`jrs audit`, OWASP Dependency-Check) | Queries a host that is not a Maven repository (OSV, GitHub advisories). `jrs.lock` holds exactly the coordinates to ask about, and `jrs outdated` already says what to upgrade to |
| Incremental Kotlin compilation (the Kotlin Build Tools API) | `compile/incremental.rs` compiles a Java-only unit file by file; a Kotlin unit is compiled whole. Kotlin's own incremental compiler keeps caches of its own and wants to run in-process, which is close to the compiler-daemon non-goal |
| Highest-wins mediation (opt-in) | SPEC §13.6 chose nearest-wins. It is Gradle's default, so migrated Gradle builds can resolve to different versions; the migration report could flag where the two strategies disagree |
| Gradle Module Metadata (`.module` files) beyond the JVM variant | jrs reads a `.module` only to follow a Multiplatform library's root to its JVM artifact (`resolve/gradle_module.rs`). Its rich versions (`strictly`, `prefer`, `reject`, ranges), dependency constraints and capabilities do not fit nearest-wins and the no-ranges rule (SPEC §8.2); honouring them means a different resolver |

## 6. Faster than Gradle

jrs starts faster than Gradle: it is a native binary with no configuration
phase. Where Gradle still wins is the JVM work itself. Most of that gap is
closed now ([specs/FASTER_BUILDS.md](specs/FASTER_BUILDS.md)): the compiler
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
  [specs/TEST_JVM_BENCHMARK.md](specs/TEST_JVM_BENCHMARK.md) is the plan
  and its decision rules; `cargo bench --bench test_jvm` is the harness.
  Its first pass (§7 there) keeps both opt-in and names what blocks them: a
  jar deserialising a project class cannot see it under the shared layout,
  the AOT cache is never assembled after Mockito self-attaches, a forked run
  never writes an archive, and the test JVM does not run in the project
  directory.
- **Per-class test results in the build cache**, keyed by each class's
  reach, so that a branch switch reuses part of a suite's results rather
  than all or nothing: [specs/TEST_RESULT_CACHE.md](specs/TEST_RESULT_CACHE.md),
  a proposal.

What stays out: a background daemon and an in-house compiler. Both would trade
away the "no daemon, just a driver" defaults the Why page rests on.

## 7. Multi-module projects

Most Spring Boot and Ktor codebases that outgrow one repository-sized service
split into modules before they split into repositories: a `domain` or `core`
library, an `api` module, one or two applications on top. Today jrs stops at
the first `<modules>` or `include`: `jrs migrate` translates the module it was
pointed at and lists the others, and the user is left wiring them together
through `~/.m2` by hand. That is the most common reason a real project cannot
try jrs at all, after the corpus failures of section 1.

This crosses the "one module per manifest" non-goal (SPEC §1.2) and reverses
SPEC open question 11, so it starts with a spec change — a section of its own,
in the manner of `specs/JVM_LANGUAGES.md` — before any code. The shape it
should take is Cargo's workspace, not Gradle's reactor: modules are declared,
not configured by a script, and the root adds nothing a member manifest could
not say for itself.

- **A workspace manifest.** A root `jrs.toml` with a `[workspace]` table
  listing its members by directory (`members = ["core", "api", "app"]`). Keys
  every member shares — `project.java`, `[repositories]`, `[managed]`, the
  language versions — are written once at the root and inherited, with a
  member's own key winning, as Cargo's `workspace = true` does. A root
  without `[project]` is a pure aggregator; a root with one is a member too.
- **Module dependencies.** A member depends on a sibling by path
  (`core = { path = "../core" }`), in any scope. The sibling's classes go on
  the classpath where its jar would, and its own dependencies join the graph
  as a jar's transitive ones do, so nearest-wins mediation runs once over the
  whole workspace. Cycles between modules are an error naming the cycle.
- **One lockfile.** `jrs.lock` lives at the root and pins one version of
  every coordinate for every member, so two modules can never link against
  different versions of the same library. Path dependencies are recorded
  relative to the root, never absolutely, and a workspace lockfile is a new
  `version`; a single-module project keeps its lockfile byte for byte.
- **Building the graph.** Members build in dependency order, independent
  ones in parallel on the `rayon` pool the downloads already use. Compile
  avoidance carries across the module boundary: a downstream unit's
  fingerprint holds `compile::api_digest` of its upstream's classes, so a
  body change in `core` recompiles `core` alone, and `jrs test`'s selection
  (`compile/impact.rs`) follows a change into the modules that reach it.
  Phase lines name their module, in a fixed order whatever the parallelism,
  so `--progress never` stays deterministic.
- **Commands.** At the root every command acts on all members; `-p <module>`
  (or running from inside a member's directory) narrows it to one and the
  modules it depends on. `jrs run` needs a member with a main class, and
  picks it when there is exactly one. `jrs package` writes one jar per member;
  a fat jar of an application carries its sibling modules' classes, under the
  same merge and relocation rules as any dependency. `[tasks]` and `[hooks]`
  stay per member, with a task able to depend on another member's.
- **Tooling.** `jrs metadata` lists every member with its module
  dependencies, so the IntelliJ plugin (section 4) imports one IntelliJ module
  per member. `jrs tree`, `jrs outdated` and `jrs add`/`remove` take `-p`;
  `jrs add` at the root of a pure aggregator asks which member.
- **Migration from Maven.** `jrs migrate` on an aggregator POM writes the
  root manifest and one manifest per `<module>`, recursively. The parent's
  `<properties>`, `<dependencyManagement>` and compiler settings become the
  root's shared keys and `[managed]`; a `<dependency>` on a sibling's own
  coordinates becomes a path dependency; `<pluginManagement>` feeds each
  member's plugin translation as Maven's inheritance does. A parent that is
  not also the aggregator, and modules reached only through a profile's
  `<modules>`, each get a review line.
- **Migration from Gradle.** `settings.gradle(.kts)`'s `include(...)` and
  `rootProject.name` give the members, `project(":core")` dependencies become
  path dependencies, and a version catalog at the root becomes shared
  `[managed]` entries. `allprojects { }` and `subprojects { }` blocks, and
  convention plugins in `buildSrc` or `build-logic`, are code: the parts that
  are plain declarations (a Java toolchain, a repository, a common dependency)
  are translated into the root's shared keys, and the rest is reported per
  member, as an unknown plugin is today. `includeBuild` stays a composite
  build (section 5).
- **One report.** The migration report gains a section per member, and the
  summary says how many members migrated cleanly. New fixtures in
  `tests/fixtures/migrate/` — a Maven aggregator with a parent, a Gradle build
  with `include`, a version catalog and a `subprojects { }` block — cover
  each rule, and multi-module Spring Boot and Ktor projects join the section 1
  corpus, so the end-to-end pass judges the result.

What stays out: a member configuring another member, build logic at the root
beyond shared keys, per-module lockfiles, and composite builds across
repositories. Each would bring back the scripted configuration phase jrs was
built without.
