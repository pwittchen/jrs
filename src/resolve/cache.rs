//! The local artifact store.
//!
//! The layout mirrors the Maven repository path, so the cache is inspectable with
//! `ls` and a path from an error message (SPEC §8.3). Writes go to a temp file in
//! the destination directory and are renamed into place, so an interrupted run
//! cannot leave a truncated jar behind for the next one to link against.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

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
}

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
    fn an_explicit_cache_dir_wins() {
        // Exercised through the public constructor rather than the environment,
        // which tests must not mutate in parallel.
        let cache = Cache::with_root("/tmp/elsewhere");
        assert_eq!(cache.root(), Path::new("/tmp/elsewhere"));
    }
}
