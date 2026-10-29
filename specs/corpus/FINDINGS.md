# Corpus findings

The ledger of [specs/CORPUS.md §6](../CORPUS.md#6-findings-and-triage):
one entry per cause, not per project. `corpus.toml` maps a step that fails
for a known cause to its entry (`known = { build = 6 }`), so the table
prints `F6` there instead of `FAIL`; when a run after the fix shows the cell
`ok`, the entry is closed and its mapping removed. An entry whose fix has
landed but that no run has confirmed yet is `fixed`, its mapping still in
place.

Bins: **1** a jrs bug (fixed with a regression test from a minimal
reproduction in `tests/`), **2** a migration gap (a rule in `src/migrate/`
and a fixture in `tests/fixtures/migrate/`), **3** a non-goal in the way
(cited in [ROADMAP_ARCHIVE §5](../ROADMAP_ARCHIVE.md#5-needs-a-spec-decision-first)).

| # | Seen in | Step | Bin | State |
| --- | --- | --- | --- | --- |
| 1 | petclinic (all three) | `test`, with `share-classes` | 1 | open |
| 2 | petclinic (all three) | `test`, with `share-classes = "aot"` | 1 | open |
| 3 | petclinic (all three) | `test`, with `share-classes` | 1 | open |
| 4 | petclinic (all three) | `test`, with `--manifest-path` | 1 | open |
| 5 | petclinic-kotlin | `build` | 2 | closed |
| 6 | petclinic-maven | `build` | 2 | closed |
| 7 | petclinic-gradle | `build` | 2 | closed |
| 8 | ktor-serialization, ktor-h2 | `build` | 2 | closed |
| 9 | ktor-h2 | `run` | 2 | closed |
| 10 | service-rest | `build` | 2 | closed |
| 11 | microservices | `build` | 3 | open |
| 12 | service-rest | `test`, `run`, `package` | 2 | fixed |
| 13 | petclinic (all three), service-rest | `build` (classpath) | 1 | open |
| 14 | ktor-serialization | `build` (classpath), `test` | 1 | open |
| 15 | ktor-serialization, ktor-h2 | `build` (classpath versions, which the harness accepts from Gradle) | 3 | open |
| 16 | petclinic-kotlin | `build` (classpath) | 3 | open |
| 17 | petclinic-kotlin | `build` (classpath) | 2 | open |
| 18 | petclinic (all three) | `build` (classpath) | 2 | open |
| 19 | ktor-h2 | `run`, `package` | – | open |

Findings 1–5 are those of the first test JVM measurement,
[TEST_JVM_BENCHMARK §7.1](../TEST_JVM_BENCHMARK.md#71-findings), in
its order; 1–3 only show with `test.share-classes` turned on, and 4 only
when `jrs` runs outside the project, so the corpus, which runs neither way,
does not see them. Findings 6–11 are from the first corpus run, and 12–19
from the run after [CORPUS_FIXES](../CORPUS_FIXES.md) closed 5–10,
which took those six projects past `build` for the first time. A step marked
`(classpath)` runs, but its runtime classpath differs from the reference's.
Bin `–` is a failure the project's own build has too.

---

## 1. A library that deserialises a project class fails under the shared layout

TEST_JVM_BENCHMARK §7.1, finding 1. `VetTests.serialization()`: Spring's
`SerializationUtils` (on `-cp`) resolves the class through the application
loader, which cannot see the class directories `share-classes` moves onto
the launcher's loader: `ClassNotFoundException: …petclinic.vet.Vet`.
Decides whether `share-classes` can become the default (SPEC §10.2).

## 2. The AOT cache cannot be assembled after Mockito self-attaches

TEST_JVM_BENCHMARK §7.1, finding 2. `-XX:AOTMode=create` fails with
`NoClassDefFoundError: …MockMethodDispatcher`; nothing remembers the failure,
so every later run records again. A failed assembly should fall back to the
dynamic archive for that key.

## 3. A forked run never writes an archive

TEST_JVM_BENCHMARK §7.1, finding 3. Forks read an archive but never dump
one, so a suite of 16 or more classes gets the shared layout's risk and none
of its gain. One fork could dump while the others read nothing.

## 4. The test JVM works in jrs's own directory

TEST_JVM_BENCHMARK §7.1, finding 4. With `--manifest-path` from elsewhere,
petclinic's `I18nPropertiesSyncTest` (`NoSuchFileException: src/main`) and
`PostgresIntegrationTests` (its compose file) fail; Maven and Gradle run
tests in the project directory. A SPEC §10.2 change, made before the fix.

## 5. A Kotlin plugin version from a `val` in `plugins { }`, and the Kotlin `@SpringBootApplication`

TEST_JVM_BENCHMARK §7.1, finding 5, seen again in the first corpus run.
`spring-petclinic-kotlin` declares `val kotlinVersion = "2.4.20"` inside
`plugins { }` and `kotlin("jvm") version kotlinVersion`. `jrs migrate`
says "`kotlinVersion` is set inside a conditional, a loop or a block jrs
does not run; no [kotlin] table was written", so `[kotlin] plugins`
(`spring`) and `-Xjsr305=strict` go too, and `jrs build` stops: "found 24
.kt files under src/main/kotlin, but jrs.toml has no [kotlin] table". With
no Kotlin source root, the `@SpringBootApplication` class in
`PetClinicApplication.kt` is not found either and `project.main-class` is
left unset; that half follows from the first once the table is written.

**Fix:** read a `val` declared at the top of the `plugins { }` block
(`gradle_vars.rs`), as `ext` and `gradle.properties` variables are read.

**Closed:** `Variables::read_script` reads the top-level `plugins { }` as
unguarded, and `read_plugin` no longer takes the `val` line for a plugin
(`a_val_in_the_top_level_plugins_block_is_read`, and `kotlinVersion` in the
`spring-boot-kotlin` fixture). petclinic-kotlin builds, with its
`@SpringBootApplication` class as `project.main-class`.

## 6. Maven Checkstyle: the execution's configuration is not read

First corpus run, petclinic-maven, `build`. The pom configures
`maven-checkstyle-plugin` inside an `<execution>` (`nohttp-checkstyle-validation`):
`<configLocation>src/checkstyle/nohttp-checkstyle.xml</configLocation>`, all
of `${basedir}` as the sources, `<propertyExpansion>config_loc=…</propertyExpansion>`,
and `io.spring.nohttp:nohttp-checkstyle` as a plugin dependency. Migration
reads none of it: the task runs `/sun_checks.xml` over `src/main/java` and
`src/test/java`, which fails petclinic with hundreds of `[ERROR]` lines, so
the post-compile hook fails the build.

**Fix:** read `<configuration>` from the plugin's executions as well as the
plugin itself; carry `<propertyExpansion>` into the task (a task has no
`jvm-args`, but Checkstyle expands properties from the system properties, so
`env = { JAVA_TOOL_OPTIONS = "-Dconfig_loc=…" }` does, as would a `-p`
properties file), and the plugin's `<dependencies>` into the task's
`[tasks.checkstyle.dependencies]`. A
`<sourceDirectories>` of the whole tree with `<includes>`/`<excludes>` may
not translate; then the task is reported rather than written.

**Closed:** each execution with a `<configuration>` is a task, read over the
plugin's; `<propertyExpansion>` goes into `JAVA_TOOL_OPTIONS`, the plugin's
other dependencies into the task's, `<sourceDirectories>` and Ant `<excludes>`
into directories and `-x` patterns anchored at `{root}`
(`tests/fixtures/migrate/maven-checkstyle-execution`,
`each_checkstyle_execution_with_a_configuration_is_a_task`). petclinic-maven's
post-compile Checkstyle task passes.

## 7. Gradle Checkstyle: `config_loc` and the checker's own dependencies

First corpus run, petclinic-gradle, `build`. `checkstyle { configDirectory
= …; configFile = file('src/checkstyle/nohttp-checkstyle.xml') }` becomes a
task with the right `-c`, but Gradle also sets `config_loc` to the
configuration directory, which the file uses: `CheckstyleException: Property
${config_loc} has not been set`. The `checkstyle` configuration's other
dependency (`io.spring.javaformat:spring-javaformat-checkstyle`) is dropped
as well, and the `io.spring.nohttp` plugin, which puts `nohttp-checkstyle`
on that classpath, is not reported.

**Fix:** set `config_loc` to `configDirectory` (the config file's directory
when none is set), the way finding 6 carries `<propertyExpansion>`, and add
every `checkstyle` dependency to `[tasks.checkstyle.dependencies]`.

**Closed:** `config_loc` is `configDirectory` (Gradle's default,
`config/checkstyle`, without one), `configProperties` joins it, every
`checkstyle` dependency joins the task's, and `io.spring.nohttp` is reported
with `nohttp-checkstyle` at its version on the task's classpath
(`tests/fixtures/migrate/gradle-checkstyle`). petclinic-gradle's
post-compile Checkstyle task passes.

## 8. The Ktor Gradle plugin's BOM

First corpus run, ktor-serialization and ktor-h2, `build`.
`id("io.ktor.plugin") version "3.6.0"` manages the version of every
`io.ktor` dependency, which the samples declare without one
(`implementation("io.ktor:ktor-server-core")`). Migration drops each as
"has no version, and nothing in the build manages one", and the Kotlin
compile fails on `unresolved reference 'server'`.

**Fix:** as `org.springframework.boot` becomes a `[managed]` BOM, the Ktor
plugin becomes `[managed] "io.ktor:ktor-bom" = { version = "<plugin
version>", bom = true }`, and the versionless `io.ktor` dependencies take
their version from it. A fixture in `tests/fixtures/migrate/` with a Ktor
plugin and two versionless dependencies.

**Closed:** `io.ktor.plugin` with a version becomes `[managed]
"io.ktor:ktor-bom"`, as the plugin adds it to `implementation`
(`tests/fixtures/migrate/ktor-gradle`,
`the_ktor_plugin_needs_a_version_to_bring_its_bom`). Both Ktor samples build.

## 9. A Kotlin string template in `run.jvm-args`

First corpus run, ktor-h2, migration output; `run` was not reached.
`applicationDefaultJvmArgs = listOf("-Dio.ktor.development=$isDevelopment")`
becomes `run.jvm-args = ["-Dio.ktor.development=$isDevelopment"]`, the
template left as text: the JVM gets the literal `$isDevelopment`.
`isDevelopment` is `project.ext.has("development")`, which jrs cannot run.

**Fix:** an argument holding a `$name` or `${…}` that migration cannot
resolve is reported under "Not migrated" instead of copied.

**Closed:** a `$name` or `${…}` in a JVM argument takes a literal variable's
value, and otherwise the argument is reported and left out (`gradle-kts`
fixture). ktor-h2's migrated `jrs.toml` has no template, and its development
mode is off, as in a plain Gradle build. Its `run` still fails, for the
sample's own reason: finding 19.

## 10. OpenAPI Generator's sources

First corpus run, service-rest, `build`. `openapi-generator-maven-plugin`
generates the `org.springframework.samples.petclinic.rest.dto` and API
interfaces from `src/main/resources/openapi.yml`; migration reports "jrs
has no plugin system", so `javac` fails on `package …rest.dto does not
exist`.

**Fix:** translate the plugin into a `pre-compile` task, as Checkstyle's is:
`org.openapitools:openapi-generator-cli` (its `main` is
`org.openapitools.codegen.OpenAPIGenerator`) with `generate -i <inputSpec>
-g <generatorName> -o {target}/generated/openapi` and the plugin's
`<configOptions>` as `--additional-properties`, its output a
`source-outputs` directory (SPEC §7.6). `build-helper-maven-plugin`, which
adds that directory as a source root, is then covered by the task.

**Closed:** each `openapi-generator-maven-plugin` execution becomes a
`pre-compile` task over the generator's CLI writing to
`{target}/generated-sources/openapi`, with `src/main/java` under it as its
`source-outputs`; `build-helper-maven-plugin`'s `add-source` of it is covered
(`src/migrate/maven_openapi.rs`, `tests/fixtures/migrate/maven-openapi`).
service-rest builds; its tests then fail on finding 12.

## 11. A version range in a transitive POM

First corpus run, microservices, `build`. `org.webjars.npm:vue-router:4.5.1`
(in `spring-petclinic-api-gateway`) depends on
`org.webjars.npm:vue__devtools-api` with `[6.6.4,7.0.0-0)`. jrs rejects
version ranges (SPEC §8.2), so resolution stops. A `[managed]` pin of
`vue__devtools-api` settles it without the exclusion the error suggests,
since a managed version replaces what the POM asks for before the range is
looked at; but migration does not resolve, so it cannot know which version
to pin, and the corpus allows no hand edit.

Bin 3, cited beside "Version ranges" in ROADMAP_ARCHIVE §5. The error
now suggests the `[managed]` pin, with the range's lower bound, before the
exclusion (`a_transitive_range_is_rejected_too`).

## 12. Maven: the compiler plugin's execution configuration is not read

Second corpus run, service-rest, `test`, `run` and `package`. The pom
configures `maven-compiler-plugin` inside its `default-compile` execution:
`<annotationProcessorPaths>` with `mapstruct-processor`, and `<compilerArgs>`
with `-Amapstruct.defaultComponentModel=spring`. Migration reads the plugin's
own `<configuration>` only, as it did Checkstyle's (finding 6), so the
mappers are not Spring beans (and on JDK 23 and later, with the processor a
plain dependency and no `-proc:full`, not generated at all): the context
fails on `No qualifying bean of type '…mapper.OwnerMapper'`, and 232 of 237
tests with it.

**Fix:** read `maven-compiler-plugin`'s `default-compile` (and
`default-testCompile`) execution configuration over the plugin's, as
`read_checkstyle` does.

**Fixed:** `compiler_configs` reads each execution running the goal, then the
plugin's own configuration, key by key; `<release>`, `<compilerArgs>` and
`<annotationProcessorPaths>` come from there. A `testCompile` execution's
processors become dev-dependencies and its `<compilerArgs>` are reported
(`the_compiler_executions_configuration_is_read_over_the_plugins`).

## 13. A BOM's exclusions on a managed dependency are not applied

Second corpus run, petclinic (all three) and service-rest, `build`
(classpath): jrs puts `com.google.protobuf:protobuf-java` on the runtime
classpath, the reference does not. Spring Boot's BOM manages
`com.mysql:mysql-connector-j` with an `<exclusion>` of `protobuf-java`;
Maven and Gradle's dependency-management plugin apply a managed entry's
exclusions wherever the dependency is reached, jrs's `[managed]` import
takes only the version.

**Fix:** carry a BOM entry's `<exclusions>` with its version in
`resolve::managed_versions`, and apply them where `admissible` gives the
managed version (SPEC §8.9).

## 14. A profile a dependency's POM activates by OS

Second corpus run, ktor-serialization, `build` (classpath) and `test`.
`com.aayushatharva.brotli4j:brotli4j` adds its native jar
(`native-osx-aarch64` here) through `<profiles>` activated by `<os>` family
and arch. Maven and Gradle activate them on the host, jrs does not read a
dependency's profiles, so `ResponseFormatTest.brotli` fails on
`UnsatisfiedLinkError: Failed to load Brotli native library`. Activating them
makes the resolved graph depend on the host, which `jrs.lock` is written not
to; `{os-classifier}` is the precedent for doing it explicitly.

## 15. Highest-wins mediation

Second corpus run, ktor-serialization and ktor-h2, `build` (classpath):
`org.jetbrains:annotations` 13.0 against the reference's 23.0.0,
`org.slf4j:slf4j-api` 2.0.17 against 2.0.19, and in ktor-serialization
`kotlinx-io-core-jvm` and `kotlinx-io-bytestring-jvm` 0.6.0 against 0.9.1.
Gradle takes the highest version in the graph, jrs the nearest (SPEC §13.6).
The harness expects this of a Gradle build and does not fail `build` on
versions alone, so no step maps here; ktor-serialization's `build` fails on
finding 14.

Bin 3, ROADMAP_ARCHIVE §5, "Highest-wins mediation (opt-in)", whose
suggestion — the migration report flagging where the two disagree — would
have named these.

## 16. Gradle Module Metadata's runtime variant

Second corpus run, petclinic-kotlin, `build` (classpath).
`org.apiguardian:apiguardian-api` is on jrs's runtime classpath and not the
reference's: JUnit's POMs declare it at compile scope, its `.module` files
in `apiElements` only, and Gradle reads the `.module`. petclinic-kotlin
declares its test starters on `implementation`, so JUnit is on the main
runtime classpath at all. `org.junit.platform:junit-platform-launcher` is on
the reference's and not jrs's; no POM or `.module` on that classpath was
found to declare it at runtime, so its cause is not settled yet.

Bin 3, ROADMAP_ARCHIVE §5, "Gradle Module Metadata beyond the JVM variant".

## 17. A versionless `org.jetbrains.kotlin` library takes the Kotlin plugin's version

Second corpus run, petclinic-kotlin, `build` (classpath): `kotlin-reflect`
2.3.21 against the reference's 2.4.20. The build declares
`implementation("org.jetbrains.kotlin:kotlin-reflect")` without a version;
the Kotlin Gradle plugin gives its own Kotlin libraries its version, while
the migrated `{}` takes Spring Boot's BOM's.

**Fix:** migration writes a versionless `org.jetbrains.kotlin:kotlin-*`
dependency at `[kotlin]`'s version, as it does `kotlin("reflect")`.

## 18. `spring-boot-devtools`

Second corpus run, petclinic (all three), `build` (classpath): the
reference's runtime classpath has `spring-boot-devtools` (Maven's
`<optional>true</optional>`, Gradle's `developmentOnly`), and migration
reports it, since jrs.toml has no scope for a dependency `jrs run` alone
sees. Boot leaves it out of the packaged jar, so only `run` differs. Either
such a scope (a SPEC decision) or a review line saying devtools is left out.

## 19. ktor-h2 names a module function that does not exist

Second corpus run, ktor-h2, `run` and `package`. `application.conf` names
`io.ktor.samples.ApplicationKt.module`, but the function is in
`H2Application.kt`, so it compiles into `H2ApplicationKt`:
`ReloadingException: Module function cannot be found`. The sample's own
`./gradlew run` fails the same way; nothing for jrs to fix. Closed when the
sample is fixed and the pin moved, or `run` and `package` dropped from its
row.
