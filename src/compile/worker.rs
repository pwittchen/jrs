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
//! The `javac` worker's JVM maps an archive of its own: from JDK 25 an AOT
//! cache, which takes its first compile from twice a forked `javac`'s time
//! to about a forked one's (`specs/todo/BEATING_THE_DAEMON_2.md` §3). It is
//! recorded by the first worker of the first command that finds none, and
//! by no other worker in its pool (`specs/todo/BEATING_THE_DAEMON_3.md`
//! §2); a single-module build, which starts no worker until an API change
//! needs one, trains it beside the build instead ([`Worker::train_beside`],
//! `specs/todo/BEATING_THE_DAEMON_4.md` §2). In a command that is not `--watch`, that worker records its first
//! compile alone: it is then stopped, its JVM dumps and the cache is
//! assembled on threads the command joins at its end, while a new worker
//! takes its place for the members still to compile. A workspace command
//! starts its first worker as the command starts, so that its JVM is up by
//! the first member's compile, and primes it with a one-class compile of its
//! own, so that the compiler's classes are initialised and its hot code
//! compiled before the first member arrives (§3). Once the last member has
//! compiled, `jrs build` and `jrs run` stop the workers at once, rather than
//! one after another as the command ends. A single-module build whose API
//! change takes more than one round of `javac` sends the later rounds to a
//! worker of its own, once that cache is there to start it warm, and starts
//! that worker beside the first round when the change is to a class others
//! name (§4).
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

use super::share::{self, Share};
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
    /// Whether these workers serve one command rather than a `--watch`
    /// session: a step a forked compiler does faster than a cold worker is
    /// then forked, since no later rebuild repays the worker's warming.
    one_command: bool,
    /// Whether a worker may record its JVM's archive: not one whose exit,
    /// and the archive's assembly, would hold up an edit's build.
    records: bool,
    /// Held while a worker's program is compiled: workers start outside
    /// the pools' locks, and two must not build it in one directory.
    building: Mutex<()>,
}

/// The one-class compile a prestarted worker is primed with.
const PRIME: &str =
    "final class Prime {\n    static String name() {\n        return \"prime\";\n    }\n}\n";

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
    /// The compiles it has answered for the command, primes not counted.
    compilations: u32,
    /// The id of the last request sent.
    requests: u32,
    slowest: Duration,
    /// Its class-data-sharing archive, put in place once it exits.
    share: Option<Share>,
    /// A prime sent and not answered yet: its request id and the scratch
    /// directory it compiles in.
    primed: Option<(u32, PathBuf)>,
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
            one_command: false,
            records: true,
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
        worker.one_command = true;
        worker
    }

    /// A `javac` worker for the later rounds of one unit's compile, which
    /// maps its archive but never records it: its exit is on the build's
    /// critical path.
    #[must_use]
    pub fn for_rounds(dir: PathBuf) -> Worker {
        let mut worker = Worker::javac_only(dir);
        worker.records = false;
        worker
    }

    /// Whether a `javac` worker on `toolchain` would start warm: from an
    /// AOT cache or archive, or as an idle JVM already there.
    ///
    /// # Panics
    ///
    /// If a thread panicked while holding the lock on the worker.
    #[must_use]
    pub fn starts_warm(&self, toolchain: &Toolchain) -> bool {
        let key = toolchain.javac.display().to_string();
        if self.running.lock().unwrap().iter().any(|r| r.key == key) {
            return true;
        }
        let jar = self.program_jar(Kind::Javac, toolchain);
        jar.is_file()
            && Share::worker_ready(
                &self.dir.join("cds"),
                toolchain,
                Kind::Javac.main_class(),
                &jar,
            )
    }

    /// Whether a step a forked `javac` could do should be forked instead:
    /// these workers serve one command, and the next one would start cold.
    #[must_use]
    pub fn starts_cold(&self, toolchain: &Toolchain) -> bool {
        self.one_command && !self.starts_warm(toolchain)
    }

    /// Start a `javac` worker on `toolchain` now, idle until the first
    /// compile takes it, unless one is idle already: its JVM starts while
    /// the command does the rest of its work.
    ///
    /// # Panics
    ///
    /// If a thread panicked while holding the lock on the worker.
    pub fn prestart(&self, toolchain: &Toolchain, ui: &Ui) {
        if !self.running.lock().unwrap().is_empty() {
            return;
        }
        let launch = Launch {
            toolchain,
            classpath: &[],
            jvm_args: &[],
            args: &[],
            key: toolchain.javac.display().to_string(),
        };
        match self.start(Kind::Javac, &launch, ui) {
            Ok(mut started) => {
                self.prime(&mut started, ui);
                self.running.lock().unwrap().push(started);
            }
            Err(why) => ui.verbose(format!("javac worker: not started: {why}")),
        }
    }

    /// Send a worker a one-class compile it answers while nothing waits on
    /// it (`specs/todo/BEATING_THE_DAEMON_3.md` §3), read before its first
    /// real request. A prime that cannot be written is not sent.
    fn prime(&self, worker: &mut Running, ui: &Ui) {
        let scratch = self.dir.join(format!(
            ".prime-{}-{}",
            std::process::id(),
            worker.child.id()
        ));
        let source = scratch.join("Prime.java");
        let argfile = scratch.join("prime.args");
        let written = std::fs::create_dir_all(&scratch).is_ok()
            && std::fs::write(&source, PRIME).is_ok()
            && std::fs::write(
                &argfile,
                super::render_argfile(
                    &[
                        "--release".to_string(),
                        share::TRAINING_RELEASE.to_string(),
                        "-d".to_string(),
                        scratch.join("classes").display().to_string(),
                    ],
                    &[source],
                ),
            )
            .is_ok();
        let sent = written && worker.send(&argfile).is_ok();
        if sent {
            ui.verbose(format!("javac worker (pid {}): primed", worker.child.id()));
            worker.primed = Some((worker.requests, scratch));
        } else {
            let _ = std::fs::remove_dir_all(&scratch);
        }
    }

    /// Compile the training workload (`share.rs`) in a `javac` worker of its
    /// own, so that its JVM records its archive when there is none, and
    /// stop it: `jrs fetch` and `jrs jdk install` make the worker's cache
    /// this way, at a moment the user already waits. The cache is in place
    /// once this returns.
    pub fn train(&self, toolchain: &Toolchain, ui: &Ui) {
        self.record(toolchain, Some(ui));
        share::join_background();
    }

    /// Train the `javac` worker's cache under `dir` as [`Worker::train`]
    /// does, on a thread of its own that the command joins at its end, when
    /// there is none and, unless `always`, no command before this one looked
    /// for it (`specs/todo/BEATING_THE_DAEMON_4.md` §2). A single-module
    /// build starts no worker that would record it, and without it an API
    /// change's later rounds fork a cold `javac`. Whether a training started.
    #[must_use]
    pub fn train_beside(dir: PathBuf, toolchain: &Toolchain, always: bool) -> bool {
        let worker = Worker::javac_only(dir);
        let jar = worker.program_jar(Kind::Javac, toolchain);
        if !share::worker_training_due(
            &worker.dir.join("cds"),
            toolchain,
            Kind::Javac.main_class(),
            &jar,
            always,
        ) {
            return false;
        }
        let toolchain = toolchain.clone();
        share::background(move || worker.record(&toolchain, None));
        true
    }

    /// The training both [`Worker::train`] and [`Worker::train_beside`]
    /// run, saying what it does on `ui` when there is one.
    fn record(&self, toolchain: &Toolchain, ui: Option<&Ui>) {
        let launch = Launch {
            toolchain,
            classpath: &[],
            jvm_args: &[],
            args: &[],
            key: toolchain.javac.display().to_string(),
        };
        let mut worker = match self.spawn(Kind::Javac, &launch) {
            Ok(worker) => worker,
            Err(why) => {
                if let Some(ui) = ui {
                    ui.verbose(format!("javac worker: not started: {why}"));
                }
                return;
            }
        };
        if let Some(ui) = ui {
            ui.verbose(format!(
                "started a javac worker (pid {})",
                worker.child.id()
            ));
        }
        if !worker.share.as_ref().is_some_and(Share::dumps) {
            // A cache there already, or another worker recording it.
            worker.stop();
            return;
        }
        let scratch = self.dir.join(format!(".train-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&scratch);
        let argfile = scratch.join("train.args");
        let written = share::write_training(&scratch.join("src")).is_some_and(|sources| {
            let args = [
                "--release".to_string(),
                share::TRAINING_RELEASE.to_string(),
                "-encoding".to_string(),
                "UTF-8".to_string(),
                "-d".to_string(),
                scratch.join("classes").display().to_string(),
            ];
            std::fs::write(&argfile, super::render_argfile(&args, &sources)).is_ok()
        });
        if written && let Err(why) = worker.request(&argfile) {
            if let Some(ui) = ui {
                ui.verbose(format!("javac worker: training: {why}"));
            }
            worker.compilations = 0;
        }
        worker.stop();
        let _ = std::fs::remove_dir_all(&scratch);
    }

    /// Stop every idle worker now, each on a thread of its own that the
    /// command joins at its end: for a command with nothing left to
    /// compile, whose workers' exits, dumps and assemblies then overlap
    /// whatever it still does. A later compile starts a new worker.
    ///
    /// # Panics
    ///
    /// If a thread panicked while holding the lock on the worker.
    pub fn retire(&self) {
        for slot in [&self.running, &self.kotlinc] {
            let idle: Vec<Running> = slot.lock().unwrap().drain(..).collect();
            for running in idle {
                share::background(move || running.stop());
            }
        }
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
                // A command's recording worker has compiled enough to train
                // its cache: it stops now, beside the rest of the command,
                // and a new one takes its place.
                if self.one_command && worker.share.as_ref().is_some_and(Share::dumps) {
                    share::background(move || worker.stop());
                    match self.start(kind, launch, ui) {
                        Ok(started) => slot.lock().unwrap().push(started),
                        Err(why) => ui.verbose(format!("{name} worker: not started: {why}")),
                    }
                } else {
                    slot.lock().unwrap().push(worker);
                }
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
        let running = self.spawn(kind, launch)?;
        ui.verbose(format!(
            "started a {} worker (pid {})",
            kind.name(),
            running.child.id()
        ));
        Ok(running)
    }

    /// [`Worker::start`], saying nothing.
    fn spawn(&self, kind: Kind, launch: &Launch<'_>) -> Result<Running, String> {
        let toolchain = launch.toolchain;
        let jar = self.program(kind, toolchain)?;
        // The `kotlinc` worker loads the compiler's classes, which the
        // compiler's own archive already maps for forked runs.
        let share = (kind == Kind::Javac)
            .then(|| {
                Share::for_worker(
                    &self.dir.join("cds"),
                    toolchain,
                    kind.main_class(),
                    &jar,
                    self.records,
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
        Ok(Running {
            stdin: child.stdin.take(),
            stdout: child.stdout.take().map(BufReader::new),
            child,
            key: launch.key.clone(),
            compilations: 0,
            requests: 0,
            slowest: Duration::ZERO,
            share,
            primed: None,
        })
    }

    /// A worker's program as a jar — a class directory could not be
    /// mapped from a class-data-sharing archive — compiled with
    /// `toolchain`'s `javac` once per jrs version and JDK.
    fn program(&self, kind: Kind, toolchain: &Toolchain) -> Result<PathBuf, String> {
        let _building = self.building.lock().unwrap();
        let jar = self.program_jar(kind, toolchain);
        if jar.is_file() {
            return Ok(jar);
        }
        let home = self.dir.join(kind.program_dir(toolchain.version));
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

impl Worker {
    /// Where [`Worker::program`] puts a worker's program.
    fn program_jar(&self, kind: Kind, toolchain: &Toolchain) -> PathBuf {
        self.dir
            .join(kind.program_dir(toolchain.version))
            .join("worker.jar")
    }
}

impl Running {
    /// Send one request and read its answer, within the worker's patience,
    /// after the prime's when one is outstanding.
    fn request(&mut self, argfile: &Path) -> Result<CapturedOutput, String> {
        if let Some((id, scratch)) = self.primed.take() {
            let primed = self.receive(id);
            let _ = std::fs::remove_dir_all(scratch);
            primed?;
        }
        self.compilations += 1;
        self.send(argfile)?;
        self.receive(self.requests)
    }

    /// Send a request without waiting for its answer.
    fn send(&mut self, argfile: &Path) -> Result<(), String> {
        self.requests += 1;
        let id = self.requests;
        let stdin = self.stdin.as_mut().ok_or("its stdin is gone")?;
        writeln!(stdin, "{id} {}", argfile.display())
            .and_then(|()| stdin.flush())
            .map_err(|e| format!("the request could not be sent ({e})"))
    }

    /// Read the answer to request `id`, within the worker's patience.
    fn receive(&mut self, id: u32) -> Result<CapturedOutput, String> {
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
    /// place once it has exited on its own. A worker that writes no archive
    /// is killed instead, since it has nothing left to do and its exit
    /// would only be waited for; so is one that compiled nothing — started
    /// early by a command that found every member up to date — whose
    /// archive would hold no compiler.
    fn stop(mut self) {
        if let Some((_, scratch)) = self.primed.take() {
            let _ = std::fs::remove_dir_all(scratch);
        }
        if self.compilations == 0 || !self.share.as_ref().is_some_and(Share::dumps) {
            let _ = self.child.kill();
            let _ = self.child.wait();
            if let Some(share) = self.share.take() {
                share.finish(false);
            }
            return;
        }
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
    /// Stop every worker, all at once: each one that writes an archive is
    /// waited for while it exits and dumps, and they would otherwise wait
    /// one after another.
    fn drop(&mut self) {
        let mut idle = Vec::new();
        for slot in [&self.running, &self.kotlinc] {
            if let Ok(mut pool) = slot.lock() {
                idle.extend(pool.drain(..));
            }
        }
        if idle.len() == 1 {
            idle.into_iter().for_each(Running::stop);
            return;
        }
        std::thread::scope(|scope| {
            for running in idle {
                // A thread that cannot be started drops its worker, whose
                // closed stdin ends it all the same.
                let _ = std::thread::Builder::new().spawn_scoped(scope, move || running.stop());
            }
        });
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

    #[test]
    fn a_prestarted_worker_serves_the_first_compile() {
        let Ok(toolchain) = Toolchain::discover() else {
            eprintln!("SKIPPED: no usable JDK");
            return;
        };
        let tree = Tree::new("prestart");
        // A worker that records its cache is replaced after one compile.
        let worker = Worker::for_rounds(tree.root.join("cache/worker"));
        let ui = ui();
        worker.prestart(&toolchain, &ui);
        let pid = worker.pid().unwrap();
        worker.prestart(&toolchain, &ui);
        assert_eq!(worker.pid(), Some(pid), "one is enough");
        assert!(worker.starts_warm(&toolchain), "an idle JVM is warm enough");
        let argfile = tree.argfile("First", "class First {}\n");
        assert!(worker.compile(&toolchain, &argfile, &ui).unwrap().ok());
        assert_eq!(worker.pid(), Some(pid));
        assert!(
            !std::fs::read_dir(tree.root.join("cache/worker"))
                .unwrap()
                .flatten()
                .any(|e| e.file_name().to_string_lossy().starts_with(".prime-")),
            "the prime was answered, and its scratch directory is gone"
        );

        let unused = Worker::javac_only(tree.root.join("cache/worker"));
        unused.prestart(&toolchain, &ui);
        let started = Instant::now();
        drop(unused);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "a worker that compiled nothing is killed, not waited for"
        );
    }

    #[test]
    fn the_first_command_records_the_workers_aot_cache_and_the_next_maps_it() {
        let Ok(toolchain) = Toolchain::discover() else {
            eprintln!("SKIPPED: no usable JDK");
            return;
        };
        if toolchain.version < super::super::share::JAVAC_AOT_SINCE {
            eprintln!("SKIPPED: the worker's AOT cache needs JDK 25");
            return;
        }
        let tree = Tree::new("worker-aot");
        let dir = tree.root.join("cache/worker");
        let ui = ui();
        let first = tree.argfile("Cached", "class Cached {}\n");
        let second = tree.argfile("Again", "class Again {}\n");
        let worker = Worker::javac_only(dir.clone());
        worker.prestart(&toolchain, &ui);
        let recorder = worker.pid().unwrap();
        assert!(worker.compile(&toolchain, &first, &ui).unwrap().ok());
        assert_ne!(
            worker.pid(),
            Some(recorder),
            "the recording worker stops after its first compile"
        );
        assert!(worker.compile(&toolchain, &second, &ui).unwrap().ok());
        drop(worker);
        share::join_background();
        let worker = Worker::for_rounds(dir.clone());
        let listing: Vec<String> = std::fs::read_dir(dir.join("cds"))
            .map(|d| {
                d.flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        assert!(
            worker.starts_warm(&toolchain),
            "the cache is assembled once the command's worker exited: {listing:?}"
        );
        assert!(worker.compile(&toolchain, &first, &ui).unwrap().ok());
    }

    #[test]
    fn retired_workers_stop_beside_the_command() {
        let Ok(toolchain) = Toolchain::discover() else {
            eprintln!("SKIPPED: no usable JDK");
            return;
        };
        let tree = Tree::new("retire");
        let worker = Worker::for_rounds(tree.root.join("cache/worker"));
        let ui = ui();
        let argfile = tree.argfile("Retired", "class Retired {}\n");
        assert!(worker.compile(&toolchain, &argfile, &ui).unwrap().ok());
        assert!(worker.pid().is_some());
        worker.retire();
        assert!(worker.pid().is_none(), "no idle worker is left");
        assert!(
            worker.compile(&toolchain, &argfile, &ui).unwrap().ok(),
            "a later compile starts a new one"
        );
        share::join_background();
    }
}
