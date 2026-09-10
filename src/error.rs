//! Error types and user-facing formatting.
//!
//! Every fallible operation in jrs returns [`Result`]. The CLI layer is the only
//! place that decides how an error is rendered; library code never prints.

use std::fmt;
use std::path::{Path, PathBuf};

pub type Result<T> = std::result::Result<T, JrsError>;

/// Exit codes, per SPEC §5.2.
pub mod exit {
    pub const SUCCESS: i32 = 0;
    pub const FAILURE: i32 = 1;
    pub const USAGE: i32 = 2;
    pub const INTERNAL: i32 = 101;
}

#[derive(Debug, thiserror::Error)]
pub enum JrsError {
    /// Bad command-line usage or a missing prerequisite the user must fix.
    #[error("{0}")]
    Usage(String),

    /// `jrs.toml` is missing, unparseable or semantically invalid.
    #[error("{0}")]
    Manifest(String),

    /// No usable JDK, or one too old for the manifest.
    #[error("{0}")]
    Toolchain(String),

    /// Dependency resolution failed (network, checksum, malformed POM, ...).
    #[error("{0}")]
    Resolve(String),

    /// `javac`, `jar` or packaging failed.
    #[error("{0}")]
    Build(String),

    /// The user's tests failed, or the launcher could not be started.
    #[error("{0}")]
    Test(String),

    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl JrsError {
    pub fn io(path: impl AsRef<Path>, source: std::io::Error) -> Self {
        JrsError::Io {
            path: path.as_ref().to_path_buf(),
            source,
        }
    }

    pub fn usage(msg: impl fmt::Display) -> Self {
        JrsError::Usage(msg.to_string())
    }

    pub fn manifest(msg: impl fmt::Display) -> Self {
        JrsError::Manifest(msg.to_string())
    }

    pub fn toolchain(msg: impl fmt::Display) -> Self {
        JrsError::Toolchain(msg.to_string())
    }

    pub fn resolve(msg: impl fmt::Display) -> Self {
        JrsError::Resolve(msg.to_string())
    }

    pub fn build(msg: impl fmt::Display) -> Self {
        JrsError::Build(msg.to_string())
    }

    pub fn test(msg: impl fmt::Display) -> Self {
        JrsError::Test(msg.to_string())
    }

    /// The process exit code this error should produce.
    pub fn exit_code(&self) -> i32 {
        match self {
            JrsError::Usage(_) | JrsError::Manifest(_) => exit::USAGE,
            _ => exit::FAILURE,
        }
    }
}

/// Convenience for `std::io` results that know their own path.
pub trait IoResultExt<T> {
    fn path(self, path: impl AsRef<Path>) -> Result<T>;
}

impl<T> IoResultExt<T> for std::io::Result<T> {
    fn path(self, path: impl AsRef<Path>) -> Result<T> {
        self.map_err(|e| JrsError::io(path, e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_follow_the_spec() {
        assert_eq!(JrsError::usage("x").exit_code(), 2);
        assert_eq!(JrsError::manifest("x").exit_code(), 2);
        assert_eq!(JrsError::build("x").exit_code(), 1);
        assert_eq!(JrsError::test("x").exit_code(), 1);
        assert_eq!(JrsError::resolve("x").exit_code(), 1);
        assert_eq!(JrsError::toolchain("x").exit_code(), 1);
    }

    #[test]
    fn io_errors_name_their_path() {
        let e = JrsError::io(
            "/nope/jrs.toml",
            std::io::Error::new(std::io::ErrorKind::NotFound, "no such file"),
        );
        assert!(e.to_string().contains("/nope/jrs.toml"));
        assert!(e.to_string().contains("no such file"));
    }
}
