//! `maven-metadata.xml`: what a repository says about an artifact's versions.
//!
//! Two files share the name. The one beside the version directories
//! (`g/a/maven-metadata.xml`) lists every published version, which is what
//! `jrs outdated` and `jrs add` read. The one inside a `-SNAPSHOT` directory
//! (`g/a/1.0-SNAPSHOT/maven-metadata.xml`) names the timestamped file that the
//! snapshot currently points at, which is how a snapshot published to a remote
//! repository is found at all.

use super::coord::{SNAPSHOT_SUFFIX, compare_versions};
use super::pom::{Element, parse_xml};
use crate::error::Result;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Metadata {
    pub release: Option<String>,
    pub latest: Option<String>,
    pub versions: Vec<String>,
    /// `<snapshot><timestamp>` and `<buildNumber>`, the older way of naming the
    /// current snapshot build.
    pub snapshot: Option<(String, String)>,
    /// `<localCopy>true</localCopy>`: a snapshot installed by `mvn install`,
    /// stored under its plain `-SNAPSHOT` name.
    pub local_copy: bool,
    pub snapshot_versions: Vec<SnapshotVersion>,
}

/// One `<snapshotVersion>`: the file a snapshot resolves to, per extension and
/// classifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotVersion {
    pub classifier: Option<String>,
    pub extension: String,
    pub value: String,
}

impl Metadata {
    /// Read a `maven-metadata.xml`. Elements it does not know are ignored.
    ///
    /// # Errors
    ///
    /// [`JrsError::Resolve`](crate::error::JrsError::Resolve) when the bytes
    /// are not well-formed XML with a root element.
    pub fn parse(bytes: &[u8]) -> Result<Metadata> {
        let root = parse_xml(bytes)?;
        let versioning = root.child("versioning");
        let text =
            |e: Option<&Element>, name: &str| e.and_then(|v| v.text_of(name)).map(str::to_string);
        let snapshot = versioning.and_then(|v| v.child("snapshot"));
        Ok(Metadata {
            release: text(versioning, "release"),
            latest: text(versioning, "latest"),
            versions: versioning
                .map(|v| {
                    v.list("versions", "version")
                        .into_iter()
                        .map(|e| e.text.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect()
                })
                .unwrap_or_default(),
            snapshot: snapshot.and_then(|s| {
                Some((
                    s.text_of("timestamp")?.to_string(),
                    s.text_of("buildNumber")?.to_string(),
                ))
            }),
            local_copy: snapshot
                .and_then(|s| s.text_of("localCopy"))
                .is_some_and(|v| v == "true"),
            snapshot_versions: versioning
                .map(|v| {
                    v.list("snapshotVersions", "snapshotVersion")
                        .into_iter()
                        .filter_map(|s| {
                            Some(SnapshotVersion {
                                classifier: s.text_of("classifier").map(str::to_string),
                                extension: s.text_of("extension")?.to_string(),
                                value: s.text_of("value")?.to_string(),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default(),
        })
    }

    /// The version string in the file name of snapshot `version`'s current build,
    /// for the file with extension `ext` and classifier `classifier`.
    ///
    /// `None` means the plain `-SNAPSHOT` name: a locally installed snapshot, or
    /// metadata that names no build.
    #[must_use]
    pub fn snapshot_file_version(
        &self,
        version: &str,
        ext: &str,
        classifier: Option<&str>,
    ) -> Option<String> {
        if self.local_copy {
            return None;
        }
        if let Some(v) = self
            .snapshot_versions
            .iter()
            .find(|s| s.extension == ext && s.classifier.as_deref() == classifier)
        {
            return Some(v.value.clone());
        }
        let (timestamp, build) = self.snapshot.as_ref()?;
        let base = version.strip_suffix(SNAPSHOT_SUFFIX).unwrap_or(version);
        Some(format!("{base}-{timestamp}-{build}"))
    }
}

/// Whether a version is a pre-release: an alpha, beta, milestone, release
/// candidate, snapshot, or an early-access / preview build.
#[must_use]
pub fn is_prerelease(version: &str) -> bool {
    let lower = version.to_ascii_lowercase();
    let mut word = String::new();
    let mut words = Vec::new();
    for c in lower.chars().chain(std::iter::once('.')) {
        if c.is_ascii_alphabetic() {
            word.push(c);
        } else if !word.is_empty() {
            words.push(std::mem::take(&mut word));
        }
    }
    words.iter().any(|w| {
        matches!(
            w.as_str(),
            "a" | "alpha"
                | "b"
                | "beta"
                | "m"
                | "milestone"
                | "rc"
                | "cr"
                | "snapshot"
                | "ea"
                | "preview"
                | "dev"
                | "pre"
        )
    })
}

/// The trailing qualifier a release line carries on every version: `-jre` and
/// `-android` for Guava, `.Final` for Hibernate. Candidates for an upgrade keep
/// the same one, so a Guava on `-android` is not told to move to `-jre`.
fn flavour(version: &str) -> String {
    let tail: String = version
        .chars()
        .rev()
        .take_while(|c| !c.is_ascii_digit())
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    tail.to_ascii_lowercase()
}

/// The newest version worth moving to from `current`, if it is newer.
///
/// Pre-releases are only offered to a project that is already on one.
#[must_use]
pub fn newest(versions: &[String], current: &str) -> Option<String> {
    let allow_prerelease = is_prerelease(current);
    let current_flavour = flavour(current);
    versions
        .iter()
        .filter(|v| allow_prerelease || !is_prerelease(v))
        .filter(|v| is_prerelease(v) || flavour(v) == current_flavour)
        .max_by(|a, b| compare_versions(a, b))
        .filter(|v| compare_versions(v, current) == std::cmp::Ordering::Greater)
        .cloned()
}

/// The newest stable version, for adding a dependency with no version given.
#[must_use]
pub fn newest_release(metadata: &Metadata) -> Option<String> {
    metadata
        .versions
        .iter()
        .filter(|v| !is_prerelease(v))
        .max_by(|a, b| compare_versions(a, b))
        .cloned()
        .or_else(|| metadata.release.clone().filter(|r| !is_prerelease(r)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const VERSIONS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<metadata>
  <groupId>com.google.guava</groupId>
  <artifactId>guava</artifactId>
  <versioning>
    <latest>33.1.0-jre</latest>
    <release>33.1.0-jre</release>
    <versions>
      <version>32.1.3-jre</version>
      <version>32.1.3-android</version>
      <version>33.0.0-jre</version>
      <version>33.0.0-android</version>
      <version>33.1.0-rc1-jre</version>
      <version>33.1.0-jre</version>
      <version>33.1.0-android</version>
    </versions>
    <lastUpdated>20240315000000</lastUpdated>
  </versioning>
</metadata>"#;

    const SNAPSHOT: &str = r#"<metadata modelVersion="1.1.0">
  <groupId>g</groupId><artifactId>lib</artifactId><version>1.0-SNAPSHOT</version>
  <versioning>
    <snapshot><timestamp>20240101.120000</timestamp><buildNumber>3</buildNumber></snapshot>
    <lastUpdated>20240101120000</lastUpdated>
    <snapshotVersions>
      <snapshotVersion><extension>jar</extension><value>1.0-20240101.120000-3</value></snapshotVersion>
      <snapshotVersion><extension>pom</extension><value>1.0-20240101.120000-3</value></snapshotVersion>
      <snapshotVersion><classifier>sources</classifier><extension>jar</extension><value>1.0-20231231.235959-2</value></snapshotVersion>
    </snapshotVersions>
  </versioning>
</metadata>"#;

    #[test]
    fn the_version_list_parses() {
        let m = Metadata::parse(VERSIONS.as_bytes()).unwrap();
        assert_eq!(m.release.as_deref(), Some("33.1.0-jre"));
        assert_eq!(m.versions.len(), 7);
    }

    #[test]
    fn a_snapshot_resolves_to_its_timestamped_file() {
        let m = Metadata::parse(SNAPSHOT.as_bytes()).unwrap();
        assert_eq!(
            m.snapshot_file_version("1.0-SNAPSHOT", "jar", None)
                .as_deref(),
            Some("1.0-20240101.120000-3")
        );
        assert_eq!(
            m.snapshot_file_version("1.0-SNAPSHOT", "jar", Some("sources"))
                .as_deref(),
            Some("1.0-20231231.235959-2")
        );
    }

    #[test]
    fn older_metadata_names_the_build_by_timestamp_alone() {
        let m = Metadata::parse(
            b"<metadata><versioning><snapshot><timestamp>20240101.120000</timestamp>\
              <buildNumber>7</buildNumber></snapshot></versioning></metadata>",
        )
        .unwrap();
        assert_eq!(
            m.snapshot_file_version("2.1-SNAPSHOT", "jar", None)
                .as_deref(),
            Some("2.1-20240101.120000-7")
        );
    }

    #[test]
    fn a_locally_installed_snapshot_keeps_its_plain_name() {
        let m = Metadata::parse(
            b"<metadata><versioning><snapshot><localCopy>true</localCopy></snapshot>\
              </versioning></metadata>",
        )
        .unwrap();
        assert_eq!(m.snapshot_file_version("1.0-SNAPSHOT", "jar", None), None);
    }

    #[test]
    fn prereleases_are_recognised() {
        for v in [
            "1.0-alpha1",
            "2.0.0-M3",
            "5.11.0-RC1",
            "1.0-SNAPSHOT",
            "3.0-beta-2",
            "24-ea",
            "33.1.0-rc1-jre",
        ] {
            assert!(is_prerelease(v), "{v}");
        }
        for v in ["1.0", "33.0.0-jre", "5.6.15.Final", "2.17.0", "1.0-android"] {
            assert!(!is_prerelease(v), "{v}");
        }
    }

    #[test]
    fn upgrades_stay_on_the_same_flavour_and_skip_prereleases() {
        let m = Metadata::parse(VERSIONS.as_bytes()).unwrap();
        assert_eq!(
            newest(&m.versions, "32.1.3-jre").as_deref(),
            Some("33.1.0-jre")
        );
        assert_eq!(
            newest(&m.versions, "33.0.0-android").as_deref(),
            Some("33.1.0-android")
        );
        assert_eq!(newest(&m.versions, "33.1.0-jre"), None, "already newest");
    }

    #[test]
    fn a_project_on_a_prerelease_is_offered_newer_prereleases() {
        let versions: Vec<String> = ["1.0-M1", "1.0-M2", "0.9"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(newest(&versions, "1.0-M1").as_deref(), Some("1.0-M2"));
        assert_eq!(newest(&versions, "0.8"), Some("0.9".to_string()));
    }

    #[test]
    fn the_newest_release_ignores_prereleases() {
        let m = Metadata {
            versions: vec!["1.0".into(), "1.1".into(), "2.0-RC1".into()],
            release: Some("2.0-RC1".into()),
            ..Metadata::default()
        };
        assert_eq!(newest_release(&m).as_deref(), Some("1.1"));
    }
}
