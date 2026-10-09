# jrs — Measuring the test JVM: `share-classes` by default, and the AOT cache

The plan for the two decisions [FASTER_BUILDS.md](FASTER_BUILDS.md) left to
measurement (§3.4 and §3.5, milestones F1 and F7), and the harness that
makes the measurement one command.

Status: **the harness and both modes are implemented, and a first pass of
the measurement is in (§7): both modes stay opt-in.** `test.share-classes =
true` maps the dependency jars from a dynamic class-data-sharing archive,
and `test.share-classes = "aot"` from JEP 483's AOT cache on JDK 24 and
later (the dynamic archive on an older JDK), both opt-in (SPEC §10.2). This
document says how to decide whether either becomes the default, and §7 what
the first pass decided and which jrs findings stand in the way.

---

## 1. The two questions

1. **May `share-classes` be on by default?** The layout it needs puts the
   class directories on the launcher's own class loader, which is
   parent-first: a jar's copy of a name wins over the project's. jrs keeps
   the usual layout when a class or resource name is in both, but it cannot
   see order-sensitive registries — `META-INF/services/*`,
   `spring.factories`, `META-INF/spring/*.imports` — where the project's
   entries now come after the jars'. Nor can it see a test that reads
   `java.class.path` instead of the context class loader. Only running real
   suites both ways answers it.
2. **Is the AOT cache worth what it costs over the dynamic archive?** It keeps
   classes linked and, from JDK 25, method profiles, which should matter most
   for a Spring context that loads thousands of classes. It costs a JVM run
   of its own to assemble, once per change of the jars, and an older JDK
   cannot use it at all.

## 2. The harness

`benches/test_jvm.rs` takes one project and changes nothing in it:

```text
JRS_BENCH_PROJECT=<path> [JRS_BENCH_RUNS=5] [JRS_BENCH_FORKS=1] cargo bench --bench test_jvm
```

It copies the project (without `target/` and `.git/`) to a scratch
directory and, for each mode — `off`, `archive` (`true`), `aot` — sets
`[test] share-classes`, runs `jrs test --all --no-build-cache --forks N`
once untimed, then `JRS_BENCH_RUNS` times, and prints per mode:

| Column | What it is |
| --- | --- |
| first | the untimed run: compile, plus the archive dumped or the AOT cache recorded and assembled |
| wall | median wall time of `jrs test` |
| test JVM | median of the `test JVM` row of `--timings` |
| outcome | the exit code and the `Finished` line's counts |
| archive | what `-v` said: mapping which archive, or why the usual layout was kept |

A mode whose outcome differs from `off`'s is marked, and an outcome the runs
of one mode did not agree on says how many of them did — a flaky test shows
there before step 3 of §4 has to rule it out. When any run fails, every
run's output is kept and the directory printed, so the test tree names the
test that differs. `jrs` runs inside the copy, as a user runs it: the test
JVM works in jrs's own directory (there is no `test.cwd`), and a suite that
reads `src/main` or a compose file by a relative path fails from anywhere
else. The shared cache is the user's own, so the jars are not downloaded per
mode; the archives the copy wrote there, keyed by its path, are deleted at
the end.

A first run on a one-test JUnit 5.13.4 project, JDK 25, macOS, release build:

| mode | wall | test JVM |
| --- | --- | --- |
| off | 336 ms | 276 ms |
| archive | 265 ms | 204 ms |
| aot | 218 ms | 158 ms |

It shows the harness works against the real console launcher; a one-test
project says nothing about a Spring Boot suite.

## 3. The corpus

The projects of [ROADMAP §1](../ROADMAP.md#1-hardening-against-real-projects)
that already pass `jrs test` with the usual layout — the measurement means
nothing on a project that does not. At least:

- **Spring Boot on Maven and on Gradle**, `spring-petclinic` first: one
  `@SpringBootTest` context, many slices, `META-INF/spring/*.imports` on the
  classpath — the case §3.5 of FASTER_BUILDS expects the largest gain from,
  and the registry-order risk lives in.
- **Spring Boot in Kotlin**, for Kotlin's own runtime jars on the classpath.
- **A Ktor service** with `ktor-server-test-host`: a different framework,
  `META-INF/services` heavy.
- **A mid-sized service** with Testcontainers, Mockito (and its agent from
  `test.java-agents`, which turns sharing off — it should show as such) and a
  database driver loaded through `ServiceLoader`.
- **A plain library** with JUnit alone, as the floor.

Each project is pinned to the commit the corpus pins, and run on the JDKs
jrs supports that CI installs: 17 and 21 (archive only; `aot` falls back) and
25 (both).

## 4. Procedure

For every project × JDK:

1. Run the harness with `JRS_BENCH_RUNS=7` and `JRS_BENCH_FORKS=1`, then once
   more with `JRS_BENCH_FORKS` set to the fork count `jrs test` picks for the
   project by itself, since forks only read an archive and their run is what
   a user waits for.
2. Record the table, the JDK build, the machine and the number of test
   classes.
3. For every marked outcome: rerun once to rule out a flaky test, then find
   the test that differs and its cause — a registry order, a shadowed
   resource, a `java.class.path` reader — and file it as a jrs finding with a
   minimal reproduction, as ROADMAP §1 asks for any failure.
4. For every "not shared" line: record the name that overlapped. A pattern
   (Logback's `logback-test.xml` in a test jar, say) may be one jrs should
   treat as harmless, which is a SPEC change of its own.

The results go into a section of the ROADMAP §2 benchmark report.

## 5. Decisions

**`share-classes` on by default** (archive mode, every JDK) when all of:

- no corpus project's outcome differs from `off` on any JDK, after flaky
  tests are ruled out;
- the overlap check keeps the usual layout on no more than one corpus project
  in five, or a pattern from step 4 explains each one;
- the median `test JVM` time improves by at least 10% on the Spring Boot
  projects, and is not slower anywhere by more than the run-to-run noise;
- the first run's extra cost (`first` against `off`'s) is recovered within
  three later runs.

If outcomes differ only through registry order, the fallback is a narrower
default: on, with `META-INF/services` and Spring's registries checked by name
too, so a project shipping its own entries keeps the usual layout. That is a
change to SPEC §10.2's overlap rule, decided with the data.

**`"aot"` as what `true` means on JDK 24 and later** — keeping `"aot"` as an
explicit value — when, on JDK 25:

- its `test JVM` median beats the archive's by at least 15% on the Spring
  Boot projects;
- its first run (recording plus assembly) is recovered within five later
  runs;
- outcomes match `off` exactly, as for the archive.

Otherwise it stays opt-in, and FASTER_BUILDS §3.5 records the numbers that
kept it there.

## 6. What it does not measure

- **Coverage and agent runs**, which never share (SPEC §10.2) until FASTER_BUILDS
  open question 1 is answered per JDK.
- **Cached test runs** (SPEC §7.8): every harness run is `--no-build-cache`,
  since a cache hit starts no JVM at all.
- **Cold disk caches.** The archive is a file the OS caches like any jar;
  the first run after a reboot is not separated out.

## 7. Results, first pass

Run on 2026-10-09 with jrs 0.8.0 (`0fa0b14` and this harness), on an Apple
M4 Max (14 cores) under macOS 27.0.1, with Temurin 25.0.2+10 and Corretto
23.0.2+7 — JDK 17 and 21 were not installed, so 23 stands in for them:
`"aot"` falls back to the archive there, as on 17 and 21. Seven runs per
mode, medians; Docker was up, so the Testcontainers tests ran.

| Project | Kind | Commit | Test classes |
| --- | --- | --- | --- |
| `spring-petclinic`, from `pom.xml` | Spring Boot 4.1, Maven, Testcontainers | `500158f` | 19 |
| the same, from `build.gradle` | Spring Boot 4.1, Gradle | `500158f` | 19 |
| `spring-petclinic-kotlin`, from `build.gradle.kts` | Spring Boot 4.1 in Kotlin | `c77f77b` | 14 |
| `examples/bookmarks` | Spring Boot, Mockito from `test.java-agents` | this repo | 2 |
| `examples/orders` | Ktor, stand-in for a Ktor sample | this repo | 2 |
| `examples/wordstats` | a plain library, JUnit alone | this repo | 3 |

Every project passes `jrs test` with the usual layout once `jrs` runs in
it. The Kotlin petclinic needed a `[kotlin]` table written by hand (finding
5). There is no mid-sized service in this pass. Test counts were not
compared against Maven's or Gradle's.

`test JVM` medians, one JVM (`JRS_BENCH_FORKS=1`), against `off`:

| Project | JDK | off | archive | aot | outcome with sharing |
| --- | --- | --- | --- | --- | --- |
| petclinic (Maven) | 25 | 15.98 s | 15.30 s (−4.2%) | 21.83 s (+37%) | 80 of 81 |
| petclinic (Gradle) | 25 | 16.40 s | 15.24 s (−7.1%) | 21.70 s (+32%) | 80 of 81 |
| petclinic-kotlin | 25 | 3.79 s | 3.44 s (−9.2%) | 8.86 s (+134%) | 40 of 41 |
| bookmarks | 25 | 1020 ms | 1010 ms | 1022 ms | usual layout (agent) |
| orders | 25 | 340 ms | 236 ms (−31%) | 195 ms (−43%) | same |
| wordstats | 25 | 305 ms | 221 ms (−28%) | 176 ms (−42%) | same |
| petclinic (Maven) | 23 | 16.64 s | 15.34 s (−7.8%) | = archive | 80 of 81 |
| petclinic (Gradle) | 23 | 16.66 s | 15.42 s (−7.4%) | = archive | 80 of 81 |
| petclinic-kotlin | 23 | 3.99 s | 3.47 s (−13%) | = archive | 40 of 41 |
| bookmarks | 23 | 1053 ms | 1052 ms | = archive | usual layout (agent) |
| orders | 23 | 346 ms | 263 ms (−24%) | = archive | same |
| wordstats | 23 | 316 ms | 242 ms (−23%) | = archive | same |

With the fork count `jrs test` picks for petclinic by itself (2), no mode
wrote an archive at all (finding 3): `off` 14.15 s, `archive` 14.25 s, `aot`
15.00 s on JDK 25, and 15.91 s, 14.86 s, 15.94 s on JDK 23 — which also
puts the run-to-run noise on this suite at about 7%. Both share modes still
failed the same test there, since the layout moves whether or not an
archive is mapped.

The first run's extra cost, recovered by the wall-time saving of later
runs: 6.3 runs on petclinic (Maven), 3.0 on Gradle, 7.3 on Kotlin (JDK 25);
3.3–3.7 on JDK 23; at once on `orders` and `wordstats`. On JDK 23 the
matrix's `aot` first runs found the archive `archive` had written, the
same file; the harness now deletes the copy's archives before each mode.

### 7.1 Findings

1. **A library that deserialises a project class fails under the shared
   layout.** `VetTests.serialization()` in all three petclinics: Spring's
   `SerializationUtils` (in `spring-core`, on `-cp`) reads the object back,
   and `ObjectInputStream.resolveClass` looks the class up through the
   nearest user-defined loader on the stack — the application loader that
   loaded `spring-core` — which cannot see the class directories now on the
   launcher's loader: `ClassNotFoundException: …petclinic.vet.Vet`. This is
   neither registry order nor `java.class.path` (§1): any jar that loads a
   project class by name through its own loader fails the same way, and no
   overlap check can see it. A minimal reproduction is a test that
   round-trips a project class through a jar's `ObjectInputStream`
   subclass. A reading run could append the class directories to `-cp`
   after the jars — CDS allows a run to append to the classpath it was
   dumped with — so that the application loader finds them, at the cost of
   a dumping run that still cannot.
2. **The AOT cache cannot be assembled after Mockito self-attaches.**
   Mockito 5's inline mock maker (the default, pulled in by
   `spring-boot-starter-test`) attaches its agent at run time and appends
   its dispatcher to the boot class path. The recording run records that,
   and `-XX:AOTMode=create` fails with `NoClassDefFoundError:
   org/mockito/internal/creation/bytebuddy/inject/MockMethodDispatcher`.
   `Share::finish` throws the failure away, as it should, but nothing
   remembers it, so every later run records again: that is the +32–134%
   above. The same append is likely why the archive's gain on the Spring
   projects is a fifth of the plain ones' — the JVM warns "Sharing is only
   supported for boot loader classes because bootstrap classpath has been
   appended". A failed assembly should fall back to the dynamic archive
   for that key instead of recording on every run.
3. **A forked run never writes an archive.** Forks read one but never dump
   it (FASTER_BUILDS §8, question 2), and the single JVM that would dump it
   only runs when the suite is small or the run is a `--method`,
   `--rerun-failed` or `--fail-fast` run. A suite of 16 or more classes
   therefore gets the shared layout's risk from `jrs test` and none of its
   gain. One fork (the first) could dump while the others read nothing.
4. **The test JVM works in jrs's own directory.** Run with
   `--manifest-path` from elsewhere, petclinic's `I18nPropertiesSyncTest`
   (`NoSuchFileException: src/main`) and `PostgresIntegrationTests` (its
   compose file) fail, where Maven and Gradle run tests in the project
   directory. There is no `test.cwd` (SPEC §11.3 says as much of `workingDir`).
   Running the test JVM in the project root is a SPEC §10.2 change.
5. **Two migration gaps in the Kotlin petclinic.** A Kotlin plugin whose
   version is a `val` inside `plugins { }` (`version kotlinVersion`) writes
   no `[kotlin]` table, so `[kotlin] plugins` and `-Xjsr305=strict` go too,
   and the `@SpringBootApplication` class is only looked for in `.java`
   sources, so `project.main-class` is left unset.

No project kept the usual layout through an overlapping name; `bookmarks`
kept it through `test.java-agents`, as SPEC §10.2 says.

### 7.2 Decisions

**`share-classes` stays off by default.** The first rule of §5 fails on
three projects of six, on both JDKs, and the narrower default of §5 does not
help: the cause is finding 1, not registry order. The Spring Boot gain is
4–13%, short of 10% on four of the six runs. Before a second pass is worth
running, finding 1 needs an answer and finding 3 a fix, since the default
fork count would otherwise leave every large suite without an archive.

**`"aot"` stays opt-in.** On JDK 25 it beats the archive by 17–20% where it
can be assembled (`orders`, `wordstats`) and pays its first run back
within four runs, but on the Spring Boot projects it never is (finding 2),
which fails the first rule of §5 outright.

