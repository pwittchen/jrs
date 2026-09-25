//! `jrs self check` and `jrs self update`: jrs's own releases on GitHub.
//!
//! The latest release is read from where `releases/latest` redirects to, not
//! from the GitHub API: the redirect names the tag, needs no JSON, and is not
//! rate-limited per address. An update downloads the same unversioned asset
//! `website/install.sh` does, checks it against the release's `SHA256SUMS`,
//! makes sure it runs, and renames it over the running binary, so an
//! interrupted update leaves the old binary in place.

use std::cmp::Ordering;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate::config::ProxyConfig;
use crate::error::{IoResultExt, JrsError, Result};
use crate::resolve::repo::{build_proxy, sha256_hex};

/// The repository the release workflow publishes to.
pub const RELEASES: &str = "https://github.com/pwittchen/jrs/releases";

/// A release archive is a few megabytes; anything past this is not one.
const MAX_ASSET_BYTES: u64 = 256 * 1024 * 1024;

/// The version this binary was built as.
#[must_use]
pub fn current_version() -> Version {
    // Cargo guarantees a `major.minor.patch` package version.
    Version::parse(env!("CARGO_PKG_VERSION")).unwrap_or(Version {
        parts: Vec::new(),
        pre: None,
    })
}

/// A release version: numeric components compared in order, `0.10.0` after
/// `0.9.1`. A pre-release (`0.8.0-rc.1`) sorts before its release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    parts: Vec<u64>,
    pre: Option<String>,
}

impl Version {
    /// `0.7.0`, or the same with a leading `v` as tags carry it.
    #[must_use]
    pub fn parse(text: &str) -> Option<Version> {
        let text = text.trim();
        let text = text.strip_prefix('v').unwrap_or(text);
        let (release, pre) = match text.split_once('-') {
            Some((_, "")) => return None,
            Some((release, pre)) => (release, Some(pre.to_string())),
            None => (text, None),
        };
        let parts = release
            .split('.')
            .map(|p| p.parse::<u64>().ok())
            .collect::<Option<Vec<_>>>()?;
        Some(Version { parts, pre })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Version) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Version) -> Ordering {
        let len = self.parts.len().max(other.parts.len());
        let at = |v: &Version, i: usize| v.parts.get(i).copied().unwrap_or(0);
        (0..len)
            .map(|i| at(self, i).cmp(&at(other, i)))
            .find(|o| o.is_ne())
            .unwrap_or_else(|| match (&self.pre, &other.pre) {
                (None, None) => Ordering::Equal,
                (None, Some(_)) => Ordering::Greater,
                (Some(_), None) => Ordering::Less,
                (Some(a), Some(b)) => a.cmp(b),
            })
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let parts: Vec<String> = self.parts.iter().map(u64::to_string).collect();
        f.write_str(&parts.join("."))?;
        match &self.pre {
            Some(pre) => write!(f, "-{pre}"),
            None => Ok(()),
        }
    }
}

/// Where this binary stands against the latest release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Standing {
    UpToDate,
    Outdated,
    /// A development build, ahead of anything released.
    Ahead,
}

#[must_use]
pub fn standing(current: &Version, latest: &Version) -> Standing {
    match current.cmp(latest) {
        Ordering::Less => Standing::Outdated,
        Ordering::Equal => Standing::UpToDate,
        Ordering::Greater => Standing::Ahead,
    }
}

/// The release target this binary was built for: the part of the asset name
/// after `jrs-`, as the release workflow's `dist` matrix names it. `None` on a
/// platform no release is built for.
#[must_use]
pub fn target() -> Option<&'static str> {
    let arch = std::env::consts::ARCH;
    let os = std::env::consts::OS;
    target_for(os, arch)
}

fn target_for(os: &str, arch: &str) -> Option<&'static str> {
    match (os, arch) {
        // The Linux builds link musl statically, so they replace a glibc build too.
        ("linux", "x86_64") => Some("x86_64-unknown-linux-musl"),
        ("linux", "aarch64") => Some("aarch64-unknown-linux-musl"),
        ("macos", "x86_64") => Some("x86_64-apple-darwin"),
        ("macos", "aarch64") => Some("aarch64-apple-darwin"),
        ("windows", "x86_64") => Some("x86_64-pc-windows-msvc"),
        _ => None,
    }
}

/// The archive a target's release ships as.
#[must_use]
pub fn asset_name(target: &str) -> String {
    if target.contains("windows") {
        format!("jrs-{target}.zip")
    } else {
        format!("jrs-{target}.tar.gz")
    }
}

/// The name of the binary inside the archive, and on disk.
fn binary_name(target: &str) -> &'static str {
    if target.contains("windows") {
        "jrs.exe"
    } else {
        "jrs"
    }
}

/// Talks to the GitHub releases.
pub struct Releases {
    /// Follows no redirects, so `releases/latest` can be read for its target.
    lookup: ureq::Agent,
    /// Follows them, since release assets are served from another host.
    download: ureq::Agent,
}

impl Releases {
    /// # Errors
    ///
    /// [`JrsError::Usage`] when the configured proxy URL is not usable.
    pub fn new(proxy: Option<&ProxyConfig>) -> Result<Releases> {
        Ok(Releases {
            lookup: agent(proxy, 0)?,
            download: agent(proxy, 10)?,
        })
    }

    /// The latest release's version.
    ///
    /// # Errors
    ///
    /// [`JrsError::Resolve`] when GitHub cannot be reached or names no release.
    pub fn latest(&self) -> Result<Version> {
        let url = format!("{RELEASES}/latest");
        let response = self
            .lookup
            .get(&url)
            .call()
            .map_err(|e| JrsError::resolve(format!("could not reach {url}\n\n{e}")))?;
        let status = response.status().as_u16();
        let location = response
            .headers()
            .get("location")
            .and_then(|l| l.to_str().ok());
        match location.and_then(tag_of) {
            Some(version) if (300..400).contains(&status) => Ok(version),
            _ => Err(JrsError::resolve(format!(
                "{url} answered HTTP {status} without naming a release"
            ))),
        }
    }

    /// Download `target`'s archive of release `version`, check it against the
    /// release's `SHA256SUMS`, and return its bytes.
    ///
    /// # Errors
    ///
    /// [`JrsError::Resolve`] when either download fails, the release lists no
    /// checksum for the archive, or the archive does not match it.
    pub fn download(&self, version: &Version, target: &str) -> Result<Vec<u8>> {
        let base = format!("{RELEASES}/download/v{version}");
        let asset = asset_name(target);
        let archive = self.get(&format!("{base}/{asset}"))?;
        let sums = self.get(&format!("{base}/SHA256SUMS"))?;
        let sums = String::from_utf8_lossy(&sums);
        let expected = checksum_for(&sums, &asset).ok_or_else(|| {
            JrsError::resolve(format!("{base}/SHA256SUMS lists no checksum for {asset}"))
        })?;
        let actual = sha256_hex(&archive);
        if !actual.eq_ignore_ascii_case(expected) {
            return Err(JrsError::resolve(format!(
                "checksum mismatch for {asset}: expected {expected}, got {actual}"
            )));
        }
        Ok(archive)
    }

    fn get(&self, url: &str) -> Result<Vec<u8>> {
        let fail =
            |e: &dyn fmt::Display| JrsError::resolve(format!("could not download {url}\n\n{e}"));
        let mut response = self.download.get(url).call().map_err(|e| fail(&e))?;
        let status = response.status().as_u16();
        if status != 200 {
            return Err(fail(&format!("HTTP {status}")));
        }
        response
            .body_mut()
            .with_config()
            .limit(MAX_ASSET_BYTES)
            .read_to_vec()
            .map_err(|e| fail(&e))
    }
}

fn agent(proxy: Option<&ProxyConfig>, redirects: u32) -> Result<ureq::Agent> {
    let mut config = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .max_redirects(redirects)
        .user_agent(concat!("jrs/", env!("CARGO_PKG_VERSION")))
        .timeout_connect(Some(Duration::from_secs(30)))
        .timeout_global(Some(Duration::from_secs(600)));
    if let Some(proxy) = proxy {
        config = config.proxy(Some(build_proxy(proxy)?));
    }
    Ok(ureq::Agent::new_with_config(config.build()))
}

/// The version in a `…/releases/tag/v0.7.0` redirect target.
fn tag_of(location: &str) -> Option<Version> {
    let (_, tag) = location
        .trim_end_matches('/')
        .rsplit_once("/releases/tag/")?;
    Version::parse(tag)
}

/// The checksum `SHA256SUMS` lists for `file`, in `sha256sum`'s format, text
/// (`hash  name`) or binary (`hash *name`).
fn checksum_for<'a>(sums: &'a str, file: &str) -> Option<&'a str> {
    sums.lines().find_map(|line| {
        let (hash, name) = line.split_once(char::is_whitespace)?;
        let name = name.trim_start();
        let name = name.strip_prefix('*').unwrap_or(name);
        (name.trim_end() == file).then_some(hash)
    })
}

/// Unpack the binary from `archive`, a release asset for `target`, into
/// `scratch`, and return its path there.
///
/// # Errors
///
/// [`JrsError::Build`] when the archive holds no binary or cannot be read.
pub fn unpack(archive: &[u8], target: &str, scratch: &Path) -> Result<PathBuf> {
    let binary = binary_name(target);
    let out = scratch.join(binary);
    std::fs::create_dir_all(scratch).path(scratch)?;
    if target.contains("windows") {
        let bad = |e: &dyn fmt::Display| JrsError::build(format!("{}: {e}", asset_name(target)));
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(archive)).map_err(|e| bad(&e))?;
        let mut entry = zip.by_name(binary).map_err(|e| bad(&e))?;
        let mut file = std::fs::File::create(&out).path(&out)?;
        std::io::copy(&mut entry, &mut file).path(&out)?;
    } else {
        // Every macOS and Linux machine has a `tar` that reads gzip; the
        // installer relies on it too.
        let file = scratch.join(asset_name(target));
        std::fs::write(&file, archive).path(&file)?;
        let status = Command::new("tar")
            .arg("-xzf")
            .arg(&file)
            .arg("-C")
            .arg(scratch)
            .arg(binary)
            .status()
            .map_err(|e| JrsError::build(format!("could not run `tar`: {e}")))?;
        if !status.success() || !out.is_file() {
            return Err(JrsError::build(format!(
                "{} holds no `{binary}`",
                asset_name(target)
            )));
        }
    }
    Ok(out)
}

/// What `binary --version` prints, as a check that it runs on this machine.
///
/// # Errors
///
/// [`JrsError::Build`] when it does not run, or does not say it is jrs.
pub fn probe(binary: &Path) -> Result<String> {
    make_executable(binary)?;
    let output = Command::new(binary)
        .arg("--version")
        .output()
        .map_err(|e| JrsError::build(format!("the downloaded jrs does not run here: {e}")))?;
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !output.status.success() || !text.starts_with("jrs ") {
        return Err(JrsError::build(
            "the downloaded jrs does not run on this machine",
        ));
    }
    Ok(text)
}

/// Put `binary` where `exe` is. It is copied next to `exe` first and renamed
/// over it, so a failure halfway leaves `exe` as it was. Windows will not
/// replace a running executable, but will rename one: the old binary is moved
/// aside to `jrs.exe.old`, removed on the next update.
///
/// # Errors
///
/// [`JrsError::Io`] when `exe`'s directory cannot be written to.
pub fn replace(binary: &Path, exe: &Path) -> Result<()> {
    let dir = exe
        .parent()
        .ok_or_else(|| JrsError::build(format!("{} has no parent directory", exe.display())))?;
    let staged = dir.join(format!(".jrs.{}.new", std::process::id()));
    let result = (|| {
        std::fs::copy(binary, &staged).path(&staged)?;
        make_executable(&staged)?;
        if cfg!(windows) {
            let old = exe.with_extension("exe.old");
            let _ = std::fs::remove_file(&old);
            std::fs::rename(exe, &old).path(exe)?;
            if let Err(e) = std::fs::rename(&staged, exe) {
                let _ = std::fs::rename(&old, exe);
                return Err(JrsError::io(exe, e));
            }
        } else {
            std::fs::rename(&staged, exe).path(exe)?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&staged);
    }
    result
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).path(path)
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<()> {
    Ok(())
}

/// The binary running now, through any symlink to it: a link in `PATH` is
/// left alone and what it points at is updated.
///
/// # Errors
///
/// [`JrsError::Build`] when the platform cannot say where the binary is.
pub fn current_exe() -> Result<PathBuf> {
    let exe = std::env::current_exe()
        .map_err(|e| JrsError::build(format!("could not find the running jrs binary: {e}")))?;
    Ok(std::fs::canonicalize(&exe).unwrap_or(exe))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn versions_compare_numerically() {
        assert!(v("0.10.0") > v("0.9.1"));
        assert!(v("v1.0.0") > v("0.99.99"));
        assert_eq!(v("v0.7.0"), v("0.7.0"));
        assert_eq!(v("0.7").cmp(&v("0.7.0")), Ordering::Equal);
        assert_eq!(v("0.8.0-rc.1").to_string(), "0.8.0-rc.1");
        assert_eq!(v("v0.7.0").to_string(), "0.7.0");
    }

    #[test]
    fn a_pre_release_sorts_before_its_release() {
        assert!(v("0.8.0-rc.1") < v("0.8.0"));
        assert!(v("0.8.0-rc.1") > v("0.7.9"));
    }

    #[test]
    fn junk_is_not_a_version() {
        for junk in ["", "v", "latest", "1.x.0", "1..0", "1.0.0-"] {
            assert_eq!(Version::parse(junk), None, "{junk}");
        }
    }

    #[test]
    fn standing_says_which_way() {
        assert_eq!(standing(&v("0.7.0"), &v("0.8.0")), Standing::Outdated);
        assert_eq!(standing(&v("0.8.0"), &v("0.8.0")), Standing::UpToDate);
        assert_eq!(standing(&v("0.9.0"), &v("0.8.0")), Standing::Ahead);
    }

    #[test]
    fn the_latest_release_is_read_from_the_redirect() {
        assert_eq!(
            tag_of("https://github.com/pwittchen/jrs/releases/tag/v0.7.0"),
            Some(v("0.7.0"))
        );
        // No release yet: GitHub sends the releases page instead.
        assert_eq!(tag_of("https://github.com/pwittchen/jrs/releases"), None);
    }

    #[test]
    fn checksums_are_found_in_either_format() {
        let sums = "abc123  jrs-x86_64-apple-darwin.tar.gz\n\
                    def456 *jrs-x86_64-pc-windows-msvc.zip\n";
        assert_eq!(
            checksum_for(sums, "jrs-x86_64-apple-darwin.tar.gz"),
            Some("abc123")
        );
        assert_eq!(
            checksum_for(sums, "jrs-x86_64-pc-windows-msvc.zip"),
            Some("def456")
        );
        assert_eq!(checksum_for(sums, "jrs-aarch64-apple-darwin.tar.gz"), None);
    }

    #[test]
    fn targets_match_the_release_assets() {
        assert_eq!(target_for("macos", "aarch64"), Some("aarch64-apple-darwin"));
        assert_eq!(
            target_for("linux", "x86_64"),
            Some("x86_64-unknown-linux-musl")
        );
        assert_eq!(target_for("freebsd", "x86_64"), None);
        assert_eq!(
            asset_name("x86_64-pc-windows-msvc"),
            "jrs-x86_64-pc-windows-msvc.zip"
        );
        assert_eq!(
            asset_name("aarch64-apple-darwin"),
            "jrs-aarch64-apple-darwin.tar.gz"
        );
    }

    #[test]
    fn this_build_has_a_version() {
        assert_eq!(current_version().to_string(), env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn replacing_swaps_the_file_and_leaves_nothing_behind() {
        let dir = std::env::temp_dir().join(format!("jrs-selfupdate-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join(if cfg!(windows) { "jrs.exe" } else { "jrs" });
        let new = dir.join("downloaded");
        std::fs::write(&exe, b"old").unwrap();
        std::fs::write(&new, b"new").unwrap();

        replace(&new, &exe).unwrap();

        assert_eq!(std::fs::read(&exe).unwrap(), b"new");
        let staged: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with(".jrs."))
            .collect();
        assert!(staged.is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
