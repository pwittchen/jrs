//! Finding and running the JDK.
//!
//! jrs is a driver over `javac`, `java` and `jar`, not a reimplementation of them
//! (SPEC §6.2). This module locates those three binaries once, probes the
//! compiler's version, and provides the two ways jrs ever runs a subprocess:
//! captured (so the live region can be torn down before diagnostics appear) and
//! inherited (so an interactive program owns the terminal).

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

use crate::error::{JrsError, Result};
use crate::resolve::coord::compare_versions;
use crate::ui::{Stream, Ui};

/// The lowest release jrs is willing to drive. `--release` needs JDK 9+, and 17
/// is the oldest LTS still worth targeting.
pub const MINIMUM_JDK: u32 = 17;

#[derive(Debug, Clone)]
pub struct Toolchain {
    pub javac: PathBuf,
    pub java: PathBuf,
    pub jar: PathBuf,
    /// Feature version of `javac`: 21, 23, ...
    pub version: u32,
    /// Where it was found, for error messages.
    pub home: Option<PathBuf>,
}

static DISCOVERED: OnceLock<std::result::Result<Toolchain, String>> = OnceLock::new();

impl Toolchain {
    /// Locate the JDK, caching the result for the process lifetime (SPEC §7.1).
    ///
    /// # Errors
    ///
    /// [`JrsError::Toolchain`] if `javac` can't be found at `JAVA_HOME` or on
    /// `PATH`, `java` or `jar` is missing beside it, its version can't be read,
    /// or it is older than [`MINIMUM_JDK`].
    pub fn discover() -> Result<Toolchain> {
        DISCOVERED
            .get_or_init(|| Toolchain::probe().map_err(|e| e.to_string()))
            .clone()
            .map_err(JrsError::Toolchain)
    }

    /// The JDK a project builds with: the one it pins, when it pins one, and
    /// [`Toolchain::discover`]'s otherwise.
    ///
    /// A pinned version is looked for in the user's `[jdks]` table first, then
    /// at `JAVA_HOME` / `PATH`, then among the JDKs installed where installers
    /// and version managers put them. When several patch releases of it are
    /// installed, the newest wins.
    ///
    /// # Errors
    ///
    /// [`JrsError::Toolchain`] if the configured home for the pinned version is
    /// not a JDK or is another version, if the pinned version is installed
    /// nowhere jrs looks, and, without a pin, whatever [`Toolchain::discover`]
    /// returns.
    pub fn select(pin: Option<&JdkPin>, configured: &BTreeMap<u32, PathBuf>) -> Result<Toolchain> {
        let Some(pin) = pin else {
            return Toolchain::discover();
        };
        if let Some(home) = configured.get(&pin.version) {
            let toolchain = Toolchain::probe_home(home)?;
            if toolchain.version != pin.version {
                return Err(JrsError::toolchain(format!(
                    "the jrs config file says JDK {} is at {}, but that is JDK {}",
                    pin.version,
                    home.display(),
                    toolchain.version
                )));
            }
            return Ok(toolchain);
        }
        let default = Toolchain::discover().ok();
        if let Some(toolchain) = &default
            && toolchain.version == pin.version
        {
            return Ok(toolchain.clone());
        }

        let installed = installed_jdks();
        let best = installed
            .iter()
            .filter(|(v, _)| feature_version(v) == Some(pin.version))
            .max_by(|a, b| compare_versions(&a.0, &b.0));
        if let Some((_, home)) = best {
            return Toolchain::probe_home(home);
        }

        let mut versions: Vec<u32> = installed
            .iter()
            .filter_map(|(v, _)| feature_version(v))
            .chain(default.as_ref().map(|t| t.version))
            .collect();
        versions.sort_unstable();
        versions.dedup();
        let seen = if versions.is_empty() {
            "found no JDK at all".to_string()
        } else {
            let list: Vec<String> = versions.iter().map(u32::to_string).collect();
            format!("found JDK {}", list.join(", "))
        };
        Err(JrsError::toolchain(format!(
            "{} pins JDK {v}, but none is installed where jrs looks ({seen})\n\n\
             install JDK {v}, or say where it is in {}:\n\n    [jdks]\n    {v} = \"/path/to/jdk-{v}\"",
            pin.from,
            crate::config::default_path().map_or_else(
                || "the jrs config file".to_string(),
                |p| p.display().to_string()
            ),
            v = pin.version,
        )))
    }

    /// Another JDK tool — `javadoc`, `jdeps`, `jlink`, `jpackage` — beside
    /// `javac`, or an error naming the JDK that lacks it.
    ///
    /// # Errors
    ///
    /// [`JrsError::Toolchain`] if there is no `name` beside `javac`.
    pub fn tool(&self, name: &str) -> Result<PathBuf> {
        let path = self
            .javac
            .parent()
            .unwrap_or(Path::new("."))
            .join(exe(name));
        if path.is_file() {
            return Ok(path);
        }
        Err(JrsError::toolchain(format!(
            "the JDK at {} has no `{name}`\n\nsome distributions leave it out; \
             install a full JDK",
            self.javac
                .parent()
                .and_then(Path::parent)
                .unwrap_or(Path::new("."))
                .display()
        )))
    }

    fn probe_home(home: &Path) -> Result<Toolchain> {
        let javac = home.join("bin").join(exe("javac"));
        if !javac.is_file() {
            return Err(JrsError::toolchain(format!(
                "{} is not a JDK: there is no {}",
                home.display(),
                javac.display()
            )));
        }
        Toolchain::probe_javac(javac, Some(home.to_path_buf()))
    }

    fn probe() -> Result<Toolchain> {
        let (javac, home) = match std::env::var_os("JAVA_HOME") {
            Some(home) if !home.is_empty() => {
                let home = PathBuf::from(home);
                let candidate = home.join("bin").join(exe("javac"));
                if !candidate.is_file() {
                    return Err(JrsError::toolchain(format!(
                        "JAVA_HOME is set to {} but {} does not exist\n\n\
                         unset JAVA_HOME to fall back to PATH, or point it at a JDK \
                         (not a JRE)",
                        home.display(),
                        candidate.display()
                    )));
                }
                (candidate, Some(home))
            }
            _ => {
                let found = which("javac").ok_or_else(|| {
                    JrsError::toolchain(
                        "no `javac` on PATH and JAVA_HOME is not set\n\n\
                         install a JDK (17 or newer) and make sure `javac -version` works",
                    )
                })?;
                let home = found
                    .parent()
                    .and_then(|b| b.parent())
                    .map(Path::to_path_buf);
                (found, home)
            }
        };
        Toolchain::probe_javac(javac, home)
    }

    fn probe_javac(javac: PathBuf, home: Option<PathBuf>) -> Result<Toolchain> {
        let bin = javac.parent().unwrap_or(Path::new("."));
        let java = bin.join(exe("java"));
        let jar = bin.join(exe("jar"));
        for (name, path) in [("java", &java), ("jar", &jar)] {
            if !path.is_file() {
                return Err(JrsError::toolchain(format!(
                    "found `javac` at {} but no `{name}` beside it\n\n\
                     that usually means a partial JDK installation",
                    javac.display()
                )));
            }
        }

        let output = Command::new(&javac)
            .arg("-version")
            .output()
            .map_err(|e| JrsError::toolchain(format!("could not run {}: {e}", javac.display())))?;
        // Older JDKs print the version to stderr, newer ones to stdout.
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let version = parse_javac_version(&text).ok_or_else(|| {
            JrsError::toolchain(format!(
                "could not read a version out of `{} -version`:\n\n{}",
                javac.display(),
                text.trim()
            ))
        })?;
        if version < MINIMUM_JDK {
            return Err(JrsError::toolchain(format!(
                "found JDK {version} at {}, but jrs needs {MINIMUM_JDK} or newer",
                javac.display()
            )));
        }

        Ok(Toolchain {
            javac,
            java,
            jar,
            version,
            home,
        })
    }

    /// The `--release` to compile against: the manifest's, or this JDK's.
    ///
    /// # Errors
    ///
    /// [`JrsError::Toolchain`] if `requested` is newer than this JDK.
    pub fn release(&self, requested: Option<u32>) -> Result<u32> {
        match requested {
            None => Ok(self.version),
            Some(r) if r <= self.version => Ok(r),
            Some(r) => Err(JrsError::toolchain(format!(
                "jrs.toml asks for Java {r}, but the JDK at {} is {}\n\n\
                 install a newer JDK, or lower `java.source`",
                self.javac.display(),
                self.version
            ))),
        }
    }

    /// The classpath separator: `:` everywhere but Windows.
    #[must_use]
    pub fn classpath_separator() -> &'static str {
        if cfg!(windows) { ";" } else { ":" }
    }

    /// Join paths into one `-cp` argument.
    #[must_use]
    pub fn classpath(entries: &[PathBuf]) -> String {
        entries
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(Toolchain::classpath_separator())
    }
}

/// Parse `javac 23.0.2`, `javac 21`, or the pre-9 `javac 1.8.0_292`.
#[must_use]
pub fn parse_javac_version(text: &str) -> Option<u32> {
    let token = text
        .split_whitespace()
        .find(|t| t.chars().next().is_some_and(|c| c.is_ascii_digit()))?;
    let mut parts = token.split(['.', '_', '-', '+']);
    let first: u32 = parts.next()?.parse().ok()?;
    if first == 1 {
        // `1.8.0` means 8.
        parts.next()?.parse().ok()
    } else {
        Some(first)
    }
}

/// A JDK feature version a project pins, and what pinned it — for messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JdkPin {
    pub version: u32,
    pub from: String,
}

/// The JDK a project pins: `java.jdk` in the manifest, else `.java-version`
/// (jenv, asdf, mise), else `.sdkmanrc` (SDKMAN!) in the project root.
pub fn project_pin(manifest_jdk: Option<u32>, root: &Path) -> Option<JdkPin> {
    let pin = |version, from: &str| {
        Some(JdkPin {
            version,
            from: from.to_string(),
        })
    };
    if let Some(v) = manifest_jdk {
        return pin(v, "`java.jdk` in jrs.toml");
    }
    if let Ok(text) = std::fs::read_to_string(root.join(".java-version"))
        && let Some(v) = text
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty() && !l.starts_with('#'))
            .and_then(pinned_feature)
    {
        return pin(v, ".java-version");
    }
    if let Ok(text) = std::fs::read_to_string(root.join(".sdkmanrc"))
        && let Some(v) = text
            .lines()
            .find_map(|l| l.trim().strip_prefix("java="))
            .and_then(|v| pinned_feature(v.trim()))
    {
        return pin(v, ".sdkmanrc");
    }
    None
}

/// The feature version in a version manager's spelling of a JDK: `21`,
/// `21.0.2`, `temurin-21.0.2+13`, `21.0.2-tem`, `corretto-17`, `1.8`.
#[must_use]
pub fn pinned_feature(spec: &str) -> Option<u32> {
    let token = spec
        .split(['-', '_', '+'])
        .find(|t| t.starts_with(|c: char| c.is_ascii_digit()))?;
    feature_version(token)
}

/// `21.0.5` → 21, `1.8.0_432` → 8.
fn feature_version(version: &str) -> Option<u32> {
    let mut parts = version.split(['.', '_']);
    let first: u32 = parts.next()?.parse().ok()?;
    if first == 1 {
        parts.next()?.parse().ok()
    } else {
        Some(first)
    }
}

/// Every JDK installed where installers and version managers put them, as
/// (`JAVA_VERSION` from its `release` file, home).
///
/// The `release` file is read rather than `javac -version` run, so looking at
/// a dozen installed JDKs costs a dozen small reads, not a dozen JVM starts.
fn installed_jdks() -> Vec<(String, PathBuf)> {
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
        let home = PathBuf::from(home);
        for relative in [
            ".sdkman/candidates/java",
            ".jdks",
            ".asdf/installs/java",
            ".local/share/mise/installs/java",
            ".gradle/jdks",
            "Library/Java/JavaVirtualMachines",
        ] {
            roots.push(home.join(relative));
        }
    }
    for dir in [
        "/Library/Java/JavaVirtualMachines",
        "/usr/lib/jvm",
        "/usr/java",
        "/opt/java",
    ] {
        roots.push(PathBuf::from(dir));
    }
    if cfg!(windows) {
        for var in ["ProgramFiles", "ProgramW6432"] {
            if let Some(dir) = std::env::var_os(var) {
                for vendor in [
                    "Java",
                    "Eclipse Adoptium",
                    "Microsoft",
                    "Zulu",
                    "Amazon Corretto",
                    "BellSoft",
                ] {
                    roots.push(PathBuf::from(&dir).join(vendor));
                }
            }
        }
    }

    // `actions/setup-java` exports `JAVA_HOME_<version>_<arch>` for every JDK
    // it installs, which is how a CI matrix gets several side by side.
    let mut homes: Vec<PathBuf> = std::env::vars_os()
        .filter(|(k, _)| k.to_string_lossy().starts_with("JAVA_HOME_"))
        .map(|(_, v)| PathBuf::from(v))
        .collect();
    homes.sort();
    for root in roots {
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        let mut dirs: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        dirs.sort();
        for dir in dirs {
            // macOS bundles keep the JDK under Contents/Home.
            homes.push(dir.join("Contents").join("Home"));
            homes.push(dir);
        }
    }

    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for home in homes {
        if !home.join("bin").join(exe("javac")).is_file() {
            continue;
        }
        // SDKMAN!'s `current` is a link to one of its siblings.
        if !seen.insert(home.canonicalize().unwrap_or_else(|_| home.clone())) {
            continue;
        }
        if let Some(version) = release_version(&home) {
            out.push((version, home));
        }
    }
    out
}

/// `JAVA_VERSION="21.0.5"` from a JDK's `release` file.
fn release_version(home: &Path) -> Option<String> {
    let text = std::fs::read_to_string(home.join("release")).ok()?;
    text.lines()
        .find_map(|l| l.strip_prefix("JAVA_VERSION="))
        .map(|v| v.trim().trim_matches('"').to_string())
        .filter(|v| !v.is_empty())
}

fn exe(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

/// Find an executable on `PATH`, so errors can name an absolute path.
#[must_use]
pub fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(exe(name)))
        .find(|candidate| candidate.is_file())
}

// ---- running subprocesses --------------------------------------------------

#[derive(Debug)]
pub struct CapturedOutput {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

impl CapturedOutput {
    #[must_use]
    pub fn ok(&self) -> bool {
        self.status == 0
    }
}

fn describe(program: &Path, args: &[impl AsRef<OsStr>]) -> String {
    let mut s = program.display().to_string();
    for a in args {
        s.push(' ');
        s.push_str(&a.as_ref().to_string_lossy());
    }
    s
}

/// Run a tool and capture its output.
///
/// Capturing is what lets the live region come down before a `javac` diagnostic
/// is printed (SPEC §5.3.8); the caller replays the output verbatim afterwards.
///
/// # Errors
///
/// [`JrsError::Build`] if `program` cannot be started. A non-zero exit is not an
/// error: it is in [`CapturedOutput::status`].
pub fn run_captured(ui: &Ui, program: &Path, args: &[impl AsRef<OsStr>]) -> Result<CapturedOutput> {
    ui.verbose(describe(program, args));
    let output = Command::new(program)
        .args(args.iter().map(AsRef::as_ref))
        .stdin(Stdio::null())
        .output()
        .map_err(|e| JrsError::build(format!("could not run {}: {e}", program.display())))?;
    let status = output.status.code().unwrap_or(-1);
    ui.verbose(format!("{} exited with {status}", program.display()));
    Ok(CapturedOutput {
        status,
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

/// Run a tool with the terminal handed straight to it.
///
/// Used for `jrs run`: the user's program owns stdin and stdout, and jrs is not
/// in the middle of them. The live region is torn down first.
///
/// # Errors
///
/// [`JrsError::Build`] if `program` cannot be started. A non-zero exit is not an
/// error: the code is returned.
pub fn run_inherited(ui: &Ui, program: &Path, args: &[impl AsRef<OsStr>]) -> Result<i32> {
    run_inherited_in(ui, program, args, &Environment::default())
}

/// Where a JVM started for the user runs, and what it adds to the environment
/// it inherits from jrs: `run.cwd`, `run.env` and `test.env`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Environment {
    /// `None` keeps jrs's own working directory.
    pub cwd: Option<PathBuf>,
    /// Set on top of the inherited environment, in order.
    pub vars: Vec<(String, String)>,
}

impl Environment {
    fn apply(&self, ui: &Ui, command: &mut Command) {
        if let Some(cwd) = &self.cwd {
            ui.verbose(format!("in {}", cwd.display()));
            command.current_dir(cwd);
        }
        for (key, value) in &self.vars {
            ui.verbose(format!("with {key}={value}"));
            command.env(key, value);
        }
    }
}

/// [`run_inherited`], in `environment`.
///
/// # Errors
///
/// As for [`run_inherited`].
pub fn run_inherited_in(
    ui: &Ui,
    program: &Path,
    args: &[impl AsRef<OsStr>],
    environment: &Environment,
) -> Result<i32> {
    ui.verbose(describe(program, args));
    ui.suspend();
    let mut command = Command::new(program);
    environment.apply(ui, &mut command);
    let status = command
        .args(args.iter().map(AsRef::as_ref))
        .status()
        .map_err(|e| JrsError::build(format!("could not run {}: {e}", program.display())))?;
    let code = status.code().unwrap_or(-1);
    ui.verbose(format!("{} exited with {code}", program.display()));
    Ok(code)
}

/// Run a tool, streaming each stdout line through the UI as it arrives.
///
/// The callback sees every line before it is printed, which is how `jrs test`
/// keeps a live counter without ever writing to the terminal itself.
///
/// # Errors
///
/// [`JrsError::Build`] if `program` cannot be started or waited for. A non-zero
/// exit is not an error: the code is returned.
pub fn run_streaming(
    ui: &Ui,
    program: &Path,
    args: &[impl AsRef<OsStr>],
    on_line: impl FnMut(&str),
) -> Result<i32> {
    run_streaming_in(ui, program, args, &Environment::default(), on_line)
}

/// [`run_streaming`], in `environment`.
///
/// # Errors
///
/// As for [`run_streaming`].
pub fn run_streaming_in(
    ui: &Ui,
    program: &Path,
    args: &[impl AsRef<OsStr>],
    environment: &Environment,
    mut on_line: impl FnMut(&str),
) -> Result<i32> {
    use std::io::BufRead;

    ui.verbose(describe(program, args));
    let mut command = Command::new(program);
    environment.apply(ui, &mut command);
    let mut child = command
        .args(args.iter().map(AsRef::as_ref))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| JrsError::build(format!("could not run {}: {e}", program.display())))?;

    // stderr is drained on its own thread so a chatty tool cannot deadlock on a
    // full pipe while we are reading stdout.
    let stderr = child.stderr.take();
    let drain = std::thread::spawn(move || {
        let mut buf = String::new();
        if let Some(mut e) = stderr {
            let _ = std::io::Read::read_to_string(&mut e, &mut buf);
        }
        buf
    });

    if let Some(stdout) = child.stdout.take() {
        for line in std::io::BufReader::new(stdout).lines() {
            let line = line.unwrap_or_default();
            on_line(&line);
            ui.println_out(&line);
        }
    }

    let status = child
        .wait()
        .map_err(|e| JrsError::build(format!("{} did not finish: {e}", program.display())))?;
    let errors = drain.join().unwrap_or_default();
    if !errors.trim().is_empty() {
        ui.passthrough(Stream::Err, errors.trim_end());
    }
    let code = status.code().unwrap_or(-1);
    ui.verbose(format!("{} exited with {code}", program.display()));
    Ok(code)
}

/// [`run_streaming_in`], except that `on_line` may stop the process: a
/// `Break` kills it, and the line that asked for that is not passed through.
/// The second value says whether that happened; the exit code of a killed
/// process is whatever the platform reports for one.
///
/// # Errors
///
/// [`JrsError::Build`] if the program cannot be started or waited for.
pub fn run_streaming_until(
    ui: &Ui,
    program: &Path,
    args: &[impl AsRef<OsStr>],
    environment: &Environment,
    mut on_line: impl FnMut(&str) -> std::ops::ControlFlow<()>,
) -> Result<(i32, bool)> {
    use std::io::BufRead;

    ui.verbose(describe(program, args));
    let mut command = Command::new(program);
    environment.apply(ui, &mut command);
    let mut child = command
        .args(args.iter().map(AsRef::as_ref))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| JrsError::build(format!("could not run {}: {e}", program.display())))?;

    // As in `run_streaming`: stderr drains on its own thread.
    let stderr = child.stderr.take();
    let drain = std::thread::spawn(move || {
        let mut buf = String::new();
        if let Some(mut e) = stderr {
            let _ = std::io::Read::read_to_string(&mut e, &mut buf);
        }
        buf
    });

    let mut stopped = false;
    if let Some(stdout) = child.stdout.take() {
        for line in std::io::BufReader::new(stdout).lines() {
            let line = line.unwrap_or_default();
            if on_line(&line).is_break() {
                stopped = true;
                // It may have exited on its own in the meantime; either way
                // it is gone once `wait` returns.
                let _ = child.kill();
                break;
            }
            ui.println_out(&line);
        }
    }

    let status = child
        .wait()
        .map_err(|e| JrsError::build(format!("{} did not finish: {e}", program.display())))?;
    let errors = drain.join().unwrap_or_default();
    if !errors.trim().is_empty() {
        ui.passthrough(Stream::Err, errors.trim_end());
    }
    let code = status.code().unwrap_or(-1);
    if stopped {
        ui.verbose(format!("{} stopped by jrs", program.display()));
    } else {
        ui.verbose(format!("{} exited with {code}", program.display()));
    }
    Ok((code, stopped))
}

// ---- running tasks ---------------------------------------------------------

/// How a task's process starts (TASKS.md §4.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Launch {
    /// A program and its arguments, with no shell in between.
    Exec { program: PathBuf, args: Vec<String> },
    /// A string for `sh -c` on Unix and `cmd /C` on Windows; `args` become its
    /// positional parameters.
    Shell { script: String, args: Vec<String> },
}

impl Launch {
    /// The command line, for `--verbose`.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Launch::Exec { program, args } => describe(program, args),
            Launch::Shell { script, args } => {
                let shell = if cfg!(windows) { "cmd /C" } else { "sh -c" };
                let mut s = format!("{shell} {script}");
                for a in args {
                    s.push(' ');
                    s.push_str(a);
                }
                s
            }
        }
    }
}

/// One task process: what to start, where, and what to add to the inherited
/// environment.
#[derive(Debug, Clone, Copy)]
pub struct TaskProcess<'a> {
    pub name: &'a str,
    pub launch: &'a Launch,
    pub cwd: &'a Path,
    pub env: &'a [(String, std::ffi::OsString)],
}

impl TaskProcess<'_> {
    fn command(&self) -> Command {
        let mut command = match self.launch {
            Launch::Exec { program, args } => {
                let mut c = Command::new(program);
                c.args(args);
                c
            }
            Launch::Shell { script, args } => shell_command(self.name, script, args),
        };
        command.current_dir(self.cwd);
        for (key, value) in self.env {
            command.env(key, value);
        }
        command
    }

    fn start_error(&self, e: &std::io::Error) -> JrsError {
        let program = match self.launch {
            Launch::Exec { program, .. } => program.display().to_string(),
            Launch::Shell { .. } => (if cfg!(windows) { "cmd" } else { "sh" }).to_string(),
        };
        JrsError::build(format!(
            "could not start task `{}`: {program}: {e}",
            self.name
        ))
    }
}

#[cfg(not(windows))]
fn shell_command(name: &str, script: &str, args: &[String]) -> Command {
    // `$0` is the task's name, so the arguments are `$1`, `$2`, ... as they
    // would be in a script.
    let mut c = Command::new("sh");
    c.arg("-c").arg(script).arg(name).args(args);
    c
}

#[cfg(windows)]
fn shell_command(_name: &str, script: &str, args: &[String]) -> Command {
    use std::os::windows::process::CommandExt;
    // `cmd` parses its own command line, so the string goes to it untouched:
    // its quoting rules apply, not the ones Rust uses for other programs.
    let mut line = script.to_string();
    for a in args {
        line.push(' ');
        if a.is_empty() || a.contains([' ', '\t', '"']) {
            line.push('"');
            line.push_str(&a.replace('"', "\"\""));
            line.push('"');
        } else {
            line.push_str(a);
        }
    }
    let mut c = Command::new("cmd");
    c.args(["/D", "/S", "/C"]).raw_arg(format!("\"{line}\""));
    c
}

/// Find `name` in the directories of `path`, the way the OS would, so that a
/// missing program can be reported with where jrs looked.
#[must_use]
pub fn find_program(name: &str, path: &OsStr) -> Option<PathBuf> {
    let candidates: Vec<String> = if cfg!(windows) && Path::new(name).extension().is_none() {
        let extensions =
            std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string());
        extensions
            .split(';')
            .filter(|e| !e.is_empty())
            .map(|e| format!("{name}{}", e.to_ascii_lowercase()))
            .collect()
    } else {
        vec![name.to_string()]
    };
    std::env::split_paths(path)
        .flat_map(|dir| candidates.iter().map(move |c| dir.join(c)))
        .find(|p| is_executable(p))
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

/// Run a task a hook or a `depends-on` reached: stdin closed, and stdout and
/// stderr both forwarded to jrs's stderr line by line as they arrive, since
/// stdout belongs to the command's own output (TASKS.md §9.3).
///
/// # Errors
///
/// [`JrsError::Build`] if the process cannot be started or waited for. A
/// non-zero exit is not an error: the code is returned.
pub fn run_task_streamed(ui: &Ui, process: &TaskProcess<'_>) -> Result<i32> {
    use std::io::BufRead;

    let mut command = process.command();
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().map_err(|e| process.start_error(&e))?;

    // stderr gets its own thread, as in `run_streaming`, so that a full pipe
    // on one stream cannot stall the other.
    let stderr = child.stderr.take();
    let forward = ui.clone();
    let drain = std::thread::spawn(move || {
        if let Some(e) = stderr {
            for line in std::io::BufReader::new(e).split(b'\n') {
                let Ok(line) = line else { break };
                forward.passthrough_line(Stream::Err, &text_line(&line));
            }
        }
    });
    if let Some(stdout) = child.stdout.take() {
        for line in std::io::BufReader::new(stdout).split(b'\n') {
            let Ok(line) = line else { break };
            ui.passthrough_line(Stream::Err, &text_line(&line));
        }
    }

    let status = child
        .wait()
        .map_err(|e| JrsError::build(format!("task `{}` did not finish: {e}", process.name)))?;
    let _ = drain.join();
    let code = status.code().unwrap_or(-1);
    ui.verbose(format!("task `{}` exited with {code}", process.name));
    Ok(code)
}

/// Run the task named on `jrs task <name>` with the terminal handed to it, as
/// `jrs run` does for the program.
///
/// # Errors
///
/// [`JrsError::Build`] if the process cannot be started. A non-zero exit is
/// not an error: the code is returned.
pub fn run_task_inherited(ui: &Ui, process: &TaskProcess<'_>) -> Result<i32> {
    ui.suspend();
    let status = process
        .command()
        .status()
        .map_err(|e| process.start_error(&e))?;
    let code = status.code().unwrap_or(-1);
    ui.verbose(format!("task `{}` exited with {code}", process.name));
    Ok(code)
}

/// One line of output, without its `\r` on Windows.
fn text_line(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    text.strip_suffix('\r').unwrap_or(&text).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn javac_versions_parse() {
        assert_eq!(parse_javac_version("javac 23.0.2"), Some(23));
        assert_eq!(parse_javac_version("javac 21"), Some(21));
        assert_eq!(parse_javac_version("javac 17.0.9+9"), Some(17));
        assert_eq!(parse_javac_version("javac 1.8.0_292"), Some(8));
        assert_eq!(parse_javac_version("javac 11.0.2\n"), Some(11));
        assert_eq!(parse_javac_version("command not found"), None);
    }

    #[test]
    fn the_requested_release_must_fit_the_jdk() {
        let tc = Toolchain {
            javac: PathBuf::from("/jdk/bin/javac"),
            java: PathBuf::from("/jdk/bin/java"),
            jar: PathBuf::from("/jdk/bin/jar"),
            version: 21,
            home: None,
        };
        assert_eq!(tc.release(None).unwrap(), 21);
        assert_eq!(tc.release(Some(17)).unwrap(), 17);
        assert_eq!(tc.release(Some(21)).unwrap(), 21);

        let err = tc.release(Some(25)).unwrap_err().to_string();
        assert!(err.contains("asks for Java 25"), "{err}");
        assert!(err.contains("java.source"), "{err}");
    }

    #[test]
    fn classpaths_use_the_platform_separator() {
        let entries = vec![PathBuf::from("/a.jar"), PathBuf::from("/b.jar")];
        let joined = Toolchain::classpath(&entries);
        assert!(joined.contains(Toolchain::classpath_separator()));
        assert_eq!(joined.split(Toolchain::classpath_separator()).count(), 2);
    }

    #[test]
    fn an_empty_classpath_is_an_empty_string() {
        assert_eq!(Toolchain::classpath(&[]), "");
    }

    #[test]
    fn version_manager_spellings_name_a_feature_version() {
        assert_eq!(pinned_feature("21"), Some(21));
        assert_eq!(pinned_feature("21.0.2"), Some(21));
        assert_eq!(pinned_feature("temurin-21.0.2+13"), Some(21));
        assert_eq!(pinned_feature("21.0.2-tem"), Some(21));
        assert_eq!(pinned_feature("corretto-17"), Some(17));
        assert_eq!(pinned_feature("openjdk64-17.0.2"), Some(17));
        assert_eq!(pinned_feature("1.8"), Some(8));
        assert_eq!(pinned_feature("8.0.432-zulu"), Some(8));
        assert_eq!(pinned_feature("system"), None);
    }

    #[test]
    fn a_project_pins_its_jdk_in_the_manifest_or_beside_it() {
        let dir = std::env::temp_dir().join(format!("jrs-pin-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(project_pin(None, &dir), None);

        std::fs::write(dir.join(".sdkmanrc"), "# sdkman\njava=17.0.14-zulu\n").unwrap();
        assert_eq!(project_pin(None, &dir).unwrap().version, 17);
        assert_eq!(project_pin(None, &dir).unwrap().from, ".sdkmanrc");

        std::fs::write(dir.join(".java-version"), "temurin-21\n").unwrap();
        assert_eq!(project_pin(None, &dir).unwrap().version, 21);

        let manifest = project_pin(Some(25), &dir).unwrap();
        assert_eq!(manifest.version, 25);
        assert!(manifest.from.contains("java.jdk"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_jdk_release_file_names_its_version() {
        let dir = std::env::temp_dir().join(format!("jrs-release-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("release"),
            "IMPLEMENTOR=\"Eclipse Adoptium\"\nJAVA_VERSION=\"21.0.5\"\n",
        )
        .unwrap();
        assert_eq!(release_version(&dir).as_deref(), Some("21.0.5"));
        assert_eq!(feature_version("21.0.5"), Some(21));
        assert_eq!(feature_version("1.8.0_432"), Some(8));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_pinned_jdk_that_is_not_installed_says_where_it_goes() {
        let pin = JdkPin {
            version: 3,
            from: ".java-version".into(),
        };
        let err = Toolchain::select(Some(&pin), &BTreeMap::new())
            .unwrap_err()
            .to_string();
        assert!(err.contains(".java-version pins JDK 3"), "{err}");
        assert!(err.contains("[jdks]"), "{err}");
    }

    #[test]
    fn a_configured_home_must_be_the_version_it_claims() {
        let Ok(default) = Toolchain::discover() else {
            eprintln!("SKIPPED toolchain::a_configured_home_must_be_the_version_it_claims: no JDK");
            return;
        };
        let Some(home) = default.javac.parent().and_then(Path::parent) else {
            return;
        };
        let wrong = default.version + 1;
        let configured: BTreeMap<u32, PathBuf> = [(wrong, home.to_path_buf())].into();
        let pin = JdkPin {
            version: wrong,
            from: "`java.jdk` in jrs.toml".into(),
        };
        let err = Toolchain::select(Some(&pin), &configured)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains(&format!("that is JDK {}", default.version)),
            "{err}"
        );

        let right: BTreeMap<u32, PathBuf> = [(default.version, home.to_path_buf())].into();
        let pin = JdkPin {
            version: default.version,
            from: "test".into(),
        };
        assert_eq!(
            Toolchain::select(Some(&pin), &right).unwrap().version,
            default.version
        );
        assert!(default.tool("javadoc").is_ok() || default.tool("javadoc").is_err());
        assert!(
            default
                .tool("no-such-tool")
                .unwrap_err()
                .to_string()
                .contains("no-such-tool")
        );
    }
}
