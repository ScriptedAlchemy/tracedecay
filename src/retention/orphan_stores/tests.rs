use std::path::{Path, PathBuf};

use super::*;
use crate::global_db::{GlobalDb, StoreInstanceUpsert};
use crate::storage::{STORE_MANIFEST_SCHEMA_VERSION, StorageMode, StoreKind, StoreManifest};

const DAY: i64 = 24 * 60 * 60;

fn entry(
    store_id: &str,
    canonical_root: PathBuf,
    display_root: Option<PathBuf>,
    manifest_root: Option<PathBuf>,
    data_root: PathBuf,
    last_write_secs: i64,
    size_bytes: u64,
) -> StoreCensusEntry {
    StoreCensusEntry {
        project_id: format!("proj_{store_id}"),
        store_id: store_id.to_string(),
        canonical_root,
        display_root,
        data_root,
        manifest_root,
        last_write_secs,
        size_bytes,
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
        }],
        retained_immature: vec![OrphanStoreFinding {
            project_id: "proj_keep".into(),
            store_id: "keep".into(),
            data_root: keep_dir.clone(),
            disposition: StoreDisposition::Orphaned,
            age_secs: DAY,
            size_bytes: 0,
        }],
        relink: Vec::new(),
    };

    let outcome = execute_collection(&plan);
    assert_eq!(outcome.collected.len(), 1);
    assert_eq!(outcome.reclaimed_bytes, 7);
    assert!(outcome.errors.is_empty());
    assert!(!collect_dir.exists(), "collected store must be removed");
    assert!(keep_dir.exists(), "immature store must be untouched");
}

#[test]
fn already_missing_directory_collects_idempotently() {
    let plan = CollectionPlan {
        collect: vec![OrphanStoreFinding {
            project_id: "proj_gone".into(),
            store_id: "gone".into(),
            data_root: PathBuf::from("/definitely/not/here/store"),
            disposition: StoreDisposition::Orphaned,
            age_secs: 90 * DAY,
            size_bytes: 42,
        }],
        ..CollectionPlan::default()
    };
    let outcome = execute_collection(&plan);
    assert_eq!(outcome.collected.len(), 1);
    assert!(outcome.errors.is_empty());
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

    let db = GlobalDb::open_at(&profile_root.join("global.db"))
        .await
        .unwrap();

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
    let report = sweep_orphan_stores(&db, &profile_root, 7 * DAY, now, false).await;
    assert_eq!(report.plan.collect.len(), 1, "one orphan should be planned");
    assert_eq!(report.plan.collect[0].store_id, "store_orphan");
    assert!(!report.applied);
    assert!(orphan_data_root.exists(), "dry run must not delete");

    // Apply: orphan store removed, row retired, live store untouched.
    let report = sweep_orphan_stores(&db, &profile_root, 7 * DAY, now, true).await;
    assert!(report.applied);
    assert_eq!(report.outcome.collected.len(), 1);
    assert_eq!(report.retired_registry_rows, 1);
    assert!(!orphan_data_root.exists(), "orphan store must be collected");
    assert!(live_root.exists());

    let live_data_root = profile_root.join("stores/store_live");
    assert!(live_data_root.exists(), "live store must be untouched");

    let remaining: Vec<_> = db
        .list_code_projects(usize::MAX)
        .await
        .into_iter()
        .map(|p| p.project_id)
        .collect();
    assert!(remaining.contains(&"proj_live".to_string()));
    assert!(
        !remaining.contains(&"proj_orphan".to_string()),
        "orphan identity row must be retired"
    );
}

/// Register a profile-sharded store and write its manifest + a payload file.
/// Returns the on-disk data root. The manifest `project_root` matches the
/// registry root so a dead root is a true orphan (not a re-link candidate).
async fn seed_store(
    db: &GlobalDb,
    profile_root: &Path,
    project_id: &str,
    store_id: &str,
    project_root: &Path,
    created_at: i64,
) -> PathBuf {
    let data_root = profile_root.join("stores").join(store_id);
    std::fs::create_dir_all(&data_root).unwrap();
    std::fs::write(data_root.join("graph.db"), b"payload-bytes").unwrap();

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

    db.upsert_code_project(project_id, project_root, None, None, None)
        .await
        .unwrap();
    db.upsert_store_instance(StoreInstanceUpsert {
        store_id: store_id.to_string(),
        project_id: project_id.to_string(),
        store_kind: "project".to_string(),
        storage_mode: "profile_sharded".to_string(),
        store_relpath: format!("stores/{store_id}"),
        manifest_relpath: None,
        last_verified_at: None,
        last_write_at: Some(created_at),
    })
    .await
    .unwrap();
    data_root
}
