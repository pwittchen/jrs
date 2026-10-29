//! The checks a manifest's values must pass beyond their types: names,
//! classes, packages, jar attributes, versions, and what only the whole
//! manifest can tell.

use std::path::Path;

use super::template::{Placeholder, Template};
use super::{Dependency, Managed, Manifest, RESERVED_TASK_NAMES, TaskDef};
use crate::compile::lang::{self, Language};
use crate::error::{JrsError, Result};

/// Whether `pattern` is something `groups` can hold: a group (`com.acme`), or
/// a group followed by `.*` for the groups under it. No other wildcard.
#[must_use]
pub fn valid_group_pattern(pattern: &str) -> bool {
    let base = pattern.strip_suffix(".*").unwrap_or(pattern);
    !base.is_empty()
        && !base.starts_with('.')
        && !base.ends_with('.')
        && !base.contains("..")
        && base
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

/// What the language tables mean for the rest of the manifest: the source
/// encoding kotlinc can read, the `javac-args` Groovy's joint `javac` can be
/// handed, and a declared runtime library at another version than its
/// compiler.
pub(super) fn check_languages(manifest: &Manifest) -> Result<Vec<String>> {
    let mut warnings = Vec::new();
    for config in &manifest.languages {
        let key = config.language.key();
        match config.language {
            Language::Kotlin => {
                let encoding = manifest
                    .java
                    .encoding
                    .to_ascii_uppercase()
                    .replace('_', "-");
                if encoding != "UTF-8" && encoding != "UTF8" {
                    warnings.push(format!(
                        "`java.encoding` is {}, but kotlinc reads sources as UTF-8 only; the \
                         Kotlin sources are read as UTF-8",
                        manifest.java.encoding
                    ));
                }
            }
            Language::Groovy => {
                lang::groovy_javac_args(&manifest.java.javac_args).map_err(|e| {
                    JrsError::manifest(format!(
                        "`java.javac-args`: {e}\n\n\
                         with [groovy] on, groovyc runs javac itself, and hands it these"
                    ))
                })?;
            }
            Language::Java | Language::Scala => {}
        }
        for (group, artifact) in config.language.runtime_libraries(&config.version) {
            if let Some(d) = manifest
                .dependencies
                .iter()
                .chain(&manifest.dev_dependencies)
                .find(|d| d.group == group && d.artifact == artifact && d.classifier.is_none())
                && d.version != config.version
            {
                warnings.push(format!(
                    "`{group}:{artifact}` is declared at {}, but `{key}.version` is {}; \
                     kotlinc and scalac reject a runtime library newer than themselves, so \
                     keep the two in step",
                    d.version, config.version
                ));
            }
        }
    }
    Ok(warnings)
}

/// A task's path-valued keys hold paths: no classpath, port or argfile.
pub(super) fn check_path_templates(task: &TaskDef, section: &str) -> Result<()> {
    for (key, t) in task
        .path_templates()
        .chain(task.cwd.iter().map(|t| ("cwd", t)))
    {
        if let Some(p) = t.placeholders().find(|p| p.is_classpath()) {
            return Err(JrsError::manifest(format!(
                "`{section}.{key}`: `{{{}}}` is a classpath, not a path; it cannot \
                 name a file or directory",
                p.name()
            )));
        }
        if t.has_free_port() {
            return Err(JrsError::manifest(format!(
                "`{section}.{key}`: `{{free-port.<name>}}` is a port, not a path; it \
                 belongs in `env` or `args`"
            )));
        }
        if t.placeholders().any(|p| p == Placeholder::SourcesArgfile) {
            return Err(JrsError::manifest(format!(
                "`{section}.{key}`: `{{sources-argfile}}` is written when the task runs, \
                 for its arguments; name the source directories here instead"
            )));
        }
    }
    Ok(())
}

/// `{jar}` and the argfiles have a value in a task only: `jrs run` and
/// `jrs test` package nothing, and an argfile is a task's own.
pub(super) fn refuse_task_placeholders(template: &Template, key: &str) -> Result<()> {
    for p in template.placeholders() {
        let why = match p {
            Placeholder::Jar => "`jrs run` and `jrs test` do not package, so there is no jar",
            Placeholder::ClasspathArgfile => {
                "the argfile is a task's own; use `{classpath}`, `{runtime-classpath}` or \
                 `{test-classpath}`"
            }
            Placeholder::SourcesArgfile => "the argfile is a task's own",
            _ => continue,
        };
        return Err(JrsError::manifest(format!(
            "`{key}`: `{{{}}}` has no value here: {why}",
            p.name()
        )));
    }
    Ok(())
}

pub(super) fn validate_task_name(name: &str) -> Result<()> {
    let well_formed = name.starts_with(|c: char| c.is_ascii_lowercase())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if !well_formed {
        return Err(JrsError::manifest(format!(
            "`tasks.{name}`: a task name is lowercase letters, digits and `-`, \
             starting with a letter"
        )));
    }
    if RESERVED_TASK_NAMES.contains(&name) {
        return Err(JrsError::manifest(format!(
            "`tasks.{name}`: `{name}` is a jrs command, so it cannot be a task name"
        )));
    }
    Ok(())
}

/// A classifier holds no braces but those of `{os-classifier}`, the one
/// placeholder there is: anything else would reach the repository as a
/// file name.
pub(super) fn check_classifier(dep: &Dependency, section: &str, key: &str) -> Result<()> {
    let Some(classifier) = &dep.classifier else {
        return Ok(());
    };
    if classifier
        .replace(crate::resolve::coord::OS_CLASSIFIER, "")
        .contains(['{', '}'])
    {
        return Err(JrsError::manifest(format!(
            "`{section}.\"{key}\"`: the classifier `{classifier}` holds a placeholder jrs does \
             not know; the one there is, `{}`, stands for the platform jrs runs on, as \
             `linux-x86_64` or `osx-aarch_64`",
            crate::resolve::coord::OS_CLASSIFIER
        )));
    }
    Ok(())
}

/// A dependency that leaves its version out takes it from `[managed]`. With a
/// BOM there, whether it covers the artifact is only known once the BOM is
/// read, at resolution; without one, the table has to name it.
pub(super) fn check_versionless(
    dependencies: &[Dependency],
    dev_dependencies: &[Dependency],
    managed: &[Managed],
) -> Result<()> {
    if managed.iter().any(|m| m.bom) {
        return Ok(());
    }
    let tables = dependencies
        .iter()
        .map(|d| ("dependencies", d))
        .chain(dev_dependencies.iter().map(|d| ("dev-dependencies", d)));
    for (section, d) in tables {
        let covered = managed
            .iter()
            .any(|m| m.group == d.group && m.artifact == d.artifact);
        if d.is_managed() && !covered {
            let what = if managed.is_empty() {
                "there is no [managed] table"
            } else {
                "[managed] does not name it"
            };
            return Err(JrsError::manifest(format!(
                "`{section}.\"{}\"` has no version, and {what}\n\n\
                 give it a version, or manage it:\n\n    [managed]\n    \"{}:{}\" = \"<version>\"",
                d.key(),
                d.group,
                d.artifact
            )));
        }
    }
    Ok(())
}

/// `compile-only` and `runtime-only` together would put the jar on both
/// classpaths, which is what a plain dependency already is.
pub(super) fn check_one_classpath(dep: &Dependency, section: &str) -> Result<()> {
    if dep.compile_only && dep.runtime_only {
        return Err(JrsError::manifest(format!(
            "`{section}.\"{}\"` is both `compile-only` and `runtime-only`, which is \
             what a plain dependency is; drop both",
            dep.key()
        )));
    }
    Ok(())
}

/// A dotted Java name, ASCII only: the names relocation rewrites are matched
/// byte for byte in class files, jar entries and registry files alike.
pub(super) fn validate_package_name(name: &str, key: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.split('.').all(|segment| {
            segment
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '$')
                && segment
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
        });
    if !ok {
        return Err(JrsError::manifest(format!(
            "`{key}`: `{name}` is not a package name: dot-separated Java identifiers, in ASCII"
        )));
    }
    Ok(())
}

/// A main-section attribute name, per the jar specification: an ASCII letter
/// or digit, then letters, digits, `-` and `_`, 70 bytes at most.
pub(super) fn validate_attribute_name(name: &str) -> std::result::Result<(), String> {
    let mut chars = name.chars();
    let first_ok = chars.next().is_some_and(|c| c.is_ascii_alphanumeric());
    if !first_ok || !chars.all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err(
            "an attribute name is an ASCII letter or digit followed by letters, digits, \
             `-` and `_`"
                .to_string(),
        );
    }
    if name.len() > 70 {
        return Err(format!(
            "an attribute name is at most 70 bytes (this one is {})",
            name.len()
        ));
    }
    Ok(())
}

pub(super) fn validate_name(name: &str) -> Result<()> {
    if name.trim().is_empty() {
        return Err(JrsError::manifest("`project.name` must not be empty"));
    }
    if name.contains(['/', '\\', ':', '\0']) || name == "." || name == ".." {
        return Err(JrsError::manifest(format!(
            "`project.name` must be a valid file name (got `{name}`); it is used \
             for the jar file"
        )));
    }
    Ok(())
}

/// `key` is how the message names the value: `project.main-class`.
pub(super) fn validate_class_name(class: &str, key: &str) -> Result<()> {
    let ok = !class.is_empty()
        && !class.starts_with('.')
        && !class.ends_with('.')
        && !class.contains("..")
        && class
            .chars()
            .all(|c| c.is_alphanumeric() || c == '.' || c == '_' || c == '$');
    if !ok {
        return Err(JrsError::manifest(format!(
            "`{key}` must be a fully-qualified class name (got `{class}`)"
        )));
    }
    Ok(())
}

/// Where jrs releases are published, for a project that needs a newer one.
pub const RELEASES_URL: &str = "https://github.com/pwittchen/jrs/releases";

/// Fail unless `running` is at least `required`, naming both and where newer
/// releases are. `required` has been validated; a `running` version this
/// cannot read (it is jrs's own) passes.
///
/// # Errors
///
/// [`JrsError::Manifest`] when `running` is older than `required`.
pub fn check_jrs_version(required: &str, running: &str, path: &Path) -> Result<()> {
    // A pre-release or build suffix on the running version does not count:
    // `0.9.0-dev` is taken as 0.9.0.
    let numeric = running.split(['-', '+']).next().unwrap_or(running);
    let (Some(needed), Some(have)) = (release_parts(required), release_parts(numeric)) else {
        return Ok(());
    };
    if have >= needed {
        return Ok(());
    }
    Err(JrsError::manifest(format!(
        "{} needs jrs {required} or newer (`project.jrs-version`), but this is jrs \
         {running}\n\n`jrs self update --to {}` installs it, or install a newer release \
         from {RELEASES_URL}",
        path.display(),
        needed.map(|p| p.to_string()).join(".")
    )))
}

/// `0.9` or `0.9.1` as `[0, 9, 0]` / `[0, 9, 1]`; anything else is `None`.
pub(super) fn release_parts(version: &str) -> Option<[u64; 3]> {
    let parts: Vec<&str> = version.split('.').collect();
    if !(2..=3).contains(&parts.len()) {
        return None;
    }
    let mut out = [0; 3];
    for (slot, part) in out.iter_mut().zip(&parts) {
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        *slot = part.parse().ok()?;
    }
    Some(out)
}
