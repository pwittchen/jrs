//! Gradle module metadata: where a Kotlin Multiplatform library keeps its
//! JVM classes.
//!
//! A Multiplatform library is published as a root artifact, `ktor-server-core`,
//! whose POM lists the dependencies of its common code and whose jar holds no
//! JVM classes at all, and one artifact per platform, `ktor-server-core-jvm`.
//! Only the `.module` file beside the root POM says which is which: each of
//! its variants names a platform and, for the platform artifacts, the module
//! it is `available-at`. Gradle follows that to the JVM artifact; a POM-only
//! resolver finds nothing to compile against. jrs follows it too, so a
//! Multiplatform library is declared by its root coordinate, as a Gradle
//! build declares it.

use crate::json::Json;

use super::coord::Coord;

/// The line the Gradle publishing plugin writes into every POM it publishes
/// beside a `.module` file.
const MARKER: &[u8] = b"published-with-gradle-metadata";

/// Whether a POM was published with Gradle module metadata beside it.
#[must_use]
pub fn announced(pom: &[u8]) -> bool {
    pom.windows(MARKER.len()).any(|w| w == MARKER)
}

/// The artifact a module file's JVM variant is `available-at`, when it is
/// another artifact than `coord`: the one that holds the JVM classes.
///
/// A variant counts when it is a library for the JVM platform, as Kotlin
/// Multiplatform marks one (`org.jetbrains.kotlin.platform.type = jvm`), and
/// for the runtime or the API; the runtime variant is preferred, since it
/// carries every dependency the program needs. A library that is its own JVM
/// artifact has no `available-at`, and gets `None`: its POM already says
/// everything.
///
/// # Errors
///
/// A message when the file is not JSON.
pub fn jvm_variant(module: &[u8], coord: &Coord) -> Result<Option<Coord>, String> {
    let text = std::str::from_utf8(module).map_err(|_| "it is not UTF-8".to_string())?;
    let doc = Json::parse(text)?;
    let jvm = |usage: &str| {
        doc.get("variants")
            .map(Json::items)
            .unwrap_or_default()
            .iter()
            .find(|variant| {
                let attribute = |name: &str| {
                    variant
                        .get("attributes")
                        .and_then(|a| a.get(name))
                        .and_then(Json::as_str)
                };
                attribute("org.jetbrains.kotlin.platform.type") == Some("jvm")
                    && attribute("org.gradle.category").is_none_or(|c| c == "library")
                    && attribute("org.gradle.usage") == Some(usage)
            })
            .and_then(|variant| variant.get("available-at"))
            .and_then(|at| {
                Some(Coord::new(
                    at.get("group")?.as_str()?,
                    at.get("module")?.as_str()?,
                    at.get("version")?.as_str()?,
                ))
            })
    };
    Ok(jvm("java-runtime")
        .or_else(|| jvm("java-api"))
        .filter(|target| target.ga() != coord.ga()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape of `ktor-server-core-3.6.0.module`, cut down to one variant
    /// per kind.
    const MULTIPLATFORM: &str = r#"{
      "formatVersion": "1.1",
      "component": {"group": "io.ktor", "module": "ktor-server-core", "version": "3.6.0"},
      "variants": [
        {"name": "metadataApiElements",
         "attributes": {"org.gradle.category": "library", "org.gradle.usage": "kotlin-metadata",
                        "org.jetbrains.kotlin.platform.type": "common"}},
        {"name": "jsApiElements-published",
         "attributes": {"org.gradle.category": "library", "org.gradle.usage": "kotlin-api",
                        "org.jetbrains.kotlin.platform.type": "js"},
         "available-at": {"url": "../../ktor-server-core-js/3.6.0/ktor-server-core-js-3.6.0.module",
                          "group": "io.ktor", "module": "ktor-server-core-js", "version": "3.6.0"}},
        {"name": "jvmSourcesElements-published",
         "attributes": {"org.gradle.category": "documentation", "org.gradle.usage": "java-runtime",
                        "org.jetbrains.kotlin.platform.type": "jvm"},
         "available-at": {"url": "../../ktor-server-core-jvm-sources/3.6.0/x.module",
                          "group": "io.ktor", "module": "ktor-server-core-jvm-sources", "version": "3.6.0"}},
        {"name": "jvmApiElements-published",
         "attributes": {"org.gradle.category": "library", "org.gradle.usage": "java-api",
                        "org.jetbrains.kotlin.platform.type": "jvm"},
         "available-at": {"url": "../../ktor-server-core-jvm/3.6.0/ktor-server-core-jvm-3.6.0.module",
                          "group": "io.ktor", "module": "ktor-server-core-jvm", "version": "3.6.0"}},
        {"name": "jvmRuntimeElements-published",
         "attributes": {"org.gradle.category": "library", "org.gradle.usage": "java-runtime",
                        "org.jetbrains.kotlin.platform.type": "jvm"},
         "available-at": {"url": "../../ktor-server-core-jvm/3.6.0/ktor-server-core-jvm-3.6.0.module",
                          "group": "io.ktor", "module": "ktor-server-core-jvm", "version": "3.6.0"}}
      ]
    }"#;

    fn root() -> Coord {
        Coord::new("io.ktor", "ktor-server-core", "3.6.0")
    }

    #[test]
    fn a_multiplatform_root_leads_to_its_jvm_artifact() {
        assert_eq!(
            jvm_variant(MULTIPLATFORM.as_bytes(), &root()).unwrap(),
            Some(Coord::new("io.ktor", "ktor-server-core-jvm", "3.6.0"))
        );
    }

    #[test]
    fn a_library_that_is_its_own_jvm_artifact_leads_nowhere() {
        // kotlin-stdlib, or the -jvm artifact itself: variants without
        // `available-at`, or pointing back at the same artifact.
        let own = r#"{"variants": [
          {"name": "jvmRuntimeElements",
           "attributes": {"org.gradle.usage": "java-runtime",
                          "org.jetbrains.kotlin.platform.type": "jvm"}}]}"#;
        assert_eq!(jvm_variant(own.as_bytes(), &root()).unwrap(), None);
        let jvm = Coord::new("io.ktor", "ktor-server-core-jvm", "3.6.0");
        assert_eq!(jvm_variant(MULTIPLATFORM.as_bytes(), &jvm).unwrap(), None);
    }

    #[test]
    fn a_plain_java_library_is_left_alone() {
        let java = r#"{"variants": [
          {"name": "runtimeElements",
           "attributes": {"org.gradle.category": "library", "org.gradle.usage": "java-runtime"}}]}"#;
        assert_eq!(jvm_variant(java.as_bytes(), &root()).unwrap(), None);
    }

    #[test]
    fn only_a_marked_pom_is_asked_about() {
        assert!(announced(
            b"<project><!-- do_not_remove: published-with-gradle-metadata --></project>"
        ));
        assert!(!announced(b"<project></project>"));
        assert!(jvm_variant(b"{not json", &root()).is_err());
    }
}
