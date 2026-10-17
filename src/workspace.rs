//! Workspaces: several members built from one root, with one lockfile
//! (`specs/WORKSPACES.md`).
//!
//! A root `jrs.toml` with a `[workspace]` table lists its members by
//! directory. Each member is an ordinary manifest that inherits the root's
//! shared tables, its own keys winning (§3), and may depend on a sibling by
//! path (§4). Nothing here builds anything: this module turns the manifests
//! into a graph of members, the one manifest the whole workspace resolves as
//! (§5), and each member's share of that resolution. `cli.rs` drives the
//! builds.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt::Write as _;
use std::path::{Component, Path, PathBuf};

use crate::error::{IoResultExt, JrsError, Result};
use crate::manifest::{
    self, Dependency, Hook, JavaAgent, MANIFEST_FILE, Manifest, ModuleDependency, TaskRef,
};
use crate::resolve::coord::Ga;
use crate::resolve::{Classpath, LocalJar, Resolution, ResolvedPackage};

/// The tables a member inherits from the root, merged key by key.
const SHARED: &[&str] = &["java", "repositories", "managed"];
/// The language tables, whose keys a member inherits only when it turns the
/// language on itself: whether a member is Kotlin is its own business.
const SHARED_LANGUAGES: &[&str] = &["kotlin", "scala", "groovy"];
const WORKSPACE_KEYS: &[&str] = &["members", "parallel-tests"];

/// A loaded workspace: the root and every member's manifest.
#[derive(Debug)]
pub struct Workspace {
    /// The root directory, absolute.
    pub root: PathBuf,
    /// The root `jrs.toml`.
    pub path: PathBuf,
    /// The root's `project.name`, or its directory's name for a pure
    /// aggregator.
    pub name: String,
    /// In declaration order: the root first when it is a member too.
    pub members: Vec<Member>,
    /// Members in build order: each after every member it depends on, ties
    /// in declaration order.
    pub order: Vec<usize>,
    /// What the root's own tables raised, beside each member's.
    pub warnings: Vec<String>,
    /// `workspace.parallel-tests`: whether members' test JVMs may run at
    /// once, up to `--jobs` (`specs/WORKSPACES.md` §6.2). Off by default,
    /// since suites that share a port or a database would race.
    pub parallel_tests: bool,
}

/// One member: its manifest, as inherited, and the members it depends on.
#[derive(Debug)]
pub struct Member {
    /// Its directory relative to the root, `/`-separated; `.` for the root.
    pub dir: String,
    pub manifest: Manifest,
    /// The members its `path` dependencies name, each with the classpath it
    /// declares them on, in declaration order.
    pub modules: Vec<(usize, Classpath)>,
}

/// An upstream member as a dependent sees it: where its classes are, and the
/// classpath they go on.
#[derive(Debug, Clone)]
pub struct Upstream {
    pub name: String,
    pub version: String,
    /// Its manifest's directory, absolute.
    pub root: PathBuf,
    pub classes_dir: PathBuf,
    pub jar_path: PathBuf,
    pub classpath: Classpath,
    /// Whether the dependent names it itself, rather than through another
    /// member.
    pub direct: bool,
}

impl Workspace {
    /// The workspace the manifest at `path` belongs to: its own, when it is a
    /// root, or the nearest one above it that lists its directory as a
    /// member. With the second, the member's index comes back too: running
    /// from inside a member narrows a command to it (§6).
    ///
    /// # Errors
    ///
    /// As for [`Workspace::load`], for the manifest at `path` or a root
    /// above it that lists it; [`JrsError::Io`] when the manifest at `path`
    /// cannot be read. A manifest above that does not parse is passed over.
    pub fn find(path: &Path) -> Result<Option<(Workspace, Option<usize>)>> {
        let path = std::path::absolute(path).path(path)?;
        if let Some(table) = read_table(&path)?
            && table.contains_key("workspace")
        {
            return Ok(Some((Workspace::load(&path, &table)?, None)));
        }
        let Some(dir) = path.parent() else {
            return Ok(None);
        };
        let mut above = dir.parent();
        while let Some(candidate) = above {
            let root_manifest = candidate.join(MANIFEST_FILE);
            // A manifest above that cannot be read is no workspace of this
            // one's: it is some other project's business, not this build's.
            if let Ok(Some(table)) = read_table(&root_manifest)
                && let Some(members) = table.get("workspace").and_then(|w| w.get("members"))
            {
                let wanted = relative_dir(candidate, dir);
                let listed = members.as_array().into_iter().flatten().any(|m| {
                    m.as_str()
                        .and_then(|m| normalize(m).ok())
                        .is_some_and(|m| Some(m) == wanted)
                });
                if !listed {
                    // A workspace that does not list this directory is not
                    // its workspace; the manifest stands alone.
                    return Ok(None);
                }
                let workspace = Workspace::load(&root_manifest, &table)?;
                let index = workspace
                    .members
                    .iter()
                    .position(|m| Some(&m.dir) == wanted.as_ref());
                return Ok(Some((workspace, index)));
            }
            above = candidate.parent();
        }
        Ok(None)
    }

    /// Load the workspace whose root manifest is at `path` and reads as
    /// `table`.
    ///
    /// # Errors
    ///
    /// [`JrsError::Manifest`] when `[workspace]` is malformed, a member's
    /// directory is outside the root or holds no `jrs.toml`, a member has a
    /// `[workspace]` of its own, two members share a name, a `path`
    /// dependency names no member or names one by another name, or the
    /// members depend on each other in a cycle; as for
    /// [`Manifest::from_table`] for any member's manifest.
    #[allow(
        clippy::too_many_lines,
        reason = "`[workspace]`, the root member, then each listed member, each check \
                  with the message that names it"
    )]
    pub fn load(path: &Path, table: &toml::Table) -> Result<Workspace> {
        let path = std::path::absolute(path).path(path)?;
        let root = path
            .parent()
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        let mut warnings = Vec::new();
        let section = table
            .get("workspace")
            .and_then(toml::Value::as_table)
            .ok_or_else(|| JrsError::manifest("`workspace` must be a table"))?;
        for key in section.keys() {
            if !WORKSPACE_KEYS.contains(&key.as_str()) {
                warnings.push(format!("unknown key `workspace.{key}` (ignored)"));
            }
        }
        let listed = section
            .get("members")
            .ok_or_else(|| {
                JrsError::manifest(format!(
                    "{}: `[workspace]` needs `members`, the members' directories, as \
                     `members = [\"core\", \"app\"]`",
                    path.display()
                ))
            })?
            .as_array()
            .ok_or_else(|| JrsError::manifest("`workspace.members` must be an array"))?;
        let parallel_tests = match section.get("parallel-tests") {
            None => false,
            Some(value) => value.as_bool().ok_or_else(|| {
                JrsError::manifest("`workspace.parallel-tests` must be `true` or `false`")
            })?,
        };

        let shared: toml::Table = table
            .iter()
            .filter(|(k, _)| SHARED.contains(&k.as_str()) || SHARED_LANGUAGES.contains(&k.as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        let mut members = Vec::new();
        if table.contains_key("project") {
            members.push(Member {
                dir: ".".to_string(),
                manifest: Manifest::from_table(table, &path, &root)?,
                modules: Vec::new(),
            });
        } else {
            for key in table.keys() {
                let known = key == "workspace"
                    || SHARED.contains(&key.as_str())
                    || SHARED_LANGUAGES.contains(&key.as_str());
                if !known {
                    warnings.push(format!(
                        "`[{key}]` in a workspace root without [project] applies to nothing; \
                         only {} are shared with the members",
                        shared_list()
                    ));
                }
            }
        }
        for value in listed {
            let raw = value.as_str().ok_or_else(|| {
                JrsError::manifest("`workspace.members` must hold directory names")
            })?;
            let dir = normalize(raw).map_err(|why| {
                JrsError::manifest(format!(
                    "`workspace.members` names `{raw}`, which {why}; a member is a directory \
                     inside the workspace, relative to its root"
                ))
            })?;
            if dir == "." {
                return Err(JrsError::manifest(
                    "`workspace.members` names the root itself; give the root a [project] \
                     table instead, and it is a member too",
                ));
            }
            if members.iter().any(|m: &Member| m.dir == dir) {
                return Err(JrsError::manifest(format!(
                    "`workspace.members` names `{dir}` twice"
                )));
            }
            let member_root = root.join(&dir);
            let member_path = member_root.join(MANIFEST_FILE);
            let text = std::fs::read_to_string(&member_path).map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    JrsError::manifest(format!(
                        "the workspace member `{dir}` has no `{MANIFEST_FILE}`: {} does not \
                         exist",
                        member_path.display()
                    ))
                } else {
                    JrsError::io(&member_path, e)
                }
            })?;
            let mut member_table = manifest::parse_toml(&text, &member_path)?;
            if member_table.contains_key("workspace") {
                return Err(JrsError::manifest(format!(
                    "{} has a [workspace] of its own; a member cannot be a workspace too",
                    member_path.display()
                )));
            }
            inherit(&mut member_table, &shared);
            members.push(Member {
                dir,
                manifest: Manifest::from_table(&member_table, &member_path, &member_root)?,
                modules: Vec::new(),
            });
        }
        if members.is_empty() {
            return Err(JrsError::manifest(format!(
                "{}: the workspace has no members",
                path.display()
            )));
        }
        for (i, member) in members.iter().enumerate() {
            if let Some(other) = members[..i]
                .iter()
                .find(|o| o.manifest.name == member.manifest.name)
            {
                return Err(JrsError::manifest(format!(
                    "the workspace members `{}` and `{}` are both called `{}`; each member's \
                     `project.name` must be its own",
                    other.dir, member.dir, member.manifest.name
                )));
            }
        }
        let edges = module_edges(&members)?;
        for (member, modules) in members.iter_mut().zip(edges) {
            member.modules = modules;
        }
        let order = build_order(&members)?;
        check_member_tasks(&members)?;
        let name = members
            .iter()
            .find(|m| m.dir == ".")
            .map(|m| m.manifest.name.clone())
            .or_else(|| root.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_else(|| "workspace".to_string());
        Ok(Workspace {
            root,
            path,
            name,
            members,
            order,
            warnings,
            parallel_tests,
        })
    }

    /// The member called `name`.
    #[must_use]
    pub fn member(&self, name: &str) -> Option<usize> {
        self.members.iter().position(|m| m.manifest.name == name)
    }

    /// The members' names, in declaration order, as a message lists them.
    #[must_use]
    pub fn names(&self) -> String {
        self.members
            .iter()
            .map(|m| format!("`{}`", m.manifest.name))
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// The members a command acts on: those `-p` names, else the one it was
    /// run from inside, else every member.
    ///
    /// # Errors
    ///
    /// [`JrsError::Usage`] when `-p` names no member.
    pub fn selected(&self, names: &[String], location: Option<usize>) -> Result<Vec<usize>> {
        if names.is_empty() {
            return Ok(match location {
                Some(i) => vec![i],
                None => self.order.clone(),
            });
        }
        let mut out = Vec::new();
        for name in names {
            let index = self.member(name).ok_or_else(|| {
                JrsError::usage(format!(
                    "there is no member `{name}` in the workspace at {}\n\nits members are {}",
                    self.root.display(),
                    self.names()
                ))
            })?;
            if !out.contains(&index) {
                out.push(index);
            }
        }
        Ok(out)
    }

    /// `members` and every member they depend on, in build order.
    #[must_use]
    pub fn with_upstream(&self, members: &[usize]) -> Vec<usize> {
        let mut wanted: HashSet<usize> = members.iter().copied().collect();
        let mut queue: VecDeque<usize> = members.iter().copied().collect();
        while let Some(i) = queue.pop_front() {
            for (up, _) in &self.members[i].modules {
                if wanted.insert(*up) {
                    queue.push_back(*up);
                }
            }
        }
        self.order
            .iter()
            .copied()
            .filter(|i| wanted.contains(i))
            .collect()
    }

    /// How many of `members`, which are in build order with what they
    /// depend on among them, can compile at once at most: the most members
    /// at one depth of the graph, a member's depth being one more than its
    /// deepest upstream member's (`specs/todo/BEATING_THE_DAEMON_4.md` §4).
    #[must_use]
    pub fn widest_level(&self, members: &[usize]) -> usize {
        let mut depth: HashMap<usize, usize> = HashMap::new();
        let mut width: HashMap<usize, usize> = HashMap::new();
        for i in members {
            let d = self.members[*i]
                .modules
                .iter()
                .filter_map(|(up, _)| depth.get(up).map(|d| d + 1))
                .max()
                .unwrap_or(0);
            depth.insert(*i, d);
            *width.entry(d).or_default() += 1;
        }
        width.into_values().max().unwrap_or(0)
    }

    /// Every member `member` depends on, directly or through another, with
    /// the classpath its classes go on there: nearest first, then in
    /// declaration order.
    #[must_use]
    pub fn upstream(&self, member: usize) -> Vec<(usize, Classpath)> {
        let mut reached: Vec<(usize, Classpath)> = Vec::new();
        let mut queue: VecDeque<(usize, Classpath)> =
            self.members[member].modules.iter().copied().collect();
        while let Some((i, classpath)) = queue.pop_front() {
            if i == member {
                continue;
            }
            if let Some(seen) = reached.iter_mut().find(|(r, _)| *r == i) {
                let joined = seen.1.join(classpath);
                if joined == seen.1 {
                    continue;
                }
                seen.1 = joined;
            } else {
                reached.push((i, classpath));
            }
            for (up, edge) in &self.members[i].modules {
                if let Some(c) = carried(classpath, *edge) {
                    queue.push_back((*up, c));
                }
            }
        }
        reached
    }

    /// [`upstream`](Self::upstream), with what a dependent needs of each.
    #[must_use]
    pub fn upstream_of(&self, member: usize) -> Vec<Upstream> {
        self.upstream(member)
            .into_iter()
            .map(|(i, classpath)| {
                let m = &self.members[i].manifest;
                let project = crate::project::Project::new(m);
                Upstream {
                    name: m.name.clone(),
                    version: m.version.clone(),
                    root: m.root.clone(),
                    classes_dir: project.classes_dir(),
                    jar_path: project.jar_path(),
                    classpath,
                    direct: self.members[member].modules.iter().any(|(m, _)| *m == i),
                }
            })
            .collect()
    }

    /// The other members' tasks that member `member`'s command may reach,
    /// each as (member, task), in the order they run: through its hooks,
    /// and through `named`, the task `jrs task` names. Each one's own
    /// cross-member dependencies come before it (§6.3).
    #[must_use]
    pub fn member_tasks(&self, member: usize, named: Option<&str>) -> Vec<(usize, String)> {
        let manifest = &self.members[member].manifest;
        let mut roots: Vec<TaskRef> = Hook::ALL
            .iter()
            .flat_map(|h| manifest.hooks.tasks(*h))
            .map(|n| TaskRef::Task(n.clone()))
            .collect();
        if let Some(name) = named {
            roots.push(TaskRef::Task(name.to_string()));
        }
        let mut out = Vec::new();
        for step in crate::task::plan(manifest, &roots) {
            if let TaskRef::Member { member: name, task } = step
                && let Some(other) = self.member(&name)
            {
                for before in self.member_tasks(other, Some(&task)) {
                    if !out.contains(&before) {
                        out.push(before);
                    }
                }
                let this = (other, task);
                if !out.contains(&this) {
                    out.push(this);
                }
            }
        }
        out
    }

    /// The members' directories, as `jrs.lock`'s `members` lists them.
    #[must_use]
    pub fn member_dirs(&self) -> Vec<String> {
        self.members.iter().map(|m| m.dir.clone()).collect()
    }

    /// Every member's dependencies as one manifest, rooted at the
    /// workspace's root: what the workspace resolves, once, so that
    /// nearest-wins mediation picks one version of every artifact for every
    /// member (§5). Members come in declaration order, each one's
    /// dependencies in its own; a coordinate two members declare is taken
    /// at the first one's version, with a warning when they differ.
    ///
    /// # Errors
    ///
    /// [`JrsError::Manifest`] when two members turn on one language at two
    /// versions, pin two `ProGuard` releases, or name one java agent at two
    /// versions: a workspace pins one of each.
    #[allow(
        clippy::too_many_lines,
        reason = "one fold per table the aggregate takes from the members, each with the \
                  conflict it refuses"
    )]
    pub fn aggregate(&self) -> Result<(Manifest, Vec<String>)> {
        let mut manifest = manifest::blank(&self.name, "0", &self.root);
        manifest.path.clone_from(&self.path);
        manifest.repositories.clear();
        let mut warnings = Vec::new();
        let mut declared_by: HashMap<String, (String, String)> = HashMap::new();

        for member in &self.members {
            let m = &member.manifest;
            for repository in &m.repositories {
                if !manifest
                    .repositories
                    .iter()
                    .any(|r| r.name == repository.name)
                {
                    manifest.repositories.push(repository.clone());
                }
            }
            for entry in &m.managed {
                if !manifest.managed.iter().any(|e| e.key() == entry.key()) {
                    manifest.managed.push(entry.clone());
                }
            }
            for (dependency, dev) in m
                .effective_dependencies()
                .into_iter()
                .map(|d| (d, false))
                .chain(m.dev_dependencies.iter().cloned().map(|d| (d, true)))
            {
                let dependency = Self::workspace_dependency(member, dependency);
                let key = dependency.key();
                if let Some((first, version)) = declared_by.get(&key) {
                    if !dependency.is_local() && *version != dependency.version {
                        warnings.push(format!(
                            "`{key}` is declared at {} by `{first}` and at {} by `{}`; \
                             the workspace pins one version, and `{first}` came first",
                            display_version(version),
                            display_version(&dependency.version),
                            m.name
                        ));
                    }
                } else {
                    declared_by.insert(key.clone(), (m.name.clone(), dependency.version.clone()));
                }
                let main = manifest.dependencies.iter().position(|d| d.key() == key);
                let test = manifest
                    .dev_dependencies
                    .iter()
                    .position(|d| d.key() == key);
                match (dev, main, test) {
                    (_, Some(i), _) => merge(&mut manifest.dependencies[i], &dependency),
                    (false, None, Some(i)) => {
                        // Declared for the tests first, and for the main
                        // sources now: the main declaration wins.
                        let mut moved = manifest.dev_dependencies.remove(i);
                        moved.compile_only = dependency.compile_only;
                        moved.runtime_only = dependency.runtime_only;
                        merge(&mut moved, &dependency);
                        manifest.dependencies.push(moved);
                    }
                    (true, None, Some(i)) => merge(&mut manifest.dev_dependencies[i], &dependency),
                    (false, None, None) => manifest.dependencies.push(dependency),
                    (true, None, None) => manifest.dev_dependencies.push(dependency),
                }
            }
            for config in &m.languages {
                match manifest
                    .languages
                    .iter_mut()
                    .find(|c| c.language == config.language)
                {
                    None => manifest.languages.push(config.clone()),
                    Some(existing) if existing.version != config.version => {
                        return Err(JrsError::manifest(format!(
                            "the workspace's members turn {} on at {} and {}; one workspace \
                             pins one compiler for each language, so give them one version, \
                             once, in the root's [{}]",
                            config.language,
                            existing.version,
                            config.version,
                            config.language.key()
                        )));
                    }
                    Some(existing) => {
                        for plugin in &config.plugins {
                            if !existing.plugins.contains(plugin) {
                                existing.plugins.push(plugin.clone());
                            }
                        }
                    }
                }
            }
            if let Some(obfuscate) = &m.obfuscate {
                match &manifest.obfuscate {
                    None => manifest.obfuscate = Some(obfuscate.clone()),
                    Some(existing) if existing.version != obfuscate.version => {
                        return Err(JrsError::manifest(format!(
                            "the workspace's members pin ProGuard {} and {}; one workspace \
                             pins one obfuscator",
                            existing.version, obfuscate.version
                        )));
                    }
                    Some(_) => {}
                }
            }
            for (section, agents, into) in [
                ("run", &m.run.java_agents, &mut manifest.run.java_agents),
                ("test", &m.test.java_agents, &mut manifest.test.java_agents),
            ] {
                for agent in agents {
                    let JavaAgent::Pinned(coord) = agent else {
                        continue;
                    };
                    let name = agent.tool_name(section);
                    match into.iter().find(|a| a.tool_name(section) == name) {
                        None => into.push(agent.clone()),
                        Some(JavaAgent::Pinned(existing)) if existing != coord => {
                            return Err(JrsError::manifest(format!(
                                "the workspace's members name the java agent `{}` at {} and \
                                 {}; one workspace pins one of each",
                                coord.ga(),
                                existing.version,
                                coord.version
                            )));
                        }
                        Some(_) => {}
                    }
                }
            }
            for task in m.tasks.iter().filter(|t| !t.dependencies.is_empty()) {
                let mut task = task.clone();
                task.name = task_tool(&m.name, &task.name);
                manifest.tasks.push(task);
            }
        }
        if manifest.repositories.is_empty() {
            manifest.repositories = manifest::blank("x", "0", &self.root).repositories;
        }
        Ok((manifest, warnings))
    }

    /// A member's dependency as the aggregate holds it: a local jar named
    /// and placed relative to the root, so that two members' `driver`s are
    /// two jars.
    fn workspace_dependency(member: &Member, mut dependency: Dependency) -> Dependency {
        if let Some(path) = &dependency.path
            && member.dir != "."
        {
            dependency.path = Some(format!("{}/{path}", member.dir));
            dependency.artifact = local_name(&member.dir, &dependency.artifact);
        }
        dependency
    }

    /// Member `member`'s share of the workspace's resolution `graph`: the
    /// packages its own dependencies and its upstream members' reach, each on
    /// the classpath this member puts it on (§5.2). Versions are the
    /// workspace's; nothing is mediated again.
    #[must_use]
    #[allow(
        clippy::too_many_lines,
        reason = "the roots, the walk from each, then the packages and roots it reached"
    )]
    pub fn project(&self, member: usize, graph: &Resolution) -> Resolution {
        let by_ga: HashMap<Ga, &ResolvedPackage> =
            graph.packages.iter().map(|p| (p.ga(), p)).collect();
        let mut reached: HashMap<Ga, (Classpath, usize, bool)> = HashMap::new();
        let mut local: Vec<LocalJar> = Vec::new();
        let own = &self.members[member];

        let mut roots: Vec<(&Member, Dependency, Classpath, bool)> = Vec::new();
        for d in own.manifest.effective_dependencies() {
            let classpath = classpath_of(&d);
            roots.push((own, d, classpath, true));
        }
        for d in &own.manifest.dev_dependencies {
            roots.push((own, d.clone(), Classpath::Test, true));
        }
        for (up, via) in self.upstream(member) {
            let upstream = &self.members[up];
            for d in upstream.manifest.effective_dependencies() {
                if let Some(classpath) = carried(via, classpath_of(&d)) {
                    roots.push((upstream, d, classpath, false));
                }
            }
        }

        for (owner, dependency, classpath, direct) in &roots {
            if dependency.is_local() {
                let name = Self::workspace_dependency(owner, dependency.clone()).artifact;
                if let Some(jar) = graph.local.iter().find(|l| l.name == name) {
                    match local.iter_mut().find(|l| l.name == dependency.artifact) {
                        Some(seen) => seen.classpath = seen.classpath.join(*classpath),
                        None => local.push(LocalJar {
                            name: dependency.artifact.clone(),
                            classpath: *classpath,
                            ..jar.clone()
                        }),
                    }
                }
                continue;
            }
            let start = Ga::new(&dependency.group, &dependency.artifact)
                .with_classifier(dependency.classifier.clone());
            let exclusions: Vec<Ga> = dependency
                .exclusions
                .iter()
                .map(|e| Ga::new(&e.group, &e.artifact))
                .collect();
            let mut seen: HashSet<Ga> = HashSet::new();
            let mut queue: VecDeque<(Ga, usize)> = VecDeque::new();
            queue.push_back((start.clone(), if *direct { 1 } else { 2 }));
            while let Some((ga, depth)) = queue.pop_front() {
                if !seen.insert(ga.clone()) {
                    continue;
                }
                let Some(package) = by_ga.get(&ga) else {
                    continue;
                };
                let entry = reached
                    .entry(ga.clone())
                    .or_insert((*classpath, depth, false));
                entry.0 = entry.0.join(*classpath);
                entry.1 = entry.1.min(depth);
                entry.2 |= *direct && ga == start;
                for child in &package.dependencies {
                    if !exclusions.iter().any(|e| child.excluded_by(e)) {
                        queue.push_back((child.clone(), depth + 1));
                    }
                }
            }
        }

        let packages: Vec<ResolvedPackage> = graph
            .packages
            .iter()
            .filter_map(|p| {
                let (classpath, depth, direct) = *reached.get(&p.ga())?;
                Some(ResolvedPackage {
                    classpath,
                    depth,
                    direct,
                    dependencies: p
                        .dependencies
                        .iter()
                        .filter(|d| reached.contains_key(*d))
                        .cloned()
                        .collect(),
                    ..p.clone()
                })
            })
            .collect();
        let ga =
            |d: &Dependency| Ga::new(&d.group, &d.artifact).with_classifier(d.classifier.clone());
        let mut main_roots: Vec<Ga> = Vec::new();
        for d in own
            .manifest
            .effective_dependencies()
            .iter()
            .filter(|d| !d.is_local())
        {
            let ga = ga(d);
            if reached.contains_key(&ga) && !main_roots.contains(&ga) {
                main_roots.push(ga);
            }
        }
        let mut test_roots: Vec<Ga> = Vec::new();
        for d in own
            .manifest
            .dev_dependencies
            .iter()
            .filter(|d| !d.is_local())
        {
            let ga = ga(d);
            if reached.contains_key(&ga) && !test_roots.contains(&ga) {
                test_roots.push(ga);
            }
        }
        Resolution {
            packages,
            roots: main_roots,
            test_roots,
            warnings: Vec::new(),
            downloaded: 0,
            local,
        }
    }
}

/// The name a task with dependencies of its own has in the aggregate, so
/// that two members' `check` tasks are pinned apart: `<member>/<task>`.
#[must_use]
pub fn task_tool(member: &str, task: &str) -> String {
    format!("{member}/{task}")
}

/// A local jar's name in the aggregate.
fn local_name(dir: &str, name: &str) -> String {
    format!("{dir}/{name}")
}

fn display_version(version: &str) -> String {
    if version.is_empty() {
        "the managed version".to_string()
    } else {
        version.to_string()
    }
}

/// Fold a second member's declaration of one coordinate into the first's:
/// an exclusion holds only if both make it, since the graph is walked once
/// for both, and each member's share honours its own (§5.2).
fn merge(into: &mut Dependency, other: &Dependency) {
    into.exclusions.retain(|e| other.exclusions.contains(e));
    into.compile_only &= other.compile_only;
    into.runtime_only &= other.runtime_only;
}

/// The classpath a declared dependency goes on.
fn classpath_of(d: &Dependency) -> Classpath {
    if d.compile_only {
        Classpath::Provided
    } else if d.runtime_only {
        Classpath::Runtime
    } else {
        Classpath::Compile
    }
}

/// How a module dependency's own declaration reaches a dependent that sees
/// the module on `via`: as a jar's transitive dependencies do, a
/// `compile-only` or test dependency not at all, and a `runtime-only` one at
/// runtime (§4.2). `None` when it does not reach it.
#[must_use]
pub fn carried(via: Classpath, declared: Classpath) -> Option<Classpath> {
    match (via, declared) {
        (_, Classpath::Provided | Classpath::Test) => None,
        (Classpath::Test, _) => Some(Classpath::Test),
        (via, Classpath::Compile) => Some(via),
        (Classpath::Compile | Classpath::Runtime, Classpath::Runtime) => Some(Classpath::Runtime),
        (Classpath::Provided, Classpath::Runtime) => Some(Classpath::Provided),
    }
}

/// The classpath a member declares a module dependency on.
fn module_classpath(m: &ModuleDependency) -> Classpath {
    if m.dev {
        Classpath::Test
    } else if m.compile_only {
        Classpath::Provided
    } else if m.runtime_only {
        Classpath::Runtime
    } else {
        Classpath::Compile
    }
}

/// Each member's module dependencies as edges to other members.
fn module_edges(members: &[Member]) -> Result<Vec<Vec<(usize, Classpath)>>> {
    let mut all = Vec::new();
    for (index, member) in members.iter().enumerate() {
        let mut edges = Vec::new();
        for module in &member.manifest.modules {
            let joined = if member.dir == "." {
                module.path.clone()
            } else {
                format!("{}/{}", member.dir, module.path)
            };
            let section = if module.dev {
                "dev-dependencies"
            } else {
                "dependencies"
            };
            let target = normalize(&joined)
                .ok()
                .and_then(|dir| members.iter().position(|m| m.dir == dir))
                .ok_or_else(|| {
                    JrsError::manifest(format!(
                        "{}: `{section}.{}` points at `{}`, which is not a member of the \
                         workspace\n\nits members are {}",
                        member.manifest.path.display(),
                        module.name,
                        module.path,
                        members
                            .iter()
                            .map(|m| format!("`{}` ({})", m.manifest.name, m.dir))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ))
                })?;
            let found = &members[target].manifest.name;
            if *found != module.name {
                return Err(JrsError::manifest(format!(
                    "{}: `{section}.{}` points at `{}`, which is the member `{found}`; the key \
                     of a member's dependency is its `project.name`",
                    member.manifest.path.display(),
                    module.name,
                    module.path
                )));
            }
            if target == index {
                return Err(JrsError::manifest(format!(
                    "{}: `{section}.{}` is the member itself",
                    member.manifest.path.display(),
                    module.name
                )));
            }
            edges.push((target, module_classpath(module)));
        }
        all.push(edges);
    }
    Ok(all)
}

/// Every `"<member>:<task>"` names a member and one of its tasks, and no
/// task reaches itself through other members' (§6.3).
fn check_member_tasks(members: &[Member]) -> Result<()> {
    // A walk over (member, task), through both kinds of `depends-on`.
    fn walk(
        members: &[Member],
        node: (usize, String),
        path: &mut Vec<(usize, String)>,
        done: &mut HashSet<(usize, String)>,
    ) -> std::result::Result<(), Vec<(usize, String)>> {
        if done.contains(&node) {
            return Ok(());
        }
        if let Some(at) = path.iter().position(|n| *n == node) {
            let mut cycle = path[at..].to_vec();
            cycle.push(node);
            return Err(cycle);
        }
        path.push(node.clone());
        let manifest = &members[node.0].manifest;
        if let Some(task) = manifest.task(&node.1) {
            for dep in &task.depends_on {
                let next = match dep {
                    TaskRef::Task(name) => (node.0, name.clone()),
                    TaskRef::Member { member, task } => {
                        let Some(i) = members.iter().position(|m| m.manifest.name == *member)
                        else {
                            continue;
                        };
                        (i, task.clone())
                    }
                    TaskRef::Builtin(_) => continue,
                };
                walk(members, next, path, done)?;
            }
        }
        path.pop();
        done.insert(node);
        Ok(())
    }
    let find = |name: &str| members.iter().position(|m| m.manifest.name == name);
    for member in members {
        for task in &member.manifest.tasks {
            for dep in &task.depends_on {
                let TaskRef::Member {
                    member: name,
                    task: other,
                } = dep
                else {
                    continue;
                };
                let Some(i) = find(name) else {
                    return Err(JrsError::manifest(format!(
                        "{}: `tasks.{}.depends-on` names `{dep}`, and there is no member \
                         `{name}` in the workspace",
                        member.manifest.path.display(),
                        task.name
                    )));
                };
                if members[i].manifest.task(other).is_none() {
                    return Err(JrsError::manifest(format!(
                        "{}: `tasks.{}.depends-on` names `{dep}`, and `{name}` has no task \
                         `{other}`",
                        member.manifest.path.display(),
                        task.name
                    )));
                }
            }
        }
    }
    let mut done = HashSet::new();
    for (i, member) in members.iter().enumerate() {
        for task in &member.manifest.tasks {
            if let Err(cycle) = walk(members, (i, task.name.clone()), &mut Vec::new(), &mut done) {
                let names: Vec<String> = cycle
                    .iter()
                    .map(|(m, t)| format!("{}:{t}", members[*m].manifest.name))
                    .collect();
                return Err(JrsError::manifest(format!(
                    "the workspace's tasks depend on each other in a cycle: {}",
                    names.join(" → ")
                )));
            }
        }
    }
    Ok(())
}

/// The members in build order, or the cycle that prevents one.
fn build_order(members: &[Member]) -> Result<Vec<usize>> {
    // Depth-first, so that a cycle can be named as the path it is.
    fn visit(
        i: usize,
        members: &[Member],
        state: &mut [u8],
        stack: &mut Vec<usize>,
    ) -> std::result::Result<(), Vec<usize>> {
        match state[i] {
            2 => return Ok(()),
            1 => {
                let at = stack.iter().position(|s| *s == i).unwrap_or(0);
                let mut cycle = stack[at..].to_vec();
                cycle.push(i);
                return Err(cycle);
            }
            _ => {}
        }
        state[i] = 1;
        stack.push(i);
        for (up, _) in &members[i].modules {
            visit(*up, members, state, stack)?;
        }
        stack.pop();
        state[i] = 2;
        Ok(())
    }
    let mut state = vec![0u8; members.len()];
    for i in 0..members.len() {
        if let Err(cycle) = visit(i, members, &mut state, &mut Vec::new()) {
            let names: Vec<&str> = cycle
                .iter()
                .map(|i| members[*i].manifest.name.as_str())
                .collect();
            return Err(JrsError::manifest(format!(
                "the workspace's members depend on each other in a cycle: {}\n\n\
                 a member's classes are compiled before any member that depends on them, \
                 so the members cannot depend on each other both ways",
                names.join(" → ")
            )));
        }
    }
    // Kahn's algorithm, taking the earliest declared member that is ready.
    let mut done: Vec<bool> = vec![false; members.len()];
    let mut order = Vec::with_capacity(members.len());
    while order.len() < members.len() {
        let next = (0..members.len())
            .find(|i| !done[*i] && members[*i].modules.iter().all(|(up, _)| done[*up]))
            .unwrap_or(0);
        done[next] = true;
        order.push(next);
    }
    Ok(order)
}

/// Merge the root's shared tables into a member's, the member's own keys
/// winning (§3).
fn inherit(member: &mut toml::Table, shared: &toml::Table) {
    for (key, value) in shared {
        let toml::Value::Table(root) = value else {
            continue;
        };
        let language = SHARED_LANGUAGES.contains(&key.as_str());
        if language && !member.contains_key(key) {
            continue;
        }
        let entry = member
            .entry(key.clone())
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if let toml::Value::Table(own) = entry {
            for (k, v) in root {
                if !own.contains_key(k) {
                    own.insert(k.clone(), v.clone());
                }
            }
        }
    }
}

fn shared_list() -> String {
    let all: Vec<String> = SHARED
        .iter()
        .chain(SHARED_LANGUAGES)
        .map(|k| format!("[{k}]"))
        .collect();
    all.join(", ")
}

/// The table at `path`, or `None` when there is no file.
fn read_table(path: &Path) -> Result<Option<toml::Table>> {
    match std::fs::read_to_string(path) {
        Ok(text) => manifest::parse_toml(&text, path).map(Some),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(JrsError::io(path, e)),
    }
}

/// `dir` relative to `root`, `/`-separated, when it is inside it.
fn relative_dir(root: &Path, dir: &Path) -> Option<String> {
    let rest = dir.strip_prefix(root).ok()?;
    let parts: Vec<String> = rest
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    Some(if parts.is_empty() {
        ".".to_string()
    } else {
        parts.join("/")
    })
}

/// A relative directory, `/`-separated, with `.` and `..` worked out:
/// `app/../core` is `core`. An error saying why when it is absolute or
/// leaves the root.
fn normalize(path: &str) -> std::result::Result<String, &'static str> {
    let path = path.replace('\\', "/");
    if Path::new(&path).is_absolute() || path.starts_with('/') {
        return Err("is absolute");
    }
    let mut parts: Vec<String> = Vec::new();
    for component in Path::new(&path).components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if parts.pop().is_none() {
                    return Err("is outside the workspace");
                }
            }
            Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
            Component::RootDir | Component::Prefix(_) => return Err("is absolute"),
        }
    }
    Ok(if parts.is_empty() {
        ".".to_string()
    } else {
        parts.join("/")
    })
}

/// A short account of the workspace for `-v`: each member and what it
/// depends on.
#[must_use]
pub fn describe(workspace: &Workspace) -> String {
    let mut s = String::new();
    for i in &workspace.order {
        let member = &workspace.members[*i];
        let _ = write!(s, "{} ({})", member.manifest.name, member.dir);
        if !member.modules.is_empty() {
            let names: Vec<&str> = member
                .modules
                .iter()
                .map(|(up, _)| workspace.members[*up].manifest.name.as_str())
                .collect();
            let _ = write!(s, " → {}", names.join(", "));
        }
        s.push('\n');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Tree(PathBuf);

    impl Tree {
        fn new(name: &str) -> Tree {
            let dir =
                std::env::temp_dir().join(format!("jrs-workspace-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Tree(dir)
        }

        fn write(&self, path: &str, text: &str) -> &Tree {
            let path = self.0.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
            self
        }

        fn load(&self) -> Result<Workspace> {
            Workspace::find(&self.0.join(MANIFEST_FILE)).map(|w| w.unwrap().0)
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn member(name: &str, body: &str) -> String {
        format!("[project]\nname = '{name}'\nversion = '1.0.0'\n{body}")
    }

    #[test]
    fn members_inherit_the_shared_tables_and_their_own_keys_win() {
        let tree = Tree::new("inherit");
        tree.write(
            "jrs.toml",
            "[workspace]\nmembers = ['core', 'app']\n\
             [java]\nsource = 21\nencoding = 'ISO-8859-1'\n\
             [managed]\n'g:a' = '1.0'\n\
             [kotlin]\nversion = '2.1.0'\n",
        )
        .write("core/jrs.toml", &member("core", "[java]\nsource = 17\n"))
        .write(
            "app/jrs.toml",
            &member("app", "[kotlin]\n[dependencies]\n'g:a' = {}\n"),
        );
        let ws = tree.load().unwrap();
        let core = &ws.members[0].manifest;
        assert_eq!(core.java.source, Some(17), "the member's own key wins");
        assert_eq!(core.java.encoding, "ISO-8859-1", "the rest is inherited");
        assert!(
            core.languages.is_empty(),
            "a language is the member's choice"
        );
        let app = &ws.members[1].manifest;
        assert_eq!(app.java.source, Some(21));
        assert_eq!(app.languages[0].version, "2.1.0");
        assert_eq!(app.managed.len(), 1, "[managed] reaches the member");
    }

    #[test]
    fn module_dependencies_order_the_build_and_carry_their_classpath() {
        let tree = Tree::new("order");
        tree.write(
            "jrs.toml",
            "[workspace]\nmembers = ['app', 'api', 'core', 'testing']\n",
        )
        .write(
            "app/jrs.toml",
            &member(
                "app",
                "[dependencies]\napi = { path = '../api' }\n\
                     [dev-dependencies]\ntesting = { path = '../testing' }\n",
            ),
        )
        .write(
            "api/jrs.toml",
            &member("api", "[dependencies]\ncore = { path = '../core' }\n"),
        )
        .write("core/jrs.toml", &member("core", ""))
        .write(
            "testing/jrs.toml",
            &member(
                "testing",
                "[dependencies]\ncore = { path = '../core', runtime-only = true }\n",
            ),
        );
        let ws = tree.load().unwrap();
        let names: Vec<&str> = ws
            .order
            .iter()
            .map(|i| ws.members[*i].manifest.name.as_str())
            .collect();
        assert_eq!(names, ["core", "api", "testing", "app"]);
        let upstream: Vec<(&str, Classpath)> = ws
            .upstream(0)
            .into_iter()
            .map(|(i, c)| (ws.members[i].manifest.name.as_str(), c))
            .collect();
        assert_eq!(
            upstream,
            [
                ("api", Classpath::Compile),
                ("testing", Classpath::Test),
                ("core", Classpath::Compile),
            ]
        );
        assert_eq!(ws.with_upstream(&[1]), vec![2, 1]);
        assert_eq!(ws.widest_level(&ws.order), 2, "api and testing");
        assert_eq!(ws.widest_level(&ws.with_upstream(&[1])), 1);
        assert_eq!(ws.widest_level(&[]), 0);
    }

    #[test]
    fn a_cycle_is_named() {
        let tree = Tree::new("cycle");
        tree.write("jrs.toml", "[workspace]\nmembers = ['a', 'b']\n")
            .write(
                "a/jrs.toml",
                &member("a", "[dependencies]\nb = { path = '../b' }\n"),
            )
            .write(
                "b/jrs.toml",
                &member("b", "[dependencies]\na = { path = '../a' }\n"),
            );
        let message = tree.load().unwrap_err().to_string();
        assert!(message.contains("a → b → a"), "{message}");
    }

    #[test]
    fn a_path_must_name_a_member_by_its_name() {
        let tree = Tree::new("names");
        tree.write("jrs.toml", "[workspace]\nmembers = ['a', 'b']\n")
            .write(
                "a/jrs.toml",
                &member("a", "[dependencies]\nlib = { path = '../b' }\n"),
            )
            .write("b/jrs.toml", &member("b", ""));
        let message = tree.load().unwrap_err().to_string();
        assert!(message.contains("which is the member `b`"), "{message}");

        let tree = Tree::new("stranger");
        tree.write("jrs.toml", "[workspace]\nmembers = ['a']\n")
            .write(
                "a/jrs.toml",
                &member("a", "[dependencies]\nb = { path = '../b' }\n"),
            );
        let message = tree.load().unwrap_err().to_string();
        assert!(
            message.contains("not a member of the workspace"),
            "{message}"
        );
    }

    #[test]
    fn a_member_is_found_from_inside_it() {
        let tree = Tree::new("find");
        tree.write("jrs.toml", "[workspace]\nmembers = ['libs/core']\n")
            .write("libs/core/jrs.toml", &member("core", ""))
            .write("other/jrs.toml", &member("other", ""));
        let (ws, at) = Workspace::find(&tree.0.join("libs/core/jrs.toml"))
            .unwrap()
            .unwrap();
        assert_eq!(at, Some(0));
        assert_eq!(ws.members[0].dir, "libs/core");
        assert!(
            Workspace::find(&tree.0.join("other/jrs.toml"))
                .unwrap()
                .is_none(),
            "a directory the root does not list stands alone"
        );

        let broken = Tree::new("broken-above");
        broken
            .write("jrs.toml", "this is [not toml")
            .write("app/jrs.toml", &member("app", ""));
        assert!(
            Workspace::find(&broken.0.join("app/jrs.toml"))
                .unwrap()
                .is_none(),
            "a broken manifest above is not this project's workspace"
        );
    }

    #[test]
    fn carried_follows_the_transitive_rules() {
        use Classpath::{Compile, Provided, Runtime, Test};
        assert_eq!(carried(Compile, Compile), Some(Compile));
        assert_eq!(carried(Compile, Runtime), Some(Runtime));
        assert_eq!(carried(Compile, Provided), None);
        assert_eq!(carried(Runtime, Compile), Some(Runtime));
        assert_eq!(carried(Provided, Compile), Some(Provided));
        assert_eq!(carried(Test, Compile), Some(Test));
        assert_eq!(carried(Compile, Test), None);
    }

    #[test]
    fn the_aggregate_takes_the_first_version_and_warns() {
        let tree = Tree::new("aggregate");
        tree.write("jrs.toml", "[workspace]\nmembers = ['a', 'b']\n")
            .write(
                "a/jrs.toml",
                &member(
                    "a",
                    "[dependencies]\n'g:x' = '1.0'\ndriver = { path = 'libs/d.jar' }\n",
                ),
            )
            .write(
                "b/jrs.toml",
                &member(
                    "b",
                    "[dependencies]\n'g:x' = '2.0'\n[dev-dependencies]\n'g:t' = '1'\n",
                ),
            );
        let ws = tree.load().unwrap();
        let (aggregate, warnings) = ws.aggregate().unwrap();
        let keys: Vec<String> = aggregate
            .dependencies
            .iter()
            .map(Dependency::to_string)
            .collect();
        assert_eq!(keys, ["g:x:1.0", "a/driver (a/libs/d.jar)"]);
        assert_eq!(aggregate.dev_dependencies.len(), 1);
        assert!(warnings[0].contains("`g:x` is declared at 1.0 by `a` and at 2.0 by `b`"));
    }

    #[test]
    fn normalize_works_out_dots() {
        assert_eq!(normalize("app/../core").unwrap(), "core");
        assert_eq!(normalize("./a/b/").unwrap(), "a/b");
        assert!(normalize("../x").is_err());
        assert!(normalize("/x").is_err());
    }
}
