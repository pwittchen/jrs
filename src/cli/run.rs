//! `jrs run`: the application's JVM, and what it is started with.

use std::path::PathBuf;

use super::session::Session;
use crate::error::{JrsError, Result};
use crate::manifest::{self, Hook};
use crate::resolve::Classpath;
use crate::{compile, runner, task, toolchain};

impl Session<'_> {
    pub(super) fn run_command(
        &self,
        args: &[String],
        debug: Option<&runner::DebugAddress>,
    ) -> Result<i32> {
        let launch = self.launch(args, debug)?;
        let code =
            toolchain::run_inherited_in(self.ui, &launch.java, &launch.args, &launch.environment)?;
        if code != 0 {
            self.ui.phase(
                "Finished",
                format!("{} exited with {code}", launch.main_class),
            );
        }
        Ok(code)
    }

    /// Everything `jrs run` does before the program gets the terminal:
    /// build, the `pre-run` hook, the command line, `--timings` and the
    /// `Running` line.
    pub(super) fn launch(
        &self,
        args: &[String],
        debug: Option<&runner::DebugAddress>,
    ) -> Result<Launch> {
        let main_class = self.manifest.require_main_class("run")?.to_string();
        let built = self.build()?;
        self.check_main_class(&main_class)?;
        self.hook(Hook::PreRun)?;
        let toolchain = self.toolchain()?;

        let mut classpath = vec![self.project().classes_dir()];
        classpath.extend(self.upstream_dirs(Classpath::runs));
        classpath.extend(built.resolution.runtime_classpath());
        let mut agents = self.java_agents(
            "run",
            &self.manifest.run.java_agents,
            &built.resolution,
            true,
        )?;
        let environment = self.jvm_environment(
            "run",
            &self.manifest.run.env,
            self.manifest.run.cwd.as_ref(),
        )?;
        if environment.cwd.is_some() {
            // Relative paths would be read from the other directory.
            let absolute = |p: PathBuf| std::path::absolute(&p).unwrap_or(p);
            classpath = classpath.into_iter().map(absolute).collect();
            agents = agents.into_iter().map(absolute).collect();
        }
        let mut jvm_args = runner::jvm_prefix(debug, &agents);
        jvm_args.extend(self.manifest.run.jvm_args.iter().cloned());

        // The program's own run is not a build phase: the report covers what
        // led up to it, and comes before the program gets the terminal.
        self.report_timings("run")?;
        self.ui.phase("Running", &main_class);
        if let Some(debug) = debug {
            self.announce_debugger(debug);
        }
        Ok(Launch {
            java: toolchain.java,
            args: runner::java_args(&jvm_args, &classpath, &main_class, args),
            environment,
            main_class,
        })
    }

    /// A Kotlin `main` at file level compiles to `<File>Kt`, so a `main-class`
    /// naming the file's class instead is the likeliest slip in a Kotlin
    /// project. When there is no such class but there is its `Kt` one, say so
    /// rather than let `java` report a missing class.
    fn check_main_class(&self, main_class: &str) -> Result<()> {
        let classes = self.project().classes_dir();
        let kt = format!("{main_class}Kt");
        if compile::class_file(&classes, main_class).is_file()
            || !compile::class_file(&classes, &kt).is_file()
        {
            return Ok(());
        }
        Err(JrsError::manifest(format!(
            "there is no class `{main_class}` in {}, but there is `{kt}`: a Kotlin `main` \
             function at file level compiles to a class named after its file\n\n\
             set it in {}:\n\n    [project]\n    main-class = \"{kt}\"",
            classes.display(),
            self.manifest.path.display()
        )))
    }

    /// `<section>.env` and `<section>.cwd`, expanded for the JVM `jrs run` or
    /// `jrs test` starts.
    pub(super) fn jvm_environment(
        &self,
        section: &str,
        env: &[(String, manifest::Template)],
        cwd: Option<&manifest::Template>,
    ) -> Result<toolchain::Environment> {
        let vars = if env.is_empty() {
            Vec::new()
        } else {
            task::jvm_env(&self.manifest, env, &self.classpaths()?)?
        };
        let cwd = match cwd {
            None => None,
            Some(template) => {
                let dir = task::jvm_cwd(&self.manifest, template)?;
                if !dir.is_dir() {
                    return Err(JrsError::manifest(format!(
                        "`{section}.cwd` is {}, which is not a directory",
                        dir.display()
                    )));
                }
                Some(dir)
            }
        };
        Ok(toolchain::Environment { cwd, vars })
    }

    /// Say where to attach before a JVM starts waiting for a debugger, since
    /// nothing else happens until one does.
    pub(super) fn announce_debugger(&self, debug: &runner::DebugAddress) {
        self.ui.phase(
            "Debugging",
            format!(
                "attach a debugger to {}; the JVM waits until one does",
                debug.attach_to()
            ),
        );
    }
}

/// The JVM `jrs run` starts: `java`, its arguments, where and with what.
pub(super) struct Launch {
    pub(super) java: PathBuf,
    pub(super) args: Vec<String>,
    pub(super) environment: toolchain::Environment,
    pub(super) main_class: String,
}
