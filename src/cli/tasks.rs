//! `jrs task` and the hooks: user-defined tasks run at their fixed points
//! (SPEC §7.6).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use super::session::{CacheUse, Session};
use super::{PackageArgs, TaskArgs, TestArgs};
use crate::build_cache::BuildCache;
use crate::error::{JrsError, Result, exit};
use crate::manifest::{Builtin, Hook, TaskDef, TaskRef};
use crate::resolve::repo::Fetcher;
use crate::resolve::{self, Classpath, Resolution};
use crate::{task, toolchain};

impl Session<'_> {
    pub(super) fn task_command(&self, args: &TaskArgs) -> Result<i32> {
        let Some(name) = args.name.as_deref().filter(|_| !args.list) else {
            return Ok(self.task_list());
        };
        if self.manifest.task(name).is_none() {
            return Err(JrsError::usage(format!(
                "there is no task `{name}` in {}\n\n`jrs task --list` shows the tasks there are",
                self.manifest.path.display()
            )));
        }
        let plan = task::plan(&self.manifest, &[TaskRef::Task(name.to_string())]);
        // The named task comes last; what it depends on runs as a hook's
        // tasks do, and only the named one gets the terminal.
        for step in plan
            .iter()
            .filter(|s| **s != TaskRef::Task(name.to_string()))
        {
            self.step(step, None)?;
        }
        let code = match self.manifest.task(name) {
            Some(def) if self.ran.borrow_mut().insert(name.to_string()) => {
                self.run_task(def, None, Some(&args.args))?
            }
            _ => exit::SUCCESS,
        };
        if code == exit::SUCCESS {
            self.ui
                .phase("Finished", format!("task {name} in {}", self.elapsed()));
        } else {
            self.ui
                .phase("Finished", format!("task {name} exited with {code}"));
        }
        Ok(code)
    }

    /// `jrs task --list`, on stdout: it is the command's real output.
    fn task_list(&self) -> i32 {
        let lines = task::list(&self.manifest);
        if lines.is_empty() {
            self.ui.phase(
                "Finished",
                format!("{} declares no [tasks]", self.manifest.path.display()),
            );
            return exit::SUCCESS;
        }
        self.ui.suspend();
        for line in lines {
            self.ui.println_out(line);
        }
        exit::SUCCESS
    }

    /// Run the tasks a lifecycle point names, and what they depend on. Every
    /// time the point is reached, whether or not `javac` had anything to do:
    /// skipping work is a task's own business, through its inputs and outputs.
    pub(super) fn hook(&self, hook: Hook) -> Result<()> {
        let roots: Vec<TaskRef> = self
            .manifest
            .hooks
            .tasks(hook)
            .iter()
            .map(|name| TaskRef::Task(name.clone()))
            .collect();
        for step in task::plan(&self.manifest, &roots) {
            self.step(&step, Some(hook))?;
        }
        Ok(())
    }

    /// One step of a plan: a built-in runs as its command would, hooks
    /// included; a task runs unless it already has. Either fails the command
    /// when it fails.
    pub(super) fn step(&self, step: &TaskRef, hook: Option<Hook>) -> Result<()> {
        match step {
            TaskRef::Builtin(builtin) => self.builtin(*builtin),
            // Another member's task: the workspace ran it, in that member,
            // before this member's command began (`specs/WORKSPACES.md` §6.3).
            TaskRef::Member { .. } => Ok(()),
            TaskRef::Task(name) => {
                if !self.ran.borrow_mut().insert(name.clone()) {
                    return Ok(());
                }
                let Some(def) = self.manifest.task(name) else {
                    return Ok(());
                };
                match self.run_task(def, hook, None)? {
                    exit::SUCCESS => Ok(()),
                    // Its output has been passed through already; this only
                    // says which task it was.
                    code => Err(JrsError::build(format!(
                        "task `{name}` failed (exit code {code})"
                    ))),
                }
            }
        }
    }

    /// A built-in named in a `depends-on`: the command's work and its hooks,
    /// without its `Finished` line and summary.
    pub(super) fn builtin(&self, builtin: Builtin) -> Result<()> {
        if !self.done.borrow_mut().insert(builtin) {
            return Ok(());
        }
        match builtin {
            Builtin::Build => {
                self.build()?;
            }
            Builtin::Test => {
                if let Some(outcome) = self.test(&TestArgs::default())?
                    && !outcome.ok()
                {
                    return Err(JrsError::test(format!(
                        "tests failed ({})",
                        outcome.describe()
                    )));
                }
            }
            Builtin::Package => {
                self.package(&PackageArgs::default())?;
            }
            Builtin::Doc => {
                self.doc()?;
            }
        }
        Ok(())
    }

    /// Run one task and return its exit code. `named` carries the arguments
    /// of `jrs task <name> -- ...`: that task gets the terminal, like `jrs run`;
    /// any other task has its output streamed to stderr.
    fn run_task(&self, def: &TaskDef, hook: Option<Hook>, named: Option<&[String]>) -> Result<i32> {
        if def.action.is_none() {
            return Ok(exit::SUCCESS);
        }
        let toolchain = self.toolchain()?;
        let classpaths = if task::needs_classpath(def) || self.resolution.get().is_some() {
            Some(self.classpaths()?)
        } else {
            None
        };
        let tool_classpath = if def.dependencies.is_empty() {
            None
        } else {
            Some(self.task_classpath(def)?)
        };
        let jar = self.jar.borrow().clone();
        let path = std::env::var_os("PATH");
        let ctx = task::Context {
            manifest: &self.manifest,
            toolchain: &toolchain,
            hook,
            classpaths: classpaths.as_ref(),
            jar: jar.as_deref(),
            tool_classpath: tool_classpath.as_deref(),
            offline: self.offline,
            path: path.as_deref(),
        };
        let started = Instant::now();
        let prepared = task::prepare(def, &ctx, named.unwrap_or_default())?;
        // Arguments after `--` are not in the manifest: a run with them is
        // neither restored nor stored, or the cache would fill with one-offs.
        let cached = prepared.is_cached() && named.is_none_or(<[String]>::is_empty);
        let verifying = cached && self.cache_use == CacheUse::Verify;
        if !verifying && prepared.is_fresh() {
            self.ui
                .phase("Fresh", self.named(format!("{} (task)", def.name)));
            self.timings
                .since(task_timing_label(&def.name, hook, Some("fresh")), started);
            return Ok(exit::SUCCESS);
        }
        let key = if cached {
            self.task_key(&prepared)
        } else {
            None
        };
        if let Some((cache, key)) = &key
            && self.restore_task(&prepared, cache, key)?
        {
            self.ui.phase(
                "Restored",
                self.named(format!("{} (task, from the build cache)", def.name)),
            );
            self.timings.since(
                task_timing_label(&def.name, hook, Some("restored")),
                started,
            );
            return Ok(exit::SUCCESS);
        }

        match hook {
            Some(hook) => self
                .ui
                .phase("Task", self.named(format!("{} ({hook})", def.name))),
            None => self.ui.phase("Task", self.named(&def.name)),
        }
        self.ui.verbose(prepared.launch.describe());
        self.ui.verbose(format!("in {}", prepared.cwd.display()));
        for (key, value) in prepared.env.iter().filter(|(k, _)| k != "PATH") {
            self.ui
                .verbose(format!("{key}={}", value.to_string_lossy()));
        }
        let process = toolchain::TaskProcess {
            name: &def.name,
            launch: &prepared.launch,
            cwd: &prepared.cwd,
            env: &prepared.env,
        };
        let code = if named.is_some() {
            toolchain::run_task_inherited(self.ui, &process)?
        } else {
            let scope = self.ui.spinner("Task", &def.name);
            let code = toolchain::run_task_streamed(self.ui, &process);
            scope.finish();
            code?
        };
        self.timings
            .since(task_timing_label(&def.name, hook, None), started);
        if code == exit::SUCCESS {
            prepared.record()?;
            if let Some((cache, key)) = &key {
                self.store_task(&prepared, cache, key)?;
            }
        } else {
            prepared.forget();
            prepared.forget_outputs();
        }
        Ok(code)
    }

    /// The build cache and the key a cached task's outputs are kept under
    /// (SPEC §7.6), or `None` when the cache is off or the task cannot be
    /// keyed.
    fn task_key(&self, prepared: &task::Prepared) -> Option<(Arc<BuildCache>, String)> {
        let cache = self.build_cache()?;
        let Some(text) = prepared.cache_text(&cache) else {
            self.ui.verbose(format!(
                "build cache: task `{}` cannot be keyed, so it runs",
                prepared.name
            ));
            return None;
        };
        let key = cache.key("task", &text);
        Some((cache, key))
    }

    /// Put a cached task's outputs in place of a run, when the cache holds
    /// them and every output is safe to replace. `false` on a miss, under
    /// `--verify-cache`, and whenever anything stands in the way: the task
    /// runs then, and no failure here fails the command.
    fn restore_task(
        &self,
        prepared: &task::Prepared,
        cache: &BuildCache,
        key: &str,
    ) -> Result<bool> {
        if cache.verifies() {
            return Ok(false);
        }
        let Some(entries) = cache.load_task(key, self.ui) else {
            return Ok(false);
        };
        if let Some(file) = prepared.user_file() {
            self.ui.verbose(format!(
                "build cache: task `{}` runs rather than restoring over {}, which it did \
                 not write",
                prepared.name,
                file.display()
            ));
            return Ok(false);
        }
        // Until the outputs are in place, nothing may say they are current.
        prepared.forget();
        prepared.forget_outputs();
        match prepared.restore_outputs(&entries) {
            Ok(true) => {}
            Ok(false) => {
                self.ui.verbose(format!(
                    "build cache: the entry {key} does not describe task `{}`'s outputs; \
                     it runs",
                    prepared.name
                ));
                return Ok(false);
            }
            Err(e) => {
                self.ui.verbose(format!(
                    "build cache: could not restore task `{}`: {e}; it runs",
                    prepared.name
                ));
                return Ok(false);
            }
        }
        prepared.record_outputs(&task::TaskEntry {
            entries,
            ..task::TaskEntry::default()
        })?;
        prepared.record()?;
        self.ui.verbose(format!(
            "build cache: restored task `{}` from {key}",
            prepared.name
        ));
        Ok(true)
    }

    /// After a successful run of a cached task: record its outputs, and
    /// store them — or under `--verify-cache`, compare them with what the
    /// cache holds.
    ///
    /// # Errors
    ///
    /// `JrsError::Build` under `--verify-cache` when the cache holds other
    /// outputs than the task wrote; `JrsError::Io` if the record cannot be
    /// written.
    fn store_task(&self, prepared: &task::Prepared, cache: &BuildCache, key: &str) -> Result<()> {
        let entry = match prepared.collect_outputs() {
            Ok(task::Collected::Entry(entry)) => entry,
            Ok(task::Collected::Link(link)) => {
                self.ui.verbose(format!(
                    "build cache: task `{}` is not stored: {} is a symbolic link",
                    prepared.name,
                    link.display()
                ));
                prepared.forget_outputs();
                return Ok(());
            }
            Err(e) => {
                self.ui.verbose(format!(
                    "build cache: could not read task `{}`'s outputs: {e}",
                    prepared.name
                ));
                prepared.forget_outputs();
                return Ok(());
            }
        };
        prepared.record_outputs(&entry)?;
        if cache.verifies()
            && let Some(held) = cache.load_task(key, self.ui)
        {
            let held: BTreeMap<&str, &[u8]> = held
                .iter()
                .map(|(n, b)| (n.as_str(), b.as_slice()))
                .collect();
            let written: BTreeMap<&str, &[u8]> = entry
                .entries
                .iter()
                .map(|(n, b)| (n.as_str(), b.as_slice()))
                .collect();
            if let Some(name) = held
                .keys()
                .chain(written.keys())
                .find(|name| held.get(*name) != written.get(*name))
            {
                return Err(JrsError::build(format!(
                    "the build cache's entry {key} for task `{}` differs from what it \
                     wrote: `{name}`\n\nsomething wrote that entry from other inputs than \
                     its key says, or the task is not deterministic; whoever can push to \
                     the remote cache should look into it",
                    prepared.name
                )));
            }
            return Ok(());
        }
        cache.save_task(key, &entry.entries, &entry.executable, self.ui);
        Ok(())
    }

    /// The classpaths a task can name, as `jrs classpath` prints them — with
    /// absolute class directories, since a task may run somewhere else.
    pub(super) fn classpaths(&self) -> Result<task::Classpaths> {
        let resolution = self.resolved()?;
        let project = self.project();
        let absolute = |p: PathBuf| std::path::absolute(&p).unwrap_or(p);
        let classes = absolute(project.classes_dir());
        let test_classes = absolute(project.test_classes_dir());
        let with = |dirs: Vec<PathBuf>, jars: Vec<PathBuf>| dirs.into_iter().chain(jars).collect();
        let upstream = |include: fn(Classpath) -> bool| {
            self.upstream_dirs(include)
                .into_iter()
                .map(absolute)
                .collect::<Vec<_>>()
        };
        let dirs = |first: Vec<PathBuf>, include: fn(Classpath) -> bool| {
            first.into_iter().chain(upstream(include)).collect()
        };
        Ok(task::Classpaths {
            compile: with(
                dirs(vec![classes.clone()], Classpath::compiles),
                resolution.classpath(Classpath::Compile),
            ),
            runtime: with(
                dirs(vec![classes.clone()], Classpath::runs),
                resolution.runtime_classpath(),
            ),
            test: with(
                dirs(vec![test_classes, classes], |_| true),
                resolution.classpath(Classpath::Test),
            ),
        })
    }

    /// Download one task's tool graph, as the compilers' are, and return its
    /// classpath.
    pub(super) fn fetch_task_tool(
        &self,
        name: &str,
        graph: &mut Resolution,
        fetcher: &Fetcher,
    ) -> Result<Vec<PathBuf>> {
        let started = Instant::now();
        resolve::locate_cached(graph, fetcher);
        let missing = graph
            .packages
            .iter()
            .filter(|p| p.jar.is_none() && p.packaging != "pom")
            .count();
        if missing > 0 && !self.offline {
            let artifact = graph
                .roots
                .first()
                .map_or_else(String::new, |r| r.artifact.clone());
            self.ui
                .phase("Downloading", format!("{artifact} (task {name})"));
            let scope = self.ui.downloads(missing);
            let result = resolve::fetch_jars(graph, fetcher, self.jobs);
            scope.finish();
            result?;
        } else {
            resolve::fetch_jars(graph, fetcher, self.jobs)?;
        }
        self.timings
            .since(format!("downloads (task {name})"), started);
        // How a tool's own graph mediated is the tool's business.
        for warning in &graph.warnings {
            self.ui.verbose(format!("tasks.{name}: {warning}"));
        }
        Ok(graph.runtime_classpath())
    }

    /// The classpath of `def`'s own dependencies, downloaded if it has not
    /// been yet in this invocation.
    pub(super) fn task_classpath(&self, def: &TaskDef) -> Result<Vec<PathBuf>> {
        if let Some(classpath) = self.task_classpaths.borrow().get(&def.name) {
            return Ok(classpath.clone());
        }
        self.resolved()?;
        if let Some(classpath) = self.task_classpaths.borrow().get(&def.name) {
            return Ok(classpath.clone());
        }
        let mut graph = self
            .task_tools
            .get()
            .and_then(|tools| tools.iter().find(|(name, _)| *name == def.name))
            .map(|(_, graph)| graph.clone())
            .ok_or_else(|| {
                JrsError::build(format!(
                    "the dependencies of task `{}` were not resolved",
                    def.name
                ))
            })?;
        let classpath = self.fetch_task_tool(&def.name, &mut graph, &self.fetcher()?)?;
        self.task_classpaths
            .borrow_mut()
            .insert(def.name.clone(), classpath.clone());
        Ok(classpath)
    }
}

/// A task's row in the `--timings` report: `task gen (pre-compile)`, with
/// `fresh` when its up-to-date check let it be skipped.
fn task_timing_label(name: &str, hook: Option<Hook>, state: Option<&str>) -> String {
    match (hook, state) {
        (Some(hook), None) => format!("task {name} ({hook})"),
        (Some(hook), Some(state)) => format!("task {name} ({hook}, {state})"),
        (None, None) => format!("task {name}"),
        (None, Some(state)) => format!("task {name} ({state})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_task_row_names_its_hook_and_freshness() {
        assert_eq!(
            task_timing_label("gen", Some(Hook::PreCompile), None),
            "task gen (pre-compile)"
        );
        assert_eq!(
            task_timing_label("gen", Some(Hook::PreCompile), Some("fresh")),
            "task gen (pre-compile, fresh)"
        );
        assert_eq!(task_timing_label("fmt", None, None), "task fmt");
        assert_eq!(
            task_timing_label("fmt", None, Some("fresh")),
            "task fmt (fresh)"
        );
        assert_eq!(
            task_timing_label("gen", None, Some("restored")),
            "task gen (restored)"
        );
    }
}
