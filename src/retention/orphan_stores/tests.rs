use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use super::*;
use crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1;
use crate::db::DaemonDatabaseScope;
use crate::global_db::RegisteredGlobalDb;
use crate::storage::{STORE_MANIFEST_SCHEMA_VERSION, StorageMode, StoreKind, StoreManifest};

const DAY: i64 = 24 * 60 * 60;
static TEST_RUNTIME_NONCE: AtomicU64 = AtomicU64::new(1);

async fn open_registered_db(
    profile_root: &Path,
) -> (
    DaemonSessionRuntimeRegistryV1,
    DaemonDatabaseScope,
    Arc<RegisteredGlobalDb>,
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
    let identity = crate::daemon::profile_identity::load_or_create(profile_root).unwrap();
    let nonce = TEST_RUNTIME_NONCE.fetch_add(1, Ordering::Relaxed);
    let scope =
        crate::db::enter_daemon_database_scope(profile_root, nonce, "orphan store sweep test")
            .unwrap();
    let registry = DaemonSessionRuntimeRegistryV1::open(identity)
        .await
        .unwrap();
    let database = registry.profile_database().await.unwrap();
    (registry, scope, database)
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
        data_root.join(crate::storage::STORE_MANIFEST_FILENAME),
        b"{ this is not valid manifest json",
    )
    .unwrap();

    // Exercise the same parse the census performs, so the fixture proves the
    // production decode path — not a hand-set flag — yields unverifiable.
    let bytes = std::fs::read(data_root.join(crate::storage::STORE_MANIFEST_FILENAME)).unwrap();
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
            Err(_) => {
                outcome.errors.push(CollectionFailure {
                    store_id: finding.store_id.clone(),
                    kind: CollectionFailureKind::RemoveFailed,
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

    let (_registry, _scope, db) = open_registered_db(&profile_root).await;

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

#[tokio::test]
async fn sweep_preserves_immature_sibling_store_identity() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let dead_root = tmp.path().join("moved-away-repo");
    let (_registry, _scope, db) = open_registered_db(&profile_root).await;
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
    let (_registry, _scope, db) = open_registered_db(&profile_root).await;
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
        branch_meta_relpath: PathBuf::from(crate::storage::BRANCH_META_FILENAME),
    };
    std::fs::write(
        store_root.join(crate::storage::STORE_MANIFEST_FILENAME),
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
    let relinked_manifest = crate::storage::read_store_manifest(
        &store_root.join(crate::storage::STORE_MANIFEST_FILENAME),
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
    let (_registry, _scope, db) = open_registered_db(&profile_root).await;
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
    let mut manifest = crate::storage::read_store_manifest(
        &store_root.join(crate::storage::STORE_MANIFEST_FILENAME),
    )
    .unwrap();
    manifest.project_id = Some("proj_live".to_string());
    manifest.project_root = live_root;
    crate::storage::write_store_manifest_to_path(
        &store_root.join(crate::storage::STORE_MANIFEST_FILENAME),
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
    let (_registry, _scope, db) = open_registered_db(&profile_root).await;
    let store_root = seed_store(
        &db,
        &profile_root,
        "proj_old",
        "store_moved",
        &dead_root,
        1_700_000_000,
    )
    .await;
    let mut manifest = crate::storage::read_store_manifest(
        &store_root.join(crate::storage::STORE_MANIFEST_FILENAME),
    )
    .unwrap();
    manifest.project_root = unregistered_live_root;
    std::fs::write(
        store_root.join(crate::storage::STORE_MANIFEST_FILENAME),
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
    let unchanged_manifest = crate::storage::read_store_manifest(
        &store_root.join(crate::storage::STORE_MANIFEST_FILENAME),
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
    let (_registry, _scope, db) = open_registered_db(&profile_root).await;
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
    let mut manifest = crate::storage::read_store_manifest(
        &store_root.join(crate::storage::STORE_MANIFEST_FILENAME),
    )
    .unwrap();
    manifest.project_root = live_root;
    std::fs::write(
        store_root.join(crate::storage::STORE_MANIFEST_FILENAME),
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
    let restored_manifest = crate::storage::read_store_manifest(
        &store_root.join(crate::storage::STORE_MANIFEST_FILENAME),
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
        branch_meta_relpath: PathBuf::from(crate::storage::BRANCH_META_FILENAME),
    };
    std::fs::write(
        data_root.join(crate::storage::STORE_MANIFEST_FILENAME),
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
            crate::db::engine::params![
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
            crate::db::engine::params![project_id, root.as_str(), timestamp],
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
            crate::db::engine::params![root, project_id, timestamp],
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
    let (_registry, _scope, db) = open_registered_db(&profile_root).await;
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
    let (_registry, _scope, db) = open_registered_db(&profile_root).await;
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
    let (_registry, _scope, db) = open_registered_db(&profile_root).await;
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
    let (_registry, _scope, db) = open_registered_db(&profile_root).await;
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
    let (_registry, _scope, db) = open_registered_db(&profile_root).await;

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
async fn sweep_unregistered_stores_collects_past_retention_and_retains_young() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let (_registry, _scope, db) = open_registered_db(&profile_root).await;

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
    assert_eq!(report.outcome.collected.len(), 1);
    assert!(
        !old_dir.exists(),
        "past-retention unregistered dir must be collected"
    );
    assert!(
        young_dir.exists(),
        "immature unregistered dir must be retained"
    );
}

/// A registered project id must never be treated as an unregistered
/// candidate, and re-registering between census and collection must abort
/// the delete for that finding (closing the revival window).
#[tokio::test]
async fn sweep_unregistered_stores_aborts_when_directory_gets_registered_first() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let (_registry, _scope, db) = open_registered_db(&profile_root).await;

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

/// A durable-memory guard applies to unregistered directories exactly as it
/// does to registered orphan stores.
#[tokio::test]
async fn sweep_unregistered_stores_never_deletes_durable_memory_rows() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let (_registry, _scope, db) = open_registered_db(&profile_root).await;

    let base = 1_700_000_000i64;
    let dir = profile_root.join("projects").join("proj_ghost_with_memory");
    std::fs::create_dir_all(&dir).unwrap();
    {
        let connection = rusqlite::Connection::open(dir.join(crate::config::DB_FILENAME)).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE memory_facts (fact_id INTEGER PRIMARY KEY, content TEXT NOT NULL);
                 INSERT INTO memory_facts (fact_id, content) VALUES (1, 'durable fact');",
            )
            .unwrap();
    }
    filetime::set_file_mtime(
        dir.join(crate::config::DB_FILENAME),
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

/// A store is not one database. The durable-data check has to cover the
/// manifest-selected main graph, every registered graph scope, and the branch
/// databases discovered on disk — and refuse to answer at all when the
/// manifest that names them cannot be read.
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
            branch_meta_relpath: PathBuf::from(crate::storage::BRANCH_META_FILENAME),
        };
        serde_json::to_vec(&manifest).unwrap()
    }

    #[test]
    fn branch_databases_are_part_of_the_inventory() {
        let store = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(store.path().join("branches")).unwrap();
        std::fs::write(store.path().join("branches/feature-x.db"), b"").unwrap();
        std::fs::write(store.path().join("branches/main.db"), b"").unwrap();
        // Non-database debris must not enter the inventory.
        std::fs::write(store.path().join("branches/notes.txt"), b"").unwrap();

        let DurableDatabaseInventoryV1::Resolved(inventory) = durable_database_inventory(
            store.path(),
            Some(&manifest_bytes(crate::config::DB_FILENAME)),
            &[],
        ) else {
            panic!("a readable manifest must resolve an inventory");
        };

        assert!(inventory.contains(&PathBuf::from(crate::config::DB_FILENAME)));
        assert!(inventory.contains(&PathBuf::from("branches/feature-x.db")));
        assert!(inventory.contains(&PathBuf::from("branches/main.db")));
        assert!(
            !inventory.contains(&PathBuf::from("branches/notes.txt")),
            "only databases belong in the durable inventory"
        );
    }

    #[test]
    fn registered_graph_scopes_at_custom_paths_are_covered() {
        let store = tempfile::tempdir().unwrap();
        let custom = PathBuf::from("scopes/custom-scope.db");

        let DurableDatabaseInventoryV1::Resolved(inventory) = durable_database_inventory(
            store.path(),
            Some(&manifest_bytes("custom-main.db")),
            std::slice::from_ref(&custom),
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
    fn a_missing_manifest_fails_closed() {
        let store = tempfile::tempdir().unwrap();
        assert_eq!(
            durable_database_inventory(store.path(), None, &[]),
            DurableDatabaseInventoryV1::Unverifiable,
            "without a manifest the store's graph path is a guess, not a fact"
        );
    }

    #[test]
    fn a_malformed_manifest_fails_closed() {
        let store = tempfile::tempdir().unwrap();
        assert_eq!(
            durable_database_inventory(store.path(), Some(b"{ not json"), &[]),
            DurableDatabaseInventoryV1::Unverifiable
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
        )
        .await;

        assert_eq!(
            check,
            DurableMemoryCheck::Unverifiable,
            "an unverifiable inventory must protect the store, never clear it for deletion"
        );
    }
}
