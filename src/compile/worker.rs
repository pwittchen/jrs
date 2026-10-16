//! Warm compilers for `--watch` (SPEC §7.5): a `javac` worker, and a
//! `kotlinc` one; and `javac` workers for a workspace command, which
//! compiles member after member (`specs/todo/BEATING_THE_DAEMON.md` §6).
//!
//! A `--watch` session keeps one jrs process alive across rebuilds, and in
//! it, worker JVMs per compiler: started the first time that compiler's
//! step runs, and sent every later one. Each kind is a pool, so members
//! compiling at once get a JVM each, and a member compiling after another
//! reuses one, with the JIT the first one warmed. A workspace command that
//! is not `--watch` holds a pool of `javac` workers the same way, for as
//! long as it runs. The `javac` worker is
//! `JavacWorker.java`, embedded here and compiled on first use with the
//! project's own `javac` into `<cache>/worker/<jrs version>-jdk<n>/`, so jrs
//! ships no binary and downloads nothing. It runs `javac` in process,
//! through `ToolProvider.getSystemJavaCompiler()`, over the argfile jrs
//! writes for a forked `javac` anyway, and answers with the exit code and
//! both streams, which pass through verbatim as a forked run's do.
//!
//! The `kotlinc` worker is `KotlincWorker.java`, compiled the same way into
//! `<cache>/worker/kotlinc-<jrs version>-jdk<n>/` and run with the Kotlin
//! compiler's jars on its classpath. It reaches the compiler by reflection,
//! so one program serves every Kotlin version, and reads the argfile that
//! holds the compiler's own arguments — the file a forked `kotlinc` reads as
//! `@argfile` too. A third kind would be a Java file and a main class.
//!
//! A worker is not a compiler daemon (SPEC §1.2). It is a child of the
//! jrs command and nothing else can reach it: it reads its requests from a
//! pipe jrs holds and exits when that pipe closes, which happens however jrs
//! exits. jrs starts a new one after a number of compilations of its kind
//! ([`RESTART_AFTER`], [`KOTLINC_RESTART_AFTER`]), to bound what leaked class
//! loaders and static state cost, and whenever the JDK or the compiler
//! changes. And it may never fail a build: a worker that dies, answers
//! garbage or takes far longer than a forked run would is killed, and that
//! step runs as a forked compiler, whose output alone reaches the terminal.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Mutex;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use super::share::Share;
use crate::toolchain::{CapturedOutput, Toolchain};
use crate::ui::Ui;

/// How many compilations one `javac` worker JVM serves before a new one
/// takes over.
pub const RESTART_AFTER: u32 = 50;

/// The same for `kotlinc`, which keeps more static state than `javac` from
/// one compile to the next.
pub const KOTLINC_RESTART_AFTER: u32 = 20;

/// The least a request may take before the worker counts as hung; past it,
/// ten times the slowest compile the worker has answered.
const PATIENCE: Duration = Duration::from_secs(120);

/// Which compiler a worker JVM runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Javac,
    Kotlinc,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Javac => "javac",
            Kind::Kotlinc => "kotlinc",
        }
    }

    /// The worker's source, compiled on first use.
    fn source(self) -> &'static str {
        match self {
            Kind::Javac => include_str!("JavacWorker.java"),
            Kind::Kotlinc => include_str!("KotlincWorker.java"),
        }
    }

    /// The worker's main class.
    fn main_class(self) -> &'static str {
        match self {
            Kind::Javac => "JavacWorker",
            Kind::Kotlinc => "KotlincWorker",
        }
    }

    fn restart_after(self) -> u32 {
        match self {
            Kind::Javac => RESTART_AFTER,
            Kind::Kotlinc => KOTLINC_RESTART_AFTER,
        }
    }

    /// The directory under `<cache>/worker` its program is compiled into.
    fn program_dir(self, jdk: u32) -> String {
        let version = env!("CARGO_PKG_VERSION");
        match self {
            Kind::Javac => format!("{version}-jdk{jdk}"),
            Kind::Kotlinc => format!("kotlinc-{version}-jdk{jdk}"),
        }
    }
}

/// How to start a worker JVM, and what makes a running one stale.
struct Launch<'a> {
    toolchain: &'a Toolchain,
    /// The compiler's own jars, beside the worker's program.
    classpath: &'a [PathBuf],
    /// Flags for the worker's JVM.
    jvm_args: &'a [String],
    /// Arguments to the worker's program.
    args: &'a [&'a str],
    /// What a worker started with these settings is known by: a request
    /// with another key replaces it.
    key: String,
}

/// The warm compilers of one `--watch` session, or of one command that
/// compiles more than once.
///
/// Each kind is a pool: a request takes an idle worker JVM, or starts one
/// when none is idle, and gives it back once answered, so members built at
/// once each compile in a JVM of their own, and one built after another
/// reuses the JVM — and the JIT — the first one warmed.
pub struct Worker {
    /// `<cache>/worker`.
    dir: PathBuf,
    /// Idle `javac` workers.
    running: Mutex<Vec<Running>>,
    /// Idle `kotlinc` workers.
    kotlinc: Mutex<Vec<Running>>,
    /// Whether `kotlinc` steps are served too, or always forked.
    serves_kotlinc: bool,
    /// Held while a worker's program is compiled: workers start outside
    /// the pools' locks, and two must not build it in one directory.
    building: Mutex<()>,
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
    /// [`Launch::key`] of the settings it was started with.
    key: String,
    compilations: u32,
    slowest: Duration,
    /// Its class-data-sharing archive, put in place once it exits.
    share: Option<Share>,
}

impl Worker {
    /// Workers whose programs and archives live under `dir`. Nothing starts
    /// until the first compile.
    #[must_use]
    pub fn new(dir: PathBuf) -> Worker {
        Worker {
            dir,
            running: Mutex::new(Vec::new()),
            kotlinc: Mutex::new(Vec::new()),
            serves_kotlinc: true,
            building: Mutex::new(()),
        }
    }

    /// Workers for one command, not a `--watch` session: `javac` alone, since
    /// `kotlinc` keeps static state from one compile to the next that only
    /// a session that rebuilds one project should carry.
    #[must_use]
    pub fn javac_only(dir: PathBuf) -> Worker {
        let mut worker = Worker::new(dir);
        worker.serves_kotlinc = false;
        worker
    }

    /// Compile with the argfile at `argfile`, as `javac @argfile` would.
    /// `None` when the worker could not answer: the caller forks `javac`
    /// instead, and the next request starts a new worker.
    pub fn compile(
        &self,
        toolchain: &Toolchain,
        argfile: &Path,
        ui: &Ui,
    ) -> Option<CapturedOutput> {
        let launch = Launch {
            toolchain,
            classpath: &[],
            jvm_args: &[],
            args: &[],
            key: toolchain.javac.display().to_string(),
        };
        self.serve(Kind::Javac, &self.running, &launch, argfile, ui)
    }

    /// Run the Kotlin compiler `main_class`, from `classpath`, in a JVM
    /// started with `jvm_args`, over the compiler argfile at `argfile` — as
    /// `java -cp <classpath> <main_class> @argfile` would. `None` when the
    /// worker could not answer, as for [`Worker::compile`].
    pub fn kotlinc(
        &self,
        toolchain: &Toolchain,
        classpath: &[PathBuf],
        jvm_args: &[String],
        main_class: &str,
        argfile: &Path,
        ui: &Ui,
    ) -> Option<CapturedOutput> {
        if !self.serves_kotlinc {
            return None;
        }
        let key = format!(
            "{}\n{}\n{}\n{main_class}",
            toolchain.javac.display(),
            Toolchain::classpath(classpath),
            jvm_args.join("\n")
        );
        let launch = Launch {
            toolchain,
            classpath,
            jvm_args,
            args: &[main_class],
            key,
        };
        self.serve(Kind::Kotlinc, &self.kotlinc, &launch, argfile, ui)
    }

    fn serve(
        &self,
        kind: Kind,
        slot: &Mutex<Vec<Running>>,
        launch: &Launch<'_>,
        argfile: &Path,
        ui: &Ui,
    ) -> Option<CapturedOutput> {
        let name = kind.name();
        let (idle, stale) = {
            let mut pool = slot.lock().unwrap();
            let mut stale = Vec::new();
            let mut idle = None;
            while let Some(running) = pool.pop() {
                if running.key == launch.key && running.compilations < kind.restart_after() {
                    idle = Some(running);
                    break;
                }
                stale.push(running);
            }
            (idle, stale)
        };
        for old in stale {
            old.stop();
        }
        let mut worker = match idle {
            Some(worker) => worker,
            None => match self.start(kind, launch, ui) {
                Ok(started) => started,
                Err(why) => {
                    ui.verbose(format!("{name} worker: not started: {why}"));
                    return None;
                }
            },
        };
        ui.verbose(format!(
            "{name} worker (pid {}): @{}",
            worker.child.id(),
            argfile.display()
        ));
        match worker.request(argfile) {
            Ok(output) => {
                slot.lock().unwrap().push(worker);
                Some(output)
            }
            Err(why) => {
                ui.verbose(format!(
                    "{name} worker: {why}; it is stopped, and {name} runs on its own"
                ));
                let _ = worker.child.kill();
                let _ = worker.child.wait();
                if let Some(share) = worker.share.take() {
                    share.finish(false);
                }
                None
            }
        }
    }

    /// An idle `javac` worker's process id, while one runs.
    ///
    /// # Panics
    ///
    /// If a thread panicked while holding the lock on the worker.
    #[must_use]
    pub fn pid(&self) -> Option<u32> {
        self.running.lock().unwrap().last().map(|r| r.child.id())
    }

    /// An idle `kotlinc` worker's process id, while one runs.
    ///
    /// # Panics
    ///
    /// If a thread panicked while holding the lock on the worker.
    #[must_use]
    pub fn kotlinc_pid(&self) -> Option<u32> {
        self.kotlinc.lock().unwrap().last().map(|r| r.child.id())
    }

    /// Start a worker JVM of `kind` on `launch`'s JDK, compiling its program
    /// first if this jrs and JDK have not.
    fn start(&self, kind: Kind, launch: &Launch<'_>, ui: &Ui) -> Result<Running, String> {
        let toolchain = launch.toolchain;
        let jar = self.program(kind, toolchain)?;
        // The `kotlinc` worker loads the compiler's classes, which the
        // compiler's own archive already maps for forked runs.
        let share = (kind == Kind::Javac)
            .then(|| {
                Share::new(
                    &self.dir.join("cds"),
                    toolchain,
                    kind.main_class(),
                    std::slice::from_ref(&jar),
                    &[],
                )
            })
            .flatten();
        let mut classpath = vec![jar];
        classpath.extend(launch.classpath.iter().cloned());
        let mut command = Command::new(&toolchain.java);
        if let Some(share) = &share {
            command.args(&share.flags);
        }
        let mut child = command
            .args(launch.jvm_args)
            .arg("-cp")
            .arg(Toolchain::classpath(&classpath))
            .arg(kind.main_class())
            .args(launch.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Nothing of the worker's own may reach the terminal.
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("{}: {e}", toolchain.java.display()))?;
        ui.verbose(format!(
            "started a {} worker (pid {})",
            kind.name(),
            child.id()
        ));
        Ok(Running {
            stdin: child.stdin.take(),
            stdout: child.stdout.take().map(BufReader::new),
            child,
            key: launch.key.clone(),
            compilations: 0,
            slowest: Duration::ZERO,
            share,
        })
    }

    /// A worker's program as a jar — a class directory could not be
    /// mapped from a class-data-sharing archive — compiled with
    /// `toolchain`'s `javac` once per jrs version and JDK.
    fn program(&self, kind: Kind, toolchain: &Toolchain) -> Result<PathBuf, String> {
        let _building = self.building.lock().unwrap();
        let home = self.dir.join(kind.program_dir(toolchain.version));
        let jar = home.join("worker.jar");
        if jar.is_file() {
            return Ok(jar);
        }
        let work = home.join(format!(".build-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&work);
        let classes = work.join("classes");
        std::fs::create_dir_all(&classes).map_err(|e| format!("{}: {e}", classes.display()))?;
        let source = work.join(format!("{}.java", kind.main_class()));
        std::fs::write(&source, kind.source()).map_err(|e| format!("{}: {e}", source.display()))?;
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
        for slot in [&self.running, &self.kotlinc] {
            if let Ok(mut pool) = slot.lock() {
                for running in pool.drain(..) {
                    running.stop();
                }
            }
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
            .last_mut()
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
