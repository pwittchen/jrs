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
//! The test JVM is left out: CDS refuses a classpath holding a non-empty
//! class directory, and `target/classes` is one.

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
];

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// One compiler run's use of its archive.
#[derive(Debug)]
pub(super) struct Share {
    /// The JVM flags: read the archive, or dump one.
    pub flags: Vec<String>,
    /// When dumping: the temporary file, and where it goes once the run
    /// succeeded.
    dump: Option<(PathBuf, PathBuf)>,
}

impl Share {
    /// The archive `tool` reads under `dir`, or `None` when it gets none. The
    /// tool is the program the JVM runs — `javac`'s module, a compiler's
    /// main class — and `classpath` what it is loaded from; `user_args` are
    /// the JVM flags the user gave it.
    pub(super) fn new(
        dir: &Path,
        toolchain: &Toolchain,
        tool: &str,
        classpath: &[PathBuf],
        user_args: &[String],
    ) -> Option<Share> {
        if user_args
            .iter()
            .any(|a| USER_FLAGS.iter().any(|f| a.starts_with(f)))
        {
            return None;
        }
        let home = jdk_home(toolchain)?;
        if !has_base_archive(&home) {
            return None;
        }
        // A missing directory is fatal to a JVM asked to dump into it.
        std::fs::create_dir_all(dir).ok()?;
        let archive = dir.join(format!(
            "{}-jdk{}-{}.jsa",
            file_safe(tool),
            toolchain.version,
            key(&home, toolchain, tool, classpath)
        ));
        if archive.is_file() {
            return Some(Share {
                flags: vec![
                    format!("-XX:SharedArchiveFile={}", archive.display()),
                    QUIET.to_string(),
                ],
                dump: None,
            });
        }
        let temp = dir.join(format!(
            ".{}.{}-{}.tmp",
            archive.file_name()?.to_string_lossy(),
            std::process::id(),
            TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        // A JVM that cannot write its dump does not start at all, so jrs
        // makes sure it can before asking.
        std::fs::File::create(&temp).ok()?;
        Some(Share {
            flags: vec![
                format!("-XX:ArchiveClassesAtExit={}", temp.display()),
                QUIET.to_string(),
            ],
            dump: Some((temp, archive)),
        })
    }

    /// After the run: put a dumped archive in place if the run succeeded,
    /// and drop it otherwise. A failure here only costs the next run its
    /// archive.
    pub(super) fn finish(self, ok: bool) {
        let Some((temp, archive)) = self.dump else {
            return;
        };
        let written = std::fs::metadata(&temp).is_ok_and(|m| m.len() > 0);
        if !(ok && written && std::fs::rename(&temp, &archive).is_ok()) {
            let _ = std::fs::remove_file(&temp);
        }
    }
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
pub(super) fn run<T>(
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
    fn an_unwritable_directory_means_no_archive() {
        let tree = Tree::new("unwritable");
        let jdk = tree.jdk(true);
        let file = tree.root.join("not-a-dir");
        std::fs::write(&file, "").unwrap();
        assert!(Share::new(&file.join("cds"), &jdk, "javac", &[], &[]).is_none());
    }
}
