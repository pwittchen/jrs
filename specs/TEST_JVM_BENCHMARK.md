# jrs — Measuring the test JVM: `share-classes` by default, and the AOT cache

The plan for the two decisions [FASTER_BUILDS.md](FASTER_BUILDS.md) left to
measurement (§3.4 and §3.5, milestones F1 and F7), and the harness that
makes the measurement one command.

Status: **the harness and both modes are implemented; the measurement has not
been run on the corpus.** `test.share-classes = true` maps the dependency
jars from a dynamic class-data-sharing archive, and `test.share-classes =
"aot"` from JEP 483's AOT cache on JDK 24 and later (the dynamic archive on
an older JDK), both opt-in (SPEC §10.2). This document says how to decide
whether either becomes the default.

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

A mode whose outcome differs from `off`'s is marked. The shared cache is the
user's own, so the jars are not downloaded per mode.

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
