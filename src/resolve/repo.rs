//! Repository access: URL layout, fetching, checksum verification.
//!
//! A [`Fetcher`] tries the cache first, then each configured repository in
//! order, with Maven Central last (SPEC §4.2). `file://` URLs are supported, so
//! integration tests can run against a repository fixture on disk and stay
//! hermetic (SPEC §10.1).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use sha1::Digest;

use super::cache::Cache;
use super::coord::Coord;
use super::metadata::Metadata;
use crate::config::{Credentials, ProxyConfig};
use crate::error::{IoResultExt, JrsError, Result};
use crate::manifest::Repository;

/// How a fetcher reaches remote repositories, beyond their URLs. Built from the
/// user's configuration file and environment, never from `jrs.toml`.
#[derive(Debug, Clone)]
pub struct Network {
    /// Credentials by repository name.
    pub credentials: BTreeMap<String, Credentials>,
    /// An explicit proxy. `None` leaves ureq's own reading of `HTTPS_PROXY`,
    /// `HTTP_PROXY`, `ALL_PROXY` and `NO_PROXY` in charge.
    pub proxy: Option<ProxyConfig>,
    /// Tries per request, the first one included.
    pub attempts: u32,
    /// The wait before the first retry; it doubles for each one after.
    pub backoff: Duration,
}

impl Default for Network {
    fn default() -> Self {
        Network {
            credentials: BTreeMap::new(),
            proxy: None,
            attempts: 3,
            backoff: Duration::from_millis(500),
        }
    }
}

/// Jars larger than this are almost certainly a misconfigured mirror serving an
/// HTML error page or a tarball; refuse rather than fill the disk.
const MAX_ARTIFACT_BYTES: u64 = 512 * 1024 * 1024;

/// How long a cached snapshot is trusted before its repository is asked again.
const SNAPSHOT_RECHECK: Duration = Duration::from_secs(24 * 60 * 60);

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
    /// A failed attempt is being retried; the byte count starts over.
    fn restart(&self, _id: u64) {}
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
    network: Network,
    /// Re-check every cached snapshot, whatever its age (`jrs update`).
    refresh_snapshots: bool,
}

/// An HTTP agent that reports statuses as values, through `proxy` when given.
fn build_agent(proxy: Option<&ProxyConfig>) -> Result<ureq::Agent> {
    let mut config = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .user_agent(concat!("jrs/", env!("CARGO_PKG_VERSION")))
        .timeout_connect(Some(Duration::from_secs(30)))
        .timeout_global(Some(Duration::from_secs(600)));
    if let Some(proxy) = proxy {
        config = config.proxy(Some(build_proxy(proxy)?));
    }
    Ok(ureq::Agent::new_with_config(config.build()))
}

fn build_proxy(proxy: &ProxyConfig) -> Result<ureq::Proxy> {
    // The URL may carry a password, so it is never echoed back.
    let bad = |e: ureq::Error| JrsError::usage(format!("the configured proxy is not usable: {e}"));
    let parsed = ureq::Proxy::new(&proxy.url).map_err(bad)?;
    let mut builder = ureq::Proxy::builder(parsed.protocol())
        .host(parsed.host())
        .port(parsed.port());
    if let Some(username) = parsed.username() {
        builder = builder.username(username);
    }
    if let Some(password) = parsed.password() {
        builder = builder.password(password);
    }
    for host in &proxy.no_proxy {
        builder = builder.no_proxy(host);
    }
    builder.build().map_err(bad)
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
        Fetcher {
            // Without a proxy to parse there is nothing that can fail.
            agent: build_agent(None).unwrap_or_else(|_| ureq::Agent::new_with_defaults()),
            repos,
            cache,
            offline,
            reporter,
            downloaded: AtomicU64::new(0),
            warnings: Mutex::new(Vec::new()),
            network: Network::default(),
            refresh_snapshots: false,
        }
    }

    /// Ask the repositories about every cached snapshot again, however recently
    /// it was checked.
    pub fn refreshing_snapshots(mut self, refresh: bool) -> Fetcher {
        self.refresh_snapshots = refresh;
        self
    }

    /// Reach remote repositories with `network`'s proxy, credentials and retry
    /// policy.
    pub fn with_network(mut self, network: Network) -> Result<Fetcher> {
        self.agent = build_agent(network.proxy.as_ref())?;
        self.network = network;
        Ok(self)
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
        let (path, _) = self.artifact(&coord.pom_coord(), "pom", false, None)?;
        std::fs::read(&path).path(&path)
    }

    /// A jar's cached path, downloading it with progress if it is not there yet.
    pub fn jar(&self, coord: &Coord) -> Result<(PathBuf, Origin)> {
        self.artifact(coord, "jar", true, None)
    }

    /// Like [`Fetcher::jar`], but a download must also match `pin` — the
    /// `sha1:<hex>` / `sha256:<hex>` checksum `jrs.lock` recorded for it.
    ///
    /// The bytes are already in memory, so this costs one hash, and it turns the
    /// lockfile from a record into an integrity pin: a repository that starts
    /// serving different bytes under the same coordinate fails the build. A jar
    /// already in the cache is not re-hashed; `jrs verify` does that on demand.
    pub fn jar_pinned(&self, coord: &Coord, pin: Option<&str>) -> Result<(PathBuf, Origin)> {
        self.artifact(coord, "jar", true, pin)
    }

    fn artifact(
        &self,
        coord: &Coord,
        ext: &str,
        report: bool,
        pin: Option<&str>,
    ) -> Result<(PathBuf, Origin)> {
        let cached = self.cache.path_for(coord, ext);
        if cached.is_file() {
            if !coord.is_snapshot() || self.offline || !self.snapshot_due(coord, ext) {
                super::cache::mark_used(&cached);
                return Ok((cached, Origin::Cache));
            }
            // A snapshot is republished under the same name by design, so a
            // cached one is only trusted until it is due for a re-check. A
            // repository that cannot be reached is not a reason to fail a build
            // that has a perfectly usable copy.
            return self.download(coord, ext, report, None).or_else(|e| {
                let reason = e.to_string();
                self.warn(format!(
                    "could not re-check the snapshot `{coord}` ({ext}); using the \
                         cached copy: {}",
                    reason.lines().next().unwrap_or_default()
                ));
                Ok((cached, Origin::Cache))
            });
        }
        if self.offline {
            return Err(JrsError::resolve(format!(
                "`{coord}` ({ext}) is not in the local cache and --offline was given\n\
                 expected at {}",
                cached.display()
            )));
        }
        self.download(coord, ext, report, pin.filter(|_| !coord.is_snapshot()))
    }

    /// Fetch from the first repository that has it, verify, and cache.
    fn download(
        &self,
        coord: &Coord,
        ext: &str,
        report: bool,
        pin: Option<&str>,
    ) -> Result<(PathBuf, Origin)> {
        let id = if report {
            self.reporter.start(&coord.file_name(ext))
        } else {
            0
        };
        let result = self.download_reported(coord, ext, report, id, pin);
        if report {
            self.reporter.finish(id);
        }
        result
    }

    fn download_reported(
        &self,
        coord: &Coord,
        ext: &str,
        report: bool,
        id: u64,
        pin: Option<&str>,
    ) -> Result<(PathBuf, Origin)> {
        let cached = self.cache.path_for(coord, ext);
        let mut misses = Vec::new();
        for repo in &self.repos {
            // A snapshot in a remote repository is stored under a timestamped
            // name that only its `maven-metadata.xml` knows.
            let timestamped = if coord.is_snapshot() {
                self.snapshot_file_version(repo, coord, ext)?
            } else {
                None
            };
            if let Some(v) = &timestamped
                && cached.is_file()
                && self
                    .cache
                    .snapshot_record(coord, ext)
                    .is_some_and(|r| r.file_version == *v)
            {
                // The build the cache holds is still the current one.
                self.cache.record_snapshot(coord, ext, v, &repo.name)?;
                return Ok((cached, Origin::Cache));
            }
            let file_version = timestamped.as_deref().unwrap_or(&coord.version);
            let remote = coord.repo_path_as(ext, file_version);

            let Some(bytes) = self.fetch_one(repo, &remote, report, id)? else {
                misses.push(repo.name.clone());
                continue;
            };
            if report {
                self.reporter.verifying(id);
            }
            self.verify(repo, coord, ext, &remote, &bytes)?;
            check_pin(coord, ext, pin, &bytes)?;

            if coord.is_snapshot() {
                // Rewriting identical bytes would move the jar's mtime, and with
                // it the compile fingerprint, for nothing.
                let unchanged = std::fs::read(&cached).is_ok_and(|old| old == bytes);
                let path = if unchanged {
                    cached.clone()
                } else {
                    self.cache.store(coord, ext, &bytes)?
                };
                self.cache
                    .record_snapshot(coord, ext, file_version, &repo.name)?;
                if unchanged {
                    return Ok((path, Origin::Cache));
                }
            } else {
                self.cache.store(coord, ext, &bytes)?;
            }
            if ext == "jar" {
                // POMs and checksum files are traffic, not artifacts; the
                // summary counts what ends up on the classpath.
                self.downloaded.fetch_add(1, Ordering::Relaxed);
            }
            return Ok((cached, Origin::Network));
        }

        Err(JrsError::resolve(format!(
            "could not find `{coord}` ({ext})\n\nlooked in: {}",
            misses.join(", ")
        )))
    }

    /// Whether a cached snapshot should be checked against its repository again:
    /// once a day, which is Maven's default update policy; on every build when
    /// it came from a local directory, where checking is free and `mvn install`
    /// is expected to show up at once; and whenever `jrs update` asks.
    fn snapshot_due(&self, coord: &Coord, ext: &str) -> bool {
        if self.refresh_snapshots {
            return true;
        }
        let Some(record) = self.cache.snapshot_record(coord, ext) else {
            return true;
        };
        let local = self
            .repos
            .iter()
            .any(|r| r.name == record.repo && local_repo_root(&r.url).is_some());
        let age = record.checked.elapsed().unwrap_or(Duration::MAX);
        local || age > SNAPSHOT_RECHECK
    }

    /// The version in the file name of `coord`'s current snapshot build in
    /// `repo`, or `None` for the plain `-SNAPSHOT` name.
    fn snapshot_file_version(
        &self,
        repo: &Repository,
        coord: &Coord,
        ext: &str,
    ) -> Result<Option<String>> {
        let path = format!("{}/maven-metadata.xml", coord.version_dir());
        let Some(bytes) = self.fetch_one(repo, &path, false, 0)? else {
            return Ok(None);
        };
        let classifier = coord.classifier.as_deref().filter(|_| ext != "pom");
        Ok(Metadata::parse(&bytes)
            .ok()
            .and_then(|m| m.snapshot_file_version(&coord.version, ext, classifier)))
    }

    /// Every version the repositories list for `group:artifact`, merged.
    ///
    /// Never cached: the answer changes whenever anything is published, and the
    /// commands that ask (`jrs outdated`, `jrs add`) are asking precisely that.
    pub fn metadata(&self, group: &str, artifact: &str) -> Result<Metadata> {
        if self.offline {
            return Err(JrsError::resolve(format!(
                "cannot ask a repository which versions of `{group}:{artifact}` exist: \
                 --offline was given"
            )));
        }
        let dir = format!("{}/{artifact}", group.replace('.', "/"));
        let mut merged: Option<Metadata> = None;
        let mut looked = Vec::new();
        for repo in &self.repos {
            looked.push(repo.name.clone());
            // `mvn install` writes the local variant of the file.
            let mut names = vec!["maven-metadata.xml"];
            if local_repo_root(&repo.url).is_some() {
                names.push("maven-metadata-local.xml");
            }
            for name in names {
                let Some(bytes) = self.fetch_one(repo, &format!("{dir}/{name}"), false, 0)? else {
                    continue;
                };
                let Ok(found) = Metadata::parse(&bytes) else {
                    self.warn(format!(
                        "`{}` serves an unreadable {name} for `{group}:{artifact}`",
                        repo.name
                    ));
                    continue;
                };
                let m = merged.get_or_insert_with(Metadata::default);
                for v in found.versions {
                    if !m.versions.contains(&v) {
                        m.versions.push(v);
                    }
                }
                m.release = m.release.take().or(found.release);
                m.latest = m.latest.take().or(found.latest);
            }
        }
        merged.ok_or_else(|| {
            JrsError::resolve(format!(
                "no repository lists versions of `{group}:{artifact}`\n\nlooked in: {}",
                looked.join(", ")
            ))
        })
    }

    /// `Ok(None)` means "this repository does not have it"; keep looking.
    fn fetch_one(
        &self,
        repo: &Repository,
        path: &str,
        report: bool,
        id: u64,
    ) -> Result<Option<Vec<u8>>> {
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
        // A dropped connection or a briefly unwell server is the commonest way a
        // CI build fails for no reason of its own, so those are retried, with a
        // backoff that doubles. Anything the repository said on purpose — a 404,
        // a 401 — is believed the first time.
        let mut attempt = 1;
        loop {
            match self.get(repo, &url, report, id) {
                Ok(found) => return Ok(found),
                Err(Attempt::Transient(_)) if attempt < self.network.attempts => {
                    if report {
                        self.reporter.restart(id);
                    }
                    std::thread::sleep(self.network.backoff * 2u32.pow(attempt - 1));
                    attempt += 1;
                }
                Err(Attempt::Transient(reason)) => {
                    let tries = if attempt > 1 {
                        format!(" (gave up after {attempt} attempts)")
                    } else {
                        String::new()
                    };
                    return Err(JrsError::resolve(format!("{url}\n\n{reason}{tries}")));
                }
                Err(Attempt::Fatal(e)) => return Err(e),
            }
        }
    }

    /// One HTTP GET. `Ok(None)` means the repository does not have it.
    fn get(
        &self,
        repo: &Repository,
        url: &str,
        report: bool,
        id: u64,
    ) -> std::result::Result<Option<Vec<u8>>, Attempt> {
        let credentials = self.network.credentials.get(&repo.name);
        let mut request = self.agent.get(url);
        if let Some(c) = credentials {
            request = request.header("Authorization", authorization(c));
        }
        let mut response = request.call().map_err(|e| {
            if is_transient(&e) {
                Attempt::Transient(e.to_string())
            } else {
                Attempt::Fatal(JrsError::resolve(format!("{url}\n\n{e}")))
            }
        })?;
        let status = response.status().as_u16();
        match status {
            200 => {}
            404 | 410 => return Ok(None),
            429 | 500 | 502 | 503 | 504 => {
                return Err(Attempt::Transient(format!(
                    "repository `{}` answered HTTP {status}",
                    repo.name
                )));
            }
            401 | 403 => {
                return Err(Attempt::Fatal(JrsError::resolve(format!(
                    "{url}\n\nrepository `{}` answered HTTP {status}\n\n{}",
                    repo.name,
                    credentials_hint(&repo.name, credentials.is_some())
                ))));
            }
            _ => {
                return Err(Attempt::Fatal(JrsError::resolve(format!(
                    "{url}\n\nrepository `{}` answered HTTP {status}",
                    repo.name
                ))));
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
            return Err(Attempt::Fatal(JrsError::resolve(format!(
                "{url}\n\nrefusing a {len}-byte artifact; that is not a jar"
            ))));
        }

        let mut reader = response
            .body_mut()
            .with_config()
            .limit(MAX_ARTIFACT_BYTES)
            .reader();
        let mut out = Vec::with_capacity(total.unwrap_or(64 * 1024) as usize);
        let mut chunk = vec![0u8; 64 * 1024];
        loop {
            // A connection that dies halfway through a body is as transient as
            // one that never opened.
            let n = std::io::Read::read(&mut reader, &mut chunk)
                .map_err(|e| Attempt::Transient(e.to_string()))?;
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
    fn verify(
        &self,
        repo: &Repository,
        coord: &Coord,
        ext: &str,
        remote: &str,
        bytes: &[u8],
    ) -> Result<()> {
        for (suffix, compute) in [
            ("sha1", sha1_hex as fn(&[u8]) -> String),
            ("sha256", sha256_hex as fn(&[u8]) -> String),
        ] {
            let published = match self.fetch_one(repo, &format!("{remote}.{suffix}"), false, 0) {
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

/// Why one HTTP attempt failed.
enum Attempt {
    /// Worth another try: the connection dropped, or the server was unwell.
    Transient(String),
    Fatal(JrsError),
}

fn is_transient(e: &ureq::Error) -> bool {
    matches!(
        e,
        ureq::Error::Io(_)
            | ureq::Error::Timeout(_)
            | ureq::Error::HostNotFound
            | ureq::Error::ConnectionFailed
            | ureq::Error::Protocol(_)
            | ureq::Error::BodyStalled
            | ureq::Error::ConnectProxyFailed(_)
    )
}

/// The `Authorization` header value for `credentials`.
fn authorization(credentials: &Credentials) -> String {
    match credentials {
        Credentials::Basic { username, password } => {
            format!(
                "Basic {}",
                base64(format!("{username}:{password}").as_bytes())
            )
        }
        Credentials::Bearer(token) => format!("Bearer {token}"),
    }
}

/// What to tell someone whose repository refused them.
fn credentials_hint(repository: &str, had_credentials: bool) -> String {
    if had_credentials {
        return format!("the credentials configured for `{repository}` were refused");
    }
    let var = crate::config::env_name(repository);
    let file = crate::config::default_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "the jrs config file".to_string());
    format!(
        "it may need credentials: set JRS_REPO_{var}_USERNAME and \
         JRS_REPO_{var}_PASSWORD (or JRS_REPO_{var}_TOKEN), or add a \
         [credentials.{repository}] table to {file}"
    )
}

/// Standard base64, with padding — all HTTP basic auth needs.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i)) as usize & 63] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Fail unless `bytes` match the lockfile's `pin`, when there is one.
fn check_pin(coord: &Coord, ext: &str, pin: Option<&str>, bytes: &[u8]) -> Result<()> {
    let Some(pin) = pin else { return Ok(()) };
    let Some(actual) = digest_as(pin, bytes) else {
        // An algorithm this jrs does not know cannot be checked; the repository
        // checksum has already been, so this is not worth failing a build over.
        return Ok(());
    };
    if actual.eq_ignore_ascii_case(pin) {
        return Ok(());
    }
    Err(JrsError::resolve(format!(
        "checksum mismatch for `{coord}` ({ext}) against jrs.lock\n\n  \
         locked {pin}\n  got    {actual}\n\n\
         the download was discarded; if the artifact was legitimately republished, \
         run `jrs update` to pin the new one"
    )))
}

/// Hash `bytes` with the algorithm `pin` names (`sha1:` or `sha256:`),
/// returning the digest in the same `<algorithm>:<hex>` form.
///
/// `None` when the prefix is not an algorithm jrs knows.
pub fn digest_as(pin: &str, bytes: &[u8]) -> Option<String> {
    let (algorithm, _) = pin.split_once(':')?;
    match algorithm {
        "sha1" => Some(format!("sha1:{}", sha1_hex(bytes))),
        "sha256" => Some(format!("sha256:{}", sha256_hex(bytes))),
        _ => None,
    }
}

/// The filesystem root behind a `file://` URL, if that is what this is.
fn local_repo_root(url: &str) -> Option<PathBuf> {
    let rest = url.strip_prefix("file://")?;
    // `file:///C:/x` names the drive path `C:/x`, not `/C:/x`.
    let bytes = rest.as_bytes();
    if bytes.len() >= 3 && bytes[0] == b'/' && bytes[1].is_ascii_alphabetic() && bytes[2] == b':' {
        return Some(PathBuf::from(&rest[1..]));
    }
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
///
/// Separators are always `/`, so the URL can sit in a TOML string: a Windows
/// path's backslashes would read as escapes there.
pub fn file_url(dir: &Path) -> String {
    let path = dir.display().to_string().replace('\\', "/");
    if path.starts_with('/') {
        format!("file://{path}")
    } else {
        // `C:/Users/...` becomes `file:///C:/Users/...`.
        format!("file:///{path}")
    }
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

    // ---- over HTTP, against a throwaway local server ----------------------

    use std::io::Write as _;
    use std::net::TcpStream;
    use std::sync::Arc;

    /// A one-thread HTTP server: each connection gets the next canned
    /// response, and every request head it sees is recorded. A `CONNECT` is
    /// accepted and the tunnelled request served on the same socket, so the
    /// server doubles as a proxy.
    struct Server {
        url: String,
        requests: Arc<Mutex<Vec<String>>>,
    }

    fn serve(responses: Vec<(u16, Vec<u8>)>) -> Server {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = requests.clone();
        std::thread::spawn(move || {
            let mut responses = responses.into_iter();
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut head = read_head(&mut stream);
                // `read_head` lower-cases what it reads.
                if head.starts_with("connect ") {
                    seen.lock().unwrap().push(head);
                    let _ = stream.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n");
                    head = read_head(&mut stream);
                }
                seen.lock().unwrap().push(head);
                let Some((status, body)) = responses.next() else {
                    break;
                };
                let reply = format!(
                    "HTTP/1.1 {status} Canned\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(reply.as_bytes());
                let _ = stream.write_all(&body);
            }
        });
        Server { url, requests }
    }

    fn read_head(stream: &mut TcpStream) -> String {
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            match std::io::Read::read(stream, &mut byte) {
                Ok(1) => head.push(byte[0]),
                _ => break,
            }
        }
        String::from_utf8_lossy(&head).to_ascii_lowercase()
    }

    impl Server {
        fn requests(&self) -> Vec<String> {
            self.requests.lock().unwrap().clone()
        }
    }

    fn http_fetcher(fx: &Fixture, url: &str, network: Network) -> Fetcher {
        Fetcher::new(
            vec![Repository {
                name: "remote".into(),
                url: url.to_string(),
            }],
            fx.cache(),
            false,
        )
        .with_network(network)
        .unwrap()
    }

    fn quick(attempts: u32) -> Network {
        Network {
            attempts,
            backoff: Duration::ZERO,
            ..Network::default()
        }
    }

    fn jar_and_checksum(bytes: &[u8]) -> Vec<(u16, Vec<u8>)> {
        vec![(200, bytes.to_vec()), (200, sha1_hex(bytes).into_bytes())]
    }

    #[test]
    fn a_transient_failure_is_retried() {
        let fx = Fixture::new("http-retry");
        let mut responses = vec![(503, Vec::new())];
        responses.extend(jar_and_checksum(b"jar bytes"));
        let server = serve(responses);

        let coord = Coord::new("org.example", "thing", "1.0");
        let (path, _) = http_fetcher(&fx, &server.url, quick(3))
            .jar(&coord)
            .unwrap();
        assert_eq!(std::fs::read(path).unwrap(), b"jar bytes");
        let requests = server.requests();
        assert_eq!(requests.len(), 3, "{requests:?}");
        assert!(requests[0].starts_with("get /org/example/thing/1.0/thing-1.0.jar "));
        assert!(!requests[0].contains("authorization"), "{}", requests[0]);
    }

    #[test]
    fn retries_are_bounded() {
        let fx = Fixture::new("http-give-up");
        let server = serve(vec![(503, Vec::new()); 3]);
        let err = http_fetcher(&fx, &server.url, quick(3))
            .jar(&Coord::new("org.example", "thing", "1.0"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("HTTP 503"), "{err}");
        assert!(err.contains("gave up after 3 attempts"), "{err}");
    }

    #[test]
    fn a_missing_artifact_is_not_retried() {
        let fx = Fixture::new("http-404");
        let server = serve(vec![(404, Vec::new()); 3]);
        let err = http_fetcher(&fx, &server.url, quick(3))
            .jar(&Coord::new("org.example", "thing", "1.0"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("could not find"), "{err}");
        assert_eq!(server.requests().len(), 1);
    }

    #[test]
    fn credentials_are_sent_to_their_repository() {
        let fx = Fixture::new("http-auth");
        let server = serve(jar_and_checksum(b"private jar"));
        let mut network = quick(1);
        network.credentials.insert(
            "remote".into(),
            Credentials::Basic {
                username: "u".into(),
                password: "p".into(),
            },
        );
        http_fetcher(&fx, &server.url, network)
            .jar(&Coord::new("org.example", "thing", "1.0"))
            .unwrap();
        for request in server.requests() {
            assert!(
                request.contains("authorization: basic dtpw\r\n"),
                "{request}"
            );
        }
    }

    #[test]
    fn a_refusal_says_where_credentials_go() {
        let fx = Fixture::new("http-401");
        let server = serve(vec![(401, Vec::new())]);
        let err = http_fetcher(&fx, &server.url, quick(3))
            .jar(&Coord::new("org.example", "thing", "1.0"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("HTTP 401"), "{err}");
        assert!(err.contains("JRS_REPO_REMOTE_USERNAME"), "{err}");
        assert!(err.contains("[credentials.remote]"), "{err}");
        assert_eq!(server.requests().len(), 1, "a refusal is not retried");
    }

    #[test]
    fn a_configured_proxy_carries_the_traffic() {
        let fx = Fixture::new("http-proxy");
        let proxy = serve(jar_and_checksum(b"proxied jar"));
        let network = Network {
            proxy: Some(ProxyConfig {
                url: proxy.url.clone(),
                no_proxy: vec![],
            }),
            ..quick(1)
        };
        // `.invalid` never resolves, so only a proxy can reach it.
        let (path, _) = http_fetcher(&fx, "http://repo.invalid/maven2", network)
            .jar(&Coord::new("org.example", "thing", "1.0"))
            .unwrap_or_else(|e| panic!("{e}\n\nthe proxy saw {:?}", proxy.requests()));
        assert_eq!(std::fs::read(path).unwrap(), b"proxied jar");
        assert!(
            proxy
                .requests()
                .iter()
                .any(|r| r.starts_with("connect repo.invalid:80")),
            "{:?}",
            proxy.requests()
        );
    }

    #[test]
    fn no_proxy_hosts_go_direct() {
        let fx = Fixture::new("http-no-proxy");
        let proxy = serve(jar_and_checksum(b"proxied jar"));
        let network = Network {
            proxy: Some(ProxyConfig {
                url: proxy.url.clone(),
                no_proxy: vec!["repo.invalid".into()],
            }),
            ..quick(1)
        };
        assert!(
            http_fetcher(&fx, "http://repo.invalid/maven2", network)
                .jar(&Coord::new("org.example", "thing", "1.0"))
                .is_err()
        );
        assert!(proxy.requests().is_empty(), "{:?}", proxy.requests());
    }

    #[test]
    fn base64_matches_the_rfc_vectors() {
        for (input, expected) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(input.as_bytes()), expected, "{input}");
        }
    }

    #[test]
    fn a_download_must_match_its_lockfile_pin() {
        let fx = Fixture::new("pinned");
        let coord = Coord::new("org.example", "thing", "1.0");
        fx.publish(&coord, "jar", b"republished bytes", true);

        // The repository's own checksum matches; the lockfile's does not.
        let pin = format!("sha1:{}", sha1_hex(b"the bytes that were locked"));
        let fetcher = fx.fetcher(false);
        let err = fetcher
            .jar_pinned(&coord, Some(&pin))
            .unwrap_err()
            .to_string();
        assert!(err.contains("against jrs.lock"), "{err}");
        assert!(err.contains("jrs update"), "{err}");
        assert!(!fetcher.cache().contains(&coord, "jar"));

        let good = format!("sha1:{}", sha1_hex(b"republished bytes"));
        fetcher.jar_pinned(&coord, Some(&good)).unwrap();
        assert!(fetcher.cache().contains(&coord, "jar"));
    }

    #[test]
    fn pins_name_their_algorithm() {
        assert_eq!(
            digest_as("sha1:whatever", b"abc").as_deref(),
            Some("sha1:a9993e364706816aba3e25717850c26c9cd0d89d")
        );
        assert!(
            digest_as("sha256:x", b"abc")
                .unwrap()
                .starts_with("sha256:ba7816bf")
        );
        assert_eq!(digest_as("md5:x", b"abc"), None);
        assert_eq!(digest_as("no-prefix", b"abc"), None);
    }

    #[test]
    fn file_urls_survive_windows_paths() {
        assert_eq!(file_url(Path::new("/tmp/repo")), "file:///tmp/repo");
        assert_eq!(
            local_repo_root("file:///tmp/repo"),
            Some(PathBuf::from("/tmp/repo"))
        );
        assert_eq!(
            local_repo_root("file:///C:/Users/ci/repo"),
            Some(PathBuf::from("C:/Users/ci/repo"))
        );
        assert_eq!(local_repo_root("https://x"), None);
    }

    fn publish_snapshot(fx: &Fixture, build: u32, bytes: &[u8]) {
        let coord = Coord::new("org.example", "thing", "1.0-SNAPSHOT");
        let stamp = format!("1.0-20240101.12000{build}-{build}");
        let path = fx.repo().join(coord.repo_path_as("jar", &stamp));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, bytes).unwrap();
        std::fs::write(path.with_extension("jar.sha1"), sha1_hex(bytes)).unwrap();
        std::fs::write(
            path.parent().unwrap().join("maven-metadata.xml"),
            format!(
                "<metadata><versioning><snapshotVersions><snapshotVersion>\
                 <extension>jar</extension><value>{stamp}</value>\
                 </snapshotVersion></snapshotVersions></versioning></metadata>"
            ),
        )
        .unwrap();
    }

    #[test]
    fn a_timestamped_snapshot_is_found_through_its_metadata() {
        let fx = Fixture::new("snapshot");
        publish_snapshot(&fx, 1, b"first build");
        let coord = Coord::new("org.example", "thing", "1.0-SNAPSHOT");

        let fetcher = fx.fetcher(false);
        let (path, origin) = fetcher.jar(&coord).unwrap();
        assert_eq!(origin, Origin::Network);
        assert_eq!(std::fs::read(&path).unwrap(), b"first build");
        assert!(
            path.ends_with("thing-1.0-SNAPSHOT.jar"),
            "{}",
            path.display()
        );

        // A new build is published. A local repository is re-checked on every
        // build, so the next fetch picks it up.
        publish_snapshot(&fx, 2, b"second build");
        let (path, origin) = fx.fetcher(false).jar(&coord).unwrap();
        assert_eq!(origin, Origin::Network);
        assert_eq!(std::fs::read(&path).unwrap(), b"second build");

        // Nothing new: the cached build is kept, and not rewritten.
        let (_, origin) = fx.fetcher(false).jar(&coord).unwrap();
        assert_eq!(origin, Origin::Cache);
    }

    #[test]
    fn a_snapshot_that_cannot_be_rechecked_falls_back_to_the_cache() {
        let fx = Fixture::new("snapshot-gone");
        publish_snapshot(&fx, 1, b"first build");
        let coord = Coord::new("org.example", "thing", "1.0-SNAPSHOT");
        fx.fetcher(false).jar(&coord).unwrap();

        std::fs::remove_dir_all(fx.repo()).unwrap();
        let fetcher = fx.fetcher(false);
        let (path, origin) = fetcher.jar(&coord).unwrap();
        assert_eq!(origin, Origin::Cache);
        assert_eq!(std::fs::read(path).unwrap(), b"first build");
        assert!(
            fetcher
                .take_warnings()
                .iter()
                .any(|w| w.contains("cached copy")),
        );
    }

    #[test]
    fn a_plainly_named_snapshot_resolves_without_metadata() {
        // What `mvn install` leaves in ~/.m2: no timestamps, no remote metadata.
        let fx = Fixture::new("snapshot-local");
        let coord = Coord::new("org.example", "thing", "1.0-SNAPSHOT");
        fx.publish(&coord, "jar", b"installed", true);
        let (path, _) = fx.fetcher(false).jar(&coord).unwrap();
        assert_eq!(std::fs::read(path).unwrap(), b"installed");
    }

    #[test]
    fn versions_are_listed_from_every_repository() {
        let fx = Fixture::new("metadata");
        let dir = fx.repo().join("org/example/thing");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("maven-metadata.xml"),
            "<metadata><versioning><release>1.1</release><versions>\
             <version>1.0</version><version>1.1</version></versions></versioning></metadata>",
        )
        .unwrap();
        let m = fx.fetcher(false).metadata("org.example", "thing").unwrap();
        assert_eq!(m.versions, vec!["1.0", "1.1"]);
        assert_eq!(m.release.as_deref(), Some("1.1"));

        let err = fx
            .fetcher(false)
            .metadata("org.example", "absent")
            .unwrap_err();
        assert!(err.to_string().contains("no repository lists"), "{err}");
        let err = fx
            .fetcher(true)
            .metadata("org.example", "thing")
            .unwrap_err();
        assert!(err.to_string().contains("--offline"), "{err}");
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
