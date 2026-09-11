//! Distribution archives: `jrs package --dist` (SPEC §9.6).
//!
//! The portable layout — the app jar with its `lib/` beside it, or a fat jar
//! alone — plus launch scripts in `bin/` that run it on whatever Java the
//! machine has: `$JAVA_HOME/bin/java` when `JAVA_HOME` is set, the `java` on
//! `PATH` otherwise. It is Gradle's `installDist` and `distZip` in one: staged
//! in `target/dist/<name>-<version>/`, which runs in place, then zipped into
//! `target/<name>-<version>.zip` under a top-level `<name>-<version>/`.
//!
//! The zip is deterministic like the jars: sorted entries, the fixed 1980
//! timestamp and fixed modes — 0755 for the POSIX launcher, which has to stay
//! executable once unzipped, and 0644 for everything else. Zip only: a tar.gz
//! would need a crate.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use zip::write::SimpleFileOptions;

use crate::error::{IoResultExt, JrsError, Result};
use crate::image::{App, bat_quote, is_sh_safe, sh_quote};
use crate::package::fixed_timestamp;
use crate::project;

/// What [`write_dist`] produced.
#[derive(Debug)]
pub struct DistOutcome {
    /// The staged distribution, `target/dist/<name>-<version>`.
    pub dir: PathBuf,
    /// `target/<name>-<version>.zip`.
    pub zip: PathBuf,
    /// The zip's size.
    pub bytes: u64,
    /// The files in it.
    pub entries: usize,
}

/// `<name>-<version>`: the staged directory's name and the zip's top-level
/// folder.
#[must_use]
pub fn base_name(app: &App) -> String {
    format!("{}-{}", app.name, app.version)
}

/// Stage `app` in `staging/<name>-<version>/` — the jar, `lib/` when it has
/// one, and the launchers `bin/<name>` (sh) and `bin/<name>.bat` — then zip
/// that directory into `zip`.
///
/// The staged directory is removed first: it lives under `target/`, and a
/// dependency that was dropped must not linger in `lib/`.
///
/// # Errors
///
/// [`JrsError::Build`] if the project is named after the `java` launcher (its
/// script would run itself), the jar's name is not UTF-8, or the zip writer
/// fails; [`JrsError::Io`] if the directory cannot be staged or the zip
/// written.
pub fn write_dist(app: &App, staging: &Path, zip: &Path) -> Result<DistOutcome> {
    // With the distribution's bin/ on PATH, `java` would find the launcher
    // itself, and it would exec itself forever.
    if matches!(app.name, "java" | "javaw") {
        return Err(JrsError::build(format!(
            "cannot write a launcher named `{}`: it would shadow the java it runs\n\n\
             rename the project",
            app.name
        )));
    }
    let base = base_name(app);
    let jar_name = app.jar_name()?;
    let dir = staging.join(&base);
    if dir.exists() {
        std::fs::remove_dir_all(&dir).path(&dir)?;
    }
    app.stage(&dir)?;

    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).path(&bin)?;
    let sh = bin.join(app.name);
    std::fs::write(&sh, sh_launcher(app.jvm_args, jar_name, app.java_agents)).path(&sh)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&sh, std::fs::Permissions::from_mode(0o755)).path(&sh)?;
    }
    let bat = bin.join(format!("{}.bat", app.name));
    std::fs::write(&bat, bat_launcher(app.jvm_args, jar_name, app.java_agents)).path(&bat)?;

    let entries = write_zip(&dir, &base, &format!("bin/{}", app.name), zip)?;
    let bytes = std::fs::metadata(zip).path(zip)?.len();
    Ok(DistOutcome {
        dir,
        zip: zip.to_path_buf(),
        bytes,
        entries,
    })
}

/// Zip every file under `dir` into `output`, each under `prefix/`, sorted by
/// name. `executable`, a path relative to `dir`, gets mode 0755. Returns the
/// number of entries.
fn write_zip(dir: &Path, prefix: &str, executable: &str, output: &Path) -> Result<usize> {
    let files: BTreeMap<String, PathBuf> = project::find_all(dir)?
        .into_iter()
        .map(|path| {
            let name = project::slash_path(path.strip_prefix(dir).unwrap_or(&path));
            (name, path)
        })
        .collect();

    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent).path(parent)?;
    }
    let file = std::fs::File::create(output).path(output)?;
    let mut writer = zip::ZipWriter::new(std::io::BufWriter::new(file));
    let zip_error =
        |e: zip::result::ZipError| JrsError::build(format!("{}: {e}", output.display()));
    for (name, path) in &files {
        let mode = if name == executable { 0o755 } else { 0o644 };
        let options = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .last_modified_time(fixed_timestamp())
            .unix_permissions(mode);
        writer
            .start_file(format!("{prefix}/{name}").as_str(), options)
            .map_err(zip_error)?;
        let bytes = std::fs::read(path).path(path)?;
        writer.write_all(&bytes).path(output)?;
    }
    let inner = writer.finish().map_err(zip_error)?;
    inner
        .into_inner()
        .map_err(|e| JrsError::build(format!("{}: {}", output.display(), e.error())))?;
    Ok(files.len())
}

/// The POSIX launcher. It finds the distribution from its own location, so it
/// can be unpacked anywhere, and the JVM from `JAVA_HOME` or `PATH`, saying
/// which it looked for when there is none. `$JAVA_OPTS` comes before the
/// fixed arguments, as the conventional escape hatch. `agents` are
/// `lib/<file>` entries, relative to the distribution's root.
pub(crate) fn sh_launcher(jvm_args: &[String], jar_name: &str, agents: &[String]) -> String {
    let mut s = String::from("#!/bin/sh\n# Generated by jrs.\n");
    s.push_str("DIR=$(cd \"$(dirname \"$0\")/..\" && pwd)\n");
    s.push_str("if [ -n \"$JAVA_HOME\" ]; then\n");
    s.push_str("    JAVACMD=\"$JAVA_HOME/bin/java\"\n");
    s.push_str("    if [ ! -x \"$JAVACMD\" ]; then\n");
    s.push_str("        echo \"error: JAVA_HOME is $JAVA_HOME, but there is no $JAVACMD\" >&2\n");
    s.push_str("        exit 1\n");
    s.push_str("    fi\n");
    s.push_str("else\n");
    s.push_str("    JAVACMD=java\n");
    s.push_str("    if ! command -v java >/dev/null 2>&1; then\n");
    s.push_str("        echo \"error: JAVA_HOME is not set and there is no java on PATH\" >&2\n");
    s.push_str("        exit 1\n");
    s.push_str("    fi\n");
    s.push_str("fi\n");
    s.push_str("# JAVA_OPTS is unquoted on purpose: it holds any number of options.\n");
    s.push_str("exec \"$JAVACMD\" $JAVA_OPTS");
    for agent in agents {
        if agent.chars().all(|c| is_sh_safe(c) || c == '/') {
            let _ = write!(s, " \"-javaagent:$DIR/{agent}\"");
        } else {
            let _ = write!(s, " \"-javaagent:$DIR/\"{}", sh_quote(agent));
        }
    }
    for arg in jvm_args {
        s.push(' ');
        s.push_str(&sh_quote(arg));
    }
    if jar_name.chars().all(is_sh_safe) {
        let _ = write!(s, " -jar \"$DIR/{jar_name}\"");
    } else {
        let _ = write!(s, " -jar \"$DIR/\"{}", sh_quote(jar_name));
    }
    s.push_str(" \"$@\"\n");
    s
}

/// The Windows launcher, with CRLF line endings because `cmd` is happiest
/// with them. `java.exe` from `JAVA_HOME` when it is set, from `PATH`
/// otherwise; the program's exit code is the script's. `agents` are
/// `lib/<file>` entries, relative to the distribution's root.
pub(crate) fn bat_launcher(jvm_args: &[String], jar_name: &str, agents: &[String]) -> String {
    let mut s = String::new();
    for line in [
        "@echo off",
        "rem Generated by jrs.",
        "setlocal",
        "set \"DIR=%~dp0..\"",
        "set \"JAVACMD=java.exe\"",
        "if defined JAVA_HOME set \"JAVACMD=%JAVA_HOME%\\bin\\java.exe\"",
    ] {
        s.push_str(line);
        s.push_str("\r\n");
    }
    s.push_str("\"%JAVACMD%\" %JAVA_OPTS%");
    for agent in agents {
        let _ = write!(
            s,
            " \"-javaagent:%DIR%\\{}\"",
            agent.replace('/', "\\").replace('%', "%%")
        );
    }
    for arg in jvm_args {
        s.push(' ');
        s.push_str(&bat_quote(arg));
    }
    let _ = write!(s, " -jar \"%DIR%\\{}\" %*\r\n", jar_name.replace('%', "%%"));
    s.push_str("exit /b %ERRORLEVEL%\r\n");
    s
}

#[cfg(test)]
mod tests {
    use std::io::Read as _;

    use super::*;

    struct Tree {
        root: PathBuf,
    }

    impl Tree {
        fn new(name: &str) -> Tree {
            let root = std::env::temp_dir().join(format!("jrs-dist-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap();
            Tree { root }
        }

        fn write(&self, relative: &str, contents: &str) -> PathBuf {
            let path = self.root.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, contents).unwrap();
            path
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(ToString::to_string).collect()
    }

    /// Name, unix mode and contents of every entry, in archive order.
    fn entries(zip: &Path) -> Vec<(String, Option<u32>, String)> {
        let mut archive = zip::ZipArchive::new(std::fs::File::open(zip).unwrap()).unwrap();
        (0..archive.len())
            .map(|i| {
                let mut entry = archive.by_index(i).unwrap();
                let mut text = String::new();
                entry.read_to_string(&mut text).unwrap();
                (entry.name().to_string(), entry.unix_mode(), text)
            })
            .collect()
    }

    #[test]
    fn the_sh_launcher_takes_java_from_java_home_or_path() {
        let text = sh_launcher(&strings(&["-Xmx512m", "-Dq=it's"]), "app-1.0.jar", &[]);
        assert_eq!(
            text,
            "#!/bin/sh\n\
             # Generated by jrs.\n\
             DIR=$(cd \"$(dirname \"$0\")/..\" && pwd)\n\
             if [ -n \"$JAVA_HOME\" ]; then\n    \
                 JAVACMD=\"$JAVA_HOME/bin/java\"\n    \
                 if [ ! -x \"$JAVACMD\" ]; then\n        \
                     echo \"error: JAVA_HOME is $JAVA_HOME, but there is no $JAVACMD\" >&2\n        \
                     exit 1\n    \
                 fi\n\
             else\n    \
                 JAVACMD=java\n    \
                 if ! command -v java >/dev/null 2>&1; then\n        \
                     echo \"error: JAVA_HOME is not set and there is no java on PATH\" >&2\n        \
                     exit 1\n    \
                 fi\n\
             fi\n\
             # JAVA_OPTS is unquoted on purpose: it holds any number of options.\n\
             exec \"$JAVACMD\" $JAVA_OPTS '-Xmx512m' '-Dq=it'\\''s' -jar \"$DIR/app-1.0.jar\" \"$@\"\n"
        );
    }

    #[test]
    fn the_sh_launcher_quotes_an_awkward_jar_name() {
        let text = sh_launcher(&[], "my app's.jar", &[]);
        assert!(
            text.contains(r#"-jar "$DIR/"'my app'\''s.jar' "$@""#),
            "{text}"
        );
    }

    #[test]
    fn the_bat_launcher_takes_java_from_java_home_or_path() {
        let text = bat_launcher(
            &strings(&["-Xmx512m", "-Dgreeting=hello world", "-Dp=5%"]),
            "app.jar",
            &[],
        );
        assert_eq!(
            text,
            "@echo off\r\n\
             rem Generated by jrs.\r\n\
             setlocal\r\n\
             set \"DIR=%~dp0..\"\r\n\
             set \"JAVACMD=java.exe\"\r\n\
             if defined JAVA_HOME set \"JAVACMD=%JAVA_HOME%\\bin\\java.exe\"\r\n\
             \"%JAVACMD%\" %JAVA_OPTS% -Xmx512m \"-Dgreeting=hello world\" -Dp=5%% \
             -jar \"%DIR%\\app.jar\" %*\r\n\
             exit /b %ERRORLEVEL%\r\n"
        );
    }

    #[test]
    fn the_launchers_load_agents_from_lib_ahead_of_the_jvm_arguments() {
        let agents = strings(&["lib/agent-1.0.jar", "lib/my agent.jar"]);
        let sh = sh_launcher(&strings(&["-Xmx1g"]), "app.jar", &agents);
        assert!(
            sh.contains(
                " $JAVA_OPTS \"-javaagent:$DIR/lib/agent-1.0.jar\" \
                 \"-javaagent:$DIR/\"'lib/my agent.jar' '-Xmx1g' -jar"
            ),
            "{sh}"
        );
        let bat = bat_launcher(&strings(&["-Xmx1g"]), "app.jar", &agents[..1]);
        assert!(
            bat.contains(" %JAVA_OPTS% \"-javaagent:%DIR%\\lib\\agent-1.0.jar\" -Xmx1g -jar"),
            "{bat}"
        );
    }

    #[test]
    fn the_bat_launcher_escapes_percent_in_the_jar_name() {
        let text = bat_launcher(&[], "100%.jar", &[]);
        assert!(text.contains(r#""%DIR%\100%%.jar" %*"#), "{text}");
    }

    fn app<'a>(jar: &'a Path, lib: Option<&'a Path>, jvm_args: &'a [String]) -> App<'a> {
        App {
            name: "demo",
            version: "1.2.0",
            main_class: Some("app.Main"),
            jar,
            lib_dir: lib,
            jvm_args,
            java_agents: &[],
        }
    }

    #[test]
    fn a_distribution_is_staged_and_zipped_under_one_folder() {
        let tree = Tree::new("layout");
        let jar = tree.write("target/demo-1.2.0.jar", "the jar");
        tree.write("target/lib/z.jar", "z");
        tree.write("target/lib/a.jar", "a");
        let lib = tree.root.join("target/lib");
        let jvm_args = strings(&["-Dmode=dist"]);
        let zip = tree.root.join("target/demo-1.2.0.zip");
        // A stale file from an earlier distribution does not survive.
        tree.write("target/dist/demo-1.2.0/lib/stale.jar", "old");

        let outcome = write_dist(
            &app(&jar, Some(&lib), &jvm_args),
            &tree.root.join("target/dist"),
            &zip,
        )
        .unwrap();
        assert_eq!(outcome.dir, tree.root.join("target/dist/demo-1.2.0"));
        assert_eq!(outcome.entries, 5);
        assert!(outcome.bytes > 0);
        assert!(!outcome.dir.join("lib/stale.jar").exists());

        let listed = entries(&zip);
        let names: Vec<&str> = listed.iter().map(|(n, _, _)| n.as_str()).collect();
        assert_eq!(
            names,
            [
                "demo-1.2.0/bin/demo",
                "demo-1.2.0/bin/demo.bat",
                "demo-1.2.0/demo-1.2.0.jar",
                "demo-1.2.0/lib/a.jar",
                "demo-1.2.0/lib/z.jar",
            ]
        );
        for (name, mode, _) in &listed {
            let expected = if name == "demo-1.2.0/bin/demo" {
                0o755
            } else {
                0o644
            };
            assert_eq!(mode.map(|m| m & 0o777), Some(expected), "{name}");
        }
        assert!(
            listed[0]
                .2
                .contains("'-Dmode=dist' -jar \"$DIR/demo-1.2.0.jar\"")
        );
        assert_eq!(listed[2].2, "the jar");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(outcome.dir.join("bin/demo"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o755);
        }
    }

    #[test]
    fn a_fat_distribution_has_no_lib() {
        let tree = Tree::new("fat");
        let jar = tree.write("target/demo-1.2.0.jar", "fat");
        let zip = tree.root.join("target/demo-1.2.0.zip");
        write_dist(&app(&jar, None, &[]), &tree.root.join("target/dist"), &zip).unwrap();
        let names: Vec<String> = entries(&zip).into_iter().map(|(n, _, _)| n).collect();
        assert_eq!(
            names,
            [
                "demo-1.2.0/bin/demo",
                "demo-1.2.0/bin/demo.bat",
                "demo-1.2.0/demo-1.2.0.jar"
            ]
        );
    }

    #[test]
    fn the_zip_is_byte_identical_across_builds() {
        let tree = Tree::new("deterministic");
        let jar = tree.write("target/demo-1.2.0.jar", "the jar");
        tree.write("target/lib/a.jar", "a");
        let lib = tree.root.join("target/lib");
        let first = tree.root.join("first.zip");
        let second = tree.root.join("second.zip");
        for zip in [&first, &second] {
            write_dist(
                &app(&jar, Some(&lib), &[]),
                &tree.root.join("target/dist"),
                zip,
            )
            .unwrap();
            // Restaging rewrites every file; the zip must not notice.
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(
            std::fs::read(&first).unwrap(),
            std::fs::read(&second).unwrap()
        );
    }

    #[test]
    fn a_project_named_java_is_refused() {
        let tree = Tree::new("java");
        let jar = tree.write("target/java-1.0.jar", "");
        let app = App {
            name: "java",
            ..app(&jar, None, &[])
        };
        let err = write_dist(&app, &tree.root.join("dist"), &tree.root.join("x.zip")).unwrap_err();
        assert!(err.to_string().contains("rename the project"), "{err}");
    }
}
