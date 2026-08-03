mod artifacts;
mod hermes;
mod model {
    pub use tracedecay_migrate::inventory::*;
}
mod project;
mod sqlite;

use std::collections::HashSet;
use std::path::Path;

pub use model::*;

use crate::config::TRACEDECAY_DIR;
use crate::errors::Result;
use crate::global_db;

pub async fn build_inventory(options: MigrationInventoryOptions) -> Result<MigrationInventory> {
    let profile_root = options
        .global_db_path
        .as_deref()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .map_or_else(crate::storage::default_profile_root, Ok)?;
    let lifecycle = crate::lifecycle_lease::acquire_exclusive_for_profile(
        &profile_root,
        "migration inventory",
    )?;
    let _database_scope = crate::db::enter_maintenance_database_scope(
        &lifecycle,
        &profile_root,
        "migration inventory",
    )?;
    build_inventory_in_scope(options).await
}

async fn build_inventory_in_scope(
    options: MigrationInventoryOptions,
) -> Result<MigrationInventory> {
    let mut stores = Vec::new();
    let mut skipped = Vec::new();
    let mut seen_data_dirs = HashSet::new();
    let explicit_global_db_path = options.global_db_path.is_some();
    let global_db_path = options.global_db_path.or_else(global_db::global_db_path);

    for root in &options.roots {
        project::scan_root(
            root,
            options.follow_symlinks,
            &mut seen_data_dirs,
            &mut stores,
            &mut skipped,
        )
        .await?;
    }
    let include_default_hermes_home = options.roots.is_empty() && !explicit_global_db_path;
    hermes::scan_hermes_sources(
        include_default_hermes_home,
        options.follow_symlinks,
        &mut seen_data_dirs,
        &mut stores,
        &mut skipped,
    )
    .await?;

    let global_db = match global_db_path {
        Some(path) => Some(
            sqlite::inspect_global_db(
                &path,
                explicit_global_db_path || global_db::global_db_path_is_overridden(),
            )
            .await,
        ),
        None => None,
    };
    let registered_project_keys = global_db
        .as_ref()
        .map(|global| project::canonical_path_set(&global.registered_project_paths))
        .unwrap_or_default();

    let include_registered_roots = options.roots.is_empty() || options.include_all_registered;
    if include_registered_roots {
        if let Some(global) = &global_db {
            for root in &global.registered_project_paths {
                let before = stores.len();
                project::inspect_data_dir_candidate(
                    root,
                    TRACEDECAY_DIR,
                    options.follow_symlinks,
                    &mut seen_data_dirs,
                    &mut stores,
                    &mut skipped,
                    StoreRole::CodeProjectStore,
                )
                .await?;
                if stores.len() == before {
                    stores.push(project::missing_registered_store(root));
                }
            }
        }
    }

    project::mark_registry_status(&mut stores, &registered_project_keys);
    stores.sort_by(|a, b| a.project_root.cmp(&b.project_root));
    skipped.sort_by(|a, b| a.path.cmp(&b.path));

    Ok(MigrationInventory {
        stores,
        skipped,
        global_db,
    })
}

#[cfg(test)]
mod prune_dir_tests {
    use super::project::should_prune_dir;

    #[test]
    fn prunes_shared_generated_segments_and_the_local_git_addition() {
        for name in [
            "node_modules",
            "target",
            "vendor",
            "dist",
            "build",
            ".next",
            ".venv",
            ".git", // site-local addition, not part of GENERATED_DIR_SEGMENTS
        ] {
            assert!(should_prune_dir(name), "{name} should be pruned");
        }
    }

    #[test]
    fn gains_segments_it_previously_missed_via_the_shared_list() {
        // These were absent from the old hand-maintained should_prune_dir
        // list but are part of the shared GENERATED_DIR_SEGMENTS union that
        // other call sites already recognized.
        for name in [".worktrees", "coverage", "out", ".cache", "venv"] {
            assert!(should_prune_dir(name), "{name} should now be pruned too");
        }
    }

    #[test]
    fn does_not_prune_real_source_dirs() {
        for name in ["src", "builder", "distributed"] {
            assert!(!should_prune_dir(name), "{name} is real source");
        }
    }
}
