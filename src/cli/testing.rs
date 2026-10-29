//! `jrs test`: compiling the tests, choosing which to run, and the `JUnit` launcher.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use super::session::{CacheUse, Session, display_roots};
use super::{TestArgs, counted};
use crate::build_cache::{self, BuildCache};
use crate::compile::{self, impact, share};
use crate::error::{IoResultExt, JrsError, Result, exit};
use crate::manifest::{self, Hook, ShareClasses};
use crate::project::{self, Project, Sources, Unit};
use crate::resolve::cache::Cache;
use crate::resolve::coord::Coord;
use crate::resolve::{Classpath, Resolution};
use crate::toolchain::Toolchain;
use crate::ui::{self, Ui};
use crate::{runner, task, test as junit, test_report};

/// The entry of a cached test run that holds its counts, beside its
/// reports.
const TEST_OUTCOME: &str = "jrs-test-outcome.txt";

impl Session<'_> {
    pub(super) fn test_command(&self, args: &TestArgs) -> Result<i32> {
        let Some(outcome) = self.test(args)? else {
            self.report_timings("test")?;
            return Ok(exit::SUCCESS);
        };
        self.ui.phase(
            "Finished",
            format!("{} in {}", outcome.describe(), self.elapsed()),
        );
        // A failing run is timed too: how long the tests took is part of it.
        self.report_timings("test")?;
        if outcome.ok() && !outcome.coverage_shortfalls.is_empty() {
            let lines: Vec<String> = outcome
                .coverage_shortfalls
                .iter()
                .map(|s| format!("    {s}"))
                .collect();
            return Err(JrsError::test(format!(
                "coverage is below `test.coverage-minimum` in jrs.toml\n\n{}",
                lines.join("\n")
            )));
        }
        if outcome.ok() {
            Ok(exit::SUCCESS)
        } else {
            // The launcher already printed the failures verbatim; do not restate
            // them, only say that the run failed.
            Err(JrsError::test("tests failed"))
        }
    }

    /// Build, compile the tests, run them, then — only if they passed — the
    /// `post-test` hook. `None` when there are no tests to run.
    #[allow(
        clippy::too_many_lines,
        reason = "the test phases in the order they run, each already a helper or a \
                  library call; the extra lines are their --timings rows"
    )]
    pub(super) fn test(&self, args: &TestArgs) -> Result<Option<junit::TestOutcome>> {
        let suite = match &args.suite {
            None => None,
            Some(name) => Some(self.manifest.test.suite(name).ok_or_else(|| {
                let known: Vec<String> = self
                    .manifest
                    .test
                    .suites
                    .iter()
                    .map(|s| format!("`{}`", s.name))
                    .collect();
                JrsError::usage(if known.is_empty() {
                    format!("there is no test suite `{name}`: jrs.toml declares no [test.suites]")
                } else {
                    format!(
                        "there is no test suite `{name}` in jrs.toml (declared: {})",
                        known.join(", ")
                    )
                })
            })?),
        };
        let project = self.project();
        let reports_dir = match suite {
            None => project.target_dir().join("test-reports"),
            Some(s) => project.suite_dir(&s.name).join("test-reports"),
        };
        // `--rerun-failed` reads the last run's reports before anything else:
        // this run is about to replace them.
        let rerun = if args.rerun_failed {
            match self.last_failures(&reports_dir)? {
                Some(rerun) => Some(rerun),
                None => return Ok(None),
            }
        } else {
            None
        };
        let built = self.build()?;
        self.hook(Hook::PreTest)?;
        let toolchain = self.toolchain()?;

        let generated = task::generated(&self.manifest, Hook::PreTest)?;
        let sources = project.sources(Unit::Test, &generated.sources)?;
        // A suite compiles against the default tests, which may be none.
        if sources.is_empty() && suite.is_none() {
            self.testing(format!(
                "no tests found under {}",
                display_roots(&project.roots(Unit::Test))
            ));
            return Ok(None);
        }

        // Tests compile against the main classes plus the test classpath, and
        // Kotlin tests may use the main module's `internal` declarations.
        let mut classpath = vec![project.classes_dir()];
        classpath.extend(self.upstream_dirs(|_| true));
        classpath.extend(built.resolution.classpath(Classpath::Test));
        if !sources.is_empty() {
            self.compile_tests(&project, &toolchain, &sources, &classpath)?;
        }
        let syncing = Instant::now();
        project::sync_resources(
            &self.manifest.test_resource_path(),
            &project.test_classes_dir(),
            &project.work_dir().join("resources-test.list"),
            None,
        )?;
        self.sync_generated(&generated.resources, &project.test_classes_dir(), "test")?;
        self.timings.since("resources test", syncing);

        // What runs: the default tests, or a suite compiled on top of them.
        let (sources, classpath, scan_dir) = match suite {
            None => (sources, classpath, project.test_classes_dir()),
            Some(suite) => {
                let (sources, classpath) =
                    self.compile_suite(&project, &toolchain, suite, classpath)?;
                if sources.is_empty() {
                    self.testing(format!(
                        "no tests found under {}",
                        self.manifest.root.join(&suite.test_dir).display()
                    ));
                    return Ok(None);
                }
                (
                    sources,
                    classpath,
                    project.suite_dir(&suite.name).join("classes"),
                )
            }
        };
        let (jvm_args, env, forks, retries, section) = match suite {
            None => (
                &self.manifest.test.jvm_args,
                &self.manifest.test.env,
                self.manifest.test.forks(),
                self.manifest.test.retries,
                "test".to_string(),
            ),
            Some(s) => (
                &s.jvm_args,
                &s.env,
                s.forks(),
                s.retries,
                format!("test.suites.{}", s.name),
            ),
        };

        // The launcher, and JaCoCo when coverage is on, are internal
        // dependencies: resolved by jrs, never on the user's own classpath.
        let launcher = junit::launcher_coordinate(&self.manifest, &built.resolution)?;
        let jacoco = self
            .manifest
            .test
            .jacoco_version
            .clone()
            .unwrap_or_else(|| junit::JACOCO_VERSION.to_string());
        let mut internal = vec![(launcher.clone(), "test launcher")];
        if args.coverage {
            internal.push((junit::jacoco_agent(&jacoco), "coverage agent"));
            internal.push((junit::jacoco_cli(&jacoco), "coverage report"));
        }
        let fetched = self.fetch_internal(&internal)?;

        let mut test_classpath = vec![scan_dir.clone()];
        test_classpath.extend(junit::without_bundled_launcher(
            classpath,
            &built.resolution,
        ));
        test_classpath.push(fetched[0].clone());

        let mut jvm_args = jvm_args.clone();
        let exec = project.target_dir().join("jacoco.exec");
        if args.coverage {
            let _ = std::fs::remove_file(&exec);
            jvm_args.insert(0, junit::agent_argument(&fetched[1], &exec));
        }
        // The debugger and `test.java-agents` go ahead of JaCoCo's agent.
        jvm_args.splice(0..0, self.test_jvm_prefix(&built.resolution, args)?);

        let mut run = junit::TestRun {
            jvm_args,
            classpath: test_classpath,
            scan_dir,
            filter: junit::class_name_filter(args.filter.as_deref(), sources.foreign().is_some()),
            include_tags: args.include_tag.clone(),
            exclude_tags: args.exclude_tag.clone(),
            methods: args.method.clone(),
            reports_dir: Some(reports_dir),
            color: self.ui.color(),
            ascii: self.ui.glyphs().charset == ui::Charset::Ascii,
            launcher_version: launcher.version.clone(),
            work_dir: project.work_dir(),
            environment: self.jvm_environment(&section, env, None)?,
            unique_ids: Vec::new(),
            classes: Vec::new(),
            fail_fast: args.fail_fast,
            class_path: Vec::new(),
            share_flags: Vec::new(),
        };
        let mut label = suite.map_or_else(|| "test".to_string(), |s| format!("suite-{}", s.name));
        let mut classes = junit::test_classes(&run.scan_dir)?;
        if let Some(shard) = args.shard {
            // The shard is the whole run from here on: what it records is its
            // own, so that another shard run here does not take it for its.
            classes = shard.select(&classes);
            if classes.is_empty() {
                self.testing(format!("shard {shard} has no test classes to run"));
                return Ok(None);
            }
            run.filter = Some(junit::fork_pattern(&classes, run.filter.as_deref()));
            let _ = write!(label, "-shard-{}-of-{}", shard.index, shard.count);
        }
        let state = project.work_dir().join(format!("{label}.tested"));
        let times = project.work_dir().join(format!("{label}.times"));
        // Only a run of the whole suite says anything about the next one.
        let whole = args.filter.is_none()
            && args.include_tag.is_empty()
            && args.exclude_tag.is_empty()
            && args.method.is_empty()
            && rerun.is_none();
        let impact = if whole {
            Some(self.test_impact(&run, &state)?)
        } else {
            None
        };
        let selected = match &impact {
            Some((impact::Selection::Only(only), _))
                if !args.all && !args.coverage && args.debug.is_none() =>
            {
                // A change can reach a class another shard runs.
                Some(
                    only.iter()
                        .filter(|class| classes.binary_search(class).is_ok())
                        .cloned()
                        .collect::<Vec<_>>(),
                )
            }
            _ => None,
        };
        if let Some(only) = &selected
            && only.is_empty()
        {
            self.testing(
                "no test class reaches a change since the last run (`--all` runs them all)",
            );
            if let Some((_, snapshot)) = &impact {
                snapshot.record(&state, [])?;
            }
            return Ok(None);
        }

        // A passing run of these very classes, here or on another machine,
        // stands for this one (SPEC §7.8).
        let cached = match &impact {
            Some((_, snapshot)) if !args.all && !args.coverage && args.debug.is_none() => {
                self.cached_tests(&run, snapshot)
            }
            _ => None,
        };
        if let Some((cache, key)) = &cached
            && let Some(outcome) = self.restore_tests(cache, key, &run, classes.len())?
        {
            if let Some((_, snapshot)) = &impact {
                snapshot.record(&state, [])?;
            }
            self.hook(Hook::PostTest)?;
            return Ok(Some(outcome));
        }

        let cds = self.share_layout(&mut run, args);
        let mut forks = Self::forks(
            &run,
            args,
            forks,
            rerun.is_some(),
            selected.as_deref().unwrap_or(&classes),
            &junit::load_times(&times),
        );
        if forks.is_empty()
            && let Some(only) = &selected
        {
            run.filter = Some(junit::fork_pattern(only, run.filter.as_deref()));
        }
        let share =
            cds.and_then(|dir| self.test_archive(&dir, &toolchain, &run, &label, &mut forks));
        let reached = selected.as_ref().map(|only| (only.len(), classes.len()));
        let sharded = args.shard.map(|shard| (shard, classes.len()));
        self.announce_tests(
            &mut run,
            rerun.as_ref(),
            &sources,
            reached,
            sharded,
            forks.len(),
        );
        let started = Instant::now();
        let outcome = self
            .launch_tests(&toolchain, &run, &forks, share, args.debug.as_ref())
            .and_then(|mut outcome| {
                self.conclude_tests(&toolchain, &run, args, retries, &mut outcome)?;
                Ok(outcome)
            });
        self.timings.since("test JVM", started);
        if outcome.is_err() {
            impact::forget(&state);
        }
        let mut outcome = outcome?;
        let results = run
            .reports_dir
            .as_deref()
            .and_then(|dir| test_report::load(dir).ok().flatten());
        if let Some(results) = &results {
            junit::record_times(&times, &junit::class_times(results), &classes);
        }
        let ran_all = selected.is_none();
        if let Some((_, snapshot)) = &impact {
            // What failed, or passed only on a retry, runs again next time
            // whatever changes: by class when the reports say which, and
            // otherwise everything that ran.
            let unsettled = |results: &test_report::Results| -> Vec<String> {
                results
                    .failed()
                    .into_iter()
                    .chain(results.flaky())
                    .map(|case| junit::top_level_class(&case.class))
                    .collect()
            };
            let pending: Vec<String> = match &results {
                Some(results) if outcome.ok() || !outcome.stopped_early => {
                    let pending = unsettled(results);
                    if pending.is_empty() && !outcome.ok() {
                        selected.unwrap_or(classes)
                    } else {
                        pending
                    }
                }
                _ if outcome.ok() => Vec::new(),
                _ => selected.unwrap_or(classes),
            };
            snapshot.record(&state, pending)?;
        }

        if let Some((cache, key)) = &cached
            && ran_all
            && outcome.ok()
            && outcome.flaky == 0
            && !outcome.stopped_early
        {
            Self::store_tests(cache, key, &run, &outcome, self.ui);
        }

        // Coverage is reported for a failing run too: which code the failing
        // tests reached is part of working out why.
        if args.coverage && exec.is_file() {
            let started = Instant::now();
            self.coverage_report(&project, &toolchain, exec, &fetched[2])?;
            self.timings.since("coverage report", started);
            outcome.coverage_shortfalls = self.coverage_shortfalls(&project)?;
        }

        if outcome.ok() && outcome.coverage_shortfalls.is_empty() {
            self.hook(Hook::PostTest)?;
        }
        Ok(Some(outcome))
    }

    /// Compile the default test sources into `target/test-classes`, unless
    /// they are fresh.
    fn compile_tests(
        &self,
        project: &Project<'_>,
        toolchain: &Toolchain,
        sources: &Sources,
        classpath: &[PathBuf],
    ) -> Result<()> {
        let mut unit = self.compile_unit(
            "test",
            sources,
            project.test_classes_dir(),
            classpath.to_vec(),
            vec![project.classes_dir()],
        )?;
        // Compile avoidance: the tests see the main classes' API, not their
        // bytes, so a changed method body leaves them fresh (SPEC §7.2).
        let digesting = Instant::now();
        let mut api = compile::api_digest(&project.classes_dir())?;
        if let Some(upstream) = self.upstream_api(|_| true)? {
            api.push('\n');
            api.push_str(&upstream);
        }
        unit.main_api = Some(api);
        self.timings.since("compile test: api digest", digesting);
        let checking = Instant::now();
        // And when it changes, only the tests that use what changed compile.
        unit.tracked = vec![project.classes_dir()];
        unit.tracked.extend(self.upstream_dirs(|_| true));
        if self.cache_use == CacheUse::Verify {
            compile::forget(&unit);
        }
        let stale = compile::is_stale(&unit)?;
        if stale && compile::restore(&unit, self.ui)?.is_some() {
            self.ui.phase(
                "Restored",
                self.named(format!(
                    "{} (from the build cache)",
                    sources.describe("test sources")
                )),
            );
            self.timings.since("compile test (restored)", checking);
        } else if stale {
            let what = sources.describe("test sources");
            self.ui.phase("Compiling", self.named(&what));
            let scope = self.ui.spinner("Compiling", &what);
            let mut steps = Vec::new();
            let result = compile::compile_timed(toolchain, &unit, self.ui, &mut steps);
            scope.finish();
            self.record_steps("test", &steps);
            self.report_recompiled(&unit, &result?);
        } else {
            self.timings.since("compile test (fresh)", checking);
        }
        Ok(())
    }

    /// Compile a `[test.suites]` suite into `target/suites/<name>/classes`
    /// against the main classes, the default test classes and the test
    /// classpath, and copy its resources beside it. Returns its sources and
    /// the classpath it runs with, less its own classes.
    fn compile_suite(
        &self,
        project: &Project<'_>,
        toolchain: &Toolchain,
        suite: &manifest::TestSuite,
        test_classpath: Vec<PathBuf>,
    ) -> Result<(Sources, Vec<PathBuf>)> {
        let sources = project.sources_under(&self.manifest.root.join(&suite.test_dir))?;
        let dir = project.suite_dir(&suite.name);
        let classes = dir.join("classes");
        let mut classpath = vec![project.test_classes_dir()];
        classpath.extend(test_classpath);
        if sources.is_empty() {
            return Ok((sources, classpath));
        }
        let label = format!("suite-{}", suite.name);
        let mut unit = self.compile_unit(
            &label,
            &sources,
            classes.clone(),
            classpath.clone(),
            vec![project.classes_dir(), project.test_classes_dir()],
        )?;
        // What the suite can see of the main and the default test classes.
        let digesting = Instant::now();
        unit.main_api = Some(format!(
            "{}\n{}\n{}",
            compile::api_digest(&project.classes_dir())?,
            compile::api_digest(&project.test_classes_dir())?,
            self.upstream_api(|_| true)?.unwrap_or_default()
        ));
        self.timings
            .since(format!("compile {label}: api digest"), digesting);
        let checking = Instant::now();
        let stale = compile::is_stale(&unit)?;
        if stale && compile::restore(&unit, self.ui)?.is_some() {
            self.ui.phase(
                "Restored",
                format!(
                    "{} (from the build cache)",
                    sources.describe(&format!("{} sources", suite.name))
                ),
            );
            self.timings
                .since(format!("compile {label} (restored)"), checking);
        } else if stale {
            let what = sources.describe(&format!("{} sources", suite.name));
            self.ui.phase("Compiling", self.named(&what));
            let scope = self.ui.spinner("Compiling", &what);
            let mut steps = Vec::new();
            let result = compile::compile_timed(toolchain, &unit, self.ui, &mut steps);
            scope.finish();
            self.record_steps(&label, &steps);
            self.report_recompiled(&unit, &result?);
        } else {
            self.timings
                .since(format!("compile {label} (fresh)"), checking);
        }
        project::sync_resources(
            &self.manifest.root.join(&suite.test_resource_dir),
            &classes,
            &project.work_dir().join(format!("resources-{label}.list")),
            None,
        )?;
        Ok((sources, classpath))
    }

    /// The build cache and the key a whole run of `run` is kept under: its
    /// settings, as the test record's are, with jars by identity, paths
    /// relative to the project and the shared cache, and every file in the
    /// class directories by its contents.
    fn cached_tests(
        &self,
        run: &junit::TestRun,
        snapshot: &impact::Snapshot,
    ) -> Option<(Arc<BuildCache>, String)> {
        let cache = self.build_cache()?;
        let mut text = format!(
            "{}\n{}\n{}\n",
            cache.relative(&run.jvm_args.join("\u{1}")),
            run.launcher_version,
            cache.relative(&run.scan_dir.display().to_string())
        );
        // A shard's pattern: another shard of the same classes runs others.
        if let Some(filter) = &run.filter {
            let _ = writeln!(text, "filter {filter}");
        }
        for (key, value) in &run.environment.vars {
            let _ = writeln!(text, "env {key}={}", cache.relative(value));
        }
        for entry in &run.classpath {
            let shown = cache.relative(&entry.display().to_string());
            if entry.is_dir() {
                let _ = writeln!(text, "dir {shown}");
            } else {
                let _ = writeln!(text, "jar {}", cache.jar(entry)?);
            }
        }
        for (path, hash) in snapshot.files() {
            let _ = writeln!(
                text,
                "file {hash} {}",
                cache.relative(&path.display().to_string())
            );
        }
        let key = cache.key("test", &text);
        Some((cache, key))
    }

    /// Put a cached passing run's reports in place of a run, and say so.
    /// `None` on a miss.
    fn restore_tests(
        &self,
        cache: &BuildCache,
        key: &str,
        run: &junit::TestRun,
        classes: usize,
    ) -> Result<Option<junit::TestOutcome>> {
        let Some(reports) = &run.reports_dir else {
            return Ok(None);
        };
        let Some(mut entries) = cache.load(key, self.ui) else {
            return Ok(None);
        };
        let Some(at) = entries.iter().position(|(name, _)| name == TEST_OUTCOME) else {
            return Ok(None);
        };
        let (_, counts) = entries.remove(at);
        let mut outcome = junit::TestOutcome::default();
        for line in String::from_utf8_lossy(&counts).lines() {
            match line.split_once(' ') {
                Some(("found", n)) => outcome.found = n.parse().unwrap_or_default(),
                Some(("passed", n)) => outcome.passed = n.parse().unwrap_or_default(),
                Some(("skipped", n)) => outcome.skipped = n.parse().unwrap_or_default(),
                _ => {}
            }
        }
        build_cache::extract(&entries, reports)?;
        self.testing(format!(
            "{classes} test {}: passed in the cached run (`--all` runs them)",
            if classes == 1 { "class" } else { "classes" }
        ));
        self.ui
            .verbose(format!("build cache: restored the test reports from {key}"));
        Ok(Some(outcome))
    }

    /// Keep a passing whole run's reports and counts under `key`.
    fn store_tests(
        cache: &BuildCache,
        key: &str,
        run: &junit::TestRun,
        outcome: &junit::TestOutcome,
        ui: &Ui,
    ) {
        let Some(reports) = &run.reports_dir else {
            return;
        };
        let Ok(mut entries) = build_cache::collect(reports, &HashSet::<String>::new()) else {
            return;
        };
        entries.push((
            TEST_OUTCOME.to_string(),
            format!(
                "found {}\npassed {}\nskipped {}\n",
                outcome.found, outcome.passed, outcome.skipped
            )
            .into_bytes(),
        ));
        cache.save(key, &entries, ui);
    }

    /// `test.share-classes`: move the class directories to the launcher's
    /// own class loader, so the test JVM's classpath holds jars alone and
    /// can be mapped from a class-data-sharing archive. Returns where the
    /// archives live, or `None` — having said why under `-v` — when this
    /// run keeps the usual layout: a debugger attaching while classes load,
    /// a java agent's class-file hook, or a class directory sharing a name
    /// with a jar (SPEC §10.2).
    fn share_layout(&self, run: &mut junit::TestRun, args: &TestArgs) -> Option<PathBuf> {
        if self.manifest.test.share_classes == ShareClasses::Off {
            return None;
        }
        let skipped = if args.debug.is_some() {
            Some("a --debug run")
        } else if args.coverage {
            Some("a --coverage run")
        } else if !self.manifest.test.java_agents.is_empty() {
            Some("a run with `test.java-agents`")
        } else {
            None
        };
        if let Some(why) = skipped {
            self.ui.verbose(format!(
                "test classes not shared: {why} keeps the usual layout"
            ));
            return None;
        }
        let dir = Cache::discover().ok()?.root().join("cds");
        match junit::share_layout(run, &dir.join("entries")) {
            Ok(shared) => {
                *run = shared;
                Some(dir)
            }
            Err(why) => {
                self.ui.verbose(format!("test classes not shared: {why}"));
                None
            }
        }
    }

    /// The test JVM's class-data-sharing archive under `dir`, for a run in
    /// one JVM: read, or dumped by this run. Forks read one they all get
    /// here; when there is none yet, the first fork alone dumps it, so that
    /// no two JVMs dump at once and a suite large enough to fork still gets
    /// an archive (`specs/TEST_JVM_BENCHMARK.md` §7.1, finding 3). What it
    /// returns is the share the caller finishes once its JVM is done.
    fn test_archive(
        &self,
        dir: &Path,
        toolchain: &Toolchain,
        run: &junit::TestRun,
        label: &str,
        forks: &mut [(junit::TestRun, usize)],
    ) -> Option<share::Share> {
        let project = format!("{}\n{label}", self.manifest.path.display());
        let share = share::Share::for_tests(
            dir,
            toolchain,
            &project,
            &run.classpath,
            &run.jvm_args,
            true,
            aot_tests(self.manifest.test.share_classes, toolchain),
        )?;
        if let Some(archive) = share.archive() {
            let what = match (share.is_aot(), share.dumps()) {
                (true, true) => "recording for the AOT cache",
                (true, false) => "mapping the AOT cache",
                (false, true) => "dumping the class-data archive",
                (false, false) => "mapping the class-data archive",
            };
            self.ui
                .verbose(format!("test JVM: {what} {}", archive.display()));
        }
        if forks.is_empty() {
            return Some(share);
        }
        if share.dumps() {
            forks[0].0.share_flags.clone_from(&share.flags);
            return Some(share);
        }
        for (fork, _) in forks {
            fork.share_flags.clone_from(&share.flags);
        }
        None
    }

    /// What goes ahead of the test JVM's other arguments: the debugger's
    /// agent under `--debug`, then `test.java-agents`, from the test
    /// classpath or pinned apart from it.
    fn test_jvm_prefix(&self, resolution: &Resolution, args: &TestArgs) -> Result<Vec<String>> {
        let agents =
            self.java_agents("test", &self.manifest.test.java_agents, resolution, false)?;
        Ok(runner::jvm_prefix(args.debug.as_ref(), &agents))
    }

    /// The launchers `test.forks` (or `--forks`) splits `classes` among,
    /// each with the number of classes it was dealt; none for a run in one
    /// JVM. Unset, the number follows from the classes and the machine's
    /// cores, and `times` balances the split. An explicit selection —
    /// `--method`, `--rerun-failed` — runs whole in one, and so does a run
    /// under `--debug`, which waits for one debugger, or `--fail-fast`, which
    /// stops the whole run at its first failure.
    pub(super) fn forks(
        run: &junit::TestRun,
        args: &TestArgs,
        configured: Option<u32>,
        rerun: bool,
        classes: &[String],
        times: &std::collections::BTreeMap<String, u64>,
    ) -> Vec<(junit::TestRun, usize)> {
        let asked = args.forks.or(configured);
        if asked.is_some_and(|forks| forks < 2)
            || rerun
            || !run.methods.is_empty()
            || args.debug.is_some()
            || args.fail_fast
        {
            return Vec::new();
        }
        let forks = asked.map_or_else(
            || {
                let cores = std::thread::available_parallelism().map_or(1, usize::from);
                junit::default_forks(classes.len(), cores)
            },
            |forks| usize::try_from(forks).unwrap_or(usize::MAX),
        );
        let shares = junit::split(classes, forks, times);
        if shares.len() < 2 {
            return Vec::new();
        }
        let sizes = shares.iter().map(Vec::len);
        junit::forked(run, &shares).into_iter().zip(sizes).collect()
    }

    /// Which test classes a change since the last run reaches, and the class
    /// directories as they are now, to record once this run is over. The
    /// settings are what else the outcome depends on: the JVM's flags and
    /// environment, the launcher, and each jar by its size and time.
    fn test_impact(
        &self,
        run: &junit::TestRun,
        state: &Path,
    ) -> Result<(impact::Selection, impact::Snapshot)> {
        let mut settings = format!(
            "{}\n{}\n{}\n",
            run.jvm_args.join("\u{1}"),
            run.launcher_version,
            run.scan_dir.display()
        );
        for (key, value) in &run.environment.vars {
            let _ = writeln!(settings, "env {key}={value}");
        }
        let mut dirs = Vec::new();
        for entry in &run.classpath {
            if entry.is_dir() {
                dirs.push(entry.clone());
                let _ = writeln!(settings, "dir {}", entry.display());
            } else {
                let stamp = std::fs::metadata(entry)
                    .ok()
                    .map(|m| {
                        let modified = m
                            .modified()
                            .ok()
                            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                            .map_or(0, |d| d.as_nanos());
                        format!("{} {modified}", m.len())
                    })
                    .unwrap_or_default();
                let _ = writeln!(settings, "jar {} {stamp}", entry.display());
            }
        }
        let checking = Instant::now();
        let (selection, snapshot) = impact::select(state, &settings, &dirs, &run.scan_dir)?;
        self.timings.since("test impact", checking);
        if let impact::Selection::All(reason) = &selection {
            self.ui
                .verbose(format!("running every test class: {reason}"));
        }
        Ok((selection, snapshot))
    }

    /// Run the launcher under the live test counter, or, when the JVM waits
    /// for a debugger, without one: it has nothing to count yet, and a
    /// counter animating over it would say otherwise.
    ///
    /// With `forks`, they all run at once under the one counter, and each
    /// one's output is passed through whole once they are done, in fork
    /// order, under a line naming it.
    fn launch_tests(
        &self,
        toolchain: &Toolchain,
        run: &junit::TestRun,
        forks: &[(junit::TestRun, usize)],
        share: Option<share::Share>,
        debug: Option<&runner::DebugAddress>,
    ) -> Result<junit::TestOutcome> {
        if let Some(debug) = debug {
            self.announce_debugger(debug);
        }
        let scope = debug.is_none().then(|| self.ui.tests());
        if forks.is_empty() {
            let outcome = junit::run_sharing(toolchain, run, share, self.ui);
            if let Some(scope) = scope {
                scope.finish();
            }
            return outcome;
        }
        let mut runs: Vec<junit::TestRun> = forks.iter().map(|(fork, _)| fork.clone()).collect();
        let mut result = junit::run_forks(toolchain, run, &runs, self.ui);
        // The first fork dumped the archive: it goes in place if that fork
        // got as far as running tests. One that did not may have failed for
        // the dump alone, so the forks run again without it.
        if let Some(share) = share {
            let started = result.as_ref().is_ok_and(|forks| {
                forks.first().is_some_and(|f| {
                    f.outcome.ok() || f.outcome.found > 0 || f.outcome.stopped_early
                })
            });
            share.finish(started);
            if !started {
                runs[0].share_flags.clear();
                result = junit::run_forks(toolchain, run, &runs, self.ui);
            }
        }
        if let Some(scope) = scope {
            scope.finish();
        }
        let mut outcome = junit::TestOutcome::default();
        for (n, (fork, (_, classes))) in result?.into_iter().zip(forks).enumerate() {
            let noun = if *classes == 1 { "class" } else { "classes" };
            self.ui.status(
                "Fork",
                format!("{} of {}: {classes} test {noun}", n + 1, forks.len()),
            );
            for line in &fork.stdout {
                self.ui.println_out(line);
            }
            if !fork.stderr.trim().is_empty() {
                self.ui.passthrough(ui::Stream::Err, fork.stderr.trim_end());
            }
            outcome.absorb(&fork.outcome);
        }
        Ok(outcome)
    }

    /// Point `run` at what `--rerun-failed` selected, and say what is about to
    /// run, in how many JVMs, and when `--fail-fast` cannot be honoured.
    fn announce_tests(
        &self,
        run: &mut junit::TestRun,
        rerun: Option<&(usize, test_report::Selection)>,
        sources: &Sources,
        reached: Option<(usize, usize)>,
        shard: Option<(junit::Shard, usize)>,
        forks: usize,
    ) {
        if let Some((_, selection)) = rerun {
            // The selectors name exactly what failed; no class-name pattern
            // may narrow them.
            run.filter = None;
            run.methods.clone_from(&selection.methods);
            run.unique_ids.clone_from(&selection.unique_ids);
            run.classes.clone_from(&selection.classes);
        }
        if run.fail_fast_mode() == junit::FailFast::Unsupported {
            self.ui.warn(format!(
                "the JUnit Platform {} console launcher cannot stop at the first failure: it has \
                 no --fail-fast (JUnit 6) and no test feed to follow (1.10), so every test runs",
                run.launcher_version
            ));
        }
        if let Some((failed, _)) = rerun {
            self.testing(format!(
                "{} that failed in the last run",
                counted(*failed, "test")
            ));
            return;
        }
        let what = match (reached, shard) {
            (Some((n, of)), None) => format!("{n} of {of} test classes a change reaches"),
            (Some((n, of)), Some((shard, _))) => {
                format!("{n} of shard {shard}'s {of} test classes a change reaches")
            }
            (None, Some((shard, 1))) => format!("shard {shard}: 1 test class"),
            (None, Some((shard, of))) => format!("shard {shard}: {of} test classes"),
            (None, None) => sources.describe("test sources"),
        };
        if forks > 1 {
            self.testing(format!("{what} in {forks} JVMs"));
        } else {
            self.testing(what);
        }
    }

    /// After the launcher: say what stopping it cost, retry what failed, and
    /// write the page.
    fn conclude_tests(
        &self,
        toolchain: &Toolchain,
        run: &junit::TestRun,
        args: &TestArgs,
        configured_retries: u32,
        outcome: &mut junit::TestOutcome,
    ) -> Result<()> {
        if outcome.stopped_early && run.fail_fast_mode() == junit::FailFast::Stop {
            self.ui.warn(format!(
                "the JUnit Platform {} console launcher has no --fail-fast (JUnit 6 added it), \
                 so jrs stopped it after the first failure: this run has no XML report",
                run.launcher_version
            ));
        }
        // A retry is a launcher of its own, and under `--debug` it would stop
        // and wait for a debugger again, unannounced: no retries there.
        let retries = if args.debug.is_some() {
            0
        } else {
            args.retries.unwrap_or(configured_retries)
        };
        if !outcome.ok() && !outcome.stopped_early && retries > 0 {
            self.retry_failures(toolchain, run, retries, outcome)?;
        }
        self.report_tests(run, outcome)
    }

    /// What `--rerun-failed` runs: the tests still failing at the end of the
    /// last run, retries included, as its reports record them, and how many
    /// there are. `None`, having said so, when there are none.
    ///
    /// No reports at all is an error, not nothing to do: a run that ended
    /// before the launcher wrote any did not pass.
    fn last_failures(&self, reports: &Path) -> Result<Option<(usize, test_report::Selection)>> {
        let Some(results) = test_report::load(reports)? else {
            return Err(JrsError::usage(format!(
                "there is no test run to rerun: {} holds no JUnit XML\n\nrun `jrs test` first",
                reports.display()
            )));
        };
        let failed = results.failed();
        if failed.is_empty() {
            self.testing("no failed tests to rerun: nothing failed in the last run");
            return Ok(None);
        }
        Ok(Some((failed.len(), test_report::select(failed))))
    }

    /// `test.retries`: run what is still failing again, each attempt in a
    /// launcher of its own writing its reports into `retry-<n>/`, until it
    /// passes or the attempts run out. What passes on a retry is flaky, which
    /// `report_tests` counts; everything passing in the end makes the run a
    /// success, as it does with Gradle's test-retry plugin.
    fn retry_failures(
        &self,
        toolchain: &Toolchain,
        first: &junit::TestRun,
        retries: u32,
        outcome: &mut junit::TestOutcome,
    ) -> Result<()> {
        let Some(reports) = first.reports_dir.as_deref() else {
            return Ok(());
        };
        for attempt in 1..=retries {
            let Some(results) = test_report::load(reports)? else {
                break;
            };
            let failed = results.failed();
            let selection = test_report::select(failed.iter().copied());
            if selection.is_empty() {
                break;
            }
            self.ui.phase(
                "Retrying",
                format!(
                    "{} (attempt {} of {})",
                    counted(failed.len(), "failed test"),
                    attempt + 1,
                    retries + 1
                ),
            );
            let retry = junit::TestRun {
                jvm_args: junit::appending_coverage(&first.jvm_args),
                filter: None,
                include_tags: Vec::new(),
                exclude_tags: Vec::new(),
                methods: selection.methods,
                unique_ids: selection.unique_ids,
                classes: selection.classes,
                reports_dir: Some(test_report::retry_dir(reports, attempt)),
                fail_fast: false,
                ..first.clone()
            };
            let scope = self.ui.tests();
            let result = junit::run(toolchain, &retry, self.ui);
            scope.finish();
            if result?.ok() {
                outcome.exit_code = exit::SUCCESS;
                break;
            }
        }
        Ok(())
    }

    /// Read the run's reports back, retries folded in: name and count the
    /// flaky tests, and write `index.html` beside the XML. A run that left no
    /// XML — stopped before the launcher wrote it — gets no page.
    fn report_tests(&self, run: &junit::TestRun, outcome: &mut junit::TestOutcome) -> Result<()> {
        let Some(reports) = run.reports_dir.as_deref() else {
            return Ok(());
        };
        let Some(results) = test_report::load(reports)? else {
            return Ok(());
        };
        if results.retries > 0 {
            let flaky = results.flaky();
            for case in &flaky {
                self.ui.status(
                    "Flaky",
                    format!("{} (passed on attempt {})", case.label(), case.attempts),
                );
            }
            outcome.flaky = u64::try_from(flaky.len()).unwrap_or(u64::MAX);
            outcome.failed = if outcome.ok() {
                0
            } else {
                outcome.failed.saturating_sub(outcome.flaky)
            };
        }
        let index = reports.join(test_report::INDEX);
        self.ui.phase(
            "Reporting",
            format!("test results into {}", index.display()),
        );
        test_report::write_html(reports, &self.manifest.name, &results)?;
        Ok(())
    }

    /// The totals of the coverage report just written that fall short of
    /// `test.coverage-minimum`.
    pub(super) fn coverage_shortfalls(
        &self,
        project: &Project<'_>,
    ) -> Result<Vec<junit::CoverageShortfall>> {
        let minimums = &self.manifest.test.coverage_minimum;
        if minimums.is_empty() {
            return Ok(Vec::new());
        }
        let xml = project.target_dir().join("coverage").join("jacoco.xml");
        let text = std::fs::read_to_string(&xml).path(&xml)?;
        Ok(junit::coverage_shortfalls(&text, minimums))
    }

    /// Fetch jrs's own test-time tools, announcing the ones not cached yet.
    /// The jars come back in the order of `internal`.
    pub(super) fn fetch_internal(&self, internal: &[(Coord, &str)]) -> Result<Vec<PathBuf>> {
        let fetcher = self.fetcher()?;
        let missing: Vec<_> = internal
            .iter()
            .filter(|(c, _)| !fetcher.cache().contains(c, "jar"))
            .collect();
        for (coord, what) in &missing {
            self.ui
                .phase("Downloading", format!("{} ({what})", coord.artifact));
        }
        let started = Instant::now();
        let scope = self.ui.downloads(missing.len());
        let jars: Result<Vec<PathBuf>> = internal
            .iter()
            .map(|(coord, _)| fetcher.jar(coord).map(|(path, _)| path))
            .collect();
        scope.finish();
        self.timings.since("downloads (test tools)", started);
        jars
    }

    /// Render `exec` into `target/coverage`, with the report tool at `cli_jar`.
    fn coverage_report(
        &self,
        project: &Project<'_>,
        toolchain: &Toolchain,
        exec: PathBuf,
        cli_jar: &Path,
    ) -> Result<()> {
        let html = project.target_dir().join("coverage");
        if html.exists() {
            std::fs::remove_dir_all(&html).path(&html)?;
        }
        std::fs::create_dir_all(&html).path(&html)?;
        self.ui
            .phase("Reporting", format!("coverage into {}", html.display()));
        let report = junit::CoverageReport {
            exec,
            classes: project.classes_dir(),
            sources: project.roots(Unit::Main),
            xml: html.join("jacoco.xml"),
            html: html.clone(),
            name: self.manifest.name.clone(),
        };
        let coverage = junit::report_coverage(toolchain, &report, cli_jar, self.ui)?;
        self.ui.phase(
            "Coverage",
            format!(
                "{} ({})",
                coverage.describe(),
                html.join("index.html").display()
            ),
        );
        Ok(())
    }
}

/// Whether the test JVM's archive is an AOT cache: asked for as `"aot"`,
/// or `true` on a JDK whose AOT cache keeps method profiles too, where it
/// starts a suite 17–20% faster than the dynamic archive
/// (`specs/TEST_JVM_BENCHMARK.md` §7.2). Where it cannot be assembled,
/// `Share::for_tests` falls back to the archive, so `true` cannot lose by
/// it (`specs/BEATING_THE_DAEMON_2.md` §7).
fn aot_tests(share: ShareClasses, toolchain: &Toolchain) -> bool {
    share == ShareClasses::Aot
        || (share == ShareClasses::On && toolchain.version >= share::JAVAC_AOT_SINCE)
}
