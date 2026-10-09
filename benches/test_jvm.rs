//! Test JVM benchmark: what `test.share-classes` buys on a real project, and
//! whether the project's tests still pass with it (SPEC §10.2,
//! specs/FASTER_BUILDS.md §3 and §9.1, F1 and F7).
//!
//! It copies the project at `JRS_BENCH_PROJECT` into a scratch directory,
//! leaving the original alone, and runs `jrs test --all` there once per
//! mode — `share-classes` off, `true` (the dynamic archive) and `"aot"` (the
//! AOT cache, on JDK 24 and later) — after one untimed run that compiles
//! and writes the mode's archive. Every run is `--no-build-cache`, so each
//! starts the test JVM, and in one JVM unless `JRS_BENCH_FORKS` says
//! otherwise, since forks only read an archive. The shared cache is the
//! user's own (or `JRS_CACHE_DIR`), so the project's jars are not
//! downloaded again.
//!
//! ```text
//! JRS_BENCH_PROJECT=~/corpus/petclinic cargo bench --bench test_jvm
//! JRS_BENCH_PROJECT=... JRS_BENCH_RUNS=7 JRS_BENCH_FORKS=4 cargo bench --bench test_jvm
//! ```
//!
//! The table gives each mode's first run — the compile, and the archive
//! written or the AOT cache recorded and assembled, which a project pays once
//! per change of its jars — then its median wall time and `test JVM` row of
//! `--timings`, and the outcome: the exit code and the `Finished` line. A
//! mode whose outcome differs from the usual layout's is the evidence that
//! decides whether `share-classes` may become the default; a mode that kept
//! the usual layout says why (a class directory shadowing a jar, say).

use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const MODES: &[(&str, Option<&str>)] = &[
    ("off", None),
    ("archive", Some("true")),
    ("aot", Some("\"aot\"")),
];

fn main() {
    let Some(source) = std::env::var_os("JRS_BENCH_PROJECT").map(PathBuf::from) else {
        println!("SKIPPED the test JVM benchmark: set JRS_BENCH_PROJECT to a project jrs can test");
        return;
    };
    let runs = env_usize("JRS_BENCH_RUNS", 5).max(1);
    let forks = env_usize("JRS_BENCH_FORKS", 1).max(1);
    let scratch = std::env::temp_dir().join(format!("jrs-bench-test-jvm-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    let project = scratch.join("project");
    copy_project(&source, &project);
    let manifest = project.join("jrs.toml");
    let original = std::fs::read_to_string(&manifest).expect("read jrs.toml");
    let log = scratch.join("stderr.log");

    println!("jrs test JVM benchmark (SPEC §10.2)\n");
    println!("  project  {}", source.display());
    println!("  runs     {runs} per mode, medians; `jrs test --all --forks {forks}`\n");
    println!(
        "  {:<9}{:>10}{:>10}{:>10}  {:<28}archive",
        "", "first", "wall", "test JVM", "outcome"
    );
    let mut baseline: Option<String> = None;
    for (mode, value) in MODES {
        std::fs::write(&manifest, with_share_classes(&original, *value)).expect("write jrs.toml");
        let _ = std::fs::remove_dir_all(project.join("target"));
        // Compiles, and writes the mode's archive.
        let first = test(&project, forks, &log);
        let samples: Vec<Sample> = (0..runs).map(|_| test(&project, forks, &log)).collect();
        let outcome = samples[samples.len() / 2].outcome.clone();
        let same = match &baseline {
            None => {
                baseline = Some(outcome.clone());
                String::new()
            }
            Some(b) if *b == outcome => String::new(),
            Some(_) => "  ← differs from `off`".to_string(),
        };
        println!(
            "  {mode:<9}{:>10}{:>10}{:>10}  {:<28}{}{same}",
            ms(first.wall),
            ms(median(samples.iter().map(|s| s.wall))),
            ms(median(samples.iter().map(|s| s.jvm))),
            outcome,
            samples[samples.len() / 2]
                .archive
                .clone()
                .or(first.archive)
                .unwrap_or_default(),
        );
    }
    let _ = std::fs::remove_dir_all(&scratch);
}

struct Sample {
    wall: Duration,
    /// The `test JVM` row of `--timings`.
    jvm: Duration,
    /// The exit code and the `Finished` line.
    outcome: String,
    /// What `-v` said of the archive, or why there was none.
    archive: Option<String>,
}

fn test(project: &Path, forks: usize, log: &Path) -> Sample {
    let stderr = File::create(log).expect("create the stderr log");
    let start = Instant::now();
    let status = Command::new(env!("CARGO_BIN_EXE_jrs"))
        .arg("--manifest-path")
        .arg(project)
        .args([
            "test",
            "--all",
            "--no-build-cache",
            "--timings",
            "--verbose",
        ])
        .args(["--progress", "never", "--forks", &forks.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(stderr)
        .status()
        .expect("spawn jrs");
    let wall = start.elapsed();
    let text = std::fs::read_to_string(log).unwrap_or_default();
    let timings =
        std::fs::read_to_string(project.join("target/.jrs/timings.txt")).unwrap_or_default();
    let jvm = timings
        .lines()
        .filter_map(|l| l.strip_prefix("test JVM\t"))
        .filter_map(|ms| ms.parse::<u64>().ok())
        .map(Duration::from_millis)
        .sum();
    let finished = text
        .lines()
        .find_map(|l| l.trim_start().strip_prefix("Finished "))
        .map(|l| l.rsplit_once(" in ").map_or(l, |(counts, _)| counts))
        .unwrap_or("no Finished line");
    let archive = text.lines().find_map(|l| {
        l.strip_prefix("+ test JVM: ")
            .or_else(|| l.strip_prefix("+ test classes not shared: "))
            .map(|what| what.split(" /").next().unwrap_or(what).to_string())
    });
    Sample {
        wall,
        jvm,
        outcome: format!("exit {} {finished}", status.code().unwrap_or(-1)),
        archive,
    }
}

/// `manifest` with `[test] share-classes` set to `value`, or left out.
fn with_share_classes(manifest: &str, value: Option<&str>) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut in_test = false;
    let mut placed = false;
    for line in manifest.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_test = trimmed == "[test]";
        }
        if in_test && trimmed.starts_with("share-classes") {
            continue;
        }
        lines.push(line.to_string());
        if trimmed == "[test]"
            && let Some(value) = value
        {
            lines.push(format!("share-classes = {value}"));
            placed = true;
        }
    }
    if !placed && let Some(value) = value {
        lines.push(String::new());
        lines.push("[test]".to_string());
        lines.push(format!("share-classes = {value}"));
    }
    lines.join("\n") + "\n"
}

/// Copy the project, leaving out its build output and version control.
fn copy_project(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("create the copy");
    for entry in std::fs::read_dir(from).expect("read the project") {
        let entry = entry.expect("read an entry");
        let name = entry.file_name();
        if from.join("jrs.toml").is_file() && (name == "target" || name == ".git") {
            continue;
        }
        let target = to.join(&name);
        if entry.file_type().expect("a file type").is_dir() {
            copy_project(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).expect("copy a file");
        }
    }
}

fn median(values: impl Iterator<Item = Duration>) -> Duration {
    let mut values: Vec<Duration> = values.collect();
    values.sort();
    values[values.len() / 2]
}

fn ms(d: Duration) -> String {
    format!("{} ms", d.as_millis())
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}
