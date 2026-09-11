//! The project model `jrs metadata` prints, like `cargo metadata`: what an
//! editor plugin or a language server needs to set a project up without
//! reading `jrs.toml` itself — the source and resource roots per language,
//! the output directories, the three classpaths, the JDK and `--release`,
//! the main class, and the tasks.
//!
//! It is versioned JSON ([`FORMAT_VERSION`]), written by [`crate::json`].
//! Every path is absolute, and every key comes out in a fixed order, so the
//! same project prints the same bytes. Building the model never compiles:
//! it needs the manifest, the resolved graph (unless `--no-deps`), and the
//! JDK when there is one.

use std::path::{Path, PathBuf};

use crate::compile::Language;
use crate::error::Result;
use crate::json::Json;
use crate::manifest::{Hook, Manifest};
use crate::project::Project;
use crate::resolve::cache::Cache;
use crate::resolve::{self, Classpath, Resolution};
use crate::task;
use crate::toolchain::Toolchain;

/// The format version, the document's first key. It goes up only when a key
/// changes meaning or goes away; new keys may appear within a version.
pub const FORMAT_VERSION: i64 = 1;

/// What the model is built from.
pub struct Inputs<'a> {
    pub manifest: &'a Manifest,
    /// The JDK the project builds with, if one was found.
    pub toolchain: Option<&'a Toolchain>,
    /// The resolved graph, jars located; `None` under `--no-deps`.
    pub resolution: Option<&'a Resolution>,
    /// Where to look for each package's `-sources.jar`.
    pub cache: Option<&'a Cache>,
}

/// The whole document.
///
/// # Errors
///
/// None in practice: the task paths it expands were checked when the
/// manifest loaded.
pub fn metadata(inputs: &Inputs<'_>) -> Result<Json> {
    let m = inputs.manifest;
    let project = Project::new(m);
    let main_generated = task::generated(m, Hook::PreCompile)?;
    let test_generated = task::generated(m, Hook::PreTest)?;

    Ok(Json::object([
        ("version", Json::Int(FORMAT_VERSION)),
        ("jrs", Json::string(env!("CARGO_PKG_VERSION"))),
        ("project", project_json(m)),
        ("jdk", Json::or_null(inputs.toolchain, jdk_json)),
        ("java", java_json(m, inputs.toolchain)),
        ("languages", languages_json(m)),
        (
            "main",
            unit_json(
                &source_roots(m, false),
                &main_generated.sources,
                &[m.resource_path()],
                &main_generated.resources,
                &project.classes_dir(),
            ),
        ),
        (
            "test",
            unit_json(
                &source_roots(m, true),
                &test_generated.sources,
                &[m.test_resource_path()],
                &test_generated.resources,
                &project.test_classes_dir(),
            ),
        ),
        ("target-dir", Json::path(&absolute(&project.target_dir()))),
        ("jar", Json::path(&absolute(&project.jar_path()))),
        (
            "classpaths",
            Json::or_null(inputs.resolution, |r| classpaths_json(r, inputs.cache)),
        ),
        ("tasks", tasks_json(m)),
    ]))
}

fn project_json(m: &Manifest) -> Json {
    Json::object([
        ("name", Json::string(&m.name)),
        ("version", Json::string(&m.version)),
        (
            "main-class",
            Json::or_null(m.main_class.as_deref(), Json::string),
        ),
        (
            "jrs-version",
            Json::or_null(m.jrs_version.as_deref(), Json::string),
        ),
        ("root", Json::path(&absolute(&m.root))),
        ("manifest", Json::path(&absolute(&m.path))),
        ("lockfile", Json::path(&absolute(&m.lock_path()))),
    ])
}

fn jdk_json(toolchain: &Toolchain) -> Json {
    Json::object([
        (
            "home",
            Json::or_null(toolchain.home.as_deref(), |h| Json::path(&absolute(h))),
        ),
        ("version", Json::Int(i64::from(toolchain.version))),
        ("javac", Json::path(&absolute(&toolchain.javac))),
        ("java", Json::path(&absolute(&toolchain.java))),
    ])
}

/// `release` is what `javac --release` gets: `java.source`, or the JDK's own
/// release when the manifest leaves it out — `null` with neither.
fn java_json(m: &Manifest, toolchain: Option<&Toolchain>) -> Json {
    let release = toolchain
        .and_then(|t| t.release(m.java.source).ok())
        .or(m.java.source);
    Json::object([
        (
            "release",
            Json::or_null(release, |r| Json::Int(i64::from(r))),
        ),
        (
            "target",
            Json::or_null(m.java.target, |t| Json::Int(i64::from(t))),
        ),
        ("encoding", Json::string(&m.java.encoding)),
        (
            "javac-args",
            Json::Array(m.java.javac_args.iter().map(Json::string).collect()),
        ),
    ])
}

/// Java, always on, then each language the manifest turns on, with the
/// version and compiler jrs pins for it.
fn languages_json(m: &Manifest) -> Json {
    let mut out = vec![Json::object([
        ("name", Json::string(Language::Java.key())),
        ("version", Json::Null),
        ("compiler", Json::Null),
    ])];
    for config in &m.languages {
        out.push(Json::object([
            ("name", Json::string(config.language.key())),
            ("version", Json::string(&config.version)),
            (
                "compiler",
                Json::or_null(config.language.compiler(&config.version), |c| {
                    Json::string(c.coord.to_string())
                }),
            ),
        ]));
    }
    Json::Array(out)
}

/// A unit's source roots with the language each one is for: the project's
/// Java root, then each turned-on language's, as `Project::roots` has them.
fn source_roots(m: &Manifest, test: bool) -> Vec<(Language, PathBuf)> {
    let mut roots = vec![(
        Language::Java,
        if test { m.test_path() } else { m.source_path() },
    )];
    for config in &m.languages {
        let dir = m.root.join(if test {
            &config.test_dir
        } else {
            &config.source_dir
        });
        if !roots.iter().any(|(_, d)| *d == dir) {
            roots.push((config.language, dir));
        }
    }
    roots
}

fn unit_json(
    sources: &[(Language, PathBuf)],
    generated_sources: &[PathBuf],
    resources: &[PathBuf],
    generated_resources: &[PathBuf],
    output: &Path,
) -> Json {
    let paths =
        |dirs: &[PathBuf]| Json::Array(dirs.iter().map(|d| Json::path(&absolute(d))).collect());
    Json::object([
        (
            "sources",
            Json::Array(
                sources
                    .iter()
                    .map(|(language, dir)| {
                        Json::object([
                            ("language", Json::string(language.key())),
                            ("dir", Json::path(&absolute(dir))),
                        ])
                    })
                    .collect(),
            ),
        ),
        ("generated-sources", paths(generated_sources)),
        ("resources", paths(resources)),
        ("generated-resources", paths(generated_resources)),
        ("output", Json::path(&absolute(output))),
    ])
}

/// The three classpaths, from the functions the build itself uses, so each
/// lists exactly the jars `javac`, `java` and the test JVM get, in their
/// order. Class directories are not in them: they are `main.output` and
/// `test.output`, and go first, as jrs puts them.
fn classpaths_json(resolution: &Resolution, cache: Option<&Cache>) -> Json {
    Json::object([
        (
            "compile",
            entries(resolution, &resolution.classpath(Classpath::Compile), cache),
        ),
        (
            "runtime",
            entries(resolution, &resolution.runtime_classpath(), cache),
        ),
        (
            "test",
            entries(resolution, &resolution.classpath(Classpath::Test), cache),
        ),
    ])
}

/// One entry per jar: its coordinate, where it is, and its `-sources.jar`
/// when `jrs fetch --sources` has put one in the cache (`null` otherwise).
fn entries(resolution: &Resolution, jars: &[PathBuf], cache: Option<&Cache>) -> Json {
    Json::Array(
        jars.iter()
            .map(|jar| {
                let package = resolution
                    .packages
                    .iter()
                    .find(|p| p.jar.as_ref() == Some(jar));
                let sources = package
                    .zip(cache)
                    .and_then(|(p, c)| resolve::cached_sources(&p.coord, c));
                Json::object([
                    (
                        "coordinate",
                        Json::or_null(package, |p| Json::string(p.coord.to_string())),
                    ),
                    ("jar", Json::path(&absolute(jar))),
                    (
                        "sources",
                        Json::or_null(sources, |s| Json::path(&absolute(&s))),
                    ),
                ])
            })
            .collect(),
    )
}

/// The tasks in declaration order: name, description, and the hooks that run
/// them.
fn tasks_json(m: &Manifest) -> Json {
    Json::Array(
        m.tasks
            .iter()
            .map(|t| {
                let hooks: Vec<Json> = m
                    .hooks
                    .iter()
                    .filter(|(_, names)| names.contains(&t.name))
                    .map(|(hook, _)| Json::string(hook.name()))
                    .collect();
                Json::object([
                    ("name", Json::string(&t.name)),
                    (
                        "description",
                        Json::or_null(t.description.as_deref(), Json::string),
                    ),
                    ("hooks", Json::Array(hooks)),
                ])
            })
            .collect(),
    )
}

fn absolute(p: &Path) -> PathBuf {
    std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lockfile::Lockfile;
    use crate::resolve::coord::Coord;

    fn root() -> PathBuf {
        std::env::temp_dir().join(format!("jrs-model-{}", std::process::id()))
    }

    fn manifest(body: &str) -> Manifest {
        let root = root();
        let text = format!("[project]\nname = \"app\"\nversion = \"1.0.0\"\n{body}");
        Manifest::parse(&text, &root.join("jrs.toml"), &root).unwrap()
    }

    fn render(inputs: &Inputs<'_>) -> String {
        metadata(inputs).unwrap().render()
    }

    #[test]
    fn a_bare_project_has_every_key_in_a_fixed_order() {
        let m = manifest("");
        let text = render(&Inputs {
            manifest: &m,
            toolchain: None,
            resolution: None,
            cache: None,
        });
        let keys: Vec<&str> = text
            .lines()
            .filter(|l| l.starts_with("  \"") && !l.starts_with("   "))
            .map(|l| l.trim_start().split('"').nth(1).unwrap())
            .collect();
        assert_eq!(
            keys,
            [
                "version",
                "jrs",
                "project",
                "jdk",
                "java",
                "languages",
                "main",
                "test",
                "target-dir",
                "jar",
                "classpaths",
                "tasks"
            ]
        );
        assert!(text.starts_with("{\n  \"version\": 1,\n"), "{text}");
        assert!(text.contains("\"jdk\": null"), "{text}");
        assert!(text.contains("\"classpaths\": null"), "{text}");
        assert!(text.contains("\"main-class\": null"), "{text}");
        let root = root();
        for dir in [
            root.join("src").join("main").join("java"),
            root.join("src").join("test").join("resources"),
            root.join("target").join("test-classes"),
            root.join("target").join("app-1.0.0.jar"),
        ] {
            assert!(
                text.contains(&crate::json::escape(&dir.display().to_string())),
                "{} missing from {text}",
                dir.display()
            );
        }
    }

    #[test]
    fn languages_roots_and_tasks_come_from_the_manifest() {
        let m = manifest(
            "main-class = \"com.example.MainKt\"\n\
             [kotlin]\nversion = \"2.4.20\"\n\
             [tasks.gen]\ndescription = \"Generate code\"\nrun = [\"true\"]\n\
             outputs = [\"{target}/generated/sources\"]\n\
             source-outputs = [\"{target}/generated/sources\"]\n\
             [tasks.lint]\nrun = [\"true\"]\n\
             [hooks]\npre-compile = [\"gen\"]\n",
        );
        let text = render(&Inputs {
            manifest: &m,
            toolchain: None,
            resolution: None,
            cache: None,
        });
        assert!(text.contains("\"main-class\": \"com.example.MainKt\""));
        assert!(text.contains("\"name\": \"kotlin\",\n      \"version\": \"2.4.20\",\n      \"compiler\": \"org.jetbrains.kotlin:kotlin-compiler-embeddable:2.4.20\""), "{text}");
        let kotlin_root = root().join("src").join("main").join("kotlin");
        assert!(
            text.contains(&format!(
                "\"language\": \"kotlin\",\n        \"dir\": {}",
                crate::json::escape(&kotlin_root.display().to_string())
            )),
            "{text}"
        );
        let generated = root().join("target").join("generated").join("sources");
        assert!(
            text.contains(&format!(
                "\"generated-sources\": [\n      {}\n    ]",
                crate::json::escape(&generated.display().to_string())
            )),
            "{text}"
        );
        assert!(
            text.contains(
                "\"name\": \"gen\",\n      \"description\": \"Generate code\",\n      \
                 \"hooks\": [\n        \"pre-compile\"\n      ]"
            ),
            "{text}"
        );
        assert!(
            text.contains("\"name\": \"lint\",\n      \"description\": null,\n      \"hooks\": []"),
            "{text}"
        );
    }

    #[test]
    fn classpaths_name_each_jar_and_its_sources_when_cached() {
        let cache_dir = root().join("cache");
        let _ = std::fs::remove_dir_all(&cache_dir);
        let cache = Cache::with_root(&cache_dir);
        let lock = Lockfile::parse(
            "version = 1\nmanifest-checksum = \"x\"\n\
             roots = [\"org.example:lib\", \"org.example:api\"]\ntest-roots = [\"org.example:junit\"]\n\
             [[package]]\ngroup = \"org.example\"\nartifact = \"api\"\nversion = \"2.0\"\n\
             classpath = \"provided\"\npackaging = \"jar\"\ndepth = 1\ndirect = true\n\
             [[package]]\ngroup = \"org.example\"\nartifact = \"junit\"\nversion = \"5.0\"\n\
             classpath = \"test\"\npackaging = \"jar\"\ndepth = 1\ndirect = true\n\
             [[package]]\ngroup = \"org.example\"\nartifact = \"lib\"\nversion = \"1.0\"\n\
             classpath = \"compile\"\npackaging = \"jar\"\ndepth = 1\ndirect = true\n",
            Path::new("jrs.lock"),
        )
        .unwrap();
        let mut resolution = lock.to_resolution();
        for p in &mut resolution.packages {
            p.jar = Some(cache.path_for(&p.coord, "jar"));
        }
        let lib = Coord::new("org.example", "lib", "1.0");
        let sources = cache.path_for(&lib.sources(), "jar");
        std::fs::create_dir_all(sources.parent().unwrap()).unwrap();
        std::fs::write(&sources, b"sources").unwrap();

        let m = manifest("");
        let text = render(&Inputs {
            manifest: &m,
            toolchain: None,
            resolution: Some(&resolution),
            cache: Some(&cache),
        });
        let _ = std::fs::remove_dir_all(&cache_dir);

        let section = |name: &str| {
            let start = text.find(&format!("\"{name}\": [")).unwrap();
            let end = start + text[start..].find("\n    ]").unwrap();
            text[start..end].to_string()
        };
        let coordinates = |s: &str| -> Vec<String> {
            s.lines()
                .filter_map(|l| l.trim().strip_prefix("\"coordinate\": "))
                .map(|c| c.trim_end_matches(',').to_string())
                .collect()
        };
        assert_eq!(
            coordinates(&section("compile")),
            ["\"org.example:api:2.0\"", "\"org.example:lib:1.0\""]
        );
        assert_eq!(
            coordinates(&section("runtime")),
            ["\"org.example:lib:1.0\""]
        );
        assert_eq!(
            coordinates(&section("test")),
            [
                "\"org.example:api:2.0\"",
                "\"org.example:junit:5.0\"",
                "\"org.example:lib:1.0\""
            ]
        );
        let runtime = section("runtime");
        assert!(
            runtime.contains(&format!(
                "\"sources\": {}",
                crate::json::escape(&absolute(&sources).display().to_string())
            )),
            "{runtime}"
        );
        assert!(
            section("compile").contains("\"sources\": null"),
            "api has no sources jar"
        );
    }
}
