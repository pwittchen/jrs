//! The output layer.
//!
//! Build code never prints. It reports progress by updating shared state, and
//! everything that reaches a terminal goes through this module (SPEC §6.2). That
//! boundary is what keeps the animated and plain modes the same build, and what
//! makes the library testable without a TTY.

pub mod glyphs;
pub mod progress;
pub mod render;

use std::io::IsTerminal;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub use glyphs::{Charset, GlyphSet, Outcome, Style};
pub use progress::{DownloadState, Live, TestState, Transfer};
pub use render::{Capture, Geometry, Stream};

/// The render tick: 12.5 fps. Fast enough to look alive, slow enough that the
/// render thread is never the reason a build is slow.
const TICK: Duration = Duration::from_millis(80);

/// A tri-state CLI toggle (`auto` / `always` / `never`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum When {
    #[default]
    Auto,
    Always,
    Never,
}

/// `--charset auto|unicode|ascii`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CharsetChoice {
    #[default]
    Auto,
    Unicode,
    Ascii,
}

#[derive(Debug, Clone)]
pub struct UiOptions {
    pub verbose: bool,
    pub quiet: bool,
    pub progress: When,
    pub color: When,
    pub charset: CharsetChoice,
    pub jobs: usize,
}

impl Default for UiOptions {
    fn default() -> Self {
        UiOptions {
            verbose: false,
            quiet: false,
            progress: When::Auto,
            color: When::Auto,
            charset: CharsetChoice::Auto,
            jobs: 4,
        }
    }
}

/// How much jrs says, and how.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Errors only.
    Quiet,
    /// One line when a phase starts, one when it ends. A log, not a canvas.
    Plain,
    /// Spinners, live bars, framed summary.
    Animated,
    /// Plain, plus every subprocess command line and its exit status.
    Verbose,
}

impl Mode {
    pub fn animates(self) -> bool {
        self == Mode::Animated
    }
}

/// True when the environment looks like CI, where animation is noise.
fn in_ci() -> bool {
    const VARS: &[&str] = &[
        "CI",
        "GITHUB_ACTIONS",
        "GITLAB_CI",
        "TRAVIS",
        "CIRCLECI",
        "BUILDKITE",
        "TEAMCITY_VERSION",
        "JENKINS_URL",
    ];
    VARS.iter().any(|v| {
        std::env::var(v)
            .map(|s| !s.is_empty() && s != "0" && s != "false")
            .unwrap_or(false)
    })
}

fn dumb_terminal() -> bool {
    std::env::var("NO_COLOR")
        .map(|v| !v.is_empty())
        .unwrap_or(false)
        || std::env::var("TERM").map(|t| t == "dumb").unwrap_or(false)
}

fn decide_mode(opts: &UiOptions, stderr_tty: bool) -> Mode {
    if opts.quiet {
        return Mode::Quiet;
    }
    if opts.verbose {
        // Verbose interleaves subprocess output, which would fight the live
        // region, so the two modes are mutually exclusive by construction.
        return Mode::Verbose;
    }
    match opts.progress {
        When::Never => Mode::Plain,
        When::Always => Mode::Animated,
        When::Auto => {
            if stderr_tty && !dumb_terminal() && !in_ci() {
                Mode::Animated
            } else {
                Mode::Plain
            }
        }
    }
}

fn decide_color(opts: &UiOptions, stderr_tty: bool) -> bool {
    match opts.color {
        When::Never => false,
        When::Always => true,
        When::Auto => stderr_tty && !dumb_terminal(),
    }
}

fn decide_charset(choice: CharsetChoice) -> Charset {
    match choice {
        CharsetChoice::Unicode => Charset::Unicode,
        CharsetChoice::Ascii => Charset::Ascii,
        CharsetChoice::Auto => Charset::probe(),
    }
}

struct Inner {
    mode: Mode,
    color: bool,
    glyphs: GlyphSet,
    jobs: usize,
    live: Mutex<Live>,
    renderer: Mutex<render::Renderer>,
    tick: AtomicU64,
    /// Set while a live scope owns the region; used to stop the render thread.
    running: AtomicBool,
    /// Suppressed in captured mode so tests drive frames by hand.
    threaded: bool,
}

/// The handle every command carries. Cheap to clone; all clones share one
/// terminal.
#[derive(Clone)]
pub struct Ui {
    inner: Arc<Inner>,
}

impl Ui {
    pub fn new(opts: UiOptions) -> Ui {
        let stderr_tty = std::io::stderr().is_terminal();
        let mode = decide_mode(&opts, stderr_tty);
        let color = decide_color(&opts, stderr_tty);
        Ui {
            inner: Arc::new(Inner {
                mode,
                color,
                glyphs: GlyphSet::for_charset(decide_charset(opts.charset)),
                jobs: opts.jobs.max(1),
                live: Mutex::new(Live::None),
                renderer: Mutex::new(render::Renderer::terminal(mode.animates())),
                tick: AtomicU64::new(0),
                running: AtomicBool::new(false),
                threaded: true,
            }),
        }
    }

    /// A `Ui` that draws into memory, with a fixed terminal size and a clock
    /// that only advances when a test says so.
    pub fn captured(opts: UiOptions, geometry: Geometry) -> (Ui, Capture) {
        let capture = Capture::new();
        let mode = decide_mode(&opts, true);
        let color = decide_color(&opts, true);
        let ui = Ui {
            inner: Arc::new(Inner {
                mode,
                color,
                glyphs: GlyphSet::for_charset(decide_charset(opts.charset)),
                jobs: opts.jobs.max(1),
                live: Mutex::new(Live::None),
                renderer: Mutex::new(render::Renderer::captured(
                    capture.clone(),
                    mode.animates(),
                    geometry,
                )),
                tick: AtomicU64::new(0),
                running: AtomicBool::new(false),
                threaded: false,
            }),
        };
        (ui, capture)
    }

    pub fn mode(&self) -> Mode {
        self.inner.mode
    }

    pub fn is_verbose(&self) -> bool {
        self.inner.mode == Mode::Verbose
    }

    pub fn is_quiet(&self) -> bool {
        self.inner.mode == Mode::Quiet
    }

    pub fn color(&self) -> bool {
        self.inner.color
    }

    pub fn glyphs(&self) -> &GlyphSet {
        &self.inner.glyphs
    }

    pub fn jobs(&self) -> usize {
        self.inner.jobs
    }

    /// Advance the clock by one frame and redraw. The render thread calls this;
    /// output tests call it directly.
    pub fn render_frame(&self) {
        let tick = self.inner.tick.fetch_add(1, Ordering::Relaxed);
        self.draw(tick);
    }

    fn draw(&self, tick: u64) {
        let mut renderer = self.inner.renderer.lock().unwrap();
        if !renderer.animates() {
            return;
        }
        let geom = renderer.geometry();
        let live = self.inner.live.lock().unwrap().clone();
        let lines = live.lines(
            &self.inner.glyphs,
            self.inner.color,
            tick,
            geom,
            self.inner.jobs,
        );
        renderer.set_live(lines);
    }

    /// Mutate the live region's state. Workers call this; nothing here touches
    /// the terminal.
    pub fn update_live(&self, f: impl FnOnce(&mut Live)) {
        let mut live = self.inner.live.lock().unwrap();
        f(&mut live);
    }

    // ---- permanent output -------------------------------------------------

    fn permanent(&self, stream: Stream, text: &str) {
        self.inner.renderer.lock().unwrap().permanent(stream, text);
    }

    /// A Cargo-style phase line: bold green verb, right-aligned in 12 columns.
    pub fn phase(&self, verb: &str, msg: impl std::fmt::Display) {
        if self.inner.mode == Mode::Quiet {
            return;
        }
        let line = progress::phase_line(self.inner.color, Style::BoldGreen, verb, &msg.to_string());
        self.permanent(Stream::Err, &line);
    }

    /// A phase line in the dimmer "this is a detail" register.
    pub fn status(&self, verb: &str, msg: impl std::fmt::Display) {
        if self.inner.mode == Mode::Quiet {
            return;
        }
        let line = progress::phase_line(self.inner.color, Style::BoldCyan, verb, &msg.to_string());
        self.permanent(Stream::Err, &line);
    }

    pub fn warn(&self, msg: impl std::fmt::Display) {
        if self.inner.mode == Mode::Quiet {
            return;
        }
        let tag = glyphs::paint(self.inner.color, Style::BoldYellow, "warning");
        self.permanent(Stream::Err, &format!("{tag}: {msg}"));
    }

    pub fn error(&self, msg: impl std::fmt::Display) {
        let tag = glyphs::paint(self.inner.color, Style::BoldRed, "error");
        self.permanent(Stream::Err, &format!("{tag}: {msg}"));
    }

    /// Subprocess command lines and other `-v` chatter.
    pub fn verbose(&self, msg: impl std::fmt::Display) {
        if self.inner.mode != Mode::Verbose {
            return;
        }
        let tag = glyphs::paint(self.inner.color, Style::Dim, "+");
        self.permanent(Stream::Err, &format!("{tag} {msg}"));
    }

    /// Real output: `jrs tree`, the user's program, the migration report.
    pub fn println_out(&self, line: impl std::fmt::Display) {
        self.permanent(Stream::Out, &line.to_string());
    }

    /// Verbatim toolchain output, replayed after the live region is down.
    pub fn passthrough(&self, stream: Stream, text: &str) {
        if text.is_empty() {
            return;
        }
        let mut renderer = self.inner.renderer.lock().unwrap();
        renderer.clear_live();
        for line in text.lines() {
            renderer.permanent(stream, line);
        }
    }

    /// Tear the live region down. Called before anything writes to the terminal
    /// behind jrs's back — a subprocess inheriting stdio, or a diagnostic.
    pub fn suspend(&self) {
        self.inner.renderer.lock().unwrap().clear_live();
    }

    /// The wordmark, shown once by `init` and `migrate` — TTY only, never in the
    /// plain-output mode (SPEC §5.3.7).
    pub fn banner(&self) {
        if !self.inner.mode.animates() {
            return;
        }
        let text = glyphs::paint(self.inner.color, Style::BoldCyan, glyphs::BANNER);
        for line in text.lines() {
            self.permanent(Stream::Err, line);
        }
    }

    // ---- live scopes ------------------------------------------------------

    // A live scope only ever animates. The permanent phase line that names what
    // is happening is the caller's, so that the plain and animated modes print
    // the same transcript and differ only in what moves.

    /// Begin an indeterminate phase: a verb, a message, and a spinner.
    pub fn spinner(&self, verb: &str, msg: impl std::fmt::Display) -> LiveScope {
        if !self.inner.mode.animates() {
            return LiveScope::inert();
        }
        *self.inner.live.lock().unwrap() = Live::Spinner {
            verb: verb.to_string(),
            msg: msg.to_string(),
        };
        self.start_scope()
    }

    /// Begin the download phase. Workers publish transfers into the live state.
    pub fn downloads(&self, total: usize) -> LiveScope {
        if !self.inner.mode.animates() {
            return LiveScope::inert();
        }
        *self.inner.live.lock().unwrap() = Live::Downloads(DownloadState {
            total,
            finished: 0,
            active: Vec::new(),
        });
        self.start_scope()
    }

    /// Begin the test phase.
    pub fn tests(&self) -> LiveScope {
        if !self.inner.mode.animates() {
            return LiveScope::inert();
        }
        *self.inner.live.lock().unwrap() = Live::Tests(TestState::default());
        self.start_scope()
    }

    fn start_scope(&self) -> LiveScope {
        self.inner.running.store(true, Ordering::SeqCst);
        self.draw(self.inner.tick.load(Ordering::Relaxed));
        let thread = if self.inner.threaded {
            let ui = self.clone();
            Some(std::thread::spawn(move || {
                while ui.inner.running.load(Ordering::SeqCst) {
                    // Parked rather than asleep, so ending a scope wakes the
                    // thread at once: a `sleep` here made every phase wait out
                    // up to a tick when it finished, which the M5 benchmark
                    // measured at ~90 ms a build.
                    std::thread::park_timeout(TICK);
                    if !ui.inner.running.load(Ordering::SeqCst) {
                        break;
                    }
                    ui.render_frame();
                }
            }))
        } else {
            None
        };
        LiveScope {
            ui: Some(self.clone()),
            thread,
        }
    }

    // ---- composite output -------------------------------------------------

    /// The framed end-of-build block (SPEC §5.3.6).
    ///
    /// The frame is the animated mode's flourish; a plain run gets the same facts
    /// as ordinary lines.
    pub fn summary(&self, rows: &[(&str, String)]) {
        if self.inner.mode == Mode::Quiet {
            return;
        }
        if !self.inner.mode.animates() {
            for (label, value) in rows {
                self.status(label, value);
            }
            return;
        }
        for line in render_summary(rows, &self.inner.glyphs, self.inner.color) {
            self.permanent(Stream::Err, &line);
        }
    }

    /// Draw a tree to stdout — `jrs tree` is real output, not progress.
    pub fn tree(&self, root: &TreeNode) {
        for line in render_tree(root, &self.inner.glyphs, self.inner.color) {
            self.println_out(line);
        }
    }
}

/// Holds the live region for the duration of a phase, and takes it down again.
pub struct LiveScope {
    ui: Option<Ui>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl LiveScope {
    fn inert() -> LiveScope {
        LiveScope {
            ui: None,
            thread: None,
        }
    }

    /// End the phase now rather than at the end of the enclosing block.
    pub fn finish(mut self) {
        self.stop();
    }

    fn stop(&mut self) {
        let Some(ui) = self.ui.take() else { return };
        ui.inner.running.store(false, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            t.thread().unpark();
            let _ = t.join();
        }
        ui.suspend();
        *ui.inner.live.lock().unwrap() = Live::None;
    }
}

impl Drop for LiveScope {
    fn drop(&mut self) {
        self.stop();
    }
}

/// A node in a rendered tree, with an optional highlight.
#[derive(Debug, Clone)]
pub struct TreeNode {
    pub label: String,
    pub style: Style,
    pub children: Vec<TreeNode>,
}

impl TreeNode {
    pub fn new(label: impl Into<String>) -> TreeNode {
        TreeNode {
            label: label.into(),
            style: Style::None,
            children: Vec::new(),
        }
    }

    pub fn styled(label: impl Into<String>, style: Style) -> TreeNode {
        TreeNode {
            label: label.into(),
            style,
            children: Vec::new(),
        }
    }
}

pub fn render_tree(root: &TreeNode, g: &GlyphSet, color: bool) -> Vec<String> {
    let mut out = vec![glyphs::paint(color, root.style, &root.label)];
    push_children(&root.children, "", g, color, &mut out);
    out
}

fn push_children(
    nodes: &[TreeNode],
    prefix: &str,
    g: &GlyphSet,
    color: bool,
    out: &mut Vec<String>,
) {
    for (i, node) in nodes.iter().enumerate() {
        let last = i + 1 == nodes.len();
        let branch = if last { g.elbow } else { g.tee };
        out.push(format!(
            "{prefix}{branch}{}",
            glyphs::paint(color, node.style, &node.label)
        ));
        let child_prefix = format!("{prefix}{}", if last { g.blank } else { g.pipe });
        push_children(&node.children, &child_prefix, g, color, out);
    }
}

pub fn render_summary(rows: &[(&str, String)], g: &GlyphSet, color: bool) -> Vec<String> {
    let label_width = rows
        .iter()
        .map(|(l, _)| l.chars().count())
        .max()
        .unwrap_or(0);
    let body: Vec<String> = rows
        .iter()
        .map(|(l, v)| format!("  {l:<label_width$}  {v}"))
        .collect();
    let inner = body
        .iter()
        .map(|b| b.chars().count())
        .max()
        .unwrap_or(0)
        .max(14)
        + 2;

    let title = " jrs ";
    let dashes = inner.saturating_sub(1 + title.chars().count());
    let top = format!(
        "  {}{}{}{}{}",
        g.frame_tl,
        g.frame_h,
        title,
        g.frame_h.repeat(dashes),
        g.frame_tr
    );
    let bottom = format!("  {}{}{}", g.frame_bl, g.frame_h.repeat(inner), g.frame_br);

    let mut lines = vec![glyphs::paint(color, Style::Dim, top)];
    for b in body {
        let pad = inner.saturating_sub(b.chars().count());
        lines.push(format!(
            "  {}{}{}{}",
            glyphs::paint(color, Style::Dim, g.frame_v),
            b,
            " ".repeat(pad),
            glyphs::paint(color, Style::Dim, g.frame_v)
        ));
    }
    lines.push(glyphs::paint(color, Style::Dim, bottom));
    lines
}

/// `2.31s`, or `412ms` for the quick ones.
pub fn format_duration(d: Duration) -> String {
    let secs = d.as_secs_f64();
    if secs < 1.0 {
        format!("{}ms", d.as_millis())
    } else if secs < 60.0 {
        format!("{secs:.2}s")
    } else {
        format!("{}m {:.0}s", (secs / 60.0) as u64, secs % 60.0)
    }
}

/// `412 KB`, for the packaged jar.
pub fn format_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    let b = bytes as f64;
    if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.0} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn geom() -> Geometry {
        Geometry {
            width: 60,
            height: 24,
        }
    }

    fn opts(progress: When) -> UiOptions {
        UiOptions {
            progress,
            color: When::Never,
            charset: CharsetChoice::Ascii,
            jobs: 4,
            ..Default::default()
        }
    }

    #[test]
    fn quiet_and_verbose_never_animate() {
        let mut o = opts(When::Always);
        o.quiet = true;
        assert_eq!(decide_mode(&o, true), Mode::Quiet);
        let mut o = opts(When::Always);
        o.verbose = true;
        assert_eq!(decide_mode(&o, true), Mode::Verbose);
    }

    #[test]
    fn animation_is_off_when_stderr_is_not_a_tty() {
        assert_eq!(decide_mode(&opts(When::Auto), false), Mode::Plain);
        assert_eq!(
            decide_mode(&opts(When::Always), false),
            Mode::Animated,
            "--progress always overrides detection"
        );
    }

    #[test]
    fn plain_mode_produces_a_log_not_a_canvas() {
        // The phase lines are the caller's; a live scope contributes nothing but
        // motion, so plain mode is the same transcript with the motion removed.
        let (ui, cap) = Ui::captured(opts(When::Never), geom());
        ui.phase("Compiling", "my-app v1.0.0 (47 source files)");
        let scope = ui.spinner("Compiling", "47 source files");
        ui.render_frame();
        ui.render_frame();
        scope.finish();
        ui.phase("Finished", "build in 2.31s");
        assert_eq!(
            cap.stderr(),
            "   Compiling my-app v1.0.0 (47 source files)\n    Finished build in 2.31s\n"
        );
    }

    #[test]
    fn animated_mode_redraws_in_place() {
        let (ui, cap) = Ui::captured(opts(When::Always), geom());
        let scope = ui.spinner("Resolving", "14 dependencies");
        ui.render_frame();
        ui.render_frame();
        scope.finish();
        let err = cap.stderr();
        assert!(err.contains("   Resolving | 14 dependencies"));
        assert!(err.contains("   Resolving / 14 dependencies"));
        assert!(err.contains("\x1b[1A\x1b[0J"), "no in-place redraw");
        assert!(err.ends_with("\x1b[?25h"), "cursor not restored: {err:?}");
    }

    #[test]
    fn phase_lines_land_above_the_live_region() {
        let (ui, cap) = Ui::captured(opts(When::Always), geom());
        let scope = ui.spinner("Downloading", "guava");
        ui.render_frame();
        ui.phase("Compiling", "my-app v1.0.0");
        scope.finish();
        let err = cap.stderr();
        let phase = err.find("   Compiling my-app").unwrap();
        let erase = err[..phase].rfind("\x1b[1A\x1b[0J").unwrap();
        assert!(erase < phase, "live region was not erased first");
    }

    #[test]
    fn the_banner_is_animated_mode_only() {
        let (ui, cap) = Ui::captured(opts(When::Never), geom());
        ui.banner();
        assert_eq!(cap.stderr(), "");
        let (ui, cap) = Ui::captured(opts(When::Always), geom());
        ui.banner();
        assert!(cap.stderr().contains("a Java build system in Rust"));
    }

    #[test]
    fn real_output_goes_to_stdout() {
        let (ui, cap) = Ui::captured(opts(When::Always), geom());
        let scope = ui.spinner("Resolving", "deps");
        ui.render_frame();
        ui.println_out("my-app v1.0.0");
        scope.finish();
        assert_eq!(cap.stdout(), "my-app v1.0.0\n");
    }

    #[test]
    fn quiet_says_nothing_but_errors() {
        let mut o = opts(When::Never);
        o.quiet = true;
        let (ui, cap) = Ui::captured(o, geom());
        ui.phase("Compiling", "x");
        ui.warn("something odd");
        ui.summary(&[("build", "ok".into())]);
        assert_eq!(cap.stderr(), "");
        ui.error("javac failed");
        assert_eq!(cap.stderr(), "error: javac failed\n");
    }

    #[test]
    fn tree_uses_the_ascii_fallback() {
        let g = GlyphSet::for_charset(Charset::Ascii);
        let mut root = TreeNode::new("my-app v1.0.0");
        let mut guava = TreeNode::new("com.google.guava:guava:33.0.0-jre");
        guava
            .children
            .push(TreeNode::new("com.google.guava:failureaccess:1.0.2"));
        root.children.push(guava);
        root.children
            .push(TreeNode::new("org.apache.commons:commons-lang3:3.14.0"));
        let lines = render_tree(&root, &g, false);
        assert_eq!(
            lines,
            vec![
                "my-app v1.0.0",
                "|-- com.google.guava:guava:33.0.0-jre",
                "|   `-- com.google.guava:failureaccess:1.0.2",
                "`-- org.apache.commons:commons-lang3:3.14.0",
            ]
        );
    }

    #[test]
    fn summary_frame_is_rectangular() {
        let g = GlyphSet::for_charset(Charset::Unicode);
        let lines = render_summary(
            &[
                ("build", "ok      47 classes".into()),
                ("deps", "14      3 downloaded".into()),
                ("jar", "my-app-1.0.0.jar   412 KB".into()),
                ("time", "2.31s".into()),
            ],
            &g,
            false,
        );
        let widths: Vec<usize> = lines.iter().map(|l| glyphs::display_width(l)).collect();
        assert!(
            widths.windows(2).all(|w| w[0] == w[1]),
            "ragged frame: {widths:?}"
        );
        assert!(lines[0].contains("┌─ jrs "));
        assert!(lines.last().unwrap().contains("└"));
    }

    #[test]
    fn durations_and_sizes_read_naturally() {
        assert_eq!(format_duration(Duration::from_millis(412)), "412ms");
        assert_eq!(format_duration(Duration::from_millis(2310)), "2.31s");
        assert_eq!(format_bytes(421_888), "412 KB");
        assert_eq!(format_bytes(12), "12 B");
    }
}
