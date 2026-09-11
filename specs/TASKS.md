# jrs — Custom tasks and lifecycle hooks

Design proposal for user-defined tasks and build hooks, in the spirit of
Gradle's `tasks.register` / `dependsOn` / `doLast`, but declarative and small.

Status: **T1, T2 and T3 (tool dependencies, §8) implemented.** This
crossed a line drawn in [SPEC §1.2](INITIAL_SPEC.md#12-non-goals) ("plugin
systems, custom task graphs, or a build DSL"), so per
[ROADMAP §5](../ROADMAP.md#5-needs-a-spec-decision-first) it needed an
INITIAL_SPEC.md change before it needed code. §2 below is that argument; §10
lists the INITIAL_SPEC.md edits, which have been made. This document is kept as
the design record, corrected where the implementation settled a detail
differently; [SPEC §7.6](INITIAL_SPEC.md#76-tasks-and-hooks) is the condensed
contract.

---

## 1. Problem

jrs covers the build a Java project *usually* has: resolve, compile, test,
package. Real projects almost always have one or two steps that are theirs
alone:

- **Code generation before `javac`** — protobuf/gRPC stubs, a `BuildInfo.java`
  carrying the version and git hash, JOOQ classes, ANTLR parsers.
- **Post-processing after compilation** — bytecode weaving, generating a
  resource from the compiled classes, an index file.
- **Work after packaging** — signing the jar, writing a `.sha256`, copying the
  artifact somewhere, building a Docker image.
- **Standalone chores** — formatting (`google-java-format`), a linter, a
  database migration, starting a dev dependency.

Today the only answer is a wrapper — a `Makefile`, `justfile` or shell script
that calls `jrs`. That works for standalone chores and for work after
`jrs package`, but it **cannot reach into the middle of a build**: generated
sources have to exist before `javac` runs, and `javac` runs inside `jrs build`.
A wrapper would have to generate first and hope nothing else is needed. It
also has no access to what jrs knows: the pinned JDK (§7.1), the resolved
classpath, the jar path.

## 2. Position on the non-goal

SPEC §1.2 rules out three things. This proposal keeps two of them out and
narrows the third:

| SPEC §1.2 says no to | This proposal |
| --- | --- |
| **A build DSL** (Groovy/Kotlin/XML) | Still no. Tasks are TOML tables. There is no expression language, no conditionals, no loops. |
| **Plugin systems** | Still no. No plugin API, nothing loaded into the jrs process, no extension points in Rust. A task is a subprocess, which is what jrs already does with `javac`. |
| **Custom task graphs** | **Narrowed.** Users may declare named tasks, order them with `depends-on`, and attach them to a fixed set of lifecycle points. The built-in pipeline itself is not a graph users can rewire: phases cannot be removed, replaced or reordered. |

The proposed replacement text for §1.2 is in §10.

The deciding argument is §1.1's "thin, predictable driver over the JDK
toolchain". A task is exactly that — jrs spawning a program with a known
environment, at a known point, passing its output through verbatim. The line
that matters is that **build logic never runs inside jrs**; it runs in a
subprocess that jrs launches. That line holds.

### 2.1 Alternatives considered

- **Do nothing; recommend a wrapper.** Honest, zero code, and fine for chores.
  Fails on code generation (§1), which is the most common need.
- **Embedded scripting (Rhai, Lua, Starlark).** A new crate, a new language to
  learn, and a DSL by another name. Rejected by §1.2 and §13.1 both.
- **Plugins as dynamic libraries or WASM.** An ABI to maintain, a loader, a
  security story. Far outside "small enough to be readable end to end".
- **A Java plugin API** (jrs loads user classes into a JVM it controls). Needs a
  jrs-side Java library, versioning, a protocol between JVM and jrs. The
  `script` task kind (§4.2) gets most of the value — build logic written in
  Java — without any of that.

---

## 3. Overview

```toml
[tasks.build-info]
description = "Generate BuildInfo.java with the version and git hash"
script = "build/GenerateBuildInfo.java"      # run with the project's JDK
args = ["{target}/generated/sources", "{project.version}"]
inputs = ["build/GenerateBuildInfo.java", ".git/HEAD"]
source-outputs = ["{target}/generated/sources"]

[tasks.checksum]
description = "Write a SHA-256 next to the jar"
shell = "shasum -a 256 \"$JRS_JAR\" > \"$JRS_JAR.sha256\""

[tasks.format]
description = "Format the sources in place"
run = ["google-java-format", "--replace", "--glob=src/**/*.java"]

[tasks.release]
description = "Package, then checksum"
depends-on = ["package", "checksum"]

[hooks]
pre-compile = ["build-info"]
post-package = ["checksum"]
```

```
$ jrs package
    Resolving 3 declared dependencies
         Task build-info (pre-compile)
    Compiling my-app v1.0.0 (48 source files)
    Packaging target/my-app-1.0.0.jar
         Task checksum (post-package)
     Finished package in 1.84s

$ jrs task format
         Task format
     Finished task format in 0.62s

$ jrs task --list
build-info   Generate BuildInfo.java with the version and git hash   (pre-compile)
checksum     Write a SHA-256 next to the jar                          (post-package)
format       Format the sources in place
release      Package, then checksum
```

Three concepts, no more:

1. **Task** — a named command in `[tasks.<name>]`.
2. **Dependency** — `depends-on`: other tasks, or a built-in command, that must
   run first.
3. **Hook** — `[hooks]`: tasks that run at a fixed point in a built-in command.

---

## 4. Manifest format

### 4.1 `[tasks.<name>]`

| Key | Required | Default | Notes |
| --- | --- | --- | --- |
| `description` | no | — | Shown by `jrs task --list`. |
| `run` | one of | — | Argument vector: program, then arguments. No shell involved. |
| `shell` | one of | — | One string, run by `sh -c` (Unix) or `cmd /C` (Windows). |
| `script` | one of | — | A `.java` file, run with the project's JDK in source-launcher mode. |
| `args` | no | `[]` | Extra arguments: appended to `run`, passed to the `script`, and a `shell` string's positional parameters (`$1`…). |
| `depends-on` | no | `[]` | Task names, or `build` / `test` / `package` / `doc`. |
| `env` | no | `{}` | Extra environment variables, name → string. |
| `cwd` | no | project root | Working directory, relative to the root. |
| `inputs` | no | — | Files or directories whose change makes the task stale (§6). |
| `outputs` | no | — | Files or directories the task writes (§6). |
| `source-outputs` | no | `[]` | Directories of generated `.java` files to compile with the main sources (§7). Must be inside `project.target-dir`. |
| `resource-outputs` | no | `[]` | Directories of generated resources, copied like `src/main/resources`. Must be inside `project.target-dir`. |

A task must have **exactly one** action — `run`, `shell` or `script` — or none,
in which case it only aggregates its `depends-on` (like `release` above). A task
with neither an action nor dependencies is a manifest error.

Task names match `[a-z][a-z0-9-]*` and must not be a built-in command name
(`build`, `test`, `run`, `package`, `doc`, `clean`, …), so `depends-on` is
never ambiguous.

### 4.2 The three action kinds

**`run`** is the default recommendation for calling an existing tool. It is
passed to the OS as an argument vector, with no shell parsing, quoting or
globbing, so it behaves the same on Linux, macOS and Windows. The program is
looked up on `PATH` (with the JDK's `bin/` first, §5.2), or taken relative to
the project root if it contains a path separator.

**`shell`** exists because pipes, redirects and `&&` are genuinely useful.
It is **not portable**: the same string goes to `sh` on Unix and `cmd` on
Windows. The docs say so, and `jrs task --list` marks shell tasks with `(sh)`.

**`script`** is the jrs-flavoured answer to "no DSL": build logic written in
Java, run as `java <file> <args…>` using the JDK's single-file source launcher.
It needs no compile step, it runs on the pinned JDK, and it behaves the same on
every OS, because the only thing it needs is the JDK jrs has already found.
It is also what jrs's own integration tests use to exercise tasks portably.

```java
// build/GenerateBuildInfo.java
import java.nio.file.*;

class GenerateBuildInfo {
    public static void main(String[] a) throws Exception {
        var dir = Path.of(a[0], "com/example");
        Files.createDirectories(dir);
        Files.writeString(dir.resolve("BuildInfo.java"),
            "package com.example; public final class BuildInfo {"
            + " public static final String VERSION = \"" + a[1] + "\"; }");
    }
}
```

### 4.3 `[hooks]`

```toml
[hooks]
pre-compile  = ["build-info"]
post-compile = []
pre-test     = []
post-test    = []
post-package = ["checksum"]
pre-run      = []
```

Each value is a list of task names. Hooks name tasks; they cannot hold a
command inline. That keeps one mechanism: anything a hook runs can also be run
on its own with `jrs task <name>`, which is how hooks get debugged.

| Hook | Runs | Available (§5) |
| --- | --- | --- |
| `pre-compile` | After dependencies are resolved, before main sources are globbed and compiled. For code generation. | classpaths |
| `post-compile` | After main classes and resources are in `target/classes`. | classpaths, classes |
| `pre-test` | After the main build, before test sources compile. | + test classpath |
| `post-test` | After the test launcher, only if tests passed. | + test reports |
| `post-package` | After the jar (and any image) is written. | + `jar` |
| `pre-run` | After the build, before the program starts. | classpaths, classes |

Commands include each other the way they do today, so their hooks do too:
`build` fires `pre-compile` and `post-compile`; `test` fires those plus
`pre-test` / `post-test`; `package` fires the build hooks plus `post-package`;
`run` fires the build hooks plus `pre-run`. `jrs doc` fires `pre-compile`,
because it documents the same sources — generated ones included.

Hooks run **every time the lifecycle point is reached**, whether or not `javac`
actually had anything to do. A `post-compile` hook runs on a `Fresh` build too.
Skipping work is the task's own business, through `inputs`/`outputs` (§6);
jrs does not guess from whether compilation happened.

No hook runs for `tree`, `classpath`, `update`, `verify`, `outdated`, `add`,
`remove`, `cache`, `init`, `migrate`, `completions` or `clean`. None of them
builds, and none should execute project code.

### 4.4 Validation

Per SPEC §4.3: errors name the key (`tasks.build-info.run`), unknown keys in a
task table or in `[hooks]` are warnings. At parse time, before anything runs:

- a `depends-on` or hook entry naming an unknown task is an error;
- a `depends-on` cycle is an error that names the cycle (`a → b → a`);
- a task reachable from a hook must not depend on the built-in command that
  fires that hook: `pre-compile = ["x"]` with `x` depending on `build` is a
  cycle through the built-in, reported as one;
- `source-outputs` and `resource-outputs` outside `target-dir` are an error
  (§7 says why); on a task no `pre-compile` or `pre-test` hook reaches they
  are a warning, and ignored;
- `env` may not set names starting with `JRS_` — those are jrs's (§5.2);
- an unknown `{placeholder}` (§5.1) is an error, not an empty string, so typos
  fail at parse time rather than as a baffling tool error;
- a classpath placeholder in a path-valued key (`cwd`, `inputs`, `outputs`,
  `source-outputs`, `resource-outputs`) is an error;
- `{jar}` in a task reachable from any hook other than `post-package` is an
  error, unless the task depends on `package`.

Tasks do **not** feed `manifest-checksum` in `jrs.lock`, because they don't
change resolution. Adding a task does not re-resolve. (§8 is the exception.)

---

## 5. What a task sees

### 5.1 Placeholders

`run`, `args`, `cwd`, `env` values, `inputs`, `outputs` and the `*-outputs`
lists may contain placeholders, expanded by jrs before the process starts.
`shell` strings get the environment variables of §5.2 instead: the shell already
expands `$VAR`, and a second substitution syntax layered over it would quote
badly.

| Placeholder | Value |
| --- | --- |
| `{root}` | Project root (absolute). |
| `{target}` | `project.target-dir` (absolute). |
| `{project.name}`, `{project.version}` | From `[project]`. |
| `{classes}` | `target/classes` (absolute). |
| `{test-classes}` | `target/test-classes` (absolute). |
| `{classpath}` | Exactly what `jrs classpath` prints: `target/classes`, then the compile jars, joined with the platform separator. |
| `{runtime-classpath}` | Exactly what `jrs classpath --runtime` prints (no `compile-only`). |
| `{test-classpath}` | Exactly what `jrs classpath --test` prints. |
| `{classpath-argfile}` | Path to an argfile holding `-cp <compile classpath>`, for `java @{classpath-argfile}`. |
| `{jar}` | The packaged jar. Only valid once `package` has run in the same invocation: in `post-package`, or in a task that depends on `package`. |
| `{{`, `}}` | Literal braces. |

`{classpath-argfile}` exists because of the "argfiles, not command lines" rule:
a few dozen dependencies overflow Windows' 32 K command line, and `{classpath}`
in an argument vector is exactly how you'd hit that. The argfile goes in
`target/.jrs/tasks/<name>.cp.args`.

A placeholder whose value is not available at the point the task runs — `{jar}`
in a `pre-compile` task — is a manifest error when it can be decided
statically: `{jar}` in a task reachable from any hook other than
`post-package`, unless the task depends on `package`. What cannot be decided
statically is checked when the task runs: `jrs task checksum` on its own, with
`checksum` using `{jar}` but not depending on `package`, is a build error
telling the user to add `package` to `depends-on`.

Classpath placeholders are refused in the path-valued keys (`cwd`, `inputs`,
`outputs`, `source-outputs`, `resource-outputs`); a classpath is not a path.

Classpath placeholders trigger dependency resolution if nothing else has: a
standalone `jrs task` that mentions `{classpath}` resolves (lockfile-first, as
always) but does not compile. `shell` tasks get no placeholders, but a `shell`
string that mentions `JRS_CLASSPATH` or `JRS_RUNTIME_CLASSPATH` triggers
resolution the same way.

### 5.2 Environment

Every task inherits jrs's environment, plus:

| Variable | Value |
| --- | --- |
| `JAVA_HOME` | The selected JDK (SPEC §7.1), pin honoured. |
| `PATH` | `$JAVA_HOME/bin` prepended, so `java` in a task is the project's JDK. |
| `JRS_TASK` | The task's name. |
| `JRS_HOOK` | The hook that triggered it, if any. |
| `JRS_ROOT`, `JRS_TARGET_DIR`, `JRS_CLASSES_DIR` | As the placeholders. |
| `JRS_PROJECT_NAME`, `JRS_PROJECT_VERSION` | As the placeholders. |
| `JRS_CLASSPATH`, `JRS_RUNTIME_CLASSPATH` | When dependencies have been resolved. |
| `JRS_JAR` | Once `package` has run in the same invocation, as `{jar}`. |
| `JRS_OFFLINE` | `1` under `--offline`, so a task can honour it. |
| `SOURCE_DATE_EPOCH` | `315532800` (1980-01-01), the timestamp jrs's own jars use. |

`SOURCE_DATE_EPOCH` is the reproducible-builds convention; tools that honour it
(including recent `javadoc` and many generators) produce timestamps that match
jrs's. It is a nudge toward determinism, not a guarantee: a task that writes
the current time into a class breaks byte-identical jars, and that is the
task's doing. The task's own `env` wins over this, so it can be turned off.

---

## 6. Up-to-date checking

A task with **both** `inputs` and `outputs` can be skipped. Without them it
always runs, like a Gradle task that declares no outputs.

Entries in `inputs` and `outputs` are files or directories, relative to the
root; a directory means everything under it, walked with the same sorted
traversal as source globbing (`project::find_all`). **No glob syntax in v1**:
directories cover the common cases, and globbing would mean either a new crate
or another hand-written matcher. If it's needed, it comes later.

The fingerprint follows the pattern of `compile/mod.rs`:

- the expanded action (argv or shell string, `args`, `cwd`, `env`),
- the JDK version, since a `script` task's output can depend on it,
- for every input file: its path, size and modification time (one `stat` each),
- the values of any classpath placeholders it uses, with each jar's size and
  mtime, as the compile fingerprint does for snapshots.

It is written to `target/.jrs/tasks/<name>.fingerprint` only after a successful
run, and deleted on failure. A task is fresh when the fingerprint matches and
every output exists. A fresh task prints `Fresh <name> (task)` and is not run.

Because it lives in `target/`, `jrs clean` forgets it, and the next build runs
every task. `target/` stays fully disposable.

---

## 7. Generated sources and resources

`source-outputs` is how code generation plugs into compilation. After the
`pre-compile` hooks have run, jrs adds every `.java` file under each listed
directory to the main compile unit's source list, alongside `project.source-dir`.
`resource-outputs` directories are synced into `target/classes` with the same
record-keeping as `src/main/resources` (SPEC §7.3).

For a `pre-test` task the same keys feed the **test** compile unit and
`target/test-classes` instead; a task both hooks reach runs once, in
`pre-compile`, and feeds the main unit. "A `pre-compile` task" means any task the
`pre-compile` hook reaches, directly or through `depends-on`; likewise for
`pre-test`. On a task that neither hook reaches, `source-outputs` and
`resource-outputs` are ignored, with a manifest warning.

Why generated directories must live under `target-dir`:

- **`jrs clean` must never lose user data.** Generated code under `target/` is
  regenerable by definition. Allowing it under `src/` invites a task that
  overwrites a hand-edited file.
- **`--watch` must not loop.** Watch mode polls the source trees; a generator
  writing into one would retrigger itself forever. Nothing under `target/` is
  watched.
- **The compile fingerprint already covers it.** Generated files join the
  source list, whose paths and mtimes are part of `is_stale`, so a regenerated
  file triggers a recompile and an unchanged one does not.

A project that *wants* checked-in generated code can run the generator as a
standalone `jrs task` into `src/` and commit the result. That is a different
workflow, and jrs doesn't pretend otherwise.

`jrs build` currently fails with "no .java files" when the source tree is
empty. That check moves to after `pre-compile`, so a project whose sources are
entirely generated still builds.

---

## 8. Tool dependencies

`script` tasks cover build logic you write yourself; `run` covers tools on
`PATH`. The gap was **Java tools from Maven Central** — `google-java-format`,
`protoc-jar`, Flyway, Checkstyle — which would have had to be installed by
hand, defeating the point of a build tool that resolves dependencies.

```toml
[tasks.format]
main = "com.google.googlejavaformat.java.Main"
args = ["--replace", "src/main/java/com/example/App.java"]

[tasks.format.dependencies]
"com.google.googlejavaformat:google-java-format" = "1.22.0"
```

- A fourth action kind, `main = "<class>"`, runs
  `java @target/.jrs/tasks/<name>.tool.args <class> <args>`, the argfile
  holding `-cp` and the tool's classpath, for the reason `{classpath-argfile}`
  exists. `script` tasks may take `dependencies` too, as the same argfile
  ahead of the file. `main` without dependencies, and dependencies on a
  `run` or `shell` task or on an aggregate, are manifest errors.
- `[tasks.<name>.dependencies]` uses the `[dependencies]` value forms (SPEC §4.2),
  without `compile-only`, `runtime-only`, local jars or a version left to
  `[managed]` (SPEC §8.9): each of those means something only in the
  project's graph.
- Each task's dependencies are resolved **as their own graph**, never merged
  into the project's: a formatter's Guava must not mediate against the
  project's Guava. It is `resolve::resolve_tool_dependencies`, the
  `resolve_tool` the compilers go through, over declared dependencies: a
  `manifest::blank` holding them, fetched from the project's repositories.
- They are pinned in `jrs.lock`, since an unpinned build tool isn't
  reproducible either: one `[[tool]]` block per task, named `tasks.<name>`,
  which no compiler's name can be, holding `[[tool.package]]` entries in the
  existing package format. Task dependencies join `manifest-checksum` as
  `task <name> <dependency>` lines, so a manifest without them keeps its
  checksum, and a lockfile without a task's block does not match.
- The jars are downloaded with the rest when the graph is resolved afresh, so
  that `jrs.lock` pins their checksums; from `jrs.lock`, only when the task
  first runs, so `jrs build` never downloads a formatter it does not run.
- The task's fingerprint (§6) covers every tool jar's size and mtime.
- `jrs tree --task <name>` prints a tool's graph; `jrs cache prune` and
  `jrs verify` see what `[[tool]]` blocks name, as they do the compilers'.

This was held back to its own milestone until §3–§7 had been used; it touches
the lockfile format and the resolver's inputs.

---

## 9. Execution

### 9.1 CLI

```
jrs task <name> [-- args...]    run a task (and whatever it depends on)
jrs task --list                 list tasks, their descriptions and hooks
```

- A subcommand, not top-level dynamic commands (`jrs format`). clap's command
  tree stays static, a task can never shadow a future built-in, and
  `jrs completions` stays static. It completes `jrs task` and `--list`, but not
  task names: the completion design says nothing calls back into jrs at
  completion time.
- Arguments after `--` are appended to the argument vector of the named task
  (not its dependencies). For `shell` tasks they become positional parameters
  (`sh -c '<script>' <name> args…` → `$1…`); under `cmd` they are appended to
  the command line.
- `--list` writes to **stdout**, since it is the command's real output, like
  `jrs tree`. Shell tasks are marked `(sh)`.
- `jrs task` works with `--watch`: rerun whenever any task's `inputs`, the
  manifest or the source trees change. Same loop as `build --watch`, which,
  like `test --watch`, now also watches the `inputs` of every task. Nothing
  under `target-dir` is ever watched.

### 9.2 Ordering

- `depends-on` and hooks together form a DAG over tasks plus the built-in
  commands. Ties are broken by **declaration order**, which concretely means:
  `depends-on` entries run in the order the list names them, depth-first,
  each one's dependencies before it; and a hook's tasks run in the order the
  hook lists them. Order is kept because `manifest.rs` already parses a
  `toml::Table` by hand (a `serde` derive would lose it).
- Each task runs **at most once per invocation**, however many paths reach it.
  So does each built-in: `build` in the `depends-on` of a task hooked into
  `jrs test` does not build a second time.
- Tasks run **serially** in v1. Parallel tasks would interleave output, which
  the verbatim-passthrough rule cannot untangle, and the tasks this is for are
  few and short. `--jobs` doesn't apply to them.
- A built-in in `depends-on` runs as that command would, hooks included:
  `depends-on = ["package"]` runs `pre-compile`, `post-compile` and
  `post-package` hooks. It does not print that command's `Finished` line or
  summary box, which belong to the command actually invoked. `test` runs with
  no filters, and `package` builds the plain thin jar.

### 9.3 Output

Everything goes through `ui/`, in line with the layer rules:

- `cli.rs` emits the phase line, unconditionally: `Task <name>` or
  `Task <name> (<hook>)`. A spinner scope may add motion on top, never a line
  of its own (SPEC §12.1 item 4).
- **Tasks run by hooks or as dependencies** have stdin closed, and stdout and
  stderr streamed line by line through `ui.passthrough(Stream::Err, …)`. Both
  go to **stderr**, because stdout belongs to the command's real output. A
  `pre-run` hook must not print into the program's stdout, and must not corrupt
  a `jrs classpath`-style pipe.
- **The task named on `jrs task <name>`** inherits the terminal, like
  `jrs run`: stdin, stdout and stderr are its own, the live region is torn
  down first, so `jrs task repl` or `jrs task print-version | pbcopy` work.
- `--verbose` echoes the expanded command line, the working directory and the
  `JRS_*` environment additions, as it does for `javac`.
- Output is never reformatted (SPEC §6.2). jrs only adds the phase line before
  and, on failure, one error line after.

This needs one new helper in `toolchain.rs`: today's `run_streaming` streams
stdout and collects stderr at the end, and no helper takes an environment or a
working directory. The new one spawns with `env`, `cwd` and `stdin(null)`,
drains stderr on its own thread (as `run_streaming` does, so a full pipe can't
deadlock), and forwards both streams as they arrive.

### 9.4 Failure and exit codes

- A **hook or dependency** that exits non-zero stops the command with
  `JrsError::Build("task `<name>` failed (exit code N)")` → exit `1`. Its
  output has already been passed through; jrs doesn't restate it. Its
  fingerprint is deleted.
- **`jrs task <name>`** returns the task's own exit code, as `jrs run` returns
  the program's. The caveat is the same as `jrs run`'s: a task that exits `2`
  looks like a usage error.
- A program that can't be started (not on `PATH`, not executable) is a
  `Build` error naming it and the `PATH` that was searched.
- `post-test` does not run when tests fail, and there is no `always`/finally
  flag in v1. A hook that must run on failure is an open question (§12).
- Ctrl-C reaches the child through the process group, as it reaches `javac`
  today; the cursor guard restores the terminal as usual.

### 9.5 Trust

Before this, `jrs build` on a freshly cloned repository ran only the JDK —
though annotation processors on the classpath could already run arbitrary
code. With hooks, `jrs build` runs whatever `[hooks]` names, exactly as
`gradle build` or `npm install` does. That is the expected behaviour of a
build tool, but the README should say it plainly, and the list of commands
that **never** run tasks (§4.3) becomes a documented guarantee: inspecting a
project with `jrs tree` or `jrs classpath` is always safe.

---

## 10. INITIAL_SPEC.md changes

Applied along with T1 and T2. §1.2's code-generation bullet was also narrowed
to "built-in code generators", since a generator can now run as a task; §5.1
gained `jrs task <name> --watch` and §5.3.2 the `Fresh <name> (task)` line;
§7.5 notes that task inputs are watched; §12's M7 records T3 as deferred.

1. **§1.2**, replace the first bullet with:
   > - Plugin systems or a build DSL (Groovy/Kotlin/XML). Configuration is
   >   declarative TOML only. User-defined tasks (§7.6) are subprocesses jrs
   >   launches at fixed lifecycle points; they cannot replace, remove or
   >   reorder the built-in phases, and no user code runs inside jrs.
2. **§4.1**, add `[tasks.*]` and `[hooks]` to the full example.
3. **§4.2**, add rows for `tasks.*` and `hooks.*`, pointing at §7.6.
4. **§5.1**, add `jrs task <name> [-- args]` and `jrs task --list`.
5. **§5.3.2**, add `Task` to the phase-line examples.
6. **New §7.6 "Tasks and hooks"**, condensed from §§4–7 and §9 of this
   document.
7. **§6**, add `task.rs` to the module tree.
8. **§12**, a milestone **M7 — Tasks** (below).
9. **§13**, a new open question recording the non-goal decision and its
   reasoning, in the style of the existing entries.
10. **README / ROADMAP**, remove "custom tasks" from anything that implies it
    is out of scope, and state the trust point from §9.5.

No new crates. `toml` already parses the tables, `std::process` spawns, and
`project::find_all` walks input directories.

---

## 11. Implementation plan

### 11.1 Code layout

| Where | What |
| --- | --- |
| `manifest.rs` | `TaskDef`, `Action { Run, Shell, Script }`, `Hooks`; parsed by hand like the rest, order preserved, per-key diagnostics, unknown keys → warnings. Pure structural validation: kinds, names, `target-dir` containment, `JRS_` env names. |
| `task.rs` (new) | Everything that doesn't touch a process or a terminal: the plan (DAG, cycle detection incl. through built-ins, topological order with declaration-order ties), placeholder expansion, placeholder-availability checks per hook, the environment map, fingerprint compute/read/write. Knows nothing about `Ui`. |
| `toolchain.rs` | The spawn helper of §9.3: argv/shell/script → `Command`, env, cwd, streamed passthrough; an inherited variant for the named task. |
| `cli.rs` | `Command::Task(TaskArgs)` (`name`, `list`, `watch`, `args`); `Session::hook(Hook)` called from `build()`, `test_command`, `package_command`, `run_command`, `doc_command`; `task_command`; phase lines; turning a failed task into `JrsError`. `Session::build()` gains the `pre-compile` call between `dependencies()` and source globbing, and the `post-compile` call after the resource sync. |
| `project.rs` | Main/test source lists accept extra roots (the `source-outputs`). |
| `completions.rs` | Nothing by hand. The new subcommand is picked up from the clap definition. |

`task.rs` stays free of `Ui` so the ordering and expansion rules are unit-tested
without a TTY, as `resolve/` is. `ui` keeps depending on nothing.

### 11.2 Milestones

**T1 — Tasks and hooks.** `[tasks]` with `run` / `shell` / `script`, `args`,
`env`, `cwd`, `description`, `depends-on` (tasks and built-ins), `[hooks]`,
placeholders and environment, `jrs task` and `jrs task --list`, serial
execution, streamed output, the INITIAL_SPEC.md changes. Tasks always run.

**T2 — Incremental and generated code.** `inputs` / `outputs` and the
fingerprint, `Fresh` lines, `source-outputs` / `resource-outputs` wired into
the main and test compile units, the "no sources" check moved after
`pre-compile`, `jrs task --watch`, and task inputs added to
`Session::watched_paths`.

**T3 — Tool dependencies.** `main` action, `[tasks.<name>.dependencies]`,
isolated resolution, `[[tool]]` in `jrs.lock`, `jrs tree --task`, prune
awareness. Landed after T1 and T2 had seen use.

### 11.3 Tests

Following CLAUDE.md's test layout:

- **Unit (`manifest.rs`)**: each action kind parses; zero or two actions
  rejected with the key named; unknown task keys warn; built-in names refused
  as task names; `source-outputs` outside `target-dir` rejected; `JRS_` env
  rejected.
- **Unit (`task.rs`)**: `depends-on` and hook entries run in list order; each
  task, and each built-in, once on a diamond; cycle messages name the path; the
  hook-through-built-in cycle; placeholder expansion and `{{`/`}}` escapes;
  unknown and unavailable placeholders rejected; fingerprint changes with an
  input's mtime, the argv, an env value, a snapshot jar's size.
- **`tests/output.rs`**: plain and animated transcripts for a hooked build
  (`Task … (pre-compile)` in the right place), a `Fresh` task, a failing hook
  whose output lands before the error line, the ASCII fallback. Frozen clock
  and fixed width, as today.
- **`tests/build.rs`** (`require_jdk!`): a `pre-compile` `script` task
  generating a class that main code references, which proves ordering and
  `source-outputs` end to end; a second build that skips the fresh task and
  doesn't recompile; `jrs clean` then rebuild; a failing hook → exit 1, no
  fingerprint; `post-package` sees `JRS_JAR`; `jrs task` passes `-- args`
  through; a `pre-run` hook's stdout doesn't reach jrs's stdout. **Every
  test task is a `script`**, so the suite needs nothing but the JDK and runs
  identically on the Linux, macOS and Windows CI legs. `run` is exercised via
  `java` itself; `shell` gets a unix-only test and a Windows-only test, not a
  shared one.
- **Determinism**: two `jrs package` runs with a generating hook still produce
  byte-identical jars, since the generator is itself deterministic.

---

## 12. Open questions

1. **`always` hooks.** Should a task opt into running when the command fails
   (`post-test` after red tests, for a report upload)? Gradle has
   `finalizedBy`. Proposed: not in T1; revisit with a real use.
2. **Skipping hooks.** A `--no-hooks` flag, or Gradle's `-x <task>`? Handy for
   debugging, but a build that skips its code generator won't compile, so it
   mostly produces confusing errors. Proposed: no, use `jrs task` to run pieces
   in isolation instead.
3. **Parallel tasks.** Independent tasks could run concurrently under `--jobs`
   with output buffered per task and replayed whole. Proposed: only if serial
   execution shows up in a benchmark.
4. **Glob inputs.** `src/**/*.proto` instead of `src/main/proto`. Needs a
   matcher. Proposed: wait for a case directories can't express.
5. **Migration.** Gradle tasks are translated where they are literal
   (`migrate/gradle_tasks.rs`, SPEC §11.3): `Exec` → `run`, `JavaExec` over
   the main runtime classpath → `java @{classpath-argfile} <main>` depending
   on `build`, `dependsOn`-only tasks → aggregates, and `dependsOn` /
   `finalizedBy` on `compileJava`, `test`, `jar` and `run` → hooks. The rest
   is listed as "not migrated", task by task. Maven's `exec-maven-plugin`
   executions are translated too (`migrate/maven.rs`): `exec` becomes a
   `run` task, `java` a `java @{classpath-argfile} <main>` one, or a `main`
   task over the plugin's own `<dependencies>` when `includePluginDependencies`
   asks for them (§8), and the `<phase>` the hook that fires at the same
   point — `generate-sources` and the phases before compilation
   `pre-compile`, `compile` `post-compile`, the test-compilation phases
   `pre-test`, `test` `post-test`, `package` `post-package`. An execution in
   another phase, or with `<async>` or `<outputFile>`, is reported whole.
   `maven-antrun-plugin` still is.
6. **Lockfile compatibility for T3.** An older jrs ignores `[[tool]]` blocks
   and would drop them when it rewrites the lockfile. **Settled:** a task's
   graph is a `[[tool]]` block like a compiler's, and a lockfile says
   `version = 2` only when it has one, which a jrs from before tools refuses
   rather than rewrite. Projects without tools keep a byte-identical
   lockfile.
