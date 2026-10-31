//! Writing a `Manifest` back out as `jrs.toml`, the inverse of parsing:
//! what `jrs init`, `jrs add`, `jrs remove` and `jrs migrate` write.

use std::fmt::Write as _;
use std::path::Path;

use super::parse::default_test_resource_dir;
use super::template::Template;
use super::{
    Action, CENTRAL_URL, Dependency, JavaAgent, Manifest, ModuleDependency, Repository,
    ResourcesConfig, RunConfig, ShareClasses, TaskDef, TestConfig,
};

impl Manifest {
    /// Render this manifest back to TOML, optionally with a comment header.
    ///
    /// Used by `jrs init` and `jrs migrate`; the output is deliberately
    /// hand-formatted, since a generated manifest is something a human reads.
    #[must_use]
    #[allow(
        clippy::too_many_lines,
        reason = "one short block per table, in the order jrs.toml lists them"
    )]
    pub fn render(&self, header: Option<&str>) -> String {
        let mut s = String::new();
        if let Some(h) = header {
            for line in h.lines() {
                let _ = writeln!(s, "# {line}");
            }
            s.push('\n');
        }
        let _ = writeln!(s, "[project]");
        let _ = writeln!(s, "name = {}", quote(&self.name));
        let _ = writeln!(s, "version = {}", quote(&self.version));
        if let Some(group) = &self.group {
            let _ = writeln!(s, "group = {}", quote(group));
        }
        if let Some(mc) = &self.main_class {
            let _ = writeln!(s, "main-class = {}", quote(mc));
        }
        for (key, value, default) in [
            ("source-dir", &self.source_dir, "src/main/java"),
            ("test-dir", &self.test_dir, "src/test/java"),
            ("resource-dir", &self.resource_dir, "src/main/resources"),
            ("target-dir", &self.target_dir, "target"),
        ] {
            let value = to_slash(value);
            if value != default {
                let _ = writeln!(s, "{key} = {}", quote(&value));
            }
        }
        let test_resources = to_slash(&self.test_resource_dir);
        if test_resources != to_slash(&default_test_resource_dir(&self.test_dir)) {
            let _ = writeln!(s, "test-resource-dir = {}", quote(&test_resources));
        }
        if let Some(v) = &self.jrs_version {
            let _ = writeln!(s, "jrs-version = {}", quote(v));
        }

        let java = &self.java;
        if java.source.is_some()
            || java.target.is_some()
            || java.encoding != "UTF-8"
            || !java.javac_args.is_empty()
            || !java.javadoc_args.is_empty()
            || java.jdk.is_some()
        {
            let _ = writeln!(s, "\n[java]");
            if let Some(v) = java.source {
                let _ = writeln!(s, "source = {v}");
            }
            if let Some(v) = java.target
                && Some(v) != java.source
            {
                let _ = writeln!(s, "target = {v}");
            }
            if java.encoding != "UTF-8" {
                let _ = writeln!(s, "encoding = {}", quote(&java.encoding));
            }
            if !java.javac_args.is_empty() {
                let _ = writeln!(s, "javac-args = {}", quote_list(&java.javac_args));
            }
            if !java.javadoc_args.is_empty() {
                let _ = writeln!(s, "javadoc-args = {}", quote_list(&java.javadoc_args));
            }
            if let Some(v) = java.jdk {
                let _ = writeln!(s, "jdk = {v}");
            }
        }
        for config in &self.languages {
            let key = config.language.key();
            let _ = writeln!(s, "\n[{key}]");
            let _ = writeln!(s, "version = {}", quote(&config.version));
            for (name, value, default) in [
                ("source-dir", &config.source_dir, format!("src/main/{key}")),
                ("test-dir", &config.test_dir, format!("src/test/{key}")),
            ] {
                let value = to_slash(value);
                if value != default {
                    let _ = writeln!(s, "{name} = {}", quote(&value));
                }
            }
            if !config.compiler_args.is_empty() {
                let _ = writeln!(
                    s,
                    "{} = {}",
                    config.language.args_key(),
                    quote_list(&config.compiler_args)
                );
            }
            if !config.compiler_jvm_args.is_empty() {
                let _ = writeln!(
                    s,
                    "compiler-jvm-args = {}",
                    quote_list(&config.compiler_jvm_args)
                );
            }
            if !config.plugins.is_empty() {
                let _ = writeln!(s, "plugins = {}", quote_list(&config.plugins));
            }
        }
        if self.run != RunConfig::default() {
            let _ = writeln!(s, "\n[run]");
            if !self.run.jvm_args.is_empty() {
                let _ = writeln!(s, "jvm-args = {}", quote_list(&self.run.jvm_args));
            }
            render_jvm_extras(&mut s, &self.run.java_agents, &self.run.env);
            if let Some(cwd) = &self.run.cwd {
                let _ = writeln!(s, "cwd = {}", quote(&cwd.raw));
            }
        }
        if self.test != TestConfig::default() {
            let _ = writeln!(s, "\n[test]");
            if !self.test.jvm_args.is_empty() {
                let _ = writeln!(s, "jvm-args = {}", quote_list(&self.test.jvm_args));
            }
            if let Some(v) = &self.test.jacoco_version {
                let _ = writeln!(s, "jacoco-version = {}", quote(v));
            }
            render_jvm_extras(&mut s, &self.test.java_agents, &self.test.env);
            if self.test.retries > 0 {
                let _ = writeln!(s, "retries = {}", self.test.retries);
            }
            if self.test.forks > 0 {
                let _ = writeln!(s, "forks = {}", self.test.forks);
            }
            if !self.test.coverage_minimum.is_empty() {
                let entries: Vec<String> = self
                    .test
                    .coverage_minimum
                    .iter()
                    .map(|m| format!("{} = {}", m.counter.key(), m.ratio()))
                    .collect();
                let _ = writeln!(s, "coverage-minimum = {{ {} }}", entries.join(", "));
            }
            match self.test.share_classes {
                ShareClasses::Off => {}
                ShareClasses::On => {
                    let _ = writeln!(s, "share-classes = true");
                }
                ShareClasses::Archive => {
                    let _ = writeln!(s, "share-classes = \"archive\"");
                }
                ShareClasses::Aot => {
                    let _ = writeln!(s, "share-classes = \"aot\"");
                }
            }
        }
        for suite in &self.test.suites {
            let _ = writeln!(s, "\n[test.suites.{}]", suite.name);
            let _ = writeln!(s, "test-dir = {}", quote(&to_slash(&suite.test_dir)));
            let resources = to_slash(&suite.test_resource_dir);
            if resources != to_slash(&default_test_resource_dir(&suite.test_dir)) {
                let _ = writeln!(s, "test-resource-dir = {}", quote(&resources));
            }
            if !suite.jvm_args.is_empty() {
                let _ = writeln!(s, "jvm-args = {}", quote_list(&suite.jvm_args));
            }
            if !suite.env.is_empty() {
                let _ = writeln!(s, "env = {}", inline_env(&suite.env));
            }
            if suite.retries > 0 {
                let _ = writeln!(s, "retries = {}", suite.retries);
            }
            if suite.forks > 0 {
                let _ = writeln!(s, "forks = {}", suite.forks);
            }
        }
        if !self.package.add_modules.is_empty() || !self.package.native_image_args.is_empty() {
            let _ = writeln!(s, "\n[package]");
            if !self.package.add_modules.is_empty() {
                let _ = writeln!(s, "add-modules = {}", quote_list(&self.package.add_modules));
            }
            if !self.package.native_image_args.is_empty() {
                let _ = writeln!(
                    s,
                    "native-image-args = {}",
                    quote_list(&self.package.native_image_args)
                );
            }
        }
        if !self.package.manifest.is_empty() {
            // Attribute names are validated to bare-key characters on the way
            // in, so they need no quoting.
            let _ = writeln!(s, "\n[package.manifest]");
            for (name, value) in &self.package.manifest {
                let _ = writeln!(s, "{name} = {}", quote(&value.raw));
            }
        }
        if !self.package.relocate.is_empty() {
            let _ = writeln!(s, "\n[package.relocate]");
            for r in &self.package.relocate {
                if r.exclude.is_empty() {
                    let _ = writeln!(s, "{} = {}", quote(&r.from), quote(&r.to));
                } else {
                    let _ = writeln!(
                        s,
                        "{} = {{ to = {}, exclude = {} }}",
                        quote(&r.from),
                        quote(&r.to),
                        quote_list(&r.exclude)
                    );
                }
            }
        }
        if self.resources != ResourcesConfig::default() {
            let _ = writeln!(s, "\n[resources]");
            if !self.resources.expand.is_empty() {
                let _ = writeln!(s, "expand = {}", quote_list(&self.resources.expand));
            }
            if !self.resources.properties.is_empty() {
                let _ = writeln!(s, "\n[resources.properties]");
                for (name, value) in &self.resources.properties {
                    // A dotted name is quoted, or TOML would read a nested table.
                    let bare = name
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
                    let key = if bare { name.clone() } else { quote(name) };
                    let _ = writeln!(s, "{key} = {}", quote(&value.raw));
                }
            }
        }
        if let Some(obf) = &self.obfuscate {
            let _ = writeln!(s, "\n[obfuscate]");
            let _ = writeln!(s, "version = {}", quote(&obf.version));
            if !obf.keep.is_empty() {
                let _ = writeln!(s, "keep = {}", quote_list(&obf.keep));
            }
            if !obf.proguard_args.is_empty() {
                let _ = writeln!(s, "proguard-args = {}", quote_list(&obf.proguard_args));
            }
        }

        if !self.managed.is_empty() {
            let _ = writeln!(s, "\n[managed]");
            for m in &self.managed {
                let value = if m.bom {
                    format!("{{ version = {}, bom = true }}", quote(&m.version))
                } else {
                    quote(&m.version)
                };
                let _ = writeln!(s, "{} = {value}", quote(&m.key()));
            }
        }
        let modules = |dev: bool| self.modules.iter().filter(move |m| m.dev == dev);
        if !self.dependencies.is_empty() || modules(false).next().is_some() {
            let _ = writeln!(s, "\n[dependencies]");
            for m in modules(false) {
                let _ = writeln!(s, "{}", render_module(m));
            }
            for d in &self.dependencies {
                let _ = writeln!(s, "{}", render_dependency(d));
            }
        }
        if !self.dev_dependencies.is_empty() || modules(true).next().is_some() {
            let _ = writeln!(s, "\n[dev-dependencies]");
            for m in modules(true) {
                let _ = writeln!(s, "{}", render_module(m));
            }
            for d in &self.dev_dependencies {
                let _ = writeln!(s, "{}", render_dependency(d));
            }
        }
        let extra: Vec<&Repository> = self
            .repositories
            .iter()
            .filter(|r| r.url.trim_end_matches('/') != CENTRAL_URL || !r.groups.is_empty())
            .collect();
        if !extra.is_empty() {
            let _ = writeln!(s, "\n[repositories]");
            for r in extra {
                if r.groups.is_empty() {
                    let _ = writeln!(s, "{} = {}", quote(&r.name), quote(&r.url));
                } else {
                    let _ = writeln!(
                        s,
                        "{} = {{ url = {}, groups = {} }}",
                        quote(&r.name),
                        quote(&r.url),
                        quote_list(&r.groups)
                    );
                }
            }
        }
        for task in &self.tasks {
            render_task(&mut s, task);
        }
        if !self.hooks.is_empty() {
            let _ = writeln!(s, "\n[hooks]");
            for (hook, names) in self.hooks.iter() {
                let _ = writeln!(s, "{hook} = {}", quote_list(names));
            }
        }
        s
    }
}

/// One `[tasks.<name>]` table, its keys in the order TASKS.md §4.1 lists them.
fn render_task(s: &mut String, task: &TaskDef) {
    let _ = writeln!(s, "\n[tasks.{}]", task.name);
    if let Some(d) = &task.description {
        let _ = writeln!(s, "description = {}", quote(d));
    }
    match &task.action {
        Some(Action::Run(argv)) => {
            let _ = writeln!(s, "run = {}", quote_templates(argv));
        }
        Some(Action::Shell(script)) => {
            let _ = writeln!(s, "shell = {}", quote(script));
        }
        Some(Action::Script(file)) => {
            let _ = writeln!(s, "script = {}", quote(&file.raw));
        }
        Some(Action::Main(class)) => {
            let _ = writeln!(s, "main = {}", quote(class));
        }
        None => {}
    }
    if !task.args.is_empty() {
        let _ = writeln!(s, "args = {}", quote_templates(&task.args));
    }
    if !task.depends_on.is_empty() {
        let names: Vec<String> = task.depends_on.iter().map(ToString::to_string).collect();
        let _ = writeln!(s, "depends-on = {}", quote_list(&names));
    }
    if !task.env.is_empty() {
        let _ = writeln!(s, "env = {}", inline_env(&task.env));
    }
    if let Some(cwd) = &task.cwd {
        let _ = writeln!(s, "cwd = {}", quote(&cwd.raw));
    }
    for (key, list) in [
        ("inputs", &task.inputs),
        ("outputs", &task.outputs),
        ("source-outputs", &task.source_outputs),
        ("resource-outputs", &task.resource_outputs),
    ] {
        if !list.is_empty() {
            let _ = writeln!(s, "{key} = {}", quote_templates(list));
        }
    }
    if task.cache {
        s.push_str("cache = true\n");
    }
    if !task.dependencies.is_empty() {
        let _ = writeln!(s, "\n[tasks.{}.dependencies]", task.name);
        for d in &task.dependencies {
            let _ = writeln!(s, "{}", render_dependency(d));
        }
    }
}

/// An `env` table as one inline TOML table, in declaration order.
fn inline_env(env: &[(String, Template)]) -> String {
    let vars: Vec<String> = env
        .iter()
        .map(|(k, v)| format!("{} = {}", quote(k), quote(&v.raw)))
        .collect();
    format!("{{ {} }}", vars.join(", "))
}

/// The `java-agents` and `env` lines `[run]` and `[test]` share.
fn render_jvm_extras(s: &mut String, agents: &[JavaAgent], env: &[(String, Template)]) {
    if !agents.is_empty() {
        let names: Vec<String> = agents.iter().map(ToString::to_string).collect();
        let _ = writeln!(s, "java-agents = {}", quote_list(&names));
    }
    if !env.is_empty() {
        let _ = writeln!(s, "env = {}", inline_env(env));
    }
}

fn quote_templates(items: &[Template]) -> String {
    let raw: Vec<String> = items.iter().map(|t| t.raw.clone()).collect();
    quote_list(&raw)
}

/// One `[dependencies]` line for a workspace member.
fn render_module(m: &ModuleDependency) -> String {
    let mut fields = vec![format!("path = {}", quote(&m.path))];
    if m.compile_only {
        fields.push("compile-only = true".to_string());
    }
    if m.runtime_only {
        fields.push("runtime-only = true".to_string());
    }
    format!("{} = {{ {} }}", m.name, fields.join(", "))
}

/// One `[dependencies]` line: the short form when it says everything, the
/// inline table otherwise.
fn render_dependency(d: &Dependency) -> String {
    let (key, value) = dependency_entry(d);
    format!("{} = {value}", quote(&key))
}

/// A dependency as a table entry: its key, unquoted, and its value as TOML.
/// `jrs add` writes exactly this, so an added line reads like a generated one.
#[must_use]
pub fn dependency_entry(d: &Dependency) -> (String, String) {
    if let Some(path) = &d.path {
        let mut fields = vec![format!("path = {}", quote(path))];
        if d.compile_only {
            fields.push("compile-only = true".to_string());
        }
        if d.runtime_only {
            fields.push("runtime-only = true".to_string());
        }
        return (d.artifact.clone(), format!("{{ {} }}", fields.join(", ")));
    }
    // The classifier goes in the key when it can, so two classifiers of one
    // artifact stay two distinct keys.
    let key_classifier = d
        .classifier
        .as_ref()
        .filter(|c| !c.starts_with(|ch: char| ch.is_ascii_digit()));
    let key = match key_classifier {
        Some(c) => format!("{}:{}:{c}", d.group, d.artifact),
        None => format!("{}:{}", d.group, d.artifact),
    };
    let table_classifier = d.classifier.as_ref().filter(|_| key_classifier.is_none());
    if d.is_plain() && table_classifier.is_none() && !d.is_managed() {
        return (key, quote(&d.version));
    }
    // A managed dependency has no `version` field; `{}` alone says so.
    let mut fields = Vec::new();
    if !d.is_managed() {
        fields.push(format!("version = {}", quote(&d.version)));
    }
    if let Some(c) = table_classifier {
        fields.push(format!("classifier = {}", quote(c)));
    }
    if !d.exclusions.is_empty() {
        let patterns: Vec<String> = d.exclusions.iter().map(ToString::to_string).collect();
        fields.push(format!("exclusions = {}", quote_list(&patterns)));
    }
    if d.compile_only {
        fields.push("compile-only = true".to_string());
    }
    if d.runtime_only {
        fields.push("runtime-only = true".to_string());
    }
    if fields.is_empty() {
        return (key, "{}".to_string());
    }
    (key, format!("{{ {} }}", fields.join(", ")))
}

fn quote_list(items: &[String]) -> String {
    let quoted: Vec<String> = items.iter().map(|a| quote(a)).collect();
    format!("[{}]", quoted.join(", "))
}

pub(super) fn quote(s: &str) -> String {
    let escaped = s
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n");
    format!("\"{escaped}\"")
}

fn to_slash(p: &Path) -> String {
    p.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}
