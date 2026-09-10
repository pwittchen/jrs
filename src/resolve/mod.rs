//! Dependency resolution: the graph walk, conflict mediation, and the classpath.
//!
//! The algorithm is SPEC §8.2, breadth-first by level so that each level's POM
//! fetches batch together, with nearest-wins mediation on top. Version ranges are
//! rejected rather than guessed at.

pub mod cache;
pub mod coord;
pub mod pom;
pub mod repo;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
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
/// `Compile` dominates: a package reached both ways is a compile dependency that
/// tests also happen to see.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Classpath {
    Compile,
    Test,
}

impl Classpath {
    pub fn as_str(self) -> &'static str {
        match self {
            Classpath::Compile => "compile",
            Classpath::Test => "test",
        }
    }

    pub fn parse(s: &str) -> Classpath {
        match s {
            "test" => Classpath::Test,
            _ => Classpath::Compile,
        }
    }
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
}

impl ResolvedPackage {
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
}

impl Resolution {
    pub fn get(&self, ga: &Ga) -> Option<&ResolvedPackage> {
        self.packages.iter().find(|p| p.ga() == *ga)
    }

    /// Jars for `javac -cp` and `java -cp`, in a stable order: direct
    /// dependencies first, then transitives, each sorted by coordinate
    /// (SPEC §8.2 step 7).
    pub fn classpath(&self, which: Classpath) -> Vec<PathBuf> {
        let mut direct: Vec<&ResolvedPackage> = Vec::new();
        let mut transitive: Vec<&ResolvedPackage> = Vec::new();
        for p in &self.packages {
            if which == Classpath::Compile && p.classpath == Classpath::Test {
                continue;
            }
            if p.jar.is_none() {
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
        direct
            .into_iter()
            .chain(transitive)
            .filter_map(|p| p.jar.clone())
            .collect()
    }

    /// Packages whose jars belong inside a fat jar, or on the `Class-Path` of a
    /// thin one: everything on the compile classpath.
    pub fn runtime_packages(&self) -> impl Iterator<Item = &ResolvedPackage> {
        self.packages
            .iter()
            .filter(|p| p.classpath == Classpath::Compile && p.jar.is_some())
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
pub fn resolve(manifest: &Manifest, fetcher: &Fetcher, jobs: usize) -> Result<Resolution> {
    for dep in manifest
        .dependencies
        .iter()
        .chain(&manifest.dev_dependencies)
    {
        if is_range(&dep.version) {
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

    let mut selected: HashMap<Ga, Selected> = HashMap::new();
    let mut edges: HashMap<Ga, Vec<Ga>> = HashMap::new();
    let mut packaging: HashMap<Ga, String> = HashMap::new();
    let mut level: Vec<Pending> = seed(manifest);

    let roots: Vec<Ga> = manifest
        .dependencies
        .iter()
        .map(|d| Ga::new(&d.group, &d.artifact))
        .collect();
    let test_roots: Vec<Ga> = manifest
        .dev_dependencies
        .iter()
        .map(|d| Ga::new(&d.group, &d.artifact))
        .collect();

    let mut depth = 1;
    while !level.is_empty() {
        // Mediate this level against what is already selected. A `ga` first seen
        // at a shallower depth keeps its version; within a level, the earlier
        // declaration wins.
        let mut admitted: Vec<&Pending> = Vec::new();
        for item in &level {
            let ga = item.coord.ga();
            match selected.get_mut(&ga) {
                Some(existing) => {
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
                    if item.classpath == Classpath::Compile {
                        existing.classpath = Classpath::Compile;
                    }
                    existing.direct |= item.depth == 1;
                }
                None => {
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
                .map(|(i, item)| ctx.effective(&item.coord).map(|e| (i, e)))
                .collect()
        });

        let mut next: Vec<Pending> = Vec::new();
        for result in effectives {
            let (i, eff) = result?;
            let parent = &admitted[i];
            let parent_ga = parent.coord.ga();
            packaging.insert(parent_ga.clone(), eff.packaging.clone());
            for dep in &eff.dependencies {
                let dep = eff.manage(dep);
                let Some(child) = admissible(&dep, parent, &eff, &ctx)? else {
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

    Ok(Resolution {
        packages,
        roots,
        test_roots,
        warnings: ctx.warnings.into_inner().unwrap(),
        downloaded: 0,
    })
}

fn seed(manifest: &Manifest) -> Vec<Pending> {
    let mut out = Vec::new();
    let mut push = |d: &Dependency, classpath: Classpath| {
        out.push(Pending {
            coord: Coord::new(&d.group, &d.artifact, &d.version),
            classpath,
            depth: 1,
            exclusions: Vec::new(),
            parent: None,
        });
    };
    for d in &manifest.dependencies {
        push(d, Classpath::Compile);
    }
    for d in &manifest.dev_dependencies {
        push(d, Classpath::Test);
    }
    out
}

/// Decide whether a child dependency is walked at all (SPEC §8.2 step 4).
fn admissible(
    dep: &PomDependency,
    parent: &Pending,
    eff: &Effective,
    ctx: &Context,
) -> Result<Option<Coord>> {
    if dep.optional || !dep.scope().is_transitive() {
        return Ok(None);
    }
    if dep.kind != "jar" || dep.classifier.is_some() {
        // Only plain jars land on a classpath; anything else is reported rather
        // than silently mis-resolved.
        return Ok(None);
    }
    let ga = dep.ga();
    if parent.exclusions.iter().any(|e| {
        (e.group == "*" || e.group == ga.group) && (e.artifact == "*" || e.artifact == ga.artifact)
    }) {
        return Ok(None);
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
    Ok(Some(Coord::new(&ga.group, &ga.artifact, version)))
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
pub fn fetch_jars(resolution: &mut Resolution, fetcher: &Fetcher, jobs: usize) -> Result<()> {
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(jobs.max(1))
        .build()
        .map_err(|e| JrsError::resolve(format!("could not start a worker pool: {e}")))?;

    let fetched: Vec<Result<FetchedJar>> = pool.install(|| {
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
                let (path, _origin) = fetcher.jar(&p.coord)?;
                // A checksum carried over from the lockfile is not recomputed:
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

    for result in fetched {
        let (i, path, checksum) = result?;
        resolution.packages[i].jar = path;
        resolution.packages[i].checksum = checksum;
    }
    resolution.downloaded = fetcher.downloaded();
    resolution.warnings.extend(fetcher.take_warnings());
    Ok(())
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

// ---- the download bars -----------------------------------------------------

/// Publishes transfer progress into the UI's live region.
pub struct UiReporter {
    ui: Ui,
    next_id: AtomicU64,
}

impl UiReporter {
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
                vec![manifest::Repository {
                    name: "fixture".into(),
                    url: repo::file_url(&self.repo_dir()),
                }],
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
