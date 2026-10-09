//! A warm `javac` for `--watch` (SPEC §7.5).
//!
//! A `--watch` session keeps one jrs process alive across rebuilds, and in
//! it, one `javac` worker JVM: started the first time a `javac` step runs,
//! and sent every later one. The worker is `JavacWorker.java`, embedded
//! here and compiled on first use with the project's own `javac` into
//! `<cache>/worker/<jrs version>-jdk<n>/`, so jrs ships no binary and
//! downloads nothing. It runs `javac` in process, through
//! `ToolProvider.getSystemJavaCompiler()`, over the argfile jrs writes for a
//! forked `javac` anyway, and answers with the exit code and both streams,
//! which pass through verbatim as a forked run's do.
//!
//! The worker is not a compiler daemon (SPEC §1.2). It is a child of the
//! jrs process and nothing else can reach it: it reads its requests from a
//! pipe jrs holds and exits when that pipe closes, which happens however jrs
//! exits. jrs starts a new one after [`RESTART_AFTER`] compilations, to
//! bound what a processor's leaked class loaders cost, and whenever the JDK
//! changes. And it may never fail a build: a worker that dies, answers
//! garbage or takes far longer than a forked `javac` would is killed, and
//! that step runs as a forked `javac`, whose output alone reaches the
//! terminal.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Mutex;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use super::share::Share;
use crate::toolchain::{CapturedOutput, Toolchain};
use crate::ui::Ui;

/// The worker's source, compiled on first use.
const SOURCE: &str = include_str!("JavacWorker.java");

/// The worker's main class.
const MAIN_CLASS: &str = "JavacWorker";

/// How many compilations one worker JVM serves before a new one takes over.
pub const RESTART_AFTER: u32 = 50;

/// The least a request may take before the worker counts as hung; past it,
/// ten times the slowest compile the worker has answered.
const PATIENCE: Duration = Duration::from_secs(120);

/// The `javac` worker of one `--watch` session.
pub struct Worker {
    /// `<cache>/worker`.
    dir: PathBuf,
    running: Mutex<Option<Running>>,
}

impl std::fmt::Debug for Worker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Worker")
            .field("dir", &self.dir)
            .finish_non_exhaustive()
    }
}

/// A worker JVM and the pipes to it.
struct Running {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: Option<BufReader<ChildStdout>>,
    /// The `javac` whose JDK it runs on.
    javac: PathBuf,
    compilations: u32,
    slowest: Duration,
    /// Its class-data-sharing archive, put in place once it exits.
    share: Option<Share>,
}

impl Worker {
    /// A worker whose program and archive live under `dir`. Nothing starts
    /// until the first compile.
    #[must_use]
    pub fn new(dir: PathBuf) -> Worker {
        Worker {
            dir,
            running: Mutex::new(None),
        }
    }

    /// Compile with the argfile at `argfile`, as `javac @argfile` would.
    /// `None` when the worker could not answer: the caller forks `javac`
    /// instead, and the next request starts a new worker.
    ///
    /// # Panics
    ///
    /// If a thread panicked while holding the lock on the worker.
    pub fn compile(
        &self,
        toolchain: &Toolchain,
        argfile: &Path,
        ui: &Ui,
    ) -> Option<CapturedOutput> {
        let mut running = self.running.lock().unwrap();
        let stale = running
            .as_ref()
            .is_some_and(|r| r.javac != toolchain.javac || r.compilations >= RESTART_AFTER);
        if stale && let Some(old) = running.take() {
            old.stop();
        }
        if running.is_none() {
            match self.start(toolchain, ui) {
                Ok(started) => *running = Some(started),
                Err(why) => {
                    ui.verbose(format!("javac worker: not started: {why}"));
                    return None;
                }
            }
        }
        let worker = running.as_mut()?;
        ui.verbose(format!(
            "javac worker (pid {}): @{}",
            worker.child.id(),
            argfile.display()
        ));
        match worker.request(argfile) {
            Ok(output) => Some(output),
            Err(why) => {
                ui.verbose(format!(
                    "javac worker: {why}; it is stopped, and javac runs on its own"
                ));
                if let Some(mut failed) = running.take() {
                    let _ = failed.child.kill();
                    let _ = failed.child.wait();
                    if let Some(share) = failed.share.take() {
                        share.finish(false);
                    }
                }
                None
            }
        }
    }

    /// The worker's process id, while one runs.
    ///
    /// # Panics
    ///
    /// If a thread panicked while holding the lock on the worker.
    #[must_use]
    pub fn pid(&self) -> Option<u32> {
        self.running.lock().unwrap().as_ref().map(|r| r.child.id())
    }

    /// Start a worker JVM on `toolchain`'s JDK, compiling its program first
    /// if this jrs and JDK have not.
    fn start(&self, toolchain: &Toolchain, ui: &Ui) -> Result<Running, String> {
        let jar = self.program(toolchain)?;
        let share = Share::new(
            &self.dir.join("cds"),
            toolchain,
            MAIN_CLASS,
            std::slice::from_ref(&jar),
            &[],
        );
        let mut command = Command::new(&toolchain.java);
        if let Some(share) = &share {
            command.args(&share.flags);
        }
        let mut child = command
            .arg("-cp")
            .arg(&jar)
            .arg(MAIN_CLASS)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Nothing of the worker's own may reach the terminal.
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("{}: {e}", toolchain.java.display()))?;
        ui.verbose(format!("started a javac worker (pid {})", child.id()));
        Ok(Running {
            stdin: child.stdin.take(),
            stdout: child.stdout.take().map(BufReader::new),
            child,
            javac: toolchain.javac.clone(),
            compilations: 0,
            slowest: Duration::ZERO,
            share,
        })
    }

    /// The worker's program as a jar — a class directory could not be
    /// mapped from a class-data-sharing archive — compiled with
    /// `toolchain`'s `javac` once per jrs version and JDK.
    fn program(&self, toolchain: &Toolchain) -> Result<PathBuf, String> {
        let home = self.dir.join(format!(
            "{}-jdk{}",
            env!("CARGO_PKG_VERSION"),
            toolchain.version
        ));
        let jar = home.join("worker.jar");
        if jar.is_file() {
            return Ok(jar);
        }
        let work = home.join(format!(".build-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&work);
        let classes = work.join("classes");
        std::fs::create_dir_all(&classes).map_err(|e| format!("{}: {e}", classes.display()))?;
        let source = work.join(format!("{MAIN_CLASS}.java"));
        std::fs::write(&source, SOURCE).map_err(|e| format!("{}: {e}", source.display()))?;
        let output = Command::new(&toolchain.javac)
            .arg("-d")
            .arg(&classes)
            .arg(&source)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("{}: {e}", toolchain.javac.display()))?;
        if !output.status.success() {
            let _ = std::fs::remove_dir_all(&work);
            return Err(format!(
                "its program did not compile: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        let built = work.join("worker.jar");
        crate::package::write_thin_jar(&classes, &built, &crate::package::JarManifest::default())
            .map_err(|e| e.to_string())?;
        // Another session may have put one in place meanwhile; either will do.
        let renamed = std::fs::rename(&built, &jar);
        let _ = std::fs::remove_dir_all(&work);
        renamed.map_err(|e| format!("{}: {e}", jar.display()))?;
        Ok(jar)
    }
}

impl Running {
    /// Send one request and read its answer, within the worker's patience.
    fn request(&mut self, argfile: &Path) -> Result<CapturedOutput, String> {
        self.compilations += 1;
        let id = self.compilations;
        let stdin = self.stdin.as_mut().ok_or("its stdin is gone")?;
        writeln!(stdin, "{id} {}", argfile.display())
            .and_then(|()| stdin.flush())
            .map_err(|e| format!("the request could not be sent ({e})"))?;
        let mut stdout = self.stdout.take().ok_or("its stdout is gone")?;
        let started = Instant::now();
        let patience = PATIENCE.max(self.slowest * 10);
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let answer = read_answer(&mut stdout, id);
            let _ = tx.send((stdout, answer));
        });
        let (stdout, answer) = rx
            .recv_timeout(patience)
            .map_err(|_| format!("no answer within {}s", patience.as_secs()))?;
        self.stdout = Some(stdout);
        let output = answer?;
        self.slowest = self.slowest.max(started.elapsed());
        Ok(output)
    }

    /// Close the worker's stdin, which ends it, and put its archive in
    /// place once it has exited on its own.
    fn stop(mut self) {
        drop(self.stdin.take());
        let deadline = Instant::now() + Duration::from_secs(10);
        let exited = loop {
            match self.child.try_wait() {
                Ok(Some(status)) => break status.success(),
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                _ => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    break false;
                }
            }
        };
        if let Some(share) = self.share.take() {
            share.finish(exited);
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        if let Ok(mut running) = self.running.lock()
            && let Some(running) = running.take()
        {
            running.stop();
        }
    }
}

/// Read the answer to request `id`: the header line, then both streams.
fn read_answer(stdout: &mut BufReader<ChildStdout>, id: u32) -> Result<CapturedOutput, String> {
    let mut header = String::new();
    let read = stdout
        .read_line(&mut header)
        .map_err(|e| format!("its answer could not be read ({e})"))?;
    if read == 0 {
        return Err("it exited".to_string());
    }
    let fields: Vec<&str> = header.trim_end().split(' ').collect();
    let [tag, answered, code, out, err] = fields[..] else {
        return Err(format!("it answered garbage: {:?}", header.trim_end()));
    };
    let parsed = (
        answered.parse::<u32>(),
        code.parse::<i32>(),
        out.parse::<usize>(),
        err.parse::<usize>(),
    );
    let (Ok(answered), Ok(status), Ok(out), Ok(err)) = parsed else {
        return Err(format!("it answered garbage: {:?}", header.trim_end()));
    };
    if tag != "jrs-worker" || answered != id {
        return Err(format!("it answered garbage: {:?}", header.trim_end()));
    }
    let mut take = |n: usize| -> Result<String, String> {
        let mut bytes = vec![0; n];
        stdout
            .read_exact(&mut bytes)
            .map_err(|e| format!("its answer broke off ({e})"))?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    };
    let stdout = take(out)?;
    let stderr = take(err)?;
    Ok(CapturedOutput {
        status,
        stdout,
        stderr,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::{CharsetChoice, Geometry, UiOptions, When};

    struct Tree {
        root: PathBuf,
    }

    impl Tree {
        fn new(name: &str) -> Tree {
            let root =
                std::env::temp_dir().join(format!("jrs-worker-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap();
            Tree { root }
        }

        /// An argfile compiling `source`, holding `body`, into `out`.
        fn argfile(&self, name: &str, body: &str) -> PathBuf {
            let source = self.root.join(format!("src/{name}.java"));
            std::fs::create_dir_all(source.parent().unwrap()).unwrap();
            std::fs::write(&source, body).unwrap();
            let argfile = self.root.join(format!("{name}.args"));
            std::fs::write(
                &argfile,
                super::super::render_argfile(
                    &[
                        "-d".to_string(),
                        self.root.join("out").display().to_string(),
                    ],
                    &[source],
                ),
            )
            .unwrap();
            argfile
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn ui() -> Ui {
        Ui::captured(
            UiOptions {
                progress: When::Never,
                color: When::Never,
                charset: CharsetChoice::Ascii,
                ..Default::default()
            },
            Geometry {
                width: 100,
                height: 24,
            },
        )
        .0
    }

    fn forked(toolchain: &Toolchain, argfile: &Path) -> CapturedOutput {
        crate::toolchain::run_captured(
            &ui(),
            &toolchain.javac,
            &[format!("@{}", argfile.display())],
        )
        .unwrap()
    }

    #[test]
    fn one_worker_answers_every_request_as_a_forked_javac_would() {
        let Ok(toolchain) = Toolchain::discover() else {
            eprintln!("SKIPPED: no usable JDK");
            return;
        };
        let tree = Tree::new("protocol");
        let worker = Worker::new(tree.root.join("cache/worker"));
        let ui = ui();
        for i in 0..3 {
            let argfile = tree.argfile(&format!("Ok{i}"), &format!("class Ok{i} {{}}\n"));
            let output = worker.compile(&toolchain, &argfile, &ui).unwrap();
            assert!(output.ok(), "{}", output.stderr);
            assert!(tree.root.join(format!("out/Ok{i}.class")).is_file());
        }
        let pid = worker.pid().unwrap();

        let broken = tree.argfile("Broken", "class Broken { int x = \"no\"; }\n");
        let warm = worker.compile(&toolchain, &broken, &ui).unwrap();
        let cold = forked(&toolchain, &broken);
        assert_eq!(warm.status, cold.status);
        assert_ne!(warm.status, 0);
        assert_eq!(warm.stderr, cold.stderr, "the diagnostics, byte for byte");
        assert_eq!(warm.stdout, cold.stdout);
        assert_eq!(worker.pid(), Some(pid), "one worker served them all");
    }

    #[test]
    fn a_killed_worker_hands_the_step_back_and_a_new_one_takes_over() {
        let Ok(toolchain) = Toolchain::discover() else {
            eprintln!("SKIPPED: no usable JDK");
            return;
        };
        let tree = Tree::new("killed");
        let worker = Worker::new(tree.root.join("cache/worker"));
        let ui = ui();
        let argfile = tree.argfile("Fine", "class Fine {}\n");
        assert!(worker.compile(&toolchain, &argfile, &ui).unwrap().ok());
        let pid = worker.pid().unwrap();
        worker
            .running
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .child
            .kill()
            .unwrap();
        assert!(
            worker.compile(&toolchain, &argfile, &ui).is_none(),
            "the caller forks javac instead"
        );
        assert!(worker.pid().is_none());
        assert!(worker.compile(&toolchain, &argfile, &ui).unwrap().ok());
        assert_ne!(worker.pid(), Some(pid), "a new worker");
    }
}
