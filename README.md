<p align="center">
  <img src="logo.png" alt="jrs logo" width="180">
</p>

<h1 align="center">jrs</h1>

<p align="center">A Java build system, written in Rust.</p>

<p align="center">
  <a href="https://github.com/pwittchen/jrs/actions/workflows/rust.yml"><img src="https://github.com/pwittchen/jrs/actions/workflows/rust.yml/badge.svg" alt="Rust"></a>
</p>

jrs builds, tests, runs and packages a single-module Java project from one
`jrs.toml` manifest, resolving dependencies from Maven Central. It aims for the
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
  exclusions, compile-only dependencies and SNAPSHOT versions
- Checksum-verified downloads into a shared local cache, plus an `--offline`
  mode and cache pruning
- A committed `jrs.lock` for reproducible resolution
- Compilation of multi-file source trees with configurable `javac` flags, with
  the JDK pinned per project if you want
- JUnit 5 and 6 tests, and JUnit 4 through the Vintage engine, with class, tag
  and method selection, JUnit XML reports and JaCoCo coverage
- Packaging to a plain jar, a portable jar with its `lib/`, a self-contained fat
  jar, a `jlink` runtime image or a `jpackage` installer
- Running the project's main class directly, and rebuilding on every change
- User-defined tasks and lifecycle hooks, for code generators, post-packaging
  steps and chores, run with the project's JDK and classpath
- Javadoc, dependency trees with `--why`, outdated-dependency reports, and
  `jrs add` / `jrs remove`
- Parallel resolution, download and compilation
- Progress output that adapts to the terminal: spinners, live download bars and
  a build summary, with ASCII and no-colour fallbacks
- One-shot migration from Maven (`pom.xml`) and Gradle (`build.gradle`,
  `build.gradle.kts`)
- Shell completions for bash, zsh and fish

See [specs/INITIAL_SPEC.md](specs/INITIAL_SPEC.md) for the design behind them.

## Requirements

A JDK 17 or newer on `PATH`, or pointed at by `JAVA_HOME`. jrs shells out to
`javac`, `java` and `jar`; it does not bundle a compiler.

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
first build downloads it.

A whole project is one manifest and a source tree:

```
my-project/
├── jrs.toml
├── jrs.lock                  # generated by resolution; commit it
├── src/
│   ├── main/java/            # production sources
│   ├── main/resources/       # copied into the jar
│   └── test/java/            # test sources
└── target/                   # generated, git-ignored
```

[`examples/wordstats`](examples/wordstats) is a complete sample project, with
Maven Central dependencies, resources, a `ServiceLoader` plugin and JUnit 5
tests, for trying every command end to end.

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

### `[java]`

| Key | Default | Meaning |
| --- | --- | --- |
| `source` | the toolchain's release | `javac --release`. |
| `target` | — | Set only when it differs from `source`. |
| `encoding` | `UTF-8` | `javac -encoding`. |
| `javac-args` | `[]` | Extra flags passed through verbatim. |
| `javadoc-args` | `[]` | Extra flags for `jrs doc`, such as `-Xdoclint:none`. |
| `jdk` | — | The JDK to build with, by feature version. See [Choosing the JDK](#choosing-the-jdk). |

### `[run]`, `[test]` and `[package]`

```toml
[run]
jvm-args = ["-Xmx512m", "--enable-preview"]   # for `jrs run`, before -cp

[test]
jvm-args = ["-Dmode=test"]                   # for the test JVM
jacoco-version = "0.8.15"                    # for `jrs test --coverage`; optional

[package]
add-modules = ["jdk.crypto.ec"]              # for --jlink / --jpackage images
```

`add-modules` names JDK modules an image needs beyond the ones `jdeps` finds
itself. `jdeps` cannot see modules that are only reached by reflection or
`ServiceLoader`, such as the TLS providers in `jdk.crypto.ec`.

### `[dependencies]` and `[dev-dependencies]`

Each entry maps a `"group:artifact"` coordinate to a version string.
`[dev-dependencies]` are on the test classpath only and are excluded from a fat
jar.

The long form is a table with a `version` and any of the following:

```toml
[dependencies]
"com.google.guava:guava" = { version = "33.0.0-jre", exclusions = ["com.google.code.findbugs:jsr305"] }
"jakarta.servlet:jakarta.servlet-api" = { version = "6.0.0", compile-only = true }
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
- **`classifier`** selects a file published beside the main jar, such as
  natives or a platform build. Writing the classifier in the key lets one table
  hold the same artifact with and without it.

A version ending in `-SNAPSHOT` resolves through the repository's
`maven-metadata.xml`, and its checksum is not pinned in `jrs.lock`. A cached
snapshot is re-checked once a day. When it came from a `file://` repository,
such as `~/.m2/repository`, it is re-checked on every build. `jrs update`
re-checks every snapshot at once.

### `[repositories]`

Additional repositories, tried in declaration order. Maven Central is implicit
and always tried last.

```toml
[repositories]
internal = "https://repo.example.com/maven2"
```

`jrs.toml` is committed, so credentials for a private repository never go in it
— see [User configuration](#user-configuration).

## Commands

| Command | Behaviour |
| --- | --- |
| `jrs build [--watch]` | Resolve → compile main sources → copy resources. `--watch` rebuilds on every change. |
| `jrs test` | `build` + compile test sources + run the tests. See [Tests](#tests) for its flags. |
| `jrs run [-- args...]` | `build` + run `main-class` with `args`. |
| `jrs package` | `build` + produce `target/<name>-<version>.jar`. |
| `jrs package --portable` | Same, with the runtime dependencies copied into `target/lib/`. |
| `jrs package --fat` | Same, with every runtime dependency unpacked into the jar. |
| `jrs package --jlink` | Also build a trimmed runtime image in `target/image`, with a launcher in `bin/`. |
| `jrs package --jpackage [type]` | Also build a native package in `target/jpackage`, using jpackage's own types (`app-image`, `dmg`, `pkg`, `deb`, `rpm`, `exe`, `msi`). |
| `jrs doc` | Generate Javadoc into `target/doc`. |
| `jrs clean` | Remove `target/`. |
| `jrs tree [--depth <n>] [--why <artifact>]` | Print the resolved dependency graph, or every path that leads to one artifact. |
| `jrs classpath [--test \| --runtime]` | Print the resolved classpath, for editors and `java -cp "$(jrs classpath)"`. |
| `jrs update` | Re-resolve and rewrite `jrs.lock`. |
| `jrs verify` | Re-hash the cached dependency jars against the checksums in `jrs.lock`. |
| `jrs outdated` | List declared dependencies that have newer releases. |
| `jrs add <group:artifact[:version[:classifier]]>... [--dev] [--compile-only]` | Add dependencies to `jrs.toml`, at their newest release unless given a version. |
| `jrs remove <group:artifact>... [--dev]` | Remove dependencies from `jrs.toml`. |
| `jrs cache path` / `jrs cache prune` | Print where the cache is, or remove what no project uses. See [Dependency cache](#dependency-cache). |
| `jrs init [--lib] [--name <name>] [path]` | Scaffold `jrs.toml`, a starter class and its test. |
| `jrs migrate` | Generate `jrs.toml` from an existing `pom.xml` or Gradle build. |
| `jrs completions <bash\|zsh\|fish>` | Print a shell completion script. See [Shell completions](#shell-completions). |
| `jrs task <name> [--watch] [-- args...]` | Run a task from `jrs.toml`, and whatever it depends on. See [Tasks and hooks](#tasks-and-hooks). |
| `jrs task --list` | List the tasks, their descriptions and the hooks that run them. |

Global flags: `-v/--verbose`, `-q/--quiet`, `--offline`, `-j/--jobs <n>`,
`--manifest-path <p>`, `--progress <auto|always|never>`,
`--color <auto|always|never>`, `--charset <auto|unicode|ascii>`.

Exit codes: `0` success, `1` build or test failure, `2` usage or manifest error.

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

`jrs add` and `jrs remove` edit `jrs.toml` in place, keeping its comments and
order. An edit that jrs cannot make safely is refused, with a message saying to
edit the file by hand. After each change the graph is resolved again, and if
that fails — a coordinate that does not exist, say — the manifest is put back
the way it was.

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
exactly what those projects' lockfiles name. It also drops the parent POMs,
BOMs, test launchers and JaCoCo jars that only a fresh resolution or a
`--coverage` run needs; those are downloaded again when they are next wanted.
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
- `/Library/Java/JavaVirtualMachines` and `/usr/lib/jvm`
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

| Flag | Effect |
| --- | --- |
| `--filter <regex>` | Only classes whose name matches. |
| `--include-tag <expr>` / `--exclude-tag <expr>` | JUnit tag expressions; repeatable. |
| `--method <class#method>` | Only this method, e.g. `com.example.FooTest#adds`; repeatable. |
| `--coverage` | Record coverage with JaCoCo and write a report to `target/coverage` (HTML, plus `jacoco.xml`). |
| `--watch` | Test again on every change. |

JUnit XML reports land in `target/test-reports`, where CI systems look for
them. `[test] jvm-args` sets the test JVM's arguments.

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
the project's JDK; a `shell` task's `args` arrive as `$1`, `$2`…. The full list is in
[SPEC §7.6](specs/INITIAL_SPEC.md#76-tasks-and-hooks).

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

## Roadmap

Known gaps and features worth building next are tracked in
[ROADMAP.md](ROADMAP.md).

## Development

```
cargo build
cargo test
cargo run
```

`cargo test` is hermetic: dependency resolution is exercised against a `file://`
repository fixture rather than the network. The tests that do reach Maven Central
live behind a feature flag:

```
cargo test --features network-tests --test network
```

The integration tests build real Java fixture projects, so they need a JDK; they
announce that they were skipped if there is none.

CI runs `cargo fmt --check`, `cargo clippy --all-targets -D warnings`,
`cargo build` and `cargo test` on Linux, macOS and Windows, on every push and
pull request against `master`.

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
commits that to `master` and moves the tag onto the new commit, then builds the
binaries for every platform above and publishes them as a GitHub release. Run
`git pull` and `git fetch --tags --force` afterwards to pick up the version commit
and the moved tag.

## Licence

Apache License 2.0. See [LICENSE](LICENSE).
