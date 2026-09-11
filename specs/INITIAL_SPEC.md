# jrs — Design Specification

Working design document for `jrs`, a Java build system written in Rust.
It expands the capability list from [README.md](../README.md) into a concrete scope,
so that implementation can start from agreed contracts instead of ad-hoc decisions.

Status: **implemented** — every milestone in the [Roadmap](#12-roadmap) has
landed, except the tool dependencies M7 defers and the documentation tools M8
defers. The document still describes the design rather than the code, so where
the two differ the code is authoritative; the deliberate divergences are listed
in [§12.1](#121-where-the-implementation-diverges), and the decisions taken on
[Open questions](#13-open-questions) are recorded there.

---

## 1. Goals and non-goals

### 1.1 Goals

- Build, test, run and package a single-module Java project with **zero configuration
  beyond one `jrs.toml` file**. Kotlin, Scala and Groovy sources compile
  alongside the Java ones (§7.7), but Java is the default and the JDK is the
  toolchain: jrs stays a Java build system.
- Resolve dependencies (including transitive ones) from Maven Central.
- Be fast: parallel compilation and downloads, incremental where cheap to do so.
- Be a **thin, predictable driver over the JDK toolchain** (`javac`, `java`, `jar`),
  not a reimplementation of it.
- Stay small enough to be readable end to end — this is an experiment, and its
  value is in being understandable.

### 1.2 Non-goals

- Plugin systems or a build DSL (Groovy/Kotlin/XML). Configuration is
  declarative TOML only. User-defined tasks (§7.6) are subprocesses jrs
  launches at fixed lifecycle points; they cannot replace, remove or
  reorder the built-in phases, and no user code runs inside jrs.
- Multi-module / aggregator builds (v1 is one module per manifest).
- Publishing artifacts to a repository (`deploy`/`publish`).
- JVM languages beyond Java, Kotlin, Scala and Groovy; and for those three,
  anything off the JVM (Multiplatform, JS, Native, Android), compiler
  plugins, kapt/KSP, incremental compilation and compiler daemons (§7.7).
- Android, JPMS module descriptors, annotation-processor configuration,
  built-in code generators, or IDE project file generation. A generator can
  run as a task (§7.6); jrs does not ship one.
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
│   │   ├── kotlin/           # with [kotlin]: *.kt (likewise scala/, groovy/; §7.7)
│   │   └── resources/        # copied verbatim into the jar
│   └── test/
│       ├── java/             # test sources
│       ├── kotlin/
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
jdk = 21                             # optional; build with JDK 21 (§7.1)

[run]
jvm-args = ["-Xmx512m"]              # `java` flags for `jrs run`

[test]
jvm-args = ["-Dmode=test"]           # `java` flags for the test JVM

[kotlin]                             # optional: Kotlin alongside Java (§7.7)
version = "2.4.20"                   # the compiler, and the implied kotlin-stdlib

[managed]
# a version for wherever an artifact turns up in the graph (§8.9)
"com.fasterxml.jackson.core:jackson-databind" = "2.17.2"
# a BOM: its <dependencyManagement> versions apply the same way
"org.springframework.boot:spring-boot-dependencies" = { version = "3.3.4", bom = true }

[dependencies]
# short form: version string
"com.google.guava:guava" = "33.0.0-jre"
# no version: [managed] gives it one
"org.springframework.boot:spring-boot-starter-web" = {}
# long form: table
"org.apache.commons:commons-lang3" = { version = "3.14.0" }
"jakarta.servlet:jakarta.servlet-api" = { version = "6.0.0", compile-only = true }
"org.postgresql:postgresql" = { version = "42.7.3", runtime-only = true }
"io.netty:netty-handler" = { version = "4.1.100.Final", exclusions = ["io.netty:netty-codec"] }
# a classifier: in the key, or as `classifier = "..."` in the table
"org.lwjgl:lwjgl:natives-linux" = "3.3.3"
# a jar kept in the project: no coordinate, no transitive graph (§8.8)
ojdbc = { path = "libs/ojdbc11.jar" }

[dev-dependencies]
# available only on the test classpath
"org.junit.jupiter:junit-jupiter" = "5.10.2"

[repositories]
# optional; Maven Central is implicit and always last
# long form: asked only for these groups, and the only one asked for them (§8.7)
internal = { url = "https://nexus.example.com/maven", groups = ["com.acme", "com.acme.*"] }
central = "https://repo1.maven.org/maven2"

[tasks.build-info]
# a user-defined task (§7.6): here a Java file run with the project's JDK
script = "build/GenerateBuildInfo.java"
args = ["{target}/generated/sources", "{project.version}"]
inputs = ["build/GenerateBuildInfo.java"]
outputs = ["{target}/generated/sources"]
source-outputs = ["{target}/generated/sources"]   # compiled with the main sources

[tasks.checksum]
script = "build/Checksum.java"
args = ["{jar}"]

[tasks.format]
# a Java tool from Maven Central, resolved and pinned as the task's own graph
main = "com.google.googlejavaformat.java.Main"
args = ["--replace", "src/main/java/com/example/Main.java"]

[tasks.format.dependencies]
"com.google.googlejavaformat:google-java-format" = "1.22.0"

[hooks]
pre-compile = ["build-info"]
post-package = ["checksum"]
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
| `project.test-resource-dir` | no | beside `test-dir` | `src/test/resources` for the default `test-dir`. |
| `project.target-dir` | no | `target` | |
| `project.jrs-version` | no | — | The oldest jrs that may build the project, as `MAJOR.MINOR` or `MAJOR.MINOR.PATCH` (§4.3). |
| `java.source` | no | detected JDK | Passed as `--release`. |
| `java.target` | no | `java.source` | Only used when it differs from `source`. |
| `java.encoding` | no | `UTF-8` | |
| `java.javac-args` | no | `[]` | Appended verbatim, after jrs-generated flags. |
| `java.javadoc-args` | no | `[]` | Appended verbatim to `jrs doc`'s `javadoc` (§7.4). |
| `java.jdk` | no | — | JDK feature version to build with (§7.1). |
| `run.jvm-args` | no | `[]` | `java` flags for `jrs run`, before `-cp`. |
| `run.java-agents` | no | `[]` | `group:artifact` of agents on the runtime classpath, passed as `-javaagent:<jar>` ahead of `run.jvm-args`, from the jars `jrs.lock` pins; baked into `--jlink` / `--jpackage` / `--dist` launchers (§9.4, §9.6). |
| `run.env` | no | `{}` | Environment variables for the program, name → string. A task's placeholders (§7.6), except `{jar}` and `{classpath-argfile}`; `JRS_*` names are refused. |
| `run.cwd` | no | jrs's own | The program's working directory, relative to the root; placeholders as `run.env`, but no classpath. |
| `test.jvm-args` | no | `[]` | `java` flags for the test JVM. |
| `test.jacoco-version` | no | jrs's default | JaCoCo release for `jrs test --coverage` (§10.2). |
| `test.java-agents` | no | `[]` | As `run.java-agents`, looked up on the test classpath (dev-dependencies included), ahead of JaCoCo's agent (§10.2). |
| `test.env` | no | `{}` | As `run.env`, for the test JVM. |
| `test.retries` | no | `0` | Run a failed test again up to this many times (§10.2). One that passes on a retry is reported as flaky, not as passed. `jrs test --retries <n>` overrides it. |
| `test.forks` | no | `1` | Test JVMs a run's classes are split among, run at once (§10.2): Gradle's `maxParallelForks`, surefire's `<forkCount>`. `jrs test --forks <n>` overrides it. |
| `test.coverage-minimum` | no | `{}` | Ratios from 0 to 1 that `jrs test --coverage` must reach, per JaCoCo counter: `instruction`, `branch`, `line`, `complexity`, `method`, `class` — e.g. `{ line = 0.80, branch = 0.70 }` (§10.2). Ignored without `--coverage`. |
| `package.add-modules` | no | `[]` | Modules a runtime image needs beyond what `jdeps` finds (§9.4). |
| `package.manifest.*` | no | `{}` | Extra `MANIFEST.MF` main attributes, written after jrs's own in declaration order (§9.1). Values may use `{project.name}` and `{project.version}`; `Main-Class`, `Class-Path`, `Created-By`, `Manifest-Version` and `Name` are jrs's and refused. |
| `package.native-image-args` | no | `[]` | Passed through verbatim to `native-image` by `--native-image` (§9.7). |
| `kotlin.*`, `scala.*`, `groovy.*` | no | — | The table turns the language on (§7.7): `version` (required, exact), `source-dir` / `test-dir` (`src/main/<lang>` / `src/test/<lang>`), `kotlinc-args` / `scalac-args` / `groovyc-args`, `compiler-jvm-args`. |
| `dependencies.*` | no | `{}` | Key is `group:artifact` or `group:artifact:classifier`; value is a version, or a table with `version` and optionally `classifier`, `exclusions` (`group:artifact` patterns, `*` allowed), and `compile-only` or `runtime-only`. A table without `version`, `{}` at its shortest, takes the version `[managed]` gives it (§8.9). A table with `path` instead is a local jar: the key is a name, the path is relative to the project root, and only `compile-only` / `runtime-only` go with it (§8.8). |
| `dev-dependencies.*` | no | `{}` | Test classpath only; never packaged. Same forms, without `compile-only` or `runtime-only`. |
| `managed.*` | no | `{}` | `group:artifact` → a version the artifact is held to wherever it turns up in the graph, or `{ version, bom = true }`, a BOM whose managed versions apply the same way (§8.9). |
| `repositories.*` | no | Central | Name → base URL, or `{ url, groups }` to confine the repository to those groups (§8.7). |
| `tasks.<name>.*` | no | `{}` | A user-defined task: one action (`run`, `shell`, `script` or `main`) or none, plus `description`, `args`, `depends-on`, `env`, `cwd`, `inputs`, `outputs`, `source-outputs`, `resource-outputs`, and `[tasks.<name>.dependencies]` for a `main` or `script` (§7.6). |
| `hooks.*` | no | `{}` | Lifecycle point (`pre-compile`, `post-compile`, `pre-test`, `post-test`, `post-package`, `pre-run`) → list of task names (§7.6). |

### 4.3 Validation

Parsing failures must name the offending key and, where the TOML parser
supplies it, the line/column. Unknown keys are a **warning**, not an error,
so that manifests stay forward-compatible.

Forward compatibility has a limit: a jrs that ignores what it does not
understand can build a newer manifest wrongly. `project.jrs-version`, like
Cargo's `rust-version`, names the oldest jrs that may build the project:
`"0.9"` or `"0.9.1"`, anything else being an error naming the key. It is
checked as soon as `[project]` is found, before any other key is read, so an
older jrs stops with exit `2` — naming the version the project needs, its own
version, and where releases are — instead of warning about keys it does not
know or failing on a form it cannot read. A pre-release suffix on the running
version is ignored (`0.9.0-dev` satisfies `0.9`). `jrs init` and `jrs migrate`
do not write the key. Fetching the right jrs, as `gradlew` does, is not part
of it.

### 4.4 Lockfile (`jrs.lock`)

Generated by resolution, committed by the user. Records every resolved
coordinate with its exact version and a SHA-1/SHA-256 checksum, so builds are
reproducible and offline-capable. Regenerated when `jrs.toml` changes or when
`jrs update` is run. Format: TOML, `[[package]]` array (Cargo-like).

The checksums are pins, not only a record: a jar downloaded while the lockfile
is in use must match its recorded checksum as well as the repository's, or the
download is discarded and the build fails (§8.3). A `-SNAPSHOT` is republished
under one name by design, so it is recorded without a checksum.

A project with Kotlin, Scala or Groovy pins each compiler's graph as well, in
a `[[tool]]` block after the `[[package]]` entries: a `name`
(`kotlin-compiler`), its `roots`, and `[[tool.package]]` entries in the same
format. A compiler decides the bytecode, so an unpinned one would not be
reproducible (§7.7). The file says `version = 2` only when it has `[[tool]]`
blocks, so a Java project's lockfile stays byte-identical, and a jrs that
predates tools refuses a version 2 file rather than drop the pins when it
rewrites it. The implied runtime libraries are ordinary `[[package]]` entries,
and `manifest-checksum` covers them and each language's version. A task with
`[tasks.<name>.dependencies]` pins its graph the same way, in a `[[tool]]`
block named `tasks.<name>` (§7.6), and `manifest-checksum` covers those
dependencies too.

A package whose version `[managed]` decided (§8.9) says `managed = true`. The
line is written only then, and `manifest-checksum` gains `[managed]`'s entries
only when there are some, so a manifest without the table keeps its lockfile
byte for byte.

A `[[package]]`'s `classpath` is `compile`, `provided`, `runtime` or `test`
(§8.2). A local jar (§8.8) is a `[[local]]` block after the packages: its
`name`, its `path` as the manifest writes it, relative to the project root, its
`classpath`, and a `sha256` checksum. It needs no new `version`: a jrs that
predates local jars refuses the manifest's `path` key outright, so it never gets
to rewrite such a lockfile. A lockfile without them is byte-identical to one
written before they existed.

### 4.5 Editing the manifest

`jrs add` and `jrs remove` change the dependency tables without rewriting the
file: comments, order, quoting and line endings survive. The editor is
line-based. It knows what jrs itself writes — one entry per line, a string or
an inline table — and refuses anything else, with an error saying to edit the
file by hand. That covers sub-table declarations, dotted keys and values
spanning lines. The result must parse back as a manifest before it is written.
If the new graph does not resolve, the original file is restored.

---

## 5. CLI surface

```
jrs <command> [options]
```

### 5.1 Commands

| Command | Behaviour |
| --- | --- |
| `jrs build [--watch]` | Resolve → compile main sources → copy resources. `--watch` repeats on every change (§7.5). |
| `jrs test [--debug[=port]]` | `build` + compile test sources + run the test engine (§10.2). `--rerun-failed` runs only what failed last time, `--fail-fast` stops at the first failure, `--retries <n>` overrides `test.retries`, `--forks <n>` overrides `test.forks`. |
| `jrs run [--debug[=port]] [-- args...]` | `build` + `java [<jdwp>] [<run.java-agents>] <run.jvm-args> -cp <cp> <main-class> args...`, in `run.cwd` with `run.env`. |
| `jrs package` | `build` + produce `target/<name>-<version>.jar`. |
| `jrs package --portable` | Same, with the runtime dependencies in `target/lib/` (§9.3). |
| `jrs package --fat` | Same, but with all runtime dependencies unpacked into the jar. |
| `jrs package --jlink` / `--jpackage [type]` | Also a runtime image or a native package (§9.4). |
| `jrs package --sources` / `--javadoc` | Also `target/<name>-<version>-sources.jar` / `-javadoc.jar` (§9.5). |
| `jrs package --dist` | Also a distribution with launch scripts, zipped into `target/<name>-<version>.zip` (§9.6). |
| `jrs package --native-image` | Also a GraalVM native executable in `target/native` (§9.7). |
| `jrs doc` | Generate Javadoc into `target/doc` (§7.4). |
| `jrs clean` | Remove `target/`. |
| `jrs tree [--depth n] [--why artifact] [--tool name] [--task name]` | Print the resolved dependency graph, `n` levels deep, inverted from one artifact to the manifest, a compiler's own graph (§7.7), or a task's (§7.6). |
| `jrs classpath [--test \| --runtime]` | Print the resolved classpath to stdout. |
| `jrs update` | Re-resolve and rewrite `jrs.lock`; re-check every cached snapshot. |
| `jrs verify` | Re-hash the cached jars against the checksums in `jrs.lock`; exit `1` on a mismatch. |
| `jrs outdated` | List declared dependencies and `[managed]` entries with newer releases, from `maven-metadata.xml`. |
| `jrs add` / `jrs remove` | Edit `[dependencies]` / `[dev-dependencies]` in place, then re-resolve (§4.5). `jrs add` leaves the version out of what `[managed]` covers (§8.9). |
| `jrs cache path` / `jrs cache prune` | Show or prune the shared cache (§8.6). |
| `jrs init [--lib] [--lang java\|kotlin\|scala\|groovy]` | Scaffold `jrs.toml`, a starter class and a starter test: JUnit 5 for Java and Kotlin, MUnit for Scala, and for Groovy Java code with Spock specs (§7.7). |
| `jrs migrate` | Generate `jrs.toml` from an existing `pom.xml` or Gradle build (§11). |
| `jrs completions <shell>` | Print a bash, zsh or fish completion script. |
| `jrs task <name> [-- args...]` | Run a user-defined task and whatever it depends on (§7.6). |
| `jrs task <name> --watch` | The same, repeated on every change to the task's inputs, the manifest or the source trees (§7.5). |
| `jrs task --list` | List the tasks, their descriptions and the hooks that run them, to stdout. |
| `--timings` on `build`, `test`, `run`, `package` | Report the wall time of each phase after the summary, with a copy in `target/.jrs/timings.txt` (§5.3.9). |
| `jrs metadata [--no-deps]` | Print the project model as versioned JSON on stdout, for editors and tools; resolves but never compiles (§5.4). |
| `jrs fetch [--sources]` | Resolve and download the dependencies, the compilers, the tasks' tools and the test launcher into the cache without building; `--sources` adds each dependency's `-sources.jar` (§5.4). |

`--debug[=[host:]port]` on `run` and `test` puts
`-agentlib:jdwp=transport=dt_socket,server=y,suspend=y,address=<port>` ahead of
every other JVM argument, on port 5005 unless given one, and prints a
`Debugging` phase line naming where to attach before the JVM starts waiting. A
bare port is the JDK's localhost-only reading of one; `*:<port>` listens on
every interface. The value is checked while the arguments are parsed, so a bad
port is a usage error (exit `2`). The test JVM runs without the live counter
while it waits.

### 5.2 Global flags

| Flag | Effect |
| --- | --- |
| `-v, --verbose` | Echo every subprocess command line and its exit status. |
| `-q, --quiet` | Errors only. |
| `--offline` | Fail rather than hit the network; use cache + lockfile only. |
| `--jobs <n>` | Cap parallelism; defaults to the user config's `jobs` (§8.5), else available cores. |
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
  only real output (`jrs tree`, `jrs run`'s program output, the task
  `jrs task` names, `--dry-run` manifests), so pipes and redirects stay clean.
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
        Fresh build-info (task)
    Compiling my-app v1.0.0 (47 source files)
    Packaging target/my-app-1.0.0.jar
         Task checksum (post-package)
     Finished package in 2.31s
```

A task prints `Task <name>`, with the hook that ran it in parentheses, or
`Fresh <name> (task)` when its up-to-date check lets it be skipped (§7.6).

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

#### 5.3.9 Timings

`--timings` on `build`, `test`, `run` and `package` adds a table after the
summary with the wall time of each phase and its share of the total:

```
  phase                           time   share
  resolution (jrs.lock)            4ms    0.1%
  downloads                      1.12s   15.5%
  task build-info (pre-compile)  310ms    4.3%
  compile main: kotlinc          3.02s   41.9%
  compile main: javac            640ms    8.9%
  resources main                   3ms    0.0%
  test JVM                       1.89s   26.2%
  total                          7.21s
```

The rows, in the order the phases ran: `resolution`, or `resolution
(jrs.lock)` when the lockfile answered; `downloads` for the project's jars,
`downloads (<Language> compiler)` for each compiler's and `downloads (test
tools)` for the launcher and JaCoCo; one row per step of a compile unit
(`compile main: kotlinc`, `compile test: javac`), or `compile main (fresh)`
for the staleness check of an up-to-date one; `resources main` and `resources
test`; `task <name> (<hook>)` for each task, with `fresh` when its check let
it be skipped; `test JVM`; `coverage report`; `packaging`; `jdeps`, `jlink`
and `jpackage`. Only leaf phases are rows, so they never overlap, and what the
total has beyond their sum is jrs's own bookkeeping.

The timing is `Session`'s and the table is `ui`'s, so it is the same in the
plain and animated modes (a dim header and a bold total when coloured). `-q`
prints none of it, but the copy is written in every mode, `-q` included:
`target/.jrs/timings.txt`, a comment line naming the command, then
tab-separated `phase` and `ms` columns and a `total` row, for scripts and the
benchmark. `jrs run` reports before the program starts, since the program's
own run is not a build phase. `jrs test` reports for a failing run too; a
command that fails before its summary reports nothing.

### 5.4 The project model (`jrs metadata`) and `jrs fetch`

`jrs metadata`, like `cargo metadata`, prints the project as JSON on stdout —
what an editor plugin or a language server needs, without reading `jrs.toml`
itself. It resolves the graph as a build would (from `jrs.lock` when it
matches) but never compiles. `--no-deps` skips resolution: `classpaths` is
`null`, and nothing is downloaded or written. The JSON is written by hand
(`json.rs`; `serde_json` would be a new crate), pretty-printed, with keys in
the order below and every path absolute. `version` is the format version: it
goes up only when a key changes meaning or goes away, and new keys may appear
within a version.

```
{
  "version": 1,
  "jrs": "0.4.0",                          the running jrs
  "project": {
    "name": "my-app",
    "version": "1.0.0",
    "main-class": "com.example.Main",      or null
    "jrs-version": "0.9",                  or null
    "root": "/work/my-app",
    "manifest": "/work/my-app/jrs.toml",
    "lockfile": "/work/my-app/jrs.lock"
  },
  "jdk": {                                 null when no JDK is found
    "home": "/usr/lib/jvm/jdk-21",         or null
    "version": 21,
    "javac": "/usr/lib/jvm/jdk-21/bin/javac",
    "java": "/usr/lib/jvm/jdk-21/bin/java"
  },
  "java": {
    "release": 21,                         what javac --release gets; or null
    "target": null,
    "encoding": "UTF-8",
    "javac-args": []
  },
  "languages": [
    { "name": "java", "version": null, "compiler": null },
    { "name": "kotlin", "version": "2.4.20",
      "compiler": "org.jetbrains.kotlin:kotlin-compiler-embeddable:2.4.20" }
  ],
  "main": {
    "sources": [
      { "language": "java", "dir": "/work/my-app/src/main/java" },
      { "language": "kotlin", "dir": "/work/my-app/src/main/kotlin" }
    ],
    "generated-sources": ["/work/my-app/target/generated/sources"],
    "resources": ["/work/my-app/src/main/resources"],
    "generated-resources": [],
    "output": "/work/my-app/target/classes"
  },
  "test": { the same keys, "output": "/work/my-app/target/test-classes" },
  "target-dir": "/work/my-app/target",
  "jar": "/work/my-app/target/my-app-1.0.0.jar",
  "classpaths": {                          null with --no-deps
    "compile": [
      { "coordinate": "com.google.guava:guava:33.0.0-jre",
        "jar": "<cache>/com/google/guava/guava/33.0.0-jre/guava-33.0.0-jre.jar",
        "sources": "<cache>/…/guava-33.0.0-jre-sources.jar" }
    ],
    "runtime": [ … ],
    "test": [ … ]
  },
  "tasks": [
    { "name": "build-info", "description": "Generate BuildInfo", "hooks": ["pre-compile"] }
  ]
}
```

- `generated-sources` and `generated-resources` are the `source-outputs` and
  `resource-outputs` of the tasks the unit's hook reaches (§7.6): known from
  the manifest, whether or not the tasks have run.
- The three classpaths come from the functions the build uses, in the build's
  order (direct dependencies first, §8.2): `compile` is what `javac` gets for
  the main sources, `runtime` what `jrs run` and the jars get, `test` what the
  test JVM gets. They hold jars only; the class directories are
  `main.output` and `test.output`, which jrs puts first. A local jar (§8.8)
  has no coordinate, so its `coordinate` is `null`.
- `sources` is the dependency's `-sources.jar` when it is in the cache, and
  `null` otherwise. A classified artifact (natives, a platform build) points
  at its main artifact's sources.

`jrs fetch` resolves and downloads everything a build and a test run need —
the dependencies, the compilers (§7.7), each task's own tools (§7.6) and the
JUnit launcher when there is a JUnit to derive it from — without building, so
a CI job can warm the cache and
run `--offline` after it. `--sources` also downloads the `-sources.jar`
(classifier `sources`) of every dependency, under the usual `Downloading` line
and bars, which is what an editor needs for go-to-definition. They are cached
in the Maven layout beside the jars, checked against the repository's
checksum like any download, and not pinned in `jrs.lock`. A sources jar no
repository has is a warning, not an error: many artifacts publish none, and
nothing a build does needs one.

---

## 6. Architecture

Single binary, library-first: all logic lives in `src/lib.rs` modules so it can
be unit-tested without spawning the CLI. `main.rs` is five lines: a call into
`cli::main()`, which parses the arguments, and an exit with its code.

```
src/
├── main.rs           # five-line entry point: exit(cli::main())
├── lib.rs            # public API surface for tests
├── cli.rs            # command definitions and dispatch
├── completions.rs    # bash/zsh/fish scripts, from the clap definition
├── config.rs         # the per-user config file (§8.5)
├── manifest.rs       # jrs.toml parsing + validation + defaults
├── edit.rs           # format-preserving edits to the dependency tables (§4.5)
├── lockfile.rs       # jrs.lock read/write
├── project.rs        # layout discovery, source globbing, target dir mgmt
├── toolchain.rs      # locate the JDK (pin, JAVA_HOME, PATH), version probe
├── compile/
│   ├── mod.rs        # compile units and their steps, fingerprint, staleness, argfiles
│   ├── javac.rs      # javac and javadoc invocation
│   ├── incremental.rs # file-by-file compilation of a Java unit (§7.2)
│   └── lang.rs       # Kotlin, Scala, Groovy: compilers, runtime libraries, flags (§7.7)
├── image.rs          # jdeps, jlink, jpackage (§9.4)
├── resolve/
│   ├── mod.rs        # resolution algorithm, conflict mediation
│   ├── coord.rs      # GAV parsing, comparison, version ordering
│   ├── pom.rs        # POM XML parsing: deps, parent, properties, dependencyMgmt
│   ├── metadata.rs   # maven-metadata.xml: versions, snapshot builds
│   ├── repo.rs       # HTTP fetch, URL layout, checksum verification
│   └── cache.rs      # local artifact store, pruning
├── migrate/
│   ├── mod.rs        # build-system detection, manifest emission, report
│   ├── maven.rs      # pom.xml → Manifest (reuses resolve::pom)
│   └── gradle.rs     # build.gradle[.kts] → Manifest
├── package.rs        # jar creation, MANIFEST.MF, fat-jar merging
├── runner.rs         # `java` invocation for run + test
├── test.rs           # test discovery and engine launch
├── task.rs           # user tasks: plan, cycles, placeholders, env, fingerprints (§7.6)
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
  to `target/.jrs/javac-main.args` (and `javac-test.args`) and passed as
  `@argfile`, sidestepping OS command-line length limits.
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

A project may pin a JDK feature version. It is a version, not a path, since a
path belongs to one machine and the manifest is committed. The pin comes from
`java.jdk`, else `.java-version` (jenv, asdf, mise), else `.sdkmanrc`. A pinned
version is looked for in this order:
1. The user config's `[jdks]` table (§8.5).
2. The JDK above, if it is that version.
3. The JDKs installed in the usual places:
   - SDKMAN!, asdf, mise, IntelliJ and Gradle toolchain directories
   - `/Library/Java/JavaVirtualMachines`, `/usr/lib/jvm` and `Program Files`
   - the `JAVA_HOME_<n>_<arch>` variables CI setups export

Installed JDKs are identified by their `release` file, so looking costs reads
and not JVM starts. The newest build of the pinned version wins. None found →
an error naming the versions that were.

Other JDK tools (`javadoc`, `jdeps`, `jlink`, `jpackage`) are taken from beside
the selected `javac`, and a JDK that lacks one is an error naming it.
`native-image` is looked for there too (§9.7): a JDK without it is not GraalVM,
and the error says how to build with one that is — `JAVA_HOME`, or `java.jdk`
with its home in the `[jdks]` table. jrs does not look for a GraalVM JDK on its
own.

### 7.2 Compilation

- Glob `**/*.java` under the source root — and with another language on, every
  root for every language's extension (§7.7).
- Compute a staleness check: the unit is stale if a source changed, or if the
  settings did — the flags, the source list, and the classpath, including a
  jar's size or modification time, which is how a snapshot rebuilt under the
  same path is noticed. A source has changed when its contents have: one
  whose size or mtime moved is hashed and compared with the index below, so a
  touched file is not a change. A unit without an index compares the newest
  source with the newest `.class` in `target/classes/`.
- A Java-only unit compiles **file by file** when it can. Every build records
  in `target/.jrs/<unit>.index` which classes each source compiled to, tied
  by their `SourceFile` attribute, with each class's API digest (as below),
  its constants, and the unit's classes it refers to. With the settings
  unchanged, the next build deletes the changed sources' classes and runs
  `javac` over those sources alone, with `target/classes` first on `-cp`. If
  the API of a class they compile to changed, a second run compiles every
  source that refers to one of those classes, and every source that refers
  to one of theirs, transitively, since a subclass passes an inherited change
  on to its own users. A change that stays inside method bodies compiles one
  source. The classes are the ones a whole build would write, byte for byte.
- The whole unit is compiled instead, into an emptied `target/classes/`, when
  file by file cannot be sure. That means changed settings or no index; a
  source added or deleted, or a top-level class that appeared or went away,
  since a simple name elsewhere may now mean a different class; a changed
  compile-time constant, which `javac` inlines without a reference back; a
  class not tied to exactly one source; annotation processors or `javac`
  plugins (a `-processor*` or `-Xplugin` flag, or a processor registered on
  the classpath or in `target/classes` without `-proc:none`); a
  `module-info.java`; and any unit with Kotlin, Scala or Groovy. Emptying
  the directory first means a class whose source was deleted or renamed
  cannot survive onto the classpath or into the jar.
- A failed `javac` run leaves the index saying that every source it was given
  must be compiled again, and the next build deletes every class no
  unchanged source owns before it compiles.
- The test unit compiles against `target/classes`, and counts it by its API,
  not its bytes (compile avoidance): its fingerprint holds a digest of what
  `javac` can see of those classes from another unit. That is each class's
  flags, supertypes and nesting, and every non-private member's name,
  descriptor, generic signature, annotations, exceptions and constant value,
  since `javac` inlines constants. Method bodies, private members, and local
  and anonymous classes are left out. A main change that alters that API
  recompiles the tests; one that does not leaves them fresh. Kotlin and Scala
  classes count by their bytes, since inline functions and macros carry
  bodies across the boundary, and so does every class once the main classes
  register an annotation processor or a Groovy AST transformation, which run
  while the tests compile.
- Invoke:
  ```
  javac --release <n> -encoding <enc> -d target/classes \
        -cp <resolved classpath> <javac-args> @sources.args
  ```
- Non-zero exit → surface `javac` stderr and exit `1`.
- A unit with Kotlin, Scala or Groovy sources runs that language's compiler
  first, into the same output directory and under the same fingerprint; such
  a unit is all-or-nothing (§7.7).

### 7.3 Resource handling

Copy `src/main/resources/**` into `target/classes/`, preserving structure,
skipping files whose mtime and size match the destination. Test resources go to
`target/test-classes/` the same way.

A resource deleted from the source tree is deleted from the output too. Since
the output directory is shared with `javac`, jrs records the resources it
copied (`target/.jrs/resources-main.list`, `resources-test.list`) and only ever
removes paths from that record — never a class file or a processor's output.

### 7.4 Documentation

`jrs doc` documents the main sources, generated ones included, into
`target/doc`, which is emptied first so a deleted class does not keep its
page. The tool follows the unit's language (§7.7):

| Unit holds | Tool | Reads |
| --- | --- | --- |
| Java, or Java + Kotlin | `javadoc` | the Java sources; Kotlin's are left out with a warning, and the build runs first so that `javadoc` finds their classes |
| Scala 2.13 (+ Java) | Scaladoc, `scala.tools.nsc.ScalaDoc` in the compiler's graph | every source, Java included |
| Scala 3 (+ Java) | `org.scala-lang:scaladoc_3` at `scala.version` | the TASTy in `target/classes`, so the build runs first; Java sources have none and are left out with a warning |
| Groovy (+ Java) | `org.apache.groovy:groovy-groovydoc` at `groovy.version` | every source, Java included, relative to its root |

`javadoc` is driven the way `javac` is: the main sources and the compile
classpath go into `target/.jrs/javadoc.args`, the flags are `--release`, the
encodings and a title, and `java.javadoc-args` is appended verbatim.

Scaladoc and Groovydoc run on the project's JDK as `java
@target/.jrs/<scaladoc|groovydoc>.args`, as the compilers do, with
`compiler-jvm-args` for their JVM; `javadoc-args` is not passed to them. Their
graphs are resolved when `jrs doc` needs them and are not pinned in
`jrs.lock`, like the test launcher's. From Scala 3.9 on, scaladoc's graph gets
`com.fasterxml.jackson.core:jackson-annotations:2.21` as a second root, since
nearest-wins would otherwise give it a version too old to start with
(`JVM_LANGUAGES.md` §9). Groovydoc gets `-javaVersion=JAVA_<n>` for the
release, and no classpath, since it only parses.

Every tool's output is passed through verbatim. Groovydoc skips a source it
cannot parse and still exits 0, so jrs fails the run on its message. A tool
that does not know the release fails with its own message, and jrs adds the
one-line hint it adds for compilers (§7.7).

### 7.5 Watch mode

`jrs build --watch` and `jrs test --watch` run the command, then poll the
manifest, the source trees (every language's roots, §7.7) and the resource
trees every 300 ms. Each poll is a size and
mtime picture over the same sorted walk a build does. They run the command
again once a change has settled. A failure is reported and waited out rather
than ending the loop, since the next save is usually the fix. The manifest is
re-read on every run.

The `inputs` of every task (§7.6) are watched too, and `jrs task <name>
--watch` runs the same loop. Nothing under `project.target-dir` is ever
watched, so a task's output cannot retrigger it.

### 7.6 Tasks and hooks

A task is a named command in `[tasks.<name>]`, run as a subprocess. `[hooks]`
attaches tasks to fixed points in the built-in commands. The built-in phases
cannot be removed, replaced or reordered, and no user code runs inside jrs.

**Actions.** A task has exactly one of:

- `run` — an argument vector, with no shell. The program is looked up on
  `PATH`, the JDK's `bin/` first, or taken relative to the root if it contains
  a path separator. The portable choice.
- `shell` — one string, for `sh -c` on Unix and `cmd /C` on Windows. Not
  portable; `jrs task --list` marks it `(sh)`.
- `script` — a `.java` file, run by the project's JDK in source-launcher mode
  (`java <file> <args…>`). Portable, since it needs only the JDK jrs found.
- `main` — a class from the task's own dependencies, run by the project's JDK
  as `java @target/.jrs/tasks/<name>.tool.args <class> <args…>`, the argfile
  holding `-cp` and their classpath.

A task with no action only runs its `depends-on`; one with neither is a
manifest error. `args` follow the action: appended to `run`, passed to the
`script` or the `main` class, and a `shell` string's positional parameters
(`$1`…). `depends-on` names
tasks or the built-ins `build`, `test`, `package` and `doc`. `env` adds
variables, `cwd` is relative to the root. Task names match `[a-z][a-z0-9-]*`
and may not be a built-in command's name.

**Tool dependencies.** `[tasks.<name>.dependencies]` takes the
`[dependencies]` value forms (§4.2), without `compile-only`, `runtime-only`,
local jars or a version left to `[managed]`. It is the classpath of a `main`
action, and of a `script`, which gets the same argfile ahead of its file; on
any other task it is a manifest error, as is a `main` without it. The
dependencies are resolved as a graph of their own that never meets the
project's, as a compiler's is (§7.7), and pinned in `jrs.lock` as a `[[tool]]`
block named `tasks.<name>` (§4.4). They are downloaded with everything else
when the graph is resolved afresh, so that the lockfile pins their checksums,
and otherwise when the task first runs. `jrs tree --task <name>` prints the
graph, and `jrs cache prune` and `jrs verify` see it as they see the
compilers'.

**Hooks.** Each value is a list of task names; a hook cannot hold a command
inline, so anything it runs can be run alone with `jrs task`.

| Hook | Runs |
| --- | --- |
| `pre-compile` | After dependencies are resolved, before the main sources are globbed and compiled. |
| `post-compile` | After main classes and resources are in `target/classes`. |
| `pre-test` | After the main build, before test sources compile. |
| `post-test` | After the test launcher, only if the tests passed. |
| `post-package` | After the jar, and any image, is written. |
| `pre-run` | After the build, before the program starts. |

Commands include each other, so their hooks do too: `build` fires the two
compile hooks, `test`, `package` and `run` add their own, and `doc` fires
`pre-compile`, since it documents generated sources too. A hook runs every time
its point is reached, whether or not `javac` had work to do; skipping work is
the task's own business. `tree`, `classpath`, `update`, `verify`, `outdated`,
`add`, `remove`, `cache`, `init`, `migrate`, `completions` and `clean` never
run a task. That is a guarantee: inspecting a freshly cloned project is safe.

**Ordering.** Tasks run serially, and each runs at most once per invocation.
So does each built-in: `build` in the `depends-on` of a task hooked into
`jrs test` does not build a second time. `depends-on` entries run in the order
the list names them, depth-first, dependencies before dependents, and a hook's
tasks run in the order the hook lists them. A built-in in `depends-on` runs as
its command would, hooks included, but without that command's `Finished` line
and summary box. `test` runs with no filters, and `package` builds the plain
thin jar.

**Validation**, at parse time (§4.3). Unknown keys in a task or in `[hooks]`
warn. These are errors:

- a `depends-on` or hook entry naming an unknown task;
- a cycle, named in full (`a → b → a`), including one through a built-in, such
  as a `pre-compile` task that depends on `build`;
- `source-outputs` or `resource-outputs` outside `project.target-dir`;
- an `env` name starting with `JRS_`;
- an unknown placeholder, or a classpath placeholder in a path-valued key
  (`cwd`, `inputs`, `outputs`, `source-outputs`, `resource-outputs`);
- `{jar}` in a task reachable from any hook but `post-package`, unless the task
  depends on `package`.

Tasks do not feed `manifest-checksum`, so adding one does not re-resolve;
their `dependencies` do, since those are resolved.

**Placeholders.** `run`, `args`, `cwd`, `env` values, `inputs`, `outputs` and
the `*-outputs` lists are expanded before the process starts. `{{` and `}}` are
literal braces.

| Placeholder | Value |
| --- | --- |
| `{root}`, `{target}` | Project root and `project.target-dir`, absolute. |
| `{project.name}`, `{project.version}` | From `[project]`. |
| `{classes}`, `{test-classes}` | `target/classes`, `target/test-classes`, absolute. |
| `{classpath}`, `{runtime-classpath}`, `{test-classpath}` | Exactly what `jrs classpath`, `jrs classpath --runtime` and `jrs classpath --test` print. `{classpath}` is `target/classes`, then the compile jars. |
| `{classpath-argfile}` | `target/.jrs/tasks/<name>.cp.args`, holding `-cp <compile classpath>`, for `java @{classpath-argfile}`. A long classpath in an argument vector overflows the OS limit. |
| `{jar}` | The packaged jar. |

A classpath placeholder resolves dependencies if nothing else has, lockfile
first; a standalone `jrs task` then resolves but does not compile. `{jar}` in a
task run before `package` has run in the same invocation is a build error
telling the user to add `package` to `depends-on`. `shell` strings get no
placeholders, since the shell already expands `$VAR`; one that mentions
`JRS_CLASSPATH` or `JRS_RUNTIME_CLASSPATH` resolves dependencies the same way.

**Environment.** A task inherits jrs's environment, plus `JAVA_HOME` (the JDK
of §7.1, pin honoured), `PATH` with `$JAVA_HOME/bin` first, `JRS_TASK`,
`JRS_HOOK` (when a hook ran it), `JRS_ROOT`, `JRS_TARGET_DIR`,
`JRS_CLASSES_DIR`, `JRS_PROJECT_NAME`, `JRS_PROJECT_VERSION`,
`JRS_CLASSPATH` and `JRS_RUNTIME_CLASSPATH` (once resolved), `JRS_JAR` (once
packaged), `JRS_OFFLINE=1` under `--offline`, and `SOURCE_DATE_EPOCH=315532800`,
the timestamp jrs's own jars carry. The task's `env` wins over all of these
except the `JRS_` names it may not set.

**Up-to-date checks.** Only a task with both `inputs` and `outputs` can be
skipped. Entries are files or directories relative to the root, a directory
meaning everything under it, walked as sources are; there are no globs. The
fingerprint covers the expanded action (argv or shell string, `args`, `cwd`,
`env`), the JDK version, each input's path, size and mtime, the value of
every classpath placeholder the task uses, with each jar's size and mtime, and
the size and mtime of each jar of the task's own dependencies. It
is written to `target/.jrs/tasks/<name>.fingerprint` after a successful run and
deleted on failure. A task is fresh when the fingerprint matches and every
output exists: it prints `Fresh <name> (task)` and does not run. `jrs clean`
forgets every fingerprint.

**Generated sources and resources.** The `source-outputs` and
`resource-outputs` of tasks reached from `pre-compile` feed the main compile
unit (every `.java` under them joins the source list) and `target/classes`
(synced with the record-keeping of §7.3). Those of tasks reached from
`pre-test` feed the test unit and `target/test-classes`; a task both hooks
reach runs once, in `pre-compile`, and feeds the main unit. On a task no
`pre-compile` or `pre-test` hook reaches they are ignored, with a manifest
warning. They must lie under `project.target-dir`: `jrs clean` must never lose
user data, watch mode must not retrigger on a generator's output, and the
compile fingerprint already covers generated files through the source list.
The "no `.java` files" check runs after `pre-compile`, so a project whose
sources are all generated still builds.

**Running.** `cli.rs` emits the phase line for every task (§5.3.2).

- The task named on `jrs task <name>` inherits the terminal, as `jrs run`'s
  program does, and receives the arguments after `--` (as positional
  parameters, for `shell`). `jrs task` exits with its exit code.
- Tasks run by hooks or as dependencies have stdin closed, and both output
  streams passed through verbatim to stderr, so stdout stays the command's own.
  One that exits non-zero stops the command with a build error naming it:
  exit `1`, after its output.
- A program that cannot be started is a build error naming it and the `PATH`
  that was searched.
- `--verbose` echoes the expanded command, the working directory and the
  `JRS_` variables.
- `jrs task --list` writes to stdout. Completions cover `jrs task` and its
  flags, but not task names: nothing calls back into jrs at completion time.

### 7.7 Other JVM languages

Kotlin, Scala and Groovy compile alongside Java, on the JVM only. The design,
the alternatives, and what the spike before it found are in
[JVM_LANGUAGES.md](JVM_LANGUAGES.md); this is the contract.

**Turning one on.** A `[kotlin]`, `[scala]` or `[groovy]` table turns the
language on and pins its compiler. `version` is required and exact: at least
Kotlin 2.0, Scala 2.13.9 or 3.3, or Groovy 4.0. `source-dir` and `test-dir`
add a root (`src/main/<lang>` and `src/test/<lang>` by default);
`kotlinc-args` / `scalac-args` / `groovyc-args` are appended verbatim, and
`compiler-jvm-args` go to the compiler's JVM.

**Sources.** Every root of a unit — `project.source-dir` and each language's —
is scanned for every extension (`.java`, `.kt`, `.scala`, `.groovy`), so a
`.kt` file under `src/main/java` compiles too. A source in a language that is
off is a manifest error naming the table to add, rather than a file silently
left out. So is a unit with two languages besides Java, since neither
compiler reads the other's sources. Main and test are separate units: Kotlin
main code with Groovy tests is fine.

**The runtime library** — `kotlin-stdlib`; `scala-library` on 2.13,
`scala3-library_3` on 3.0–3.7 and both from 3.8; `org.apache.groovy:groovy` —
is an implied dependency at the compiler's version, resolved as if declared
last in `[dependencies]`, so it wins nearest-wins against transitive copies. A
declaration in either table replaces it: Groovy in `[dev-dependencies]` keeps
it off the runtime classpath. A declaration at another version warns. It is
never written into `jrs.toml`, and `jrs tree` labels it `(implied by [kotlin])`.

**The compiler** is a tool: resolved from the project's repositories as a
graph of its own, never mediated against the project's, pinned in `jrs.lock`
(§4.4), and run on the project's JDK (§7.1). It is
`kotlin-compiler-embeddable`, `scala3-compiler_3` or `scala-compiler`, or
Groovy's core jar, and it is downloaded like the test launcher, under a
`Downloading <artifact> (<Language> compiler)` line.

**A unit's steps** share one output directory, one fingerprint and one
staleness decision (§7.2):

| Unit holds | Step 1 | Step 2 |
| --- | --- | --- |
| Java only | `javac` | — |
| Kotlin or Scala (+ Java) | the compiler, over its own sources and the Java ones, which it reads for their symbols | `javac` over the Java sources, step 1's classes first on `-cp` |
| Groovy (+ Java) | `groovyc -j`, which runs `javac` itself (joint compilation) | — |

The fingerprint covers every step's flags, the compiler's version and jars,
and the sources, and any change reruns every step into an emptied directory.
The phase line counts by language:
`Compiling orders v1.0.0 (2 Kotlin + 1 Java source files)`.

**Invocation.** `java @target/.jrs/<kotlinc|scalac|groovyc>-<unit>.args`. The
one argfile holds the compiler's JVM flags, its classpath, its main class, its
flags and the sources, in the `java` launcher's quoting, which is `javac`'s.
The compilers' own `@file` readers disagree about backslashes and about what a
file may hold, so jrs does not use them. Output is passed through verbatim.

**Flags.** `java.source`, or the JDK's version, is the one release for every
language:

| Language | jrs generates |
| --- | --- |
| Kotlin | `-no-stdlib -no-reflect -jvm-target <n> -Xjdk-release=<n> -module-name <name>`; the tests get `-module-name <name>_test -Xfriend-paths=target/classes`, so they see `internal` declarations |
| Scala | `-encoding <enc>`, and `-java-output-version <n>` on 3 or `-release <n>` on 2.13; `-color:never` on 3 when jrs's output is not coloured |
| Groovy | `--encoding=<enc>` and `-Dgroovy.target.bytecode=<n>` for its JVM; with Java sources, `-j -J=-release=<n>` and `java.javac-args` translated: `-F=<flag>` for a single token, `-J=name=value` or `-F=-name=value` for a known `-name value` pair. An argument the rule cannot place is a manifest error. |

A compiler that does not know the release fails with its own message, and jrs
adds one line: lower `java.source`, or raise the language's version.

**Around the build.** `jrs run` suggests `<main-class>Kt` when that is the
only class of the two. `jrs doc` runs Scaladoc or Groovydoc, and `javadoc` over
a Kotlin unit's Java sources (§7.4). `jrs
outdated` lists each `<lang>.version` against its compiler's releases, `jrs
tree --tool <name>` prints a compiler's graph, `jrs add` warns about a `_2.13`
or `_3` suffix that does not match `[scala]`, and `jrs verify` and `jrs cache
prune` cover the pinned compilers (§8.6). Resolution warns about a Scala 2
library newer than its compiler, two Scala lines' builds of one library, and a
pre-1.8 `kotlin-stdlib-jdk7`/`-jdk8` beside a Kotlin 2 stdlib.

---

## 8. Dependency resolution

### 8.1 Repository layout

For `group:artifact:version`, the artifact URL is:

```
<repo>/<group with . → />/<artifact>/<version>/<artifact>-<version>.jar
<repo>/<group with . → />/<artifact>/<version>/<artifact>-<version>.pom
```

Both are fetched; the POM drives transitive resolution.

A classified artifact is `<artifact>-<version>-<classifier>.jar` in the same
directory, described by the unclassified POM. A `-SNAPSHOT` published to a
remote repository is stored under a timestamped name
(`lib-1.0-20240101.120000-3.jar`). The `maven-metadata.xml` in the version
directory names the current build. Without one, as in a local `~/.m2`, the
plain `-SNAPSHOT` name is used. The `maven-metadata.xml` one level up lists
every published version, which `jrs outdated` and `jrs add` read.

### 8.2 Algorithm

1. Seed a work queue with the manifest's direct dependencies.
2. For each coordinate: fetch the POM (cache first), parse it.
3. Interpolate `${...}` properties; walk `<parent>` chains and apply
   `<dependencyManagement>` to fill in missing versions. Then a version the
   manifest's `[managed]` holds an artifact to (§8.9) replaces whatever the
   POM asked for, wherever in the graph the artifact turns up.
4. Collect `<dependencies>` with scope in {`compile`, `runtime`} — skip
   `provided`, `system`, `test`, and any `<optional>true</optional>` entry.
   Honour `<exclusions>`, and the manifest's own. A `<type>` of `jar`,
   `bundle`, `ejb` or `maven-plugin` is a jar; `test-jar` is the jar
   classified `tests`; `pom` contributes dependencies but no jar; anything
   else is skipped with a warning. A classifier is part of an artifact's
   identity.
5. Enqueue unseen coordinates; repeat until the queue drains.
6. **Conflict mediation: nearest-wins** (Maven semantics) — the version at the
   shallowest depth from the root wins; ties broken by declaration order.
   Emit a warning naming both versions when they differ. A managed version is
   settled before mediation (step 3), so every path to a managed artifact
   asks for the same version and there is nothing to mediate; as in Maven, a
   version the manifest declares still wins for its own dependency, at depth 1.
   Every package also lands on one of four classpaths:
   - compile: needed at runtime too
   - provided: a `compile-only` dependency, and whatever only it brings in
   - runtime: a `runtime-only` dependency, and whatever only it brings in —
     run, packaged and tested with, never compiled against by the main sources
   - test

   A package reached several ways lands everywhere one of them puts it:
   `provided` and `runtime` together make `compile`, and `test` adds nothing
   to either, since tests see every classpath. The test sources compile and run
   against one classpath, so a `runtime-only` jar is on both, as Maven's
   `runtime` scope is. A package widened after its own dependencies were walked
   hands the wider classpath down to them.
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
  mismatch → delete and fail loudly. When the jar is pinned in `jrs.lock`, the
  download must match the pin too. A jar already in the cache is not re-hashed
  on every build; `jrs verify` does that on demand.
- Downloads are written to a temp file and atomically renamed, so an
  interrupted run cannot leave a corrupt jar.
- `--offline` uses the cache exclusively and errors on a miss.
- When a cached snapshot is due, it is checked again against its repository.
  It is due once a day (Maven's default policy), on every build when it came
  from a `file://` repository, and whenever `jrs update` runs. If that
  repository cannot be reached, the cached copy is used, with a warning.
  Identical bytes are not rewritten, so a check that finds nothing new does
  not force a recompile.
- A cached file's last use is recorded in its access time, at most once a
  day, for `jrs cache prune --unused-for` (§8.6). Its modification time, which
  the compile fingerprint reads, is left alone.

### 8.4 Parallelism

- POM fetches and jar downloads run concurrently, bounded by `--jobs`
  (default: core count, min 4 for network-bound work).
- Resolution is breadth-first by level so each level's fetches batch together.
- Resource copying parallelises trivially; `javac` is invoked once and
  parallelises internally.

### 8.5 Network access and user configuration

`jrs.toml` is committed, so nothing that belongs to a person or a machine may
live in it. Those settings go in a per-user file: `$XDG_CONFIG_HOME/jrs/config.toml`
(`~/.config/jrs/config.toml`) on Unix and macOS, `%APPDATA%\jrs\config.toml` on
Windows, or wherever `JRS_CONFIG` points. It is parsed like the manifest —
unknown keys warn, errors name the key — and a missing file means every default.

| Key | Effect |
| --- | --- |
| `jobs` | Default for `--jobs`. |
| `proxy.url`, `proxy.no-proxy` | An explicit HTTP proxy. Without it, `HTTPS_PROXY` / `HTTP_PROXY` / `ALL_PROXY` / `NO_PROXY` apply. |
| `mirrors.<repo>` | Fetch repository `<repo>` (Central included) from another URL; `*` covers every repository without a mirror of its own. `file://` repositories are never mirrored. |
| `credentials.<repo>` | `username` + `password`, or `token`; each secret may instead be read from a variable named by `password-env` / `token-env`. |
| `jdks.<n>` | The home of JDK `n`, for a pinned version jrs would not find on its own (§7.1). |

- Credentials from `JRS_REPO_<NAME>_USERNAME` + `JRS_REPO_<NAME>_PASSWORD` or
  `JRS_REPO_<NAME>_TOKEN` override the file. They are sent only to the
  repository they are named for, and never appear in `{:?}` output.
- A mirror changes where bytes come from, not what the project is: the
  manifest checksum in `jrs.lock` is computed from `jrs.toml`'s own URLs.
- A repository's `groups` (§8.7) decide whether it is asked at all; a mirror
  then changes where the bytes of the ones that are asked come from, and
  credentials still go by the repository's name.
- A failed connection, a dropped body, or an HTTP 429/500/502/503/504 is
  retried up to three attempts in total with a doubling backoff. A 404 or 410
  moves on to the next repository; a 401 or 403 fails at once and says where
  credentials go.

### 8.6 Cache maintenance

The cache is shared and only grows, so it can be pruned. `jrs cache path`
prints where it is. Pruning removes whole version directories, so an
artifact's jar, POM, checksums and snapshot records go together, and parents
left empty go too:

- `jrs cache prune` keeps what some project's `jrs.lock` names, the compilers
  in its `[[tool]]` blocks included, and removes the rest. Every build records its lockfile's path in `<cache>/.jrs/projects`.
  This is machine-local bookkeeping, which is why it may hold absolute paths
  when `jrs.lock` never does. Lockfiles that no longer exist are forgotten. An
  empty record, or a lockfile that cannot be read, stops the prune rather than
  risking a guess.
- `jrs cache prune --unused-for <days>` removes what no build has used for that
  long (§8.3), whatever references it.

Either takes `--dry-run`. What a lockfile does not name — parent POMs, BOMs,
the test launcher, JaCoCo — is pruned too, and downloaded again when next
wanted.

### 8.7 Repository groups

A repository in the long form,
`internal = { url = "...", groups = ["com.acme", "com.acme.*"] }`, serves only
the groups its patterns name: `com.acme` is that group, `com.acme.*` the groups
below it (not `com.acme` itself), and there is no other wildcard.

Exclusivity is always on. A group that some repository's `groups` name is looked
up only in the repositories that name it, in declaration order, and in no other,
Maven Central included. A group no repository claims is looked up in the
repositories without `groups`, Central last among them. That closes the
dependency-confusion hole, where an extra public repository listed first can
answer for an internal group, and saves a 404 per artifact per extra repository.
There is no separate switch like Gradle's `exclusiveContent`: a filter that
other repositories could still answer behind would leave the hole open.

`groups` count toward `manifest-checksum`. An unknown key in the long form is an
error, not a warning, since a misspelt `groups` would silently open the
repository to every group. Mirrors (§8.5) still apply to the URL, and
credentials go by the repository's name. When nothing may be asked for a group,
the error says so; when a filter decided where jrs looked, the "could not find"
error says that too.

### 8.8 Local jars

`name = { path = "libs/driver.jar" }`, in either dependency table, takes a jar
kept in the project as it is: no POM, no transitive graph, no mediation. It has
no coordinate, so its key is a name of its own (letters, digits, `.`, `-` and
`_`), which `jrs tree`, `jrs tree --why` and `jrs remove` use. The path is
relative to the project root and may not leave it; `version`, `classifier` and
`exclusions` do not go with it. `compile-only` and `runtime-only` apply as to
any dependency, and in `[dev-dependencies]` the jar is on the test classpath. On
a classpath, local jars come after the declared coordinates and before the
transitive ones, sorted by name.

`jrs.lock` records the relative path and a `sha256` pin (§4.4). The jar is
hashed on every build — unlike a cached jar, the file can change under the same
name, and a checked-in jar is small — and one that no longer matches its pin
fails the build with exit `1`, naming the file: the pin is an integrity check,
not a record that follows the file. `jrs update` pins the new bytes, and so does
any other change that re-resolves (a changed dependency table, `jrs add`). A
missing jar, or a directory, is a manifest error, exit `2`. `jrs outdated`
skips local jars, and `jrs verify` re-hashes them against their pins.

### 8.9 Managed versions

`[managed]` keeps versions for the whole graph in one place, which is what
Maven's `<dependencyManagement>` and Gradle's `platform()` and
`constraints { }` are for:

- `"group:artifact" = "version"` holds the artifact to that version wherever
  resolution reaches it, in place of the version any POM asks for (§8.2 step
  3). It puts nothing on a classpath: an artifact nothing depends on stays out.
- `"group:artifact" = { version = "...", bom = true }` imports a BOM: that
  POM's `<dependencyManagement>`, its own imports folded in, applies the same
  way. The BOM itself is not in the graph, and not in `jrs.lock`.
- The table's own entries come first, then each BOM in declaration order, and
  the first to name an artifact wins, as the first import does in Maven. A key
  names no classifier: a managed version covers every classifier of its
  `group:artifact`. A version is exact; a range is a manifest error.
- A dependency in either table may leave its version out, as `{}` or a long
  form without `version`, and take it from here. With no BOM in the table, a
  dependency it does not name is a manifest error, exit `2`. With one, whether
  it covers the dependency is only known once the BOM is read, so a dependency
  nothing covers fails resolution, exit `1`, naming it.
- A version written in `[dependencies]` still wins for its own dependency. The
  implied runtime libraries (§7.7) are written at their compiler's version, so
  they win too.
- The compilers' and the tasks' tool graphs (§7.6, §7.7) are resolved
  without it.

`[managed]` feeds `manifest-checksum` (§4.4). `jrs tree` marks a package at its
managed version `(managed)`, `jrs outdated` checks every entry, a BOM like any
other, and `jrs add` leaves the version out of whatever `[managed]` covers. A
jrs older than the table warns that it does not know it and refuses a
dependency without a version; a project that relies on it can say so with
`project.jrs-version` (§4.3).

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
  <[package.manifest] attributes>         # in declaration order
  ```
- Deterministic output: entries sorted, fixed timestamps, so repeated builds
  are byte-identical.
- `[package.manifest]` adds main attributes after jrs's own, in declaration
  order, so the jar stays byte-identical: `Implementation-Title`,
  `Automatic-Module-Name`, `Premain-Class`, `Add-Opens` and the like. Names
  follow the jar specification — an ASCII letter or digit, then letters,
  digits, `-` and `_`, at most 70 bytes — and are case-insensitive. The
  attributes jrs writes itself (`Manifest-Version`, `Created-By`,
  `Main-Class`, `Class-Path`) and `Name`, which starts a per-entry section,
  are refused with a manifest error naming the key. Values are one line and
  may use `{project.name}` and `{project.version}`. Every header is wrapped at
  72 bytes with continuation lines. `Implementation-Version` is not defaulted
  to `project.version`, since that would change the bytes of every existing
  jar; `Implementation-Version = "{project.version}"` sets it. The portable
  and fat jars get the same attributes.
- The `Class-Path` names the runtime classpath: `compile-only` jars are left
  out of it, as they are left out of `jrs run`, a fat jar and `lib/`.
  `runtime-only` jars and local jars are in all four.

### 9.2 Fat jar (`jrs package --fat`)

- Unpack every runtime dependency jar into a staging dir, then jar the result.
- Merge/conflict rules:
  - `META-INF/MANIFEST.MF` from dependencies is dropped.
  - `META-INF/services/*` entries are **concatenated**, not overwritten —
    getting this wrong silently breaks `ServiceLoader`.
  - Groovy extension-module descriptors
    (`META-INF/groovy/org.codehaus.groovy.runtime.ExtensionModule`, and the
    legacy copy under `META-INF/services/`) are **merged** into one at the
    `META-INF/groovy/` path, the union of every module's `extensionClasses`
    and `staticExtensionClasses`. First-wins would keep one Groovy module's
    extension methods and silently drop the rest.
  - Spring's registries are merged too, since an overwrite loses
    auto-configuration without an error: `META-INF/spring.factories` key by
    key, each key once with the union of every copy's comma-separated values;
    `META-INF/spring/*.imports` as the union of their lines, comments and blank
    lines dropped; `META-INF/spring.handlers`, `spring.schemas` and
    `spring.tooling` concatenated like service files (Maven Shade's
    `AppendingTransformer`), so a key two jars share resolves as it does on the
    unpacked classpath, to the later copy. First seen comes first throughout.
  - In every merge the project's own copy comes first, then each dependency's
    in classpath order: a project's own service file is merged, not replaced.
  - Signature files (`META-INF/*.SF`, `*.DSA`, `*.RSA`) are dropped, since the
    merged jar invalidates them. So are the dependencies' module descriptors
    (`module-info.class`, and under `META-INF/versions/<n>/`): the merged jar
    is none of those modules, and `kotlin-stdlib` and `kotlinx-coroutines`, for
    two, both ship one.
  - Duplicate classes: first wins, with a warning naming both sources.
- Requires `project.main-class`; error out clearly if it is missing.

### 9.3 Portable layout (`jrs package --portable`)

A thin jar's `Class-Path` points into the local cache, so it runs only on the
machine that built it. The portable layout copies the runtime dependencies
into `target/lib/` and writes relative `lib/<file>` entries, so the jar and its
`lib/` ship together. `target/lib/` is emptied first. Jars keep their file
names, except when two share one — the same artifact name in two groups — in
which case both are prefixed with their group.

### 9.4 Runtime images (`--jlink`, `--jpackage [type]`)

Both are JDK tools, so jrs only drives them:

1. `jdeps --print-module-deps --ignore-missing-deps --multi-release <release>`
   over the application jar and every runtime dependency. `jdeps` does not
   read `@argfiles` — handed one, it warns and succeeds with an empty answer
   — so the jars go on the command line, in batches that fit the OS limit.
   `package.add-modules` is added, for modules only reached by reflection or
   `ServiceLoader`.
2. `--jlink`: `jlink --add-modules <those> --strip-debug --no-header-files
   --no-man-pages` into `target/image`. The application goes under `app/`, and
   launchers go at `bin/<name>` and `bin/<name>.bat`. Each launcher runs the
   image's own `java` with `$JAVA_OPTS`, `run.jvm-args`, then
   `-jar app/<jar>`.
3. `--jpackage`: `jpackage` with the same modules, the staged application,
   `run.jvm-args` as `--java-options`, and an `--app-version` taken from the
   version's leading numbers. It writes into `target/jpackage`. The type is
   jpackage's own and passed through unchecked, since jpackage knows what the
   platform can build.

The image is built from the portable layout, or from the fat jar with `--fat`.
Both need `project.main-class`. `--dist` (§9.6) can be given with either, and
ships the same jar.

`run.java-agents` go into both, ahead of `run.jvm-args`: the launchers pass
`-javaagent:` with the agent's jar under `app/lib/`, and jpackage gets
`-javaagent:$APPDIR/lib/<file>`, which its launcher expands. An image of the
fat jar has no agent jar left to point at, so `run.java-agents` with `--fat`
is a manifest error. `run.env` and `run.cwd` belong to `jrs run` and do not go
into an image.

### 9.5 Sources and Javadoc jars (`--sources`, `--javadoc`)

`--sources` writes `target/<name>-<version>-sources.jar` from every main source
root — Java's and each turned-on language's — and the `source-outputs` of the
`pre-compile` tasks, so generated code is in it too. Each file sits at its path
relative to its root. `--javadoc` runs `jrs doc` (§7.4), once per invocation
however it is reached, and jars `target/doc` into
`target/<name>-<version>-javadoc.jar`; `javadoc` runs with `-notimestamp`, so
its pages carry no date. In a Scala or Groovy project the jar holds Scaladoc or
Groovydoc, which is what a repository expects under that name. Groovydoc runs
with `-notimestamp -noversionstamp`, and both scaladocs write the same pages
from the same classes. Both jars use the deterministic jar writer, carry only
`Manifest-Version` and `Created-By`, and combine with every other flag.

### 9.6 Distribution archives (`--dist`)

Gradle's `installDist` and `distZip` in one. The portable layout — or the fat
jar alone, with `--fat` — is staged in `target/dist/<name>-<version>/` with the
launchers `bin/<name>` (sh) and `bin/<name>.bat`, then zipped into
`target/<name>-<version>.zip` under a top-level `<name>-<version>/`. A launcher
finds the distribution from its own location and runs `$JAVA_HOME/bin/java`
when `JAVA_HOME` is set, the `java` on `PATH` otherwise, with `$JAVA_OPTS`,
`run.java-agents` (from `lib/`, as in §9.4), `run.jvm-args`, then `-jar <jar>` and the program's arguments; it says which it
looked for when there is none. The zip is deterministic like the jars: entries
sorted, the fixed 1980 timestamp, mode `0755` for `bin/<name>` and `0644` for
everything else. Zip only, since tar.gz would need a crate. `--dist` requires
`project.main-class`, and combines with `--jlink` and `--jpackage`.

### 9.7 Native images (`--native-image`)

`native-image` is a tool in a GraalVM JDK's `bin/` (`native-image.cmd` on
Windows), so jrs drives it the way it drives `jlink`. The JDK in use must be
GraalVM: without `native-image` beside `javac`, `--native-image` fails before
anything is built, with a toolchain error saying how to select one (§7.1). The
classpath (`target/classes`, then the runtime dependencies),
`-o target/native/<name>`, `package.native-image-args` and `project.main-class`
go into `target/.jrs/native-image.args`. `target/native` is emptied first, and
`native-image`'s output is passed through verbatim. The reachability metadata
that libraries ship under `META-INF/native-image/` is read by `native-image`
itself; jrs does not download the shared metadata repository the Gradle plugin
uses.

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
- CI (`.github/workflows/rust.yml`) runs `cargo build` + `cargo test` on Linux,
  macOS and Windows with a JDK from `actions/setup-java`, and the network tests
  on Linux.

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
- `--include-tag` / `--exclude-tag` pass straight through. `--method
  <class#method>` becomes `--select-method` and replaces the class-path scan,
  since the launcher refuses to combine the two.
- Reports: `--reports-dir target/test-reports` (emptied first), so JUnit XML
  lands where CI systems look for it.
- The launcher follows the resolved graph first: the version of the
  `junit-platform-engine` there. That is how Spock, Kotest and ScalaTest,
  which bring the platform themselves, get a launcher their engine agrees
  with. Otherwise it follows the declared Jupiter version: `5.x.y` →
  platform `1.x.y`; from JUnit 6 on, the two are the same. A project with only
  `junit:junit`, declared or brought in (as MUnit brings it), gets a fixed 1.x
  launcher. Its bundled Vintage engine runs JUnit 4 tests, and with both
  declared, both kinds run.
- The launcher goes last on the test JVM's classpath, so the project's own
  JUnit jars win — except the launcher's own parts (`junit-platform-launcher`,
  `-console`, `-reporting`), which it bundles at its version. A copy the graph
  brings in, as `kotlin-test-junit5` brings an older `junit-platform-launcher`,
  is left off, since it would shadow the launcher's classes.
- When any test source is not Java, the default `--include-classname` is the
  launcher's own pattern plus `.*Spec` and `.*Suite`, the names Spock, Kotest,
  ScalaTest and MUnit classes carry; without them those classes do not run at
  all. `--filter` still replaces it, and a Java-only project keeps the
  launcher's default.
- `test.jvm-args` go before `-cp`.
- `test.java-agents` are looked up in the resolved graph, anywhere on the test
  classpath, and go ahead of JaCoCo's agent as `-javaagent:<jar>`; under
  `--debug`, the JDWP agent goes first of all. An agent the graph does not
  hold is a manifest error that says to add it as a dependency. `test.env` is
  added to the environment the test JVM inherits.
- Coverage (`jrs test --coverage`) uses JaCoCo, resolved as an internal
  dependency like the launcher:
  - The agent (`org.jacoco.agent`, classifier `runtime`) goes on the test JVM
    as a `-javaagent`, recording to `target/jacoco.exec`.
  - The CLI (`org.jacoco.cli`, classifier `nodeps`) then writes HTML and
    `jacoco.xml` into `target/coverage`, reading every main source root, so
    Kotlin, Scala and Groovy files show too.
  - The line and branch totals are read back out of the XML for the summary.
  - A failing run still gets its report.
- An HTML page, `target/test-reports/index.html`, is written beside the XML
  after every run that left XML, a failing one included, and `jrs test`
  prints its path. It is built from the XML alone — hand-written, no script,
  no external resource, every string escaped — so the same reports give the
  same bytes: one row per class with its counts and time, the classes with
  failures first and unfolded with each failure's stack trace, the rest
  folded.
- `--rerun-failed` reads the last run's XML, retries folded in, and selects
  what is still failing. A test whose unique ID (recorded in the XML's
  `<system-out>`) ends in a `[method:…]` segment becomes `--select-method
  Class#method(params)`: a nested class by its binary name `Outer$Inner`, the
  parameter types from the unique ID. Anything else with a unique ID — one
  invocation of a parameterised test, one dynamic test, a Vintage test,
  another engine's test — is selected with `--select-unique-id`, and a case
  with no unique ID selects its whole class with `--select-class`. No reports
  at all is a usage error (exit `2`), since a run that wrote none did not
  pass; reports without a failure print "no failed tests to rerun" and exit
  `0`. It conflicts with `--filter`, `--method`, the tag flags and `--watch`.
- `--fail-fast` stops at the first failure. The launcher has its own
  `--fail-fast` from JUnit 6 (platform 6.0) on, and gets it. No 1.x launcher
  has one (1.14 included), and the tree it prints by default appears only once
  the whole run is over. So from 1.10 on jrs asks it for its test feed
  (`--details=testfeed`) instead, which reports each test as it starts and
  finishes, and kills the launcher when the test after the first failure
  starts, with the failure and its stack trace already passed through. That
  run has no summary block, so its counts are jrs's own, and no XML report or
  HTML page, which jrs says in a warning. A launcher before 1.10 has no test
  feed either, so there `--fail-fast` is ignored with a warning and every test
  runs. A run stopped early is not retried.
- `test.retries = n` (or `--retries n`) runs what is still failing again, up
  to `n` times. Each attempt is a fresh launcher that selects the failures
  the way `--rerun-failed` does and writes its XML into
  `target/test-reports/retry-<n>/`, leaving the first attempt's XML where it
  was. A test that passes on a retry is flaky: a `Flaky` line names it, the
  `Finished` line counts it apart from `passed`, and the page marks it. The
  run exits `0` if everything passed in the end, as with Gradle's test-retry
  plugin. The coverage agent appends to the first attempt's data. Under
  `--debug` there are no retries: each would be a JVM waiting for a debugger
  again.
- `test.forks = n` (or `--forks n`) splits a class-path scan among `n`
  launchers run at once, Gradle's `maxParallelForks`. jrs lists the top-level
  classes in `target/test-classes` and deals them out in name order, one to
  each launcher in turn. Each still scans, with one `--include-classname`: a
  lookahead naming its classes, nested ones included, in front of `--filter`,
  the pattern for the project's languages, or the launcher's own. It cannot
  be `--select-class`, since the launcher adds every selected class to its
  class-name patterns, and a class `--filter` leaves out would run. Each
  launcher writes its XML into `fork-<k>/`, which jrs moves up as
  `TEST-<engine>-fork-<k>.xml`, so the first attempt's reports stay in one
  directory. The counts are added up; the live counter follows all of them;
  their output is held and passed through whole, launcher by launcher under a
  `Fork` line, once all have finished. The coverage agents append to one
  `jacoco.exec`, which JaCoCo locks for each write. Retries run in one
  launcher over what failed. An explicit selection (`--method`,
  `--rerun-failed`), `--fail-fast`, which stops the whole run, and `--debug`,
  which waits for one debugger, run in one JVM. There is no `forkEvery`: each
  JVM runs its whole share.
- `test.coverage-minimum = { line = 0.80, branch = 0.70 }` makes
  `jrs test --coverage` exit `1` when a project-wide total in `jacoco.xml` is
  below its minimum, and the error names both numbers. The counters are
  JaCoCo's: `instruction`, `branch`, `line`, `complexity`, `method` and
  `class`. The comparison is exact, in integers, and a counter with nothing to
  count (a project without a branch) meets any minimum, as in JaCoCo's own
  check. Failing tests are reported before a shortfall, and the `post-test`
  hook runs only when the tests pass and the minimums are met. Without
  `--coverage` there are no totals, and the key is ignored.

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
| `<dependencies>` scope `compile` | `[dependencies]` |
| `<dependencies>` scope `runtime` | `[dependencies]`, as `runtime-only` |
| `<dependencies>` scope `test` | `[dev-dependencies]` |
| `<repositories>` | `[repositories]` |
| `<dependencies>` scope `system`, with a `<systemPath>` under `${project.basedir}` | a local jar (§8.8), `compile-only`, as Maven never packages it; a review line says so |
| `<dependencyManagement>` `<scope>import</scope>` | `[managed]` BOMs (§8.9); a dependency that gets no version from the POMs on disk is then written without one |
| `<dependencyManagement>` entries no declared dependency uses | `[managed]` versions; a declared dependency gets its managed version written out |
| `<parent>` `spring-boot-starter-parent`, not on disk | `[managed]` Boot's BOM, `spring-boot-dependencies` at the parent's version; `java.version` → `java.source`; `-parameters` in `java.javac-args` |
| `<repositories>` | `[repositories]` |
| `maven-jar-plugin` → `<mainClass>`, `maven-shade-plugin`'s transformer, `spring-boot-maven-plugin`'s `<mainClass>` or `start-class`; for Spring Boot, else the `@SpringBootApplication` class | `project.main-class` |
| `maven-jar-plugin` → `<archive><manifestEntries>` | `[package.manifest]`; `${project.version}` and `${project.artifactId}` become placeholders, any other property is reported |
| `exec-maven-plugin` executions | `[tasks]` and `[hooks]`: `exec` → a `run` task, `java` → `java @{classpath-argfile} <main>`, or a `main` task over the plugin's `<dependencies>` with `includePluginDependencies`; the `<phase>` → the hook at the same point ([TASKS.md §12](TASKS.md#12-open-questions) item 5). With no execution, its `<mainClass>` → `project.main-class` |
| `<profiles>` a plain `mvn` build activates: each whose `<activation>` holds with no `-P` and no `-D` — its conditions all negated properties (`!skipDocs`) — else the `<activeByDefault>` ones | merged into their POM before anything is read, as Maven injects them, the profile dominant: properties by name, dependencies and repositories by key, plugins by key with `<configuration>` merged element by element, executions by `<id>`. A default profile gets a review line when another profile could activate on some machine and turn it off |
| `maven-surefire-plugin` `<argLine>`, `<systemPropertyVariables>` | `test.jvm-args` |
| `maven-surefire-plugin` `<forkCount>`, a number | `test.forks` |

Reported, not translated:

- `<modules>` — multi-module builds are out of scope (§1.2). jrs migrates the
  module it was pointed at and lists the others so they can be migrated
  individually.
- A parent in a repository, other than `spring-boot-starter-parent`.
- A `system` scope outside the project, `<optional>`, and a `<type>` that
  does not go on a classpath.
- Any plugin other than `maven-compiler-plugin`, `maven-jar-plugin`,
  `maven-surefire-plugin`, `maven-shade-plugin`, `spring-boot-maven-plugin`,
  `exec-maven-plugin` and the language plugins of §11.6 — each is named in
  the report as unmigrated. An `exec-maven-plugin` execution jrs cannot
  translate whole (another phase, `<async>`, a property it cannot evaluate)
  is named on its own.
- Profiles a plain build does not activate — named only by `-P`, or waiting
  on a JDK, an OS, a file or a property that must be set — each with its
  condition: `jrs.toml` is one fixed configuration.
- A per-core `<forkCount>` (`1C`), which depends on the machine, and
  `<reuseForks>false</reuseForks>`, a fresh JVM per test class.

### 11.3 Gradle (`build.gradle`, `build.gradle.kts`)

Gradle build scripts are programs, so parsing them exactly would mean running
Gradle. jrs does not. Instead it does **line-oriented pattern extraction** over
the conventional declarative subset, and is explicit about the fact:

- `dependencies { }` entries of the form
  `implementation 'g:a:v'` / `implementation("g:a:v")` (also `api` →
  `[dependencies]`; `compileOnly` and `runtimeOnly` → `[dependencies]` as
  `compile-only` and `runtime-only`, and both of one library as a plain entry;
  `testImplementation`, `testRuntimeOnly` → `[dev-dependencies]`).
- `files('libs/a.jar')` and `fileTree(dir: 'libs', include: ['*.jar'])` with
  literal arguments → local jars (§8.8), named after their files. A `fileTree`
  is expanded to the jars it holds when jrs migrates, one entry each, with a
  review line, since a jar added there later needs an entry of its own. A
  closure, an `exclude`, or any include but `*.jar` and `**/*.jar` is reported.
- `group`, `version`, `rootProject.name` (also read from `settings.gradle`).
- `sourceCompatibility` / `targetCompatibility` /
  `java { toolchain { languageVersion = JavaLanguageVersion.of(n) } }`.
- `application { mainClass = "..." }` / `mainClassName`.
- `repositories { maven { url ... } }`; `mavenCentral()` is implicit. A
  `content { }` block's `includeGroup`, `includeGroupAndSubgroups` and
  `includeGroupByRegex` (when the regex names whole groups), and
  `exclusiveContent { forRepository { maven { url ... } } filter { ... } }`,
  become `groups` (§8.7). `groups` are exclusive, so a plain `content { }` block
  gets a review line. Module, version and exclude filters are reported.
- `environment` and `workingDir` on the `run` and `test` tasks, when literal
  (→ `run.env`, `run.cwd`, `test.env`; a `workingDir` on `test` is reported,
  as there is no `test.cwd`). A `-javaagent:` in `jvmArgs` is a path into
  Gradle's cache and is reported, except Mockito's recipe, which becomes
  `test.java-agents = ["org.mockito:mockito-core"]`.
- `maxParallelForks` on the `test` task, when literal → `test.forks`. A
  computed value, usually worked out from `availableProcessors()`, is
  reported, and so is `forkEvery`: jrs never restarts a test JVM part of the
  way through its share.
- `jar { manifest { attributes(...) } }` (also `tasks.jar`, Kotlin's
  `mapOf(...)` and `attributes["..."] = ...`) with literal values →
  `[package.manifest]`; `version` / `project.version` → `{project.version}`,
  and a `Main-Class` attribute → `project.main-class`. Computed values and
  the attributes jrs writes are reported, as are `withSourcesJar()` and
  `withJavadocJar()`, which are the `jrs package --sources` / `--javadoc`
  flags rather than manifest keys.
- Managed versions (§8.9). `platform('g:a:v')` and `enforcedPlatform(...)`,
  also through a catalog, and `dependencyManagement { imports { mavenBom
  'g:a:v' } }` → `[managed]` BOMs; `dependencyManagement { dependencies {
  dependency 'g:a:v' } }` and `constraints { implementation 'g:a:v' }` →
  `[managed]` versions. With anything there, a dependency without a version
  (`implementation 'g:a'`, a catalog entry with none) is migrated without one.
- Spring Boot. The `org.springframework.boot` plugin at a literal version,
  with `io.spring.dependency-management` or
  `platform(SpringBootPlugin.BOM_COORDINATES)`, → `[managed]` Boot's BOM,
  `spring-boot-dependencies` at that version. The plugin compiles with
  `-parameters`, which goes into `java.javac-args`, and makes the
  `@SpringBootApplication` class the main class when none is named, which
  migration does by reading the main sources. `bootJar`'s nested layout is not
  reproduced: `jrs package --fat` builds a flat jar with Spring's registries
  merged (§9.2), and a review line says so.

Anything jrs cannot read confidently is skipped and reported — never guessed:

- Version catalogs (`libs.versions.toml`) are parsed when present, since they
  are declarative; `libs.foo.bar` references are resolved through them.
  Unresolvable aliases become a warning with the reference left in a comment.
- Dependencies built from variables, `ext` blocks, loops or conditionals.
- Plugins, `subprojects { }` / `allprojects { }`, and multi-project
  `settings.gradle` includes (listed, not migrated).
- Custom tasks, except those that say everything in literals (§7.6,
  [TASKS.md §12](TASKS.md#12-open-questions) item 5): an `Exec` task becomes
  a `run` task, a `JavaExec` over `sourceSets.main.runtimeClasspath` becomes
  `java @{classpath-argfile} <main>` depending on `build`, and a task with only
  `dependsOn` becomes an aggregate. `compileJava.dependsOn`, `test.dependsOn`,
  `run.dependsOn` and `finalizedBy` on `compileJava`, `test` or `jar` become
  `[hooks]`. A task with a `doLast { }` closure, another type, an interpolated
  value or a dependency on a task left out is listed with the reason, whole:
  half a task would run and do the wrong thing.

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

### 11.6 Kotlin, Scala and Groovy

What the builds already say about the other languages (§7.7) is translated too:

| Source | Construct | Becomes |
| --- | --- | --- |
| Gradle | `kotlin("jvm") version "x"`, `id("org.jetbrains.kotlin.jvm") version "x"` | `[kotlin] version = "x"` |
| Gradle | `kotlin { jvmToolchain(n) }` | `java.jdk = n` |
| Gradle | `id 'groovy'` / `id 'scala'` plus the library dependency | `[groovy]` / `[scala]`, at that dependency's version |
| Maven | `kotlin-maven-plugin` at `${kotlin.version}` | `[kotlin]`; its `<jvmTarget>` → `java.source` when that is not set |
| Maven | `scala-maven-plugin`, `gmavenplus-plugin` | `[scala]` / `[groovy]` |
| both | an explicit `kotlin-stdlib` / `scala-library` / `groovy` dependency at the compiler's version | dropped from `[dependencies]`, since it is implied; reported as migrated |
| both | compiler plugins (`allopen`, `spring`, `serialization`, `kapt`) | not migrated, each with its reason |

A version below jrs's minimum is not migrated, and the report says why.

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
- ☑ benchmark against a fixture project with ~20 transitive dependencies;
  target: resolution dominated by network, not by jrs. Re-run the benchmark with
  `--progress never` to prove the renderer costs nothing measurable.
  `cargo bench --bench resolution` serves a 22-artifact graph from a local
  repository that adds latency to every request. Two findings:
  - About 70–80% of wall time is network; jrs's own work takes around 15 ms.
  - The first run showed the renderer did cost something: every live scope
    waited out a render tick when it ended, roughly 90 ms a build. The render
    thread now parks instead of sleeping and is woken to stop. The two modes
    are now within noise of each other.

### M6 — Migration
- ☑ migrating a project from Maven (`pom.xml`)
- ☑ migrating a project from Gradle (`build.gradle`, `build.gradle.kts`)
- Deliberately last: migration is only useful once every feature it can
  translate into actually works, and it reuses `resolve/pom.rs` from M3.

### M7 — Tasks
- ☑ user-defined tasks and lifecycle hooks: `[tasks.*]`, `[hooks]`,
  `jrs task`, `jrs task --list` (T1)
- ☑ up-to-date checks, generated sources and resources, `jrs task --watch`
  and task inputs in watch mode (T2)
- ☑ tool dependencies: a `main` action and `[tasks.<name>.dependencies]`,
  Java tools from Maven Central resolved as a graph of the task's own and
  pinned in `jrs.lock`, and `jrs tree --task` (T3)
- Added after M1–M6 had landed; the design, and the argument for narrowing
  §1.2, is in [TASKS.md](TASKS.md). §7.6 is the condensed contract. T3 waited
  until T1 and T2 had seen use, since it changes the lockfile's inputs.

### M8 — JVM languages
- ☑ multi-step compile units, multi-root sources, and compilers as isolated
  tool graphs pinned in `jrs.lock` (L0)
- ☑ Kotlin (L1), Groovy (L2), and Scala 2.13 and 3 (L3), each mixed with Java
  both ways, and tested on the JUnit Platform: JUnit 5, Spock, MUnit
- ☑ `jrs init --lang`, the `jrs migrate` rows, `jrs outdated` for compiler
  versions and `jrs tree --tool` (L4)
- Added after M7; the design, and the argument for narrowing §1.2, is in
  [JVM_LANGUAGES.md](JVM_LANGUAGES.md), and §7.7 is the condensed contract.
  Scaladoc and Groovydoc for `jrs doc` came later (§7.4); Kotlin's Dokka is
  still out.

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
   `jrs add`/`jrs remove` and shell completions looked as if they needed
   `toml_edit` and `clap_complete`; both are hand-rolled instead. The first is
   a line editor for the dependency tables that refuses what it cannot edit
   safely (§4.5). The second is a generator driven by clap's own introspection
   of the command definition.
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
   **Decision:** the JUnit Platform only. JUnit 5 came first. JUnit 4 followed
   through the Vintage engine, which the console launcher bundles, so a project
   on `junit:junit` needs nothing else. JUnit 6 runs like 5. TestNG stays out:
   it would mean a second launcher and a second output format to follow.
8. **Windows support.** Path separators (`;` vs `:`) on the classpath and
   `.exe` suffixes on toolchain binaries need handling from the start if it is
   in scope at all.
   **Decision:** in scope, and handled — `Toolchain::classpath_separator`, the
   `.exe` suffix, `%LOCALAPPDATA%` for the cache, and drive letters in
   `file://` URLs. CI runs the whole suite on Linux, macOS and Windows.
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
12. **User-defined build steps.** §1.2 first ruled out "plugin systems, custom
    task graphs, or a build DSL". But real projects nearly always have a step
    of their own: sources generated before `javac`, a jar signed or checksummed
    after `jar`. A wrapper script handles chores, but cannot reach into the
    middle of `jrs build`, and cannot see the pinned JDK or the resolved
    classpath. Embedded scripting (Rhai, Lua), native or WASM plugins, and a
    Java plugin API were the other candidates; each is a DSL or a plugin system
    by another name.
    **Decision:** tasks as subprocesses (§7.6, designed in [TASKS.md](TASKS.md)).
    A task is a TOML table naming a command, which is what jrs already does
    with `javac`, and it attaches only at fixed lifecycle points. There is still
    no DSL, no plugin API, and no way to rewire the built-in pipeline; build
    logic written in Java is a `script` task on the JDK's source launcher. §1.2
    was narrowed to say so. Tool dependencies (TASKS.md §8) are deferred, since
    they change the lockfile format and the resolver's inputs.
13. **Other JVM languages.** §1.2 first ruled out Kotlin, Scala and Groovy.
    But their compilers are JVM programs on Maven Central, their runtime
    libraries are ordinary dependencies, and their output is class files that
    every later phase already handles: what was missing was one compile step
    before `javac`. Compilers from `PATH`, detecting a language by extension
    with no manifest key, and delegating to Gradle, Maven or sbt were the
    alternatives.
    **Decision:** one table per language (§7.7, designed in
    [JVM_LANGUAGES.md](JVM_LANGUAGES.md)). The compiler is pinned in the
    manifest and in `jrs.lock`, resolved as a graph of its own and run on the
    project's JDK, as `javac` and the JUnit launcher are. This built the
    isolated tool graph and the `[[tool]]` lockfile blocks that tool
    dependencies (12) will reuse. §1.2 was narrowed to say what stays out.
