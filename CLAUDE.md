# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`jrs` is a Java build system written in Rust: it builds, tests, runs and packages a
single-module Java project from one `jrs.toml` manifest, resolving dependencies from
Maven Central. It shells out to `javac`, `java` and `jar` — it is a driver, not a
reimplementation of the JDK. Kotlin, Scala and Groovy compile alongside Java
(`specs/JVM_LANGUAGES.md`, SPEC §7.7): their compilers are resolved from Maven
Central as isolated tool graphs, pinned in `jrs.lock`'s `[[tool]]` blocks, and
run on the project's JDK — still a driver.

`specs/INITIAL_SPEC.md` is the design document the implementation follows, and module
doc comments cite it by section (`SPEC §8.2`). Read the relevant section before changing
behaviour; **§12.1 lists the four places where the code deliberately diverges from the
spec** — those divergences are intentional, don't "fix" them back.

## Commands

```
cargo build
cargo test                      # hermetic: no network, uses a file:// repo fixture
cargo fmt --check
cargo clippy --all-targets -- -D warnings
```

CI (`.github/workflows/rust.yml`) runs exactly those four on Linux, macOS and
Windows (`fmt` on Linux only), plus the network tests on Linux, on push and PR
against `master`. Changes that only touch `*.md` or `LICENSE` are excluded
via `paths-ignore` — they cannot break the build, so they do not run it. A `v*` tag
push runs the same checks, then `bump` writes the tag's version into `Cargo.toml`
and `Cargo.lock`, commits it to `master` and moves the tag onto that commit, the
`dist` matrix builds release binaries for Linux (musl), macOS and Windows from it,
and `release` publishes them. The tag must point at the tip of `master`. Don't
bump the version by hand — tagging is the release process.

Single tests and single suites:

```
cargo test --test build                             # one integration suite
cargo test --test output the_ascii_fallback         # one integration test
cargo test resolve::                                # unit tests in one module
cargo test --features network-tests --test network  # the Maven Central tests
cargo bench --bench resolution                      # SPEC §12 M5: network vs jrs vs renderer
```

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
`javac` with a staleness check, then a resource copy.

A compile unit (`compile/mod.rs`) is ordered steps that share one output
directory and one fingerprint: the compiler of the unit's other language, then
`javac` (`compile/javac.rs`); Groovy does both in one joint step. Each
language's compiler coordinate, runtime library and flags are plain data on
`enum Language` in `compile/lang.rs`. A compiler runs as `java @argfile`, with
its whole invocation — classpath, main class, flags, sources — in the file.

User-defined tasks (`[tasks]`, `[hooks]`, SPEC §7.6) are planned in `task.rs`:
ordering, cycle checks, placeholder expansion, the environment and fingerprints.
`Session` in `cli.rs` fires the hooks at their fixed points and emits the
`Task`/`Fresh` phase lines. Tasks are subprocesses; nothing a user writes runs
inside jrs, and the built-in phases cannot be reordered.

### Layer boundaries that must hold

These are the invariants the codebase is organised around; breaking one is a design
regression, not a style nit.

- **Only `ui/` touches the terminal.** Build code reports progress by mutating shared
  state that a single render thread reads. No `println!`/`eprintln!` outside `ui/`.
  This is what makes `--progress never` and the animated mode provably the same build,
  and what lets `tests/output.rs` snapshot the output without a TTY.
- **Phase lines are emitted by `cli.rs`, unconditionally.** Live scopes (`ui.spinner`,
  `ui.downloads`) only add motion on top; they never own a line that plain mode needs.
- **Errors are values.** One `JrsError` enum (`error.rs`); library code never prints,
  and only the CLI layer renders. Exit codes: `0` ok, `1` build/test failure, `2` usage
  or manifest error. No `unwrap()` outside `#[cfg(test)]`.
- **`ui` depends on nothing; `manifest`/`project`/`resolve`/`task` know nothing
  about terminals.** The dependency arrows point one way, toward `cli`.
- **`target/` is fully disposable.** Nothing is written there that cannot be
  regenerated, so `jrs clean` can never lose user data. `target/.jrs/` is jrs's own
  scratch space (argfiles, fingerprints).

### Things that are load-bearing for correctness

- **Determinism.** Directory traversal is sorted, jar entries are sorted with a fixed
  1980 timestamp and fixed permissions, and the classpath is ordered direct-then-
  transitive, each sorted by coordinate. Two builds of the same inputs produce
  byte-identical jars; keep it that way.
- **Argfiles, not command lines.** Sources and classpaths go to `target/.jrs/*.args`
  and are passed as `@argfile` — a few dozen dependencies blow past the OS argument
  limit otherwise.
- **Nearest-wins mediation, breadth-first by level** (`resolve/mod.rs`), ties broken on
  manifest declaration order — which is why `manifest.rs` parses a `toml::Table` by
  hand instead of using a `serde` derive (order preservation, per-key diagnostics,
  unknown keys as warnings). Version ranges are rejected, never guessed at.
- **Atomic, checksum-verified cache writes** (`resolve/cache.rs`): temp file in the
  destination directory, then rename, so an interrupted run cannot leave a truncated
  jar for the next build to link against.
- **Fat-jar merge rules** (`package.rs`): `META-INF/services/*` entries are
  concatenated, not overwritten — getting this wrong breaks `ServiceLoader` silently.
- **Toolchain output is passed through verbatim.** `javac` and the JUnit launcher have
  good diagnostics; jrs never reformats them, it only tears the live region down first.
- **`jrs.lock` records no absolute paths.** Cache paths are recomputed on load; the
  `manifest-checksum` field is what triggers re-resolution. It is `version = 1`
  byte for byte until a `[[tool]]` block makes it `version = 2`.
- **The implied runtime library.** Resolution and `manifest-checksum` read
  `Manifest::effective_dependencies()` — the declared dependencies plus the
  languages' runtime libraries, declared last — never `manifest.dependencies`
  alone; `render()` never writes the implied ones.
- **A compiler's graph never meets the project's.** `resolve::resolve_tool`
  resolves it alone, so the Kotlin compiler's own coroutines cannot mediate
  against the project's.

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
