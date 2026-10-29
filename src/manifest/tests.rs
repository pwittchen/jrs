use super::*;

fn parse(text: &str) -> Result<Manifest> {
    Manifest::parse(text, Path::new("/p/jrs.toml"), Path::new("/p"))
}

const FULL: &str = r#"
[project]
name = "my-app"
version = "1.0.0"
main-class = "com.example.Main"

[java]
source = 21
target = 21
encoding = "UTF-8"
javac-args = ["-Xlint:all", "-Werror"]

[dependencies]
"com.google.guava:guava" = "33.0.0-jre"
"org.apache.commons:commons-lang3" = { version = "3.14.0" }

[dev-dependencies]
"org.junit.jupiter:junit-jupiter" = "5.10.2"

[repositories]
internal = "https://nexus.example.com/repository/maven-public/"
"#;

#[test]
fn parses_the_full_example_from_the_spec() {
    let m = parse(FULL).unwrap();
    assert_eq!(m.name, "my-app");
    assert_eq!(m.version, "1.0.0");
    assert_eq!(m.main_class.as_deref(), Some("com.example.Main"));
    assert_eq!(m.java.source, Some(21));
    assert_eq!(m.java.javac_args, vec!["-Xlint:all", "-Werror"]);
    assert_eq!(m.jar_name(), "my-app-1.0.0.jar");
    assert!(m.warnings.is_empty(), "{:?}", m.warnings);
}

#[test]
fn short_and_long_dependency_forms_agree() {
    let m = parse(FULL).unwrap();
    assert_eq!(
        m.dependencies,
        vec![
            Dependency::new("com.google.guava", "guava", "33.0.0-jre"),
            Dependency::new("org.apache.commons", "commons-lang3", "3.14.0"),
        ]
    );
    assert_eq!(m.dev_dependencies.len(), 1);
}

#[test]
fn a_classifier_may_name_the_hosts_platform() {
    let base = "[project]\nname = 'a'\nversion = '1'\n";
    let m = parse(&format!(
        "{base}[dependencies]\n\
         'io.netty:netty-transport-native-epoll:{{os-classifier}}' = '4.1.115.Final'\n\
         'io.netty:netty-tcnative-boringssl-static' = \
         {{ version = '2.0.69.Final', classifier = '{{os-classifier}}' }}\n"
    ))
    .unwrap();
    for d in &m.dependencies {
        assert_eq!(d.classifier.as_deref(), Some("{os-classifier}"));
    }
    let err = parse(&format!("{base}[dependencies]\n'g:a:{{os}}' = '1'\n"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("placeholder jrs does not know"), "{err}");
}

#[test]
fn the_long_form_carries_classifiers_exclusions_and_compile_only() {
    let m = parse(
        r#"
[project]
name = "a"
version = "1"
[dependencies]
"org.lwjgl:lwjgl" = "3.3.3"
"org.lwjgl:lwjgl:natives-linux" = "3.3.3"
"io.netty:netty-transport-native-epoll" = { version = "4.1.100.Final", classifier = "linux-x86_64" }
"com.google.guava:guava" = { version = "33.0.0-jre", exclusions = ["com.google.code.findbugs:jsr305", "org.checkerframework:*"] }
"jakarta.servlet:jakarta.servlet-api" = { version = "6.0.0", compile-only = true }
"#,
    )
    .unwrap();
    let d = &m.dependencies;
    assert_eq!(d[0].classifier, None);
    assert_eq!(d[1].classifier.as_deref(), Some("natives-linux"));
    assert_eq!(d[1].key(), "org.lwjgl:lwjgl:natives-linux");
    assert_eq!(d[2].classifier.as_deref(), Some("linux-x86_64"));
    assert_eq!(
        d[3].exclusions,
        vec![
            Exclusion {
                group: "com.google.code.findbugs".into(),
                artifact: "jsr305".into()
            },
            Exclusion {
                group: "org.checkerframework".into(),
                artifact: "*".into()
            },
        ]
    );
    assert!(d[4].compile_only);
    assert!(!d[3].compile_only);

    // Rendering round-trips, classifiers and all.
    let again = parse(&m.render(None)).unwrap();
    assert_eq!(again.dependencies, m.dependencies);
}

#[test]
fn malformed_long_forms_name_the_key() {
    let base = "[project]\nname='a'\nversion='1'\n";
    let err = parse(&format!(
        "{base}[dependencies]\n'g:a' = {{ version = '1', exclusions = ['nope'] }}"
    ))
    .unwrap_err();
    assert!(err.to_string().contains("exclusions"), "{err}");
    let err = parse(&format!(
        "{base}[dependencies]\n'g:a' = {{ version = '1', compile-only = 'yes' }}"
    ))
    .unwrap_err();
    assert!(err.to_string().contains("compile-only"), "{err}");
    let err = parse(&format!(
        "{base}[dev-dependencies]\n'g:a' = {{ version = '1', compile-only = true }}"
    ))
    .unwrap_err();
    assert!(err.to_string().contains("[dependencies]"), "{err}");
    let err = parse(&format!(
        "{base}[dependencies]\n'g:a:x' = {{ version = '1', classifier = 'y' }}"
    ))
    .unwrap_err();
    assert!(err.to_string().contains("classifier"), "{err}");
    let err = parse(&format!(
        "{base}[dependencies]\n'g:a' = {{ version = '1', scope = 'provided' }}"
    ))
    .unwrap_err();
    assert!(err.to_string().contains("compile-only"), "{err}");
}

#[test]
fn jvm_arguments_and_the_pinned_jdk_are_read() {
    let m = parse(
        "[project]\nname='a'\nversion='1'\n[java]\njdk = 21\n\
         [run]\njvm-args = ['-Xmx256m', '--enable-preview']\n\
         [test]\njvm-args = ['-Dmode=test']\njacoco-version = '0.8.15'\nforks = 4\n\
         [package]\nadd-modules = ['jdk.crypto.ec']",
    )
    .unwrap();
    assert!(m.warnings.is_empty(), "{:?}", m.warnings);
    assert_eq!(m.java.jdk, Some(21));
    assert_eq!(m.run.jvm_args, vec!["-Xmx256m", "--enable-preview"]);
    assert_eq!(m.test.jvm_args, vec!["-Dmode=test"]);
    assert_eq!(m.test.jacoco_version.as_deref(), Some("0.8.15"));
    assert_eq!(m.test.forks(), Some(4));
    assert_eq!(m.package.add_modules, vec!["jdk.crypto.ec"]);

    let again = parse(&m.render(None)).unwrap();
    assert_eq!(again.java, m.java);
    assert_eq!(again.run, m.run);
    assert_eq!(again.test, m.test);
    assert_eq!(again.package, m.package);

    let err = parse("[project]\nname='a'\nversion='1'\n[run]\njvm-args = '-Xmx1g'").unwrap_err();
    assert!(err.to_string().contains("run.jvm-args"), "{err}");

    let one = parse("[project]\nname='a'\nversion='1'\n").unwrap();
    assert_eq!(one.test.forks(), None, "jrs picks when the key is not set");
    for bad in ["forks = 0", "forks = -2", "forks = 'auto'"] {
        let err = parse(&format!("[project]\nname='a'\nversion='1'\n[test]\n{bad}"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("`test.forks` must be"), "{bad}: {err}");
    }
}

const DEMO: &str = "[project]\nname = \"demo\"\nversion = \"2.1.0\"\n\n";

fn attributes(m: &Manifest) -> Vec<(String, String)> {
    m.jar_attributes().unwrap()
}

fn pairs(items: &[(&str, &str)]) -> Vec<(String, String)> {
    items
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

#[test]
fn test_suites_parse_render_and_refuse_what_they_cannot_be() {
    let m = parse(&format!(
        "{DEMO}[test]\njvm-args = [\"-Dunit=1\"]\n\n[test.suites.e2e]\n\
         test-dir = \"src/e2e/java\"\njvm-args = [\"-Dheadless=true\"]\n\
         env = {{ MODE = \"{{project.version}}\" }}\nforks = 2\n\n\
         [test.suites.integration]\ntest-dir = \"it/java\"\ntest-resource-dir = \"it/data\"\n"
    ))
    .unwrap();
    assert!(m.warnings.is_empty(), "{:?}", m.warnings);
    let e2e = m.test.suite("e2e").unwrap();
    assert_eq!(e2e.test_dir, PathBuf::from("src/e2e/java"));
    assert_eq!(e2e.test_resource_dir, PathBuf::from("src/e2e/resources"));
    assert_eq!(e2e.jvm_args, vec!["-Dheadless=true"]);
    assert_eq!(e2e.forks(), Some(2));
    let it = m.test.suite("integration").unwrap();
    assert_eq!(it.test_resource_dir, PathBuf::from("it/data"));
    assert_eq!(it.forks(), None);

    let text = m.render(None);
    assert!(
        text.contains("\n[test.suites.e2e]\ntest-dir = \"src/e2e/java\"\n"),
        "{text}"
    );
    assert!(
        !text.contains("src/e2e/resources"),
        "the default is not written: {text}"
    );
    assert_eq!(parse(&text).unwrap().test, m.test);

    for (body, expected) in [
        ("[test.suites.E2E]\ntest-dir = \"x\"", "a suite name is"),
        (
            "[test.suites.e2e]\njvm-args = []",
            "`test.suites.e2e.test-dir` is required",
        ),
        (
            "[test.suites.e2e]\ntest-dir = \"x\"\nforks = 0",
            "whole number",
        ),
    ] {
        let err = parse(&format!("{DEMO}{body}\n")).unwrap_err().to_string();
        assert!(err.contains(expected), "{body}: {err}");
    }
}

#[test]
fn resources_name_files_to_expand_and_their_properties() {
    let m = parse(&format!(
        "{DEMO}[resources]\nexpand = [\"application.yml\", \"config/**/*.properties\"]\n\
         properties = {{ version = \"{{project.version}}\", \"app.group\" = \"com.example\" }}\n"
    ))
    .unwrap();
    assert!(m.warnings.is_empty(), "{:?}", m.warnings);
    assert_eq!(
        m.resources.expand,
        vec!["application.yml", "config/**/*.properties"]
    );
    assert_eq!(
        m.resource_properties().unwrap(),
        pairs(&[
            ("project.name", "demo"),
            ("project.version", "2.1.0"),
            ("version", "2.1.0"),
            ("app.group", "com.example"),
        ])
    );

    // Rendered back, it reads the same.
    let text = m.render(None);
    assert!(
        text.contains(
            "\n[resources]\nexpand = [\"application.yml\", \"config/**/*.properties\"]\n"
        ),
        "{text}"
    );
    assert_eq!(parse(&text).unwrap().resources, m.resources);

    // Without the table there is nothing to render.
    assert!(!parse(DEMO).unwrap().render(None).contains("[resources]"));
}

#[test]
fn resources_refuse_what_cannot_be_expanded() {
    for (body, expected) in [
        ("expand = [\"../secrets.yml\"]", "must be a relative"),
        ("expand = [\"/abs.yml\"]", "must be a relative"),
        ("expand = [\"\"]", "must be a relative"),
        (
            "expand = [\"a.yml\"]\nproperties = { cp = \"{classpath}\" }",
            "is not known when resources are copied",
        ),
        (
            "expand = [\"a.yml\"]\nproperties = { \"project.version\" = \"1\" }",
            "always set",
        ),
        (
            "expand = [\"a.yml\"]\nproperties = { \"a b\" = \"1\" }",
            "a property name is",
        ),
        ("properties = { v = \"1\" }", "names no file"),
    ] {
        let err = parse(&format!("{DEMO}[resources]\n{body}\n"))
            .unwrap_err()
            .to_string();
        assert!(err.contains(expected), "{body}: {err}");
    }
}

#[test]
fn jar_attributes_keep_declaration_order_and_expand_placeholders() {
    let m = parse(&format!(
        "{DEMO}[package]\nnative-image-args = [\"--no-fallback\"]\n\n\
         [package.manifest]\nZ-Last-Alphabetically = \"first\"\n\
         Implementation-Title = \"{{project.name}}\"\n\
         Implementation-Version = \"{{project.version}}\"\n\
         Add-Opens = \"java.base/java.lang {{{{braces}}}}\"\n"
    ))
    .unwrap();
    assert!(m.warnings.is_empty(), "{:?}", m.warnings);
    assert_eq!(
        attributes(&m),
        pairs(&[
            ("Z-Last-Alphabetically", "first"),
            ("Implementation-Title", "demo"),
            ("Implementation-Version", "2.1.0"),
            ("Add-Opens", "java.base/java.lang {braces}"),
        ])
    );
    assert_eq!(m.package.native_image_args, vec!["--no-fallback"]);

    // Rendered back, it reads the same, placeholders and order included.
    let text = m.render(None);
    assert!(text.contains("\n[package.manifest]\nZ-Last-Alphabetically = \"first\"\n"));
    let again = parse(&text).unwrap();
    assert_eq!(again.package, m.package);
}

#[test]
fn relocations_keep_declaration_order_and_render_back() {
    let m = parse(&format!(
        "{DEMO}[package.relocate]\n\"org.slf4j\" = {{ to = \"com.example.shaded.slf4j\", \
         exclude = [\"org.slf4j.impl.*\", \"org.slf4j.Marker\"] }}\n\
         \"com.google.common\" = \"com.example.shaded.guava\"\n"
    ))
    .unwrap();
    assert!(m.warnings.is_empty(), "{:?}", m.warnings);
    assert_eq!(
        m.package.relocate,
        [
            Relocation {
                from: "org.slf4j".into(),
                to: "com.example.shaded.slf4j".into(),
                exclude: vec!["org.slf4j.impl.*".into(), "org.slf4j.Marker".into()],
            },
            Relocation {
                from: "com.google.common".into(),
                to: "com.example.shaded.guava".into(),
                exclude: Vec::new(),
            },
        ]
    );
    let text = m.render(None);
    assert!(
        text.contains(
            "\n[package.relocate]\n\"org.slf4j\" = { to = \"com.example.shaded.slf4j\", \
             exclude = [\"org.slf4j.impl.*\", \"org.slf4j.Marker\"] }\n\
             \"com.google.common\" = \"com.example.shaded.guava\"\n"
        ),
        "{text}"
    );
    assert_eq!(parse(&text).unwrap().package, m.package);
}

#[test]
fn a_relocation_moves_a_package_somewhere_it_can_go() {
    for (entry, expected) in [
        ("\"com..x\" = \"y\"", "`com..x` is not a package name"),
        ("\"com.x\" = \"1y\"", "`1y` is not a package name"),
        ("\"com.x\" = \"shaded.é\"", "in ASCII"),
        ("\"java.util\" = \"shaded.util\"", "belongs to the JDK"),
        ("\"com.x\" = \"java.x\"", "belongs to the JDK"),
        ("\"com.x\" = \"com.x\"", "to itself"),
        ("\"com.x\" = 3", "must be the package"),
        ("\"com.x\" = { exclude = [] }", "needs `to`"),
        (
            "\"com.x\" = { to = \"y\", exclude = [\"org.other.*\"] }",
            "`org.other.*` is not in `com.x`",
        ),
        (
            "\"com.x\" = { to = \"y\", exclude = \"com.x.A\" }",
            "array of strings",
        ),
    ] {
        let err = parse(&format!("{DEMO}[package.relocate]\n{entry}\n"))
            .unwrap_err()
            .to_string();
        assert!(err.contains(expected), "{entry}: {err}");
        assert!(err.contains("package.relocate.\""), "{entry}: {err}");
    }
}

#[test]
fn an_unknown_relocation_key_warns() {
    let m = parse(&format!(
        "{DEMO}[package.relocate]\n\"com.x\" = {{ to = \"y\", includes = [\"com.x.A\"] }}\n"
    ))
    .unwrap();
    assert!(
        m.warnings
            .iter()
            .any(|w| w.contains("package.relocate.\"com.x\".includes")),
        "{:?}",
        m.warnings
    );
}

#[test]
fn obfuscate_is_off_until_the_table_turns_it_on() {
    assert!(parse(DEMO).unwrap().obfuscate.is_none());
}

#[test]
fn obfuscate_pins_the_version_and_carries_keep_and_args() {
    let m = parse(&format!(
        "{DEMO}[obfuscate]\nversion = \"7.6.1\"\nkeep = [\"com.example.Api\"]\n\
         proguard-args = [\"-dontwarn org.foo.**\"]\n"
    ))
    .unwrap();
    assert!(m.warnings.is_empty(), "{:?}", m.warnings);
    let obf = m.obfuscate.as_ref().unwrap();
    assert_eq!(obf.version, "7.6.1");
    assert_eq!(obf.keep, vec!["com.example.Api"]);
    assert_eq!(obf.proguard_args, vec!["-dontwarn org.foo.**"]);
    // Rendered back, it reads the same.
    let again = parse(&m.render(None)).unwrap();
    assert_eq!(again.obfuscate, m.obfuscate);
}

#[test]
fn obfuscate_needs_a_version() {
    let err = parse(&format!("{DEMO}[obfuscate]\nkeep = [\"x\"]\n")).unwrap_err();
    assert_eq!(err.exit_code(), 2);
    assert!(err.to_string().contains("obfuscate.version"), "{err}");
}

#[test]
fn an_unknown_obfuscate_key_warns() {
    let m = parse(&format!(
        "{DEMO}[obfuscate]\nversion = \"7.6.1\"\nmangle = true\n"
    ))
    .unwrap();
    assert!(
        m.warnings.iter().any(|w| w.contains("obfuscate.mangle")),
        "{:?}",
        m.warnings
    );
}

#[test]
fn the_inline_table_form_reads_the_same() {
    let m = parse(&format!(
        "{DEMO}[package]\nmanifest = {{ B-Second = \"2\", A-First = \"1\" }}\n"
    ))
    .unwrap();
    assert_eq!(
        attributes(&m),
        pairs(&[("B-Second", "2"), ("A-First", "1")])
    );
}

#[test]
fn attributes_jrs_writes_itself_are_refused() {
    for (name, hint) in [
        ("Main-Class", "project.main-class"),
        ("class-path", "--portable"),
        ("Created-By", "writes it itself"),
        ("MANIFEST-VERSION", "writes it itself"),
        ("Name", "per-entry section"),
    ] {
        let err = parse(&format!("{DEMO}[package.manifest]\n{name} = \"x\"\n")).unwrap_err();
        assert_eq!(err.exit_code(), 2);
        let text = err.to_string();
        assert!(
            text.contains(&format!("`package.manifest.{name}`")),
            "{text}"
        );
        assert!(text.contains("belongs to jrs"), "{text}");
        assert!(text.contains(hint), "{text}");
    }
}

#[test]
fn attribute_names_follow_the_jar_specification() {
    let longest = "A".repeat(70);
    for ok in ["X", "X_1-a", "Premain-Class", "9-Lives", longest.as_str()] {
        let m = parse(&format!("{DEMO}[package.manifest]\n\"{ok}\" = \"v\"\n")).unwrap();
        assert_eq!(attributes(&m)[0].0, ok);
    }
    let too_long = "A".repeat(71);
    for bad in [
        "-Leading",
        "_Leading",
        "Has.Dot",
        "Has Space",
        "Ümlaut",
        too_long.as_str(),
    ] {
        let err = parse(&format!("{DEMO}[package.manifest]\n\"{bad}\" = \"v\"\n")).unwrap_err();
        assert!(err.to_string().contains("attribute name"), "{bad}: {err}");
    }
}

#[test]
fn attribute_values_are_one_line_strings() {
    for (value, complaint) in [
        ("\"a\\nb\"", "one line"),
        ("1", "must be a string"),
        ("\"{classpath}\"", "not known when a jar is written"),
        ("\"{nope}\"", "unknown placeholder"),
        ("\"{unclosed\"", "unclosed"),
    ] {
        let err = parse(&format!("{DEMO}[package.manifest]\nX-Value = {value}\n")).unwrap_err();
        assert!(err.to_string().contains(complaint), "{value}: {err}");
    }
    let err = parse(&format!("{DEMO}[package]\nmanifest = [\"x\"]\n")).unwrap_err();
    assert!(err.to_string().contains("must be a table"), "{err}");
}

#[test]
fn attribute_names_are_case_insensitive() {
    let err = parse(&format!(
        "{DEMO}[package.manifest]\nX-Flavour = \"a\"\nx-flavour = \"b\"\n"
    ))
    .unwrap_err();
    assert!(
        err.to_string().contains("`X-Flavour` is already set"),
        "{err}"
    );
}

#[test]
fn runtime_only_and_local_jars_parse_and_render_back() {
    let m = parse(
        r#"
[project]
name = "a"
version = "1"
[dependencies]
"org.postgresql:postgresql" = { version = "42.7.3", runtime-only = true }
ojdbc = { path = "libs/ojdbc11.jar" }
"vendor-api" = { path = 'libs\vendor api.jar', compile-only = true }
[dev-dependencies]
fixtures = { path = "test-libs/fixtures.jar" }
"#,
    )
    .unwrap();
    assert!(m.warnings.is_empty(), "{:?}", m.warnings);
    assert!(m.dependencies[0].runtime_only);
    assert!(!m.dependencies[0].is_plain());
    let ojdbc = &m.dependencies[1];
    assert!(ojdbc.is_local());
    assert_eq!(ojdbc.key(), "ojdbc");
    assert_eq!(ojdbc.path.as_deref(), Some("libs/ojdbc11.jar"));
    assert_eq!(
        m.dependencies[2].path.as_deref(),
        Some("libs/vendor api.jar"),
        "a backslash becomes a slash"
    );
    assert!(m.dependencies[2].compile_only);
    assert!(m.dev_dependencies[0].is_local());
    assert!(m.effective_dependencies().iter().any(Dependency::is_local));

    let text = m.render(None);
    assert!(
        text.contains("\"ojdbc\" = { path = \"libs/ojdbc11.jar\" }"),
        "{text}"
    );
    assert!(
        text.contains("{ version = \"42.7.3\", runtime-only = true }"),
        "{text}"
    );
    let again = parse(&text).unwrap();
    assert_eq!(again.dependencies, m.dependencies);
    assert_eq!(again.dev_dependencies, m.dev_dependencies);
}

#[test]
fn managed_versions_and_boms_parse_and_render_back() {
    let m = parse(
        r#"
[project]
name = "a"
version = "1"
[managed]
"org.springframework.boot:spring-boot-dependencies" = { version = "3.3.4", bom = true }
"com.fasterxml.jackson.core:jackson-databind" = "2.17.2"
[dependencies]
"org.springframework.boot:spring-boot-starter-web" = {}
"org.slf4j:slf4j-api" = { exclusions = ["x:y"] }
[dev-dependencies]
"org.springframework.boot:spring-boot-starter-test" = {}
"#,
    )
    .unwrap();
    assert!(m.warnings.is_empty(), "{:?}", m.warnings);
    assert_eq!(m.managed.len(), 2);
    assert!(m.managed[0].bom && !m.managed[1].bom);
    assert_eq!(
        m.managed[1].key(),
        "com.fasterxml.jackson.core:jackson-databind"
    );
    let web = &m.dependencies[0];
    assert!(web.is_managed() && !m.dependencies[0].is_local());
    assert_eq!(
        web.to_string(),
        "org.springframework.boot:spring-boot-starter-web"
    );
    assert!(m.dev_dependencies[0].is_managed());

    let text = m.render(None);
    assert!(
        text.contains(
            "\n[managed]\n\
             \"org.springframework.boot:spring-boot-dependencies\" = { version = \"3.3.4\", bom = true }\n\
             \"com.fasterxml.jackson.core:jackson-databind\" = \"2.17.2\"\n\n[dependencies]\n"
        ),
        "{text}"
    );
    assert!(
        text.contains("\"org.springframework.boot:spring-boot-starter-web\" = {}\n"),
        "{text}"
    );
    assert!(
        text.contains("\"org.slf4j:slf4j-api\" = { exclusions = [\"x:y\"] }\n"),
        "{text}"
    );
    let again = parse(&text).unwrap();
    assert_eq!(again.managed, m.managed);
    assert_eq!(again.dependencies, m.dependencies);
    assert_eq!(again.dev_dependencies, m.dev_dependencies);
}

#[test]
fn a_task_runs_a_class_from_dependencies_of_its_own() {
    let m = parse(
        r#"
[project]
name = "a"
version = "1"
[tasks.format]
main = "com.google.googlejavaformat.java.Main"
args = ["--replace", "{root}/src/main/java"]
[tasks.format.dependencies]
"com.google.googlejavaformat:google-java-format" = "1.22.0"
"org.example:extra" = { version = "2.0", exclusions = ["x:y"] }
[tasks.gen]
script = "build/Gen.java"
[tasks.gen.dependencies]
"com.squareup:javapoet" = "1.13.0"
"#,
    )
    .unwrap();
    assert!(m.warnings.is_empty(), "{:?}", m.warnings);
    let format = m.task("format").unwrap();
    assert_eq!(
        format.action,
        Some(Action::Main("com.google.googlejavaformat.java.Main".into()))
    );
    assert_eq!(format.dependencies.len(), 2);
    assert_eq!(format.dependencies[1].exclusions.len(), 1);
    assert_eq!(format.tool_name(), "tasks.format");
    assert_eq!(m.task("gen").unwrap().dependencies.len(), 1);
    assert!(
        m.dependencies.is_empty(),
        "a task's dependencies are not the project's"
    );

    let text = m.render(None);
    assert!(
        text.contains("main = \"com.google.googlejavaformat.java.Main\"\n"),
        "{text}"
    );
    assert!(
        text.contains(
            "\n[tasks.format.dependencies]\n\
             \"com.google.googlejavaformat:google-java-format\" = \"1.22.0\"\n"
        ),
        "{text}"
    );
    assert_eq!(parse(&text).unwrap().tasks, m.tasks);
}

#[test]
fn a_tasks_dependencies_go_only_where_they_mean_something() {
    let base = "[project]\nname='a'\nversion='1'\n";
    let err = |body: &str| parse(&format!("{base}{body}")).unwrap_err().to_string();
    for (body, needle) in [
        ("[tasks.t]\nmain = 'x.Y'", "has none"),
        (
            "[tasks.t]\nshell = 'x'\n[tasks.t.dependencies]\n'g:a' = '1'",
            "`main` or `script` action",
        ),
        (
            "[tasks.t]\nmain = 'x.Y'\n[tasks.t.dependencies]\nd = { path = 'd.jar' }",
            "a local jar",
        ),
        (
            "[tasks.t]\nmain = 'x.Y'\n[tasks.t.dependencies]\n'g:a' = { version = '1', compile-only = true }",
            "compile-only",
        ),
        (
            "[managed]\n'g:a' = '1'\n[tasks.t]\nmain = 'x.Y'\n[tasks.t.dependencies]\n'g:a' = {}",
            "[managed]",
        ),
        (
            "[tasks.t]\nmain = 'x.Y'\nscript = 'A.java'\n[tasks.t.dependencies]\n'g:a' = '1'",
            "exactly one of",
        ),
        (
            "[tasks.t]\nmain = 'not a class'\n[tasks.t.dependencies]\n'g:a' = '1'",
            "`tasks.t.main` must be a fully-qualified class name",
        ),
        (
            "[tasks.t]\nmain = 'x.Y'\n[tasks.t.dependencies]\n'g:a' = '[1,2)'",
            "",
        ),
    ] {
        // A range is refused at resolution, as in [dependencies].
        if needle.is_empty() {
            assert!(parse(&format!("{base}{body}")).is_ok(), "{body}");
            continue;
        }
        let e = err(body);
        assert!(e.contains(needle), "{body}: {e}");
    }
}

#[test]
fn a_versionless_dependency_needs_something_to_manage_it() {
    let base = "[project]\nname='a'\nversion='1'\n";
    let err = |body: &str| parse(&format!("{base}{body}")).unwrap_err().to_string();
    for (body, needle) in [
        ("[dependencies]\n'g:a' = {}", "there is no [managed] table"),
        (
            "[managed]\n'g:b' = '1'\n[dev-dependencies]\n'g:a' = {}",
            "[managed] does not name it",
        ),
        ("[dependencies]\n'g:a' = ''", "write `{}`"),
        (
            "[dependencies]\n'g:a' = { version = '' }",
            "leave `version` out",
        ),
        ("[managed]\n'g:a' = '[1,2)'", "range"),
        ("[managed]\n'g:a:natives' = '1'", "every classifier"),
        (
            "[managed]\n'g:a' = { version = '1', scope = 'x' }",
            "unknown key `scope`",
        ),
        ("[managed]\n'g:a' = { bom = true }", "missing a `version`"),
        (
            "[managed]\n'g:a' = { version = '1', bom = 'yes' }",
            "`true` or `false`",
        ),
        ("[managed]\n'g:a' = ''", "empty version"),
    ] {
        let e = err(body);
        assert!(e.contains(needle), "{body}: {e}");
    }
    // With a BOM there, what it covers is only known at resolution.
    let m = parse(&format!(
        "{base}[managed]\n'g:bom' = {{ version = '1', bom = true }}\n\
         [dependencies]\n'g:a' = {{}}"
    ))
    .unwrap();
    assert!(m.dependencies[0].is_managed());
}

#[test]
fn malformed_runtime_only_and_local_jars_name_the_problem() {
    let base = "[project]\nname='a'\nversion='1'\n";
    let err = |body: &str| parse(&format!("{base}{body}")).unwrap_err().to_string();
    for (body, needle) in [
        (
            "[dependencies]\n'g:a' = { version = '1', compile-only = true, runtime-only = true }",
            "drop both",
        ),
        (
            "[dependencies]\n'g:a' = { version = '1', runtime-only = 'yes' }",
            "runtime-only",
        ),
        (
            "[dev-dependencies]\n'g:a' = { version = '1', runtime-only = true }",
            "[dependencies]",
        ),
        (
            "[dependencies]\n'g:a' = { path = 'libs/a.jar' }",
            "name of its own",
        ),
        (
            "[dependencies]\ndriver = { path = 'libs/a.jar', version = '1' }",
            "no coordinate",
        ),
        (
            "[dependencies]\ndriver = { path = '../a.jar' }",
            "inside the project",
        ),
        (
            "[dependencies]\ndriver = { path = '/opt/a.jar' }",
            "inside the project",
        ),
        (
            "[dependencies]\ndriver = { path = '' }",
            "inside the project",
        ),
        ("[dependencies]\ndriver = 'libs/a.jar'", "group:artifact"),
        (
            "[dev-dependencies]\ndriver = { path = 'a.jar', compile-only = true }",
            "[dependencies]",
        ),
    ] {
        let message = err(body);
        assert!(message.contains(needle), "{body}: {message}");
    }
}

#[test]
fn a_repository_can_be_confined_to_groups() {
    let m = parse(
        "[project]\nname='a'\nversion='1'\n[repositories]\n\
         internal = { url = 'https://nexus.example.com/m2/', groups = ['com.acme', 'com.acme.*'] }\n\
         plain = 'https://x.example.com'",
    )
    .unwrap();
    let internal = &m.repositories[0];
    assert_eq!(internal.url, "https://nexus.example.com/m2");
    assert_eq!(internal.groups, ["com.acme", "com.acme.*"]);
    assert!(internal.claims("com.acme") && internal.claims("com.acme.billing"));
    assert!(!internal.claims("com.acmex"));
    assert!(m.repositories[1].groups.is_empty());
    assert!(!m.repositories[1].claims("com.acme"), "claims nothing");
    assert_eq!(m.repositories[2].url, CENTRAL_URL);
    let again = parse(&m.render(None)).unwrap();
    assert_eq!(again.repositories, m.repositories);

    let base = "[project]\nname='a'\nversion='1'\n[repositories]\n";
    for (body, needle) in [
        (
            "internal = { url = 'https://x', group = ['com.acme'] }",
            "unknown key `group`",
        ),
        ("internal = { groups = ['com.acme'] }", "needs a `url`"),
        ("internal = { url = 'https://x', groups = [] }", "empty"),
        (
            "internal = { url = 'https://x', groups = ['com.*.x'] }",
            "not a group",
        ),
        (
            "internal = { url = 'https://x', groups = ['*'] }",
            "not a group",
        ),
        ("internal = 3", "URL string"),
    ] {
        let message = parse(&format!("{base}{body}")).unwrap_err().to_string();
        assert!(message.contains(needle), "{body}: {message}");
    }
}

#[test]
fn group_patterns_match_a_group_or_the_groups_below_it() {
    assert!(group_matches("com.acme", "com.acme"));
    assert!(!group_matches("com.acme", "com.acme.billing"));
    assert!(group_matches("com.acme.*", "com.acme.billing"));
    assert!(group_matches("com.acme.*", "com.acme.billing.api"));
    assert!(!group_matches("com.acme.*", "com.acme"));
    assert!(!group_matches("com.acme.*", "com.acmex.billing"));
    assert!(valid_group_pattern("io.github.some-one_2.*"));
    assert!(!valid_group_pattern("com..acme"));
    assert!(!valid_group_pattern(".*"));
}

#[test]
fn declaration_order_survives_parsing() {
    // Conflict mediation breaks ties on declaration order, so this is load
    // bearing, not cosmetic.
    let m = parse(
        r#"
[project]
name = "a"
version = "1"
[dependencies]
"z.z:zeta" = "1"
"a.a:alpha" = "2"
"m.m:mu" = "3"
"#,
    )
    .unwrap();
    let keys: Vec<String> = m.dependencies.iter().map(|d| d.key()).collect();
    assert_eq!(keys, vec!["z.z:zeta", "a.a:alpha", "m.m:mu"]);
}

#[test]
fn central_is_implicit_and_always_last() {
    let m = parse(FULL).unwrap();
    assert_eq!(m.repositories.len(), 2);
    assert_eq!(m.repositories[0].name, "internal");
    assert_eq!(
        m.repositories[0].url,
        "https://nexus.example.com/repository/maven-public"
    );
    assert_eq!(m.repositories[1].url, CENTRAL_URL);

    let bare = parse("[project]\nname='a'\nversion='1'").unwrap();
    assert_eq!(
        bare.repositories,
        vec![Repository::new(CENTRAL_NAME, CENTRAL_URL)]
    );
}

#[test]
fn defaults_follow_the_maven_like_layout() {
    let m = parse("[project]\nname='a'\nversion='1'").unwrap();
    assert_eq!(m.source_dir, PathBuf::from("src/main/java"));
    assert_eq!(m.test_dir, PathBuf::from("src/test/java"));
    assert_eq!(m.resource_dir, PathBuf::from("src/main/resources"));
    assert_eq!(m.test_resource_dir, PathBuf::from("src/test/resources"));
    assert_eq!(m.target_dir, PathBuf::from("target"));
    assert_eq!(m.java.encoding, "UTF-8");
    assert_eq!(m.java.source, None);
}

#[test]
fn flat_layouts_can_override_the_source_root() {
    let m = parse("[project]\nname='a'\nversion='1'\nsource-dir='src'\ntest-dir='test'").unwrap();
    assert_eq!(m.source_path(), PathBuf::from("/p/src"));
    assert_eq!(m.test_path(), PathBuf::from("/p/test"));
}

#[test]
fn the_test_resource_directory_is_configurable() {
    let derived = parse("[project]\nname='a'\nversion='1'\ntest-dir='test/java'").unwrap();
    assert_eq!(derived.test_resource_dir, PathBuf::from("test/resources"));

    let explicit = parse("[project]\nname='a'\nversion='1'\ntest-resource-dir='fixtures'").unwrap();
    assert_eq!(explicit.test_resource_path(), PathBuf::from("/p/fixtures"));
    assert!(explicit.warnings.is_empty(), "{:?}", explicit.warnings);
    let text = explicit.render(None);
    assert!(text.contains("test-resource-dir = \"fixtures\""), "{text}");
    assert!(!derived.render(None).contains("test-resource-dir"));

    let err = parse("[project]\nname='a'\nversion='1'\ntest-resource-dir='/abs'").unwrap_err();
    assert!(err.to_string().contains("relative path"), "{err}");
}

#[test]
fn unknown_keys_warn_rather_than_fail() {
    let m = parse("[project]\nname='a'\nversion='1'\nfuture-key='x'\n[wat]\nk=1\n[java]\nlevel=9")
        .unwrap();
    assert_eq!(m.warnings.len(), 3, "{:?}", m.warnings);
    assert!(m.warnings.iter().any(|w| w.contains("project.future-key")));
    assert!(m.warnings.iter().any(|w| w.contains("`wat`")));
    assert!(m.warnings.iter().any(|w| w.contains("java.level")));
}

#[test]
fn syntax_errors_name_the_line_and_column() {
    let err = parse("[project]\nname = \nversion = '1'").unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("jrs.toml:2:"), "{msg}");
}

#[test]
fn missing_required_keys_name_the_key() {
    let err = parse("[project]\nversion='1'").unwrap_err();
    assert!(err.to_string().contains("`project.name`"));
    let err = parse("[java]\nsource=21").unwrap_err();
    assert!(err.to_string().contains("[project]"));
}

#[test]
fn a_dependency_key_carrying_a_version_is_rejected() {
    let err = parse("[project]\nname='a'\nversion='1'\n[dependencies]\n\"g:a:1.0\" = \"1.0\"")
        .unwrap_err();
    assert!(err.to_string().contains("group:artifact"), "{err}");
}

#[test]
fn escaping_the_project_root_is_rejected() {
    let err = parse("[project]\nname='a'\nversion='1'\ntarget-dir='../elsewhere'").unwrap_err();
    assert!(err.to_string().contains("relative path"), "{err}");
}

#[test]
fn a_name_that_is_not_a_file_name_is_rejected() {
    let err = parse("[project]\nname='a/b'\nversion='1'").unwrap_err();
    assert!(err.to_string().contains("valid file name"), "{err}");
}

#[test]
fn old_style_java_versions_are_accepted() {
    let m = parse("[project]\nname='a'\nversion='1'\n[java]\nsource='1.8'").unwrap();
    assert_eq!(m.java.source, Some(8));
}

#[test]
fn missing_main_class_names_the_command_that_wanted_it() {
    let m = parse("[project]\nname='a'\nversion='1'").unwrap();
    let err = m.require_main_class("run").unwrap_err();
    assert!(err.to_string().contains("`jrs run`"), "{err}");
    assert!(err.to_string().contains("main-class"));
}

#[test]
fn duplicate_declarations_warn() {
    let m = parse(
        "[project]\nname='a'\nversion='1'\n[dependencies]\n'g:a'='1'\n[dev-dependencies]\n'g:a'='2'",
    )
    .unwrap();
    assert!(
        m.warnings.iter().any(|w| w.contains("both")),
        "{:?}",
        m.warnings
    );
}

#[test]
fn retries_and_coverage_minimums_parse_and_render_back() {
    let m = parse(
        "[project]\nname='a'\nversion='1'\n\
         [test]\nretries = 2\ncoverage-minimum = { line = 0.80, branch = 0.755, class = 1 }",
    )
    .unwrap();
    assert!(m.warnings.is_empty(), "{:?}", m.warnings);
    assert_eq!(m.test.retries, 2);
    assert_eq!(
        m.test.coverage_minimum,
        [
            CoverageMinimum {
                counter: CoverageCounter::Line,
                basis_points: 8000
            },
            CoverageMinimum {
                counter: CoverageCounter::Branch,
                basis_points: 7550
            },
            CoverageMinimum {
                counter: CoverageCounter::Class,
                basis_points: 10_000
            },
        ],
        "in declaration order"
    );
    let rendered = m.render(None);
    assert!(rendered.contains("retries = 2\n"), "{rendered}");
    assert!(
        rendered.contains("coverage-minimum = { line = 0.8, branch = 0.755, class = 1.0 }\n"),
        "{rendered}"
    );
    assert_eq!(parse(&rendered).unwrap().test, m.test);

    let plain = parse("[project]\nname='a'\nversion='1'\n[test]\n").unwrap();
    assert_eq!(plain.test.retries, 0);
    assert!(plain.test.coverage_minimum.is_empty());
}

#[test]
fn share_classes_is_off_unless_asked_for() {
    let m = parse("[project]\nname='a'\nversion='1'\n[test]\nshare-classes = true\n").unwrap();
    assert!(m.warnings.is_empty(), "{:?}", m.warnings);
    assert_eq!(m.test.share_classes, ShareClasses::On);
    let rendered = m.render(None);
    assert!(rendered.contains("share-classes = true\n"), "{rendered}");
    assert_eq!(parse(&rendered).unwrap().test, m.test);

    let archive =
        parse("[project]\nname='a'\nversion='1'\n[test]\nshare-classes = 'archive'\n").unwrap();
    assert_eq!(archive.test.share_classes, ShareClasses::Archive);
    let rendered = archive.render(None);
    assert!(
        rendered.contains("share-classes = \"archive\"\n"),
        "{rendered}"
    );
    assert_eq!(parse(&rendered).unwrap().test, archive.test);

    let aot = parse("[project]\nname='a'\nversion='1'\n[test]\nshare-classes = 'aot'\n").unwrap();
    assert_eq!(aot.test.share_classes, ShareClasses::Aot);
    let rendered = aot.render(None);
    assert!(rendered.contains("share-classes = \"aot\"\n"), "{rendered}");
    assert_eq!(parse(&rendered).unwrap().test, aot.test);

    let plain = parse("[project]\nname='a'\nversion='1'\n[test]\n").unwrap();
    assert_eq!(plain.test.share_classes, ShareClasses::Off);
    let err = parse("[project]\nname='a'\nversion='1'\n[test]\nshare-classes = 'yes'\n")
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("`test.share-classes` must be `true`, `false`, `\"archive\"` or `\"aot\"`"),
        "{err}"
    );
}

#[test]
fn coverage_minimums_are_ratios_checked_per_key() {
    let error = |body: &str| {
        parse(&format!("[project]\nname='a'\nversion='1'\n[test]\n{body}"))
            .unwrap_err()
            .to_string()
    };
    let err = error("coverage-minimum = { line = 80 }");
    assert!(err.contains("`test.coverage-minimum.line` is 80"), "{err}");
    assert!(err.contains("0.8 for 80%"), "{err}");
    let err = error("coverage-minimum = { branch = 1.5 }");
    assert!(
        err.contains("`test.coverage-minimum.branch` is 1.5"),
        "{err}"
    );
    let err = error("coverage-minimum = { branch = -0.1 }");
    assert!(err.contains("`test.coverage-minimum.branch`"), "{err}");
    let err = error("coverage-minimum = { line = '80%' }");
    assert!(err.contains("must be a number from 0 to 1"), "{err}");
    let err = error("coverage-minimum = 0.8");
    assert!(err.contains("must be a table of ratios"), "{err}");
    let err = error("retries = -1");
    assert!(
        err.contains("`test.retries` must be a whole number"),
        "{err}"
    );
    let err = error("retries = 'twice'");
    assert!(err.contains("`test.retries`"), "{err}");

    // A counter jrs does not know is a warning, as any unknown key is.
    let m = parse(
        "[project]\nname='a'\nversion='1'\n[test]\ncoverage-minimum = { lines = 0.8, line = 0.5 }",
    )
    .unwrap();
    assert_eq!(m.test.coverage_minimum.len(), 1);
    assert!(
        m.warnings
            .iter()
            .any(|w| w.contains("`test.coverage-minimum.lines`")),
        "{:?}",
        m.warnings
    );
}

#[test]
fn rendering_round_trips() {
    let original = parse(FULL).unwrap();
    let text = original.render(Some("generated by jrs"));
    assert!(text.starts_with("# generated by jrs\n"));
    let again = Manifest::parse(&text, Path::new("/p/jrs.toml"), Path::new("/p")).unwrap();
    assert_eq!(again.name, original.name);
    assert_eq!(again.version, original.version);
    assert_eq!(again.main_class, original.main_class);
    assert_eq!(again.java, original.java);
    assert_eq!(again.dependencies, original.dependencies);
    assert_eq!(again.dev_dependencies, original.dev_dependencies);
    assert_eq!(again.repositories, original.repositories);
}

/// A manifest with nothing but `[project]` and `extra`.
fn with(extra: &str) -> Result<Manifest> {
    parse(&format!("[project]\nname='a'\nversion='1'\n{extra}"))
}

const TASKS: &str = r#"
[tasks.build-info]
description = "Generate BuildInfo.java"
script = "build/GenerateBuildInfo.java"
args = ["{target}/generated/sources", "{project.version}"]
inputs = ["build/GenerateBuildInfo.java", ".git/HEAD"]
outputs = ["{target}/generated/sources"]
source-outputs = ["{target}/generated/sources"]

[tasks.checksum]
shell = "shasum -a 256 \"$JRS_JAR\" > \"$JRS_JAR.sha256\""

[tasks.format]
run = ["google-java-format", "--replace"]
env = { MODE = "ci" }
cwd = "src"

[tasks.release]
depends-on = ["package", "checksum"]

[hooks]
pre-compile = ["build-info"]
post-package = ["checksum"]
"#;

#[test]
fn the_task_example_from_the_proposal_parses() {
    let m = with(TASKS).unwrap();
    assert!(m.warnings.is_empty(), "{:?}", m.warnings);
    let names: Vec<&str> = m.tasks.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["build-info", "checksum", "format", "release"]);
    assert!(matches!(m.tasks[0].action, Some(Action::Script(_))));
    assert!(matches!(m.tasks[1].action, Some(Action::Shell(_))));
    assert!(matches!(&m.tasks[2].action, Some(Action::Run(argv)) if argv.len() == 2));
    assert_eq!(m.tasks[2].env[0].0, "MODE");
    assert!(m.tasks[3].action.is_none(), "release only aggregates");
    assert_eq!(
        m.tasks[3].depends_on,
        [
            TaskRef::Builtin(Builtin::Package),
            TaskRef::Task("checksum".into())
        ]
    );
    assert_eq!(m.hooks.tasks(Hook::PreCompile), ["build-info"]);
    assert_eq!(m.hooks.tasks(Hook::PostPackage), ["checksum"]);
    assert!(m.hooks.tasks(Hook::PreRun).is_empty());
    assert_eq!(m.task("format").unwrap().name, "format");
}

#[test]
fn a_task_runs_exactly_one_action_or_aggregates() {
    let err = with("[tasks.t]\nrun = ['x']\nshell = 'y'\n").unwrap_err();
    assert!(err.to_string().contains("`run` and `shell`"), "{err}");
    let err = with("[tasks.t]\ndescription = 'nothing'\n").unwrap_err();
    assert!(err.to_string().contains("`tasks.t` does nothing"), "{err}");
    let err = with("[tasks.t]\nrun = []\n").unwrap_err();
    assert!(
        err.to_string()
            .contains("`tasks.t.run` must name a program"),
        "{err}"
    );
    let err = with("[tasks.t]\nrun = 'x y'\n").unwrap_err();
    assert!(
        err.to_string()
            .contains("`tasks.t.run` must be an array of strings"),
        "{err}"
    );
    let err = with("[tasks.t]\nshell = ['x']\n").unwrap_err();
    assert!(err.to_string().contains("`tasks.t.shell`"), "{err}");
}

#[test]
fn task_names_are_lowercase_and_never_a_command() {
    for name in ["Build", "1st", "under_score"] {
        let err = with(&format!("[tasks.{name}]\nshell = 'x'\n")).unwrap_err();
        assert!(
            err.to_string().contains("lowercase letters"),
            "{name}: {err}"
        );
    }
    for name in ["build", "test", "task", "clean", "help"] {
        let err = with(&format!("[tasks.{name}]\nshell = 'x'\n")).unwrap_err();
        assert!(
            err.to_string().contains("is a jrs command"),
            "{name}: {err}"
        );
    }
}

#[test]
fn depends_on_takes_tasks_and_four_built_ins() {
    let m = with("[tasks.t]\ndepends-on = ['build', 'test', 'package', 'doc']\n").unwrap();
    assert_eq!(m.tasks[0].depends_on.len(), 4);
    let err = with("[tasks.t]\ndepends-on = ['run']\n").unwrap_err();
    assert!(err.to_string().contains("cannot depend on"), "{err}");
    let err = with("[tasks.t]\ndepends-on = ['build', 'build']\n").unwrap_err();
    assert!(err.to_string().contains("names `build` twice"), "{err}");
}

#[test]
fn unknown_task_and_hook_keys_warn() {
    let m = with("[tasks.t]\nshell = 'x'\nretries = 3\n[hooks]\npre-deploy = ['t']\n").unwrap();
    assert_eq!(m.warnings.len(), 2, "{:?}", m.warnings);
    assert!(m.warnings.iter().any(|w| w.contains("`tasks.t.retries`")));
    assert!(m.warnings.iter().any(|w| w.contains("`hooks.pre-deploy`")));
}

#[test]
fn jrs_environment_names_belong_to_jrs() {
    for key in ["JRS_TASK", "jrs_mine"] {
        let err = with(&format!(
            "[tasks.t]\nshell = 'x'\nenv = {{ {key} = 'v' }}\n"
        ))
        .unwrap_err();
        assert!(
            err.to_string().contains(&format!("`tasks.t.env.{key}`")),
            "{err}"
        );
    }
    let err = with("[tasks.t]\nshell = 'x'\nenv = { A = 1 }\n").unwrap_err();
    assert!(err.to_string().contains("must be a string"), "{err}");
}

#[test]
fn a_free_port_is_one_port_per_name_and_only_where_a_process_reads_it() {
    let m = with(
        "[test]\nenv = { PORT = '{free-port.web}', URL = 'http://localhost:{free-port.web}/x', \
         API = '{free-port.api}' }\n",
    )
    .unwrap();
    let value = |key: &str| {
        let (_, t) = m.test.env.iter().find(|(k, _)| k == key).unwrap();
        t.expand(|_| unreachable!()).unwrap()
    };
    let port = value("PORT");
    assert!(port.parse::<u16>().is_ok_and(|p| p > 0), "{port}");
    assert_eq!(value("URL"), format!("http://localhost:{port}/x"));
    assert_ne!(value("API"), port, "two names, two ports");

    let err = with("[tasks.t]\nshell = 'x'\noutputs = ['{free-port.a}']\n").unwrap_err();
    assert!(err.to_string().contains("is a port, not a path"), "{err}");
    let err = with("[run]\ncwd = '{free-port.a}'\n").unwrap_err();
    assert!(err.to_string().contains("not a directory"), "{err}");
    let err = with("[test]\nenv = { P = '{free-port.}' }\n").unwrap_err();
    assert!(err.to_string().contains("needs a name"), "{err}");
}

#[test]
fn kotlin_compiler_plugins_are_named_and_checked() {
    let m = with("[kotlin]\nversion = '2.4.20'\nplugins = ['serialization', 'spring']\n").unwrap();
    assert_eq!(m.languages[0].plugins, ["serialization", "spring"]);
    assert!(
        m.render(None)
            .contains("plugins = [\"serialization\", \"spring\"]")
    );
    let err = with("[kotlin]\nversion = '2.4.20'\nplugins = ['kapt']\n").unwrap_err();
    assert!(
        err.to_string().contains("not a compiler plugin jrs knows"),
        "{err}"
    );
    let scala = with("[scala]\nversion = '3.9.0'\nplugins = ['serialization']\n").unwrap();
    assert!(
        scala.warnings.iter().any(|w| w.contains("plugins")),
        "{:?}",
        scala.warnings
    );
}

#[test]
fn placeholders_are_checked_when_the_manifest_loads() {
    let err = with("[tasks.t]\nshell = 'x'\nargs = ['{tagret}/out']\n").unwrap_err();
    assert!(
        err.to_string()
            .contains("`tasks.t.args`: unknown placeholder `{tagret}`"),
        "{err}"
    );
    let err = with("[tasks.t]\nrun = ['x', '{root']\n").unwrap_err();
    assert!(err.to_string().contains("unclosed"), "{err}");
    let err = with("[tasks.t]\nrun = ['x', 'a}b']\n").unwrap_err();
    assert!(err.to_string().contains("stray"), "{err}");
    let err = with("[tasks.t]\nshell = 'x'\ninputs = ['{classpath}']\n").unwrap_err();
    assert!(
        err.to_string().contains("is a classpath, not a path"),
        "{err}"
    );
    // A shell string is the shell's to expand: braces there are literal.
    with("[tasks.t]\nshell = 'echo ${HOME} {not-a-placeholder}'\n").unwrap();
}

#[test]
fn a_template_splits_into_text_and_placeholders() {
    let t = Template::parse("{root}/a-{{b}}-{jar}").unwrap();
    assert_eq!(
        t.segments,
        [
            Segment::Placeholder(Placeholder::Root),
            Segment::Text("/a-{b}-".into()),
            Segment::Placeholder(Placeholder::Jar),
        ]
    );
    assert_eq!(t.raw, "{root}/a-{{b}}-{jar}");
    let plain = Template::parse("no braces").unwrap();
    assert!(!plain.has_free_port());
    assert_eq!(plain.segments, [Segment::Text("no braces".into())]);
}

#[test]
fn rendering_keeps_tasks_and_hooks() {
    let text = r#"
[project]
name = "a"
version = "1"

[tasks.gen]
description = "Generate"
run = ["java", "@{classpath-argfile}", "Gen", "{{literal}}"]
env = { "MODE" = "fast", "OUT" = "{target}/gen" }
cwd = "tools"
inputs = ["src/gen"]
outputs = ["target/gen"]
source-outputs = ["target/gen"]

[tasks.check-all]
depends-on = ["test", "gen"]

[tasks.sh]
shell = "echo \"$JRS_JAR\""
depends-on = ["build"]

[hooks]
pre-compile = ["gen"]
post-package = ["sh"]
"#;
    let manifest = parse(text).unwrap();
    let again = parse(&manifest.render(None)).unwrap();
    assert_eq!(again.tasks, manifest.tasks);
    assert_eq!(again.hooks, manifest.hooks);
    assert!(again.warnings.is_empty(), "{:?}", again.warnings);
}

#[test]
fn rendering_omits_defaults() {
    let m = blank("app", "0.1.0", Path::new("/p"));
    let text = m.render(None);
    assert!(!text.contains("source-dir"));
    assert!(!text.contains("[java]"));
    assert!(!text.contains("[repositories]"));
    assert!(text.contains("name = \"app\""));
}

#[test]
fn each_language_table_parses_and_round_trips() {
    let m = with(
        "[kotlin]\nversion = '2.4.20'\nkotlinc-args = ['-Xjsr305=strict']\n\
         compiler-jvm-args = ['-Xmx2g']\n\
         [scala]\nversion = '3.9.0'\nsource-dir = 'src/main/sc'\nscalac-args = ['-deprecation']\n\
         [groovy]\nversion = '5.1.2'\ntest-dir = 'spec'\ngroovyc-args = ['--compile-static']\n",
    )
    .unwrap();
    assert!(m.warnings.is_empty(), "{:?}", m.warnings);
    let keys: Vec<&str> = m.languages.iter().map(|c| c.language.key()).collect();
    assert_eq!(keys, ["kotlin", "scala", "groovy"]);
    let kotlin = m.language(Language::Kotlin).unwrap();
    assert_eq!(kotlin.version, "2.4.20");
    assert_eq!(kotlin.source_dir, PathBuf::from("src/main/kotlin"));
    assert_eq!(kotlin.test_dir, PathBuf::from("src/test/kotlin"));
    assert_eq!(kotlin.compiler_args, ["-Xjsr305=strict"]);
    assert_eq!(kotlin.compiler_jvm_args, ["-Xmx2g"]);
    let scala = m.language(Language::Scala).unwrap();
    assert_eq!(scala.source_dir, PathBuf::from("src/main/sc"));
    assert_eq!(scala.compiler_args, ["-deprecation"]);
    let groovy = m.language(Language::Groovy).unwrap();
    assert_eq!(groovy.test_dir, PathBuf::from("spec"));

    let text = m.render(None);
    assert!(text.contains("[kotlin]\nversion = \"2.4.20\"\n"), "{text}");
    assert!(
        !text.contains("src/main/kotlin"),
        "defaults stay out:\n{text}"
    );
    let again = parse(&text).unwrap();
    assert_eq!(again.languages, m.languages);
    assert!(!with("").unwrap().render(None).contains("[kotlin]"));
}

#[test]
fn a_language_table_needs_a_version_it_can_pin() {
    let err = with("[kotlin]\n").unwrap_err().to_string();
    assert!(err.contains("`kotlin.version`"), "{err}");
    assert!(err.contains("version = \""), "{err}");
    let err = with("[scala]\nversion = 3\n").unwrap_err().to_string();
    assert!(
        err.contains("`scala.version` must be a version string"),
        "{err}"
    );
    let err = with("[groovy]\nversion = '[4.0,5.0)'\n")
        .unwrap_err()
        .to_string();
    assert!(err.contains("range"), "{err}");
    let err = with("[kotlin]\nversion = '1.9.24'\n")
        .unwrap_err()
        .to_string();
    assert!(err.contains("Kotlin 2.0 or newer"), "{err}");
    let err = with("[scala]\nversion = '2.12.18'\n")
        .unwrap_err()
        .to_string();
    assert!(err.contains("2.12 is not supported"), "{err}");
    let err = with("[groovy]\nversion = '3.0.22'\n")
        .unwrap_err()
        .to_string();
    assert!(err.contains("Groovy 4.0 or newer"), "{err}");
    let err = with("[kotlin]\nversion = '2.4.20'\nsource-dir = '../elsewhere'\n")
        .unwrap_err()
        .to_string();
    assert!(err.contains("`kotlin.source-dir`"), "{err}");
}

#[test]
fn unknown_language_keys_warn() {
    let m = with("[kotlin]\nversion = '2.4.20'\nscalac-args = ['-x']\n").unwrap();
    assert_eq!(m.warnings.len(), 1, "{:?}", m.warnings);
    assert!(
        m.warnings[0].contains("`kotlin.scalac-args`"),
        "{:?}",
        m.warnings
    );
}

#[test]
fn the_runtime_library_is_implied_unless_declared() {
    let m = with("[kotlin]\nversion = '2.4.20'\n[dependencies]\n'g:a' = '1'\n").unwrap();
    let effective: Vec<String> = m
        .effective_dependencies()
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(
        effective,
        ["g:a:1", "org.jetbrains.kotlin:kotlin-stdlib:2.4.20"],
        "the implied library comes last"
    );
    assert_eq!(m.implied_dependencies()[0].1, Language::Kotlin);
    assert!(
        !m.render(None).contains("kotlin-stdlib"),
        "an implied library is never written into jrs.toml"
    );

    // A declaration in either table replaces it, and a Java manifest
    // implies nothing.
    let dev = with(
        "[groovy]\nversion = '5.1.2'\n[dev-dependencies]\n'org.apache.groovy:groovy' = '5.1.2'\n",
    )
    .unwrap();
    assert!(dev.implied_dependencies().is_empty());
    assert!(dev.effective_dependencies().is_empty());
    let main =
        with("[kotlin]\nversion = '2.4.20'\n[dependencies]\n'org.jetbrains.kotlin:kotlin-stdlib' = '2.4.20'\n")
            .unwrap();
    assert!(main.implied_dependencies().is_empty());
    assert_eq!(main.effective_dependencies(), main.dependencies);
    assert!(with("").unwrap().implied_dependencies().is_empty());

    // Scala 3.8 split its library in two, and both are implied.
    let scala = with("[scala]\nversion = '3.9.0'\n").unwrap();
    let implied: Vec<String> = scala
        .implied_dependencies()
        .iter()
        .map(|(d, _)| d.to_string())
        .collect();
    assert_eq!(
        implied,
        [
            "org.scala-lang:scala3-library_3:3.9.0",
            "org.scala-lang:scala-library:3.9.0"
        ]
    );
}

#[test]
fn a_runtime_library_declared_at_another_version_warns() {
    let m = with(
        "[kotlin]\nversion = '2.4.20'\n[dependencies]\n'org.jetbrains.kotlin:kotlin-stdlib' = '2.5.0'\n",
    )
    .unwrap();
    assert_eq!(m.warnings.len(), 1, "{:?}", m.warnings);
    assert!(
        m.warnings[0].contains("declared at 2.5.0"),
        "{:?}",
        m.warnings
    );
}

#[test]
fn kotlin_reads_utf8_only() {
    let m = with("[java]\nencoding = 'ISO-8859-1'\n[kotlin]\nversion = '2.4.20'\n").unwrap();
    assert!(
        m.warnings
            .iter()
            .any(|w| w.contains("kotlinc reads sources as UTF-8")),
        "{:?}",
        m.warnings
    );
    let m = with("[java]\nencoding = 'utf8'\n[kotlin]\nversion = '2.4.20'\n").unwrap();
    assert!(m.warnings.is_empty(), "{:?}", m.warnings);
}

#[test]
fn javac_args_groovy_cannot_hand_on_are_a_manifest_error() {
    let err = with("[java]\njavac-args = ['-Xlint:all', 'oops']\n[groovy]\nversion = '5.1.2'\n")
        .unwrap_err();
    assert_eq!(err.exit_code(), 2);
    assert!(err.to_string().contains("`oops`"), "{err}");
    // Without [groovy], javac-args are javac's own business.
    with("[java]\njavac-args = ['-Xlint:all', 'oops']\n").unwrap();
}

// ---- project.jrs-version ----------------------------------------------

#[test]
fn a_jrs_version_this_jrs_satisfies_is_kept_and_rendered() {
    let m = parse("[project]\nname='a'\nversion='1'\njrs-version='0.1'").unwrap();
    assert_eq!(m.jrs_version.as_deref(), Some("0.1"));
    assert!(m.warnings.is_empty(), "{:?}", m.warnings);
    let text = m.render(None);
    assert!(text.contains("jrs-version = \"0.1\"\n"), "{text}");
    let again = Manifest::parse(&text, Path::new("/p/jrs.toml"), Path::new("/p")).unwrap();
    assert_eq!(again.jrs_version.as_deref(), Some("0.1"));
    // `init` and `migrate` start from a blank manifest, which pins nothing.
    assert!(
        !blank("a", "1", Path::new("/p"))
            .render(None)
            .contains("jrs-version")
    );
}

#[test]
fn a_newer_jrs_version_stops_before_any_other_key_is_read() {
    // A manifest for a future jrs: an unknown key, and a dependency form
    // this jrs cannot read. The version is the only thing it says.
    let err = parse(
        "[project]\nname='a'\nversion='1'\njrs-version='999.0'\nfuture-key=1\n\
         [dependencies]\n'g:a' = { path = 'libs/a.jar' }\n",
    )
    .unwrap_err();
    assert_eq!(err.exit_code(), 2);
    let msg = err.to_string();
    assert!(msg.contains("needs jrs 999.0 or newer"), "{msg}");
    assert!(
        msg.contains(&format!("this is jrs {}", env!("CARGO_PKG_VERSION"))),
        "{msg}"
    );
    assert!(msg.contains(RELEASES_URL), "{msg}");
    assert!(msg.contains("/p/jrs.toml"), "{msg}");
}

#[test]
fn jrs_versions_compare_numerically() {
    let path = Path::new("jrs.toml");
    for (required, running) in [
        ("0.9", "0.9.0"),
        ("0.9.0", "0.9.0"),
        ("0.9", "0.10.0"),
        ("0.9.1", "0.9.2"),
        ("1.0", "1.0.0-dev"),
        ("0.4", "1.0.0"),
    ] {
        check_jrs_version(required, running, path)
            .unwrap_or_else(|e| panic!("{required} vs {running}: {e}"));
    }
    for (required, running) in [("0.10", "0.9.9"), ("0.9.1", "0.9.0"), ("1.0", "0.99.99")] {
        assert!(
            check_jrs_version(required, running, path).is_err(),
            "{required} vs {running}"
        );
    }
}

#[test]
fn a_malformed_jrs_version_names_the_key() {
    for value in [
        "'1'",
        "'0.9.1.2'",
        "'0.x'",
        "'v0.9'",
        "'0.9-rc1'",
        "''",
        "0.9",
        "[1]",
    ] {
        let err = parse(&format!(
            "[project]\nname='a'\nversion='1'\njrs-version={value}"
        ))
        .unwrap_err();
        assert_eq!(err.exit_code(), 2, "{value}");
        assert!(
            err.to_string().contains("`project.jrs-version`"),
            "{value}: {err}"
        );
    }
}

const JVM_HEAD: &str = "[project]\nname='a'\nversion='1'\n";

#[test]
fn agents_environment_and_working_directory_are_read_and_rendered_back() {
    let m = parse(&format!(
        "{JVM_HEAD}[run]\njava-agents = ['io.opentelemetry:otel-agent']\n\
         env = {{ APP_MODE = 'dev', OUT = '{{target}}/out' }}\ncwd = '{{target}}/work'\n\
         [test]\njava-agents = ['org.mockito:mockito-core', 'net.bytebuddy:byte-buddy-agent']\n\
         env = {{ CP = '{{test-classpath}}' }}\n"
    ))
    .unwrap();
    assert!(m.warnings.is_empty(), "{:?}", m.warnings);
    assert_eq!(
        m.run.java_agents,
        vec![JavaAgent::Graph(Ga::new("io.opentelemetry", "otel-agent"))]
    );
    let names: Vec<&str> = m.run.env.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(names, ["APP_MODE", "OUT"], "declaration order is kept");
    assert_eq!(m.run.cwd.as_ref().unwrap().raw, "{target}/work");
    assert_eq!(m.test.java_agents.len(), 2);
    assert_eq!(
        m.test.java_agents[1].to_string(),
        "net.bytebuddy:byte-buddy-agent"
    );
    assert!(
        m.test.env[0]
            .1
            .placeholders()
            .any(|p| p == Placeholder::TestClasspath),
        "a classpath is a value an environment variable can hold"
    );

    let text = m.render(None);
    assert!(
        text.contains("java-agents = [\"io.opentelemetry:otel-agent\"]"),
        "{text}"
    );
    assert!(
        text.contains("env = { \"APP_MODE\" = \"dev\", \"OUT\" = \"{target}/out\" }"),
        "{text}"
    );
    let again = parse(&text).unwrap();
    assert_eq!(again.run, m.run);
    assert_eq!(again.test, m.test);

    // A [run] with only a working directory still renders.
    let cwd_only = parse(&format!("{JVM_HEAD}[run]\ncwd = 'work'\n")).unwrap();
    assert!(cwd_only.render(None).contains("[run]\ncwd = \"work\"\n"));
}

#[test]
fn an_agent_with_a_version_is_pinned_apart_from_the_graph() {
    let m = parse(&format!(
        "{JVM_HEAD}[run]\njava-agents = ['io.otel:agent:2.10.0', 'org.mockito:mockito-core']\n\
         [test]\njava-agents = ['org.jacoco:org.jacoco.agent:0.8.15:runtime']\n"
    ))
    .unwrap();
    assert_eq!(
        m.run.java_agents[0],
        JavaAgent::Pinned(Coord::new("io.otel", "agent", "2.10.0"))
    );
    assert!(matches!(m.run.java_agents[1], JavaAgent::Graph(_)));
    assert_eq!(
        m.pinned_agents(),
        vec![
            (
                "run.java-agents.io.otel:agent".to_string(),
                Coord::new("io.otel", "agent", "2.10.0")
            ),
            (
                "test.java-agents.org.jacoco:org.jacoco.agent:runtime".to_string(),
                Coord::new("org.jacoco", "org.jacoco.agent", "0.8.15")
                    .with_classifier(Some("runtime".into()))
            ),
        ],
        "run's first; a graph agent has no pin of its own"
    );

    let text = m.render(None);
    assert!(
        text.contains("java-agents = [\"io.otel:agent:2.10.0\", \"org.mockito:mockito-core\"]"),
        "{text}"
    );
    assert!(
        text.contains("java-agents = [\"org.jacoco:org.jacoco.agent:0.8.15:runtime\"]"),
        "{text}"
    );
    let again = parse(&text).unwrap();
    assert_eq!(again.run, m.run);
    assert_eq!(again.test, m.test);

    let err = parse(&format!("{JVM_HEAD}[run]\njava-agents = ['a:b:[1,2)']\n")).unwrap_err();
    assert!(err.to_string().contains("the range `[1,2)`"), "{err}");
    // With a version or without, it is the one agent.
    let err = parse(&format!(
        "{JVM_HEAD}[run]\njava-agents = ['a:b', 'a:b:1']\n"
    ))
    .unwrap_err();
    assert!(err.to_string().contains("names `a:b` twice"), "{err}");
}

#[test]
fn agents_are_named_by_group_and_artifact_once() {
    for bad in ["mockito-core", "a:b:1:c:d", "a::1", "org.mockito:", " a:b"] {
        let err = parse(&format!("{JVM_HEAD}[test]\njava-agents = ['{bad}']\n")).unwrap_err();
        assert_eq!(err.exit_code(), 2);
        let msg = err.to_string();
        assert!(msg.contains("`test.java-agents`"), "{bad}: {msg}");
        assert!(
            msg.contains("not a `group:artifact` coordinate"),
            "{bad}: {msg}"
        );
    }
    let err = parse(&format!("{JVM_HEAD}[run]\njava-agents = ['a:b', 'a:b']\n")).unwrap_err();
    assert!(err.to_string().contains("names `a:b` twice"), "{err}");
    let err = parse(&format!("{JVM_HEAD}[run]\njava-agents = 'a:b'\n")).unwrap_err();
    assert!(err.to_string().contains("run.java-agents"), "{err}");
}

#[test]
fn the_jvm_environment_is_checked_like_a_tasks() {
    let cases = [
        (
            "[run]\nenv = { JRS_X = '1' }",
            "`run.env.JRS_X`",
            "jrs's own",
        ),
        (
            "[test]\nenv = { A = 1 }",
            "`test.env.A`",
            "must be a string",
        ),
        (
            "[test]\nenv = { A = '{nope}' }",
            "`test.env.A`",
            "unknown placeholder",
        ),
        ("[run]\nenv = { J = '{jar}' }", "`run.env.J`", "no jar"),
        (
            "[test]\nenv = { CP = '@{classpath-argfile}' }",
            "`test.env.CP`",
            "a task's own",
        ),
        ("[run]\ncwd = '{classpath}'", "`run.cwd`", "not a path"),
        ("[run]\ncwd = '{jar}'", "`run.cwd`", "no jar"),
        ("[run]\ncwd = 3", "`run.cwd`", "must be a string"),
    ];
    for (table, key, why) in cases {
        let err = parse(&format!("{JVM_HEAD}{table}\n")).unwrap_err();
        assert_eq!(err.exit_code(), 2, "{table}");
        let msg = err.to_string();
        assert!(msg.contains(key) && msg.contains(why), "{table}: {msg}");
    }
    // `JRS_` stays reserved in a task, with the task's own explanation.
    let err = parse(&format!(
        "{JVM_HEAD}[tasks.t]\nshell = 'x'\nenv = {{ JRS_A = '1' }}\n"
    ))
    .unwrap_err();
    assert!(err.to_string().contains("set for every task"), "{err}");
    // `test.cwd` is not a key: it warns, as every unknown key does.
    let m = parse(&format!("{JVM_HEAD}[test]\ncwd = 'x'\n")).unwrap();
    assert!(
        m.warnings.iter().any(|w| w.contains("test.cwd")),
        "{:?}",
        m.warnings
    );
}
