//! `jrs wrapper`, `jrs self` and `jrs completions`: jrs itself.

use std::path::{Path, PathBuf};

use clap::CommandFactory;

use super::{Cli, SelfCommand, manifest_path};
use crate::completions;
use crate::config::Config;
use crate::error::{IoResultExt, JrsError, Result, exit};
use crate::selfupdate::{self, Releases, Standing};
use crate::ui::Ui;

/// `jrs wrapper`: `jrsw` and `jrsw.cmd` beside the manifest, or in the
/// working directory when there is none (SPEC §4.3).
pub(super) fn wrapper_command(
    cli: &Cli,
    ui: &Ui,
    version: Option<&selfupdate::Version>,
) -> Result<i32> {
    if cli.global.offline {
        return Err(JrsError::usage(
            "`jrs wrapper` reads the release's checksums from GitHub, which --offline rules out",
        ));
    }
    let config = Config::load()?;
    for warning in &config.warnings {
        ui.warn(warning);
    }
    let version = version.cloned().unwrap_or_else(selfupdate::current_version);
    let dir = manifest_path(cli)
        .ok()
        .filter(|p| p.is_file())
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."));

    let releases = Releases::new(config.proxy.as_ref())?;
    ui.phase("Checking", format!("the checksums of jrs {version}"));
    let scope = ui.spinner(
        "Checking",
        format!("{}/download/v{version}", selfupdate::RELEASES),
    );
    let sums = releases.sums(&version);
    scope.finish();
    let targets = crate::wrapper::targets(&sums?);
    if targets.is_empty() {
        return Err(JrsError::resolve(format!(
            "the SHA256SUMS of jrs {version} lists no release archive"
        )));
    }

    let posix = dir.join(crate::wrapper::POSIX);
    std::fs::write(&posix, crate::wrapper::posix(&version, &targets)).path(&posix)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&posix, std::fs::Permissions::from_mode(0o755)).path(&posix)?;
    }
    let windows = dir.join(crate::wrapper::WINDOWS);
    std::fs::write(&windows, crate::wrapper::windows(&version, &targets)).path(&windows)?;
    ui.phase(
        "Wrote",
        format!(
            "{} and {} (jrs {version}, {} targets)",
            crate::wrapper::POSIX,
            crate::wrapper::WINDOWS,
            targets.len()
        ),
    );
    Ok(exit::SUCCESS)
}

/// `jrs self check` and `jrs self update`, against the GitHub releases.
pub(super) fn self_command(cli: &Cli, ui: &Ui, action: &SelfCommand) -> Result<i32> {
    if cli.global.offline {
        return Err(JrsError::usage(
            "`jrs self` asks GitHub for the latest release, which --offline rules out",
        ));
    }
    let config = Config::load()?;
    for warning in &config.warnings {
        ui.warn(warning);
    }
    let releases = Releases::new(config.proxy.as_ref())?;
    let current = selfupdate::current_version();

    let wanted = if let SelfCommand::Update { to: Some(to) } = action {
        if *to == current {
            ui.phase("Finished", format!("jrs {current} is already installed"));
            return Ok(exit::SUCCESS);
        }
        to.clone()
    } else {
        ui.phase(
            "Checking",
            format!("{} for the latest jrs", selfupdate::RELEASES),
        );
        let scope = ui.spinner("Checking", "the latest release");
        let latest = releases.latest();
        scope.finish();
        let latest = latest?;

        let standing = selfupdate::standing(&current, &latest);
        if matches!(action, SelfCommand::Check) || standing != Standing::Outdated {
            let msg = match standing {
                Standing::UpToDate => format!("jrs {current} is the latest release"),
                Standing::Ahead => {
                    format!("jrs {current} is newer than the latest release, {latest}")
                }
                Standing::Outdated => format!(
                    "jrs {current} is outdated; the latest release is {latest}, \
                     `jrs self update` installs it"
                ),
            };
            ui.phase("Finished", msg);
            return Ok(exit::SUCCESS);
        }
        latest
    };

    let target = selfupdate::target().ok_or_else(|| {
        JrsError::usage(format!(
            "no jrs release is built for {} on {}; build it from source, see \
             https://getjrs.dev/#install",
            std::env::consts::OS,
            std::env::consts::ARCH
        ))
    })?;
    let exe = selfupdate::current_exe()?;
    let asset = selfupdate::asset_name(target);
    ui.phase("Downloading", format!("{asset} ({wanted})"));
    let scope = ui.spinner("Downloading", &asset);
    let archive = releases.download(&wanted, target);
    scope.finish();
    let archive = archive?;

    let scratch = std::env::temp_dir().join(format!("jrs-self-update-{}", std::process::id()));
    let installed = (|| {
        let binary = selfupdate::unpack(&archive, target, &scratch)?;
        let version = selfupdate::probe(&binary)?;
        selfupdate::replace(&binary, &exe).map_err(|e| match e {
            JrsError::Io { source, .. }
                if source.kind() == std::io::ErrorKind::PermissionDenied =>
            {
                JrsError::usage(format!(
                    "{} is not writable by this user\n\nrun `jrs self update` as a \
                     user that can write there, or reinstall jrs somewhere you own",
                    exe.display()
                ))
            }
            e => e,
        })?;
        Ok(version)
    })();
    let _ = std::fs::remove_dir_all(&scratch);
    let installed = installed?;
    ui.phase(
        "Installed",
        format!("{installed} to {} (was {current})", exe.display()),
    );
    Ok(exit::SUCCESS)
}

pub(super) fn completions_command(ui: &Ui, shell: &str) -> Result<i32> {
    let shell = completions::Shell::parse(shell).ok_or_else(|| {
        JrsError::usage(format!(
            "`{shell}` is not a shell jrs writes completions for"
        ))
    })?;
    let script = completions::generate(shell, &Cli::command());
    for line in script.lines() {
        ui.println_out(line);
    }
    Ok(exit::SUCCESS)
}
