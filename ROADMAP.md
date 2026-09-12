# jrs — Roadmap

Every milestone in [SPEC §12](specs/INITIAL_SPEC.md#12-roadmap) has landed. This
document lists only what is still open. Much of it closes a gap with Gradle, and those items
name the Gradle feature they answer. jrs is not trying to become Gradle
(SPEC §1.2), so each one is the single-module, declarative version of what a
Gradle build gets from a plugin or a DSL block, and it arrives with its row in
`jrs migrate` and a fixture in `tests/fixtures/migrate/`, as `test.forks` did
for `maxParallelForks` (`gradle-forks`). Anything that touches a
[non-goal](specs/INITIAL_SPEC.md#12-non-goals) or adds a crate is a spec-level
decision (see the last section) — it needs a spec change before it needs code.

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
  - *a non-goal in the way* — the project is cited in section 3, next to the
    idea it needs. Several rows there wait for exactly this evidence (Kotlin
    compiler plugins for Spring's `allopen` and `kotlinx.serialization`, the
    annotation-processor path, Gradle Module Metadata, the compiler daemon).
- **Java and Spring Boot, what to check.** The Spring Boot BOM through
  `[managed]` and the starters' deep graphs, including optional and
  `provided` dependencies; Lombok, MapStruct and
  `spring-boot-configuration-processor` as processors on the compile
  classpath; `application.yml` and profile resources; `@SpringBootTest`
  contexts, Mockito's agent on JDK 21 and later, Testcontainers; the fat jar's
  Spring registry merge against a real application's auto-configuration, and
  whether `jrs package` has to answer Boot's nested-jar layout
  (`BOOT-INF/`) or a flat fat jar is enough.
- **Kotlin and Ktor, what to check.** Ktor and kotlinx publish Kotlin
  Multiplatform artifacts, and far more of them than `examples/orders` uses.
  Some root POMs depend on their `-jvm` artifact and resolve as they are;
  others, like `kotlinx-datetime`'s, do not, and today the user has to declare
  the `-jvm` artifact by hand ([JVM_LANGUAGES.md](specs/JVM_LANGUAGES.md)).
  `jrs migrate` should do that, or say which ones need it; `io.ktor.server.netty.EngineMain` as the main
  class with `application.conf` or `application.yaml`; `ktor-server-test-host`
  tests; Logback and `META-INF/services` in the fat jar; mixed Kotlin and Java
  sources; `kotlinc` arguments and `jvmToolchain` read from the Gradle build.
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

## 3. Needs a spec decision first

These cross a line drawn in SPEC §1.2 or §13 (or the dependency list). They are
listed so the discussion has a home, not because they are planned.

| Idea | What it crosses |
| --- | --- |
| Multi-module builds / workspaces | Non-goal: one module per manifest |
| Composite builds (Gradle's `includeBuild`), dependency substitution with a local checkout | Non-goal: one module per manifest. Without them, the way to try a change to a library in the project that uses it is a `-SNAPSHOT` in `~/.m2` through a `file://` repository |
| Publishing (Gradle's `maven-publish`, `publishToMavenLocal`) | Non-goal: SPEC §1.2 rules out `deploy`/`publish`. Done properly it means generating a POM from the manifest, sources and Javadoc jars (`jrs package --sources --javadoc` writes them), signing, and uploading with credentials. A `jrs install` into `~/.m2` is the smallest version, and it is the one that makes library development across projects bearable without a reactor |
| A build cache (Gradle's local and remote build cache) | New shared state beside the dependency cache, holding compile, test and task outputs keyed by their inputs' hash. The fingerprints already exist; the cache would reuse outputs across branches, checkouts and CI machines |
| JDK auto-provisioning (Gradle's toolchain resolvers, foojay) | Downloads from a host that is not a Maven repository. Unix JDKs ship as tar.gz, which means new crates. Today a pinned JDK that is not installed is an error listing the ones that are (SPEC §7.1) |
| A wrapper that fetches the pinned jrs (Gradle's `gradlew`) | jrs would download and run executables. Today `project.jrs-version` (SPEC §4.3) makes an older jrs stop and name the version the project needs |
| Extra source sets and test suites (`integrationTest`, `java-test-fixtures`), multi-release jars | SPEC §3's layout is `main` plus `test`. A second test suite with its own dependencies and its own `jrs test` selection is Gradle's JVM Test Suite plugin; multi-release jars need a source root per Java release |
| Build variants (Gradle `-P` properties, Maven profiles) | The manifest is one fixed configuration. Conditional configuration is the start of a DSL |
| PGP signature verification (Gradle's dependency verification) | An OpenPGP crate and a trust store. `jrs.lock` already pins a checksum for every non-snapshot jar, which covers the "bytes changed under the same version" case |
| A compiler daemon (the Gradle daemon) | Non-goal: SPEC §1.2 rules out compiler daemons. kotlinc and scalac add a second or so of JVM start-up to a changed build ([JVM_LANGUAGES.md §14.1](specs/JVM_LANGUAGES.md#14-open-questions)); a daemon would hide it, and the Gradle daemon does so for `javac` too, by running it in-process. The cost is jrs managing a long-lived process, worth it only if real projects feel the start-up |
| A Build Server Protocol server | A long-lived JSON-RPC process that IntelliJ, Metals and VS Code can talk to. `jrs metadata` (SPEC §5.4) is the first step and needs no daemon |
| Annotation-processor path in the manifest | Non-goal: no annotation-processor configuration. Processors on the compile classpath, as `compile-only` dependencies with `-proc:full`, work today. Error Prone, NullAway and other `javac` plugins need a processor path too |
| JPMS (`module-info.java`, module path) | Non-goal |
| JVM languages beyond Kotlin, Scala and Groovy; Kotlin Multiplatform | Non-goal (SPEC §1.2) |
| Kotlin compiler plugins (`allopen`, `spring`, `serialization`), kapt/KSP | Non-goal: compiler-plugin configuration ([JVM_LANGUAGES.md §14.2](specs/JVM_LANGUAGES.md#14-open-questions)). Kotlin on Spring needs `allopen`, so this is the first to revisit: a Spring Boot build in Kotlin migrates (`spring-boot-kotlin`), but does not build as Spring expects |
| Dokka | A plugin host with its own configuration (JVM_LANGUAGES.md §14.4). `jrs doc` runs Scaladoc and Groovydoc, but a Kotlin unit's Kotlin sources are still left out of its `javadoc`, with a warning |
| sbt-style `%%` cross-version keys | New key syntax, touching `edit.rs`, the lockfile and migration (JVM_LANGUAGES.md §14.3) |
| TestNG | SPEC §13.7: the JUnit Platform only (Jupiter and Vintage) |
| Version ranges | SPEC §8.2 rejects them rather than guessing |
| Highest-wins mediation (opt-in) | SPEC §13.6 chose nearest-wins. It is Gradle's default, so migrated Gradle builds can resolve to different versions; the migration report could flag where the two strategies disagree |
| Gradle Module Metadata (`.module` files) | Its rich versions (`strictly`, `prefer`, `reject`, ranges), dependency constraints and capabilities do not fit nearest-wins and the no-ranges rule (SPEC §8.2); honouring them means a different resolver. Reading `.module` files also needs a JSON parser |
