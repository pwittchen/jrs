//! `--watch`: rebuilding, retesting or restarting on every change.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use super::session::Session;
use super::{Cli, manifest_path, report};
use crate::compile::worker::Worker;
use crate::error::Result;
use crate::project::Snapshot;
use crate::resolve::cache::Cache;
use crate::ui::Ui;
use crate::{runner, toolchain};

/// How often `--watch` looks for changes. The walk is the same sorted one a
/// build does, so a few times a second costs nothing noticeable.
const WATCH_INTERVAL: Duration = Duration::from_millis(300);

/// Run a command, then again every time a watched file changes, until
/// interrupted. A failure is reported and waited out rather than ending the
/// loop: the next save is usually the fix.
pub(super) fn watch(cli: &Cli, ui: &Ui, run: impl Fn(&Session) -> Result<i32>) -> Result<i32> {
    // One warm `javac` for the whole session: a child of this process, which
    // nothing else can reach and which exits with it (SPEC §7.5).
    let worker = Cache::discover()
        .ok()
        .map(|cache| Arc::new(Worker::new(cache.root().join("worker"))));
    loop {
        let watched = match Session::open(cli, ui).map(|s| s.with_worker(worker.clone())) {
            Ok(session) => {
                if let Err(e) = run(&session) {
                    report(ui, &e);
                }
                session.watched_paths(true)
            }
            Err(e) => {
                // A manifest that does not parse is worth waiting on too.
                report(ui, &e);
                vec![manifest_path(cli)?]
            }
        };
        ui.phase("Watching", "for changes (ctrl-c to stop)");
        wait_for_change(ui, &watched, || {});
    }
}

/// How long a program `jrs run --watch` restarts has to shut down after
/// `SIGTERM` before it is killed.
pub(super) const RESTART_GRACE: Duration = Duration::from_secs(10);

/// `jrs run --watch`: build and start the program, and when a watched file
/// changes, stop it, build again and start it again — what Spring's
/// devtools or a Ktor `-t` build give a developer. A program that exits by
/// itself, or a build that fails, is waited out until the next change.
pub(super) fn run_watch(
    cli: &Cli,
    ui: &Ui,
    args: &[String],
    debug: Option<&runner::DebugAddress>,
) -> Result<i32> {
    let worker = Cache::discover()
        .ok()
        .map(|cache| Arc::new(Worker::new(cache.root().join("worker"))));
    loop {
        let session = match Session::open(cli, ui).map(|s| s.with_worker(worker.clone())) {
            Ok(session) => session,
            Err(e) => {
                report(ui, &e);
                ui.phase("Watching", "for changes (ctrl-c to stop)");
                wait_for_change(ui, &[manifest_path(cli)?], || {});
                continue;
            }
        };
        let watched = session.watched_paths(false);
        let started = session.launch(args, debug).and_then(|launch| {
            let child =
                toolchain::spawn_inherited_in(ui, &launch.java, &launch.args, &launch.environment)?;
            Ok((launch.main_class, child))
        });
        let (main_class, mut child) = match started {
            Ok(started) => started,
            Err(e) => {
                report(ui, &e);
                ui.phase("Watching", "for changes (ctrl-c to stop)");
                wait_for_change(ui, &watched, || {});
                continue;
            }
        };
        let mut exited = None;
        wait_for_change(ui, &watched, || {
            if exited.is_none() {
                exited = child
                    .try_wait()
                    .ok()
                    .flatten()
                    .map(|s| s.code().unwrap_or(-1));
                if let Some(code) = exited {
                    ui.phase("Finished", format!("{main_class} exited with {code}"));
                    ui.phase("Watching", "for changes (ctrl-c to stop)");
                }
            }
        });
        if exited.is_none() {
            ui.phase("Stopping", format!("{main_class} to restart it"));
            toolchain::stop(&mut child, RESTART_GRACE);
        }
    }
}

/// Wait until a file under `paths` changes, and say which. `between` runs
/// before every look, which is how `jrs run --watch` notices its program
/// exiting while it waits.
pub(super) fn wait_for_change(ui: &Ui, paths: &[PathBuf], mut between: impl FnMut()) {
    let before = Snapshot::take(paths);
    loop {
        std::thread::sleep(WATCH_INTERVAL);
        between();
        let mut now = Snapshot::take(paths);
        if now == before {
            continue;
        }
        // Editors save in more than one step; let the tree settle first.
        loop {
            std::thread::sleep(WATCH_INTERVAL / 3);
            let again = Snapshot::take(paths);
            if again == now {
                break;
            }
            now = again;
        }
        if let Some(path) = before.first_difference(&now) {
            ui.phase("Changed", path.display());
        }
        return;
    }
}
