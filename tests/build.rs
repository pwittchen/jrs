//! Fixture Java projects, built end to end through the library API.
//!
//! These are the tests that need a JDK. CI installs one; on a machine without
//! `javac` they announce that they were skipped rather than failing quietly
//! (SPEC §10.1).

mod common;

use std::path::{Path, PathBuf};

use common::{FAKE_LAUNCHER_1, FAKE_LAUNCHER_6, FixtureRepo, Scratch, copy_dir, fixtures};
use jrs::cli;
use jrs::compile::{self, CompileUnit};
use jrs::manifest::Manifest;
use jrs::package::{self, JarManifest};
use jrs::project::{self, Project};
use jrs::resolve::coord::Coord;
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
    let sources = project.main_sources().unwrap().files;
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
        foreign: None,
        main_api: None,
    };
    match compile::compile(toolchain, &unit, &silent_ui()).unwrap() {
        compile::Outcome::Compiled { classes, .. } => classes,
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
            ..JarManifest::default()
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
            ..JarManifest::default()
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
        sources: project.main_sources().unwrap().files,
        output_dir: project.classes_dir(),
        classpath: Vec::new(),
        release: toolchain.release(manifest.java.source).unwrap(),
        target: manifest.java.target,
        encoding: manifest.java.encoding.clone(),
        extra_args: manifest.java.javac_args.clone(),
        work_dir: project.work_dir(),
        foreign: None,
        main_api: None,
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

const CALC_V1: &str = "package com.example;\n\npublic final class Calc {\n    \
    public static final int LIMIT = 10;\n\n    private Calc() {}\n\n    \
    public static int add(int a, int b) {\n        return a + b;\n    }\n}\n";

/// `CALC_V1`'s API with another body: a lambda and an anonymous class in it,
/// a private helper, an import and a comment.
const CALC_NEW_BODY: &str = "package com.example;\n\n\
    import java.util.function.IntBinaryOperator;\n\n// Adds, the long way round.\n\
    public final class Calc {\n    public static final int LIMIT = 10;\n\n    \
    private Calc() {}\n\n    public static int add(int a, int b) {\n        \
    IntBinaryOperator op = (x, y) -> twice(x) / 2 + y;\n        \
    Runnable noop = new Runnable() {\n            public void run() {}\n        };\n        \
    noop.run();\n        return op.applyAsInt(a, b);\n    }\n\n    \
    private static int twice(int x) {\n        return x * 2;\n    }\n}\n";

#[test]
fn tests_recompile_when_the_main_api_changes_and_only_then() {
    let toolchain = require_jdk!();
    let scratch = Scratch::new("build-compile-avoidance");
    let calc = scratch.write("app/src/main/java/com/example/Calc.java", CALC_V1);
    let check = scratch.write(
        "app/src/test/java/com/example/CalcCheck.java",
        "package com.example;\n\nclass CalcCheck {\n    int sum = Calc.add(1, 2) + Calc.LIMIT;\n}\n",
    );
    let target = scratch.join("app/target");
    let classes = target.join("classes");
    let release = toolchain.release(None).unwrap();
    let unit = |label: &str, source: &Path, output: PathBuf, classpath: Vec<PathBuf>| CompileUnit {
        label: label.into(),
        sources: vec![source.to_path_buf()],
        output_dir: output,
        classpath,
        release,
        target: None,
        encoding: "UTF-8".into(),
        extra_args: Vec::new(),
        work_dir: target.join(".jrs"),
        foreign: None,
        main_api: None,
    };
    let main = || unit("main", &calc, classes.clone(), Vec::new());
    // As `jrs test` builds it: the main classes' API, taken after main built.
    let test = || CompileUnit {
        main_api: Some(compile::api_digest(&classes).unwrap()),
        ..unit(
            "test",
            &check,
            target.join("test-classes"),
            vec![classes.clone()],
        )
    };
    let ui = silent_ui();
    let rebuild_main = |text: &str| {
        std::thread::sleep(std::time::Duration::from_millis(1100));
        std::fs::write(&calc, text).unwrap();
        assert!(matches!(
            compile::compile(&toolchain, &main(), &ui).unwrap(),
            compile::Outcome::Compiled { .. }
        ));
    };

    compile::compile(&toolchain, &main(), &ui).unwrap();
    compile::compile(&toolchain, &test(), &ui).unwrap();
    assert!(!compile::is_stale(&test()).unwrap());

    rebuild_main(CALC_NEW_BODY);
    assert!(
        classes.join("com/example/Calc$1.class").is_file(),
        "the anonymous class was compiled"
    );
    assert!(
        !compile::is_stale(&test()).unwrap(),
        "a new body is not a new API"
    );

    rebuild_main(&CALC_NEW_BODY.replace("LIMIT = 10", "LIMIT = 11"));
    assert!(
        compile::is_stale(&test()).unwrap(),
        "javac inlined the old constant into the tests"
    );
    compile::compile(&toolchain, &test(), &ui).unwrap();
    assert!(!compile::is_stale(&test()).unwrap());

    rebuild_main(&CALC_V1.replace("LIMIT = 10", "LIMIT = 11").replace(
        "    private Calc() {}\n",
        "    private Calc() {}\n\n    public static int sub(int a, int b) {\n        \
         return a - b;\n    }\n",
    ));
    assert!(
        compile::is_stale(&test()).unwrap(),
        "a new public method is a new API"
    );
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
        sources: project.main_sources().unwrap().files,
        output_dir: project.classes_dir(),
        classpath: Vec::new(),
        release: toolchain.release(manifest.java.source).unwrap(),
        target: manifest.java.target,
        encoding: manifest.java.encoding.clone(),
        extra_args: manifest.java.javac_args.clone(),
        work_dir: project.work_dir(),
        foreign: None,
        main_api: None,
    };
    let error = compile::compile(&toolchain, &unit, &silent_ui()).unwrap_err();
    assert!(error.to_string().contains("compilation failed"), "{error}");
    assert_eq!(error.exit_code(), 1);
    assert!(
        compile::is_stale(&unit).unwrap(),
        "a failed build must not be recorded as up to date"
    );
}

/// A Java unit over every source under `root/src`, into `root/<out>`.
fn java_unit(toolchain: &Toolchain, root: &Path, out: &str) -> CompileUnit {
    CompileUnit {
        label: "main".into(),
        sources: project::find_by_extension(&root.join("src"), "java").unwrap(),
        output_dir: root.join(out).join("classes"),
        classpath: Vec::new(),
        release: toolchain.release(None).unwrap(),
        target: None,
        encoding: "UTF-8".into(),
        extra_args: Vec::new(),
        work_dir: root.join(out).join(".jrs"),
        foreign: None,
        main_api: None,
    }
}

/// Build `root` again and say how many sources that compiled.
fn rebuild(toolchain: &Toolchain, root: &Path) -> Result<usize, jrs::JrsError> {
    match compile::compile(
        toolchain,
        &java_unit(toolchain, root, "target"),
        &silent_ui(),
    )? {
        compile::Outcome::Compiled { sources, .. } => Ok(sources),
        compile::Outcome::UpToDate => Ok(0),
    }
}

/// Every file under `dir`, with its bytes.
fn tree_of(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    project::find_all(dir)
        .unwrap()
        .into_iter()
        .map(|p| {
            (
                p.strip_prefix(dir).unwrap().to_path_buf(),
                std::fs::read(&p).unwrap(),
            )
        })
        .collect()
}

/// The classes compiled file by file are the classes a whole build makes.
fn assert_matches_a_whole_build(toolchain: &Toolchain, root: &Path) {
    let whole = java_unit(toolchain, root, "whole");
    let _ = std::fs::remove_dir_all(root.join("whole"));
    compile::compile(toolchain, &whole, &silent_ui()).unwrap();
    assert_eq!(
        tree_of(&root.join("target/classes")),
        tree_of(&whole.output_dir),
        "an incremental build differs from a whole one"
    );
}

const SHAPE: &str = "package geo;\n\npublic abstract class Shape {\n    \
    public abstract double area();\n\n    public String describe() {\n        \
    return \"area \" + area();\n    }\n}\n";
const SQUARE: &str = "package geo;\n\npublic final class Square extends Shape {\n    \
    private final double side;\n\n    public Square(double side) {\n        \
    this.side = side;\n    }\n\n    public double area() {\n        \
    return side * side;\n    }\n\n    public static final class Builder {\n        \
    public Square build() {\n            return new Square(1);\n        }\n    }\n}\n";
/// Calls `describe`, which `Square` inherits: its class file names `Square`
/// and never `Shape`.
const REPORT: &str = "package app;\n\nimport geo.Square;\n\npublic final class Report {\n    \
    public static String of(Square s) {\n        return s.describe();\n    }\n}\n";
const LIMITS: &str = "package geo;\n\npublic final class Limits {\n    \
    public static final int MAX = 10;\n}\n";
/// Reads `Limits.MAX`, which `javac` copies in: no reference to `Limits`.
const CLAMP: &str = "package app;\n\nfinal class Clamp {\n    \
    static int clamp(int x) {\n        return Math.min(x, geo.Limits.MAX);\n    }\n}\n";
const ALONE: &str = "package app;\n\nfinal class Alone {\n    int one() {\n        \
    return 1;\n    }\n}\n";

fn geometry(scratch: &Scratch) -> PathBuf {
    for (path, text) in [
        ("geo/src/geo/Shape.java", SHAPE),
        ("geo/src/geo/Square.java", SQUARE),
        ("geo/src/geo/Limits.java", LIMITS),
        ("geo/src/app/Report.java", REPORT),
        ("geo/src/app/Clamp.java", CLAMP),
        ("geo/src/app/Alone.java", ALONE),
    ] {
        scratch.write(path, text);
    }
    scratch.join("geo")
}

#[test]
fn a_changed_body_compiles_only_its_own_source() {
    let toolchain = require_jdk!();
    let scratch = Scratch::new("build-file-by-file");
    let root = geometry(&scratch);
    let square = root.join("src/geo/Square.java");
    assert_eq!(rebuild(&toolchain, &root).unwrap(), 6);

    std::fs::write(&square, SQUARE.replace("side * side", "Math.pow(side, 2)")).unwrap();
    assert_eq!(rebuild(&toolchain, &root).unwrap(), 1, "only Square");
    assert_matches_a_whole_build(&toolchain, &root);

    // Touched, same contents: nothing to compile, and not even stale.
    let text = std::fs::read(&square).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::write(&square, text).unwrap();
    assert!(!compile::is_stale(&java_unit(&toolchain, &root, "target")).unwrap());

    // A nested class taken away is an API change: its class goes, and
    // Report, which names Square, compiles again. Clamp and Alone do not.
    let builder = root.join("target/classes/geo/Square$Builder.class");
    assert!(builder.is_file());
    let without = SQUARE.replace(
        "    public static final class Builder {\n        public Square build() {\n            \
         return new Square(1);\n        }\n    }\n",
        "",
    );
    assert_ne!(without, SQUARE);
    std::fs::write(&square, without).unwrap();
    assert_eq!(rebuild(&toolchain, &root).unwrap(), 2, "Square and Report");
    assert!(!builder.exists(), "the removed class was left behind");
    assert_matches_a_whole_build(&toolchain, &root);
}

#[test]
fn an_api_change_reaches_callers_through_a_subclass() {
    let toolchain = require_jdk!();
    let scratch = Scratch::new("build-inherited-api");
    let root = geometry(&scratch);
    let shape = root.join("src/geo/Shape.java");
    rebuild(&toolchain, &root).unwrap();

    // Report calls describe() on a Square. Square's own API is unchanged,
    // but what it inherits is not, so Report has to be compiled — and fail.
    let without = SHAPE.replace(
        "\n    public String describe() {\n        return \"area \" + area();\n    }\n",
        "",
    );
    assert_ne!(without, SHAPE);
    std::fs::write(&shape, without).unwrap();
    let error = rebuild(&toolchain, &root).unwrap_err();
    assert_eq!(error.exit_code(), 1, "{error}");
    assert!(compile::is_stale(&java_unit(&toolchain, &root, "target")).unwrap());

    // Put back, the next build compiles everything the failed one had
    // started on, and ends where a whole build would.
    std::fs::write(&shape, SHAPE).unwrap();
    assert_eq!(
        rebuild(&toolchain, &root).unwrap(),
        3,
        "Shape, Square, Report"
    );
    assert_matches_a_whole_build(&toolchain, &root);
    assert_eq!(rebuild(&toolchain, &root).unwrap(), 0);
}

#[test]
fn a_changed_constant_or_a_new_source_compiles_the_whole_unit() {
    let toolchain = require_jdk!();
    let scratch = Scratch::new("build-whole-unit");
    let root = geometry(&scratch);
    rebuild(&toolchain, &root).unwrap();

    std::fs::write(
        root.join("src/geo/Limits.java"),
        LIMITS.replace("MAX = 10", "MAX = 100"),
    )
    .unwrap();
    assert_eq!(
        rebuild(&toolchain, &root).unwrap(),
        6,
        "Clamp holds the old constant and does not name Limits"
    );
    assert_matches_a_whole_build(&toolchain, &root);

    scratch.write(
        "geo/src/app/Extra.java",
        "package app;\n\nfinal class Extra {}\n",
    );
    assert_eq!(rebuild(&toolchain, &root).unwrap(), 7);
    assert_matches_a_whole_build(&toolchain, &root);
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
            ..JarManifest::default()
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
            ..JarManifest::default()
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

// ---- java agents and the JVMs' environment ---------------------------------
//
// An agent jar with a `Premain-Class`, and a stand-in for JUnit's console
// launcher, are compiled at test time and published into the fixture
// repository, so `jrs run` and `jrs test` load a real `-javaagent` without the
// network. They drive the binary with a cache of their own, as the polyglot
// tests below do. `--debug` is not started here: its JVM would wait for a
// debugger; `cli.rs` and `runner.rs` test how it is built.

const MARKER_AGENT: &str = "package agent;\n\n\
    public final class Marker {\n    private Marker() {}\n\n    \
    public static void premain(String args, java.lang.instrument.Instrumentation inst) {\n        \
    System.setProperty(\"jrs.agent\", \"marker 1.0\");\n    }\n}\n";

/// Stands in for JUnit's console launcher: says what its JVM was given, then
/// prints the summary block jrs takes its counts from.
const FAKE_LAUNCHER: &str = "package org.junit.platform.console;\n\n\
    public final class ConsoleLauncher {\n    private ConsoleLauncher() {}\n\n    \
    public static void main(String[] args) {\n        \
    System.out.println(\"agent=\" + System.getProperty(\"jrs.agent\")\n            \
    + \" greeting=\" + System.getenv(\"GREETING\"));\n        \
    System.out.println(\"[         1 tests found           ]\");\n        \
    System.out.println(\"[         1 tests successful      ]\");\n        \
    System.out.println(\"[         0 tests failed          ]\");\n    }\n}\n";

const AGENT_APP: &str = "package com.example;\n\n\
    public class App {\n    public static void main(String[] args) {\n        \
    System.out.println(System.getProperty(\"jrs.agent\") + \"|\" + System.getenv(\"GREETING\")\n            \
    + \"|\" + java.nio.file.Path.of(\"\").toAbsolutePath().getFileName());\n    }\n}\n";

const AGENT_TABLES: &str = r#"[run]
java-agents = ["org.example.agents:marker-agent"]
env = { GREETING = "hi from {project.name} {project.version}" }
cwd = "work"

[test]
java-agents = ["org.example.agents:marker-agent"]
env = { GREETING = "tests of {project.name}" }

[dependencies]
"org.example.agents:marker-agent" = "1.0"

[dev-dependencies]
"org.junit.platform:junit-platform-console-standalone" = "1.10.2"
"#;

/// Compile the one class `class` from `source` and publish it as `coord`,
/// with `manifest` as its jar's manifest.
fn publish_class(
    fixture: &FixtureRepo,
    scratch: &Scratch,
    toolchain: &Toolchain,
    coord: &Coord,
    (class, source): (&str, &str),
    manifest: &str,
) {
    let work = scratch.join(&format!("{}-build", coord.artifact));
    let file = work
        .join("src")
        .join(format!("{}.java", class.replace('.', "/")));
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, source).unwrap();
    let classes = work.join("classes");
    let output = std::process::Command::new(&toolchain.javac)
        .args(["--release", "17", "-d"])
        .arg(&classes)
        .arg(&file)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let manifest_file = work.join("MANIFEST.MF");
    std::fs::write(&manifest_file, manifest).unwrap();
    let jar = work.join("out.jar");
    let output = std::process::Command::new(&toolchain.jar)
        .arg("--create")
        .arg("--file")
        .arg(&jar)
        .arg("--manifest")
        .arg(&manifest_file)
        .arg("-C")
        .arg(&classes)
        .arg(".")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    fixture.publish_pom(
        coord,
        &format!(
            "<project><groupId>{}</groupId><artifactId>{}</artifactId>\
             <version>{}</version></project>",
            coord.group, coord.artifact, coord.version
        ),
    );
    fixture.publish_jar(coord, &std::fs::read(&jar).unwrap());
}

/// A project that prints what its JVM was given, with `tables` in its
/// manifest, the marker agent and the stand-in launcher in its repository.
fn agent_project(scratch: &Scratch, toolchain: &Toolchain, tables: &str) -> Polyglot {
    let fixture = FixtureRepo::new(scratch);
    publish_class(
        &fixture,
        scratch,
        toolchain,
        &Coord::new("org.example.agents", "marker-agent", "1.0"),
        ("agent.Marker", MARKER_AGENT),
        "Premain-Class: agent.Marker\n",
    );
    publish_class(
        &fixture,
        scratch,
        toolchain,
        &Coord::new(
            "org.junit.platform",
            "junit-platform-console-standalone",
            "1.10.2",
        ),
        ("org.junit.platform.console.ConsoleLauncher", FAKE_LAUNCHER),
        "Created-By: jrs tests\n",
    );
    scratch.write(
        "app/jrs.toml",
        &format!(
            "[project]\nname = \"agents\"\nversion = \"1.0.0\"\n\
             main-class = \"com.example.App\"\n\n{tables}\n{}",
            fixture.manifest_section()
        ),
    );
    scratch.write("app/src/main/java/com/example/App.java", AGENT_APP);
    scratch.write(
        "app/src/test/java/com/example/AppTest.java",
        "package com.example;\n\nclass AppTest {}\n",
    );
    std::fs::create_dir_all(scratch.join("app/work")).unwrap();
    Polyglot {
        root: scratch.join("app"),
        cache: scratch.join("jrs-cache"),
        config: scratch.join("no-config.toml"),
    }
}

#[test]
fn run_and_test_load_their_agents_and_get_their_environment() {
    let toolchain = require_jdk!();
    let scratch = Scratch::new("agents-env");
    let app = agent_project(&scratch, &toolchain, AGENT_TABLES);

    let (code, stdout, stderr) = app.jrs(&["-v", "run"]);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        stdout.trim(),
        "marker 1.0|hi from agents 1.0.0|work",
        "the agent ran, the environment was set, and the program ran in run.cwd"
    );
    let java = stderr
        .lines()
        .find(|l| l.contains("com.example.App") && l.contains(" -cp "))
        .unwrap_or_else(|| panic!("no java command line in:\n{stderr}"));
    let agent = java.find("-javaagent:").unwrap();
    assert!(agent < java.find(" -cp ").unwrap(), "{java}");
    assert!(java[agent..].contains("marker-agent-1.0.jar"), "{java}");

    let (code, stdout, stderr) = app.jrs(&["test"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stdout.contains("agent=marker 1.0 greeting=tests of agents"),
        "{stdout}"
    );
    assert!(stderr.contains("1 tests, 1 passed"), "{stderr}");
}

#[test]
fn an_agent_the_graph_does_not_hold_where_the_jvm_looks_is_a_manifest_error() {
    let toolchain = require_jdk!();
    let scratch = Scratch::new("agents-missing");
    let app = agent_project(
        &scratch,
        &toolchain,
        "[run]\njava-agents = [\"io.opentelemetry.javaagent:opentelemetry-javaagent\"]\n\n\
         [dev-dependencies]\n\"org.example.agents:marker-agent\" = \"1.0\"\n",
    );
    let (code, _, stderr) = app.jrs(&["run"]);
    assert_eq!(code, 2, "{stderr}");
    assert!(
        stderr.contains(
            "`run.java-agents` names `io.opentelemetry.javaagent:opentelemetry-javaagent`, \
             which is not in the resolved dependency graph"
        ),
        "{stderr}"
    );
    assert!(stderr.contains("[dependencies]"), "{stderr}");

    // A dev-dependency is there for the tests, but not for `jrs run`.
    let manifest = app.root.join("jrs.toml");
    let text = std::fs::read_to_string(&manifest).unwrap();
    std::fs::write(
        &manifest,
        text.replace(
            "io.opentelemetry.javaagent:opentelemetry-javaagent",
            "org.example.agents:marker-agent",
        ),
    )
    .unwrap();
    let (code, _, stderr) = app.jrs(&["run"]);
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("only on the test classpath"), "{stderr}");
}

#[cfg(unix)]
#[test]
fn a_jlink_image_launches_with_the_run_agents() {
    let toolchain = require_jdk!();
    let scratch = Scratch::new("agents-jlink");
    let app = agent_project(&scratch, &toolchain, AGENT_TABLES);

    let (code, _, stderr) = app.jrs(&["package", "--jlink"]);
    assert_eq!(code, 0, "{stderr}");
    let launcher = app.root.join("target/image/bin/agents");
    let output = std::process::Command::new(&launcher)
        .env_remove("GREETING")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    // The agent is baked in; `run.env` belongs to `jrs run`, not the image.
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.starts_with("marker 1.0|null|"), "{stdout}");

    // A fat jar has no agent jar left to point at.
    let (code, _, stderr) = app.jrs(&["package", "--fat", "--jlink"]);
    assert_eq!(code, 2, "{stderr}");
    assert!(
        stderr.contains("an image or a distribution of a fat jar"),
        "{stderr}"
    );
}

// ---- runtime-only dependencies and local jars ------------------------------
//
// These resolve, so they drive the jrs binary with a cache and a config of
// their own, as the Kotlin tests below do: nothing they publish may reach the
// user's cache.

/// Run the jrs binary against the project at `root`, isolated from the user.
fn jrs_isolated(scratch: &Scratch, root: &Path, args: &[&str]) -> (i32, String, String) {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_jrs"))
        .arg("--manifest-path")
        .arg(root)
        .args([
            "--progress",
            "never",
            "--color",
            "never",
            "--charset",
            "ascii",
        ])
        .args(args)
        .env("JRS_CACHE_DIR", scratch.join("jrs-cache"))
        .env("JRS_CONFIG", scratch.join("no-config.toml"))
        .output()
        .unwrap();
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// Compile one class, `class` (fully qualified) holding `body`, into a jar at
/// `jar`.
fn class_jar(toolchain: &Toolchain, scratch: &Scratch, class: &str, body: &str, jar: &Path) {
    let (package, name) = class.rsplit_once('.').unwrap();
    let work = scratch.join(&format!("class-jar/{name}"));
    let _ = std::fs::remove_dir_all(&work);
    let source = work.join(format!("src/{}/{name}.java", package.replace('.', "/")));
    std::fs::create_dir_all(source.parent().unwrap()).unwrap();
    std::fs::write(
        &source,
        format!("package {package};\n\npublic final class {name} {{\n{body}\n}}\n"),
    )
    .unwrap();
    let classes = work.join("classes");
    let status = std::process::Command::new(&toolchain.javac)
        .args(["--release", "17", "-d"])
        .arg(&classes)
        .arg(&source)
        .status()
        .unwrap();
    assert!(status.success());
    std::fs::create_dir_all(jar.parent().unwrap()).unwrap();
    package::write_thin_jar(&classes, jar, &JarManifest::default()).unwrap();
}

/// Finds its driver the way JDBC does: by name, at run time.
const REFLECTIVE_APP: &str = "package com.example;\n\n\
    public class App {\n    public static void main(String[] args) throws Exception {\n        \
    Class<?> driver = Class.forName(\"org.example.driver.Driver\");\n        \
    System.out.println(\"loaded \" + driver.getMethod(\"name\").invoke(null));\n    }\n}\n";

#[test]
fn a_runtime_only_dependency_runs_and_ships_but_is_not_compiled_against() {
    let toolchain = require_jdk!();
    let scratch = Scratch::new("build-runtime-only");
    let fixture = FixtureRepo::new(&scratch);
    let driver = scratch.join("driver.jar");
    class_jar(
        &toolchain,
        &scratch,
        "org.example.driver.Driver",
        "    public static String name() { return \"driver 1.0\"; }",
        &driver,
    );
    let coord = jrs::resolve::coord::Coord::new("org.example", "driver", "1.0.0");
    fixture.publish_pom(
        &coord,
        "<project><groupId>org.example</groupId><artifactId>driver</artifactId>\
         <version>1.0.0</version></project>",
    );
    fixture.publish_jar(&coord, &std::fs::read(&driver).unwrap());
    scratch.write(
        "app/jrs.toml",
        &format!(
            "[project]\nname = \"app\"\nversion = \"1.0.0\"\nmain-class = \"com.example.App\"\n\n\
             [dependencies]\n\"org.example:driver\" = {{ version = \"1.0.0\", runtime-only = true }}\n\n{}",
            fixture.manifest_section()
        ),
    );
    scratch.write("app/src/main/java/com/example/App.java", REFLECTIVE_APP);
    let root = scratch.join("app");

    let (code, stdout, stderr) = jrs_isolated(&scratch, &root, &["run"]);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stdout.trim(), "loaded driver 1.0");
    let lock = std::fs::read_to_string(root.join("jrs.lock")).unwrap();
    assert!(lock.contains("classpath = \"runtime\""), "{lock}");

    let classpath = |args: &[&str]| jrs_isolated(&scratch, &root, args).1;
    assert!(!classpath(&["classpath"]).contains("driver-1.0.0.jar"));
    assert!(classpath(&["classpath", "--runtime"]).contains("driver-1.0.0.jar"));
    assert!(classpath(&["classpath", "--test"]).contains("driver-1.0.0.jar"));
    let (_, tree, stderr) = jrs_isolated(&scratch, &root, &["tree"]);
    assert!(
        tree.contains("org.example:driver:1.0.0 (runtime-only)"),
        "{tree}{stderr}"
    );

    // The fat jar carries it, and runs with nothing else...
    let (code, _, stderr) = jrs_isolated(&scratch, &root, &["package", "--fat"]);
    assert_eq!(code, 0, "{stderr}");
    let output = std::process::Command::new(&toolchain.java)
        .arg("-jar")
        .arg(root.join("target/app-1.0.0.jar"))
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "loaded driver 1.0",
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    // ...and so does the portable layout's lib/.
    let (code, _, stderr) = jrs_isolated(&scratch, &root, &["package", "--portable"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(root.join("target/lib/driver-1.0.0.jar").is_file());

    // Code that names the driver's class does not compile: it is not on
    // javac's classpath.
    scratch.write(
        "app/src/main/java/com/example/Direct.java",
        "package com.example;\n\nclass Direct {\n    String name = org.example.driver.Driver.name();\n}\n",
    );
    let (code, stdout, stderr) = jrs_isolated(&scratch, &root, &["build"]);
    assert_eq!(code, 1, "{stdout}{stderr}");
    assert!(
        format!("{stdout}{stderr}").contains("org.example.driver"),
        "{stdout}{stderr}"
    );
}

/// A Java tool as a task would run one from a repository: it writes its first
/// argument's file and says so.
const GREETER: &str = "    public static void main(String[] args) throws Exception {\n        \
    java.nio.file.Files.createDirectories(java.nio.file.Path.of(args[0]).getParent());\n        \
    java.nio.file.Files.writeString(java.nio.file.Path.of(args[0]), \"greeted \" + args[1]);\n        \
    System.out.println(\"greeter ran\");\n    }";

#[test]
fn a_task_runs_a_java_tool_resolved_as_a_graph_of_its_own() {
    let toolchain = require_jdk!();
    let scratch = Scratch::new("build-task-tool");
    let fixture = FixtureRepo::new(&scratch);
    let tool = scratch.join("greeter.jar");
    class_jar(
        &toolchain,
        &scratch,
        "org.example.tool.Greeter",
        GREETER,
        &tool,
    );
    // The tool wants lib 2.0.0 and the project lib 1.0.0: the two graphs never
    // meet, so each gets its own.
    let coord = jrs::resolve::coord::Coord::new("org.example", "greeter", "1.0.0");
    fixture.publish_pom(
        &coord,
        "<project><groupId>org.example</groupId><artifactId>greeter</artifactId>\
         <version>1.0.0</version><dependencies><dependency><groupId>org.example</groupId>\
         <artifactId>lib</artifactId><version>2.0.0</version></dependency></dependencies>\
         </project>",
    );
    fixture.publish_jar(&coord, &std::fs::read(&tool).unwrap());
    scratch.write(
        "app/jrs.toml",
        &format!(
            "[project]\nname = \"app\"\nversion = \"1.0.0\"\n\n\
             [dependencies]\n\"org.example:lib\" = \"1.0.0\"\n\n\
             [tasks.greet]\nmain = \"org.example.tool.Greeter\"\n\
             args = [\"{{target}}/greeting.txt\"]\ninputs = [\"jrs.toml\"]\n\
             outputs = [\"{{target}}/greeting.txt\"]\n\n\
             [tasks.greet.dependencies]\n\"org.example:greeter\" = \"1.0.0\"\n\n{}",
            fixture.manifest_section()
        ),
    );
    scratch.write(
        "app/src/main/java/com/example/App.java",
        "package com.example;\n\npublic class App {}\n",
    );
    let root = scratch.join("app");

    let (code, stdout, stderr) = jrs_isolated(&scratch, &root, &["task", "greet", "--", "jrs"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stderr.contains("and the tools of task greet"), "{stderr}");
    assert!(stdout.contains("greeter ran"), "{stdout}{stderr}");
    assert_eq!(
        std::fs::read_to_string(root.join("target/greeting.txt")).unwrap(),
        "greeted jrs"
    );

    let lock = std::fs::read_to_string(root.join("jrs.lock")).unwrap();
    assert!(lock.contains("version = 2\n"), "{lock}");
    let (project, tool) = lock.split_once("\n[[tool]]\n").unwrap();
    assert!(tool.starts_with("name = \"tasks.greet\"\n"), "{tool}");
    assert!(
        tool.contains("artifact = \"greeter\"") && tool.contains("version = \"2.0.0\""),
        "{tool}"
    );
    assert!(
        tool.contains("checksum = \"sha1:"),
        "a fresh graph is pinned: {tool}"
    );
    assert!(project.contains("version = \"1.0.0\"") && !project.contains("greeter"));
    let (_, classpath, _) = jrs_isolated(&scratch, &root, &["classpath"]);
    assert!(!classpath.contains("greeter"), "{classpath}");

    let (code, tree, stderr) = jrs_isolated(&scratch, &root, &["tree", "--task", "greet"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(tree.starts_with("tasks.greet\n"), "{tree}");
    assert!(tree.contains("org.example:greeter:1.0.0"), "{tree}");
    assert!(tree.contains("org.example:lib:2.0.0"), "{tree}");
    let (code, _, stderr) = jrs_isolated(&scratch, &root, &["tree", "--task", "nope"]);
    assert_eq!(code, 2);
    assert!(
        stderr.contains("the tasks with dependencies are `greet`"),
        "{stderr}"
    );

    // Nothing changed, so the task is fresh and nothing is downloaded.
    let (code, _, stderr) = jrs_isolated(&scratch, &root, &["task", "greet", "--", "jrs"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stderr.contains("Fresh greet (task)"), "{stderr}");

    // From jrs.lock, the tool is downloaded when the task runs, not when the
    // graph is read. (The fixture's own jars are not real zips, so this reads
    // the graph with `classpath` rather than compiling against it.)
    let tool_dir = scratch.join("jrs-cache/org/example/greeter");
    std::fs::remove_dir_all(&tool_dir).unwrap();
    let (code, _, stderr) = jrs_isolated(&scratch, &root, &["classpath"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(!stderr.contains("(task greet)"), "{stderr}");
    let (code, _, stderr) = jrs_isolated(&scratch, &root, &["task", "greet", "--", "again"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stderr.contains("Downloading greeter (task greet)"),
        "{stderr}"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("target/greeting.txt")).unwrap(),
        "greeted again"
    );

    // `jrs fetch` downloads it too, so the task runs under `--offline`.
    std::fs::remove_dir_all(&tool_dir).unwrap();
    let (code, _, stderr) = jrs_isolated(&scratch, &root, &["fetch"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stderr.contains("Downloading greeter (task greet)"),
        "{stderr}"
    );
    let (code, _, stderr) = jrs_isolated(
        &scratch,
        &root,
        &["--offline", "task", "greet", "--", "offline"],
    );
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        std::fs::read_to_string(root.join("target/greeting.txt")).unwrap(),
        "greeted offline"
    );
}

#[test]
fn a_bom_in_managed_versions_what_the_manifest_leaves_out() {
    let scratch = Scratch::new("build-managed-bom");
    let fixture = FixtureRepo::new(&scratch);
    fixture.publish_pom(
        &jrs::resolve::coord::Coord::new("org.example", "platform", "1.0.0"),
        "<project><groupId>org.example</groupId><artifactId>platform</artifactId>\
         <version>1.0.0</version><packaging>pom</packaging><dependencyManagement>\
         <dependencies><dependency><groupId>org.example</groupId><artifactId>lib</artifactId>\
         <version>1.0.0</version></dependency><dependency><groupId>org.example</groupId>\
         <artifactId>core</artifactId><version>1.0.0</version></dependency></dependencies>\
         </dependencyManagement></project>",
    );
    scratch.write(
        "app/jrs.toml",
        &format!(
            "[project]\nname = \"app\"\nversion = \"1.0.0\"\n\n\
             [managed]\n\"org.example:platform\" = {{ version = \"1.0.0\", bom = true }}\n\n\
             [dependencies]\n\"org.example:lib\" = {{}}\n\n{}",
            fixture.manifest_section()
        ),
    );
    let root = scratch.join("app");

    let (code, tree, stderr) = jrs_isolated(&scratch, &root, &["tree"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(tree.contains("org.example:lib:1.0.0 (managed)"), "{tree}");
    assert!(tree.contains("org.example:core:1.0.0 (managed)"), "{tree}");
    let lock = std::fs::read_to_string(root.join("jrs.lock")).unwrap();
    assert_eq!(lock.matches("managed = true").count(), 2, "{lock}");
    assert!(
        !lock.contains("platform"),
        "a BOM is not in the graph: {lock}"
    );

    // What the BOM covers is added without a version.
    let (code, _, stderr) = jrs_isolated(&scratch, &root, &["add", "org.example:core"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stderr.contains("at the version [managed] gives it"),
        "{stderr}"
    );
    let manifest = std::fs::read_to_string(root.join("jrs.toml")).unwrap();
    assert!(
        manifest.contains("\"org.example:core\" = {}\n"),
        "{manifest}"
    );
}

const SHOUTER: &str =
    "    public static String shout(String s) { return s.toUpperCase() + \"!\"; }";

const SHOUTING_APP: &str = "package com.example;\n\nimport org.example.Shouter;\n\n\
    public class App {\n    public static void main(String[] args) {\n        \
    System.out.println(Shouter.shout(\"hi\"));\n    }\n}\n";

#[test]
fn a_local_jar_builds_runs_ships_and_is_pinned_by_its_path() {
    let toolchain = require_jdk!();
    let scratch = Scratch::new("build-local-jar");
    let root = scratch.join("app");
    let jar = root.join("libs/shouter.jar");
    class_jar(&toolchain, &scratch, "org.example.Shouter", SHOUTER, &jar);
    scratch.write(
        "app/jrs.toml",
        "[project]\nname = \"app\"\nversion = \"1.0.0\"\nmain-class = \"com.example.App\"\n\n\
         [dependencies]\nshouter = { path = \"libs/shouter.jar\" }\n",
    );
    scratch.write("app/src/main/java/com/example/App.java", SHOUTING_APP);
    let jrs = |args: &[&str]| jrs_isolated(&scratch, &root, args);

    let (code, stdout, stderr) = jrs(&["run"]);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stdout.trim(), "HI!");

    let lock = std::fs::read_to_string(root.join("jrs.lock")).unwrap();
    assert!(lock.contains("version = 1\n"), "{lock}");
    assert!(
        lock.contains("[[local]]\nname = \"shouter\"\npath = \"libs/shouter.jar\""),
        "{lock}"
    );
    assert!(
        !lock.contains(&scratch.path.display().to_string()),
        "no absolute paths:\n{lock}"
    );
    let (_, tree, _) = jrs(&["tree"]);
    assert!(tree.contains("shouter = libs/shouter.jar"), "{tree}");
    let (code, why, stderr) = jrs(&["tree", "--why", "shouter"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(why.contains("app v1.0.0 [dependencies]"), "{why}");
    let (code, _, stderr) = jrs(&["outdated"]);
    assert_eq!(
        code, 0,
        "a local jar has no newer release to ask for: {stderr}"
    );
    let (code, _, stderr) = jrs(&["verify"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stderr.contains("Verified"), "{stderr}");

    // The portable layout ships it in lib/, under its own file name.
    let (code, _, stderr) = jrs(&["package", "--portable"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(root.join("target/lib/shouter.jar").is_file());
    let output = std::process::Command::new(&toolchain.java)
        .arg("-jar")
        .arg(root.join("target/app-1.0.0.jar"))
        .current_dir(&scratch.path)
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "HI!");

    // New bytes under the same name fail the build until they are pinned.
    class_jar(
        &toolchain,
        &scratch,
        "org.example.Shouter",
        &format!("{SHOUTER}\n    public static String whisper(String s) {{ return s; }}"),
        &jar,
    );
    let (code, _, stderr) = jrs(&["build"]);
    assert_eq!(code, 1, "{stderr}");
    assert!(
        stderr.contains("has changed since jrs.lock pinned it"),
        "{stderr}"
    );
    assert!(stderr.contains("libs/shouter.jar"), "{stderr}");
    assert_eq!(jrs(&["verify"]).0, 1);
    let (code, _, stderr) = jrs(&["update"]);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(jrs(&["run"]).0, 0);

    // A jar that is not there is a manifest error that names it.
    std::fs::remove_file(&jar).unwrap();
    let (code, _, stderr) = jrs(&["build"]);
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("libs/shouter.jar"), "{stderr}");
    assert!(stderr.contains("does not exist"), "{stderr}");
}

// ---- Kotlin, Scala and Groovy (JVM_LANGUAGES.md) ---------------------------
//
// These run against the fake compilers in tests/fixtures/fake-compiler,
// published into the fixture repository as every compiler jrs resolves. That
// drives the whole pipeline the real ones go through — isolated tool graphs,
// `[[tool]]` pins, running on the project's JDK, step order, the implied
// runtime library — on every CI leg, without the network.

/// A project with the fake compilers published beside it, run through the jrs
/// binary with a cache of its own: these tests resolve, and the fake
/// compilers must never land in the user's cache.
struct Polyglot {
    root: PathBuf,
    cache: PathBuf,
    config: PathBuf,
}

impl Polyglot {
    fn new(
        scratch: &Scratch,
        toolchain: &Toolchain,
        manifest: &str,
        files: &[(&str, &str)],
    ) -> Polyglot {
        let fixture = FixtureRepo::new(scratch);
        fixture.publish_fake_compilers(scratch, toolchain);
        scratch.write(
            "app/jrs.toml",
            &format!("{manifest}\n{}", fixture.manifest_section()),
        );
        for (path, contents) in files {
            scratch.write(&format!("app/{path}"), contents);
        }
        Polyglot {
            root: scratch.join("app"),
            cache: scratch.join("jrs-cache"),
            // A user's mirrors or proxy must not reach the fixture.
            config: scratch.join("no-config.toml"),
        }
    }

    fn jrs(&self, args: &[&str]) -> (i32, String, String) {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_jrs"))
            .arg("--manifest-path")
            .arg(&self.root)
            .args([
                "--progress",
                "never",
                "--color",
                "never",
                "--charset",
                "ascii",
            ])
            .args(args)
            .env("JRS_CACHE_DIR", &self.cache)
            .env("JRS_CONFIG", &self.config)
            .output()
            .unwrap();
        (
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    }

    /// What a fake compiler was run with: its name, then one argument a line,
    /// then what it saw of its own JVM.
    fn record(&self, unit: &str, tool: &str) -> Vec<String> {
        let path = self.root.join(format!("target/{unit}.fake-{tool}"));
        std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn modified(&self, relative: &str) -> std::time::SystemTime {
        std::fs::metadata(self.root.join(relative))
            .unwrap()
            .modified()
            .unwrap()
    }
}

/// Whether `flag` is followed by `value` somewhere in `args`.
fn pair(args: &[String], flag: &str, value: &str) -> bool {
    args.windows(2).any(|w| w[0] == flag && w[1] == value)
}

/// "Kotlin" that the fake kotlinc compiles as Java: it uses a Java class and
/// the implied runtime library, and a Java class uses it back.
const GREETER_KT: &str = "package com.example;\n\n\
    public final class Greeter {\n    public static String greet(String name) {\n        \
    return \"Hello, \" + Util.shout(name) + \" from \" + kotlin.FakeStdlib.mark();\n    }\n}\n";

const UTIL_JAVA: &str = "package com.example;\n\n\
    public final class Util {\n    public static String shout(String s) {\n        \
    return s.toUpperCase();\n    }\n}\n";

const APP_JAVA: &str = "package com.example;\n\n\
    public class App {\n    public static void main(String[] args) {\n        \
    System.out.println(Greeter.greet(\"jrs\"));\n    }\n}\n";

const KOTLIN_APP: &str = "[project]\nname = \"mixed\"\nversion = \"1.0.0\"\n\
    main-class = \"com.example.App\"\n\n[kotlin]\nversion = \"2.9.9\"\n";

fn kotlin_app(scratch: &Scratch, toolchain: &Toolchain) -> Polyglot {
    Polyglot::new(
        scratch,
        toolchain,
        KOTLIN_APP,
        &[
            ("src/main/kotlin/com/example/Greeter.kt", GREETER_KT),
            ("src/main/java/com/example/Util.java", UTIL_JAVA),
            ("src/main/java/com/example/App.java", APP_JAVA),
        ],
    )
}

#[test]
fn a_mixed_kotlin_project_compiles_in_two_steps_and_runs() {
    let toolchain = require_jdk!();
    let scratch = Scratch::new("lang-kotlin");
    let p = kotlin_app(&scratch, &toolchain);

    let (code, stdout, stderr) = p.jrs(&["run"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stderr.contains("Resolving 0 declared dependencies and the Kotlin compiler"),
        "{stderr}"
    );
    assert!(
        stderr.contains("Downloading kotlin-compiler-embeddable (Kotlin compiler)"),
        "{stderr}"
    );
    assert!(
        stderr.contains("Compiling mixed v1.0.0 (1 Kotlin + 2 Java source files)"),
        "{stderr}"
    );
    // Java calls "Kotlin", "Kotlin" calls Java, and the implied stdlib is on
    // the runtime classpath.
    assert_eq!(stdout.trim(), "Hello, JRS from kotlin-stdlib 2.9.9");

    let kotlinc = p.record("classes", "kotlinc");
    let release = toolchain.release(None).unwrap().to_string();
    assert!(kotlinc.contains(&"-no-stdlib".to_string()), "{kotlinc:?}");
    assert!(pair(&kotlinc, "-jvm-target", &release), "{kotlinc:?}");
    assert!(pair(&kotlinc, "-module-name", "mixed"), "{kotlinc:?}");
    assert!(
        kotlinc.iter().any(|a| a.ends_with("Util.java")),
        "kotlinc reads the Java sources: {kotlinc:?}"
    );
    assert!(
        kotlinc.contains(&"support=fake-compiler-support".to_string()),
        "the compiler runs with its whole graph: {kotlinc:?}"
    );
    assert!(
        p.root.join("target/.jrs/javac-main.args").is_file(),
        "javac compiled the Java sources after kotlinc"
    );

    let lock = std::fs::read_to_string(p.root.join("jrs.lock")).unwrap();
    assert!(lock.contains("\nversion = 2\n"), "{lock}");
    assert!(
        lock.contains("roots = [\"org.jetbrains.kotlin:kotlin-stdlib\"]"),
        "{lock}"
    );
    assert!(
        lock.contains("[[tool]]\nname = \"kotlin-compiler\"\n"),
        "{lock}"
    );
    assert!(
        lock.contains("artifact = \"fake-compiler-support\""),
        "{lock}"
    );

    let (code, stdout, _) = p.jrs(&["tree"]);
    assert_eq!(code, 0);
    assert!(
        stdout.contains("org.jetbrains.kotlin:kotlin-stdlib:2.9.9 (implied by [kotlin])"),
        "{stdout}"
    );
    let (code, stdout, _) = p.jrs(&["tree", "--tool", "kotlin-compiler"]);
    assert_eq!(code, 0);
    assert!(
        stdout.contains("kotlin-compiler (Kotlin 2.9.9)"),
        "{stdout}"
    );
    assert!(
        stdout.contains("org.example.fake:fake-compiler-support:1.0"),
        "{stdout}"
    );
    let (code, _, stderr) = p.jrs(&["tree", "--tool", "scala-compiler"]);
    assert_eq!(code, 2);
    assert!(stderr.contains("`kotlin-compiler`"), "{stderr}");

    // Nothing changed: the unit is fresh, and neither step runs.
    let kotlinc_at = p.modified("target/classes.fake-kotlinc");
    let javac_at = p.modified("target/.jrs/javac-main.args");
    let (code, _, stderr) = p.jrs(&["build"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stderr.contains("Fresh mixed v1.0.0"), "{stderr}");
    assert!(!stderr.contains("Compiling"), "{stderr}");
    assert!(
        !stderr.contains("Downloading"),
        "the compiler is cached: {stderr}"
    );
    assert_eq!(p.modified("target/classes.fake-kotlinc"), kotlinc_at);

    // A touched .kt reruns both steps.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let greeter = p.root.join("src/main/kotlin/com/example/Greeter.kt");
    std::fs::write(&greeter, format!("{GREETER_KT}// touched\n")).unwrap();
    let (code, _, stderr) = p.jrs(&["build"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stderr.contains("Compiling mixed v1.0.0"), "{stderr}");
    assert_ne!(p.modified("target/classes.fake-kotlinc"), kotlinc_at);
    assert_ne!(p.modified("target/.jrs/javac-main.args"), javac_at);
}

/// What the fake doc tool wrote into `target/doc/index.html`: the tool's
/// name, its arguments one per line, then its classpath and graph.
fn doc_page(p: &Polyglot) -> Vec<String> {
    std::fs::read_to_string(p.root.join("target/doc/index.html"))
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
fn groovydoc_documents_the_groovy_and_java_sources_together() {
    let toolchain = require_jdk!();
    let scratch = Scratch::new("doc-groovy");
    let p = Polyglot::new(
        &scratch,
        &toolchain,
        "[project]\nname = \"groovy-app\"\nversion = \"1.0.0\"\n\n[groovy]\nversion = \"4.9.9\"\n",
        &[
            (
                "src/main/groovy/com/example/Greeter.groovy",
                "package com.example;\n\npublic final class Greeter {}\n",
            ),
            ("src/main/java/com/example/Util.java", UTIL_JAVA),
        ],
    );
    let (code, _, stderr) = p.jrs(&["doc"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stderr.contains("Downloading groovy-groovydoc (Groovy doc tool)"),
        "{stderr}"
    );
    assert!(
        stderr.contains("Documenting groovy-app v1.0.0 (1 Groovy + 1 Java source files)"),
        "{stderr}"
    );
    assert!(
        !stderr.contains("Compiling"),
        "groovydoc reads the sources: {stderr}"
    );

    let page = doc_page(&p);
    assert_eq!(page[0], "groovydoc");
    let release = toolchain.release(None).unwrap();
    assert!(
        page.contains(&format!("-javaVersion=JAVA_{release}")),
        "{page:?}"
    );
    assert!(
        page.contains(&"-windowtitle=groovy-app 1.0.0".to_string()),
        "{page:?}"
    );
    // Relative to their roots, which is how Groovydoc finds their packages.
    let relative = |name: &str| {
        Path::new("com")
            .join("example")
            .join(name)
            .display()
            .to_string()
    };
    assert!(page.contains(&relative("Greeter.groovy")), "{page:?}");
    assert!(page.contains(&relative("Util.java")), "{page:?}");
    assert!(
        page.contains(&"support=fake-compiler-support".to_string()),
        "the tool runs with its whole graph: {page:?}"
    );
    let lock = std::fs::read_to_string(p.root.join("jrs.lock")).unwrap();
    assert!(!lock.contains("groovydoc"), "never pinned: {lock}");

    let (code, _, stderr) = p.jrs(&["doc"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(
        !stderr.contains("doc tool"),
        "resolved and downloaded once: {stderr}"
    );
}

#[test]
fn scaladoc_3_documents_the_compiled_scala_classes() {
    let toolchain = require_jdk!();
    let scratch = Scratch::new("doc-scala");
    let p = Polyglot::new(
        &scratch,
        &toolchain,
        "[project]\nname = \"scala-app\"\nversion = \"1.0.0\"\n\n[scala]\nversion = \"3.9.9\"\n",
        &[
            (
                "src/main/scala/com/example/Greeter.scala",
                "package com.example;\n\npublic final class Greeter {\n    \
                 public static String greet() {\n        return Util.shout(\"hi\");\n    }\n}\n",
            ),
            ("src/main/java/com/example/Util.java", UTIL_JAVA),
        ],
    );
    let (code, _, stderr) = p.jrs(&["doc"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(
        line_of(&stderr, "Compiling scala-app") < line_of(&stderr, "Documenting"),
        "it reads TASTy, so the build runs first: {stderr}"
    );
    assert!(
        stderr.contains("Documenting scala-app v1.0.0 (1 Scala source files)"),
        "{stderr}"
    );
    assert!(
        stderr.contains("1 Java source files were left out"),
        "{stderr}"
    );
    assert!(
        stderr.contains("Downloading scaladoc_3 (Scala doc tool)"),
        "{stderr}"
    );

    let page = doc_page(&p);
    assert_eq!(page[0], "scaladoc");
    assert!(pair(&page, "-project", "scala-app"), "{page:?}");
    assert!(pair(&page, "-project-version", "1.0.0"), "{page:?}");
    assert!(
        page.iter()
            .any(|a| Path::new(a).ends_with(Path::new("target").join("classes"))),
        "the classes are its input: {page:?}"
    );
    let cp = page.iter().position(|a| a == "-classpath").unwrap();
    assert!(page[cp + 1].contains("scala-library-3.9.9.jar"), "{page:?}");
    let tool = page.iter().find(|l| l.starts_with("classpath=")).unwrap();
    assert!(
        tool.contains("jackson-annotations-2.21.jar"),
        "the pin runs beside scaladoc 3.9: {tool}"
    );
}

#[test]
fn kotlin_sources_are_left_out_of_javadoc_with_a_warning() {
    let toolchain = require_jdk!();
    let scratch = Scratch::new("doc-kotlin");
    let p = kotlin_app(&scratch, &toolchain);
    let (code, _, stderr) = p.jrs(&["doc"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stderr
            .contains("jrs has no documentation tool for Kotlin: its 1 source files were left out"),
        "{stderr}"
    );
    assert!(
        stderr.contains("Documenting mixed v1.0.0 (2 source files)"),
        "{stderr}"
    );
    assert!(p.root.join("target/doc/com/example/Util.html").is_file());
}

#[test]
fn a_mixed_project_packages_byte_identical_fat_jars() {
    let toolchain = require_jdk!();
    let scratch = Scratch::new("lang-fat");
    let p = kotlin_app(&scratch, &toolchain);
    let jar = p.root.join("target/mixed-1.0.0.jar");

    let (code, _, stderr) = p.jrs(&["package", "--fat"]);
    assert_eq!(code, 0, "{stderr}");
    let first = std::fs::read(&jar).unwrap();
    let output = std::process::Command::new(&toolchain.java)
        .arg("-jar")
        .arg(&jar)
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "Hello, JRS from kotlin-stdlib 2.9.9",
        "the implied stdlib is in the fat jar"
    );

    assert_eq!(p.jrs(&["clean"]).0, 0);
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let (code, _, stderr) = p.jrs(&["package", "--fat"]);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(first, std::fs::read(&jar).unwrap());
}

#[test]
fn groovy_compiles_its_java_sources_jointly_in_one_step() {
    let toolchain = require_jdk!();
    let scratch = Scratch::new("lang-groovy");
    let p = Polyglot::new(
        &scratch,
        &toolchain,
        "[project]\nname = \"groovy-app\"\nversion = \"1.0.0\"\n\
         main-class = \"com.example.App\"\n\n[java]\njavac-args = [\"-Xlint:all\"]\n\n\
         [groovy]\nversion = \"4.9.9\"\n",
        &[
            (
                "src/main/groovy/com/example/Greeter.groovy",
                "package com.example;\n\npublic final class Greeter {\n    \
                 public static String greet(String name) {\n        \
                 return \"Hello, \" + Util.shout(name) + \" from \" + groovy.lang.FakeGroovy.MARK;\n    \
                 }\n}\n",
            ),
            ("src/main/java/com/example/Util.java", UTIL_JAVA),
            ("src/main/java/com/example/App.java", APP_JAVA),
        ],
    );

    let (code, stdout, stderr) = p.jrs(&["run"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stderr.contains("Compiling groovy-app v1.0.0 (1 Groovy + 2 Java source files)"),
        "{stderr}"
    );
    assert_eq!(stdout.trim(), "Hello, JRS from groovy runtime");

    let groovyc = p.record("classes", "groovyc");
    let release = toolchain.release(None).unwrap();
    assert_eq!(groovyc[1], "-cp", "groovyc wants its classpath first");
    assert!(groovyc.contains(&"-j".to_string()), "{groovyc:?}");
    assert!(
        groovyc.contains(&format!("-J=-release={release}")),
        "{groovyc:?}"
    );
    assert!(groovyc.contains(&"-F=Xlint:all".to_string()), "{groovyc:?}");
    assert!(
        groovyc.contains(&format!("groovy.target.bytecode={release}")),
        "{groovyc:?}"
    );
    assert!(
        !p.root.join("target/.jrs/javac-main.args").exists(),
        "groovyc ran javac itself"
    );
}

#[test]
fn scala_3_gets_its_own_flags_and_both_halves_of_its_library() {
    let toolchain = require_jdk!();
    let scratch = Scratch::new("lang-scala");
    let p = Polyglot::new(
        &scratch,
        &toolchain,
        "[project]\nname = \"scala-app\"\nversion = \"1.0.0\"\n\
         main-class = \"com.example.App\"\n\n[scala]\nversion = \"3.9.9\"\n\
         scalac-args = [\"-deprecation\"]\n",
        &[
            (
                "src/main/scala/com/example/Greeter.scala",
                "package com.example;\n\npublic final class Greeter {\n    \
                 public static String greet(String name) {\n        \
                 return \"Hello, \" + Util.shout(name) + \" from \" + scala.FakeLibrary.mark();\n    \
                 }\n}\n",
            ),
            ("src/main/java/com/example/Util.java", UTIL_JAVA),
            ("src/main/java/com/example/App.java", APP_JAVA),
        ],
    );
    let (code, stdout, stderr) = p.jrs(&["run"]);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stdout.trim(), "Hello, JRS from scala-library 3.9.9");
    let scalac = p.record("classes", "scalac");
    let release = toolchain.release(None).unwrap().to_string();
    assert!(
        pair(&scalac, "-java-output-version", &release),
        "{scalac:?}"
    );
    assert!(scalac.contains(&"-color:never".to_string()), "{scalac:?}");
    let at = |arg: &str| scalac.iter().position(|a| a == arg);
    assert!(
        at("-deprecation") > at("-java-output-version"),
        "scalac-args come after jrs's own flags: {scalac:?}"
    );
    let lock = std::fs::read_to_string(p.root.join("jrs.lock")).unwrap();
    assert!(
        lock.contains(
            "roots = [\"org.scala-lang:scala3-library_3\", \"org.scala-lang:scala-library\"]"
        ),
        "{lock}"
    );
}

#[test]
fn kotlin_tests_see_the_main_modules_internals() {
    let toolchain = require_jdk!();
    let scratch = Scratch::new("lang-kotlin-tests");
    let p = kotlin_app(&scratch, &toolchain);
    scratch.write(
        "app/src/test/kotlin/com/example/GreeterTest.kt",
        "package com.example;\n\nclass GreeterTest {\n    String probe() {\n        \
         return Greeter.greet(\"test\");\n    }\n}\n",
    );
    // No JUnit in the fixture: the tests compile, then the launcher is missing.
    let (code, _, stderr) = p.jrs(&["test"]);
    assert_eq!(code, 1, "{stderr}");
    assert!(
        stderr.contains("Compiling 1 Kotlin test sources"),
        "{stderr}"
    );
    assert!(stderr.contains("needs JUnit"), "{stderr}");
    let kotlinc = p.record("test-classes", "kotlinc");
    assert!(pair(&kotlinc, "-module-name", "mixed_test"), "{kotlinc:?}");
    let classes = p.root.join("target/classes");
    let friends = kotlinc
        .iter()
        .find_map(|a| a.strip_prefix("-Xfriend-paths="))
        .unwrap_or_else(|| panic!("no friend paths: {kotlinc:?}"));
    assert_eq!(
        std::fs::canonicalize(friends).unwrap(),
        std::fs::canonicalize(&classes).unwrap()
    );
}

#[test]
fn kotlin_main_code_takes_groovy_tests() {
    let toolchain = require_jdk!();
    let scratch = Scratch::new("lang-kotlin-groovy");
    let p = Polyglot::new(
        &scratch,
        &toolchain,
        &format!("{KOTLIN_APP}\n[groovy]\nversion = \"4.9.9\"\n"),
        &[
            ("src/main/kotlin/com/example/Greeter.kt", GREETER_KT),
            ("src/main/java/com/example/Util.java", UTIL_JAVA),
            ("src/main/java/com/example/App.java", APP_JAVA),
            (
                "src/test/groovy/com/example/GreeterSpec.groovy",
                "package com.example;\n\nclass GreeterSpec {\n    String probe() {\n        \
                 return Greeter.greet(\"spec\");\n    }\n}\n",
            ),
        ],
    );
    let (code, _, stderr) = p.jrs(&["test"]);
    assert_eq!(code, 1, "no JUnit in the fixture: {stderr}");
    assert!(
        stderr.contains("Compiling 1 Groovy test sources"),
        "{stderr}"
    );
    let groovyc = p.record("test-classes", "groovyc");
    assert!(
        !groovyc.contains(&"-j".to_string()),
        "no Java tests, so no joint compilation: {groovyc:?}"
    );
    let lock = std::fs::read_to_string(p.root.join("jrs.lock")).unwrap();
    assert!(lock.contains("name = \"kotlin-compiler\""), "{lock}");
    assert!(lock.contains("name = \"groovy-compiler\""), "{lock}");
}

#[test]
fn a_file_level_main_gets_its_class_name_suggested() {
    let toolchain = require_jdk!();
    let scratch = Scratch::new("lang-main-kt");
    let p = Polyglot::new(
        &scratch,
        &toolchain,
        KOTLIN_APP,
        &[(
            "src/main/kotlin/com/example/App.kt",
            "package com.example;\n\n// A file-level `main` in App.kt compiles to AppKt.\n\
             final class AppKt {\n    public static void main(String[] args) {}\n}\n",
        )],
    );
    let (code, _, stderr) = p.jrs(&["run"]);
    assert_eq!(code, 2, "{stderr}");
    assert!(
        stderr.contains("there is no class `com.example.App`"),
        "{stderr}"
    );
    assert!(
        stderr.contains("main-class = \"com.example.AppKt\""),
        "{stderr}"
    );
}

#[test]
fn a_compiler_is_not_downloaded_offline() {
    let toolchain = require_jdk!();
    let scratch = Scratch::new("lang-offline");
    let p = kotlin_app(&scratch, &toolchain);
    let (code, _, stderr) = p.jrs(&["--offline", "build"]);
    assert_ne!(code, 0, "{stderr}");
    assert!(stderr.contains("offline"), "{stderr}");
}

#[test]
fn sources_in_a_language_that_is_off_fail_the_build() {
    let _toolchain = require_jdk!();
    let scratch = Scratch::new("lang-off");
    let root = hello_with(&scratch, "", &[]);
    scratch.write("hello/src/main/kotlin/com/example/Stray.kt", "class Stray");
    let (code, _, stderr) = jrs(&root, &["build"]);
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("found 1 .kt files under"), "{stderr}");
    assert!(stderr.contains("has no [kotlin] table"), "{stderr}");
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

// ---- --timings and project.jrs-version --------------------------------------

/// The rows of `target/.jrs/timings.txt`, by phase.
fn timings_file(root: &Path) -> Vec<(String, u128)> {
    let text = std::fs::read_to_string(root.join("target/.jrs/timings.txt")).unwrap();
    let mut lines = text.lines();
    assert!(
        lines.next().unwrap().starts_with("# jrs "),
        "no comment line:\n{text}"
    );
    assert_eq!(lines.next(), Some("phase\tms"), "{text}");
    lines
        .map(|l| {
            let (phase, ms) = l.split_once('\t').unwrap();
            (phase.to_string(), ms.parse().unwrap())
        })
        .collect()
}

#[test]
fn build_timings_follow_the_summary_and_are_written_to_target() {
    let _toolchain = require_jdk!();
    let scratch = Scratch::new("timings-build");
    let root = hello_project(&scratch).root;

    // Without the flag, no table and no file.
    let (code, _, stderr) = jrs(&root, &["build"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(!stderr.contains("  phase "), "{stderr}");
    assert!(!root.join("target/.jrs/timings.txt").exists());

    std::fs::remove_dir_all(root.join("target")).unwrap();
    let (code, stdout, stderr) = jrs(&root, &["build", "--timings"]);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stdout, "", "the report goes to stderr");
    let summary = line_of(&stderr, "        time ");
    let header = line_of(&stderr, "  phase ");
    assert!(summary < header, "the table follows the summary:\n{stderr}");
    assert!(
        header < line_of(&stderr, "  compile main: javac "),
        "{stderr}"
    );
    assert!(
        line_of(&stderr, "  compile main: javac ") < line_of(&stderr, "  resources main "),
        "{stderr}"
    );
    assert!(
        stderr
            .trim_end()
            .lines()
            .last()
            .unwrap()
            .starts_with("  total "),
        "{stderr}"
    );

    let rows = timings_file(&root);
    let phases: Vec<&str> = rows.iter().map(|(p, _)| p.as_str()).collect();
    assert_eq!(phases, ["compile main: javac", "resources main", "total"]);
    let (_, total) = rows.last().unwrap();
    let sum: u128 = rows[..rows.len() - 1].iter().map(|(_, ms)| ms).sum();
    assert!(sum <= *total, "the phases do not overlap: {rows:?}");

    // A second build is fresh, and says so in the report.
    let (code, _, stderr) = jrs(&root, &["build", "--timings"]);
    assert_eq!(code, 0, "{stderr}");
    let phases: Vec<String> = timings_file(&root).into_iter().map(|(p, _)| p).collect();
    assert_eq!(phases, ["compile main (fresh)", "resources main", "total"]);
}

#[test]
fn quiet_timings_print_nothing_but_still_write_the_file() {
    let _toolchain = require_jdk!();
    let scratch = Scratch::new("timings-quiet");
    let root = hello_project(&scratch).root;
    // The mode is the `Ui`'s, decided before dispatch, as `cli::main` does
    // from `-q`.
    let (ui, capture) = Ui::captured(
        UiOptions {
            quiet: true,
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
    let code = cli::run_with(
        [
            "jrs",
            "--manifest-path",
            &root.display().to_string(),
            "-q",
            "build",
            "--timings",
        ],
        &ui,
    );
    assert_eq!(code, 0, "{}", capture.stderr());
    assert_eq!(
        (capture.stdout(), capture.stderr()),
        (String::new(), String::new())
    );
    assert_eq!(timings_file(&root).last().unwrap().0, "total");
}

#[test]
fn run_timings_come_before_the_program_starts() {
    let _toolchain = require_jdk!();
    let scratch = Scratch::new("timings-run");
    let root = hello_project(&scratch).root;
    let (code, _, stderr) = jrs(&root, &["run", "--timings", "--", "world"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(
        line_of(&stderr, "  total ") < line_of(&stderr, "Running com.example.Hello"),
        "the program's run is not a build phase:\n{stderr}"
    );
    let text = std::fs::read_to_string(root.join("target/.jrs/timings.txt")).unwrap();
    assert!(text.starts_with("# jrs run --timings"), "{text}");
}

#[test]
fn package_timings_include_the_task_hooks_and_packaging() {
    let _toolchain = require_jdk!();
    let scratch = Scratch::new("timings-package");
    let root = generating_project(&scratch, "", "");
    let (code, _, stderr) = jrs(&root, &["package", "--timings"]);
    assert_eq!(code, 0, "{stderr}");
    let phases: Vec<String> = timings_file(&root).into_iter().map(|(p, _)| p).collect();
    assert_eq!(
        phases,
        [
            "task build-info (pre-compile)",
            "compile main: javac",
            "resources main",
            "packaging",
            "total"
        ]
    );
    // Run again: the task is fresh, and so is the unit.
    let (code, _, stderr) = jrs(&root, &["package", "--timings"]);
    assert_eq!(code, 0, "{stderr}");
    let phases: Vec<String> = timings_file(&root).into_iter().map(|(p, _)| p).collect();
    assert_eq!(phases[0], "task build-info (pre-compile, fresh)");
    assert_eq!(phases[1], "compile main (fresh)");
}

#[test]
fn a_project_needing_a_newer_jrs_stops_before_any_warning() {
    let scratch = Scratch::new("jrs-version");
    scratch.write(
        "app/jrs.toml",
        "[project]\nname = \"app\"\nversion = \"1.0.0\"\njrs-version = \"999.0\"\n\
         future-key = true\n\n[future-table]\nkey = 1\n",
    );
    let (code, stdout, stderr) = jrs(&scratch.join("app"), &["build"]);
    assert_eq!(code, 2, "{stderr}");
    assert_eq!(stdout, "");
    assert!(
        !stderr.contains("warning"),
        "an older jrs names the version, not the keys it does not know:\n{stderr}"
    );
    assert!(stderr.contains("needs jrs 999.0 or newer"), "{stderr}");
    assert!(
        stderr.contains(&format!("this is jrs {}", env!("CARGO_PKG_VERSION"))),
        "{stderr}"
    );
    assert!(
        stderr.contains("https://github.com/pwittchen/jrs/releases"),
        "{stderr}"
    );

    // A version this jrs satisfies is kept, and shows in the model.
    scratch.write(
        "ok/jrs.toml",
        "[project]\nname = \"ok\"\nversion = \"1.0.0\"\njrs-version = \"0.1\"\n",
    );
    let (code, stdout, stderr) = jrs(&scratch.join("ok"), &["metadata", "--no-deps"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("\"jrs-version\": \"0.1\""), "{stdout}");
}

// ---- packaging: attributes, sources, Javadoc, distributions, native images --

fn zip_names(path: &Path) -> Vec<String> {
    let mut archive = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
    (0..archive.len())
        .map(|i| archive.by_index(i).unwrap().name().to_string())
        .collect()
}

fn zip_text(path: &Path, name: &str) -> String {
    let mut archive = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
    let mut entry = archive
        .by_name(name)
        .unwrap_or_else(|e| panic!("{name} in {}: {e}", path.display()));
    let mut text = String::new();
    std::io::Read::read_to_string(&mut entry, &mut text).unwrap();
    text
}

/// Run a program and return its stdout, failing the test if it fails.
fn run_ok(command: &mut std::process::Command) -> String {
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{command:?} failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n")
}

const VERSION_JAVA: &str = "package com.example;\n\npublic final class Version {\n    \
    public static void main(String[] args) {\n        \
    Package p = Version.class.getPackage();\n        \
    System.out.println(p.getImplementationTitle() + \" \" + p.getImplementationVersion());\n    \
    }\n}\n";

#[test]
fn manifest_attributes_reach_every_kind_of_jar() {
    let toolchain = require_jdk!();
    let scratch = Scratch::new("package-attributes");
    let root = hello_with(
        &scratch,
        "[package.manifest]\nImplementation-Title = \"{project.name}\"\n\
         Implementation-Version = \"{project.version}\"\n\
         Automatic-Module-Name = \"com.example.hello\"\n",
        &[("src/main/java/com/example/Version.java", VERSION_JAVA)],
    );
    let jar = root.join("target/hello-1.0.0.jar");
    for flag in [None, Some("--portable"), Some("--fat")] {
        let mut args = vec!["package"];
        args.extend(flag);
        let (code, _, stderr) = jrs(&root, &args);
        assert_eq!(code, 0, "{flag:?}: {stderr}");

        let manifest = zip_text(&jar, "META-INF/MANIFEST.MF");
        assert!(
            manifest.contains("Main-Class: com.example.Hello\n"),
            "{manifest}"
        );
        let ours: Vec<&str> = manifest
            .lines()
            .skip_while(|l| !l.starts_with("Implementation-Title"))
            .take(3)
            .collect();
        assert_eq!(
            ours,
            [
                "Implementation-Title: hello",
                "Implementation-Version: 1.0.0",
                "Automatic-Module-Name: com.example.hello"
            ],
            "{flag:?}: {manifest}"
        );
        // What `Package` reports at run time is what the manifest says.
        let out = run_ok(
            std::process::Command::new(&toolchain.java)
                .arg("-cp")
                .arg(&jar)
                .arg("com.example.Version"),
        );
        assert_eq!(out.trim(), "hello 1.0.0", "{flag:?}");
    }
}

#[test]
fn an_attribute_jrs_writes_itself_is_a_manifest_error() {
    let scratch = Scratch::new("package-owned-attribute");
    let root = hello_with(
        &scratch,
        "[package.manifest]\nMain-Class = \"com.example.Other\"\n",
        &[],
    );
    let (code, _, stderr) = jrs(&root, &["package"]);
    assert_eq!(code, 2, "{stderr}");
    assert!(
        stderr.contains("`package.manifest.Main-Class`: `Main-Class` belongs to jrs"),
        "{stderr}"
    );
}

#[test]
fn sources_and_javadoc_jars_hold_what_the_build_compiled() {
    let _toolchain = require_jdk!();
    let scratch = Scratch::new("package-sources-javadoc");
    let root = generating_project(&scratch, "", "");
    let (code, _, stderr) = jrs(&root, &["package", "--sources", "--javadoc"]);
    assert_eq!(code, 0, "{stderr}");

    let sources = root.join("target/app-1.2.3-sources.jar");
    let javadoc = root.join("target/app-1.2.3-javadoc.jar");
    // The generated source is there as well as the written one.
    assert_eq!(
        zip_names(&sources),
        [
            "META-INF/MANIFEST.MF",
            "com/example/App.java",
            "com/example/BuildInfo.java"
        ]
    );
    assert!(zip_text(&sources, "com/example/BuildInfo.java").contains("\"1.2.3\""));
    let pages = zip_names(&javadoc);
    assert!(pages.iter().any(|p| p == "index.html"), "{pages:?}");
    assert!(
        pages.iter().any(|p| p == "com/example/BuildInfo.html"),
        "{pages:?}"
    );
    assert!(
        line_of(&stderr, "Documenting") < line_of(&stderr, "javadoc.jar"),
        "{stderr}"
    );

    // Both are byte-identical from one build to the next.
    let before = (
        std::fs::read(&sources).unwrap(),
        std::fs::read(&javadoc).unwrap(),
    );
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let (code, _, stderr) = jrs(&root, &["package", "--sources", "--javadoc"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(
        std::fs::read(&sources).unwrap() == before.0,
        "the sources jar changed"
    );
    assert!(
        std::fs::read(&javadoc).unwrap() == before.1,
        "the Javadoc jar changed"
    );
}

const SHOUTER_JAVA: &str = "package org.example;\npublic final class Shouter {\n\
    public static String shout(String s) { return s.toUpperCase() + \"!\"; }\n}\n";

const LIB_TASK_JAVA: &str = "package org.example;\npublic final class LibTask implements Runnable {\n\
     public void run() {}\n}\n";

const SHIPPING_APP_JAVA: &str = "package com.example;\n\n\
    import java.util.ServiceLoader;\nimport org.example.Shouter;\n\n\
    public class App {\n    public static void main(String[] args) {\n        \
    long providers = ServiceLoader.load(Runnable.class).stream().count();\n        \
    System.out.println(Shouter.shout(\"hi\") + \" mode=\" + System.getProperty(\"mode\")\n            \
    + \" providers=\" + providers + \" args=\" + String.join(\"|\", args));\n    }\n}\n";

/// An application with a dependency from the fixture repository, run through
/// the jrs binary with a cache of its own, as [`Polyglot`] is. The dependency,
/// `org.example:shouter`, registers a `Runnable` service, and so does the
/// application: a fat jar has to keep both.
fn shipping_app(scratch: &Scratch, toolchain: &Toolchain) -> Polyglot {
    let fixture = FixtureRepo::new(scratch);
    let shouter = scratch.write("library/src/org/example/Shouter.java", SHOUTER_JAVA);
    let task = scratch.write("library/src/org/example/LibTask.java", LIB_TASK_JAVA);
    let classes = scratch.join("library/classes");
    let status = std::process::Command::new(&toolchain.javac)
        .arg("-d")
        .arg(&classes)
        .args([&shouter, &task])
        .status()
        .unwrap();
    assert!(status.success());
    scratch.write(
        "library/classes/META-INF/services/java.lang.Runnable",
        "org.example.LibTask\n",
    );
    let jar = scratch.join("shouter.jar");
    package::write_thin_jar(&classes, &jar, &JarManifest::default()).unwrap();
    let coord = jrs::resolve::coord::Coord::new("org.example", "shouter", "1.0.0");
    fixture.publish_pom(
        &coord,
        "<project><groupId>org.example</groupId><artifactId>shouter</artifactId>\
         <version>1.0.0</version></project>",
    );
    fixture.publish_jar(&coord, &std::fs::read(&jar).unwrap());

    scratch.write(
        "app/jrs.toml",
        &format!(
            "[project]\nname = \"app\"\nversion = \"1.0.0\"\nmain-class = \"com.example.App\"\n\n\
             [run]\njvm-args = [\"-Dmode=dist\"]\n\n\
             [dependencies]\n\"org.example:shouter\" = \"1.0.0\"\n\n{}",
            fixture.manifest_section()
        ),
    );
    scratch.write("app/src/main/java/com/example/App.java", SHIPPING_APP_JAVA);
    scratch.write(
        "app/src/main/java/com/example/AppTask.java",
        "package com.example;\npublic final class AppTask implements Runnable {\n\
         public void run() {}\n}\n",
    );
    scratch.write(
        "app/src/main/resources/META-INF/services/java.lang.Runnable",
        "com.example.AppTask\n",
    );
    Polyglot {
        root: scratch.join("app"),
        cache: scratch.join("jrs-cache"),
        config: scratch.join("no-config.toml"),
    }
}

#[test]
fn a_distribution_zip_unpacks_into_a_launcher_that_runs() {
    let toolchain = require_jdk!();
    let scratch = Scratch::new("package-dist");
    let app = shipping_app(&scratch, &toolchain);
    let (code, _, stderr) = app.jrs(&["package", "--dist"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stderr.contains("Archiving"), "{stderr}");

    let zip = app.root.join("target/app-1.0.0.zip");
    assert_eq!(
        zip_names(&zip),
        [
            "app-1.0.0/app-1.0.0.jar",
            "app-1.0.0/bin/app",
            "app-1.0.0/bin/app.bat",
            "app-1.0.0/lib/shouter-1.0.0.jar"
        ]
    );
    let bat = zip_text(&zip, "app-1.0.0/bin/app.bat");
    assert!(
        bat.contains("\"%JAVACMD%\" %JAVA_OPTS% -Dmode=dist -jar \"%DIR%\\app-1.0.0.jar\" %*"),
        "{bat}"
    );

    // A second build zips the same bytes.
    let first = std::fs::read(&zip).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let (code, _, stderr) = app.jrs(&["package", "--dist"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(std::fs::read(&zip).unwrap() == first, "the zip changed");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let unpacked = scratch.join("unpacked");
        zip::ZipArchive::new(std::fs::File::open(&zip).unwrap())
            .unwrap()
            .extract(&unpacked)
            .unwrap();
        // The distribution needs nothing the build left behind.
        std::fs::remove_dir_all(&app.cache).unwrap();
        let launcher = unpacked.join("app-1.0.0/bin/app");
        let mode = std::fs::metadata(&launcher).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755, "the launcher lost its executable bit");

        let bin = toolchain.java.parent().unwrap();
        let out = run_ok(
            std::process::Command::new(&launcher)
                .args(["one", "two words"])
                .env("JAVA_HOME", bin.parent().unwrap())
                .env_remove("JAVA_OPTS"),
        );
        assert_eq!(out, "HI! mode=dist providers=2 args=one|two words\n");

        // Without JAVA_HOME, the java on PATH.
        let out = run_ok(
            std::process::Command::new(&launcher)
                .arg("x")
                .env_remove("JAVA_HOME")
                .env_remove("JAVA_OPTS")
                .env("PATH", format!("{}:/usr/bin:/bin", bin.display())),
        );
        assert_eq!(out, "HI! mode=dist providers=2 args=x\n");
    }
}

#[test]
fn a_fat_jar_keeps_the_projects_services_and_its_dependencys() {
    let toolchain = require_jdk!();
    let scratch = Scratch::new("package-fat-services");
    let app = shipping_app(&scratch, &toolchain);
    let (code, _, stderr) = app.jrs(&["package", "--fat", "--dist"]);
    assert_eq!(code, 0, "{stderr}");

    let jar = app.root.join("target/app-1.0.0.jar");
    assert_eq!(
        zip_text(&jar, "META-INF/services/java.lang.Runnable"),
        "com.example.AppTask\norg.example.LibTask\n"
    );
    let out = run_ok(
        std::process::Command::new(&toolchain.java)
            .arg("-jar")
            .arg(&jar),
    );
    assert_eq!(out, "HI! mode=null providers=2 args=\n");
    // A fat distribution is the jar alone, with no lib/.
    assert_eq!(
        zip_names(&app.root.join("target/app-1.0.0.zip")),
        [
            "app-1.0.0/app-1.0.0.jar",
            "app-1.0.0/bin/app",
            "app-1.0.0/bin/app.bat"
        ]
    );
}

#[test]
fn a_distribution_needs_a_main_class() {
    let scratch = Scratch::new("package-dist-no-main");
    scratch.write(
        "lib/jrs.toml",
        "[project]\nname = \"lib\"\nversion = \"1.0.0\"\n",
    );
    let (code, _, stderr) = jrs(&scratch.join("lib"), &["package", "--dist"]);
    assert_eq!(code, 2, "{stderr}");
    assert!(
        stderr.contains("`jrs package --dist` needs a main class"),
        "{stderr}"
    );
}

#[test]
fn a_native_image_needs_a_graalvm_jdk() {
    let toolchain = require_jdk!();
    let scratch = Scratch::new("package-native");
    scratch.write(
        "plain/src/main/java/com/example/App.java",
        "package com.example;\n\npublic class App {\n    public static void main(String[] args) {\n        \
         System.out.println(\"native args=\" + String.join(\"|\", args));\n    }\n}\n",
    );
    scratch.write(
        "plain/jrs.toml",
        "[project]\nname = \"plain\"\nversion = \"1.0.0\"\nmain-class = \"com.example.App\"\n\n\
         [package]\nnative-image-args = [\"--no-fallback\"]\n",
    );
    let root = scratch.join("plain");

    if let Err(e) = jrs::native_image::find(&toolchain) {
        eprintln!(
            "SKIPPED {}: building a native image needs GraalVM; checked the refusal only ({})",
            concat!(module_path!(), "::", line!()),
            e.to_string().lines().next().unwrap_or_default()
        );
        let (code, _, stderr) = jrs(&root, &["package", "--native-image"]);
        assert_eq!(code, 1, "{stderr}");
        assert!(stderr.contains("is not GraalVM"), "{stderr}");
        assert!(
            !stderr.contains("Compiling"),
            "refused before anything is built: {stderr}"
        );
        return;
    }

    let (code, _, stderr) = jrs(&root, &["package", "--native-image"]);
    assert_eq!(code, 0, "{stderr}");
    let argfile = std::fs::read_to_string(root.join("target/.jrs/native-image.args")).unwrap();
    assert!(
        argfile.contains("--no-fallback\ncom.example.App\n"),
        "{argfile}"
    );
    let executable = jrs::native_image::executable(&root.join("target/native"), "plain");
    let out = run_ok(std::process::Command::new(&executable).args(["a", "b c"]));
    assert_eq!(out, "native args=a|b c\n");
}

// ---- test reports, reruns, retries and --fail-fast -------------------------
//
// These run against the fake console launcher in tests/fixtures/fake-launcher,
// published into the fixture repository at a 1.x and a 6.x version, with a
// `@Test` of its own. It prints the tree and the summary block the real one
// prints and writes its XML report, so the HTML page, `--rerun-failed`,
// `test.retries` and both kinds of `--fail-fast` run end to end on every CI
// leg without the network. `tests/network.rs` repeats them against the real
// launcher, whose XML is what they are built from.

/// A project tested with the fake launcher at `launcher`. `test_table` goes
/// into `[test]`, after a `marker.dir` system property the tests keep state in
/// between runs.
fn junit_project(
    scratch: &Scratch,
    toolchain: &Toolchain,
    launcher: &str,
    test_table: &str,
    files: &[(&str, &str)],
) -> Polyglot {
    let fixture = FixtureRepo::new(scratch);
    fixture.publish_fake_launcher(scratch, toolchain);
    let markers = scratch.join("markers");
    std::fs::create_dir_all(&markers).unwrap();
    scratch.write(
        "app/jrs.toml",
        &format!(
            "[project]\nname = \"tested\"\nversion = \"1.0.0\"\n\n\
             [test]\njvm-args = ['-Dmarker.dir={}']\n{test_table}\n\n\
             [dev-dependencies]\n\
             \"org.junit.platform:junit-platform-console-standalone\" = \"{launcher}\"\n\n{}",
            markers.display(),
            fixture.manifest_section()
        ),
    );
    scratch.write(
        "app/src/main/java/com/example/Calc.java",
        "package com.example;\n\npublic final class Calc {\n    \
         public static int add(int a, int b) {\n        return a + b;\n    }\n}\n",
    );
    for (path, contents) in files {
        scratch.write(&format!("app/src/test/java/com/example/{path}"), contents);
    }
    Polyglot {
        root: scratch.join("app"),
        cache: scratch.join("jrs-cache"),
        config: scratch.join("no-config.toml"),
    }
}

const ADD_TEST: &str = "package com.example;\n\nimport org.junit.jupiter.api.Test;\n\n\
    class AddTest {\n    @Test\n    void adds() {\n        \
    if (Calc.add(2, 2) != 4) throw new AssertionError(\"2 + 2\");\n    }\n}\n";

#[test]
fn jrs_test_recompiles_the_tests_only_for_a_new_main_api() {
    let toolchain = require_jdk!();
    let scratch = Scratch::new("test-compile-avoidance");
    let p = junit_project(
        &scratch,
        &toolchain,
        FAKE_LAUNCHER_6,
        "",
        &[("AddTest.java", ADD_TEST)],
    );
    let (code, _, stderr) = p.jrs(&["test"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stderr.contains("Compiling 1 test sources"), "{stderr}");

    let calc = p.root.join("src/main/java/com/example/Calc.java");
    let text = std::fs::read_to_string(&calc).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1100));
    std::fs::write(&calc, text.replace("return a + b;", "return b + a;")).unwrap();
    let (code, _, stderr) = p.jrs(&["test"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stderr.contains("Compiling tested v1.0.0"), "{stderr}");
    assert!(
        !stderr.contains("Compiling 1 test sources"),
        "the main classes' API did not change: {stderr}"
    );

    std::thread::sleep(std::time::Duration::from_millis(1100));
    std::fs::write(
        &calc,
        text.replace(
            "    public static int add",
            "    public static int twice(int a) {\n        return a + a;\n    }\n\n    \
             public static int add",
        ),
    )
    .unwrap();
    let (code, _, stderr) = p.jrs(&["test"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stderr.contains("Compiling 1 test sources"), "{stderr}");
}

/// Every launch of the fake launcher, as the argument line it starts with.
fn launches(stdout: &str) -> Vec<&str> {
    stdout
        .lines()
        .filter(|l| l.starts_with("fake-launcher "))
        .collect()
}

/// The line of `text` that contains `needle`.
fn line_with<'a>(text: &'a str, needle: &str) -> &'a str {
    text.lines()
        .find(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("no line with {needle:?} in:\n{text}"))
}

const CALC_TEST: &str = r#"package com.example;

import java.nio.file.Files;
import java.nio.file.Path;
import org.junit.jupiter.api.Test;

class CalcTest {
    @Test
    void adds() {
        if (Calc.add(1, 1) != 2) throw new AssertionError("1 + 1");
    }

    @Test
    void fixedLater() {
        if (!Files.exists(Path.of(System.getProperty("marker.dir"), "fixed"))) {
            throw new AssertionError("1 + 1 <is> 3 & more");
        }
    }
}
"#;

const OTHER_TEST: &str = "package com.example;\n\nimport org.junit.jupiter.api.Test;\n\n\
    class OtherTest {\n    @Test\n    void passes() {}\n}\n";

/// Fails the first time it runs, passes after that.
const FLAKY_TEST: &str = r#"package com.example;

import java.nio.file.Files;
import java.nio.file.Path;
import org.junit.jupiter.api.Test;

class FlakyTest {
    @Test
    void sometimes() throws Exception {
        Path seen = Path.of(System.getProperty("marker.dir"), "flaky-seen");
        if (!Files.exists(seen)) {
            Files.createFile(seen);
            throw new AssertionError("only the first time");
        }
    }
}
"#;

const BROKEN_TEST: &str = "package com.example;\n\nimport org.junit.jupiter.api.Test;\n\n\
    class BrokenTest {\n    @Test\n    void always() {\n        \
    throw new IllegalStateException(\"never works\");\n    }\n}\n";

/// A failure, then a test slow enough that jrs is long gone if it stops the
/// launcher at the failure, and that leaves a mark if it runs.
const STOP_TEST: &str = r#"package com.example;

import java.nio.file.Files;
import java.nio.file.Path;
import org.junit.jupiter.api.Test;

class StopTest {
    @Test
    void aFails() {
        throw new AssertionError("the first failure");
    }

    @Test
    void bSlow() throws Exception {
        Thread.sleep(5000);
        Files.createFile(Path.of(System.getProperty("marker.dir"), "slow-ran"));
    }
}
"#;

/// `test.forks`: the classes dealt out to launchers run at once, each
/// scanning with a pattern of its own; their XML side by side where CI looks
/// for it, their counts added up, and what failed retried in a launcher of
/// its own. `--forks 1`, `--fail-fast` and `--method` run in one JVM.
#[test]
fn forks_split_the_classes_among_launchers_and_add_them_up() {
    let toolchain = require_jdk!();
    let scratch = Scratch::new("tests-forks");
    let p = junit_project(
        &scratch,
        &toolchain,
        FAKE_LAUNCHER_1,
        "forks = 2\nretries = 1",
        &[
            ("AddTest.java", ADD_TEST),
            ("FlakyTest.java", FLAKY_TEST),
            ("OtherTest.java", OTHER_TEST),
        ],
    );

    let (code, stdout, stderr) = p.jrs(&["test"]);
    assert_eq!(code, 0, "the flaky test passes on its retry: {stderr}");
    assert!(
        stderr.contains("Testing 3 test sources in 2 JVMs"),
        "{stderr}"
    );
    assert!(stderr.contains("Fork 1 of 2: 2 test classes"), "{stderr}");
    assert!(stderr.contains("Fork 2 of 2: 1 test class"), "{stderr}");
    assert!(
        stderr.contains("Finished 3 tests, 2 passed, 1 flaky in"),
        "{stderr}"
    );

    // In name order, the first fork has AddTest and OtherTest, the second
    // FlakyTest; each fork's output is whole, the first fork's first.
    let runs = launches(&stdout);
    assert_eq!(runs.len(), 3, "two forks, then a retry: {stdout}");
    for (run, mine, theirs) in [
        (runs[0], "AddTest", "FlakyTest"),
        (runs[1], "FlakyTest", "OtherTest"),
    ] {
        assert!(run.contains("--scan-class-path"), "{run}");
        assert!(run.contains(&format!(r"\Qcom.example.{mine}\E")), "{run}");
        assert!(
            !run.contains(&format!(r"\Qcom.example.{theirs}\E")),
            "{run}"
        );
    }
    assert!(
        runs[2].contains("--select-method com.example.FlakyTest#sometimes()"),
        "{}",
        runs[2]
    );
    let first_fork = stdout.find(runs[0]).unwrap();
    let second_fork = stdout.find(runs[1]).unwrap();
    let adds = stdout.find("adds()").unwrap();
    assert!(
        first_fork < adds && adds < second_fork,
        "not interleaved: {stdout}"
    );

    let reports = p.root.join("target/test-reports");
    for fork in 1..=2 {
        assert!(
            reports
                .join(format!("TEST-junit-jupiter-fork-{fork}.xml"))
                .is_file()
        );
        assert!(!reports.join(format!("fork-{fork}")).exists());
    }
    assert!(reports.join("retry-1/TEST-junit-jupiter.xml").is_file());
    let html = std::fs::read_to_string(reports.join("index.html")).unwrap();
    for class in ["com.example.AddTest", "com.example.OtherTest"] {
        assert!(html.contains(class), "{html}");
    }
    assert!(
        html.contains("<span class=\"badge flaky\">FLAKY</span>"),
        "{html}"
    );

    // One JVM when asked for, and for runs that cannot be split.
    for args in [
        &["test", "--forks", "1"][..],
        &["test", "--fail-fast"][..],
        &["test", "--method", "com.example.AddTest#adds"][..],
    ] {
        let (code, stdout, stderr) = p.jrs(args);
        assert_eq!(code, 0, "{args:?}: {stderr}");
        assert_eq!(launches(&stdout).len(), 1, "{args:?}: {stdout}");
        assert!(!stderr.contains("JVMs"), "{args:?}: {stderr}");
        assert!(reports.join("TEST-junit-jupiter.xml").is_file(), "{args:?}");
        assert!(
            !reports.join("TEST-junit-jupiter-fork-1.xml").exists(),
            "{args:?}: the forked run's reports are cleared"
        );
    }
}

#[test]
fn a_failing_run_leaves_a_page_and_reruns_only_what_failed() {
    let toolchain = require_jdk!();
    let scratch = Scratch::new("tests-rerun");
    let p = junit_project(
        &scratch,
        &toolchain,
        FAKE_LAUNCHER_1,
        "",
        &[("CalcTest.java", CALC_TEST), ("OtherTest.java", OTHER_TEST)],
    );

    // A rerun with no run before it is an error, not a success: the run
    // that should have been there did not pass.
    let (code, _, stderr) = p.jrs(&["test", "--rerun-failed"]);
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("there is no test run to rerun"), "{stderr}");
    assert!(stderr.contains("run `jrs test` first"), "{stderr}");

    let (code, stdout, stderr) = p.jrs(&["test"]);
    assert_eq!(code, 1, "{stderr}");
    assert!(
        launches(&stdout)[0].contains("--scan-class-path"),
        "{stdout}"
    );
    assert!(
        stderr.contains("Finished 3 tests, 2 passed, 1 failed"),
        "{stderr}"
    );
    // The page is written for a failing run, and the output says where.
    let reporting = line_with(&stderr, "Reporting test results into");
    assert!(
        reporting.ends_with(&format!(
            "target{}test-reports{}index.html",
            std::path::MAIN_SEPARATOR,
            std::path::MAIN_SEPARATOR
        )),
        "{reporting}"
    );
    assert!(
        line_of(&stderr, "Reporting") < line_of(&stderr, "Finished"),
        "{stderr}"
    );
    let html = std::fs::read_to_string(p.root.join("target/test-reports/index.html")).unwrap();
    assert!(html.contains("1 + 1 &lt;is&gt; 3 &amp; more"), "{html}");
    assert!(
        html.contains("<details class=\"class failed\" open>"),
        "{html}"
    );
    assert!(
        html.find("com.example.CalcTest").unwrap() < html.find("com.example.OtherTest").unwrap(),
        "the failing class comes first"
    );

    // The rerun selects the one test that failed, by method, instead of
    // scanning.
    let (code, stdout, stderr) = p.jrs(&["test", "--rerun-failed"]);
    assert_eq!(code, 1, "{stderr}");
    assert!(
        stderr.contains("Testing 1 test that failed in the last run"),
        "{stderr}"
    );
    let args = launches(&stdout)[0];
    assert!(
        args.contains("--select-method com.example.CalcTest#fixedLater()"),
        "{args}"
    );
    assert!(!args.contains("--scan-class-path"), "{args}");
    assert!(
        stderr.contains("Finished 1 tests, 0 passed, 1 failed"),
        "{stderr}"
    );

    // Fixed, it passes; after that there is nothing left to rerun, which is
    // a success.
    std::fs::write(scratch.join("markers/fixed"), "").unwrap();
    let (code, _, stderr) = p.jrs(&["test", "--rerun-failed"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stderr.contains("Finished 1 tests, 1 passed"), "{stderr}");
    let (code, stdout, stderr) = p.jrs(&["test", "--rerun-failed"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stderr.contains("no failed tests to rerun"), "{stderr}");
    assert!(launches(&stdout).is_empty(), "nothing ran: {stdout}");

    let (code, _, stderr) = p.jrs(&["test", "--rerun-failed", "--method", "a.BTest#c"]);
    assert_eq!(code, 2, "{stderr}");
}

#[test]
fn a_test_that_passes_on_a_retry_is_flaky_not_passed() {
    let toolchain = require_jdk!();
    let scratch = Scratch::new("tests-retries");
    let p = junit_project(
        &scratch,
        &toolchain,
        FAKE_LAUNCHER_1,
        "retries = 2",
        &[
            ("BrokenTest.java", BROKEN_TEST),
            ("FlakyTest.java", FLAKY_TEST),
            ("OtherTest.java", OTHER_TEST),
        ],
    );

    let (code, stdout, stderr) = p.jrs(&["test"]);
    assert_eq!(code, 1, "BrokenTest never passes: {stderr}");
    assert!(
        stderr.contains("Retrying 2 failed tests (attempt 2 of 3)"),
        "{stderr}"
    );
    assert!(
        stderr.contains("Retrying 1 failed test (attempt 3 of 3)"),
        "{stderr}"
    );
    assert!(
        stderr.contains("Flaky com.example.FlakyTest#sometimes() (passed on attempt 2)"),
        "{stderr}"
    );
    assert!(
        stderr.contains("Finished 3 tests, 1 passed, 1 flaky, 1 failed"),
        "{stderr}"
    );

    // Each retry is a launcher of its own, selecting only what still fails
    // and writing its XML beside the first run's.
    let runs = launches(&stdout);
    assert_eq!(runs.len(), 3, "{stdout}");
    for (retry, methods) in [
        (
            1,
            &[
                "com.example.BrokenTest#always()",
                "com.example.FlakyTest#sometimes()",
            ][..],
        ),
        (2, &["com.example.BrokenTest#always()"][..]),
    ] {
        let args = runs[retry];
        assert!(!args.contains("--scan-class-path"), "{args}");
        assert_eq!(
            args.matches("--select-method").count(),
            methods.len(),
            "{args}"
        );
        for method in methods {
            assert!(
                args.contains(&format!("--select-method {method}")),
                "{args}"
            );
        }
        let dir = format!("retry-{retry}");
        assert!(args.contains(&dir), "{args}");
    }
    let reports = p.root.join("target/test-reports");
    let first = std::fs::read_to_string(reports.join("TEST-junit-jupiter.xml")).unwrap();
    assert!(
        first.contains("sometimes()"),
        "the first run's XML is left as it was"
    );
    let second = std::fs::read_to_string(reports.join("retry-2/TEST-junit-jupiter.xml")).unwrap();
    assert!(second.contains("always()") && !second.contains("sometimes()"));
    let html = std::fs::read_to_string(reports.join("index.html")).unwrap();
    assert!(
        html.contains("<span class=\"badge flaky\">FLAKY</span>"),
        "{html}"
    );
    assert!(html.contains("failed all 3 attempts"), "{html}");

    // `--rerun-failed` reruns what is still failing, not what was flaky.
    let (_, stdout, stderr) = p.jrs(&["test", "--rerun-failed", "--retries", "0"]);
    let args = launches(&stdout)[0];
    assert!(args.contains("BrokenTest#always()"), "{args}\n{stderr}");
    assert!(!args.contains("FlakyTest"), "{args}");

    // Without the broken test, the flaky one does not fail the run — but it
    // is still not counted as passed.
    std::fs::remove_file(p.root.join("src/test/java/com/example/BrokenTest.java")).unwrap();
    std::fs::remove_file(scratch.join("markers/flaky-seen")).unwrap();
    let (code, _, stderr) = p.jrs(&["test"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stderr.contains("Finished 2 tests, 1 passed, 1 flaky in"),
        "{stderr}"
    );

    // `--retries 0` overrides the manifest.
    std::fs::remove_file(scratch.join("markers/flaky-seen")).unwrap();
    let (code, _, stderr) = p.jrs(&["test", "--retries", "0"]);
    assert_eq!(code, 1, "{stderr}");
    assert!(!stderr.contains("Retrying"), "{stderr}");
    assert!(!reports.join("retry-1").exists(), "old retries are cleared");
}

#[test]
fn fail_fast_stops_at_the_first_failure_on_either_launcher_line() {
    let toolchain = require_jdk!();
    for launcher in [FAKE_LAUNCHER_1, FAKE_LAUNCHER_6] {
        let scratch = Scratch::new(&format!("tests-fail-fast-{launcher}"));
        let p = junit_project(
            &scratch,
            &toolchain,
            launcher,
            "retries = 2",
            &[("StopTest.java", STOP_TEST)],
        );
        let native = launcher == FAKE_LAUNCHER_6;

        let (code, stdout, stderr) = p.jrs(&["test", "--fail-fast"]);
        assert_eq!(code, 1, "{launcher}: {stderr}");
        assert!(
            !scratch.join("markers/slow-ran").exists(),
            "{launcher}: the test after the failure ran"
        );
        assert!(
            stderr.contains("stopped at the first failure"),
            "{launcher}: {stderr}"
        );
        assert!(
            !stderr.contains("Retrying"),
            "{launcher}: a stopped run is not retried: {stderr}"
        );
        // JUnit 6 stops itself. A 1.x launcher would refuse the option, and
        // prints its tree only at the end, so jrs follows its test feed and
        // stops it once the failure and its trace are through — before the
        // next test's start is shown — and says what that costs.
        let args = launches(&stdout)[0];
        assert_eq!(args.contains("--fail-fast"), native, "{args}");
        let details = if native {
            "--details=tree"
        } else {
            "--details=testfeed"
        };
        assert!(args.contains(details), "{args}");
        assert!(stdout.contains("the first failure"), "{launcher}: {stdout}");
        assert!(!stdout.contains("bSlow"), "{launcher}: {stdout}");
        assert_eq!(
            stderr.contains("has no --fail-fast"),
            !native,
            "{launcher}: {stderr}"
        );
        assert_eq!(
            p.root.join("target/test-reports/index.html").is_file(),
            native,
            "{launcher}: a launcher stopped by jrs writes no XML, so there is no page"
        );
    }
}
