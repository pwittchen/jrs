//! The critical path through a workspace's members (specs/todo/GRADLE_GAPS.md
//! §2.1): how long a clean build of it would take with unlimited cores, from
//! each member's wall time in `--timings`.
//!
//! A member's time is its `member <name>` row, which jrs records when members
//! build at once, or else the sum of its `<name>: …` phase rows, which a build
//! one member at a time records. `sequential` is the sum of them, what a
//! build one member at a time takes; `critical path` is each member's time
//! plus its slowest upstream member's path, at most. The report sets them
//! beside jrs's own clean build and Gradle's `--parallel` one.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;
use std::time::Duration;

use super::{Column, Outcome};

/// Each member of the workspace at `root`, by name, with the names of the
/// members it depends on; empty when `root` is not a workspace jrs reads.
pub fn member_graph(root: &Path) -> Vec<(String, Vec<String>)> {
    let Ok(Some((workspace, _))) = jrs::workspace::Workspace::find(&root.join("jrs.toml")) else {
        return Vec::new();
    };
    workspace
        .order
        .iter()
        .map(|&i| {
            let name = workspace.members[i].manifest.name.clone();
            let upstream = workspace
                .upstream(i)
                .into_iter()
                .map(|(u, _)| workspace.members[u].manifest.name.clone())
                .collect();
            (name, upstream)
        })
        .collect()
}

/// Each member's wall time in one sample's `--timings` rows.
fn member_times(
    graph: &[(String, Vec<String>)],
    phases: &[(String, u64)],
) -> BTreeMap<String, u64> {
    graph
        .iter()
        .map(|(name, _)| {
            let own = format!("member {name}");
            let prefix = format!("{name}: ");
            let at_once: u64 = phases
                .iter()
                .filter(|(l, _)| *l == own)
                .map(|(_, ms)| ms)
                .sum();
            let time = if at_once > 0 {
                at_once
            } else {
                phases
                    .iter()
                    .filter(|(l, _)| l.starts_with(&prefix))
                    .map(|(_, ms)| ms)
                    .sum()
            };
            (name.clone(), time)
        })
        .collect()
}

/// The sum of the members' times, and the longest path through them.
/// `graph` is in build order, so every member's upstream is reckoned first.
pub fn sequential_and_critical(
    graph: &[(String, Vec<String>)],
    times: &BTreeMap<String, u64>,
) -> (u64, u64) {
    let mut path: BTreeMap<&str, u64> = BTreeMap::new();
    for (name, upstream) in graph {
        let before = upstream
            .iter()
            .filter_map(|u| path.get(u.as_str()))
            .max()
            .copied()
            .unwrap_or(0);
        path.insert(name, before + times.get(name).copied().unwrap_or(0));
    }
    let sequential = times.values().sum();
    (sequential, path.values().max().copied().unwrap_or(0))
}

fn median(mut values: Vec<u64>) -> Option<u64> {
    values.sort_unstable();
    values.get(values.len() / 2).copied()
}

fn median_wall(outcome: &Outcome, column: Column) -> Option<Duration> {
    let cell = outcome.cells.get("clean")?.get(&column)?;
    cell.stats().map(|(_, median, _)| median)
}

/// The report's section: one row per workspace whose `clean` jrs ran.
pub fn report(out: &mut String, results: &[Outcome]) {
    let mut rows = Vec::new();
    for outcome in results
        .iter()
        .filter(|r| r.refused.is_none() && !r.member_graph.is_empty())
    {
        let Some(cell) = outcome
            .cells
            .get("clean")
            .and_then(|c| c.get(&Column::Jrs))
            .filter(|c| c.timed())
        else {
            continue;
        };
        let (mut sequential, mut critical) = (Vec::new(), Vec::new());
        for sample in &cell.samples {
            let times = member_times(&outcome.member_graph, &sample.phases);
            let (s, c) = sequential_and_critical(&outcome.member_graph, &times);
            sequential.push(s);
            critical.push(c);
        }
        let (Some(sequential), Some(critical)) = (median(sequential), median(critical)) else {
            continue;
        };
        let jrs = median_wall(outcome, Column::Jrs);
        let gradle = median_wall(outcome, Column::GradleParallel);
        let members = outcome.member_graph.len();
        #[allow(
            clippy::cast_precision_loss,
            reason = "milliseconds of a build, far below 2^52"
        )]
        let saving = if sequential == 0 {
            0.0
        } else {
            (sequential - critical) as f64 / sequential as f64
        };
        let gradle_wins = match (jrs, gradle) {
            (Some(jrs), Some(gradle)) => gradle.as_secs_f64() < jrs.as_secs_f64() / 1.2,
            _ => false,
        };
        let gate = if (members >= 4 && saving >= 0.25) || gradle_wins {
            "holds"
        } else {
            "does not hold"
        };
        let shown = |d: Option<Duration>| {
            d.map_or_else(|| "—".to_string(), |d| format!("{} ms", d.as_millis()))
        };
        rows.push(format!(
            "| `{}` | {members} | {sequential} ms | {critical} ms ({:.0}% less) | {} | {} | {gate} |",
            outcome.name,
            saving * 100.0,
            shown(jrs),
            shown(gradle),
        ));
    }
    if rows.is_empty() {
        return;
    }
    out.push_str(
        "## Workspace members at once\n\n\
         specs/todo/GRADLE_GAPS.md §2.1: each member's wall time in jrs's clean build, \
         summed (`sequential`) and along the longest path through the member graph \
         (`critical path`, the floor with unlimited cores), beside jrs's and Gradle's \
         `--parallel` clean builds. The gate holds when the critical path is at least \
         25% below the sum on four or more members, or Gradle `--parallel` beats jrs \
         by more than 20%. Medians, in milliseconds.\n\n\
         | Project | Members | Sequential | Critical path | jrs | Gradle (daemon, --parallel) | Gate |\n\
         | --- | --- | --- | --- | --- | --- | --- |\n",
    );
    for row in rows {
        let _ = writeln!(out, "{row}");
    }
    out.push('\n');
}
