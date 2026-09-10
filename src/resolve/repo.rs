//! Repository access: URL layout, fetching, checksum verification.
//!
//! A [`Fetcher`] tries the cache first, then each configured repository in
//! order, with Maven Central last (SPEC §4.2). `file://` URLs are supported, so
//! integration tests can run against a repository fixture on disk and stay
//! hermetic (SPEC §10.1).

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use sha1::Digest;

use super::cache::Cache;
use super::coord::Coord;
use crate::error::{IoResultExt, JrsError, Result};
use crate::manifest::Repository;

/// Jars larger than this are almost certainly a misconfigured mirror serving an
/// HTML error page or a tarball; refuse rather than fill the disk.
const MAX_ARTIFACT_BYTES: u64 = 512 * 1024 * 1024;

/// Where a byte-count for the live download bars is published.
///
/// Implemented over the UI in `resolve::mod`; the fetcher itself knows nothing
/// about terminals.
pub trait TransferReporter: Send + Sync {
    /// Announce a transfer, returning the id used for later updates. The length
    /// is not known until the response headers arrive, so it comes separately.
    fn start(&self, name: &str) -> u64;
    fn set_total(&self, id: u64, total: Option<u64>);
    fn advance(&self, id: u64, bytes: u64);
    /// Bytes are in; the checksum is being checked.
    fn verifying(&self, id: u64);
    fn finish(&self, id: u64);
}

/// The reporter used when nothing is watching.
pub struct SilentReporter;

impl TransferReporter for SilentReporter {
    fn start(&self, _name: &str) -> u64 {
        0
    }
    fn set_total(&self, _id: u64, _total: Option<u64>) {}
    fn advance(&self, _id: u64, _bytes: u64) {}
    fn verifying(&self, _id: u64) {}
    fn finish(&self, _id: u64) {}
}

/// Whether an artifact came off the disk or the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Cache,
    Network,
}

pub struct Fetcher {
    agent: ureq::Agent,
    repos: Vec<Repository>,
    cache: Cache,
    offline: bool,
    reporter: Box<dyn TransferReporter>,
    downloaded: AtomicU64,
    warnings: Mutex<Vec<String>>,
}

impl Fetcher {
    pub fn new(repos: Vec<Repository>, cache: Cache, offline: bool) -> Fetcher {
        Fetcher::with_reporter(repos, cache, offline, Box::new(SilentReporter))
    }

    pub fn with_reporter(
        repos: Vec<Repository>,
        cache: Cache,
        offline: bool,
        reporter: Box<dyn TransferReporter>,
    ) -> Fetcher {
        let config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .user_agent(concat!("jrs/", env!("CARGO_PKG_VERSION")))
            .timeout_connect(Some(Duration::from_secs(30)))
            .timeout_global(Some(Duration::from_secs(600)))
            .build();
        Fetcher {
            agent: ureq::Agent::new_with_config(config),
            repos,
            cache,
            offline,
            reporter,
            downloaded: AtomicU64::new(0),
            warnings: Mutex::new(Vec::new()),
        }
    }

    pub fn cache(&self) -> &Cache {
        &self.cache
    }

    /// How many artifacts this fetcher pulled over the network.
    pub fn downloaded(&self) -> u64 {
        self.downloaded.load(Ordering::Relaxed)
    }

    pub fn take_warnings(&self) -> Vec<String> {
        std::mem::take(&mut self.warnings.lock().unwrap())
    }

    fn warn(&self, msg: String) {
        let mut w = self.warnings.lock().unwrap();
        if !w.contains(&msg) {
            w.push(msg);
        }
    }

    /// A POM's bytes. Small, so they are read whole and not reported on.
    pub fn pom(&self, coord: &Coord) -> Result<Vec<u8>> {
        let (path, _) = self.artifact(coord, "pom", false)?;
        std::fs::read(&path).path(&path)
    }

    /// A jar's cached path, downloading it with progress if it is not there yet.
    pub fn jar(&self, coord: &Coord) -> Result<(PathBuf, Origin)> {
        self.artifact(coord, "jar", true)
    }

    fn artifact(&self, coord: &Coord, ext: &str, report: bool) -> Result<(PathBuf, Origin)> {
        let cached = self.cache.path_for(coord, ext);
        if cached.is_file() {
            return Ok((cached, Origin::Cache));
        }
        if self.offline {
            return Err(JrsError::resolve(format!(
                "`{coord}` ({ext}) is not in the local cache and --offline was given\n\
                 expected at {}",
                cached.display()
            )));
        }

        let id = if report {
            self.reporter.start(&coord.file_name(ext))
        } else {
            0
        };

        let mut misses = Vec::new();
        for repo in &self.repos {
            match self.fetch_one(repo, coord, ext, report, id) {
                Ok(Some(bytes)) => {
                    if report {
                        self.reporter.verifying(id);
                    }
                    self.verify(repo, coord, ext, &bytes)?;
                    let path = self.cache.store(coord, ext, &bytes)?;
                    if ext == "jar" {
                        // POMs and checksum files are traffic, not artifacts;
                        // the summary counts what ends up on the classpath.
                        self.downloaded.fetch_add(1, Ordering::Relaxed);
                    }
                    if report {
                        self.reporter.finish(id);
                    }
                    return Ok((path, Origin::Network));
                }
                Ok(None) => misses.push(repo.name.clone()),
                Err(e) => {
                    if report {
                        self.reporter.finish(id);
                    }
                    return Err(e);
                }
            }
        }

        if report {
            self.reporter.finish(id);
        }
        Err(JrsError::resolve(format!(
            "could not find `{coord}` ({ext})\n\nlooked in: {}",
            misses.join(", ")
        )))
    }

    /// `Ok(None)` means "this repository does not have it"; keep looking.
    fn fetch_one(
        &self,
        repo: &Repository,
        coord: &Coord,
        ext: &str,
        report: bool,
        id: u64,
    ) -> Result<Option<Vec<u8>>> {
        let path = coord.repo_path(ext);
        if let Some(base) = local_repo_root(&repo.url) {
            let file = base.join(path.replace('/', std::path::MAIN_SEPARATOR_STR));
            return match std::fs::read(&file) {
                Ok(bytes) => {
                    if report {
                        self.reporter.advance(id, bytes.len() as u64);
                    }
                    Ok(Some(bytes))
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(JrsError::io(&file, e)),
            };
        }

        let url = format!("{}/{}", repo.url.trim_end_matches('/'), path);
        let mut response = self
            .agent
            .get(&url)
            .call()
            .map_err(|e| JrsError::resolve(format!("{url}\n\n{e}")))?;
        let status = response.status().as_u16();
        match status {
            200 => {}
            404 | 410 => return Ok(None),
            _ => {
                return Err(JrsError::resolve(format!(
                    "{url}\n\nrepository `{}` answered HTTP {status}",
                    repo.name
                )));
            }
        }

        let total = response.body().content_length();
        if report {
            // The bar starts indeterminate and gains its total here, once the
            // response headers have been read.
            self.reporter.set_total(id, total);
        }
        if let Some(len) = total
            && len > MAX_ARTIFACT_BYTES
        {
            return Err(JrsError::resolve(format!(
                "{url}\n\nrefusing a {len}-byte artifact; that is not a jar"
            )));
        }

        let mut reader = response
            .body_mut()
            .with_config()
            .limit(MAX_ARTIFACT_BYTES)
            .reader();
        let mut out = Vec::with_capacity(total.unwrap_or(64 * 1024) as usize);
        let mut chunk = vec![0u8; 64 * 1024];
        loop {
            let n = std::io::Read::read(&mut reader, &mut chunk)
                .map_err(|e| JrsError::resolve(format!("{url}\n\n{e}")))?;
            if n == 0 {
                break;
            }
            out.extend_from_slice(&chunk[..n]);
            if report {
                self.reporter.advance(id, n as u64);
            }
        }
        Ok(Some(out))
    }

    /// Check the artifact against its published `.sha1` (or `.sha256`).
    ///
    /// A mismatch deletes nothing — the bytes are not in the cache yet — and
    /// fails loudly. A *missing* checksum is a warning: some internal mirrors
    /// do not publish them, and refusing to build would be worse than saying so.
    fn verify(&self, repo: &Repository, coord: &Coord, ext: &str, bytes: &[u8]) -> Result<()> {
        for (suffix, compute) in [
            ("sha1", sha1_hex as fn(&[u8]) -> String),
            ("sha256", sha256_hex as fn(&[u8]) -> String),
        ] {
            let checksum_ext = format!("{ext}.{suffix}");
            let published = match self.fetch_one(repo, coord, &checksum_ext, false, 0) {
                Ok(Some(raw)) => raw,
                // A checksum file that will not download is not a reason to fail
                // the build; it is a reason to say the artifact went unverified.
                Ok(None) | Err(_) => continue,
            };
            let Some(expected) = parse_checksum(&published) else {
                continue;
            };
            let actual = compute(bytes);
            if actual.eq_ignore_ascii_case(&expected) {
                return Ok(());
            }
            self.cache.evict(coord, ext);
            return Err(JrsError::resolve(format!(
                "checksum mismatch for `{coord}` ({ext}) from `{}`\n\n  \
                 expected {suffix} {expected}\n  got      {suffix} {actual}\n\n\
                 the download was discarded; the repository or the connection \
                 cannot be trusted",
                repo.name
            )));
        }
        self.warn(format!(
            "`{coord}` ({ext}) has no published checksum in `{}`; it was not verified",
            repo.name
        ));
        Ok(())
    }
}

/// The filesystem root behind a `file://` URL, if that is what this is.
fn local_repo_root(url: &str) -> Option<PathBuf> {
    let rest = url.strip_prefix("file://")?;
    // `file:///abs/path` leaves a leading slash; `file://./rel` does not.
    Some(PathBuf::from(if rest.is_empty() { "/" } else { rest }))
}

/// Maven checksum files are a hex digest, sometimes followed by a filename.
fn parse_checksum(raw: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(raw);
    let token = text.split_whitespace().next()?;
    let hex: String = token
        .chars()
        .take_while(|c| c.is_ascii_hexdigit())
        .collect();
    if hex.len() == 40 || hex.len() == 64 {
        Some(hex)
    } else {
        None
    }
}

pub fn sha1_hex(bytes: &[u8]) -> String {
    let mut h = sha1::Sha1::new();
    h.update(bytes);
    hex(&h.finalize())
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = sha2::Sha256::new();
    h.update(bytes);
    hex(&h.finalize())
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Build a `file://` URL for `dir` — used by tests and by `--offline` fixtures.
pub fn file_url(dir: &Path) -> String {
    format!("file://{}", dir.display())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        dir: PathBuf,
    }

    impl Fixture {
        fn new(name: &str) -> Fixture {
            let dir =
                std::env::temp_dir().join(format!("jrs-repo-test-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Fixture { dir }
        }

        fn repo(&self) -> PathBuf {
            self.dir.join("repo")
        }

        fn cache(&self) -> Cache {
            Cache::with_root(self.dir.join("cache"))
        }

        fn publish(&self, coord: &Coord, ext: &str, bytes: &[u8], checksum: bool) {
            let path = self.repo().join(coord.repo_path(ext));
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, bytes).unwrap();
            if checksum {
                std::fs::write(path.with_extension(format!("{ext}.sha1")), sha1_hex(bytes))
                    .unwrap();
            }
        }

        fn fetcher(&self, offline: bool) -> Fetcher {
            Fetcher::new(
                vec![Repository {
                    name: "fixture".into(),
                    url: file_url(&self.repo()),
                }],
                self.cache(),
                offline,
            )
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn a_file_repository_is_fetched_and_cached() {
        let fx = Fixture::new("file-repo");
        let coord = Coord::new("org.example", "thing", "1.0");
        fx.publish(&coord, "jar", b"jar bytes", true);

        let fetcher = fx.fetcher(false);
        let (path, origin) = fetcher.jar(&coord).unwrap();
        assert_eq!(origin, Origin::Network);
        assert_eq!(std::fs::read(&path).unwrap(), b"jar bytes");
        assert_eq!(fetcher.downloaded(), 1);

        // Second time it comes off the disk.
        let (_, origin) = fetcher.jar(&coord).unwrap();
        assert_eq!(origin, Origin::Cache);
        assert_eq!(fetcher.downloaded(), 1);
    }

    #[test]
    fn a_bad_checksum_fails_loudly_and_stores_nothing() {
        let fx = Fixture::new("bad-checksum");
        let coord = Coord::new("org.example", "thing", "1.0");
        fx.publish(&coord, "jar", b"jar bytes", false);
        std::fs::write(
            fx.repo().join(coord.repo_path("jar.sha1")),
            "0000000000000000000000000000000000000000",
        )
        .unwrap();

        let fetcher = fx.fetcher(false);
        let err = fetcher.jar(&coord).unwrap_err().to_string();
        assert!(err.contains("checksum mismatch"), "{err}");
        assert!(!fetcher.cache().contains(&coord, "jar"));
    }

    #[test]
    fn a_missing_checksum_warns_but_builds() {
        let fx = Fixture::new("no-checksum");
        let coord = Coord::new("org.example", "thing", "1.0");
        fx.publish(&coord, "jar", b"jar bytes", false);

        let fetcher = fx.fetcher(false);
        fetcher.jar(&coord).unwrap();
        let warnings = fetcher.take_warnings();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("not verified"), "{warnings:?}");
    }

    #[test]
    fn offline_uses_the_cache_and_errors_on_a_miss() {
        let fx = Fixture::new("offline");
        let coord = Coord::new("org.example", "thing", "1.0");
        fx.publish(&coord, "jar", b"jar bytes", true);

        let err = fx.fetcher(true).jar(&coord).unwrap_err().to_string();
        assert!(err.contains("--offline"), "{err}");

        fx.fetcher(false).jar(&coord).unwrap();
        let (_, origin) = fx.fetcher(true).jar(&coord).unwrap();
        assert_eq!(origin, Origin::Cache);
    }

    #[test]
    fn a_missing_artifact_names_every_repository_tried() {
        let fx = Fixture::new("missing");
        let coord = Coord::new("org.example", "absent", "1.0");
        let err = fx.fetcher(false).jar(&coord).unwrap_err().to_string();
        assert!(err.contains("could not find"), "{err}");
        assert!(err.contains("fixture"), "{err}");
    }

    #[test]
    fn repositories_are_tried_in_order() {
        let fx = Fixture::new("order");
        let coord = Coord::new("org.example", "thing", "1.0");
        fx.publish(&coord, "jar", b"from second", true);

        let empty = fx.dir.join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        let fetcher = Fetcher::new(
            vec![
                Repository {
                    name: "first".into(),
                    url: file_url(&empty),
                },
                Repository {
                    name: "second".into(),
                    url: file_url(&fx.repo()),
                },
            ],
            fx.cache(),
            false,
        );
        let (path, _) = fetcher.jar(&coord).unwrap();
        assert_eq!(std::fs::read(path).unwrap(), b"from second");
    }

    #[test]
    fn checksum_files_with_a_trailing_filename_parse() {
        let digest = "a9993e364706816aba3e25717850c26c9cd0d89d";
        assert_eq!(parse_checksum(digest.as_bytes()).unwrap(), digest);
        assert_eq!(
            parse_checksum(format!("{digest}  thing-1.0.jar\n").as_bytes()).unwrap(),
            digest
        );
        assert_eq!(parse_checksum(b"<html>not a checksum</html>"), None);
    }

    #[test]
    fn known_digests_match() {
        assert_eq!(sha1_hex(b"abc"), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
