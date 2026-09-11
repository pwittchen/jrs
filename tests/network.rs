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

use common::{Scratch, copy_dir, fixtures};
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
        foreign: None,
        main_api: None,
    };

    compile::compile(
        &toolchain,
        &unit(
            "main",
            project.main_sources().unwrap().files,
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
            project.test_sources().unwrap().files,
            project.test_classes_dir(),
            test_classpath.clone(),
        ),
        &silent_ui(),
    )
    .unwrap();

    // The console launcher is jrs's own dependency, at the platform version that
    // matches the declared Jupiter version.
    let launcher = junit::launcher_coordinate(&manifest, &resolution).unwrap();
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

// ---- Kotlin, Scala and Groovy with their real compilers (JVM_LANGUAGES.md) --
//
// One small mixed project per language, driven through the jrs binary as a
// user would: built, tested with the language's usual framework, run, fat-
// jarred, linked into an image, and built twice to compare the jars. This is
// the only place the real compilers' determinism is checked.

/// Run the jrs binary against `root`.
fn jrs(root: &Path, args: &[&str]) -> (i32, String, String) {
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
        .output()
        .unwrap();
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn write_project(root: &Path, files: &[(&str, &str)]) {
    for (path, contents) in files {
        let file = root.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, contents).unwrap();
    }
}

// ---- Spring Boot from start.spring.io (SPEC §11.3) --------------------------

/// A start.spring.io Gradle build, migrated, then built, tested and packaged
/// by jrs, and its flat fat jar run up to a started application context: the
/// proof that Boot's versions come through `[managed]` and that its nested
/// `bootJar` layout is not needed.
#[test]
fn a_spring_boot_project_from_gradle_migrates_builds_and_starts() {
    let toolchain = require_jdk!();
    let scratch = Scratch::new("net-spring-boot");
    let root = scratch.join("demo");
    copy_dir(&fixtures().join("migrate/spring-boot-gradle"), &root);
    std::fs::remove_file(root.join("expected-jrs.toml")).unwrap();

    let path = root.display().to_string();
    let (code, _, stderr) = jrs(&root, &["migrate", "--path", &path]);
    assert_eq!(code, 0, "{stderr}");
    let (code, _, stderr) = jrs(&root, &["test"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stderr.contains("1 passed"), "{stderr}");
    let (code, tree, stderr) = jrs(&root, &["tree", "--depth", "1"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(
        tree.contains("org.springframework.boot:spring-boot-starter-webmvc:4.1.1 (managed)"),
        "{tree}"
    );
    let (code, _, stderr) = jrs(&root, &["package", "--fat"]);
    assert_eq!(code, 0, "{stderr}");

    // Boot logs to stdout; the line it writes once the context is up says so.
    let mut child = std::process::Command::new(&toolchain.java)
        .arg("-jar")
        .arg(root.join("target/demo-0.0.1-SNAPSHOT.jar"))
        .arg("--server.port=0")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let (lines, received) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        use std::io::BufRead;
        for line in std::io::BufReader::new(stdout)
            .lines()
            .map_while(Result::ok)
        {
            if lines.send(line).is_err() {
                break;
            }
        }
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    let mut seen = Vec::new();
    let started = loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        match received.recv_timeout(left) {
            Ok(line) if line.contains("Started DemoApplication") => break true,
            Ok(line) => seen.push(line),
            Err(_) => break false,
        }
    };
    let _ = child.kill();
    let _ = child.wait();
    assert!(
        started,
        "the application never started:\n{}",
        seen.join("\n")
    );
}

/// Test, run, package fat, link, and package fat again from clean: the jars
/// must match byte for byte. Returns the fat jar's entry names.
fn exercise(root: &Path, jar: &str, expected_output: &str, tests: &str) -> Vec<String> {
    let (code, _, stderr) = jrs(root, &["test"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stderr.contains(tests), "{stderr}");

    let (code, stdout, stderr) = jrs(root, &["run"]);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stdout.trim(), expected_output);

    let (code, _, stderr) = jrs(root, &["package", "--fat", "--jlink"]);
    assert_eq!(code, 0, "{stderr}");
    let jar = root.join("target").join(jar);
    let first = std::fs::read(&jar).unwrap();
    let launcher = if cfg!(windows) { ".bat" } else { "" };
    let name = root.file_name().unwrap().to_string_lossy().into_owned();
    assert!(
        root.join(format!("target/image/bin/{name}{launcher}"))
            .is_file(),
        "no image launcher"
    );

    assert_eq!(jrs(root, &["clean"]).0, 0);
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let (code, _, stderr) = jrs(root, &["package", "--fat"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(
        first == std::fs::read(&jar).unwrap(),
        "two builds of the same sources gave different jars"
    );

    let mut archive = zip::ZipArchive::new(std::fs::File::open(&jar).unwrap()).unwrap();
    (0..archive.len())
        .map(|i| archive.by_index(i).unwrap().name().to_string())
        .collect()
}

#[test]
fn a_mixed_kotlin_project_builds_with_the_real_compiler() {
    let _toolchain = require_jdk!();
    let scratch = Scratch::new("net-kotlin");
    let root = scratch.join("kotlin-app");
    write_project(
        &root,
        &[
            (
                "jrs.toml",
                "[project]\nname = \"kotlin-app\"\nversion = \"1.0.0\"\n\
                 main-class = \"com.example.MainKt\"\n\n[java]\nsource = 17\n\n\
                 [kotlin]\nversion = \"2.4.20\"\n\n[dev-dependencies]\n\
                 \"org.junit.jupiter:junit-jupiter\" = \"5.13.4\"\n\
                 \"org.jetbrains.kotlin:kotlin-test-junit5\" = \"2.4.20\"\n",
            ),
            (
                "src/main/kotlin/com/example/Main.kt",
                "package com.example\n\nclass Greeter(private val name: String) {\n    \
                 fun greet(): String = \"Hello, \" + Shout.shout(name)\n}\n\n\
                 internal fun secret(): Int = 42\n\n\
                 fun main() {\n    println(Greeter(\"kotlin\").greet() + \" / \" + Shout.viaKotlin())\n}\n",
            ),
            (
                "src/main/java/com/example/Shout.java",
                "package com.example;\n\npublic final class Shout {\n    \
                 public static String shout(String s) { return s.toUpperCase(); }\n    \
                 public static String viaKotlin() { return new Greeter(\"java\").greet(); }\n}\n",
            ),
            (
                "src/test/kotlin/com/example/GreeterTest.kt",
                "package com.example\n\nimport kotlin.test.Test\nimport kotlin.test.assertEquals\n\n\
                 class GreeterTest {\n    @Test\n    fun greets() {\n        \
                 assertEquals(\"Hello, JRS\", Greeter(\"jrs\").greet())\n    }\n\n    \
                 @Test\n    fun seesInternals() {\n        assertEquals(42, secret())\n    }\n}\n",
            ),
        ],
    );
    let entries = exercise(
        &root,
        "kotlin-app-1.0.0.jar",
        "Hello, KOTLIN / Hello, JAVA",
        "2 tests, 2 passed",
    );
    assert!(
        entries.iter().any(|e| e == "kotlin/Unit.class"),
        "the implied stdlib is in the fat jar"
    );
}

#[test]
fn java_main_code_with_spock_tests_builds_with_the_real_groovy() {
    let _toolchain = require_jdk!();
    let scratch = Scratch::new("net-groovy");
    let root = scratch.join("groovy-app");
    write_project(
        &root,
        &[
            (
                "jrs.toml",
                "[project]\nname = \"groovy-app\"\nversion = \"1.0.0\"\n\
                 main-class = \"com.example.Main\"\n\n[java]\nsource = 17\n\n\
                 [groovy]\nversion = \"5.1.2\"\n\n[dev-dependencies]\n\
                 \"org.apache.groovy:groovy\" = \"5.1.2\"\n\
                 \"org.spockframework:spock-core\" = \"2.4-groovy-5.0\"\n",
            ),
            (
                "src/main/java/com/example/Main.java",
                "package com.example;\n\npublic final class Main {\n    \
                 static int total(int... prices) { int t = 0; for (int p : prices) t += p; return t; }\n    \
                 public static void main(String[] args) { System.out.println(total(1, 2, 3)); }\n}\n",
            ),
            (
                "src/test/groovy/com/example/MainSpec.groovy",
                "package com.example\n\nimport spock.lang.Specification\n\n\
                 class MainSpec extends Specification {\n    def \"adds up prices\"() {\n        \
                 expect:\n        Main.total(*prices) == total\n\n        where:\n        \
                 prices    | total\n        [1, 2]    | 3\n        [5, 5, 5] | 15\n    }\n}\n",
            ),
        ],
    );
    let entries = exercise(&root, "groovy-app-1.0.0.jar", "6", "3 tests, 3 passed");
    assert!(
        !entries.iter().any(|e| e.starts_with("groovy/")),
        "Groovy is a test dependency here, and stays out of the jar"
    );
}

#[test]
fn a_mixed_scala_3_project_builds_with_the_real_compiler() {
    let _toolchain = require_jdk!();
    let scratch = Scratch::new("net-scala");
    let root = scratch.join("scala-app");
    write_project(
        &root,
        &[
            (
                "jrs.toml",
                "[project]\nname = \"scala-app\"\nversion = \"1.0.0\"\n\
                 main-class = \"com.example.Main\"\n\n[java]\nsource = 17\n\n\
                 [scala]\nversion = \"3.9.0\"\n\n[dev-dependencies]\n\
                 \"org.scalameta:munit_3\" = \"1.3.6\"\n",
            ),
            (
                "src/main/scala/com/example/Main.scala",
                "package com.example\n\nobject Main:\n  def greeting(name: String): String = \
                 \"Hello, \" + Shout.shout(name)\n\n  def main(args: Array[String]): Unit =\n    \
                 println(greeting(\"scala\") + \" / \" + Shout.viaScala())\n",
            ),
            (
                "src/main/java/com/example/Shout.java",
                "package com.example;\n\npublic final class Shout {\n    \
                 public static String shout(String s) { return s.toUpperCase(); }\n    \
                 public static String viaScala() { return Main.greeting(\"java\"); }\n}\n",
            ),
            (
                "src/test/scala/com/example/MainSuite.scala",
                "package com.example\n\nclass MainSuite extends munit.FunSuite:\n  \
                 test(\"greets\") {\n    assertEquals(Main.greeting(\"jrs\"), \"Hello, JRS\")\n  }\n",
            ),
        ],
    );
    let entries = exercise(
        &root,
        "scala-app-1.0.0.jar",
        "Hello, SCALA / Hello, JAVA",
        "1 tests, 1 passed",
    );
    assert!(
        entries.iter().any(|e| e == "scala/Option.class"),
        "the implied library is in the fat jar"
    );
}

// ---- Scaladoc and Groovydoc with the real tools (SPEC §7.4) ---------------

/// `jrs doc`, then `jrs package --javadoc` twice: the pages are where they
/// belong, and the Javadoc jar is byte-identical from one run to the next.
fn documented(root: &Path, pages: &[&str]) {
    let (code, _, stderr) = jrs(root, &["doc"]);
    assert_eq!(code, 0, "{stderr}");
    for page in pages {
        assert!(
            root.join("target/doc").join(page).is_file(),
            "no {page}: {stderr}"
        );
    }
    let name = root.file_name().unwrap().to_string_lossy().into_owned();
    let jar = root.join(format!("target/{name}-1.0.0-javadoc.jar"));
    let (code, _, stderr) = jrs(root, &["package", "--javadoc"]);
    assert_eq!(code, 0, "{stderr}");
    let first = std::fs::read(&jar).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let (code, _, stderr) = jrs(root, &["package", "--javadoc"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(
        first == std::fs::read(&jar).unwrap(),
        "two runs gave different Javadoc jars"
    );
}

const DOC_HELPER: &str = "package com.example;\n\n/** A Java helper. */\n\
    public final class Helper {\n    private Helper() {}\n\n    /** Upper-cases. */\n    \
    public static String shout(String s) { return s.toUpperCase(); }\n}\n";

#[test]
fn scala_and_groovy_are_documented_with_their_real_tools() {
    let _toolchain = require_jdk!();
    let scratch = Scratch::new("net-doc");

    let groovy = scratch.join("groovy-doc");
    write_project(
        &groovy,
        &[
            (
                "jrs.toml",
                "[project]\nname = \"groovy-doc\"\nversion = \"1.0.0\"\n\n[java]\nsource = 17\n\n\
                 [groovy]\nversion = \"5.1.2\"\n",
            ),
            (
                "src/main/groovy/com/example/Greeter.groovy",
                "package com.example\n\n/** Greets people, in Groovy. */\nclass Greeter {\n    \
                 String name\n\n    /** The greeting. */\n    \
                 String greet() { Helper.shout(\"hello $name\") }\n}\n",
            ),
            ("src/main/java/com/example/Helper.java", DOC_HELPER),
        ],
    );
    documented(
        &groovy,
        &[
            "index.html",
            "com/example/Greeter.html",
            "com/example/Helper.html",
        ],
    );

    // Scala 3.9 needs the jackson pin to start at all; Scala 2's scaladoc is
    // the compiler's, and documents the Java source too.
    for (name, version) in [("scala3-doc", "3.9.0"), ("scala2-doc", "2.13.18")] {
        let root = scratch.join(name);
        let manifest = format!(
            "[project]\nname = \"{name}\"\nversion = \"1.0.0\"\n\n[java]\nsource = 17\n\n\
             [scala]\nversion = \"{version}\"\n"
        );
        write_project(
            &root,
            &[
                ("jrs.toml", &manifest),
                (
                    "src/main/scala/com/example/Greeter.scala",
                    "package com.example\n\n/** Greets people, in Scala. */\n\
                     class Greeter(name: String) {\n  /** The greeting. */\n  \
                     def greet(): String = Helper.shout(\"hello \" + name)\n}\n",
                ),
                ("src/main/java/com/example/Helper.java", DOC_HELPER),
            ],
        );
        let mut pages = vec!["index.html", "com/example/Greeter.html"];
        if version.starts_with('2') {
            pages.push("com/example/Helper.html");
        }
        documented(&root, &pages);
    }
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

// ---- test reports, reruns, retries, --fail-fast and coverage minimums ------
//
// tests/build.rs drives these through a fake launcher. Here they meet the
// real one, whose XML the page and the rerun selectors are read from, on both
// launcher lines, and real JaCoCo.

const CALC: &str = "package com.example;\n\npublic final class Calc {\n    \
    private Calc() {}\n\n    public static int add(int a, int b) {\n        return a + b;\n    }\n\n    \
    public static int unused() {\n        return 42;\n    }\n}\n";

/// A JUnit 5 suite with every shape of test a rerun has to select: a method
/// with parameters, a nested class, one invocation of a parameterised test,
/// one dynamic test, and a test that fails only the first time it runs.
const SHAPES_TEST: &str = r#"package com.example;

import static org.junit.jupiter.api.Assertions.*;

import java.nio.file.Files;
import java.nio.file.Path;
import java.util.stream.Stream;
import org.junit.jupiter.api.*;
import org.junit.jupiter.params.ParameterizedTest;
import org.junit.jupiter.params.provider.ValueSource;

class ShapesTest {
    @Test
    void passes() {
        assertEquals(2, Calc.add(1, 1));
    }

    @Test
    void withInfo(TestInfo info) {
        fail("plain <method> & parameters");
    }

    @Test
    void flaky() throws Exception {
        Path seen = Path.of(System.getProperty("marker.dir"), "flaky-seen");
        if (!Files.exists(seen)) {
            Files.createFile(seen);
            fail("only the first time");
        }
    }

    @ParameterizedTest
    @ValueSource(ints = {1, 2, 3})
    void param(int n) {
        assertNotEquals(2, n);
    }

    @TestFactory
    Stream<DynamicTest> dynamic() {
        return Stream.of(
            DynamicTest.dynamicTest("ok", () -> {}),
            DynamicTest.dynamicTest("bad", () -> fail("dynamic")));
    }

    @Nested
    class Inner {
        @Test
        void innerPasses() {}

        @Test
        void innerFails() {
            fail("nested");
        }
    }
}
"#;

/// A failure, then a test slow enough that jrs is long gone if the run
/// stops at the failure, and that leaves a mark if it runs.
const STOP_TEST: &str = r#"package com.example;

import java.nio.file.Files;
import java.nio.file.Path;
import org.junit.jupiter.api.*;

@TestMethodOrder(MethodOrderer.MethodName.class)
class StopTest {
    @Test
    void aFails() {
        Assertions.fail("the first failure");
    }

    @Test
    void bSlow() throws Exception {
        Thread.sleep(5000);
        Files.createFile(Path.of(System.getProperty("marker.dir"), "slow-ran"));
    }
}
"#;

/// A project on `junit-jupiter` at `jupiter`, with a `marker.dir` for its
/// tests and `test_table` in `[test]`.
fn tested_project(scratch: &Scratch, jupiter: &str, test_table: &str, test: (&str, &str)) {
    let markers = scratch.join("markers");
    std::fs::create_dir_all(&markers).unwrap();
    let manifest = format!(
        "[project]\nname = \"tested\"\nversion = \"1.0.0\"\n\n\
         [test]\njvm-args = ['-Dmarker.dir={}']\n{test_table}\n\n\
         [dev-dependencies]\n\"org.junit.jupiter:junit-jupiter\" = \"{jupiter}\"\n",
        markers.display()
    );
    let test_path = format!("src/test/java/com/example/{}", test.0);
    write_project(
        &scratch.join("app"),
        &[
            ("jrs.toml", &manifest),
            ("src/main/java/com/example/Calc.java", CALC),
            (&test_path, test.1),
        ],
    );
}

#[test]
fn the_real_launchers_xml_reruns_each_shape_of_test_on_its_own() {
    let _toolchain = require_jdk!();
    let scratch = Scratch::new("net-test-reruns");
    tested_project(
        &scratch,
        "5.13.4",
        "retries = 1",
        ("ShapesTest.java", SHAPES_TEST),
    );
    let root = scratch.join("app");

    let (code, _, stderr) = jrs(&root, &["test"]);
    assert_eq!(code, 1, "{stderr}");
    assert!(
        stderr.contains("Retrying 5 failed tests (attempt 2 of 2)"),
        "{stderr}"
    );
    assert!(
        stderr.contains("Flaky com.example.ShapesTest#flaky() (passed on attempt 2)"),
        "{stderr}"
    );
    assert!(stderr.contains("5 passed, 1 flaky, 4 failed"), "{stderr}");

    // The retry ran each failure on its own: the one invocation, the one
    // dynamic test, the one nested method — and nothing that had passed.
    let reports = root.join("target/test-reports");
    let retry = std::fs::read_to_string(reports.join("retry-1/TEST-junit-jupiter.xml")).unwrap();
    let ran = |name: &str| retry.contains(&format!("name=\"{name}\""));
    for name in [
        "withInfo(TestInfo)",
        "flaky()",
        "param(int)[2]",
        "dynamic()[2]",
        "innerFails()",
    ] {
        assert!(ran(name), "{name} was not retried:\n{retry}");
    }
    for name in [
        "passes()",
        "param(int)[1]",
        "param(int)[3]",
        "dynamic()[1]",
        "innerPasses()",
    ] {
        assert!(!ran(name), "{name} passed, yet was retried:\n{retry}");
    }

    let html = std::fs::read_to_string(reports.join("index.html")).unwrap();
    assert!(
        html.contains("plain &lt;method&gt; &amp; parameters"),
        "{html}"
    );
    assert!(
        html.contains("<span class=\"badge flaky\">FLAKY</span>"),
        "{html}"
    );

    // `--rerun-failed` picks up the four still failing, and only those.
    let (code, _, stderr) = jrs(&root, &["test", "--rerun-failed", "--retries", "0"]);
    assert_eq!(code, 1, "{stderr}");
    assert!(
        stderr.contains("Testing 4 tests that failed in the last run"),
        "{stderr}"
    );
    assert!(stderr.contains(" 0 passed, 4 failed"), "{stderr}");
}

#[test]
fn the_real_launcher_fails_fast_on_either_line() {
    let _toolchain = require_jdk!();
    // JUnit 5 runs on a 1.x launcher, which jrs has to stop itself; JUnit 6's
    // launcher stops on its own.
    for (jupiter, native) in [("5.13.4", false), ("6.0.0", true)] {
        let scratch = Scratch::new(&format!("net-fail-fast-{jupiter}"));
        tested_project(
            &scratch,
            jupiter,
            "retries = 1",
            ("StopTest.java", STOP_TEST),
        );
        let (code, _, stderr) = jrs(&scratch.join("app"), &["test", "--fail-fast"]);
        assert_eq!(code, 1, "{jupiter}: {stderr}");
        assert!(
            !scratch.join("markers/slow-ran").exists(),
            "{jupiter}: the test after the failure ran"
        );
        assert!(
            stderr.contains("stopped at the first failure"),
            "{jupiter}: {stderr}"
        );
        assert!(!stderr.contains("Retrying"), "{jupiter}: {stderr}");
        assert_eq!(
            stderr.contains("has no --fail-fast"),
            !native,
            "{jupiter}: {stderr}"
        );
    }
}

#[test]
fn coverage_minimums_fail_a_run_that_falls_short() {
    let _toolchain = require_jdk!();
    let scratch = Scratch::new("net-coverage-minimum");
    let test = "package com.example;\n\nimport static org.junit.jupiter.api.Assertions.assertEquals;\n\
                import org.junit.jupiter.api.Test;\n\nclass CalcTest {\n    @Test\n    void adds() {\n        \
                assertEquals(2, Calc.add(1, 1));\n    }\n}\n";
    // One of Calc's two counted lines runs: JaCoCo leaves the empty private
    // constructor out.
    tested_project(
        &scratch,
        "5.13.4",
        "coverage-minimum = { line = 0.9, branch = 0.5 }",
        ("CalcTest.java", test),
    );
    let root = scratch.join("app");

    let (code, _, stderr) = jrs(&root, &["test", "--coverage"]);
    assert_eq!(code, 1, "{stderr}");
    assert!(
        stderr.contains("coverage is below `test.coverage-minimum`"),
        "{stderr}"
    );
    assert!(
        stderr.contains("line coverage is 50% (1 of 2), below the minimum of 90%"),
        "{stderr}"
    );
    assert!(
        !stderr.contains("branch coverage"),
        "no branches, so nothing to fall short of: {stderr}"
    );

    // Without --coverage there are no totals, and the minimum is not checked.
    let (code, _, stderr) = jrs(&root, &["test"]);
    assert_eq!(code, 0, "{stderr}");

    let manifest = root.join("jrs.toml");
    let text = std::fs::read_to_string(&manifest).unwrap();
    std::fs::write(&manifest, text.replace("line = 0.9", "line = 0.3")).unwrap();
    let (code, _, stderr) = jrs(&root, &["test", "--coverage"]);
    assert_eq!(code, 0, "{stderr}");
}
