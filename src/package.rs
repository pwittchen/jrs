//! Building jars.
//!
//! Output is deterministic: entries sorted, timestamps fixed, permissions fixed,
//! so two builds of the same inputs produce byte-identical jars (SPEC §9.1).
//!
//! The fat jar's merge rules are where the real care goes. `META-INF/services/*`
//! entries are concatenated rather than overwritten — getting that wrong breaks
//! `ServiceLoader` silently, which is the worst kind of packaging bug. Groovy
//! extension-module descriptors get the same care for the same reason: every one
//! on the classpath, at either location, is merged into a single descriptor whose
//! class lists are the union of them all (SPEC §9.2's rule for Groovy extension
//! modules).
//!
//! Spring keeps its own registries in the same shape, and loses them the same
//! silent way (SPEC §9.2): `META-INF/spring.factories` is merged key by key,
//! each key's comma-separated values the union of every copy;
//! `META-INF/spring/*.imports` files, one class a line, are the union of their
//! lines; `spring.handlers`, `spring.schemas` and `spring.tooling` are
//! concatenated like service files, which is Maven Shade's
//! `AppendingTransformer`. In every merge the project's own copy comes first,
//! then each jar's in classpath order.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};

use zip::write::SimpleFileOptions;

use crate::error::{IoResultExt, JrsError, Result};
use crate::project;
use crate::relocate::Relocator;

/// The epoch every entry is stamped with, so repeated builds match byte for byte.
/// 1980-01-01 is the earliest a zip timestamp can express.
pub(crate) fn fixed_timestamp() -> zip::DateTime {
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
    /// `[package.manifest]`, expanded: written after jrs's own attributes,
    /// in this order.
    pub attributes: Vec<(String, String)>,
}

impl JarManifest {
    /// Render `META-INF/MANIFEST.MF`, wrapped the way the jar spec requires.
    #[must_use]
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
        for (name, value) in &self.attributes {
            s.push_str(&wrap_header(name, value));
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
#[must_use]
pub fn class_path_entry(path: &Path) -> String {
    format!("file:{}", url_encode(&path.to_string_lossy()))
}

/// Percent-encode everything a `Class-Path` URL cannot carry verbatim.
fn url_encode(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'/'
            | b'-'
            | b'_'
            | b'.'
            | b'~'
            | b'+'
            | b':' => out.push(byte as char),
            b'\\' if cfg!(windows) => out.push('/'),
            other => {
                let _ = write!(out, "%{other:02X}");
            }
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
///
/// # Errors
///
/// `JrsError::Io` if `lib_dir` cannot be emptied or created, or a jar cannot be
/// copied into it.
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
///
/// # Errors
///
/// `JrsError::Io` if `classes_dir` cannot be walked or read, or the jar cannot
/// be written; `JrsError::Build` if the zip writer rejects an entry.
pub fn write_thin_jar(
    classes_dir: &Path,
    output: &Path,
    manifest: &JarManifest,
) -> Result<PackageOutcome> {
    let mut plan: BTreeMap<String, Entry> = BTreeMap::new();
    let unrelocated = Relocator::default();
    collect_directory(classes_dir, &mut plan, &unrelocated)?;
    write_jar(
        output,
        manifest,
        &plan,
        no_archives(),
        Vec::new(),
        &unrelocated,
    )
}

/// A fat jar: this project's classes plus every runtime dependency, unpacked.
///
/// Dependencies are read straight into the output rather than through a staging
/// directory — the resulting jar is identical, and a build that packages 40 MB of
/// dependencies should not write 40 MB to disk twice.
///
/// Groovy extension-module descriptors, the project's own first and then each
/// jar's in classpath order, are merged into one at
/// `META-INF/groovy/org.codehaus.groovy.runtime.ExtensionModule`, so no module's
/// extension methods are lost to first-wins (SPEC §9.2).
///
/// `relocator` moves the packages `[package.relocate]` names (SPEC §9.9): an
/// entry's name is relocated as it is planned, so a relocated class that
/// collides with another is reported like any duplicate, and every class's
/// constant pool and every registry's class names are relocated as they are
/// written. The main class moves with its package.
///
/// # Errors
///
/// `JrsError::Manifest` if `manifest` has no main class; `JrsError::Build` if
/// a dependency is not a readable jar, a class cannot be relocated or the zip
/// writer fails; `JrsError::Io` if a file cannot be read or the jar cannot be
/// written.
pub fn write_fat_jar(
    classes_dir: &Path,
    dependency_jars: &[PathBuf],
    relocator: &Relocator,
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
    collect_directory(classes_dir, &mut plan, relocator)?;

    // Its extension-module descriptors are not copied but merged, ahead of any
    // dependency's. Both names sort in this order, as the walk found them.
    let mut module_files = Vec::new();
    for name in [EXTENSION_MODULE, LEGACY_EXTENSION_MODULE] {
        if let Some(Entry::File(path)) = plan.remove(name) {
            module_files.push(path);
        }
    }
    let mut module_entries = Vec::new();

    // So are its service files and Spring registries: they head the merge,
    // rather than being replaced by the first dependency that has one.
    let owned: Vec<String> = plan
        .keys()
        .filter(|name| merge_kind(name).is_some())
        .cloned()
        .collect();
    for name in owned {
        if let (Some(kind), Some(Entry::File(path))) = (merge_kind(&name), plan.remove(&name)) {
            plan.insert(name, Entry::Merged(kind, vec![Source::File(path)]));
        }
    }

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
            // Checked before the services rule: the legacy descriptor lives under
            // `META-INF/services/` but must be merged, not concatenated.
            if is_extension_module(&name) {
                module_entries.push((index, i));
                continue;
            }
            let name = relocator.entry_name(&name);
            if let Some(kind) = merge_kind(&name) {
                match plan.get_mut(&name) {
                    Some(Entry::Merged(_, sources)) => sources.push(Source::Jar(index, i)),
                    _ => {
                        plan.insert(name, Entry::Merged(kind, vec![Source::Jar(index, i)]));
                    }
                }
                continue;
            }
            match plan.get(&name) {
                #[allow(
                    clippy::case_sensitive_file_extension_comparisons,
                    reason = "jar entry names are case-sensitive; `Foo.CLASS` is not a class"
                )]
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

    // Even a single descriptor goes through the merge, so the output is always
    // normalised and always at the canonical location.
    if !module_files.is_empty() || !module_entries.is_empty() {
        plan.insert(
            EXTENSION_MODULE.to_string(),
            Entry::ExtensionModules(module_files, module_entries),
        );
    }

    let manifest = JarManifest {
        main_class: manifest
            .main_class
            .as_deref()
            .map(|main| relocator.class_name(main)),
        ..manifest.clone()
    };
    write_jar(output, &manifest, &plan, archives, warnings, relocator)
}

/// A sources jar: each of `files` at its path relative to the root that holds
/// it — the deepest of `roots` it lies under, so a nested root still gives
/// package-relative names. Files under no root are left out; where two roots
/// hold the same relative path, the one found first wins.
///
/// The manifest carries only `Manifest-Version` and `Created-By`: a sources
/// jar has no main class, and `[package.manifest]` describes the program.
///
/// # Errors
///
/// `JrsError::Io` if a file cannot be read or the jar cannot be written;
/// `JrsError::Build` if the zip writer rejects an entry.
pub fn write_sources_jar(
    roots: &[PathBuf],
    files: &[PathBuf],
    output: &Path,
) -> Result<PackageOutcome> {
    let mut plan: BTreeMap<String, Entry> = BTreeMap::new();
    for file in files {
        let Some(root) = roots
            .iter()
            .filter(|r| file.starts_with(r))
            .max_by_key(|r| r.components().count())
        else {
            continue;
        };
        let name = project::slash_path(file.strip_prefix(root).unwrap_or(file));
        plan.entry(name)
            .or_insert_with(|| Entry::File(file.clone()));
    }
    write_jar(
        output,
        &JarManifest::default(),
        &plan,
        no_archives(),
        Vec::new(),
        &Relocator::default(),
    )
}

/// A Javadoc jar: everything under `doc_dir`, as `jrs doc` left it.
///
/// # Errors
///
/// As for [`write_thin_jar`].
pub fn write_javadoc_jar(doc_dir: &Path, output: &Path) -> Result<PackageOutcome> {
    write_thin_jar(doc_dir, output, &JarManifest::default())
}

/// Where an entry's bytes come from.
enum Entry {
    File(PathBuf),
    Jar(usize, usize),
    /// A resource several jars have, combined per [`Merge`]: the project's
    /// copy first, then jar entries in classpath order.
    Merged(Merge, Vec<Source>),
    /// Groovy extension-module descriptors, merged into one: the project's own
    /// files first, then jar entries in classpath order.
    ExtensionModules(Vec<PathBuf>, Vec<(usize, usize)>),
}

impl Entry {
    fn describe(&self, classes_dir: &Path, jars: &[PathBuf]) -> String {
        let jar = |index: &usize| {
            jars.get(*index)
                .map_or_else(|| "an earlier jar".to_string(), |p| p.display().to_string())
        };
        match self {
            Entry::File(_) => classes_dir.display().to_string(),
            Entry::Jar(index, _) => jar(index),
            Entry::Merged(_, sources) => match sources.first() {
                Some(Source::File(_)) => classes_dir.display().to_string(),
                Some(Source::Jar(index, _)) => jar(index),
                None => "an earlier jar".to_string(),
            },
            Entry::ExtensionModules(files, _) if !files.is_empty() => {
                classes_dir.display().to_string()
            }
            Entry::ExtensionModules(_, sources) => sources
                .first()
                .map_or_else(|| "an earlier jar".to_string(), |(index, _)| jar(index)),
        }
    }
}

/// Every file under `dir`, by its jar entry name — relocated by `relocator`,
/// except a Groovy extension-module descriptor, which Groovy finds by path.
fn collect_directory(
    dir: &Path,
    plan: &mut BTreeMap<String, Entry>,
    relocator: &Relocator,
) -> Result<()> {
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
        let name = if is_extension_module(&name) {
            name
        } else {
            relocator.entry_name(&name)
        };
        plan.insert(name, Entry::File(path));
    }
    Ok(())
}

/// Signature files and module descriptors no longer describe the merged jar
/// (SPEC §9.2): it is neither signed nor any one of the dependencies' modules.
/// A descriptor may sit at the root or, in a multi-release jar, under
/// `META-INF/versions/<n>/`, as `kotlin-stdlib`'s does.
#[allow(
    clippy::case_sensitive_file_extension_comparisons,
    reason = "compares an upper-cased copy, so the match is already case-insensitive"
)]
fn is_dropped(name: &str) -> bool {
    if name == "META-INF/MANIFEST.MF" || name == "META-INF/INDEX.LIST" {
        return true;
    }
    if name == "module-info.class"
        || name
            .strip_prefix("META-INF/versions/")
            .and_then(|rest| rest.split_once('/'))
            .is_some_and(|(version, file)| {
                file == "module-info.class" && version.chars().all(|c| c.is_ascii_digit())
            })
    {
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

/// A class file, by its entry name: what relocation rewrites.
#[allow(
    clippy::case_sensitive_file_extension_comparisons,
    reason = "jar entry names are case-sensitive; `Foo.CLASS` is not a class"
)]
fn is_class(name: &str) -> bool {
    name.ends_with(".class")
}

fn is_service_file(name: &str) -> bool {
    name.starts_with("META-INF/services/") && name.len() > "META-INF/services/".len()
}

/// One input to a merged entry.
enum Source {
    /// The project's own copy, under the classes directory.
    File(PathBuf),
    /// Entry `.1` of dependency jar `.0`.
    Jar(usize, usize),
}

/// How a fat jar combines a resource that more than one jar has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Merge {
    /// Concatenated, a newline between files: `META-INF/services/*`, and
    /// Spring's `spring.handlers`, `spring.schemas` and `spring.tooling` —
    /// Maven Shade's `AppendingTransformer`. A key two copies share is then
    /// read the way the unpacked classpath reads it: the later one wins.
    Append,
    /// `META-INF/spring.factories`: each key once, first seen first, with the
    /// union of every copy's comma-separated values.
    SpringFactories,
    /// `META-INF/spring/*.imports`: the union of the files' lines, first seen
    /// first, comments and blank lines dropped.
    Lines,
}

/// The merge rule for `name`, when it has one. Groovy's extension-module
/// descriptors are not here: they are merged by [`ExtensionModule`], and are
/// checked for first.
#[allow(
    clippy::case_sensitive_file_extension_comparisons,
    reason = "jar entry names are case-sensitive, and Spring looks for `.imports` exactly"
)]
fn merge_kind(name: &str) -> Option<Merge> {
    match name {
        "META-INF/spring.factories" => Some(Merge::SpringFactories),
        "META-INF/spring.handlers" | "META-INF/spring.schemas" | "META-INF/spring.tooling" => {
            Some(Merge::Append)
        }
        _ if is_service_file(name) => Some(Merge::Append),
        _ => name
            .strip_prefix("META-INF/spring/")
            .filter(|file| !file.contains('/') && file.len() > ".imports".len())
            .filter(|file| file.ends_with(".imports"))
            .map(|_| Merge::Lines),
    }
}

/// Combine the copies of one resource, in order, per `kind`.
fn merge(kind: Merge, copies: &[Vec<u8>]) -> Vec<u8> {
    match kind {
        Merge::Append => {
            // A newline between files, so no line is glued onto the tail of
            // the previous file's last one.
            let mut merged = Vec::new();
            for bytes in copies {
                if !merged.is_empty() && !merged.ends_with(b"\n") {
                    merged.push(b'\n');
                }
                merged.extend_from_slice(bytes);
            }
            merged
        }
        Merge::SpringFactories => {
            let mut keys: Vec<(String, Vec<String>)> = Vec::new();
            for bytes in copies {
                for (key, value) in properties(&String::from_utf8_lossy(bytes)) {
                    let slot = keys.iter().position(|(k, _)| *k == key).unwrap_or_else(|| {
                        keys.push((key, Vec::new()));
                        keys.len() - 1
                    });
                    let values = &mut keys[slot].1;
                    for v in value.split(',').map(str::trim).filter(|v| !v.is_empty()) {
                        if !values.iter().any(|seen| seen == v) {
                            values.push(v.to_string());
                        }
                    }
                }
            }
            let mut s = String::new();
            for (key, values) in keys {
                let _ = writeln!(s, "{key}={}", values.join(","));
            }
            s.into_bytes()
        }
        Merge::Lines => {
            let mut lines: Vec<String> = Vec::new();
            for bytes in copies {
                for line in String::from_utf8_lossy(bytes).lines() {
                    // Spring strips a `#` comment wherever it starts.
                    let line = line.split('#').next().unwrap_or_default().trim();
                    if !line.is_empty() && !lines.iter().any(|seen| seen == line) {
                        lines.push(line.to_string());
                    }
                }
            }
            let mut s = String::new();
            for line in lines {
                s.push_str(&line);
                s.push('\n');
            }
            s.into_bytes()
        }
    }
}

/// The `key`/`value` pairs of a `.properties` file, read leniently: `key=value`
/// or `key: value`, `#` and `!` comments, trailing-`\` continuations. Keys and
/// values come back trimmed; a line with no separator is skipped.
fn properties(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut push = |logical: &str| {
        if let Some(split) = logical.find(['=', ':']) {
            out.push((
                logical[..split].trim().to_string(),
                logical[split + 1..].trim().to_string(),
            ));
        }
    };
    let mut pending: Option<String> = None;
    for raw in text.lines() {
        let line = raw.trim_start();
        let mut logical = match pending.take() {
            Some(logical) => logical,
            None if line.is_empty() || line.starts_with('#') || line.starts_with('!') => {
                continue;
            }
            None => String::new(),
        };
        // An odd run of trailing backslashes continues the line; an even run
        // is escaped backslashes.
        let backslashes = line.len() - line.trim_end_matches('\\').len();
        if backslashes % 2 == 1 {
            logical.push_str(&line[..line.len() - 1]);
            pending = Some(logical);
        } else {
            logical.push_str(line);
            push(&logical);
        }
    }
    if let Some(logical) = pending {
        push(&logical);
    }
    out
}

/// Where Groovy 2.5+ looks for an extension-module descriptor, and where the
/// merged one is written.
const EXTENSION_MODULE: &str = "META-INF/groovy/org.codehaus.groovy.runtime.ExtensionModule";

/// Where older Groovy modules put theirs. Read, never written.
const LEGACY_EXTENSION_MODULE: &str =
    "META-INF/services/org.codehaus.groovy.runtime.ExtensionModule";

fn is_extension_module(name: &str) -> bool {
    name == EXTENSION_MODULE || name == LEGACY_EXTENSION_MODULE
}

/// The union of several Groovy extension-module descriptors.
///
/// Only the two class lists survive a merge; `moduleName` and `moduleVersion`
/// describe one module, and the merged descriptor is no one module.
#[derive(Debug, Default)]
struct ExtensionModule {
    extension_classes: Vec<String>,
    static_extension_classes: Vec<String>,
}

impl ExtensionModule {
    /// Fold one descriptor in. Parsing is lenient — `key=value` or `key: value`,
    /// `#` and `!` comments, trailing-`\` continuations — and classes already
    /// seen are skipped, so the lists keep first-seen order without duplicates.
    fn add(&mut self, text: &str) {
        for (key, value) in properties(text) {
            let list = match key.as_str() {
                "extensionClasses" => &mut self.extension_classes,
                "staticExtensionClasses" => &mut self.static_extension_classes,
                _ => continue,
            };
            for class in value.split(',').map(str::trim) {
                if !class.is_empty() && !list.iter().any(|seen| seen == class) {
                    list.push(class.to_string());
                }
            }
        }
    }

    fn render(&self) -> String {
        let mut s = String::from("moduleName=merged-by-jrs\nmoduleVersion=1.0\n");
        if !self.extension_classes.is_empty() {
            let _ = writeln!(s, "extensionClasses={}", self.extension_classes.join(","));
        }
        if !self.static_extension_classes.is_empty() {
            let _ = writeln!(
                s,
                "staticExtensionClasses={}",
                self.static_extension_classes.join(",")
            );
        }
        s
    }
}

/// The empty archive list a thin jar needs, with the type parameter pinned.
fn no_archives() -> Vec<zip::ZipArchive<std::fs::File>> {
    Vec::new()
}

fn write_jar<R: Read + Seek>(
    output: &Path,
    manifest: &JarManifest,
    plan: &BTreeMap<String, Entry>,
    mut archives: Vec<zip::ZipArchive<R>>,
    warnings: Vec<String>,
    relocator: &Relocator,
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

    for (name, entry) in plan {
        writer
            .start_file(name.as_str(), options)
            .map_err(|e| JrsError::build(format!("{}: {e}", output.display())))?;
        match entry {
            Entry::File(path) => {
                let mut bytes = std::fs::read(path).path(path)?;
                if is_class(name) {
                    bytes = relocator.class_file(bytes).map_err(|why| {
                        JrsError::build(format!("{}: cannot relocate: {why}", path.display()))
                    })?;
                }
                writer.write_all(&bytes).path(output)?;
            }
            Entry::Jar(archive, index) => {
                let mut source = archives[*archive]
                    .by_index(*index)
                    .map_err(|e| JrsError::build(format!("{}: {e}", output.display())))?;
                if relocator.is_empty() || !is_class(name) {
                    std::io::copy(&mut source, &mut writer).path(output)?;
                } else {
                    let mut bytes = Vec::new();
                    source.read_to_end(&mut bytes).path(output)?;
                    let bytes = relocator.class_file(bytes).map_err(|why| {
                        JrsError::build(format!("{name}: cannot relocate: {why}"))
                    })?;
                    writer.write_all(&bytes).path(output)?;
                }
            }
            Entry::Merged(kind, sources) => {
                let mut copies = Vec::with_capacity(sources.len());
                for source in sources {
                    copies.push(match source {
                        Source::File(path) => std::fs::read(path).path(path)?,
                        Source::Jar(archive, index) => {
                            let mut source = archives[*archive].by_index(*index).map_err(|e| {
                                JrsError::build(format!("{}: {e}", output.display()))
                            })?;
                            let mut bytes = Vec::new();
                            source.read_to_end(&mut bytes).path(output)?;
                            bytes
                        }
                    });
                }
                writer
                    .write_all(&relocator.text(merge(*kind, &copies)))
                    .path(output)?;
            }
            Entry::ExtensionModules(files, sources) => {
                let mut merged = ExtensionModule::default();
                for path in files {
                    let bytes = std::fs::read(path).path(path)?;
                    merged.add(&String::from_utf8_lossy(&bytes));
                }
                for (archive, index) in sources {
                    let mut source = archives[*archive]
                        .by_index(*index)
                        .map_err(|e| JrsError::build(format!("{}: {e}", output.display())))?;
                    let mut bytes = Vec::new();
                    source.read_to_end(&mut bytes).path(output)?;
                    merged.add(&String::from_utf8_lossy(&bytes));
                }
                writer
                    .write_all(&relocator.text(merged.render().into_bytes()))
                    .path(output)?;
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
                ..JarManifest::default()
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
            ..JarManifest::default()
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
            &Relocator::default(),
            &out,
            &JarManifest {
                main_class: Some("com.example.Main".into()),
                class_path: vec![],
                ..JarManifest::default()
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
            &Relocator::default(),
            &out,
            &JarManifest {
                main_class: Some("Main".into()),
                class_path: vec![],
                ..JarManifest::default()
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
            &Relocator::default(),
            &out,
            &JarManifest {
                main_class: Some("Main".into()),
                class_path: vec![],
                ..JarManifest::default()
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
    fn dependency_module_descriptors_are_dropped() {
        assert!(is_dropped("module-info.class"));
        assert!(is_dropped("META-INF/versions/9/module-info.class"));
        assert!(is_dropped("META-INF/versions/21/module-info.class"));
        // A versioned class is not a descriptor, nor is a descriptor's
        // look-alike somewhere else.
        assert!(!is_dropped("META-INF/versions/9/kotlin/Unit.class"));
        assert!(!is_dropped("com/example/module-info.class"));
        assert!(!is_dropped("META-INF/versions/nine/module-info.class"));
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
            &Relocator::default(),
            &out,
            &JarManifest {
                main_class: Some("Main".into()),
                class_path: vec![],
                ..JarManifest::default()
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
            &Relocator::default(),
            &out,
            &JarManifest {
                main_class: Some("Main".into()),
                class_path: vec![],
                ..JarManifest::default()
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
            &Relocator::default(),
            &out,
            &JarManifest {
                main_class: Some("Main".into()),
                class_path: vec![],
                ..JarManifest::default()
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
                ..JarManifest::default()
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

    fn fat_jar(tree: &Tree, jars: &[PathBuf], out: &Path) {
        write_fat_jar(
            &tree.root.join("classes"),
            jars,
            &Relocator::default(),
            out,
            &JarManifest {
                main_class: Some("Main".into()),
                class_path: vec![],
                ..JarManifest::default()
            },
        )
        .unwrap();
    }

    fn extension_module(contents: &BTreeMap<String, Vec<u8>>) -> String {
        String::from_utf8(contents[EXTENSION_MODULE].clone()).unwrap()
    }

    #[test]
    fn extension_modules_from_two_jars_merge_into_one() {
        let tree = Tree::new("groovy-two-jars");
        std::fs::create_dir_all(tree.root.join("classes")).unwrap();
        let datetime = tree.jar(
            "groovy-datetime.jar",
            &[(
                EXTENSION_MODULE,
                b"moduleName=groovy-datetime\nmoduleVersion=4.0.0\n\
                  extensionClasses=org.groovy.DateTimeExtensions\n\
                  staticExtensionClasses=org.groovy.DateTimeStaticExtensions\n"
                    as &[u8],
            )],
        );
        let sql = tree.jar(
            "groovy-sql.jar",
            &[(
                EXTENSION_MODULE,
                b"moduleName=groovy-sql\nmoduleVersion=4.0.0\n\
                  extensionClasses=org.groovy.SqlExtensions, org.groovy.SqlGroovyMethods\n\
                  staticExtensionClasses=org.groovy.SqlStaticExtensions\n"
                    as &[u8],
            )],
        );

        let out = tree.root.join("fat.jar");
        fat_jar(&tree, &[datetime, sql], &out);
        assert_eq!(
            extension_module(&read_jar(&out)),
            "moduleName=merged-by-jrs\nmoduleVersion=1.0\n\
             extensionClasses=org.groovy.DateTimeExtensions,org.groovy.SqlExtensions,\
             org.groovy.SqlGroovyMethods\n\
             staticExtensionClasses=org.groovy.DateTimeStaticExtensions,\
             org.groovy.SqlStaticExtensions\n"
        );
    }

    #[test]
    fn a_legacy_extension_module_is_merged_not_concatenated() {
        let tree = Tree::new("groovy-legacy");
        std::fs::create_dir_all(tree.root.join("classes")).unwrap();
        let old = tree.jar(
            "old.jar",
            &[
                (
                    LEGACY_EXTENSION_MODULE,
                    b"moduleName=old\nmoduleVersion=1.0\nextensionClasses=com.old.Ext\n" as &[u8],
                ),
                ("META-INF/services/java.sql.Driver", b"com.old.Driver\n"),
            ],
        );
        let new = tree.jar(
            "new.jar",
            &[
                (
                    EXTENSION_MODULE,
                    b"moduleName=new\nmoduleVersion=2.0\nextensionClasses=com.new.Ext\n" as &[u8],
                ),
                ("META-INF/services/java.sql.Driver", b"com.new.Driver\n"),
            ],
        );

        let out = tree.root.join("fat.jar");
        fat_jar(&tree, &[old, new], &out);
        let contents = read_jar(&out);
        assert_eq!(
            extension_module(&contents),
            "moduleName=merged-by-jrs\nmoduleVersion=1.0\n\
             extensionClasses=com.old.Ext,com.new.Ext\n"
        );
        assert!(
            !contents.contains_key(LEGACY_EXTENSION_MODULE),
            "the legacy location must not be written"
        );
        assert_eq!(
            contents["META-INF/services/java.sql.Driver"],
            b"com.old.Driver\ncom.new.Driver\n"
        );
    }

    #[test]
    fn the_projects_own_extension_module_comes_first_without_duplicates() {
        let tree = Tree::new("groovy-project");
        tree.write("classes/Main.class", b"main");
        tree.write(
            &format!("classes/{EXTENSION_MODULE}"),
            b"moduleName=app\nmoduleVersion=0.1\nextensionClasses=com.app.Ext,com.lib.Ext\n",
        );
        let lib = tree.jar(
            "lib.jar",
            &[(
                EXTENSION_MODULE,
                b"moduleName=lib\nmoduleVersion=1.0\nextensionClasses=com.lib.Ext,com.app.Ext\n\
                  staticExtensionClasses=com.lib.Static\n" as &[u8],
            )],
        );

        let out = tree.root.join("fat.jar");
        fat_jar(&tree, &[lib], &out);
        assert_eq!(
            extension_module(&read_jar(&out)),
            "moduleName=merged-by-jrs\nmoduleVersion=1.0\n\
             extensionClasses=com.app.Ext,com.lib.Ext\n\
             staticExtensionClasses=com.lib.Static\n"
        );
    }

    #[test]
    fn a_merged_fat_jar_is_byte_identical_across_builds() {
        let tree = Tree::new("groovy-deterministic");
        tree.write(
            &format!("classes/{LEGACY_EXTENSION_MODULE}"),
            b"extensionClasses=com.app.Ext\n",
        );
        let first = tree.jar(
            "first.jar",
            &[(
                EXTENSION_MODULE,
                b"extensionClasses=com.first.Ext\n" as &[u8],
            )],
        );
        let second = tree.jar(
            "second.jar",
            &[(
                LEGACY_EXTENSION_MODULE,
                b"staticExtensionClasses=com.second.Static\n" as &[u8],
            )],
        );

        let one = tree.root.join("one.jar");
        let two = tree.root.join("two.jar");
        for out in [&one, &two] {
            fat_jar(&tree, &[first.clone(), second.clone()], out);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(std::fs::read(&one).unwrap(), std::fs::read(&two).unwrap());
    }

    #[test]
    fn extension_module_parsing_is_lenient() {
        let mut merged = ExtensionModule::default();
        merged.add(
            "# a comment\n! another\n\n  moduleName = spaced\r\n\
             extensionClasses : com.a.One, \\\n    com.a.Two,\\\n  com.a.Three\n\
             staticExtensionClasses=\n",
        );
        assert_eq!(
            merged.render(),
            "moduleName=merged-by-jrs\nmoduleVersion=1.0\n\
             extensionClasses=com.a.One,com.a.Two,com.a.Three\n"
        );
    }

    fn text(contents: &BTreeMap<String, Vec<u8>>, name: &str) -> String {
        String::from_utf8(contents[name].clone()).unwrap()
    }

    #[test]
    fn extra_attributes_follow_jrs_own_in_declaration_order() {
        let long = "v".repeat(200);
        let manifest = JarManifest {
            main_class: Some("com.example.Main".into()),
            class_path: vec!["lib/a.jar".into()],
            attributes: vec![
                ("Implementation-Version".into(), "1.0".into()),
                ("Automatic-Module-Name".into(), "com.example".into()),
                ("X-Long".into(), long.clone()),
            ],
        };
        let rendered = manifest.render();
        let names: Vec<&str> = rendered
            .lines()
            .filter(|l| !l.is_empty() && !l.starts_with(' '))
            .map(|l| l.split(':').next().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "Manifest-Version",
                "Created-By",
                "Main-Class",
                "Class-Path",
                "Implementation-Version",
                "Automatic-Module-Name",
                "X-Long"
            ]
        );
        for line in rendered.lines() {
            assert!(line.len() <= 72, "line too long ({}): {line}", line.len());
        }
        let folded: String = rendered
            .lines()
            .skip_while(|l| !l.starts_with("X-Long:"))
            .take_while(|l| l.starts_with("X-Long:") || l.starts_with(' '))
            .map(|l| l.strip_prefix(' ').unwrap_or(l))
            .collect();
        assert_eq!(folded, format!("X-Long: {long}"));
        assert!(
            rendered.ends_with("\n\n"),
            "a manifest ends with a blank line"
        );
    }

    #[test]
    fn the_projects_own_service_file_is_merged_not_replaced() {
        let tree = Tree::new("own-services");
        tree.write("classes/META-INF/services/x.Y", b"com.app.Impl\n");
        let dep = tree.jar(
            "dep.jar",
            &[("META-INF/services/x.Y", b"com.dep.Impl\n" as &[u8])],
        );
        let out = tree.root.join("fat.jar");
        fat_jar(&tree, &[dep], &out);
        assert_eq!(
            text(&read_jar(&out), "META-INF/services/x.Y"),
            "com.app.Impl\ncom.dep.Impl\n"
        );
    }

    #[test]
    fn a_service_file_only_the_project_has_is_kept_as_it_is() {
        let tree = Tree::new("own-services-alone");
        tree.write("classes/META-INF/services/x.Y", b"com.app.Impl");
        let out = tree.root.join("fat.jar");
        fat_jar(&tree, &[], &out);
        assert_eq!(
            text(&read_jar(&out), "META-INF/services/x.Y"),
            "com.app.Impl"
        );
    }

    #[test]
    fn spring_factories_merge_key_by_key() {
        let tree = Tree::new("spring-factories");
        tree.write(
            "classes/META-INF/spring.factories",
            b"org.springframework.context.ApplicationListener=com.app.Listener\n",
        );
        let boot = tree.jar(
            "boot.jar",
            &[(
                "META-INF/spring.factories",
                b"# Auto Configure\n\
                  org.springframework.boot.autoconfigure.EnableAutoConfiguration=\\\n  \
                  com.boot.A,\\\n  com.boot.B\n\
                  org.springframework.context.ApplicationListener=com.boot.Listener\n"
                    as &[u8],
            )],
        );
        let other = tree.jar(
            "other.jar",
            &[(
                "META-INF/spring.factories",
                b"org.springframework.boot.autoconfigure.EnableAutoConfiguration=\
                  com.other.C, com.boot.A" as &[u8],
            )],
        );
        let out = tree.root.join("fat.jar");
        fat_jar(&tree, &[boot, other], &out);
        assert_eq!(
            text(&read_jar(&out), "META-INF/spring.factories"),
            "org.springframework.context.ApplicationListener=com.app.Listener,com.boot.Listener\n\
             org.springframework.boot.autoconfigure.EnableAutoConfiguration=\
             com.boot.A,com.boot.B,com.other.C\n"
        );
    }

    #[test]
    fn spring_imports_files_are_the_union_of_their_lines() {
        let name =
            "META-INF/spring/org.springframework.boot.autoconfigure.AutoConfiguration.imports";
        let tree = Tree::new("spring-imports");
        std::fs::create_dir_all(tree.root.join("classes")).unwrap();
        let boot = tree.jar(
            "boot.jar",
            &[(name, b"# Boot's own\ncom.boot.A\ncom.boot.B\n\n" as &[u8])],
        );
        let other = tree.jar(
            "other.jar",
            &[(name, b"com.other.C\r\ncom.boot.A # again" as &[u8])],
        );
        let out = tree.root.join("fat.jar");
        fat_jar(&tree, &[boot, other], &out);
        assert_eq!(
            text(&read_jar(&out), name),
            "com.boot.A\ncom.boot.B\ncom.other.C\n"
        );
    }

    #[test]
    fn spring_handlers_and_schemas_are_concatenated() {
        let tree = Tree::new("spring-handlers");
        std::fs::create_dir_all(tree.root.join("classes")).unwrap();
        let beans = tree.jar(
            "beans.jar",
            &[
                (
                    "META-INF/spring.handlers",
                    b"http\\://www.springframework.org/schema/c=org.C" as &[u8],
                ),
                (
                    "META-INF/spring.schemas",
                    b"http\\://www.springframework.org/schema/beans.xsd=beans.xsd\n",
                ),
            ],
        );
        let context = tree.jar(
            "context.jar",
            &[
                (
                    "META-INF/spring.handlers",
                    b"http\\://www.springframework.org/schema/context=org.Ctx\n" as &[u8],
                ),
                (
                    "META-INF/spring.schemas",
                    b"http\\://www.springframework.org/schema/context.xsd=context.xsd\n",
                ),
            ],
        );
        let out = tree.root.join("fat.jar");
        fat_jar(&tree, &[beans, context], &out);
        let contents = read_jar(&out);
        assert_eq!(
            text(&contents, "META-INF/spring.handlers"),
            "http\\://www.springframework.org/schema/c=org.C\n\
             http\\://www.springframework.org/schema/context=org.Ctx\n"
        );
        assert_eq!(
            text(&contents, "META-INF/spring.schemas"),
            "http\\://www.springframework.org/schema/beans.xsd=beans.xsd\n\
             http\\://www.springframework.org/schema/context.xsd=context.xsd\n"
        );
    }

    #[test]
    fn merge_rules_are_chosen_by_exact_name() {
        for (name, kind) in [
            ("META-INF/services/java.sql.Driver", Some(Merge::Append)),
            ("META-INF/spring.factories", Some(Merge::SpringFactories)),
            ("META-INF/spring.handlers", Some(Merge::Append)),
            ("META-INF/spring.schemas", Some(Merge::Append)),
            ("META-INF/spring.tooling", Some(Merge::Append)),
            (
                "META-INF/spring/a.b.AutoConfiguration.imports",
                Some(Merge::Lines),
            ),
            ("META-INF/spring/nested/a.imports", None),
            ("META-INF/spring/.imports", None),
            ("META-INF/spring/a.IMPORTS", None),
            ("META-INF/spring.factories.bak", None),
            ("spring.factories", None),
            ("META-INF/services/", None),
        ] {
            assert_eq!(merge_kind(name), kind, "{name}");
        }
    }

    #[test]
    fn a_spring_fat_jar_is_byte_identical_across_builds() {
        let tree = Tree::new("spring-deterministic");
        tree.write("classes/META-INF/spring.factories", b"k=com.app.A\n");
        let dep = tree.jar(
            "dep.jar",
            &[
                (
                    "META-INF/spring.factories",
                    b"k=com.dep.B\nj=com.dep.C\n" as &[u8],
                ),
                ("META-INF/spring/x.imports", b"com.dep.D\n"),
            ],
        );
        let one = tree.root.join("one.jar");
        let two = tree.root.join("two.jar");
        for out in [&one, &two] {
            fat_jar(&tree, std::slice::from_ref(&dep), out);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(std::fs::read(&one).unwrap(), std::fs::read(&two).unwrap());
    }

    #[test]
    fn a_sources_jar_names_files_by_their_root() {
        let tree = Tree::new("sources");
        let java = tree.root.join("src/main/java");
        let kotlin = tree.root.join("src/main/kotlin");
        let generated = tree.root.join("target/generated/sources");
        let files = vec![
            tree.write("src/main/java/com/example/App.java", b"app"),
            tree.write("src/main/kotlin/com/example/Util.kt", b"util"),
            tree.write(
                "target/generated/sources/com/example/BuildInfo.java",
                b"info",
            ),
            // Under no root: not a source of this jar.
            tree.write("elsewhere/Stray.java", b"stray"),
        ];
        let roots = vec![java, kotlin, generated];

        let first = tree.root.join("first-sources.jar");
        let second = tree.root.join("second-sources.jar");
        let outcome = write_sources_jar(&roots, &files, &first).unwrap();
        assert_eq!(outcome.entries, 4);
        assert_eq!(
            entry_order(&first),
            [
                "META-INF/MANIFEST.MF",
                "com/example/App.java",
                "com/example/BuildInfo.java",
                "com/example/Util.kt"
            ]
        );
        let contents = read_jar(&first);
        assert_eq!(contents["com/example/Util.kt"], b"util");
        let manifest = text(&contents, "META-INF/MANIFEST.MF");
        assert!(!manifest.contains("Main-Class"), "{manifest}");

        std::thread::sleep(std::time::Duration::from_millis(5));
        write_sources_jar(&roots, &files, &second).unwrap();
        assert_eq!(
            std::fs::read(&first).unwrap(),
            std::fs::read(&second).unwrap()
        );
    }

    #[test]
    fn a_nested_root_gives_package_relative_names() {
        let tree = Tree::new("sources-nested");
        let outer = tree.root.join("src");
        let inner = tree.root.join("src/main/kotlin");
        let file = tree.write("src/main/kotlin/com/example/Util.kt", b"util");
        let out = tree.root.join("sources.jar");
        write_sources_jar(&[outer, inner], &[file], &out).unwrap();
        assert!(read_jar(&out).contains_key("com/example/Util.kt"));
    }

    #[test]
    fn a_javadoc_jar_holds_the_doc_tree() {
        let tree = Tree::new("javadoc");
        tree.write("doc/index.html", b"<html>");
        tree.write("doc/com/example/App.html", b"<html>app");
        let out = tree.root.join("javadoc.jar");
        write_javadoc_jar(&tree.root.join("doc"), &out).unwrap();
        assert_eq!(
            entry_order(&out),
            ["META-INF/MANIFEST.MF", "com/example/App.html", "index.html"]
        );
    }

    /// A class file by hand, its constant pool holding `names`: enough for
    /// relocation, which reads and rewrites nothing else.
    fn class_file(names: &[&str]) -> Vec<u8> {
        let mut bytes = vec![0xCA, 0xFE, 0xBA, 0xBE, 0, 0, 0, 61];
        bytes.extend_from_slice(&u16::try_from(names.len() + 1).unwrap().to_be_bytes());
        for name in names {
            bytes.push(1);
            bytes.extend_from_slice(&u16::try_from(name.len()).unwrap().to_be_bytes());
            bytes.extend_from_slice(name.as_bytes());
        }
        bytes.extend_from_slice(b"the rest of the class");
        bytes
    }

    fn relocating(from: &str, to: &str) -> Relocator {
        Relocator::new(&[crate::manifest::Relocation {
            from: from.into(),
            to: to.into(),
            exclude: Vec::new(),
        }])
    }

    fn relocated_fat_jar(tree: &Tree, jars: &[PathBuf], out: &Path, main: &str) {
        write_fat_jar(
            &tree.root.join("classes"),
            jars,
            &relocating("org.dep", "com.example.shaded"),
            out,
            &JarManifest {
                main_class: Some(main.into()),
                ..JarManifest::default()
            },
        )
        .unwrap();
    }

    #[test]
    fn a_relocated_fat_jar_moves_the_package_and_rewrites_its_references() {
        let tree = Tree::new("relocate");
        tree.write(
            "classes/com/example/Main.class",
            &class_file(&["com/example/Main", "(Lorg/dep/Thing;)V"]),
        );
        tree.write(
            "classes/META-INF/services/org.dep.Spi",
            b"com.example.MySpi\n",
        );
        let dep = tree.jar(
            "dep.jar",
            &[
                (
                    "org/dep/Thing.class",
                    &class_file(&["org/dep/Thing", "org.dep.Thing"]) as &[u8],
                ),
                (
                    "META-INF/versions/11/org/dep/Thing.class",
                    &class_file(&["org/dep/Thing"]),
                ),
                ("org/dep/messages.properties", b"k=v"),
                ("META-INF/services/org.dep.Spi", b"org.dep.DefaultSpi\n"),
                ("META-INF/services/java.lang.Runnable", b"org.dep.Task\n"),
            ],
        );

        let out = tree.root.join("fat.jar");
        relocated_fat_jar(&tree, &[dep], &out, "com.example.Main");
        let contents = read_jar(&out);
        assert_eq!(
            contents["com/example/Main.class"],
            class_file(&["com/example/Main", "(Lcom/example/shaded/Thing;)V"])
        );
        assert_eq!(
            contents["com/example/shaded/Thing.class"],
            class_file(&["com/example/shaded/Thing", "com.example.shaded.Thing"])
        );
        assert_eq!(
            contents["META-INF/versions/11/com/example/shaded/Thing.class"],
            class_file(&["com/example/shaded/Thing"])
        );
        assert_eq!(contents["com/example/shaded/messages.properties"], b"k=v");
        // The service file follows its interface, and the project's copy still
        // heads the merge.
        assert_eq!(
            text(&contents, "META-INF/services/com.example.shaded.Spi"),
            "com.example.MySpi\ncom.example.shaded.DefaultSpi\n"
        );
        assert_eq!(
            text(&contents, "META-INF/services/java.lang.Runnable"),
            "com.example.shaded.Task\n"
        );
        assert!(
            !contents
                .keys()
                .any(|k| k.contains("org/dep") || k.contains("org.dep")),
            "{:?}",
            contents.keys()
        );
    }

    #[test]
    fn a_main_class_in_a_relocated_package_moves_with_it() {
        let tree = Tree::new("relocate-main");
        tree.write("classes/org/dep/Main.class", &class_file(&["org/dep/Main"]));
        let out = tree.root.join("fat.jar");
        relocated_fat_jar(&tree, &[], &out, "org.dep.Main");
        let contents = read_jar(&out);
        let manifest = text(&contents, "META-INF/MANIFEST.MF");
        assert!(
            manifest.contains("Main-Class: com.example.shaded.Main\n"),
            "{manifest}"
        );
        assert!(contents.contains_key("com/example/shaded/Main.class"));
    }

    #[test]
    fn a_relocated_fat_jar_is_byte_identical_across_builds() {
        let tree = Tree::new("relocate-deterministic");
        tree.write(
            "classes/com/example/Main.class",
            &class_file(&["Lorg/dep/Thing;"]),
        );
        let dep = tree.jar(
            "dep.jar",
            &[(
                "org/dep/Thing.class",
                &class_file(&["org/dep/Thing"]) as &[u8],
            )],
        );
        let one = tree.root.join("one.jar");
        let two = tree.root.join("two.jar");
        for out in [&one, &two] {
            relocated_fat_jar(&tree, std::slice::from_ref(&dep), out, "com.example.Main");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(std::fs::read(&one).unwrap(), std::fs::read(&two).unwrap());
    }

    #[test]
    fn a_class_relocation_cannot_read_fails_the_jar() {
        let tree = Tree::new("relocate-broken");
        std::fs::create_dir_all(tree.root.join("classes")).unwrap();
        let dep = tree.jar(
            "dep.jar",
            &[("org/dep/Broken.class", b"not a class" as &[u8])],
        );
        let err = write_fat_jar(
            &tree.root.join("classes"),
            &[dep],
            &relocating("org.dep", "com.example.shaded"),
            &tree.root.join("fat.jar"),
            &JarManifest {
                main_class: Some("Main".into()),
                ..JarManifest::default()
            },
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("com/example/shaded/Broken.class: cannot relocate"),
            "{err}"
        );
    }

    #[test]
    fn a_fat_jar_without_a_main_class_is_refused() {
        let tree = Tree::new("fat-no-main");
        std::fs::create_dir_all(tree.root.join("classes")).unwrap();
        let err = write_fat_jar(
            &tree.root.join("classes"),
            &[],
            &Relocator::default(),
            &tree.root.join("fat.jar"),
            &JarManifest::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("main class"), "{err}");
        assert!(err.contains("main-class"), "{err}");
    }
}
