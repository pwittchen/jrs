//! Running the user's tests through the JUnit Platform Console Launcher.
//!
//! v1 targets JUnit 5 (SPEC §10.2). The launcher is an internal dependency: jrs
//! resolves it itself, at the platform version that matches the Jupiter version
//! the user declared, and puts it last on the classpath so the user's own JUnit
//! jars win every conflict.
//!
//! The launcher's output is passed through verbatim; jrs only reads it to keep a
//! live counter, and takes its authoritative numbers from the summary block the
//! launcher prints at the end.

use std::path::PathBuf;

use crate::error::{JrsError, Result};
use crate::manifest::Manifest;
use crate::resolve::coord::{Coord, compare_versions};
use crate::toolchain::{Toolchain, run_streaming};
use crate::ui::{Live, Outcome, Ui};

pub const LAUNCHER_GROUP: &str = "org.junit.platform";
pub const LAUNCHER_ARTIFACT: &str = "junit-platform-console-standalone";
pub const LAUNCHER_MAIN: &str = "org.junit.platform.console.ConsoleLauncher";

/// JUnit Jupiter `5.X.Y` ships alongside JUnit Platform `1.X.Y`.
pub fn platform_version(jupiter: &str) -> Option<String> {
    let rest = jupiter.strip_prefix("5.")?;
    Some(format!("1.{rest}"))
}

/// Which console launcher to run.
///
/// An explicit `junit-platform-console-standalone` in `dev-dependencies` wins;
/// otherwise the version is derived from whichever Jupiter artifact is declared.
pub fn launcher_coordinate(manifest: &Manifest) -> Result<Coord> {
    if let Some(d) = manifest
        .dev_dependencies
        .iter()
        .find(|d| d.group == LAUNCHER_GROUP && d.artifact == LAUNCHER_ARTIFACT)
    {
        return Ok(Coord::new(&d.group, &d.artifact, &d.version));
    }

    const JUPITER_ARTIFACTS: &[&str] = &[
        "junit-jupiter",
        "junit-jupiter-api",
        "junit-jupiter-engine",
        "junit-jupiter-params",
    ];
    let jupiter = manifest
        .dev_dependencies
        .iter()
        .find(|d| {
            d.group == "org.junit.jupiter" && JUPITER_ARTIFACTS.contains(&d.artifact.as_str())
        })
        .ok_or_else(|| {
            JrsError::test(
                "`jrs test` needs JUnit 5\n\n\
                 add it to jrs.toml:\n\n    [dev-dependencies]\n    \
                 \"org.junit.jupiter:junit-jupiter\" = \"5.10.2\"",
            )
        })?;

    let version = platform_version(&jupiter.version).ok_or_else(|| {
        JrsError::test(format!(
            "`{}` is at version {}, which is not a JUnit 5 release\n\n\
             jrs v1 supports JUnit 5 only; declare \
             `org.junit.platform:junit-platform-console-standalone` explicitly if you \
             need a different launcher",
            jupiter.key(),
            jupiter.version
        ))
    })?;
    Ok(Coord::new(LAUNCHER_GROUP, LAUNCHER_ARTIFACT, version))
}

/// The platform release that introduced the `execute` subcommand. Invoking the
/// launcher without it is deprecated from here on, and prints a warning.
const EXECUTE_SUBCOMMAND_SINCE: &str = "1.10";

#[derive(Debug)]
pub struct TestRun {
    /// The test classpath, launcher jar last.
    pub classpath: Vec<PathBuf>,
    /// Where compiled tests live; scanned for test classes.
    pub scan_dir: PathBuf,
    /// `jrs test --filter <pattern>` maps onto `--include-classname`.
    pub filter: Option<String>,
    pub color: bool,
    pub ascii: bool,
    /// The console launcher's own version, which decides its calling convention.
    pub launcher_version: String,
}

impl TestRun {
    pub fn args(&self) -> Vec<String> {
        let mut args = vec![
            "-cp".to_string(),
            Toolchain::classpath(&self.classpath),
            LAUNCHER_MAIN.to_string(),
        ];
        if compare_versions(&self.launcher_version, EXECUTE_SUBCOMMAND_SINCE)
            != std::cmp::Ordering::Less
        {
            args.push("execute".to_string());
        }
        args.extend([
            "--scan-class-path".to_string(),
            self.scan_dir.display().to_string(),
            "--details=tree".to_string(),
            format!(
                "--details-theme={}",
                if self.ascii { "ascii" } else { "unicode" }
            ),
        ]);
        if !self.color {
            args.push("--disable-ansi-colors".to_string());
        }
        if let Some(filter) = &self.filter {
            args.push("--include-classname".to_string());
            args.push(filter.clone());
        }
        args
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct TestOutcome {
    pub exit_code: i32,
    pub found: u64,
    pub passed: u64,
    pub failed: u64,
    pub skipped: u64,
}

impl TestOutcome {
    pub fn ok(&self) -> bool {
        self.exit_code == 0
    }

    /// `31 tests, 30 passed, 1 failed`
    pub fn describe(&self) -> String {
        let mut s = format!("{} tests, {} passed", self.found, self.passed);
        if self.failed > 0 {
            s.push_str(&format!(", {} failed", self.failed));
        }
        if self.skipped > 0 {
            s.push_str(&format!(", {} skipped", self.skipped));
        }
        s
    }
}

/// Launch the console launcher and follow along.
pub fn run(toolchain: &Toolchain, run: &TestRun, ui: &Ui) -> Result<TestOutcome> {
    // The launcher's output is always streamed, even with nothing animated: the
    // counts jrs reports come from the summary block it prints, and reading them
    // costs nothing.
    let args = run.args();
    let mut outcome = TestOutcome::default();
    let mut summary = Vec::new();
    let exit_code = run_streaming(ui, &toolchain.java, &args, |line| {
        if let Some(mark) = test_mark(line) {
            ui.update_live(|live| {
                if let Live::Tests(state) = live {
                    state.marks.push(mark);
                    match mark {
                        Outcome::Pass => state.passed += 1,
                        Outcome::Fail => state.failed += 1,
                        Outcome::Skip => state.skipped += 1,
                    }
                }
            });
        }
        if let Some(entry) = summary_entry(line) {
            summary.push(entry);
        }
    })?;

    outcome.exit_code = exit_code;
    for (count, what) in summary {
        match what.as_str() {
            "found" => outcome.found = count,
            "successful" => outcome.passed = count,
            "failed" => outcome.failed = count,
            "skipped" => outcome.skipped = count,
            _ => {}
        }
    }
    Ok(outcome)
}

/// Recognise a finished *test* in the launcher's tree output.
///
/// Container lines carry the same marks, so the `()` of a method's display name
/// is what separates a test from the class that holds it. A live counter that is
/// occasionally off by a container is fine — the authoritative numbers come from
/// the launcher's own summary.
pub fn test_mark(line: &str) -> Option<Outcome> {
    if !line.contains("()") {
        return None;
    }
    // Unicode theme first, then the ascii one.
    for (needle, outcome) in [
        ("✔", Outcome::Pass),
        ("✘", Outcome::Fail),
        ("↷", Outcome::Skip),
        ("[OK]", Outcome::Pass),
        ("[X]", Outcome::Fail),
        ("[A]", Outcome::Fail),
        ("[S]", Outcome::Skip),
    ] {
        if line.contains(needle) {
            return Some(outcome);
        }
    }
    None
}

/// Parse one line of the launcher's closing summary, e.g.
/// `[         2 tests successful      ]`.
pub fn summary_entry(line: &str) -> Option<(u64, String)> {
    let trimmed = line.trim();
    let inner = trimmed.strip_prefix('[')?.strip_suffix(']')?;
    let mut parts = inner.split_whitespace();
    let count: u64 = parts.next()?.parse().ok()?;
    let noun = parts.next()?;
    if noun != "tests" && noun != "test" {
        return None;
    }
    Some((count, parts.next()?.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn manifest(body: &str) -> Manifest {
        let text = format!("[project]\nname='app'\nversion='1.0.0'\n{body}");
        Manifest::parse(&text, Path::new("/p/jrs.toml"), Path::new("/p")).unwrap()
    }

    #[test]
    fn the_platform_version_tracks_the_jupiter_version() {
        assert_eq!(platform_version("5.10.2").as_deref(), Some("1.10.2"));
        assert_eq!(platform_version("5.11.0-M1").as_deref(), Some("1.11.0-M1"));
        assert_eq!(platform_version("4.13.2"), None);
    }

    #[test]
    fn the_launcher_is_derived_from_the_declared_jupiter_version() {
        let m = manifest("[dev-dependencies]\n'org.junit.jupiter:junit-jupiter'='5.10.2'");
        assert_eq!(
            launcher_coordinate(&m).unwrap().to_string(),
            "org.junit.platform:junit-platform-console-standalone:1.10.2"
        );

        let m = manifest("[dev-dependencies]\n'org.junit.jupiter:junit-jupiter-api'='5.11.4'");
        assert_eq!(
            launcher_coordinate(&m).unwrap().version,
            "1.11.4",
            "the api artifact carries the same version"
        );
    }

    #[test]
    fn an_explicit_launcher_wins() {
        let m = manifest(
            "[dev-dependencies]\n'org.junit.jupiter:junit-jupiter'='5.10.2'\n\
             'org.junit.platform:junit-platform-console-standalone'='1.9.3'",
        );
        assert_eq!(launcher_coordinate(&m).unwrap().version, "1.9.3");
    }

    #[test]
    fn a_project_without_junit_is_told_what_to_add() {
        let err = launcher_coordinate(&manifest("")).unwrap_err().to_string();
        assert!(err.contains("needs JUnit 5"), "{err}");
        assert!(err.contains("dev-dependencies"), "{err}");
    }

    #[test]
    fn junit_4_is_rejected_with_an_actionable_message() {
        let m = manifest("[dev-dependencies]\n'org.junit.jupiter:junit-jupiter'='4.13.2'");
        let err = launcher_coordinate(&m).unwrap_err().to_string();
        assert!(err.contains("JUnit 5 only"), "{err}");
    }

    #[test]
    fn launcher_arguments_match_the_console_launcher() {
        let run = TestRun {
            classpath: vec![PathBuf::from("/target/test-classes")],
            scan_dir: PathBuf::from("/target/test-classes"),
            filter: None,
            color: true,
            ascii: false,
            launcher_version: "1.10.2".into(),
        };
        let args = run.args();
        assert_eq!(args[0], "-cp");
        assert_eq!(args[2], LAUNCHER_MAIN);
        assert_eq!(args[3], "execute");
        assert_eq!(args[4], "--scan-class-path");
        assert_eq!(args[5], "/target/test-classes");
        assert!(args.contains(&"--details=tree".to_string()));
        assert!(args.contains(&"--details-theme=unicode".to_string()));
        assert!(!args.contains(&"--disable-ansi-colors".to_string()));
    }

    #[test]
    fn the_ascii_fallback_reaches_the_launcher_too() {
        let run = TestRun {
            classpath: vec![],
            scan_dir: PathBuf::from("/t"),
            filter: None,
            color: false,
            ascii: true,
            launcher_version: "1.10.2".into(),
        };
        let args = run.args();
        assert!(args.contains(&"--details-theme=ascii".to_string()));
        assert!(args.contains(&"--disable-ansi-colors".to_string()));
    }

    #[test]
    fn an_older_launcher_is_invoked_without_the_execute_subcommand() {
        let run = TestRun {
            classpath: vec![],
            scan_dir: PathBuf::from("/t"),
            filter: None,
            color: false,
            ascii: false,
            launcher_version: "1.9.3".into(),
        };
        let args = run.args();
        assert!(!args.contains(&"execute".to_string()));
        assert_eq!(args[3], "--scan-class-path");
    }

    #[test]
    fn a_filter_becomes_include_classname() {
        let run = TestRun {
            classpath: vec![],
            scan_dir: PathBuf::from("/t"),
            filter: Some(".*ServiceTest".into()),
            color: false,
            ascii: false,
            launcher_version: "1.10.2".into(),
        };
        let args = run.args();
        let i = args
            .iter()
            .position(|a| a == "--include-classname")
            .unwrap();
        assert_eq!(args[i + 1], ".*ServiceTest");
    }

    #[test]
    fn test_lines_are_recognised_in_both_themes() {
        assert_eq!(test_mark("│  ├─ addsNumbers() ✔"), Some(Outcome::Pass));
        assert_eq!(test_mark("│  └─ failsLoudly() ✘"), Some(Outcome::Fail));
        assert_eq!(test_mark("   +-- ignored() ↷"), Some(Outcome::Skip));
        assert_eq!(test_mark("  +-- addsNumbers() [OK]"), Some(Outcome::Pass));
        assert_eq!(test_mark("  +-- failsLoudly() [X]"), Some(Outcome::Fail));
    }

    #[test]
    fn container_lines_are_not_counted_as_tests() {
        assert_eq!(test_mark("├─ JUnit Jupiter ✔"), None);
        assert_eq!(test_mark("│  └─ CalculatorTest ✔"), None);
        assert_eq!(test_mark("plain output"), None);
    }

    #[test]
    fn the_closing_summary_parses() {
        assert_eq!(
            summary_entry("[         3 tests found           ]"),
            Some((3, "found".to_string()))
        );
        assert_eq!(
            summary_entry("[         2 tests successful      ]"),
            Some((2, "successful".to_string()))
        );
        assert_eq!(
            summary_entry("[         1 tests failed          ]"),
            Some((1, "failed".to_string()))
        );
        assert_eq!(summary_entry("[   1 containers found      ]"), None);
        assert_eq!(summary_entry("not a summary line"), None);
    }

    #[test]
    fn outcomes_describe_themselves() {
        let outcome = TestOutcome {
            exit_code: 1,
            found: 31,
            passed: 30,
            failed: 1,
            skipped: 0,
        };
        assert!(!outcome.ok());
        assert_eq!(outcome.describe(), "31 tests, 30 passed, 1 failed");

        let clean = TestOutcome {
            exit_code: 0,
            found: 5,
            passed: 5,
            failed: 0,
            skipped: 0,
        };
        assert!(clean.ok());
        assert_eq!(clean.describe(), "5 tests, 5 passed");
    }
}
