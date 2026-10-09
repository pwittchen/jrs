# jrs — Faster builds: the rest of "Faster than Gradle"

Design proposal for the four items still open in
[ROADMAP §6](../ROADMAP.md#6-faster-than-gradle): class-data sharing for the
test JVM, pipelined downloads, a build cache, and a warm `javac` for
`--watch`.

Status: **F1–F7 implemented** (§9.1), F7 opt-in as `test.share-classes =
"aot"`, in two steps (record, then assemble) rather than JDK 25's one-step
flag, which prints into the test output. Which mode becomes the default is
decided by measurement: [TEST_JVM_BENCHMARK.md](TEST_JVM_BENCHMARK.md),
whose first pass keeps both opt-in. Open
questions 5 and 7 have proposals of their own:
[TASK_OUTPUT_CACHE.md](TASK_OUTPUT_CACHE.md) and
[TEST_RESULT_CACHE.md](TEST_RESULT_CACHE.md). The SPEC.md edits of §7 are in:
SPEC §1.2 draws the line between a compiler daemon and a `--watch` worker,
§7.5 has the worker, §7.8 the build cache, §8.3–§8.6 the two download waves,
`[build-cache]` and pruning, and §10.2 the test JVM's archive. Those sections
are the contract now; this document stays the design record. Where the code
settled a question this proposal left open, the SPEC section says how: the
second download wave starts once the first is in rather than beside it
(SPEC §8.3), forks read an archive but never dump one (§8, question 2), and
units with annotation processors are cached, trusting their declared inputs
(§8, question 4).

---

## 1. Problem

The first half of ROADMAP §6 has landed (SPEC §7.2, §10.2):

- each compiler JVM maps its classes from a class-data-sharing archive in
  `<cache>/cds/`, and a file-by-file `javac` starts with C1 and the serial
  collector;
- `jrs test` runs only the test classes a change reaches
  (`compile/impact.rs`);
- an unset `test.forks` splits a large suite among JVMs, balanced by each
  class's last time.

What Gradle still has over jrs comes down to four things:

1. **A test JVM that starts cold.** A Spring Boot test suite loads thousands
   of classes into one context before its first test runs. The compilers'
   archives do not reach the test JVM: CDS refuses to dump a classpath
   holding a non-empty class directory, and the test classpath starts with
   two (`target/test-classes`, `target/classes`).
2. **Downloads that serialise with compilation.** On a cold cache,
   `Session::dependencies()` downloads every jar of the graph, test-only ones
   included, before `javac` starts. Gradle resolves configurations lazily, so
   its compile task waits only for `compileClasspath`.
3. **No memory of outputs beyond `target/`.** The fingerprints say when the
   current `target/` is up to date, but a branch switch, a `git stash` or a
   `jrs clean` throws the work away. Gradle's build cache restores task
   outputs by their inputs' hash, locally and from a remote cache in CI.
4. **A compiler JVM per build.** Every rebuild under `--watch` starts `javac`
   in a fresh JVM with a cold JIT, while the Gradle daemon keeps `javac`
   in-process and hot across builds.

Every part below is judged by the benchmarks of
[ROADMAP §2](../ROADMAP.md#2-benchmarks-against-maven-and-gradle). A part
whose benchmark does not show a gain worth its code is not merged.

## 2. Principles

These hold for every part, and come from the parts of SPEC that are
load-bearing for correctness:

- **Errs towards doing the work.** Compile avoidance and test selection run
  the whole unit or the whole suite when they cannot vouch for a shortcut.
  So does everything here: a cache entry, an archive or a worker that might
  be wrong is not used. A spurious compile costs seconds, a wrong class
  costs a wrong result.
- **Never a failure of its own.** An archive that cannot be written, a cache
  that cannot be read, a worker that dies: each costs the speed-up and
  nothing else, and the build goes on as it would have without it.
- **Same bytes, same transcript.** A restored `target/classes` is byte for
  byte what a compile writes. `--progress never` prints the same phase lines
  whichever path a build took, except where a line says that work was
  skipped and why, as `Fresh` does today.
- **`target/` stays disposable**, and nothing a user needs lives in the
  shared cache either: `jrs cache prune` may delete any of it.
- **No new crate.** `zip`, `sha2`, `ureq` and `rayon` cover all four parts.
- **No process outlives the command.** jrs starts no background process that
  survives it (§6.2).

---

## 3. Class-data sharing for the test JVM

### 3.1 What CDS allows

Measured on JDK 25 with `junit-platform-console-standalone` 1.14.4:

- A dump (`-XX:ArchiveClassesAtExit`) with a non-empty directory anywhere on
  `-cp` fails: `Cannot have non-empty directory in paths`.
- An archive dumped with `-cp a.jar:b.jar` is used by a run with
  `-cp a.jar:b.jar:dir`: the runtime classpath may **append** to the
  dump-time one.
- The same archive is rejected by a run with `-cp dir:a.jar:b.jar`: `The
  name of app classpath [1] does not match`. Every entry up to the dump-time
  length must be the same.
- Classes loaded through a custom class loader (the console launcher's own
  `--class-path` loader) are archived too, and a class whose bytes changed
  since the dump is loaded from its file, not from the archive: the JVM
  compares an unregistered loader's class with the archived one before using
  it. A stale archive cannot serve old project code.
- The launcher run over one trivial test went from 0.24 s to 0.18 s, with
  2,617 classes mapped from the archive and 180 loaded otherwise.

So the archive can cover the dependency jars, provided the project's class
directories never come first on the JVM's classpath.

### 3.2 The layout

Today (`TestRun::args`):

```
java [agents] [jvm-args] -cp target/test-classes:target/classes:<test jars>:<launcher> \
     ConsoleLauncher execute --scan-class-path target/test-classes …
```

With class-data sharing:

```
java [agents] [jvm-args] -XX:SharedArchiveFile=… -cp <test jars>:<launcher> \
     ConsoleLauncher execute --class-path target/test-classes:target/classes \
     --scan-class-path target/test-classes …
```

The class directories go to the launcher's `--class-path`, which it puts in
a `URLClassLoader` whose parent is the application loader, and makes the
thread's context class loader for the run. The JVM's own `-cp` holds jars
only, so it can be dumped and matched.

That changes one thing a test can observe: **delegation is parent-first.** A
class or resource in a class directory that a jar also has is now found in
the jar, and `getResources` lists the jars' copies before the project's.
Three cases matter:

- a project class shadowing a dependency's class of the same name (a patched
  copy) — the jar's would win;
- a resource of the same name in both (`logback.xml`, `application.yml`
  inside a library) — the jar's would win;
- order-sensitive registries (`META-INF/services/*`, `spring.factories`) —
  the project's entries would come after the jars', so a
  `ServiceLoader.findFirst()` could pick another provider.

jrs checks the first two before it uses the layout: it lists every entry of
the class directories and every jar's central directory, and any path in
both (outside `META-INF/`) keeps today's layout for that run, with a
`-v` line naming the path. The jars' entry lists are cached beside the
archive, keyed by each jar's size and mtime, so the check costs a walk of
`target/` and nothing per jar after the first run. The third case cannot be
detected from names, which is why the layout is opt-in (§3.4).

Other things that read `java.class.path` instead of the context loader (a
hand-rolled classpath scanner) would no longer see the class directories.
Spring, JUnit, ClassGraph and Testcontainers use the context loader.

### 3.3 The archive

- **Where:** `<cache>/cds/test-<project>-<deps>.jsa`, where `<project>` is a
  hash of the manifest's path and `<deps>` a hash of the `-cp` entries (path,
  size, mtime), the JDK (home, module image) and the JVM flags that change
  what is archived (`-javaagent`, `-XX:` flags). One archive per project:
  writing a new one deletes the project's older ones, so a project whose
  dependencies change daily does not fill the cache with 100 MB archives.
- **Life cycle:** as for the compilers (`compile/share.rs`). A run without an
  archive dumps into a temporary file that jrs renames into place after the
  run, whether the tests passed or not; a test failure is not an archive
  failure. A run whose JVM failed to start while dumping runs again without
  the flags. `-Xlog:cds*=off` keeps the JVM quiet about a mismatch.
- **What is not shared:** a `--debug` run (JDWP, and a debugger attaching
  while classes load), a `--coverage` run and any run with `test.java-agents`
  until it is measured that an agent's class-file hook and CDS agree on
  every JDK jrs supports (§8, question 1).
- **Forks** read one archive; only a run in one JVM dumps, so concurrent
  forks never race for the temporary file. A suite that always forks gets its
  archive from the first `--forks 1` or single-JVM run, or from a fork chosen
  to dump (§8, question 2).

### 3.4 Manifest

```toml
[test]
share-classes = true    # default false
```

Opt-in at first, since §3.2's ordering change cannot be fully checked from
names. The default flips once the corpus of ROADMAP §1 runs green with it on.
`jrs migrate` writes nothing for it.

### 3.5 The JDK 24+ AOT cache

JEP 483's AOT cache (`-XX:AOTCache`, one step with `-XX:AOTCacheOutput` from
JDK 25, JEP 514) also stores linked classes and, from JDK 25, method
profiles, which is the larger win for Spring. It has the same classpath rule
and caches only the built-in loaders' classes, which with §3.2's layout are
exactly the dependency jars. It is the same design with other flags, chosen
by `Toolchain::version`. It ships after the dynamic archive has been measured
against it on the corpus.

The first measurement ([TEST_JVM_BENCHMARK.md §7](TEST_JVM_BENCHMARK.md#7-results-first-pass))
kept it opt-in. On JDK 25 it beat the archive's `test JVM` time by 17–20%
on a Ktor project and a plain library (−43% and −42% against no sharing),
but on three Spring Boot petclinics it was never assembled: Mockito's
self-attached agent appends to the boot class path in the recording run,
`-XX:AOTMode=create` fails on it, and every later run recorded again, 32–134%
slower than no sharing at all.

---

## 4. Pipelined downloads

### 4.1 What waits on what

`Session::dependencies()` today:

```
resolve (or read jrs.lock) ─► download every jar ─► compiler tools ─► write jrs.lock
                                                                          │
build(): compile main ◄───────────────────────────────────────────────────┘
test():  compile tests, fetch the launcher, run
```

The main compile needs the compile classpath (`Classpath::Compile`) and its
language's compiler graph. Nothing else: runtime-only and test-scoped jars,
task tools, agents and the obfuscator are needed later or never.

### 4.2 Two waves

```
resolve ─► wave 1: Compile classpath + compiler tools ─► compile main ─► … ─► join ─► write jrs.lock
      └─► wave 2: everything else (and, for `jrs test`, the launcher) ──────┘
```

- **Wave 1** is downloaded as today, in the foreground, under the
  `Downloading` phase line and its bars.
- **Wave 2** starts on a thread of its own as soon as wave 1 has been handed
  to the pool. Both waves share one pool of `--jobs` workers, wave 1's
  downloads queued first, so the background wave never takes bandwidth from
  the jars the compiler is waiting for.
- **The join** comes before whatever first needs wave 2: compiling the
  tests, packaging, running, writing `jrs.lock`. `jrs build` joins at the end
  of `build()`, so that a fresh resolution still writes a lockfile that pins
  every jar's checksum, as SPEC §8 requires, and `jrs build` still leaves the
  cache complete for `--offline`.
- **Errors** from wave 2 are held and reported at the join, with the same
  message and exit code as today. A main compile that fails first is reported
  first: it is the error the user can act on.

### 4.3 Output

Phase lines keep today's order. `Downloading <n> artifacts` is still printed
once, before `Compiling`, with `n` counting both waves. The output layer has
one live region (`ui::Live`), so wave 2 has no bars while the `Compiling`
spinner owns it; if wave 2 is still running at the join, the `Downloads`
region takes over with what is left, as if it had just started. Under
`--progress never` the transcript is identical to today's.

### 4.4 Scope

Only a cold or partly cold cache gains anything; with everything cached,
both waves are a `locate_cached` pass and the thread is not started. The
benchmark is "clean build, cold dependency cache" of ROADMAP §2, against a
local mirror. The change is in `cli.rs` (`dependencies`, `download`, the join
points) and `resolve::fetch_jars`, which gains a variant taking a subset of
the packages.

---

## 5. A build cache

### 5.1 What it stores

Two kinds of entry, both content-addressed:

| Entry | Key | Value |
| --- | --- | --- |
| A compile unit's output | §5.2 | The `.class` files and other compiler output the unit wrote, as a deterministic zip |
| A passing test run | §5.3 | The XML reports and the outcome's counts |

Task outputs (`[tasks]` with `outputs`) are left for later (§8, question 5).

### 5.2 Compile keys

A unit's key is a SHA-256 over a text that must not contain an absolute path
or a modification time, so that two checkouts of the same commit — on two
machines, or in two directories — get the same key:

- the jrs version, which generates the flags;
- the JDK: `javac -version`'s full output, since a patch release may change
  the bytes `javac` writes;
- the unit's generated and user flags, with the project root replaced by a
  placeholder (`-d {root}/target/classes`);
- each source's path relative to the root, and the SHA-256 of its contents
  (the incremental index and `impact.rs` already hash them);
- each classpath entry: a dependency jar by coordinate and its pinned
  checksum from `jrs.lock` (a snapshot or a local jar by its content hash);
  `target/classes` on the test unit's classpath by `compile::api_digest`,
  as the test fingerprint already counts it;
- the foreign compiler: coordinate, version, flags and its own graph's
  checksums.

This is `CompileUnit::settings` with paths relativised and hashes in place
of sizes and mtimes. An annotation processor on the classpath is covered by
its jar's checksum and its options by the flags. A processor or Groovy AST
transformation that reads a file it was not given, or an environment
variable, is outside the key: such a build sets `--no-build-cache` (§5.6).
That is the same trust Gradle's cache asks of a task's declared inputs, and
it is the one place this proposal accepts a rule the user has to know (§8,
question 4).

### 5.3 Test keys

The test record of `impact.rs` already digests what a run's outcome depends
on: the JVM's arguments and environment, the launcher, each jar, and every
file in the class directories. A whole-suite run that passed, with no flaky
test, stores its reports under SHA-256 of that digest with paths relativised.
A later run whose class directories hash the same, after a branch switch or
on another CI machine, restores the reports and prints

```
     Testing 214 test classes: passed in the cached run (`--all` runs them)
```

instead of starting the JVM. A failing or flaky run is never stored, and the
selective runs of §10.2 (`--filter`, tags, `--method`) neither read nor
write it.

### 5.4 Flow

```
 compile unit stale?
    │ yes
    ▼
 key ─► local entry? ─ no ─► remote entry? (§5.5) ─ no ─► compile; store; done
    │ yes                         │ yes
    ▼                             ▼
 empty the output dir, extract, write the fingerprint, rebuild the index
    │
 phase line:  Restored my-app v1.0.0 (from the build cache)
```

- **Restoring** writes the classes the zip holds into an emptied output
  directory, then the unit's fingerprint, then the incremental index by the
  same `record` pass a whole compile ends with (the index holds paths and
  mtimes, so it is rebuilt, never stored). Resources are not in the entry:
  the resource phase syncs them after a compile or a restore alike.
- **Storing** happens after every successful compile, whole or file by file.
  The value is every file in the output directory that the resource sync did
  not put there (`resources-<unit>.list` says which), so annotation
  processors' generated resources are kept. It is written as `package.rs`
  writes jars: sorted entries, the fixed 1980 timestamp. Restored classes
  are byte-identical to compiled ones because both are what the compiler
  wrote. `tests/build.rs` checks it on two checkouts.
- **Layout:** `<cache>/build/<k[0..2]>/<key>.zip`, written to a temporary
  file and renamed, as every cache write is. A hit touches the entry's
  access time, as `mark_used` does for jars, so `jrs cache prune
  --unused-for` keeps what is used. A plain `jrs cache prune` drops all of
  `build/`, since no lockfile names it.

### 5.5 Remote

A remote cache is the same keys over HTTP:

- `GET <url>/<key>.zip` on a local miss, stored locally on a hit;
- `PUT <url>/<key>.zip` after a store, **only** when pushing is turned on.

Configuration is per machine, not per project, since it names
infrastructure:

```toml
# ~/.config/jrs/config.toml
[build-cache]
url = "https://cache.example.com/jrs"   # or file:///mnt/shared/jrs-cache
push = false                             # CI sets true
credentials = "build-cache"              # a [credentials.<name>] entry
```

`JRS_BUILD_CACHE_URL` and `JRS_BUILD_CACHE_PUSH` override it, for CI.
`file://` is supported as `resolve/repo.rs` supports it, which is also how
the tests run without a server. Requests go through the existing `ureq`
agent, so the `[proxy]` settings apply. A remote that fails or
times out is reported once with `-v` and skipped for the rest of the command.

**Trust.** Anyone who can push can put classes into every build that reads
the cache, which is a supply-chain door the rest of jrs keeps shut
(checksummed jars, no build scripts). So pushing is off unless asked for,
the docs say to give push rights to CI alone, and an entry is only ever read
by its content key: a client cannot be served an entry for other inputs, only
a wrong value under the right key. `jrs build --verify-cache` compiles
anyway and compares with what the cache holds, failing on a difference; a CI
job can run it nightly (§8, question 6).

### 5.6 Turning it off

- `--no-build-cache` on `build`, `test`, `run` and `package`;
- `JRS_BUILD_CACHE=off`;
- `[build-cache] enabled = false` in the user config.

The local cache is on by default, as the dependency cache is; the remote one
exists only when configured.

---

## 6. A warm `javac` under `--watch`

### 6.1 The worker

`jrs build --watch`, `jrs test --watch` and `jrs task --watch` keep one jrs
process alive for the session (SPEC §7.5). Inside it, jrs starts one `javac`
worker JVM the first time a `javac` step runs, and sends it every later
`javac` step of the session:

- **The program.** A Java class of about a hundred lines,
  `JavacWorker.java`, embedded in jrs with `include_str!`. On first use it
  is compiled with the project's own `javac` into
  `<cache>/worker/<jrs version>-jdk<n>/`, as the tests compile their fake
  compilers, so jrs ships no binary and downloads nothing. The worker gets
  its own CDS archive through `compile/share.rs`.
- **The protocol.** One line per request on the worker's stdin: an id and
  the argfile path jrs already writes (`target/.jrs/javac-<unit>.args`). The
  worker runs
  `ToolProvider.getSystemJavaCompiler().run(null, out, err, "@" + argfile)`
  with `out` and `err` captured, and answers on stdout with a header line
  `jrs-worker <id> <exit code> <bytes>` followed by the captured output.
  javac's in-process exit codes are the process's (0, 1, 2, 3, 4), and its
  diagnostics are the same text, so the output layer passes them through
  verbatim as before.
- **Freshness.** Each request gets a new `StandardJavaFileManager`, closed
  after the run, so no jar stays open or cached between builds: a snapshot
  rewritten in place, or a jar `jrs update` replaced, is read again. Javac
  builds a new `Context` for every call, and annotation processors get a new
  class loader per compilation, so no compiler state crosses builds.
- **Flags.** The worker's JVM gets no quick flags (§1): it is the opposite
  of short-lived. `-Xss` and `-Xmx` follow the JVM's defaults; a
  `[java] worker-jvm-args` key is not added until a project needs one.

### 6.2 Lifetime

The worker is a child of the jrs process, and dies with it:

- it exits when its stdin closes, which happens when jrs exits by any route,
  `SIGKILL` included, so no orphan can outlive the session;
- jrs restarts it after 50 compilations, to bound what a processor's leaked
  class loaders cost, and whenever the manifest's JDK changes;
- a worker that dies, answers garbage or takes longer than the forked
  `javac` would by a wide margin is killed, and that step runs as a forked
  `javac`. Its output is then the forked run's alone, so a worker failure is
  never visible in the transcript except as a `-v` line.

Outside `--watch` nothing changes: a single build pays the worker's start-up
for one compile, so it forks `javac` as today.

### 6.3 Why this is not the compiler daemon

SPEC §1.2 rules out compiler daemons, and ROADMAP §5 keeps "a compiler
daemon (the Gradle daemon)" there. The line this proposal draws is
**lifetime**: the Gradle daemon outlives the command that started it, is
found again by later commands, and is managed (status, stop, idle timeout).
The worker is born and dies inside one `jrs … --watch` command, is reached
only through a pipe jrs holds, and needs no management because it cannot be
found by anything else. §7 gives the SPEC wording.

### 6.4 Kotlin, Scala, Groovy

Out of scope for this proposal. kotlinc's in-process entry point is the
Kotlin Build Tools API, which also brings incremental Kotlin compilation and
its own caches (ROADMAP §5); scalac's is `scala.tools.nsc.Main.process`, and
Scala 3's `dotty.tools.dotc.Main`. Each is a worker of the same shape with its
own protocol shim, and each waits for a measurement on the corpus saying the
second of JVM start-up JVM_LANGUAGES.md §14.1 notes is felt.

---

## 7. SPEC.md changes

Each part changes SPEC.md before its code lands:

1. **Test CDS (§3).** SPEC §4.2: `test.share-classes`. SPEC §10.2: the layout,
   the overlap check, the archive's key and what is never shared. ARCH §13:
   `cds/test-*.jsa` in the shared cache.
2. **Pipelining (§4).** SPEC §8.3 and §8.4 (caching, parallelism): the two
   waves, and that a fresh `jrs.lock` is still written only once every jar is
   checksummed. SPEC §6.1's data flow diagram.
3. **Build cache (§5).** A new SPEC §7.8 "Build cache" with §5.2–§5.6 in
   condensed form; SPEC §8.5 (network access and user configuration):
   `[build-cache]`; SPEC §8.6 (cache maintenance): `build/` and `prune`; the
   `--no-build-cache` flag in the command table; ROADMAP §5 loses its build
   cache row.
4. **Warm compiler (§6).** SPEC §1.2's language bullet becomes:

   > … compiler plugins, kapt/KSP, incremental compilation and compiler
   > daemons (§7.7). A compiler daemon is a process that outlives the jrs
   > command that started it; a `--watch` session may keep one `javac`
   > worker for its own lifetime (§7.5), which no later command can reach.

   SPEC §7.5 gains a paragraph on the worker, and ROADMAP §5's compiler
   daemon row says what is left of it (a daemon across commands, Kotlin).

---

## 8. Open questions

1. **Agents and CDS.** JaCoCo and Mockito's agent install a class-file load
   hook. Recent JDKs re-read the class bytes for a hooked class and keep
   sharing the rest; whether that holds from JDK 17 on, and costs nothing in
   correctness, needs a test per JDK before `--coverage` and
   `test.java-agents` runs may share.
2. **Who dumps under forks.** A suite that always forks never has a
   single-JVM run. The first fork could dump while the others read nothing,
   at the cost of that run's balance; or the archive could come from a
   launcher run that discovers but executes nothing. To be measured.
3. **Remote cache protocol.** Plain `GET`/`PUT` of `<key>.zip` fits nginx,
   S3 presigned URLs and a shared drive. Gradle's HTTP cache uses the same
   shape, so a Gradle cache node could serve jrs too. Whether that is worth
   a test against a real node is open.
4. **Undeclared inputs.** A processor or AST transformation reading files by
   path is outside the key (§5.2). The alternative to trusting it is to keep
   any unit with a registered processor out of the cache, which leaves out
   every Lombok project. The proposal trusts it and documents
   `--no-build-cache`; the corpus may say otherwise.
5. **Task outputs.** A task with `inputs` and `outputs` (TASKS.md §6) has
   what a cache key needs. Its outputs are arbitrary paths, though, some
   outside `target/`, and restoring them is a second design.
6. **Verifying a remote cache.** `--verify-cache` compiles and compares. Its
   only use is in CI, which is where the push credentials are; whether a
   separate command or a flag is right is open.
7. **The test cache and test selection.** A whole-suite hit (§5.3) skips the
   JVM, while `impact.rs` keeps its own per-checkout record. A combined
   design — per-class results in the cache, keyed by each class's reach —
   would let a branch switch reuse part of a suite's results. It needs §5 to
   exist first.

---

## 9. Implementation plan

### 9.1 Milestones

In order of value per line of code, each with its benchmark from ROADMAP §2:

| | Part | Measured by |
| --- | --- | --- |
| F1 | Test CDS, opt-in (§3), dynamic archive only | `test` on the Spring Boot corpus projects, warm |
| F2 | Local build cache, compile units (§5.1–§5.4, §5.6) | `build` after a branch switch; after `jrs clean` |
| F3 | Warm `javac` under `--watch` (§6) | incremental rebuild after one edit, the second and later |
| F4 | Remote build cache (§5.5) | a CI clean build with a warm remote |
| F5 | Test results in the build cache (§5.3) | `test` after a branch switch |
| F6 | Pipelined downloads (§4) | clean build, cold dependency cache, local mirror |
| F7 | AOT cache on JDK 25 (§3.5) | as F1 |

### 9.2 Code layout

- `compile/share.rs` gains the test archive (§3.3) beside the compilers'.
- `test.rs`: `TestRun::args` with the `--class-path` layout; the overlap
  check.
- `build_cache.rs` (new): keys, zip in and out, local and remote stores.
  `compile/mod.rs` calls it from `compile_timed`; `cli.rs` emits the
  `Restored` phase line.
- `compile/worker.rs` (new) and `compile/JavacWorker.java`: the worker and
  its protocol; `javac::run` asks it first under `--watch`.
- `cli.rs` and `resolve/mod.rs`: the two waves of §4.

### 9.3 Tests

All hermetic, against the `file://` fixture repository, as the rest of the
suite is:

- **Test CDS:** an integration test that a second `jrs test` with
  `share-classes = true` maps an archive (checked through `-Xlog` in a
  verbose run), that a changed test class runs its new code, and that a
  project shadowing a fixture jar's class keeps today's layout.
- **Build cache:** two `Scratch` checkouts of one project sharing a
  `JRS_CACHE_DIR`: the second build prints `Restored` and its classes equal
  the first's byte for byte; a changed source, flag, jar or JDK misses; a
  corrupt entry is ignored and overwritten; a `file://` remote serves a third
  checkout with an empty local cache, and is never written without `push`.
- **Worker:** the protocol is tested in `compile/worker.rs` against a real
  worker, without `--watch`: three requests through one worker, a compile
  error's diagnostics equal a forked `javac`'s byte for byte, and a killed
  worker falls back to a forked `javac`. No test drives a `--watch` session
  today; one that spawns `jrs build --watch`, edits a source three times and
  checks for a single worker start in the `-v` output comes with F3.
- **Pipelining:** `tests/output.rs` snapshots the plain transcript of a cold
  build against today's; a failing wave-2 download fails the build with
  today's message after the main compile.
