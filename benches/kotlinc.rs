//! What `kotlinc` costs, and what a warm one would save
//! (specs/todo/GRADLE_GAPS.md §3.1).
//!
//! It copies the project at `JRS_BENCH_PROJECT` into a scratch directory,
//! leaving the original alone, runs `jrs build --timings --no-build-cache`
//! there once to warm everything, then edits one Kotlin source
//! `JRS_BENCH_RUNS` times each way — a body change (a comment), then an API
//! change (a new top-level function) — and builds after each. Per edit it
//! records:
//!
//! | Column | What it is |
//! | --- | --- |
//! | `kotlinc` | the `compile main: kotlinc` row of `--timings` |
//! | `floor` | the same JVM, archive and classpath compiling one empty `.kt` file |
//! | `warm` | the same compile run twice in one JVM, the second timed |
//! | `javac` | the unit's `javac` row, for scale |
//!
//! `floor` is what starting the JVM and loading the compiler cost;
//! `kotlinc − warm` is what the `--watch` worker saves. The `warm` driver is
//! a throwaway Java file compiled here, calling the compiler by reflection
//! as the worker does; it is not shipped.
//!
//! ```text
//! JRS_BENCH_PROJECT=examples/orders cargo bench --bench kotlinc
//! JRS_BENCH_PROJECT=~/corpus/ktor-sample JRS_BENCH_RUNS=7 cargo bench --bench kotlinc
//! ```
//!
//! The last line says whether §3.1's gate holds: `kotlinc − warm` at least
//! one second and at least 40% of `kotlinc`, for a body edit.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Times `K2JVMCompiler.exec` twice in one JVM, printing the second in ms.
const DRIVER: &str = r#"
import java.io.PrintStream;
import java.lang.reflect.Method;

public final class KotlincTwice {
    public static void main(String[] args) throws Exception {
        Class<?> compiler = Class.forName(args[0]);
        Method exec = compiler.getMethod("exec", PrintStream.class, String[].class);
        PrintStream quiet = new PrintStream(PrintStream.nullOutputStream());
        long elapsed = 0;
        for (int run = 0; run < 2; run++) {
            long start = System.nanoTime();
            Object exit = exec.invoke(compiler.getDeclaredConstructor().newInstance(), quiet,
                    new String[] {"@" + args[1]});
            elapsed = System.nanoTime() - start;
            int code = (Integer) exit.getClass().getMethod("getCode").invoke(exit);
            if (code != 0) {
                System.err.println("kotlinc exited with " + code);
                System.exit(1);
            }
        }
        System.out.println(elapsed / 1_000_000);
    }
}
"#;

fn main() {
    let Some(source) = std::env::var_os("JRS_BENCH_PROJECT").map(PathBuf::from) else {
        println!(
            "SKIPPED the kotlinc benchmark: set JRS_BENCH_PROJECT to a Kotlin project jrs can build"
        );
        return;
    };
    let runs = std::env::var("JRS_BENCH_RUNS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(5usize)
        .max(1);
    let scratch = std::env::temp_dir().join(format!("jrs-bench-kotlinc-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    let project = scratch.join("project");
    copy_project(&source, &project);
    let toolchain = jrs::toolchain::Toolchain::discover().expect("a JDK");

    let warmed = build(&project);
    assert!(
        warmed.kotlinc.is_some(),
        "{} has no `compile main: kotlinc` row: is it a Kotlin project?",
        source.display()
    );
    let file = kotlin_source(&project).expect("a .kt file under the project's sources");
    let original = std::fs::read_to_string(&file).expect("read the Kotlin source");
    let driver = compile_driver(&toolchain, &scratch);

    println!("jrs kotlinc benchmark (specs/todo/GRADLE_GAPS.md §3.1)\n");
    println!("  project  {}", source.display());
    println!(
        "  edited   {}",
        file.strip_prefix(&project).unwrap_or(&file).display()
    );
    println!("  runs     {runs} per edit, medians\n");
    println!(
        "  {:<5}  {:>9}  {:>9}  {:>9}  {:>9}  {:>16}",
        "edit", "kotlinc", "floor", "warm", "javac", "kotlinc − warm"
    );
    let mut gate = None;
    for edit in ["body", "api"] {
        let mut samples = Vec::new();
        for n in 0..runs {
            let text = match edit {
                "body" => format!("{original}\n// jrs bench edit {n}\n"),
                _ => format!("{original}\nfun jrsBenchApi{n}(): Int = {n}\n"),
            };
            std::fs::write(&file, text).expect("edit the Kotlin source");
            let built = build(&project);
            let floor = floor(&toolchain, &project, &scratch);
            let warm = warm(&toolchain, &project, &driver);
            samples.push((built, floor, warm));
        }
        std::fs::write(&file, &original).expect("restore the Kotlin source");
        let kotlinc = median(samples.iter().filter_map(|s| s.0.kotlinc));
        let javac = median(samples.iter().filter_map(|s| s.0.javac));
        let floor = median(samples.iter().filter_map(|s| s.1));
        let warm = median(samples.iter().filter_map(|s| s.2));
        let saving = match (kotlinc, warm) {
            (Some(k), Some(w)) => Some(k.saturating_sub(w)),
            _ => None,
        };
        let share = match (saving, kotlinc) {
            (Some(s), Some(k)) if !k.is_zero() => {
                format!(" ({:.0}%)", 100.0 * s.as_secs_f64() / k.as_secs_f64())
            }
            _ => String::new(),
        };
        println!(
            "  {edit:<5}  {:>9}  {:>9}  {:>9}  {:>9}  {:>16}",
            ms(kotlinc),
            ms(floor),
            ms(warm),
            ms(javac),
            format!("{}{share}", ms(saving)),
        );
        if edit == "body" {
            gate = match (saving, kotlinc) {
                (Some(s), Some(k)) => {
                    Some(s >= Duration::from_secs(1) && s.as_secs_f64() >= 0.4 * k.as_secs_f64())
                }
                _ => None,
            };
        }
    }
    println!(
        "\n  gate     {}",
        match gate {
            Some(true) => "holds: a body edit's kotlinc − warm is at least 1 s and 40% of kotlinc",
            Some(false) =>
                "does not hold: a body edit's kotlinc − warm is under 1 s or 40% of kotlinc",
            None => "unknown: a column could not be measured",
        }
    );
    let _ = std::fs::remove_dir_all(&scratch);
}

struct Built {
    kotlinc: Option<Duration>,
    javac: Option<Duration>,
}

fn build(project: &Path) -> Built {
    let status = Command::new(env!("CARGO_BIN_EXE_jrs"))
        .arg("--manifest-path")
        .arg(project)
        .args([
            "--progress",
            "never",
            "build",
            "--timings",
            "--no-build-cache",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("spawn jrs");
    assert!(
        status.success(),
        "jrs build failed in {}",
        project.display()
    );
    let timings =
        std::fs::read_to_string(project.join("target/.jrs/timings.txt")).unwrap_or_default();
    let row = |name: &str| {
        timings
            .lines()
            .filter_map(|l| l.strip_prefix(name)?.strip_prefix('\t'))
            .filter_map(|ms| ms.parse::<u64>().ok())
            .map(Duration::from_millis)
            .reduce(|a, b| a + b)
    };
    Built {
        kotlinc: row("compile main: kotlinc"),
        javac: row("compile main: javac"),
    }
}

/// The launcher's argfile as jrs wrote it, one argument a line, and the
/// compiler's.
fn argfiles(project: &Path) -> (Vec<String>, Vec<String>) {
    let read = |name: &str| -> Vec<String> {
        std::fs::read_to_string(project.join("target/.jrs").join(name))
            .unwrap_or_default()
            .lines()
            .map(unquote)
            .collect()
    };
    (
        read("kotlinc-main.args"),
        read("kotlinc-main.compiler.args"),
    )
}

fn unquote(line: &str) -> String {
    match line.strip_prefix('"').and_then(|l| l.strip_suffix('"')) {
        Some(inner) => inner.replace("\\\"", "\"").replace("\\\\", "\\"),
        None => line.to_string(),
    }
}

/// The compiler's arguments with its sources replaced by one empty file,
/// and its output sent to `out`.
fn empty_compile(compiler: &[String], empty: &Path, out: &Path) -> Vec<String> {
    let mut args = Vec::new();
    let mut i = 0;
    while i < compiler.len() {
        let arg = &compiler[i];
        if arg == "-d" {
            args.push(arg.clone());
            args.push(out.display().to_string());
            i += 2;
            continue;
        }
        if !(arg.ends_with(".kt") || arg.ends_with(".java")) {
            args.push(arg.clone());
        }
        i += 1;
    }
    args.push(empty.display().to_string());
    args
}

fn write_argfile(path: &Path, args: &[String]) {
    let text: String = args
        .iter()
        .map(|a| format!("\"{}\"\n", a.replace('\\', "\\\\").replace('"', "\\\"")))
        .collect();
    std::fs::write(path, text).expect("write an argfile");
}

/// The launcher as jrs runs it, compiling one empty file.
fn floor(
    toolchain: &jrs::toolchain::Toolchain,
    project: &Path,
    scratch: &Path,
) -> Option<Duration> {
    let (launcher, compiler) = argfiles(project);
    let empty = scratch.join("Empty.kt");
    std::fs::write(&empty, "").ok()?;
    let compiler_file = scratch.join("floor.compiler.args");
    write_argfile(
        &compiler_file,
        &empty_compile(&compiler, &empty, &scratch.join("floor-out")),
    );
    let mut args: Vec<String> = launcher
        .into_iter()
        .filter(|a| !a.starts_with('@'))
        .collect();
    args.push(format!("@{}", compiler_file.display()));
    let launcher_file = scratch.join("floor.args");
    write_argfile(&launcher_file, &args);
    let started = Instant::now();
    let status = Command::new(&toolchain.java)
        .arg(format!("@{}", launcher_file.display()))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()?;
    status.success().then(|| started.elapsed())
}

fn compile_driver(toolchain: &jrs::toolchain::Toolchain, scratch: &Path) -> PathBuf {
    let dir = scratch.join("driver");
    std::fs::create_dir_all(&dir).expect("create the driver's directory");
    let source = dir.join("KotlincTwice.java");
    std::fs::write(&source, DRIVER).expect("write the driver");
    let status = Command::new(&toolchain.javac)
        .arg("-d")
        .arg(&dir)
        .arg(&source)
        .status()
        .expect("spawn javac");
    assert!(status.success(), "the driver did not compile");
    dir
}

/// The project's compile run twice in one JVM; the second, timed by the
/// driver.
fn warm(toolchain: &jrs::toolchain::Toolchain, project: &Path, driver: &Path) -> Option<Duration> {
    let (launcher, _) = argfiles(project);
    let at = launcher.iter().position(|a| a == "-cp")?;
    let classpath = launcher.get(at + 1)?;
    let main_class = launcher.get(at + 2)?;
    let compiler_file = project.join("target/.jrs/kotlinc-main.compiler.args");
    let separator = if cfg!(windows) { ";" } else { ":" };
    let output = Command::new(&toolchain.java)
        .arg("-cp")
        .arg(format!("{}{separator}{classpath}", driver.display()))
        .arg("KotlincTwice")
        .arg(main_class)
        .arg(&compiler_file)
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let ms: u64 = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .ok()?;
    output.status.success().then(|| Duration::from_millis(ms))
}

/// The first `.kt` file under the project's sources, by path.
fn kotlin_source(project: &Path) -> Option<PathBuf> {
    let mut found = Vec::new();
    collect(&project.join("src/main"), &mut found);
    found.sort();
    found.into_iter().next()
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, out);
        } else if path.extension().is_some_and(|e| e == "kt") {
            out.push(path);
        }
    }
}

/// Copy the project, leaving out its build output and version control.
fn copy_project(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("create the copy");
    for entry in std::fs::read_dir(from).expect("read the project") {
        let entry = entry.expect("read an entry");
        let name = entry.file_name();
        if name == "target" || name == ".git" {
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

fn median(values: impl Iterator<Item = Duration>) -> Option<Duration> {
    let mut values: Vec<Duration> = values.collect();
    values.sort();
    values.get(values.len() / 2).copied()
}

fn ms(d: Option<Duration>) -> String {
    d.map_or_else(|| "—".to_string(), |d| format!("{} ms", d.as_millis()))
}
