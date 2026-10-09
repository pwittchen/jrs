//! Test impact analysis: which test classes a change can reach.
//!
//! After a test run, jrs records in `target/.jrs/<suite>.tested` every file
//! in the class directories on the test classpath — the main classes, the
//! test classes, and a suite's — with a hash of each. The next `jrs test`
//! hashes them again, and when what changed is classes alone, it runs only
//! the test classes whose constant pools reach a changed class, directly or
//! through other classes of the project's own. Gradle can only run a test
//! task whole or skip it.
//!
//! The rule is compile avoidance's: anything this cannot account for runs
//! the whole suite, since a skipped test that would have failed is a wrong
//! result, and a test run that was not needed costs only time.
//!
//! - no earlier run, or different settings: the JVM's flags and environment,
//!   the launcher, a jar on the classpath (a changed dependency);
//! - a resource that changed, or a class that was added or removed;
//! - a class that cannot be read, or a Groovy class, whose calls are
//!   dispatched at run time and leave no trace in the constant pool;
//! - a changed class that no test class reaches: whatever loads it, loads it
//!   by name — a component scan, a `ServiceLoader`, `Class.forName` — and
//!   which tests do that is not in the class files.
//!
//! And a test class whose reach includes something that finds classes by
//! itself — Spring's test context, Micronaut's, Quarkus's, Ktor's test host,
//! the `JUnit` suite engine, Cucumber, `ArchUnit`, `ServiceLoader` or
//! `java.lang.reflect` — runs on every change, reached or not.
//!
//! A run that fails leaves the classes that failed (or every class it ran,
//! when it cannot tell) to run again the next time, whatever changed.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use super::abi;
use crate::error::{IoResultExt, Result};
use crate::project;
use crate::resolve::repo::sha256_hex;

const HEADER: &str = "jrs test impact 1";

/// Class-name prefixes of what finds classes by itself. A test class that
/// reaches one of these runs whenever anything changed.
const DYNAMIC: &[&str] = &[
    "org/springframework/boot/test/",
    "org/springframework/test/",
    "io/micronaut/test/",
    "io/quarkus/test/",
    "io/ktor/server/testing/",
    "org/junit/platform/suite/",
    "io/cucumber/",
    "com/tngtech/archunit/",
    "org/reflections/",
    "io/github/classgraph/",
    "java/util/ServiceLoader",
    "java/lang/reflect/",
];

/// Which test classes to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection {
    /// Every one, for the reason given.
    All(String),
    /// Only these top-level test classes, by binary name and sorted. Empty
    /// when nothing changed that a test can see.
    Only(Vec<String>),
}

/// The class directories as they are now, to record once the run is over.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snapshot {
    settings: String,
    files: Vec<Entry>,
    /// Test classes that have to run next time, whatever changed.
    pending: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    path: PathBuf,
    size: u64,
    modified: u128,
    hash: String,
}

impl Snapshot {
    fn parse(text: &str) -> Option<Snapshot> {
        let mut lines = text.lines();
        if lines.next()? != HEADER {
            return None;
        }
        let settings = lines.next()?.strip_prefix("settings ")?.to_string();
        let mut snapshot = Snapshot {
            settings,
            ..Snapshot::default()
        };
        for line in lines {
            let (key, rest) = line.split_once(' ')?;
            match key {
                "pending" => {
                    snapshot.pending.insert(rest.to_string());
                }
                "file" => {
                    let mut fields = rest.splitn(4, ' ');
                    snapshot.files.push(Entry {
                        size: fields.next()?.parse().ok()?,
                        modified: fields.next()?.parse().ok()?,
                        hash: fields.next()?.to_string(),
                        path: PathBuf::from(fields.next()?),
                    });
                }
                _ => return None,
            }
        }
        Some(snapshot)
    }

    fn render(&self) -> String {
        let mut out = format!("{HEADER}\nsettings {}\n", self.settings);
        for class in &self.pending {
            let _ = writeln!(out, "pending {class}");
        }
        for file in &self.files {
            let _ = writeln!(
                out,
                "file {} {} {} {}",
                file.size,
                file.modified,
                file.hash,
                file.path.display()
            );
        }
        out
    }

    /// Record this snapshot as what the next run compares with, with
    /// `failed` — top-level test classes by binary name — to run again
    /// then whatever changes.
    ///
    /// # Errors
    ///
    /// [`crate::error::JrsError::Io`] if `state` cannot be written.
    pub fn record(&self, state: &Path, failed: impl IntoIterator<Item = String>) -> Result<()> {
        let snapshot = Snapshot {
            pending: failed.into_iter().collect(),
            ..self.clone()
        };
        if let Some(dir) = state.parent() {
            std::fs::create_dir_all(dir).path(dir)?;
        }
        std::fs::write(state, snapshot.render()).path(state)
    }
}

/// Drop what the last run recorded, so the next one runs every test.
pub fn forget(state: &Path) {
    let _ = std::fs::remove_file(state);
}

/// Work out which test classes to run. `state` is what the last run
/// recorded, `settings` everything besides the class directories that the
/// tests' outcome depends on, `class_dirs` the directories on the test
/// classpath and `scan_dir` the one among them holding the tests to run.
///
/// # Errors
///
/// [`crate::error::JrsError::Io`] if a class directory cannot be walked or
/// a file in it read.
pub fn select(
    state: &Path,
    settings: &str,
    class_dirs: &[PathBuf],
    scan_dir: &Path,
) -> Result<(Selection, Snapshot)> {
    let previous = std::fs::read_to_string(state)
        .ok()
        .and_then(|text| Snapshot::parse(&text));
    let settings = sha256_hex(settings.as_bytes());
    let known: HashMap<&Path, &Entry> = previous
        .iter()
        .flat_map(|p| &p.files)
        .map(|e| (e.path.as_path(), e))
        .collect();
    let mut files = Vec::new();
    for dir in class_dirs {
        for path in project::find_all(dir)? {
            let meta = std::fs::metadata(&path).path(&path)?;
            let size = meta.len();
            let modified = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos());
            let hash = match known.get(path.as_path()) {
                Some(e) if (e.size, e.modified) == (size, modified) => e.hash.clone(),
                _ => sha256_hex(&std::fs::read(&path).path(&path)?),
            };
            files.push(Entry {
                path,
                size,
                modified,
                hash,
            });
        }
    }
    let snapshot = Snapshot {
        settings,
        files,
        pending: BTreeSet::new(),
    };
    let selection = match &previous {
        None => Selection::All("there is no earlier run to compare with".to_string()),
        Some(previous) if previous.settings != snapshot.settings => {
            Selection::All("the test classpath or JVM settings changed".to_string())
        }
        Some(previous) => compare(previous, &snapshot, scan_dir)?,
    };
    Ok((selection, snapshot))
}

/// What changed from `previous` to `now`, and which test classes it reaches.
fn compare(previous: &Snapshot, now: &Snapshot, scan_dir: &Path) -> Result<Selection> {
    let before: HashMap<&Path, &str> = previous
        .files
        .iter()
        .map(|e| (e.path.as_path(), e.hash.as_str()))
        .collect();
    let mut changed_files = Vec::new();
    for entry in &now.files {
        match before.get(entry.path.as_path()) {
            None => return Ok(added_or_removed(&entry.path)),
            Some(hash) if *hash != entry.hash => changed_files.push(&entry.path),
            Some(_) => {}
        }
    }
    if before.len() != now.files.len() {
        return Ok(Selection::All(
            "a file was removed from the classes".to_string(),
        ));
    }
    if let Some(resource) = changed_files.iter().find(|p| !is_class(p)) {
        return Ok(Selection::All(format!(
            "a resource changed: {}",
            resource.display()
        )));
    }

    // Every class, read: its name, what it refers to, and whether it is a
    // test class to run.
    let mut classes: BTreeMap<String, abi::ClassInfo> = BTreeMap::new();
    let mut by_path: HashMap<&Path, String> = HashMap::new();
    let mut tests: BTreeSet<String> = BTreeSet::new();
    for entry in now.files.iter().filter(|e| is_class(&e.path)) {
        let bytes = std::fs::read(&entry.path).path(&entry.path)?;
        let Some(info) = abi::class_info(&bytes) else {
            return Ok(Selection::All(format!(
                "a class could not be read: {}",
                entry.path.display()
            )));
        };
        if info
            .source_file
            .as_deref()
            .is_some_and(|s| s.ends_with(".groovy"))
        {
            return Ok(Selection::All(
                "Groovy classes are called by name at run time".to_string(),
            ));
        }
        if entry.path.starts_with(scan_dir) {
            tests.insert(info.name.clone());
        }
        by_path.insert(entry.path.as_path(), info.name.clone());
        classes.insert(info.name.clone(), info);
    }
    let changed: BTreeSet<&str> = changed_files
        .iter()
        .filter_map(|p| by_path.get(p.as_path()).map(String::as_str))
        .collect();

    let mut users: HashMap<&str, Vec<&str>> = HashMap::new();
    for (name, info) in &classes {
        for target in &info.refs {
            if target != name && classes.contains_key(target) {
                users.entry(target.as_str()).or_default().push(name);
            }
        }
    }
    let mut reached: BTreeSet<&str> = BTreeSet::new();
    for &class in &changed {
        let from_here = walk(class, |c| users.get(c).cloned().unwrap_or_default());
        if !from_here.iter().any(|c| tests.contains(*c)) {
            return Ok(Selection::All(format!(
                "`{}` changed, and no test class refers to it",
                class.replace('/', ".")
            )));
        }
        reached.extend(from_here);
    }

    let mut selected: BTreeSet<String> = reached
        .iter()
        .filter(|c| tests.contains(**c))
        .map(|c| top_level(c))
        .collect();
    if !changed.is_empty() {
        selected.extend(finding_classes(&classes, &tests));
    }
    let present: HashSet<String> = tests.iter().map(|t| top_level(t)).collect();
    selected.extend(
        previous
            .pending
            .iter()
            .filter(|p| present.contains(*p))
            .cloned(),
    );
    Ok(Selection::Only(selected.into_iter().collect()))
}

/// The top-level test classes whose reach, through the project's classes,
/// includes one that refers to something in [`DYNAMIC`].
fn finding_classes(
    classes: &BTreeMap<String, abi::ClassInfo>,
    tests: &BTreeSet<String>,
) -> BTreeSet<String> {
    let finds = |class: &str| {
        classes[class]
            .refs
            .iter()
            .any(|r| DYNAMIC.iter().any(|d| r.starts_with(d)))
    };
    let mut out = BTreeSet::new();
    for test in tests {
        let top = top_level(test);
        if out.contains(&top) {
            continue;
        }
        let reach = walk(test, |c| {
            classes[c]
                .refs
                .iter()
                .filter(|r| classes.contains_key(r.as_str()))
                .map(String::as_str)
                .collect()
        });
        if reach.iter().any(|c| finds(c)) {
            out.insert(top);
        }
    }
    out
}

fn added_or_removed(path: &Path) -> Selection {
    if is_class(path) {
        Selection::All("a class was added or removed".to_string())
    } else {
        Selection::All(format!("a resource changed: {}", path.display()))
    }
}

fn is_class(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "class")
}

/// Every class reachable from `start` along `next`, `start` included.
fn walk<'a>(start: &'a str, next: impl Fn(&'a str) -> Vec<&'a str>) -> BTreeSet<&'a str> {
    let mut seen = BTreeSet::from([start]);
    let mut queue = vec![start];
    while let Some(class) = queue.pop() {
        for n in next(class) {
            if seen.insert(n) {
                queue.push(n);
            }
        }
    }
    seen
}

/// `com/example/FooTest$Inner` → `com.example.FooTest`: the class a scan
/// runs a nested test class with.
fn top_level(internal: &str) -> String {
    let (package, simple) = internal.rsplit_once('/').unwrap_or(("", internal));
    let simple = simple.split('$').next().unwrap_or(simple);
    if package.is_empty() {
        simple.to_string()
    } else {
        format!("{}.{simple}", package.replace('/', "."))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::toolchain::Toolchain;

    struct Tree {
        root: PathBuf,
    }

    impl Tree {
        fn new(name: &str) -> Tree {
            let root =
                std::env::temp_dir().join(format!("jrs-impact-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap();
            Tree { root }
        }

        fn write(&self, relative: &str, contents: &str) {
            let path = self.root.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, contents).unwrap();
        }

        /// Compile every source under `src/<unit>` into `<unit>`, against
        /// the main classes.
        fn javac(&self, unit: &str) -> bool {
            let Ok(toolchain) = Toolchain::discover() else {
                return false;
            };
            let sources =
                project::find_by_extension(&self.root.join("src").join(unit), "java").unwrap();
            let out = self.root.join(unit);
            let _ = std::fs::remove_dir_all(&out);
            let status = std::process::Command::new(&toolchain.javac)
                .arg("-d")
                .arg(&out)
                .arg("-cp")
                .arg(self.root.join("main"))
                .args(&sources)
                .status()
                .unwrap();
            assert!(status.success());
            true
        }

        fn dirs(&self) -> Vec<PathBuf> {
            vec![self.root.join("test"), self.root.join("main")]
        }

        fn select(&self, state: &Path) -> (Selection, Snapshot) {
            select(state, "settings", &self.dirs(), &self.root.join("test")).unwrap()
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn only(classes: &[&str]) -> Selection {
        Selection::Only(classes.iter().map(|c| (*c).to_string()).collect())
    }

    /// `Calc` used by `CalcTest`, `Fmt` by `FmtTest`, and `Report` using
    /// `Calc` by `ReportTest`.
    fn project(tree: &Tree) -> bool {
        tree.write(
            "src/main/p/Calc.java",
            "package p; public class Calc { public int add(int a, int b) { return a + b; } }",
        );
        tree.write(
            "src/main/p/Fmt.java",
            "package p; public class Fmt { public String f(int a) { return \"\" + a; } }",
        );
        tree.write("src/main/p/Report.java", "package p; public class Report { public int total() { return new Calc().add(1, 2); } }");
        tree.write("src/test/p/CalcTest.java", "package p; public class CalcTest { void t() { new Calc().add(1, 1); } class Inner {} }");
        tree.write(
            "src/test/p/FmtTest.java",
            "package p; public class FmtTest { void t() { new Fmt().f(1); } }",
        );
        tree.write(
            "src/test/p/ReportTest.java",
            "package p; public class ReportTest { void t() { new Report().total(); } }",
        );
        tree.javac("main") && tree.javac("test")
    }

    #[test]
    fn a_change_runs_the_tests_that_reach_it() {
        let tree = Tree::new("reach");
        if !project(&tree) {
            return;
        }
        let state = tree.root.join("state");
        let (selection, snapshot) = tree.select(&state);
        assert!(matches!(selection, Selection::All(_)), "no earlier run");
        snapshot.record(&state, []).unwrap();
        assert_eq!(tree.select(&state).0, only(&[]), "nothing changed");

        tree.write(
            "src/main/p/Calc.java",
            "package p; public class Calc { public int add(int a, int b) { return b + a; } }",
        );
        tree.javac("main");
        assert_eq!(
            tree.select(&state).0,
            only(&["p.CalcTest", "p.ReportTest"]),
            "Report reaches Calc"
        );

        // A recorded failure runs again, whatever changes.
        let (_, snapshot) = tree.select(&state);
        snapshot.record(&state, ["p.FmtTest".to_string()]).unwrap();
        assert_eq!(tree.select(&state).0, only(&["p.FmtTest"]));
    }

    #[test]
    fn what_the_class_files_cannot_account_for_runs_everything() {
        let tree = Tree::new("everything");
        if !project(&tree) {
            return;
        }
        let state = tree.root.join("state");
        let record = || tree.select(&state).1.record(&state, []).unwrap();
        record();

        let other = select(
            &state,
            "other settings",
            &tree.dirs(),
            &tree.root.join("test"),
        );
        assert!(matches!(other.unwrap().0, Selection::All(_)));

        tree.write("main/app.properties", "a=1");
        assert!(matches!(tree.select(&state).0, Selection::All(r) if r.contains("resource")));
        record();
        tree.write("main/app.properties", "a=2");
        assert!(matches!(tree.select(&state).0, Selection::All(r) if r.contains("resource")));
        record();

        // A class only reflection can reach.
        tree.write(
            "src/main/p/Plugin.java",
            "package p; public class Plugin {}",
        );
        tree.javac("main");
        assert!(matches!(tree.select(&state).0, Selection::All(r) if r.contains("added")));
        record();
        tree.write(
            "src/main/p/Plugin.java",
            "package p; public class Plugin { int x; }",
        );
        tree.javac("main");
        assert!(
            matches!(tree.select(&state).0, Selection::All(r) if r.contains("`p.Plugin` changed"))
        );
    }

    #[test]
    fn a_test_that_finds_classes_itself_runs_on_every_change() {
        let tree = Tree::new("dynamic");
        if !project(&tree) {
            return;
        }
        tree.write(
            "src/test/p/LoaderTest.java",
            "package p; public class LoaderTest { void t() { java.util.ServiceLoader.load(Runnable.class); } }",
        );
        tree.javac("test");
        let state = tree.root.join("state");
        tree.select(&state).1.record(&state, []).unwrap();
        tree.write(
            "src/main/p/Fmt.java",
            "package p; public class Fmt { public String f(int a) { return \"x\" + a; } }",
        );
        tree.javac("main");
        assert_eq!(tree.select(&state).0, only(&["p.FmtTest", "p.LoaderTest"]));
    }

    #[test]
    fn a_snapshot_reads_back_what_it_wrote() {
        let snapshot = Snapshot {
            settings: "abc".into(),
            files: vec![Entry {
                path: PathBuf::from("/t/classes/a b/C.class"),
                size: 3,
                modified: 7,
                hash: "ff".into(),
            }],
            pending: BTreeSet::from(["p.ATest".to_string()]),
        };
        assert_eq!(Snapshot::parse(&snapshot.render()), Some(snapshot));
        assert_eq!(Snapshot::parse("something else"), None);
    }

    #[test]
    fn nested_classes_run_with_their_top_level_class() {
        assert_eq!(top_level("p/q/FooTest$Inner$Deeper"), "p.q.FooTest");
        assert_eq!(top_level("FooTest"), "FooTest");
    }
}
