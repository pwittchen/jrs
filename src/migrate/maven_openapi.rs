//! `openapi-generator-maven-plugin` → a `pre-compile` task over the `OpenAPI`
//! Generator's CLI, one per `<execution>`, which writes where the plugin
//! does and hands that tree to `javac` as the task's `source-outputs`
//! (SPEC §7.6). The generator's graph is the task's own; the libraries the
//! generated code needs are already the project's dependencies, as they are
//! in the Maven build. `build-helper-maven-plugin`'s `add-source` of that
//! tree is then covered too.

use super::Report;
use super::gradle_generated::{
    OPENAPI_CLI, OPENAPI_MAIN, OPENAPI_MAPS, OPENAPI_STRINGS, OPENAPI_SWITCHES,
};
use crate::manifest::{Action, Dependency, Hook, Manifest, TaskDef, Template};
use crate::resolve::pom::{self, Effective, Element, Pom};

/// The plugin's artifact.
pub(super) const PLUGIN: &str = "openapi-generator-maven-plugin";
/// The plugin that adds a generated tree as a source root.
pub(super) const BUILD_HELPER: &str = "build-helper-maven-plugin";

/// Configuration keys that name a file or directory, and the CLI's option.
/// `<output>` is read on its own: it has a default, and is the task's output.
const PATHS: &[(&str, &str)] = &[
    ("inputSpec", "-i"),
    ("templateDirectory", "-t"),
    ("configurationFile", "-c"),
    ("ignoreFileOverride", "--ignore-file-override"),
];

/// Keys the Maven plugin spells differently from the Gradle one.
const STRINGS: &[(&str, &str)] = &[
    ("artifactId", "--artifact-id"),
    ("artifactVersion", "--artifact-version"),
];

/// `<generateApis>` and its kin: which of the generator's outputs it writes,
/// as `--global-property` entries, and the key naming a subset of each.
const SELECTED: &[(&str, &str, &str)] = &[
    ("generateApis", "apis", "apisToGenerate"),
    ("generateModels", "models", "modelsToGenerate"),
    (
        "generateSupportingFiles",
        "supportingFiles",
        "supportingFilesToGenerate",
    ),
];

/// Switches that are on unless turned off, as `--global-property` entries.
const DOCS_AND_TESTS: &[(&str, &str)] = &[
    ("generateApiTests", "apiTests"),
    ("generateApiDocumentation", "apiDocs"),
    ("generateModelTests", "modelTests"),
    ("generateModelDocumentation", "modelDocs"),
];

/// What the plugin reads that changes nothing the CLI would do differently.
const IGNORED: &[&str] = &[
    "output",
    "addCompileSourceRoot",
    "addTestCompileSourceRoot",
    "skipIfSpecIsUnchanged",
    "cleanupOutput",
    "languageSpecificPrimitives",
];

/// Translate the plugin's executions, and `build-helper-maven-plugin`'s
/// `add-source` when what it adds is theirs. Returns the plugins handled.
pub(super) fn read(
    pom: &Pom,
    effective: &Effective,
    out: &mut Manifest,
    report: &mut Report,
) -> Vec<&'static str> {
    let Some(plugin) = pom.build.plugins.iter().find(|p| p.artifact == PLUGIN) else {
        return Vec::new();
    };
    let from = format!("plugin {PLUGIN}");
    let version = pom
        .root
        .path(&["build", "plugins"])
        .and_then(|p| {
            p.children_named("plugin")
                .find(|p| p.text_of("artifactId") == Some(PLUGIN))
        })
        .and_then(|p| p.text_of("version"))
        .map(|v| pom::interpolate(v.trim(), &effective.properties))
        .filter(|v| !v.contains("${"));
    let Some(version) = version else {
        report.skipped(format!(
            "{from} — its version could not be read, and the generator's CLI is pinned at it"
        ));
        return vec![PLUGIN];
    };
    let executions: Vec<&Element> = plugin
        .executions
        .iter()
        .filter(|e| {
            e.list("goals", "goal")
                .iter()
                .any(|g| g.text.trim() == "generate")
        })
        .collect();
    if executions.is_empty() {
        report.skipped(format!(
            "{from} — no <execution> runs its `generate` goal, so a build never runs it"
        ));
        return vec![PLUGIN];
    }
    let several = executions.len() > 1;
    let mut generated = Vec::new();
    for execution in executions {
        let id = execution.text_of("id").unwrap_or("default");
        let what = if id == "default" {
            from.clone()
        } else {
            format!("{from}, execution `{id}`")
        };
        let name = if several {
            match super::gradle_tasks::task_name(id) {
                Ok(n) => format!("openapi-{n}"),
                Err(why) => {
                    report.skipped(format!("{what} — {why}"));
                    continue;
                }
            }
        } else {
            "openapi".to_string()
        };
        if out.task(&name).is_some() {
            report.skipped(format!("{what} — a task named `{name}` is already there"));
            continue;
        }
        // The execution's own configuration wins over the plugin's, key by key.
        let configs: Vec<&Element> = execution
            .child("configuration")
            .into_iter()
            .chain(plugin.configuration.as_ref())
            .collect();
        let reading = Reading {
            configs: &configs,
            effective,
            target_dir: out.target_dir.to_string_lossy().replace('\\', "/"),
        };
        match reading.task(&name, &version, report) {
            Ok(Some((def, source))) => {
                report.migrated(format!(
                    "{what} → [tasks.{name}], run by hooks.pre-compile; its sources in \
                     {source} are compiled with the main ones"
                ));
                generated.push(source);
                out.hooks.add(Hook::PreCompile, &name);
                out.tasks.push(def);
            }
            Ok(None) => report.skipped(format!("{what} — it is skipped (`<skip>true</skip>`)")),
            Err(why) => report.skipped(format!("{what} — {why}; the task was not migrated")),
        }
    }
    let mut done = vec![PLUGIN];
    if read_build_helper(pom, &generated, report) {
        done.push(BUILD_HELPER);
    }
    done
}

/// One execution's configurations, read against the POM's properties.
struct Reading<'a> {
    configs: &'a [&'a Element],
    effective: &'a Effective,
    /// `project.target-dir`, as the manifest writes it.
    target_dir: String,
}

impl Reading<'_> {
    fn get(&self, key: &str) -> Option<&Element> {
        self.configs.iter().find_map(|c| c.child(key))
    }

    /// A value with the POM's properties in it; an error names one that
    /// cannot be read.
    fn value(&self, raw: &str) -> std::result::Result<String, String> {
        let value = pom::interpolate(raw.trim(), &self.effective.properties);
        if value.contains("${")
            && ![
                "${project.basedir}",
                "${basedir}",
                "${project.build.directory}",
            ]
            .iter()
            .any(|p| value.starts_with(p))
        {
            return Err(format!("`{raw}` names a property jrs cannot read"));
        }
        Ok(value)
    }

    fn text(&self, key: &str) -> std::result::Result<Option<String>, String> {
        match self
            .get(key)
            .map(|e| e.text.trim())
            .filter(|t| !t.is_empty())
        {
            Some(raw) => self.value(raw).map(Some),
            None => Ok(None),
        }
    }

    fn switch(&self, key: &str, default: bool) -> std::result::Result<bool, String> {
        Ok(self.text(key)?.map_or(default, |t| t == "true"))
    }

    /// A path as the project's: `{target}/…` under the build directory,
    /// relative to the root otherwise, where the task runs. The text of a
    /// template.
    fn path(&self, raw: &str) -> std::result::Result<String, String> {
        let value = self.value(raw)?.replace('\\', "/");
        let escaped = |s: &str| Template::literal(s).raw;
        for prefix in ["${project.basedir}/", "${basedir}/"] {
            if let Some(rest) = value.strip_prefix(prefix) {
                return self.path(rest);
            }
        }
        if let Some(rest) = value.strip_prefix("${project.build.directory}") {
            return Ok(format!("{{target}}{}", escaped(rest)));
        }
        let relative = value.trim_start_matches("./");
        if relative.contains("${") || relative.starts_with('/') || relative.contains(':') {
            return Err(format!("`{raw}` is not a path inside the project"));
        }
        let target = self.target_dir.trim_end_matches('/');
        if relative == target {
            return Ok("{target}".to_string());
        }
        if let Some(rest) = relative.strip_prefix(&format!("{target}/")) {
            return Ok(format!("{{target}}/{}", escaped(rest)));
        }
        Ok(escaped(relative))
    }

    /// `k=v` entries of a map-valued key: `<configOptions><k>v</k>`,
    /// `<importMappings><importMapping>k=v</importMapping>`, or `k=v,k=v`.
    fn map(&self, key: &str) -> std::result::Result<Vec<String>, String> {
        let Some(element) = self.get(key) else {
            return Ok(Vec::new());
        };
        if element.children.is_empty() {
            return element
                .text
                .split(',')
                .map(str::trim)
                .filter(|e| !e.is_empty())
                .map(|e| self.value(e))
                .collect();
        }
        let singular = key.strip_suffix('s').unwrap_or(key);
        element
            .children
            .iter()
            .map(|c| {
                let text = self.value(&c.text)?;
                if c.name == singular && text.contains('=') {
                    Ok(text)
                } else {
                    Ok(format!("{}={text}", c.name))
                }
            })
            .collect()
    }

    /// `<languageSpecificPrimitives>`, a list or `a,b`.
    fn list(&self, key: &str) -> std::result::Result<Vec<String>, String> {
        let Some(element) = self.get(key) else {
            return Ok(Vec::new());
        };
        let items: Vec<&str> = if element.children.is_empty() {
            element.text.split(',').collect()
        } else {
            element.children.iter().map(|c| c.text.as_str()).collect()
        };
        items
            .into_iter()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(|t| self.value(t))
            .collect()
    }

    /// `--global-property` entries for what the plugin's switches select,
    /// none when it writes everything, as the CLI does by default.
    fn global_properties(&self) -> std::result::Result<Vec<String>, String> {
        let mut selected = Vec::new();
        let mut narrowed = false;
        for (switch, property, subset) in SELECTED {
            let on = self.switch(switch, true)?;
            let only = self.text(subset)?;
            narrowed |= !on || only.is_some();
            if on {
                selected.push(match only {
                    Some(names) => format!("{property}={}", names.replace(',', ":")),
                    None => (*property).to_string(),
                });
            }
        }
        if narrowed && selected.is_empty() {
            return Err("it generates neither APIs, models nor supporting files".to_string());
        }
        let mut entries = if narrowed { selected } else { Vec::new() };
        for (switch, property) in DOCS_AND_TESTS {
            if !self.switch(switch, true)? {
                entries.push(format!("{property}=false"));
            }
        }
        Ok(entries)
    }

    /// Every other key, as the CLI's options appended to `args`; one it
    /// does not know is reported. The generator's source folder, when the
    /// configuration names one.
    fn options(
        &self,
        name: &str,
        args: &mut Vec<Template>,
        report: &mut Report,
    ) -> std::result::Result<Option<String>, String> {
        let mut maps: Vec<(&str, Vec<String>)> = Vec::new();
        let mut merge = |option: &'static str, entries: Vec<String>| {
            if entries.is_empty() {
                return;
            }
            match maps.iter_mut().find(|(o, _)| *o == option) {
                Some((_, list)) => list.extend(entries),
                None => maps.push((option, entries)),
            }
        };
        merge("--global-property", self.global_properties()?);
        let mut source_folder = None;
        let known = |key: &str| {
            PATHS.iter().any(|(k, _)| *k == key)
                || STRINGS.iter().any(|(k, _)| *k == key)
                || OPENAPI_STRINGS.iter().any(|(k, _)| *k == key)
                || OPENAPI_MAPS.iter().any(|(k, _)| *k == key)
                || OPENAPI_SWITCHES.iter().any(|(k, _)| *k == key)
                || SELECTED.iter().any(|(k, _, s)| *k == key || *s == key)
                || DOCS_AND_TESTS.iter().any(|(k, _)| *k == key)
                || IGNORED.contains(&key)
                || key == "skip"
        };
        let mut keys: Vec<&str> = Vec::new();
        for config in self.configs {
            for child in &config.children {
                if !keys.contains(&child.name.as_str()) {
                    keys.push(&child.name);
                }
            }
        }
        for key in keys {
            if !known(key) {
                report.review(format!(
                    "{PLUGIN} <{key}> — not translated to the generator's CLI; add its \
                     option to [tasks.{name}] args by hand"
                ));
                continue;
            }
            let option = STRINGS
                .iter()
                .chain(
                    OPENAPI_STRINGS
                        .iter()
                        .filter(|(k, _)| !matches!(*k, "id" | "version")),
                )
                .find(|(k, _)| *k == key);
            if let Some((_, option)) = option {
                if let Some(text) = self.text(key)? {
                    args.push(Template::literal(option));
                    args.push(Template::literal(&text));
                }
            } else if let Some((_, option)) = OPENAPI_MAPS.iter().find(|(k, _)| *k == key) {
                let entries = self.map(key)?;
                if key == "configOptions" || key == "additionalProperties" {
                    source_folder = source_folder.or_else(|| {
                        entries
                            .iter()
                            .find_map(|e| e.strip_prefix("sourceFolder="))
                            .map(str::to_string)
                    });
                }
                merge(option, entries);
            } else if let Some((_, option)) = OPENAPI_SWITCHES.iter().find(|(k, _)| *k == key)
                && self.switch(key, false)?
            {
                args.push(Template::literal(option));
            }
        }
        let primitives = self.list("languageSpecificPrimitives")?;
        if !primitives.is_empty() {
            args.push(Template::literal("--language-specific-primitives"));
            args.push(Template::literal(&primitives.join(",")));
        }
        for (option, entries) in maps {
            args.push(Template::literal(option));
            args.push(Template::literal(&entries.join(",")));
        }
        Ok(source_folder)
    }

    /// The task, and its source root as the report shows it; `None` when the
    /// execution is skipped.
    fn task(
        &self,
        name: &str,
        version: &str,
        report: &mut Report,
    ) -> std::result::Result<Option<(TaskDef, String)>, String> {
        if self.switch("skip", false)? {
            return Ok(None);
        }
        let template = |raw: &str| Template::parse(raw).map_err(|e| format!("`{raw}`: {e}"));
        let spec = self.text("inputSpec")?.ok_or("it names no <inputSpec>")?;
        let mut args = vec![Template::literal("generate")];
        let mut inputs = Vec::new();
        for (key, option) in PATHS {
            let Some(raw) = self.text(key)? else {
                continue;
            };
            // A spec read from a URL is no file of the project's.
            let path = if *key == "inputSpec" && raw.contains("://") {
                Template::literal(&raw).raw
            } else {
                let path = self.path(&raw)?;
                if *key != "ignoreFileOverride" {
                    inputs.push(template(&path)?);
                }
                path
            };
            args.push(Template::literal(option));
            args.push(template(&path)?);
        }
        // The plugin's default, which the CLI's (the working directory)
        // is not.
        let output = match self.text("output")? {
            Some(raw) => self.path(&raw)?,
            None => "{target}/generated-sources/openapi".to_string(),
        };
        if !output.starts_with("{target}") {
            return Err(format!(
                "its <output> `{output}` is outside the build directory, and a task's \
                 generated sources must be inside it"
            ));
        }
        args.push(Template::literal("-o"));
        args.push(template(&output)?);

        let source_folder = self.options(name, &mut args, report)?;

        // Where the plugin adds a compile source root: the generator's source
        // folder under the output, `src/main/java` for the Java generators.
        let mut source_outputs = Vec::new();
        let mut source = String::new();
        if self.switch("addCompileSourceRoot", true)? {
            let folder = source_folder.unwrap_or_else(|| "src/main/java".to_string());
            source = format!(
                "{output}/{}",
                Template::literal(folder.trim_matches('/')).raw
            );
            source_outputs.push(template(&source)?);
        }
        let shown_spec = spec
            .trim_start_matches("${project.basedir}/")
            .trim_start_matches("${basedir}/");
        let def = TaskDef {
            name: name.to_string(),
            description: Some(format!(
                "Generate sources from {shown_spec} (OpenAPI Generator {version})"
            )),
            action: Some(Action::Main(OPENAPI_MAIN.to_string())),
            args,
            depends_on: Vec::new(),
            env: Vec::new(),
            cwd: None,
            inputs,
            outputs: vec![template(&output)?],
            source_outputs,
            resource_outputs: Vec::new(),
            cache: false,
            dependencies: vec![Dependency::new(OPENAPI_CLI.0, OPENAPI_CLI.1, version)],
        };
        Ok(Some((def, source)))
    }
}

/// `build-helper-maven-plugin`, when every directory its `add-source`
/// executions add is a tree an `OpenAPI` task generates: reported as
/// migrated through those tasks. Whether it was.
fn read_build_helper(pom: &Pom, generated: &[String], report: &mut Report) -> bool {
    let Some(helper) = pom
        .build
        .plugins
        .iter()
        .find(|p| p.artifact == BUILD_HELPER)
    else {
        return false;
    };
    let mut sources = Vec::new();
    for execution in &helper.executions {
        let goals = execution.list("goals", "goal");
        if goals.iter().any(|g| g.text.trim() != "add-source") {
            return false;
        }
        for source in execution
            .child("configuration")
            .map(|c| c.list("sources", "source"))
            .unwrap_or_default()
        {
            sources.push(source.text.trim().to_string());
        }
    }
    let as_generated = |source: &str| {
        let source = source
            .replace('\\', "/")
            .replace("${project.build.directory}", "target")
            .replace("${project.basedir}/", "")
            .replace("${basedir}/", "");
        let source = source.trim_end_matches('/');
        let source = source.strip_prefix("target").unwrap_or(source);
        generated
            .iter()
            .any(|g| g.strip_prefix("{target}") == Some(source))
    };
    if sources.is_empty() || !sources.iter().all(|s| as_generated(s)) {
        return false;
    }
    report.migrated(format!(
        "plugin {BUILD_HELPER} — its add-source {} is the OpenAPI task's source-outputs",
        sources.join(", ")
    ));
    true
}
