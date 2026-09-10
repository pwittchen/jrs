//! The live region, the terminal sink, and the cursor guard.
//!
//! This is the only module in jrs that writes escape sequences. Everything above
//! it hands down finished strings; everything below it is the terminal.
//!
//! Two invariants matter more than the drawing itself (SPEC §5.3.1):
//!
//! * the live region is erased before any permanent line is written above it, so
//!   scrollback holds a clean transcript;
//! * the cursor is restored on every exit path — normal, error, panic, signal.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Once};

/// Which stream a permanent line belongs on.
///
/// Progress and diagnostics go to stderr; stdout carries only real output, so
/// pipes and redirects stay clean.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    Out,
    Err,
}

/// An in-memory pair of streams, used by the output tests.
#[derive(Debug, Clone, Default)]
pub struct Capture {
    pub out: Arc<Mutex<Vec<u8>>>,
    pub err: Arc<Mutex<Vec<u8>>>,
}

impl Capture {
    pub fn new() -> Capture {
        Capture::default()
    }

    pub fn stdout(&self) -> String {
        String::from_utf8_lossy(&self.out.lock().unwrap()).into_owned()
    }

    pub fn stderr(&self) -> String {
        String::from_utf8_lossy(&self.err.lock().unwrap()).into_owned()
    }
}

/// Where rendered bytes end up.
#[derive(Debug, Clone)]
pub enum Sink {
    Terminal,
    Capture(Capture),
}

impl Sink {
    fn write(&self, stream: Stream, bytes: &[u8]) {
        match self {
            Sink::Terminal => {
                let _ = match stream {
                    Stream::Out => std::io::stdout().write_all(bytes),
                    Stream::Err => std::io::stderr().write_all(bytes),
                };
                let _ = match stream {
                    Stream::Out => std::io::stdout().flush(),
                    Stream::Err => std::io::stderr().flush(),
                };
            }
            Sink::Capture(c) => {
                let target = match stream {
                    Stream::Out => &c.out,
                    Stream::Err => &c.err,
                };
                target.lock().unwrap().extend_from_slice(bytes);
            }
        }
    }
}

const HIDE_CURSOR: &[u8] = b"\x1b[?25l";
const SHOW_CURSOR: &[u8] = b"\x1b[?25h";

static CURSOR_HIDDEN: AtomicBool = AtomicBool::new(false);
static GUARD_INSTALLED: Once = Once::new();

/// Restore the cursor if we hid it. Safe to call any number of times.
pub fn restore_cursor() {
    if CURSOR_HIDDEN.swap(false, Ordering::SeqCst) {
        let _ = std::io::stderr().write_all(SHOW_CURSOR);
        let _ = std::io::stderr().flush();
    }
}

/// Install the panic hook and signal handlers that restore the cursor.
///
/// Leaving a user with an invisible cursor is a bug of the same severity as a
/// wrong classpath, so this runs before the first hide, not on a best-effort
/// basis afterwards.
fn install_guard() {
    GUARD_INSTALLED.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore_cursor();
            previous(info);
        }));
        #[cfg(unix)]
        install_signal_handlers();
    });
}

#[cfg(unix)]
fn install_signal_handlers() {
    // The handler does nothing but write six bytes and re-raise with the default
    // disposition, both of which are async-signal-safe.
    extern "C" fn on_signal(sig: libc::c_int) {
        if CURSOR_HIDDEN.swap(false, std::sync::atomic::Ordering::SeqCst) {
            unsafe {
                libc::write(
                    libc::STDERR_FILENO,
                    SHOW_CURSOR.as_ptr() as *const libc::c_void,
                    SHOW_CURSOR.len(),
                );
            }
        }
        unsafe {
            libc::signal(sig, libc::SIG_DFL);
            libc::raise(sig);
        }
    }

    unsafe {
        libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
    }
}

/// Terminal geometry, probed once and refreshed on each frame.
#[derive(Debug, Clone, Copy)]
pub struct Geometry {
    pub width: usize,
    pub height: usize,
}

impl Default for Geometry {
    fn default() -> Self {
        Geometry {
            width: 80,
            height: 24,
        }
    }
}

fn probe_geometry() -> Option<Geometry> {
    let (terminal_size::Width(w), terminal_size::Height(h)) =
        terminal_size::terminal_size_of(std::io::stderr())?;
    Some(Geometry {
        width: w as usize,
        height: h as usize,
    })
}

/// Owns the terminal: what is currently drawn, and how to replace it.
#[derive(Debug)]
pub struct Renderer {
    sink: Sink,
    /// Lines of the live region, already styled but not yet truncated.
    live: Vec<String>,
    /// How many lines are physically on screen right now.
    drawn: usize,
    /// Whether a live region may be drawn at all. Plain mode sets this false and
    /// the renderer degrades to a log.
    animate: bool,
    /// Fixed geometry, for tests with a known terminal width.
    fixed: Option<Geometry>,
    /// Whether *this* renderer hid the cursor. Mirrored into the process-wide
    /// flag only for the real terminal, so captured renderers in unit tests do
    /// not fight each other over one global.
    cursor_hidden: bool,
}

impl Renderer {
    pub fn terminal(animate: bool) -> Renderer {
        Renderer {
            sink: Sink::Terminal,
            live: Vec::new(),
            drawn: 0,
            animate,
            fixed: None,
            cursor_hidden: false,
        }
    }

    /// A renderer that draws into memory with a known terminal size.
    pub fn captured(capture: Capture, animate: bool, geometry: Geometry) -> Renderer {
        Renderer {
            sink: Sink::Capture(capture),
            live: Vec::new(),
            drawn: 0,
            animate,
            fixed: Some(geometry),
            cursor_hidden: false,
        }
    }

    fn is_terminal(&self) -> bool {
        matches!(self.sink, Sink::Terminal)
    }

    pub fn geometry(&self) -> Geometry {
        self.fixed.or_else(probe_geometry).unwrap_or_default()
    }

    pub fn animates(&self) -> bool {
        self.animate
    }

    /// Write a line that stays in the scrollback, above the live region.
    pub fn permanent(&mut self, stream: Stream, text: &str) {
        self.erase();
        let mut bytes = text.as_bytes().to_vec();
        bytes.push(b'\n');
        self.sink.write(stream, &bytes);
        self.paint();
    }

    /// Replace the live region's contents and redraw it in place.
    pub fn set_live(&mut self, lines: Vec<String>) {
        if !self.animate {
            return;
        }
        self.erase();
        self.live = lines;
        self.paint();
    }

    /// Tear the live region down. Called before subprocess passthrough, before
    /// any diagnostic, and at the end of every phase.
    pub fn clear_live(&mut self) {
        self.erase();
        self.live.clear();
        if self.cursor_hidden {
            self.cursor_hidden = false;
            if self.is_terminal() {
                CURSOR_HIDDEN.store(false, Ordering::SeqCst);
            }
            self.sink.write(Stream::Err, SHOW_CURSOR);
        }
    }

    fn erase(&mut self) {
        if self.drawn == 0 {
            return;
        }
        // Move up over everything we drew, then clear to the end of the screen.
        let seq = format!("\x1b[{}A\x1b[0J", self.drawn);
        self.sink.write(Stream::Err, seq.as_bytes());
        self.drawn = 0;
    }

    fn paint(&mut self) {
        if self.live.is_empty() {
            return;
        }
        if !self.cursor_hidden {
            self.cursor_hidden = true;
            if self.is_terminal() {
                install_guard();
                CURSOR_HIDDEN.store(true, Ordering::SeqCst);
            }
            self.sink.write(Stream::Err, HIDE_CURSOR);
        }
        let width = self.geometry().width;
        let mut buf = String::new();
        for line in &self.live {
            buf.push_str(&super::glyphs::truncate(line, width));
            buf.push('\n');
        }
        self.drawn = self.live.len();
        self.sink.write(Stream::Err, buf.as_bytes());
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        self.clear_live();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn captured(animate: bool) -> (Renderer, Capture) {
        let cap = Capture::new();
        let r = Renderer::captured(
            cap.clone(),
            animate,
            Geometry {
                width: 40,
                height: 24,
            },
        );
        (r, cap)
    }

    #[test]
    fn plain_mode_never_draws_a_live_region() {
        let (mut r, cap) = captured(false);
        r.set_live(vec!["spinning".into()]);
        r.permanent(Stream::Err, "   Compiling app");
        assert_eq!(cap.stderr(), "   Compiling app\n");
    }

    #[test]
    fn live_region_is_erased_before_a_permanent_line() {
        let (mut r, cap) = captured(true);
        r.set_live(vec!["one".into(), "two".into()]);
        r.permanent(Stream::Err, "done");
        let err = cap.stderr();
        // The permanent line must be preceded by an erase of both live lines.
        let erase = "\x1b[2A\x1b[0J";
        let idx_erase = err.find(erase).expect("live region was not erased");
        let idx_line = err.find("done\n").expect("permanent line missing");
        assert!(idx_erase < idx_line);
        // ...and the live region is repainted afterwards.
        assert!(err[idx_line..].contains("one\n"));
    }

    #[test]
    fn permanent_lines_reach_the_right_stream() {
        let (mut r, cap) = captured(true);
        r.permanent(Stream::Out, "program output");
        r.permanent(Stream::Err, "  Downloading");
        assert_eq!(cap.stdout(), "program output\n");
        assert!(cap.stderr().contains("  Downloading"));
    }

    #[test]
    fn live_lines_are_truncated_to_the_terminal_width() {
        let (mut r, cap) = captured(true);
        r.set_live(vec!["x".repeat(80)]);
        let err = cap.stderr();
        let line = err.lines().last().unwrap();
        assert_eq!(super::super::glyphs::display_width(line), 40);
    }

    #[test]
    fn clearing_restores_the_cursor() {
        let (mut r, cap) = captured(true);
        r.set_live(vec!["working".into()]);
        assert!(cap.stderr().contains("\x1b[?25l"));
        r.clear_live();
        assert!(cap.stderr().contains("\x1b[?25h"));
    }
}
