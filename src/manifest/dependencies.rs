//! The dependencies resolution reads: the declared ones, plus each
//! language's runtime library, plus the agents pinned apart from the graph.

use super::{Dependency, JavaAgent, Manifest};
use crate::compile::lang::Language;
use crate::resolve::coord::Coord;

impl Manifest {
    /// The runtime libraries the languages imply, each with its language:
    /// the ones the manifest does not declare itself, in either table
    /// (`JVM_LANGUAGES.md` §4.3). They are never written into `jrs.toml`.
    #[must_use]
    pub fn implied_dependencies(&self) -> Vec<(Dependency, Language)> {
        let declared = |group: &str, artifact: &str| {
            self.dependencies
                .iter()
                .chain(&self.dev_dependencies)
                .any(|d| d.group == group && d.artifact == artifact && d.classifier.is_none())
        };
        let mut implied = Vec::new();
        for config in &self.languages {
            for (group, artifact) in config.language.runtime_libraries(&config.version) {
                if !declared(group, artifact) {
                    implied.push((
                        Dependency::new(group, artifact, &config.version),
                        config.language,
                    ));
                }
            }
        }
        implied
    }

    /// `[dependencies]` as resolution sees them: the declared ones, then the
    /// implied runtime libraries as if declared last. Each implied library is
    /// then a direct dependency, and beats a transitive copy under
    /// nearest-wins; a declared dependency still wins a tie at the same depth.
    #[must_use]
    pub fn effective_dependencies(&self) -> Vec<Dependency> {
        let mut dependencies = self.dependencies.clone();
        dependencies.extend(self.implied_dependencies().into_iter().map(|(d, _)| d));
        dependencies
    }

    /// The agents `run.java-agents` and `test.java-agents` pin apart from the
    /// project's graph, each with the name of its `[[tool]]` block: `run`'s
    /// first, in the order the manifest names them.
    #[must_use]
    pub fn pinned_agents(&self) -> Vec<(String, Coord)> {
        [
            ("run", &self.run.java_agents),
            ("test", &self.test.java_agents),
        ]
        .into_iter()
        .flat_map(|(section, agents)| {
            agents.iter().filter_map(move |agent| match agent {
                JavaAgent::Pinned(coord) => Some((agent.tool_name(section), coord.clone())),
                JavaAgent::Graph(_) => None,
            })
        })
        .collect()
    }
}
