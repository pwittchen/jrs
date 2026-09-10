//! The local artifact store.
//!
//! The layout mirrors the Maven repository path, so the cache is inspectable with
//! `ls` and a path from an error message (SPEC §8.3). Writes go to a temp file in
//! the destination directory and are renamed into place, so an interrupted run
//! cannot leave a truncated jar behind for the next one to link against.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use super::coord::Coord;
use crate::error::{IoResultExt, JrsError, Result};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone)]
pub struct Cache {
    root: PathBuf,
}

impl Cache {
    /// The platform cache directory, overridable with `JRS_CACHE_DIR`.
    pub fn discover() -> Result<Cache> {
        if let Some(dir) = std::env::var_os("JRS_CACHE_DIR") {
            return Ok(Cache::with_root(PathBuf::from(dir)));
        }
        let root = platform_cache_dir().ok_or_else(|| {
            JrsError::resolve(
                "could not determine a cache directory; set JRS_CACHE_DIR to choose one",
            )
        })?;
        Ok(Cache::with_root(root))
    }

    pub fn with_root(root: impl Into<PathBuf>) -> Cache {
        Cache { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Where `coord`'s `.jar` / `.pom` / `.sha1` lives.
    pub fn path_for(&self, coord: &Coord, ext: &str) -> PathBuf {
        self.root.join(
            coord
                .repo_path(ext)
                .replace('/', std::path::MAIN_SEPARATOR_STR),
        )
    }

    pub fn contains(&self, coord: &Coord, ext: &str) -> bool {
        self.path_for(coord, ext).is_file()
    }

    pub fn read(&self, coord: &Coord, ext: &str) -> Result<Vec<u8>> {
        let path = self.path_for(coord, ext);
        std::fs::read(&path).path(&path)
    }

    /// Write bytes into the cache atomically, returning the final path.
    pub fn store(&self, coord: &Coord, ext: &str, bytes: &[u8]) -> Result<PathBuf> {
        let path = self.path_for(coord, ext);
        write_atomic(&path, bytes)?;
        Ok(path)
    }

    /// Remove a cached artifact — used when a checksum does not match.
    pub fn evict(&self, coord: &Coord, ext: &str) {
        let _ = std::fs::remove_file(self.path_for(coord, ext));
    }

    /// Where the record of a cached snapshot's build lives: beside the file,
    /// so pruning a version directory takes it along.
    fn snapshot_record_path(&self, coord: &Coord, ext: &str) -> PathBuf {
        let mut path = self.path_for(coord, ext).into_os_string();
        path.push(".jrs-snapshot");
        PathBuf::from(path)
    }

    /// Which build of a snapshot the cache holds, from where, and when that was
    /// last confirmed.
    pub fn snapshot_record(&self, coord: &Coord, ext: &str) -> Option<SnapshotRecord> {
        let path = self.snapshot_record_path(coord, ext);
        let text = std::fs::read_to_string(&path).ok()?;
        let mut lines = text.lines();
        Some(SnapshotRecord {
            file_version: lines.next()?.to_string(),
            repo: lines.next().unwrap_or_default().to_string(),
            checked: std::fs::metadata(&path).ok()?.modified().ok()?,
        })
    }

    /// Record that the cached `coord` is snapshot build `file_version` from
    /// `repo`, as of now.
    pub fn record_snapshot(
        &self,
        coord: &Coord,
        ext: &str,
        file_version: &str,
        repo: &str,
    ) -> Result<()> {
        write_atomic(
            &self.snapshot_record_path(coord, ext),
            format!("{file_version}\n{repo}\n").as_bytes(),
        )
    }
}

/// The record of which projects use a cache: one lockfile path per line.
const REGISTRY: &str = ".jrs/projects";

/// What `jrs cache prune` removes.
#[derive(Debug, Clone)]
pub enum Prune {
    /// Every version directory no registered project's lockfile names. The
    /// set holds `Coord::version_dir` strings.
    Unreferenced(HashSet<String>),
    /// Every version directory not used for this long.
    UnusedFor(Duration),
}

/// What a prune did, or would do on a dry run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pruned {
    /// Version directories removed, relative to the cache root.
    pub removed: Vec<String>,
    pub bytes: u64,
    pub kept: usize,
}

impl Cache {
    /// Remember that the project whose lockfile is `lock` uses this cache, so
    /// `jrs cache prune` knows what is still wanted.
    ///
    /// The record is machine-local bookkeeping inside the cache, which is why it
    /// may hold absolute paths when `jrs.lock` itself never does.
    pub fn register_project(&self, lock: &Path) -> Result<()> {
        let lock = lock.canonicalize().unwrap_or_else(|_| lock.to_path_buf());
        let mut known = self.projects();
        if known.contains(&lock) {
            return Ok(());
        }
        known.push(lock);
        self.write_projects(&known)
    }

    /// The lockfiles of every project that has used this cache.
    pub fn projects(&self) -> Vec<PathBuf> {
        std::fs::read_to_string(self.root.join(REGISTRY))
            .unwrap_or_default()
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(PathBuf::from)
            .collect()
    }

    /// Drop the projects whose lockfile is gone, returning the rest.
    pub fn forget_missing_projects(&self) -> Result<Vec<PathBuf>> {
        let known = self.projects();
        let existing: Vec<PathBuf> = known.iter().filter(|p| p.is_file()).cloned().collect();
        if existing.len() != known.len() {
            self.write_projects(&existing)?;
        }
        Ok(existing)
    }

    fn write_projects(&self, projects: &[PathBuf]) -> Result<()> {
        let mut sorted = projects.to_vec();
        sorted.sort();
        sorted.dedup();
        let mut text = String::new();
        for p in &sorted {
            text.push_str(&p.display().to_string());
            text.push('\n');
        }
        write_atomic(&self.root.join(REGISTRY), text.as_bytes())
    }

    /// Remove whole version directories — an artifact's jar, POM, checksums
    /// and snapshot records go together — according to `rule`.
    pub fn prune(&self, rule: &Prune, dry_run: bool) -> Result<Pruned> {
        let mut groups: BTreeMap<PathBuf, (u64, SystemTime)> = BTreeMap::new();
        let bookkeeping = self.root.join(".jrs");
        for file in crate::project::find_all(&self.root)? {
            if file.starts_with(&bookkeeping) {
                continue;
            }
            let Some(dir) = file.parent() else { continue };
            let meta = std::fs::metadata(&file).path(&file)?;
            let used = meta
                .accessed()
                .into_iter()
                .chain(meta.modified())
                .max()
                .unwrap_or(SystemTime::UNIX_EPOCH);
            let entry = groups
                .entry(dir.to_path_buf())
                .or_insert((0, SystemTime::UNIX_EPOCH));
            entry.0 += meta.len();
            entry.1 = entry.1.max(used);
        }

        let now = SystemTime::now();
        let mut pruned = Pruned::default();
        for (dir, (bytes, last_used)) in groups {
            let relative = crate::project::slash_path(dir.strip_prefix(&self.root).unwrap_or(&dir));
            let remove = match rule {
                Prune::Unreferenced(keep) => !keep.contains(&relative),
                Prune::UnusedFor(age) => now.duration_since(last_used).unwrap_or_default() > *age,
            };
            if !remove {
                pruned.kept += 1;
                continue;
            }
            if !dry_run {
                std::fs::remove_dir_all(&dir).path(&dir)?;
                self.remove_empty_parents(&dir);
            }
            pruned.bytes += bytes;
            pruned.removed.push(relative);
        }
        Ok(pruned)
    }

    /// Walk up from a removed directory, removing parents it left empty.
    fn remove_empty_parents(&self, dir: &Path) {
        let mut current = dir.parent();
        while let Some(parent) = current {
            if parent == self.root || !parent.starts_with(&self.root) {
                break;
            }
            let empty = std::fs::read_dir(parent).is_ok_and(|mut d| d.next().is_none());
            if !empty || std::fs::remove_dir(parent).is_err() {
                break;
            }
            current = parent.parent();
        }
    }
}

/// A cached snapshot's provenance.
#[derive(Debug, Clone)]
pub struct SnapshotRecord {
    /// The timestamped version in the remote file name, or the plain one.
    pub file_version: String,
    /// The repository it came from, by name.
    pub repo: String,
    pub checked: SystemTime,
}

/// Record that a cached file was used, for `jrs cache prune --unused-for`.
///
/// The access time is the record. Set explicitly, it works on filesystems
/// mounted `noatime`, and it leaves the modification time — which the compile
/// fingerprint reads — alone. It is written at most once a day per file, so a
/// no-op build does not become a flurry of metadata writes; failure is ignored,
/// since this is bookkeeping, not the build.
pub fn mark_used(path: &Path) {
    let now = SystemTime::now();
    let fresh = std::fs::metadata(path)
        .and_then(|m| m.accessed())
        .is_ok_and(|t| now.duration_since(t).unwrap_or_default() < USE_GRANULARITY);
    if fresh {
        return;
    }
    if let Ok(file) = std::fs::OpenOptions::new().write(true).open(path) {
        let _ = file.set_times(std::fs::FileTimes::new().set_accessed(now));
    }
}

/// How finely [`mark_used`] tracks use.
const USE_GRANULARITY: Duration = Duration::from_secs(24 * 60 * 60);

/// Write to a sibling temp file, then rename. Rename is atomic within a
/// filesystem, which is why the temp file goes next to the destination rather
/// than in `/tmp`.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).path(parent)?;
    }
    let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let temp = path.with_extension(format!("jrs-tmp-{}-{n}", std::process::id()));
    std::fs::write(&temp, bytes).path(&temp)?;
    match std::fs::rename(&temp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&temp);
            Err(JrsError::io(path, e))
        }
    }
}

fn platform_cache_dir() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        return std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Caches/jrs"));
    }
    if cfg!(windows) {
        return std::env::var_os("LOCALAPPDATA")
            .map(|d| PathBuf::from(d).join("jrs").join("cache"));
    }
    if let Some(xdg) = std::env::var_os("XDG_CACHE_HOME")
        && !xdg.is_empty()
    {
        return Some(PathBuf::from(xdg).join("jrs"));
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache").join("jrs"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("jrs-cache-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_layout_mirrors_the_maven_repository() {
        let cache = Cache::with_root("/cache");
        let coord = Coord::new("com.google.guava", "guava", "33.0.0-jre");
        assert_eq!(
            cache.path_for(&coord, "jar"),
            PathBuf::from("/cache").join("com/google/guava/guava/33.0.0-jre/guava-33.0.0-jre.jar")
        );
    }

    #[test]
    fn storing_creates_directories_and_round_trips() {
        let dir = temp_dir("store");
        let cache = Cache::with_root(&dir);
        let coord = Coord::new("org.example", "thing", "1.0");
        assert!(!cache.contains(&coord, "jar"));

        let path = cache.store(&coord, "jar", b"contents").unwrap();
        assert!(path.is_file());
        assert!(cache.contains(&coord, "jar"));
        assert_eq!(cache.read(&coord, "jar").unwrap(), b"contents");

        cache.evict(&coord, "jar");
        assert!(!cache.contains(&coord, "jar"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn no_temp_files_survive_a_successful_write() {
        let dir = temp_dir("atomic");
        let cache = Cache::with_root(&dir);
        let coord = Coord::new("org.example", "thing", "1.0");
        cache.store(&coord, "jar", b"x").unwrap();
        let parent = cache
            .path_for(&coord, "jar")
            .parent()
            .unwrap()
            .to_path_buf();
        let names: Vec<String> = std::fs::read_dir(&parent)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["thing-1.0.jar"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn projects_are_registered_once_and_forgotten_when_gone() {
        let dir = temp_dir("registry");
        let cache = Cache::with_root(dir.join("cache"));
        let lock = dir.join("app/jrs.lock");
        std::fs::create_dir_all(lock.parent().unwrap()).unwrap();
        std::fs::write(&lock, "version = 1\n").unwrap();

        cache.register_project(&lock).unwrap();
        cache.register_project(&lock).unwrap();
        assert_eq!(cache.projects().len(), 1);

        std::fs::remove_file(&lock).unwrap();
        assert!(cache.forget_missing_projects().unwrap().is_empty());
        assert!(cache.projects().is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn pruning_removes_whole_unreferenced_version_directories() {
        let dir = temp_dir("prune");
        let cache = Cache::with_root(&dir);
        let kept = Coord::new("org.example", "kept", "1.0");
        let gone = Coord::new("org.example", "gone", "1.0");
        for coord in [&kept, &gone] {
            cache.store(coord, "jar", b"jar bytes").unwrap();
            cache.store(coord, "pom", b"<project/>").unwrap();
        }
        cache
            .register_project(&dir.join("does-not-matter.lock"))
            .unwrap();

        let keep: HashSet<String> = [kept.version_dir()].into();
        let rule = Prune::Unreferenced(keep);
        let dry = cache.prune(&rule, true).unwrap();
        assert_eq!(dry.removed, vec![gone.version_dir()]);
        assert!(cache.contains(&gone, "jar"), "a dry run removed something");

        let done = cache.prune(&rule, false).unwrap();
        assert_eq!(done.removed, vec![gone.version_dir()]);
        assert_eq!(done.kept, 1);
        assert_eq!(
            done.bytes,
            (b"jar bytes".len() + b"<project/>".len()) as u64
        );
        assert!(!cache.contains(&gone, "jar"));
        assert!(
            !dir.join("org/example/gone").exists(),
            "an empty parent was left"
        );
        assert!(cache.contains(&kept, "jar"));
        assert_eq!(cache.projects().len(), 1, "the registry is not an artifact");

        // Everything was used just now, so nothing is old enough to go.
        let fresh = cache
            .prune(&Prune::UnusedFor(Duration::from_secs(3600)), false)
            .unwrap();
        assert!(fresh.removed.is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn marking_use_moves_the_access_time_and_not_the_modification_time() {
        let dir = temp_dir("mark-used");
        let file = dir.join("thing.jar");
        std::fs::write(&file, b"x").unwrap();
        let old = SystemTime::now() - Duration::from_secs(10 * 24 * 60 * 60);
        std::fs::File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_times(
                std::fs::FileTimes::new()
                    .set_accessed(old)
                    .set_modified(old),
            )
            .unwrap();

        mark_used(&file);
        let meta = std::fs::metadata(&file).unwrap();
        assert!(meta.accessed().unwrap() > old + Duration::from_secs(60));
        assert!(meta.modified().unwrap() < old + Duration::from_secs(60));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_explicit_cache_dir_wins() {
        // Exercised through the public constructor rather than the environment,
        // which tests must not mutate in parallel.
        let cache = Cache::with_root("/tmp/elsewhere");
        assert_eq!(cache.root(), Path::new("/tmp/elsewhere"));
    }
}
