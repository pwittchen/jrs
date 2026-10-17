//! Recompiling a Java unit file by file (SPEC §7.2).
//!
//! A unit whose settings have not changed since its last build does not need
//! every source compiled again when a few of them changed. After each build
//! jrs records, in `target/.jrs/<unit>.index`, what every source compiled
//! to: its classes, the API of each as `abi.rs` reads it — its header and
//! each of its members apart — its supertypes, and which of the unit's other
//! classes they refer to. The next build compiles the changed sources alone,
//! against the classes already in the output directory, and compares their
//! API with the recorded one. A change that stops at method bodies ends
//! there.
//!
//! One that reaches the API is followed in rounds (`specs/todo/BEATING_THE_DAEMON.md`
//! §2). A class that only gained members compiles, in the next round, the
//! sources that refer to it or to one of its subtypes — whose inherited
//! members changed with it, and a new overload can change which method a
//! caller of either picks. A round's classes whose own API is then unchanged
//! end the walk; those whose API changed start the next round. A change that
//! is more than an addition — a member removed or changed, a supertype, a
//! flag, an annotation — breaks what was compiled against it in ways the
//! constant pools cannot all show, so it recompiles every source that refers
//! to a changed class, and every source that refers to one of those,
//! transitively, as does a walk past [`ROUNDS`] rounds.
//!
//! A unit can also follow the class directories it compiles against class by
//! class ([`CompileUnit::tracked`]): the main classes, for the tests, and the
//! upstream members' classes, for a workspace member. The index then records
//! each of their classes the sources refer to, and those classes'
//! supertypes there, as they were compiled against. When their API changes,
//! the unit compiles the sources that refer to the classes that changed —
//! all that reach them, for a change more than an addition — and goes on in
//! rounds from there, instead of compiling whole.
//!
//! Anything this cannot account for compiles the whole unit, as before:
//!
//! - different settings: flags, a jar, the main classes' API for the tests
//!   when they are not tracked;
//! - a source added or deleted, and a top-level class that appeared or went
//!   away, in the unit or in a tracked directory: a simple name elsewhere may
//!   now mean another class, and nothing in that source's class files says
//!   it used the name;
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

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ffi::OsStr;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, UNIX_EPOCH};

use super::abi::{self, ClassInfo, Kind};
use super::worker::Worker;
use super::{CompileUnit, count, javac};
use crate::error::{IoResultExt, Result};
use crate::project;
use crate::resolve::repo::sha256_hex;
use crate::toolchain::Toolchain;
use crate::ui::Ui;

const HEADER: &str = "jrs compile index 2";

/// The most sources a file-by-file `javac` run may hold and still start its
/// JVM with [`javac::QUICK_JVM_FLAGS`]. Measured on JDK 25, C1 and the serial
/// collector beat the full JIT up to a few hundred small sources; a
/// whole-unit compile keeps the full JIT whatever its size.
pub(super) const QUICK_SOURCES: usize = 100;

/// A `javac` worker for the rounds after the first of a unit's compile
/// (`specs/todo/BEATING_THE_DAEMON_2.md` §5), when the unit holds none of
/// its own and the worker would start warm: one that starts cold costs
/// its first round twice what a forked `javac` does. It lives in the
/// shared cache beside the archives, as every other `javac` worker does.
fn worker_for_rounds(unit: &CompileUnit, toolchain: &Toolchain) -> Option<Worker> {
    if unit.worker.is_some() {
        return None;
    }
    let worker = Worker::for_rounds(unit.share_dir.as_deref()?.parent()?.join("worker"));
    worker.starts_warm(toolchain).then_some(worker)
}

/// How many rounds of direct dependents an API change may take, each a
/// `javac` run, before whatever else it reaches compiles in one more.
const ROUNDS: usize = 4;

/// The service file through which `javac` finds annotation processors on
/// its classpath.
const PROCESSORS: &str = "META-INF/services/javax.annotation.processing.Processor";

/// The one through which groovyc finds AST transformations: a tracked
/// directory that holds one counts by its bytes, as `abi.rs` has it.
const AST_TRANSFORMATIONS: &str =
    "META-INF/services/org.codehaus.groovy.transform.ASTTransformation";

/// What the last successful build compiled, source by source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Index {
    /// A digest of [`CompileUnit::settings`].
    settings: String,
    /// A digest of the top-level classes the tracked directories held;
    /// `-` for a unit that tracks none.
    upstream: String,
    /// In the unit's source order.
    sources: Vec<Source>,
    /// The tracked classes the sources were compiled against, as they were
    /// then: each one a source refers to, and their supertypes there.
    seen: Vec<Class>,
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
    /// The tracked classes this source's refer to.
    ext: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Class {
    /// `com/example/Main$Inner`.
    name: String,
    kind: Kind,
    api: String,
    constants: String,
    /// [`ClassInfo::header`].
    header: String,
    /// [`ClassInfo::members`].
    members: Vec<String>,
    /// Its superclass and interfaces.
    supers: Vec<String>,
}

impl Class {
    fn new(info: &ClassInfo) -> Class {
        Class {
            name: info.name.clone(),
            kind: info.kind,
            api: info.api.clone(),
            constants: info.constants.clone(),
            header: info.header.clone(),
            members: info.members.clone(),
            supers: info.supers.clone(),
        }
    }

    fn parse(rest: &str) -> Option<Class> {
        let mut fields = rest.splitn(5, ' ');
        let kind = match fields.next()? {
            "top" => Kind::TopLevel,
            "nested" => Kind::Nested,
            "hidden" => Kind::Hidden,
            _ => return None,
        };
        Some(Class {
            kind,
            api: fields.next()?.to_string(),
            constants: fields.next()?.to_string(),
            header: fields.next()?.to_string(),
            name: fields.next()?.to_string(),
            members: Vec::new(),
            supers: Vec::new(),
        })
    }

    fn render(&self, key: &str, out: &mut String) {
        let kind = match self.kind {
            Kind::TopLevel => "top",
            Kind::Nested => "nested",
            Kind::Hidden => "hidden",
        };
        let _ = writeln!(
            out,
            "{key} {kind} {} {} {} {}",
            self.api, self.constants, self.header, self.name
        );
        if !self.members.is_empty() {
            let _ = writeln!(out, "members {}", self.members.join(" "));
        }
        for name in &self.supers {
            let _ = writeln!(out, "super {name}");
        }
    }

    /// Whether `self`, a later build of `before` with another API, only
    /// gained members: the same header, and every member it had.
    fn only_adds_to(&self, before: &Class) -> bool {
        self.kind == before.kind
            && self.header == before.header
            && before
                .members
                .iter()
                .all(|m| self.members.binary_search(m).is_ok())
    }
}

/// What changed in the API of a set of classes, by class name.
#[derive(Debug, Default, PartialEq, Eq)]
struct Changes {
    /// Classes that only gained members.
    added: BTreeSet<String>,
    /// Classes whose API changed otherwise, or that went away.
    broken: BTreeSet<String>,
}

impl Changes {
    fn is_empty(&self) -> bool {
        self.added.is_empty() && self.broken.is_empty()
    }

    fn all(&self) -> BTreeSet<String> {
        self.added.union(&self.broken).cloned().collect()
    }
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
        let upstream = lines.next()?.strip_prefix("upstream ")?.to_string();
        let mut sources: Vec<Source> = Vec::new();
        let mut seen: Vec<Class> = Vec::new();
        for line in lines {
            let (key, rest) = line.split_once(' ')?;
            match key {
                "source" if seen.is_empty() => {
                    let mut fields = rest.splitn(4, ' ');
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
                        ext: BTreeSet::new(),
                    });
                }
                "class" if seen.is_empty() => {
                    sources.last_mut()?.classes.push(Class::parse(rest)?);
                }
                "seen" => seen.push(Class::parse(rest)?),
                "members" | "super" => {
                    let class = match seen.last_mut() {
                        Some(class) => class,
                        None => sources.last_mut()?.classes.last_mut()?,
                    };
                    if key == "members" {
                        class.members = rest.split(' ').map(str::to_string).collect();
                    } else {
                        class.supers.push(rest.to_string());
                    }
                }
                "dep" if seen.is_empty() => {
                    sources.last_mut()?.deps.insert(rest.to_string());
                }
                "ext" if seen.is_empty() => {
                    sources.last_mut()?.ext.insert(rest.to_string());
                }
                _ => return None,
            }
        }
        Some(Index {
            settings,
            upstream,
            sources,
            seen,
        })
    }

    fn render(&self) -> String {
        let mut out = format!(
            "{HEADER}\nsettings {}\nupstream {}\n",
            self.settings, self.upstream
        );
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
                class.render("class", &mut out);
            }
            for dep in &source.deps {
                let _ = writeln!(out, "dep {dep}");
            }
            for ext in &source.ext {
                let _ = writeln!(out, "ext {ext}");
            }
        }
        for class in &self.seen {
            class.render("seen", &mut out);
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

/// The contents hash of each of `sources` that the unit's index recorded
/// at its present size and modification time, so that a key over the
/// sources reads only those that moved.
pub(super) fn known_hashes(unit: &CompileUnit) -> HashMap<PathBuf, String> {
    let Some(index) = Index::load(unit) else {
        return HashMap::new();
    };
    index
        .sources
        .into_iter()
        .filter(|s| !s.hash.is_empty() && stat(&s.path) == Some((s.size, s.modified)))
        .map(|s| (s.path, s.hash))
        .collect()
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

/// The class directories the unit follows class by class: none when one of
/// them holds code a compiler runs, whose every class counts by its bytes.
fn tracked_dirs(unit: &CompileUnit) -> Vec<PathBuf> {
    if unit
        .tracked
        .iter()
        .any(|dir| dir.join(PROCESSORS).is_file() || dir.join(AST_TRANSFORMATIONS).is_file())
    {
        return Vec::new();
    }
    unit.tracked.clone()
}

/// A digest of the top-level classes under `dirs`, by name: a class file
/// whose name has no `$`. One appearing or going away may change what a
/// simple name means in a source that never mentioned it.
fn top_level(dirs: &[PathBuf]) -> Result<String> {
    let mut text = String::new();
    for dir in dirs {
        let _ = writeln!(text, "dir");
        let mut names: Vec<String> = project::find_by_extension(dir, "class")?
            .iter()
            .filter_map(|file| file.strip_prefix(dir).ok())
            .map(|relative| {
                relative
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/")
            })
            .filter(|name| !name.contains('$'))
            .collect();
        names.sort();
        for name in names {
            let _ = writeln!(text, "{name}");
        }
    }
    Ok(sha256_hex(text.as_bytes()))
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
    /// [`tracked_dirs`].
    tracked: Vec<PathBuf>,
    /// [`top_level`] of `tracked` as they are now; `-` with none.
    upstream: String,
}

impl Tracker {
    /// `None` when the unit is always compiled whole; its index, if it had
    /// one, is dropped.
    ///
    /// # Errors
    ///
    /// `JrsError::Io` if a source or a tracked directory cannot be read.
    pub(super) fn new(unit: &CompileUnit) -> Result<Option<Tracker>> {
        if !allowed(unit) {
            forget(unit);
            return Ok(None);
        }
        let tracked = tracked_dirs(unit);
        let settings = sha256_hex(unit.settings_with(tracked.is_empty()).as_bytes());
        let upstream = if tracked.is_empty() {
            "-".to_string()
        } else {
            top_level(&tracked)?
        };
        let previous = Index::load(unit).filter(|index| {
            index.settings == settings
                && (index.upstream == "-") == tracked.is_empty()
                && unit.output_dir.is_dir()
        });
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
            tracked,
            upstream,
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
    #[allow(
        clippy::too_many_lines,
        reason = "the rounds of one recompile share their state: what compiled, what \
                  changed, and the index as it stands"
    )]
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
        // A new or deleted source brings or takes away a top-level class,
        // and so may a tracked directory.
        if previous.sources.len() != self.current.len()
            || previous
                .sources
                .iter()
                .zip(&self.current)
                .any(|(p, c)| p.path != c.path)
            || previous.upstream != self.upstream
        {
            return Ok(None);
        }
        let Some(upstream) = self.upstream_changes(previous)? else {
            return Ok(None);
        };
        let mut first: BTreeSet<usize> = (0..self.current.len())
            .filter(|&i| previous.sources[i].hash != self.current[i].hash)
            .collect();
        if first.is_empty() && upstream.is_empty() {
            // Touched, not changed: remember the new mtimes.
            let mut index = previous.clone();
            for (source, now) in index.sources.iter_mut().zip(&self.current) {
                (source.size, source.modified) = (now.size, now.modified);
            }
            index.save(unit)?;
            return Ok(Some(0));
        }
        first.extend(reached_from_upstream(previous, &upstream));
        if first.is_empty() {
            // No source here uses what changed: only what was seen moves.
            let Some(after) = self.record(unit)? else {
                return Ok(None);
            };
            after.save(unit)?;
            return Ok(Some(0));
        }

        // The sources compiled first lose their old classes, and so does
        // anything a failed build left behind; the rest stay, to be compiled
        // against.
        let mut compiled = first.clone();
        sweep(unit, previous, &compiled)?;
        // The later rounds go to a warm `javac` worker held for the rest of
        // this compile, when the unit has none of its own. It starts beside
        // the first round when other sources name a class that round
        // compiles, as an API change there would take a second
        // (`specs/todo/BEATING_THE_DAEMON_3.md` §4).
        let mut later: Option<Option<Worker>> = None;
        if unit.worker.is_none() && is_named(previous, &first) {
            let worker = worker_for_rounds(unit, toolchain);
            if let Some(worker) = &worker {
                worker.prestart(toolchain, ui);
            }
            later = Some(worker);
        }
        let mut round: Vec<usize> = first.into_iter().collect();
        self.javac(
            toolchain, unit, ui, steps, &round, previous, &compiled, None,
        )?;
        let Some(mut after) = self.record(unit)? else {
            return Ok(None);
        };
        let mut changes = Changes::default();
        for &i in &round {
            if !compare(
                &previous.sources[i].classes,
                &after.sources[i].classes,
                &mut changes,
            ) {
                return Ok(None);
            }
        }
        // A class here that inherits from a changed tracked one has new
        // inherited members too, whether or not its own API moved.
        let classes = || after.sources.iter().flat_map(|s| &s.classes);
        let added = inheriting(classes(), &upstream.added);
        let broken = inheriting(classes(), &upstream.broken);
        changes.added.extend(added);
        changes.broken.extend(broken);

        let mut rounds = 1;
        while !changes.is_empty() {
            let just: BTreeSet<usize> = round.iter().copied().collect();
            if !changes.broken.is_empty() || rounds >= ROUNDS {
                // Whatever the change reaches, in one more run.
                let reached = subtypes(&after, &changes.all());
                let rest = dependents(&after, &reached, &just);
                if !rest.is_empty() {
                    remove_classes(unit, &after, &rest)?;
                    compiled.extend(&rest);
                    let worker = later
                        .get_or_insert_with(|| worker_for_rounds(unit, toolchain))
                        .as_ref();
                    self.javac(
                        toolchain, unit, ui, steps, &rest, previous, &compiled, worker,
                    )?;
                    let Some(next) = self.record(unit)? else {
                        return Ok(None);
                    };
                    after = next;
                    rounds += 1;
                }
                break;
            }
            let reached = subtypes(&after, &changes.added);
            let next: Vec<usize> = direct_users(&after, &reached)
                .difference(&just)
                .copied()
                .collect();
            if next.is_empty() {
                break;
            }
            remove_classes(unit, &after, &next)?;
            compiled.extend(&next);
            let worker = later
                .get_or_insert_with(|| worker_for_rounds(unit, toolchain))
                .as_ref();
            self.javac(
                toolchain, unit, ui, steps, &next, previous, &compiled, worker,
            )?;
            let Some(recorded) = self.record(unit)? else {
                return Ok(None);
            };
            changes = Changes::default();
            for &i in &next {
                if !compare(
                    &after.sources[i].classes,
                    &recorded.sources[i].classes,
                    &mut changes,
                ) {
                    return Ok(None);
                }
            }
            after = recorded;
            round = next;
            rounds += 1;
        }
        ui.verbose(format!(
            "{} unit: {} compiled in {rounds} {}",
            unit.label,
            count(compiled.len(), None),
            if rounds == 1 { "round" } else { "rounds" }
        ));
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
        if let Some(index) = self.record(unit)? {
            index.save(unit)
        } else {
            forget(unit);
            Ok(())
        }
    }

    /// What changed in the tracked classes the sources were compiled
    /// against: those that are gone count as broken, and a subtype of a
    /// changed class changes with it. `None` when only compiling the whole
    /// unit is safe: a class there cannot be read, or a constant changed.
    fn upstream_changes(&self, previous: &Index) -> Result<Option<Changes>> {
        let mut changes = Changes::default();
        if self.tracked.is_empty() {
            return Ok(Some(changes));
        }
        let mut now: Vec<Class> = Vec::new();
        for before in &previous.seen {
            let Some(path) = self.find_tracked(&before.name) else {
                changes.broken.insert(before.name.clone());
                continue;
            };
            let bytes = std::fs::read(&path).path(&path)?;
            let Some(info) = abi::class_info(&bytes) else {
                return Ok(None);
            };
            let after = Class::new(&info);
            if after.constants != before.constants {
                return Ok(None);
            }
            if after.api != before.api || after.kind != before.kind {
                if after.only_adds_to(before) {
                    changes.added.insert(before.name.clone());
                } else {
                    changes.broken.insert(before.name.clone());
                }
            }
            now.push(after);
        }
        let added = inheriting(&now, &changes.added);
        let broken = inheriting(&now, &changes.broken);
        changes.added.extend(added);
        changes.broken.extend(broken);
        Ok(Some(changes))
    }

    /// The class file of `name` in the first tracked directory that has one.
    fn find_tracked(&self, name: &str) -> Option<PathBuf> {
        self.tracked
            .iter()
            .map(|dir| class_path(dir, name))
            .find(|path| path.is_file())
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
        worker: Option<&Worker>,
    ) -> Result<()> {
        let paths: Vec<PathBuf> = sources
            .iter()
            .map(|&i| self.current[i].path.clone())
            .collect();
        let started = Instant::now();
        let quick = paths.len() <= QUICK_SOURCES;
        let result = javac::run(
            toolchain,
            unit,
            worker.or(unit.worker.as_deref()),
            &paths,
            true,
            quick,
            &count(paths.len(), None),
            ui,
        );
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

    /// Index what the output directory holds now, and the tracked classes
    /// its classes refer to. `None` if a class cannot be read, or cannot be
    /// tied to exactly one source: by its `SourceFile` name, and when two
    /// sources share that name, by its package too.
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
            let owner = if let [one] = candidates {
                *one
            } else {
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
            };
            unit_classes.insert(info.name.clone());
            classes[owner].push(Class::new(&info));
            refs[owner].extend(info.refs);
        }

        let Some(seen) = self.seen(&refs, &unit_classes)? else {
            return Ok(None);
        };
        let sources = self
            .current
            .iter()
            .zip(classes)
            .zip(refs)
            .map(|((now, classes), refs)| {
                let own: HashSet<&str> = classes.iter().map(|c| c.name.as_str()).collect();
                let ext = refs
                    .iter()
                    .filter(|r| seen.contains_key(r.as_str()))
                    .cloned()
                    .collect();
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
                    ext,
                }
            })
            .collect();
        Ok(Some(Index {
            settings: self.settings.clone(),
            upstream: self.upstream.clone(),
            sources,
            seen: seen.into_values().collect(),
        }))
    }

    /// The tracked classes the unit's classes refer to, and their
    /// supertypes there, by name. `None` if one cannot be read.
    fn seen(
        &self,
        refs: &[BTreeSet<String>],
        unit_classes: &HashSet<String>,
    ) -> Result<Option<BTreeMap<String, Class>>> {
        let mut found = BTreeMap::new();
        if self.tracked.is_empty() {
            return Ok(Some(found));
        }
        let mut visited: HashSet<String> = HashSet::new();
        let mut queue: Vec<String> = refs
            .iter()
            .flatten()
            .filter(|r| !unit_classes.contains(*r))
            .cloned()
            .collect();
        while let Some(name) = queue.pop() {
            if !visited.insert(name.clone()) {
                continue;
            }
            let Some(path) = self.find_tracked(&name) else {
                continue;
            };
            let bytes = std::fs::read(&path).path(&path)?;
            let Some(info) = abi::class_info(&bytes) else {
                return Ok(None);
            };
            queue.extend(info.supers.iter().cloned());
            found.insert(name, Class::new(&info));
        }
        Ok(Some(found))
    }
}

/// The sources a change to the tracked classes reaches first: those that
/// refer to a changed class, and, for a break, whatever refers to their
/// classes too, transitively.
fn reached_from_upstream(index: &Index, upstream: &Changes) -> BTreeSet<usize> {
    let users: BTreeSet<usize> = (0..index.sources.len())
        .filter(|&i| {
            let ext = &index.sources[i].ext;
            upstream
                .added
                .iter()
                .chain(&upstream.broken)
                .any(|c| ext.contains(c))
        })
        .collect();
    let broken: BTreeSet<String> = users
        .iter()
        .filter(|&&i| {
            upstream
                .broken
                .iter()
                .any(|c| index.sources[i].ext.contains(c))
        })
        .flat_map(|&i| index.sources[i].classes.iter().map(|c| c.name.clone()))
        .collect();
    let mut reached = users;
    reached.extend(dependents(index, &broken, &BTreeSet::new()));
    reached
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

/// Delete the classes `index` records for `sources`, which compile next.
fn remove_classes(unit: &CompileUnit, index: &Index, sources: &[usize]) -> Result<()> {
    for &i in sources {
        for class in &index.sources[i].classes {
            let path = class_path(&unit.output_dir, &class.name);
            match std::fs::remove_file(&path) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                result => result.path(&path)?,
            }
        }
    }
    Ok(())
}

/// Compare what a recompiled source compiles to with what it compiled to
/// before, adding to `changes` every class whose API other sources may have
/// compiled against differently. `false` when only compiling the whole unit
/// is safe: a top-level class appeared or went away, or a constant changed.
fn compare(old: &[Class], new: &[Class], changes: &mut Changes) -> bool {
    for before in old {
        if let Some(after) = new.iter().find(|c| c.name == before.name) {
            if after.constants != before.constants
                || (after.kind != before.kind
                    && Kind::TopLevel == after.kind.max_visibility(before.kind))
            {
                return false;
            }
            if after.api != before.api || after.kind != before.kind {
                if after.only_adds_to(before) {
                    changes.added.insert(before.name.clone());
                } else {
                    changes.broken.insert(before.name.clone());
                }
            }
        } else {
            if before.kind == Kind::TopLevel || before.constants != "-" {
                return false;
            }
            if before.kind != Kind::Hidden {
                changes.broken.insert(before.name.clone());
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
            changes.added.insert(after.name.clone());
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

/// The classes among `classes` that are a subtype, at any depth through
/// `classes`, of one in `changed`, less those in it: their inherited
/// members changed with it.
fn inheriting<'a>(
    classes: impl IntoIterator<Item = &'a Class>,
    changed: &BTreeSet<String>,
) -> BTreeSet<String> {
    let mut subclasses: HashMap<&str, Vec<&str>> = HashMap::new();
    for class in classes {
        for name in &class.supers {
            subclasses
                .entry(name.as_str())
                .or_default()
                .push(&class.name);
        }
    }
    let mut reached: BTreeSet<String> = BTreeSet::new();
    let mut queue: Vec<&str> = changed.iter().map(String::as_str).collect();
    while let Some(name) = queue.pop() {
        for &sub in subclasses.get(name).map_or(&[][..], Vec::as_slice) {
            if !changed.contains(sub) && reached.insert(sub.to_string()) {
                queue.push(sub);
            }
        }
    }
    reached
}

/// `changed` and every class of the unit that inherits from one of them.
fn subtypes(index: &Index, changed: &BTreeSet<String>) -> BTreeSet<String> {
    let mut all = inheriting(index.sources.iter().flat_map(|s| &s.classes), changed);
    all.extend(changed.iter().cloned());
    all
}

/// Whether a source outside `sources` refers to a class one of them holds.
fn is_named(index: &Index, sources: &BTreeSet<usize>) -> bool {
    let classes: BTreeSet<String> = sources
        .iter()
        .flat_map(|&i| index.sources[i].classes.iter().map(|c| c.name.clone()))
        .collect();
    direct_users(index, &classes)
        .iter()
        .any(|i| !sources.contains(i))
}

/// The sources that refer to a class in `changed`.
fn direct_users(index: &Index, changed: &BTreeSet<String>) -> BTreeSet<usize> {
    (0..index.sources.len())
        .filter(|&i| index.sources[i].deps.iter().any(|d| changed.contains(d)))
        .collect()
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
            header: "h".into(),
            members: Vec::new(),
            supers: Vec::new(),
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
            ext: BTreeSet::new(),
        }
    }

    fn top(name: &str) -> Class {
        class(name, Kind::TopLevel, name, "-")
    }

    /// `name`, with `members`, extending `supers`.
    fn shaped(name: &str, header: &str, members: &[&str], supers: &[&str]) -> Class {
        let mut members: Vec<String> = members.iter().map(|m| (*m).to_string()).collect();
        members.sort();
        Class {
            api: format!("{header}{}", members.join("")),
            header: header.into(),
            members,
            supers: supers.iter().map(|s| (*s).to_string()).collect(),
            ..top(name)
        }
    }

    fn index(sources: Vec<Source>) -> Index {
        Index {
            settings: "s".into(),
            upstream: "-".into(),
            sources,
            seen: Vec::new(),
        }
    }

    fn names(set: &BTreeSet<String>) -> Vec<&str> {
        set.iter().map(String::as_str).collect()
    }

    #[test]
    fn an_index_reads_back_what_it_wrote() {
        let mut failed = source("/src/b dir/B.java", vec![top("p/B")], &[]);
        failed.hash.clear();
        failed.ext.insert("q/Up".into());
        let mut index = index(vec![
            source(
                "/src/A.java",
                vec![
                    shaped("p/A", "a1", &["m1", "m2"], &["java/lang/Object", "p/I"]),
                    class("p/A$In", Kind::Nested, "a2", "-"),
                    class("p/A$1", Kind::Hidden, "-", "-"),
                ],
                &["p/B"],
            ),
            failed,
        ]);
        index.upstream = "u".into();
        index.seen = vec![shaped("q/Up", "u1", &["x"], &["q/Base"]), top("q/Base")];
        assert_eq!(Index::parse(&index.render()), Some(index));
        assert_eq!(Index::parse("jrs compile index 1\nsettings s\n"), None);
        assert_eq!(
            Index::parse(&format!(
                "{HEADER}\nsettings s\nupstream -\nclass top a c h p/A\n"
            )),
            None
        );
        assert_eq!(
            Index::parse(&format!(
                "{HEADER}\nsettings s\nupstream -\nseen top a c h q/A\nsource 1 2 h /A.java\n"
            )),
            None,
            "the seen classes come last"
        );
    }

    #[test]
    fn a_body_change_changes_no_api() {
        let old = [top("p/A"), class("p/A$1", Kind::Hidden, "-", "-")];
        let new = [top("p/A"), class("p/A$2", Kind::Hidden, "-", "-")];
        let mut changes = Changes::default();
        assert!(compare(&old, &new, &mut changes));
        assert!(changes.is_empty(), "{changes:?}");
    }

    #[test]
    fn a_new_member_or_nested_class_is_an_addition() {
        let mut changes = Changes::default();
        assert!(compare(
            &[shaped("p/A", "h", &["m1"], &[])],
            &[
                shaped("p/A", "h", &["m1", "m2"], &[]),
                class("p/A$In", Kind::Nested, "x", "-"),
            ],
            &mut changes
        ));
        assert_eq!(names(&changes.added), ["p/A", "p/A$In"]);
        assert!(changes.broken.is_empty());
    }

    #[test]
    fn a_removed_member_or_a_new_header_is_a_break() {
        let before = shaped("p/A", "h", &["m1", "m2"], &[]);
        for after in [
            shaped("p/A", "h", &["m1"], &[]),
            shaped("p/A", "h", &["m1", "m3"], &[]),
            shaped("p/A", "other", &["m1", "m2"], &[]),
        ] {
            let mut changes = Changes::default();
            assert!(compare(
                std::slice::from_ref(&before),
                &[after],
                &mut changes
            ));
            assert_eq!(names(&changes.broken), ["p/A"]);
            assert!(changes.added.is_empty());
        }
        let mut changes = Changes::default();
        assert!(compare(
            &[top("p/A"), class("p/A$In", Kind::Nested, "x", "-")],
            &[top("p/A")],
            &mut changes
        ));
        assert_eq!(
            names(&changes.broken),
            ["p/A$In"],
            "a vanished nested class"
        );
    }

    #[test]
    fn constants_and_top_level_classes_compile_the_whole_unit() {
        let mut changes = Changes::default();
        let with = |c: &str| class("p/A", Kind::TopLevel, "a", c);
        assert!(
            !compare(&[with("1")], &[with("2")], &mut changes),
            "a constant"
        );
        assert!(
            !compare(&[top("p/A")], &[top("p/A"), top("p/Extra")], &mut changes),
            "a new top-level class"
        );
        assert!(
            !compare(&[top("p/A"), top("p/Gone")], &[top("p/A")], &mut changes),
            "a vanished top-level class"
        );
        assert!(
            !compare(
                &[top("p/A"), class("p/A$K", Kind::Nested, "k", "1")],
                &[top("p/A")],
                &mut changes
            ),
            "a vanished nested class with constants"
        );
    }

    #[test]
    fn dependents_are_found_transitively_through_what_was_compiled() {
        // C calls a method B inherits from A; A changed. B was compiled in
        // the first run, but C only names B, and still has to be compiled.
        let index = index(vec![
            source("A.java", vec![top("p/A")], &[]),
            source("B.java", vec![top("p/B")], &["p/A"]),
            source("C.java", vec![top("p/C")], &["p/B"]),
            source("D.java", vec![top("p/D")], &[]),
        ]);
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

    #[test]
    fn a_round_reaches_the_users_of_a_class_and_of_its_subtypes_only() {
        // B extends A, C uses B, D uses C, E uses A: an addition to A
        // compiles B, C and E next — C through what B inherits — but not D,
        // which only C's own API would reach.
        let index = index(vec![
            source("A.java", vec![top("p/A")], &[]),
            source("B.java", vec![shaped("p/B", "h", &[], &["p/A"])], &["p/A"]),
            source("C.java", vec![top("p/C")], &["p/B"]),
            source("D.java", vec![top("p/D")], &["p/C"]),
            source("E.java", vec![top("p/E")], &["p/A"]),
        ]);
        let reached = subtypes(&index, &BTreeSet::from(["p/A".to_string()]));
        assert_eq!(names(&reached), ["p/A", "p/B"]);
        assert_eq!(
            direct_users(&index, &reached)
                .into_iter()
                .collect::<Vec<_>>(),
            [1, 2, 4]
        );
    }

    #[test]
    fn a_subtype_of_a_changed_class_changes_with_it_at_any_depth() {
        let classes = [
            shaped("q/A", "h", &[], &[]),
            shaped("q/B", "h", &[], &["q/A"]),
            shaped("q/C", "h", &[], &["q/I", "q/B"]),
            shaped("q/D", "h", &[], &[]),
        ];
        let reached = inheriting(&classes, &BTreeSet::from(["q/A".to_string()]));
        assert_eq!(names(&reached), ["q/B", "q/C"]);
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
                share_dir: None,
                build_cache: None,
                worker: None,
                tracked: Vec::new(),
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
        let index = index(vec![Source {
            path: path.clone(),
            size,
            modified,
            hash: sha256_hex(b"class A {}"),
            classes: Vec::new(),
            deps: BTreeSet::new(),
            ext: BTreeSet::new(),
        }]);
        assert!(index.holds(std::slice::from_ref(&path)));
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&path, b"class A {}").unwrap();
        assert!(index.holds(std::slice::from_ref(&path)), "touched");
        std::fs::write(&path, b"class A { }").unwrap();
        assert!(!index.holds(std::slice::from_ref(&path)), "changed");
        assert!(!index.holds(&[]), "another source set");
    }
}
