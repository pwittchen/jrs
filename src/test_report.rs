//! What a test run leaves behind: the `JUnit` XML the console launcher writes
//! into `target/test-reports`, read back (SPEC §10.2).
//!
//! Three things start here. `jrs test --rerun-failed` and `test.retries` turn
//! the failed test cases into launcher selectors, and every run that wrote XML
//! gets a static page beside it, `target/test-reports/index.html`, so that a
//! failure in a large suite is a click away rather than a scroll through the
//! launcher's tree. The page is written by hand — no crate, no script, no
//! external resource — and depends on nothing but the XML, so the same reports
//! always give the same bytes.
//!
//! The launcher's XML reporter writes one file per engine
//! (`TEST-junit-jupiter.xml`, `TEST-junit-vintage.xml`, …) and records each
//! test's unique ID in its `<system-out>`. A retry writes its own reports into
//! `retry-<n>/` below the first run's, and [`load`] folds them in: a test that
//! failed and then passed is [`Status::Flaky`], never passed.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use quick_xml::XmlVersion;
use quick_xml::events::{BytesRef, BytesStart, Event};

use crate::error::{IoResultExt, JrsError, Result};

/// The page written beside the XML.
pub const INDEX: &str = "index.html";

/// Retry `n` writes its reports into `retry-<n>/`, from `retry-1` on.
pub const RETRY_PREFIX: &str = "retry-";

/// Where retry `attempt` (1 for the first retry) writes its reports.
#[must_use]
pub fn retry_dir(reports: &Path, attempt: u32) -> PathBuf {
    reports.join(format!("{RETRY_PREFIX}{attempt}"))
}

/// How a test ended, ordered worst first: sorting by it puts failures on top.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Status {
    /// Failed on every attempt it had.
    Failed,
    /// Failed, then passed on a retry.
    Flaky,
    Skipped,
    Passed,
}

impl Status {
    fn class(self) -> &'static str {
        match self {
            Status::Failed => "failed",
            Status::Flaky => "flaky",
            Status::Skipped => "skipped",
            Status::Passed => "passed",
        }
    }
}

/// A `<failure>` (an assertion) or an `<error>` (any other exception).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Failure {
    pub error: bool,
    /// The exception's class, `org.opentest4j.AssertionFailedError`.
    pub kind: String,
    pub message: String,
    /// The stack trace, as the launcher wrote it.
    pub trace: String,
}

/// One `<testcase>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestCase {
    /// `com.example.CalcTest`, or `com.example.CalcTest$Inner` for a test in
    /// a nested class.
    pub class: String,
    /// `adds()`, `param(int)[2]`, or a `JUnit` 4 test's bare `adds`.
    pub name: String,
    /// Summed over every attempt.
    pub millis: u64,
    pub status: Status,
    /// The latest failure; for a flaky test, the one it had before passing.
    pub failure: Option<Failure>,
    pub skip_reason: Option<String>,
    /// `[engine:junit-jupiter]/[class:com.example.CalcTest]/[method:adds()]`.
    pub unique_id: Option<String>,
    pub display_name: Option<String>,
    /// How many runs the test was part of: 1, plus one per retry.
    pub attempts: u32,
}

impl TestCase {
    /// `com.example.CalcTest#adds()`, the way `--method` spells a test.
    #[must_use]
    pub fn label(&self) -> String {
        if self.class.is_empty() {
            self.name.clone()
        } else {
            format!("{}#{}", self.class, self.name)
        }
    }

    /// What a retry's result is matched to the first run's by.
    fn key(&self) -> String {
        self.unique_id.clone().unwrap_or_else(|| self.label())
    }
}

/// A run's test cases, with any retries folded in.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Results {
    /// The first run's cases in report order, then any a retry found that the
    /// first run did not.
    pub cases: Vec<TestCase>,
    /// How many retries were folded in.
    pub retries: u32,
}

impl Results {
    /// The tests that are still failing.
    #[must_use]
    pub fn failed(&self) -> Vec<&TestCase> {
        self.with(Status::Failed)
    }

    /// The tests that failed and then passed on a retry.
    #[must_use]
    pub fn flaky(&self) -> Vec<&TestCase> {
        self.with(Status::Flaky)
    }

    fn with(&self, status: Status) -> Vec<&TestCase> {
        self.cases.iter().filter(|c| c.status == status).collect()
    }

    /// Fold one retry's cases in. A failed test that passes becomes flaky; one
    /// that fails again keeps failing, with the newer failure.
    pub fn merge_retry(&mut self, retry: Vec<TestCase>) {
        self.retries += 1;
        for case in retry {
            let key = case.key();
            let Some(existing) = self.cases.iter_mut().find(|c| c.key() == key) else {
                self.cases.push(case);
                continue;
            };
            existing.attempts += 1;
            existing.millis = existing.millis.saturating_add(case.millis);
            match case.status {
                Status::Failed if existing.status == Status::Failed => {
                    existing.failure = case.failure;
                }
                Status::Passed if existing.status == Status::Failed => {
                    existing.status = Status::Flaky;
                }
                _ => {}
            }
        }
    }
}

// ---- reading the XML --------------------------------------------------------

/// Read a run's reports back: every `TEST-*.xml` in `reports`, then each
/// `retry-<n>/` in order. `None` when `reports` holds no report at all — there
/// was no run, or it ended before the launcher wrote one.
///
/// # Errors
///
/// [`JrsError::Io`] if a report cannot be read, and [`JrsError::Test`] if one
/// is not well-formed XML.
pub fn load(reports: &Path) -> Result<Option<Results>> {
    let first = report_files(reports)?;
    if first.is_empty() {
        return Ok(None);
    }
    let mut results = Results::default();
    for file in first {
        results.cases.extend(read(&file)?);
    }
    for dir in retry_dirs(reports)? {
        let mut retry = Vec::new();
        for file in report_files(&dir)? {
            retry.extend(read(&file)?);
        }
        results.merge_retry(retry);
    }
    Ok(Some(results))
}

fn read(file: &Path) -> Result<Vec<TestCase>> {
    let bytes = std::fs::read(file).path(file)?;
    parse_report(&bytes).map_err(|e| JrsError::test(format!("{}: {e}", file.display())))
}

/// The `TEST-*.xml` files directly in `dir`, sorted.
fn report_files(dir: &Path) -> Result<Vec<PathBuf>> {
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .path(dir)?
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                    n.starts_with("TEST-")
                        && Path::new(n)
                            .extension()
                            .is_some_and(|e| e.eq_ignore_ascii_case("xml"))
                })
        })
        .collect();
    files.sort();
    Ok(files)
}

/// The `retry-<n>` directories in `reports`, in numeric order.
fn retry_dirs(reports: &Path) -> Result<Vec<PathBuf>> {
    let mut dirs: Vec<(u32, PathBuf)> = std::fs::read_dir(reports)
        .path(reports)?
        .filter_map(std::result::Result::ok)
        .filter_map(|entry| {
            let attempt = entry
                .file_name()
                .to_str()?
                .strip_prefix(RETRY_PREFIX)?
                .parse()
                .ok()?;
            entry.path().is_dir().then(|| (attempt, entry.path()))
        })
        .collect();
    dirs.sort();
    Ok(dirs.into_iter().map(|(_, dir)| dir).collect())
}

/// Where the text inside a `<testcase>` goes.
#[derive(Clone, Copy)]
enum Sink {
    None,
    Failure,
    Skipped,
    SystemOut,
}

/// Parse one `JUnit` XML report into its test cases, in document order.
///
/// # Errors
///
/// [`JrsError::Test`] when the XML is malformed.
pub fn parse_report(xml: &[u8]) -> Result<Vec<TestCase>> {
    let malformed = |e: &dyn std::fmt::Display| JrsError::test(format!("malformed JUnit XML: {e}"));
    let mut reader = quick_xml::Reader::from_reader(xml);
    reader.config_mut().trim_text(false);

    let mut cases = Vec::new();
    let mut current: Option<TestCase> = None;
    let mut sink = Sink::None;
    let mut system_out = String::new();
    let mut buf = Vec::new();
    loop {
        let event = reader
            .read_event_into(&mut buf)
            .map_err(|e| malformed(&e))?;
        match event {
            Event::Eof => break,
            Event::Start(e) => {
                if e.local_name().as_ref() == "testcase" {
                    current = Some(test_case(&e)?);
                    system_out.clear();
                } else if let Some(case) = current.as_mut() {
                    sink = open_child(case, &e)?;
                }
            }
            Event::Empty(e) => {
                if e.local_name().as_ref() == "testcase" {
                    cases.push(test_case(&e)?);
                } else if let Some(case) = current.as_mut() {
                    open_child(case, &e)?;
                }
            }
            Event::Text(t) => append(&mut current, sink, &mut system_out, &t.xml10_content()),
            Event::CData(t) => append(&mut current, sink, &mut system_out, &t.into_inner()),
            Event::GeneralRef(r) => {
                append(&mut current, sink, &mut system_out, &resolve_reference(&r));
            }
            Event::End(e) => {
                if e.local_name().as_ref() == "testcase"
                    && let Some(mut case) = current.take()
                {
                    finish(&mut case, &system_out);
                    cases.push(case);
                }
                sink = Sink::None;
            }
            _ => {}
        }
        buf.clear();
    }
    Ok(cases)
}

fn test_case(e: &BytesStart<'_>) -> Result<TestCase> {
    Ok(TestCase {
        class: attribute(e, "classname")?.unwrap_or_default(),
        name: attribute(e, "name")?.unwrap_or_default(),
        millis: attribute(e, "time")?.as_deref().map_or(0, millis),
        status: Status::Passed,
        failure: None,
        skip_reason: None,
        unique_id: None,
        display_name: None,
        attempts: 1,
    })
}

/// A `<failure>`, `<error>`, `<skipped>` or `<system-out>` inside a test case.
fn open_child(case: &mut TestCase, e: &BytesStart<'_>) -> Result<Sink> {
    Ok(match e.local_name().as_ref() {
        name @ ("failure" | "error") => {
            case.status = Status::Failed;
            case.failure = Some(Failure {
                error: name == "error",
                kind: attribute(e, "type")?.unwrap_or_default(),
                message: attribute(e, "message")?.unwrap_or_default(),
                trace: String::new(),
            });
            Sink::Failure
        }
        "skipped" => {
            case.status = Status::Skipped;
            case.skip_reason = Some(attribute(e, "message")?.unwrap_or_default());
            Sink::Skipped
        }
        "system-out" => Sink::SystemOut,
        _ => Sink::None,
    })
}

fn append(current: &mut Option<TestCase>, sink: Sink, system_out: &mut String, text: &str) {
    let Some(case) = current.as_mut() else { return };
    match sink {
        Sink::Failure => {
            if let Some(failure) = case.failure.as_mut() {
                failure.trace.push_str(text);
            }
        }
        Sink::Skipped => case.skip_reason.get_or_insert_default().push_str(text),
        Sink::SystemOut => system_out.push_str(text),
        Sink::None => {}
    }
}

/// Tidy a finished case, and take its unique ID and display name out of the
/// `<system-out>` the launcher recorded them in.
fn finish(case: &mut TestCase, system_out: &str) {
    for line in system_out.lines() {
        if let Some(id) = line.strip_prefix("unique-id: ") {
            case.unique_id = Some(id.trim().to_string());
        } else if let Some(name) = line.strip_prefix("display-name: ") {
            case.display_name = Some(name.trim().to_string());
        }
    }
    if let Some(failure) = case.failure.as_mut() {
        failure.trace = failure.trace.trim().to_string();
    }
    if let Some(reason) = case.skip_reason.take() {
        let reason = reason.trim();
        case.skip_reason = (!reason.is_empty()).then(|| reason.to_string());
    }
}

fn attribute(e: &BytesStart<'_>, name: &str) -> Result<Option<String>> {
    for attr in e.attributes() {
        let attr = attr.map_err(|err| JrsError::test(format!("malformed JUnit XML: {err}")))?;
        if attr.key.local_name().as_ref() == name {
            let value = attr
                .normalized_value(XmlVersion::Implicit1_0)
                .map_err(|err| JrsError::test(format!("malformed JUnit XML: {err}")))?;
            return Ok(Some(value.into_owned()));
        }
    }
    Ok(None)
}

/// `&lt;` and friends, and character references, in text.
fn resolve_reference(r: &BytesRef<'_>) -> String {
    if let Ok(Some(c)) = r.resolve_char_ref() {
        return c.to_string();
    }
    match &**r {
        "lt" => "<",
        "gt" => ">",
        "amp" => "&",
        "quot" => "\"",
        "apos" => "'",
        _ => "",
    }
    .to_string()
}

/// `0.031` seconds as 31 milliseconds, without going through a float.
fn millis(time: &str) -> u64 {
    let time = time.trim();
    let (whole, fraction) = time.split_once('.').unwrap_or((time, ""));
    let whole: u64 = whole.parse().unwrap_or(0);
    let fraction: String = fraction
        .chars()
        .chain(std::iter::repeat('0'))
        .take(3)
        .collect();
    whole
        .saturating_mul(1000)
        .saturating_add(fraction.parse().unwrap_or(0))
}

// ---- selecting tests again --------------------------------------------------

/// Launcher selectors for a set of tests: `--select-method`,
/// `--select-unique-id` and `--select-class`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Selection {
    pub methods: Vec<String>,
    pub unique_ids: Vec<String>,
    pub classes: Vec<String>,
}

impl Selection {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.methods.is_empty() && self.unique_ids.is_empty() && self.classes.is_empty()
    }
}

/// The selectors that run `cases` again, and nothing else.
///
/// - A test that is a plain method — its unique ID ends in `[method:…]` —
///   becomes `--select-method Class#method(params)`, through the same path as
///   `jrs test --method`. That covers nested classes, whose `classname` is the
///   binary name `Outer$Inner`, and takes the parameter types from the unique
///   ID, which spells them out in full where the `name` attribute does not.
/// - Anything else the XML has a unique ID for — one invocation of a
///   parameterised test, one dynamic test, a `JUnit` 4 test on Vintage, a Spock
///   feature — is selected by that ID, so a retry reruns the one invocation
///   that failed, not all of them.
/// - A case with no unique ID at all falls back to its whole class.
#[must_use]
pub fn select<'a>(cases: impl IntoIterator<Item = &'a TestCase>) -> Selection {
    let mut selection = Selection::default();
    let push = |list: &mut Vec<String>, item: String| {
        if !list.contains(&item) {
            list.push(item);
        }
    };
    for case in cases {
        if let Some(method) = method_selector(case) {
            push(&mut selection.methods, method);
        } else if let Some(id) = &case.unique_id {
            push(&mut selection.unique_ids, id.clone());
        } else if !case.class.is_empty() {
            push(&mut selection.classes, case.class.clone());
        }
    }
    selection
}

fn method_selector(case: &TestCase) -> Option<String> {
    let id = case.unique_id.as_deref()?;
    let last = id.rsplit("]/[").next()?;
    let method = last
        .trim_start_matches('[')
        .strip_prefix("method:")?
        .strip_suffix(']')?;
    (!case.class.is_empty()).then(|| format!("{}#{method}", case.class))
}

// ---- the HTML page ----------------------------------------------------------

/// Write `index.html` for `results` into `reports`, and return its path.
///
/// # Errors
///
/// [`JrsError::Io`] if the page cannot be written.
pub fn write_html(reports: &Path, project: &str, results: &Results) -> Result<PathBuf> {
    let index = reports.join(INDEX);
    std::fs::write(&index, render_html(project, results)).path(&index)?;
    Ok(index)
}

/// Counts over some test cases.
#[derive(Debug, Default, Clone, Copy)]
struct Tally {
    tests: usize,
    passed: usize,
    failed: usize,
    flaky: usize,
    skipped: usize,
    millis: u64,
}

impl Tally {
    fn of<'a>(cases: impl IntoIterator<Item = &'a TestCase>) -> Tally {
        let mut tally = Tally::default();
        for case in cases {
            tally.tests += 1;
            tally.millis = tally.millis.saturating_add(case.millis);
            match case.status {
                Status::Passed => tally.passed += 1,
                Status::Failed => tally.failed += 1,
                Status::Flaky => tally.flaky += 1,
                Status::Skipped => tally.skipped += 1,
            }
        }
        tally
    }

    /// The worst status among the cases, which decides where a class sorts.
    fn worst(&self) -> Status {
        if self.failed > 0 {
            Status::Failed
        } else if self.flaky > 0 {
            Status::Flaky
        } else if self.passed == 0 && self.skipped > 0 {
            Status::Skipped
        } else {
            Status::Passed
        }
    }
}

const STYLE: &str = "
:root { --bg: #fbfbf9; --fg: #1d1f21; --muted: #6a6f75; --line: #e3e3de; --row: #ffffff;
  --failed: #b3261e; --failed-bg: #fbeaea; --flaky: #8a5a00; --flaky-bg: #fdf3dc;
  --passed: #1e6b3a; --skipped: #5f6368; --code: #f3f3ef; color-scheme: light dark; }
@media (prefers-color-scheme: dark) {
  :root { --bg: #16181a; --fg: #e6e6e3; --muted: #9a9fa5; --line: #2c2f33; --row: #1d2023;
    --failed: #f28b82; --failed-bg: #3a1f1d; --flaky: #f6c26b; --flaky-bg: #35291a;
    --passed: #81c995; --skipped: #9aa0a6; --code: #111315; }
}
* { box-sizing: border-box; }
body { margin: 0; padding: 2rem 1rem 3rem; background: var(--bg); color: var(--fg);
  font: 14px/1.45 system-ui, -apple-system, 'Segoe UI', sans-serif; }
header, main, footer { max-width: 72rem; margin: 0 auto; }
h1 { font-size: 1.5rem; margin: 0 0 .25rem; font-weight: 650; }
h1 span { color: var(--muted); font-weight: 400; }
.verdict { font-size: 1.1rem; font-weight: 600; margin: .5rem 0; }
.verdict.failed { color: var(--failed); } .verdict.passed { color: var(--passed); }
.verdict.flaky { color: var(--flaky); }
.totals { color: var(--muted); margin: 0 0 1.5rem; }
.totals b { color: var(--fg); font-weight: 600; }
.note { color: var(--muted); margin: -1rem 0 1.5rem; }
.table { overflow-x: auto; border: 1px solid var(--line); border-radius: 8px; background: var(--row); }
.row { display: grid; grid-template-columns: minmax(16rem, 1fr) repeat(5, 4.5rem) 5.5rem;
  gap: .5rem; padding: .55rem .9rem; min-width: 46rem; align-items: baseline; }
.row > span:not(:first-child) { text-align: right; font-variant-numeric: tabular-nums; }
.head { color: var(--muted); font-size: .8rem; text-transform: uppercase; letter-spacing: .04em;
  border-bottom: 1px solid var(--line); }
details.class { border-bottom: 1px solid var(--line); }
details.class:last-child { border-bottom: 0; }
summary { cursor: pointer; list-style: none; }
summary::-webkit-details-marker { display: none; }
summary .name { font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; overflow-wrap: anywhere; }
summary .name::before { content: '\\25B8'; display: inline-block; width: 1.1em; color: var(--muted); }
details[open] > summary .name::before { content: '\\25BE'; }
details.failed > summary { background: var(--failed-bg); }
details.flaky > summary { background: var(--flaky-bg); }
.zero { color: var(--muted); opacity: .5; }
.count.failed { color: var(--failed); font-weight: 600; } .count.flaky { color: var(--flaky); font-weight: 600; }
.tests { list-style: none; margin: 0; padding: .25rem .9rem .75rem 2.3rem; min-width: 46rem; }
.test { display: grid; grid-template-columns: 4.5rem minmax(0, 1fr) 5.5rem; gap: .5rem; padding: .3rem 0;
  border-top: 1px dashed var(--line); align-items: baseline; }
.test:first-child { border-top: 0; }
.test .time { text-align: right; color: var(--muted); font-variant-numeric: tabular-nums; }
.test .what { overflow-wrap: anywhere; }
.test code { font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; }
.test .detail { color: var(--muted); }
.badge { font-size: .72rem; font-weight: 700; letter-spacing: .05em; border-radius: 4px;
  padding: .1rem .35rem; text-align: center; border: 1px solid currentColor; }
.badge.failed { color: var(--failed); } .badge.flaky { color: var(--flaky); }
.badge.passed { color: var(--passed); } .badge.skipped { color: var(--skipped); }
.trace { grid-column: 2 / 4; }
.trace summary { color: var(--muted); font-size: .85rem; }
.message { color: var(--failed); margin: .15rem 0; white-space: pre-wrap; overflow-wrap: anywhere; }
pre { margin: .35rem 0 .25rem; padding: .6rem .75rem; background: var(--code); border-radius: 6px;
  overflow-x: auto; font: 12px/1.4 ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; }
footer { color: var(--muted); font-size: .8rem; margin-top: 1.5rem; }
";

/// The page for `results`: one row per class, the classes with failures first
/// and open, each failure's stack trace shown; the rest folded away.
#[must_use]
pub fn render_html(project: &str, results: &Results) -> String {
    let mut classes: BTreeMap<&str, Vec<&TestCase>> = BTreeMap::new();
    for case in &results.cases {
        classes.entry(case.class.as_str()).or_default().push(case);
    }
    let mut classes: Vec<(&str, Vec<&TestCase>, Tally)> = classes
        .into_iter()
        .map(|(class, cases)| {
            let tally = Tally::of(cases.iter().copied());
            (class, cases, tally)
        })
        .collect();
    classes.sort_by(|a, b| a.2.worst().cmp(&b.2.worst()).then(a.0.cmp(b.0)));
    let total = Tally::of(&results.cases);

    let verdict = if total.failed > 0 {
        (
            "failed",
            format!(
                "{} of {} failed",
                total.failed,
                noun(total.tests, "test", "tests")
            ),
        )
    } else if total.flaky > 0 {
        (
            "flaky",
            format!(
                "Passed, but {} only on a retry",
                noun(total.flaky, "test passed", "tests passed")
            ),
        )
    } else {
        (
            "passed",
            format!("All {} passed", noun(total.tests, "test", "tests")),
        )
    };

    let mut html = String::new();
    let _ = write!(
        html,
        "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <title>{project}: {verdict}</title>\n<style>{STYLE}</style>\n</head>\n<body>\n\
         <header>\n<h1>{project} <span>test results</span></h1>\n\
         <p class=\"verdict {status}\">{verdict}</p>\n<p class=\"totals\">",
        project = escape(project),
        verdict = escape(&verdict.1),
        status = verdict.0,
    );
    let mut parts = vec![format!("<b>{}</b>", noun(total.tests, "test", "tests"))];
    for (count, what) in [
        (total.passed, "passed"),
        (total.failed, "failed"),
        (total.flaky, "flaky"),
        (total.skipped, "skipped"),
    ] {
        if count > 0 {
            parts.push(format!("<b>{count}</b> {what}"));
        }
    }
    parts.push(format!(
        "<b>{}</b> in {}",
        seconds(total.millis),
        noun(classes.len(), "class", "classes")
    ));
    html.push_str(&parts.join(" &middot; "));
    html.push_str("</p>\n");
    if results.retries > 0 {
        let _ = writeln!(
            html,
            "<p class=\"note\">Failed tests were run again, up to {} more. \
             A test that failed and then passed is flaky; the XML of each retry is in \
             <code>{RETRY_PREFIX}&lt;n&gt;/</code>.</p>",
            noun(
                usize::try_from(results.retries).unwrap_or(usize::MAX),
                "time",
                "times"
            )
        );
    }
    html.push_str(
        "</header>\n<main>\n<div class=\"table\">\n<div class=\"row head\"><span>Class</span>\
         <span>Tests</span><span>Passed</span><span>Failed</span><span>Flaky</span>\
         <span>Skipped</span><span>Time</span></div>\n",
    );
    for (class, cases, tally) in &classes {
        render_class(&mut html, class, cases, tally);
    }
    html.push_str(
        "</div>\n</main>\n<footer>Written by jrs from the JUnit XML reports in this directory.\
         </footer>\n</body>\n</html>\n",
    );
    html
}

fn render_class(html: &mut String, class: &str, cases: &[&TestCase], tally: &Tally) {
    let worst = tally.worst();
    let open = if worst == Status::Failed { " open" } else { "" };
    let name = if class.is_empty() {
        "(no class)"
    } else {
        class
    };
    let _ = write!(
        html,
        "<details class=\"class {}\"{open}>\n<summary class=\"row\"><span class=\"name\">{}</span>\
         <span>{}</span>",
        worst.class(),
        escape(name),
        tally.tests
    );
    for (count, status) in [
        (tally.passed, Status::Passed),
        (tally.failed, Status::Failed),
        (tally.flaky, Status::Flaky),
        (tally.skipped, Status::Skipped),
    ] {
        if count == 0 {
            html.push_str("<span class=\"zero\">0</span>");
        } else {
            let _ = write!(
                html,
                "<span class=\"count {}\">{count}</span>",
                status.class()
            );
        }
    }
    let _ = writeln!(html, "<span>{}</span></summary>", seconds(tally.millis));

    let mut cases = cases.to_vec();
    cases.sort_by_key(|c| c.status);
    html.push_str("<ul class=\"tests\">\n");
    for case in cases {
        render_test(html, case);
    }
    html.push_str("</ul>\n</details>\n");
}

fn render_test(html: &mut String, case: &TestCase) {
    let badge = match case.status {
        Status::Failed if case.failure.as_ref().is_some_and(|f| f.error) => "error",
        Status::Failed => "fail",
        Status::Flaky => "flaky",
        Status::Skipped => "skip",
        Status::Passed => "pass",
    };
    let _ = write!(
        html,
        "<li class=\"test\"><span class=\"badge {}\">{}</span><span class=\"what\"><code>{}</code>",
        case.status.class(),
        badge.to_uppercase(),
        escape(&case.name)
    );
    if let Some(display) = case
        .display_name
        .as_deref()
        .filter(|d| *d != case.name && !d.is_empty())
    {
        let _ = write!(html, " <span class=\"detail\">{}</span>", escape(display));
    }
    match case.status {
        Status::Flaky => {
            let _ = write!(
                html,
                " <span class=\"detail\">passed on attempt {} of {}</span>",
                case.attempts, case.attempts
            );
        }
        Status::Failed if case.attempts > 1 => {
            let _ = write!(
                html,
                " <span class=\"detail\">failed all {} attempts</span>",
                case.attempts
            );
        }
        Status::Skipped => {
            if let Some(reason) = &case.skip_reason {
                let _ = write!(html, " <span class=\"detail\">{}</span>", escape(reason));
            }
        }
        _ => {}
    }
    let _ = write!(
        html,
        "</span><span class=\"time\">{}</span>",
        seconds(case.millis)
    );
    if let Some(failure) = &case.failure
        && matches!(case.status, Status::Failed | Status::Flaky)
    {
        // A failure's trace is what the page is for; a flaky test's is the
        // failure it recovered from, so it starts folded.
        let (open, label) = if case.status == Status::Failed {
            (" open", "stack trace")
        } else {
            ("", "the failure before it passed")
        };
        let _ = write!(
            html,
            "<details class=\"trace\"{open}><summary>{label}</summary>"
        );
        if !failure.message.is_empty() {
            let _ = write!(
                html,
                "<p class=\"message\">{}</p>",
                escape(&failure.message)
            );
        }
        let trace = if failure.trace.is_empty() {
            &failure.kind
        } else {
            &failure.trace
        };
        let _ = write!(html, "<pre>{}</pre></details>", escape(trace));
    }
    html.push_str("</li>\n");
}

/// Escape text for HTML, in content and in attributes.
#[must_use]
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

fn seconds(millis: u64) -> String {
    format!("{}.{:03}s", millis / 1000, millis % 1000)
}

fn noun(count: usize, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A report as the 1.13 launcher writes it, trimmed: properties dropped,
    /// traces shortened, the cases in its own order.
    const JUPITER: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<testsuite name="JUnit Jupiter" tests="9" skipped="1" failures="4" errors="1" time="0.031" hostname="h" timestamp="2026-09-11T11:39:16">
<properties>
<property name="line.separator" value="&#10;"/>
</properties>
<testcase name="skipped()" classname="com.example.CalcTest" time="0">
<skipped><![CDATA[not today]]></skipped>
<system-out><![CDATA[
unique-id: [engine:junit-jupiter]/[class:com.example.CalcTest]/[method:skipped()]
display-name: skipped()
]]></system-out>
</testcase>
<testcase name="dynamic()[2]" classname="com.example.CalcTest" time="0.002">
<failure message="dyn" type="org.opentest4j.AssertionFailedError"><![CDATA[org.opentest4j.AssertionFailedError: dyn
	at com.example.CalcTest.lambda$dynamic$1(CalcTest.java:46)
]]></failure>
<system-out><![CDATA[
unique-id: [engine:junit-jupiter]/[class:com.example.CalcTest]/[test-factory:dynamic()]/[dynamic-test:#2]
display-name: bad
]]></system-out>
</testcase>
<testcase name="withArgs(TestInfo)" classname="com.example.CalcTest" time="0.001">
<failure message="args" type="org.opentest4j.AssertionFailedError"><![CDATA[org.opentest4j.AssertionFailedError: args]]></failure>
<system-out><![CDATA[
unique-id: [engine:junit-jupiter]/[class:com.example.CalcTest]/[method:withArgs(org.junit.jupiter.api.TestInfo)]
display-name: withArgs(TestInfo)
]]></system-out>
</testcase>
<testcase name="param(int)[2]" classname="com.example.CalcTest" time="0">
<failure message="expected: not equal but was: &lt;2&gt;" type="org.opentest4j.AssertionFailedError"><![CDATA[org.opentest4j.AssertionFailedError: expected: not equal but was: <2>]]></failure>
<system-out><![CDATA[
unique-id: [engine:junit-jupiter]/[class:com.example.CalcTest]/[test-template:param(int)]/[test-template-invocation:#2]
display-name: [2] 2
]]></system-out>
</testcase>
<testcase name="innerFails()" classname="com.example.CalcTest$Inner" time="0">
<failure message="inner" type="org.opentest4j.AssertionFailedError"><![CDATA[org.opentest4j.AssertionFailedError: inner]]></failure>
<system-out><![CDATA[
unique-id: [engine:junit-jupiter]/[class:com.example.CalcTest]/[nested-class:Inner]/[method:innerFails()]
display-name: innerFails()
]]></system-out>
</testcase>
<testcase name="never()" classname="com.example.SetupTest" time="0">
<error message="setup broke" type="java.lang.IllegalStateException"><![CDATA[java.lang.IllegalStateException: setup broke
	at com.example.SetupTest.setup(SetupTest.java:8)
]]></error>
<system-out><![CDATA[
unique-id: [engine:junit-jupiter]/[class:com.example.SetupTest]/[method:never()]
display-name: never()
]]></system-out>
</testcase>
<testcase name="passes()" classname="com.example.CalcTest" time="1.5">
<system-out><![CDATA[
unique-id: [engine:junit-jupiter]/[class:com.example.CalcTest]/[method:passes()]
display-name: passes()
]]></system-out>
</testcase>
<testcase name="fails()" classname="com.example.CalcTest" time="0">
<failure message="one is not &lt;two&gt; &amp; more" type="org.opentest4j.AssertionFailedError">one is not &lt;two&gt; &amp; more&#10;	at com.example.CalcTest.fails(CalcTest.java:19)</failure>
<system-out><![CDATA[
unique-id: [engine:junit-jupiter]/[class:com.example.CalcTest]/[method:fails()]
display-name: fails()
]]></system-out>
</testcase>
<testcase name="bare()" classname="com.example.OtherTest" time="0.010"/>
<system-out><![CDATA[
unique-id: [engine:junit-jupiter]
display-name: JUnit Jupiter
]]></system-out>
</testsuite>
"#;

    /// A JUnit 4 test on Vintage: no `()` in its name, a runner in its ID.
    const VINTAGE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<testsuite name="JUnit Vintage" tests="2" skipped="0" failures="1" errors="0" time="0.006">
<testcase name="oldFails" classname="com.example.OldTest" time="0.002">
<failure message="expected:&lt;1&gt; but was:&lt;2&gt;" type="java.lang.AssertionError"><![CDATA[java.lang.AssertionError: expected:<1> but was:<2>]]></failure>
<system-out><![CDATA[
unique-id: [engine:junit-vintage]/[runner:com.example.OldTest]/[test:oldFails(com.example.OldTest)]
display-name: JUnit Vintage > OldTest > oldFails
]]></system-out>
</testcase>
<testcase name="oldPasses" classname="com.example.OldTest" time="0"/>
</testsuite>
"#;

    fn cases(xml: &str) -> Vec<TestCase> {
        parse_report(xml.as_bytes()).unwrap()
    }

    fn named<'a>(cases: &'a [TestCase], name: &str) -> &'a TestCase {
        cases.iter().find(|c| c.name == name).unwrap()
    }

    #[test]
    fn a_report_parses_into_its_cases() {
        let cases = cases(JUPITER);
        assert_eq!(cases.len(), 9, "the suite's own system-out is not a case");

        let fails = named(&cases, "fails()");
        assert_eq!(fails.class, "com.example.CalcTest");
        assert_eq!(fails.status, Status::Failed);
        let failure = fails.failure.as_ref().unwrap();
        assert!(!failure.error);
        assert_eq!(failure.kind, "org.opentest4j.AssertionFailedError");
        assert_eq!(failure.message, "one is not <two> & more");
        assert_eq!(
            failure.trace,
            "one is not <two> & more\n\tat com.example.CalcTest.fails(CalcTest.java:19)",
            "entities and character references in text are resolved"
        );
        assert_eq!(
            fails.unique_id.as_deref(),
            Some("[engine:junit-jupiter]/[class:com.example.CalcTest]/[method:fails()]")
        );

        let never = named(&cases, "never()");
        assert!(
            never.failure.as_ref().unwrap().error,
            "an <error> is a failure too"
        );
        assert!(
            never
                .failure
                .as_ref()
                .unwrap()
                .trace
                .ends_with("(SetupTest.java:8)")
        );

        let skipped = named(&cases, "skipped()");
        assert_eq!(skipped.status, Status::Skipped);
        assert_eq!(skipped.skip_reason.as_deref(), Some("not today"));

        assert_eq!(named(&cases, "passes()").millis, 1500);
        assert_eq!(named(&cases, "dynamic()[2]").millis, 2);
        assert_eq!(
            named(&cases, "dynamic()[2]").display_name.as_deref(),
            Some("bad")
        );
        let bare = named(&cases, "bare()");
        assert_eq!(
            (bare.status, bare.millis, bare.unique_id.as_deref()),
            (Status::Passed, 10, None),
            "an empty <testcase/> passed"
        );
        assert!(cases.iter().all(|c| c.attempts == 1));
    }

    #[test]
    fn malformed_xml_is_an_error() {
        let err = parse_report(b"<testsuite><testcase name='a'></testsuite>").unwrap_err();
        assert!(err.to_string().contains("malformed JUnit XML"), "{err}");
    }

    #[test]
    fn failures_become_the_narrowest_selectors_the_launcher_takes() {
        let mut all = cases(JUPITER);
        all.extend(cases(VINTAGE));
        // One with no unique ID at all, as another engine might write it.
        all.push(TestCase {
            class: "com.example.Mystery".into(),
            name: "what".into(),
            status: Status::Failed,
            unique_id: None,
            ..named(&all, "fails()").clone()
        });
        let results = Results {
            cases: all,
            retries: 0,
        };
        let selection = select(results.failed());
        assert_eq!(
            selection.methods,
            [
                "com.example.CalcTest#withArgs(org.junit.jupiter.api.TestInfo)",
                "com.example.CalcTest$Inner#innerFails()",
                "com.example.SetupTest#never()",
                "com.example.CalcTest#fails()",
            ],
            "plain methods, nested ones too, with their parameter types in full"
        );
        assert_eq!(
            selection.unique_ids,
            [
                "[engine:junit-jupiter]/[class:com.example.CalcTest]/[test-factory:dynamic()]/[dynamic-test:#2]",
                "[engine:junit-jupiter]/[class:com.example.CalcTest]/[test-template:param(int)]/[test-template-invocation:#2]",
                "[engine:junit-vintage]/[runner:com.example.OldTest]/[test:oldFails(com.example.OldTest)]",
            ],
            "one invocation, one dynamic test, one JUnit 4 test"
        );
        assert_eq!(selection.classes, ["com.example.Mystery"]);
        assert!(select(Vec::<&TestCase>::new()).is_empty());
    }

    #[test]
    fn a_selector_is_asked_for_once() {
        let fails = named(&cases(JUPITER), "fails()").clone();
        let selection = select([&fails, &fails]);
        assert_eq!(selection.methods.len(), 1);
    }

    #[test]
    fn a_retry_turns_a_passing_failure_flaky_and_keeps_the_rest_failing() {
        let mut results = Results {
            cases: cases(JUPITER),
            retries: 0,
        };
        let mut retry = cases(JUPITER);
        // In the retry, `fails()` passes and `withArgs` fails differently.
        for case in &mut retry {
            if case.name == "fails()" {
                case.status = Status::Passed;
                case.failure = None;
            }
            if let Some(f) = case.failure.as_mut() {
                f.message = format!("again: {}", f.message);
            }
        }
        let retry: Vec<TestCase> = retry
            .into_iter()
            .filter(|c| ["fails()", "withArgs(TestInfo)"].contains(&c.name.as_str()))
            .collect();
        results.merge_retry(retry);

        assert_eq!(results.retries, 1);
        let fails = named(&results.cases, "fails()");
        assert_eq!(fails.status, Status::Flaky);
        assert_eq!(fails.attempts, 2);
        assert_eq!(
            fails.failure.as_ref().unwrap().message,
            "one is not <two> & more",
            "a flaky test keeps the failure it recovered from"
        );
        let with_args = named(&results.cases, "withArgs(TestInfo)");
        assert_eq!(with_args.status, Status::Failed);
        assert_eq!(with_args.failure.as_ref().unwrap().message, "again: args");
        assert_eq!(named(&results.cases, "never()").attempts, 1, "not retried");
        assert_eq!(results.flaky().len(), 1);
        assert_eq!(results.failed().len(), 5);
    }

    #[test]
    fn reports_and_their_retries_load_in_order() {
        let dir = std::env::temp_dir().join(format!("jrs-test-report-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(load(&dir).unwrap(), None, "no directory, no run");
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(load(&dir).unwrap(), None, "no report, no run");

        let one = |status: &str| {
            format!(
                "<testsuite><testcase name=\"flaky()\" classname=\"A\" time=\"0.5\">{status}\
                 <system-out><![CDATA[\nunique-id: [engine:e]/[class:A]/[method:flaky()]\n]]>\
                 </system-out></testcase></testsuite>"
            )
        };
        let failed = one("<failure message=\"no\" type=\"E\">trace</failure>");
        std::fs::write(dir.join("TEST-e.xml"), &failed).unwrap();
        std::fs::write(dir.join("unrelated.xml"), "not even xml").unwrap();
        // Ten retries: `retry-10` must come after `retry-9`, not after
        // `retry-1`. The last one passes.
        for attempt in 1..=10 {
            let retry = retry_dir(&dir, attempt);
            std::fs::create_dir_all(&retry).unwrap();
            let xml = if attempt == 10 {
                one("")
            } else {
                failed.clone()
            };
            std::fs::write(retry.join("TEST-e.xml"), xml).unwrap();
        }
        let results = load(&dir).unwrap().unwrap();
        std::fs::remove_dir_all(&dir).unwrap();

        assert_eq!(results.retries, 10);
        assert_eq!(results.cases.len(), 1);
        let case = &results.cases[0];
        assert_eq!((case.status, case.attempts), (Status::Flaky, 11));
        assert_eq!(case.millis, 5500, "time adds up over the attempts");
    }

    fn page() -> String {
        let mut all = cases(JUPITER);
        all.extend(cases(VINTAGE));
        let mut results = Results {
            cases: all,
            retries: 0,
        };
        let mut flaky = named(&results.cases, "fails()").clone();
        flaky.status = Status::Passed;
        results.merge_retry(vec![flaky]);
        render_html("app <demo>", &results)
    }

    #[test]
    fn the_page_puts_failing_classes_first_and_open() {
        let html = page();
        let at = |needle: &str| {
            html.find(needle)
                .unwrap_or_else(|| panic!("{needle:?} is not on the page"))
        };
        // CalcTest, OldTest and SetupTest have failures; OtherTest passed and
        // CalcTest$Inner failed too. Failing classes sort by name, passing
        // ones after them.
        let calc = at("<span class=\"name\">com.example.CalcTest</span>");
        let inner = at("<span class=\"name\">com.example.CalcTest$Inner</span>");
        let old = at("<span class=\"name\">com.example.OldTest</span>");
        let other = at("<span class=\"name\">com.example.OtherTest</span>");
        assert!(calc < inner && inner < old && old < other, "{html}");
        assert!(html.contains("<details class=\"class failed\" open>"));
        assert!(
            html.contains("<details class=\"class passed\">"),
            "passing classes fold"
        );
        assert!(html.contains("<details class=\"trace\" open><summary>stack trace"));
        assert!(
            html.contains("(SetupTest.java:8)"),
            "stack traces are on the page"
        );
        assert!(html.contains("<p class=\"verdict failed\">6 of 11 tests failed</p>"));
    }

    #[test]
    fn the_page_marks_flaky_tests() {
        let html = page();
        assert!(html.contains(
            "<span class=\"badge flaky\">FLAKY</span><span class=\"what\"><code>fails()</code>"
        ));
        assert!(html.contains("passed on attempt 2 of 2"));
        assert!(html.contains("<b>1</b> flaky"));
        assert!(
            html.contains("retry-&lt;n&gt;/"),
            "the page says where the retries are"
        );
    }

    #[test]
    fn the_page_escapes_everything_and_reaches_for_nothing_outside() {
        let html = page();
        assert!(html.contains("<title>app &lt;demo&gt;: "));
        assert!(html.contains("expected:&lt;1&gt; but was:&lt;2&gt;"));
        assert!(html.contains("one is not &lt;two&gt; &amp; more"));
        assert!(!html.contains("<two>"));
        for outside in [
            "<script", "<link", "src=", "href=", "http://", "https://", "@import",
        ] {
            assert!(!html.contains(outside), "{outside} on the page");
        }
    }

    #[test]
    fn the_page_is_the_same_for_the_same_reports() {
        assert_eq!(page(), page());
        let passed = Results {
            cases: cases(VINTAGE)
                .into_iter()
                .filter(|c| c.status == Status::Passed)
                .collect(),
            retries: 0,
        };
        let html = render_html("app", &passed);
        assert!(html.contains("<p class=\"verdict passed\">All 1 test passed</p>"));
        assert!(!html.contains(" open>"), "nothing to unfold");
    }

    #[test]
    fn times_are_read_without_floats() {
        assert_eq!(millis("0"), 0);
        assert_eq!(millis("0.031"), 31);
        assert_eq!(millis("12.5"), 12_500);
        assert_eq!(millis("1.23456"), 1234);
        assert_eq!(millis("junk"), 0);
        assert_eq!(seconds(12_345), "12.345s");
    }
}
