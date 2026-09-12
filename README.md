<p align="center">
  <img src="logo.png" alt="jrs logo" width="180">
</p>

<h1 align="center">jrs</h1>

<p align="center">A JVM build system, written in Rust.</p>

<p align="center"><a href="https://getjrs.dev"><strong>getjrs.dev</strong></a></p>

<p align="center">
  <a href="https://github.com/pwittchen/jrs/actions/workflows/rust.yml"><img src="https://github.com/pwittchen/jrs/actions/workflows/rust.yml/badge.svg" alt="Rust"></a>
</p>

jrs builds, tests, runs and packages a single-module Java project from one
`jrs.toml` manifest, resolving dependencies from Maven Central. Kotlin, Scala
and Groovy sources compile alongside the Java ones. It aims for the
ergonomics of Cargo: a small manifest, a committed lockfile, one binary, and no
build script to write.

## Project status

jrs is an **experimental**, single-module build system, not a replacement for Maven
or Gradle. It implements a deliberately narrow subset of what those tools do —
there is no plugin system, no build DSL and no multi-module reactor. A project's
own build steps are [tasks](DOCS.md#tasks-and-hooks): commands jrs runs as
subprocesses at fixed points in its lifecycle, which they cannot replace or
reorder. It is offered as-is under the Apache 2.0 licence; evaluate it against
your own requirements before adopting it for production builds.

## Features

- Dependency resolution from Maven Central and other repositories, with
  nearest-wins mediation, BOMs and a committed `jrs.lock`
- Checksum-verified downloads into a shared cache, and an `--offline` mode
- Kotlin, Scala and Groovy beside Java, each compiler pinned in `jrs.lock`
- JUnit 4, 5 and 6, Spock, Kotest, ScalaTest and MUnit, with parallel test
  JVMs, retries, reports and JaCoCo coverage
- Plain, portable and fat jars, distributions, `jlink` images, `jpackage`
  installers, GraalVM native executables and ProGuard obfuscation
- User-defined tasks and lifecycle hooks, with Java tools from Maven Central
- One-shot migration from Maven and Gradle, Spring Boot builds included
- Watch mode, dependency trees, outdated reports, `jrs add` / `jrs remove`
  and shell completions

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

Windows binaries, other versions and manual installation are covered in
[DOCS.md](DOCS.md#installation).

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
Kotlin, Scala and Groovy.

## Documentation

- [DOCS.md](DOCS.md) — the full reference: the manifest, every command, tests,
  tasks, JVM languages, configuration and migration; also on the web at
  [getjrs.dev/docs](https://getjrs.dev/docs/)
- [ARCH.md](ARCH.md) — how the code is put together
- [specs/INITIAL_SPEC.md](specs/INITIAL_SPEC.md) — the design behind it
- [ROADMAP.md](ROADMAP.md) — known gaps and what comes next

## Development

```
cargo build
cargo test
```

See [DOCS.md](DOCS.md#development) for the network tests, the benchmarks and
the release process.

## Licence

Apache License 2.0. See [LICENSE](LICENSE).
