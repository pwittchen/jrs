//! Running the user's tests through the `JUnit` Platform Console Launcher.
//!
//! `JUnit` 5 and 6 run on the Jupiter engine; `JUnit` 4 runs on the Vintage engine,
//! which the same launcher bundles (SPEC §10.2). The launcher is an internal
//! dependency: jrs resolves it itself, at the platform version that matches the
//! Jupiter version the user declared, and puts it last on the classpath so the
//! user's own `JUnit` jars win every conflict — except the launcher's own
//! parts, which it bundles at its version: a copy of those that the project's
//! graph brings in would shadow the console launcher's classes with another
//! release of them.
//!
//! The launcher's output is passed through verbatim; jrs only reads it to keep a
//! live counter, and takes its authoritative numbers from the summary block the
//! launcher prints at the end.

use std::fmt::Write as _;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};

use crate::error::{IoResultExt, JrsError, Result, exit};
use crate::manifest::{CoverageCounter, CoverageMinimum, Manifest};
use crate::resolve::Resolution;
use crate::resolve::coord::{Coord, Ga, compare_versions};
use crate::toolchain::{Environment, Toolchain, run_captured, run_streaming_until};
use crate::ui::{Live, Outcome, Stream, Ui};

pub const LAUNCHER_GROUP: &str = "org.junit.platform";
pub const LAUNCHER_ARTIFACT: &str = "junit-platform-console-standalone";
pub const LAUNCHER_MAIN: &str = "org.junit.platform.console.ConsoleLauncher";

/// The launcher a `JUnit` 4 project runs on. The Vintage engine is bundled in
/// it, so the project needs nothing but `junit:junit`; this is the last 1.x
/// release, which supports every JDK jrs does.
pub const VINTAGE_LAUNCHER: &str = "1.14.4";

/// The `JaCoCo` release `jrs test --coverage` uses unless `[test] jacoco-version`
/// says otherwise. `JaCoCo` has to understand the class files the JDK writes, so
/// a JDK newer than this release may need a newer one.
pub const JACOCO_VERSION: &str = "0.8.15";

/// The `JUnit` Platform version that ships with a Jupiter version: `5.X.Y` with
/// `1.X.Y`; from `JUnit` 6 on, the versions are one and the same.
#[must_use]
pub fn platform_version(jupiter: &str) -> Option<String> {
    if let Some(rest) = jupiter.strip_prefix("5.") {
        return Some(format!("1.{rest}"));
    }
    let major: u32 = jupiter.split('.').next()?.parse().ok()?;
    (major >= 6).then(|| jupiter.to_string())
}

/// Which console launcher to run.
///
/// An explicit `junit-platform-console-standalone` in `dev-dependencies` wins.
/// Otherwise the launcher follows the resolved graph: the version of the
/// `junit-platform-engine` there, which is how Spock, Kotest and `ScalaTest`,
/// which bring the platform transitively, get a launcher their engine agrees
/// with. Failing that, the version is derived from whichever Jupiter artifact
/// is declared, and a project on `junit:junit`, declared or brought in (as
/// `MUnit` brings it), gets the Vintage launcher.
///
/// # Errors
///
/// [`JrsError::Test`] if no `JUnit` is declared or resolved, or a Jupiter
/// artifact is at a version that is not a `JUnit` 5 or 6 release.
pub fn launcher_coordinate(manifest: &Manifest, resolution: &Resolution) -> Result<Coord> {
    const JUPITER_ARTIFACTS: &[&str] = &[
        "junit-jupiter",
        "junit-jupiter-api",
        "junit-jupiter-engine",
        "junit-jupiter-params",
    ];

    if let Some(d) = manifest
        .dev_dependencies
        .iter()
        .find(|d| d.group == LAUNCHER_GROUP && d.artifact == LAUNCHER_ARTIFACT)
    {
        return Ok(Coord::new(&d.group, &d.artifact, &d.version));
    }

    if let Some(engine) = resolution
        .get(&Ga::new(LAUNCHER_GROUP, "junit-platform-engine"))
        .filter(|p| platform_release(&p.coord.version))
    {
        return Ok(Coord::new(
            LAUNCHER_GROUP,
            LAUNCHER_ARTIFACT,
            &engine.coord.version,
        ));
    }

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
        || resolution.get(&Ga::new("junit", "junit")).is_some()
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

/// Whether a `junit-platform-engine` version has a console launcher to match:
/// `1.x` or, from `JUnit` 6 on, the Jupiter version itself.
fn platform_release(version: &str) -> bool {
    version.starts_with("1.") || platform_version(version).is_some()
}

/// The launcher's own default class-name pattern, plus the `Spec` and `Suite`
/// suffixes Spock, Kotest, `ScalaTest` and `MUnit` name their classes with.
/// Without them those classes are not run at all.
pub const NON_JAVA_CLASS_PATTERN: &str = r"^(Test.*|.+[.$]Test.*|.*Tests?|.*Spec|.*Suite)$";

/// The launcher's own parts, which the standalone jar bundles at the version
/// jrs picked. `kotlin-test-junit5`, for one, brings `junit-platform-launcher`
/// at an older release, and first on the classpath it would shadow the console
/// launcher's classes: a `NoSuchMethodError` before any test runs.
const BUNDLED_PARTS: &[&str] = &[
    "junit-platform-launcher",
    "junit-platform-console",
    "junit-platform-reporting",
];

/// The test JVM's classpath: `classpath` without the jars of the graph's own
/// copies of the launcher's parts, since the standalone launcher brings them.
#[must_use]
pub fn without_bundled_launcher(classpath: Vec<PathBuf>, resolution: &Resolution) -> Vec<PathBuf> {
    let bundled: Vec<&PathBuf> = resolution
        .packages
        .iter()
        .filter(|p| {
            p.coord.group == LAUNCHER_GROUP && BUNDLED_PARTS.contains(&p.coord.artifact.as_str())
        })
        .filter_map(|p| p.jar.as_ref())
        .collect();
    classpath
        .into_iter()
        .filter(|entry| !bundled.contains(&entry))
        .collect()
}

/// The `--include-classname` pattern: `--filter` when given, else the wider
/// pattern when the tests are not all Java. A Java-only project keeps the
/// launcher's default, so that Java classes named `*Spec` that never ran do
/// not start running (`JVM_LANGUAGES.md` §14.5).
#[must_use]
pub fn class_name_filter(filter: Option<&str>, non_java_tests: bool) -> Option<String> {
    filter
        .map(str::to_string)
        .or_else(|| non_java_tests.then(|| NON_JAVA_CLASS_PATTERN.to_string()))
}

/// The platform release that introduced the `execute` subcommand. Invoking the
/// launcher without it is deprecated from here on, and prints a warning.
const EXECUTE_SUBCOMMAND_SINCE: &str = "1.10";

/// The platform release that introduced `--color-palette`.
const COLOR_PALETTE_SINCE: &str = "1.9";

/// The platform release that introduced `--fail-fast`: `JUnit` 6. No 1.x
/// launcher has it, up to and including 1.14.
pub const FAIL_FAST_SINCE: &str = "6.0";

/// What a launcher with `--fail-fast` prints when it stopped a run early.
const FAIL_FAST_CANCELLED: &str = "cancelled due to --fail-fast";

/// The platform release that introduced `--details=testfeed`, which reports
/// each test as it finishes. The default tree is printed only once the whole
/// run is over, which is too late to stop at the first failure.
const TESTFEED_SINCE: &str = "1.10";

/// How `jrs test --fail-fast` is carried out with a given launcher.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailFast {
    /// Not asked for.
    Off,
    /// The launcher's own `--fail-fast`, from `JUnit` 6 on.
    Native,
    /// A 1.10 to 1.14 launcher: jrs asks for its test feed instead of the
    /// tree, and stops the launcher when the test after the first failure
    /// starts.
    Stop,
    /// A launcher before 1.10 has neither, so every test runs.
    Unsupported,
}

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

#[derive(Debug, Clone, Default)]
pub struct TestRun {
    /// `[test] jvm-args`, and the coverage agent when there is one.
    pub jvm_args: Vec<String>,
    /// The test classpath, launcher jar last.
    pub classpath: Vec<PathBuf>,
    /// Where compiled tests live; scanned for test classes.
    pub scan_dir: PathBuf,
    /// `jrs test --filter <pattern>` maps onto `--include-classname`.
    pub filter: Option<String>,
    /// `--include-tag` / `--exclude-tag`, `JUnit`'s tag expressions.
    pub include_tags: Vec<String>,
    pub exclude_tags: Vec<String>,
    /// `com.example.FooTest#bar`: run these methods instead of scanning.
    pub methods: Vec<String>,
    /// Where the launcher writes `JUnit` XML, which is what CI systems read.
    pub reports_dir: Option<PathBuf>,
    pub color: bool,
    pub ascii: bool,
    /// The console launcher's own version, which decides its calling convention.
    pub launcher_version: String,
    /// jrs's scratch space, where the colour palette is written.
    pub work_dir: PathBuf,
    /// `test.env`, added to what the test JVM inherits.
    pub environment: Environment,
    /// Tests selected by unique ID (`--select-unique-id`): one invocation of a
    /// parameterised test, one dynamic test. Like `methods`, replaces the scan.
    pub unique_ids: Vec<String>,
    /// Whole classes (`--select-class`), for a failed test the reports give no
    /// narrower selector for. Like `methods`, replaces the scan.
    pub classes: Vec<String>,
    /// `jrs test --fail-fast`: stop at the first failure, as [`FailFast`]
    /// says this launcher can.
    pub fail_fast: bool,
}

impl TestRun {
    #[must_use]
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
        if self.methods.is_empty() && self.unique_ids.is_empty() && self.classes.is_empty() {
            args.push("--scan-class-path".to_string());
            args.push(self.scan_dir.display().to_string());
        } else {
            for method in &self.methods {
                args.push("--select-method".to_string());
                args.push(method.clone());
            }
            for id in &self.unique_ids {
                args.push("--select-unique-id".to_string());
                args.push(id.clone());
            }
            for class in &self.classes {
                args.push("--select-class".to_string());
                args.push(class.clone());
            }
        }
        args.extend([
            format!(
                "--details={}",
                if self.fail_fast_mode() == FailFast::Stop {
                    "testfeed"
                } else {
                    "tree"
                }
            ),
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
        if self.fail_fast_mode() == FailFast::Native {
            args.push("--fail-fast".to_string());
        }
        args
    }

    /// How `--fail-fast` is carried out with this launcher. A 1.x one refuses
    /// `--fail-fast` as an unknown option, so jrs stops it instead where it
    /// can follow the run as it happens.
    #[must_use]
    pub fn fail_fast_mode(&self) -> FailFast {
        let since = |version: &str| {
            compare_versions(&self.launcher_version, version) != std::cmp::Ordering::Less
        };
        if !self.fail_fast {
            FailFast::Off
        } else if since(FAIL_FAST_SINCE) {
            FailFast::Native
        } else if since(TESTFEED_SINCE) {
            FailFast::Stop
        } else {
            FailFast::Unsupported
        }
    }

    /// Where the colour palette goes, when this run is coloured and the launcher
    /// is new enough to accept one. Older launchers keep their own defaults.
    #[must_use]
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
    /// Tests that failed and then passed on a retry: counted here, not as
    /// passed, so a retry cannot hide them.
    pub flaky: u64,
    /// `--fail-fast` ended the run before every test had run.
    pub stopped_early: bool,
    /// The totals below `test.coverage-minimum`, on a `--coverage` run.
    pub coverage_shortfalls: Vec<CoverageShortfall>,
}

impl TestOutcome {
    #[must_use]
    pub fn ok(&self) -> bool {
        self.exit_code == 0
    }

    /// `31 tests, 29 passed, 1 flaky, 1 failed`
    #[must_use]
    pub fn describe(&self) -> String {
        let mut s = format!("{} tests, {} passed", self.found, self.passed);
        if self.flaky > 0 {
            let _ = write!(s, ", {} flaky", self.flaky);
        }
        if self.failed > 0 {
            let _ = write!(s, ", {} failed", self.failed);
        }
        if self.skipped > 0 {
            let _ = write!(s, ", {} skipped", self.skipped);
        }
        if self.stopped_early {
            s.push_str("; stopped at the first failure");
        }
        s
    }
}

/// Launch the console launcher and follow along.
///
/// # Errors
///
/// [`JrsError::Io`] if the colour palette cannot be written or an earlier run's
/// reports cannot be removed, and [`JrsError::Build`] if `java` cannot be
/// started. Failing tests are not an error: they are in the [`TestOutcome`].
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
    // Counted here as well, for a run jrs stops before its summary.
    let mut seen = TestOutcome::default();
    let mut cancelled = false;
    let stop_it = run.fail_fast_mode() == FailFast::Stop;
    let mut failing = false;
    let (exit_code, stopped) =
        run_streaming_until(ui, &toolchain.java, &args, &run.environment, |line| {
            // The test after the first failure has started: stop it before it
            // gets anywhere, with the failure and its trace already through.
            if stop_it && failing && feed_started(line) {
                return ControlFlow::Break(());
            }
            if let Some(mark) = test_mark(line).or_else(|| feed_mark(line)) {
                failing |= mark == Outcome::Fail;
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
                match mark {
                    Outcome::Pass => seen.passed += 1,
                    Outcome::Fail => seen.failed += 1,
                    Outcome::Skip => seen.skipped += 1,
                }
            }
            if let Some(entry) = summary_entry(line) {
                summary.push(entry);
            }
            cancelled |= line.contains(FAIL_FAST_CANCELLED);
            ControlFlow::Continue(())
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
    if stopped {
        // Stopped before its summary: what jrs counted is all there is, and
        // it stopped because something failed.
        let failed = seen.failed.max(1);
        outcome = TestOutcome {
            exit_code: exit::FAILURE,
            found: seen.passed + failed + seen.skipped,
            passed: seen.passed,
            failed,
            skipped: seen.skipped,
            ..TestOutcome::default()
        };
    }
    outcome.stopped_early = stopped || cancelled;
    Ok(outcome)
}

/// Recognise a finished *test* in the launcher's tree output.
///
/// Container lines carry the same marks, so the `()` of a method's display name
/// is what separates a test from the class that holds it. A live counter that is
/// occasionally off by a container is fine — the authoritative numbers come from
/// the launcher's own summary.
#[must_use]
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

/// Recognise a finished test in the launcher's test feed
/// (`--details=testfeed`), which `--fail-fast` asks a 1.x launcher for:
/// `JUnit Jupiter > CalcTest > adds() :: SUCCESSFUL`. Unlike the tree, it
/// names each test's status, so a `JUnit` 4 test without `()` counts too.
#[must_use]
pub fn feed_mark(line: &str) -> Option<Outcome> {
    let status = feed_status(line)?;
    if status.starts_with("SUCCESSFUL") {
        Some(Outcome::Pass)
    } else if status.starts_with("FAILED") {
        Some(Outcome::Fail)
    } else if status.starts_with("ABORTED") || status.starts_with("SKIPPED") {
        Some(Outcome::Skip)
    } else {
        None
    }
}

/// Whether a line of the test feed says a test has started.
#[must_use]
pub fn feed_started(line: &str) -> bool {
    feed_status(line).is_some_and(|status| status.starts_with("STARTED"))
}

/// The status a test-feed event ends in. The feed indents the stack traces
/// under its events, and never the events themselves.
fn feed_status(line: &str) -> Option<&str> {
    if line.starts_with(char::is_whitespace) {
        return None;
    }
    line.rsplit_once(" :: ").map(|(_, status)| status)
}

/// Parse one line of the launcher's closing summary, e.g.
/// `[         2 tests successful      ]`.
#[must_use]
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

/// `JaCoCo`'s agent, which instruments classes as the test JVM loads them.
#[must_use]
pub fn jacoco_agent(version: &str) -> Coord {
    Coord::new("org.jacoco", "org.jacoco.agent", version).with_classifier(Some("runtime".into()))
}

/// `JaCoCo`'s command-line tool, with its own dependencies bundled, which turns
/// the agent's execution data into a report.
#[must_use]
pub fn jacoco_cli(version: &str) -> Coord {
    Coord::new("org.jacoco", "org.jacoco.cli", version).with_classifier(Some("nodeps".into()))
}

/// The `-javaagent` argument that records coverage into `exec`.
///
/// `JaCoCo`'s option syntax splits on `,` and `=`, so neither may appear in the
/// path; jrs's own cache and target paths do not have them.
#[must_use]
pub fn agent_argument(agent: &Path, exec: &Path) -> String {
    format!(
        "-javaagent:{}=destfile={},append=false",
        agent.display(),
        exec.display()
    )
}

/// One `JaCoCo` report: execution data and classes in, HTML and XML out.
#[derive(Debug)]
pub struct CoverageReport {
    pub exec: PathBuf,
    pub classes: PathBuf,
    /// Every main source root, so the HTML shows Kotlin, Scala and Groovy
    /// files as well as Java ones.
    pub sources: Vec<PathBuf>,
    pub html: PathBuf,
    pub xml: PathBuf,
    pub name: String,
}

impl CoverageReport {
    #[must_use]
    pub fn args(&self, cli: &Path) -> Vec<String> {
        let path = |p: &Path| p.display().to_string();
        let mut args = vec![
            "-jar".to_string(),
            path(cli),
            "report".to_string(),
            path(&self.exec),
            "--classfiles".to_string(),
            path(&self.classes),
        ];
        for root in &self.sources {
            args.push("--sourcefiles".to_string());
            args.push(path(root));
        }
        args.extend([
            "--html".to_string(),
            path(&self.html),
            "--xml".to_string(),
            path(&self.xml),
            "--name".to_string(),
            self.name.clone(),
        ]);
        args
    }
}

/// Covered and total counts, as `JaCoCo` counts them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Coverage {
    pub lines: (u64, u64),
    pub branches: (u64, u64),
}

impl Coverage {
    /// `83.1% of lines, 71.0% of branches`
    #[must_use]
    pub fn describe(&self) -> String {
        #[allow(
            clippy::cast_precision_loss,
            reason = "a percentage shown to one decimal place; line counts are nowhere near 2^52"
        )]
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

/// Write the coverage report, passing `JaCoCo`'s output through, and read the
/// project-wide totals back out of its XML.
///
/// # Errors
///
/// [`JrsError::Build`] if `JaCoCo` cannot be started or fails, and
/// [`JrsError::Io`] if the XML report it wrote cannot be read.
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

/// The report-level counters of a `JaCoCo` XML report.
///
/// They are the `<counter>` elements after the last package (or group), so a
/// scan of the tail finds them without a full parse — and without the external
/// DTD `JaCoCo`'s doctype points at.
#[must_use]
pub fn coverage_summary(xml: &str) -> Option<Coverage> {
    let tail = report_tail(xml)?;
    Some(Coverage {
        lines: report_counter(tail, "LINE")?,
        branches: report_counter(tail, "BRANCH").unwrap_or((0, 0)),
    })
}

/// The part of a `JaCoCo` XML report after its last package or group, where
/// the report-level counters are.
fn report_tail(xml: &str) -> Option<&str> {
    let start = ["</package>", "</group>", "<report"]
        .iter()
        .filter_map(|marker| xml.rfind(marker).map(|at| at + marker.len()))
        .max()?;
    Some(&xml[start..])
}

/// One `<counter type="…">` of a report's tail, as covered and total.
fn report_counter(tail: &str, kind: &str) -> Option<(u64, u64)> {
    let at = tail.find(&format!("type=\"{kind}\""))?;
    let element = &tail[at..at + tail[at..].find("/>")?];
    let attribute = |name: &str| -> Option<u64> {
        let value = element.split(&format!("{name}=\"")).nth(1)?;
        value.split('"').next()?.parse().ok()
    };
    let (missed, covered) = (attribute("missed")?, attribute("covered")?);
    Some((covered, covered + missed))
}

/// A project-wide total below its `test.coverage-minimum`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoverageShortfall {
    pub counter: CoverageCounter,
    pub covered: u64,
    pub total: u64,
    /// The minimum, in hundredths of a percent.
    pub minimum: u32,
}

impl std::fmt::Display for CoverageShortfall {
    /// `line coverage is 72.4% (131 of 181), below the minimum of 80%`
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let actual = u128::from(self.covered) * 10_000 / u128::from(self.total.max(1));
        write!(
            f,
            "{} coverage is {} ({} of {}), below the minimum of {}",
            self.counter.key(),
            percent(u64::try_from(actual).unwrap_or(u64::MAX)),
            self.covered,
            self.total,
            percent(u64::from(self.minimum))
        )
    }
}

/// Hundredths of a percent as a percentage: `8000` is `80%`, `7243` is
/// `72.43%`. The actual figure is rounded down, so a total just short of its
/// minimum never reads as equal to it.
#[must_use]
pub fn percent(basis_points: u64) -> String {
    let (whole, fraction) = (basis_points / 100, basis_points % 100);
    if fraction == 0 {
        format!("{whole}%")
    } else if fraction % 10 == 0 {
        format!("{whole}.{}%", fraction / 10)
    } else {
        format!("{whole}.{fraction:02}%")
    }
}

/// The minimums a `JaCoCo` XML report's totals fall short of, in the order
/// they were declared. The comparison is exact, in integers. A counter with
/// nothing to count — a project without a single branch — meets any minimum,
/// as it does in `JaCoCo`'s own check.
#[must_use]
pub fn coverage_shortfalls(xml: &str, minimums: &[CoverageMinimum]) -> Vec<CoverageShortfall> {
    let tail = report_tail(xml).unwrap_or("");
    minimums
        .iter()
        .filter_map(|minimum| {
            let (covered, total) =
                report_counter(tail, minimum.counter.jacoco_type()).unwrap_or((0, 0));
            let met = total == 0
                || u128::from(covered) * 10_000
                    >= u128::from(minimum.basis_points) * u128::from(total);
            (!met).then_some(CoverageShortfall {
                counter: minimum.counter,
                covered,
                total,
                minimum: minimum.basis_points,
            })
        })
        .collect()
}

/// The test JVM's arguments for a retry: the coverage agent, when there is
/// one, appends to the execution data the first attempt recorded instead of
/// starting it over.
#[must_use]
pub fn retry_jvm_args(jvm_args: &[String]) -> Vec<String> {
    jvm_args
        .iter()
        .map(|arg| match arg.strip_suffix(",append=false") {
            Some(agent) if arg.starts_with("-javaagent:") && arg.contains("=destfile=") => {
                format!("{agent},append=true")
            }
            _ => arg.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(body: &str) -> Manifest {
        let text = format!("[project]\nname='app'\nversion='1.0.0'\n{body}");
        Manifest::parse(&text, Path::new("/p/jrs.toml"), Path::new("/p")).unwrap()
    }

    /// Today's rules, from the manifest alone: a graph with nothing in it.
    fn launcher_coordinate(manifest: &Manifest) -> Result<Coord> {
        super::launcher_coordinate(manifest, &Resolution::default())
    }

    fn resolved(gavs: &[&str]) -> Resolution {
        Resolution {
            packages: gavs
                .iter()
                .map(|gav| crate::resolve::ResolvedPackage {
                    coord: Coord::parse(gav).unwrap(),
                    classpath: crate::resolve::Classpath::Test,
                    packaging: "jar".into(),
                    depth: 2,
                    direct: false,
                    dependencies: Vec::new(),
                    jar: None,
                    checksum: None,
                    mediated: false,
                })
                .collect(),
            ..Resolution::default()
        }
    }

    #[test]
    fn the_launcher_follows_the_resolved_platform_engine() {
        // Spock brings the platform transitively; nothing JUnit is declared.
        let m = manifest("[dev-dependencies]\n'org.spockframework:spock-core'='2.4-groovy-5.0'");
        let r = resolved(&["org.junit.platform:junit-platform-engine:1.14.1"]);
        assert_eq!(
            super::launcher_coordinate(&m, &r).unwrap().to_string(),
            "org.junit.platform:junit-platform-console-standalone:1.14.1"
        );
        // And where mediation moved the engine, the launcher moves with it.
        let m = manifest("[dev-dependencies]\n'org.junit.jupiter:junit-jupiter'='5.10.2'");
        let r = resolved(&["org.junit.platform:junit-platform-engine:1.11.4"]);
        assert_eq!(
            super::launcher_coordinate(&m, &r).unwrap().version,
            "1.11.4"
        );
        let r = resolved(&["org.junit.platform:junit-platform-engine:6.0.1"]);
        assert_eq!(super::launcher_coordinate(&m, &r).unwrap().version, "6.0.1");
    }

    #[test]
    fn a_transitive_junit_4_gets_the_vintage_launcher() {
        // MUnit is a JUnit 4 runner and brings `junit:junit` itself.
        let m = manifest("[dev-dependencies]\n'org.scalameta:munit_3'='1.3.6'");
        let r = resolved(&["junit:junit:4.13.2"]);
        assert_eq!(
            super::launcher_coordinate(&m, &r).unwrap().version,
            VINTAGE_LAUNCHER
        );
        assert!(super::launcher_coordinate(&m, &Resolution::default()).is_err());
    }

    #[test]
    fn the_graphs_own_launcher_parts_leave_the_test_classpath() {
        let mut r = resolved(&[
            "org.junit.platform:junit-platform-launcher:1.10.1",
            "org.junit.platform:junit-platform-engine:1.13.4",
        ]);
        for p in &mut r.packages {
            p.jar = Some(PathBuf::from(format!("/c/{}.jar", p.coord.artifact)));
        }
        let classpath = vec![
            PathBuf::from("/t/classes"),
            PathBuf::from("/c/junit-platform-launcher.jar"),
            PathBuf::from("/c/junit-platform-engine.jar"),
        ];
        assert_eq!(
            without_bundled_launcher(classpath, &r),
            vec![
                PathBuf::from("/t/classes"),
                PathBuf::from("/c/junit-platform-engine.jar")
            ],
            "the engine is the user's; the launcher is the standalone jar's"
        );
    }

    #[test]
    fn spec_and_suite_classes_run_only_with_non_java_tests() {
        assert_eq!(class_name_filter(None, false), None);
        let pattern = class_name_filter(None, true).unwrap();
        assert_eq!(pattern, NON_JAVA_CLASS_PATTERN);
        assert_eq!(
            class_name_filter(Some(".*Only"), true).as_deref(),
            Some(".*Only"),
            "--filter still replaces it"
        );
        // The launcher's own default is the pattern's first three branches.
        for class in ["com.example.FooTest", "com.example.TestFoo", "FooTests"] {
            assert!(matches_pattern(class), "{class}");
        }
        for class in ["com.example.GreeterSpec", "demo.GreeterSuite"] {
            assert!(matches_pattern(class), "{class}");
        }
        assert!(!matches_pattern("com.example.Helper"));
    }

    /// The pattern's branches, checked by hand: jrs has no regex crate, and
    /// the launcher does the real matching.
    fn matches_pattern(class: &str) -> bool {
        let simple = class.rsplit(['.', '$']).next().unwrap_or(class);
        simple.starts_with("Test")
            || simple.ends_with("Test")
            || simple.ends_with("Tests")
            || simple.ends_with("Spec")
            || simple.ends_with("Suite")
    }

    #[test]
    fn coverage_reads_every_source_root() {
        let report = CoverageReport {
            exec: PathBuf::from("/t/jacoco.exec"),
            classes: PathBuf::from("/t/classes"),
            sources: vec![
                PathBuf::from("/p/src/main/java"),
                PathBuf::from("/p/src/main/kotlin"),
            ],
            html: PathBuf::from("/t/coverage"),
            xml: PathBuf::from("/t/coverage/jacoco.xml"),
            name: "app".into(),
        };
        let args = report.args(Path::new("/c/cli.jar"));
        let roots: Vec<&str> = args
            .windows(2)
            .filter(|w| w[0] == "--sourcefiles")
            .map(|w| w[1].as_str())
            .collect();
        assert_eq!(roots, ["/p/src/main/java", "/p/src/main/kotlin"]);
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
            ..TestOutcome::default()
        };
        assert!(!outcome.ok());
        assert_eq!(outcome.describe(), "31 tests, 30 passed, 1 failed");

        let clean = TestOutcome {
            exit_code: 0,
            found: 5,
            passed: 5,
            failed: 0,
            skipped: 0,
            ..TestOutcome::default()
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

    #[test]
    fn unique_ids_and_classes_replace_the_scan_too() {
        let run = TestRun {
            unique_ids: vec!["[engine:junit-jupiter]/[class:A]/[test-factory:f()]".into()],
            classes: vec!["com.example.Mystery".into()],
            ..run("1.10.2")
        };
        let args = run.args();
        assert!(!args.contains(&"--scan-class-path".to_string()), "{args:?}");
        let pairs: Vec<(&str, &str)> = args
            .windows(2)
            .map(|w| (w[0].as_str(), w[1].as_str()))
            .collect();
        assert!(pairs.contains(&(
            "--select-unique-id",
            "[engine:junit-jupiter]/[class:A]/[test-factory:f()]"
        )));
        assert!(pairs.contains(&("--select-class", "com.example.Mystery")));
    }

    #[test]
    fn fail_fast_is_the_launchers_own_from_junit_6_and_read_off_the_feed_before() {
        for (version, mode) in [
            ("1.9.3", FailFast::Unsupported),
            ("1.10.2", FailFast::Stop),
            ("1.14.4", FailFast::Stop),
            ("6.0.0", FailFast::Native),
            ("6.1.3", FailFast::Native),
        ] {
            let run = TestRun {
                fail_fast: true,
                ..run(version)
            };
            assert_eq!(run.fail_fast_mode(), mode, "{version}");
            let args = run.args();
            assert_eq!(
                args.contains(&"--fail-fast".to_string()),
                mode == FailFast::Native,
                "a 1.x launcher refuses the option: {version}"
            );
            // The tree is printed when the run is over; the feed as it goes.
            let details = if mode == FailFast::Stop {
                "--details=testfeed"
            } else {
                "--details=tree"
            };
            assert!(args.contains(&details.to_string()), "{version}: {args:?}");
        }
        assert_eq!(run("6.0.0").fail_fast_mode(), FailFast::Off);
        assert!(!run("6.0.0").args().contains(&"--fail-fast".to_string()));
    }

    #[test]
    fn the_test_feed_is_read_as_it_arrives() {
        assert_eq!(
            feed_mark("JUnit Jupiter > CalcTest > adds() :: SUCCESSFUL"),
            Some(Outcome::Pass)
        );
        assert_eq!(
            feed_mark("JUnit Jupiter > CalcTest > param(int) > [2] 2 :: FAILED"),
            Some(Outcome::Fail)
        );
        assert_eq!(
            feed_mark("JUnit Vintage > OldTest > oldFails :: FAILED"),
            Some(Outcome::Fail),
            "a JUnit 4 test has no ()"
        );
        assert_eq!(
            feed_mark("JUnit Jupiter > CalcTest > assumes() :: ABORTED"),
            Some(Outcome::Skip)
        );
        assert_eq!(
            feed_mark("JUnit Jupiter > CalcTest > skipped() :: SKIPPED"),
            Some(Outcome::Skip)
        );
        assert_eq!(
            feed_mark("JUnit Jupiter > CalcTest > adds() :: STARTED"),
            None
        );
        assert!(feed_started("JUnit Jupiter > CalcTest > adds() :: STARTED"));
        assert!(
            !feed_started("\t\tat a.B.c(B.java:1) :: STARTED"),
            "a trace line"
        );
        assert_eq!(
            feed_mark("\torg.opentest4j.AssertionFailedError: a :: FAILED"),
            None
        );
        assert_eq!(feed_mark("| +-- adds() [OK]"), None, "a tree line");
    }

    #[test]
    fn outcomes_count_flaky_tests_apart_and_say_when_they_stopped() {
        let retried = TestOutcome {
            found: 31,
            passed: 29,
            flaky: 1,
            failed: 1,
            ..TestOutcome::default()
        };
        assert_eq!(retried.describe(), "31 tests, 29 passed, 1 flaky, 1 failed");
        let stopped = TestOutcome {
            exit_code: 1,
            found: 3,
            passed: 2,
            failed: 1,
            stopped_early: true,
            ..TestOutcome::default()
        };
        assert_eq!(
            stopped.describe(),
            "3 tests, 2 passed, 1 failed; stopped at the first failure"
        );
    }

    const JACOCO_TAIL: &str = r#"<report name="app"><package name="p"><counter type="LINE" missed="99" covered="1"/></package><counter type="INSTRUCTION" missed="10" covered="90"/><counter type="BRANCH" missed="1" covered="3"/><counter type="LINE" missed="5" covered="15"/><counter type="METHOD" missed="0" covered="4"/></report>"#;

    fn minimum(counter: CoverageCounter, basis_points: u32) -> CoverageMinimum {
        CoverageMinimum {
            counter,
            basis_points,
        }
    }

    #[test]
    fn coverage_minimums_are_compared_exactly() {
        let shortfalls = coverage_shortfalls(
            JACOCO_TAIL,
            &[
                minimum(CoverageCounter::Line, 8000),
                // 3 of 4 is exactly 75%: met.
                minimum(CoverageCounter::Branch, 7500),
                minimum(CoverageCounter::Instruction, 9001),
                minimum(CoverageCounter::Method, 10_000),
                // No complexity or class counter in the report: nothing to
                // count, so nothing short.
                minimum(CoverageCounter::Complexity, 10_000),
                minimum(CoverageCounter::Class, 10_000),
            ],
        );
        assert_eq!(
            shortfalls
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            [
                "line coverage is 75% (15 of 20), below the minimum of 80%",
                "instruction coverage is 90% (90 of 100), below the minimum of 90.01%",
            ]
        );
        assert!(
            coverage_shortfalls("not xml", &[minimum(CoverageCounter::Line, 10_000)]).is_empty()
        );
    }

    #[test]
    fn percentages_round_down_and_drop_needless_digits() {
        assert_eq!(percent(8000), "80%");
        assert_eq!(percent(8050), "80.5%");
        assert_eq!(percent(7243), "72.43%");
        assert_eq!(percent(5), "0.05%");
        let just_short = CoverageShortfall {
            counter: CoverageCounter::Line,
            covered: 7999,
            total: 10_000,
            minimum: 8000,
        };
        assert!(
            just_short.to_string().contains("is 79.99% "),
            "{just_short}"
        );
    }

    #[test]
    fn a_retry_adds_to_the_coverage_the_first_attempt_recorded() {
        let first = vec![
            "-javaagent:/c/agent.jar=destfile=/t/jacoco.exec,append=false".to_string(),
            "-Xmx256m".to_string(),
        ];
        assert_eq!(
            retry_jvm_args(&first),
            [
                "-javaagent:/c/agent.jar=destfile=/t/jacoco.exec,append=true",
                "-Xmx256m"
            ]
        );
        let other = vec!["-javaagent:/c/mockito.jar".to_string()];
        assert_eq!(retry_jvm_args(&other), other, "other agents are left alone");
    }
}
