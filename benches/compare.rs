//! jrs against Maven and Gradle (specs/todo/BENCHMARKS.md, ROADMAP §2): the
//! same projects built, tested, run and packaged by each tool, checked to be
//! the same build before anything is timed, from a local mirror of Maven
//! Central, with the results written to `benchmarks/REPORT.md` and every
//! sample to `benchmarks/raw/<date>-<host>.tsv`.
//!
//! ```text
//! JRS_BENCH_RECORD=1 cargo bench --bench compare        # fill the mirror from Maven Central
//! cargo bench --bench compare
//! JRS_BENCH_TOOLS=jrs,gradle JRS_BENCH_PROJECTS=gen-300 \
//!   JRS_BENCH_SCENARIOS=noop,incremental JRS_BENCH_RUNS=11 cargo bench --bench compare
//! ```
//!
//! | Variable | Default | Meaning |
//! | --- | --- | --- |
//! | `JRS_BENCH_TOOLS` | `jrs,maven,gradle` | tools to run; `gradle` is the three Gradle columns |
//! | `JRS_BENCH_PROJECTS` | `all` | a comma list of the parity set's names and the corpus's |
//! | `JRS_BENCH_SCENARIOS` | all | a comma list of [`SCENARIOS`] |
//! | `JRS_BENCH_RUNS` | `7` | measured runs per cell |
//! | `JRS_BENCH_WARMUP` | `2` | untimed runs per cell before them |
//! | `JRS_BENCH_MIRROR` | `~/.cache/jrs-bench/mirror` | the mirror directory |
//! | `JRS_BENCH_LATENCY_MS` | `0` | fixed latency per mirror response |
//! | `JRS_BENCH_REPORT` | `benchmarks/REPORT.md` | where the report is written |
//!
//! **The projects.** The parity set is one build written three times:
//! `wordstats` and `bookmarks` are the examples of the same names, with the
//! Maven and Gradle builds of `benchmarks/projects/<name>/` laid over a copy;
//! `gen-300`, `gen-2000` and `gen-ws` are written by [`generate`], seeded,
//! with all three build files from one description. The corpus set is every
//! project of `docs/corpus/corpus.toml` whose last corpus run passed every
//! step, from that run's clone, against the build it ships.
//!
//! **The mirror.** No measured run touches the network: every tool is
//! pointed at `benches/support/mirror.rs`, serving Maven Central's layout
//! under `/central/` and the Gradle Plugin Portal's under `/plugins/`. With
//! `JRS_BENCH_RECORD=1` a miss is fetched from the real host and kept, and
//! the harness runs only the equivalence checks, each tool once over every
//! project from cold caches, which is what fills the mirror. Without it a
//! miss is a 404, and the cell it happened in is `incomplete`.
//!
//! **One run.** Each column builds its own copy of the project, with its own
//! caches (`JRS_CACHE_DIR`, `-Dmaven.repo.local`, `GRADLE_USER_HOME`), under
//! `target/compare/<run>/`; the logs stay there, the copies are deleted at
//! the end. Before a project is timed, [`check`] runs `test`, the runtime
//! classpath, `run` and `package` on each tool and refuses to time a project
//! whose tools disagree (§5.1). Then every scenario puts each column's copy
//! in its state, untimed, and times one command, round-robin over the
//! columns, warm-ups first. A command's output goes to a log, never a
//! terminal.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant, SystemTime};

use jrs::json::Json;

#[path = "support/host.rs"]
mod host;
#[path = "support/junit.rs"]
mod junit;
#[path = "support/mirror.rs"]
mod mirror;
#[path = "support/proc.rs"]
mod proc;

use junit::{Counts, Reports, Tests};
use mirror::{Route, Server};
use proc::{Exec, http_status, spawn, stop};

/// The scenario ids of BENCHMARKS.md §4, in the report's order.
const SCENARIOS: [&str; 9] = [
    "cold",
    "clean",
    "noop",
    "incremental",
    "abi",
    "test",
    "test-edit",
    "run",
    "package",
];

/// How long one command may take before its process group is killed.
const COMMAND_TIMEOUT: Duration = Duration::from_mins(30);

/// How long `run` waits for its probe.
const PROBE_TIMEOUT: Duration = Duration::from_mins(3);

/// Pinned, so the project's own pin cannot change the output format.
const DEPENDENCY_PLUGIN: &str = "org.apache.maven.plugins:maven-dependency-plugin:3.8.1";

const CENTRAL: &str = "https://repo.maven.apache.org/maven2/";
const PLUGIN_PORTAL: &str = "https://plugins.gradle.org/m2/";

/// The versions the generated build files pin; the hand-written ones in
/// `benchmarks/projects/` say the same.
const JUNIT: &str = "5.11.4";
const JUNIT_PLATFORM: &str = "1.11.4";
const SHADOW: &str = "9.4.1";
const RELEASE: u32 = 17;

/// The dependencies the generator draws from, each with a class to refer to:
/// every one a jar with no runtime dependency of its own, unsigned, so the
/// three tools' runtime classpaths are the same set by construction and a
/// fat jar of them starts.
const DEPENDENCIES: &[(&str, &str)] = &[
    (
        "org.apache.commons:commons-lang3:3.14.0",
        "org.apache.commons.lang3.StringUtils",
    ),
    (
        "commons-io:commons-io:2.16.1",
        "org.apache.commons.io.IOUtils",
    ),
    (
        "org.apache.commons:commons-collections4:4.4",
        "org.apache.commons.collections4.CollectionUtils",
    ),
    (
        "commons-codec:commons-codec:1.17.0",
        "org.apache.commons.codec.binary.Hex",
    ),
    (
        "org.apache.commons:commons-math3:3.6.1",
        "org.apache.commons.math3.util.FastMath",
    ),
    (
        "commons-cli:commons-cli:1.8.0",
        "org.apache.commons.cli.Options",
    ),
    ("com.google.code.gson:gson:2.10.1", "com.google.gson.Gson"),
    ("org.slf4j:slf4j-api:2.0.13", "org.slf4j.LoggerFactory"),
    ("org.yaml:snakeyaml:2.2", "org.yaml.snakeyaml.Yaml"),
    ("joda-time:joda-time:2.12.7", "org.joda.time.DateTime"),
    ("org.jsoup:jsoup:1.17.2", "org.jsoup.Jsoup"),
    ("com.h2database:h2:2.2.224", "org.h2.Driver"),
    ("org.ow2.asm:asm:9.7", "org.objectweb.asm.ClassReader"),
    (
        "net.bytebuddy:byte-buddy:1.14.17",
        "net.bytebuddy.ByteBuddy",
    ),
    ("org.javassist:javassist:3.30.2-GA", "javassist.ClassPool"),
    (
        "org.hdrhistogram:HdrHistogram:2.2.2",
        "org.HdrHistogram.Histogram",
    ),
    ("org.lz4:lz4-java:1.8.0", "net.jpountz.lz4.LZ4Factory"),
    ("org.tukaani:xz:1.9", "org.tukaani.xz.XZInputStream"),
    ("org.json:json:20240303", "org.json.JSONObject"),
    (
        "net.sf.jopt-simple:jopt-simple:5.0.4",
        "joptsimple.OptionParser",
    ),
    ("info.picocli:picocli:4.7.6", "picocli.CommandLine"),
    (
        "com.beust:jcommander:1.82",
        "com.beust.jcommander.JCommander",
    ),
    (
        "org.antlr:antlr4-runtime:4.13.1",
        "org.antlr.v4.runtime.CharStreams",
    ),
    (
        "org.apache.commons:commons-pool2:2.12.0",
        "org.apache.commons.pool2.impl.GenericObjectPool",
    ),
    (
        "commons-logging:commons-logging:1.3.2",
        "org.apache.commons.logging.LogFactory",
    ),
    ("org.hsqldb:hsqldb:2.7.3", "org.hsqldb.jdbc.JDBCDriver"),
    (
        "org.jline:jline:3.26.2",
        "org.jline.terminal.TerminalBuilder",
    ),
    (
        "org.fusesource.jansi:jansi:2.4.1",
        "org.fusesource.jansi.AnsiConsole",
    ),
    ("org.objenesis:objenesis:3.4", "org.objenesis.ObjenesisStd"),
    ("org.mindrot:jbcrypt:0.4", "org.mindrot.jbcrypt.BCrypt"),
    (
        "com.google.zxing:core:3.5.3",
        "com.google.zxing.MultiFormatReader",
    ),
    ("net.java.dev.jna:jna:5.14.0", "com.sun.jna.Native"),
    (
        "org.freemarker:freemarker:2.3.33",
        "freemarker.template.Configuration",
    ),
    ("org.mozilla:rhino:1.7.15", "org.mozilla.javascript.Context"),
    ("org.ini4j:ini4j:0.5.4", "org.ini4j.Ini"),
    ("org.jdom:jdom2:2.0.6.1", "org.jdom2.Document"),
    (
        "org.codehaus.woodstox:stax2-api:4.2.2",
        "org.codehaus.stax2.XMLStreamReader2",
    ),
    ("com.lmax:disruptor:4.0.0", "com.lmax.disruptor.RingBuffer"),
    (
        "org.jctools:jctools-core:4.0.5",
        "org.jctools.queues.MpscArrayQueue",
    ),
    (
        "com.carrotsearch:hppc:0.10.0",
        "com.carrotsearch.hppc.IntArrayList",
    ),
    (
        "net.openhft:zero-allocation-hashing:0.16",
        "net.openhft.hashing.LongHashFunction",
    ),
    (
        "com.googlecode.javaewah:JavaEWAH:1.2.3",
        "com.googlecode.javaewah.EWAHCompressedBitmap",
    ),
    (
        "org.locationtech.jts:jts-core:1.19.0",
        "org.locationtech.jts.geom.GeometryFactory",
    ),
    (
        "com.univocity:univocity-parsers:2.9.1",
        "com.univocity.parsers.csv.CsvParser",
    ),
    (
        "io.github.classgraph:classgraph:4.8.174",
        "io.github.classgraph.ClassGraph",
    ),
    (
        "org.checkerframework:checker-qual:3.43.0",
        "org.checkerframework.checker.nullness.qual.Nullable",
    ),
    (
        "com.google.errorprone:error_prone_annotations:2.28.0",
        "com.google.errorprone.annotations.CanIgnoreReturnValue",
    ),
    (
        "com.google.code.findbugs:jsr305:3.0.2",
        "javax.annotation.Nonnull",
    ),
    (
        "org.jetbrains:annotations:24.1.0",
        "org.jetbrains.annotations.NotNull",
    ),
    (
        "jakarta.inject:jakarta.inject-api:2.0.1",
        "jakarta.inject.Inject",
    ),
    (
        "jakarta.annotation:jakarta.annotation-api:2.1.1",
        "jakarta.annotation.PostConstruct",
    ),
    ("javax.inject:javax.inject:1", "javax.inject.Inject"),
    (
        "com.google.protobuf:protobuf-java:3.25.3",
        "com.google.protobuf.Message",
    ),
    (
        "org.msgpack:msgpack-core:0.9.8",
        "org.msgpack.core.MessagePack",
    ),
    (
        "com.esotericsoftware:minlog:1.3.1",
        "com.esotericsoftware.minlog.Log",
    ),
    (
        "net.sf.trove4j:trove4j:3.0.3",
        "gnu.trove.list.array.TIntArrayList",
    ),
    (
        "org.apache.logging.log4j:log4j-api:2.23.1",
        "org.apache.logging.log4j.LogManager",
    ),
    ("org.tinylog:tinylog-api:2.7.0", "org.tinylog.Logger"),
    (
        "com.github.javaparser:javaparser-core:3.26.0",
        "com.github.javaparser.StaticJavaParser",
    ),
    (
        "org.apache.commons:commons-exec:1.4.0",
        "org.apache.commons.exec.CommandLine",
    ),
];

/// A project the generator writes (BENCHMARKS.md §2).
struct Gen {
    name: &'static str,
    classes: usize,
    packages: usize,
    dependencies: usize,
    /// A workspace's members and the members each depends on, in build
    /// order; empty for a single project.
    members: &'static [(&'static str, &'static [&'static str])],
}

const GENERATED: [Gen; 3] = [
    Gen {
        name: "gen-300",
        classes: 300,
        packages: 20,
        dependencies: 30,
        members: &[],
    },
    Gen {
        name: "gen-2000",
        classes: 2000,
        packages: 100,
        dependencies: 60,
        members: &[],
    },
    Gen {
        name: "gen-ws",
        classes: 300,
        packages: 18,
        dependencies: 30,
        members: &[
            ("core", &[]),
            ("model", &["core"]),
            ("store", &["core"]),
            ("service", &["model", "store"]),
            ("web", &["service"]),
            ("app", &["web", "service"]),
        ],
    },
];

/// The parity set's projects taken from `examples/`, in the report's order
/// before the generated ones, each with what its `run` prints or answers.
const EXAMPLES: [(&str, Flavor, Probe); 2] = [
    (
        "wordstats",
        Flavor::Plain,
        Probe::Stdout("bundled sample: 103 words"),
    ),
    (
        "bookmarks",
        Flavor::SpringBoot,
        Probe::Http("http://localhost:8080/api/bookmarks", 200),
    ),
];

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Tool {
    Jrs,
    Maven,
    Gradle,
}

impl Tool {
    fn name(self) -> &'static str {
        match self {
            Tool::Jrs => "jrs",
            Tool::Maven => "maven",
            Tool::Gradle => "gradle",
        }
    }

    fn reports(self) -> Reports {
        match self {
            Tool::Jrs => Reports::Jrs,
            Tool::Maven => Reports::Surefire,
            Tool::Gradle => Reports::Gradle,
        }
    }
}

/// A column of the report: one tool run one way (BENCHMARKS.md §5).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Column {
    Jrs,
    Maven,
    GradleDaemon,
    GradleCc,
    GradleNoDaemon,
}

const COLUMNS: [Column; 5] = [
    Column::Jrs,
    Column::Maven,
    Column::GradleDaemon,
    Column::GradleCc,
    Column::GradleNoDaemon,
];

impl Column {
    fn tool(self) -> Tool {
        match self {
            Column::Jrs => Tool::Jrs,
            Column::Maven => Tool::Maven,
            Column::GradleDaemon | Column::GradleCc | Column::GradleNoDaemon => Tool::Gradle,
        }
    }

    /// The name in the raw samples and the work directory.
    fn id(self) -> &'static str {
        match self {
            Column::Jrs => "jrs",
            Column::Maven => "maven",
            Column::GradleDaemon => "gradle-daemon",
            Column::GradleCc => "gradle-daemon-cc",
            Column::GradleNoDaemon => "gradle-no-daemon",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Column::Jrs => "jrs",
            Column::Maven => "Maven",
            Column::GradleDaemon => "Gradle (daemon)",
            Column::GradleCc => "Gradle (daemon, CC)",
            Column::GradleNoDaemon => "Gradle (no daemon)",
        }
    }
}

/// What a project's `run` and `package` mean to Maven and Gradle.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Flavor {
    /// `exec:java` and `run`; `maven-shade-plugin` and `shadowJar`.
    Plain,
    /// `spring-boot:run` and `bootRun`; Boot's repackaged jar and `bootJar`.
    SpringBoot,
}

/// How to tell the application started.
#[derive(Clone)]
enum Probe {
    Stdout(&'static str),
    Http(&'static str, u16),
    /// The corpus's, read from `corpus.toml`.
    OwnedStdout(String),
    OwnedHttp(String, u16),
}

impl Probe {
    fn url(&self) -> Option<(&str, u16)> {
        match self {
            Probe::Http(url, status) => Some((url, *status)),
            Probe::OwnedHttp(url, status) => Some((url, *status)),
            Probe::Stdout(_) | Probe::OwnedStdout(_) => None,
        }
    }

    fn stdout(&self) -> Option<&str> {
        match self {
            Probe::Stdout(text) => Some(text),
            Probe::OwnedStdout(text) => Some(text),
            Probe::Http(..) | Probe::OwnedHttp(..) => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Set {
    Parity,
    Corpus,
}

enum Origin {
    /// `examples/<name>`, with `benchmarks/projects/<name>/` laid over it.
    Example(PathBuf, PathBuf),
    Generated(&'static Gen),
    /// The corpus run's clone, at the build file's directory.
    Clone(PathBuf),
}

/// One source file's edit, for `incremental`, `abi` and `test-edit`.
#[derive(Clone)]
struct Edit {
    /// Relative to the project.
    file: PathBuf,
    from: String,
    to: String,
    /// What the report says the edit is.
    what: String,
}

struct Project {
    name: String,
    set: Set,
    flavor: Flavor,
    origin: Origin,
    run: Option<Probe>,
    /// Why `run` is not timed, when it is not.
    no_run: Option<&'static str>,
    /// The workspace member `run` and `package` act on.
    member: Option<String>,
    /// The members, whose own jars leave the classpath comparison.
    members: Vec<String>,
    /// The tools whose build it has.
    tools: BTreeSet<Tool>,
    /// For the corpus set, the version differences of Gradle's
    /// highest-wins mediation are recorded, not failed (CORPUS.md §4.1).
    versions_may_differ: bool,
}

struct Settings {
    columns: Vec<Column>,
    projects: Option<BTreeSet<String>>,
    scenarios: Vec<&'static str>,
    runs: usize,
    warmup: usize,
    mirror: PathBuf,
    latency: u64,
    report: PathBuf,
    record: bool,
}

/// The tools found on this machine.
struct Tools {
    java_home: Option<PathBuf>,
    mvn: Option<PathBuf>,
    gradle: Option<PathBuf>,
    /// Why a tool asked for is not there.
    missing: BTreeMap<Tool, String>,
}

fn main() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let settings = settings(repo_root);
    let started = SystemTime::now();
    let tools = discover_tools(&settings);
    for (tool, why) in &tools.missing {
        println!("SKIPPED {}: {why}", tool.name());
    }
    if tools.java_home.is_none() {
        println!("SKIPPED the comparison: no JDK at JAVA_HOME or on PATH");
        return;
    }

    let run_dir = repo_root
        .join("target/compare")
        .join(host::stamp(started, true));
    std::fs::create_dir_all(&run_dir).expect("create the run's directory");
    std::fs::create_dir_all(&settings.mirror).expect("create the mirror directory");
    let server = Server::with_routes(vec![
        Route {
            prefix: "/central/".to_string(),
            root: settings.mirror.join("central"),
            upstream: Some(CENTRAL.to_string()),
        },
        Route {
            prefix: "/plugins/".to_string(),
            root: settings.mirror.join("plugins"),
            upstream: Some(PLUGIN_PORTAL.to_string()),
        },
    ]);
    server.set_recording(settings.record);
    server.set_latency(settings.latency);
    let env = Env::new(&run_dir, &server, &settings, &tools);

    let projects = projects(repo_root, &settings, &tools);
    println!(
        "jrs against Maven and Gradle (specs/todo/BENCHMARKS.md){}\n",
        if settings.record {
            ", recording the mirror"
        } else {
            ""
        }
    );
    println!("  run      {}", run_dir.display());
    println!("  mirror   {}", settings.mirror.display());
    println!(
        "  columns  {}",
        settings
            .columns
            .iter()
            .filter(|c| !tools.missing.contains_key(&c.tool()))
            .map(|c| c.title())
            .collect::<Vec<_>>()
            .join(", ")
    );
    println!(
        "  runs     {} measured, {} warm-up, per cell\n",
        settings.runs, settings.warmup
    );

    let mut results = Vec::new();
    for project in &projects.projects {
        println!("  {} …", project.name);
        let result = bench_project(project, &env, &settings, &tools);
        results.push(result);
    }
    if settings.record {
        let (requests, misses) = server.counts();
        println!(
            "\n  recorded: {requests} requests, {misses} the mirror could not answer; {}",
            settings.mirror.display()
        );
        for path in server.take_missed().iter().take(20) {
            println!("    miss {path}");
        }
        return;
    }
    let header = header(repo_root, started, &settings, &tools, &projects);
    let report = report(&header, &results, &settings, &projects.left_out, repo_root);
    if let Some(parent) = settings.report.parent() {
        std::fs::create_dir_all(parent).expect("create the report's directory");
    }
    std::fs::write(&settings.report, &report).expect("write the report");
    let raw = raw_path(&settings.report, started);
    if let Some(parent) = raw.parent() {
        std::fs::create_dir_all(parent).expect("create the raw samples' directory");
    }
    std::fs::write(&raw, raw_samples(&results)).expect("write the raw samples");
    println!("\n{report}");
    println!("  report   {}", settings.report.display());
    println!("  samples  {}", raw.display());
    println!("  logs     {}", run_dir.display());
    if results.iter().any(Outcome::failed) {
        std::process::exit(1);
    }
}

fn settings(repo_root: &Path) -> Settings {
    let list = |name: &str| -> Option<Vec<String>> {
        std::env::var(name)
            .ok()
            .filter(|v| !v.trim().is_empty() && v.trim() != "all")
            .map(|v| {
                v.split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
    };
    let tools: Vec<String> = list("JRS_BENCH_TOOLS")
        .unwrap_or_else(|| vec!["jrs".into(), "maven".into(), "gradle".into()]);
    for tool in &tools {
        assert!(
            ["jrs", "maven", "gradle"].contains(&tool.as_str()),
            "JRS_BENCH_TOOLS: no tool `{tool}`; the tools are jrs, maven and gradle"
        );
    }
    let columns = COLUMNS
        .into_iter()
        .filter(|c| tools.iter().any(|t| t == c.tool().name()))
        .collect();
    let scenarios = match list("JRS_BENCH_SCENARIOS") {
        None => SCENARIOS.to_vec(),
        Some(names) => {
            for name in &names {
                assert!(
                    SCENARIOS.contains(&name.as_str()),
                    "JRS_BENCH_SCENARIOS: no scenario `{name}`; the scenarios are {SCENARIOS:?}"
                );
            }
            SCENARIOS
                .into_iter()
                .filter(|s| names.iter().any(|n| n == s))
                .collect()
        }
    };
    let home = std::env::var_os("HOME").map_or_else(std::env::temp_dir, PathBuf::from);
    let mirror = std::env::var_os("JRS_BENCH_MIRROR")
        .map_or_else(|| home.join(".cache/jrs-bench/mirror"), PathBuf::from);
    let report = std::env::var_os("JRS_BENCH_REPORT")
        .map_or_else(|| repo_root.join("benchmarks/REPORT.md"), PathBuf::from);
    Settings {
        columns,
        projects: list("JRS_BENCH_PROJECTS").map(|v| v.into_iter().collect()),
        scenarios,
        runs: env_usize("JRS_BENCH_RUNS", 7).max(1),
        warmup: env_usize("JRS_BENCH_WARMUP", 2),
        mirror,
        latency: env_usize("JRS_BENCH_LATENCY_MS", 0) as u64,
        report,
        record: std::env::var("JRS_BENCH_RECORD").is_ok_and(|v| !v.is_empty() && v != "0"),
    }
}

fn env_usize(name: &str, default: usize) -> usize {
    match std::env::var(name) {
        Ok(v) => v
            .trim()
            .parse()
            .unwrap_or_else(|_| panic!("{name} must be a whole number, got `{v}`")),
        Err(_) => default,
    }
}

fn discover_tools(settings: &Settings) -> Tools {
    let wanted = |tool: Tool| settings.columns.iter().any(|c| c.tool() == tool);
    let mut missing = BTreeMap::new();
    let mvn = on_path(if cfg!(windows) { "mvn.cmd" } else { "mvn" });
    let gradle = on_path(if cfg!(windows) {
        "gradle.bat"
    } else {
        "gradle"
    });
    if wanted(Tool::Maven) && mvn.is_none() {
        missing.insert(
            Tool::Maven,
            "mvn not on PATH (a corpus project's mvnw is still used)".to_string(),
        );
    }
    if wanted(Tool::Gradle) && gradle.is_none() {
        missing.insert(
            Tool::Gradle,
            "gradle not on PATH (a corpus project's gradlew is still used)".to_string(),
        );
    }
    Tools {
        java_home: java_home(),
        mvn,
        gradle,
        missing,
    }
}

fn on_path(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(name))
        .find(|p| p.is_file())
}

/// `JAVA_HOME`, or the home of the `java` on `PATH`: every subprocess gets
/// it, so the three tools run on one JDK (BENCHMARKS.md §3.2).
fn java_home() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("JAVA_HOME").map(PathBuf::from)
        && home.join("bin").join(java_exe()).is_file()
    {
        return Some(home);
    }
    let output = Command::new("java")
        .args(["-XshowSettings:properties", "-version"])
        .output()
        .ok()?;
    String::from_utf8_lossy(&output.stderr)
        .lines()
        .find_map(|l| l.trim().strip_prefix("java.home = "))
        .map(PathBuf::from)
}

fn java_exe() -> &'static str {
    if cfg!(windows) { "java.exe" } else { "java" }
}

/// Everything a command needs that is the same for every project: the
/// mirror's URLs and the files that point each tool at it.
struct Env {
    run_dir: PathBuf,
    server: Server,
    java_home: PathBuf,
    jrs_config: PathBuf,
    maven_settings: PathBuf,
    gradle_init: PathBuf,
    /// Shared by every Gradle home, so a wrapper's distribution is
    /// downloaded once, outside any timing.
    gradle_dists: PathBuf,
    maven_dists: PathBuf,
    mvn: Option<PathBuf>,
    gradle: Option<PathBuf>,
}

impl Env {
    fn new(run_dir: &Path, server: &Server, settings: &Settings, tools: &Tools) -> Env {
        let central = format!("{}/central/", server.url);
        let plugins = format!("{}/plugins/", server.url);
        let jrs_config = run_dir.join("jrs-config.toml");
        std::fs::write(
            &jrs_config,
            format!(
                "# Written by benches/compare.rs: every repository is the local mirror.\n\
                 [mirrors]\n\"*\" = \"{central}\"\n"
            ),
        )
        .expect("write the jrs config");
        let maven_settings = run_dir.join("settings.xml");
        std::fs::write(
            &maven_settings,
            format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
                 <!-- Written by benches/compare.rs: every repository is the local mirror. -->\n\
                 <settings>\n  <mirrors>\n    <mirror>\n      <id>jrs-bench</id>\n      \
                 <mirrorOf>*</mirrorOf>\n      <url>{central}</url>\n    </mirror>\n  \
                 </mirrors>\n</settings>\n"
            ),
        )
        .expect("write the Maven settings");
        let gradle_init = run_dir.join("mirror.gradle");
        std::fs::write(
            &gradle_init,
            gradle_init_script(&server.url, &central, &plugins),
        )
        .expect("write the Gradle init script");
        let shared = settings
            .mirror
            .parent()
            .map_or_else(|| settings.mirror.clone(), Path::to_path_buf);
        Env {
            run_dir: run_dir.to_path_buf(),
            server: server.clone(),
            java_home: tools.java_home.clone().expect("a JDK"),
            jrs_config,
            maven_settings,
            gradle_init,
            gradle_dists: shared.join("gradle-dists"),
            maven_dists: shared.join("maven-dists"),
            mvn: tools.mvn.clone(),
            gradle: tools.gradle.clone(),
        }
    }

    fn java(&self) -> PathBuf {
        self.java_home.join("bin").join(java_exe())
    }
}

/// The init script every Gradle run gets: every repository a build declares
/// is pointed at the mirror (`plugins.gradle.org` at its `/plugins/`, any
/// other at `/central/`), and every project gets a `jrsBenchClasspath` task
/// writing its runtime classpath's module ids. Groovy rather than Kotlin:
/// it is compiled again in every cold Gradle home, and a Kotlin script's
/// compile would be the harness's cost counted as Gradle's.
fn gradle_init_script(server: &str, central: &str, plugins: &str) -> String {
    format!(
        "// Written by jrs's benchmark harness (benches/compare.rs): every repository
// is the local mirror, and every project has a jrsBenchClasspath task.
def server = '{server}'
def central = '{central}'
def plugins = '{plugins}'
def mirror = {{ handler ->
    handler.all {{ repo ->
        if (repo instanceof MavenArtifactRepository) {{
            def url = repo.url.toString()
            if (!url.startsWith(server)) {{
                repo.url = url.contains('plugins.gradle.org') ? plugins : central
            }}
            repo.allowInsecureProtocol = true
        }}
    }}
}}

beforeSettings {{ settings ->
    settings.pluginManagement.repositories {{
        maven {{ url = plugins; allowInsecureProtocol = true }}
        maven {{ url = central; allowInsecureProtocol = true }}
    }}
    mirror(settings.pluginManagement.repositories)
    mirror(settings.dependencyResolutionManagement.repositories)
}}

allprojects {{ project ->
    mirror(project.buildscript.repositories)
    mirror(project.repositories)
    project.tasks.register('jrsBenchClasspath') {{
        def configuration = project.configurations.findByName('runtimeClasspath')
        def out = new File(project.findProperty('jrsBenchClasspath').toString())
        doLast {{
            if (configuration == null) return
            configuration.incoming.artifacts.artifacts.each {{ a ->
                def id = a.id.componentIdentifier
                if (id instanceof org.gradle.api.artifacts.component.ModuleComponentIdentifier) {{
                    def prefix = id.module + '-' + id.version + '-'
                    def name = a.file.name
                    def classifier = name.startsWith(prefix) && name.endsWith('.jar') ?
                        name.substring(prefix.length(), name.length() - 4) : ''
                    out << id.group + ':' + id.module + ':' + id.version +
                        (classifier ? ':' + classifier : '') + '\\n'
                }}
            }}
        }}
    }}
}}
"
    )
}

struct Selection {
    projects: Vec<Project>,
    /// Projects asked for, or in the corpus, that are not benchmarked, and why.
    left_out: Vec<(String, String)>,
    /// The corpus run the corpus set came from.
    corpus_run: Option<PathBuf>,
}

fn projects(repo_root: &Path, settings: &Settings, tools: &Tools) -> Selection {
    let wanted = |name: &str| settings.projects.as_ref().is_none_or(|p| p.contains(name));
    let mut chosen = Vec::new();
    let mut left_out = Vec::new();
    let all_tools: BTreeSet<Tool> = [Tool::Jrs, Tool::Maven, Tool::Gradle].into();
    for (name, flavor, probe) in EXAMPLES {
        if !wanted(name) {
            continue;
        }
        chosen.push(Project {
            name: name.to_string(),
            set: Set::Parity,
            flavor,
            origin: Origin::Example(
                repo_root.join("examples").join(name),
                repo_root.join("benchmarks/projects").join(name),
            ),
            run: Some(probe),
            no_run: None,
            member: None,
            members: Vec::new(),
            tools: all_tools.clone(),
            versions_may_differ: false,
        });
    }
    for generator in &GENERATED {
        if !wanted(generator.name) {
            continue;
        }
        let workspace = !generator.members.is_empty();
        chosen.push(Project {
            name: generator.name.to_string(),
            set: Set::Parity,
            flavor: Flavor::Plain,
            origin: Origin::Generated(generator),
            run: (!workspace).then_some(Probe::OwnedStdout(format!("{} ok", generator.name))),
            no_run: workspace.then_some(
                "`exec:java` cannot run one member of a reactor without installing the others",
            ),
            member: workspace.then(|| "app".to_string()),
            members: generator
                .members
                .iter()
                .map(|(m, _)| (*m).to_string())
                .collect(),
            tools: all_tools.clone(),
            versions_may_differ: false,
        });
    }
    let (corpus, corpus_run) = corpus_projects(repo_root, settings, tools, &mut left_out);
    chosen.extend(corpus);
    if let Some(names) = &settings.projects {
        for name in names {
            let known =
                chosen.iter().any(|p| &p.name == name) || left_out.iter().any(|(n, _)| n == name);
            assert!(
                known,
                "JRS_BENCH_PROJECTS: no project `{name}`; the parity set is wordstats, \
                 bookmarks, gen-300, gen-2000 and gen-ws, and the corpus's are in \
                 docs/corpus/corpus.toml"
            );
        }
    }
    for (name, why) in &left_out {
        println!("SKIPPED {name}: {why}");
    }
    Selection {
        projects: chosen,
        left_out,
        corpus_run,
    }
}

/// The corpus set: every project of `corpus.toml` whose row in the latest
/// corpus run's `REPORT.md` passed every step, from that run's clone, which
/// holds the `jrs.toml` `jrs migrate` wrote.
#[allow(
    clippy::too_many_lines,
    reason = "one check per reason a corpus project is left out, in order"
)]
fn corpus_projects(
    repo_root: &Path,
    settings: &Settings,
    tools: &Tools,
    left_out: &mut Vec<(String, String)>,
) -> (Vec<Project>, Option<PathBuf>) {
    let Ok(text) = std::fs::read_to_string(repo_root.join("docs/corpus/corpus.toml")) else {
        return (Vec::new(), None);
    };
    let corpus: toml::Table = toml::from_str(&text).expect("parse docs/corpus/corpus.toml");
    let entries = corpus
        .get("project")
        .and_then(toml::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let run = latest_corpus_run(repo_root);
    let report = run
        .as_ref()
        .and_then(|r| std::fs::read_to_string(r.join("REPORT.md")).ok())
        .unwrap_or_default();
    let host_jdk = host::host_jdk();
    let docker = Command::new("docker")
        .arg("info")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    let mut chosen = Vec::new();
    for entry in &entries {
        let field = |key: &str| entry.get(key).and_then(toml::Value::as_str);
        let Some(name) = field("name") else { continue };
        let asked = settings.projects.as_ref().is_some_and(|p| p.contains(name));
        if settings.projects.is_some() && !asked {
            continue;
        }
        let mut leave = |why: String| {
            // Unasked, a corpus project that cannot run is only a line.
            left_out.push((name.to_string(), why));
        };
        let Some(run) = &run else {
            leave("no corpus run under target/corpus/; run `cargo bench --bench corpus`".into());
            continue;
        };
        if !corpus_row_passed(&report, name) {
            leave(format!(
                "its row in {} did not pass every step",
                run.join("REPORT.md").display()
            ));
            continue;
        }
        let dir = field("dir").unwrap_or(".");
        let clone = run.join(name).join("clone").join(dir);
        if !clone.join("jrs.toml").is_file() {
            leave(format!("{} holds no jrs.toml", clone.display()));
            continue;
        }
        let jdk = entry
            .get("jdk")
            .and_then(toml::Value::as_integer)
            .unwrap_or(17);
        if host_jdk.is_none_or(|v| i64::from(v) < jdk) {
            leave(format!("it needs JDK {jdk} or later"));
            continue;
        }
        let needs_docker = entry
            .get("needs")
            .and_then(toml::Value::as_array)
            .is_some_and(|n| n.iter().any(|v| v.as_str() == Some("docker")));
        if needs_docker && !docker {
            leave("it needs Docker, which is not running".into());
            continue;
        }
        let build = match field("build") {
            Some("maven") => Tool::Maven,
            _ => Tool::Gradle,
        };
        if tools.missing.contains_key(&build) && !has_wrapper(&clone, build) {
            leave(format!("its build is {}, which is not here", build.name()));
            continue;
        }
        let probe = |key: &str| -> Option<Probe> {
            let table = entry.get(key)?.as_table()?;
            if let Some(url) = table.get("probe").and_then(toml::Value::as_str) {
                let expect = table
                    .get("expect")
                    .and_then(toml::Value::as_integer)
                    .and_then(|s| u16::try_from(s).ok())
                    .unwrap_or(200);
                return Some(Probe::OwnedHttp(url.to_string(), expect));
            }
            let text = table.get("stdout")?.as_str()?;
            Some(Probe::OwnedStdout(text.to_string()))
        };
        let member = field("member").map(str::to_string);
        chosen.push(Project {
            name: name.to_string(),
            set: Set::Corpus,
            flavor: if field("kind") == Some("spring-boot") {
                Flavor::SpringBoot
            } else {
                Flavor::Plain
            },
            origin: Origin::Clone(clone.clone()),
            run: probe("run"),
            no_run: None,
            members: workspace_members(&clone),
            member,
            tools: [Tool::Jrs, build].into(),
            versions_may_differ: build == Tool::Gradle,
        });
    }
    (chosen, run)
}

fn latest_corpus_run(repo_root: &Path) -> Option<PathBuf> {
    let mut runs: Vec<PathBuf> = std::fs::read_dir(repo_root.join("target/corpus"))
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join("REPORT.md").is_file())
        .collect();
    runs.sort();
    runs.pop()
}

/// Whether the project's row has no failing, unrun or unselected cell.
fn corpus_row_passed(report: &str, name: &str) -> bool {
    report.lines().any(|line| {
        let mut cells = line.split_whitespace();
        cells.next() == Some(name)
            && cells.next().is_some()
            && cells.clone().count() >= 6
            && cells.take(6).all(|c| {
                !(c == "FAIL"
                    || c == "·"
                    || c == "-"
                    || c == "skip"
                    || (c.starts_with('F') && c[1..].chars().all(|d| d.is_ascii_digit())))
            })
    })
}

fn workspace_members(dir: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(dir.join("jrs.toml")) else {
        return Vec::new();
    };
    let Ok(table) = toml::from_str::<toml::Table>(&text) else {
        return Vec::new();
    };
    table
        .get("workspace")
        .and_then(|w| w.get("members"))
        .and_then(toml::Value::as_array)
        .map(|m| {
            m.iter()
                .filter_map(toml::Value::as_str)
                .map(|m| m.rsplit('/').next().unwrap_or(m).to_string())
                .collect()
        })
        .unwrap_or_default()
}

fn has_wrapper(dir: &Path, tool: Tool) -> bool {
    cfg!(unix)
        && match tool {
            Tool::Maven => dir.join("mvnw").is_file(),
            Tool::Gradle => dir.join("gradlew").is_file(),
            Tool::Jrs => false,
        }
}

// ---------------------------------------------------------------------------
// Running one project

/// What happened to one project.
struct Outcome {
    name: String,
    set: Set,
    /// Why the project was not timed: the checks failed, or it could not be
    /// set up.
    refused: Option<String>,
    notes: Vec<String>,
    /// By scenario, then column.
    cells: BTreeMap<&'static str, BTreeMap<Column, Cell>>,
    /// The columns this project ran.
    columns: Vec<Column>,
    /// What the edits of `incremental`, `abi` and `test-edit` were.
    edits: Vec<String>,
    /// The Gradle version a wrapper picked, when the project ships one.
    gradle_version: Option<String>,
    maven_version: Option<String>,
}

impl Outcome {
    fn failed(&self) -> bool {
        self.refused.is_some()
            || self
                .cells
                .values()
                .flat_map(BTreeMap::values)
                .any(|c| c.failure.is_some())
    }
}

#[derive(Default)]
struct Cell {
    samples: Vec<Sample>,
    failure: Option<String>,
    /// Why the cell was not run at all.
    skipped: Option<String>,
    misses: u64,
}

struct Sample {
    run: usize,
    wall: Duration,
    exit: i32,
    misses: u64,
    /// `--timings` rows, for jrs.
    phases: Vec<(String, u64)>,
}

impl Cell {
    fn incomplete(&self) -> bool {
        self.misses > 0
    }

    fn timed(&self) -> bool {
        self.failure.is_none()
            && self.skipped.is_none()
            && !self.incomplete()
            && !self.samples.is_empty()
    }

    /// Min, median and max wall time.
    fn stats(&self) -> Option<(Duration, Duration, Duration)> {
        if !self.timed() {
            return None;
        }
        let mut walls: Vec<Duration> = self.samples.iter().map(|s| s.wall).collect();
        walls.sort();
        Some((walls[0], walls[walls.len() / 2], walls[walls.len() - 1]))
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "the checks, then the scenarios round-robin over the columns, in order"
)]
fn bench_project(project: &Project, env: &Env, settings: &Settings, tools: &Tools) -> Outcome {
    let work = env.run_dir.join("work").join(&project.name);
    let logs = env.run_dir.join("logs").join(&project.name);
    std::fs::create_dir_all(&logs).expect("create the project's logs");
    let mut outcome = Outcome {
        name: project.name.clone(),
        set: project.set,
        refused: None,
        notes: Vec::new(),
        cells: BTreeMap::new(),
        columns: Vec::new(),
        edits: Vec::new(),
        gradle_version: None,
        maven_version: None,
    };
    let template = work.join("template");
    if let Err(e) = materialize(project, &template) {
        outcome.refused = Some(format!("cannot set it up: {e}"));
        return outcome;
    }
    let edits = choose_edits(project, &template);
    outcome.edits = [
        edits
            .body
            .as_ref()
            .map(|e| format!("`incremental`, `test-edit`: {}", e.what)),
        edits.abi.as_ref().map(|e| format!("`abi`: {}", e.what)),
    ]
    .into_iter()
    .flatten()
    .collect();
    let columns: Vec<Column> = settings
        .columns
        .iter()
        .copied()
        .filter(|c| project.tools.contains(&c.tool()))
        .filter(|c| !tools.missing.contains_key(&c.tool()) || has_wrapper(&template, c.tool()))
        .collect();
    outcome.columns.clone_from(&columns);
    if has_wrapper(&template, Tool::Gradle) {
        outcome.gradle_version =
            wrapper_version(&template.join("gradle/wrapper/gradle-wrapper.properties"));
    }
    if has_wrapper(&template, Tool::Maven) {
        outcome.maven_version =
            wrapper_version(&template.join(".mvn/wrapper/maven-wrapper.properties"));
    }

    // §5.1: the same build on every tool, or no time.
    let checked = check(project, &columns, &template, &work, &logs, env);
    outcome.notes.extend(checked.notes);
    if let Some(refused) = checked.refused {
        println!("  {} not timed: {refused}", project.name);
        outcome.refused = Some(refused);
        cleanup(&work);
        return outcome;
    }
    println!("  {} checked: the same build on every tool", project.name);
    if settings.record {
        cleanup(&work);
        return outcome;
    }

    let mut lanes: Vec<Lane> = columns
        .iter()
        .map(|&column| Lane::new(column, project, &template, &work, &logs, env))
        .collect();
    for &scenario in &settings.scenarios {
        let mut cells: BTreeMap<Column, Cell> =
            columns.iter().map(|c| (*c, Cell::default())).collect();
        let skip = match scenario {
            "run" if project.run.is_none() => Some(
                project
                    .no_run
                    .unwrap_or("the project has no run probe")
                    .to_string(),
            ),
            "incremental" | "test-edit" if edits.body.is_none() => {
                Some("no source to edit".to_string())
            }
            "abi" if edits.abi.is_none() => Some("no source to edit".to_string()),
            _ => None,
        };
        if let Some(why) = skip {
            for cell in cells.values_mut() {
                cell.skipped = Some(why.clone());
            }
            outcome.cells.insert(scenario, cells);
            continue;
        }
        for round in 0..settings.warmup + settings.runs {
            let measured = round >= settings.warmup;
            for lane in &mut lanes {
                let cell = cells.get_mut(&lane.column).expect("a cell per column");
                if cell.failure.is_some() {
                    continue;
                }
                if let Err(e) = lane.prepare(scenario, &edits) {
                    cell.failure = Some(format!("preparing it: {e}"));
                    continue;
                }
                let sample = lane.timed(scenario, round);
                if let Some(e) = sample.failure {
                    cell.failure = Some(e);
                    continue;
                }
                if measured {
                    cell.misses += sample.sample.misses;
                    cell.samples.push(Sample {
                        run: round - settings.warmup + 1,
                        ..sample.sample
                    });
                }
            }
        }
        let line: Vec<String> = columns
            .iter()
            .map(|c| format!("{} {}", c.id(), cell_text(&cells[c], None)))
            .collect();
        println!("  {} {scenario}: {}", project.name, line.join(", "));
        outcome.cells.insert(scenario, cells);
    }
    for lane in &lanes {
        lane.stop_daemon();
    }
    cleanup(&work);
    outcome
}

fn cleanup(work: &Path) {
    let _ = std::fs::remove_dir_all(work);
}

fn wrapper_version(properties: &Path) -> Option<String> {
    let text = std::fs::read_to_string(properties).ok()?;
    let url = text
        .lines()
        .find_map(|l| l.trim().strip_prefix("distributionUrl="))?;
    let file = url.rsplit('/').next()?;
    let version = file
        .trim_start_matches("gradle-")
        .trim_start_matches("apache-maven-")
        .split(['-'])
        .next()?;
    Some(version.to_string())
}

/// A project copied into `to`: the example with its build files over it,
/// the generator's output, or the corpus clone without its build output.
fn materialize(project: &Project, to: &Path) -> Result<(), String> {
    let _ = std::fs::remove_dir_all(to);
    match &project.origin {
        Origin::Example(example, overlay) => {
            copy_tree(example, to)?;
            copy_tree(overlay, to)?;
        }
        Origin::Generated(generator) => generate(generator, to)?,
        Origin::Clone(clone) => copy_tree(clone, to)?,
    }
    Ok(())
}

/// Copy a tree, leaving out build output, IDE files and version control.
fn copy_tree(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(|e| format!("create {}: {e}", to.display()))?;
    let mut entries: Vec<_> = std::fs::read_dir(from)
        .map_err(|e| format!("read {}: {e}", from.display()))?
        .flatten()
        .collect();
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if [".git", ".idea", ".gradle", "target", "build", ".jrs"].contains(&&*name_str) {
            continue;
        }
        let target = to.join(&name);
        let kind = entry.file_type().map_err(|e| e.to_string())?;
        if kind.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else if kind.is_file() {
            // `std::fs::copy` keeps the permissions, so `mvnw` stays executable.
            std::fs::copy(entry.path(), &target)
                .map_err(|e| format!("copy {}: {e}", entry.path().display()))?;
        }
    }
    Ok(())
}

struct Edits {
    body: Option<Edit>,
    abi: Option<Edit>,
}

/// The edits the scenarios make. The generator marks them; elsewhere the
/// source with the fewest dependents by a grep for its simple name gets a
/// private method, which no other class can see, and the one with the most
/// a public one.
fn choose_edits(project: &Project, root: &Path) -> Edits {
    if let Origin::Generated(generator) = project.origin {
        let (leaf, hub) = generated_edit_targets(generator);
        return Edits {
            body: Some(Edit {
                file: leaf.clone(),
                from: BODY_MARK.to_string(),
                to: BODY_MARK.replace("v += 1;", "v += 2;"),
                what: format!(
                    "a method body changed in `{}`, which nothing else uses",
                    leaf.display()
                ),
            }),
            abi: Some(Edit {
                file: hub.clone(),
                from: ABI_MARK.to_string(),
                to: format!("{ABI_MARK}\n{ADDED_PUBLIC}"),
                what: format!(
                    "a public method added to `{}`, which every package reaches",
                    hub.display()
                ),
            }),
        };
    }
    let mut sources = Vec::new();
    main_sources(root, root, &mut sources);
    let texts: Vec<(PathBuf, String)> = sources
        .into_iter()
        .filter_map(|p| {
            let text = std::fs::read_to_string(root.join(&p)).ok()?;
            Some((p, text))
        })
        .filter(|(p, text)| {
            let name = p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            name != "package-info.java"
                && name != "module-info.java"
                && !text.contains("@interface")
                && text.trim_end().ends_with('}')
        })
        .collect();
    let dependents = |i: usize| -> usize {
        let stem = texts[i]
            .0
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        texts
            .iter()
            .enumerate()
            .filter(|(j, (_, text))| *j != i && contains_word(text, &stem))
            .count()
    };
    let counts: Vec<usize> = (0..texts.len()).map(dependents).collect();
    let pick = |want_most: bool| -> Option<usize> {
        (0..texts.len()).min_by_key(|&i| {
            let key = if want_most {
                usize::MAX - counts[i]
            } else {
                counts[i]
            };
            (key, texts[i].0.clone())
        })
    };
    let insert = |i: usize, method: &str, what: String| {
        let (file, text) = &texts[i];
        let close = text.rfind('}').unwrap_or(text.len());
        Edit {
            file: file.clone(),
            from: text[close..].to_string(),
            to: format!("{method}\n{}", &text[close..]),
            what,
        }
    };
    Edits {
        body: pick(false).map(|i| {
            insert(
                i,
                ADDED_PRIVATE,
                format!(
                    "a private method added to `{}`, which {} other source(s) name",
                    texts[i].0.display(),
                    counts[i]
                ),
            )
        }),
        abi: pick(true).map(|i| {
            insert(
                i,
                ADDED_PUBLIC,
                format!(
                    "a public method added to `{}`, which {} other source(s) name",
                    texts[i].0.display(),
                    counts[i]
                ),
            )
        }),
    }
}

const BODY_MARK: &str = "v += 1; // jrs-bench: body";
const ABI_MARK: &str = "    // jrs-bench: abi";
const ADDED_PUBLIC: &str = "    public static int jrsBenchAdded() {\n        return 0;\n    }";
const ADDED_PRIVATE: &str = "    private static int jrsBenchEdit() {\n        return 1;\n    }";

fn contains_word(text: &str, word: &str) -> bool {
    text.match_indices(word).any(|(at, _)| {
        let before = text[..at].chars().next_back();
        let after = text[at + word.len()..].chars().next();
        let boundary = |c: Option<char>| c.is_none_or(|c| !(c.is_alphanumeric() || c == '_'));
        boundary(before) && boundary(after)
    })
}

/// Every `.java` file under a `src/main/java`, relative to `root`.
fn main_sources(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            main_sources(root, &path, out);
        } else if path.extension().is_some_and(|e| e == "java")
            && path.strip_prefix(root).is_ok_and(|p| {
                p.to_string_lossy()
                    .replace('\\', "/")
                    .contains("src/main/java/")
            })
            && let Ok(relative) = path.strip_prefix(root)
        {
            out.push(relative.to_path_buf());
        }
    }
}

// ---------------------------------------------------------------------------
// The equivalence checks of §5.1

struct Checked {
    refused: Option<String>,
    notes: Vec<String>,
}

/// What one tool built, from cold caches: its tests, its runtime classpath,
/// and whether `run` and the package start.
struct Built {
    tool: Tool,
    tests: Option<Counts>,
    classpath: Option<Classpath>,
    failures: Vec<String>,
}

fn check(
    project: &Project,
    columns: &[Column],
    template: &Path,
    work: &Path,
    logs: &Path,
    env: &Env,
) -> Checked {
    let mut tools: Vec<Column> = Vec::new();
    for column in columns {
        // The Gradle columns build the same thing; one checks for all three.
        let column = if column.tool() == Tool::Gradle {
            Column::GradleNoDaemon
        } else {
            *column
        };
        if !tools.contains(&column) {
            tools.push(column);
        }
    }
    let mut built = Vec::new();
    for column in tools {
        let lane = Lane::new(
            column,
            project,
            template,
            &work.join("check"),
            &logs.join("check"),
            env,
        );
        let result = check_tool(&lane, project, env);
        println!(
            "  {} check {}: {}",
            project.name,
            column.tool().name(),
            if result.failures.is_empty() {
                "ok".to_string()
            } else {
                result.failures.join("; ")
            }
        );
        lane.stop_daemon();
        let _ = std::fs::remove_dir_all(&lane.dir);
        built.push(result);
    }
    let mut problems: Vec<String> = Vec::new();
    let mut notes = Vec::new();
    for b in &built {
        for f in &b.failures {
            problems.push(format!("{}: {f}", b.tool.name()));
        }
    }
    let reference = built.iter().find(|b| b.tool == Tool::Jrs);
    if let Some(jrs) = reference {
        for other in built.iter().filter(|b| b.tool != Tool::Jrs) {
            if let (Some(ours), Some(theirs)) = (&jrs.classpath, &other.classpath) {
                let diff = compare_classpaths(ours, theirs, project);
                if !diff.missing.is_empty() || !diff.extra.is_empty() {
                    problems.push(format!(
                        "the runtime classpath differs from {}'s: {}",
                        other.tool.name(),
                        diff.describe()
                    ));
                } else if !diff.versions.is_empty() {
                    if project.versions_may_differ && other.tool == Tool::Gradle {
                        notes.push(format!(
                            "the runtime classpath has {} version(s) other than Gradle's, \
                             as its highest-wins mediation picks (CORPUS.md §4.1): {}",
                            diff.versions.len(),
                            diff.describe()
                        ));
                    } else {
                        problems.push(format!(
                            "the runtime classpath differs from {}'s: {}",
                            other.tool.name(),
                            diff.describe()
                        ));
                    }
                }
            }
            if let (Some(ours), Some(theirs)) = (jrs.tests, other.tests)
                && ours != theirs
            {
                problems.push(format!(
                    "the tests differ: jrs {ours}; {} {theirs}",
                    other.tool.name()
                ));
            }
        }
    }
    let totals: Vec<String> = built
        .iter()
        .filter_map(|b| b.tests.map(|t| format!("{} {t}", b.tool.name())))
        .collect();
    if !totals.is_empty() && problems.is_empty() {
        notes.push(format!(
            "checked: the same runtime classpath and tests ({})",
            built
                .iter()
                .find_map(|b| b.tests)
                .map(|t| t.to_string())
                .unwrap_or_default()
        ));
    }
    Checked {
        refused: (!problems.is_empty()).then(|| problems.join("; ")),
        notes,
    }
}

fn check_tool(lane: &Lane, project: &Project, env: &Env) -> Built {
    let mut built = Built {
        tool: lane.column.tool(),
        tests: None,
        classpath: None,
        failures: Vec::new(),
    };
    let log = lane.logs.join("check.log");
    // As `cold` starts: a shipped lockfile would spare jrs the resolution,
    // and a recording run would then miss the POMs `cold` asks for.
    let _ = std::fs::remove_file(lane.tree.join("jrs.lock"));
    let tested = lane.exec(Action::TestAll, &log);
    if !tested.success() {
        built.failures.push(format!(
            "`test` {} (log: {})",
            tested.describe(),
            log.display()
        ));
        return built;
    }
    let tests = Tests::collect(&lane.tree, lane.column.tool().reports(), &[]);
    let total = tests.total();
    if total.failed > 0 {
        built.failures.push(format!("the tests fail: {total}"));
    }
    built.tests = Some(total);

    let out = lane.dir.join("classpath.txt");
    let listed = lane.exec_with(Action::Classpath, &log, &out);
    if listed.success() {
        built.classpath = Some(match lane.column.tool() {
            Tool::Jrs => jrs_classpath(&out),
            Tool::Maven => maven_classpath(&out),
            Tool::Gradle => gradle_classpath(&out),
        });
    } else {
        built.failures.push(format!(
            "the classpath {} (log: {})",
            listed.describe(),
            log.display()
        ));
    }

    if let Some(probe) = &project.run {
        let run_log = lane.logs.join("check-run.log");
        if let Err(e) = started(&mut lane.command(Action::Run), probe, &run_log) {
            built
                .failures
                .push(format!("`run`: {e} (log: {})", run_log.display()));
        }
    }

    let packaged = lane.exec(Action::Package, &log);
    if !packaged.success() {
        built.failures.push(format!(
            "`package` {} (log: {})",
            packaged.describe(),
            log.display()
        ));
        return built;
    }
    let probe = project.run.clone().or_else(|| match &project.origin {
        Origin::Generated(g) => Some(Probe::OwnedStdout(format!("{} ok", g.name))),
        _ => None,
    });
    match (package_jar(lane, project), probe) {
        (None, _) => built
            .failures
            .push("`package` wrote no jar the harness can find".into()),
        (Some(_), None) => {}
        (Some(jar), Some(probe)) => {
            let jar_log = lane.logs.join("check-package.log");
            let mut command = Command::new(env.java());
            command.arg("-jar").arg(&jar).current_dir(&lane.tree);
            if let Err(e) = started(&mut command, &probe, &jar_log) {
                built.failures.push(format!(
                    "`java -jar {}`: {e} (log: {})",
                    jar.display(),
                    jar_log.display()
                ));
            }
        }
    }
    built
}

/// The jar the project ships, by each tool's layout: jrs's
/// `<name>-<version>.jar`, Maven's shaded or repackaged jar (never
/// `original-*`), Gradle's `-all` shadow jar or Boot's `bootJar` (never
/// `-plain`).
fn package_jar(lane: &Lane, project: &Project) -> Option<PathBuf> {
    let base = match &project.member {
        Some(member) => lane.tree.join(member),
        None => lane.tree.clone(),
    };
    let dirs: Vec<PathBuf> = match lane.column.tool() {
        Tool::Jrs => vec![base.join("target"), lane.tree.join("target")],
        Tool::Maven => vec![base.join("target")],
        Tool::Gradle => vec![base.join("build/libs")],
    };
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut jars: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "jar"))
            .filter(|p| {
                let name = p
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                !name.starts_with("original-")
                    && !name.ends_with("-plain.jar")
                    && !name.ends_with("-sources.jar")
                    && !name.ends_with("-javadoc.jar")
                    && !name.ends_with("-tests.jar")
            })
            .filter(|p| {
                project.member.as_ref().is_none_or(|m| {
                    p.file_name()
                        .is_some_and(|n| n.to_string_lossy().starts_with(m.as_str()))
                })
            })
            .collect();
        jars.sort_by_key(|p| {
            let all = p.to_string_lossy().ends_with("-all.jar");
            (!all, p.clone())
        });
        if let Some(jar) = jars.into_iter().next() {
            return Some(jar);
        }
    }
    None
}

/// `group:artifact[:classifier]` → version.
type Classpath = BTreeMap<String, String>;

fn key(group: &str, artifact: &str, classifier: Option<&str>) -> String {
    match classifier {
        Some(c) if !c.is_empty() => format!("{group}:{artifact}:{c}"),
        _ => format!("{group}:{artifact}"),
    }
}

/// `classpaths.runtime` of `jrs metadata`, across every member of a
/// workspace, whose members are class directories and so not listed.
fn jrs_classpath(out: &Path) -> Classpath {
    let mut classpath = Classpath::new();
    let Some(model) = std::fs::read_to_string(out)
        .ok()
        .and_then(|t| Json::parse(&t).ok())
    else {
        return classpath;
    };
    let projects: Vec<&Json> = match model.get("members") {
        Some(list) => list.items().iter().collect(),
        None => vec![&model],
    };
    for project in projects {
        let runtime = project
            .get("classpaths")
            .and_then(|c| c.get("runtime"))
            .map_or(&[][..], Json::items);
        for jar in runtime {
            if let Some(coordinate) = jar.get("coordinate").and_then(Json::as_str) {
                let parts: Vec<&str> = coordinate.split(':').collect();
                let (group, artifact, version, classifier) = match parts.as_slice() {
                    [g, a, v] => (*g, *a, *v, None),
                    [g, a, v, c] => (*g, *a, *v, Some(*c)),
                    _ => continue,
                };
                classpath.insert(key(group, artifact, classifier), version.to_string());
            }
        }
    }
    classpath
}

/// `dependency:list` lines: `g:a:type[:classifier]:v:scope`, with a
/// ` -- module …` suffix on newer plugins.
fn maven_classpath(out: &Path) -> Classpath {
    let text = std::fs::read_to_string(out).unwrap_or_default();
    let mut classpath = Classpath::new();
    for line in text.lines() {
        let line = line.split(" -- ").next().unwrap_or(line).trim();
        let parts: Vec<&str> = line.split(':').collect();
        let (group, artifact, classifier, version) = match parts.as_slice() {
            [g, a, kind, v, _scope] if is_jar(kind) => (*g, *a, None, *v),
            [g, a, kind, c, v, _scope] if is_jar(kind) => (*g, *a, Some(*c), *v),
            _ => continue,
        };
        classpath.insert(key(group, artifact, classifier), version.to_string());
    }
    classpath
}

fn is_jar(kind: &str) -> bool {
    kind == "jar" || kind == "bundle"
}

/// The init script's `g:a:v[:classifier]` lines.
fn gradle_classpath(out: &Path) -> Classpath {
    let text = std::fs::read_to_string(out).unwrap_or_default();
    let mut classpath = Classpath::new();
    for line in text.lines() {
        let parts: Vec<&str> = line.trim().split(':').collect();
        let (group, artifact, version, classifier) = match parts.as_slice() {
            [g, a, v] => (*g, *a, *v, None),
            [g, a, v, c] => (*g, *a, *v, Some(*c)),
            _ => continue,
        };
        classpath.insert(key(group, artifact, classifier), version.to_string());
    }
    classpath
}

struct ClasspathDiff {
    /// The other tool's, not jrs's.
    missing: Vec<String>,
    /// jrs's, not the other tool's.
    extra: Vec<String>,
    /// `key jrs's -> theirs`.
    versions: Vec<String>,
}

impl ClasspathDiff {
    fn describe(&self) -> String {
        let mut parts = Vec::new();
        if !self.missing.is_empty() {
            parts.push(format!("jrs lacks {}", self.missing.join(", ")));
        }
        if !self.extra.is_empty() {
            parts.push(format!("only jrs has {}", self.extra.join(", ")));
        }
        if !self.versions.is_empty() {
            parts.push(format!("versions {}", self.versions.join(", ")));
        }
        parts.join("; ")
    }
}

/// The members' own jars are left out: jrs puts their class directories on
/// the classpath, and Maven and Gradle their jars or modules.
fn compare_classpaths(ours: &Classpath, theirs: &Classpath, project: &Project) -> ClasspathDiff {
    let generated = matches!(project.origin, Origin::Generated(_));
    let member = |k: &str| {
        let mut parts = k.split(':');
        let group = parts.next().unwrap_or_default();
        let artifact = parts.next().unwrap_or_default();
        project.members.iter().any(|m| m == artifact) && (!generated || group == "bench")
    };
    let theirs: Classpath = theirs
        .iter()
        .filter(|(k, _)| !member(k))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let mut diff = ClasspathDiff {
        missing: Vec::new(),
        extra: Vec::new(),
        versions: Vec::new(),
    };
    for (k, v) in &theirs {
        match ours.get(k) {
            None => diff.missing.push(format!("{k}:{v}")),
            Some(o) if o != v => diff.versions.push(format!("{k} {o} -> {v}")),
            Some(_) => {}
        }
    }
    for (k, v) in ours {
        if !theirs.contains_key(k) {
            diff.extra.push(format!("{k}:{v}"));
        }
    }
    diff
}

/// Start `command`, wait for the probe, and stop its whole process group.
/// `Ok` is the time from the spawn to the probe's answer.
fn started(command: &mut Command, probe: &Probe, log: &Path) -> Result<Duration, String> {
    if let Some((url, _)) = probe.url() {
        wait_for_port_free(url);
        if let Some(status) = http_status(url) {
            return Err(format!(
                "{url} already answers {status} before the application starts; is the port in use?"
            ));
        }
    }
    let offset = std::fs::metadata(log).map_or(0, |m| m.len());
    let start = Instant::now();
    let mut child = spawn(command, log, None).map_err(|e| format!("cannot start: {e}"))?;
    let outcome = wait_for_probe(&mut child, probe, log, offset, start);
    stop(&mut child);
    if let Some((url, _)) = probe.url() {
        wait_for_port_free(url);
    }
    outcome
}

fn wait_for_probe(
    child: &mut Child,
    probe: &Probe,
    log: &Path,
    offset: u64,
    start: Instant,
) -> Result<Duration, String> {
    let deadline = start + PROBE_TIMEOUT;
    let mut last_http = start;
    loop {
        let exited = child.try_wait().ok().flatten();
        if let Some(text) = probe.stdout()
            && let Ok(bytes) = std::fs::read(log)
            && usize::try_from(offset)
                .ok()
                .and_then(|o| bytes.get(o..))
                .is_some_and(|new| String::from_utf8_lossy(new).contains(text))
        {
            return Ok(start.elapsed());
        }
        if let Some((url, expect)) = probe.url()
            && last_http.elapsed() >= Duration::from_millis(25)
        {
            last_http = Instant::now();
            match http_status(url) {
                Some(status) if status == expect => return Ok(start.elapsed()),
                Some(status) => return Err(format!("{url} answered {status}, not {expect}")),
                None => {}
            }
        }
        if let Some(status) = exited {
            return Err(format!("it exited ({status}) before the probe answered"));
        }
        if Instant::now() > deadline {
            return Err(format!(
                "the probe did not answer within {} s",
                PROBE_TIMEOUT.as_secs()
            ));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Wait, up to half a minute, for nothing to listen on the probe's port.
fn wait_for_port_free(url: &str) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while http_status(url).is_some() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(200));
    }
}

// ---------------------------------------------------------------------------
// One column's copy of a project

#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    Build,
    Test,
    TestAll,
    Run,
    Package,
    Classpath,
}

/// One column's copy of a project, its caches and its state between runs.
struct Lane {
    column: Column,
    flavor: Flavor,
    member: Option<String>,
    probe: Option<Probe>,
    server: Server,
    /// Holds `tree/`, `cache/` and the classpath file.
    dir: PathBuf,
    tree: PathBuf,
    cache: PathBuf,
    logs: PathBuf,
    program: PathBuf,
    jrs_config: PathBuf,
    maven_settings: PathBuf,
    gradle_init: PathBuf,
    gradle_dists: PathBuf,
    maven_dists: PathBuf,
    java_home: PathBuf,
    /// The main classes are compiled and up to date.
    built: bool,
    /// The test classes too.
    tested: bool,
    /// The files an edit changed, with their original text.
    edited: Vec<(PathBuf, String)>,
}

struct Timed {
    sample: Sample,
    failure: Option<String>,
}

impl Lane {
    fn new(
        column: Column,
        project: &Project,
        template: &Path,
        work: &Path,
        logs: &Path,
        env: &Env,
    ) -> Lane {
        let dir = work.join(column.id());
        let tree = dir.join("tree");
        let _ = std::fs::remove_dir_all(&dir);
        copy_tree(template, &tree).expect("copy the project");
        std::fs::create_dir_all(logs).expect("create the logs");
        let program = match column.tool() {
            Tool::Jrs => PathBuf::from(env!("CARGO_BIN_EXE_jrs")),
            Tool::Maven if has_wrapper(&tree, Tool::Maven) => tree.join("mvnw"),
            Tool::Maven => env.mvn.clone().unwrap_or_else(|| PathBuf::from("mvn")),
            Tool::Gradle if has_wrapper(&tree, Tool::Gradle) => tree.join("gradlew"),
            Tool::Gradle => env
                .gradle
                .clone()
                .unwrap_or_else(|| PathBuf::from("gradle")),
        };
        let lane = Lane {
            column,
            flavor: project.flavor,
            member: project.member.clone(),
            probe: project.run.clone(),
            server: env.server.clone(),
            cache: dir.join("cache"),
            dir,
            tree,
            logs: logs.join(column.id()),
            program,
            jrs_config: env.jrs_config.clone(),
            maven_settings: env.maven_settings.clone(),
            gradle_init: env.gradle_init.clone(),
            gradle_dists: env.gradle_dists.clone(),
            maven_dists: env.maven_dists.clone(),
            java_home: env.java_home.clone(),
            built: false,
            tested: false,
            edited: Vec::new(),
        };
        std::fs::create_dir_all(&lane.logs).expect("create the column's logs");
        lane.empty_caches();
        lane
    }

    /// The column's caches, empty, with only what is seeded: a wrapper's
    /// distribution and Gradle's settings.
    fn empty_caches(&self) {
        let _ = std::fs::remove_dir_all(&self.cache);
        std::fs::create_dir_all(&self.cache).expect("create the cache");
        match self.column.tool() {
            Tool::Jrs => {}
            Tool::Maven => {
                let wrapper = self.cache.join("m2/wrapper");
                std::fs::create_dir_all(&wrapper).expect("create the Maven home");
                std::fs::create_dir_all(&self.maven_dists).expect("create the Maven distributions");
                link(&self.maven_dists, &wrapper.join("dists"));
            }
            Tool::Gradle => {
                let wrapper = self.cache.join("wrapper");
                std::fs::create_dir_all(&wrapper).expect("create the Gradle home");
                std::fs::create_dir_all(&self.gradle_dists)
                    .expect("create the Gradle distributions");
                link(&self.gradle_dists, &wrapper.join("dists"));
                std::fs::write(
                    self.cache.join("gradle.properties"),
                    "# Written by benches/compare.rs.\n\
                     org.gradle.java.installations.auto-download=false\n\
                     org.gradle.welcome=never\n",
                )
                .expect("write the Gradle home's properties");
            }
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one table of every action for each of the three tools"
    )]
    fn command(&self, action: Action) -> Command {
        let mut command = Command::new(&self.program);
        command
            .current_dir(&self.tree)
            .env("JAVA_HOME", &self.java_home);
        let member: Vec<String> = self
            .member
            .iter()
            .flat_map(|m| ["-p".to_string(), m.clone()])
            .collect();
        match self.column.tool() {
            Tool::Jrs => {
                command
                    .env("JRS_CACHE_DIR", &self.cache)
                    .env("JRS_CONFIG", &self.jrs_config)
                    .args(["--progress", "never", "--color", "never"]);
                match action {
                    Action::Build => command.args(["build", "--no-build-cache", "--timings"]),
                    Action::Test => command.args(["test", "--no-build-cache", "--timings"]),
                    Action::TestAll => {
                        command.args(["test", "--all", "--no-build-cache", "--timings"])
                    }
                    Action::Run => command.args(&member).args(["run", "--no-build-cache"]),
                    // At the root, as Maven's reactor packages: a library
                    // member gets its plain jar, the application its fat
                    // one. `-p` would ask the library for a fat jar too.
                    Action::Package => {
                        command.args(["package", "--fat", "--no-build-cache", "--timings"])
                    }
                    Action::Classpath => command.arg("metadata"),
                };
            }
            Tool::Maven => {
                command
                    .env("MAVEN_USER_HOME", self.cache.join("m2"))
                    .arg("-B")
                    .arg("-s")
                    .arg(&self.maven_settings)
                    .arg(format!(
                        "-Dmaven.repo.local={}",
                        self.cache.join("repository").display()
                    ))
                    .arg("-Dmaven.build.cache.enabled=false");
                match action {
                    Action::Build => command.arg("compile"),
                    Action::Test | Action::TestAll => command.arg("test"),
                    Action::Run => match self.flavor {
                        Flavor::Plain => command.arg("exec:java"),
                        Flavor::SpringBoot => command.arg("spring-boot:run"),
                    },
                    Action::Package => command.args(["package", "-DskipTests"]),
                    Action::Classpath => command
                        .arg("compile")
                        .arg(format!("{DEPENDENCY_PLUGIN}:list"))
                        .args([
                            "-DincludeScope=runtime",
                            "-DexcludeTransitive=false",
                            "-DappendOutput=true",
                            "-DoutputAbsoluteArtifactFilename=false",
                        ])
                        .arg(format!(
                            "-DoutputFile={}",
                            self.dir.join("classpath.txt").display()
                        )),
                };
            }
            Tool::Gradle => {
                command
                    .env("GRADLE_USER_HOME", &self.cache)
                    .arg("--console=plain")
                    .arg("--init-script")
                    .arg(&self.gradle_init)
                    .args(["--no-build-cache", "-Dorg.gradle.caching=false"]);
                match self.column {
                    Column::GradleCc => {
                        command.arg("--configuration-cache");
                    }
                    Column::GradleNoDaemon => {
                        command.arg("--no-daemon");
                    }
                    _ => {}
                }
                let task = |name: &str| match &self.member {
                    Some(m) => format!(":{m}:{name}"),
                    None => name.to_string(),
                };
                match action {
                    Action::Build => command.arg("classes"),
                    Action::Test => command.arg("test"),
                    Action::TestAll => command.args(["test", "--rerun"]),
                    Action::Run => match self.flavor {
                        Flavor::Plain => command.arg(task("run")),
                        Flavor::SpringBoot => command.arg(task("bootRun")),
                    },
                    Action::Package => match self.flavor {
                        Flavor::Plain => command.arg(task("shadowJar")),
                        Flavor::SpringBoot => command.arg(task("bootJar")),
                    },
                    Action::Classpath => command
                        .args(["--no-configuration-cache", "jrsBenchClasspath"])
                        .arg(format!(
                            "-PjrsBenchClasspath={}",
                            self.dir.join("classpath.txt").display()
                        )),
                };
            }
        }
        command
    }

    fn exec(&self, action: Action, log: &Path) -> Exec {
        proc::exec(&mut self.command(action), log, None, COMMAND_TIMEOUT)
    }

    /// For [`Action::Classpath`]: jrs prints the model to stdout, the
    /// others write the file themselves.
    fn exec_with(&self, action: Action, log: &Path, out: &Path) -> Exec {
        let _ = std::fs::remove_file(out);
        let stdout = (self.column.tool() == Tool::Jrs).then_some(out);
        proc::exec(&mut self.command(action), log, stdout, COMMAND_TIMEOUT)
    }

    fn log(&self, scenario: &str) -> PathBuf {
        self.logs.join(format!("{scenario}.log"))
    }

    /// Put the copy in the scenario's state, untimed (BENCHMARKS.md §4).
    fn prepare(&mut self, scenario: &str, edits: &Edits) -> Result<(), String> {
        self.restore()?;
        match scenario {
            "cold" => {
                self.stop_daemon();
                self.empty_caches();
                self.remove_output(true);
                let _ = std::fs::remove_file(self.tree.join("jrs.lock"));
            }
            "clean" => self.remove_output(false),
            "noop" | "run" => self.ensure_built(scenario)?,
            "incremental" => {
                self.ensure_built(scenario)?;
                self.apply(edits.body.as_ref())?;
            }
            "abi" => {
                self.ensure_built(scenario)?;
                self.apply(edits.abi.as_ref())?;
            }
            "test" => {
                self.ensure_tested(scenario)?;
                self.remove_test_results();
            }
            "test-edit" => {
                self.ensure_tested(scenario)?;
                self.apply(edits.body.as_ref())?;
            }
            "package" => self.ensure_tested(scenario)?,
            _ => unreachable!("an unknown scenario"),
        }
        Ok(())
    }

    fn ensure_built(&mut self, scenario: &str) -> Result<(), String> {
        if self.built {
            return Ok(());
        }
        let done = self.exec(Action::Build, &self.log(scenario));
        if !done.success() {
            return Err(format!("the build {}", done.describe()));
        }
        self.built = true;
        Ok(())
    }

    fn ensure_tested(&mut self, scenario: &str) -> Result<(), String> {
        if self.tested {
            return Ok(());
        }
        let done = self.exec(Action::TestAll, &self.log(scenario));
        if !done.success() {
            return Err(format!("the tests {}", done.describe()));
        }
        self.built = true;
        self.tested = true;
        Ok(())
    }

    fn apply(&mut self, edit: Option<&Edit>) -> Result<(), String> {
        let Some(edit) = edit else {
            return Err("no edit".into());
        };
        let path = self.tree.join(&edit.file);
        let original = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
        let Some(at) = original.rfind(&edit.from) else {
            return Err(format!("`{}` is not in {}", edit.from, edit.file.display()));
        };
        let edited = format!(
            "{}{}{}",
            &original[..at],
            edit.to,
            &original[at + edit.from.len()..]
        );
        std::fs::write(&path, edited).map_err(|e| e.to_string())?;
        self.edited.push((path, original));
        self.built = false;
        self.tested = false;
        Ok(())
    }

    /// Undo the last edit, so every run makes the same one. The original
    /// is newer than the classes now, so the next preparation builds again.
    fn restore(&mut self) -> Result<(), String> {
        for (path, original) in self.edited.drain(..) {
            std::fs::write(&path, original).map_err(|e| e.to_string())?;
            self.built = false;
            self.tested = false;
        }
        Ok(())
    }

    /// Every `target/` and `build/` in the copy; with `all`, also Gradle's
    /// `.gradle/` (file hashes and the configuration cache), which a clean
    /// build keeps, as `gradle clean` does.
    fn remove_output(&mut self, all: bool) {
        remove_named(&self.tree, &["target", "build"], all);
        self.built = false;
        self.tested = false;
    }

    fn remove_test_results(&self) {
        let dirs: &[&str] = match self.column.tool() {
            Tool::Jrs => &["test-reports"],
            Tool::Maven => &["surefire-reports"],
            Tool::Gradle => &["test-results"],
        };
        remove_named(&self.tree, dirs, false);
    }

    /// One timed command, from spawn to exit, or to the probe for `run`.
    fn timed(&mut self, scenario: &str, round: usize) -> Timed {
        let action = match scenario {
            "test" => Action::TestAll,
            "test-edit" => Action::Test,
            "run" => Action::Run,
            "package" => Action::Package,
            _ => Action::Build,
        };
        let log = self.log(scenario);
        let _ = std::fs::remove_file(self.tree.join("target/.jrs/timings.txt"));
        let before = self.server.counts().1;
        let (wall, exit, failure) = if action == Action::Run {
            match self.probe.clone() {
                Some(probe) => match started(&mut self.command(Action::Run), &probe, &log) {
                    Ok(wall) => (wall, 0, None),
                    Err(e) => (
                        Duration::ZERO,
                        -1,
                        Some(format!("run {round}: {e} (log: {})", log.display())),
                    ),
                },
                None => (Duration::ZERO, -1, Some("no probe".to_string())),
            }
        } else {
            let done = self.exec(action, &log);
            let exit = done.status.and_then(|s| s.code()).unwrap_or(-1);
            let failure = (!done.success())
                .then(|| format!("run {round} {} (log: {})", done.describe(), log.display()));
            (done.wall, exit, failure)
        };
        let misses = self.server.counts().1 - before;
        if failure.is_none() {
            match action {
                Action::Build | Action::Package => self.built = true,
                Action::Test | Action::TestAll => {
                    self.built = true;
                    self.tested = true;
                }
                Action::Run | Action::Classpath => {}
            }
        }
        let phases = if self.column == Column::Jrs {
            read_timings(&self.tree.join("target/.jrs/timings.txt"))
        } else {
            Vec::new()
        };
        Timed {
            sample: Sample {
                run: round,
                wall,
                exit,
                misses,
                phases,
            },
            failure,
        }
    }

    /// `gradle --stop` for this column's home, so no daemon outlives it.
    fn stop_daemon(&self) {
        if self.column.tool() != Tool::Gradle {
            return;
        }
        let mut command = Command::new(&self.program);
        command
            .current_dir(&self.tree)
            .env("JAVA_HOME", &self.java_home)
            .env("GRADLE_USER_HOME", &self.cache)
            .arg("--stop");
        let _ = proc::exec(
            &mut command,
            &self.logs.join("stop.log"),
            None,
            Duration::from_mins(2),
        );
    }
}

fn link(target: &Path, at: &Path) {
    let _ = std::fs::remove_dir_all(at);
    let _ = std::fs::remove_file(at);
    #[cfg(unix)]
    {
        let _ = std::os::unix::fs::symlink(target, at);
    }
    #[cfg(not(unix))]
    {
        let _ = std::fs::create_dir_all(at);
        let _ = target;
    }
}

/// Remove every directory called one of `names` under `dir`, and with
/// `gradle_state` every `.gradle/` too. A source directory is never one:
/// the walk does not go below `src/`.
fn remove_named(dir: &Path, names: &[&str], gradle_state: bool) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let path = entry.path();
        if names.contains(&&*name) || (gradle_state && name == ".gradle") {
            let _ = std::fs::remove_dir_all(&path);
        } else if name != "src" && name != ".git" {
            remove_named(&path, names, gradle_state);
        }
    }
}

/// The `phase\tms` rows of `--timings`, without the header and `total`.
fn read_timings(path: &Path) -> Vec<(String, u64)> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.starts_with('#') && *l != "phase\tms")
        .filter_map(|l| {
            let (label, ms) = l.rsplit_once('\t')?;
            Some((label.to_string(), ms.parse().ok()?))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// The generator of §2

/// A small deterministic generator (`SplitMix64`), so every run of the
/// harness writes the same project.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % n.max(1) as u64).unwrap_or(0)
    }
}

/// One module of a generated project: the root of a single project, or a
/// workspace member.
struct Module {
    name: String,
    /// `""` at the root.
    dir: String,
    upstream: Vec<String>,
    classes: usize,
    packages: usize,
    dependencies: Vec<(&'static str, &'static str)>,
    /// Whether it holds `bench.Main`.
    app: bool,
}

fn modules(generator: &Gen) -> Vec<Module> {
    let deps = &DEPENDENCIES[..generator.dependencies.min(DEPENDENCIES.len())];
    if generator.members.is_empty() {
        return vec![Module {
            name: generator.name.to_string(),
            dir: String::new(),
            upstream: Vec::new(),
            classes: generator.classes,
            packages: generator.packages,
            dependencies: deps.to_vec(),
            app: true,
        }];
    }
    let n = generator.members.len();
    generator
        .members
        .iter()
        .enumerate()
        .map(|(i, (name, upstream))| Module {
            name: (*name).to_string(),
            dir: (*name).to_string(),
            upstream: upstream.iter().map(|u| (*u).to_string()).collect(),
            classes: generator.classes / n,
            packages: (generator.packages / n).max(1),
            dependencies: deps
                .iter()
                .enumerate()
                .filter(|(d, _)| d % n == i)
                .map(|(_, d)| *d)
                .collect(),
            app: i == n - 1,
        })
        .collect()
}

fn package_name(module: &Module, workspace: bool, p: usize) -> String {
    if workspace {
        format!("bench.{}.p{p:02}", module.name)
    } else {
        format!("bench.p{p:02}")
    }
}

/// The package and class of a module's class `k`.
fn class_of(module: &Module, workspace: bool, k: usize) -> (String, String) {
    let per = module.classes.div_ceil(module.packages);
    (package_name(module, workspace, k / per), format!("C{k:04}"))
}

fn source_file(module: &Module, workspace: bool, k: usize, test: bool) -> PathBuf {
    let (package, class) = class_of(module, workspace, k);
    let mut path = PathBuf::from(&module.dir);
    path.push(if test {
        "src/test/java"
    } else {
        "src/main/java"
    });
    for part in package.split('.') {
        path.push(part);
    }
    path.push(format!("{class}{}.java", if test { "Test" } else { "" }));
    path
}

/// The leaf whose body `incremental` changes, and the hub `abi` adds to.
fn generated_edit_targets(generator: &Gen) -> (PathBuf, PathBuf) {
    let modules = modules(generator);
    let workspace = !generator.members.is_empty();
    let app = modules.last().expect("a module");
    let core = modules.first().expect("a module");
    (
        source_file(app, workspace, app.classes - 1, false),
        source_file(core, workspace, 0, false),
    )
}

/// Write the project of `generator` into `root`: sources, tests, and the
/// three build files from one description, so they cannot drift.
fn generate(generator: &Gen, root: &Path) -> Result<(), String> {
    let workspace = !generator.members.is_empty();
    let modules = modules(generator);
    let mut rng = Rng(0x6A72_7362_656E_6368 ^ generator.classes as u64);
    let write = |path: PathBuf, text: String| -> Result<(), String> {
        let path = root.join(path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&path, text).map_err(|e| format!("write {}: {e}", path.display()))
    };
    let last = modules.len() - 1;
    for (m, module) in modules.iter().enumerate() {
        let per = module.classes.div_ceil(module.packages);
        for k in 0..module.classes {
            let first = k - k % per;
            let mut calls: Vec<String> = Vec::new();
            if k == 0 {
                for up in &module.upstream {
                    let upstream = modules.iter().find(|u| &u.name == up).expect("a member");
                    let (package, class) = class_of(upstream, workspace, 0);
                    calls.push(format!("{package}.{class}"));
                }
            } else if k == first {
                let (package, class) = class_of(module, workspace, 0);
                calls.push(format!("{package}.{class}"));
            } else {
                let (package, class) = class_of(module, workspace, first);
                calls.push(format!("{package}.{class}"));
                for _ in 0..2 {
                    let span = k - first;
                    let j = first + rng.below(span);
                    let (package, class) = class_of(module, workspace, j);
                    let call = format!("{package}.{class}");
                    if !calls.contains(&call) {
                        calls.push(call);
                    }
                }
            }
            let dependency = (!module.dependencies.is_empty())
                .then(|| module.dependencies[k % module.dependencies.len()].1);
            let leaf = m == last && k == module.classes - 1;
            let hub = m == 0 && k == 0;
            write(
                source_file(module, workspace, k, false),
                class_source(module, workspace, k, &calls, dependency, leaf, hub),
            )?;
            if k % 10 == 0 {
                write(
                    source_file(module, workspace, k, true),
                    test_source(module, workspace, k),
                )?;
            }
        }
        if module.app {
            write(
                PathBuf::from(&module.dir).join("src/main/java/bench/Main.java"),
                main_source(generator, &modules, workspace),
            )?;
        }
    }
    write_build_files(generator, &modules, workspace, &write)
}

fn class_source(
    module: &Module,
    workspace: bool,
    k: usize,
    calls: &[String],
    dependency: Option<&str>,
    leaf: bool,
    hub: bool,
) -> String {
    let (package, class) = class_of(module, workspace, k);
    let mut body = String::new();
    for call in calls {
        let _ = writeln!(body, "        v += {call}.value(x - 1);");
    }
    if let Some(dependency) = dependency {
        let _ = writeln!(
            body,
            "        v += {dependency}.class.getSimpleName().length();"
        );
    }
    if leaf {
        let _ = writeln!(body, "        {BODY_MARK}");
    }
    let abi = if hub {
        format!("\n{ABI_MARK}\n")
    } else {
        String::new()
    };
    format!(
        "package {package};

/** Generated by benches/compare.rs. */
public final class {class} {{
    private {class}() {{}}

    public static int value(int x) {{
        if (x <= 0) {{
            return {k};
        }}
        int v = x * 31 + {k};
{body}        return v;
    }}

    public static String describe(int x) {{
        StringBuilder out = new StringBuilder(\"{class}:\");
        for (int i = 0; i < x; i++) {{
            out.append(i * {seed} % 7).append(',');
        }}
        return out.toString();
    }}
{abi}}}
",
        seed = k % 13 + 1
    )
}

fn test_source(module: &Module, workspace: bool, k: usize) -> String {
    let (package, class) = class_of(module, workspace, k);
    format!(
        "package {package};

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertTrue;

import org.junit.jupiter.api.Test;

/** Generated by benches/compare.rs. */
class {class}Test {{
    @Test
    void valueIsStable() {{
        assertEquals({class}.value(2), {class}.value(2));
    }}

    @Test
    void describeNamesTheClass() {{
        assertTrue({class}.describe(3).startsWith(\"{class}:\"));
    }}
}}
"
    )
}

fn main_source(generator: &Gen, modules: &[Module], workspace: bool) -> String {
    let app = modules.last().expect("a module");
    let mut sum = String::new();
    for k in 0..app.classes {
        let (package, class) = class_of(app, workspace, k);
        let _ = writeln!(sum, "        sum += {package}.{class}.value(2);");
    }
    for module in &modules[..modules.len() - 1] {
        let (package, class) = class_of(module, workspace, 0);
        let _ = writeln!(sum, "        sum += {package}.{class}.value(2);");
    }
    format!(
        "package bench;

/** Generated by benches/compare.rs. */
public final class Main {{
    private Main() {{}}

    public static void main(String[] args) {{
        long sum = 0;
{sum}        System.out.println(\"{name} ok \" + sum);
    }}
}}
",
        name = generator.name
    )
}

type Writer<'a> = dyn Fn(PathBuf, String) -> Result<(), String> + 'a;

fn write_build_files(
    generator: &Gen,
    modules: &[Module],
    workspace: bool,
    write: &Writer<'_>,
) -> Result<(), String> {
    let header = "Generated by benches/compare.rs (specs/todo/BENCHMARKS.md §2)";
    if workspace {
        let members: Vec<String> = modules.iter().map(|m| format!("\"{}\"", m.name)).collect();
        write(
            PathBuf::from("jrs.toml"),
            format!(
                "# {header}.\n[workspace]\nmembers = [{}]\n\n[java]\nsource = {RELEASE}\n",
                members.join(", ")
            ),
        )?;
        write(
            PathBuf::from("pom.xml"),
            maven_parent(generator, modules, header),
        )?;
        write(
            PathBuf::from("settings.gradle.kts"),
            format!(
                "// {header}.\nrootProject.name = \"{}\"\n\ninclude({})\n",
                generator.name,
                members.join(", ")
            ),
        )?;
    } else {
        write(
            PathBuf::from("settings.gradle.kts"),
            format!("// {header}.\nrootProject.name = \"{}\"\n", generator.name),
        )?;
    }
    for module in modules {
        let dir = PathBuf::from(&module.dir);
        write(
            dir.join("jrs.toml"),
            jrs_manifest(module, workspace, header),
        )?;
        write(dir.join("pom.xml"), maven_module(module, workspace, header))?;
        write(
            dir.join("build.gradle.kts"),
            gradle_module(module, workspace, header),
        )?;
    }
    Ok(())
}

fn jrs_manifest(module: &Module, workspace: bool, header: &str) -> String {
    let mut out = format!(
        "# {header}.\n[project]\nname = \"{}\"\nversion = \"1.0.0\"\n",
        module.name
    );
    if module.app {
        out.push_str("main-class = \"bench.Main\"\n");
    }
    if !workspace {
        let _ = write!(out, "\n[java]\nsource = {RELEASE}\n");
    }
    out.push_str("\n[dependencies]\n");
    for up in &module.upstream {
        let _ = writeln!(out, "{up} = {{ path = \"../{up}\" }}");
    }
    for (coordinate, _) in &module.dependencies {
        let (ga, version) = coordinate.rsplit_once(':').expect("a coordinate");
        let _ = writeln!(out, "\"{ga}\" = \"{version}\"");
    }
    let _ = write!(
        out,
        "\n[dev-dependencies]\n\"org.junit.jupiter:junit-jupiter\" = \"{JUNIT}\"\n"
    );
    out
}

const MAVEN_PLUGINS: &str = "      <plugins>
        <plugin>
          <artifactId>maven-compiler-plugin</artifactId>
          <version>3.13.0</version>
        </plugin>
        <plugin>
          <artifactId>maven-resources-plugin</artifactId>
          <version>3.3.1</version>
        </plugin>
        <plugin>
          <artifactId>maven-surefire-plugin</artifactId>
          <version>3.5.2</version>
        </plugin>
        <plugin>
          <artifactId>maven-jar-plugin</artifactId>
          <version>3.4.2</version>
        </plugin>
      </plugins>";

const MAVEN_APP_PLUGINS: &str = "    <plugins>
      <plugin>
        <groupId>org.codehaus.mojo</groupId>
        <artifactId>exec-maven-plugin</artifactId>
        <version>3.5.0</version>
        <configuration>
          <mainClass>bench.Main</mainClass>
        </configuration>
      </plugin>
      <plugin>
        <artifactId>maven-shade-plugin</artifactId>
        <version>3.6.0</version>
        <executions>
          <execution>
            <phase>package</phase>
            <goals>
              <goal>shade</goal>
            </goals>
            <configuration>
              <createDependencyReducedPom>false</createDependencyReducedPom>
              <transformers>
                <transformer implementation=\"org.apache.maven.plugins.shade.resource.ManifestResourceTransformer\">
                  <mainClass>bench.Main</mainClass>
                </transformer>
                <transformer implementation=\"org.apache.maven.plugins.shade.resource.ServicesResourceTransformer\"/>
              </transformers>
            </configuration>
          </execution>
        </executions>
      </plugin>
    </plugins>";

const POM_HEAD: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<project xmlns=\"http://maven.apache.org/POM/4.0.0\"
         xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\"
         xsi:schemaLocation=\"http://maven.apache.org/POM/4.0.0 https://maven.apache.org/xsd/maven-4.0.0.xsd\">
  <modelVersion>4.0.0</modelVersion>";

fn maven_parent(generator: &Gen, modules: &[Module], header: &str) -> String {
    let list = modules.iter().fold(String::new(), |mut out, m| {
        let _ = writeln!(out, "    <module>{}</module>", m.name);
        out
    });
    format!(
        "{POM_HEAD}
  <!-- {header}. -->

  <groupId>bench</groupId>
  <artifactId>{name}</artifactId>
  <version>1.0.0</version>
  <packaging>pom</packaging>

  <modules>
{list}  </modules>

  <properties>
    <maven.compiler.release>{RELEASE}</maven.compiler.release>
    <project.build.sourceEncoding>UTF-8</project.build.sourceEncoding>
  </properties>

  <build>
    <pluginManagement>
{MAVEN_PLUGINS}
    </pluginManagement>
  </build>
</project>
",
        name = generator.name
    )
}

fn maven_module(module: &Module, workspace: bool, header: &str) -> String {
    let mut dependencies = String::new();
    for up in &module.upstream {
        let _ = write!(
            dependencies,
            "    <dependency>\n      <groupId>bench</groupId>\n      <artifactId>{up}</artifactId>\n      \
             <version>1.0.0</version>\n    </dependency>\n"
        );
    }
    for (coordinate, _) in &module.dependencies {
        let mut parts = coordinate.split(':');
        let (g, a, v) = (
            parts.next().unwrap_or_default(),
            parts.next().unwrap_or_default(),
            parts.next().unwrap_or_default(),
        );
        let _ = write!(
            dependencies,
            "    <dependency>\n      <groupId>{g}</groupId>\n      <artifactId>{a}</artifactId>\n      \
             <version>{v}</version>\n    </dependency>\n"
        );
    }
    let _ = write!(
        dependencies,
        "    <dependency>\n      <groupId>org.junit.jupiter</groupId>\n      \
         <artifactId>junit-jupiter</artifactId>\n      <version>{JUNIT}</version>\n      \
         <scope>test</scope>\n    </dependency>\n"
    );
    let (parent, properties, management) = if workspace {
        (
            "\n  <parent>\n    <groupId>bench</groupId>\n    <artifactId>gen-ws</artifactId>\n    \
             <version>1.0.0</version>\n  </parent>\n"
                .to_string(),
            String::new(),
            String::new(),
        )
    } else {
        (
            String::new(),
            format!(
                "\n  <properties>\n    <maven.compiler.release>{RELEASE}</maven.compiler.release>\n    \
                 <project.build.sourceEncoding>UTF-8</project.build.sourceEncoding>\n  </properties>\n"
            ),
            format!("    <pluginManagement>\n{MAVEN_PLUGINS}\n    </pluginManagement>\n"),
        )
    };
    let plugins = if module.app {
        format!("{MAVEN_APP_PLUGINS}\n")
    } else {
        String::new()
    };
    format!(
        "{POM_HEAD}
  <!-- {header}. -->
{parent}
  <groupId>bench</groupId>
  <artifactId>{name}</artifactId>
  <version>1.0.0</version>
  <packaging>jar</packaging>
{properties}
  <dependencies>
{dependencies}  </dependencies>

  <build>
{management}{plugins}  </build>
</project>
",
        name = module.name
    )
}

fn gradle_module(module: &Module, workspace: bool, header: &str) -> String {
    let mut plugins = String::from("    `java-library`\n");
    if module.app {
        let _ = write!(
            plugins,
            "    application\n    id(\"com.gradleup.shadow\") version \"{SHADOW}\"\n"
        );
    }
    let mut dependencies = String::new();
    for up in &module.upstream {
        let _ = writeln!(dependencies, "    api(project(\":{up}\"))");
    }
    for (coordinate, _) in &module.dependencies {
        let _ = writeln!(dependencies, "    api(\"{coordinate}\")");
    }
    let _ = write!(
        dependencies,
        "    testImplementation(\"org.junit.jupiter:junit-jupiter:{JUNIT}\")\n    \
         testRuntimeOnly(\"org.junit.platform:junit-platform-launcher:{JUNIT_PLATFORM}\")\n"
    );
    let app = if module.app {
        "\napplication {\n    mainClass = \"bench.Main\"\n}\n\ntasks.shadowJar {\n    mergeServiceFiles()\n}\n"
    } else {
        ""
    };
    let _ = workspace;
    format!(
        "// {header}.
plugins {{
{plugins}}}

group = \"bench\"
version = \"1.0.0\"

repositories {{
    mavenCentral()
}}

dependencies {{
{dependencies}}}

tasks.withType<JavaCompile>().configureEach {{
    options.release = {RELEASE}
    options.encoding = \"UTF-8\"
}}

tasks.test {{
    useJUnitPlatform()
}}
{app}"
    )
}

// ---------------------------------------------------------------------------
// The report of §6

#[allow(
    clippy::too_many_lines,
    reason = "one row per line of the report's header, in order"
)]
fn header(
    repo_root: &Path,
    started: SystemTime,
    settings: &Settings,
    tools: &Tools,
    projects: &Selection,
) -> String {
    let jrs = first_line(
        Command::new(env!("CARGO_BIN_EXE_jrs")).arg("--version"),
        false,
    )
    .unwrap_or_else(|| "jrs".to_string());
    let commit = first_line(
        Command::new("git")
            .arg("-C")
            .arg(repo_root)
            .args(["rev-parse", "--short", "HEAD"]),
        false,
    )
    .unwrap_or_else(|| "unknown".to_string());
    let java = tools
        .java_home
        .as_ref()
        .map(|h| h.join("bin").join(java_exe()))
        .and_then(|java| {
            let output = Command::new(java).arg("-version").output().ok()?;
            Some(
                String::from_utf8_lossy(&output.stderr)
                    .lines()
                    .map(str::trim)
                    .collect::<Vec<_>>()
                    .join("; "),
            )
        })
        .unwrap_or_else(|| "none".to_string());
    let maven = tools
        .mvn
        .as_ref()
        .and_then(|m| first_line(Command::new(m).arg("-v").env("MAVEN_OPTS", ""), false))
        .unwrap_or_else(|| "not on PATH".to_string());
    let gradle = tools
        .gradle
        .as_ref()
        .and_then(|g| {
            let output = Command::new(g).arg("--version").output().ok()?;
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .find(|l| l.starts_with("Gradle "))
                .map(str::to_string)
        })
        .unwrap_or_else(|| "not on PATH".to_string());
    let mut out = String::new();
    let _ = writeln!(
        out,
        "<!-- Generated by `cargo bench --bench compare` (benches/compare.rs); never edited by hand. -->\n"
    );
    let _ = writeln!(out, "# jrs against Maven and Gradle\n");
    let _ = writeln!(
        out,
        "The plan is [specs/todo/BENCHMARKS.md](../specs/todo/BENCHMARKS.md); every sample is in `{}`.\n",
        raw_path(&settings.report, started)
            .strip_prefix(settings.report.parent().unwrap_or(Path::new("")))
            .map_or_else(|_| raw_path(&settings.report, started), Path::to_path_buf)
            .display()
    );
    let _ = writeln!(out, "| | |\n| --- | --- |");
    let rows = [
        ("Date", host::stamp(started, false)),
        ("jrs", format!("{jrs} (`{commit}`)")),
        ("Machine", machine()),
        ("OS", host::os_version()),
        ("Java", java),
        ("Maven", maven),
        ("Gradle", gradle),
        (
            "Runs",
            format!(
                "{} measured, {} warm-up, per cell, interleaved over the tools",
                settings.runs, settings.warmup
            ),
        ),
        (
            "Mirror",
            format!("local, {} ms per response", settings.latency),
        ),
        (
            "Corpus",
            projects
                .corpus_run
                .as_ref()
                .filter(|_| projects.projects.iter().any(|p| p.set == Set::Corpus))
                .map_or_else(
                    || "none benchmarked".to_string(),
                    |r| r.strip_prefix(repo_root).unwrap_or(r).display().to_string(),
                ),
        ),
    ];
    for (k, v) in rows {
        let _ = writeln!(out, "| {k} | {} |", v.replace('|', "\\|"));
    }
    out.push_str(
        "
**The rules** (BENCHMARKS.md §5):

- Each tool runs the build as written, with no tuning flag jrs has no
  counterpart for; a project's own `gradle.properties` and `.mvn/` are kept.
- No build cache on either side: jrs `--no-build-cache`, Gradle
  `--no-build-cache -Dorg.gradle.caching=false`, Maven
  `-Dmaven.build.cache.enabled=false`.
- Gradle three ways: a daemon kept from the warm-ups, the same with
  `--configuration-cache`, and `--no-daemon`; `gradle --stop` after each
  project. Maven as shipped, with no `-T`.
- Measured runs go round-robin over the columns, so drift lands on all of
  them alike.
- A project is timed only once the tools agree on its runtime classpath,
  its tests, its `run` and its package; a failing command is reported as
  failing, not as fast.
- Every tool reads the same local mirror; a cell where it missed is
  `incomplete`, and the mirror needs recording again.
- Output goes to a file: jrs `--progress never`, Maven `-B`, Gradle
  `--console=plain`. A cell is the median wall time, (min–max), and each
  tool's ratio to jrs: `2.4×` means jrs was 2.4 times faster, `0.8×` slower.
  `noisy` marks a cell whose max is over 1.5× its median.
",
    );
    out
}

fn first_line(command: &mut Command, stderr: bool) -> Option<String> {
    let output = command.output().ok()?;
    let text = if stderr { output.stderr } else { output.stdout };
    String::from_utf8_lossy(&text)
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(|l| l.replace("\u{1b}[1m", "").replace("\u{1b}[m", ""))
}

/// The CPU model, its cores and the memory.
fn machine() -> String {
    let sysctl = |key: &str| first_line(Command::new("sysctl").args(["-n", key]), false);
    let (cpu, memory) = match std::env::consts::OS {
        "macos" => (
            sysctl("machdep.cpu.brand_string"),
            sysctl("hw.memsize").and_then(|b| b.parse::<u64>().ok()),
        ),
        "linux" => {
            let cpuinfo = std::fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
            let meminfo = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
            (
                cpuinfo
                    .lines()
                    .find_map(|l| l.strip_prefix("model name"))
                    .map(|l| l.trim_start_matches([' ', '\t', ':']).to_string()),
                meminfo
                    .lines()
                    .find_map(|l| l.strip_prefix("MemTotal:"))
                    .and_then(|l| l.trim().trim_end_matches(" kB").parse::<u64>().ok())
                    .map(|kb| kb * 1024),
            )
        }
        _ => (None, None),
    };
    let cores = std::thread::available_parallelism().map_or(0, std::num::NonZero::get);
    format!(
        "{}, {cores} cores, {}",
        cpu.unwrap_or_else(|| std::env::consts::ARCH.to_string()),
        memory.map_or_else(
            || "memory unknown".to_string(),
            |b| format!("{} GiB", b / (1024 * 1024 * 1024))
        )
    )
}

fn host_name() -> String {
    first_line(&mut Command::new("hostname"), false)
        .map_or_else(
            || "host".to_string(),
            |h| h.split('.').next().unwrap_or(&h).to_string(),
        )
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

fn raw_path(report: &Path, started: SystemTime) -> PathBuf {
    report
        .parent()
        .unwrap_or(Path::new("."))
        .join("raw")
        .join(format!(
            "{}-{}.tsv",
            host::stamp(started, false),
            host_name()
        ))
}

fn raw_samples(results: &[Outcome]) -> String {
    let mut out = String::from("project\tscenario\ttool\trun\tms\texit\tmirror misses\n");
    for outcome in results {
        for (scenario, cells) in &outcome.cells {
            for (column, cell) in cells {
                for s in &cell.samples {
                    let _ = writeln!(
                        out,
                        "{}\t{scenario}\t{}\t{}\t{}\t{}\t{}",
                        outcome.name,
                        column.id(),
                        s.run,
                        s.wall.as_millis(),
                        s.exit,
                        s.misses
                    );
                }
            }
        }
    }
    out
}

fn scenario_title(scenario: &str) -> &'static str {
    match scenario {
        "cold" => {
            "`build` with empty caches and a clean tree: resolution and every download, from the mirror"
        }
        "clean" => "`build` with warm caches and a clean tree",
        "noop" => "the same `build` again, nothing changed",
        "incremental" => "`build` after one method body changed in a class nothing else uses",
        "abi" => "`build` after a public method was added to a class many others use",
        "test" => "the whole suite, every class compiled, no results",
        "test-edit" => {
            "`test` after one method body changed: jrs runs the tests the change reaches, Maven and Gradle the whole suite — selection, not a faster JVM"
        }
        "run" => "start-up to the first expected output or HTTP answer, main classes compiled",
        "package" => "the jar the project ships, every class compiled",
        _ => "",
    }
}

fn duration(d: Duration) -> String {
    let ms = d.as_secs_f64() * 1000.0;
    if ms < 1000.0 {
        format!("{ms:.0} ms")
    } else if ms < 10_000.0 {
        format!("{:.2} s", ms / 1000.0)
    } else if ms < 100_000.0 {
        format!("{:.1} s", ms / 1000.0)
    } else {
        format!("{:.0} s", ms / 1000.0)
    }
}

fn range(min: Duration, max: Duration) -> String {
    let (a, b) = (duration(min), duration(max));
    match (a.rsplit_once(' '), b.rsplit_once(' ')) {
        (Some((a, ua)), Some((b, ub))) if ua == ub => format!("{a}–{b}"),
        _ => format!("{a}–{b}"),
    }
}

fn ratio(theirs: Duration, ours: Duration) -> String {
    let r = theirs.as_secs_f64() / ours.as_secs_f64().max(f64::EPSILON);
    if r >= 10.0 {
        format!("{r:.0}×")
    } else {
        format!("{r:.1}×")
    }
}

/// A cell of a scenario table, with its ratio to `jrs` when given.
fn cell_text(cell: &Cell, jrs: Option<Duration>) -> String {
    if let Some(why) = &cell.skipped {
        return if why.is_empty() {
            "—".into()
        } else {
            "n/a".into()
        };
    }
    if cell.failure.is_some() {
        return "failed".to_string();
    }
    if cell.incomplete() {
        return format!("incomplete ({} misses)", cell.misses);
    }
    let Some((min, median, max)) = cell.stats() else {
        return "—".to_string();
    };
    let mut text = format!("{} ({})", duration(median), range(min, max));
    if let Some(jrs) = jrs {
        let _ = write!(text, " {}", ratio(median, jrs));
    }
    if max.as_secs_f64() > 1.5 * median.as_secs_f64() {
        text.push_str(" noisy");
    }
    text
}

#[allow(
    clippy::too_many_lines,
    reason = "one block per section, in the order the report has them"
)]
fn report(
    header: &str,
    results: &[Outcome],
    settings: &Settings,
    left_out: &[(String, String)],
    repo_root: &Path,
) -> String {
    let mut out = header.to_string();
    let columns: Vec<Column> = COLUMNS
        .into_iter()
        .filter(|c| results.iter().any(|r| r.columns.contains(c)))
        .collect();
    let sets = [
        (Set::Parity, "The parity set: one build written three times"),
        (
            Set::Corpus,
            "The corpus set: real projects against their own build",
        ),
    ];
    let mut failures: Vec<String> = Vec::new();
    for (set, title) in sets {
        let rows: Vec<&Outcome> = results.iter().filter(|r| r.set == set).collect();
        if rows.is_empty() {
            continue;
        }
        let _ = writeln!(out, "\n## {title}\n");
        for &scenario in &settings.scenarios {
            let _ = writeln!(out, "### {scenario} — {}\n", scenario_title(scenario));
            let mut head = vec!["Project".to_string()];
            head.extend(columns.iter().map(|c| c.title().to_string()));
            let _ = writeln!(out, "| {} |", head.join(" | "));
            let _ = writeln!(out, "|{}", " --- |".repeat(head.len()));
            for row in &rows {
                let mut line = vec![format!("`{}`", row.name)];
                if let Some(refused) = &row.refused {
                    line.push(format!("not timed: {}", refused.replace('|', "\\|")));
                    line.extend(std::iter::repeat_n(String::new(), columns.len() - 1));
                    let _ = writeln!(out, "| {} |", line.join(" | "));
                    continue;
                }
                let cells = row.cells.get(scenario);
                let jrs = cells
                    .and_then(|c| c.get(&Column::Jrs))
                    .and_then(Cell::stats)
                    .map(|(_, m, _)| m);
                for column in &columns {
                    let text = match cells.and_then(|c| c.get(column)) {
                        None if !row.columns.contains(column) => "—".to_string(),
                        None => String::new(),
                        Some(cell) => {
                            if let Some(f) = &cell.failure {
                                failures.push(format!(
                                    "`{}` {scenario}, {}: {f}",
                                    row.name,
                                    column.title()
                                ));
                            }
                            cell_text(cell, jrs.filter(|_| *column != Column::Jrs))
                        }
                    };
                    line.push(text);
                }
                let _ = writeln!(out, "| {} |", line.join(" | "));
            }
            out.push('\n');
        }
    }

    out.push_str("## Where jrs is slower\n\n");
    let mut slower = Vec::new();
    for row in results {
        for (scenario, cells) in in_order(&row.cells) {
            let Some(jrs_cell) = cells.get(&Column::Jrs) else {
                continue;
            };
            let Some((jmin, jmedian, jmax)) = jrs_cell.stats() else {
                continue;
            };
            for (column, cell) in cells {
                if *column == Column::Jrs {
                    continue;
                }
                let Some((_, median, _)) = cell.stats() else {
                    continue;
                };
                if median < jmedian && jmedian.saturating_sub(median) > jmax.saturating_sub(jmin) {
                    slower.push(format!(
                        "| `{}` | {scenario} | {} | {} | {} | {} |",
                        row.name,
                        column.title(),
                        duration(jmedian),
                        duration(median),
                        slowest_phase(jrs_cell).unwrap_or_else(|| "—".to_string())
                    ));
                }
            }
        }
    }
    if slower.is_empty() {
        out.push_str(
            "No cell where another tool's median beats jrs's by more than jrs's spread.\n\n",
        );
    } else {
        out.push_str("| Project | Scenario | Tool | jrs | Theirs | jrs's longest phase |\n| --- | --- | --- | --- | --- | --- |\n");
        for line in slower {
            let _ = writeln!(out, "{line}");
        }
        out.push('\n');
    }

    out.push_str("## jrs's phases\n\nThe median of each `--timings` row (`target/.jrs/timings.txt`), in milliseconds.\n\n");
    for row in results.iter().filter(|r| r.refused.is_none()) {
        let mut any = false;
        for (scenario, cells) in in_order(&row.cells) {
            let Some(cell) = cells.get(&Column::Jrs).filter(|c| c.timed()) else {
                continue;
            };
            let phases = phase_medians(cell);
            if phases.is_empty() {
                continue;
            }
            if !any {
                let _ = writeln!(
                    out,
                    "### `{}`\n\n| Scenario | Phases |\n| --- | --- |",
                    row.name
                );
                any = true;
            }
            let text: Vec<String> = phases.iter().map(|(l, ms)| format!("{l} {ms}")).collect();
            let _ = writeln!(out, "| {scenario} | {} |", text.join(" · "));
        }
        if any {
            out.push('\n');
        }
    }

    out.push_str("## Notes\n\n");
    for row in results {
        let mut notes = row.notes.clone();
        notes.extend(row.edits.iter().cloned());
        if let Some(v) = &row.gradle_version {
            notes.push(format!("Gradle {v}, from the project's wrapper"));
        }
        if let Some(v) = &row.maven_version {
            notes.push(format!("Maven {v}, from the project's wrapper"));
        }
        for cells in row.cells.values() {
            for (column, cell) in cells {
                if let Some(why) = cell.skipped.as_ref().filter(|w| !w.is_empty())
                    && *column == Column::Jrs
                {
                    notes.push(format!("not applicable: {why}"));
                }
            }
        }
        notes.dedup();
        if notes.is_empty() {
            continue;
        }
        let _ = writeln!(out, "- `{}`:", row.name);
        for note in notes {
            let _ = writeln!(out, "  - {note}");
        }
    }
    for (name, why) in left_out {
        let _ = writeln!(out, "- `{name}` is left out: {why}");
    }
    if !failures.is_empty() {
        out.push_str("\n### Failures\n\n");
        for f in failures {
            let _ = writeln!(out, "- {f}");
        }
    }

    out.push_str("\n## The test JVM\n\n");
    out.push_str(&test_jvm_section(repo_root));
    out
}

fn slowest_phase(cell: &Cell) -> Option<String> {
    phase_medians(cell)
        .into_iter()
        .filter(|(label, _)| label != "total")
        .max_by_key(|(_, ms)| *ms)
        .map(|(label, ms)| format!("{label} ({ms} ms)"))
}

/// Each `--timings` row's median, in the order the rows first appeared.
fn phase_medians(cell: &Cell) -> Vec<(String, u64)> {
    let mut order: Vec<String> = Vec::new();
    let mut values: BTreeMap<String, Vec<u64>> = BTreeMap::new();
    for sample in &cell.samples {
        let mut sums: BTreeMap<&str, u64> = BTreeMap::new();
        for (label, ms) in &sample.phases {
            *sums.entry(label).or_default() += ms;
            if !order.contains(label) {
                order.push(label.clone());
            }
        }
        for (label, ms) in sums {
            values.entry(label.to_string()).or_default().push(ms);
        }
    }
    order
        .into_iter()
        .map(|label| {
            let mut v = values.remove(&label).unwrap_or_default();
            v.sort_unstable();
            let median = v.get(v.len() / 2).copied().unwrap_or(0);
            (label, median)
        })
        .collect()
}

/// `TEST_JVM_BENCHMARK.md` §4 sends its results to this report: its latest
/// §7, copied so the two need not be read together.
fn test_jvm_section(repo_root: &Path) -> String {
    let text =
        std::fs::read_to_string(repo_root.join("specs/TEST_JVM_BENCHMARK.md")).unwrap_or_default();
    let Some(start) = text.find("\n## 7.") else {
        return "specs/TEST_JVM_BENCHMARK.md has no §7 results yet.\n".to_string();
    };
    let section = &text[start + 1..];
    let body = section.split_once('\n').map_or("", |(_, rest)| rest);
    let body = body.split("\n## ").next().unwrap_or(body);
    format!(
        "Copied from [specs/TEST_JVM_BENCHMARK.md](../specs/TEST_JVM_BENCHMARK.md) §7, \
         the `test.share-classes` benchmark (`benches/test_jvm.rs`):\n\n{}\n",
        body.trim()
            .lines()
            .map(|l| if l.starts_with("### ") {
                format!("#{l}")
            } else {
                l.to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    )
}

/// A project's cells in the scenarios' order, not the map's.
fn in_order<'a>(
    cells: &'a BTreeMap<&'static str, BTreeMap<Column, Cell>>,
) -> impl Iterator<Item = (&'static str, &'a BTreeMap<Column, Cell>)> {
    SCENARIOS
        .into_iter()
        .filter_map(move |s| cells.get(s).map(|c| (s, c)))
}
