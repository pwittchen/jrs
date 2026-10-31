//! `jrs install`: the package, then its copy in a local Maven repository.

use std::path::Path;

use super::session::Session;
use super::{InstallArgs, PackageArgs};
use crate::error::{Result, exit};
use crate::install::{self, Installation};
use crate::package::{self, JarManifest};

impl Session<'_> {
    pub(super) fn install_command(&self, args: &InstallArgs) -> Result<i32> {
        install::check(&self.manifest)?;
        let repo = install::repository(args.repo.as_deref())?;
        let mut rows = self.install(args, &repo)?;
        self.ui
            .phase("Finished", format!("install in {}", self.elapsed()));
        rows.push(("time", self.elapsed()));
        self.ui.summary(&rows);
        Ok(exit::SUCCESS)
    }

    /// Package, write the jar a consumer gets, and copy it, its POM and the
    /// jars the flags asked for into `repo`. Returns the summary rows.
    pub(super) fn install(
        &self,
        args: &InstallArgs,
        repo: &Path,
    ) -> Result<Vec<(&'static str, String)>> {
        let mut rows = self.package(&PackageArgs {
            sources: args.sources,
            javadoc: args.javadoc,
            no_build_cache: args.no_build_cache,
            ..PackageArgs::default()
        })?;

        // Not the jar `package` wrote: its `Class-Path` names this machine's
        // cache, and on a consumer's classpath those jars would load beside
        // whatever versions the consumer's own mediation chose.
        let project = self.project();
        let jar = project
            .work_dir()
            .join("install")
            .join(self.manifest.jar_name());
        package::write_thin_jar(
            &project.classes_dir(),
            &jar,
            &JarManifest {
                main_class: self.manifest.main_class.clone(),
                class_path: Vec::new(),
                attributes: self.manifest.jar_attributes()?,
            },
        )?;

        let resolution = self.resolved()?;
        let upstream = self.member.as_ref().map_or(&[][..], |m| &m.upstream);
        let dependencies = install::pom_dependencies(&self.manifest, &resolution, upstream)?;
        let target = project.target_dir();
        let base = format!("{}-{}", self.manifest.name, self.manifest.version);
        let classified = |wanted: bool, classifier: &'static str| {
            wanted.then(|| (classifier, target.join(format!("{base}-{classifier}.jar"))))
        };
        let installed = Installation {
            group: self.manifest.require_group("install")?,
            artifact: &self.manifest.name,
            version: &self.manifest.version,
            jar: &jar,
            classified: [
                classified(args.sources, "sources"),
                classified(args.javadoc, "javadoc"),
            ]
            .into_iter()
            .flatten()
            .collect(),
            dependencies,
        }
        .write(repo)?;

        if installed.replaced_release {
            self.ui.warn(format!(
                "replaced the jar of the release `{0}:{1}:{2}` with different bytes: a project \
                 that pinned the old jar fails against this one, so a version under development \
                 is better as `{2}-SNAPSHOT`",
                installed.group, installed.artifact, installed.version
            ));
        }
        self.ui.phase(
            "Installed",
            format!(
                "{}:{}:{} to {}",
                installed.group,
                installed.artifact,
                installed.version,
                installed.dir.display()
            ),
        );
        rows.push(("installed", installed.dir.display().to_string()));
        Ok(rows)
    }
}
