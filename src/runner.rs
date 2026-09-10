//! Running the compiled project.
//!
//! `jrs run` hands the terminal to the user's program: stdin, stdout and stderr
//! are inherited, and the live region comes down first, so a program that draws
//! its own output is not fighting jrs for the cursor.

use std::path::PathBuf;

use crate::error::Result;
use crate::toolchain::{Toolchain, run_inherited};
use crate::ui::Ui;

/// Build the argument list for `java -cp <cp> <main-class> args...`.
pub fn java_args(classpath: &[PathBuf], main_class: &str, program_args: &[String]) -> Vec<String> {
    let mut args = Vec::with_capacity(program_args.len() + 3);
    if !classpath.is_empty() {
        args.push("-cp".to_string());
        args.push(Toolchain::classpath(classpath));
    }
    args.push(main_class.to_string());
    args.extend(program_args.iter().cloned());
    args
}

/// Run the project's main class, returning its exit code.
pub fn run_main(
    toolchain: &Toolchain,
    classpath: &[PathBuf],
    main_class: &str,
    program_args: &[String],
    ui: &Ui,
) -> Result<i32> {
    let args = java_args(classpath, main_class, program_args);
    run_inherited(ui, &toolchain.java, &args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_are_ordered_for_java() {
        let args = java_args(
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
    fn program_arguments_are_passed_through_untouched() {
        // Anything after `--` belongs to the program, including things that look
        // like jrs flags.
        let args = java_args(&[], "Main", &["--verbose".into(), "-q".into()]);
        assert_eq!(args, vec!["Main", "--verbose", "-q"]);
    }

    #[test]
    fn an_empty_classpath_emits_no_cp_flag() {
        assert_eq!(java_args(&[], "Main", &[]), vec!["Main"]);
    }
}
