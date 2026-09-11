//! Build timings: the wall time of each phase of one command, for
//! `--timings` on `build`, `test`, `run` and `package`.
//!
//! `Session` in `cli.rs` records a row as each phase ends — resolution,
//! downloads, each step of a compile unit, resources, each task, the test
//! JVM, packaging — so the rows come out in the order the phases ran. Only
//! leaf phases are recorded, never a phase and the phases inside it, so the
//! rows do not overlap and add up to the total less jrs's own bookkeeping.
//! The table on the terminal is `ui`'s to draw; the copy written under
//! `target/.jrs/` is rendered here, since it is a file, not output.

use std::cell::RefCell;
use std::fmt::Write as _;
use std::time::{Duration, Instant};

/// The copy of the report, in the project's `target/.jrs/`.
pub const FILE: &str = "timings.txt";

/// The rows one command has recorded so far.
#[derive(Debug, Default)]
pub struct Timings {
    rows: RefCell<Vec<(String, Duration)>>,
}

impl Timings {
    #[must_use]
    pub fn new() -> Timings {
        Timings::default()
    }

    /// A phase called `label` that took `took`.
    pub fn record(&self, label: impl Into<String>, took: Duration) {
        self.rows.borrow_mut().push((label.into(), took));
    }

    /// A phase called `label` that started at `started` and ends now.
    pub fn since(&self, label: impl Into<String>, started: Instant) {
        self.record(label, started.elapsed());
    }

    /// Every row, in the order the phases ended.
    #[must_use]
    pub fn rows(&self) -> Vec<(String, Duration)> {
        self.rows.borrow().clone()
    }
}

/// The file copy of a report: a comment naming the command, then one
/// tab-separated `phase` / milliseconds row per phase and a `total` row.
/// The columns are fixed, so a script can `cut` it and the section 6
/// benchmark can read it back.
#[must_use]
pub fn render_file(command: &str, rows: &[(String, Duration)], total: Duration) -> String {
    let mut out = format!("# jrs {command} --timings: wall time of each phase, in milliseconds\n");
    out.push_str("phase\tms\n");
    for (label, took) in rows {
        let _ = writeln!(out, "{label}\t{}", took.as_millis());
    }
    let _ = writeln!(out, "total\t{}", total.as_millis());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_keep_the_order_the_phases_ended_in() {
        let timings = Timings::new();
        timings.record("resolution", Duration::from_millis(12));
        timings.since("compile main: javac", Instant::now());
        timings.record("resources main", Duration::from_millis(3));
        let labels: Vec<String> = timings.rows().into_iter().map(|(l, _)| l).collect();
        assert_eq!(
            labels,
            ["resolution", "compile main: javac", "resources main"]
        );
    }

    #[test]
    fn the_file_has_fixed_tab_separated_columns() {
        let rows = vec![
            (
                "resolution (jrs.lock)".to_string(),
                Duration::from_micros(2_400),
            ),
            (
                "compile main: javac".to_string(),
                Duration::from_millis(1_234),
            ),
            (
                "task build-info (pre-compile)".to_string(),
                Duration::from_millis(90),
            ),
        ];
        assert_eq!(
            render_file("build", &rows, Duration::from_millis(1_400)),
            concat!(
                "# jrs build --timings: wall time of each phase, in milliseconds\n",
                "phase\tms\n",
                "resolution (jrs.lock)\t2\n",
                "compile main: javac\t1234\n",
                "task build-info (pre-compile)\t90\n",
                "total\t1400\n",
            )
        );
    }
}
