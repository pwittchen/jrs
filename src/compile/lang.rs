//! The languages a compile unit can hold besides Java (`JVM_LANGUAGES.md`).
//!
//! What jrs knows about each one is plain data, matched on an enum: the file
//! extension, the default source roots, the compiler resolved from Maven
//! Central and run on the project's JDK, the runtime library a project gets
//! without declaring it, and the flags jrs generates. There is no registry
//! and no trait object; a fourth language would be a fourth arm in each match.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::path::Path;

use super::{CompileUnit, ForeignCompiler};
use crate::manifest::Manifest;
use crate::resolve::Resolution;
use crate::resolve::coord::{Coord, compare_versions};
use crate::toolchain::Toolchain;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Language {
    Java,
    Kotlin,
    Scala,
    Groovy,
}

/// A compiler as an internal tool: the artifact its graph is resolved from,
/// and the class `java` starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Compiler {
    pub coord: Coord,
    pub main_class: &'static str,
}

/// The tool `jrs doc` documents a language with, as an internal tool: the
/// roots of its graph and the class `java` starts (SPEC §7.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocTool {
    /// `scaladoc` or `groovydoc`, for messages and the argfile's name.
    pub name: &'static str,
    /// Empty when the tool ships in the compiler and runs on its graph.
    pub roots: Vec<Coord>,
    pub main_class: &'static str,
}

impl Language {
    /// Every language but Java, in the order jrs.toml's tables are read and
    /// written.
    pub const FOREIGN: [Language; 3] = [Language::Kotlin, Language::Scala, Language::Groovy];

    /// The manifest table's name, and the directory under `src/main`.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Language::Java => "java",
            Language::Kotlin => "kotlin",
            Language::Scala => "scala",
            Language::Groovy => "groovy",
        }
    }

    /// For messages: `Kotlin`.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Language::Java => "Java",
            Language::Kotlin => "Kotlin",
            Language::Scala => "Scala",
            Language::Groovy => "Groovy",
        }
    }

    #[must_use]
    pub fn extension(self) -> &'static str {
        match self {
            Language::Java => "java",
            Language::Kotlin => "kt",
            Language::Scala => "scala",
            Language::Groovy => "groovy",
        }
    }

    /// A source file's language, by its extension. A `.kts` script is never
    /// a source.
    #[must_use]
    pub fn of(path: &Path) -> Option<Language> {
        let extension = path.extension()?.to_str()?;
        [Language::Java]
            .into_iter()
            .chain(Language::FOREIGN)
            .find(|l| l.extension() == extension)
    }

    /// `kotlinc`: the compiler's usual name, for argfiles and manifest keys.
    #[must_use]
    pub fn compiler_name(self) -> &'static str {
        match self {
            Language::Java => "javac",
            Language::Kotlin => "kotlinc",
            Language::Scala => "scalac",
            Language::Groovy => "groovyc",
        }
    }

    /// The key for flags appended verbatim to the compiler: `kotlinc-args`.
    #[must_use]
    pub fn args_key(self) -> String {
        format!("{}-args", self.compiler_name())
    }

    /// The name of the compiler's graph in `jrs.lock`'s `[[tool]]` blocks,
    /// and for `jrs tree --tool`.
    #[must_use]
    pub fn tool_name(self) -> String {
        format!("{}-compiler", self.key())
    }

    /// The language whose compiler `jrs.lock` names `name`.
    #[must_use]
    pub fn from_tool_name(name: &str) -> Option<Language> {
        Language::FOREIGN
            .into_iter()
            .find(|l| l.tool_name() == name)
    }

    /// The version `jrs init --lang` pins, and the one suggested to a project
    /// with sources in a language it has not turned on. Java has none: its
    /// compiler is the JDK.
    #[must_use]
    pub fn starter_version(self) -> Option<&'static str> {
        match self {
            Language::Java => None,
            Language::Kotlin => Some("2.4.20"),
            Language::Scala => Some("3.9.0"),
            Language::Groovy => Some("5.1.2"),
        }
    }

    /// Whether jrs drives this compiler version. The floors keep the flag
    /// tables small (`JVM_LANGUAGES.md` §4.1).
    ///
    /// # Errors
    ///
    /// The minimum, and why, when `version` is older.
    pub fn check_version(self, version: &str) -> std::result::Result<(), String> {
        let at_least = |minimum: &str| compare_versions(version, minimum) != Ordering::Less;
        match self {
            Language::Java => Ok(()),
            Language::Kotlin if at_least("2.0") => Ok(()),
            Language::Kotlin => Err("jrs needs Kotlin 2.0 or newer, the K2 compiler".to_string()),
            Language::Scala if is_scala3(version) && at_least("3.3") => Ok(()),
            Language::Scala if version.starts_with("2.13.") && at_least("2.13.9") => Ok(()),
            Language::Scala => Err(
                "jrs needs Scala 2.13.9 or newer on the 2.13 line, or Scala 3.3 or newer; \
                 Scala 2.12 is not supported"
                    .to_string(),
            ),
            Language::Groovy if at_least("4.0") => Ok(()),
            Language::Groovy => Err(
                "jrs needs Groovy 4.0 or newer, published under `org.apache.groovy`".to_string(),
            ),
        }
    }

    /// The compiler at `version`. Scala 2 and 3 are different artifacts with
    /// different entry points; Groovy's compiler is in its core jar.
    #[must_use]
    pub fn compiler(self, version: &str) -> Option<Compiler> {
        let (group, artifact, main_class) = match self {
            Language::Java => return None,
            Language::Kotlin => (
                "org.jetbrains.kotlin",
                "kotlin-compiler-embeddable",
                "org.jetbrains.kotlin.cli.jvm.K2JVMCompiler",
            ),
            Language::Scala if is_scala3(version) => (
                "org.scala-lang",
                "scala3-compiler_3",
                "dotty.tools.dotc.Main",
            ),
            Language::Scala => ("org.scala-lang", "scala-compiler", "scala.tools.nsc.Main"),
            Language::Groovy => (
                "org.apache.groovy",
                "groovy",
                "org.codehaus.groovy.tools.FileSystemCompiler",
            ),
        };
        Some(Compiler {
            coord: Coord::new(group, artifact, version),
            main_class,
        })
    }

    /// The tool that documents this language's sources, at the compiler's
    /// version. Scaladoc 2 is in the compiler; Scala 3's is an artifact of
    /// its own that reads the compiled classes' TASTy, and Groovydoc is a
    /// small graph beside Groovy. Kotlin has none: Dokka is a plugin host with
    /// a configuration of its own (`JVM_LANGUAGES.md` §14.4).
    ///
    /// A table per version line, like the runtime libraries. Scaladoc 3.9
    /// moved to jackson-databind 3, which needs `jackson-annotations` 2.21,
    /// while the `liqp` it also depends on asks for 2.13 one level nearer the
    /// root. sbt resolves highest-wins and never sees the clash; nearest-wins
    /// picks 2.13 and scaladoc fails to start. So from 3.9 on, jrs declares
    /// the newer one beside it, the way a project pins a transitive version.
    #[must_use]
    pub fn doc_tool(self, version: &str) -> Option<DocTool> {
        match self {
            Language::Java | Language::Kotlin => None,
            Language::Scala if is_scala3(version) => {
                let mut roots = vec![Coord::new("org.scala-lang", "scaladoc_3", version)];
                if compare_versions(version, "3.9") != Ordering::Less {
                    roots.push(Coord::new(
                        "com.fasterxml.jackson.core",
                        "jackson-annotations",
                        "2.21",
                    ));
                }
                Some(DocTool {
                    name: "scaladoc",
                    roots,
                    main_class: "dotty.tools.scaladoc.Main",
                })
            }
            Language::Scala => Some(DocTool {
                name: "scaladoc",
                roots: Vec::new(),
                main_class: "scala.tools.nsc.ScalaDoc",
            }),
            Language::Groovy => Some(DocTool {
                name: "groovydoc",
                roots: vec![Coord::new("org.apache.groovy", "groovy-groovydoc", version)],
                main_class: "org.codehaus.groovy.tools.groovydoc.Main",
            }),
        }
    }

    /// The runtime library code in this language links against, as
    /// `(group, artifact)`: an implied dependency at the compiler's version.
    ///
    /// A table per version line, not a formula. Scala 3.8 moved the standard
    /// library into `scala-library` 3.x and left `scala3-library_3` as a shim
    /// that depends on it, so from 3.8 on both are implied: the shim at the
    /// compiler's version keeps an older `scala3-library_3` that a library
    /// brings in from landing beside the new one.
    #[must_use]
    pub fn runtime_libraries(self, version: &str) -> Vec<(&'static str, &'static str)> {
        match self {
            Language::Java => Vec::new(),
            Language::Kotlin => vec![("org.jetbrains.kotlin", "kotlin-stdlib")],
            Language::Scala if !is_scala3(version) => vec![("org.scala-lang", "scala-library")],
            Language::Scala if compare_versions(version, "3.8") != Ordering::Less => vec![
                ("org.scala-lang", "scala3-library_3"),
                ("org.scala-lang", "scala-library"),
            ],
            Language::Scala => vec![("org.scala-lang", "scala3-library_3")],
            Language::Groovy => vec![("org.apache.groovy", "groovy")],
        }
    }

    /// The line jrs adds under a compiler's or doc tool's error when it does
    /// not know the Java release it was asked for. The tool's own message has
    /// been passed through; jrs does not clamp the release silently.
    #[must_use]
    pub fn release_hint(self, version: &str, release: u32, output: &str) -> Option<String> {
        let unknown = match self {
            Language::Java => false,
            Language::Kotlin => output.contains("unknown JVM target version"),
            // `'25' is not a valid choice for '-release'` on 2.13, and the
            // same words about `-java-output-version` on 3.
            Language::Scala => output.contains("is not a valid choice for"),
            // groovyc's, then groovydoc's: Groovy 4's parses Java up to 21.
            Language::Groovy => {
                (output.contains("Bytecode version") && output.contains("is not supported"))
                    || output.contains("Unsupported Java Version")
            }
        };
        unknown.then(|| {
            format!(
                "{} {version} does not know Java {release}: lower `java.source`, or raise `{}.version`",
                self.name(),
                self.key()
            )
        })
    }
}

impl std::fmt::Display for Language {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// Scala 3 and Scala 2 are different compilers with different flags.
#[must_use]
pub fn is_scala3(version: &str) -> bool {
    version.split('.').next() == Some("3")
}

/// The cross-build suffix a Scala library of this line carries: `_3` or
/// `_2.13`.
#[must_use]
pub fn scala_suffix(version: &str) -> &'static str {
    if is_scala3(version) { "_3" } else { "_2.13" }
}

/// `_2.13` of `cats-core_2.13`: the Scala line a cross-built artifact is for.
#[must_use]
pub fn cross_build_suffix(artifact: &str) -> Option<&str> {
    let (_, suffix) = artifact.rsplit_once('_')?;
    matches!(suffix, "2.11" | "2.12" | "2.13" | "3").then_some(suffix)
}

/// A release as the JVM-language compilers spell it: `1.8`, then `9`, `17`...
fn java_version(release: u32) -> String {
    if release <= 8 {
        format!("1.{release}")
    } else {
        release.to_string()
    }
}

// ---- flags -----------------------------------------------------------------

/// The `java` flags for the compiler's own JVM: `compiler-jvm-args`, and for
/// Groovy the bytecode level of the classes it writes, which it reads from a
/// system property rather than a flag.
#[must_use]
pub fn jvm_flags(unit: &CompileUnit, compiler: &ForeignCompiler) -> Vec<String> {
    let mut flags = compiler.jvm_args.clone();
    if compiler.language == Language::Groovy {
        flags.push(format!(
            "-Dgroovy.target.bytecode={}",
            java_version(unit.target.unwrap_or(unit.release))
        ));
    }
    flags
}

/// The flags jrs generates for the unit's compiler, then `<lang>.<tool>-args`
/// verbatim. `joint` says whether the unit has Java sources too, which only
/// Groovy's flags depend on.
///
/// Colour is not in here: it changes what the terminal shows, not what is
/// compiled, so it stays out of the fingerprint and is added by the caller.
///
/// # Errors
///
/// A message naming the argument, when a `java.javac-args` entry cannot be
/// handed to Groovy's joint `javac` (see [`groovy_javac_args`]).
pub fn flags(
    unit: &CompileUnit,
    compiler: &ForeignCompiler,
    joint: bool,
) -> std::result::Result<Vec<String>, String> {
    let out = unit.output_dir.display().to_string();
    let classpath = (!unit.classpath.is_empty()).then(|| Toolchain::classpath(&unit.classpath));
    let mut args: Vec<String> = Vec::new();
    match compiler.language {
        Language::Java => {}
        Language::Kotlin => {
            args.extend(["-d".into(), out]);
            if let Some(cp) = classpath {
                args.extend(["-classpath".into(), cp]);
            }
            // The stdlib is a resolved dependency like any other. Without
            // these, kotlinc adds the one from its own distribution, which
            // need not be the one the program runs with.
            args.extend(["-no-stdlib".into(), "-no-reflect".into()]);
            args.extend([
                "-jvm-target".into(),
                java_version(unit.target.unwrap_or(unit.release)),
            ]);
            if unit.target.is_none() {
                args.push(format!("-Xjdk-release={}", java_version(unit.release)));
            }
            args.extend(["-module-name".into(), compiler.module_name.clone()]);
            if !compiler.friend_paths.is_empty() {
                let friends: Vec<String> = compiler
                    .friend_paths
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect();
                args.push(format!("-Xfriend-paths={}", friends.join(",")));
            }
        }
        Language::Scala => {
            args.extend(["-d".into(), out]);
            if let Some(cp) = classpath {
                args.extend(["-classpath".into(), cp]);
            }
            args.extend(["-encoding".into(), unit.encoding.clone()]);
            // Scala 3 renamed `-release` to `-java-output-version`; 3.3 has
            // the new name already, and 2.13 only the old one.
            let release = if is_scala3(&compiler.version) {
                "-java-output-version"
            } else {
                "-release"
            };
            args.extend([release.into(), unit.release.to_string()]);
        }
        Language::Groovy => {
            // groovyc wants its classpath first.
            if let Some(cp) = classpath {
                args.extend(["-cp".into(), cp]);
            }
            args.extend(["-d".into(), out]);
            args.push(format!("--encoding={}", unit.encoding));
            if joint {
                args.push("-j".into());
                match unit.target {
                    Some(target) => args.extend([
                        format!("-J=source={}", unit.release),
                        format!("-J=target={target}"),
                    ]),
                    // groovyc prepends a `-`, so this reaches javac as
                    // `--release <n>`.
                    None => args.push(format!("-J=-release={}", unit.release)),
                }
                args.extend(groovy_javac_args(&unit.extra_args)?);
            }
        }
    }
    args.extend(compiler.extra_args.iter().cloned());
    Ok(args)
}

/// The flag that turns a compiler's colour off, for a terminal that has none.
/// Only Scala 3 colours output it is not printing to a terminal.
#[must_use]
pub fn no_color_flag(compiler: &ForeignCompiler) -> Option<&'static str> {
    (compiler.language == Language::Scala && is_scala3(&compiler.version)).then_some("-color:never")
}

/// `javac` options whose value is the next argument. groovyc hands javac its
/// options one at a time, so these are the pairs it has to be told about.
const JAVAC_VALUE_OPTIONS: &[&str] = &[
    "-classpath",
    "-cp",
    "--class-path",
    "-bootclasspath",
    "--boot-class-path",
    "-d",
    "-s",
    "-h",
    "-encoding",
    "-source",
    "--source",
    "-target",
    "--target",
    "--release",
    "-processor",
    "-processorpath",
    "--processor-path",
    "--processor-module-path",
    "-sourcepath",
    "--source-path",
    "-extdirs",
    "-endorseddirs",
    "-p",
    "--module-path",
    "--upgrade-module-path",
    "--system",
    "--add-modules",
    "--limit-modules",
    "--add-exports",
    "--add-reads",
    "--patch-module",
    "-m",
    "--module",
    "--module-source-path",
    "--module-version",
    "--default-module-for-created-files",
    "-Xmaxerrs",
    "-Xmaxwarns",
    "-Xstdout",
];

/// `java.javac-args` for the `javac` that groovyc runs in joint compilation.
///
/// groovyc takes a single-token flag as `-F=<flag>` and a `-name value` pair as
/// `-J=name=value`, and puts the `-` back itself. A `--name value` pair
/// becomes the one token `--name=value`, which javac reads the same way.
///
/// # Errors
///
/// A message naming the argument that fits neither shape: a value with no
/// flag jrs knows to take one, or such a flag with no value after it. jrs does
/// not guess which flag a stray value belongs to.
pub fn groovy_javac_args(args: &[String]) -> std::result::Result<Vec<String>, String> {
    let mut out = Vec::with_capacity(args.len());
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if JAVAC_VALUE_OPTIONS.contains(&arg.as_str()) {
            let value = iter.next().ok_or_else(|| {
                format!("`{arg}` needs a value after it, to be handed to Groovy's joint javac")
            })?;
            let name = &arg[1..];
            if name.starts_with('-') {
                out.push(format!("-F={name}={value}"));
            } else {
                out.push(format!("-J={name}={value}"));
            }
        } else if let Some(flag) = arg.strip_prefix('-').filter(|f| !f.is_empty()) {
            out.push(format!("-F={flag}"));
        } else {
            return Err(format!(
                "`{arg}` is not a flag, nor the value of one jrs knows to take a value, so jrs \
                 cannot hand it to the javac Groovy's joint compilation runs; write the flag and \
                 its value as one argument (`--flag=value`), or leave it out"
            ));
        }
    }
    Ok(out)
}

// ---- resolution warnings -----------------------------------------------------

/// What a resolved graph gets wrong about the languages a project uses:
/// a Scala 2 library newer than its compiler, two Scala lines' builds of one
/// library, and the Kotlin 1.x `-jdk7`/`-jdk8` stdlib extensions, whose
/// classes the Kotlin 2 stdlib already has.
///
/// Warnings, not errors, and nothing is aligned implicitly: that would be a
/// second mediation rule beside nearest-wins (SPEC §8.2).
#[must_use]
pub fn resolution_warnings(manifest: &Manifest, resolution: &Resolution) -> Vec<String> {
    let mut warnings = Vec::new();
    for config in &manifest.languages {
        let version = config.version.as_str();
        match config.language {
            Language::Scala if !is_scala3(version) => {
                if let Some(library) = resolution.packages.iter().find(|p| {
                    p.coord.group == "org.scala-lang" && p.coord.artifact == "scala-library"
                }) && compare_versions(&library.coord.version, version) == Ordering::Greater
                {
                    warnings.push(format!(
                        "`org.scala-lang:scala-library` resolved to {}, newer than the Scala \
                         {version} compiler in [scala]; scalac rejects a library newer than \
                         itself, so raise `scala.version` to {} or newer",
                        library.coord.version, library.coord.version
                    ));
                }
            }
            Language::Kotlin => {
                for p in &resolution.packages {
                    let extension = p.coord.group == "org.jetbrains.kotlin"
                        && matches!(
                            p.coord.artifact.as_str(),
                            "kotlin-stdlib-jdk7" | "kotlin-stdlib-jdk8"
                        );
                    if extension && compare_versions(&p.coord.version, "1.8") == Ordering::Less {
                        warnings.push(format!(
                            "`{}` is from before Kotlin 1.8 and duplicates classes that \
                             kotlin-stdlib {version} has; declare `{}:{}` at \"{version}\" so \
                             nearest-wins picks the empty 1.8+ one",
                            p.coord, p.coord.group, p.coord.artifact
                        ));
                    }
                }
            }
            _ => {}
        }
    }
    if manifest.language(Language::Scala).is_some() {
        let mut lines: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
        for p in &resolution.packages {
            if let Some(suffix) = cross_build_suffix(&p.coord.artifact) {
                let base =
                    p.coord.artifact[..p.coord.artifact.len() - suffix.len() - 1].to_string();
                let found = lines.entry((p.coord.group.clone(), base)).or_default();
                if !found.iter().any(|a| a == &p.coord.artifact) {
                    found.push(p.coord.artifact.clone());
                }
            }
        }
        for ((group, _), artifacts) in lines.into_iter().filter(|(_, a)| a.len() > 1) {
            let names: Vec<String> = artifacts.iter().map(|a| format!("`{group}:{a}`")).collect();
            warnings.push(format!(
                "{} are all on the classpath: builds of one library for different Scala \
                 lines; exclude all but one",
                names.join(" and ")
            ));
        }
    }
    warnings
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolve::{Classpath, ResolvedPackage};
    use std::path::PathBuf;

    fn unit() -> CompileUnit {
        CompileUnit {
            label: "main".into(),
            sources: Vec::new(),
            output_dir: PathBuf::from("/p/target/classes"),
            classpath: vec![PathBuf::from("/c/stdlib.jar")],
            release: 21,
            target: None,
            encoding: "UTF-8".into(),
            extra_args: vec!["-Xlint:all".into()],
            work_dir: PathBuf::from("/p/target/.jrs"),
            foreign: None,
            main_api: None,
        }
    }

    fn compiler(language: Language, version: &str) -> ForeignCompiler {
        ForeignCompiler {
            language,
            version: version.into(),
            classpath: vec![PathBuf::from("/c/compiler.jar")],
            jvm_args: vec!["-Xss4m".into()],
            extra_args: vec!["-verbose-flag".into()],
            module_name: "app".into(),
            friend_paths: Vec::new(),
            color: false,
        }
    }

    fn pairs(args: &[String]) -> Vec<(&str, &str)> {
        args.windows(2)
            .map(|w| (w[0].as_str(), w[1].as_str()))
            .collect()
    }

    #[test]
    fn sources_are_told_apart_by_extension() {
        assert_eq!(Language::of(Path::new("a/B.java")), Some(Language::Java));
        assert_eq!(Language::of(Path::new("a/B.kt")), Some(Language::Kotlin));
        assert_eq!(Language::of(Path::new("a/B.scala")), Some(Language::Scala));
        assert_eq!(
            Language::of(Path::new("a/B.groovy")),
            Some(Language::Groovy)
        );
        assert_eq!(Language::of(Path::new("build.gradle.kts")), None);
        assert_eq!(Language::of(Path::new("notes.txt")), None);
    }

    #[test]
    fn kotlin_links_against_the_resolved_stdlib_at_the_projects_release() {
        let flags = flags(&unit(), &compiler(Language::Kotlin, "2.4.20"), true).unwrap();
        assert!(flags.contains(&"-no-stdlib".to_string()), "{flags:?}");
        assert!(flags.contains(&"-no-reflect".to_string()), "{flags:?}");
        let p = pairs(&flags);
        assert!(p.contains(&("-d", "/p/target/classes")), "{flags:?}");
        assert!(p.contains(&("-classpath", "/c/stdlib.jar")), "{flags:?}");
        assert!(p.contains(&("-jvm-target", "21")), "{flags:?}");
        assert!(p.contains(&("-module-name", "app")), "{flags:?}");
        assert!(flags.contains(&"-Xjdk-release=21".to_string()), "{flags:?}");
        assert!(!flags.iter().any(|f| f.starts_with("-Xfriend-paths")));
        assert_eq!(
            flags.last().unwrap(),
            "-verbose-flag",
            "kotlinc-args come last"
        );
        assert!(
            !flags.contains(&"-Xlint:all".to_string()),
            "javac-args are javac's"
        );
    }

    #[test]
    fn kotlin_tests_see_the_main_modules_internals() {
        let mut test = compiler(Language::Kotlin, "2.4.20");
        test.module_name = "app_test".into();
        test.friend_paths = vec![PathBuf::from("/p/target/classes")];
        let flags = flags(&unit(), &test, false).unwrap();
        assert!(pairs(&flags).contains(&("-module-name", "app_test")));
        assert!(flags.contains(&"-Xfriend-paths=/p/target/classes".to_string()));
    }

    #[test]
    fn an_old_release_is_spelled_the_old_way_and_a_target_replaces_jdk_release() {
        let mut u = unit();
        u.release = 8;
        let old = flags(&u, &compiler(Language::Kotlin, "2.4.20"), false).unwrap();
        assert!(pairs(&old).contains(&("-jvm-target", "1.8")), "{old:?}");
        assert!(old.contains(&"-Xjdk-release=1.8".to_string()));

        let mut u = unit();
        u.target = Some(17);
        let targeted = flags(&u, &compiler(Language::Kotlin, "2.4.20"), false).unwrap();
        assert!(
            pairs(&targeted).contains(&("-jvm-target", "17")),
            "{targeted:?}"
        );
        assert!(!targeted.iter().any(|f| f.starts_with("-Xjdk-release")));
    }

    #[test]
    fn scala_2_and_3_name_the_release_differently() {
        let three = flags(&unit(), &compiler(Language::Scala, "3.9.0"), true).unwrap();
        assert!(
            pairs(&three).contains(&("-java-output-version", "21")),
            "{three:?}"
        );
        assert!(pairs(&three).contains(&("-encoding", "UTF-8")));
        let two = flags(&unit(), &compiler(Language::Scala, "2.13.18"), true).unwrap();
        assert!(pairs(&two).contains(&("-release", "21")), "{two:?}");
        assert_eq!(
            no_color_flag(&compiler(Language::Scala, "3.3.6")),
            Some("-color:never")
        );
        assert_eq!(no_color_flag(&compiler(Language::Scala, "2.13.18")), None);
        assert_eq!(no_color_flag(&compiler(Language::Kotlin, "2.4.20")), None);
    }

    #[test]
    fn groovy_compiles_java_jointly_and_hands_it_the_javac_args() {
        let groovy = compiler(Language::Groovy, "5.1.2");
        let joint = flags(&unit(), &groovy, true).unwrap();
        assert_eq!(
            &joint[..2],
            &["-cp", "/c/stdlib.jar"],
            "the classpath goes first"
        );
        assert!(joint.contains(&"-j".to_string()), "{joint:?}");
        assert!(joint.contains(&"-J=-release=21".to_string()), "{joint:?}");
        assert!(joint.contains(&"-F=Xlint:all".to_string()), "{joint:?}");
        assert!(joint.contains(&"--encoding=UTF-8".to_string()));

        let groovy_only = flags(&unit(), &groovy, false).unwrap();
        assert!(!groovy_only.contains(&"-j".to_string()));
        assert!(!groovy_only.iter().any(|f| f.starts_with("-J=")));

        assert_eq!(
            jvm_flags(&unit(), &groovy),
            vec!["-Xss4m", "-Dgroovy.target.bytecode=21"]
        );
        let mut u = unit();
        u.target = Some(17);
        let joint = flags(&u, &groovy, true).unwrap();
        assert!(joint.contains(&"-J=source=21".to_string()), "{joint:?}");
        assert!(joint.contains(&"-J=target=17".to_string()), "{joint:?}");
        assert_eq!(jvm_flags(&u, &groovy)[1], "-Dgroovy.target.bytecode=17");
    }

    #[test]
    fn javac_args_are_translated_for_groovy_by_a_fixed_rule() {
        let args = |a: &[&str]| a.iter().map(ToString::to_string).collect::<Vec<_>>();
        assert_eq!(
            groovy_javac_args(&args(&[
                "-Xlint:all",
                "-Werror",
                "-parameters",
                "-Akey=v",
                "--enable-preview",
                "-Xmaxerrs",
                "5",
                "--add-exports",
                "java.base/sun.nio.ch=ALL-UNNAMED",
                "-processorpath",
                "/a b/p.jar",
            ]))
            .unwrap(),
            args(&[
                "-F=Xlint:all",
                "-F=Werror",
                "-F=parameters",
                "-F=Akey=v",
                "-F=-enable-preview",
                "-J=Xmaxerrs=5",
                "-F=-add-exports=java.base/sun.nio.ch=ALL-UNNAMED",
                "-J=processorpath=/a b/p.jar",
            ])
        );
    }

    #[test]
    fn a_javac_arg_groovy_cannot_place_is_refused_by_name() {
        let err = groovy_javac_args(&["-Xlint:all".into(), "stray".into()]).unwrap_err();
        assert!(err.contains("`stray`"), "{err}");
        let err = groovy_javac_args(&["-Xmaxerrs".into()]).unwrap_err();
        assert!(err.contains("`-Xmaxerrs` needs a value"), "{err}");
    }

    #[test]
    fn minimum_versions_are_enforced_per_line() {
        assert!(Language::Kotlin.check_version("2.0.0").is_ok());
        assert!(Language::Kotlin.check_version("2.4.20").is_ok());
        assert!(Language::Kotlin.check_version("1.9.24").is_err());
        assert!(Language::Scala.check_version("3.3.0").is_ok());
        assert!(Language::Scala.check_version("3.9.0").is_ok());
        assert!(Language::Scala.check_version("2.13.9").is_ok());
        assert!(Language::Scala.check_version("2.13.18").is_ok());
        assert!(Language::Scala.check_version("3.2.2").is_err());
        assert!(Language::Scala.check_version("2.13.8").is_err());
        let err = Language::Scala.check_version("2.12.18").unwrap_err();
        assert!(err.contains("2.12 is not supported"), "{err}");
        assert!(Language::Groovy.check_version("4.0.0").is_ok());
        assert!(Language::Groovy.check_version("5.1.2").is_ok());
        assert!(Language::Groovy.check_version("3.0.22").is_err());
    }

    #[test]
    fn each_line_has_its_compiler_and_runtime_library() {
        let main = |l: Language, v: &str| l.compiler(v).unwrap().coord.to_string();
        assert_eq!(
            main(Language::Kotlin, "2.4.20"),
            "org.jetbrains.kotlin:kotlin-compiler-embeddable:2.4.20"
        );
        assert_eq!(
            main(Language::Scala, "3.9.0"),
            "org.scala-lang:scala3-compiler_3:3.9.0"
        );
        assert_eq!(
            main(Language::Scala, "2.13.18"),
            "org.scala-lang:scala-compiler:2.13.18"
        );
        assert_eq!(
            main(Language::Groovy, "5.1.2"),
            "org.apache.groovy:groovy:5.1.2"
        );
        assert_eq!(Language::Java.compiler("21"), None);

        assert_eq!(
            Language::Scala.runtime_libraries("3.3.6"),
            vec![("org.scala-lang", "scala3-library_3")]
        );
        assert_eq!(
            Language::Scala.runtime_libraries("3.8.0"),
            vec![
                ("org.scala-lang", "scala3-library_3"),
                ("org.scala-lang", "scala-library")
            ]
        );
        assert_eq!(
            Language::Scala.runtime_libraries("2.13.18"),
            vec![("org.scala-lang", "scala-library")]
        );
        assert_eq!(
            Language::from_tool_name("kotlin-compiler"),
            Some(Language::Kotlin)
        );
        assert_eq!(Language::from_tool_name("javac-compiler"), None);
    }

    #[test]
    fn a_release_the_compiler_does_not_know_gets_one_line_of_advice() {
        let hint = Language::Kotlin
            .release_hint(
                "2.0.0",
                27,
                "error: unknown JVM target version: 27\nSupported versions: 1.8, 9",
            )
            .unwrap();
        assert_eq!(
            hint,
            "Kotlin 2.0.0 does not know Java 27: lower `java.source`, or raise `kotlin.version`"
        );
        assert!(
            Language::Scala
                .release_hint(
                    "3.3.6",
                    26,
                    "26 is not a valid choice for -java-output-version"
                )
                .is_some()
        );
        assert!(
            Language::Groovy
                .release_hint(
                    "4.0.28",
                    26,
                    "BUG! Bytecode version [null] is not supported by the compiler"
                )
                .is_some()
        );
        assert_eq!(
            Language::Kotlin.release_hint("2.4.20", 21, "Main.kt:1:1: error: nope"),
            None
        );
        assert!(
            Language::Groovy
                .release_hint(
                    "4.0.28",
                    23,
                    "java.lang.IllegalArgumentException: Unsupported Java Version: JAVA_23"
                )
                .is_some(),
            "groovydoc's words"
        );
    }

    #[test]
    fn each_line_has_its_doc_tool() {
        let scala3 = Language::Scala.doc_tool("3.3.6").unwrap();
        assert_eq!(scala3.main_class, "dotty.tools.scaladoc.Main");
        assert_eq!(
            scala3.roots,
            vec![Coord::new("org.scala-lang", "scaladoc_3", "3.3.6")]
        );
        assert_eq!(
            Language::Scala.doc_tool("3.8.4").unwrap().roots.len(),
            1,
            "jackson 2 until 3.9"
        );
        let pinned = Language::Scala.doc_tool("3.9.0").unwrap();
        assert_eq!(
            pinned.roots[1].to_string(),
            "com.fasterxml.jackson.core:jackson-annotations:2.21"
        );

        let scala2 = Language::Scala.doc_tool("2.13.18").unwrap();
        assert!(scala2.roots.is_empty(), "Scaladoc 2 is in the compiler");
        assert_eq!(scala2.main_class, "scala.tools.nsc.ScalaDoc");

        let groovy = Language::Groovy.doc_tool("5.1.2").unwrap();
        assert_eq!(groovy.name, "groovydoc");
        assert_eq!(
            groovy.roots[0].to_string(),
            "org.apache.groovy:groovy-groovydoc:5.1.2"
        );
        assert_eq!(Language::Kotlin.doc_tool("2.4.20"), None);
        assert_eq!(Language::Java.doc_tool("21"), None);
    }

    fn manifest(body: &str) -> Manifest {
        let text = format!("[project]\nname='app'\nversion='1.0.0'\n{body}");
        Manifest::parse(&text, Path::new("/p/jrs.toml"), Path::new("/p")).unwrap()
    }

    fn resolved(gavs: &[&str]) -> Resolution {
        Resolution {
            packages: gavs
                .iter()
                .map(|gav| ResolvedPackage {
                    coord: Coord::parse(gav).unwrap(),
                    classpath: Classpath::Compile,
                    packaging: "jar".into(),
                    depth: 2,
                    direct: false,
                    dependencies: Vec::new(),
                    jar: None,
                    checksum: None,
                    mediated: false,
                })
                .collect(),
            ..Resolution::default()
        }
    }

    #[test]
    fn a_scala_2_library_newer_than_its_compiler_is_reported() {
        let m = manifest("[scala]\nversion = '2.13.12'");
        let warnings =
            resolution_warnings(&m, &resolved(&["org.scala-lang:scala-library:2.13.16"]));
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0].contains("newer than the Scala 2.13.12"),
            "{warnings:?}"
        );
        assert!(
            resolution_warnings(&m, &resolved(&["org.scala-lang:scala-library:2.13.12"]))
                .is_empty()
        );
    }

    #[test]
    fn two_scala_lines_of_one_library_are_reported() {
        let m = manifest("[scala]\nversion = '3.9.0'");
        let warnings = resolution_warnings(
            &m,
            &resolved(&[
                "org.typelevel:cats-core_2.13:2.13.0",
                "org.typelevel:cats-core_3:2.13.0",
                "org.typelevel:cats-kernel_3:2.13.0",
            ]),
        );
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("cats-core_2.13"), "{warnings:?}");
        assert!(warnings[0].contains("cats-core_3"), "{warnings:?}");
    }

    #[test]
    fn an_old_kotlin_stdlib_extension_is_reported_with_the_fix() {
        let m = manifest("[kotlin]\nversion = '2.4.20'");
        let warnings = resolution_warnings(
            &m,
            &resolved(&[
                "org.jetbrains.kotlin:kotlin-stdlib-jdk8:1.7.20",
                "org.jetbrains.kotlin:kotlin-stdlib-jdk7:1.9.0",
            ]),
        );
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0].contains("kotlin-stdlib-jdk8:1.7.20"),
            "{warnings:?}"
        );
        assert!(warnings[0].contains("\"2.4.20\""), "{warnings:?}");
        assert!(
            resolution_warnings(
                &manifest(""),
                &resolved(&["org.jetbrains.kotlin:kotlin-stdlib-jdk8:1.7.20"])
            )
            .is_empty(),
            "a Java project gets no Kotlin advice"
        );
    }

    #[test]
    fn cross_build_suffixes_are_read_off_artifact_names() {
        assert_eq!(cross_build_suffix("cats-core_2.13"), Some("2.13"));
        assert_eq!(cross_build_suffix("munit_3"), Some("3"));
        assert_eq!(cross_build_suffix("scala3-library_3"), Some("3"));
        assert_eq!(cross_build_suffix("error_prone_annotations"), None);
        assert_eq!(scala_suffix("3.9.0"), "_3");
        assert_eq!(scala_suffix("2.13.18"), "_2.13");
    }
}
