# jrs — Design Specification

Working design document for `jrs`, a Java build system written in Rust.
It expands the capability list from [README.md](README.md) into a concrete scope,
so that implementation can start from agreed contracts instead of ad-hoc decisions.

Status: **implemented** — every milestone in the [Roadmap](#12-roadmap) has
landed. The document still describes the design rather than the code, so where
the two differ the code is authoritative; the deliberate divergences are listed
in [§12.1](#121-where-the-implementation-diverges), and the decisions taken on
[Open questions](#13-open-questions) are recorded there.

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
- Being a drop-in Maven/Gradle replacement, or *building* from their build files.
  Reading `pom.xml` / `build.gradle` is confined to the one-shot `jrs migrate`
  command (§11); jrs never treats them as a build input at compile time.

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

### 5.1 Commands

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
| `jrs migrate` | Generate `jrs.toml` from an existing `pom.xml` or Gradle build (§11). |

### 5.2 Global flags

| Flag | Effect |
| --- | --- |
| `-v, --verbose` | Echo every subprocess command line and its exit status. |
| `-q, --quiet` | Errors only. |
| `--offline` | Fail rather than hit the network; use cache + lockfile only. |
| `--jobs <n>` | Cap parallelism; defaults to available cores. |
| `--manifest-path <p>` | Run against a manifest outside the CWD. |
| `--progress <auto\|always\|never>` | Live animated output. `auto` = on when stderr is a TTY. |
| `--color <auto\|always\|never>` | Colour and styling. `auto` = on when stderr is a TTY. |
| `--charset <auto\|unicode\|ascii>` | Glyph set for spinners, bars and trees. |

Exit codes: `0` success, `1` build/test failure, `2` usage or manifest error,
`101` internal error (panic).

### 5.3 Terminal output and progress

A build tool spends most of its time waiting — on the network, on `javac` — and
a static cursor makes a fast tool feel slow. `jrs` therefore ships a real
terminal UI: animated spinners, live progress bars, and ASCII/Unicode art for
the dependency graph and the build summary.

The constraint that keeps this from becoming a liability: **the animation is a
presentation layer over a build that would produce identical results without
it.** It renders to stderr, degrades to plain lines whenever it is not talking
to a human, and never delays the work.

#### 5.3.1 Rules

- **stderr only.** Progress, spinners and banners go to stderr; stdout carries
  only real output (`jrs tree`, `jrs run`'s program output, `--dry-run`
  manifests), so pipes and redirects stay clean.
- **Degrade automatically.** Animation is off when stderr is not a TTY, under
  `--quiet` or `--verbose` (verbose interleaves subprocess output, which would
  fight the live region), when `NO_COLOR` or `TERM=dumb` is set, or when a CI
  environment variable is detected. In that mode each phase prints one
  plain line when it starts and one when it ends — a log, not a canvas.
- **Never corrupt the terminal.** The cursor is hidden while a live region is
  active and restored by a guard that runs on normal exit, on error, on panic
  and on `SIGINT`/`SIGTERM`. Leaving a user with an invisible cursor is a bug
  of the same severity as a wrong classpath.
- **Never cost time.** Workers publish counters into shared state; a single
  render thread ticks at a fixed ~80 ms (12.5 fps) and draws. No worker thread
  ever writes to the terminal. `--progress never` must not change the build's
  wall-clock beyond noise.
- **Never scroll.** The live region is erased before any permanent line is
  written above it, so scrollback contains a clean transcript with no
  half-drawn frames. Lines are truncated to the terminal width — the live
  region must never wrap, since wrapping breaks in-place redraw.
- **Unicode with an ASCII fallback.** Every glyph has an ASCII twin, selected by
  `--charset` or probed from the locale. No build output is Unicode-only.

#### 5.3.2 Phase lines

Cargo-style, right-aligned in 12 columns, verb in bold green:

```
    Resolving 14 dependencies
  Downloading guava-33.0.0-jre.jar
    Compiling my-app v1.0.0 (47 source files)
    Packaging target/my-app-1.0.0.jar
     Finished build in 2.31s
```

#### 5.3.3 Spinners

Indeterminate work (POM resolution, `javac`) gets a spinner suffix, frames
advancing on the render tick:

```
unicode: ⠋ ⠙ ⠹ ⠸ ⠼ ⠴ ⠦ ⠧ ⠇ ⠏
ascii:   | / - \
```

#### 5.3.4 Download bars

Parallel downloads render one bar per in-flight transfer, capped at `--jobs`
lines (and at a third of the terminal height), redrawn in place:

```
  Downloading ⠹ 5/14
  [████████████░░░░░░░░]  61%  guava-33.0.0-jre.jar        1.9/3.1 MB
  [██████░░░░░░░░░░░░░░]  30%  commons-lang3-3.14.0.jar    0.2/0.6 MB
  [████████████████████] 100%  checker-qual-3.42.0.jar     verifying…
```

ASCII fallback uses `[####----]`. Completed transfers collapse into a single
summary line (`Downloaded 14 crates in 1.12s`) rather than leaving 14 dead bars
in the scrollback.

#### 5.3.5 Compilation and tests

`javac` reports no progress, so compilation shows a spinner plus the source
count. Tests do have progress: the runner parses the JUnit launcher's output and
maintains a live counter, with a bar of per-test result marks.

```
    Compiling ⠼ 47 source files
      Testing ⠦ 23/31  ✔✔✔✔✔✔✘✔✔✔✔  (22 passed, 1 failed)
```

ASCII fallback: `ok` / `FAIL`, and `+`/`x` marks.

#### 5.3.6 Trees and summaries

`jrs tree` draws box-drawing characters (`├──`, `└──`, `│`) with an ASCII
fallback (`|--`, `` `-- ``), colouring conflict-mediated versions so the
nearest-wins decision from §8.2 is visible at a glance.

The final summary is a small framed block, printed once:

```
  ┌─ jrs ────────────────────────────────┐
  │  build   ok      47 classes          │
  │  deps    14      3 downloaded        │
  │  jar     my-app-1.0.0.jar   412 KB   │
  │  time    2.31s                       │
  └──────────────────────────────────────┘
```

#### 5.3.7 Banner

`jrs init` and `jrs migrate` open with a small ASCII wordmark — once, TTY only,
never in the plain-output mode:

```
   _
  (_)_ __ ___
  | | '__/ __|
  | | |  \__ \
  |_|_|  |___/   a Java build system in Rust
```

No other command prints it. A banner on every `jrs build` would be charming
exactly twice.

#### 5.3.8 Errors

The live region is torn down before any diagnostic is printed, so `javac`
errors, resolution failures and panics always land in a clean terminal — §6.2's
"toolchain output is passed through verbatim" wins over any animation.

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
├── migrate/
│   ├── mod.rs        # build-system detection, manifest emission, report
│   ├── maven.rs      # pom.xml → Manifest (reuses resolve::pom)
│   └── gradle.rs     # build.gradle[.kts] → Manifest
├── package.rs        # jar creation, MANIFEST.MF, fat-jar merging
├── runner.rs         # `java` invocation for run + test
├── test.rs           # test discovery and engine launch
├── ui/
│   ├── mod.rs        # output mode detection (TTY, NO_COLOR, CI), phase lines
│   ├── render.rs     # render thread, live region, cursor guard
│   ├── progress.rs   # shared counters, spinners, download bars
│   └── glyphs.rs     # unicode/ascii glyph sets, tree drawing, banner
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
- **The UI is a layer, not sprinkled `println!`s.** Build code reports progress
  by updating shared state; only `ui/` touches the terminal. That is what keeps
  the animated and the plain modes (§5.3) the same build, and what makes the
  library testable without a TTY.

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
- **Output tests** render `ui/` into an in-memory buffer with a fixed terminal
  width and a frozen clock, so both the plain transcript and a sequence of
  animation frames can be snapshot-asserted without a TTY. Width truncation and
  the ASCII fallback get their own cases — they are exactly the paths a
  developer on a Unicode terminal never exercises by hand.
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

## 11. Migration from Maven and Gradle

The fastest way to get a project onto `jrs` is not to write `jrs.toml` by hand.
`jrs migrate` reads an existing `pom.xml` or Gradle build and emits an
equivalent manifest.

This is a **one-shot, best-effort translation**, not an ongoing compatibility
layer: it runs once, writes files, prints a report, and is then out of the
picture. The original build files are left untouched, so the project can keep
building with its old tool while the migration is evaluated.

### 11.1 CLI

```
jrs migrate [--from maven|gradle] [--dry-run] [--force] [--path <dir>]
```

| Flag | Effect |
| --- | --- |
| `--from` | Force the source build system; otherwise auto-detected. |
| `--dry-run` | Print the manifest that would be written; touch nothing. |
| `--force` | Overwrite an existing `jrs.toml` (default: refuse). |
| `--path <dir>` | Project root to migrate; defaults to the CWD. |

Detection order: `pom.xml` → Maven; `build.gradle` / `build.gradle.kts` →
Gradle; both present → error asking for `--from`; neither → exit `2` with a
message naming what was looked for.

Exit codes follow §5: `0` when a manifest was written (warnings do not change
this), `2` when detection or parsing fails.

### 11.2 Maven (`pom.xml`)

The POM parser from `resolve/pom.rs` is reused, so property interpolation,
parent chains and `<dependencyManagement>` already work.

| POM element | Manifest target |
| --- | --- |
| `<artifactId>` | `project.name` |
| `<version>` (own, after parent inheritance) | `project.version` |
| `maven.compiler.release` / `.source` / `<release>` | `java.source` |
| `maven.compiler.target` | `java.target` |
| `project.build.sourceEncoding` | `java.encoding` |
| `<sourceDirectory>`, `<testSourceDirectory>`, `<resources><directory>` | `project.source-dir`, `test-dir`, `resource-dir` — only when non-default |
| `<build><directory>` | `project.target-dir` |
| `<dependencies>` scope `compile`/`runtime` | `[dependencies]` |
| `<dependencies>` scope `test` | `[dev-dependencies]` |
| `<repositories>` | `[repositories]` |
| `maven-jar-plugin` → `<mainClass>`, or `maven-shade-plugin`'s transformer | `project.main-class` |

Reported, not translated:

- `<modules>` — multi-module builds are out of scope (§1.2). jrs migrates the
  module it was pointed at and lists the others so they can be migrated
  individually.
- `provided`/`system` scopes, `<optional>`, `<classifier>`, `<type>` other
  than `jar`.
- Any plugin other than `maven-compiler-plugin`, `maven-jar-plugin`,
  `maven-surefire-plugin` and `maven-shade-plugin` — each is named in the
  report as unmigrated.
- Profiles: only the default-active ones are read; the rest are listed.

### 11.3 Gradle (`build.gradle`, `build.gradle.kts`)

Gradle build scripts are programs, so parsing them exactly would mean running
Gradle. jrs does not. Instead it does **line-oriented pattern extraction** over
the conventional declarative subset, and is explicit about the fact:

- `dependencies { }` entries of the form
  `implementation 'g:a:v'` / `implementation("g:a:v")` (also `api`,
  `compileOnly`, `runtimeOnly` → `[dependencies]`;
  `testImplementation`, `testRuntimeOnly` → `[dev-dependencies]`).
- `group`, `version`, `rootProject.name` (also read from `settings.gradle`).
- `sourceCompatibility` / `targetCompatibility` /
  `java { toolchain { languageVersion = JavaLanguageVersion.of(n) } }`.
- `application { mainClass = "..." }` / `mainClassName`.
- `repositories { maven { url ... } }`; `mavenCentral()` is implicit.

Anything jrs cannot read confidently is skipped and reported — never guessed:

- Version catalogs (`libs.versions.toml`) are parsed when present, since they
  are declarative; `libs.foo.bar` references are resolved through them.
  Unresolvable aliases become a warning with the reference left in a comment.
- Dependencies built from variables, `ext` blocks, loops or conditionals.
- Custom tasks, plugins, `subprojects { }` / `allprojects { }`,
  and multi-project `settings.gradle` includes (listed, not migrated).

The report opens with a plain statement that Gradle migration is approximate
and the emitted manifest must be reviewed.

### 11.4 Output

1. `jrs.toml` in the project root. Nothing is overwritten without `--force`.
2. A comment header in the generated manifest naming the source file and the
   jrs version that produced it.
3. A migration report on stdout, in three blocks:
   - **Migrated** — what landed in the manifest.
   - **Needs review** — translated but lossy (e.g. a version range clamped to
     its lower bound, a non-default layout).
   - **Not migrated** — plugins, modules and constructs skipped, each with a
     one-line reason.
4. A suggested next step: `jrs build`, then `jrs tree` to compare the resolved
   graph against `mvn dependency:tree` / `gradle dependencies`.

Migration never runs a build, never deletes files, and never writes outside the
project root.

### 11.5 Testing

- Fixture `pom.xml` and `build.gradle[.kts]` files under
  `tests/fixtures/migrate/`, each paired with the expected `jrs.toml`; the test
  asserts an exact match so translation stays reviewable in diffs.
- Round-trip check on a fixture project: migrate, then `jrs build`, and assert
  the resolved classpath matches a checked-in expectation.
- Fixtures deliberately include unsupported constructs, so the "not migrated"
  report is tested too.

---

## 12. Roadmap

Milestones map one-to-one onto the README capability list, ordered so each one
produces something runnable.

### M1 — Skeleton
- CLI scaffolding, `manifest.rs`, `toolchain.rs`, `jrs clean`, `jrs init`.
- `ui/` in its plain form: mode detection, phase lines, the cursor guard.
  Landing the output layer first means no later milestone has to be retrofitted
  away from ad-hoc printing; the animated renderer arrives in M5.
- No README checkbox yet; unblocks everything below.

### M2 — Compile and package
- ☑ compiling project consisting of multiple `*.java` files
- ☑ compiling project into a single `*.jar` file
- ☑ handling build flags
- ☑ running compiled project

### M3 — Dependencies
- ☑ downloading dependencies provided in the `*.toml` file
- ☑ resolving dependencies available in maven central repository
- ☑ resolving transitive dependencies

### M4 — Tests and distribution
- ☑ executing unit tests
- ☑ creating a "fat jar" with all dependencies included within it

### M5 — Performance and polish
- ☑ parallel execution to make build process faster
- ☑ animated progress output: spinners, live download bars, ASCII summary (§5.3)
- Benchmark against a fixture project with ~20 transitive dependencies;
  target: resolution dominated by network, not by jrs. Re-run the benchmark with
  `--progress never` to prove the renderer costs nothing measurable.

### M6 — Migration
- ☑ migrating a project from Maven (`pom.xml`)
- ☑ migrating a project from Gradle (`build.gradle`, `build.gradle.kts`)
- Deliberately last: migration is only useful once every feature it can
  translate into actually works, and it reuses `resolve/pom.rs` from M3.

Tick the corresponding README boxes as each lands — the README is the
user-facing progress tracker, this document is the design behind it.

### 12.1 Where the implementation diverges

Four places where the code deliberately does something other than what is
written above. Each is a smaller deviation than the alternative would have been.

1. **The console launcher's flag is `--scan-class-path`, not
   `--select-class-path`** (§10.2). The latter is not an option the JUnit
   Platform Console Launcher has; the invocation as specified would not run.
   On launchers from 1.10 onwards the `execute` subcommand is passed too,
   since going without it is deprecated and prints a warning.
2. **The live test counter shows a running count, not `23/31`** (§5.3.5). The
   launcher does not announce a total before it starts, and a discovery pass to
   learn one would double JVM startup for a cosmetic gain. The marks bar carries
   the same information; the authoritative totals come from the launcher's own
   summary, which jrs parses and reports on the `Finished` line.
3. **The fat jar is written straight from the dependency jars, with no staging
   directory** (§9.2). The resulting archive is identical — first-wins ordering
   is preserved by reading in classpath order — and a build that packages 40 MB
   of dependencies does not write 40 MB to disk twice.
4. **Phase lines are emitted by the command layer, not by the live scopes.** The
   spec's plain mode "prints one line when a phase starts" (§5.3.1); doing that
   inside `spinner()` would have made the animated and plain transcripts
   diverge. Instead every phase line is unconditional and a live scope adds only
   motion, which is what makes the two modes provably the same build.

---

## 13. Open questions

Answered by the implementation. The questions are kept because the reasoning is
still the interesting part; the **decision** lines record what was settled on.

1. **Dependency crates.** Which to take on? Candidates: `clap` (CLI),
   `serde` + `toml` (manifest), `ureq`/`reqwest` (HTTP), `quick-xml`
   (POM), `zip` (jar), `rayon` (parallelism), `thiserror` (errors),
   `sha1`/`sha2` (checksums), `indicatif` + `console` (progress UI).
   Each one costs compile time and readability;
   the experiment's value argues for a minimal set.
   **Decision:** `clap`, `toml` (with `serde` for its derives), `ureq`,
   `quick-xml`, `zip`, `rayon`, `thiserror`, `sha1`/`sha2`, `terminal_size`,
   and `libc` on unix for the signal handler. No `indicatif`, no `console`,
   no `walkdir` — see 10.
2. **Async or threads?** `rayon` + blocking `ureq` keeps the code simple and
   is likely fast enough given downloads are the bottleneck; `tokio` +
   `reqwest` scales better but colours the whole codebase.
   **Decision:** threads. `rayon` with a pool sized to `--jobs`, and blocking
   `ureq`. Downloads dominate, and nothing in the codebase is `async`.
3. **JDK version floor.** `--release` requires JDK 9+. Is JDK 17 a reasonable
   minimum, given the manifest's `edition = "2024"` posture?
   **Decision:** 17. `toolchain::MINIMUM_JDK` enforces it with an actionable
   error naming the JDK it found.
4. **Should `jrs.lock` be committed?** Cargo says yes for binaries, no for
   libraries. Java has no such split — proposal: always commit.
   **Decision:** always commit. The generated file says so in its header, and
   it records no absolute paths, so it is machine-independent.
5. **Fat-jar shading.** Package relocation (Maven Shade-style) is a real need
   for dependency conflicts but a large chunk of work. Out of scope for v1?
   **Decision:** out of scope. Duplicate classes are reported by name, with
   both sources, rather than relocated.
6. **Nearest-wins vs. highest-wins** for version conflicts. Maven does
   nearest, Gradle does highest. Nearest is specified above; highest surprises
   users less in practice. Worth revisiting once real projects exercise it.
   **Decision:** nearest-wins, as specified, with a warning naming both
   versions and the depth the winner came from.
7. **Test engines beyond JUnit 5.** JUnit 4 and TestNG are still common —
   pluggable engine selection, or JUnit 5 only?
   **Decision:** JUnit 5 only. A project on JUnit 4 gets an error that says so
   rather than a launcher that silently finds no tests.
8. **Windows support.** Path separators (`;` vs `:`) on the classpath and
   `.exe` suffixes on toolchain binaries need handling from the start if it is
   in scope at all.
   **Decision:** in scope, and handled — `Toolchain::classpath_separator`, the
   `.exe` suffix, and `%LOCALAPPDATA%` for the cache. Untested on Windows: CI
   runs on Linux only.
9. **Gradle migration fidelity.** Pattern extraction (§11.3) is honest but
   limited. The alternative — shelling out to `gradle dependencies` and parsing
   its output — is far more accurate, at the cost of requiring a working Gradle
   install and a slow build. Worth offering as an opt-in `--probe` flag?
   **Decision:** no `--probe`. Pattern extraction it is, and the report opens
   by saying the translation is approximate.
10. **Hand-rolled renderer or `indicatif`?** `indicatif` gives multi-bar layout,
    tick threads and terminal-width handling for free; hand-rolling it is maybe
    300 lines of ANSI escapes and is more in the spirit of a build system whose
    value is being readable end to end. The `ui/` boundary (§6.2) means this can
    be decided late and reversed.
    **Decision:** hand-rolled. `ui/render.rs` is the only module that writes an
    escape sequence, and it is about 200 lines.
11. **Should `jrs migrate` offer multi-module output?** A Maven aggregator could
    emit one `jrs.toml` per module. That is a useful escape hatch, but it edges
    towards the multi-module support §1.2 rules out.
    **Decision:** no. `<modules>` and `include` are listed in the report with a
    suggestion to run `jrs migrate --path <module>` on each.
