//! The real-project corpus (specs/todo/CORPUS.md, ROADMAP §1): every project
//! in `docs/corpus/corpus.toml`, cloned at its pinned commit, taken through
//! `jrs migrate`, `build`, `test`, `run` and `package` with no hand edits,
//! and checked against the project's own Maven or Gradle build — the same
//! runtime classpath, the same tests run, passed and skipped.
//!
//! ```text
//! JRS_CORPUS=all cargo bench --bench corpus
//! JRS_CORPUS=petclinic-maven,ktor-serialization cargo bench --bench corpus
//! JRS_CORPUS=all JRS_CORPUS_STEPS=migrate,build cargo bench --bench corpus
//! ```
//!
//! Without `JRS_CORPUS` it prints one `SKIPPED` line. It needs `git`, the
//! network, a JDK and, for the reference, Maven or Gradle (a project's
//! wrapper first), and drives the `jrs` binary as a user would, in the
//! project's directory, never the library in-process.
//!
//! Everything happens under `target/corpus/<run>/`, which is kept: `cache/`
//! is the run's `JRS_CACHE_DIR`, never the user's own, and each project gets
//! `clone/` (where jrs works), `reference/` (a copy of the clone the project's
//! own build runs in, so its output cannot leak into jrs's), `logs/` with one
//! log per step, and the classpath and test comparisons. The table goes to
//! stdout and to `REPORT.md`. A cell is `ok`, `skip` (a missing need), `·`
//! (an earlier step's command failed), `-` (a step `JRS_CORPUS_STEPS` left
//! out), `F<n>` (a failure `known` maps to finding n of `docs/corpus/FINDINGS.md`)
//! or `FAIL`, which also makes the run exit 1.
//!
//! A step whose command fails stops the project there. A step whose command
//! succeeds but whose comparison with the reference does not hold fails its
//! cell and lets the next step run: a classpath difference says nothing of
//! whether the tests pass.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs::File;
use std::io::{Read as _, Write as _};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use jrs::json::Json;
use jrs::test_report::{self, Status};
use serde::Deserialize;

/// The steps in order. `clone` always runs; `JRS_CORPUS_STEPS` picks among
/// the rest.
const STEPS: [&str; 7] = [
    "clone",
    "reference",
    "migrate",
    "build",
    "test",
    "run",
    "package",
];

/// How long one command may take before its process group is killed.
const STEP_TIMEOUT: Duration = Duration::from_mins(45);

/// How long `run` and `package` wait for the probe to answer.
const PROBE_TIMEOUT: Duration = Duration::from_mins(2);

/// Pinned, so the project's own pin cannot change the output format.
const DEPENDENCY_PLUGIN: &str = "org.apache.maven.plugins:maven-dependency-plugin:3.8.1";

/// The Gradle init script the reference runs with: a task in every project
/// that appends its runtime classpath to the file `-PjrsCorpusClasspath`
/// names, one `group:artifact:version[:classifier]` per line.
const GRADLE_INIT: &str = "\
// Written by jrs's corpus harness (benches/corpus.rs).
allprojects {
    tasks.register('jrsCorpusClasspath') {
        def configuration = project.configurations.findByName('runtimeClasspath')
        def out = new File(project.property('jrsCorpusClasspath').toString())
        doLast {
            if (configuration == null) return
            configuration.resolvedConfiguration.resolvedArtifacts.each { a ->
                def id = a.moduleVersion.id
                out << \"${id.group}:${id.name}:${id.version}${a.classifier ? ':' + a.classifier : ''}\\n\"
            }
        }
    }
}
";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Corpus {
    project: Vec<Entry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    name: String,
    repo: String,
    commit: String,
    #[serde(default = "here")]
    dir: String,
    build: Build,
    kind: String,
    jdk: u32,
    run: Option<Probe>,
    package: Option<Probe>,
    member: Option<String>,
    #[serde(default)]
    needs: Vec<String>,
    #[serde(default)]
    known: BTreeMap<String, u32>,
    #[serde(default)]
    exclude: Vec<String>,
}

fn here() -> String {
    ".".to_string()
}

#[derive(Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Build {
    Maven,
    Gradle,
}

impl Build {
    fn name(self) -> &'static str {
        match self {
            Build::Maven => "maven",
            Build::Gradle => "gradle",
        }
    }
}

/// How to tell the application started: an HTTP probe and the status it
/// should answer, or a substring of its output. `fat` belongs to `package`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Probe {
    #[serde(rename = "probe")]
    url: Option<String>,
    expect: Option<u16>,
    stdout: Option<String>,
    #[serde(default)]
    #[allow(
        dead_code,
        reason = "a fat jar is the only kind `package` builds today"
    )]
    fat: bool,
}

/// What the host has, read once.
struct Host {
    jdk: Option<u32>,
    docker: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Ok,
    Skip,
    /// An earlier step's command failed.
    NotRun,
    /// `JRS_CORPUS_STEPS` left the step out.
    Unselected,
    Fail(Option<u32>),
}

#[derive(Clone)]
struct Cell {
    state: State,
    /// What follows the state: `81/81`, `cp −2 +1 ~5`.
    detail: String,
}

impl Cell {
    fn new(state: State, detail: impl Into<String>) -> Cell {
        Cell {
            state,
            detail: detail.into(),
        }
    }

    fn render(&self) -> String {
        let head = match self.state {
            State::Ok if !self.detail.is_empty() && !self.detail.contains('/') => {
                return self.detail.clone();
            }
            State::Ok => "ok".to_string(),
            State::Skip => "skip".to_string(),
            State::NotRun => "·".to_string(),
            State::Unselected => "-".to_string(),
            State::Fail(Some(n)) => format!("F{n}"),
            State::Fail(None) => "FAIL".to_string(),
        };
        if self.detail.is_empty() {
            head
        } else {
            format!("{head} {}", self.detail)
        }
    }

    /// `ok` or `skip`, as the headline numbers count them.
    fn passed(&self) -> bool {
        matches!(self.state, State::Ok | State::Skip | State::Unselected)
    }
}

/// One project's row and what the report says under it.
struct Row {
    name: String,
    build: Build,
    cells: BTreeMap<&'static str, Cell>,
    notes: Vec<String>,
    wall: Duration,
    /// The project's own build failed: the list needs fixing, and the row
    /// is left out of the headline numbers.
    reference_failed: bool,
    /// The host cannot run it at all (an older JDK).
    skipped: bool,
}

fn main() {
    let Some(selection) = std::env::var("JRS_CORPUS").ok().filter(|s| !s.is_empty()) else {
        println!(
            "SKIPPED the corpus: set JRS_CORPUS to `all` or to project names from docs/corpus/corpus.toml"
        );
        return;
    };
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(repo_root.join("docs/corpus/corpus.toml"))
        .expect("read docs/corpus/corpus.toml");
    let corpus: Corpus = toml::from_str(&text).expect("parse docs/corpus/corpus.toml");
    check(&corpus);
    let entries = select(&corpus, &selection);
    let steps = selected_steps();

    let started = SystemTime::now();
    let run = repo_root.join("target/corpus").join(stamp(started, true));
    std::fs::create_dir_all(&run).expect("create the run's directory");
    let cache = run.join("cache");
    let host = Host {
        jdk: host_jdk(),
        docker: Command::new("docker")
            .arg("info")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success()),
    };
    let header = header(repo_root, started, &steps, &host);
    println!("{header}\n");
    println!("  run      {}\n", run.display());

    let mut rows = Vec::new();
    for entry in entries {
        println!("  {} …", entry.name);
        let row = project(entry, &run, &cache, &steps, &host);
        println!("  {} {}", entry.name, summary(&row));
        rows.push(row);
    }
    let report = report(&header, &rows, &run);
    println!("\n{report}");
    std::fs::write(run.join("REPORT.md"), &report).expect("write REPORT.md");
    if rows
        .iter()
        .any(|r| r.cells.values().any(|c| c.state == State::Fail(None)))
    {
        std::process::exit(1);
    }
}

/// What `corpus.toml` must hold beyond its shape.
fn check(corpus: &Corpus) {
    let mut names = BTreeSet::new();
    for e in &corpus.project {
        assert!(
            names.insert(e.name.as_str()),
            "corpus.toml: `{}` is listed twice",
            e.name
        );
        assert!(
            e.commit.len() == 40 && e.commit.chars().all(|c| c.is_ascii_hexdigit()),
            "corpus.toml: `{}`'s commit must be a full 40-character hash",
            e.name
        );
        assert!(
            ["spring-boot", "ktor", "service", "workspace"].contains(&e.kind.as_str()),
            "corpus.toml: `{}` has kind `{}`",
            e.name,
            e.kind
        );
        for need in &e.needs {
            assert!(
                need == "docker",
                "corpus.toml: `{}` needs `{need}`, which the harness does not know",
                e.name
            );
        }
        for step in e.known.keys() {
            assert!(
                STEPS[1..].contains(&step.as_str()),
                "corpus.toml: `{}` maps an unknown step `{step}`",
                e.name
            );
        }
        for probe in e.run.iter().chain(&e.package) {
            assert!(
                probe.url.is_some() != probe.stdout.is_some(),
                "corpus.toml: `{}` needs `probe` or `stdout`, one of them",
                e.name
            );
        }
    }
}

fn select<'a>(corpus: &'a Corpus, selection: &str) -> Vec<&'a Entry> {
    if selection == "all" {
        return corpus.project.iter().collect();
    }
    selection
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|name| {
            corpus
                .project
                .iter()
                .find(|e| e.name == name)
                .unwrap_or_else(|| {
                    let known: Vec<&str> = corpus.project.iter().map(|e| e.name.as_str()).collect();
                    panic!(
                        "JRS_CORPUS: no project `{name}`; the corpus has {}",
                        known.join(", ")
                    )
                })
        })
        .collect()
}

fn selected_steps() -> BTreeSet<&'static str> {
    let Ok(list) = std::env::var("JRS_CORPUS_STEPS") else {
        return STEPS.iter().copied().collect();
    };
    let mut steps: BTreeSet<&'static str> = BTreeSet::from(["clone"]);
    for name in list.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let step = STEPS.iter().find(|s| **s == name).unwrap_or_else(|| {
            panic!("JRS_CORPUS_STEPS: no step `{name}`; the steps are {STEPS:?}")
        });
        steps.insert(step);
    }
    steps
}

/// Take one project through every step.
#[allow(
    clippy::too_many_lines,
    reason = "one block per step, in the order the table has them"
)]
fn project(entry: &Entry, run: &Path, cache: &Path, steps: &BTreeSet<&str>, host: &Host) -> Row {
    let started = Instant::now();
    let dir = run.join(&entry.name);
    let clone = dir.join("clone");
    let reference = dir.join("reference");
    let logs = dir.join("logs");
    std::fs::create_dir_all(&logs).expect("create the project's log directory");
    let work = clone.join(&entry.dir);
    let mut row = Row {
        name: entry.name.clone(),
        build: entry.build,
        cells: BTreeMap::new(),
        notes: Vec::new(),
        wall: Duration::ZERO,
        reference_failed: false,
        skipped: false,
    };
    if host.jdk.is_none_or(|v| v < entry.jdk) {
        for step in STEPS {
            row.cells.insert(step, Cell::new(State::Skip, ""));
        }
        row.notes.push(format!(
            "skipped: it needs JDK {} or later, and the host has {}",
            entry.jdk,
            host.jdk
                .map_or_else(|| "none".to_string(), |v| v.to_string())
        ));
        row.skipped = true;
        return row;
    }
    let missing: Vec<&String> = entry
        .needs
        .iter()
        .filter(|n| *n == "docker" && !host.docker)
        .collect();
    let skip_tests = !missing.is_empty();
    if skip_tests {
        row.notes.push(format!(
            "the tests are skipped on both sides: the host has no {}",
            missing
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let jrs = |args: &[&str]| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_jrs"));
        command
            .args(["--progress", "never", "--color", "never"])
            .args(args)
            .current_dir(&work)
            .env("JRS_CACHE_DIR", cache);
        command
    };
    // Fails the cell, mapped to its finding when `known` has one.
    let fail =
        |step: &str, detail: String| Cell::new(State::Fail(entry.known.get(step).copied()), detail);
    let mut stopped = false;
    let record = |row: &mut Row, step: &'static str, cell: Cell| {
        if cell.state == State::Ok
            && cell.detail.is_empty()
            && let Some(n) = entry.known.get(step)
        {
            row.notes.push(format!(
                "`{step}` passes, which `known` maps to F{n}: close the finding and remove the mapping"
            ));
        }
        row.cells.insert(step, cell);
    };

    // clone
    let cloned = clone_at(entry, &clone, &logs.join("clone.log"));
    match cloned {
        Ok(()) => record(&mut row, "clone", Cell::new(State::Ok, "")),
        Err(e) => {
            record(&mut row, "clone", Cell::new(State::Fail(None), ""));
            row.notes.push(format!("clone: {e}"));
            stopped = true;
        }
    }

    // reference
    let mut reference_classpath: Option<Classpath> = None;
    let mut reference_tests: Option<Tests> = None;
    if stopped {
        record(&mut row, "reference", Cell::new(State::NotRun, ""));
    } else if !steps.contains("reference") {
        record(&mut row, "reference", Cell::new(State::Unselected, ""));
    } else {
        copy_tree(&clone, &reference);
        let out = dir.join("reference-classpath.txt");
        let _ = std::fs::remove_file(&out);
        let at = reference.join(&entry.dir);
        let mut command = reference_command(
            entry,
            &at,
            &out,
            &run.join("corpus.init.gradle"),
            skip_tests,
        );
        let result = exec(&mut command, &logs.join("reference.log"), None);
        if result.success() {
            reference_classpath = Some(read_reference_classpath(&out, entry.build));
            if !skip_tests {
                reference_tests = Some(Tests::collect(&at, entry.build.reports(), &entry.exclude));
            }
            record(&mut row, "reference", Cell::new(State::Ok, ""));
        } else {
            row.reference_failed = true;
            row.notes.push(format!(
                "reference: the project's own build {}; fix the pin in corpus.toml (log: {})",
                result.describe(),
                logs.join("reference.log").display()
            ));
            record(&mut row, "reference", Cell::new(State::Fail(None), ""));
        }
    }

    // migrate
    if stopped {
        record(&mut row, "migrate", Cell::new(State::NotRun, ""));
    } else if !steps.contains("migrate") {
        record(&mut row, "migrate", Cell::new(State::Unselected, ""));
    } else {
        let log = logs.join("migrate.log");
        let migrated = exec(
            &mut jrs(&["migrate", "--from", entry.build.name()]),
            &log,
            None,
        );
        let cell = if migrated.success() {
            let model = exec(
                &mut jrs(&["metadata", "--no-deps"]),
                &logs.join("migrate-metadata.log"),
                Some(&dir.join("metadata-no-deps.json")),
            );
            if model.success() {
                Cell::new(State::Ok, "")
            } else {
                stopped = true;
                row.notes.push(format!(
                    "migrate: `jrs metadata` rejects the jrs.toml it wrote (log: {})",
                    logs.join("migrate-metadata.log").display()
                ));
                fail("migrate", String::new())
            }
        } else {
            stopped = true;
            row.notes.push(format!(
                "migrate: `jrs migrate` {} (log: {})",
                migrated.describe(),
                log.display()
            ));
            fail("migrate", String::new())
        };
        record(&mut row, "migrate", cell);
    }

    // build
    let mut model: Option<Json> = None;
    if stopped {
        record(&mut row, "build", Cell::new(State::NotRun, ""));
    } else if !steps.contains("build") {
        record(&mut row, "build", Cell::new(State::Unselected, ""));
    } else {
        let log = logs.join("build.log");
        let built = exec(&mut jrs(&["build"]), &log, None);
        let json = dir.join("metadata.json");
        let described = built.success().then(|| {
            exec(
                &mut jrs(&["metadata"]),
                &logs.join("build-metadata.log"),
                Some(&json),
            )
        });
        let cell = if !built.success() {
            stopped = true;
            row.notes.push(format!(
                "build: `jrs build` {} (log: {})",
                built.describe(),
                log.display()
            ));
            fail("build", String::new())
        } else if !described.as_ref().is_some_and(Exec::success) {
            stopped = true;
            row.notes
                .push("build: `jrs metadata` failed after the build".to_string());
            fail("build", String::new())
        } else {
            let parsed = std::fs::read_to_string(&json)
                .ok()
                .and_then(|t| Json::parse(&t).ok());
            match (&parsed, &reference_classpath) {
                (None, _) => {
                    row.notes
                        .push("build: `jrs metadata` printed no JSON".to_string());
                    fail("build", String::new())
                }
                (Some(_), None) => {
                    row.notes
                        .push("build: the classpath is unchecked, without a reference".to_string());
                    Cell::new(State::Ok, "")
                }
                (Some(m), Some(reference)) => {
                    let diff = compare_classpaths(reference, &jrs_classpath(m), entry.build);
                    std::fs::write(dir.join("classpath.diff"), diff.render())
                        .expect("write the classpath comparison");
                    let cell = diff.cell();
                    if diff.counts() != (0, 0, 0) {
                        row.notes.push(format!(
                            "build: {} missing, {} extra, {} at another version than the reference{} ({})",
                            diff.missing.len(),
                            diff.extra.len(),
                            diff.versions.len(),
                            if entry.build == Build::Gradle && diff.missing.is_empty() && diff.extra.is_empty() {
                                ", expected of Gradle's highest-wins"
                            } else {
                                ""
                            },
                            dir.join("classpath.diff").display()
                        ));
                    }
                    match cell {
                        Some(detail) => fail("build", detail),
                        None if diff.versions.is_empty() => Cell::new(State::Ok, ""),
                        None => Cell::new(State::Ok, format!("cp ~{}", diff.versions.len())),
                    }
                }
            }
        };
        model = parsed_model(&json);
        record(&mut row, "build", cell);
    }

    // test
    if stopped {
        record(&mut row, "test", Cell::new(State::NotRun, ""));
    } else if !steps.contains("test") {
        record(&mut row, "test", Cell::new(State::Unselected, ""));
    } else if skip_tests {
        record(&mut row, "test", Cell::new(State::Skip, ""));
    } else {
        let log = logs.join("test.log");
        let tested = exec(&mut jrs(&["test", "--all"]), &log, None);
        let ours = Tests::collect(&work, Reports::Jrs, &entry.exclude);
        let detail = format!("{}/{}", ours.total().passed(), ours.total().ran());
        let mut cell = if tested.success() {
            Cell::new(State::Ok, detail.clone())
        } else {
            row.notes.push(format!(
                "test: `jrs test --all` {} (log: {})",
                tested.describe(),
                log.display()
            ));
            fail("test", detail.clone())
        };
        if let Some(reference) = &reference_tests {
            let differences = ours.compare(reference);
            if !differences.is_empty() {
                std::fs::write(dir.join("tests.diff"), differences.join("\n") + "\n")
                    .expect("write the test comparison");
                row.notes.push(format!(
                    "test: jrs {} against the reference's {}; {} class(es) differ ({})",
                    ours.total(),
                    reference.total(),
                    differences.len(),
                    dir.join("tests.diff").display()
                ));
                if cell.state == State::Ok {
                    cell = fail("test", detail);
                }
            }
        }
        // A failing suite says nothing of whether the application starts.
        record(&mut row, "test", cell);
    }

    // run
    let member: Vec<&str> = entry
        .member
        .iter()
        .flat_map(|m| ["-p", m.as_str()])
        .collect();
    if stopped {
        record(&mut row, "run", Cell::new(State::NotRun, ""));
    } else if !steps.contains("run") {
        record(&mut row, "run", Cell::new(State::Unselected, ""));
    } else if let Some(probe) = &entry.run {
        let mut args = member.clone();
        args.push("run");
        let log = logs.join("run.log");
        let cell = match started_and_answered(&mut jrs(&args), probe, &log) {
            Ok(()) => Cell::new(State::Ok, ""),
            Err(e) => {
                row.notes.push(format!("run: {e} (log: {})", log.display()));
                fail("run", String::new())
            }
        };
        record(&mut row, "run", cell);
    } else {
        row.notes.push("run: no probe in corpus.toml".to_string());
        record(&mut row, "run", Cell::new(State::Skip, ""));
    }

    // package
    if stopped {
        record(&mut row, "package", Cell::new(State::NotRun, ""));
    } else if !steps.contains("package") {
        record(&mut row, "package", Cell::new(State::Unselected, ""));
    } else if let Some(probe) = &entry.package {
        let log = logs.join("package.log");
        let mut args = member.clone();
        args.extend(["package", "--fat"]);
        let packaged = exec(&mut jrs(&args), &log, None);
        let cell = if packaged.success() {
            let model = model.or_else(|| {
                let json = dir.join("metadata-package.json");
                exec(
                    &mut jrs(&["metadata", "--no-deps"]),
                    &logs.join("package-metadata.log"),
                    Some(&json),
                );
                parsed_model(&json)
            });
            match model
                .as_ref()
                .and_then(|m| fat_jar(m, entry.member.as_deref()))
            {
                None => {
                    row.notes
                        .push("package: `jrs metadata` names no jar".to_string());
                    fail("package", String::new())
                }
                Some((jar, java)) => {
                    let log = logs.join("package-run.log");
                    let mut command = Command::new(java);
                    command.arg("-jar").arg(&jar).current_dir(&work);
                    match started_and_answered(&mut command, probe, &log) {
                        Ok(()) => Cell::new(State::Ok, ""),
                        Err(e) => {
                            row.notes.push(format!(
                                "package: `java -jar` on {}: {e} (log: {})",
                                jar.display(),
                                log.display()
                            ));
                            fail("package", String::new())
                        }
                    }
                }
            }
        } else {
            row.notes.push(format!(
                "package: `jrs package --fat` {} (log: {})",
                packaged.describe(),
                log.display()
            ));
            fail("package", String::new())
        };
        record(&mut row, "package", cell);
    } else {
        row.notes
            .push("package: no probe in corpus.toml".to_string());
        record(&mut row, "package", Cell::new(State::Skip, ""));
    }

    row.wall = started.elapsed();
    row
}

fn parsed_model(json: &Path) -> Option<Json> {
    std::fs::read_to_string(json)
        .ok()
        .and_then(|t| Json::parse(&t).ok())
}

/// `git init`, a shallow fetch of the pinned commit, and a check that the
/// tree is at it.
fn clone_at(entry: &Entry, clone: &Path, log: &Path) -> Result<(), String> {
    let _ = std::fs::remove_dir_all(clone);
    std::fs::create_dir_all(clone).map_err(|e| e.to_string())?;
    let git = |args: &[&str]| {
        let mut command = Command::new("git");
        command.arg("-C").arg(clone).args(args);
        command
    };
    for args in [
        vec!["init", "-q"],
        vec!["fetch", "-q", "--depth", "1", &entry.repo, &entry.commit],
        vec![
            "-c",
            "advice.detachedHead=false",
            "checkout",
            "-q",
            "FETCH_HEAD",
        ],
    ] {
        let result = exec(&mut git(&args), log, None);
        if !result.success() {
            return Err(format!("`git {}` {}", args.join(" "), result.describe()));
        }
    }
    let head = git(&["rev-parse", "HEAD"])
        .output()
        .map_err(|e| e.to_string())?;
    let head = String::from_utf8_lossy(&head.stdout).trim().to_string();
    if head != entry.commit {
        return Err(format!("the tree is at {head}, not {}", entry.commit));
    }
    if !clone.join(&entry.dir).is_dir() {
        return Err(format!("the clone has no `{}`", entry.dir));
    }
    Ok(())
}

/// The project's own build: the tests, then its runtime classpath into `out`.
fn reference_command(
    entry: &Entry,
    at: &Path,
    out: &Path,
    init: &Path,
    skip_tests: bool,
) -> Command {
    let wrapper = |name: &str| {
        let local = at.join(name);
        (cfg!(unix) && local.is_file()).then_some(local)
    };
    let mut command;
    match entry.build {
        Build::Maven => {
            command = Command::new(wrapper("mvnw").unwrap_or_else(|| PathBuf::from("mvn")));
            command.args(["-B", "-ntp"]);
            if skip_tests {
                command.arg("-DskipTests");
            }
            command
                .arg("test")
                .arg(format!("{DEPENDENCY_PLUGIN}:list"))
                .args([
                    "-DincludeScope=runtime",
                    "-DexcludeTransitive=false",
                    "-DappendOutput=true",
                    "-DoutputAbsoluteArtifactFilename=false",
                ])
                .arg(format!("-DoutputFile={}", out.display()));
        }
        Build::Gradle => {
            std::fs::write(init, GRADLE_INIT).expect("write the Gradle init script");
            command = Command::new(wrapper("gradlew").unwrap_or_else(|| PathBuf::from("gradle")));
            command
                .args(["--no-daemon", "--console=plain", "--no-configuration-cache"])
                .arg("--init-script")
                .arg(init)
                .arg(format!("-PjrsCorpusClasspath={}", out.display()));
            if !skip_tests {
                command.arg("test");
            }
            command.arg("jrsCorpusClasspath");
        }
    }
    command.current_dir(at);
    command
}

/// `group:artifact[:classifier]` → version, or `local:<file>` → `-` for a
/// jar without a coordinate.
type Classpath = BTreeMap<String, String>;

/// Maven's `dependency:list` lines (`g:a:type[:classifier]:v:scope`, with a
/// ` -- module …` suffix on newer plugins) or the init script's
/// `g:a:v[:classifier]`.
fn read_reference_classpath(out: &Path, build: Build) -> Classpath {
    let text = std::fs::read_to_string(out).unwrap_or_default();
    let mut classpath = Classpath::new();
    for line in text.lines() {
        let line = line.split(" -- ").next().unwrap_or(line).trim();
        let parts: Vec<&str> = line.split(':').collect();
        let (group, artifact, classifier, version) = match (build, parts.as_slice()) {
            (Build::Maven, [g, a, kind, v, _scope]) if is_jar(kind) => (*g, *a, None, *v),
            (Build::Maven, [g, a, kind, c, v, _scope]) if is_jar(kind) => (*g, *a, Some(*c), *v),
            (Build::Gradle, [g, a, v]) => (*g, *a, None, *v),
            (Build::Gradle, [g, a, v, c]) => (*g, *a, Some(*c), *v),
            _ => continue,
        };
        classpath.insert(key(group, artifact, classifier), version.to_string());
    }
    classpath
}

fn is_jar(kind: &str) -> bool {
    kind == "jar" || kind == "bundle"
}

fn key(group: &str, artifact: &str, classifier: Option<&str>) -> String {
    match classifier {
        Some(c) if !c.is_empty() => format!("{group}:{artifact}:{c}"),
        _ => format!("{group}:{artifact}"),
    }
}

/// `classpaths.runtime` of `jrs metadata`, across every member of a
/// workspace. A member's own jar is not on another's list: members are
/// class directories, so the reference's entries for them are dropped in
/// [`compare_classpaths`] by name. A single project has no members.
fn jrs_classpath(model: &Json) -> (Classpath, BTreeSet<String>) {
    let mut classpath = Classpath::new();
    let mut members = BTreeSet::new();
    let workspace = model.get("members");
    let projects: Vec<&Json> = match workspace {
        Some(list) => list.items().iter().collect(),
        None => vec![model],
    };
    for project in projects {
        // A single project's name is no sibling's: ktor-h2's `h2` would
        // drop `com.h2database:h2` from the reference.
        if let Some(name) = project
            .get("project")
            .and_then(|p| p.get("name"))
            .and_then(Json::as_str)
            .filter(|_| workspace.is_some())
        {
            members.insert(name.to_string());
        }
        let runtime = project
            .get("classpaths")
            .and_then(|c| c.get("runtime"))
            .map_or(&[][..], Json::items);
        for jar in runtime {
            if let Some(coordinate) = jar.get("coordinate").and_then(Json::as_str) {
                let parts: Vec<&str> = coordinate.split(':').collect();
                let (group, artifact, version, classifier) = match parts.as_slice() {
                    [g, a, v] => (*g, *a, *v, None),
                    [g, a, v, c] => (*g, *a, *v, Some(*c)),
                    _ => continue,
                };
                classpath.insert(key(group, artifact, classifier), version.to_string());
            } else {
                let file = jar
                    .get("jar")
                    .and_then(Json::as_str)
                    .map(|p| {
                        Path::new(p)
                            .file_name()
                            .map_or(p.to_string(), |n| n.to_string_lossy().into_owned())
                    })
                    .unwrap_or_default();
                classpath.insert(format!("local:{file}"), "-".to_string());
            }
        }
    }
    (classpath, members)
}

struct ClasspathDiff {
    missing: Vec<(String, String)>,
    extra: Vec<(String, String)>,
    /// `(key, reference's version, jrs's version)`.
    versions: Vec<(String, String, String)>,
    build: Build,
}

/// The groups of the language runtimes jrs implies
/// (`Manifest::effective_dependencies`): an extra one of these is not a
/// finding when the reference has the same group under another artifact.
const RUNTIME_GROUPS: &[&str] = &[
    "org.jetbrains.kotlin",
    "org.scala-lang",
    "org.apache.groovy",
];

fn compare_classpaths(
    reference: &Classpath,
    (ours, members): &(Classpath, BTreeSet<String>),
    build: Build,
) -> ClasspathDiff {
    let reference: Classpath = reference
        .iter()
        .filter(|(k, _)| !k.split(':').nth(1).is_some_and(|a| members.contains(a)))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let group = |k: &str| k.split(':').next().unwrap_or_default().to_string();
    let reference_groups: BTreeSet<String> = reference.keys().map(|k| group(k)).collect();
    let mut diff = ClasspathDiff {
        missing: Vec::new(),
        extra: Vec::new(),
        versions: Vec::new(),
        build,
    };
    for (k, v) in &reference {
        match ours.get(k) {
            None => diff.missing.push((k.clone(), v.clone())),
            Some(o) if o != v => diff.versions.push((k.clone(), v.clone(), o.clone())),
            Some(_) => {}
        }
    }
    for (k, v) in ours {
        let g = group(k);
        let implied = RUNTIME_GROUPS.contains(&g.as_str()) && reference_groups.contains(&g);
        if !reference.contains_key(k) && !implied {
            diff.extra.push((k.clone(), v.clone()));
        }
    }
    diff
}

impl ClasspathDiff {
    fn counts(&self) -> (usize, usize, usize) {
        (self.missing.len(), self.extra.len(), self.versions.len())
    }

    /// The failing cell's detail, or `None` when the classpath holds: for a
    /// Gradle project a version difference is expected (it mediates
    /// highest-wins, SPEC §13.6) and only recorded.
    fn cell(&self) -> Option<String> {
        let (missing, extra, versions) = self.counts();
        let fails = missing > 0 || extra > 0 || (self.build == Build::Maven && versions > 0);
        if !fails {
            return None;
        }
        let mut parts = vec!["cp".to_string()];
        if missing > 0 {
            parts.push(format!("−{missing}"));
        }
        if extra > 0 {
            parts.push(format!("+{extra}"));
        }
        if versions > 0 {
            parts.push(format!("~{versions}"));
        }
        Some(parts.join(" "))
    }

    fn render(&self) -> String {
        let mut out = String::new();
        for (k, v) in &self.missing {
            let _ = writeln!(out, "- {k} {v}    (the reference's, not jrs's)");
        }
        for (k, v) in &self.extra {
            let _ = writeln!(out, "+ {k} {v}    (jrs's, not the reference's)");
        }
        for (k, r, o) in &self.versions {
            let _ = writeln!(out, "~ {k} {r} -> {o}    (the reference's -> jrs's)");
        }
        out
    }
}

/// Where a side writes its `JUnit` XML.
#[derive(Clone, Copy)]
enum Reports {
    /// Surefire: `target/surefire-reports`.
    Surefire,
    /// Gradle: `build/test-results/test`.
    Gradle,
    /// jrs: `target/test-reports`.
    Jrs,
}

impl Build {
    fn reports(self) -> Reports {
        match self {
            Build::Maven => Reports::Surefire,
            Build::Gradle => Reports::Gradle,
        }
    }
}

impl Reports {
    fn holds(self, dir: &Path) -> bool {
        let name = |p: &Path| p.file_name().map(|n| n.to_string_lossy().into_owned());
        match self {
            Reports::Surefire => name(dir).as_deref() == Some("surefire-reports"),
            Reports::Gradle => {
                name(dir).as_deref() == Some("test")
                    && dir.parent().and_then(name).as_deref() == Some("test-results")
            }
            Reports::Jrs => name(dir).as_deref() == Some("test-reports"),
        }
    }
}

#[derive(Default, Clone, Copy, PartialEq, Eq)]
struct Counts {
    tests: usize,
    failed: usize,
    skipped: usize,
}

impl Counts {
    fn ran(self) -> usize {
        self.tests - self.skipped
    }

    fn passed(self) -> usize {
        self.ran() - self.failed
    }
}

impl std::fmt::Display for Counts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} tests, {} failed, {} skipped",
            self.tests, self.failed, self.skipped
        )
    }
}

/// Test counts by class, from every report directory under a tree.
struct Tests {
    classes: BTreeMap<String, Counts>,
}

impl Tests {
    fn collect(root: &Path, reports: Reports, exclude: &[String]) -> Tests {
        let mut classes: BTreeMap<String, Counts> = BTreeMap::new();
        let mut dirs = Vec::new();
        report_dirs(root, reports, &mut dirs);
        for dir in dirs {
            let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
                .into_iter()
                .flatten()
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name().is_some_and(|n| {
                        let n = n.to_string_lossy();
                        n.starts_with("TEST-") && n.ends_with(".xml")
                    })
                })
                .collect();
            files.sort();
            for file in files {
                let Ok(bytes) = std::fs::read(&file) else {
                    continue;
                };
                let Ok(cases) = test_report::parse_report(&bytes) else {
                    continue;
                };
                for case in cases {
                    let outer = case.class.split('$').next().unwrap_or(&case.class);
                    if exclude.iter().any(|x| x == outer) {
                        continue;
                    }
                    let counts = classes.entry(outer.to_string()).or_default();
                    counts.tests += 1;
                    match case.status {
                        Status::Failed => counts.failed += 1,
                        Status::Skipped => counts.skipped += 1,
                        Status::Flaky | Status::Passed => {}
                    }
                }
            }
        }
        Tests { classes }
    }

    fn total(&self) -> Counts {
        self.classes
            .values()
            .fold(Counts::default(), |t, c| Counts {
                tests: t.tests + c.tests,
                failed: t.failed + c.failed,
                skipped: t.skipped + c.skipped,
            })
    }

    /// One line per class whose counts differ, or which one side alone ran.
    fn compare(&self, reference: &Tests) -> Vec<String> {
        let names: BTreeSet<&String> = self
            .classes
            .keys()
            .chain(reference.classes.keys())
            .collect();
        names
            .into_iter()
            .filter_map(
                |name| match (self.classes.get(name), reference.classes.get(name)) {
                    (Some(o), Some(r)) if o == r => None,
                    (Some(o), Some(r)) => Some(format!("~ {name}: jrs {o}; the reference {r}")),
                    (Some(o), None) => Some(format!("+ {name}: jrs alone ran it ({o})")),
                    (None, Some(r)) => Some(format!("- {name}: the reference alone ran it ({r})")),
                    (None, None) => None,
                },
            )
            .collect()
    }
}

fn report_dirs(dir: &Path, reports: Reports, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut children: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .filter(|p| {
            p.file_name().is_none_or(|n| {
                ![".git", ".gradle", "node_modules", ".jrs"].contains(&&*n.to_string_lossy())
            })
        })
        .collect();
    children.sort();
    for child in children {
        if reports.holds(&child) {
            out.push(child);
        } else {
            report_dirs(&child, reports, out);
        }
    }
}

/// The fat jar `jrs package --fat` wrote, from the model, and the `java`
/// that runs it.
fn fat_jar(model: &Json, member: Option<&str>) -> Option<(PathBuf, PathBuf)> {
    let project = match (model.get("members"), member) {
        (Some(members), Some(name)) => members.items().iter().find(|m| {
            m.get("project")
                .and_then(|p| p.get("name"))
                .and_then(Json::as_str)
                == Some(name)
        })?,
        (Some(_), None) => return None,
        (None, _) => model,
    };
    let jar = PathBuf::from(project.get("jar")?.as_str()?);
    let java = project
        .get("jdk")
        .and_then(|j| j.get("java"))
        .and_then(Json::as_str)
        .map_or_else(|| PathBuf::from("java"), PathBuf::from);
    Some((jar, java))
}

/// Start `command` in the background, wait for the probe, and stop it —
/// its whole process group — on every path out.
fn started_and_answered(command: &mut Command, probe: &Probe, log: &Path) -> Result<(), String> {
    if let Some(url) = &probe.url
        && let Some(status) = http_status(url)
    {
        return Err(format!(
            "{url} already answers {status} before the application starts; is the port in use?"
        ));
    }
    let mut child = spawn(command, log, None).map_err(|e| format!("cannot start: {e}"))?;
    let deadline = Instant::now() + PROBE_TIMEOUT;
    let outcome = loop {
        let exited = child.try_wait().ok().flatten();
        if let Some(url) = &probe.url {
            let expect = probe.expect.unwrap_or(200);
            match http_status(url) {
                Some(status) if status == expect => break Ok(()),
                Some(status) => break Err(format!("{url} answered {status}, not {expect}")),
                None => {}
            }
        }
        if let Some(text) = &probe.stdout
            && std::fs::read_to_string(log).is_ok_and(|l| l.contains(text.as_str()))
        {
            break Ok(());
        }
        if let Some(status) = exited {
            break Err(format!("it exited ({status}) before the probe answered"));
        }
        if Instant::now() > deadline {
            break Err(format!(
                "the probe did not answer within {} s",
                PROBE_TIMEOUT.as_secs()
            ));
        }
        std::thread::sleep(Duration::from_millis(500));
    };
    stop(&mut child);
    outcome
}

/// The status line's code of a `GET`, or `None` while nothing listens.
fn http_status(url: &str) -> Option<u16> {
    let rest = url.strip_prefix("http://")?;
    let (host, path) = match rest.split_once('/') {
        Some((host, path)) => (host, format!("/{path}")),
        None => (rest, "/".to_string()),
    };
    let address = if host.contains(':') {
        host.to_string()
    } else {
        format!("{host}:80")
    };
    let mut stream = address
        .to_socket_addrs()
        .ok()?
        .find_map(|a| TcpStream::connect_timeout(&a, Duration::from_secs(2)).ok())?;
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .ok()?;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nAccept: */*\r\nConnection: close\r\n\r\n"
    )
    .ok()?;
    let mut head = [0u8; 64];
    let mut read = 0;
    while read < 12 {
        let n = stream.read(&mut head[read..]).ok()?;
        if n == 0 {
            break;
        }
        read += n;
    }
    String::from_utf8_lossy(&head[..read])
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

/// A finished command: its status, `None` when it could not start or was
/// killed at [`STEP_TIMEOUT`].
struct Exec {
    status: Option<ExitStatus>,
    failure: Option<String>,
}

impl Exec {
    fn success(&self) -> bool {
        self.status.is_some_and(|s| s.success())
    }

    fn describe(&self) -> String {
        match (&self.status, &self.failure) {
            (_, Some(f)) => f.clone(),
            (Some(s), None) => format!(
                "exited with {}",
                s.code().map_or_else(|| s.to_string(), |c| c.to_string())
            ),
            (None, None) => "did not finish".to_string(),
        }
    }
}

/// Run `command` to its end, its output appended to `log` (stdout to
/// `stdout` instead, when given), killed with its process group at
/// [`STEP_TIMEOUT`].
fn exec(command: &mut Command, log: &Path, stdout: Option<&Path>) -> Exec {
    let mut child = match spawn(command, log, stdout) {
        Ok(child) => child,
        Err(e) => {
            return Exec {
                status: None,
                failure: Some(format!("could not start: {e}")),
            };
        }
    };
    let deadline = Instant::now() + STEP_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return Exec {
                    status: Some(status),
                    failure: None,
                };
            }
            Ok(None) if Instant::now() > deadline => {
                stop(&mut child);
                return Exec {
                    status: None,
                    failure: Some(format!(
                        "timed out after {} min",
                        STEP_TIMEOUT.as_secs() / 60
                    )),
                };
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(200)),
            Err(e) => {
                return Exec {
                    status: None,
                    failure: Some(e.to_string()),
                };
            }
        }
    }
}

/// Start `command` in a process group of its own, so that [`stop`] reaches
/// the JVM `jrs run` or a wrapper started too.
fn spawn(command: &mut Command, log: &Path, stdout: Option<&Path>) -> std::io::Result<Child> {
    let mut file = File::options().create(true).append(true).open(log)?;
    writeln!(file, "$ {command:?}")?;
    let out = match stdout {
        Some(path) => File::create(path)?,
        None => file.try_clone()?,
    };
    command.stdin(Stdio::null()).stdout(out).stderr(file);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }
    command.spawn()
}

/// `SIGTERM` to the child's process group, so shutdown hooks run and the
/// port is free for the next step; `SIGKILL` after fifteen seconds.
fn stop(child: &mut Child) {
    #[cfg(unix)]
    {
        let Ok(group) = i32::try_from(child.id()) else {
            let _ = child.kill();
            let _ = child.wait();
            return;
        };
        // SAFETY: `kill` takes no pointers; a stale group id fails with ESRCH.
        unsafe {
            libc::kill(-group, libc::SIGTERM);
        }
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            if matches!(child.try_wait(), Ok(Some(_))) {
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        // The group outlives its leader when the JVM ignored the signal.
        // SAFETY: as above.
        unsafe {
            libc::kill(-group, libc::SIGKILL);
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Copy a tree, leaving out `.git`.
fn copy_tree(from: &Path, to: &Path) {
    let _ = std::fs::remove_dir_all(to);
    std::fs::create_dir_all(to).expect("create the copy");
    for entry in std::fs::read_dir(from).expect("read the clone") {
        let entry = entry.expect("read an entry");
        if entry.file_name() == ".git" {
            continue;
        }
        let target = to.join(entry.file_name());
        let kind = entry.file_type().expect("a file type");
        if kind.is_dir() {
            copy_tree(&entry.path(), &target);
        } else if kind.is_file() {
            // `std::fs::copy` keeps the permissions, so `mvnw` stays executable.
            std::fs::copy(entry.path(), &target).expect("copy a file");
        }
    }
}

/// The JDK's feature release, from `java -version`.
fn host_jdk() -> Option<u32> {
    let java = std::env::var_os("JAVA_HOME")
        .map(|home| PathBuf::from(home).join("bin/java"))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from("java"));
    let output = Command::new(java).arg("-version").output().ok()?;
    let text = String::from_utf8_lossy(&output.stderr);
    let version = text.split('"').nth(1)?;
    let version = version.strip_prefix("1.").unwrap_or(version);
    version.split(['.', '-', '+']).next()?.parse().ok()
}

fn jdk_version() -> String {
    Command::new("java")
        .arg("-version")
        .output()
        .ok()
        .and_then(|o| {
            String::from_utf8_lossy(&o.stderr)
                .split('"')
                .nth(1)
                .map(str::to_string)
        })
        .unwrap_or_else(|| "none".to_string())
}

fn os_version() -> String {
    let sw_vers = || {
        let output = Command::new("sw_vers")
            .arg("-productVersion")
            .output()
            .ok()?;
        Some(format!(
            "macOS {}",
            String::from_utf8_lossy(&output.stdout).trim()
        ))
    };
    let os_release = || {
        let text = std::fs::read_to_string("/etc/os-release").ok()?;
        text.lines()
            .find_map(|l| l.strip_prefix("PRETTY_NAME="))
            .map(|v| v.trim_matches('"').to_string())
    };
    match std::env::consts::OS {
        "macos" => sw_vers(),
        "linux" => os_release(),
        _ => None,
    }
    .unwrap_or_else(|| std::env::consts::OS.to_string())
}

fn header(repo_root: &Path, started: SystemTime, steps: &BTreeSet<&str>, host: &Host) -> String {
    let jrs = Command::new(env!("CARGO_BIN_EXE_jrs"))
        .arg("--version")
        .output()
        .map_or_else(
            |_| "jrs".to_string(),
            |o| String::from_utf8_lossy(&o.stdout).trim().to_string(),
        );
    let commit = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map_or_else(
            || "unknown".to_string(),
            |o| String::from_utf8_lossy(&o.stdout).trim().to_string(),
        );
    let mut header = format!(
        "jrs corpus — {jrs} ({commit}), JDK {}, {}, {}",
        jdk_version(),
        os_version(),
        stamp(started, false)
    );
    if !host.docker {
        header.push_str(", no Docker");
    }
    if steps.len() < STEPS.len() {
        let _ = write!(
            header,
            "\nsteps: {}",
            STEPS
                .iter()
                .filter(|s| steps.contains(*s))
                .copied()
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    header
}

/// The run's directory name (`2026-10-20-142501`, UTC), or its date alone.
fn stamp(at: SystemTime, with_time: bool) -> String {
    let secs = at.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let (year, month, day) = civil(days);
    let date = format!("{year:04}-{month:02}-{day:02}");
    if !with_time {
        return date;
    }
    let time = secs % 86_400;
    format!(
        "{date}-{:02}{:02}{:02}",
        time / 3600,
        time % 3600 / 60,
        time % 60
    )
}

/// Days since 1970-01-01 to a proleptic Gregorian date (Howard Hinnant's
/// `civil_from_days`).
fn civil(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

fn summary(row: &Row) -> String {
    STEPS
        .iter()
        .map(|s| {
            format!(
                "{s} {}",
                row.cells.get(s).map_or_else(String::new, Cell::render)
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn minutes(d: Duration) -> String {
    let secs = d.as_secs();
    format!("{}m{:02}s", secs / 60, secs % 60)
}

/// The table, the two headline numbers, and every row's notes.
fn report(header: &str, rows: &[Row], run: &Path) -> String {
    let columns = ["reference", "migrate", "build", "test", "run", "package"];
    let mut table = vec![{
        let mut head = vec!["project".to_string(), "build".to_string()];
        head.extend(columns.iter().map(|c| (*c).to_string()));
        head.push("time".to_string());
        head
    }];
    for row in rows {
        let mut line = vec![row.name.clone(), row.build.name().to_string()];
        if row
            .cells
            .get("clone")
            .is_some_and(|c| matches!(c.state, State::Fail(_)))
        {
            line[1] = format!("{} (clone FAIL)", row.build.name());
        }
        line.extend(
            columns
                .iter()
                .map(|c| row.cells.get(c).map_or_else(String::new, Cell::render)),
        );
        line.push(minutes(row.wall));
        table.push(line);
    }
    let widths: Vec<usize> = (0..table[0].len())
        .map(|i| {
            table
                .iter()
                .map(|r| r[i].chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect();
    let mut out = format!("```text\n{header}\n\n");
    for line in &table {
        let cells: Vec<String> = line
            .iter()
            .zip(&widths)
            .map(|(c, w)| format!("{c:<w$}"))
            .collect();
        let _ = writeln!(out, "{}", cells.join("  ").trim_end());
    }
    let counted: Vec<&Row> = rows
        .iter()
        .filter(|r| !r.reference_failed && !r.skipped)
        .collect();
    let steps = ["migrate", "build", "test", "run", "package"];
    let end_to_end = counted
        .iter()
        .filter(|r| {
            steps
                .iter()
                .all(|s| r.cells.get(s).is_some_and(Cell::passed))
        })
        .count();
    let first_try = counted
        .iter()
        .filter(|r| {
            ["migrate", "build"]
                .iter()
                .all(|s| r.cells.get(s).is_some_and(|c| c.state == State::Ok))
        })
        .count();
    let _ = writeln!(
        out,
        "\nend to end: {end_to_end} of {n}; migrate first try: {first_try} of {n}\n```",
        n = counted.len()
    );
    if rows.iter().any(|r| !r.notes.is_empty()) {
        out.push_str("\n## Notes\n");
        for row in rows.iter().filter(|r| !r.notes.is_empty()) {
            let _ = writeln!(out, "\n### {}\n", row.name);
            for note in &row.notes {
                let _ = writeln!(out, "- {note}");
            }
        }
    }
    let _ = writeln!(
        out,
        "\nLogs, classpath and test comparisons: {}",
        run.display()
    );
    out
}
