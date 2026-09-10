# jrs — Roadmap

Every milestone in [SPEC.md §12](SPEC.md#12-roadmap) has landed. This document
is the forward-looking list: known gaps in what jrs does today, and features
worth building next.

Items are grouped by area and, within each group, roughly ordered by value for
effort. Anything that touches a [non-goal](SPEC.md#12-non-goals) or adds a crate
is a spec-level decision (see the last section) — it needs a SPEC.md change
before it needs code.

---

## 1. Correctness gaps

Things jrs currently gets wrong or leaves loose. These come before new features.

- **Stale class files survive a source deletion.** When a `.java` file is
  deleted or renamed, the fingerprint change triggers a recompile, but
  `target/classes/` is not cleared first — the orphaned `.class` stays on the
  classpath and ends up in the packaged jar. When the source *set* changes,
  `compile.rs` should wipe the output directory before invoking `javac`.
- **Deleted resources linger too.** `project::copy_tree` only adds and updates;
  it never prunes files that no longer exist under `src/main/resources/` (or
  `src/test/resources/`), so they keep shipping in the jar until `jrs clean`.
- **Lockfile checksums are recorded but not enforced.** When `jrs.lock` is
  reused and a jar is missing from the cache, the fresh download is verified
  against the repository's `.sha1`/`.sha256` only — never against the checksum
  pinned in the lockfile. Comparing a new download with the locked checksum
  costs nothing (the bytes are already in memory) and makes the lockfile an
  integrity pin rather than a record. A `jrs verify` command could also re-hash
  the cached jars on demand, since doing that on every build is deliberately
  avoided.
- **The test resource directory is not configurable.** It is derived from
  `test-dir`'s parent, and there is no `project.test-resource-dir` key to
  match `resource-dir`.
- **Windows is handled but untested.** Classpath separators, `.exe` suffixes
  and `%LOCALAPPDATA%` are implemented (SPEC §13.8), but CI runs on Linux only.
  Add `windows-latest` and `macos-latest` to the CI matrix.

## 2. Dependency resolution

- **Authenticated repositories.** Private repositories (Nexus, Artifactory,
  GitHub Packages) need basic or bearer auth. Credentials should come from
  environment variables or a user-level config file, never from `jrs.toml`,
  which is committed.
- **HTTP proxies.** `HTTPS_PROXY` / `NO_PROXY` are not honoured, which rules
  jrs out on most corporate networks.
- **Retries.** A transient 5xx or a connection reset fails the build outright.
  A small bounded retry with backoff on idempotent GETs would fix most flaky CI
  runs.
- **Mirrors of Central.** Maven Central is implicit and always tried last, and
  there is no way to replace it with a mirror. Air-gapped and corporate setups
  need a user-level override that redirects Central.
- **SNAPSHOT dependencies.** There is no `maven-metadata.xml` handling, so
  timestamped snapshots published to a remote repository cannot be resolved,
  and nothing re-checks a snapshot once it is cached.
- **A richer long-form dependency table.** The table form accepts only
  `version`. Useful additions:
  - `exclusions = ["group:artifact", ...]`, which the resolver already
    implements for POM-declared exclusions;
  - a compile-only scope (Maven `provided`, Gradle `compileOnly`) for APIs
    supplied at runtime (servlet API, Lombok);
  - `classifier`, for artifacts such as platform-specific natives. Classified
    and non-`jar` transitive dependencies are currently skipped.
- **Cache maintenance.** The shared cache only grows. `jrs cache prune` (drop
  entries no lockfile references, or anything unused for N days) and
  `jrs cache path` would help, especially on CI.
- **Gradle Module Metadata.** Resolution reads POMs only. Libraries that
  publish richer variant information in `.module` files fall back to their POM,
  which is usually, but not always, equivalent.

## 3. Build and compilation

- **Per-project JVM arguments.** There is no way to pass `-Xmx`, `-D` system
  properties or `--enable-preview` to `jrs run` or to the test JVM. A
  `[run] jvm-args` (and `[test] jvm-args`) key would cover it.
- **Annotation processors.** Since JDK 23, `javac` no longer runs processors
  found on the classpath implicitly, so Lombok, MapStruct, Dagger and similar
  need `javac-args = ["-proc:full"]` today. Until there is a proper answer, the
  README should document that workaround. The proper answer is a processor path
  declared separately from the compile classpath. Configuring processors is a
  non-goal, so that needs a spec change (see §8).
- **JDK selection per project.** The toolchain is `JAVA_HOME`, then `PATH`.
  Honouring a project-pinned JDK (for example a `java.home`, or reading
  `.java-version` / `.sdkmanrc`) would make builds more portable across
  machines.
- **Watch mode.** `jrs build --watch` / `jrs test --watch` rebuilding on change.
  A polling loop over the existing sorted directory walk keeps it
  dependency-free.
- **Finer-grained incremental compilation.** The staleness check is
  all-or-nothing by design (SPEC §7.2). Worth revisiting only if large projects
  show `javac` time dominating a no-dependency-change rebuild.
- **Javadoc.** `jrs doc`, driving `javadoc` the same way `javac` is driven, with
  an argfile.

## 4. Testing

- **CI-friendly reports.** Pass `--reports-dir target/test-reports` to the
  console launcher so JUnit XML lands where CI systems pick it up. It is almost
  free.
- **Richer selection.** `--filter` maps to class names only. Tag
  include/exclude (`--include-tag`, `--exclude-tag`) and method-level selection
  are natural next flags.
- **JUnit 4 via the Vintage engine.** JUnit 5 only was a deliberate decision
  (SPEC §13.7), but the Vintage engine runs JUnit 4 tests on the same Platform
  launcher jrs already drives. It could bring JUnit 4 projects in at little cost.
  TestNG remains out of scope.
- **Coverage.** Attaching the JaCoCo agent to the test JVM and writing a report
  under `target/`.

## 5. Packaging and distribution of user projects

- **A portable thin-jar layout.** A thin jar's `Class-Path` points at absolute
  paths in the local cache, so it only runs on the machine that built it.
  Add a layout that copies runtime dependencies into `target/lib/` and writes
  relative `Class-Path` entries, ready to zip and ship.
- **Runtime images.** `jrs package --jlink` / `--jpackage` for a trimmed
  runtime or a native installer. Both are JDK tools, so this stays within
  "driver, not reimplementation".
- **Shading.** Package relocation for conflicting dependencies was ruled out
  for v1 (SPEC §13.5). Duplicate classes are reported today; relocation is the
  next step if real projects hit it.

## 6. CLI and ergonomics

- **`jrs add` / `jrs remove`.** Edit `[dependencies]` from the command line,
  optionally resolving the latest release. Rewriting `jrs.toml` without losing
  comments and ordering needs a format-preserving TOML editor, which means a new
  crate (see §8).
- **`jrs outdated`.** List dependencies with newer releases, read from
  `maven-metadata.xml`. It shares its groundwork with SNAPSHOT support.
- **`jrs tree` filters.** `--why <artifact>` (the inverted path to a
  dependency) and `--depth <n>`. The mediation data is already in the
  resolution.
- **`jrs classpath [--test]`.** Print the resolved classpath to stdout for
  editors, language servers and ad-hoc `java` invocations. This gives most of
  the IDE benefit without generating IDE project files, which is a non-goal.
- **`jrs init` templates.** For example `--lib` (no main class) and a starter
  JUnit test, so `jrs test` works straight after scaffolding.
- **Shell completions.** Generated from the clap definition. This adds
  `clap_complete` (see §8).
- **A user-level config file.** A single place (for example
  `~/.config/jrs/config.toml`) for credentials, mirrors, proxy and a default
  `--jobs`. Several items above depend on it, so it is worth designing once.

## 7. jrs itself

- **Package managers.** Tagged releases publish prebuilt binaries for Linux,
  macOS and Windows. A Homebrew formula (and perhaps Scoop and a `.deb`) would
  make installing and upgrading them a one-liner.
- **The M5 benchmark.** SPEC §12 asks for a benchmark against a fixture with
  about 20 transitive dependencies, run with and without `--progress never`.
  There is no checked-in harness for it yet.
- **Migration fidelity.** Keep growing the `tests/fixtures/migrate/` corpus
  from real-world `pom.xml` and Gradle builds. Every construct that lands in the
  "not migrated" block is a candidate for translation.

## 8. Needs a spec decision first

These cross a line drawn in SPEC §1.2 or §13 (or the dependency list). They are
listed so the discussion has a home, not because they are planned.

| Idea | What it crosses |
| --- | --- |
| Multi-module builds / workspaces | Non-goal: one module per manifest |
| `jrs publish` to a Maven repository | Non-goal: no publishing |
| Annotation-processor path in the manifest | Non-goal: no annotation-processor configuration |
| JPMS (`module-info.java`, module path) | Non-goal |
| Kotlin or other JVM languages | Non-goal |
| Version ranges | SPEC §8.2 rejects them rather than guessing |
| Highest-wins mediation (opt-in) | SPEC §13.6 chose nearest-wins |
| `toml_edit` for `jrs add`, `clap_complete` for completions | SPEC §13.1 minimal crate list |
