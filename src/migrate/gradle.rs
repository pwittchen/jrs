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
use crate::manifest::{self, Dependency, Exclusion, Manifest};

const PREAMBLE: &str = "Gradle migration is approximate. jrs reads the declarative \
                        parts of a build script by pattern, not by running Gradle, \
                        so review the manifest below before relying on it.";

/// Configurations that land on the main classpath.
const MAIN_CONFIGS: &[&str] = &["implementation", "api", "runtimeOnly", "compile", "runtime"];

/// Compiled against but not shipped: `compile-only`.
const COMPILE_ONLY_CONFIGS: &[&str] = &["compileOnly", "compileOnlyApi", "providedCompile"];

/// Annotation processors. jrs has no processor path (SPEC §1.2), but `javac`
/// runs a processor it finds on the compile classpath, so these become
/// compile-only dependencies.
const PROCESSOR_CONFIGS: &[&str] = &["annotationProcessor"];

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
    "testAnnotationProcessor",
    "kapt",
    "developmentOnly",
    "providedRuntime",
];

/// Translate `build_file`, with `settings.gradle` and a version catalog from
/// `root` when they are there.
///
/// # Errors
///
/// [`JrsError::Io`](crate::error::JrsError::Io) if `build_file` cannot be read.
/// Everything jrs cannot read in it goes into the report, not an error.
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

    let version = if let Some(v) = assignment(&script, "version") {
        report.migrated(format!("project.version = {v}"));
        v
    } else {
        report.review("project.version — no `version = ...` found; defaulted to 0.1.0".to_string());
        "0.1.0".to_string()
    };

    let mut out = manifest::blank(&name, &version, root);
    report.migrated(format!("project.name = {name}"));

    read_java(&script, &mut out, &mut report);
    read_main_class(&script, &mut out, &mut report);
    read_dependencies(&script, &catalog, &mut out, &mut report);
    read_jvm_args(&script, &mut out, &mut report);
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

        let (group, artifact) = if let Some(module) = entry.get("module").and_then(|v| v.as_str()) {
            match module.split_once(':') {
                Some((g, a)) => (g.to_string(), a.to_string()),
                None => continue,
            }
        } else {
            let group = entry.get("group").and_then(|v| v.as_str());
            let name = entry.get("name").and_then(|v| v.as_str());
            match (group, name) {
                (Some(g), Some(a)) => (g.to_string(), a.to_string()),
                _ => continue,
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
        entries.insert(alias.clone(), Dependency::new(group, artifact, version));
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
                .take_while(char::is_ascii_digit)
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
    // The declaration whose `{ exclude ... }` closure is still open, and how
    // deep inside it the scan is.
    let mut closure: Option<(Target, usize, usize)> = None;
    let mut processors = Vec::new();

    for line in block_lines(script, "dependencies") {
        let trimmed = line.trim();
        let (opens, closes) = (trimmed.matches('{').count(), trimmed.matches('}').count());
        if let Some((target, index, depth)) = &mut closure {
            if let Some(exclusion) = parse_exclude(trimmed) {
                table(out, *target)[*index].exclusions.push(exclusion);
            }
            *depth = (*depth + opens).saturating_sub(closes);
            if *depth == 0 {
                closure = None;
            }
            continue;
        }

        let Some(config) = leading_word(trimmed) else {
            continue;
        };
        let target = if MAIN_CONFIGS.contains(&config.as_str()) {
            Target::Main
        } else if COMPILE_ONLY_CONFIGS.contains(&config.as_str()) {
            Target::CompileOnly
        } else if PROCESSOR_CONFIGS.contains(&config.as_str()) {
            Target::Processor
        } else if TEST_CONFIGS.contains(&config.as_str()) {
            Target::Test
        } else if REPORTED_CONFIGS.contains(&config.as_str()) {
            report.skipped(format!(
                "`{trimmed}` — the `{config}` configuration has no equivalent in jrs.toml"
            ));
            continue;
        } else {
            continue;
        };

        // Everything up to a trailing closure is the declaration itself.
        let (declaration, inline_closure) = match trimmed.find('{') {
            Some(at) => (&trimmed[..at], Some(&trimmed[at..])),
            None => (trimmed, None),
        };
        let Some(mut dep) = read_declaration(declaration, &config, catalog, report) else {
            continue;
        };
        if let Some(body) = inline_closure {
            dep.exclusions.extend(parse_exclude(body));
        }
        if target == Target::Processor {
            processors.push(dep.key());
        }
        let index = push(out, target, dep, report, config.as_str());
        if opens > closes {
            closure = Some((target, index, opens - closes));
        }
    }

    if !processors.is_empty() {
        let has_flag = out.java.javac_args.iter().any(|a| a.starts_with("-proc:"));
        let added = out.java.source.is_some_and(|s| s >= 21) && !has_flag;
        if added {
            out.java.javac_args.push("-proc:full".to_string());
        }
        report.review(format!(
            "annotation processors {} — put on the compile classpath as compile-only \
             dependencies; javac from JDK 23 on runs them only with `-proc:full` in \
             java.javac-args{}",
            processors.join(", "),
            if added {
                ", which was added"
            } else {
                ", which JDK 17 does not accept, so it was not added"
            }
        ));
    }
}

/// One dependency declaration, without its closure: a catalog reference, a
/// `g:a:v` literal, or the map notation.
fn read_declaration(
    text: &str,
    config: &str,
    catalog: &Catalog,
    report: &mut Report,
) -> Option<Dependency> {
    // `libs.foo.bar`, resolved through the version catalog.
    if let Some(reference) = catalog_reference(text) {
        let found = catalog.get(&reference).cloned();
        if found.is_none() {
            report.skipped(format!(
                "`{config} libs.{reference}` — no such alias in the version \
                 catalog; add it to jrs.toml by hand"
            ));
        }
        return found;
    }

    // `implementation 'g:a:v'` / `implementation("g:a:v")`
    let literals = quoted(text);
    if let Some(dep) = literals.first().and_then(|s| parse_gav(s)) {
        return Some(dep);
    }

    // `implementation group: 'g', name: 'a', version: 'v'`
    if let Some(dep) = parse_map_notation(text) {
        return Some(dep);
    }

    let trimmed = text.trim();
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
    None
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Target {
    Main,
    CompileOnly,
    Processor,
    Test,
}

fn table(out: &mut Manifest, target: Target) -> &mut Vec<Dependency> {
    match target {
        Target::Test => &mut out.dev_dependencies,
        _ => &mut out.dependencies,
    }
}

/// Add a dependency, returning its index in its table.
///
/// `compileOnly` and `annotationProcessor` naming the same library is the usual
/// Lombok setup, and becomes one compile-only entry; a plain `implementation`
/// of it wins over both.
fn push(
    out: &mut Manifest,
    target: Target,
    mut dep: Dependency,
    report: &mut Report,
    config: &str,
) -> usize {
    dep.compile_only = matches!(target, Target::CompileOnly | Target::Processor);
    let entries = table(out, target);
    if let Some(index) = entries.iter().position(|d| d.key() == dep.key()) {
        if target == Target::Main {
            entries[index].compile_only = false;
        }
        report.migrated(format!(
            "{} ({config}, merged with an earlier declaration)",
            dep.key()
        ));
        return index;
    }
    report.migrated(format!("{} ({config})", dep.key()));
    entries.push(dep);
    entries.len() - 1
}

/// `exclude group: 'x', module: 'y'`, `exclude(group = "x")`, or
/// `transitive = false`, which excludes everything.
fn parse_exclude(text: &str) -> Option<Exclusion> {
    let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    if compact.contains("transitive=false") || compact.contains("isTransitive=false") {
        return Some(Exclusion {
            group: "*".into(),
            artifact: "*".into(),
        });
    }
    let at = text.find("exclude")?;
    let rest = &text[at + "exclude".len()..];
    let field = |key: &str| -> Option<String> {
        let at = rest.find(key)?;
        let after = rest[at + key.len()..].trim_start();
        after
            .starts_with([':', '='])
            .then(|| quoted(after).into_iter().next())
            .flatten()
    };
    let group = field("group");
    let module = field("module");
    if group.is_none() && module.is_none() {
        return None;
    }
    Some(Exclusion {
        group: group.unwrap_or_else(|| "*".into()),
        artifact: module.unwrap_or_else(|| "*".into()),
    })
}

/// `applicationDefaultJvmArgs` for `jrs run`; the test task's `jvmArgs` and
/// `systemProperty` for `jrs test`.
fn read_jvm_args(script: &str, out: &mut Manifest, report: &mut Report) {
    if let Some(line) = script
        .lines()
        .find(|l| l.trim().starts_with("applicationDefaultJvmArgs"))
    {
        let args = quoted(line);
        if !args.is_empty() {
            report.migrated(format!("run.jvm-args = {args:?}"));
            out.run.jvm_args = args;
        }
    }

    let mut test_args = Vec::new();
    for line in blocks_where(script, is_test_block) {
        let trimmed = line.trim();
        if trimmed.starts_with("jvmArgs") {
            test_args.extend(quoted(trimmed));
        } else if trimmed.starts_with("systemProperty") && !trimmed.starts_with("systemProperties")
        {
            if let [key, value] = quoted(trimmed).as_slice() {
                test_args.push(format!("-D{key}={value}"));
            }
        } else if trimmed.contains("useTestNG") {
            report.skipped(
                "`useTestNG()` — jrs runs JUnit 5, and JUnit 4 through the Vintage \
                 engine; TestNG is not supported"
                    .to_string(),
            );
        }
    }
    if !test_args.is_empty() {
        report.migrated(format!("test.jvm-args = {test_args:?}"));
        out.test.jvm_args = test_args;
    }
}

/// `test { }`, and the `tasks.test` / `tasks.withType(Test)` spellings of it.
fn is_test_block(header: &str) -> bool {
    let compact: String = header.chars().filter(|c| !c.is_whitespace()).collect();
    compact.starts_with("test{")
        || compact.starts_with("tasks.test")
        || compact.starts_with("tasks.withType(Test")
        || compact.starts_with("tasks.withType<Test>")
        || compact.starts_with("tasks.named<Test>")
        || compact.starts_with("tasks.named('test'")
        || compact.starts_with("tasks.named(\"test\"")
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
#[must_use]
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
#[must_use]
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
#[must_use]
pub fn block_lines<'a>(script: &'a str, name: &str) -> Vec<&'a str> {
    blocks_where(script, |header| header.starts_with(name))
}

/// The lines inside every block whose opening line satisfies `header`.
fn blocks_where(script: &str, header: impl Fn(&str) -> bool) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut inside = false;

    for line in script.lines() {
        let opens = line.matches('{').count();
        let closes = line.matches('}').count();
        if !inside {
            if header(line.trim()) && opens > 0 {
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
#[must_use]
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

/// `g:a:v`, `g:a:v:classifier`, or `g:a:v@jar`.
fn parse_gav(text: &str) -> Option<Dependency> {
    // `"com.example:thing:$version"` is a Groovy template, not a coordinate.
    // Accepting it would put a literal `$version` in the manifest, which is a
    // worse outcome than saying jrs could not read the line.
    if text.contains('$') {
        return None;
    }
    let (text, extension) = match text.split_once('@') {
        Some((coordinate, ext)) => (coordinate, Some(ext)),
        None => (text, None),
    };
    if extension.is_some_and(|e| e != "jar") {
        return None;
    }
    let parts: Vec<&str> = text.split(':').map(str::trim).collect();
    if !(3..=4).contains(&parts.len()) || parts.iter().any(|p| p.is_empty()) {
        return None;
    }
    let mut dep = Dependency::new(parts[0], parts[1], parts[2]);
    dep.classifier = parts.get(3).map(ToString::to_string);
    Some(dep)
}

/// `implementation group: 'g', name: 'a', version: 'v'` (and `classifier:`).
fn parse_map_notation(line: &str) -> Option<Dependency> {
    let field = |key: &str| -> Option<String> {
        let at = line.find(&format!("{key}:"))?;
        quoted(&line[at..]).into_iter().next()
    };
    let mut dep = Dependency::new(field("group")?, field("name")?, field("version")?);
    dep.classifier = field("classifier");
    Some(dep)
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
#[must_use]
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
    fn unknown_plugins_are_reported() {
        let dir = Dir::new("unsupported");
        let skipped = dir.migrate(GROOVY).report.not_migrated.join("\n");
        assert!(skipped.contains("shadow"), "{skipped}");
        assert!(!skipped.contains("id 'java'"), "{skipped}");
    }

    #[test]
    fn compile_only_and_annotation_processors_become_one_compile_only_entry() {
        let dir = Dir::new("processors");
        let migration = dir.migrate(GROOVY);
        let lombok = migration
            .manifest
            .dependencies
            .iter()
            .find(|d| d.artifact == "lombok")
            .unwrap();
        assert!(lombok.compile_only);
        assert_eq!(
            migration
                .manifest
                .dependencies
                .iter()
                .filter(|d| d.artifact == "lombok")
                .count(),
            1
        );
        assert_eq!(migration.manifest.java.javac_args, vec!["-proc:full"]);
        let review = migration.report.needs_review.join("\n");
        assert!(review.contains("annotation processors"), "{review}");
    }

    #[test]
    fn classifiers_and_exclusions_are_read() {
        let dir = Dir::new("extras");
        let m = dir
            .migrate(
                "dependencies {\n\
                 \x20 implementation 'org.lwjgl:lwjgl:3.3.3:natives-linux'\n\
                 \x20 implementation group: 'io.netty', name: 'netty-transport-native-epoll', \
                 version: '4.1.100.Final', classifier: 'linux-x86_64'\n\
                 \x20 implementation('com.google.guava:guava:33.0.0-jre') {\n\
                 \x20   exclude group: 'com.google.code.findbugs', module: 'jsr305'\n\
                 \x20   exclude module: 'checker-qual'\n\
                 \x20 }\n\
                 \x20 implementation('g:solo:1.0') { transitive = false }\n\
                 \x20 implementation 'g:after:1.0'\n\
                 }\n",
            )
            .manifest;
        let keys: Vec<String> = m.dependencies.iter().map(|d| d.key()).collect();
        assert_eq!(
            keys,
            vec![
                "org.lwjgl:lwjgl:natives-linux",
                "io.netty:netty-transport-native-epoll:linux-x86_64",
                "com.google.guava:guava",
                "g:solo",
                "g:after",
            ]
        );
        let exclusions: Vec<String> = m.dependencies[2]
            .exclusions
            .iter()
            .map(|e| e.to_string())
            .collect();
        assert_eq!(
            exclusions,
            vec!["com.google.code.findbugs:jsr305", "*:checker-qual"]
        );
        assert_eq!(m.dependencies[3].exclusions[0].to_string(), "*:*");
        assert!(m.dependencies[4].exclusions.is_empty());
    }

    #[test]
    fn jvm_arguments_are_read_for_run_and_test() {
        let dir = Dir::new("jvm-args");
        let migration = dir.migrate(
            "application {\n  mainClass = 'x.Y'\n  applicationDefaultJvmArgs = ['-Xmx1g', '-Dmode=prod']\n}\n\
             tasks.test {\n  useJUnitPlatform()\n  jvmArgs '-Xmx256m'\n  \
             systemProperty 'env', 'test'\n}\n",
        );
        let m = &migration.manifest;
        assert_eq!(m.run.jvm_args, vec!["-Xmx1g", "-Dmode=prod"]);
        assert_eq!(m.test.jvm_args, vec!["-Xmx256m", "-Denv=test"]);

        let testng = dir.migrate("test {\n  useTestNG()\n}\n");
        assert!(
            testng
                .report
                .not_migrated
                .iter()
                .any(|s| s.contains("TestNG"))
        );
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
        assert_eq!(
            parse_gav("com.example:thing:1.0:tests")
                .unwrap()
                .classifier
                .as_deref(),
            Some("tests")
        );
        assert!(parse_gav("com.example:thing:1.0@jar").is_some());
        assert!(parse_gav("com.example:thing:1.0@aar").is_none());
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
