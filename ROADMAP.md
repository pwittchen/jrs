# jrs — Roadmap

Every milestone in [SPEC §12](specs/INITIAL_SPEC.md#12-roadmap) has landed, and so has
most of what this document used to list: Windows and macOS in CI, SNAPSHOT
dependencies, the long dependency form, cache maintenance, JVM arguments, JDK
pinning, watch mode, Javadoc, test reports and selection, JUnit 4, coverage,
the portable layout, runtime images, `add`/`remove`/`outdated`, `tree` filters,
`classpath`, `init` templates, shell completions, the M5 benchmark,
user-defined tasks and lifecycle hooks (SPEC §7.6), and Kotlin, Scala and
Groovy alongside Java (SPEC §7.7).

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
- **Build timings.** Gradle has `--profile` and build scans. jrs has only a
  total on the `Finished` line. `jrs build --timings` (and the same flag on
  `test`, `run` and `package`) would report the wall time of each phase:
  resolution, downloads, each step of the compile unit, resources, each task
  and hook, the test JVM, and packaging. The report would print as a table
  after the summary, with a copy under `target/.jrs/`. Phase lines already come
  from `cli.rs`, so the timing belongs to `Session` and the rendering to `ui/`.
  The frozen clock in `tests/output.rs` makes the report easy to snapshot.
  Section 6's benchmark would use the same numbers to explain its totals.
- **Pinning the jrs version.** The Gradle wrapper makes every checkout build
  with the Gradle version the project chose. jrs warns on unknown manifest
  keys, so an older jrs builds a newer manifest after ignoring what it does not
  understand. A `project.jrs-version = "0.9"` minimum, like Cargo's
  `rust-version`, would make an older jrs stop with exit `2`, name the version
  the project needs, and say where releases are. Downloading the right jrs
  itself, as `gradlew` does, is a different matter (see the last section).

## 2. Dependencies

- **Runtime-only dependencies.** Gradle has `runtimeOnly` and Maven has
  `runtime` scope. They cover JDBC drivers, SLF4J bindings and Logback: jars
  the program needs at run time that the sources must not compile against.
  jrs has `compile-only` but not the reverse, and `jrs migrate` maps
  `runtimeOnly` onto `[dependencies]`, so the migrated code can quietly start
  compiling against the driver. Adding `runtime-only = true` beside
  `compile-only` would put the jar on the `run`, test and package classpaths
  but not on `javac`'s. That is a fourth value for the lockfile's `classpath`
  field. It also joins the widening rule in `resolve/mod.rs`, where a package
  reached as both runtime-only and compile widens to compile. `jrs add` would
  get a `--runtime-only` flag, and migration would translate `runtimeOnly` and
  Maven's `runtime` scope exactly.
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
- **Local jar files.** Gradle has `implementation files('libs/driver.jar')` and
  `flatDir`. Some jars are in no repository: a vendor's JDBC driver, or a
  licensed SDK checked into `libs/`. Today they need a `file://` repository in
  Maven layout, with a POM written by hand. A `path = "libs/driver.jar"` long
  form would take the jar as it is, relative to the root, with no transitive
  graph. The lockfile would pin its checksum and record the relative path,
  never an absolute one. Migration would translate `files(...)` and
  `fileTree(dir: 'libs')` when they are literals.
- **Repository content filtering.** Gradle has `content { includeGroup }` and
  `exclusiveContent`. Every coordinate is looked for in every repository, in
  declaration order. An extra public repository listed before Central, such as
  JitPack or a vendor's, is therefore asked for everything and could answer
  for anything. A long form,
  `internal = { url = "...", groups = ["com.acme", "com.acme.*"] }`, would
  confine a repository to the groups it serves, and could confine a group to
  one repository. That closes the dependency-confusion hole and saves a 404 per
  artifact per extra repository. Mirrors (SPEC §8.5) keep applying to the URL.
  Migration would translate Gradle's `content` and `exclusiveContent` blocks.

## 3. Tests

- **Java agents on the test and run JVMs.** Mockito 5 on JDK 21 and later
  attaches its inline mock maker at run time. The JVM warns about it, and
  dynamic agent loading is on its way out (JEP 451). Mockito's documentation
  tells Gradle users to add `-javaagent:` pointing at `mockito-core`'s jar.
  jrs has no way to write that path: it lives in the machine's cache, and
  `jrs.toml` is committed. `test.java-agents = ["org.mockito:mockito-core"]`
  would name agents by coordinate. jrs would look them up in the resolved
  graph, so the agent is the pinned version, and pass them as `-javaagent:`
  ahead of the JaCoCo agent. `run.java-agents` would do the same for `jrs run`
  and the `--jlink` launchers. An agent the graph does not contain, such as
  the OpenTelemetry agent, would need resolving as a tool the way JaCoCo is.
- **Environment for tests and `run`.** Gradle has
  `test { environment(...); systemProperty(...) }` and `run { workingDir }`.
  jrs has `jvm-args`, which covers system properties but not environment
  variables or the working directory. `test.env`, `run.env` and `run.cwd` would
  reuse the tasks' `env` and `cwd` parsing and placeholders (SPEC §7.6), so
  `{target}` and `{project.version}` work there too. The environment is part of
  what a test run depends on, but jrs does not cache test results, so there is
  no fingerprint to extend.
- **An HTML test report.** Gradle writes `build/reports/tests/test/index.html`.
  jrs writes JUnit XML, which CI systems read but people do not, so a failure
  in a large suite means scrolling the launcher's tree. A static page, generated
  from the XML jrs already asks the launcher for, with one row per class,
  failures first and the stack traces folded open, would sit in
  `target/test-reports/index.html`. It would be hand-written HTML with no crate,
  and would be generated even when the run fails, as the coverage report is.
- **Rerunning failures, failing fast, retries.** Gradle has `--fail-fast` and
  the test-retry plugin. `jrs test --rerun-failed` would read the last run's XML
  and select the methods that failed, through the `--select-method` path
  `--method` already uses. `--fail-fast` would stop at the first failure; first
  check which console-launcher versions support it, since jrs drives both the
  1.x and 6.x lines. `test.retries = n` would rerun failed methods up to `n`
  times. A test that passes on a retry would be reported as flaky in the
  summary, not as passed, so a retry cannot hide a flaky test.
- **Parallel test JVMs.** Gradle has `maxParallelForks` and `forkEvery`. jrs
  starts one launcher JVM. Jupiter's in-JVM parallelism already works through
  `test.jvm-args` (`-Djunit.jupiter.execution.parallel.enabled=true`), and the
  README should say so. Forking several JVMs means splitting the test classes
  among launchers and merging their summaries, XML and coverage data. The live
  counter would then follow several processes. Worth it once the section 6
  benchmark shows test time dominating.
- **Coverage thresholds.** Gradle has `jacocoTestCoverageVerification`.
  `test.coverage-minimum = { line = 0.80, branch = 0.70 }` would fail
  `jrs test --coverage` with exit `1` when the totals fall short. jrs already
  reads those totals out of `jacoco.xml` for the summary, so the check is a
  comparison and an error message naming both numbers. Per-package rules can
  wait for someone to ask for them.
- **Debugging.** Gradle has `--debug-jvm`. `jrs run --debug[=port]` and
  `jrs test --debug[=port]` would add
  `-agentlib:jdwp=transport=dt_socket,server=y,suspend=y,address=<port>` ahead
  of `-cp`, default to port 5005, and print where to attach before the JVM
  starts waiting. There is one test JVM, so there is one place to attach, until
  parallel forks arrive.

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
- **Jar manifest attributes.** Gradle has
  `jar { manifest { attributes(...) } }`. jrs writes `Main-Class`,
  `Class-Path` and `Created-By`, and nothing else can be added. Real jars need
  more:
  - `Implementation-Title` and `Implementation-Version`, which
    `Package.getImplementationVersion()` reads.
  - `Automatic-Module-Name`, so a library has a stable module name for
    downstream JPMS users even though jrs itself does not do JPMS.
  - `Premain-Class` and `Agent-Class`, for a project that is an agent.
  - `Add-Opens`, `Enable-Native-Access` and `Launcher-Agent-Class`, for an
    executable jar.

  A `package.manifest` table would add attributes in declaration order, so the
  jar stays byte-identical across builds. It would refuse the attributes jrs
  owns. `Implementation-Version` could default to `project.version`, although
  that changes every existing jar's bytes once.
- **Sources and Javadoc jars.** Gradle has `java { withSourcesJar();
  withJavadocJar() }`. `jrs package --sources` would write
  `target/<name>-<version>-sources.jar` from every main source root, plus the
  tasks' `source-outputs`. `--javadoc` would jar up `target/doc`, running
  `jrs doc` first. Both would use the deterministic jar writer that already
  exists. They are what an IDE needs to show a library's source, and what a
  repository wants beside a jar, so they are a prerequisite for publishing if
  that is ever reconsidered.
- **Distribution archives.** Gradle's `application` plugin has `installDist`
  and `distZip`. The portable layout has `lib/` but no launcher, and the
  `--jlink` image has launchers but ships a whole runtime. `jrs package --dist`
  would combine the portable layout with the `bin/<name>` and `bin/<name>.bat`
  scripts `--jlink` already writes, pointed at `JAVA_HOME` or the `java` on
  `PATH` instead of a bundled runtime. It would zip the result
  deterministically into `target/<name>-<version>.zip`. It would be zip only,
  since tar.gz would add a crate.
- **GraalVM native images.** Gradle has the `org.graalvm.buildtools.native`
  plugin. `native-image` is a tool in a GraalVM JDK's `bin/`, so jrs can drive
  it the way it drives `jlink` (SPEC §9.4). `--native-image` would build
  `target/native/<name>` from the runtime classpath and `project.main-class`,
  and `package.native-image-args` would be passed through. `native-image`
  already reads the reachability metadata that libraries ship under
  `META-INF/native-image/`. The Gradle plugin also downloads the shared
  metadata repository; jrs would not. The toolchain lookup (SPEC §7.1) would
  need to find a GraalVM JDK for the pinned version, and fail with a clear
  message when the JDK in use is not one.

## 5. Editors and tooling

- **A machine-readable project model.** IntelliJ, Eclipse and VS Code import a
  Gradle build through its Tooling API. jrs offers `jrs classpath`, which is
  enough for a shell but not for an editor. `jrs metadata`, like Cargo's
  `cargo metadata`, would print the model as versioned JSON on stdout: the source
  and resource roots per language, the output directories, the three
  classpaths, the JDK home and `--release`, the main class, and the tasks. The
  JSON would be written by hand, since the shape is small and `serde_json` would
  be a new crate. This is not IDE project file generation (a non-goal); it is
  what lets an editor plugin or `jdtls` be written outside jrs. A companion
  `jrs fetch --sources` would download the `-sources.jar` of every dependency,
  which Gradle does on import so that go-to-definition works.
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
  the numbers. Where jrs is slower, the report says so.
- **Where it runs.** Maven and Gradle are not something `cargo test` or `cargo
  bench` can assume, so the harness lives outside the default test run and skips
  a tool that is not installed, as `require_jdk!` does. It adds no crate to jrs.
  A manually triggered CI workflow can regenerate the report on a fixed runner.

## 7. jrs itself

- **Migration fidelity.** Keep growing the `tests/fixtures/migrate/` corpus
  from real-world `pom.xml` and Gradle builds. Every construct that lands in the
  "not migrated" block is a candidate for translation. Classifiers, exclusions,
  `provided`/`compileOnly`, test-jars, annotation processors, Surefire's
  `argLine` and Gradle's JVM arguments now translate; profiles, `system` scope
  and anything computed still do not. An `exec-maven-plugin` execution bound
  to `generate-sources` would map cleanly onto a `pre-compile` task
  ([TASKS.md §12](specs/TASKS.md#12-open-questions)). Each item in sections
  2–4 that answers a Gradle construct (`runtimeOnly`, `platform()`, `files()`,
  `content { }`, `jar { manifest { } }`, `environment`, `maxParallelForks`)
  should arrive with its migration row and a fixture.
- **Spring Boot projects from Gradle.** Spring Boot is the most common kind of
  Java project, and `jrs migrate` cannot migrate it yet. A `start.spring.io`
  build applies the `org.springframework.boot` and
  `io.spring.dependency-management` plugins and declares its starters with no
  version (`implementation 'org.springframework.boot:spring-boot-starter-web'`).
  The versions come from the `spring-boot-dependencies` BOM that the plugins
  import. The Gradle reader needs `g:a:v`, so every starter is listed as not
  migrated, both plugins get "jrs has no plugin system", and the manifest ends up
  with an empty `[dependencies]`. Getting this working takes three pieces:
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
  - *A fat jar that boots.* The merge rules concatenate only
    `META-INF/services/*`. Spring Boot also keeps
    `META-INF/spring.factories`, a properties file whose keys repeat across jars
    with comma-separated values, and
    `META-INF/spring/*.AutoConfiguration.imports`. When those files overwrite
    each other, auto-configuration disappears without an error, the same way a
    lost service file breaks `ServiceLoader`. `spring.handlers` and
    `spring.schemas` need merging too. jrs builds a flat fat jar, not Boot's
    nested `bootJar` layout, and that is fine as long as these files merge.
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
| Publishing (Gradle's `maven-publish`, `publishToMavenLocal`) | Non-goal: SPEC §1.2 rules out `deploy`/`publish`. Done properly it means generating a POM from the manifest, sources and Javadoc jars (section 4), signing, and uploading with credentials. A `jrs install` into `~/.m2` is the smallest version, and it is the one that makes library development across projects bearable without a reactor |
| A build cache (Gradle's local and remote build cache) | New shared state beside the dependency cache, holding compile, test and task outputs keyed by their inputs' hash. The fingerprints already exist; the cache would reuse outputs across branches, checkouts and CI machines |
| JDK auto-provisioning (Gradle's toolchain resolvers, foojay) | Downloads from a host that is not a Maven repository. Unix JDKs ship as tar.gz, which means new crates. Today a pinned JDK that is not installed is an error listing the ones that are (SPEC §7.1) |
| A wrapper that fetches the pinned jrs (Gradle's `gradlew`) | jrs would download and run executables. `project.jrs-version` (section 1) is the part that needs no spec change |
| Extra source sets and test suites (`integrationTest`, `java-test-fixtures`), multi-release jars | SPEC §3's layout is `main` plus `test`. A second test suite with its own dependencies and its own `jrs test` selection is Gradle's JVM Test Suite plugin; multi-release jars need a source root per Java release |
| Build variants (Gradle `-P` properties, Maven profiles) | The manifest is one fixed configuration. Conditional configuration is the start of a DSL |
| PGP signature verification (Gradle's dependency verification) | An OpenPGP crate and a trust store. `jrs.lock` already pins a checksum for every non-snapshot jar, which covers the "bytes changed under the same version" case |
| A Build Server Protocol server | A long-lived JSON-RPC process that IntelliJ, Metals and VS Code can talk to. `jrs metadata` (section 5) is the first step and needs no daemon |
| Annotation-processor path in the manifest | Non-goal: no annotation-processor configuration. Processors on the compile classpath, as `compile-only` dependencies with `-proc:full`, work today. Error Prone, NullAway and other `javac` plugins need a processor path too |
| JPMS (`module-info.java`, module path) | Non-goal |
| JVM languages beyond Kotlin, Scala and Groovy; Kotlin Multiplatform | Non-goal (SPEC §1.2) |
| Kotlin compiler plugins (`allopen`, `spring`, `serialization`), kapt/KSP | Non-goal: compiler-plugin configuration ([JVM_LANGUAGES.md §14.2](specs/JVM_LANGUAGES.md#14-open-questions)). Kotlin on Spring needs `allopen`, so this is the first to revisit |
| sbt-style `%%` cross-version keys | New key syntax, touching `edit.rs`, the lockfile and migration (JVM_LANGUAGES.md §14.3) |
| TestNG | SPEC §13.7: the JUnit Platform only (Jupiter and Vintage) |
| Version ranges | SPEC §8.2 rejects them rather than guessing |
| Highest-wins mediation (opt-in) | SPEC §13.6 chose nearest-wins. It is Gradle's default, so migrated Gradle builds can resolve to different versions; the migration report could flag where the two strategies disagree |
| Gradle Module Metadata (`.module` files) | Its rich versions (`strictly`, `prefer`, `reject`, ranges), dependency constraints and capabilities do not fit nearest-wins and the no-ranges rule (SPEC §8.2); honouring them means a different resolver. Reading `.module` files also needs a JSON parser |
