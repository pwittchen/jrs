//! Fixture Java projects, built end to end through the library API.
//!
//! These are the tests that need a JDK. CI installs one; on a machine without
//! `javac` they announce that they were skipped rather than failing quietly
//! (SPEC §10.1).

mod common;

use std::path::{Path, PathBuf};

use common::{FixtureRepo, Scratch, copy_dir, fixtures};
use jrs::compile::{self, CompileUnit};
use jrs::manifest::Manifest;
use jrs::package::{self, JarManifest};
use jrs::project::{self, Project};
use jrs::resolve::{self, Classpath};
use jrs::runner;
use jrs::toolchain::Toolchain;
use jrs::ui::{CharsetChoice, Ui, UiOptions, When};

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
        .args(runner::java_args(&runtime, "com.example.App", &[]))
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
