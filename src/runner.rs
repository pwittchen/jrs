//! Running the compiled project.
//!
//! `jrs run` hands the terminal to the user's program: stdin, stdout and stderr
//! are inherited, and the live region comes down first, so a program that draws
//! its own output is not fighting jrs for the cursor.

use std::path::{Path, PathBuf};

use crate::error::{JrsError, Result};
use crate::resolve::coord::Ga;
use crate::resolve::{Classpath, Resolution};
use crate::toolchain::{Environment, Toolchain, run_inherited_in};
use crate::ui::Ui;

/// The port `--debug` listens on unless given one: the one IDEs default to.
pub const DEFAULT_DEBUG_PORT: u16 = 5005;

/// Where `jrs run --debug` and `jrs test --debug` have the JVM wait for a
/// debugger: `[HOST:]PORT`.
///
/// A bare port listens on localhost only, which is the JDK's own reading of
/// one since JDK 9, so nothing else on the network can attach. `*:5005`
/// listens on every interface, for a JVM in a container or on another host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DebugAddress {
    pub host: Option<String>,
    pub port: u16,
}

impl Default for DebugAddress {
    fn default() -> Self {
        DebugAddress {
            host: None,
            port: DEFAULT_DEBUG_PORT,
        }
    }
}

impl DebugAddress {
    /// Parse `--debug`'s value: `5005`, `*:5005`, `0.0.0.0:5005`.
    ///
    /// # Errors
    ///
    /// A message for a port that is not a number from 1 to 65535, or a host
    /// that is empty or would break the agent's option syntax.
    pub fn parse(s: &str) -> std::result::Result<DebugAddress, String> {
        let (host, port) = match s.rsplit_once(':') {
            Some((host, port)) => (Some(host), port),
            None => (None, s),
        };
        let port = port.parse::<u16>().ok().filter(|p| *p > 0).ok_or_else(|| {
            format!(
                "`{port}` is not a port: expected a number from 1 to 65535, as in \
                 `--debug=5005`, or a host and a port, as in `--debug=*:5005`"
            )
        })?;
        if let Some(host) = host
            && (host.is_empty() || host.contains([',', '=']) || host.contains(char::is_whitespace))
        {
            return Err(format!(
                "`{host}` is not a host to listen on: expected `*`, a name or an address"
            ));
        }
        Ok(DebugAddress {
            host: host.map(str::to_string),
            port,
        })
    }

    /// The JDWP agent, suspended until a debugger attaches.
    #[must_use]
    pub fn jdwp_argument(&self) -> String {
        format!("-agentlib:jdwp=transport=dt_socket,server=y,suspend=y,address={self}")
    }

    /// Where a debugger attaches, for the line jrs prints before the JVM waits.
    #[must_use]
    pub fn attach_to(&self) -> String {
        match self.host.as_deref() {
            None => format!("localhost:{}", self.port),
            Some("*" | "0.0.0.0") => format!("port {} on any interface", self.port),
            Some(host) => format!("{host}:{}", self.port),
        }
    }
}

impl std::fmt::Display for DebugAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.host {
            Some(host) => write!(f, "{host}:{}", self.port),
            None => write!(f, "{}", self.port),
        }
    }
}

/// `-javaagent:<jar>`.
#[must_use]
pub fn java_agent_argument(jar: &Path) -> String {
    format!("-javaagent:{}", jar.display())
}

/// What goes ahead of every other JVM argument: the debugger's agent, then
/// the java agents, in the order the manifest names them.
#[must_use]
pub fn jvm_prefix(debug: Option<&DebugAddress>, agents: &[PathBuf]) -> Vec<String> {
    debug
        .map(DebugAddress::jdwp_argument)
        .into_iter()
        .chain(agents.iter().map(|jar| java_agent_argument(jar)))
        .collect()
}

/// The jars `<section>.java-agents` names, looked up in the resolved graph so
/// that each is the version `jrs.lock` pins: on the runtime classpath for
/// `run` (`runtime`), anywhere on the test classpath for `test`.
///
/// # Errors
///
/// [`JrsError::Manifest`] for an agent the graph does not hold, or holds
/// only where this JVM does not look, or holds without a jar.
pub fn java_agents(
    section: &str,
    agents: &[Ga],
    resolution: &Resolution,
    runtime: bool,
) -> Result<Vec<PathBuf>> {
    let key = format!("{section}.java-agents");
    let table = if runtime {
        "dependencies"
    } else {
        "dev-dependencies"
    };
    agents
        .iter()
        .map(|ga| {
            let Some(package) = resolution.get(ga) else {
                return Err(JrsError::manifest(format!(
                    "`{key}` names `{ga}`, which is not in the resolved dependency graph\n\n\
                     an agent is loaded from the jar jrs resolved, so it has to be a \
                     dependency; add it to jrs.toml:\n\n    [{table}]\n    \"{ga}\" = \"<version>\""
                )));
            };
            if runtime && !matches!(package.classpath, Classpath::Compile | Classpath::Runtime) {
                let why = match package.classpath {
                    Classpath::Test => "is only on the test classpath",
                    _ => "is only reached through `compile-only`, so not at run time",
                };
                return Err(JrsError::manifest(format!(
                    "`{key}` names `{ga}`, which {why}\n\n\
                     `jrs run` loads its agents from the runtime classpath: declare \
                     `{ga}` in [dependencies]"
                )));
            }
            package.jar.clone().ok_or_else(|| {
                JrsError::manifest(format!(
                    "`{key}` names `{ga}`, which has no jar to load (it is `{}`-packaged)",
                    package.packaging
                ))
            })
        })
        .collect()
}

/// Build the argument list for `java <jvm-args> -cp <cp> <main-class> args...`.
///
/// JVM arguments (`[run] jvm-args`) go first: anything after the main class
/// belongs to the program.
#[must_use]
pub fn java_args(
    jvm_args: &[String],
    classpath: &[PathBuf],
    main_class: &str,
    program_args: &[String],
) -> Vec<String> {
    let mut args = Vec::with_capacity(jvm_args.len() + program_args.len() + 3);
    args.extend(jvm_args.iter().cloned());
    if !classpath.is_empty() {
        args.push("-cp".to_string());
        args.push(Toolchain::classpath(classpath));
    }
    args.push(main_class.to_string());
    args.extend(program_args.iter().cloned());
    args
}

/// Run the project's main class in `environment`, returning its exit code.
///
/// # Errors
///
/// [`JrsError::Build`] if `java` cannot be started. A program that exits
/// non-zero is not an error: its code is returned.
pub fn run_main(
    toolchain: &Toolchain,
    jvm_args: &[String],
    classpath: &[PathBuf],
    main_class: &str,
    program_args: &[String],
    environment: &Environment,
    ui: &Ui,
) -> Result<i32> {
    let args = java_args(jvm_args, classpath, main_class, program_args);
    run_inherited_in(ui, &toolchain.java, &args, environment)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_are_ordered_for_java() {
        let args = java_args(
            &[],
            &[PathBuf::from("/classes"), PathBuf::from("/dep.jar")],
            "com.example.Main",
            &["--flag".into(), "value".into()],
        );
        assert_eq!(args[0], "-cp");
        assert!(args[1].contains("classes"));
        assert_eq!(args[2], "com.example.Main");
        assert_eq!(&args[3..], &["--flag", "value"]);
    }

    #[test]
    fn jvm_arguments_come_before_the_main_class() {
        let args = java_args(
            &["-Xmx256m".into(), "--enable-preview".into()],
            &[PathBuf::from("/classes")],
            "Main",
            &["-Xmx1g".into()],
        );
        assert_eq!(&args[..2], &["-Xmx256m", "--enable-preview"]);
        assert_eq!(args[2], "-cp");
        assert_eq!(args[4], "Main");
        assert_eq!(
            args[5], "-Xmx1g",
            "the same flag after the main class is the program's"
        );
    }

    #[test]
    fn program_arguments_are_passed_through_untouched() {
        // Anything after `--` belongs to the program, including things that look
        // like jrs flags.
        let args = java_args(&[], &[], "Main", &["--verbose".into(), "-q".into()]);
        assert_eq!(args, vec!["Main", "--verbose", "-q"]);
    }

    #[test]
    fn an_empty_classpath_emits_no_cp_flag() {
        assert_eq!(java_args(&[], &[], "Main", &[]), vec!["Main"]);
    }

    #[test]
    fn a_debug_address_is_a_port_or_a_host_and_a_port() {
        assert_eq!(
            DebugAddress::parse("5005").unwrap(),
            DebugAddress::default()
        );
        let any = DebugAddress::parse("*:8000").unwrap();
        assert_eq!(any.host.as_deref(), Some("*"));
        assert_eq!(any.port, 8000);
        assert_eq!(any.to_string(), "*:8000");
        assert_eq!(any.attach_to(), "port 8000 on any interface");
        assert_eq!(DebugAddress::default().attach_to(), "localhost:5005");
        assert_eq!(
            DebugAddress::parse("[::1]:9000").unwrap().attach_to(),
            "[::1]:9000"
        );
        for bad in ["", "0", "65536", "port", "-1", ":5005", "a b:5005", "x=y:1"] {
            assert!(DebugAddress::parse(bad).is_err(), "{bad:?}");
        }
        assert!(
            DebugAddress::parse("99999")
                .unwrap_err()
                .contains("from 1 to 65535")
        );
    }

    #[test]
    fn debugging_suspends_the_jvm_on_the_address_given() {
        assert_eq!(
            DebugAddress::default().jdwp_argument(),
            "-agentlib:jdwp=transport=dt_socket,server=y,suspend=y,address=5005",
            "a bare port: the JDK listens on localhost only"
        );
        assert!(
            DebugAddress::parse("*:5005")
                .unwrap()
                .jdwp_argument()
                .ends_with("address=*:5005")
        );
    }

    #[test]
    fn the_debugger_then_the_agents_go_ahead_of_everything() {
        let debug = DebugAddress::default();
        let agents = [PathBuf::from("/c/a.jar"), PathBuf::from("/c/b.jar")];
        let mut jvm = jvm_prefix(Some(&debug), &agents);
        jvm.push("-Xmx1g".into());
        let args = java_args(&jvm, &[PathBuf::from("/classes")], "Main", &[]);
        assert!(args[0].starts_with("-agentlib:jdwp="));
        assert_eq!(args[1], "-javaagent:/c/a.jar");
        assert_eq!(args[2], "-javaagent:/c/b.jar");
        assert_eq!(args[3], "-Xmx1g");
        assert_eq!(args[4], "-cp");
        assert!(jvm_prefix(None, &[]).is_empty());
    }

    fn graph(entries: &[(&str, Classpath, Option<&str>)]) -> Resolution {
        use crate::resolve::ResolvedPackage;
        use crate::resolve::coord::Coord;
        Resolution {
            packages: entries
                .iter()
                .map(|(gav, classpath, jar)| ResolvedPackage {
                    coord: Coord::parse(gav).unwrap(),
                    classpath: *classpath,
                    packaging: if jar.is_some() { "jar" } else { "pom" }.into(),
                    depth: 1,
                    direct: true,
                    dependencies: Vec::new(),
                    jar: jar.map(PathBuf::from),
                    checksum: None,
                    mediated: false,
                })
                .collect(),
            ..Resolution::default()
        }
    }

    #[test]
    fn agents_come_from_the_resolved_graph_at_its_pinned_version() {
        let r = graph(&[
            (
                "org.mockito:mockito-core:5.14.2",
                Classpath::Test,
                Some("/c/mockito-core-5.14.2.jar"),
            ),
            (
                "io.otel:agent:2.0",
                Classpath::Compile,
                Some("/c/agent-2.0.jar"),
            ),
            ("jakarta:api:1", Classpath::Provided, Some("/c/api-1.jar")),
            ("org.example:bom:1", Classpath::Compile, None),
            (
                "org.example:runtime-agent:1",
                Classpath::Runtime,
                Some("/c/runtime-agent-1.jar"),
            ),
        ]);
        let ga = |s: &str| Ga::parse(s).unwrap();

        // Tests look anywhere on the test classpath, dev-dependencies included.
        assert_eq!(
            java_agents(
                "test",
                &[ga("org.mockito:mockito-core"), ga("io.otel:agent")],
                &r,
                false
            )
            .unwrap(),
            vec![
                PathBuf::from("/c/mockito-core-5.14.2.jar"),
                PathBuf::from("/c/agent-2.0.jar")
            ]
        );
        // `jrs run` only on the runtime classpath.
        assert_eq!(
            java_agents("run", &[ga("io.otel:agent")], &r, true).unwrap(),
            vec![PathBuf::from("/c/agent-2.0.jar")]
        );
        let err = java_agents("run", &[ga("org.mockito:mockito-core")], &r, true)
            .unwrap_err()
            .to_string();
        assert!(err.contains("only on the test classpath"), "{err}");
        let err = java_agents("run", &[ga("jakarta:api")], &r, true)
            .unwrap_err()
            .to_string();
        assert!(err.contains("compile-only"), "{err}");
        // `runtime-only` is the natural way to declare an agent: the sources
        // never compile against it, and both JVMs can load it.
        for (section, runtime) in [("run", true), ("test", false)] {
            assert_eq!(
                java_agents(section, &[ga("org.example:runtime-agent")], &r, runtime).unwrap(),
                vec![PathBuf::from("/c/runtime-agent-1.jar")]
            );
        }

        let err = java_agents(
            "test",
            &[ga("io.opentelemetry:opentelemetry-javaagent")],
            &r,
            false,
        )
        .unwrap_err();
        assert_eq!(err.exit_code(), 2, "a manifest error");
        let err = err.to_string();
        assert!(err.contains("`test.java-agents`"), "{err}");
        assert!(
            err.contains("`io.opentelemetry:opentelemetry-javaagent`"),
            "{err}"
        );
        assert!(err.contains("[dev-dependencies]"), "{err}");
        let err = java_agents("run", &[ga("io.opentelemetry:x")], &r, true)
            .unwrap_err()
            .to_string();
        assert!(err.contains("[dependencies]"), "{err}");

        let err = java_agents("run", &[ga("org.example:bom")], &r, true)
            .unwrap_err()
            .to_string();
        assert!(err.contains("`pom`-packaged"), "{err}");
    }
}
