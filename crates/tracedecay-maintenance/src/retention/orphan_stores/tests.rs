use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use super::quarantine::{QuarantineRecoveryOutcome, recover_existing_store_quarantine};
use super::*;
use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime;
use tracedecay_runtime_core::cancellation::{CancellationToken, MonotonicDeadline};
use tracedecay_runtime_core::storage::{
    STORE_MANIFEST_SCHEMA_VERSION, StorageMode, StoreKind, StoreManifest,
};

const DAY: i64 = 24 * 60 * 60;
#[cfg(unix)]
const OCCUPIED_RENAME_RAW_OS_ERROR: i32 = 17;
#[cfg(windows)]
const OCCUPIED_RENAME_RAW_OS_ERROR: i32 = 183;

async fn open_registered_db(
    profile_root: &Path,
) -> (
    RegisteredGlobalDbTestRuntime,
    tracedecay_global_db::RegisteredGlobalDbLeaseV1,
) {
    // `create_dir_all` in the callers honours the ambient umask, so under a
    // group-writable umask (0002) the profile root lands at 0775 and profile
    // identity refuses it. A real profile root is always 0700; make the
    // fixture match rather than depending on the developer's umask.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(profile_root, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let runtime = RegisteredGlobalDbTestRuntime::profile(profile_root)
        .await
        .unwrap();
    let database = runtime.profile_database_arc();
    (runtime, database)
}

fn entry(
    store_id: &str,
    canonical_root: PathBuf,
    display_root: Option<PathBuf>,
    manifest_root: Option<PathBuf>,
    data_root: PathBuf,
    last_write_secs: i64,
    size_bytes: u64,
) -> StoreCensusEntry {
    let expected_store_relpath = data_root.to_string_lossy().into_owned();
    StoreCensusEntry {
        project_id: format!("proj_{store_id}"),
        store_id: store_id.to_string(),
        canonical_root,
        display_root,
        git_common_dir: None,
        alias_roots: Vec::new(),
        manifest_readable: true,
        data_root,
        manifest_root,
        last_write_secs,
        size_bytes,
        expected_store_relpath,
        expected_created_at: 0,
        expected_last_write_at: Some(last_write_secs),
        expected_payload_mtime_secs: last_write_secs,
        expected_data_root_fence: StoreDirectoryFence::Missing,
        expected_content_fence: StoreContentFence::Missing,
        expected_manifest_bytes: None,
        graph_scope_relpaths: Vec::new(),
    }
}

#[test]
fn live_root_is_never_collected() {
    let live = std::env::current_dir().unwrap();
    let census = vec![entry(
        "live",
        live.clone(),
        None,
        None,
        PathBuf::from("/profile/stores/live"),
        0,
        4096,
    )];
    let findings = classify_stores(&census, 1_000 * DAY);
    assert_eq!(findings[0].disposition, StoreDisposition::Live);

    let plan = plan_collection(findings, 0);
    assert!(
        plan.collect.is_empty(),
        "a live store must never be collected"
    );
    assert!(plan.relink.is_empty());
    assert!(plan.retained_immature.is_empty());
}

#[test]
fn live_registered_alias_keeps_the_store_out_of_every_collectable_bucket() {
    let dead = PathBuf::from("/definitely/not/here/retired-checkout");
    let live_alias = std::env::current_dir().unwrap();
    let mut census_entry = entry(
        "aliased",
        dead,
        None,
        None,
        PathBuf::from("/profile/stores/aliased"),
        0,
        4096,
    );
    census_entry.alias_roots = vec![live_alias];

    let findings = classify_stores(&[census_entry], 1_000 * DAY);
    assert_eq!(
        findings[0].disposition,
        StoreDisposition::Live,
        "a registered alias that still exists keeps the store's identity live"
    );

    let plan = plan_collection(findings, 0);
    assert!(plan.collect.is_empty());
    assert!(plan.retained_immature.is_empty());
    assert!(plan.unverifiable.is_empty());
}

#[test]
fn live_git_common_dir_keeps_a_linked_worktree_store_live() {
    // A linked worktree's own root can vanish while the repository — and every
    // other checkout sharing its common directory — stays live.
    let gone_worktree = PathBuf::from("/definitely/not/here/linked-worktree");
    let shared_common_dir = std::env::current_dir().unwrap();
    let mut census_entry = entry(
        "worktree",
        gone_worktree,
        None,
        None,
        PathBuf::from("/profile/stores/worktree"),
        0,
        4096,
    );
    census_entry.git_common_dir = Some(shared_common_dir);

    let findings = classify_stores(&[census_entry], 1_000 * DAY);
    assert_eq!(findings[0].disposition, StoreDisposition::Live);
    assert!(plan_collection(findings, 0).collect.is_empty());
}

#[test]
fn unreadable_manifest_is_unverifiable_never_orphaned() {
    let dead = PathBuf::from("/definitely/not/here/gone");
    let mut census_entry = entry(
        "malformed",
        dead,
        None,
        None,
        PathBuf::from("/profile/stores/malformed"),
        0,
        4096,
    );
    census_entry.manifest_readable = false;

    let findings = classify_stores(&[census_entry], 1_000 * DAY);
    assert_eq!(
        findings[0].disposition,
        StoreDisposition::Unverifiable {
            reason: UnverifiableReason::ManifestUnreadable
        },
        "a manifest that will not parse must fail closed, not read as an orphan"
    );

    // Even with a zero retention window the store is never collectable.
    let plan = plan_collection(findings, 0);
    assert!(plan.collect.is_empty());
    assert!(plan.relink.is_empty());
    assert_eq!(plan.unverifiable.len(), 1);
}

#[test]
fn malformed_manifest_bytes_mark_the_census_entry_unverifiable() {
    let profile = tempfile::tempdir().unwrap();
    let data_root = profile.path().join("stores/malformed");
    std::fs::create_dir_all(&data_root).unwrap();
    std::fs::write(
        data_root.join(tracedecay_runtime_core::storage::STORE_MANIFEST_FILENAME),
        b"{ this is not valid manifest json",
    )
    .unwrap();

    // Exercise the same parse the census performs, so the fixture proves the
    // production decode path — not a hand-set flag — yields unverifiable.
    let bytes =
        std::fs::read(data_root.join(tracedecay_runtime_core::storage::STORE_MANIFEST_FILENAME))
            .unwrap();
    let parsed = serde_json::from_slice::<StoreManifest>(&bytes).ok();
    assert!(parsed.is_none(), "fixture manifest must be unparseable");

    let mut census_entry = entry(
        "malformed",
        PathBuf::from("/definitely/not/here/gone"),
        None,
        None,
        data_root,
        0,
        4096,
    );
    census_entry.manifest_readable = parsed.is_some();

    let findings = classify_stores(&[census_entry], 1_000 * DAY);
    assert!(
        matches!(
            findings[0].disposition,
            StoreDisposition::Unverifiable { .. }
        ),
        "unparseable manifest bytes must classify as unverifiable"
    );
}

#[test]
fn moved_repository_relinks_instead_of_collecting() {
    let dead = PathBuf::from("/definitely/not/here/old-name");
    let live = std::env::current_dir().unwrap();
    let census = vec![entry(
        "moved",
        dead,
        None,
        Some(live.clone()),
        PathBuf::from("/profile/stores/moved"),
        0,
        8192,
    )];
    let findings = classify_stores(&census, 1_000 * DAY);
    assert_eq!(
        findings[0].disposition,
        StoreDisposition::Relinkable { live_root: live }
    );

    let plan = plan_collection(findings, 0);
    assert!(
        plan.collect.is_empty(),
        "a re-linkable (moved) store must never be collected"
    );
    assert_eq!(plan.relink.len(), 1);
}

#[test]
fn orphan_respects_retention_window() {
    let dead = PathBuf::from("/definitely/not/here/gone");
    let now = 100 * DAY;
    // Written 10 days ago; manifest root also dead → orphaned.
    let census = vec![entry(
        "orphan",
        dead.clone(),
        None,
        Some(PathBuf::from("/definitely/not/here/also-gone")),
        PathBuf::from("/profile/stores/orphan"),
        now - 10 * DAY,
        1_000_000,
    )];
    let findings = classify_stores(&census, now);
    assert_eq!(findings[0].disposition, StoreDisposition::Orphaned);
    assert_eq!(findings[0].age_secs, 10 * DAY);
    assert_eq!(findings[0].size_bytes, 1_000_000);

    // 30-day window → still immature, not collected.
    let plan = plan_collection(findings.clone(), 30 * DAY);
    assert!(plan.collect.is_empty());
    assert_eq!(plan.retained_immature.len(), 1);

    // 7-day window → past retention, eligible for collection.
    let plan = plan_collection(findings, 7 * DAY);
    assert_eq!(plan.collect.len(), 1);
    assert_eq!(plan.collectable_bytes(), 1_000_000);
    assert!(plan.retained_immature.is_empty());
}

/// Delete the on-disk data directories for every store in `plan.collect`.
/// Re-linkable and immature stores are left untouched. A directory that is
/// already gone counts as collected (idempotent). Best-effort: a failed
/// removal is recorded in `errors` and does not abort the rest.
///
/// Production collects through [`execute_registered_collection`], which
/// re-proves registry, manifest, and payload identity before removing
/// anything. This is the unverified core, kept here so the two tests below can
/// pin the profile containment fence and the idempotent-delete contract on
/// their own.
fn execute_collection(plan: &CollectionPlan, profile_root: &Path) -> CollectionOutcome {
    let mut outcome = CollectionOutcome::default();
    let canonical_profile = match profile_root.canonicalize() {
        Ok(path) => path,
        Err(_) => {
            outcome
                .errors
                .extend(plan.collect.iter().map(|finding| CollectionFailure {
                    store_id: finding.store_id.clone(),
                    kind: CollectionFailureKind::InspectFailed,
                }));
            return outcome;
        }
    };
    for finding in &plan.collect {
        let canonical_target = match finding.data_root.canonicalize() {
            Ok(path) => path,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let is_profile_child = finding
                    .data_root
                    .parent()
                    .and_then(|parent| parent.canonicalize().ok())
                    .is_some_and(|parent| {
                        parent.starts_with(&canonical_profile) && parent != canonical_profile
                    });
                if !is_profile_child {
                    outcome.errors.push(CollectionFailure {
                        store_id: finding.store_id.clone(),
                        kind: CollectionFailureKind::OutsideProfile,
                    });
                    continue;
                }
                outcome.reclaimed_bytes =
                    outcome.reclaimed_bytes.saturating_add(finding.size_bytes);
                outcome.collected.push(CollectedStore {
                    project_id: finding.project_id.clone(),
                    store_id: finding.store_id.clone(),
                    data_root: finding.data_root.clone(),
                    size_bytes: finding.size_bytes,
                });
                continue;
            }
            Err(_) => {
                outcome.errors.push(CollectionFailure {
                    store_id: finding.store_id.clone(),
                    kind: CollectionFailureKind::InspectFailed,
                });
                continue;
            }
        };
        if canonical_target == canonical_profile
            || !canonical_target.starts_with(&canonical_profile)
        {
            outcome.errors.push(CollectionFailure {
                store_id: finding.store_id.clone(),
                kind: CollectionFailureKind::OutsideProfile,
            });
            continue;
        }
        match std::fs::remove_dir_all(&finding.data_root) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                outcome.errors.push(CollectionFailure {
                    store_id: finding.store_id.clone(),
                    kind: CollectionFailureKind::RemoveFailed(
                        CollectionMutationFailure::from_io_error(
                            CollectionMutationOperation::RecursiveRemove,
                            finding.data_root.clone(),
                            match &finding.expected_content_fence {
                                StoreContentFence::Present(inventory) => {
                                    Some(inventory.root.clone())
                                }
                                StoreContentFence::Missing | StoreContentFence::Unverifiable => {
                                    None
                                }
                            },
                            &error,
                        ),
                    ),
                });
                continue;
            }
        }
        outcome.reclaimed_bytes = outcome.reclaimed_bytes.saturating_add(finding.size_bytes);
        outcome.collected.push(CollectedStore {
            project_id: finding.project_id.clone(),
            store_id: finding.store_id.clone(),
            data_root: finding.data_root.clone(),
            size_bytes: finding.size_bytes,
        });
    }
    outcome
}

#[test]
fn execute_collection_deletes_only_collect_set() {
    let tmp = tempfile::TempDir::new().unwrap();
    let collect_dir = tmp.path().join("collect-me");
    let keep_dir = tmp.path().join("keep-me");
    std::fs::create_dir_all(&collect_dir).unwrap();
    std::fs::create_dir_all(&keep_dir).unwrap();
    std::fs::write(collect_dir.join("graph.db"), b"payload").unwrap();

    let plan = CollectionPlan {
        collect: vec![OrphanStoreFinding {
            project_id: "proj_collect".into(),
            store_id: "collect".into(),
            data_root: collect_dir.clone(),
            disposition: StoreDisposition::Orphaned,
            age_secs: 90 * DAY,
            size_bytes: 7,
            expected_store_relpath: "collect-me".into(),
            expected_created_at: 0,
            expected_last_write_at: None,
            expected_payload_mtime_secs: 0,
            expected_data_root_fence: StoreDirectoryFence::Missing,
            expected_content_fence: StoreContentFence::Missing,
            expected_manifest_bytes: None,
            graph_scope_relpaths: Vec::new(),
        }],
        retained_immature: vec![OrphanStoreFinding {
            project_id: "proj_keep".into(),
            store_id: "keep".into(),
            data_root: keep_dir.clone(),
            disposition: StoreDisposition::Orphaned,
            age_secs: DAY,
            size_bytes: 0,
            expected_store_relpath: "keep-me".into(),
            expected_created_at: 0,
            expected_last_write_at: None,
            expected_payload_mtime_secs: 0,
            expected_data_root_fence: StoreDirectoryFence::Missing,
            expected_content_fence: StoreContentFence::Missing,
            expected_manifest_bytes: None,
            graph_scope_relpaths: Vec::new(),
        }],
        relink: Vec::new(),
        unverifiable: Vec::new(),
    };

    let outcome = execute_collection(&plan, tmp.path());
    assert_eq!(outcome.collected.len(), 1);
    assert_eq!(outcome.reclaimed_bytes, 7);
    assert!(outcome.errors.is_empty());
    assert!(!collect_dir.exists(), "collected store must be removed");
    assert!(keep_dir.exists(), "immature store must be untouched");
}

#[test]
fn already_missing_directory_collects_idempotently() {
    let tmp = tempfile::TempDir::new().unwrap();
    let stores = tmp.path().join("stores");
    std::fs::create_dir_all(&stores).unwrap();
    let plan = CollectionPlan {
        collect: vec![OrphanStoreFinding {
            project_id: "proj_gone".into(),
            store_id: "gone".into(),
            data_root: stores.join("gone"),
            disposition: StoreDisposition::Orphaned,
            age_secs: 90 * DAY,
            size_bytes: 42,
            expected_store_relpath: "stores/gone".into(),
            expected_created_at: 0,
            expected_last_write_at: None,
            expected_payload_mtime_secs: 0,
            expected_data_root_fence: StoreDirectoryFence::Missing,
            expected_content_fence: StoreContentFence::Missing,
            expected_manifest_bytes: None,
            graph_scope_relpaths: Vec::new(),
        }],
        ..CollectionPlan::default()
    };
    let outcome = execute_collection(&plan, tmp.path());
    assert_eq!(outcome.collected.len(), 1);
    assert!(outcome.errors.is_empty());
}

#[test]
fn execute_collection_rejects_store_outside_profile() {
    let profile = tempfile::TempDir::new().unwrap();
    let outside = tempfile::TempDir::new().unwrap();
    let target = outside.path().join("store");
    std::fs::create_dir_all(&target).unwrap();
    let plan = CollectionPlan {
        collect: vec![OrphanStoreFinding {
            project_id: "proj_escape".into(),
            store_id: "escape".into(),
            data_root: target.clone(),
            disposition: StoreDisposition::Orphaned,
            age_secs: 90 * DAY,
            size_bytes: 1,
            expected_store_relpath: "store".into(),
            expected_created_at: 0,
            expected_last_write_at: None,
            expected_payload_mtime_secs: 0,
            expected_data_root_fence: StoreDirectoryFence::Missing,
            expected_content_fence: StoreContentFence::Missing,
            expected_manifest_bytes: None,
            graph_scope_relpaths: Vec::new(),
        }],
        ..CollectionPlan::default()
    };

    let outcome = execute_collection(&plan, profile.path());

    assert!(target.exists(), "outside-profile store must not be removed");
    assert!(outcome.collected.is_empty());
    assert_eq!(
        outcome.errors,
        vec![CollectionFailure {
            store_id: "escape".into(),
            kind: CollectionFailureKind::OutsideProfile,
        }]
    );
}

/// Seed a profile with one live store and one identity-drift orphan store, then
/// prove the async sweep collects only the orphan and retires its registry row.
#[tokio::test]
async fn sweep_collects_orphan_store_and_retires_row() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    std::fs::create_dir_all(&profile_root).unwrap();

    // Live repository root that still exists on disk.
    let live_root = tmp.path().join("live-repo");
    std::fs::create_dir_all(&live_root).unwrap();
    // Orphan identity: canonical + display roots that no longer exist.
    let dead_root = tmp.path().join("moved-away-repo");

    let (_runtime, db) = open_registered_db(&profile_root).await;

    // Anchor timestamps at a real epoch base so the recorded last-write drives
    // the age (not the freshly-written file mtime, which would be "now").
    let base = 1_700_000_000i64;
    seed_store(
        &db,
        &profile_root,
        "proj_live",
        "store_live",
        &live_root,
        base,
    )
    .await;
    let orphan_data_root = seed_store(
        &db,
        &profile_root,
        "proj_orphan",
        "store_orphan",
        &dead_root,
        base - 100 * DAY,
    )
    .await;
    assert!(orphan_data_root.exists());

    let now = base;
    // Dry run: plan classifies orphan, mutates nothing.
    let report = sweep_orphan_stores(&db, &profile_root, 7 * DAY, now, false)
        .await
        .unwrap();
    assert_eq!(report.plan.collect.len(), 1, "one orphan should be planned");
    assert_eq!(report.plan.collect[0].store_id, "store_orphan");
    assert!(!report.applied);
    assert!(orphan_data_root.exists(), "dry run must not delete");

    // Apply: orphan store removed, row retired, live store untouched.
    let report = sweep_orphan_stores(&db, &profile_root, 7 * DAY, now, true)
        .await
        .unwrap();
    assert!(report.applied);
    assert_eq!(
        report.outcome.collected.len(),
        1,
        "ordinary empty stores must remain collectable: {report:#?}"
    );
    assert_eq!(report.retired_registry_rows, 1);
    assert!(!orphan_data_root.exists(), "orphan store must be collected");
    assert!(live_root.exists());

    let live_data_root = profile_root.join("stores/store_live");
    assert!(live_data_root.exists(), "live store must be untouched");

    let remaining: Vec<_> = db
        .list_code_projects(usize::MAX)
        .await
        .unwrap()
        .into_iter()
        .map(|p| p.project_id)
        .collect();
    assert!(remaining.contains(&"proj_live".to_string()));
    assert!(
        !remaining.contains(&"proj_orphan".to_string()),
        "orphan identity row must be retired"
    );
}

/// The collection plan is only an inspection receipt.  Replacing its directory
/// with byte-identical contents in the same timestamp second must still abort
/// the apply rather than retire a newly-created store identity.
#[cfg(unix)]
#[tokio::test]
async fn registered_collection_refuses_same_second_directory_replacement() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let dead_root = tmp.path().join("moved-away-repo");
    let (_runtime, db) = open_registered_db(&profile_root).await;
    let data_root = seed_store(
        &db,
        &profile_root,
        "proj_replaced",
        "store_replaced",
        &dead_root,
        1_700_000_000 - 100 * DAY,
    )
    .await;

    let census = build_store_census(&db, &profile_root).await.unwrap();
    let plan = plan_collection(classify_stores(&census, 1_700_000_000), 7 * DAY);
    assert_eq!(
        plan.collect.len(),
        1,
        "fixture must be eligible before replacement"
    );

    let displaced = profile_root.join("displaced-store");
    std::fs::rename(&data_root, &displaced).unwrap();
    std::fs::create_dir_all(&data_root).unwrap();
    for name in [
        "graph.db",
        tracedecay_runtime_core::storage::STORE_MANIFEST_FILENAME,
    ] {
        let source = displaced.join(name);
        let target = data_root.join(name);
        std::fs::copy(&source, &target).unwrap();
        let modified =
            filetime::FileTime::from_system_time(source.metadata().unwrap().modified().unwrap());
        filetime::set_file_mtime(&target, modified).unwrap();
    }
    let original_directory_time =
        filetime::FileTime::from_system_time(displaced.metadata().unwrap().modified().unwrap());
    filetime::set_file_mtime(&data_root, original_directory_time).unwrap();

    let (outcome, retired) = execute_registered_collection(&db, &plan, &profile_root)
        .await
        .unwrap();

    assert_eq!(retired, 0);
    assert!(outcome.collected.is_empty());
    assert_eq!(
        outcome.errors,
        vec![CollectionFailure {
            store_id: "store_replaced".to_owned(),
            kind: CollectionFailureKind::PayloadChanged,
        }]
    );
    assert!(data_root.exists(), "replacement directory must survive");
    assert_eq!(
        db.try_list_store_instances_for_project("proj_replaced")
            .await
            .unwrap()
            .len(),
        1,
        "a rejected collection must leave the registry authority intact"
    );
}

/// A profile-contained symlink is still not a store directory authority.  The
/// collector must refuse it instead of deleting the link and retiring the
/// registry row while its target payload survives without an owner.
#[cfg(unix)]
#[tokio::test]
async fn registered_collection_rejects_profile_contained_data_root_symlink() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let dead_root = tmp.path().join("moved-away-repo");
    let (_runtime, db) = open_registered_db(&profile_root).await;
    let data_root = seed_store(
        &db,
        &profile_root,
        "proj_symlinked_root",
        "store_symlinked_root",
        &dead_root,
        1_700_000_000 - 100 * DAY,
    )
    .await;

    let census = build_store_census(&db, &profile_root).await.unwrap();
    let plan = plan_collection(classify_stores(&census, 1_700_000_000), 7 * DAY);
    assert_eq!(
        plan.collect.len(),
        1,
        "fixture must be eligible before replacement"
    );

    let held_payload = profile_root.join("held-payload");
    std::fs::rename(&data_root, &held_payload).unwrap();
    std::os::unix::fs::symlink(&held_payload, &data_root).unwrap();

    let (outcome, retired) = execute_registered_collection(&db, &plan, &profile_root)
        .await
        .unwrap();

    assert_eq!(retired, 0);
    assert!(outcome.collected.is_empty());
    assert_eq!(
        outcome.errors,
        vec![CollectionFailure {
            store_id: "store_symlinked_root".to_owned(),
            kind: CollectionFailureKind::OutsideProfile,
        }]
    );
    assert!(held_payload.exists(), "the payload target must survive");
    assert!(
        data_root
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        db.try_list_store_instances_for_project("proj_symlinked_root")
            .await
            .unwrap()
            .len(),
        1,
        "a rejected collection must leave the registry authority intact"
    );
}

#[tokio::test]
async fn sweep_preserves_immature_sibling_store_identity() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let dead_root = tmp.path().join("moved-away-repo");
    let (_runtime, db) = open_registered_db(&profile_root).await;
    let now = 1_700_000_000i64;
    let old_root = seed_store(
        &db,
        &profile_root,
        "proj_orphan",
        "store_old",
        &dead_root,
        now - 100 * DAY,
    )
    .await;
    let young_root = seed_store(
        &db,
        &profile_root,
        "proj_orphan",
        "store_young",
        &dead_root,
        now - DAY,
    )
    .await;

    let report = sweep_orphan_stores(&db, &profile_root, 7 * DAY, now, true)
        .await
        .unwrap();

    assert_eq!(report.retired_registry_rows, 1);
    assert!(!old_root.exists());
    assert!(young_root.exists());
    let stores = db
        .try_list_store_instances_for_project("proj_orphan")
        .await
        .unwrap();
    assert_eq!(
        stores
            .into_iter()
            .map(|store| store.store_id)
            .collect::<Vec<_>>(),
        vec!["store_young"]
    );
    assert!(
        db.list_code_projects(usize::MAX)
            .await
            .unwrap()
            .into_iter()
            .any(|project| project.project_id == "proj_orphan")
    );
}

#[tokio::test]
async fn sweep_atomically_relinks_moved_store_to_registered_live_project() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let dead_root = tmp.path().join("old-repository-root");
    let live_root = tmp.path().join("renamed-repository-root");
    std::fs::create_dir_all(&live_root).unwrap();
    let (_runtime, db) = open_registered_db(&profile_root).await;
    let store_root = seed_store(
        &db,
        &profile_root,
        "proj_old",
        "store_moved",
        &dead_root,
        1_700_000_000,
    )
    .await;
    seed_project(&db, "proj_live", &live_root, 1_700_000_000).await;

    let manifest = StoreManifest {
        schema_version: STORE_MANIFEST_SCHEMA_VERSION,
        project_id: Some("proj_old".to_string()),
        store_kind: StoreKind::CodeProject,
        storage_mode: StorageMode::ProfileSharded,
        project_root: live_root,
        data_root: store_root.clone(),
        graph_db_relpath: PathBuf::from("graph.db"),
        sessions_db_relpath: PathBuf::from("sessions.db"),
        branch_meta_relpath: PathBuf::from(tracedecay_runtime_core::storage::BRANCH_META_FILENAME),
    };
    std::fs::write(
        store_root.join(tracedecay_runtime_core::storage::STORE_MANIFEST_FILENAME),
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();

    let report = sweep_orphan_stores(&db, &profile_root, 7 * DAY, 1_700_000_000, true)
        .await
        .unwrap();

    assert_eq!(report.relinked_registry_rows, 1);
    assert!(report.outcome.collected.is_empty());
    assert!(store_root.exists(), "re-link must not delete store payload");
    assert!(
        db.try_list_store_instances_for_project("proj_old")
            .await
            .unwrap()
            .is_empty()
    );
    let target_stores = db
        .try_list_store_instances_for_project("proj_live")
        .await
        .unwrap();
    assert_eq!(target_stores.len(), 1);
    assert_eq!(target_stores[0].store_id, "store_moved");
    assert_eq!(target_stores[0].project_id, "proj_live");
    let relinked_manifest = tracedecay_runtime_core::storage::read_store_manifest(
        &store_root.join(tracedecay_runtime_core::storage::STORE_MANIFEST_FILENAME),
    )
    .unwrap();
    assert_eq!(relinked_manifest.project_id.as_deref(), Some("proj_live"));
    assert!(
        !db.list_code_projects(usize::MAX)
            .await
            .unwrap()
            .into_iter()
            .any(|project| project.project_id == "proj_old")
    );
}

#[tokio::test]
async fn sweep_resumes_manifest_forward_after_interrupted_relink() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let dead_root = tmp.path().join("old-repository-root");
    let live_root = tmp.path().join("renamed-repository-root");
    std::fs::create_dir_all(&live_root).unwrap();
    let (_runtime, db) = open_registered_db(&profile_root).await;
    let store_root = seed_store(
        &db,
        &profile_root,
        "proj_old",
        "store_moved",
        &dead_root,
        1_700_000_000,
    )
    .await;
    seed_project(&db, "proj_live", &live_root, 1_700_000_000).await;
    let mut manifest = tracedecay_runtime_core::storage::read_store_manifest(
        &store_root.join(tracedecay_runtime_core::storage::STORE_MANIFEST_FILENAME),
    )
    .unwrap();
    manifest.project_id = Some("proj_live".to_string());
    manifest.project_root = live_root;
    tracedecay_runtime_core::storage::write_store_manifest_to_path(
        &store_root.join(tracedecay_runtime_core::storage::STORE_MANIFEST_FILENAME),
        &manifest,
    )
    .unwrap();

    let report = sweep_orphan_stores(&db, &profile_root, 7 * DAY, 1_700_000_000, true)
        .await
        .unwrap();

    assert_eq!(report.relinked_registry_rows, 1);
    assert!(
        db.try_list_store_instances_for_project("proj_old")
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        db.try_list_store_instances_for_project("proj_live")
            .await
            .unwrap()
            .into_iter()
            .map(|store| store.store_id)
            .collect::<Vec<_>>(),
        vec!["store_moved"]
    );
}

#[tokio::test]
async fn sweep_leaves_relinkable_store_unchanged_without_exact_target_registration() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let dead_root = tmp.path().join("old-repository-root");
    let unregistered_live_root = tmp.path().join("unregistered-live-root");
    std::fs::create_dir_all(&unregistered_live_root).unwrap();
    let (_runtime, db) = open_registered_db(&profile_root).await;
    let store_root = seed_store(
        &db,
        &profile_root,
        "proj_old",
        "store_moved",
        &dead_root,
        1_700_000_000,
    )
    .await;
    let mut manifest = tracedecay_runtime_core::storage::read_store_manifest(
        &store_root.join(tracedecay_runtime_core::storage::STORE_MANIFEST_FILENAME),
    )
    .unwrap();
    manifest.project_root = unregistered_live_root;
    std::fs::write(
        store_root.join(tracedecay_runtime_core::storage::STORE_MANIFEST_FILENAME),
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();

    let report = sweep_orphan_stores(&db, &profile_root, 7 * DAY, 1_700_000_000, true)
        .await
        .unwrap();

    assert_eq!(report.relinked_registry_rows, 0);
    assert!(report.outcome.collected.is_empty());
    assert!(store_root.exists());
    let prior = db
        .try_list_store_instances_for_project("proj_old")
        .await
        .unwrap();
    assert_eq!(prior.len(), 1);
    assert_eq!(prior[0].store_id, "store_moved");
    let unchanged_manifest = tracedecay_runtime_core::storage::read_store_manifest(
        &store_root.join(tracedecay_runtime_core::storage::STORE_MANIFEST_FILENAME),
    )
    .unwrap();
    assert_eq!(unchanged_manifest.project_id.as_deref(), Some("proj_old"));
}

#[tokio::test]
async fn relink_database_failure_rolls_back_manifest_and_registry() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let dead_root = tmp.path().join("old-repository-root");
    let live_root = tmp.path().join("registered-live-root");
    std::fs::create_dir_all(&live_root).unwrap();
    let (_runtime, db) = open_registered_db(&profile_root).await;
    let store_root = seed_store(
        &db,
        &profile_root,
        "proj_old",
        "store_moved",
        &dead_root,
        1_700_000_000,
    )
    .await;
    seed_project(&db, "proj_live", &live_root, 1_700_000_000).await;
    let mut manifest = tracedecay_runtime_core::storage::read_store_manifest(
        &store_root.join(tracedecay_runtime_core::storage::STORE_MANIFEST_FILENAME),
    )
    .unwrap();
    manifest.project_root = live_root;
    std::fs::write(
        store_root.join(tracedecay_runtime_core::storage::STORE_MANIFEST_FILENAME),
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();
    db.writer_connection()
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER reject_test_relink
             BEFORE INSERT ON store_instances
             WHEN NEW.project_id = 'proj_live'
             BEGIN SELECT RAISE(ABORT, 'test relink rejection'); END;",
        )
        .await
        .unwrap();

    assert!(
        sweep_orphan_stores(&db, &profile_root, 7 * DAY, 1_700_000_000, true)
            .await
            .is_err()
    );

    let prior = db
        .try_list_store_instances_for_project("proj_old")
        .await
        .unwrap();
    assert_eq!(prior.len(), 1);
    assert_eq!(prior[0].store_id, "store_moved");
    assert!(
        db.try_list_store_instances_for_project("proj_live")
            .await
            .unwrap()
            .is_empty()
    );
    let restored_manifest = tracedecay_runtime_core::storage::read_store_manifest(
        &store_root.join(tracedecay_runtime_core::storage::STORE_MANIFEST_FILENAME),
    )
    .unwrap();
    assert_eq!(restored_manifest.project_id.as_deref(), Some("proj_old"));
}

/// Register a profile-sharded store and write its manifest + a payload file.
/// Returns the on-disk data root. The manifest `project_root` matches the
/// registry root so a dead root is a true orphan (not a re-link candidate).
async fn seed_store(
    db: &RegisteredGlobalDb,
    profile_root: &Path,
    project_id: &str,
    store_id: &str,
    project_root: &Path,
    created_at: i64,
) -> PathBuf {
    let data_root = profile_root.join("stores").join(store_id);
    std::fs::create_dir_all(&data_root).unwrap();
    // A real (schema-empty) SQLite file, not raw bytes: the durable-memory
    // guard opens this file through `sqlite_read_snapshot` before any
    // collection, so the fixture must be a database the guard can actually
    // inspect and prove carries no `memory_facts`/etc. rows.
    rusqlite::Connection::open(data_root.join("graph.db")).unwrap();

    let manifest = StoreManifest {
        schema_version: STORE_MANIFEST_SCHEMA_VERSION,
        project_id: Some(project_id.to_string()),
        store_kind: StoreKind::CodeProject,
        storage_mode: StorageMode::ProfileSharded,
        project_root: project_root.to_path_buf(),
        data_root: data_root.clone(),
        graph_db_relpath: PathBuf::from("graph.db"),
        sessions_db_relpath: PathBuf::from("sessions.db"),
        branch_meta_relpath: PathBuf::from(tracedecay_runtime_core::storage::BRANCH_META_FILENAME),
    };
    std::fs::write(
        data_root.join(tracedecay_runtime_core::storage::STORE_MANIFEST_FILENAME),
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();

    seed_project(db, project_id, project_root, created_at).await;
    let transaction = db.begin_write_transaction().await.unwrap();
    transaction
        .execute(
            "INSERT INTO store_instances (
                store_id, project_id, store_kind, storage_mode, store_relpath,
                manifest_relpath, created_at, last_verified_at, last_write_at
             ) VALUES (?1, ?2, 'project', 'profile_sharded', ?3, NULL, ?4, NULL, ?4)",
            tracedecay_runtime_core::db::engine::params![
                store_id,
                project_id,
                format!("stores/{store_id}"),
                created_at
            ],
        )
        .await
        .unwrap();
    transaction.commit().await.unwrap();
    data_root
}

async fn seed_project(
    db: &RegisteredGlobalDb,
    project_id: &str,
    project_root: &Path,
    timestamp: i64,
) {
    let root = RegisteredGlobalDb::canonical_project_key(project_root);
    let transaction = db.begin_write_transaction().await.unwrap();
    transaction
        .execute(
            "INSERT INTO code_projects (
                project_id, canonical_root, display_root, created_at, last_seen_at
             ) VALUES (?1, ?2, ?2, ?3, ?3)
             ON CONFLICT(project_id) DO NOTHING",
            tracedecay_runtime_core::db::engine::params![project_id, root.as_str(), timestamp],
        )
        .await
        .unwrap();
    transaction
        .execute(
            "INSERT INTO project_aliases (alias_path, project_id, last_seen_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(alias_path) DO UPDATE SET
                project_id = excluded.project_id,
                last_seen_at = excluded.last_seen_at",
            tracedecay_runtime_core::db::engine::params![root, project_id, timestamp],
        )
        .await
        .unwrap();
    transaction.commit().await.unwrap();
}

#[tokio::test]
async fn registered_store_census_resumes_across_bounded_project_pages() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let (_runtime, db) = open_registered_db(&profile_root).await;
    for suffix in ["a", "b", "c"] {
        seed_store(
            db.as_ref(),
            &profile_root,
            &format!("project-{suffix}"),
            &format!("store-{suffix}"),
            &tmp.path().join(format!("missing-{suffix}")),
            1_700_000_000,
        )
        .await;
    }

    let first = build_store_census_page(db.as_ref(), &profile_root, None, 2)
        .await
        .unwrap();
    assert_eq!(first.entries.len(), 2);
    assert_eq!(first.next_cursor.as_deref(), Some("project-b"));

    let second =
        build_store_census_page(db.as_ref(), &profile_root, first.next_cursor.as_deref(), 2)
            .await
            .unwrap();
    assert_eq!(second.entries.len(), 1);
    assert_eq!(second.entries[0].project_id, "project-c");
    assert!(second.next_cursor.is_none());
}

// === Durable-memory guard ===================================================

/// A store whose graph database carries durable `memory_facts` rows must
/// never be collected, even when every registry/manifest/payload revival
/// check passes and the store is otherwise a textbook orphan.
#[tokio::test]
async fn durable_memory_rows_block_orphan_store_collection() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let dead_root = tmp.path().join("moved-away-repo");
    let (_runtime, db) = open_registered_db(&profile_root).await;
    let base = 1_700_000_000i64;
    let data_root = seed_store(
        &db,
        &profile_root,
        "proj_memory",
        "store_memory",
        &dead_root,
        base - 100 * DAY,
    )
    .await;

    {
        let connection = rusqlite::Connection::open(data_root.join("graph.db")).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE memory_facts (fact_id INTEGER PRIMARY KEY, content TEXT NOT NULL);
                 INSERT INTO memory_facts (fact_id, content) VALUES (1, 'durable fact');",
            )
            .unwrap();
    }

    let report = sweep_orphan_stores(&db, &profile_root, 7 * DAY, base, true)
        .await
        .unwrap();

    assert!(
        report.outcome.collected.is_empty(),
        "a store with durable memory rows must never be collected"
    );
    assert_eq!(report.outcome.errors.len(), 1);
    assert_eq!(
        report.outcome.errors[0].kind,
        CollectionFailureKind::DurableDataProtected
    );
    assert!(
        data_root.exists(),
        "durable-memory-protected store must remain on disk"
    );
    assert!(
        db.list_code_projects(usize::MAX)
            .await
            .unwrap()
            .into_iter()
            .any(|project| project.project_id == "proj_memory"),
        "registry row for a protected store must not be retired"
    );
}

/// The guard is schema-discovered, so current and future Memory V2 tables are
/// protected without adding every table name to a second hand-maintained list.
#[tokio::test]
async fn memory_v2_rows_block_orphan_store_collection() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let dead_root = tmp.path().join("moved-away-repo");
    let (_runtime, db) = open_registered_db(&profile_root).await;
    let base = 1_700_000_000i64;
    let data_root = seed_store(
        &db,
        &profile_root,
        "proj_memory_v2",
        "store_memory_v2",
        &dead_root,
        base - 100 * DAY,
    )
    .await;

    {
        let connection = rusqlite::Connection::open(data_root.join("graph.db")).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE memory_v2_assertions (
                    assertion_id TEXT PRIMARY KEY,
                    payload TEXT NOT NULL
                 );
                 INSERT INTO memory_v2_assertions (assertion_id, payload)
                 VALUES ('assertion-1', 'durable v2 fact');",
            )
            .unwrap();
    }

    let report = sweep_orphan_stores(&db, &profile_root, 7 * DAY, base, true)
        .await
        .unwrap();

    assert!(report.outcome.collected.is_empty());
    assert_eq!(
        report.outcome.errors[0].kind,
        CollectionFailureKind::DurableDataProtected
    );
    assert!(data_root.exists());
}

/// A durable memory table that exists but is empty must not block collection
/// — only an actual row does.
#[tokio::test]
async fn empty_memory_table_does_not_block_collection() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let dead_root = tmp.path().join("moved-away-repo");
    let (_runtime, db) = open_registered_db(&profile_root).await;
    let base = 1_700_000_000i64;
    let data_root = seed_store(
        &db,
        &profile_root,
        "proj_empty_memory",
        "store_empty_memory",
        &dead_root,
        base - 100 * DAY,
    )
    .await;
    {
        let connection = rusqlite::Connection::open(data_root.join("graph.db")).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE memory_facts (
                    fact_id INTEGER PRIMARY KEY,
                    content TEXT NOT NULL
                 );
                 CREATE VIRTUAL TABLE memory_facts_fts USING fts5(content);",
            )
            .unwrap();
    }

    let report = sweep_orphan_stores(&db, &profile_root, 7 * DAY, base, true)
        .await
        .unwrap();

    assert_eq!(
        report.outcome.collected.len(),
        1,
        "empty durable-memory tables must not block collection: {report:#?}"
    );
    assert!(!data_root.exists());
}

// === Unregistered store directories =========================================

#[tokio::test]
async fn census_finds_unregistered_project_dir_and_ignores_registered_ones() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let (_runtime, db) = open_registered_db(&profile_root).await;

    let registered_root = tmp.path().join("registered-repo");
    std::fs::create_dir_all(&registered_root).unwrap();
    seed_project(&db, "proj_registered", &registered_root, 1_700_000_000).await;
    let registered_dir = profile_root.join("projects").join("proj_registered");
    std::fs::create_dir_all(&registered_dir).unwrap();

    // A genuinely unregistered directory: no `code_projects` row was ever
    // written for it.
    let unregistered_dir = profile_root.join("projects").join("proj_ghost");
    std::fs::create_dir_all(&unregistered_dir).unwrap();
    std::fs::write(unregistered_dir.join("payload.bin"), vec![0u8; 4096]).unwrap();

    // A stray, unsafely-named entry under `projects/` must never be treated
    // as a candidate.
    std::fs::write(profile_root.join("projects").join("not-a-store.txt"), b"x").unwrap();

    let now = 1_700_100_000i64;
    let findings = census_unregistered_project_dirs(&db, &profile_root, now)
        .await
        .unwrap();

    assert_eq!(findings.len(), 1, "findings: {findings:?}");
    assert_eq!(findings[0].project_dir_name, "proj_ghost");
    assert_eq!(findings[0].data_root, unregistered_dir);
    assert!(findings[0].size_bytes >= 4096);
}

#[tokio::test]
async fn sweep_unregistered_stores_protects_unverifiable_payload_and_retains_young() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let (_runtime, db) = open_registered_db(&profile_root).await;

    let base = 1_700_000_000i64;
    let old_dir = profile_root.join("projects").join("proj_old_ghost");
    std::fs::create_dir_all(&old_dir).unwrap();
    std::fs::write(old_dir.join("payload.bin"), b"old").unwrap();
    filetime::set_file_mtime(
        old_dir.join("payload.bin"),
        filetime::FileTime::from_unix_time(base - 100 * DAY, 0),
    )
    .unwrap();

    let young_dir = profile_root.join("projects").join("proj_young_ghost");
    std::fs::create_dir_all(&young_dir).unwrap();
    std::fs::write(young_dir.join("payload.bin"), b"young").unwrap();
    filetime::set_file_mtime(
        young_dir.join("payload.bin"),
        filetime::FileTime::from_unix_time(base - DAY, 0),
    )
    .unwrap();

    // Dry run: classifies but mutates nothing.
    let report = sweep_unregistered_stores(&db, &profile_root, 7 * DAY, base, false)
        .await
        .unwrap();
    assert_eq!(report.plan.collect.len(), 1);
    assert_eq!(report.plan.collect[0].project_dir_name, "proj_old_ghost");
    assert_eq!(report.plan.retained_immature.len(), 1);
    assert!(!report.applied);
    assert!(old_dir.exists(), "dry run must not delete");

    let report = sweep_unregistered_stores(&db, &profile_root, 7 * DAY, base, true)
        .await
        .unwrap();
    assert!(report.applied);
    assert!(report.outcome.collected.is_empty());
    assert_eq!(report.outcome.errors.len(), 1);
    assert_eq!(
        report.outcome.errors[0].kind,
        CollectionFailureKind::DurableDataProtected
    );
    assert!(
        old_dir.exists(),
        "arbitrary payload without a manifest cannot prove durable-data absence"
    );
    assert!(
        young_dir.exists(),
        "immature unregistered dir must be retained"
    );
}

#[tokio::test]
async fn sweep_unregistered_stores_collects_an_exactly_empty_old_directory() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let (_runtime, db) = open_registered_db(&profile_root).await;

    let base = 1_700_000_000i64;
    let empty_dir = profile_root.join("projects").join("proj_empty_ghost");
    std::fs::create_dir_all(&empty_dir).unwrap();

    let report = sweep_unregistered_stores(&db, &profile_root, 7 * DAY, base, true)
        .await
        .unwrap();

    assert_eq!(report.plan.collect.len(), 1);
    assert_eq!(report.outcome.collected.len(), 1);
    assert!(report.outcome.errors.is_empty());
    assert!(!empty_dir.exists());
}

/// A registered project id must never be treated as an unregistered
/// candidate, and re-registering between census and collection must abort
/// the delete for that finding (closing the revival window).
#[tokio::test]
async fn sweep_unregistered_stores_aborts_when_directory_gets_registered_first() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let (_runtime, db) = open_registered_db(&profile_root).await;

    let base = 1_700_000_000i64;
    let dir = profile_root.join("projects").join("proj_became_live");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("payload.bin"), b"payload").unwrap();
    filetime::set_file_mtime(
        dir.join("payload.bin"),
        filetime::FileTime::from_unix_time(base - 100 * DAY, 0),
    )
    .unwrap();

    let findings = census_unregistered_project_dirs(&db, &profile_root, base)
        .await
        .unwrap();
    assert_eq!(findings.len(), 1);
    let plan = plan_unregistered_collection(findings, 7 * DAY);
    assert_eq!(plan.collect.len(), 1);

    // The directory gets registered for real between census and apply.
    let live_root = tmp.path().join("now-live-repo");
    std::fs::create_dir_all(&live_root).unwrap();
    seed_project(&db, "proj_became_live", &live_root, base).await;

    let outcome = execute_unregistered_collection(&db, &plan, &profile_root)
        .await
        .unwrap();
    assert!(outcome.collected.is_empty());
    assert_eq!(outcome.errors.len(), 1);
    assert_eq!(
        outcome.errors[0].kind,
        CollectionFailureKind::RegistryChanged
    );
    assert!(
        dir.exists(),
        "a directory registered before delete must survive"
    );
}

/// An unregistered directory uses the same inspect→confirm→apply boundary as
/// a registered orphan. A same-second replacement of an empty directory must
/// not inherit the original collection decision.
#[cfg(unix)]
#[tokio::test]
async fn unregistered_collection_refuses_same_second_directory_replacement() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let (_runtime, db) = open_registered_db(&profile_root).await;
    let data_root = profile_root.join("projects/proj_replaced_unregistered");
    std::fs::create_dir_all(&data_root).unwrap();

    let now = walk_store_stats(&data_root)
        .newest_mtime_secs
        .saturating_add(100 * DAY);
    let findings = census_unregistered_project_dirs(&db, &profile_root, now)
        .await
        .unwrap();
    let plan = plan_unregistered_collection(findings, 7 * DAY);
    assert_eq!(
        plan.collect.len(),
        1,
        "fixture must be eligible before replacement"
    );

    let displaced = profile_root.join("displaced-unregistered-store");
    let original_time =
        filetime::FileTime::from_system_time(data_root.metadata().unwrap().modified().unwrap());
    std::fs::rename(&data_root, &displaced).unwrap();
    std::fs::create_dir_all(&data_root).unwrap();
    filetime::set_file_mtime(&data_root, original_time).unwrap();

    let outcome = execute_unregistered_collection(&db, &plan, &profile_root)
        .await
        .unwrap();

    assert!(outcome.collected.is_empty());
    assert_eq!(
        outcome.errors,
        vec![CollectionFailure {
            store_id: "proj_replaced_unregistered".to_owned(),
            kind: CollectionFailureKind::PayloadChanged,
        }]
    );
    assert!(data_root.exists(), "replacement directory must survive");
}

/// An unregistered leaf must not become a deletion target merely because its
/// symlink resolves back inside the profile. The physical `<profile>/projects`
/// path, rather than canonicalized containment, is the destructive authority.
#[cfg(unix)]
#[tokio::test]
async fn unregistered_collection_rejects_profile_contained_data_root_symlink() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let (_runtime, db) = open_registered_db(&profile_root).await;
    let data_root = profile_root.join("projects/proj_symlinked_unregistered");
    std::fs::create_dir_all(&data_root).unwrap();

    let now = walk_store_stats(&data_root)
        .newest_mtime_secs
        .saturating_add(100 * DAY);
    let findings = census_unregistered_project_dirs(&db, &profile_root, now)
        .await
        .unwrap();
    let plan = plan_unregistered_collection(findings, 7 * DAY);
    assert_eq!(
        plan.collect.len(),
        1,
        "fixture must be eligible before the symlink swap"
    );

    let held_payload = profile_root.join("held-unregistered-payload");
    std::fs::rename(&data_root, &held_payload).unwrap();
    std::os::unix::fs::symlink(&held_payload, &data_root).unwrap();

    let outcome = execute_unregistered_collection(&db, &plan, &profile_root)
        .await
        .unwrap();

    assert!(outcome.collected.is_empty());
    assert_eq!(
        outcome.errors,
        vec![CollectionFailure {
            store_id: "proj_symlinked_unregistered".to_owned(),
            kind: CollectionFailureKind::OutsideProfile,
        }]
    );
    assert!(
        held_payload.is_dir(),
        "the in-profile symlink target must survive"
    );
}

/// A manifest write can preserve second-resolution mtimes and leave the store
/// directory untouched. The quarantine boundary must therefore hash and
/// re-verify the moved children before any deletion is possible.
#[test]
fn quarantine_restores_same_second_manifest_mutation() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    let data_root = profile_root.join("stores/manifest-race");
    std::fs::create_dir_all(&data_root).unwrap();
    let manifest = data_root.join(tracedecay_runtime_core::storage::STORE_MANIFEST_FILENAME);
    std::fs::write(&manifest, b"before").unwrap();
    let expected = capture_store_content_fence(&profile_root, &data_root).unwrap();
    let original_time =
        filetime::FileTime::from_system_time(manifest.metadata().unwrap().modified().unwrap());

    std::fs::write(&manifest, b"after!").unwrap();
    filetime::set_file_mtime(&manifest, original_time).unwrap();

    let result =
        quarantine_store_for_verified_collection(&profile_root, &data_root, &expected).unwrap();
    assert!(matches!(result, QuarantineStoreOutcome::Restored { .. }));
    assert_eq!(std::fs::read(&manifest).unwrap(), b"after!");
    assert!(
        data_root.is_dir(),
        "changed bytes must be restored, not deleted"
    );
}

/// `SQLite` pages can change in place without changing the parent directory.
/// Hashing the opened child handles makes that post-census mutation visible at
/// the recovery boundary even when the writer resets the database mtime.
#[test]
fn quarantine_restores_same_second_sqlite_mutation() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    let data_root = profile_root.join("stores/sqlite-race");
    std::fs::create_dir_all(&data_root).unwrap();
    let database = data_root.join("graph.db");
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection
        .execute_batch("CREATE TABLE facts (value TEXT NOT NULL);")
        .unwrap();
    drop(connection);
    let expected = capture_store_content_fence(&profile_root, &data_root).unwrap();
    let original_time =
        filetime::FileTime::from_system_time(database.metadata().unwrap().modified().unwrap());

    let connection = rusqlite::Connection::open(&database).unwrap();
    connection
        .execute("INSERT INTO facts (value) VALUES ('post-census')", [])
        .unwrap();
    drop(connection);
    filetime::set_file_mtime(&database, original_time).unwrap();

    let result =
        quarantine_store_for_verified_collection(&profile_root, &data_root, &expected).unwrap();
    assert!(matches!(result, QuarantineStoreOutcome::Restored { .. }));
    let connection = rusqlite::Connection::open(&database).unwrap();
    let rows: i64 = connection
        .query_row("SELECT COUNT(*) FROM facts", [], |row| row.get(0))
        .unwrap();
    assert_eq!(rows, 1, "mutated SQLite bytes must survive recovery");
}

/// A path replacement immediately before the atomic boundary may cause the
/// rename to capture the replacement. It must be verified and restored rather
/// than recursively deleting whichever directory won the path race.
#[test]
fn quarantine_restores_path_replacement_before_delete() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    let data_root = profile_root.join("stores/rename-race");
    std::fs::create_dir_all(&data_root).unwrap();
    std::fs::write(data_root.join("payload.bin"), b"inspected").unwrap();
    let expected = capture_store_content_fence(&profile_root, &data_root).unwrap();

    let displaced = profile_root.join("stores/displaced");
    std::fs::rename(&data_root, &displaced).unwrap();
    std::fs::create_dir_all(&data_root).unwrap();
    std::fs::write(data_root.join("payload.bin"), b"replacement").unwrap();

    let result =
        quarantine_store_for_verified_collection(&profile_root, &data_root, &expected).unwrap();
    assert!(matches!(result, QuarantineStoreOutcome::Restored { .. }));
    assert_eq!(
        std::fs::read(data_root.join("payload.bin")).unwrap(),
        b"replacement"
    );
    assert_eq!(
        std::fs::read(displaced.join("payload.bin")).unwrap(),
        b"inspected"
    );
}

/// Even an empty replacement is not the inspected directory. Its child list
/// is identical, so the moved root's stable identity must participate in the
/// post-rename comparison before collection can remove anything.
#[test]
fn quarantine_restores_empty_directory_replacement_before_delete() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    let data_root = profile_root.join("stores/empty-rename-race");
    std::fs::create_dir_all(&data_root).unwrap();
    let expected = capture_store_content_fence(&profile_root, &data_root).unwrap();

    let displaced = profile_root.join("stores/displaced-empty");
    std::fs::rename(&data_root, &displaced).unwrap();
    std::fs::create_dir_all(&data_root).unwrap();

    let result =
        quarantine_store_for_verified_collection(&profile_root, &data_root, &expected).unwrap();

    assert!(matches!(result, QuarantineStoreOutcome::Restored { .. }));
    assert!(data_root.is_dir(), "fresh empty replacement must survive");
    assert!(displaced.is_dir(), "inspected empty directory must survive");
}

/// A Windows directory capability denies same-parent rename because cap-std
/// deliberately omits FILE_SHARE_DELETE. The failed retirement must remain a
/// typed deferral over the exact censused store, then converge once that
/// external owner releases its handle.
#[cfg(windows)]
#[test]
fn quarantine_defers_held_windows_store_and_converges_after_release() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    let data_root = profile_root.join("stores/held-capability");
    let payload_path = data_root.join("payload.bin");
    let payload = b"held store bytes remain authoritative";
    std::fs::create_dir_all(&data_root).unwrap();
    std::fs::write(&payload_path, payload).unwrap();
    let expected = capture_store_content_fence(&profile_root, &data_root).unwrap();
    let StoreContentFence::Present(expected_inventory) = &expected else {
        panic!("fixture must capture an exact present-store fence");
    };
    let expected_root_identity = expected_inventory.root.clone();
    let external_owner =
        cap_std::fs::Dir::open_ambient_dir(&data_root, cap_std::ambient_authority()).unwrap();

    let failure =
        match quarantine_store_for_verified_collection(&profile_root, &data_root, &expected) {
            Err(failure) => failure,
            Ok(_) => panic!("held live leaf must not enter quarantine"),
        };
    let CollectionFailureKind::RemoveFailed(failure) = failure else {
        panic!("held live leaf must report a structured mutation failure");
    };
    assert_eq!(
        failure.operation,
        CollectionMutationOperation::RenameLiveLeafToQuarantine
    );
    assert!(
        matches!(failure.raw_os_error, Some(5 | 32)),
        "Windows held-directory rename must preserve access-denied/sharing violation: {failure:?}"
    );
    assert!(failure.retryable());
    assert_eq!(
        failure.classification,
        CollectionMutationFailureClassification::RetryableDeferred
    );
    assert_eq!(failure.target_path, data_root);
    assert_eq!(failure.expected_root_identity, Some(expected_root_identity));
    assert_eq!(std::fs::read(&payload_path).unwrap(), payload);
    assert!(
        data_root.is_dir(),
        "failed quarantine must not collect the store"
    );
    assert!(
        read_pending_quarantine_receipts(&profile_root)
            .unwrap()
            .is_empty(),
        "the failed rename's prepared journal must still be cleared"
    );

    drop(external_owner);
    let result =
        quarantine_store_for_verified_collection(&profile_root, &data_root, &expected).unwrap();
    let QuarantineStoreOutcome::Verified(quarantine) = result else {
        panic!("retry after releasing the external owner must verify quarantine");
    };
    assert_eq!(
        std::fs::read(quarantine.quarantine_path().join("payload.bin")).unwrap(),
        payload
    );
    quarantine.mark_retirement_committed().unwrap();
    let QuarantineFinalizeOutcome::Removed { journal_failure } =
        quarantine.finalize(unbounded_collection_control())
    else {
        panic!("released quarantine must complete the durable removal sequence");
    };
    assert_eq!(journal_failure, None);
    assert!(!data_root.exists());
    assert!(
        read_pending_quarantine_receipts(&profile_root)
            .unwrap()
            .is_empty()
    );
}

/// This simulates a process death after the registry phase was durably marked
/// but before recursive removal. The journal, quarantine path, and committed
/// phase remain readable without relying on an in-memory collection outcome.
#[test]
fn committed_quarantine_crash_boundary_has_a_readable_recovery_receipt() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    let data_root = profile_root.join("stores/crash-boundary");
    std::fs::create_dir_all(&data_root).unwrap();
    std::fs::write(data_root.join("payload.bin"), b"preserve until finalize").unwrap();
    let expected = capture_store_content_fence(&profile_root, &data_root).unwrap();

    let result =
        quarantine_store_for_verified_collection(&profile_root, &data_root, &expected).unwrap();
    let QuarantineStoreOutcome::Verified(quarantine) = result else {
        panic!("fixture must reach verified quarantine");
    };
    quarantine.mark_retirement_committed().unwrap();
    drop(quarantine);

    let receipts = read_pending_quarantine_receipts(&profile_root).unwrap();
    assert_eq!(receipts.len(), 1);
    assert!(receipts[0].retirement_committed);
    assert!(receipts[0].quarantine_path.is_dir());
    assert!(!data_root.exists(), "live name remains private after crash");
}

/// A restore rename can complete before the parent-directory sync or journal
/// cleanup. The mounted reader must expose the bytes' original live path,
/// rather than the now-absent quarantine name, while retaining the receipt.
#[test]
fn pending_quarantine_reader_reports_restored_path_when_sync_is_unconfirmed() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    let data_root = profile_root.join("stores/restore-sync-boundary");
    std::fs::create_dir_all(&data_root).unwrap();
    std::fs::write(
        data_root.join("payload.bin"),
        b"restore location is authoritative",
    )
    .unwrap();
    let expected = capture_store_content_fence(&profile_root, &data_root).unwrap();

    let result =
        quarantine_store_for_verified_collection(&profile_root, &data_root, &expected).unwrap();
    let QuarantineStoreOutcome::Verified(quarantine) = result else {
        panic!("fixture must reach verified quarantine");
    };
    let quarantine_path = quarantine.quarantine_path().to_path_buf();
    drop(quarantine);
    // This is the persisted shape after the rename succeeds but a later
    // directory sync/journal cleanup cannot be confirmed.
    std::fs::rename(&quarantine_path, &data_root).unwrap();

    let receipts = read_pending_quarantine_receipts(&profile_root).unwrap();
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].quarantine_path, quarantine_path);
    assert_eq!(receipts[0].actual_path, data_root);
}

/// Cancellation is checked before any recursive SHA-256 read. A cancelled
/// maintenance admission cannot turn a deep inventory into a partial plan or
/// an implicit deletion permit.
#[test]
fn cancelled_content_census_stops_before_hashing_or_mutation() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    let data_root = profile_root.join("stores/cancelled-census");
    std::fs::create_dir_all(&data_root).unwrap();
    std::fs::write(data_root.join("large.bin"), vec![7_u8; 512 * 1024]).unwrap();
    let cancellation = CancellationToken::new();
    cancellation.cancel();

    let result = super::fence::capture_store_content_fence_controlled(
        &profile_root,
        &data_root,
        CollectionControl::new(
            &cancellation,
            MonotonicDeadline::at(Instant::now() + Duration::from_secs(1)),
        ),
    );

    assert_eq!(result, Err(CollectionFailureKind::Cancelled));
    assert!(data_root.join("large.bin").is_file());
}

/// Recursive age and size accounting obey the same cancellation authority as
/// hashing, so a deep store cannot consume an unbounded maintenance budget
/// after the pass has already been cancelled.
#[test]
fn cancelled_mtime_and_size_walks_stop_before_descending() {
    let tmp = tempfile::TempDir::new().unwrap();
    let data_root = tmp.path().join("deep");
    std::fs::create_dir_all(data_root.join("a/b/c")).unwrap();
    std::fs::write(data_root.join("a/b/c/payload.bin"), vec![3_u8; 4096]).unwrap();
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let control = CollectionControl::new(
        &cancellation,
        MonotonicDeadline::at(Instant::now() + Duration::from_secs(1)),
    );

    assert_eq!(
        newest_mtime_secs_controlled(&data_root, control),
        Err(CollectionFailureKind::Cancelled)
    );
    assert_eq!(
        dir_size_bytes_controlled(&data_root, control),
        Err(CollectionFailureKind::Cancelled)
    );
    assert!(data_root.join("a/b/c/payload.bin").is_file());
}

/// Build enough no-follow entries that a bounded apply can be interrupted in
/// the payload-mtime fence itself, after the apply loop has admitted the
/// finding. The production path must stop with a typed completion rather than
/// recording `Cancelled` as an ordinary per-store error and claiming success.
fn seed_payload_fence_work(data_root: &Path) {
    std::fs::create_dir_all(data_root).unwrap();
    for bucket_index in 0..32 {
        std::fs::create_dir_all(data_root.join(format!("bucket-{bucket_index:03}"))).unwrap();
    }
    for index in 0..30_000usize {
        let bucket = data_root.join(format!("bucket-{:03}", index % 32));
        std::fs::write(bucket.join(format!("payload-{index:05}.bin")), b"x").unwrap();
    }
}

fn payload_fence_finding(data_root: PathBuf, expected_store_relpath: &str) -> OrphanStoreFinding {
    let profile_root = data_root
        .parent()
        .and_then(Path::parent)
        .expect("fixture data root has a two-component profile path")
        .to_path_buf();
    OrphanStoreFinding {
        project_id: "proj_payload_fence_interrupt".to_owned(),
        store_id: "store_payload_fence_interrupt".to_owned(),
        data_root: data_root.clone(),
        disposition: StoreDisposition::Orphaned,
        age_secs: 90 * DAY,
        size_bytes: 30_000,
        expected_store_relpath: expected_store_relpath.to_owned(),
        expected_created_at: 1,
        expected_last_write_at: None,
        expected_payload_mtime_secs: walk_store_stats(&data_root).newest_mtime_secs,
        expected_data_root_fence: capture_store_directory_fence(&profile_root, &data_root).unwrap(),
        // The mtime fence is the boundary under test; no later phase should be
        // reached when this control is interrupted.
        expected_content_fence: StoreContentFence::Missing,
        expected_manifest_bytes: None,
        graph_scope_relpaths: Vec::new(),
    }
}

#[tokio::test]
async fn registered_collection_payload_fence_cancellation_is_terminal() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    let data_root = profile_root.join("stores/payload-fence-cancelled");
    seed_payload_fence_work(&data_root);
    let (_runtime, db) = open_registered_db(&profile_root).await;
    let finding = payload_fence_finding(data_root.clone(), "stores/payload-fence-cancelled");
    let plan = CollectionPlan {
        collect: vec![finding],
        ..CollectionPlan::default()
    };
    let cancellation = CancellationToken::new();
    let started = std::sync::Arc::new(AtomicBool::new(false));
    let started_thread = std::sync::Arc::clone(&started);
    let cancellation_thread = cancellation.clone();
    let signal = std::thread::spawn(move || {
        while !started_thread.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
        std::thread::sleep(Duration::from_millis(20));
        cancellation_thread.cancel();
    });
    started.store(true, Ordering::Release);

    let (outcome, retired) = execute_registered_collection_controlled(
        &db,
        &plan,
        &profile_root,
        CollectionControl::new(
            &cancellation,
            MonotonicDeadline::at(Instant::now() + Duration::from_secs(5)),
        ),
    )
    .await
    .unwrap();
    signal.join().unwrap();

    assert_eq!(retired, 0);
    assert_eq!(outcome.completion, CollectionCompletionV1::Cancelled);
    assert!(outcome.errors.is_empty());
    assert!(outcome.collected.is_empty());
    assert!(data_root.exists());
}

#[tokio::test]
async fn unregistered_collection_payload_fence_deadline_is_distinct() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    let data_root = profile_root.join("projects/proj_payload_fence_deadline");
    seed_payload_fence_work(&data_root);
    let (_runtime, db) = open_registered_db(&profile_root).await;
    let finding = payload_fence_finding(data_root.clone(), "projects/proj_payload_fence_deadline");
    let plan = UnregisteredCollectionPlan {
        collect: vec![UnregisteredStoreFinding {
            project_dir_name: "proj_payload_fence_deadline".to_owned(),
            data_root: finding.data_root,
            age_secs: finding.age_secs,
            size_bytes: finding.size_bytes,
            expected_payload_mtime_secs: finding.expected_payload_mtime_secs,
            expected_data_root_fence: finding.expected_data_root_fence,
            expected_content_fence: finding.expected_content_fence,
        }],
        ..UnregisteredCollectionPlan::default()
    };

    let cancellation = CancellationToken::new();
    let (outcome, deadline) = {
        let deadline = MonotonicDeadline::at(Instant::now() + Duration::from_millis(20));
        let outcome = execute_unregistered_collection_controlled(
            &db,
            &plan,
            &profile_root,
            CollectionControl::new(&cancellation, deadline),
        )
        .await
        .unwrap();
        (outcome, deadline)
    };

    assert!(deadline.is_elapsed_at(Instant::now()));
    assert_eq!(outcome.completion, CollectionCompletionV1::DeadlineExceeded);
    assert!(outcome.errors.is_empty());
    assert!(outcome.collected.is_empty());
    assert!(data_root.exists());
}

/// Once SQL retirement has been marked, cancellation during recursive remove
/// retains the journal-backed quarantine rather than reporting reclaimed
/// bytes. Restart reconciliation owns the remaining irreversible work.
#[test]
fn cancelled_quarantine_finalization_retains_a_readable_recovery_record() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    let data_root = profile_root.join("stores/cancelled-finalize");
    std::fs::create_dir_all(&data_root).unwrap();
    std::fs::write(data_root.join("payload.bin"), b"retain while cancelled").unwrap();
    let expected = capture_store_content_fence(&profile_root, &data_root).unwrap();
    let result =
        quarantine_store_for_verified_collection(&profile_root, &data_root, &expected).unwrap();
    let QuarantineStoreOutcome::Verified(quarantine) = result else {
        panic!("fixture must reach verified quarantine");
    };
    quarantine.mark_retirement_committed().unwrap();
    let quarantine_path = quarantine.quarantine_path().to_path_buf();
    let cancellation = CancellationToken::new();
    cancellation.cancel();

    assert!(matches!(
        quarantine.finalize(CollectionControl::new(
            &cancellation,
            MonotonicDeadline::at(Instant::now() + Duration::from_secs(1),),
        )),
        QuarantineFinalizeOutcome::Interrupted { .. }
    ));
    assert!(quarantine_path.is_dir());
    assert_eq!(
        read_pending_quarantine_receipts(&profile_root)
            .unwrap()
            .len(),
        1
    );
}

/// A process can stop after the same-parent rename and before it has an
/// in-memory outcome to return. The next maintenance admission must restore
/// that durable quarantine and surface it for a fresh census, not leave it
/// invisible under an internal sibling name.
#[test]
fn interrupted_quarantine_is_restored_on_the_next_collection_admission() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    let data_root = profile_root.join("projects/proj_recover_quarantine");
    std::fs::create_dir_all(&data_root).unwrap();
    std::fs::write(data_root.join("payload.bin"), b"recover me").unwrap();
    let quarantine =
        profile_root.join("projects/.tracedecay-orphan-quarantine-proj_recover_quarantine-42-7");
    std::fs::rename(&data_root, &quarantine).unwrap();

    let outcomes = recover_existing_store_quarantine(&profile_root, &data_root).unwrap();

    assert_eq!(
        outcomes,
        vec![QuarantineRecoveryOutcome::Restored {
            restored_path: data_root.clone(),
            failure: None,
        }]
    );
    assert_eq!(
        std::fs::read(data_root.join("payload.bin")).unwrap(),
        b"recover me"
    );
    assert!(!quarantine.exists());
}

/// Recovery must never overwrite a newly created live path merely to put an
/// interrupted quarantine back at its old name. Both byte sets remain
/// available for a typed operator recovery outcome.
#[test]
fn interrupted_quarantine_is_retained_when_a_new_live_store_owns_its_name() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    let data_root = profile_root.join("projects/proj_retained_quarantine");
    std::fs::create_dir_all(&data_root).unwrap();
    std::fs::write(data_root.join("payload.bin"), b"quarantined bytes").unwrap();
    let quarantine =
        profile_root.join("projects/.tracedecay-orphan-quarantine-proj_retained_quarantine-42-7");
    std::fs::rename(&data_root, &quarantine).unwrap();
    std::fs::create_dir_all(&data_root).unwrap();
    std::fs::write(data_root.join("payload.bin"), b"new live bytes").unwrap();

    let outcomes = recover_existing_store_quarantine(&profile_root, &data_root).unwrap();

    let expected_failure = CollectionMutationFailure {
        operation: CollectionMutationOperation::RestoreLiveLeafFromQuarantine,
        raw_os_error: Some(OCCUPIED_RENAME_RAW_OS_ERROR),
        target_path: data_root.clone(),
        expected_root_identity: None,
        classification: CollectionMutationFailureClassification::NonRetryable,
    };
    assert_eq!(
        outcomes,
        vec![QuarantineRecoveryOutcome::Retained {
            quarantine_path: quarantine.clone(),
            failure: Some(expected_failure.clone()),
        }]
    );
    assert_eq!(
        std::fs::read(data_root.join("payload.bin")).unwrap(),
        b"new live bytes"
    );
    assert_eq!(
        std::fs::read(quarantine.join("payload.bin")).unwrap(),
        b"quarantined bytes"
    );

    let mut collection = CollectionOutcome::default();
    assert!(!reconcile_existing_quarantine(
        &profile_root,
        &data_root,
        "proj_retained_quarantine",
        &mut collection,
    ));
    assert_eq!(
        collection,
        CollectionOutcome {
            errors: vec![
                CollectionFailure {
                    store_id: "proj_retained_quarantine".to_owned(),
                    kind: CollectionFailureKind::RemoveFailed(expected_failure),
                },
                CollectionFailure {
                    store_id: "proj_retained_quarantine".to_owned(),
                    kind: CollectionFailureKind::PayloadChanged,
                },
            ],
            recovery_receipts: vec![CollectionRecoveryReceipt {
                store_id: "proj_retained_quarantine".to_owned(),
                original_path: data_root,
                quarantine_path: quarantine.clone(),
                actual_path: quarantine,
                action: CollectionRecoveryAction::RetainedForRecovery,
            }],
            ..CollectionOutcome::default()
        }
    );
}

/// Unregistered projects are an on-disk-only class, but their retention work
/// still advances through a bounded, resumable page rather than recursing the
/// entire profile under a single writer admission.
#[tokio::test]
async fn unregistered_store_sweep_applies_one_cursor_page_at_a_time() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let (_runtime, db) = open_registered_db(&profile_root).await;
    let base = 1_700_000_000i64;
    for name in ["proj_page_a", "proj_page_b", "proj_page_c"] {
        std::fs::create_dir_all(profile_root.join("projects").join(name)).unwrap();
    }
    let cancellation = CancellationToken::new();
    let deadline = MonotonicDeadline::at(Instant::now() + Duration::from_secs(1));

    let first = sweep_unregistered_store_page(
        &db,
        &profile_root,
        UnregisteredStoreSweepRequestV1 {
            cursor: None,
            limit: 2,
            retention_secs: 0,
            now: base,
            apply: true,
            cancellation: &cancellation,
            deadline,
        },
    )
    .await
    .unwrap();
    assert_eq!(first.completion, UnregisteredSweepCompletionV1::Complete);
    assert_eq!(first.outcome.collected.len(), 2);
    let cursor = first
        .next_cursor
        .clone()
        .expect("a third directory requires a second page");

    let second = sweep_unregistered_store_page(
        &db,
        &profile_root,
        UnregisteredStoreSweepRequestV1 {
            cursor: Some(cursor),
            limit: 2,
            retention_secs: 0,
            now: base,
            apply: true,
            cancellation: &cancellation,
            deadline,
        },
    )
    .await
    .unwrap();
    assert_eq!(second.completion, UnregisteredSweepCompletionV1::Complete);
    assert_eq!(second.outcome.collected.len(), 1);
    assert!(second.next_cursor.is_none());
    assert!(
        !profile_root.join("projects/proj_page_a").exists()
            && !profile_root.join("projects/proj_page_b").exists()
            && !profile_root.join("projects/proj_page_c").exists(),
        "both bounded pages must eventually reclaim their disjoint directories"
    );
}

/// The page cursor must survive both a process restart and directory
/// mutation. It is the last name the previous page returned, so nothing about
/// resuming depends on a live directory stream, an OS offset cookie, or a
/// sidecar log: a fresh process resumes the same slice, entries the previous
/// pages already collected can be gone, and entries created behind the cursor
/// cannot silently displace the ones ahead of it.
#[test]
fn project_directory_cursor_resumes_after_restart_and_directory_mutation() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    let projects_dir = profile_root.join("projects");
    for name in ["proj_a", "proj_b", "proj_c", "proj_d", "proj_e", "proj_f"] {
        std::fs::create_dir_all(projects_dir.join(name)).unwrap();
    }
    let interrupted = || false;
    let page_names = |cursor: Option<&str>| {
        let page = super::unregistered_page::read_project_directory_page(
            &profile_root,
            cursor,
            2,
            &interrupted,
        )
        .unwrap()
        .expect("an uninterrupted page is never a cancelled read");
        let names = page
            .entries
            .iter()
            .map(|entry| match entry {
                super::ProjectDirectoryWorkV1::Project(name) => name.clone(),
                super::ProjectDirectoryWorkV1::Quarantine {
                    quarantine_name, ..
                } => quarantine_name.clone(),
            })
            .collect::<Vec<_>>();
        (names, page.next_cursor)
    };

    let (first, first_cursor) = page_names(None);
    assert_eq!(first, vec!["proj_a".to_owned(), "proj_b".to_owned()]);
    let first_cursor = first_cursor.expect("four more directories require a second page");
    assert_eq!(
        first_cursor, "name-v3:proj_b",
        "the resume token must be the last returned name, not an OS stream offset"
    );

    // Everything a restart plus an applied page can do to the directory: the
    // collected entries disappear, an unrelated entry ahead of the cursor
    // disappears, and new entries appear on both sides of the cursor.
    std::fs::remove_dir(projects_dir.join("proj_a")).unwrap();
    std::fs::remove_dir(projects_dir.join("proj_b")).unwrap();
    std::fs::remove_dir(projects_dir.join("proj_e")).unwrap();
    std::fs::create_dir_all(projects_dir.join("proj_aa")).unwrap();
    std::fs::create_dir_all(projects_dir.join("proj_z")).unwrap();

    let (second, second_cursor) = page_names(Some(&first_cursor));
    assert_eq!(
        second,
        vec!["proj_c".to_owned(), "proj_d".to_owned()],
        "a mutated directory must resume at the cursor rather than restart or terminate early"
    );
    let second_cursor = second_cursor.expect("two directories still sort after the cursor");

    // Replaying the same cursor is a position, not a consumed stream slot.
    let (replayed, replayed_cursor) = page_names(Some(&first_cursor));
    assert_eq!(replayed, second);
    assert_eq!(replayed_cursor.as_deref(), Some(second_cursor.as_str()));

    let (third, third_cursor) = page_names(Some(&second_cursor));
    assert_eq!(
        third,
        vec!["proj_f".to_owned(), "proj_z".to_owned()],
        "a directory created after the cursor must still be reached in this pass"
    );
    assert_eq!(
        third_cursor, None,
        "an exhausted name order must terminate the pass instead of paging forever"
    );

    // `proj_aa` sorts behind the cursor, so this pass never returns it; the
    // next full pass does. Nothing else was skipped.
    assert!(projects_dir.join("proj_aa").is_dir());
}

/// An unknown or truncated resume token restarts the pass. Treating it as a
/// name would silently skip every directory that sorts before it.
#[test]
fn project_directory_unknown_cursor_restarts_the_pass() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    for name in ["proj_a", "proj_b"] {
        std::fs::create_dir_all(profile_root.join("projects").join(name)).unwrap();
    }
    let interrupted = || false;
    for cursor in ["", "proj_b", "v1:2049:12345:99", "name-v3:"] {
        let page = super::unregistered_page::read_project_directory_page(
            &profile_root,
            Some(cursor),
            8,
            &interrupted,
        )
        .unwrap()
        .expect("an uninterrupted page is never a cancelled read");
        assert_eq!(
            page.entries.len(),
            2,
            "cursor {cursor:?} must restart the pass rather than skip directories"
        );
        assert_eq!(page.next_cursor, None);
    }
}

/// Cancellation is a typed page result and must prevent both inspection and
/// collection; it is not an empty successful census.
#[tokio::test]
async fn unregistered_store_sweep_returns_cancelled_without_mutation() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    std::fs::create_dir_all(profile_root.join("projects/proj_cancelled")).unwrap();
    let (_runtime, db) = open_registered_db(&profile_root).await;
    let cancellation = CancellationToken::new();
    cancellation.cancel();

    let report = sweep_unregistered_store_page(
        &db,
        &profile_root,
        UnregisteredStoreSweepRequestV1 {
            cursor: None,
            limit: 1,
            retention_secs: 0,
            now: 1_700_000_000,
            apply: true,
            cancellation: &cancellation,
            deadline: MonotonicDeadline::at(Instant::now() + Duration::from_secs(1)),
        },
    )
    .await
    .unwrap();

    assert_eq!(report.completion, UnregisteredSweepCompletionV1::Cancelled);
    assert!(report.plan.collect.is_empty());
    assert!(report.outcome.collected.is_empty());
    assert!(profile_root.join("projects/proj_cancelled").is_dir());
}

/// A deadline is distinct from cancellation and must also leave the current
/// page untouched. It is surfaced to the maintenance coordinator so it does
/// not checkpoint partial unregistered work as successful progress.
#[tokio::test]
async fn unregistered_store_sweep_returns_deadline_without_mutation() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    std::fs::create_dir_all(profile_root.join("projects/proj_deadline")).unwrap();
    let (_runtime, db) = open_registered_db(&profile_root).await;
    let cancellation = CancellationToken::new();

    let report = sweep_unregistered_store_page(
        &db,
        &profile_root,
        UnregisteredStoreSweepRequestV1 {
            cursor: None,
            limit: 1,
            retention_secs: 0,
            now: 1_700_000_000,
            apply: true,
            cancellation: &cancellation,
            deadline: MonotonicDeadline::at(Instant::now()),
        },
    )
    .await
    .unwrap();

    assert_eq!(
        report.completion,
        UnregisteredSweepCompletionV1::DeadlineExceeded
    );
    assert!(report.plan.collect.is_empty());
    assert!(report.outcome.collected.is_empty());
    assert!(profile_root.join("projects/proj_deadline").is_dir());
}

/// The mounted unregistered pager recognizes a durable quarantine even though
/// it is not a valid `project_id` leaf. It restores the bytes, emits a typed
/// receipt, and deliberately defers deletion until a later fresh census.
#[tokio::test]
async fn unregistered_store_sweep_reconciles_interrupted_quarantine() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    let projects = profile_root.join("projects");
    std::fs::create_dir_all(&projects).unwrap();
    let quarantine = projects.join(".tracedecay-orphan-quarantine-proj_paged_recovery-42-7");
    std::fs::create_dir_all(&quarantine).unwrap();
    std::fs::write(quarantine.join("payload.bin"), b"recover through pager").unwrap();
    let (_runtime, db) = open_registered_db(&profile_root).await;
    let cancellation = CancellationToken::new();

    let report = sweep_unregistered_store_page(
        &db,
        &profile_root,
        UnregisteredStoreSweepRequestV1 {
            cursor: None,
            limit: 1,
            retention_secs: 0,
            now: 1_700_000_000,
            apply: true,
            cancellation: &cancellation,
            deadline: MonotonicDeadline::at(Instant::now() + Duration::from_secs(1)),
        },
    )
    .await
    .unwrap();

    let restored = projects.join("proj_paged_recovery");
    assert_eq!(report.completion, UnregisteredSweepCompletionV1::Complete);
    assert!(report.outcome.collected.is_empty());
    assert_eq!(report.outcome.recovery_receipts.len(), 1);
    assert_eq!(
        std::fs::read(restored.join("payload.bin")).unwrap(),
        b"recover through pager"
    );
    assert!(!quarantine.exists());
}

#[tokio::test]
async fn unregistered_store_sweep_reports_failed_legacy_restore() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    let projects = profile_root.join("projects");
    let data_root = projects.join("proj_paged_retained");
    let quarantine = projects.join(".tracedecay-orphan-quarantine-proj_paged_retained-42-7");
    std::fs::create_dir_all(&data_root).unwrap();
    std::fs::write(data_root.join("payload.bin"), b"new live bytes").unwrap();
    std::fs::create_dir_all(&quarantine).unwrap();
    std::fs::write(quarantine.join("payload.bin"), b"legacy quarantine bytes").unwrap();
    let (_runtime, db) = open_registered_db(&profile_root).await;
    let cancellation = CancellationToken::new();

    let report = sweep_unregistered_store_page(
        &db,
        &profile_root,
        UnregisteredStoreSweepRequestV1 {
            cursor: None,
            limit: 2,
            retention_secs: 0,
            now: 1_700_000_000,
            apply: true,
            cancellation: &cancellation,
            deadline: MonotonicDeadline::at(Instant::now() + Duration::from_secs(1)),
        },
    )
    .await
    .unwrap();

    let failure = CollectionMutationFailure {
        operation: CollectionMutationOperation::RestoreLiveLeafFromQuarantine,
        raw_os_error: Some(OCCUPIED_RENAME_RAW_OS_ERROR),
        target_path: data_root.clone(),
        expected_root_identity: None,
        classification: CollectionMutationFailureClassification::NonRetryable,
    };
    assert_eq!(
        report.outcome.errors,
        vec![
            CollectionFailure {
                store_id: "proj_paged_retained".to_owned(),
                kind: CollectionFailureKind::RemoveFailed(failure),
            },
            CollectionFailure {
                store_id: "proj_paged_retained".to_owned(),
                kind: CollectionFailureKind::PayloadChanged,
            },
        ]
    );
    assert_eq!(
        report.outcome.recovery_receipts,
        vec![CollectionRecoveryReceipt {
            store_id: "proj_paged_retained".to_owned(),
            original_path: data_root.clone(),
            quarantine_path: quarantine.clone(),
            actual_path: quarantine.clone(),
            action: CollectionRecoveryAction::RetainedForRecovery,
        }]
    );
    assert_eq!(
        std::fs::read(data_root.join("payload.bin")).unwrap(),
        b"new live bytes"
    );
    assert_eq!(
        std::fs::read(quarantine.join("payload.bin")).unwrap(),
        b"legacy quarantine bytes"
    );
}

/// A durable-memory guard applies to unregistered directories exactly as it
/// does to registered orphan stores.
#[tokio::test]
async fn sweep_unregistered_stores_never_deletes_durable_memory_rows() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let (_runtime, db) = open_registered_db(&profile_root).await;

    let base = 1_700_000_000i64;
    let dir = profile_root.join("projects").join("proj_ghost_with_memory");
    std::fs::create_dir_all(&dir).unwrap();
    {
        let connection =
            rusqlite::Connection::open(dir.join(tracedecay_runtime_core::config::DB_FILENAME))
                .unwrap();
        connection
            .execute_batch(
                "CREATE TABLE memory_facts (fact_id INTEGER PRIMARY KEY, content TEXT NOT NULL);
                 INSERT INTO memory_facts (fact_id, content) VALUES (1, 'durable fact');",
            )
            .unwrap();
    }
    filetime::set_file_mtime(
        dir.join(tracedecay_runtime_core::config::DB_FILENAME),
        filetime::FileTime::from_unix_time(base - 100 * DAY, 0),
    )
    .unwrap();

    let report = sweep_unregistered_stores(&db, &profile_root, 7 * DAY, base, true)
        .await
        .unwrap();
    assert!(report.outcome.collected.is_empty());
    assert_eq!(
        report.outcome.errors[0].kind,
        CollectionFailureKind::DurableDataProtected
    );
    assert!(dir.exists());
}

/// The durable-data check covers the manifest-selected project graph and every
/// registered project graph scope, and refuses to answer when the manifest
/// that names them cannot be read.
mod durable_inventory {
    use super::*;

    fn manifest_bytes(graph_db_relpath: &str) -> Vec<u8> {
        let project_root = PathBuf::from("/definitely/not/here/gone");
        let manifest = StoreManifest {
            schema_version: STORE_MANIFEST_SCHEMA_VERSION,
            project_id: Some("proj_inventory".to_string()),
            store_kind: StoreKind::CodeProject,
            storage_mode: StorageMode::ProfileSharded,
            project_root: project_root.clone(),
            data_root: project_root,
            graph_db_relpath: PathBuf::from(graph_db_relpath),
            sessions_db_relpath: PathBuf::from("sessions.db"),
            branch_meta_relpath: PathBuf::from(
                tracedecay_runtime_core::storage::BRANCH_META_FILENAME,
            ),
        };
        serde_json::to_vec(&manifest).unwrap()
    }

    #[test]
    fn registered_graph_scopes_at_custom_paths_are_covered() {
        let store = tempfile::tempdir().unwrap();
        let custom = PathBuf::from("scopes/custom-scope.db");

        let DurableDatabaseInventoryV1::Resolved(inventory) = durable_database_inventory(
            store.path(),
            Some(&manifest_bytes("custom-main.db")),
            std::slice::from_ref(&custom),
            unbounded_collection_control(),
        ) else {
            panic!("a readable manifest must resolve an inventory");
        };

        assert!(
            inventory.contains(&PathBuf::from("custom-main.db")),
            "the manifest's custom main graph path must be honoured, not the default filename"
        );
        assert!(inventory.contains(&custom));
    }

    #[test]
    fn branch_databases_are_part_of_the_inventory() {
        let store = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(store.path().join("branches")).unwrap();
        std::fs::write(store.path().join("branches/feature-x.db"), b"").unwrap();
        std::fs::write(store.path().join("branches/main.db"), b"").unwrap();
        std::fs::write(store.path().join("branches/notes.txt"), b"").unwrap();

        let DurableDatabaseInventoryV1::Resolved(inventory) = durable_database_inventory(
            store.path(),
            Some(&manifest_bytes("code.db")),
            &[],
            unbounded_collection_control(),
        ) else {
            panic!("a readable manifest must resolve an inventory");
        };

        assert!(
            inventory.contains(&PathBuf::from("branches/feature-x.db")),
            "a branch database can hold the only surviving durable rows"
        );
        assert!(inventory.contains(&PathBuf::from("branches/main.db")));
        assert!(!inventory.contains(&PathBuf::from("branches/notes.txt")));
    }

    #[test]
    fn a_missing_manifest_fails_closed() {
        let store = tempfile::tempdir().unwrap();
        assert_eq!(
            durable_database_inventory(store.path(), None, &[], unbounded_collection_control()),
            DurableDatabaseInventoryV1::Unverifiable,
            "without a manifest the store's graph path is a guess, not a fact"
        );
    }

    #[test]
    fn a_malformed_manifest_fails_closed() {
        let store = tempfile::tempdir().unwrap();
        assert_eq!(
            durable_database_inventory(
                store.path(),
                Some(b"{ not json"),
                &[],
                unbounded_collection_control(),
            ),
            DurableDatabaseInventoryV1::Unverifiable
        );
    }

    #[test]
    fn manifest_graph_path_must_be_normalized_relative() {
        for graph_path in [PathBuf::from(""), PathBuf::from("../graph.db")] {
            let bytes = manifest_bytes(graph_path.to_string_lossy().as_ref());
            assert_eq!(
                durable_database_inventory(
                    Path::new("/tmp/store"),
                    Some(&bytes),
                    &[],
                    unbounded_collection_control(),
                ),
                DurableDatabaseInventoryV1::Unverifiable,
                "graph path {graph_path:?} must not escape the store"
            );
        }

        assert_eq!(
            durable_database_inventory(
                Path::new("/tmp/store"),
                Some(&manifest_bytes("/tmp/graph.db")),
                &[],
                unbounded_collection_control(),
            ),
            DurableDatabaseInventoryV1::Unverifiable,
            "an absolute graph path must not replace the store root"
        );
    }

    #[test]
    fn registered_graph_scope_path_must_be_normalized_relative() {
        assert_eq!(
            durable_database_inventory(
                Path::new("/tmp/store"),
                Some(&manifest_bytes("graph.db")),
                &[PathBuf::from("scopes/../../escape.db")],
                unbounded_collection_control(),
            ),
            DurableDatabaseInventoryV1::Unverifiable
        );
        assert_eq!(
            durable_database_inventory(
                Path::new("/tmp/store"),
                Some(&manifest_bytes("graph.db")),
                &[PathBuf::from("/tmp/escape.db")],
                unbounded_collection_control(),
            ),
            DurableDatabaseInventoryV1::Unverifiable
        );
    }

    #[test]
    fn cancelled_control_interrupts_branch_database_inventory() {
        let store = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(store.path().join("branches")).unwrap();
        std::fs::write(store.path().join("branches/only-memory.db"), b"").unwrap();
        let cancellation = CancellationToken::new();
        cancellation.cancel();

        assert_eq!(
            durable_database_inventory(
                store.path(),
                Some(&manifest_bytes("graph.db")),
                &[],
                CollectionControl::new(
                    &cancellation,
                    MonotonicDeadline::at(Instant::now() + Duration::from_secs(1)),
                ),
            ),
            DurableDatabaseInventoryV1::Interrupted,
            "a cancelled admission must not finish the lazy branch scan"
        );
    }

    #[tokio::test]
    async fn a_store_whose_manifest_is_unreadable_is_never_reported_empty() {
        let profile = tempfile::tempdir().unwrap();
        let data_root = profile.path().join("stores/unreadable");
        std::fs::create_dir_all(&data_root).unwrap();

        let check = check_store_durable_memory(
            &data_root,
            Some(b"{ not json"),
            &[],
            &durable_check_scratch_root(profile.path()),
            unbounded_collection_control(),
        )
        .await;

        assert_eq!(
            check,
            DurableMemoryCheck::Unverifiable,
            "an unverifiable inventory must protect the store, never clear it for deletion"
        );
    }

    #[tokio::test]
    async fn a_pre_cancelled_durable_snapshot_reports_interrupted() {
        let profile = tempfile::tempdir().unwrap();
        let data_root = profile.path().join("stores/cancelled");
        std::fs::create_dir_all(&data_root).unwrap();
        rusqlite::Connection::open(data_root.join("graph.db")).unwrap();

        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let check = check_store_durable_memory(
            &data_root,
            Some(&manifest_bytes("graph.db")),
            &[],
            &durable_check_scratch_root(profile.path()),
            CollectionControl::new(
                &cancellation,
                MonotonicDeadline::at(Instant::now() + Duration::from_secs(1)),
            ),
        )
        .await;

        assert_eq!(
            check,
            DurableMemoryCheck::Interrupted,
            "a pre-cancelled durable snapshot must not report an empty database"
        );
    }
}

#[cfg(unix)]
#[tokio::test]
async fn symlink_manifest_is_unverifiable_and_never_collected() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let dead_root = tmp.path().join("moved-away-repo");
    let (_runtime, db) = open_registered_db(&profile_root).await;
    let data_root = seed_store(
        &db,
        &profile_root,
        "proj_symlink_manifest",
        "store_symlink_manifest",
        &dead_root,
        1_700_000_000 - 100 * DAY,
    )
    .await;
    let manifest_path = data_root.join(tracedecay_runtime_core::storage::STORE_MANIFEST_FILENAME);
    let target = tmp.path().join("manifest-target.json");
    std::fs::copy(&manifest_path, &target).unwrap();
    std::fs::remove_file(&manifest_path).unwrap();
    std::os::unix::fs::symlink(&target, &manifest_path).unwrap();

    let report = sweep_orphan_stores(&db, &profile_root, 7 * DAY, 1_700_000_000, true)
        .await
        .unwrap();

    assert!(report.plan.collect.is_empty());
    assert_eq!(report.plan.unverifiable.len(), 1);
    assert!(report.outcome.collected.is_empty());
    assert!(data_root.exists());
}

#[cfg(unix)]
#[tokio::test]
async fn symlink_graph_database_is_durable_data_protected() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let dead_root = tmp.path().join("moved-away-repo");
    let (_runtime, db) = open_registered_db(&profile_root).await;
    let data_root = seed_store(
        &db,
        &profile_root,
        "proj_symlink_graph",
        "store_symlink_graph",
        &dead_root,
        1_700_000_000 - 100 * DAY,
    )
    .await;
    let graph_path = data_root.join("graph.db");
    let target = tmp.path().join("graph-target.db");
    rusqlite::Connection::open(&target).unwrap();
    std::fs::remove_file(&graph_path).unwrap();
    std::os::unix::fs::symlink(&target, &graph_path).unwrap();

    let report = sweep_orphan_stores(&db, &profile_root, 7 * DAY, 1_700_000_000, true)
        .await
        .unwrap();

    assert_eq!(report.plan.collect.len(), 1);
    assert!(report.outcome.collected.is_empty());
    assert_eq!(report.outcome.errors.len(), 1);
    assert_eq!(
        report.outcome.errors[0].kind,
        CollectionFailureKind::DurableDataProtected
    );
    assert!(data_root.exists());
}

#[cfg(unix)]
#[tokio::test]
async fn symlink_branch_database_is_durable_data_protected() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let dead_root = tmp.path().join("moved-away-repo");
    let (_runtime, db) = open_registered_db(&profile_root).await;
    let data_root = seed_store(
        &db,
        &profile_root,
        "proj_symlink_branch",
        "store_symlink_branch",
        &dead_root,
        1_700_000_000 - 100 * DAY,
    )
    .await;
    let branches = data_root.join("branches");
    std::fs::create_dir_all(&branches).unwrap();
    let target = tmp.path().join("branch-target.db");
    rusqlite::Connection::open(&target).unwrap();
    std::os::unix::fs::symlink(&target, branches.join("feature.db")).unwrap();

    let report = sweep_orphan_stores(&db, &profile_root, 7 * DAY, 1_700_000_000, true)
        .await
        .unwrap();

    assert_eq!(report.plan.collect.len(), 1);
    assert!(report.outcome.collected.is_empty());
    assert_eq!(report.outcome.errors.len(), 1);
    assert_eq!(
        report.outcome.errors[0].kind,
        CollectionFailureKind::DurableDataProtected
    );
    assert!(data_root.exists());
}
