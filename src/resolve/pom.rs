//! POM parsing: a small XML tree, and the Maven model read out of it.
//!
//! Parsing stops at the tree; nothing here does IO. Walking `<parent>` chains and
//! importing BOMs needs the network, so that lives in the resolver, which hands
//! the assembled chain back to [`effective`].
//!
//! `migrate::maven` reuses this module wholesale (SPEC §11.2), which is why the
//! raw element tree is kept alongside the parsed model.

use std::collections::BTreeMap;

use quick_xml::events::Event;

use super::coord::{Coord, Ga, Scope};
use crate::error::{JrsError, Result};

// ---- a minimal XML tree ----------------------------------------------------

/// One XML element: its local name, its own text, and its children.
///
/// Attributes are dropped — no part of a POM that jrs reads lives in one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Element {
    pub name: String,
    pub text: String,
    pub children: Vec<Element>,
}

impl Element {
    pub fn child(&self, name: &str) -> Option<&Element> {
        self.children.iter().find(|c| c.name == name)
    }

    pub fn children_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Element> {
        self.children.iter().filter(move |c| c.name == name)
    }

    /// Descend through a chain of single children.
    pub fn path(&self, names: &[&str]) -> Option<&Element> {
        let mut cur = self;
        for n in names {
            cur = cur.child(n)?;
        }
        Some(cur)
    }

    /// The trimmed text of a child element, if it has any.
    pub fn text_of(&self, name: &str) -> Option<&str> {
        self.child(name)
            .map(|c| c.text.trim())
            .filter(|t| !t.is_empty())
    }

    /// All `<x>` elements under `<xs>`, the Maven plural convention.
    pub fn list<'a>(&'a self, plural: &str, singular: &'a str) -> Vec<&'a Element> {
        self.child(plural)
            .map(|c| c.children_named(singular).collect())
            .unwrap_or_default()
    }
}

/// Parse an XML document into an [`Element`] tree.
pub fn parse_xml(bytes: &[u8]) -> Result<Element> {
    let mut reader = quick_xml::Reader::from_reader(bytes);
    let config = reader.config_mut();
    config.trim_text(false);
    config.check_end_names = false;

    let mut stack: Vec<Element> = Vec::new();
    let mut root: Option<Element> = None;
    let mut buf = Vec::new();

    loop {
        let event = reader
            .read_event_into(&mut buf)
            .map_err(|e| JrsError::resolve(format!("malformed XML: {e}")))?;
        match event {
            Event::Eof => break,
            Event::Start(e) => stack.push(Element {
                name: e.local_name().as_ref().to_string(),
                ..Default::default()
            }),
            Event::Empty(e) => {
                let empty = Element {
                    name: e.local_name().as_ref().to_string(),
                    ..Default::default()
                };
                match stack.last_mut() {
                    Some(parent) => parent.children.push(empty),
                    None => root = Some(empty),
                }
            }
            Event::Text(t) => {
                if let Some(top) = stack.last_mut() {
                    top.text.push_str(&t.xml10_content());
                }
            }
            Event::CData(t) => {
                if let Some(top) = stack.last_mut() {
                    top.text.push_str(&t.into_inner());
                }
            }
            // `&amp;` and friends arrive as their own event; a POM that contains
            // one wants the character, not the reference.
            Event::GeneralRef(r) => {
                if let Some(top) = stack.last_mut() {
                    top.text.push_str(&r.xml10_content());
                }
            }
            Event::End(_) => {
                let Some(done) = stack.pop() else { continue };
                match stack.last_mut() {
                    Some(parent) => parent.children.push(done),
                    None => root = Some(done),
                }
            }
            _ => {}
        }
        buf.clear();
    }

    root.ok_or_else(|| JrsError::resolve("XML document has no root element"))
}

// ---- the Maven model -------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParentRef {
    pub group: String,
    pub artifact: String,
    pub version: String,
    pub relative_path: Option<String>,
}

/// A `<dependency>` as written, before interpolation or management.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PomDependency {
    pub group: String,
    pub artifact: String,
    pub version: Option<String>,
    pub scope: Option<String>,
    pub optional: bool,
    pub kind: String,
    pub classifier: Option<String>,
    pub exclusions: Vec<Ga>,
}

impl PomDependency {
    pub fn ga(&self) -> Ga {
        Ga::new(&self.group, &self.artifact)
    }

    pub fn scope(&self) -> Scope {
        self.scope
            .as_deref()
            .map(Scope::parse)
            .unwrap_or(Scope::Compile)
    }

    /// Exclusion wildcards (`*`) match everything, as in Maven.
    pub fn excludes(&self, ga: &Ga) -> bool {
        self.exclusions.iter().any(|e| {
            (e.group == "*" || e.group == ga.group)
                && (e.artifact == "*" || e.artifact == ga.artifact)
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BuildInfo {
    pub source_directory: Option<String>,
    pub test_source_directory: Option<String>,
    pub directory: Option<String>,
    pub resource_directories: Vec<String>,
    pub final_name: Option<String>,
    pub plugins: Vec<PluginInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginInfo {
    pub group: String,
    pub artifact: String,
    pub configuration: Option<Element>,
    /// Executions carry configuration too — `maven-shade-plugin` puts its
    /// transformers there.
    pub executions: Vec<Element>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    pub id: String,
    pub active_by_default: bool,
    pub element: Element,
}

/// One POM, exactly as written.
#[derive(Debug, Clone)]
pub struct Pom {
    pub root: Element,
    pub parent: Option<ParentRef>,
    pub group: Option<String>,
    pub artifact: String,
    pub version: Option<String>,
    pub packaging: String,
    pub properties: BTreeMap<String, String>,
    pub dependency_management: Vec<PomDependency>,
    pub dependencies: Vec<PomDependency>,
    pub repositories: Vec<(String, String)>,
    pub modules: Vec<String>,
    pub build: BuildInfo,
    pub profiles: Vec<Profile>,
}

impl Pom {
    pub fn parse(bytes: &[u8]) -> Result<Pom> {
        let root = parse_xml(bytes)?;
        if root.name != "project" {
            return Err(JrsError::resolve(format!(
                "expected a <project> root element, found <{}>",
                root.name
            )));
        }
        Ok(Pom::from_element(root))
    }

    pub fn from_element(root: Element) -> Pom {
        let parent = root.child("parent").and_then(|p| {
            Some(ParentRef {
                group: p.text_of("groupId")?.to_string(),
                artifact: p.text_of("artifactId")?.to_string(),
                version: p.text_of("version")?.to_string(),
                // An explicit but empty `<relativePath/>` means "do not look on
                // disk, resolve from a repository" — distinct from the element
                // being absent, so the emptiness is preserved rather than trimmed
                // away into `None`.
                relative_path: p.child("relativePath").map(|e| e.text.trim().to_string()),
            })
        });

        let profiles = root
            .list("profiles", "profile")
            .into_iter()
            .map(|p| Profile {
                id: p.text_of("id").unwrap_or("").to_string(),
                active_by_default: p
                    .path(&["activation", "activeByDefault"])
                    .map(|e| e.text.trim() == "true")
                    .unwrap_or(false),
                element: p.clone(),
            })
            .collect();

        Pom {
            parent,
            group: root.text_of("groupId").map(str::to_string),
            artifact: root.text_of("artifactId").unwrap_or_default().to_string(),
            version: root.text_of("version").map(str::to_string),
            packaging: root.text_of("packaging").unwrap_or("jar").to_string(),
            properties: read_properties(&root),
            dependency_management: root
                .path(&["dependencyManagement"])
                .map(read_dependencies)
                .unwrap_or_default(),
            dependencies: read_dependencies(&root),
            repositories: read_repositories(&root),
            modules: root
                .list("modules", "module")
                .into_iter()
                .map(|m| m.text.trim().to_string())
                .filter(|m| !m.is_empty())
                .collect(),
            build: read_build(&root),
            profiles,
            root,
        }
    }

    /// The declared group, falling back to the parent's — Maven's inheritance
    /// rule for the two coordinates a child may omit.
    pub fn group_id(&self) -> Option<&str> {
        self.group
            .as_deref()
            .or(self.parent.as_ref().map(|p| p.group.as_str()))
    }

    pub fn version_id(&self) -> Option<&str> {
        self.version
            .as_deref()
            .or(self.parent.as_ref().map(|p| p.version.as_str()))
    }

    pub fn coord(&self) -> Option<Coord> {
        Some(Coord::new(
            self.group_id()?,
            &self.artifact,
            self.version_id()?,
        ))
    }
}

fn read_properties(root: &Element) -> BTreeMap<String, String> {
    root.child("properties")
        .map(|p| {
            p.children
                .iter()
                .map(|c| (c.name.clone(), c.text.trim().to_string()))
                .collect()
        })
        .unwrap_or_default()
}

fn read_dependencies(parent: &Element) -> Vec<PomDependency> {
    parent
        .list("dependencies", "dependency")
        .into_iter()
        .filter_map(|d| {
            Some(PomDependency {
                group: d.text_of("groupId")?.to_string(),
                artifact: d.text_of("artifactId")?.to_string(),
                version: d.text_of("version").map(str::to_string),
                scope: d.text_of("scope").map(str::to_string),
                optional: d.text_of("optional").map(|o| o == "true").unwrap_or(false),
                kind: d.text_of("type").unwrap_or("jar").to_string(),
                classifier: d.text_of("classifier").map(str::to_string),
                exclusions: d
                    .list("exclusions", "exclusion")
                    .into_iter()
                    .filter_map(|e| Some(Ga::new(e.text_of("groupId")?, e.text_of("artifactId")?)))
                    .collect(),
            })
        })
        .collect()
}

fn read_repositories(root: &Element) -> Vec<(String, String)> {
    root.list("repositories", "repository")
        .into_iter()
        .filter_map(|r| {
            let url = r.text_of("url")?.to_string();
            let id = r.text_of("id").unwrap_or("repository").to_string();
            Some((id, url))
        })
        .collect()
}

fn read_build(root: &Element) -> BuildInfo {
    let Some(build) = root.child("build") else {
        return BuildInfo::default();
    };
    BuildInfo {
        source_directory: build.text_of("sourceDirectory").map(str::to_string),
        test_source_directory: build.text_of("testSourceDirectory").map(str::to_string),
        directory: build.text_of("directory").map(str::to_string),
        final_name: build.text_of("finalName").map(str::to_string),
        resource_directories: build
            .list("resources", "resource")
            .into_iter()
            .filter_map(|r| r.text_of("directory").map(str::to_string))
            .collect(),
        plugins: build
            .list("plugins", "plugin")
            .into_iter()
            .filter_map(|p| {
                Some(PluginInfo {
                    group: p
                        .text_of("groupId")
                        .unwrap_or("org.apache.maven.plugins")
                        .to_string(),
                    artifact: p.text_of("artifactId")?.to_string(),
                    configuration: p.child("configuration").cloned(),
                    executions: p
                        .list("executions", "execution")
                        .into_iter()
                        .cloned()
                        .collect(),
                })
            })
            .collect(),
    }
}

// ---- the effective POM -----------------------------------------------------

/// A managed version, from `<dependencyManagement>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Managed {
    pub version: Option<String>,
    pub scope: Option<String>,
    pub exclusions: Vec<Ga>,
}

/// A POM with its parent chain merged and its `${...}` placeholders resolved.
#[derive(Debug, Clone)]
pub struct Effective {
    pub coord: Coord,
    pub packaging: String,
    pub properties: BTreeMap<String, String>,
    pub managed: BTreeMap<Ga, Managed>,
    pub dependencies: Vec<PomDependency>,
    pub repositories: Vec<(String, String)>,
    /// `<dependencyManagement>` entries with `<scope>import</scope>`, which the
    /// resolver must fetch and merge before this POM is usable.
    pub imports: Vec<Coord>,
}

/// Merge a POM with its ancestors, nearest first.
///
/// `chain[0]` is the POM itself; each subsequent entry is its parent. Properties
/// and managed versions from nearer POMs win, which is Maven's inheritance rule.
pub fn effective(chain: &[Pom]) -> Result<Effective> {
    let pom = chain
        .first()
        .ok_or_else(|| JrsError::resolve("empty POM chain"))?;

    // Properties: walk from the furthest ancestor inwards so nearer wins.
    let mut properties = BTreeMap::new();
    for ancestor in chain.iter().rev() {
        properties.extend(ancestor.properties.clone());
    }

    let coord = pom
        .coord()
        .or_else(|| {
            // A child that omits groupId *and* version inherits both, but the
            // chain may itself be truncated; fall back to whatever is known.
            let group = chain.iter().find_map(|p| p.group_id())?;
            let version = chain.iter().find_map(|p| p.version_id())?;
            Some(Coord::new(group, &pom.artifact, version))
        })
        .ok_or_else(|| {
            JrsError::resolve(format!(
                "POM for `{}` has no resolvable groupId/version",
                pom.artifact
            ))
        })?;

    // Built-in properties, always available and never overridable.
    properties.insert("project.groupId".into(), coord.group.clone());
    properties.insert("project.artifactId".into(), coord.artifact.clone());
    properties.insert("project.version".into(), coord.version.clone());
    properties.insert("pom.groupId".into(), coord.group.clone());
    properties.insert("pom.artifactId".into(), coord.artifact.clone());
    properties.insert("pom.version".into(), coord.version.clone());
    properties.insert("version".into(), coord.version.clone());
    if let Some(parent) = &pom.parent {
        properties.insert("project.parent.version".into(), parent.version.clone());
        properties.insert("project.parent.groupId".into(), parent.group.clone());
    }

    let mut managed: BTreeMap<Ga, Managed> = BTreeMap::new();
    let mut imports = Vec::new();
    for ancestor in chain.iter().rev() {
        for d in &ancestor.dependency_management {
            let d = interpolate_dep(d, &properties);
            if d.scope.as_deref() == Some("import") && d.kind == "pom" {
                if let Some(v) = &d.version {
                    imports.push(Coord::new(&d.group, &d.artifact, v));
                }
                continue;
            }
            managed.insert(
                d.ga(),
                Managed {
                    version: d.version.clone(),
                    scope: d.scope.clone(),
                    exclusions: d.exclusions.clone(),
                },
            );
        }
    }

    let dependencies = pom
        .dependencies
        .iter()
        .map(|d| interpolate_dep(d, &properties))
        .collect();

    let mut repositories = Vec::new();
    for ancestor in chain.iter().rev() {
        for (id, url) in &ancestor.repositories {
            let url = interpolate(url, &properties);
            if !repositories
                .iter()
                .any(|(_, u): &(String, String)| *u == url)
            {
                repositories.push((id.clone(), url));
            }
        }
    }

    Ok(Effective {
        coord,
        packaging: pom.packaging.clone(),
        properties,
        managed,
        dependencies,
        repositories,
        imports,
    })
}

impl Effective {
    /// Apply `<dependencyManagement>` to a dependency that omits its version or
    /// scope, and merge in any managed exclusions (SPEC §8.2 step 3).
    pub fn manage(&self, dep: &PomDependency) -> PomDependency {
        let mut out = dep.clone();
        if let Some(m) = self.managed.get(&dep.ga()) {
            if out.version.is_none() {
                out.version = m.version.clone();
            }
            if out.scope.is_none() {
                out.scope = m.scope.clone();
            }
            for e in &m.exclusions {
                if !out.exclusions.contains(e) {
                    out.exclusions.push(e.clone());
                }
            }
        }
        out
    }

    /// Fold an imported BOM's managed versions in, without letting them override
    /// what this POM already manages directly.
    pub fn absorb_import(&mut self, bom: &Effective) {
        for (ga, m) in &bom.managed {
            self.managed.entry(ga.clone()).or_insert_with(|| m.clone());
        }
        for (k, v) in &bom.properties {
            self.properties
                .entry(k.clone())
                .or_insert_with(|| v.clone());
        }
    }
}

fn interpolate_dep(d: &PomDependency, props: &BTreeMap<String, String>) -> PomDependency {
    PomDependency {
        group: interpolate(&d.group, props),
        artifact: interpolate(&d.artifact, props),
        version: d.version.as_ref().map(|v| interpolate(v, props)),
        scope: d.scope.as_ref().map(|s| interpolate(s, props)),
        optional: d.optional,
        kind: d.kind.clone(),
        classifier: d.classifier.clone(),
        exclusions: d
            .exclusions
            .iter()
            .map(|e| {
                Ga::new(
                    interpolate(&e.group, props),
                    interpolate(&e.artifact, props),
                )
            })
            .collect(),
    }
}

/// Expand `${...}` placeholders, following chains up to a small depth.
///
/// Unresolvable placeholders are left verbatim: a `${...}` in an error message is
/// far more diagnosable than an empty string silently becoming a coordinate.
pub fn interpolate(text: &str, props: &BTreeMap<String, String>) -> String {
    const MAX_DEPTH: usize = 8;
    let mut out = text.to_string();
    for _ in 0..MAX_DEPTH {
        if !out.contains("${") {
            break;
        }
        let mut next = String::with_capacity(out.len());
        let mut rest = out.as_str();
        let mut changed = false;
        while let Some(start) = rest.find("${") {
            next.push_str(&rest[..start]);
            let after = &rest[start + 2..];
            match after.find('}') {
                Some(end) => {
                    let key = &after[..end];
                    match props.get(key) {
                        Some(value) => {
                            next.push_str(value);
                            changed = true;
                        }
                        None => next.push_str(&rest[start..start + 2 + end + 1]),
                    }
                    rest = &after[end + 1..];
                }
                None => {
                    next.push_str(rest);
                    rest = "";
                }
            }
        }
        next.push_str(rest);
        out = next;
        if !changed {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHILD: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<project xmlns="http://maven.apache.org/POM/4.0.0">
  <modelVersion>4.0.0</modelVersion>
  <parent>
    <groupId>com.example</groupId>
    <artifactId>parent</artifactId>
    <version>2.0.0</version>
  </parent>
  <artifactId>child</artifactId>
  <properties>
    <guava.version>33.0.0-jre</guava.version>
  </properties>
  <dependencies>
    <dependency>
      <groupId>com.google.guava</groupId>
      <artifactId>guava</artifactId>
      <version>${guava.version}</version>
      <exclusions>
        <exclusion>
          <groupId>com.google.code.findbugs</groupId>
          <artifactId>jsr305</artifactId>
        </exclusion>
      </exclusions>
    </dependency>
    <dependency>
      <groupId>org.apache.commons</groupId>
      <artifactId>commons-lang3</artifactId>
    </dependency>
    <dependency>
      <groupId>junit</groupId>
      <artifactId>junit</artifactId>
      <version>4.13.2</version>
      <scope>test</scope>
    </dependency>
    <dependency>
      <groupId>org.projectlombok</groupId>
      <artifactId>lombok</artifactId>
      <version>1.18.30</version>
      <optional>true</optional>
    </dependency>
  </dependencies>
</project>"#;

    const PARENT: &str = r#"<project>
  <groupId>com.example</groupId>
  <artifactId>parent</artifactId>
  <version>2.0.0</version>
  <packaging>pom</packaging>
  <properties>
    <lang3.version>3.14.0</lang3.version>
    <guava.version>32.0.0-jre</guava.version>
  </properties>
  <dependencyManagement>
    <dependencies>
      <dependency>
        <groupId>org.apache.commons</groupId>
        <artifactId>commons-lang3</artifactId>
        <version>${lang3.version}</version>
      </dependency>
      <dependency>
        <groupId>org.junit</groupId>
        <artifactId>junit-bom</artifactId>
        <version>5.10.2</version>
        <type>pom</type>
        <scope>import</scope>
      </dependency>
    </dependencies>
  </dependencyManagement>
  <repositories>
    <repository><id>internal</id><url>https://nexus.example.com/maven</url></repository>
  </repositories>
</project>"#;

    fn chain() -> Vec<Pom> {
        vec![
            Pom::parse(CHILD.as_bytes()).unwrap(),
            Pom::parse(PARENT.as_bytes()).unwrap(),
        ]
    }

    #[test]
    fn namespaced_element_names_are_stripped() {
        let e = parse_xml(
            b"<pom:project xmlns:pom='x'><pom:artifactId>a</pom:artifactId></pom:project>",
        )
        .unwrap();
        assert_eq!(e.name, "project");
        assert_eq!(e.text_of("artifactId"), Some("a"));
    }

    #[test]
    fn empty_elements_and_cdata_parse() {
        let e = parse_xml(b"<a><b/><c><![CDATA[hi]]></c></a>").unwrap();
        assert_eq!(e.child("b").unwrap().text, "");
        assert_eq!(e.text_of("c"), Some("hi"));
    }

    #[test]
    fn a_non_project_root_is_rejected() {
        let err = Pom::parse(b"<settings/>").unwrap_err();
        assert!(err.to_string().contains("<project>"), "{err}");
    }

    #[test]
    fn coordinates_are_inherited_from_the_parent() {
        let pom = Pom::parse(CHILD.as_bytes()).unwrap();
        assert_eq!(pom.coord().unwrap().to_string(), "com.example:child:2.0.0");
    }

    #[test]
    fn properties_interpolate_with_the_nearest_definition_winning() {
        let eff = effective(&chain()).unwrap();
        let guava = eff
            .dependencies
            .iter()
            .find(|d| d.artifact == "guava")
            .unwrap();
        assert_eq!(
            guava.version.as_deref(),
            Some("33.0.0-jre"),
            "the child's property must override the parent's"
        );
    }

    #[test]
    fn dependency_management_fills_in_a_missing_version() {
        let eff = effective(&chain()).unwrap();
        let lang3 = eff
            .dependencies
            .iter()
            .find(|d| d.artifact == "commons-lang3")
            .unwrap();
        assert_eq!(lang3.version, None, "as written, the version is absent");
        let managed = eff.manage(lang3);
        assert_eq!(managed.version.as_deref(), Some("3.14.0"));
    }

    #[test]
    fn bom_imports_are_reported_for_the_resolver_to_fetch() {
        let eff = effective(&chain()).unwrap();
        assert_eq!(
            eff.imports,
            vec![Coord::new("org.junit", "junit-bom", "5.10.2")]
        );
        assert!(
            !eff.managed.contains_key(&Ga::new("org.junit", "junit-bom")),
            "an import is not itself a managed dependency"
        );
    }

    #[test]
    fn absorbing_an_import_does_not_override_direct_management() {
        let mut eff = effective(&chain()).unwrap();
        let bom = Effective {
            coord: Coord::new("org.junit", "junit-bom", "5.10.2"),
            packaging: "pom".into(),
            properties: BTreeMap::new(),
            managed: [
                (
                    Ga::new("org.apache.commons", "commons-lang3"),
                    Managed {
                        version: Some("9.9.9".into()),
                        scope: None,
                        exclusions: vec![],
                    },
                ),
                (
                    Ga::new("org.junit.jupiter", "junit-jupiter"),
                    Managed {
                        version: Some("5.10.2".into()),
                        scope: None,
                        exclusions: vec![],
                    },
                ),
            ]
            .into_iter()
            .collect(),
            dependencies: vec![],
            repositories: vec![],
            imports: vec![],
        };
        eff.absorb_import(&bom);
        assert_eq!(
            eff.managed[&Ga::new("org.apache.commons", "commons-lang3")]
                .version
                .as_deref(),
            Some("3.14.0")
        );
        assert_eq!(
            eff.managed[&Ga::new("org.junit.jupiter", "junit-jupiter")]
                .version
                .as_deref(),
            Some("5.10.2")
        );
    }

    #[test]
    fn scopes_optionals_and_exclusions_are_read() {
        let eff = effective(&chain()).unwrap();
        let junit = eff
            .dependencies
            .iter()
            .find(|d| d.artifact == "junit")
            .unwrap();
        assert_eq!(junit.scope(), Scope::Test);
        assert!(!junit.scope().is_transitive());

        let lombok = eff
            .dependencies
            .iter()
            .find(|d| d.artifact == "lombok")
            .unwrap();
        assert!(lombok.optional);

        let guava = eff
            .dependencies
            .iter()
            .find(|d| d.artifact == "guava")
            .unwrap();
        assert!(guava.excludes(&Ga::new("com.google.code.findbugs", "jsr305")));
        assert!(!guava.excludes(&Ga::new("com.google.guava", "failureaccess")));
    }

    #[test]
    fn wildcard_exclusions_match_everything() {
        let dep = PomDependency {
            group: "g".into(),
            artifact: "a".into(),
            version: None,
            scope: None,
            optional: false,
            kind: "jar".into(),
            classifier: None,
            exclusions: vec![Ga::new("*", "*")],
        };
        assert!(dep.excludes(&Ga::new("anything", "at-all")));
    }

    #[test]
    fn repositories_are_inherited() {
        let eff = effective(&chain()).unwrap();
        assert_eq!(
            eff.repositories,
            vec![(
                "internal".to_string(),
                "https://nexus.example.com/maven".to_string()
            )]
        );
    }

    #[test]
    fn built_in_properties_are_available() {
        let props = effective(&chain()).unwrap().properties;
        assert_eq!(props["project.version"], "2.0.0");
        assert_eq!(props["project.artifactId"], "child");
        assert_eq!(props["project.groupId"], "com.example");
    }

    #[test]
    fn unresolvable_placeholders_are_left_verbatim() {
        let props = BTreeMap::new();
        assert_eq!(interpolate("${nope}", &props), "${nope}");
        assert_eq!(interpolate("a-${nope}-b", &props), "a-${nope}-b");
    }

    #[test]
    fn placeholder_chains_resolve() {
        let props: BTreeMap<String, String> = [
            ("a".to_string(), "${b}".to_string()),
            ("b".to_string(), "final".to_string()),
        ]
        .into_iter()
        .collect();
        assert_eq!(interpolate("${a}", &props), "final");
    }

    #[test]
    fn self_referential_properties_terminate() {
        let props: BTreeMap<String, String> = [("loop".to_string(), "${loop}".to_string())]
            .into_iter()
            .collect();
        assert_eq!(interpolate("${loop}", &props), "${loop}");
    }

    #[test]
    fn build_settings_and_plugins_are_available_to_migrate() {
        let pom = Pom::parse(
            br#"<project>
              <groupId>g</groupId><artifactId>a</artifactId><version>1</version>
              <modules><module>core</module><module>web</module></modules>
              <build>
                <sourceDirectory>src</sourceDirectory>
                <directory>build</directory>
                <resources><resource><directory>res</directory></resource></resources>
                <plugins>
                  <plugin>
                    <artifactId>maven-jar-plugin</artifactId>
                    <configuration><archive><manifest>
                      <mainClass>com.example.Main</mainClass>
                    </manifest></archive></configuration>
                  </plugin>
                </plugins>
              </build>
            </project>"#,
        )
        .unwrap();
        assert_eq!(pom.modules, vec!["core", "web"]);
        assert_eq!(pom.build.source_directory.as_deref(), Some("src"));
        assert_eq!(pom.build.directory.as_deref(), Some("build"));
        assert_eq!(pom.build.resource_directories, vec!["res"]);
        let plugin = &pom.build.plugins[0];
        assert_eq!(plugin.artifact, "maven-jar-plugin");
        assert_eq!(
            plugin
                .configuration
                .as_ref()
                .unwrap()
                .path(&["archive", "manifest"])
                .unwrap()
                .text_of("mainClass"),
            Some("com.example.Main")
        );
    }

    #[test]
    fn profiles_record_whether_they_are_active_by_default() {
        let pom = Pom::parse(
            br#"<project><groupId>g</groupId><artifactId>a</artifactId><version>1</version>
              <profiles>
                <profile><id>ci</id></profile>
                <profile><id>dev</id><activation><activeByDefault>true</activeByDefault></activation></profile>
              </profiles>
            </project>"#,
        )
        .unwrap();
        assert_eq!(pom.profiles.len(), 2);
        assert!(!pom.profiles[0].active_by_default);
        assert!(pom.profiles[1].active_by_default);
    }
}
