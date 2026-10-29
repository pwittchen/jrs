//! `jrs init`: a new project, with a starter source and test.

use std::path::{Path, PathBuf};

use crate::checkers;
use crate::compile::Language;
use crate::error::{IoResultExt, JrsError, Result, exit};
use crate::manifest::{self, Dependency, Hook, LanguageConfig, MANIFEST_FILE};
use crate::toolchain::Toolchain;
use crate::ui::Ui;

/// The `JUnit` a new project starts with: the newest 5.x, which still compiles
/// for every `java.source` a project might lower itself to.
const STARTER_JUNIT: &str = "5.13.4";

const STARTER_MAIN: &str = r#"package com.example;

public class Main {
    public static void main(String[] args) {
        System.out.println(greeting());
    }

    static String greeting() {
        return "Hello from jrs";
    }
}
"#;

const STARTER_MAIN_TEST: &str = r#"package com.example;

import static org.junit.jupiter.api.Assertions.assertEquals;

import org.junit.jupiter.api.Test;

class MainTest {
    @Test
    void greets() {
        assertEquals("Hello from jrs", Main.greeting());
    }
}
"#;

const STARTER_LIBRARY: &str = r#"package com.example;

public final class Library {
    private Library() {}

    public static String greeting(String name) {
        return "Hello, " + name;
    }
}
"#;

const STARTER_LIBRARY_TEST: &str = r#"package com.example;

import static org.junit.jupiter.api.Assertions.assertEquals;

import org.junit.jupiter.api.Test;

class LibraryTest {
    @Test
    void greetsByName() {
        assertEquals("Hello, jrs", Library.greeting("jrs"));
    }
}
"#;

/// `MUnit`, the Scala starter's test framework: a `JUnit` 4 runner, so it runs
/// on the launcher's Vintage engine.
const STARTER_MUNIT: &str = "1.3.6";

/// Spock, the Groovy starter's test framework, in its build for the Groovy
/// line the starter pins.
const STARTER_SPOCK: &str = "2.4-groovy-5.0";

const STARTER_MAIN_KOTLIN: &str = r#"package com.example

fun main() {
    println(greeting())
}

fun greeting(): String = "Hello from jrs"
"#;

const STARTER_MAIN_TEST_KOTLIN: &str = r#"package com.example

import kotlin.test.Test
import kotlin.test.assertEquals

class MainTest {
    @Test
    fun greets() {
        assertEquals("Hello from jrs", greeting())
    }
}
"#;

const STARTER_LIBRARY_KOTLIN: &str = r#"package com.example

object Library {
    fun greeting(name: String): String = "Hello, $name"
}
"#;

const STARTER_LIBRARY_TEST_KOTLIN: &str = r#"package com.example

import kotlin.test.Test
import kotlin.test.assertEquals

class LibraryTest {
    @Test
    fun greetsByName() {
        assertEquals("Hello, jrs", Library.greeting("jrs"))
    }
}
"#;

const STARTER_MAIN_SCALA: &str = r#"package com.example

object Main:
  def main(args: Array[String]): Unit =
    println(greeting)

  def greeting: String = "Hello from jrs"
"#;

const STARTER_MAIN_SUITE_SCALA: &str = r#"package com.example

class MainSuite extends munit.FunSuite:
  test("greets") {
    assertEquals(Main.greeting, "Hello from jrs")
  }
"#;

const STARTER_LIBRARY_SCALA: &str = r#"package com.example

object Library:
  def greeting(name: String): String = s"Hello, $name"
"#;

const STARTER_LIBRARY_SUITE_SCALA: &str = r#"package com.example

class LibrarySuite extends munit.FunSuite:
  test("greets by name") {
    assertEquals(Library.greeting("jrs"), "Hello, jrs")
  }
"#;

const STARTER_MAIN_SPEC_GROOVY: &str = r#"package com.example

import spock.lang.Specification

class MainSpec extends Specification {
    def "greets"() {
        expect:
        Main.greeting() == "Hello from jrs"
    }
}
"#;

const STARTER_LIBRARY_SPEC_GROOVY: &str = r#"package com.example

import spock.lang.Specification

class LibrarySpec extends Specification {
    def "greets by name"() {
        expect:
        Library.greeting(name) == greeting

        where:
        name  | greeting
        "jrs" | "Hello, jrs"
        "you" | "Hello, you"
    }
}
"#;

/// What `jrs init` writes for a language: the main class, the test
/// dependencies, and two files — the starter and its test — relative to the
/// project root.
struct Starter {
    main_class: &'static str,
    dev_dependencies: Vec<Dependency>,
    files: [(&'static str, &'static str); 2],
}

fn starter(language: Language, lib: bool) -> Starter {
    let junit = || Dependency::new("org.junit.jupiter", "junit-jupiter", STARTER_JUNIT);
    let version = language.starter_version().unwrap_or_default();
    match (language, lib) {
        (Language::Java, false) => Starter {
            main_class: "com.example.Main",
            dev_dependencies: vec![junit()],
            files: [
                ("src/main/java/com/example/Main.java", STARTER_MAIN),
                ("src/test/java/com/example/MainTest.java", STARTER_MAIN_TEST),
            ],
        },
        (Language::Java, true) => Starter {
            main_class: "com.example.Main",
            dev_dependencies: vec![junit()],
            files: [
                ("src/main/java/com/example/Library.java", STARTER_LIBRARY),
                (
                    "src/test/java/com/example/LibraryTest.java",
                    STARTER_LIBRARY_TEST,
                ),
            ],
        },
        (Language::Kotlin, _) => Starter {
            // A `main` at file level compiles to a class named after the file.
            main_class: "com.example.MainKt",
            dev_dependencies: vec![
                junit(),
                Dependency::new("org.jetbrains.kotlin", "kotlin-test-junit5", version),
            ],
            files: if lib {
                [
                    (
                        "src/main/kotlin/com/example/Library.kt",
                        STARTER_LIBRARY_KOTLIN,
                    ),
                    (
                        "src/test/kotlin/com/example/LibraryTest.kt",
                        STARTER_LIBRARY_TEST_KOTLIN,
                    ),
                ]
            } else {
                [
                    ("src/main/kotlin/com/example/Main.kt", STARTER_MAIN_KOTLIN),
                    (
                        "src/test/kotlin/com/example/MainTest.kt",
                        STARTER_MAIN_TEST_KOTLIN,
                    ),
                ]
            },
        },
        (Language::Scala, _) => Starter {
            main_class: "com.example.Main",
            dev_dependencies: vec![Dependency::new("org.scalameta", "munit_3", STARTER_MUNIT)],
            files: if lib {
                [
                    (
                        "src/main/scala/com/example/Library.scala",
                        STARTER_LIBRARY_SCALA,
                    ),
                    (
                        "src/test/scala/com/example/LibrarySuite.scala",
                        STARTER_LIBRARY_SUITE_SCALA,
                    ),
                ]
            } else {
                [
                    ("src/main/scala/com/example/Main.scala", STARTER_MAIN_SCALA),
                    (
                        "src/test/scala/com/example/MainSuite.scala",
                        STARTER_MAIN_SUITE_SCALA,
                    ),
                ]
            },
        },
        // Groovy usually arrives for its tests: Java main code, Spock specs,
        // and Groovy declared as a test dependency so it stays out of the jar.
        (Language::Groovy, _) => Starter {
            main_class: "com.example.Main",
            dev_dependencies: vec![
                Dependency::new("org.apache.groovy", "groovy", version),
                Dependency::new("org.spockframework", "spock-core", STARTER_SPOCK),
            ],
            files: if lib {
                [
                    ("src/main/java/com/example/Library.java", STARTER_LIBRARY),
                    (
                        "src/test/groovy/com/example/LibrarySpec.groovy",
                        STARTER_LIBRARY_SPEC_GROOVY,
                    ),
                ]
            } else {
                [
                    ("src/main/java/com/example/Main.java", STARTER_MAIN),
                    (
                        "src/test/groovy/com/example/MainSpec.groovy",
                        STARTER_MAIN_SPEC_GROOVY,
                    ),
                ]
            },
        },
    }
}

pub(super) fn init(
    ui: &Ui,
    name: Option<&str>,
    lib: bool,
    language: Language,
    check: bool,
    format: bool,
    path: Option<&Path>,
) -> Result<i32> {
    ui.banner();

    // Groovy's starter keeps its main code in Java; Kotlin's and Scala's have
    // no Java sources for PMD to read.
    if check && matches!(language, Language::Kotlin | Language::Scala) {
        return Err(JrsError::usage(format!(
            "`--check` scaffolds PMD, which checks Java sources, and the {language} starter \
             has none"
        )));
    }
    if format && language == Language::Scala {
        return Err(JrsError::usage(
            "`--format` scaffolds google-java-format for Java and ktfmt for Kotlin; there is \
             no Scala formatter to scaffold"
                .to_string(),
        ));
    }

    let root = path.map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    std::fs::create_dir_all(&root).path(&root)?;

    let name = match name {
        Some(n) => n.to_string(),
        None => root
            .canonicalize()
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_else(|| "app".to_string()),
    };

    let manifest_path = root.join(MANIFEST_FILE);
    if manifest_path.exists() {
        return Err(JrsError::usage(format!(
            "{} already exists",
            manifest_path.display()
        )));
    }

    let starter = starter(language, lib);
    let mut manifest = manifest::blank(&name, "0.1.0", &root);
    if !lib {
        manifest.main_class = Some(starter.main_class.to_string());
    }
    if let Ok(toolchain) = Toolchain::discover() {
        manifest.java.source = Some(toolchain.version);
    }
    if let Some(version) = language.starter_version() {
        let key = language.key();
        manifest.languages.push(LanguageConfig {
            language,
            version: version.to_string(),
            source_dir: PathBuf::from(format!("src/main/{key}")),
            test_dir: PathBuf::from(format!("src/test/{key}")),
            compiler_args: Vec::new(),
            compiler_jvm_args: Vec::new(),
            plugins: Vec::new(),
        });
    }
    manifest.dev_dependencies = starter.dev_dependencies;
    let mut scaffolded = Vec::new();
    if check {
        scaffolded.push(checkers::pmd(
            "check",
            checkers::Tool::Pmd.default_version(),
            &["src/main/java".to_string()],
            "rulesets/java/quickstart.xml",
        )?);
    }
    if format {
        scaffolded.extend(if language == Language::Kotlin {
            // Kotlin's own four-space style, as the starters are written.
            checkers::ktfmt(
                checkers::Tool::Ktfmt.default_version(),
                Some("--kotlinlang-style"),
                &["src/main/kotlin".to_string(), "src/test/kotlin".to_string()],
            )?
        } else {
            // AOSP's four spaces, as the starters are written.
            checkers::google_java_format(checkers::Tool::GoogleJavaFormat.default_version(), true)?
        });
    }
    for checkers::Scaffolded { task, hooked } in scaffolded {
        if hooked {
            manifest.hooks.add(Hook::PostCompile, &task.name);
        }
        manifest.tasks.push(task);
    }
    std::fs::write(&manifest_path, manifest.render(None)).path(&manifest_path)?;
    ui.phase("Created", manifest_path.display());

    for (relative, contents) in starter.files {
        let path = root.join(relative);
        if path.exists() {
            continue;
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).path(parent)?;
        }
        std::fs::write(&path, contents).path(&path)?;
        ui.phase("Created", path.display());
    }

    ui.phase("Next", if lib { "jrs test" } else { "jrs run" });
    Ok(exit::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::LangArg;
    use crate::manifest::Manifest;
    use crate::project::Project;
    use crate::ui::{CharsetChoice, UiOptions, When};

    #[test]
    fn init_scaffolds_each_language_as_a_project_jrs_can_build() {
        let ui = Ui::new(UiOptions {
            quiet: true,
            progress: When::Never,
            color: When::Never,
            charset: CharsetChoice::Ascii,
            ..UiOptions::default()
        });
        for (lang, lib) in [
            (LangArg::Java, false),
            (LangArg::Kotlin, false),
            (LangArg::Kotlin, true),
            (LangArg::Scala, false),
            (LangArg::Scala, true),
            (LangArg::Groovy, false),
            (LangArg::Groovy, true),
        ] {
            let language = Language::from(lang);
            let root = std::env::temp_dir().join(format!(
                "jrs-init-{}-{lib}-{}",
                language.key(),
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&root);
            init(&ui, Some("app"), lib, language, false, false, Some(&root)).unwrap();

            let manifest = Manifest::load(root.join(MANIFEST_FILE)).unwrap();
            assert!(manifest.warnings.is_empty(), "{:?}", manifest.warnings);
            let project = Project::new(&manifest);
            let main = project.main_sources().unwrap();
            let test = project.test_sources().unwrap();
            assert_eq!(main.len(), 1, "{language}");
            assert_eq!(test.len(), 1, "{language}");
            match language {
                Language::Java => {
                    assert!(manifest.languages.is_empty());
                    assert_eq!(main.foreign(), None);
                }
                Language::Groovy => {
                    // Java main code, Groovy specs, Groovy off the runtime.
                    assert_eq!(main.foreign(), None);
                    assert_eq!(test.foreign(), Some(Language::Groovy));
                    assert!(manifest.implied_dependencies().is_empty());
                }
                other => {
                    assert_eq!(main.foreign(), Some(other));
                    assert_eq!(test.foreign(), Some(other));
                    assert!(!manifest.implied_dependencies().is_empty());
                }
            }
            assert!(manifest.language(language).is_some() || language == Language::Java);
            if lib {
                assert_eq!(manifest.main_class, None);
            } else if language == Language::Kotlin {
                assert_eq!(manifest.main_class.as_deref(), Some("com.example.MainKt"));
            }
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    #[test]
    fn init_check_hooks_a_pmd_task_into_post_compile() {
        let ui = Ui::new(UiOptions {
            quiet: true,
            progress: When::Never,
            color: When::Never,
            charset: CharsetChoice::Ascii,
            ..UiOptions::default()
        });
        let scratch = |what: &str| {
            let root =
                std::env::temp_dir().join(format!("jrs-init-check-{what}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            root
        };
        for (language, lib) in [
            (Language::Java, false),
            (Language::Java, true),
            (Language::Groovy, false),
        ] {
            let root = scratch(&format!("{}-{lib}", language.key()));
            init(&ui, Some("app"), lib, language, true, false, Some(&root)).unwrap();

            let manifest = Manifest::load(root.join(MANIFEST_FILE)).unwrap();
            assert!(manifest.warnings.is_empty(), "{:?}", manifest.warnings);
            let check = manifest.tasks.iter().find(|t| t.name == "check").unwrap();
            assert_eq!(
                check.action,
                Some(manifest::Action::Main(
                    "net.sourceforge.pmd.cli.PmdCli".to_string()
                ))
            );
            let tools: Vec<String> = check
                .dependencies
                .iter()
                .map(|d| d.key().to_string())
                .collect();
            assert_eq!(
                tools,
                [
                    "net.sourceforge.pmd:pmd-cli",
                    "net.sourceforge.pmd:pmd-java"
                ]
            );
            assert_eq!(manifest.hooks.tasks(Hook::PostCompile), ["check"]);
            // The tool's graph is its own: none of PMD joins the project's.
            assert!(manifest.dependencies.is_empty());
            let _ = std::fs::remove_dir_all(&root);
        }
        for language in [Language::Kotlin, Language::Scala] {
            let root = scratch(language.key());
            let err =
                init(&ui, Some("app"), false, language, true, false, Some(&root)).unwrap_err();
            assert!(err.to_string().contains("--check"), "{err}");
            assert!(!root.join(MANIFEST_FILE).exists());
            let _ = std::fs::remove_dir_all(&root);
        }

        // `--format`: the language's formatter, with only its check hooked.
        for (language, main) in [
            (Language::Java, "com.google.googlejavaformat.java.Main"),
            (Language::Kotlin, "com.facebook.ktfmt.cli.Main"),
        ] {
            let root = scratch(&format!("format-{}", language.key()));
            init(&ui, Some("app"), false, language, false, true, Some(&root)).unwrap();
            let manifest = Manifest::load(root.join(MANIFEST_FILE)).unwrap();
            assert!(manifest.warnings.is_empty(), "{:?}", manifest.warnings);
            let names: Vec<&str> = manifest.tasks.iter().map(|t| t.name.as_str()).collect();
            assert_eq!(names, ["format", "format-check"]);
            assert_eq!(
                manifest.tasks[0].action,
                Some(manifest::Action::Main(main.to_string()))
            );
            assert_eq!(manifest.hooks.tasks(Hook::PostCompile), ["format-check"]);
            let _ = std::fs::remove_dir_all(&root);
        }
        let root = scratch("format-scala");
        let err = init(
            &ui,
            Some("app"),
            false,
            Language::Scala,
            false,
            true,
            Some(&root),
        )
        .unwrap_err();
        assert!(err.to_string().contains("--format"), "{err}");
        let _ = std::fs::remove_dir_all(&root);
    }
}
