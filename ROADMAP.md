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

## 1. Tests

- **Java agents outside the graph.** `test.java-agents` and `run.java-agents`
  load the jar the resolved graph pinned, so an agent has to be a dependency.
  One the program never calls, such as the OpenTelemetry agent, belongs off
  the classpath: it would be resolved as a tool, the way JaCoCo is, and
  pinned in `jrs.lock`. That would also let a `--fat` image carry a run
  agent, which today it refuses, since the fat jar has unpacked the agent's
  jar.

## 2. Packaging

- **Shading.** Package relocation for conflicting dependencies was ruled out
  for v1 (SPEC §13.5). Duplicate classes are reported today; relocation — which
  means rewriting class files' constant pools — is the next step if real
  projects hit it.
## 3. Editors and tooling

- **Checks and formatting.** Gradle has the Checkstyle, PMD, SpotBugs and
  Spotless plugins. In jrs these are tasks, and a task can depend on a pinned
  Java tool ([TASKS.md §8](specs/TASKS.md#8-tool-dependencies)), so none of
  them needs installing. What is left is a starting point: `jrs init` could
  offer a `check` task and a `post-compile` hook. Error Prone is the exception:
  it is a `javac` plugin, so it waits on the annotation-processor path (see the
  last section).

## 4. Benchmarks against Maven and Gradle

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

## 5. Needs a spec decision first

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
