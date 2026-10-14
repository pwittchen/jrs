//! What a benchmark report says of the machine it ran on: the JDK, the OS,
//! and the date.

#![allow(
    dead_code,
    reason = "each benchmark that includes this module uses a part of it"
)]

use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

/// The JDK's feature release, from `java -version`.
pub fn host_jdk() -> Option<u32> {
    let java = std::env::var_os("JAVA_HOME")
        .map(|home| PathBuf::from(home).join("bin/java"))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from("java"));
    let output = Command::new(java).arg("-version").output().ok()?;
    let text = String::from_utf8_lossy(&output.stderr);
    let version = text.split('"').nth(1)?;
    let version = version.strip_prefix("1.").unwrap_or(version);
    version.split(['.', '-', '+']).next()?.parse().ok()
}

pub fn jdk_version() -> String {
    Command::new("java")
        .arg("-version")
        .output()
        .ok()
        .and_then(|o| {
            String::from_utf8_lossy(&o.stderr)
                .split('"')
                .nth(1)
                .map(str::to_string)
        })
        .unwrap_or_else(|| "none".to_string())
}

pub fn os_version() -> String {
    let sw_vers = || {
        let output = Command::new("sw_vers")
            .arg("-productVersion")
            .output()
            .ok()?;
        Some(format!(
            "macOS {}",
            String::from_utf8_lossy(&output.stdout).trim()
        ))
    };
    let os_release = || {
        let text = std::fs::read_to_string("/etc/os-release").ok()?;
        text.lines()
            .find_map(|l| l.strip_prefix("PRETTY_NAME="))
            .map(|v| v.trim_matches('"').to_string())
    };
    match std::env::consts::OS {
        "macos" => sw_vers(),
        "linux" => os_release(),
        _ => None,
    }
    .unwrap_or_else(|| std::env::consts::OS.to_string())
}

/// The run's directory name (`2026-10-20-142501`, UTC), or its date alone.
pub fn stamp(at: SystemTime, with_time: bool) -> String {
    let secs = at.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let (year, month, day) = civil(days);
    let date = format!("{year:04}-{month:02}-{day:02}");
    if !with_time {
        return date;
    }
    let time = secs % 86_400;
    format!(
        "{date}-{:02}{:02}{:02}",
        time / 3600,
        time % 3600 / 60,
        time % 60
    )
}

/// Days since 1970-01-01 to a proleptic Gregorian date (Howard Hinnant's
/// `civil_from_days`).
fn civil(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}
