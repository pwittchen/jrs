# Corpus findings

The ledger of [specs/todo/CORPUS.md §6](../specs/todo/CORPUS.md#6-findings-and-triage):
one entry per cause, not per project. `corpus.toml` maps a step that fails
for a known cause to its entry (`known = { build = 6 }`), so the table
prints `F6` there instead of `FAIL`; when a run after the fix shows the cell
`ok`, the entry is closed and its mapping removed.

Bins: **1** a jrs bug (fixed with a regression test from a minimal
reproduction in `tests/`), **2** a migration gap (a rule in `src/migrate/`
and a fixture in `tests/fixtures/migrate/`), **3** a non-goal in the way
(cited in [ROADMAP_ARCHIVE §5](../specs/ROADMAP_ARCHIVE.md#5-needs-a-spec-decision-first)).

| # | Seen in | Step | Bin | State |
| --- | --- | --- | --- | --- |
| 1 | petclinic (all three) | `test`, with `share-classes` | 1 | open |
| 2 | petclinic (all three) | `test`, with `share-classes = "aot"` | 1 | open |
| 3 | petclinic (all three) | `test`, with `share-classes` | 1 | open |
| 4 | petclinic (all three) | `test`, with `--manifest-path` | 1 | open |
| 5 | petclinic-kotlin | `build` | 2 | open |
| 6 | petclinic-maven | `build` | 2 | open |
| 7 | petclinic-gradle | `build` | 2 | open |
| 8 | ktor-serialization, ktor-h2 | `build` | 2 | open |
| 9 | ktor-h2 | `run` (not reached yet) | 2 | open |
| 10 | service-rest | `build` | 2 | open |
| 11 | microservices | `build` | 3 | open |

Findings 1–5 are those of the first test JVM measurement,
[TEST_JVM_BENCHMARK §7.1](../specs/TEST_JVM_BENCHMARK.md#71-findings), in
its order; 1–3 only show with `test.share-classes` turned on, and 4 only
when `jrs` runs outside the project, so the corpus, which runs neither way,
does not see them. Findings 6–11 are from the first corpus run.

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

## 9. A Kotlin string template in `run.jvm-args`

First corpus run, ktor-h2, migration output; `run` was not reached.
`applicationDefaultJvmArgs = listOf("-Dio.ktor.development=$isDevelopment")`
becomes `run.jvm-args = ["-Dio.ktor.development=$isDevelopment"]`, the
template left as text: the JVM gets the literal `$isDevelopment`.
`isDevelopment` is `project.ext.has("development")`, which jrs cannot run.

**Fix:** an argument holding a `$name` or `${…}` that migration cannot
resolve is reported under "Not migrated" instead of copied.

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

## 11. A version range in a transitive POM

First corpus run, microservices, `build`. `org.webjars.npm:vue-router:4.5.1`
(in `spring-petclinic-api-gateway`) depends on
`org.webjars.npm:vue__devtools-api` with `[6.6.4,7.0.0-0)`. jrs rejects
version ranges (SPEC §8.2), so resolution stops. A `[managed]` pin of
`vue__devtools-api` settles it without the exclusion the error suggests,
since a managed version replaces what the POM asks for before the range is
looked at; but migration does not resolve, so it cannot know which version
to pin, and the corpus allows no hand edit.

Bin 3, cited beside "Version ranges" in ROADMAP_ARCHIVE §5. A smaller,
bin-1 improvement: the error could suggest the `[managed]` pin.
