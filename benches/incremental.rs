//! Rebuild benchmark: how much of a rebuild after a one-file change is
//! `javac`, and what compiling file by file saves (SPEC §7.2).
//!
//! It works the way the M5 harness (`benches/resolution.rs`) does: it
//! synthesises a project, times the jrs binary end to end over fixed runs,
//! and reports medians. The project has a few hundred classes, each calling
//! up to three others, and no dependencies, so resolution costs nothing and
//! the time goes to jrs and `javac`. Every scenario edits one file,
//! alternating between two versions so that every run has a change to build:
//!
//! - a method body in the class everything else depends on, which compiles
//!   that one source;
//! - the API of a class nothing depends on;
//! - the API of the class everything depends on, which compiles every source
//!   that reaches it — here, all of them, in a second `javac` run.
//!
//! ```text
//! cargo bench --bench incremental
//! JRS_BENCH_CLASSES=1000 JRS_BENCH_RUNS=7 cargo bench --bench incremental
//! ```
//!
//! The `javac` column is the `compile main: javac` rows of `--timings`
//! (`target/.jrs/timings.txt`), summed when a build ran `javac` twice.
//! Without a JDK the benchmark says so and exits.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const PER_PACKAGE: usize = 25;

fn main() {
    let classes = env_usize("JRS_BENCH_CLASSES", 400).max(2);
    let runs = env_usize("JRS_BENCH_RUNS", 5).max(1);
    if let Err(e) = jrs::toolchain::Toolchain::discover() {
        println!("SKIPPED the rebuild benchmark: no usable JDK ({e})");
        return;
    }

    let scratch = Scratch::new();
    let project = scratch.0.join("project");
    write_project(&project, classes);
    let bench = Bench {
        project: project.clone(),
        cache: scratch.0.join("cache"),
        config: scratch.0.join("no-such-config.toml"),
        stderr: scratch.0.join("stderr.log"),
    };

    let root = source_path(&project, 0);
    let leaf = source_path(&project, classes - 1);
    let body = |on: bool| edit(&root, "return v + 0;", "return v + 10;", on);
    let api_leaf = |on: bool| edit(&leaf, EXTRA_MARK, EXTRA_METHOD, on);
    let api_root = |on: bool| edit(&root, EXTRA_MARK, EXTRA_METHOD, on);

    // One discarded build pages jrs and the JDK in and resolves the (empty)
    // graph, so the first timed run is not also measuring the disk.
    bench.build();

    let clean: Vec<Sample> = (0..runs)
        .map(|_| {
            let _ = std::fs::remove_dir_all(project.join("target"));
            bench.build()
        })
        .collect();
    let noop: Vec<Sample> = (0..runs).map(|_| bench.build()).collect();
    let scenario = |change: &dyn Fn(bool)| -> Vec<Sample> {
        let samples = (0..runs)
            .map(|run| {
                change(run % 2 == 0);
                bench.build()
            })
            .collect();
        // Back to the original, built, so the next scenario starts clean.
        change(false);
        bench.build();
        samples
    };
    let body = scenario(&body);
    let api_leaf = scenario(&api_leaf);
    let api_root = scenario(&api_root);

    println!("jrs rebuild benchmark (SPEC §7.2)\n");
    println!(
        "  project  {classes} classes in {} packages, each calling up to 3 others; no dependencies",
        classes.div_ceil(PER_PACKAGE)
    );
    println!("  runs     {runs} per scenario, medians; `--progress never`\n");
    println!(
        "  {:<34}{:>9}{:>9}{:>8}{:>10}",
        "", "wall", "javac", "share", "compiled"
    );
    for (label, samples) in [
        ("clean build", &clean),
        ("no-op rebuild", &noop),
        ("body of the most-used class", &body),
        ("API of an unused class", &api_leaf),
        ("API of the most-used class", &api_root),
    ] {
        let wall = median(samples.iter().map(|s| s.wall));
        let javac = median(samples.iter().map(|s| s.javac));
        let share = if javac.is_zero() {
            "-".to_string()
        } else {
            format!("{:.0}%", javac.as_secs_f64() / wall.as_secs_f64() * 100.0)
        };
        let compiled = samples[samples.len() / 2].compiled.clone();
        println!(
            "  {label:<34}{:>9}{:>9}{share:>8}{compiled:>10}",
            ms(wall),
            ms(javac)
        );
    }
}

struct Bench {
    project: PathBuf,
    cache: PathBuf,
    config: PathBuf,
    stderr: PathBuf,
}

struct Sample {
    wall: Duration,
    javac: Duration,
    /// `all`, `none`, or how many sources the main unit compiled.
    compiled: String,
}

impl Bench {
    fn build(&self) -> Sample {
        let stderr = File::create(&self.stderr).expect("create the stderr log");
        let start = Instant::now();
        let status = Command::new(env!("CARGO_BIN_EXE_jrs"))
            .arg("--manifest-path")
            .arg(&self.project)
            .args(["build", "--progress", "never", "--timings", "--verbose"])
            .env("JRS_CACHE_DIR", &self.cache)
            .env("JRS_CONFIG", &self.config)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(stderr)
            .status()
            .expect("spawn jrs");
        let wall = start.elapsed();
        let log = std::fs::read_to_string(&self.stderr).unwrap_or_default();
        assert!(status.success(), "jrs build failed ({status}):\n{log}");

        let timings = std::fs::read_to_string(self.project.join("target/.jrs/timings.txt"))
            .unwrap_or_default();
        let javac = timings
            .lines()
            .filter_map(|l| l.strip_prefix("compile main: javac\t"))
            .filter_map(|ms| ms.parse::<u64>().ok())
            .map(Duration::from_millis)
            .sum();
        let compiled = if let Some(line) = log.lines().find(|l| l.contains("main unit: compiled "))
        {
            line.split("compiled ")
                .nth(1)
                .and_then(|rest| rest.split_whitespace().next())
                .unwrap_or("?")
                .to_string()
        } else if log.contains("Fresh") {
            "none".to_string()
        } else {
            "all".to_string()
        };
        Sample {
            wall,
            javac,
            compiled,
        }
    }
}

const EXTRA_MARK: &str = "    // extra\n";
const EXTRA_METHOD: &str = "    public int extra() {\n        return seed;\n    }\n";

/// Switch `file` between `off` and `on`.
fn edit(file: &Path, off: &str, on: &str, turn_on: bool) {
    let text = std::fs::read_to_string(file).expect("read a source");
    let (from, to) = if turn_on { (off, on) } else { (on, off) };
    std::fs::write(file, text.replacen(from, to, 1)).expect("write a source");
}

fn package(i: usize) -> String {
    format!("bench.p{:02}", i / PER_PACKAGE)
}

fn source_path(project: &Path, i: usize) -> PathBuf {
    project
        .join("src/main/java")
        .join(package(i).replace('.', "/"))
        .join(format!("C{i}.java"))
}

/// Class `i` calls classes `i - 1`, `i / 2` and `i / 3`, so class 0 is
/// reached from every other one.
fn write_project(project: &Path, classes: usize) {
    std::fs::create_dir_all(project).expect("create the project");
    std::fs::write(
        project.join("jrs.toml"),
        "[project]\nname = \"rebuild\"\nversion = \"1.0.0\"\n",
    )
    .expect("write jrs.toml");
    for i in 0..classes {
        let mut deps: Vec<usize> = if i == 0 {
            Vec::new()
        } else {
            vec![i - 1, i / 2, i / 3]
        };
        deps.sort_unstable();
        deps.dedup();
        let calls: String = deps
            .iter()
            .map(|&d| format!("        v += new {}.C{d}(v).value() % 7;\n", package(d)))
            .collect();
        let text = format!(
            "package {pkg};\n\nimport java.util.ArrayList;\nimport java.util.List;\n\n\
             public final class C{i} {{\n    private final int seed;\n\n    \
             public C{i}(int seed) {{\n        this.seed = seed;\n    }}\n\n    \
             public int value() {{\n        int v = seed * {i};\n{calls}        \
             return v + 0;\n    }}\n\n    \
             public List<String> words(int n) {{\n        List<String> out = new ArrayList<>();\n        \
             for (int k = 0; k < n; k++) {{\n            out.add(\"C{i}-\" + k);\n        }}\n        \
             out.sort((x, y) -> y.compareTo(x));\n        return out;\n    }}\n\
             {EXTRA_MARK}}}\n",
            pkg = package(i)
        );
        let path = source_path(project, i);
        std::fs::create_dir_all(path.parent().expect("a package dir")).expect("create a package");
        std::fs::write(path, text).expect("write a source");
    }
}

/// The benchmark's working directory, removed however `main` ends.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Scratch {
        let dir = std::env::temp_dir().join(format!("jrs-bench-rebuild-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create the scratch dir");
        Scratch(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn env_usize(name: &str, default: usize) -> usize {
    match std::env::var(name) {
        Ok(v) => v
            .parse()
            .unwrap_or_else(|_| panic!("{name} must be a whole number, got `{v}`")),
        Err(_) => default,
    }
}

fn median(durations: impl Iterator<Item = Duration>) -> Duration {
    let mut all: Vec<Duration> = durations.collect();
    all.sort();
    all[all.len() / 2]
}

fn ms(d: Duration) -> String {
    format!("{:.0} ms", d.as_secs_f64() * 1000.0)
}
