//! Compiling a unit of sources: `javac`, and before it the compiler of the
//! unit's other language when it has one (`JVM_LANGUAGES.md` §6).
//!
//! Three things here are load-bearing. Sources and classpaths go into an
//! `@argfile` rather than onto the command line, which sidesteps the OS argument
//! length limit that a few dozen dependencies will otherwise hit (SPEC §6.2).
//! Every compiler's output is passed through verbatim — their diagnostics are
//! already good, and jrs reformatting them would only make them worse. And a
//! unit is all-or-nothing whatever it holds (SPEC §7.2): its steps share one
//! output directory, one fingerprint and one staleness decision.
//!
//! Kotlin, Scala and Groovy compilers are Java programs, run on the project's
//! JDK as `java @argfile`. The one argfile holds the compiler's classpath, its
//! main class, its flags and its sources, so every compiler reads its
//! arguments with the `java` launcher's quoting — which is `javac`'s. Their
//! own `@file` readers disagree on backslashes (scalac keeps them) and on what
//! a file may hold (groovyc's lists only sources), so jrs never uses them.

pub mod abi;
pub mod javac;
pub mod lang;

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub use abi::api_digest;
pub use javac::{DocUnit, javadoc};
pub use lang::Language;

use crate::error::{IoResultExt, JrsError, Result};
use crate::project;
use crate::toolchain::{Toolchain, run_captured};
use crate::ui::{Stream, Ui};

/// One compile unit: the main sources or the test sources.
#[derive(Debug)]
pub struct CompileUnit {
    /// A name for the fingerprint and argfiles: `main` or `test`.
    pub label: String,
    /// Every source, in every language the unit holds.
    pub sources: Vec<PathBuf>,
    pub output_dir: PathBuf,
    pub classpath: Vec<PathBuf>,
    pub release: u32,
    /// Only emitted when it differs from `release`.
    pub target: Option<u32>,
    pub encoding: String,
    /// `java.javac-args`, appended verbatim after the flags jrs generates
    /// (SPEC §4.2).
    pub extra_args: Vec<String>,
    pub work_dir: PathBuf,
    /// The compiler of the unit's language besides Java, when it has one.
    pub foreign: Option<ForeignCompiler>,
    /// For the test unit: [`api_digest`] of the main classes it compiles
    /// against. It is in the fingerprint, so a main change that alters that
    /// API recompiles the tests and one that does not leaves them fresh.
    pub main_api: Option<String>,
}

/// A Kotlin, Scala or Groovy compiler, and what its run needs beyond the
/// unit's own settings.
#[derive(Debug, Clone)]
pub struct ForeignCompiler {
    pub language: Language,
    /// The compiler's version: Scala 2 and 3 take different flags.
    pub version: String,
    /// The compiler's own classpath: its resolved tool graph, never the
    /// project's.
    pub classpath: Vec<PathBuf>,
    /// `<lang>.compiler-jvm-args`, for the JVM the compiler runs in.
    pub jvm_args: Vec<String>,
    /// `<lang>.<tool>-args`, appended verbatim after jrs's own flags.
    pub extra_args: Vec<String>,
    /// Kotlin's `-module-name`: the project's name, `_test` for the tests.
    pub module_name: String,
    /// Kotlin: class directories whose `internal` declarations the unit may
    /// use, which is how tests see the main module's.
    pub friend_paths: Vec<PathBuf>,
    /// Whether diagnostics may be coloured. It changes nothing compiled, so
    /// it is not part of the fingerprint.
    pub color: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Nothing had changed since the last build.
    UpToDate,
    Compiled {
        classes: usize,
    },
}

impl CompileUnit {
    fn fingerprint_path(&self) -> PathBuf {
        self.work_dir.join(format!("{}.fingerprint", self.label))
    }

    /// The unit's sources in `language`, in their sorted order.
    #[must_use]
    pub fn sources_in(&self, language: Language) -> Vec<PathBuf> {
        self.sources
            .iter()
            .filter(|p| Language::of(p) == Some(language))
            .cloned()
            .collect()
    }

    /// The compiler that runs before `javac`: only when the unit has sources
    /// in its language.
    fn foreign_step(&self) -> Option<&ForeignCompiler> {
        self.foreign.as_ref().filter(|f| {
            self.sources
                .iter()
                .any(|p| Language::of(p) == Some(f.language))
        })
    }

    /// Everything that, if changed, means the previous output cannot be reused:
    /// every step's flags, every jar either compiler reads, the main classes'
    /// API for the tests, and the sources.
    ///
    /// A jar's path names its version, so a new version is already a new
    /// classpath. A snapshot is the exception — a new build lands at the same
    /// path — which is why each jar's size and modification time are in here
    /// too. Both come from one `stat`, which costs nothing next to a compiler.
    /// A class directory on the classpath is not a jar: the tests see
    /// `target/classes` through `main_api` instead.
    fn fingerprint(&self) -> String {
        let mut s = String::new();
        s.push_str(&self.javac_flags(false).join("\u{1}"));
        s.push('\n');
        let mut jars: Vec<&PathBuf> = self.classpath.iter().collect();
        if let Some(foreign) = &self.foreign {
            let joint = !self.sources_in(Language::Java).is_empty();
            let _ = writeln!(
                s,
                "{} {} {} {} {}",
                foreign.language.key(),
                foreign.version,
                Toolchain::classpath(&foreign.classpath),
                lang::jvm_flags(self, foreign).join("\u{1}"),
                lang::flags(self, foreign, joint)
                    .unwrap_or_default()
                    .join("\u{1}")
            );
            jars.extend(&foreign.classpath);
        }
        for entry in jars {
            if let Ok(meta) = std::fs::metadata(entry)
                && meta.is_file()
            {
                let modified = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_nanos())
                    .unwrap_or_default();
                let _ = writeln!(s, "jar {} {modified}", meta.len());
            }
        }
        if let Some(api) = &self.main_api {
            let _ = writeln!(s, "main-api {api}");
        }
        for source in &self.sources {
            s.push_str(&source.display().to_string());
            s.push('\n');
        }
        s
    }
}

/// Whether the unit has to be compiled again.
///
/// v1 is deliberately coarse: all-or-nothing, because `javac` needs the full
/// source set anyway when types are interdependent (SPEC §7.2) — and so does
/// every other compiler.
///
/// # Errors
///
/// None today: a fingerprint or class tree that cannot be read counts as
/// stale rather than as a failure.
pub fn is_stale(unit: &CompileUnit) -> Result<bool> {
    if unit.sources.is_empty() {
        return Ok(false);
    }
    if !unit.output_dir.is_dir() {
        return Ok(true);
    }
    let previous = std::fs::read_to_string(unit.fingerprint_path()).unwrap_or_default();
    if previous != unit.fingerprint() {
        return Ok(true);
    }
    let Some(newest_class) = project::newest_mtime_under(&unit.output_dir) else {
        return Ok(true);
    };
    let Some(newest_source) = project::newest_mtime(&unit.sources) else {
        return Ok(true);
    };
    Ok(newest_source > newest_class)
}

/// Compile, unless nothing changed.
///
/// The steps run in order into one emptied output directory: the unit's
/// other compiler over its sources and the Java ones (which Kotlin and Scala
/// only read, for their symbols), then `javac` over the Java sources with
/// those classes on its classpath. Groovy compiles both itself, running the
/// JDK's `javac` in its joint mode. A failing step ends the unit.
///
/// # Errors
///
/// `JrsError::Build` if a compiler cannot be started or reports a failure;
/// `JrsError::Manifest` if `java.javac-args` cannot be handed to Groovy's
/// joint `javac`; `JrsError::Io` if the output or work directory, an argfile,
/// the fingerprint or the compiled classes cannot be written or read.
pub fn compile(toolchain: &Toolchain, unit: &CompileUnit, ui: &Ui) -> Result<Outcome> {
    compile_timed(toolchain, unit, ui, &mut Vec::new())
}

/// [`compile`], also pushing the wall time of each step it runs onto
/// `steps`, named by its compiler (`kotlinc`, `javac`), in the order they
/// ran — for `--timings`. A step that fails is pushed too.
///
/// # Errors
///
/// As for [`compile`].
pub fn compile_timed(
    toolchain: &Toolchain,
    unit: &CompileUnit,
    ui: &Ui,
    steps: &mut Vec<(&'static str, Duration)>,
) -> Result<Outcome> {
    if unit.sources.is_empty() {
        return Ok(Outcome::UpToDate);
    }
    if !is_stale(unit)? {
        return Ok(Outcome::UpToDate);
    }

    // Compilation is all-or-nothing, so the output starts empty: a class whose
    // source was deleted or renamed must not survive onto the classpath and
    // into the jar. Resources are copied in again afterwards.
    if unit.output_dir.exists() {
        std::fs::remove_dir_all(&unit.output_dir).path(&unit.output_dir)?;
    }
    std::fs::create_dir_all(&unit.output_dir).path(&unit.output_dir)?;
    std::fs::create_dir_all(&unit.work_dir).path(&unit.work_dir)?;

    let result = run_steps(toolchain, unit, ui, steps);
    if result.is_err() {
        // A failed build must never be recorded as up to date.
        let _ = std::fs::remove_file(unit.fingerprint_path());
    }
    result?;

    let fingerprint = unit.fingerprint_path();
    std::fs::write(&fingerprint, unit.fingerprint()).path(&fingerprint)?;

    let classes = project::find_by_extension(&unit.output_dir, "class")?.len();
    Ok(Outcome::Compiled { classes })
}

fn run_steps(
    toolchain: &Toolchain,
    unit: &CompileUnit,
    ui: &Ui,
    steps: &mut Vec<(&'static str, Duration)>,
) -> Result<()> {
    let java = unit.sources_in(Language::Java);
    let mut after_foreign = false;
    if let Some(foreign) = unit.foreign_step() {
        let started = Instant::now();
        let result = run_foreign(toolchain, unit, foreign, !java.is_empty(), ui);
        steps.push((foreign.language.compiler_name(), started.elapsed()));
        result?;
        if foreign.language == Language::Groovy {
            // Joint compilation: groovyc has run javac over the Java sources.
            return Ok(());
        }
        after_foreign = true;
    }
    if java.is_empty() {
        return Ok(());
    }
    let what = if after_foreign {
        count(java.len(), Some(Language::Java))
    } else {
        count(java.len(), None)
    };
    let started = Instant::now();
    let result = javac::run(toolchain, unit, &java, after_foreign, &what, ui);
    steps.push(("javac", started.elapsed()));
    result
}

/// `3 Kotlin source files`, or `3 source files` for a unit with one language.
fn count(n: usize, language: Option<Language>) -> String {
    match language {
        Some(l) => format!("{n} {l} source files"),
        None => format!("{n} source files"),
    }
}

/// The unit's other compiler, over its own sources and the Java ones.
fn run_foreign(
    toolchain: &Toolchain,
    unit: &CompileUnit,
    foreign: &ForeignCompiler,
    joint: bool,
    ui: &Ui,
) -> Result<()> {
    let language = foreign.language;
    let compiler = language
        .compiler(&foreign.version)
        .ok_or_else(|| JrsError::build(format!("{language} has no compiler for jrs to run")))?;
    let own = unit.sources_in(language);

    let mut args = lang::jvm_flags(unit, foreign);
    args.extend([
        "-cp".to_string(),
        Toolchain::classpath(&foreign.classpath),
        compiler.main_class.to_string(),
    ]);
    args.extend(
        lang::flags(unit, foreign, joint)
            .map_err(|e| JrsError::manifest(format!("`java.javac-args`: {e}")))?,
    );
    if !foreign.color
        && let Some(flag) = lang::no_color_flag(foreign)
    {
        args.push(flag.to_string());
    }
    // Kotlin and Scala read the Java sources for their symbols and write only
    // their own classes; Groovy compiles both.
    let mut sources = own.clone();
    sources.extend(unit.sources_in(Language::Java));

    let argfile = unit
        .work_dir
        .join(format!("{}-{}.args", language.compiler_name(), unit.label));
    std::fs::write(&argfile, render_argfile(&args, &sources)).path(&argfile)?;
    let output = run_captured(ui, &toolchain.java, &[format!("@{}", argfile.display())])?;

    if !output.stderr.trim().is_empty() {
        ui.passthrough(Stream::Err, output.stderr.trim_end());
    }
    if !output.stdout.trim().is_empty() {
        ui.passthrough(Stream::Err, output.stdout.trim_end());
    }
    if output.ok() {
        return Ok(());
    }
    let mut message = format!("compilation failed ({})", count(own.len(), Some(language)));
    let text = format!("{}\n{}", output.stdout, output.stderr);
    if let Some(hint) = language.release_hint(&foreign.version, unit.release, &text) {
        let _ = write!(message, "\n\n{hint}");
    }
    Err(JrsError::build(message))
}

/// Render an argfile for `javac`, `javadoc`, or the `java` launcher, which
/// read the same syntax.
///
/// The format is one argument per line; anything with whitespace or a backslash
/// is quoted, because Windows paths contain both.
#[must_use]
pub fn render_argfile(flags: &[String], sources: &[PathBuf]) -> String {
    let mut s = String::new();
    for flag in flags {
        s.push_str(&quote_argument(flag));
        s.push('\n');
    }
    for source in sources {
        s.push_str(&quote_argument(&source.display().to_string()));
        s.push('\n');
    }
    s
}

fn quote_argument(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '"', '\\', '\'', '#']) {
        return arg.to_string();
    }
    format!("\"{}\"", arg.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Where a class file for `class_name` would land under `dir`.
#[must_use]
pub fn class_file(dir: &Path, class_name: &str) -> PathBuf {
    let mut path = dir.to_path_buf();
    for part in class_name.split('.') {
        path.push(part);
    }
    path.set_extension("class");
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(root: &Path, sources: Vec<PathBuf>) -> CompileUnit {
        CompileUnit {
            label: "main".into(),
            sources,
            output_dir: root.join("target/classes"),
            classpath: vec![root.join("dep.jar")],
            release: 21,
            target: None,
            encoding: "UTF-8".into(),
            extra_args: vec!["-Xlint:all".into()],
            work_dir: root.join("target/.jrs"),
            foreign: None,
            main_api: None,
        }
    }

    fn kotlin(root: &Path) -> ForeignCompiler {
        ForeignCompiler {
            language: Language::Kotlin,
            version: "2.4.20".into(),
            classpath: vec![root.join("kotlin-compiler.jar")],
            jvm_args: Vec::new(),
            extra_args: Vec::new(),
            module_name: "app".into(),
            friend_paths: Vec::new(),
            color: false,
        }
    }

    struct Tree {
        root: PathBuf,
    }

    impl Tree {
        fn new(name: &str) -> Tree {
            let root =
                std::env::temp_dir().join(format!("jrs-compile-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap();
            Tree { root }
        }

        fn write(&self, relative: &str, contents: &str) -> PathBuf {
            let path = self.root.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, contents).unwrap();
            path
        }

        /// Pretend a build completed: classes on disk, fingerprint written.
        fn built(&self, u: &CompileUnit) {
            std::fs::create_dir_all(&u.work_dir).unwrap();
            self.write("target/classes/Main.class", "bytes");
            std::fs::write(u.fingerprint_path(), u.fingerprint()).unwrap();
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn generated_flags_come_before_the_users_own() {
        let tree = Tree::new("flags");
        let flags = unit(&tree.root, vec![]).javac_flags(false);
        assert_eq!(&flags[0..2], &["--release", "21"]);
        assert!(flags.contains(&"-encoding".to_string()));
        assert_eq!(
            flags.last().unwrap(),
            "-Xlint:all",
            "javac-args are appended verbatim, last"
        );
    }

    #[test]
    fn a_differing_target_switches_to_source_and_target() {
        let tree = Tree::new("target");
        let mut u = unit(&tree.root, vec![]);
        u.target = Some(17);
        let flags = u.javac_flags(false);
        assert!(!flags.contains(&"--release".to_string()));
        assert_eq!(&flags[0..4], &["-source", "21", "-target", "17"]);
    }

    #[test]
    fn an_empty_classpath_emits_no_cp_flag() {
        let tree = Tree::new("no-cp");
        let mut u = unit(&tree.root, vec![]);
        u.classpath.clear();
        assert!(!u.javac_flags(false).contains(&"-cp".to_string()));
    }

    #[test]
    fn javac_after_another_compiler_sees_its_classes_first() {
        let tree = Tree::new("after-foreign");
        let u = unit(&tree.root, vec![]);
        let flags = u.javac_flags(true);
        let cp = flags.iter().position(|f| f == "-cp").unwrap();
        assert_eq!(
            flags[cp + 1],
            Toolchain::classpath(&[u.output_dir.clone(), tree.root.join("dep.jar")])
        );
    }

    #[test]
    fn argfiles_quote_paths_with_spaces() {
        let rendered = render_argfile(
            &["-d".into(), "/out dir".into()],
            &[PathBuf::from("/src/Main.java")],
        );
        assert_eq!(rendered, "-d\n\"/out dir\"\n/src/Main.java\n");
    }

    #[test]
    fn argfiles_escape_backslashes() {
        let rendered = render_argfile(&[], &[PathBuf::from(r"C:\src\Main.java")]);
        assert_eq!(rendered, "\"C:\\\\src\\\\Main.java\"\n");
    }

    #[test]
    fn argfiles_quote_what_the_launcher_would_otherwise_misread() {
        // An empty argument would vanish, and a `#` could start a comment.
        let rendered = render_argfile(&["".into(), "#1".into()], &[]);
        assert_eq!(rendered, "\"\"\n\"#1\"\n");
    }

    #[test]
    fn nothing_to_compile_is_never_stale() {
        let tree = Tree::new("empty");
        assert!(!is_stale(&unit(&tree.root, vec![])).unwrap());
    }

    #[test]
    fn a_missing_output_directory_is_stale() {
        let tree = Tree::new("missing-out");
        let source = tree.write("src/Main.java", "class Main {}");
        assert!(is_stale(&unit(&tree.root, vec![source])).unwrap());
    }

    #[test]
    fn a_changed_source_is_stale_but_an_unchanged_one_is_not() {
        let tree = Tree::new("staleness");
        let source = tree.write("src/Main.java", "class Main {}");
        let u = unit(&tree.root, vec![source.clone()]);
        tree.built(&u);
        assert!(!is_stale(&u).unwrap());

        // Touching the source makes it newer than the newest class file.
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&source, "class Main { int x; }").unwrap();
        assert!(is_stale(&u).unwrap());
    }

    #[test]
    fn a_changed_classpath_is_stale() {
        let tree = Tree::new("classpath-change");
        let source = tree.write("src/Main.java", "class Main {}");
        let u = unit(&tree.root, vec![source.clone()]);
        tree.built(&u);
        assert!(!is_stale(&u).unwrap());

        let mut changed = unit(&tree.root, vec![source]);
        changed.classpath.push(tree.root.join("another.jar"));
        assert!(
            is_stale(&changed).unwrap(),
            "a new jar on the classpath must force a rebuild"
        );
    }

    #[test]
    fn a_new_source_file_is_stale() {
        let tree = Tree::new("new-source");
        let first = tree.write("src/Main.java", "class Main {}");
        let u = unit(&tree.root, vec![first.clone()]);
        tree.built(&u);

        let second = tree.write("src/Other.java", "class Other {}");
        assert!(is_stale(&unit(&tree.root, vec![first, second])).unwrap());
    }

    #[test]
    fn the_fingerprint_covers_every_step() {
        let tree = Tree::new("fingerprint-steps");
        let source = tree.write("src/Main.kt", "class Main");
        let jar = tree.write("kotlin-compiler.jar", "compiler");
        let mut u = unit(&tree.root, vec![source]);
        u.foreign = Some(kotlin(&tree.root));
        tree.built(&u);
        assert!(!is_stale(&u).unwrap());

        // Colour changes what the terminal shows, not what is compiled.
        u.foreign.as_mut().unwrap().color = true;
        assert!(!is_stale(&u).unwrap());

        u.foreign.as_mut().unwrap().extra_args = vec!["-Xjsr305=strict".into()];
        assert!(is_stale(&u).unwrap(), "a kotlinc flag changed");
        u.foreign.as_mut().unwrap().extra_args.clear();
        assert!(!is_stale(&u).unwrap());

        u.foreign.as_mut().unwrap().version = "2.4.21".into();
        assert!(is_stale(&u).unwrap(), "another compiler version");
        u.foreign.as_mut().unwrap().version = "2.4.20".into();

        // A rebuilt compiler jar at the same path, as a snapshot would be.
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&jar, "another compiler build").unwrap();
        assert!(is_stale(&u).unwrap(), "the compiler jar changed");
    }

    #[test]
    fn the_steps_follow_the_units_languages() {
        let tree = Tree::new("steps");
        let kt = tree.write("src/A.kt", "");
        let java = tree.write("src/B.java", "");
        let mut u = unit(&tree.root, vec![kt.clone(), java.clone()]);
        assert!(
            u.foreign_step().is_none(),
            "a Java-only unit is one javac run"
        );
        u.foreign = Some(kotlin(&tree.root));
        assert_eq!(u.foreign_step().unwrap().language, Language::Kotlin);
        assert_eq!(u.sources_in(Language::Kotlin), vec![kt]);
        assert_eq!(u.sources_in(Language::Java), vec![java.clone()]);

        // Kotlin turned on, but nothing to compile with it: javac alone.
        let u = CompileUnit {
            foreign: Some(kotlin(&tree.root)),
            ..unit(&tree.root, vec![java])
        };
        assert!(u.foreign_step().is_none());
    }

    #[test]
    fn class_files_follow_the_package_structure() {
        assert_eq!(
            class_file(Path::new("/out"), "com.example.Main"),
            PathBuf::from("/out/com/example/Main.class")
        );
        assert_eq!(
            class_file(Path::new("/out"), "Main"),
            PathBuf::from("/out/Main.class")
        );
    }
}
