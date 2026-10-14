# jrs — Roadmap

Every milestone in [SPEC §12](SPEC.md#12-roadmap) has landed, and so have
workspaces and the IntelliJ plugin. What jrs lacks now is not features but
evidence: that it builds the projects people bring to it, and that it is
faster than Gradle where it says it is. This roadmap is four items, in order.
Each has a spec in [`specs/todo/`](specs/todo/) that says what to build and
when it counts as done.

The roadmap before this one, with the positioning ("Where jrs is aimed"), the
IntelliJ plugin's open items and the table of ideas that need a spec decision
first, is kept in [specs/ROADMAP_ARCHIVE.md](specs/ROADMAP_ARCHIVE.md). Its
open items are still open; they are just not next.

**Order of work.** Section 1 comes first because a user whose first project
does not migrate never sees anything else. Section 2 runs on the projects
section 1 makes build, and its numbers decide which parts of section 3 are
worth their code. Section 4 needs little code and can go alongside.

**What waits.** New packaging formats, more JVM languages, a Build Server
Protocol server and any daemon. Each adds surface nobody has asked for yet,
and none of it helps the comparison with Gradle until sections 1 and 2 are
done.

---

## 1. Hardening against real projects

[specs/todo/CORPUS.md](specs/todo/CORPUS.md)

A pinned corpus of open-source Spring Boot and Ktor projects, on Maven and
on Gradle, and a harness that takes each through `jrs migrate`, `build`,
`test`, `run` and `package` and compares the result with the project's own
build: the same runtime classpath, the same tests run and passed. Every
failure is fixed with a regression test, added as a migration rule, or cited
as a non-goal. The measure is how often `jrs migrate` works on the first
try, published as a compatibility table in `DOCS.md` and on the website.

The harness, the list and the ledger have landed (`cargo bench --bench
corpus`, [`corpus/`](corpus/)). The first run took 4 of 11 projects end to
end; the other seven stop at `build` on six findings, five of them migration
gaps ([corpus/FINDINGS.md](corpus/FINDINGS.md)).

## 2. Benchmarks against Maven and Gradle

[specs/todo/BENCHMARKS.md](specs/todo/BENCHMARKS.md)

The same projects built by jrs, Maven and Gradle (warm daemon and
`--no-daemon`), checked to be the same build before anything is timed:
clean builds with a cold and a warm cache, a no-op build, a one-file
incremental build, `test`, `run` and `package`. Medians and spread, from a
local mirror, in a generated `benchmarks/REPORT.md` that says where jrs is
slower. The Why page and the README link to it once it exists.

## 3. Closing the gaps where Gradle wins

[specs/todo/GRADLE_GAPS.md](specs/todo/GRADLE_GAPS.md)

Four gaps, each judged by the section 2 numbers before it gets code:

- **Workspace members built in parallel**, with each member's output
  buffered and printed in build order, as Gradle's `--parallel` does.
- **Kotlin compile cost**: measuring what `kotlinc`'s JVM start-up costs a
  Ktor build, and a `kotlinc` worker for `--watch` if it is felt.
- **JDK provisioning**: a pinned JDK that is not installed is downloaded,
  as Gradle's toolchains do, unpacked by the system `tar` so no crate is
  added.
- **Pinning jrs itself**: what Gradle's wrapper gives a team, from
  `project.jrs-version` and `setup-jrs`.

## 4. Reaching more users

[specs/todo/REACH.md](specs/todo/REACH.md)

Cheap work that lowers the cost of trying jrs:

- **VS Code without a daemon**: Eclipse `.project` and `.classpath` files
  generated from the resolved project, which the Java extension's language
  server reads as they are.
- **Package managers and the JetBrains Marketplace**: the Homebrew tap, the
  Scoop bucket and winget, from the manifests each release already writes,
  and the IntelliJ plugin published.
- **A guide for coding agents** in `DOCS.md` and on the website.
- **`jrs install`** into `~/.m2`, so a library can be shared between
  projects in different repositories: the smallest form of publishing, and
  a SPEC §1.2 decision.
