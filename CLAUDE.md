# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`jrs` is a Java build system written in Rust: it builds, tests, runs and packages a
Java project from one `jrs.toml` manifest — or a workspace of several, one per module
under a root `jrs.toml` that lists them (`specs/WORKSPACES.md`) — resolving
dependencies from Maven Central. It shells out to `javac`, `java` and the other JDK tools — it is a
driver, not a reimplementation of the JDK. Kotlin, Scala and Groovy compile alongside
Java (`specs/JVM_LANGUAGES.md`, SPEC §7.7): their compilers are resolved from Maven
Central as isolated tool graphs, pinned in `jrs.lock`'s `[[tool]]` blocks, and
run on the project's JDK — still a driver.

`SPEC.md` is the design document the implementation follows, and module
doc comments cite it by section (`SPEC §8.2`). Read the relevant section before changing
behaviour; **§12.1 lists the four places where the code deliberately diverges from the
spec** — those divergences are intentional, don't "fix" them back.

## Commands

```
cargo build
cargo test                      # hermetic: no network, uses a file:// repo fixture
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo lint                      # clippy + the pedantic group (alias in .cargo/config.toml)
```

**After implementing each task, run `cargo lint`.** If it reports any errors or
warnings, fix them — in the code where the lint has a point, or with a local
`#[allow(..., reason = "...")]` where the code is deliberate — and run it again
until it is clean. Only then mark the task as done.

CI (`.github/workflows/rust.yml`) runs exactly those four on Linux, macOS and
Windows (`fmt` on Linux only), plus the network tests on Linux, on push and PR
against `master`. Its `reproducible` job packages every `examples/` project
from two checkouts at different paths and compares what was written byte for
byte: a compiler or writer that records a path or a time fails there. Changes that only touch `*.md`, `LICENSE` or `website/` are
excluded via `paths-ignore` — they cannot break the build, so they do not run it;
neither do changes under `plugins/`. `.github/workflows/intellij.yml` tests and
builds the IntelliJ plugin with Gradle on changes to it or to a `jrs.toml` under
`examples/` or `tests/fixtures`, and `rust.yml` calls it on a release. A `v*` tag
push runs the same checks, then `bump` writes the tag's version into `Cargo.toml`
and `Cargo.lock` and commits it to `master` (the tag itself is never moved), the
`dist` matrix builds release binaries for Linux (musl), macOS and Windows (x86_64
and ARM64) from that commit, `plugin` builds the IntelliJ plugin from it (at the
`pluginVersion` in `plugins/intellij/gradle.properties`, which is set by hand for
now: `bump` leaves it alone),
and `release` publishes them, the plugin as `jrs-intellij.zip`, and writes the
Homebrew, Scoop and winget manifests from their checksums
(`packaging/manifests.sh`, kept as the `package-manifests` artifact; publishing
those is manual). The tag must point at the tip of `master`. Don't
bump the version by hand — tagging is the release process. `setup-jrs/action.yml`
is the GitHub Action that installs a release (`uses: pwittchen/jrs/setup-jrs@<tag>`);
it, `website/install.sh` and `packaging/manifests.sh` all depend on the asset names.
`.github/workflows/corpus.yml` runs the corpus (below) on demand only
(`workflow_dispatch`, with `projects` and `jdk` inputs) and puts its
`REPORT.md` in the job summary; nothing calls it, so no release waits on a
third party's repository.

`.github/workflows/website.yml` builds `website/` with Bun and rsyncs it to
`getjrs.dev` on the mikr.us VPS (the `VPS_*` repository secrets) on pushes to
`master` that touch `website/` or `logo.png`. `rust.yml` also calls it after
`release`, with the version commit and `secrets: inherit`, since that commit is
pushed with the workflow token and triggers nothing itself — so the site's
version stays current. The site also serves `website/install.sh` as
`getjrs.dev/install.sh`, the `curl | sh` installer for the release binaries; it
fetches `releases/latest/download/jrs-<target>.tar.gz` and `SHA256SUMS`, so the
asset names in `dist` must stay unversioned. `website/docs/index.html` is the
user documentation at `getjrs.dev/docs/`, written from `DOCS.md` and the
specs: a change to a command, a flag or a manifest key belongs there too.
`README.md` is a short overview that links into `DOCS.md`, the full reference;
keep the details in `DOCS.md`.

Single tests and single suites:

```
cargo test --test build                             # one integration suite
cargo test --test output the_ascii_fallback         # one integration test
cargo test resolve::                                # unit tests in one module
cargo test --features network-tests --test network  # the Maven Central tests
cargo bench --bench resolution                      # SPEC §12 M5: network vs jrs vs renderer
cargo bench --bench incremental                     # SPEC §7.2: rebuild time after one edit, javac's share
JRS_BENCH_PROJECT=<path> cargo bench --bench test_jvm  # SPEC §10.2: share-classes off/true/"aot" on a real project
JRS_CORPUS=all cargo bench --bench corpus       # ROADMAP §1: real projects vs their own Maven/Gradle build
JRS_BENCH_RECORD=1 cargo bench --bench compare  # ROADMAP §2: fill the local mirror the comparison reads
cargo bench --bench compare                     # ROADMAP §2: jrs vs Maven vs Gradle, into benchmarks/REPORT.md
```

Only the corpus (`docs/corpus/corpus.toml`, `specs/todo/CORPUS.md`),
`tests/network.rs` and the comparison's recording run may reach Maven
Central. The corpus also clones its
projects and runs their own builds, and keeps everything under
`target/corpus/<run>/`; `docs/corpus/FINDINGS.md` is the ledger of what it found.
The comparison (`benches/compare.rs`, `specs/todo/BENCHMARKS.md`) serves every
tool from a local mirror (`benches/support/mirror.rs`, which `resolution.rs`
shares) and refuses to time a project the tools do not build alike; the
hand-written Maven and Gradle builds of its parity set are in
`benchmarks/projects/`, and `benchmarks/REPORT.md` is generated, never edited.
`.github/workflows/benchmarks.yml` runs it on demand only, on JDK 21 and 25,
and commits nothing.

Driving jrs against a Java project:

```
cargo run -- --manifest-path /path/to/project build
cargo run -- run -- arg1 arg2
```

A JDK 17+ must be on `PATH` or at `JAVA_HOME`. Tests that need one skip loudly via the
`require_jdk!` macro rather than failing, so a green `cargo test` on a machine without
`javac` does not mean the build pipeline was exercised — check the `SKIPPED` lines.

Set `JRS_CACHE_DIR` to relocate the shared artifact cache (`~/Library/Caches/jrs` on
macOS) when experimenting.

## Git

- **Never push.** Pushing is manual and stays the maintainer's decision — do not run
  `git push`, and do not open or merge pull requests. Committing locally when asked is
  fine; getting the commit onto a remote is not.
- **No AI attribution in commit messages.** No `Co-Authored-By: Claude`, no
  "Generated with Claude Code" trailer, no tool mention in the subject or body. Write
  the message as the change itself warrants.

## Architecture

**`ARCH.md` is the full architecture map** — module layers, the `Session` spine,
resolution, compile units, the output layer, on-disk layout, with ASCII diagrams.
Read it before a change that crosses module boundaries, and keep it in step when
one moves a boundary, a phase or a file under `target/`. What follows is the
summary.

Library-first: everything lives in `src/lib.rs` modules; `main.rs` is five lines of
`std::process::exit(jrs::cli::main())`. Every phase can be driven from a test without
spawning the CLI, and the integration tests do exactly that.

```
jrs.toml ──parse──► Manifest ──► Project (layout, source globbing)
                        │
                        └──► resolve ──► jrs.lock ──► Classpath
                                                │
Project + Classpath ──► compile ──► target/classes ──► package | runner | test
```

`cli.rs` owns dispatch. `Session` holds one command's manifest, UI and clock, and
`Session::build()` is the shared spine of `build`/`test`/`run`/`package`:
`dependencies()` (lockfile or fresh resolution → cache lookup → downloads) then
the compile unit with a staleness check, then a resource copy.

A compile unit (`compile/mod.rs`) is ordered steps that share one output
directory and one fingerprint: the compiler of the unit's other language, then
`javac` (`compile/javac.rs`); Groovy does both in one joint step. Each
language's compiler coordinate, runtime library and flags are plain data on
`enum Language` in `compile/lang.rs`. A compiler runs as `java @argfile`, with
its whole invocation — classpath, main class, flags, sources — in the file.

A workspace (`workspace.rs`, SPEC §1.2, `specs/WORKSPACES.md`) is loaded before
any `Session`: members inherit the root's shared tables, module dependencies are
edges between members, and the whole workspace resolves once as an aggregate
manifest whose session owns the root `jrs.lock`. `cli/members.rs` then gives each
member a `Session` of its own, preset with its share of that graph and its upstream
members' class directories, and runs the command member by member in build order.

User-defined tasks (`[tasks]`, `[hooks]`, SPEC §7.6) are planned in `task.rs`:
ordering, cycle checks, placeholder expansion, the environment and fingerprints.
`Session` in `cli.rs` fires the hooks at their fixed points and emits the
`Task`/`Fresh` phase lines. Tasks are subprocesses; nothing a user writes runs
inside jrs, and the built-in phases cannot be reordered.

### Layer boundaries that must hold

These are the invariants the codebase is organised around; breaking one is a design
regression, not a style nit.

- **Only `ui/` touches the terminal.** Build code reports progress by mutating shared
  state that a single render thread reads. No `println!`/`eprintln!` outside `ui/`
  and tests.
  This is what makes `--progress never` and the animated mode provably the same build,
  and what lets `tests/output.rs` snapshot the output without a TTY.
- **Phase lines are emitted by `cli.rs`, unconditionally.** Live scopes (`ui.spinner`,
  `ui.downloads`) only add motion on top; they never own a line that plain mode needs.
- **Errors are values.** One `JrsError` enum (`error.rs`); library code never prints,
  and only the CLI layer renders. Exit codes: `0` ok, `1` build/test failure, `2` usage
  or manifest error. No `unwrap()` outside `#[cfg(test)]`, except on lock poisoning.
- **`ui` depends on nothing; `manifest`/`project`/`resolve`/`task` know nothing
  about terminals.** The dependency arrows point one way, toward `cli`.
- **`target/` is fully disposable.** Nothing is written there that cannot be
  regenerated, so `jrs clean` can never lose user data. `target/.jrs/` is jrs's own
  scratch space (argfiles, fingerprints).

### Things that are load-bearing for correctness

- **Determinism.** Directory traversal is sorted, jar entries are sorted with a fixed
  1980 timestamp and fixed permissions, and the classpath is ordered direct-then-
  transitive, each sorted by coordinate. Two builds of the same inputs produce
  byte-identical jars; keep it that way. Every package is also a directory entry
  of its own (`package.rs`, `directories_of`), written before what it holds:
  without it `ClassLoader.getResources("com/example")` finds nothing, and every
  classpath scanner — Spring's component scan first among them — comes up empty.
- **Argfiles, not command lines.** Sources and classpaths go to `target/.jrs/*.args`
  and are passed as `@argfile` — a few dozen dependencies blow past the OS argument
  limit otherwise.
- **Compile avoidance errs towards recompiling.** The test unit's fingerprint holds
  `compile::api_digest` of `target/classes` (`compile/abi.rs`): what another unit's
  `javac` can see, never method bodies. Kotlin and Scala classes count by their
  bytes, and so does every class once the main classes carry an annotation
  processor or a Groovy AST transformation. When in doubt, a class counts by its
  bytes: a spurious recompile costs seconds, a stale test class costs a wrong result.
  The same rule governs file-by-file compilation inside a Java-only unit
  (`compile/incremental.rs`, `target/.jrs/<unit>.index`): a body change compiles
  its own source, an API change also compiles everything that transitively
  refers to it, and anything the index cannot account for — a new or deleted
  source, a changed constant, a processor, another language — compiles the unit whole.
  Test selection follows it too (`compile/impact.rs`, `target/.jrs/test.tested`):
  `jrs test` runs the test classes a changed class reaches through the constant
  pools, and anything it cannot vouch for — a resource, an added class, a class
  only reflection could reach — runs the whole suite.
- **Nearest-wins mediation, breadth-first by level** (`resolve/mod.rs`), ties broken on
  manifest declaration order — which is why `manifest.rs` parses a `toml::Table` by
  hand instead of using a `serde` derive (order preservation, per-key diagnostics,
  unknown keys as warnings). Version ranges are rejected, never guessed at.
- **Atomic, checksum-verified cache writes** (`resolve/cache.rs`): temp file in the
  destination directory, then rename, so an interrupted run cannot leave a truncated
  jar for the next build to link against.
- **Fat-jar merge rules** (`package.rs`): `META-INF/services/*` entries are
  concatenated, not overwritten — getting this wrong breaks `ServiceLoader` silently.
  Spring's registries get the same care — `spring.factories` merged key by key,
  `META-INF/spring/*.imports` as a union of lines, `spring.handlers`/`schemas`/
  `tooling` concatenated — since an overwrite there loses auto-configuration
  without an error. The project's own copy always comes first.
- **Relocation rewrites names, nothing else** (`relocate.rs`, `[package.relocate]`,
  fat jar only). A class is relocated by rewriting its constant pool's
  `CONSTANT_Utf8` entries in place — never their number or order — so every index
  stays valid and the rest of the class file is copied through. A class it cannot
  read fails the jar rather than shipping unrelocated.
- **A build-cache key holds no absolute path and no mtime** (`build_cache.rs`,
  `CompileUnit::cache_text`): sources by relative path and hash, jars by
  coordinate and pinned checksum, paths as `{root}`/`{cache}`. That is what
  lets two checkouts share entries; a path in it costs only hits, but an input
  left out of it serves wrong classes. A unit that cannot be keyed compiles,
  and no cache failure fails a build. The in-process `jrs()` test helper
  passes `--no-build-cache`, since it shares the user's cache.
- **The `--watch` `javac` worker never outlives the command** (`compile/worker.rs`):
  it is a child reading a pipe jrs holds, and any failure of it falls back to
  a forked `javac`. A process that outlives the command is the compiler daemon
  SPEC §1.2 rules out.
- **Toolchain output is passed through verbatim.** `javac` and the JUnit launcher have
  good diagnostics; jrs never reformats them, it only tears the live region down first.
- **`jrs.lock` records no absolute paths.** Cache paths are recomputed on load; the
  `manifest-checksum` field is what triggers re-resolution. It is `version = 1`
  byte for byte until a `[[tool]]` block makes it `version = 2`.
- **The implied runtime library.** Resolution and `manifest-checksum` read
  `Manifest::effective_dependencies()` — the declared dependencies plus the
  languages' runtime libraries, declared last — never `manifest.dependencies`
  alone; `render()` never writes the implied ones.
- **A tool's graph never meets the project's.** `resolve::resolve_tool`
  resolves a compiler alone, and `resolve::resolve_tool_dependencies` a task's
  `[tasks.<name>.dependencies]` (a `[[tool]]` block named `tasks.<name>`), so
  the Kotlin compiler's own coroutines cannot mediate against the project's.
  `[managed]` does not reach them either.
- **A workspace resolves once.** A member never resolves on its own: its graph is
  its share of the aggregate's (`Workspace::project`), so two members cannot link
  against two versions of one library. The aggregate resolves with
  `resolve::resolve_workspace`, which re-walks a package reached by a path that
  excludes less, so one member's exclusion never drops a jar another needs. A member's main unit fingerprints its
  upstream members' `api_digest`, as the test unit does the main classes'. A
  single-module project, its lockfile and its `jrs metadata` stay byte for byte
  what they were; a workspace's lockfile is `version = 3`.
- **A managed version is settled before mediation.** `[managed]` (SPEC §8.9),
  its own entries first and then each BOM's, replaces the version any POM asks
  for, in `resolve::admissible`; a version declared in `[dependencies]` still
  wins for its own dependency. A dependency written without a version (`{}`,
  `Dependency::is_managed`) takes it from there. A manifest without the table
  keeps its `manifest-checksum` and its lockfile byte for byte.

## Tests

- **Unit tests** live inline as `#[cfg(test)] mod tests` at the bottom of each module.
- **`tests/output.rs`** drives the real output layer with a fixed width and a frozen
  clock, asserting both the plain transcript and individual animation frames. ASCII
  fallback and narrow-terminal truncation have dedicated cases — they are exactly the
  paths a developer on a Unicode terminal never hits by hand.
- **`tests/resolution.rs`, `tests/build.rs`, `tests/migration.rs`** run against
  `tests/common/mod.rs` scaffolding: a self-cleaning `Scratch` dir and a `file://`
  `FixtureRepo`. The fixture's POMs are checked in under `tests/fixtures/repo`; the
  jars beside them are synthesised at test time, so nothing binary is committed and
  the default test run never touches the network. Add new resolution cases by
  publishing into the fixture, not by reaching for Maven Central.
- **Kotlin, Scala and Groovy** run end to end in `tests/build.rs` against fake
  compilers: `tests/fixtures/fake-compiler` holds Java classes named like the
  real compilers' main classes, compiled and published into the fixture repo
  at test time. Those tests drive the jrs binary with their own
  `JRS_CACHE_DIR`, so fake artifacts never reach the user's cache.
- **`tests/workspace.rs`** drives the jrs binary over workspaces — build order,
  compile avoidance across members, `-p`, one lockfile, fat jars carrying
  siblings, cross-member tasks — each test with its own `JRS_CACHE_DIR`.
  Workspace migration has its fixtures in `tests/fixtures/migrate/*-workspace`,
  an `expected-jrs.toml` beside each build file.
- **`tests/network.rs`** is the only suite allowed to hit Maven Central, behind the
  `network-tests` feature. It also builds one project per language with the
  real compilers, which is the only check of their determinism.

## Dependencies

The crate list is deliberately minimal and was argued through in SPEC §13: `clap`,
`toml`+`serde`, `ureq` (blocking, rustls), `quick-xml`, `zip`, `rayon`, `thiserror`,
`sha1`/`sha2`, `terminal_size`, `libc` on unix. Nothing is `async` — `rayon` plus
blocking IO, since downloads dominate. No `indicatif`, no `console`, no `walkdir`: the
progress UI and the directory walk are hand-written on purpose. So are the
`jrs.toml` editor behind `jrs add`/`remove` (`edit.rs`) and the shell completions
(`completions.rs`): no `toml_edit`, no `clap_complete`. Adding a dependency is
a spec-level decision.
