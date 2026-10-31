# jrs — Reaching more users

Three pieces of work that each make jrs cheaper to try or to keep, and none
of which changes how a build works: jrs in Homebrew and the plugin on the
JetBrains Marketplace; a guide for coding agents; and `jrs install`, which
puts a jar and a POM into `~/.m2` so another project can depend on it.

Status: **done**, as section 8.4 of [ROADMAP_ARCHIVE.md](ROADMAP_ARCHIVE.md). One part
crossed a line drawn in [SPEC §1.2](../SPEC.md#12-non-goals): §4 ("Publishing
artifacts to a repository"), which argues for the narrowing it needed. Scoop
and winget were part of this plan and were dropped: `packaging/manifests.sh`
writes the Homebrew formula alone.

Editor support beyond the IntelliJ plugin — Eclipse project files for
VS Code and the other JDT language server clients — is not planned.

---

## 1. Problem

The audiences [ROADMAP_ARCHIVE.md](ROADMAP_ARCHIVE.md) names as the
best fit for jrs (services, command-line tools, teams for whom the JVM is a
guest language, coding agents) meet three obstacles before they meet the
build itself:

1. **Installing jrs takes a `curl | sh`.** That is fine on a laptop and a
   reason to say no in many companies. `brew install` is what people
   expect, and the release job writes the formula
   (`packaging/manifests.sh`).
2. **An agent has to discover how to drive jrs.** Everything it needs
   exists — `--progress never`, `jrs metadata`, fixed exit codes — but it is
   spread over `DOCS.md` and written for people.
3. **A library cannot be shared between two repositories.** A workspace
   reaches only its own members, under one root. Today the way to try a
   change to a library in the project that uses it is to build the jar,
   copy it into a Maven layout by hand, and point a `file://` repository
   at it with an absolute path that cannot be committed.

## 2. Package managers and the JetBrains Marketplace

The release job (`rust.yml`, `release`) writes the Homebrew formula from
`SHA256SUMS`, and keeps it as the `package-manifests` artifact. This part
publishes it. It changes no Rust code.

### 2.1 Homebrew

- A tap repository, `pwittchen/homebrew-tap`, holding `Formula/jrs.rb`, so
  `brew install pwittchen/tap/jrs` works. Done: `specs/HOMEBREW.md`. The formula `manifests.sh` writes
  already installs the binary, generates completions from `jrs completions`
  and tests `jrs --version`.
- A `homebrew` job after `release`, which dispatches the tap's own Update
  formulae workflow with a fine-grained token scoped to that one repository
  (secret `HOMEBREW_TAP_TOKEN`); that workflow commits `jrs <version>`. The job
  runs only on a `v*` tag and only from `pwittchen/jrs`, never from a fork.
  Done: `specs/HOMEBREW.md` §4.
- `homebrew-core` is a later step: it wants the formula to build from
  source and a project with some notability, neither of which is the first
  release's problem.

### 2.2 JetBrains Marketplace

`plugins/intellij` builds with the IntelliJ Platform Gradle Plugin, which has
`publishPlugin` and `signPlugin`. The plugin's ID is already
`dev.getjrs.jrs`, and its version is set by hand in `gradle.properties`
(`bump` leaves it alone).

- `build.gradle.kts` gains `signing { … }` and `publishing { token = … }`
  read from environment variables (`PUBLISH_TOKEN`, `CERTIFICATE_CHAIN`,
  `PRIVATE_KEY`, `PRIVATE_KEY_PASSWORD`), with nothing in the repository.
- `intellij.yml` gains a `publish` input; `rust.yml`'s `plugin` job passes
  it on a release. Publishing runs only when `pluginVersion` is not yet on
  the Marketplace, so a jrs release that did not touch the plugin publishes
  nothing; the check is the Marketplace's own API, by version.
- The first upload, which needs the vendor profile and the listing text, is
  done by hand. The listing reuses the plugin's README and screenshot.

### 2.3 Failure policy

None of the publishing jobs may fail a release. They run after `release`,
each on its own, and a failure is a red job the maintainer retries; the
GitHub release, the binaries and `install.sh` are already out. That keeps
the asset names, which `setup-jrs`, `install.sh` and `manifests.sh` depend
on, the one contract.

## 3. A guide for coding agents

A section "For coding agents and scripts" in `DOCS.md`, mirrored in
`website/docs/index.html`, with a short `AGENTS.md` snippet a project can
paste into its own repository. It documents what exists; it adds no flag.
What it covers:

- **The loop.** `jrs build`, `jrs test` (and `--filter`, `--method`,
  `--rerun-failed`), `jrs run -- <args>`, in that order, and why
  `jrs test` may run fewer classes than exist (test selection, SPEC §10.2)
  and how `--all` turns that off.
- **Stable output.** `--progress never --color never`, and that both are
  already the default when stderr is not a TTY or when `NO_COLOR` or
  `TERM=dumb` is set; a CI variable (`ui::in_ci`) turns the animation off and
  leaves colour to the terminal. Phase lines go to stderr. What a command
  reports goes to stdout: `classpath`, `metadata`, `tree`, `outdated`,
  `licenses`, `task --list`, `completions`, `cache path`, `migrate`, the
  JUnit tree and the program `jrs run` starts.
- **Exit codes.** `0`, `1` build or test failure, `2` usage or manifest
  error, `101` a jrs bug. An agent retrying on `1` is fixing code; on `2`
  it is fixing `jrs.toml` or the command.
- **Errors.** One `error: …` line from jrs (`cli::report`), then the
  toolchain's own output verbatim: `javac`'s `File.java:12: error:` lines
  and the JUnit tree. Nothing is reformatted, so the usual parsers work.
- **Changing dependencies.** `jrs add` and `jrs remove` rather than
  editing `jrs.toml`, since they keep the file's order and comments and
  re-resolve; `jrs tree --why` before adding an exclusion.
- **Reading the project.** `jrs metadata --no-deps` for roots and outputs
  without touching the network, `jrs metadata` for classpaths, and the
  rule that its `version` only goes up when a key changes meaning.
- **What not to do.** Editing `jrs.lock` by hand, deleting it to "fix"
  resolution (`jrs update` exists), and running `java` or `javac`
  directly against `target/` paths.

Writing it is also an audit: anything an agent needs that the guide has to
explain around (a JSON form of test results beyond JUnit XML, say) goes on
the roadmap as a finding, not into the guide as a workaround.

## 4. `jrs install`

### 4.1 What it crosses

SPEC §1.2 rules out "publishing artifacts to a repository". The reasons
behind it — credentials, signing, staging repositories, the many plugins
`maven-publish` stands in for — are all about remote repositories. A local
one has none of them: it is a directory on the developer's machine, written
by the developer's own command. This part narrows the non-goal to
"publishing to a remote repository", and adds the smallest version of the
rest: installing into a local Maven repository. It is what Maven's
`install` and Gradle's `publishToMavenLocal` do, and what makes a library
in one repository usable from a project in another without a reactor or a
composite build (ROADMAP_ARCHIVE §5).

### 4.2 The command

```text
jrs install [--repo <dir>] [--sources] [--javadoc]
```

Runs `jrs package`, then writes a jar without a `Class-Path`, and copies it,
a POM (§4.4) and, with the flags, the sources and Javadoc jars (`--sources`,
`--javadoc` as for `jrs package`) into
`<repo>/<group path>/<artifact>/<version>/`. The repository is `--repo`, or
`<localRepository>` in `~/.m2/settings.xml`, or `~/.m2/repository`. A
repository moved there is where `mvn` looks, so an install anywhere else would
reach nobody; this is the one piece of Maven's configuration jrs reads.
`${user.home}` is the one property expanded, and a settings file that cannot
be understood stops the command and names `--repo`. `-Dmaven.repo.local` is a
flag on one `mvn` command, and `--repo` is its counterpart here. The jar
`jrs package` wrote cannot go as it is: its `Class-Path` names this
machine's cache, and on a consumer's classpath those jars would load beside
the versions the consumer's own mediation chose. The jar for the repository
is written to `target/.jrs/install/`.

Each file is written as the cache writes one: to a temporary file in the
destination directory, then renamed (SPEC §8.6), so a consumer resolving at
the same moment never reads half a jar, and the jar and the POM get a `.sha1`
beside them, without which a consumer warns that it could not verify them. The
phase line is `Installed <group>:<artifact>:<version> to <dir>`. It also takes
`--no-build-cache`, as every command that builds does.

It also writes `maven-metadata-local.xml` beside the version directory, as
Maven does, listing the installed versions, so `jrs outdated` and Maven's
own resolver see them. `_remote.repositories` is not written; Maven treats
its absence as "installed locally", which is the truth.

### 4.3 A group

`[project]` has no group: a jrs project was never something another
project depends on. `jrs install` needs one, so `[project]` gains
`group`, optional, required by `jrs install` alone (exit `2` naming the
key when missing). `jrs migrate` writes it from `<groupId>` or a literal
Gradle `group = ...`. The artifact ID is `project.name`. `install` also
becomes a reserved task name, like every command: a manifest with a
`[tasks.install]` has to rename it.

### 4.4 The POM

Written with `quick-xml`, in a fixed order, as the project's dependencies
were resolved, not as they were declared:

| From the manifest | In the POM |
| --- | --- |
| `[dependencies]` entry | `<dependency>` in scope `compile` |
| `compile-only = true` | scope `provided` |
| `runtime-only = true` | scope `runtime` |
| a version from `[managed]` (`{}` or a BOM) | the concrete version the build used, written out |
| `classifier` | `<classifier>` |
| `exclusions` | `<exclusions>`, `*` kept as Maven reads it |
| a language's runtime library (`effective_dependencies`) | a `compile` dependency, as a Kotlin library's POM has `kotlin-stdlib` |
| `[dev-dependencies]` | left out |
| a local jar (`path = …`) | refused (exit `2`): no consumer can resolve it |

No `<dependencyManagement>`, no `<parent>`, no BOM import. The POM states
what this jar was built and tested against; a consumer's own mediation
decides the rest. Writing each version concretely means a consumer does not
need the producer's `[managed]` to resolve it. The exclusions a BOM declares
on a managed dependency (SPEC §8.9) are not written: they are not in the
manifest, and a consumer that wants them imports the BOM.

### 4.5 Workspaces

`jrs install` at a workspace root installs every member (narrowed by `-p`),
each with its own POM. A module dependency on another member becomes a
dependency on that member's `group:name:version`, so installing a member
installs the members it depends on first, as `-p` builds them. A member's
`group` falls back to `[workspace] group`, then to the root's `[project]
group`.

### 4.6 The consumer, the lockfile and snapshots

A consumer reaches the local repository through `[repositories]`. Since a
`file://` URL with an absolute home directory cannot be committed,
`[repositories]` accepts `file://~/` with `~` expanded to the user's home
directory, and nothing else of the shell's syntax:

```toml
[repositories]
local = { url = "file://~/.m2/repository", groups = ["com.acme"] }
```

The `groups` restriction keeps a stale `~/.m2` from answering for anything
else. What follows is the resolver's behaviour today, unchanged:

- **A `-SNAPSHOT` version** is the development loop. Its checksum is not
  pinned in `jrs.lock`, and a snapshot from a `file://` repository is
  re-checked on every build (SPEC §8.6). Edit the library, `jrs install`,
  build the consumer: it links the new jar. Since jars are byte-identical
  for identical inputs, reinstalling an unchanged library changes nothing
  downstream.
- **A release version** is pinned by checksum in the consumer's
  `jrs.lock`. Installing different bytes under the same release version
  fails a consumer that has to fetch the jar with a checksum mismatch, which
  is the right outcome; a consumer with the old jar in its cache goes on
  building against the bytes it pinned. `jrs install` warns when it
  overwrites a release version whose jar differs, and suggests a `-SNAPSHOT`.

`jrs install` reads the producer's `jrs.lock` as any build does and never
writes the consumer's.

### 4.7 What stays out

Deploying to a remote repository, credentials, signing, Gradle Module
Metadata (`.module`), the `maven-metadata.xml` of a remote layout and
timestamped snapshot names. Each of those is the publishing the narrowed
non-goal still rules out.

### 4.8 Tests

`tests/build.rs`: install a fixture library into a scratch repository,
check the files and the POM byte for byte; a consumer fixture resolves it
through `file://~/` with `HOME` pointed at the scratch directory; a
snapshot reinstall is picked up by the consumer's next build; a release
reinstall with different bytes fails it. Unit tests for the POM table in
§4.4, and `tests/workspace.rs` for a member depending on a member.

## 5. Decided

`jrs init` does not write an `AGENTS.md`: the file is the user's, so the guide
gives them a snippet to paste instead.
