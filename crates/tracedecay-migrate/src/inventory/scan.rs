//! Preflight scanners that open stores and the registry to build an
//! inventory. The record vocabulary they populate lives in the parent module.

use std::collections::HashSet;
use std::path::Path;

use super::*;

use crate::root_seam::config::TRACEDECAY_DIR;
use crate::root_seam::errors::Result;
use crate::root_seam::global_db;

pub async fn build_inventory(options: MigrationInventoryOptions) -> Result<MigrationInventory> {
    let profile_root = options
        .global_db_path
        .as_deref()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .map_or_else(crate::root_seam::storage::default_profile_root, Ok)?;
    let lifecycle = crate::root_seam::lifecycle_lease::acquire_exclusive_for_profile(
        &profile_root,
        "migration inventory",
    )?;
    let _database_scope = crate::root_seam::db::enter_maintenance_database_scope(
        &lifecycle,
        &profile_root,
        "migration inventory",
    )?;
    let identity = crate::root_seam::daemon::profile_identity::load_or_create(&profile_root)?;
    let registry =
        match crate::root_seam::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1::open(
            identity,
        )
        .await
        {
            Ok(registry) => registry,
            Err(_) => return build_inventory_in_scope(options, None).await,
        };
    match registry.profile_database().await {
        Ok(global_db) => build_inventory_in_scope(options, Some(global_db.as_ref())).await,
        // Inventory is also the recovery path for a corrupt or otherwise
        // unreadable registry. Retain the exclusive maintenance boundary, but
        // inspect the file as immutable metadata when no runtime can attach it.
        Err(_) => build_inventory_in_scope(options, None).await,
    }
}

/// Builds a read-only inventory through the daemon's existing database
/// authority instead of opening a second process-local database client.
pub(crate) async fn build_inventory_for_daemon(
    options: MigrationInventoryOptions,
    global_db: &global_db::RegisteredGlobalDb,
) -> Result<MigrationInventory> {
    if options
        .global_db_path
        .as_deref()
        .is_some_and(|path| path != global_db.db_path())
    {
        return Err(crate::root_seam::errors::TraceDecayError::Config {
            message: "daemon migration inventory cannot inspect a different profile database"
                .to_string(),
        });
    }
    build_inventory_in_scope(options, Some(global_db)).await
}

async fn build_inventory_in_scope(
    options: MigrationInventoryOptions,
    daemon_global_db: Option<&global_db::RegisteredGlobalDb>,
) -> Result<MigrationInventory> {
    let mut stores = Vec::new();
    let mut skipped = Vec::new();
    let mut seen_data_dirs = HashSet::new();
    let explicit_global_db_path = options.global_db_path.is_some();
    let global_db_path = options
        .global_db_path
        .clone()
        .or_else(global_db::global_db_path);
    let scan_options = project::InventoryScanOptions {
        follow_symlinks: options.follow_symlinks,
        integrity: options.integrity,
    };

    for root in &options.roots {
        project::scan_root(
            root,
            scan_options,
            &mut seen_data_dirs,
            &mut stores,
            &mut skipped,
        )
        .await?;
    }
    let include_default_hermes_home = options.roots.is_empty() && !explicit_global_db_path;
    hermes::scan_hermes_sources(
        include_default_hermes_home,
        scan_options,
        &mut seen_data_dirs,
        &mut stores,
        &mut skipped,
    )
    .await?;

    let global_db = match (global_db_path, daemon_global_db) {
        (_, Some(global_db)) => Some(
            sqlite::inspect_daemon_global_db(
                global_db,
                explicit_global_db_path || global_db::global_db_path_is_overridden(),
                options.integrity,
            )
            .await,
        ),
        (Some(path), None) => Some(
            sqlite::inspect_global_db(
                &path,
                explicit_global_db_path || global_db::global_db_path_is_overridden(),
                options.integrity,
            )
            .await,
        ),
        (None, None) => None,
    };
    let registered_project_keys = global_db
        .as_ref()
        .map(|global| project::canonical_path_set(&global.registered_project_paths))
        .unwrap_or_default();

    let include_registered_roots = options.roots.is_empty() || options.include_all_registered;
    let mut inventory_roots = options.roots.clone();
    if include_registered_roots && let Some(global) = &global_db {
        inventory_roots.extend(global.registered_project_paths.iter().cloned());
    }
    let mut seen_roots = HashSet::new();
    for root in inventory_roots {
        let root_key = project::canonicalize_lossy(&root);
        if !seen_roots.insert(root_key.clone()) {
            continue;
        }

        if include_registered_roots {
            project::inspect_data_dir_candidate(
                &root,
                TRACEDECAY_DIR,
                scan_options,
                &mut seen_data_dirs,
                &mut stores,
                &mut skipped,
                StoreRole::CodeProjectStore,
            )
            .await?;
        }

        let profile_root = global_db
            .as_ref()
            .and_then(|inventory| inventory.path.parent());
        let profile_data_root =
            if let (Some(db), Some(profile_root)) = (daemon_global_db, profile_root) {
                db.try_resolve_project_store_record_by_alias(&root)
                    .await?
                    .and_then(|store| {
                        crate::root_seam::storage::classify_registry_storage(&root, profile_root, &store)
                    })
                    .map(|location| location.data_root)
            } else if let Some(profile_root) = profile_root {
                crate::root_seam::storage::resolve_layout(&root, profile_root)
                    .ok()
                    .map(|layout| layout.data_root)
            } else {
                None
            };
        if let Some(data_root) = profile_data_root.as_ref()
            && data_root.is_dir()
        {
            project::inspect_data_dir(
                &root,
                data_root,
                scan_options,
                &mut seen_data_dirs,
                &mut stores,
                &mut skipped,
                StoreRole::CodeProjectStore,
            )
            .await?;
        }

        let registered = registered_project_keys.contains(&root_key);
        let inventoried = stores
            .iter()
            .any(|store| project::canonicalize_lossy(&store.project_root) == root_key);
        if registered && !inventoried {
            if let Some(data_root) = profile_data_root {
                stores.push(project::missing_registered_store_at(&root, data_root));
            } else if include_registered_roots {
                stores.push(project::missing_registered_store(&root));
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
