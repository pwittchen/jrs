# jrs — Roadmap

Every milestone in [SPEC §12](specs/INITIAL_SPEC.md#12-roadmap) has landed, and so has
most of what this document used to list: Windows and macOS in CI, SNAPSHOT
dependencies, the long dependency form, cache maintenance, JVM arguments, JDK
pinning, watch mode, Javadoc, test reports and selection, JUnit 4, coverage,
the portable layout, runtime images, `add`/`remove`/`outdated`, `tree` filters,
`classpath`, `init` templates, shell completions, the M5 benchmark,
user-defined tasks and lifecycle hooks (SPEC §7.6), and Kotlin, Scala and
Groovy alongside Java (SPEC §7.7).

What is left is below. Anything that touches a [non-goal](specs/INITIAL_SPEC.md#12-non-goals)
or adds a crate is a spec-level decision (see the last section) — it needs a
spec change before it needs code.

---

## 1. Build and compilation

- **Finer-grained incremental compilation.** The staleness check is
  all-or-nothing by design (SPEC §7.2). Worth revisiting only if large projects
  show `javac` time dominating a no-dependency-change rebuild; the M5 benchmark
  harness is the place to measure it first.
- **Compiler start-up.** kotlinc and scalac add a second or so of JVM start-up
  to a changed build ([JVM_LANGUAGES.md §14.1](specs/JVM_LANGUAGES.md#14-open-questions)).
  A compiler daemon would hide it, at the cost of jrs managing a long-lived
  process; worth it only if real projects feel it.
- **Scaladoc and Groovydoc.** `jrs doc` documents the Java sources only.
  Scaladoc 2 ships in the compiler, Scala 3's is an artifact of its own that
  reads TASTy, and Groovydoc is a small graph. Dokka, for Kotlin, is a plugin
  host with its own configuration (JVM_LANGUAGES.md §14.4).

## 2. Packaging

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

## 3. jrs itself

- **Migration fidelity.** Keep growing the `tests/fixtures/migrate/` corpus
  from real-world `pom.xml` and Gradle builds. Every construct that lands in the
  "not migrated" block is a candidate for translation. Classifiers, exclusions,
  `provided`/`compileOnly`, test-jars, annotation processors, Surefire's
  `argLine` and Gradle's JVM arguments now translate; profiles, `system` scope
  and anything computed still do not. An `exec-maven-plugin` execution bound
  to `generate-sources` would map cleanly onto a `pre-compile` task
  ([TASKS.md §12](specs/TASKS.md#12-open-questions)).
- **Task tool dependencies (T3).** Java tools from Maven Central —
  `google-java-format`, Checkstyle, Flyway — as a task's own dependencies,
  resolved as a graph separate from the project's and pinned in `jrs.lock`
  ([TASKS.md §8](specs/TASKS.md#8-tool-dependencies-a-later-milestone)). Deferred
  until tasks and hooks have seen real use; it changes the lockfile format.

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
  the numbers. Where jrs is slower, the report says so.
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
| Annotation-processor path in the manifest | Non-goal: no annotation-processor configuration. Processors on the compile classpath, as `compile-only` dependencies with `-proc:full`, work today |
| JPMS (`module-info.java`, module path) | Non-goal |
| JVM languages beyond Kotlin, Scala and Groovy; Kotlin Multiplatform | Non-goal (SPEC §1.2) |
| Kotlin compiler plugins (`allopen`, `spring`, `serialization`), kapt/KSP | Non-goal: compiler-plugin configuration ([JVM_LANGUAGES.md §14.2](specs/JVM_LANGUAGES.md#14-open-questions)). Kotlin on Spring needs `allopen`, so this is the first to revisit |
| sbt-style `%%` cross-version keys | New key syntax, touching `edit.rs`, the lockfile and migration (JVM_LANGUAGES.md §14.3) |
| TestNG | SPEC §13.7: the JUnit Platform only (Jupiter and Vintage) |
| Version ranges | SPEC §8.2 rejects them rather than guessing |
| Highest-wins mediation (opt-in) | SPEC §13.6 chose nearest-wins |
| Gradle Module Metadata (`.module` files) | Its rich versions (`strictly`, `prefer`, `reject`, ranges), dependency constraints and capabilities do not fit nearest-wins and the no-ranges rule (SPEC §8.2); honouring them means a different resolver. Reading `.module` files also needs a JSON parser |
