# jrs — Design Specification

Working design document for `jrs`, a Java build system written in Rust.
It expands the capability list from [README.md](README.md) into a concrete scope,
so that implementation can start from agreed contracts instead of ad-hoc decisions.

Status: **draft** — nothing here is implemented yet (`src/main.rs` is still the
Cargo template). Every section is open to change; see [Open questions](#12-open-questions).

---

## 1. Goals and non-goals

### 1.1 Goals

- Build, test, run and package a single-module Java project with **zero configuration
  beyond one `jrs.toml` file**.
- Resolve dependencies (including transitive ones) from Maven Central.
- Be fast: parallel compilation and downloads, incremental where cheap to do so.
- Be a **thin, predictable driver over the JDK toolchain** (`javac`, `java`, `jar`),
  not a reimplementation of it.
- Stay small enough to be readable end to end — this is an experiment, and its
  value is in being understandable.

### 1.2 Non-goals

- Plugin systems, custom task graphs, or a build DSL (Groovy/Kotlin/XML).
  Configuration is declarative TOML only.
- Multi-module / aggregator builds (v1 is one module per manifest).
- Publishing artifacts to a repository (`deploy`/`publish`).
- Non-Java JVM languages (Kotlin, Scala, Groovy).
- Android, JPMS module descriptors, annotation-processor configuration,
  code generation, or IDE project file generation.
- Being a drop-in Maven/Gradle replacement, or consuming their build files.

### 1.3 Success criterion

A developer can clone a Java project containing only `src/` and `jrs.toml`,
run `jrs run`, and get a working program with dependencies downloaded and
linked — with no JDK-specific flags typed by hand.

---

## 2. Terminology

| Term | Meaning |
| --- | --- |
| **Manifest** | `jrs.toml` at the project root; the single source of build configuration. |
| **Module** | One manifest + its source tree. v1 supports exactly one. |
| **Coordinate** | Maven GAV triple: `group:artifact:version`. |
| **Resolution** | Turning declared coordinates into a flat, deduplicated set of jars. |
| **Classpath** | Ordered list of jars + class directories passed to `javac`/`java`. |
| **Cache** | Local store of downloaded artifacts, shared across projects. |
| **Target dir** | Per-project build output directory (`target/`). |

---

## 3. Project layout

Convention over configuration, Maven-like but simplified:

```
my-project/
├── jrs.toml                  # manifest
├── jrs.lock                  # generated: resolved dependency graph
├── src/
│   ├── main/
│   │   ├── java/             # production sources (*.java)
│   │   └── resources/        # copied verbatim into the jar
│   └── test/
│       ├── java/             # test sources
│       └── resources/
└── target/                   # generated, git-ignored
    ├── classes/              # compiled main classes
    ├── test-classes/         # compiled test classes
    ├── deps/                 # symlinks/refs to resolved jars (optional)
    └── my-project-1.0.0.jar  # packaged artifact
```

Rules:

- Package structure mirrors directory structure (standard Java rule); `jrs`
  does not validate it — `javac` already does.
- `src/main/resources/**` is copied into `target/classes/` before packaging,
  preserving relative paths.
- `target/` is fully disposable; deleting it must never lose user data.
- Source roots are overridable in the manifest for projects with a flat `src/`
  layout (see §4.2).

---

## 4. Manifest format (`jrs.toml`)

### 4.1 Full example

```toml
[project]
name = "my-app"
version = "1.0.0"
main-class = "com.example.Main"     # required for `jrs run` and fat jars

[java]
source = 21                          # -> javac --release 21
target = 21                          # optional; defaults to `source`
encoding = "UTF-8"                   # default
javac-args = ["-Xlint:all", "-Werror"]

[dependencies]
# short form: version string
"com.google.guava:guava" = "33.0.0-jre"
# long form: table
"org.apache.commons:commons-lang3" = { version = "3.14.0" }

[dev-dependencies]
# available only on the test classpath
"org.junit.jupiter:junit-jupiter" = "5.10.2"

[repositories]
# optional; Maven Central is implicit and always last
central = "https://repo1.maven.org/maven2"
```

### 4.2 Field reference

| Key | Required | Default | Notes |
| --- | --- | --- | --- |
| `project.name` | yes | — | Used for the jar file name. Must be a valid file name. |
| `project.version` | yes | — | Free-form string; used in the jar name. |
| `project.main-class` | no | — | Fully-qualified class name. Required by `run` and `package --fat`. |
| `project.source-dir` | no | `src/main/java` | Override for flat layouts. |
| `project.test-dir` | no | `src/test/java` | |
| `project.resource-dir` | no | `src/main/resources` | |
| `project.target-dir` | no | `target` | |
| `java.source` | no | detected JDK | Passed as `--release`. |
| `java.target` | no | `java.source` | Only used when it differs from `source`. |
| `java.encoding` | no | `UTF-8` | |
| `java.javac-args` | no | `[]` | Appended verbatim, after jrs-generated flags. |
| `dependencies.*` | no | `{}` | Key is `group:artifact`, value is a version or table. |
| `dev-dependencies.*` | no | `{}` | Test classpath only; never packaged. |
| `repositories.*` | no | Central | Name → base URL. |

### 4.3 Validation

Parsing failures must name the offending key and, where the TOML parser
supplies it, the line/column. Unknown keys are a **warning**, not an error,
so that manifests stay forward-compatible.

### 4.4 Lockfile (`jrs.lock`)

Generated by resolution, committed by the user. Records every resolved
coordinate with its exact version and a SHA-1/SHA-256 checksum, so builds are
reproducible and offline-capable. Regenerated when `jrs.toml` changes or when
`jrs update` is run. Format: TOML, `[[package]]` array (Cargo-like).

---

## 5. CLI surface

```
jrs <command> [options]
```

| Command | Behaviour |
| --- | --- |
| `jrs build` | Resolve → compile main sources → copy resources. |
| `jrs test` | `build` + compile test sources + run the test engine. |
| `jrs run [-- args...]` | `build` + `java -cp <cp> <main-class> args...`. |
| `jrs package` | `build` + produce `target/<name>-<version>.jar`. |
| `jrs package --fat` | Same, but with all runtime dependencies unpacked into the jar. |
| `jrs clean` | Remove `target/`. |
| `jrs tree` | Print the resolved dependency graph. |
| `jrs update` | Re-resolve and rewrite `jrs.lock`. |
| `jrs init` | Scaffold `jrs.toml` + `src/main/java/Main.java`. |

Global flags:

| Flag | Effect |
| --- | --- |
| `-v, --verbose` | Echo every subprocess command line and its exit status. |
| `-q, --quiet` | Errors only. |
| `--offline` | Fail rather than hit the network; use cache + lockfile only. |
| `--jobs <n>` | Cap parallelism; defaults to available cores. |
| `--manifest-path <p>` | Run against a manifest outside the CWD. |

Exit codes: `0` success, `1` build/test failure, `2` usage or manifest error,
`101` internal error (panic).

---

## 6. Architecture

Single binary, library-first: all logic lives in `src/lib.rs` modules so it can
be unit-tested without spawning the CLI. `main.rs` is argument parsing plus a
call into the library.

```
src/
├── main.rs           # CLI entry; arg parsing, exit codes
├── lib.rs            # public API surface for tests
├── cli.rs            # command definitions and dispatch
├── manifest.rs       # jrs.toml parsing + validation + defaults
├── lockfile.rs       # jrs.lock read/write
├── project.rs        # layout discovery, source globbing, target dir mgmt
├── toolchain.rs      # locate javac/java/jar (JAVA_HOME, PATH), version probe
├── compile.rs        # javac invocation, argfile generation, staleness check
├── resolve/
│   ├── mod.rs        # resolution algorithm, conflict mediation
│   ├── coord.rs      # GAV parsing, comparison, version ordering
│   ├── pom.rs        # POM XML parsing: deps, parent, properties, dependencyMgmt
│   ├── repo.rs       # HTTP fetch, URL layout, checksum verification
│   └── cache.rs      # local artifact store
├── package.rs        # jar creation, MANIFEST.MF, fat-jar merging
├── runner.rs         # `java` invocation for run + test
├── test.rs           # test discovery and engine launch
└── error.rs          # error types, user-facing formatting
```

### 6.1 Data flow

```
jrs.toml ──parse──► Manifest
                       │
                       ├──► Project (layout, source file list)
                       │
                       └──► Resolver ──► jrs.lock ──► Classpath
                                              │
Project + Classpath ──► Compiler ──► target/classes/
                                              │
                                              ├──► Packager ──► *.jar
                                              ├──► Runner   ──► java
                                              └──► Tester   ──► junit
```

### 6.2 Key design decisions

- **Shell out to the JDK.** `javac`, `java` and `jar` are located once and
  invoked as subprocesses. No JNI, no bundled compiler.
- **Argfiles over long command lines.** Source lists and classpaths are written
  to `target/.jrs/javac.args` and passed as `@argfile`, sidestepping OS
  command-line length limits.
- **Errors are values.** A single `JrsError` enum with `thiserror`; the CLI
  layer decides how to render it. No `unwrap()` outside tests.
- **Toolchain output is passed through verbatim.** `javac` diagnostics are
  already good; jrs does not reformat them.

---

## 7. Build pipeline

### 7.1 Toolchain discovery

1. `JAVA_HOME/bin/javac` if `JAVA_HOME` is set.
2. Otherwise `javac` from `PATH`.
3. Probe `javac -version`; fail with an actionable message if absent or older
   than the manifest's `java.source`.

Result is cached for the process lifetime.

### 7.2 Compilation

- Glob `**/*.java` under the source root.
- Compute a staleness check: recompile everything if any source is newer than
  the newest `.class` in `target/classes/`, or if the classpath changed.
  (v1 is coarse-grained, all-or-nothing; per-file incremental compilation is
  explicitly out of scope — `javac` needs the full source set for correctness
  anyway when types are interdependent.)
- Invoke:
  ```
  javac --release <n> -encoding <enc> -d target/classes \
        -cp <resolved classpath> <javac-args> @sources.args
  ```
- Non-zero exit → surface `javac` stderr and exit `1`.

### 7.3 Resource handling

Copy `src/main/resources/**` into `target/classes/`, preserving structure,
skipping files whose mtime and size match the destination.

---

## 8. Dependency resolution

### 8.1 Repository layout

For `group:artifact:version`, the artifact URL is:

```
<repo>/<group with . → />/<artifact>/<version>/<artifact>-<version>.jar
<repo>/<group with . → />/<artifact>/<version>/<artifact>-<version>.pom
```

Both are fetched; the POM drives transitive resolution.

### 8.2 Algorithm

1. Seed a work queue with the manifest's direct dependencies.
2. For each coordinate: fetch the POM (cache first), parse it.
3. Interpolate `${...}` properties; walk `<parent>` chains and apply
   `<dependencyManagement>` to fill in missing versions.
4. Collect `<dependencies>` with scope in {`compile`, `runtime`} — skip
   `provided`, `system`, `test`, and any `<optional>true</optional>` entry.
   Honour `<exclusions>`.
5. Enqueue unseen coordinates; repeat until the queue drains.
6. **Conflict mediation: nearest-wins** (Maven semantics) — the version at the
   shallowest depth from the root wins; ties broken by declaration order.
   Emit a warning naming both versions when they differ.
7. Write `jrs.lock`; build the classpath in stable, deterministic order
   (direct dependencies first, then transitives, each sorted by coordinate).

Cycles in the graph are possible in the wild; the `seen` set makes them
terminate. Version ranges (`[1.0,2.0)`) are **rejected with a clear error** in
v1 rather than silently mishandled.

### 8.3 Caching

- Location: `$XDG_CACHE_HOME/jrs` (Linux), `~/Library/Caches/jrs` (macOS),
  `%LOCALAPPDATA%\jrs\cache` (Windows). Override via `JRS_CACHE_DIR`.
- Layout mirrors the Maven repository path, so the cache is inspectable.
- Every download is checksum-verified against the `.sha1` sibling file;
  mismatch → delete and fail loudly.
- Downloads are written to a temp file and atomically renamed, so an
  interrupted run cannot leave a corrupt jar.
- `--offline` uses the cache exclusively and errors on a miss.

### 8.4 Parallelism

- POM fetches and jar downloads run concurrently, bounded by `--jobs`
  (default: core count, min 4 for network-bound work).
- Resolution is breadth-first by level so each level's fetches batch together.
- Resource copying parallelises trivially; `javac` is invoked once and
  parallelises internally.

---

## 9. Packaging

### 9.1 Thin jar (`jrs package`)

- Zip `target/classes/**` into `target/<name>-<version>.jar`.
- Generate `META-INF/MANIFEST.MF`:
  ```
  Manifest-Version: 1.0
  Created-By: jrs <version>
  Main-Class: <project.main-class>        # if set
  Class-Path: <space-separated dep jars>  # if set
  ```
- Deterministic output: entries sorted, fixed timestamps, so repeated builds
  are byte-identical.

### 9.2 Fat jar (`jrs package --fat`)

- Unpack every runtime dependency jar into a staging dir, then jar the result.
- Merge/conflict rules:
  - `META-INF/MANIFEST.MF` from dependencies is dropped.
  - `META-INF/services/*` entries are **concatenated**, not overwritten —
    getting this wrong silently breaks `ServiceLoader`.
  - Signature files (`META-INF/*.SF`, `*.DSA`, `*.RSA`) are dropped, since the
    merged jar invalidates them.
  - Duplicate classes: first wins, with a warning naming both sources.
- Requires `project.main-class`; error out clearly if it is missing.

---

## 10. Testing

### 10.1 Testing jrs itself

- **Unit tests** per module: manifest parsing, coordinate parsing, POM parsing
  (against checked-in fixture XML), version comparison, conflict mediation,
  classpath ordering.
- **Integration tests** in `tests/`: fixture Java projects under
  `tests/fixtures/`, each built end to end through the library API.
  Network-dependent tests are gated behind a feature flag or run against a
  local file:// repository fixture so CI stays hermetic.
- CI (`.github/workflows/rust.yml`) already runs `cargo build` + `cargo test`;
  it will need a JDK setup step (`actions/setup-java`) before integration tests
  can pass.

### 10.2 Running the user's tests (`jrs test`)

- v1 targets **JUnit 5** via the JUnit Platform Console Launcher, resolved as
  an internal dependency when the user's `dev-dependencies` include
  `junit-jupiter`.
- Compile `src/test/java` against `target/classes` + main classpath +
  dev-dependencies, output to `target/test-classes`.
- Launch:
  ```
  java -cp <test classpath> org.junit.platform.console.ConsoleLauncher \
       --select-class-path target/test-classes --details=tree
  ```
- Pass the launcher's exit code through: non-zero → `jrs test` exits `1`.
- Filtering (`jrs test --filter <pattern>`) maps to `--include-classname`.

---

## 11. Roadmap

Milestones map one-to-one onto the README capability list, ordered so each one
produces something runnable.

### M1 — Skeleton
- CLI scaffolding, `manifest.rs`, `toolchain.rs`, `jrs clean`, `jrs init`.
- No README checkbox yet; unblocks everything below.

### M2 — Compile and package
- ☐ compiling project consisting of multiple `*.java` files
- ☐ compiling project into a single `*.jar` file
- ☐ handling build flags
- ☐ running compiled project

### M3 — Dependencies
- ☐ downloading dependencies provided in the `*.toml` file
- ☐ resolving dependencies available in maven central repository
- ☐ resolving transitive dependencies

### M4 — Tests and distribution
- ☐ executing unit tests
- ☐ creating a "fat jar" with all dependencies included within it

### M5 — Performance
- ☐ parallel execution to make build process faster
- Benchmark against a fixture project with ~20 transitive dependencies;
  target: resolution dominated by network, not by jrs.

Tick the corresponding README boxes as each lands — the README is the
user-facing progress tracker, this document is the design behind it.

---

## 12. Open questions

1. **Dependency crates.** Which to take on? Candidates: `clap` (CLI),
   `serde` + `toml` (manifest), `ureq`/`reqwest` (HTTP), `quick-xml`
   (POM), `zip` (jar), `rayon` (parallelism), `thiserror` (errors),
   `sha1`/`sha2` (checksums). Each one costs compile time and readability;
   the experiment's value argues for a minimal set.
2. **Async or threads?** `rayon` + blocking `ureq` keeps the code simple and
   is likely fast enough given downloads are the bottleneck; `tokio` +
   `reqwest` scales better but colours the whole codebase.
3. **JDK version floor.** `--release` requires JDK 9+. Is JDK 17 a reasonable
   minimum, given the manifest's `edition = "2024"` posture?
4. **Should `jrs.lock` be committed?** Cargo says yes for binaries, no for
   libraries. Java has no such split — proposal: always commit.
5. **Fat-jar shading.** Package relocation (Maven Shade-style) is a real need
   for dependency conflicts but a large chunk of work. Out of scope for v1?
6. **Nearest-wins vs. highest-wins** for version conflicts. Maven does
   nearest, Gradle does highest. Nearest is specified above; highest surprises
   users less in practice. Worth revisiting once real projects exercise it.
7. **Test engines beyond JUnit 5.** JUnit 4 and TestNG are still common —
   pluggable engine selection, or JUnit 5 only?
8. **Windows support.** Path separators (`;` vs `:`) on the classpath and
   `.exe` suffixes on toolchain binaries need handling from the start if it is
   in scope at all.
