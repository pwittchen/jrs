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
use crate::compile::lang::Language;
use crate::error::{IoResultExt, Result};
use crate::manifest::{self, Dependency, Exclusion, Manifest};
use crate::resolve::coord::Ga;

const PREAMBLE: &str = "Gradle migration is approximate. jrs reads the declarative \
                        parts of a build script by pattern, not by running Gradle, \
                        so review the manifest below before relying on it.";

/// Configurations that land on the main classpath.
const MAIN_CONFIGS: &[&str] = &["implementation", "api", "compile"];

/// Compiled against but not shipped: `compile-only`.
const COMPILE_ONLY_CONFIGS: &[&str] = &["compileOnly", "compileOnlyApi", "providedCompile"];

/// Shipped and run with, but not compiled against: `runtime-only`. The old
/// `runtime` configuration, gone since Gradle 7, meant the same.
const RUNTIME_ONLY_CONFIGS: &[&str] = &["runtimeOnly", "runtime"];

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

    let plugins = read_plugins(&script);
    read_java(&script, &mut out, &mut report);
    // Before the dependencies: `kotlin("reflect")` takes [kotlin]'s version.
    read_kotlin(&script, &plugins, &mut out, &mut report);
    read_main_class(&script, &mut out, &mut report);
    // Before the dependencies: whether anything manages versions decides
    // whether a coordinate without one can be migrated.
    read_managed(&script, &plugins, &catalog, &mut out, &mut report);
    read_dependencies(&script, &catalog, &mut out, &mut report);
    // After them: `groovy` and `scala` take their library's version.
    read_library_languages(&plugins, &mut out, &mut report);
    read_jvm_args(&script, &mut out, &mut report);
    super::gradle_tasks::read_jvm_environment(&script, &mut out, &mut report);
    read_jar_manifest(&script, &mut out, &mut report);
    read_proguard(&plugins, &mut out, &mut report);
    super::gradle_repos::read(&script, &mut out, &mut report);
    super::gradle_tasks::read(&script, &mut out, &mut report);
    report_the_unreadable(&script, &settings, &plugins, &mut report);

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
            // The compact `alias = "g:a:v"` form, or `"g:a"` for a library a
            // platform versions.
            if let Some(gav) = value.as_str()
                && let Some(dep) = parse_gav(gav).or_else(|| parse_ga(gav))
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

        // No version at all is a library a platform versions; it is read as
        // one, and whether anything manages it is decided where it is used.
        if entry.get("version").is_none() && entry.get("version.ref").is_none() {
            entries.insert(alias.clone(), Dependency::new(group, artifact, ""));
            continue;
        }
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
    // A toolchain inside `kotlin { }` pins the JDK, not the release; that one
    // is `read_kotlin`'s.
    let kotlin = kotlin_lines(script);
    let toolchain = script
        .lines()
        .filter(|l| !kotlin.iter().any(|k| std::ptr::eq(k.as_ptr(), l.as_ptr())))
        .find(|l| l.contains("languageVersion") && l.contains("JavaLanguageVersion.of"))
        .and_then(|l| number_after(l, "JavaLanguageVersion.of"));

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
    let kotlin = out.language(Language::Kotlin).map(|c| c.version.clone());
    let managed = !out.managed.is_empty();
    // How deep inside `constraints { }` the scan is; `read_managed` read it.
    let mut constraints = 0usize;

    for line in block_lines(script, "dependencies") {
        let trimmed = line.trim();
        let (opens, closes) = (trimmed.matches('{').count(), trimmed.matches('}').count());
        if constraints > 0 {
            constraints = (constraints + opens).saturating_sub(closes);
            continue;
        }
        if trimmed.starts_with("constraints") && opens > closes {
            constraints = opens - closes;
            continue;
        }
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
        } else if RUNTIME_ONLY_CONFIGS.contains(&config.as_str()) {
            Target::RuntimeOnly
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
        // `platform(...)` is a BOM, which `read_managed` put in [managed].
        if platform_argument(declaration).is_some() {
            continue;
        }
        // `files(...)` and `fileTree(...)`: jars in the project, not coordinates.
        if let Some(jars) = super::gradle_files::read(trimmed, &out.root, report) {
            for dep in jars {
                push_local(out, target, dep, report, config.as_str());
            }
            continue;
        }
        let Some(mut dep) = read_declaration(
            declaration,
            &config,
            kotlin.as_deref(),
            managed,
            catalog,
            report,
        ) else {
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
/// `g:a:v` literal, the map notation, or `kotlin("<module>")` at `kotlin`,
/// the `[kotlin]` version. A coordinate without a version is migrated as one
/// when something `[managed]` holds (`managed`) can version it.
fn read_declaration(
    text: &str,
    config: &str,
    kotlin: Option<&str>,
    managed: bool,
    catalog: &Catalog,
    report: &mut Report,
) -> Option<Dependency> {
    if let Some((module, version)) = kotlin_notation(text) {
        return kotlin_dependency(text.trim(), &module, version.as_deref().or(kotlin), report);
    }
    let trimmed = text.trim();
    let unversioned = |dep: Dependency, report: &mut Report| {
        if managed {
            return Some(dep);
        }
        report.skipped(format!(
            "`{trimmed}` — {} has no version, and nothing in the build manages one",
            dep.key()
        ));
        None
    };

    // `libs.foo.bar`, resolved through the version catalog.
    if let Some(reference) = catalog_reference(text) {
        let found = catalog.get(&reference).cloned();
        return match found {
            Some(dep) if dep.is_managed() => unversioned(dep, report),
            Some(dep) => Some(dep),
            None => {
                report.skipped(format!(
                    "`{config} libs.{reference}` — no such alias in the version \
                     catalog; add it to jrs.toml by hand"
                ));
                None
            }
        };
    }

    // `implementation 'g:a:v'` / `implementation("g:a:v")`, or `'g:a'`
    let literals = quoted(text);
    if let Some(dep) = literals.first().and_then(|s| parse_gav(s)) {
        return Some(dep);
    }
    if let Some(dep) = literals.first().and_then(|s| parse_ga(s)) {
        return unversioned(dep, report);
    }

    // `implementation group: 'g', name: 'a', version: 'v'`
    if let Some(dep) = parse_map_notation(text) {
        return if dep.is_managed() {
            unversioned(dep, report)
        } else {
            Some(dep)
        };
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
    None
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Target {
    Main,
    CompileOnly,
    RuntimeOnly,
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
/// of it wins over both. `compileOnly` and `runtimeOnly` of one library add up
/// to a plain entry.
fn push(
    out: &mut Manifest,
    target: Target,
    mut dep: Dependency,
    report: &mut Report,
    config: &str,
) -> usize {
    dep.compile_only = matches!(target, Target::CompileOnly | Target::Processor);
    dep.runtime_only = target == Target::RuntimeOnly;
    let entries = table(out, target);
    if let Some(index) = entries.iter().position(|d| d.key() == dep.key()) {
        let existing = &mut entries[index];
        let widened = target == Target::Main
            || (existing.compile_only && dep.runtime_only)
            || (existing.runtime_only && dep.compile_only);
        if widened {
            existing.compile_only = false;
            existing.runtime_only = false;
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

/// Add a local jar. Two jars with one file name in different directories get
/// distinct keys, `-2` and on after the first.
fn push_local(
    out: &mut Manifest,
    target: Target,
    mut dep: Dependency,
    report: &mut Report,
    config: &str,
) {
    let base = dep.artifact.clone();
    let mut n = 2;
    while table(out, target)
        .iter()
        .any(|d| d.key() == dep.key() && d.path != dep.path)
    {
        dep.artifact = format!("{base}-{n}");
        n += 1;
    }
    push(out, target, dep, report, config);
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

/// One statement in `constraints { }`. A constraint versions the artifact
/// wherever it turns up, which is what a `[managed]` version does; only a
/// literal `g:a:v` with no closure (`plain`) is one jrs can read.
fn constraint(
    statement: &str,
    plain: bool,
    out: &mut Manifest,
    report: &mut Report,
    pinned: &mut usize,
) {
    if !leading_word(statement).is_some_and(|w| is_configuration(&w)) {
        return;
    }
    match quoted(statement).first().and_then(|s| parse_gav(s)) {
        Some(d) if plain => {
            if super::add_managed(out, &d.group, &d.artifact, &d.version) {
                *pinned += 1;
            }
        }
        _ => report.skipped(format!(
            "`{statement}` — a constraint jrs cannot read: only a literal \
             group:artifact:version is; pin it in [managed] by hand"
        )),
    }
}

/// What follows `platform(` or `enforcedPlatform(` in a declaration: the
/// BOM it names, and whatever comes after.
fn platform_argument(text: &str) -> Option<&str> {
    ["enforcedPlatform(", "platform("]
        .iter()
        .find_map(|call| text.find(call).map(|at| &text[at + call.len()..]))
}

/// Whether `word` is a configuration a dependency is declared in.
fn is_configuration(word: &str) -> bool {
    [
        MAIN_CONFIGS,
        COMPILE_ONLY_CONFIGS,
        RUNTIME_ONLY_CONFIGS,
        PROCESSOR_CONFIGS,
        TEST_CONFIGS,
    ]
    .iter()
    .any(|configs| configs.contains(&word))
}

/// What versions the dependencies the build leaves unversioned, into
/// `[managed]` (SPEC §8.9): Spring Boot's plugins, which import Boot's BOM;
/// the dependency-management plugin's `dependencyManagement { }` block, with
/// its `mavenBom` imports and `dependency` pins; and `platform()` and
/// `constraints { }` in `dependencies { }`.
fn read_managed(
    script: &str,
    plugins: &[Plugin],
    catalog: &Catalog,
    out: &mut Manifest,
    report: &mut Report,
) {
    let boot = plugins.iter().find(|p| p.id == "org.springframework.boot");
    let boot_version = boot.and_then(|p| p.version.clone());
    if let Some(boot) = boot {
        let from = "plugin `org.springframework.boot`";
        let dependency_management = plugins
            .iter()
            .any(|p| p.id == "io.spring.dependency-management");
        match &boot.version {
            // Without the dependency-management plugin, Boot's BOM comes in
            // only through `platform(SpringBootPlugin.BOM_COORDINATES)`,
            // which is read below.
            Some(version) if dependency_management => super::spring_boot_bom(
                out,
                version,
                "plugins `org.springframework.boot` and `io.spring.dependency-management`",
                report,
            ),
            Some(_) => {}
            None => report.skipped(format!(
                "{from} — its version is set somewhere jrs does not read (settings, a \
                 catalog, a variable), so Spring Boot's BOM was not added to [managed]"
            )),
        }
        super::spring_boot_parameters(out, from, report);
        super::spring_boot_application(out, from, report);
    }

    let mut pinned = read_dependency_management(script, out, report);

    let mut constraints = 0usize;
    for line in block_lines(script, "dependencies") {
        let trimmed = line.trim();
        let (opens, closes) = (trimmed.matches('{').count(), trimmed.matches('}').count());
        if constraints > 0 {
            constraints = (constraints + opens).saturating_sub(closes);
            constraint(trimmed, opens == 0, out, report, &mut pinned);
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("constraints")
            && rest.trim_start().starts_with('{')
        {
            // `constraints { implementation 'g:a:v' }` on one line.
            for statement in rest.trim_start()[1..].split(['}', ';']) {
                constraint(statement.trim(), true, out, report, &mut pinned);
            }
            constraints = opens.saturating_sub(closes);
            continue;
        }
        let Some(argument) = platform_argument(trimmed) else {
            continue;
        };
        let bom = if argument
            .trim_start()
            .starts_with("SpringBootPlugin.BOM_COORDINATES")
        {
            boot_version
                .as_ref()
                .map(|v| Dependency::new(super::SPRING_BOOT_GROUP, "spring-boot-dependencies", v))
        } else if let Some(reference) = catalog_reference(argument) {
            catalog.get(&reference).filter(|d| !d.is_managed()).cloned()
        } else {
            quoted(argument).first().and_then(|s| parse_gav(s))
        };
        let Some(bom) = bom else {
            report.skipped(format!(
                "`{trimmed}` — the platform is not a group:artifact:version jrs can read; \
                 add it to [managed] as a BOM by hand"
            ));
            continue;
        };
        super::add_bom(
            out,
            &bom.group,
            &bom.artifact,
            &bom.version,
            &format!("`{trimmed}`"),
            report,
        );
        if trimmed.contains("enforcedPlatform(") {
            report.review(format!(
                "`{trimmed}` — [managed] holds the whole graph to the BOM's versions, as \
                 enforcedPlatform does, but a version written in [dependencies] still wins"
            ));
        }
    }
    if pinned > 0 {
        let s = if pinned == 1 { "" } else { "s" };
        report.migrated(format!(
            "[managed] — {pinned} version{s} from the build's dependency management and \
             constraints"
        ));
    }
}

/// The dependency-management plugin's `dependencyManagement { }` block: its
/// `mavenBom` imports and `dependency` pins. Returns how many versions it
/// pinned.
fn read_dependency_management(script: &str, out: &mut Manifest, report: &mut Report) -> usize {
    let mut pinned = 0;
    for line in block_lines(script, "dependencyManagement") {
        // `imports { mavenBom '...' }` is as often written on one line as on
        // three, so each statement between braces is read on its own.
        for statement in line.split(['{', '}', ';']).map(str::trim) {
            let Some(word) = leading_word(statement) else {
                continue;
            };
            let literal = quoted(statement).first().and_then(|s| parse_gav(s));
            match (word.as_str(), literal) {
                ("mavenBom", Some(bom)) => super::add_bom(
                    out,
                    &bom.group,
                    &bom.artifact,
                    &bom.version,
                    &format!("`{statement}`"),
                    report,
                ),
                ("dependency", Some(d)) => {
                    if super::add_managed(out, &d.group, &d.artifact, &d.version) {
                        pinned += 1;
                    }
                }
                ("mavenBom" | "dependency", None) => report.skipped(format!(
                    "`{statement}` — not a literal group:artifact:version, which jrs can \
                     read without running Gradle; add it to [managed] by hand"
                )),
                ("dependencySet", _) => report.skipped(format!(
                    "`{statement}` — dependency sets are not read; pin each version in \
                     [managed]"
                )),
                _ => {}
            }
        }
    }
    pinned
}

/// `applicationDefaultJvmArgs` for `jrs run`; the test task's `jvmArgs` and
/// `systemProperty` for `jrs test`.
fn read_jvm_args(script: &str, out: &mut Manifest, report: &mut Report) {
    if let Some(line) = script
        .lines()
        .find(|l| l.trim().starts_with("applicationDefaultJvmArgs"))
    {
        let mut args = quoted(line);
        for agent in take_java_agents(&mut args) {
            translate_java_agent("run", &agent, out, report);
        }
        if !args.is_empty() {
            report.migrated(format!("run.jvm-args = {args:?}"));
            out.run.jvm_args = args;
        }
    }

    let mut test_args = Vec::new();
    let mut test_agents = Vec::new();
    for line in blocks_where(script, is_test_block) {
        let trimmed = line.trim();
        if trimmed.starts_with("jvmArgs") {
            let mut args = quoted(trimmed);
            test_agents.extend(take_java_agents(&mut args));
            test_args.extend(args);
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
        } else if let Some(value) = setting(trimmed, "maxParallelForks") {
            read_max_parallel_forks(trimmed, value, out, report);
        } else if setting(trimmed, "forkEvery").is_some() {
            report.skipped(format!(
                "`{trimmed}` — Gradle restarts a test JVM after so many classes; jrs starts \
                 each test JVM once, for its whole share of the classes"
            ));
        }
    }
    if !test_args.is_empty() {
        report.migrated(format!("test.jvm-args = {test_args:?}"));
        out.test.jvm_args = test_args;
    }
    for agent in test_agents {
        translate_java_agent("test", &agent, out, report);
    }
}

/// The value of `key = value` or `key value` on one line of a Gradle block, in
/// either DSL; `None` when the line sets something else.
fn setting<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let rest = line.strip_prefix(key)?;
    let value = match rest.trim_start().strip_prefix('=') {
        Some(value) => value,
        None if rest.starts_with(char::is_whitespace) => rest,
        None => return None,
    };
    Some(value.trim().trim_end_matches(';').trim_end())
}

/// `maxParallelForks` → `test.forks`. Builds usually compute it from the
/// machine's cores, which a committed jrs.toml cannot, so that is reported.
fn read_max_parallel_forks(line: &str, value: &str, out: &mut Manifest, report: &mut Report) {
    match value.parse::<u32>() {
        // Gradle's default, and jrs's.
        Ok(0 | 1) => {}
        Ok(n) => {
            out.test.forks = n;
            report.migrated(format!("test.forks = {n} (from maxParallelForks)"));
        }
        Err(_) => report.skipped(format!(
            "`{line}` — its value is computed; set test.forks to a number of test JVMs"
        )),
    }
}

/// The `-javaagent:` arguments out of `args`, in order.
fn take_java_agents(args: &mut Vec<String>) -> Vec<String> {
    let (agents, rest) = std::mem::take(args)
        .into_iter()
        .partition(|a| a.starts_with("-javaagent:"));
    *args = rest;
    agents
}

/// A `-javaagent:` argument names its jar by path, and in a Gradle build that
/// path is into Gradle's own cache, which a committed jrs.toml cannot name.
/// Mockito's documented recipe is the one jrs can translate: its agent is the
/// `mockito-core` jar the project already depends on, so `java-agents` names
/// that. Any other agent is reported.
fn translate_java_agent(section: &str, arg: &str, out: &mut Manifest, report: &mut Report) {
    let is_mockito = |d: &Dependency| d.group == "org.mockito" && d.artifact == "mockito-core";
    let declared = if section == "run" {
        out.dependencies.iter().any(is_mockito)
    } else {
        out.dependencies
            .iter()
            .chain(&out.dev_dependencies)
            .any(is_mockito)
    };
    if !(declared && arg.to_ascii_lowercase().contains("mockito")) {
        report.skipped(format!(
            "`{arg}` in the {section} JVM's arguments — a path jrs cannot carry over; name \
             the agent by `group:artifact` in `{section}.java-agents`, and declare it as a \
             dependency"
        ));
        return;
    }
    let mockito = Ga::new("org.mockito", "mockito-core");
    let agents = if section == "run" {
        &mut out.run.java_agents
    } else {
        &mut out.test.java_agents
    };
    if !agents.contains(&mockito) {
        agents.push(mockito);
    }
    report.migrated(format!(
        "{section}.java-agents = [\"org.mockito:mockito-core\"] (from `{arg}`)"
    ));
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

/// `jar { manifest { attributes(...) } }`, in either DSL, → `[package.manifest]`.
///
/// Literal values are carried over; `version` and `project.version` become
/// `{project.version}`, and `project.name` and `rootProject.name` become
/// `{project.name}`. Any other expression is reported, not guessed at. A
/// `Main-Class` attribute sets `project.main-class` when nothing else did;
/// the other attributes jrs writes itself are reported. `withSourcesJar()`
/// and `withJavadocJar()` have no manifest key — they are `jrs package`
/// flags — so they are reported with the flag to use.
fn read_jar_manifest(script: &str, out: &mut Manifest, report: &mut Report) {
    let mut text = blocks_where(script, is_jar_block).join("\n");
    for line in script.lines() {
        if line.trim().starts_with("jar.manifest") {
            text.push('\n');
            text.push_str(line);
        }
    }
    for (name, expression) in jar_attribute_pairs(&text) {
        let Some(raw) = attribute_value(&expression) else {
            report.skipped(format!(
                "jar manifest attribute `{name}` — its value is computed (`{expression}`)"
            ));
            continue;
        };
        if name.eq_ignore_ascii_case("Main-Class") {
            match &out.main_class {
                None if !raw.contains('{') => {
                    report.migrated(format!(
                        "project.main-class = {raw} (from the jar manifest)"
                    ));
                    out.main_class = Some(raw);
                }
                Some(main) if *main == raw => {}
                _ => report.skipped(format!(
                    "jar manifest attribute `Main-Class: {raw}` — the main class is \
                     `project.main-class`"
                )),
            }
            continue;
        }
        if out
            .package
            .manifest
            .iter()
            .any(|(n, _)| n.eq_ignore_ascii_case(&name))
        {
            report.skipped(format!(
                "jar manifest attribute `{name}` — set more than once; the first is kept"
            ));
            continue;
        }
        match manifest::jar_attribute(&name, &raw) {
            Ok(template) => {
                report.migrated(format!("package.manifest.{name} = {raw}"));
                out.package.manifest.push((name, template));
            }
            Err(e) => report.skipped(format!("jar manifest attribute — {e}")),
        }
    }

    for (call, flag) in [
        ("withSourcesJar", "--sources"),
        ("withJavadocJar", "--javadoc"),
    ] {
        if script.contains(call) {
            report.skipped(format!(
                "`{call}()` — a flag in jrs, not a manifest key: `jrs package {flag}`"
            ));
        }
    }
}

/// `jar { }`, and the `tasks.jar` / `tasks.named('jar')` /
/// `tasks.withType(Jar)` spellings of it.
fn is_jar_block(header: &str) -> bool {
    let compact: String = header.chars().filter(|c| !c.is_whitespace()).collect();
    [
        "jar{",
        "tasks.jar{",
        "tasks.named('jar')",
        "tasks.named(\"jar\")",
        "tasks.named<Jar>(\"jar\")",
        "tasks.getByName<Jar>(\"jar\")",
        "tasks.withType(Jar)",
        "tasks.withType<Jar>",
    ]
    .iter()
    .any(|start| compact.starts_with(start))
}

/// Every `name`/value-expression pair the `attributes` calls in `text` set:
/// `attributes('K': v, ...)` and `attributes 'K': v` in Groovy,
/// `attributes("K" to v, ...)`, `attributes(mapOf(...))` and
/// `attributes["K"] = v` in Kotlin. Quoted text is never mistaken for the call.
fn jar_attribute_pairs(text: &str) -> Vec<(String, String)> {
    const CALL: &str = "attributes";
    let chars: Vec<char> = text.chars().collect();
    let is_ident = |c: char| c.is_alphanumeric() || c == '_';
    let line_end = |from: usize| {
        chars[from..]
            .iter()
            .position(|c| *c == '\n')
            .map_or(chars.len(), |p| from + p)
    };
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\'' || c == '"' {
            i = skip_quoted(&chars, i);
            continue;
        }
        let at_call = chars[i..].starts_with(&CALL.chars().collect::<Vec<_>>())
            && (i == 0 || !is_ident(chars[i - 1]))
            && !chars.get(i + CALL.len()).copied().is_some_and(is_ident);
        if !at_call {
            i += 1;
            continue;
        }
        let mut j = i + CALL.len();
        while chars.get(j).is_some_and(|c| *c == ' ' || *c == '\t') {
            j += 1;
        }
        match chars.get(j) {
            Some('[') => {
                // `attributes["K"] = v`
                let end = line_end(j);
                let segment: String = chars[j + 1..end].iter().collect();
                if let Some((name, rest)) = first_literal(&segment)
                    && let Some(value) = rest
                        .trim_start()
                        .strip_prefix(']')
                        .and_then(|r| r.trim_start().strip_prefix('='))
                {
                    out.push((name, value.trim().to_string()));
                }
                i = end;
            }
            Some('(') => {
                let end = closing_paren(&chars, j);
                let inner: String = chars[j + 1..end].iter().collect();
                attribute_items(&inner, &mut out);
                i = end + 1;
            }
            _ => {
                // Groovy's command syntax runs to the end of the line, and on
                // over any line that ends in a comma.
                let mut end = line_end(j);
                while end < chars.len()
                    && chars[j..end]
                        .iter()
                        .collect::<String>()
                        .trim_end()
                        .ends_with(',')
                {
                    end = line_end(end + 1);
                }
                let segment: String = chars[j..end].iter().collect();
                attribute_items(&segment, &mut out);
                i = end;
            }
        }
    }
    out
}

/// The index just past the string literal that opens at `start`.
fn skip_quoted(chars: &[char], start: usize) -> usize {
    let quote = chars[start];
    let mut i = start + 1;
    while i < chars.len() {
        match chars[i] {
            '\\' => i += 2,
            c if c == quote => return i + 1,
            _ => i += 1,
        }
    }
    chars.len()
}

/// The index of the `)` that closes the `(` at `open`, or the end of `chars`.
fn closing_paren(chars: &[char], open: usize) -> usize {
    let mut depth = 0usize;
    let mut i = open;
    while i < chars.len() {
        match chars[i] {
            '\'' | '"' => {
                i = skip_quoted(chars, i);
                continue;
            }
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return i;
                }
            }
            _ => {}
        }
        i += 1;
    }
    chars.len()
}

/// The `'K': v` / `"K" to v` items of one `attributes` argument list.
fn attribute_items(list: &str, out: &mut Vec<(String, String)>) {
    let list = list.trim();
    let list = list
        .strip_prefix("mapOf(")
        .and_then(|l| l.strip_suffix(')'))
        .unwrap_or(list);
    let chars: Vec<char> = list.chars().collect();
    let mut items = Vec::new();
    let (mut start, mut depth, mut i) = (0, 0usize, 0);
    while i < chars.len() {
        match chars[i] {
            '\'' | '"' => {
                i = skip_quoted(&chars, i);
                continue;
            }
            '(' | '[' => depth += 1,
            ')' | ']' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                items.push(chars[start..i].iter().collect::<String>());
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    items.push(chars[start..].iter().collect::<String>());

    for item in items {
        let item = item.trim();
        if !item.starts_with(['\'', '"']) {
            continue;
        }
        let Some((name, rest)) = first_literal(item) else {
            continue;
        };
        let rest = rest.trim_start();
        let value = rest
            .strip_prefix(':')
            .or_else(|| rest.strip_prefix("to "))
            .map(str::trim);
        if let Some(value) = value.filter(|v| !v.is_empty()) {
            out.push((name, value.to_string()));
        }
    }
}

/// An attribute's value expression as a `[package.manifest]` value, when it is
/// a literal or names the project's version or name.
fn attribute_value(expression: &str) -> Option<String> {
    let expression = expression.trim().trim_end_matches(';').trim();
    match expression {
        "version"
        | "project.version"
        | "project.version.toString()"
        | "\"$version\""
        | "\"${version}\""
        | "\"${project.version}\"" => return Some("{project.version}".to_string()),
        "project.name" | "rootProject.name" | "\"${project.name}\"" | "\"${rootProject.name}\"" => {
            return Some("{project.name}".to_string());
        }
        _ => {}
    }
    let (literal, rest) = first_literal(expression)?;
    let whole = expression.starts_with(['\'', '"']) && rest.trim().is_empty();
    // A `$` in a double-quoted string is interpolation, which jrs cannot run.
    let interpolated = expression.starts_with('"') && literal.contains('$');
    (whole && !interpolated).then(|| manifest::Template::literal(&literal).raw)
}

fn report_the_unreadable(
    script: &str,
    settings: &Settings,
    plugins: &[Plugin],
    report: &mut Report,
) {
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
        (
            "sourceSets",
            "`sourceSets { }` — use project.source-dir instead",
        ),
    ] {
        if script.lines().any(|l| l.trim().starts_with(needle)) {
            report.skipped(what.to_string());
        }
    }
    for plugin in plugins {
        // Language plugins and Kotlin's compiler plugins have been reported,
        // migrated or not, by `read_kotlin` and `read_library_languages`.
        let id = plugin.id.as_str();
        // Spring Boot's two have been reported by `read_managed`.
        let understood = matches!(
            id,
            "java"
                | "java-library"
                | "application"
                | "org.springframework.boot"
                | "io.spring.dependency-management"
                | "com.guardsquare.proguard"
        ) || language_plugin(id).is_some()
            || kotlin_compiler_plugin(id).is_some();
        if !understood {
            report.skipped(format!("plugin `{id}` — jrs has no plugin system"));
        }
    }
}

// ---- JVM languages (JVM_LANGUAGES.md §10) -----------------------------------

/// One entry of the `plugins { }` block.
struct Plugin {
    id: String,
    /// The version written beside the id, when it is a literal.
    version: Option<String>,
}

/// `id 'x' version 'v'`, `id("x") version "v"`, `kotlin("jvm") version "v"`,
/// and the Kotlin DSL's bare `java` or `groovy`.
fn read_plugins(script: &str) -> Vec<Plugin> {
    block_lines(script, "plugins")
        .into_iter()
        .filter_map(read_plugin)
        .collect()
}

fn read_plugin(line: &str) -> Option<Plugin> {
    let trimmed = line.trim();
    // `kotlin("jvm")` is Gradle's shorthand for `id("org.jetbrains.kotlin.jvm")`.
    if let Some(args) = trimmed.strip_prefix("kotlin(") {
        let (name, rest) = first_literal(args)?;
        return Some(Plugin {
            id: format!("org.jetbrains.kotlin.{name}"),
            version: plugin_version(rest),
        });
    }
    let bare = !trimmed.is_empty()
        && trimmed
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | '`'));
    if bare {
        return Some(Plugin {
            id: trimmed.trim_matches('`').to_string(),
            version: None,
        });
    }
    let (id, rest) = first_literal(trimmed)?;
    Some(Plugin {
        id,
        version: plugin_version(rest),
    })
}

/// `version "2.2.0"` or `.version("2.2.0")` after a plugin's id. A version
/// from a variable or a catalog is not one jrs can read.
fn plugin_version(rest: &str) -> Option<String> {
    let rest = rest.trim_start_matches([')', ' ', '\t', '.']);
    let (version, _) = first_literal(rest.strip_prefix("version")?)?;
    (!version.contains('$')).then_some(version)
}

/// The plugins that turn a language on.
fn language_plugin(id: &str) -> Option<Language> {
    match id {
        "org.jetbrains.kotlin.jvm" => Some(Language::Kotlin),
        "scala" => Some(Language::Scala),
        "groovy" => Some(Language::Groovy),
        _ => None,
    }
}

/// `org.jetbrains.kotlin.plugin.spring` → `spring`, and kapt, which is a
/// compiler plugin too.
fn kotlin_compiler_plugin(id: &str) -> Option<&str> {
    if id == "org.jetbrains.kotlin.kapt" {
        return Some("kapt");
    }
    id.strip_prefix("org.jetbrains.kotlin.plugin.")
}

/// The Kotlin plugin's version turns `[kotlin]` on, `jvmToolchain` pins the
/// JDK, and the compiler plugins beside it are reported.
fn read_kotlin(script: &str, plugins: &[Plugin], out: &mut Manifest, report: &mut Report) {
    for plugin in plugins {
        let from = format!("plugin `{}`", plugin.id);
        if language_plugin(&plugin.id) == Some(Language::Kotlin) {
            match &plugin.version {
                Some(version) => {
                    super::enable_language(out, Language::Kotlin, version, &from, report);
                }
                None => report.skipped(format!(
                    "{from} — its version is set somewhere jrs does not read (settings, a \
                     catalog, a variable); no [kotlin] table was written"
                )),
            }
        } else if let Some(name) = kotlin_compiler_plugin(&plugin.id) {
            super::report_compiler_plugin(name, &from, report);
        }
    }

    // `jvmToolchain(21)`, or the block form when all it says is the version.
    let kotlin = kotlin_lines(script);
    let jdk = kotlin
        .iter()
        .find_map(|l| number_after(l, "jvmToolchain"))
        .or_else(|| {
            kotlin
                .iter()
                .find_map(|l| number_after(l, "JavaLanguageVersion.of"))
        });
    if let Some(jdk) = jdk
        && out.java.jdk.is_none()
    {
        out.java.jdk = Some(jdk);
        report.migrated(format!("java.jdk = {jdk} (from kotlin {{ jvmToolchain }})"));
    }
    read_kotlinc_args(script, out, report);
}

/// `freeCompilerArgs` → `[kotlin] kotlinc-args`, when every argument is a
/// literal: in `kotlin { compilerOptions { } }`, as start.spring.io writes
/// it, or in the older `kotlinOptions { }` of a `KotlinCompile` task. An
/// `addAll(` or `listOf(` may carry on over several lines.
fn read_kotlinc_args(script: &str, out: &mut Manifest, report: &mut Report) {
    let lines: Vec<&str> = script.lines().map(str::trim).collect();
    let mut args = Vec::new();
    let mut next = 0;
    while next < lines.len() {
        let line = lines[next];
        next += 1;
        if !line.contains("freeCompilerArgs") || line.starts_with("//") {
            continue;
        }
        let mut statement = line.to_string();
        while statement.matches('(').count() > statement.matches(')').count() && next < lines.len()
        {
            statement.push(' ');
            statement.push_str(lines[next]);
            next += 1;
        }
        let literals = quoted(&statement);
        if literals.is_empty()
            || literals.iter().any(|l| l.contains('$'))
            || !only_literals(&statement)
        {
            report.skipped(format!(
                "`{statement}` — not every argument is a literal; set [kotlin] kotlinc-args \
                 by hand"
            ));
            continue;
        }
        args.extend(literals);
    }
    if args.is_empty() {
        return;
    }
    let Some(kotlin) = out
        .languages
        .iter_mut()
        .find(|c| c.language == Language::Kotlin)
    else {
        report.skipped(format!(
            "freeCompilerArgs {args:?} — no [kotlin] table was written to hold them"
        ));
        return;
    };
    report.migrated(format!(
        "[kotlin] kotlinc-args = {args:?} (from freeCompilerArgs)"
    ));
    kotlin.compiler_args.extend(args);
}

/// Whether a `freeCompilerArgs` statement is made of string literals and the
/// few words that put them in a list, and nothing that is worked out when
/// Gradle runs.
fn only_literals(statement: &str) -> bool {
    let mut rest = String::new();
    let mut quote = None;
    for c in statement.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == '"' || c == '\'' => quote = Some(c),
            None => rest.push(c),
        }
    }
    let mut rest = rest.replace("freeCompilerArgs", "");
    for word in ["mutableListOf", "listOf", "arrayOf", "addAll", "add", "set"] {
        rest = rest.replace(word, "");
    }
    rest.chars()
        .all(|c| c.is_whitespace() || "=+.,()[]{}".contains(c))
}

/// `groovy` and `scala` name no version: it is their library's, read out of
/// the dependencies. Then the runtime libraries the tables imply are taken
/// out.
fn read_library_languages(plugins: &[Plugin], out: &mut Manifest, report: &mut Report) {
    for plugin in plugins {
        if let Some(language @ (Language::Scala | Language::Groovy)) = language_plugin(&plugin.id) {
            super::enable_from_library(out, language, &format!("plugin `{}`", plugin.id), report);
        }
    }
    super::drop_implied_libraries(out, report);
}

/// The Guardsquare `ProGuard` plugin (`com.guardsquare.proguard`) → `[obfuscate]`.
/// The plugin's own version is the `ProGuard` release, so it pins `version`; the
/// keep rules and options live in a `proguard { }` task jrs cannot read, so
/// they are flagged for `keep` / `proguard-args`.
fn read_proguard(plugins: &[Plugin], out: &mut Manifest, report: &mut Report) {
    let Some(plugin) = plugins.iter().find(|p| p.id == "com.guardsquare.proguard") else {
        return;
    };
    match &plugin.version {
        Some(version) => {
            report.migrated(format!("obfuscate.version = {version}"));
            report.review(
                "com.guardsquare.proguard — the `proguard { }` task's keep rules and options \
                 do not translate; add them to [obfuscate].keep / proguard-args, then run \
                 `jrs package --obfuscate`"
                    .to_string(),
            );
            out.obfuscate = Some(manifest::ObfuscateConfig {
                version: version.clone(),
                keep: Vec::new(),
                proguard_args: Vec::new(),
            });
        }
        None => report.review(
            "com.guardsquare.proguard — set obfuscate.version to the ProGuard release to pin \
             (the plugin block names no version)"
                .to_string(),
        ),
    }
}

/// The `kotlin { }` extension's lines, opening lines included: `kotlin {
/// jvmToolchain(21) }` is as often written on one line as on three.
fn kotlin_lines(script: &str) -> Vec<&str> {
    let mut lines: Vec<&str> = script
        .lines()
        .filter(|l| is_kotlin_block(l.trim()) || l.trim().starts_with("kotlin."))
        .collect();
    lines.extend(blocks_where(script, is_kotlin_block));
    lines
}

fn is_kotlin_block(header: &str) -> bool {
    let compact: String = header.chars().filter(|c| !c.is_whitespace()).collect();
    compact.starts_with("kotlin{")
}

/// `kotlin("reflect")` → `reflect`, and `kotlin("stdlib", "2.1.0")` with its
/// own version.
fn kotlin_notation(text: &str) -> Option<(String, Option<String>)> {
    let at = text.find("kotlin(")?;
    if text[..at].ends_with(|c: char| c.is_alphanumeric() || c == '_' || c == '.') {
        return None;
    }
    let args = &text[at + "kotlin(".len()..];
    let args = &args[..args.find(')')?];
    let mut literals = quoted(args).into_iter();
    Some((literals.next()?, literals.next()))
}

/// `kotlin("<module>")` is `org.jetbrains.kotlin:kotlin-<module>` at the Kotlin
/// plugin's version, with two readings of jrs's own.
fn kotlin_dependency(
    text: &str,
    module: &str,
    version: Option<&str>,
    report: &mut Report,
) -> Option<Dependency> {
    let Some(version) = version else {
        report.skipped(format!(
            "`{text}` — `kotlin(\"{module}\")` takes the Kotlin plugin's version, and no \
             [kotlin] version was migrated"
        ));
        return None;
    };
    let artifact = match module {
        "stdlib" => "kotlin-stdlib".to_string(),
        // Kotlin 2's stdlib has the classes these once added; they are empty.
        "stdlib-jdk7" | "stdlib-jdk8" => {
            report.migrated(format!(
                "`{text}` — read as kotlin-stdlib, which has had the -jdk7/-jdk8 classes \
                 since Kotlin 1.8"
            ));
            "kotlin-stdlib".to_string()
        }
        "test" => {
            report.review(format!(
                "`{text}` → org.jetbrains.kotlin:kotlin-test-junit5 — Gradle picks \
                 kotlin-test's framework variant by capability, from the test task's \
                 framework; jrs picked the JUnit 5 one"
            ));
            "kotlin-test-junit5".to_string()
        }
        other => format!("kotlin-{other}"),
    };
    Some(Dependency::new("org.jetbrains.kotlin", artifact, version))
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

/// The first quoted literal in `text`, and what follows its closing quote.
fn first_literal(text: &str) -> Option<(String, &str)> {
    let start = text.find(['\'', '"'])?;
    let quote = &text[start..=start];
    let body = &text[start + 1..];
    let end = body.find(quote)?;
    Some((body[..end].to_string(), &body[end + 1..]))
}

/// The whole number right after `needle`: `jvmToolchain(21)`,
/// `JavaLanguageVersion.of(17)`.
fn number_after(line: &str, needle: &str) -> Option<u32> {
    let rest = line.split(needle).nth(1)?;
    let digits: String = rest
        .trim_start_matches(['(', ' '])
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
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

/// `g:a`: a library whose version a platform or BOM supplies.
fn parse_ga(text: &str) -> Option<Dependency> {
    if text.contains(['$', '@']) {
        return None;
    }
    let parts: Vec<&str> = text.split(':').map(str::trim).collect();
    match parts.as_slice() {
        [g, a] if !g.is_empty() && !a.is_empty() => Some(Dependency::new(*g, *a, "")),
        _ => None,
    }
}

/// `implementation group: 'g', name: 'a', version: 'v'` (and `classifier:`).
/// Without `version:`, the version is left to `[managed]`.
fn parse_map_notation(line: &str) -> Option<Dependency> {
    let field = |key: &str| -> Option<String> {
        let at = line.find(&format!("{key}:"))?;
        quoted(&line[at..]).into_iter().next()
    };
    let mut dep = Dependency::new(
        field("group")?,
        field("name")?,
        field("version").unwrap_or_default(),
    );
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
pub(super) fn repository_name(url: &str) -> String {
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
    fn runtime_only_configurations_become_runtime_only() {
        let dir = Dir::new("runtime-only");
        let m = dir
            .migrate(
                "dependencies {\n    runtimeOnly 'org.postgresql:postgresql:42.7.3'\n    \
                 runtime 'ch.qos.logback:logback-classic:1.5.6'\n    \
                 compileOnly 'org.slf4j:slf4j-api:2.0.12'\n    \
                 runtimeOnly 'org.slf4j:slf4j-api:2.0.12'\n    \
                 testRuntimeOnly 'org.junit.platform:junit-platform-launcher:1.10.2'\n}\n",
            )
            .manifest;
        let d = &m.dependencies;
        assert_eq!(d.len(), 3);
        assert!(d[0].runtime_only && d[1].runtime_only);
        assert!(
            !d[2].compile_only && !d[2].runtime_only,
            "compileOnly plus runtimeOnly is a plain dependency"
        );
        assert!(!m.dev_dependencies[0].runtime_only);
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
    fn free_compiler_args_become_kotlinc_args_when_they_are_literals() {
        let dir = Dir::new("kotlinc-args");
        let plugin = "plugins {\n  kotlin(\"jvm\") version \"2.2.21\"\n}\n";
        for (body, expected) in [
            (
                "kotlin {\n  compilerOptions {\n    freeCompilerArgs.addAll(\"-Xjsr305=strict\", \
                 \"-Xcontext-parameters\")\n  }\n}\n",
                &["-Xjsr305=strict", "-Xcontext-parameters"][..],
            ),
            (
                "kotlin {\n  compilerOptions {\n    freeCompilerArgs.addAll(\n      \
                 \"-Xjsr305=strict\",\n      \"-Xcontext-parameters\"\n    )\n  }\n}\n",
                &["-Xjsr305=strict", "-Xcontext-parameters"][..],
            ),
            (
                "tasks.withType<KotlinCompile> {\n  kotlinOptions {\n    \
                 freeCompilerArgs += \"-Xjsr305=strict\"\n    jvmTarget = \"17\"\n  }\n}\n",
                &["-Xjsr305=strict"][..],
            ),
            (
                "tasks.withType<KotlinCompile> {\n  kotlinOptions {\n    \
                 freeCompilerArgs = freeCompilerArgs + listOf(\"-Xjsr305=strict\")\n  }\n}\n",
                &["-Xjsr305=strict"][..],
            ),
        ] {
            let migration = dir.migrate(&format!("{plugin}{body}"));
            let kotlin = &migration.manifest.languages[0];
            assert_eq!(kotlin.compiler_args, expected, "{body}");
            assert!(migration.report.not_migrated.is_empty(), "{body}");
        }

        let computed = dir.migrate(&format!(
            "{plugin}kotlin {{\n  compilerOptions {{\n    freeCompilerArgs.addAll(strictArgs)\n    \
             freeCompilerArgs.add(\"-Xopt-in=${{optIn}}\")\n  }}\n}}\n"
        ));
        assert!(computed.manifest.languages[0].compiler_args.is_empty());
        let skipped = computed.report.not_migrated.join("\n");
        assert_eq!(
            skipped.matches("not every argument is a literal").count(),
            2,
            "{skipped}"
        );
    }

    #[test]
    fn max_parallel_forks_becomes_test_forks_unless_it_is_computed() {
        let dir = Dir::new("forks");
        for script in [
            "test {\n  maxParallelForks = 3\n}\n",
            "test {\n  maxParallelForks 3\n}\n",
            "tasks.withType<Test> {\n  maxParallelForks = 3\n}\n",
        ] {
            let migration = dir.migrate(script);
            assert_eq!(migration.manifest.test.forks, 3, "{script}");
            assert!(
                migration
                    .report
                    .migrated
                    .iter()
                    .any(|s| s == "test.forks = 3 (from maxParallelForks)"),
                "{script}"
            );
        }

        let computed = dir.migrate(
            "test {\n  maxParallelForks = Runtime.runtime.availableProcessors().intdiv(2) ?: 1\n  \
             forkEvery 100\n  maxParallelForksExtra = 2\n}\n",
        );
        assert_eq!(computed.manifest.test.forks, 0);
        let skipped = computed.report.not_migrated.join("\n");
        assert!(skipped.contains("its value is computed"), "{skipped}");
        assert!(
            skipped.contains("`forkEvery 100` — Gradle restarts a test JVM"),
            "{skipped}"
        );
        assert!(!skipped.contains("Extra"), "another setting: {skipped}");

        let default = dir.migrate("test {\n  maxParallelForks = 1\n}\n");
        assert_eq!(default.manifest.test.forks, 0, "one JVM is the default");
    }

    #[test]
    fn the_environment_and_working_directory_of_run_and_test_are_read() {
        let dir = Dir::new("jvm-env");
        let migration = dir.migrate(
            "dependencies {\n  testImplementation 'org.mockito:mockito-core:5.14.2'\n}\n\
             run {\n  workingDir = file('work')\n  environment 'APP_MODE', 'dev'\n  \
             environment 'APP_MODE', 'prod'\n  environment 'BRACES', '{x}'\n}\n\
             test {\n  jvmArgs \"-javaagent:${configurations.mockitoAgent.asPath}\", '-Xmx256m'\n  \
             environment 'TZ', 'UTC'\n  environment 'HOME_DIR', System.getProperty('user.home')\n  \
             environment 'JRS_MODE', 'x'\n  workingDir 'build/tmp'\n}\n",
        );
        let m = &migration.manifest;
        let env = |vars: &[(String, manifest::Template)]| -> Vec<(String, String)> {
            vars.iter()
                .map(|(k, v)| (k.clone(), v.raw.clone()))
                .collect()
        };
        assert_eq!(
            env(&m.run.env),
            [
                ("APP_MODE".to_string(), "prod".to_string()),
                ("BRACES".to_string(), "{{x}}".to_string()),
            ],
            "the last value wins, and braces stay literal"
        );
        assert_eq!(m.run.cwd.as_ref().unwrap().raw, "work");
        assert_eq!(env(&m.test.env), [("TZ".to_string(), "UTC".to_string())]);
        assert_eq!(
            m.test.java_agents,
            vec![Ga::new("org.mockito", "mockito-core")]
        );
        assert_eq!(
            m.test.jvm_args,
            vec!["-Xmx256m"],
            "no path into Gradle's cache"
        );

        let skipped = migration.report.not_migrated.join("\n");
        assert!(skipped.contains("HOME_DIR"), "{skipped}");
        assert!(skipped.contains("JRS_MODE"), "{skipped}");
        assert!(skipped.contains("no `test.cwd`"), "{skipped}");

        // The manifest this writes is one jrs reads back the same.
        let again =
            Manifest::parse(&m.render(None), Path::new("/p/jrs.toml"), Path::new("/p")).unwrap();
        assert_eq!(again.run, m.run);
        assert_eq!(again.test, m.test);
    }

    #[test]
    fn the_kotlin_dsl_names_the_run_task_and_an_unknown_agent_is_reported() {
        let dir = Dir::new("jvm-env-kts");
        let migration = dir.migrate(
            "application {\n  applicationDefaultJvmArgs = listOf(\"-javaagent:/opt/otel.jar\", \"-Xmx1g\")\n}\n\
             tasks.named<JavaExec>(\"run\") {\n  workingDir = projectDir\n  environment(\"A\", \"b\")\n}\n\
             tasks.withType<Test> {\n  environment(mapOf(\"C\" to \"d\"))\n  jvmArgs(\"-javaagent:/opt/x.jar\")\n}\n",
        );
        let m = &migration.manifest;
        assert_eq!(m.run.cwd.as_ref().unwrap().raw, ".", "Gradle's default");
        assert_eq!(m.run.env.len(), 1);
        assert_eq!(m.run.jvm_args, vec!["-Xmx1g"]);
        assert!(m.run.java_agents.is_empty() && m.test.java_agents.is_empty());
        assert!(m.test.env.is_empty());
        let skipped = migration.report.not_migrated.join("\n");
        assert!(skipped.contains("-javaagent:/opt/otel.jar"), "{skipped}");
        assert!(skipped.contains("run.java-agents"), "{skipped}");
        assert!(skipped.contains("test.java-agents"), "{skipped}");
        assert!(skipped.contains("mapOf"), "{skipped}");
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
    fn jar_attributes_are_read_in_the_kotlin_dsl() {
        let dir = Dir::new("jar-kts");
        let migration = dir.migrate(
            "version = \"1.0\"\n\ntasks.jar {\n    manifest {\n        \
             attributes(mapOf(\"Implementation-Title\" to \"kts\", \
             \"Implementation-Version\" to project.version))\n        \
             attributes[\"Automatic-Module-Name\"] = \"com.example.kts\"\n        \
             attributes(\"X-Commit\" to \"${gitCommit}\", \"X-Brace\" to \"a{b}\")\n    \
             }\n}\n",
        );
        let attributes: Vec<(&str, &str)> = migration
            .manifest
            .package
            .manifest
            .iter()
            .map(|(n, t)| (n.as_str(), t.raw.as_str()))
            .collect();
        assert_eq!(
            attributes,
            [
                ("Implementation-Title", "kts"),
                ("Implementation-Version", "{project.version}"),
                ("Automatic-Module-Name", "com.example.kts"),
                ("X-Brace", "a{{b}}"),
            ]
        );
        let skipped = migration.report.not_migrated.join("\n");
        assert!(
            skipped.contains("`X-Commit` — its value is computed"),
            "{skipped}"
        );
    }

    #[test]
    fn the_word_attributes_inside_a_string_is_not_a_call() {
        assert!(jar_attribute_pairs("description = 'no attributes(here)'\n").is_empty());
        assert_eq!(
            jar_attribute_pairs("attributes('A': 'x', 'B': version)"),
            [
                ("A".to_string(), "'x'".to_string()),
                ("B".to_string(), "version".to_string())
            ]
        );
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

    #[test]
    fn the_groovy_dsl_names_kotlin_by_plugin_id() {
        let dir = Dir::new("kotlin-ids");
        let migration = dir.migrate(
            "plugins {\n  id 'org.jetbrains.kotlin.jvm' version '2.2.0'\n  \
             id 'org.jetbrains.kotlin.plugin.jpa' version '2.2.0'\n}\n\
             kotlin {\n  jvmToolchain {\n    languageVersion = JavaLanguageVersion.of(17)\n  }\n}\n\
             dependencies {\n  implementation 'org.jetbrains.kotlin:kotlin-stdlib:2.2.0'\n}\n",
        );
        let m = &migration.manifest;
        assert_eq!(m.language(Language::Kotlin).unwrap().version, "2.2.0");
        assert_eq!(m.java.jdk, Some(17));
        assert_eq!(
            m.java.source, None,
            "Kotlin's toolchain pins the JDK, not the release"
        );
        assert!(m.dependencies.is_empty(), "the stdlib is implied");
        let skipped = migration.report.not_migrated.join("\n");
        assert!(skipped.contains("plugin.jpa"), "{skipped}");
        assert!(skipped.contains("JPA"), "{skipped}");
        assert!(!skipped.contains("no plugin system"), "{skipped}");
    }

    #[test]
    fn kotlin_modules_need_a_kotlin_version_jrs_can_use() {
        let dir = Dir::new("kotlin-old");
        let migration = dir.migrate(
            "plugins {\n  id(\"org.jetbrains.kotlin.jvm\") version \"1.9.24\"\n}\n\
             kotlin { jvmToolchain(21) }\n\
             dependencies {\n  implementation(kotlin(\"stdlib-jdk8\"))\n}\n",
        );
        let m = &migration.manifest;
        assert!(m.languages.is_empty());
        assert!(m.dependencies.is_empty());
        assert_eq!(m.java.jdk, Some(21), "one line is enough for jvmToolchain");
        let skipped = migration.report.not_migrated.join("\n");
        assert!(skipped.contains("Kotlin 2.0"), "{skipped}");
        assert!(skipped.contains("kotlin(\"stdlib-jdk8\")"), "{skipped}");
        let text = migration.render_manifest();
        Manifest::parse(&text, &dir.path.join("jrs.toml"), &dir.path).unwrap();
    }

    #[test]
    fn scala_and_groovy_take_their_librarys_version() {
        let dir = Dir::new("library-versions");
        let file = dir.write(
            "build.gradle.kts",
            "plugins {\n  scala\n}\n\
             dependencies {\n  implementation(\"org.scala-lang:scala-library:2.13.16\")\n}\n",
        );
        let scala = super::migrate(&file, &dir.path).unwrap();
        assert_eq!(
            scala.manifest.language(Language::Scala).unwrap().version,
            "2.13.16"
        );
        assert!(scala.manifest.dependencies.is_empty());

        let groovy = dir.migrate(
            "plugins {\n  id(\"groovy\")\n}\n\
             dependencies {\n  implementation 'org.codehaus.groovy:groovy:3.0.22'\n}\n",
        );
        assert!(groovy.manifest.languages.is_empty());
        assert_eq!(groovy.manifest.dependencies.len(), 1, "kept as it was");
        let skipped = groovy.report.not_migrated.join("\n");
        assert!(skipped.contains("org.apache.groovy"), "{skipped}");
    }

    #[test]
    fn plugin_lines_are_read_in_every_spelling() {
        let read = |line: &str| {
            let p = read_plugin(line).unwrap();
            (p.id, p.version)
        };
        let some = |s: &str| Some(s.to_string());
        assert_eq!(
            read("kotlin(\"jvm\") version \"2.2.0\""),
            ("org.jetbrains.kotlin.jvm".into(), some("2.2.0"))
        );
        assert_eq!(
            read("id(\"x.y\").version(\"1.0\")"),
            ("x.y".into(), some("1.0"))
        );
        assert_eq!(read("id 'x.y' version '1.0'"), ("x.y".into(), some("1.0")));
        assert_eq!(read("`java-library`"), ("java-library".into(), None));
        assert_eq!(
            read("kotlin(\"jvm\") version kotlinVersion"),
            ("org.jetbrains.kotlin.jvm".into(), None)
        );
        assert!(read_plugin("alias(libs.plugins.x)").is_none());
    }

    #[test]
    fn platforms_constraints_and_dependency_management_become_managed() {
        let dir = Dir::new("managed");
        let migration = dir.migrate(
            "plugins { id 'java' }\n\
             dependencyManagement {\n    \
                 imports { mavenBom 'org.springframework.cloud:spring-cloud-dependencies:2025.0.0' }\n    \
                 dependencies {\n        dependency 'com.google.guava:guava:33.0.0-jre'\n    }\n\
             }\n\
             dependencies {\n    \
                 implementation platform('io.micronaut.platform:micronaut-platform:4.5.0')\n    \
                 implementation(enforcedPlatform(\"com.fasterxml.jackson:jackson-bom:2.19.0\"))\n    \
                 implementation 'org.springframework.cloud:spring-cloud-starter-config'\n    \
                 constraints {\n        \
                     implementation 'org.slf4j:slf4j-api:2.0.17'\n        \
                     implementation('org.yaml:snakeyaml') { version { strictly '2.2' } }\n    \
                 }\n    \
                 constraints { runtimeOnly 'org.postgresql:postgresql:42.7.3' }\n\
             }\n",
        );
        let m = &migration.manifest;
        let managed: Vec<(String, bool)> = m
            .managed
            .iter()
            .map(|x| (format!("{}:{}", x.key(), x.version), x.bom))
            .collect();
        assert_eq!(
            managed,
            [
                (
                    "org.springframework.cloud:spring-cloud-dependencies:2025.0.0".to_string(),
                    true
                ),
                ("com.google.guava:guava:33.0.0-jre".to_string(), false),
                (
                    "io.micronaut.platform:micronaut-platform:4.5.0".to_string(),
                    true
                ),
                ("com.fasterxml.jackson:jackson-bom:2.19.0".to_string(), true),
                ("org.slf4j:slf4j-api:2.0.17".to_string(), false),
                ("org.postgresql:postgresql:42.7.3".to_string(), false),
            ]
        );
        // Platforms and constraints are not dependencies.
        assert_eq!(m.dependencies.len(), 1, "{:?}", m.dependencies);
        assert!(m.dependencies[0].is_managed());
        let review = migration.report.needs_review.join("\n");
        assert!(review.contains("enforcedPlatform"), "{review}");
        let skipped = migration.report.not_migrated.join("\n");
        assert!(skipped.contains("snakeyaml"), "{skipped}");
        let text = migration.render_manifest();
        Manifest::parse(&text, &dir.path.join("jrs.toml"), &dir.path).unwrap();
    }

    #[test]
    fn a_dependency_without_a_version_needs_something_to_manage_it() {
        let dir = Dir::new("versionless");
        let migration = dir.migrate(
            "dependencies {\n    implementation 'org.example:lib'\n    \
             implementation group: 'org.example', name: 'other'\n}\n",
        );
        assert!(migration.manifest.dependencies.is_empty());
        let skipped = migration.report.not_migrated.join("\n");
        assert_eq!(
            skipped
                .matches("has no version, and nothing in the build manages one")
                .count(),
            2,
            "{skipped}"
        );
    }

    #[test]
    fn the_spring_boot_plugin_needs_a_version_to_bring_its_bom() {
        let dir = Dir::new("boot-no-version");
        dir.write(
            "src/main/kotlin/com/example/App.kt",
            "package com.example\n\n@SpringBootApplication\nclass App\n",
        );
        let migration = dir.migrate(
            "plugins {\n    id 'org.springframework.boot'\n    \
             id 'io.spring.dependency-management'\n    \
             id 'org.jetbrains.kotlin.jvm' version '2.4.20'\n}\n",
        );
        let m = &migration.manifest;
        assert!(m.managed.is_empty());
        assert_eq!(m.java.javac_args, vec!["-parameters"]);
        assert_eq!(
            m.main_class.as_deref(),
            Some("com.example.AppKt"),
            "Kotlin's main function compiles into <File>Kt"
        );
        let skipped = migration.report.not_migrated.join("\n");
        assert!(
            skipped.contains("Spring Boot's BOM was not added"),
            "{skipped}"
        );
        assert!(!skipped.contains("no plugin system"), "{skipped}");
    }
}
