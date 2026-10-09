//! The build cache (SPEC §7.8): a compile unit's output, and a passing test
//! run's reports, kept by the hash of their inputs.
//!
//! The fingerprints in `target/.jrs/` say whether `target/` is up to date;
//! they cannot bring back what a branch switch, a `git stash` or a
//! `jrs clean` threw away. This can. Each entry is content-addressed: its
//! key is a SHA-256 over a text that holds no absolute path and no
//! modification time — sources by their path under the project root and
//! their contents, jars by coordinate and pinned checksum — so two checkouts
//! of one commit, in two directories or on two machines, share their
//! entries.
//!
//! An entry is a zip written as `package.rs` writes jars: entries sorted,
//! one fixed timestamp, fixed permissions. Locally it lives at
//! `<cache>/build/<k[0..2]>/<key>.zip`, written to a temporary file and
//! renamed, as every cache write is; a remote cache, when one is
//! configured, is the same keys over HTTP or `file://`: `GET <url>/<key>.zip`
//! on a local miss, `PUT` after a store only when pushing is turned on.
//!
//! The rules are compile avoidance's. A key that cannot be worked out — a
//! class directory on the classpath that nothing digests, a jar that cannot
//! be read — is no key, and the unit compiles. An entry that does not read
//! back as a zip is a miss, and is overwritten by the next store. And none
//! of it can fail a build: a cache that cannot be read or written costs the
//! speed-up, and a remote that fails is reported once under `-v` and left
//! alone for the rest of the command.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::config::{BuildCacheConfig, Credentials, ProxyConfig};
use crate::error::{IoResultExt, JrsError, Result};
use crate::project;
use crate::resolve::cache::{mark_used, write_atomic};
use crate::resolve::repo::{authorization, build_proxy, local_repo_root, sha256_hex};
use crate::toolchain::Toolchain;
use crate::ui::Ui;

/// The first line of every key's text: a new layout of the text is a new
/// set of keys.
const FORMAT: &str = "jrs build cache 1";

/// An entry's files: a path relative to the directory they belong in,
/// `/`-separated, and the bytes.
pub type Entries = Vec<(String, Vec<u8>)>;

/// The build cache of one command: where entries live, and what a key
/// needs beyond a unit's own settings.
pub struct BuildCache {
    /// `<cache>/build`.
    local: PathBuf,
    /// The project root and the shared cache's, as a key's text replaces
    /// them with `{root}` and `{cache}`.
    root: String,
    cache_root: String,
    /// The JDK, by its `release` file (vendor, version and build), or by
    /// `javac -version` when it has none.
    jdk: String,
    /// Each resolved jar's identity — coordinate and pinned checksum — by
    /// its path in the cache.
    jars: HashMap<PathBuf, String>,
    /// The identity of each jar that is not resolved: its content hash,
    /// worked out once.
    hashed: Mutex<HashMap<PathBuf, String>>,
    /// The key a unit's restore worked out, by output directory, so that
    /// its store does not work it out again.
    keys: Mutex<HashMap<PathBuf, String>>,
    remote: Option<Remote>,
    /// `--verify-cache`: compile anyway, and fail on a difference from what
    /// the cache holds.
    verify: bool,
}

impl std::fmt::Debug for BuildCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BuildCache")
            .field("local", &self.local)
            .field("remote", &self.remote.as_ref().map(|r| &r.url))
            .field("verify", &self.verify)
            .finish_non_exhaustive()
    }
}

impl BuildCache {
    /// A cache under `local` for the project at `root`, whose jars come from
    /// the shared cache at `cache_root`, built with `toolchain`.
    #[must_use]
    pub fn new(
        local: PathBuf,
        root: &Path,
        cache_root: &Path,
        toolchain: &Toolchain,
    ) -> BuildCache {
        BuildCache {
            local,
            root: root.display().to_string(),
            cache_root: cache_root.display().to_string(),
            jdk: jdk_identity(toolchain),
            jars: HashMap::new(),
            hashed: Mutex::new(HashMap::new()),
            keys: Mutex::new(HashMap::new()),
            remote: None,
            verify: false,
        }
    }

    /// Name each resolved jar by its identity rather than its contents.
    #[must_use]
    pub fn with_jars(mut self, jars: HashMap<PathBuf, String>) -> BuildCache {
        self.jars = jars;
        self
    }

    /// Read from, and maybe write to, `remote` as well.
    #[must_use]
    pub fn with_remote(mut self, remote: Option<Remote>) -> BuildCache {
        self.remote = remote;
        self
    }

    /// `--verify-cache`: never restore; compare what was compiled with the
    /// entry instead.
    #[must_use]
    pub fn verifying(mut self, verify: bool) -> BuildCache {
        self.verify = verify;
        self
    }

    #[must_use]
    pub fn verifies(&self) -> bool {
        self.verify
    }

    /// `text` with the project root and the shared cache's root replaced by
    /// placeholders, so that it holds no path of this checkout's.
    #[must_use]
    pub fn relative(&self, text: &str) -> String {
        let mut text = text.replace(&self.root, "{root}");
        if !self.cache_root.is_empty() {
            text = text.replace(&self.cache_root, "{cache}");
        }
        text
    }

    /// A jar's identity in a key: its coordinate and pinned checksum when
    /// it was resolved, its content hash otherwise. `None` when it cannot
    /// be read.
    ///
    /// # Panics
    ///
    /// If a thread panicked while holding the lock on the hashes.
    #[must_use]
    pub fn jar(&self, path: &Path) -> Option<String> {
        if let Some(identity) = self.jars.get(path) {
            return Some(identity.clone());
        }
        if let Some(hash) = self.hashed.lock().unwrap().get(path) {
            return Some(hash.clone());
        }
        let hash = format!("sha256:{}", sha256_hex(&std::fs::read(path).ok()?));
        self.hashed
            .lock()
            .unwrap()
            .insert(path.to_path_buf(), hash.clone());
        Some(hash)
    }

    /// The key of an entry of `kind` (`compile`, `test`) whose inputs `text`
    /// describes, with jrs's version and the JDK's in front of it.
    #[must_use]
    pub fn key(&self, kind: &str, text: &str) -> String {
        sha256_hex(
            format!(
                "{FORMAT}\n{kind}\njrs {}\njdk {}\n{text}",
                env!("CARGO_PKG_VERSION"),
                self.jdk
            )
            .as_bytes(),
        )
    }

    /// Remember the key worked out for the unit writing into `dir`.
    ///
    /// # Panics
    ///
    /// If a thread panicked while holding the lock on the keys.
    pub fn remember(&self, dir: &Path, key: &str) {
        self.keys
            .lock()
            .unwrap()
            .insert(dir.to_path_buf(), key.to_string());
    }

    /// The key remembered for the unit writing into `dir`, forgotten as it
    /// is handed back.
    ///
    /// # Panics
    ///
    /// If a thread panicked while holding the lock on the keys.
    #[must_use]
    pub fn recall(&self, dir: &Path) -> Option<String> {
        self.keys.lock().unwrap().remove(dir)
    }

    fn local_path(&self, key: &str) -> PathBuf {
        self.local.join(&key[..2]).join(format!("{key}.zip"))
    }

    /// The entry stored under `key`: the local one, or else the remote one,
    /// which is then kept locally. `None` on a miss, and for an entry that
    /// does not read back.
    pub fn load(&self, key: &str, ui: &Ui) -> Option<Entries> {
        self.load_from(key, self.remote.as_ref(), ui)
    }

    /// A task's entry under `key` (SPEC §7.6): as [`BuildCache::load`], but
    /// from the remote only when `[build-cache] tasks` lets it serve them,
    /// since a task's output may be code that runs outside any JVM.
    pub fn load_task(&self, key: &str, ui: &Ui) -> Option<Entries> {
        self.load_from(key, self.remote.as_ref().filter(|r| r.tasks), ui)
    }

    fn load_from(&self, key: &str, remote: Option<&Remote>, ui: &Ui) -> Option<Entries> {
        let path = self.local_path(key);
        if let Ok(bytes) = std::fs::read(&path) {
            if let Some(entries) = unzip(&bytes) {
                mark_used(&path);
                return Some(entries);
            }
            ui.verbose(format!(
                "build cache: {} does not read back; it is ignored",
                path.display()
            ));
            let _ = std::fs::remove_file(&path);
        }
        let bytes = remote?.get(key, ui)?;
        let Some(entries) = unzip(&bytes) else {
            ui.verbose(format!(
                "build cache: the remote entry {key} does not read back; it is ignored"
            ));
            return None;
        };
        if let Err(e) = write_atomic(&path, &bytes) {
            ui.verbose(format!("build cache: could not keep {key} locally: {e}"));
        }
        Some(entries)
    }

    /// Store `entries` under `key`: locally, and on the remote when pushing
    /// is turned on.
    pub fn save(&self, key: &str, entries: &Entries, ui: &Ui) {
        self.store(key, zip(entries), self.remote.as_ref(), ui);
    }

    /// Store a task's `entries` under `key`, the files named in `executable`
    /// with mode `755`: locally, and on the remote only when pushing is on
    /// and `[build-cache] tasks` lets it hold task entries.
    pub fn save_task<S: std::hash::BuildHasher>(
        &self,
        key: &str,
        entries: &Entries,
        executable: &HashSet<String, S>,
        ui: &Ui,
    ) {
        let remote = self.remote.as_ref().filter(|r| r.tasks);
        self.store(key, zip_with_modes(entries, executable), remote, ui);
    }

    fn store(&self, key: &str, bytes: Result<Vec<u8>>, remote: Option<&Remote>, ui: &Ui) {
        let bytes = match bytes {
            Ok(bytes) => bytes,
            Err(e) => {
                ui.verbose(format!("build cache: could not write an entry: {e}"));
                return;
            }
        };
        let path = self.local_path(key);
        match write_atomic(&path, &bytes) {
            Ok(()) => ui.verbose(format!("build cache: stored {}", path.display())),
            Err(e) => ui.verbose(format!("build cache: could not store {key}: {e}")),
        }
        if let Some(remote) = remote
            && remote.push
        {
            remote.put(key, &bytes, ui);
        }
    }
}

/// A remote build cache: `<url>/<key>.zip`, over HTTP or from a directory.
pub struct Remote {
    url: String,
    push: bool,
    /// Whether task entries are read from and pushed to it as well.
    tasks: bool,
    authorization: Option<String>,
    agent: ureq::Agent,
    /// Set by the first failure, after which the remote is left alone.
    failed: AtomicBool,
}

impl Remote {
    /// The remote `config` names, if it names one, reached through `proxy`
    /// with `credentials`.
    ///
    /// # Errors
    ///
    /// [`JrsError::Usage`] when the proxy is not usable.
    pub fn new(
        config: &BuildCacheConfig,
        credentials: Option<&Credentials>,
        proxy: Option<&ProxyConfig>,
    ) -> Result<Option<Remote>> {
        let Some(url) = &config.url else {
            return Ok(None);
        };
        // A cache that does not answer quickly is slower than compiling.
        let mut agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .user_agent(concat!("jrs/", env!("CARGO_PKG_VERSION")))
            .timeout_connect(Some(Duration::from_secs(5)))
            .timeout_global(Some(Duration::from_secs(120)));
        if let Some(proxy) = proxy {
            agent = agent.proxy(Some(build_proxy(proxy)?));
        }
        Ok(Some(Remote {
            url: url.trim_end_matches('/').to_string(),
            push: config.push,
            tasks: config.tasks,
            authorization: credentials.map(authorization),
            agent: ureq::Agent::new_with_config(agent.build()),
            failed: AtomicBool::new(false),
        }))
    }

    fn entry_url(&self, key: &str) -> String {
        format!("{}/{key}.zip", self.url)
    }

    /// Give up on the remote for the rest of the command, saying so once.
    fn fail(&self, ui: &Ui, what: &str) {
        if !self.failed.swap(true, Ordering::Relaxed) {
            ui.verbose(format!(
                "build cache: {} {what}; not asked again in this run",
                self.url
            ));
        }
    }

    fn get(&self, key: &str, ui: &Ui) -> Option<Vec<u8>> {
        if self.failed.load(Ordering::Relaxed) {
            return None;
        }
        if let Some(dir) = local_repo_root(&self.url) {
            return std::fs::read(dir.join(format!("{key}.zip"))).ok();
        }
        let mut request = self.agent.get(&self.entry_url(key));
        if let Some(auth) = &self.authorization {
            request = request.header("Authorization", auth);
        }
        let mut response = match request.call() {
            Ok(response) => response,
            Err(e) => {
                self.fail(ui, &format!("could not be reached ({e})"));
                return None;
            }
        };
        match response.status().as_u16() {
            200 => {}
            404 | 410 => return None,
            status => {
                self.fail(ui, &format!("answered HTTP {status}"));
                return None;
            }
        }
        match response.body_mut().read_to_vec() {
            Ok(bytes) => Some(bytes),
            Err(e) => {
                self.fail(ui, &format!("broke off an answer ({e})"));
                None
            }
        }
    }

    fn put(&self, key: &str, bytes: &[u8], ui: &Ui) {
        if self.failed.load(Ordering::Relaxed) {
            return;
        }
        if let Some(dir) = local_repo_root(&self.url) {
            if let Err(e) = write_atomic(&dir.join(format!("{key}.zip")), bytes) {
                self.fail(ui, &format!("could not be written ({e})"));
            }
            return;
        }
        let mut request = self.agent.put(&self.entry_url(key));
        if let Some(auth) = &self.authorization {
            request = request.header("Authorization", auth);
        }
        match request.send(bytes) {
            Ok(response) if response.status().is_success() => {
                ui.verbose(format!("build cache: pushed {key} to {}", self.url));
            }
            Ok(response) => {
                self.fail(ui, &format!("refused an entry: HTTP {}", response.status()));
            }
            Err(e) => self.fail(ui, &format!("could not be reached ({e})")),
        }
    }
}

/// What the JDK is, for a key: its `release` file, which names the vendor,
/// the version and the build, or else `javac -version`'s full output, since
/// a patch release may change the bytes `javac` writes.
fn jdk_identity(toolchain: &Toolchain) -> String {
    let java = std::fs::canonicalize(&toolchain.java).unwrap_or_else(|_| toolchain.java.clone());
    if let Some(home) = java.parent().and_then(Path::parent)
        && let Ok(release) = std::fs::read(home.join("release"))
    {
        return format!("release {}", sha256_hex(&release));
    }
    std::process::Command::new(&toolchain.javac)
        .arg("-version")
        .stdin(std::process::Stdio::null())
        .output()
        .map_or_else(
            |_| format!("javac {} (unknown build)", toolchain.version),
            |o| {
                format!(
                    "javac {}{}",
                    String::from_utf8_lossy(&o.stdout).trim(),
                    String::from_utf8_lossy(&o.stderr).trim()
                )
            },
        )
}

/// Every file under `dir` but those named in `left_out`, as an entry's
/// files, sorted by name.
///
/// # Errors
///
/// [`JrsError::Io`] if `dir` cannot be walked or a file in it read.
pub fn collect<S: std::hash::BuildHasher>(
    dir: &Path,
    left_out: &HashSet<String, S>,
) -> Result<Entries> {
    let mut entries = Vec::new();
    for file in project::find_all(dir)? {
        let name = project::slash_path(file.strip_prefix(dir).unwrap_or(&file));
        if left_out.contains(&name) {
            continue;
        }
        let bytes = std::fs::read(&file).path(&file)?;
        entries.push((name, bytes));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(entries)
}

/// Write `entries` into `dir`, which is emptied first: what it held was
/// another build's.
///
/// # Errors
///
/// [`JrsError::Io`] if `dir` cannot be emptied or a file written.
pub fn extract(entries: &Entries, dir: &Path) -> Result<()> {
    if dir.exists() {
        std::fs::remove_dir_all(dir).path(dir)?;
    }
    std::fs::create_dir_all(dir).path(dir)?;
    for (name, bytes) in entries {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).path(parent)?;
        }
        std::fs::write(&path, bytes).path(&path)?;
    }
    Ok(())
}

/// `entries` as a deterministic zip: sorted, one fixed timestamp, fixed
/// permissions.
///
/// # Errors
///
/// [`JrsError::Build`] if the zip cannot be written.
pub fn zip(entries: &Entries) -> Result<Vec<u8>> {
    zip_with_modes(entries, &HashSet::<String>::new())
}

/// [`zip`], with the files named in `executable` at mode `755` rather than
/// `644`: a task's outputs keep their execute bit, and the modes stay a
/// fixed set, so the entry stays deterministic.
///
/// # Errors
///
/// [`JrsError::Build`] if the zip cannot be written.
pub fn zip_with_modes<S: std::hash::BuildHasher>(
    entries: &Entries,
    executable: &HashSet<String, S>,
) -> Result<Vec<u8>> {
    let sorted: BTreeMap<&str, &[u8]> = entries
        .iter()
        .map(|(n, b)| (n.as_str(), b.as_slice()))
        .collect();
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .last_modified_time(crate::package::fixed_timestamp());
    let fail = |e: &dyn std::fmt::Display| JrsError::build(format!("build cache entry: {e}"));
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (name, bytes) in sorted {
        let mode = if executable.contains(name) {
            0o755
        } else {
            0o644
        };
        writer
            .start_file(name, options.unix_permissions(mode))
            .map_err(|e| fail(&e))?;
        writer.write_all(bytes).map_err(|e| fail(&e))?;
    }
    Ok(writer.finish().map_err(|e| fail(&e))?.into_inner())
}

/// The names of the files [`zip_with_modes`] wrote with mode `755`, or
/// `None` when `bytes` do not read back as a zip.
#[must_use]
pub fn executables(bytes: &[u8]) -> Option<HashSet<String>> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).ok()?;
    let mut out = HashSet::new();
    for i in 0..archive.len() {
        let file = archive.by_index_raw(i).ok()?;
        if file.unix_mode().is_some_and(|m| m & 0o111 != 0) {
            out.insert(file.name().to_string());
        }
    }
    Some(out)
}

/// The files of a zip [`zip`] wrote, or `None` when it does not read back
/// as one — truncated, corrupt, or naming a path outside its directory.
#[must_use]
pub fn unzip(bytes: &[u8]) -> Option<Entries> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).ok()?;
    let mut entries = Vec::with_capacity(archive.len());
    for i in 0..archive.len() {
        let mut file = archive.by_index(i).ok()?;
        if file.is_dir() {
            continue;
        }
        let name = file.name().to_string();
        let safe = !name.is_empty()
            && !name.starts_with('/')
            && !name.contains('\\')
            && !name.contains(':')
            && name
                .split('/')
                .all(|c| !c.is_empty() && c != "." && c != "..");
        if !safe {
            return None;
        }
        let mut contents = Vec::new();
        file.read_to_end(&mut contents).ok()?;
        entries.push((name, contents));
    }
    Some(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::{CharsetChoice, Geometry, UiOptions, When};

    struct Tree {
        root: PathBuf,
    }

    impl Tree {
        fn new(name: &str) -> Tree {
            let root =
                std::env::temp_dir().join(format!("jrs-build-cache-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap();
            Tree { root }
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn ui() -> Ui {
        Ui::captured(
            UiOptions {
                progress: When::Never,
                color: When::Never,
                charset: CharsetChoice::Ascii,
                ..Default::default()
            },
            Geometry {
                width: 100,
                height: 24,
            },
        )
        .0
    }

    fn cache(tree: &Tree) -> BuildCache {
        BuildCache {
            local: tree.root.join("cache/build"),
            root: tree.root.join("app").display().to_string(),
            cache_root: tree.root.join("cache").display().to_string(),
            jdk: "release test".to_string(),
            jars: HashMap::new(),
            hashed: Mutex::new(HashMap::new()),
            keys: Mutex::new(HashMap::new()),
            remote: None,
            verify: false,
        }
    }

    fn entries() -> Entries {
        vec![
            ("com/example/B.class".to_string(), b"b".to_vec()),
            ("com/example/A.class".to_string(), b"a".to_vec()),
        ]
    }

    #[test]
    fn an_entry_is_a_deterministic_zip_that_reads_back_sorted() {
        let first = zip(&entries()).unwrap();
        let mut reversed = entries();
        reversed.reverse();
        assert_eq!(first, zip(&reversed).unwrap(), "byte for byte");
        let back = unzip(&first).unwrap();
        assert_eq!(back[0].0, "com/example/A.class");
        assert_eq!(back[1], ("com/example/B.class".to_string(), b"b".to_vec()));
        assert!(
            unzip(&first[..first.len() / 2]).is_none(),
            "a truncated entry"
        );
        assert!(unzip(b"not a zip").is_none());
        let escaping = zip(&vec![("../evil.class".to_string(), b"x".to_vec())]).unwrap();
        assert!(unzip(&escaping).is_none(), "a path outside the directory");
    }

    #[test]
    fn a_task_entry_keeps_the_execute_bit_and_stays_deterministic() {
        let executable = HashSet::from(["0/bin/run".to_string()]);
        let mut files = entries();
        files.push(("0/bin/run".to_string(), b"#!/bin/sh".to_vec()));
        let bytes = zip_with_modes(&files, &executable).unwrap();
        files.reverse();
        assert_eq!(bytes, zip_with_modes(&files, &executable).unwrap());
        assert_eq!(executables(&bytes).unwrap(), executable);
        assert!(executables(&zip(&files).unwrap()).unwrap().is_empty());
        assert_eq!(unzip(&bytes).unwrap().len(), 3);
    }

    #[test]
    fn keys_hold_no_path_of_the_checkout() {
        let tree = Tree::new("keys");
        let cache = cache(&tree);
        let flags = format!(
            "-d {}/target/classes -cp {}/org/x/1.0/x-1.0.jar",
            tree.root.join("app").display(),
            tree.root.join("cache").display()
        );
        assert_eq!(
            cache.relative(&flags),
            "-d {root}/target/classes -cp {cache}/org/x/1.0/x-1.0.jar"
        );
        assert_ne!(cache.key("compile", "a"), cache.key("compile", "b"));
        assert_ne!(cache.key("compile", "a"), cache.key("test", "a"));

        let jar = tree.root.join("local.jar");
        std::fs::write(&jar, "one").unwrap();
        let resolved = tree.root.join("resolved.jar");
        let cache = cache.with_jars(HashMap::from([(
            resolved.clone(),
            "org:x:1.0 abc".to_string(),
        )]));
        assert_eq!(cache.jar(&resolved).unwrap(), "org:x:1.0 abc");
        assert_eq!(
            cache.jar(&jar).unwrap(),
            format!("sha256:{}", sha256_hex(b"one"))
        );
        assert!(cache.jar(&tree.root.join("missing.jar")).is_none());
    }

    #[test]
    fn a_stored_entry_loads_and_a_corrupt_one_is_a_miss() {
        let tree = Tree::new("store");
        let cache = cache(&tree);
        let ui = ui();
        let key = cache.key("compile", "x");
        assert!(cache.load(&key, &ui).is_none());
        cache.save(&key, &entries(), &ui);
        let path = cache.local_path(&key);
        assert!(path.is_file());
        assert_eq!(cache.load(&key, &ui).unwrap().len(), 2);

        std::fs::write(&path, "garbage").unwrap();
        assert!(cache.load(&key, &ui).is_none());
        assert!(
            !path.exists(),
            "a corrupt entry is dropped, to be stored again"
        );
    }

    #[test]
    fn a_file_remote_serves_a_miss_and_is_written_only_when_pushing() {
        let tree = Tree::new("remote");
        let ui = ui();
        let remote_dir = tree.root.join("remote");
        std::fs::create_dir_all(&remote_dir).unwrap();
        let config = |push: bool| BuildCacheConfig {
            url: Some(crate::resolve::repo::file_url(&remote_dir)),
            push,
            ..BuildCacheConfig::default()
        };
        let reader = cache(&tree).with_remote(Remote::new(&config(false), None, None).unwrap());
        let key = reader.key("compile", "x");
        reader.save(&key, &entries(), &ui);
        assert!(
            !remote_dir.join(format!("{key}.zip")).exists(),
            "never written without push"
        );

        let pusher = BuildCache {
            local: tree.root.join("pusher/build"),
            ..cache(&tree)
        }
        .with_remote(Remote::new(&config(true), None, None).unwrap());
        pusher.save(&key, &entries(), &ui);
        assert!(remote_dir.join(format!("{key}.zip")).is_file());

        let fresh = BuildCache {
            local: tree.root.join("fresh/build"),
            ..cache(&tree)
        }
        .with_remote(Remote::new(&config(false), None, None).unwrap());
        assert_eq!(fresh.load(&key, &ui).unwrap().len(), 2);
        assert!(fresh.local_path(&key).is_file(), "kept locally");
    }

    #[test]
    fn collect_leaves_out_what_it_is_told_and_extract_empties_first() {
        let tree = Tree::new("collect");
        let dir = tree.root.join("classes");
        std::fs::create_dir_all(dir.join("com")).unwrap();
        std::fs::write(dir.join("com/A.class"), "a").unwrap();
        std::fs::write(dir.join("app.properties"), "resource").unwrap();
        let left_out = HashSet::from(["app.properties".to_string()]);
        let collected = collect(&dir, &left_out).unwrap();
        assert_eq!(collected, vec![("com/A.class".to_string(), b"a".to_vec())]);

        std::fs::write(dir.join("Stale.class"), "old").unwrap();
        extract(&collected, &dir).unwrap();
        assert!(!dir.join("Stale.class").exists());
        assert_eq!(std::fs::read(dir.join("com/A.class")).unwrap(), b"a");
    }
}
