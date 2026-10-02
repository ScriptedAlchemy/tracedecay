use std::path::{Path, PathBuf};
use std::time::Duration;

use tracedecay_domain::errors::{ResettableStoreV1, Result, StoreResetRequiredV1, TraceDecayError};
use tracedecay_global_db::profile_registry_maintenance::verify_store_path_absent;
use tracedecay_hooks::{
    PRE_LEDGER_PENDING_WORK_DIR, PROFILE_HOOK_ADMISSIONS_DIR, PROJECT_HOOK_ADMISSIONS_DIR,
};
use tracedecay_runtime_core::config::ProfileRoot;
use tracedecay_runtime_core::storage::{
    SESSIONS_DB_FILENAME, profile_sharded_data_root, validate_project_id,
};
use tracedecay_sessions::runtime::USER_SESSIONS_DB_FILENAME;

use super::storage::{
    ProfileOfflineAuthority, join_outcome_and_restore, remove_fixed_profile_path,
    take_profile_offline,
};

const OPERATION: &str = "wipe --stale";

/// How long the reset waits for the operator of an unmanaged daemon to stop
/// it. ponytail: a fixed interactive bound; a daemon shutdown request would
/// let the reset stop an unmanaged daemon itself.
const UNMANAGED_DAEMON_STOP_TIMEOUT: Duration = Duration::from_secs(300);

/// Deletes exactly the stores the running daemon holds in their typed
/// reset-required state, nothing else. The daemon is the authority that
/// refused them, so the list is read from it before the profile is taken
/// offline; each deleted store is recreated empty on its next open.
#[hotpath::measure(label = "cli.wipe.stale", future = true)]
pub(crate) async fn handle_wipe_stale(profile: &ProfileRoot, assume_yes: bool) -> Result<()> {
    if !assume_yes {
        return Err(TraceDecayError::Config {
            message: "wipe --stale deletes every store the daemon reports as requiring reset; \
                      pass --yes to confirm. Nothing was wiped."
                .to_owned(),
        });
    }
    if !tracedecay_daemon_control::daemon_reachable(profile) {
        return Err(TraceDecayError::Config {
            message: "wipe --stale reads the stores that require reset from the running daemon; \
                      start it (`tracedecay daemon start`) and re-run. Nothing was wiped."
                .to_owned(),
        });
    }
    let stores = tracedecay_daemon_control::daemon_reset_required_stores(
        profile,
        crate::product_runtime::PRODUCT_BUILD_VERSION,
    )?;
    if stores.is_empty() {
        eprintln!("No store requires reset. Nothing was wiped.");
        return Ok(());
    }
    let targets = resettable_targets(&stores)?;
    let profile_root = profile.data_dir().to_path_buf();
    let profile_offline = take_stale_reset_offline(profile, &profile_root)?;
    let outcome = reset_stores(&profile_root, &targets);
    let restore = profile_offline.finish();
    join_outcome_and_restore(OPERATION, outcome, restore)
}

/// The managed daemon is quiesced and restored afterwards. An unmanaged daemon
/// (`tracedecay daemon run`) has no service to stop and its census ends with
/// it, so the census is read first and the reset waits for its operator to
/// stop it.
fn take_stale_reset_offline(
    profile: &ProfileRoot,
    profile_root: &Path,
) -> Result<ProfileOfflineAuthority> {
    if tracedecay_daemon_control::installed_service_state(profile)?
        != tracedecay_daemon_control::DaemonServiceState::Missing
    {
        return take_profile_offline(profile, profile_root, OPERATION);
    }
    eprintln!(
        "An unmanaged TraceDecay daemon holds the profile; stop it and wipe --stale continues \
         (waiting up to {}s).",
        UNMANAGED_DAEMON_STOP_TIMEOUT.as_secs()
    );
    let lease = tracedecay_runtime_core::lifecycle_lease::acquire_exclusive_with_timeout(
        profile_root,
        OPERATION,
        UNMANAGED_DAEMON_STOP_TIMEOUT,
    )?;
    Ok(ProfileOfflineAuthority::Lease(lease))
}

/// Every reported store as one this command resets on its own, or the
/// refusal naming the reset a store needs instead. Nothing is deleted unless
/// all of them are resettable.
fn resettable_targets(stores: &[StoreResetRequiredV1]) -> Result<Vec<ResettableStoreV1>> {
    stores
        .iter()
        .map(|store| {
            ResettableStoreV1::from_label(&store.store).ok_or_else(|| TraceDecayError::Config {
                message: format!(
                    "{} requires reset ({}) and is not reset on its own; run `{}`. Nothing was \
                     wiped.",
                    store.store, store.reason, store.remedy
                ),
            })
        })
        .collect()
}

fn reset_stores(profile_root: &Path, targets: &[ResettableStoreV1]) -> Result<()> {
    for target in targets {
        let (directory, members) = store_location(profile_root, target)?;
        let removed = match members {
            StoreMembers::DatabaseFamily(database) => remove_store_family(&directory, database)?,
            StoreMembers::Directories(names) => {
                let mut removed = 0;
                for name in names {
                    removed += usize::from(remove_fixed_profile_path(&directory, name)?);
                    verify_store_path_absent(&directory.join(name))?;
                }
                removed
            }
        };
        println!(
            "reset {} ({removed} entries removed from {}); the daemon recreates it empty",
            target.label(),
            directory.display()
        );
    }
    Ok(())
}

/// What a reset deletes inside a store's directory.
enum StoreMembers {
    /// A database and every entry named after it.
    DatabaseFamily(&'static str),
    /// These exact directories.
    Directories(&'static [&'static str]),
}

fn store_location(
    profile_root: &Path,
    target: &ResettableStoreV1,
) -> Result<(PathBuf, StoreMembers)> {
    let project_root = |project_id: &str| {
        validate_project_id(project_id).map_err(|message| TraceDecayError::Config {
            message: format!("daemon reported an invalid project id `{project_id}`: {message}"),
        })?;
        Ok::<_, TraceDecayError>(profile_sharded_data_root(profile_root, project_id))
    };
    Ok(match target {
        ResettableStoreV1::ProfileSessions => (
            profile_root.to_path_buf(),
            StoreMembers::DatabaseFamily(USER_SESSIONS_DB_FILENAME),
        ),
        ResettableStoreV1::ProjectSessions { project_id } => (
            project_root(project_id)?,
            StoreMembers::DatabaseFamily(SESSIONS_DB_FILENAME),
        ),
        ResettableStoreV1::ProfileHookAdmissions => (
            profile_root.to_path_buf(),
            StoreMembers::Directories(&[PROFILE_HOOK_ADMISSIONS_DIR]),
        ),
        ResettableStoreV1::ProjectHookAdmissions { project_id } => (
            project_root(project_id)?,
            StoreMembers::Directories(&[PROJECT_HOOK_ADMISSIONS_DIR, PRE_LEDGER_PENDING_WORK_DIR]),
        ),
    })
}

/// Removes `database` and every entry named after it in `directory`: its
/// SQLite sidecars and spools (`<name>.db…`), its host-admission directory
/// (`.<name>.db…`), and its session relation graph (`<name>.grafeo…`).
fn remove_store_family(directory: &Path, database: &str) -> Result<usize> {
    let stem = database.strip_suffix(".db").unwrap_or(database);
    let owned_prefix = format!("{stem}.");
    let hidden_prefix = format!(".{database}.");
    let entries = std::fs::read_dir(directory).map_err(|error| TraceDecayError::Config {
        message: format!("failed to list '{}': {error}", directory.display()),
    })?;
    let mut members = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| TraceDecayError::Config {
            message: format!("failed to list '{}': {error}", directory.display()),
        })?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(&owned_prefix) || name.starts_with(&hidden_prefix) {
            members.push(name);
        }
    }
    let mut removed = 0;
    for name in &members {
        removed += usize::from(remove_fixed_profile_path(directory, name)?);
        verify_store_path_absent(&directory.join(name))?;
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_family_removal_keeps_every_other_store_in_the_directory() {
        let directory = tempfile::TempDir::new().expect("store directory");
        let root = directory.path();
        for file in [
            "sessions.db",
            "sessions.db-wal",
            "sessions.db-shm",
            "sessions.grafeo",
            "tracedecay.db",
            "tracedecay.db-wal",
            "tracedecay.grafeo",
            "store_manifest.json",
            "user-sessions.db",
        ] {
            std::fs::write(root.join(file), file).expect("write store file");
        }
        for dir in [
            ".sessions.db.host-admission",
            "sessions.db.delivery-settlement-spool-v1",
            "code-index-v1",
        ] {
            std::fs::create_dir(root.join(dir)).expect("create store directory");
            std::fs::write(root.join(dir).join("entry"), dir).expect("write store entry");
        }

        assert_eq!(remove_store_family(root, "sessions.db").unwrap(), 6);

        let mut kept: Vec<String> = std::fs::read_dir(root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        kept.sort();
        assert_eq!(
            kept,
            [
                "code-index-v1",
                "store_manifest.json",
                "tracedecay.db",
                "tracedecay.db-wal",
                "tracedecay.grafeo",
                "user-sessions.db",
            ]
        );
    }

    #[test]
    fn a_store_that_is_not_resettable_on_its_own_refuses_the_whole_reset() {
        let store = |label: &str, remedy: &str| StoreResetRequiredV1 {
            store: label.to_owned(),
            authority: "observations".to_owned(),
            found_version: None,
            required_version: None,
            reason: "rows predate the unified identity".to_owned(),
            remedy: remedy.to_owned(),
        };
        assert_eq!(
            resettable_targets(&[
                store("profile sessions", "tracedecay wipe --stale --yes"),
                store(
                    "project sessions proj_a5b3d7e3ebe14ca7",
                    "tracedecay wipe --stale --yes"
                ),
                store("profile hook admissions", "tracedecay wipe --stale --yes"),
                store(
                    "project hook admissions proj_a5b3d7e3ebe14ca7",
                    "tracedecay wipe --stale --yes"
                ),
            ])
            .unwrap(),
            [
                ResettableStoreV1::ProfileSessions,
                ResettableStoreV1::ProjectSessions {
                    project_id: "proj_a5b3d7e3ebe14ca7".to_owned()
                },
                ResettableStoreV1::ProfileHookAdmissions,
                ResettableStoreV1::ProjectHookAdmissions {
                    project_id: "proj_a5b3d7e3ebe14ca7".to_owned()
                },
            ]
        );
        assert_eq!(
            resettable_targets(&[
                store("profile sessions", "tracedecay wipe --stale --yes"),
                store("profile authority", "tracedecay wipe --all --yes"),
            ])
            .unwrap_err()
            .to_string(),
            "config error: profile authority requires reset (rows predate the unified identity) \
             and is not reset on its own; run `tracedecay wipe --all --yes`. Nothing was wiped."
        );
    }
}
