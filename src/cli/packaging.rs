//! `jrs package`: the jar, and the extras its flags ask for.

use std::path::{Path, PathBuf};
use std::time::Instant;

use super::PackageArgs;
use super::session::{Built, Session, deps_row};
use crate::error::{IoResultExt, JrsError, Result, exit};
use crate::manifest::{Builtin, Hook, JavaAgent};
use crate::native_image::{self, NativeImage};
use crate::obfuscate::{self, Obfuscation};
use crate::package::{self, JarManifest};
use crate::project::Unit;
use crate::relocate::Relocator;
use crate::resolve::{self, Classpath, Resolution};
use crate::{dist, image, sbom, task, ui};

impl Session<'_> {
    /// `run.java-agents` as the image's launchers name them, relative to the
    /// application directory: `lib/<file>` for an agent from the graph, where
    /// the portable layout put its jar, and `agents/<file>` for a pinned one,
    /// which the image stages there — with those jars.
    fn image_agents(&self, lib_dir: Option<&Path>) -> Result<ImageAgents> {
        let wanted = &self.manifest.run.java_agents;
        if wanted.is_empty() {
            return Ok((Vec::new(), Vec::new()));
        }
        if lib_dir.is_none()
            && let Some(ga) = wanted.iter().find_map(|a| match a {
                JavaAgent::Graph(ga) => Some(ga),
                JavaAgent::Pinned(_) => None,
            })
        {
            return Err(JrsError::manifest(format!(
                "`run.java-agents` names `{ga}`, which cannot go into an image or a \
                 distribution of a fat jar: an agent from the dependency graph loads from a \
                 jar of its own, and the fat jar has unpacked it\n\n\
                 leave out --fat, and the portable layout is used, with each agent in its \
                 lib/; or name the agent with its version, as in `\"{ga}:<version>\"`, and \
                 jrs pins its jar apart from the graph and ships it in agents/"
            )));
        }
        let resolution = self.resolved()?;
        let jars = self.java_agents("run", wanted, &resolution, true)?;
        let mut launch = Vec::new();
        let mut staged: Vec<(PathBuf, String)> = Vec::new();
        for (agent, jar) in wanted.iter().zip(jars) {
            let name = jar
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let group = agent.ga().group;
            let file = if let (JavaAgent::Graph(_), Some(lib_dir)) = (agent, lib_dir) {
                // `copy_libraries` prefixes a file name two groups share.
                let prefixed = format!("{group}.{name}");
                if lib_dir.join(&prefixed).is_file() {
                    format!("lib/{prefixed}")
                } else {
                    format!("lib/{name}")
                }
            } else {
                // As `copy_libraries` does, when two pinned agents'
                // files share a name.
                let plain = format!("agents/{name}");
                let file = if staged.iter().any(|(_, f)| *f == plain) {
                    format!("agents/{group}.{name}")
                } else {
                    plain
                };
                staged.push((jar, file.clone()));
                file
            };
            launch.push(file);
        }
        Ok((launch, staged))
    }

    pub(super) fn package_command(&self, args: &PackageArgs) -> Result<i32> {
        let mut rows = self.package(args)?;
        self.ui
            .phase("Finished", format!("build in {}", self.elapsed()));
        rows.push(("time", self.elapsed()));
        self.ui.summary(&rows);
        self.report_timings("package")?;
        Ok(exit::SUCCESS)
    }

    /// Build, write the jar (and any image), then the `post-package` hook.
    /// Returns the summary rows.
    #[allow(
        clippy::too_many_lines,
        reason = "the checks every flag needs before the build, then one arm per jar kind; \
                  images and the other artifacts are already helpers"
    )]
    pub(super) fn package(&self, args: &PackageArgs) -> Result<Vec<(&'static str, String)>> {
        let images = args.jlink || args.jpackage.is_some();
        if images {
            // An image starts its application with `java -jar`.
            let command = if args.jlink {
                "package --jlink"
            } else {
                "package --jpackage"
            };
            self.manifest.require_main_class(command)?;
        }
        if args.dist {
            self.manifest.require_main_class("package --dist")?;
        }
        if args.native_image {
            self.manifest.require_main_class("package --native-image")?;
            // Before anything is built: without GraalVM nothing can use it.
            native_image::find(&self.toolchain()?)?;
        }
        if args.obfuscate && self.manifest.obfuscate.is_none() {
            return Err(JrsError::usage(
                "`jrs package --obfuscate` needs an [obfuscate] table in jrs.toml\n\n\
                 add one with the ProGuard release to pin, for example:\n\n    \
                 [obfuscate]\n    version = \"7.6.1\""
                    .to_string(),
            ));
        }
        let attributes = self.manifest.jar_attributes()?;
        let built = self.build()?;
        let project = self.project();
        let output = project.jar_path();
        // The members this one depends on go first, as jars of their own,
        // under the rules any dependency's jar is packaged by.
        let mut runtime = self.module_jars()?;
        runtime.extend(built.resolution.runtime_classpath());
        // An image or a distribution needs a jar that runs anywhere: the fat
        // one when asked for, the portable layout otherwise.
        let portable = args.portable || ((images || args.dist) && !args.fat);
        let lib_dir = project.target_dir().join("lib");

        let relocations = &self.manifest.package.relocate;
        if !args.fat && !relocations.is_empty() {
            self.ui.warn(
                "[package.relocate] applies to a fat jar, and this jar leaves its dependencies \
                 outside it, unrelocated; `jrs package --fat` builds one",
            );
        }

        let packaging = Instant::now();
        let outcome = if args.fat {
            let relocated = match relocations.len() {
                0 => String::new(),
                1 => ", 1 package relocated".to_string(),
                n => format!(", {n} packages relocated"),
            };
            self.ui.phase(
                "Packaging",
                format!(
                    "{} (fat, {} dependencies{relocated})",
                    output.display(),
                    runtime.len()
                ),
            );
            package::write_fat_jar(
                &project.classes_dir(),
                &runtime,
                &Relocator::new(relocations),
                &output,
                &JarManifest {
                    main_class: self.manifest.main_class.clone(),
                    class_path: Vec::new(),
                    attributes,
                },
            )?
        } else if portable {
            self.ui.phase(
                "Packaging",
                format!(
                    "{} (portable, {} dependencies in {})",
                    output.display(),
                    runtime.len(),
                    lib_dir.display()
                ),
            );
            let libraries: Vec<(String, PathBuf)> = runtime
                .iter()
                .map(|jar| {
                    let group = built
                        .resolution
                        .packages
                        .iter()
                        .find(|p| p.jar.as_ref() == Some(jar))
                        .map(|p| p.coord.group.clone())
                        .unwrap_or_default();
                    (group, jar.clone())
                })
                .collect();
            let class_path = package::copy_libraries(&libraries, &lib_dir, "lib")?;
            package::write_thin_jar(
                &project.classes_dir(),
                &output,
                &JarManifest {
                    main_class: self.manifest.main_class.clone(),
                    class_path,
                    attributes,
                },
            )?
        } else {
            self.ui.phase("Packaging", output.display());
            // A thin jar points at the cached dependency jars, so `java -jar`
            // works without a classpath argument on this machine.
            let class_path = runtime
                .iter()
                .map(|p| package::class_path_entry(p))
                .collect();
            package::write_thin_jar(
                &project.classes_dir(),
                &output,
                &JarManifest {
                    main_class: self.manifest.main_class.clone(),
                    class_path,
                    attributes,
                },
            )?
        };
        self.timings.since("packaging", packaging);
        for warning in &outcome.warnings {
            self.ui.warn(warning);
        }

        let mut rows = vec![
            ("build", format!("ok      {} classes", built.classes)),
            ("deps", deps_row(&built.resolution)),
            (
                "jar",
                format!(
                    "{}   {}",
                    self.manifest.jar_name(),
                    ui::format_bytes(outcome.bytes)
                ),
            ),
        ];
        if images {
            rows.extend(self.images(args, &output, portable.then_some(lib_dir.as_path()))?);
        }
        rows.extend(self.package_extras(
            args,
            &output,
            portable.then_some(lib_dir.as_path()),
            &built,
        )?);

        *self.jar.borrow_mut() = Some(output);
        self.hook(Hook::PostPackage)?;
        Ok(rows)
    }

    /// `--jlink` and `--jpackage`: runtime images of the packaged jar.
    fn images(
        &self,
        args: &PackageArgs,
        jar: &Path,
        lib_dir: Option<&Path>,
    ) -> Result<Vec<(&'static str, String)>> {
        let toolchain = self.toolchain()?;
        let project = self.project();
        let (agents, agent_jars) = self.image_agents(lib_dir)?;
        let app = image::App {
            name: &self.manifest.name,
            version: &self.manifest.version,
            main_class: self.manifest.main_class.as_deref(),
            jar,
            lib_dir,
            jvm_args: &self.manifest.run.jvm_args,
            java_agents: &agents,
            agent_jars: &agent_jars,
        };

        self.ui.phase("Analysing", "module dependencies with jdeps");
        let started = Instant::now();
        let scope = self.ui.spinner("Analysing", "module dependencies");
        let modules = app.jars().and_then(|jars| {
            image::modules(
                &toolchain,
                &jars,
                toolchain.release(self.manifest.java.source)?,
                &self.manifest.package.add_modules,
                self.ui,
            )
        });
        scope.finish();
        self.timings.since("jdeps", started);
        let modules = modules?;
        self.ui.verbose(format!("modules: {}", modules.join(",")));

        let mut rows = Vec::new();
        if args.jlink {
            let output = project.target_dir().join("image");
            self.ui.phase(
                "Linking",
                format!("{} ({} modules)", output.display(), modules.len()),
            );
            let started = Instant::now();
            let scope = self.ui.spinner("Linking", "a runtime image");
            let linked = image::jlink(&toolchain, &app, &modules, &output, self.ui);
            scope.finish();
            self.timings.since("jlink", started);
            let linked = linked?;
            rows.push((
                "image",
                format!(
                    "{}   {}",
                    linked.path.display(),
                    ui::format_bytes(linked.bytes)
                ),
            ));
        }
        if let Some(kind) = &args.jpackage {
            let dest = project.target_dir().join("jpackage");
            self.ui.phase(
                "Bundling",
                format!(
                    "{} ({})",
                    dest.display(),
                    kind.as_deref().unwrap_or("the platform's default package")
                ),
            );
            let started = Instant::now();
            let scope = self.ui.spinner("Bundling", "with jpackage");
            let bundled = image::jpackage(
                &toolchain,
                &app,
                &modules,
                kind.as_deref(),
                &dest,
                &project.work_dir(),
                self.ui,
            );
            scope.finish();
            self.timings.since("jpackage", started);
            let bundled = bundled?;
            self.ui.phase("Bundled", bundled.display());
            rows.push(("package", bundled.display().to_string()));
        }
        Ok(rows)
    }

    /// `--sources`, `--javadoc`, `--dist` and `--native-image`: what
    /// `package` writes beside the jar, from the jar (and `lib/`) just
    /// written. Returns their summary rows.
    fn package_extras(
        &self,
        args: &PackageArgs,
        jar: &Path,
        lib_dir: Option<&Path>,
        built: &Built,
    ) -> Result<Vec<(&'static str, String)>> {
        let project = self.project();
        let target = project.target_dir();
        let base = format!("{}-{}", self.manifest.name, self.manifest.version);
        let row = |path: &Path, bytes: u64| {
            let name = path.file_name().map_or_else(
                || path.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            );
            format!("{name}   {}", ui::format_bytes(bytes))
        };
        let mut rows = Vec::new();

        if args.sources {
            // What the build compiled: every main root, and what the
            // `pre-compile` tasks generated.
            let generated = task::generated(&self.manifest, Hook::PreCompile)?;
            let sources = project.sources(Unit::Main, &generated.sources)?;
            let mut roots = project.roots(Unit::Main);
            roots.extend(generated.sources);
            let output = target.join(format!("{base}-sources.jar"));
            self.ui.phase(
                "Packaging",
                format!(
                    "{} ({})",
                    output.display(),
                    sources.describe("source files")
                ),
            );
            let outcome = package::write_sources_jar(&roots, &sources.files, &output)?;
            rows.push(("sources", row(&output, outcome.bytes)));
        }

        if args.javadoc {
            // Once per invocation, however a task's `depends-on` reached it.
            self.builtin(Builtin::Doc)?;
            let output = target.join(format!("{base}-javadoc.jar"));
            self.ui.phase("Packaging", output.display());
            let outcome = package::write_javadoc_jar(&target.join("doc"), &output)?;
            rows.push(("javadoc", row(&output, outcome.bytes)));
        }

        if args.dist {
            let (agents, agent_jars) = self.image_agents(lib_dir)?;
            let app = image::App {
                name: &self.manifest.name,
                version: &self.manifest.version,
                main_class: self.manifest.main_class.as_deref(),
                jar,
                lib_dir,
                jvm_args: &self.manifest.run.jvm_args,
                java_agents: &agents,
                agent_jars: &agent_jars,
            };
            let zip = target.join(format!("{base}.zip"));
            self.ui.phase(
                "Archiving",
                format!(
                    "{} (launchers in {})",
                    zip.display(),
                    target.join("dist").join(&base).join("bin").display()
                ),
            );
            let outcome = dist::write_dist(&app, &target.join("dist"), &zip)?;
            rows.push(("dist", row(&outcome.zip, outcome.bytes)));
        }

        if args.native_image {
            let toolchain = self.toolchain()?;
            let tool = native_image::find(&toolchain)?;
            let main_class = self.manifest.require_main_class("package --native-image")?;
            let mut classpath = vec![project.classes_dir()];
            classpath.extend(self.upstream_dirs(Classpath::runs));
            classpath.extend(built.resolution.runtime_classpath());
            let image = NativeImage {
                name: &self.manifest.name,
                main_class,
                classpath: &classpath,
                extra_args: &self.manifest.package.native_image_args,
            };
            let output_dir = target.join("native");
            self.ui.phase(
                "Building",
                format!("a native image of {main_class} in {}", output_dir.display()),
            );
            let scope = self.ui.spinner("Building", "a native image");
            let result =
                native_image::build(&tool, &image, &output_dir, &project.work_dir(), self.ui);
            scope.finish();
            let executable = result?;
            let bytes = std::fs::metadata(&executable).path(&executable)?.len();
            rows.push(("native", row(&executable, bytes)));
        }

        if args.obfuscate {
            let bytes = self.obfuscate_jar(args, jar, built)?;
            rows.push(("obfuscated", row(jar, bytes)));
        }

        if args.sbom {
            let output = target.join(format!("{base}-cyclonedx.json"));
            let bytes = self.write_sbom(&built.resolution, &output)?;
            rows.push(("sbom", row(&output, bytes)));
        }
        Ok(rows)
    }

    /// `--sbom`: the runtime graph as a `CycloneDX` document at `output`.
    /// Returns its size.
    fn write_sbom(&self, resolution: &Resolution, output: &Path) -> Result<u64> {
        let packages: Vec<&resolve::ResolvedPackage> = resolution
            .packages
            .iter()
            .filter(|p| p.classpath.runs())
            .collect();
        let locals: Vec<&resolve::LocalJar> = resolution
            .local
            .iter()
            .filter(|l| l.classpath.runs())
            .collect();
        let licenses = if packages.is_empty() {
            Vec::new()
        } else {
            self.licenses_of(&packages)?
        };
        self.ui.phase("Writing", output.display());
        let components: Vec<sbom::Component<'_>> = packages
            .iter()
            .zip(&licenses)
            .map(|(p, licenses)| sbom::Component {
                package: p,
                licenses,
                // A platform classifier's jar has no pin: this one's own hash.
                checksum: p.checksum.clone().or_else(|| {
                    let bytes = std::fs::read(p.jar.as_ref()?).ok()?;
                    Some(format!("sha256:{}", resolve::repo::sha256_hex(&bytes)))
                }),
            })
            .collect();
        for (p, licenses) in packages.iter().zip(&licenses) {
            if licenses.is_empty() && p.packaging != "pom" {
                self.ui
                    .verbose(format!("`{}` declares no licence", p.coord));
            }
        }
        let text = sbom::cyclonedx(&self.manifest, &components, &locals).render();
        std::fs::write(output, &text).path(output)?;
        Ok(text.len() as u64)
    }

    /// `--obfuscate`: rewrite the jar just written in place with `ProGuard`,
    /// and return the obfuscated jar's size in bytes.
    fn obfuscate_jar(&self, args: &PackageArgs, jar: &Path, built: &Built) -> Result<u64> {
        // Present: `package` checked it before the build (see `package`).
        let config = self
            .manifest
            .obfuscate
            .as_ref()
            .expect("--obfuscate requires [obfuscate]");
        let toolchain = self.toolchain()?;
        let tool_classpath = self.obfuscator_classpath()?;
        // The JDK's modules are always library jars; the dependency jars are
        // too unless a fat jar already holds their classes.
        let mut library_jars = obfuscate::jdk_library_jars(&toolchain);
        if !args.fat {
            library_jars.extend(self.upstream_dirs(Classpath::runs));
            library_jars.extend(built.resolution.runtime_classpath());
        }
        let obfuscation = Obfuscation {
            jar,
            tool_classpath: &tool_classpath,
            library_jars: &library_jars,
            main_class: self.manifest.main_class.as_deref(),
            keep: &config.keep,
            extra_args: &config.proguard_args,
        };
        self.ui.phase(
            "Obfuscating",
            format!("{} with ProGuard {}", jar.display(), config.version),
        );
        let started = Instant::now();
        let scope = self.ui.spinner("Obfuscating", "with ProGuard");
        let result = obfuscate::build(
            &toolchain.java,
            &obfuscation,
            &self.project().work_dir(),
            self.ui,
        );
        scope.finish();
        self.timings.since("obfuscate", started);
        result
    }
}

/// `run.java-agents` as an image's launchers name them, relative to the
/// application directory, and the pinned agents' jars with where they are
/// staged.
pub(super) type ImageAgents = (Vec<String>, Vec<(PathBuf, String)>);
