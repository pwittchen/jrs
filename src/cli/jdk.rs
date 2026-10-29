//! `jrs jdk`: the JDKs jrs installs, and the compilers it warms.

use std::path::{Path, PathBuf};

use super::{Cli, JdkCommand, manifest_path};
use crate::compile::share;
use crate::compile::worker::Worker;
use crate::config::Config;
use crate::error::{JrsError, Result, exit};
use crate::manifest::Manifest;
use crate::resolve::cache::Cache;
use crate::toolchain::{self, Toolchain};
use crate::ui::{self, Ui};

/// `jrs jdk list` and `jrs jdk install` (SPEC §7.1).
pub(super) fn jdk_command(cli: &Cli, ui: &Ui, action: &JdkCommand) -> Result<i32> {
    let config = Config::load()?;
    for warning in &config.warnings {
        ui.warn(warning);
    }
    match action {
        JdkCommand::Install { version } => {
            if cli.global.offline {
                return Err(JrsError::usage(
                    "`jrs jdk install` downloads a JDK, which --offline rules out",
                ));
            }
            let home = install_jdk(ui, &config, *version)?;
            if let Ok(toolchain) = Toolchain::probe_home(&home) {
                if let Ok(cache) = Cache::discover() {
                    share::train_javac(&cache.root().join("cds"), &toolchain, true);
                }
                warm_compilers(ui, &toolchain);
            }
            Ok(exit::SUCCESS)
        }
        JdkCommand::List => {
            // The project here, if there is one, says which JDK it would pick.
            let picked = manifest_path(cli)
                .ok()
                .and_then(|path| Manifest::load(path).ok())
                .and_then(|manifest| {
                    let pin = toolchain::project_pin(manifest.java.jdk, &manifest.root);
                    let toolchain = match &pin {
                        Some(pin) => Toolchain::select_installed(pin, &config.jdks).ok()??,
                        None => Toolchain::discover().ok()?,
                    };
                    toolchain.home
                })
                .map(|home| home.canonicalize().unwrap_or(home));
            let visible = Toolchain::visible(&config.jdks);
            if visible.is_empty() {
                ui.phase("Finished", "jrs sees no JDK");
                return Ok(exit::SUCCESS);
            }
            let width = visible.iter().map(|j| j.version.len()).max().unwrap_or(0);
            let source = visible.iter().map(|j| j.source.len()).max().unwrap_or(0);
            for jdk in &visible {
                let home = jdk.home.canonicalize().unwrap_or_else(|_| jdk.home.clone());
                let mark = if picked.as_ref() == Some(&home) {
                    "  (this project)"
                } else {
                    ""
                };
                ui.println_out(format!(
                    "{:width$}  {:source$}  {}{mark}",
                    jdk.version,
                    jdk.source,
                    jdk.home.display()
                ));
            }
            Ok(exit::SUCCESS)
        }
    }
}

/// Whether `source` is one `javac` compiles.
pub(super) fn is_java(source: &Path) -> bool {
    source.extension().is_some_and(|e| e == "java")
}

/// Make the `javac` worker's cache for `toolchain` when there is none, and
/// wait for every cache still being made (`specs/BEATING_THE_DAEMON_3.md`
/// §2): for `jrs fetch` and `jrs jdk install`, where the user already waits,
/// so that the next build finds them.
pub(super) fn warm_compilers(ui: &Ui, toolchain: &Toolchain) {
    let Ok(cache) = Cache::discover() else {
        return;
    };
    let scope = ui.spinner("Warming", format!("javac on JDK {}", toolchain.version));
    Worker::javac_only(cache.root().join("worker")).train(toolchain, ui);
    share::join_background();
    scope.finish();
}

/// Download the newest GA build of JDK `feature` into `<cache>/jdks`, with
/// its phase lines, and return its home.
pub(super) fn install_jdk(ui: &Ui, config: &Config, feature: u32) -> Result<PathBuf> {
    let jdks = Cache::discover()?.root().join(crate::jdk::JDKS_DIR);
    let vendor = crate::jdk::Vendor::new(&config.jdk, config.proxy.as_ref())?;
    let scope = ui.spinner("Checking", format!("{} for JDK {feature}", config.jdk.url));
    let build = vendor.latest(feature);
    scope.finish();
    let build = build?;
    ui.phase(
        "Downloading",
        format!("JDK {} ({})", build.version, build.platform()),
    );
    let scope = ui.downloads(1);
    ui.update_live(|live| {
        if let ui::Live::Downloads(d) = live {
            d.active.push(ui::Transfer {
                id: 0,
                name: build.name.clone(),
                done: 0,
                total: build.size,
                verifying: false,
            });
        }
    });
    let archive = vendor.download(&build, &jdks, &mut |done, total| {
        ui.update_live(|live| {
            if let ui::Live::Downloads(d) = live
                && let Some(t) = d.active.first_mut()
            {
                t.done = done;
                t.total = total;
            }
        });
    });
    scope.finish();
    let archive = archive?;
    let scope = ui.spinner("Unpacking", &build.name);
    let home = crate::jdk::install(&archive, &build, feature, &jdks);
    scope.finish();
    let home = home?;
    ui.phase(
        "Installed",
        format!("JDK {} in {}", build.version, home.display()),
    );
    Ok(home)
}
