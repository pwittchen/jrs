# jrs — Kotlin, Scala and Groovy

Design proposal for compiling Kotlin, Scala and Groovy sources alongside Java,
in the same single-module project and from the same `jrs.toml`.

Status: **implemented**, as milestone M8 of [SPEC §12](INITIAL_SPEC.md#12-roadmap);
[SPEC §7.7](INITIAL_SPEC.md#77-other-jvm-languages) is the condensed contract.
This removed a line drawn in [SPEC §1.2](INITIAL_SPEC.md#12-non-goals)
("Non-Java JVM languages (Kotlin, Scala, Groovy)"), so it needed an
INITIAL_SPEC.md change before it needed code. §2 below is that argument; §12
lists the INITIAL_SPEC.md edits it implied, which have been made.

Where this document states how a third-party compiler behaves, the claim was
checked by the L0 spike (§13.2) before code depended on it. What the spike
found is in §15, and the text below has been corrected where the proposal
guessed wrong. Scaladoc and Groovydoc for `jrs doc` (§9) are the one part not
built.

---

## 1. Problem

A large share of JVM projects are not pure Java:

- **Kotlin** is the default for new Android and a growing part of server work
  (Spring, Ktor), often mixed with an existing Java codebase.
- **Groovy** is mostly found in tests. Spock is the reason a Java team has
  `src/test/groovy`.
- **Scala** projects are usually all Scala, but they depend on Java libraries
  and sometimes carry Java sources.

Today jrs globs `**/*.java` and runs `javac`. A `.kt` file under `src/main/java`
is ignored without a word. The only way out is to leave jrs for Gradle, Maven
or sbt, which means leaving the manifest, the lockfile, the pinned JDK and the
rest of what jrs already does.

What these languages need from a build tool is small next to what jrs already
has. Their compilers are **JVM programs published on Maven Central**. Their
runtime libraries are **ordinary dependencies**. Their output is **`.class`
files** that the jar writer, the runner, the JUnit launcher, JaCoCo and `jlink`
already handle. The missing piece is one compile step before `javac`.

## 2. Position on the non-goal

SPEC §1.2 lists non-Java languages as a non-goal. This proposal lifts that for
the three named languages, **on the JVM only**, and keeps the rest:

| Still out | Why |
| --- | --- |
| Kotlin Multiplatform, Kotlin/JS, Kotlin/Native, Scala.js, Scala Native, Android | Not the JVM. Different toolchains, different outputs, often Gradle Module Metadata (ROADMAP §4). |
| `kapt`, KSP, Compose, macro-paradise style setups | Annotation-processor or compiler-plugin *configuration*, already a non-goal. Java annotation processing in a mixed project keeps working (it is `javac`'s). |
| Incremental compilation (Zinc, Kotlin IC) | SPEC §7.2 is all-or-nothing on purpose. |
| Compiler daemons (Kotlin daemon, Bloop) | A long-lived process jrs would have to manage. See §14.1. |
| sbt-style `%%` cross-version keys | New key syntax. See §14.3. |
| Other JVM languages (Clojure, JRuby, Jython, …) | No demand has shown up, and each has its own model (Clojure AOT, a scripting runtime). The design in §6 leaves room for them without promising them. |
| A build DSL | Still no. The manifest is TOML. |

The deciding argument is §1.1's "thin, predictable driver over the JDK
toolchain". A Kotlin, Scala or Groovy compiler is a Java program. jrs runs it
on the **project's own JDK** (SPEC §7.1), with an argfile, and passes its
diagnostics through verbatim. That is how it already runs `javac`, the JUnit
launcher and JaCoCo. No compiler code runs inside jrs, and no crate is added.

### 2.1 Alternatives considered

- **Compilers from `PATH`** (`kotlinc`, `scalac`, `groovyc`), like `javac`.
  That is simple, but the compiler version then belongs to the machine rather
  than the project. A `kotlinc` from SDKMAN! also brings its own stdlib, which
  fights the resolved one. The JDK is the exception because it is not on
  Maven Central. These compilers are.
- **Detect the language by file extension alone, with no manifest key.** This
  needs no configuration, but the compiler version has to be pinned
  somewhere, and the committed manifest is the only place that is not
  per-machine (SPEC §8.5).
- **Derive the compiler version from a declared stdlib**, the way the JUnit
  launcher follows the declared Jupiter version (SPEC §10.2). This is tempting
  and consistent, but the stdlib can arrive transitively, Scala 3's library is
  called `scala3-library_3`, and each language needs a home for its compiler
  flags anyway. An explicit table (§4) is clearer and costs one line.
- **Delegate to Gradle, Maven or sbt for non-Java sources.** That would make jrs
  a wrapper around the build tools it replaces (§1.2's "not a drop-in
  replacement").

---

## 3. Overview

```toml
[project]
name = "orders"
version = "1.0.0"
main-class = "com.example.orders.MainKt"

[java]
source = 21

[kotlin]
version = "2.2.0"

[dependencies]
"io.ktor:ktor-server-netty-jvm" = "3.2.0"

[dev-dependencies]
"org.junit.jupiter:junit-jupiter" = "5.13.4"
"org.jetbrains.kotlin:kotlin-test-junit5" = "2.2.0"
```

```
src/main/kotlin/com/example/orders/Main.kt
src/main/kotlin/com/example/orders/Order.kt
src/main/java/com/example/orders/LegacyPricing.java     # may use Order, and vice versa
src/test/kotlin/com/example/orders/OrderTest.kt
```

```
$ jrs build
    Resolving 3 declared dependencies
  Downloading kotlin-compiler-embeddable (Kotlin compiler)
    Compiling orders v1.0.0 (2 Kotlin + 1 Java source files)
     Finished build in 6.84s
```

Everything after `Compiling` works unchanged: `run`, `test`, `package`
(thin, portable, fat, `--jlink`, `--jpackage`), `test --coverage`, `tree`,
`classpath`, `--watch`.

The model has three parts:

1. **A language table** (`[kotlin]`, `[scala]`, `[groovy]`) turns the language
   on and pins its compiler version.
2. **The compiler** is an internal tool: resolved from Maven Central as its own
   graph, pinned in `jrs.lock`, run on the project's JDK.
3. **The runtime library** (`kotlin-stdlib`, `scala-library`, `groovy`) is an
   implied project dependency at the same version, unless the manifest declares
   it itself.

---

## 4. Manifest format

### 4.1 Language tables

```toml
[kotlin]
version = "2.2.0"                        # required: the compiler, and the implied stdlib
kotlinc-args = ["-Xjsr305=strict"]       # appended verbatim, after jrs's own flags
compiler-jvm-args = ["-Xmx2g"]           # for the JVM the compiler runs in
source-dir = "src/main/kotlin"           # default
test-dir = "src/test/kotlin"             # default

[scala]
version = "3.7.1"
scalac-args = ["-deprecation", "-feature"]

[groovy]
version = "4.0.27"
groovyc-args = ["--compile-static"]
```

| Key | Required | Default | Notes |
| --- | --- | --- | --- |
| `<lang>.version` | yes | — | Exact version, as dependency versions are. Ranges rejected (SPEC §8.2). |
| `<lang>.source-dir` | no | `src/main/<lang>` | An extra main source root. |
| `<lang>.test-dir` | no | `src/test/<lang>` | An extra test source root. |
| `kotlin.kotlinc-args` / `scala.scalac-args` / `groovy.groovyc-args` | no | `[]` | Appended verbatim after jrs's flags, like `java.javac-args`. |
| `<lang>.compiler-jvm-args` | no | `[]` | `java` flags for the compiler's JVM: heap and stack size mostly. `-Xss` matters for scalac. |

The table's presence turns the language on. An empty `[kotlin]` without a
`version` is a manifest error that names the key. Unknown keys warn (SPEC §4.3).

**Minimum versions** keep the flag tables in §6 small. Anything older is an
error naming the minimum:

| Language | Minimum | Why |
| --- | --- | --- |
| Kotlin | 2.0 | The K2 compiler; `-Xjdk-release` and current `-jvm-target` values. |
| Scala | 2.13.9 and 3.3 (LTS) | `-release <n>` on both lines. 2.12 is out. |
| Groovy | 4.0 | The `org.apache.groovy` group; the JPMS-clean split packages. |

### 4.2 Source roots

With a language on, each compile unit globs more than one root and more than
one extension:

| Unit | Roots | Extensions |
| --- | --- | --- |
| main | `project.source-dir`, each enabled `<lang>.source-dir` | `.java`, and each enabled language's (`.kt`, `.scala`, `.groovy`) |
| test | `project.test-dir`, each enabled `<lang>.test-dir` | the same |

Every root is scanned for every enabled extension, so `.kt` files under
`src/main/java` compile too, as they do under Gradle's Kotlin plugin. The walk
is the existing sorted `project::walk`, so argfiles stay deterministic. `.kts`
scripts are never sources.

Two rules, checked after globbing and before any compiler runs:

- **A source file of a language that is not enabled is an error**, not a
  silent skip: `found 3 .kt files under src/main/kotlin, but jrs.toml has no
  [kotlin] table`, with the three lines to add. Today such files are ignored,
  which is the bug a user hits first.
- **At most one non-Java language per compile unit.** Kotlin with Java, or Scala
  with Java, can see each other's types (§6). Kotlin and Scala in one unit
  cannot, since neither compiler reads the other's sources. Main and test are
  separate units, so **Kotlin or Java main code with Groovy (Spock) tests is
  fine**, and that is the common case this rule must not break.

### 4.3 The implied runtime library

Code compiled by these compilers links against a runtime library:

| Language | Implied dependency |
| --- | --- |
| Kotlin | `org.jetbrains.kotlin:kotlin-stdlib:<version>` |
| Scala 2.13 | `org.scala-lang:scala-library:<version>` |
| Scala 3.0–3.7 | `org.scala-lang:scala3-library_3:<version>` |
| Scala 3.8+ | `org.scala-lang:scala3-library_3:<version>` and `org.scala-lang:scala-library:<version>`: 3.8 rebuilt the standard library with Scala 3 as `scala-library` 3.x, and left `scala3-library_3` a shim that depends on it (§15). The mapping is a per-version table in `compile/lang.rs`, not a formula. |
| Groovy | `org.apache.groovy:groovy:<version>` |

- It joins `[dependencies]` as if declared **last**, so it is a direct
  dependency at depth 1 and wins nearest-wins mediation against transitive
  copies (SPEC §8.2). If two versions are tied at the same depth, a declared
  dependency beats it, because it comes later in declaration order.
- **An explicit declaration wins**, in either table. Declaring
  `org.apache.groovy:groovy` under `[dev-dependencies]` is how a project with
  only Groovy *tests* keeps Groovy off its runtime classpath and out of its fat
  jar. A declared version that differs from `<lang>.version` gets a warning,
  because a stdlib newer than its compiler is rejected by kotlinc and scalac.
- It is **not written into `jrs.toml`**. `render()`, `jrs add` and `jrs remove`
  never see it. `jrs tree` shows it labelled `(implied by [kotlin])`.
- `Manifest::effective_dependencies()` is what `resolve::resolve` and
  `lockfile::manifest_checksum` read. The checksum also gains one
  `lang <name> <version>` line per enabled language, since the compiler's
  pinned graph (§5) depends on it. A Java-only manifest adds no lines, so its
  existing lockfile still matches.

---

## 5. The compiler as an internal tool

### 5.1 Resolution

The JUnit launcher and JaCoCo are single, self-contained jars, so
`fetcher.jar(coord)` is enough for them. Compilers are not self-contained:

| Language | Compiler artifact | Main class |
| --- | --- | --- |
| Kotlin | `org.jetbrains.kotlin:kotlin-compiler-embeddable` | `org.jetbrains.kotlin.cli.jvm.K2JVMCompiler` |
| Scala 2.13 | `org.scala-lang:scala-compiler` | `scala.tools.nsc.Main` |
| Scala 3 | `org.scala-lang:scala3-compiler_3` | `dotty.tools.dotc.Main` |
| Groovy | `org.apache.groovy:groovy` (the compiler is in the core jar) | `org.codehaus.groovy.tools.FileSystemCompiler` |

Each compiler is resolved **as its own graph**, never merged into the
project's. The Kotlin compiler's own `kotlinx-coroutines` must not mediate
against the project's. Internally this is `resolve::resolve` over a synthetic
manifest (`manifest::blank` plus the project's repositories), which is exactly
the isolated tool graph that [TASKS.md §8](TASKS.md#8-tool-dependencies-a-later-milestone)
proposes for task dependencies. **Both proposals share one mechanism**, and
whichever lands first builds it.

### 5.2 Pinning

A compiler decides the bytecode, so an unpinned one is not reproducible. Its
graph goes into `jrs.lock` beside the project's:

```toml
version = 2
manifest-checksum = "sha256:…"

[[package]]
# … the project's graph, unchanged …

[[tool]]
name = "kotlin-compiler"
[[tool.package]]
group = "org.jetbrains.kotlin"
artifact = "kotlin-compiler-embeddable"
version = "2.2.0"
checksum = "sha256:…"
# … its transitives, in the existing package format …
```

- Checksums are pins, as in SPEC §4.4: a download that does not match is
  discarded.
- **`version = 2` only when `[[tool]]` blocks exist.** A Java-only project keeps
  a byte-identical version-1 lockfile. An older jrs meeting a version-2
  lockfile refuses it with its existing "run `jrs update`" error, instead of
  silently dropping the pins on rewrite. That also answers TASKS.md §12.6.
- `jrs cache prune` keeps what `[[tool]]` names. `jrs verify` re-hashes it.
  `jrs update` re-resolves it.
- The UI shows it like the test launcher: `Downloading kotlin-compiler-embeddable
  (Kotlin compiler)`, one phase line per tool, then the shared download bars.
  `--offline` with a cold cache fails with the usual cache-miss error.

### 5.3 Invocation

```
java @target/.jrs/kotlinc-main.args
```

- `java` is the **selected toolchain's** `java`, so the compiler sees the pinned
  JDK's class library, and `-jdk-home` / `-release` agree with it.
- **One argfile holds everything**: `compiler-jvm-args`, `-cp` and the
  compiler's own classpath, its main class, its flags, and the sources. The
  `java` launcher has read `@argfiles` since JDK 9, and it hands the arguments
  after the main class to the program as they are in the file, quoted as
  `javac`'s argfiles are. So `render_argfile` serves every compiler, and the
  "argfiles, not command lines" rule holds without the compilers' help.
- The proposal had each compiler read its own `@file`, with a `Dialect` per
  compiler. The spike found the dialects disagree in ways that matter: scalac
  keeps backslashes, which mangles every Windows path, and groovyc's `@file`
  lists source files only, never options (§15). One launcher argfile sidesteps
  all three.
- Output goes through `run_captured` and is replayed verbatim, as `javac`'s
  is (SPEC §6.2).

---

## 6. Compilation

### 6.1 One unit, several steps

`CompileUnit` today is one `javac` run. It becomes an ordered list of steps
that share one output directory, one fingerprint and one staleness decision:

| Unit contains | Step 1 | Step 2 |
| --- | --- | --- |
| Java only | `javac` | — |
| Kotlin (+ Java) | `kotlinc` over `.kt` **and** `.java` files; it reads the Java for symbols and emits only Kotlin classes | `javac` over `.java`, with step 1's output on `-cp` |
| Scala (+ Java) | `scalac` over `.scala` **and** `.java` files; it parses the Java for signatures | `javac` over `.java`, with step 1's output on `-cp` |
| Groovy (+ Java) | `groovyc -j` (joint compilation); it generates stubs and runs the JDK's `javac` itself | — |

- Step 2 is skipped when there are no `.java` files. Step 1 is skipped when
  there are no files in the language.
- **Still all-or-nothing** (SPEC §7.2). Any change reruns every step into an
  emptied output directory. The fingerprint concatenates every step's flags,
  every step's tool classpath (jar size and mtime, as today) and the full
  source list. `is_stale` does not change otherwise.
- The phase line counts by language: `Compiling orders v1.0.0 (2 Kotlin + 1
  Java source files)`. A single-language unit reads as today.
- A failure in step 1 stops the unit: `compilation failed (2 Kotlin source
  files)`. Its diagnostics have already been passed through.

### 6.2 One release for every language

`java.source`, or the JDK's version when it is unset, is the one target for
the project. Mixed bytecode levels in one jar are a known source of runtime
`UnsupportedClassVersionError`, so there is no per-language target:

| Language | Flags jrs generates |
| --- | --- |
| Java | `--release <n>` (or `-source`/`-target`, as today) |
| Kotlin | `-jvm-target <n> -Xjdk-release=<n>` |
| Scala | `-release <n>` on 2.13; `-java-output-version <n>` on 3, the new name of the same flag, which 3.3 has too |
| Groovy | `-Dgroovy.target.bytecode=<n>` for groovyc's JVM, since groovyc has no flag for it, and `-J=-release=<n>`, which reaches the joint `javac` as `--release <n>` |

A Kotlin version older than the JDK may not know `-jvm-target 25`. kotlinc's
own error is passed through, and jrs adds one line: lower `java.source`, or
raise `kotlin.version`. jrs does not clamp silently.

### 6.3 Per-language details

**Kotlin**

- `-no-stdlib -no-reflect`, with the *resolved* `kotlin-stdlib` on
  `-classpath`. Without these flags, kotlinc adds the stdlib from its own
  distribution, which can differ from the one the program runs with.
- `-module-name <project.name>` for main. The test unit uses
  `<project.name>_test` with `-Xfriend-paths=target/classes`, so tests can see
  `internal` declarations, as they can under Gradle.
- kotlinc reads sources as UTF-8 only. A `java.encoding` other than UTF-8 in a
  Kotlin project gets a warning.
- A `main` function at file level compiles to `<File>Kt`. When `jrs run` finds
  no class for `main-class` but finds `<main-class>Kt`, the error suggests it.

**Scala**

- `-encoding <java.encoding>`, `-classpath`, `-d`.
- Resolution warns when a Scala 2 project's resolved `scala-library` is newer
  than its compiler (the rule sbt enforces), and when two cross-built variants
  of one library, `foo_2.13` and `foo_3`, are both on the classpath.

**Groovy**

- `--encoding=<java.encoding>`, `-cp` (first: groovyc wants it there), `-d`,
  and `-j` when the unit has `.java` files. Without `-j`, groovyc compiles
  `.java` files as Groovy source.
- In joint mode, jrs's own `javac` flags are translated exactly.
  `java.javac-args` go to the embedded `javac` by a fixed rule. groovyc puts
  the leading `-` back itself, so a single-token flag `-X` becomes `-F=X`, and
  a `-key value` pair listed in the rule's table becomes `-J=key=value`. A
  `--key value` pair becomes the one token `-F=-key=value`, which javac reads
  as `--key=value`. An argument the rule cannot place is a manifest error that
  names it. jrs does not guess.

### 6.4 Code layout

The per-language knowledge sits in plain data, matched on an enum, in the
style of the rest of the codebase. There is no trait-object registry.

```
src/compile/
├── mod.rs        # CompileUnit (steps), fingerprint, is_stale, argfile dialects
├── javac.rs      # today's compile.rs body; javadoc stays here too
└── lang.rs       # enum Language { Java, Kotlin, Scala, Groovy }:
                  #   extension, default dirs, compiler coordinate + main class,
                  #   runtime library, minimum version, flags(unit) -> Vec<String>
```

`compile.rs` becomes `compile/mod.rs`. Its public API (`compile`, `is_stale`,
`javadoc`, `class_file`, `render_argfile`) keeps its names, so `cli.rs` and
the tests move with little churn.

---

## 7. Testing (`jrs test`)

SPEC §13.7 holds: **the JUnit Platform only**. That covers these languages'
test frameworks:

| Framework | How it runs on the JUnit Platform |
| --- | --- |
| JUnit 5 / `kotlin-test-junit5` | Jupiter. Works today. |
| Spock 2 (Groovy) | Its own Platform engine (`spock-core`). |
| Kotest | Its Platform engine (`kotest-runner-junit5`). |
| ScalaTest | Its Platform engine (`org.scalatestplus:junit-5-*`). |
| MUnit (Scala) | A JUnit 4 runner, so Vintage, which the launcher bundles. |

Three changes make these work:

1. **The launcher follows the resolved graph, not only the declared one.** Today
   `launcher_coordinate` reads `dev-dependencies`. Spock and Kotest bring
   `junit-platform-engine` transitively, and MUnit brings `junit:junit`. The
   launcher version then comes from the resolved `junit-platform-engine`
   (falling back to today's rules), so the standalone launcher and the
   engine's platform agree.
2. **The default class-name filter includes `.*Spec` and `.*Suite`** when
   the test unit has non-Java sources. Spock and Kotest specs are named
   `*Spec`, ScalaTest and MUnit suites `*Suite`, and the launcher's default
   pattern only matches `*Test`/`*Tests`/`Test*`, so they are skipped: the
   spike found zero tests in a Spock spec and in an MUnit suite until the
   pattern had them (§15). `--filter` still replaces it.
3. **Coverage reads every source root.** `CoverageReport.sources` becomes a
   list, so JaCoCo's HTML can show Kotlin, Scala and Groovy files. JaCoCo
   already filters Kotlin's synthetic code.

---

## 8. Packaging, running, images

These are unchanged, since the output is class files, with one exception:

- **Groovy extension modules in fat jars.**
  `META-INF/groovy/org.codehaus.groovy.runtime.ExtensionModule` (and its
  pre-2.5 `META-INF/services/` location) is a properties file that each Groovy
  module ships (`groovy-json`, `groovy-sql`, …). First-wins keeps one
  module's extension methods and silently drops the rest, which is the same
  failure SPEC §9.2 guards against for `META-INF/services`. The merge rule
  unions `extensionClasses` and `staticExtensionClasses` (comma lists) and
  writes one descriptor. It is a new branch beside `is_service_file` in
  `package.rs`.
- Kotlin's `META-INF/*.kotlin_module` files are named per module, so they do
  not collide.
- `jlink`/`jdeps` read bytecode. `kotlin-stdlib` has a `module-info`, and
  `scala-library` gets an automatic module name. Nothing changes, and the
  network suite packages one image per language to confirm it.

---

## 9. Documentation (`jrs doc`)

v1 documents **Java sources only**, as today. When a unit has sources in other
languages, a warning names what was left out. Documenting them means a second
tool for each language:

- Scaladoc ships inside the Scala compiler (`scala.tools.nsc.ScalaDoc`, Scala
  3's `-doc` mode), so it costs an invocation and no new graph.
- Groovydoc is `org.apache.groovy:groovy-groovydoc`, a small graph.
- Dokka is a CLI plus plugins with a configuration file. It is heavy, and it
  is an open question (§14.4).

---

## 10. Scaffolding and migration

**`jrs init --lang kotlin|scala|groovy`** scaffolds the language table, a
starter in `src/main/<lang>`, and a starter test:

| `--lang` | Test dependency (pinned in the template, like the JUnit version today) |
| --- | --- |
| `kotlin` | `junit-jupiter` + `kotlin-test-junit5` |
| `scala` | `munit_3` |
| `groovy` | a Java main class, with `spock-core` (`-groovy-4.0`) tests, since tests are where Groovy usually lives |

`--lang java` is the default. Completions pick up the flag from the clap
definition.

**`jrs migrate`** reads what the builds already say:

| Source | Construct | Becomes |
| --- | --- | --- |
| Gradle | `kotlin("jvm") version "x"`, `id("org.jetbrains.kotlin.jvm") version "x"` | `[kotlin] version = "x"` |
| Gradle | `kotlin { jvmToolchain(n) }` | `java.jdk = n` |
| Gradle | `id 'groovy'` / `id 'scala'` + the library dependency | `[groovy]` / `[scala]`, version from that dependency |
| Maven | `kotlin-maven-plugin` + `${kotlin.version}` | `[kotlin]`; `<jvmTarget>` → `java.source` when it is not set |
| Maven | `scala-maven-plugin`, `gmavenplus-plugin` | `[scala]` / `[groovy]` |
| both | an explicit `kotlin-stdlib` / `scala-library` / `groovy` dependency at the compiler's version | dropped from `[dependencies]`, since it is implied now; reported under "Migrated" |
| both | compiler plugins (`allopen`, `spring`, `serialization`, `kapt`) | "Not migrated", each with its reason |

The three Maven plugins join `UNDERSTOOD_PLUGINS`. Fixtures follow SPEC §11.5:
an input build paired with its expected `jrs.toml`.

---

## 11. Everything else that has to notice

| Where | Change |
| --- | --- |
| `--watch` | `watched_paths` gains every enabled `<lang>.source-dir` / `test-dir`. |
| `jrs tree` | The implied library is labelled. `jrs tree --tool kotlin-compiler` prints a compiler graph, sharing TASKS.md's `--task` flag design. |
| `jrs outdated` | Lists `<lang>.version` against the compiler artifact's `maven-metadata.xml`. |
| `jrs add` | Warns when a `_2.13`/`_3` suffix does not match the Scala line in `[scala]`. It does not rewrite the suffix. |
| Resolution warnings | A `kotlin-stdlib-jdk7`/`-jdk8` older than 1.8 beside a Kotlin 2 stdlib duplicates classes. The warning names the fix, which is to declare it at `kotlin.version` so nearest-wins picks that. jrs does not align versions implicitly, since that would be a second mediation rule. |
| Kotlin Multiplatform libraries | Their root artifact's POM may not point at the `-jvm` variant, because Gradle Module Metadata does that. The spike found both kinds (§15): `kotlinx-coroutines-core`'s root POM is `pom`-packaged and depends on `-jvm`, so it resolves as is, while `kotlinx-datetime`'s does not. Documented: when a root does not resolve to classes, declare the `-jvm` artifact. The general fix is ROADMAP §4's `.module` row. |

---

## 12. INITIAL_SPEC.md changes

1. **§1.2**, replace "Non-Java JVM languages (Kotlin, Scala, Groovy)" with:
   > - JVM languages beyond Java, Kotlin, Scala and Groovy; and for those three,
   >   anything off the JVM (Multiplatform, JS, Native, Android), compiler
   >   plugins, kapt/KSP, incremental compilation and compiler daemons.
2. **§1.1 / title / README / CLAUDE.md "What this is"**: jrs stays "a Java
   build system": Java is the default and the JDK is the toolchain, and
   Kotlin, Scala and Groovy compile alongside it.
3. **§3**, show `src/main/kotlin` etc. in the layout.
4. **§4.1 / §4.2**, the language tables and keys (§4.1 here). **§4.4**, the
   `[[tool]]` blocks and the version-2 rule.
5. **§5.1**, `jrs init --lang`.
6. **§6**, the `compile/` module tree.
7. **New §7.6 "Other JVM languages"**, condensed from §4–§6 of this document.
   **§7.2** points to it for multi-step units.
8. **§9.2**, the Groovy extension-module merge rule.
9. **§10.2**, the launcher-from-resolved-graph rule and the `*Spec` pattern.
10. **§11**, the migration rows in §10.
11. **§12**, milestone **M8 — JVM languages** (§13.2 below).
12. **§13**, a new question recording the non-goal decision and §2's reasoning.
13. **ROADMAP §4**, remove the "Kotlin or other JVM languages" row.

No new crates. `std::process` runs the compilers, `resolve` resolves them,
and `zip` merges the Groovy descriptors.

---

## 13. Implementation plan

### 13.1 Code layout

| Where | What |
| --- | --- |
| `manifest.rs` | `LanguageConfig { version, source_dir, test_dir, compiler_args, compiler_jvm_args }` per language, parsed by hand like `[java]`. Minimum-version check. `effective_dependencies()` with the implied library, flagged `implied` so `render()` skips it. |
| `compile/` | §6.4. `CompileUnit` grows `steps`, and the fingerprint covers them all. `Dialect`-aware argfiles. |
| `project.rs` | `main_sources()` / `test_sources()` return a `Sources` value: roots × extensions, grouped by language. Also the "language not enabled" and "one non-Java language per unit" checks. |
| `resolve/` | `resolve_tool(name, coords, repositories, fetcher)`, the isolated graph shared with TASKS.md T3. |
| `lockfile.rs` | `[[tool]]` read/write, version 2 only when present, the `lang` lines in `manifest_checksum`. |
| `cli.rs` | `Session::tools()` (lockfile-first, like `dependencies()`). `build()` and `test_command` build multi-step units. The phase-line wording. `init --lang`. `watched_paths`. |
| `test.rs` | Launcher from the resolved graph, the default `*Spec` pattern, multi-root coverage sources. |
| `package.rs` | The Groovy extension-module merge. |
| `migrate/` | §10's rows. |

The layer rules in CLAUDE.md hold: `compile/` and `resolve/` know nothing about
terminals, only `ui/` prints, and phase lines come from `cli.rs`.

### 13.2 Milestones

**L0 — Foundations and spike.** Before any language ships:
- A one-day spike that runs each real compiler by hand on JDK 17 and 25, on
  Linux and Windows. It settles every *(verify)* in this document: argfile
  dialects, Groovy's `-J`/`-F` and target flags, Spock and the class-name
  filter, the Scala 3.8 library name, and KMP root POMs. The findings are
  written back into this file.
- Multi-step `CompileUnit`, `Language` with only `Java` wired, and multi-root
  `Sources`. Pure refactor: no behaviour change, and the existing suites stay
  green.
- The isolated tool graph and `[[tool]]` in the lockfile. If TASKS.md T3 has
  not landed, this is its first half.
- The fake-compiler fixture (§13.3).

**L1 — Kotlin.** `[kotlin]`, the implied stdlib, kotlinc invocation, mixed
Kotlin/Java both ways, the test unit with friend paths, the `MainKt` hint, the
UTF-8 warning, the network test, and the INITIAL_SPEC.md changes. Kotlin comes first
because it is the most asked-for.

**L2 — Groovy.** `[groovy]`, joint compilation, the fat-jar descriptor merge,
Spock through the resolved-graph launcher and the `*Spec` pattern. It is the
cheapest compiler, since it is one jar, and it is what Java teams with Spock
tests need.

**L3 — Scala.** `[scala]` for 2.13 and 3, mixed Scala/Java, the two resolution
warnings, and MUnit through Vintage and ScalaTest through its engine.

**L4 — Ecosystem.** `jrs init --lang`, the `jrs migrate` rows, `jrs outdated`
for compiler versions, `jrs tree --tool`, Scaladoc and Groovydoc for `jrs doc`.
Last, for the same reason migration was M6: it translates into features that
have to exist first.

Each milestone ends with its README box ticked and SPEC §12 updated, as the
existing milestones did.

### 13.3 Tests

The default `cargo test` must stay hermetic (CLAUDE.md), and a Kotlin
compiler is about 60 MB of jars. The answer is a **fake compiler**:

- `tests/fixtures/fake-compiler/` holds a few dozen lines of Java: one entry class
  per real main class name (`org.jetbrains.kotlin.cli.jvm.K2JVMCompiler`,
  `dotty.tools.dotc.Main`, `scala.tools.nsc.Main`,
  `org.codehaus.groovy.tools.FileSystemCompiler`). It reads its `@argfile`,
  records the argv to a file, and compiles the "foreign" sources with
  `javax.tools`. The fixture's `.kt` / `.scala` / `.groovy` files contain
  valid Java.
- At test time (`require_jdk!`) it is compiled, jarred and published into the
  `FixtureRepo` as each compiler artifact at a fixture version, with a POM
  that gives it one transitive dependency, plus a fake runtime library with
  one class. Nothing binary is committed, as today.
- That exercises the real pipeline end to end on all three CI operating
  systems: isolated resolution, `[[tool]]` pinning, running on the selected
  JDK, argfile dialects, step order (Java referencing "Kotlin" classes and the
  reverse), the implied library on the runtime classpath, run, test,
  fat jar, and determinism.

The layers:

- **Unit (`manifest.rs`)**: each table parses; a missing `version` names the
  key; below-minimum versions are rejected; the implied library is added,
  overridden by an explicit declaration in either table, and never rendered;
  the checksum changes with `kotlin.version` and does not change for a
  Java-only manifest.
- **Unit (`compile/`)**: each language's flags for main and test (friend
  paths, `-no-stdlib`, release mapping); the Groovy `javac-args` rule and its
  refusals; each argfile dialect with spaces and backslashes; a fingerprint
  that changes with any step's flags or tool jar.
- **Unit (`project.rs`)**: roots × extensions, `.kt` under `src/main/java`,
  the not-enabled error, the two-languages-in-one-unit error, and a Kotlin
  main with Groovy tests allowed.
- **Unit (`lockfile.rs`)**: a `[[tool]]` round trip; version 1 without tools
  and 2 with them; an old-version lockfile with tools refused.
- **Unit (`package.rs`)**: two Groovy extension descriptors merge into one with
  both class lists.
- **Unit (`test.rs`)**: the launcher version from a resolved
  `junit-platform-engine`; Vintage from a transitive `junit:junit`; the
  `*Spec` pattern only with non-Java tests.
- **`tests/output.rs`**: the mixed-language `Compiling` line, the
  compiler-download phase line, and the ASCII fallback.
- **`tests/build.rs`** (fake compiler): the end-to-end list above, plus an
  unchanged rebuild that is `Fresh` without running either step, a touched
  `.kt` that reruns both, and byte-identical jars from two builds.
- **`tests/network.rs`** (`network-tests`, Linux CI): one small mixed project
  per language, built, tested with its usual framework (JUnit 5, Spock, MUnit),
  fat-jarred, and run with real compilers from Maven Central. Two builds are
  compared for byte-identical jars, which is the only place real compilers'
  determinism is checked.
- **`benches/`**: compiler JVM start-up per language, measured with the M5
  harness, which gives §14.1 a number to decide on.

---

## 14. Open questions

1. **Compiler start-up.** kotlinc and scalac spend seconds starting a cold JVM,
  and `javac` does not. The Kotlin daemon or a Zinc/Bloop server would hide
  that, but either means jrs managing a long-lived process. Proposed: measure
  it in L0's bench first, then decide. The spike measured instead of a bench
  (§15): one small mixed unit takes 1.8 s in kotlinc, 0.9 s in scalac 3,
  0.7 s in scalac 2.13 and 0.5 s in groovyc, wall clock, on JDK 23. A second
  of JVM start-up per changed build is noticeable next to `javac`, but not
  yet worth a daemon: no daemon for now.
2. **Compiler plugins.** Spring projects in Kotlin need `allopen`/`spring`
   (classes are final by default), and `kotlinx.serialization` needs its
   plugin. Both are Maven artifacts passed as `-Xplugin=<jar>`, so the
   mechanics are cheap: `kotlin.compiler-plugins` as coordinates, resolved into
   the compiler's tool graph. But it edges toward the plugin-configuration
   non-goal. Proposed: not in L1. Revisit it before calling Kotlin
   production-ready, since Kotlin on Spring is a large share of Kotlin on the
   server.
3. **Cross-version keys.** sbt writes `"org.typelevel" %% "cats-core"` and fills
   in `_3`. A `"org.typelevel::cats-core"` key would do the same in jrs, but it
   touches key parsing, `edit.rs`, the lockfile and migration. Proposed: no;
   write the suffix. `jrs add` warns on a mismatched one.
4. **Dokka.** It is the only credible Kotlin doc tool, and it is a plugin host
   with its own configuration. Proposed: leave Kotlin out of `jrs doc` and say
   so, unless there is demand.
5. **`*Spec` by default everywhere.** Should the broader class-name pattern
   apply to Java-only projects too, for consistency? That would start running
   Java classes named `*Spec` that do not run today. Proposed: only with
   non-Java test sources.
6. **Scala 2.12.** It is still used by Spark. It would need
   `-target:jvm-1.8`-era flags and a second Scala 2 table. Proposed: out unless
   asked for.

---

## 15. What the L0 spike found

Every *(verify)* in the proposal was run by hand before the code depended on
it. The setup was JDK 23 on macOS (arm64), with Kotlin 2.4.20, Scala 3.9.0,
3.3.6 and 2.13.18, Groovy 5.1.2 and 4.0.28, Spock `2.4-groovy-5.0`, MUnit
1.3.6, and the JUnit console launcher 1.14.1. Each compiler got a mixed source
set that used Java both ways, through an argfile whose output path held a
space and a literal backslash.

| Question | Finding |
| --- | --- |
| Do kotlinc, scalac and groovyc read `@file`? | kotlinc reads `@argfile` with `javac`'s quoting. scalac, 2 and 3 alike, strips the quotes but keeps backslashes, so `"C:\\src"` arrives doubled. groovyc's `@file` is a list of source files, one per line, and never options. So jrs uses none of them: the `java` launcher reads the whole invocation from one argfile, and passes what follows the main class through unchanged (§5.3). |
| Groovy's `-J` and `-F` | `-J=name=value` reaches javac as `-name value`, and `-F=flag` as `-flag`: groovyc adds the `-`. `-J=-release=17` is therefore `--release 17`, and `-F=-add-exports=…` is `--add-exports=…`. Flags such as `Xlint:all`, `Werror`, `parameters` and `Akey=v` pass through `-F`, and classpath jars are visible to the joint javac. |
| Groovy's target level | groovyc has no flag for it: it reads `groovy.target.bytecode` from its own JVM. Without the property, Groovy 5 writes the level of the JDK it runs on (major 67 on JDK 23), not the project's. Given a level it does not know, Groovy 4 fails (`Bytecode version … is not supported by the compiler`), and Groovy 5 silently writes Java 11 bytecode (major 55). jrs cannot catch the second case. |
| groovyc without `-j` | It compiles `.java` files as Groovy source, so jrs passes `-j` whenever the unit has Java sources. |
| Spock, MUnit and the class-name filter | `--scan-class-path` with the launcher's default pattern found 0 tests in a Spock spec, and 0 in an MUnit suite. With `.*Spec` and `.*Suite` added, it found 1 in each (§7). |
| The Scala 3.8+ library | `org.scala-lang:scala-library:3.9.0` is the 9.8 MB standard library, and `scala3-library_3:3.9.0` is a 344-byte jar that depends on it. MUnit 1.3.6 brings `scala3-library_3:3.3.8`, which has real classes, and the 3.9 compiler fails against it ("Bad symbolic reference") unless the 3.9 shim wins mediation. So from 3.8 both are implied at depth 1. |
| KMP root POMs | They vary. `kotlinx-coroutines-core` and `kotlinx-serialization-json` are `pom`-packaged roots that depend on their `-jvm` artifacts, which jrs resolves as they are. `kotlinx-datetime`'s root does not point at `-jvm` (§11). |
| A release the compiler does not know | kotlinc 2.4 takes `-jvm-target` 1.8 and 9–26, and fails with `unknown JVM target version` past that. scalac fails with `is not a valid choice for` on both lines, and Groovy 4 as above. jrs adds its one-line hint under those phrases (§6.2). |
| Friend paths, output, colour | `-Xfriend-paths` with `-module-name <name>_test` gives tests the main module's `internal` declarations. kotlinc and scalac write only their own classes from a mixed source set, so `javac` after them is the only writer of Java classes. Scala 3 colours its diagnostics even into a pipe, hence `-color:never`. |
| Start-up | Wall clock for one small mixed unit: kotlinc 1.8 s, scalac 3.9 0.9 s, scalac 2.13 0.7 s, groovyc 0.5 s (§14.1). |

### 15.1 Where the implementation differs from the proposal

1. **One `java` argfile per compiler run, and no `Dialect`** (§5.3), for the
   reasons in the first row above.
2. **`.*Suite` joins `.*Spec`** in the non-Java class-name pattern (§7), for
   ScalaTest and MUnit.
3. **Scala 3.8+ implies two libraries** (§4.3): the shim and the real one.
4. **`jrs doc` builds first in a mixed project**, so that `javadoc` finds the
   other language's classes when the Java sources use them. It documents the
   Java sources and warns about the rest (§9). Scaladoc and Groovydoc are not
   built: Scala 3's scaladoc is its own artifact that reads TASTy, not
   sources, and neither is needed to build.
5. **No start-up bench**: the spike's measurements answer §14.1's question
   for now, and `benches/` keeps the one M5 harness.
6. **The phase line names the compilers** when a fresh resolution resolves
   them: `Resolving 3 declared dependencies and the Kotlin compiler`.
7. **The graph's own launcher parts leave the test JVM's classpath** (§7).
   `kotlin-test-junit5` 2.4 depends on `junit-platform-launcher` 1.10, which,
   ahead of a 1.13 console launcher on the classpath, failed every run with a
   `NoSuchMethodError`. The standalone launcher bundles its own launcher,
   console and reporting parts, so copies of those from the graph are left
   off; the engines and everything else are still the project's.
