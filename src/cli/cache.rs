//! `jrs cache`: the shared artifact cache.

use std::collections::HashSet;
use std::time::Duration;

use super::{CacheCommand, Cli, manifest_path};
use crate::error::{JrsError, Result, exit};
use crate::lockfile::Lockfile;
use crate::manifest;
use crate::resolve::cache::{Cache, Prune};
use crate::ui::{self, Ui};

pub(super) fn cache_command(cli: &Cli, ui: &Ui, action: &CacheCommand) -> Result<i32> {
    let cache = Cache::discover()?;
    match action {
        CacheCommand::Path => {
            ui.println_out(cache.root().display());
            Ok(exit::SUCCESS)
        }
        CacheCommand::Prune {
            unused_for,
            dry_run,
            jdks,
        } => {
            // The project jrs is run from is a user of the cache whether or
            // not it was built since projects started being recorded.
            if let Ok(path) = manifest_path(cli) {
                let lock = path.with_file_name(manifest::LOCK_FILE);
                if lock.is_file() {
                    cache.register_project(&lock)?;
                }
            }
            let rule = match unused_for {
                Some(days) => Prune::UnusedFor(Duration::from_secs(days * 24 * 60 * 60)),
                None => Prune::Unreferenced(referenced(ui, &cache)?),
            };
            ui.phase(
                if *dry_run { "Checking" } else { "Pruning" },
                cache.root().display(),
            );
            let pruned = cache.prune_with(&rule, *dry_run, *jdks)?;
            if *dry_run {
                ui.suspend();
                for dir in &pruned.removed {
                    ui.println_out(dir);
                }
            }
            ui.phase(
                if *dry_run { "Would remove" } else { "Removed" },
                format!(
                    "{} artifacts ({}); kept {}",
                    pruned.removed.len(),
                    ui::format_bytes(pruned.bytes),
                    pruned.kept
                ),
            );
            Ok(exit::SUCCESS)
        }
    }
}

/// Every version directory a known project's `jrs.lock` names.
fn referenced(ui: &Ui, cache: &Cache) -> Result<HashSet<String>> {
    let projects = cache.forget_missing_projects()?;
    if projects.is_empty() {
        return Err(JrsError::usage(format!(
            "jrs has no record of a project using {}, so it cannot tell what is \
             still needed\n\nbuild your projects once so they are recorded, or prune \
             by age instead: `jrs cache prune --unused-for 30`",
            cache.root().display()
        )));
    }
    let mut keep = HashSet::new();
    for lock in &projects {
        // A lockfile that cannot be read could name anything: rather than guess,
        // keep everything.
        let parsed = Lockfile::load(lock).map_err(|e| {
            JrsError::usage(format!(
                "{} could not be read, so nothing was pruned: {e}",
                lock.display()
            ))
        })?;
        // The compilers it pins are its too.
        if let Some(lock) = parsed {
            for package in lock.all_packages() {
                keep.insert(package.coord.version_dir());
            }
        }
    }
    ui.verbose(format!(
        "{} projects use this cache: {}",
        projects.len(),
        projects
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    ));
    Ok(keep)
}
