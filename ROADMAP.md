# jrs — Roadmap

Every milestone in [SPEC §12](SPEC.md#12-roadmap) has landed, and so have
workspaces, the IntelliJ plugin, the corpus, the benchmarks against Maven
and Gradle and the gaps they showed. What is left is the last few projects
the corpus cannot take end to end, the cells where Gradle still wins and the
cost of lowering the bar to try jrs. This roadmap is five items, in order.

The roadmap before this one, with the positioning ("Where jrs is aimed"), the
IntelliJ plugin's open items and the table of ideas that need a spec decision
first, is kept in [specs/ROADMAP_ARCHIVE.md](specs/ROADMAP_ARCHIVE.md). Its
open items are still open; they are just not next.

**Legend.** ✅ done · ⬜ to do

**Order of work.** Section 1 comes first because a user whose first project
does not migrate never sees anything else. Section 2 needs a measurement
before it gets code. Sections 3 and 4 need little code and can go alongside;
section 5 changes no behaviour and fits between the others.

**What waits.** New packaging formats, more JVM languages, a Build Server
Protocol server and any daemon. Each adds surface nobody has asked for yet.

**What is not planned.** Editor integration beyond the IntelliJ plugin:
no Eclipse `.project`/`.classpath` files, no VS Code support.

---

## 1. The rest of the corpus

[specs/corpus/FINDINGS.md](specs/corpus/FINDINGS.md)

10 of 11 corpus projects get past `build`, but six stop short of `ok` on
findings 12–19. Three of them are gaps jrs can close without crossing a
non-goal, and one more can at least be reported:

- ✅ **Finding 12**: read `maven-compiler-plugin`'s `<execution>`
  configuration, as finding 6 did for Checkstyle.
- ✅ **Finding 13**: apply a BOM's exclusions to the managed dependency they
  are declared on. Today the classpath differs without an error.
- ✅ **Finding 17**: a versionless `org.jetbrains.kotlin` library takes the
  Kotlin plugin's version on migration.
- ✅ **Finding 15**: the migration report names the dependencies where
  nearest-wins and Gradle's highest-wins pick different versions. Mediation
  itself stays nearest-wins (SPEC §13.6).

Each fix lands with a regression test in the fixture repo or a migration
fixture, and the compatibility table in `DOCS.md` is updated from the next
corpus run.

**Status: the four fixes have landed; the corpus run that confirms them and
updates the compatibility table is still to do.**

## 2. Faster on large projects

[specs/BENCHMARKS_SUMMARY.md](specs/BENCHMARKS_SUMMARY.md)

The 0.14.7 pass leaves Gradle's warm daemon ahead in a handful of cells,
all on the large project or the workspace:

- ✅ **`clean` on `gen-2000`**: 1.79 s against 1.50–1.56 s, 1,368 ms of it
  `javac`. Splitting the sources and the JVM flags were already measured in
  [BEATING_THE_DAEMON_4.md §5](specs/BEATING_THE_DAEMON_4.md#5-a-large-compile-in-a-cold-jit-)
  and do not pay, so `javac` itself stays as it is. A clean tree's sources,
  and the jars' processor check, are now read in parallel, which only reaches
  the part outside `javac`. The 0.16.0 pass has it at 1.55 s, ahead of Gradle
  CC's 1.60 s and 90 ms behind the plain daemon, with `javac` at 1,334 ms: what
  is left is the JIT warming up.
- ✅ **`abi` on `gen-2000` and `gen-ws`**: 779 ms against 631–640 ms. A round
  compiled every user of a class that gained a method. When all it gained is
  methods with a body, it now compiles the sources with a subtype of it and
  those whose constant pools hold a new name (`compile/incremental.rs`): a
  source that names none of them compiles to the same bytes. `-v` prints the
  count the roadmap asked for, `<n> of <m> sources that refer to what changed
  can feel it`. The 0.16.0 pass has it at 333 ms against 640–658 ms on
  `gen-2000` and 274 ms against 430–499 ms on `gen-ws`, ahead on all five
  projects. The benchmark's edit is a method nothing calls, its best case.
- ✅ **`test` on `gen-2000`**: 911 ms against 779–825 ms, 682 ms of it the
  test JVM. A full run (seven measured runs per cell) is still what tells a
  regression from a one-off. Outside the JVM, the main classes' API digest
  (55–64 ms) is kept under the classes' sizes and times
  (`compile::api_digest_remembered`), an up-to-date check no longer stats
  every class, and the first hash of the class directories runs in parallel.
  The 0.16.0 pass has it at 730 ms, ahead of 860–869 ms, but `test-edit` went
  the other way, to 1.19 s against 1.06–1.16 s; a full run after the 0.16.0
  release says which side each is on.
- ✅ **Corpus numbers for the parallel members and the `kotlinc` worker**,
  to be taken after the 0.16.0 release
  ([GRADLE_GAPS.md §9](specs/GRADLE_GAPS.md#9-what-was-built)).

**Status: ✅ done; the full run and the corpus run follow the 0.16.0
release.**

## 3. Releases without manual steps

[CLAUDE.md](CLAUDE.md), [specs/HOMEBREW.md](specs/HOMEBREW.md)

A `v*` tag builds and publishes everything, and two steps were still by hand:

- ✅ **The Homebrew tap updated by the release**: the `homebrew` job, after
  `release`, dispatches the tap's Update formulae workflow, which commits
  `Formula/jrs.rb` to `pwittchen/homebrew-tap` after checking it against the
  release's checksums. It needs the `HOMEBREW_TAP_TOKEN` secret.
- ✅ **The IntelliJ plugin's version**: it stays independent of the tag, and
  [its README](plugins/intellij/README.md) says why.

**Status: ✅ done.**

## 4. Reaching more users

[specs/REACH.md](specs/REACH.md)

Cheap work that lowers the cost of trying jrs:

- ✅ **The IntelliJ plugin on the JetBrains Marketplace.**
- ✅ **A guide for coding agents** in `DOCS.md` and on the website.
- ✅ **`jrs install`** into `~/.m2`, so a library can be shared between
  projects in different repositories: the smallest form of publishing, and
  a SPEC §1.2 decision.

Scoop and winget were dropped: `brew` and `install.sh` stay the ways to
install jrs, and on Windows the release's zip.

**Status: ✅ done.**

## 5. Splitting the two largest modules

[ARCH.md](ARCH.md)

`cli.rs` was over 7,500 lines and `manifest.rs` over 5,800. A change to one
command or one table meant working in a file that held all of them:

- ✅ **`cli.rs`**: `Session` and its `build()` spine in `cli/session.rs`, and
  the commands in `cli/` by group, beside `cli/members.rs`: `run`, `testing`,
  `packaging`, `doc`, `deps`, `tasks`, `watch`, `add`, `init`, `migration`,
  `cache`, `jdk` and `setup`. `cli.rs` keeps the clap definitions and
  dispatch, under 1,300 lines.
- ✅ **`manifest.rs`**: now `manifest/`, the types in `mod.rs`, table parsing
  in `parse.rs` apart from validation in `validate.rs` and from
  `effective_dependencies()` in `dependencies.rs`; rendering and templates in
  modules of their own, and the tests in `tests.rs`.

A refactor only: no behaviour, output or lockfile changes, with the
integration suites as the check. `ARCH.md` follows the new layout.

**Status: ✅ done.**
