//! Fixture Java projects, built end to end through the library API.
//!
//! These are the tests that need a JDK. CI installs one; on a machine without
//! `javac` they announce that they were skipped rather than failing quietly
//! (SPEC §10.1).

mod common;

use std::path::{Path, PathBuf};

use common::{FixtureRepo, Scratch, copy_dir, fixtures};
use jrs::cli;
use jrs::compile::{self, CompileUnit};
use jrs::manifest::Manifest;
use jrs::package::{self, JarManifest};
use jrs::project::{self, Project};
use jrs::resolve::{self, Classpath};
use jrs::runner;
use jrs::toolchain::Toolchain;
use jrs::ui::{CharsetChoice, Geometry, Ui, UiOptions, When};

fn silent_ui() -> Ui {
    Ui::new(UiOptions {
        quiet: true,
        progress: When::Never,
        color: When::Never,
        charset: CharsetChoice::Ascii,
        ..Default::default()
    })
}

/// Lay the `hello` fixture out in a scratch directory so the build can write.
fn hello_project(scratch: &Scratch) -> Manifest {
    let root = scratch.join("hello");
    copy_dir(&fixtures().join("hello"), &root);
    Manifest::load(root.join("jrs.toml")).unwrap()
}

fn compile_main(manifest: &Manifest, toolchain: &Toolchain, classpath: Vec<PathBuf>) -> usize {
    let project = Project::new(manifest);
    let sources = project.main_sources().unwrap();
    assert!(!sources.is_empty(), "the fixture has no sources");

    let unit = CompileUnit {
        label: "main".into(),
        sources,
        output_dir: project.classes_dir(),
        classpath,
        release: toolchain.release(manifest.java.source).unwrap(),
        target: manifest.java.target,
        encoding: manifest.java.encoding.clone(),
        extra_args: manifest.java.javac_args.clone(),
        work_dir: project.work_dir(),
    };
    match compile::compile(toolchain, &unit, &silent_ui()).unwrap() {
        compile::Outcome::Compiled { classes } => classes,
        compile::Outcome::UpToDate => panic!("a fresh output directory cannot be up to date"),
    }
}

#[test]
fn a_multi_file_project_compiles_and_packages() {
    let toolchain = require_jdk!();
    let scratch = Scratch::new("build-hello");
    let manifest = hello_project(&scratch);
    let project = Project::new(&manifest);

    let classes = compile_main(&manifest, &toolchain, Vec::new());
    assert_eq!(classes, 2, "both source files should produce a class");
    assert!(
        project
            .classes_dir()
            .join("com/example/Hello.class")
            .is_file()
    );
    assert!(
        project
            .classes_dir()
            .join("com/example/Greeter.class")
            .is_file()
    );

    // Resources land beside the classes, ready to be packaged.
    let copied = project::copy_tree(&manifest.resource_path(), &project.classes_dir()).unwrap();
    assert_eq!(copied, 1);

    let jar = project.jar_path();
    let outcome = package::write_thin_jar(
        &project.classes_dir(),
        &jar,
        &JarManifest {
            main_class: manifest.main_class.clone(),
            class_path: Vec::new(),
        },
    )
    .unwrap();
    assert!(jar.is_file());
    assert_eq!(
        outcome.entries, 4,
        "two classes, one resource, one manifest"
    );
}

#[test]
fn the_packaged_jar_runs() {
    let toolchain = require_jdk!();
    let scratch = Scratch::new("build-run");
    let manifest = hello_project(&scratch);
    let project = Project::new(&manifest);

    compile_main(&manifest, &toolchain, Vec::new());
    project::copy_tree(&manifest.resource_path(), &project.classes_dir()).unwrap();
    package::write_thin_jar(
        &project.classes_dir(),
        &project.jar_path(),
        &JarManifest {
            main_class: manifest.main_class.clone(),
            class_path: Vec::new(),
        },
    )
    .unwrap();

    // `java -jar` reads Main-Class out of the manifest jrs generated, and the
    // program reads the resource jrs packaged.
    let output = std::process::Command::new(&toolchain.java)
        .arg("-jar")
        .arg(project.jar_path())
        .arg("world")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "hello from a resource world"
    );
}

#[test]
fn running_the_main_class_passes_arguments_through() {
    let toolchain = require_jdk!();
    let scratch = Scratch::new("build-args");
    let manifest = hello_project(&scratch);
    let project = Project::new(&manifest);

    compile_main(&manifest, &toolchain, Vec::new());
    project::copy_tree(&manifest.resource_path(), &project.classes_dir()).unwrap();

    let args = runner::java_args(
        &[],
        &[project.classes_dir()],
        manifest.main_class.as_deref().unwrap(),
        &["everyone".to_string()],
    );
    let output = std::process::Command::new(&toolchain.java)
        .args(&args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "hello from a resource everyone"
    );
}

#[test]
fn a_second_build_is_up_to_date_and_a_touched_source_is_not() {
    let toolchain = require_jdk!();
    let scratch = Scratch::new("build-incremental");
    let manifest = hello_project(&scratch);
    let project = Project::new(&manifest);

    let unit = || CompileUnit {
        label: "main".into(),
        sources: project.main_sources().unwrap(),
        output_dir: project.classes_dir(),
        classpath: Vec::new(),
        release: toolchain.release(manifest.java.source).unwrap(),
        target: manifest.java.target,
        encoding: manifest.java.encoding.clone(),
        extra_args: manifest.java.javac_args.clone(),
        work_dir: project.work_dir(),
    };

    compile::compile(&toolchain, &unit(), &silent_ui()).unwrap();
    assert_eq!(
        compile::compile(&toolchain, &unit(), &silent_ui()).unwrap(),
        compile::Outcome::UpToDate
    );

    std::thread::sleep(std::time::Duration::from_millis(1100));
    let source = manifest.source_path().join("com/example/Greeter.java");
    let text = std::fs::read_to_string(&source).unwrap();
    std::fs::write(&source, format!("{text}\n// touched\n")).unwrap();

    assert!(matches!(
        compile::compile(&toolchain, &unit(), &silent_ui()).unwrap(),
        compile::Outcome::Compiled { .. }
    ));
}

#[test]
fn a_deleted_source_leaves_no_class_behind() {
    let toolchain = require_jdk!();
    let scratch = Scratch::new("build-deleted-source");
    let manifest = hello_project(&scratch);
    let project = Project::new(&manifest);

    let extra = manifest.source_path().join("com/example/Extra.java");
    std::fs::write(&extra, "package com.example;\nclass Extra {}\n").unwrap();
    assert_eq!(compile_main(&manifest, &toolchain, Vec::new()), 3);
    assert!(
        project
            .classes_dir()
            .join("com/example/Extra.class")
            .is_file()
    );

    // Deleting the source changes the source set, which forces a rebuild — and
    // the rebuild must not leave the orphaned class on the classpath.
    std::fs::remove_file(&extra).unwrap();
    assert_eq!(compile_main(&manifest, &toolchain, Vec::new()), 2);
    assert!(
        !project
            .classes_dir()
            .join("com/example/Extra.class")
            .exists()
    );
}

#[test]
fn a_compilation_error_fails_the_build_and_leaves_no_fingerprint() {
    let toolchain = require_jdk!();
    let scratch = Scratch::new("build-broken");
    let manifest = hello_project(&scratch);
    let project = Project::new(&manifest);

    std::fs::write(
        manifest.source_path().join("com/example/Broken.java"),
        "package com.example;\npublic class Broken { this is not java }\n",
    )
    .unwrap();

    let unit = CompileUnit {
        label: "main".into(),
        sources: project.main_sources().unwrap(),
        output_dir: project.classes_dir(),
        classpath: Vec::new(),
        release: toolchain.release(manifest.java.source).unwrap(),
        target: manifest.java.target,
        encoding: manifest.java.encoding.clone(),
        extra_args: manifest.java.javac_args.clone(),
        work_dir: project.work_dir(),
    };
    let error = compile::compile(&toolchain, &unit, &silent_ui()).unwrap_err();
    assert!(error.to_string().contains("compilation failed"), "{error}");
    assert_eq!(error.exit_code(), 1);
    assert!(
        compile::is_stale(&unit).unwrap(),
        "a failed build must not be recorded as up to date"
    );
}

#[test]
fn a_project_compiles_against_a_resolved_dependency() {
    let toolchain = require_jdk!();
    let scratch = Scratch::new("build-with-dep");
    let fixture = FixtureRepo::new(&scratch);

    // Build a real library jar with javac, and publish it into the fixture
    // repository, so the classpath is exercised for real rather than with a
    // placeholder.
    let library_src = scratch.join("library/src");
    std::fs::create_dir_all(library_src.join("org/example")).unwrap();
    std::fs::write(
        library_src.join("org/example/Shouter.java"),
        "package org.example;\npublic final class Shouter {\n\
         public static String shout(String s) { return s.toUpperCase() + \"!\"; }\n}\n",
    )
    .unwrap();
    let library_classes = scratch.join("library/classes");
    let status = std::process::Command::new(&toolchain.javac)
        .arg("-d")
        .arg(&library_classes)
        .arg(library_src.join("org/example/Shouter.java"))
        .status()
        .unwrap();
    assert!(status.success());

    let library_jar = scratch.join("shouter.jar");
    package::write_thin_jar(&library_classes, &library_jar, &JarManifest::default()).unwrap();
    let coord = jrs::resolve::coord::Coord::new("org.example", "shouter", "1.0.0");
    fixture.publish_pom(
        &coord,
        "<project><groupId>org.example</groupId><artifactId>shouter</artifactId>\
         <version>1.0.0</version></project>",
    );
    fixture.publish_jar(&coord, &std::fs::read(&library_jar).unwrap());

    // A project that uses it.
    let root = scratch.join("app");
    std::fs::create_dir_all(root.join("src/main/java/com/example")).unwrap();
    std::fs::write(
        root.join("jrs.toml"),
        format!(
            "[project]\nname = \"app\"\nversion = \"1.0.0\"\n\
             main-class = \"com.example.App\"\n\n\
             [dependencies]\n\"org.example:shouter\" = \"1.0.0\"\n\n{}",
            fixture.manifest_section()
        ),
    )
    .unwrap();
    std::fs::write(
        root.join("src/main/java/com/example/App.java"),
        "package com.example;\nimport org.example.Shouter;\n\
         public class App {\n  public static void main(String[] args) {\n\
         System.out.println(Shouter.shout(\"hi\"));\n  }\n}\n",
    )
    .unwrap();

    let manifest = Manifest::load(root.join("jrs.toml")).unwrap();
    let fetcher = fixture.fetcher();
    let mut resolution = resolve::resolve(&manifest, &fetcher, 4).unwrap();
    resolve::fetch_jars(&mut resolution, &fetcher, 4).unwrap();
    let classpath = resolution.classpath(Classpath::Compile);
    assert_eq!(classpath.len(), 1);

    compile_main(&manifest, &toolchain, classpath.clone());

    // And it runs against the same classpath.
    let project = Project::new(&manifest);
    let mut runtime = vec![project.classes_dir()];
    runtime.extend(classpath.clone());
    let output = std::process::Command::new(&toolchain.java)
        .args(runner::java_args(&[], &runtime, "com.example.App", &[]))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "HI!");

    // A fat jar of the same project runs with no classpath at all.
    let fat = scratch.join("app-fat.jar");
    package::write_fat_jar(
        &project.classes_dir(),
        &classpath,
        &fat,
        &JarManifest {
            main_class: Some("com.example.App".into()),
            class_path: Vec::new(),
        },
    )
    .unwrap();
    let output = std::process::Command::new(&toolchain.java)
        .arg("-jar")
        .arg(&fat)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "HI!");

    // So does the portable layout, moved somewhere the cache is not: its
    // Class-Path is relative to the jar.
    let shipped = scratch.join("shipped");
    let groups: Vec<(String, PathBuf)> = classpath
        .iter()
        .map(|jar| ("org.example".to_string(), jar.clone()))
        .collect();
    let class_path = package::copy_libraries(&groups, &shipped.join("lib"), "lib").unwrap();
    package::write_thin_jar(
        &project.classes_dir(),
        &shipped.join("app.jar"),
        &JarManifest {
            main_class: Some("com.example.App".into()),
            class_path,
        },
    )
    .unwrap();
    std::fs::remove_dir_all(&fixture.cache).unwrap();
    let output = std::process::Command::new(&toolchain.java)
        .arg("-jar")
        .arg(shipped.join("app.jar"))
        .current_dir(&scratch.path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "HI!");
}

#[test]
fn cleaning_removes_only_the_target_directory() {
    let toolchain = require_jdk!();
    let scratch = Scratch::new("build-clean");
    let manifest = hello_project(&scratch);
    let project = Project::new(&manifest);

    compile_main(&manifest, &toolchain, Vec::new());
    assert!(project.target_dir().is_dir());

    assert!(project.clean().unwrap());
    assert!(!project.target_dir().exists());
    assert!(
        manifest
            .source_path()
            .join("com/example/Hello.java")
            .is_file()
    );
    assert!(manifest.path.is_file());
}

// ---- tasks and hooks (TASKS.md) --------------------------------------------
//
// These drive whole commands through `cli::run_with`, as the binary would,
// with a plain captured `Ui`. Every task is a Java `script` (or `run`s `java`),
// so they need nothing but the JDK and behave the same on every CI leg; only
// the `shell` tests are per-platform.

/// Run a jrs command line against the project at `root`. Returns the exit
/// code, and what reached stdout and stderr.
fn jrs(root: &Path, args: &[&str]) -> (i32, String, String) {
    let (ui, capture) = Ui::captured(
        UiOptions {
            progress: When::Never,
            color: When::Never,
            charset: CharsetChoice::Ascii,
            ..Default::default()
        },
        Geometry {
            width: 100,
            height: 24,
        },
    );
    let mut argv = vec![
        "jrs".to_string(),
        "--manifest-path".to_string(),
        root.display().to_string(),
    ];
    argv.extend(args.iter().map(ToString::to_string));
    let code = cli::run_with(argv, &ui);
    (code, capture.stdout(), capture.stderr())
}

/// Which line of `text` first contains `needle`, for asserting order.
fn line_of(text: &str, needle: &str) -> usize {
    text.lines()
        .position(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("no line with {needle:?} in:\n{text}"))
}

const GENERATOR: &str = r#"import java.nio.file.*;

class Gen {
    public static void main(String[] a) throws Exception {
        var dir = Path.of(a[0], "com", "example");
        Files.createDirectories(dir);
        Files.writeString(dir.resolve("BuildInfo.java"),
            "package com.example;\npublic final class BuildInfo {\n"
            + "    public static final String VERSION = \"" + a[1] + "\";\n}\n");
        System.out.println("generated BuildInfo " + a[1]);
    }
}
"#;

/// A project whose `App` uses a class its `pre-compile` task generates.
/// `hooks` goes on in the `[hooks]` table, and `tables` after it.
fn generating_project(scratch: &Scratch, hooks: &str, tables: &str) -> PathBuf {
    scratch.write("app/build/Gen.java", GENERATOR);
    scratch.write(
        "app/src/main/java/com/example/App.java",
        "package com.example;\n\npublic class App {\n    public static void main(String[] args) {\n\
         \x20       System.out.println(\"version \" + BuildInfo.VERSION);\n    }\n}\n",
    );
    scratch.write(
        "app/jrs.toml",
        &format!(
            r#"[project]
name = "app"
version = "1.2.3"
main-class = "com.example.App"

[tasks.build-info]
description = "Generate BuildInfo.java"
script = "build/Gen.java"
args = ["{{target}}/generated/sources", "{{project.version}}"]
inputs = ["build/Gen.java"]
outputs = ["{{target}}/generated/sources"]
source-outputs = ["{{target}}/generated/sources"]

[hooks]
pre-compile = ["build-info"]
{hooks}
{tables}"#
        ),
    );
    scratch.join("app")
}

/// The `hello` fixture with `extra` appended to its manifest and `files`
/// written beside it.
fn hello_with(scratch: &Scratch, extra: &str, files: &[(&str, &str)]) -> PathBuf {
    let manifest = hello_project(scratch);
    let root = manifest.root.clone();
    let text = std::fs::read_to_string(&manifest.path).unwrap();
    std::fs::write(&manifest.path, format!("{text}\n{extra}")).unwrap();
    for (name, contents) in files {
        std::fs::write(root.join(name), contents).unwrap();
    }
    root
}

#[test]
fn a_pre_compile_task_generates_code_the_build_compiles() {
    let _toolchain = require_jdk!();
    let scratch = Scratch::new("tasks-generate");
    let root = generating_project(&scratch, "", "");

    let (code, stdout, stderr) = jrs(&root, &["build"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(
        line_of(&stderr, "Task build-info (pre-compile)")
            < line_of(&stderr, "Compiling app v1.2.3 (2 source files)"),
        "{stderr}"
    );
    // A hook's output is passed through on stderr, never on stdout.
    assert!(stderr.contains("generated BuildInfo 1.2.3"), "{stderr}");
    assert_eq!(stdout, "");
    assert!(
        root.join("target/classes/com/example/BuildInfo.class")
            .is_file()
    );

    // Nothing changed: the task is fresh, so javac has nothing to do either.
    let (code, _, stderr) = jrs(&root, &["build"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stderr.contains("Fresh build-info (task)"), "{stderr}");
    assert!(stderr.contains("Fresh app v1.2.3"), "{stderr}");
    assert!(!stderr.contains("Compiling"), "{stderr}");

    // `jrs clean` forgets the task's fingerprint with the rest of target/.
    assert_eq!(jrs(&root, &["clean"]).0, 0);
    let (code, _, stderr) = jrs(&root, &["build"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stderr.contains("Task build-info (pre-compile)"), "{stderr}");
}

#[test]
fn a_project_whose_sources_are_all_generated_builds() {
    let _toolchain = require_jdk!();
    let scratch = Scratch::new("tasks-all-generated");
    let root = generating_project(&scratch, "", "");
    std::fs::remove_dir_all(root.join("src")).unwrap();
    let (code, _, stderr) = jrs(&root, &["build"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stderr.contains("(1 source files)"), "{stderr}");
}

#[test]
fn a_generating_hook_keeps_jars_byte_identical() {
    let _toolchain = require_jdk!();
    let scratch = Scratch::new("tasks-determinism");
    let root = generating_project(&scratch, "", "");
    let jar = root.join("target/app-1.2.3.jar");

    assert_eq!(jrs(&root, &["package"]).0, 0);
    let first = std::fs::read(&jar).unwrap();
    assert_eq!(jrs(&root, &["clean"]).0, 0);
    std::thread::sleep(std::time::Duration::from_millis(1100));
    assert_eq!(jrs(&root, &["package"]).0, 0);
    assert_eq!(first, std::fs::read(&jar).unwrap());
}

#[test]
fn a_failing_hook_stops_the_build_and_is_not_remembered() {
    let _toolchain = require_jdk!();
    let scratch = Scratch::new("tasks-failing-hook");
    let root = hello_with(
        &scratch,
        "[tasks.fail]\nscript = \"Fail.java\"\ninputs = [\"Fail.java\"]\n\
         outputs = [\"Fail.java\"]\n\n[hooks]\npost-compile = [\"fail\"]\n",
        &[(
            "Fail.java",
            "class Fail {\n    public static void main(String[] a) {\n\
             \x20       System.err.println(\"boom from the hook\");\n\
             \x20       System.exit(3);\n    }\n}\n",
        )],
    );

    let (code, _, stderr) = jrs(&root, &["build"]);
    assert_eq!(code, 1, "{stderr}");
    assert!(
        line_of(&stderr, "boom from the hook")
            < line_of(&stderr, "error: task `fail` failed (exit code 3)"),
        "the task's output comes first, then one line saying which task it was:\n{stderr}"
    );
    assert!(!root.join("target/.jrs/tasks/fail.fingerprint").exists());
    // Its inputs and outputs are unchanged, but a failure is never fresh.
    let (_, _, stderr) = jrs(&root, &["build"]);
    assert!(stderr.contains("Task fail (post-compile)"), "{stderr}");
}

const SEEN: &str = r#"import java.nio.file.*;

class Seen {
    public static void main(String[] a) throws Exception {
        String jar = System.getenv("JRS_JAR");
        Files.writeString(Path.of(System.getenv("JRS_TARGET_DIR"), "seen.txt"),
            jar + "\n" + a[0] + "\n" + Files.isRegularFile(Path.of(jar)) + "\n"
            + System.getenv("JRS_HOOK") + "\n");
    }
}
"#;

#[test]
fn post_package_sees_the_jar_and_each_step_runs_once() {
    let _toolchain = require_jdk!();
    let scratch = Scratch::new("tasks-post-package");
    let root = hello_with(
        &scratch,
        "[tasks.checksum]\nscript = \"Seen.java\"\nargs = [\"{jar}\"]\n\n\
         [tasks.release]\ndescription = \"Package, then checksum\"\n\
         depends-on = [\"package\", \"checksum\"]\n\n\
         [hooks]\npost-package = [\"checksum\"]\n",
        &[("Seen.java", SEEN)],
    );

    // `release` reaches `checksum` twice — through `package`'s hook, and
    // directly — and it still runs once.
    let (code, _, stderr) = jrs(&root, &["task", "release"]);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stderr.matches("Task checksum").count(), 1, "{stderr}");
    assert_eq!(stderr.matches("Packaging").count(), 1, "{stderr}");
    assert!(stderr.contains("Task checksum (post-package)"), "{stderr}");
    assert!(stderr.contains("Finished task release"), "{stderr}");
    assert!(
        !stderr.contains("Finished build"),
        "no summary for a dependency"
    );

    let seen = std::fs::read_to_string(root.join("target/seen.txt")).unwrap();
    let lines: Vec<&str> = seen.lines().collect();
    assert!(lines[0].ends_with("hello-1.0.0.jar"), "{seen}");
    assert_eq!(lines[0], lines[1], "`{{jar}}` and `JRS_JAR` agree");
    assert_eq!(lines[2], "true", "the jar exists when post-package runs");
    assert_eq!(lines[3], "post-package");
}

#[test]
fn a_named_task_gets_its_arguments_and_returns_its_exit_code() {
    let _toolchain = require_jdk!();
    let scratch = Scratch::new("tasks-named");
    let root = hello_with(
        &scratch,
        "[tasks.echo]\ndescription = \"Write the arguments down\"\n\
         run = [\"java\", \"Echo.java\"]\nargs = [\"first\"]\n\n\
         [tasks.exit]\nscript = \"Exit.java\"\n",
        &[
            (
                "Echo.java",
                "import java.nio.file.*;\n\nclass Echo {\n\
                 \x20   public static void main(String[] a) throws Exception {\n\
                 \x20       var dir = Path.of(System.getenv(\"JRS_TARGET_DIR\"));\n\
                 \x20       Files.createDirectories(dir);\n\
                 \x20       Files.writeString(dir.resolve(\"args.txt\"), String.join(\"|\", a));\n\
                 \x20   }\n}\n",
            ),
            (
                "Exit.java",
                "class Exit {\n    public static void main(String[] a) {\n\
                 \x20       System.exit(4);\n    }\n}\n",
            ),
        ],
    );

    // `run` finds `java` on PATH — the project's JDK, which jrs puts first.
    let (code, _, stderr) = jrs(&root, &["task", "echo", "--", "one", "two words"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stderr.contains("Finished task echo"), "{stderr}");
    assert_eq!(
        std::fs::read_to_string(root.join("target/args.txt")).unwrap(),
        "first|one|two words"
    );

    let (code, _, stderr) = jrs(&root, &["task", "exit"]);
    assert_eq!(code, 4, "{stderr}");
    assert!(
        stderr.contains("Finished task exit exited with 4"),
        "{stderr}"
    );

    let (code, stdout, _) = jrs(&root, &["task", "--list"]);
    assert_eq!(code, 0);
    assert_eq!(stdout, "echo   Write the arguments down\nexit\n");

    let (code, _, stderr) = jrs(&root, &["task", "nope"]);
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("there is no task `nope`"), "{stderr}");
}

#[test]
fn a_pre_run_hook_prints_to_stderr_not_the_programs_stdout() {
    let _toolchain = require_jdk!();
    let scratch = Scratch::new("tasks-pre-run");
    let root = hello_with(
        &scratch,
        "[tasks.announce]\nscript = \"Announce.java\"\n\n[hooks]\npre-run = [\"announce\"]\n",
        &[(
            "Announce.java",
            "class Announce {\n    public static void main(String[] a) {\n\
             \x20       System.out.println(\"announced on stdout\");\n    }\n}\n",
        )],
    );
    let (code, stdout, stderr) = jrs(&root, &["run"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(!stdout.contains("announced"), "{stdout}");
    assert!(
        line_of(&stderr, "Task announce (pre-run)") < line_of(&stderr, "announced on stdout"),
        "{stderr}"
    );
    assert!(
        line_of(&stderr, "announced on stdout") < line_of(&stderr, "Running com.example.Hello"),
        "{stderr}"
    );
}

#[cfg(unix)]
#[test]
fn a_shell_task_gets_positional_arguments_on_unix() {
    let _toolchain = require_jdk!();
    let scratch = Scratch::new("tasks-shell-unix");
    let root = hello_with(
        &scratch,
        "[tasks.sh]\nshell = 'mkdir -p \"$JRS_TARGET_DIR\" && \
         printf \"%s %s %s\" \"$0\" \"$1\" \"$JRS_PROJECT_NAME\" > \"$JRS_TARGET_DIR/shell.txt\"'\n",
        &[],
    );
    let (code, _, stderr) = jrs(&root, &["task", "sh", "--", "hi"]);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        std::fs::read_to_string(root.join("target/shell.txt")).unwrap(),
        "sh hi hello"
    );
    let (_, stdout, _) = jrs(&root, &["task", "--list"]);
    assert_eq!(stdout, "sh   (sh)\n");
}

#[cfg(windows)]
#[test]
fn a_shell_task_runs_under_cmd_on_windows() {
    let _toolchain = require_jdk!();
    let scratch = Scratch::new("tasks-shell-windows");
    let root = hello_with(
        &scratch,
        "[tasks.sh]\nshell = 'echo %JRS_PROJECT_NAME%> \"%JRS_TARGET_DIR%\\shell.txt\"'\n",
        &[],
    );
    std::fs::create_dir_all(root.join("target")).unwrap();
    let (code, _, stderr) = jrs(&root, &["task", "sh"]);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        std::fs::read_to_string(root.join("target/shell.txt"))
            .unwrap()
            .trim(),
        "hello"
    );
}

#[test]
fn the_fixture_manifest_is_the_one_the_repository_ships() {
    // Guards against the fixture drifting from what the tests above assume.
    let manifest = Manifest::load(fixtures().join("hello/jrs.toml")).unwrap();
    assert_eq!(manifest.name, "hello");
    assert_eq!(manifest.main_class.as_deref(), Some("com.example.Hello"));
    assert_eq!(manifest.java.javac_args, vec!["-Xlint:all"]);
    assert!(manifest.warnings.is_empty());
    assert_eq!(
        project::find_by_extension(&fixtures().join("hello/src/main/java"), "java")
            .unwrap()
            .len(),
        2
    );
    assert_eq!(manifest.source_dir, Path::new("src/main/java"));
}
