# jrs — Roadmap

Every milestone in [SPEC §12](specs/INITIAL_SPEC.md#12-roadmap) has landed, and so has
most of what this document used to list: Windows and macOS in CI, SNAPSHOT
dependencies, the long dependency form, cache maintenance, JVM arguments, JDK
pinning, watch mode, Javadoc, test reports and selection, JUnit 4, coverage,
the portable layout, runtime images, `add`/`remove`/`outdated`, `tree` filters,
`classpath`, `init` templates, shell completions, the M5 benchmark, and
user-defined tasks and lifecycle hooks (SPEC §7.6).

What is left is below. Anything that touches a [non-goal](specs/INITIAL_SPEC.md#12-non-goals)
or adds a crate is a spec-level decision (see the last section) — it needs a
spec change before it needs code.

---

## 1. Build and compilation

- **Finer-grained incremental compilation.** The staleness check is
  all-or-nothing by design (SPEC §7.2). Worth revisiting only if large projects
  show `javac` time dominating a no-dependency-change rebuild; the M5 benchmark
  harness is the place to measure it first.

## 2. Packaging

- **Shading.** Package relocation for conflicting dependencies was ruled out
  for v1 (SPEC §13.5). Duplicate classes are reported today; relocation — which
  means rewriting class files' constant pools — is the next step if real
  projects hit it.

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

## 4. Needs a spec decision first

These cross a line drawn in SPEC §1.2 or §13 (or the dependency list). They are
listed so the discussion has a home, not because they are planned.

| Idea | What it crosses |
| --- | --- |
| Multi-module builds / workspaces | Non-goal: one module per manifest |
| Annotation-processor path in the manifest | Non-goal: no annotation-processor configuration. Processors on the compile classpath, as `compile-only` dependencies with `-proc:full`, work today |
| JPMS (`module-info.java`, module path) | Non-goal |
| Kotlin or other JVM languages | Non-goal |
| TestNG | SPEC §13.7: the JUnit Platform only (Jupiter and Vintage) |
| Version ranges | SPEC §8.2 rejects them rather than guessing |
| Highest-wins mediation (opt-in) | SPEC §13.6 chose nearest-wins |
| Gradle Module Metadata (`.module` files) | Its rich versions (`strictly`, `prefer`, `reject`, ranges), dependency constraints and capabilities do not fit nearest-wins and the no-ranges rule (SPEC §8.2); honouring them means a different resolver. Reading `.module` files also needs a JSON parser |
