<p align="center">
  <img src="logo.png" alt="jrs logo" width="180">
</p>

<h1 align="center">jrs</h1>

<p align="center">A JVM build system, written in Rust.</p>

<p align="center"><a href="https://pwittchen.github.io/jrs/">visit project website</a></p>

<p align="center">
  <a href="https://github.com/pwittchen/jrs/actions/workflows/rust.yml"><img src="https://github.com/pwittchen/jrs/actions/workflows/rust.yml/badge.svg" alt="Rust"></a>
</p>

jrs builds, tests, runs and packages a single-module Java project from one
`jrs.toml` manifest, resolving dependencies from Maven Central. Kotlin, Scala
and Groovy sources compile alongside the Java ones. It aims for the
ergonomics of Cargo: a small manifest, a committed lockfile, one binary, and no
build script to write.

## Project status

jrs is an experimental, single-module build system, not a replacement for Maven
or Gradle. It implements a deliberately narrow subset of what those tools do —
there is no plugin system, no build DSL and no multi-module reactor. A project's
own build steps are [tasks](#tasks-and-hooks): commands jrs runs as
subprocesses at fixed points in its lifecycle, which they cannot replace or
reorder. It is offered as-is under the Apache 2.0 licence; evaluate it against
your own requirements before adopting it for production builds.

## Features

- Dependency resolution against Maven Central and additional repositories, with
  transitive dependencies and nearest-wins version mediation, classifiers,
  exclusions, compile-only and runtime-only dependencies, local jar files,
  repositories confined to the groups they serve, and SNAPSHOT versions
- Checksum-verified downloads into a shared local cache, plus an `--offline`
  mode and cache pruning
- A committed `jrs.lock` for reproducible resolution
- Compilation of multi-file source trees with configurable `javac` flags, with
  the JDK pinned per project if you want
- Kotlin, Scala and Groovy beside Java, mixed both ways: each compiler resolved
  from Maven Central, pinned in `jrs.lock` and run on the project's JDK, with
  its runtime library implied
- JUnit 5 and 6 tests, and JUnit 4 through the Vintage engine, with class, tag
  and method selection, reruns of what failed, fail-fast, retries that report
  flaky tests, JUnit XML and HTML reports, and JaCoCo coverage with minimums;
  Spock, Kotest, ScalaTest and MUnit through their JUnit Platform engines
- Packaging to a plain jar, a portable jar with its `lib/`, a self-contained fat
  jar (Spring's registries merged), a zipped distribution with launch scripts,
  a `jlink` runtime image, a `jpackage` installer or a GraalVM native
  executable, plus sources and Javadoc jars and jar manifest attributes of
  your own
- Running the project's main class directly, and rebuilding on every change
- User-defined tasks and lifecycle hooks, for code generators, post-packaging
  steps and chores, run with the project's JDK and classpath
- Javadoc, dependency trees with `--why`, outdated-dependency reports, and
  `jrs add` / `jrs remove`
- Parallel resolution, download and compilation, and tests that recompile only
  when the main classes' API changes
- Progress output that adapts to the terminal: spinners, live download bars and
  a build summary, with ASCII and no-colour fallbacks
- One-shot migration from Maven (`pom.xml`) and Gradle (`build.gradle`,
  `build.gradle.kts`)
- Shell completions for bash, zsh and fish
- Per-phase build timings, a JSON project model for editors (`jrs metadata`),
  sources jars for go-to-definition (`jrs fetch --sources`), and a minimum jrs
  version per project

See [specs/INITIAL_SPEC.md](specs/INITIAL_SPEC.md) for the design behind them,
and [ARCH.md](ARCH.md) for how the code is put together.

## Requirements

A JDK 17 or newer on `PATH`, or pointed at by `JAVA_HOME`. jrs shells out to
`javac`, `java` and `jar`; it does not bundle a compiler. The Kotlin, Scala and
Groovy compilers are downloaded from Maven Central when a project turns them
on.

Building jrs from source requires a Rust toolchain with edition 2024 support; the
prebuilt binaries do not.

## Installation

### Prebuilt binaries

Every tagged [release](https://github.com/pwittchen/jrs/releases) ships a binary
for each of these platforms:

| Platform | `TARGET` |
| --- | --- |
| macOS, Apple Silicon | `aarch64-apple-darwin` |
| macOS, Intel | `x86_64-apple-darwin` |
| Linux, x86_64 | `x86_64-unknown-linux-musl` |
| Linux, ARM64 | `aarch64-unknown-linux-musl` |
| Windows, x86_64 | `x86_64-pc-windows-msvc` |

The Linux binaries are statically linked and run on any distribution. To install
the latest release into `~/.local/bin` on macOS or Linux:

```
TARGET=aarch64-apple-darwin
mkdir -p ~/.local/bin
curl -fsSL "https://github.com/pwittchen/jrs/releases/latest/download/jrs-$TARGET.tar.gz" | tar -xz -C ~/.local/bin jrs
```

Make sure `~/.local/bin` is on your `PATH`. On Windows, in PowerShell:

```
curl.exe -fsSLO https://github.com/pwittchen/jrs/releases/latest/download/jrs-x86_64-pc-windows-msvc.zip
tar -xf jrs-x86_64-pc-windows-msvc.zip jrs.exe
```

then move `jrs.exe` to a directory on your `PATH`. Each release also carries a
`SHA256SUMS` file for verifying the downloads.

### From source

```
git clone https://github.com/pwittchen/jrs.git
cd jrs
cargo install --path .
```

This places the `jrs` binary in `~/.cargo/bin`.

## Uninstallation

Delete the binary — `rm ~/.local/bin/jrs` for a prebuilt one, or

```
cargo uninstall jrs
```

if it was installed from source. This removes the binary but not the downloaded
dependencies. To reclaim that
space too, delete the [dependency cache](#dependency-cache) (for example
`rm -rf ~/Library/Caches/jrs` on macOS, or whatever `JRS_CACHE_DIR` points at).
Each project's `target/` directory is disposable and can be removed with
`jrs clean` before uninstalling, or deleted by hand afterwards.

## Getting started

```
jrs init            # scaffold jrs.toml, a starter main class and its test
jrs run             # compile and run
jrs test            # compile the tests and run them
```

`jrs init --lib` scaffolds a library instead: no main class, just a starter
class and its test. Both templates declare JUnit 5 as a dev-dependency, so the
first build downloads it. `jrs init --lang kotlin` scaffolds the same in
Kotlin, `--lang scala` in Scala with MUnit tests, and `--lang groovy` Java
code with Spock specs; see
[Kotlin, Scala and Groovy](#kotlin-scala-and-groovy).

A whole project is one manifest and a source tree:

```
my-project/
├── jrs.toml
├── jrs.lock                  # generated by resolution; commit it
├── src/
│   ├── main/java/            # production sources
│   ├── main/kotlin/          # with [kotlin]; likewise scala/ and groovy/
│   ├── main/resources/       # copied into the jar
│   └── test/java/            # test sources
└── target/                   # generated; git-ignore it
```

[`examples`](examples) holds complete sample projects for trying every command
end to end:

- [`wordstats`](examples/wordstats): Java, with Maven Central dependencies,
  resources, a `ServiceLoader` plugin and JUnit 5 tests
- [`orders`](examples/orders): Kotlin and Java calling each other, coroutines
  from a Kotlin Multiplatform library, and tests in both languages
- [`calc`](examples/calc): Scala 3 and Java calling each other, with MUnit tests
- [`cart`](examples/cart): Java code specified with Spock, Groovy on the test
  classpath only

## The manifest

```toml
[project]
name = "my-app"
version = "1.0.0"
main-class = "com.example.Main"      # required by `run` and `package --fat`

[java]
source = 21                          # -> javac --release 21
javac-args = ["-Xlint:all", "-Werror"]

[dependencies]
"com.google.guava:guava" = "33.0.0-jre"

[dev-dependencies]                   # test classpath only
"org.junit.jupiter:junit-jupiter" = "5.10.2"
```

### `[project]`

| Key | Default | Meaning |
| --- | --- | --- |
| `name` | — | Required. Also names the packaged jar. |
| `version` | — | Required. |
| `main-class` | — | Entry point for `run` and `package --fat`. |
| `source-dir` | `src/main/java` | Production sources. |
| `test-dir` | `src/test/java` | Test sources. |
| `resource-dir` | `src/main/resources` | Copied into the jar. |
| `test-resource-dir` | beside `test-dir` (`src/test/resources`) | On the test classpath only. |
| `target-dir` | `target` | Build output. |
| `jrs-version` | — | The oldest jrs that may build the project, as `"0.9"` or `"0.9.1"`. |

`jrs-version` works like Cargo's `rust-version`. An older jrs stops with exit
code `2`, says which version the project needs and where to download it, and
does not try to build a manifest it may not understand.

### `[java]`

| Key | Default | Meaning |
| --- | --- | --- |
| `source` | the toolchain's release | `javac --release`. |
| `target` | — | Set only when it differs from `source`. |
| `encoding` | `UTF-8` | `javac -encoding`. |
| `javac-args` | `[]` | Extra flags passed through verbatim. |
| `javadoc-args` | `[]` | Extra flags for `jrs doc`, such as `-Xdoclint:none`. |
| `jdk` | — | The JDK to build with, by feature version. See [Choosing the JDK](#choosing-the-jdk). |

### `[kotlin]`, `[scala]` and `[groovy]`

```toml
[kotlin]
version = "2.4.20"                    # the compiler, and the implied kotlin-stdlib
kotlinc-args = ["-Xjsr305=strict"]    # optional, appended verbatim
compiler-jvm-args = ["-Xmx2g"]        # optional, for the compiler's JVM
```

A table turns the language on. See [Kotlin, Scala and Groovy](#kotlin-scala-and-groovy).

| Key | Default | Meaning |
| --- | --- | --- |
| `version` | — | Required. The compiler, pinned: Kotlin 2.0+, Scala 2.13.9+ or 3.3+, Groovy 4.0+. |
| `source-dir` | `src/main/<lang>` | Another main source root. |
| `test-dir` | `src/test/<lang>` | Another test source root. |
| `kotlinc-args` / `scalac-args` / `groovyc-args` | `[]` | Extra compiler flags, passed through verbatim. |
| `compiler-jvm-args` | `[]` | Flags for the JVM the compiler runs in, such as `-Xmx2g` or `-Xss4m`. |

### `[run]`, `[test]` and `[package]`

```toml
[run]
jvm-args = ["-Xmx512m", "--enable-preview"]   # for `jrs run`, before -cp
java-agents = ["com.example:my-agent"]       # -javaagent:, from the resolved graph
env = { APP_MODE = "dev", DATA = "{target}/data" }
cwd = "work"                                 # relative to the project root

[test]
jvm-args = ["-Dmode=test"]                   # for the test JVM
jacoco-version = "0.8.15"                    # for `jrs test --coverage`; optional
java-agents = ["org.mockito:mockito-core"]
env = { TZ = "UTC" }
retries = 2                                  # run failed tests again; optional
coverage-minimum = { line = 0.80, branch = 0.70 }   # for `jrs test --coverage`; optional

[package]
add-modules = ["jdk.crypto.ec"]              # for --jlink / --jpackage images
native-image-args = ["--no-fallback"]        # for --native-image

[package.manifest]                           # extra MANIFEST.MF attributes
Implementation-Title = "{project.name}"
Implementation-Version = "{project.version}"
Automatic-Module-Name = "com.example.app"
```

`add-modules` names JDK modules an image needs beyond the ones `jdeps` finds
itself. `jdeps` cannot see modules that are only reached by reflection or
`ServiceLoader`, such as the TLS providers in `jdk.crypto.ec`.

`java-agents` names agents by `group:artifact`, never by path: `jrs.toml` is
committed, and the jar lives in the machine's cache. Each one is looked up in
the resolved graph, so it is the version `jrs.lock` pins, and it has to be a
dependency. `test.java-agents` looks on the test classpath, dev-dependencies
included, and `run.java-agents` on the runtime one. An agent the graph does
not hold there is a manifest error that says where to declare it. Agents go
to the JVM as `-javaagent:` ahead of every other argument, and ahead of
JaCoCo's agent under `--coverage`. Mockito 5 on JDK 21 and later needs exactly
`test.java-agents = ["org.mockito:mockito-core"]`; without it, Mockito
attaches itself at run time and the JVM warns. `run.java-agents` also goes
into the launchers of a `--jlink` or `--jpackage` image and of a `--dist`
archive, which load it from their `lib/`. An image or a distribution of a
`--fat` jar cannot carry one, and says so.

`env` adds environment variables, and `run.cwd` sets the program's working
directory, relative to the project root; without it the program runs where
jrs was started. Both take the placeholders a task does (`{target}`,
`{project.version}`, `{runtime-classpath}`, … — see
[Tasks and hooks](#tasks-and-hooks)), except `{jar}` and
`{classpath-argfile}`, which only a task has. There is no `test.cwd`: the test
JVM runs where jrs does. `env` and `cwd` belong to `jrs run` and `jrs test`,
so an image's launchers do not take them.

`[package.manifest]` adds attributes to the jar's `META-INF/MANIFEST.MF` — the
thin, portable and fat jar alike — after the ones jrs writes, in the order they
are declared, so the jar stays byte-identical from build to build. Values may
use `{project.name}` and `{project.version}`. `Main-Class`, `Class-Path`,
`Created-By`, `Manifest-Version` and `Name` belong to jrs and are refused, and
a name must be one the jar specification allows. `Implementation-Version` is
only there if you set it, as above; `Package.getImplementationVersion()` then
reads it.

`native-image-args` is passed through verbatim to `native-image` by
`jrs package --native-image`: `--no-fallback`, `--initialize-at-build-time`,
resource and reflection configuration.

### `[dependencies]` and `[dev-dependencies]`

Each entry maps a `"group:artifact"` coordinate to a version string.
`[dev-dependencies]` are on the test classpath only and are excluded from a fat
jar.

The long form is a table with a `version` and any of the following:

```toml
[dependencies]
"com.google.guava:guava" = { version = "33.0.0-jre", exclusions = ["com.google.code.findbugs:jsr305"] }
"jakarta.servlet:jakarta.servlet-api" = { version = "6.0.0", compile-only = true }
"org.postgresql:postgresql" = { version = "42.7.3", runtime-only = true }
"io.netty:netty-transport-native-epoll" = { version = "4.1.100.Final", classifier = "linux-x86_64" }
"org.lwjgl:lwjgl" = "3.3.3"
"org.lwjgl:lwjgl:natives-linux" = "3.3.3"   # a classifier in the key
```

- **`exclusions`** are `"group:artifact"` patterns, where `*` can stand for
  either half. The dependency's transitive graph stops at anything they match.
- **`compile-only = true`** means the dependency, and everything it brings in,
  is on the compile and test classpaths but not the runtime one. It stays out
  of `jrs run`, the thin jar's `Class-Path`, `lib/` and the fat jar. This is
  Maven's `provided` and Gradle's `compileOnly`.
- **`runtime-only = true`** is the reverse: the dependency, and everything it
  brings in, is on the runtime and test classpaths but not on the one the main
  sources compile against. JDBC drivers, SLF4J bindings and Logback belong
  here. It is in `jrs run`, the thin jar's `Class-Path`, `lib/`, the fat jar
  and runtime images. This is Maven's `runtime` and Gradle's `runtimeOnly`.
  The tests compile against it too, as they do in Maven. A package one path
  brings in as compile-only and another as runtime-only is needed on both
  classpaths, so it ends up as a plain dependency.
- **`classifier`** selects a file published beside the main jar, such as
  natives or a platform build. Writing the classifier in the key lets one table
  hold the same artifact with and without it.

A version ending in `-SNAPSHOT` resolves through the repository's
`maven-metadata.xml`, and its checksum is not pinned in `jrs.lock`. A cached
snapshot is re-checked once a day. When it came from a `file://` repository,
such as `~/.m2/repository`, it is re-checked on every build. `jrs update`
re-checks every snapshot at once.

A jar that is in no repository — a vendor's JDBC driver, a licensed SDK checked
into `libs/` — can be named by its path instead:

```toml
[dependencies]
ojdbc = { path = "libs/ojdbc11.jar" }
vendor-api = { path = "libs/vendor-api.jar", compile-only = true }
```

The jar has no coordinate, so its key is a name of your choosing (letters,
digits, `.`, `-` and `_`), and the path is relative to the project root. The
jar is taken as it is, with no transitive graph. `compile-only` and
`runtime-only` apply as usual, and in `[dev-dependencies]` the jar is
test-only. `jrs.lock` records the relative path and pins the jar's SHA-256.
The jar is re-hashed on every build, and one that changed under the same name
fails the build until `jrs update` pins it again. A missing jar is an error
that names its path. `jrs tree` shows it as `ojdbc = libs/ojdbc11.jar`,
`jrs remove ojdbc` removes it, and `jrs outdated` skips it.

### `[repositories]`

Additional repositories, tried in declaration order. Maven Central is implicit
and always tried last.

```toml
[repositories]
internal = "https://repo.example.com/maven2"
```

The long form confines a repository to the groups it serves:

```toml
[repositories]
internal = { url = "https://nexus.example.com/maven", groups = ["com.acme", "com.acme.*"] }
jitpack = { url = "https://jitpack.io", groups = ["com.github.someone"] }
```

`com.acme` is that group, and `com.acme.*` is every group below it. A
repository with `groups` is asked only for those groups. A group that some
repository's `groups` name is looked up only there, never in another repository
or in Maven Central. That keeps an extra public repository from answering for
your internal groups (dependency confusion), and saves a request per artifact
to repositories that cannot have it. Mirrors and credentials work as they do
for any repository.

`jrs.toml` is committed, so credentials for a private repository never go in it
— see [User configuration](#user-configuration).

## Commands

| Command | Behaviour |
| --- | --- |
| `jrs build [--watch]` | Resolve → compile main sources → copy resources. `--watch` rebuilds on every change. |
| `jrs test` | `build` + compile test sources + run the tests. The tests are not recompiled for a main change that leaves the main classes' API alone. See [Tests](#tests) for its flags. |
| `jrs run [--debug[=<port>]] [-- args...]` | `build` + run `main-class` with `args`. `--debug` waits for a debugger first; see below. |
| `jrs package` | `build` + produce `target/<name>-<version>.jar`. |
| `jrs package --portable` | Same, with the runtime dependencies copied into `target/lib/`. |
| `jrs package --fat` | Same, with every runtime dependency unpacked into the jar. |
| `jrs package --jlink` | Also build a trimmed runtime image in `target/image`, with a launcher in `bin/`. |
| `jrs package --jpackage [type]` | Also build a native package in `target/jpackage`, using jpackage's own types (`app-image`, `dmg`, `pkg`, `deb`, `rpm`, `exe`, `msi`). |
| `jrs package --sources` / `--javadoc` | Also write `target/<name>-<version>-sources.jar` / `-javadoc.jar`; `--javadoc` runs `jrs doc` first. |
| `jrs package --dist` | Also write a distribution with launch scripts in `bin/`, zipped into `target/<name>-<version>.zip`. |
| `jrs package --native-image` | Also build a native executable in `target/native` with GraalVM's `native-image`. |
| `jrs doc` | Generate Javadoc into `target/doc`. |
| `jrs clean` | Remove `target/`. |
| `jrs tree [--depth <n>] [--why <artifact>] [--tool <name>]` | Print the resolved dependency graph, every path that leads to one artifact, or a compiler's own graph (`kotlin-compiler`, `scala-compiler`, `groovy-compiler`). |
| `jrs classpath [--test \| --runtime]` | Print the resolved classpath, for editors and `java -cp "$(jrs classpath)"`. |
| `jrs update` | Re-resolve and rewrite `jrs.lock`. |
| `jrs verify` | Re-hash the cached dependency jars against the checksums in `jrs.lock`. |
| `jrs outdated` | List declared dependencies that have newer releases. |
| `jrs add <group:artifact[:version[:classifier]]>... [--dev] [--compile-only \| --runtime-only]` | Add dependencies to `jrs.toml`, at their newest release unless given a version. |
| `jrs remove <group:artifact \| name>... [--dev]` | Remove dependencies from `jrs.toml`; a local jar goes by its name. |
| `jrs cache path` / `jrs cache prune` | Print where the cache is, or remove what no project uses. See [Dependency cache](#dependency-cache). |
| `jrs init [--lib] [--lang <java\|kotlin\|scala\|groovy>] [--name <name>] [path]` | Scaffold `jrs.toml`, a starter class and its test. |
| `jrs migrate` | Generate `jrs.toml` from an existing `pom.xml` or Gradle build. |
| `jrs completions <bash\|zsh\|fish>` | Print a shell completion script. See [Shell completions](#shell-completions). |
| `jrs task <name> [--watch] [-- args...]` | Run a task from `jrs.toml`, and whatever it depends on. See [Tasks and hooks](#tasks-and-hooks). |
| `jrs task --list` | List the tasks, their descriptions and the hooks that run them. |
| `jrs metadata [--no-deps]` | Print the project model as JSON, for editors and tools. |
| `jrs fetch [--sources]` | Download the dependencies into the cache without building, and with `--sources` their `-sources.jar`s. |

Global flags: `-v/--verbose`, `-q/--quiet`, `--offline`, `-j/--jobs <n>`,
`--manifest-path <p>`, `--progress <auto|always|never>`,
`--color <auto|always|never>`, `--charset <auto|unicode|ascii>`.

Exit codes: `0` success, `1` build or test failure, `2` usage or manifest error,
`101` internal error.

`jrs package` writes a thin jar whose `Class-Path` points at the cached
dependency jars, so `java -jar` works without a classpath argument on the
machine that built it. `--portable` copies the dependencies into `target/lib/`
and points the `Class-Path` there instead, so the jar and its `lib/` can be
zipped up and shipped together. `--fat` unpacks the dependencies into the jar
itself, producing a single artifact that runs anywhere.

`--jlink` and `--jpackage` go one step further and ship a Java runtime too.
`jdeps` works out which JDK modules the application and its dependencies need,
then `jlink` builds a runtime with only those. `--jlink` leaves it in
`target/image`, with the application under `app/` and a launcher script at
`bin/<name>`. `--jpackage` hands everything to `jpackage` for a native
installer. Both use the portable layout unless `--fat` is given too. Both
require `project.main-class`.

`jrs run --debug` and `jrs test --debug` start the JVM with the JDWP agent,
suspended until a debugger attaches, and first print where to attach:
`localhost:5005`, or another port with `--debug=8000`. A bare port listens on
localhost only, as the JDK does since 9; `--debug=*:5005` listens on every
interface, for a JVM in a container. While the test JVM waits, the live test
counter stays off.

`--dist` ships the application without a runtime: the portable layout (or the
fat jar, with `--fat`) plus the launch scripts `bin/<name>` and
`bin/<name>.bat`, which run it on `$JAVA_HOME/bin/java`, or the `java` on
`PATH`, with `run.jvm-args`. It is staged in `target/dist/<name>-<version>/`,
where it runs in place, and zipped into `target/<name>-<version>.zip`,
deterministically and with the POSIX launcher kept executable.
`--native-image` builds a native executable with GraalVM's `native-image`; the
JDK jrs builds with has to be a GraalVM one, and jrs says so before building
anything when it is not. `--sources` and `--javadoc` write the jars an IDE or a
repository wants beside the main one. All of them combine with each other and
with `--fat` or `--portable`; `--dist` and `--native-image` require
`project.main-class`.

A fat jar merges what several jars register instead of letting one copy
overwrite the rest: `META-INF/services/*` files, Groovy extension modules, and
Spring's `spring.factories` (key by key), `META-INF/spring/*.imports`,
`spring.handlers`, `spring.schemas` and `spring.tooling`. The project's own copy
comes first.

`jrs add` and `jrs remove` edit `jrs.toml` in place, keeping its comments and
order. An edit that jrs cannot make safely is refused, with a message saying to
edit the file by hand. After each change the graph is resolved again, and if
that fails — a coordinate that does not exist, say — the manifest is put back
the way it was.

`--timings` on `build`, `test`, `run` and `package` prints the wall time of
each phase after the summary. The phases are resolution, downloads, each
compiler step, resources, each task, the test JVM and packaging. The same
numbers go to `target/.jrs/timings.txt` as tab-separated columns, even under
`-q`. `jrs run` prints the table before the program starts.

`jrs metadata` prints the project model as JSON on stdout, the way
`cargo metadata` does. It includes the source and resource roots for each
language, the output directories, the compile, runtime and test classpaths
(each jar with its coordinate), the JDK and `--release`, the main class and
the tasks. It resolves the dependencies but never compiles. `--no-deps` skips
resolution. [SPEC §5.4](specs/INITIAL_SPEC.md#54-the-project-model-jrs-metadata-and-jrs-fetch)
has the full schema. `jrs fetch` downloads everything a build needs without
building, which warms a CI cache for `--offline` runs. `jrs fetch --sources`
also downloads each dependency's `-sources.jar`, and `jrs metadata` then lists
it next to the jar, so an editor can go to a library's definitions. A library
that publishes no sources jar is only a warning.

## Dependency cache

Downloaded artifacts are stored in a shared, Maven-layout cache outside the
project:

| Platform | Location |
| --- | --- |
| macOS | `~/Library/Caches/jrs` |
| Linux and other Unix | `$XDG_CACHE_HOME/jrs`, else `~/.cache/jrs` |
| Windows | `%LOCALAPPDATA%\jrs\cache` |

Set `JRS_CACHE_DIR` to override it — useful for CI, where the cache is worth
persisting between runs. Writes are atomic and checksum-verified, so the cache
is safe to share between concurrent builds.

`jrs.lock` pins a checksum for every jar. A download is checked against it as
well as against the repository's own `.sha1`, so a repository that starts
serving different bytes under the same version fails the build. Jars already in
the cache are not re-hashed on every build; `jrs verify` does that on demand.

The cache only grows until it is pruned:

```
jrs cache path                        # where it is
jrs cache prune --dry-run             # what would go
jrs cache prune                       # drop what no project's jrs.lock names
jrs cache prune --unused-for 30       # drop what no build has used in 30 days
```

Every project jrs builds is recorded in the cache, so a plain `prune` keeps
exactly what those projects' lockfiles name, their pinned compilers included.
It also drops the parent POMs, BOMs, test launchers and JaCoCo jars that only
a fresh resolution or a `--coverage` run needs; those are downloaded again
when they are next wanted.
`--unused-for` goes by when each artifact was last used. jrs records that in
the file's access time at most once a day, so it works even on filesystems
mounted `noatime`.

## User configuration

Settings that belong to a person or a machine rather than to the project live
in one file per user:

| Platform | Location |
| --- | --- |
| Linux, macOS and other Unix | `$XDG_CONFIG_HOME/jrs/config.toml`, else `~/.config/jrs/config.toml` |
| Windows | `%APPDATA%\jrs\config.toml` |

Set `JRS_CONFIG` to use another file. Every setting is optional, and a missing
file is not an error.

```toml
jobs = 8                                  # default for --jobs

[proxy]
url = "http://proxy.example.com:3128"
no-proxy = ["localhost", ".internal.example.com"]

[mirrors]                                 # repository name → URL to use instead
central = "https://nexus.example.com/repository/maven-central"

[credentials.internal]                    # a repository name from jrs.toml
username = "ci"
password-env = "NEXUS_PASSWORD"           # or `password`, `token`, `token-env`

[jdks]                                    # JDKs jrs would not find on its own
21 = "/opt/jdks/temurin-21"
```

- **Credentials** are sent as HTTP basic auth (`username` + `password`) or as a
  bearer token (`token`). The `-env` variants read the secret from an
  environment variable instead of the file. `JRS_REPO_<NAME>_USERNAME` and
  `JRS_REPO_<NAME>_PASSWORD`, or `JRS_REPO_<NAME>_TOKEN`, take precedence over
  the file — `<NAME>` is the repository name upper-cased, with every other
  character turned into `_` (`my-repo` → `JRS_REPO_MY_REPO_TOKEN`).
- **Mirrors** redirect a repository, Maven Central included, to another URL.
  The key `*` mirrors every repository that has no mirror of its own. Mirrors
  do not change `jrs.lock`.
- **Proxy**: without a `[proxy]` table, `HTTPS_PROXY`, `HTTP_PROXY`,
  `ALL_PROXY` and `NO_PROXY` from the environment apply.
- **JDKs**: `[jdks]` maps a Java feature version to a JDK home, for a
  project that pins a version installed somewhere jrs does not look.

A dropped connection or an HTTP 429/5xx from a repository is retried twice,
with a backoff, before the build fails. A 404 or a 401 is believed the first
time.

## Choosing the JDK

By default jrs builds with the JDK at `JAVA_HOME`, or failing that the `javac`
on `PATH`. A project can pin a version instead, in any of three places,
checked in this order:

- `jdk = 21` under `[java]` in `jrs.toml`
- a `.java-version` file, as written by jenv, asdf or mise (`21`, `temurin-21.0.2`)
- a `.sdkmanrc` file with a `java=21.0.2-tem` line

A pinned version is looked up in the user configuration's `[jdks]` table
first, then at `JAVA_HOME` / `PATH`, then among the JDKs installed in the
usual places:
- SDKMAN!, asdf, mise, IntelliJ and Gradle toolchains in your home directory
- `/Library/Java/JavaVirtualMachines`, `/usr/lib/jvm`, `/usr/java` and `/opt/java`
- `Program Files` on Windows
- `JAVA_HOME_<version>_<arch>`, as exported by `actions/setup-java`

When several builds of that version are installed, the newest wins. If none
is found, the error lists the versions that were found. `jrs -v build` names
the JDK it picked.

## Tests

`jrs test` runs the JUnit Platform console launcher, which jrs resolves itself:

- **JUnit 5 and 6.** Declare `org.junit.jupiter:junit-jupiter`. The launcher
  version follows from it.
- **JUnit 4.** Declare `junit:junit`, and the tests run on the Vintage engine
  that the launcher bundles. With both declared, both kinds run.
- **Spock, Kotest, ScalaTest and MUnit.** Declare the framework: each runs on
  its own JUnit Platform engine (MUnit on Vintage), and the launcher follows
  the platform version the framework brings. With tests that are not all Java,
  classes named `*Spec` and `*Suite` run as well as `*Test`.

| Flag | Effect |
| --- | --- |
| `--filter <regex>` | Only classes whose name matches. |
| `--include-tag <expr>` / `--exclude-tag <expr>` | JUnit tag expressions; repeatable. |
| `--method <class#method>` | Only this method, e.g. `com.example.FooTest#adds`; repeatable. |
| `--coverage` | Record coverage with JaCoCo and write a report to `target/coverage` (HTML, plus `jacoco.xml`). |
| `--watch` | Test again on every change. |
| `--debug[=<port>]` | Wait for a debugger on port 5005, or `<port>`, or `<host>:<port>`, before the tests start. |
| `--rerun-failed` | Only the tests that failed in the last run, read from its JUnit XML. |
| `--fail-fast` | Stop at the first failing test. |
| `--retries <n>` | Run failing tests again up to `n` times; overrides `[test] retries`. |

JUnit XML reports land in `target/test-reports`, where CI systems look for
them. `[test] jvm-args` sets the test JVM's arguments, and `[test] env` and
`java-agents` its environment and agents (see
[`[run]`, `[test]` and `[package]`](#run-test-and-package)).

Jupiter runs tests in parallel inside the one test JVM when asked to, and
asking goes through `jvm-args`:

```toml
[test]
jvm-args = [
    "-Djunit.jupiter.execution.parallel.enabled=true",
    "-Djunit.jupiter.execution.parallel.mode.default=concurrent",
]
```

A `junit-platform.properties` in `src/test/resources` with the same keys does
the same.

Every run that leaves XML also gets `target/test-reports/index.html`, a static
page with one row per class. Classes with failures come first and unfolded,
with each failure's stack trace; the rest are folded. The page is written for
a failing run too, and `jrs test` prints its path.

`--rerun-failed` runs what failed last time. A plain test method is selected
by name; one invocation of a parameterised test, or one dynamic test, by its
JUnit unique ID; and a test the XML gives nothing narrower for, by its class.
When nothing failed it says so and exits `0`; with no earlier run at all it
exits `2`.

`[test] retries = n` runs failing tests again, up to `n` times, each attempt
in a launcher of its own that writes its XML into
`target/test-reports/retry-<n>/`. A test that passes on a retry does not fail
the run, but it is not counted as passed either: it is reported as flaky, by
name and in the `Finished` line, and marked on the page. `--debug` turns
retries off, since each would wait for a debugger again.

`--fail-fast` passes the launcher's own `--fail-fast` on JUnit 6. The 1.x
launchers that JUnit 5 and 4 run on do not have one, so there jrs shows the
launcher's test feed instead of its tree and stops the launcher itself after
the first failure; that run has no XML report. Launchers older than 1.10
(JUnit 5.9 and before) cannot be followed that way, and run every test. A run
stopped early is not retried.

`[test] coverage-minimum` makes `jrs test --coverage` exit `1` when a
project-wide total falls short, naming the total and the minimum. It takes
ratios from 0 to 1 for any of JaCoCo's counters: `line`, `branch`,
`instruction`, `method`, `class` and `complexity`. Without `--coverage` it is
ignored.

## Kotlin, Scala and Groovy

A `[kotlin]`, `[scala]` or `[groovy]` table compiles that language alongside
Java:

```toml
[kotlin]
version = "2.4.20"
```

```
src/main/kotlin/com/example/Main.kt      # fun main() at file level: main-class = "com.example.MainKt"
src/main/java/com/example/Legacy.java    # may use the Kotlin classes, as they may use it
src/test/kotlin/com/example/MainTest.kt
```

- **The compiler is jrs's dependency, not yours.** It is resolved from Maven
  Central as a graph of its own, pinned in `jrs.lock` beside yours, and run on
  the project's JDK. `jrs tree --tool kotlin-compiler` shows its graph, and
  `jrs outdated` lists newer compiler releases.
- **The runtime library is implied**: `kotlin-stdlib`, `scala-library` /
  `scala3-library_3`, or `groovy`, at the compiler's version. You don't declare
  it, and if you do, your declaration wins. Groovy declared under
  `[dev-dependencies]` stays off the runtime classpath and out of the jar,
  which is what a project with only Spock tests wants.
- **Mixed sources work both ways.** Kotlin and Scala compile first, reading
  the Java sources for their symbols, and then `javac` compiles the Java ones
  against their classes; Groovy compiles both itself. A compile unit mixes Java
  with at most one other language, but main and test are separate units, so
  Kotlin code with Spock tests is fine. A `.kt` file without a `[kotlin]`
  table is an error, not a file silently skipped.
- **One release for everything.** `java.source`, or the JDK's version, is
  passed to every compiler, so a jar never mixes bytecode levels.
- Kotlin tests see the main code's `internal` declarations. A `main` at file
  level in `Main.kt` compiles to `MainKt`, and `jrs run` says so when
  `main-class` names `Main`.
- `jrs doc` documents the Java sources only. Compiler plugins (Kotlin's
  `allopen`, `spring`, `serialization`), kapt/KSP and Kotlin Multiplatform
  projects are not supported. A Multiplatform library whose root artifact
  does not resolve to classes needs its `-jvm` artifact declared instead.

The design is [specs/JVM_LANGUAGES.md](specs/JVM_LANGUAGES.md).

## Tasks and hooks

A task is a command jrs runs for you: a code generator before `javac`, a
checksum after the jar, a chore on demand. Tasks are declared in `jrs.toml`,
and `[hooks]` attaches them to the build:

```toml
[tasks.build-info]
description = "Generate BuildInfo.java"
script = "build/GenerateBuildInfo.java"            # a Java file, run with the project's JDK
args = ["{target}/generated/sources", "{project.version}"]
inputs = ["build/GenerateBuildInfo.java"]
outputs = ["{target}/generated/sources"]
source-outputs = ["{target}/generated/sources"]    # compiled with the main sources

[tasks.checksum]
description = "Write a SHA-256 next to the jar"
shell = "shasum -a 256 \"$JRS_JAR\" > \"$JRS_JAR.sha256\""

[tasks.release]
description = "Package, then checksum"
depends-on = ["package", "checksum"]

[hooks]
pre-compile = ["build-info"]
post-package = ["checksum"]
```

```
jrs task release                # run a task, and whatever it depends on
jrs task build-info --watch     # again whenever its inputs change
jrs task --list                 # every task, its description and hooks
```

Each task has one action:

- **`run`** is a program and its arguments, passed to the OS without a shell,
  so it behaves the same everywhere. `java` in a task is the project's JDK.
- **`shell`** is a string for `sh -c`, or `cmd /C` on Windows. Pipes and
  redirects work, but it is **not portable**: the same string goes to a
  different shell on each platform.
- **`script`** is a `.java` file run with the project's JDK, no compile step
  needed. It is the portable way to write build logic.

A task with no action, like `release`, only runs its `depends-on`. That list
names other tasks, or `build`, `test`, `package` and `doc`, which run as their
commands would. Each task, and each built-in, runs at most once per command.

| Hook | Runs |
| --- | --- |
| `pre-compile` | Before the main sources are compiled. For code generation. |
| `post-compile` | After the main classes and resources are in `target/classes`. |
| `pre-test` | Before the test sources are compiled. |
| `post-test` | After the tests, if they passed. |
| `post-package` | After the jar is written. |
| `pre-run` | Before `jrs run` starts the program. |

Commands include each other, so `jrs package` runs the compile hooks too. A
hook runs every time its point is reached. A task that declares both `inputs`
and `outputs` is skipped, printing `Fresh`, while neither has changed; a task
without them always runs.

Placeholders such as `{root}`, `{target}`, `{classes}`, `{project.version}`,
`{classpath}` (what `jrs classpath` prints) and `{jar}` are expanded in `run`,
`args`, `cwd` and `env`. `shell` strings use environment variables instead:
`JRS_ROOT`, `JRS_CLASSPATH`, `JRS_JAR` and the rest, with `JAVA_HOME` set to
the project's JDK; a `shell` task's `args` arrive as `$1`, `$2`…. The full
list is in [SPEC §7.6](specs/INITIAL_SPEC.md#76-tasks-and-hooks).

Generated sources and resources must live under `target-dir`, since `jrs
clean` must never delete anything you wrote and watch mode must not rebuild on
a generator's own output. `source-outputs` of a `pre-compile` task are compiled
with the main sources, and those of a `pre-test` task with the tests.

A hook or dependency that fails stops the build with exit code `1`, after its
own output. `jrs task <name>` returns the task's own exit code, as `jrs run`
returns the program's.

**Tasks run code.** `jrs build` on a freshly cloned project runs whatever its
`[hooks]` name, as `gradle build` or `npm install` would. Read a project's
`jrs.toml` before building it if you do not trust it. `tree`, `classpath`,
`update`, `verify`, `outdated`, `add`, `remove`, `cache`, `init`, `migrate`,
`completions` and `clean` never run a task, so inspecting a project with them
is always safe.

## Annotation processors

jrs has no separate processor path. Configuring processors is a
[non-goal](specs/INITIAL_SPEC.md#12-non-goals). But `javac` runs any processor it finds on
the compile classpath, so Lombok, MapStruct, Dagger and the like work as
compile-only dependencies:

```toml
[java]
javac-args = ["-proc:full"]        # needed from JDK 23 on

[dependencies]
"org.projectlombok:lombok" = { version = "1.18.34", compile-only = true }
```

Since JDK 23, `javac` only runs processors found on the classpath when given
`-proc:full`. JDK 21 and 22 accept the flag and don't need it. JDK 17 rejects
it, so leave it out there. `jrs migrate` translates `annotationProcessor` and
`<annotationProcessorPaths>` into exactly this.

## Shell completions

```
source <(jrs completions bash)          # in ~/.bashrc
source <(jrs completions zsh)           # in ~/.zshrc, after compinit
jrs completions fish | source           # in ~/.config/fish/config.fish
```

## Migrating an existing project

```
jrs migrate --dry-run      # see the manifest that would be written
jrs migrate                # write jrs.toml; the original build file is untouched
```

`--from <system>` forces the source build system instead of detecting it,
`--force` overwrites an existing `jrs.toml`, and `--path <dir>` selects a project
root other than the current directory.

Migration is a one-shot, best-effort translation. It prints a report in three
blocks — what was migrated, what needs review, and what was skipped, each with a
reason. Gradle migration in particular reads the declarative subset of a build
script by pattern rather than by running Gradle, and says so.

Gradle tasks that say everything in literals become `[tasks]`: an `Exec` task
a `run` command, a `JavaExec` over the main runtime classpath a `java` command
over jrs's classpath, and a task with only `dependsOn` an aggregate.
`compileJava.dependsOn`, `test.dependsOn`, `run.dependsOn` and `jar.finalizedBy`
become `[hooks]`. A task with a `doLast { }` closure, another type such as
`Copy`, or a value built from a variable is listed with the reason, to rewrite
by hand as a `run`, `shell` or `script` task.

`environment` and `workingDir` on Gradle's `run` and `test` tasks become
`run.env`, `run.cwd` and `test.env` when they are literals. A `-javaagent:` in
`jvmArgs` is a path into Gradle's cache, so it is reported, except for
Mockito's own recipe, which becomes
`test.java-agents = ["org.mockito:mockito-core"]`.

A `jar { manifest { attributes(...) } }` block, and Maven's
`<manifestEntries>`, become `[package.manifest]` when the values are literals
or the project's version. `withSourcesJar()` and `withJavadocJar()` are
reported: in jrs they are the `jrs package --sources` and `--javadoc` flags.

Scopes keep their meaning. Maven's `provided` and Gradle's `compileOnly` become
`compile-only`, and `runtime` and `runtimeOnly` become `runtime-only`. Gradle's
`files(...)` and `fileTree(...)` become local jars, with a `fileTree` expanded
to the jars it holds when you migrate. A repository's `content { includeGroup }`
or `exclusiveContent` filter becomes its `groups`.

Kotlin, Scala and Groovy builds migrate too. The Kotlin Gradle plugin,
`id 'groovy'` and `id 'scala'`, and Maven's `kotlin-maven-plugin`,
`scala-maven-plugin` and `gmavenplus-plugin` become language tables. An
explicit standard-library dependency is dropped, since it is implied now.
Compiler plugins are reported as not migrated.

## Roadmap

Known gaps and features worth building next are tracked in
[ROADMAP.md](ROADMAP.md).

## Development

```
cargo build
cargo test
cargo run
```

[ARCH.md](ARCH.md) describes the architecture: the modules and how they
depend on each other, how a command flows through them, dependency resolution,
compile units, the output layer, and the invariants the code is organised
around.

`cargo test` is hermetic: dependency resolution is exercised against a `file://`
repository fixture rather than the network. The tests that do reach Maven Central
live behind a feature flag:

```
cargo test --features network-tests --test network
```

The integration tests build real Java fixture projects, so they need a JDK; they
announce that they were skipped if there is none.

CI runs `cargo fmt --check` (on Linux), `cargo clippy --all-targets -- -D
warnings`, `cargo build` and `cargo test` on Linux, macOS and Windows, plus the
network tests on Linux, on every push and pull request against `master` that
touches more than Markdown or `LICENSE`.

The M5 benchmark from SPEC §12 resolves a graph of about twenty artifacts from a
local repository that adds latency to every request. It runs with
`--progress never` and with `--progress always`, and prints how much of the
wall time is the network, how much is jrs, and what the animated renderer
costs:

```
cargo bench --bench resolution
JRS_BENCH_LATENCY_MS=50 JRS_BENCH_RUNS=10 cargo bench --bench resolution
```

To cut a release, tag the tip of `master` and push the tag:

```
git tag v0.2.0
git push origin v0.2.0
```

CI runs the same checks, writes the version into `Cargo.toml` and `Cargo.lock`,
commits that to `master`, then builds the binaries for every platform above from
that commit and publishes them as a GitHub release under the tag. The tag stays on
the commit you pushed it on; run `git pull` afterwards to pick up the version
commit.

## Licence

Apache License 2.0. See [LICENSE](LICENSE).
