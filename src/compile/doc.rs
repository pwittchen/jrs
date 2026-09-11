//! Documenting Scala and Groovy sources with their own tools (SPEC §7.4):
//! Scaladoc and Groovydoc, run on the project's JDK as `java @argfile`, the
//! way their compilers are (`JVM_LANGUAGES.md` §9).
//!
//! Each tool reads what its language's compiler reads. Scaladoc 2 and
//! Groovydoc read the sources, the Java ones included, and document both;
//! Scala 3's scaladoc reads the `TASTy` in the compiled classes, so the build
//! runs before it and Java sources, which have no `TASTy`, are left out.
//!
//! What running them by hand found, and what jrs does about it:
//!
//! - Both scaladocs refuse an output directory that does not exist yet, so it
//!   is created, emptied, first — as it is for `javadoc`.
//! - Groovydoc's `-classpath` takes no value (its launch script consumes it),
//!   so a value throws its option parsing off. It needs no classpath: it only
//!   parses. It does need `-javaVersion`, or it fails on a `false` default;
//!   Groovy 4's parses Java up to 21.
//! - Groovydoc takes a source's package from its path relative to
//!   `-sourcepath`. An absolute path puts every class in `DefaultPackage`, so
//!   jrs passes each source relative to its root.
//! - Groovydoc skips a source it cannot parse with `ignored due to parsing
//!   exception` and still exits 0. jrs fails the run on those words.

use std::path::{Path, PathBuf};

use super::lang::{DocTool, Language, is_scala3};
use super::render_argfile;
use crate::error::{IoResultExt, JrsError, Result};
use crate::toolchain::{Toolchain, run_captured};
use crate::ui::{Stream, Ui};

/// What Groovydoc says when it skips a source it could not parse.
const GROOVYDOC_SKIPPED: &str = "ignored due to parsing exception";

/// One Scaladoc or Groovydoc run over the main sources.
#[derive(Debug)]
pub struct ForeignDoc {
    pub language: Language,
    /// The language's version: Scala 2 and 3 document differently.
    pub version: String,
    pub tool: DocTool,
    /// The tool's own classpath: its graph, or for Scaladoc 2 the compiler's.
    pub tool_classpath: Vec<PathBuf>,
    /// `<lang>.compiler-jvm-args`, for the tool's JVM as for the compiler's.
    pub jvm_args: Vec<String>,
    /// The sources documented. Scala 3's scaladoc reaches them through the
    /// classes' `TASTy`, so for it they are the Scala sources alone.
    pub sources: Vec<PathBuf>,
    /// Every root the sources can be under: Groovydoc's `-sourcepath`.
    pub roots: Vec<PathBuf>,
    /// `target/classes`, which Scala 3's scaladoc reads.
    pub classes_dir: PathBuf,
    /// The main compile classpath: what the documented code links against.
    pub classpath: Vec<PathBuf>,
    pub output_dir: PathBuf,
    pub release: u32,
    pub encoding: String,
    /// The project's name and version, for the titles.
    pub name: String,
    pub project_version: String,
    pub work_dir: PathBuf,
}

impl ForeignDoc {
    /// Whether the tool reads compiled classes rather than sources.
    #[must_use]
    pub fn reads_classes(language: Language, version: &str) -> bool {
        language == Language::Scala && is_scala3(version)
    }

    /// The tool's flags and what it reads, in the order it sees them.
    #[must_use]
    pub fn args(&self) -> Vec<String> {
        let out = self.output_dir.display().to_string();
        let classpath = (!self.classpath.is_empty()).then(|| Toolchain::classpath(&self.classpath));
        let mut args = vec!["-d".to_string(), out];
        match self.language {
            Language::Scala if is_scala3(&self.version) => {
                if let Some(cp) = classpath {
                    args.extend(["-classpath".into(), cp]);
                }
                args.extend([
                    "-project".into(),
                    self.name.clone(),
                    "-project-version".into(),
                    self.project_version.clone(),
                    self.classes_dir.display().to_string(),
                ]);
            }
            Language::Scala => {
                if let Some(cp) = classpath {
                    args.extend(["-classpath".into(), cp]);
                }
                args.extend([
                    "-encoding".into(),
                    self.encoding.clone(),
                    "-doc-title".into(),
                    self.name.clone(),
                    "-doc-version".into(),
                    self.project_version.clone(),
                ]);
                args.extend(self.sources.iter().map(|p| p.display().to_string()));
            }
            Language::Groovy => {
                let title = format!("{} {}", self.name, self.project_version);
                let roots: Vec<PathBuf> =
                    self.roots.iter().filter(|r| r.is_dir()).cloned().collect();
                args.extend([
                    format!("-javaVersion=JAVA_{}", self.release),
                    "-fileEncoding=UTF-8".into(),
                    "-charset=UTF-8".into(),
                    format!("-windowtitle={title}"),
                    format!("-doctitle={title}"),
                    // Neither a date nor Groovy's version in the pages, so a
                    // Javadoc jar of them is byte-identical build to build.
                    "-notimestamp".into(),
                    "-noversionstamp".into(),
                    "-quiet".into(),
                    format!("-sourcepath={}", Toolchain::classpath(&roots)),
                ]);
                args.extend(
                    self.sources
                        .iter()
                        .map(|p| relative_to_root(p, &roots).display().to_string()),
                );
            }
            Language::Java | Language::Kotlin => {}
        }
        args
    }
}

/// `com/example/Greeter.groovy` for a source under one of `roots`.
fn relative_to_root(source: &Path, roots: &[PathBuf]) -> PathBuf {
    roots
        .iter()
        .find_map(|root| source.strip_prefix(root).ok())
        .map_or_else(|| source.to_path_buf(), Path::to_path_buf)
}

/// Generate the documentation with the language's own tool: an argfile in,
/// output passed through verbatim, into an emptied `output_dir`.
///
/// # Errors
///
/// `JrsError::Build` if the tool cannot be started, reports a failure, or
/// (Groovydoc) skipped a source it could not parse; `JrsError::Io` if the
/// output or work directory or the argfile cannot be written.
pub fn document(toolchain: &Toolchain, unit: &ForeignDoc, ui: &Ui) -> Result<()> {
    if unit.output_dir.exists() {
        std::fs::remove_dir_all(&unit.output_dir).path(&unit.output_dir)?;
    }
    std::fs::create_dir_all(&unit.output_dir).path(&unit.output_dir)?;
    std::fs::create_dir_all(&unit.work_dir).path(&unit.work_dir)?;

    let mut args = unit.jvm_args.clone();
    args.extend([
        "-cp".to_string(),
        Toolchain::classpath(&unit.tool_classpath),
        unit.tool.main_class.to_string(),
    ]);
    args.extend(unit.args());
    let argfile = unit.work_dir.join(format!("{}.args", unit.tool.name));
    std::fs::write(&argfile, render_argfile(&args, &[])).path(&argfile)?;
    let output = run_captured(ui, &toolchain.java, &[format!("@{}", argfile.display())])?;

    if !output.stderr.trim().is_empty() {
        ui.passthrough(Stream::Err, output.stderr.trim_end());
    }
    if !output.stdout.trim().is_empty() {
        ui.passthrough(Stream::Err, output.stdout.trim_end());
    }
    let text = format!("{}\n{}", output.stdout, output.stderr);
    let what = format!("{} source files", unit.sources.len());
    if !output.ok() {
        let mut message = format!("{} failed ({what})", unit.tool.name);
        if let Some(hint) = unit
            .language
            .release_hint(&unit.version, unit.release, &text)
        {
            message.push_str("\n\n");
            message.push_str(&hint);
        }
        return Err(JrsError::build(message));
    }
    if unit.language == Language::Groovy && text.contains(GROOVYDOC_SKIPPED) {
        return Err(JrsError::build(format!(
            "groovydoc left out a source file it could not parse ({what}); \
             its message is above, and `jrs build` shows the compiler's"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolve::coord::Coord;

    fn unit(language: Language, version: &str) -> ForeignDoc {
        ForeignDoc {
            language,
            version: version.into(),
            tool: language.doc_tool(version).unwrap(),
            tool_classpath: vec![PathBuf::from("/c/tool.jar")],
            jvm_args: Vec::new(),
            sources: vec![
                PathBuf::from("/p/src/main/groovy/com/example/Greeter.groovy"),
                PathBuf::from("/p/src/main/java/com/example/Util.java"),
            ],
            roots: vec![
                PathBuf::from("/p/src/main/java"),
                PathBuf::from("/p/src/main/groovy"),
            ],
            classes_dir: PathBuf::from("/p/target/classes"),
            classpath: vec![PathBuf::from("/c/lib.jar")],
            output_dir: PathBuf::from("/p/target/doc"),
            release: 21,
            encoding: "UTF-8".into(),
            name: "app".into(),
            project_version: "1.0.0".into(),
            work_dir: PathBuf::from("/p/target/.jrs"),
        }
    }

    fn pairs(args: &[String]) -> Vec<(&str, &str)> {
        args.windows(2)
            .map(|w| (w[0].as_str(), w[1].as_str()))
            .collect()
    }

    #[test]
    fn scaladoc_3_reads_the_compiled_classes() {
        let args = unit(Language::Scala, "3.9.0").args();
        let p = pairs(&args);
        assert!(p.contains(&("-d", "/p/target/doc")), "{args:?}");
        assert!(p.contains(&("-classpath", "/c/lib.jar")), "{args:?}");
        assert!(p.contains(&("-project", "app")), "{args:?}");
        assert!(p.contains(&("-project-version", "1.0.0")), "{args:?}");
        assert_eq!(args.last().unwrap(), "/p/target/classes");
        assert!(!args.iter().any(|a| a.ends_with(".java")), "{args:?}");
        assert!(ForeignDoc::reads_classes(Language::Scala, "3.3.6"));
        assert!(!ForeignDoc::reads_classes(Language::Scala, "2.13.18"));
    }

    #[test]
    fn scaladoc_2_reads_the_sources_java_included() {
        let args = unit(Language::Scala, "2.13.18").args();
        let p = pairs(&args);
        assert!(p.contains(&("-doc-title", "app")), "{args:?}");
        assert!(p.contains(&("-doc-version", "1.0.0")), "{args:?}");
        assert!(p.contains(&("-encoding", "UTF-8")), "{args:?}");
        assert!(args.iter().any(|a| a.ends_with("Util.java")), "{args:?}");
    }

    #[test]
    fn groovydoc_gets_sources_relative_to_their_roots_and_no_classpath() {
        let tree = std::env::temp_dir().join(format!("jrs-doc-roots-{}", std::process::id()));
        let java = tree.join("src/main/java");
        let groovy = tree.join("src/main/groovy");
        std::fs::create_dir_all(&java).unwrap();
        std::fs::create_dir_all(&groovy).unwrap();
        let mut u = unit(Language::Groovy, "5.1.2");
        u.roots = vec![java.clone(), groovy.clone(), tree.join("src/main/missing")];
        u.sources = vec![
            groovy.join("com/example/Greeter.groovy"),
            java.join("com/example/Util.java"),
        ];
        let args = u.args();
        std::fs::remove_dir_all(&tree).unwrap();

        assert!(
            !args.iter().any(|a| a == "-classpath" || a == "-cp"),
            "{args:?}"
        );
        assert!(
            args.contains(&"-javaVersion=JAVA_21".to_string()),
            "{args:?}"
        );
        assert!(args.contains(&"-notimestamp".to_string()), "{args:?}");
        assert!(
            args.contains(&"-windowtitle=app 1.0.0".to_string()),
            "{args:?}"
        );
        assert!(
            args.contains(&format!(
                "-sourcepath={}",
                Toolchain::classpath(&[java, groovy])
            )),
            "a missing root is left off: {args:?}"
        );
        let n = args.len();
        assert_eq!(
            &args[n - 2..],
            &[
                Path::new("com/example/Greeter.groovy")
                    .display()
                    .to_string(),
                Path::new("com/example/Util.java").display().to_string(),
            ]
        );
    }

    #[test]
    fn a_source_under_no_root_keeps_its_path() {
        assert_eq!(
            relative_to_root(Path::new("/elsewhere/A.groovy"), &[PathBuf::from("/p/src")]),
            PathBuf::from("/elsewhere/A.groovy")
        );
    }

    #[test]
    fn the_tool_is_named_after_its_graph() {
        let u = unit(Language::Groovy, "4.0.28");
        assert_eq!(
            u.tool.roots,
            vec![Coord::new(
                "org.apache.groovy",
                "groovy-groovydoc",
                "4.0.28"
            )]
        );
        assert_eq!(u.tool.name, "groovydoc");
    }
}
