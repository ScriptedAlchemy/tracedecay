//! Bounded orphan-store collection, unregistered-directory collection and
//! incident-debris sweeping over profile-sharded stores. Code-generation retention of registered projects,
//! mounted or not, is a unit of the maintenance tick's store window
//! (`store_maintenance::run_registered_code_generation_retention`).

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt};
#[cfg(unix)]
use cap_std::fs::OpenOptionsExt;
use cap_std::fs::{Dir, OpenOptions};
use serde::{Deserialize, Serialize};
use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_private_fs::capability_dir::{replace_file, sync_directory};
use tracedecay_runtime_core::cancellation::{CancellationToken, MonotonicDeadline};

use super::{incident_debris, orphan_stores};
use crate::clock::now_secs_i64;

const COLD_STORE_PAGE_LIMIT: usize = 8;
const CHECKPOINT_DIRECTORY: &str = "maintenance";
const CHECKPOINT_FILE: &str = "retention-cold-store-cursor-v1.json";
const MAX_CHECKPOINT_BYTES: u64 = 64 * 1024;
static CURSOR_TEMP_NONCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
struct ColdStoreCursorV1 {
    after_project_id: Option<String>,
    /// Where the unregistered-directory sweep resumes. A cursor written before
    /// this field existed reads as a fresh sweep.
    #[serde(default)]
    after_unregistered_directory: Option<String>,
}

/// Wall-clock bound on one unregistered-directory page, matching the bounded
/// page `orphan_stores::sweep_unregistered_stores` runs.
const UNREGISTERED_PAGE_BUDGET: std::time::Duration = std::time::Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColdStorePageOutcomeV1 {
    Processed,
    Missing,
    Unreadable,
    Cancelled,
}

impl ColdStorePageOutcomeV1 {
    #[must_use]
    pub const fn was_processed(self) -> bool {
        matches!(self, Self::Processed)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColdStorePageReportV1 {
    pub processed_stores: u64,
    pub unavailable_stores: u64,
    pub reclaimed_bytes: u64,
    pub outcome: ColdStorePageOutcomeV1,
}

impl Default for ColdStorePageReportV1 {
    fn default() -> Self {
        Self {
            processed_stores: 0,
            unavailable_stores: 0,
            reclaimed_bytes: 0,
            outcome: ColdStorePageOutcomeV1::Processed,
        }
    }
}

/// Applies one bounded orphan and debris page to profile stores. A store of
/// any of `owner_roots` is live whether or not its root is still on disk.
#[tracing::instrument(name = "maintenance.cold_store.page", level = "trace", skip_all)]
pub async fn run_cold_store_page(
    profile_root: &Path,
    profile_database: &RegisteredGlobalDb,
    orphan_store_gc_days: Option<u64>,
    owner_roots: &BTreeSet<PathBuf>,
    cancellation: &CancellationToken,
) -> tracedecay_domain::errors::Result<ColdStorePageReportV1> {
    let checkpoint_path = checkpoint_path(profile_root);
    let cursor = load_cursor(&checkpoint_path).unwrap_or(ColdStoreCursorV1 {
        after_project_id: None,
        after_unregistered_directory: None,
    });
    let page = orphan_stores::build_store_census_page(
        profile_database,
        profile_root,
        cursor.after_project_id.as_deref(),
        COLD_STORE_PAGE_LIMIT,
    )
    .await?;
    let mut report = ColdStorePageReportV1::default();
    for entry in &page.entries {
        let outcome = classify_cold_store_state(
            cancellation.is_cancelled(),
            entry.manifest_readable,
            entry.data_root.is_dir(),
        );
        match outcome {
            ColdStorePageOutcomeV1::Processed => {
                report.processed_stores = report.processed_stores.saturating_add(1);
            }
            ColdStorePageOutcomeV1::Cancelled => {
                report.outcome = outcome;
                return Ok(report);
            }
            ColdStorePageOutcomeV1::Missing | ColdStorePageOutcomeV1::Unreadable => {
                if report.outcome == ColdStorePageOutcomeV1::Processed {
                    report.outcome = outcome;
                }
                report.unavailable_stores = report.unavailable_stores.saturating_add(1);
            }
        }
    }
    let mut collected_store_ids = BTreeSet::new();
    let now =
        now_secs_i64().map_err(
            |message| tracedecay_domain::errors::TraceDecayError::Config {
                message: message.to_owned(),
            },
        )?;
    if let Some(days) = orphan_store_gc_days {
        let findings = orphan_stores::classify_stores(&page.entries, now, owner_roots);
        let plan = orphan_stores::plan_collection(findings, retention_window_secs(days));
        let (outcome, _) =
            orphan_stores::execute_registered_collection(profile_database, &plan, profile_root)
                .await?;
        report.reclaimed_bytes = report
            .reclaimed_bytes
            .saturating_add(outcome.reclaimed_bytes);
        report.unavailable_stores = report
            .unavailable_stores
            .saturating_add(outcome.errors.len() as u64);
        if !outcome.errors.is_empty() {
            report.outcome = ColdStorePageOutcomeV1::Unreadable;
        }
        collected_store_ids.extend(outcome.collected.into_iter().map(|store| store.store_id));
    }
    // A store this page collected is gone, so it has no debris left to sweep.
    let surviving: Vec<_> = page
        .entries
        .iter()
        .filter(|entry| !collected_store_ids.contains(&entry.store_id))
        .cloned()
        .collect();
    let sweep = incident_debris::sweep_incident_debris(&surviving, profile_root);
    report.reclaimed_bytes = report.reclaimed_bytes.saturating_add(sweep.reclaimed_bytes);
    report.unavailable_stores = report
        .unavailable_stores
        .saturating_add(sweep.errors.len() as u64);
    if !sweep.errors.is_empty() {
        report.outcome = ColdStorePageOutcomeV1::Unreadable;
    }
    // A project directory with no registry row has no other collector: the
    // registered census above never sees it. One bounded page per tick, under
    // the same window as orphaned stores; the sweep never removes a directory
    // that holds durable memory or a payload it cannot inspect.
    let mut after_unregistered_directory = cursor.after_unregistered_directory.clone();
    if let Some(days) = orphan_store_gc_days {
        let unregistered = orphan_stores::sweep_unregistered_store_page(
            profile_database,
            profile_root,
            orphan_stores::UnregisteredStoreSweepRequestV1 {
                cursor: cursor.after_unregistered_directory.clone(),
                limit: COLD_STORE_PAGE_LIMIT,
                retention_secs: retention_window_secs(days),
                now,
                apply: true,
                cancellation,
                deadline: MonotonicDeadline::at(Instant::now() + UNREGISTERED_PAGE_BUDGET),
            },
        )
        .await?;
        report.reclaimed_bytes = report
            .reclaimed_bytes
            .saturating_add(unregistered.outcome.reclaimed_bytes);
        report.unavailable_stores = report
            .unavailable_stores
            .saturating_add(unregistered.outcome.errors.len() as u64);
        if !unregistered.outcome.errors.is_empty() {
            report.outcome = ColdStorePageOutcomeV1::Unreadable;
        }
        match unregistered.completion {
            orphan_stores::UnregisteredSweepCompletionV1::Complete => {
                after_unregistered_directory = unregistered.next_cursor;
            }
            orphan_stores::UnregisteredSweepCompletionV1::Cancelled => {
                report.outcome = ColdStorePageOutcomeV1::Cancelled;
            }
            orphan_stores::UnregisteredSweepCompletionV1::DeadlineExceeded => {}
        }
    }
    let project_ids = page
        .entries
        .iter()
        .map(|entry| entry.project_id.clone())
        .collect::<Vec<_>>();
    let next_cursor = ColdStoreCursorV1 {
        after_project_id: next_cold_store_cursor(
            cursor.after_project_id.as_deref(),
            &project_ids,
            page.next_cursor.is_some(),
        ),
        after_unregistered_directory,
    };
    persist_cursor(&checkpoint_path, &next_cursor).map_err(|error| {
        tracedecay_domain::errors::TraceDecayError::Config {
            message: format!("persist maintenance cold-store cursor: {error}"),
        }
    })?;
    Ok(report)
}

fn next_cold_store_cursor(
    previous: Option<&str>,
    project_ids: &[String],
    has_more: bool,
) -> Option<String> {
    if !has_more {
        return None;
    }
    project_ids
        .last()
        .cloned()
        .or_else(|| previous.map(str::to_owned))
}

fn classify_cold_store_state(
    cancelled: bool,
    manifest_readable: bool,
    data_root_exists: bool,
) -> ColdStorePageOutcomeV1 {
    if cancelled {
        ColdStorePageOutcomeV1::Cancelled
    } else if !data_root_exists {
        ColdStorePageOutcomeV1::Missing
    } else if !manifest_readable {
        ColdStorePageOutcomeV1::Unreadable
    } else {
        ColdStorePageOutcomeV1::Processed
    }
}

fn checkpoint_path(profile_root: &Path) -> PathBuf {
    profile_root
        .join(CHECKPOINT_DIRECTORY)
        .join(CHECKPOINT_FILE)
}

// Keep the maintenance directory capability open for the whole cursor read or
// publish. A path check followed by a path-based open or rename can be raced
// with a symlink substitution of "maintenance".
fn checkpoint_directory(path: &Path, create: bool) -> io::Result<Dir> {
    let profile = path
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| io::Error::other("maintenance cursor has no profile root"))?;
    let root = Dir::open_ambient_dir(profile, cap_std::ambient_authority())?;
    if create {
        match root.create_dir(CHECKPOINT_DIRECTORY) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    root.open_dir_nofollow(CHECKPOINT_DIRECTORY)
}

#[tracing::instrument(name = "maintenance.cold_store.load_cursor", level = "trace", skip_all)]
fn load_cursor(path: &Path) -> Option<ColdStoreCursorV1> {
    let directory = match checkpoint_directory(path, false) {
        Ok(directory) => directory,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return None,
        Err(error) => {
            tracing::warn!(
                event = "retention_cold_store_cursor_invalid",
                reason = "untrusted_directory",
                %error,
                "maintenance cursor directory is unsafe; restarting its bounded census"
            );
            return None;
        }
    };
    load_cursor_in_dir(&directory)
}

fn load_cursor_in_dir(directory: &Dir) -> Option<ColdStoreCursorV1> {
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    let file = match directory.open_with(CHECKPOINT_FILE, &options) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return None,
        Err(error) => {
            tracing::warn!(
                event = "retention_cold_store_cursor_invalid",
                %error,
                "cannot safely open the saved maintenance cursor"
            );
            return None;
        }
    };
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.len() > MAX_CHECKPOINT_BYTES {
        tracing::warn!(
            event = "retention_cold_store_cursor_invalid",
            reason = "nonregular_or_oversized",
            "saved maintenance cursor is unsafe; restarting its bounded census"
        );
        return None;
    }
    let mut limited = file.take(MAX_CHECKPOINT_BYTES + 1);
    let mut bytes = Vec::new();
    limited.read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 > MAX_CHECKPOINT_BYTES {
        tracing::warn!(
            event = "retention_cold_store_cursor_invalid",
            reason = "oversized_after_open",
            "saved maintenance cursor grew during its read; restarting its bounded census"
        );
        return None;
    }
    match serde_json::from_slice(&bytes) {
        Ok(cursor) => Some(cursor),
        Err(error) => {
            tracing::warn!(
                event = "retention_cold_store_cursor_invalid",
                %error,
                "saved maintenance cursor is invalid; restarting its bounded census"
            );
            None
        }
    }
}

#[tracing::instrument(
    name = "maintenance.cold_store.persist_cursor",
    level = "trace",
    skip_all
)]
fn persist_cursor(path: &Path, cursor: &ColdStoreCursorV1) -> io::Result<()> {
    let directory = checkpoint_directory(path, true)?;
    persist_cursor_in_dir(&directory, cursor)
}

fn persist_cursor_in_dir(directory: &Dir, cursor: &ColdStoreCursorV1) -> io::Result<()> {
    let bytes = serde_json::to_vec(cursor).map_err(io::Error::other)?;
    // The name supplies no authority. create_new + no-follow guarantees that
    // even a collided or attacker-prepared staging name cannot be truncated.
    for _ in 0..16 {
        let nonce = CURSOR_TEMP_NONCE.fetch_add(1, Ordering::Relaxed);
        let temporary_name = format!(".retention-cold-store-{}-{nonce}.tmp", std::process::id());
        let mut options = OpenOptions::new();
        options
            .write(true)
            .create_new(true)
            .follow(FollowSymlinks::No);
        #[cfg(unix)]
        options.mode(0o600);
        let mut temporary = match directory.open_with(&temporary_name, &options) {
            Ok(temporary) => temporary,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        };
        let result = (|| {
            temporary.write_all(&bytes)?;
            temporary.sync_all()?;
            drop(temporary);
            replace_file(
                directory,
                OsStr::new(&temporary_name),
                OsStr::new(CHECKPOINT_FILE),
            )?;
            sync_directory(directory)
        })();
        if result.is_err() {
            let _ = directory.remove_file(&temporary_name);
        }
        return result;
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "maintenance cursor staging names are occupied",
    ))
}

fn retention_window_secs(days: u64) -> i64 {
    i64::try_from(days)
        .ok()
        .and_then(|days| days.checked_mul(24 * 60 * 60))
        .unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_resumes_after_the_last_complete_project() {
        let first = next_cold_store_cursor(
            None,
            &["project-a".to_owned(), "project-b".to_owned()],
            true,
        )
        .expect("first page cursor");
        assert_eq!(first, "project-b");
        assert_eq!(
            next_cold_store_cursor(Some(&first), &["project-c".to_owned()], false),
            None
        );
    }

    #[test]
    fn checkpoint_survives_restart() {
        let root = tempfile::tempdir().expect("checkpoint root");
        let path = checkpoint_path(root.path());
        let expected = ColdStoreCursorV1 {
            after_project_id: Some("project-b".to_owned()),
            after_unregistered_directory: Some("proj_0000000000000000".to_owned()),
        };

        persist_cursor(&path, &expected).expect("persist cursor");

        assert_eq!(load_cursor(&path), Some(expected.clone()));
        let updated = ColdStoreCursorV1 {
            after_project_id: Some("project-c".to_owned()),
            after_unregistered_directory: None,
        };
        persist_cursor(&path, &updated).expect("replace existing checkpoint");
        assert_eq!(load_cursor(&path), Some(updated));
        assert!(!path.with_extension("json.tmp").exists());
    }

    #[cfg(unix)]
    #[test]
    fn checkpoint_publication_does_not_follow_foreign_file_symlinks() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().expect("checkpoint root");
        let path = checkpoint_path(root.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let foreign = root.path().join("foreign-data");
        std::fs::write(&foreign, b"unrelated durable bytes").unwrap();
        // Both a leftover fixed temporary name and the destination may be
        // replaced by a link. Neither may grant write authority to its target.
        let legacy_temporary = path.with_extension("json.tmp");
        symlink(&foreign, &legacy_temporary).unwrap();
        symlink(&foreign, &path).unwrap();
        assert!(
            load_cursor(&path).is_none(),
            "foreign checkpoint links cannot be adopted as cursors"
        );

        let expected = ColdStoreCursorV1 {
            after_project_id: Some("project-c".to_owned()),
            after_unregistered_directory: None,
        };
        persist_cursor(&path, &expected).expect("replace checkpoint safely");
        assert_eq!(std::fs::read(&foreign).unwrap(), b"unrelated durable bytes");
        assert!(
            std::fs::symlink_metadata(&legacy_temporary)
                .unwrap()
                .file_type()
                .is_symlink(),
            "foreign legacy temporary entries must not be overwritten"
        );
        assert!(
            std::fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_file(),
            "a checkpoint symlink is replaced by an ordinary file"
        );
        assert_eq!(load_cursor(&path), Some(expected));
    }

    #[cfg(unix)]
    #[test]
    fn a_linked_maintenance_directory_never_claims_foreign_cursors() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().expect("profile root");
        let profile = root.path().join("profile");
        let foreign = root.path().join("foreign");
        std::fs::create_dir(&profile).unwrap();
        std::fs::create_dir(&foreign).unwrap();
        let path = checkpoint_path(&profile);
        let foreign_cursor = foreign.join(CHECKPOINT_FILE);
        let foreign_bytes = br#"{"after_project_id":"foreign"}"#;
        std::fs::write(&foreign_cursor, foreign_bytes).unwrap();
        symlink(&foreign, profile.join(CHECKPOINT_DIRECTORY)).unwrap();

        assert_eq!(
            load_cursor(&path),
            None,
            "a linked checkpoint parent cannot supply a foreign resume position"
        );
        let expected = ColdStoreCursorV1 {
            after_project_id: Some("project-local".to_owned()),
            after_unregistered_directory: None,
        };
        assert!(
            persist_cursor(&path, &expected).is_err(),
            "publication must not cross a substituted maintenance directory"
        );
        assert_eq!(std::fs::read(&foreign_cursor).unwrap(), foreign_bytes);
        assert_eq!(
            std::fs::read_dir(&foreign).unwrap().count(),
            1,
            "no staging file may be created in the foreign directory"
        );

        std::fs::remove_file(profile.join(CHECKPOINT_DIRECTORY)).unwrap();
        persist_cursor(&path, &expected).unwrap();
        assert_eq!(load_cursor(&path), Some(expected));
        assert_eq!(std::fs::read(&foreign_cursor).unwrap(), foreign_bytes);
    }

    #[cfg(unix)]
    #[test]
    fn a_replaced_parent_cannot_redirect_an_open_checkpoint_capability() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let profile = root.path().join("profile");
        let foreign = root.path().join("foreign");
        std::fs::create_dir(&profile).unwrap();
        std::fs::create_dir(&foreign).unwrap();
        let path = checkpoint_path(&profile);
        let directory = checkpoint_directory(&path, true).unwrap();
        let expected = ColdStoreCursorV1 {
            after_project_id: Some("local".to_owned()),
            after_unregistered_directory: None,
        };
        persist_cursor_in_dir(&directory, &expected).unwrap();
        std::fs::rename(profile.join(CHECKPOINT_DIRECTORY), profile.join("detached")).unwrap();
        let foreign_cursor = foreign.join(CHECKPOINT_FILE);
        std::fs::write(&foreign_cursor, br#"{"after_project_id":"foreign"}"#).unwrap();
        symlink(&foreign, profile.join(CHECKPOINT_DIRECTORY)).unwrap();

        assert_eq!(load_cursor_in_dir(&directory), Some(expected.clone()));
        let updated = ColdStoreCursorV1 {
            after_project_id: Some("local-next".to_owned()),
            after_unregistered_directory: None,
        };
        persist_cursor_in_dir(&directory, &updated).unwrap();
        assert_eq!(load_cursor_in_dir(&directory), Some(updated));
        assert_eq!(
            std::fs::read(&foreign_cursor).unwrap(),
            br#"{"after_project_id":"foreign"}"#,
            "a path substitution must not grant the foreign directory write authority"
        );
        assert_eq!(std::fs::read_dir(&foreign).unwrap().count(), 1);
        assert!(persist_cursor(&path, &expected).is_err());
        assert_eq!(load_cursor(&path), None);
    }

    #[test]
    fn malformed_and_oversized_checkpoints_do_not_authorize_unbounded_reads() {
        let root = tempfile::tempdir().expect("checkpoint root");
        let path = checkpoint_path(root.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        std::fs::write(&path, vec![b' '; (MAX_CHECKPOINT_BYTES + 1) as usize])
            .expect("oversized checkpoint");
        assert_eq!(load_cursor(&path), None);
        std::fs::write(&path, b"{invalid").expect("malformed checkpoint");
        assert_eq!(load_cursor(&path), None);

        let valid = ColdStoreCursorV1 {
            after_project_id: None,
            after_unregistered_directory: None,
        };
        persist_cursor(&path, &valid).expect("restore valid checkpoint");
        assert_eq!(load_cursor(&path), Some(valid));
    }

    #[test]
    fn state_distinguishes_missing_unreadable_and_cancelled() {
        assert_eq!(
            classify_cold_store_state(false, true, true),
            ColdStorePageOutcomeV1::Processed
        );
        assert_eq!(
            classify_cold_store_state(false, true, false),
            ColdStorePageOutcomeV1::Missing
        );
        assert_eq!(
            classify_cold_store_state(false, false, true),
            ColdStorePageOutcomeV1::Unreadable
        );
        assert_eq!(
            classify_cold_store_state(true, true, true),
            ColdStorePageOutcomeV1::Cancelled
        );
    }

    #[test]
    fn a_cursor_written_before_the_unregistered_sweep_still_loads() {
        let root = tempfile::tempdir().expect("checkpoint root");
        let path = checkpoint_path(root.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, br#"{"after_project_id":"project-b"}"#).unwrap();

        assert_eq!(
            load_cursor(&path),
            Some(ColdStoreCursorV1 {
                after_project_id: Some("project-b".to_owned()),
                after_unregistered_directory: None,
            })
        );
    }

    #[test]
    fn retention_window_conversion_never_wraps_negative() {
        assert_eq!(retention_window_secs(u64::MAX), i64::MAX);
    }
}
