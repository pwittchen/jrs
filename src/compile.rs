//! Driving `javac`.
//!
//! Two things here are load-bearing. Sources and the classpath go into an
//! `@argfile` rather than onto the command line, which sidesteps the OS argument
//! length limit that a few dozen dependencies will otherwise hit (SPEC §6.2). And
//! `javac`'s output is passed through verbatim — its diagnostics are already good,
//! and jrs reformatting them would only make them worse.

use std::path::{Path, PathBuf};

use crate::error::{IoResultExt, JrsError, Result};
use crate::project;
use crate::toolchain::{Toolchain, run_captured};
use crate::ui::{Stream, Ui};

/// One `javac` invocation.
#[derive(Debug)]
pub struct CompileUnit {
    /// A name for the fingerprint file: `main` or `test`.
    pub label: String,
    pub sources: Vec<PathBuf>,
    pub output_dir: PathBuf,
    pub classpath: Vec<PathBuf>,
    pub release: u32,
    /// Only emitted when it differs from `release`.
    pub target: Option<u32>,
    pub encoding: String,
    /// Appended verbatim, after the flags jrs generates (SPEC §4.2).
    pub extra_args: Vec<String>,
    pub work_dir: PathBuf,
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
    /// The flags jrs generates, in the order `javac` sees them.
    fn flags(&self) -> Vec<String> {
        let mut args = vec![
            "--release".to_string(),
            self.release.to_string(),
            "-encoding".to_string(),
            self.encoding.clone(),
            "-d".to_string(),
            self.output_dir.display().to_string(),
        ];
        if let Some(target) = self.target {
            // `--release` already pins both, so an explicit target is only
            // meaningful as the older -source/-target pair.
            args = vec![
                "-source".to_string(),
                self.release.to_string(),
                "-target".to_string(),
                target.to_string(),
                "-encoding".to_string(),
                self.encoding.clone(),
                "-d".to_string(),
                self.output_dir.display().to_string(),
            ];
        }
        if !self.classpath.is_empty() {
            args.push("-cp".to_string());
            args.push(Toolchain::classpath(&self.classpath));
        }
        args.extend(self.extra_args.iter().cloned());
        args
    }

    fn argfile_path(&self) -> PathBuf {
        self.work_dir.join(format!("javac-{}.args", self.label))
    }

    fn fingerprint_path(&self) -> PathBuf {
        self.work_dir.join(format!("{}.fingerprint", self.label))
    }

    /// Everything that, if changed, means the previous output cannot be reused.
    fn fingerprint(&self) -> String {
        let mut s = String::new();
        s.push_str(&self.flags().join("\u{1}"));
        s.push('\n');
        for source in &self.sources {
            s.push_str(&source.display().to_string());
            s.push('\n');
        }
        s
    }
}

/// Whether `javac` has to run.
///
/// v1 is deliberately coarse: all-or-nothing, because `javac` needs the full
/// source set anyway when types are interdependent (SPEC §7.2).
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
pub fn compile(toolchain: &Toolchain, unit: &CompileUnit, ui: &Ui) -> Result<Outcome> {
    if unit.sources.is_empty() {
        return Ok(Outcome::UpToDate);
    }
    if !is_stale(unit)? {
        return Ok(Outcome::UpToDate);
    }

    std::fs::create_dir_all(&unit.output_dir).path(&unit.output_dir)?;
    std::fs::create_dir_all(&unit.work_dir).path(&unit.work_dir)?;

    let argfile = unit.argfile_path();
    std::fs::write(&argfile, render_argfile(&unit.flags(), &unit.sources)).path(&argfile)?;

    let output = run_captured(ui, &toolchain.javac, &[format!("@{}", argfile.display())])?;

    // The live region comes down before any diagnostic reaches the terminal.
    if !output.stderr.trim().is_empty() {
        ui.passthrough(Stream::Err, output.stderr.trim_end());
    }
    if !output.stdout.trim().is_empty() {
        ui.passthrough(Stream::Err, output.stdout.trim_end());
    }

    if !output.ok() {
        let _ = std::fs::remove_file(unit.fingerprint_path());
        return Err(JrsError::build(format!(
            "compilation failed ({} source files)",
            unit.sources.len()
        )));
    }

    let fingerprint = unit.fingerprint_path();
    std::fs::write(&fingerprint, unit.fingerprint()).path(&fingerprint)?;

    let classes = project::find_by_extension(&unit.output_dir, "class")?.len();
    Ok(Outcome::Compiled { classes })
}

/// Render a `javac` argfile.
///
/// The format is one argument per line; anything with whitespace or a backslash
/// is quoted, because Windows paths contain both.
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
    if !arg.contains([' ', '\t', '"', '\\', '\'']) {
        return arg.to_string();
    }
    format!("\"{}\"", arg.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Where a class file for `class_name` would land under `dir`.
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
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn generated_flags_come_before_the_users_own() {
        let tree = Tree::new("flags");
        let flags = unit(&tree.root, vec![]).flags();
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
        let flags = u.flags();
        assert!(!flags.contains(&"--release".to_string()));
        assert_eq!(&flags[0..4], &["-source", "21", "-target", "17"]);
    }

    #[test]
    fn an_empty_classpath_emits_no_cp_flag() {
        let tree = Tree::new("no-cp");
        let mut u = unit(&tree.root, vec![]);
        u.classpath.clear();
        assert!(!u.flags().contains(&"-cp".to_string()));
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

        // Simulate a completed build: classes on disk, fingerprint written.
        std::fs::create_dir_all(&u.work_dir).unwrap();
        tree.write("target/classes/Main.class", "bytes");
        std::fs::write(u.fingerprint_path(), u.fingerprint()).unwrap();
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
        std::fs::create_dir_all(&u.work_dir).unwrap();
        tree.write("target/classes/Main.class", "bytes");
        std::fs::write(u.fingerprint_path(), u.fingerprint()).unwrap();
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
        std::fs::create_dir_all(&u.work_dir).unwrap();
        tree.write("target/classes/Main.class", "bytes");
        std::fs::write(u.fingerprint_path(), u.fingerprint()).unwrap();

        let second = tree.write("src/Other.java", "class Other {}");
        assert!(is_stale(&unit(&tree.root, vec![first, second])).unwrap());
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
