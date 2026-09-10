# Architecture

This document describes how jrs is put together: the modules, the way a command
flows through them, and the invariants the code is organised around. It is a map
of the code as it is. The design the code follows is
[specs/INITIAL_SPEC.md](specs/INITIAL_SPEC.md), with
[specs/JVM_LANGUAGES.md](specs/JVM_LANGUAGES.md) for Kotlin, Scala and Groovy and
[specs/TASKS.md](specs/TASKS.md) for tasks and hooks. Module doc comments cite
those by section (`SPEC §8.2`), and SPEC §12.1 lists the four places where the
code deliberately diverges from SPEC.

## Contents

1. [The shape of it](#1-the-shape-of-it)
2. [Source map](#2-source-map)
3. [Module layers](#3-module-layers)
4. [From `main` to an exit code](#4-from-main-to-an-exit-code)
5. [The build spine: `Session`](#5-the-build-spine-session)
6. [Dependency resolution](#6-dependency-resolution)
7. [Compile units](#7-compile-units)
8. [Tests](#8-tests)
9. [Packaging](#9-packaging)
10. [Tasks and hooks](#10-tasks-and-hooks)
11. [The output layer](#11-the-output-layer)
12. [Errors and exit codes](#12-errors-and-exit-codes)
13. [Files on disk](#13-files-on-disk)
14. [How jrs itself is tested](#14-how-jrs-itself-is-tested)
15. [Invariants](#15-invariants)

## 1. The shape of it

jrs is a **driver**, not a reimplementation of the JDK. It reads one manifest,
resolves a dependency graph from Maven repositories, and then shells out to the
JDK's own tools. Kotlin, Scala and Groovy compilers are Java programs, so they
are resolved like dependencies and run on the project's JDK too.

```
                         ┌──────────────────────────────────────────┐
  jrs.toml ─────────────►│                   jrs                    │
  jrs.lock ◄────────────►│                                          │
  config.toml ──────────►│  manifest → resolve → compile → package  │
                         │                 │          │        │    │
                         └─────────────────┼──────────┼────────┼────┘
                                           │          │        │
               ┌───────────────────────────┘          │        │
               ▼                                      ▼        ▼
   ┌───────────────────────┐       ┌───────────────────────────────────────┐
   │ Maven repositories    │       │ The JDK (subprocesses)                │
   │  • [repositories] …   │       │  javac · java · javadoc               │
   │  • Maven Central last │       │  jdeps · jlink · jpackage             │
   │  • file:// in tests   │       │  kotlinc/scalac/groovyc (java @file)  │
   └──────────┬────────────┘       │  java … ConsoleLauncher (JUnit)       │
              ▼                    └───────────────────────────────────────┘
   ┌───────────────────────┐
   │ shared artifact cache │   Maven layout, atomic writes,
   │ (~/.cache/jrs, …)     │   checksum-verified
   └───────────────────────┘
```

Everything is a library (`src/lib.rs`); `src/main.rs` is only this:

```rust
fn main() {
    std::process::exit(jrs::cli::main());
}
```

so every phase can be driven from a test without spawning the binary.

## 2. Source map

```
src/
├── main.rs            the five-line entry point
├── lib.rs             module list; re-exports JrsError and Result
├── cli.rs             clap definitions, dispatch, Session (the build spine)
├── error.rs           JrsError, exit codes
├── manifest.rs        jrs.toml: hand-parsed, ordered, per-key diagnostics
├── config.rs          the per-user config.toml: credentials, mirrors, proxy, jdks
├── edit.rs            format-preserving line editor for `jrs add` / `jrs remove`
├── lockfile.rs        jrs.lock: read, write, manifest-checksum
├── project.rs         layout, source globbing, target/, resource sync, snapshots
├── toolchain.rs       finding the JDK; running subprocesses (captured/inherited)
├── compile/
│   ├── mod.rs         CompileUnit: steps, fingerprint, staleness, argfiles
│   ├── javac.rs       javac and javadoc
│   └── lang.rs        enum Language: Kotlin/Scala/Groovy as plain data
├── resolve/
│   ├── mod.rs         breadth-first walk, nearest-wins mediation, Resolution
│   ├── coord.rs       coordinates, scopes, Maven version ordering
│   ├── pom.rs         POM XML → effective model (parents, BOMs, properties)
│   ├── metadata.rs    maven-metadata.xml: version lists, snapshot builds
│   ├── repo.rs        Fetcher: cache → repositories, checksums, retries
│   └── cache.rs       the local store: layout, atomic writes, pruning
├── test.rs            the JUnit Platform console launcher; JaCoCo
├── package.rs         thin / portable / fat jars; deterministic zip writing
├── image.rs           jdeps, jlink, jpackage
├── runner.rs          `jrs run`: the user's program gets the terminal
├── task.rs            [tasks] and [hooks]: plan, cycles, placeholders, freshness
├── migrate/
│   ├── mod.rs         detection, report, manifest emission
│   ├── maven.rs       pom.xml → Manifest (reuses resolve::pom)
│   └── gradle.rs      build.gradle[.kts] → Manifest (pattern extraction)
├── completions.rs     bash/zsh/fish scripts generated from the clap definition
└── ui/
    ├── mod.rs         Ui handle: mode detection, phase lines, live scopes
    ├── progress.rs    Live state and the pure functions that turn it into lines
    ├── render.rs      the only writer of escape sequences; cursor guard
    └── glyphs.rs      Unicode/ASCII glyph sets, colour, the wordmark
```

## 3. Module layers

The dependency arrows point one way: toward `cli`. The picture below is drawn
from the `use crate::…` lines in the source, simplified to the edges that
matter.

```
                                ┌────────┐
                                │  cli   │  dispatch, Session, phase lines
                                └───┬────┘
         ┌──────────┬──────────┬────┴─────┬───────────┬──────────┬──────────┐
         ▼          ▼          ▼          ▼           ▼          ▼          ▼
   ┌──────────┐┌─────────┐┌─────────┐┌─────────┐┌──────────┐┌─────────┐┌──────────┐
   │ package  ││  image  ││  test   ││ runner  ││ compile  ││  task   ││ migrate  │
   └────┬─────┘└────┬────┘└────┬────┘└────┬────┘└────┬─────┘└────┬────┘└────┬─────┘
        │           │          │          │          │           │          │
        │           ▼          ▼          ▼          ▼           │          │
        │      ┌──────────────────────────────────────────┐      │          │
        │      │ toolchain   (find the JDK, run processes)│◄─────┘          │
        │      └───────────────────┬──────────────────────┘                 │
        ▼                          │                                        │
   ┌──────────┐   ┌──────────┐     │     ┌────────────────────────────┐     │
   │ project  │──►│ manifest │◄────┼─────│ lockfile                   │     │
   └──────────┘   └────┬─────┘     │     └─────────────┬──────────────┘     │
                       │           │                   ▼                    │
                       │           │     ┌────────────────────────────┐     │
                       └───────────┼────►│ resolve  (coord, pom,      │◄────┘
                                   │     │  metadata, repo, cache)    │
                                   │     └─────────────┬──────────────┘
                                   │                   ▼
                                   │              ┌──────────┐
                                   │              │  config  │
                                   │              └──────────┘
                                   ▼
                              ┌──────────┐        ┌──────────┐
                              │    ui    │        │  error   │ ◄── everyone
                              └──────────┘        └──────────┘
                           depends on nothing    depends on nothing
```

What the layering buys:

- **`ui` depends on nothing.** It knows glyphs, colours, lines and a cursor. It
  does not know what a dependency or a class file is.
- **`manifest`, `project`, `resolve` and `task` know nothing about terminals.**
  The `Fetcher` reports transfers through a `TransferReporter` trait. The CLI
  plugs in `resolve::UiReporter`, an adapter that only mutates the UI's shared
  live state, and tests plug in a `SilentReporter`.
- **`manifest` borrows data, not behaviour, from above it:** the `Language`
  enum from `compile::lang`, coordinate parsing from `resolve::coord`, and
  `task::check` for the whole-manifest task validation.
- **Modules that run processes take a `&Ui`** (`toolchain`, `compile`, `test`,
  `runner`, `image`) only so they can tear the live region down before a
  subprocess writes to the terminal, and pass that output through verbatim.
- **`cli` is the only module that decides what a user sees**, and the only one
  that renders an error.

## 4. From `main` to an exit code

```
 main.rs
   │  std::process::exit(jrs::cli::main())
   ▼
 cli::main()
   │
   ▼
 Cli::try_parse() ─────────── Err ──► clap prints it ─► --help/--version: exit 0
   │ Ok                                                  anything else:  exit 2
   ▼
 Ui::new(global flags)       --progress, --color, --charset, -v, -q; the
   │                         first live frame on a terminal installs the
   ▼                         cursor guard (ui/render.rs: panic hook + signals)
 dispatch(cli, ui)
   │
   ├── needs no manifest, or manages its own sessions:
   │     init · migrate · completions · cache · add · remove
   │     build --watch · test --watch · task --watch   (a fresh Session per change)
   │
   └── everything else:
         Session::open(cli, ui)        load jrs.toml (walk up from cwd, or
           │                           --manifest-path), config.toml, warnings
           ▼
         session.<command>()           build · test · run · package · doc · task
           │                           clean · tree · classpath · update
           │                           verify · outdated
           ▼
         Result<i32>
           ├── Ok(code) ─────────────────────────────────────────────────► code
           └── Err(JrsError) ─► ui.suspend(); ui.error(e) ─► e.exit_code() 1 | 2
```

## 5. The build spine: `Session`

`Session` (`cli.rs`) holds one command's worth of state: the manifest, the
user configuration, the `Ui`, the clock, and memoised results. Every expensive
step is behind a `OnceCell`, so however many hooks and `depends-on` lists reach
it, it runs **at most once per invocation**.

```
 Session
 ├── manifest, config, ui, jobs, offline, started
 ├── toolchain  : OnceCell<Toolchain>       found once
 ├── resolution : OnceCell<Resolution>      the project's graph
 ├── tools      : OnceCell<Vec<Tool>>       each compiler's own graph
 ├── built      : OnceCell<Built>           the result of build()
 ├── ran        : RefCell<HashSet<String>>  tasks already run or found fresh
 ├── done       : RefCell<HashSet<Builtin>> built-ins a depends-on already ran
 └── jar        : RefCell<Option<PathBuf>>  set once package has written it
```

The commands nest. `build()` is the shared spine; the others call it first and
add to it:

```
 build()                                                   jrs build
 │  toolchain()                  JAVA_HOME / PATH / pinned JDK (§7.1)
 │  resolved() ─► dependencies() lockfile or fresh resolution, downloads (§6)
 │  hook(pre-compile)            code generators run before sources are globbed
 │  project.sources(Main, generated)
 │  compile_unit("main") ─► is_stale? ─► compile::compile   or   "Fresh"
 │  sync_resources  src/main/resources ─► target/classes
 │  sync_generated  task-generated resources ─► target/classes
 └  hook(post-compile)

 test()                                                     jrs test
 │  build()
 │  hook(pre-test)
 │  compile_unit("test")  classpath = target/classes + test classpath
 │  sync test resources
 │  fetch_internal: JUnit console launcher (+ JaCoCo agent/cli with --coverage)
 │  test::run  ─► java … ConsoleLauncher --scan-class-path …
 │  coverage report (even when tests failed)
 └  hook(post-test)      only if the tests passed

 package()                                                  jrs package
 │  build()
 │  write_thin_jar | copy_libraries + write_thin_jar | write_fat_jar
 │  images: jdeps ─► jlink / jpackage        (--jlink / --jpackage)
 └  hook(post-package)

 run_command()                                              jrs run
 │  build()
 │  hook(pre-run)
 └  runner::run_main ─► java … <main-class> args   (stdio inherited)
```

Every phase line (`Resolving`, `Downloading`, `Compiling`, `Fresh`, `Testing`,
`Packaging`, `Running`, `Task`, `Finished`, …) is printed by these methods,
unconditionally.
The spinner or download bars around a phase are a separate `LiveScope` that
only adds motion. See [§11](#11-the-output-layer).

## 6. Dependency resolution

### 6.1 Lockfile or fresh resolution

`Session::dependencies()` decides whether the graph comes from `jrs.lock` or
from the network:

```
               Manifest::effective_dependencies()
               = [dependencies] + implied runtime libraries (kotlin-stdlib, …)
                               │
                               ▼
                 ┌───────────────────────────┐
                 │ jrs.lock exists, and its  │
                 │ manifest-checksum matches,│── yes ──► lock.to_resolution()
                 │ and not `jrs update`?     │           lock.tool(name) per language
                 └─────────────┬─────────────┘                   │
                               │ no                              │
                               ▼                                 │
                 resolve::resolve(manifest)       ─┐             │
                 resolve::resolve_tool(compiler)   │ "Resolving" │
                   one isolated graph per language ─┘             │
                               │                                 │
                               ▼                                 ▼
                 ┌──────────────────────────────────────────────────┐
                 │ locate_cached → fetch_jars      "Downloading"    │
                 │ (pinned checksums from jrs.lock checked on       │
                 │  download; cached jars are not re-hashed)        │
                 └───────────────────────┬──────────────────────────┘
                                         ▼
                       fresh? ─► write jrs.lock ([[package]] + [[tool]])
                       register the project in the cache (for `cache prune`)
                                         ▼
                                     Resolution
```

`jrs.lock` records coordinates and checksums, never absolute paths; cache paths
are recomputed on load. It is `version = 1` byte for byte until a `[[tool]]`
block makes it `version = 2`.

### 6.2 The graph walk

`resolve::resolve` is breadth-first **by level**, so that each level's POM
fetches go out together on a `rayon` pool sized to `--jobs`:

```
 level 1:  declared deps, in manifest order (dev-dependencies on the Test classpath)
    │
    ▼
 ┌─────────────────────────────────────────────────────────────────────────┐
 │ for each pending node, in order:                                        │
 │   group:artifact already selected?                                      │
 │     yes → keep the shallower (earlier) version  ◄── nearest-wins;       │
 │           warn if the versions differ               ties broken by      │
 │           widen its classpath (test → compile)      declaration order   │
 │     no  → select it, admit it to this level                             │
 └───────────────────────────────┬─────────────────────────────────────────┘
                                 ▼
          par_iter over admitted nodes: fetch + parse POM → effective model
            (parent chain, BOM imports, properties, <dependencyManagement>)
                                 │
                                 ▼
          children → next level   (skipping optional deps, non-transitive
                                   scopes and exclusions; a version range
                                   anywhere in the graph is an error)
                                 │
                        depth > 64? ─► error
                                 │
                                 ▼
     after the walk: propagate classpath widening down the edges to a fixpoint
                                 ▼
          Resolution { packages sorted by coordinate, roots, warnings }
```

Version ranges are rejected with an error, never guessed at. The classpath jrs
hands to `javac` is ordered direct dependencies first, then transitive ones,
each sorted by coordinate, so it is deterministic.

A compiler's graph goes through the same function via `resolve_tool`, but as
a separate resolution: the Kotlin compiler's own `kotlinx-coroutines` must
never mediate against the project's.

### 6.3 Fetching and the cache

```
 Fetcher::jar(coord) / pom(coord)
   │
   ├─► cache hit? ─── yes ─► (snapshot and due for a re-check? re-check,
   │                          fall back to the cached copy on failure)
   │                          mark_used ─► path
   │ no
   ├─► --offline? ─── yes ─► error naming the expected cache path
   │
   └─► for repo in [repositories…] (mirrors applied), then Maven Central:
         GET  (retry twice on dropped connection / 429 / 5xx;
               believe 404 and 401 at once)
         verify the repository's .sha1, and the jrs.lock pin if there is one
         cache.store ─► temp file in the destination dir, then rename
```

Credentials, proxies and mirrors come from the user's `config.toml`
(`config.rs`), never from `jrs.toml`, and a mirror never changes `jrs.lock`.
The cache mirrors the Maven repository layout, so a path from an error message
can be inspected with `ls`.

## 7. Compile units

A compile unit (`compile/mod.rs`) is the main sources or the test sources. It
is **all-or-nothing**: its steps share one output directory, one fingerprint and
one staleness decision, and a stale unit is compiled from an emptied output
directory so a deleted source cannot leave its class behind.

```
 CompileUnit { sources, output_dir, classpath, release, javac flags,
               foreign: Option<ForeignCompiler> }
      │
      ▼
 is_stale?  ── fingerprint differs (flags, every jar's size+mtime, source list)
      │        or newest source is newer than newest class
      │
      ├── Java only
      │     javac @target/.jrs/javac-main.args
      │
      ├── Java + Kotlin, or Java + Scala
      │     java @target/.jrs/kotlinc-main.args   compiler reads .kt/.scala AND
      │       (compiler classpath, main class,     .java (for symbols), writes
      │        flags and sources in one argfile)   only its own classes
      │     javac @target/.jrs/javac-main.args    Java sources, with those
      │                                           classes on the classpath
      │
      └── Java + Groovy
            java @target/.jrs/groovyc-main.args   joint compilation: groovyc
                                                  runs javac itself
      │
      ▼
 success ─► write target/.jrs/main.fingerprint      failure ─► delete it
```

What jrs knows about each language — file extension, source roots, compiler
coordinate and main class, implied runtime library, generated flags — is plain
data matched on `enum Language` in `compile/lang.rs`. There is no registry and
no trait object. A unit mixes Java with at most one other language; main and
test are separate units, so Kotlin code with Spock tests is fine.

Every compiler is run as `java @argfile` (or `javac @argfile`), never with a
long command line: a few dozen dependency jars exceed the OS argument limit.
Compiler output is passed through verbatim.

## 8. Tests

`jrs test` runs the JUnit Platform Console Launcher, which jrs treats as an
internal dependency:

```
 test classpath =  target/test-classes
                 + target/classes
                 + the Test classpath from the resolution
                   (minus launcher parts the project's graph would shadow)
                 + junit-platform-console-standalone     ◄── last, so the
                                                              user's jars win
 java [jvm-args] [-javaagent:jacoco…] -cp … ConsoleLauncher [execute]
      --scan-class-path target/test-classes   (or --select-method …, which
                                               replaces the scan)
      --include-classname … --include-tag … --exclude-tag …
      --reports-dir target/test-reports
        │
        ├── stdout/stderr passed through verbatim
        ├── each line read to advance the live counter (TestState)
        └── the launcher's summary block parsed for the authoritative totals
```

The launcher version is derived from the Jupiter (or other engine) version the
project declares. JUnit 4 runs on the Vintage engine the launcher bundles.

## 9. Packaging

```
                              target/classes
                                    │
          ┌─────────────────────────┼──────────────────────────┐
          ▼                         ▼                          ▼
     thin (default)            --portable                    --fat
  Class-Path: absolute     deps copied to target/lib/   deps unpacked into
  paths into the cache     Class-Path: lib/<file>.jar   one jar; project
                                    │                   classes win conflicts
                                    │                          │
                                    ├──────────────────────────┤
                                    ▼                          ▼
                         --jlink / --jpackage use the portable layout,
                         or the fat jar when --fat is given too
                                    │
                        jdeps ─► module list (+ [package] add-modules)
                                    │
                    ┌───────────────┴────────────────┐
                    ▼                                ▼
            jlink ─► target/image           jpackage ─► target/jpackage
```

All jars are **deterministic**: entries sorted, a fixed 1980 timestamp, fixed
permissions, so two builds of the same inputs are byte-identical. The fat jar
is written straight from the dependency jars without a staging directory, and
its merge rules are load-bearing: `META-INF/services/*` files are concatenated
(overwriting them breaks `ServiceLoader` silently), and Groovy extension-module
descriptors are merged into one.

## 10. Tasks and hooks

User-defined tasks are subprocesses. Nothing a user writes runs inside jrs, and
hooks fire at fixed points of the spine in [§5](#5-the-build-spine-session);
they cannot reorder or replace the built-in phases.

```
             ┌────────────────────── cli.rs (Session) ─────────────────────┐
             │ hook(PreCompile) ─► task::plan(manifest, roots)             │
             │                       │  depends-on expanded, ordered,      │
             │                       │  built-ins (build/test/package/doc) │
             │                       ▼  as steps                           │
             │   for step in plan:                                         │
             │     Builtin ─► self.build() / test() / package() / doc()    │
             │     Task    ─► already in `ran`? skip                       │
             │                task::prepare ─► placeholders expanded,      │
             │                                  env (JRS_*, JAVA_HOME),    │
             │                                  inputs/outputs fingerprint │
             │                  fresh?  ─► "Fresh (task)"                  │
             │                  else    ─► "Task" + toolchain::run_task_*  │
             │                              success ─► record fingerprint  │
             │                              failure ─► build error, exit 1 │
             └─────────────────────────────────────────────────────────────┘

 hook points:   pre-compile ─ post-compile ─ pre-test ─ post-test
                post-package ─ pre-run
```

`task.rs` holds everything that neither starts a process nor touches a
terminal: whole-manifest checks (unknown references, cycles, where a
placeholder is available, generated output must be under `target-dir`),
ordering, placeholder expansion, the environment and fingerprints. It is tested
without a TTY; `cli.rs` decides when a task runs and what is printed around it.

## 11. The output layer

Only `ui/` touches the terminal. Build code never prints: it either calls a
`Ui` method that writes a **permanent** line, or mutates the **live** state that
a single render thread draws.

```
   worker threads                         Ui (Arc<Inner>, cheap to clone)
 ┌──────────────────┐   update_live()   ┌─────────────────────────────────┐
 │ rayon downloads  │──────────────────►│ live: Mutex<Live>               │
 │ (via UiReporter) │                   │   None | Spinner | Downloads    │
 │ test line reader │                   │        | Tests                  │
 └──────────────────┘                   │ tick: AtomicU64                 │
                                        │ renderer: Mutex<Renderer>       │
 cli.rs                                 └──────┬───────────────────┬──────┘
 ┌──────────────────┐   phase()/warn()/        │                   │
 │ Session methods  │   error()/summary()      │ render thread,    │
 │                  │──────────────────────────┤ every 80 ms while │
 └──────────────────┘   permanent lines        │ a LiveScope lives │
                                               ▼                   ▼
                                 progress::Live::lines(glyphs, color,
                                     tick, width) ─► Vec<String>   (pure)
                                               │
                                               ▼
                            render::Renderer ── the only writer of
                            escape sequences:
                              • erase the live region before any
                                permanent line, then redraw it below
                              • truncate to the terminal width
                              • restore the cursor on every exit path
                                               │
                                               ▼
                                   stderr (progress)  /  stdout (real output:
                                                         tree, classpath, …)
```

The mode is decided once, at start-up:

```
  -q ──────────────────────────────────► Quiet     errors only
  -v ──────────────────────────────────► Verbose   plain + every command line
  --progress never ────────────────────► Plain     one line per phase
  --progress always ───────────────────► Animated  spinners, bars, framed summary
  --progress auto:  stderr is a TTY,
                    TERM != dumb, no NO_COLOR,
                    not CI  ───────────► Animated, otherwise Plain
```

Because phase lines are emitted by `cli.rs` in every mode and live scopes only
add motion, `--progress never` and the animated mode are provably the same
build with the same transcript (SPEC §12.1, divergence 4). `Ui::captured`
swaps the terminal for an in-memory `Capture`, a fixed geometry and a clock
that only advances when a test calls `render_frame`, which is how
`tests/output.rs` snapshots animation frames.

Before a subprocess writes to the terminal — `javac` diagnostics, a test run,
`jrs run` handing stdio to the user's program — the live region is torn down
(`Ui::suspend`), and the output is passed through untouched.

## 12. Errors and exit codes

Errors are values. Every fallible function returns `jrs::Result<T>`, and there
is one error enum:

```
 JrsError                         exit code
 ├── Usage(String)      ────────►  2   bad flags, missing prerequisite
 ├── Manifest(String)   ────────►  2   jrs.toml missing, invalid, unknown ref
 ├── Toolchain(String)  ────────►  1   no JDK, or too old
 ├── Resolve(String)    ────────►  1   network, checksum, bad POM, range
 ├── Build(String)      ────────►  1   compiler, packaging or task failure
 ├── Test(String)       ────────►  1   tests failed, launcher did not start
 └── Io { path, source }────────►  1   always names the path
```

Library code never prints an error; `cli::main` renders it once, after tearing
the live region down. `jrs run` and `jrs task <name>` return the program's own
exit code. There is no `unwrap()` outside `#[cfg(test)]`, except on lock
poisoning, which is documented under each function's `# Panics`.

## 13. Files on disk

```
 my-project/
 ├── jrs.toml                     the manifest (committed)
 ├── jrs.lock                     the resolved graph + compiler graphs (committed)
 ├── src/main/{java,kotlin,scala,groovy,resources}
 ├── src/test/{java,kotlin,scala,groovy,resources}
 └── target/                      fully disposable: `jrs clean` removes it
     ├── classes/                 main classes + resources
     ├── test-classes/
     ├── <name>-<version>.jar
     ├── lib/                     --portable
     ├── image/  jpackage/        --jlink / --jpackage
     ├── doc/                     jrs doc
     ├── test-reports/            JUnit XML
     ├── coverage/  jacoco.exec   jrs test --coverage
     ├── generated/…              by convention, task output
     └── .jrs/                    jrs's own scratch space
         ├── javac-main.args  javac-test.args  kotlinc-main.args  …
         ├── main.fingerprint  test.fingerprint
         ├── resources-main.list  resources-test.list  resources-*-generated-*.list
         ├── tasks/                 <task>.fingerprint  <task>.cp.args
         └── javadoc.args  junit-palette.properties  jpackage-input/

 shared cache  (JRS_CACHE_DIR, or ~/Library/Caches/jrs, $XDG_CACHE_HOME/jrs,
                ~/.cache/jrs, %LOCALAPPDATA%\jrs\cache)
 ├── com/google/guava/guava/33.0.0-jre/guava-33.0.0-jre.jar   Maven layout
 │     (a snapshot keeps a *.jrs-snapshot record of its build beside it)
 └── .jrs/projects                the lockfiles of every project built with it,
                                  which `jrs cache prune` keeps alive

 user config   (JRS_CONFIG, or $XDG_CONFIG_HOME/jrs/config.toml,
               ~/.config/jrs/config.toml, %APPDATA%\jrs\config.toml)
               jobs, [proxy], [mirrors], [credentials.<repo>], [jdks]
```

Nothing is written into `target/` that cannot be regenerated, so `jrs clean`
can never lose user data, and task-generated sources must live under
`target-dir` for the same reason.

## 14. How jrs itself is tested

```
 cargo test   (hermetic: no network)
 │
 ├── unit tests            #[cfg(test)] mod tests at the bottom of each module
 │
 ├── tests/output.rs       Ui::captured: fixed width, frozen clock; plain
 │                         transcripts and individual animation frames,
 │                         ASCII fallback, narrow-terminal truncation
 │
 ├── tests/resolution.rs ┐
 ├── tests/build.rs      ├─ tests/common/mod.rs: a self-cleaning Scratch dir
 ├── tests/migration.rs  ┘  and a file:// FixtureRepo
 │                           ├── tests/fixtures/repo/…/*.pom   checked in
 │                           ├── jars                           synthesised at test time
 │                           └── tests/fixtures/fake-compiler   Java classes named like
 │                                kotlinc/scalac/groovyc main classes, compiled and
 │                                published into the fixture repo, run with their
 │                                own JRS_CACHE_DIR
 │
 └── require_jdk!          tests needing javac skip loudly (SKIPPED) without one

 cargo test --features network-tests --test network
 └── tests/network.rs      the only suite that reaches Maven Central; also builds
                           one project per language with the real compilers

 cargo bench --bench resolution
 └── benches/resolution.rs SPEC §12 M5: network vs jrs vs renderer cost
```

## 15. Invariants

These are the rules the architecture above exists to keep. Breaking one is a
design regression, not a style nit.

| Invariant | Where it is enforced |
| --- | --- |
| Only `ui/` touches the terminal; no `println!` elsewhere | `ui/render.rs`, `Ui` |
| Phase lines are emitted by `cli.rs` unconditionally; live scopes only add motion | `cli.rs`, `Ui::spinner` |
| Errors are values; only the CLI renders them; exit codes 0 / 1 / 2 | `error.rs`, `cli::main` |
| Dependency arrows point toward `cli`; `ui` depends on nothing | module imports |
| `target/` is fully disposable | `project.rs`, `task.rs` checks |
| Sorted traversal, sorted jar entries, fixed timestamps: byte-identical jars | `project.rs`, `package.rs` |
| Sources and classpaths go through argfiles | `compile/`, `test.rs` |
| Nearest-wins, breadth-first by level, ties on declaration order; no ranges | `resolve/mod.rs`, `manifest.rs` |
| Atomic, checksum-verified cache writes | `resolve/cache.rs`, `resolve/repo.rs` |
| `META-INF/services/*` concatenated in fat jars | `package.rs` |
| Toolchain output passed through verbatim | `compile/`, `test.rs`, `image.rs` |
| `jrs.lock` holds no absolute paths; `manifest-checksum` triggers re-resolution | `lockfile.rs` |
| Resolution reads `effective_dependencies()`, never `dependencies` alone | `manifest.rs`, `resolve/mod.rs` |
| A compiler's graph never meets the project's | `resolve::resolve_tool` |
| Tasks are subprocesses at fixed points; built-in phases cannot be reordered | `task.rs`, `cli.rs` |

The dependency list is kept deliberately short (SPEC §13): `clap`,
`toml`+`serde`, `ureq`, `quick-xml`, `zip`, `rayon`, `thiserror`,
`sha1`/`sha2`, `terminal_size`, and `libc` on unix. The progress renderer, the
directory walk, the `jrs.toml` editor and the shell completions are hand-written
rather than pulled in. Adding a crate is a spec-level decision.
