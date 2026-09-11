//! Gradle's `repositories { }` → `[repositories]`, with what `content { }`
//! and `exclusiveContent { }` say about groups turned into `groups`.
//!
//! jrs's `groups` are always exclusive (`resolve::repo::repositories_for`): a
//! group a repository claims is looked up nowhere else. That is exactly what
//! `exclusiveContent` means. A plain `content { includeGroup }` is weaker in
//! Gradle, which still asks the other repositories for those groups, so its
//! translation is flagged for review. Only group filters translate; module,
//! version and exclude filters are reported.

use super::Report;
use super::gradle::{block_lines, quoted, repository_name};
use crate::manifest::{self, Manifest, Repository};

/// Read the script's `repositories { }` into `out.repositories`, Maven Central
/// last.
pub fn read(script: &str, out: &mut Manifest, report: &mut Report) {
    let mut repos: Vec<Repository> = Vec::new();
    for entry in entries(&block_lines(script, "repositories")) {
        let Some(mut repo) = read_entry(&entry, report) else {
            continue;
        };
        if repo.url == manifest::CENTRAL_URL {
            continue;
        }
        let base = repo.name.clone();
        let mut n = 2;
        while repos.iter().any(|r| r.name == repo.name) {
            repo.name = format!("{base}-{n}");
            n += 1;
        }
        repos.push(repo);
    }
    repos.push(Repository::new(
        manifest::CENTRAL_NAME,
        manifest::CENTRAL_URL,
    ));
    out.repositories = repos;
}

/// The block's lines, one group per top-level statement: `mavenCentral()`, a
/// `maven { ... }` block, an `exclusiveContent { ... }` block.
fn entries<'a>(lines: &[&'a str]) -> Vec<Vec<&'a str>> {
    let mut out: Vec<Vec<&str>> = Vec::new();
    let mut depth = 0usize;
    for line in lines {
        if depth == 0 {
            out.push(Vec::new());
        }
        if let Some(entry) = out.last_mut() {
            entry.push(line);
        }
        depth = (depth + line.matches('{').count()).saturating_sub(line.matches('}').count());
    }
    out
}

/// One statement's repository: its URL and the groups its filter includes.
/// `None` for a statement with no URL, such as `mavenCentral()`.
fn read_entry(lines: &[&str], report: &mut Report) -> Option<Repository> {
    let exclusive = lines
        .first()
        .is_some_and(|l| l.trim_start().starts_with("exclusiveContent"));
    let Some(url_line) = lines.iter().map(|l| l.trim()).find(|l| l.contains("url")) else {
        if exclusive {
            report.skipped(
                "`exclusiveContent` for a repository with no `url` — only a `maven { url }` \
                 repository can have `groups` in jrs.toml; Maven Central is asked for \
                 whatever no other repository claims"
                    .to_string(),
            );
        }
        return None;
    };
    let Some(url) = quoted(url_line).into_iter().next() else {
        report.skipped(format!(
            "`{url_line}` — repository URL built from an expression"
        ));
        return None;
    };
    let url = url.trim_end_matches('/').to_string();
    let name = repository_name(&url);
    let (mut groups, approximate) = read_groups(lines, &name, report);

    if !groups.is_empty() && url == manifest::CENTRAL_URL {
        report.skipped(format!(
            "the filter on Maven Central ({}) — jrs asks Central for whatever no other \
             repository claims, so it has no `groups`",
            groups.join(", ")
        ));
        groups.clear();
    }
    if groups.is_empty() {
        report.migrated(format!("repository {url}"));
    } else {
        report.migrated(format!("repository {url}, serving {}", groups.join(", ")));
        if !exclusive {
            report.review(format!(
                "repository `{name}` — Gradle's `content {{ }}` still lets other \
                 repositories serve {}; jrs's `groups` are exclusive, so those are \
                 looked up in {url} only",
                groups.join(", ")
            ));
        }
        for line in approximate {
            report.review(format!(
                "`{line}` in the `{name}` repository's filter — read as the group and the \
                 groups under it; the regex also matched groups that merely start the same"
            ));
        }
    }
    Some(Repository { name, url, groups })
}

/// The group patterns a statement's filter includes, and the regex lines read
/// only approximately. What names more or less than whole groups is reported.
fn read_groups(lines: &[&str], name: &str, report: &mut Report) -> (Vec<String>, Vec<String>) {
    let mut groups: Vec<String> = Vec::new();
    let mut approximate = Vec::new();
    // One statement a piece: `filter { includeGroup 'x' }` is written on one
    // line as often as on three.
    for line in lines
        .iter()
        .flat_map(|l| l.split(['{', '}', ';']))
        .map(str::trim)
    {
        let Some(word) = leading_word(line) else {
            continue;
        };
        let values = quoted(line);
        let read: Option<Vec<String>> = match word.as_str() {
            "includeGroup" => Some(values),
            "includeGroupAndSubgroups" => Some(
                values
                    .iter()
                    .flat_map(|g| [g.clone(), format!("{g}.*")])
                    .collect(),
            ),
            "includeGroupByRegex" => {
                values
                    .first()
                    .and_then(|r| groups_from_regex(r))
                    .map(|(patterns, approx)| {
                        if approx {
                            approximate.push(line.to_string());
                        }
                        patterns
                    })
            }
            w if w.starts_with("include")
                || w.starts_with("exclude")
                || matches!(
                    w,
                    "releasesOnly"
                        | "snapshotsOnly"
                        | "onlyForConfigurations"
                        | "notForConfigurations"
                ) =>
            {
                None
            }
            _ => continue,
        };
        match read.filter(|g| !g.is_empty() && g.iter().all(|p| manifest::valid_group_pattern(p))) {
            Some(patterns) => {
                for p in patterns {
                    if !groups.contains(&p) {
                        groups.push(p);
                    }
                }
            }
            None => report.skipped(format!(
                "`{line}` in the `{name}` repository's filter — jrs.toml's `groups` \
                 include whole groups only"
            )),
        }
    }
    (groups, approximate)
}

/// `includeGroupByRegex` in the shapes that name whole groups: `com\.acme`,
/// `com\.acme\..*` (the groups under it), `com\.acme(\..*)?` (both), and
/// `com\.acme.*`, read as both but flagged, since it also matched `com.acmex`.
/// The flag is the second value.
fn groups_from_regex(raw: &str) -> Option<(Vec<String>, bool)> {
    // Groovy and Kotlin string escapes: `\\.` in the source is the regex `\.`.
    let unescaped = raw.replace("\\\\", "\\");
    let r = unescaped.strip_prefix('^').unwrap_or(&unescaped);
    let r = r.strip_suffix('$').unwrap_or(r);
    if let Some(base) = r.strip_suffix("(\\..*)?") {
        let g = literal_group(base)?;
        return Some((vec![g.clone(), format!("{g}.*")], false));
    }
    if let Some(base) = r.strip_suffix("\\..*") {
        return Some((vec![format!("{}.*", literal_group(base)?)], false));
    }
    if let Some(base) = r.strip_suffix(".*") {
        let g = literal_group(base)?;
        return Some((vec![g.clone(), format!("{g}.*")], true));
    }
    literal_group(r).map(|g| (vec![g], false))
}

/// `com\.acme` → `com.acme`, when every dot is escaped and nothing else is
/// regex syntax.
fn literal_group(escaped: &str) -> Option<String> {
    let segments: Vec<&str> = escaped.split("\\.").collect();
    segments
        .iter()
        .all(|s| {
            !s.is_empty()
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        })
        .then(|| segments.join("."))
}

fn leading_word(line: &str) -> Option<String> {
    let word: String = line
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    (!word.is_empty()).then_some(word)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn read_script(script: &str) -> (Vec<Repository>, Report) {
        let mut out = manifest::blank("app", "1", Path::new("/p"));
        let mut report = Report::default();
        read(script, &mut out, &mut report);
        (out.repositories, report)
    }

    #[test]
    fn content_and_exclusive_content_become_groups() {
        let (repos, report) = read_script(
            "repositories {\n\
             \x20   mavenCentral()\n\
             \x20   maven {\n\
             \x20       url 'https://acme.jfrog.io/artifactory/libs'\n\
             \x20       content {\n\
             \x20           includeGroup 'com.acme'\n\
             \x20           includeGroupByRegex 'com\\\\.acme\\\\..*'\n\
             \x20       }\n\
             \x20   }\n\
             \x20   exclusiveContent {\n\
             \x20       forRepository {\n\
             \x20           maven { url = uri(\"https://jitpack.io\") }\n\
             \x20       }\n\
             \x20       filter { includeGroupAndSubgroups(\"com.github.someone\") }\n\
             \x20   }\n\
             \x20   maven { url 'https://plain.example.com/m2' }\n\
             }\n",
        );
        assert_eq!(repos.len(), 4, "{repos:?}");
        assert_eq!(repos[0].name, "acme");
        assert_eq!(repos[0].groups, ["com.acme", "com.acme.*"]);
        assert_eq!(repos[1].name, "jitpack");
        assert_eq!(repos[1].url, "https://jitpack.io");
        assert_eq!(
            repos[1].groups,
            ["com.github.someone", "com.github.someone.*"]
        );
        assert!(repos[2].groups.is_empty());
        assert_eq!(repos[3].url, manifest::CENTRAL_URL);

        let review = report.needs_review.join("\n");
        assert!(review.contains("repository `acme`"), "{review}");
        assert!(
            !review.contains("jitpack"),
            "exclusiveContent is exact: {review}"
        );
    }

    #[test]
    fn filters_that_are_not_whole_groups_are_reported() {
        let (repos, report) = read_script(
            "repositories {\n  maven {\n    url 'https://vendor.example.com/m2'\n    \
             content {\n      excludeGroup 'org.unwanted'\n      \
             includeModule 'com.vendor', 'sdk'\n      includeGroupByRegex 'com\\\\.v[a-z]+'\n    \
             }\n    mavenContent { releasesOnly() }\n  }\n}\n",
        );
        assert!(repos[0].groups.is_empty());
        let skipped = report.not_migrated.join("\n");
        for directive in [
            "excludeGroup",
            "includeModule",
            "includeGroupByRegex",
            "releasesOnly",
        ] {
            assert!(skipped.contains(directive), "{directive}: {skipped}");
        }
    }

    #[test]
    fn simple_regexes_name_groups() {
        let g = |r: &str| groups_from_regex(r);
        assert_eq!(g("com\\\\.acme"), Some((vec!["com.acme".into()], false)));
        assert_eq!(
            g("^com\\.acme\\..*$"),
            Some((vec!["com.acme.*".into()], false))
        );
        assert_eq!(
            g("com\\.acme(\\..*)?"),
            Some((vec!["com.acme".into(), "com.acme.*".into()], false))
        );
        assert_eq!(
            g("com\\.acme.*"),
            Some((vec!["com.acme".into(), "com.acme.*".into()], true))
        );
        assert_eq!(g("com.acme"), None, "an unescaped dot is any character");
        assert_eq!(g("com\\.(acme|other)"), None);
    }
}
