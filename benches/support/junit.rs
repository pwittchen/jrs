//! Test counts from the `JUnit` XML each build tool writes, by class: what
//! `benches/corpus.rs` compares with a project's own build, and what
//! `benches/compare.rs` checks the three tools agree on before it times
//! them (specs/BENCHMARKS.md §5.1).

#![allow(
    dead_code,
    reason = "each benchmark that includes this module uses a part of it"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use jrs::test_report::{self, Status};

/// Where a side writes its `JUnit` XML.
#[derive(Clone, Copy)]
pub enum Reports {
    /// Surefire: `target/surefire-reports`.
    Surefire,
    /// Gradle: `build/test-results/test`.
    Gradle,
    /// jrs: `target/test-reports`.
    Jrs,
}

impl Reports {
    fn holds(self, dir: &Path) -> bool {
        let name = |p: &Path| p.file_name().map(|n| n.to_string_lossy().into_owned());
        match self {
            Reports::Surefire => name(dir).as_deref() == Some("surefire-reports"),
            Reports::Gradle => {
                name(dir).as_deref() == Some("test")
                    && dir.parent().and_then(name).as_deref() == Some("test-results")
            }
            Reports::Jrs => name(dir).as_deref() == Some("test-reports"),
        }
    }
}

#[derive(Default, Clone, Copy, PartialEq, Eq)]
pub struct Counts {
    pub tests: usize,
    pub failed: usize,
    pub skipped: usize,
}

impl Counts {
    pub fn ran(self) -> usize {
        self.tests - self.skipped
    }

    pub fn passed(self) -> usize {
        self.ran() - self.failed
    }
}

impl std::fmt::Display for Counts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} tests, {} failed, {} skipped",
            self.tests, self.failed, self.skipped
        )
    }
}

/// Test counts by class, from every report directory under a tree.
pub struct Tests {
    pub classes: BTreeMap<String, Counts>,
}

impl Tests {
    pub fn collect(root: &Path, reports: Reports, exclude: &[String]) -> Tests {
        let mut classes: BTreeMap<String, Counts> = BTreeMap::new();
        let mut dirs = Vec::new();
        report_dirs(root, reports, &mut dirs);
        for dir in dirs {
            let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
                .into_iter()
                .flatten()
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name().is_some_and(|n| {
                        let n = n.to_string_lossy();
                        n.starts_with("TEST-") && n.ends_with(".xml")
                    })
                })
                .collect();
            files.sort();
            for file in files {
                let Ok(bytes) = std::fs::read(&file) else {
                    continue;
                };
                let Ok(cases) = test_report::parse_report(&bytes) else {
                    continue;
                };
                for case in cases {
                    let outer = case.class.split('$').next().unwrap_or(&case.class);
                    if exclude.iter().any(|x| x == outer) {
                        continue;
                    }
                    let counts = classes.entry(outer.to_string()).or_default();
                    counts.tests += 1;
                    match case.status {
                        Status::Failed => counts.failed += 1,
                        Status::Skipped => counts.skipped += 1,
                        Status::Flaky | Status::Passed => {}
                    }
                }
            }
        }
        Tests { classes }
    }

    pub fn total(&self) -> Counts {
        self.classes
            .values()
            .fold(Counts::default(), |t, c| Counts {
                tests: t.tests + c.tests,
                failed: t.failed + c.failed,
                skipped: t.skipped + c.skipped,
            })
    }

    /// One line per class whose counts differ, or which one side alone ran.
    pub fn compare(&self, reference: &Tests) -> Vec<String> {
        let names: BTreeSet<&String> = self
            .classes
            .keys()
            .chain(reference.classes.keys())
            .collect();
        names
            .into_iter()
            .filter_map(
                |name| match (self.classes.get(name), reference.classes.get(name)) {
                    (Some(o), Some(r)) if o == r => None,
                    (Some(o), Some(r)) => Some(format!("~ {name}: jrs {o}; the reference {r}")),
                    (Some(o), None) => Some(format!("+ {name}: jrs alone ran it ({o})")),
                    (None, Some(r)) => Some(format!("- {name}: the reference alone ran it ({r})")),
                    (None, None) => None,
                },
            )
            .collect()
    }
}

fn report_dirs(dir: &Path, reports: Reports, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut children: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .filter(|p| {
            p.file_name().is_none_or(|n| {
                ![".git", ".gradle", "node_modules", ".jrs"].contains(&&*n.to_string_lossy())
            })
        })
        .collect();
    children.sort();
    for child in children {
        if reports.holds(&child) {
            out.push(child);
        } else {
            report_dirs(&child, reports, out);
        }
    }
}
