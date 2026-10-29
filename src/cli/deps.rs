//! The commands about the dependency graph: `tree`, `classpath`, `update`,
//! `outdated`, `licenses`, `audit`, `verify`, `metadata` and `fetch`.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::path::Path;

use rayon::prelude::*;

use super::jdk::warm_compilers;
use super::session::Session;
use crate::audit::{self, Osv};
use crate::error::{JrsError, Result, exit};
use crate::lockfile::Lockfile;
use crate::resolve::cache::Cache;
use crate::resolve::coord::{Coord, Ga};
use crate::resolve::{self, Classpath, Resolution, metadata};
use crate::toolchain::Toolchain;
use crate::ui::{Style, TreeNode};
use crate::{model, obfuscate, test as junit};

impl Session<'_> {
    pub(super) fn tree_command(
        &self,
        depth: Option<usize>,
        why: Option<&str>,
        tool: Option<&str>,
        task: Option<&str>,
    ) -> Result<i32> {
        let resolution = self.dependencies(false)?;
        let trees = match (tool, task, why) {
            (Some(name), _, _) => vec![self.tool_tree(name, depth)?],
            (None, Some(name), _) => vec![self.task_tree(name, depth)?],
            (None, None, Some(target)) => self.why(&resolution, target)?,
            (None, None, None) => vec![self.tree(&resolution, depth)],
        };
        self.ui.suspend();
        for tree in &trees {
            self.ui.tree(tree);
        }
        Ok(exit::SUCCESS)
    }

    pub(super) fn classpath_command(&self, test: bool, runtime: bool) -> Result<i32> {
        let resolution = self.dependencies(false)?;
        let project = self.project();
        let mut entries = Vec::new();
        if test {
            entries.push(project.test_classes_dir());
        }
        entries.push(project.classes_dir());
        entries.extend(self.upstream_dirs(if test {
            |_| true
        } else if runtime {
            Classpath::runs
        } else {
            Classpath::compiles
        }));
        entries.extend(if test {
            resolution.classpath(Classpath::Test)
        } else if runtime {
            resolution.runtime_classpath()
        } else {
            resolution.classpath(Classpath::Compile)
        });
        // The classpath is real output, for `java -cp "$(jrs classpath)"`.
        self.ui.suspend();
        self.ui.println_out(Toolchain::classpath(&entries));
        Ok(exit::SUCCESS)
    }

    pub(super) fn update_command(&self) -> Result<i32> {
        let resolution = self.dependencies(true)?;
        self.ui.phase(
            "Updated",
            format!(
                "{} in {} packages",
                self.manifest.lock_path().display(),
                resolution.packages.len()
            ),
        );
        Ok(exit::SUCCESS)
    }

    /// What `jrs outdated` checks, as (row name, group, artifact, current
    /// version): the declared dependencies, `[managed]`, then each language's
    /// compiler. A local jar has no repository to ask, and a dependency that
    /// takes its version from `[managed]` is outdated through the entry there,
    /// so both are left out. A BOM is outdated like any dependency.
    fn outdated_checks(&self) -> Vec<(String, String, String, String)> {
        let mut checks: Vec<(String, String, String, String)> = self
            .manifest
            .dependencies
            .iter()
            .map(|d| (d.key(), d))
            .chain(
                self.manifest
                    .dev_dependencies
                    .iter()
                    .map(|d| (format!("{} (dev)", d.key()), d)),
            )
            .filter(|(_, d)| !d.is_local() && !d.is_managed())
            .map(|(name, d)| (name, d.group.clone(), d.artifact.clone(), d.version.clone()))
            .collect();
        for m in &self.manifest.managed {
            let kind = if m.bom { "bom" } else { "managed" };
            checks.push((
                format!("{} ({kind})", m.key()),
                m.group.clone(),
                m.artifact.clone(),
                m.version.clone(),
            ));
        }
        for config in &self.manifest.languages {
            if let Some(compiler) = config.language.compiler(&config.version) {
                checks.push((
                    format!("{}.version", config.language.key()),
                    compiler.coord.group,
                    compiler.coord.artifact,
                    config.version.clone(),
                ));
            }
        }
        checks
    }

    pub(super) fn outdated_command(&self) -> Result<i32> {
        let declared = self.outdated_checks();
        if declared.is_empty() {
            self.ui.phase("Finished", "no dependencies are declared");
            return Ok(exit::SUCCESS);
        }
        let fetcher = self.fetcher()?;
        self.ui.phase(
            "Checking",
            format!("{} dependencies for newer releases", declared.len()),
        );
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(self.jobs.max(1))
            .build()
            .map_err(|e| JrsError::resolve(format!("could not start a worker pool: {e}")))?;
        let scope = self
            .ui
            .spinner("Checking", format!("{} dependencies", declared.len()));
        let answers: Vec<Result<metadata::Metadata>> = pool.install(|| {
            declared
                .par_iter()
                .map(|(_, group, artifact, _)| fetcher.metadata(group, artifact))
                .collect()
        });
        scope.finish();

        let mut rows = Vec::new();
        for ((name, _, _, version), answer) in declared.iter().zip(answers) {
            match answer {
                Ok(m) => {
                    if let Some(newer) = metadata::newest(&m.versions, version) {
                        rows.push([name.clone(), version.clone(), newer]);
                    }
                }
                // One unreachable artifact should not hide the rest.
                Err(e) => self
                    .ui
                    .warn(e.to_string().lines().next().unwrap_or_default()),
            }
        }
        for warning in fetcher.take_warnings() {
            self.ui.warn(warning);
        }

        if rows.is_empty() {
            self.ui
                .phase("Finished", "every dependency is on its newest release");
            return Ok(exit::SUCCESS);
        }
        let header = [
            "dependency".to_string(),
            "current".to_string(),
            "newest".to_string(),
        ];
        let widths: Vec<usize> = (0..3)
            .map(|i| {
                rows.iter()
                    .chain(std::iter::once(&header))
                    .map(|r| r[i].chars().count())
                    .max()
                    .unwrap_or(0)
            })
            .collect();
        self.ui.suspend();
        for row in std::iter::once(&header).chain(&rows) {
            self.ui.println_out(format!(
                "{:<w0$}  {:<w1$}  {}",
                row[0],
                row[1],
                row[2],
                w0 = widths[0],
                w1 = widths[1]
            ));
        }
        self.ui.phase(
            "Finished",
            format!(
                "{} of {} dependencies have newer releases",
                rows.len(),
                declared.len()
            ),
        );
        Ok(exit::SUCCESS)
    }

    pub(super) fn licenses_command(&self, runtime: bool) -> Result<i32> {
        let resolution = self.dependencies(false)?;
        let packages: Vec<&resolve::ResolvedPackage> = resolution
            .packages
            .iter()
            .filter(|p| p.packaging != "pom" && (!runtime || p.classpath.runs()))
            .collect();
        let locals: Vec<&resolve::LocalJar> = resolution
            .local
            .iter()
            .filter(|l| !runtime || l.classpath.runs())
            .collect();
        if packages.is_empty() && locals.is_empty() {
            self.ui.phase("Finished", "there are no dependencies");
            return Ok(exit::SUCCESS);
        }
        let licenses = self.licenses_of(&packages)?;

        let mut rows = Vec::new();
        let mut unlicensed = Vec::new();
        for (p, licenses) in packages.iter().zip(&licenses) {
            let name = format!("{}{}", p.coord, classpath_suffix(p.classpath));
            if licenses.is_empty() {
                unlicensed.push(p.coord.to_string());
                rows.push([name, "(none declared)".to_string()]);
            } else {
                let labels: Vec<String> = licenses
                    .iter()
                    .map(resolve::license::License::label)
                    .collect();
                rows.push([name, labels.join(", ")]);
            }
        }
        for l in &locals {
            unlicensed.push(l.name.clone());
            rows.push([
                format!("{}{}", l.name, classpath_suffix(l.classpath)),
                "(a local jar: no POM to declare one)".to_string(),
            ]);
        }
        let header = ["dependency".to_string(), "licence".to_string()];
        let width = rows
            .iter()
            .chain(std::iter::once(&header))
            .map(|r| r[0].chars().count())
            .max()
            .unwrap_or(0);
        self.ui.suspend();
        for row in std::iter::once(&header).chain(&rows) {
            self.ui.println_out(
                format!("{:<width$}  {}", row[0], row[1])
                    .trim_end()
                    .to_string(),
            );
        }
        for name in &unlicensed {
            self.ui.warn(format!("`{name}` declares no licence"));
        }
        let total = rows.len();
        self.ui.phase(
            "Finished",
            match unlicensed.len() {
                0 => format!("{total} dependencies, each with a licence"),
                n => format!("{total} dependencies, {n} without a licence"),
            },
        );
        Ok(exit::SUCCESS)
    }

    pub(super) fn audit_command(&self, runtime: bool, ignore: &[String]) -> Result<i32> {
        let resolution = self.dependencies(false)?;
        for l in &resolution.local {
            if !runtime || l.classpath.runs() {
                self.ui.warn(format!(
                    "the local jar `{}` has no coordinate to audit",
                    l.name
                ));
            }
        }
        // A classifier's jar shares its version's advisories: ask once.
        let mut seen = HashSet::new();
        let packages: Vec<&resolve::ResolvedPackage> = resolution
            .packages
            .iter()
            .filter(|p| p.packaging != "pom" && (!runtime || p.classpath.runs()))
            .filter(|p| seen.insert((&p.coord.group, &p.coord.artifact, &p.coord.version)))
            .collect();
        if packages.is_empty() {
            self.ui
                .phase("Finished", "there are no dependencies to audit");
            return Ok(exit::SUCCESS);
        }
        let coords: Vec<Coord> = packages
            .iter()
            .map(|p| Coord::new(&p.coord.group, &p.coord.artifact, &p.coord.version))
            .collect();
        let found = self.advisories_of(&coords)?;

        let mut rows = Vec::new();
        let mut affected = 0;
        let mut ignored = 0;
        let mut used = vec![false; ignore.len()];
        for ((p, c), advisories) in packages.iter().zip(&coords).zip(found) {
            let mut hit = false;
            for advisory in advisories {
                if let Some(i) = ignore.iter().position(|name| advisory.is_named(name)) {
                    used[i] = true;
                    ignored += 1;
                    self.ui
                        .verbose(format!("{} in {c}: ignored", advisory.label()));
                    continue;
                }
                hit = true;
                rows.push([
                    format!("{c}{}", classpath_suffix(p.classpath)),
                    advisory.label(),
                    advisory.severity.clone().unwrap_or_else(|| "-".into()),
                    advisory.fixed_after(&c.version).unwrap_or("-").to_string(),
                    advisory.summary.clone(),
                ]);
            }
            affected += usize::from(hit);
        }
        for (name, used) in ignore.iter().zip(used) {
            if !used {
                self.ui.warn(format!(
                    "`--ignore {name}` matches no advisory of any dependency"
                ));
            }
        }
        let ignored = match ignored {
            0 => String::new(),
            1 => ", 1 advisory ignored".to_string(),
            n => format!(", {n} advisories ignored"),
        };
        if rows.is_empty() {
            self.ui.phase(
                "Finished",
                format!(
                    "{} dependencies, no known vulnerabilities{ignored}",
                    coords.len()
                ),
            );
            return Ok(exit::SUCCESS);
        }

        let header =
            ["dependency", "advisory", "severity", "fixed in", "summary"].map(String::from);
        self.ui.suspend();
        for line in columns(std::iter::once(&header).chain(&rows)) {
            self.ui.println_out(line);
        }
        let advisories = if rows.len() == 1 {
            "1 advisory affects".to_string()
        } else {
            format!("{} advisories affect", rows.len())
        };
        Err(JrsError::build(format!(
            "{advisories} {affected} of {} dependencies{ignored}\n\n\
             upgrade to a fixed version (`jrs tree --why <artifact>` shows what \
             brings one in), or pass `--ignore <ID>` for an advisory that does not apply",
            coords.len()
        )))
    }

    /// The advisories the OSV API (SPEC §8.12) knows for each of `coords`.
    fn advisories_of(&self, coords: &[Coord]) -> Result<Vec<Vec<audit::Advisory>>> {
        let url = self
            .config
            .audit_url
            .as_deref()
            .unwrap_or(audit::DEFAULT_URL);
        let osv = Osv::new(url, self.config.proxy.as_ref())?;
        self.ui.phase(
            "Auditing",
            format!("{} dependencies against {url}", coords.len()),
        );
        let scope = self
            .ui
            .spinner("Auditing", format!("{} dependencies", coords.len()));
        let found = osv.query(coords).and_then(|ids| {
            let wanted: Vec<(String, String)> = coords
                .iter()
                .zip(&ids)
                .flat_map(|(c, ids)| {
                    ids.iter()
                        .map(move |id| (id.clone(), format!("{}:{}", c.group, c.artifact)))
                })
                .collect();
            let mut advisories = osv.advisories(&wanted, self.jobs)?.into_iter();
            Ok(ids
                .iter()
                .map(|ids| advisories.by_ref().take(ids.len()).collect())
                .collect())
        });
        scope.finish();
        found
    }

    /// The licences each of `packages` declares, by its POM or the nearest
    /// parent's, fetched `jobs` at a time.
    pub(super) fn licenses_of(
        &self,
        packages: &[&resolve::ResolvedPackage],
    ) -> Result<Vec<Vec<resolve::license::License>>> {
        let fetcher = self.fetcher()?;
        let coords: Vec<Coord> = packages.iter().map(|p| p.coord.clone()).collect();
        self.ui.phase(
            "Reading",
            match coords.len() {
                1 => "the licence of 1 dependency".to_string(),
                n => format!("the licences of {n} dependencies"),
            },
        );
        let scope = self
            .ui
            .spinner("Reading", format!("{} licences", coords.len()));
        let answers = resolve::license::of_each(&fetcher, &coords, self.jobs);
        scope.finish();
        for warning in fetcher.take_warnings() {
            self.ui.warn(warning);
        }
        answers.into_iter().collect()
    }

    /// `jrs verify` on a jar `jrs.lock` holds no checksum for: a warning,
    /// unless its classifier names the host's platform, which is never pinned.
    fn unpinned(&self, coord: &Coord, lock_path: &Path) {
        if coord
            .classifier
            .as_deref()
            .is_some_and(resolve::coord::is_platform_classifier)
        {
            self.ui.verbose(format!(
                "{coord} names the host's platform, whose jar is never pinned"
            ));
        } else {
            self.ui.warn(format!(
                "`{coord}` has no checksum in {}; it was not verified",
                lock_path.display()
            ));
        }
    }

    pub(super) fn verify_command(&self) -> Result<i32> {
        let lock_path = self.manifest.lock_path();
        let lock = Lockfile::load(&lock_path)?.ok_or_else(|| {
            JrsError::usage(format!(
                "{} does not exist\n\nrun `jrs build` or `jrs update` to resolve and pin \
                 the dependencies first",
                lock_path.display()
            ))
        })?;
        if !lock.matches(&self.manifest) {
            self.ui.warn(format!(
                "{} is out of date with {}; verifying what it records \
                 (run `jrs update` to re-resolve)",
                lock_path.display(),
                self.manifest.path.display()
            ));
        }

        // The compilers' pins are checked with the project's.
        let resolution = Resolution {
            packages: lock.all_packages().cloned().collect(),
            ..lock.to_resolution()
        };
        let count = resolution
            .packages
            .iter()
            .filter(|p| p.packaging != "pom")
            .count()
            + lock.local.len();
        self.ui
            .phase("Verifying", format!("{count} locked artifacts"));
        let scope = self
            .ui
            .spinner("Verifying", format!("{count} locked artifacts"));
        let checked = resolve::verify_cached(&resolution, &Cache::discover()?, self.jobs);
        scope.finish();

        let mut verified = 0;
        let mut not_cached = 0;
        let mut mismatches = Vec::new();
        for c in checked? {
            match c.integrity {
                resolve::Integrity::Verified => verified += 1,
                resolve::Integrity::NotCached => {
                    not_cached += 1;
                    self.ui.verbose(format!("{} is not cached", c.coord));
                }
                resolve::Integrity::Snapshot => {
                    self.ui
                        .verbose(format!("{} is a snapshot, which is never pinned", c.coord));
                }
                resolve::Integrity::Unpinned => self.unpinned(&c.coord, &lock_path),
                resolve::Integrity::Mismatch { expected, actual } => mismatches.push(format!(
                    "  {}\n    locked {expected}\n    cached {actual}",
                    c.path.display()
                )),
            }
        }
        // Local jars are checked where the project keeps them.
        for local in &lock.local {
            let path = self.manifest.root.join(&local.path);
            match resolve::local_integrity(&path, local.checksum.as_deref())? {
                resolve::Integrity::Verified => verified += 1,
                resolve::Integrity::Mismatch { expected, actual } => mismatches.push(format!(
                    "  {} (local jar `{}`)\n    locked {expected}\n    found  {actual}",
                    local.path, local.name
                )),
                resolve::Integrity::NotCached => mismatches.push(format!(
                    "  {} (local jar `{}`)\n    missing",
                    local.path, local.name
                )),
                resolve::Integrity::Unpinned | resolve::Integrity::Snapshot => {
                    self.ui.warn(format!(
                        "the local jar `{}` has no checksum in {}; it was not verified",
                        local.name,
                        lock_path.display()
                    ));
                }
            }
        }

        if !mismatches.is_empty() {
            return Err(JrsError::build(format!(
                "{} cached artifacts do not match {}:\n\n{}\n\n\
                 delete them and build again to re-download them; if it is jrs.lock \
                 that changed, run `jrs update`",
                mismatches.len(),
                lock_path.display(),
                mismatches.join("\n")
            )));
        }
        let mut message = format!("{verified} artifacts against {}", lock_path.display());
        if not_cached > 0 {
            let _ = write!(
                message,
                " ({not_cached} not cached; checked when they are downloaded)"
            );
        }
        self.ui.phase("Verified", message);
        Ok(exit::SUCCESS)
    }

    /// `jrs metadata`: the project model as versioned JSON on stdout
    /// (`model.rs`). It resolves, unless `--no-deps`, and never compiles.
    pub(super) fn metadata_command(&self, no_deps: bool) -> Result<i32> {
        let resolution = if no_deps {
            None
        } else {
            Some(self.dependencies(false)?)
        };
        // Without a JDK the layout and the classpaths are still worth having,
        // so a missing one leaves `jdk` null rather than failing the command.
        let toolchain = match self.toolchain() {
            Ok(toolchain) => Some(toolchain),
            Err(e) => {
                self.ui.warn(format!(
                    "{}; the model has no `jdk`",
                    e.to_string().lines().next().unwrap_or_default()
                ));
                None
            }
        };
        let cache = Cache::discover().ok();
        let document = model::metadata(&model::Inputs {
            manifest: &self.manifest,
            toolchain: toolchain.as_ref(),
            resolution: resolution.as_ref(),
            cache: cache.as_ref(),
            workspace: None,
        })?;
        // The model is real output, for `jrs metadata | jq`.
        self.ui.suspend();
        for line in document.render().lines() {
            self.ui.println_out(line);
        }
        Ok(exit::SUCCESS)
    }

    /// `jrs fetch`: resolve and download what a build and a test run need —
    /// the dependencies, the compilers, the test launcher — without building;
    /// with `--sources`, every dependency's `-sources.jar` as well.
    pub(super) fn fetch_command(&self, sources: bool) -> Result<i32> {
        // The compiler caches too, which a build would otherwise make, both
        // beside the downloads.
        let toolchain = self.installed_toolchain();
        if let Some(toolchain) = &toolchain {
            self.train_javac(toolchain, true);
        }
        let resolution = self.dependencies(false)?;
        // The launcher `jrs test` would run, when there is a JUnit to derive
        // it from, so that `jrs test --offline` works after a fetch.
        if let Ok(launcher) = junit::launcher_coordinate(&self.manifest, &resolution) {
            self.fetch_internal(&[(launcher, "test launcher")])?;
        }
        // Each task's own tools too, which a build otherwise downloads only
        // when the task first runs, so that its hooks work under `--offline`.
        for def in self
            .manifest
            .tasks
            .iter()
            .filter(|t| !t.dependencies.is_empty())
        {
            self.task_classpath(def)?;
        }
        let jars = resolution
            .packages
            .iter()
            .filter(|p| p.jar.is_some())
            .count();
        let mut fetched = format!("{jars} dependencies");
        if sources {
            let (found, wanted) = self.fetch_sources(&resolution)?;
            let _ = write!(fetched, " and {found} of {wanted} sources jars");
        }
        if let Some(toolchain) = &toolchain {
            warm_compilers(self.ui, toolchain);
        }
        self.ui.phase(
            "Finished",
            format!("fetched {fetched} in {}", self.elapsed()),
        );
        Ok(exit::SUCCESS)
    }

    /// Every dependency's `-sources.jar`, into the cache, under the usual
    /// `Downloading` line and bars. One that is not published is a warning,
    /// not an error: many libraries publish none, and no build needs one.
    /// Returns how many are cached now, out of how many were wanted.
    fn fetch_sources(&self, resolution: &Resolution) -> Result<(usize, usize)> {
        let fetcher = self.fetcher()?;
        let wanted = resolve::sources_coords(resolution);
        let missing = wanted
            .iter()
            .filter(|c| !fetcher.cache().contains(c, "jar"))
            .count();
        let results = if missing > 0 && !self.offline {
            self.ui
                .phase("Downloading", format!("{missing} sources jars"));
            let scope = self.ui.downloads(missing);
            let results = resolve::fetch_sources(&wanted, &fetcher, self.jobs);
            scope.finish();
            results?
        } else {
            resolve::fetch_sources(&wanted, &fetcher, self.jobs)?
        };

        let mut found = 0;
        let mut unpublished = Vec::new();
        let mut not_cached = Vec::new();
        for (coord, result) in results {
            let Err(e) = result else {
                found += 1;
                continue;
            };
            let artifact = coord.pom_coord().to_string();
            let reason = e.to_string();
            let reason = reason.lines().next().unwrap_or_default();
            if self.offline {
                not_cached.push(artifact);
            } else if reason.starts_with("could not find") {
                // What the fetcher says when no repository has the file.
                unpublished.push(artifact);
            } else {
                self.ui.warn(format!(
                    "could not download the sources jar of `{artifact}`: {reason}"
                ));
            }
        }
        for warning in fetcher.take_warnings() {
            self.ui.warn(warning);
        }
        if !unpublished.is_empty() {
            let (noun, verb) = if unpublished.len() == 1 {
                ("dependency", "publishes")
            } else {
                ("dependencies", "publish")
            };
            self.ui.warn(format!(
                "{} {noun} {verb} no sources jar: {}",
                unpublished.len(),
                unpublished.join(", ")
            ));
        }
        if !not_cached.is_empty() {
            self.ui.warn(format!(
                "{} sources jars are not in the cache, and --offline was given: {}",
                not_cached.len(),
                not_cached.join(", ")
            ));
        }
        Ok((found, wanted.len()))
    }

    /// `jrs tree --task <name>`: a task's own dependency graph.
    fn task_tree(&self, name: &str, limit: Option<usize>) -> Result<TreeNode> {
        let task_tools = self.task_tools.get().cloned().unwrap_or_default();
        let Some((_, graph)) = task_tools.iter().find(|(n, _)| n == name) else {
            let known: Vec<String> = task_tools.iter().map(|(n, _)| format!("`{n}`")).collect();
            let hint = if known.is_empty() {
                "none of this project's tasks has a [tasks.<name>.dependencies] table".to_string()
            } else {
                format!("the tasks with dependencies are {}", known.join(", "))
            };
            return Err(JrsError::usage(format!(
                "there is no task `{name}` with dependencies of its own\n\n{hint}"
            )));
        };
        let mut root = TreeNode::styled(format!("tasks.{name}"), Style::Bold);
        if limit == Some(0) {
            return Ok(root);
        }
        let mut seen = Vec::new();
        for ga in &graph.roots {
            root.children
                .push(Self::tree_node(graph, ga, &mut seen, 0, limit));
        }
        Ok(root)
    }

    /// `jrs tree --tool <name>`: a compiler's own graph, or the obfuscator's.
    pub(super) fn tool_tree(&self, name: &str, limit: Option<usize>) -> Result<TreeNode> {
        if name == obfuscate::TOOL_NAME {
            let graph = self.obfuscator.get().cloned().flatten().ok_or_else(|| {
                JrsError::usage(
                    "this project does not obfuscate: add an [obfuscate] table to turn it on"
                        .to_string(),
                )
            })?;
            let version = self
                .manifest
                .obfuscate
                .as_ref()
                .map_or("", |c| c.version.as_str());
            let mut root = TreeNode::styled(format!("{name} (ProGuard {version})"), Style::Bold);
            if limit == Some(0) {
                return Ok(root);
            }
            let mut seen = Vec::new();
            for ga in &graph.roots {
                root.children
                    .push(Self::tree_node(&graph, ga, &mut seen, 0, limit));
            }
            return Ok(root);
        }
        let tools = self.tools.get().cloned().unwrap_or_default();
        let Some(tool) = tools.iter().find(|t| t.language.tool_name() == name) else {
            let known: Vec<String> = tools
                .iter()
                .map(|t| format!("`{}`", t.language.tool_name()))
                .collect();
            let hint = if known.is_empty() {
                "this project has none: jrs resolves a compiler for each of [kotlin], [scala] \
                 and [groovy] it turns on"
                    .to_string()
            } else {
                format!("this project's are {}", known.join(", "))
            };
            return Err(JrsError::usage(format!(
                "there is no tool `{name}`\n\n{hint}"
            )));
        };
        let version = self
            .manifest
            .language(tool.language)
            .map_or("", |c| c.version.as_str());
        let mut root =
            TreeNode::styled(format!("{name} ({} {version})", tool.language), Style::Bold);
        if limit == Some(0) {
            return Ok(root);
        }
        let mut seen = Vec::new();
        for ga in &tool.resolution.roots {
            root.children
                .push(Self::tree_node(&tool.resolution, ga, &mut seen, 0, limit));
        }
        Ok(root)
    }

    /// The dependency graph as a drawable tree, `limit` levels deep.
    pub(super) fn tree(&self, resolution: &Resolution, limit: Option<usize>) -> TreeNode {
        let mut root = TreeNode::styled(
            format!("{} v{}", self.manifest.name, self.manifest.version),
            Style::Bold,
        );
        if limit == Some(0) {
            return root;
        }
        // A workspace member it depends on comes first: built here, so with
        // nothing below it but what its own dependencies put in the graph.
        for upstream in self.member.iter().flat_map(|m| &m.upstream) {
            if upstream.direct {
                let scope = match upstream.classpath {
                    Classpath::Compile => String::new(),
                    other => format!(", {}", other.as_str()),
                };
                root.children.push(TreeNode::styled(
                    format!("{} v{} (module{scope})", upstream.name, upstream.version),
                    Style::None,
                ));
            }
        }
        let mut seen = Vec::new();
        let implied = self.manifest.implied_dependencies();
        for ga in resolution.roots.iter().chain(&resolution.test_roots) {
            let mut node = Self::tree_node(resolution, ga, &mut seen, 0, limit);
            if let Some((_, language)) = implied.iter().find(|(d, _)| {
                d.group == ga.group && d.artifact == ga.artifact && ga.classifier.is_none()
            }) {
                let _ = write!(node.label, " (implied by [{}])", language.key());
            }
            root.children.push(node);
        }
        // A local jar has no graph: one line, declared, with nothing below it.
        for local in &resolution.local {
            root.children
                .push(TreeNode::styled(local_label(local), Style::None));
        }
        root
    }

    fn tree_node(
        resolution: &Resolution,
        ga: &Ga,
        seen: &mut Vec<Ga>,
        depth: usize,
        limit: Option<usize>,
    ) -> TreeNode {
        let Some(package) = resolution.get(ga) else {
            return TreeNode::styled(format!("{ga} (unresolved)"), Style::Red);
        };
        let (label, style) = package_label(package);

        if seen.contains(ga) {
            return TreeNode::styled(format!("{label} (*)"), Style::Dim);
        }
        if depth > 32 {
            return TreeNode::styled(format!("{label} (...)"), Style::Dim);
        }
        seen.push(ga.clone());

        let mut node = TreeNode::styled(label, style);
        if limit.is_none_or(|l| depth + 1 < l) {
            for child in &package.dependencies {
                node.children
                    .push(Self::tree_node(resolution, child, seen, depth + 1, limit));
            }
        }
        node
    }

    /// `jrs tree --why`: for each package matching `target`, the inverted
    /// graph — the target at the top, the packages that pull it in below it,
    /// down to the manifest that declared them.
    fn why(&self, resolution: &Resolution, target: &str) -> Result<Vec<TreeNode>> {
        let wanted = Ga::parse(target);
        let matches: Vec<Ga> = resolution
            .packages
            .iter()
            .filter(|p| match &wanted {
                Some(ga) => {
                    p.coord.group == ga.group
                        && p.coord.artifact == ga.artifact
                        && (ga.classifier.is_none() || p.coord.classifier == ga.classifier)
                }
                None => p.coord.artifact == target,
            })
            .map(resolve::ResolvedPackage::ga)
            .collect();
        let local = resolution.local_jar(target).map(|l| self.local_why(l));
        if matches.is_empty() && local.is_none() {
            return Err(JrsError::usage(format!(
                "`{target}` is not in the resolved graph\n\n\
                 run `jrs tree` to see what is"
            )));
        }

        let mut dependents: HashMap<Ga, Vec<Ga>> = HashMap::new();
        for p in &resolution.packages {
            for child in &p.dependencies {
                dependents.entry(child.clone()).or_default().push(p.ga());
            }
        }
        for parents in dependents.values_mut() {
            parents.sort();
            parents.dedup();
        }
        Ok(matches
            .iter()
            .map(|ga| self.why_node(resolution, &dependents, ga, &mut vec![ga.clone()]))
            .chain(local)
            .collect())
    }

    /// `jrs tree --why <name>` for a local jar: nothing pulls it in but the
    /// manifest.
    fn local_why(&self, local: &resolve::LocalJar) -> TreeNode {
        let table = if local.classpath == Classpath::Test {
            "dev-dependencies"
        } else {
            "dependencies"
        };
        let mut node = TreeNode::styled(local_label(local), Style::None);
        node.children.push(TreeNode::styled(
            format!(
                "{} v{} [{table}]",
                self.manifest.name, self.manifest.version
            ),
            Style::Bold,
        ));
        node
    }

    fn why_node(
        &self,
        resolution: &Resolution,
        dependents: &HashMap<Ga, Vec<Ga>>,
        ga: &Ga,
        path: &mut Vec<Ga>,
    ) -> TreeNode {
        let mut node = match resolution.get(ga) {
            Some(package) => {
                let (label, style) = package_label(package);
                TreeNode::styled(label, style)
            }
            None => TreeNode::styled(format!("{ga} (unresolved)"), Style::Red),
        };
        let project = format!("{} v{}", self.manifest.name, self.manifest.version);
        if resolution.roots.contains(ga) {
            node.children.push(TreeNode::styled(
                format!("{project} [dependencies]"),
                Style::Bold,
            ));
        }
        if resolution.test_roots.contains(ga) {
            node.children.push(TreeNode::styled(
                format!("{project} [dev-dependencies]"),
                Style::Bold,
            ));
        }
        if path.len() > 32 {
            return node;
        }
        for parent in dependents.get(ga).into_iter().flatten() {
            if path.contains(parent) {
                node.children
                    .push(TreeNode::styled(format!("{parent} (cycle)"), Style::Dim));
                continue;
            }
            path.push(parent.clone());
            node.children
                .push(self.why_node(resolution, dependents, parent, path));
            path.pop();
        }
        node
    }
}

/// A package as `jrs tree` draws it. Mediated versions are coloured so the
/// nearest-wins decision is visible at a glance (SPEC §5.3.6), and a version
/// `[managed]` decided says so.
fn package_label(package: &resolve::ResolvedPackage) -> (String, Style) {
    let mut label = package.coord.to_string();
    label.push_str(classpath_suffix(package.classpath));
    if package.managed {
        label.push_str(" (managed)");
    }
    let style = if package.mediated {
        Style::Yellow
    } else {
        Style::None
    };
    (label, style)
}

/// What `jrs tree` adds after a package that is not on every classpath.
fn classpath_suffix(classpath: Classpath) -> &'static str {
    match classpath {
        Classpath::Test => " (test)",
        Classpath::Provided => " (compile-only)",
        Classpath::Runtime => " (runtime-only)",
        Classpath::Compile => "",
    }
}

/// Rows as left-aligned columns two spaces apart, the last one unpadded.
fn columns<'a, const N: usize>(rows: impl Iterator<Item = &'a [String; N]> + Clone) -> Vec<String> {
    let widths: Vec<usize> = (0..N)
        .map(|i| {
            rows.clone()
                .map(|r| r[i].chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect();
    rows.map(|row| {
        let mut line = String::new();
        for (i, cell) in row.iter().enumerate() {
            if i + 1 < N {
                let _ = write!(line, "{cell:<w$}  ", w = widths[i]);
            } else {
                line.push_str(cell);
            }
        }
        line.trim_end().to_string()
    })
    .collect()
}

/// A local jar as `jrs tree` draws it: its name, then the path it is at.
fn local_label(local: &resolve::LocalJar) -> String {
    format!(
        "{} = {}{}",
        local.name,
        local.path,
        classpath_suffix(local.classpath)
    )
}
