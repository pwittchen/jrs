//! Running the user's tests through the JUnit Platform Console Launcher.
//!
//! JUnit 5 and 6 run on the Jupiter engine; JUnit 4 runs on the Vintage engine,
//! which the same launcher bundles (SPEC §10.2). The launcher is an internal
//! dependency: jrs resolves it itself, at the platform version that matches the
//! Jupiter version the user declared, and puts it last on the classpath so the
//! user's own JUnit jars win every conflict.
//!
//! The launcher's output is passed through verbatim; jrs only reads it to keep a
//! live counter, and takes its authoritative numbers from the summary block the
//! launcher prints at the end.

use std::path::{Path, PathBuf};

use crate::error::{IoResultExt, JrsError, Result};
use crate::manifest::Manifest;
use crate::resolve::coord::{Coord, compare_versions};
use crate::toolchain::{Toolchain, run_captured, run_streaming};
use crate::ui::{Live, Outcome, Stream, Ui};

pub const LAUNCHER_GROUP: &str = "org.junit.platform";
pub const LAUNCHER_ARTIFACT: &str = "junit-platform-console-standalone";
pub const LAUNCHER_MAIN: &str = "org.junit.platform.console.ConsoleLauncher";

/// The launcher a JUnit 4 project runs on. The Vintage engine is bundled in
/// it, so the project needs nothing but `junit:junit`; this is the last 1.x
/// release, which supports every JDK jrs does.
pub const VINTAGE_LAUNCHER: &str = "1.14.4";

/// The JaCoCo release `jrs test --coverage` uses unless `[test] jacoco-version`
/// says otherwise. JaCoCo has to understand the class files the JDK writes, so
/// a JDK newer than this release may need a newer one.
pub const JACOCO_VERSION: &str = "0.8.15";

/// The JUnit Platform version that ships with a Jupiter version: `5.X.Y` with
/// `1.X.Y`; from JUnit 6 on, the versions are one and the same.
pub fn platform_version(jupiter: &str) -> Option<String> {
    if let Some(rest) = jupiter.strip_prefix("5.") {
        return Some(format!("1.{rest}"));
    }
    let major: u32 = jupiter.split('.').next()?.parse().ok()?;
    (major >= 6).then(|| jupiter.to_string())
}

/// Which console launcher to run.
///
/// An explicit `junit-platform-console-standalone` in `dev-dependencies` wins;
/// otherwise the version is derived from whichever Jupiter artifact is
/// declared, and a project on `junit:junit` alone gets the Vintage launcher.
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
    let jupiter = manifest.dev_dependencies.iter().find(|d| {
        d.group == "org.junit.jupiter" && JUPITER_ARTIFACTS.contains(&d.artifact.as_str())
    });
    if let Some(jupiter) = jupiter {
        let version = platform_version(&jupiter.version).ok_or_else(|| {
            JrsError::test(format!(
                "`{}` is at version {}, which is not a JUnit 5 or 6 release\n\n\
                 for JUnit 4, declare `junit:junit` instead, which jrs runs on the \
                 Vintage engine; or declare \
                 `org.junit.platform:junit-platform-console-standalone` to pick a \
                 launcher yourself",
                jupiter.key(),
                jupiter.version
            ))
        })?;
        return Ok(Coord::new(LAUNCHER_GROUP, LAUNCHER_ARTIFACT, version));
    }

    if manifest
        .dev_dependencies
        .iter()
        .any(|d| d.group == "junit" && d.artifact == "junit")
    {
        return Ok(Coord::new(
            LAUNCHER_GROUP,
            LAUNCHER_ARTIFACT,
            VINTAGE_LAUNCHER,
        ));
    }

    Err(JrsError::test(
        "`jrs test` needs JUnit\n\n\
         add it to jrs.toml:\n\n    [dev-dependencies]\n    \
         \"org.junit.jupiter:junit-jupiter\" = \"5.13.4\"\n\n\
         (or `\"junit:junit\" = \"4.13.2\"` for JUnit 4)",
    ))
}

/// The platform release that introduced the `execute` subcommand. Invoking the
/// launcher without it is deprecated from here on, and prints a warning.
const EXECUTE_SUBCOMMAND_SINCE: &str = "1.10";

/// The platform release that introduced `--color-palette`.
const COLOR_PALETTE_SINCE: &str = "1.9";

/// Overrides for the launcher's ANSI palette, keyed by its `Style` names.
///
/// The defaults paint test names blue (34), which is close to unreadable on a
/// dark terminal, and reported output white (37), which vanishes on a light one.
/// Both get the terminal's own foreground instead, and containers get it in bold
/// so the tree keeps its hierarchy. The status colours are left alone: green,
/// red, yellow and magenta read on either background.
const COLOR_PALETTE: &str = "\
# Written by jrs before each `jrs test`, see src/test.rs
CONTAINER=1
TEST=39
REPORTED=39
";

#[derive(Debug, Default)]
pub struct TestRun {
    /// `[test] jvm-args`, and the coverage agent when there is one.
    pub jvm_args: Vec<String>,
    /// The test classpath, launcher jar last.
    pub classpath: Vec<PathBuf>,
    /// Where compiled tests live; scanned for test classes.
    pub scan_dir: PathBuf,
    /// `jrs test --filter <pattern>` maps onto `--include-classname`.
    pub filter: Option<String>,
    /// `--include-tag` / `--exclude-tag`, JUnit's tag expressions.
    pub include_tags: Vec<String>,
    pub exclude_tags: Vec<String>,
    /// `com.example.FooTest#bar`: run these methods instead of scanning.
    pub methods: Vec<String>,
    /// Where the launcher writes JUnit XML, which is what CI systems read.
    pub reports_dir: Option<PathBuf>,
    pub color: bool,
    pub ascii: bool,
    /// The console launcher's own version, which decides its calling convention.
    pub launcher_version: String,
    /// jrs's scratch space, where the colour palette is written.
    pub work_dir: PathBuf,
}

impl TestRun {
    pub fn args(&self) -> Vec<String> {
        let mut args = self.jvm_args.clone();
        args.extend([
            "-cp".to_string(),
            Toolchain::classpath(&self.classpath),
            LAUNCHER_MAIN.to_string(),
        ]);
        if compare_versions(&self.launcher_version, EXECUTE_SUBCOMMAND_SINCE)
            != std::cmp::Ordering::Less
        {
            args.push("execute".to_string());
        }
        // Scanning and explicit selectors cannot be combined: the launcher
        // refuses. A method selection replaces the scan.
        if self.methods.is_empty() {
            args.push("--scan-class-path".to_string());
            args.push(self.scan_dir.display().to_string());
        } else {
            for method in &self.methods {
                args.push("--select-method".to_string());
                args.push(method.clone());
            }
        }
        args.extend([
            "--details=tree".to_string(),
            format!(
                "--details-theme={}",
                if self.ascii { "ascii" } else { "unicode" }
            ),
        ]);
        if !self.color {
            args.push("--disable-ansi-colors".to_string());
        } else if let Some(palette) = self.palette() {
            args.push(format!("--color-palette={}", palette.display()));
        }
        if let Some(dir) = &self.reports_dir {
            args.push("--reports-dir".to_string());
            args.push(dir.display().to_string());
        }
        if let Some(filter) = &self.filter {
            args.push("--include-classname".to_string());
            args.push(filter.clone());
        }
        for tag in &self.include_tags {
            args.push("--include-tag".to_string());
            args.push(tag.clone());
        }
        for tag in &self.exclude_tags {
            args.push("--exclude-tag".to_string());
            args.push(tag.clone());
        }
        args
    }

    /// Where the colour palette goes, when this run is coloured and the launcher
    /// is new enough to accept one. Older launchers keep their own defaults.
    pub fn palette(&self) -> Option<PathBuf> {
        let supported = compare_versions(&self.launcher_version, COLOR_PALETTE_SINCE)
            != std::cmp::Ordering::Less;
        (self.color && supported).then(|| self.work_dir.join("junit-palette.properties"))
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
    if let Some(palette) = run.palette() {
        std::fs::create_dir_all(&run.work_dir).path(&run.work_dir)?;
        std::fs::write(&palette, COLOR_PALETTE).path(&palette)?;
    }
    if let Some(dir) = &run.reports_dir {
        // Reports from an earlier run — of a test class since deleted, say —
        // must not be read by CI as this run's.
        if dir.exists() {
            std::fs::remove_dir_all(dir).path(dir)?;
        }
    }

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

// ---- coverage --------------------------------------------------------------

/// JaCoCo's agent, which instruments classes as the test JVM loads them.
pub fn jacoco_agent(version: &str) -> Coord {
    Coord::new("org.jacoco", "org.jacoco.agent", version).with_classifier(Some("runtime".into()))
}

/// JaCoCo's command-line tool, with its own dependencies bundled, which turns
/// the agent's execution data into a report.
pub fn jacoco_cli(version: &str) -> Coord {
    Coord::new("org.jacoco", "org.jacoco.cli", version).with_classifier(Some("nodeps".into()))
}

/// The `-javaagent` argument that records coverage into `exec`.
///
/// JaCoCo's option syntax splits on `,` and `=`, so neither may appear in the
/// path; jrs's own cache and target paths do not have them.
pub fn agent_argument(agent: &Path, exec: &Path) -> String {
    format!(
        "-javaagent:{}=destfile={},append=false",
        agent.display(),
        exec.display()
    )
}

/// One JaCoCo report: execution data and classes in, HTML and XML out.
#[derive(Debug)]
pub struct CoverageReport {
    pub exec: PathBuf,
    pub classes: PathBuf,
    pub sources: PathBuf,
    pub html: PathBuf,
    pub xml: PathBuf,
    pub name: String,
}

impl CoverageReport {
    pub fn args(&self, cli: &Path) -> Vec<String> {
        let path = |p: &Path| p.display().to_string();
        vec![
            "-jar".to_string(),
            path(cli),
            "report".to_string(),
            path(&self.exec),
            "--classfiles".to_string(),
            path(&self.classes),
            "--sourcefiles".to_string(),
            path(&self.sources),
            "--html".to_string(),
            path(&self.html),
            "--xml".to_string(),
            path(&self.xml),
            "--name".to_string(),
            self.name.clone(),
        ]
    }
}

/// Covered and total counts, as JaCoCo counts them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Coverage {
    pub lines: (u64, u64),
    pub branches: (u64, u64),
}

impl Coverage {
    /// `83.1% of lines, 71.0% of branches`
    pub fn describe(&self) -> String {
        let percent = |(covered, total): (u64, u64)| {
            if total == 0 {
                "n/a".to_string()
            } else {
                format!("{:.1}%", covered as f64 * 100.0 / total as f64)
            }
        };
        format!(
            "{} of lines, {} of branches",
            percent(self.lines),
            percent(self.branches)
        )
    }
}

/// Write the coverage report, passing JaCoCo's output through, and read the
/// project-wide totals back out of its XML.
pub fn report_coverage(
    toolchain: &Toolchain,
    report: &CoverageReport,
    cli: &Path,
    ui: &Ui,
) -> Result<Coverage> {
    let output = run_captured(ui, &toolchain.java, &report.args(cli))?;
    if !output.ok() {
        ui.passthrough(Stream::Err, output.stdout.trim_end());
        ui.passthrough(Stream::Err, output.stderr.trim_end());
        return Err(JrsError::build("the coverage report could not be written"));
    }
    let xml = std::fs::read_to_string(&report.xml).path(&report.xml)?;
    Ok(coverage_summary(&xml).unwrap_or_default())
}

/// The report-level counters of a JaCoCo XML report.
///
/// They are the `<counter>` elements after the last package (or group), so a
/// scan of the tail finds them without a full parse — and without the external
/// DTD JaCoCo's doctype points at.
pub fn coverage_summary(xml: &str) -> Option<Coverage> {
    let start = ["</package>", "</group>", "<report"]
        .iter()
        .filter_map(|marker| xml.rfind(marker).map(|at| at + marker.len()))
        .max()?;
    let tail = &xml[start..];
    let counter = |kind: &str| -> Option<(u64, u64)> {
        let at = tail.find(&format!("type=\"{kind}\""))?;
        let element = &tail[at..at + tail[at..].find("/>")?];
        let attribute = |name: &str| -> Option<u64> {
            let value = element.split(&format!("{name}=\"")).nth(1)?;
            value.split('"').next()?.parse().ok()
        };
        let (missed, covered) = (attribute("missed")?, attribute("covered")?);
        Some((covered, covered + missed))
    };
    Some(Coverage {
        lines: counter("LINE")?,
        branches: counter("BRANCH").unwrap_or((0, 0)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(body: &str) -> Manifest {
        let text = format!("[project]\nname='app'\nversion='1.0.0'\n{body}");
        Manifest::parse(&text, Path::new("/p/jrs.toml"), Path::new("/p")).unwrap()
    }

    fn run(launcher_version: &str) -> TestRun {
        TestRun {
            classpath: vec![PathBuf::from("/target/test-classes")],
            scan_dir: PathBuf::from("/target/test-classes"),
            launcher_version: launcher_version.into(),
            work_dir: PathBuf::from("/target/.jrs"),
            ..TestRun::default()
        }
    }

    #[test]
    fn the_platform_version_tracks_the_jupiter_version() {
        assert_eq!(platform_version("5.10.2").as_deref(), Some("1.10.2"));
        assert_eq!(platform_version("5.11.0-M1").as_deref(), Some("1.11.0-M1"));
        assert_eq!(platform_version("6.0.1").as_deref(), Some("6.0.1"));
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

        let m = manifest("[dev-dependencies]\n'org.junit.jupiter:junit-jupiter'='6.1.3'");
        assert_eq!(launcher_coordinate(&m).unwrap().version, "6.1.3");
    }

    #[test]
    fn junit_4_runs_on_the_vintage_launcher() {
        let m = manifest("[dev-dependencies]\n'junit:junit'='4.13.2'");
        assert_eq!(launcher_coordinate(&m).unwrap().version, VINTAGE_LAUNCHER);

        // With Jupiter declared too, Jupiter's launcher runs both.
        let m = manifest(
            "[dev-dependencies]\n'junit:junit'='4.13.2'\n\
             'org.junit.jupiter:junit-jupiter'='5.10.2'",
        );
        assert_eq!(launcher_coordinate(&m).unwrap().version, "1.10.2");
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
        assert!(err.contains("needs JUnit"), "{err}");
        assert!(err.contains("dev-dependencies"), "{err}");
        assert!(err.contains("junit:junit"), "{err}");
    }

    #[test]
    fn a_jupiter_artifact_at_a_junit_4_version_is_rejected_with_an_actionable_message() {
        let m = manifest("[dev-dependencies]\n'org.junit.jupiter:junit-jupiter'='4.13.2'");
        let err = launcher_coordinate(&m).unwrap_err().to_string();
        assert!(err.contains("not a JUnit 5 or 6 release"), "{err}");
        assert!(err.contains("junit:junit"), "{err}");
    }

    #[test]
    fn launcher_arguments_match_the_console_launcher() {
        let run = TestRun {
            color: true,
            ..run("1.10.2")
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
    fn jvm_arguments_come_before_the_classpath() {
        let run = TestRun {
            jvm_args: vec!["-Xmx256m".into(), "-javaagent:/a.jar=destfile=/x".into()],
            ..run("1.10.2")
        };
        let args = run.args();
        assert_eq!(&args[..2], &["-Xmx256m", "-javaagent:/a.jar=destfile=/x"]);
        assert_eq!(args[2], "-cp");
    }

    #[test]
    fn the_ascii_fallback_reaches_the_launcher_too() {
        let run = TestRun {
            ascii: true,
            ..run("1.10.2")
        };
        let args = run.args();
        assert!(args.contains(&"--details-theme=ascii".to_string()));
        assert!(args.contains(&"--disable-ansi-colors".to_string()));
    }

    #[test]
    fn an_older_launcher_is_invoked_without_the_execute_subcommand() {
        let args = run("1.9.3").args();
        assert!(!args.contains(&"execute".to_string()));
        assert_eq!(args[3], "--scan-class-path");
    }

    #[test]
    fn a_filter_becomes_include_classname() {
        let run = TestRun {
            filter: Some(".*ServiceTest".into()),
            ..run("1.10.2")
        };
        let args = run.args();
        let i = args
            .iter()
            .position(|a| a == "--include-classname")
            .unwrap();
        assert_eq!(args[i + 1], ".*ServiceTest");
    }

    #[test]
    fn tags_and_methods_select_what_runs() {
        let run = TestRun {
            include_tags: vec!["fast".into()],
            exclude_tags: vec!["slow | flaky".into()],
            methods: vec![
                "com.example.FooTest#bar".into(),
                "com.example.Baz#qux".into(),
            ],
            ..run("1.10.2")
        };
        let args = run.args();
        assert!(!args.contains(&"--scan-class-path".to_string()));
        let pairs: Vec<(&str, &str)> = args
            .windows(2)
            .map(|w| (w[0].as_str(), w[1].as_str()))
            .collect();
        assert!(pairs.contains(&("--select-method", "com.example.FooTest#bar")));
        assert!(pairs.contains(&("--select-method", "com.example.Baz#qux")));
        assert!(pairs.contains(&("--include-tag", "fast")));
        assert!(pairs.contains(&("--exclude-tag", "slow | flaky")));
    }

    #[test]
    fn reports_land_where_ci_looks_for_them() {
        let run = TestRun {
            reports_dir: Some(PathBuf::from("/target/test-reports")),
            ..run("1.10.2")
        };
        let args = run.args();
        let i = args.iter().position(|a| a == "--reports-dir").unwrap();
        assert_eq!(args[i + 1], "/target/test-reports");
    }

    fn coloured_run(launcher_version: &str) -> TestRun {
        TestRun {
            color: true,
            ..run(launcher_version)
        }
    }

    #[test]
    fn a_coloured_run_gets_a_palette_readable_on_any_background() {
        let run = coloured_run("1.10.2");
        let palette = run.work_dir.join("junit-palette.properties");
        let args = run.args();
        assert!(
            args.contains(&format!("--color-palette={}", palette.display())),
            "{args:?}"
        );
        // Blue is unreadable on dark terminals and white on light ones; neither
        // may come back through the overrides.
        for style in ["CONTAINER", "TEST", "REPORTED"] {
            let value = COLOR_PALETTE
                .lines()
                .find_map(|l| l.strip_prefix(&format!("{style}=")))
                .unwrap_or_else(|| panic!("{style} is not overridden"));
            assert!(
                !value.split(';').any(|c| c == "34" || c == "37"),
                "{style}={value}"
            );
        }
    }

    #[test]
    fn launchers_without_palette_support_keep_their_defaults() {
        assert_eq!(coloured_run("1.8.2").palette(), None);
        assert!(
            !coloured_run("1.8.2")
                .args()
                .iter()
                .any(|a| a.starts_with("--color-palette"))
        );
        assert!(coloured_run("1.9.0").palette().is_some());
        assert!(coloured_run("6.0.0").palette().is_some());
    }

    #[test]
    fn a_colourless_run_gets_no_palette() {
        let run = TestRun {
            color: false,
            ..coloured_run("1.10.2")
        };
        assert_eq!(run.palette(), None);
        assert!(!run.args().iter().any(|a| a.starts_with("--color-palette")));
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

    #[test]
    fn jacoco_comes_as_two_classified_jars() {
        assert_eq!(
            jacoco_agent("0.8.15").repo_path("jar"),
            "org/jacoco/org.jacoco.agent/0.8.15/org.jacoco.agent-0.8.15-runtime.jar"
        );
        assert_eq!(
            jacoco_cli("0.8.15").repo_path("jar"),
            "org/jacoco/org.jacoco.cli/0.8.15/org.jacoco.cli-0.8.15-nodeps.jar"
        );
        assert_eq!(
            agent_argument(Path::new("/c/agent.jar"), Path::new("/t/jacoco.exec")),
            "-javaagent:/c/agent.jar=destfile=/t/jacoco.exec,append=false"
        );
    }

    #[test]
    fn the_report_totals_are_read_from_the_tail_of_the_xml() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><!DOCTYPE report PUBLIC "-//JACOCO//DTD Report 1.1//EN" "report.dtd"><report name="app"><sessioninfo id="x" start="1" dump="2"/><package name="com/example"><class name="com/example/A"><counter type="LINE" missed="99" covered="1"/></class><counter type="LINE" missed="5" covered="15"/></package><counter type="INSTRUCTION" missed="10" covered="90"/><counter type="BRANCH" missed="1" covered="3"/><counter type="LINE" missed="5" covered="15"/><counter type="METHOD" missed="0" covered="4"/></report>"#;
        let coverage = coverage_summary(xml).unwrap();
        assert_eq!(coverage.lines, (15, 20));
        assert_eq!(coverage.branches, (3, 4));
        assert_eq!(coverage.describe(), "75.0% of lines, 75.0% of branches");

        let no_branches = Coverage {
            lines: (0, 0),
            branches: (0, 0),
        };
        assert_eq!(no_branches.describe(), "n/a of lines, n/a of branches");
        assert_eq!(coverage_summary("not xml"), None);
    }
}
