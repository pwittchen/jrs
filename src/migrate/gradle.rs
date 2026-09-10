//! `build.gradle` / `build.gradle.kts` → `jrs.toml`.
//!
//! Gradle build scripts are programs, so parsing one exactly would mean running
//! Gradle. jrs does not. What happens instead is line-oriented pattern extraction
//! over the conventional declarative subset, and everything jrs cannot read
//! confidently is reported rather than guessed at (SPEC §11.3).
//!
//! Version catalogs are the exception to "programs are unreadable": a
//! `libs.versions.toml` is declarative, so `libs.foo.bar` references are resolved
//! through it.

use std::collections::BTreeMap;
use std::path::Path;

use super::{Migration, Report, Source};
use crate::error::{IoResultExt, Result};
use crate::manifest::{self, Dependency, Manifest};

const PREAMBLE: &str = "Gradle migration is approximate. jrs reads the declarative \
                        parts of a build script by pattern, not by running Gradle, \
                        so review the manifest below before relying on it.";

/// Configurations that land on the main classpath.
const MAIN_CONFIGS: &[&str] = &[
    "implementation",
    "api",
    "compileOnly",
    "runtimeOnly",
    "compile",
    "runtime",
];

/// Configurations that land on the test classpath.
const TEST_CONFIGS: &[&str] = &[
    "testImplementation",
    "testApi",
    "testCompileOnly",
    "testRuntimeOnly",
    "testCompile",
    "testRuntime",
];

/// Configurations jrs knows about but has nowhere to put.
const REPORTED_CONFIGS: &[&str] = &[
    "annotationProcessor",
    "testAnnotationProcessor",
    "kapt",
    "developmentOnly",
    "providedRuntime",
    "providedCompile",
];

pub fn migrate(build_file: &Path, root: &Path) -> Result<Migration> {
    let mut report = Report {
        preamble: Some(PREAMBLE.to_string()),
        ..Report::default()
    };

    let script = strip_comments(&std::fs::read_to_string(build_file).path(build_file)?);
    let settings = read_settings(root, &mut report);
    let catalog = read_catalog(root, &mut report);

    let name = settings
        .root_project_name
        .clone()
        .or_else(|| {
            root.canonicalize()
                .ok()?
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "app".to_string());

    let version = match assignment(&script, "version") {
        Some(v) => {
            report.migrated(format!("project.version = {v}"));
            v
        }
        None => {
            report.review(
                "project.version — no `version = ...` found; defaulted to 0.1.0".to_string(),
            );
            "0.1.0".to_string()
        }
    };

    let mut out = manifest::blank(&name, &version, root);
    report.migrated(format!("project.name = {name}"));

    read_java(&script, &mut out, &mut report);
    read_main_class(&script, &mut out, &mut report);
    read_dependencies(&script, &catalog, &mut out, &mut report);
    read_repositories(&script, &mut out, &mut report);
    report_the_unreadable(&script, &settings, &mut report);

    Ok(Migration {
        source: Source::Gradle,
        source_file: build_file.to_path_buf(),
        manifest: out,
        report,
    })
}

// ---- settings.gradle -------------------------------------------------------

#[derive(Debug, Default)]
struct Settings {
    root_project_name: Option<String>,
    includes: Vec<String>,
}

fn read_settings(root: &Path, report: &mut Report) -> Settings {
    let mut settings = Settings::default();
    for name in ["settings.gradle", "settings.gradle.kts"] {
        let path = root.join(name);
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let text = strip_comments(&text);
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("rootProject.name") {
                if let Some(value) = quoted(trimmed).first() {
                    settings.root_project_name = Some(value.clone());
                    report.migrated(format!("rootProject.name = {value} (from {name})"));
                }
            } else if trimmed.starts_with("include") {
                settings.includes.extend(quoted(trimmed));
            }
        }
        break;
    }
    settings
}

// ---- version catalog -------------------------------------------------------

/// `alias` → `group:artifact:version`, read from `gradle/libs.versions.toml`.
#[derive(Debug, Default)]
struct Catalog {
    entries: BTreeMap<String, Dependency>,
}

impl Catalog {
    /// Gradle's accessors replace `-`, `_` and `.` with `.`, so `libs.commons.lang3`
    /// and the alias `commons-lang3` are the same thing.
    fn get(&self, reference: &str) -> Option<&Dependency> {
        let normalised = reference.replace(['-', '_', '.'], ".");
        self.entries.iter().find_map(|(alias, dep)| {
            (alias.replace(['-', '_', '.'], ".") == normalised).then_some(dep)
        })
    }
}

fn read_catalog(root: &Path, report: &mut Report) -> Catalog {
    let path = root.join("gradle").join("libs.versions.toml");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Catalog::default();
    };
    let Ok(table) = toml::from_str::<toml::Table>(&text) else {
        report.skipped("gradle/libs.versions.toml — could not be parsed".to_string());
        return Catalog::default();
    };

    let versions: BTreeMap<String, String> = table
        .get("versions")
        .and_then(|v| v.as_table())
        .map(|t| {
            t.iter()
                .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
                .collect()
        })
        .unwrap_or_default();

    let mut entries = BTreeMap::new();
    let libraries = table.get("libraries").and_then(|v| v.as_table());
    for (alias, value) in libraries.into_iter().flatten() {
        let Some(entry) = value.as_table() else {
            // The compact `alias = "g:a:v"` form.
            if let Some(gav) = value.as_str()
                && let Some(dep) = parse_gav(gav)
            {
                entries.insert(alias.clone(), dep);
            }
            continue;
        };

        let (group, artifact) = match entry.get("module").and_then(|v| v.as_str()) {
            Some(module) => match module.split_once(':') {
                Some((g, a)) => (g.to_string(), a.to_string()),
                None => continue,
            },
            None => {
                let group = entry.get("group").and_then(|v| v.as_str());
                let name = entry.get("name").and_then(|v| v.as_str());
                match (group, name) {
                    (Some(g), Some(a)) => (g.to_string(), a.to_string()),
                    _ => continue,
                }
            }
        };

        let version = match entry.get("version") {
            Some(toml::Value::String(v)) => Some(v.clone()),
            Some(toml::Value::Table(t)) => t
                .get("ref")
                .and_then(|v| v.as_str())
                .and_then(|r| versions.get(r).cloned())
                .or_else(|| {
                    t.get("require")
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                }),
            _ => entry
                .get("version.ref")
                .and_then(|v| v.as_str())
                .and_then(|r| versions.get(r).cloned()),
        };

        let Some(version) = version else {
            report.skipped(format!(
                "catalog alias `{alias}` — its version could not be resolved"
            ));
            continue;
        };
        entries.insert(
            alias.clone(),
            Dependency {
                group,
                artifact,
                version,
            },
        );
    }

    if !entries.is_empty() {
        report.migrated(format!(
            "gradle/libs.versions.toml — {} catalog entries read",
            entries.len()
        ));
    }
    Catalog { entries }
}

// ---- the build script ------------------------------------------------------

fn read_java(script: &str, out: &mut Manifest, report: &mut Report) {
    let toolchain = script
        .lines()
        .find(|l| l.contains("languageVersion") && l.contains("JavaLanguageVersion.of"))
        .and_then(|l| l.split("JavaLanguageVersion.of").nth(1))
        .and_then(|rest| {
            let digits: String = rest
                .trim_start_matches(['(', ' '])
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            digits.parse::<u32>().ok()
        });

    let source = toolchain
        .or_else(|| assignment(script, "sourceCompatibility").and_then(|v| java_version(&v)));
    if let Some(source) = source {
        out.java.source = Some(source);
        report.migrated(format!("java.source = {source}"));
    }

    if let Some(target) = assignment(script, "targetCompatibility").and_then(|v| java_version(&v))
        && Some(target) != out.java.source
    {
        out.java.target = Some(target);
        report.migrated(format!("java.target = {target}"));
    }
}

fn read_main_class(script: &str, out: &mut Manifest, report: &mut Report) {
    for line in script.lines() {
        let trimmed = line.trim();
        let is_main = trimmed.starts_with("mainClass")
            || trimmed.starts_with("mainClassName")
            || trimmed.starts_with("getMainClass()");
        if !is_main {
            continue;
        }
        if let Some(value) = quoted(trimmed).first() {
            out.main_class = Some(value.clone());
            report.migrated(format!("project.main-class = {value}"));
            return;
        }
    }
}

fn read_dependencies(script: &str, catalog: &Catalog, out: &mut Manifest, report: &mut Report) {
    for line in block_lines(script, "dependencies") {
        let trimmed = line.trim();
        let Some(config) = leading_word(trimmed) else {
            continue;
        };

        let target = if MAIN_CONFIGS.contains(&config.as_str()) {
            Target::Main
        } else if TEST_CONFIGS.contains(&config.as_str()) {
            Target::Test
        } else if REPORTED_CONFIGS.contains(&config.as_str()) {
            report.skipped(format!(
                "`{}` — the `{config}` configuration has no equivalent in jrs.toml",
                trimmed
            ));
            continue;
        } else {
            continue;
        };

        // `libs.foo.bar`, resolved through the version catalog.
        if let Some(reference) = catalog_reference(trimmed) {
            match catalog.get(&reference) {
                Some(dep) => {
                    push(out, target, dep.clone(), report, config.as_str());
                }
                None => report.skipped(format!(
                    "`{config} libs.{reference}` — no such alias in the version \
                     catalog; add it to jrs.toml by hand"
                )),
            }
            continue;
        }

        // `implementation 'g:a:v'` / `implementation("g:a:v")`
        let literals = quoted(trimmed);
        if let Some(dep) = literals.first().and_then(|s| parse_gav(s)) {
            push(out, target, dep, report, config.as_str());
            continue;
        }

        // `implementation group: 'g', name: 'a', version: 'v'`
        if let Some(dep) = parse_map_notation(trimmed) {
            push(out, target, dep, report, config.as_str());
            continue;
        }

        if literals.len() == 1 {
            let literal = &literals[0];
            let why = if literal.contains('$') {
                "interpolated from a variable, which jrs cannot evaluate \
                 without running Gradle"
            } else {
                "not a group:artifact:version coordinate"
            };
            report.skipped(format!("`{trimmed}` — `{literal}` is {why}"));
        } else {
            report.skipped(format!(
                "`{trimmed}` — built from a variable or an expression, which jrs \
                 cannot evaluate without running Gradle"
            ));
        }
    }
}

#[derive(Clone, Copy)]
enum Target {
    Main,
    Test,
}

fn push(out: &mut Manifest, target: Target, dep: Dependency, report: &mut Report, config: &str) {
    report.migrated(format!("{} ({config})", dep.key()));
    match target {
        Target::Main => out.dependencies.push(dep),
        Target::Test => out.dev_dependencies.push(dep),
    }
}

fn read_repositories(script: &str, out: &mut Manifest, report: &mut Report) {
    let mut repos = Vec::new();
    for line in block_lines(script, "repositories") {
        let trimmed = line.trim();
        if !trimmed.contains("url") {
            continue;
        }
        let Some(url) = quoted(trimmed).into_iter().next() else {
            report.skipped(format!(
                "`{trimmed}` — repository URL built from an expression"
            ));
            continue;
        };
        let url = url.trim_end_matches('/').to_string();
        if url == manifest::CENTRAL_URL {
            continue;
        }
        report.migrated(format!("repository {url}"));
        repos.push(manifest::Repository {
            name: repository_name(&url),
            url,
        });
    }
    repos.push(manifest::Repository {
        name: manifest::CENTRAL_NAME.into(),
        url: manifest::CENTRAL_URL.into(),
    });
    out.repositories = repos;
}

fn report_the_unreadable(script: &str, settings: &Settings, report: &mut Report) {
    if !settings.includes.is_empty() {
        report.skipped(format!(
            "settings.gradle includes {} — jrs builds one module per manifest; \
             migrate each with `jrs migrate --path <module>`",
            settings.includes.join(", ")
        ));
    }
    for (needle, what) in [
        (
            "subprojects",
            "`subprojects { }` — per-project configuration",
        ),
        (
            "allprojects",
            "`allprojects { }` — per-project configuration",
        ),
        ("ext {", "`ext { }` — script variables jrs cannot evaluate"),
        ("tasks.register", "custom tasks — jrs has no task graph"),
        ("task ", "custom tasks — jrs has no task graph"),
        (
            "sourceSets",
            "`sourceSets { }` — use project.source-dir instead",
        ),
    ] {
        if script.lines().any(|l| l.trim().starts_with(needle)) {
            report.skipped(what.to_string());
        }
    }
    for line in block_lines(script, "plugins") {
        let trimmed = line.trim();
        if let Some(id) = quoted(trimmed).first()
            && id != "java"
            && id != "java-library"
            && id != "application"
        {
            report.skipped(format!("plugin `{id}` — jrs has no plugin system"));
        }
    }
}

// ---- text helpers ----------------------------------------------------------

/// Remove `//` and `/* */` comments, leaving line structure intact.
pub fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_block = false;
    let mut in_string: Option<char> = None;

    while let Some(c) = chars.next() {
        if in_block {
            if c == '*' && chars.peek() == Some(&'/') {
                chars.next();
                in_block = false;
            } else if c == '\n' {
                out.push('\n');
            }
            continue;
        }
        if let Some(quote) = in_string {
            out.push(c);
            if c == '\\' {
                if let Some(next) = chars.next() {
                    out.push(next);
                }
            } else if c == quote {
                in_string = None;
            }
            continue;
        }
        match c {
            '\'' | '"' => {
                in_string = Some(c);
                out.push(c);
            }
            '/' if chars.peek() == Some(&'/') => {
                for skipped in chars.by_ref() {
                    if skipped == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                in_block = true;
            }
            _ => out.push(c),
        }
    }
    out
}

/// Every single- or double-quoted literal on a line.
pub fn quoted(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\'' && c != '"' {
            continue;
        }
        let mut value = String::new();
        for inner in chars.by_ref() {
            if inner == c {
                break;
            }
            value.push(inner);
        }
        out.push(value);
    }
    out
}

/// The lines inside a top-level `name { ... }` block, brace-balanced.
pub fn block_lines<'a>(script: &'a str, name: &str) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut inside = false;

    for line in script.lines() {
        let opens = line.matches('{').count();
        let closes = line.matches('}').count();
        if !inside {
            if line.trim().starts_with(name) && opens > 0 {
                depth = opens.saturating_sub(closes);
                inside = depth > 0;
            }
            continue;
        }
        let next = (depth + opens).saturating_sub(closes);
        if next == 0 {
            // The line that closes the block is punctuation, not content.
            inside = false;
            continue;
        }
        out.push(line);
        depth = next;
    }
    out
}

/// The value of a `name = <literal>` assignment anywhere in the script.
pub fn assignment(script: &str, name: &str) -> Option<String> {
    script.lines().find_map(|line| {
        let trimmed = line.trim();
        let rest = trimmed.strip_prefix(name)?;
        let rest = rest.trim_start();
        let rest = rest.strip_prefix('=')?.trim();
        quoted(rest)
            .into_iter()
            .next()
            .or_else(|| Some(rest.trim_end_matches(['(', ')', ';']).to_string()))
            .filter(|v| !v.is_empty())
    })
}

fn leading_word(line: &str) -> Option<String> {
    let word: String = line
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    (!word.is_empty()).then_some(word)
}

/// `implementation(libs.commons.lang3)` → `commons.lang3`
fn catalog_reference(line: &str) -> Option<String> {
    let start = line.find("libs.")?;
    let rest = &line[start + "libs.".len()..];
    let reference: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '.' || *c == '_' || *c == '-')
        .collect();
    (!reference.is_empty()).then_some(reference.trim_end_matches('.').to_string())
}

fn parse_gav(text: &str) -> Option<Dependency> {
    let parts: Vec<&str> = text.split(':').collect();
    if parts.len() != 3 || parts.iter().any(|p| p.trim().is_empty()) {
        return None;
    }
    // `"com.example:thing:$version"` is a Groovy template, not a coordinate.
    // Accepting it would put a literal `$version` in the manifest, which is a
    // worse outcome than saying jrs could not read the line.
    if text.contains('$') {
        return None;
    }
    Some(Dependency {
        group: parts[0].trim().to_string(),
        artifact: parts[1].trim().to_string(),
        version: parts[2].trim().to_string(),
    })
}

/// `implementation group: 'g', name: 'a', version: 'v'`
fn parse_map_notation(line: &str) -> Option<Dependency> {
    let field = |key: &str| -> Option<String> {
        let at = line.find(&format!("{key}:"))?;
        quoted(&line[at..]).into_iter().next()
    };
    Some(Dependency {
        group: field("group")?,
        artifact: field("name")?,
        version: field("version")?,
    })
}

/// `JavaVersion.VERSION_21`, `21`, `'1.8'`, `JavaVersion.VERSION_1_8`.
fn java_version(raw: &str) -> Option<u32> {
    let t = raw.trim().trim_matches(['\'', '"']);
    if let Some(rest) = t.strip_prefix("JavaVersion.VERSION_") {
        let rest = rest.strip_prefix("1_").unwrap_or(rest);
        return rest.parse().ok();
    }
    if let Some(rest) = t.strip_prefix("1.") {
        return rest.parse().ok();
    }
    t.parse().ok()
}

/// A readable name for a repository URL, since Gradle rarely gives one.
fn repository_name(url: &str) -> String {
    url.trim_start_matches("https://")
        .trim_start_matches("http://")
        .split(['/', '.'])
        .find(|s| !s.is_empty() && *s != "www")
        .unwrap_or("repository")
        .to_string()
}

/// Kept so `migrate::mod` can name the file it looked for.
pub fn build_file_names() -> [&'static str; 2] {
    ["build.gradle", "build.gradle.kts"]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct Dir {
        path: PathBuf,
    }

    impl Dir {
        fn new(name: &str) -> Dir {
            let path =
                std::env::temp_dir().join(format!("jrs-gradle-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Dir { path }
        }

        fn write(&self, name: &str, contents: &str) -> PathBuf {
            let file = self.path.join(name);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(&file, contents).unwrap();
            file
        }

        fn migrate(&self, script: &str) -> Migration {
            let file = self.write("build.gradle", script);
            super::migrate(&file, &self.path).unwrap()
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    const GROOVY: &str = r#"
plugins {
    id 'java'
    id 'application'
    id 'com.github.johnrengelman.shadow' version '8.1.1'
}

group = 'com.example'
version = '1.0.0'
sourceCompatibility = JavaVersion.VERSION_21

repositories {
    mavenCentral()
    maven { url 'https://nexus.example.com/repository/maven-public/' }
}

dependencies {
    implementation 'com.google.guava:guava:33.0.0-jre'
    api group: 'org.apache.commons', name: 'commons-lang3', version: '3.14.0'
    compileOnly 'org.projectlombok:lombok:1.18.30'
    annotationProcessor 'org.projectlombok:lombok:1.18.30'
    testImplementation 'org.junit.jupiter:junit-jupiter:5.10.2'
    implementation someVariable
}

application {
    mainClass = 'com.example.Main'
}
"#;

    #[test]
    fn the_declarative_subset_is_translated() {
        let dir = Dir::new("groovy");
        let m = dir.migrate(GROOVY).manifest;
        assert_eq!(m.version, "1.0.0");
        assert_eq!(m.java.source, Some(21));
        assert_eq!(m.main_class.as_deref(), Some("com.example.Main"));

        let deps: Vec<String> = m.dependencies.iter().map(|d| d.to_string()).collect();
        assert_eq!(
            deps,
            vec![
                "com.google.guava:guava:33.0.0-jre",
                "org.apache.commons:commons-lang3:3.14.0",
                "org.projectlombok:lombok:1.18.30",
            ]
        );
        assert_eq!(
            m.dev_dependencies[0].to_string(),
            "org.junit.jupiter:junit-jupiter:5.10.2"
        );
    }

    #[test]
    fn the_report_opens_by_saying_it_is_approximate() {
        let dir = Dir::new("preamble");
        let migration = dir.migrate(GROOVY);
        let preamble = migration.report.preamble.unwrap();
        assert!(preamble.contains("approximate"), "{preamble}");
        assert!(preamble.contains("review"), "{preamble}");
    }

    #[test]
    fn dependencies_built_from_variables_are_reported_not_guessed() {
        let dir = Dir::new("variables");
        let skipped = dir.migrate(GROOVY).report.not_migrated.join("\n");
        assert!(skipped.contains("someVariable"), "{skipped}");
        assert!(skipped.contains("without running Gradle"), "{skipped}");
    }

    #[test]
    fn annotation_processors_and_unknown_plugins_are_reported() {
        let dir = Dir::new("unsupported");
        let skipped = dir.migrate(GROOVY).report.not_migrated.join("\n");
        assert!(skipped.contains("annotationProcessor"), "{skipped}");
        assert!(skipped.contains("shadow"), "{skipped}");
        assert!(!skipped.contains("id 'java'"), "{skipped}");
    }

    #[test]
    fn repositories_are_read_with_central_left_implicit() {
        let dir = Dir::new("repos");
        let m = dir.migrate(GROOVY).manifest;
        assert_eq!(m.repositories.len(), 2);
        assert_eq!(
            m.repositories[0].url,
            "https://nexus.example.com/repository/maven-public"
        );
        assert_eq!(m.repositories[0].name, "nexus");
        assert_eq!(m.repositories[1].url, manifest::CENTRAL_URL);
    }

    #[test]
    fn the_kotlin_dsl_reads_the_same() {
        let dir = Dir::new("kts");
        let file = dir.write(
            "build.gradle.kts",
            r#"
version = "2.0.0"
java {
    toolchain {
        languageVersion.set(JavaLanguageVersion.of(17))
    }
}
dependencies {
    implementation("com.google.guava:guava:33.0.0-jre")
    testImplementation("org.junit.jupiter:junit-jupiter:5.10.2")
}
application {
    mainClass.set("com.example.Boot")
}
"#,
        );
        let m = super::migrate(&file, &dir.path).unwrap().manifest;
        assert_eq!(m.version, "2.0.0");
        assert_eq!(m.java.source, Some(17));
        assert_eq!(m.main_class.as_deref(), Some("com.example.Boot"));
        assert_eq!(m.dependencies.len(), 1);
        assert_eq!(m.dev_dependencies.len(), 1);
    }

    #[test]
    fn the_project_name_comes_from_settings_gradle() {
        let dir = Dir::new("settings");
        dir.write("settings.gradle", "rootProject.name = 'my-app'\n");
        assert_eq!(dir.migrate("version = '1.0'").manifest.name, "my-app");
    }

    #[test]
    fn multi_project_includes_are_listed() {
        let dir = Dir::new("includes");
        dir.write(
            "settings.gradle",
            "rootProject.name = 'root'\ninclude 'core', 'web'\n",
        );
        let skipped = dir.migrate("").report.not_migrated.join("\n");
        assert!(skipped.contains("core, web"), "{skipped}");
    }

    #[test]
    fn version_catalog_references_resolve() {
        let dir = Dir::new("catalog");
        dir.write(
            "gradle/libs.versions.toml",
            r#"
[versions]
guava = "33.0.0-jre"

[libraries]
guava = { module = "com.google.guava:guava", version.ref = "guava" }
commons-lang3 = { group = "org.apache.commons", name = "commons-lang3", version = "3.14.0" }
junit = "org.junit.jupiter:junit-jupiter:5.10.2"
"#,
        );
        let migration = dir.migrate(
            "dependencies {\n  implementation(libs.guava)\n  \
             implementation libs.commons.lang3\n  \
             testImplementation(libs.junit)\n  \
             implementation(libs.missing.alias)\n}\n",
        );
        let deps: Vec<String> = migration
            .manifest
            .dependencies
            .iter()
            .map(|d| d.to_string())
            .collect();
        assert_eq!(
            deps,
            vec![
                "com.google.guava:guava:33.0.0-jre",
                "org.apache.commons:commons-lang3:3.14.0",
            ]
        );
        assert_eq!(migration.manifest.dev_dependencies.len(), 1);

        let skipped = migration.report.not_migrated.join("\n");
        assert!(skipped.contains("missing.alias"), "{skipped}");
    }

    #[test]
    fn comments_do_not_become_dependencies() {
        let dir = Dir::new("comments");
        let m = dir
            .migrate(
                "dependencies {\n  // implementation 'commented:out:1.0'\n  \
                 /* implementation 'also:out:1.0' */\n  \
                 implementation 'real:dep:1.0'\n}\n",
            )
            .manifest;
        assert_eq!(m.dependencies.len(), 1);
        assert_eq!(m.dependencies[0].to_string(), "real:dep:1.0");
    }

    #[test]
    fn a_url_inside_a_comment_is_not_a_repository() {
        let dir = Dir::new("comment-url");
        let m = dir
            .migrate("repositories {\n  // maven { url 'https://old.example.com' }\n  mavenCentral()\n}\n")
            .manifest;
        assert_eq!(m.repositories.len(), 1);
        assert_eq!(m.repositories[0].url, manifest::CENTRAL_URL);
    }

    #[test]
    fn a_missing_version_defaults_and_says_so() {
        let dir = Dir::new("no-version");
        let migration = dir.migrate("dependencies {\n}\n");
        assert_eq!(migration.manifest.version, "0.1.0");
        assert!(
            migration
                .report
                .needs_review
                .iter()
                .any(|r| r.contains("0.1.0")),
            "{:?}",
            migration.report.needs_review
        );
    }

    #[test]
    fn an_interpolated_coordinate_is_refused_rather_than_written_out() {
        assert_eq!(parse_gav("com.example:thing:1.0").unwrap().version, "1.0");
        assert!(parse_gav("com.example:thing:$version").is_none());
        assert!(parse_gav("com.example:thing:${version}").is_none());

        let dir = Dir::new("interpolated");
        let migration =
            dir.migrate("def v = '1.0'\ndependencies {\n  implementation \"g:a:$v\"\n}\n");
        assert!(migration.manifest.dependencies.is_empty());
        let skipped = migration.report.not_migrated.join("\n");
        assert!(
            skipped.contains("interpolated from a variable"),
            "{skipped}"
        );
    }

    #[test]
    fn java_versions_parse_in_every_spelling() {
        assert_eq!(java_version("21"), Some(21));
        assert_eq!(java_version("'1.8'"), Some(8));
        assert_eq!(java_version("JavaVersion.VERSION_21"), Some(21));
        assert_eq!(java_version("JavaVersion.VERSION_1_8"), Some(8));
        assert_eq!(java_version("whatever"), None);
    }

    #[test]
    fn blocks_are_brace_balanced() {
        let script = "dependencies {\n  a { nested }\n  b\n}\nafter\n";
        assert_eq!(
            block_lines(script, "dependencies"),
            vec!["  a { nested }", "  b"]
        );
    }

    #[test]
    fn quoted_literals_are_extracted_in_order() {
        assert_eq!(quoted("implementation 'a:b:c'"), vec!["a:b:c"]);
        assert_eq!(
            quoted(r#"group: "g", name: 'n'"#),
            vec!["g".to_string(), "n".to_string()]
        );
        assert!(quoted("no literals here").is_empty());
    }

    #[test]
    fn build_file_names_are_the_ones_detection_looks_for() {
        assert_eq!(build_file_names(), ["build.gradle", "build.gradle.kts"]);
    }
}
