# jrs — Workspaces

Design for building a project of several modules — a `core` library, an
`api`, an application on top — from one root, with one lockfile.

Status: **implemented**, as section 7 of [ROADMAP_ARCHIVE.md](ROADMAP_ARCHIVE.md). It
moves a line drawn in [SPEC §1.2](../SPEC.md#12-non-goals) ("Multi-module /
aggregator builds (v1 is one module per manifest)") and reverses SPEC open
question 11, so it needed an SPEC.md change before code; §2 is the argument,
and §10 lists the SPEC.md edits, which have been made. The code cites this
document by section: `workspace.rs` (§3–§5), `cli/members.rs` (§6),
`model.rs` (§7) and `migrate/workspace.rs` (§8).

---

## 1. Problem

Most Spring Boot and Ktor codebases that outgrow one module split into
modules before they split into repositories: a `domain` or `core` library, an
`api` module, one or two applications on top. Until now jrs stopped at the
first `<modules>` or `include`. `jrs migrate` translated the module it was
pointed at and listed the others, and the user was left to wire them together
through `~/.m2` by hand: install `core`, point `app` at a `file://` repository,
remember to reinstall after every change. That was the most common reason a
real project could not try jrs at all.

What such a build needs is small next to what jrs already has. Each module is
already a single-module project jrs can build: its own sources, its own
dependencies, its own jar. What is missing is a way to say that one module's
classes go on another's classpath, a way to say the shared settings once, and
a guarantee that the modules agree on the versions of what they share.

## 2. Why this is not a reactor

Maven's reactor and Gradle's multi-project builds configure modules from the
outside: a parent POM's plugins run in every child, `subprojects { }` is code
that reaches into every project, and a module's build can depend on what
another module's script did. That is the scripted configuration phase jrs was
built without (SPEC §1.2), and it is not needed to build several modules.

The shape here is Cargo's workspace. Modules are **declared**, not
configured:

- A root `jrs.toml` lists the members by directory. It adds nothing a member
  manifest could not say for itself: the keys it holds are defaults that each
  member inherits and can override, key by key.
- Each member is an ordinary manifest. Read on its own, with the root's keys
  merged in, it describes one module exactly as a single-module manifest does.
- A member depends on another by path, as it would depend on a jar. Nothing a
  member writes changes how another member is built.

So the non-goal narrows rather than goes away: one manifest is still one
module, and a workspace is several manifests that share defaults and a
lockfile. A member configuring another member, build logic at the root,
per-module lockfiles, and composite builds across repositories all stay out
(§9).

## 3. The workspace manifest

A workspace's root is a `jrs.toml` with a `[workspace]` table:

```toml
[workspace]
members = ["core", "api", "app"]

[java]
source = 21

[managed]
"org.springframework.boot:spring-boot-dependencies" = { version = "3.5.0", bom = true }

[repositories]
internal = "https://nexus.example.com/repository/maven-public/"
```

- **`members`** lists the members' directories, relative to the root,
  `/`-separated. A member may sit deeper (`libs/core`); it may not be outside
  the root, and the root is never listed. The order is the declaration order
  used for ties (§5.1, §6.1); the build order comes from the dependencies.
- **A root without `[project]` is a pure aggregator**: it has no sources and
  builds nothing itself. A root with `[project]` is a member too, the first
  one, and its own `[dependencies]`, `[tasks]` and the rest apply to it as to
  any member.
- **Shared keys.** `[java]`, `[repositories]` and `[managed]` are inherited by
  every member, key by key, a member's own key winning. `[kotlin]`, `[scala]`
  and `[groovy]` are inherited only by a member that has the table itself:
  whether a member is a Kotlin module is its own business, but the version and
  flags can be said once (`[kotlin]` in the member with nothing under it takes
  the root's `version`). A key in a pure aggregator's root outside these
  tables applies to nothing, and is a warning.
- A member's inherited `[repositories]` and `[managed]` entries come after its
  own, so its own BOM wins a tie, as the nearer import does in Maven.
- A member cannot have a `[workspace]` of its own, two members cannot share a
  `project.name`, and every listed directory must hold a `jrs.toml`.

Inheritance happens on the TOML tables, before a member's manifest is parsed
(`Manifest::from_table`), so every check the parser makes — versionless
dependencies against `[managed]`, a language's version — sees the merged
manifest.

**Finding the workspace.** A command run at the root acts on the workspace.
Run from inside a member's directory (or with `--manifest-path` pointing at
one), jrs walks up from the member until it finds a `jrs.toml` with
`[workspace]` that lists the directory; the member is then the workspace's,
and the command is narrowed to it (§6.1). A `jrs.toml` above that does not
list the directory is not its workspace, and the manifest stands alone.

## 4. Module dependencies

A member depends on another by its directory:

```toml
[dependencies]
core = { path = "../core" }
api = { path = "../api", compile-only = true }

[dev-dependencies]
testing = { path = "../testing" }
```

- The key is the member's `project.name`; the `path` is its directory,
  relative to this manifest's. A `path` that ends in `.jar` is still a local
  jar (SPEC §8.8), so the two cannot be confused.
- `compile-only` and `runtime-only` mean what they mean for a jar. A member
  named in `[dev-dependencies]` is on the test classpath only.
- A path that names no member, or a member by another name, is an error
  naming both; so is a member that depends on itself, and a cycle, which is
  named as the path it is: `the workspace's members depend on each other in a
  cycle: api → core → api`.
- Outside a workspace, a path dependency on a directory is an error: there is
  nothing to build the module from.

### 4.1 Where a member's classes go

A member's `target/classes` goes on a dependent's classpaths **where its jar
would**: on the main compile classpath for a plain or `compile-only`
dependency, on the runtime classpath (`jrs run`, packaging, `{classpath}`
placeholders) for a plain or `runtime-only` one, and on the test classpath for
all of them. Class directories come ahead of the jars, nearest member first.

### 4.2 What a member's own dependencies bring

A member's own dependencies join a dependent's graph **as a jar's transitive
ones do**. With `via` the classpath the dependent reaches the member on, and
`declared` how the member declares its dependency:

| `declared` → | plain | `runtime-only` | `compile-only` | dev |
| --- | --- | --- | --- | --- |
| `via` plain | plain | runtime | — | — |
| `via` runtime | runtime | runtime | — | — |
| `via` compile-only | compile-only | compile-only | — | — |
| `via` dev | dev | dev | — | — |

The same rule carries a member's own module dependencies, so `app → api →
core` puts `core`'s classes on `app`'s classpath too (`workspace::carried`).

## 5. One lockfile

### 5.1 The workspace's graph

The workspace has **one** `jrs.lock`, at the root. It is the resolution of
one manifest, the *aggregate* (`Workspace::aggregate`): every member's
dependencies, members in declaration order and each member's in its own,
dev-dependencies after them; the union of the members' `[managed]` and
`[repositories]`; and each compiler, task graph, obfuscator and pinned agent
the members ask for. Nearest-wins mediation then runs **once** over the whole
workspace, so two members can never link against two versions of one library.

The walk differs from a single project's in one way
(`resolve::resolve_workspace`). A package's children are walked with the
exclusions of the path that first reached it; when another path reaches it
excluding less, the children only that path keeps are walked too. Otherwise
one member's exclusion would keep a jar out of the graph, and so off the
classpath of a member that reaches the same package another way and excludes
nothing.

A coordinate two members declare at two versions is taken at the first one's,
with a warning naming both. One workspace pins one compiler per language and
one `ProGuard`: two members asking for two versions is an error that says to
set the version once in the root. A task's own graph is pinned under
`<member>/<task>`, so two members' `check` tasks stay apart; a local jar is
named and placed relative to the root (`core/libs/x.jar`), so two members'
`driver`s are two jars.

### 5.2 A member's share

A member does not resolve. Its graph is its **share** of the workspace's
(`Workspace::project`): the packages its own dependencies, and its upstream
members' (§4.2), reach along the resolved graph's edges, each with the
classpath this member puts it on, its own declarations at depth 1 and its
upstream members' at depth 2. Exclusions are the member's own: the aggregate
keeps an exclusion only when every member that declares the coordinate makes
it, and each share applies its own while it walks.

### 5.3 The file

A workspace's lockfile says `version = 3` and lists the members' directories
after `manifest-checksum`:

```toml
version = 3
manifest-checksum = "sha256:…"
members = ["core", "api", "app"]
```

Everything else is the format SPEC §4.4 describes; `[[tool]]` blocks do not
take it back to 2. `manifest-checksum` is the aggregate's, so a change to any
member's dependencies re-resolves. A member never has a `jrs.lock` of its
own, and a single-module project keeps its lockfile byte for byte.

## 6. Commands

### 6.1 Which members a command acts on

| Run | Acts on |
| --- | --- |
| at the root | every member |
| with `-p <member>` (`--module`, repeatable) | the members named |
| from inside a member's directory | that member |

and, for the commands that build, **the members those depend on**, built
first. In detail:

| Command | In a workspace |
| --- | --- |
| `build` | the chosen members and their upstream members, in build order |
| `test` | builds the same, and tests the chosen members; a member whose tests fail does not stop the others, and the command fails at the end naming them; `--fail-fast` stops at the first |
| `run` | the chosen member, or the only member with a main class; with several, `-p` says which |
| `package` | one jar per member, chosen and upstream. At the root, a library member gets its plain jar where the flags that need a main class (`--fat`, `--dist`, `--jlink`, `--jpackage`, `--native-image`) ask the applications for more, and the portable layout where they get a fat jar (§6.4) |
| `doc` | the chosen members, after building their upstream members |
| `task <name>` | every chosen member that declares it; `--list` lists each member's tasks under its name |
| `clean` | the chosen members' target directories |
| `tree` | each chosen member's share of the graph, its module dependencies first; `--why` looks in each; `--tool` reads the workspace's |
| `classpath` | one member: `-p`, the member it is run from, or the only one |
| `add`, `remove` | one member: `-p`, the member it is run from, or the root when it is a member; at a pure aggregator's root, without `-p`, it asks which |
| `update`, `verify`, `fetch`, `outdated`, `licenses`, `audit` | the workspace's graph |
| `metadata` | §7 |

The `--watch` forms watch the root manifest and every chosen member's and
upstream member's watched paths, and start over with the workspace as it is
after a change.

### 6.2 Building the graph

Members build **in dependency order**, each after every member it depends on,
ties in declaration order (`Workspace::order`). Each member is a `Session` of
its own, given its share of the graph (§5.2) and its upstream members; a
member is built at most once per command, however many others depend on it.

**Compile avoidance carries across the module boundary.** A unit's
fingerprint holds `compile::api_digest` of every upstream member's classes it
compiles against, beside its jars, the way the test unit already holds the
main classes'. A body change in `core` recompiles `core` alone; a change to
what another unit's `javac` can see recompiles the members that see it.
`jrs test`'s selection (`compile/impact.rs`) hashes every class directory on
the test classpath, upstream members' included, so it follows a change into
the tests that reach it. The build cache keys paths against the workspace's
root, so an upstream member's classes are `{root}/core/target/classes` on
every machine.

**Phase lines name their module.** `Compiling core v1.0.0`, `Fresh app
v1.0.0` already did; the lines that did not — the tests' `Compiling` and
`Testing`, `Task`, `Fresh (task)` — are prefixed with the member's name in a
workspace.

**Members build at once** ([GRADLE_GAPS.md §2](todo/GRADLE_GAPS.md#2-a--workspace-members-in-parallel)).
A member is ready when every member it depends on has built; ready members
start in build order, up to `--jobs` at once, each on a thread of its own
with a session of its own. The graph and every jar are fetched before they
fan out, so two members never race for a download. The unit of work is a
member's build and what the command then does with it — `package`'s jar
included — so the transcript is the same at any `--jobs`:

> The earliest unfinished member in build order writes to the terminal;
> every other member writes to a buffer (`Ui::deferred`). When the writing
> member finishes, the next member in order is released: its buffer is
> replayed and it writes straight through from then on.

While members build at once, the live region is one spinner naming those
running, `Compiling core, api (2 of 5 modules)`; a member's own spinners and
bars stay down. When a member fails, no other starts, the running ones
finish, every member that finished is released in build order, and the
command fails with the first failure in build order. `--timings` gains a
`member <name>` row per member built at once.

What stays one member at a time: `run` (one member), `--watch` (its one
`javac` worker would queue them), `--jobs 1`, and any workspace where a
member's hook or task depends on another member's task (§6.3), since that
task runs in the other member's session, which lives on another thread —
`-v` says why. Tests run member by member too: a test JVM already forks per
core (`test.forks`), and two suites at once would fight over cores and
ports. `jrs test` builds every member first, at once, then tests them in
build order; at `--jobs 1` each member builds and tests in turn, as before.

### 6.3 Tasks and hooks

`[tasks]` and `[hooks]` stay per member: a member's tasks see its own
classpaths, its own `target/`, its own placeholders. A task can depend on
another member's task with `"<member>:<task>"` in `depends-on`:

```toml
[tasks.check]
script = "Check.java"
depends-on = ["core:stamp"]
```

The other member's task runs in that member's session, once per command,
before the command reaches the member whose task needs it. The workspace
checks, when it loads, that every such reference names a member and one of
its tasks, and that no task reaches itself through other members' tasks.
Outside a workspace, a `"<member>:<task>"` reference is an error.

### 6.4 Packaging

A member's jar is its own classes, as a single module's is. What its upstream
members add depends on the layout:

- **thin** — each upstream member that runs with it is a jar on the
  `Class-Path`, written fresh from its classes into its own
  `target/.jrs/module/<name>-<version>.jar`.
- **portable** — those jars are copied into `lib/` beside the dependencies'.
- **fat** — those jars' entries are written into the fat jar under the same
  merge and relocation rules as any dependency's; the member's own classes
  still win.

A thin jar's `Class-Path` names absolute paths, as it always has, so it is
not reproducible across checkouts; that is why a library under a root
`--fat` gets the portable layout rather than a thin jar.

## 7. Tooling

`jrs metadata` at a workspace's root prints the **workspace model**:

```
{
  "version": 1,
  "jrs": "…",
  "workspace": { "name": "shop", "root": "/work/shop",
                 "manifest": "/work/shop/jrs.toml", "lockfile": "/work/shop/jrs.lock" },
  "members": [ { …one member's model… }, … ]
}
```

Each member's model is the single-project model of SPEC §5.4, its
`project.lockfile` the root's, with two keys after `tasks`: `workspace`, the
root's directory, and `modules`, the members it depends on, nearest first,
each with its `name`, `version`, `root`, `output` (its classes directory),
`classpath` (`compile`, `provided`, `runtime` or `test`) and whether it is
`direct`. With `-p` or from inside a member, `jrs metadata` prints that
member's model alone. A single project's model is unchanged, byte for byte.

The IntelliJ plugin reads either: it sets up one IntelliJ module per member,
each depending on the members in its `modules` with the scope their
classpath implies. Opened at a member's directory, it reads that member's
`workspace` and imports the whole workspace, so the members it depends on are
modules too.

## 8. Migration

`jrs migrate` on a multi-module build writes the root manifest and one
manifest per module, each module translated by the single-module readers as
before, with what the build shares in reach.

### 8.1 Maven

- An aggregator's `<modules>` are walked recursively. A `jar` module is a
  member; a `pom` module with modules of its own is walked in turn; a `pom`
  module without (a BOM, a parent) is reported and left out. A root POM
  that is not `pom`-packaged is a member too.
- Each member is read with its parent chain, as a single module is, and with
  Maven's plugin inheritance: a plugin an ancestor declares is the member's
  too (unless `<inherited>false</inherited>`), and a plugin the member
  declares takes the `<configuration>` and `<executions>` the nearest
  `<pluginManagement>` gives it for what it does not set itself.
- A `<dependency>` on a sibling's own `groupId:artifactId` becomes a path
  dependency, in the same scope; one on a sibling's `test-jar` has no
  counterpart and gets a review line.
- The root's own `<dependencyManagement>` versions that the members use go in
  the root's `[managed]`, and the members' declarations lose their versions to
  it; the parent's compiler settings and repositories, being the same in every
  member, move into the root (§8.3).
- A module whose `<parent>` is not the root POM, and a module listed only in a
  profile's `<modules>`, each get a review line.

### 8.2 Gradle

- `settings.gradle(.kts)`'s `include(...)` gives the members (`:libs:util` is
  `libs/util`), and `rootProject.name` names the root. The root project is a
  member when it has a `src/` of its own.
- The root script's `subprojects { }` and `allprojects { }` bodies are read
  into each member's script, after its own, so its own settings come first.
  The parts that are plain declarations — a Java toolchain, a repository, a
  dependency — are translated as they would be in the member; whatever is
  the same in every member then moves into the root (§8.3), and the rest is
  reported per member, as an unknown plugin is.
- `project(":core")` and `projects.core` dependencies become path
  dependencies, in the configuration's scope.
- A version catalog at the root becomes shared `[managed]` entries: each
  library a member uses goes in the root's `[managed]`, and the members'
  declarations lose their versions to it.
- `includeBuild` stays a composite build (§9).

### 8.3 The root and the report

After the members are translated, what every member has in common moves into
the root: `[java]` when it is the same in every member, `[repositories]` when
they are, and the common tail of `[managed]`, since an inherited entry comes
after a member's own (§3). Member entries the root now holds are dropped.

The report gains a section per member after the workspace's own, and its last
line says how many members migrated without anything to review. Fixtures:
`tests/fixtures/migrate/maven-workspace` (an aggregator with a parent, a
nested aggregator, a BOM module, a profile's module, plugin inheritance) and
`gradle-workspace` (`include`, a version catalog, a `subprojects { }` block).

## 9. What stays out

- **A member configuring another member**, and **build logic at the root**
  beyond the shared tables. Each would bring back the scripted configuration
  phase.
- **Per-module lockfiles.** One lockfile is what makes the versions agree.
- **Composite builds across repositories** (Gradle's `includeBuild`): a
  workspace's members live under its root.
- **Members tested in parallel.** Members build at once (§6.2), but their
  tests run one member at a time: two suites at once would need a fork
  budget shared across members, and would still fight over the ports and
  databases both use. To measure with the `test JVM` rows per member first
  ([GRADLE_GAPS.md §7](todo/GRADLE_GAPS.md#7-open-questions)).

## 10. SPEC.md edits

- §1.1 and §1.2: a project is one module or a workspace of several; the
  non-goal is narrowed to what §9 keeps out.
- §4.4: the `version = 3` workspace lockfile.
- §5.1, §5.2: the commands in a workspace (§6.1), `-p, --module`.
- §5.4: the workspace model (§7).
- §11.2, §11.3: `<modules>` and `include` are migrated (§8).
- §12: milestone M10.
- §13, question 11: the decision reversed.
