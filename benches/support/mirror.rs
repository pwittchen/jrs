//! The local repository server the benchmarks stand in for a remote one
//! with: `benches/resolution.rs` serves the repository it synthesises from
//! it, `benches/compare.rs` its mirror of Maven Central and of the Gradle
//! Plugin Portal (specs/todo/BENCHMARKS.md §3.1).
//!
//! Std only: a thread per connection, one request per connection, and a
//! fixed sleep before every answer to model the round trip. A route maps a
//! path prefix to a directory; with recording on, a route that names an
//! upstream fetches a miss from it with `ureq`, writes it into the
//! directory and serves it, and records an upstream 404 as a `.404` marker
//! beside where the file would be, so a later run answers it the same way
//! without counting it as a miss.

#![allow(
    dead_code,
    reason = "each benchmark that includes this module uses a part of it"
)]

use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// A path prefix (`/central/`, or `/` for the whole server) and the
/// directory it serves, with the remote repository a recording fills it from.
pub struct Route {
    pub prefix: String,
    pub root: PathBuf,
    pub upstream: Option<String>,
}

#[derive(Clone)]
pub struct Server {
    pub url: String,
    shared: Arc<Shared>,
}

struct Shared {
    routes: Vec<Route>,
    latency_ms: AtomicU64,
    requests: AtomicU64,
    misses: AtomicU64,
    recording: AtomicBool,
    /// The paths answered 404 that no recording vouched for.
    missed: Mutex<BTreeSet<String>>,
    agent: ureq::Agent,
    temp: AtomicU64,
}

impl Server {
    /// One directory served at the root, never recorded into.
    pub fn start(root: PathBuf) -> Server {
        Server::with_routes(vec![Route {
            prefix: "/".to_string(),
            root,
            upstream: None,
        }])
    }

    pub fn with_routes(routes: Vec<Route>) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind the bench server");
        let url = format!("http://{}", listener.local_addr().expect("server address"));
        let agent = ureq::Agent::new_with_config(
            ureq::Agent::config_builder()
                .http_status_as_error(false)
                .timeout_connect(Some(Duration::from_secs(30)))
                .timeout_global(Some(Duration::from_secs(600)))
                .build(),
        );
        let shared = Arc::new(Shared {
            routes,
            latency_ms: AtomicU64::new(0),
            requests: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            recording: AtomicBool::new(false),
            missed: Mutex::new(BTreeSet::new()),
            agent,
            temp: AtomicU64::new(0),
        });
        let state = shared.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let state = state.clone();
                std::thread::spawn(move || {
                    let _ = serve(stream, &state);
                });
            }
        });
        Server { url, shared }
    }

    pub fn set_latency(&self, ms: u64) {
        self.shared.latency_ms.store(ms, Ordering::Relaxed);
    }

    /// Whether a miss is fetched from the route's upstream.
    pub fn set_recording(&self, on: bool) {
        self.shared.recording.store(on, Ordering::Relaxed);
    }

    /// Requests and misses so far.
    pub fn counts(&self) -> (u64, u64) {
        let s = &self.shared;
        (
            s.requests.load(Ordering::Relaxed),
            s.misses.load(Ordering::Relaxed),
        )
    }

    /// Every path missed so far, and forget them.
    pub fn take_missed(&self) -> Vec<String> {
        let mut missed = self.shared.missed.lock().expect("the miss list");
        std::mem::take(&mut *missed).into_iter().collect()
    }
}

fn serve(stream: TcpStream, state: &Shared) -> std::io::Result<()> {
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

    let mut words = request_line.split_whitespace();
    let method = words.next().unwrap_or("GET");
    let target = words.next().unwrap_or("/");
    let path = target.split(['?', '#']).next().unwrap_or(target);
    let (status, body) = match answer(state, path) {
        Answer::Found(body) => ("200 OK", body),
        Answer::Absent => ("404 Not Found", Vec::new()),
        Answer::Missed => {
            state.misses.fetch_add(1, Ordering::Relaxed);
            state
                .missed
                .lock()
                .expect("the miss list")
                .insert(path.to_string());
            ("404 Not Found", Vec::new())
        }
    };
    let mut stream = stream;
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    if method != "HEAD" {
        stream.write_all(&body)?;
    }
    stream.flush()
}

enum Answer {
    Found(Vec<u8>),
    /// A 404 a recording made: the upstream does not have it either.
    Absent,
    Missed,
}

fn answer(state: &Shared, path: &str) -> Answer {
    if path.split('/').any(|s| s == "..") {
        return Answer::Missed;
    }
    let Some(route) = state
        .routes
        .iter()
        .filter(|r| path.starts_with(&r.prefix))
        .max_by_key(|r| r.prefix.len())
    else {
        return Answer::Missed;
    };
    let relative = &path[route.prefix.len()..];
    if relative.is_empty() || relative.ends_with('/') {
        return Answer::Missed;
    }
    let file = route.root.join(relative);
    if let Ok(body) = std::fs::read(&file) {
        return Answer::Found(body);
    }
    let marker = absent_marker(&file);
    if marker.is_file() {
        return Answer::Absent;
    }
    match &route.upstream {
        Some(upstream) if state.recording.load(Ordering::Relaxed) => {
            record(state, &format!("{upstream}{relative}"), &file, &marker)
        }
        _ => Answer::Missed,
    }
}

fn absent_marker(file: &Path) -> PathBuf {
    let mut name = file.file_name().unwrap_or_default().to_os_string();
    name.push(".404");
    file.with_file_name(name)
}

/// Fetch `url` into `file`, through a temporary file and a rename, so an
/// interrupted recording leaves no truncated jar for the next run to serve.
fn record(state: &Shared, url: &str, file: &Path, marker: &Path) -> Answer {
    let Ok(mut response) = state.agent.get(url).call() else {
        return Answer::Missed;
    };
    let status = response.status().as_u16();
    let Some(parent) = file.parent() else {
        return Answer::Missed;
    };
    if std::fs::create_dir_all(parent).is_err() {
        return Answer::Missed;
    }
    if status == 404 || status == 410 {
        let _ = std::fs::write(marker, b"");
        return Answer::Absent;
    }
    if status != 200 {
        return Answer::Missed;
    }
    let Ok(body) = response
        .body_mut()
        .with_config()
        .limit(u64::MAX)
        .read_to_vec()
    else {
        return Answer::Missed;
    };
    let n = state.temp.fetch_add(1, Ordering::Relaxed);
    let temp = parent.join(format!(".recording-{}-{n}", std::process::id()));
    if std::fs::write(&temp, &body).is_err() || std::fs::rename(&temp, file).is_err() {
        let _ = std::fs::remove_file(&temp);
        return Answer::Missed;
    }
    Answer::Found(body)
}
