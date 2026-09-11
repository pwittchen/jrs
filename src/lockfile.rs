//! `jrs.lock`: the resolved dependency graph, written down.
//!
//! The lockfile makes builds reproducible and offline-capable (SPEC §4.4). It
//! records coordinates and checksums, but never absolute paths — those are a
//! property of the machine, not of the resolution, and are recomputed from the
//! cache on load.
//!
//! It is regenerated when the manifest's dependency-affecting fields change,
//! which is what `manifest-checksum` is for, or when `jrs update` is run.

use std::fmt::Write as _;
use std::path::Path;

use crate::error::{IoResultExt, JrsError, Result};
use crate::manifest::{Dependency, Manifest};
use crate::resolve::coord::{Coord, Ga};
use crate::resolve::repo::sha256_hex;
use crate::resolve::{Classpath, LocalJar, Resolution, ResolvedPackage};

pub const LOCK_VERSION: u64 = 1;

/// The format with `[[tool]]` blocks (`JVM_LANGUAGES.md` §5.2). A lockfile
/// with no tools stays at [`LOCK_VERSION`], byte for byte; a jrs that predates
/// tools meets this number and refuses the file, rather than dropping the
/// compiler pins the next time it rewrites it.
pub const TOOLS_LOCK_VERSION: u64 = 2;

#[derive(Debug, Clone)]
pub struct Lockfile {
    pub version: u64,
    /// Digest of everything in the manifest that can change resolution.
    pub manifest_checksum: String,
    pub roots: Vec<Ga>,
    pub test_roots: Vec<Ga>,
    pub packages: Vec<ResolvedPackage>,
    /// Graphs jrs resolves for itself, each apart from the project's: the
    /// compilers of the project's other languages.
    pub tools: Vec<LockedTool>,
    /// The manifest's local jars, as `[[local]]` blocks: the relative path and
    /// the pinned checksum, never the absolute path. A lockfile without them
    /// renders exactly as it did before they existed. They need no new
    /// `version`: a jrs that predates them refuses the manifest's `path` key
    /// outright, so it can never rewrite this file and drop the pins.
    pub local: Vec<LocalJar>,
}

/// One tool's pinned graph, in the package format the project's uses.
#[derive(Debug, Clone)]
pub struct LockedTool {
    /// `kotlin-compiler`.
    pub name: String,
    pub roots: Vec<Ga>,
    pub packages: Vec<ResolvedPackage>,
}

impl Lockfile {
    #[must_use]
    pub fn from_resolution(manifest: &Manifest, resolution: &Resolution) -> Lockfile {
        Lockfile {
            version: LOCK_VERSION,
            manifest_checksum: manifest_checksum(manifest),
            roots: resolution.roots.clone(),
            test_roots: resolution.test_roots.clone(),
            packages: sorted(&resolution.packages),
            tools: Vec::new(),
            local: resolution
                .local
                .iter()
                .map(|l| LocalJar {
                    jar: None,
                    ..l.clone()
                })
                .collect(),
        }
    }

    /// Pin a tool's graph beside the project's, which makes this a version 2
    /// lockfile.
    #[must_use]
    pub fn with_tool(mut self, name: &str, resolution: &Resolution) -> Lockfile {
        self.tools.push(LockedTool {
            name: name.to_string(),
            roots: resolution.roots.clone(),
            packages: sorted(&resolution.packages),
        });
        self.version = TOOLS_LOCK_VERSION;
        self
    }

    /// The tool `name`'s graph, as a resolution with its jar paths left empty.
    #[must_use]
    pub fn tool(&self, name: &str) -> Option<Resolution> {
        self.tools
            .iter()
            .find(|t| t.name == name)
            .map(|t| Resolution {
                packages: t.packages.clone(),
                roots: t.roots.clone(),
                test_roots: Vec::new(),
                warnings: Vec::new(),
                downloaded: 0,
                local: Vec::new(),
            })
    }

    /// Every package the lockfile pins, the tools' included: what `jrs verify`
    /// re-hashes and `jrs cache prune` keeps.
    pub fn all_packages(&self) -> impl Iterator<Item = &ResolvedPackage> {
        self.packages
            .iter()
            .chain(self.tools.iter().flat_map(|t| &t.packages))
    }

    /// Turn the lockfile back into a resolution. Jar paths are left empty; the
    /// caller fills them from the cache or by downloading, and the local jars'
    /// from the project with [`crate::resolve::attach_local`].
    #[must_use]
    pub fn to_resolution(&self) -> Resolution {
        Resolution {
            packages: self.packages.clone(),
            roots: self.roots.clone(),
            test_roots: self.test_roots.clone(),
            warnings: Vec::new(),
            downloaded: 0,
            local: self.local.clone(),
        }
    }

    /// Whether this lockfile still describes `manifest`: its dependencies,
    /// and a pinned compiler for every language it turns on.
    #[must_use]
    pub fn matches(&self, manifest: &Manifest) -> bool {
        supported(self.version)
            && self.manifest_checksum == manifest_checksum(manifest)
            && manifest
                .languages
                .iter()
                .all(|c| self.tools.iter().any(|t| t.name == c.language.tool_name()))
    }

    /// Read and parse the lockfile at `path`; `Ok(None)` when there is none.
    ///
    /// # Errors
    ///
    /// As for [`Lockfile::parse`], plus [`JrsError::Io`] when the file exists
    /// but cannot be read.
    pub fn load(path: &Path) -> Result<Option<Lockfile>> {
        match std::fs::read_to_string(path) {
            Ok(text) => Lockfile::parse(&text, path).map(Some),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(JrsError::io(path, e)),
        }
    }

    /// Write the rendered lockfile to `path`.
    ///
    /// # Errors
    ///
    /// [`JrsError::Io`] when the file cannot be written.
    pub fn write(&self, path: &Path) -> Result<()> {
        std::fs::write(path, self.render()).path(path)
    }

    /// Parse lockfile `text`; `path` is only for error messages.
    ///
    /// # Errors
    ///
    /// [`JrsError::Resolve`] when `text` is not TOML, its `version` is neither
    /// [`LOCK_VERSION`] nor [`TOOLS_LOCK_VERSION`], a `[[tool]]` has no
    /// `name`, or a `[[package]]` is not a table or lacks its `group`,
    /// `artifact` or `version`.
    pub fn parse(text: &str, path: &Path) -> Result<Lockfile> {
        let table: toml::Table = toml::from_str(text)
            .map_err(|e| JrsError::resolve(format!("{}: {}", path.display(), e.message())))?;
        // A negative version is as unsupported as a missing one.
        let version = table
            .get("version")
            .and_then(toml::Value::as_integer)
            .and_then(|n| u64::try_from(n).ok())
            .unwrap_or(0);
        if !supported(version) {
            return Err(JrsError::resolve(format!(
                "{}: lockfile version {version} is not supported by jrs {} \
                 (expected {LOCK_VERSION} or {TOOLS_LOCK_VERSION})\n\nrun `jrs update` to \
                 regenerate it",
                path.display(),
                env!("CARGO_PKG_VERSION"),
            )));
        }

        let mut tools = Vec::new();
        for tool in table
            .get("tool")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
        {
            let t = tool.as_table().ok_or_else(|| {
                JrsError::resolve(format!("{}: [[tool]] must be a table", path.display()))
            })?;
            let name = t.get("name").and_then(|v| v.as_str()).ok_or_else(|| {
                JrsError::resolve(format!(
                    "{}: [[tool]] is missing `name`\n\nrun `jrs update` to regenerate it",
                    path.display()
                ))
            })?;
            tools.push(LockedTool {
                name: name.to_string(),
                roots: read_gas(t, "roots"),
                packages: read_packages(t, path)?,
            });
        }

        Ok(Lockfile {
            version,
            manifest_checksum: table
                .get("manifest-checksum")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            roots: read_gas(&table, "roots"),
            test_roots: read_gas(&table, "test-roots"),
            packages: read_packages(&table, path)?,
            tools,
            local: read_local(&table, path)?,
        })
    }

    #[must_use]
    pub fn render(&self) -> String {
        let mut s = String::new();
        s.push_str("# Generated by jrs. Commit this file.\n");
        s.push_str("# It records the exact dependency graph, so the same manifest\n");
        s.push_str("# resolves to the same jars on every machine.\n\n");
        let _ = writeln!(s, "version = {}", self.version);
        let _ = writeln!(s, "manifest-checksum = \"{}\"", self.manifest_checksum);
        let _ = writeln!(s, "roots = {}", render_gas(&self.roots));
        let _ = writeln!(s, "test-roots = {}", render_gas(&self.test_roots));

        for p in &self.packages {
            render_package(&mut s, "package", p);
        }
        for l in &self.local {
            render_local(&mut s, l);
        }
        for tool in &self.tools {
            s.push_str("\n[[tool]]\n");
            let _ = writeln!(s, "name = \"{}\"", tool.name);
            let _ = writeln!(s, "roots = {}", render_gas(&tool.roots));
            for p in &tool.packages {
                render_package(&mut s, "tool.package", p);
            }
        }
        s
    }
}

fn supported(version: u64) -> bool {
    version == LOCK_VERSION || version == TOOLS_LOCK_VERSION
}

fn sorted(packages: &[ResolvedPackage]) -> Vec<ResolvedPackage> {
    let mut packages = packages.to_vec();
    packages.sort_by(|a, b| a.coord.cmp(&b.coord));
    packages
}

fn render_package(s: &mut String, header: &str, p: &ResolvedPackage) {
    let _ = writeln!(s, "\n[[{header}]]");
    let _ = writeln!(s, "group = \"{}\"", p.coord.group);
    let _ = writeln!(s, "artifact = \"{}\"", p.coord.artifact);
    let _ = writeln!(s, "version = \"{}\"", p.coord.version);
    if let Some(c) = &p.coord.classifier {
        let _ = writeln!(s, "classifier = \"{c}\"");
    }
    let _ = writeln!(s, "classpath = \"{}\"", p.classpath.as_str());
    let _ = writeln!(s, "packaging = \"{}\"", p.packaging);
    let _ = writeln!(s, "depth = {}", p.depth);
    let _ = writeln!(s, "direct = {}", p.direct);
    if let Some(c) = &p.checksum {
        let _ = writeln!(s, "checksum = \"{c}\"");
    }
    if !p.dependencies.is_empty() {
        let _ = writeln!(s, "dependencies = {}", render_gas(&p.dependencies));
    }
}

/// One `[[local]]` block. Its `path` is the manifest's, relative to the
/// project root: an absolute path would tie the lockfile to one machine.
fn render_local(s: &mut String, l: &LocalJar) {
    s.push_str("\n[[local]]\n");
    let _ = writeln!(s, "name = \"{}\"", l.name);
    let _ = writeln!(s, "path = {}", toml::Value::String(l.path.clone()));
    let _ = writeln!(s, "classpath = \"{}\"", l.classpath.as_str());
    if let Some(c) = &l.checksum {
        let _ = writeln!(s, "checksum = \"{c}\"");
    }
}

fn read_local(table: &toml::Table, path: &Path) -> Result<Vec<LocalJar>> {
    let mut out = Vec::new();
    for value in table
        .get("local")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
    {
        let t = value.as_table().ok_or_else(|| {
            JrsError::resolve(format!("{}: [[local]] must be a table", path.display()))
        })?;
        let field = |name: &str| -> Result<String> {
            t.get(name)
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .ok_or_else(|| {
                    JrsError::resolve(format!(
                        "{}: [[local]] is missing `{name}`\n\nrun `jrs update` to regenerate it",
                        path.display()
                    ))
                })
        };
        out.push(LocalJar {
            name: field("name")?,
            path: field("path")?,
            classpath: Classpath::parse(
                t.get("classpath")
                    .and_then(|v| v.as_str())
                    .unwrap_or("compile"),
            ),
            checksum: t
                .get("checksum")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            jar: None,
        });
    }
    Ok(out)
}

fn read_packages(table: &toml::Table, path: &Path) -> Result<Vec<ResolvedPackage>> {
    table
        .get("package")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .map(|p| read_package(p, path))
                .collect::<Result<Vec<_>>>()
        })
        .transpose()
        .map(Option::unwrap_or_default)
}

fn read_gas(table: &toml::Table, key: &str) -> Vec<Ga> {
    table
        .get(key)
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str())
                .filter_map(Ga::parse)
                .collect()
        })
        .unwrap_or_default()
}

fn render_gas(gas: &[Ga]) -> String {
    let items: Vec<String> = gas.iter().map(|g| format!("\"{g}\"")).collect();
    format!("[{}]", items.join(", "))
}

fn read_package(value: &toml::Value, path: &Path) -> Result<ResolvedPackage> {
    let t = value.as_table().ok_or_else(|| {
        JrsError::resolve(format!("{}: [[package]] must be a table", path.display()))
    })?;
    let field = |name: &str| -> Result<String> {
        t.get(name)
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .ok_or_else(|| {
                JrsError::resolve(format!(
                    "{}: [[package]] is missing `{name}`\n\nrun `jrs update` to regenerate it",
                    path.display()
                ))
            })
    };
    Ok(ResolvedPackage {
        coord: Coord::new(field("group")?, field("artifact")?, field("version")?).with_classifier(
            t.get("classifier")
                .and_then(|v| v.as_str())
                .map(str::to_string),
        ),
        classpath: Classpath::parse(
            t.get("classpath")
                .and_then(|v| v.as_str())
                .unwrap_or("compile"),
        ),
        packaging: t
            .get("packaging")
            .and_then(|v| v.as_str())
            .unwrap_or("jar")
            .to_string(),
        // A negative or oversized depth falls back like a missing one.
        depth: t
            .get("depth")
            .and_then(toml::Value::as_integer)
            .and_then(|n| usize::try_from(n).ok())
            .unwrap_or(1),
        direct: t
            .get("direct")
            .and_then(toml::Value::as_bool)
            .unwrap_or(false),
        dependencies: read_gas(t, "dependencies"),
        jar: None,
        checksum: t
            .get("checksum")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        mediated: false,
    })
}

/// Digest the manifest fields that can change what resolution produces.
///
/// Deliberately narrow: changing `main-class` or `javac-args` must not invalidate
/// a perfectly good lockfile. The implied runtime libraries count as the
/// dependencies they are, and each language adds a `lang` line, since its
/// compiler's pinned graph depends on the version. A Java-only manifest adds
/// nothing, so its existing lockfile still matches.
#[must_use]
pub fn manifest_checksum(manifest: &Manifest) -> String {
    // The long form's extras are appended only when present, so a lockfile
    // written before they existed still matches the manifest it was made from.
    let line = |d: &Dependency| {
        let mut s = match &d.path {
            Some(path) => format!("{} path={path}", d.artifact),
            None => d.to_string(),
        };
        for e in &d.exclusions {
            let _ = write!(s, " exclude={e}");
        }
        if d.compile_only {
            s.push_str(" compile-only");
        }
        if d.runtime_only {
            s.push_str(" runtime-only");
        }
        s
    };
    let mut canonical = String::new();
    for d in &manifest.effective_dependencies() {
        let _ = writeln!(canonical, "dep {}", line(d));
    }
    for d in &manifest.dev_dependencies {
        let _ = writeln!(canonical, "dev {}", line(d));
    }
    for r in &manifest.repositories {
        // `groups` decide which repository an artifact may come from, and so
        // whether it resolves at all.
        let _ = write!(canonical, "repo {} {}", r.name, r.url);
        if !r.groups.is_empty() {
            let _ = write!(canonical, " groups={}", r.groups.join(","));
        }
        canonical.push('\n');
    }
    for c in &manifest.languages {
        let _ = writeln!(canonical, "lang {} {}", c.language.key(), c.version);
    }
    format!("sha256:{}", sha256_hex(canonical.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn manifest(body: &str) -> Manifest {
        let text = format!("[project]\nname='app'\nversion='1.0.0'\n{body}");
        Manifest::parse(&text, Path::new("/p/jrs.toml"), Path::new("/p")).unwrap()
    }

    fn package(gav: &str, direct: bool, classpath: Classpath) -> ResolvedPackage {
        ResolvedPackage {
            coord: Coord::parse(gav).unwrap(),
            classpath,
            packaging: "jar".into(),
            depth: if direct { 1 } else { 2 },
            direct,
            dependencies: vec![Ga::new("g", "child")],
            jar: Some(PathBuf::from("/cache/whatever.jar")),
            checksum: Some("sha1:abc123".into()),
            mediated: false,
        }
    }

    fn resolution() -> Resolution {
        Resolution {
            packages: vec![
                package("g:a:1.0", true, Classpath::Compile),
                package("g:child:2.0", false, Classpath::Compile),
                package("g:junit:5.0", true, Classpath::Test),
            ],
            roots: vec![Ga::new("g", "a")],
            test_roots: vec![Ga::new("g", "junit")],
            warnings: vec![],
            downloaded: 0,
            local: vec![],
        }
    }

    #[test]
    fn a_lockfile_round_trips() {
        let m = manifest("[dependencies]\n'g:a'='1.0'\n[dev-dependencies]\n'g:junit'='5.0'");
        let lock = Lockfile::from_resolution(&m, &resolution());
        let text = lock.render();
        let again = Lockfile::parse(&text, Path::new("jrs.lock")).unwrap();

        assert_eq!(again.version, LOCK_VERSION);
        assert_eq!(again.manifest_checksum, lock.manifest_checksum);
        assert_eq!(again.roots, lock.roots);
        assert_eq!(again.test_roots, lock.test_roots);
        assert_eq!(again.packages.len(), 3);

        let a = &again.packages[0];
        assert_eq!(a.coord.to_string(), "g:a:1.0");
        assert!(a.direct);
        assert_eq!(a.classpath, Classpath::Compile);
        assert_eq!(a.checksum.as_deref(), Some("sha1:abc123"));
        assert_eq!(a.dependencies, vec![Ga::new("g", "child")]);
    }

    #[test]
    fn classifiers_and_compile_only_survive_the_round_trip() {
        let m = manifest("[dependencies]\n'g:a'='1.0'");
        let mut r = resolution();
        r.packages[1].coord.classifier = Some("natives-linux".into());
        r.packages[1].classpath = Classpath::Provided;
        r.roots = vec![Ga::new("g", "a").with_classifier(Some("natives-linux".into()))];
        let again = Lockfile::parse(
            &Lockfile::from_resolution(&m, &r).render(),
            Path::new("jrs.lock"),
        )
        .unwrap();
        let child = again
            .packages
            .iter()
            .find(|p| p.coord.artifact == "child")
            .unwrap();
        assert_eq!(child.coord.classifier.as_deref(), Some("natives-linux"));
        assert_eq!(child.classpath, Classpath::Provided);
        assert_eq!(again.roots, r.roots);
    }

    #[test]
    fn no_absolute_paths_are_written_down() {
        let m = manifest("[dependencies]\n'g:a'='1.0'");
        let text = Lockfile::from_resolution(&m, &resolution()).render();
        assert!(
            !text.contains("/cache/"),
            "a lockfile must be machine-independent:\n{text}"
        );
    }

    #[test]
    fn packages_are_written_in_a_stable_order() {
        let m = manifest("[dependencies]\n'g:a'='1.0'");
        let mut shuffled = resolution();
        shuffled.packages.reverse();
        assert_eq!(
            Lockfile::from_resolution(&m, &shuffled).render(),
            Lockfile::from_resolution(&m, &resolution()).render()
        );
    }

    #[test]
    fn the_checksum_tracks_dependency_changes_only() {
        let base = manifest("[dependencies]\n'g:a'='1.0'");
        let same = manifest("main-class='x.Y'\n[dependencies]\n'g:a'='1.0'");
        let bumped = manifest("[dependencies]\n'g:a'='1.1'");
        let added = manifest("[dependencies]\n'g:a'='1.0'\n'g:b'='1.0'");
        let dev = manifest("[dependencies]\n'g:a'='1.0'\n[dev-dependencies]\n'g:t'='1.0'");
        let repo =
            manifest("[dependencies]\n'g:a'='1.0'\n[repositories]\nx='https://example.com/m2'");
        let excluded = manifest("[dependencies]\n'g:a'={version='1.0', exclusions=['x:y']}");
        let compile_only = manifest("[dependencies]\n'g:a'={version='1.0', compile-only=true}");
        let classified = manifest("[dependencies]\n'g:a:natives'='1.0'");
        // Tasks do not change resolution, so adding one does not re-resolve.
        let tasked = manifest(
            "[dependencies]\n'g:a'='1.0'\n[tasks.t]\nshell='x'\n[hooks]\npost-compile=['t']",
        );

        assert_eq!(manifest_checksum(&base), manifest_checksum(&same));
        assert_eq!(manifest_checksum(&base), manifest_checksum(&tasked));
        assert_ne!(manifest_checksum(&base), manifest_checksum(&excluded));
        assert_ne!(manifest_checksum(&base), manifest_checksum(&compile_only));
        assert_ne!(manifest_checksum(&base), manifest_checksum(&classified));
        assert_ne!(manifest_checksum(&base), manifest_checksum(&bumped));
        assert_ne!(manifest_checksum(&base), manifest_checksum(&added));
        assert_ne!(manifest_checksum(&base), manifest_checksum(&dev));
        assert_ne!(manifest_checksum(&base), manifest_checksum(&repo));
    }

    #[test]
    fn the_checksum_follows_the_compiler_version() {
        let java = manifest("[dependencies]\n'g:a'='1.0'");
        let kotlin = manifest("[dependencies]\n'g:a'='1.0'\n[kotlin]\nversion='2.4.20'");
        let newer = manifest("[dependencies]\n'g:a'='1.0'\n[kotlin]\nversion='2.4.21'");
        let flags = manifest(
            "[dependencies]\n'g:a'='1.0'\n[kotlin]\nversion='2.4.20'\nkotlinc-args=['-x']",
        );
        assert_ne!(manifest_checksum(&java), manifest_checksum(&kotlin));
        assert_ne!(manifest_checksum(&kotlin), manifest_checksum(&newer));
        assert_eq!(
            manifest_checksum(&kotlin),
            manifest_checksum(&flags),
            "compiler flags do not change what is resolved"
        );
        // A Java manifest's checksum is what it was before languages existed:
        // `dep`, `dev` and `repo` lines only.
        let canonical = format!(
            "dep g:a:1.0\nrepo central {}\n",
            crate::manifest::CENTRAL_URL
        );
        assert_eq!(
            manifest_checksum(&java),
            format!("sha256:{}", sha256_hex(canonical.as_bytes()))
        );
    }

    fn compiler_graph() -> Resolution {
        Resolution {
            packages: vec![
                package(
                    "org.jetbrains.kotlin:kotlin-compiler-embeddable:2.4.20",
                    true,
                    Classpath::Compile,
                ),
                package("g:child:2.0", false, Classpath::Compile),
            ],
            roots: vec![Ga::new(
                "org.jetbrains.kotlin",
                "kotlin-compiler-embeddable",
            )],
            ..Resolution::default()
        }
    }

    #[test]
    fn a_tool_graph_round_trips_in_a_version_2_lockfile() {
        let m = manifest("[dependencies]\n'g:a'='1.0'\n[kotlin]\nversion='2.4.20'");
        let lock = Lockfile::from_resolution(&m, &resolution())
            .with_tool("kotlin-compiler", &compiler_graph());
        assert_eq!(lock.version, TOOLS_LOCK_VERSION);
        let text = lock.render();
        assert!(text.contains("version = 2\n"), "{text}");
        assert!(
            text.contains("\n[[tool]]\nname = \"kotlin-compiler\"\n"),
            "{text}"
        );
        assert!(text.contains("\n[[tool.package]]\n"), "{text}");

        let again = Lockfile::parse(&text, Path::new("jrs.lock")).unwrap();
        assert_eq!(again.version, TOOLS_LOCK_VERSION);
        assert_eq!(again.packages.len(), 3, "the project's graph is untouched");
        let tool = again.tool("kotlin-compiler").unwrap();
        assert_eq!(tool.roots, compiler_graph().roots);
        assert_eq!(tool.packages.len(), 2);
        assert_eq!(tool.packages[0].checksum.as_deref(), Some("sha1:abc123"));
        assert!(again.tool("scala-compiler").is_none());
        assert_eq!(again.all_packages().count(), 5);
        assert!(again.matches(&m));
        assert_eq!(again.render(), text, "rendering is stable");
    }

    #[test]
    fn a_lockfile_without_tools_stays_version_1() {
        let m = manifest("[dependencies]\n'g:a'='1.0'");
        let text = Lockfile::from_resolution(&m, &resolution()).render();
        assert!(text.contains("version = 1\n"), "{text}");
        assert!(!text.contains("[[tool"), "{text}");
    }

    #[test]
    fn a_lockfile_missing_a_languages_compiler_does_not_match() {
        let m = manifest("[dependencies]\n'g:a'='1.0'\n[kotlin]\nversion='2.4.20'");
        let without = Lockfile::from_resolution(&m, &resolution());
        assert!(!without.matches(&m), "the Kotlin compiler is not pinned");
        assert!(
            without
                .with_tool("kotlin-compiler", &compiler_graph())
                .matches(&m)
        );
    }

    #[test]
    fn a_newer_lockfile_version_is_refused() {
        let err = Lockfile::parse("version = 3\n", Path::new("jrs.lock"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("version 3 is not supported"), "{err}");
        assert!(err.contains("jrs update"), "{err}");
        let err = Lockfile::parse("version = 2\n[[tool]]\nroots = []\n", Path::new("jrs.lock"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("[[tool]] is missing `name`"), "{err}");
    }

    #[test]
    fn a_stale_lockfile_is_detected() {
        let before = manifest("[dependencies]\n'g:a'='1.0'");
        let lock = Lockfile::from_resolution(&before, &resolution());
        assert!(lock.matches(&before));
        assert!(!lock.matches(&manifest("[dependencies]\n'g:a'='2.0'")));
    }

    #[test]
    fn an_unknown_lockfile_version_says_how_to_recover() {
        let err = Lockfile::parse("version = 99\n", Path::new("jrs.lock"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("jrs update"), "{err}");
    }

    #[test]
    fn a_missing_lockfile_is_not_an_error() {
        let missing = std::env::temp_dir().join("jrs-no-such-lockfile.lock");
        let _ = std::fs::remove_file(&missing);
        assert!(Lockfile::load(&missing).unwrap().is_none());
    }

    #[test]
    fn local_jars_are_pinned_by_relative_path_and_checksum() {
        let m = manifest(
            "[dependencies]\n'g:a'='1.0'\ndriver = { path = 'libs/driver.jar', runtime-only = true }",
        );
        let mut r = resolution();
        r.local = vec![LocalJar {
            name: "driver".into(),
            path: "libs/driver.jar".into(),
            classpath: Classpath::Runtime,
            checksum: Some("sha256:abc".into()),
            jar: Some(PathBuf::from("/home/me/project/libs/driver.jar")),
        }];
        let text = Lockfile::from_resolution(&m, &r).render();
        assert!(text.contains("version = 1\n"), "{text}");
        assert!(
            text.contains(
                "\n[[local]]\nname = \"driver\"\npath = \"libs/driver.jar\"\n\
                 classpath = \"runtime\"\nchecksum = \"sha256:abc\"\n"
            ),
            "{text}"
        );
        assert!(!text.contains("/home/me"), "{text}");

        let again = Lockfile::parse(&text, Path::new("jrs.lock")).unwrap();
        assert!(again.matches(&m));
        assert_eq!(again.local.len(), 1);
        assert_eq!(again.local[0].classpath, Classpath::Runtime);
        assert_eq!(again.local[0].jar, None);
        assert_eq!(again.to_resolution().local, again.local);
        assert_eq!(again.render(), text, "rendering is stable");

        // A lockfile without them renders exactly as before they existed.
        assert!(
            !Lockfile::from_resolution(&m, &resolution())
                .render()
                .contains("[[local]]")
        );
        let err = Lockfile::parse(
            "version = 1\n[[local]]\nname = \"d\"\n",
            Path::new("jrs.lock"),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("[[local]] is missing `path`"), "{err}");
    }

    #[test]
    fn the_checksum_follows_runtime_only_local_jars_and_groups() {
        let base = manifest("[dependencies]\n'g:a'='1.0'");
        let runtime = manifest("[dependencies]\n'g:a'={version='1.0', runtime-only=true}");
        let local = manifest("[dependencies]\n'g:a'='1.0'\nd={path='libs/d.jar'}");
        let moved = manifest("[dependencies]\n'g:a'='1.0'\nd={path='lib/d.jar'}");
        let plain =
            manifest("[dependencies]\n'g:a'='1.0'\n[repositories]\nx='https://example.com/m2'");
        let grouped = manifest(
            "[dependencies]\n'g:a'='1.0'\n[repositories]\nx={url='https://example.com/m2', groups=['g']}",
        );
        assert_ne!(manifest_checksum(&base), manifest_checksum(&runtime));
        assert_ne!(manifest_checksum(&base), manifest_checksum(&local));
        assert_ne!(manifest_checksum(&local), manifest_checksum(&moved));
        assert_ne!(manifest_checksum(&plain), manifest_checksum(&grouped));
    }

    #[test]
    fn a_truncated_package_entry_says_how_to_recover() {
        let err = Lockfile::parse(
            "version = 1\n[[package]]\ngroup = \"g\"\n",
            Path::new("jrs.lock"),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("artifact"), "{err}");
        assert!(err.contains("jrs update"), "{err}");
    }
}
