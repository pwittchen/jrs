//! `jrs migrate`: a `jrs.toml` from a Maven or Gradle build.

use std::path::{Path, PathBuf};

use super::session::Session;
use super::{Cli, counted};
use crate::config::Config;
use crate::error::{Result, exit};
use crate::ui::Ui;
use crate::{migrate, resolve};

pub(super) fn migrate_command(
    cli: &Cli,
    ui: &Ui,
    from: Option<&str>,
    dry_run: bool,
    force: bool,
    path: Option<&Path>,
) -> Result<i32> {
    ui.banner();

    let root = path.map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    let source = from.map(migrate::Source::parse).transpose()?;
    if let Some(workspace) = migrate::plan_workspace(&root, source)? {
        return migrate_workspace(ui, &workspace, &root, dry_run, force);
    }
    let mut migration = migrate::plan(&root, source)?;

    ui.phase(
        "Migrating",
        format!(
            "{} ({})",
            migration.source_file.display(),
            migration.source.as_str()
        ),
    );
    if migration.source == migrate::Source::Gradle {
        compare_with_highest_wins(cli, ui, &mut migration);
    }

    // The report and the manifest are real output: they belong on stdout so they
    // can be piped, diffed or redirected.
    ui.suspend();
    if let Some(preamble) = &migration.report.preamble {
        ui.println_out(preamble);
        ui.println_out("");
    }

    print_block(ui, "Migrated", &migration.report.migrated);
    print_block(ui, "Needs review", &migration.report.needs_review);
    print_block(ui, "Not migrated", &migration.report.not_migrated);

    if dry_run {
        ui.println_out("--- jrs.toml (dry run; nothing was written) ---");
        for line in migration.render_manifest().lines() {
            ui.println_out(line);
        }
        return Ok(exit::SUCCESS);
    }

    if let Some(written) = migrate::write(&migration, &root, force)? {
        ui.phase("Created", written.display());
    }
    ui.println_out("");
    ui.println_out("Next: `jrs build`, then `jrs tree` to compare the resolved graph");
    ui.println_out("against `mvn dependency:tree` / `gradle dependencies`.");
    Ok(exit::SUCCESS)
}

/// Resolve a migrated Gradle build once, so that the report names where
/// jrs's nearest-wins and Gradle's highest-wins part ways (SPEC §13.6). A
/// graph that does not resolve is said in the report and never fails the
/// migration: `jrs build` says why in full.
fn compare_with_highest_wins(cli: &Cli, ui: &Ui, migration: &mut migrate::Migration) {
    let what = "the migrated graph, to compare it with Gradle's highest-wins";
    ui.phase("Resolving", what);
    let compared = Config::load().and_then(|config| {
        for warning in &config.warnings {
            ui.warn(warning);
        }
        let session = Session::new(cli, ui, migration.manifest.clone(), config);
        let fetcher = session.fetcher()?;
        let scope = ui.spinner("Resolving", what);
        let compared =
            resolve::resolve_comparing_highest(&session.manifest, &fetcher, session.jobs);
        scope.finish();
        compared
    });
    match compared {
        Ok((_, disagreements)) => migration.report.highest_wins(&disagreements),
        Err(e) => {
            let e = e.to_string();
            migration.report.review(format!(
                "the migrated dependencies did not resolve ({}), so where jrs's \
                 nearest-wins and Gradle's highest-wins pick different versions was \
                 not checked",
                e.lines().next().unwrap_or_default()
            ));
        }
    }
}

/// `jrs migrate` of a multi-module build: the root's report, then a section
/// per member, then every file (`specs/WORKSPACES.md` §8.3).
fn migrate_workspace(
    ui: &Ui,
    workspace: &migrate::workspace::WorkspaceMigration,
    root: &Path,
    dry_run: bool,
    force: bool,
) -> Result<i32> {
    ui.phase(
        "Migrating",
        format!(
            "{} ({}, a workspace of {})",
            workspace.source_file.display(),
            workspace.source.as_str(),
            counted(workspace.members.len(), "member")
        ),
    );
    ui.suspend();
    if let Some(preamble) = &workspace.report.preamble {
        ui.println_out(preamble);
        ui.println_out("");
    }
    ui.println_out("== workspace ==");
    ui.println_out("");
    print_block(ui, "Migrated", &workspace.report.migrated);
    print_block(ui, "Needs review", &workspace.report.needs_review);
    print_block(ui, "Not migrated", &workspace.report.not_migrated);
    for (dir, member) in &workspace.members {
        ui.println_out(format!("== {} ({dir}) ==", member.manifest.name));
        ui.println_out("");
        print_block(ui, "Migrated", &member.report.migrated);
        print_block(ui, "Needs review", &member.report.needs_review);
        print_block(ui, "Not migrated", &member.report.not_migrated);
    }
    ui.println_out(format!(
        "{} of {} members migrated without anything to review",
        workspace.clean(),
        workspace.members.len()
    ));
    ui.println_out("");

    if dry_run {
        for (file, text) in workspace.files() {
            ui.println_out(format!("--- {file} (dry run; nothing was written) ---"));
            for line in text.lines() {
                ui.println_out(line);
            }
        }
        return Ok(exit::SUCCESS);
    }
    for written in migrate::workspace::write(workspace, root, force)? {
        ui.phase("Created", written.display());
    }
    ui.println_out("");
    ui.println_out("Next: `jrs build` at the root builds every member; `jrs tree` shows each");
    ui.println_out(
        "member's graph to compare against `mvn dependency:tree` / `gradle dependencies`.",
    );
    Ok(exit::SUCCESS)
}

fn print_block(ui: &Ui, title: &str, entries: &[String]) {
    ui.println_out(format!("{title}:"));
    if entries.is_empty() {
        ui.println_out("  (nothing)");
    } else {
        for entry in entries {
            ui.println_out(format!("  - {entry}"));
        }
    }
    ui.println_out("");
}
