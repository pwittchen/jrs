//! `jrs doc`: Javadoc, and the other languages' documentation tools.

use std::path::PathBuf;
use std::time::Instant;

use super::session::{Session, display_roots};
use crate::compile::{self, DocTool, DocUnit, ForeignDoc, Language};
use crate::error::{JrsError, Result, exit};
use crate::manifest::{Hook, LanguageConfig};
use crate::project::{Sources, Unit};
use crate::resolve::{self, Classpath, Resolution};
use crate::task;
use crate::toolchain::Toolchain;

impl Session<'_> {
    pub(super) fn doc_command(&self) -> Result<i32> {
        let index = self.doc()?;
        self.ui.phase(
            "Finished",
            format!("{} in {}", index.display(), self.elapsed()),
        );
        Ok(exit::SUCCESS)
    }

    /// Document the main sources, generated ones included, so the
    /// `pre-compile` hook runs first. Returns the index page.
    ///
    /// Scala and Groovy units go to Scaladoc or Groovydoc (SPEC §7.4). The
    /// rest go to `javadoc`, and a Kotlin unit's Kotlin sources are left out,
    /// with a warning: the build runs first, since the Java sources may use
    /// the Kotlin classes and `javadoc` has to find them on the classpath.
    pub(super) fn doc(&self) -> Result<PathBuf> {
        let toolchain = self.toolchain()?;
        let resolution = self.resolved()?;
        self.hook(Hook::PreCompile)?;
        let project = self.project();
        let generated = task::generated(&self.manifest, Hook::PreCompile)?;
        let all = project.sources(Unit::Main, &generated.sources)?;
        if let Some(language) = all.foreign()
            && let Some(config) = self.manifest.language(language)
            && let Some(tool) = language.doc_tool(&config.version)
        {
            let mut roots = project.roots(Unit::Main);
            roots.extend(generated.sources.iter().cloned());
            return self.foreign_doc(&toolchain, &resolution, config, tool, &all, roots);
        }

        let javadoc = toolchain.tool("javadoc")?;
        let mut classpath = self.upstream_dirs(Classpath::compiles);
        classpath.extend(resolution.classpath(Classpath::Compile));
        if let Some(language) = all.foreign() {
            self.ui.warn(format!(
                "jrs has no documentation tool for {language}: its {} source files were left \
                 out, and javadoc documents the Java ones",
                all.count(language)
            ));
            self.build()?;
            classpath.insert(0, project.classes_dir());
        }
        let sources: Vec<PathBuf> = all
            .files
            .iter()
            .filter(|p| Language::of(p) == Some(Language::Java))
            .cloned()
            .collect();
        if sources.is_empty() {
            return Err(JrsError::build(format!(
                "no .java files under {} to document",
                display_roots(&project.roots(Unit::Main))
            )));
        }
        let unit = DocUnit {
            sources: sources.clone(),
            output_dir: project.target_dir().join("doc"),
            classpath,
            release: toolchain.release(self.manifest.java.source)?,
            encoding: self.manifest.java.encoding.clone(),
            extra_args: self.manifest.java.javadoc_args.clone(),
            title: format!("{} {}", self.manifest.name, self.manifest.version),
            work_dir: project.work_dir(),
        };
        self.ui.phase(
            "Documenting",
            format!(
                "{} v{} ({} source files)",
                self.manifest.name,
                self.manifest.version,
                sources.len()
            ),
        );
        let scope = self
            .ui
            .spinner("Documenting", format!("{} source files", sources.len()));
        let result = compile::javadoc(&javadoc, &unit, self.ui);
        scope.finish();
        result?;
        Ok(unit.output_dir.join("index.html"))
    }

    /// Document a Scala or Groovy unit with its language's own tool, into
    /// `target/doc` as `javadoc` would. Scala 3's scaladoc reads the compiled
    /// classes' `TASTy`, so the build runs first and the Java sources, which
    /// have none, are left out; Scaladoc 2 and Groovydoc read every source.
    fn foreign_doc(
        &self,
        toolchain: &Toolchain,
        resolution: &Resolution,
        config: &LanguageConfig,
        tool: DocTool,
        all: &Sources,
        roots: Vec<PathBuf>,
    ) -> Result<PathBuf> {
        let language = config.language;
        let project = self.project();
        let reads_classes = ForeignDoc::reads_classes(language, &config.version);
        if reads_classes {
            self.build()?;
        }
        let tool_classpath = if tool.roots.is_empty() {
            self.tools()?
                .into_iter()
                .find(|t| t.language == language)
                .ok_or_else(|| {
                    JrsError::build(format!("the {language} compiler was not resolved"))
                })?
                .resolution
                .runtime_classpath()
        } else {
            self.doc_tool_classpath(language, &tool)?
        };

        let (sources, what) = if reads_classes {
            let java = all.count(Language::Java);
            if java > 0 {
                self.ui.warn(format!(
                    "scaladoc for Scala 3 reads TASTy, which Java classes have none of: {java} \
                     Java source files were left out"
                ));
            }
            let own: Vec<PathBuf> = all
                .files
                .iter()
                .filter(|p| Language::of(p) == Some(language))
                .cloned()
                .collect();
            let what = format!("{} {language} source files", own.len());
            (own, what)
        } else {
            (all.files.clone(), all.describe("source files"))
        };
        let unit = ForeignDoc {
            language,
            version: config.version.clone(),
            tool,
            tool_classpath,
            jvm_args: config.compiler_jvm_args.clone(),
            sources,
            roots,
            classes_dir: project.classes_dir(),
            classpath: self
                .upstream_dirs(Classpath::compiles)
                .into_iter()
                .chain(resolution.classpath(Classpath::Compile))
                .collect(),
            output_dir: project.target_dir().join("doc"),
            release: toolchain.release(self.manifest.java.source)?,
            encoding: self.manifest.java.encoding.clone(),
            name: self.manifest.name.clone(),
            project_version: self.manifest.version.clone(),
            work_dir: project.work_dir(),
        };
        self.ui.phase(
            "Documenting",
            format!("{} v{} ({what})", self.manifest.name, self.manifest.version),
        );
        let scope = self.ui.spinner("Documenting", &what);
        let result = compile::document(toolchain, &unit, self.ui);
        scope.finish();
        result?;
        Ok(unit.output_dir.join("index.html"))
    }

    /// A doc tool's own graph. It is resolved when `jrs doc` needs it and not
    /// pinned in `jrs.lock`: like the test launcher it is an internal tool at
    /// an exact version, and a build never needs it.
    fn doc_tool_classpath(&self, language: Language, tool: &DocTool) -> Result<Vec<PathBuf>> {
        let fetcher = self.fetcher()?;
        let started = Instant::now();
        let artifact = tool
            .roots
            .first()
            .map_or_else(String::new, |r| r.artifact.clone());
        let what = format!("{artifact} ({language} doc tool)");
        if !self.offline
            && tool
                .roots
                .first()
                .is_some_and(|root| !fetcher.cache().contains(root, "pom"))
        {
            self.ui.phase("Resolving", &what);
        }
        let mut resolution = resolve::resolve_tool(&tool.roots, &fetcher, self.jobs)?;
        resolve::locate_cached(&mut resolution, &fetcher);
        let missing = resolution
            .packages
            .iter()
            .filter(|p| p.jar.is_none() && p.packaging != "pom")
            .count();
        if missing > 0 && !self.offline {
            self.ui.phase("Downloading", &what);
            let scope = self.ui.downloads(missing);
            let result = resolve::fetch_jars(&mut resolution, &fetcher, self.jobs);
            scope.finish();
            result?;
        } else {
            resolve::fetch_jars(&mut resolution, &fetcher, self.jobs)?;
        }
        self.timings
            .since(format!("downloads ({language} doc tool)"), started);
        for warning in &resolution.warnings {
            self.ui.verbose(format!("{}: {warning}", tool.name));
        }
        Ok(resolution.runtime_classpath())
    }
}
