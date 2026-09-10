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
