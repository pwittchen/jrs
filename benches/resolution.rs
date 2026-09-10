//! Resolution benchmark: SPEC §12, M5.
//!
//! The milestone asks two things of a fixture project with ~20 transitive
//! dependencies: that resolution is dominated by the network rather than by
//! jrs, and that `--progress never` costs nothing measurable against the
//! animated renderer (SPEC §5.3.1, "never cost time").
//!
//! Maven Central cannot answer either question — its latency swamps any signal
//! and varies from run to run — so this harness synthesises a Maven-layout
//! repository of 22 artifacts, serves it from a local HTTP server that sleeps a
//! fixed time before every response, and times `jrs update` against it with a
//! fresh cache and no `jrs.lock` on every run. Because the injected latency is
//! known, the wall time can be split into a network floor and what jrs adds.
//!
//! ```text
//! cargo bench --bench resolution
//! JRS_BENCH_LATENCY_MS=50 JRS_BENCH_RUNS=10 JRS_BENCH_JOBS=4 cargo bench --bench resolution
//! ```
//!
//! stderr goes to a file, not a terminal: `--progress always` still draws every
//! frame, but a real terminal's own rendering cost is not part of the number.

use std::collections::HashSet;
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use jrs::resolve::repo::sha1_hex;

const GROUP: &str = "bench";

/// Artifact, version, and its dependencies. Shared leaves (`io`, `log-api`,
/// `charset`, `strings`) are reached along several paths, and `data` asks for
/// `json:1.1` at the same depth `web` asks for `json:1.0`, so mediation has
/// real work to do rather than walking a tree.
type Node = (
    &'static str,
    &'static str,
    &'static [(&'static str, &'static str)],
);
const GRAPH: &[Node] = &[
    (
        "web",
        "1.0",
        &[("http", "1.0"), ("json", "1.0"), ("log", "1.0")],
    ),
    (
        "data",
        "1.0",
        &[
            ("sql", "1.0"),
            ("pool", "1.0"),
            ("json", "1.1"),
            ("log", "1.0"),
        ],
    ),
    ("util", "1.0", &[("strings", "1.0"), ("collections", "1.0")]),
    (
        "http",
        "1.0",
        &[("codec", "1.0"), ("io", "1.0"), ("cookies", "1.0")],
    ),
    ("json", "1.0", &[("annotations", "1.0"), ("io", "1.0")]),
    ("json", "1.1", &[("annotations", "1.1"), ("io", "1.0")]),
    ("log", "1.0", &[("log-api", "1.0")]),
    ("sql", "1.0", &[("io", "1.0"), ("time", "1.0")]),
    ("pool", "1.0", &[("concurrent", "1.0"), ("log-api", "1.0")]),
    ("strings", "1.0", &[("charset", "1.0")]),
    ("collections", "1.0", &[("primitives", "1.0")]),
    ("codec", "1.0", &[("charset", "1.0")]),
    ("io", "1.0", &[("buffers", "1.0")]),
    ("cookies", "1.0", &[("strings", "1.0")]),
    ("time", "1.0", &[("tz-data", "1.0")]),
    ("concurrent", "1.0", &[("atomics", "1.0")]),
    ("annotations", "1.0", &[]),
    ("annotations", "1.1", &[]),
    ("log-api", "1.0", &[]),
    ("charset", "1.0", &[]),
    ("primitives", "1.0", &[]),
    ("buffers", "1.0", &[]),
    ("tz-data", "1.0", &[]),
    ("atomics", "1.0", &[]),
];
const ROOTS: &[&str] = &["web", "data", "util"];

/// Big enough that the download bars have bytes to count, small enough that
/// transfer time stays negligible next to the injected latency.
const JAR_PADDING: usize = 48 * 1024;

fn main() {
    let latency = env_u64("JRS_BENCH_LATENCY_MS", 25);
    let runs = env_u64("JRS_BENCH_RUNS", 5).max(1) as usize;
    let jobs = env_u64("JRS_BENCH_JOBS", 8).max(1) as usize;

    let scratch = Scratch::new();
    publish(&scratch.path("repo"));
    let server = Server::start(scratch.path("repo"));
    write_project(&scratch.path("project"), &server.url);

    let levels = level_widths();
    let artifacts: usize = levels.iter().sum();
    let bench = Bench {
        scratch: &scratch,
        server: &server,
        jobs,
        next: AtomicU64::new(0),
    };

    // One discarded run pages the binary in, so the first timed run is not
    // also measuring the disk.
    server.set_latency(0);
    bench.run("never");

    let baseline: Vec<Sample> = (0..runs).map(|_| bench.run("never")).collect();
    server.set_latency(latency);
    // Interleaved, so drift over the run (thermals, other load) lands on both
    // modes alike instead of biasing whichever went second.
    let (mut never, mut always) = (Vec::new(), Vec::new());
    for _ in 0..runs {
        never.push(bench.run("never"));
        always.push(bench.run("always"));
    }

    let requests = never[0].requests;
    let misses: u64 = never.iter().chain(&always).map(|s| s.misses).sum();
    let steady = never.iter().chain(&always).all(|s| s.requests == requests);

    println!("jrs resolution benchmark (SPEC §12, M5)\n");
    println!(
        "  graph    {artifacts} artifacts in {} levels {levels:?}, one mediated conflict",
        levels.len()
    );
    println!("  server   {latency} ms per request, {requests} requests per run, {misses} misses");
    println!("  runs     {runs} per mode, --jobs {jobs}, fresh cache and no jrs.lock each time");
    if !steady {
        println!("  WARNING  request counts differed between runs; the comparison is suspect");
    }
    println!("\n  {:<34}{:>9}{:>9}{:>9}", "", "min", "median", "max");
    for (label, samples) in [
        ("latency 0 ms, --progress never", &baseline),
        ("--progress never", &never),
        ("--progress always", &always),
    ] {
        let (min, med, max) = stats(samples);
        println!("  {label:<34}{:>9}{:>9}{:>9}", ms(min), ms(med), ms(max));
    }

    // The resolver fetches one level's POMs in parallel, then the next level's
    // (resolve/mod.rs), and each download is followed by its `.sha1`. So each
    // level costs ceil(width / jobs) rounds of two requests, and the jars that
    // follow cost ceil(artifacts / jobs) more. This is a model, not a bound:
    // rayon does not schedule in strict rounds, and connection setup is not free.
    let rounds: usize =
        levels.iter().map(|w| w.div_ceil(jobs)).sum::<usize>() + artifacts.div_ceil(jobs);
    let floor = Duration::from_millis(rounds as u64 * 2 * latency);
    let own = stats(&baseline).1;
    let never_med = stats(&never).1;
    let always_med = stats(&always).1;

    println!(
        "\n  network floor, level-by-level model  {:>7}   {rounds} rounds x 2 requests x {latency} ms",
        ms(floor)
    );
    println!("  jrs on its own (latency 0 median)    {:>7}", ms(own));
    println!(
        "  never median above the floor         {:>7}",
        signed_ms(never_med.as_secs_f64() - floor.as_secs_f64())
    );
    let share = floor.as_secs_f64() / never_med.as_secs_f64() * 100.0;
    println!(
        "  network share of wall time           {share:>6.0}%   {}",
        if share >= 50.0 {
            "network-dominated"
        } else {
            "NOT network-dominated"
        }
    );

    let delta = always_med.as_secs_f64() - never_med.as_secs_f64();
    let noise = (never_med.as_secs_f64() * 0.05).max(0.020);
    println!(
        "\n  renderer cost, always - never        {:>7}   ({:+.1}%)",
        signed_ms(delta),
        delta / never_med.as_secs_f64() * 100.0
    );
    println!(
        "  {} (threshold: 5% of the never median or 20 ms, whichever is larger)",
        if delta.abs() <= noise {
            "within noise"
        } else {
            "OUTSIDE noise"
        }
    );
    // Evidence the animated mode really drew: the plain transcript is a few
    // lines, the animated one carries every frame.
    println!(
        "  stderr per run: never {} bytes, always {} bytes",
        stats_bytes(&never),
        stats_bytes(&always)
    );
}

struct Sample {
    wall: Duration,
    requests: u64,
    misses: u64,
    stderr_bytes: u64,
}

struct Bench<'a> {
    scratch: &'a Scratch,
    server: &'a Server,
    jobs: usize,
    next: AtomicU64,
}

impl Bench<'_> {
    /// One `jrs update` from nothing: a new, empty cache and no lockfile, so
    /// every POM and jar crosses the wire.
    fn run(&self, progress: &str) -> Sample {
        let n = self.next.fetch_add(1, Ordering::Relaxed);
        let project = self.scratch.path("project");
        let cache = self.scratch.path(&format!("cache-{n}"));
        let stderr_path = self.scratch.path("stderr.log");
        let _ = std::fs::remove_file(project.join("jrs.lock"));
        std::fs::create_dir_all(&cache).expect("create the cache dir");
        let stderr = File::create(&stderr_path).expect("create the stderr log");

        let (requests, misses) = self.server.counts();
        let start = Instant::now();
        let status = Command::new(env!("CARGO_BIN_EXE_jrs"))
            .arg("--manifest-path")
            .arg(&project)
            .args([
                "update",
                "--progress",
                progress,
                "--jobs",
                &self.jobs.to_string(),
            ])
            .env("JRS_CACHE_DIR", &cache)
            // The user's own config could add a proxy or change retries.
            .env("JRS_CONFIG", self.scratch.path("no-such-config.toml"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(stderr)
            .status()
            .expect("spawn jrs");
        let wall = start.elapsed();

        let log = std::fs::read(&stderr_path).unwrap_or_default();
        if !status.success() || !project.join("jrs.lock").is_file() {
            panic!(
                "jrs update --progress {progress} failed ({status}):\n{}",
                String::from_utf8_lossy(&log)
            );
        }
        let _ = std::fs::remove_dir_all(&cache);
        let (after_requests, after_misses) = self.server.counts();
        Sample {
            wall,
            requests: after_requests - requests,
            misses: after_misses - misses,
            stderr_bytes: log.len() as u64,
        }
    }
}

/// POM and jar for every node, each with a `.sha1`, so the fetcher verifies
/// everything it downloads instead of warning and moving on.
fn publish(repo: &Path) {
    for (artifact, version, deps) in GRAPH {
        let dir = repo.join(GROUP).join(artifact).join(version);
        std::fs::create_dir_all(&dir).expect("create the repository");
        let deps: String = deps
            .iter()
            .map(|(a, v)| {
                format!(
                    "    <dependency><groupId>{GROUP}</groupId><artifactId>{a}</artifactId>\
                     <version>{v}</version></dependency>\n"
                )
            })
            .collect();
        let pom = format!(
            "<project>\n  <modelVersion>4.0.0</modelVersion>\n  <groupId>{GROUP}</groupId>\n  \
             <artifactId>{artifact}</artifactId>\n  <version>{version}</version>\n  \
             <dependencies>\n{deps}  </dependencies>\n</project>\n"
        );
        for (ext, bytes) in [("pom", pom.into_bytes()), ("jar", jar_bytes(artifact))] {
            let file = dir.join(format!("{artifact}-{version}.{ext}"));
            std::fs::write(file.with_extension(format!("{ext}.sha1")), sha1_hex(&bytes))
                .expect("write a checksum");
            std::fs::write(file, bytes).expect("write an artifact");
        }
    }
}

/// A valid, empty zip: only the end-of-central-directory record, padded out
/// with its comment field.
fn jar_bytes(artifact: &str) -> Vec<u8> {
    let comment: Vec<u8> = artifact.bytes().cycle().take(JAR_PADDING).collect();
    let mut out = b"PK\x05\x06".to_vec();
    out.extend_from_slice(&[0; 16]); // disk numbers, entry counts, directory size and offset
    out.extend_from_slice(&(comment.len() as u16).to_le_bytes());
    out.extend(comment);
    out
}

fn write_project(dir: &Path, repo_url: &str) {
    std::fs::create_dir_all(dir).expect("create the project");
    let deps: String = ROOTS
        .iter()
        .map(|r| format!("\"{GROUP}:{r}\" = \"1.0\"\n"))
        .collect();
    let manifest = format!(
        "[project]\nname = \"bench\"\nversion = \"1.0.0\"\n\n\
         [repositories]\nbench = \"{repo_url}\"\n\n[dependencies]\n{deps}"
    );
    std::fs::write(dir.join("jrs.toml"), manifest).expect("write jrs.toml");
}

/// How many POMs each breadth-first level fetches after nearest-wins
/// mediation — a replay of SPEC §8.2 over [`GRAPH`], used only for the model.
fn level_widths() -> Vec<usize> {
    let deps_of = |a: &str, v: &str| {
        GRAPH
            .iter()
            .find(|(ga, gv, _)| *ga == a && *gv == v)
            .map_or(&[][..], |(_, _, d)| *d)
    };
    let mut seen = HashSet::new();
    let mut level: Vec<(&str, &str)> = ROOTS.iter().map(|r| (*r, "1.0")).collect();
    let mut widths = Vec::new();
    while !level.is_empty() {
        let selected: Vec<_> = level.into_iter().filter(|(a, _)| seen.insert(*a)).collect();
        if !selected.is_empty() {
            widths.push(selected.len());
        }
        level = selected
            .iter()
            .flat_map(|(a, v)| deps_of(a, v).iter().copied())
            .collect();
    }
    widths
}

/// A static-file HTTP server that stands in for a remote repository: a thread
/// per connection, one request per connection, and a fixed sleep before every
/// answer to model the round trip.
struct Server {
    url: String,
    shared: Arc<Shared>,
}

#[derive(Default)]
struct Shared {
    latency_ms: AtomicU64,
    requests: AtomicU64,
    misses: AtomicU64,
}

impl Server {
    fn start(root: PathBuf) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind the bench server");
        let url = format!("http://{}", listener.local_addr().expect("server address"));
        let shared = Arc::new(Shared::default());
        let state = shared.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let (root, state) = (root.clone(), state.clone());
                std::thread::spawn(move || {
                    let _ = serve(stream, &root, &state);
                });
            }
        });
        Server { url, shared }
    }

    fn set_latency(&self, ms: u64) {
        self.shared.latency_ms.store(ms, Ordering::Relaxed);
    }

    fn counts(&self) -> (u64, u64) {
        let s = &self.shared;
        (
            s.requests.load(Ordering::Relaxed),
            s.misses.load(Ordering::Relaxed),
        )
    }
}

fn serve(stream: TcpStream, root: &Path, state: &Shared) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    let mut header = String::new();
    while reader.read_line(&mut header)? > 0 && header != "\r\n" {
        header.clear();
    }

    state.requests.fetch_add(1, Ordering::Relaxed);
    std::thread::sleep(Duration::from_millis(
        state.latency_ms.load(Ordering::Relaxed),
    ));

    let path = request_line.split_whitespace().nth(1).unwrap_or("/");
    let body = if path.split('/').any(|s| s == "..") {
        None
    } else {
        std::fs::read(root.join(path.trim_start_matches('/'))).ok()
    };
    let (status, body) = match body {
        Some(body) => ("200 OK", body),
        None => {
            state.misses.fetch_add(1, Ordering::Relaxed);
            ("404 Not Found", Vec::new())
        }
    };
    let mut stream = stream;
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(&body)?;
    stream.flush()
}

/// The benchmark's working directory, removed however `main` ends.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Scratch {
        let dir = std::env::temp_dir().join(format!("jrs-bench-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create the scratch dir");
        Scratch(dir)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn env_u64(name: &str, default: u64) -> u64 {
    match std::env::var(name) {
        Ok(v) => v
            .parse()
            .unwrap_or_else(|_| panic!("{name} must be a whole number, got `{v}`")),
        Err(_) => default,
    }
}

/// Min, median and max wall time.
fn stats(samples: &[Sample]) -> (Duration, Duration, Duration) {
    let mut walls: Vec<Duration> = samples.iter().map(|s| s.wall).collect();
    walls.sort();
    (walls[0], walls[walls.len() / 2], walls[walls.len() - 1])
}

fn stats_bytes(samples: &[Sample]) -> u64 {
    let mut bytes: Vec<u64> = samples.iter().map(|s| s.stderr_bytes).collect();
    bytes.sort();
    bytes[bytes.len() / 2]
}

fn ms(d: Duration) -> String {
    format!("{:.0} ms", d.as_secs_f64() * 1000.0)
}

fn signed_ms(secs: f64) -> String {
    format!("{:+.0} ms", secs * 1000.0)
}
