//! Snapshot tests for the terminal UI.
//!
//! The whole point of the `ui` boundary is that a build's output can be produced
//! and inspected without a TTY (SPEC §10.1). These tests drive a scripted build
//! through the real output layer with a fixed terminal width and a clock that
//! only moves when the test says so, then assert on the bytes.

use jrs::ui::{
    CharsetChoice, DownloadState, Geometry, Live, TestState, Transfer, Ui, UiOptions, When,
    glyphs::{self, Outcome},
};

const WIDTH: usize = 60;

fn options(progress: When, charset: CharsetChoice) -> UiOptions {
    UiOptions {
        progress,
        color: When::Never,
        charset,
        jobs: 3,
        ..Default::default()
    }
}

fn geometry(width: usize) -> Geometry {
    Geometry { width, height: 24 }
}

/// The same scripted build, played through whatever `Ui` it is given.
fn play_a_build(ui: &Ui, frames_per_phase: usize) {
    ui.phase("Resolving", "2 declared dependencies");
    let scope = ui.spinner("Resolving", "2 declared dependencies");
    for _ in 0..frames_per_phase {
        ui.render_frame();
    }
    scope.finish();

    ui.phase("Downloading", "3 artifacts");
    let scope = ui.downloads(3);
    ui.update_live(|live| {
        if let Live::Downloads(d) = live {
            d.active = vec![
                Transfer {
                    id: 1,
                    name: "guava-33.0.0-jre.jar".into(),
                    done: 1_992_294,
                    total: Some(3_250_585),
                    verifying: false,
                },
                Transfer {
                    id: 2,
                    name: "commons-lang3-3.14.0.jar".into(),
                    done: 209_715,
                    total: Some(629_145),
                    verifying: false,
                },
                Transfer {
                    id: 3,
                    name: "checker-qual-3.42.0.jar".into(),
                    done: 100,
                    total: Some(100),
                    verifying: true,
                },
            ];
        }
    });
    for _ in 0..frames_per_phase {
        ui.render_frame();
    }
    scope.finish();

    ui.phase("Compiling", "my-app v1.0.0 (47 source files)");
    let scope = ui.spinner("Compiling", "47 source files");
    for _ in 0..frames_per_phase {
        ui.render_frame();
    }
    scope.finish();

    ui.phase("Finished", "build in 2.31s");
    ui.summary(&[
        ("build", "ok      47 classes".into()),
        ("deps", "14      3 downloaded".into()),
        ("time", "2.31s".into()),
    ]);
}

/// Strip escape sequences, so a transcript can be read as text.
fn plain_text(raw: &str) -> String {
    let mut out = String::new();
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        for skipped in chars.by_ref() {
            if skipped.is_ascii_alphabetic() {
                break;
            }
        }
    }
    out
}

#[test]
fn the_plain_transcript_is_a_log() {
    let (ui, capture) = Ui::captured(options(When::Never, CharsetChoice::Ascii), geometry(WIDTH));
    play_a_build(&ui, 3);

    assert_eq!(
        capture.stderr(),
        concat!(
            "   Resolving 2 declared dependencies\n",
            " Downloading 3 artifacts\n",
            "   Compiling my-app v1.0.0 (47 source files)\n",
            "    Finished build in 2.31s\n",
            "       build ok      47 classes\n",
            "        deps 14      3 downloaded\n",
            "        time 2.31s\n",
        )
    );
    assert!(
        !capture.stderr().contains('\x1b'),
        "plain mode must emit no escape sequences"
    );
    assert_eq!(capture.stdout(), "", "progress never goes to stdout");
}

#[test]
fn the_animated_transcript_leaves_the_same_scrollback() {
    let (plain, plain_capture) =
        Ui::captured(options(When::Never, CharsetChoice::Ascii), geometry(WIDTH));
    play_a_build(&plain, 3);

    let (animated, animated_capture) =
        Ui::captured(options(When::Always, CharsetChoice::Ascii), geometry(WIDTH));
    play_a_build(&animated, 3);

    // What survives in the scrollback is every permanent line; the animated run
    // adds frames on top of it, and the summary frame at the end.
    let scrollback = plain_capture.stderr();
    let animated_text = plain_text(&animated_capture.stderr());
    for line in scrollback.lines().take(4) {
        assert!(
            animated_text.contains(line),
            "the animated run lost the permanent line {line:?}"
        );
    }
}

#[test]
fn animation_frames_advance_and_redraw_in_place() {
    let (ui, capture) = Ui::captured(options(When::Always, CharsetChoice::Ascii), geometry(WIDTH));
    let scope = ui.spinner("Resolving", "14 dependencies");
    for _ in 0..4 {
        ui.render_frame();
    }
    scope.finish();

    let raw = capture.stderr();
    for frame in ["|", "/", "-", "\\"] {
        assert!(
            raw.contains(&format!("   Resolving {frame} 14 dependencies")),
            "missing spinner frame {frame:?}"
        );
    }
    assert!(raw.contains("\x1b[1A\x1b[0J"), "no in-place redraw");
    assert!(raw.starts_with("\x1b[?25l"), "the cursor was never hidden");
    assert!(raw.ends_with("\x1b[?25h"), "the cursor was never restored");
}

#[test]
fn download_bars_render_one_line_per_transfer() {
    let (ui, capture) = Ui::captured(options(When::Always, CharsetChoice::Ascii), geometry(80));
    let scope = ui.downloads(14);
    ui.update_live(|live| {
        if let Live::Downloads(d) = live {
            *d = DownloadState {
                total: 14,
                finished: 5,
                active: vec![
                    Transfer {
                        id: 1,
                        name: "guava-33.0.0-jre.jar".into(),
                        done: 1_992_294,
                        total: Some(3_250_585),
                        verifying: false,
                    },
                    Transfer {
                        id: 2,
                        name: "checker-qual-3.42.0.jar".into(),
                        done: 100,
                        total: Some(100),
                        verifying: true,
                    },
                ],
            };
        }
    });
    ui.render_frame();
    scope.finish();

    // The scope draws an opening frame before the transfers are published, so
    // the frame to assert on is the last one.
    let text = plain_text(&capture.stderr());
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let frame = &lines[lines.len() - 3..];
    assert_eq!(frame[0], " Downloading | 5/14");
    assert!(frame[1].contains("61%"), "{:?}", frame[1]);
    assert!(frame[1].contains("guava-33.0.0-jre.jar"), "{:?}", frame[1]);
    assert!(frame[1].contains("1.9/3.1 MB"), "{:?}", frame[1]);
    assert!(frame[2].contains("100%"), "{:?}", frame[2]);
    assert!(frame[2].ends_with("verifying..."), "{:?}", frame[2]);
}

#[test]
fn the_test_counter_shows_marks_and_a_tally() {
    let (ui, capture) = Ui::captured(options(When::Always, CharsetChoice::Unicode), geometry(80));
    let scope = ui.tests();
    ui.update_live(|live| {
        if let Live::Tests(state) = live {
            *state = TestState {
                marks: vec![Outcome::Pass, Outcome::Pass, Outcome::Fail, Outcome::Pass],
                passed: 3,
                failed: 1,
                skipped: 0,
            };
        }
    });
    ui.render_frame();
    scope.finish();

    let text = plain_text(&capture.stderr());
    let line = text.lines().rfind(|l| l.contains("Testing")).unwrap();
    assert!(line.contains("✔✔✘✔"), "{line:?}");
    assert!(line.ends_with("(3 passed, 1 failed)"), "{line:?}");
}

#[test]
fn the_ascii_fallback_never_emits_a_non_ascii_byte() {
    let (ui, capture) = Ui::captured(options(When::Always, CharsetChoice::Ascii), geometry(WIDTH));
    play_a_build(&ui, 2);

    let scope = ui.tests();
    ui.update_live(|live| {
        if let Live::Tests(state) = live {
            state.marks = vec![Outcome::Pass, Outcome::Fail, Outcome::Skip];
            state.passed = 1;
            state.failed = 1;
            state.skipped = 1;
        }
    });
    ui.render_frame();
    scope.finish();

    let raw = capture.stderr();
    assert!(
        raw.is_ascii(),
        "the ascii charset produced non-ascii output: {:?}",
        raw.chars().find(|c| !c.is_ascii())
    );
    assert!(raw.contains("+-"), "the ascii marks are missing");
}

#[test]
fn the_unicode_summary_frame_is_rectangular() {
    let (ui, capture) = Ui::captured(
        options(When::Always, CharsetChoice::Unicode),
        geometry(WIDTH),
    );
    ui.summary(&[
        ("build", "ok      47 classes".into()),
        ("deps", "14      3 downloaded".into()),
        ("jar", "my-app-1.0.0.jar   412 KB".into()),
        ("time", "2.31s".into()),
    ]);

    let rendered = capture.stderr();
    let lines: Vec<&str> = rendered.lines().collect();
    let widths: Vec<usize> = lines.iter().map(|l| glyphs::display_width(l)).collect();
    assert!(
        widths.windows(2).all(|w| w[0] == w[1]),
        "ragged frame: {widths:?} in {lines:#?}"
    );
    assert!(lines[0].contains("┌─ jrs "));
    assert!(lines.last().unwrap().contains('┘'));
}

#[test]
fn a_narrow_terminal_truncates_instead_of_wrapping() {
    // Wrapping would break in-place redraw, so the live region must never exceed
    // the terminal width.
    for width in [20, 30, 40] {
        let (ui, capture) =
            Ui::captured(options(When::Always, CharsetChoice::Ascii), geometry(width));
        let scope = ui.downloads(14);
        ui.update_live(|live| {
            if let Live::Downloads(d) = live {
                d.finished = 5;
                d.active = vec![Transfer {
                    id: 1,
                    name: "a-very-long-artifact-name-1.2.3-with-a-classifier.jar".into(),
                    done: 1,
                    total: Some(2),
                    verifying: false,
                }];
            }
        });
        ui.render_frame();
        scope.finish();

        for line in plain_text(&capture.stderr()).lines() {
            assert!(
                line.chars().count() <= width,
                "a {} column line in a {width} column terminal: {line:?}",
                line.chars().count()
            );
        }
    }
}

#[test]
fn quiet_output_is_empty_until_something_goes_wrong() {
    let mut opts = options(When::Never, CharsetChoice::Ascii);
    opts.quiet = true;
    let (ui, capture) = Ui::captured(opts, geometry(WIDTH));
    play_a_build(&ui, 2);
    assert_eq!(capture.stderr(), "");

    ui.error("compilation failed (47 source files)");
    assert_eq!(
        capture.stderr(),
        "error: compilation failed (47 source files)\n"
    );
}

#[test]
fn toolchain_output_is_passed_through_after_the_live_region_comes_down() {
    let (ui, capture) = Ui::captured(options(When::Always, CharsetChoice::Ascii), geometry(WIDTH));
    let scope = ui.spinner("Compiling", "47 source files");
    ui.render_frame();
    ui.passthrough(
        jrs::ui::Stream::Err,
        "Main.java:3: error: cannot find symbol\n  Foo bar;\n  ^",
    );
    scope.finish();

    let raw = capture.stderr();
    let diagnostic = raw.find("Main.java:3").unwrap();
    let erase = raw[..diagnostic].rfind("\x1b[1A\x1b[0J").unwrap();
    assert!(
        erase < diagnostic,
        "a diagnostic must land in a clean terminal"
    );
    // Every line of it survives, verbatim.
    for line in [
        "Main.java:3: error: cannot find symbol",
        "  Foo bar;",
        "  ^",
    ] {
        assert!(raw.contains(line), "lost {line:?}");
    }
}
