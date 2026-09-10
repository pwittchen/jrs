//! Tests that talk to Maven Central.
//!
//! Gated behind a feature flag so the default `cargo test` stays hermetic
//! (SPEC §10.1). Run them with:
//!
//! ```text
//! cargo test --features network-tests --test network
//! ```

#![cfg(feature = "network-tests")]

mod common;

use std::path::Path;

use common::Scratch;
use jrs::compile::{self, CompileUnit};
use jrs::manifest::Manifest;
use jrs::project::Project;
use jrs::resolve::cache::Cache;
use jrs::resolve::coord::Ga;
use jrs::resolve::repo::Fetcher;
use jrs::resolve::{self, Classpath};
use jrs::test as junit;
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

fn fetcher(scratch: &Scratch) -> Fetcher {
    Fetcher::new(
        jrs::manifest::blank("probe", "0", Path::new(".")).repositories,
        Cache::with_root(scratch.join("cache")),
        false,
    )
}

#[test]
fn guavas_transitive_graph_resolves_from_central() {
    let scratch = Scratch::new("net-guava");
    let manifest = Manifest::parse(
        "[project]\nname='app'\nversion='1.0.0'\n\
         [dependencies]\n\"com.google.guava:guava\" = \"33.0.0-jre\"",
        Path::new("/p/jrs.toml"),
        Path::new("/p"),
    )
    .unwrap();

    let fetcher = fetcher(&scratch);
    let mut resolution = resolve::resolve(&manifest, &fetcher, 8).unwrap();
    resolve::fetch_jars(&mut resolution, &fetcher, 8).unwrap();

    // Guava's published dependencies, as of 33.0.0-jre.
    for artifact in [
        "guava",
        "failureaccess",
        "jsr305",
        "checker-qual",
        "error_prone_annotations",
        "j2objc-annotations",
    ] {
        assert!(
            resolution
                .packages
                .iter()
                .any(|p| p.coord.artifact == artifact),
            "`{artifact}` is missing from the resolved graph"
        );
    }
    assert!(resolution.classpath(Classpath::Compile).len() >= 6);
    assert!(
        resolution
            .get(&Ga::new("com.google.guava", "guava"))
            .unwrap()
            .checksum
            .is_some(),
        "every downloaded artifact is checksum-verified"
    );
}

#[test]
fn a_project_with_dependencies_compiles_tests_and_runs_them() {
    let toolchain = require_jdk!();
    let scratch = Scratch::new("net-junit");
    let root = scratch.join("app");

    std::fs::create_dir_all(root.join("src/main/java/com/example")).unwrap();
    std::fs::create_dir_all(root.join("src/test/java/com/example")).unwrap();
    std::fs::write(
        root.join("jrs.toml"),
        "[project]\nname = \"app\"\nversion = \"1.0.0\"\n\
         main-class = \"com.example.App\"\n\n\
         [dependencies]\n\"org.apache.commons:commons-lang3\" = \"3.14.0\"\n\n\
         [dev-dependencies]\n\"org.junit.jupiter:junit-jupiter\" = \"5.10.2\"\n",
    )
    .unwrap();
    std::fs::write(
        root.join("src/main/java/com/example/App.java"),
        "package com.example;\nimport org.apache.commons.lang3.StringUtils;\n\
         public final class App {\n\
         \x20 public static String shout(String s) { return StringUtils.upperCase(s) + \"!\"; }\n\
         \x20 public static void main(String[] args) { System.out.println(shout(\"hi\")); }\n}\n",
    )
    .unwrap();
    std::fs::write(
        root.join("src/test/java/com/example/AppTest.java"),
        "package com.example;\n\
         import static org.junit.jupiter.api.Assertions.assertEquals;\n\
         import org.junit.jupiter.api.Test;\n\
         class AppTest {\n\
         \x20 @Test void shouts() { assertEquals(\"HI!\", App.shout(\"hi\")); }\n\
         \x20 @Test void handlesEmpty() { assertEquals(\"!\", App.shout(\"\")); }\n}\n",
    )
    .unwrap();

    let manifest = Manifest::load(root.join("jrs.toml")).unwrap();
    let project = Project::new(&manifest);
    let fetcher = fetcher(&scratch);
    let mut resolution = resolve::resolve(&manifest, &fetcher, 8).unwrap();
    resolve::fetch_jars(&mut resolution, &fetcher, 8).unwrap();

    let release = toolchain.release(manifest.java.source).unwrap();
    let unit = |label: &str, sources, output, classpath| CompileUnit {
        label: label.into(),
        sources,
        output_dir: output,
        classpath,
        release,
        target: None,
        encoding: "UTF-8".into(),
        extra_args: Vec::new(),
        work_dir: project.work_dir(),
    };

    compile::compile(
        &toolchain,
        &unit(
            "main",
            project.main_sources().unwrap(),
            project.classes_dir(),
            resolution.classpath(Classpath::Compile),
        ),
        &silent_ui(),
    )
    .unwrap();

    let mut test_classpath = vec![project.classes_dir()];
    test_classpath.extend(resolution.classpath(Classpath::Test));
    compile::compile(
        &toolchain,
        &unit(
            "test",
            project.test_sources().unwrap(),
            project.test_classes_dir(),
            test_classpath.clone(),
        ),
        &silent_ui(),
    )
    .unwrap();

    // The console launcher is jrs's own dependency, at the platform version that
    // matches the declared Jupiter version.
    let launcher = junit::launcher_coordinate(&manifest).unwrap();
    assert_eq!(launcher.version, "1.10.2");
    let (launcher_jar, _) = fetcher.jar(&launcher).unwrap();

    let mut classpath = vec![project.test_classes_dir()];
    classpath.extend(test_classpath);
    classpath.push(launcher_jar);

    let outcome = junit::run(
        &toolchain,
        &junit::TestRun {
            classpath,
            scan_dir: project.test_classes_dir(),
            filter: None,
            color: false,
            ascii: true,
            launcher_version: launcher.version.clone(),
            work_dir: project.work_dir(),
            reports_dir: Some(project.target_dir().join("test-reports")),
            ..Default::default()
        },
        &silent_ui(),
    )
    .unwrap();
    assert!(
        project
            .target_dir()
            .join("test-reports/TEST-junit-jupiter.xml")
            .is_file(),
        "JUnit XML should land where CI looks for it"
    );

    assert!(
        outcome.ok(),
        "the launcher exited with {}",
        outcome.exit_code
    );
    assert_eq!(outcome.found, 2);
    assert_eq!(outcome.passed, 2);
    assert_eq!(outcome.failed, 0);
    assert_eq!(outcome.describe(), "2 tests, 2 passed");
}

#[test]
fn centrals_version_list_names_newer_releases() {
    let scratch = Scratch::new("net-metadata");
    let metadata = fetcher(&scratch)
        .metadata("com.google.guava", "guava")
        .unwrap();
    assert!(metadata.versions.iter().any(|v| v == "33.0.0-jre"));
    // `jrs outdated` stays on the flavour a project is on.
    let newer = jrs::resolve::metadata::newest(&metadata.versions, "33.0.0-android").unwrap();
    assert!(newer.ends_with("-android"), "{newer}");
    assert!(
        jrs::resolve::metadata::newest_release(&metadata)
            .is_some_and(|v| !jrs::resolve::metadata::is_prerelease(&v))
    );
}

#[test]
fn a_classified_artifact_downloads_from_central() {
    // JaCoCo's agent is only published classified, which is how `jrs test
    // --coverage` gets it.
    let scratch = Scratch::new("net-classifier");
    let (jar, _) = fetcher(&scratch)
        .jar(&junit::jacoco_agent(junit::JACOCO_VERSION))
        .unwrap();
    assert!(jar.ends_with(format!(
        "org.jacoco.agent-{}-runtime.jar",
        junit::JACOCO_VERSION
    )));
}
