//! `pom.xml` → `jrs.toml`.
//!
//! The POM parser from `resolve::pom` is reused wholesale (SPEC §11.2), so
//! property interpolation, parent chains and `<dependencyManagement>` already
//! work here. Parent chains are followed on disk only: migration never touches
//! the network, so a parent that lives in a repository is reported rather than
//! silently dropped.

use std::path::{Path, PathBuf};

use super::{
    Migration, Report, Source, drop_implied_libraries, enable_from_library, enable_language,
    report_compiler_plugin,
};
use crate::compile::lang::Language;
use crate::error::{IoResultExt, JrsError, Result};
use crate::manifest::{self, Dependency, Exclusion, Manifest, Repository};
use crate::resolve::coord::{Scope, is_range};
use crate::resolve::pom::{self, Effective, Element, PluginInfo, Pom};

/// Plugins jrs knows how to read something out of. Anything else is reported.
const UNDERSTOOD_PLUGINS: &[&str] = &[
    "maven-compiler-plugin",
    "maven-jar-plugin",
    "maven-surefire-plugin",
    "maven-shade-plugin",
    "kotlin-maven-plugin",
    "scala-maven-plugin",
    "gmavenplus-plugin",
];

/// Translate the POM at `pom_path`, and the parents beside it on disk.
///
/// # Errors
///
/// [`JrsError::Io`] if a POM in the chain cannot be read,
/// [`JrsError::Usage`] if one is not a well-formed POM, and
/// [`JrsError::Resolve`] if the project's groupId or version cannot be worked out.
pub fn migrate(pom_path: &Path, root: &Path) -> Result<Migration> {
    let mut report = Report::default();
    let chain = read_chain(pom_path, &mut report)?;
    let effective = pom::effective(&chain)?;
    let pom = &chain[0];

    let mut out = manifest::blank(&effective.coord.artifact, &effective.coord.version, root);
    report.migrated(format!("project.name = {}", out.name));
    report.migrated(format!("project.version = {}", out.version));

    read_java_settings(&effective, pom, &mut out, &mut report);
    read_main_class(pom, &mut out, &mut report);
    read_dependencies(&effective, &mut out, &mut report);
    // The layout depends on which languages are on, and Scala's and Groovy's
    // versions on the dependencies.
    read_languages(&chain, pom, &effective, &mut out, &mut report);
    read_layout(pom, &mut out, &mut report);
    read_annotation_processors(&effective, pom, &mut out, &mut report);
    read_test_settings(&effective, pom, &mut out, &mut report);
    read_repositories(&effective, &mut out, &mut report);
    read_the_rest(pom, &mut report);

    Ok(Migration {
        source: Source::Maven,
        source_file: pom_path.to_path_buf(),
        manifest: out,
        report,
    })
}

/// Read the POM and follow `<parent>` through `relativePath` while the files
/// exist on disk.
fn read_chain(pom_path: &Path, report: &mut Report) -> Result<Vec<Pom>> {
    let mut chain = Vec::new();
    let mut current = pom_path.to_path_buf();
    loop {
        let bytes = std::fs::read(&current).path(&current)?;
        let parsed = Pom::parse(&bytes)
            .map_err(|e| JrsError::usage(format!("{}: {e}", current.display())))?;
        let parent = parsed.parent.clone();
        let dir = current
            .parent()
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        chain.push(parsed);

        let Some(parent) = parent else { break };
        let relative = parent.relative_path.as_deref().unwrap_or("../pom.xml");
        if relative.is_empty() {
            report.skipped(format!(
                "<parent> {}:{}:{} — resolved from a repository, which migration \
                 does not do; anything it contributes is missing here",
                parent.group, parent.artifact, parent.version
            ));
            break;
        }
        let mut candidate = dir.join(relative);
        if candidate.is_dir() {
            candidate = candidate.join("pom.xml");
        }
        if !candidate.is_file() {
            report.skipped(format!(
                "<parent> {}:{}:{} — not found at {}; its properties, managed \
                 versions and plugins were not applied",
                parent.group,
                parent.artifact,
                parent.version,
                candidate.display()
            ));
            break;
        }
        report.migrated(format!("<parent> read from {}", candidate.display()));
        current = candidate;
        if chain.len() >= 16 {
            report.skipped("<parent> chain deeper than 16; stopped following it");
            break;
        }
    }
    Ok(chain)
}

fn read_java_settings(effective: &Effective, pom: &Pom, out: &mut Manifest, report: &mut Report) {
    let property = |key: &str| effective.properties.get(key).map(String::as_str);
    let compiler = plugin(pom, "maven-compiler-plugin");
    let configured = |key: &str| {
        compiler
            .and_then(|p| p.configuration.as_ref())
            .and_then(|c| c.text_of(key))
            .map(|v| pom::interpolate(v, &effective.properties))
    };

    let release = configured("release")
        .or_else(|| property("maven.compiler.release").map(str::to_string))
        .or_else(|| configured("source"))
        .or_else(|| property("maven.compiler.source").map(str::to_string));
    if let Some(release) = release {
        match parse_release(&release) {
            Some(n) => {
                out.java.source = Some(n);
                report.migrated(format!("java.source = {n}"));
            }
            None => report.review(format!(
                "compiler release `{release}` could not be read as a version; \
                 java.source was left to the JDK default"
            )),
        }
    }

    let target = configured("target")
        .or_else(|| property("maven.compiler.target").map(str::to_string))
        .and_then(|t| parse_release(&t));
    if let Some(target) = target
        && Some(target) != out.java.source
    {
        out.java.target = Some(target);
        report.migrated(format!("java.target = {target}"));
    }

    if let Some(encoding) = property("project.build.sourceEncoding")
        && encoding != "UTF-8"
    {
        out.java.encoding = encoding.to_string();
        report.migrated(format!("java.encoding = {encoding}"));
    }

    if let Some(args) = compiler
        .and_then(|p| p.configuration.as_ref())
        .and_then(|c| c.child("compilerArgs"))
    {
        let flags: Vec<String> = args
            .children
            .iter()
            .map(|a| pom::interpolate(a.text.trim(), &effective.properties))
            .filter(|a| !a.is_empty())
            .collect();
        if !flags.is_empty() {
            report.migrated(format!("java.javac-args = {flags:?}"));
            out.java.javac_args = flags;
        }
    }
}

fn read_layout(pom: &Pom, out: &mut Manifest, report: &mut Report) {
    let build = &pom.build;
    let languages: Vec<&str> = out.languages.iter().map(|c| c.language.key()).collect();
    // `parent` is where a language keeps its own root: `src/main` for
    // `src/main/kotlin`. Such a root is compiled through its table already,
    // and moving the Java root there would lose `src/main/java`.
    let mut set = |value: &Option<String>,
                   field: &mut PathBuf,
                   default: &str,
                   name: &str,
                   parent: Option<&str>| {
        let Some(raw) = value else { return };
        let cleaned = strip_basedir(raw);
        if cleaned == default {
            return;
        }
        if let Some(parent) = parent
            && let Some(key) = languages
                .iter()
                .find(|k| cleaned == format!("{parent}/{k}"))
        {
            report.migrated(format!(
                "project.{name} left at {default}: {cleaned} is [{key}]'s own root, \
                 which jrs compiles too"
            ));
            return;
        }
        *field = PathBuf::from(&cleaned);
        report.review(format!(
            "project.{name} = {cleaned} — a non-default layout; check that it is \
             what you expect"
        ));
    };
    set(
        &build.source_directory,
        &mut out.source_dir,
        "src/main/java",
        "source-dir",
        Some("src/main"),
    );
    set(
        &build.test_source_directory,
        &mut out.test_dir,
        "src/test/java",
        "test-dir",
        Some("src/test"),
    );
    set(
        &build.directory,
        &mut out.target_dir,
        "target",
        "target-dir",
        None,
    );

    match build.resource_directories.len() {
        0 => {}
        1 => {
            let cleaned = strip_basedir(&build.resource_directories[0]);
            if cleaned != "src/main/resources" {
                out.resource_dir = PathBuf::from(&cleaned);
                report.review(format!("project.resource-dir = {cleaned}"));
            }
        }
        n => {
            let cleaned = strip_basedir(&build.resource_directories[0]);
            out.resource_dir = PathBuf::from(&cleaned);
            report.skipped(format!(
                "{n} <resource> directories — jrs supports one; kept {cleaned} and \
                 dropped the rest"
            ));
        }
    }
}

fn read_main_class(pom: &Pom, out: &mut Manifest, report: &mut Report) {
    let from_jar_plugin = plugin(pom, "maven-jar-plugin")
        .and_then(|p| p.configuration.as_ref())
        .and_then(|c| c.path(&["archive", "manifest"]))
        .and_then(|m| m.text_of("mainClass"));

    let from_shade = plugin(pom, "maven-shade-plugin").and_then(|p| {
        // Shade puts its transformers under an <execution>'s configuration as
        // often as under the plugin's own, so both are searched.
        let mut configurations: Vec<&Element> = p
            .executions
            .iter()
            .filter_map(|e| e.child("configuration"))
            .collect();
        configurations.extend(p.configuration.as_ref());
        configurations
            .into_iter()
            .filter_map(|c| c.child("transformers"))
            .flat_map(|t| &t.children)
            .find_map(|t| t.text_of("mainClass"))
    });

    if let Some(main) = from_jar_plugin.or(from_shade) {
        out.main_class = Some(main.to_string());
        report.migrated(format!("project.main-class = {main}"));
    }
}

fn read_dependencies(effective: &Effective, out: &mut Manifest, report: &mut Report) {
    for raw in &effective.dependencies {
        let managed = effective.manage(raw);
        let key = format!("{}:{}", managed.group, managed.artifact);

        if managed.optional {
            report.skipped(format!("{key} — <optional>true</optional>"));
            continue;
        }
        // `<type>` names a kind of file; the jar-shaped ones translate, and a
        // test-jar is simply the jar classified `tests`.
        let classifier = match managed.kind.as_str() {
            "jar" | "bundle" | "ejb" => managed.classifier.clone(),
            "test-jar" => Some(
                managed
                    .classifier
                    .clone()
                    .unwrap_or_else(|| "tests".to_string()),
            ),
            other => {
                report.skipped(format!(
                    "{key} — <type>{other}</type> does not go on a classpath"
                ));
                continue;
            }
        };

        let scope = managed.scope();
        let (target, compile_only) = match scope {
            Scope::Compile | Scope::Runtime => (&mut out.dependencies, false),
            Scope::Provided => (&mut out.dependencies, true),
            Scope::Test => (&mut out.dev_dependencies, false),
            Scope::System | Scope::Import => {
                report.skipped(format!(
                    "{key} — <scope>{}</scope> has no equivalent in jrs.toml",
                    scope.as_str()
                ));
                continue;
            }
        };

        let Some(version) = managed.version.clone() else {
            report.skipped(format!(
                "{key} — no version, and none found in <dependencyManagement>"
            ));
            continue;
        };
        if version.contains("${") {
            report.skipped(format!("{key} — version `{version}` did not interpolate"));
            continue;
        }
        let version = if is_range(&version) {
            let clamped = lower_bound(&version);
            report.review(format!(
                "{key} — the range `{version}` was clamped to {clamped}"
            ));
            clamped
        } else {
            version
        };

        let mut dep = Dependency::new(&managed.group, &managed.artifact, version);
        dep.classifier = classifier;
        dep.compile_only = compile_only;
        dep.exclusions = managed
            .exclusions
            .iter()
            .map(|e| Exclusion {
                group: e.group.clone(),
                artifact: e.artifact.clone(),
            })
            .collect();
        if target.iter().any(|d| d.key() == dep.key()) {
            report.skipped(format!(
                "{} — declared twice in the same scope; kept the first",
                dep.key()
            ));
            continue;
        }

        let mut notes = vec![scope.as_str().to_string()];
        if compile_only {
            notes.push("as compile-only".to_string());
        }
        if !dep.exclusions.is_empty() {
            notes.push(format!("{} exclusions", dep.exclusions.len()));
        }
        report.migrated(format!("{} ({})", dep.key(), notes.join(", ")));
        target.push(dep);
    }
}

/// `maven-surefire-plugin`'s `<argLine>` and `<systemPropertyVariables>` are
/// the test JVM's arguments.
fn read_test_settings(effective: &Effective, pom: &Pom, out: &mut Manifest, report: &mut Report) {
    let Some(config) = plugin(pom, "maven-surefire-plugin").and_then(|p| p.configuration.as_ref())
    else {
        return;
    };
    let mut args = Vec::new();
    if let Some(line) = config.text_of("argLine") {
        let line = pom::interpolate(line, &effective.properties);
        if line.contains("${") || line.contains("@{") {
            // Usually `@{argLine}`, filled in at build time by JaCoCo's plugin.
            report.skipped(format!(
                "surefire <argLine>{line}</argLine> — refers to a property another \
                 plugin sets at build time; use `jrs test --coverage` for JaCoCo"
            ));
        } else {
            args.extend(split_arguments(&line));
        }
    }
    if let Some(props) = config.child("systemPropertyVariables") {
        for p in &props.children {
            let value = pom::interpolate(p.text.trim(), &effective.properties);
            args.push(format!("-D{}={value}", p.name));
        }
    }
    if !args.is_empty() {
        report.migrated(format!(
            "test.jvm-args = {args:?} (from maven-surefire-plugin)"
        ));
        out.test.jvm_args = args;
    }
}

/// `<annotationProcessorPaths>`: jrs has no processor path (SPEC §1.2), but a
/// processor on the compile classpath runs all the same, so each becomes a
/// compile-only dependency. Since JDK 23 `javac` wants `-proc:full` for that.
fn read_annotation_processors(
    effective: &Effective,
    pom: &Pom,
    out: &mut Manifest,
    report: &mut Report,
) {
    let Some(paths) = plugin(pom, "maven-compiler-plugin")
        .and_then(|p| p.configuration.as_ref())
        .and_then(|c| c.child("annotationProcessorPaths"))
    else {
        return;
    };
    let mut added = Vec::new();
    for path in &paths.children {
        let (Some(group), Some(artifact)) = (path.text_of("groupId"), path.text_of("artifactId"))
        else {
            continue;
        };
        let group = pom::interpolate(group, &effective.properties);
        let artifact = pom::interpolate(artifact, &effective.properties);
        let managed = effective
            .managed
            .get(&crate::resolve::coord::Ga::new(&group, &artifact))
            .and_then(|m| m.version.clone());
        let Some(version) = path
            .text_of("version")
            .map(|v| pom::interpolate(v, &effective.properties))
            .or(managed)
            .filter(|v| !v.contains("${"))
        else {
            report.skipped(format!(
                "annotation processor {group}:{artifact} — no version could be read"
            ));
            continue;
        };
        if out
            .dependencies
            .iter()
            .any(|d| d.group == group && d.artifact == artifact)
        {
            added.push(format!("{group}:{artifact}"));
            continue;
        }
        let mut dep = Dependency::new(&group, &artifact, version);
        dep.compile_only = true;
        added.push(dep.key());
        out.dependencies.push(dep);
    }
    if added.is_empty() {
        return;
    }
    let has_flag = out.java.javac_args.iter().any(|a| a.starts_with("-proc:"));
    if out.java.source.is_some_and(|s| s >= 21) && !has_flag {
        out.java.javac_args.push("-proc:full".to_string());
    }
    report.review(format!(
        "annotation processors {} — put on the compile classpath as compile-only \
         dependencies; javac from JDK 23 on runs them only with `-proc:full` in \
         java.javac-args{}",
        added.join(", "),
        if out.java.javac_args.iter().any(|a| a == "-proc:full") {
            ", which was added"
        } else {
            ", which JDK 17 does not accept, so it was not added"
        }
    ));
}

/// Split a command line on whitespace, keeping double-quoted runs together.
fn split_arguments(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for c in line.chars() {
        match c {
            '"' => quoted = !quoted,
            c if c.is_whitespace() && !quoted => {
                if !current.is_empty() {
                    out.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

fn read_repositories(effective: &Effective, out: &mut Manifest, report: &mut Report) {
    let mut repos: Vec<Repository> = Vec::new();
    for (id, url) in &effective.repositories {
        let url = url.trim_end_matches('/').to_string();
        if url == manifest::CENTRAL_URL {
            continue;
        }
        report.migrated(format!("repository {id} = {url}"));
        repos.push(Repository {
            name: id.clone(),
            url,
        });
    }
    repos.push(Repository {
        name: manifest::CENTRAL_NAME.into(),
        url: manifest::CENTRAL_URL.into(),
    });
    out.repositories = repos;
}

fn read_the_rest(pom: &Pom, report: &mut Report) {
    if !pom.modules.is_empty() {
        report.skipped(format!(
            "<modules> {} — jrs builds one module per manifest; migrate each of \
             them separately with `jrs migrate --path <module>`",
            pom.modules.join(", ")
        ));
    }
    for plugin in &pom.build.plugins {
        if !UNDERSTOOD_PLUGINS.contains(&plugin.artifact.as_str()) {
            report.skipped(format!(
                "plugin {} — jrs has no plugin system; whatever it did must be done \
                 another way",
                plugin.artifact
            ));
        }
    }
    for profile in &pom.profiles {
        if profile.active_by_default {
            report.review(format!(
                "<profile> {} is active by default; its contents were not merged",
                profile.id
            ));
        } else {
            report.skipped(format!(
                "<profile> {} — only default-active profiles are read",
                profile.id
            ));
        }
    }
}

/// The compiler plugins for Kotlin, Scala and Groovy turn their language on
/// (`JVM_LANGUAGES.md` §10), and the runtime library each then implies is taken
/// out of `[dependencies]`.
fn read_languages(
    chain: &[Pom],
    pom: &Pom,
    effective: &Effective,
    out: &mut Manifest,
    report: &mut Report,
) {
    if let Some(kotlin) = plugin(pom, "kotlin-maven-plugin") {
        read_kotlin(chain, effective, kotlin, out, report);
    }
    if let Some(scala) = plugin(pom, "scala-maven-plugin") {
        let configured = scala
            .configuration
            .as_ref()
            .and_then(|c| c.text_of("scalaVersion"))
            .map(|v| pom::interpolate(v, &effective.properties));
        match configured {
            Some(version) => {
                enable_language(
                    out,
                    Language::Scala,
                    &version,
                    "scala-maven-plugin <scalaVersion>",
                    report,
                );
            }
            None => enable_from_library(out, Language::Scala, "scala-maven-plugin", report),
        }
    }
    if plugin(pom, "gmavenplus-plugin").is_some() {
        enable_from_library(out, Language::Groovy, "gmavenplus-plugin", report);
    }
    drop_implied_libraries(out, report);
}

/// kotlin-maven-plugin: its version is the compiler's, `<jvmTarget>` is the
/// release when nothing else set one, and its compiler plugins and kapt are
/// reported.
fn read_kotlin(
    chain: &[Pom],
    effective: &Effective,
    kotlin: &PluginInfo,
    out: &mut Manifest,
    report: &mut Report,
) {
    match plugin_version(chain, "kotlin-maven-plugin") {
        Some(version) => {
            let version = pom::interpolate(&version, &effective.properties);
            enable_language(
                out,
                Language::Kotlin,
                &version,
                "kotlin-maven-plugin",
                report,
            );
        }
        None => enable_from_library(out, Language::Kotlin, "kotlin-maven-plugin", report),
    }

    let jvm_target = kotlin
        .configuration
        .as_ref()
        .and_then(|c| c.text_of("jvmTarget"))
        .map(str::to_string)
        .or_else(|| {
            effective
                .properties
                .get("kotlin.compiler.jvmTarget")
                .cloned()
        })
        .map(|t| pom::interpolate(&t, &effective.properties));
    if let Some(raw) = jvm_target {
        match (parse_release(&raw), out.java.source) {
            (Some(n), None) => {
                out.java.source = Some(n);
                report.migrated(format!(
                    "java.source = {n} (from kotlin-maven-plugin <jvmTarget>)"
                ));
            }
            (Some(n), Some(source)) if n != source => report.review(format!(
                "kotlin-maven-plugin <jvmTarget>{n}</jvmTarget> — java.source is {source}, \
                 and jrs compiles Java and Kotlin for that one release"
            )),
            (Some(_), Some(_)) => {}
            (None, _) => report.review(format!(
                "kotlin-maven-plugin <jvmTarget>{raw}</jvmTarget> could not be read as a \
                 Java release; java.source was not set from it"
            )),
        }
    }

    // `<compilerPlugins>` sits under the plugin's configuration or an
    // execution's, as shade's transformers do.
    let mut configurations: Vec<&Element> = kotlin
        .executions
        .iter()
        .filter_map(|e| e.child("configuration"))
        .collect();
    configurations.extend(kotlin.configuration.as_ref());
    let mut seen: Vec<String> = Vec::new();
    for name in configurations
        .iter()
        .filter_map(|c| c.child("compilerPlugins"))
        .flat_map(|list| &list.children)
        .map(|p| p.text.trim().to_string())
        .filter(|n| !n.is_empty())
    {
        if !seen.contains(&name) {
            report_compiler_plugin(
                &name,
                &format!("kotlin-maven-plugin compiler plugin `{name}`"),
                report,
            );
            seen.push(name);
        }
    }
    let kapt = kotlin.executions.iter().any(|e| {
        e.list("goals", "goal")
            .iter()
            .any(|g| g.text.trim() == "kapt")
    });
    if kapt {
        report_compiler_plugin("kapt", "kotlin-maven-plugin's `kapt` goal", report);
    }
}

/// A plugin's `<version>`, which `PluginInfo` does not keep: from `<plugins>`,
/// else from `<pluginManagement>`, nearest POM first. Not yet interpolated.
fn plugin_version(chain: &[Pom], artifact: &str) -> Option<String> {
    let find = |plugins: Option<&Element>| {
        plugins?
            .children_named("plugin")
            .find(|p| p.text_of("artifactId") == Some(artifact))?
            .text_of("version")
            .map(str::to_string)
    };
    chain.iter().find_map(|pom| {
        let build = pom.root.child("build")?;
        find(build.child("plugins")).or_else(|| find(build.path(&["pluginManagement", "plugins"])))
    })
}

fn plugin<'a>(pom: &'a Pom, artifact: &str) -> Option<&'a PluginInfo> {
    pom.build.plugins.iter().find(|p| p.artifact == artifact)
}

/// `${project.basedir}/src/gen` → `src/gen`.
fn strip_basedir(raw: &str) -> String {
    let mut s = raw.trim().replace('\\', "/");
    for prefix in [
        "${project.basedir}/",
        "${basedir}/",
        "${project.build.directory}/",
        "./",
    ] {
        if let Some(rest) = s.strip_prefix(prefix) {
            s = rest.to_string();
        }
    }
    s.trim_start_matches('/').to_string()
}

/// `21`, `1.8`, `17` — Maven accepts all three spellings.
fn parse_release(raw: &str) -> Option<u32> {
    let t = raw.trim();
    if let Some(rest) = t.strip_prefix("1.") {
        return rest.parse().ok();
    }
    t.parse().ok()
}

/// The lower bound of a Maven range, which is the closest single version to it.
fn lower_bound(range: &str) -> String {
    let inner = range
        .trim()
        .trim_start_matches(['[', '('])
        .trim_end_matches([']', ')']);
    let first = inner.split(',').next().unwrap_or("").trim();
    if first.is_empty() {
        inner.trim().to_string()
    } else {
        first.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Dir {
        path: PathBuf,
    }

    impl Dir {
        fn new(name: &str) -> Dir {
            let path =
                std::env::temp_dir().join(format!("jrs-maven-{name}-{}", std::process::id()));
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

        fn migrate(&self, pom: &str) -> Migration {
            let file = self.write("pom.xml", pom);
            super::migrate(&file, &self.path).unwrap()
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    const FULL: &str = r#"<project>
  <groupId>com.example</groupId>
  <artifactId>my-app</artifactId>
  <version>1.0.0</version>
  <properties>
    <maven.compiler.release>21</maven.compiler.release>
    <project.build.sourceEncoding>UTF-8</project.build.sourceEncoding>
    <guava.version>33.0.0-jre</guava.version>
  </properties>
  <dependencies>
    <dependency>
      <groupId>com.google.guava</groupId>
      <artifactId>guava</artifactId>
      <version>${guava.version}</version>
    </dependency>
    <dependency>
      <groupId>org.junit.jupiter</groupId>
      <artifactId>junit-jupiter</artifactId>
      <version>5.10.2</version>
      <scope>test</scope>
    </dependency>
    <dependency>
      <groupId>jakarta.servlet</groupId>
      <artifactId>jakarta.servlet-api</artifactId>
      <version>6.0.0</version>
      <scope>provided</scope>
    </dependency>
    <dependency>
      <groupId>org.projectlombok</groupId>
      <artifactId>lombok</artifactId>
      <version>1.18.30</version>
      <optional>true</optional>
    </dependency>
  </dependencies>
  <repositories>
    <repository><id>internal</id><url>https://nexus.example.com/maven/</url></repository>
  </repositories>
  <build>
    <plugins>
      <plugin>
        <artifactId>maven-jar-plugin</artifactId>
        <configuration><archive><manifest>
          <mainClass>com.example.Main</mainClass>
        </manifest></archive></configuration>
      </plugin>
      <plugin>
        <artifactId>maven-antrun-plugin</artifactId>
      </plugin>
    </plugins>
  </build>
</project>"#;

    #[test]
    fn the_basics_are_translated() {
        let dir = Dir::new("basics");
        let m = dir.migrate(FULL).manifest;
        assert_eq!(m.name, "my-app");
        assert_eq!(m.version, "1.0.0");
        assert_eq!(m.java.source, Some(21));
        assert_eq!(m.main_class.as_deref(), Some("com.example.Main"));
    }

    #[test]
    fn scopes_route_dependencies_to_the_right_table() {
        let dir = Dir::new("scopes");
        let migration = dir.migrate(FULL);
        let m = &migration.manifest;
        assert_eq!(m.dependencies.len(), 2);
        assert_eq!(
            m.dependencies[0].to_string(),
            "com.google.guava:guava:33.0.0-jre"
        );
        assert_eq!(m.dev_dependencies.len(), 1);
        assert_eq!(
            m.dev_dependencies[0].to_string(),
            "org.junit.jupiter:junit-jupiter:5.10.2"
        );

        // `provided` is what `compile-only` is for.
        let servlet = &m.dependencies[1];
        assert_eq!(servlet.artifact, "jakarta.servlet-api");
        assert!(servlet.compile_only);

        let skipped = migration.report.not_migrated.join("\n");
        assert!(skipped.contains("lombok"), "{skipped}");
        assert!(!skipped.contains("jakarta.servlet-api"), "{skipped}");
    }

    #[test]
    fn classifiers_test_jars_and_system_scope() {
        let dir = Dir::new("classifiers");
        let migration = dir.migrate(
            "<project><groupId>g</groupId><artifactId>a</artifactId><version>1</version>\
             <dependencies>\
             <dependency><groupId>org.lwjgl</groupId><artifactId>lwjgl</artifactId>\
             <version>3.3.3</version><classifier>natives-linux</classifier></dependency>\
             <dependency><groupId>g</groupId><artifactId>core</artifactId>\
             <version>1.0</version><type>test-jar</type><scope>test</scope></dependency>\
             <dependency><groupId>g</groupId><artifactId>war</artifactId>\
             <version>1.0</version><type>war</type></dependency>\
             <dependency><groupId>g</groupId><artifactId>local</artifactId>\
             <version>1.0</version><scope>system</scope></dependency>\
             </dependencies></project>",
        );
        let m = &migration.manifest;
        assert_eq!(m.dependencies.len(), 1);
        assert_eq!(m.dependencies[0].key(), "org.lwjgl:lwjgl:natives-linux");
        assert_eq!(
            m.dev_dependencies[0].classifier.as_deref(),
            Some("tests"),
            "a test-jar is the jar classified `tests`"
        );
        let skipped = migration.report.not_migrated.join("\n");
        assert!(skipped.contains("<type>war</type>"), "{skipped}");
        assert!(skipped.contains("system"), "{skipped}");
    }

    #[test]
    fn surefire_arguments_become_test_jvm_arguments() {
        let dir = Dir::new("surefire");
        let migration = dir.migrate(
            "<project><groupId>g</groupId><artifactId>a</artifactId><version>1</version>\
             <properties><heap>512m</heap></properties>\
             <build><plugins><plugin><artifactId>maven-surefire-plugin</artifactId>\
             <configuration><argLine>-Xmx${heap} -Dname=\"two words\"</argLine>\
             <systemPropertyVariables><env>test</env></systemPropertyVariables>\
             </configuration></plugin></plugins></build></project>",
        );
        assert_eq!(
            migration.manifest.test.jvm_args,
            vec!["-Xmx512m", "-Dname=two words", "-Denv=test"]
        );

        let jacoco = dir.migrate(
            "<project><groupId>g</groupId><artifactId>a</artifactId><version>1</version>\
             <build><plugins><plugin><artifactId>maven-surefire-plugin</artifactId>\
             <configuration><argLine>@{argLine} -Xmx1g</argLine></configuration>\
             </plugin></plugins></build></project>",
        );
        assert!(jacoco.manifest.test.jvm_args.is_empty());
        let skipped = jacoco.report.not_migrated.join("\n");
        assert!(skipped.contains("--coverage"), "{skipped}");
    }

    #[test]
    fn annotation_processor_paths_become_compile_only_dependencies() {
        let dir = Dir::new("processors");
        let migration = dir.migrate(
            "<project><groupId>g</groupId><artifactId>a</artifactId><version>1</version>\
             <properties><maven.compiler.release>21</maven.compiler.release></properties>\
             <build><plugins><plugin><artifactId>maven-compiler-plugin</artifactId>\
             <configuration><annotationProcessorPaths><path>\
             <groupId>org.mapstruct</groupId><artifactId>mapstruct-processor</artifactId>\
             <version>1.5.5.Final</version></path></annotationProcessorPaths>\
             </configuration></plugin></plugins></build></project>",
        );
        let m = &migration.manifest;
        assert_eq!(m.dependencies[0].key(), "org.mapstruct:mapstruct-processor");
        assert!(m.dependencies[0].compile_only);
        assert_eq!(m.java.javac_args, vec!["-proc:full"]);
        let review = migration.report.needs_review.join("\n");
        assert!(review.contains("-proc:full"), "{review}");
    }

    #[test]
    fn arguments_split_on_whitespace_outside_quotes() {
        assert_eq!(
            split_arguments(" -Xmx1g   -Da=\"b c\" -ea "),
            vec!["-Xmx1g", "-Da=b c", "-ea"]
        );
    }

    #[test]
    fn unknown_plugins_are_named_in_the_report() {
        let dir = Dir::new("plugins");
        let migration = dir.migrate(FULL);
        let skipped = migration.report.not_migrated.join("\n");
        assert!(skipped.contains("maven-antrun-plugin"), "{skipped}");
        assert!(
            !skipped.contains("maven-jar-plugin"),
            "the jar plugin is understood: {skipped}"
        );
    }

    #[test]
    fn repositories_are_carried_over_with_central_last() {
        let dir = Dir::new("repos");
        let m = dir.migrate(FULL).manifest;
        assert_eq!(m.repositories.len(), 2);
        assert_eq!(m.repositories[0].name, "internal");
        assert_eq!(m.repositories[0].url, "https://nexus.example.com/maven");
        assert_eq!(m.repositories[1].url, manifest::CENTRAL_URL);
    }

    #[test]
    fn a_local_parent_is_followed() {
        let dir = Dir::new("parent");
        dir.write(
            "pom.xml",
            "<project><groupId>g</groupId><artifactId>parent</artifactId>\
             <version>2.0.0</version><packaging>pom</packaging>\
             <properties><maven.compiler.release>17</maven.compiler.release></properties>\
             <dependencyManagement><dependencies><dependency>\
             <groupId>com.google.guava</groupId><artifactId>guava</artifactId>\
             <version>32.1.3-jre</version></dependency></dependencies></dependencyManagement>\
             </project>",
        );
        let child = dir.write(
            "app/pom.xml",
            "<project><parent><groupId>g</groupId><artifactId>parent</artifactId>\
             <version>2.0.0</version></parent><artifactId>app</artifactId>\
             <dependencies><dependency><groupId>com.google.guava</groupId>\
             <artifactId>guava</artifactId></dependency></dependencies></project>",
        );
        let migration = super::migrate(&child, &dir.path.join("app")).unwrap();
        let m = &migration.manifest;
        assert_eq!(m.name, "app");
        assert_eq!(m.version, "2.0.0", "inherited from the parent");
        assert_eq!(m.java.source, Some(17));
        assert_eq!(m.dependencies[0].version, "32.1.3-jre");
    }

    #[test]
    fn a_parent_that_is_not_on_disk_is_reported_not_guessed() {
        let dir = Dir::new("remote-parent");
        let migration = dir.migrate(
            "<project><parent><groupId>org.springframework.boot</groupId>\
             <artifactId>spring-boot-starter-parent</artifactId><version>3.2.0</version>\
             <relativePath/></parent><artifactId>app</artifactId>\
             <version>1.0.0</version></project>",
        );
        let skipped = migration.report.not_migrated.join("\n");
        assert!(skipped.contains("spring-boot-starter-parent"), "{skipped}");
        assert!(skipped.contains("repository"), "{skipped}");
    }

    #[test]
    fn modules_are_listed_rather_than_migrated() {
        let dir = Dir::new("modules");
        let migration = dir.migrate(
            "<project><groupId>g</groupId><artifactId>agg</artifactId><version>1</version>\
             <packaging>pom</packaging><modules><module>core</module><module>web</module>\
             </modules></project>",
        );
        let skipped = migration.report.not_migrated.join("\n");
        assert!(skipped.contains("core, web"), "{skipped}");
        assert!(skipped.contains("separately"), "{skipped}");
    }

    #[test]
    fn a_non_default_layout_is_flagged_for_review() {
        let dir = Dir::new("layout");
        let migration = dir.migrate(
            "<project><groupId>g</groupId><artifactId>a</artifactId><version>1</version>\
             <build><sourceDirectory>${project.basedir}/java</sourceDirectory>\
             <directory>build</directory></build></project>",
        );
        assert_eq!(migration.manifest.source_dir, PathBuf::from("java"));
        assert_eq!(migration.manifest.target_dir, PathBuf::from("build"));
        let review = migration.report.needs_review.join("\n");
        assert!(review.contains("source-dir = java"), "{review}");
    }

    #[test]
    fn a_default_layout_is_not_written_out() {
        let dir = Dir::new("default-layout");
        let migration = dir.migrate(
            "<project><groupId>g</groupId><artifactId>a</artifactId><version>1</version>\
             <build><sourceDirectory>src/main/java</sourceDirectory></build></project>",
        );
        assert!(migration.report.needs_review.is_empty());
        assert!(!migration.render_manifest().contains("source-dir"));
    }

    #[test]
    fn a_version_range_is_clamped_and_flagged() {
        let dir = Dir::new("range");
        let migration = dir.migrate(
            "<project><groupId>g</groupId><artifactId>a</artifactId><version>1</version>\
             <dependencies><dependency><groupId>g</groupId><artifactId>lib</artifactId>\
             <version>[1.2,2.0)</version></dependency></dependencies></project>",
        );
        assert_eq!(migration.manifest.dependencies[0].version, "1.2");
        let review = migration.report.needs_review.join("\n");
        assert!(review.contains("clamped to 1.2"), "{review}");
    }

    #[test]
    fn exclusions_are_carried_over() {
        let dir = Dir::new("exclusions");
        let migration = dir.migrate(
            "<project><groupId>g</groupId><artifactId>a</artifactId><version>1</version>\
             <dependencies><dependency><groupId>g</groupId><artifactId>lib</artifactId>\
             <version>1.0</version><exclusions><exclusion><groupId>x</groupId>\
             <artifactId>y</artifactId></exclusion></exclusions></dependency>\
             </dependencies></project>",
        );
        let dep = &migration.manifest.dependencies[0];
        assert_eq!(dep.exclusions.len(), 1);
        assert_eq!(dep.exclusions[0].to_string(), "x:y");
        assert!(migration.report.not_migrated.is_empty());
        assert!(
            migration
                .render_manifest()
                .contains(r#""g:lib" = { version = "1.0", exclusions = ["x:y"] }"#)
        );
    }

    #[test]
    fn the_shade_plugins_main_class_is_found() {
        let dir = Dir::new("shade");
        let migration = dir.migrate(
            "<project><groupId>g</groupId><artifactId>a</artifactId><version>1</version>\
             <build><plugins><plugin><artifactId>maven-shade-plugin</artifactId>\
             <executions><execution><configuration><transformers><transformer>\
             <mainClass>com.example.Boot</mainClass></transformer></transformers>\
             </configuration></execution></executions></plugin></plugins></build></project>",
        );
        assert_eq!(
            migration.manifest.main_class.as_deref(),
            Some("com.example.Boot")
        );
    }

    #[test]
    fn compiler_arguments_are_carried_over() {
        let dir = Dir::new("compiler-args");
        let migration = dir.migrate(
            "<project><groupId>g</groupId><artifactId>a</artifactId><version>1</version>\
             <build><plugins><plugin><artifactId>maven-compiler-plugin</artifactId>\
             <configuration><release>21</release><compilerArgs>\
             <arg>-Xlint:all</arg><arg>-Werror</arg></compilerArgs></configuration>\
             </plugin></plugins></build></project>",
        );
        assert_eq!(migration.manifest.java.source, Some(21));
        assert_eq!(
            migration.manifest.java.javac_args,
            vec!["-Xlint:all", "-Werror"]
        );
    }

    #[test]
    fn profiles_are_listed() {
        let dir = Dir::new("profiles");
        let migration = dir.migrate(
            "<project><groupId>g</groupId><artifactId>a</artifactId><version>1</version>\
             <profiles><profile><id>ci</id></profile>\
             <profile><id>dev</id><activation><activeByDefault>true</activeByDefault>\
             </activation></profile></profiles></project>",
        );
        assert!(
            migration
                .report
                .not_migrated
                .iter()
                .any(|s| s.contains("ci"))
        );
        assert!(
            migration
                .report
                .needs_review
                .iter()
                .any(|s| s.contains("dev"))
        );
    }

    #[test]
    fn releases_parse_in_every_spelling_maven_accepts() {
        assert_eq!(parse_release("21"), Some(21));
        assert_eq!(parse_release("1.8"), Some(8));
        assert_eq!(parse_release(" 17 "), Some(17));
        assert_eq!(parse_release("nonsense"), None);
    }

    #[test]
    fn basedir_prefixes_are_stripped() {
        assert_eq!(strip_basedir("${project.basedir}/src/gen"), "src/gen");
        assert_eq!(strip_basedir("${basedir}/src"), "src");
        assert_eq!(strip_basedir("./src"), "src");
        assert_eq!(strip_basedir("src/main/java"), "src/main/java");
    }

    #[test]
    fn range_lower_bounds_are_extracted() {
        assert_eq!(lower_bound("[1.2,2.0)"), "1.2");
        assert_eq!(lower_bound("(1.0,]"), "1.0");
        assert_eq!(lower_bound("[1.5]"), "1.5");
    }

    #[test]
    fn the_generated_manifest_parses_back() {
        let dir = Dir::new("round-trip");
        let migration = dir.migrate(FULL);
        let text = migration.render_manifest();
        let parsed = Manifest::parse(&text, &dir.path.join("jrs.toml"), &dir.path).unwrap();
        assert_eq!(parsed.name, "my-app");
        assert_eq!(parsed.dependencies.len(), 2);
        assert_eq!(parsed.dev_dependencies.len(), 1);
        assert!(parsed.warnings.is_empty(), "{:?}", parsed.warnings);
    }

    #[test]
    fn the_kotlin_version_can_come_from_plugin_management() {
        let dir = Dir::new("kotlin-managed");
        let migration = dir.migrate(
            "<project><groupId>g</groupId><artifactId>a</artifactId><version>1</version>\
             <properties><kotlin.version>2.2.0</kotlin.version></properties>\
             <dependencies><dependency><groupId>org.jetbrains.kotlin</groupId>\
             <artifactId>kotlin-stdlib</artifactId><version>2.1.0</version></dependency>\
             </dependencies>\
             <build><pluginManagement><plugins><plugin>\
             <groupId>org.jetbrains.kotlin</groupId><artifactId>kotlin-maven-plugin</artifactId>\
             <version>${kotlin.version}</version></plugin></plugins></pluginManagement>\
             <plugins><plugin><groupId>org.jetbrains.kotlin</groupId>\
             <artifactId>kotlin-maven-plugin</artifactId>\
             <executions><execution><goals><goal>kapt</goal></goals></execution></executions>\
             </plugin></plugins></build></project>",
        );
        let m = &migration.manifest;
        assert_eq!(m.language(Language::Kotlin).unwrap().version, "2.2.0");
        assert_eq!(
            m.dependencies[0].version, "2.1.0",
            "a stdlib at another version than the compiler is kept"
        );
        let review = migration.report.needs_review.join("\n");
        assert!(review.contains("kept at the version"), "{review}");
        let skipped = migration.report.not_migrated.join("\n");
        assert!(skipped.contains("`kapt` goal"), "{skipped}");
        assert!(!skipped.contains("no plugin system"), "{skipped}");
    }

    #[test]
    fn a_compiler_jrs_cannot_drive_gets_no_table() {
        let dir = Dir::new("old-compilers");
        for (build, reason) in [
            (
                "<build><plugins><plugin><groupId>org.jetbrains.kotlin</groupId>\
                 <artifactId>kotlin-maven-plugin</artifactId><version>1.9.24</version>\
                 </plugin></plugins></build>",
                "Kotlin 2.0",
            ),
            (
                "<dependencies><dependency><groupId>org.scala-lang</groupId>\
                 <artifactId>scala-library</artifactId><version>2.12.18</version>\
                 </dependency></dependencies><build><plugins><plugin>\
                 <groupId>net.alchim31.maven</groupId><artifactId>scala-maven-plugin</artifactId>\
                 </plugin></plugins></build>",
                "2.12 is not supported",
            ),
            (
                "<dependencies><dependency><groupId>org.codehaus.groovy</groupId>\
                 <artifactId>groovy</artifactId><version>3.0.22</version>\
                 </dependency></dependencies><build><plugins><plugin>\
                 <groupId>org.codehaus.gmavenplus</groupId><artifactId>gmavenplus-plugin</artifactId>\
                 </plugin></plugins></build>",
                "org.apache.groovy",
            ),
        ] {
            let migration = dir.migrate(&format!(
                "<project><groupId>g</groupId><artifactId>a</artifactId><version>1</version>\
                 {build}</project>"
            ));
            assert!(migration.manifest.languages.is_empty(), "{build}");
            let skipped = migration.report.not_migrated.join("\n");
            assert!(skipped.contains(reason), "{skipped}");
            let text = migration.render_manifest();
            Manifest::parse(&text, &dir.path.join("jrs.toml"), &dir.path).unwrap();
        }
    }

    #[test]
    fn a_language_root_as_source_directory_leaves_the_java_root_alone() {
        let dir = Dir::new("kotlin-layout");
        let migration = dir.migrate(
            "<project><groupId>g</groupId><artifactId>a</artifactId><version>1</version>\
             <build><sourceDirectory>${basedir}/src/main/kotlin</sourceDirectory>\
             <testSourceDirectory>src/test/scala</testSourceDirectory>\
             <plugins><plugin><groupId>org.jetbrains.kotlin</groupId>\
             <artifactId>kotlin-maven-plugin</artifactId><version>2.2.0</version>\
             </plugin></plugins></build></project>",
        );
        let m = &migration.manifest;
        assert_eq!(m.source_dir, PathBuf::from("src/main/java"));
        assert_eq!(
            m.test_dir,
            PathBuf::from("src/test/scala"),
            "Scala is not on, so its root is just a directory"
        );
        let migrated = migration.report.migrated.join("\n");
        assert!(migrated.contains("[kotlin]'s own root"), "{migrated}");
    }
}
