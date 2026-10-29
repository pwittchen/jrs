//! `jrs add` and `jrs remove`: editing `jrs.toml`'s dependency tables.

use super::session::Session;
use super::{Cli, manifest_path};
use crate::compile::{Language, lang};
use crate::config::Config;
use crate::edit;
use crate::error::{IoResultExt, JrsError, Result, exit};
use crate::manifest::{self, Dependency, Manifest};
use crate::resolve::coord::Ga;
use crate::resolve::{self, metadata};
use crate::ui::Ui;
use crate::workspace::Workspace;

/// `only` is `(--compile-only, --runtime-only)`, which clap keeps from both
/// being set.
pub(super) fn add_command(
    cli: &Cli,
    ui: &Ui,
    coordinates: &[String],
    dev: bool,
    only: (bool, bool),
) -> Result<i32> {
    add_to(cli, ui, &Session::open(cli, ui)?, coordinates, dev, only)
}

/// `jrs add` into the manifest `session` is over.
pub(super) fn add_to(
    cli: &Cli,
    ui: &Ui,
    session: &Session,
    coordinates: &[String],
    dev: bool,
    only: (bool, bool),
) -> Result<i32> {
    let (compile_only, runtime_only) = only;
    let section = if dev {
        "dev-dependencies"
    } else {
        "dependencies"
    };
    let fetcher = session.fetcher()?;
    // What `[managed]` and its BOMs cover can go in without a version, so
    // that upgrading the BOM upgrades it too.
    let managed = if session.manifest.managed.is_empty() {
        resolve::ManagedVersions::default()
    } else {
        resolve::managed_versions(&session.manifest, &fetcher)?
    };

    let mut additions = Vec::new();
    for coordinate in coordinates {
        let parts: Vec<&str> = coordinate.split(':').collect();
        let bad = || {
            JrsError::usage(format!(
                "`{coordinate}` is not `group:artifact`, `group:artifact:version` or \
                 `group:artifact:version:classifier`"
            ))
        };
        if parts.iter().any(|p| p.trim().is_empty()) {
            return Err(bad());
        }
        let (group, artifact, version, classifier) = match parts.as_slice() {
            [g, a] => (*g, *a, None, None),
            [g, a, v] => (*g, *a, Some(v.to_string()), None),
            [g, a, v, c] => (*g, *a, Some(v.to_string()), Some(c.to_string())),
            _ => return Err(bad()),
        };
        let version = if let Some(v) = version {
            v
        } else if managed.get(group, artifact).is_some() {
            String::new()
        } else {
            ui.phase(
                "Looking up",
                format!("the newest release of {group}:{artifact}"),
            );
            let known = fetcher.metadata(group, artifact)?;
            metadata::newest_release(&known).ok_or_else(|| {
                JrsError::resolve(format!(
                    "`{group}:{artifact}` has no stable release to add\n\n\
                     name a version: `jrs add {group}:{artifact}:<version>`"
                ))
            })?
        };
        if let Some(scala) = session.manifest.language(Language::Scala)
            && let Some(suffix) = lang::cross_build_suffix(artifact)
            && format!("_{suffix}") != lang::scala_suffix(&scala.version)
        {
            ui.warn(format!(
                "`{group}:{artifact}` is built for Scala {suffix}, but [scala] is Scala {}; \
                 jrs adds it as written, and does not rewrite the suffix",
                scala.version
            ));
        }
        let mut dep = Dependency::new(group, artifact, version);
        dep.classifier = classifier;
        dep.compile_only = compile_only;
        dep.runtime_only = runtime_only;
        additions.push(dep);
    }

    edit_manifest(cli, ui, &session.manifest, |text| {
        let mut text = text.to_string();
        for dep in &additions {
            let (key, value) = manifest::dependency_entry(dep);
            let edited = edit::upsert(&text, section, &key, &value)?;
            match &edited {
                edit::Edited::Added(_) if dep.is_managed() => ui.phase(
                    "Adding",
                    format!("{dep} to [{section}], at the version [managed] gives it"),
                ),
                edit::Edited::Added(_) => ui.phase("Adding", format!("{dep} to [{section}]")),
                edit::Edited::Replaced { previous, .. } => {
                    ui.phase("Updating", format!("{key} from {previous} to {value}"));
                }
            }
            text = edited.text().to_string();
        }
        Ok(text)
    })
}

pub(super) fn remove_command(cli: &Cli, ui: &Ui, keys: &[String], dev: bool) -> Result<i32> {
    remove_from(cli, ui, &Session::open(cli, ui)?, keys, dev)
}

/// `jrs remove` from the manifest `session` is over.
pub(super) fn remove_from(
    cli: &Cli,
    ui: &Ui,
    session: &Session,
    keys: &[String],
    dev: bool,
) -> Result<i32> {
    let sections: &[&str] = if dev {
        &["dev-dependencies"]
    } else {
        &["dependencies", "dev-dependencies"]
    };
    let local = |key: &str| {
        let m = &session.manifest;
        m.dependencies
            .iter()
            .chain(&m.dev_dependencies)
            .any(|d| d.is_local() && d.key() == key)
    };
    for key in keys {
        if !local(key) && (Ga::parse(key).is_none() || key.split(':').count() > 3) {
            return Err(JrsError::usage(format!(
                "`{key}` is not a `group:artifact` key, as jrs.toml names dependencies"
            )));
        }
    }
    let path = session.manifest.path.clone();
    edit_manifest(cli, ui, &session.manifest, |text| {
        let mut text = text.to_string();
        for key in keys {
            let mut removed = false;
            for section in sections {
                if let Some(edited) = edit::remove(&text, section, key)? {
                    ui.phase("Removing", format!("{key} from [{section}]"));
                    text = edited;
                    removed = true;
                    break;
                }
            }
            if !removed {
                return Err(JrsError::usage(format!(
                    "`{key}` is not declared in {}",
                    path.display()
                )));
            }
        }
        Ok(text)
    })
}

/// Rewrite `jrs.toml` through `change`, then resolve against the result.
///
/// The edited text must parse back as a manifest before it is written, and if
/// the new graph does not resolve — a coordinate that does not exist, say —
/// the original file is put back, so a failed `jrs add` leaves nothing behind.
fn edit_manifest(
    cli: &Cli,
    ui: &Ui,
    current: &Manifest,
    change: impl FnOnce(&str) -> Result<String>,
) -> Result<i32> {
    let path = &current.path;
    let original = std::fs::read_to_string(path).path(path)?;
    let edited = change(&original)?;
    // A workspace member's manifest reads only with what it inherits from
    // the root: it is checked by loading the workspace once it is written,
    // and put back if that fails.
    let in_workspace = Workspace::find(&manifest_path(cli)?)?.is_some();
    if !in_workspace {
        Manifest::parse(&edited, path, &current.root).map_err(|e| {
            JrsError::manifest(format!(
                "the edit would leave {} unreadable, so it was not written:\n\n{e}",
                path.display()
            ))
        })?;
    }
    std::fs::write(path, &edited).path(path)?;

    let resolved = if in_workspace {
        Workspace::find(&manifest_path(cli)?).and_then(|found| {
            let (workspace, _) = found.ok_or_else(|| {
                JrsError::manifest(format!("{} left the workspace", path.display()))
            })?;
            let (aggregate, _) = workspace.aggregate()?;
            let mut session = Session::new(cli, ui, aggregate, Config::load()?);
            session.workspace_members = workspace.member_dirs();
            let resolution = session.dependencies(false)?;
            Ok((session.manifest.lock_path(), resolution.packages.len()))
        })
    } else {
        Session::open(cli, ui).and_then(|s| {
            let resolution = s.dependencies(false)?;
            Ok((s.manifest.lock_path(), resolution.packages.len()))
        })
    };
    match resolved {
        Ok((lock, packages)) => {
            ui.phase(
                "Updated",
                format!("{} ({packages} packages)", path.display()),
            );
            if packages > 0 {
                ui.verbose(format!("resolved into {}", lock.display()));
            }
            Ok(exit::SUCCESS)
        }
        Err(e) => {
            std::fs::write(path, &original).path(path)?;
            ui.phase(
                "Restored",
                format!("{} to what it was before", path.display()),
            );
            Err(e)
        }
    }
}
