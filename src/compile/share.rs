//! Class-data sharing for the compiler JVMs.
//!
//! `javac`, kotlinc, scalac and groovyc load the same few thousand classes on
//! every run, and a JVM that maps them from a CDS archive starts faster than
//! one that parses and verifies them again. Each compiler gets an archive of
//! its own in the shared cache, `<cache>/cds/`, keyed by the JDK and by the
//! compiler's classpath, so a new JDK or a new compiler is a new archive. It
//! never goes in `target/`, which stays disposable.
//!
//! The archive is written in two steps, as the cache writes a jar: the first
//! run dumps the classes it loaded into a temporary file
//! (`-XX:ArchiveClassesAtExit`, JDK 13+), and jrs renames it into place once
//! that run succeeded, so no run maps a half-written archive. JDK 19's
//! `-XX:+AutoCreateSharedArchive` would do both in one flag, but rewrites the
//! file it reads, in place, and the JDK 17 jrs supports does not have it.
//!
//! None of this may fail a build. An archive that cannot be written or read
//! is skipped: the JVM's CDS logging is turned off, so a stale or truncated
//! archive costs the start-up it was meant to save, and nothing else. A JDK
//! without its own base archive, or a user who passes CDS flags of their own,
//! gets no archive from jrs at all.
//!
//! From JDK 25, `javac` gets JEP 483's AOT cache instead
//! (`specs/todo/BEATING_THE_DAEMON.md` §6): its classes kept loaded and
//! linked, and the method profiles of the run that trained it (JEP 515), in
//! `<cache>/cds/javac-jdk<n>-<key>.aot`, one per JDK. It is made in the two
//! steps the test JVM's is, below; what it holds is no project's, and it
//! changes how fast `javac` starts, never what it writes. The `javac`
//! worker's JVM gets one of its own the same way, keyed by its program too.
//!
//! Neither is made inside an edit (`specs/todo/BEATING_THE_DAEMON_2.md` §2).
//! `javac`'s archive is written by a whole-unit compile alone: a run over a
//! few files maps it, with its own quick JVM flags, and never records one,
//! since that would cost the edit several times what it saves. And neither
//! is written by the first command that finds none: that one leaves a
//! marker beside where it goes, `<archive>.seen`, and the next command
//! that finds the marker writes it. A one-off build, and a CI run on an
//! empty cache, pay nothing for an archive they would never read.
//!
//! The test JVM gets an archive too, when `test.share-classes` asks for one
//! (SPEC §10.2). CDS refuses to dump a classpath holding a non-empty class
//! directory, and lets a run append to the classpath it was dumped with, so
//! the test JVM's `-cp` holds the jars alone and the class directories go to
//! the launcher's own class loader. Its archive is one per project and
//! suite, `test-<project>-<deps>.jsa`: writing a new one deletes the older
//! ones, since a project whose dependencies change daily must not fill the
//! cache with archives a hundred megabytes each.
//!
//! `test.share-classes = "aot"` asks for JEP 483's AOT cache instead, on a
//! JDK 24 or later, and `true` does from JDK 25 (`"archive"` keeps the
//! dynamic archive everywhere). An AOT cache that cannot be assembled for a
//! classpath and its flags — Mockito's inline mock maker appends to the boot
//! class path while the tests run — leaves `<cache>.unassembled` beside
//! where it would go, and from then on that classpath gets the dynamic
//! archive, rather than a recording run every time that comes to nothing.
//! The AOT cache follows the same classpath rule, but keeps classes linked,
//! and from JDK 25 method profiles too. It is made in two steps, both jrs's: the
//! test run records which classes it used (`-XX:AOTMode=record`), and once it
//! is over jrs assembles the cache from that record in a JVM of its own
//! (`-XX:AOTMode=create`) whose output it keeps to itself. JDK 25's one-step
//! `-XX:AOTCacheOutput` would print its progress between the tests' own
//! lines, and so would the recording run, unless the VM's own output is
//! turned off for it (`-XX:-DisplayVMOutput`).

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::UNIX_EPOCH;

use crate::resolve::repo::sha256_hex;
use crate::toolchain::Toolchain;

/// The flag that silences every CDS message: a mismatched or truncated
/// archive is not worth a line between the compiler's diagnostics.
const QUIET: &str = "-Xlog:cds*=off";

/// JVM flags that mean the user has taken class-data sharing into their own
/// hands.
const USER_FLAGS: &[&str] = &[
    "-Xshare",
    "-XX:SharedArchiveFile",
    "-XX:ArchiveClassesAtExit",
    "-XX:+AutoCreateSharedArchive",
    "-XX:AOTCache",
    "-XX:AOTMode",
    "-XX:AOTConfiguration",
];

/// The AOT cache's own messages, off as [`QUIET`] turns off CDS's.
const AOT_QUIET: &str = "-Xlog:aot*=off";

/// The first JDK with the AOT cache (JEP 483).
pub const AOT_SINCE: u32 = 24;

/// The first JDK whose `javac` gets an AOT cache rather than a dynamic
/// archive: from JDK 25 the cache also keeps the method profiles of the run
/// that trained it (JEP 515), so `javac` starts with its hot code known.
pub const JAVAC_AOT_SINCE: u32 = 25;

/// What the `javac` launcher runs, which the run that assembles its AOT
/// cache has to name as the training run did: a cache assembled for
/// another main module maps no module classes.
const JAVAC_MAIN: &[&str] = &[
    "--add-modules",
    "ALL-DEFAULT",
    "-m",
    "jdk.compiler/com.sun.tools.javac.Main",
];

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// One JVM run's use of its archive.
#[derive(Debug)]
pub struct Share {
    /// The JVM flags: read the archive, or dump one.
    pub flags: Vec<String>,
    /// When dumping: the temporary file, and where it goes once the run
    /// succeeded.
    dump: Option<(PathBuf, PathBuf)>,
    /// The file-name prefix of the archives this one replaces, deleted once
    /// it is in place: a project's older test archives.
    replaces: Option<String>,
    /// For an AOT cache, how to assemble it from what the run recorded
    /// into the temporary file.
    assemble: Option<Assemble>,
    /// For a test JVM's AOT cache, the marker that says it could not be
    /// assembled, written when a run that succeeded could not make it.
    unassembled: Option<PathBuf>,
}

/// The `-XX:AOTMode=create` run that turns a recorded configuration into
/// an AOT cache.
#[derive(Debug)]
struct Assemble {
    java: PathBuf,
    /// The run's `-cp`; `None` for a run without one, which names its main
    /// module in `main` instead.
    classpath: Option<Vec<PathBuf>>,
    /// The run's own flags that change what is cached.
    flags: Vec<String>,
    /// What the run ran, after its classpath.
    main: Vec<String>,
}

impl Share {
    /// The archive `tool` reads under `dir`, or `None` when it gets none. The
    /// tool is the program the JVM runs — `javac`'s module, a compiler's
    /// main class — and `classpath` what it is loaded from; `user_args` are
    /// the JVM flags the user gave it.
    pub(crate) fn new(
        dir: &Path,
        toolchain: &Toolchain,
        tool: &str,
        classpath: &[PathBuf],
        user_args: &[String],
    ) -> Option<Share> {
        let home = usable(dir, toolchain, user_args, true)?;
        let archive = dynamic_archive(dir, &home, toolchain, tool, classpath);
        Share::at(dir, archive, true, None)
    }

    /// `javac`'s archive under `dir`, or `None` when it gets none: from
    /// JDK 25 an AOT cache (`specs/todo/BEATING_THE_DAEMON.md` §6), keyed
    /// by the JDK; before, the dynamic archive [`Share::new`] gives any
    /// compiler. Like the test JVM's, the AOT cache is made in two steps:
    /// the first run records, and jrs assembles the cache from that record
    /// once it succeeded. A `quick` run — a few files, started with the
    /// quick JVM flags — maps the archive when there is one and never
    /// writes it; any other writes it once a command before this one found
    /// none ([`seen_before`]).
    pub(crate) fn for_javac(dir: &Path, toolchain: &Toolchain, quick: bool) -> Option<Share> {
        let (archive, aot) = javac_archive(dir, toolchain)?;
        if archive.is_file() {
            return Some(Share::reading(&archive, aot));
        }
        if !seen_before(&archive) || quick {
            return None;
        }
        if !aot {
            return Share::at(dir, archive, true, None);
        }
        let temp = temporary(dir, &archive)?;
        Some(Share {
            flags: Share::recording(&temp),
            dump: Some((temp, archive)),
            replaces: None,
            unassembled: None,
            assemble: Some(Assemble {
                java: toolchain.java.clone(),
                classpath: None,
                flags: Vec::new(),
                main: JAVAC_MAIN.iter().map(|f| (*f).to_string()).collect(),
            }),
        })
    }

    /// Whether a forked `javac` would map an archive under `dir` now.
    pub(crate) fn javac_ready(dir: &Path, toolchain: &Toolchain) -> bool {
        javac_archive(dir, toolchain).is_some_and(|(archive, _)| archive.is_file())
    }

    /// The archive under `dir` of a compiler worker's JVM, which runs
    /// `main_class` from the jar `program`, or `None` when it gets none:
    /// from JDK 25 an AOT cache keyed by the JDK and the program, before a
    /// dynamic archive. It is written once a command before this one found
    /// none ([`seen_before`]), and only when `record` allows; the worker
    /// records it over its whole life, and the cache is assembled once it
    /// has exited.
    pub(crate) fn for_worker(
        dir: &Path,
        toolchain: &Toolchain,
        main_class: &str,
        program: &Path,
        record: bool,
    ) -> Option<Share> {
        let (archive, aot) = worker_archive(dir, toolchain, main_class, program)?;
        if archive.is_file() {
            return Some(Share::reading(&archive, aot));
        }
        if !record || !seen_before(&archive) {
            return None;
        }
        if !aot {
            return Share::at(dir, archive, true, None);
        }
        let temp = temporary(dir, &archive)?;
        Some(Share {
            flags: Share::recording(&temp),
            dump: Some((temp, archive)),
            replaces: None,
            unassembled: None,
            assemble: Some(Assemble {
                java: toolchain.java.clone(),
                classpath: Some(vec![program.to_path_buf()]),
                flags: Vec::new(),
                main: Vec::new(),
            }),
        })
    }

    /// Whether a worker's JVM would map an archive under `dir` now.
    pub(crate) fn worker_ready(
        dir: &Path,
        toolchain: &Toolchain,
        main_class: &str,
        program: &Path,
    ) -> bool {
        worker_archive(dir, toolchain, main_class, program)
            .is_some_and(|(archive, _)| archive.is_file())
    }

    /// A run that maps `archive`, an AOT cache when `aot` holds and a
    /// dynamic archive otherwise.
    fn reading(archive: &Path, aot: bool) -> Share {
        let flags = if aot {
            vec![
                format!("-XX:AOTCache={}", archive.display()),
                AOT_QUIET.to_string(),
                QUIET.to_string(),
            ]
        } else {
            vec![
                format!("-XX:SharedArchiveFile={}", archive.display()),
                QUIET.to_string(),
            ]
        };
        Share {
            flags,
            dump: None,
            replaces: None,
            assemble: None,
            unassembled: None,
        }
    }

    /// The flags of a run that records which classes it used into `temp`,
    /// for an AOT cache, and says nothing of it.
    fn recording(temp: &Path) -> Vec<String> {
        vec![
            "-XX:AOTMode=record".to_string(),
            format!("-XX:AOTConfiguration={}", temp.display()),
            // The recording run says that it recorded, on stdout, between
            // the tool's own lines, whatever its logging is set to.
            "-XX:+UnlockDiagnosticVMOptions".to_string(),
            "-XX:-DisplayVMOutput".to_string(),
            AOT_QUIET.to_string(),
            QUIET.to_string(),
        ]
    }

    /// The test JVM's archive under `dir`, or `None` when it gets none.
    /// `project` names the project and suite, `classpath` is the JVM's own
    /// `-cp` — jars only — and `jvm_args` its other flags, of which
    /// `-javaagent` and `-XX:` ones change what is archived. Without `dump`
    /// it only reads an archive that is already there: forks read one,
    /// and none of them writes it. With `aot`, on a JDK that has it, the
    /// archive is an AOT cache — unless one could not be assembled for
    /// these jars and flags before, as after Mockito appends to the boot
    /// class path (`specs/TEST_JVM_BENCHMARK.md` §7.1, finding 2): then it
    /// is the dynamic archive, so that no run records again in vain.
    #[must_use]
    pub fn for_tests(
        dir: &Path,
        toolchain: &Toolchain,
        project: &str,
        classpath: &[PathBuf],
        jvm_args: &[String],
        dump: bool,
        aot: bool,
    ) -> Option<Share> {
        let archived: Vec<String> = jvm_args
            .iter()
            .filter(|a| a.starts_with("-javaagent") || a.starts_with("-XX:"))
            .cloned()
            .collect();
        let prefix = format!("test-{}-", &sha256_hex(project.as_bytes())[..16]);
        let archive_of = |home: &Path, kind: &str| {
            let tool = format!("junit-{kind}\u{1}{}", archived.join("\u{1}"));
            dir.join(format!(
                "{prefix}{}.{kind}",
                key(home, toolchain, &tool, classpath)
            ))
        };
        if aot && toolchain.version >= AOT_SINCE {
            let home = usable(dir, toolchain, jvm_args, false)?;
            let archive = archive_of(&home, "aot");
            if archive.is_file() {
                return Some(Share::reading(&archive, true));
            }
            let unassembled = unassembled(&archive);
            if !unassembled.is_file() {
                if !dump {
                    return None;
                }
                let temp = temporary(dir, &archive)?;
                return Some(Share {
                    flags: Share::recording(&temp),
                    dump: Some((temp, archive)),
                    replaces: Some(prefix),
                    assemble: Some(Assemble {
                        java: toolchain.java.clone(),
                        classpath: Some(classpath.to_vec()),
                        flags: archived,
                        main: Vec::new(),
                    }),
                    unassembled: Some(unassembled),
                });
            }
        }
        let home = usable(dir, toolchain, jvm_args, true)?;
        Share::at(dir, archive_of(&home, "jsa"), dump, Some(prefix))
    }

    /// Read `archive` if it is there, or else dump into a temporary file
    /// beside it when `dump` allows.
    fn at(dir: &Path, archive: PathBuf, dump: bool, replaces: Option<String>) -> Option<Share> {
        if archive.is_file() {
            return Some(Share::reading(&archive, false));
        }
        if !dump {
            return None;
        }
        let temp = temporary(dir, &archive)?;
        Some(Share {
            flags: vec![
                format!("-XX:ArchiveClassesAtExit={}", temp.display()),
                QUIET.to_string(),
            ],
            dump: Some((temp, archive)),
            replaces,
            assemble: None,
            unassembled: None,
        })
    }

    /// Whether this run writes the archive rather than reading it.
    #[must_use]
    pub fn dumps(&self) -> bool {
        self.dump.is_some()
    }

    /// The archive this run reads or writes.
    #[must_use]
    pub fn archive(&self) -> Option<&Path> {
        match &self.dump {
            Some((_, archive)) => Some(archive),
            None => self
                .flags
                .first()
                .and_then(|f| {
                    f.strip_prefix("-XX:SharedArchiveFile=")
                        .or_else(|| f.strip_prefix("-XX:AOTCache="))
                })
                .map(Path::new),
        }
    }

    /// Whether the archive is an AOT cache rather than a dynamic archive.
    #[must_use]
    pub fn is_aot(&self) -> bool {
        self.flags
            .iter()
            .any(|f| f.starts_with("-XX:AOTCache=") || f == "-XX:AOTMode=record")
    }

    /// After the run: put a dumped archive in place if the run succeeded,
    /// and drop it otherwise. A failure here only costs the next run its
    /// archive.
    pub fn finish(self, ok: bool) {
        let Some((temp, archive)) = self.dump else {
            return;
        };
        let written = std::fs::metadata(&temp).is_ok_and(|m| m.len() > 0);
        let placed = match &self.assemble {
            None => ok && written && std::fs::rename(&temp, &archive).is_ok(),
            Some(assemble) => ok && written && assemble.run(&temp, &archive),
        };
        if !placed {
            let _ = std::fs::remove_file(&temp);
            if let Some(marker) = self.unassembled.filter(|_| ok && written) {
                let _ = std::fs::write(marker, "");
            }
            return;
        }
        if self.assemble.is_some() {
            let _ = std::fs::remove_file(&temp);
        }
        let _ = std::fs::remove_file(marker(&archive));
        let (Some(prefix), Some(dir)) = (self.replaces, archive.parent()) else {
            return;
        };
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            // A marker that an AOT cache could not be assembled goes once
            // one has been; the archive that stands in for one keeps it.
            let stale = |e: &std::ffi::OsStr| {
                e == "jsa" || e == "aot" || (e == "unassembled" && self.assemble.is_some())
            };
            if path != archive && name.starts_with(&prefix) && path.extension().is_some_and(stale) {
                let _ = std::fs::remove_file(path);
            }
        }
    }
}

impl Assemble {
    /// Assemble the AOT cache `archive` from the configuration recorded in
    /// `recorded`, by way of a temporary file. Its output is the JVM's
    /// account of its work, which no one needs to read: it is dropped.
    fn run(&self, recorded: &Path, archive: &Path) -> bool {
        let Some(dir) = archive.parent() else {
            return false;
        };
        let Some(temp) = temporary(dir, archive) else {
            return false;
        };
        let mut command = std::process::Command::new(&self.java);
        command
            .arg("-XX:AOTMode=create")
            .arg(format!("-XX:AOTConfiguration={}", recorded.display()))
            .arg(format!("-XX:AOTCache={}", temp.display()))
            .args([AOT_QUIET, QUIET])
            .args(&self.flags);
        if let Some(classpath) = &self.classpath {
            command.arg("-cp").arg(Toolchain::classpath(classpath));
        }
        let assembled = command
            .args(&self.main)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        let written = std::fs::metadata(&temp).is_ok_and(|m| m.len() > 0);
        if assembled && written && std::fs::rename(&temp, archive).is_ok() {
            return true;
        }
        let _ = std::fs::remove_file(&temp);
        false
    }
}

/// `javac`'s archive under `dir`, and whether it is an AOT cache, or `None`
/// when it gets none.
fn javac_archive(dir: &Path, toolchain: &Toolchain) -> Option<(PathBuf, bool)> {
    if toolchain.version < JAVAC_AOT_SINCE {
        let home = usable(dir, toolchain, &[], true)?;
        let archive = dynamic_archive(dir, &home, toolchain, "jdk.compiler", &[]);
        return Some((archive, false));
    }
    let home = usable(dir, toolchain, &[], false)?;
    // The key an earlier jrs gave the cache of a `javac` started with the
    // full JIT, so that cache stays in use.
    let archive = dir.join(format!(
        "javac-jdk{}-{}.aot",
        toolchain.version,
        key(&home, toolchain, "javac-aot\u{1}", &[])
    ));
    Some((archive, true))
}

/// A worker JVM's archive under `dir`, and whether it is an AOT cache, or
/// `None` when it gets none.
fn worker_archive(
    dir: &Path,
    toolchain: &Toolchain,
    main_class: &str,
    program: &Path,
) -> Option<(PathBuf, bool)> {
    let program = [program.to_path_buf()];
    if toolchain.version < JAVAC_AOT_SINCE {
        let home = usable(dir, toolchain, &[], true)?;
        let archive = dynamic_archive(dir, &home, toolchain, main_class, &program);
        return Some((archive, false));
    }
    let home = usable(dir, toolchain, &[], false)?;
    let archive = dir.join(format!(
        "{}-jdk{}-{}.aot",
        file_safe(main_class),
        toolchain.version,
        key(&home, toolchain, &format!("{main_class}-aot"), &program)
    ));
    Some((archive, true))
}

/// The dynamic archive under `dir` of `tool`, loaded from `classpath`.
fn dynamic_archive(
    dir: &Path,
    home: &Path,
    toolchain: &Toolchain,
    tool: &str,
    classpath: &[PathBuf],
) -> PathBuf {
    dir.join(format!(
        "{}-jdk{}-{}.jsa",
        file_safe(tool),
        toolchain.version,
        key(home, toolchain, tool, classpath)
    ))
}

/// Whether a command before this one looked for `archive` and found none,
/// as the marker beside it says; if none did, this command leaves the
/// marker. It holds the process id of the command that left it, so this
/// command's second look — another member's compile, another worker — is
/// still its first.
fn seen_before(archive: &Path) -> bool {
    let marker = marker(archive);
    let me = std::process::id().to_string();
    if let Ok(seen) = std::fs::read_to_string(&marker) {
        return seen.trim() != me;
    }
    let _ = std::fs::write(&marker, me);
    false
}

/// The marker that the AOT cache `archive` could not be assembled.
fn unassembled(archive: &Path) -> PathBuf {
    let mut name = archive.as_os_str().to_owned();
    name.push(".unassembled");
    PathBuf::from(name)
}

/// The marker [`seen_before`] leaves for `archive`.
fn marker(archive: &Path) -> PathBuf {
    let mut name = archive.as_os_str().to_owned();
    name.push(".seen");
    PathBuf::from(name)
}

/// A new temporary file beside `archive`, for a run to write it into, once
/// whatever earlier runs abandoned there is swept away. A JVM that cannot
/// write its dump does not start at all, so jrs makes sure it can before
/// asking.
fn temporary(dir: &Path, archive: &Path) -> Option<PathBuf> {
    let name = archive.file_name()?.to_string_lossy().into_owned();
    sweep(dir, &name);
    let temp = dir.join(format!(
        ".{name}.{}-{}.tmp",
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::File::create(&temp).ok()?;
    Some(temp)
}

/// Delete the dumps of `archive` that a run abandoned: a `--watch`
/// session's `javac` worker dumps only when it exits, and a session ended
/// with ctrl-c never puts its dump in place. A temporary file a minute old
/// is either one of those or a run still going, whose JVM writes its dump
/// afresh at exit; either way it is not worth the disk it takes.
fn sweep(dir: &Path, archive: &str) {
    let prefix = format!(".{archive}.");
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let abandoned = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > ABANDONED);
        let temp = Path::new(&name).extension().is_some_and(|e| e == "tmp");
        if name.starts_with(&prefix) && temp && abandoned {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// How old a temporary dump has to be before [`sweep`] takes it for
/// abandoned.
const ABANDONED: std::time::Duration = std::time::Duration::from_secs(60);

/// The JDK's home, when a JVM started with `user_args` may use an archive
/// under `dir`: the user has not taken class-data sharing into their own
/// hands, the JDK has the base archive a dynamic one is layered on (`base`;
/// an AOT cache stands alone), and the directory can be created.
fn usable(dir: &Path, toolchain: &Toolchain, user_args: &[String], base: bool) -> Option<PathBuf> {
    if user_args
        .iter()
        .any(|a| USER_FLAGS.iter().any(|f| a.starts_with(f)))
    {
        return None;
    }
    let home = jdk_home(toolchain)?;
    if base && !has_base_archive(&home) {
        return None;
    }
    // A missing directory is fatal to a JVM asked to dump into it.
    std::fs::create_dir_all(dir).ok()?;
    Some(home)
}

/// Run a compiler with `share`'s flags, which `launch` puts where its JVM
/// reads them. A run that was dumping an archive and failed runs once more
/// without them: a JVM that cannot write the dump — a full disk, say —
/// fails to start, and that must not fail the build. A real compile error
/// costs one extra run, on the first build after a new JDK or compiler only.
///
/// # Errors
///
/// Whatever `launch` returns.
pub(crate) fn run<T>(
    share: Option<Share>,
    ok: impl Fn(&T) -> bool,
    mut launch: impl FnMut(&[String]) -> crate::error::Result<T>,
) -> crate::error::Result<T> {
    let Some(share) = share else {
        return launch(&[]);
    };
    let output = launch(&share.flags)?;
    let succeeded = ok(&output);
    let dumping = share.dump.is_some();
    share.finish(succeeded);
    if succeeded || !dumping {
        return Ok(output);
    }
    launch(&[])
}

/// The JDK's home: two levels above its `java`.
fn jdk_home(toolchain: &Toolchain) -> Option<PathBuf> {
    let java = std::fs::canonicalize(&toolchain.java).unwrap_or_else(|_| toolchain.java.clone());
    Some(java.parent()?.parent()?.to_path_buf())
}

/// Whether the JDK ships the default CDS archive a dynamic one is layered
/// on. Without it the JVM warns on every run that it cannot dump one.
fn has_base_archive(home: &Path) -> bool {
    ["lib", "bin"]
        .iter()
        .any(|dir| home.join(dir).join("server").join("classes.jsa").is_file())
}

/// What the archive was dumped from: the JDK, by its home and its module
/// image, which a JDK updated in place rewrites, and the tool with every
/// jar it loads from, by path, size and modification time.
fn key(home: &Path, toolchain: &Toolchain, tool: &str, classpath: &[PathBuf]) -> String {
    let mut s = format!("{}\n{}\n{tool}\n", home.display(), toolchain.version);
    for entry in std::iter::once(home.join("lib").join("modules")).chain(classpath.iter().cloned())
    {
        let (size, modified) = std::fs::metadata(&entry)
            .ok()
            .map(|m| {
                let modified = m
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_nanos());
                (m.len(), modified)
            })
            .unwrap_or_default();
        let _ = writeln!(s, "{} {size} {modified}", entry.display());
    }
    sha256_hex(s.as_bytes())[..16].to_string()
}

/// `org.jetbrains.kotlin.cli.jvm.K2JVMCompiler` as a file name: letters,
/// digits, dots and dashes.
fn file_safe(tool: &str) -> String {
    tool.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Tree {
        root: PathBuf,
    }

    impl Tree {
        fn new(name: &str) -> Tree {
            let root =
                std::env::temp_dir().join(format!("jrs-share-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap();
            Tree { root }
        }

        /// A JDK with a `java`, a module image and, unless `base` is false,
        /// the default CDS archive.
        fn jdk(&self, base: bool) -> Toolchain {
            let home = self.root.join("jdk");
            for file in ["bin/java", "bin/javac", "lib/modules"] {
                let path = home.join(file);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(&path, "x").unwrap();
            }
            if base {
                let path = home.join("lib/server/classes.jsa");
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(&path, "x").unwrap();
            }
            Toolchain {
                javac: home.join("bin/javac"),
                java: home.join("bin/java"),
                jar: home.join("bin/jar"),
                version: 21,
                home: Some(home),
            }
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn flag<'a>(share: &'a Share, prefix: &str) -> Option<&'a str> {
        share.flags.iter().find_map(|f| f.strip_prefix(prefix))
    }

    #[test]
    fn the_first_run_dumps_and_later_runs_read_the_archive() {
        let tree = Tree::new("cycle");
        let jdk = tree.jdk(true);
        let dir = tree.root.join("cache/cds");
        let share = Share::new(&dir, &jdk, "jdk.compiler", &[], &[]).unwrap();
        assert!(share.flags.contains(&QUIET.to_string()));
        let temp = PathBuf::from(flag(&share, "-XX:ArchiveClassesAtExit=").unwrap());
        assert_eq!(temp.parent(), Some(dir.as_path()));
        std::fs::write(&temp, "archive").unwrap();
        share.finish(true);
        assert!(!temp.exists(), "the dump was moved into place");

        let share = Share::new(&dir, &jdk, "jdk.compiler", &[], &[]).unwrap();
        let archive = PathBuf::from(flag(&share, "-XX:SharedArchiveFile=").unwrap());
        assert_eq!(std::fs::read_to_string(archive).unwrap(), "archive");
        assert!(flag(&share, "-XX:ArchiveClassesAtExit=").is_none());
    }

    #[test]
    fn a_failed_or_empty_dump_is_thrown_away() {
        let tree = Tree::new("failed");
        let jdk = tree.jdk(true);
        let dir = tree.root.join("cds");
        for (ok, contents) in [(false, "archive"), (true, "")] {
            let share = Share::new(&dir, &jdk, "javac", &[], &[]).unwrap();
            let temp = PathBuf::from(flag(&share, "-XX:ArchiveClassesAtExit=").unwrap());
            std::fs::write(&temp, contents).unwrap();
            share.finish(ok);
            assert!(!temp.exists());
            let next = Share::new(&dir, &jdk, "javac", &[], &[]).unwrap();
            assert!(
                flag(&next, "-XX:ArchiveClassesAtExit=").is_some(),
                "still no archive to read"
            );
        }
    }

    #[test]
    fn each_tool_and_classpath_has_an_archive_of_its_own() {
        let tree = Tree::new("keys");
        let jdk = tree.jdk(true);
        let dir = tree.root.join("cds");
        let jar = tree.root.join("kotlin-compiler.jar");
        std::fs::write(&jar, "one").unwrap();
        let target = |tool: &str| {
            let share = Share::new(&dir, &jdk, tool, std::slice::from_ref(&jar), &[]).unwrap();
            let temp = PathBuf::from(flag(&share, "-XX:ArchiveClassesAtExit=").unwrap());
            temp.file_name().unwrap().to_string_lossy().into_owned()
        };
        assert!(
            target("org.jetbrains.kotlin.cli.jvm.K2JVMCompiler")
                .starts_with(".org.jetbrains.kotlin.cli.jvm.K2JVMCompiler-jdk21-")
        );
        assert!(target("scala.tools.nsc.Main").starts_with(".scala.tools.nsc.Main-jdk21-"));
        let before = key(
            &jdk_home(&jdk).unwrap(),
            &jdk,
            "t",
            std::slice::from_ref(&jar),
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&jar, "another build").unwrap();
        assert_ne!(
            before,
            key(
                &jdk_home(&jdk).unwrap(),
                &jdk,
                "t",
                std::slice::from_ref(&jar)
            ),
            "a rebuilt jar is another archive"
        );
    }

    #[test]
    fn no_archive_without_a_base_archive_or_against_the_users_flags() {
        let tree = Tree::new("skipped");
        let dir = tree.root.join("cds");
        assert!(Share::new(&dir, &tree.jdk(false), "javac", &[], &[]).is_none());
        let jdk = tree.jdk(true);
        for flag in ["-Xshare:off", "-XX:SharedArchiveFile=/mine.jsa"] {
            assert!(Share::new(&dir, &jdk, "javac", &[], &[flag.to_string()]).is_none());
        }
        assert!(Share::new(&dir, &jdk, "javac", &[], &["-Xmx2g".to_string()]).is_some());
    }

    #[test]
    fn a_failed_run_that_was_dumping_runs_again_without_sharing() {
        let tree = Tree::new("retry");
        let jdk = tree.jdk(true);
        let dir = tree.root.join("cds");
        let mut seen: Vec<Vec<String>> = Vec::new();
        let share = Share::new(&dir, &jdk, "javac", &[], &[]);
        let ok = run(
            share,
            |ok: &bool| *ok,
            |flags| {
                seen.push(flags.to_vec());
                Ok(flags.is_empty())
            },
        )
        .unwrap();
        assert!(ok, "the run without sharing is the one that counts");
        assert_eq!(seen.len(), 2);
        assert!(seen[0][0].starts_with("-XX:ArchiveClassesAtExit="));
        assert!(seen[1].is_empty());

        // Reading an archive is never retried: the JVM skips one it cannot
        // map, so a failure there is the compiler's own.
        std::fs::write(
            dir.join(format!(
                "javac-jdk21-{}.jsa",
                key(&jdk_home(&jdk).unwrap(), &jdk, "javac", &[])
            )),
            "a",
        )
        .unwrap();
        let share = Share::new(&dir, &jdk, "javac", &[], &[]);
        let mut runs = 0;
        let ok = run(
            share,
            |ok: &bool| *ok,
            |_| {
                runs += 1;
                Ok(false)
            },
        )
        .unwrap();
        assert!(!ok);
        assert_eq!(runs, 1);
    }

    #[test]
    fn a_projects_test_archive_replaces_its_older_ones_and_forks_only_read() {
        let tree = Tree::new("tests");
        let jdk = tree.jdk(true);
        let dir = tree.root.join("cds");
        let jar = tree.root.join("dep.jar");
        std::fs::write(&jar, "one").unwrap();
        let dump = |project: &str| {
            let share = Share::for_tests(
                &dir,
                &jdk,
                project,
                std::slice::from_ref(&jar),
                &[],
                true,
                false,
            )
            .unwrap();
            assert!(share.dumps());
            let temp = PathBuf::from(flag(&share, "-XX:ArchiveClassesAtExit=").unwrap());
            std::fs::write(&temp, "archive").unwrap();
            let archive = share.archive().unwrap().to_path_buf();
            share.finish(true);
            archive
        };
        assert!(
            Share::for_tests(
                &dir,
                &jdk,
                "app",
                std::slice::from_ref(&jar),
                &[],
                false,
                false
            )
            .is_none(),
            "a fork does not dump"
        );
        let first = dump("app");
        let other = dump("another project");
        let read = Share::for_tests(
            &dir,
            &jdk,
            "app",
            std::slice::from_ref(&jar),
            &[],
            false,
            false,
        )
        .unwrap();
        assert_eq!(read.archive(), Some(first.as_path()));

        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&jar, "a new release").unwrap();
        let second = dump("app");
        assert_ne!(first, second);
        assert!(!first.exists(), "the project's older archive is gone");
        assert!(other.exists(), "another project's is not");

        let agent = ["-javaagent:/x.jar".to_string()];
        let with_agent = Share::for_tests(
            &dir,
            &jdk,
            "app",
            std::slice::from_ref(&jar),
            &agent,
            true,
            false,
        )
        .unwrap();
        assert!(with_agent.dumps(), "an agent changes what is archived");
        with_agent.finish(false);
    }

    #[test]
    fn the_aot_cache_needs_jdk_24_and_records_quietly() {
        let tree = Tree::new("aot");
        let dir = tree.root.join("cds");
        let mut jdk = tree.jdk(false);
        let for_tests = |jdk: &Toolchain| Share::for_tests(&dir, jdk, "app", &[], &[], true, true);
        assert!(
            for_tests(&jdk).is_none(),
            "JDK 21 falls back to the dynamic archive, which needs a base archive"
        );
        jdk.version = AOT_SINCE;
        let share = for_tests(&jdk).unwrap();
        assert!(share.is_aot() && share.dumps());
        assert!(share.flags.contains(&"-XX:AOTMode=record".to_string()));
        assert!(share.flags.contains(&"-XX:-DisplayVMOutput".to_string()));
        let archive = share.archive().unwrap().to_path_buf();
        assert!(archive.extension().is_some_and(|e| e == "aot"));
        share.finish(false);

        std::fs::write(&archive, "cache").unwrap();
        let share = for_tests(&jdk).unwrap();
        assert!(share.is_aot() && !share.dumps());
        assert_eq!(
            share.flags[0],
            format!("-XX:AOTCache={}", archive.display())
        );
    }

    #[test]
    fn a_test_cache_that_cannot_be_assembled_falls_back_to_the_archive() {
        let tree = Tree::new("unassembled");
        let dir = tree.root.join("cds");
        let mut jdk = tree.jdk(true);
        jdk.version = JAVAC_AOT_SINCE;
        let for_tests = |dump: bool| Share::for_tests(&dir, &jdk, "app", &[], &[], dump, true);
        let share = for_tests(true).unwrap();
        assert!(share.is_aot() && share.dumps());
        let temp = PathBuf::from(flag(&share, "-XX:AOTConfiguration=").unwrap());
        let cache = share.archive().unwrap().to_path_buf();
        std::fs::write(&temp, "recorded").unwrap();
        // The fake JDK's `java` cannot assemble anything.
        share.finish(true);
        assert!(!cache.exists());
        assert!(unassembled(&cache).is_file());

        let share = for_tests(true).unwrap();
        assert!(
            !share.is_aot() && share.dumps(),
            "the dynamic archive instead"
        );
        let temp = PathBuf::from(flag(&share, "-XX:ArchiveClassesAtExit=").unwrap());
        std::fs::write(&temp, "archive").unwrap();
        share.finish(true);
        assert!(unassembled(&cache).is_file(), "kept beside the archive");
        let share = for_tests(false).unwrap();
        assert!(!share.is_aot() && !share.dumps(), "forks map the archive");

        let failed = Share::for_tests(&dir, &jdk, "other", &[], &[], true, true).unwrap();
        let cache = failed.archive().unwrap().to_path_buf();
        failed.finish(false);
        assert!(
            !unassembled(&cache).exists(),
            "a failed run says nothing of the assembly"
        );
    }

    /// Leave the marker [`seen_before`] reads as another command's, and
    /// return it.
    fn seen_by_another_command(archive: &Path) -> PathBuf {
        let marker = marker(archive);
        std::fs::write(&marker, "0").unwrap();
        marker
    }

    #[test]
    fn javac_gets_an_aot_cache_from_jdk_25_made_by_the_second_command() {
        let tree = Tree::new("javac-aot");
        let dir = tree.root.join("cds");
        let mut jdk = tree.jdk(true);
        jdk.version = JAVAC_AOT_SINCE;
        let (archive, aot) = javac_archive(&dir, &jdk).unwrap();
        assert!(aot);
        assert!(
            archive
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("javac-jdk25-")
        );
        assert!(
            Share::for_javac(&dir, &jdk, false).is_none(),
            "the first command to find none records nothing"
        );
        assert!(marker(&archive).is_file());
        assert!(
            Share::for_javac(&dir, &jdk, false).is_none(),
            "and neither does its second look"
        );

        let marker = seen_by_another_command(&archive);
        assert!(
            Share::for_javac(&dir, &jdk, true).is_none(),
            "a few files never record"
        );
        let share = Share::for_javac(&dir, &jdk, false).unwrap();
        assert!(share.is_aot() && share.dumps());
        assert!(share.flags.contains(&"-XX:-DisplayVMOutput".to_string()));
        assert_eq!(share.archive(), Some(archive.as_path()));
        let assemble = share.assemble.as_ref().unwrap();
        assert_eq!(
            assemble.classpath, None,
            "javac runs its module, not a classpath"
        );
        assert_eq!(
            assemble.main.last().unwrap(),
            "jdk.compiler/com.sun.tools.javac.Main"
        );
        share.finish(false);
        assert!(marker.is_file(), "still to be written");

        std::fs::write(&archive, "cache").unwrap();
        assert!(Share::javac_ready(&dir, &jdk));
        for quick in [false, true] {
            let share = Share::for_javac(&dir, &jdk, quick).unwrap();
            assert!(share.is_aot() && !share.dumps(), "one cache for both");
            assert_eq!(
                share.flags[0],
                format!("-XX:AOTCache={}", archive.display())
            );
        }
    }

    #[test]
    fn an_archive_in_place_takes_its_marker_away() {
        let tree = Tree::new("marker");
        let dir = tree.root.join("cds");
        let jdk = tree.jdk(true);
        let (archive, aot) = javac_archive(&dir, &jdk).unwrap();
        assert!(!aot, "JDK 21 dumps a dynamic archive");
        assert!(Share::for_javac(&dir, &jdk, false).is_none());
        let marker = seen_by_another_command(&archive);
        let share = Share::for_javac(&dir, &jdk, false).unwrap();
        let temp = PathBuf::from(flag(&share, "-XX:ArchiveClassesAtExit=").unwrap());
        std::fs::write(&temp, "archive").unwrap();
        share.finish(true);
        assert!(archive.is_file());
        assert!(!marker.exists());
    }

    #[test]
    fn the_worker_records_an_aot_cache_of_its_own_only_when_allowed() {
        let tree = Tree::new("worker-aot");
        let dir = tree.root.join("cds");
        let mut jdk = tree.jdk(true);
        jdk.version = JAVAC_AOT_SINCE;
        let program = tree.root.join("worker.jar");
        std::fs::write(&program, "jar").unwrap();
        let (archive, _) = worker_archive(&dir, &jdk, "JavacWorker", &program).unwrap();
        assert_ne!(
            Some(archive.clone()),
            javac_archive(&dir, &jdk).map(|a| a.0)
        );
        assert!(Share::for_worker(&dir, &jdk, "JavacWorker", &program, true).is_none());
        seen_by_another_command(&archive);
        assert!(
            Share::for_worker(&dir, &jdk, "JavacWorker", &program, false).is_none(),
            "a worker that may not record"
        );
        let share = Share::for_worker(&dir, &jdk, "JavacWorker", &program, true).unwrap();
        assert!(share.is_aot() && share.dumps());
        let assemble = share.assemble.as_ref().unwrap();
        assert_eq!(assemble.classpath, Some(vec![program.clone()]));
        share.finish(false);

        assert!(!Share::worker_ready(&dir, &jdk, "JavacWorker", &program));
        std::fs::write(&archive, "cache").unwrap();
        assert!(Share::worker_ready(&dir, &jdk, "JavacWorker", &program));
        let share = Share::for_worker(&dir, &jdk, "JavacWorker", &program, false).unwrap();
        assert!(share.is_aot() && !share.dumps(), "mapped by any worker");
    }

    #[test]
    fn an_abandoned_dump_is_swept_and_a_recent_one_is_not() {
        let tree = Tree::new("sweep");
        let jdk = tree.jdk(true);
        let dir = tree.root.join("cds");
        let first = Share::new(&dir, &jdk, "JavacWorker", &[], &[]).unwrap();
        let temp = PathBuf::from(flag(&first, "-XX:ArchiveClassesAtExit=").unwrap());
        let second = Share::new(&dir, &jdk, "JavacWorker", &[], &[]).unwrap();
        assert!(temp.exists(), "a dump a moment old may still be written");
        let long_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        std::fs::File::options()
            .write(true)
            .open(&temp)
            .unwrap()
            .set_modified(long_ago)
            .unwrap();
        let third = Share::new(&dir, &jdk, "JavacWorker", &[], &[]).unwrap();
        assert!(!temp.exists(), "an abandoned one is gone");
        second.finish(false);
        third.finish(false);
    }

    #[test]
    fn an_unwritable_directory_means_no_archive() {
        let tree = Tree::new("unwritable");
        let jdk = tree.jdk(true);
        let file = tree.root.join("not-a-dir");
        std::fs::write(&file, "").unwrap();
        assert!(Share::new(&file.join("cds"), &jdk, "javac", &[], &[]).is_none());
    }
}
