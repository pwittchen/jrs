//! Writing a project into a local Maven repository (`specs/REACH.md` §4).
//!
//! The layout is Maven's: `<repo>/<group path>/<artifact>/<version>/` holds
//! the jar and a POM, and `maven-metadata-local.xml` beside the version
//! directories lists what has been installed. A local repository is a
//! directory on the developer's machine, so this is the one kind of
//! publishing jrs does (SPEC §1.2): no credentials, signing or staging.
//!
//! A file lands the way a cached one does ([`write_atomic`]), so a consumer
//! resolving at the same moment never reads half a jar. `.sha1` files go
//! beside the jar and the POM, which `mvn install` leaves out, so that a jrs
//! consumer has something to verify against and no warning to print.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use quick_xml::Writer;
use quick_xml::events::{BytesDecl, BytesEnd, BytesStart, BytesText, Event};

use crate::error::{IoResultExt, JrsError, Result};
use crate::manifest::{Exclusion, Manifest};
use crate::resolve::cache::write_atomic;
use crate::resolve::coord::{Ga, SNAPSHOT_SUFFIX, compare_versions, expand_classifier};
use crate::resolve::metadata::Metadata;
use crate::resolve::pom::parse_xml;
use crate::resolve::repo::sha1_hex;
use crate::resolve::{Classpath, Resolution};
use crate::workspace::Upstream;

const LOCAL_METADATA: &str = "maven-metadata-local.xml";

/// What would stop `manifest` from being installed, before anything is built.
///
/// # Errors
///
/// [`JrsError::Manifest`] when the project has no `project.group`;
/// [`JrsError::Usage`] when it depends on a local jar, which no consumer of
/// its POM could resolve.
pub fn check(manifest: &Manifest) -> Result<()> {
    manifest.require_group("install")?;
    if let Some(local) = manifest.dependencies.iter().find(|d| d.is_local()) {
        return Err(JrsError::usage(format!(
            "`jrs install` cannot write a POM for `{}`: it depends on the local jar `{}`, and \
             no project that resolves the POM could find that\n\n\
             publish the jar to a repository and depend on its coordinate",
            manifest.name, local
        )));
    }
    Ok(())
}

/// The repository to write to: `--repo`, then `<localRepository>` in
/// `~/.m2/settings.xml`, then `~/.m2/repository`.
///
/// # Errors
///
/// [`JrsError::Usage`] when none of them names a directory, or the settings
/// file cannot be understood; [`JrsError::Io`] when the path cannot be made
/// absolute or the settings file cannot be read.
pub fn repository(flag: Option<&Path>) -> Result<PathBuf> {
    if let Some(dir) = flag {
        return std::path::absolute(dir).path(dir);
    }
    let home = std::env::home_dir().ok_or_else(|| {
        JrsError::usage(
            "`jrs install` could not find your home directory, and with it \
             ~/.m2/repository\n\nname the repository with `--repo <dir>`",
        )
    })?;
    default_repository(&home)
}

/// The repository Maven itself installs into for a user whose home is `home`.
///
/// This is the one piece of Maven's configuration jrs reads: a repository
/// moved in `settings.xml` is where `mvn` looks, so an install anywhere else
/// would reach nobody. A file that cannot be understood stops the command
/// rather than sending the jar somewhere the user did not mean.
fn default_repository(home: &Path) -> Result<PathBuf> {
    let settings = home.join(".m2").join("settings.xml");
    let bytes = match std::fs::read(&settings) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(home.join(".m2").join("repository"));
        }
        Err(e) => return Err(JrsError::io(&settings, e)),
    };
    let refuse = |why: String| {
        JrsError::usage(format!(
            "{}: {why}\n\nname the repository with `--repo <dir>`",
            settings.display()
        ))
    };
    let root = parse_xml(&bytes).map_err(|e| refuse(e.to_string()))?;
    let Some(configured) = root.text_of("localRepository") else {
        return Ok(home.join(".m2").join("repository"));
    };
    let path = configured.replace("${user.home}", &home.to_string_lossy());
    if path.contains("${") {
        return Err(refuse(format!(
            "`<localRepository>` is `{configured}`, and `${{user.home}}` is the only \
             property jrs expands there"
        )));
    }
    std::path::absolute(&path).path(&path)
}

/// One `<dependency>` of an installed POM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PomDependency {
    pub group: String,
    pub artifact: String,
    pub version: String,
    pub classifier: Option<String>,
    /// `pom` for an artifact that is only a POM; `None` for a jar.
    pub kind: Option<String>,
    pub scope: Classpath,
    pub exclusions: Vec<Exclusion>,
}

/// The POM's dependencies, as the project was built rather than as it was
/// declared: the members it names first, then `effective_dependencies()`,
/// each in declaration order and each at the version the build used, so that
/// a consumer needs none of the producer's `[managed]`.
///
/// # Errors
///
/// [`JrsError::Build`] when a dependency has no version the build settled
/// on, or a member it depends on has no group.
pub fn pom_dependencies(
    manifest: &Manifest,
    resolution: &Resolution,
    upstream: &[Upstream],
) -> Result<Vec<PomDependency>> {
    let mut dependencies = Vec::new();
    for member in upstream.iter().filter(|u| u.direct) {
        if member.classpath == Classpath::Test {
            continue;
        }
        dependencies.push(PomDependency {
            group: member.group.clone().ok_or_else(|| {
                JrsError::build(format!(
                    "`{}` depends on the member `{}`, which has no `project.group`",
                    manifest.name, member.name
                ))
            })?,
            artifact: member.name.clone(),
            version: member.version.clone(),
            classifier: None,
            kind: None,
            scope: member.classpath,
            exclusions: Vec::new(),
        });
    }
    for dep in manifest.effective_dependencies() {
        let ga = Ga::new(&dep.group, &dep.artifact);
        let package = resolution.get(&ga);
        let version = package.map_or_else(|| dep.version.clone(), |p| p.coord.version.clone());
        if version.is_empty() {
            return Err(JrsError::build(format!(
                "`{dep}` has no version the build settled on, so the POM cannot name one"
            )));
        }
        dependencies.push(PomDependency {
            group: dep.group.clone(),
            artifact: dep.artifact.clone(),
            version,
            classifier: dep
                .classifier
                .as_deref()
                .map(|c| expand_classifier(c).into_owned()),
            kind: package
                .filter(|p| p.packaging == "pom")
                .map(|p| p.packaging.clone()),
            scope: if dep.compile_only {
                Classpath::Provided
            } else if dep.runtime_only {
                Classpath::Runtime
            } else {
                Classpath::Compile
            },
            exclusions: dep.exclusions.clone(),
        });
    }
    Ok(dependencies)
}

type Xml = Writer<Vec<u8>>;

fn open(xml: &mut Xml, name: &str) -> std::io::Result<()> {
    xml.write_event(Event::Start(BytesStart::new(name)))
}

fn close(xml: &mut Xml, name: &str) -> std::io::Result<()> {
    xml.write_event(Event::End(BytesEnd::new(name)))
}

fn field(xml: &mut Xml, name: &str, text: &str) -> std::io::Result<()> {
    open(xml, name)?;
    xml.write_event(Event::Text(BytesText::new(text)))?;
    close(xml, name)
}

fn document() -> std::io::Result<Xml> {
    let mut xml = Writer::new_with_indent(Vec::new(), b' ', 2);
    xml.write_event(Event::Decl(BytesDecl::new("1.0", Some("UTF-8"), None)))?;
    Ok(xml)
}

fn finished(xml: Xml) -> Vec<u8> {
    let mut bytes = xml.into_inner();
    let _ = bytes.write_all(b"\n");
    bytes
}

fn write_pom(
    group: &str,
    artifact: &str,
    version: &str,
    dependencies: &[PomDependency],
) -> std::io::Result<Vec<u8>> {
    let mut xml = document()?;
    let mut project = BytesStart::new("project");
    project.push_attribute(("xmlns", "http://maven.apache.org/POM/4.0.0"));
    xml.write_event(Event::Start(project))?;
    field(&mut xml, "modelVersion", "4.0.0")?;
    field(&mut xml, "groupId", group)?;
    field(&mut xml, "artifactId", artifact)?;
    field(&mut xml, "version", version)?;
    field(&mut xml, "packaging", "jar")?;
    if !dependencies.is_empty() {
        open(&mut xml, "dependencies")?;
        for dep in dependencies {
            open(&mut xml, "dependency")?;
            field(&mut xml, "groupId", &dep.group)?;
            field(&mut xml, "artifactId", &dep.artifact)?;
            field(&mut xml, "version", &dep.version)?;
            if let Some(classifier) = &dep.classifier {
                field(&mut xml, "classifier", classifier)?;
            }
            if let Some(kind) = &dep.kind {
                field(&mut xml, "type", kind)?;
            }
            field(&mut xml, "scope", dep.scope.as_str())?;
            if !dep.exclusions.is_empty() {
                open(&mut xml, "exclusions")?;
                for exclusion in &dep.exclusions {
                    open(&mut xml, "exclusion")?;
                    field(&mut xml, "groupId", &exclusion.group)?;
                    field(&mut xml, "artifactId", &exclusion.artifact)?;
                    close(&mut xml, "exclusion")?;
                }
                close(&mut xml, "exclusions")?;
            }
            close(&mut xml, "dependency")?;
        }
        close(&mut xml, "dependencies")?;
    }
    close(&mut xml, "project")?;
    Ok(finished(xml))
}

/// The POM for `group:artifact:version`, in a fixed order: the same project
/// always installs the same bytes. No `<dependencyManagement>`, no
/// `<parent>`: it states what this jar was built against, and a consumer's
/// own mediation decides the rest.
///
/// # Errors
///
/// [`JrsError::Build`] when the XML writer refuses it.
pub fn render_pom(
    group: &str,
    artifact: &str,
    version: &str,
    dependencies: &[PomDependency],
) -> Result<Vec<u8>> {
    write_pom(group, artifact, version, dependencies)
        .map_err(|e| JrsError::build(format!("could not write the POM of `{artifact}`: {e}")))
}

fn write_metadata(group: &str, artifact: &str, versions: &[String]) -> std::io::Result<Vec<u8>> {
    let mut xml = document()?;
    open(&mut xml, "metadata")?;
    field(&mut xml, "groupId", group)?;
    field(&mut xml, "artifactId", artifact)?;
    open(&mut xml, "versioning")?;
    if let Some(release) = versions.iter().rfind(|v| !v.ends_with(SNAPSHOT_SUFFIX)) {
        field(&mut xml, "release", release)?;
    }
    open(&mut xml, "versions")?;
    for version in versions {
        field(&mut xml, "version", version)?;
    }
    close(&mut xml, "versions")?;
    close(&mut xml, "versioning")?;
    close(&mut xml, "metadata")?;
    Ok(finished(xml))
}

/// `maven-metadata-local.xml` once `version` is in it, from the file as it
/// stands: every version installed so far, oldest first. It carries no
/// `<lastUpdated>`, which would make two installs of one project differ.
///
/// # Errors
///
/// [`JrsError::Build`] when the XML writer refuses it.
pub fn local_metadata(
    existing: Option<&[u8]>,
    group: &str,
    artifact: &str,
    version: &str,
) -> Result<Vec<u8>> {
    let mut versions = existing
        .and_then(|bytes| Metadata::parse(bytes).ok())
        .map(|m| m.versions)
        .unwrap_or_default();
    if !versions.iter().any(|v| v == version) {
        versions.push(version.to_string());
    }
    versions.sort_by(|a, b| compare_versions(a, b));
    write_metadata(group, artifact, &versions)
        .map_err(|e| JrsError::build(format!("could not write the metadata of `{artifact}`: {e}")))
}

/// What goes into the repository for one project.
#[derive(Debug)]
pub struct Installation<'a> {
    pub group: &'a str,
    pub artifact: &'a str,
    pub version: &'a str,
    /// The jar a consumer gets, without a `Class-Path`.
    pub jar: &'a Path,
    /// Each jar published beside it, by classifier.
    pub classified: Vec<(&'static str, PathBuf)>,
    pub dependencies: Vec<PomDependency>,
}

/// What [`Installation::write`] did.
#[derive(Debug)]
pub struct Installed {
    pub group: String,
    pub artifact: String,
    pub version: String,
    pub dir: PathBuf,
    /// Whether a release's jar was there already, with other bytes.
    pub replaced_release: bool,
}

/// `bytes` at `path`, with its `.sha1` beside it.
fn put(path: &Path, bytes: &[u8]) -> Result<()> {
    write_atomic(path, bytes)?;
    let mut sha1 = path.as_os_str().to_os_string();
    sha1.push(".sha1");
    write_atomic(Path::new(&sha1), sha1_hex(bytes).as_bytes())
}

impl Installation<'_> {
    /// Write everything into `repo`. The jars go before the POM, and the POM
    /// before the metadata, since a resolver reads them in the other order:
    /// whatever it finds, what that names is already there.
    ///
    /// # Errors
    ///
    /// [`JrsError::Io`] when a jar cannot be read or the repository written.
    pub fn write(&self, repo: &Path) -> Result<Installed> {
        let artifact_dir = self
            .group
            .split('.')
            .fold(repo.to_path_buf(), |dir, part| dir.join(part))
            .join(self.artifact);
        let dir = artifact_dir.join(self.version);
        let base = format!("{}-{}", self.artifact, self.version);

        let jar = std::fs::read(self.jar).path(self.jar)?;
        let destination = dir.join(format!("{base}.jar"));
        let replaced_release = !self.version.ends_with(SNAPSHOT_SUFFIX)
            && std::fs::read(&destination).is_ok_and(|old| old != jar);
        put(&destination, &jar)?;
        for (classifier, path) in &self.classified {
            let bytes = std::fs::read(path).path(path)?;
            put(&dir.join(format!("{base}-{classifier}.jar")), &bytes)?;
        }
        let pom = render_pom(self.group, self.artifact, self.version, &self.dependencies)?;
        put(&dir.join(format!("{base}.pom")), &pom)?;

        let metadata = artifact_dir.join(LOCAL_METADATA);
        let existing = match std::fs::read(&metadata) {
            Ok(bytes) => Some(bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(JrsError::io(&metadata, e)),
        };
        write_atomic(
            &metadata,
            &local_metadata(existing.as_deref(), self.group, self.artifact, self.version)?,
        )?;

        Ok(Installed {
            group: self.group.to_string(),
            artifact: self.artifact.to_string(),
            version: self.version.to_string(),
            dir,
            replaced_release,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{Dependency, ModuleDependency, blank};
    use crate::resolve::coord::Coord;
    use crate::resolve::{ResolvedPackage, pom::Pom};

    fn resolved(group: &str, artifact: &str, version: &str, packaging: &str) -> ResolvedPackage {
        ResolvedPackage {
            coord: Coord::new(group, artifact, version),
            classpath: Classpath::Compile,
            packaging: packaging.to_string(),
            depth: 1,
            direct: true,
            dependencies: Vec::new(),
            jar: None,
            checksum: None,
            mediated: false,
            managed: false,
        }
    }

    fn scopes(manifest: &Manifest, resolution: &Resolution) -> Vec<(String, &'static str)> {
        pom_dependencies(manifest, resolution, &[])
            .unwrap()
            .into_iter()
            .map(|d| (d.artifact, d.scope.as_str()))
            .collect()
    }

    #[test]
    fn each_kind_of_dependency_takes_its_scope() {
        let mut m = blank("lib", "1.0.0", Path::new("/p"));
        m.dependencies.push(Dependency::new("g", "plain", "1"));
        let mut provided = Dependency::new("g", "api", "1");
        provided.compile_only = true;
        m.dependencies.push(provided);
        let mut runtime = Dependency::new("g", "driver", "1");
        runtime.runtime_only = true;
        m.dependencies.push(runtime);
        m.dev_dependencies.push(Dependency::new("g", "junit", "1"));
        assert_eq!(
            scopes(&m, &Resolution::default()),
            [
                ("plain".to_string(), "compile"),
                ("api".to_string(), "provided"),
                ("driver".to_string(), "runtime"),
            ],
            "and [dev-dependencies] stay out"
        );
    }

    #[test]
    fn a_managed_version_is_written_out() {
        let mut m = blank("lib", "1.0.0", Path::new("/p"));
        m.dependencies.push(Dependency::new("g", "managed", ""));
        let resolution = Resolution {
            packages: vec![resolved("g", "managed", "2.3", "jar")],
            ..Resolution::default()
        };
        let deps = pom_dependencies(&m, &resolution, &[]).unwrap();
        assert_eq!(deps[0].version, "2.3");
        assert!(
            pom_dependencies(&m, &Resolution::default(), &[]).is_err(),
            "a POM never goes out without a version"
        );
    }

    #[test]
    fn a_pom_only_dependency_keeps_its_type() {
        let mut m = blank("lib", "1.0.0", Path::new("/p"));
        m.dependencies.push(Dependency::new("g", "aggregate", "1"));
        let resolution = Resolution {
            packages: vec![resolved("g", "aggregate", "1", "pom")],
            ..Resolution::default()
        };
        let deps = pom_dependencies(&m, &resolution, &[]).unwrap();
        assert_eq!(deps[0].kind.as_deref(), Some("pom"));
    }

    #[test]
    fn a_local_jar_or_no_group_refuses_the_install() {
        let mut m = blank("lib", "1.0.0", Path::new("/p"));
        assert_eq!(check(&m).unwrap_err().exit_code(), 2);
        m.group = Some("com.acme".to_string());
        check(&m).unwrap();
        m.dev_dependencies
            .push(Dependency::local("t", "libs/t.jar"));
        check(&m).expect("a test-only jar is nobody else's business");
        m.dependencies.push(Dependency::local("x", "libs/x.jar"));
        let error = check(&m).unwrap_err();
        assert_eq!(error.exit_code(), 2);
        assert!(error.to_string().contains("libs/x.jar"), "{error}");
    }

    #[test]
    fn members_come_first_and_a_test_only_member_stays_out() {
        let member = |name: &str, classpath, direct| Upstream {
            name: name.to_string(),
            version: "1.0.0".to_string(),
            group: Some("com.acme".to_string()),
            root: PathBuf::new(),
            classes_dir: PathBuf::new(),
            jar_path: PathBuf::new(),
            classpath,
            direct,
        };
        let mut m = blank("app", "1.0.0", Path::new("/p"));
        m.dependencies.push(Dependency::new("g", "a", "1"));
        m.modules.push(ModuleDependency {
            name: "core".to_string(),
            path: "../core".to_string(),
            dev: false,
            compile_only: false,
            runtime_only: false,
        });
        let upstream = [
            member("core", Classpath::Compile, true),
            member("base", Classpath::Compile, false),
            member("fixtures", Classpath::Test, true),
        ];
        let deps = pom_dependencies(&m, &Resolution::default(), &upstream).unwrap();
        let names: Vec<&str> = deps.iter().map(|d| d.artifact.as_str()).collect();
        assert_eq!(names, ["core", "a"]);
        assert_eq!(deps[0].group, "com.acme");
    }

    #[test]
    fn the_pom_reads_back_as_the_resolver_reads_it() {
        let bytes = render_pom(
            "com.acme",
            "lib",
            "1.0.0",
            &[PomDependency {
                group: "g".to_string(),
                artifact: "a&b".to_string(),
                version: "1".to_string(),
                classifier: Some("linux".to_string()),
                kind: None,
                scope: Classpath::Runtime,
                exclusions: vec![Exclusion {
                    group: "*".to_string(),
                    artifact: "*".to_string(),
                }],
            }],
        )
        .unwrap();
        let pom = Pom::parse(&bytes).unwrap();
        assert_eq!(pom.artifact, "lib");
        let dep = &pom.dependencies[0];
        assert_eq!(dep.artifact, "a&b");
        assert_eq!(dep.scope.as_deref(), Some("runtime"));
        assert_eq!(dep.classifier.as_deref(), Some("linux"));
        assert_eq!(dep.exclusions.len(), 1);
    }

    #[test]
    fn the_default_repository_follows_the_settings_file() {
        let home = std::env::temp_dir().join(format!("jrs-install-{}", std::process::id()));
        std::fs::create_dir_all(home.join(".m2")).unwrap();
        let settings = |text: &str| std::fs::write(home.join(".m2/settings.xml"), text).unwrap();
        let namespace = "xmlns=\"http://maven.apache.org/SETTINGS/1.0.0\"";

        assert_eq!(
            default_repository(&home).unwrap(),
            home.join(".m2").join("repository"),
            "no settings file"
        );
        settings(&format!(
            "<settings {namespace}><offline>true</offline></settings>"
        ));
        assert_eq!(
            default_repository(&home).unwrap(),
            home.join(".m2").join("repository"),
            "a settings file that does not move it"
        );
        settings(&format!(
            "<settings {namespace}><localRepository> /opt/m2 </localRepository></settings>"
        ));
        assert_eq!(
            default_repository(&home).unwrap(),
            std::path::absolute("/opt/m2").unwrap(),
            "trimmed, as Maven reads it"
        );
        settings("<settings><localRepository>${user.home}/m2</localRepository></settings>");
        assert_eq!(
            default_repository(&home).unwrap(),
            PathBuf::from(format!("{}/m2", home.display()))
        );

        for unusable in [
            "<settings><localRepository>${env.REPO}/m2</localRepository></settings>",
            "<settings>",
        ] {
            settings(unusable);
            let error = default_repository(&home).unwrap_err();
            assert_eq!(error.exit_code(), 2, "{unusable}");
            assert!(error.to_string().contains("--repo"), "{error}");
        }
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn the_local_metadata_keeps_what_was_installed_in_order() {
        let first = local_metadata(None, "g", "a", "1.10.0").unwrap();
        let second = local_metadata(Some(&first), "g", "a", "1.9.0").unwrap();
        let third = local_metadata(Some(&second), "g", "a", "2.0.0-SNAPSHOT").unwrap();
        assert_eq!(
            local_metadata(Some(&third), "g", "a", "1.9.0").unwrap(),
            third,
            "installing a version twice changes nothing"
        );
        let metadata = Metadata::parse(&third).unwrap();
        assert_eq!(metadata.versions, ["1.9.0", "1.10.0", "2.0.0-SNAPSHOT"]);
        assert_eq!(metadata.release.as_deref(), Some("1.10.0"));
        let unreadable = local_metadata(Some(b"not xml"), "g", "a", "1").unwrap();
        assert_eq!(Metadata::parse(&unreadable).unwrap().versions, ["1"]);
    }
}
