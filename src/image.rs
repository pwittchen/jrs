//! Runtime images: `jdeps`, `jlink` and `jpackage`.
//!
//! The input is a packaged application in the portable layout — an app jar whose
//! `Class-Path` names `lib/<file>.jar`, with that `lib/` beside it (or a fat jar
//! and no `lib/`). Everything here drives the JDK's own tools and passes their
//! output through verbatim; jrs only decides what goes in and where it lands.
//!
//! `jdeps` is the odd one out among the JDK tools: it does not expand
//! `@argfiles`. Handed one, it warns that the path does not exist and exits 0
//! with an empty answer, which would quietly shrink every image to `java.base`.
//! So the jars go on the command line instead, in batches short enough for the
//! Windows command-line limit, and the answers are merged.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::error::{IoResultExt, JrsError, Result};
use crate::project;
use crate::toolchain::{CapturedOutput, Toolchain, run_captured};
use crate::ui::{Stream, Ui};

/// The module every runtime has, and all an application needs when `jdeps`
/// finds nothing more.
const BASE_MODULE: &str = "java.base";

/// How many bytes of jar paths one `jdeps` command line may carry. Windows caps a
/// whole command line at 32,767 UTF-16 units; this leaves room for the program
/// path and the flags.
const ARG_BUDGET: usize = 24_000;

/// A JDK tool that lives beside `javac` — `jdeps`, `jlink`, `jpackage`,
/// `javadoc` — or an error naming the JDK that lacks it.
///
/// Some JDK packages (headless Linux ones in particular) leave these tools out,
/// so their absence is a toolchain problem to report, not an assumption to make.
///
/// # Errors
///
/// [`JrsError::Toolchain`] if there is no `name` beside `javac`.
pub fn tool(toolchain: &Toolchain, name: &str) -> Result<PathBuf> {
    toolchain.tool(name)
}

/// The JDK modules an application needs, per `jdeps --print-module-deps`, plus
/// `extra` (`[package] add-modules`), sorted and deduplicated. `java.base` when
/// `jdeps` finds nothing.
///
/// `jars` should be every jar the application runs with — see [`App::jars`] —
/// so modules used only by a dependency are included too. What `jdeps` cannot
/// see (reflection, `ServiceLoader` lookups of JDK services) is what `extra` is
/// for.
///
/// # Errors
///
/// [`JrsError::Toolchain`] if the JDK has no `jdeps`, and [`JrsError::Build`]
/// if one of `jars` does not exist or `jdeps` cannot be started or fails.
pub fn modules(
    toolchain: &Toolchain,
    jars: &[PathBuf],
    release: u32,
    extra: &[String],
    ui: &Ui,
) -> Result<Vec<String>> {
    let jdeps = tool(toolchain, "jdeps")?;
    // jdeps only warns about a missing input and carries on, so a typo'd path
    // would silently drop that jar's modules.
    if let Some(missing) = jars.iter().find(|j| !j.is_file()) {
        return Err(JrsError::build(format!(
            "{}: no such jar to analyse",
            missing.display()
        )));
    }

    let mut found = BTreeSet::new();
    for batch in batches(jars, ARG_BUDGET) {
        let mut args: Vec<OsString> = vec![
            "--ignore-missing-deps".into(),
            "--print-module-deps".into(),
            "--multi-release".into(),
            release.to_string().into(),
        ];
        args.extend(batch.iter().map(|j| j.as_os_str().to_os_string()));

        let output = run_captured(ui, &jdeps, &args)?;
        if !output.ok() {
            replay(ui, &output);
            return Err(JrsError::build(
                "jdeps could not work out which JDK modules the application needs\n\n\
                 its output is above; modules it misses can be listed under \
                 `[package] add-modules`",
            ));
        }
        let (modules, chatter) = module_deps(&output.stdout);
        // Whatever else jdeps printed is a warning meant for the user.
        if !chatter.is_empty() {
            ui.passthrough(Stream::Err, &chatter.join("\n"));
        }
        if !output.stderr.trim().is_empty() {
            ui.passthrough(Stream::Err, output.stderr.trim_end());
        }
        found.extend(modules);
    }
    Ok(merge_modules(found, extra))
}

/// Split `jars` into runs whose paths fit in `budget` bytes. Every run holds at
/// least one jar, however long its path.
fn batches(jars: &[PathBuf], budget: usize) -> Vec<&[PathBuf]> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut used = 0;
    for (i, jar) in jars.iter().enumerate() {
        // A separator and a pair of quotes, should the platform need them.
        let len = jar.as_os_str().len() + 3;
        if i > start && used + len > budget {
            out.push(&jars[start..i]);
            start = i;
            used = 0;
        }
        used += len;
    }
    if start < jars.len() {
        out.push(&jars[start..]);
    }
    out
}

/// Read `jdeps --print-module-deps` output: the module list is the last
/// non-empty line, comma-separated. Returns the modules and every other
/// non-empty line (jdeps' warnings). A last line that is not a module list —
/// jdeps prints only a warning when it has nothing to analyse — yields no
/// modules rather than garbage ones.
fn module_deps(stdout: &str) -> (Vec<String>, Vec<&str>) {
    let lines: Vec<&str> = stdout
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.trim().is_empty())
        .collect();
    let Some((last, rest)) = lines.split_last() else {
        return (Vec::new(), Vec::new());
    };
    let modules: Vec<String> = last.split(',').map(|m| m.trim().to_string()).collect();
    if modules.iter().all(|m| is_module_name(m)) {
        (modules, rest.to_vec())
    } else {
        (Vec::new(), lines)
    }
}

/// A dotted sequence of Java identifiers, which is all a module name can be.
fn is_module_name(name: &str) -> bool {
    !name.is_empty()
        && name.split('.').all(|part| {
            let mut chars = part.chars();
            chars
                .next()
                .is_some_and(|c| c.is_alphabetic() || c == '_' || c == '$')
                && chars.all(|c| c.is_alphanumeric() || c == '_' || c == '$')
        })
}

fn merge_modules(found: impl IntoIterator<Item = String>, extra: &[String]) -> Vec<String> {
    let mut all: BTreeSet<String> = found.into_iter().collect();
    if all.is_empty() {
        all.insert(BASE_MODULE.to_string());
    }
    all.extend(
        extra
            .iter()
            .map(|m| m.trim().to_string())
            .filter(|m| !m.is_empty()),
    );
    all.into_iter().collect()
}

/// A packaged application, as `jlink` and `jpackage` take it.
#[derive(Debug, Clone, Copy)]
pub struct App<'a> {
    pub name: &'a str,
    pub version: &'a str,
    pub main_class: Option<&'a str>,
    /// The app jar; a thin one's `Class-Path` holds relative `lib/` entries.
    pub jar: &'a Path,
    /// The `lib/` directory beside a thin jar; `None` for a fat jar.
    pub lib_dir: Option<&'a Path>,
    /// `[run] jvm-args`, baked into the launchers.
    pub jvm_args: &'a [String],
    /// `[run] java-agents`, as `lib/<file>` or `agents/<file>` relative to
    /// the application directory, loaded with `-javaagent:` ahead of
    /// `jvm_args`.
    pub java_agents: &'a [String],
    /// The agents pinned apart from the project's graph, which no `lib/`
    /// holds: each jar, and the `agents/<file>` it is staged as. A fat jar
    /// can carry these, since nothing unpacked them.
    pub agent_jars: &'a [(PathBuf, String)],
}

impl App<'_> {
    /// Every jar the application runs with: its own, then `lib/`'s, sorted,
    /// then the pinned agents' — what [`modules`] wants to see. An agent runs
    /// in the image's runtime too, and needs `java.instrument` at the least.
    ///
    /// # Errors
    ///
    /// [`JrsError::Io`] if the `lib/` directory cannot be read.
    pub fn jars(&self) -> Result<Vec<PathBuf>> {
        let mut jars = vec![self.jar.to_path_buf()];
        if let Some(lib) = self.lib_dir {
            jars.extend(project::find_by_extension(lib, "jar")?);
        }
        jars.extend(self.agent_jars.iter().map(|(jar, _)| jar.clone()));
        Ok(jars)
    }

    /// The jar's file name, which the launchers and `jpackage --main-jar` name.
    pub(crate) fn jar_name(&self) -> Result<&str> {
        self.jar
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| {
                JrsError::build(format!(
                    "{}: the app jar needs a UTF-8 file name to be launched by name",
                    self.jar.display()
                ))
            })
    }

    /// Copy the jar and, when there is one, `lib/` into `dir`, keeping the
    /// relative `Class-Path` valid, then each pinned agent into `agents/`.
    pub(crate) fn stage(&self, dir: &Path) -> Result<()> {
        std::fs::create_dir_all(dir).path(dir)?;
        let jar = dir.join(self.jar_name()?);
        std::fs::copy(self.jar, &jar).path(&jar)?;
        if let Some(lib) = self.lib_dir {
            project::copy_tree(lib, &dir.join("lib"))?;
        }
        for (source, relative) in self.agent_jars {
            let staged = dir.join(relative);
            if let Some(parent) = staged.parent() {
                std::fs::create_dir_all(parent).path(parent)?;
            }
            std::fs::copy(source, &staged).path(&staged)?;
        }
        Ok(())
    }
}

/// What [`jlink`] built.
#[derive(Debug)]
pub struct ImageOutcome {
    pub path: PathBuf,
    pub modules: Vec<String>,
    /// Total size of every file in the image.
    pub bytes: u64,
}

/// `jlink` a trimmed runtime into `output`, copy the application into
/// `output/app/` (jar and `lib/`), and write the launchers `output/bin/<name>`
/// (sh) and `output/bin/<name>.bat`, which run
/// `java [jvm-args] -jar app/<jar> <args>` relative to the image.
///
/// `output` is removed first: jlink refuses an existing directory, and it lives
/// under `target/`, which is disposable by contract.
///
/// # Errors
///
/// [`JrsError::Toolchain`] if the JDK has no `jlink`; [`JrsError::Build`] if the
/// app jar's name is not UTF-8, `jlink` cannot be started or fails, or a
/// launcher would overwrite one of the runtime's own; [`JrsError::Io`] if
/// `output` cannot be cleared or written.
pub fn jlink(
    toolchain: &Toolchain,
    app: &App,
    modules: &[String],
    output: &Path,
    ui: &Ui,
) -> Result<ImageOutcome> {
    let jlink = tool(toolchain, "jlink")?;
    let jar_name = app.jar_name()?;
    let modules = if modules.is_empty() {
        vec![BASE_MODULE.to_string()]
    } else {
        modules.to_vec()
    };

    if output.exists() {
        std::fs::remove_dir_all(output).path(output)?;
    }
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent).path(parent)?;
    }

    // No `--compress`: its syntax changed between JDK releases (`2` became
    // `zip-6`), and the defaults are a fine trade.
    let args: Vec<OsString> = vec![
        "--add-modules".into(),
        modules.join(",").into(),
        "--output".into(),
        output.as_os_str().to_os_string(),
        "--strip-debug".into(),
        "--no-header-files".into(),
        "--no-man-pages".into(),
    ];
    let result = run_captured(ui, &jlink, &args)?;
    replay(ui, &result);
    if !result.ok() {
        return Err(JrsError::build(format!(
            "jlink could not build a runtime image with {}",
            modules.join(", ")
        )));
    }

    // A project named `java` (or `keytool`, ...) would overwrite the runtime's
    // own launcher, and the image would then run itself instead of the JVM. On
    // Windows the runtime's launcher is `java.exe`, which `cmd` picks over
    // `java.bat`, so there the project's launcher would never run at all.
    let bin = output.join("bin");
    let sh = bin.join(app.name);
    let bat = bin.join(format!("{}.bat", app.name));
    let exe = bin.join(format!("{}.exe", app.name));
    for existing in [&sh, &bat, &exe] {
        if existing.exists() {
            return Err(JrsError::build(format!(
                "cannot write the launcher for {}: the Java runtime already has {}\n\n\
                 rename the project so its launcher does not clash with one of the runtime's own",
                app.name,
                existing.display()
            )));
        }
    }

    app.stage(&output.join("app"))?;
    std::fs::write(&sh, sh_launcher(app.jvm_args, jar_name, app.java_agents)).path(&sh)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&sh, std::fs::Permissions::from_mode(0o755)).path(&sh)?;
    }
    std::fs::write(&bat, bat_launcher(app.jvm_args, jar_name, app.java_agents)).path(&bat)?;

    let mut bytes = 0;
    for file in project::find_all(output)? {
        bytes += std::fs::metadata(&file).path(&file)?.len();
    }
    Ok(ImageOutcome {
        path: output.to_path_buf(),
        modules,
        bytes,
    })
}

/// The POSIX launcher. It finds the image from its own location, so the image
/// can be moved or unpacked anywhere; `$JAVA_OPTS` comes before the fixed
/// arguments, as the conventional escape hatch. `agents` are `lib/<file>`
/// entries under `app/`.
fn sh_launcher(jvm_args: &[String], jar_name: &str, agents: &[String]) -> String {
    let mut s = String::from("#!/bin/sh\n# Generated by jrs.\n");
    s.push_str("DIR=$(cd \"$(dirname \"$0\")/..\" && pwd)\n");
    s.push_str("# JAVA_OPTS is unquoted on purpose: it holds any number of options.\n");
    s.push_str("exec \"$DIR/bin/java\" $JAVA_OPTS");
    for agent in agents {
        if agent.chars().all(|c| is_sh_safe(c) || c == '/') {
            let _ = write!(s, " \"-javaagent:$DIR/app/{agent}\"");
        } else {
            let _ = write!(s, " \"-javaagent:$DIR/app/\"{}", sh_quote(agent));
        }
    }
    for arg in jvm_args {
        s.push(' ');
        s.push_str(&sh_quote(arg));
    }
    if jar_name.chars().all(is_sh_safe) {
        let _ = write!(s, " -jar \"$DIR/app/{jar_name}\"");
    } else {
        let _ = write!(s, " -jar \"$DIR/app/\"{}", sh_quote(jar_name));
    }
    s.push_str(" \"$@\"\n");
    s
}

/// Characters that mean nothing to the shell inside double quotes.
pub(crate) fn is_sh_safe(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '+' | '@' | '%' | ',' | ':' | '=')
}

/// Single quotes make everything literal; a single quote itself has to close
/// the string, be escaped, and reopen it.
pub(crate) fn sh_quote(arg: &str) -> String {
    format!("'{}'", arg.replace('\'', r"'\''"))
}

/// The Windows launcher, with CRLF line endings because `cmd` is happiest
/// with them. `agents` are `lib/<file>` entries under `app\`.
fn bat_launcher(jvm_args: &[String], jar_name: &str, agents: &[String]) -> String {
    let mut s = String::new();
    for line in [
        "@echo off",
        "rem Generated by jrs.",
        "setlocal",
        "set \"DIR=%~dp0..\"",
    ] {
        s.push_str(line);
        s.push_str("\r\n");
    }
    s.push_str("\"%DIR%\\bin\\java.exe\" %JAVA_OPTS%");
    for agent in agents {
        let _ = write!(
            s,
            " \"-javaagent:%DIR%\\app\\{}\"",
            agent.replace('/', "\\").replace('%', "%%")
        );
    }
    for arg in jvm_args {
        s.push(' ');
        s.push_str(&bat_quote(arg));
    }
    let _ = write!(
        s,
        " -jar \"%DIR%\\app\\{}\" %*\r\n",
        jar_name.replace('%', "%%")
    );
    s
}

/// Quote one argument for a `.bat` line that starts a Windows program.
///
/// Two parsers see it: `cmd`, which expands `%` (hence `%%`) and treats
/// `& | < > ^` as operators outside quotes, and then the Java launcher, which
/// splits the command line by the MSVCRT rules — so backslashes are doubled
/// only where they precede a quote.
pub(crate) fn bat_quote(arg: &str) -> String {
    let arg = arg.replace('%', "%%");
    let needs_quotes = arg.is_empty()
        || arg
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, '"' | '&' | '|' | '<' | '>' | '^'));
    if !needs_quotes {
        return arg;
    }
    let mut out = String::from("\"");
    let mut backslashes = 0;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.push_str(&"\\".repeat(backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.push_str(&"\\".repeat(backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    out.push_str(&"\\".repeat(backslashes * 2));
    out.push('"');
    out
}

/// `jpackage` a native installer — or, with `kind` `app-image`, a bare
/// application image — into `dest`, and return the path it produced.
///
/// `kind` is jpackage's own `--type` (`app-image`, `dmg`, `pkg`, `deb`, `rpm`,
/// `exe`, `msi`), passed through unvalidated: jpackage knows what the platform
/// can build, and `None` leaves it to choose the platform default. The input is
/// staged in `work_dir/jpackage-input/` — jpackage copies that whole directory
/// into the application, so `lib/` beside the jar keeps the relative
/// `Class-Path` working. Each JVM argument becomes one `--java-options`.
///
/// `--app-version` is derived by [`app_version`]; where jpackage still rejects
/// it (macOS refuses a version starting with 0), jpackage's own error is passed
/// through verbatim.
///
/// # Errors
///
/// [`JrsError::Manifest`] if the app has no main class; [`JrsError::Toolchain`]
/// if the JDK has no `jpackage`; [`JrsError::Build`] if the app jar's name is
/// not UTF-8, or `jpackage` cannot be started, fails, or leaves nothing in
/// `dest`; [`JrsError::Io`] if the input or `dest` cannot be staged.
pub fn jpackage(
    toolchain: &Toolchain,
    app: &App,
    modules: &[String],
    kind: Option<&str>,
    dest: &Path,
    work_dir: &Path,
    ui: &Ui,
) -> Result<PathBuf> {
    // A manifest problem, so it is reported before anything is looked for.
    let main_class = app.main_class.ok_or_else(|| {
        JrsError::manifest(
            "`jrs package --jpackage` needs a main class\n\n\
             add it to jrs.toml:\n\n    [project]\n    main-class = \"com.example.Main\"",
        )
    })?;
    let jpackage = tool(toolchain, "jpackage")?;
    let jar_name = app.jar_name()?;

    let input = work_dir.join("jpackage-input");
    if input.exists() {
        std::fs::remove_dir_all(&input).path(&input)?;
    }
    app.stage(&input)?;

    // Emptied so whatever is in it afterwards is what this run produced.
    if dest.exists() {
        std::fs::remove_dir_all(dest).path(dest)?;
    }
    std::fs::create_dir_all(dest).path(dest)?;

    let modules = if modules.is_empty() {
        BASE_MODULE.to_string()
    } else {
        modules.join(",")
    };
    let mut args: Vec<OsString> = vec![
        "--input".into(),
        input.as_os_str().to_os_string(),
        "--main-jar".into(),
        jar_name.into(),
        "--main-class".into(),
        main_class.into(),
        "--name".into(),
        app.name.into(),
        "--dest".into(),
        dest.as_os_str().to_os_string(),
        "--add-modules".into(),
        modules.into(),
    ];
    if let Some(kind) = kind {
        args.push("--type".into());
        args.push(kind.into());
    }
    if let Some(version) = app_version(app.version) {
        args.push("--app-version".into());
        args.push(version.into());
    }
    // jpackage's launcher expands `$APPDIR` to where the staged input landed.
    for agent in app.java_agents {
        args.push("--java-options".into());
        args.push(format!("-javaagent:$APPDIR/{agent}").into());
    }
    for option in app.jvm_args {
        args.push("--java-options".into());
        args.push(option.into());
    }

    let result = run_captured(ui, &jpackage, &args)?;
    replay(ui, &result);
    if !result.ok() {
        return Err(JrsError::build(format!(
            "jpackage could not package {}",
            app.name
        )));
    }

    let mut produced: Vec<PathBuf> = std::fs::read_dir(dest)
        .path(dest)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    // jpackage makes one artifact per run; sorting keeps the choice stable
    // should a future version leave something beside it.
    produced.sort();
    produced.into_iter().next().ok_or_else(|| {
        JrsError::build(format!(
            "jpackage reported success but left nothing in {}",
            dest.display()
        ))
    })
}

/// The `--app-version` jpackage will accept: one to three dot-separated
/// numbers, taken from the front of the project version (`0.1.0-SNAPSHOT` →
/// `0.1.0`, `2.3.1.Final` → `2.3.1`, `1.2.3.4` → `1.2.3`). `None` when the
/// version does not start with a number, and the flag is then left out.
#[must_use]
pub fn app_version(version: &str) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    let mut rest = version;
    while parts.len() < 3 {
        let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        if digits == 0 {
            break;
        }
        parts.push(&rest[..digits]);
        rest = &rest[digits..];
        // Only a dot followed by another number continues the version.
        match rest.strip_prefix('.') {
            Some(next) if next.starts_with(|c: char| c.is_ascii_digit()) => rest = next,
            _ => break,
        }
    }
    (!parts.is_empty()).then(|| parts.join("."))
}

/// Pass a tool's output through verbatim, the live region torn down first.
fn replay(ui: &Ui, output: &CapturedOutput) {
    for text in [&output.stderr, &output.stdout] {
        if !text.trim().is_empty() {
            ui.passthrough(Stream::Err, text.trim_end());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package::{JarManifest, write_thin_jar};
    use crate::ui::{CharsetChoice, UiOptions, When};

    struct Tree {
        root: PathBuf,
    }

    impl Tree {
        fn new(name: &str) -> Tree {
            let root =
                std::env::temp_dir().join(format!("jrs-image-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap();
            Tree { root }
        }

        fn write(&self, relative: &str, contents: &str) -> PathBuf {
            let path = self.root.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, contents).unwrap();
            path
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn quiet_ui() -> Ui {
        Ui::new(UiOptions {
            quiet: true,
            progress: When::Never,
            color: When::Never,
            charset: CharsetChoice::Ascii,
            ..Default::default()
        })
    }

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    /// A toolchain whose `bin/` is an empty directory of our choosing.
    fn fake_toolchain(tree: &Tree) -> Toolchain {
        let bin = tree.root.join("jdk/bin");
        std::fs::create_dir_all(&bin).unwrap();
        Toolchain {
            javac: bin.join("javac"),
            java: bin.join("java"),
            jar: bin.join("jar"),
            version: 21,
            home: Some(tree.root.join("jdk")),
        }
    }

    // ---- pure -------------------------------------------------------------

    #[test]
    fn app_versions_keep_the_leading_numbers() {
        assert_eq!(app_version("0.1.0-SNAPSHOT").as_deref(), Some("0.1.0"));
        assert_eq!(app_version("2.3.1.Final").as_deref(), Some("2.3.1"));
        assert_eq!(app_version("1.2.3.4").as_deref(), Some("1.2.3"));
        assert_eq!(app_version("1.0").as_deref(), Some("1.0"));
        assert_eq!(app_version("7").as_deref(), Some("7"));
        assert_eq!(app_version("1-SNAPSHOT").as_deref(), Some("1"));
        assert_eq!(app_version("10.20-rc1").as_deref(), Some("10.20"));
        assert_eq!(app_version("1.x").as_deref(), Some("1"));
        assert_eq!(app_version("1.").as_deref(), Some("1"));
        assert_eq!(app_version("v1.0"), None);
        assert_eq!(app_version("SNAPSHOT"), None);
        assert_eq!(app_version(""), None);
    }

    #[test]
    fn the_sh_launcher_quotes_every_jvm_argument() {
        let text = sh_launcher(
            &strings(&["-Xmx512m", "-Dgreeting=hello world", "-Dq=it's"]),
            "app-1.0.jar",
            &[],
        );
        assert_eq!(
            text,
            "#!/bin/sh\n\
             # Generated by jrs.\n\
             DIR=$(cd \"$(dirname \"$0\")/..\" && pwd)\n\
             # JAVA_OPTS is unquoted on purpose: it holds any number of options.\n\
             exec \"$DIR/bin/java\" $JAVA_OPTS '-Xmx512m' '-Dgreeting=hello world' \
             '-Dq=it'\\''s' -jar \"$DIR/app/app-1.0.jar\" \"$@\"\n"
        );
    }

    #[test]
    fn the_sh_launcher_puts_java_opts_before_the_fixed_arguments() {
        let text = sh_launcher(&strings(&["-Dfixed=1"]), "app.jar", &[]);
        let exec = text.lines().last().unwrap();
        let opts = exec.find("$JAVA_OPTS").unwrap();
        assert!(opts < exec.find("'-Dfixed=1'").unwrap(), "{exec}");
        assert!(!exec.contains("\"$JAVA_OPTS\""), "must split into words");
    }

    #[test]
    fn the_sh_launcher_quotes_an_awkward_jar_name() {
        let text = sh_launcher(&[], "my app's.jar", &[]);
        assert!(
            text.contains(r#"-jar "$DIR/app/"'my app'\''s.jar' "$@""#),
            "{text}"
        );
    }

    #[test]
    fn launchers_are_deterministic() {
        let args = strings(&["-Da=b c"]);
        assert_eq!(
            sh_launcher(&args, "a.jar", &[]),
            sh_launcher(&args, "a.jar", &[])
        );
        assert_eq!(
            bat_launcher(&args, "a.jar", &[]),
            bat_launcher(&args, "a.jar", &[])
        );
    }

    #[test]
    fn the_bat_launcher_runs_the_images_own_java() {
        let text = bat_launcher(
            &strings(&["-Xmx512m", "-Dgreeting=hello world"]),
            "app.jar",
            &[],
        );
        assert_eq!(
            text,
            "@echo off\r\n\
             rem Generated by jrs.\r\n\
             setlocal\r\n\
             set \"DIR=%~dp0..\"\r\n\
             \"%DIR%\\bin\\java.exe\" %JAVA_OPTS% -Xmx512m \"-Dgreeting=hello world\" \
             -jar \"%DIR%\\app\\app.jar\" %*\r\n"
        );
    }

    #[test]
    fn launchers_load_run_agents_from_the_images_lib_ahead_of_the_jvm_arguments() {
        let agents = strings(&["lib/mockito-core-5.14.2.jar", "lib/odd name%.jar"]);
        let sh = sh_launcher(&strings(&["-Xmx1g"]), "app.jar", &agents);
        let exec = sh.lines().last().unwrap();
        assert!(
            exec.contains(
                " \"-javaagent:$DIR/app/lib/mockito-core-5.14.2.jar\" \
                 \"-javaagent:$DIR/app/\"'lib/odd name%.jar' '-Xmx1g' -jar"
            ),
            "{exec}"
        );
        assert!(exec.find("$JAVA_OPTS").unwrap() < exec.find("-javaagent").unwrap());

        let bat = bat_launcher(&strings(&["-Xmx1g"]), "app.jar", &agents);
        assert!(
            bat.contains(
                " \"-javaagent:%DIR%\\app\\lib\\mockito-core-5.14.2.jar\" \
                 \"-javaagent:%DIR%\\app\\lib\\odd name%%.jar\" -Xmx1g -jar"
            ),
            "{bat}"
        );
    }

    #[test]
    fn bat_arguments_survive_cmd_and_the_java_launcher() {
        assert_eq!(bat_quote("-Xmx1g"), "-Xmx1g");
        assert_eq!(bat_quote("-Dp=100%"), "-Dp=100%%");
        assert_eq!(bat_quote("a b"), "\"a b\"");
        assert_eq!(bat_quote("a&b"), "\"a&b\"");
        assert_eq!(bat_quote(""), "\"\"");
        assert_eq!(bat_quote(r#"say "hi""#), r#""say \"hi\"""#);
        // A trailing backslash would otherwise escape the closing quote.
        assert_eq!(bat_quote(r"C:\my dir\"), r#""C:\my dir\\""#);
        assert_eq!(bat_quote(r"C:\dir"), r"C:\dir");
    }

    #[test]
    fn jdeps_output_is_its_last_line() {
        let (modules, chatter) = module_deps("java.base,java.logging,java.sql\n");
        assert_eq!(modules, strings(&["java.base", "java.logging", "java.sql"]));
        assert!(chatter.is_empty());

        let (modules, chatter) =
            module_deps("Warning: split package: a.b\n\n  java.base , java.xml  \n\n");
        assert_eq!(modules, strings(&["java.base", "java.xml"]));
        assert_eq!(chatter, vec!["Warning: split package: a.b"]);
    }

    #[test]
    fn jdeps_output_without_a_module_list_yields_no_modules() {
        assert_eq!(module_deps("").0, Vec::<String>::new());
        assert_eq!(module_deps("\n\n").0, Vec::<String>::new());
        // What jdeps prints, with exit status 0, when an input does not exist.
        let (modules, chatter) = module_deps("Warning: Path does not exist: @jars.args\n\n");
        assert!(modules.is_empty());
        assert_eq!(chatter, vec!["Warning: Path does not exist: @jars.args"]);
    }

    #[test]
    fn module_names_are_dotted_identifiers() {
        assert!(is_module_name("java.base"));
        assert!(is_module_name("jdk.unsupported"));
        assert!(is_module_name("m"));
        assert!(!is_module_name(""));
        assert!(!is_module_name("java..base"));
        assert!(!is_module_name("1java"));
        assert!(!is_module_name("Warning: x"));
    }

    #[test]
    fn extra_modules_are_merged_sorted_and_deduplicated() {
        let merged = merge_modules(
            strings(&["java.sql", "java.base"]),
            &strings(&["jdk.crypto.ec", " java.sql ", ""]),
        );
        assert_eq!(merged, strings(&["java.base", "java.sql", "jdk.crypto.ec"]));
    }

    #[test]
    fn nothing_found_means_java_base() {
        assert_eq!(merge_modules(Vec::new(), &[]), strings(&["java.base"]));
        assert_eq!(
            merge_modules(Vec::new(), &strings(&["java.sql"])),
            strings(&["java.base", "java.sql"])
        );
    }

    #[test]
    fn jars_are_batched_under_the_budget() {
        let jars: Vec<PathBuf> = (0..5)
            .map(|i| PathBuf::from(format!("/j{i}.jar")))
            .collect();
        // Each costs 7 bytes plus 3 of slack.
        let runs = batches(&jars, 25);
        assert_eq!(runs.iter().map(|r| r.len()).collect::<Vec<_>>(), [2, 2, 1]);
        assert_eq!(runs.concat(), jars);
        assert_eq!(batches(&jars, ARG_BUDGET).len(), 1);
        // One oversized path still gets a run of its own.
        assert_eq!(batches(&jars, 1).len(), 5);
        assert!(batches(&[], ARG_BUDGET).is_empty());
    }

    #[test]
    fn a_missing_tool_names_itself_and_the_jdk() {
        let tree = Tree::new("missing-tool");
        let tc = fake_toolchain(&tree);
        let err = tool(&tc, "jpackage").unwrap_err();
        assert!(matches!(err, JrsError::Toolchain(_)));
        let text = err.to_string();
        assert!(text.contains("`jpackage`"), "{text}");
        assert!(
            text.contains(&tree.root.join("jdk").display().to_string()),
            "{text}"
        );

        let name = if cfg!(windows) { "jdeps.exe" } else { "jdeps" };
        let present = tree.write(&format!("jdk/bin/{name}"), "");
        assert_eq!(tool(&tc, "jdeps").unwrap(), present);
    }

    #[test]
    fn jpackage_needs_a_main_class() {
        let tree = Tree::new("no-main");
        let tc = fake_toolchain(&tree);
        let jar = tree.write("app.jar", "");
        let app = App {
            name: "demo",
            version: "1.0",
            main_class: None,
            jar: &jar,
            lib_dir: None,
            jvm_args: &[],
            java_agents: &[],
            agent_jars: &[],
        };
        let err = jpackage(
            &tc,
            &app,
            &[],
            None,
            &tree.root.join("dist"),
            &tree.root.join(".jrs"),
            &quiet_ui(),
        )
        .unwrap_err();
        assert!(matches!(err, JrsError::Manifest(_)));
        assert!(err.to_string().contains("main-class"), "{err}");
    }

    #[test]
    fn an_apps_jars_are_its_own_then_lib_sorted() {
        let tree = Tree::new("app-jars");
        let jar = tree.write("app.jar", "");
        tree.write("lib/z.jar", "");
        tree.write("lib/a.jar", "");
        tree.write("lib/notes.txt", "");
        let lib = tree.root.join("lib");
        let app = App {
            name: "demo",
            version: "1.0",
            main_class: None,
            jar: &jar,
            lib_dir: Some(&lib),
            jvm_args: &[],
            java_agents: &[],
            agent_jars: &[],
        };
        assert_eq!(
            app.jars().unwrap(),
            vec![jar.clone(), lib.join("a.jar"), lib.join("z.jar")]
        );
    }

    // ---- against a real JDK ----------------------------------------------

    fn jdk(test: &str) -> Option<Toolchain> {
        match Toolchain::discover() {
            Ok(t) => Some(t),
            Err(e) => {
                eprintln!("SKIPPED {test}: no JDK: {e}");
                None
            }
        }
    }

    /// A two-jar application in the portable layout: `app.jar` whose
    /// `Class-Path` is `lib/dep.jar`, and a dependency that needs
    /// `java.logging` — a module the app jar alone does not reveal.
    struct Fixture {
        tree: Tree,
        jar: PathBuf,
        lib: PathBuf,
    }

    fn fixture(tc: &Toolchain, name: &str) -> Fixture {
        let tree = Tree::new(name);
        tree.write(
            "src/dep/Dep.java",
            "package dep;\n\
             public class Dep {\n\
                 public static String message() {\n\
                     return java.util.logging.Logger.getLogger(\"dep\").getName();\n\
                 }\n\
             }\n",
        );
        tree.write(
            "src/app/Main.java",
            "package app;\n\
             public class Main {\n\
                 public static void main(String[] args) {\n\
                     System.out.println(\"greeting=\" + System.getProperty(\"greeting\"));\n\
                     System.out.println(\"extra=\" + System.getProperty(\"extra\"));\n\
                     System.out.println(\"args=\" + String.join(\"|\", args));\n\
                     System.out.println(\"dep=\" + dep.Dep.message());\n\
                 }\n\
             }\n",
        );
        let javac = |args: &[&Path]| {
            let status = std::process::Command::new(&tc.javac)
                .args(args)
                .status()
                .unwrap();
            assert!(status.success(), "javac failed");
        };
        let dep_classes = tree.root.join("dep-classes");
        let app_classes = tree.root.join("app-classes");
        javac(&[
            Path::new("-d"),
            &dep_classes,
            &tree.root.join("src/dep/Dep.java"),
        ]);
        javac(&[
            Path::new("-cp"),
            &dep_classes,
            Path::new("-d"),
            &app_classes,
            &tree.root.join("src/app/Main.java"),
        ]);

        let lib = tree.root.join("dist/lib");
        write_thin_jar(&dep_classes, &lib.join("dep.jar"), &JarManifest::default()).unwrap();
        let jar = tree.root.join("dist/app.jar");
        write_thin_jar(
            &app_classes,
            &jar,
            &JarManifest {
                main_class: Some("app.Main".into()),
                class_path: vec!["lib/dep.jar".into()],
                ..JarManifest::default()
            },
        )
        .unwrap();
        Fixture { tree, jar, lib }
    }

    fn run(program: &Path, args: &[&str], java_opts: &str) -> String {
        let output = std::process::Command::new(program)
            .args(args)
            .env("JAVA_OPTS", java_opts)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{} failed:\n{}",
            program.display(),
            String::from_utf8_lossy(&output.stderr)
        );
        // println ends lines with \r\n on Windows.
        String::from_utf8(output.stdout)
            .unwrap()
            .replace("\r\n", "\n")
    }

    #[test]
    fn jdeps_sees_what_the_dependencies_need() {
        let Some(tc) = jdk("jdeps_sees_what_the_dependencies_need") else {
            return;
        };
        let f = fixture(&tc, "jdeps");
        let ui = quiet_ui();

        let app_only = modules(&tc, std::slice::from_ref(&f.jar), tc.version, &[], &ui).unwrap();
        assert_eq!(app_only, strings(&["java.base"]));

        let app = App {
            name: "demo",
            version: "1.0",
            main_class: Some("app.Main"),
            jar: &f.jar,
            lib_dir: Some(&f.lib),
            jvm_args: &[],
            java_agents: &[],
            agent_jars: &[],
        };
        let all = modules(
            &tc,
            &app.jars().unwrap(),
            tc.version,
            &strings(&["java.sql"]),
            &ui,
        )
        .unwrap();
        assert!(all.contains(&"java.logging".to_string()), "{all:?}");
        assert!(all.contains(&"java.sql".to_string()), "{all:?}");
        let mut sorted = all.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(all, sorted);

        let missing = modules(&tc, &[f.tree.root.join("nope.jar")], tc.version, &[], &ui);
        assert!(missing.unwrap_err().to_string().contains("nope.jar"));
    }

    #[test]
    fn jlink_builds_a_runnable_trimmed_image() {
        let Some(tc) = jdk("jlink_builds_a_runnable_trimmed_image") else {
            return;
        };
        let f = fixture(&tc, "jlink");
        let ui = quiet_ui();
        let jvm_args = strings(&["-Dgreeting=it's a \"test\" $HOME"]);
        let app = App {
            name: "demo",
            version: "1.0",
            main_class: Some("app.Main"),
            jar: &f.jar,
            lib_dir: Some(&f.lib),
            jvm_args: &jvm_args,
            java_agents: &[],
            agent_jars: &[],
        };
        let mods = modules(&tc, &app.jars().unwrap(), tc.version, &[], &ui).unwrap();
        let output = f.tree.root.join("image");

        let image = jlink(&tc, &app, &mods, &output, &ui).unwrap();
        assert_eq!(image.path, output);
        assert_eq!(image.modules, mods);
        assert!(image.bytes > 1_000_000, "{} bytes", image.bytes);
        assert!(output.join("app/app.jar").is_file());
        assert!(output.join("app/lib/dep.jar").is_file());
        assert!(output.join("bin/demo.bat").is_file());

        // The image's own java runs the app: the relative Class-Path resolves,
        // and the dependency's module made it into the trimmed runtime.
        let java = output
            .join("bin")
            .join(if cfg!(windows) { "java.exe" } else { "java" });
        let jar = output.join("app/app.jar");
        let out = run(&java, &["-jar", jar.to_str().unwrap(), "x"], "");
        assert!(out.contains("dep=dep\n"), "{out}");
        assert!(out.contains("args=x\n"), "{out}");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let launcher = output.join("bin/demo");
            let mode = std::fs::metadata(&launcher).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o755);
            let out = run(&launcher, &["one", "two words"], "-Dextra=yes");
            assert_eq!(
                out,
                "greeting=it's a \"test\" $HOME\n\
                 extra=yes\n\
                 args=one|two words\n\
                 dep=dep\n"
            );
        }

        // A second run replaces the first image rather than tripping jlink's
        // refusal to write into an existing directory.
        jlink(&tc, &app, &mods, &output, &ui).unwrap();
        assert!(output.join("app/app.jar").is_file());

        // A project named after one of the runtime's launchers is refused.
        let clash = App {
            name: "java",
            ..app
        };
        let err = jlink(&tc, &clash, &mods, &output, &ui).unwrap_err();
        assert!(err.to_string().contains("bin"), "{err}");
    }

    #[test]
    fn jpackage_produces_an_app_image() {
        let Some(tc) = jdk("jpackage_produces_an_app_image") else {
            return;
        };
        if let Err(e) = tool(&tc, "jpackage") {
            eprintln!("SKIPPED jpackage_produces_an_app_image: {e}");
            return;
        }
        let f = fixture(&tc, "jpackage");
        let ui = quiet_ui();
        let jvm_args = strings(&["-Dgreeting=hi"]);
        let app = App {
            name: "demo",
            // macOS refuses an app version starting with 0.
            version: "1.2.3-SNAPSHOT",
            main_class: Some("app.Main"),
            jar: &f.jar,
            lib_dir: Some(&f.lib),
            jvm_args: &jvm_args,
            java_agents: &[],
            agent_jars: &[],
        };
        let mods = modules(&tc, &app.jars().unwrap(), tc.version, &[], &ui).unwrap();
        let dest = f.tree.root.join("installers");
        let work = f.tree.root.join(".jrs");

        let produced = jpackage(&tc, &app, &mods, Some("app-image"), &dest, &work, &ui).unwrap();
        assert!(produced.exists(), "{}", produced.display());
        assert!(produced.starts_with(&dest));
        let name = produced.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("demo"), "{name}");
        assert!(work.join("jpackage-input/lib/dep.jar").is_file());
    }
}
