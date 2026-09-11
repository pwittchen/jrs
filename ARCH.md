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
│   ├── abi.rs         class-file API digests: compile avoidance, per-class reads
│   ├── incremental.rs file-by-file compilation of a Java unit: <unit>.index
│   ├── javac.rs       javac and javadoc
│   ├── doc.rs         Scaladoc and Groovydoc
│   └── lang.rs        enum Language: Kotlin/Scala/Groovy as plain data
├── resolve/
│   ├── mod.rs         breadth-first walk, nearest-wins mediation, Resolution
│   ├── coord.rs       coordinates, scopes, Maven version ordering
│   ├── pom.rs         POM XML → effective model (parents, BOMs, properties)
│   ├── metadata.rs    maven-metadata.xml: version lists, snapshot builds
│   ├── repo.rs        Fetcher: cache → repositories, checksums, retries
│   └── cache.rs       the local store: layout, atomic writes, pruning
├── test.rs            the JUnit Platform console launcher, one or several; JaCoCo
├── test_report.rs     JUnit XML read back: the HTML page, reruns, retries
├── package.rs         thin / portable / fat / sources jars; merge rules; zip writing
├── image.rs           jdeps, jlink, jpackage
├── dist.rs            --dist: launch scripts, the staged directory, the zip
├── native_image.rs    --native-image: GraalVM's native-image, by argfile
├── obfuscate.rs       --obfuscate: ProGuard over the assembled jar, by config file
├── relocate.rs        [package.relocate]: a fat jar's packages moved, constant pools rewritten
├── runner.rs          `jrs run`: the user's program gets the terminal
├── task.rs            [tasks] and [hooks]: plan, cycles, placeholders, freshness
├── migrate/
│   ├── mod.rs         detection, report, manifest emission
│   ├── maven.rs       pom.xml → Manifest (reuses resolve::pom); exec-maven-plugin → [tasks]
│   ├── maven_profiles.rs  the <profiles> a plain `mvn` build activates, merged in
│   ├── gradle.rs      build.gradle[.kts] → Manifest (pattern extraction)
│   ├── gradle_files.rs  files() / fileTree() → local jars, literal ones only
│   ├── gradle_repos.rs  repositories { }, content filters → groups
│   └── gradle_tasks.rs  Gradle tasks → [tasks] and [hooks], literal ones only
├── completions.rs     bash/zsh/fish scripts generated from the clap definition
├── timings.rs         --timings: the per-phase recorder, and timings.txt
├── model.rs           the project model `jrs metadata` prints
├── json.rs            a small JSON writer (no serde_json), for the model
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
  `runner`, `image`, `native_image`) only so they can tear the live region down before a
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
           │                           verify · outdated · metadata · fetch
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
 ├── task_tools : OnceCell<Vec<(String, Resolution)>>
 │                                          each task's own dependencies' graph
 ├── task_classpaths : RefCell<HashMap<…>>  the task graphs downloaded so far
 ├── built      : OnceCell<Built>           the result of build()
 ├── ran        : RefCell<HashSet<String>>  tasks already run or found fresh
 ├── done       : RefCell<HashSet<Builtin>> built-ins a depends-on already ran
 ├── jar        : RefCell<Option<PathBuf>>  set once package has written it
 └── timings    : Timings                   one row per leaf phase, for --timings
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
 │  --rerun-failed: the last run's XML ─► what to select, before it is replaced
 │  build()
 │  hook(pre-test)
 │  compile_unit("test")  classpath = target/classes + test classpath,
 │                        main_api = api_digest(target/classes)
 │  sync test resources
 │  fetch_internal: JUnit console launcher (+ JaCoCo agent/cli with --coverage)
 │  test::run  ─► java … ConsoleLauncher --scan-class-path …   (+ test.env)
 │    or, with test.forks: the classes dealt out, test::run_forks, one launcher
 │    each at once, output held and passed through whole, XML moved up
 │  retries (test.retries): what still fails, one launcher each
 │  test_report: XML read back ─► flaky count, test-reports/index.html
 │  coverage report (even when tests failed), then test.coverage-minimum
 └  hook(post-test)      only if the tests passed and met the minimums

 package()                                                  jrs package
 │  build()
 │  write_thin_jar | copy_libraries + write_thin_jar | write_fat_jar
 │  images: jdeps ─► jlink / jpackage        (--jlink / --jpackage)
 └  hook(post-package)

 run_command()                                              jrs run
 │  build()
 │  hook(pre-run)
 └  runner::run_main ─► java … <main-class> args   (stdio inherited;
                                                      run.env, in run.cwd)
```

Every phase line (`Resolving`, `Downloading`, `Compiling`, `Fresh`, `Testing`,
`Packaging`, `Running`, `Task`, `Finished`, …) is printed by these methods,
unconditionally.
The spinner or download bars around a phase are a separate `LiveScope` that
only adds motion. See [§11](#11-the-output-layer).

The same methods time their phases. As each leaf phase ends — resolution,
downloads, each compile step (`compile::compile_timed` hands them back),
resources, each task, the test JVM, packaging, the image tools — `Session`
records a row in its `Timings` (`timings.rs`). Rows are recorded whatever the
flags; only `--timings` reports them, from `build_command`, `test_command` and
`package_command` after the summary, and from `run_command` before the
program starts. The report is the table through `Ui::timings` and a copy in
`target/.jrs/timings.txt`. A nested phase never gets a row of its own, so the
rows do not overlap.

Two commands use the spine without building. `jrs fetch` is `dependencies()`,
the test launcher, each task's own tools (`task_classpath`) and optionally
every `-sources.jar`
(`resolve::fetch_sources`). `jrs metadata` is `dependencies()` (unless
`--no-deps`) and `toolchain()` handed to `model::metadata`, which turns them
and the manifest into a `json::Json` document printed on stdout.

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
                 └─────────────┬─────────────┘           and per task with tools
                               │ no                              │
                               ▼                                 │
                 resolve::resolve(manifest)       ─┐             │
                 resolve::resolve_tool(compiler)   │ "Resolving" │
                   one isolated graph per language │             │
                 resolve::resolve_tool_dependencies│             │
                   one per task with [tasks.x.     │             │
                   dependencies], `tasks.x`       ─┘             │
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
block makes it `version = 2`. A task's graph is downloaded with the compilers'
when it is resolved afresh, so that `jrs.lock` pins its jars; read from
`jrs.lock`, it waits until the task runs (`Session::task_classpath`).

Local jars (`name = { path = "libs/x.jar" }`, SPEC §8.8) have no coordinate, so
they never enter the walk. `Resolution::local` holds them, `jrs.lock` writes
them as `[[local]]` blocks (the manifest's relative path and a `sha256` pin, no
new version), and `resolve::attach_local` finds each under the project root and
re-hashes it on every build — `resolve` itself, or `dependencies()` for a
lockfile. A jar that no longer matches its pin fails until `jrs update`.

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

`[managed]` (SPEC §8.9) is read before the walk: `managed_versions_with`
takes the table's own versions, then each BOM's effective `<dependencyManagement>`
(its own imports folded in), the first to name an artifact winning. A
versionless root gets its version there, or resolution fails naming it. In the
walk, `admissible` gives a child the managed version in place of the one its
POM asks for, so two paths to it agree before nearest-wins has anything to
mediate; a version the manifest declares still wins for its own dependency,
at depth 1. A package at its managed version is marked `managed`, which
`jrs.lock` records and `jrs tree` shows.

A package's `Classpath` is a set of places its jar goes, and widening is their
union (`Classpath::join`): `compile` (main compile, runtime, tests),
`provided` (compile-only: main compile and tests), `runtime` (runtime-only:
runtime and tests) and `test`. Reached as both `provided` and `runtime`, a
package is `compile`. The main sources' compilers get `compile` and `provided`;
`jrs run`, packaging and `jdeps` get `compile` and `runtime`; the test
classpath gets everything.

A compiler's graph goes through the same function via `resolve_tool`, and a
task's own dependencies via `resolve_tool_dependencies`, but each as a
separate resolution: the Kotlin compiler's own `kotlinx-coroutines` must
never mediate against the project's, nor a formatter's Guava. `[managed]`
does not reach them.

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
   └─► for repo in [repositories…] (mirrors applied), then Maven Central —
       only those whose `groups` claim the group, or, when none does, those
       without `groups` (repo::repositories_for):
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

A compile unit (`compile/mod.rs`) is the main sources or the test sources. Its
steps share one output directory, one fingerprint and one staleness decision.
A stale unit with another language is compiled whole, from an emptied output
directory, so a deleted source cannot leave its class behind. A Java-only
unit compiles file by file when its index allows it (below), and whole
otherwise.

```
 CompileUnit { sources, output_dir, classpath, release, javac flags,
               foreign: Option<ForeignCompiler> }
      │
      ▼
 is_stale?  ── fingerprint differs (flags, every jar's size+mtime, source list;
      │        for the tests, the main classes' API digest)
      │        or a source's contents differ from <unit>.index
      │        (no index: newest source is newer than newest class)
      │
      ├── Java only, same settings, <unit>.index  ──►  file by file (below)
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
             and main.index (Java only)                  and main.index
```

`compile/incremental.rs` keeps `target/.jrs/<unit>.index`, written after every
successful Java-only build. It records each source's size, mtime and hash, the
classes it compiled to (tied to it by their `SourceFile` attribute), each
class's API and constants digests (`abi::class_info`), and the unit's classes
it refers to. With unchanged settings the next build works from it:

```
 changed sources (by hash)   none ─► refresh the mtimes, done
      │ a source added or deleted ─► whole unit
      ▼
 delete every class no unchanged source owns
 javac @javac-main.args  changed sources, target/classes first on -cp
      │
      ▼
 compare with the index   a top-level class came or went,
      │                   a constant changed, a class with   ─► whole unit
      │                   no single source
      │ API unchanged ─► write index, done
      ▼
 every source that refers to a changed class, transitively
 (a subclass passes inherited changes on), less those compiled
      │
 delete their classes; javac over them; write index, done

 javac fails ─► index marks every source it was given as changed; no fingerprint
```

It is only tried when nothing else can write into the unit. An annotation
processor or `javac` plugin, a `module-info.java`, or a Kotlin, Scala or
Groovy step keeps the unit whole, and so does a processor registered on the
classpath or in the output directory, which is on the classpath during a
partial run. A compile-time constant is the one API change that leaves no
reference behind, since `javac` inlines it, so it compiles the unit whole.
`tests/build.rs` checks that the classes built file by file equal a whole
build's, byte for byte.

What jrs knows about each language — file extension, source roots, compiler
coordinate and main class, implied runtime library, generated flags — is plain
data matched on `enum Language` in `compile/lang.rs`. There is no registry and
no trait object. A unit mixes Java with at most one other language; main and
test are separate units, so Kotlin code with Spock tests is fine.

Every compiler is run as `java @argfile` (or `javac @argfile`), never with a
long command line: a few dozen dependency jars exceed the OS argument limit.
Compiler output is passed through verbatim.

The test unit compiles against `target/classes`, which is a directory, not a
jar, so its size and mtime say nothing. What its fingerprint holds instead is
`compile::api_digest` of it (`compile/abi.rs`): a hand-written class-file
reader renders what another unit's `javac` can see and hashes that, with
constant-pool indices resolved. That means flags, supertypes, own nesting,
and non-private members with their signatures, annotations and constant
values. Bodies, private members and anonymous classes are left out. A class
counts by its raw bytes instead when that is not enough: Kotlin and Scala
classes, `module-info`, anything unreadable, and every class once the main
classes register an annotation processor or a Groovy AST transformation.

```
 main change ─► compile main ─► api_digest(target/classes)
                                      │
                    same as in test.fingerprint? ── yes ─► tests "fresh"
                                      │ no
                                      ▼
                               compile the tests
```

`jrs doc` picks its tool the same way a unit picks its steps: `javadoc`
(`compile/javac.rs`) for Java and Kotlin units, and for Scala and Groovy the
language's own (`compile/doc.rs`), from `Language::doc_tool`. Scaladoc 2 runs
on the compiler's graph; Scala 3's scaladoc and Groovydoc are graphs of their
own, resolved by `resolve_tool` when `jrs doc` runs and not pinned. Each runs
as `java @target/.jrs/<tool>.args` into `target/doc`.

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
 java [-agentlib:jdwp=…]              --debug
      [-javaagent:<jar>…]             test.java-agents, from the resolved graph
      [-javaagent:jacoco…]            --coverage
      [jvm-args] -cp … ConsoleLauncher [execute]
      --scan-class-path target/test-classes   (or --select-method,
                                               --select-unique-id and
                                               --select-class, which
                                               replace the scan)
      --include-classname … --include-tag … --exclude-tag …
      --reports-dir target/test-reports       (retry-<n>/ for a retry)
      [--fail-fast]                           (6.x launchers only)
        │  environment: jrs's own + test.env
        ├── stdout/stderr passed through verbatim
        ├── each line read to advance the live counter (TestState),
        │   except under --debug, when the JVM waits for a debugger
        └── the launcher's summary block parsed for the authoritative totals
```

The launcher version is derived from the Jupiter (or other engine) version the
project declares. JUnit 4 runs on the Vintage engine the launcher bundles.

`jrs run` builds its command line the same way: `runner::jvm_prefix` puts the
JDWP agent and then the java agents (`runner::java_agents` looks each
`group:artifact` up in the resolution: the test classpath for `test`, the
runtime classpath for `run`) ahead of `jvm-args`. `run.env`, `test.env` and
`run.cwd` are the tasks' `env` and `cwd`, parsed by the same code in
`manifest.rs` and expanded by `task::jvm_env` / `task::jvm_cwd`, and
`toolchain::Environment` applies them to the process.

When the launcher exits, `test_report.rs` reads its XML back: the per-engine
`TEST-*.xml` files, with each test's unique ID taken from its `<system-out>`,
and any retries folded in over them. A run split among forks leaves the same
shape: each fork writes into `fork-<k>/`, and `test::run_forks` moves its
files up as `TEST-<engine>-fork-<k>.xml` beside the others.

```
 test-reports/TEST-*.xml ────────┐
 test-reports/retry-<n>/TEST-*.xml ┴─► test_report::load ─► Results
                                          (a failure that passed on a
                                           retry becomes Flaky)
        ┌──────────────────────────────────────┼───────────────────────────┐
        ▼                                      ▼                           ▼
 select(still failing):               Flaky lines, and the        index.html: a row
   --select-method Class#m(params)    Finished line's count       per class, failures
   else --select-unique-id            (cli.rs)                    first with their
   else --select-class                                            traces, the rest
 for --rerun-failed and test.retries                              folded
```

`--fail-fast` is the launcher's own from JUnit 6 on (`TestRun::fail_fast_mode`).
A 1.x launcher has none, and prints its tree only once the run is over, so
from 1.10 on jrs asks it for `--details=testfeed`, reads that through
`toolchain::run_streaming_until`, and kills the launcher when the test after
the first failure starts. That run has no summary block and no XML. Before
1.10 there is no feed either, and `--fail-fast` is ignored with a warning.
A retry is the first run's `TestRun`, cloned with
its selectors swapped, its reports sent to `retry-<n>/` and the JaCoCo agent
set to `append=true`.

## 9. Packaging

```
                              target/classes
                                    │
          ┌─────────────────────────┼──────────────────────────┐
          ▼                         ▼                          ▼
     thin (default)            --portable                    --fat
  Class-Path: absolute     deps copied to target/lib/   deps unpacked into
  paths into the cache     Class-Path: lib/<file>.jar   one jar; project
                                    │                   classes win conflicts;
  each gets [package.manifest]      │                   services and Spring
  after jrs's own attributes        │                   registries merged
                                    ├──────────────────────────┤
                                    ▼                          ▼
                  --jlink / --jpackage / --dist use the portable layout,
                         or the fat jar when --fat is given too
                                    │
             ┌──────────────────────┴───────────────────────┐
             ▼                                              ▼
   jdeps ─► module list (+ add-modules)      --dist: jar (+ lib/) + bin/<name>,
             │                               bin/<name>.bat (java from JAVA_HOME
     ┌───────┴──────────┐                    or PATH) ─► target/dist/<n>-<v>/
     ▼                  ▼                               ─► target/<n>-<v>.zip
 jlink ─► target/image  jpackage ─► target/jpackage

 beside any of them:
   --sources       main roots + generated sources ─► <n>-<v>-sources.jar
   --javadoc       jrs doc ─► target/doc ─► <n>-<v>-javadoc.jar
   --native-image  classes + runtime classpath ─► native-image (the GraalVM
                   JDK's bin/, by argfile) ─► target/native/<name>
   --obfuscate     the assembled jar ─► ProGuard (Maven Central, pinned; run as
                   java @args over a config file) ─► same jar, renamed
```

All jars are **deterministic**: entries sorted, a fixed 1980 timestamp, fixed
permissions, so two builds of the same inputs are byte-identical. The fat jar
is written straight from the dependency jars without a staging directory, and
its merge rules are load-bearing: `META-INF/services/*` files are concatenated
(overwriting them breaks `ServiceLoader` silently), Groovy extension-module
descriptors are merged into one, and Spring's registries are merged —
`spring.factories` key by key, `META-INF/spring/*.imports` as a union of lines,
`spring.handlers`, `spring.schemas` and `spring.tooling` concatenated. The
project's own copy of each comes first. The distribution zip is written the
same way — sorted, fixed timestamp, fixed modes, `0755` only for the POSIX
launcher — and `javadoc` runs with `-notimestamp`, so the sources, Javadoc and
distribution archives are byte-identical across builds as well.

`[package.relocate]` lives in `relocate.rs` and applies to the fat jar only,
as it is written: an entry's name is relocated when it is planned, so a moved
class that collides with another is reported like any duplicate, and each
class's constant pool and each registry's class names are relocated when the
entry is written. Every name a class file holds is a `CONSTANT_Utf8` entry
that the rest of the file refers to by index, so rewriting those entries'
contents — never their number or order — relocates the class while the bytes
after the pool are copied through. A name is recognised at the start of an
entry or after a descriptor's `L`, and only in or under the relocated package.
The main class moves with its package. `jrs run` and `jrs test` run the
unrelocated classpath.

`--dist` and `--native-image` live in `dist.rs` and `native_image.rs`. The
launchers reuse `image.rs`'s quoting; `native-image` is found beside `javac`,
and its absence means the JDK is not GraalVM, which is reported before the
build starts.

`--obfuscate` lives in `obfuscate.rs` and runs last, over the assembled jar, so
it composes with the merge rules above instead of redoing them. ProGuard is
resolved as an isolated tool graph and pinned in `jrs.lock` as the
`obfuscator` `[[tool]]` whenever `[obfuscate]` is present — like a compiler, so
the lockfile is stable whether or not the flag is passed — and downloaded when
`--obfuscate` first reaches it. jrs writes a ProGuard config file
(`target/.jrs/obfuscate.pro`) and runs `java -cp <graph> proguard.ProGuard
@obfuscate.pro`; the launcher leaves that trailing `@file` for ProGuard because
it follows the main class. The config keeps the entry point and every
`META-INF/services/` provider by name, adds `[obfuscate].keep` and
`proguard-args`, and disables shrinking and optimization so behaviour and
timing are untouched — only names change and debug information is stripped.
ProGuard writes a new jar that jrs renames over the input, so an interrupted
run leaves the plain jar intact.

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
             │                task_classpath ─► its own tool graph, when   │
             │                                  it has dependencies        │
             │                task::prepare ─► placeholders expanded,      │
             │                                  env (JRS_*, JAVA_HOME),    │
             │                                  <name>.tool.args written,  │
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

A task's action is `run`, `shell`, `script` or `main`. `main` runs a class
from the task's own `[tasks.<name>.dependencies]` (TASKS.md §8): `cli.rs`
hands `task::prepare` their classpath, which it writes to
`target/.jrs/tasks/<name>.tool.args` and passes as `java @<argfile> <class>`;
a `script` with dependencies gets the same argfile ahead of its file.

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

The `--timings` table is one more composite output, like the summary.
`Ui::timings` takes rows of labels and `Duration`s that `Session` collected,
and `render_timings` turns them into lines. That function is pure: its
columns depend on the labels alone. The lines go to stderr in every mode
except quiet, so `tests/output.rs` snapshots the table with fixed durations,
the same way it snapshots the summary.

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
 ├── jrs.lock                     the resolved graph + compiler / obfuscator graphs (committed)
 ├── src/main/{java,kotlin,scala,groovy,resources}
 ├── src/test/{java,kotlin,scala,groovy,resources}
 └── target/                      fully disposable: `jrs clean` removes it
     ├── classes/                 main classes + resources
     ├── test-classes/
     ├── <name>-<version>.jar
     ├── <name>-<version>-sources.jar  <name>-<version>-javadoc.jar
     │                            --sources / --javadoc
     ├── <name>-<version>.zip     --dist, zipped from dist/<name>-<version>/
     ├── lib/                     --portable
     ├── dist/                    --dist: the staged distribution, bin/ launchers
     ├── image/  jpackage/        --jlink / --jpackage
     ├── native/                  --native-image
     ├── doc/                     jrs doc
     ├── test-reports/            JUnit XML, index.html; retry-<n>/ per retry
     ├── coverage/  jacoco.exec   jrs test --coverage
     ├── generated/…              by convention, task output
     └── .jrs/                    jrs's own scratch space
         ├── javac-main.args  javac-test.args  kotlinc-main.args  …
         ├── main.fingerprint  test.fingerprint
         ├── main.index  test.index  per source: classes, API digests, references
         ├── resources-main.list  resources-test.list  resources-*-generated-*.list
         ├── tasks/                 <task>.fingerprint  <task>.cp.args
         │                          <task>.tool.args
         ├── javadoc.args  scaladoc.args  groovydoc.args
         ├── junit-palette.properties  jpackage-input/
         │   native-image.args
         ├── obfuscate.pro  obfuscate.args  obfuscated.jar   --obfuscate
         └── timings.txt            --timings: phase<TAB>ms, the last run's

 shared cache  (JRS_CACHE_DIR, or ~/Library/Caches/jrs, $XDG_CACHE_HOME/jrs,
                ~/.cache/jrs, %LOCALAPPDATA%\jrs\cache)
 ├── com/google/guava/guava/33.0.0-jre/guava-33.0.0-jre.jar   Maven layout
 │     (a snapshot keeps a *.jrs-snapshot record of its build beside it)
 │     (and guava-33.0.0-jre-sources.jar, once `jrs fetch --sources` ran)
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
 │                                kotlinc/scalac/groovyc and proguard.ProGuard main
 │                                classes, compiled and published into the fixture repo,
 │                                run with their own JRS_CACHE_DIR
 │
 └── require_jdk!          tests needing javac skip loudly (SKIPPED) without one

 cargo test --features network-tests --test network
 └── tests/network.rs      the only suite that reaches Maven Central; also builds
                           one project per language with the real compilers

 cargo bench --bench resolution
 └── benches/resolution.rs SPEC §12 M5: network vs jrs vs renderer cost

 cargo bench --bench incremental
 └── benches/incremental.rs SPEC §7.2: rebuilds after one edit, javac's share
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
| Fat-jar merge rules: `META-INF/services/*`, Groovy extension modules and Spring's registries merged, never overwritten; the project's copy first | `package.rs` |
| Relocation rewrites only a class's `CONSTANT_Utf8` entries, never their number or order; a class it cannot read fails the jar | `relocate.rs` |
| Toolchain output passed through verbatim | `compile/`, `test.rs`, `image.rs` |
| `jrs.lock` holds no absolute paths; `manifest-checksum` triggers re-resolution | `lockfile.rs` |
| Resolution reads `effective_dependencies()`, never `dependencies` alone | `manifest.rs`, `resolve/mod.rs` |
| A managed version replaces what a POM asks for, before mediation; a declared version still wins for its own dependency | `resolve::admissible`, `resolve::with_managed_versions` |
| A compiler's, a task's or the obfuscator's graph never meets the project's | `resolve::resolve_tool`, `resolve::resolve_tool_dependencies` |
| Obfuscation runs after packaging, over the assembled jar, keeping the entry point and `ServiceLoader` names; it changes names, not behaviour or timing | `obfuscate.rs`, `cli.rs` |
| A group a repository's `groups` claim is looked up nowhere else | `resolve::repo::repositories_for` |
| Tasks are subprocesses at fixed points; built-in phases cannot be reordered | `task.rs`, `cli.rs` |

The dependency list is kept deliberately short (SPEC §13): `clap`,
`toml`+`serde`, `ureq`, `quick-xml`, `zip`, `rayon`, `thiserror`,
`sha1`/`sha2`, `terminal_size`, and `libc` on unix. The progress renderer, the
directory walk, the `jrs.toml` editor and the shell completions are hand-written
rather than pulled in. Adding a crate is a spec-level decision.
