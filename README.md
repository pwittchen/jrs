<p align="center">
  <img src="logo.png" alt="jrs logo" width="180">
</p>

<h1 align="center">jrs</h1>

<p align="center">A simple, fast JVM build system, written in Rust.</p>

<p align="center"><a href="https://getjrs.dev"><strong>getjrs.dev</strong></a></p>

<p align="center">
  <a href="https://github.com/pwittchen/jrs/actions/workflows/rust.yml"><img src="https://github.com/pwittchen/jrs/actions/workflows/rust.yml/badge.svg" alt="Rust"></a>
  <a href="https://github.com/pwittchen/jrs/actions/workflows/intellij.yml"><img src="https://github.com/pwittchen/jrs/actions/workflows/intellij.yml/badge.svg" alt="IntelliJ"></a>
  <a href="https://github.com/pwittchen/jrs/actions/workflows/corpus.yml"><img src="https://github.com/pwittchen/jrs/actions/workflows/corpus.yml/badge.svg" alt="Corpus"></a>
  <a href="https://github.com/pwittchen/jrs/actions/workflows/website.yml"><img src="https://github.com/pwittchen/jrs/actions/workflows/website.yml/badge.svg" alt="Website"></a>
</p>

jrs builds, tests, runs and packages a Java project from one `jrs.toml`
manifest — or a multi-module one from a manifest per module under a workspace
root — resolving dependencies from Maven Central. Kotlin, Scala
and Groovy sources compile alongside the Java ones. It aims for the
ergonomics of Cargo: a small manifest, a committed lockfile, one binary, and no
build script to write.

## Project status

jrs is an **experimental** build system, not a replacement for Maven
or Gradle. It implements a deliberately narrow subset of what those tools do —
there is no plugin system, no build DSL and no reactor: several modules build
together as a [workspace](DOCS.md#workspaces), declared rather than
configured. A project's
own build steps are [tasks](DOCS.md#tasks-and-hooks): commands jrs runs as
subprocesses at fixed points in its lifecycle, which they cannot replace or
reorder. It is offered as-is under the Apache 2.0 licence; evaluate it against
your own requirements before adopting it for production builds.

## Why jrs

- **One native binary, no daemon.** The build tool itself starts in
  milliseconds; the JVM starts only to run `javac`, your tests and your
  application.
- **Pinned and reproducible by default.** Every version and checksum sits in a
  committed `jrs.lock`, and two builds of the same inputs produce
  byte-identical jars, with no configuration.
- **No build script to audit.** `jrs.toml` is data, and there are no
  plugins. Nothing a project writes runs inside jrs; custom steps are tasks,
  run as subprocesses.
- **Packaging built in.** Fat jars with correctly merged service and Spring
  registries, `jlink` runtimes, `jpackage` installers, GraalVM native
  executables and ProGuard, without a plugin each.
- **Easy for people and for agents.** A manifest you can read in one screen,
  `jrs add` / `jrs remove`, plain output and `jrs metadata` as JSON.

It fits services in one module or a few (Spring Boot, Ktor), command-line
tools, teams for whom the JVM is not their main language, and builds with
supply-chain requirements. It does not fit library publishing, Android or
builds whose modules configure each other in code. The longer case, and where jrs is not the right choice, is at
[getjrs.dev/why](https://getjrs.dev/why/).

## Features

- Dependency resolution from Maven Central and other repositories, with
  nearest-wins mediation, BOMs and a committed `jrs.lock`
- Checksum-verified downloads into a shared cache, and an `--offline` mode
- Kotlin, Scala and Groovy beside Java, each compiler pinned in `jrs.lock`
- JUnit 4, 5 and 6, Spock, Kotest, ScalaTest and MUnit, with parallel test
  JVMs, sharding across CI machines, retries, reports and JaCoCo coverage
- Plain, portable and fat jars, distributions, `jlink` images, `jpackage`
  installers, GraalVM native executables and ProGuard obfuscation
- A CycloneDX SBOM beside the jar, a licence report, and a vulnerability
  audit against the OSV database
- User-defined tasks and lifecycle hooks, with Java tools from Maven Central:
  formatters and checkers such as google-java-format, ktfmt, Checkstyle, PMD
  and SpotBugs need no plugin
- Multi-module workspaces: members that depend on each other by path, shared
  settings, one lockfile, and every command over all members or one with `-p`
- One-shot migration from Maven and Gradle, Spring Boot and multi-module
  builds included
- Watch mode that rebuilds, retests or restarts the program, dependency
  trees, outdated reports, `jrs add` / `jrs remove` and shell completions

## Installation

jrs needs a JDK 17 or newer on `PATH`, or pointed at by `JAVA_HOME`. On macOS
or Linux:

```
curl -fsSL https://getjrs.dev/install.sh | sh
```

or, with a Rust toolchain, from source:

```
cargo install --git https://github.com/pwittchen/jrs.git
```

In GitHub Actions, [`setup-jrs`](https://github.com/pwittchen/setup-jrs)
([Marketplace](https://github.com/marketplace/actions/set-up-jrs)) installs jrs
on Linux, macOS and Windows runners and can cache its dependencies between runs:

```yaml
- uses: actions/setup-java@v6
  with: { distribution: temurin, java-version: '21' }
- uses: pwittchen/setup-jrs@v1
  with: { cache: true }
- run: jrs test
```

For tab completion of commands and flags, load the script `jrs completions`
prints from your shell's startup file:

```sh
# bash, in ~/.bashrc
source <(jrs completions bash)

# zsh, in ~/.zshrc, after compinit
source <(jrs completions zsh)
```

In zsh you can instead save it as `_jrs` in a directory on `$fpath`, before
`compinit` runs: `jrs completions zsh > ~/.zfunc/_jrs`, with
`fpath=(~/.zfunc $fpath)` in `~/.zshrc`. Fish is covered in
[DOCS.md](DOCS.md#shell-completions).

Windows binaries, other versions, manual installation and the action's inputs
are covered in [DOCS.md](DOCS.md#installation). A prebuilt jrs updates itself:
`jrs self check` says whether a newer release is out, and `jrs self update`
installs it ([Updating](DOCS.md#updating)); `jrs wrapper` pins a project to
one release, as `gradlew` does ([Pinning jrs](DOCS.md#pinning-jrs-for-a-team)).

## Getting started

```
jrs init            # scaffold jrs.toml, a starter main class and its test
jrs run             # compile and run
jrs test            # compile the tests and run them
jrs package --fat   # build a self-contained jar
```

A whole project is one manifest and a source tree:

```toml
[project]
name = "my-app"
version = "1.0.0"
main-class = "com.example.Main"

[java]
source = 21

[dependencies]
"com.google.guava:guava" = "33.0.0-jre"

[dev-dependencies]
"org.junit.jupiter:junit-jupiter" = "5.10.2"
```

To move an existing project over, run `jrs migrate` next to its `pom.xml` or
Gradle build. [`examples`](examples) holds complete sample projects in Java,
Kotlin, Scala and Groovy, and a multi-module workspace (`examples/ledger`).

## IntelliJ IDEA

[`plugins/intellij`](plugins/intellij) is an IntelliJ IDEA plugin. It imports
a jrs project from `jrs metadata` with its source roots, JDK and libraries,
and a workspace as one module per member.
Build Project runs `jrs build`, and it has run configurations for `jrs run`,
`jrs test` and `jrs task`, with the debugger and the test runner tree. It also
adds a jrs tool window and completion in `jrs.toml`. Install it from the
[JetBrains Marketplace](https://plugins.jetbrains.com/plugin/35092-jrs); each
release also carries it as `jrs-intellij.zip`, and its [README](plugins/intellij/README.md) says how
to build, install and use it.

## Documentation

- [DOCS.md](DOCS.md) — the full reference: the manifest, every command, tests,
  tasks, JVM languages, configuration and migration; also on the web at
  [getjrs.dev/docs](https://getjrs.dev/docs/)
- [ARCH.md](ARCH.md) — how the code is put together
- [SPEC.md](SPEC.md) — the design behind it
- [ROADMAP.md](ROADMAP.md) — known gaps and what comes next

## Development

```
cargo build
cargo test
```

See [DOCS.md](DOCS.md#development) for the network tests, the benchmarks and
the release process.

## Corpus

The corpus checks jrs against real projects rather than ones written for it:
a pinned list of open-source Spring Boot and Ktor projects
([`specs/corpus/corpus.toml`](specs/corpus/corpus.toml)), each taken through
`jrs migrate`, `build`, `test`, `run` and `package` and compared with the
project's own Maven or Gradle build. It clones the projects, reaches Maven
Central and keeps everything under `target/corpus/<run>/`:

```
JRS_CORPUS=all cargo bench --bench corpus                         # every project
JRS_CORPUS=petclinic-maven,ktor-serialization cargo bench --bench corpus
JRS_CORPUS=all JRS_CORPUS_STEPS=migrate,build cargo bench --bench corpus
```

Every failure it finds is recorded in
[`specs/corpus/FINDINGS.md`](specs/corpus/FINDINGS.md); see
[`specs/CORPUS.md`](specs/CORPUS.md) for the method. It also runs on demand in
CI (`.github/workflows/corpus.yml`).

## Benchmarks

### Running them by hand

The comparison builds, tests, runs and packages the same projects with jrs,
Maven and Gradle. It needs a JDK, `mvn` and `gradle` (9 or later) on `PATH`.
No timed run touches the network: one recording run fills a local mirror of
Maven Central and the Gradle Plugin Portal, and every later run reads it:

```
JRS_BENCH_RECORD=1 cargo bench --bench compare   # fill the mirror, once
cargo bench --bench compare                      # measure, into benchmarks/REPORT.md
JRS_BENCH_RUNS=1 JRS_BENCH_WARMUP=0 JRS_BENCH_PROJECTS=gen-300 \
  cargo bench --bench compare                    # one quick run of one project
```

`JRS_BENCH_TOOLS`, `JRS_BENCH_PROJECTS`, `JRS_BENCH_SCENARIOS`,
`JRS_BENCH_RUNS` (default 7), `JRS_BENCH_WARMUP` (2), `JRS_BENCH_MIRROR` and
`JRS_BENCH_REPORT` narrow or redirect it; see
[DOCS.md](DOCS.md#development) for the rest and
[`specs/BENCHMARKS.md`](specs/BENCHMARKS.md) for the method.

### Running them with Claude Code

The comparison can also be run as a Claude Code skill:

```
/run-benchmarks              # one run per cell, the parity set
/run-benchmarks full         # the harness's defaults: 7 measured runs, 2 warm-up
/run-benchmarks projects=a,b # only the projects named
```

Under the hood the skill
([`.claude/skills/run-benchmarks`](.claude/skills/run-benchmarks/SKILL.md))
checks the tree is clean, the machine is plugged in and the JDK, Maven and
Gradle match the last recorded pass. It then builds the benches, records a
fresh local Maven mirror (the only step that reaches the network) and runs
`cargo bench --bench compare` against it, keeping the report, the raw samples
and the logs under `target/bench/<stamp>/`. From the raw samples it writes the
result as a new pass in
[`specs/BENCHMARKS_SUMMARY.md`](specs/BENCHMARKS_SUMMARY.md) — the machine,
the tool versions, the commits since the previous pass and the cells that
moved — and brings the website's benchmarks page up to the new numbers. It
stops without editing anything if a project fails its checks, and it leaves
the changes uncommitted. A run takes roughly 15–30 minutes on an otherwise
idle machine.

## Licence

Apache License 2.0. See [LICENSE](LICENSE).
