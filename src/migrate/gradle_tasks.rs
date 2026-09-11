//! Gradle tasks → `[tasks]` and `[hooks]` (TASKS.md §12, item 5).
//!
//! A Gradle task is a program, and most cannot be translated without running
//! Gradle. Three kinds say everything they do in literals, and those are
//! translated: an `Exec` task (a command line), a `JavaExec` task over the main
//! runtime classpath (`java` over the project's classpath), and a task with no
//! action that only depends on others. `dependsOn` on `compileJava`, `test` or
//! `run`, and `finalizedBy` on `compileJava`, `test` or `jar`, become hooks.
//!
//! Anything else in a task (a `doLast { }` closure, a `Copy` spec, a value
//! built from a variable) keeps the whole task out of the manifest, with the
//! reason in the report, and so does a dependency on a task that was left out.
//! Half a task would be worse than none: it would run, and do the wrong thing.

use super::Report;
use crate::manifest::{
    Action, Builtin, Hook, Hooks, Manifest, Placeholder, RESERVED_TASK_NAMES, Segment, TaskDef,
    TaskRef, Template,
};

// ---- lexing ----------------------------------------------------------------

/// A Groovy or Kotlin string literal.
struct Literal {
    text: String,
    /// `"$x"` or `"${x}"`: a template jrs cannot evaluate.
    interpolated: bool,
}

/// `elem` when it is exactly one string literal, escapes decoded.
fn literal(elem: &str) -> Option<Literal> {
    let elem = elem.trim();
    let quote = elem.chars().next().filter(|c| matches!(c, '\'' | '"'))?;
    if elem.len() < 2
        || !elem.ends_with(quote)
        || elem.starts_with("\"\"\"")
        || elem.starts_with("'''")
    {
        return None;
    }
    let mut text = String::new();
    let mut interpolated = false;
    let mut chars = elem[1..elem.len() - 1].chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next()? {
                'n' => text.push('\n'),
                't' => text.push('\t'),
                other => text.push(other),
            },
            // The literal ended early: `'a' + 'b'` is an expression.
            c if c == quote => return None,
            '$' if quote == '"' => {
                interpolated = true;
                text.push(c);
            }
            _ => text.push(c),
        }
    }
    Some(Literal { text, interpolated })
}

/// The characters of `line` outside string literals, with their byte offsets.
/// Braces, brackets, commas and semicolons only count there.
fn unquoted(line: &str) -> Vec<(usize, char)> {
    let mut out = Vec::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (i, c) in line.char_indices() {
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == q {
                quote = None;
            }
            continue;
        }
        if matches!(c, '\'' | '"') {
            quote = Some(c);
        } else {
            out.push((i, c));
        }
    }
    out
}

/// How many more blocks `line` opens than it closes.
fn brace_delta(line: &str) -> i64 {
    unquoted(line).into_iter().fold(0, |d, (_, c)| match c {
        '{' => d + 1,
        '}' => d - 1,
        _ => d,
    })
}

/// The byte offset of the last `}` outside quotes.
fn last_close(line: &str) -> Option<usize> {
    unquoted(line)
        .into_iter()
        .rev()
        .find(|(_, c)| *c == '}')
        .map(|(i, _)| i)
}

/// Split `text` on `sep` where it stands outside quotes and brackets.
fn split_top(text: &str, sep: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0i64;
    let mut start = 0;
    for (i, c) in unquoted(text) {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            c if c == sep && depth == 0 => {
                parts.push(&text[start..i]);
                start = i + c.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(&text[start..]);
    parts
}

/// One statement of a script or of a block: its head, and the text of the
/// block it opens, when it opens one.
struct Stmt {
    head: String,
    block: Option<String>,
}

/// The statements of `text` at its own level; nested blocks stay inside the
/// statement that opens them.
fn statements(text: &str) -> Vec<Stmt> {
    let mut out = Vec::new();
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        for part in split_top(line, ';') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            let open = unquoted(part)
                .into_iter()
                .find(|(_, c)| *c == '{')
                .map(|(i, _)| i);
            let Some(open) = open else {
                out.push(Stmt {
                    head: part.to_string(),
                    block: None,
                });
                continue;
            };
            let rest = &part[open + 1..];
            let mut depth = 1 + brace_delta(rest);
            let mut body = String::new();
            if depth <= 0 {
                // Opened and closed on one line.
                body.push_str(&rest[..last_close(rest).unwrap_or(rest.len())]);
            } else {
                body.push_str(rest);
                body.push('\n');
                for inner in lines.by_ref() {
                    depth += brace_delta(inner);
                    if depth <= 0 {
                        body.push_str(&inner[..last_close(inner).unwrap_or(0)]);
                        break;
                    }
                    body.push_str(inner);
                    body.push('\n');
                }
            }
            out.push(Stmt {
                head: part[..open].trim().to_string(),
                block: Some(body),
            });
        }
    }
    out
}

/// `(inner)` or `[inner]` at the start of `s`: the inner text, and what
/// follows the closing bracket.
fn bracketed(s: &str) -> Option<(&str, &str)> {
    let open = s.chars().next().filter(|c| matches!(c, '(' | '['))?;
    let close = if open == '(' { ')' } else { ']' };
    let mut depth = 0i64;
    for (i, c) in unquoted(s) {
        if c == open {
            depth += 1;
        } else if c == close {
            depth -= 1;
            if depth == 0 {
                return Some((&s[1..i], &s[i + 1..]));
            }
        }
    }
    None
}

/// Calls that only wrap a list or a path, and say nothing jrs needs.
const WRAPPERS: &[&str] = &[
    "",
    "listOf",
    "mutableListOf",
    "arrayOf",
    "setOf",
    "files",
    "file",
    "project.file",
    "layout.projectDirectory.file",
    "layout.projectDirectory.dir",
];

/// `text` without the wrappers around it: `(listOf("a", "b"))` → `"a", "b"`.
fn unwrap(text: &str) -> &str {
    let mut text = text.trim();
    loop {
        let inner = WRAPPERS.iter().find_map(|w| {
            let (inner, after) = bracketed(text.strip_prefix(w)?)?;
            after.trim().is_empty().then_some(inner)
        });
        match inner {
            Some(inner) => text = inner.trim(),
            None => return text,
        }
    }
}

/// What follows a property or a method: `= x`, `(x)`, ` x`, `.set(x)`.
fn argument(rest: &str) -> &str {
    let rest = rest.trim();
    let rest = rest.strip_prefix(".set").unwrap_or(rest).trim_start();
    rest.strip_prefix('=').unwrap_or(rest).trim()
}

/// Every element of `rest` as a string literal, or why one is not.
fn strings(rest: &str) -> Result<Vec<String>, String> {
    split_top(unwrap(argument(rest)), ',')
        .into_iter()
        .map(str::trim)
        .filter(|e| !e.is_empty())
        .map(|e| {
            let e = unwrap(e);
            match literal(e) {
                Some(l) if l.interpolated => Err(format!(
                    "`{e}` is interpolated from a variable, which jrs cannot evaluate \
                     without running Gradle"
                )),
                Some(l) => Ok(l.text),
                None => Err(format!(
                    "`{e}` is an expression, which jrs cannot evaluate without running Gradle"
                )),
            }
        })
        .collect()
}

/// `rest` as exactly one string literal.
fn string(rest: &str) -> Result<String, String> {
    let mut all = strings(rest)?;
    match all.len() {
        1 => Ok(all.remove(0)),
        _ => Err(format!("`{}` is not one string", argument(rest))),
    }
}

/// The leading `[A-Za-z_][A-Za-z0-9_]*` of `s`.
fn identifier(s: &str) -> Option<&str> {
    let end = s
        .char_indices()
        .find(|(i, c)| !(c.is_ascii_alphabetic() || *c == '_' || (*i > 0 && c.is_ascii_digit())))
        .map_or(s.len(), |(i, _)| i);
    (end > 0).then(|| &s[..end])
}

/// `<Type>` at the start of `s`, and what follows it.
fn type_argument(s: &str) -> (Option<&str>, &str) {
    s.strip_prefix('<')
        .and_then(|r| r.split_once('>'))
        .map_or((None, s), |(ty, after)| (Some(ty), after))
}

/// `org.gradle.api.tasks.Exec::class.java` → `Exec`.
fn type_name(s: &str) -> String {
    let s = s.trim();
    let s = s.strip_suffix(".java").unwrap_or(s);
    let s = s.strip_suffix("::class").unwrap_or(s);
    s.rsplit('.').next().unwrap_or(s).trim().to_string()
}

/// `dependsOn` arguments as Gradle task names, or why one is not a name.
fn task_refs(rest: &str) -> Result<Vec<String>, String> {
    split_top(unwrap(argument(rest)), ',')
        .into_iter()
        .map(str::trim)
        .filter(|e| !e.is_empty())
        .map(|e| task_ref(e).ok_or_else(|| format!("`{e}` does not name a task jrs can read")))
        .collect()
}

/// A literal, a bare name, `tasks.named("x")`, `tasks.getByName("x")`,
/// `tasks.x` or `tasks["x"]`.
fn task_ref(e: &str) -> Option<String> {
    let e = unwrap(e);
    let e = e.strip_suffix(".get()").unwrap_or(e);
    let named = |l: Literal| (!l.interpolated).then_some(l.text);
    if let Some(l) = literal(e) {
        return named(l);
    }
    if let Some(rest) = e.strip_prefix("tasks") {
        if let Some((inner, after)) = bracketed(rest) {
            return after
                .is_empty()
                .then(|| literal(inner))
                .flatten()
                .and_then(named);
        }
        let rest = rest.strip_prefix('.')?;
        for f in ["named", "getByName"] {
            if let Some(after) = rest.strip_prefix(f) {
                let (inner, after) = bracketed(type_argument(after).1)?;
                return after
                    .is_empty()
                    .then(|| literal(inner))
                    .flatten()
                    .and_then(named);
            }
        }
        return identifier(rest)
            .filter(|id| id.len() == rest.len())
            .map(str::to_string);
    }
    identifier(e)
        .filter(|id| id.len() == e.len())
        .map(str::to_string)
}

// ---- declarations and configuration ----------------------------------------

/// What a task's declaration says about it, besides its name.
#[derive(Default)]
struct Header {
    /// `Exec`, `JavaExec`, ...; `None` for a plain task.
    kind: Option<String>,
    /// Groovy's `task x(dependsOn: ...)`.
    depends_on: Vec<String>,
    /// Groovy's `task x(description: ...)`.
    description: Option<String>,
}

/// The Gradle name a declaration gives its task, and what else it says, or
/// why that cannot be read. `None` when `head` declares no task.
fn declaration(head: &str) -> Option<(String, Result<Header, String>)> {
    let head = head.trim();
    let head = head.strip_prefix("project.").unwrap_or(head);

    // `val name by tasks.registering(Type::class)`
    if let Some(rest) = head.strip_prefix("val ") {
        let rest = rest.trim_start();
        let name = identifier(rest)?;
        let after = rest[name.len()..]
            .trim_start()
            .strip_prefix("by ")?
            .trim_start();
        let after = after
            .strip_prefix("tasks.registering")
            .or_else(|| after.strip_prefix("tasks.creating"))?
            .trim();
        let header = if after.is_empty() {
            Ok(Header::default())
        } else {
            match bracketed(after) {
                Some((ty, "")) if !ty.contains(',') => Ok(Header {
                    kind: Some(type_name(ty)),
                    ..Header::default()
                }),
                _ => Err(format!("`{head}` passes arguments jrs does not read")),
            }
        };
        return Some((name.to_string(), header));
    }

    // `tasks.register("name", Type::class)`, `tasks.register<Type>("name")`
    for f in ["tasks.register", "tasks.create"] {
        let Some(after) = head.strip_prefix(f) else {
            continue;
        };
        let (ty, after) = type_argument(after);
        let (args, _) = bracketed(after.trim_start())?;
        let args = split_top(args, ',');
        let first = args[0].trim();
        let Some(name) = literal(first).filter(|l| !l.interpolated) else {
            return Some((
                first.to_string(),
                Err("its name is an expression jrs cannot evaluate".to_string()),
            ));
        };
        let header = match (ty, args.get(1), args.len()) {
            (_, _, 3..) => Err(format!("`{head}` passes arguments jrs does not read")),
            (Some(ty), None, _) => Ok(Some(type_name(ty))),
            (None, Some(ty), _) => Ok(Some(type_name(ty))),
            (None, None, _) => Ok(None),
            (Some(_), Some(_), _) => Err(format!("`{head}` names its type twice")),
        };
        return Some((
            name.text,
            header.map(|kind| Header {
                kind,
                ..Header::default()
            }),
        ));
    }

    // Groovy: `task name`, `task name(type: T, dependsOn: [...])`,
    // `task('name', type: T)`.
    let rest = head.strip_prefix("task")?;
    let (name, entries) = if let Some((args, _)) = bracketed(rest) {
        let mut parts = split_top(args, ',').into_iter();
        let first = parts.next().unwrap_or_default().trim();
        let name = literal(first)?.text;
        (name, parts.collect::<Vec<_>>())
    } else {
        let rest = rest.strip_prefix(|c: char| c.is_whitespace())?.trim_start();
        let name = identifier(rest)?;
        let entries = bracketed(rest[name.len()..].trim_start())
            .map(|(args, _)| split_top(args, ','))
            .unwrap_or_default();
        (name.to_string(), entries)
    };
    let mut header = Header::default();
    for entry in entries.iter().map(|e| e.trim()).filter(|e| !e.is_empty()) {
        let parsed = match entry.split_once(':') {
            Some(("type", value)) => {
                header.kind = Some(type_name(value));
                Ok(())
            }
            Some(("dependsOn", value)) => task_refs(value).map(|r| header.depends_on = r),
            Some(("description", value)) => string(value).map(|d| header.description = Some(d)),
            Some(("group", _)) => Ok(()),
            _ => Err(format!(
                "`{entry}` in its declaration is not something jrs reads"
            )),
        };
        if let Err(why) = parsed {
            return Some((name, Err(why)));
        }
    }
    Some((name, Ok(header)))
}

/// The task `withType<T>` configures, among those jrs has a hook beside.
fn task_of_type(ty: &str) -> Option<&'static str> {
    Some(match ty {
        "JavaCompile" => "compileJava",
        "KotlinCompile" | "KotlinJvmCompile" => "compileKotlin",
        "ScalaCompile" => "compileScala",
        "GroovyCompile" => "compileGroovy",
        "Test" => "test",
        "Jar" => "jar",
        _ => return None,
    })
}

/// The Gradle task a configuring statement names, and what follows it:
/// `compileJava.dependsOn x` → `compileJava`, `.dependsOn x`;
/// `tasks.named("test")` → `test`, and nothing.
fn configured(head: &str) -> Option<(String, &str)> {
    let s = head.trim();
    let s = s.strip_prefix("project.").unwrap_or(s);
    let (target, rest): (String, &str) = if let Some(r) = s.strip_prefix("tasks.") {
        if let Some(after) = r.strip_prefix("withType") {
            let (ty, after) = match type_argument(after) {
                (Some(ty), after) => (ty, after),
                (None, after) => bracketed(after)?,
            };
            (task_of_type(&type_name(ty))?.to_string(), after)
        } else if let Some(after) = r
            .strip_prefix("named")
            .or_else(|| r.strip_prefix("getByName"))
        {
            let (inner, after) = bracketed(type_argument(after).1)?;
            (literal(inner)?.text, after)
        } else {
            let id = identifier(r)?;
            if matches!(
                id,
                "register" | "create" | "all" | "matching" | "configureEach" | "whenTaskAdded"
            ) {
                return None;
            }
            (id.to_string(), &r[id.len()..])
        }
    } else if let Some(r) = s.strip_prefix("tasks") {
        let (inner, after) = bracketed(r)?;
        (literal(inner)?.text, after)
    } else {
        let id = identifier(s)?;
        (id.to_string(), &s[id.len()..])
    };
    let mut rest = rest.trim_start();
    while let Some(r) = [".configure", ".configureEach", ".get()"]
        .iter()
        .find_map(|p| rest.strip_prefix(p))
    {
        rest = r.trim_start();
    }
    Some((target, rest))
}

/// How one task is tied to another.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Relation {
    DependsOn,
    FinalizedBy,
    /// `mustRunAfter` / `shouldRunAfter`: ordering only, which jrs gets from
    /// `depends-on` alone.
    RunsAfter,
}

/// `dependsOn x`, `.finalizedBy(x)`, ...: the relation and its argument.
fn relation(text: &str) -> Option<(Relation, &str)> {
    let text = text.trim().trim_start_matches('.');
    [
        ("dependsOn", Relation::DependsOn),
        ("finalizedBy", Relation::FinalizedBy),
        ("mustRunAfter", Relation::RunsAfter),
        ("shouldRunAfter", Relation::RunsAfter),
    ]
    .into_iter()
    .find_map(|(word, rel)| {
        let rest = text.strip_prefix(word)?;
        (!rest.starts_with(|c: char| c.is_ascii_alphanumeric())).then_some((rel, rest))
    })
}

// ---- a task's body ---------------------------------------------------------

/// What a task's block sets, before its type decides what that means.
#[derive(Default)]
struct Body {
    description: Option<String>,
    depends_on: Vec<String>,
    command_line: Vec<String>,
    executable: Option<String>,
    args: Vec<String>,
    jvm_args: Vec<String>,
    main_class: Option<String>,
    /// `classpath = sourceSets.main.runtimeClasspath`.
    main_classpath: bool,
    working_dir: Option<String>,
    env: Vec<(String, String)>,
    inputs: Vec<String>,
    outputs: Vec<String>,
    /// `inputs` / `outputs` lines jrs could not read.
    unread_io: Vec<String>,
}

fn read_body(block: &str, body: &mut Body) -> Result<(), String> {
    for stmt in statements(block) {
        let text = stmt.head.as_str();
        if stmt.block.is_some() {
            let what = identifier(text).unwrap_or(text);
            return Err(format!(
                "`{what} {{ }}` is Gradle code, which jrs cannot run"
            ));
        }
        if let Some((relation, rest)) = relation(text) {
            match relation {
                Relation::DependsOn => body.depends_on.extend(task_refs(rest)?),
                Relation::RunsAfter => {}
                Relation::FinalizedBy => {
                    return Err("`finalizedBy` has no equivalent inside a jrs task".to_string());
                }
            }
            continue;
        }
        let end = text
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '.'))
            .unwrap_or(text.len());
        let (key, rest) = text.split_at(end);
        match key.strip_suffix(".set").unwrap_or(key) {
            "description" => body.description = Some(string(rest)?),
            "group" | "standardInput" => {}
            "commandLine" | "setCommandLine" => body.command_line = strings(rest)?,
            "executable" => body.executable = Some(string(rest)?),
            "args" | "setArgs" => body.args.extend(strings(rest)?),
            "jvmArgs" => body.jvm_args.extend(strings(rest)?),
            "mainClass" | "main" => body.main_class = Some(string(rest)?),
            "classpath" if is_main_runtime_classpath(rest) => body.main_classpath = true,
            "classpath" => {
                return Err(format!(
                    "its classpath `{}` is not `sourceSets.main.runtimeClasspath`, the \
                     only one jrs can give it",
                    argument(rest)
                ));
            }
            "workingDir" => {
                let dir = unwrap(argument(rest));
                body.working_dir = match dir {
                    "projectDir" | "rootDir" | "project.projectDir" | "project.rootDir" => None,
                    _ => Some(string(rest)?),
                };
            }
            "environment" => match strings(rest)?.as_slice() {
                [name, value] => body.env.push((name.clone(), value.clone())),
                _ => return Err(format!("`{text}` is not one name and one value")),
            },
            "inputs.file" | "inputs.files" | "inputs.dir" => match strings(rest) {
                Ok(paths) => body.inputs.extend(paths),
                Err(_) => body.unread_io.push(text.to_string()),
            },
            "outputs.file" | "outputs.files" | "outputs.dir" => match strings(rest) {
                Ok(paths) => body.outputs.extend(paths),
                Err(_) => body.unread_io.push(text.to_string()),
            },
            "ignoreExitValue" | "isIgnoreExitValue" => {
                return Err(
                    "it ignores the exit code, and jrs stops the build on a failing task"
                        .to_string(),
                );
            }
            _ => return Err(format!("`{text}` is not something jrs can translate")),
        }
    }
    Ok(())
}

/// `sourceSets.main.runtimeClasspath`, in its Groovy and Kotlin spellings.
fn is_main_runtime_classpath(rest: &str) -> bool {
    let compact: String = unwrap(argument(rest))
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    let compact = compact
        .replace(".get()", "")
        .replace("[\"main\"]", ".main")
        .replace("['main']", ".main")
        .replace(".getByName(\"main\")", ".main")
        .replace(".named(\"main\")", ".main");
    let compact = compact.strip_prefix("project.").unwrap_or(&compact);
    let compact = compact.strip_prefix("java.").unwrap_or(compact);
    compact == "sourceSets.main.runtimeClasspath"
}

/// `generateBuildInfo` → `generate-build-info`, `HTMLReport` → `html-report`.
/// Maven's execution ids go through it too.
pub(super) fn task_name(gradle: &str) -> Result<String, String> {
    let chars: Vec<char> = gradle.chars().collect();
    let mut out = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if matches!(c, '_' | '-' | '.') {
            if !out.is_empty() && !out.ends_with('-') {
                out.push('-');
            }
            continue;
        }
        if !c.is_ascii_alphanumeric() {
            return Err(format!(
                "its name `{gradle}` has characters a jrs task name cannot"
            ));
        }
        if c.is_ascii_uppercase() {
            let prev = i.checked_sub(1).map(|j| chars[j]);
            let next = chars.get(i + 1);
            let boundary = prev.is_some_and(|p| p.is_ascii_lowercase() || p.is_ascii_digit())
                || (prev.is_some_and(|p| p.is_ascii_uppercase())
                    && next.is_some_and(char::is_ascii_lowercase));
            if boundary && !out.ends_with('-') {
                out.push('-');
            }
        }
        out.push(c.to_ascii_lowercase());
    }
    let out = out.trim_end_matches('-').to_string();
    if !out.starts_with(|c: char| c.is_ascii_lowercase()) {
        return Err(format!(
            "its name `{gradle}` does not start with a letter, as a jrs task name must"
        ));
    }
    if RESERVED_TASK_NAMES.contains(&out.as_str()) {
        return Err(format!("its name would be `{out}`, which is a jrs command"));
    }
    Ok(out)
}

/// `@{classpath-argfile}`: `-cp` and the classpath, from a file.
fn classpath_argfile() -> Template {
    Template {
        raw: "@{classpath-argfile}".to_string(),
        segments: vec![
            Segment::Text("@".to_string()),
            Segment::Placeholder(Placeholder::ClasspathArgfile),
        ],
    }
}

/// A task the build script declares, translated but for its `depends-on`,
/// which waits until every task's fate is known.
struct Declared {
    gradle: String,
    kind: String,
    def: TaskDef,
    /// Gradle task names.
    depends_on: Vec<String>,
    /// What the task needs whatever it declares: a `JavaExec` needs classes.
    implied: Vec<Builtin>,
    review: Vec<String>,
}

fn translate(gradle: &str, header: Header, block: Option<&str>) -> Result<Declared, String> {
    let name = task_name(gradle)?;
    if let Some(other) = header
        .kind
        .as_deref()
        .filter(|k| !matches!(*k, "Exec" | "JavaExec" | "DefaultTask"))
    {
        return Err(format!(
            "its type `{other}` has no equivalent in jrs; rewrite it as a task that runs \
             a command or a script"
        ));
    }
    let mut body = Body {
        description: header.description,
        depends_on: header.depends_on,
        ..Body::default()
    };
    if let Some(block) = block {
        read_body(block, &mut body)?;
    }
    let kind = header.kind.unwrap_or_else(|| "DefaultTask".to_string());
    let (argv, implied) = command(&kind, &body)?;
    if let Some((var, _)) = body
        .env
        .iter()
        .find(|(k, _)| k.to_ascii_uppercase().starts_with("JRS_") || k.contains('='))
    {
        return Err(format!(
            "`{var}` is not an environment variable jrs lets a task set"
        ));
    }
    let review = review_notes(&name, &body);

    Ok(Declared {
        gradle: gradle.to_string(),
        kind,
        def: TaskDef {
            name,
            description: body.description,
            action: argv.map(Action::Run),
            args: Vec::new(),
            depends_on: Vec::new(),
            env: body
                .env
                .iter()
                .map(|(k, v)| (k.clone(), Template::literal(v)))
                .collect(),
            cwd: body.working_dir.as_deref().map(Template::literal),
            inputs: literals(&body.inputs),
            outputs: literals(&body.outputs),
            source_outputs: Vec::new(),
            resource_outputs: Vec::new(),
            dependencies: Vec::new(),
        },
        depends_on: body.depends_on,
        implied,
        review,
    })
}

fn literals(values: &[String]) -> Vec<Template> {
    values.iter().map(|v| Template::literal(v)).collect()
}

/// The command a task of type `kind` runs, if it runs one, and the built-in
/// phases it needs whatever it declares.
fn command(kind: &str, body: &Body) -> Result<(Option<Vec<Template>>, Vec<Builtin>), String> {
    let java_exec = body.main_class.is_some() || body.main_classpath || !body.jvm_args.is_empty();
    let exec = !body.command_line.is_empty() || body.executable.is_some();
    let mut implied = Vec::new();
    let argv = match kind {
        "Exec" if java_exec => {
            return Err("it sets `JavaExec` properties on an `Exec` task".to_string());
        }
        "Exec" => {
            let argv = match (&body.executable, body.command_line.is_empty()) {
                (None, false) if body.args.is_empty() => body.command_line.clone(),
                (Some(exe), true) => std::iter::once(exe.clone())
                    .chain(body.args.clone())
                    .collect(),
                (None, true) => return Err("it names no `commandLine` or `executable`".to_string()),
                _ => {
                    return Err(
                        "it sets `commandLine` together with `executable` or `args`, \
                                which Gradle combines in an order jrs does not guess at"
                            .to_string(),
                    );
                }
            };
            Some(literals(&argv))
        }
        "JavaExec" if exec => {
            return Err("it sets `commandLine` or `executable` on a `JavaExec` task".to_string());
        }
        "JavaExec" => {
            let Some(main) = &body.main_class else {
                return Err("it names no `mainClass`".to_string());
            };
            if !body.main_classpath {
                return Err(
                    "its classpath is not `sourceSets.main.runtimeClasspath`, the \
                            only one jrs can give it"
                        .to_string(),
                );
            }
            implied.push(Builtin::Build);
            let mut argv = vec![Template::literal("java")];
            argv.extend(literals(&body.jvm_args));
            argv.push(classpath_argfile());
            argv.push(Template::literal(main));
            argv.extend(literals(&body.args));
            Some(argv)
        }
        "DefaultTask" if exec || java_exec || !body.args.is_empty() => {
            return Err("it sets a command on a task that has no type to run one".to_string());
        }
        "DefaultTask" if body.depends_on.is_empty() => {
            return Err("it does nothing jrs can see: no action, and no `dependsOn`".to_string());
        }
        "DefaultTask" => None,
        other => {
            return Err(format!(
                "its type `{other}` has no equivalent in jrs; rewrite it as a task that \
                 runs a command or a script"
            ));
        }
    };
    Ok((argv, implied))
}

/// What the user should check by hand once the task is migrated.
fn review_notes(name: &str, body: &Body) -> Vec<String> {
    let mut review: Vec<String> = body
        .unread_io
        .iter()
        .map(|line| {
            format!(
                "[tasks.{name}] — `{line}` could not be read, so the task has no \
                 up-to-date check and always runs"
            )
        })
        .collect();
    let gradle_output = body
        .command_line
        .iter()
        .chain(&body.executable)
        .chain(&body.args)
        .chain(&body.working_dir)
        .chain(&body.inputs)
        .chain(&body.outputs)
        .any(|v| v == "build" || v.starts_with("build/") || v.contains("/build/"));
    if gradle_output {
        review.push(format!(
            "[tasks.{name}] — names `build/`, Gradle's output directory; jrs writes to \
             target/ (`{{target}}` in a task, `{{jar}}` for the jar)"
        ));
    }
    review
}

// ---- the environment of Gradle's `run` and `test` --------------------------

/// `environment` and `workingDir` in the blocks that configure Gradle's own
/// `run` and `test` tasks, as `run.env`, `run.cwd` and `test.env`. Literals
/// translate; anything else is reported, as in a task.
pub(super) fn read_jvm_environment(script: &str, out: &mut Manifest, report: &mut Report) {
    for stmt in statements(script) {
        let Some(block) = stmt.block.as_deref() else {
            continue;
        };
        if declaration(&stmt.head).is_some() {
            continue;
        }
        let section = match configured(&stmt.head) {
            Some((target, "")) if target == "run" => "run",
            Some((target, "")) if target == "test" => "test",
            _ => continue,
        };
        for inner in statements(block).into_iter().filter(|s| s.block.is_none()) {
            let text = inner.head.as_str();
            let end = text
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '.'))
                .unwrap_or(text.len());
            let (key, rest) = text.split_at(end);
            match (key.strip_suffix(".set").unwrap_or(key), section) {
                ("environment", _) => read_environment(section, text, rest, out, report),
                ("workingDir", "run") => read_working_dir(text, rest, out, report),
                ("workingDir", _) => report.skipped(format!(
                    "`{text}` in the `test` task — jrs starts the test JVM where jrs itself \
                     runs, and has no `test.cwd`"
                )),
                _ => {}
            }
        }
    }
}

/// `environment 'NAME', 'value'` into `<section>.env`; a name set twice keeps
/// its last value, as in Gradle.
fn read_environment(
    section: &str,
    text: &str,
    rest: &str,
    out: &mut Manifest,
    report: &mut Report,
) {
    let pair = strings(rest).and_then(|values| match values.as_slice() {
        [name, value] if name.is_empty() || name.contains(['=', '\0']) => {
            Err(format!("`{name}` is not an environment variable name"))
        }
        [name, _] if name.to_ascii_uppercase().starts_with("JRS_") => Err(format!(
            "`{name}` starts with `JRS_`, which jrs keeps for itself"
        )),
        [name, value] => Ok((name.clone(), value.clone())),
        _ => Err("it is not one name and one value".to_string()),
    });
    match pair {
        Ok((name, value)) => {
            let env = if section == "run" {
                &mut out.run.env
            } else {
                &mut out.test.env
            };
            env.retain(|(k, _)| *k != name);
            env.push((name.clone(), Template::literal(&value)));
            report.migrated(format!("{section}.env.{name} = {value:?}"));
        }
        Err(why) => report.skipped(format!("`{text}` in the `{section}` task — {why}")),
    }
}

/// `workingDir` of Gradle's `run` into `run.cwd`. Gradle's default, the
/// project directory, is `.`.
fn read_working_dir(text: &str, rest: &str, out: &mut Manifest, report: &mut Report) {
    let dir = match unwrap(argument(rest)) {
        "projectDir"
        | "rootDir"
        | "project.projectDir"
        | "project.rootDir"
        | "layout.projectDirectory" => ".".to_string(),
        _ => match string(rest) {
            Ok(dir) => dir,
            Err(why) => {
                report.skipped(format!("`{text}` in the `run` task — {why}"));
                return;
            }
        },
    };
    if dir == "build" || dir.starts_with("build/") {
        report.review(format!(
            "run.cwd — `{dir}` is under `build/`, Gradle's output directory; jrs writes to \
             target/ (`{{target}}` in `run.cwd`)"
        ));
    } else if std::path::Path::new(&dir).is_absolute() {
        report.review(format!(
            "run.cwd — `{dir}` is an absolute path, which only means something on one machine"
        ));
    }
    report.migrated(format!("run.cwd = {dir:?}"));
    out.run.cwd = Some(Template::literal(&dir));
}

// ---- tying it together -----------------------------------------------------

/// Gradle's own tasks a jrs task can depend on, as the built-ins doing the
/// same work.
fn builtins_for(gradle: &str) -> Option<&'static [Builtin]> {
    Some(match gradle {
        "classes" | "compileJava" | "compileKotlin" | "compileScala" | "compileGroovy"
        | "processResources" => &[Builtin::Build],
        "jar" | "assemble" => &[Builtin::Package],
        "test" | "check" => &[Builtin::Test],
        "javadoc" => &[Builtin::Doc],
        "build" => &[Builtin::Test, Builtin::Package],
        _ => return None,
    })
}

/// The tasks that compile the main sources; a hook beside one is a hook
/// beside all of them.
const COMPILE_TASKS: &[&str] = &[
    "compileJava",
    "compileKotlin",
    "compileScala",
    "compileGroovy",
    "processResources",
    "classes",
];

/// Gradle's own tasks. A `dependsOn` on one jrs has no hook beside is
/// reported, not passed over.
const GRADLE_TASKS: &[&str] = &[
    "compileJava",
    "compileKotlin",
    "compileScala",
    "compileGroovy",
    "processResources",
    "classes",
    "compileTestJava",
    "processTestResources",
    "testClasses",
    "test",
    "check",
    "jar",
    "assemble",
    "build",
    "javadoc",
    "run",
    "clean",
    "distZip",
    "installDist",
    "shadowJar",
    "bootJar",
    "bootRun",
];

/// The hook that runs where `relation` on Gradle's `target` would.
fn hook_for(target: &str, relation: Relation) -> Option<Hook> {
    let compile = COMPILE_TASKS.contains(&target);
    Some(match (relation, target) {
        (Relation::DependsOn, _) if compile => Hook::PreCompile,
        (Relation::FinalizedBy, _) if compile => Hook::PostCompile,
        (Relation::DependsOn, "test") => Hook::PreTest,
        (Relation::FinalizedBy, "test") => Hook::PostTest,
        (Relation::FinalizedBy, "jar" | "assemble") => Hook::PostPackage,
        (Relation::DependsOn, "run") => Hook::PreRun,
        _ => return None,
    })
}

/// A `dependsOn` or `finalizedBy` written apart from a declaration.
struct Tie {
    target: String,
    relation: Relation,
    refs: Vec<String>,
    text: String,
}

/// Translate the script's tasks into `out.tasks` and `out.hooks`, reporting
/// every task and every tie that could not be.
pub(super) fn read(script: &str, out: &mut Manifest, report: &mut Report) {
    let top = statements(script);
    let (mut declared, mut failed) = read_declarations(&top);
    let custom: Vec<String> = declared
        .iter()
        .map(|d| d.gradle.clone())
        .chain(failed.iter().map(|(g, _)| g.clone()))
        .collect();
    let (ties, reconfigured) = read_ties(&top, &custom, report);

    let mut fail = |declared: &mut Vec<Declared>, gradle: &str, why: String| {
        if let Some(i) = declared.iter().position(|d| d.gradle == gradle) {
            declared.remove(i);
            failed.push((gradle.to_string(), why));
        }
    };
    for (target, head) in reconfigured {
        fail(
            &mut declared,
            &target,
            format!("it is configured again at `{head}`, which jrs does not merge"),
        );
    }
    let mut hooked = Vec::new();
    for tie in ties {
        if custom.contains(&tie.target) {
            match tie.relation {
                Relation::DependsOn => {
                    if let Some(d) = declared.iter_mut().find(|d| d.gradle == tie.target) {
                        d.depends_on.extend(tie.refs);
                    }
                }
                Relation::RunsAfter => {}
                Relation::FinalizedBy => fail(
                    &mut declared,
                    &tie.target,
                    format!("`{}` has no equivalent in jrs", tie.text),
                ),
            }
            continue;
        }
        if tie.relation == Relation::RunsAfter {
            continue;
        }
        match hook_for(&tie.target, tie.relation) {
            Some(hook) => hooked.extend(tie.refs.into_iter().map(|r| (hook, r, tie.text.clone()))),
            None => report.skipped(format!(
                "`{}` — jrs has no hook that runs where Gradle's `{}` does; run the task \
                 with `jrs task`, or hook it by hand",
                tie.text, tie.target
            )),
        }
    }

    // A task that depends on one left out goes too, and so on up.
    while let Some((gradle, missing)) = declared.iter().find_map(|d| {
        d.depends_on
            .iter()
            .find(|r| builtins_for(r).is_none() && !declared.iter().any(|o| o.gradle == **r))
            .map(|r| (d.gradle.clone(), r.clone()))
    }) {
        let why = if custom.contains(&missing) {
            format!("it depends on `{missing}`, which was not migrated")
        } else {
            format!("it depends on `{missing}`, Gradle's own task, which jrs has no equivalent of")
        };
        fail(&mut declared, &gradle, why);
    }
    for (gradle, why) in &failed {
        report.skipped(format!("task `{gradle}` — {why}"));
    }

    link_depends_on(&mut declared, report);
    apply_hooks(&mut declared, hooked, &custom, out, report);
    out.tasks = declared.into_iter().map(|d| d.def).collect();

    // What the translation cannot rule out, `task::check` can: a manifest jrs
    // would refuse to load is never written.
    if let Err(e) = crate::task::check(out) {
        report.skipped(format!(
            "[hooks] — {e}; left out, so hook the tasks by hand"
        ));
        out.hooks = Hooks::default();
        if let Err(e) = crate::task::check(out) {
            report.skipped(format!("[tasks] — {e}; left out"));
            out.tasks.clear();
        }
    }
}

/// The script's task declarations, translated, and those that could not be,
/// with why.
fn read_declarations(top: &[Stmt]) -> (Vec<Declared>, Vec<(String, String)>) {
    let mut declared: Vec<Declared> = Vec::new();
    let mut failed: Vec<(String, String)> = Vec::new();
    for stmt in top {
        let Some((gradle, header)) = declaration(&stmt.head) else {
            continue;
        };
        match header.and_then(|h| translate(&gradle, h, stmt.block.as_deref())) {
            Ok(d) if declared.iter().any(|o| o.def.name == d.def.name) => failed.push((
                gradle,
                format!("its name would be `{}`, which another task has", d.def.name),
            )),
            Ok(d) => declared.push(d),
            Err(why) => failed.push((gradle, why)),
        }
    }
    (declared, failed)
}

/// The ties written apart from the declarations, and the `custom` tasks
/// configured again after theirs, with the statement that does it.
fn read_ties(
    top: &[Stmt],
    custom: &[String],
    report: &mut Report,
) -> (Vec<Tie>, Vec<(String, String)>) {
    let mut ties = Vec::new();
    let mut reconfigured = Vec::new();
    for stmt in top.iter().filter(|s| declaration(&s.head).is_none()) {
        let Some((target, rest)) = configured(&stmt.head) else {
            continue;
        };
        let is_custom = custom.contains(&target);
        if !is_custom && !GRADLE_TASKS.contains(&target.as_str()) {
            continue;
        }
        let mut found: Vec<(Relation, String, String)> = Vec::new();
        if let Some((rel, arg)) = relation(rest) {
            found.push((rel, arg.to_string(), stmt.head.clone()));
        } else if is_custom && !rest.is_empty() {
            reconfigured.push((target.clone(), stmt.head.clone()));
        }
        for inner in stmt.block.as_deref().map(statements).unwrap_or_default() {
            match relation(&inner.head) {
                Some((rel, arg)) if inner.block.is_none() => found.push((
                    rel,
                    arg.to_string(),
                    format!("{} {{ {} }}", stmt.head, inner.head),
                )),
                _ if is_custom => reconfigured.push((target.clone(), stmt.head.clone())),
                _ => {}
            }
        }
        for (relation, arg, text) in found {
            match task_refs(&arg) {
                Ok(refs) => ties.push(Tie {
                    target: target.clone(),
                    relation,
                    refs,
                    text,
                }),
                Err(why) => report.skipped(format!("`{text}` — {why}")),
            }
        }
    }
    (ties, reconfigured)
}

/// Turn each task's Gradle `dependsOn` names into jrs references, now that
/// every task's fate is known, and report it migrated.
fn link_depends_on(declared: &mut [Declared], report: &mut Report) {
    let names: Vec<(String, String)> = declared
        .iter()
        .map(|d| (d.gradle.clone(), d.def.name.clone()))
        .collect();
    for d in declared.iter_mut() {
        let mut refs: Vec<TaskRef> = d.implied.iter().map(|b| TaskRef::Builtin(*b)).collect();
        for r in &d.depends_on {
            let resolved: Vec<TaskRef> = match names.iter().find(|(g, _)| g == r) {
                Some((_, name)) => vec![TaskRef::Task(name.clone())],
                None => builtins_for(r)
                    .unwrap_or_default()
                    .iter()
                    .map(|b| TaskRef::Builtin(*b))
                    .collect(),
            };
            for reference in resolved {
                if !refs.contains(&reference) {
                    refs.push(reference);
                }
            }
        }
        d.def.depends_on = refs;
        report.migrated(format!(
            "task `{}` → [tasks.{}] ({})",
            d.gradle, d.def.name, d.kind
        ));
        for line in d.review.drain(..) {
            report.review(line);
        }
    }
}

/// Add the `hooked` tasks to `out.hooks`, reporting those that were not
/// migrated or would do nothing there.
fn apply_hooks(
    declared: &mut [Declared],
    hooked: Vec<(Hook, String, String)>,
    custom: &[String],
    out: &mut Manifest,
    report: &mut Report,
) {
    for (hook, gradle, text) in hooked {
        let Some(d) = declared.iter_mut().find(|d| d.gradle == gradle) else {
            let why = if custom.contains(&gradle) {
                format!("`{gradle}` was not migrated")
            } else {
                format!("`{gradle}` is Gradle's own task, and a jrs hook runs only tasks")
            };
            report.skipped(format!("`{text}` — {why}"));
            continue;
        };
        // `jar.finalizedBy checksum`, with `checksum.dependsOn jar`: the hook
        // already runs it after the jar, and keeping the dependency too would
        // be a cycle through `package`.
        let kept: Vec<TaskRef> = d
            .def
            .depends_on
            .iter()
            .filter(|t| !matches!(t, TaskRef::Builtin(b) if b.hooks().contains(&hook)))
            .cloned()
            .collect();
        let name = d.def.name.clone();
        if kept.len() != d.def.depends_on.len() {
            if d.def.action.is_none() && kept.is_empty() {
                report.skipped(format!(
                    "`{text}` — [tasks.{name}] only depends on the command that fires \
                     hooks.{hook}, so hooking it there would do nothing"
                ));
                continue;
            }
            d.def.depends_on = kept;
            report.review(format!(
                "[tasks.{name}] — no longer depends on the command hooks.{hook} follows; \
                 the hook runs it after that command, but `jrs task {name}` alone does not \
                 run the command first"
            ));
        }
        if hook == Hook::PostTest {
            report.review(format!(
                "hooks.post-test runs [tasks.{name}] only when the tests pass; Gradle's \
                 `finalizedBy` runs it either way"
            ));
        }
        out.hooks.add(hook, &name);
        report.migrated(format!("`{text}` → hooks.{hook} = [\"{name}\"]"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// The tasks and hooks `script` becomes, rendered, and the report.
    fn migrate(script: &str) -> (Manifest, Report) {
        let mut out = crate::manifest::blank("app", "1.0.0", Path::new("/p"));
        let mut report = Report::default();
        read(script, &mut out, &mut report);
        // Whatever comes out must be a manifest jrs loads.
        Manifest::parse(&out.render(None), Path::new("/p/jrs.toml"), Path::new("/p")).unwrap();
        (out, report)
    }

    fn run(m: &Manifest, name: &str) -> Vec<String> {
        match &m.task(name).unwrap().action {
            Some(Action::Run(argv)) => argv.iter().map(|t| t.raw.clone()).collect(),
            other => panic!("{name}: {other:?}"),
        }
    }

    fn depends_on(m: &Manifest, name: &str) -> Vec<String> {
        m.task(name)
            .unwrap()
            .depends_on
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    #[test]
    fn the_kotlin_dsl_spellings_are_read() {
        let (m, report) = migrate(
            r#"
val generateSources by tasks.registering(Exec::class) {
    commandLine("protoc", "--java_out=target/gen", "src/main/proto/a.proto")
    workingDir = file("tools")
}
tasks.register<JavaExec>("runTool") {
    classpath = sourceSets["main"].runtimeClasspath
    mainClass.set("com.example.Tool")
    args("--verbose")
}
tasks.register("all") { dependsOn(generateSources, tasks.named("runTool")) }
tasks.named("compileJava") { dependsOn(generateSources) }
tasks.test { finalizedBy("runTool") }
"#,
        );
        assert_eq!(
            run(&m, "generate-sources"),
            ["protoc", "--java_out=target/gen", "src/main/proto/a.proto"]
        );
        assert_eq!(
            m.task("generate-sources")
                .unwrap()
                .cwd
                .as_ref()
                .unwrap()
                .raw,
            "tools"
        );
        assert_eq!(
            run(&m, "run-tool"),
            [
                "java",
                "@{classpath-argfile}",
                "com.example.Tool",
                "--verbose"
            ]
        );
        assert_eq!(depends_on(&m, "run-tool"), ["build"]);
        assert_eq!(depends_on(&m, "all"), ["generate-sources", "run-tool"]);
        assert_eq!(m.hooks.tasks(Hook::PreCompile), ["generate-sources"]);
        assert_eq!(m.hooks.tasks(Hook::PostTest), ["run-tool"]);
        let review = report.needs_review.join("\n");
        assert!(review.contains("only when the tests pass"), "{review}");
        assert!(report.not_migrated.is_empty(), "{:?}", report.not_migrated);
    }

    #[test]
    fn one_line_blocks_and_groovy_declarations_are_read() {
        let (m, _) = migrate(
            "task lint(type: Exec, dependsOn: ['classes'], description: 'Lint') { \
             executable 'checkstyle'; args '-c', 'rules.xml' }\n\
             task('fmt', type: Exec) { commandLine 'gjf', '--replace' }\n",
        );
        assert_eq!(run(&m, "lint"), ["checkstyle", "-c", "rules.xml"]);
        assert_eq!(depends_on(&m, "lint"), ["build"]);
        assert_eq!(m.task("lint").unwrap().description.as_deref(), Some("Lint"));
        assert_eq!(run(&m, "fmt"), ["gjf", "--replace"]);
    }

    #[test]
    fn a_task_jrs_cannot_read_whole_is_left_out_with_the_reason() {
        let (m, report) = migrate(
            "tasks.register('stamp', Exec) { commandLine 'echo', \"v$version\" }\n\
             tasks.register('lenient', Exec) { commandLine 'true'; ignoreExitValue = true }\n\
             tasks.register('wrap', Exec) { commandLine 'x'; standardOutput = new FileOutputStream('o') }\n\
             tasks.register('after') { dependsOn 'stamp' }\n\
             tasks.register('clean2') { dependsOn 'clean' }\n",
        );
        assert!(m.tasks.is_empty(), "{:?}", m.tasks);
        let skipped = report.not_migrated.join("\n");
        assert!(
            skipped.contains("task `stamp` — `\"v$version\"` is interpolated"),
            "{skipped}"
        );
        assert!(
            skipped.contains("task `lenient` — it ignores the exit code"),
            "{skipped}"
        );
        assert!(
            skipped.contains("task `wrap` — `standardOutput"),
            "{skipped}"
        );
        assert!(
            skipped.contains("task `after` — it depends on `stamp`"),
            "{skipped}"
        );
        assert!(
            skipped.contains("task `clean2` — it depends on `clean`, Gradle's own"),
            "{skipped}"
        );
    }

    #[test]
    fn braces_in_a_command_stay_literal() {
        let (m, _) = migrate("tasks.register('tpl', Exec) { commandLine 'echo', '{root}' }\n");
        assert_eq!(run(&m, "tpl"), ["echo", "{{root}}"]);
        assert!(
            m.task("tpl")
                .unwrap()
                .templates()
                .all(|(_, t)| t.placeholders().next().is_none())
        );
    }

    #[test]
    fn a_hook_names_tasks_not_gradles_own() {
        let (m, report) = migrate("test.dependsOn 'jar'\nclasses.dependsOn missing\n");
        assert!(m.hooks.is_empty());
        let skipped = report.not_migrated.join("\n");
        assert!(skipped.contains("`jar` is Gradle's own task"), "{skipped}");
        assert!(
            skipped.contains("`missing` is Gradle's own task"),
            "{skipped}"
        );
    }

    #[test]
    fn a_task_configured_twice_is_left_out() {
        let (m, report) = migrate(
            "tasks.register('gen', Exec) { commandLine 'gen' }\n\
             tasks.named('gen') { environment 'A', 'b' }\n",
        );
        assert!(m.tasks.is_empty());
        let skipped = report.not_migrated.join("\n");
        assert!(skipped.contains("configured again"), "{skipped}");
    }

    #[test]
    fn gradle_names_become_jrs_names() {
        assert_eq!(
            task_name("generateBuildInfo").unwrap(),
            "generate-build-info"
        );
        assert_eq!(task_name("HTMLReport").unwrap(), "html-report");
        assert_eq!(task_name("copy_jars2").unwrap(), "copy-jars2");
        assert_eq!(task_name("lint").unwrap(), "lint");
        assert!(task_name("run").unwrap_err().contains("jrs command"));
        assert!(task_name("2fast").is_err());
    }

    #[test]
    fn literals_decode_escapes_and_spot_templates() {
        let l = literal(r#""a\"b""#).unwrap();
        assert_eq!(l.text, "a\"b");
        assert!(!l.interpolated);
        assert!(literal("\"${x}\"").unwrap().interpolated);
        assert!(
            !literal("'$x'").unwrap().interpolated,
            "single quotes do not interpolate"
        );
        assert!(literal("'a' + 'b'").is_none());
        assert!(literal("name").is_none());
    }
}
