//! Formatters and checkers as tasks: PMD, Checkstyle, `SpotBugs`,
//! google-java-format and ktfmt, each a `main` task over a tool graph of its
//! own (TASKS.md §8), so nothing needs installing and nothing runs inside
//! jrs.
//!
//! `jrs init --check` and `--format` scaffold them; `jrs migrate` writes them
//! in place of the Gradle and Maven plugins that ran them. A checker is meant
//! for the `post-compile` hook, which is where both put it; a formatter's
//! `format` task rewrites the sources and is only ever run by hand, and its
//! `format-check` task is the one a hook runs.

use crate::error::{JrsError, Result};
use crate::manifest::{Action, Dependency, TaskDef, Template};

/// The tools there are tasks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Pmd,
    Checkstyle,
    SpotBugs,
    GoogleJavaFormat,
    Ktfmt,
}

impl Tool {
    /// The release a task pins when nothing says which: the newest that
    /// runs on JDK 17, the oldest JDK jrs drives, since a tool's JVM is the
    /// project's. Checkstyle 13 and google-java-format 1.36 need JDK 21.
    #[must_use]
    pub fn default_version(self) -> &'static str {
        match self {
            Tool::Pmd => "7.27.0",
            Tool::Checkstyle => "12.3.1",
            Tool::SpotBugs => "4.10.4",
            Tool::GoogleJavaFormat => "1.35.0",
            Tool::Ktfmt => "0.64",
        }
    }

    /// The name the tool goes by.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Tool::Pmd => "PMD",
            Tool::Checkstyle => "Checkstyle",
            Tool::SpotBugs => "SpotBugs",
            Tool::GoogleJavaFormat => "google-java-format",
            Tool::Ktfmt => "ktfmt",
        }
    }
}

/// A task and whether it belongs in the `post-compile` hook.
#[derive(Debug, Clone)]
pub struct Scaffolded {
    pub task: TaskDef,
    pub hooked: bool,
}

fn templates(raw: &[&str]) -> Result<Vec<Template>> {
    raw.iter()
        .map(|r| Template::parse(r).map_err(JrsError::manifest))
        .collect()
}

fn main_task(
    name: &str,
    description: String,
    main: &str,
    args: Vec<Template>,
    dependencies: Vec<Dependency>,
) -> TaskDef {
    TaskDef {
        name: name.to_string(),
        description: Some(description),
        action: Some(Action::Main(main.to_string())),
        args,
        depends_on: Vec::new(),
        env: Vec::new(),
        cwd: None,
        inputs: Vec::new(),
        outputs: Vec::new(),
        source_outputs: Vec::new(),
        resource_outputs: Vec::new(),
        cache: false,
        dependencies,
    }
}

/// PMD over `dirs` with `rulesets`, as task `name`. PMD's cache goes under
/// the target directory.
///
/// # Errors
///
/// [`JrsError::Manifest`] for a directory or rule set that is not a valid
/// template.
pub fn pmd(name: &str, version: &str, dirs: &[String], rulesets: &str) -> Result<Scaffolded> {
    let mut args = vec!["check", "--no-progress", "--rulesets", rulesets];
    for dir in dirs {
        args.extend(["--dir", dir]);
    }
    args.extend(["--cache", "{target}/pmd.cache"]);
    let pmd = |artifact| Dependency::new("net.sourceforge.pmd", artifact, version);
    Ok(Scaffolded {
        task: main_task(
            name,
            format!("Check the Java sources with PMD ({rulesets})"),
            "net.sourceforge.pmd.cli.PmdCli",
            templates(&args)?,
            vec![pmd("pmd-cli"), pmd("pmd-java")],
        ),
        hooked: true,
    })
}

/// Checkstyle over `dirs` with `config`: a file, or one of the
/// configurations Checkstyle carries, `/sun_checks.xml` or
/// `/google_checks.xml`.
///
/// # Errors
///
/// [`JrsError::Manifest`] for a directory or configuration that is not a
/// valid template.
pub fn checkstyle(version: &str, config: &str, dirs: &[String]) -> Result<Scaffolded> {
    let mut args = vec!["-c", config];
    args.extend(dirs.iter().map(String::as_str));
    Ok(Scaffolded {
        task: main_task(
            "checkstyle",
            format!("Check the Java sources with Checkstyle ({config})"),
            "com.puppycrawl.tools.checkstyle.Main",
            templates(&args)?,
            vec![Dependency::new(
                "com.puppycrawl.tools",
                "checkstyle",
                version,
            )],
        ),
        hooked: true,
    })
}

/// `SpotBugs` over the compiled main classes, with the compile classpath to
/// resolve what they refer to; it fails when it finds a bug.
///
/// # Errors
///
/// Never, in practice: the arguments are fixed.
pub fn spotbugs(version: &str) -> Result<Scaffolded> {
    Ok(Scaffolded {
        task: main_task(
            "spotbugs",
            "Look for bugs in the compiled classes with SpotBugs".to_string(),
            "edu.umd.cs.findbugs.FindBugs2",
            templates(&[
                "-exitcode",
                "-effort:max",
                "-auxclasspath",
                "{classpath}",
                "{classes}",
            ])?,
            vec![Dependency::new("com.github.spotbugs", "spotbugs", version)],
        ),
        hooked: true,
    })
}

/// What google-java-format's JVM has to be let into: the compiler's
/// internals, which it parses with.
const GJF_EXPORTS: &str = "--add-exports=jdk.compiler/com.sun.tools.javac.api=ALL-UNNAMED \
     --add-exports=jdk.compiler/com.sun.tools.javac.code=ALL-UNNAMED \
     --add-exports=jdk.compiler/com.sun.tools.javac.file=ALL-UNNAMED \
     --add-exports=jdk.compiler/com.sun.tools.javac.parser=ALL-UNNAMED \
     --add-exports=jdk.compiler/com.sun.tools.javac.tree=ALL-UNNAMED \
     --add-exports=jdk.compiler/com.sun.tools.javac.util=ALL-UNNAMED";

/// google-java-format: `format` rewrites the project's sources in place,
/// and `format-check` fails when one is not formatted. `aosp` is its
/// four-space style. It reads the file list from `{sources-argfile}`, and its
/// JVM flags go in through `JDK_JAVA_OPTIONS`, since a `main` task's JVM
/// takes none of its own.
///
/// # Errors
///
/// Never, in practice: the arguments are fixed.
pub fn google_java_format(version: &str, aosp: bool) -> Result<[Scaffolded; 2]> {
    let task = |name: &str, description: &str, mode: &[&str], hooked: bool| -> Result<Scaffolded> {
        let mut args: Vec<&str> = mode.to_vec();
        if aosp {
            args.push("--aosp");
        }
        args.push("@{sources-argfile}");
        let mut task = main_task(
            name,
            description.to_string(),
            "com.google.googlejavaformat.java.Main",
            templates(&args)?,
            vec![Dependency::new(
                "com.google.googlejavaformat",
                "google-java-format",
                version,
            )],
        );
        task.env = vec![(
            "JDK_JAVA_OPTIONS".to_string(),
            Template::literal(GJF_EXPORTS),
        )];
        Ok(Scaffolded { task, hooked })
    };
    Ok([
        task(
            "format",
            "Format the Java sources with google-java-format",
            &["--replace"],
            false,
        )?,
        task(
            "format-check",
            "Fail when a Java source is not formatted as google-java-format would",
            &["--dry-run", "--set-exit-if-changed"],
            true,
        )?,
    ])
}

/// ktfmt over `dirs`: `format` and `format-check`, as for
/// [`google_java_format`]. `style` is ktfmt's own flag, `--google-style` or
/// `--kotlinlang-style`; its default is Meta's.
///
/// # Errors
///
/// [`JrsError::Manifest`] for a directory that is not a valid template.
pub fn ktfmt(version: &str, style: Option<&str>, dirs: &[String]) -> Result<[Scaffolded; 2]> {
    let task = |name: &str, description: &str, mode: &[&str], hooked: bool| -> Result<Scaffolded> {
        let mut args: Vec<&str> = mode.to_vec();
        args.extend(style);
        args.extend(dirs.iter().map(String::as_str));
        Ok(Scaffolded {
            task: main_task(
                name,
                description.to_string(),
                "com.facebook.ktfmt.cli.Main",
                templates(&args)?,
                vec![Dependency::new("com.facebook", "ktfmt", version)],
            ),
            hooked,
        })
    };
    Ok([
        task("format", "Format the Kotlin sources with ktfmt", &[], false)?,
        task(
            "format-check",
            "Fail when a Kotlin source is not formatted as ktfmt would",
            &["--dry-run", "--set-exit-if-changed"],
            true,
        )?,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(task: &TaskDef) -> Vec<String> {
        task.args.iter().map(|t| t.raw.clone()).collect()
    }

    #[test]
    fn each_tool_is_a_main_task_over_a_graph_of_its_own() {
        let pmd = pmd(
            "check",
            "7.0.0",
            &["src/main/java".into()],
            "rulesets/java/quickstart.xml",
        )
        .unwrap();
        assert!(pmd.hooked);
        assert_eq!(
            raw(&pmd.task),
            [
                "check",
                "--no-progress",
                "--rulesets",
                "rulesets/java/quickstart.xml",
                "--dir",
                "src/main/java",
                "--cache",
                "{target}/pmd.cache"
            ]
        );
        assert_eq!(pmd.task.dependencies.len(), 2);

        let style = checkstyle("10.0", "/google_checks.xml", &["src/main/java".into()]).unwrap();
        assert_eq!(
            raw(&style.task),
            ["-c", "/google_checks.xml", "src/main/java"]
        );
        assert_eq!(
            style.task.action,
            Some(Action::Main("com.puppycrawl.tools.checkstyle.Main".into()))
        );

        let bugs = spotbugs("4.9.3").unwrap();
        assert!(raw(&bugs.task).contains(&"{classes}".to_string()));
    }

    #[test]
    fn a_formatter_is_a_format_task_and_a_hooked_check() {
        let [format, check] = google_java_format("1.28.0", true).unwrap();
        assert_eq!(
            (format.task.name.as_str(), format.hooked),
            ("format", false)
        );
        assert_eq!(
            (check.task.name.as_str(), check.hooked),
            ("format-check", true)
        );
        assert_eq!(
            raw(&check.task),
            [
                "--dry-run",
                "--set-exit-if-changed",
                "--aosp",
                "@{sources-argfile}"
            ]
        );
        assert_eq!(check.task.env[0].0, "JDK_JAVA_OPTIONS");
        assert!(check.task.env[0].1.raw.contains("javac.api=ALL-UNNAMED"));

        let [format, _] =
            ktfmt("0.54", Some("--google-style"), &["src/main/kotlin".into()]).unwrap();
        assert_eq!(raw(&format.task), ["--google-style", "src/main/kotlin"]);
    }
}
