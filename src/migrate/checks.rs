//! Formatter and checker plugins → tasks (`crate::checkers`): Spotless with
//! google-java-format or ktfmt, `fmt-maven-plugin`, Checkstyle, PMD and
//! `SpotBugs`. Each becomes a `main` task over a graph of its own, a checker
//! hooked `post-compile`, which is where a Gradle `check` or a Maven
//! `verify` would have run it.

use super::Report;
use crate::checkers::{Scaffolded, Tool};
use crate::compile::lang::Language;
use crate::error::Result;
use crate::manifest::{Hook, Manifest};

/// The version to pin: the one the build names, or the default.
pub(super) fn version(tool: Tool, given: Option<String>) -> String {
    given
        .filter(|v| !v.trim().is_empty() && !v.contains('$'))
        .unwrap_or_else(|| tool.default_version().to_string())
}

/// The Java source directories of the main and test units that exist, as
/// the manifest writes them; the main one when neither does yet.
pub(super) fn java_dirs(out: &Manifest) -> Vec<String> {
    let dirs = existing(out, [out.source_dir.clone(), out.test_dir.clone()]);
    if dirs.is_empty() {
        vec![out.source_dir.to_string_lossy().replace('\\', "/")]
    } else {
        dirs
    }
}

/// The Kotlin source directories that exist; the main one when neither
/// does yet.
pub(super) fn kotlin_dirs(out: &Manifest) -> Vec<String> {
    let (main, test) = out.language(Language::Kotlin).map_or_else(
        || ("src/main/kotlin".into(), "src/test/kotlin".into()),
        |k| (k.source_dir.clone(), k.test_dir.clone()),
    );
    let dirs = existing(out, [main.clone(), test]);
    if dirs.is_empty() {
        vec![main.to_string_lossy().replace('\\', "/")]
    } else {
        dirs
    }
}

fn existing(out: &Manifest, dirs: impl IntoIterator<Item = std::path::PathBuf>) -> Vec<String> {
    dirs.into_iter()
        .filter(|d| out.root.join(d).is_dir())
        .map(|d| d.to_string_lossy().replace('\\', "/"))
        .collect()
}

/// Add `scaffolded` to `out`, hooking the checks, unless the build already
/// had a task of the same name; `from` names the plugin in the report.
pub(super) fn add(
    out: &mut Manifest,
    scaffolded: Result<Vec<Scaffolded>>,
    tool: Tool,
    from: &str,
    report: &mut Report,
) {
    let scaffolded = match scaffolded {
        Ok(s) => s,
        Err(e) => {
            report.skipped(format!("{from} — {e}"));
            return;
        }
    };
    if let Some(taken) = scaffolded
        .iter()
        .find(|s| out.tasks.iter().any(|t| t.name == s.task.name))
    {
        report.skipped(format!(
            "{from} — a task named `{}` is already there",
            taken.task.name
        ));
        return;
    }
    let mut names = Vec::new();
    for Scaffolded { task, hooked } in scaffolded {
        if hooked {
            out.hooks.add(Hook::PostCompile, &task.name);
            names.push(format!("`{}` (post-compile)", task.name));
        } else {
            names.push(format!("`{}`", task.name));
        }
        out.tasks.push(task);
    }
    report.migrated(format!(
        "{from} → {} task {}, {}",
        tool.name(),
        names.join(" and "),
        "its jars resolved and pinned by jrs"
    ));
}

/// An Ant pattern of the paths to leave out (`**/.git/**`, `**/*.class`),
/// relative to the project root, as a regular expression Checkstyle's `-x`
/// matches against each file's and directory's absolute path, in template
/// text. It is anchored at `{root}`: unanchored, `**/target/**` would leave
/// out every file of a project that is itself under a `target` directory.
/// A directory pattern matches the directory, so Checkstyle never walks it.
/// `None` for a pattern that cannot be translated.
pub(super) fn ant_exclusion(pattern: &str) -> Option<String> {
    const SEP: &str = r"[/\\]";
    const NAME: &str = r"[^/\\]";
    let pattern = pattern.trim().replace('\\', "/");
    if pattern.is_empty() || pattern.starts_with('/') || pattern.contains('$') {
        return None;
    }
    // Ant reads a trailing `/` as `/**`: the directory and all below it.
    let (rest, below) = ["/**/*", "/**/", "/**", "/"]
        .iter()
        .find_map(|suffix| pattern.strip_suffix(suffix))
        .map_or((pattern.as_str(), false), |rest| (rest, true));
    let mut regex = String::new();
    for segment in rest.split('/') {
        if segment.is_empty() {
            return None;
        }
        if segment == "**" {
            for piece in ["(?:", SEP, NAME, "*)*"] {
                regex.push_str(piece);
            }
            continue;
        }
        regex.push_str(SEP);
        for c in segment.chars() {
            match c {
                '*' => {
                    regex.push_str(NAME);
                    regex.push('*');
                }
                '?' => regex.push_str(NAME),
                c if c.is_alphanumeric() || matches!(c, '_' | '-') => regex.push(c),
                c => {
                    regex.push('\\');
                    regex.push(c);
                }
            }
        }
    }
    regex.push_str(if below { r"(?:[/\\].*)?$" } else { "$" });
    let tail = crate::manifest::Template::literal(&regex).raw;
    Some(format!(r"^\Q{{root}}\E{tail}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ant_exclusions_become_regexes_anchored_at_the_root() {
        assert_eq!(
            ant_exclusion("**/.git/**/*").as_deref(),
            Some(r"^\Q{root}\E(?:[/\\][^/\\]*)*[/\\]\.git(?:[/\\].*)?$")
        );
        assert_eq!(
            ant_exclusion("**/*.class").as_deref(),
            Some(r"^\Q{root}\E(?:[/\\][^/\\]*)*[/\\][^/\\]*\.class$")
        );
        assert_eq!(
            ant_exclusion("**/target/**/"),
            ant_exclusion("**/target/**"),
            "a trailing slash is `**`"
        );
        assert_eq!(
            ant_exclusion("docs/a?.md").as_deref(),
            Some(r"^\Q{root}\E[/\\]docs[/\\]a[^/\\]\.md$")
        );
        for unreadable in ["", "/abs/**", "${dir}/**", "a//b"] {
            assert_eq!(ant_exclusion(unreadable), None, "{unreadable}");
        }
    }
}
