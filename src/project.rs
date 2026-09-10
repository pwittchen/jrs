//! Layout discovery, source globbing, and the target directory.
//!
//! `target/` is fully disposable: nothing here writes anything into it that
//! could not be regenerated, so `jrs clean` can never lose user data (SPEC §3).

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::compile::lang::Language;
use crate::error::{IoResultExt, JrsError, Result};
use crate::manifest::Manifest;

/// Which compile unit sources feed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    Main,
    Test,
}

impl Unit {
    fn name(self) -> &'static str {
        match self {
            Unit::Main => "main",
            Unit::Test => "test",
        }
    }
}

/// A compile unit's sources: every root, every language, sorted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sources {
    pub files: Vec<PathBuf>,
}

impl Sources {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.files.len()
    }

    #[must_use]
    pub fn count(&self, language: Language) -> usize {
        self.files
            .iter()
            .filter(|p| Language::of(p) == Some(language))
            .count()
    }

    /// The unit's language besides Java, when it has sources in one. The
    /// checks in [`Project::sources`] make sure there is at most one.
    #[must_use]
    pub fn foreign(&self) -> Option<Language> {
        Language::FOREIGN.into_iter().find(|l| self.count(*l) > 0)
    }

    /// `2 Kotlin + 1 Java source files`, counted by language, for the phase
    /// line; a unit with Java only reads `3 source files`, as it always has.
    #[must_use]
    pub fn describe(&self, noun: &str) -> String {
        if self.foreign().is_none() {
            return format!("{} {noun}", self.len());
        }
        let parts: Vec<String> = Language::FOREIGN
            .into_iter()
            .chain([Language::Java])
            .map(|l| (l, self.count(l)))
            .filter(|(_, n)| *n > 0)
            .map(|(l, n)| format!("{n} {l}"))
            .collect();
        format!("{} {noun}", parts.join(" + "))
    }
}

/// The directories a build reads from and writes to.
pub struct Project<'a> {
    pub manifest: &'a Manifest,
}

impl<'a> Project<'a> {
    #[must_use]
    pub fn new(manifest: &'a Manifest) -> Project<'a> {
        Project { manifest }
    }

    #[must_use]
    pub fn target_dir(&self) -> PathBuf {
        self.manifest.target_path()
    }

    #[must_use]
    pub fn classes_dir(&self) -> PathBuf {
        self.target_dir().join("classes")
    }

    #[must_use]
    pub fn test_classes_dir(&self) -> PathBuf {
        self.target_dir().join("test-classes")
    }

    /// jrs's own scratch space: argfiles, fingerprints, fat-jar staging.
    #[must_use]
    pub fn work_dir(&self) -> PathBuf {
        self.target_dir().join(".jrs")
    }

    #[must_use]
    pub fn jar_path(&self) -> PathBuf {
        self.target_dir().join(self.manifest.jar_name())
    }

    /// Where the unit's sources live: the project's root for it, then each
    /// turned-on language's.
    #[must_use]
    pub fn roots(&self, unit: Unit) -> Vec<PathBuf> {
        let m = self.manifest;
        let mut roots = vec![match unit {
            Unit::Main => m.source_path(),
            Unit::Test => m.test_path(),
        }];
        for config in &m.languages {
            let dir = m.root.join(match unit {
                Unit::Main => &config.source_dir,
                Unit::Test => &config.test_dir,
            });
            if !roots.contains(&dir) {
                roots.push(dir);
            }
        }
        roots
    }

    /// The main sources, in every language, sorted. See [`Project::sources`].
    ///
    /// # Errors
    ///
    /// As for [`Project::sources`].
    pub fn main_sources(&self) -> Result<Sources> {
        self.sources(Unit::Main, &[])
    }

    /// The test sources, in every language, sorted. See [`Project::sources`].
    ///
    /// # Errors
    ///
    /// As for [`Project::sources`].
    pub fn test_sources(&self) -> Result<Sources> {
        self.sources(Unit::Test, &[])
    }

    /// Every source file of `unit`: each of its roots and each `generated`
    /// directory, scanned for every language's extension, so a `.kt` file
    /// under `src/main/java` compiles too. The walk is the sorted one, so
    /// argfiles stay deterministic.
    ///
    /// Two rules are checked here, before any compiler runs. A source in a
    /// language the manifest has not turned on is an error rather than a file
    /// silently left out — which is also why a language's default directory
    /// is scanned when its table is missing. And a unit holds at most one
    /// language besides Java, since neither compiler reads the other's
    /// sources.
    ///
    /// # Errors
    ///
    /// [`JrsError::Manifest`] for either rule; [`JrsError::Io`] if a directory
    /// under a root cannot be read.
    pub fn sources(&self, unit: Unit, generated: &[PathBuf]) -> Result<Sources> {
        let m = self.manifest;
        let mut roots = self.roots(unit);
        roots.extend(generated.iter().cloned());
        let off: Vec<Language> = Language::FOREIGN
            .into_iter()
            .filter(|l| m.language(*l).is_none())
            .collect();
        for language in &off {
            let dir = m
                .root
                .join(format!("src/{}/{}", unit.name(), language.key()));
            if !roots.contains(&dir) {
                roots.push(dir);
            }
        }

        let mut files = Vec::new();
        for root in &roots {
            if root.is_dir() {
                walk(root, &mut |path| {
                    if Language::of(path).is_some() {
                        files.push(path.to_path_buf());
                    }
                })?;
            }
        }
        files.sort();
        files.dedup();
        let sources = Sources { files };

        for language in off {
            let n = sources.count(language);
            if n == 0 {
                continue;
            }
            let first = sources
                .files
                .iter()
                .find(|p| Language::of(p) == Some(language));
            let dir = first
                .and_then(|f| roots.iter().find(|r| f.starts_with(r)))
                .cloned()
                .unwrap_or_else(|| m.root.clone());
            return Err(JrsError::manifest(format!(
                "found {n} .{} files under {}, but {} has no [{key}] table\n\n\
                 add it to turn {language} on:\n\n    [{key}]\n    version = \"{}\"",
                language.extension(),
                dir.display(),
                m.path.display(),
                language.starter_version().unwrap_or_default(),
                key = language.key(),
            )));
        }
        let present: Vec<Language> = Language::FOREIGN
            .into_iter()
            .filter(|l| sources.count(*l) > 0)
            .collect();
        if present.len() > 1 {
            let counts: Vec<String> = present
                .iter()
                .map(|l| format!("{} .{}", sources.count(*l), l.extension()))
                .collect();
            let names: Vec<&str> = present.iter().map(|l| l.name()).collect();
            return Err(JrsError::manifest(format!(
                "the {} sources mix {} ({}); a compile unit mixes Java with at most one \
                 other language, since neither compiler reads the other's sources\n\n\
                 main and test are separate units, so Kotlin main code with Groovy tests is fine",
                unit.name(),
                names.join(" and "),
                counts.join(", ")
            )));
        }
        Ok(sources)
    }

    /// Remove `target/`. Returns whether there was anything to remove.
    ///
    /// # Errors
    ///
    /// [`JrsError::Manifest`] if the target directory is the project root, and
    /// [`JrsError::Io`] if it cannot be removed.
    pub fn clean(&self) -> Result<bool> {
        let dir = self.target_dir();
        if !dir.exists() {
            return Ok(false);
        }
        // Refuse to delete anything that is not plausibly a build directory —
        // a mistyped `target-dir` should not take the source tree with it.
        if dir == self.manifest.root {
            return Err(JrsError::manifest(
                "`project.target-dir` points at the project root; refusing to delete it",
            ));
        }
        std::fs::remove_dir_all(&dir).path(&dir)?;
        Ok(true)
    }
}

/// Every file under `root` with the given extension, sorted for determinism.
///
/// A missing root is not an error: a project with no tests is a normal project.
///
/// # Errors
///
/// [`JrsError::Io`] if a directory under `root` cannot be read.
pub fn find_by_extension(root: &Path, extension: &str) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    if !root.exists() {
        return Ok(out);
    }
    walk(root, &mut |path| {
        if path.extension().and_then(|e| e.to_str()) == Some(extension) {
            out.push(path.to_path_buf());
        }
    })?;
    out.sort();
    Ok(out)
}

/// Every file under `root`, whatever its name.
///
/// # Errors
///
/// [`JrsError::Io`] if a directory under `root` cannot be read.
pub fn find_all(root: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    if !root.exists() {
        return Ok(out);
    }
    walk(root, &mut |path| out.push(path.to_path_buf()))?;
    out.sort();
    Ok(out)
}

fn walk(dir: &Path, visit: &mut impl FnMut(&Path)) -> Result<()> {
    let entries = std::fs::read_dir(dir).path(dir)?;
    let mut subdirs = Vec::new();
    for entry in entries {
        let entry = entry.path(dir)?;
        let path = entry.path();
        let kind = entry.file_type().path(&path)?;
        if kind.is_dir() {
            subdirs.push(path);
        } else if kind.is_file() {
            visit(&path);
        }
    }
    // Deterministic traversal makes argfiles and jars byte-identical between runs.
    subdirs.sort();
    for sub in subdirs {
        walk(&sub, visit)?;
    }
    Ok(())
}

/// Copy `from` into `to`, preserving relative paths.
///
/// Files whose size and mtime already match are skipped, which is what keeps a
/// no-op `jrs build` from rewriting a resource tree (SPEC §7.3).
///
/// # Errors
///
/// [`JrsError::Io`] if `from` cannot be walked, a file's metadata cannot be
/// read, or a directory or file under `to` cannot be written.
pub fn copy_tree(from: &Path, to: &Path) -> Result<usize> {
    if !from.is_dir() {
        return Ok(0);
    }
    let mut copied = 0;
    for source in find_all(from)? {
        let relative = source.strip_prefix(from).unwrap_or(&source);
        let destination = to.join(relative);
        if up_to_date(&source, &destination)? {
            continue;
        }
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent).path(parent)?;
        }
        std::fs::copy(&source, &destination).path(&destination)?;
        copied += 1;
    }
    Ok(copied)
}

/// What [`sync_resources`] did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Synced {
    pub copied: usize,
    pub removed: usize,
}

/// Mirror `from` into `to`: copy what is new or changed, and delete what an
/// earlier run copied but has since disappeared from `from`.
///
/// `to` is shared with `javac`'s output, so it cannot simply be emptied first.
/// Instead `record` lists the paths this function put there last time, and only
/// those are ever deleted — a class file, or anything an annotation processor
/// generated, is never touched.
///
/// # Errors
///
/// [`JrsError::Io`] if a stale file cannot be removed (one already gone is
/// fine), copying fails as in [`copy_tree`], or `record` cannot be written.
pub fn sync_resources(from: &Path, to: &Path, record: &Path) -> Result<Synced> {
    let mut current: Vec<String> = if from.is_dir() {
        find_all(from)?
            .iter()
            .map(|p| slash_path(p.strip_prefix(from).unwrap_or(p)))
            .collect()
    } else {
        Vec::new()
    };
    current.sort();

    let previous = std::fs::read_to_string(record).unwrap_or_default();
    let mut removed = 0;
    for name in previous.lines() {
        // The record is jrs's own file, but a path that climbs out of `to` is
        // never followed, whoever wrote it.
        if name.is_empty() || name.split('/').any(|c| c == "..") {
            continue;
        }
        if current.binary_search_by(|c| c.as_str().cmp(name)).is_ok() {
            continue;
        }
        let stale = to.join(name);
        match std::fs::remove_file(&stale) {
            Ok(()) => removed += 1,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(JrsError::io(&stale, e)),
        }
    }

    let copied = copy_tree(from, to)?;

    if current.is_empty() {
        let _ = std::fs::remove_file(record);
    } else {
        if let Some(parent) = record.parent() {
            std::fs::create_dir_all(parent).path(parent)?;
        }
        let mut text = current.join("\n");
        text.push('\n');
        std::fs::write(record, text).path(record)?;
    }
    Ok(Synced { copied, removed })
}

/// A relative path with `/` separators on every platform.
#[must_use]
pub fn slash_path(p: &Path) -> String {
    p.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

fn up_to_date(source: &Path, destination: &Path) -> Result<bool> {
    let Ok(dest_meta) = std::fs::metadata(destination) else {
        return Ok(false);
    };
    let source_meta = std::fs::metadata(source).path(source)?;
    if source_meta.len() != dest_meta.len() {
        return Ok(false);
    }
    match (source_meta.modified(), dest_meta.modified()) {
        (Ok(s), Ok(d)) => Ok(s <= d),
        _ => Ok(false),
    }
}

/// A cheap picture of a set of files — path, size and mtime — for `--watch`.
/// Two pictures differ exactly when a file was added, removed or changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot(Vec<(PathBuf, u64, Option<SystemTime>)>);

impl Snapshot {
    /// Every file under `roots` (a root may itself be a file, or not exist).
    #[must_use]
    pub fn take(roots: &[PathBuf]) -> Snapshot {
        let mut files = Vec::new();
        for root in roots {
            if root.is_file() {
                files.push(root.clone());
            } else if let Ok(found) = find_all(root) {
                files.extend(found);
            }
        }
        files.sort();
        files.dedup();
        Snapshot(
            files
                .into_iter()
                .filter_map(|p| {
                    let meta = std::fs::metadata(&p).ok()?;
                    Some((p, meta.len(), meta.modified().ok()))
                })
                .collect(),
        )
    }

    /// The first path that differs between two pictures, for the message.
    #[must_use]
    pub fn first_difference<'a>(&'a self, other: &'a Snapshot) -> Option<&'a Path> {
        for (a, b) in self.0.iter().zip(&other.0) {
            if a != b {
                return Some(if a.0 <= b.0 { &a.0 } else { &b.0 });
            }
        }
        let longer = if self.0.len() > other.0.len() {
            self
        } else {
            other
        };
        longer
            .0
            .get(self.0.len().min(other.0.len()))
            .map(|e| e.0.as_path())
    }
}

/// The most recent mtime among `paths`, or `None` if there are none.
#[must_use]
pub fn newest_mtime(paths: &[PathBuf]) -> Option<SystemTime> {
    paths
        .iter()
        .filter_map(|p| std::fs::metadata(p).ok()?.modified().ok())
        .max()
}

/// The most recent mtime of any file under `dir`.
#[must_use]
pub fn newest_mtime_under(dir: &Path) -> Option<SystemTime> {
    newest_mtime(&find_all(dir).ok()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Tree {
        root: PathBuf,
    }

    impl Tree {
        fn new(name: &str) -> Tree {
            let root =
                std::env::temp_dir().join(format!("jrs-project-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap();
            Tree { root }
        }

        fn write(&self, relative: &str, contents: &str) -> PathBuf {
            let path = self.root.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, contents).unwrap();
            path
        }

        fn manifest(&self, extra: &str) -> Manifest {
            let text = format!("[project]\nname='app'\nversion='1.0.0'\n{extra}");
            Manifest::parse(&text, &self.root.join("jrs.toml"), &self.root).unwrap()
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn sources_are_found_recursively_and_sorted() {
        let tree = Tree::new("sources");
        tree.write("src/main/java/com/example/Main.java", "");
        tree.write("src/main/java/com/example/util/Helper.java", "");
        tree.write("src/main/java/Root.java", "");
        tree.write("src/main/java/notes.txt", "");

        let m = tree.manifest("");
        let sources = Project::new(&m).main_sources().unwrap();
        let names: Vec<String> = sources
            .files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["Root.java", "Main.java", "Helper.java"]);
    }

    #[test]
    fn a_project_with_no_tests_is_not_an_error() {
        let tree = Tree::new("no-tests");
        tree.write("src/main/java/Main.java", "");
        let m = tree.manifest("");
        assert!(Project::new(&m).test_sources().unwrap().is_empty());
    }

    fn names(sources: &Sources) -> Vec<String> {
        sources
            .files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn every_root_is_scanned_for_every_language() {
        let tree = Tree::new("roots");
        tree.write("src/main/java/com/example/Legacy.java", "");
        tree.write("src/main/java/com/example/Stray.kt", "");
        tree.write("src/main/kotlin/com/example/Main.kt", "");
        tree.write("src/main/kotlin/build.gradle.kts", "");
        tree.write("src/test/kotlin/com/example/MainTest.kt", "");

        let m = tree.manifest("[kotlin]\nversion = '2.4.20'\n");
        let p = Project::new(&m);
        assert_eq!(
            p.roots(Unit::Main),
            vec![
                tree.root.join("src/main/java"),
                tree.root.join("src/main/kotlin")
            ]
        );
        let main = p.main_sources().unwrap();
        assert_eq!(names(&main), ["Legacy.java", "Stray.kt", "Main.kt"]);
        assert_eq!(main.foreign(), Some(Language::Kotlin));
        assert_eq!(
            main.describe("source files"),
            "2 Kotlin + 1 Java source files"
        );
        let test = p.test_sources().unwrap();
        assert_eq!(names(&test), ["MainTest.kt"]);
        assert_eq!(test.describe("test sources"), "1 Kotlin test sources");
    }

    #[test]
    fn a_java_only_unit_reads_as_it_always_has() {
        let tree = Tree::new("java-describe");
        tree.write("src/main/java/A.java", "");
        tree.write("src/main/java/B.java", "");
        let m = tree.manifest("");
        let main = Project::new(&m).main_sources().unwrap();
        assert_eq!(main.foreign(), None);
        assert_eq!(main.describe("source files"), "2 source files");
    }

    #[test]
    fn sources_in_a_language_that_is_off_are_an_error_not_a_skip() {
        let tree = Tree::new("not-enabled");
        tree.write("src/main/java/Main.java", "");
        for name in ["A", "B", "C"] {
            tree.write(&format!("src/main/kotlin/{name}.kt"), "");
        }
        let m = tree.manifest("");
        let err = Project::new(&m).main_sources().unwrap_err();
        assert_eq!(err.exit_code(), 2);
        let err = err.to_string();
        assert!(err.contains("found 3 .kt files under"), "{err}");
        assert!(err.contains("src/main/kotlin"), "{err}");
        assert!(err.contains("has no [kotlin] table"), "{err}");
        assert!(err.contains("[kotlin]\n    version = \""), "{err}");

        // Found under the Java root too, not only in the default directory.
        let tree = Tree::new("not-enabled-java-root");
        tree.write("src/main/java/Script.groovy", "");
        let m = tree.manifest("");
        let err = Project::new(&m).main_sources().unwrap_err().to_string();
        assert!(err.contains("found 1 .groovy files under"), "{err}");
        assert!(err.contains("src/main/java"), "{err}");
    }

    #[test]
    fn a_unit_mixes_java_with_one_other_language_only() {
        let tree = Tree::new("two-languages");
        tree.write("src/main/kotlin/A.kt", "");
        tree.write("src/main/scala/B.scala", "");
        let m = tree.manifest("[kotlin]\nversion = '2.4.20'\n[scala]\nversion = '3.9.0'\n");
        let err = Project::new(&m).main_sources().unwrap_err().to_string();
        assert!(
            err.contains("the main sources mix Kotlin and Scala"),
            "{err}"
        );
        assert!(err.contains("1 .kt, 1 .scala"), "{err}");
    }

    #[test]
    fn kotlin_main_code_with_groovy_tests_is_fine() {
        let tree = Tree::new("kotlin-groovy");
        tree.write("src/main/kotlin/Main.kt", "");
        tree.write("src/test/groovy/MainSpec.groovy", "");
        let m = tree.manifest("[kotlin]\nversion = '2.4.20'\n[groovy]\nversion = '5.1.2'\n");
        let p = Project::new(&m);
        assert_eq!(p.main_sources().unwrap().foreign(), Some(Language::Kotlin));
        assert_eq!(p.test_sources().unwrap().foreign(), Some(Language::Groovy));
    }

    #[test]
    fn generated_directories_are_scanned_like_roots() {
        let tree = Tree::new("generated");
        tree.write("src/main/java/Main.java", "");
        tree.write("target/generated/Gen.java", "");
        tree.write("target/generated/Gen2.kt", "");
        let m = tree.manifest("[kotlin]\nversion = '2.4.20'\n");
        let sources = Project::new(&m)
            .sources(Unit::Main, &[tree.root.join("target/generated")])
            .unwrap();
        assert_eq!(names(&sources), ["Main.java", "Gen.java", "Gen2.kt"]);
    }

    #[test]
    fn the_layout_follows_the_manifest() {
        let tree = Tree::new("layout");
        let m = tree.manifest("target-dir='build'");
        let p = Project::new(&m);
        assert_eq!(p.target_dir(), tree.root.join("build"));
        assert_eq!(p.classes_dir(), tree.root.join("build/classes"));
        assert_eq!(p.test_classes_dir(), tree.root.join("build/test-classes"));
        assert_eq!(p.work_dir(), tree.root.join("build/.jrs"));
        assert_eq!(p.jar_path(), tree.root.join("build/app-1.0.0.jar"));
    }

    #[test]
    fn cleaning_removes_the_target_directory_and_nothing_else() {
        let tree = Tree::new("clean");
        tree.write("src/main/java/Main.java", "class Main {}");
        tree.write("target/classes/Main.class", "bytes");
        let m = tree.manifest("");
        let p = Project::new(&m);

        assert!(p.clean().unwrap());
        assert!(!p.target_dir().exists());
        assert!(tree.root.join("src/main/java/Main.java").exists());
        assert!(!p.clean().unwrap(), "cleaning twice is a no-op");
    }

    #[test]
    fn resources_are_copied_preserving_structure() {
        let tree = Tree::new("resources");
        tree.write("src/main/resources/app.properties", "k=v");
        tree.write("src/main/resources/i18n/messages.properties", "hello=hi");

        let from = tree.root.join("src/main/resources");
        let to = tree.root.join("target/classes");
        assert_eq!(copy_tree(&from, &to).unwrap(), 2);
        assert_eq!(
            std::fs::read_to_string(to.join("i18n/messages.properties")).unwrap(),
            "hello=hi"
        );
    }

    #[test]
    fn unchanged_resources_are_not_copied_again() {
        let tree = Tree::new("resource-skip");
        tree.write("res/app.properties", "k=v");
        let from = tree.root.join("res");
        let to = tree.root.join("out");

        assert_eq!(copy_tree(&from, &to).unwrap(), 1);
        assert_eq!(copy_tree(&from, &to).unwrap(), 0);

        // A changed file is copied again: the size differs.
        tree.write("res/app.properties", "k=v2");
        assert_eq!(copy_tree(&from, &to).unwrap(), 1);
    }

    #[test]
    fn a_deleted_resource_is_removed_from_the_output() {
        let tree = Tree::new("resource-prune");
        tree.write("res/keep.properties", "k=v");
        tree.write("res/nested/gone.properties", "x=y");
        // Something that did not come from the resource directory — a class, or
        // a processor's output — must survive the sync.
        tree.write("out/Main.class", "bytes");
        let (from, to) = (tree.root.join("res"), tree.root.join("out"));
        let record = tree.root.join(".jrs/resources.list");

        let first = sync_resources(&from, &to, &record).unwrap();
        assert_eq!(
            first,
            Synced {
                copied: 2,
                removed: 0
            }
        );

        std::fs::remove_file(from.join("nested/gone.properties")).unwrap();
        let second = sync_resources(&from, &to, &record).unwrap();
        assert_eq!(
            second,
            Synced {
                copied: 0,
                removed: 1
            }
        );
        assert!(!to.join("nested/gone.properties").exists());
        assert!(to.join("keep.properties").exists());
        assert!(to.join("Main.class").exists());

        // Removing the whole directory removes everything it contributed.
        std::fs::remove_dir_all(&from).unwrap();
        let third = sync_resources(&from, &to, &record).unwrap();
        assert_eq!(third.removed, 1);
        assert!(!to.join("keep.properties").exists());
        assert!(to.join("Main.class").exists());
        assert!(!record.exists());
    }

    #[test]
    fn a_resource_record_cannot_reach_outside_the_output() {
        let tree = Tree::new("resource-escape");
        let outside = tree.write("precious.txt", "keep me");
        let record = tree.write(".jrs/resources.list", "../precious.txt\n");
        std::fs::create_dir_all(tree.root.join("out")).unwrap();
        sync_resources(&tree.root.join("res"), &tree.root.join("out"), &record).unwrap();
        assert!(outside.exists());
    }

    #[test]
    fn a_missing_resource_directory_copies_nothing() {
        let tree = Tree::new("no-resources");
        assert_eq!(
            copy_tree(&tree.root.join("absent"), &tree.root.join("out")).unwrap(),
            0
        );
    }

    #[test]
    fn a_snapshot_changes_when_a_file_does() {
        let tree = Tree::new("snapshot");
        tree.write("src/A.java", "class A {}");
        let manifest = tree.write("jrs.toml", "[project]");
        let roots = vec![tree.root.join("src"), manifest, tree.root.join("absent")];

        let first = Snapshot::take(&roots);
        assert_eq!(first, Snapshot::take(&roots), "nothing changed");

        tree.write("src/B.java", "class B {}");
        let second = Snapshot::take(&roots);
        assert_ne!(first, second);
        assert!(
            first
                .first_difference(&second)
                .is_some_and(|p| p.ends_with("B.java") || p.ends_with("jrs.toml"))
        );

        tree.write("src/A.java", "class A { int changed; }");
        assert_ne!(second, Snapshot::take(&roots));
    }

    #[test]
    fn a_target_dir_pointing_at_the_root_is_refused() {
        let tree = Tree::new("dangerous-clean");
        tree.write("src/main/java/Main.java", "");
        let m = tree.manifest("target-dir='.'");
        let err = Project::new(&m).clean().unwrap_err().to_string();
        assert!(err.contains("refusing"), "{err}");
        assert!(tree.root.join("src/main/java/Main.java").exists());
    }
}
