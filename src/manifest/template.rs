//! Templates: a string with `${...}` placeholders in it, as `[tasks]`,
//! `[run]`, `[test]` and `[package.manifest]` write them (SPEC §7.6).

use crate::error::{JrsError, Result};

/// A `{placeholder}` in a task's value, expanded by jrs before the process
/// starts (TASKS.md §5.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Placeholder {
    Root,
    Target,
    ProjectName,
    ProjectVersion,
    Classes,
    TestClasses,
    Classpath,
    RuntimeClasspath,
    TestClasspath,
    ClasspathArgfile,
    SourcesArgfile,
    Jar,
}

impl Placeholder {
    pub const ALL: [Placeholder; 12] = [
        Placeholder::Root,
        Placeholder::Target,
        Placeholder::ProjectName,
        Placeholder::ProjectVersion,
        Placeholder::Classes,
        Placeholder::TestClasses,
        Placeholder::Classpath,
        Placeholder::RuntimeClasspath,
        Placeholder::TestClasspath,
        Placeholder::ClasspathArgfile,
        Placeholder::SourcesArgfile,
        Placeholder::Jar,
    ];

    /// The name between the braces.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Placeholder::Root => "root",
            Placeholder::Target => "target",
            Placeholder::ProjectName => "project.name",
            Placeholder::ProjectVersion => "project.version",
            Placeholder::Classes => "classes",
            Placeholder::TestClasses => "test-classes",
            Placeholder::Classpath => "classpath",
            Placeholder::RuntimeClasspath => "runtime-classpath",
            Placeholder::TestClasspath => "test-classpath",
            Placeholder::ClasspathArgfile => "classpath-argfile",
            Placeholder::SourcesArgfile => "sources-argfile",
            Placeholder::Jar => "jar",
        }
    }

    /// The ones whose value needs the resolved dependency graph.
    #[must_use]
    pub fn is_classpath(self) -> bool {
        matches!(
            self,
            Placeholder::Classpath
                | Placeholder::RuntimeClasspath
                | Placeholder::TestClasspath
                | Placeholder::ClasspathArgfile
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    Text(String),
    Placeholder(Placeholder),
    /// `{free-port.<name>}`: a TCP port nothing listens on, the same one for
    /// every value that names it during one jrs command.
    FreePort(String),
}

/// The prefix of `{free-port.<name>}`.
const FREE_PORT: &str = "free-port.";

/// The ports `{free-port.<name>}` stood for so far, by name. One per name for
/// the whole command, so `TEST_PORT` and a URL built from the same name agree.
static FREE_PORTS: std::sync::Mutex<Vec<(String, u16)>> = std::sync::Mutex::new(Vec::new());

/// The port `{free-port.<name>}` stands for: asked of the OS the first time a
/// name is expanded, by binding port 0 on the loopback interface and letting
/// go of it, and remembered after that. Two names never share a port.
///
/// # Errors
///
/// [`JrsError::Build`] when no port can be bound.
///
/// # Panics
///
/// If a thread panicked while holding the table, poisoning its lock.
pub fn free_port(name: &str) -> Result<u16> {
    let mut ports = FREE_PORTS.lock().unwrap();
    if let Some((_, port)) = ports.iter().find(|(n, _)| n == name) {
        return Ok(*port);
    }
    for _ in 0..32 {
        let port = std::net::TcpListener::bind(("127.0.0.1", 0))
            .and_then(|l| l.local_addr())
            .map_err(|e| JrsError::build(format!("`{{free-port.{name}}}`: no free port: {e}")))?
            .port();
        if !ports.iter().any(|(_, p)| *p == port) {
            ports.push((name.to_string(), port));
            return Ok(port);
        }
    }
    Err(JrsError::build(format!(
        "`{{free-port.{name}}}`: the OS kept handing out ports already taken"
    )))
}

/// A string that may hold placeholders, parsed once when the manifest loads
/// so that a typo fails then rather than as a baffling tool error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Template {
    /// As written in `jrs.toml`.
    pub raw: String,
    pub segments: Vec<Segment>,
}

impl Template {
    /// Parse `raw`. `{{` and `}}` are literal braces.
    ///
    /// # Errors
    ///
    /// A message (without the key, which the caller knows) for an unknown
    /// placeholder, an unclosed `{` or a stray `}`.
    pub fn parse(raw: &str) -> std::result::Result<Template, String> {
        let mut segments = Vec::new();
        let mut text = String::new();
        let mut chars = raw.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '{' if chars.peek() == Some(&'{') => {
                    chars.next();
                    text.push('{');
                }
                '}' if chars.peek() == Some(&'}') => {
                    chars.next();
                    text.push('}');
                }
                '{' => {
                    let mut name = String::new();
                    loop {
                        match chars.next() {
                            Some('}') => break,
                            Some(ch) => name.push(ch),
                            None => {
                                return Err(format!(
                                    "`{raw}` has an unclosed `{{`; write `{{{{` for a literal brace"
                                ));
                            }
                        }
                    }
                    if let Some(port) = name.strip_prefix(FREE_PORT) {
                        let well_formed = !port.is_empty()
                            && port
                                .chars()
                                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
                        if !well_formed {
                            return Err(format!(
                                "`{{{name}}}` needs a name of letters, digits, `-` and `_` \
                                 after `{FREE_PORT}`"
                            ));
                        }
                        if !text.is_empty() {
                            segments.push(Segment::Text(std::mem::take(&mut text)));
                        }
                        segments.push(Segment::FreePort(port.to_string()));
                        continue;
                    }
                    let placeholder = Placeholder::ALL
                        .into_iter()
                        .find(|p| p.name() == name)
                        .ok_or_else(|| {
                            let known: Vec<String> = Placeholder::ALL
                                .iter()
                                .map(|p| format!("`{{{}}}`", p.name()))
                                .collect();
                            format!(
                                "unknown placeholder `{{{name}}}` (known: {}, and \
                                 `{{free-port.<name>}}`)",
                                known.join(", ")
                            )
                        })?;
                    if !text.is_empty() {
                        segments.push(Segment::Text(std::mem::take(&mut text)));
                    }
                    segments.push(Segment::Placeholder(placeholder));
                }
                '}' => {
                    return Err(format!(
                        "`{raw}` has a stray `}}`; write `}}}}` for a literal brace"
                    ));
                }
                _ => text.push(c),
            }
        }
        if !text.is_empty() {
            segments.push(Segment::Text(text));
        }
        Ok(Template {
            raw: raw.to_string(),
            segments,
        })
    }

    /// A template that is all text: braces in `text` are escaped, so nothing
    /// in it reads as a placeholder.
    #[must_use]
    pub fn literal(text: &str) -> Template {
        Template {
            raw: text.replace('{', "{{").replace('}', "}}"),
            segments: if text.is_empty() {
                Vec::new()
            } else {
                vec![Segment::Text(text.to_string())]
            },
        }
    }

    pub fn placeholders(&self) -> impl Iterator<Item = Placeholder> + '_ {
        self.segments.iter().filter_map(|s| match s {
            Segment::Placeholder(p) => Some(*p),
            Segment::Text(_) | Segment::FreePort(_) => None,
        })
    }

    /// Whether a `{free-port.<name>}` is in it, which only a value handed to
    /// a process — an `env` entry or an argument — can use.
    #[must_use]
    pub fn has_free_port(&self) -> bool {
        self.segments
            .iter()
            .any(|s| matches!(s, Segment::FreePort(_)))
    }

    /// Substitute every placeholder with what `value` says it is.
    ///
    /// # Errors
    ///
    /// Whatever `value` returns for a placeholder it cannot supply.
    pub fn expand(&self, mut value: impl FnMut(Placeholder) -> Result<String>) -> Result<String> {
        let mut out = String::new();
        for segment in &self.segments {
            match segment {
                Segment::Text(t) => out.push_str(t),
                Segment::Placeholder(p) => out.push_str(&value(*p)?),
                Segment::FreePort(name) => out.push_str(&free_port(name)?.to_string()),
            }
        }
        Ok(out)
    }
}
