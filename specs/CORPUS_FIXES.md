# jrs — Fixing the first corpus run's findings

The plan for the findings of the first run of the real-project corpus
([CORPUS.md](CORPUS.md), ROADMAP_ARCHIVE §1): 4 of 11 projects went from
`jrs migrate` to a running fat jar with no hand edits, and the other seven
stopped at `jrs build` on six causes, recorded in
[`specs/corpus/FINDINGS.md`](corpus/FINDINGS.md). Five of them are gaps in
`jrs migrate`, not in the build; one is a non-goal.

Status: **done**, 2026-10-14. F5–F10 are closed and F11's message is
changed; all six projects get past `build`, and what their later steps
turned up is in the ledger as findings 12–19. Where the implementation
differs from the plan below: the F5 end-to-end case is in the
`spring-boot-kotlin` fixture, the Kotlin one, not `spring-boot-kts`;
Gradle's `config_loc` defaults to `config/checkstyle`, as Gradle's does,
not to the configuration file's directory; property paths are relative to
the project root, where tasks run, not `{root}`, which `JAVA_TOOL_OPTIONS`
would split at a space; and OpenAPI's Maven task writes where the plugin
does, `{target}/generated-sources/openapi`, so that `build-helper`'s
`add-source` names the same tree.

Each fix below is a migration rule in `src/migrate/`,
with a fixture in `tests/fixtures/migrate/` and its `expected-jrs.toml`
(CORPUS.md §6, bin 2). None changes how jrs builds, resolves or writes
`jrs.lock`, so none needs a SPEC edit beyond DOCS.md's migration section.

---

## 1. Order

By projects unblocked per line of code, smallest first:

| # | Finding | Unblocks | Size |
| --- | --- | --- | --- |
| 1 | F8, the Ktor plugin's BOM | ktor-serialization, ktor-h2 | small: the Spring Boot plugin's rule, again |
| 2 | F5, a `val` inside `plugins { }` | petclinic-kotlin | small: one block kind in `gradle_vars.rs` |
| 3 | F9, a Kotlin template in `run.jvm-args` | ktor-h2's `run` | small: a report instead of a copy |
| 4 | F6 and F7, Checkstyle's own configuration | petclinic-maven, petclinic-gradle | medium: Maven executions, `config_loc`, the checker's dependencies |
| 5 | F10, OpenAPI Generator as a task | service-rest | large: a new translated plugin |
| 6 | F11, the version-range error message | nothing (bin 3) | trivial: one sentence |

F9 goes before F6 and F7 because the Ktor projects reach `run` as soon as
F8 is fixed, and `h2` would then fail there.

Getting past `build` is not the end: `test`, `run` and `package` have not
run on these seven yet, and the classpath and test comparisons (CORPUS.md
§4) have not been made on them. Each fix is therefore followed by a corpus
run of the projects it unblocks (§8), and whatever that run finds is a new
entry in the ledger, not part of this plan.

## 2. F8 — the Ktor Gradle plugin's BOM

**Seen:** ktor-serialization (`httpbin`) and ktor-h2, `build`. Every
`io.ktor` dependency is declared without a version
(`implementation("io.ktor:ktor-server-core")`); `id("io.ktor.plugin")
version "3.6.0"` supplies it. Migration drops each one ("has no version,
and nothing in the build manages one") and reports the plugin as "jrs has
no plugin system"; `kotlinc` then fails on `unresolved reference 'server'`.

**Change** (`src/migrate/gradle.rs`, beside the Spring Boot plugin's rule
that calls `spring_boot_bom`): when `io.ktor.plugin` is applied with a
version, add `[managed] "io.ktor:ktor-bom" = { version = "<plugin
version>", bom = true }` before the dependencies are read, so the
versionless `io.ktor` dependencies take their version from it and are
written as `{}`, as Boot's starters are. The plugin goes from "Not
migrated" to "Migrated" (the BOM) and "Needs review" (what else it does:
`buildFatJar`, Docker images — `jrs package --fat` builds the fat jar).
Without a version, the plugin is reported as Boot's is: no BOM, and the
dependencies stay dropped with that reason.

Before writing the rule, check against the plugin's source which BOM it
applies and to which configurations, and whether it applies it to the
test configurations too (the samples' `ktor-server-test-host` is
versionless as well).

**Fixture:** `tests/fixtures/migrate/ktor-gradle/`: a `build.gradle.kts`
with `kotlin("jvm")`, `id("io.ktor.plugin") version "…"`, two versionless
`implementation` and one versionless `testImplementation` Ktor
dependencies, and one with an explicit version, which keeps it.

**Done when** both Ktor rows get past `build` in a corpus run.

## 3. F5 — a `val` inside `plugins { }`

**Seen:** petclinic-kotlin, `build`. `plugins { val kotlinVersion =
"2.4.20"; kotlin("jvm") version kotlinVersion }`. `Variables::read_script`
(`src/migrate/gradle_vars.rs`) marks every assignment inside a block other
than `buildscript` or an `ext` block as guarded, so `kotlinVersion` reads
as "set inside a conditional, a loop or a block jrs does not run", no
`[kotlin]` table is written, and with it go `[kotlin] plugins = ["spring"]`,
`-Xjsr305=strict`, the Kotlin source root and so the `@SpringBootApplication`
class in `PetClinicApplication.kt` (`project.main-class`).

**Change:** treat a top-level `plugins` block as unguarded, as
`buildscript` is: Gradle runs it once, unconditionally, and a `val` in it
is visible to the rest of the block. Only the top-level block; a
`plugins { }` nested anywhere else stays guarded. A variable assigned in
`plugins { }` and again elsewhere is two assignments and stays unread, as
today.

**Tests:** a unit test in `gradle_vars.rs` beside the `guarded` cases (a
`val` in `plugins { }` resolves; one in a nested `plugins { }` does not),
and `kotlinVersion` added to the `spring-boot-kts` fixture's
`plugins { }`, so the main class half is covered end to end.

**Done when** petclinic-kotlin gets past `build`, with `project.main-class`
set by migration.

## 4. F9 — a Kotlin string template in `run.jvm-args`

**Seen:** ktor-h2, in the migrated `jrs.toml`; `run` not reached yet.
`applicationDefaultJvmArgs = listOf("-Dio.ktor.development=$isDevelopment")`
becomes `run.jvm-args = ["-Dio.ktor.development=$isDevelopment"]`, the
template copied as text.

**Change:** an argument read from `applicationDefaultJvmArgs` (and the
test task's `jvmArgs`, which share the reader) that holds a `$name` or
`${…}` is resolved through `Variables` when the variable is a literal, and
otherwise reported under "Not migrated" with the variable named, never
copied. For `isDevelopment` in particular (`project.ext.has("development")`,
false in a plain build) the argument is dropped; a Ktor project that wants
development mode sets it in `run.jvm-args` by hand.

**Fixture:** a line with a resolvable and one with an unresolvable
template, added to `gradle-kts`.

**Done when** ktor-h2's `run` is `ok` (once F8 is fixed).

## 5. F6 and F7 — Checkstyle with the project's own configuration

Both petclinics; one cause on each side, each needing two halves.

### 5.1 F6, Maven: the execution's configuration

**Seen:** petclinic-maven, `build`. `maven-checkstyle-plugin` is configured
inside an `<execution>` (`nohttp-checkstyle-validation`), not on the
plugin. `src/migrate/maven.rs` reads `configLocation` from the plugin's
`<configuration>` only, falls back to `/sun_checks.xml`, and that fails
petclinic with hundreds of errors in the post-compile hook.

**Change:**

1. Read the configuration from each `<execution>` too. One execution with
   a configuration and none on the plugin: use it. Several executions with
   different configurations: one task per execution
   (`checkstyle-<execution id>`), each hooked as today, or, if the task
   names get in the way, the first and a review line for the others.
2. Carry `<propertyExpansion>` (`key=value` lines). A task has no
   `jvm-args`, but Checkstyle's CLI expands `${…}` from the system
   properties, so the task gets `env = { JAVA_TOOL_OPTIONS = "-Dk=v …" }`.
   `${basedir}` and `${project.basedir}` become `{root}`.
3. The plugin's `<dependencies>` other than `checkstyle` itself
   (`io.spring.nohttp:nohttp-checkstyle` here) go into
   `[tasks.checkstyle.dependencies]`: they are where a configuration's
   custom checks come from.
4. `<sourceDirectories>` and `<includes>`/`<excludes>`: Checkstyle's CLI
   takes directories and an `-x` exclusion pattern, not Ant includes. A
   source directory list maps to the task's directories; `${basedir}` with
   `**/*` and excludes (nohttp's whole-tree scan) maps to `{root}` with one
   `-x` per exclude when they translate to a regex, and is reported
   otherwise rather than narrowed silently.

`checkers::checkstyle` (`src/checkers.rs`) grows the extra dependencies,
the environment and the exclusions as parameters, shared with §5.2.

**Fixture:** `maven-checkstyle-execution/`: a pom whose execution has
`configLocation`, `propertyExpansion`, a plugin dependency and
`sourceDirectories`, with a `src/checkstyle/` config the task names.

### 5.2 F7, Gradle: `config_loc` and the checker's dependencies

**Seen:** petclinic-gradle, `build`. `checkstyle { configDirectory =
project.file('src/checkstyle'); configFile = file(…) }` becomes the right
`-c`, but Gradle also sets the `config_loc` property to the configuration
directory, which the file uses: `Property ${config_loc} has not been set`.
The `checkstyle` configuration's other dependency
(`io.spring.javaformat:spring-javaformat-checkstyle`) is dropped, and the
`io.spring.nohttp` plugin is not reported at all.

**Change** (`src/migrate/gradle.rs`, the `applied("checkstyle")` branch):
set `config_loc` to `configDirectory` when given, else the config file's
directory, through the same environment as §5.1; add every dependency
declared on the `checkstyle` configuration to the task's dependencies;
read `checkstyle { … }`'s `configProperties` the way §5.1 reads
`propertyExpansion`. `io.spring.nohttp` is reported as a plugin with no
counterpart, with a note that its check is a Checkstyle configuration a
task can run.

**Fixture:** `gradle-checkstyle/`, with `configDirectory`, a
`configProperties` entry and a second `checkstyle` dependency.

**Done when** both petclinics get past `build` with the post-compile
Checkstyle task passing. petclinic is the first corpus project with a
Testcontainers suite, so its `test` row is the first real comparison of
test counts; it needs Docker running.

## 6. F10 — OpenAPI Generator as a task

**Seen:** service-rest, `build`. `openapi-generator-maven-plugin`
generates `org.springframework.samples.petclinic.rest.dto` and the API
interfaces from `src/main/resources/openapi.yml`
(`<generatorName>spring</generatorName>`); migration reports "jrs has no
plugin system", and `javac` fails on `package …rest.dto does not exist`.

**Change:** translate the plugin into a `pre-compile` task, as the
checkers are translated, in a function of its own in `src/checkers.rs` or
a new `src/migrate/` module if it outgrows it:

```toml
[tasks.openapi]
description = "Generate sources from src/main/resources/openapi.yml (OpenAPI Generator)"
main = "org.openapitools.codegen.OpenAPIGenerator"
args = ["generate", "-i", "src/main/resources/openapi.yml", "-g", "spring",
        "-o", "{target}/generated/openapi", "--additional-properties", "…"]
inputs = ["src/main/resources/openapi.yml"]
source-outputs = ["{target}/generated/openapi/src/main/java"]

[tasks.openapi.dependencies]
"org.openapitools:openapi-generator-cli" = "<the plugin's version>"

[hooks]
pre-compile = ["openapi"]
```

- `<inputSpec>`, `<generatorName>`, `<apiPackage>`, `<modelPackage>` and
  `<configOptions>` map to CLI flags (`--api-package`, `--model-package`,
  `--additional-properties k=v,…`); a key with no CLI flag is reported.
- One task per `<execution>`, named after its id.
- The generated tree's source root is where the generator writes for the
  chosen generator (`src/main/java` under `-o` for `spring`); the task
  writes only there, so `target/` stays disposable.
- `build-helper-maven-plugin`'s `add-source` naming that directory is then
  covered: reported as migrated through the task instead of "no plugin
  system".
- The generator's own dependencies the generated code needs
  (`jackson-databind-nullable`, `swagger-annotations`) are already in the
  project's `<dependencies>`; nothing is added to the project's graph, and
  the generator's graph stays a task graph (`resolve_tool_dependencies`).

Gradle's `org.openapi.generator` plugin is the same translation from
`openApiGenerate { … }`, done in the same change if a corpus project
needs it; none does yet.

**Fixture:** `maven-openapi/`, with a minimal spec file and one execution.

**Done when** service-rest gets past `build`. MapStruct's processor is
declared as a plain dependency there, and its `test` will show whether
processors on the classpath behave as Maven's do; that is a new finding if
not.

## 7. F11 — the version-range error message

Bin 3: SPEC §8.2 rejects ranges, and migration cannot pick a version
without resolving. microservices keeps `known = { build = 11 }`.

**Change** (`src/resolve/mod.rs`, the `is_range` error in `admissible`):
the message suggests a `[managed]` pin of the ranged artifact before the
exclusion it suggests today, since a managed version replaces the range
before it is looked at:

```
jrs resolves exact versions only; pin one in [managed]
("org.webjars.npm:vue__devtools-api" = "6.6.4"), or exclude the
dependency and declare an exact version
```

A unit test on the message; no corpus change.

## 8. After each fix

1. `cargo lint`, `cargo test`, as for any change.
2. `JRS_CORPUS=<the projects it unblocks> cargo bench --bench corpus`.
3. When the step is `ok`: remove its `known` mapping from
   `specs/corpus/corpus.toml`, mark the finding closed in `specs/corpus/FINDINGS.md`
   with the fixture or test that covers it, and record any new failure
   further along as a new finding with its own mapping.
4. Update the Compatibility table in `DOCS.md` and on
   `website/docs/index.html` from the run, and the DOCS.md migration
   section with the new rule.

## 9. Done

This plan is done when F5–F10 are closed, every one of the ten projects
other than microservices gets past `build`, and the findings their later
steps turn up are in the ledger. Whether they are then `ok` end to end is
CORPUS.md §9's measure, not this plan's.
