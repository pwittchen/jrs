//! Commands over a workspace (`specs/WORKSPACES.md` §6).
//!
//! The workspace's graph is resolved once, by a session over the aggregate
//! manifest that owns the root's `jrs.lock`; each member then gets a session
//! of its own, given its share of that graph and the members it depends on,
//! and the command runs member by member in build order. The members' phase
//! lines name them and come in that fixed order, so `--progress never` reads
//! the same on every run.

use std::rc::Rc;

use super::{
    Arc, CacheCommand, Cli, Command, Config, HashMap, Instant, JrsError, Language, MemberContext,
    OnceCell, PackageArgs, Path, PathBuf, RefCell, ResolvedWithTools, Result, Session, TestArgs,
    Timings, Ui, Worker, counted, deps_row, exit, junit, report, runner, timings, toolchain,
    wait_for_change,
};
use crate::workspace::Workspace;

/// One command over a workspace.
struct Driver<'a> {
    cli: &'a Cli,
    ui: &'a Ui,
    workspace: Workspace,
    /// The member the command was run from inside, if any.
    location: Option<usize>,
    config: Config,
    started: Instant,
    worker: Option<Arc<Worker>>,
    /// The session over the aggregate manifest, which resolves and pins the
    /// workspace's graph.
    aggregate: OnceCell<Session<'a>>,
    graph: OnceCell<ResolvedWithTools>,
    /// Each member's session, once it is needed: a member is built at most
    /// once per command, however many others depend on it.
    sessions: RefCell<HashMap<usize, Rc<Session<'a>>>>,
}

/// Run the command `cli` names over `workspace`; `location` is the member
/// it was run from inside.
pub(super) fn dispatch(
    cli: &Cli,
    ui: &Ui,
    workspace: Workspace,
    location: Option<usize>,
) -> Result<i32> {
    for warning in &workspace.warnings {
        ui.warn(warning);
    }
    for member in &workspace.members {
        for warning in &member.manifest.warnings {
            ui.warn(format!("{}: {warning}", member.manifest.name));
        }
    }
    ui.verbose(format!(
        "workspace at {}:\n{}",
        workspace.root.display(),
        crate::workspace::describe(&workspace).trim_end()
    ));
    let config = Config::load()?;
    for warning in &config.warnings {
        ui.warn(warning);
    }
    let driver = Driver {
        cli,
        ui,
        workspace,
        location,
        config,
        started: Instant::now(),
        worker: None,
        aggregate: OnceCell::new(),
        graph: OnceCell::new(),
        sessions: RefCell::new(HashMap::new()),
    };
    driver.command()
}

impl<'a> Driver<'a> {
    #[allow(
        clippy::too_many_lines,
        reason = "one arm per command, each a call into the driver or a member's session"
    )]
    fn command(self) -> Result<i32> {
        let cli = self.cli;
        match &cli.command {
            Command::Build { watch: true, .. } => self.watch(true, Driver::build),
            Command::Build { .. } => self.build(),
            Command::Test(args) if args.watch => self.watch(true, |d| d.test(args)),
            Command::Test(args) => self.test(args),
            Command::Task(args) if args.watch => self.watch(true, |d| d.task(args)),
            Command::Task(args) => self.task(args),
            Command::Run {
                watch: true,
                args,
                debug,
                ..
            } => self.run_watch(args, debug.as_ref()),
            Command::Run { args, debug, .. } => self.run(args, debug.as_ref()),
            Command::Package(args) => self.package(args),
            Command::Doc => self.doc(),
            Command::Clean => {
                for i in self.selected()? {
                    self.plain(i).clean_command()?;
                }
                Ok(exit::SUCCESS)
            }
            Command::Tree {
                depth,
                why,
                tool,
                task,
            } => self.tree(*depth, why.as_deref(), tool.as_deref(), task.as_deref()),
            Command::Classpath { test, runtime } => {
                let i = self.one("classpath")?;
                self.session(i)?.classpath_command(*test, *runtime)
            }
            Command::Metadata { no_deps } => self.metadata(*no_deps),
            Command::Update => self.aggregate()?.update_command(),
            Command::Verify => self.aggregate()?.verify_command(),
            Command::Outdated => self.aggregate()?.outdated_command(),
            Command::Licenses { runtime } => self.aggregate()?.licenses_command(*runtime),
            Command::Audit { runtime, ignore } => self.aggregate()?.audit_command(*runtime, ignore),
            Command::Fetch { sources } => {
                let code = self.aggregate()?.fetch_command(*sources)?;
                // The members' own tasks' tools, which the aggregate pins
                // under other names.
                for i in self.selected()? {
                    let session = self.session(i)?;
                    for def in session
                        .manifest
                        .tasks
                        .iter()
                        .filter(|t| !t.dependencies.is_empty())
                    {
                        session.task_classpath(def)?;
                    }
                }
                Ok(code)
            }
            Command::Add {
                coordinates,
                dev,
                compile_only,
                runtime_only,
            } => {
                let i = self.editing("add")?;
                super::add_to(
                    cli,
                    self.ui,
                    &self.plain(i),
                    coordinates,
                    *dev,
                    (*compile_only, *runtime_only),
                )
            }
            Command::Remove { keys, dev } => {
                let i = self.editing("remove")?;
                super::remove_from(cli, self.ui, &self.plain(i), keys, *dev)
            }
            Command::Init { .. }
            | Command::Migrate { .. }
            | Command::Completions { .. }
            | Command::Cache {
                action: CacheCommand::Path | CacheCommand::Prune { .. },
            }
            | Command::SelfCmd { .. } => unreachable!("dispatched before a workspace is loaded"),
        }
    }

    fn elapsed(&self) -> String {
        crate::ui::format_duration(self.started.elapsed())
    }

    // ---- members ----------------------------------------------------------

    /// The members the command acts on: `-p`'s, the one it was run from
    /// inside, or all of them.
    fn selected(&self) -> Result<Vec<usize>> {
        self.workspace
            .selected(&self.cli.global.modules, self.location)
    }

    /// Whether `-p` or the working directory chose the members, rather
    /// than the command acting on all of them.
    fn chosen(&self) -> bool {
        !self.cli.global.modules.is_empty() || self.location.is_some()
    }

    /// The one member a command that acts on one acts on: the chosen one,
    /// or the workspace's only member.
    fn one(&self, command: &str) -> Result<usize> {
        let selected = self.selected()?;
        if selected.len() == 1 {
            return Ok(selected[0]);
        }
        Err(JrsError::usage(format!(
            "`jrs {command}` acts on one member, and the workspace has {}\n\nname it with \
             `-p <member>`; the members are {}",
            self.workspace.members.len(),
            self.workspace.names()
        )))
    }

    /// The member `jrs add` or `jrs remove` edits: the chosen one, or the
    /// root when it is a member itself. A pure aggregator's root has no
    /// dependencies to edit, so the member has to be named.
    fn editing(&self, command: &str) -> Result<usize> {
        if self.chosen() {
            return self.one(command);
        }
        if let Some(root) = self.workspace.members.iter().position(|m| m.dir == ".") {
            return Ok(root);
        }
        Err(JrsError::usage(format!(
            "which member should `jrs {command}` edit? The workspace root has no \
             [project] of its own\n\nname it with `-p <member>`, or run it from inside the \
             member's directory; the members are {}",
            self.workspace.names()
        )))
    }

    /// A session over member `i`'s manifest that knows nothing of the
    /// workspace's graph: for what needs none (`clean`, editing).
    fn plain(&self, i: usize) -> Session<'a> {
        Session::new(
            self.cli,
            self.ui,
            self.workspace.members[i].manifest.clone(),
            self.config.clone(),
        )
    }

    /// The session over the aggregate manifest.
    fn aggregate(&self) -> Result<&Session<'a>> {
        if let Some(session) = self.aggregate.get() {
            return Ok(session);
        }
        let (manifest, warnings) = self.workspace.aggregate()?;
        for warning in warnings {
            self.ui.warn(warning);
        }
        let mut session = Session::new(self.cli, self.ui, manifest, self.config.clone());
        session.workspace_members = self.workspace.member_dirs();
        Ok(self.aggregate.get_or_init(|| session))
    }

    /// The workspace's graph and its tools' graphs, resolved (or read from
    /// `jrs.lock`) and downloaded once.
    fn graph(&self) -> Result<&ResolvedWithTools> {
        if let Some(graph) = self.graph.get() {
            return Ok(graph);
        }
        let aggregate = self.aggregate()?;
        let resolution = aggregate.dependencies(false)?;
        let graph = (
            resolution,
            aggregate.tools.get().cloned().unwrap_or_default(),
            aggregate.task_tools.get().cloned().unwrap_or_default(),
            aggregate.obfuscator.get().cloned().flatten(),
            aggregate.agents.get().cloned().unwrap_or_default(),
        );
        Ok(self.graph.get_or_init(|| graph))
    }

    /// Member `i`'s session: its share of the graph, its tools, and the
    /// members it depends on.
    fn session(&self, i: usize) -> Result<Rc<Session<'a>>> {
        if let Some(session) = self.sessions.borrow().get(&i) {
            return Ok(Rc::clone(session));
        }
        let graph = self.graph()?;
        let member = &self.workspace.members[i];
        let manifest = member.manifest.clone();
        let languages: Vec<Language> = manifest.languages.iter().map(|c| c.language).collect();
        let tools = graph
            .1
            .iter()
            .filter(|t| languages.contains(&t.language))
            .cloned()
            .collect();
        let task_tools = manifest
            .tasks
            .iter()
            .filter_map(|t| {
                let name = crate::workspace::task_tool(&manifest.name, &t.name);
                let (_, graph) = graph.2.iter().find(|(n, _)| *n == name)?;
                Some((t.name.clone(), graph.clone()))
            })
            .collect();
        let obfuscator = manifest.obfuscate.as_ref().and_then(|_| graph.3.clone());
        let agent_names: Vec<String> = manifest
            .pinned_agents()
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        let agents = graph
            .4
            .iter()
            .filter(|(n, _)| agent_names.contains(n))
            .cloned()
            .collect();
        let preset = (
            self.workspace.project(i, &graph.0),
            tools,
            task_tools,
            obfuscator,
            agents,
        );
        let mut session = Session::new(self.cli, self.ui, manifest, self.config.clone())
            .with_worker(self.worker.clone());
        session.member = Some(MemberContext {
            preset,
            upstream: self.workspace.upstream_of(i),
            root: self.workspace.root.clone(),
        });
        let session = Rc::new(session);
        self.sessions.borrow_mut().insert(i, Rc::clone(&session));
        Ok(session)
    }

    /// Member `i`'s session, once the other members' tasks its hooks (and
    /// `named`, the task `jrs task` runs) depend on have run in theirs
    /// (`specs/WORKSPACES.md` §6.3). Each runs once per command.
    fn prepared(&self, i: usize, named: Option<&str>) -> Result<Rc<Session<'a>>> {
        for (other, task) in self.workspace.member_tasks(i, named) {
            if other == i {
                continue;
            }
            self.build_upstream(&[other])?;
            self.session(other)?
                .step(&crate::manifest::TaskRef::Task(task), None)?;
        }
        self.session(i)
    }

    /// Build every member `members` depends on, in build order, so that
    /// their classes are there to compile against.
    fn build_upstream(&self, members: &[usize]) -> Result<()> {
        for i in self.workspace.with_upstream(members) {
            if !members.contains(&i) {
                self.prepared(i, None)?.build()?;
            }
        }
        Ok(())
    }

    // ---- commands ---------------------------------------------------------

    fn build(&self) -> Result<i32> {
        let members = self.workspace.with_upstream(&self.selected()?);
        let mut classes = 0;
        for i in &members {
            classes += self.prepared(*i, None)?.build()?.classes;
        }
        self.ui
            .phase("Finished", format!("build in {}", self.elapsed()));
        self.ui.summary(&[
            (
                "build",
                format!(
                    "ok      {classes} classes in {}",
                    counted(members.len(), "module")
                ),
            ),
            ("deps", deps_row(&self.graph()?.0)),
            ("time", self.elapsed()),
        ]);
        self.report_timings("build", &members)?;
        Ok(exit::SUCCESS)
    }

    /// Test the chosen members, every member at the root, once what they
    /// depend on is built. A member whose tests fail does not stop the
    /// others: the command fails at the end, with every member's count.
    fn test(&self, args: &TestArgs) -> Result<i32> {
        let selected = self.selected()?;
        let mut total = junit::TestOutcome::default();
        let mut failed: Vec<String> = Vec::new();
        let mut shortfalls: Vec<String> = Vec::new();
        let mut tested = 0;
        for i in self.workspace.with_upstream(&selected) {
            let session = self.prepared(i, None)?;
            if !selected.contains(&i) {
                session.build()?;
                continue;
            }
            let Some(outcome) = session.test(args)? else {
                continue;
            };
            tested += 1;
            total.found += outcome.found;
            total.passed += outcome.passed;
            total.failed += outcome.failed;
            total.skipped += outcome.skipped;
            total.flaky += outcome.flaky;
            total.stopped_early |= outcome.stopped_early;
            if !outcome.ok() {
                if total.exit_code == 0 {
                    total.exit_code = outcome.exit_code;
                }
                failed.push(session.manifest.name.clone());
            } else if !outcome.coverage_shortfalls.is_empty() {
                for s in &outcome.coverage_shortfalls {
                    shortfalls.push(format!("    {}: {s}", session.manifest.name));
                }
            }
            if args.fail_fast && !outcome.ok() {
                break;
            }
        }
        self.report_timings("test", &self.workspace.with_upstream(&selected))?;
        if tested == 0 {
            return Ok(exit::SUCCESS);
        }
        self.ui.phase(
            "Finished",
            format!(
                "{} in {} in {}",
                total.describe(),
                counted(tested, "module"),
                self.elapsed()
            ),
        );
        if !failed.is_empty() {
            return Err(JrsError::test(format!(
                "tests failed in {}",
                failed
                    .iter()
                    .map(|n| format!("`{n}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        if !shortfalls.is_empty() {
            return Err(JrsError::test(format!(
                "coverage is below `test.coverage-minimum`\n\n{}",
                shortfalls.join("\n")
            )));
        }
        Ok(exit::SUCCESS)
    }

    /// The member `jrs run` starts: the chosen one, or the only member with
    /// a main class.
    fn runnable(&self) -> Result<usize> {
        if self.chosen() {
            return self.one("run");
        }
        let with_main: Vec<usize> = self
            .workspace
            .order
            .iter()
            .copied()
            .filter(|i| self.workspace.members[*i].manifest.main_class.is_some())
            .collect();
        match with_main.as_slice() {
            [one] => Ok(*one),
            [] => Err(JrsError::manifest(format!(
                "`jrs run` needs a member with a main class, and none of the workspace's \
                 members ({}) names one\n\nadd it to the application's jrs.toml:\n\n    \
                 [project]\n    main-class = \"com.example.Main\"",
                self.workspace.names()
            ))),
            several => Err(JrsError::usage(format!(
                "more than one member has a main class ({}); name the one to run with \
                 `-p <member>`",
                several
                    .iter()
                    .map(|i| format!("`{}`", self.workspace.members[*i].manifest.name))
                    .collect::<Vec<_>>()
                    .join(", ")
            ))),
        }
    }

    fn run(&self, args: &[String], debug: Option<&runner::DebugAddress>) -> Result<i32> {
        let target = self.runnable()?;
        self.build_upstream(&[target])?;
        self.prepared(target, None)?.run_command(args, debug)
    }

    /// `jrs package`: a jar for every chosen member and what it depends on.
    /// At the root, a library member gets its jar whatever the flags that
    /// need a main class ask of the applications beside it, and the portable
    /// layout where they get a fat jar.
    fn package(&self, args: &PackageArgs) -> Result<i32> {
        let members = self.workspace.with_upstream(&self.selected()?);
        let chosen = self.chosen();
        let mut classes = 0;
        let mut rows: Vec<(&'static str, String)> = Vec::new();
        for i in &members {
            let session = self.prepared(*i, None)?;
            let mut member_args = args.clone();
            if !chosen && session.manifest.main_class.is_none() {
                // A library is not run with `java -jar`: the portable layout
                // stands in for a fat jar, and, unlike a thin jar's absolute
                // `Class-Path`, it is the same on every machine.
                if member_args.fat {
                    member_args.fat = false;
                    member_args.portable = true;
                }
                member_args.jlink = false;
                member_args.jpackage = None;
                member_args.dist = false;
                member_args.native_image = false;
            }
            classes += session.build()?.classes;
            rows.extend(
                session
                    .package(&member_args)?
                    .into_iter()
                    .filter(|(key, _)| !matches!(*key, "build" | "deps")),
            );
        }
        self.ui
            .phase("Finished", format!("build in {}", self.elapsed()));
        let mut summary = vec![
            (
                "build",
                format!(
                    "ok      {classes} classes in {}",
                    counted(members.len(), "module")
                ),
            ),
            ("deps", deps_row(&self.graph()?.0)),
        ];
        summary.extend(rows);
        summary.push(("time", self.elapsed()));
        self.ui.summary(&summary);
        self.report_timings("package", &members)?;
        Ok(exit::SUCCESS)
    }

    fn doc(&self) -> Result<i32> {
        let selected = self.selected()?;
        self.build_upstream(&selected)?;
        let mut indexes = Vec::new();
        for i in self.workspace.with_upstream(&selected) {
            if selected.contains(&i) {
                indexes.push(self.prepared(i, None)?.doc()?);
            }
        }
        let shown: Vec<String> = indexes.iter().map(|p| p.display().to_string()).collect();
        self.ui.phase(
            "Finished",
            format!("{} in {}", shown.join(", "), self.elapsed()),
        );
        Ok(exit::SUCCESS)
    }

    /// `jrs task <name>` in every chosen member that declares it; `--list`
    /// lists each member's tasks under its name.
    fn task(&self, args: &super::TaskArgs) -> Result<i32> {
        let selected = self.selected()?;
        let Some(name) = args.name.as_deref().filter(|_| !args.list) else {
            self.ui.suspend();
            for i in &selected {
                let manifest = &self.workspace.members[*i].manifest;
                let lines = crate::task::list(manifest);
                if lines.is_empty() {
                    continue;
                }
                if selected.len() > 1 {
                    self.ui.println_out(format!("{}:", manifest.name));
                }
                for line in lines {
                    self.ui.println_out(if selected.len() > 1 {
                        format!("  {line}")
                    } else {
                        line
                    });
                }
            }
            return Ok(exit::SUCCESS);
        };
        let declaring: Vec<usize> = selected
            .iter()
            .copied()
            .filter(|i| self.workspace.members[*i].manifest.task(name).is_some())
            .collect();
        if declaring.is_empty() {
            return Err(JrsError::usage(format!(
                "no member declares a task `{name}`\n\n`jrs task --list` shows the tasks \
                 there are"
            )));
        }
        let mut code = exit::SUCCESS;
        for i in self.workspace.with_upstream(&declaring) {
            let named = declaring.contains(&i).then_some(name);
            let session = self.prepared(i, named)?;
            if declaring.contains(&i) {
                code = session.task_command(args)?;
                if code != exit::SUCCESS {
                    break;
                }
            } else {
                session.build()?;
            }
        }
        Ok(code)
    }

    fn tree(
        &self,
        depth: Option<usize>,
        why: Option<&str>,
        tool: Option<&str>,
        task: Option<&str>,
    ) -> Result<i32> {
        if let Some(name) = tool {
            let aggregate = self.aggregate()?;
            aggregate.dependencies(false)?;
            let tree = aggregate.tool_tree(name, depth)?;
            self.ui.suspend();
            self.ui.tree(&tree);
            return Ok(exit::SUCCESS);
        }
        let mut selected = self.selected()?;
        if let Some(name) = task {
            selected.retain(|i| self.workspace.members[*i].manifest.task(name).is_some());
        }
        let mut first_error = None;
        let mut shown = 0;
        for i in selected {
            match self.session(i)?.tree_command(depth, why, tool, task) {
                Ok(_) => shown += 1,
                // `--why` looks in every member; one without the artifact
                // has nothing to say.
                Err(e) if why.is_some() => {
                    first_error.get_or_insert(e);
                }
                Err(e) => return Err(e),
            }
        }
        match first_error {
            Some(e) if shown == 0 => Err(e),
            _ if shown == 0 => Err(JrsError::usage(format!(
                "no member declares a task `{}` with dependencies of its own",
                task.unwrap_or_default()
            ))),
            _ => Ok(exit::SUCCESS),
        }
    }

    /// `jrs metadata`: a member's model when one is chosen, and otherwise
    /// the workspace's, which holds every member's.
    fn metadata(&self, no_deps: bool) -> Result<i32> {
        let toolchain = match self.plain(self.workspace.order[0]).toolchain() {
            Ok(toolchain) => Some(toolchain),
            Err(e) => {
                self.ui.warn(format!(
                    "{}; the model has no `jdk`",
                    e.to_string().lines().next().unwrap_or_default()
                ));
                None
            }
        };
        let cache = crate::resolve::cache::Cache::discover().ok();
        let mut documents = Vec::new();
        let members = if self.chosen() {
            self.selected()?
        } else {
            self.workspace
                .members
                .iter()
                .enumerate()
                .map(|(i, _)| i)
                .collect()
        };
        for i in &members {
            let resolution = if no_deps {
                None
            } else {
                Some(self.session(*i)?.dependencies(false)?)
            };
            let upstream = self.workspace.upstream_of(*i);
            documents.push(crate::model::metadata(&crate::model::Inputs {
                manifest: &self.workspace.members[*i].manifest,
                toolchain: toolchain.as_ref(),
                resolution: resolution.as_ref(),
                cache: cache.as_ref(),
                workspace: Some(crate::model::WorkspaceInputs {
                    root: &self.workspace.root,
                    upstream: &upstream,
                }),
            })?);
        }
        let document = if self.chosen() && documents.len() == 1 {
            documents.remove(0)
        } else {
            crate::model::workspace(&self.workspace, documents)
        };
        self.ui.suspend();
        for line in document.render().lines() {
            self.ui.println_out(line);
        }
        Ok(exit::SUCCESS)
    }

    /// `--timings` over every member's phases, each row named after its
    /// member: the table, and its copy in the root's `target/.jrs/`.
    fn report_timings(&self, command: &str, members: &[usize]) -> Result<()> {
        if !self.cli.command.timings() {
            return Ok(());
        }
        let all = Timings::new();
        if let Some(aggregate) = self.aggregate.get() {
            for (label, took) in aggregate.timings.rows() {
                all.record(label, took);
            }
        }
        for i in members {
            if let Some(session) = self.sessions.borrow().get(i) {
                for (label, took) in session.timings.rows() {
                    all.record(format!("{}: {label}", session.manifest.name), took);
                }
            }
        }
        let rows = all.rows();
        let total = self.started.elapsed();
        self.ui.timings(&rows, total);
        let work_dir = self.workspace.root.join("target").join(".jrs");
        std::fs::create_dir_all(&work_dir).map_err(|e| JrsError::io(&work_dir, e))?;
        let path = work_dir.join(timings::FILE);
        std::fs::write(&path, timings::render_file(command, &rows, total))
            .map_err(|e| JrsError::io(&path, e))?;
        Ok(())
    }

    // ---- watching ---------------------------------------------------------

    /// What `--watch` keeps an eye on: the root manifest and every chosen
    /// member's and its upstream members' watched paths.
    fn watched(&self, tests: bool) -> Vec<PathBuf> {
        let mut paths = vec![self.workspace.path.clone()];
        let members = self.selected().map_or_else(
            |_| self.workspace.order.clone(),
            |s| self.workspace.with_upstream(&s),
        );
        for i in members {
            paths.extend(self.plain(i).watched_paths(tests));
        }
        paths
    }

    /// Run `once` now and again whenever a watched file changes, with a
    /// fresh workspace each time: a member's manifest may have changed.
    fn watch(self, tests: bool, once: impl Fn(&Driver<'a>) -> Result<i32>) -> Result<i32> {
        let worker = crate::resolve::cache::Cache::discover()
            .ok()
            .map(|cache| Arc::new(Worker::new(cache.root().join("worker"))));
        let (cli, ui, path) = (self.cli, self.ui, self.workspace.path.clone());
        let mut driver = Some(Driver {
            worker: worker.clone(),
            ..self
        });
        loop {
            let watched = match driver.take() {
                Some(driver) => {
                    if let Err(e) = once(&driver) {
                        report(ui, &e);
                    }
                    driver.watched(tests)
                }
                None => vec![path.clone()],
            };
            ui.phase("Watching", "for changes (ctrl-c to stop)");
            wait_for_change(ui, &watched, || {});
            driver = match reload(cli, ui, &path) {
                Ok((workspace, location)) => Config::load().ok().map(|config| Driver {
                    cli,
                    ui,
                    workspace,
                    location,
                    config,
                    started: Instant::now(),
                    worker: worker.clone(),
                    aggregate: OnceCell::new(),
                    graph: OnceCell::new(),
                    sessions: RefCell::new(HashMap::new()),
                }),
                Err(e) => {
                    report(ui, &e);
                    None
                }
            };
        }
    }

    /// `jrs run --watch` in a workspace: build what the application needs,
    /// start it, and on a change stop it and do it again.
    fn run_watch(self, args: &[String], debug: Option<&runner::DebugAddress>) -> Result<i32> {
        let worker = crate::resolve::cache::Cache::discover()
            .ok()
            .map(|cache| Arc::new(Worker::new(cache.root().join("worker"))));
        let (cli, ui, path) = (self.cli, self.ui, self.workspace.path.clone());
        let mut driver = Some(Driver {
            worker: worker.clone(),
            ..self
        });
        loop {
            let Some(current) = driver.take() else {
                ui.phase("Watching", "for changes (ctrl-c to stop)");
                wait_for_change(ui, std::slice::from_ref(&path), || {});
                driver = reopen(cli, ui, &path, worker.as_ref());
                continue;
            };
            let watched = current.watched(false);
            let started = current.runnable().and_then(|target| {
                current.build_upstream(&[target])?;
                let launch = current.prepared(target, None)?.launch(args, debug)?;
                let child = toolchain::spawn_inherited_in(
                    ui,
                    &launch.java,
                    &launch.args,
                    &launch.environment,
                )?;
                Ok((launch.main_class, child))
            });
            let (main_class, mut child) = match started {
                Ok(started) => started,
                Err(e) => {
                    report(ui, &e);
                    ui.phase("Watching", "for changes (ctrl-c to stop)");
                    wait_for_change(ui, &watched, || {});
                    driver = reopen(cli, ui, &path, worker.as_ref());
                    continue;
                }
            };
            let mut exited = None;
            wait_for_change(ui, &watched, || {
                if exited.is_none() {
                    exited = child
                        .try_wait()
                        .ok()
                        .flatten()
                        .map(|s| s.code().unwrap_or(-1));
                    if let Some(code) = exited {
                        ui.phase("Finished", format!("{main_class} exited with {code}"));
                        ui.phase("Watching", "for changes (ctrl-c to stop)");
                    }
                }
            });
            if exited.is_none() {
                ui.phase("Stopping", format!("{main_class} to restart it"));
                toolchain::stop(&mut child, super::RESTART_GRACE);
            }
            driver = reopen(cli, ui, &path, worker.as_ref());
        }
    }
}

/// The workspace at `path` again, after a change, with the member the
/// command was first run from inside.
fn reload(cli: &Cli, _ui: &Ui, path: &Path) -> Result<(Workspace, Option<usize>)> {
    let found = Workspace::find(&super::manifest_path(cli)?)?;
    match found {
        Some(found) => Ok(found),
        None => Err(JrsError::manifest(format!(
            "{} is no longer a workspace",
            path.display()
        ))),
    }
}

/// A driver over the workspace as it is now, or `None` having said why not.
fn reopen<'a>(
    cli: &'a Cli,
    ui: &'a Ui,
    path: &Path,
    worker: Option<&Arc<Worker>>,
) -> Option<Driver<'a>> {
    let opened = reload(cli, ui, path).and_then(|(workspace, location)| {
        Ok(Driver {
            cli,
            ui,
            workspace,
            location,
            config: Config::load()?,
            started: Instant::now(),
            worker: worker.cloned(),
            aggregate: OnceCell::new(),
            graph: OnceCell::new(),
            sessions: RefCell::new(HashMap::new()),
        })
    });
    match opened {
        Ok(driver) => Some(driver),
        Err(e) => {
            report(ui, &e);
            None
        }
    }
}
