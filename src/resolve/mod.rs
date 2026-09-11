//! Dependency resolution: the graph walk, conflict mediation, and the classpath.
//!
//! The algorithm is SPEC §8.2, breadth-first by level so that each level's POM
//! fetches batch together, with nearest-wins mediation on top. Version ranges are
//! rejected rather than guessed at.

pub mod cache;
pub mod coord;
pub mod metadata;
pub mod pom;
pub mod repo;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use rayon::prelude::*;

use coord::{Coord, Ga, compare_versions, is_range};
use pom::{Effective, Pom, PomDependency};
use repo::{Fetcher, TransferReporter};

use crate::error::{JrsError, Result};
use crate::manifest::{Dependency, Manifest};
use crate::ui::{Live, Transfer, Ui};

/// Which classpath a resolved package belongs to.
///
/// Each value is a set of places a jar goes: the main sources' compile
/// classpath, the runtime one, and the test one (which the tests compile and
/// run against alike). A package reached several ways lands on the union of
/// what reaches it, [`Classpath::join`]: a compile dependency that tests also
/// see is a compile dependency; one that a `compile-only` library also drags
/// in still has to be there at runtime; and one that is `compile-only` on one
/// path and `runtime-only` on another is needed on both, which is `Compile`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Classpath {
    /// Compile, test and runtime.
    Compile,
    /// Compile and test, but not runtime: `compile-only` and everything it
    /// brings in (Maven's `provided`).
    Provided,
    /// Runtime and test, but not the main sources' compile classpath:
    /// `runtime-only` and everything it brings in (Maven's `runtime`).
    Runtime,
    /// Tests only.
    Test,
}

impl Classpath {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Classpath::Compile => "compile",
            Classpath::Provided => "provided",
            Classpath::Runtime => "runtime",
            Classpath::Test => "test",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Classpath {
        match s {
            "test" => Classpath::Test,
            "provided" => Classpath::Provided,
            "runtime" => Classpath::Runtime,
            _ => Classpath::Compile,
        }
    }

    /// The classpath of a package reached both as `self` and as `other`:
    /// every place either puts it (SPEC §8.2 step 6).
    #[must_use]
    pub fn join(self, other: Classpath) -> Classpath {
        match (self, other) {
            (Classpath::Compile, _)
            | (_, Classpath::Compile)
            | (Classpath::Provided, Classpath::Runtime)
            | (Classpath::Runtime, Classpath::Provided) => Classpath::Compile,
            (Classpath::Test, other) | (other, Classpath::Test) => other,
            (Classpath::Provided, Classpath::Provided) => Classpath::Provided,
            (Classpath::Runtime, Classpath::Runtime) => Classpath::Runtime,
        }
    }

    /// On the classpath the main sources compile against — `javac`'s, and
    /// kotlinc's, scalac's and groovyc's.
    #[must_use]
    pub fn compiles(self) -> bool {
        matches!(self, Classpath::Compile | Classpath::Provided)
    }

    /// On the classpath the program runs with, and so in what is packaged.
    #[must_use]
    pub fn runs(self) -> bool {
        matches!(self, Classpath::Compile | Classpath::Runtime)
    }
}

/// A jar the manifest names by `path` rather than by coordinate
/// (`name = { path = "libs/driver.jar" }`). It is taken as it is: no POM, no
/// transitive graph, nothing to mediate. `jrs.lock` records its relative path
/// and pins its checksum.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalJar {
    /// The manifest key.
    pub name: String,
    /// Relative to the project root, `/`-separated, as the manifest writes it.
    pub path: String,
    pub classpath: Classpath,
    /// `sha256:<hex>`, once the file has been read or the lockfile pinned it.
    pub checksum: Option<String>,
    /// Filled in by [`attach_local`].
    pub jar: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct ResolvedPackage {
    pub coord: Coord,
    pub classpath: Classpath,
    /// `jar` for ordinary artifacts; `pom` for aggregates and BOMs, which
    /// contribute dependencies but no file to a classpath.
    pub packaging: String,
    /// Distance from the manifest; 1 for a declared dependency.
    pub depth: usize,
    pub direct: bool,
    /// Resolved outgoing edges, for `jrs tree`.
    pub dependencies: Vec<Ga>,
    /// Filled in by [`fetch_jars`]; `None` for `pom`-packaged artifacts, which
    /// have no jar to put on a classpath.
    pub jar: Option<PathBuf>,
    pub checksum: Option<String>,
    /// True when mediation picked this version over another one that was asked
    /// for somewhere else in the graph.
    pub mediated: bool,
    /// True when the version is the one `[managed]` names, directly or through
    /// a BOM (SPEC §8.9).
    pub managed: bool,
}

impl ResolvedPackage {
    #[must_use]
    pub fn ga(&self) -> Ga {
        self.coord.ga()
    }
}

#[derive(Debug, Clone, Default)]
pub struct Resolution {
    pub packages: Vec<ResolvedPackage>,
    /// Direct dependencies in declaration order — the head of the classpath.
    pub roots: Vec<Ga>,
    pub test_roots: Vec<Ga>,
    pub warnings: Vec<String>,
    pub downloaded: u64,
    /// The manifest's local jars, in declaration order: `[dependencies]`
    /// first, then `[dev-dependencies]`.
    pub local: Vec<LocalJar>,
}

impl Resolution {
    #[must_use]
    pub fn get(&self, ga: &Ga) -> Option<&ResolvedPackage> {
        self.packages.iter().find(|p| p.ga() == *ga)
    }

    /// Jars for `javac -cp`, in a stable order: direct dependencies first, then
    /// transitives, each sorted by coordinate (SPEC §8.2 step 7). Local jars
    /// are direct dependencies with no coordinate; they come after the
    /// coordinates, sorted by name.
    ///
    /// `Compile` (or `Provided`) is what the main sources compile against, so
    /// it includes `compile-only` jars and leaves `runtime-only` ones out;
    /// `Test` is everything; `Runtime` is [`Resolution::runtime_classpath`].
    #[must_use]
    pub fn classpath(&self, which: Classpath) -> Vec<PathBuf> {
        match which {
            Classpath::Test => self.ordered(|_| true),
            Classpath::Runtime => self.runtime_classpath(),
            Classpath::Compile | Classpath::Provided => self.ordered(Classpath::compiles),
        }
    }

    /// Jars for `java -cp` and for packaging: everything that runs, which is
    /// the compile classpath without `compile-only` and with `runtime-only`.
    #[must_use]
    pub fn runtime_classpath(&self) -> Vec<PathBuf> {
        self.ordered(Classpath::runs)
    }

    fn ordered(&self, include: impl Fn(Classpath) -> bool) -> Vec<PathBuf> {
        let mut direct: Vec<&ResolvedPackage> = Vec::new();
        let mut transitive: Vec<&ResolvedPackage> = Vec::new();
        for p in &self.packages {
            if !include(p.classpath) || p.jar.is_none() {
                continue;
            }
            if p.direct {
                direct.push(p);
            } else {
                transitive.push(p);
            }
        }
        direct.sort_by(|a, b| a.coord.cmp(&b.coord));
        transitive.sort_by(|a, b| a.coord.cmp(&b.coord));
        let mut local: Vec<&LocalJar> = self
            .local
            .iter()
            .filter(|l| include(l.classpath) && l.jar.is_some())
            .collect();
        local.sort_by(|a, b| a.name.cmp(&b.name));
        direct
            .into_iter()
            .filter_map(|p| p.jar.clone())
            .chain(local.into_iter().filter_map(|l| l.jar.clone()))
            .chain(transitive.into_iter().filter_map(|p| p.jar.clone()))
            .collect()
    }

    /// Packages whose jars belong inside a fat jar, or on the `Class-Path` of a
    /// thin one: everything on the runtime classpath that has a coordinate.
    pub fn runtime_packages(&self) -> impl Iterator<Item = &ResolvedPackage> {
        self.packages
            .iter()
            .filter(|p| p.classpath.runs() && p.jar.is_some())
    }

    /// The local jar the manifest calls `name`.
    #[must_use]
    pub fn local_jar(&self, name: &str) -> Option<&LocalJar> {
        self.local.iter().find(|l| l.name == name)
    }
}

// ---- the walk --------------------------------------------------------------

/// A pending node in the breadth-first walk.
struct Pending {
    coord: Coord,
    classpath: Classpath,
    depth: usize,
    /// Exclusions inherited from every ancestor edge, plus this edge's own.
    exclusions: Vec<Ga>,
    parent: Option<Ga>,
    /// Reached as `<type>pom</type>`: it contributes dependencies, not a jar.
    pom_only: bool,
}

struct Selected {
    coord: Coord,
    depth: usize,
    classpath: Classpath,
    direct: bool,
    mediated: bool,
}

/// Resolve the manifest's dependencies into a flat, deduplicated graph.
///
/// This fetches POMs (cheap, and cached), not jars; [`fetch_jars`] does that so
/// the two phases can own different live regions.
///
/// # Errors
///
/// [`JrsError::Resolve`] when a manifest or POM dependency asks for a version
/// range, a POM cannot be fetched or parsed, the worker pool cannot start, or
/// the graph is more than 64 levels deep; [`JrsError::Io`] when the cache
/// cannot be read or written.
///
/// # Panics
///
/// If a worker thread panicked while holding the warnings or POM lock,
/// poisoning it.
#[allow(
    clippy::too_many_lines,
    reason = "one breadth-first walk over shared state (`selected`, `edges`, \
              `packaging`); split up, every piece would take all of it"
)]
pub fn resolve(manifest: &Manifest, fetcher: &Fetcher, jobs: usize) -> Result<Resolution> {
    // The runtime libraries the manifest's languages imply are resolved as
    // direct dependencies, declared last.
    let dependencies = manifest.effective_dependencies();
    for dep in dependencies.iter().chain(&manifest.dev_dependencies) {
        if !dep.is_local() && !dep.is_managed() && is_range(&dep.version) {
            return Err(JrsError::resolve(format!(
                "`{}` asks for the version range `{}`\n\n\
                 jrs resolves exact versions only; pick one, or run `jrs migrate` \
                 against the original build to see what it settled on",
                dep.key(),
                dep.version
            )));
        }
    }

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(jobs.max(1))
        .build()
        .map_err(|e| JrsError::resolve(format!("could not start a worker pool: {e}")))?;
    let ctx = Context {
        fetcher,
        effective: Mutex::new(HashMap::new()),
        warnings: Mutex::new(Vec::new()),
    };
    // Before the walk: a versionless dependency takes its version from here,
    // and every package the walk meets is held to it.
    let managed = managed_versions_with(manifest, &ctx)?;
    let dependencies = with_managed_versions(dependencies, &managed, "dependencies")?;
    let dev_dependencies = with_managed_versions(
        manifest.dev_dependencies.clone(),
        &managed,
        "dev-dependencies",
    )?;

    let mut selected: HashMap<Ga, Selected> = HashMap::new();
    let mut edges: HashMap<Ga, Vec<Ga>> = HashMap::new();
    let mut packaging: HashMap<Ga, String> = HashMap::new();
    let mut level: Vec<Pending> = seed(&dependencies, &dev_dependencies);

    let root_ga =
        |d: &Dependency| Ga::new(&d.group, &d.artifact).with_classifier(d.classifier.clone());
    let roots: Vec<Ga> = dependencies
        .iter()
        .filter(|d| !d.is_local())
        .map(root_ga)
        .collect();
    let test_roots: Vec<Ga> = dev_dependencies
        .iter()
        .filter(|d| !d.is_local())
        .map(root_ga)
        .collect();

    let mut depth = 1;
    while !level.is_empty() {
        // Mediate this level against what is already selected. A `ga` first seen
        // at a shallower depth keeps its version; within a level, the earlier
        // declaration wins.
        let mut admitted: Vec<&Pending> = Vec::new();
        for item in &level {
            let ga = item.coord.ga();
            if let Some(existing) = selected.get_mut(&ga) {
                let differs = compare_versions(&existing.coord.version, &item.coord.version)
                    != std::cmp::Ordering::Equal;
                if differs {
                    existing.mediated = true;
                    ctx.warn(format!(
                        "`{ga}` is requested at both {} and {}; nearest-wins picked {} \
                         (depth {})",
                        existing.coord.version,
                        item.coord.version,
                        existing.coord.version,
                        existing.depth
                    ));
                }
                // A package reached from the compile graph must end up on the
                // compile classpath even if a test path found it first.
                existing.classpath = existing.classpath.join(item.classpath);
                existing.direct |= item.depth == 1;
            } else {
                selected.insert(
                    ga.clone(),
                    Selected {
                        coord: item.coord.clone(),
                        depth: item.depth,
                        classpath: item.classpath,
                        direct: item.depth == 1,
                        mediated: false,
                    },
                );
                admitted.push(item);
            }
            if let Some(parent) = &item.parent {
                edges.entry(parent.clone()).or_default().push(ga.clone());
            }
        }

        // Fetch and parse this level's POMs together.
        let effectives: Vec<Result<(usize, std::sync::Arc<Effective>)>> = pool.install(|| {
            admitted
                .par_iter()
                .enumerate()
                // A classified artifact is described by its unclassified POM.
                .map(|(i, item)| ctx.effective(&item.coord.pom_coord()).map(|e| (i, e)))
                .collect()
        });

        let mut next: Vec<Pending> = Vec::new();
        for result in effectives {
            let (i, eff) = result?;
            let parent = &admitted[i];
            let parent_ga = parent.coord.ga();
            let kind = if parent.pom_only {
                "pom".to_string()
            } else if parent.coord.classifier.is_some() {
                // `natives-linux` of a library is a jar whatever the library's
                // own packaging says.
                "jar".to_string()
            } else {
                eff.packaging.clone()
            };
            packaging.insert(parent_ga.clone(), kind);
            for dep in &eff.dependencies {
                let dep = eff.manage(dep);
                let Some((child, pom_only)) = admissible(&dep, parent, &eff, &managed, &ctx)?
                else {
                    continue;
                };
                next.push(Pending {
                    coord: child,
                    classpath: parent.classpath,
                    depth: depth + 1,
                    exclusions: {
                        let mut ex = parent.exclusions.clone();
                        ex.extend(dep.exclusions.iter().cloned());
                        ex
                    },
                    parent: Some(parent_ga.clone()),
                    pom_only,
                });
            }
        }

        level = next;
        depth += 1;
        if depth > 64 {
            return Err(JrsError::resolve(
                "dependency graph is more than 64 levels deep; refusing to continue",
            ));
        }
    }

    // A package can be widened — from test to compile, say — after its own
    // dependencies were walked with the narrower classpath, when a deeper path
    // to it turns up. The widening has to reach them too, or the program would
    // run without them.
    loop {
        let mut changed = false;
        for (parent, children) in &edges {
            let Some(widest) = selected.get(parent).map(|s| s.classpath) else {
                continue;
            };
            for child in children {
                if let Some(c) = selected.get_mut(child) {
                    let joined = c.classpath.join(widest);
                    if joined != c.classpath {
                        c.classpath = joined;
                        changed = true;
                    }
                }
            }
        }
        if !changed {
            break;
        }
    }

    let mut packages: Vec<ResolvedPackage> = selected
        .into_iter()
        .map(|(ga, s)| ResolvedPackage {
            dependencies: {
                let mut d = edges.remove(&ga).unwrap_or_default();
                d.sort();
                d.dedup();
                d
            },
            packaging: packaging.remove(&ga).unwrap_or_else(|| "jar".to_string()),
            managed: managed.get(&ga.group, &ga.artifact) == Some(s.coord.version.as_str()),
            coord: s.coord,
            classpath: s.classpath,
            depth: s.depth,
            direct: s.direct,
            jar: None,
            checksum: None,
            mediated: s.mediated,
        })
        .collect();
    packages.sort_by(|a, b| a.coord.cmp(&b.coord));

    let mut resolution = Resolution {
        packages,
        roots,
        test_roots,
        warnings: ctx.warnings.into_inner().unwrap(),
        downloaded: 0,
        local: local_jars(manifest),
    };
    attach_local(&mut resolution, &manifest.root)?;
    Ok(resolution)
}

/// The manifest's local jars, not yet read: `[dependencies]` in declaration
/// order, then `[dev-dependencies]`.
fn local_jars(manifest: &Manifest) -> Vec<LocalJar> {
    let main = manifest.dependencies.iter().map(|d| (d, false));
    let dev = manifest.dev_dependencies.iter().map(|d| (d, true));
    main.chain(dev)
        .filter_map(|(d, dev)| {
            let path = d.path.clone()?;
            let classpath = if dev {
                Classpath::Test
            } else if d.compile_only {
                Classpath::Provided
            } else if d.runtime_only {
                Classpath::Runtime
            } else {
                Classpath::Compile
            };
            Some(LocalJar {
                name: d.artifact.clone(),
                path,
                classpath,
                checksum: None,
                jar: None,
            })
        })
        .collect()
}

/// The versions `[managed]` decides (SPEC §8.9): its own entries first, then
/// each BOM's `<dependencyManagement>` in declaration order, the first to name
/// an artifact winning. They apply to every classifier of `group:artifact`.
#[derive(Debug, Clone, Default)]
pub struct ManagedVersions {
    versions: HashMap<(String, String), String>,
}

impl ManagedVersions {
    /// The version `group:artifact` is held to, if it is managed.
    #[must_use]
    pub fn get(&self, group: &str, artifact: &str) -> Option<&str> {
        self.versions
            .get(&(group.to_string(), artifact.to_string()))
            .map(String::as_str)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.versions.is_empty()
    }
}

/// Read `[managed]`, fetching each BOM it names, for `jrs add` to tell whether
/// a dependency can go in without a version. Resolution reads it itself.
///
/// # Errors
///
/// [`JrsError::Resolve`] when a BOM cannot be fetched or parsed.
pub fn managed_versions(manifest: &Manifest, fetcher: &Fetcher) -> Result<ManagedVersions> {
    let ctx = Context {
        fetcher,
        effective: Mutex::new(HashMap::new()),
        warnings: Mutex::new(Vec::new()),
    };
    managed_versions_with(manifest, &ctx)
}

fn managed_versions_with(manifest: &Manifest, ctx: &Context) -> Result<ManagedVersions> {
    let mut versions: HashMap<(String, String), String> = HashMap::new();
    for m in manifest.managed.iter().filter(|m| !m.bom) {
        versions
            .entry((m.group.clone(), m.artifact.clone()))
            .or_insert_with(|| m.version.clone());
    }
    for m in manifest.managed.iter().filter(|m| m.bom) {
        let coord = Coord::new(&m.group, &m.artifact, &m.version);
        let bom = ctx.effective(&coord).map_err(|e| {
            JrsError::resolve(format!(
                "the BOM `{coord}` in [managed] could not be read\n\n{e}"
            ))
        })?;
        for (ga, entry) in &bom.managed {
            let Some(version) = &entry.version else {
                continue;
            };
            if version.contains("${") || is_range(version) {
                continue;
            }
            versions
                .entry((ga.group.clone(), ga.artifact.clone()))
                .or_insert_with(|| version.clone());
        }
    }
    Ok(ManagedVersions { versions })
}

/// Give each versionless dependency in `section` its managed version.
fn with_managed_versions(
    mut dependencies: Vec<Dependency>,
    managed: &ManagedVersions,
    section: &str,
) -> Result<Vec<Dependency>> {
    for d in dependencies.iter_mut().filter(|d| d.is_managed()) {
        let version = managed.get(&d.group, &d.artifact).ok_or_else(|| {
            JrsError::resolve(format!(
                "`{}` in [{section}] has no version, and neither [managed] nor a BOM it \
                 names manages it\n\ngive it a version, or add it to [managed]:\n\n    \
                 [managed]\n    \"{}:{}\" = \"<version>\"",
                d.key(),
                d.group,
                d.artifact
            ))
        })?;
        d.version = version.to_string();
    }
    Ok(dependencies)
}

/// Resolve a tool — a compiler — as a graph of its own, never merged into the
/// project's: the Kotlin compiler's `kotlinx-coroutines` must not mediate
/// against the project's (`JVM_LANGUAGES.md` §5.1). It is an ordinary
/// resolution of a manifest that declares `roots` and nothing else; a task's
/// own dependencies (TASKS.md §8) go through [`resolve_tool_dependencies`].
///
/// # Errors
///
/// As for [`resolve`].
pub fn resolve_tool(roots: &[Coord], fetcher: &Fetcher, jobs: usize) -> Result<Resolution> {
    let roots: Vec<Dependency> = roots
        .iter()
        .map(|c| {
            let mut d = Dependency::new(&c.group, &c.artifact, &c.version);
            d.classifier.clone_from(&c.classifier);
            d
        })
        .collect();
    resolve_tool_dependencies(&roots, fetcher, jobs)
}

/// Resolve a java agent named with a version as a graph of one: the agent
/// alone, each of its own dependencies excluded. `-javaagent:` loads that one
/// jar, so nothing it depends on would ever be on a classpath, and an agent
/// meant to go there, such as the OpenTelemetry one, is shaded for exactly
/// that reason.
///
/// # Errors
///
/// As for [`resolve`].
pub fn resolve_agent(coord: &Coord, fetcher: &Fetcher, jobs: usize) -> Result<Resolution> {
    let mut agent = Dependency::new(&coord.group, &coord.artifact, &coord.version);
    agent.classifier.clone_from(&coord.classifier);
    agent.exclusions.push(crate::manifest::Exclusion {
        group: "*".into(),
        artifact: "*".into(),
    });
    resolve_tool_dependencies(&[agent], fetcher, jobs)
}

/// [`resolve_tool`] for declared dependencies, with their exclusions: a
/// task's `[tasks.<name>.dependencies]` (TASKS.md §8).
///
/// # Errors
///
/// As for [`resolve`].
pub fn resolve_tool_dependencies(
    dependencies: &[Dependency],
    fetcher: &Fetcher,
    jobs: usize,
) -> Result<Resolution> {
    let mut manifest = crate::manifest::blank("tool", "0", std::path::Path::new("."));
    manifest.dependencies = dependencies.to_vec();
    resolve(&manifest, fetcher, jobs)
}

fn seed(dependencies: &[Dependency], dev_dependencies: &[Dependency]) -> Vec<Pending> {
    let mut out = Vec::new();
    let mut push = |d: &Dependency, classpath: Classpath| {
        out.push(Pending {
            coord: Coord::new(&d.group, &d.artifact, &d.version)
                .with_classifier(d.classifier.clone()),
            classpath: if d.compile_only {
                Classpath::Provided
            } else if d.runtime_only {
                Classpath::Runtime
            } else {
                classpath
            },
            depth: 1,
            exclusions: d
                .exclusions
                .iter()
                .map(|e| Ga::new(&e.group, &e.artifact))
                .collect(),
            parent: None,
            pom_only: false,
        });
    };
    // A local jar has no coordinate to walk; `local_jars` takes it instead.
    for d in dependencies.iter().filter(|d| !d.is_local()) {
        push(d, Classpath::Compile);
    }
    for d in dev_dependencies.iter().filter(|d| !d.is_local()) {
        push(d, Classpath::Test);
    }
    out
}

/// Decide whether a child dependency is walked at all (SPEC §8.2 step 4), and
/// if so as which coordinate, and whether it is a POM with no jar. A version
/// `[managed]` holds the artifact to replaces the one the POM asks for.
fn admissible(
    dep: &PomDependency,
    parent: &Pending,
    eff: &Effective,
    managed: &ManagedVersions,
    ctx: &Context,
) -> Result<Option<(Coord, bool)>> {
    if dep.optional || !dep.scope().is_transitive() {
        return Ok(None);
    }
    // `<type>` names a kind of file. Most kinds are jars under another name;
    // a `test-jar` is the jar classified `tests`; a `pom` contributes its
    // dependencies and nothing else. What cannot go on a classpath at all is
    // reported rather than silently mis-resolved.
    let (classifier, pom_only) = match dep.kind.as_str() {
        "jar" | "bundle" | "ejb" | "maven-plugin" => (dep.classifier.clone(), false),
        "test-jar" => (
            Some(dep.classifier.clone().unwrap_or_else(|| "tests".into())),
            false,
        ),
        "pom" => (None, true),
        other => {
            ctx.warn(format!(
                "`{}` depends on `{}` of type `{other}`, which does not go on a \
                 classpath; skipped",
                eff.coord,
                dep.ga()
            ));
            return Ok(None);
        }
    };
    let ga = dep.ga();
    if parent.exclusions.iter().any(|e| ga.excluded_by(e)) {
        return Ok(None);
    }
    if let Some(version) = managed.get(&ga.group, &ga.artifact) {
        return Ok(Some((
            Coord::new(&ga.group, &ga.artifact, version).with_classifier(classifier),
            pom_only,
        )));
    }
    let Some(version) = &dep.version else {
        ctx.warn(format!(
            "`{}` declares `{ga}` with no version and no dependencyManagement entry; skipped",
            eff.coord
        ));
        return Ok(None);
    };
    if is_range(version) {
        return Err(JrsError::resolve(format!(
            "`{}` depends on `{ga}` with the version range `{version}`\n\n\
             jrs resolves exact versions only; exclude the dependency and declare \
             an exact version in jrs.toml",
            eff.coord
        )));
    }
    if version.contains("${") {
        ctx.warn(format!(
            "`{}` declares `{ga}` with an unresolvable version `{version}`; skipped",
            eff.coord
        ));
        return Ok(None);
    }
    Ok(Some((
        Coord::new(&ga.group, &ga.artifact, version).with_classifier(classifier),
        pom_only,
    )))
}

struct Context<'a> {
    fetcher: &'a Fetcher,
    effective: Mutex<HashMap<Coord, std::sync::Arc<Effective>>>,
    warnings: Mutex<Vec<String>>,
}

impl Context<'_> {
    fn warn(&self, msg: String) {
        let mut w = self.warnings.lock().unwrap();
        if !w.contains(&msg) {
            w.push(msg);
        }
    }

    /// The effective POM for `coord`: its parent chain merged, its properties
    /// interpolated, and any imported BOMs folded in.
    fn effective(&self, coord: &Coord) -> Result<std::sync::Arc<Effective>> {
        if let Some(hit) = self.effective.lock().unwrap().get(coord) {
            return Ok(hit.clone());
        }
        let built = std::sync::Arc::new(self.build_effective(coord, 0)?);
        self.effective
            .lock()
            .unwrap()
            .insert(coord.clone(), built.clone());
        Ok(built)
    }

    fn build_effective(&self, coord: &Coord, import_depth: usize) -> Result<Effective> {
        let mut chain: Vec<Pom> = Vec::new();
        let mut current = coord.clone();
        let mut seen: HashSet<Coord> = HashSet::new();
        loop {
            if !seen.insert(current.clone()) {
                self.warn(format!("`{coord}` has a cyclic <parent> chain; truncated"));
                break;
            }
            let bytes = self.fetcher.pom(&current)?;
            let parsed =
                Pom::parse(&bytes).map_err(|e| JrsError::resolve(format!("{current}: {e}")))?;
            let parent = parsed.parent.clone();
            chain.push(parsed);
            match parent {
                Some(p) if chain.len() < 32 => {
                    current = Coord::new(&p.group, &p.artifact, &p.version);
                }
                _ => break,
            }
        }

        let mut eff = pom::effective(&chain)?;
        if import_depth < 8 {
            for import in eff.imports.clone() {
                match self.build_effective(&import, import_depth + 1) {
                    Ok(bom) => eff.absorb_import(&bom),
                    Err(e) => self.warn(format!(
                        "`{coord}` imports the BOM `{import}`, which could not be read: {e}"
                    )),
                }
            }
        }
        Ok(eff)
    }
}

// ---- downloading -----------------------------------------------------------

/// One package's fetched jar: its index, its path on disk, and its checksum.
type FetchedJar = (usize, Option<PathBuf>, Option<String>);

/// Download every resolved jar, in parallel, filling in `jar` and `checksum`.
///
/// # Errors
///
/// [`JrsError::Resolve`] when the worker pool cannot start, a jar cannot be
/// found or downloaded, or it does not match its repository or locked
/// checksum; [`JrsError::Io`] when the cache cannot be read or written.
pub fn fetch_jars(resolution: &mut Resolution, fetcher: &Fetcher, jobs: usize) -> Result<()> {
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(jobs.max(1))
        .build()
        .map_err(|e| JrsError::resolve(format!("could not start a worker pool: {e}")))?;

    let jars: Vec<Result<FetchedJar>> = pool.install(|| {
        resolution
            .packages
            .par_iter()
            .enumerate()
            .map(|(i, p)| {
                if p.packaging == "pom" {
                    // An aggregate or BOM has no jar to link against; it earned
                    // its place in the graph by contributing dependencies.
                    return Ok((i, None, None));
                }
                if p.coord.is_snapshot() {
                    // A snapshot is republished under the same name by design,
                    // so there is nothing stable to pin.
                    let (path, _origin) = fetcher.jar(&p.coord)?;
                    return Ok((i, Some(path), None));
                }
                // A checksum carried over from the lockfile pins any download.
                let (path, _origin) = fetcher.jar_pinned(&p.coord, p.checksum.as_deref())?;
                // It is not recomputed for a jar that was already cached:
                // hashing every cached jar on every build would be the slowest
                // thing an up-to-date build does.
                let checksum = p.checksum.clone().or_else(|| {
                    std::fs::read(&path)
                        .ok()
                        .map(|bytes| format!("sha1:{}", repo::sha1_hex(&bytes)))
                });
                Ok((i, Some(path), checksum))
            })
            .collect()
    });

    for result in jars {
        let (i, path, checksum) = result?;
        resolution.packages[i].jar = path;
        resolution.packages[i].checksum = checksum;
    }
    resolution.downloaded = fetcher.downloaded();
    resolution.warnings.extend(fetcher.take_warnings());
    Ok(())
}

/// What `jrs verify` found for one locked jar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Integrity {
    /// The cached jar hashes to the checksum the lockfile pins.
    Verified,
    /// Not cached; the next build downloads it and checks it against the pin.
    NotCached,
    /// The lockfile records no checksum jrs can check.
    Unpinned,
    /// A snapshot, which is republished under one name by design and so is
    /// never pinned.
    Snapshot,
    Mismatch {
        expected: String,
        actual: String,
    },
}

#[derive(Debug, Clone)]
pub struct Checked {
    pub coord: Coord,
    pub path: PathBuf,
    pub integrity: Integrity,
}

/// Re-hash every cached jar the resolution names against its recorded checksum.
///
/// Builds deliberately never do this for jars already in the cache — it would
/// make a no-op build hash megabytes — so it is its own command.
///
/// # Errors
///
/// [`JrsError::Resolve`] when the worker pool cannot start; [`JrsError::Io`]
/// when a cached jar cannot be read.
pub fn verify_cached(
    resolution: &Resolution,
    cache: &cache::Cache,
    jobs: usize,
) -> Result<Vec<Checked>> {
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(jobs.max(1))
        .build()
        .map_err(|e| JrsError::resolve(format!("could not start a worker pool: {e}")))?;
    pool.install(|| {
        resolution
            .packages
            .par_iter()
            .filter(|p| p.packaging != "pom")
            .map(|p| {
                let path = cache.path_for(&p.coord, "jar");
                let integrity = match p.checksum.as_deref() {
                    _ if !path.is_file() => Integrity::NotCached,
                    _ if p.coord.is_snapshot() => Integrity::Snapshot,
                    None => Integrity::Unpinned,
                    Some(pin) => {
                        let bytes = std::fs::read(&path).map_err(|e| JrsError::io(&path, e))?;
                        match repo::digest_as(pin, &bytes) {
                            None => Integrity::Unpinned,
                            Some(actual) if actual.eq_ignore_ascii_case(pin) => Integrity::Verified,
                            Some(actual) => Integrity::Mismatch {
                                expected: pin.to_string(),
                                actual,
                            },
                        }
                    }
                };
                Ok(Checked {
                    coord: p.coord.clone(),
                    path,
                    integrity,
                })
            })
            .collect()
    })
}

/// Report the classpath jars that are already cached, without downloading.
pub fn locate_cached(resolution: &mut Resolution, fetcher: &Fetcher) {
    for p in &mut resolution.packages {
        let path = fetcher.cache().path_for(&p.coord, "jar");
        if path.is_file() {
            p.jar = Some(path);
        }
    }
}

/// The `-sources.jar` of every package with a jar in `resolution`, once each
/// (a library and its natives share one), sorted by coordinate.
#[must_use]
pub fn sources_coords(resolution: &Resolution) -> Vec<Coord> {
    let mut coords: Vec<Coord> = resolution
        .packages
        .iter()
        .filter(|p| p.packaging != "pom")
        .map(|p| p.coord.sources())
        .collect();
    coords.sort();
    coords.dedup();
    coords
}

/// Download each of `coords` — `-sources.jar`s, for `jrs fetch --sources` —
/// in parallel, through the cache like any jar. Each comes back with its own
/// result, in the order given: a library that publishes no sources is common,
/// and the caller decides what a missing one means.
///
/// # Errors
///
/// [`JrsError::Resolve`] when the worker pool cannot start.
pub fn fetch_sources(
    coords: &[Coord],
    fetcher: &Fetcher,
    jobs: usize,
) -> Result<Vec<(Coord, Result<PathBuf>)>> {
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(jobs.max(1))
        .build()
        .map_err(|e| JrsError::resolve(format!("could not start a worker pool: {e}")))?;
    Ok(pool.install(|| {
        coords
            .par_iter()
            .map(|c| (c.clone(), fetcher.jar(c).map(|(path, _)| path)))
            .collect()
    }))
}

/// Where `coord`'s `-sources.jar` is in the cache, if it is there.
#[must_use]
pub fn cached_sources(coord: &Coord, cache: &cache::Cache) -> Option<PathBuf> {
    Some(cache.path_for(&coord.sources(), "jar")).filter(|p| p.is_file())
}

// ---- local jars ------------------------------------------------------------

/// Find each local jar under `root`, the project directory, hash it, and fill
/// in its `jar`; one already found is left alone. [`resolve`] does this
/// itself; a resolution read back from `jrs.lock` needs it done.
///
/// The file is hashed on every build, unlike a cached jar: it is the only way
/// to notice that a jar under the same name has changed, and a checked-in jar
/// is small next to what a build reads anyway.
///
/// # Errors
///
/// [`JrsError::Manifest`] when a local jar does not exist or is a directory;
/// [`JrsError::Resolve`] when it no longer matches the checksum `jrs.lock`
/// pinned for it; [`JrsError::Io`] when it cannot be read.
pub fn attach_local(resolution: &mut Resolution, root: &Path) -> Result<()> {
    for local in resolution.local.iter_mut().filter(|l| l.jar.is_none()) {
        let path = root.join(&local.path);
        if !path.is_file() {
            let what = if path.is_dir() {
                "is a directory; name the jar itself, one entry per jar"
            } else {
                "does not exist"
            };
            return Err(JrsError::manifest(format!(
                "the local jar `{}` = {} {what}\n\n`path` is relative to the project \
                 root, {}",
                local.name,
                local.path,
                root.display()
            )));
        }
        let bytes = std::fs::read(&path).map_err(|e| JrsError::io(&path, e))?;
        let actual = format!("sha256:{}", repo::sha256_hex(&bytes));
        if let Some(pin) = &local.checksum
            && let Some(found) = repo::digest_as(pin, &bytes)
            && !found.eq_ignore_ascii_case(pin)
        {
            return Err(JrsError::resolve(format!(
                "the local jar `{}` = {} has changed since jrs.lock pinned it\n\n  \
                 locked {pin}\n  got    {found}\n\n\
                 if it was replaced on purpose, run `jrs update` to pin the new one",
                local.name, local.path
            )));
        }
        local.checksum.get_or_insert(actual);
        local.jar = Some(path);
    }
    Ok(())
}

/// What `jrs verify` finds for a local jar at `path`, against `pin`.
/// [`Integrity::NotCached`] means the file is not there at all.
///
/// # Errors
///
/// [`JrsError::Io`] when the file exists but cannot be read.
pub fn local_integrity(path: &Path, pin: Option<&str>) -> Result<Integrity> {
    if !path.is_file() {
        return Ok(Integrity::NotCached);
    }
    let Some(pin) = pin else {
        return Ok(Integrity::Unpinned);
    };
    let bytes = std::fs::read(path).map_err(|e| JrsError::io(path, e))?;
    Ok(match repo::digest_as(pin, &bytes) {
        None => Integrity::Unpinned,
        Some(actual) if actual.eq_ignore_ascii_case(pin) => Integrity::Verified,
        Some(actual) => Integrity::Mismatch {
            expected: pin.to_string(),
            actual,
        },
    })
}

// ---- the download bars -----------------------------------------------------

/// Publishes transfer progress into the UI's live region.
pub struct UiReporter {
    ui: Ui,
    next_id: AtomicU64,
}

impl UiReporter {
    #[must_use]
    pub fn new(ui: Ui) -> UiReporter {
        UiReporter {
            ui,
            next_id: AtomicU64::new(1),
        }
    }

    fn with_transfer(&self, id: u64, f: impl FnOnce(&mut Transfer)) {
        self.ui.update_live(|live| {
            if let Live::Downloads(d) = live
                && let Some(t) = d.active.iter_mut().find(|t| t.id == id)
            {
                f(t);
            }
        });
    }
}

impl TransferReporter for UiReporter {
    fn start(&self, name: &str) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let name = name.to_string();
        self.ui.update_live(|live| {
            if let Live::Downloads(d) = live {
                d.active.push(Transfer {
                    id,
                    name,
                    done: 0,
                    total: None,
                    verifying: false,
                });
            }
        });
        id
    }

    fn set_total(&self, id: u64, total: Option<u64>) {
        self.with_transfer(id, |t| t.total = total);
    }

    fn advance(&self, id: u64, bytes: u64) {
        self.with_transfer(id, |t| t.done += bytes);
    }

    fn verifying(&self, id: u64) {
        self.with_transfer(id, |t| t.verifying = true);
    }

    fn restart(&self, id: u64) {
        self.with_transfer(id, |t| {
            t.done = 0;
            t.total = None;
            t.verifying = false;
        });
    }

    fn finish(&self, id: u64) {
        self.ui.update_live(|live| {
            if let Live::Downloads(d) = live
                && let Some(pos) = d.active.iter().position(|t| t.id == id)
            {
                d.active.remove(pos);
                d.finished += 1;
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest;
    use std::path::Path;

    /// A repository fixture on disk: POMs and jars written by hand, served over
    /// `file://`, so resolution can be tested end to end without a network.
    struct Repo {
        dir: PathBuf,
    }

    impl Repo {
        fn new(name: &str) -> Repo {
            let dir =
                std::env::temp_dir().join(format!("jrs-resolve-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Repo { dir }
        }

        fn repo_dir(&self) -> PathBuf {
            self.dir.join("repo")
        }

        fn publish(&self, gav: &str, body: &str) {
            let coord = Coord::parse(gav).unwrap();
            let pom = format!(
                "<project><groupId>{}</groupId><artifactId>{}</artifactId>\
                 <version>{}</version>{body}</project>",
                coord.group, coord.artifact, coord.version
            );
            self.write(&coord, "pom", pom.as_bytes());
            self.write(&coord, "jar", format!("jar of {gav}").as_bytes());
        }

        fn publish_pom_only(&self, gav: &str, body: &str) {
            let coord = Coord::parse(gav).unwrap();
            let pom = format!(
                "<project><groupId>{}</groupId><artifactId>{}</artifactId>\
                 <version>{}</version><packaging>pom</packaging>{body}</project>",
                coord.group, coord.artifact, coord.version
            );
            self.write(&coord, "pom", pom.as_bytes());
        }

        fn write(&self, coord: &Coord, ext: &str, bytes: &[u8]) {
            let path = self.repo_dir().join(coord.repo_path(ext));
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, bytes).unwrap();
            std::fs::write(
                path.with_extension(format!("{ext}.sha1")),
                repo::sha1_hex(bytes),
            )
            .unwrap();
        }

        fn fetcher(&self) -> Fetcher {
            Fetcher::new(
                vec![manifest::Repository::new(
                    "fixture",
                    repo::file_url(&self.repo_dir()),
                )],
                cache::Cache::with_root(self.dir.join("cache")),
                false,
            )
        }

        fn manifest(&self, deps: &str) -> Manifest {
            let text = format!("[project]\nname='app'\nversion='1.0.0'\n{deps}");
            Manifest::parse(&text, Path::new("/p/jrs.toml"), Path::new("/p")).unwrap()
        }
    }

    impl Drop for Repo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn dep(body: &str) -> String {
        format!("<dependencies>{body}</dependencies>")
    }

    fn d(gav: &str, extra: &str) -> String {
        let c = Coord::parse(gav).unwrap();
        format!(
            "<dependency><groupId>{}</groupId><artifactId>{}</artifactId>\
             <version>{}</version>{extra}</dependency>",
            c.group, c.artifact, c.version
        )
    }

    fn names(r: &Resolution) -> Vec<String> {
        let mut v: Vec<String> = r.packages.iter().map(|p| p.coord.to_string()).collect();
        v.sort();
        v
    }

    #[test]
    fn transitive_dependencies_are_walked() {
        let repo = Repo::new("transitive");
        repo.publish("g:a:1.0", &dep(&d("g:b:1.0", "")));
        repo.publish("g:b:1.0", &dep(&d("g:c:1.0", "")));
        repo.publish("g:c:1.0", "");

        let m = repo.manifest("[dependencies]\n'g:a'='1.0'");
        let r = resolve(&m, &repo.fetcher(), 4).unwrap();
        assert_eq!(names(&r), vec!["g:a:1.0", "g:b:1.0", "g:c:1.0"]);
        assert_eq!(r.get(&Ga::new("g", "a")).unwrap().depth, 1);
        assert_eq!(r.get(&Ga::new("g", "c")).unwrap().depth, 3);
    }

    #[test]
    fn cycles_terminate() {
        let repo = Repo::new("cycle");
        repo.publish("g:a:1.0", &dep(&d("g:b:1.0", "")));
        repo.publish("g:b:1.0", &dep(&d("g:a:1.0", "")));

        let m = repo.manifest("[dependencies]\n'g:a'='1.0'");
        let r = resolve(&m, &repo.fetcher(), 4).unwrap();
        assert_eq!(names(&r), vec!["g:a:1.0", "g:b:1.0"]);
    }

    #[test]
    fn conflicts_are_mediated_nearest_wins() {
        let repo = Repo::new("nearest");
        // app -> a -> shared:1.0
        // app -> shared:2.0   (shallower, so it wins)
        repo.publish("g:a:1.0", &dep(&d("g:shared:1.0", "")));
        repo.publish("g:shared:1.0", "");
        repo.publish("g:shared:2.0", "");

        let m = repo.manifest("[dependencies]\n'g:a'='1.0'\n'g:shared'='2.0'");
        let r = resolve(&m, &repo.fetcher(), 4).unwrap();
        let shared = r.get(&Ga::new("g", "shared")).unwrap();
        assert_eq!(shared.coord.version, "2.0");
        assert!(shared.mediated);
        assert!(
            r.warnings
                .iter()
                .any(|w| w.contains("1.0") && w.contains("2.0")),
            "{:?}",
            r.warnings
        );
    }

    #[test]
    fn ties_within_a_level_go_to_the_earlier_declaration() {
        let repo = Repo::new("ties");
        repo.publish("g:first:1.0", &dep(&d("g:shared:1.0", "")));
        repo.publish("g:second:1.0", &dep(&d("g:shared:2.0", "")));
        repo.publish("g:shared:1.0", "");
        repo.publish("g:shared:2.0", "");

        let m = repo.manifest("[dependencies]\n'g:first'='1.0'\n'g:second'='1.0'");
        let r = resolve(&m, &repo.fetcher(), 4).unwrap();
        assert_eq!(r.get(&Ga::new("g", "shared")).unwrap().coord.version, "1.0");

        // Reversing the declaration order reverses the outcome.
        let m = repo.manifest("[dependencies]\n'g:second'='1.0'\n'g:first'='1.0'");
        let r = resolve(&m, &repo.fetcher(), 4).unwrap();
        assert_eq!(r.get(&Ga::new("g", "shared")).unwrap().coord.version, "2.0");
    }

    #[test]
    fn test_provided_and_optional_dependencies_are_not_walked() {
        let repo = Repo::new("scopes");
        repo.publish(
            "g:a:1.0",
            &dep(&format!(
                "{}{}{}{}",
                d("g:runtime-dep:1.0", "<scope>runtime</scope>"),
                d("g:test-dep:1.0", "<scope>test</scope>"),
                d("g:provided-dep:1.0", "<scope>provided</scope>"),
                d("g:optional-dep:1.0", "<optional>true</optional>"),
            )),
        );
        repo.publish("g:runtime-dep:1.0", "");
        repo.publish("g:test-dep:1.0", "");
        repo.publish("g:provided-dep:1.0", "");
        repo.publish("g:optional-dep:1.0", "");

        let m = repo.manifest("[dependencies]\n'g:a'='1.0'");
        let r = resolve(&m, &repo.fetcher(), 4).unwrap();
        assert_eq!(names(&r), vec!["g:a:1.0", "g:runtime-dep:1.0"]);
    }

    #[test]
    fn exclusions_are_honoured_and_inherited() {
        let repo = Repo::new("exclusions");
        repo.publish("g:a:1.0", &dep(&d("g:b:1.0", "")));
        repo.publish("g:b:1.0", &dep(&d("g:c:1.0", "")));
        repo.publish("g:c:1.0", "");

        let excluded = d(
            "g:a:1.0",
            "<exclusions><exclusion><groupId>g</groupId>\
             <artifactId>c</artifactId></exclusion></exclusions>",
        );
        repo.publish("g:root:1.0", &dep(&excluded));

        let m = repo.manifest("[dependencies]\n'g:root'='1.0'");
        let r = resolve(&m, &repo.fetcher(), 4).unwrap();
        assert_eq!(
            names(&r),
            vec!["g:a:1.0", "g:b:1.0", "g:root:1.0"],
            "the exclusion must survive one more level down"
        );
    }

    #[test]
    fn dev_dependencies_land_on_the_test_classpath_only() {
        let repo = Repo::new("dev");
        repo.publish("g:main:1.0", "");
        repo.publish("g:testing:1.0", &dep(&d("g:testing-core:1.0", "")));
        repo.publish("g:testing-core:1.0", "");

        let m =
            repo.manifest("[dependencies]\n'g:main'='1.0'\n[dev-dependencies]\n'g:testing'='1.0'");
        let mut r = resolve(&m, &repo.fetcher(), 4).unwrap();
        fetch_jars(&mut r, &repo.fetcher(), 4).unwrap();

        assert_eq!(
            r.get(&Ga::new("g", "main")).unwrap().classpath,
            Classpath::Compile
        );
        assert_eq!(
            r.get(&Ga::new("g", "testing-core")).unwrap().classpath,
            Classpath::Test
        );
        assert_eq!(r.classpath(Classpath::Compile).len(), 1);
        assert_eq!(r.classpath(Classpath::Test).len(), 3);
    }

    #[test]
    fn a_compile_path_widens_a_test_only_selection() {
        let repo = Repo::new("widen");
        repo.publish("g:testing:1.0", &dep(&d("g:shared:1.0", "")));
        repo.publish("g:lib:1.0", &dep(&d("g:shared:1.0", "")));
        repo.publish("g:shared:1.0", "");

        let m =
            repo.manifest("[dependencies]\n'g:lib'='1.0'\n[dev-dependencies]\n'g:testing'='1.0'");
        let r = resolve(&m, &repo.fetcher(), 4).unwrap();
        assert_eq!(
            r.get(&Ga::new("g", "shared")).unwrap().classpath,
            Classpath::Compile,
            "a package reachable from compile code belongs on the compile classpath"
        );
    }

    #[test]
    fn parent_poms_and_dependency_management_are_applied() {
        let repo = Repo::new("parent");
        repo.publish_pom_only(
            "g:parent:1.0",
            "<properties><lib.version>2.0</lib.version></properties>\
             <dependencyManagement><dependencies>\
             <dependency><groupId>g</groupId><artifactId>lib</artifactId>\
             <version>${lib.version}</version></dependency>\
             </dependencies></dependencyManagement>",
        );
        let child_pom = "<project><parent><groupId>g</groupId><artifactId>parent</artifactId>\
             <version>1.0</version></parent><artifactId>child</artifactId>\
             <dependencies><dependency><groupId>g</groupId>\
             <artifactId>lib</artifactId></dependency></dependencies></project>"
            .to_string();
        let child = Coord::new("g", "child", "1.0");
        repo.write(&child, "pom", child_pom.as_bytes());
        repo.write(&child, "jar", b"child jar");
        repo.publish("g:lib:2.0", "");

        let m = repo.manifest("[dependencies]\n'g:child'='1.0'");
        let r = resolve(&m, &repo.fetcher(), 4).unwrap();
        assert_eq!(names(&r), vec!["g:child:1.0", "g:lib:2.0"]);
    }

    #[test]
    fn imported_boms_supply_versions() {
        let repo = Repo::new("bom");
        repo.publish_pom_only(
            "g:bom:1.0",
            "<dependencyManagement><dependencies>\
             <dependency><groupId>g</groupId><artifactId>lib</artifactId>\
             <version>3.0</version></dependency>\
             </dependencies></dependencyManagement>",
        );
        repo.publish_pom_only(
            "g:uses-bom:1.0",
            "<dependencyManagement><dependencies>\
             <dependency><groupId>g</groupId><artifactId>bom</artifactId>\
             <version>1.0</version><type>pom</type><scope>import</scope></dependency>\
             </dependencies></dependencyManagement>\
             <dependencies><dependency><groupId>g</groupId>\
             <artifactId>lib</artifactId></dependency></dependencies>",
        );
        repo.publish("g:lib:3.0", "");

        let m = repo.manifest("[dependencies]\n'g:uses-bom'='1.0'");
        let r = resolve(&m, &repo.fetcher(), 4).unwrap();
        assert!(
            names(&r).contains(&"g:lib:3.0".to_string()),
            "{:?}",
            names(&r)
        );
    }

    #[test]
    fn a_managed_version_beats_what_the_graph_asks_for() {
        let repo = Repo::new("managed");
        repo.publish(
            "g:a:1.0",
            &dep(&format!(
                "{}{}",
                d("g:shared:1.0", ""),
                "<dependency><groupId>g</groupId><artifactId>ranged</artifactId>\
                 <version>[1.0,2.0)</version></dependency>"
            )),
        );
        repo.publish("g:shared:1.0", "");
        repo.publish("g:shared:3.0", "");
        repo.publish("g:ranged:1.5", "");

        // Held to 3.0 wherever it turns up, and a range [managed] settles is
        // no longer an error.
        let m = repo
            .manifest("[managed]\n'g:shared'='3.0'\n'g:ranged'='1.5'\n[dependencies]\n'g:a'='1.0'");
        let r = resolve(&m, &repo.fetcher(), 4).unwrap();
        let shared = r.get(&Ga::new("g", "shared")).unwrap();
        assert_eq!(shared.coord.version, "3.0");
        assert!(shared.managed && !shared.mediated);
        assert_eq!(r.get(&Ga::new("g", "ranged")).unwrap().coord.version, "1.5");
        assert!(!r.get(&Ga::new("g", "a")).unwrap().managed);
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);

        // A version declared in [dependencies] still wins for that dependency.
        let m = repo.manifest(
            "[managed]\n'g:shared'='3.0'\n'g:ranged'='1.5'\n\
             [dependencies]\n'g:a'='1.0'\n'g:shared'='1.0'",
        );
        let r = resolve(&m, &repo.fetcher(), 4).unwrap();
        let shared = r.get(&Ga::new("g", "shared")).unwrap();
        assert_eq!(shared.coord.version, "1.0");
        assert!(!shared.managed);
    }

    #[test]
    fn a_bom_in_managed_versions_what_the_manifest_leaves_out() {
        let repo = Repo::new("managed-bom");
        repo.publish_pom_only(
            "g:inner-bom:1.0",
            "<dependencyManagement><dependencies>\
             <dependency><groupId>g</groupId><artifactId>testing</artifactId>\
             <version>5.0</version></dependency>\
             </dependencies></dependencyManagement>",
        );
        repo.publish_pom_only(
            "g:bom:1.0",
            "<dependencyManagement><dependencies>\
             <dependency><groupId>g</groupId><artifactId>lib</artifactId>\
             <version>3.0</version></dependency>\
             <dependency><groupId>g</groupId><artifactId>shared</artifactId>\
             <version>2.0</version></dependency>\
             <dependency><groupId>g</groupId><artifactId>inner-bom</artifactId>\
             <version>1.0</version><type>pom</type><scope>import</scope></dependency>\
             </dependencies></dependencyManagement>",
        );
        repo.publish("g:lib:3.0", &dep(&d("g:shared:1.0", "")));
        repo.publish("g:shared:1.0", "");
        repo.publish("g:shared:2.0", "");
        repo.publish("g:shared:4.0", "");
        repo.publish("g:testing:5.0", "");

        let m = repo.manifest(
            "[managed]\n'g:bom'={version='1.0', bom=true}\n\
             [dependencies]\n'g:lib'={}\n[dev-dependencies]\n'g:testing'={}",
        );
        let r = resolve(&m, &repo.fetcher(), 4).unwrap();
        assert_eq!(
            names(&r),
            vec!["g:lib:3.0", "g:shared:2.0", "g:testing:5.0"],
            "the BOM's own imports count too"
        );
        assert!(r.packages.iter().all(|p| p.managed));
        assert_eq!(r.roots, vec![Ga::new("g", "lib")]);
        assert_eq!(r.test_roots, vec![Ga::new("g", "testing")]);

        // The table's own entries come before any BOM's.
        let m = repo.manifest(
            "[managed]\n'g:bom'={version='1.0', bom=true}\n'g:shared'='4.0'\n\
             [dependencies]\n'g:lib'={}",
        );
        let r = resolve(&m, &repo.fetcher(), 4).unwrap();
        assert_eq!(r.get(&Ga::new("g", "shared")).unwrap().coord.version, "4.0");

        let m = repo
            .manifest("[managed]\n'g:bom'={version='1.0', bom=true}\n[dependencies]\n'g:other'={}");
        let err = resolve(&m, &repo.fetcher(), 4).unwrap_err().to_string();
        assert!(err.contains("neither [managed] nor a BOM"), "{err}");

        let m = repo.manifest("[managed]\n'g:nope'={version='1.0', bom=true}");
        let err = resolve(&m, &repo.fetcher(), 4).unwrap_err().to_string();
        assert!(err.contains("the BOM `g:nope:1.0`"), "{err}");

        let managed = managed_versions(
            &repo.manifest("[managed]\n'g:bom'={version='1.0', bom=true}"),
            &repo.fetcher(),
        )
        .unwrap();
        assert_eq!(managed.get("g", "lib"), Some("3.0"));
        assert_eq!(managed.get("g", "other"), None);
    }

    #[test]
    fn version_ranges_are_rejected_with_a_clear_error() {
        let repo = Repo::new("ranges");
        let m = repo.manifest("[dependencies]\n'g:a'='[1.0,2.0)'");
        let err = resolve(&m, &repo.fetcher(), 4).unwrap_err().to_string();
        assert!(err.contains("version range"), "{err}");
        assert!(err.contains("exact versions only"), "{err}");
    }

    #[test]
    fn a_transitive_range_is_rejected_too() {
        let repo = Repo::new("transitive-range");
        repo.publish(
            "g:a:1.0",
            &dep("<dependency><groupId>g</groupId><artifactId>b</artifactId>\
                  <version>[1.0,2.0)</version></dependency>"),
        );
        let m = repo.manifest("[dependencies]\n'g:a'='1.0'");
        let err = resolve(&m, &repo.fetcher(), 4).unwrap_err().to_string();
        assert!(err.contains("version range"), "{err}");
    }

    #[test]
    fn classpath_order_is_deterministic() {
        let repo = Repo::new("order");
        repo.publish("g:zeta:1.0", &dep(&d("g:alpha-transitive:1.0", "")));
        repo.publish("g:alpha:1.0", "");
        repo.publish("g:alpha-transitive:1.0", "");

        let m = repo.manifest("[dependencies]\n'g:zeta'='1.0'\n'g:alpha'='1.0'");
        let mut r = resolve(&m, &repo.fetcher(), 4).unwrap();
        fetch_jars(&mut r, &repo.fetcher(), 4).unwrap();

        let names: Vec<String> = r
            .classpath(Classpath::Compile)
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            vec!["alpha-1.0.jar", "zeta-1.0.jar", "alpha-transitive-1.0.jar"],
            "direct dependencies first, then transitives, each sorted"
        );
    }

    #[test]
    fn pom_packaged_dependencies_contribute_no_jar() {
        let repo = Repo::new("pom-packaging");
        repo.publish_pom_only("g:aggregate:1.0", &dep(&d("g:real:1.0", "")));
        repo.publish("g:real:1.0", "");

        let m = repo.manifest("[dependencies]\n'g:aggregate'='1.0'");
        let mut r = resolve(&m, &repo.fetcher(), 4).unwrap();
        fetch_jars(&mut r, &repo.fetcher(), 4).unwrap();

        assert!(r.get(&Ga::new("g", "aggregate")).unwrap().jar.is_none());
        assert!(r.get(&Ga::new("g", "real")).unwrap().jar.is_some());
        assert_eq!(
            r.classpath(Classpath::Compile).len(),
            1,
            "an aggregate contributes dependencies, not a classpath entry"
        );
    }

    #[test]
    fn verification_rehashes_the_cache_against_the_recorded_checksums() {
        let repo = Repo::new("verify");
        repo.publish("g:a:1.0", "");
        repo.publish("g:b:1.0", "");
        repo.publish("g:c:1.0", "");
        let m = repo.manifest("[dependencies]\n'g:a'='1.0'\n'g:b'='1.0'\n'g:c'='1.0'");
        let fetcher = repo.fetcher();
        let mut r = resolve(&m, &fetcher, 4).unwrap();
        fetch_jars(&mut r, &fetcher, 4).unwrap();

        // Corrupt one cached jar and evict another.
        std::fs::write(
            fetcher
                .cache()
                .path_for(&Coord::new("g", "b", "1.0"), "jar"),
            b"bad",
        )
        .unwrap();
        fetcher.cache().evict(&Coord::new("g", "c", "1.0"), "jar");

        let checked = verify_cached(&r, fetcher.cache(), 2).unwrap();
        let of = |artifact: &str| {
            checked
                .iter()
                .find(|c| c.coord.artifact == artifact)
                .unwrap()
                .integrity
                .clone()
        };
        assert_eq!(of("a"), Integrity::Verified);
        assert!(
            matches!(of("b"), Integrity::Mismatch { .. }),
            "{:?}",
            of("b")
        );
        assert_eq!(of("c"), Integrity::NotCached);
    }

    #[test]
    fn an_implied_runtime_library_is_a_direct_dependency_that_wins_mediation() {
        let repo = Repo::new("implied");
        // A library built against an older stdlib, which nearest-wins must
        // not let onto the classpath beside the compiler's own.
        repo.publish(
            "g:lib:1.0",
            &dep(&d("org.jetbrains.kotlin:kotlin-stdlib:1.9.0", "")),
        );
        repo.publish("org.jetbrains.kotlin:kotlin-stdlib:1.9.0", "");
        repo.publish("org.jetbrains.kotlin:kotlin-stdlib:2.4.20", "");

        let m = repo.manifest("[kotlin]\nversion='2.4.20'\n[dependencies]\n'g:lib'='1.0'");
        let r = resolve(&m, &repo.fetcher(), 4).unwrap();
        let stdlib = r
            .get(&Ga::new("org.jetbrains.kotlin", "kotlin-stdlib"))
            .unwrap();
        assert_eq!(stdlib.coord.version, "2.4.20");
        assert!(stdlib.direct);
        assert_eq!(stdlib.classpath, Classpath::Compile);
        assert_eq!(
            r.roots,
            vec![
                Ga::new("g", "lib"),
                Ga::new("org.jetbrains.kotlin", "kotlin-stdlib")
            ],
            "declared first, implied last"
        );
    }

    #[test]
    fn a_tool_graph_is_resolved_apart_from_the_projects() {
        let repo = Repo::new("tool");
        repo.publish("g:compiler:1.0", &dep(&d("g:shared:1.0", "")));
        repo.publish("g:shared:1.0", "");
        repo.publish("g:shared:2.0", "");

        // The project pins shared 2.0; the compiler still gets the 1.0 it
        // asked for, since the two graphs never meet.
        let tool = resolve_tool(
            &[Coord::parse("g:compiler:1.0").unwrap()],
            &repo.fetcher(),
            4,
        )
        .unwrap();
        assert_eq!(names(&tool), vec!["g:compiler:1.0", "g:shared:1.0"]);
        assert_eq!(tool.roots, vec![Ga::new("g", "compiler")]);
        let project = resolve(
            &repo.manifest("[dependencies]\n'g:shared'='2.0'"),
            &repo.fetcher(),
            4,
        )
        .unwrap();
        assert_eq!(names(&project), vec!["g:shared:2.0"]);
    }

    #[test]
    fn classpaths_join_to_every_place_a_package_is_needed() {
        use Classpath::{Compile, Provided, Runtime, Test};
        let all = [Compile, Provided, Runtime, Test];
        for a in all {
            assert_eq!(a.join(a), a);
            assert_eq!(a.join(Test), a, "tests see everything anyway");
            assert_eq!(a.join(Compile), Compile);
            assert_eq!(Classpath::parse(a.as_str()), a);
            for b in all {
                assert_eq!(a.join(b), b.join(a));
            }
        }
        assert_eq!(Provided.join(Runtime), Compile);
        assert!(Compile.compiles() && Provided.compiles());
        assert!(!Runtime.compiles() && !Test.compiles());
        assert!(Compile.runs() && Runtime.runs());
        assert!(!Provided.runs() && !Test.runs());
    }

    #[test]
    fn a_runtime_only_dependency_is_walked_but_not_compiled_against() {
        let repo = Repo::new("runtime-only");
        repo.publish("g:driver:1.0", &dep(&d("g:driver-core:1.0", "")));
        repo.publish("g:driver-core:1.0", "");
        repo.publish("g:api:1.0", &dep(&d("g:driver-core:1.0", "")));

        let m = repo.manifest("[dependencies]\n'g:driver'={version='1.0', runtime-only=true}");
        let mut r = resolve(&m, &repo.fetcher(), 4).unwrap();
        fetch_jars(&mut r, &repo.fetcher(), 4).unwrap();
        let of = |r: &Resolution, a: &str| r.get(&Ga::new("g", a)).unwrap().classpath;
        assert_eq!(of(&r, "driver"), Classpath::Runtime);
        assert_eq!(of(&r, "driver-core"), Classpath::Runtime);
        assert!(r.classpath(Classpath::Compile).is_empty());
        assert_eq!(r.runtime_classpath().len(), 2);
        assert_eq!(r.runtime_packages().count(), 2);
        assert_eq!(r.classpath(Classpath::Test).len(), 2);

        // Compiled against through another path, the shared jar is needed on
        // both classpaths.
        let m = repo.manifest(
            "[dependencies]\n'g:driver'={version='1.0', runtime-only=true}\n\
             'g:api'={version='1.0', compile-only=true}",
        );
        let r = resolve(&m, &repo.fetcher(), 4).unwrap();
        assert_eq!(of(&r, "driver"), Classpath::Runtime);
        assert_eq!(of(&r, "api"), Classpath::Provided);
        assert_eq!(of(&r, "driver-core"), Classpath::Compile);
    }

    #[test]
    fn resolution_records_checksums_for_the_lockfile() {
        let repo = Repo::new("checksums");
        repo.publish("g:a:1.0", "");
        let m = repo.manifest("[dependencies]\n'g:a'='1.0'");
        let mut r = resolve(&m, &repo.fetcher(), 4).unwrap();
        fetch_jars(&mut r, &repo.fetcher(), 4).unwrap();
        let pkg = r.get(&Ga::new("g", "a")).unwrap();
        assert_eq!(
            pkg.checksum.as_deref(),
            Some(format!("sha1:{}", repo::sha1_hex(b"jar of g:a:1.0")).as_str())
        );
    }
}
