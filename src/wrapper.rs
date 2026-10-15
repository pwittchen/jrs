//! `jrs wrapper`: `jrsw` and `jrsw.cmd`, the scripts that pin a project to
//! one jrs release, as `gradlew` pins Gradle (SPEC §4.3).
//!
//! Each script holds one version and the SHA-256 of every release archive,
//! read from that release's `SHA256SUMS` when the scripts are written. Run,
//! it looks for `<cache>/dist/<version>/jrs`, and when it is not there yet
//! downloads the archive for this machine, checks it against the checksum it
//! carries, unpacks it there and runs it with its arguments. That is stronger
//! than `gradlew`'s default, which checks nothing unless asked: a release
//! re-uploaded after the scripts were written fails to run.
//!
//! The POSIX script's platform detection is `website/install.sh`'s
//! `detect_target`, copied in when the script is generated, so there is one
//! copy of it. `project.jrs-version` stays the minimum; the wrapper is the
//! exact pin.

use std::fmt::Write as _;

use crate::selfupdate::{RELEASES, Version, asset_name};

/// The POSIX script.
pub const POSIX: &str = "jrsw";
/// The Windows script.
pub const WINDOWS: &str = "jrsw.cmd";

/// The installer, whose target detection the POSIX script reuses.
const INSTALL_SH: &str = include_str!("../website/install.sh");

/// Every release target `SHA256SUMS` lists an archive for, with its checksum,
/// sorted by target.
#[must_use]
pub fn targets(sums: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = sums
        .lines()
        .filter_map(|line| {
            let (hash, name) = line.split_once(char::is_whitespace)?;
            let name = name.trim();
            let name = name.strip_prefix('*').unwrap_or(name);
            let target = name
                .strip_prefix("jrs-")?
                .strip_suffix(".tar.gz")
                .or_else(|| name.strip_prefix("jrs-")?.strip_suffix(".zip"))?;
            let ok = hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit());
            let safe = target
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
            (ok && safe && asset_name(target) == name)
                .then(|| (target.to_string(), hash.to_ascii_lowercase()))
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

/// `detect_target` from `website/install.sh`, as it is written there. A
/// test makes sure it is found; the fallback is never shipped.
fn detect_target() -> &'static str {
    INSTALL_SH
        .find("detect_target() {")
        .and_then(|start| {
            let end = INSTALL_SH[start..].find("\n}\n")?;
            Some(&INSTALL_SH[start..start + end + 2])
        })
        .unwrap_or("detect_target() {\n    die \"this jrsw cannot tell which jrs build to run\"\n}")
}

/// The POSIX `sh` script for `version`, with the checksums of `targets`.
#[must_use]
pub fn posix(version: &Version, targets: &[(String, String)]) -> String {
    let mut cases = String::new();
    for (target, hash) in targets.iter().filter(|(t, _)| !t.contains("windows")) {
        let _ = writeln!(cases, "        {target}) echo {hash} ;;");
    }
    format!(
        r#"#!/bin/sh
# Runs jrs {version}, the release this project is pinned to, downloading it
# into jrs's cache the first time. Written by `jrs wrapper`; commit it, and
# move the pin with `jrs wrapper --version <version>`.
#
# The archive is checked against the SHA-256 below, read from the release's
# SHA256SUMS when this file was written, before anything in it runs.

set -eu

JRSW_VERSION={version}
RELEASES="{RELEASES}"

die() {{
    printf 'jrsw: %s\n' "$*" >&2
    exit 1
}}

checksum() {{
    case "$1" in
{cases}        *) return 1 ;;
    esac
}}

# Where jrs keeps its cache, as jrs itself decides it.
cache_dir() {{
    if [ -n "${{JRS_CACHE_DIR:-}}" ]; then
        printf '%s\n' "$JRS_CACHE_DIR"
    elif [ "$(uname -s)" = Darwin ]; then
        printf '%s\n' "${{HOME:?HOME is not set}}/Library/Caches/jrs"
    else
        printf '%s\n' "${{XDG_CACHE_HOME:-${{HOME:?HOME is not set}}/.cache}}/jrs"
    fi
}}

# Copied from website/install.sh by `jrs wrapper`.
{detect}

download() {{
    if command -v curl >/dev/null 2>&1; then
        curl --proto '=https' --tlsv1.2 -fsSL --retry 3 -o "$2" "$1"
    elif command -v wget >/dev/null 2>&1; then
        wget --https-only -q -O "$2" "$1"
    else
        die "this script needs \`curl\` or \`wget\`, and neither is on PATH"
    fi
}}

install() {{
    target="$(detect_target)" || exit 1
    expected="$(checksum "$target")" || die "jrs $JRSW_VERSION has no build for $target"
    asset="jrs-$target.tar.gz"
    tmp="$(mktemp -d)"
    trap 'rm -rf "$tmp"' EXIT
    printf 'jrsw: downloading jrs %s (%s)\n' "$JRSW_VERSION" "$target" >&2
    download "$RELEASES/download/v$JRSW_VERSION/$asset" "$tmp/$asset" ||
        die "could not download $RELEASES/download/v$JRSW_VERSION/$asset"
    if command -v sha256sum >/dev/null 2>&1; then
        actual="$(sha256sum "$tmp/$asset")"
    elif command -v shasum >/dev/null 2>&1; then
        actual="$(shasum -a 256 "$tmp/$asset")"
    else
        die "this script needs \`sha256sum\` or \`shasum\` to check what it downloads"
    fi
    actual="${{actual%% *}}"
    [ "$actual" = "$expected" ] || die "checksum mismatch for $asset: expected $expected, got $actual"
    tar -xzf "$tmp/$asset" -C "$tmp" jrs || die "$asset is not a valid archive"
    mkdir -p "$dir" || die "could not create $dir"
    # Copied beside its destination and renamed over it, so that an
    # interrupted download never leaves a half-written jrs to run.
    cp "$tmp/jrs" "$dir/.jrs.$$" || die "could not write to $dir"
    chmod 755 "$dir/.jrs.$$"
    mv -f "$dir/.jrs.$$" "$dir/jrs" || die "could not write $dir/jrs"
    rm -rf "$tmp"
    trap - EXIT
}}

dir="$(cache_dir)/dist/$JRSW_VERSION"
[ -x "$dir/jrs" ] || install
exec "$dir/jrs" "$@"
"#,
        detect = detect_target(),
    )
}

/// The Windows script for `version`: `cmd` finds the binary and runs it,
/// and the PowerShell after the marker downloads it when it is missing.
#[must_use]
pub fn windows(version: &Version, targets: &[(String, String)]) -> String {
    let mut sums = String::new();
    for (target, hash) in targets.iter().filter(|(t, _)| t.contains("windows")) {
        let arch = target
            .split('-')
            .next()
            .unwrap_or_default()
            .to_ascii_uppercase();
        let _ = write!(sums, "set \"JRSW_SHA256_{arch}={hash}\"\r\n");
    }
    let text = format!(
        r#"@echo off
rem Runs jrs {version}, the release this project is pinned to, downloading it
rem into jrs's cache the first time. Written by `jrs wrapper`; commit it, and
rem move the pin with `jrs wrapper --version <version>`.
rem
rem The archive is checked against the SHA-256 below, read from the release's
rem SHA256SUMS when this file was written, before anything in it runs.
setlocal
set "JRSW_VERSION={version}"
{sums}if defined JRS_CACHE_DIR (set "JRSW_CACHE=%JRS_CACHE_DIR%") else (set "JRSW_CACHE=%LOCALAPPDATA%\jrs\cache")
set "JRSW_DIR=%JRSW_CACHE%\dist\%JRSW_VERSION%"
if not exist "%JRSW_DIR%\jrs.exe" (
    powershell -NoProfile -ExecutionPolicy Bypass -Command "$s = [IO.File]::ReadAllText('%~f0'); Invoke-Expression $s.Substring($s.IndexOf('#' + '#powershell'))"
    if errorlevel 1 exit /b 1
)
"%JRSW_DIR%\jrs.exe" %*
exit /b %ERRORLEVEL%

##powershell
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$version = $env:JRSW_VERSION
$dir = $env:JRSW_DIR
$arch = if ($env:PROCESSOR_ARCHITEW6432) {{ $env:PROCESSOR_ARCHITEW6432 }} else {{ $env:PROCESSOR_ARCHITECTURE }}
switch ($arch) {{
    'AMD64' {{ $target = 'x86_64-pc-windows-msvc'; $expected = $env:JRSW_SHA256_X86_64 }}
    'ARM64' {{ $target = 'aarch64-pc-windows-msvc'; $expected = $env:JRSW_SHA256_AARCH64 }}
    default {{ [Console]::Error.WriteLine("jrsw: there is no prebuilt jrs for Windows on $arch"); exit 1 }}
}}
if (-not $expected) {{ [Console]::Error.WriteLine("jrsw: jrs $version has no build for $target"); exit 1 }}
$asset = "jrs-$target.zip"
$tmp = Join-Path ([IO.Path]::GetTempPath()) ('jrsw-' + [guid]::NewGuid())
New-Item -ItemType Directory -Path $tmp | Out-Null
try {{
    [Console]::Error.WriteLine("jrsw: downloading jrs $version ($target)")
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
    Invoke-WebRequest -UseBasicParsing -Uri "{RELEASES}/download/v$version/$asset" -OutFile (Join-Path $tmp $asset)
    $actual = (Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $tmp $asset)).Hash
    if ($actual -ne $expected) {{
        [Console]::Error.WriteLine("jrsw: checksum mismatch for ${{asset}}: expected $expected, got $actual")
        exit 1
    }}
    Expand-Archive -LiteralPath (Join-Path $tmp $asset) -DestinationPath (Join-Path $tmp 'out') -Force
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
    # Moved beside its destination first and renamed over it, so that an
    # interrupted download never leaves a half-written jrs.exe to run.
    $staged = Join-Path $dir ".jrs.$PID.exe"
    Move-Item -Force -LiteralPath (Join-Path (Join-Path $tmp 'out') 'jrs.exe') -Destination $staged
    Move-Item -Force -LiteralPath $staged -Destination (Join-Path $dir 'jrs.exe')
}} finally {{
    Remove-Item -Recurse -Force -LiteralPath $tmp -ErrorAction SilentlyContinue
}}
"#
    );
    // `cmd` reads a batch file best with CRLF line ends.
    text.replace("\r\n", "\n").replace('\n', "\r\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SUMS: &str = "\
1111111111111111111111111111111111111111111111111111111111111111  jrs-aarch64-apple-darwin.tar.gz
2222222222222222222222222222222222222222222222222222222222222222  jrs-x86_64-unknown-linux-musl.tar.gz
3333333333333333333333333333333333333333333333333333333333333333 *jrs-x86_64-pc-windows-msvc.zip
4444444444444444444444444444444444444444444444444444444444444444  jrs-intellij.zip
5555555555555555555555555555555555555555555555555555555555555555  jrs-x86_64-unknown-linux-musl.zip
not-a-hash  jrs-aarch64-unknown-linux-musl.tar.gz
";

    fn version() -> Version {
        Version::parse("0.11.1").unwrap()
    }

    #[test]
    fn only_release_archives_with_a_checksum_are_targets() {
        let targets = targets(SUMS);
        let names: Vec<&str> = targets.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(
            names,
            [
                "aarch64-apple-darwin",
                "x86_64-pc-windows-msvc",
                "x86_64-unknown-linux-musl"
            ]
        );
    }

    #[test]
    fn the_posix_script_pins_the_version_and_its_checksums() {
        let script = posix(&version(), &targets(SUMS));
        assert!(script.starts_with("#!/bin/sh\n"));
        assert!(script.contains("JRSW_VERSION=0.11.1\n"));
        assert!(script.contains(
            "aarch64-apple-darwin) echo 1111111111111111111111111111111111111111111111111111111111111111 ;;"
        ));
        assert!(
            !script.contains("3333"),
            "no Windows checksum in the POSIX script"
        );
        assert!(script.contains("detect_target() {"));
        assert!(
            script.contains("x86_64-unknown-linux-musl ;;"),
            "install.sh's detection"
        );
        assert!(script.contains("exec \"$dir/jrs\" \"$@\""));
    }

    #[cfg(unix)]
    #[test]
    fn the_posix_script_is_valid_sh() {
        let dir = std::env::temp_dir().join(format!("jrs-wrapper-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join(POSIX);
        std::fs::write(&script, posix(&version(), &targets(SUMS))).unwrap();
        let status = std::process::Command::new("sh")
            .arg("-n")
            .arg(&script)
            .status()
            .unwrap();
        assert!(status.success());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_posix_script_runs_a_cached_jrs_without_downloading() {
        if cfg!(windows) {
            return;
        }
        let dir = std::env::temp_dir().join(format!("jrs-wrapper-run-{}", std::process::id()));
        let cached = dir.join("cache/dist/0.11.1");
        std::fs::create_dir_all(&cached).unwrap();
        std::fs::write(cached.join("jrs"), "#!/bin/sh\necho \"cached jrs $*\"\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(cached.join("jrs"), std::fs::Permissions::from_mode(0o755))
                .unwrap();
        }
        let script = dir.join(POSIX);
        std::fs::write(&script, posix(&version(), &targets(SUMS))).unwrap();
        let output = std::process::Command::new("sh")
            .arg(&script)
            .args(["build", "--offline"])
            .env("JRS_CACHE_DIR", dir.join("cache"))
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "cached jrs build --offline\n"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_windows_script_carries_the_windows_checksums_in_crlf() {
        let script = windows(&version(), &targets(SUMS));
        assert!(script.starts_with("@echo off\r\n"));
        assert!(script.contains("set \"JRSW_VERSION=0.11.1\"\r\n"));
        assert!(script.contains(
            "set \"JRSW_SHA256_X86_64=3333333333333333333333333333333333333333333333333333333333333333\"\r\n"
        ));
        assert!(!script.contains("1111"));
        assert!(!script.replace("\r\n", "").contains('\n'));
        // The marker the `cmd` half looks for appears once, as itself.
        assert_eq!(script.matches("##powershell").count(), 1);
    }
}
