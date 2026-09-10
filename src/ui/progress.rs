//! The live region's data model, and the pure functions that turn it into lines.
//!
//! Workers only ever mutate the state in here; a single render thread reads it
//! and draws (SPEC §5.3.1). Keeping the line generation pure — state plus glyphs
//! plus a tick plus a width, in; strings, out — is what makes the animated output
//! testable without a TTY.

use super::glyphs::{self, GlyphSet, Outcome, Style};
use super::render::Geometry;

/// A right-aligned Cargo-style phase prefix: 12 columns, then the message.
pub fn phase_line(color: bool, style: Style, verb: &str, msg: &str) -> String {
    let painted = glyphs::paint(color, style, verb);
    let pad = 12usize.saturating_sub(verb.chars().count());
    format!("{}{} {}", " ".repeat(pad), painted, msg)
}

/// One in-flight download.
#[derive(Debug, Clone)]
pub struct Transfer {
    pub id: u64,
    pub name: String,
    pub done: u64,
    pub total: Option<u64>,
    /// Bytes are in; the checksum is being verified.
    pub verifying: bool,
}

#[derive(Debug, Clone, Default)]
pub struct DownloadState {
    pub total: usize,
    pub finished: usize,
    pub active: Vec<Transfer>,
}

#[derive(Debug, Clone, Default)]
pub struct TestState {
    pub marks: Vec<Outcome>,
    pub passed: usize,
    pub failed: usize,
    pub skipped: usize,
}

/// What the live region is currently showing.
#[derive(Debug, Clone, Default)]
pub enum Live {
    #[default]
    None,
    /// Indeterminate work: a verb, a message, and a spinner between them.
    Spinner {
        verb: String,
        msg: String,
    },
    Downloads(DownloadState),
    Tests(TestState),
}

impl Live {
    /// Render the current state into terminal lines.
    ///
    /// `jobs` caps the number of download bars, as does a third of the terminal
    /// height — a build with 64 parallel fetches must not own the whole screen.
    pub fn lines(
        &self,
        g: &GlyphSet,
        color: bool,
        tick: u64,
        geom: Geometry,
        jobs: usize,
    ) -> Vec<String> {
        match self {
            Live::None => Vec::new(),
            Live::Spinner { verb, msg } => {
                let spin = glyphs::paint(color, Style::BoldCyan, g.spinner_frame(tick));
                vec![phase_line(
                    color,
                    Style::BoldGreen,
                    verb,
                    &format!("{spin} {msg}"),
                )]
            }
            Live::Downloads(d) => download_lines(d, g, color, tick, geom, jobs),
            Live::Tests(t) => test_lines(t, g, color, tick, geom),
        }
    }
}

fn download_lines(
    d: &DownloadState,
    g: &GlyphSet,
    color: bool,
    tick: u64,
    geom: Geometry,
    jobs: usize,
) -> Vec<String> {
    let spin = glyphs::paint(color, Style::BoldCyan, g.spinner_frame(tick));
    let mut lines = vec![phase_line(
        color,
        Style::BoldGreen,
        "Downloading",
        &format!("{spin} {}/{}", d.finished, d.total),
    )];

    let cap = jobs.max(1).min((geom.height / 3).max(1));
    let name_width = d
        .active
        .iter()
        .take(cap)
        .map(|t| t.name.chars().count())
        .max()
        .unwrap_or(0)
        .min(32);

    for t in d.active.iter().take(cap) {
        let fraction = match t.total {
            Some(total) if total > 0 => t.done as f64 / total as f64,
            _ => 0.0,
        };
        let bar = g.bar(if t.verifying { 1.0 } else { fraction }, 20);
        let bar = glyphs::paint(color, Style::Green, bar);
        let pct = if t.verifying {
            100
        } else {
            (fraction * 100.0).round() as u32
        };
        let tail = if t.verifying {
            format!("verifying{}", g.ellipsis)
        } else {
            size_pair(t.done, t.total)
        };
        lines.push(format!(
            "  [{bar}] {pct:>3}%  {:<name_width$}  {tail}",
            t.name
        ));
    }
    lines
}

fn test_lines(t: &TestState, g: &GlyphSet, color: bool, tick: u64, geom: Geometry) -> Vec<String> {
    let spin = glyphs::paint(color, Style::BoldCyan, g.spinner_frame(tick));
    let run = t.passed + t.failed + t.skipped;

    let mut tally = format!("({} passed", t.passed);
    if t.failed > 0 {
        tally.push_str(&format!(", {} failed", t.failed));
    }
    if t.skipped > 0 {
        tally.push_str(&format!(", {} skipped", t.skipped));
    }
    tally.push(')');

    // Reserve room for the prefix, counter and tally, and show as many of the
    // most recent marks as fit in what is left.
    let fixed = 12 + 1 + 2 + run.to_string().len() + 2 + tally.chars().count() + 2;
    let room = geom.width.saturating_sub(fixed);
    let shown = t.marks.len().min(room);
    let marks: String = t.marks[t.marks.len() - shown..]
        .iter()
        .map(|o| {
            let style = match o {
                Outcome::Pass => Style::Green,
                Outcome::Fail => Style::Red,
                Outcome::Skip => Style::Yellow,
            };
            glyphs::paint(color, style, g.mark(*o))
        })
        .collect();

    vec![phase_line(
        color,
        Style::BoldGreen,
        "Testing",
        &format!("{spin} {run}  {marks}  {tally}"),
    )]
}

/// `1.9/3.1 MB`, or just the transferred amount when the length is unknown.
pub fn size_pair(done: u64, total: Option<u64>) -> String {
    match total {
        Some(total) if total > 0 => {
            let (unit, div) = unit_for(total);
            format!("{:.1}/{:.1} {unit}", done as f64 / div, total as f64 / div)
        }
        _ => {
            let (unit, div) = unit_for(done);
            format!("{:.1} {unit}", done as f64 / div)
        }
    }
}

fn unit_for(bytes: u64) -> (&'static str, f64) {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    if bytes as f64 >= MB {
        ("MB", MB)
    } else if bytes as f64 >= KB {
        ("KB", KB)
    } else {
        ("B", 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::glyphs::Charset;

    fn geom() -> Geometry {
        Geometry {
            width: 80,
            height: 24,
        }
    }

    #[test]
    fn phase_lines_right_align_the_verb_in_twelve_columns() {
        let line = phase_line(false, Style::BoldGreen, "Compiling", "my-app v1.0.0");
        assert_eq!(line, "   Compiling my-app v1.0.0");
        let line = phase_line(false, Style::BoldGreen, "Downloading", "guava.jar");
        assert_eq!(line, " Downloading guava.jar");
        let line = phase_line(false, Style::BoldGreen, "Finished", "build in 2.31s");
        assert_eq!(line, "    Finished build in 2.31s");
    }

    #[test]
    fn padding_is_computed_before_colour_is_applied() {
        let plain = phase_line(false, Style::BoldGreen, "Compiling", "x");
        let painted = phase_line(true, Style::BoldGreen, "Compiling", "x");
        assert_eq!(
            glyphs::display_width(&plain),
            glyphs::display_width(&painted)
        );
    }

    #[test]
    fn spinner_advances_with_the_tick() {
        let g = GlyphSet::for_charset(Charset::Ascii);
        let live = Live::Spinner {
            verb: "Resolving".into(),
            msg: "14 dependencies".into(),
        };
        let a = live.lines(&g, false, 0, geom(), 4);
        let b = live.lines(&g, false, 1, geom(), 4);
        assert_eq!(a[0], "   Resolving | 14 dependencies");
        assert_eq!(b[0], "   Resolving / 14 dependencies");
    }

    #[test]
    fn download_bars_are_capped_by_jobs() {
        let g = GlyphSet::for_charset(Charset::Ascii);
        let state = DownloadState {
            total: 14,
            finished: 5,
            active: (0..9)
                .map(|i| Transfer {
                    id: i,
                    name: format!("dep-{i}.jar"),
                    done: 100,
                    total: Some(200),
                    verifying: false,
                })
                .collect(),
        };
        let lines = Live::Downloads(state).lines(&g, false, 0, geom(), 3);
        assert_eq!(lines.len(), 4, "one header plus three bars");
        assert_eq!(lines[0], " Downloading | 5/14");
        assert!(lines[1].contains("##########----------"));
        assert!(lines[1].contains(" 50%"));
    }

    #[test]
    fn a_short_terminal_shows_fewer_bars_than_jobs_allows() {
        let g = GlyphSet::for_charset(Charset::Ascii);
        let state = DownloadState {
            total: 14,
            finished: 0,
            active: (0..9)
                .map(|i| Transfer {
                    id: i,
                    name: format!("dep-{i}.jar"),
                    done: 0,
                    total: Some(200),
                    verifying: false,
                })
                .collect(),
        };
        let short = Geometry {
            width: 80,
            height: 9,
        };
        let lines = Live::Downloads(state).lines(&g, false, 0, short, 8);
        assert_eq!(lines.len(), 4, "capped at a third of the terminal height");
    }

    #[test]
    fn verifying_transfers_show_a_full_bar() {
        let g = GlyphSet::for_charset(Charset::Ascii);
        let state = DownloadState {
            total: 1,
            finished: 0,
            active: vec![Transfer {
                id: 0,
                name: "checker-qual-3.42.0.jar".into(),
                done: 200,
                total: Some(200),
                verifying: true,
            }],
        };
        let lines = Live::Downloads(state).lines(&g, false, 0, geom(), 4);
        assert!(lines[1].contains("####################"));
        assert!(lines[1].contains("100%"));
        assert!(lines[1].ends_with("verifying..."));
    }

    #[test]
    fn test_marks_are_trimmed_to_fit_the_terminal() {
        let g = GlyphSet::for_charset(Charset::Ascii);
        let state = TestState {
            marks: vec![Outcome::Pass; 200],
            passed: 200,
            failed: 0,
            skipped: 0,
        };
        let lines = Live::Tests(state).lines(&g, false, 0, geom(), 4);
        assert!(
            glyphs::display_width(&lines[0]) <= 80,
            "line was {} columns: {}",
            glyphs::display_width(&lines[0]),
            lines[0]
        );
    }

    #[test]
    fn test_tally_counts_failures() {
        let g = GlyphSet::for_charset(Charset::Ascii);
        let mut marks = vec![Outcome::Pass; 22];
        marks.insert(6, Outcome::Fail);
        let state = TestState {
            marks,
            passed: 22,
            failed: 1,
            skipped: 0,
        };
        let lines = Live::Tests(state).lines(&g, false, 0, geom(), 4);
        assert!(lines[0].starts_with("     Testing | 23  "));
        assert!(lines[0].ends_with("(22 passed, 1 failed)"));
        assert!(lines[0].contains("++++++x"));
    }

    #[test]
    fn sizes_pick_a_readable_unit() {
        assert_eq!(size_pair(1_992_294, Some(3_250_585)), "1.9/3.1 MB");
        assert_eq!(size_pair(512, Some(2048)), "0.5/2.0 KB");
        assert_eq!(size_pair(10, None), "10.0 B");
    }
}
