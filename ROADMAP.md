# jrs — Roadmap

Every milestone in [SPEC §12](SPEC.md#12-roadmap) has landed, and so have
workspaces, the IntelliJ plugin, the corpus, the benchmarks against Maven
and Gradle and the gaps they showed. What is left is the last few projects
the corpus cannot take end to end, the cells where Gradle still wins, the
release steps still done by hand and the cost of lowering the bar to try
jrs. This roadmap is five items, in order.

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

- ⬜ **`clean` on `gen-2000`**: 1.79 s against 1.50–1.56 s, 1,368 ms of it
  `javac`. Measure what the `javac` invocation's JVM flags and splitting
  the sources into parallel batches would save.
- ⬜ **`abi` on `gen-2000` and `gen-ws`**: 779 ms against 631–640 ms. Count
  how many sources a round of "compile its users" recompiles against how
  many a change actually reaches.
- ⬜ **`test` on `gen-2000`**: 911 ms against 779–825 ms, 682 ms of it the
  test JVM. A full run (seven measured runs per cell) first, to tell a
  regression from a one-off.
- ⬜ **Corpus numbers for the parallel members and the `kotlinc` worker**,
  still to be taken ([GRADLE_GAPS.md §9](specs/GRADLE_GAPS.md#9-what-was-built)).

Nothing here gets code until a measurement says where the time goes.

**Status: ⬜ to do.**

## 3. Releases without manual steps

[CLAUDE.md](CLAUDE.md), [specs/HOMEBREW.md](specs/HOMEBREW.md)

A `v*` tag builds and publishes everything, but two steps are still by hand:

- ⬜ **The Homebrew tap updated by the release**: `release` commits
  `Formula/jrs.rb` to `pwittchen/homebrew-tap` from the checksums it
  already has, instead of copying it over by hand.
- ⬜ **The IntelliJ plugin's version from the tag**: `bump` writes
  `pluginVersion` in `plugins/intellij/gradle.properties` too, or the reason
  it stays independent is written down.

**Status: ⬜ to do.**

## 4. Reaching more users

[specs/todo/REACH.md](specs/todo/REACH.md)

Cheap work that lowers the cost of trying jrs:

- ⬜ **Scoop and winget**, from the manifests each release already writes.
- ✅ **The IntelliJ plugin on the JetBrains Marketplace.**
- ⬜ **A guide for coding agents** in `DOCS.md` and on the website.
- ⬜ **`jrs install`** into `~/.m2`, so a library can be shared between
  projects in different repositories: the smallest form of publishing, and
  a SPEC §1.2 decision.

**Status: ⬜ to do.**

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
