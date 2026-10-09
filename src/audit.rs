//! `jrs audit`: the resolved graph checked against the OSV vulnerability
//! database (SPEC §8.12).
//!
//! OSV (<https://osv.dev>) aggregates the GitHub advisories and NVD's for the
//! Maven ecosystem, and answers by coordinate: `group:artifact` and a version,
//! exactly what `jrs.lock` pins. One `POST /v1/querybatch` asks about the whole
//! graph and answers with advisory IDs only; `GET /v1/vulns/<id>` then reads
//! each advisory once, for its summary, severity and fixed versions. Nothing
//! is cached: an advisory published since the last run is the point.

use std::time::Duration;

use rayon::prelude::*;

use crate::config::ProxyConfig;
use crate::error::{JrsError, Result};
use crate::json::Json;
use crate::resolve::coord::{Coord, compare_versions};
use crate::resolve::repo::build_proxy;

/// The public OSV API.
pub const DEFAULT_URL: &str = "https://api.osv.dev";

/// OSV takes at most this many queries in one batch.
const BATCH: usize = 1000;

/// An advisory answer is a few kilobytes; a batch answer for a large graph a
/// few hundred.
const MAX_BODY_BYTES: u64 = 64 * 1024 * 1024;

/// One advisory, as far as it concerns one package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Advisory {
    /// `GHSA-…`, or whatever database OSV has it from.
    pub id: String,
    pub aliases: Vec<String>,
    pub summary: String,
    /// The GitHub advisory's rating, lower-cased: `low`, `moderate`, `high`,
    /// `critical`. `None` when the advisory carries none.
    pub severity: Option<String>,
    /// Every version a range of the advisory names as fixed for the package.
    pub fixed: Vec<String>,
}

impl Advisory {
    /// The ID, with the first CVE alias beside it when there is one.
    #[must_use]
    pub fn label(&self) -> String {
        match self.aliases.iter().find(|a| a.starts_with("CVE-")) {
            Some(cve) if *cve != self.id => format!("{} ({cve})", self.id),
            _ => self.id.clone(),
        }
    }

    /// Whether `name` is this advisory's ID or one of its aliases.
    #[must_use]
    pub fn is_named(&self, name: &str) -> bool {
        self.id.eq_ignore_ascii_case(name)
            || self.aliases.iter().any(|a| a.eq_ignore_ascii_case(name))
    }

    /// The lowest fixed version past `current`: the nearest upgrade that
    /// leaves the advisory behind.
    #[must_use]
    pub fn fixed_after(&self, current: &str) -> Option<&str> {
        self.fixed
            .iter()
            .filter(|f| compare_versions(f, current).is_gt())
            .min_by(|a, b| compare_versions(a, b))
            .map(String::as_str)
    }
}

/// A client for an OSV-compatible API.
pub struct Osv {
    url: String,
    agent: ureq::Agent,
}

impl Osv {
    /// `url` is the API's root, without `/v1`.
    ///
    /// # Errors
    ///
    /// [`JrsError::Usage`] when the configured proxy URL is not usable.
    pub fn new(url: &str, proxy: Option<&ProxyConfig>) -> Result<Osv> {
        let mut config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .user_agent(concat!("jrs/", env!("CARGO_PKG_VERSION")))
            .timeout_connect(Some(Duration::from_secs(30)))
            .timeout_global(Some(Duration::from_secs(300)));
        if let Some(proxy) = proxy {
            config = config.proxy(Some(build_proxy(proxy)?));
        }
        Ok(Osv {
            url: url.trim_end_matches('/').to_string(),
            agent: ureq::Agent::new_with_config(config.build()),
        })
    }

    /// The IDs of the advisories affecting each of `coords`, in order.
    ///
    /// # Errors
    ///
    /// [`JrsError::Resolve`] when the API cannot be reached or answers with
    /// something that is not an OSV batch answer.
    pub fn query(&self, coords: &[Coord]) -> Result<Vec<Vec<String>>> {
        let mut ids = Vec::with_capacity(coords.len());
        for chunk in coords.chunks(BATCH) {
            let url = format!("{}/v1/querybatch", self.url);
            let answer = self.post(&url, &batch_body(chunk))?;
            let results = parse_batch(&answer, chunk.len()).map_err(|e| malformed(&url, &e))?;
            for (coord, (mut found, mut token)) in chunk.iter().zip(results) {
                // A package with more advisories than one page holds.
                while let Some(page) = token {
                    let url = format!("{}/v1/query", self.url);
                    let answer = self.post(&url, &query_body(coord, &page))?;
                    let (more, next) = parse_page(&answer).map_err(|e| malformed(&url, &e))?;
                    found.extend(more);
                    token = next;
                }
                ids.push(found);
            }
        }
        Ok(ids)
    }

    /// Read each advisory in `ids`, `jobs` at a time, keeping what concerns
    /// `group:artifact` — the package it was found for.
    ///
    /// # Errors
    ///
    /// [`JrsError::Resolve`] for the first advisory that cannot be read.
    pub fn advisories(&self, wanted: &[(String, String)], jobs: usize) -> Result<Vec<Advisory>> {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(jobs.max(1))
            .build()
            .map_err(|e| JrsError::resolve(format!("could not start a worker pool: {e}")))?;
        pool.install(|| {
            wanted
                .par_iter()
                .map(|(id, package)| {
                    let url = format!("{}/v1/vulns/{id}", self.url);
                    let answer = self.get(&url)?;
                    parse_advisory(&answer, package).map_err(|e| malformed(&url, &e))
                })
                .collect()
        })
    }

    fn post(&self, url: &str, body: &str) -> Result<String> {
        let response = self
            .agent
            .post(url)
            .header("Content-Type", "application/json")
            .send(body);
        read(url, response)
    }

    fn get(&self, url: &str) -> Result<String> {
        read(url, self.agent.get(url).call())
    }
}

fn read(
    url: &str,
    response: std::result::Result<ureq::http::Response<ureq::Body>, ureq::Error>,
) -> Result<String> {
    let fail =
        |e: &dyn std::fmt::Display| JrsError::resolve(format!("could not reach {url}\n\n{e}"));
    let mut response = response.map_err(|e| fail(&e))?;
    let status = response.status().as_u16();
    let body = response
        .body_mut()
        .with_config()
        .limit(MAX_BODY_BYTES)
        .read_to_string()
        .map_err(|e| fail(&e))?;
    if status != 200 {
        let detail = body.lines().next().unwrap_or_default();
        return Err(fail(&format!("HTTP {status} {detail}")));
    }
    Ok(body)
}

fn malformed(url: &str, e: &str) -> JrsError {
    JrsError::resolve(format!(
        "{url} answered with something that is not OSV: {e}"
    ))
}

fn package(coord: &Coord) -> Json {
    Json::object([
        ("ecosystem", Json::string("Maven")),
        (
            "name",
            Json::string(format!("{}:{}", coord.group, coord.artifact)),
        ),
    ])
}

/// The body of a `querybatch` for `coords`.
fn batch_body(coords: &[Coord]) -> String {
    let queries = coords
        .iter()
        .map(|c| {
            Json::object([
                ("package", package(c)),
                ("version", Json::string(c.version.clone())),
            ])
        })
        .collect();
    Json::object([("queries", Json::Array(queries))]).render()
}

/// The body of a single query for the next page of `coord`'s advisories.
fn query_body(coord: &Coord, page_token: &str) -> String {
    Json::object([
        ("package", package(coord)),
        ("version", Json::string(coord.version.clone())),
        ("page_token", Json::string(page_token)),
    ])
    .render()
}

/// The advisory IDs of one result, and the token of its next page.
type Page = (Vec<String>, Option<String>);

fn page_of(result: &Json) -> Page {
    let ids = result
        .get("vulns")
        .map(Json::items)
        .unwrap_or_default()
        .iter()
        .filter_map(|v| v.get("id").and_then(Json::as_str))
        .map(str::to_string)
        .collect();
    let token = result
        .get("next_page_token")
        .and_then(Json::as_str)
        .filter(|t| !t.is_empty())
        .map(str::to_string);
    (ids, token)
}

/// A `querybatch` answer: one result per query, in order.
fn parse_batch(text: &str, expected: usize) -> std::result::Result<Vec<Page>, String> {
    let doc = Json::parse(text)?;
    let results = doc
        .get("results")
        .ok_or_else(|| "no `results`".to_string())?
        .items();
    if results.len() != expected {
        return Err(format!("{} results for {expected} queries", results.len()));
    }
    Ok(results.iter().map(page_of).collect())
}

/// A single query's answer.
fn parse_page(text: &str) -> std::result::Result<Page, String> {
    Ok(page_of(&Json::parse(text)?))
}

/// An advisory, keeping the fixed versions of `package` (`group:artifact`)
/// only: one advisory often covers several artifacts of a project.
fn parse_advisory(text: &str, package: &str) -> std::result::Result<Advisory, String> {
    let doc = Json::parse(text)?;
    let id = doc
        .get("id")
        .and_then(Json::as_str)
        .ok_or_else(|| "no `id`".to_string())?
        .to_string();
    let strings = |key: &str| -> Vec<String> {
        doc.get(key)
            .map(Json::items)
            .unwrap_or_default()
            .iter()
            .filter_map(Json::as_str)
            .map(str::to_string)
            .collect()
    };
    let summary = doc
        .get("summary")
        .and_then(Json::as_str)
        .or_else(|| {
            doc.get("details")
                .and_then(Json::as_str)
                .and_then(|d| d.lines().find(|l| !l.trim().is_empty()))
        })
        .unwrap_or_default()
        .trim()
        .to_string();
    let severity = doc
        .get("database_specific")
        .and_then(|d| d.get("severity"))
        .and_then(Json::as_str)
        .map(str::to_ascii_lowercase);
    let mut fixed: Vec<String> = Vec::new();
    for affected in doc.get("affected").map(Json::items).unwrap_or_default() {
        let Some(p) = affected.get("package") else {
            continue;
        };
        let ecosystem = p.get("ecosystem").and_then(Json::as_str);
        let name = p.get("name").and_then(Json::as_str);
        if ecosystem != Some("Maven") || name != Some(package) {
            continue;
        }
        for range in affected.get("ranges").map(Json::items).unwrap_or_default() {
            for event in range.get("events").map(Json::items).unwrap_or_default() {
                if let Some(v) = event.get("fixed").and_then(Json::as_str)
                    && !fixed.iter().any(|f| f == v)
                {
                    fixed.push(v.to_string());
                }
            }
        }
    }
    Ok(Advisory {
        id,
        aliases: strings("aliases"),
        summary,
        severity,
        fixed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::io::{Read, Write};
    use std::sync::{Arc, Mutex};

    fn coord(text: &str) -> Coord {
        let mut parts = text.split(':');
        Coord::new(
            parts.next().unwrap(),
            parts.next().unwrap(),
            parts.next().unwrap(),
        )
    }

    const LOG4SHELL: &str = r#"{
      "id": "GHSA-jfh8-c2jp-5v3q",
      "summary": "Remote code injection in Log4j",
      "aliases": ["CVE-2021-44228"],
      "affected": [
        {
          "package": {"ecosystem": "Maven", "name": "org.apache.logging.log4j:log4j-core"},
          "ranges": [{"type": "ECOSYSTEM", "events": [
            {"introduced": "2.13.0"}, {"fixed": "2.15.0"},
            {"introduced": "2.4"}, {"fixed": "2.12.2"},
            {"introduced": "2.0-beta9"}, {"fixed": "2.3.1"}
          ]}]
        },
        {
          "package": {"ecosystem": "Maven", "name": "org.ops4j.pax.logging:pax-logging-log4j2"},
          "ranges": [{"type": "ECOSYSTEM", "events": [{"introduced": "0"}, {"fixed": "1.11.10"}]}]
        }
      ],
      "database_specific": {"severity": "CRITICAL", "cwe_ids": ["CWE-502"]}
    }"#;

    #[test]
    fn an_advisory_keeps_the_fixes_of_its_own_package() {
        let a = parse_advisory(LOG4SHELL, "org.apache.logging.log4j:log4j-core").unwrap();
        assert_eq!(a.id, "GHSA-jfh8-c2jp-5v3q");
        assert_eq!(a.label(), "GHSA-jfh8-c2jp-5v3q (CVE-2021-44228)");
        assert_eq!(a.summary, "Remote code injection in Log4j");
        assert_eq!(a.severity.as_deref(), Some("critical"));
        assert_eq!(a.fixed, ["2.15.0", "2.12.2", "2.3.1"]);
        assert!(a.is_named("cve-2021-44228"));
        assert!(!a.is_named("CVE-2021-45046"));
    }

    #[test]
    fn the_nearest_fix_is_the_lowest_one_past_the_current_version() {
        let a = parse_advisory(LOG4SHELL, "org.apache.logging.log4j:log4j-core").unwrap();
        assert_eq!(a.fixed_after("2.14.1"), Some("2.15.0"));
        assert_eq!(a.fixed_after("2.11.0"), Some("2.12.2"));
        assert_eq!(a.fixed_after("2.16.0"), None);
    }

    #[test]
    fn an_advisory_without_a_summary_or_rating_still_reads() {
        let text = r#"{"id": "CVE-2020-1", "details": "\n  First line.\nSecond line."}"#;
        let a = parse_advisory(text, "g:a").unwrap();
        assert_eq!(a.summary, "First line.");
        assert_eq!(a.severity, None);
        assert!(a.fixed.is_empty());
        assert_eq!(a.label(), "CVE-2020-1");
        assert!(parse_advisory("{}", "g:a").is_err());
    }

    #[test]
    fn a_batch_asks_for_each_coordinate_in_the_maven_ecosystem() {
        let body = batch_body(&[coord("g:a:1.0")]);
        let doc = Json::parse(&body).unwrap();
        let query = &doc.get("queries").unwrap().items()[0];
        assert_eq!(query.get("version").and_then(Json::as_str), Some("1.0"));
        let package = query.get("package").unwrap();
        assert_eq!(
            package.get("ecosystem").and_then(Json::as_str),
            Some("Maven")
        );
        assert_eq!(package.get("name").and_then(Json::as_str), Some("g:a"));
    }

    #[test]
    fn a_batch_answer_must_answer_every_query() {
        let text = r#"{"results": [{"vulns": [{"id": "GHSA-1"}, {"id": "GHSA-2"}]}, {}]}"#;
        let pages = parse_batch(text, 2).unwrap();
        assert_eq!(pages[0], (vec!["GHSA-1".into(), "GHSA-2".into()], None));
        assert_eq!(pages[1], (vec![], None));
        assert!(parse_batch(text, 3).is_err());
        assert!(parse_batch("[]", 0).is_err());
    }

    /// Answers each request by its path, after reading its body, and records
    /// `METHOD path body`.
    fn serve(routes: BTreeMap<&'static str, String>) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") && stream.read(&mut byte).unwrap_or(0) == 1 {
                    head.push(byte[0]);
                }
                let head = String::from_utf8_lossy(&head).to_string();
                let length = head
                    .lines()
                    .find_map(|l| {
                        let (k, v) = l.split_once(':')?;
                        k.eq_ignore_ascii_case("content-length")
                            .then(|| v.trim().parse::<usize>().ok())?
                    })
                    .unwrap_or(0);
                let mut body = vec![0u8; length];
                stream.read_exact(&mut body).unwrap();
                let mut words = head.split_whitespace();
                let (method, path) = (words.next().unwrap_or(""), words.next().unwrap_or(""));
                log.lock().unwrap().push(format!(
                    "{method} {path} {}",
                    String::from_utf8_lossy(&body)
                ));
                let (status, reply) = routes
                    .get(path)
                    .map_or((404, String::new()), |r| (200, r.clone()));
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status} Canned\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                    reply.len()
                );
            }
        });
        (url, seen)
    }

    #[test]
    fn the_client_queries_pages_and_reads_each_advisory() {
        let routes = BTreeMap::from([
            (
                "/v1/querybatch",
                r#"{"results": [{}, {"vulns": [{"id": "GHSA-jfh8-c2jp-5v3q"}], "next_page_token": "p2"}]}"#
                    .to_string(),
            ),
            ("/v1/query", r#"{"vulns": [{"id": "GHSA-2"}]}"#.to_string()),
            ("/v1/vulns/GHSA-jfh8-c2jp-5v3q", LOG4SHELL.to_string()),
        ]);
        let (url, seen) = serve(routes);
        let osv = Osv::new(&format!("{url}/"), None).unwrap();
        let coords = [
            coord("org.slf4j:slf4j-api:2.0.13"),
            coord("org.apache.logging.log4j:log4j-core:2.14.1"),
        ];

        let ids = osv.query(&coords).unwrap();
        assert_eq!(
            ids,
            [
                vec![],
                vec!["GHSA-jfh8-c2jp-5v3q".to_string(), "GHSA-2".into()]
            ]
        );
        let requests = seen.lock().unwrap().clone();
        assert!(requests[0].starts_with("POST /v1/querybatch "));
        assert!(requests[1].starts_with("POST /v1/query "));
        assert!(
            requests[1].contains("\"page_token\": \"p2\""),
            "{}",
            requests[1]
        );

        let wanted = [(
            "GHSA-jfh8-c2jp-5v3q".to_string(),
            "org.apache.logging.log4j:log4j-core".to_string(),
        )];
        let advisories = osv.advisories(&wanted, 2).unwrap();
        assert_eq!(advisories[0].fixed_after("2.14.1"), Some("2.15.0"));

        let missing = [("GHSA-gone".to_string(), "g:a".to_string())];
        let e = osv.advisories(&missing, 1).unwrap_err().to_string();
        assert!(e.contains("HTTP 404"), "{e}");
    }
}
