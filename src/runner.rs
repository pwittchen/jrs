//! Running the compiled project.
//!
//! `jrs run` hands the terminal to the user's program: stdin, stdout and stderr
//! are inherited, and the live region comes down first, so a program that draws
//! its own output is not fighting jrs for the cursor.

use std::path::PathBuf;

use crate::error::Result;
use crate::toolchain::{Toolchain, run_inherited};
use crate::ui::Ui;

/// Build the argument list for `java <jvm-args> -cp <cp> <main-class> args...`.
///
/// JVM arguments (`[run] jvm-args`) go first: anything after the main class
/// belongs to the program.
#[must_use]
pub fn java_args(
    jvm_args: &[String],
    classpath: &[PathBuf],
    main_class: &str,
    program_args: &[String],
) -> Vec<String> {
    let mut args = Vec::with_capacity(jvm_args.len() + program_args.len() + 3);
    args.extend(jvm_args.iter().cloned());
    if !classpath.is_empty() {
        args.push("-cp".to_string());
        args.push(Toolchain::classpath(classpath));
    }
    args.push(main_class.to_string());
    args.extend(program_args.iter().cloned());
    args
}

/// Run the project's main class, returning its exit code.
///
/// # Errors
///
/// [`JrsError::Build`](crate::error::JrsError::Build) if `java` cannot be
/// started. A program that exits non-zero is not an error: its code is returned.
pub fn run_main(
    toolchain: &Toolchain,
    jvm_args: &[String],
    classpath: &[PathBuf],
    main_class: &str,
    program_args: &[String],
    ui: &Ui,
) -> Result<i32> {
    let args = java_args(jvm_args, classpath, main_class, program_args);
    run_inherited(ui, &toolchain.java, &args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_are_ordered_for_java() {
        let args = java_args(
            &[],
            &[PathBuf::from("/classes"), PathBuf::from("/dep.jar")],
            "com.example.Main",
            &["--flag".into(), "value".into()],
        );
        assert_eq!(args[0], "-cp");
        assert!(args[1].contains("classes"));
        assert_eq!(args[2], "com.example.Main");
        assert_eq!(&args[3..], &["--flag", "value"]);
    }

    #[test]
    fn jvm_arguments_come_before_the_main_class() {
        let args = java_args(
            &["-Xmx256m".into(), "--enable-preview".into()],
            &[PathBuf::from("/classes")],
            "Main",
            &["-Xmx1g".into()],
        );
        assert_eq!(&args[..2], &["-Xmx256m", "--enable-preview"]);
        assert_eq!(args[2], "-cp");
        assert_eq!(args[4], "Main");
        assert_eq!(
            args[5], "-Xmx1g",
            "the same flag after the main class is the program's"
        );
    }

    #[test]
    fn program_arguments_are_passed_through_untouched() {
        // Anything after `--` belongs to the program, including things that look
        // like jrs flags.
        let args = java_args(&[], &[], "Main", &["--verbose".into(), "-q".into()]);
        assert_eq!(args, vec!["Main", "--verbose", "-q"]);
    }

    #[test]
    fn an_empty_classpath_emits_no_cp_flag() {
        assert_eq!(java_args(&[], &[], "Main", &[]), vec!["Main"]);
    }
}
