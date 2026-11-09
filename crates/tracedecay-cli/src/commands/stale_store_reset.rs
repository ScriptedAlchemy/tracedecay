use std::path::{Path, PathBuf};
use std::time::Duration;

use tracedecay_domain::errors::{ResettableStoreV1, Result, StoreResetRequiredV1, TraceDecayError};
use tracedecay_global_db::profile_registry_maintenance::verify_store_path_absent;
use tracedecay_hooks::{PROFILE_HOOK_ADMISSIONS_DIR, PROJECT_HOOK_ADMISSIONS_DIR};
use tracedecay_runtime_core::config::{GLOBAL_DB_FILENAME, ProfileRoot};
use tracedecay_runtime_core::shard_runtime::registry::lock_offline_sqlite_family_for_reset;
use tracedecay_runtime_core::storage::{
    SESSIONS_DB_FILENAME, STORE_MANIFEST_FILENAME, USER_SESSIONS_DB_FILENAME,
    profile_sharded_data_root, read_store_manifest, validate_project_id,
};

use super::storage::{
    ProfileOfflineAuthority, RestoreWording, join_outcome_and_restore, remove_fixed_profile_path,
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
#[tracing::instrument(name = "cli.wipe.stale", level = "trace", skip_all)]
pub(crate) async fn handle_wipe_stale(profile: &ProfileRoot, assume_yes: bool) -> Result<()> {
    if !assume_yes {
        return Err(TraceDecayError::Config {
            message: "wipe --stale deletes every store the daemon reports as requiring reset; \
                      pass --yes to confirm. Nothing was wiped."
                .to_owned(),
        });
    }
    if !tracedecay_daemon_control::daemon_reachable(
        profile,
        crate::product_runtime::PRODUCT_BUILD_VERSION,
    ) {
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
    join_outcome_and_restore(RestoreWording::ManagedService, OPERATION, outcome, restore)
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
        if *target == ResettableStoreV1::ProfileAuthority {
            println!(
                "resetting the profile authority loses the project registry, usage accounting \
                 and remote-deletion records it holds; every project store (code index, \
                 graph, sessions, memory) stays in place"
            );
        }
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
    if targets.contains(&ResettableStoreV1::ProfileAuthority) {
        print_project_registrations(profile_root)?;
    }
    Ok(())
}

/// Each project store records its own root and identity, so the commands
/// that register it in the recreated profile authority are derived from its
/// manifest.
fn print_project_registrations(profile_root: &Path) -> Result<()> {
    let projects_root = profile_root.join("projects");
    let entries = match std::fs::read_dir(&projects_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(TraceDecayError::Config {
                message: format!("failed to list '{}': {error}", projects_root.display()),
            });
        }
    };
    let mut manifests = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| TraceDecayError::Config {
            message: format!("failed to list '{}': {error}", projects_root.display()),
        })?;
        let path = entry.path().join(STORE_MANIFEST_FILENAME);
        if path.is_file() {
            manifests.push(path);
        }
    }
    manifests.sort();
    let mut lines = Vec::new();
    for path in manifests {
        lines.push(match read_store_manifest(&path) {
            Ok(manifest) if manifest.project_root.is_dir() => format!(
                "  tracedecay init {}",
                shell_words::quote(&manifest.project_root.to_string_lossy())
            ),
            Ok(manifest) => format!(
                "  (not registrable: {} names {}, which no longer exists)",
                path.display(),
                manifest.project_root.display()
            ),
            Err(error) => format!("  (not registrable: {}: {error})", path.display()),
        });
    }
    if !lines.is_empty() {
        println!("the project registry is empty; register each project store again with:");
        for line in lines {
            println!("{line}");
        }
    }
    Ok(())
}

/// What a reset deletes inside a store's directory.
enum StoreMembers {
    /// The SQLite family and its registered store artifacts.
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
        ResettableStoreV1::ProfileAuthority => (
            profile_root.to_path_buf(),
            StoreMembers::DatabaseFamily(GLOBAL_DB_FILENAME),
        ),
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
            StoreMembers::Directories(&[PROJECT_HOOK_ADMISSIONS_DIR]),
        ),
    })
}

/// The offline lifecycle authority admits deletion of the stale store
/// itself. Similar names, private initialization data and stable coordination
/// locks are not members of its data family.
fn remove_store_family(directory: &Path, database: &str) -> Result<usize> {
    let _initialization =
        lock_offline_sqlite_family_for_reset(&directory.join(database)).map_err(|error| {
            TraceDecayError::Config {
                message: format!("cannot reset SQLite family {database}: {error}"),
            }
        })?;
    let mut members: Vec<(String, bool)> = ["", "-wal", "-shm", "-journal"]
        .iter()
        .map(|suffix| (format!("{database}{suffix}"), false))
        .collect();
    if database == SESSIONS_DB_FILENAME || database == USER_SESSIONS_DB_FILENAME {
        let stem = database.strip_suffix(".db").unwrap_or(database);
        let graph = format!("{stem}.grafeo");
        members.extend([
            // A graph container owns its verification marker beside it:
            // sessions.grafeo -> sessions.verified (also user-sessions).
            (format!("{stem}.verified"), false),
            (format!("{graph}.wal"), true),
            (graph, false),
            (format!(".{database}.host-admission"), true),
            (format!("{database}.delivery-settlement-spool-v1"), true),
        ]);
    }
    // Validate all exact names before deleting the first file. The exclusive
    // offline lease then excludes other cooperating profile writers.
    for (name, is_directory) in &members {
        let path = directory.join(name);
        tracedecay_runtime_core::storage::reject_symlink_components(
            &path,
            "stale store reset member",
        )
        .map_err(|error| TraceDecayError::Config {
            message: error.to_string(),
        })?;
        match std::fs::symlink_metadata(&path) {
            Ok(metadata)
                if (*is_directory && metadata.is_dir())
                    || (!is_directory && metadata.is_file()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Ok(_) => {
                return Err(TraceDecayError::Config {
                    message: format!(
                        "unexpected file type for stale store reset member '{}'",
                        path.display()
                    ),
                });
            }
            Err(error) => {
                return Err(TraceDecayError::Config {
                    message: format!(
                        "cannot inspect stale store reset member '{}': {error}",
                        path.display()
                    ),
                });
            }
        }
    }
    let mut removed = 0;
    for (name, _) in &members {
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
            "sessions.verified",
            "user-sessions.verified",
            "tracedecay.verified",
            "tracedecay.db",
            "tracedecay.db-wal",
            "tracedecay.grafeo",
            "store_manifest.json",
            "user-sessions.db",
            "global.db",
            "global.db-wal",
            "global.db-shm",
            "profile-identity.json",
            "sessions.unrelated",
            ".sessions.db.notes",
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

        assert_eq!(remove_store_family(root, "sessions.db").unwrap(), 7);
        assert_eq!(remove_store_family(root, GLOBAL_DB_FILENAME).unwrap(), 3);

        let mut kept: Vec<String> = std::fs::read_dir(root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        kept.sort();
        assert_eq!(
            kept,
            [
                ".sessions.db.notes",
                "code-index-v1",
                "profile-identity.json",
                "sessions.unrelated",
                "store_manifest.json",
                "tracedecay.db",
                "tracedecay.db-wal",
                "tracedecay.grafeo",
                "tracedecay.verified",
                "user-sessions.db",
                "user-sessions.verified",
            ]
        );
    }


    #[test]
    fn profile_sessions_reset_removes_only_its_graph_verification_marker() {
        let directory = tempfile::TempDir::new().unwrap();
        let root = directory.path();
        for name in [
            "sessions.db",
            "sessions.verified",
            "user-sessions.db",
            "user-sessions.verified",
        ] {
            std::fs::write(root.join(name), name).unwrap();
        }

        assert_eq!(remove_store_family(root, USER_SESSIONS_DB_FILENAME).unwrap(), 2);
        assert!(!root.join("user-sessions.db").exists());
        assert!(!root.join("user-sessions.verified").exists());
        assert_eq!(std::fs::read(root.join("sessions.db")).unwrap(), b"sessions.db");
        assert_eq!(
            std::fs::read(root.join("sessions.verified")).unwrap(),
            b"sessions.verified"
        );
    }

    #[test]
    fn unexpected_graph_verification_marker_kind_preserves_the_store() {
        let directory = tempfile::TempDir::new().unwrap();
        let root = directory.path();
        std::fs::write(root.join("sessions.db"), b"store").unwrap();
        let marker = root.join("sessions.verified");
        std::fs::create_dir(&marker).unwrap();
        std::fs::write(marker.join("foreign"), b"preserve").unwrap();

        assert!(remove_store_family(root, SESSIONS_DB_FILENAME).is_err());
        assert_eq!(std::fs::read(root.join("sessions.db")).unwrap(), b"store");
        assert_eq!(std::fs::read(marker.join("foreign")).unwrap(), b"preserve");
    }

    #[test]
    fn unexpected_sqlite_member_kind_preserves_the_whole_family() {
        let directory = tempfile::TempDir::new().unwrap();
        let root = directory.path();
        std::fs::write(root.join("sessions.db"), b"main").unwrap();
        let foreign = root.join("sessions.db-wal");
        std::fs::create_dir(&foreign).unwrap();
        std::fs::write(foreign.join("entry"), b"keep").unwrap();
        assert!(remove_store_family(root, "sessions.db").is_err());
        assert_eq!(std::fs::read(root.join("sessions.db")).unwrap(), b"main");
        assert_eq!(std::fs::read(foreign.join("entry")).unwrap(), b"keep");
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
                store("profile authority", "tracedecay wipe --stale --yes"),
                store("profile sessions", "tracedecay wipe --stale --yes"),
            ])
            .unwrap(),
            [
                ResettableStoreV1::ProfileAuthority,
                ResettableStoreV1::ProfileSessions
            ]
        );
        assert_eq!(
            resettable_targets(&[
                store("profile sessions", "tracedecay wipe --stale --yes"),
                store("ProfileMemory", "tracedecay wipe --all --yes"),
            ])
            .unwrap_err()
            .to_string(),
            "config error: ProfileMemory requires reset (rows predate the unified identity) \
             and is not reset on its own; run `tracedecay wipe --all --yes`. Nothing was wiped."
        );
    }
}
