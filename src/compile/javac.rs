//! Driving `javac` and `javadoc`.
//!
//! `javac` is the last step of every compile unit, and the only one in a
//! Java-only unit. Its output is passed through verbatim — its diagnostics are
//! already good, and jrs reformatting them would only make them worse.

use std::path::{Path, PathBuf};

use super::share::{self, Share};
use super::{CompileUnit, render_argfile};
use crate::error::{IoResultExt, JrsError, Result};
use crate::toolchain::{CapturedOutput, Toolchain, run_captured};
use crate::ui::{Stream, Ui};

impl CompileUnit {
    /// The flags jrs generates for `javac`, in the order it sees them.
    /// `output_first` puts the unit's own output directory at the head of the
    /// classpath: the classes an earlier step wrote there, or an earlier
    /// build when only some sources are compiled, are what these sources
    /// compile against.
    pub(super) fn javac_flags(&self, output_first: bool) -> Vec<String> {
        let mut args = if let Some(target) = self.target {
            // `--release` already pins both, so an explicit target is only
            // meaningful as the older -source/-target pair.
            vec![
                "-source".to_string(),
                self.release.to_string(),
                "-target".to_string(),
                target.to_string(),
            ]
        } else {
            vec!["--release".to_string(), self.release.to_string()]
        };
        args.extend([
            "-encoding".to_string(),
            self.encoding.clone(),
            "-d".to_string(),
            self.output_dir.display().to_string(),
        ]);
        let mut classpath = Vec::new();
        if output_first {
            classpath.push(self.output_dir.clone());
        }
        classpath.extend(self.classpath.iter().cloned());
        if !classpath.is_empty() {
            args.push("-cp".to_string());
            args.push(Toolchain::classpath(&classpath));
        }
        args.extend(self.extra_args.iter().cloned());
        args
    }
}

/// JVM flags for a `javac` that compiles a few files and exits: C1 alone and
/// the serial collector start faster than the tiered JIT and G1, which a run
/// this short never gets to use.
pub(super) const QUICK_JVM_FLAGS: &[&str] = &["-XX:TieredStopAtLevel=1", "-XX:+UseSerialGC"];

/// Run `javac` over `sources`, one step of `unit`. `quick` starts its JVM
/// with [`QUICK_JVM_FLAGS`], for a run over a handful of files; with
/// `unit.share_dir`, it maps its classes from an archive there.
///
/// # Errors
///
/// `JrsError::Build` if `javac` cannot be started or reports a failure, with
/// `what` saying which sources those were; `JrsError::Io` if the argfile
/// cannot be written.
pub(super) fn run(
    toolchain: &Toolchain,
    unit: &CompileUnit,
    sources: &[PathBuf],
    output_first: bool,
    quick: bool,
    what: &str,
    ui: &Ui,
) -> Result<()> {
    let argfile = unit.work_dir.join(format!("javac-{}.args", unit.label));
    std::fs::write(
        &argfile,
        render_argfile(&unit.javac_flags(output_first), sources),
    )
    .path(&argfile)?;

    // JVM flags reach `javac`'s JVM as `-J` options, which only its command
    // line may hold, never an argfile.
    let quick: &[&str] = if quick { QUICK_JVM_FLAGS } else { &[] };
    let share = unit
        .share_dir
        .as_deref()
        .and_then(|dir| Share::new(dir, toolchain, "jdk.compiler", &[], &[]));
    let output = share::run(share, CapturedOutput::ok, |shared| {
        let mut args: Vec<String> = quick
            .iter()
            .map(|f| (*f).to_string())
            .chain(shared.iter().cloned())
            .map(|f| format!("-J{f}"))
            .collect();
        args.push(format!("@{}", argfile.display()));
        run_captured(ui, &toolchain.javac, &args)
    })?;

    // The live region comes down before any diagnostic reaches the terminal.
    if !output.stderr.trim().is_empty() {
        ui.passthrough(Stream::Err, output.stderr.trim_end());
    }
    if !output.stdout.trim().is_empty() {
        ui.passthrough(Stream::Err, output.stdout.trim_end());
    }
    if !output.ok() {
        return Err(JrsError::build(format!("compilation failed ({what})")));
    }
    Ok(())
}

/// One `javadoc` run over the main sources.
#[derive(Debug)]
pub struct DocUnit {
    pub sources: Vec<PathBuf>,
    pub output_dir: PathBuf,
    pub classpath: Vec<PathBuf>,
    pub release: u32,
    pub encoding: String,
    /// `java.javadoc-args`, appended verbatim after jrs's own flags.
    pub extra_args: Vec<String>,
    /// `my-app 1.0.0`, for the page and window titles.
    pub title: String,
    pub work_dir: PathBuf,
}

impl DocUnit {
    fn flags(&self) -> Vec<String> {
        let mut args = vec![
            "--release".to_string(),
            self.release.to_string(),
            "-encoding".to_string(),
            self.encoding.clone(),
            "-docencoding".to_string(),
            "UTF-8".to_string(),
            "-charset".to_string(),
            "UTF-8".to_string(),
            "-d".to_string(),
            self.output_dir.display().to_string(),
            "-doctitle".to_string(),
            self.title.clone(),
            "-windowtitle".to_string(),
            self.title.clone(),
            // Progress chatter off; warnings and errors still come through.
            "-quiet".to_string(),
            // No date in the pages, so a Javadoc jar is byte-identical from
            // build to build (SPEC §9.1).
            "-notimestamp".to_string(),
        ];
        if !self.classpath.is_empty() {
            args.push("-cp".to_string());
            args.push(Toolchain::classpath(&self.classpath));
        }
        args.extend(self.extra_args.iter().cloned());
        args
    }
}

/// Generate API documentation with `javadoc`, driven like `javac`: an argfile
/// in, output passed through verbatim.
///
/// The output directory is emptied first, so a class that was deleted does not
/// keep its page. Nothing is skipped on a rerun: `javadoc` is fast next to the
/// question of which pages a change touched.
///
/// # Errors
///
/// `JrsError::Build` if `javadoc` cannot be started or reports a failure;
/// `JrsError::Io` if the output or work directory or the argfile cannot be
/// written.
pub fn javadoc(javadoc: &Path, unit: &DocUnit, ui: &Ui) -> Result<()> {
    if unit.output_dir.exists() {
        std::fs::remove_dir_all(&unit.output_dir).path(&unit.output_dir)?;
    }
    std::fs::create_dir_all(&unit.output_dir).path(&unit.output_dir)?;
    std::fs::create_dir_all(&unit.work_dir).path(&unit.work_dir)?;

    let argfile = unit.work_dir.join("javadoc.args");
    std::fs::write(&argfile, render_argfile(&unit.flags(), &unit.sources)).path(&argfile)?;
    let output = run_captured(ui, javadoc, &[format!("@{}", argfile.display())])?;
    if !output.stderr.trim().is_empty() {
        ui.passthrough(Stream::Err, output.stderr.trim_end());
    }
    if !output.stdout.trim().is_empty() {
        ui.passthrough(Stream::Err, output.stdout.trim_end());
    }
    if !output.ok() {
        return Err(JrsError::build(format!(
            "javadoc failed ({} source files)",
            unit.sources.len()
        )));
    }
    Ok(())
}
