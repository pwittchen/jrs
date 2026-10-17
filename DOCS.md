# jrs documentation

The full reference for jrs: installation, the `jrs.toml` manifest, every
command, and how dependencies, tests, other JVM languages, tasks and migration
work. For an overview, see the [README](README.md); the same documentation is
published at [getjrs.dev/docs](https://getjrs.dev/docs/).

## Contents

- [Installation](#installation)
- [Uninstallation](#uninstallation)
- [Getting started](#getting-started)
- [The manifest](#the-manifest)
- [Commands](#commands)
- [Dependency cache](#dependency-cache)
- [Build cache](#build-cache)
- [User configuration](#user-configuration)
- [Choosing the JDK](#choosing-the-jdk)
- [Tests](#tests)
- [Kotlin, Scala and Groovy](#kotlin-scala-and-groovy)
- [Tasks and hooks](#tasks-and-hooks)
- [Workspaces](#workspaces)
- [Annotation processors](#annotation-processors)
- [Shell completions](#shell-completions)
- [Editors and IntelliJ IDEA](#editors-and-intellij-idea)
- [Migrating an existing project](#migrating-an-existing-project)
- [Compatibility](#compatibility)
- [Development](#development)

## Installation

### Prebuilt binaries

On macOS or Linux:

```
curl -fsSL https://getjrs.dev/install.sh | sh
```

The script picks the build for the machine, checks it against the release's
`SHA256SUMS` and installs `jrs` into `~/.local/bin`, which must be on your
`PATH`. A release and a directory other than the latest and `~/.local/bin` go
after `sh -s --`:

```
curl -fsSL https://getjrs.dev/install.sh | sh -s -- --version 0.5.0 --to ~/bin
```

or come from the environment as `JRS_INSTALL_VERSION` and `JRS_INSTALL_DIR`. The
script is [`website/install.sh`](website/install.sh).

Every tagged [release](https://github.com/pwittchen/jrs/releases) ships a binary
for each of these platforms:

| Platform | `TARGET` |
| --- | --- |
| macOS, Apple Silicon | `aarch64-apple-darwin` |
| macOS, Intel | `x86_64-apple-darwin` |
| Linux, x86_64 | `x86_64-unknown-linux-musl` |
| Linux, ARM64 | `aarch64-unknown-linux-musl` |
| Windows, x86_64 | `x86_64-pc-windows-msvc` |
| Windows, ARM64 | `aarch64-pc-windows-msvc` |

The Linux binaries are statically linked and run on any distribution. To install
the latest release into `~/.local/bin` by hand, without the script:

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

### In GitHub Actions

The [`setup-jrs`](https://github.com/pwittchen/setup-jrs) action, on the
[GitHub Marketplace](https://github.com/marketplace/actions/set-up-jrs),
installs a prebuilt jrs on the runner, checked against the release's
`SHA256SUMS`, and puts it on `PATH` — on Linux, macOS and Windows, x86_64 and
ARM64:

```yaml
- uses: actions/checkout@v5
- uses: actions/setup-java@v6
  with: { distribution: temurin, java-version: '21' }
- uses: pwittchen/setup-jrs@v1
  with: { cache: true }
- run: jrs test
```

It installs the latest release; `with: { version: '0.11.1' }` picks another.
`version: manifest` installs the newest release the project's
[`jrs-version`](#project) allows, read from `jrs.toml` (or the file
`manifest:` names): `"0.10"` is the newest `0.10.x`, `"0.10.1"` that release
exactly, since a `0.x` minor may break; from `1.0`, the newest release of the
same major version at or above it.

```yaml
- uses: pwittchen/setup-jrs@v1
  with: { version: manifest }
```

`cache: true` points `JRS_CACHE_DIR` at a fixed directory and saves it with
`actions/cache`, keyed on the runner's OS and architecture and the hash of
every `jrs.lock` (`cache-dependency-path` changes the glob), so dependencies
and the build cache survive between runs. Its outputs are `version`, the
version installed, `path`, the directory it went into, and `cache-hit`.

The older form, `uses: pwittchen/jrs/setup-jrs@<tag>`
([`setup-jrs/action.yml`](setup-jrs/action.yml) in this repository), still
works: it installs the release it is used at, and has no `cache` input.

The installer takes the same choice: `curl -fsSL https://getjrs.dev/install.sh
| sh -s -- --version-from jrs.toml`.

### From source

```
git clone https://github.com/pwittchen/jrs.git
cd jrs
cargo install --path .
```

This places the `jrs` binary in `~/.cargo/bin`.

### Updating

```
jrs self check                # is this the latest release, and which is?
jrs self update               # install the latest release over this jrs
jrs self update --to 0.10.0   # install that release, older ones included
```

`jrs self update` downloads the latest release's binary for the machine, checks
it against the release's `SHA256SUMS`, makes sure it runs, and renames it over
the running `jrs` (through a symlink, the file it points at), so a failed
update leaves the old binary in place. A jrs that is already the latest, or
newer, is left alone; `--to` installs the release it names whatever this one
is. Both reach GitHub, through the proxy in the
[config file](#user-configuration) when one is set, and refuse `--offline`. A
jrs installed with `cargo install` is better updated the same way it came.

### Pinning jrs for a team

`project.jrs-version` is a minimum. To build every checkout with the same
jrs, as `gradlew` does for Gradle, commit a wrapper:

```
jrs wrapper                     # pin this jrs's version
jrs wrapper --version 0.11.1    # or another release
./jrsw build                    # jrsw.cmd on Windows
```

`jrs wrapper` writes `jrsw` (POSIX `sh`) and `jrsw.cmd` beside `jrs.toml`.
Each holds one jrs version and the SHA-256 of every release archive, read
from that release's `SHA256SUMS` when it is written. Run, it looks for the
binary in jrs's cache (`<cache>/dist/<version>/`), and when it is not there
downloads the archive for the machine, checks it against the checksum it
carries — a re-uploaded release fails to run — unpacks it and runs it with
the arguments it was given. It needs `curl` or `wget`, `tar` and `sha256sum`
or `shasum`; `jrsw.cmd` needs PowerShell. Run `jrs wrapper --version <v>` to
move the pin.

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
[Kotlin, Scala and Groovy](#kotlin-scala-and-groovy). `jrs init --check` also
adds a `check` task that runs PMD over the Java sources after every compile;
see [Tasks and hooks](#tasks-and-hooks).

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
- [`bookmarks`](examples/bookmarks): a Spring Boot REST service, versioned by
  Boot's BOM through `[managed]`, packaged as a fat jar that starts with
  `java -jar`

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
code `2`, says which version the project needs and that
`jrs self update --to <version>` installs it, and does not try to build a
manifest it may not understand. To pin jrs exactly, see
[Pinning jrs for a team](#pinning-jrs-for-a-team).

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
plugins = ["serialization"]           # optional, Kotlin compiler plugins
```

A table turns the language on. See [Kotlin, Scala and Groovy](#kotlin-scala-and-groovy).

| Key | Default | Meaning |
| --- | --- | --- |
| `version` | — | Required. The compiler, pinned: Kotlin 2.0+, Scala 2.13.9+ or 3.3+, Groovy 4.0+. |
| `source-dir` | `src/main/<lang>` | Another main source root. |
| `test-dir` | `src/test/<lang>` | Another test source root. |
| `kotlinc-args` / `scalac-args` / `groovyc-args` | `[]` | Extra compiler flags, passed through verbatim. |
| `compiler-jvm-args` | `[]` | Flags for the JVM the compiler runs in, such as `-Xmx2g` or `-Xss4m`. |
| `plugins` | `[]` | Kotlin only: compiler plugins by name — `serialization`, `spring`, `jpa`, `allopen`, `noarg`, `power-assert`. |

### `[run]`, `[test]` and `[package]`

```toml
[run]
jvm-args = ["-Xmx512m", "--enable-preview"]   # for `jrs run`, before -cp
java-agents = [
  "com.example:my-agent",                    # -javaagent:, from the resolved graph
  "io.opentelemetry.javaagent:opentelemetry-javaagent:2.10.0",   # pinned apart from it
]
env = { APP_MODE = "dev", DATA = "{target}/data" }
cwd = "work"                                 # relative to the project root

[test]
jvm-args = ["-Dmode=test"]                   # for the test JVM
jacoco-version = "0.8.15"                    # for `jrs test --coverage`; optional
java-agents = ["org.mockito:mockito-core"]
env = { TZ = "UTC" }
retries = 2                                  # run failed tests again; optional
forks = 4                                    # test JVMs to split the classes among; jrs picks unless set
coverage-minimum = { line = 0.80, branch = 0.70 }   # for `jrs test --coverage`; optional
share-classes = true                         # start the test JVM from a CDS archive or AOT cache ("archive", "aot"); off by default

[package]
add-modules = ["jdk.crypto.ec"]              # for --jlink / --jpackage images
native-image-args = ["--no-fallback"]        # for --native-image

[package.manifest]                           # extra MANIFEST.MF attributes
Implementation-Title = "{project.name}"
Implementation-Version = "{project.version}"
Automatic-Module-Name = "com.example.app"

[package.relocate]                           # for --fat: move a package, references and all
"com.google.common" = "com.example.shaded.guava"
"org.slf4j" = { to = "com.example.shaded.slf4j", exclude = ["org.slf4j.impl.*"] }
```

`add-modules` names JDK modules an image needs beyond the ones `jdeps` finds
itself. `jdeps` cannot see modules that are only reached by reflection or
`ServiceLoader`, such as the TLS providers in `jdk.crypto.ec`.

`java-agents` names agents by coordinate, never by path: `jrs.toml` is
committed, and the jar lives in the machine's cache. A `group:artifact` is
looked up in the resolved graph, so it is the version `jrs.lock` pins, and it
has to be a dependency. `test.java-agents` looks on the test classpath,
dev-dependencies included, and `run.java-agents` on the runtime one. An agent
the graph does not hold there is a manifest error that says where to declare
it. An agent the program never calls, such as the OpenTelemetry one, is named
with its version instead, `group:artifact:version` (a classifier may follow).
jrs resolves that jar alone, apart from the graph, as it does a compiler, pins
it in `jrs.lock` as a `[[tool]]` named `run.java-agents.<group>:<artifact>`
(or `test.…`), and keeps it off every classpath. Agents go
to the JVM as `-javaagent:` ahead of every other argument, and ahead of
JaCoCo's agent under `--coverage`. Mockito 5 on JDK 21 and later needs exactly
`test.java-agents = ["org.mockito:mockito-core"]`; without it, Mockito
attaches itself at run time and the JVM warns. `run.java-agents` also goes
into the launchers of a `--jlink` or `--jpackage` image and of a `--dist`
archive, which load an agent from the graph from their `lib/` and a pinned
one from their `agents/`. An image or a distribution of a `--fat` jar can
carry only pinned agents, since the fat jar has unpacked the others, and it
says so.

`env` adds environment variables, and `run.cwd` sets the program's working
directory, relative to the project root; without it the program runs where
jrs was started. Both take the placeholders a task does (`{target}`,
`{project.version}`, `{runtime-classpath}`, … — see
[Tasks and hooks](#tasks-and-hooks)), except `{jar}` and
`{classpath-argfile}`, which only a task has. An `env` value may also name
`{free-port.<name>}`: a TCP port nothing listens on, picked when the command
starts and the same for every value that names it, so a test server's port
and the URL built from it agree (`PORT = "{free-port.web}"`,
`URL = "http://localhost:{free-port.web}"`). There is no `test.cwd`: the test
JVM runs where jrs does. `env` and `cwd` belong to `jrs run` and `jrs test`,
so an image's launchers do not take them.

`share-classes = "archive"` starts the test JVM from a class-data-sharing
archive of the dependency jars, as jrs already does for the compilers, and so
does `true` on a JDK older than 25: a Spring Boot
suite that loads thousands of library classes before its first test starts
noticeably sooner. The first `jrs test` writes the archive into the cache
(`cds/test-*.jsa`, one per project, replaced when the dependencies change),
and later runs map it. To make that possible, `target/classes` and
`target/test-classes` are loaded by the launcher's own class loader instead
of being on the JVM's `-cp`, and that changes one thing a test can see: when
a class directory and a jar hold a class or resource of the same name, the
jar's is found first. jrs checks for such a name before every run and keeps
the usual layout when it finds one (`-v` says which), but it cannot check the
order of `META-INF/services` entries or `spring.factories`, and code that
reads `java.class.path` itself no longer sees the class directories — hence
opt-in. Nor does a library in a jar that loads a project class by name
through its own class loader: Spring's `SerializationUtils` deserialising a
project class fails with `ClassNotFoundException`. `--debug`, `--coverage`
and `test.java-agents` turn it off for the run. When the tests fork and there
is no archive yet, the first fork writes it and the others run without one;
from then on every fork maps it.

`share-classes = "aot"` uses the JDK's AOT cache instead (JDK 24 and later;
an older JDK gets the archive), and so does `true` from JDK 25. Besides the classes it keeps them linked,
and from JDK 25 it keeps method profiles too, which helps a large test
context most. The first run records what it loaded and jrs then builds the
cache (`cds/test-*.aot`) in a JVM of its own, which makes that first run
slower; later runs map it. When the tests attach an agent at run time —
Mockito's inline mock maker does, unless it is in `test.java-agents` — the
cache cannot be built; jrs remembers that (`cds/test-*.aot.unassembled`) and
gives those jars the archive from then on, so `true` is never slower than
`"archive"` for long. To see what either mode
buys on your project,
and whether its tests still pass, run `JRS_BENCH_PROJECT=<path> cargo bench
--bench test_jvm` from a jrs checkout.

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

`[package.relocate]` moves packages inside a fat jar, as Maven Shade's
relocation does, so the jar carries a private copy of a library that cannot
clash with another copy on the same classpath — for an agent, a plugin, or a job
submitted to a cluster. `jrs package --fat` renames the package and every
package under it, moves its classes and resources, and rewrites every reference
to them: in the project's classes, in every dependency's, in
`META-INF/services/` files and in Spring's registries. `exclude` keeps classes,
or packages written `<package>.*`, where they are. Only the fat jar is
relocated; `jrs run`, `jrs test` and the thin and portable jars use the
dependencies as they are, and `jrs package` without `--fat` warns that the
table does not apply. jrs rewrites the names in each class's constant pool and
nothing else, so no tool is downloaded for it. A class name in the middle of a
longer string, or inside any other resource, is left as it is.

### `[resources]`

```toml
[resources]
expand = ["application.yml", "config/*.properties"]   # under project.resource-dir

[resources.properties]                               # optional
version = "{project.version}"
group = "com.example"
```

Resources are copied into `target/classes` as they are, except the files
`expand` names: in those, every `${name}` is replaced as the file is copied,
the way Gradle's `processResources { expand(...) }` and Maven's resource
filtering do. `${project.name}` and `${project.version}` are always known;
`properties` adds more, and its values may use `{project.name}` and
`{project.version}`. `\${name}` is a literal `${name}`, so a placeholder meant
for Spring, such as `"\${DB_URL:}"`, survives the copy. Any other `$` is left
alone.

A `${name}` that is neither built in nor declared fails the build, naming the
file and the line, rather than shipping a resource with the placeholder still
in it. A pattern is a `/`-separated path under `resource-dir`, where `*` and
`?` match within one directory and `**` across any number of them; a pattern
that matches nothing is not an error. Only the main resources are expanded,
and each expanded file is rewritten whenever its expanded content changes, so
a new `project.version` reaches it without touching the file. There is no
scripting: Gradle's `$name` without braces and its `<% %>` blocks are Groovy,
and jrs runs no build code.

### `[obfuscate]`

```toml
[obfuscate]
version = "7.6.1"                            # the ProGuard release to pin
keep = ["com.example.plugin.Api"]            # class names reflection looks up; optional
proguard-args = ["-dontwarn org.foo.**"]     # passed to ProGuard verbatim; optional
```

The table turns obfuscation on and is opt-in: `jrs package --obfuscate` runs
ProGuard over the assembled jar, renaming classes, methods and fields and
stripping debug information while leaving behaviour untouched — the same entry
point runs and the same tests pass against the obfuscated jar. Plain
`jrs package` is unaffected. ProGuard is resolved from Maven Central as its own
tool graph, pinned in `jrs.lock` and run on the project's JDK, exactly as the
compilers are; `jrs tree --tool obfuscator` prints its graph.

Obfuscation runs after packaging, over the jar just written, so it composes
with the fat-jar merge rules rather than redoing them. The entry point and
every `META-INF/services/` provider keep their names, so reflection and
`ServiceLoader` still work; `keep` adds any other names a framework looks up,
and `proguard-args` passes flags through for anything else. Nothing is removed
or reordered, so the measured `run` timing does not change. Pair `--obfuscate`
with `--fat` for a self-contained obfuscated jar.

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

Netty's native transports, `netty-tcnative` and others publish one jar per
platform. `{os-classifier}` in a classifier stands for the platform jrs runs
on, spelled as Maven's `os-maven-plugin` spells `${os.detected.classifier}`:
`linux-x86_64`, `linux-aarch_64`, `osx-aarch_64`, `windows-x86_64`.

```toml
[dependencies]
"io.netty:netty-transport-native-epoll:{os-classifier}" = "4.1.115.Final"
"io.netty:netty-tcnative-boringssl-static" = { version = "2.0.69.Final", classifier = "{os-classifier}" }
```

`jrs.lock` keeps the placeholder, so it is the same file on every machine,
and pins no checksum for such a jar, since each platform downloads another;
the repository's own checksum is still checked. `JRS_OS_CLASSIFIER` set to a
classifier stands for the host's, to package for another platform. A
platform that names its builds differently — JavaFX's `mac-aarch64`, LWJGL's
`natives-macos-arm64` — is written out.

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

A `path` that names a directory rather than a `.jar` is another module of the
same [workspace](#workspaces): `core = { path = "../core" }`.

### `[managed]`

Versions kept in one place, which is what Maven's `<dependencyManagement>` and
Gradle's `platform()` and `constraints { }` are for:

```toml
[managed]
"org.springframework.boot:spring-boot-dependencies" = { version = "4.1.1", bom = true }
"com.fasterxml.jackson.core:jackson-databind" = "2.19.0"

[dependencies]
"org.springframework.boot:spring-boot-starter-webmvc" = {}   # versioned by the BOM
```

A managed version holds an artifact to that version wherever the graph
reaches it, ahead of nearest-wins mediation, but it adds nothing to a
classpath: an artifact nothing depends on stays out. `bom = true` imports a
BOM's own managed versions instead. A dependency may then leave its version
out, as `{}` or as a long form without `version`, and upgrading the BOM
upgrades it. A version written in `[dependencies]` still wins for that
dependency. The table's own entries come before any BOM's, and an earlier BOM
before a later one; a managed version covers every classifier of its
`group:artifact`. `jrs tree` marks a managed version `(managed)`,
`jrs outdated` checks every entry, BOMs included, and `jrs add` leaves the
version out of what `[managed]` covers.

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
| `jrs build [--watch]` | Resolve → compile main sources → copy resources. `--watch` rebuilds on every change, compiling in one warm `javac` for the whole session. |
| `jrs build --verify-cache` | Compile everything and run every cached task, and fail if the [build cache](#build-cache) holds different classes or outputs for the same inputs. |
| `jrs test` | `build` + compile test sources + run the tests a change since the last run reaches. The tests are not recompiled for a main change that leaves the main classes' API alone. See [Tests](#tests) for its flags. |
| `jrs run [--watch] [--debug[=<port>]] [-- args...]` | `build` + run `main-class` with `args`. `--watch` stops the program, rebuilds and starts it again on every change; `--debug` waits for a debugger first; see below. |
| `jrs package` | `build` + produce `target/<name>-<version>.jar`. |
| `jrs package --portable` | Same, with the runtime dependencies copied into `target/lib/`. |
| `jrs package --fat` | Same, with every runtime dependency unpacked into the jar. |
| `jrs package --jlink` | Also build a trimmed runtime image in `target/image`, with a launcher in `bin/`. |
| `jrs package --jpackage [type]` | Also build a native package in `target/jpackage`, using jpackage's own types (`app-image`, `dmg`, `pkg`, `deb`, `rpm`, `exe`, `msi`). |
| `jrs package --sources` / `--javadoc` | Also write `target/<name>-<version>-sources.jar` / `-javadoc.jar`; `--javadoc` runs `jrs doc` first. |
| `jrs package --dist` | Also write a distribution with launch scripts in `bin/`, zipped into `target/<name>-<version>.zip`. |
| `jrs package --native-image` | Also build a native executable in `target/native` with GraalVM's `native-image`. |
| `jrs package --obfuscate` | Obfuscate the packaged jar with ProGuard; needs an `[obfuscate]` table. |
| `jrs package --sbom` | Also write a CycloneDX SBOM of the runtime dependencies, `target/<name>-<version>-cyclonedx.json`. |
| `jrs doc` | Generate API docs into `target/doc`: Javadoc, or Scaladoc / Groovydoc for Scala and Groovy code. |
| `jrs clean` | Remove `target/`. |
| `jrs tree [--depth <n>] [--why <artifact>] [--tool <name>] [--task <name>]` | Print the resolved dependency graph, every path that leads to one artifact, a compiler's own graph (`kotlin-compiler`, `scala-compiler`, `groovy-compiler`), the obfuscator's (`obfuscator`), or a task's. |
| `jrs classpath [--test \| --runtime]` | Print the resolved classpath, for editors and `java -cp "$(jrs classpath)"`. |
| `jrs update` | Re-resolve and rewrite `jrs.lock`. |
| `jrs verify` | Re-hash the cached dependency jars against the checksums in `jrs.lock`. |
| `jrs outdated` | List declared dependencies and `[managed]` versions that have newer releases. |
| `jrs licenses [--runtime]` | List each dependency's licence, as its POM or its parent's declares it, and flag those that declare none. |
| `jrs audit [--runtime] [--ignore <id>]...` | Check every dependency against the [OSV](https://osv.dev) vulnerability database, and fail if one has a known advisory. |
| `jrs add <group:artifact[:version[:classifier]]>... [--dev] [--compile-only \| --runtime-only]` | Add dependencies to `jrs.toml`, at their newest release unless given a version; what `[managed]` covers goes in without one. |
| `jrs remove <group:artifact \| name>... [--dev]` | Remove dependencies from `jrs.toml`; a local jar goes by its name. |
| `jrs cache path` / `jrs cache prune` | Print where the cache is, or remove what no project uses. See [Dependency cache](#dependency-cache). |
| `jrs init [--lib] [--lang <java\|kotlin\|scala\|groovy>] [--check] [--format] [--name <name>] [path]` | Scaffold `jrs.toml`, a starter class and its test; `--check` adds a PMD `check` task in the `post-compile` hook, `--format` a formatter (see [Tasks and hooks](#tasks-and-hooks)). |
| `jrs migrate` | Generate `jrs.toml` from an existing `pom.xml` or Gradle build. |
| `jrs completions <bash\|zsh\|fish>` | Print a shell completion script. See [Shell completions](#shell-completions). |
| `jrs self check` / `jrs self update [--to <version>]` | Say whether jrs is the latest release, or update it to that release or the one `--to` names. See [Updating](#updating). |
| `jrs wrapper [--version <v>]` | Write `jrsw` and `jrsw.cmd`, which run one pinned jrs release. See [Pinning jrs for a team](#pinning-jrs-for-a-team). |
| `jrs jdk list` | Print every JDK jrs can see, where it found it, and which one the project here builds with. See [Choosing the JDK](#choosing-the-jdk). |
| `jrs jdk install <version>` | Download the newest Temurin build of that Java version into jrs's cache, where a project that pins it finds it, and make its `javac` caches. |
| `jrs task <name> [--watch] [-- args...]` | Run a task from `jrs.toml`, and whatever it depends on. See [Tasks and hooks](#tasks-and-hooks). |
| `jrs task --list` | List the tasks, their descriptions and the hooks that run them. |
| `jrs metadata [--no-deps]` | Print the project model as JSON, for editors and tools. |
| `jrs fetch [--sources]` | Download the dependencies into the cache without building, and make the `javac` caches for the project's JDK; with `--sources`, also their `-sources.jar`s. |

`--no-build-cache` on `build`, `test`, `run`, `package` and `task` leaves the
[build cache](#build-cache) alone for that command.

`--watch` keeps one `javac` JVM running for the session and hands it every
rebuild, so the second and later compiles skip the JVM's start-up and run
with a warm JIT; a Kotlin project gets a warm `kotlinc` beside it, which takes
a small project's Kotlin compile from about a second to a third of one. Each
is a child of `jrs` and stops with it: nothing runs in the background between
commands. If one ever fails, that compile runs in a fresh JVM as usual; `-v`
says so.

`jrs run --watch` restarts the program, as Spring's devtools or a Ktor `-t`
build do: when a main source, a resource or `jrs.toml` changes, it stops the
program — `SIGTERM` first on Unix, so shutdown hooks run and its port is free
again, and a kill after ten seconds — builds, and starts it again. A change to
the tests restarts nothing. A program that exits on its own, or a build that
fails, is waited out until the next change. The program keeps the terminal
while it runs, so its input still reaches it.

Global flags: `-v/--verbose`, `-q/--quiet`, `--offline`, `-j/--jobs <n>`
(downloads at once, and in a workspace members built at once),
`--manifest-path <p>`, `-p/--module <name>` (in a [workspace](#workspaces),
act on that member and the members it depends on; repeatable),
`--progress <auto|always|never>`, `--color <auto|always|never>`,
`--charset <auto|unicode|ascii>`.

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
comes first. A duplicate class is reported by name, with both jars; with
`[package.relocate]`, the fat jar can move a package out of the way instead.

Every jar jrs writes carries a directory entry for each package, as `jar cf`
does, so a framework that scans the classpath by asking the class loader for a
package — Spring's component scan, among others — finds the classes inside it.

`--sbom` writes a [CycloneDX](https://cyclonedx.org) 1.5 document beside the
jar: every package on the runtime classpath (and every local jar), with its
package URL (`pkg:maven/group/artifact@version`), the checksum `jrs.lock`
pins for it, its licences — by SPDX identifier when the POM's name or URL is
a licence jrs recognises, by name otherwise — and its place in the graph. It
holds no timestamp, so the same inputs write the same bytes, like the jar.

`jrs licenses` lists every dependency with the licences its POM declares, or
the nearest parent POM's, as Maven inherits them, and warns about each
dependency that declares none. `--runtime` leaves out test and compile-only
dependencies, for what a package actually ships. It reads POMs only, from the
cache when they are there; it never interprets a licence.

`jrs audit` asks the [OSV](https://osv.dev) database — which carries the
GitHub advisories and NVD's for Maven — about every package in the resolved
graph, by the coordinate and version `jrs.lock` pins, and lists each
advisory: the dependency, the advisory's ID with its CVE, its severity, the
nearest version that fixes it, and its summary.

```
dependency                                  advisory                              severity  fixed in  summary
org.apache.logging.log4j:log4j-core:2.14.1  GHSA-jfh8-c2jp-5v3q (CVE-2021-44228)  critical  2.15.0    Remote code injection in Log4j
```

It exits `1` when any advisory is found, so a CI step fails on it.
`--ignore <id>` accepts one advisory, by its ID or an alias (`GHSA-…`,
`CVE-…`), and can be repeated; an `--ignore` that matches nothing is warned
about, so stale ones get noticed. `--runtime` leaves out test and
compile-only dependencies. A local jar has no coordinate to ask about and is
warned about. `jrs tree --why <artifact>` shows which dependency brings a
vulnerable one in, and `[managed]` pins a transitive one to its fix. The
request goes to `https://api.osv.dev` unless `[audit] url` in the user
configuration, or `JRS_AUDIT_URL`, names an OSV-compatible mirror; it holds
the coordinates of the project's dependencies and nothing else.

`jrs add` and `jrs remove` edit `jrs.toml` in place, keeping its comments and
order. An edit that jrs cannot make safely is refused, with a message saying to
edit the file by hand. After each change the graph is resolved again, and if
that fails — a coordinate that does not exist, say — the manifest is put back
the way it was.

`--timings` on `build`, `test`, `run` and `package` prints the wall time of
each phase after the summary. The phases are resolution, downloads, each
compiler step, resources, each task, the test JVM and packaging. A unit's
bookkeeping has rows of its own: `compile <unit>: index`, the time spent
reading what was compiled into the unit's incremental index, and
`compile <unit>: api digest`, the time spent reading the API of the classes
it compiles against (the main classes, for the tests). The same
numbers go to `target/.jrs/timings.txt` as tab-separated columns, even under
`-q`. `jrs run` prints the table before the program starts.

`jrs metadata` prints the project model as JSON on stdout, the way
`cargo metadata` does. It includes the source and resource roots for each
language, the output directories, the compile, runtime and test classpaths
(each jar with its coordinate), the JDK and `--release`, the main class and
the tasks. It resolves the dependencies but never compiles. `--no-deps` skips
resolution. [SPEC §5.4](SPEC.md#54-the-project-model-jrs-metadata-and-jrs-fetch)
has the full schema. `jrs fetch` downloads everything a build needs without
building, the tools of its tasks included, which warms a CI cache for
`--offline` runs. `jrs fetch --sources`
also downloads each dependency's `-sources.jar`, and `jrs metadata` then lists
it next to the jar, so an editor can go to a library's definitions. A library
that publishes no sources jar is only a warning. At a
[workspace](#workspaces)'s root, `jrs metadata` prints every member's model
under `members`, each with the members it depends on under `modules`.

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
The cache also holds `cds/`, where jrs keeps a class-data-sharing archive
for each compiler on each JDK: `javac`, kotlinc, scalac and groovyc start
faster from one (from JDK 25, `javac` and its worker get an AOT cache, which
keeps method profiles too). The first compile after a new JDK or compiler
writes it. For `javac`, the first command that finds none trains it on a
small workload of jrs's own, beside the build rather than after it, and
the `javac` worker's alongside it; an edit's few files never write it; `jrs fetch` and `jrs jdk install` make
`javac`'s caches too, at a moment you are waiting anyway. `prune` drops the
directory like any other; the next build writes it again.
The JDKs `jrs jdk install` downloaded, under `jdks/`, are kept unless
`--jdks` is given, since one takes a while to download again.
`--unused-for` goes by when each artifact was last used. jrs records that in
the file's access time at most once a day, so it works even on filesystems
mounted `noatime`.

On a cold cache a build downloads in two waves: the jars the main sources
compile against (and the compilers) first, then the runtime-only and test
jars on a thread of their own while `javac` runs. The output is the same as
it would be in one wave; `jrs.lock` is written once every jar is in.

## Build cache

The `target/.jrs` fingerprints say whether `target/` is up to date, but a
branch switch, a `git stash` or a `jrs clean` throws the work away. The build
cache keeps it. After every compile, jrs stores the classes a unit compiled
under a hash of everything that went into them — the sources' contents,
the flags, each dependency by its coordinate and pinned checksum, the
compiler, the JDK build and jrs's version — and before compiling a stale unit
it looks that hash up. A hit prints

```
    Restored my-app v1.0.0 (from the build cache)
```

instead of `Compiling`, and leaves the same bytes `javac` would have written.
The hash holds no path of the checkout, so two clones of one commit, in two
directories or on two machines, share their entries. A `jrs test` run of the
whole suite that passed, with no flaky test, is kept the same way: a later
run over the same class files puts its reports back and says
`Testing 214 test classes: passed in the cached run` instead of starting the
JVM. `--all` runs them anyway.

Entries live in the dependency cache's directory, under `build/`. A plain
`jrs cache prune` drops them all; `--unused-for` drops those no build has hit
in that long. `--no-build-cache` on `build`, `test`, `run`, `package` or
`task`, `JRS_BUILD_CACHE=off`, or `enabled = false` under `[build-cache]` in
the [user configuration](#user-configuration) turns the cache off.

Tasks that say `cache = true` are kept too: their outputs, restored instead
of running the task. See [Tasks and hooks](#tasks-and-hooks).

An annotation processor or Groovy AST transformation that reads a file it was
not given, or an environment variable, is outside the hash, as it is for
Gradle's cache: build such a project with `--no-build-cache`.

**Sharing it with CI.** A remote cache is a plain HTTP server — nginx, an S3
bucket behind presigned URLs, a Gradle cache node — or a shared directory
behind a `file://` URL. jrs `GET`s `<url>/<key>.zip` on a local miss and,
only when pushing is on, `PUT`s what it compiled:

```toml
[build-cache]                             # in ~/.config/jrs/config.toml
url = "https://cache.example.com/jrs"     # or file:///mnt/shared/jrs-cache
push = false                              # CI sets true
credentials = "build-cache"               # a [credentials.<name>] entry
tasks = false                             # true: task outputs too
```

CI can set the same through `JRS_BUILD_CACHE_URL` and
`JRS_BUILD_CACHE_PUSH=true`. Whoever can push can put classes into every build
that reads the cache, so give push rights to CI alone. A remote that fails or
times out is skipped for the rest of the command (`-v` says why), and never
fails a build. `jrs build --verify-cache` compiles every unit and runs every
cached task, and fails if the cache holds different classes or outputs
under the same key — a CI job can run it nightly.

A remote serves and takes cached tasks' outputs only with `tasks = true`.
They deserve more care than classes: an output can be a script, a frontend
bundle or a binary that runs outside any JVM, and a task's key trusts its
declared inputs.

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

[build-cache]                             # see "Build cache"
url = "https://cache.example.com/jrs"
push = false

[audit]                                   # an OSV mirror for `jrs audit`
url = "https://osv.example.com"

[jdk]                                     # see "Choosing the JDK"
download = true                           # download a pinned JDK that is missing
url = "https://api.adoptium.net"          # or a mirror of Adoptium's API
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
- **Build cache**: `[build-cache]` turns the local cache off
  (`enabled = false`) or adds a remote one (`url`, `push`, `credentials`);
  see [Build cache](#build-cache). `JRS_BUILD_CACHE=off`,
  `JRS_BUILD_CACHE_URL` and `JRS_BUILD_CACHE_PUSH` override it.
- **Audit**: `[audit] url` points `jrs audit` at an OSV-compatible API
  instead of `https://api.osv.dev`; `JRS_AUDIT_URL` overrides it.
- **JDK downloads**: `[jdk] download = true` downloads a JDK a project pins
  when it is installed nowhere jrs looks; see
  [Choosing the JDK](#choosing-the-jdk). `JRS_JDK_DOWNLOAD=1` turns it on
  for one machine, a CI runner say, and `0` off; `vendor` can only be
  `"temurin"`.

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

- `jrs jdk install`'s, in jrs's cache under `jdks/`

When several builds of that version are installed, the newest wins. If none
is found, the error lists the versions that were found and says how to get
one. `jrs -v build` names the JDK it picked, and `jrs jdk list` lists every
JDK jrs can see and marks the one the project here builds with.

A pinned JDK that is missing can be downloaded:

```
jrs jdk install 21        # the newest Temurin 21, into jrs's cache
```

or, with `download = true` under `[jdk]` in the
[user configuration](#user-configuration) (or `JRS_JDK_DOWNLOAD=1`), on the
first build that needs it. jrs asks the Adoptium API for the newest GA build
for the machine, downloads it, checks its SHA-256 before unpacking anything,
checks that it runs, and only then moves it into place, so an interrupted
download leaves nothing behind. Only a pin triggers a download — a project
that pins nothing and finds no JDK still fails — and `--offline` never
downloads. jrs changes neither `PATH` nor `JAVA_HOME`: the JDK is used for
the projects that pin its version.

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
| `--forks <n>` | Split the test classes among `n` test JVMs run at once; overrides `[test] forks`. |
| `--shard <i>/<n>` | Run only the `i`-th of `n` slices of the test classes, to spread one suite over `n` CI machines. |
| `--suite <name>` | Run the suite `[test.suites.<name>]` declares instead of the tests under `test-dir`. |
| `--all` | Run every test class, not only those a change since the last run reaches, and never take a passing run from the [build cache](#build-cache). |
| `--no-build-cache` | Neither restore from nor store into the build cache. |

JUnit XML reports land in `target/test-reports`, where CI systems look for
them. `[test] jvm-args` sets the test JVM's arguments, and `[test] env` and
`java-agents` its environment and agents (see
[`[run]`, `[test]` and `[package]`](#run-test-and-package)).

**Test suites.**

```toml
[test.suites.e2e]
test-dir = "src/e2e/java"                    # required
test-resource-dir = "src/e2e/resources"      # beside test-dir unless set
jvm-args = ["--enable-preview", "-Dplaywright.headless=true"]
env = { BASE_URL = "http://localhost:8080" }
forks = 1                                    # optional, as [test]'s
retries = 0                                  # optional, as [test]'s
```

A suite is a second set of tests with sources of its own — integration or
end-to-end tests, Gradle's extra source set with a `Test` task over it.
`jrs test --suite e2e` compiles it into `target/suites/e2e/classes` against
the main classes, the default test classes and the test classpath, copies its
resources beside them, and runs it with its own `jvm-args` and `env`, not
`[test]`'s; `test.java-agents` and `test.jacoco-version` still apply. Its
reports go to `target/suites/e2e/test-reports`, so `--rerun-failed` reruns
the suite's failures. Plain `jrs test` never runs a suite, and the suites use
the dev-dependencies: there is no per-suite dependency table.

**Only what a change reaches.** `jrs test` remembers the class directories as
the last run left them, in `target/.jrs/test.tested`, and runs only the test
classes whose class files refer to a class that changed since, directly or
through the project's other classes. Nothing changed prints `no test class
reaches a change since the last run` and runs nothing. Whatever the class
files cannot account for runs every test class: the first run, a changed
resource, a class added or removed, other JVM arguments, environment or
dependencies, Groovy classes, and a changed class no test refers to, since
something loads it by name. A test class that reaches Spring's or another
framework's test support, the JUnit suite engine, Cucumber, ArchUnit,
`ServiceLoader` or `java.lang.reflect` finds classes itself, so it runs on
every change. A test class that failed, or passed only on a retry, runs again
next time whatever changed. `--all` runs everything, as do `--coverage` and
`--debug`; `--filter`, the tag flags, `--method` and `--rerun-failed` run
what they select and leave the record alone. A test that reads files outside
the class directories — fixtures under `src/test/data`, say — is not
something jrs can see: run it with `--all`, or keep the files in the test
resources.

`[test] forks = n`, or `--forks n`, splits the test classes among `n` test
JVMs that run at once, as Gradle's `maxParallelForks` does. Unset, jrs picks
a number: one JVM per 8 test classes, up to half the machine's cores, so a
suite of fewer than 16 classes runs in one. `forks = 1` keeps every class in
one JVM. Each JVM is dealt its share of the classes by their time in the
last run, the slowest first, each to the JVM with the least time so far;
classes with no recorded time are dealt out in name order. Each JVM still
scans for its share, so `--filter` and the tags apply as before. Each JVM's output is printed whole, one after another,
once all have finished; their XML lands side by side in `target/test-reports`
as `TEST-<engine>-fork-<k>.xml`, and their coverage goes into the one
`target/jacoco.exec`. Retries, `--rerun-failed`, `--method`, `--fail-fast`
and `--debug` run in one JVM. Tests that share something outside the JVM — a
port, a database, a file — must be ready for another JVM running beside them.

`--shard <i>/<n>` spreads one suite over several machines: `--shard 1/4` on
one CI job, `2/4` on the next, and so on, each running a quarter of the test
classes. The slices follow from the class names alone — every `n`-th class in
name order — never from anything one machine knows and another does not, so
together the shards run every class exactly once. Within its slice a shard
works as a whole run does: forks, retries, the build cache, and the record of
what it ran, which is kept apart from the other shards'. A shard with no
classes left, when there are more shards than classes, runs nothing and
exits `0`.

```yaml
strategy:
  matrix:
    shard: [1, 2, 3, 4]
steps:
  - run: jrs test --shard ${{ matrix.shard }}/4
```

Jupiter also runs tests in parallel inside each test JVM when asked to, and
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
- `jrs doc` runs Scaladoc for Scala and Groovydoc for Groovy, each over the
  Java sources beside it too, except Scala 3's scaladoc, which reads the
  compiled classes and leaves Java out. Kotlin sources are left out of
  `javadoc`, since Dokka is not supported. kapt/KSP and Kotlin Multiplatform
  projects are not supported.
- **Compiler plugins.** `[kotlin] plugins = ["serialization"]` turns on the
  plugins that need nothing but their name: `serialization`, `spring` and
  `jpa` (the `allopen` and `noarg` presets), and `power-assert`. Each is
  resolved into the compiler's graph at the compiler's version and handed to
  `kotlinc` as `-Xplugin`. `allopen` and `noarg` on their own need your
  annotations, as `kotlinc-args = ["-P",
  "plugin:org.jetbrains.kotlin.allopen:annotation=com.example.Open"]`.
- **Multiplatform libraries.** A Kotlin Multiplatform library is declared by
  its root coordinate, `io.ktor:ktor-server-core`, as in Gradle. Its POM
  lists only its common code's dependencies, so jrs reads the Gradle module
  metadata (`.module`) published beside it, and follows the JVM variant to
  the artifact that holds the classes, `ktor-server-core-jvm`.

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
- **`main`** is a class run from the task's own dependencies, so a Java tool
  from Maven Central needs no installing:

```toml
[tasks.format]
description = "Format App.java in place"
main = "com.google.googlejavaformat.java.Main"
args = ["--replace", "src/main/java/com/example/App.java"]

[tasks.format.dependencies]
"com.google.googlejavaformat:google-java-format" = "1.22.0"
```

A task's dependencies take the `[dependencies]` forms, without `compile-only`,
`runtime-only`, local jars or `[managed]` versions. They are resolved as a
graph of the task's own, which never meets the project's, so a formatter's
Guava does not mediate against yours. `jrs.lock` pins it as a `[[tool]]` block
named `tasks.format`, the way it pins the compilers, and `jrs tree --task
format` shows it. The jars are downloaded when the graph is resolved afresh,
or else when the task first runs, and reach `java` through an argfile. A
`script` task can take `dependencies` too, as its classpath.

Checks are tasks too, so Checkstyle, PMD or SpotBugs need neither a plugin nor
an install. `jrs init --check` starts a project with one: a `check` task that
runs PMD's `quickstart` rules over `src/main/java`, hooked into `post-compile`,
so a violation fails the build after PMD's report. `jrs task check` runs it
alone. `jrs migrate` writes the same kind of task for a build's Checkstyle,
PMD and SpotBugs plugins. The ruleset, or the tool, is yours to change in
`jrs.toml`:

```toml
[tasks.check]
description = "Check the Java sources with PMD's quickstart rules"
main = "net.sourceforge.pmd.cli.PmdCli"
args = ["check", "--no-progress", "--rulesets", "rulesets/java/quickstart.xml", "--dir", "src/main/java", "--cache", "{target}/pmd.cache"]

[tasks.check.dependencies]
"net.sourceforge.pmd:pmd-cli" = "7.27.0"
"net.sourceforge.pmd:pmd-java" = "7.27.0"

[hooks]
post-compile = ["check"]
```

Formatters work the same way. `jrs init --format` adds google-java-format
(ktfmt for a Kotlin project) as two tasks: `format`, which rewrites the
sources in place and which you run with `jrs task format`, and
`format-check`, hooked into `post-compile`, which fails the build when a
source is not formatted. google-java-format takes a list of files, which
`{sources-argfile}` hands it:

```toml
[tasks.format-check]
main = "com.google.googlejavaformat.java.Main"
args = ["--dry-run", "--set-exit-if-changed", "--aosp", "@{sources-argfile}"]
env = { JDK_JAVA_OPTIONS = "--add-exports=jdk.compiler/com.sun.tools.javac.api=ALL-UNNAMED …" }

[tasks.format-check.dependencies]
"com.google.googlejavaformat:google-java-format" = "1.35.0"
```

A `main` task's JVM takes no options of its own, so the ones
google-java-format needs reach it through `JDK_JAVA_OPTIONS`, which the
launcher reads and announces on stderr.

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

**Cached tasks.** A generator that runs before `javac` runs on every clean
build and on every CI machine. Add `cache = true` to a task with `inputs`
and `outputs`, and its outputs go into the [build cache](#build-cache)
under a hash of its command, its inputs' contents and its tool jars: after
`jrs clean`, in a second checkout or on CI, they are put back instead, and
the compile that follows them usually comes from the cache too.

```toml
[tasks.openapi]
main = "org.openapitools.codegen.OpenAPIGenerator"
args = ["generate", "-i", "api.yaml", "-g", "spring", "-o", "{target}/openapi"]
inputs = ["api.yaml"]
outputs = ["{target}/openapi"]
source-outputs = ["{target}/openapi/src/main/java"]
cache = true
```

```
    Restored openapi (task, from the build cache)
```

A restored task prints nothing of what it printed when it ran. Only say
`cache = true` of a task that reads nothing but its `inputs`, its arguments
and its tools: jrs cannot see a file, the network or the clock read behind
its back, and a `run` program is keyed by its name, not its version. A task
that uses `{jar}`, or that `post-package` or `pre-run` runs, cannot be
cached — those exist for their side effects. Outputs must be inside the
project; one outside `target-dir` (a client checked into `src/generated`)
is only replaced when it holds exactly what the task last wrote there, and
otherwise the task runs and leaves your files alone (`-v` names the file).
`jrs task <name> -- args` with extra arguments never uses the cache.

Placeholders such as `{root}`, `{target}`, `{classes}`, `{project.version}`,
`{classpath}` (what `jrs classpath` prints) and `{jar}` are expanded in `run`,
`args`, `cwd` and `env`; `args` and `env` also take `{free-port.<name>}`.
`{sources-argfile}` is a file jrs writes before the task runs, listing the
project's own main and test sources, one absolute path per line — for a tool
that reads its file list as `@file`. `shell` strings use environment variables instead:
`JRS_ROOT`, `JRS_CLASSPATH`, `JRS_JAR` and the rest, with `JAVA_HOME` set to
the project's JDK; a `shell` task's `args` arrive as `$1`, `$2`…. The full
list is in [SPEC §7.6](SPEC.md#76-tasks-and-hooks).

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
`update`, `verify`, `outdated`, `licenses`, `audit`, `add`, `remove`, `cache`, `init`,
`migrate`, `completions`, `self` and `clean` never run a task, so inspecting a project with them
is always safe.

## Workspaces

A project of several modules — a `core` library, an `api`, an application on
top — is a workspace: one `jrs.toml` per module, and a root `jrs.toml` that
lists them. `examples/ledger` is one.

```toml
# jrs.toml at the root
[workspace]
members = ["core", "api", "app"]

[java]
source = 21

[managed]
"org.junit:junit-bom" = { version = "5.14.3", bom = true }
```

```toml
# app/jrs.toml
[project]
name = "app"
version = "1.0.0"
main-class = "com.example.app.Main"

[dependencies]
api = { path = "../api" }

[dev-dependencies]
"org.junit.jupiter:junit-jupiter" = {}
```

- **Members** are directories under the root, each with its own `jrs.toml`,
  sources and `target/`. The order of `members` does not matter: a member is
  built after the members it depends on. A root without `[project]` builds
  nothing itself; a root with one is a member too.
- **Shared keys.** `[java]`, `[repositories]` and `[managed]` in the root are
  every member's, a member's own key winning. `[kotlin]`, `[scala]` and
  `[groovy]` are inherited by a member that has the table, so `[kotlin]` with
  nothing under it takes the root's version.
- **Depending on a member** is a `path` to its directory, keyed by its
  `project.name`, in `[dependencies]` (with `compile-only` or `runtime-only` if
  needed) or `[dev-dependencies]`. Its classes go where its jar would, and its
  own dependencies come with it, as a jar's transitive ones do. Members that
  depend on each other in a cycle are an error that names the cycle.
- **One lockfile.** `jrs.lock` lives at the root and pins one version of every
  library for every member: the workspace is resolved once. Two members that
  declare one library at two versions get the first one's, with a warning.
  Members have no lockfile of their own.

At the root, a command acts on every member. `-p <member>` (repeatable), or
running from inside a member's directory, narrows it to that member and the
members it depends on, which are always built first:

```
jrs build                     # every member, in dependency order
jrs test                      # every member's tests; the command fails at the end if any did
jrs -p api test               # api's tests, after building core
cd app && jrs run             # app, after building api and core
jrs run                       # the only member with a main class; with several, -p says which
jrs package --fat             # a jar per member; app's carries api's and core's classes
jrs -p app classpath          # app's classes, then api's and core's, then the jars
jrs -p core add com.google.guava:guava
```

`jrs classpath`, `jrs add` and `jrs remove` act on one member; at a root
without `[project]` they ask for `-p`. `jrs update`, `jrs verify`,
`jrs fetch`, `jrs outdated`, `jrs licenses` and `jrs audit` act on the
workspace's one graph. `jrs tree` prints each member's graph, its member
dependencies first, marked `(module)`.

A change to a member recompiles the members that depend on it only when it
changes what they can see — a method's body leaves them fresh — and then only
their sources that use a class whose API changed; `jrs test` runs the tests
a change reaches, across members. Phase lines name
the member: `Compiling core v1.0.0`, `Compiling app: 4 test sources`.

Members that do not depend on each other build at once, up to `--jobs` of
them. The output still reads as if they had built one after the other: each
member's lines come out in build order, and the member at the front streams
as it goes while the others wait their turn; the live line names the members
compiling. `jrs package` writes each member's jar as part of the same work.
`jrs test` builds every member first and then runs each member's tests in
turn, since a test JVM already uses the cores it is given — or at once, up to
`--jobs`, with `parallel-tests = true` under `[workspace]`, for suites that
share no port, database or file. `--jobs 1` builds one member at a time, as
does a workspace whose tasks depend on another member's. A command that
builds several members compiles them in warm `javac` JVMs it keeps until it
exits, so the later members compile in a JVM the earlier ones warmed; the
first one starts with the command, while the lockfile is read, and from JDK 25
maps an AOT cache of its own. `--timings` adds a `member <name>` row for each member
built at once. When a member fails, no other starts; what already ran is
printed, then the error.

In a fat jar, a member's sibling members' classes are packaged under the same
rules as a dependency's; in the portable layout they are jars in `lib/`. At the
root, `--fat`, `--dist`, `--jlink`, `--jpackage` and `--native-image` apply to
the members with a main class, and a library gets its plain jar (the portable
layout where the applications get a fat jar).

`[tasks]` and `[hooks]` stay per member, and a task can depend on another
member's task as `"<member>:<task>"`:

```toml
[tasks.check]
script = "Check.java"
depends-on = ["core:generate"]
```

The other task runs in its own member, once per command, before this member's
command reaches it.
[specs/WORKSPACES.md](specs/WORKSPACES.md) has the design.

## Annotation processors

jrs has no separate processor path. Configuring processors is a
[non-goal](SPEC.md#12-non-goals). But `javac` runs any processor it finds on
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

## Editors and IntelliJ IDEA

jrs generates no IDE project files. Instead it answers the questions a tool can
ask: `jrs metadata` prints the whole project model as JSON (source and resource
roots, output directories, the classpaths, the JDK, the main class, the tasks),
`jrs fetch --sources` downloads each dependency's `-sources.jar` for
go-to-definition, and `jrs classpath` prints a classpath for a tool that only
needs that.

### IntelliJ IDEA

The jrs plugin imports a jrs project the way the IDE imports a Maven or Gradle
one, builds and runs it through `jrs`, and knows its `jrs.toml`. It starts no
daemon and never reads the manifest itself: what it knows comes from
`jrs metadata`, and what it does runs the `jrs` binary as a subprocess. It
needs IntelliJ IDEA 2026.2 or newer and jrs 0.9 or newer, found in its
settings, on `PATH` or in the usual install directories.

To install it, download `jrs-intellij.zip` from the
[latest release](https://github.com/pwittchen/jrs/releases/latest), open
**Settings | Plugins**, choose **Install Plugin from Disk…** from the gear menu,
pick the zip and restart the IDE. Then open the directory that holds `jrs.toml`
and click **Load jrs project** in the notification. The plugin:

- **imports the project** as one module — or a [workspace](#workspaces) as one
  module per member, each depending on the members it depends on: the source
  and resource roots of every language, task-generated sources, `target/classes` as the output, the
  JDK and language level, and one library per jar with the right scope, its
  sources attached when they are in the cache. **Tools | jrs | Download
  Sources** runs `jrs fetch --sources`;
- **stays in sync**: a change to `jrs.toml` or `jrs.lock` shows a **Load jrs
  changes** banner, or reloads by itself if you turn that on;
- **builds with jrs**: **Build Project** runs `jrs build`, **Rebuild Project**
  `jrs clean` first, so the IDE and the terminal write the same
  `target/classes`. Compiler errors open their source line;
- **runs, tests and debugs**: a **jrs** run configuration for any command,
  **jrs test** on test classes and methods from the gutter, results in the test
  runner tree, and the debugger attached to `run` and `test` through `--debug`;
- **adds a jrs tool window** with the project's tasks and resolved
  dependencies, and the commands from **Tools | jrs**;
- **edits `jrs.toml`**: completion of tables and keys, a warning on an unknown
  key, and **Add** / **Remove Dependency** through `jrs add` and `jrs remove`;
- **migrates** a Maven or Gradle project with **Tools | jrs | Migrate to jrs**.

Its settings are under **Settings | Build, Execution, Deployment | Build Tools |
jrs**; turn off **Build the project with jrs** to let IntelliJ compile the
project itself. Kotlin, Scala and Groovy sources are set up, but their compiler
flags are not passed to the IDE's language plugins yet. The
[plugin's README](plugins/intellij/README.md) has the details and how to build
it.

For an IDE without jrs support, point its project at the source roots and add
the jars `jrs classpath --test` prints as libraries.

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

A multi-module build becomes a [workspace](#workspaces): a Maven aggregator's
`<modules>` (walked recursively) or the projects a Gradle build's
`settings.gradle` includes each get a `jrs.toml` of their own, and the root one
lists them. A dependency on a sibling module — Maven's own `groupId:artifactId`,
Gradle's `project(":core")` — becomes a path dependency. What every member
shares moves into the root: the parent POM's compiler settings and
`<dependencyManagement>`, or a Gradle version catalog, become the root's
`[java]` and `[managed]`, and the members' declarations take their versions
from it. Each member is read as Maven would build it, with its parent's
plugins and `<pluginManagement>`; a Gradle root's `subprojects { }` and
`allprojects { }` are read into every member. The report has a section per
member, and ends by saying how many migrated with nothing to review. A module
reached only through a profile, a `pom` module that is no aggregator (a BOM),
and a sibling's test jar are reported.

A version kept in a Gradle variable is written into each declaration that
uses it. That covers `ext`, `def`, the Kotlin DSL's `val` and `extra`, and
`gradle.properties`, used as `"g:a:$v"`, `${project.v}`, `${property("v")}`,
map notation's `version: v`, in platforms, `mavenBom`, `constraints { }` and
Spring Boot's plugin. A review line names the variable, since the manifest no
longer shows those versions moving together. Only a variable assigned once,
to a literal and outside a conditional, is used; a `val` at the top of
`plugins { }` counts, since Gradle runs that block once, so
`kotlin("jvm") version kotlinVersion` reads. What uses any other is
reported with the variable's name, never migrated without its version.

Gradle tasks that say everything in literals become `[tasks]`: an `Exec` task
a `run` command, a `JavaExec` over the main runtime classpath a `java` command
over jrs's classpath, and a task with only `dependsOn` an aggregate.
`compileJava.dependsOn`, `test.dependsOn`, `run.dependsOn` and `jar.finalizedBy`
become `[hooks]`. A task with a `doLast { }` closure, another type such as
`Copy`, or a value built from a variable is listed with the reason, to rewrite
by hand as a `run`, `shell` or `script` task.

A `Test` task whose `testClassesDirs` is another source set's output
(`sourceSets.integration.output.classesDirs`) becomes `[test.suites]`: the
source set's Java and resource directories, from `sourceSets { }` or Gradle's
`src/<set>/java` convention, what `tasks.withType(Test)` sets, and the task's
own `jvmArgs` and `systemProperty` — never the `test` task's. The suite is
named after the task, less a leading `test`: `testE2e` becomes `e2e`,
`integrationTest` becomes `integration-test`. `dependsOn` on `build`, `jar`,
`assemble` or `bootJar` becomes a `pre-compile` hook, the earliest point
before all of them, which `jrs test` and `jrs run` pass through too; on
`bootRun` it becomes `pre-run`, or nothing if `pre-compile` has it already.
The `jacoco` plugin's `toolVersion` becomes `test.jacoco-version`.

`environment` and `workingDir` on Gradle's `run` and `test` tasks become
`run.env`, `run.cwd` and `test.env` when they are literals. A `-javaagent:` in
`jvmArgs` is a path into Gradle's cache, so it is reported, except for
Mockito's own recipe, which becomes
`test.java-agents = ["org.mockito:mockito-core"]`.

`processResources { filesMatching(...) { expand(...) } }` becomes
`[resources]`: the patterns become `expand`, and `expand(project.properties)`
the properties such a file is likely to name — `version`, `name`, and the
build's literal `group` and `description`. An `expand` given a map carries its
entries over. `filter` and the rest of `ProcessResources` are reported.
`options.compilerArgs` in `compileJava { }` or `tasks.withType(JavaCompile)`
joins `java.javac-args`, and `jvmArgs` in `bootRun { }` or
`tasks.withType(JavaExec)` joins `run.jvm-args`. A `$name` or `${...}` in a
JVM argument takes the variable's value; one jrs cannot read — Ktor's
`-Dio.ktor.development=$isDevelopment` — leaves the argument out, reported,
rather than handing the JVM the template. A `version` Gradle computes,
from a property or a git tag, is reported and defaulted, since it has no value
until Gradle runs.

A `jar { manifest { attributes(...) } }` block, and Maven's
`<manifestEntries>`, become `[package.manifest]` when the values are literals
or the project's version. `withSourcesJar()` and `withJavadocJar()` are
reported: in jrs they are the `jrs package --sources` and `--javadoc` flags.

The `proguard-maven-plugin` and the Guardsquare Gradle plugin
(`com.guardsquare.proguard`) become `[obfuscate]`: the pinned ProGuard version
carries over — from the Maven plugin's `<proguardVersion>` or the Gradle
plugin's own version — and the Maven `<options>` pass through to
`proguard-args`. The Gradle `proguard { }` task's keep rules are reported to
move into `keep` / `proguard-args` by hand.

`maven-shade-plugin`'s `<relocations>` and the Shadow plugin's `relocate` calls
become `[package.relocate]`, exclusions included, and the shaded jar itself is
`jrs package --fat`. A Shade `<includes>`, a `<rawString>` relocation and a
wildcard jrs cannot match are reported.

Scopes keep their meaning. Maven's `provided` and Gradle's `compileOnly` become
`compile-only`, and `runtime` and `runtimeOnly` become `runtime-only`. Gradle's
`files(...)` and `fileTree(...)` become local jars, with a `fileTree` expanded
to the jars it holds when you migrate, and so does a Maven `system` jar under
`${project.basedir}`, compile-only as Maven has it. A repository's
`content { includeGroup }` or `exclusiveContent` filter becomes its `groups`.

Managed versions become `[managed]`: Gradle's `platform()`,
`enforcedPlatform()`, `constraints { }` and `dependencyManagement { }`, and
Maven's imported BOMs and `<dependencyManagement>`. A Spring Boot build, as
start.spring.io generates it, migrates whole. The Boot plugin with
`io.spring.dependency-management`, or Maven's `spring-boot-starter-parent`,
becomes Boot's BOM, so the starters stay versionless. `-parameters`, which
Boot compiles with, goes into `javac-args`, and the `@SpringBootApplication`
class becomes the main class. `jrs package --fat` then builds the runnable
jar, a flat one with Spring's registries merged. The Ktor Gradle plugin
(`io.ktor.plugin`) becomes `io.ktor:ktor-bom` at the plugin's version, so
the versionless Ktor dependencies, test ones included, stay versionless.

Maven's `exec-maven-plugin` executions become tasks, and their `<phase>` a
hook: `exec` a `run` command, and `java` a `java` command over jrs's
classpath, or a `main` task over the plugin's own `<dependencies>`.

Formatters and checkers become tasks over the tool's own graph, a checker
hooked into `post-compile`: Spotless's `googleJavaFormat` and `ktfmt` steps
(Gradle and Maven) and `fmt-maven-plugin` a `format` and a `format-check`
task, with the version and style the build names; Gradle's `checkstyle`, `pmd`
and `com.github.spotbugs` plugins and Maven's `maven-checkstyle-plugin`,
`maven-pmd-plugin` and `spotbugs-maven-plugin` a `checkstyle`, `pmd` or
`spotbugs` task, with the tool version, configuration file and rule sets they
set. Checkstyle's task gets what the build gives Checkstyle besides: the
plugin's other `<dependencies>` and the Gradle `checkstyle` configuration's
jars, where a configuration's custom checks come from (and nohttp's, when its
Gradle plugin is applied); Maven's `<propertyExpansion>`, Gradle's
`configProperties` and the `config_loc` Gradle sets, as system properties in
`JAVA_TOOL_OPTIONS`, since Checkstyle's command line takes none; and a
`maven-checkstyle-plugin` `<execution>`'s own configuration, one task per
execution that has one. Its `<sourceDirectories>` become the task's
directories and its Ant `<excludes>` Checkstyle's `-x` patterns, anchored at
the project root; an `<includes>` or exclusion that does not translate is
reported, not narrowed. Spotless's other steps are reported. The CycloneDX plugins point at
`jrs package --sbom`, and a licence report plugin at `jrs licenses`.
`os-maven-plugin`'s `${os.detected.classifier}` and the osdetector plugin's
classifier become `{os-classifier}`.

`jrs.toml` is one fixed configuration, so a Maven build migrates as a plain
`mvn` build runs it: the profiles that build activates — `<activeByDefault>`
ones, or one waiting only on a property being unset, such as `!skipDocs` —
are merged in first, the way Maven merges them, and the others are listed
with what activates them. Gradle's `maxParallelForks` and surefire's
`<forkCount>` become `test.forks` when they are numbers, `1` included, since
jrs would otherwise pick its own number; one worked out from the machine's
cores is reported. A build with tests and no fork count gets a review line
saying that jrs splits a large suite among several JVMs and that `forks = 1`
keeps one.

Kotlin, Scala and Groovy builds migrate too. The Kotlin Gradle plugin,
`id 'groovy'` and `id 'scala'`, and Maven's `kotlin-maven-plugin`,
`scala-maven-plugin` and `gmavenplus-plugin` become language tables. An
explicit standard-library dependency is dropped, since it is implied now.
Kotlin compiler flags — Gradle's `freeCompilerArgs`, Maven's `<args>` —
become `kotlinc-args` when they are literals, and so do the switches of
`compilerOptions { }` (`javaParameters`, `allWarningsAsErrors`). The compiler
plugins `[kotlin] plugins` knows — `kotlin("plugin.serialization")`,
`plugin.spring`, `plugin.jpa`, Maven's `<compilerPlugins>` — go there; kapt
and a bare `allopen` or `noarg` are reported.

A version catalog's `libs.bundles.<name>` becomes the libraries in the
bundle, and `alias(libs.plugins.<name>)` the plugin the catalog names, at
its version. `resolutionStrategy { force(...) }` becomes `[managed]`.

`environment` values on the `test` and `run` tasks may be numbers, and
variables set to a literal. A port a build picks at random for the server its
tests start, `nextInt(8100, 8999)` in something named a port, becomes
`{free-port.<name>}`, and a string built from it uses the same one.

Code generation migrates with the task that does it. The OpenAPI Generator
plugin's `openApiGenerate { }` becomes a `main` task over the generator's
command-line tool, at the plugin's version, with each setting as the tool's
option. So does each `<execution>` of `openapi-generator-maven-plugin`, as a
`pre-compile` task writing where the plugin does,
`target/generated-sources/openapi`, whose `src/main/java` is its
`source-outputs`; `build-helper-maven-plugin` adding that directory is then
covered. A task of the build's own that the compile or the tests depend on,
and that is Gradle code — a `doLast { }` — is left to Gradle when the
project has the wrapper: one `[tasks.gradle]` runs `./gradlew` over all of
them before the compile, with the `inputs` and `outputs` they declare, and
`project.target-dir` becomes `build`, where Gradle writes. The
`sourceSets { main { … srcDir(…) } }` directories they write become the
tasks' `source-outputs` and `resource-outputs`. Rewriting those tasks as jrs
tasks is what drops Gradle from the build.

## Compatibility

How real projects fare, taken through `jrs migrate`, `build`, `test`, `run`
and `package --fat` with no hand edits and checked against their own Maven
or Gradle build (the runtime classpath, and the tests run, passed and
skipped). The projects and their pinned commits are in
[`specs/corpus/corpus.toml`](specs/corpus/corpus.toml); each failure is a numbered entry
in [`specs/corpus/FINDINGS.md`](specs/corpus/FINDINGS.md), fixed, added as a migration
rule, or cited as a non-goal. Measured with jrs 0.11.0 on 2026-10-14, on
JDK 25.0.2 and macOS 27.0.1; a step after a failed one did not run (`·`).

| Project | Build | Kind | Commit | migrate | build | test | run | package |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `spring-guides/gs-rest-service` | Maven | Spring Boot | `3f4cef0` | ok | ok | ok 2/2 | ok | ok |
| `spring-guides/gs-accessing-data-jpa` | Maven | Spring Boot | `ef48947` | ok | ok | ok 1/1 | ok | ok |
| `spring-guides/gs-rest-service` | Gradle | Spring Boot | `3f4cef0` | ok | ok | ok 2/2 | ok | ok |
| `spring-guides/gs-accessing-data-jpa` | Gradle | Spring Boot | `ef48947` | ok | ok | ok 1/1 | ok | ok |
| `spring-projects/spring-petclinic` | Maven | Spring Boot | `500158f` | ok | [F13](specs/corpus/FINDINGS.md#13-a-boms-exclusions-on-a-managed-dependency-are-not-applied) | ok 81/81 | ok | ok |
| `spring-projects/spring-petclinic` | Gradle | Spring Boot | `500158f` | ok | [F13](specs/corpus/FINDINGS.md#13-a-boms-exclusions-on-a-managed-dependency-are-not-applied) | ok 81/81 | ok | ok |
| `spring-petclinic/spring-petclinic-kotlin` | Gradle | Spring Boot, Kotlin | `c77f77b` | ok | [F17](specs/corpus/FINDINGS.md#17-a-versionless-orgjetbrainskotlin-library-takes-the-kotlin-plugins-version) | ok 41/41 | ok | ok |
| `ktorio/ktor-samples`, `httpbin` | Gradle | Ktor | `1c9df7c` | ok | [F14](specs/corpus/FINDINGS.md#14-a-profile-a-dependencys-pom-activates-by-os) | [F14](specs/corpus/FINDINGS.md#14-a-profile-a-dependencys-pom-activates-by-os) 166/167 | ok | ok |
| `ktorio/ktor-samples`, `h2` | Gradle | Ktor | `1c9df7c` | ok | ok | ok 4/4 | [F19](specs/corpus/FINDINGS.md#19-ktor-h2-names-a-module-function-that-does-not-exist) | [F19](specs/corpus/FINDINGS.md#19-ktor-h2-names-a-module-function-that-does-not-exist) |
| `spring-petclinic/spring-petclinic-rest` | Maven | service | `afc8fc1` | ok | [F13](specs/corpus/FINDINGS.md#13-a-boms-exclusions-on-a-managed-dependency-are-not-applied) | [F12](specs/corpus/FINDINGS.md#12-maven-the-compiler-plugins-execution-configuration-is-not-read) 5/237 | [F12](specs/corpus/FINDINGS.md#12-maven-the-compiler-plugins-execution-configuration-is-not-read) | [F12](specs/corpus/FINDINGS.md#12-maven-the-compiler-plugins-execution-configuration-is-not-read) |
| `spring-petclinic/spring-petclinic-microservices` | Maven | workspace | `1d76b00` | ok | [F11](specs/corpus/FINDINGS.md#11-a-version-range-in-a-transitive-pom) | · | · | · |

End to end: 4 of 11, and 10 of 11 get past `jrs build`; what stops the
other six there is a classpath difference, a failing test or start, or the
project itself. Where
a migrated project builds, its runtime classpath matches the reference's
except as the findings say, and petclinic's 81 tests, its Testcontainers
suite among them, pass on all three. Why the others are not `ok`:

- **F13** — Spring Boot's BOM excludes `protobuf-java` from
  `mysql-connector-j`; jrs takes a managed entry's version and not its
  exclusions, so the jar is on the classpath. petclinic's devtools, which
  jrs.toml has no scope for, is missing besides
  ([F18](specs/corpus/FINDINGS.md#18-spring-boot-devtools)).
- **F17** — petclinic-kotlin's versionless `kotlin-reflect` takes Spring
  Boot's version, where the Kotlin plugin gives it its own; JUnit's
  `apiguardian-api` and devtools differ too (F16, F18).
- **F14** — brotli4j adds its native jar through a profile its POM
  activates by OS, which jrs does not read, so the Brotli test fails.
- **F12** — spring-petclinic-rest configures MapStruct in the compiler
  plugin's `default-compile` execution, which migration does not read, so
  the mappers are not Spring beans.
- **F19** — the sample's `application.conf` names a module function that
  does not exist; its own `./gradlew run` fails the same way.
- **F11** — a webjar's POM asks for a version range, which jrs rejects
  ([`[managed]`](#managed) pins one by hand, as the error now says).

Gradle resolves the highest version in a graph and jrs the nearest
([F15](specs/corpus/FINDINGS.md#15-highest-wins-mediation)), so a Gradle project's versions may
differ where nothing else does; the Ktor samples' do.

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

The rebuild benchmark generates a project of a few hundred classes, then times
a clean build, a no-op build and a rebuild after each of three one-file edits.
It prints how much of each is `javac` and how many sources the build
compiled (SPEC §7.2). It needs a JDK:

```
cargo bench --bench incremental
JRS_BENCH_CLASSES=1000 JRS_BENCH_RUNS=7 cargo bench --bench incremental
```

The `kotlinc` benchmark copies a Kotlin project, builds it once, then edits
one Kotlin source and builds again, `JRS_BENCH_RUNS` times for a body edit and
for an API edit. For each it prints the `kotlinc` and `javac` rows of
`--timings`, the floor — the same JVM compiling one empty file — and what the
same compile takes warm, run a second time in one JVM; `kotlinc − warm` is
what the `--watch` worker saves:

```
JRS_BENCH_PROJECT=examples/orders cargo bench --bench kotlinc
```

The corpus takes the real projects listed in `specs/corpus/corpus.toml` — Spring
Boot and Ktor applications on Maven and on Gradle, each cloned at a pinned
commit — through `jrs migrate`, `build`, `test`, `run` and `package` with no
hand edits, and checks each against the project's own build: the same
runtime classpath, and the same tests run, passed and skipped. It needs
`git`, the network, a JDK, and Maven or Gradle for a project without a
wrapper; Docker is used when it is running. It does nothing unless
`JRS_CORPUS` names the projects, and keeps everything under
`target/corpus/<run>/`, with the table in `REPORT.md` there. The
[Compatibility](#compatibility) section is written from it, and
`specs/corpus/FINDINGS.md` keeps what it found:

```
JRS_CORPUS=all cargo bench --bench corpus
JRS_CORPUS=petclinic-maven,ktor-serialization cargo bench --bench corpus
JRS_CORPUS=all JRS_CORPUS_STEPS=migrate,build cargo bench --bench corpus
```

The comparison with Maven and Gradle builds, tests, runs and packages the same
projects with each tool and writes `benchmarks/REPORT.md`, with every sample
beside it in `benchmarks/raw/`. Its parity set is `examples/wordstats` and
`examples/bookmarks`, with the Maven and Gradle builds of
`benchmarks/projects/` laid over them, and three generated projects
(`gen-300`, `gen-2000` and the six-member workspace `gen-ws`); the corpus
projects whose last corpus run passed every step join them. Before it times a
project it checks that every tool resolves the same runtime classpath, runs
the same tests, and starts the same application and jar; a project that fails
the check is reported, not timed. Gradle is timed three ways: with a warm
daemon, with the daemon and `--configuration-cache`, and with `--no-daemon`;
a multi-module project adds a fourth, the daemon with `--parallel`. For a
workspace the report also sets each member's time in jrs's clean build,
summed and along the critical path through the member graph, beside jrs's
and Gradle's `--parallel` clean builds.

No timed run touches the network: every tool reads a local mirror of Maven
Central and the Gradle Plugin Portal, which one recording run fills. It needs
a JDK, `mvn` and `gradle` (9 or later) on `PATH`; a tool that is missing is
skipped with a `SKIPPED` line:

```
JRS_BENCH_RECORD=1 cargo bench --bench compare        # fill ~/.cache/jrs-bench/mirror
cargo bench --bench compare
JRS_BENCH_TOOLS=jrs,gradle JRS_BENCH_PROJECTS=gen-300 \
  JRS_BENCH_SCENARIOS=noop,incremental JRS_BENCH_RUNS=11 cargo bench --bench compare
```

`JRS_BENCH_WARMUP` (default 2), `JRS_BENCH_RUNS` (7), `JRS_BENCH_MIRROR`,
`JRS_BENCH_LATENCY_MS` and `JRS_BENCH_REPORT` adjust it; the scenarios are
`cold`, `clean`, `noop`, `incremental`, `abi`, `test`, `test-edit`, `run`
and `package`. Logs stay under `target/compare/<run>/`.

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
