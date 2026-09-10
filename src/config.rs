//! The user-level configuration file.
//!
//! `jrs.toml` is committed, so nothing that belongs to a person or a machine may
//! live in it: repository credentials, a mirror for Maven Central, a proxy, a
//! preferred `--jobs`. Those go in one file per user instead, at
//! `$XDG_CONFIG_HOME/jrs/config.toml` (`~/.config/jrs/config.toml`) on Unix and
//! macOS, `%APPDATA%\jrs\config.toml` on Windows, or wherever `JRS_CONFIG`
//! points.
//!
//! ```toml
//! jobs = 8
//!
//! [proxy]
//! url = "http://proxy.example.com:3128"
//! no-proxy = ["localhost", ".internal.example.com"]
//!
//! [mirrors]
//! central = "https://nexus.example.com/repository/maven-central"
//!
//! [credentials.internal]              # a repository name from jrs.toml
//! username = "ci"
//! password-env = "NEXUS_PASSWORD"     # or `password`, `token`, `token-env`
//! ```
//!
//! Like the manifest it is parsed by hand, so an unknown key is a warning and
//! every error names the key. A missing file is not an error: every setting has
//! a default.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::error::{JrsError, Result};

pub const CONFIG_ENV: &str = "JRS_CONFIG";

/// Credentials for one repository.
#[derive(Clone, PartialEq, Eq)]
pub enum Credentials {
    Basic { username: String, password: String },
    Bearer(String),
}

// Never let a secret reach a log line through `{:?}`.
impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Credentials::Basic { username, .. } => {
                write!(
                    f,
                    "Basic {{ username: {username:?}, password: <redacted> }}"
                )
            }
            Credentials::Bearer(_) => write!(f, "Bearer(<redacted>)"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyConfig {
    pub url: String,
    /// Hosts reached directly: `localhost`, `.example.com` (a domain and its
    /// subdomains), `*` for everything.
    pub no_proxy: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Config {
    /// The file this was read from, when there was one.
    pub path: Option<PathBuf>,
    pub jobs: Option<usize>,
    /// An explicit proxy. Without one, `HTTPS_PROXY` / `HTTP_PROXY` /
    /// `ALL_PROXY` and `NO_PROXY` from the environment apply.
    pub proxy: Option<ProxyConfig>,
    /// Repository name → the URL to fetch from instead. `*` mirrors every
    /// repository without a mirror of its own.
    pub mirrors: BTreeMap<String, String>,
    /// Repository name → credentials, with the environment already applied.
    pub credentials: BTreeMap<String, Credentials>,
    /// JDK feature version → its home, for a JDK a project pins that jrs would
    /// not find on its own.
    pub jdks: BTreeMap<u32, PathBuf>,
    pub warnings: Vec<String>,
}

const TOP_KEYS: &[&str] = &["jobs", "proxy", "mirrors", "credentials", "jdks"];
const PROXY_KEYS: &[&str] = &["url", "no-proxy"];
const CREDENTIAL_KEYS: &[&str] = &["username", "password", "password-env", "token", "token-env"];

impl Config {
    /// Read the user's configuration, applying credentials from the process
    /// environment.
    pub fn load() -> Result<Config> {
        let env = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
        match default_path() {
            Some(path) => Config::load_from(&path, &env),
            None => Config::parse("", None, &env),
        }
    }

    /// Read `path` if it exists. `env` looks up environment variables, so tests
    /// never have to mutate the real environment.
    pub fn load_from(path: &Path, env: &dyn Fn(&str) -> Option<String>) -> Result<Config> {
        match std::fs::read_to_string(path) {
            Ok(text) => Config::parse(&text, Some(path), env),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Config::parse("", None, env),
            Err(e) => Err(JrsError::io(path, e)),
        }
    }

    pub fn parse(
        text: &str,
        path: Option<&Path>,
        env: &dyn Fn(&str) -> Option<String>,
    ) -> Result<Config> {
        let shown = path
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "config.toml".to_string());
        let fail = |msg: String| JrsError::usage(format!("{shown}: {msg}"));

        let table: toml::Table = toml::from_str(text).map_err(|e| fail(e.message().to_string()))?;
        let mut config = Config {
            path: path.map(Path::to_path_buf),
            ..Config::default()
        };
        warn_unknown(&table, TOP_KEYS, "", &shown, &mut config.warnings);

        if let Some(value) = table.get("jobs") {
            match value.as_integer() {
                Some(n) if n > 0 => config.jobs = Some(n as usize),
                _ => return Err(fail("`jobs` must be a positive integer".into())),
            }
        }

        if let Some(value) = table.get("proxy") {
            let t = value
                .as_table()
                .ok_or_else(|| fail("`proxy` must be a table".into()))?;
            warn_unknown(t, PROXY_KEYS, "proxy.", &shown, &mut config.warnings);
            let url = t
                .get("url")
                .and_then(|v| v.as_str())
                .ok_or_else(|| fail("`proxy.url` must be a URL string".into()))?
                .to_string();
            let no_proxy = string_list(t, "no-proxy")
                .map_err(|()| fail("`proxy.no-proxy` must be an array of strings".into()))?;
            config.proxy = Some(ProxyConfig { url, no_proxy });
        }

        if let Some(value) = table.get("mirrors") {
            let t = value
                .as_table()
                .ok_or_else(|| fail("`mirrors` must be a table".into()))?;
            for (name, url) in t {
                let url = url
                    .as_str()
                    .ok_or_else(|| fail(format!("`mirrors.{name}` must be a URL string")))?;
                config
                    .mirrors
                    .insert(name.clone(), url.trim_end_matches('/').to_string());
            }
        }

        if let Some(value) = table.get("credentials") {
            let t = value
                .as_table()
                .ok_or_else(|| fail("`credentials` must be a table".into()))?;
            for (repo, entry) in t {
                let entry = entry
                    .as_table()
                    .ok_or_else(|| fail(format!("`credentials.{repo}` must be a table")))?;
                let prefix = format!("credentials.{repo}.");
                warn_unknown(
                    entry,
                    CREDENTIAL_KEYS,
                    &prefix,
                    &shown,
                    &mut config.warnings,
                );
                let field = |key: &str| -> Result<Option<String>> {
                    let direct = match entry.get(key) {
                        None => None,
                        Some(v) => Some(
                            v.as_str()
                                .ok_or_else(|| fail(format!("`{prefix}{key}` must be a string")))?
                                .to_string(),
                        ),
                    };
                    let indirect_key = format!("{key}-env");
                    let indirect = match entry.get(&indirect_key) {
                        None => None,
                        Some(v) => {
                            let var = v.as_str().ok_or_else(|| {
                                fail(format!("`{prefix}{indirect_key}` must be a string"))
                            })?;
                            Some(env(var).ok_or_else(|| {
                                fail(format!(
                                    "`{prefix}{indirect_key}` names `{var}`, which is not set"
                                ))
                            })?)
                        }
                    };
                    Ok(direct.or(indirect))
                };
                let username = entry
                    .get("username")
                    .map(|v| {
                        v.as_str()
                            .map(str::to_string)
                            .ok_or_else(|| fail(format!("`{prefix}username` must be a string")))
                    })
                    .transpose()?;
                let credentials = match (username, field("password")?, field("token")?) {
                    (_, _, Some(token)) => Credentials::Bearer(token),
                    (Some(username), Some(password), None) => {
                        Credentials::Basic { username, password }
                    }
                    _ => {
                        return Err(fail(format!(
                            "`credentials.{repo}` needs `username` and `password` (or \
                             `password-env`), or a `token` (or `token-env`)"
                        )));
                    }
                };
                config.credentials.insert(repo.clone(), credentials);
            }
        }

        if let Some(value) = table.get("jdks") {
            let t = value
                .as_table()
                .ok_or_else(|| fail("`jdks` must be a table of version = path".into()))?;
            for (version, home) in t {
                let v: u32 = version.parse().map_err(|_| {
                    fail(format!(
                        "`jdks.{version}`: the key must be a Java feature version, like `21`"
                    ))
                })?;
                let home = home
                    .as_str()
                    .ok_or_else(|| fail(format!("`jdks.{version}` must be a path string")))?;
                config.jdks.insert(v, PathBuf::from(home));
            }
        }

        Ok(config)
    }

    /// Credentials for `repository`: the environment first, then this file.
    ///
    /// `JRS_REPO_<NAME>_TOKEN`, or `JRS_REPO_<NAME>_USERNAME` with
    /// `JRS_REPO_<NAME>_PASSWORD`, where `<NAME>` is the repository name
    /// upper-cased with every other character turned into `_`.
    pub fn credentials_for(
        &self,
        repository: &str,
        env: &dyn Fn(&str) -> Option<String>,
    ) -> Option<Credentials> {
        let prefix = format!("JRS_REPO_{}_", env_name(repository));
        if let Some(token) = env(&format!("{prefix}TOKEN")) {
            return Some(Credentials::Bearer(token));
        }
        if let (Some(username), Some(password)) = (
            env(&format!("{prefix}USERNAME")),
            env(&format!("{prefix}PASSWORD")),
        ) {
            return Some(Credentials::Basic { username, password });
        }
        self.credentials.get(repository).cloned()
    }

    /// The URL to fetch `repository` from: its mirror, or its own.
    pub fn mirror_for<'a>(&'a self, repository: &str, url: &'a str) -> &'a str {
        if url.starts_with("file://") {
            // A local directory is never worth routing through a mirror.
            return url;
        }
        self.mirrors
            .get(repository)
            .or_else(|| self.mirrors.get("*"))
            .map(String::as_str)
            .unwrap_or(url)
    }
}

/// `my-repo.internal` → `MY_REPO_INTERNAL`.
pub fn env_name(repository: &str) -> String {
    repository
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
}

/// Where the configuration file lives on this platform.
pub fn default_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os(CONFIG_ENV).filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p));
    }
    if cfg!(windows) {
        return std::env::var_os("APPDATA")
            .map(|d| PathBuf::from(d).join("jrs").join("config.toml"));
    }
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME").filter(|x| !x.is_empty()) {
        return Some(PathBuf::from(xdg).join("jrs").join("config.toml"));
    }
    std::env::var_os("HOME").map(|h| {
        PathBuf::from(h)
            .join(".config")
            .join("jrs")
            .join("config.toml")
    })
}

fn string_list(t: &toml::Table, key: &str) -> std::result::Result<Vec<String>, ()> {
    match t.get(key) {
        None => Ok(Vec::new()),
        Some(v) => v
            .as_array()
            .ok_or(())?
            .iter()
            .map(|s| s.as_str().map(str::to_string).ok_or(()))
            .collect(),
    }
}

fn warn_unknown(
    t: &toml::Table,
    known: &[&str],
    prefix: &str,
    file: &str,
    warnings: &mut Vec<String>,
) {
    for key in t.keys() {
        if !known.contains(&key.as_str()) {
            warnings.push(format!("unknown key `{prefix}{key}` in {file} (ignored)"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_env(_: &str) -> Option<String> {
        None
    }

    fn parse(text: &str) -> Result<Config> {
        Config::parse(
            text,
            Some(Path::new("/home/u/.config/jrs/config.toml")),
            &no_env,
        )
    }

    #[test]
    fn a_missing_file_is_every_default() {
        let config = Config::load_from(Path::new("/definitely/not/here.toml"), &no_env).unwrap();
        assert_eq!(config.jobs, None);
        assert!(config.proxy.is_none());
        assert!(config.mirrors.is_empty());
        assert!(config.credentials.is_empty());
        assert!(config.path.is_none());
    }

    #[test]
    fn the_documented_example_parses() {
        let env = |name: &str| (name == "NEXUS_PASSWORD").then(|| "s3cret".to_string());
        let config = Config::parse(
            r#"
jobs = 8

[proxy]
url = "http://proxy.example.com:3128"
no-proxy = ["localhost", ".internal.example.com"]

[mirrors]
central = "https://nexus.example.com/repository/maven-central/"

[credentials.internal]
username = "ci"
password-env = "NEXUS_PASSWORD"

[credentials.github]
token = "ghp_x"
"#,
            None,
            &env,
        )
        .unwrap();
        assert_eq!(config.jobs, Some(8));
        assert_eq!(
            config.proxy,
            Some(ProxyConfig {
                url: "http://proxy.example.com:3128".into(),
                no_proxy: vec!["localhost".into(), ".internal.example.com".into()],
            })
        );
        assert_eq!(
            config.mirrors["central"],
            "https://nexus.example.com/repository/maven-central"
        );
        assert_eq!(
            config.credentials["internal"],
            Credentials::Basic {
                username: "ci".into(),
                password: "s3cret".into()
            }
        );
        assert_eq!(
            config.credentials["github"],
            Credentials::Bearer("ghp_x".into())
        );
        assert!(config.warnings.is_empty(), "{:?}", config.warnings);
    }

    #[test]
    fn secrets_never_reach_debug_output() {
        let basic = Credentials::Basic {
            username: "ci".into(),
            password: "hunter2".into(),
        };
        assert!(!format!("{basic:?}").contains("hunter2"));
        assert!(!format!("{:?}", Credentials::Bearer("tok".into())).contains("tok\""));
    }

    #[test]
    fn a_referenced_variable_that_is_unset_is_an_error() {
        let err = parse("[credentials.internal]\nusername='u'\npassword-env='NOPE'")
            .unwrap_err()
            .to_string();
        assert!(err.contains("NOPE"), "{err}");
        assert!(err.contains("config.toml"), "{err}");
    }

    #[test]
    fn half_a_credential_is_an_error() {
        let err = parse("[credentials.internal]\nusername='u'")
            .unwrap_err()
            .to_string();
        assert!(err.contains("needs `username` and `password`"), "{err}");
    }

    #[test]
    fn the_environment_beats_the_file() {
        let config = parse("[credentials.my-repo]\nusername='file'\npassword='file'").unwrap();
        assert_eq!(
            config.credentials_for("my-repo", &no_env),
            Some(Credentials::Basic {
                username: "file".into(),
                password: "file".into()
            })
        );
        let env = |name: &str| match name {
            "JRS_REPO_MY_REPO_USERNAME" => Some("env".to_string()),
            "JRS_REPO_MY_REPO_PASSWORD" => Some("env-pass".to_string()),
            _ => None,
        };
        assert_eq!(
            config.credentials_for("my-repo", &env),
            Some(Credentials::Basic {
                username: "env".into(),
                password: "env-pass".into()
            })
        );
        let token = |name: &str| (name == "JRS_REPO_OTHER_TOKEN").then(|| "t".to_string());
        assert_eq!(
            config.credentials_for("other", &token),
            Some(Credentials::Bearer("t".into()))
        );
        assert_eq!(config.credentials_for("unknown", &no_env), None);
    }

    #[test]
    fn mirrors_replace_a_named_repository_or_all_of_them() {
        let config =
            parse("[mirrors]\ncentral = 'https://mirror/central'\n'*' = 'https://mirror/all'")
                .unwrap();
        assert_eq!(
            config.mirror_for("central", "https://repo1.maven.org/maven2"),
            "https://mirror/central"
        );
        assert_eq!(
            config.mirror_for("internal", "https://nexus/x"),
            "https://mirror/all"
        );
        assert_eq!(
            config.mirror_for("fixture", "file:///tmp/repo"),
            "file:///tmp/repo",
            "local repositories are never mirrored"
        );
    }

    #[test]
    fn unknown_keys_warn_and_bad_values_fail() {
        let config = parse("colour = 'blue'\n[proxy]\nurl='http://p:1'\nport=3").unwrap();
        assert_eq!(config.warnings.len(), 2, "{:?}", config.warnings);
        assert!(parse("jobs = 0").is_err());
        assert!(parse("jobs = 'many'").is_err());
        assert!(parse("[proxy]\nno-proxy = ['x']").is_err());
    }

    #[test]
    fn jdk_homes_are_keyed_by_feature_version() {
        let config = parse("[jdks]\n21 = '/opt/jdk-21'\n'17' = '/opt/jdk-17'").unwrap();
        assert_eq!(config.jdks[&21], PathBuf::from("/opt/jdk-21"));
        assert_eq!(config.jdks[&17], PathBuf::from("/opt/jdk-17"));
        assert!(parse("[jdks]\nlatest = '/opt/jdk'").is_err());
        assert!(parse("[jdks]\n21 = 21").is_err());
    }

    #[test]
    fn repository_names_map_onto_environment_variables() {
        assert_eq!(env_name("internal"), "INTERNAL");
        assert_eq!(env_name("my-repo.example"), "MY_REPO_EXAMPLE");
    }
}
