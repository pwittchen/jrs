//! Generated code in a Gradle build: where it goes, what makes it, and the
//! Gradle tasks jrs hands back to Gradle.
//!
//! A build that generates code says so in three places: a task that writes it
//! (a plugin's, such as the `OpenAPI` Generator's `openApiGenerate`, or one of
//! the build's own), a `dependsOn` from the compile task on that task, and a
//! `sourceSets { main { kotlin { srcDir(...) } } }` entry that compiles what it
//! wrote. The first becomes a `pre-compile` task, the second its hook, and the
//! third its `source-outputs` or `resource-outputs`.
//!
//! A plugin task jrs knows is translated into a `main` task over the plugin's
//! own command-line tool, from Maven Central. A task of the build's own that
//! is Gradle code (`doLast { }`) cannot be translated at all; when the build
//! has the Gradle wrapper, the tasks the compile needs are run by it instead,
//! all in one `./gradlew` call, so the build still works while they wait to be
//! rewritten.

use super::Report;
use super::gradle_tasks::{Stmt, bracketed, literal, split_top, statements, unquoted};
use crate::manifest::{Action, Dependency, Hook, Manifest, TaskDef, Template};

/// How deep a path may refer to another variable before jrs gives up.
const MAX_DEPTH: usize = 16;

// ---- paths -----------------------------------------------------------------

/// The `val`s a script assigns, as written, for working out the paths built
/// from them: the innermost scope first.
#[derive(Debug, Default, Clone)]
pub(super) struct Paths {
    vals: Vec<(String, String)>,
}

impl Paths {
    /// The `val name = expression` statements at the top level of `text`.
    pub(super) fn read(text: &str) -> Paths {
        let mut vals = Vec::new();
        for statement in joined_statements(text) {
            let Some(rest) = statement.strip_prefix("val ") else {
                continue;
            };
            let Some((name, value)) = rest.split_once('=') else {
                continue;
            };
            let (name, value) = (name.trim(), value.trim());
            // `val x: Provider<Directory> = ...`
            let name = name.split(':').next().unwrap_or(name).trim();
            if !name.is_empty() && !value.is_empty() {
                vals.push((name.to_string(), value.to_string()));
            }
        }
        Paths { vals }
    }

    /// These, with the `val`s of `block` in front: a task's own `outputDir`
    /// is not another task's.
    pub(super) fn within(&self, block: &str) -> Paths {
        let mut inner = Paths::read(block);
        inner.vals.extend(self.vals.iter().cloned());
        inner
    }

    /// The path `expression` names, relative to the project root, `/`
    /// separated: `build/generated/x` for `layout.buildDirectory.dir(
    /// "generated/x")`, `.` for the project directory. `None` for what jrs
    /// cannot work out without running Gradle.
    pub(super) fn path(&self, expression: &str) -> Option<String> {
        self.path_at(expression, 0)
    }

    fn path_at(&self, expression: &str, depth: usize) -> Option<String> {
        if depth > MAX_DEPTH {
            return None;
        }
        let e = compact(expression);
        let e = e.as_str();
        for suffix in [
            ".get()",
            ".asFile",
            ".absolutePath",
            ".path",
            ".canonicalPath",
        ] {
            if let Some(inner) = e.strip_suffix(suffix) {
                return self.path_at(inner, depth + 1);
            }
        }
        // `provider.map { it.dir("x") }`, `.map { it.asFile.absolutePath }`
        if let Some((inner, lambda)) = e.rsplit_once(".map{")
            && let Some(body) = lambda.strip_suffix('}')
        {
            let base = self.path_at(inner, depth + 1)?;
            let body = body.strip_prefix("it").unwrap_or(body);
            if body.is_empty() {
                return Some(base);
            }
            return self.path_at(&format!("{base:?}{body}"), depth + 1);
        }
        if let Some(open) = call_open(e) {
            let (callee, argument) = (&e[..open], &e[open + 1..e.len() - 1]);
            let relative = || text_path(argument);
            if let Some(receiver) = callee
                .strip_suffix(".dir")
                .or_else(|| callee.strip_suffix(".file"))
            {
                if matches!(receiver, "project" | "rootProject") {
                    return relative();
                }
                return Some(join(&self.path_at(receiver, depth + 1)?, &relative()?));
            }
            if matches!(callee, "file" | "project.file" | "rootProject.file") {
                return relative();
            }
            return None;
        }
        match e {
            "layout.buildDirectory"
            | "project.layout.buildDirectory"
            | "buildDir"
            | "project.buildDir" => return Some("build".to_string()),
            "layout.projectDirectory"
            | "project.layout.projectDirectory"
            | "projectDir"
            | "rootDir"
            | "project.projectDir"
            | "project.rootDir"
            | "rootProject.projectDir"
            | "rootProject.rootDir" => return Some(".".to_string()),
            _ => {}
        }
        if e.starts_with(['"', '\'']) {
            return text_path(e);
        }
        let (_, value) = self.vals.iter().find(|(name, _)| name == e)?;
        self.path_at(value, depth + 1)
    }
}

/// `expression` without the whitespace outside its string literals.
fn compact(expression: &str) -> String {
    let kept: Vec<usize> = unquoted(expression)
        .into_iter()
        .filter(|(_, c)| c.is_whitespace())
        .map(|(i, _)| i)
        .collect();
    expression
        .char_indices()
        .filter(|(i, _)| !kept.contains(i))
        .map(|(_, c)| c)
        .collect()
}

/// Where the argument list of a call that ends `e` opens, when `e` ends with
/// one.
fn call_open(e: &str) -> Option<usize> {
    if !e.ends_with(')') {
        return None;
    }
    let mut depth = 0i64;
    for (i, c) in unquoted(e).into_iter().rev() {
        match c {
            ')' => depth += 1,
            '(' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// A string literal as a path under the project: plain, or starting from
/// `$rootDir`, `${projectDir}` and the like. An absolute path, or one built
/// from anything else, is not one.
fn text_path(argument: &str) -> Option<String> {
    let l = literal(argument)?;
    let mut text = l.text;
    if l.interpolated {
        let root = [
            "${rootDir}",
            "${projectDir}",
            "${project.rootDir}",
            "${project.projectDir}",
            "${rootProject.rootDir}",
            "${rootProject.projectDir}",
            "$rootDir",
            "$projectDir",
        ]
        .into_iter()
        .find_map(|prefix| text.strip_prefix(prefix))?;
        if root.contains('$') {
            return None;
        }
        text = root.trim_start_matches('/').to_string();
        if text.is_empty() {
            text = ".".to_string();
        }
    }
    let path = std::path::Path::new(&text);
    if path.is_absolute() || text.contains('\\') {
        return None;
    }
    Some(join(".", &text))
}

/// `base/relative`, tidied: no `./`, no trailing `/`.
fn join(base: &str, relative: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in base.split('/').chain(relative.split('/')) {
        match part {
            "" | "." => {}
            _ => parts.push(part),
        }
    }
    if parts.is_empty() {
        ".".to_string()
    } else {
        parts.join("/")
    }
}

/// A root-relative path as a manifest value: under Gradle's `build/`, it is
/// `{target}`'s — jrs's output directory, which a migrated build that leaves
/// tasks to Gradle points at `build/` too.
pub(super) fn template(path: &str) -> Template {
    let under_build = path
        .strip_prefix("build/")
        .or_else(|| (path == "build").then_some(""));
    match under_build {
        Some(rest) => {
            let raw = if rest.is_empty() {
                "{target}".to_string()
            } else {
                format!("{{target}}/{}", rest.replace('{', "{{").replace('}', "}}"))
            };
            Template::parse(&raw).unwrap_or_else(|_| Template::literal(path))
        }
        None => Template::literal(path),
    }
}

/// Whether a path is under Gradle's output directory.
pub(super) fn under_build(path: &str) -> bool {
    path == "build" || path.starts_with("build/")
}

// ---- statements that run on over lines --------------------------------------

/// The statements of a block, one per line but for what brackets keep open:
/// a call's arguments over several lines, a lambda (`.map { it.dir("x") }`)
/// or a nested block stays in the statement it belongs to.
fn joined_statements(block: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut depth = 0i64;
    for line in block.lines() {
        // A trailing `// comment`, which would swallow what the next line adds.
        let line = unquoted(line)
            .windows(2)
            .find(|w| w[0].1 == '/' && w[1].1 == '/' && w[1].0 == w[0].0 + 1)
            .map_or(line, |w| &line[..w[0].0])
            .trim();
        if line.is_empty() {
            continue;
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(line);
        depth += unquoted(line).into_iter().fold(0, |d, (_, c)| match c {
            '(' | '[' | '{' => d + 1,
            ')' | ']' | '}' => d - 1,
            _ => d,
        });
        if depth <= 0 {
            out.push(std::mem::take(&mut current));
            depth = 0;
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// `key.set(value)`, `key = value`, `key(value)` or `key value`: the key and
/// the value as written.
fn setting(statement: &str) -> Option<(&str, &str)> {
    let statement = statement.trim();
    let end = statement
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(statement.len());
    let (key, rest) = statement.split_at(end);
    if key.is_empty() {
        return None;
    }
    let rest = rest.trim();
    let rest = rest.strip_prefix(".set").unwrap_or(rest).trim_start();
    let value = if let Some(v) = rest.strip_prefix('=') {
        v.trim()
    } else if let Some((inner, after)) = bracketed(rest) {
        if !after.trim().is_empty() {
            return None;
        }
        // Kotlin allows a trailing comma after the last argument.
        inner.trim().trim_end_matches(',').trim_end()
    } else {
        rest
    };
    Some((key, value))
}

/// `mapOf("a" to "b", "c" to "")` or Groovy's `["a": "b"]`: its pairs, when
/// every key and value is a literal (`true` and numbers count).
fn map_pairs(value: &str) -> Option<Vec<(String, String)>> {
    let value = value.trim();
    let inner = if let Some(rest) = value
        .strip_prefix("mapOf")
        .or_else(|| value.strip_prefix("mutableMapOf"))
    {
        bracketed(rest.trim_start())
            .filter(|(_, after)| after.trim().is_empty())?
            .0
    } else {
        bracketed(value)
            .filter(|(_, after)| after.trim().is_empty())?
            .0
    };
    let mut pairs = Vec::new();
    for entry in split_top(inner, ',') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let (key, value) = split_pair(entry)?;
        pairs.push((scalar(key)?, scalar(value)?));
    }
    Some(pairs)
}

/// `"k" to "v"` or `"k": "v"`.
fn split_pair(entry: &str) -> Option<(&str, &str)> {
    let chars = unquoted(entry);
    for (i, c) in &chars {
        if *c == ':' {
            return Some((&entry[..*i], &entry[i + 1..]));
        }
    }
    let at = chars
        .windows(4)
        .find(|w| w[0].1 == ' ' && w[1].1 == 't' && w[2].1 == 'o' && w[3].1 == ' ')?
        .first()?
        .0;
    Some((&entry[..at], &entry[at + 4..]))
}

/// A literal string, boolean or number, as text.
fn scalar(value: &str) -> Option<String> {
    let value = value.trim();
    if let Some(l) = literal(value) {
        return (!l.interpolated).then_some(l.text);
    }
    let plain = value == "true"
        || value == "false"
        || (!value.is_empty() && value.chars().all(|c| c.is_ascii_digit() || c == '.'));
    plain.then(|| value.to_string())
}

// ---- the OpenAPI Generator -----------------------------------------------------

/// The plugin's id.
pub(super) const OPENAPI_PLUGIN: &str = "org.openapi.generator";

/// The command-line tool the plugin runs, from Maven Central, and its entry
/// point.
const OPENAPI_CLI: (&str, &str) = ("org.openapitools", "openapi-generator-cli");
const OPENAPI_MAIN: &str = "org.openapitools.codegen.OpenAPIGenerator";

/// `openApiGenerate { }` settings that name a file or directory.
const OPENAPI_PATHS: &[(&str, &str)] = &[
    ("inputSpec", "-i"),
    ("outputDir", "-o"),
    ("templateDir", "-t"),
    ("configFile", "-c"),
    ("ignoreFileOverride", "--ignore-file-override"),
];

/// Settings that are one string, and the CLI's option for each.
const OPENAPI_STRINGS: &[(&str, &str)] = &[
    ("generatorName", "-g"),
    ("modelPackage", "--model-package"),
    ("apiPackage", "--api-package"),
    ("invokerPackage", "--invoker-package"),
    ("packageName", "--package-name"),
    ("modelNamePrefix", "--model-name-prefix"),
    ("modelNameSuffix", "--model-name-suffix"),
    ("apiNameSuffix", "--api-name-suffix"),
    ("library", "--library"),
    ("groupId", "--group-id"),
    ("id", "--artifact-id"),
    ("version", "--artifact-version"),
    ("gitHost", "--git-host"),
    ("gitUserId", "--git-user-id"),
    ("gitRepoId", "--git-repo-id"),
    ("releaseNote", "--release-note"),
    ("httpUserAgent", "--http-user-agent"),
    ("auth", "--auth"),
];

/// Settings that are a map, written `k=v,k=v`.
const OPENAPI_MAPS: &[(&str, &str)] = &[
    ("globalProperties", "--global-property"),
    ("configOptions", "--additional-properties"),
    ("additionalProperties", "--additional-properties"),
    ("typeMappings", "--type-mappings"),
    ("importMappings", "--import-mappings"),
    ("schemaMappings", "--schema-mappings"),
    ("instantiationTypes", "--instantiation-types"),
    ("nameMappings", "--name-mappings"),
    ("modelNameMappings", "--model-name-mappings"),
    ("parameterNameMappings", "--parameter-name-mappings"),
    ("enumNameMappings", "--enum-name-mappings"),
    ("inlineSchemaNameMappings", "--inline-schema-name-mappings"),
    ("reservedWordsMappings", "--reserved-words-mappings"),
];

/// Settings that are a switch, set to `true`.
const OPENAPI_SWITCHES: &[(&str, &str)] = &[
    ("skipValidateSpec", "--skip-validate-spec"),
    ("removeOperationIdPrefix", "--remove-operation-id-prefix"),
    ("skipOverwrite", "--skip-overwrite"),
    ("minimalUpdate", "--minimal-update"),
    ("generateAliasAsModel", "--generate-alias-as-model"),
    ("enablePostProcessFile", "--enable-post-process-file"),
    ("skipOperationExample", "--skip-operation-example"),
    ("dryRun", "--dry-run"),
    ("verbose", "-v"),
];

/// The block that configures the plugin's `openApiGenerate` task.
fn openapi_block(top: &[Stmt]) -> Option<&str> {
    top.iter().find_map(|s| {
        let head: String = s.head.chars().filter(|c| !c.is_whitespace()).collect();
        let configures = head == "openApiGenerate"
            || head == "tasks.openApiGenerate"
            || head.starts_with("tasks.named<GenerateTask>(\"openApiGenerate\")")
            || head.starts_with("tasks.named(\"openApiGenerate\"");
        configures.then_some(s.block.as_deref()).flatten()
    })
}

/// `openApiGenerate { }` → a `main` task over the `OpenAPI` Generator's CLI at
/// the plugin's `version`, which does what the plugin's task does: every
/// setting jrs knows becomes the CLI's option. `None`, reported, when the
/// block names something jrs cannot work out.
pub(super) fn openapi(
    script: &str,
    version: Option<&str>,
    paths: &Paths,
    report: &mut Report,
) -> Option<TaskDef> {
    let top = statements(script);
    let block = openapi_block(&top)?;
    let Some(version) = version else {
        report.skipped(
            "`openApiGenerate { }` — the plugin's version could not be read, and the \
             generator's CLI is pinned at it"
                .to_string(),
        );
        return None;
    };
    let paths = paths.within(block);
    let mut args: Vec<Template> = vec![Template::literal("generate")];
    let mut maps: Vec<(&str, Vec<String>)> = Vec::new();
    let mut inputs = Vec::new();
    let mut outputs = Vec::new();
    for statement in joined_statements(block) {
        let Some((key, value)) = setting(&statement) else {
            continue;
        };
        if let Some((_, option)) = OPENAPI_PATHS.iter().find(|(k, _)| *k == key) {
            let Some(path) = paths.path(value) else {
                report.skipped(format!(
                    "`openApiGenerate {{ {statement} }}` — not a path jrs can work out without \
                     running Gradle; the task was not migrated"
                ));
                return None;
            };
            args.push(Template::literal(option));
            args.push(path_argument(&path));
            match key {
                "outputDir" => outputs.push(template(&path)),
                "inputSpec" | "templateDir" | "configFile" => inputs.push(template(&path)),
                _ => {}
            }
        } else if let Some((_, option)) = OPENAPI_STRINGS.iter().find(|(k, _)| *k == key) {
            let Some(text) = scalar(value) else {
                report.skipped(format!(
                    "`openApiGenerate {{ {statement} }}` — not a literal; the task was not \
                     migrated"
                ));
                return None;
            };
            args.push(Template::literal(option));
            args.push(Template::literal(&text));
        } else if let Some((_, option)) = OPENAPI_MAPS.iter().find(|(k, _)| *k == key) {
            let Some(pairs) = map_pairs(value) else {
                report.skipped(format!(
                    "`openApiGenerate {{ {statement} }}` — not a map of literals; the task was \
                     not migrated"
                ));
                return None;
            };
            let entries = pairs.into_iter().map(|(k, v)| format!("{k}={v}"));
            match maps.iter_mut().find(|(o, _)| o == option) {
                Some((_, list)) => list.extend(entries),
                None => maps.push((option, entries.collect())),
            }
        } else if let Some((_, option)) = OPENAPI_SWITCHES.iter().find(|(k, _)| *k == key) {
            if value == "true" {
                args.push(Template::literal(option));
            }
        } else if key == "languageSpecificPrimitives" {
            match literal_list(value) {
                Some(list) if !list.is_empty() => {
                    args.push(Template::literal("--language-specific-primitives"));
                    args.push(Template::literal(&list.join(",")));
                }
                _ => report.review(format!(
                    "`openApiGenerate {{ {statement} }}` — not a list of literals; set \
                     --language-specific-primitives by hand"
                )),
            }
        } else {
            report.review(format!(
                "`openApiGenerate {{ {statement} }}` — not translated to the generator's CLI; \
                 add its option to [tasks.open-api-generate] args by hand"
            ));
        }
    }
    for (option, entries) in maps {
        args.push(Template::literal(option));
        args.push(Template::literal(&entries.join(",")));
    }
    Some(openapi_task(version, args, inputs, outputs))
}

/// A path as the generator's CLI argument: under `{target}` when Gradle
/// would write it into the build directory, under `{root}` otherwise.
fn path_argument(path: &str) -> Template {
    if under_build(path) {
        template(path)
    } else {
        Template::parse(&format!(
            "{{root}}/{}",
            path.replace('{', "{{").replace('}', "}}")
        ))
        .unwrap_or_else(|_| Template::literal(path))
    }
}

/// `listOf("a", "b")`, `setOf(...)` or `["a", "b"]`: the literals in it.
fn literal_list(value: &str) -> Option<Vec<String>> {
    bracketed(value.trim_start_matches(|c: char| c.is_alphabetic())).map(|(inner, _)| {
        split_top(inner, ',')
            .into_iter()
            .filter_map(scalar)
            .collect()
    })
}

/// The `[tasks.open-api-generate]` that runs the generator's CLI at `version`.
fn openapi_task(
    version: &str,
    args: Vec<Template>,
    inputs: Vec<Template>,
    outputs: Vec<Template>,
) -> TaskDef {
    let mut def = TaskDef {
        name: "open-api-generate".to_string(),
        description: Some(format!(
            "Generate code from the OpenAPI spec (the Gradle plugin's openApiGenerate, run \
             as the OpenAPI Generator CLI {version})"
        )),
        action: Some(Action::Main(OPENAPI_MAIN.to_string())),
        args,
        depends_on: Vec::new(),
        env: Vec::new(),
        cwd: None,
        inputs,
        outputs,
        source_outputs: Vec::new(),
        resource_outputs: Vec::new(),
        dependencies: vec![Dependency::new(OPENAPI_CLI.0, OPENAPI_CLI.1, version)],
    };
    // Without an output there is nothing to be fresh about.
    if def.outputs.is_empty() {
        def.inputs.clear();
    }
    def
}

// ---- tasks left to Gradle ------------------------------------------------------

/// The name of the task that runs Gradle, unless the build has a task of that
/// name already.
pub(super) fn delegate_name(taken: &[String]) -> String {
    ["gradle", "gradle-tasks", "run-gradle"]
        .into_iter()
        .find(|n| !taken.iter().any(|t| t == n))
        .unwrap_or("gradle-bridge")
        .to_string()
}

/// A task that runs `gradle` tasks with the project's Gradle wrapper, in one
/// call. `blocks` are their bodies, read for what they take in and write,
/// so the task is fresh while neither changed; the build files are inputs
/// too, since they are the tasks' code.
pub(super) fn delegate(
    name: &str,
    gradle: &[String],
    blocks: &[Option<String>],
    paths: &Paths,
    build_files: &[String],
) -> TaskDef {
    let mut argv = vec![Template::literal("./gradlew"), Template::literal("--quiet")];
    argv.extend(gradle.iter().map(|g| Template::literal(g)));
    let mut inputs: Vec<String> = build_files.to_vec();
    let mut outputs: Vec<String> = Vec::new();
    let mut every_task_says = true;
    for block in blocks {
        let (task_inputs, task_outputs) = block
            .as_deref()
            .map(|b| declared_files(b, &paths.within(b)))
            .unwrap_or_default();
        every_task_says &= !task_inputs.is_empty() && !task_outputs.is_empty();
        for path in task_inputs {
            if !inputs.contains(&path) {
                inputs.push(path);
            }
        }
        for path in task_outputs {
            if !outputs.contains(&path) {
                outputs.push(path);
            }
        }
    }
    let (inputs, outputs) = if every_task_says {
        (
            inputs.iter().map(|p| template(p)).collect(),
            outputs.iter().map(|p| template(p)).collect(),
        )
    } else {
        // One task that does not say what it reads or writes runs every time
        // in Gradle too; so does the whole call here.
        (Vec::new(), Vec::new())
    };
    TaskDef {
        name: name.to_string(),
        description: Some(format!(
            "Run the Gradle tasks jrs cannot translate: {}",
            gradle.join(", ")
        )),
        action: Some(Action::Run(argv)),
        args: Vec::new(),
        depends_on: Vec::new(),
        env: Vec::new(),
        cwd: None,
        inputs,
        outputs,
        source_outputs: Vec::new(),
        resource_outputs: Vec::new(),
        dependencies: Vec::new(),
    }
}

/// `inputs.file(...)`, `inputs.dir(...)`, `outputs.file(...)` and
/// `outputs.dir(...)` in a task's body, as paths — the ones jrs can work out.
fn declared_files(block: &str, paths: &Paths) -> (Vec<String>, Vec<String>) {
    let mut inputs = Vec::new();
    let mut outputs = Vec::new();
    for statement in joined_statements(block) {
        let compact = compact(&statement);
        for (prefix, output) in [
            ("inputs.file(", false),
            ("inputs.dir(", false),
            ("inputs.files(", false),
            ("outputs.file(", true),
            ("outputs.dir(", true),
            ("outputs.files(", true),
        ] {
            let Some(args) = compact
                .strip_prefix(prefix)
                .and_then(|rest| rest.strip_suffix(')'))
            else {
                continue;
            };
            let list = if output { &mut outputs } else { &mut inputs };
            list.extend(
                split_top(args, ',')
                    .into_iter()
                    .filter_map(|arg| paths.path(arg)),
            );
        }
    }
    (inputs, outputs)
}

// ---- source sets ---------------------------------------------------------------

/// `sourceSets { main { java/kotlin/resources { srcDir(...) } } }`: the
/// generated directories each compiles, handed to the task that writes them
/// as its `source-outputs` or `resource-outputs`. Returns whether every
/// directory there was accounted for, so that `sourceSets { }` need not be
/// reported.
pub(super) fn read_source_sets(
    script: &str,
    paths: &Paths,
    out: &mut Manifest,
    report: &mut Report,
) -> bool {
    let top = statements(script);
    let mut all_read = true;
    let mut any = false;
    for stmt in top.iter().filter(|s| s.head.trim() == "sourceSets") {
        for set in statements(stmt.block.as_deref().unwrap_or_default()) {
            if set.head.trim() != "main" {
                // Another source set is a test suite's, or jrs has no place
                // for it.
                all_read &= set.head.trim() == "test" && set.block.is_none();
                continue;
            }
            for part in statements(set.block.as_deref().unwrap_or_default()) {
                let resources = match part.head.trim() {
                    "java" | "kotlin" => false,
                    "resources" => true,
                    _ => {
                        all_read = false;
                        continue;
                    }
                };
                for dir in joined_statements(part.block.as_deref().unwrap_or_default()) {
                    let head = dir.trim();
                    let Some(args) = head
                        .strip_prefix("srcDirs")
                        .or_else(|| head.strip_prefix("srcDir"))
                        .and_then(|r| bracketed(r.trim_start()))
                        .map(|(inner, _)| inner)
                    else {
                        all_read = false;
                        continue;
                    };
                    for arg in split_top(args, ',') {
                        any = true;
                        all_read &= source_dir(arg.trim(), resources, paths, out, report);
                    }
                }
            }
        }
    }
    any && all_read
}

/// One `srcDir(...)` of the main source set: a generated one goes to the
/// task that writes it. Returns whether it was accounted for.
fn source_dir(
    arg: &str,
    resources: bool,
    paths: &Paths,
    out: &mut Manifest,
    report: &mut Report,
) -> bool {
    let kind = if resources {
        "resource-outputs"
    } else {
        "source-outputs"
    };
    let Some(path) = paths.path(arg) else {
        report.skipped(format!(
            "`sourceSets.main {{ srcDir({arg}) }}` — not a path jrs can work out without \
             running Gradle"
        ));
        return false;
    };
    if !under_build(&path) {
        let own = [
            "src/main/java",
            "src/main/kotlin",
            "src/main/resources",
            "src/main/groovy",
            "src/main/scala",
        ];
        if own.contains(&path.as_str()) {
            return true;
        }
        report.skipped(format!(
            "`sourceSets.main {{ srcDir({arg}) }}` — {path} is a second source root, which \
             jrs does not have; move its files, or point project.source-dir at it"
        ));
        return false;
    }
    let dir = template(&path);
    let generators = crate::task::reached_from(out, Hook::PreCompile)
        .into_iter()
        .map(|t| t.name.clone())
        .collect::<Vec<_>>();
    let overlaps = |t: &TaskDef| {
        t.outputs.iter().any(|o| {
            let o = o.raw.trim_end_matches('/');
            let d = dir.raw.trim_end_matches('/');
            d == o || d.starts_with(&format!("{o}/")) || o.starts_with(&format!("{d}/"))
        })
    };
    let owner = out
        .tasks
        .iter()
        .position(|t| generators.contains(&t.name) && overlaps(t))
        .or_else(|| {
            // A task that says nothing of its outputs, run by Gradle: what it
            // writes is Gradle's to know.
            out.tasks.iter().position(|t| {
                generators.contains(&t.name) && t.outputs.is_empty() && runs_gradle(t)
            })
        });
    let Some(owner) = owner else {
        report.skipped(format!(
            "`sourceSets.main {{ srcDir({arg}) }}` — nothing the build runs before the compile \
             writes {path}"
        ));
        return false;
    };
    let task = &mut out.tasks[owner];
    let list = if resources {
        &mut task.resource_outputs
    } else {
        &mut task.source_outputs
    };
    if !list.contains(&dir) {
        list.push(dir.clone());
    }
    report.migrated(format!(
        "`sourceSets.main {{ srcDir({arg}) }}` → [tasks.{}] {kind} = {:?}",
        task.name, dir.raw
    ));
    true
}

/// Whether a task runs Gradle: the one [`delegate`] writes.
pub(super) fn runs_gradle(task: &TaskDef) -> bool {
    matches!(&task.action, Some(Action::Run(argv)) if argv.first().is_some_and(|a| a.raw == "./gradlew"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCRIPT: &str = r#"
val jsTomlFile = rootProject.file("gradle/libs.versions.js.toml")
val openApiOutputDir = layout.buildDirectory.dir("generated/sources/openapi")

val generateJsUrls =
  tasks.register("generateJsUrls") {
    val outputDir = layout.buildDirectory.dir("generated/sources/jsUrls/kotlin/pl/app")
    inputs.file(jsTomlFile)
    outputs.dir(outputDir)
    doLast { println("hi") }
  }

openApiGenerate {
  generatorName.set("kotlin")
  inputSpec.set("$rootDir/src/main/resources/api-spec/spec.yml")
  outputDir.set(openApiOutputDir.map { it.asFile.absolutePath })
  modelPackage.set("pl.app.generated")
  modelNameSuffix = "Dto"
  globalProperties.set(
    mapOf(
      "models" to "", // every model
      "apis" to "false", // no // API classes
    ),
  )
  typeMappings.set(mapOf("java.math.BigDecimal" to "Double"))
}
"#;

    #[test]
    fn paths_are_worked_out_through_variables_providers_and_scopes() {
        let paths = Paths::read(SCRIPT);
        assert_eq!(
            paths
                .path("openApiOutputDir.map { it.asFile.absolutePath }")
                .as_deref(),
            Some("build/generated/sources/openapi")
        );
        assert_eq!(
            paths
                .path("openApiOutputDir.map { it.dir(\"src/main/kotlin\") }")
                .as_deref(),
            Some("build/generated/sources/openapi/src/main/kotlin")
        );
        assert_eq!(
            paths.path("jsTomlFile").as_deref(),
            Some("gradle/libs.versions.js.toml")
        );
        assert_eq!(
            paths
                .path("layout.projectDirectory.dir(\"src\")")
                .as_deref(),
            Some("src")
        );
        assert_eq!(
            paths.path("\"${rootDir}/a/b.yml\"").as_deref(),
            Some("a/b.yml")
        );
        assert_eq!(paths.path("System.getenv(\"X\")"), None);
        assert_eq!(paths.path("\"/abs/path\""), None);
        assert_eq!(paths.path("\"$other/x\""), None);

        let block = "val outputDir = layout.buildDirectory.dir(\"mine\")\n";
        assert_eq!(
            paths.within(block).path("outputDir").as_deref(),
            Some("build/mine"),
            "a task's own val first"
        );
    }

    #[test]
    fn a_build_path_is_the_target_directorys() {
        assert_eq!(template("build/generated/x").raw, "{target}/generated/x");
        assert_eq!(template("build").raw, "{target}");
        assert_eq!(template("src/main").raw, "src/main");
    }

    #[test]
    fn open_api_generate_becomes_the_generators_cli() {
        let mut report = Report::default();
        let def = openapi(SCRIPT, Some("7.21.0"), &Paths::read(SCRIPT), &mut report)
            .unwrap_or_else(|| panic!("{:?}", report.not_migrated));
        let args: Vec<&str> = def.args.iter().map(|t| t.raw.as_str()).collect();
        assert_eq!(
            args,
            [
                "generate",
                "-g",
                "kotlin",
                "-i",
                "{root}/src/main/resources/api-spec/spec.yml",
                "-o",
                "{target}/generated/sources/openapi",
                "--model-package",
                "pl.app.generated",
                "--model-name-suffix",
                "Dto",
                "--global-property",
                "models=,apis=false",
                "--type-mappings",
                "java.math.BigDecimal=Double",
            ]
        );
        assert_eq!(def.action, Some(Action::Main(OPENAPI_MAIN.to_string())));
        assert_eq!(
            def.dependencies[0].to_string(),
            "org.openapitools:openapi-generator-cli:7.21.0"
        );
        assert_eq!(def.outputs[0].raw, "{target}/generated/sources/openapi");
        assert_eq!(def.inputs[0].raw, "src/main/resources/api-spec/spec.yml");
        assert!(report.not_migrated.is_empty(), "{:?}", report.not_migrated);
        assert!(openapi("x = 1\n", Some("7.21.0"), &Paths::default(), &mut report).is_none());
    }

    #[test]
    fn a_delegated_task_is_fresh_only_when_every_task_says_what_it_touches() {
        let top = statements(SCRIPT);
        let block = top
            .iter()
            .find(|s| s.head.contains("generateJsUrls") && s.block.is_some())
            .and_then(|s| s.block.clone());
        let def = delegate(
            "gradle",
            &["generateJsUrls".to_string()],
            &[block],
            &Paths::read(SCRIPT),
            &["build.gradle.kts".to_string()],
        );
        let raw = |ts: &[Template]| ts.iter().map(|t| t.raw.clone()).collect::<Vec<_>>();
        assert_eq!(
            raw(&def.inputs),
            ["build.gradle.kts", "gradle/libs.versions.js.toml"]
        );
        assert_eq!(
            raw(&def.outputs),
            ["{target}/generated/sources/jsUrls/kotlin/pl/app"]
        );
        assert!(runs_gradle(&def));

        let silent = delegate(
            "gradle",
            &["x".to_string()],
            &[None],
            &Paths::default(),
            &[],
        );
        assert!(silent.inputs.is_empty() && silent.outputs.is_empty());
    }
}
