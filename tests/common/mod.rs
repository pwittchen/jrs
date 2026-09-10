//! Shared scaffolding for the integration tests.
//!
//! Two things live here: a scratch directory that cleans itself up, and a
//! `file://` repository fixture. The repository's POMs are checked in under
//! `tests/fixtures/repo`; the jars beside them are synthesised at test time, so
//! nothing binary is committed and every test still runs without a network
//! (SPEC §10.1).

#![allow(dead_code)]

use std::path::{Path, PathBuf};

use jrs::manifest::Repository;
use jrs::resolve::cache::Cache;
use jrs::resolve::coord::Coord;
use jrs::resolve::repo::{self, Fetcher};
use jrs::toolchain::Toolchain;

/// A scratch directory removed when the test ends.
pub struct Scratch {
    pub path: PathBuf,
}

impl Scratch {
    pub fn new(name: &str) -> Scratch {
        let path = std::env::temp_dir().join(format!(
            "jrs-it-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("could not create a scratch directory");
        Scratch { path }
    }

    pub fn join(&self, relative: &str) -> PathBuf {
        self.path.join(relative)
    }

    pub fn write(&self, relative: &str, contents: &str) -> PathBuf {
        let file = self.path.join(relative);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, contents).unwrap();
        file
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

pub fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// Copy a directory tree, used to work on a fixture without mutating it.
pub fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// The checked-in repository fixture, laid out in a scratch directory with jars
/// and their checksums filled in.
pub struct FixtureRepo {
    pub root: PathBuf,
    pub cache: PathBuf,
}

impl FixtureRepo {
    pub fn new(scratch: &Scratch) -> FixtureRepo {
        let root = scratch.join("repo");
        copy_dir(&fixtures().join("repo"), &root);

        let fixture = FixtureRepo {
            root,
            cache: scratch.join("cache"),
        };
        // Every POM in the fixture gets a jar beside it, so resolution has
        // something to put on a classpath. `app-parent` is `pom`-packaged and
        // deliberately gets none.
        for coord in [
            Coord::new("org.example", "lib", "1.0.0"),
            Coord::new("org.example", "lib", "2.0.0"),
            Coord::new("org.example", "core", "1.0.0"),
        ] {
            fixture.publish_jar(&coord, format!("jar for {coord}").as_bytes());
        }
        fixture
    }

    pub fn publish_jar(&self, coord: &Coord, bytes: &[u8]) {
        let path = self.root.join(coord.repo_path("jar"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, bytes).unwrap();
        std::fs::write(path.with_extension("jar.sha1"), repo::sha1_hex(bytes)).unwrap();
    }

    pub fn publish_pom(&self, coord: &Coord, xml: &str) {
        let path = self.root.join(coord.repo_path("pom"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, xml).unwrap();
        std::fs::write(
            path.with_extension("pom.sha1"),
            repo::sha1_hex(xml.as_bytes()),
        )
        .unwrap();
    }

    pub fn repositories(&self) -> Vec<Repository> {
        vec![Repository {
            name: "fixture".into(),
            url: repo::file_url(&self.root),
        }]
    }

    pub fn fetcher(&self) -> Fetcher {
        Fetcher::new(self.repositories(), Cache::with_root(&self.cache), false)
    }

    pub fn offline_fetcher(&self) -> Fetcher {
        Fetcher::new(self.repositories(), Cache::with_root(&self.cache), true)
    }

    /// A `[repositories]` block pointing at this fixture.
    pub fn manifest_section(&self) -> String {
        format!(
            "[repositories]\nfixture = \"{}\"\n",
            repo::file_url(&self.root)
        )
    }

    /// Compile `tests/fixtures/fake-compiler` and publish it as every
    /// compiler jrs resolves, each with a support jar as its one transitive
    /// dependency, and a one-class runtime library for each language. Needs a
    /// JDK; nothing binary is committed.
    pub fn publish_fake_compilers(&self, scratch: &Scratch, toolchain: &Toolchain) {
        let src = fixtures().join("fake-compiler");
        let work = scratch.join("fake-compiler");
        let support_classes = work.join("support");
        let compiler_classes = work.join("compiler");
        javac(
            toolchain,
            &[src.join("fake/support/Support.java")],
            &support_classes,
            &[],
        );
        let sources: Vec<PathBuf> = find_java(&src)
            .into_iter()
            .filter(|p| !p.starts_with(src.join("fake/support")))
            .collect();
        javac(
            toolchain,
            &sources,
            &compiler_classes,
            std::slice::from_ref(&support_classes),
        );

        let support = Coord::new("org.example.fake", "fake-compiler-support", "1.0");
        self.publish_pom(&support, &pom(&support, &[]));
        self.publish_jar(&support, &jar(&support_classes, &work.join("support.jar")));
        let compiler = jar(&compiler_classes, &work.join("compiler.jar"));
        for (group, artifact, version) in [
            (
                "org.jetbrains.kotlin",
                "kotlin-compiler-embeddable",
                FAKE_KOTLIN,
            ),
            ("org.scala-lang", "scala3-compiler_3", FAKE_SCALA),
            // Groovy's compiler is in its runtime jar, which is also the
            // runtime library a Groovy project implies.
            ("org.apache.groovy", "groovy", FAKE_GROOVY),
        ] {
            let coord = Coord::new(group, artifact, version);
            self.publish_pom(&coord, &pom(&coord, &[&support]));
            self.publish_jar(&coord, &compiler);
        }

        for (group, artifact, version, class) in [
            (
                "org.jetbrains.kotlin",
                "kotlin-stdlib",
                FAKE_KOTLIN,
                "kotlin.FakeStdlib",
            ),
            (
                "org.scala-lang",
                "scala-library",
                FAKE_SCALA,
                "scala.FakeLibrary",
            ),
            (
                "org.scala-lang",
                "scala3-library_3",
                FAKE_SCALA,
                "scala.FakeShim",
            ),
        ] {
            let coord = Coord::new(group, artifact, version);
            let (package, name) = class.rsplit_once('.').unwrap();
            let dir = work.join(artifact);
            let source = dir.join(format!("src/{package}/{name}.java"));
            std::fs::create_dir_all(source.parent().unwrap()).unwrap();
            std::fs::write(
                &source,
                format!(
                    "package {package};\n\npublic final class {name} {{\n    \
                     private {name}() {{}}\n\n    public static String mark() {{\n        \
                     return \"{artifact} {version}\";\n    }}\n}}\n"
                ),
            )
            .unwrap();
            javac(toolchain, &[source], &dir.join("classes"), &[]);
            self.publish_pom(&coord, &pom(&coord, &[]));
            self.publish_jar(&coord, &jar(&dir.join("classes"), &dir.join("lib.jar")));
        }
    }
}

/// The versions the fake compilers are published at: new enough for jrs's
/// minimums, and not a real release of anything.
pub const FAKE_KOTLIN: &str = "2.9.9";
pub const FAKE_SCALA: &str = "3.9.9";
pub const FAKE_GROOVY: &str = "4.9.9";

fn pom(coord: &Coord, dependencies: &[&Coord]) -> String {
    let deps: String = dependencies
        .iter()
        .map(|d| {
            format!(
                "<dependency><groupId>{}</groupId><artifactId>{}</artifactId>\
                 <version>{}</version></dependency>",
                d.group, d.artifact, d.version
            )
        })
        .collect();
    format!(
        "<project><groupId>{}</groupId><artifactId>{}</artifactId>\
         <version>{}</version><dependencies>{deps}</dependencies></project>",
        coord.group, coord.artifact, coord.version
    )
}

fn find_java(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            out.extend(find_java(&path));
        } else if path.extension().is_some_and(|e| e == "java") {
            out.push(path);
        }
    }
    out.sort();
    out
}

fn javac(toolchain: &Toolchain, sources: &[PathBuf], out: &Path, classpath: &[PathBuf]) {
    let mut command = std::process::Command::new(&toolchain.javac);
    command
        .args(["--release", "17", "-proc:none", "-d"])
        .arg(out);
    if !classpath.is_empty() {
        command.arg("-cp").arg(Toolchain::classpath(classpath));
    }
    let output = command.args(sources).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn jar(classes: &Path, output: &Path) -> Vec<u8> {
    jrs::package::write_thin_jar(classes, output, &jrs::package::JarManifest::default()).unwrap();
    std::fs::read(output).unwrap()
}

/// Skip a JDK-dependent test when there is no JDK, saying so out loud.
///
/// CI installs one (see `.github/workflows/rust.yml`), so this only fires on a
/// developer machine without `javac`.
#[macro_export]
macro_rules! require_jdk {
    () => {
        match jrs::toolchain::Toolchain::discover() {
            Ok(toolchain) => toolchain,
            Err(e) => {
                eprintln!(
                    "SKIPPED {}: no usable JDK ({e})",
                    concat!(module_path!(), "::", line!())
                );
                return;
            }
        }
    };
}
