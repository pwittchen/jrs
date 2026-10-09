//! `jrs package --sbom`: a `CycloneDX` 1.5 JSON document of what the package
//! ships — the runtime graph, each component with its coordinate as a
//! package URL, its checksum, its licences and its place in the graph.
//!
//! It is written by [`crate::json`], with no timestamp and no serial number,
//! so the same inputs give the same bytes, like the jar beside it.

use std::fmt::Write as _;

use crate::json::Json;
use crate::manifest::Manifest;
use crate::resolve::coord::{Coord, Ga, expand_classifier};
use crate::resolve::license::License;
use crate::resolve::{LocalJar, ResolvedPackage};

/// The `CycloneDX` specification version written.
pub const SPEC_VERSION: &str = "1.5";

/// One runtime package, with what the SBOM says of it that the graph does
/// not hold.
pub struct Component<'a> {
    pub package: &'a ResolvedPackage,
    pub licenses: &'a [License],
    /// `sha1:<hex>` or `sha256:<hex>`: `jrs.lock`'s pin, or the jar's own
    /// hash when it has none.
    pub checksum: Option<String>,
}

/// The document for `manifest`'s package, whose runtime graph is
/// `components` (in the graph's order) and `locals`.
#[must_use]
pub fn cyclonedx(manifest: &Manifest, components: &[Component<'_>], locals: &[&LocalJar]) -> Json {
    let root = format!("{}@{}", manifest.name, manifest.version);
    let kind = if manifest.main_class.is_some() {
        "application"
    } else {
        "library"
    };
    let shipped: Vec<Ga> = components.iter().map(|c| c.package.ga()).collect();

    let mut items = Vec::new();
    let mut graph = Vec::new();
    let mut direct = Vec::new();
    for c in components {
        let p = c.package;
        let purl = purl(&p.coord, &p.packaging);
        if p.direct {
            direct.push(Json::string(purl.clone()));
        }
        let mut fields = vec![
            ("type", Json::string("library")),
            ("bom-ref", Json::string(purl.clone())),
            ("group", Json::string(p.coord.group.clone())),
            ("name", Json::string(p.coord.artifact.clone())),
            ("version", Json::string(p.coord.version.clone())),
        ];
        if let Some(hash) = c.checksum.as_deref().and_then(hash) {
            fields.push(("hashes", Json::Array(vec![hash])));
        }
        if !c.licenses.is_empty() {
            fields.push((
                "licenses",
                Json::Array(c.licenses.iter().map(license).collect()),
            ));
        }
        fields.push(("purl", Json::string(purl.clone())));
        items.push(Json::object(fields));

        let depends: Vec<Json> = p
            .dependencies
            .iter()
            .filter_map(|ga| {
                let i = shipped.iter().position(|s| s == ga)?;
                let to = components[i].package;
                Some(Json::string(purl_of(to)))
            })
            .collect();
        graph.push(Json::object([
            ("ref", Json::string(purl)),
            ("dependsOn", Json::Array(depends)),
        ]));
    }
    for l in locals {
        let reference = format!("local:{}", l.path);
        direct.push(Json::string(reference.clone()));
        let mut fields = vec![
            ("type", Json::string("library")),
            ("bom-ref", Json::string(reference.clone())),
            ("name", Json::string(l.name.clone())),
        ];
        if let Some(hash) = l.checksum.as_deref().and_then(hash) {
            fields.push(("hashes", Json::Array(vec![hash])));
        }
        fields.push((
            "description",
            Json::string(format!("a local jar, {}", l.path)),
        ));
        items.push(Json::object(fields));
        graph.push(Json::object([
            ("ref", Json::string(reference)),
            ("dependsOn", Json::Array(Vec::new())),
        ]));
    }
    graph.insert(
        0,
        Json::object([
            ("ref", Json::string(root.clone())),
            ("dependsOn", Json::Array(direct)),
        ]),
    );

    Json::object([
        ("bomFormat", Json::string("CycloneDX")),
        ("specVersion", Json::string(SPEC_VERSION)),
        ("version", Json::Int(1)),
        ("metadata", metadata(manifest, kind, root)),
        ("components", Json::Array(items)),
        ("dependencies", Json::Array(graph)),
    ])
}

/// What wrote the document, and what it describes: the project itself.
fn metadata(manifest: &Manifest, kind: &str, root: String) -> Json {
    let jrs = Json::object([
        ("type", Json::string("application")),
        ("name", Json::string("jrs")),
        ("version", Json::string(env!("CARGO_PKG_VERSION"))),
    ]);
    Json::object([
        (
            "tools",
            Json::object([("components", Json::Array(vec![jrs]))]),
        ),
        (
            "component",
            Json::object([
                ("type", Json::string(kind)),
                ("bom-ref", Json::string(root)),
                ("name", Json::string(manifest.name.clone())),
                ("version", Json::string(manifest.version.clone())),
            ]),
        ),
    ])
}

fn purl_of(p: &ResolvedPackage) -> String {
    purl(&p.coord, &p.packaging)
}

/// `pkg:maven/com.google.guava/guava@33.0.0-jre`, with the classifier — the
/// host's, for a platform one, since that is the jar shipped — and a type
/// other than `jar` as qualifiers.
#[must_use]
pub fn purl(coord: &Coord, packaging: &str) -> String {
    let mut purl = format!(
        "pkg:maven/{}/{}@{}",
        encode(&coord.group),
        encode(&coord.artifact),
        encode(&coord.version)
    );
    let mut qualifiers = Vec::new();
    if let Some(c) = &coord.classifier {
        qualifiers.push(format!("classifier={}", encode(&expand_classifier(c))));
    }
    if packaging == "pom" {
        qualifiers.push("type=pom".to_string());
    }
    if !qualifiers.is_empty() {
        purl.push('?');
        purl.push_str(&qualifiers.join("&"));
    }
    purl
}

/// Percent-encoding for a package URL's parts: everything but letters,
/// digits and `.-_~`.
fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

/// `{"alg": "SHA-256", "content": …}` from `sha256:<hex>`.
fn hash(checksum: &str) -> Option<Json> {
    let (alg, hex) = checksum.split_once(':')?;
    let alg = match alg {
        "sha1" => "SHA-1",
        "sha256" => "SHA-256",
        "sha512" => "SHA-512",
        _ => return None,
    };
    Some(Json::object([
        ("alg", Json::string(alg)),
        ("content", Json::string(hex)),
    ]))
}

/// A licence as `CycloneDX` writes one: by SPDX identifier when it has one,
/// by name otherwise, with the URL either way.
fn license(l: &License) -> Json {
    let mut fields = Vec::new();
    match (l.spdx, &l.name) {
        (Some(id), _) => fields.push(("id", Json::string(id))),
        (None, Some(name)) => fields.push(("name", Json::string(name.clone()))),
        (None, None) => fields.push(("name", Json::string(l.url.clone().unwrap_or_default()))),
    }
    if let Some(url) = &l.url {
        fields.push(("url", Json::string(url.clone())));
    }
    Json::object([("license", Json::object(fields))])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolve::Classpath;
    use std::path::{Path, PathBuf};

    fn package(gav: &str, direct: bool, dependencies: &[&str]) -> ResolvedPackage {
        ResolvedPackage {
            coord: Coord::parse(gav).unwrap(),
            classpath: Classpath::Compile,
            packaging: "jar".into(),
            depth: if direct { 1 } else { 2 },
            direct,
            dependencies: dependencies.iter().map(|d| Ga::parse(d).unwrap()).collect(),
            jar: Some(PathBuf::from("/c/x.jar")),
            checksum: Some("sha1:abc".into()),
            mediated: false,
            managed: false,
        }
    }

    #[test]
    fn package_urls_carry_the_classifier_and_a_type_other_than_jar() {
        let c = Coord::parse("org.lwjgl:lwjgl:3.3.3").unwrap();
        assert_eq!(purl(&c, "jar"), "pkg:maven/org.lwjgl/lwjgl@3.3.3");
        let natives = c.clone().with_classifier(Some("natives-linux".into()));
        assert_eq!(
            purl(&natives, "jar"),
            "pkg:maven/org.lwjgl/lwjgl@3.3.3?classifier=natives-linux"
        );
        assert_eq!(
            purl(&Coord::parse("g:bom:1+x").unwrap(), "pom"),
            "pkg:maven/g/bom@1%2Bx?type=pom"
        );
    }

    #[test]
    fn the_document_lists_components_their_licences_and_the_graph() {
        let manifest = Manifest::parse(
            "[project]\nname = \"app\"\nversion = \"1.0\"\nmain-class = \"a.Main\"\n",
            Path::new("/p/jrs.toml"),
            Path::new("/p"),
        )
        .unwrap();
        let guava = package(
            "com.google.guava:guava:33.0.0-jre",
            true,
            &["g:failureaccess"],
        );
        let access = package("g:failureaccess:1.0", false, &[]);
        let apache = [License {
            name: Some("Apache License, Version 2.0".into()),
            url: Some("https://www.apache.org/licenses/LICENSE-2.0".into()),
            spdx: Some("Apache-2.0"),
        }];
        let local = LocalJar {
            name: "driver".into(),
            path: "libs/driver.jar".into(),
            classpath: Classpath::Compile,
            checksum: Some("sha256:def".into()),
            jar: None,
        };
        let doc = cyclonedx(
            &manifest,
            &[
                Component {
                    package: &guava,
                    licenses: &apache,
                    checksum: guava.checksum.clone(),
                },
                Component {
                    package: &access,
                    licenses: &[],
                    checksum: Some("sha256:123".into()),
                },
            ],
            &[&local],
        );
        let text = doc.render();
        assert_eq!(doc.get("bomFormat").unwrap().as_str(), Some("CycloneDX"));
        assert_eq!(doc.get("specVersion").unwrap().as_str(), Some("1.5"));
        let root = doc.get("metadata").unwrap().get("component").unwrap();
        assert_eq!(root.get("type").unwrap().as_str(), Some("application"));

        let components = doc.get("components").unwrap().items();
        assert_eq!(components.len(), 3);
        assert_eq!(
            components[0].get("purl").unwrap().as_str(),
            Some("pkg:maven/com.google.guava/guava@33.0.0-jre")
        );
        assert!(text.contains("\"alg\": \"SHA-1\""), "{text}");
        assert!(text.contains("\"id\": \"Apache-2.0\""), "{text}");
        assert!(components[1].get("licenses").is_none(), "none declared");
        assert_eq!(
            components[2].get("bom-ref").unwrap().as_str(),
            Some("local:libs/driver.jar")
        );

        let graph = doc.get("dependencies").unwrap().items();
        assert_eq!(graph[0].get("ref").unwrap().as_str(), Some("app@1.0"));
        let direct: Vec<&str> = graph[0]
            .get("dependsOn")
            .unwrap()
            .items()
            .iter()
            .filter_map(Json::as_str)
            .collect();
        assert_eq!(
            direct,
            [
                "pkg:maven/com.google.guava/guava@33.0.0-jre",
                "local:libs/driver.jar"
            ],
            "the direct dependencies, not the transitive one"
        );
        assert_eq!(
            graph[1].get("dependsOn").unwrap().items()[0].as_str(),
            Some("pkg:maven/g/failureaccess@1.0")
        );
    }
}
