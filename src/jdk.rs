//! `jrs jdk install` and the `[jdk] download` step of SPEC §7.1: a pinned JDK
//! that is installed nowhere jrs looks, downloaded from its vendor.
//!
//! The vendor's API names the newest GA build of a feature version for this
//! OS and architecture, with its SHA-256. The archive is streamed to a temp
//! file in `<cache>/jdks/`, hashed as it arrives, and compared before anything
//! is unpacked; it is unpacked into a temp directory there, its home checked
//! by running `javac -version`, and only then renamed to its final name — so
//! an interrupted install leaves nothing a later lookup could pick, the rule
//! the cache's jars follow (`resolve/cache.rs`). `toolchain::installed_jdks`
//! looks in `<cache>/jdks` like anywhere else JDKs are installed, so once one
//! is there, it is found with no network and no config.
//!
//! Only Temurin, through the Adoptium API (or a mirror of it), and only
//! releases: no early-access builds and no `GraalVM` (SPEC §9.7). No crate is
//! added (SPEC §13): `ureq` and `sha2` download and check, `json.rs` reads the
//! answer, the system `tar` unpacks a `.tar.gz` as `selfupdate` does, and the
//! `zip` crate a Windows `.zip`.

use std::fmt;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use sha2::Digest;

use crate::config::{JdkConfig, ProxyConfig};
use crate::error::{IoResultExt, JrsError, Result};
use crate::json::Json;

/// The vendor v1 knows.
pub const TEMURIN: &str = "temurin";

/// The public Adoptium API, which serves Temurin.
pub const ADOPTIUM: &str = "https://api.adoptium.net";

/// Where downloaded JDKs live, under the cache root.
pub const JDKS_DIR: &str = "jdks";

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// One GA build the vendor offers for this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Build {
    /// `21.0.5+11`, as the release names it.
    pub version: String,
    /// The archive's file name.
    pub name: String,
    pub url: String,
    pub sha256: String,
    /// The archive's size, when the API says.
    pub size: Option<u64>,
    /// The API's names for this machine: `linux`/`mac`/`windows`, `x64`/`aarch64`.
    pub os: String,
    pub arch: String,
}

impl Build {
    /// The directory it is installed as: `temurin-21.0.5+11-linux-x64`.
    #[must_use]
    pub fn dir_name(&self) -> String {
        format!("{TEMURIN}-{}-{}-{}", self.version, self.os, self.arch)
    }

    /// `temurin, linux-x64`, for the phase line.
    #[must_use]
    pub fn platform(&self) -> String {
        format!("{TEMURIN}, {}-{}", self.os, self.arch)
    }
}

/// The Adoptium API's name for this OS, `None` where Temurin builds none.
#[must_use]
pub fn api_os() -> Option<&'static str> {
    api_os_for(
        std::env::consts::OS,
        Path::new("/etc/alpine-release").exists(),
    )
}

fn api_os_for(os: &str, alpine: bool) -> Option<&'static str> {
    match os {
        // Alpine's musl cannot load a glibc JDK.
        "linux" if alpine => Some("alpine-linux"),
        "linux" => Some("linux"),
        "macos" => Some("mac"),
        "windows" => Some("windows"),
        _ => None,
    }
}

/// The Adoptium API's name for this architecture.
#[must_use]
pub fn api_arch() -> Option<&'static str> {
    api_arch_for(std::env::consts::ARCH)
}

fn api_arch_for(arch: &str) -> Option<&'static str> {
    match arch {
        "x86_64" => Some("x64"),
        "aarch64" => Some("aarch64"),
        _ => None,
    }
}

/// Talks to the vendor's API and downloads from it.
pub struct Vendor {
    agent: ureq::Agent,
    url: String,
}

impl Vendor {
    /// # Errors
    ///
    /// [`JrsError::Usage`] when the config names a vendor other than Temurin,
    /// or the proxy URL is not usable.
    pub fn new(config: &JdkConfig, proxy: Option<&ProxyConfig>) -> Result<Vendor> {
        if config.vendor != TEMURIN {
            return Err(JrsError::usage(format!(
                "`jdk.vendor` is \"{}\", but jrs can only download \"{TEMURIN}\" JDKs",
                config.vendor
            )));
        }
        Ok(Vendor {
            agent: crate::selfupdate::agent(proxy, 10)?,
            url: config.url.trim_end_matches('/').to_string(),
        })
    }

    /// The newest GA build of JDK `feature` for this machine.
    ///
    /// # Errors
    ///
    /// [`JrsError::Toolchain`] when this platform has no Temurin build, and
    /// [`JrsError::Resolve`] when the API cannot be reached or offers none.
    pub fn latest(&self, feature: u32) -> Result<Build> {
        let (Some(os), Some(arch)) = (api_os(), api_arch()) else {
            return Err(JrsError::toolchain(format!(
                "there is no {TEMURIN} JDK for {} on {}",
                std::env::consts::OS,
                std::env::consts::ARCH
            )));
        };
        let url = format!(
            "{}/v3/assets/latest/{feature}/hotspot?image_type=jdk&os={os}&architecture={arch}&vendor=eclipse",
            self.url
        );
        let fail = |e: &dyn fmt::Display| {
            JrsError::resolve(format!("could not ask {url} for JDK {feature}\n\n{e}"))
        };
        let mut response = self.agent.get(&url).call().map_err(|e| fail(&e))?;
        let status = response.status().as_u16();
        if status != 200 {
            return Err(fail(&format!("HTTP {status}")));
        }
        let text = response.body_mut().read_to_string().map_err(|e| fail(&e))?;
        let json = Json::parse(&text).map_err(|e| fail(&e))?;
        newest_build(&json, os, arch).ok_or_else(|| {
            JrsError::resolve(format!(
                "{url} offers no {TEMURIN} JDK {feature} for {os}-{arch}\n\n\
                 is {feature} a released Java version?"
            ))
        })
    }

    /// Stream `build`'s archive into `dir`, hashing it as it arrives, and
    /// return the file once its SHA-256 matches. `progress` hears the bytes
    /// in so far and the total, when known.
    ///
    /// # Errors
    ///
    /// [`JrsError::Resolve`] when the download fails or the archive does not
    /// match its checksum; nothing is left behind either way.
    pub fn download(
        &self,
        build: &Build,
        dir: &Path,
        progress: &mut dyn FnMut(u64, Option<u64>),
    ) -> Result<PathBuf> {
        std::fs::create_dir_all(dir).path(dir)?;
        let file = dir.join(format!(".tmp-{}-{}", unique(), build.name));
        let result = self.download_to(build, &file, progress);
        if result.is_err() {
            let _ = std::fs::remove_file(&file);
        }
        result.map(|()| file)
    }

    fn download_to(
        &self,
        build: &Build,
        file: &Path,
        progress: &mut dyn FnMut(u64, Option<u64>),
    ) -> Result<()> {
        let url = &build.url;
        let fail =
            |e: &dyn fmt::Display| JrsError::resolve(format!("could not download {url}\n\n{e}"));
        let mut response = self.agent.get(url).call().map_err(|e| fail(&e))?;
        let status = response.status().as_u16();
        if status != 200 {
            return Err(fail(&format!("HTTP {status}")));
        }
        let total = response
            .headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok())
            .or(build.size);
        let mut out = std::fs::File::create(file).path(file)?;
        let mut reader = response.body_mut().as_reader();
        let mut hasher = sha2::Sha256::new();
        let mut buf = vec![0u8; 64 * 1024];
        let mut done = 0u64;
        progress(0, total);
        loop {
            let n = reader.read(&mut buf).map_err(|e| fail(&e))?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            out.write_all(&buf[..n]).path(file)?;
            done += n as u64;
            progress(done, total);
        }
        out.flush().path(file)?;
        let actual = hex(&hasher.finalize());
        if !actual.eq_ignore_ascii_case(&build.sha256) {
            return Err(JrsError::resolve(format!(
                "checksum mismatch for {}: expected {}, got {actual}",
                build.name, build.sha256
            )));
        }
        Ok(())
    }
}

/// The build the API's answer to `/v3/assets/latest` names for `os`-`arch`:
/// the newest by version, should it list several.
fn newest_build(json: &Json, os: &str, arch: &str) -> Option<Build> {
    json.items()
        .iter()
        .filter_map(|asset| {
            let binary = asset.get("binary")?;
            if binary.get("os")?.as_str()? != os
                || binary.get("architecture")?.as_str()? != arch
                || binary.get("image_type")?.as_str()? != "jdk"
            {
                return None;
            }
            let package = binary.get("package")?;
            let release = asset.get("release_name")?.as_str()?;
            let version = release.strip_prefix("jdk-").unwrap_or(release);
            let size = match package.get("size") {
                Some(Json::Int(n)) => u64::try_from(*n).ok(),
                _ => None,
            };
            Some(Build {
                version: version.to_string(),
                name: package.get("name")?.as_str()?.to_string(),
                url: package.get("link")?.as_str()?.to_string(),
                sha256: package.get("checksum")?.as_str()?.to_string(),
                size,
                os: os.to_string(),
                arch: arch.to_string(),
            })
        })
        .filter(|b| is_safe_name(&b.name) && is_safe_name(&b.version))
        .max_by(|a, b| crate::resolve::coord::compare_versions(&a.version, &b.version))
}

/// A name that is one path segment: what the API says goes into file names.
fn is_safe_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains(['/', '\\', ':'])
        && !name.starts_with('.')
}

/// Unpack `archive`, a downloaded `build`, under `jdks`, check that it holds
/// a JDK of `feature`, and rename it into place as [`Build::dir_name`].
/// Returns the JDK's home.
///
/// # Errors
///
/// [`JrsError::Toolchain`] when the archive holds no JDK, or another version
/// than asked for; [`JrsError::Build`] when it cannot be unpacked.
pub fn install(archive: &Path, build: &Build, feature: u32, jdks: &Path) -> Result<PathBuf> {
    let scratch = jdks.join(format!(".tmp-{}", unique()));
    let result = (|| {
        std::fs::create_dir_all(&scratch).path(&scratch)?;
        if Path::new(&build.name)
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("zip"))
        {
            unzip(archive, &scratch)?;
        } else {
            untar(archive, &scratch)?;
        }
        let home = find_home(&scratch, 3).ok_or_else(|| {
            JrsError::toolchain(format!("{} holds no JDK (no `release` file)", build.name))
        })?;
        let toolchain = crate::toolchain::Toolchain::probe_home(&home)?;
        if toolchain.version != feature {
            return Err(JrsError::toolchain(format!(
                "{} holds JDK {}, not JDK {feature}",
                build.name, toolchain.version
            )));
        }
        // The archive's top directory becomes the installed one, so that a
        // macOS bundle keeps its `Contents/Home` inside it.
        let top = top_of(&scratch, &home);
        let target = jdks.join(build.dir_name());
        let inner = home
            .strip_prefix(&top)
            .unwrap_or(Path::new(""))
            .to_path_buf();
        if let Err(e) = std::fs::rename(&top, &target) {
            // Installed meanwhile by another jrs, which may be running it:
            // theirs is kept, and is as good.
            if target.join(&inner).join("release").is_file() {
                return Ok(target.join(&inner));
            }
            return Err(JrsError::io(&target, e));
        }
        Ok(target.join(inner))
    })();
    let _ = std::fs::remove_dir_all(&scratch);
    let _ = std::fs::remove_file(archive);
    result
}

/// The first directory of `home`'s path below `scratch`.
fn top_of(scratch: &Path, home: &Path) -> PathBuf {
    home.strip_prefix(scratch)
        .ok()
        .and_then(|rest| rest.components().next())
        .map_or_else(|| home.to_path_buf(), |first| scratch.join(first))
}

/// The directory within `depth` levels of `dir` holding a `release` file and
/// `bin/`: `jdk-21.0.5+11/` on Linux and Windows,
/// `jdk-21.0.5+11/Contents/Home/` on macOS.
fn find_home(dir: &Path, depth: usize) -> Option<PathBuf> {
    if dir.join("release").is_file() && dir.join("bin").is_dir() {
        return Some(dir.to_path_buf());
    }
    if depth == 0 {
        return None;
    }
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    entries.sort();
    entries.into_iter().find_map(|d| find_home(&d, depth - 1))
}

fn untar(archive: &Path, into: &Path) -> Result<()> {
    // Every macOS and Linux machine has a `tar` that reads gzip, as
    // `selfupdate::unpack` relies on.
    let status = Command::new("tar")
        .arg("-xzf")
        .arg(archive)
        .arg("-C")
        .arg(into)
        .status()
        .map_err(|e| JrsError::build(format!("could not run `tar`: {e}")))?;
    if !status.success() {
        return Err(JrsError::build(format!(
            "`tar` could not unpack {}",
            archive.display()
        )));
    }
    Ok(())
}

fn unzip(archive: &Path, into: &Path) -> Result<()> {
    let bad = |e: &dyn fmt::Display| JrsError::build(format!("{}: {e}", archive.display()));
    let file = std::fs::File::open(archive).path(archive)?;
    let mut zip = zip::ZipArchive::new(std::io::BufReader::new(file)).map_err(|e| bad(&e))?;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| bad(&e))?;
        // An entry that would land outside the directory is refused, not
        // skipped: an archive that has one is not a JDK.
        let Some(relative) = entry.enclosed_name() else {
            return Err(bad(&format!(
                "the entry {} escapes the archive",
                entry.name()
            )));
        };
        let path = into.join(relative);
        if entry.is_dir() {
            std::fs::create_dir_all(&path).path(&path)?;
            continue;
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).path(parent)?;
        }
        let mut out = std::fs::File::create(&path).path(&path)?;
        std::io::copy(&mut entry, &mut out).path(&path)?;
        #[cfg(unix)]
        if let Some(mode) = entry.unix_mode() {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode));
        }
    }
    Ok(())
}

fn unique() -> String {
    format!(
        "{}-{}",
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset(os: &str, arch: &str, release: &str) -> String {
        format!(
            r#"{{"binary":{{"architecture":"{arch}","image_type":"jdk","os":"{os}",
                "package":{{"checksum":"abc","link":"https://example.com/{release}.tar.gz",
                "name":"OpenJDK-{release}.tar.gz","size":201234567}}}},
               "release_name":"{release}"}}"#
        )
    }

    #[test]
    fn the_answer_names_the_build_for_this_machine() {
        let text = format!(
            "[{},{},{}]",
            asset("linux", "x64", "jdk-21.0.4+7"),
            asset("linux", "x64", "jdk-21.0.5+11"),
            asset("mac", "x64", "jdk-21.0.6+1"),
        );
        let json = Json::parse(&text).unwrap();
        let build = newest_build(&json, "linux", "x64").unwrap();
        assert_eq!(build.version, "21.0.5+11");
        assert_eq!(build.name, "OpenJDK-jdk-21.0.5+11.tar.gz");
        assert_eq!(build.sha256, "abc");
        assert_eq!(build.size, Some(201_234_567));
        assert_eq!(build.dir_name(), "temurin-21.0.5+11-linux-x64");
        assert_eq!(build.platform(), "temurin, linux-x64");
        assert_eq!(newest_build(&json, "windows", "x64"), None);
    }

    #[test]
    fn a_name_that_is_not_one_segment_is_refused() {
        let text = format!("[{}]", asset("linux", "x64", "../../evil"));
        let json = Json::parse(&text).unwrap();
        assert_eq!(newest_build(&json, "linux", "x64"), None);
    }

    #[test]
    fn platforms_map_to_the_api_names() {
        assert_eq!(api_os_for("linux", false), Some("linux"));
        assert_eq!(api_os_for("linux", true), Some("alpine-linux"));
        assert_eq!(api_os_for("macos", false), Some("mac"));
        assert_eq!(api_os_for("freebsd", false), None);
        assert_eq!(api_arch_for("x86_64"), Some("x64"));
        assert_eq!(api_arch_for("aarch64"), Some("aarch64"));
        assert_eq!(api_arch_for("riscv64"), None);
    }

    #[test]
    fn the_home_is_found_inside_a_macos_bundle() {
        let dir = std::env::temp_dir().join(format!("jrs-jdk-test-{}", unique()));
        let home = dir.join("jdk-21.0.5+11").join("Contents").join("Home");
        std::fs::create_dir_all(home.join("bin")).unwrap();
        std::fs::write(home.join("release"), "JAVA_VERSION=\"21.0.5\"\n").unwrap();
        assert_eq!(find_home(&dir, 3), Some(home.clone()));
        assert_eq!(top_of(&dir, &home), dir.join("jdk-21.0.5+11"));
        assert_eq!(find_home(&dir, 1), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_zip_entry_outside_the_directory_is_refused() {
        let dir = std::env::temp_dir().join(format!("jrs-jdk-zip-{}", unique()));
        std::fs::create_dir_all(&dir).unwrap();
        let archive = dir.join("evil.zip");
        {
            let mut zip = zip::ZipWriter::new(std::fs::File::create(&archive).unwrap());
            zip.start_file("../escape.txt", zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(b"x").unwrap();
            zip.finish().unwrap();
        }
        let into = dir.join("out");
        std::fs::create_dir_all(&into).unwrap();
        let err = unzip(&archive, &into).unwrap_err();
        assert!(err.to_string().contains("escapes"), "{err}");
        assert!(!dir.join("escape.txt").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
