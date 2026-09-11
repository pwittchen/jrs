//! `GraalVM` native images: `jrs package --native-image` (SPEC §9.7).
//!
//! `native-image` is a tool in a `GraalVM` JDK's `bin/`, so jrs drives it the
//! way it drives `jlink`: it decides what goes in and where it lands, and
//! passes the tool's output through verbatim. The whole invocation — the
//! runtime classpath, the output path, `package.native-image-args` and the
//! main class — goes into `target/.jrs/native-image.args`, since
//! `native-image` reads `@argfiles` the way the `java` launcher does.
//!
//! The reachability metadata that libraries ship under
//! `META-INF/native-image/` is found by `native-image` itself, on the
//! classpath. jrs does not download the shared metadata repository the Gradle
//! plugin uses; what a library lacks goes into `native-image-args`.

use std::path::{Path, PathBuf};

use crate::compile::render_argfile;
use crate::error::{IoResultExt, JrsError, Result};
use crate::toolchain::{Toolchain, run_captured};
use crate::ui::{Stream, Ui};

/// The names `native-image` goes by in a JDK's `bin/`: a launcher script on
/// Windows (`.cmd`), an executable everywhere else.
fn candidates() -> &'static [&'static str] {
    if cfg!(windows) {
        &["native-image.cmd", "native-image.exe", "native-image.bat"]
    } else {
        &["native-image"]
    }
}

/// `native-image` beside the toolchain's `javac` — the JDK is `GraalVM` — or an
/// error saying it is not, and how to build with one that is.
///
/// # Errors
///
/// [`JrsError::Toolchain`] if the JDK has no `native-image` in its `bin/`.
pub fn find(toolchain: &Toolchain) -> Result<PathBuf> {
    let bin = toolchain.javac.parent().unwrap_or(Path::new("."));
    if let Some(found) = candidates()
        .iter()
        .map(|name| bin.join(name))
        .find(|p| p.is_file())
    {
        return Ok(found);
    }
    Err(JrsError::toolchain(format!(
        "the JDK at {} is not GraalVM: there is no `native-image` in {}\n\n\
         `jrs package --native-image` needs a GraalVM JDK. Install one (GraalVM for JDK {v}, \
         say) and build with it: point JAVA_HOME at it, or pin `java.jdk` and name its home \
         in the jrs config's [jdks] table. Older GraalVM releases ship native-image on its \
         own: run `gu install native-image`.",
        bin.parent().unwrap_or(bin).display(),
        bin.display(),
        v = toolchain.version
    )))
}

/// What to build a native image of.
#[derive(Debug, Clone, Copy)]
pub struct NativeImage<'a> {
    /// The executable's name: `project.name`.
    pub name: &'a str,
    pub main_class: &'a str,
    /// The classes directory, then the runtime classpath.
    pub classpath: &'a [PathBuf],
    /// `package.native-image-args`, passed through verbatim.
    pub extra_args: &'a [String],
}

/// `native-image`'s arguments, in order: the classpath, the output, the
/// user's flags, then the main class, which is what ends the options.
#[must_use]
pub fn arguments(image: &NativeImage, output: &Path) -> Vec<String> {
    let mut args = vec![
        "-cp".to_string(),
        Toolchain::classpath(image.classpath),
        "-o".to_string(),
        output.display().to_string(),
    ];
    args.extend(image.extra_args.iter().cloned());
    args.push(image.main_class.to_string());
    args
}

/// The file `native-image -o <dir>/<name>` writes: `<name>.exe` on Windows.
#[must_use]
pub fn executable(output_dir: &Path, name: &str) -> PathBuf {
    if cfg!(windows) {
        output_dir.join(format!("{name}.exe"))
    } else {
        output_dir.join(name)
    }
}

/// Build `output_dir/<name>` with `native_image`, and return the executable.
///
/// `output_dir` is emptied first — it lives under `target/`, and whatever is
/// in it afterwards is what this run produced. The argfile goes in `work_dir`.
///
/// # Errors
///
/// [`JrsError::Build`] if `native-image` cannot be started, fails, or leaves
/// no executable behind; [`JrsError::Io`] if `output_dir` cannot be cleared
/// or the argfile written.
pub fn build(
    native_image: &Path,
    image: &NativeImage,
    output_dir: &Path,
    work_dir: &Path,
    ui: &Ui,
) -> Result<PathBuf> {
    if output_dir.exists() {
        std::fs::remove_dir_all(output_dir).path(output_dir)?;
    }
    std::fs::create_dir_all(output_dir).path(output_dir)?;
    std::fs::create_dir_all(work_dir).path(work_dir)?;

    let argfile = work_dir.join("native-image.args");
    let args = arguments(image, &output_dir.join(image.name));
    std::fs::write(&argfile, render_argfile(&args, &[])).path(&argfile)?;
    let result = run_captured(ui, native_image, &[format!("@{}", argfile.display())])?;
    for text in [&result.stderr, &result.stdout] {
        if !text.trim().is_empty() {
            ui.passthrough(Stream::Err, text.trim_end());
        }
    }
    if !result.ok() {
        return Err(JrsError::build(format!(
            "native-image could not build {}; its output is above\n\n\
             flags it needs (`--initialize-at-build-time`, resource or reflection \
             configuration) go in `package.native-image-args`",
            image.name
        )));
    }
    let built = executable(output_dir, image.name);
    if !built.is_file() {
        return Err(JrsError::build(format!(
            "native-image reported success but there is no {}",
            built.display()
        )));
    }
    Ok(built)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Tree {
        root: PathBuf,
    }

    impl Tree {
        fn new(name: &str) -> Tree {
            let root =
                std::env::temp_dir().join(format!("jrs-native-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(root.join("jdk/bin")).unwrap();
            Tree { root }
        }

        fn toolchain(&self) -> Toolchain {
            let bin = self.root.join("jdk/bin");
            Toolchain {
                javac: bin.join("javac"),
                java: bin.join("java"),
                jar: bin.join("jar"),
                version: 21,
                home: Some(self.root.join("jdk")),
            }
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn a_jdk_without_native_image_is_not_graalvm() {
        let tree = Tree::new("not-graal");
        let err = find(&tree.toolchain()).unwrap_err();
        assert!(matches!(err, JrsError::Toolchain(_)), "{err:?}");
        assert_eq!(err.exit_code(), 1);
        let text = err.to_string();
        assert!(text.contains("is not GraalVM"), "{text}");
        assert!(text.contains("`native-image`"), "{text}");
        assert!(text.contains("GraalVM for JDK 21"), "{text}");
        assert!(
            text.contains(&tree.root.join("jdk").display().to_string()),
            "{text}"
        );
    }

    #[test]
    fn a_graalvm_jdk_has_native_image_in_bin() {
        let tree = Tree::new("graal");
        let name = candidates()[0];
        let tool = tree.root.join("jdk/bin").join(name);
        std::fs::write(&tool, "").unwrap();
        assert_eq!(find(&tree.toolchain()).unwrap(), tool);
    }

    #[test]
    fn the_arguments_put_the_main_class_last() {
        let classpath = vec![PathBuf::from("target/classes"), PathBuf::from("/c/dep.jar")];
        let extra = vec![
            "--no-fallback".to_string(),
            "-H:+ReportExceptionStackTraces".to_string(),
        ];
        let image = NativeImage {
            name: "demo",
            main_class: "com.example.Main",
            classpath: &classpath,
            extra_args: &extra,
        };
        let args = arguments(&image, Path::new("target/native/demo"));
        assert_eq!(
            args,
            [
                "-cp",
                &Toolchain::classpath(&classpath),
                "-o",
                &Path::new("target/native/demo").display().to_string(),
                "--no-fallback",
                "-H:+ReportExceptionStackTraces",
                "com.example.Main",
            ]
        );
    }

    #[test]
    fn the_argfile_keeps_a_spaced_path_one_argument() {
        let classpath = vec![PathBuf::from("/Users/Ada Lovelace/app/classes")];
        let image = NativeImage {
            name: "demo",
            main_class: "Main",
            classpath: &classpath,
            extra_args: &[],
        };
        let text = render_argfile(&arguments(&image, Path::new("out/demo")), &[]);
        assert_eq!(
            text.lines().nth(1),
            Some("\"/Users/Ada Lovelace/app/classes\"")
        );
        assert_eq!(text.lines().last(), Some("Main"));
    }

    #[test]
    fn the_executable_has_the_platforms_suffix() {
        let path = executable(Path::new("target/native"), "demo");
        if cfg!(windows) {
            assert_eq!(path, Path::new("target/native/demo.exe"));
        } else {
            assert_eq!(path, Path::new("target/native/demo"));
        }
    }
}
