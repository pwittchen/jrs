# jrs — Task outputs in the build cache

Design proposal for the third item left in
[ROADMAP §6](../ROADMAP.md#6-faster-than-gradle), and the answer to
[FASTER_BUILDS.md §8](FASTER_BUILDS.md#8-open-questions), question 5: a
user-defined task's outputs, restored from the build cache of SPEC §7.8
instead of run again.

Status: **T1, T2 and T3 implemented** (§8.1), with one change to §4.1: a
class directory on a classpath the task reads is keyed by the bytes of its
files, not by `compile::api_digest`, since a task may run the classes it
reads, and a method body then changes its output. It extends two contracts,
the task model of [TASKS.md](TASKS.md) (SPEC §7.6) and the build cache
(SPEC §7.8), and crosses one line drawn in TASKS.md §7: there, a task's
generated output must live under `target-dir`. The SPEC edits of §6 are
made, and SPEC §7.6's **Cached tasks** and §7.8 are the contract; this
document is the design record.

---

## 1. Problem

A task with both `inputs` and `outputs` is skipped when its fingerprint
matches (TASKS.md §6, `task::Prepared::is_fresh`). That fingerprint has the
weakness the compile fingerprint had before the build cache:

- **It lives in `target/.jrs/tasks/<name>.fingerprint`.** A `jrs clean`, a
  fresh checkout or a CI machine runs every task again.
- **It is made of sizes and modification times** (`task::fingerprint`
  stamps each input, each jar and each tool jar). A branch switch that
  rewrites an input with the same bytes still reruns the task. And the
  fingerprint is full of absolute paths, so it never means anything on
  another machine.

The tasks this costs most are the code generators: an OpenAPI or protobuf
generator in `pre-compile` (`jrs migrate` writes those from Gradle's
`openApiGenerate` and `sourceSets` srcDirs), a frontend bundle, or a
`script` task over a schema. They run on every clean build, and they run
before `javac`, so the compile cache cannot help: the compile unit's key
includes the generated sources, and those do not exist yet.

The build cache already has everything the fix needs: content keys with
`{root}`/`{cache}` placeholders (`BuildCache::relative`), jar identities by
coordinate and pinned checksum (`BuildCache::jar`), deterministic entries
(`build_cache::zip`, `unzip`, `collect`, `extract`), and a local and remote
store (`BuildCache::load`, `save`, `Remote`). What it lacks is a key for a
task, and rules for writing files back into places a user may also write to.

## 2. Principles

FASTER_BUILDS.md §2 holds unchanged. In addition:

- **Opt-in per task.** Unlike a compile unit, a task is arbitrary code: jrs
  cannot know whether it reads the network, the clock or a file it was not
  told about. A wrong compile entry is a wrong class; a wrong task entry may
  be a wrong anything. A task is cached only when its manifest says so.
- **The fingerprint stays first.** `Fresh (task)` from the fingerprint costs
  a few `stat`s. The cache is asked only when the fingerprint says the task
  would run.
- **Never touch what jrs did not write.** Restoring an output must not
  delete a user's file. Where jrs cannot be sure, it runs the task.
- **A restored output is the bytes the task wrote**, with the task's own
  file modes kept for executables; nothing else about the run is replayed.

---

## 3. Which tasks are cached

### 3.1 The key

```toml
[tasks.openapi]
main = "org.openapitools.codegen.OpenAPIGenerator"
args = ["generate", "-i", "api.yaml", "-g", "spring", "-o", "{target}/openapi"]
inputs = ["api.yaml"]
outputs = ["{target}/openapi"]
source-outputs = ["{target}/openapi/src/main/java"]
cache = true

[tasks.openapi.dependencies]
"org.openapitools:openapi-generator-cli" = "7.14.0"
```

`cache = true` is a new key in `[tasks.<name>]`, default `false`. It is
accepted only on a task that declares both `inputs` and `outputs`; on any
other it is a manifest error naming the missing key, since without them the
task is not up-to-date-checked at all (TASKS.md §6), let alone keyed.

**Recommendation: opt-in, never inferred.** The alternative, caching every
task with inputs and outputs, would cache a `shell = "curl … > target/x"`
task the day it gains an `outputs` entry. Gradle draws the same line with
`@CacheableTask`: up-to-date checking is the default, caching is not.
`jrs migrate` writes `cache = true` for nothing, since a Gradle build that
caches a task proves only that Gradle did.

### 3.2 What is never cached

`check` refuses `cache = true` on:

- a task whose action uses `{jar}`: it runs after `package` and is, in
  every case seen so far, a publish or deploy step;
- a task that a `post-package` or `pre-run` hook reaches: those hooks exist
  for side effects;
- a task with an output outside the project root (§4.2).

The rest is the user's word. A task that talks to the network and declares
`cache = true` is cached; the documentation says what that means.

---

## 4. The design

### 4.1 The key

A SHA-256 over a text built as the compile key is (SPEC §7.8), of kind
`task`, so that no task key can equal a compile or test key. It holds no
absolute path and no modification time:

- jrs's version and the JDK, as every key does (`BuildCache::key`);
- the task's name;
- the launch after placeholder expansion (`task::Launch`: the program or
  shell script and its arguments), its `cwd` and its own `env`, each through
  `BuildCache::relative`, so `{root}` and `{cache}` replace the checkout's
  paths;
- for a `run` task, the program by its name as written, not as found on
  `PATH`: the key cannot vouch for a tool outside the project, and claiming
  to would be worse than leaving it to the user (§7, question 1);
- each input file by its path relative to the root and the SHA-256 of its
  contents, a directory walked with `project::find_all`, sorted, as today;
  a missing input as `missing <path>`;
- each jar of the task's own `dependencies` by `BuildCache::jar`, coordinate
  and pinned checksum;
- when the task reads a classpath placeholder (`task::needs_classpath`), each
  jar on the classpaths it names by `BuildCache::jar`, and each class
  directory on them by the hash of every file in it — by bytes, not by
  `compile::api_digest` as first proposed, since a task that runs the
  classes depends on their method bodies; a task reading the test classpath
  needs both class directories hashed, and a directory that does not exist
  yet is `missing <path>`;
- each output, by its relative path, so that two tasks with the same command
  and different outputs cannot share an entry.

What the task depends on through `depends-on` is not in the key as such. If
it feeds the task, it is an input (a file) or a classpath (a placeholder),
and is counted there. A dependency that feeds it any other way is an
undeclared input, which is the user's to declare (§2).

The fingerprint and the key are computed from the same expansion
(`task::prepare`), so `Prepared` gains the key's text beside its fingerprint
when the task is cacheable. Hashing inputs costs a read of each, which the
fingerprint avoids, so the key is worked out only after `is_fresh` said no.

### 4.2 Outputs: where they may be

`outputs` today may name any path; only `source-outputs` and
`resource-outputs` must be under `target-dir` (`task::check`). For a cached
task:

- **Under the project root, always.** An output outside the root is refused
  for `cache = true`: restoring it would write outside the project.
- **Under `target-dir`: restored freely**, since `target/` is jrs's and fully
  disposable.
- **Elsewhere under the root: restored only into a path that does not
  exist, or that the last restore or run of this task wrote whole.** A
  record `target/.jrs/tasks/<name>.outputs` lists every file the task's last
  successful run or restore left in each output, by relative path and hash.
  A file in an output that is not on that list, or whose hash differs, is
  the user's: the task runs instead, with a `-v` line naming the file.

**Recommendation:** allow outputs outside `target/` but under the root,
with that guard, because the real case exists: a generator that writes a
checked-in client into `src/generated/`. Refusing it would push exactly
those projects to an uncached task, which is today's state.

### 4.3 The entry

The value is the outputs' files as one zip in the entry format of
`build_cache::zip`, each path prefixed by its output's index
(`0/…`, `1/…`), plus one more file, `jrs-task.txt`, holding:

- each output and whether it is a directory or a file, so a restore can
  tell an output that was a file from a directory holding one file;
- each file's mode: `644` or `755`. The current entry format fixes every
  mode at `0o644`; a task's outputs need the execute bit kept (a generated
  launcher, a bundled binary). `zip` gains a variant that writes the mode
  given, still from a fixed set, so entries stay deterministic.

An output that does not exist after a successful run is recorded as absent
and restored as absent. A task whose outputs hold a symbolic link is not
stored: `-v` says so, and it runs every time it is not fresh. Links are
where an innocent-looking entry writes outside the root.

### 4.4 Flow

```
 task fresh by its fingerprint? ── yes ─► Fresh <name> (task)
    │ no
 cache = true and the cache on? ── no ─► run
    │ yes
 key ─► local entry? ─ no ─► remote entry? ─ no ─► run; on success store
    │ yes                      │ yes
    ▼                          ▼
 every output safe to replace (§4.2)? ── no ─► run
    │ yes
 remove each output, extract, write the record and the fingerprint
    │
 Restored <name> (task, from the build cache)
```

- **Restoring** removes each output first, file or directory, so that a
  file the run would not have written is not left behind, then extracts
  the entry. It writes `<name>.outputs` and the fingerprint
  (`Prepared::record`), so the next build finds the task fresh without
  reading the cache at all.
- **Storing** happens after a successful run, as `Prepared::record` does,
  with the files `collect` finds in each output. A failed run stores
  nothing and forgets the fingerprint, as today.
- **Output is not replayed.** A restored task prints its phase line and
  nothing of what it printed when it ran, as a restored compile prints no
  `javac` warnings. **Recommendation:** no replay; a task's output is a log
  of work that did not happen this time. A `-v` line names the entry.
- **The exit code** is not stored: only a successful run is ever stored.

`jrs task <name>` (a named task, `named.is_some()` in `Session::run_task`)
restores like a hook does: running it by hand is a request for its outputs,
not for its side effects. `jrs task <name> -- args` with extra arguments
never restores: its arguments are not in the manifest, and a cache keyed on
them would fill with one-off entries.

### 4.5 Turning it off

The switches of SPEC §7.8 apply: `--no-build-cache` on `build`, `test`, `run`
and `package`, `JRS_BUILD_CACHE=off`, `[build-cache] enabled = false`.
`jrs task` gains `--no-build-cache` too, for the same reason the others have
it. Removing `cache = true` from one task turns it off for that task alone.

### 4.6 The remote cache

A remote serves task entries under the same `GET`/`PUT` of `<key>.zip`.
The trust argument is stronger than for classes, and the documentation says
so:

- **An output can be executable code that runs outside any JVM**: a
  generated shell script, a bundled frontend served to browsers, a binary
  the next task executes. A class reaches the project's own JVM only.
- **A task's key is weaker than a compile unit's**: it trusts the declared
  inputs, and `run` names a program jrs does not hash (§4.1).

So a remote is read for task entries only when `[build-cache]` says
`tasks = true` as well; the default is compile and test entries only.
Pushing stays CI's alone, as SPEC §7.8 already asks. `--verify-cache`
covers tasks too: it runs every cacheable task and fails on a difference
from the entry under its key, file by file, as it does for compile units.

---

## 5. What changes in the code

- `manifest.rs`: `cache` in `TASK_KEYS`, a `cache: bool` on `TaskDef`,
  parsed by `bool_key`, rendered back by `render`.
- `task.rs`: `check` refuses `cache = true` without `inputs` and `outputs`,
  with `{jar}`, from `post-package` or `pre-run`, or with an output outside
  the root. `Prepared` gains the key's text (`cache_text`), built beside
  `fingerprint` from the same expansion, and the `<name>.outputs` record
  with the safety check of §4.2.
- `build_cache.rs`: a `zip` variant that keeps a fixed set of file modes,
  and `extract` that applies them; the `tasks` switch on `Remote`.
- `cli.rs`: `Session::run_task` asks the cache after `is_fresh`, prints
  `Restored <name> (task, from the build cache)`, and stores after a
  successful run; `--no-build-cache` on `jrs task`.
- `config.rs`: `tasks` in `[build-cache]`.

Nothing in `toolchain.rs` changes: a cached task's run is the same
subprocess as today's.

---

## 6. SPEC.md changes

1. **SPEC §4.2:** `tasks.<name>.cache`, and its errors.
2. **SPEC §7.6** (tasks and hooks): cacheable tasks, the key, the outputs
   rule of §4.2, what is never cached, and the `Restored` line. TASKS.md
   §6 gains a pointer.
3. **SPEC §7.8** (build cache): the `task` kind of key, the entry's
   `jrs-task.txt` and file modes, `--verify-cache` over tasks.
4. **SPEC §8.5:** `[build-cache] tasks`.
5. **SPEC §5.1:** `--no-build-cache` on `jrs task`.
6. **TASKS.md §7** keeps its rule for `source-outputs` and
   `resource-outputs`; this proposal does not move generated sources out
   of `target-dir`. It only lets a plain `outputs` entry outside `target/`
   be restored under the guard of §4.2.

---

## 7. Open questions

1. **`run` programs off `PATH`.** A `run = ["protoc", …]` task's key holds
   the name `protoc`, not its version. Two machines with different `protoc`
   releases share entries. Hashing the binary found on `PATH` would close
   that for a single executable, but not for a tool with a runtime beside
   it. The proposal keys by name and documents it; a
   `tool-version = ["protoc", "--version"]` key whose output joins the key
   is the alternative, at the cost of a process per build.
2. **Output records and `jrs clean`.** `<name>.outputs` lives in `target/`,
   so after a clean an output outside `target/` has no record, and every
   file in it counts as the user's: the first build after a clean runs the
   task rather than restoring. Keeping the record beside the output instead
   would put jrs's file in the user's tree. The proposal accepts the one
   extra run.
3. **Generated sources and the compile key.** A restored `source-outputs`
   tree is byte-identical to a generated one, so the compile key that
   follows hits too. Nothing here needs to know that, but it is the payoff
   worth measuring: a clean build of a generator-heavy project restoring
   both.
4. **Partial outputs.** A task that writes only some of its outputs on a
   given run (a generator that skips unchanged files) is stored as it left
   them. Since the key covers every input, the next run from the same
   inputs leaves the same files; the record of §4.2 makes the difference
   visible if not.
5. **Should tasks share an entry?** Two tasks with identical commands,
   inputs and outputs would, but for the task name in the key. Keeping the
   name costs nothing and avoids a task restoring another's entry after a
   rename that changed its meaning.

---

## 8. Implementation plan

### 8.1 Milestones

| | Part | Measured by |
| --- | --- | --- |
| T1 | `cache = true`, the key and local store for outputs under `target-dir` (§3, §4.1, §4.3, §4.4) | clean build of a project with an OpenAPI generator, after `jrs clean` |
| T2 | Outputs elsewhere under the root, with the record of §4.2 | as T1, with the generator writing into `src/generated/` |
| T3 | Remote task entries behind `[build-cache] tasks` and `--verify-cache` over tasks (§4.6) | a CI clean build with a warm remote |

T1 is what most projects need; T2 and T3 wait for a project in the corpus
of ROADMAP §1 that needs them.

### 8.2 Tests

All hermetic, as the rest of the suite is. A cacheable task in the tests is
a Java `script` (`tests/build.rs` already uses `Gen.java`), so every CI leg
runs them with nothing but a JDK:

- **Manifest:** `cache = true` without `inputs` or `outputs`, with `{jar}`,
  reached from `post-package`, or with an output outside the root is a
  manifest error naming the key; `render` writes it back.
- **Restore:** two `Scratch` checkouts sharing a `JRS_CACHE_DIR`; the second
  `jrs build` prints `Restored gen (task, from the build cache)`, does not
  start the generator (a marker file it would write is absent), and its
  output tree equals the first's byte for byte, executable bit included.
- **Misses:** a changed input, a changed `args` entry, a changed tool jar
  (`[tasks.x.dependencies]` at another version) and a changed main class
  each run the task.
- **Never touched:** a file the user put into an output outside `target/`
  makes the task run instead of restoring, with the `-v` line naming it,
  and the file survives.
- **Unchanged without the key:** a task without `cache = true` behaves as
  today, and `--no-build-cache` runs a cacheable task and stores nothing.
- **Remote:** a `file://` remote serves task entries only with
  `tasks = true`, and `--verify-cache` fails on a forged task entry.
