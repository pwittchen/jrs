//! Command definitions and dispatch.
//!
//! Everything above this line is a library; this is where a build becomes a
//! sequence of phases with output attached. The rule the rest of the codebase
//! depends on holds here too: phase lines are emitted here, and the live scopes
//! only add motion, so `--progress never` produces the same transcript.

use std::path::{Path, PathBuf};
use std::time::Instant;

use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::compile::{self, CompileUnit};
use crate::error::{IoResultExt, JrsError, Result, exit};
use crate::lockfile::Lockfile;
use crate::manifest::{MANIFEST_FILE, Manifest};
use crate::migrate;
use crate::package::{self, JarManifest};
use crate::project::{self, Project};
use crate::resolve::cache::Cache;
use crate::resolve::coord::Ga;
use crate::resolve::repo::Fetcher;
use crate::resolve::{self, Classpath, Resolution, UiReporter};
use crate::runner;
use crate::test as junit;
use crate::toolchain::Toolchain;
use crate::ui::{self, CharsetChoice, Style, TreeNode, Ui, UiOptions, When};

#[derive(Debug, Parser)]
#[command(
    name = "jrs",
    version,
    about = "A Java build system, in Rust",
    long_about = "A Java build system, in Rust.\n\n\
                  jrs builds, tests, runs and packages a single-module Java project \
                  from one jrs.toml file, resolving dependencies from Maven Central.",
    disable_help_subcommand = true
)]
pub struct Cli {
    #[command(flatten)]
    pub global: GlobalFlags,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Args)]
pub struct GlobalFlags {
    /// Echo every subprocess command line and its exit status.
    #[arg(short, long, global = true)]
    pub verbose: bool,

    /// Errors only.
    #[arg(short, long, global = true, conflicts_with = "verbose")]
    pub quiet: bool,

    /// Fail rather than hit the network; use the cache and lockfile only.
    #[arg(long, global = true)]
    pub offline: bool,

    /// Cap parallelism. Defaults to the number of available cores.
    #[arg(short = 'j', long, global = true, value_name = "N")]
    pub jobs: Option<usize>,

    /// Run against a manifest outside the current directory.
    #[arg(long, global = true, value_name = "PATH")]
    pub manifest_path: Option<PathBuf>,

    /// Live animated output.
    #[arg(long, global = true, value_name = "WHEN", default_value = "auto")]
    pub progress: WhenArg,

    /// Colour and styling.
    #[arg(long, global = true, value_name = "WHEN", default_value = "auto")]
    pub color: WhenArg,

    /// Glyph set for spinners, bars and trees.
    #[arg(long, global = true, value_name = "SET", default_value = "auto")]
    pub charset: CharsetArg,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum WhenArg {
    Auto,
    Always,
    Never,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CharsetArg {
    Auto,
    Unicode,
    Ascii,
}

impl From<WhenArg> for When {
    fn from(w: WhenArg) -> When {
        match w {
            WhenArg::Auto => When::Auto,
            WhenArg::Always => When::Always,
            WhenArg::Never => When::Never,
        }
    }
}

impl From<CharsetArg> for CharsetChoice {
    fn from(c: CharsetArg) -> CharsetChoice {
        match c {
            CharsetArg::Auto => CharsetChoice::Auto,
            CharsetArg::Unicode => CharsetChoice::Unicode,
            CharsetArg::Ascii => CharsetChoice::Ascii,
        }
    }
}

impl GlobalFlags {
    pub fn jobs(&self) -> usize {
        self.jobs.filter(|j| *j > 0).unwrap_or_else(default_jobs)
    }

    pub fn ui_options(&self) -> UiOptions {
        UiOptions {
            verbose: self.verbose,
            quiet: self.quiet,
            progress: self.progress.into(),
            color: self.color.into(),
            charset: self.charset.into(),
            jobs: self.jobs(),
        }
    }
}

/// Cores, with a floor of 4: resolution is network-bound, and four in flight
/// beats one even on a single-core machine (SPEC §8.4).
fn default_jobs() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .max(4)
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Resolve dependencies, compile main sources, copy resources.
    Build,

    /// Build, compile test sources, and run the test engine.
    Test {
        /// Only run classes matching this regular expression.
        #[arg(long, value_name = "PATTERN")]
        filter: Option<String>,
    },

    /// Build, then run the project's main class.
    Run {
        /// Arguments passed to the program, after `--`.
        #[arg(last = true, value_name = "ARGS")]
        args: Vec<String>,
    },

    /// Build, then produce target/<name>-<version>.jar.
    Package {
        /// Unpack every runtime dependency into the jar.
        #[arg(long)]
        fat: bool,
    },

    /// Remove the target directory.
    Clean,

    /// Print the resolved dependency graph.
    Tree,

    /// Re-resolve dependencies and rewrite jrs.lock.
    Update,

    /// Scaffold jrs.toml and a starter main class.
    Init {
        /// Project name. Defaults to the directory name.
        #[arg(long, value_name = "NAME")]
        name: Option<String>,
        /// Where to scaffold. Defaults to the current directory.
        #[arg(value_name = "PATH")]
        path: Option<PathBuf>,
    },

    /// Generate jrs.toml from an existing pom.xml or Gradle build.
    Migrate {
        /// Force the source build system instead of detecting it.
        #[arg(long, value_name = "SYSTEM")]
        from: Option<String>,
        /// Print the manifest that would be written; touch nothing.
        #[arg(long)]
        dry_run: bool,
        /// Overwrite an existing jrs.toml.
        #[arg(long)]
        force: bool,
        /// Project root to migrate. Defaults to the current directory.
        #[arg(long, value_name = "DIR")]
        path: Option<PathBuf>,
    },
}

/// Parse arguments, run the command, and turn the result into an exit code.
pub fn main() -> i32 {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            let _ = e.print();
            return match e.kind() {
                clap::error::ErrorKind::DisplayHelp
                | clap::error::ErrorKind::DisplayVersion
                | clap::error::ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => exit::SUCCESS,
                _ => exit::USAGE,
            };
        }
    };

    let ui = Ui::new(cli.global.ui_options());
    match dispatch(&cli, &ui) {
        Ok(code) => code,
        Err(error) => {
            // The live region comes down before any diagnostic is printed.
            ui.suspend();
            ui.error(error.to_string());
            error.exit_code()
        }
    }
}

fn dispatch(cli: &Cli, ui: &Ui) -> Result<i32> {
    match &cli.command {
        Command::Init { name, path } => return init(cli, ui, name.as_deref(), path.as_deref()),
        Command::Migrate {
            from,
            dry_run,
            force,
            path,
        } => {
            return migrate_command(ui, from.as_deref(), *dry_run, *force, path.as_deref());
        }
        _ => {}
    }

    let session = Session::open(cli, ui)?;
    match &cli.command {
        Command::Build => session.build_command(),
        Command::Test { filter } => session.test_command(filter.as_deref()),
        Command::Run { args } => session.run_command(args),
        Command::Package { fat } => session.package_command(*fat),
        Command::Clean => session.clean_command(),
        Command::Tree => session.tree_command(),
        Command::Update => session.update_command(),
        Command::Init { .. } | Command::Migrate { .. } => unreachable!("handled above"),
    }
}

/// One command's worth of state: the manifest, the UI, and the clock.
struct Session<'a> {
    manifest: Manifest,
    ui: &'a Ui,
    jobs: usize,
    offline: bool,
    started: Instant,
}

impl<'a> Session<'a> {
    fn open(cli: &Cli, ui: &'a Ui) -> Result<Session<'a>> {
        let path = match &cli.global.manifest_path {
            Some(p) if p.is_dir() => p.join(MANIFEST_FILE),
            Some(p) => p.clone(),
            None => Manifest::discover(Path::new("."))?,
        };
        let manifest = Manifest::load(&path)?;
        for warning in &manifest.warnings {
            ui.warn(warning);
        }
        Ok(Session {
            manifest,
            ui,
            jobs: cli.global.jobs(),
            offline: cli.global.offline,
            started: Instant::now(),
        })
    }

    fn project(&self) -> Project<'_> {
        Project::new(&self.manifest)
    }

    fn elapsed(&self) -> String {
        ui::format_duration(self.started.elapsed())
    }

    // ---- commands ---------------------------------------------------------

    fn clean_command(&self) -> Result<i32> {
        let project = self.project();
        let target = project.target_dir();
        if project.clean()? {
            self.ui.phase("Removed", target.display());
        } else {
            self.ui
                .phase("Clean", format!("{} does not exist", target.display()));
        }
        Ok(exit::SUCCESS)
    }

    fn build_command(&self) -> Result<i32> {
        let built = self.build()?;
        self.ui
            .phase("Finished", format!("build in {}", self.elapsed()));
        self.ui.summary(&[
            ("build", format!("ok      {} classes", built.classes)),
            (
                "deps",
                format!(
                    "{:<7} {} downloaded",
                    built.resolution.packages.len(),
                    built.resolution.downloaded
                ),
            ),
            ("time", self.elapsed()),
        ]);
        Ok(exit::SUCCESS)
    }

    fn run_command(&self, args: &[String]) -> Result<i32> {
        let main_class = self.manifest.require_main_class("run")?.to_string();
        let built = self.build()?;
        let toolchain = Toolchain::discover()?;

        let mut classpath = vec![self.project().classes_dir()];
        classpath.extend(built.resolution.classpath(Classpath::Compile));

        self.ui.phase("Running", &main_class);
        let code = runner::run_main(&toolchain, &classpath, &main_class, args, self.ui)?;
        if code != 0 {
            self.ui
                .phase("Finished", format!("{main_class} exited with {code}"));
        }
        Ok(code)
    }

    fn package_command(&self, fat: bool) -> Result<i32> {
        let built = self.build()?;
        let project = self.project();
        let output = project.jar_path();

        let outcome = if fat {
            let jars: Vec<PathBuf> = built.resolution.classpath(Classpath::Compile);
            self.ui.phase(
                "Packaging",
                format!("{} (fat, {} dependencies)", output.display(), jars.len()),
            );
            package::write_fat_jar(
                &project.classes_dir(),
                &jars,
                &output,
                &JarManifest {
                    main_class: self.manifest.main_class.clone(),
                    class_path: Vec::new(),
                },
            )?
        } else {
            self.ui.phase("Packaging", output.display());
            // A thin jar points at the cached dependency jars, so `java -jar`
            // works without a classpath argument.
            let class_path = built
                .resolution
                .classpath(Classpath::Compile)
                .iter()
                .map(|p| package::class_path_entry(p))
                .collect();
            package::write_thin_jar(
                &project.classes_dir(),
                &output,
                &JarManifest {
                    main_class: self.manifest.main_class.clone(),
                    class_path,
                },
            )?
        };

        for warning in &outcome.warnings {
            self.ui.warn(warning);
        }
        self.ui
            .phase("Finished", format!("build in {}", self.elapsed()));
        self.ui.summary(&[
            ("build", format!("ok      {} classes", built.classes)),
            (
                "deps",
                format!(
                    "{:<7} {} downloaded",
                    built.resolution.packages.len(),
                    built.resolution.downloaded
                ),
            ),
            (
                "jar",
                format!(
                    "{}   {}",
                    self.manifest.jar_name(),
                    ui::format_bytes(outcome.bytes)
                ),
            ),
            ("time", self.elapsed()),
        ]);
        Ok(exit::SUCCESS)
    }

    fn test_command(&self, filter: Option<&str>) -> Result<i32> {
        let built = self.build()?;
        let project = self.project();
        let toolchain = Toolchain::discover()?;

        let sources = project.test_sources()?;
        if sources.is_empty() {
            self.ui.phase(
                "Testing",
                format!(
                    "no tests found under {}",
                    self.manifest.test_path().display()
                ),
            );
            return Ok(exit::SUCCESS);
        }

        // Tests compile against the main classes plus the test classpath.
        let mut classpath = vec![project.classes_dir()];
        classpath.extend(built.resolution.classpath(Classpath::Test));

        let unit = CompileUnit {
            label: "test".into(),
            sources: sources.clone(),
            output_dir: project.test_classes_dir(),
            classpath: classpath.clone(),
            release: toolchain.release(self.manifest.java.source)?,
            target: self.manifest.java.target,
            encoding: self.manifest.java.encoding.clone(),
            extra_args: self.manifest.java.javac_args.clone(),
            work_dir: project.work_dir(),
        };
        if compile::is_stale(&unit)? {
            self.ui
                .phase("Compiling", format!("{} test sources", sources.len()));
            let scope = self
                .ui
                .spinner("Compiling", format!("{} test sources", sources.len()));
            let result = compile::compile(&toolchain, &unit, self.ui);
            scope.finish();
            result?;
        }
        project::copy_tree(
            &self.manifest.test_resource_path(),
            &project.test_classes_dir(),
        )?;

        // The launcher is an internal dependency: resolved by jrs, and placed
        // last so the user's own JUnit jars win.
        let launcher = junit::launcher_coordinate(&self.manifest)?;
        let fetcher = self.fetcher()?;
        if !fetcher.cache().contains(&launcher, "jar") {
            self.ui.phase(
                "Downloading",
                format!("{} (test launcher)", launcher.artifact),
            );
        }
        let scope = self.ui.downloads(1);
        let fetched = fetcher.jar(&launcher);
        scope.finish();
        let (launcher_jar, _) = fetched?;

        let mut test_classpath = vec![project.test_classes_dir()];
        test_classpath.extend(classpath);
        test_classpath.push(launcher_jar);

        let run = junit::TestRun {
            classpath: test_classpath,
            scan_dir: project.test_classes_dir(),
            filter: filter.map(str::to_string),
            color: self.ui.color(),
            ascii: self.ui.glyphs().charset == ui::Charset::Ascii,
            launcher_version: launcher.version.clone(),
            work_dir: project.work_dir(),
        };

        self.ui
            .phase("Testing", format!("{} test sources", sources.len()));
        let scope = self.ui.tests();
        let outcome = junit::run(&toolchain, &run, self.ui);
        scope.finish();
        let outcome = outcome?;

        if outcome.ok() {
            self.ui.phase(
                "Finished",
                format!("{} in {}", outcome.describe(), self.elapsed()),
            );
            Ok(exit::SUCCESS)
        } else {
            // The launcher already printed the failures verbatim; do not restate
            // them, only say that the run failed.
            self.ui.phase(
                "Finished",
                format!("{} in {}", outcome.describe(), self.elapsed()),
            );
            Err(JrsError::test("tests failed"))
        }
    }

    fn tree_command(&self) -> Result<i32> {
        let resolution = self.dependencies(false)?;
        let root = self.tree(&resolution);
        self.ui.suspend();
        self.ui.tree(&root);
        Ok(exit::SUCCESS)
    }

    fn update_command(&self) -> Result<i32> {
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

    // ---- the build pipeline -----------------------------------------------

    fn fetcher(&self) -> Result<Fetcher> {
        Ok(Fetcher::with_reporter(
            self.manifest.repositories.clone(),
            Cache::discover()?,
            self.offline,
            Box::new(UiReporter::new(self.ui.clone())),
        ))
    }

    /// Resolve, download, compile, copy resources.
    fn build(&self) -> Result<Built> {
        let toolchain = Toolchain::discover()?;
        let resolution = self.dependencies(false)?;
        let project = self.project();

        let sources = project.main_sources()?;
        if sources.is_empty() {
            return Err(JrsError::build(format!(
                "no .java files under {}\n\n\
                 check `project.source-dir` in {}, or run `jrs init` to scaffold one",
                self.manifest.source_path().display(),
                self.manifest.path.display()
            )));
        }

        let unit = CompileUnit {
            label: "main".into(),
            sources: sources.clone(),
            output_dir: project.classes_dir(),
            classpath: resolution.classpath(Classpath::Compile),
            release: toolchain.release(self.manifest.java.source)?,
            target: self.manifest.java.target,
            encoding: self.manifest.java.encoding.clone(),
            extra_args: self.manifest.java.javac_args.clone(),
            work_dir: project.work_dir(),
        };

        let outcome = if compile::is_stale(&unit)? {
            self.ui.phase(
                "Compiling",
                format!(
                    "{} v{} ({} source files)",
                    self.manifest.name,
                    self.manifest.version,
                    sources.len()
                ),
            );
            let scope = self
                .ui
                .spinner("Compiling", format!("{} source files", sources.len()));
            let result = compile::compile(&toolchain, &unit, self.ui);
            scope.finish();
            result?
        } else {
            self.ui.phase(
                "Fresh",
                format!("{} v{}", self.manifest.name, self.manifest.version),
            );
            compile::Outcome::UpToDate
        };

        let copied = project::copy_tree(&self.manifest.resource_path(), &project.classes_dir())?;
        if copied > 0 {
            self.ui.verbose(format!("copied {copied} resources"));
        }

        let classes = match outcome {
            compile::Outcome::Compiled { classes } => classes,
            compile::Outcome::UpToDate => {
                project::find_by_extension(&project.classes_dir(), "class")?.len()
            }
        };

        Ok(Built {
            resolution,
            classes,
        })
    }

    /// The resolved graph, from the lockfile when it still matches the manifest.
    fn dependencies(&self, force_update: bool) -> Result<Resolution> {
        let manifest = &self.manifest;
        if manifest.dependencies.is_empty() && manifest.dev_dependencies.is_empty() {
            return Ok(Resolution::default());
        }

        let fetcher = self.fetcher()?;
        let lock_path = manifest.lock_path();
        let existing = Lockfile::load(&lock_path)?;

        let (mut resolution, fresh) = match existing {
            Some(lock) if lock.matches(manifest) && !force_update => {
                self.ui.verbose(format!("reusing {}", lock_path.display()));
                (lock.to_resolution(), false)
            }
            _ => {
                let declared = manifest.dependencies.len() + manifest.dev_dependencies.len();
                self.ui
                    .phase("Resolving", format!("{declared} declared dependencies"));
                let scope = self
                    .ui
                    .spinner("Resolving", format!("{declared} declared dependencies"));
                let resolved = resolve::resolve(manifest, &fetcher, self.jobs);
                scope.finish();
                (resolved?, true)
            }
        };

        resolve::locate_cached(&mut resolution, &fetcher);
        let missing = resolution
            .packages
            .iter()
            .filter(|p| p.jar.is_none() && p.packaging != "pom")
            .count();
        if missing > 0 && !self.offline {
            self.ui.phase("Downloading", format!("{missing} artifacts"));
            let scope = self.ui.downloads(missing);
            let result = resolve::fetch_jars(&mut resolution, &fetcher, self.jobs);
            scope.finish();
            result?;
        } else {
            resolve::fetch_jars(&mut resolution, &fetcher, self.jobs)?;
        }

        if fresh {
            Lockfile::from_resolution(manifest, &resolution).write(&lock_path)?;
            self.ui.verbose(format!("wrote {}", lock_path.display()));
        }
        for warning in &resolution.warnings {
            self.ui.warn(warning);
        }
        Ok(resolution)
    }

    /// The dependency graph as a drawable tree.
    fn tree(&self, resolution: &Resolution) -> TreeNode {
        let mut root = TreeNode::styled(
            format!("{} v{}", self.manifest.name, self.manifest.version),
            Style::Bold,
        );
        let mut seen = Vec::new();
        for ga in resolution.roots.iter().chain(&resolution.test_roots) {
            root.children
                .push(self.tree_node(resolution, ga, &mut seen, 0));
        }
        root
    }

    fn tree_node(
        &self,
        resolution: &Resolution,
        ga: &Ga,
        seen: &mut Vec<Ga>,
        depth: usize,
    ) -> TreeNode {
        let Some(package) = resolution.get(ga) else {
            return TreeNode::styled(format!("{ga} (unresolved)"), Style::Red);
        };
        let mut label = package.coord.to_string();
        if package.classpath == Classpath::Test {
            label.push_str(" (test)");
        }
        // Mediated versions are coloured so the nearest-wins decision is visible
        // at a glance (SPEC §5.3.6).
        let style = if package.mediated {
            Style::Yellow
        } else {
            Style::None
        };

        if seen.contains(ga) {
            return TreeNode::styled(format!("{label} (*)"), Style::Dim);
        }
        if depth > 32 {
            return TreeNode::styled(format!("{label} (...)"), Style::Dim);
        }
        seen.push(ga.clone());

        let mut node = TreeNode::styled(label, style);
        for child in &package.dependencies {
            node.children
                .push(self.tree_node(resolution, child, seen, depth + 1));
        }
        node
    }
}

struct Built {
    resolution: Resolution,
    classes: usize,
}

// ---- init ------------------------------------------------------------------

const STARTER_MAIN: &str = r#"package com.example;

public class Main {
    public static void main(String[] args) {
        System.out.println("Hello from jrs");
    }
}
"#;

fn init(cli: &Cli, ui: &Ui, name: Option<&str>, path: Option<&Path>) -> Result<i32> {
    ui.banner();

    let root = path
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    std::fs::create_dir_all(&root).path(&root)?;

    let name = match name {
        Some(n) => n.to_string(),
        None => root
            .canonicalize()
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_else(|| "app".to_string()),
    };

    let manifest_path = root.join(MANIFEST_FILE);
    if manifest_path.exists() {
        return Err(JrsError::usage(format!(
            "{} already exists",
            manifest_path.display()
        )));
    }

    let mut manifest = crate::manifest::blank(&name, "0.1.0", &root);
    manifest.main_class = Some("com.example.Main".to_string());
    if let Ok(toolchain) = Toolchain::discover() {
        manifest.java.source = Some(toolchain.version);
    }
    std::fs::write(&manifest_path, manifest.render(None)).path(&manifest_path)?;
    ui.phase("Created", manifest_path.display());

    let main = manifest.source_path().join("com/example/Main.java");
    if !main.exists() {
        std::fs::create_dir_all(main.parent().unwrap()).path(&main)?;
        std::fs::write(&main, STARTER_MAIN).path(&main)?;
        ui.phase("Created", main.display());
    }

    let _ = cli;
    ui.phase("Next", "jrs run");
    Ok(exit::SUCCESS)
}

// ---- migrate ---------------------------------------------------------------

fn migrate_command(
    ui: &Ui,
    from: Option<&str>,
    dry_run: bool,
    force: bool,
    path: Option<&Path>,
) -> Result<i32> {
    ui.banner();

    let root = path
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let source = from.map(migrate::Source::parse).transpose()?;
    let migration = migrate::plan(&root, source)?;

    ui.phase(
        "Migrating",
        format!(
            "{} ({})",
            migration.source_file.display(),
            migration.source.as_str()
        ),
    );

    // The report and the manifest are real output: they belong on stdout so they
    // can be piped, diffed or redirected.
    ui.suspend();
    if let Some(preamble) = &migration.report.preamble {
        ui.println_out(preamble);
        ui.println_out("");
    }

    print_block(ui, "Migrated", &migration.report.migrated);
    print_block(ui, "Needs review", &migration.report.needs_review);
    print_block(ui, "Not migrated", &migration.report.not_migrated);

    if dry_run {
        ui.println_out("--- jrs.toml (dry run; nothing was written) ---");
        for line in migration.render_manifest().lines() {
            ui.println_out(line);
        }
        return Ok(exit::SUCCESS);
    }

    if let Some(written) = migrate::write(&migration, &root, force)? {
        ui.phase("Created", written.display());
    }
    ui.println_out("");
    ui.println_out("Next: `jrs build`, then `jrs tree` to compare the resolved graph");
    ui.println_out("against `mvn dependency:tree` / `gradle dependencies`.");
    Ok(exit::SUCCESS)
}

fn print_block(ui: &Ui, title: &str, entries: &[String]) {
    ui.println_out(format!("{title}:"));
    if entries.is_empty() {
        ui.println_out("  (nothing)");
    } else {
        for entry in entries {
            ui.println_out(format!("  - {entry}"));
        }
    }
    ui.println_out("");
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(args).unwrap()
    }

    #[test]
    fn the_command_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn every_command_in_the_spec_exists() {
        for args in [
            vec!["jrs", "build"],
            vec!["jrs", "test"],
            vec!["jrs", "run"],
            vec!["jrs", "package"],
            vec!["jrs", "package", "--fat"],
            vec!["jrs", "clean"],
            vec!["jrs", "tree"],
            vec!["jrs", "update"],
            vec!["jrs", "init"],
            vec!["jrs", "migrate"],
        ] {
            assert!(Cli::try_parse_from(&args).is_ok(), "{args:?} did not parse");
        }
    }

    #[test]
    fn global_flags_work_after_the_subcommand() {
        let cli = parse(&["jrs", "build", "--verbose", "--offline", "--jobs", "2"]);
        assert!(cli.global.verbose);
        assert!(cli.global.offline);
        assert_eq!(cli.global.jobs(), 2);
    }

    #[test]
    fn verbose_and_quiet_are_mutually_exclusive() {
        assert!(Cli::try_parse_from(["jrs", "build", "-v", "-q"]).is_err());
    }

    #[test]
    fn jobs_defaults_to_at_least_four() {
        assert!(parse(&["jrs", "build"]).global.jobs() >= 4);
        assert_eq!(
            parse(&["jrs", "build", "--jobs", "0"]).global.jobs(),
            default_jobs(),
            "a zero job count falls back to the default rather than deadlocking"
        );
    }

    #[test]
    fn program_arguments_come_after_a_double_dash() {
        let cli = parse(&["jrs", "run", "--", "--verbose", "input.txt"]);
        match cli.command {
            Command::Run { args } => assert_eq!(args, vec!["--verbose", "input.txt"]),
            other => panic!("expected run, got {other:?}"),
        }
    }

    #[test]
    fn the_tri_state_flags_parse() {
        let cli = parse(&[
            "jrs",
            "build",
            "--progress",
            "never",
            "--color",
            "always",
            "--charset",
            "ascii",
        ]);
        let opts = cli.global.ui_options();
        assert_eq!(opts.progress, When::Never);
        assert_eq!(opts.color, When::Always);
        assert_eq!(opts.charset, CharsetChoice::Ascii);
    }

    #[test]
    fn an_unknown_flag_value_is_rejected() {
        assert!(Cli::try_parse_from(["jrs", "build", "--progress", "sometimes"]).is_err());
    }

    #[test]
    fn quiet_implies_the_quiet_ui_mode() {
        let opts = parse(&["jrs", "build", "-q"]).global.ui_options();
        assert!(opts.quiet);
        assert_eq!(Ui::new(opts).mode(), ui::Mode::Quiet);
    }

    #[test]
    fn test_takes_a_filter() {
        let cli = parse(&["jrs", "test", "--filter", ".*ServiceTest"]);
        match cli.command {
            Command::Test { filter } => assert_eq!(filter.as_deref(), Some(".*ServiceTest")),
            other => panic!("expected test, got {other:?}"),
        }
    }

    #[test]
    fn migrate_takes_the_flags_the_spec_lists() {
        let cli = parse(&[
            "jrs",
            "migrate",
            "--from",
            "gradle",
            "--dry-run",
            "--force",
            "--path",
            "/p",
        ]);
        match cli.command {
            Command::Migrate {
                from,
                dry_run,
                force,
                path,
            } => {
                assert_eq!(from.as_deref(), Some("gradle"));
                assert!(dry_run);
                assert!(force);
                assert_eq!(path, Some(PathBuf::from("/p")));
            }
            other => panic!("expected migrate, got {other:?}"),
        }
    }
}
