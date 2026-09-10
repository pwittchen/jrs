//! Finding and running the JDK.
//!
//! jrs is a driver over `javac`, `java` and `jar`, not a reimplementation of them
//! (SPEC §6.2). This module locates those three binaries once, probes the
//! compiler's version, and provides the two ways jrs ever runs a subprocess:
//! captured (so the live region can be torn down before diagnostics appear) and
//! inherited (so an interactive program owns the terminal).

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

use crate::error::{JrsError, Result};
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
    pub fn discover() -> Result<Toolchain> {
        DISCOVERED
            .get_or_init(|| Toolchain::probe().map_err(|e| e.to_string()))
            .clone()
            .map_err(JrsError::Toolchain)
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
    pub fn classpath_separator() -> &'static str {
        if cfg!(windows) { ";" } else { ":" }
    }

    /// Join paths into one `-cp` argument.
    pub fn classpath(entries: &[PathBuf]) -> String {
        entries
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(Toolchain::classpath_separator())
    }
}

/// Parse `javac 23.0.2`, `javac 21`, or the pre-9 `javac 1.8.0_292`.
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

fn exe(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

/// Find an executable on `PATH`, so errors can name an absolute path.
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
pub fn run_inherited(ui: &Ui, program: &Path, args: &[impl AsRef<OsStr>]) -> Result<i32> {
    ui.verbose(describe(program, args));
    ui.suspend();
    let status = Command::new(program)
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
pub fn run_streaming(
    ui: &Ui,
    program: &Path,
    args: &[impl AsRef<OsStr>],
    mut on_line: impl FnMut(&str),
) -> Result<i32> {
    use std::io::BufRead;

    ui.verbose(describe(program, args));
    let mut child = Command::new(program)
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
}
