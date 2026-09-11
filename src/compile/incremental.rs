//! Recompiling a Java unit file by file (SPEC §7.2).
//!
//! A unit whose settings have not changed since its last build does not need
//! every source compiled again when a few of them changed. After each build
//! jrs records, in `target/.jrs/<unit>.index`, what every source compiled
//! to: its classes, the API of each as `abi.rs` reads it, and which of the
//! unit's other classes they refer to. The next build compiles the changed
//! sources alone, against the classes already in the output directory, and
//! compares their API with the recorded one. A change that stops at method
//! bodies ends there. One that reaches the API recompiles every source that
//! refers to a changed class, and every source that refers to one of
//! those, transitively — through a subclass, say, whose own API did not
//! change but whose inherited members did.
//!
//! Anything this cannot account for compiles the whole unit, as before:
//!
//! - different settings: flags, a jar, the main classes' API for the tests;
//! - a source added or deleted, and a top-level class that appeared or went
//!   away: a simple name elsewhere may now mean another class, and nothing in
//!   that source's class files says it used the name;
//! - a changed compile-time constant, which `javac` copies into its readers
//!   without a reference back;
//! - annotation processors and `javac` plugins, which read and write more
//!   than the sources they are given; a `module-info.java`; a Kotlin, Scala
//!   or Groovy step;
//! - a class the index cannot tie to exactly one source by its `SourceFile`
//!   attribute (`-g:none`, a generated class).
//!
//! As with compile avoidance, the rule is to err towards recompiling: an
//! extra `javac` run costs seconds, a stale class costs a wrong result.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::ffi::OsStr;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, UNIX_EPOCH};

use super::abi::{self, Kind};
use super::{CompileUnit, count, javac};
use crate::error::{IoResultExt, Result};
use crate::project;
use crate::resolve::repo::sha256_hex;
use crate::toolchain::Toolchain;
use crate::ui::Ui;

const HEADER: &str = "jrs compile index 1";

/// The service file through which `javac` finds annotation processors on
/// its classpath.
const PROCESSORS: &str = "META-INF/services/javax.annotation.processing.Processor";

/// What the last successful build compiled, source by source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Index {
    /// A digest of [`CompileUnit::settings`].
    settings: String,
    /// In the unit's source order.
    sources: Vec<Source>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Source {
    path: PathBuf,
    size: u64,
    modified: u128,
    /// SHA-256 of the contents. Empty when the source has to be compiled
    /// again whatever it holds, because the build that tried last failed.
    hash: String,
    classes: Vec<Class>,
    /// The unit's classes, other than its own, that this source's refer to.
    deps: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Class {
    /// `com/example/Main$Inner`.
    name: String,
    kind: Kind,
    api: String,
    constants: String,
}

fn index_path(unit: &CompileUnit) -> PathBuf {
    unit.work_dir.join(format!("{}.index", unit.label))
}

/// Drop the index, so the next build compiles everything.
pub(super) fn forget(unit: &CompileUnit) {
    let _ = std::fs::remove_file(index_path(unit));
}

impl Index {
    /// The unit's index, if it has a readable one.
    pub(super) fn load(unit: &CompileUnit) -> Option<Index> {
        Index::parse(&std::fs::read_to_string(index_path(unit)).ok()?)
    }

    fn parse(text: &str) -> Option<Index> {
        let mut lines = text.lines();
        if lines.next()? != HEADER {
            return None;
        }
        let settings = lines.next()?.strip_prefix("settings ")?.to_string();
        let mut sources: Vec<Source> = Vec::new();
        for line in lines {
            let (key, rest) = line.split_once(' ')?;
            let mut fields = rest.splitn(4, ' ');
            match key {
                "source" => {
                    let size = fields.next()?.parse().ok()?;
                    let modified = fields.next()?.parse().ok()?;
                    let hash = fields.next()?;
                    sources.push(Source {
                        size,
                        modified,
                        hash: if hash == "-" { "" } else { hash }.to_string(),
                        path: PathBuf::from(fields.next()?),
                        classes: Vec::new(),
                        deps: BTreeSet::new(),
                    });
                }
                "class" => {
                    let kind = match fields.next()? {
                        "top" => Kind::TopLevel,
                        "nested" => Kind::Nested,
                        "hidden" => Kind::Hidden,
                        _ => return None,
                    };
                    let class = Class {
                        kind,
                        api: fields.next()?.to_string(),
                        constants: fields.next()?.to_string(),
                        name: fields.next()?.to_string(),
                    };
                    sources.last_mut()?.classes.push(class);
                }
                "dep" => {
                    sources.last_mut()?.deps.insert(rest.to_string());
                }
                _ => return None,
            }
        }
        Some(Index { settings, sources })
    }

    fn render(&self) -> String {
        let mut out = format!("{HEADER}\nsettings {}\n", self.settings);
        for source in &self.sources {
            let hash = if source.hash.is_empty() {
                "-"
            } else {
                &source.hash
            };
            let _ = writeln!(
                out,
                "source {} {} {hash} {}",
                source.size,
                source.modified,
                source.path.display()
            );
            for class in &source.classes {
                let kind = match class.kind {
                    Kind::TopLevel => "top",
                    Kind::Nested => "nested",
                    Kind::Hidden => "hidden",
                };
                let _ = writeln!(
                    out,
                    "class {kind} {} {} {}",
                    class.api, class.constants, class.name
                );
            }
            for dep in &source.deps {
                let _ = writeln!(out, "dep {dep}");
            }
        }
        out
    }

    fn save(&self, unit: &CompileUnit) -> Result<()> {
        let path = index_path(unit);
        std::fs::write(&path, self.render()).path(&path)
    }

    fn by_path(&self) -> HashMap<&Path, &Source> {
        self.sources.iter().map(|s| (s.path.as_path(), s)).collect()
    }

    /// Whether `sources` are the sources this index recorded, with the same
    /// contents. A source whose size or mtime moved is read and compared
    /// by its hash, so one touched but not changed is still the same.
    pub(super) fn holds(&self, sources: &[PathBuf]) -> bool {
        if self.sources.len() != sources.len() {
            return false;
        }
        let known = self.by_path();
        sources.iter().all(|path| {
            let Some(source) = known.get(path.as_path()) else {
                return false;
            };
            if source.hash.is_empty() {
                return false;
            }
            stat(path) == Some((source.size, source.modified))
                || std::fs::read(path).is_ok_and(|bytes| sha256_hex(&bytes) == source.hash)
        })
    }
}

fn stat(path: &Path) -> Option<(u64, u128)> {
    let meta = std::fs::metadata(path).ok()?;
    let modified = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
    Some((meta.len(), modified.as_nanos()))
}

/// Whether the unit may be compiled file by file at all.
fn allowed(unit: &CompileUnit) -> bool {
    if unit.foreign_step().is_some()
        || unit
            .sources
            .iter()
            .any(|s| s.file_name() == Some(OsStr::new("module-info.java")))
    {
        return false;
    }
    let args = &unit.extra_args;
    if args.iter().any(|a| {
        a.starts_with("-processor")
            || a.starts_with("--processor")
            || a.starts_with("-Xplugin")
            || a == "-proc:only"
    }) {
        return false;
    }
    // A processor registered on the classpath runs unasked on JDKs before
    // 23, and on any JDK under `-proc:full`. The output directory counts: it
    // is on the classpath when only some sources are compiled.
    args.iter().any(|a| a == "-proc:none")
        || !unit
            .classpath
            .iter()
            .chain([&unit.output_dir])
            .any(|entry| registers_processor(entry))
}

fn registers_processor(entry: &Path) -> bool {
    if entry.is_dir() {
        return entry.join(PROCESSORS).is_file();
    }
    std::fs::File::open(entry)
        .ok()
        .and_then(|file| zip::ZipArchive::new(file).ok())
        .is_some_and(|mut jar| jar.by_name(PROCESSORS).is_ok())
}

/// Where the class `name` lives under `dir`.
fn class_path(dir: &Path, name: &str) -> PathBuf {
    let mut path = dir.to_path_buf();
    for part in name.split('/') {
        path.push(part);
    }
    path.set_extension("class");
    path
}

/// A source as it is now, taken before the compiler runs, so that a file
/// saved during the build is seen as changed by the next one.
#[derive(Debug)]
struct Current {
    path: PathBuf,
    size: u64,
    modified: u128,
    hash: String,
}

/// One compile of a unit that may go file by file.
pub(super) struct Tracker {
    settings: String,
    current: Vec<Current>,
    /// The last build's index, when it was taken under these settings.
    previous: Option<Index>,
}

impl Tracker {
    /// `None` when the unit is always compiled whole; its index, if it had
    /// one, is dropped.
    ///
    /// # Errors
    ///
    /// `JrsError::Io` if a source cannot be read.
    pub(super) fn new(unit: &CompileUnit) -> Result<Option<Tracker>> {
        if !allowed(unit) {
            forget(unit);
            return Ok(None);
        }
        let settings = sha256_hex(unit.settings().as_bytes());
        let previous = Index::load(unit)
            .filter(|index| index.settings == settings && unit.output_dir.is_dir());
        let known = previous.as_ref().map(Index::by_path).unwrap_or_default();
        let mut current = Vec::with_capacity(unit.sources.len());
        for path in &unit.sources {
            let (size, modified) = stat(path).unwrap_or_default();
            let hash = match known.get(path.as_path()) {
                Some(s) if !s.hash.is_empty() && (s.size, s.modified) == (size, modified) => {
                    s.hash.clone()
                }
                _ => sha256_hex(&std::fs::read(path).path(path)?),
            };
            current.push(Current {
                path: path.clone(),
                size,
                modified,
                hash,
            });
        }
        Ok(Some(Tracker {
            settings,
            current,
            previous,
        }))
    }

    /// Compile what changed and what depends on it. `Some(n)` when that
    /// was enough, with `n` the sources compiled; `None` when the unit has
    /// to be compiled whole, which may be found out halfway, after a first
    /// `javac` run.
    ///
    /// # Errors
    ///
    /// `JrsError::Build` if `javac` fails, after recording that every
    /// source it was given has to be compiled again; `JrsError::Io` if the
    /// output directory or the index cannot be read or written.
    pub(super) fn recompile(
        &self,
        toolchain: &Toolchain,
        unit: &CompileUnit,
        ui: &Ui,
        steps: &mut Vec<(&'static str, Duration)>,
    ) -> Result<Option<usize>> {
        let Some(previous) = &self.previous else {
            return Ok(None);
        };
        // A new or deleted source brings or takes away a top-level class.
        if previous.sources.len() != self.current.len()
            || previous
                .sources
                .iter()
                .zip(&self.current)
                .any(|(p, c)| p.path != c.path)
        {
            return Ok(None);
        }
        let changed: BTreeSet<usize> = (0..self.current.len())
            .filter(|&i| previous.sources[i].hash != self.current[i].hash)
            .collect();
        if changed.is_empty() {
            // Touched, not changed: remember the new mtimes.
            let mut index = previous.clone();
            for (source, now) in index.sources.iter_mut().zip(&self.current) {
                (source.size, source.modified) = (now.size, now.modified);
            }
            index.save(unit)?;
            return Ok(Some(0));
        }

        // The changed sources lose their old classes, and so does anything a
        // failed build left behind; the rest stay, to be compiled against.
        let mut compiled = changed.clone();
        sweep(unit, previous, &compiled)?;
        let first: Vec<usize> = changed.iter().copied().collect();
        self.javac(toolchain, unit, ui, steps, &first, previous, &compiled)?;
        let Some(after) = self.record(unit)? else {
            return Ok(None);
        };
        let mut api = BTreeSet::new();
        for &i in &first {
            if !compare(
                &previous.sources[i].classes,
                &after.sources[i].classes,
                &mut api,
            ) {
                return Ok(None);
            }
        }

        let dependents = dependents(&after, &api, &compiled);
        if dependents.is_empty() {
            after.save(unit)?;
            return Ok(Some(compiled.len()));
        }
        for &i in &dependents {
            for class in &after.sources[i].classes {
                let path = class_path(&unit.output_dir, &class.name);
                match std::fs::remove_file(&path) {
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    result => result.path(&path)?,
                }
            }
        }
        compiled.extend(&dependents);
        self.javac(toolchain, unit, ui, steps, &dependents, previous, &compiled)?;
        let Some(after) = self.record(unit)? else {
            return Ok(None);
        };
        after.save(unit)?;
        Ok(Some(compiled.len()))
    }

    /// After the whole unit compiled: index it, or drop the index if its
    /// classes cannot be tied to their sources.
    ///
    /// # Errors
    ///
    /// `JrsError::Io` if the classes cannot be read or the index written.
    pub(super) fn record_all(&self, unit: &CompileUnit) -> Result<()> {
        match self.record(unit)? {
            Some(index) => index.save(unit),
            None => {
                forget(unit);
                Ok(())
            }
        }
    }

    /// One `javac` run over some of the unit's sources, with the output
    /// directory first on its classpath. If it fails, the index is left
    /// saying that every source in `compiled` — this run's and an earlier
    /// run's in the same build — has to be compiled again, since the output
    /// directory no longer holds what the index says it does.
    #[allow(clippy::too_many_arguments)]
    fn javac(
        &self,
        toolchain: &Toolchain,
        unit: &CompileUnit,
        ui: &Ui,
        steps: &mut Vec<(&'static str, Duration)>,
        sources: &[usize],
        previous: &Index,
        compiled: &BTreeSet<usize>,
    ) -> Result<()> {
        let paths: Vec<PathBuf> = sources
            .iter()
            .map(|&i| self.current[i].path.clone())
            .collect();
        let started = Instant::now();
        let result = javac::run(toolchain, unit, &paths, true, &count(paths.len(), None), ui);
        steps.push(("javac", started.elapsed()));
        if result.is_err() {
            let mut failed = previous.clone();
            for &i in compiled {
                failed.sources[i].hash.clear();
            }
            if failed.save(unit).is_err() {
                forget(unit);
            }
            let _ = std::fs::remove_file(unit.fingerprint_path());
        }
        result
    }

    /// Index what the output directory holds now. `None` if a class cannot
    /// be read, or cannot be tied to exactly one source: by its `SourceFile`
    /// name, and when two sources share that name, by its package too.
    fn record(&self, unit: &CompileUnit) -> Result<Option<Index>> {
        let mut named: HashMap<&OsStr, Vec<usize>> = HashMap::new();
        for (i, source) in self.current.iter().enumerate() {
            if let Some(name) = source.path.file_name() {
                named.entry(name).or_default().push(i);
            }
        }
        let mut classes: Vec<Vec<Class>> = vec![Vec::new(); self.current.len()];
        let mut refs: Vec<BTreeSet<String>> = vec![BTreeSet::new(); self.current.len()];
        let mut unit_classes = HashSet::new();
        for file in project::find_by_extension(&unit.output_dir, "class")? {
            let bytes = std::fs::read(&file).path(&file)?;
            let Some(info) = abi::class_info(&bytes) else {
                return Ok(None);
            };
            // A name that does not map back to its own file (a lossy
            // non-UTF-8 name) could not be found again to delete.
            if class_path(&unit.output_dir, &info.name) != file {
                return Ok(None);
            }
            let Some(source_file) = &info.source_file else {
                return Ok(None);
            };
            let candidates = named
                .get(OsStr::new(source_file))
                .map_or(&[][..], Vec::as_slice);
            let owner = match candidates {
                [one] => *one,
                _ => {
                    let package = info.name.rsplit_once('/').map_or("", |(p, _)| p);
                    let expected = Path::new(package).join(source_file);
                    let matching: Vec<usize> = candidates
                        .iter()
                        .copied()
                        .filter(|&i| self.current[i].path.ends_with(&expected))
                        .collect();
                    match matching[..] {
                        [one] => one,
                        _ => return Ok(None),
                    }
                }
            };
            unit_classes.insert(info.name.clone());
            refs[owner].extend(info.refs);
            classes[owner].push(Class {
                name: info.name,
                kind: info.kind,
                api: info.api,
                constants: info.constants,
            });
        }

        let sources = self
            .current
            .iter()
            .zip(classes)
            .zip(refs)
            .map(|((now, classes), refs)| {
                let own: HashSet<&str> = classes.iter().map(|c| c.name.as_str()).collect();
                let deps = refs
                    .into_iter()
                    .filter(|r| unit_classes.contains(r) && !own.contains(r.as_str()))
                    .collect();
                Source {
                    path: now.path.clone(),
                    size: now.size,
                    modified: now.modified,
                    hash: now.hash.clone(),
                    classes,
                    deps,
                }
            })
            .collect();
        Ok(Some(Index {
            settings: self.settings.clone(),
            sources,
        }))
    }
}

/// Delete every class in the output directory that a source outside
/// `dirty` does not own: the old classes of the sources about to be
/// compiled, and whatever a failed build left behind.
fn sweep(unit: &CompileUnit, index: &Index, dirty: &BTreeSet<usize>) -> Result<()> {
    let keep: HashSet<PathBuf> = index
        .sources
        .iter()
        .enumerate()
        .filter(|(i, _)| !dirty.contains(i))
        .flat_map(|(_, source)| &source.classes)
        .map(|class| class_path(&unit.output_dir, &class.name))
        .collect();
    for file in project::find_by_extension(&unit.output_dir, "class")? {
        if !keep.contains(&file) {
            std::fs::remove_file(&file).path(&file)?;
        }
    }
    Ok(())
}

/// Compare what a recompiled source compiles to with what it compiled to
/// before, adding to `api` every class whose API other sources may have
/// compiled against differently. `false` when only compiling the whole unit
/// is safe: a top-level class appeared or went away, or a constant changed.
fn compare(old: &[Class], new: &[Class], api: &mut BTreeSet<String>) -> bool {
    for before in old {
        match new.iter().find(|c| c.name == before.name) {
            Some(after) => {
                if after.constants != before.constants
                    || (after.kind != before.kind
                        && Kind::TopLevel == after.kind.max_visibility(before.kind))
                {
                    return false;
                }
                if after.api != before.api || after.kind != before.kind {
                    api.insert(before.name.clone());
                }
            }
            None => {
                if before.kind == Kind::TopLevel || before.constants != "-" {
                    return false;
                }
                if before.kind != Kind::Hidden {
                    api.insert(before.name.clone());
                }
            }
        }
    }
    for after in new {
        if old.iter().any(|c| c.name == after.name) {
            continue;
        }
        if after.kind == Kind::TopLevel {
            return false;
        }
        if after.kind != Kind::Hidden {
            api.insert(after.name.clone());
        }
    }
    true
}

impl Kind {
    /// `TopLevel` if either is.
    fn max_visibility(self, other: Kind) -> Kind {
        if self == Kind::TopLevel || other == Kind::TopLevel {
            Kind::TopLevel
        } else {
            self
        }
    }
}

/// The sources that refer to a class in `changed`, and those that refer to
/// one of theirs, transitively — less the ones in `compiled`, which were
/// compiled against the new classes already. The walk goes through
/// `compiled` too: a subclass compiled in the same run still passes an
/// inherited change on to its own users.
fn dependents(index: &Index, changed: &BTreeSet<String>, compiled: &BTreeSet<usize>) -> Vec<usize> {
    let mut users: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, source) in index.sources.iter().enumerate() {
        for dep in &source.deps {
            users.entry(dep.as_str()).or_default().push(i);
        }
    }
    let mut seen: HashSet<&str> = changed.iter().map(String::as_str).collect();
    let mut queue: Vec<&str> = seen.iter().copied().collect();
    let mut reached = BTreeSet::new();
    while let Some(name) = queue.pop() {
        for &i in users.get(name).map_or(&[][..], Vec::as_slice) {
            if !reached.insert(i) {
                continue;
            }
            for class in &index.sources[i].classes {
                if seen.insert(&class.name) {
                    queue.push(&class.name);
                }
            }
        }
    }
    reached.difference(compiled).copied().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn class(name: &str, kind: Kind, api: &str, constants: &str) -> Class {
        Class {
            name: name.into(),
            kind,
            api: api.into(),
            constants: constants.into(),
        }
    }

    fn source(path: &str, classes: Vec<Class>, deps: &[&str]) -> Source {
        Source {
            path: PathBuf::from(path),
            size: 10,
            modified: 20,
            hash: "abc".into(),
            classes,
            deps: deps.iter().map(|d| (*d).to_string()).collect(),
        }
    }

    fn top(name: &str) -> Class {
        class(name, Kind::TopLevel, name, "-")
    }

    #[test]
    fn an_index_reads_back_what_it_wrote() {
        let mut failed = source("/src/b dir/B.java", vec![top("p/B")], &[]);
        failed.hash.clear();
        let index = Index {
            settings: "s".into(),
            sources: vec![
                source(
                    "/src/A.java",
                    vec![
                        class("p/A", Kind::TopLevel, "a1", "c1"),
                        class("p/A$In", Kind::Nested, "a2", "-"),
                        class("p/A$1", Kind::Hidden, "-", "-"),
                    ],
                    &["p/B"],
                ),
                failed,
            ],
        };
        assert_eq!(Index::parse(&index.render()), Some(index));
        assert_eq!(Index::parse("jrs compile index 0\nsettings s\n"), None);
        assert_eq!(
            Index::parse(&format!("{HEADER}\nsettings s\nclass top a c p/A\n")),
            None
        );
    }

    #[test]
    fn a_body_change_changes_no_api() {
        let old = [top("p/A"), class("p/A$1", Kind::Hidden, "-", "-")];
        let new = [top("p/A"), class("p/A$2", Kind::Hidden, "-", "-")];
        let mut api = BTreeSet::new();
        assert!(compare(&old, &new, &mut api));
        assert!(api.is_empty(), "{api:?}");
    }

    #[test]
    fn a_new_member_or_nested_class_is_a_changed_api() {
        let mut api = BTreeSet::new();
        assert!(compare(
            &[top("p/A")],
            &[
                class("p/A", Kind::TopLevel, "other", "-"),
                class("p/A$In", Kind::Nested, "x", "-"),
            ],
            &mut api
        ));
        assert_eq!(
            api.into_iter().collect::<Vec<_>>(),
            ["p/A".to_string(), "p/A$In".to_string()]
        );
    }

    #[test]
    fn constants_and_top_level_classes_compile_the_whole_unit() {
        let mut api = BTreeSet::new();
        let with = |c: &str| class("p/A", Kind::TopLevel, "a", c);
        assert!(!compare(&[with("1")], &[with("2")], &mut api), "a constant");
        assert!(
            !compare(&[top("p/A")], &[top("p/A"), top("p/Extra")], &mut api),
            "a new top-level class"
        );
        assert!(
            !compare(&[top("p/A"), top("p/Gone")], &[top("p/A")], &mut api),
            "a vanished top-level class"
        );
        assert!(
            !compare(
                &[top("p/A"), class("p/A$K", Kind::Nested, "k", "1")],
                &[top("p/A")],
                &mut api
            ),
            "a vanished nested class with constants"
        );
    }

    #[test]
    fn dependents_are_found_transitively_through_what_was_compiled() {
        // C calls a method B inherits from A; A changed. B was compiled in
        // the first run, but C only names B, and still has to be compiled.
        let index = Index {
            settings: "s".into(),
            sources: vec![
                source("A.java", vec![top("p/A")], &[]),
                source("B.java", vec![top("p/B")], &["p/A"]),
                source("C.java", vec![top("p/C")], &["p/B"]),
                source("D.java", vec![top("p/D")], &[]),
            ],
        };
        let changed = BTreeSet::from(["p/A".to_string()]);
        assert_eq!(
            dependents(&index, &changed, &BTreeSet::from([0, 1])),
            vec![2]
        );
        assert_eq!(
            dependents(&index, &changed, &BTreeSet::from([0])),
            vec![1, 2]
        );
    }

    struct Tree(PathBuf);

    impl Tree {
        fn new(name: &str) -> Tree {
            let root =
                std::env::temp_dir().join(format!("jrs-incremental-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap();
            Tree(root)
        }

        fn write(&self, relative: &str, contents: &[u8]) -> PathBuf {
            let path = self.0.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, contents).unwrap();
            path
        }

        fn unit(&self, sources: Vec<PathBuf>) -> CompileUnit {
            CompileUnit {
                label: "main".into(),
                sources,
                output_dir: self.0.join("target/classes"),
                classpath: Vec::new(),
                release: 21,
                target: None,
                encoding: "UTF-8".into(),
                extra_args: Vec::new(),
                work_dir: self.0.join("target/.jrs"),
                foreign: None,
                main_api: None,
            }
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn processors_plugins_and_modules_keep_the_unit_whole() {
        let tree = Tree::new("allowed");
        let main = tree.write("src/p/Main.java", b"");
        let mut unit = tree.unit(vec![main.clone()]);
        assert!(allowed(&unit));

        unit.extra_args = vec!["-processorpath".into(), "x.jar".into()];
        assert!(!allowed(&unit));
        unit.extra_args = vec!["-Xplugin:ErrorProne".into()];
        assert!(!allowed(&unit));
        unit.extra_args.clear();

        let jar = tree.0.join("processor.jar");
        let mut writer = zip::ZipWriter::new(std::fs::File::create(&jar).unwrap());
        writer
            .start_file(PROCESSORS, zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.finish().unwrap();
        unit.classpath = vec![jar];
        assert!(!allowed(&unit), "a processor on the classpath");
        unit.extra_args = vec!["-proc:none".into()];
        assert!(allowed(&unit), "turned off");
        unit.extra_args.clear();
        unit.classpath.clear();

        tree.write(&format!("target/classes/{PROCESSORS}"), b"p.Gen\n");
        assert!(!allowed(&unit), "the unit's own processor, as a resource");
        std::fs::remove_dir_all(tree.0.join("target")).unwrap();

        let module = tree.write("src/module-info.java", b"");
        assert!(!allowed(&tree.unit(vec![module, main])));
    }

    #[test]
    fn a_touched_source_with_the_same_contents_is_held() {
        let tree = Tree::new("holds");
        let path = tree.write("src/A.java", b"class A {}");
        let (size, modified) = stat(&path).unwrap();
        let index = Index {
            settings: "s".into(),
            sources: vec![Source {
                path: path.clone(),
                size,
                modified,
                hash: sha256_hex(b"class A {}"),
                classes: Vec::new(),
                deps: BTreeSet::new(),
            }],
        };
        assert!(index.holds(std::slice::from_ref(&path)));
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&path, b"class A {}").unwrap();
        assert!(index.holds(std::slice::from_ref(&path)), "touched");
        std::fs::write(&path, b"class A { }").unwrap();
        assert!(!index.holds(std::slice::from_ref(&path)), "changed");
        assert!(!index.holds(&[]), "another source set");
    }
}
