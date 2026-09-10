//! Layout discovery, source globbing, and the target directory.
//!
//! `target/` is fully disposable: nothing here writes anything into it that
//! could not be regenerated, so `jrs clean` can never lose user data (SPEC §3).

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::error::{IoResultExt, JrsError, Result};
use crate::manifest::Manifest;

/// The directories a build reads from and writes to.
pub struct Project<'a> {
    pub manifest: &'a Manifest,
}

impl<'a> Project<'a> {
    pub fn new(manifest: &'a Manifest) -> Project<'a> {
        Project { manifest }
    }

    pub fn target_dir(&self) -> PathBuf {
        self.manifest.target_path()
    }

    pub fn classes_dir(&self) -> PathBuf {
        self.target_dir().join("classes")
    }

    pub fn test_classes_dir(&self) -> PathBuf {
        self.target_dir().join("test-classes")
    }

    /// jrs's own scratch space: argfiles, fingerprints, fat-jar staging.
    pub fn work_dir(&self) -> PathBuf {
        self.target_dir().join(".jrs")
    }

    pub fn jar_path(&self) -> PathBuf {
        self.target_dir().join(self.manifest.jar_name())
    }

    pub fn main_sources(&self) -> Result<Vec<PathBuf>> {
        find_by_extension(&self.manifest.source_path(), "java")
    }

    pub fn test_sources(&self) -> Result<Vec<PathBuf>> {
        find_by_extension(&self.manifest.test_path(), "java")
    }

    /// Remove `target/`. Returns whether there was anything to remove.
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

/// The most recent mtime among `paths`, or `None` if there are none.
pub fn newest_mtime(paths: &[PathBuf]) -> Option<SystemTime> {
    paths
        .iter()
        .filter_map(|p| std::fs::metadata(p).ok()?.modified().ok())
        .max()
}

/// The most recent mtime of any file under `dir`.
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
    fn a_target_dir_pointing_at_the_root_is_refused() {
        let tree = Tree::new("dangerous-clean");
        tree.write("src/main/java/Main.java", "");
        let m = tree.manifest("target-dir='.'");
        let err = Project::new(&m).clean().unwrap_err().to_string();
        assert!(err.contains("refusing"), "{err}");
        assert!(tree.root.join("src/main/java/Main.java").exists());
    }
}
