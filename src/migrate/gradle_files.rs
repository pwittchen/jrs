//! Gradle's `files(...)` and `fileTree(...)` in a `dependencies { }` block:
//! jars kept in the project, which become local jars in `jrs.toml`
//! (`name = { path = "libs/driver.jar" }`).
//!
//! Only literals translate. `jrs.toml` names jars, not directories, so a
//! `fileTree` is expanded to the jars it holds when jrs migrates, one entry
//! each, and the report says so: a jar added there later needs an entry of its
//! own. A local jar is named after its file.

use std::path::Path;

use super::Report;
use crate::manifest::Dependency;

/// The local jars a dependency line names, when it is a `files(...)` or
/// `fileTree(...)` one: `None` for any other line, and an empty list when it
/// is one but nothing in it could be translated, which is then reported.
pub fn read(line: &str, root: &Path, report: &mut Report) -> Option<Vec<Dependency>> {
    if let Some((inner, rest)) = call_arguments(line, "fileTree(") {
        return Some(file_tree(line, inner, rest, root, report));
    }
    let (inner, _) = call_arguments(line, "files(")?;
    let Some(paths) = literals_only(inner) else {
        report.skipped(format!(
            "`{line}` — `files(...)` built from a variable or an expression, which jrs \
             cannot evaluate without running Gradle"
        ));
        return Some(Vec::new());
    };
    Some(
        paths
            .iter()
            .filter_map(|path| local_jar(line, path, root, report))
            .collect(),
    )
}

/// `fileTree(dir: 'libs', include: ['*.jar'])`, `fileTree('libs')`, or the
/// Kotlin DSL's `fileTree(mapOf("dir" to "libs", "include" to listOf("*.jar")))`.
fn file_tree(
    line: &str,
    inner: &str,
    rest: &str,
    root: &Path,
    report: &mut Report,
) -> Vec<Dependency> {
    let unreadable = |report: &mut Report, why: &str| {
        report.skipped(format!("`{line}` — {why}; list the jars with `files(...)`"));
        Vec::new()
    };
    if rest.contains('{') {
        return unreadable(
            report,
            "a `fileTree` closure, which jrs cannot evaluate without running Gradle",
        );
    }
    let Some(segments) = segments(inner) else {
        return unreadable(
            report,
            "a `fileTree` built from a variable or an expression",
        );
    };
    let mut dir = None;
    let mut includes = Vec::new();
    for (key, literals) in &segments {
        match key.as_str() {
            "" | "dir" if literals.len() == 1 && dir.is_none() => dir = Some(literals[0].clone()),
            "" if literals.is_empty() => {}
            "include" | "includes" => includes.extend(literals.iter().cloned()),
            other => {
                let what = if other.is_empty() { "arguments" } else { other };
                return unreadable(
                    report,
                    &format!(
                        "its `{what}` is more than jrs reads of a `fileTree` (a `dir` and `*.jar` includes)"
                    ),
                );
            }
        }
    }
    let Some(dir) = dir else {
        return unreadable(report, "a `fileTree` without a literal `dir`");
    };
    // Ant patterns: `*.jar` is the directory itself, `**/*.jar` everything
    // under it. With no include, Gradle takes every file; jrs takes the jars.
    let recursive = match includes
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        [] => true,
        patterns if patterns.iter().all(|p| *p == "*.jar" || *p == "**/*.jar") => {
            patterns.contains(&"**/*.jar")
        }
        _ => {
            return unreadable(
                report,
                "an `include` pattern other than `*.jar` or `**/*.jar`",
            );
        }
    };
    let dir = dir.replace('\\', "/").trim_end_matches('/').to_string();
    if escapes(&dir) {
        report.skipped(format!(
            "`{line}` — `{dir}` is outside the project; jrs.toml names jars inside it"
        ));
        return Vec::new();
    }
    if !root.join(&dir).is_dir() {
        report.skipped(format!(
            "`{line}` — `{dir}` does not exist, so there are no jars to list"
        ));
        return Vec::new();
    }
    let mut paths = Vec::new();
    jars_in(&root.join(&dir), recursive, &dir, &mut paths);
    if paths.is_empty() {
        report.skipped(format!("`{line}` — `{dir}` holds no jars"));
        return Vec::new();
    }
    report.review(format!(
        "`{line}` — expanded to the {} jars in {dir}/ as it is now; a jar added there \
         later needs an entry of its own in jrs.toml",
        paths.len()
    ));
    paths
        .into_iter()
        .map(|path| Dependency::local(jar_name(&path), path))
        .collect()
}

/// One jar of a `files(...)`, as a local jar, unless it cannot be one.
fn local_jar(line: &str, raw: &str, root: &Path, report: &mut Report) -> Option<Dependency> {
    let path = raw.replace('\\', "/");
    let path = path.trim_start_matches("./").to_string();
    if escapes(&path) {
        report.skipped(format!(
            "`{line}` — `{raw}` is outside the project; jrs.toml names jars inside it"
        ));
        return None;
    }
    let on_disk = root.join(&path);
    if on_disk.is_dir() {
        report.skipped(format!(
            "`{line}` — `{raw}` is a directory; jrs takes jars, one entry per jar"
        ));
        return None;
    }
    if !on_disk.is_file() {
        report.review(format!(
            "`{line}` — `{path}` is not there now; jrs builds fail until it is"
        ));
    }
    Some(Dependency::local(jar_name(&path), path))
}

/// Whether a path written in the build leaves the project.
fn escapes(path: &str) -> bool {
    let p = Path::new(path);
    path.is_empty()
        || p.is_absolute()
        || p.has_root()
        || path.starts_with('/')
        || path.contains(':')
        || path.split('/').any(|c| c == "..")
}

/// `libs/ojdbc11.jar` → `ojdbc11`: the file's name, as a manifest key.
fn jar_name(path: &str) -> String {
    let file = path.rsplit('/').next().unwrap_or(path);
    let stem = file
        .strip_suffix(".jar")
        .or_else(|| file.strip_suffix(".JAR"))
        .unwrap_or(file);
    let name: String = stem
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .collect();
    if name.is_empty() {
        "local".to_string()
    } else {
        name
    }
}

/// The jars in `dir`, in sorted order, as `prefix/<name>` paths.
fn jars_in(dir: &Path, recursive: bool, prefix: &str, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<_> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
    paths.sort();
    for path in paths {
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let relative = format!("{prefix}/{name}");
        if path.is_dir() {
            if recursive {
                jars_in(&path, true, &relative, out);
            }
        } else if name.to_ascii_lowercase().ends_with(".jar") {
            out.push(relative);
        }
    }
}

/// The text between the parentheses of the first call to `name` (which ends
/// in `(`) that is not the tail of a longer identifier, and what follows the
/// closing one.
fn call_arguments<'a>(line: &'a str, name: &str) -> Option<(&'a str, &'a str)> {
    let mut from = 0;
    while let Some(at) = line[from..].find(name).map(|a| a + from) {
        let before = line[..at].chars().next_back();
        if before.is_some_and(|c| c.is_alphanumeric() || c == '_') {
            from = at + name.len();
            continue;
        }
        let start = at + name.len();
        let mut depth = 1;
        let mut quote: Option<char> = None;
        for (i, c) in line[start..].char_indices() {
            match (quote, c) {
                (Some(q), c) if c == q => quote = None,
                (None, '\'' | '"') => quote = Some(c),
                (None, '(') => depth += 1,
                (None, ')') => {
                    depth -= 1;
                    if depth == 0 {
                        return Some((&line[start..start + i], &line[start + i + 1..]));
                    }
                }
                _ => {}
            }
        }
        return None;
    }
    None
}

/// Quoted literals separated by commas, and nothing else; `None` for anything
/// computed, an interpolated `$` included.
fn literals_only(inner: &str) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        match c {
            '\'' | '"' => {
                let mut value = String::new();
                loop {
                    match chars.next() {
                        Some(x) if x == c => break,
                        Some(x) => value.push(x),
                        None => return None,
                    }
                }
                if value.contains('$') {
                    return None;
                }
                out.push(value);
            }
            c if c.is_whitespace() || c == ',' => {}
            _ => return None,
        }
    }
    (!out.is_empty()).then_some(out)
}

/// A call's arguments as `(key, literals)` runs: `dir: 'libs'` and
/// `"dir" to "libs"` give `("dir", ["libs"])`; literals before any key go
/// under `""`. `None` when a literal is interpolated or unclosed.
fn segments(inner: &str) -> Option<Vec<(String, Vec<String>)>> {
    let chars: Vec<char> = inner.chars().collect();
    let mut out: Vec<(String, Vec<String>)> = vec![(String::new(), Vec::new())];
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\'' || c == '"' {
            let mut j = i + 1;
            let mut value = String::new();
            while j < chars.len() && chars[j] != c {
                value.push(chars[j]);
                j += 1;
            }
            if j >= chars.len() || value.contains('$') {
                return None;
            }
            i = j + 1;
            if let Some(next) = separator(&chars, i) {
                out.push((value, Vec::new()));
                i = next;
            } else if let Some(last) = out.last_mut() {
                last.1.push(value);
            }
            continue;
        }
        if c.is_alphanumeric() || c == '_' {
            let start = i;
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            if word != "to"
                && let Some(next) = separator(&chars, i)
            {
                out.push((word, Vec::new()));
                i = next;
            }
            continue;
        }
        i += 1;
    }
    Some(out)
}

/// What follows a key: `:`, `=` (not `==`), or Kotlin's infix `to`. The index
/// just past it.
fn separator(chars: &[char], mut i: usize) -> Option<usize> {
    while i < chars.len() && chars[i].is_whitespace() {
        i += 1;
    }
    match chars.get(i) {
        Some(':') => Some(i + 1),
        Some('=') if chars.get(i + 1) != Some(&'=') => Some(i + 1),
        Some('t')
            if chars.get(i + 1) == Some(&'o')
                && chars.get(i + 2).is_none_or(|c| !c.is_alphanumeric()) =>
        {
            Some(i + 2)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Dir {
        path: std::path::PathBuf,
    }

    impl Dir {
        fn new(name: &str) -> Dir {
            let path = std::env::temp_dir()
                .join(format!("jrs-gradle-files-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Dir { path }
        }

        fn touch(&self, relative: &str) {
            let file = self.path.join(relative);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, b"").unwrap();
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn paths(deps: &[Dependency]) -> Vec<String> {
        deps.iter()
            .map(|d| format!("{}={}", d.artifact, d.path.as_deref().unwrap()))
            .collect()
    }

    #[test]
    fn files_become_local_jars_named_after_their_file() {
        let dir = Dir::new("files");
        dir.touch("libs/ojdbc11.jar");
        dir.touch("libs/vendor sdk.jar");
        let mut report = Report::default();
        for line in [
            "implementation files('libs/ojdbc11.jar', 'libs/vendor sdk.jar')",
            "implementation(files(\"libs/ojdbc11.jar\", \"libs/vendor sdk.jar\"))",
        ] {
            let deps = read(line, &dir.path, &mut report).unwrap();
            assert_eq!(
                paths(&deps),
                ["ojdbc11=libs/ojdbc11.jar", "vendor-sdk=libs/vendor sdk.jar"]
            );
        }
        assert!(report.needs_review.is_empty(), "{:?}", report.needs_review);
        assert!(read("implementation 'g:a:1'", &dir.path, &mut report).is_none());
        assert!(read("implementation myfiles('x.jar')", &dir.path, &mut report).is_none());
    }

    #[test]
    fn what_files_cannot_say_is_reported() {
        let dir = Dir::new("files-bad");
        std::fs::create_dir_all(dir.path.join("classes")).unwrap();
        let mut report = Report::default();
        for line in [
            "implementation files(\"$buildDir/x.jar\")",
            "implementation files(someJar)",
            "implementation files('../shared/x.jar')",
            "implementation files('/opt/x.jar')",
            "implementation files('classes')",
        ] {
            assert!(
                read(line, &dir.path, &mut report).unwrap().is_empty(),
                "{line}"
            );
        }
        assert_eq!(report.not_migrated.len(), 5, "{:?}", report.not_migrated);

        // A jar that is not there yet still translates, flagged.
        let deps = read(
            "implementation files('libs/later.jar')",
            &dir.path,
            &mut report,
        )
        .unwrap();
        assert_eq!(paths(&deps), ["later=libs/later.jar"]);
        assert!(report.needs_review[0].contains("not there now"));
    }

    #[test]
    fn a_file_tree_expands_to_the_jars_present() {
        let dir = Dir::new("tree");
        dir.touch("libs/b.jar");
        dir.touch("libs/a.jar");
        dir.touch("libs/README.txt");
        dir.touch("libs/nested/c.jar");
        let mut report = Report::default();
        let top = ["a=libs/a.jar", "b=libs/b.jar"];
        let all = ["a=libs/a.jar", "b=libs/b.jar", "c=libs/nested/c.jar"];
        for (line, expected) in [
            (
                "implementation fileTree(dir: 'libs', include: ['*.jar'])",
                &top[..],
            ),
            (
                "implementation fileTree(dir: 'libs', include: '*.jar')",
                &top[..],
            ),
            (
                "implementation(fileTree(mapOf(\"dir\" to \"libs\", \"include\" to listOf(\"*.jar\"))))",
                &top[..],
            ),
            (
                "implementation fileTree(dir: 'libs', include: ['**/*.jar'])",
                &all[..],
            ),
            ("implementation fileTree('libs')", &all[..]),
        ] {
            let deps = read(line, &dir.path, &mut report).unwrap();
            assert_eq!(paths(&deps), expected, "{line}");
        }
        assert!(
            report
                .needs_review
                .iter()
                .all(|r| r.contains("as it is now")),
            "{:?}",
            report.needs_review
        );
    }

    #[test]
    fn a_file_tree_jrs_cannot_read_is_reported() {
        let dir = Dir::new("tree-bad");
        dir.touch("libs/a.jar");
        let mut report = Report::default();
        for line in [
            "implementation fileTree(dir: 'libs', include: ['*.jar'], exclude: ['a.jar'])",
            "implementation fileTree(dir: 'libs', include: ['vendor-*.jar'])",
            "implementation fileTree('libs') {",
            "implementation fileTree(dir: libsDir, include: ['*.jar'])",
            "implementation fileTree(dir: 'missing', include: ['*.jar'])",
            "implementation fileTree(dir: '../elsewhere', include: ['*.jar'])",
        ] {
            assert!(
                read(line, &dir.path, &mut report).unwrap().is_empty(),
                "{line}"
            );
        }
        assert_eq!(report.not_migrated.len(), 6, "{:?}", report.not_migrated);
    }
}
