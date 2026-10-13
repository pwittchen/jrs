# jrs plugin for IntelliJ IDEA

IntelliJ IDEA support for [jrs](https://getjrs.dev): it imports a jrs project
the way the IDE imports a Maven or Gradle one, builds and runs it through
`jrs`, and knows its `jrs.toml`. It is the plan in ROADMAP §4.

The plugin starts no daemon and does not read `jrs.toml` itself. Everything it
knows about a project comes from `jrs metadata` (SPEC §5.4), and everything it
does runs the `jrs` binary as a subprocess: `jrs build`, `jrs test`, `jrs add`
and so on.

## Requirements

- IntelliJ IDEA 2026.2 or newer, with the bundled Java plugin.
- `jrs` 0.9 or newer. The plugin looks for it in Settings, then on `PATH`,
  then in `~/.local/bin`, `~/.cargo/bin`, `/opt/homebrew/bin`,
  `/usr/local/bin` and `~/scoop/shims`.
- To build the plugin: JDK 21. Gradle downloads itself through the wrapper,
  and the IntelliJ Platform (about 1 GB) the first time it builds.

## Building

From this directory:

```
./gradlew buildPlugin
```

This writes `build/distributions/jrs-intellij-<version>.zip`, the plugin
archive. Other useful tasks:

```
./gradlew test        # the unit tests
./gradlew runIde      # start a sandboxed IntelliJ IDEA with the plugin installed
./gradlew verifyPlugin  # run the JetBrains Plugin Verifier
```

`runIde` is the quickest way to try a change: open one of the projects in
`../../examples` in the IDE it starts.

The IDE the plugin is compiled against is `platformVersion` in
`gradle.properties`, and the plugin's version is `pluginVersion`. The build
uses the [IntelliJ Platform Gradle Plugin](https://plugins.jetbrains.com/docs/intellij/tools-intellij-platform-gradle-plugin.html)
2.x and Kotlin.

## Installing

1. Build the archive (above).
2. In IntelliJ IDEA open **Settings | Plugins**, click the gear icon and choose
   **Install Plugin from Disk…**.
3. Pick `build/distributions/jrs-intellij-<version>.zip` and restart the IDE
   when it asks.

## Using it

### Opening a project

Open the directory that holds `jrs.toml` with **File | Open**. A notification
says it is a jrs project. Click **Load jrs project**, or **Always load** to skip
the question next time. You can also load it from **Tools | jrs | Load jrs
Project**, or from the refresh button in the jrs tool window.

Loading runs `jrs metadata` and sets up one module from it:

- the main and test source roots of every language, the generated sources of
  tasks (`source-outputs`) and the resource directories;
- `target/classes` and `target/test-classes` as the module's output, with the
  target directory excluded;
- the JDK jrs builds with (`jdk.home`), added to the IDE's JDKs if it is not
  there yet, and the language level from `[java]`'s release;
- one library per jar on the classpaths, with its `-sources.jar` attached when
  it is in the cache. Dependencies on the compile and runtime classpaths get the
  *Compile* scope, compile-only ones *Provided*, runtime-only ones *Runtime*,
  and test dependencies *Test*.

The first load runs `jrs metadata --no-deps` before the full one, so the
source roots are there before resolution has finished. Once a project is
loaded, it is loaded again each time it is opened.

To get sources for go-to-definition, run **Tools | jrs | Download Sources**
(`jrs fetch --sources`). It reloads the project when it is done.

### Keeping it in sync

When `jrs.toml` or `jrs.lock` changes, from the editor, from `git pull` or from
`jrs add` in a terminal, the editor shows a **Load jrs changes** banner over
both files. Turn on **Reload the project when jrs.toml or jrs.lock changes**
in the settings to reload without asking.

### Building

With the plugin on, **Build | Build Project** runs `jrs build`, and **Rebuild
Project** runs `jrs clean` and then `jrs build`. So does the build before a
run, including the IDE's own Application and JUnit configurations. The IDE and
the terminal then write the same `target/classes`. The Build tool window shows
jrs's plain transcript, and each `javac`, `kotlinc` or `scalac` error and
warning is listed as a message that opens the source line.

To have IntelliJ compile the project itself instead, turn off **Build the
project with jrs** in the settings.

### Running, testing and debugging

The **jrs** run configuration runs any jrs command: choose the command
(`run`, `test`, `package`, `task`, `tree`, …), its arguments, and the program
arguments that go after `--`. For `task`, the arguments start with the task's
name.

- **Tests from the editor.** On a test class or method, the gutter's run icon
  and the context menu offer **jrs test**. A class runs as
  `jrs test --filter '^<class>$'`, a method as
  `jrs test --method <class>#<method>`.
- **Test results.** `jrs test` shows its results in the test runner tree, read
  from the JUnit XML jrs writes to `target/test-reports`. Clicking a test opens
  it in the editor. The console keeps jrs's own output.
- **Debugging.** Debug a `run` or `test` configuration and the plugin passes
  `--debug=127.0.0.1:<free port>`. When the JVM reports that it is waiting for a
  debugger, the plugin attaches the IDE's debugger to it, so breakpoints work as
  usual.
- `javac` diagnostics in the console link to the source line.

### The jrs tool window

The **jrs** tool window, on the right, shows:

- the project's tasks from `[tasks]` with their descriptions and the hooks that
  run them; double-click a task to run `jrs task <name>`;
- the resolved dependencies, grouped by scope; select one and click **Remove
  Dependency** to run `jrs remove` on it.

Its toolbar has the same actions as **Tools | jrs**: load, `build`, `test`,
`run`, `package`, `clean`, `tree`, Download Sources, Add and Remove
Dependency, and under **More** `outdated`, `update` and `verify`. Commands run
in the Run tool window.

### Editing jrs.toml

- **Completion.** Press Ctrl+Space (⌃Space on macOS) after `[` for table
  names, or at the start of a line for the keys of the current table.
- **Validation.** An unknown table or key is highlighted with the warning the
  manifest parser gives
  (``unknown key `project.versoin` in jrs.toml (ignored)``). The inspection is **jrs | Unknown jrs.toml table or key** in
  **Settings | Editor | Inspections**. Tables whose keys you name yourself,
  such as `[dependencies]`, `[repositories]` or `[run.env]`, are not checked.
- **Dependencies.** **Tools | jrs | Add Dependency…** runs `jrs add` (with
  `--dev`, `--compile-only` or `--runtime-only` if you pick one), and **Remove
  Dependency…** runs `jrs remove`. The project then reloads. jrs does the
  edit, so the manifest's formatting and comments survive it.

`jrs.toml` and `jrs.lock` show the jrs logo in the project view and on editor
tabs.

### Migrating a Maven or Gradle project

In a project with a `pom.xml`, `build.gradle` or `build.gradle.kts` and no
`jrs.toml`, **Tools | jrs | Migrate to jrs** runs `jrs migrate`. It shows the
migration report and then loads the new jrs project.

### Settings

**Settings | Build, Execution, Deployment | Build Tools | jrs**:

| Setting | Default |
| --- | --- |
| jrs executable | empty: found on `PATH` |
| Build the project with jrs | on |
| Import projects with a jrs.toml when they are opened | off |
| Reload the project when jrs.toml or jrs.lock changes | off |

## What it does not do yet

- **Kotlin, Scala and Groovy compiler settings.** Their source roots are set
  up and their runtime libraries are on the classpath, so the IDE's language
  plugins analyse the code. The compiler arguments and `[kotlin] plugins` are
  not passed to them yet, because `jrs metadata` does not carry them.
- **One module per project.** jrs builds single-module projects, and the
  plugin sets up one IntelliJ module to match.

## Layout

```
src/main/kotlin/dev/getjrs/intellij/
├── JrsCli.kt                 finding and running the jrs binary
├── model/JrsMetadata.kt      the `jrs metadata` JSON
├── sync/                     import, the module setup, the change banner
├── build/                    Build Project → jrs build, compiler diagnostics
├── run/                      the run configuration, tests, debugger, gutter
├── toolwindow/               the jrs tool window
├── actions/                  Tools | jrs
├── manifest/                 jrs.toml completion and inspection
└── settings/                 the settings page
src/main/resources/icons/     jrs.png and jrs@2x.png, from the repository's logo.png
```

The icons are `logo.png` from the repository root, scaled to 16×16 and 32×32
(the `@2x` variant the IDE uses on HiDPI screens). `META-INF/pluginIcon.svg`
embeds it at 40×40 for the plugin list. If the logo changes, regenerate them:

```
sips -z 16 16 ../../logo.png --out src/main/resources/icons/jrs.png
sips -z 32 32 ../../logo.png --out src/main/resources/icons/jrs@2x.png
```

(on Linux: `convert ../../logo.png -resize 16x16 …` with ImageMagick).
