//! Glyph sets, colour styling and the ASCII wordmark.
//!
//! Every glyph jrs draws has a Unicode form and an ASCII twin (SPEC §5.3.1), so
//! that no build output is Unicode-only. The set is chosen once, at startup, and
//! then threaded through the rest of `ui`.

use std::fmt;

/// Which glyph repertoire to draw with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Charset {
    Unicode,
    Ascii,
}

impl Charset {
    /// Probe the locale. UTF-8 locales get Unicode; everything else gets ASCII.
    #[must_use]
    pub fn probe() -> Charset {
        if cfg!(windows) {
            // Modern Windows terminals are UTF-8 capable, but the legacy console
            // is not; be conservative unless the code page says otherwise.
            if std::env::var("WT_SESSION").is_ok() {
                return Charset::Unicode;
            }
            return Charset::Ascii;
        }
        for key in ["LC_ALL", "LC_CTYPE", "LANG"] {
            if let Ok(v) = std::env::var(key) {
                if v.is_empty() {
                    continue;
                }
                let v = v.to_ascii_lowercase();
                return if v.contains("utf-8") || v.contains("utf8") {
                    Charset::Unicode
                } else {
                    Charset::Ascii
                };
            }
        }
        Charset::Ascii
    }
}

/// The concrete glyphs used by spinners, bars, trees and frames.
#[derive(Debug, Clone, Copy)]
pub struct GlyphSet {
    pub charset: Charset,
    /// Spinner frames, advanced once per render tick.
    pub spinner: &'static [&'static str],
    /// Filled and empty cells of a progress bar.
    pub bar_full: &'static str,
    pub bar_empty: &'static str,
    /// Per-test result marks.
    pub pass: &'static str,
    pub fail: &'static str,
    pub skip: &'static str,
    /// Tree drawing.
    pub tee: &'static str,
    pub elbow: &'static str,
    pub pipe: &'static str,
    pub blank: &'static str,
    /// Frame drawing for the summary block.
    pub frame_h: &'static str,
    pub frame_v: &'static str,
    pub frame_tl: &'static str,
    pub frame_tr: &'static str,
    pub frame_bl: &'static str,
    pub frame_br: &'static str,
    /// Trailing ellipsis for in-progress labels.
    pub ellipsis: &'static str,
}

const UNICODE: GlyphSet = GlyphSet {
    charset: Charset::Unicode,
    spinner: &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"],
    bar_full: "█",
    bar_empty: "░",
    pass: "✔",
    fail: "✘",
    skip: "↷",
    tee: "├── ",
    elbow: "└── ",
    pipe: "│   ",
    blank: "    ",
    frame_h: "─",
    frame_v: "│",
    frame_tl: "┌",
    frame_tr: "┐",
    frame_bl: "└",
    frame_br: "┘",
    ellipsis: "…",
};

const ASCII: GlyphSet = GlyphSet {
    charset: Charset::Ascii,
    spinner: &["|", "/", "-", "\\"],
    bar_full: "#",
    bar_empty: "-",
    pass: "+",
    fail: "x",
    skip: "s",
    tee: "|-- ",
    elbow: "`-- ",
    pipe: "|   ",
    blank: "    ",
    frame_h: "-",
    frame_v: "|",
    frame_tl: "+",
    frame_tr: "+",
    frame_bl: "+",
    frame_br: "+",
    ellipsis: "...",
};

impl GlyphSet {
    #[must_use]
    pub fn for_charset(charset: Charset) -> GlyphSet {
        match charset {
            Charset::Unicode => UNICODE,
            Charset::Ascii => ASCII,
        }
    }

    #[must_use]
    #[allow(
        clippy::cast_possible_truncation,
        reason = "the tick only picks a frame; wrapping on a 32-bit target just restarts the cycle"
    )]
    pub fn spinner_frame(&self, tick: u64) -> &'static str {
        self.spinner[(tick as usize) % self.spinner.len()]
    }

    /// A `[####----]` style bar of `width` cells at the given fraction.
    #[must_use]
    pub fn bar(&self, fraction: f64, width: usize) -> String {
        let fraction = fraction.clamp(0.0, 1.0);
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss,
            reason = "fraction is clamped to 0..=1, so the result is a cell count in 0..=width"
        )]
        let filled = (fraction * width as f64).round() as usize;
        let filled = filled.min(width);
        let mut s = String::with_capacity(width * 3);
        for _ in 0..filled {
            s.push_str(self.bar_full);
        }
        for _ in filled..width {
            s.push_str(self.bar_empty);
        }
        s
    }

    /// Test result marks, ASCII or Unicode.
    #[must_use]
    pub fn mark(&self, outcome: Outcome) -> &'static str {
        match outcome {
            Outcome::Pass => self.pass,
            Outcome::Fail => self.fail,
            Outcome::Skip => self.skip,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Pass,
    Fail,
    Skip,
}

/// ANSI styling, suppressed wholesale when colour is off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    None,
    BoldGreen,
    BoldCyan,
    BoldYellow,
    BoldRed,
    Bold,
    Dim,
    Green,
    Yellow,
    Red,
}

impl Style {
    fn codes(self) -> &'static str {
        match self {
            Style::None => "",
            Style::BoldGreen => "\x1b[1;32m",
            Style::BoldCyan => "\x1b[1;36m",
            Style::BoldYellow => "\x1b[1;33m",
            Style::BoldRed => "\x1b[1;31m",
            Style::Bold => "\x1b[1m",
            Style::Dim => "\x1b[2m",
            Style::Green => "\x1b[32m",
            Style::Yellow => "\x1b[33m",
            Style::Red => "\x1b[31m",
        }
    }
}

/// Wrap `text` in `style` when `enabled`, otherwise return it untouched.
pub fn paint(enabled: bool, style: Style, text: impl fmt::Display) -> String {
    if !enabled || style == Style::None {
        return text.to_string();
    }
    format!("{}{}\x1b[0m", style.codes(), text)
}

/// The `jrs` wordmark, shown once by `init` and `migrate` (SPEC §5.3.7).
pub const BANNER: &str = concat!(
    "   _\n",
    "  (_)_ __ ___\n",
    "  | | '__/ __|\n",
    "  | | |  \\__ \\\n",
    "  |_|_|  |___/   a Java build system in Rust\n",
);

/// Display width of a string, ignoring ANSI escape sequences.
///
/// jrs only ever draws its own glyphs, all of which are single-width, so a
/// codepoint count is an accurate width here.
#[must_use]
pub fn display_width(s: &str) -> usize {
    let mut width = 0;
    let mut in_escape = false;
    for ch in s.chars() {
        if in_escape {
            if ch.is_ascii_alphabetic() {
                in_escape = false;
            }
            continue;
        }
        if ch == '\x1b' {
            in_escape = true;
            continue;
        }
        width += 1;
    }
    width
}

/// Truncate to `width` columns, preserving escape sequences and resetting style.
///
/// The live region must never wrap: wrapping breaks in-place redraw (SPEC §5.3.1).
#[must_use]
pub fn truncate(s: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if display_width(s) <= width {
        return s.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    let mut in_escape = false;
    let mut styled = false;
    for ch in s.chars() {
        if in_escape {
            out.push(ch);
            if ch.is_ascii_alphabetic() {
                in_escape = false;
            }
            continue;
        }
        if ch == '\x1b' {
            out.push(ch);
            in_escape = true;
            styled = true;
            continue;
        }
        if used + 1 > width.saturating_sub(1) {
            break;
        }
        out.push(ch);
        used += 1;
    }
    out.push('~');
    if styled {
        out.push_str("\x1b[0m");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_unicode_glyph_has_an_ascii_twin() {
        // A structural check: the ASCII set must be pure ASCII, so that a
        // terminal without Unicode never sees a replacement character.
        let a = GlyphSet::for_charset(Charset::Ascii);
        let mut all = vec![
            a.bar_full,
            a.bar_empty,
            a.pass,
            a.fail,
            a.skip,
            a.tee,
            a.elbow,
            a.pipe,
            a.blank,
            a.frame_h,
            a.frame_v,
            a.frame_tl,
            a.frame_tr,
            a.frame_bl,
            a.frame_br,
            a.ellipsis,
        ];
        all.extend_from_slice(a.spinner);
        for g in all {
            assert!(g.is_ascii(), "non-ascii glyph in the ascii set: {g:?}");
        }
    }

    #[test]
    fn tree_glyphs_are_the_same_width_in_both_sets() {
        let u = GlyphSet::for_charset(Charset::Unicode);
        let a = GlyphSet::for_charset(Charset::Ascii);
        assert_eq!(display_width(u.tee), display_width(a.tee));
        assert_eq!(display_width(u.elbow), display_width(a.elbow));
        assert_eq!(display_width(u.pipe), display_width(a.pipe));
        assert_eq!(display_width(u.blank), display_width(a.blank));
    }

    #[test]
    fn bar_fills_proportionally() {
        let g = GlyphSet::for_charset(Charset::Ascii);
        assert_eq!(g.bar(0.0, 4), "----");
        assert_eq!(g.bar(0.5, 4), "##--");
        assert_eq!(g.bar(1.0, 4), "####");
        assert_eq!(g.bar(9.0, 4), "####", "over-full bars are clamped");
    }

    #[test]
    fn spinner_cycles() {
        let g = GlyphSet::for_charset(Charset::Ascii);
        assert_eq!(g.spinner_frame(0), "|");
        assert_eq!(g.spinner_frame(4), "|");
        assert_eq!(g.spinner_frame(5), "/");
    }

    #[test]
    fn width_ignores_escapes() {
        assert_eq!(display_width("\x1b[1;32mabc\x1b[0m"), 3);
        assert_eq!(display_width("abc"), 3);
    }

    #[test]
    fn truncation_marks_the_cut_and_resets_style() {
        assert_eq!(truncate("abcdef", 4), "abc~");
        assert_eq!(truncate("abc", 8), "abc");
        let cut = truncate("\x1b[1;32mabcdef\x1b[0m", 4);
        assert_eq!(display_width(&cut), 4);
        assert!(cut.ends_with("\x1b[0m"));
    }

    #[test]
    fn paint_is_a_no_op_when_colour_is_off() {
        assert_eq!(paint(false, Style::BoldGreen, "Compiling"), "Compiling");
        assert_eq!(
            paint(true, Style::BoldGreen, "Compiling"),
            "\x1b[1;32mCompiling\x1b[0m"
        );
    }
}
