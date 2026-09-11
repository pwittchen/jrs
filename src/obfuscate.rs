//! Obfuscation: `jrs package --obfuscate` (SPEC §9.8).
//!
//! ProGuard is resolved from Maven Central as an isolated tool graph, pinned in
//! `jrs.lock` beside the compilers (`JVM_LANGUAGES.md` §5.2), and run on the
//! project's JDK. jrs shells out to it and never rewrites class files itself —
//! a driver here as everywhere. Obfuscation runs after `package`, over the jar
//! `package` assembled, so it composes with the fat-jar merge rules
//! (`package.rs`) instead of redoing them.
//!
//! The whole invocation goes in `target/.jrs/obfuscate.pro`, in ProGuard's own
//! configuration format, and jrs runs `java -cp <graph> proguard.ProGuard
//! @obfuscate.pro`. The `java` launcher stops expanding `@argfiles` at the main
//! class, so the trailing `@obfuscate.pro` reaches ProGuard verbatim (SPEC
//! §5.3, and the same reason `javac`'s sources may follow the main class).
//!
//! What survives, by keeping the names it names: the entry point (`-keep` on
//! `main`), every class a `META-INF/services/` file names (`ServiceLoader`),
//! and whatever `[obfuscate].keep` lists. Everything else is renamed and its
//! debug information stripped. Nothing is removed or reordered
//! (`-dontshrink -dontoptimize`), so the program behaves — and times — as the
//! plain jar does; only the names change.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use crate::compile::render_argfile;
use crate::error::{IoResultExt, JrsError, Result};
use crate::resolve::coord::Coord;
use crate::toolchain::{Toolchain, run_captured};
use crate::ui::{Stream, Ui};

/// ProGuard's coordinate and entry point. `proguard-base` is not
/// self-contained (it pulls `proguard-core` and the Kotlin metadata reader), so
/// it is resolved as a graph, not fetched as a lone jar.
pub const GROUP: &str = "com.guardsquare";
pub const ARTIFACT: &str = "proguard-base";
pub const MAIN_CLASS: &str = "proguard.ProGuard";

/// The name ProGuard's pinned graph goes by in `jrs.lock`'s `[[tool]]` blocks
/// and in `jrs tree --tool`.
pub const TOOL_NAME: &str = "obfuscator";

/// The single root of the obfuscator's tool graph, at `version`.
#[must_use]
pub fn coord(version: &str) -> Coord {
    Coord::new(GROUP, ARTIFACT, version)
}

/// What to obfuscate, and the names to leave alone while doing it.
#[derive(Debug, Clone, Copy)]
pub struct Obfuscation<'a> {
    /// The assembled jar: ProGuard's input, and where the result lands.
    pub jar: &'a Path,
    /// ProGuard's own graph, resolved apart from the project's.
    pub tool_classpath: &'a [PathBuf],
    /// The classes ProGuard may read but not obfuscate: the JDK's modules, and
    /// the dependency jars that live outside the jar (a thin or portable jar).
    /// A `.jmod` gets the module filter; a plain jar is taken whole.
    pub library_jars: &'a [PathBuf],
    /// The entry point to keep, when the project has one.
    pub main_class: Option<&'a str>,
    /// `[obfuscate].keep`: fully-qualified class names whose names must survive,
    /// for a framework or reflection that looks them up by name.
    pub keep: &'a [String],
    /// `[obfuscate].proguard-args`: passed through verbatim, last.
    pub extra_args: &'a [String],
}

/// The `.jmod` entry filter: a module archive holds its classes under
/// `classes/`, with bundled jars and a `module-info` ProGuard must not read.
const JMOD_FILTER: &str = "!**.jar;!module-info.class";

/// The JDK's modules as ProGuard library jars: every `.jmod` under the
/// toolchain's `jmods/`, sorted for a deterministic configuration. Empty when
/// the JDK has no `jmods/` (a caller can add its own with `proguard-args`).
#[must_use]
pub fn jdk_library_jars(toolchain: &Toolchain) -> Vec<PathBuf> {
    let Some(home) = &toolchain.home else {
        return Vec::new();
    };
    let mut jmods: Vec<PathBuf> = match std::fs::read_dir(home.join("jmods")) {
        Ok(entries) => entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "jmod"))
            .collect(),
        Err(_) => Vec::new(),
    };
    jmods.sort();
    jmods
}

/// The classes named in the jar's `META-INF/services/` files — the
/// `ServiceLoader` providers whose names must survive obfuscation. Sorted and
/// deduplicated so the configuration is deterministic.
///
/// # Errors
///
/// [`JrsError::Build`] if the jar cannot be opened or read as a zip.
pub fn service_classes(jar: &Path) -> Result<Vec<String>> {
    let file = std::fs::File::open(jar).path(jar)?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| JrsError::build(format!("{} is not a readable jar: {e}", jar.display())))?;
    let mut names = Vec::new();
    for i in 0..archive.len() {
        let entry = archive
            .by_index(i)
            .map_err(|e| JrsError::build(format!("{}: {e}", jar.display())))?;
        let name = entry.name();
        if !name.starts_with("META-INF/services/") || name.ends_with('/') {
            continue;
        }
        for line in BufReader::new(entry).lines() {
            let line = line.map_err(|e| JrsError::build(format!("{}: {e}", jar.display())))?;
            // A services file may carry `#` comments and blank lines.
            let class = line.split('#').next().unwrap_or("").trim();
            if !class.is_empty() {
                names.push(class.to_string());
            }
        }
    }
    names.sort();
    names.dedup();
    Ok(names)
}

/// One classpath entry in ProGuard's syntax: quoted when it has whitespace, and
/// a `.jmod` carrying the module filter.
fn entry(path: &Path) -> String {
    let text = path.display().to_string();
    let quoted = if text.contains([' ', '\t']) {
        format!("\"{text}\"")
    } else {
        text
    };
    if path.extension().is_some_and(|e| e == "jmod") {
        format!("{quoted}({JMOD_FILTER})")
    } else {
        quoted
    }
}

/// The ProGuard configuration for `obf`, writing to `outjar`. `services` is the
/// provider list [`service_classes`] read from the jar.
#[must_use]
pub fn config(obf: &Obfuscation, services: &[String], outjar: &Path) -> String {
    let mut s = String::new();
    s.push_str(&format!("-injars {}\n", entry(obf.jar)));
    s.push_str(&format!("-outjars {}\n", entry(outjar)));
    for lib in obf.library_jars {
        s.push_str(&format!("-libraryjars {}\n", entry(lib)));
    }
    // Rename and strip debug information, but change nothing else: keeping the
    // code and its order is what leaves behaviour and run timing untouched.
    s.push_str("-dontshrink\n");
    s.push_str("-dontoptimize\n");
    // Debug attributes (SourceFile, LineNumberTable, LocalVariableTable) are
    // dropped by omission; annotations and generic signatures are kept, since
    // reflection and frameworks read them.
    s.push_str("-keepattributes *Annotation*,Signature,EnclosingMethod,InnerClasses\n");
    s.push_str("-dontnote\n");
    if let Some(main) = obf.main_class {
        s.push_str(&format!(
            "-keep public class {main} {{\n    public static void main(java.lang.String[]);\n}}\n"
        ));
    }
    for provider in services {
        // Keep the provider's name (so its services file still resolves) and its
        // constructors (so ServiceLoader can instantiate it); its other members
        // are still fair game.
        s.push_str(&format!(
            "-keep class {provider} {{\n    <init>(...);\n}}\n"
        ));
    }
    if !services.is_empty() {
        // Belt and braces: were a kept name ever to change, rewrite the file to
        // match. With the names kept above this is a no-op, never a surprise.
        s.push_str("-adaptresourcefilecontents META-INF/services/**\n");
    }
    for name in obf.keep {
        s.push_str(&format!("-keep class {name}\n"));
    }
    for arg in obf.extra_args {
        s.push_str(arg);
        s.push('\n');
    }
    s
}

/// Obfuscate `obf.jar` in place with ProGuard, and return the obfuscated jar's
/// size in bytes.
///
/// ProGuard writes a fresh jar in `work_dir`, which then replaces the input, so
/// an interrupted run leaves the plain jar intact. The `java` launcher and the
/// ProGuard configuration both live in `work_dir` (`target/.jrs/`).
///
/// # Errors
///
/// [`JrsError::Build`] if the jar cannot be read, ProGuard cannot be started or
/// fails, or it leaves no output; [`JrsError::Io`] if a work file cannot be
/// written or the result cannot be moved into place.
pub fn build(java: &Path, obf: &Obfuscation, work_dir: &Path, ui: &Ui) -> Result<u64> {
    std::fs::create_dir_all(work_dir).path(work_dir)?;
    let services = service_classes(obf.jar)?;

    let outjar = work_dir.join("obfuscated.jar");
    if outjar.exists() {
        std::fs::remove_file(&outjar).path(&outjar)?;
    }
    let config_path = work_dir.join("obfuscate.pro");
    std::fs::write(&config_path, config(obf, &services, &outjar)).path(&config_path)?;

    let args = vec![
        "-cp".to_string(),
        Toolchain::classpath(obf.tool_classpath),
        MAIN_CLASS.to_string(),
        format!("@{}", config_path.display()),
    ];
    let argfile = work_dir.join("obfuscate.args");
    std::fs::write(&argfile, render_argfile(&args, &[])).path(&argfile)?;

    let result = run_captured(ui, java, &[format!("@{}", argfile.display())])?;
    for text in [&result.stdout, &result.stderr] {
        if !text.trim().is_empty() {
            ui.passthrough(Stream::Err, text.trim_end());
        }
    }
    if !result.ok() {
        return Err(JrsError::build(format!(
            "ProGuard could not obfuscate {}; its output is above\n\n\
             a class it cannot resolve needs `-libraryjars` or a `-dontwarn`, and a name it \
             must not rename needs a `keep` entry; both go in [obfuscate]",
            obf.jar.display()
        )));
    }
    if !outjar.is_file() {
        return Err(JrsError::build(
            "ProGuard reported success but wrote no jar".to_string(),
        ));
    }
    let bytes = std::fs::metadata(&outjar).path(&outjar)?.len();
    // Replace the plain jar with the obfuscated one. A rename within `target/`
    // is atomic; nothing reads the input jar between the two.
    std::fs::rename(&outjar, obf.jar)
        .or_else(|_| std::fs::copy(&outjar, obf.jar).map(|_| ()))
        .path(obf.jar)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample<'a>(jar: &'a Path, libs: &'a [PathBuf]) -> Obfuscation<'a> {
        Obfuscation {
            jar,
            tool_classpath: &[],
            library_jars: libs,
            main_class: Some("com.example.Main"),
            keep: &[],
            extra_args: &[],
        }
    }

    #[test]
    fn the_config_names_the_jar_the_output_and_the_entry_point() {
        let jar = PathBuf::from("target/app.jar");
        let obf = sample(&jar, &[]);
        let text = config(&obf, &[], Path::new("target/.jrs/obfuscated.jar"));
        assert!(text.contains("-injars target/app.jar\n"), "{text}");
        assert!(
            text.contains("-outjars target/.jrs/obfuscated.jar\n"),
            "{text}"
        );
        assert!(
            text.contains(
                "-keep public class com.example.Main {\n    public static void main(java.lang.String[]);\n}\n"
            ),
            "{text}"
        );
        // Behaviour and timing must be untouched: nothing removed, nothing
        // optimised.
        assert!(text.contains("-dontshrink\n"), "{text}");
        assert!(text.contains("-dontoptimize\n"), "{text}");
    }

    #[test]
    fn a_library_without_a_main_class_keeps_no_entry_point() {
        let jar = PathBuf::from("lib.jar");
        let obf = Obfuscation {
            main_class: None,
            ..sample(&jar, &[])
        };
        let text = config(&obf, &[], Path::new("out.jar"));
        assert!(!text.contains("public static void main"), "{text}");
    }

    #[test]
    fn service_providers_keep_their_names_and_constructors() {
        let jar = PathBuf::from("app.jar");
        let obf = sample(&jar, &[]);
        let services = vec!["com.example.Provider".to_string()];
        let text = config(&obf, &services, Path::new("out.jar"));
        assert!(
            text.contains("-keep class com.example.Provider {\n    <init>(...);\n}\n"),
            "{text}"
        );
        assert!(
            text.contains("-adaptresourcefilecontents META-INF/services/**\n"),
            "{text}"
        );
    }

    #[test]
    fn a_jmod_library_carries_the_module_filter_but_a_jar_does_not() {
        let jar = PathBuf::from("app.jar");
        let libs = vec![
            PathBuf::from("/jdk/jmods/java.base.jmod"),
            PathBuf::from("/cache/dep.jar"),
        ];
        let obf = sample(&jar, &libs);
        let text = config(&obf, &[], Path::new("out.jar"));
        assert!(
            text.contains(&format!(
                "-libraryjars /jdk/jmods/java.base.jmod({JMOD_FILTER})\n"
            )),
            "{text}"
        );
        assert!(text.contains("-libraryjars /cache/dep.jar\n"), "{text}");
    }

    #[test]
    fn keep_rules_and_extra_args_are_passed_through() {
        let jar = PathBuf::from("app.jar");
        let keep = vec!["com.example.Api".to_string()];
        let extra = vec!["-dontwarn org.foo.**".to_string()];
        let obf = Obfuscation {
            keep: &keep,
            extra_args: &extra,
            ..sample(&jar, &[])
        };
        let text = config(&obf, &[], Path::new("out.jar"));
        assert!(text.contains("-keep class com.example.Api\n"), "{text}");
        assert!(text.contains("-dontwarn org.foo.**\n"), "{text}");
    }

    #[test]
    fn a_spaced_path_is_quoted() {
        assert_eq!(
            entry(Path::new("/Users/Ada Lovelace/app.jar")),
            "\"/Users/Ada Lovelace/app.jar\""
        );
        assert_eq!(entry(Path::new("/plain/app.jar")), "/plain/app.jar");
    }
}
