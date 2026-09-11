# jrs — Roadmap

Every milestone in [SPEC §12](specs/INITIAL_SPEC.md#12-roadmap) has landed, and so has
most of what this document used to list: Windows and macOS in CI, SNAPSHOT
dependencies, the long dependency form, cache maintenance, JVM arguments, JDK
pinning, watch mode, Javadoc, test reports and selection, JUnit 4, coverage,
the portable layout, runtime images, jar manifest attributes, sources and
Javadoc jars, distribution archives, GraalVM native images, Spring's
registries merged in fat jars, `add`/`remove`/`outdated`, `tree` filters,
`classpath`, `init` templates, shell completions, the M5 benchmark,
user-defined tasks and lifecycle hooks (SPEC §7.6), with literal Gradle
tasks translated by `jrs migrate`, Kotlin, Scala and
Groovy alongside Java (SPEC §7.7), build timings (`--timings`, SPEC §5.3.9),
the `project.jrs-version` minimum (SPEC §4.3), the machine-readable
project model: `jrs metadata` and `jrs fetch --sources` (SPEC §5.4),
runtime-only dependencies, local jar files (SPEC §8.8), repository content
filtering (SPEC §8.7), and, for the test and run JVMs, Java agents from the
resolved graph, an environment and working directory, and `--debug`. So have
the HTML test report, `--rerun-failed`, `--fail-fast`, test retries that
report flaky tests, and coverage minimums.

What is left is below. Much of it closes a gap with Gradle, and those items
name the Gradle feature they answer. jrs is not trying to become Gradle
(SPEC §1.2), so each one is the single-module, declarative version of what a
Gradle build gets from a plugin or a DSL block. Anything that touches a
[non-goal](specs/INITIAL_SPEC.md#12-non-goals) or adds a crate is a spec-level
decision (see the last section) — it needs a spec change before it needs code.

---

## 1. Build and compilation

- **Finer-grained incremental compilation.** The staleness check is
  all-or-nothing by design (SPEC §7.2). Worth revisiting only if large projects
  show `javac` time dominating a no-dependency-change rebuild; the M5 benchmark
  harness is the place to measure it first. A cheaper first step is Gradle's
  compile avoidance, applied to the one boundary a single module has: when a
  main-source change leaves the classes' public API alone, the test sources do
  not need recompiling.
- **Compiler start-up.** kotlinc and scalac add a second or so of JVM start-up
  to a changed build ([JVM_LANGUAGES.md §14.1](specs/JVM_LANGUAGES.md#14-open-questions)).
  A compiler daemon would hide it, at the cost of jrs managing a long-lived
  process; worth it only if real projects feel it. This is the gap the Gradle
  daemon closes, and it does so for `javac` too, by running it in-process.
- **Scaladoc and Groovydoc.** `jrs doc` documents the Java sources only.
  Scaladoc 2 ships in the compiler, Scala 3's is an artifact of its own that
  reads TASTy, and Groovydoc is a small graph. Dokka, for Kotlin, is a plugin
  host with its own configuration (JVM_LANGUAGES.md §14.4).

## 2. Dependencies

- **Managed versions and platforms.** Gradle has `platform()`,
  `enforcedPlatform()` and `constraints { }`, and Maven has
  `<dependencyManagement>`. Today the only way to pin a transitive version in
  jrs is to declare it as a direct dependency. That puts it on the classpath
  for good, even after nothing needs it, and it outranks everything under
  nearest-wins. A `[managed]` table would hold versions that apply only if the
  artifact turns up in the graph. It would also take BOM imports, which
  `absorb_import` in `resolve/pom.rs` already knows how to fold in. A managed
  version beats mediation, as it does in Maven, so the tie-break rules in SPEC
  §8.2 need a line about it. The table feeds `manifest-checksum`, `jrs outdated`
  (a BOM is outdated like any dependency), `jrs tree` (a managed version
  should say so), and `jrs add`, which could then leave the version out. This
  is also the first half of the Spring Boot item in section 7. Once it lands,
  migration only has to translate the BOM. It is a new manifest table, so it
  is a spec decision.

## 3. Tests

- **Java agents outside the graph.** `test.java-agents` and `run.java-agents`
  load the jar the resolved graph pinned, so an agent has to be a dependency.
  One the program never calls, such as the OpenTelemetry agent, belongs off
  the classpath: it would be resolved as a tool, the way JaCoCo is, and
  pinned in `jrs.lock`. That would also let a `--fat` image carry a run
  agent, which today it refuses, since the fat jar has unpacked the agent's
  jar.
- **Parallel test JVMs.** Gradle has `maxParallelForks` and `forkEvery`. jrs
  starts one launcher JVM. Forking several JVMs means splitting the test classes
  among launchers and merging their summaries, XML and coverage data. The live
  counter would then follow several processes, and `--debug` would have several
  JVMs to attach to. Worth it once the section 6 benchmark shows test time
  dominating.

## 4. Packaging

- **Shading.** Package relocation for conflicting dependencies was ruled out
  for v1 (SPEC §13.5). Duplicate classes are reported today; relocation — which
  means rewriting class files' constant pools — is the next step if real
  projects hit it.
- **Obfuscation.** An opt-in step that obfuscates the compiled bytecode and
  bundled resources in the packaged jar to make the shipped artifact harder to
  read back into source — renamed classes, methods and fields, stripped debug
  information — while leaving the program's behaviour untouched: the same
  entry point runs, the same tests pass against the obfuscated jar, and the
  measured `run` timing is unchanged. It is off by default and set from the
  manifest; the plain jar is unaffected. In keeping with jrs being a driver,
  the obfuscator (ProGuard, R8) is resolved from Maven Central as its own
  isolated tool graph and pinned in `jrs.lock`, exactly as the JVM-language
  compilers are (SPEC §7.7), and jrs shells out to it rather than rewriting
  class files itself — obfuscation runs after `package`, over the assembled
  jar, so it composes with the fat-jar merge rules instead of fighting them.
  Reflection and `ServiceLoader` entries survive by keeping the names they
  name; getting that wrong breaks an app silently, so like shading it waits on
  a real need. It adds a manifest key, a `[[tool]]` block and a lockfile entry,
  so it is a spec-level decision (see the last section).

## 5. Editors and tooling

- **Checks and formatting.** Gradle has the Checkstyle, PMD, SpotBugs and
  Spotless plugins. In jrs these are tasks, and they become easy once a task can
  depend on a pinned Java tool (T3, section 7). `jrs init` could then offer a
  `check` task and a `post-compile` hook as a starting point. Error Prone is the
  exception: it is a `javac` plugin, so it waits on the annotation-processor path
  (see the last section).

## 6. Benchmarks against Maven and Gradle

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

## 7. jrs itself

- **Migration fidelity.** Keep growing the `tests/fixtures/migrate/` corpus
  from real-world `pom.xml` and Gradle builds. Every construct that lands in the
  "not migrated" block is a candidate for translation. Classifiers, exclusions,
  `provided`/`compileOnly`, `runtime`/`runtimeOnly`, test-jars, annotation
  processors, Surefire's `argLine`, Gradle's JVM arguments, `environment` and
  `workingDir`, `jar { manifest { attributes } }`, literal `files()` and
  `fileTree()`, repository `content { }` / `exclusiveContent { }` group
  filters, and literal Gradle tasks (`Exec`, `JavaExec`, `dependsOn`-only
  aggregates, and `dependsOn` / `finalizedBy` as hooks) now translate;
  profiles, `system` scope and anything computed still do not. A
  `system`-scoped jar under `${project.basedir}` could now become a local jar.
  Maven's `exec-maven-plugin` is still reported whole; an execution bound to
  `generate-sources` would map cleanly onto a `pre-compile` task
  ([TASKS.md §12](specs/TASKS.md#12-open-questions)). Each item in sections
  2–4 that answers a Gradle construct (`platform()`, `maxParallelForks`)
  should arrive with its migration row and a fixture.
- **Spring Boot projects from Gradle.** Spring Boot is the most common kind of
  Java project, and `jrs migrate` cannot migrate it yet. A `start.spring.io`
  build applies the `org.springframework.boot` and
  `io.spring.dependency-management` plugins and declares its starters with no
  version (`implementation 'org.springframework.boot:spring-boot-starter-web'`).
  The versions come from the `spring-boot-dependencies` BOM that the plugins
  import. The Gradle reader needs `g:a:v`, so every starter is listed as not
  migrated, both plugins get "jrs has no plugin system", and the manifest ends up
  with an empty `[dependencies]`. Getting this working takes three pieces, and
  the second has landed:
  - *Versions from a BOM.* The resolver already folds imported BOMs into a
    POM's managed versions (`absorb_import` in `resolve/pom.rs`), but a
    manifest cannot declare a BOM, and it rejects a dependency with an empty
    version. Migration should read the Boot plugin's version,
    `dependencyManagement { imports { mavenBom '...' } }` and
    `implementation platform('...')` as the BOM to use. There are two ways to
    apply it. The first writes a BOM key into the manifest and lets starters
    stay versionless, so upgrading Boot means editing one line. That is a new
    manifest key, and it touches the lockfile's `manifest-checksum`, `jrs add`
    and `jrs outdated`. It is the managed-versions item in section 2. The
    second writes every version out in full at migrate
    time, which is simpler but means migration fetches the BOM, and migration
    never touches the network today (`migrate/maven.rs`). Either way it needs a spec
    decision. Maven's `spring-boot-starter-parent` has the same problem: it is a
    parent in a repository, so it is reported and its managed versions are lost.
  - *A fat jar that boots* — done. The fat jar merges
    `META-INF/spring.factories` key by key, takes the union of the
    `META-INF/spring/*.imports` files' lines, and concatenates
    `spring.handlers`, `spring.schemas` and `spring.tooling`, the project's own
    copy first (SPEC §9.2). jrs builds a flat fat jar, not Boot's nested
    `bootJar` layout; the proof below is what shows that is enough.
  - *Proof.* Add Groovy and Kotlin DSL fixtures under `tests/fixtures/migrate/`,
    taken unchanged from `start.spring.io`. Add a `network-tests` case that
    migrates one of them, then builds it, runs its tests and runs the packaged
    jar up to a started application context. Kotlin on Spring also needs the
    `allopen` compiler plugin, which is still a non-goal (see the last section).
- **Task tool dependencies (T3).** Java tools from Maven Central —
  `google-java-format`, Checkstyle, Flyway — as a task's own dependencies,
  resolved as a graph separate from the project's and pinned in `jrs.lock`
  ([TASKS.md §8](specs/TASKS.md#8-tool-dependencies-a-later-milestone)). Deferred
  until tasks and hooks have seen real use; it changes the lockfile format.
  Most of what Gradle does with third-party plugins would become a jrs task
  through this item (section 5).

## 8. Needs a spec decision first

These cross a line drawn in SPEC §1.2 or §13 (or the dependency list). They are
listed so the discussion has a home, not because they are planned.

| Idea | What it crosses |
| --- | --- |
| Multi-module builds / workspaces | Non-goal: one module per manifest |
| Composite builds (Gradle's `includeBuild`), dependency substitution with a local checkout | Non-goal: one module per manifest. Without them, the way to try a change to a library in the project that uses it is a `-SNAPSHOT` in `~/.m2` through a `file://` repository |
| Publishing (Gradle's `maven-publish`, `publishToMavenLocal`) | Non-goal: SPEC §1.2 rules out `deploy`/`publish`. Done properly it means generating a POM from the manifest, sources and Javadoc jars (`jrs package --sources --javadoc` writes them), signing, and uploading with credentials. A `jrs install` into `~/.m2` is the smallest version, and it is the one that makes library development across projects bearable without a reactor |
| A build cache (Gradle's local and remote build cache) | New shared state beside the dependency cache, holding compile, test and task outputs keyed by their inputs' hash. The fingerprints already exist; the cache would reuse outputs across branches, checkouts and CI machines |
| JDK auto-provisioning (Gradle's toolchain resolvers, foojay) | Downloads from a host that is not a Maven repository. Unix JDKs ship as tar.gz, which means new crates. Today a pinned JDK that is not installed is an error listing the ones that are (SPEC §7.1) |
| A wrapper that fetches the pinned jrs (Gradle's `gradlew`) | jrs would download and run executables. `project.jrs-version` (SPEC §4.3), which has landed, was the part that needed no spec change: an older jrs stops and names the version the project needs |
| Extra source sets and test suites (`integrationTest`, `java-test-fixtures`), multi-release jars | SPEC §3's layout is `main` plus `test`. A second test suite with its own dependencies and its own `jrs test` selection is Gradle's JVM Test Suite plugin; multi-release jars need a source root per Java release |
| Build variants (Gradle `-P` properties, Maven profiles) | The manifest is one fixed configuration. Conditional configuration is the start of a DSL |
| PGP signature verification (Gradle's dependency verification) | An OpenPGP crate and a trust store. `jrs.lock` already pins a checksum for every non-snapshot jar, which covers the "bytes changed under the same version" case |
| A Build Server Protocol server | A long-lived JSON-RPC process that IntelliJ, Metals and VS Code can talk to. `jrs metadata` (SPEC §5.4), which has landed, is the first step and needs no daemon |
| Annotation-processor path in the manifest | Non-goal: no annotation-processor configuration. Processors on the compile classpath, as `compile-only` dependencies with `-proc:full`, work today. Error Prone, NullAway and other `javac` plugins need a processor path too |
| JPMS (`module-info.java`, module path) | Non-goal |
| JVM languages beyond Kotlin, Scala and Groovy; Kotlin Multiplatform | Non-goal (SPEC §1.2) |
| Kotlin compiler plugins (`allopen`, `spring`, `serialization`), kapt/KSP | Non-goal: compiler-plugin configuration ([JVM_LANGUAGES.md §14.2](specs/JVM_LANGUAGES.md#14-open-questions)). Kotlin on Spring needs `allopen`, so this is the first to revisit |
| sbt-style `%%` cross-version keys | New key syntax, touching `edit.rs`, the lockfile and migration (JVM_LANGUAGES.md §14.3) |
| TestNG | SPEC §13.7: the JUnit Platform only (Jupiter and Vintage) |
| Version ranges | SPEC §8.2 rejects them rather than guessing |
| Highest-wins mediation (opt-in) | SPEC §13.6 chose nearest-wins. It is Gradle's default, so migrated Gradle builds can resolve to different versions; the migration report could flag where the two strategies disagree |
| Gradle Module Metadata (`.module` files) | Its rich versions (`strictly`, `prefer`, `reject`, ranges), dependency constraints and capabilities do not fit nearest-wins and the no-ranges rule (SPEC §8.2); honouring them means a different resolver. Reading `.module` files also needs a JSON parser |
