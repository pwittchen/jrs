//! Building jars.
//!
//! Output is deterministic: entries sorted, timestamps fixed, permissions fixed,
//! so two builds of the same inputs produce byte-identical jars (SPEC §9.1).
//!
//! The fat jar's merge rules are where the real care goes. `META-INF/services/*`
//! entries are concatenated rather than overwritten — getting that wrong breaks
//! `ServiceLoader` silently, which is the worst kind of packaging bug.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};

use zip::write::SimpleFileOptions;

use crate::error::{IoResultExt, JrsError, Result};
use crate::project;

/// The epoch every entry is stamped with, so repeated builds match byte for byte.
/// 1980-01-01 is the earliest a zip timestamp can express.
fn fixed_timestamp() -> zip::DateTime {
    zip::DateTime::from_date_and_time(1980, 1, 1, 0, 0, 0).expect("a valid fixed timestamp")
}

fn entry_options() -> SimpleFileOptions {
    SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .last_modified_time(fixed_timestamp())
        .unix_permissions(0o644)
}

#[derive(Debug, Clone, Default)]
pub struct JarManifest {
    pub main_class: Option<String>,
    /// Relative jar paths for a thin jar's `Class-Path` header.
    pub class_path: Vec<String>,
}

impl JarManifest {
    /// Render `META-INF/MANIFEST.MF`, wrapped the way the jar spec requires.
    pub fn render(&self) -> String {
        let mut s = String::new();
        s.push_str("Manifest-Version: 1.0\n");
        let _ = writeln!(s, "Created-By: jrs {}", env!("CARGO_PKG_VERSION"));
        if let Some(main) = &self.main_class {
            s.push_str(&wrap_header("Main-Class", main));
        }
        if !self.class_path.is_empty() {
            s.push_str(&wrap_header("Class-Path", &self.class_path.join(" ")));
        }
        s.push('\n');
        s
    }
}

/// Jar manifest lines are limited to 72 bytes; continuations start with a space.
fn wrap_header(name: &str, value: &str) -> String {
    let line = format!("{name}: {value}");
    let mut out = String::new();
    let bytes = line.as_bytes();
    let mut start = 0;
    let mut limit = 71;
    while start < bytes.len() {
        let end = (start + limit).min(bytes.len());
        // Never split a multi-byte character.
        let mut end = end;
        while end > start && !line.is_char_boundary(end) {
            end -= 1;
        }
        if start > 0 {
            out.push(' ');
        }
        out.push_str(&line[start..end]);
        out.push('\n');
        start = end;
        limit = 70;
    }
    out
}

/// Render one `Class-Path` entry.
///
/// The header is a space-separated list of URLs, so a path with a space in it —
/// `/Users/Ada Lovelace/Library/Caches/jrs/...` — cannot go in verbatim: it would
/// read as two entries, and the classpath would silently lose a jar. Emitting a
/// `file:` URL with the reserved characters percent-encoded avoids that.
pub fn class_path_entry(path: &Path) -> String {
    format!("file:{}", url_encode(&path.to_string_lossy()))
}

/// Percent-encode everything a `Class-Path` URL cannot carry verbatim.
fn url_encode(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' => out.push(byte as char),
            b'/' | b'-' | b'_' | b'.' | b'~' | b'+' | b':' => out.push(byte as char),
            b'\\' if cfg!(windows) => out.push('/'),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// The portable layout: copy the runtime dependencies into `lib_dir` and return
/// the `Class-Path` entries, relative to the jar beside it, that point at them.
///
/// A thin jar whose `Class-Path` names the cache runs only on the machine that
/// built it; a jar with a `lib/` beside it can be zipped up and shipped.
/// `lib_dir` is emptied first — it lives under `target/`, and a dependency that
/// was dropped must not linger there. Each jar keeps its own file name, except
/// when two dependencies share one (the same artifact name in two groups): then
/// both get their group as a prefix, so neither shadows the other.
///
/// `libraries` are `(group, jar)` pairs in classpath order, and the entries
/// come back in the same order.
pub fn copy_libraries(
    libraries: &[(String, PathBuf)],
    lib_dir: &Path,
    prefix: &str,
) -> Result<Vec<String>> {
    if lib_dir.exists() {
        std::fs::remove_dir_all(lib_dir).path(lib_dir)?;
    }
    std::fs::create_dir_all(lib_dir).path(lib_dir)?;

    let file_name = |p: &Path| {
        p.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    let mut entries = Vec::with_capacity(libraries.len());
    for (group, jar) in libraries {
        let name = file_name(jar);
        let shared = libraries
            .iter()
            .filter(|(_, other)| file_name(other) == name)
            .count()
            > 1;
        let name = if shared {
            format!("{group}.{name}")
        } else {
            name
        };
        let destination = lib_dir.join(&name);
        std::fs::copy(jar, &destination).path(&destination)?;
        entries.push(format!("{prefix}/{}", url_encode(&name)));
    }
    Ok(entries)
}

#[derive(Debug)]
pub struct PackageOutcome {
    pub path: PathBuf,
    pub bytes: u64,
    pub entries: usize,
    pub warnings: Vec<String>,
}

/// A thin jar: just this project's classes and resources (SPEC §9.1).
pub fn write_thin_jar(
    classes_dir: &Path,
    output: &Path,
    manifest: &JarManifest,
) -> Result<PackageOutcome> {
    let mut plan: BTreeMap<String, Entry> = BTreeMap::new();
    collect_directory(classes_dir, &mut plan)?;
    write_jar(output, manifest, plan, no_archives(), Vec::new())
}

/// A fat jar: this project's classes plus every runtime dependency, unpacked.
///
/// Dependencies are read straight into the output rather than through a staging
/// directory — the resulting jar is identical, and a build that packages 40 MB of
/// dependencies should not write 40 MB to disk twice.
pub fn write_fat_jar(
    classes_dir: &Path,
    dependency_jars: &[PathBuf],
    output: &Path,
    manifest: &JarManifest,
) -> Result<PackageOutcome> {
    if manifest.main_class.is_none() {
        return Err(JrsError::manifest(
            "`jrs package --fat` needs a main class\n\n\
             add it to jrs.toml:\n\n    [project]\n    main-class = \"com.example.Main\"",
        ));
    }

    let mut plan: BTreeMap<String, Entry> = BTreeMap::new();
    let mut warnings = Vec::new();

    // The project's own classes go in first, so they win every conflict.
    collect_directory(classes_dir, &mut plan)?;

    let mut archives = Vec::new();
    for (index, jar) in dependency_jars.iter().enumerate() {
        let file = std::fs::File::open(jar).path(jar)?;
        let mut archive = zip::ZipArchive::new(file)
            .map_err(|e| JrsError::build(format!("{}: not a readable jar: {e}", jar.display())))?;

        for i in 0..archive.len() {
            let entry = archive
                .by_index(i)
                .map_err(|e| JrsError::build(format!("{}: {e}", jar.display())))?;
            if entry.is_dir() {
                continue;
            }
            let name = entry.name().to_string();
            if is_dropped(&name) {
                continue;
            }
            if is_service_file(&name) {
                match plan.get_mut(&name) {
                    Some(Entry::Services(sources)) => sources.push((index, i)),
                    _ => {
                        plan.insert(name, Entry::Services(vec![(index, i)]));
                    }
                }
                continue;
            }
            match plan.get(&name) {
                Some(existing) => {
                    if name.ends_with(".class") {
                        warnings.push(format!(
                            "duplicate class `{}`: kept the copy from {}, ignored {}",
                            name.trim_end_matches(".class").replace('/', "."),
                            existing.describe(classes_dir, dependency_jars),
                            jar.display()
                        ));
                    }
                }
                None => {
                    plan.insert(name, Entry::Jar(index, i));
                }
            }
        }
        archives.push(archive);
    }

    write_jar(output, manifest, plan, archives, warnings)
}

/// Where an entry's bytes come from.
enum Entry {
    File(PathBuf),
    Jar(usize, usize),
    /// A `META-INF/services` file, concatenated from every jar that has one.
    Services(Vec<(usize, usize)>),
}

impl Entry {
    fn describe(&self, classes_dir: &Path, jars: &[PathBuf]) -> String {
        let jar = |index: &usize| {
            jars.get(*index)
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "an earlier jar".to_string())
        };
        match self {
            Entry::File(_) => classes_dir.display().to_string(),
            Entry::Jar(index, _) => jar(index),
            Entry::Services(sources) => sources
                .first()
                .map(|(index, _)| jar(index))
                .unwrap_or_else(|| "an earlier jar".to_string()),
        }
    }
}

fn collect_directory(dir: &Path, plan: &mut BTreeMap<String, Entry>) -> Result<()> {
    for path in project::find_all(dir)? {
        let relative = path.strip_prefix(dir).unwrap_or(&path);
        let name = relative
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        if name == "META-INF/MANIFEST.MF" {
            // jrs writes its own; a stray one in the source tree does not win.
            continue;
        }
        plan.insert(name, Entry::File(path));
    }
    Ok(())
}

/// Signature files no longer describe the merged jar's contents (SPEC §9.2).
fn is_dropped(name: &str) -> bool {
    if name == "META-INF/MANIFEST.MF" || name == "META-INF/INDEX.LIST" {
        return true;
    }
    if let Some(rest) = name.strip_prefix("META-INF/")
        && !rest.contains('/')
    {
        let upper = rest.to_ascii_uppercase();
        return upper.ends_with(".SF")
            || upper.ends_with(".DSA")
            || upper.ends_with(".RSA")
            || upper.ends_with(".EC");
    }
    false
}

fn is_service_file(name: &str) -> bool {
    name.starts_with("META-INF/services/") && name.len() > "META-INF/services/".len()
}

/// The empty archive list a thin jar needs, with the type parameter pinned.
fn no_archives() -> Vec<zip::ZipArchive<std::fs::File>> {
    Vec::new()
}

fn write_jar<R: Read + Seek>(
    output: &Path,
    manifest: &JarManifest,
    plan: BTreeMap<String, Entry>,
    mut archives: Vec<zip::ZipArchive<R>>,
    warnings: Vec<String>,
) -> Result<PackageOutcome> {
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent).path(parent)?;
    }
    let file = std::fs::File::create(output).path(output)?;
    let mut writer = zip::ZipWriter::new(std::io::BufWriter::new(file));
    let options = entry_options();

    // The manifest goes first, as every jar reader expects.
    writer
        .start_file("META-INF/MANIFEST.MF", options)
        .map_err(|e| JrsError::build(format!("{}: {e}", output.display())))?;
    writer
        .write_all(manifest.render().as_bytes())
        .path(output)?;
    let mut entries = 1;

    for (name, entry) in &plan {
        writer
            .start_file(name.as_str(), options)
            .map_err(|e| JrsError::build(format!("{}: {e}", output.display())))?;
        match entry {
            Entry::File(path) => {
                let bytes = std::fs::read(path).path(path)?;
                writer.write_all(&bytes).path(output)?;
            }
            Entry::Jar(archive, index) => {
                let mut source = archives[*archive]
                    .by_index(*index)
                    .map_err(|e| JrsError::build(format!("{}: {e}", output.display())))?;
                std::io::copy(&mut source, &mut writer).path(output)?;
            }
            Entry::Services(sources) => {
                // Concatenated, with a newline between files, so no provider is
                // glued onto the tail of the previous one.
                let mut merged = Vec::new();
                for (archive, index) in sources {
                    let mut source = archives[*archive]
                        .by_index(*index)
                        .map_err(|e| JrsError::build(format!("{}: {e}", output.display())))?;
                    let mut bytes = Vec::new();
                    source.read_to_end(&mut bytes).path(output)?;
                    if !merged.is_empty() && !merged.ends_with(b"\n") {
                        merged.push(b'\n');
                    }
                    merged.extend_from_slice(&bytes);
                }
                writer.write_all(&merged).path(output)?;
            }
        }
        entries += 1;
    }

    let inner = writer
        .finish()
        .map_err(|e| JrsError::build(format!("{}: {e}", output.display())))?;
    inner
        .into_inner()
        .map_err(|e| JrsError::build(format!("{}: {}", output.display(), e.error())))?;

    let bytes = std::fs::metadata(output).path(output)?.len();
    Ok(PackageOutcome {
        path: output.to_path_buf(),
        bytes,
        entries,
        warnings,
    })
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
                std::env::temp_dir().join(format!("jrs-package-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap();
            Tree { root }
        }

        fn write(&self, relative: &str, contents: &[u8]) -> PathBuf {
            let path = self.root.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, contents).unwrap();
            path
        }

        /// A jar fixture built entry by entry.
        fn jar(&self, name: &str, entries: &[(&str, &[u8])]) -> PathBuf {
            let path = self.root.join(name);
            let file = std::fs::File::create(&path).unwrap();
            let mut w = zip::ZipWriter::new(file);
            for (entry, bytes) in entries {
                w.start_file(*entry, entry_options()).unwrap();
                w.write_all(bytes).unwrap();
            }
            w.finish().unwrap();
            path
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn read_jar(path: &Path) -> BTreeMap<String, Vec<u8>> {
        let file = std::fs::File::open(path).unwrap();
        let mut archive = zip::ZipArchive::new(file).unwrap();
        let mut out = BTreeMap::new();
        for i in 0..archive.len() {
            let mut entry = archive.by_index(i).unwrap();
            let name = entry.name().to_string();
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).unwrap();
            out.insert(name, bytes);
        }
        out
    }

    fn entry_order(path: &Path) -> Vec<String> {
        let file = std::fs::File::open(path).unwrap();
        let mut archive = zip::ZipArchive::new(file).unwrap();
        (0..archive.len())
            .map(|i| archive.by_index(i).unwrap().name().to_string())
            .collect()
    }

    #[test]
    fn a_thin_jar_holds_classes_and_a_generated_manifest() {
        let tree = Tree::new("thin");
        tree.write("classes/com/example/Main.class", b"main bytes");
        tree.write("classes/app.properties", b"k=v");

        let out = tree.root.join("app.jar");
        let result = write_thin_jar(
            &tree.root.join("classes"),
            &out,
            &JarManifest {
                main_class: Some("com.example.Main".into()),
                class_path: vec!["lib/guava.jar".into()],
            },
        )
        .unwrap();

        assert_eq!(result.entries, 3);
        let contents = read_jar(&out);
        assert_eq!(contents["com/example/Main.class"], b"main bytes");
        assert_eq!(contents["app.properties"], b"k=v");

        let manifest = String::from_utf8(contents["META-INF/MANIFEST.MF"].clone()).unwrap();
        assert!(manifest.starts_with("Manifest-Version: 1.0\n"));
        assert!(manifest.contains("Main-Class: com.example.Main\n"));
        assert!(manifest.contains("Class-Path: lib/guava.jar\n"));
        assert!(manifest.contains("Created-By: jrs "));
    }

    #[test]
    fn the_manifest_is_the_first_entry() {
        let tree = Tree::new("manifest-first");
        tree.write("classes/aaa/First.class", b"x");
        let out = tree.root.join("app.jar");
        write_thin_jar(&tree.root.join("classes"), &out, &JarManifest::default()).unwrap();
        assert_eq!(entry_order(&out)[0], "META-INF/MANIFEST.MF");
    }

    #[test]
    fn repeated_builds_are_byte_identical() {
        let tree = Tree::new("deterministic");
        tree.write("classes/com/example/Main.class", b"main bytes");
        tree.write("classes/com/example/Other.class", b"other bytes");

        let first = tree.root.join("first.jar");
        let second = tree.root.join("second.jar");
        for out in [&first, &second] {
            write_thin_jar(&tree.root.join("classes"), out, &JarManifest::default()).unwrap();
            // Touching the inputs between builds must not change the output.
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(
            std::fs::read(&first).unwrap(),
            std::fs::read(&second).unwrap()
        );
    }

    #[test]
    fn entries_are_sorted() {
        let tree = Tree::new("sorted");
        tree.write("classes/z/Last.class", b"z");
        tree.write("classes/a/First.class", b"a");
        tree.write("classes/m/Middle.class", b"m");

        let out = tree.root.join("app.jar");
        write_thin_jar(&tree.root.join("classes"), &out, &JarManifest::default()).unwrap();
        assert_eq!(
            entry_order(&out),
            vec![
                "META-INF/MANIFEST.MF",
                "a/First.class",
                "m/Middle.class",
                "z/Last.class"
            ]
        );
    }

    #[test]
    fn long_class_paths_wrap_at_seventy_two_bytes() {
        let manifest = JarManifest {
            main_class: None,
            class_path: (0..10)
                .map(|i| format!("lib/dependency-{i}-1.0.0.jar"))
                .collect(),
        };
        let text = manifest.render();
        for line in text.lines() {
            assert!(line.len() <= 72, "line too long ({}): {line}", line.len());
        }
        // Continuation lines are marked with a leading space, and unfolding
        // recovers the original value.
        let folded: String = text
            .lines()
            .skip_while(|l| !l.starts_with("Class-Path:"))
            .take_while(|l| l.starts_with("Class-Path:") || l.starts_with(' '))
            .map(|l| l.strip_prefix(' ').unwrap_or(l))
            .collect();
        assert_eq!(
            folded,
            format!("Class-Path: {}", manifest.class_path.join(" "))
        );
    }

    #[test]
    fn a_fat_jar_unpacks_its_dependencies() {
        let tree = Tree::new("fat");
        tree.write("classes/com/example/Main.class", b"main");
        let dep = tree.jar(
            "dep.jar",
            &[
                ("org/dep/Thing.class", b"thing"),
                ("META-INF/MANIFEST.MF", b"Manifest-Version: 1.0\n"),
            ],
        );

        let out = tree.root.join("fat.jar");
        write_fat_jar(
            &tree.root.join("classes"),
            &[dep],
            &out,
            &JarManifest {
                main_class: Some("com.example.Main".into()),
                class_path: vec![],
            },
        )
        .unwrap();

        let contents = read_jar(&out);
        assert_eq!(contents["com/example/Main.class"], b"main");
        assert_eq!(contents["org/dep/Thing.class"], b"thing");
        let manifest = String::from_utf8(contents["META-INF/MANIFEST.MF"].clone()).unwrap();
        assert!(
            manifest.contains("Main-Class: com.example.Main"),
            "the dependency's manifest must not have overwritten ours"
        );
    }

    #[test]
    fn service_files_are_concatenated_not_overwritten() {
        let tree = Tree::new("services");
        std::fs::create_dir_all(tree.root.join("classes")).unwrap();
        let first = tree.jar(
            "first.jar",
            &[(
                "META-INF/services/java.sql.Driver",
                b"com.first.Driver\n" as &[u8],
            )],
        );
        let second = tree.jar(
            "second.jar",
            &[(
                "META-INF/services/java.sql.Driver",
                b"com.second.Driver\n" as &[u8],
            )],
        );

        let out = tree.root.join("fat.jar");
        write_fat_jar(
            &tree.root.join("classes"),
            &[first, second],
            &out,
            &JarManifest {
                main_class: Some("Main".into()),
                class_path: vec![],
            },
        )
        .unwrap();

        let services =
            String::from_utf8(read_jar(&out)["META-INF/services/java.sql.Driver"].clone()).unwrap();
        assert_eq!(services, "com.first.Driver\ncom.second.Driver\n");
    }

    #[test]
    fn a_service_file_without_a_trailing_newline_still_merges_cleanly() {
        let tree = Tree::new("services-no-newline");
        std::fs::create_dir_all(tree.root.join("classes")).unwrap();
        let first = tree.jar(
            "first.jar",
            &[("META-INF/services/x.Y", b"com.first.Impl" as &[u8])],
        );
        let second = tree.jar(
            "second.jar",
            &[("META-INF/services/x.Y", b"com.second.Impl" as &[u8])],
        );

        let out = tree.root.join("fat.jar");
        write_fat_jar(
            &tree.root.join("classes"),
            &[first, second],
            &out,
            &JarManifest {
                main_class: Some("Main".into()),
                class_path: vec![],
            },
        )
        .unwrap();
        let services = String::from_utf8(read_jar(&out)["META-INF/services/x.Y"].clone()).unwrap();
        assert_eq!(services, "com.first.Impl\ncom.second.Impl");
    }

    #[test]
    fn signature_files_are_dropped() {
        assert!(is_dropped("META-INF/MANIFEST.MF"));
        assert!(is_dropped("META-INF/SIGNER.SF"));
        assert!(is_dropped("META-INF/SIGNER.DSA"));
        assert!(is_dropped("META-INF/SIGNER.RSA"));
        assert!(is_dropped("META-INF/signer.ec"));
        assert!(!is_dropped("META-INF/services/java.sql.Driver"));
        assert!(!is_dropped("META-INF/LICENSE"));
        assert!(!is_dropped("com/example/Signed.class"));
    }

    #[test]
    fn a_signed_dependency_loses_its_signature() {
        let tree = Tree::new("signed");
        std::fs::create_dir_all(tree.root.join("classes")).unwrap();
        let dep = tree.jar(
            "signed.jar",
            &[
                ("org/dep/Thing.class", b"thing" as &[u8]),
                ("META-INF/SIGNER.SF", b"signature" as &[u8]),
                ("META-INF/SIGNER.RSA", b"key" as &[u8]),
            ],
        );

        let out = tree.root.join("fat.jar");
        write_fat_jar(
            &tree.root.join("classes"),
            &[dep],
            &out,
            &JarManifest {
                main_class: Some("Main".into()),
                class_path: vec![],
            },
        )
        .unwrap();

        let contents = read_jar(&out);
        assert!(contents.contains_key("org/dep/Thing.class"));
        assert!(!contents.contains_key("META-INF/SIGNER.SF"));
        assert!(!contents.contains_key("META-INF/SIGNER.RSA"));
    }

    #[test]
    fn duplicate_classes_keep_the_first_and_warn() {
        let tree = Tree::new("duplicates");
        std::fs::create_dir_all(tree.root.join("classes")).unwrap();
        let first = tree.jar("first.jar", &[("org/Shared.class", b"from first" as &[u8])]);
        let second = tree.jar(
            "second.jar",
            &[("org/Shared.class", b"from second" as &[u8])],
        );

        let out = tree.root.join("fat.jar");
        let result = write_fat_jar(
            &tree.root.join("classes"),
            &[first, second],
            &out,
            &JarManifest {
                main_class: Some("Main".into()),
                class_path: vec![],
            },
        )
        .unwrap();

        assert_eq!(read_jar(&out)["org/Shared.class"], b"from first");
        assert_eq!(result.warnings.len(), 1);
        assert!(
            result.warnings[0].contains("org.Shared"),
            "{:?}",
            result.warnings
        );
        assert!(
            result.warnings[0].contains("second.jar"),
            "{:?}",
            result.warnings
        );
    }

    #[test]
    fn the_projects_own_classes_win_over_a_dependency() {
        let tree = Tree::new("project-wins");
        tree.write("classes/org/Shared.class", b"from the project");
        let dep = tree.jar("dep.jar", &[("org/Shared.class", b"from the jar" as &[u8])]);

        let out = tree.root.join("fat.jar");
        write_fat_jar(
            &tree.root.join("classes"),
            &[dep],
            &out,
            &JarManifest {
                main_class: Some("Main".into()),
                class_path: vec![],
            },
        )
        .unwrap();
        assert_eq!(read_jar(&out)["org/Shared.class"], b"from the project");
    }

    #[test]
    fn class_path_entries_survive_a_space_in_the_cache_path() {
        assert_eq!(
            class_path_entry(Path::new("/cache/com/google/guava/guava-33.0.0-jre.jar")),
            "file:/cache/com/google/guava/guava-33.0.0-jre.jar"
        );
        let spaced = class_path_entry(Path::new("/Users/Ada Lovelace/Caches/jrs/lib.jar"));
        assert_eq!(spaced, "file:/Users/Ada%20Lovelace/Caches/jrs/lib.jar");
        assert!(
            !spaced.contains(' '),
            "a space would split one entry into two"
        );
    }

    #[test]
    fn a_thin_jars_class_path_is_one_entry_per_dependency() {
        let tree = Tree::new("class-path");
        tree.write("classes/Main.class", b"main");
        let out = tree.root.join("app.jar");
        write_thin_jar(
            &tree.root.join("classes"),
            &out,
            &JarManifest {
                main_class: Some("Main".into()),
                class_path: vec![
                    class_path_entry(Path::new("/cache/a b/one.jar")),
                    class_path_entry(Path::new("/cache/two.jar")),
                ],
            },
        )
        .unwrap();

        let manifest = String::from_utf8(read_jar(&out)["META-INF/MANIFEST.MF"].clone()).unwrap();
        let folded: String = manifest
            .lines()
            .skip_while(|l| !l.starts_with("Class-Path:"))
            .take_while(|l| l.starts_with("Class-Path:") || l.starts_with(' '))
            .map(|l| l.strip_prefix(' ').unwrap_or(l))
            .collect();
        assert_eq!(
            folded,
            "Class-Path: file:/cache/a%20b/one.jar file:/cache/two.jar"
        );
    }

    #[test]
    fn the_portable_layout_copies_dependencies_beside_the_jar() {
        let tree = Tree::new("portable");
        let a = tree.write("cache/org/one/core/1.0/core-1.0.jar", b"one");
        let b = tree.write("cache/org/two/core/1.0/core-1.0.jar", b"two");
        let c = tree.write(
            "cache/org/x/lib with space/2.0/lib with space-2.0.jar",
            b"x",
        );
        let lib = tree.root.join("target/lib");
        tree.write("target/lib/stale.jar", b"from an earlier build");

        let entries = copy_libraries(
            &[
                ("org.one".into(), a),
                ("org.two".into(), b),
                ("org.x".into(), c),
            ],
            &lib,
            "lib",
        )
        .unwrap();
        assert_eq!(
            entries,
            vec![
                "lib/org.one.core-1.0.jar",
                "lib/org.two.core-1.0.jar",
                "lib/lib%20with%20space-2.0.jar",
            ]
        );
        assert_eq!(
            std::fs::read(lib.join("org.two.core-1.0.jar")).unwrap(),
            b"two"
        );
        assert!(lib.join("lib with space-2.0.jar").is_file());
        assert!(
            !lib.join("stale.jar").exists(),
            "a dropped dependency lingered"
        );
    }

    #[test]
    fn a_fat_jar_without_a_main_class_is_refused() {
        let tree = Tree::new("fat-no-main");
        std::fs::create_dir_all(tree.root.join("classes")).unwrap();
        let err = write_fat_jar(
            &tree.root.join("classes"),
            &[],
            &tree.root.join("fat.jar"),
            &JarManifest::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("main class"), "{err}");
        assert!(err.contains("main-class"), "{err}");
    }
}
