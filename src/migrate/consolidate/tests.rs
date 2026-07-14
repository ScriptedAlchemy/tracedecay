use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

use serde_json::json;
use tempfile::TempDir;

use super::*;
use crate::db::{Database, DatabaseAuthority};
use crate::memory::store::MemoryStore;
use crate::memory::types::{
    AddFactRequest, FactRelationKind, FeedbackAction, FeedbackRequest, MemoryCategory,
};
use crate::sessions::{SessionMessageRecord, SessionRecord};
use crate::tracedecay::{TraceDecay, TraceDecayOpenOptions};

async fn test_initialize(path: &Path) -> (Database, bool) {
    let authority = DatabaseAuthority::acquire_test(path, "consolidation test initialize").unwrap();
    Database::initialize(path, &authority).await.unwrap()
}

async fn test_open(path: &Path) -> (Database, bool) {
    let authority = DatabaseAuthority::acquire_test(path, "consolidation test open").unwrap();
    Database::open(path, &authority).await.unwrap()
}

async fn test_open_read_only(path: &Path) -> (Database, bool) {
    let authority = DatabaseAuthority::acquire_test(path, "consolidation test read").unwrap();
    Database::open_read_only(path, &authority).await.unwrap()
}

struct Fixture {
    _temp: TempDir,
    project: PathBuf,
    profile: PathBuf,
    source_id: String,
    target_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SnapshotEntry {
    Missing,
    File {
        digest: [u8; 32],
        bytes: u64,
        modified: SystemTime,
        #[cfg(unix)]
        device: u64,
        #[cfg(unix)]
        inode: u64,
        #[cfg(unix)]
        changed_seconds: i64,
        #[cfg(unix)]
        changed_nanoseconds: i64,
        #[cfg(unix)]
        links: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TreeSnapshotEntry {
    // Directory timestamps are derived state: creating and removing ignored
    // authority-lock artifacts changes their parent directories' mtime/ctime.
    // Topology, identity, permissions, and every non-ignored child remain
    // snapshotted, so persistent input mutations are still detected.
    Directory {
        #[cfg(unix)]
        device: u64,
        #[cfg(unix)]
        inode: u64,
        #[cfg(unix)]
        mode: u32,
    },
    File(SnapshotEntry),
}

fn migration_surface_snapshot(fixture: &Fixture) -> BTreeMap<PathBuf, SnapshotEntry> {
    let mut snapshot = BTreeMap::new();
    for root in [
        fixture.profile.join("projects").join(&fixture.source_id),
        fixture.profile.join("projects").join(&fixture.target_id),
    ] {
        for path in relative_file_map(&root).unwrap().into_values() {
            snapshot_file(&path, &mut snapshot);
        }
    }
    let global = fixture.profile.join("global.db");
    for path in [
        storage::enrollment_marker_path(&fixture.project),
        storage::repository_identity_path(&fixture.project).unwrap(),
        global.clone(),
        sqlite_sidecar(&global, "-wal"),
        sqlite_sidecar(&global, "-shm"),
    ] {
        snapshot_file(&path, &mut snapshot);
    }
    snapshot
}

fn snapshot_file(path: &Path, snapshot: &mut BTreeMap<PathBuf, SnapshotEntry>) {
    let entry = if path.is_file() {
        let metadata = fs::metadata(path).unwrap();
        SnapshotEntry::File {
            digest: file_digest(path).unwrap(),
            bytes: metadata.len(),
            modified: metadata.modified().unwrap(),
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
            #[cfg(unix)]
            changed_seconds: metadata.ctime(),
            #[cfg(unix)]
            changed_nanoseconds: metadata.ctime_nsec(),
            #[cfg(unix)]
            links: metadata.nlink(),
        }
    } else {
        SnapshotEntry::Missing
    };
    snapshot.insert(path.to_path_buf(), entry);
}

fn full_tree_snapshot(root: &Path) -> BTreeMap<PathBuf, TreeSnapshotEntry> {
    let mut snapshot = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        let metadata = fs::symlink_metadata(&path).unwrap();
        let relative = path.strip_prefix(root).unwrap().to_path_buf();
        let is_database_authority_artifact = relative.components().any(|component| {
            component.as_os_str() == std::ffi::OsStr::new(".tracedecay-database-locks")
        }) || relative.file_name().is_some_and(|name| {
            let name = name.to_string_lossy();
            name == "lifecycle.lock"
                || name == "lifecycle.lock.owner"
                || name.ends_with(".access.lock")
                || name.ends_with(".writer.lock")
                || name.ends_with(".writer.owner")
        });
        if is_database_authority_artifact {
            continue;
        }
        if metadata.is_dir() {
            #[cfg(unix)]
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            snapshot.insert(
                relative,
                TreeSnapshotEntry::Directory {
                    #[cfg(unix)]
                    device: metadata.dev(),
                    #[cfg(unix)]
                    inode: metadata.ino(),
                    #[cfg(unix)]
                    mode: metadata.permissions().mode(),
                },
            );
            let mut children = fs::read_dir(path)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .collect::<Vec<_>>();
            children.sort();
            pending.extend(children);
        } else {
            let mut file = BTreeMap::new();
            snapshot_file(&path, &mut file);
            snapshot.insert(
                relative,
                TreeSnapshotEntry::File(file.remove(&path).unwrap()),
            );
        }
    }
    snapshot
}

impl Fixture {
    fn options(&self) -> ConsolidationOptions {
        ConsolidationOptions {
            project_root: self.project.clone(),
            profile_root: self.profile.clone(),
            source_project_id: self.source_id.clone(),
            target_project_id: self.target_id.clone(),
        }
    }
}

fn input_manifest_paths(
    fixture: &Fixture,
    project_id: &str,
    destination_project_id: &str,
) -> (PathBuf, PathBuf) {
    let root = fixture.profile.join("projects").join(project_id);
    (
        root.join(storage::STORE_MANIFEST_FILENAME),
        root.join(format!(
            "store_manifest.consolidated-into-{destination_project_id}.json"
        )),
    )
}

#[tokio::test]
async fn dry_run_reports_live_split_shape_without_mutation() {
    let fixture = fixture().await;
    let before = migration_surface_snapshot(&fixture);
    let profile_before = full_tree_snapshot(&fixture.profile);
    assert!(!fixture.profile.join("lifecycle.lock").exists());
    let report = plan(&fixture.options()).await.unwrap();
    let after = migration_surface_snapshot(&fixture);
    let profile_after = full_tree_snapshot(&fixture.profile);

    assert!(report.dry_run);
    assert_eq!(report.state, ConsolidationState::Planned);
    assert_eq!(report.source.facts, 1);
    assert_eq!(report.source.feedback_events, 1);
    assert_eq!(report.target.facts, 1);
    assert_eq!(report.source.sessions, 1);
    assert_eq!(report.target.sessions, 1);
    assert_eq!(report.source.lcm_raw_messages, 1);
    assert_eq!(report.target.lcm_raw_messages, 1);
    assert!(!report.destination_data_root.exists());
    assert!(!report.backup_root.exists());
    assert!(!report.ledger_path.exists());
    assert_eq!(after, before, "dry-run changed an input or identity file");
    assert_eq!(
        profile_after, profile_before,
        "dry-run changed the profile tree"
    );
    assert!(
        fs::read_dir(fixture.profile.parent().unwrap())
            .unwrap()
            .all(|entry| !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".tracedecay-migration-scratch-")),
        "dry-run left migration scratch state behind"
    );
    assert!(fixture.profile.join("lifecycle.lock").exists());
    assert_eq!(
        storage::read_repository_identity_marker(&fixture.project)
            .unwrap()
            .unwrap()
            .project_id,
        fixture.target_id
    );
}

#[tokio::test]
async fn legacy_single_db_plan_is_read_only_and_apply_preserves_source_graph() {
    let fixture = fixture().await;
    let source = layout_for_id(&fixture.project, &fixture.profile, &fixture.source_id).unwrap();
    fs::remove_file(&source.branch_meta_path).unwrap();
    storage::write_store_manifest(&source).unwrap();
    let source_graph_before = file_digest(&source.graph_db_path).unwrap();
    let profile_before = full_tree_snapshot(&fixture.profile);

    let options = fixture.options();
    let planned = plan(&options).await.unwrap();

    assert_eq!(planned.source.graph_databases, 1);
    assert_eq!(planned.source.branches, 1);
    assert!(!source.branch_meta_path.exists());
    assert_eq!(
        full_tree_snapshot(&fixture.profile),
        profile_before,
        "legacy single-DB planning mutated an input"
    );

    let applied = apply(&options, &planned.confirmation_token).await.unwrap();

    assert!(!source.branch_meta_path.exists());
    assert_eq!(
        file_digest(&source.graph_db_path).unwrap(),
        source_graph_before
    );
    let default_branch = crate::branch::detect_default_branch(&fixture.project)
        .unwrap_or_else(|| "main".to_string());
    let preserved_name = format!("consolidated/{}/{}", fixture.source_id, default_branch);
    let destination_meta = branch_meta::load_branch_meta(&applied.destination_data_root).unwrap();
    let preserved = destination_meta.branches.get(&preserved_name).unwrap();
    assert_eq!(preserved.created_at, "0");
    assert_eq!(preserved.last_synced_at, "0");
    let preserved_path = applied.destination_data_root.join(&preserved.db_file);
    let (db, _) = test_open_read_only(&preserved_path).await;
    let facts = MemoryStore::new(db.conn())
        .list_facts(None, Some(0.0), 100)
        .await
        .unwrap();
    assert!(
        facts
            .iter()
            .any(|fact| fact.content == "legacy durable fact"),
        "the preserved source graph lost its legacy fact"
    );
    db.close();
}

#[tokio::test]
async fn legacy_single_db_retry_after_destination_publish_is_deterministic() {
    let fixture = fixture().await;
    let source = layout_for_id(&fixture.project, &fixture.profile, &fixture.source_id).unwrap();
    fs::remove_file(&source.branch_meta_path).unwrap();
    storage::write_store_manifest(&source).unwrap();
    let options = fixture.options();
    let planned = plan(&options).await.unwrap();

    let interrupted = apply_with_prepare_stop(
        &options,
        &planned.confirmation_token,
        prepare::PrepareStop::Publish,
    )
    .await
    .unwrap_err();
    assert!(
        interrupted.to_string().contains("synthetic interruption"),
        "{interrupted}"
    );

    std::thread::sleep(std::time::Duration::from_secs(1));
    let applied = apply(&options, &planned.confirmation_token).await.unwrap();

    assert_eq!(applied.state, ConsolidationState::Applied);
    let destination_meta = branch_meta::load_branch_meta(&applied.destination_data_root).unwrap();
    let default_branch = crate::branch::detect_default_branch(&fixture.project).unwrap();
    let preserved = &destination_meta.branches
        [&format!("consolidated/{}/{}", fixture.source_id, default_branch)];
    assert_eq!(preserved.created_at, "0");
    assert_eq!(preserved.last_synced_at, "0");
}

#[tokio::test]
async fn target_legacy_single_db_metadata_is_synthesized_without_input_mutation() {
    let fixture = fixture().await;
    let target = layout_for_id(&fixture.project, &fixture.profile, &fixture.target_id).unwrap();
    fs::remove_file(&target.branch_meta_path).unwrap();
    storage::write_store_manifest(&target).unwrap();
    let profile_before = full_tree_snapshot(&fixture.profile);
    let options = fixture.options();

    let planned = plan(&options).await.unwrap();

    assert_eq!(planned.target.branches, 1);
    assert_eq!(full_tree_snapshot(&fixture.profile), profile_before);
    let applied = apply(&options, &planned.confirmation_token).await.unwrap();
    assert!(!target.branch_meta_path.exists());
    let destination_meta = branch_meta::load_branch_meta(&applied.destination_data_root).unwrap();
    let default_branch = crate::branch::detect_default_branch(&fixture.project).unwrap();
    assert_eq!(destination_meta.default_branch, default_branch);
    assert_eq!(destination_meta.branches[&default_branch].created_at, "0");
    assert_eq!(
        destination_meta.branches[&default_branch].last_synced_at,
        "0"
    );
}

#[tokio::test]
async fn synthesized_branch_metadata_change_invalidates_confirmation_token() {
    let fixture = fixture().await;
    let source = layout_for_id(&fixture.project, &fixture.profile, &fixture.source_id).unwrap();
    fs::remove_file(&source.branch_meta_path).unwrap();
    storage::write_store_manifest(&source).unwrap();
    run_git(&fixture.project, &["branch", "-m", "trunk"]);
    let options = fixture.options();

    let planned = plan(&options).await.unwrap();

    run_git(&fixture.project, &["branch", "-m", "release"]);
    let error = apply(&options, &planned.confirmation_token)
        .await
        .unwrap_err();

    assert!(
        error.to_string().contains("confirmation token mismatch"),
        "{error}"
    );
    assert!(!source.branch_meta_path.exists());
    assert!(!planned.destination_data_root.exists());
    assert!(!planned.backup_root.exists());
    assert!(!planned.ledger_path.exists());
}

#[tokio::test]
async fn corrupt_branch_metadata_fails_closed_without_mutation() {
    for content in [
        b"{\"default_branch\":".as_slice(),
        br#"{"default_branch":"main","branches":{}}"#.as_slice(),
        br#"{"default_branch":"main","branches":{"main":{"db_file":"branches/not-main.db","created_at":"0","last_synced_at":"0"}}}"#.as_slice(),
    ] {
        let fixture = fixture().await;
        let source = layout_for_id(&fixture.project, &fixture.profile, &fixture.source_id).unwrap();
        fs::write(&source.branch_meta_path, content).unwrap();
        storage::write_store_manifest(&source).unwrap();
        let profile_before = full_tree_snapshot(&fixture.profile);

        let error = plan(&fixture.options()).await.unwrap_err();

        assert!(
            error.to_string().contains("corrupt branch metadata"),
            "{error}"
        );
        assert_eq!(
            full_tree_snapshot(&fixture.profile),
            profile_before,
            "corrupt metadata planning mutated an input"
        );
    }
}

#[tokio::test]
async fn many_branch_plan_retains_constant_database_handles_and_bounded_scratch() {
    const BRANCHES_PER_SHARD: usize = 48;
    let fixture = fixture().await;
    add_branch_links(&fixture, &fixture.source_id, BRANCHES_PER_SHARD);
    add_branch_links(&fixture, &fixture.target_id, BRANCHES_PER_SHARD);

    let resolved = resolve_plan(&fixture.options()).await.unwrap();
    assert_eq!(
        resolved.report.source.graph_databases,
        BRANCHES_PER_SHARD + 1
    );
    assert_eq!(
        resolved.report.target.graph_databases,
        BRANCHES_PER_SHARD + 1
    );
    assert_eq!(
        resolved.evidence.retained_database_count(),
        2,
        "graph snapshots must be processed and dropped one at a time; only the two session snapshots stay open"
    );

    let max_graph_family = input_database_paths(&resolved)
        .unwrap()
        .into_iter()
        .filter(|path| {
            path.file_name().and_then(|name| name.to_str()) != Some(storage::SESSIONS_DB_FILENAME)
        })
        .map(|path| sqlite_family_bytes(&path))
        .max()
        .unwrap();
    assert!(
        resolved.evidence.peak_graph_scratch_bytes() <= max_graph_family,
        "graph scratch must be bounded by one SQLite family, independent of branch count"
    );
    let session_family_bytes = [
        &resolved.source_layout.sessions_db_path,
        &resolved.target_layout.sessions_db_path,
    ]
    .into_iter()
    .map(|path| sqlite_family_bytes(path))
    .sum::<u64>();
    assert!(
        resolved.evidence.sessions.copied_bytes() <= session_family_bytes,
        "retained session scratch must be bounded by the two input families"
    );
}

#[tokio::test]
async fn interrupted_apply_retries_without_duplicates_and_cuts_over_last() {
    let fixture = fixture().await;
    add_fact_relation_to_shard(&fixture, &fixture.source_id).await;
    let options = fixture.options();
    let source_root = fixture.profile.join("projects").join(&fixture.source_id);
    let target_root = fixture.profile.join("projects").join(&fixture.target_id);
    fs::write(source_root.join(".dirty"), b"interrupted source sync").unwrap();
    fs::write(
        source_root.join("tracedecay.db.corrupt-enospc-source"),
        b"source forensic database",
    )
    .unwrap();
    fs::write(
        target_root.join("tracedecay.db.corrupt-enospc-target"),
        b"target forensic database",
    )
    .unwrap();
    let input_database_digests = [
        source_root.join(crate::config::DB_FILENAME),
        source_root.join(storage::SESSIONS_DB_FILENAME),
        target_root.join(crate::config::DB_FILENAME),
        target_root.join(storage::SESSIONS_DB_FILENAME),
    ]
    .map(|path| (path.clone(), file_digest(&path).unwrap()));
    let report = plan(&options).await.unwrap();

    let error = apply_with_stop(
        &options,
        &report.confirmation_token,
        Some(ConsolidationState::DatabasesMerged),
    )
    .await
    .unwrap_err();
    assert!(
        error.to_string().contains("synthetic interruption"),
        "{error}"
    );
    assert_eq!(
        storage::read_repository_identity_marker(&fixture.project)
            .unwrap()
            .unwrap()
            .project_id,
        fixture.target_id,
        "marker must not move before all data and registry phases succeed"
    );

    let applied = apply(&options, &report.confirmation_token).await.unwrap();
    assert_eq!(applied.state, ConsolidationState::Applied);
    assert!(!applied.dry_run);
    assert_eq!(
        storage::read_repository_identity_marker(&fixture.project)
            .unwrap()
            .unwrap()
            .project_id,
        applied.destination_project_id
    );
    assert_eq!(
        storage::read_enrollment_marker(&fixture.project)
            .unwrap()
            .unwrap()
            .project_id,
        applied.destination_project_id,
        "successful cutover must suppress legacy shard discovery even when no enrollment marker existed"
    );

    let graph = applied
        .destination_data_root
        .join(crate::config::DB_FILENAME);
    let sessions = applied
        .destination_data_root
        .join(storage::SESSIONS_DB_FILENAME);
    assert_eq!(sqlite::count_rows(&graph, "memory_facts").await.unwrap(), 3);
    assert_eq!(
        sqlite::count_rows(&graph, "memory_fact_relations")
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        sqlite::count_rows(&graph, "memory_feedback_events")
            .await
            .unwrap(),
        1
    );
    assert_eq!(sqlite::count_rows(&sessions, "sessions").await.unwrap(), 2);
    assert_eq!(
        sqlite::count_rows(&sessions, "session_messages")
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        sqlite::count_rows(&sessions, "lcm_raw_messages")
            .await
            .unwrap(),
        2
    );
    assert!(
        applied
            .destination_data_root
            .join("lcm-payloads/source.txt")
            .is_file()
    );
    assert!(
        applied
            .destination_data_root
            .join("lcm-payloads/target.txt")
            .is_file()
    );
    assert!(
        applied
            .backup_root
            .join(&fixture.source_id)
            .join(storage::STORE_MANIFEST_FILENAME)
            .is_file()
    );
    assert_eq!(
        fs::read(applied.backup_root.join(&fixture.source_id).join(".dirty")).unwrap(),
        b"interrupted source sync"
    );
    assert_eq!(
        fs::read(
            applied
                .destination_data_root
                .join("tracedecay.db.corrupt-enospc-source")
        )
        .unwrap(),
        b"source forensic database"
    );
    assert_eq!(
        fs::read(
            applied
                .destination_data_root
                .join("tracedecay.db.corrupt-enospc-target")
        )
        .unwrap(),
        b"target forensic database"
    );
    assert!(
        applied
            .backup_root
            .join(&fixture.target_id)
            .join(storage::SESSIONS_DB_FILENAME)
            .is_file()
    );
    assert!(
        fixture
            .profile
            .join("projects")
            .join(&fixture.source_id)
            .is_dir()
    );
    for project_id in [&fixture.source_id, &fixture.target_id] {
        let (canonical, retired) =
            input_manifest_paths(&fixture, project_id, &applied.destination_project_id);
        assert!(!canonical.exists());
        assert!(retired.is_file());
    }
    for (path, digest) in input_database_digests {
        assert_eq!(
            file_digest(&path).unwrap(),
            digest,
            "{} changed",
            path.display()
        );
    }
    let global = GlobalDb::open_at_without_structured_backfill(&fixture.profile.join("global.db"))
        .await
        .unwrap();
    let owners = global
        .list_code_projects(usize::MAX)
        .await
        .into_iter()
        .filter(|project| same_path(Path::new(&project.canonical_root), &fixture.project))
        .map(|project| project.project_id)
        .collect::<Vec<_>>();
    assert_eq!(owners, vec![applied.destination_project_id.clone()]);
    global.close();
    assert!(
        fixture
            .profile
            .join("projects")
            .join(&fixture.target_id)
            .is_dir()
    );

    let meta = branch_meta::load_branch_meta(&applied.destination_data_root).unwrap();
    assert!(meta.branches.contains_key("main"));
    assert!(
        meta.branches
            .contains_key(&format!("consolidated/{}/main", fixture.source_id))
    );

    let retried = apply(&options, &report.confirmation_token).await.unwrap();
    assert_eq!(retried.state, ConsolidationState::Applied);
    assert_eq!(sqlite::count_rows(&graph, "memory_facts").await.unwrap(), 3);
    assert_eq!(
        sqlite::count_rows(&graph, "memory_fact_relations")
            .await
            .unwrap(),
        1
    );
    assert_eq!(sqlite::count_rows(&sessions, "sessions").await.unwrap(), 2);
}

#[tokio::test]
async fn mixed_page_destination_survives_overlapping_watcher_opens() {
    let fixture = fixture().await;
    let source = layout_for_id(&fixture.project, &fixture.profile, &fixture.source_id).unwrap();
    let target = layout_for_id(&fixture.project, &fixture.profile, &fixture.target_id).unwrap();
    rewrite_page_size(&source.graph_db_path, 8192).await;
    rewrite_page_size(&target.graph_db_path, 4096).await;
    assert_eq!(database_page_size(&source.graph_db_path).await, 8192);
    assert_eq!(database_page_size(&target.graph_db_path).await, 4096);

    let options = fixture.options();
    let planned = plan(&options).await.unwrap();
    let applied = apply(&options, &planned.confirmation_token).await.unwrap();
    let destination = applied
        .destination_data_root
        .join(crate::config::DB_FILENAME);
    assert_eq!(database_page_size(&destination).await, 4096);

    let open_options = TraceDecayOpenOptions {
        profile_root: Some(fixture.profile.clone()),
        global_db_path: Some(fixture.profile.join("global.db")),
    };
    let cached = TraceDecay::open_with_options(&fixture.project, open_options.clone())
        .await
        .unwrap();

    for round in 0..2 {
        fs::write(
            fixture.project.join("lib.rs"),
            format!("pub fn fixture() -> usize {{ {round} }}\n"),
        )
        .unwrap();
        let watcher = TraceDecay::open_with_options(&fixture.project, open_options.clone())
            .await
            .unwrap();
        watcher
            .sync_if_stale_silent(&["lib.rs".to_string()])
            .await
            .unwrap();
        drop(watcher);

        assert!(storage::has_sqlite_database_header(&destination).unwrap());
        let (verification, _) = test_open_read_only(&destination).await;
        let mut mmap_rows = verification
            .conn()
            .query("PRAGMA mmap_size", ())
            .await
            .unwrap();
        assert_eq!(
            mmap_rows
                .next()
                .await
                .unwrap()
                .unwrap()
                .get::<i64>(0)
                .unwrap(),
            0
        );
        drop(mmap_rows);
        assert!(verification.quick_check().await.unwrap());
        verification.close();
        cached.get_all_files().await.unwrap();
    }
}

#[tokio::test]
async fn applied_manifest_retirement_handles_retry_states_and_fails_closed() {
    let fixture = fixture().await;
    let options = fixture.options();
    let report = plan(&options).await.unwrap();
    let applied = apply(&options, &report.confirmation_token).await.unwrap();
    let (source_canonical, source_retired) = input_manifest_paths(
        &fixture,
        &fixture.source_id,
        &applied.destination_project_id,
    );
    let (_, target_retired) = input_manifest_paths(
        &fixture,
        &fixture.target_id,
        &applied.destination_project_id,
    );

    fs::copy(&source_retired, &source_canonical).unwrap();
    let both_identical = apply(&options, &report.confirmation_token).await.unwrap();
    assert_eq!(both_identical.state, ConsolidationState::Applied);
    assert!(!source_canonical.exists());
    assert!(source_retired.is_file());

    fs::write(&source_canonical, b"divergent manifest").unwrap();
    let divergent = apply(&options, &report.confirmation_token)
        .await
        .unwrap_err();
    assert!(divergent.to_string().contains("manifests diverge"));
    assert_eq!(fs::read(&source_canonical).unwrap(), b"divergent manifest");
    assert!(source_retired.is_file());
    assert!(target_retired.is_file());

    fs::remove_file(&source_canonical).unwrap();
    storage::write_enrollment_marker(
        &fixture.project,
        &EnrollmentMarker {
            project_id: fixture.target_id.clone(),
            storage_mode: StorageMode::ProfileSharded,
        },
    )
    .unwrap();
    let marker_mismatch = apply(&options, &report.confirmation_token)
        .await
        .unwrap_err();
    assert!(marker_mismatch.to_string().contains("marker mismatch"));
    assert!(source_retired.is_file());
    assert!(target_retired.is_file());

    storage::write_enrollment_marker(
        &fixture.project,
        &EnrollmentMarker {
            project_id: applied.destination_project_id.clone(),
            storage_mode: StorageMode::ProfileSharded,
        },
    )
    .unwrap();
    fs::remove_file(&source_retired).unwrap();
    let missing = apply(&options, &report.confirmation_token)
        .await
        .unwrap_err();
    assert!(
        missing
            .to_string()
            .contains("neither canonical nor retired")
    );
    assert!(target_retired.is_file());
}

#[tokio::test]
async fn destination_preparation_restarts_after_every_publish_boundary() {
    for stop in [
        prepare::PrepareStop::TargetCopy,
        prepare::PrepareStop::SourceBranch(1),
        prepare::PrepareStop::BranchMetaWrite,
        prepare::PrepareStop::Publish,
    ] {
        let fixture = fixture().await;
        let options = fixture.options();
        let report = plan(&options).await.unwrap();

        let error = apply_with_prepare_stop(&options, &report.confirmation_token, stop)
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("synthetic interruption"),
            "{stop:?}: {error}"
        );

        let applied = apply(&options, &report.confirmation_token).await.unwrap();
        assert_eq!(applied.state, ConsolidationState::Applied, "{stop:?}");
        assert_eq!(
            storage::read_repository_identity_marker(&fixture.project)
                .unwrap()
                .unwrap()
                .project_id,
            applied.destination_project_id,
            "{stop:?}"
        );
    }
}

#[tokio::test]
async fn consolidation_restarts_after_every_durable_state() {
    for stop in [
        ConsolidationState::BackupsReady,
        ConsolidationState::DestinationReady,
        ConsolidationState::DatabasesMerged,
        ConsolidationState::ArtifactsMerged,
        ConsolidationState::Registered,
    ] {
        let fixture = fixture().await;
        let options = fixture.options();
        let report = plan(&options).await.unwrap();

        let error = apply_with_stop(&options, &report.confirmation_token, Some(stop.clone()))
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("synthetic interruption"),
            "{stop:?}: {error}"
        );
        assert_eq!(
            storage::read_repository_identity_marker(&fixture.project)
                .unwrap()
                .unwrap()
                .project_id,
            fixture.target_id,
            "{stop:?}: marker moved before the final state"
        );

        let applied = apply(&options, &report.confirmation_token).await.unwrap();
        assert_eq!(applied.state, ConsolidationState::Applied, "{stop:?}");
        assert_eq!(
            storage::read_repository_identity_marker(&fixture.project)
                .unwrap()
                .unwrap()
                .project_id,
            applied.destination_project_id,
            "{stop:?}"
        );
    }
}

#[tokio::test]
async fn version_one_premerge_ledger_migrates_before_resume() {
    let fixture = fixture().await;
    let options = fixture.options();
    let report = plan(&options).await.unwrap();
    let error = apply_with_stop(
        &options,
        &report.confirmation_token,
        Some(ConsolidationState::DestinationReady),
    )
    .await
    .unwrap_err();
    assert!(
        error.to_string().contains("synthetic interruption"),
        "{error:#}"
    );

    let mut ledger = load_ledger(&report.ledger_path).unwrap().unwrap();
    ledger.schema_version = 1;
    save_ledger(&report.ledger_path, &ledger).unwrap();

    let applied = apply(&options, &report.confirmation_token).await.unwrap();
    assert_eq!(applied.state, ConsolidationState::Applied);
    assert_eq!(
        load_ledger(&report.ledger_path)
            .unwrap()
            .unwrap()
            .schema_version,
        LEDGER_SCHEMA_VERSION
    );
}

#[tokio::test]
async fn version_one_postmerge_ledger_fails_closed() {
    let fixture = fixture().await;
    let options = fixture.options();
    let report = plan(&options).await.unwrap();
    let error = apply_with_stop(
        &options,
        &report.confirmation_token,
        Some(ConsolidationState::DatabasesMerged),
    )
    .await
    .unwrap_err();
    assert!(
        error.to_string().contains("synthetic interruption"),
        "{error}"
    );

    let mut ledger = load_ledger(&report.ledger_path).unwrap().unwrap();
    ledger.schema_version = 1;
    save_ledger(&report.ledger_path, &ledger).unwrap();

    let error = apply(&options, &report.confirmation_token)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("cannot be migrated safely"));
    assert_eq!(
        storage::read_repository_identity_marker(&fixture.project)
            .unwrap()
            .unwrap()
            .project_id,
        fixture.target_id
    );
}

#[tokio::test]
async fn verification_rejects_a_missing_unique_row_when_target_is_larger() {
    let fixture = fixture().await;
    for suffix in ["one", "two"] {
        add_fact_to_shard(
            &fixture,
            &fixture.target_id,
            &format!("extra target fact {suffix}"),
            "target-extra",
            json!({"suffix": suffix}),
            None,
        )
        .await;
    }
    let options = fixture.options();
    let report = plan(&options).await.unwrap();
    apply_with_stop(
        &options,
        &report.confirmation_token,
        Some(ConsolidationState::DatabasesMerged),
    )
    .await
    .unwrap_err();

    let graph_path = report
        .destination_data_root
        .join(crate::config::DB_FILENAME);
    let (graph, _) = test_open(&graph_path).await;
    graph
        .conn()
        .execute_batch(
            "PRAGMA foreign_keys = OFF;
             DELETE FROM memory_facts WHERE content = 'legacy durable fact';",
        )
        .await
        .unwrap();
    graph.checkpoint().await.unwrap();
    graph.close();
    assert_eq!(
        sqlite::count_rows(&graph_path, "memory_facts")
            .await
            .unwrap(),
        report.target.facts,
        "the old max(input) count check would have accepted this loss"
    );

    let error = apply(&options, &report.confirmation_token)
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("destination fact logical union differs"),
        "{error}"
    );
    assert_eq!(
        storage::read_repository_identity_marker(&fixture.project)
            .unwrap()
            .unwrap()
            .project_id,
        fixture.target_id
    );
}

#[tokio::test]
async fn verification_checks_session_bounds_and_immutable_message_payloads() {
    let fixture = fixture().await;
    let options = fixture.options();
    let report = plan(&options).await.unwrap();
    apply_with_stop(
        &options,
        &report.confirmation_token,
        Some(ConsolidationState::DatabasesMerged),
    )
    .await
    .unwrap_err();
    let sessions = report
        .destination_data_root
        .join(storage::SESSIONS_DB_FILENAME);

    execute_sql(
        &sessions,
        "UPDATE sessions SET ended_at=42 WHERE session_id='legacy-session'",
    )
    .await;
    let error = apply(&options, &report.confirmation_token)
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("destination session logical union differs"),
        "{error}"
    );

    execute_sql(
        &sessions,
        "UPDATE sessions SET ended_at=1800000001 WHERE session_id='legacy-session';
         UPDATE session_messages SET text='corrupted text'
         WHERE message_id='message-legacy-session';",
    )
    .await;
    let error = apply(&options, &report.confirmation_token)
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("destination session message logical union differs"),
        "{error}"
    );

    execute_sql(
        &sessions,
        "UPDATE session_messages SET text='message from legacy-session'
         WHERE message_id='message-legacy-session';
         UPDATE lcm_raw_messages SET content_hash='corrupted-hash'
         WHERE session_id='legacy-session';",
    )
    .await;
    let error = apply(&options, &report.confirmation_token)
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("destination LCM raw message logical union differs"),
        "{error}"
    );
}

#[tokio::test]
async fn divergent_session_message_projections_preserve_a_source_variant() {
    let fixture = fixture().await;
    let source_sessions = fixture
        .profile
        .join("projects")
        .join(&fixture.source_id)
        .join(storage::SESSIONS_DB_FILENAME);
    execute_sql(
        &source_sessions,
        "INSERT INTO session_messages(
             provider, message_id, session_id, role, timestamp, ordinal, text, kind, model
         ) VALUES
             ('codex', 'message-current-session', 'legacy-session', 'assistant',
              1800000000, 1, 'source divergent projection', 'message', 'source-model'),
             ('codex', 'source-only-message', 'legacy-session', 'user',
              1800000001, 2, 'source-only projection', 'message', NULL);",
    )
    .await;

    let options = fixture.options();
    let report = plan(&options).await.unwrap();
    assert_eq!(report.collisions.message_overlaps, 1);
    assert_eq!(report.collisions.divergent_lcm_messages, 0);

    let applied = apply(&options, &report.confirmation_token).await.unwrap();
    let sessions = GlobalDb::open_read_only_at(
        &applied
            .destination_data_root
            .join(storage::SESSIONS_DB_FILENAME),
    )
    .await
    .unwrap();
    let selected = sessions
        .get_session_message("codex", "message-current-session")
        .await
        .unwrap();
    assert_eq!(selected.session_id, "current-session");
    assert_eq!(selected.text, "message from current-session");
    assert_eq!(selected.role, "user");
    let variant_id = format!("consolidated/{}/message-current-session", fixture.source_id);
    let variant = sessions
        .get_session_message("codex", &variant_id)
        .await
        .unwrap();
    assert_eq!(variant.session_id, "legacy-session");
    assert_eq!(variant.text, "source divergent projection");
    assert_eq!(variant.role, "assistant");
    let source_only = sessions
        .get_session_message("codex", "source-only-message")
        .await
        .unwrap();
    assert_eq!(source_only.session_id, "legacy-session");
    assert_eq!(source_only.text, "source-only projection");
    sessions.close();
}

#[tokio::test]
async fn lcm_representation_drift_uses_the_selected_target_row() {
    let fixture = fixture().await;
    let source_sessions = fixture
        .profile
        .join("projects")
        .join(&fixture.source_id)
        .join(storage::SESSIONS_DB_FILENAME);
    let target_sessions = fixture
        .profile
        .join("projects")
        .join(&fixture.target_id)
        .join(storage::SESSIONS_DB_FILENAME);
    let target = GlobalDb::open_read_only_at(&target_sessions).await.unwrap();
    let target_raw = target
        .lcm_load_raw_message("codex", "message-current-session")
        .await
        .unwrap();
    target.close();
    let source = GlobalDb::open_at_without_structured_backfill(&source_sessions)
        .await
        .unwrap();
    source
        .conn()
        .execute(
            "UPDATE lcm_raw_messages
             SET message_id=?1, content=?2, content_hash=?3,
                 storage_kind='external', payload_ref=NULL
             WHERE provider='codex' AND message_id='message-legacy-session'",
            libsql::params![
                target_raw.message_id.clone(),
                target_raw.content.clone(),
                target_raw.content_hash.clone()
            ],
        )
        .await
        .unwrap();
    source.checkpoint().await;
    source.close();

    let options = fixture.options();
    let report = plan(&options).await.unwrap();
    assert_eq!(report.collisions.lcm_message_overlaps, 1);
    assert_eq!(report.collisions.divergent_lcm_messages, 1);
    assert_eq!(report.collisions.divergent_lcm_session_ids, 1);
    assert_eq!(report.collisions.divergent_lcm_content_hashes, 0);
    assert_eq!(report.collisions.divergent_lcm_storage_kinds, 1);
    assert_eq!(report.collisions.divergent_lcm_payload_refs, 0);

    let applied = apply(&options, &report.confirmation_token).await.unwrap();
    let destination = GlobalDb::open_read_only_at(
        &applied
            .destination_data_root
            .join(storage::SESSIONS_DB_FILENAME),
    )
    .await
    .unwrap();
    assert_eq!(
        destination
            .lcm_load_raw_message("codex", "message-current-session")
            .await
            .unwrap(),
        target_raw
    );
    destination.close();
}

#[tokio::test]
async fn session_only_divergence_does_not_duplicate_identical_external_raw_family() {
    let fixture = fixture().await;
    let source = layout_for_id(&fixture.project, &fixture.profile, &fixture.source_id).unwrap();
    let target = layout_for_id(&fixture.project, &fixture.profile, &fixture.target_id).unwrap();
    let payload = b"shared payload";
    let payload_ref = "shared.payload";
    let content_hash = crate::sessions::lcm::raw::sha256_hex("shared payload");
    for layout in [&source, &target] {
        fs::create_dir_all(layout.data_root.join("lcm-payloads")).unwrap();
        fs::write(
            layout.data_root.join("lcm-payloads").join(payload_ref),
            payload,
        )
        .unwrap();
    }
    execute_sql(
        &source.sessions_db_path,
        &format!(
            "UPDATE session_messages
             SET message_id='message-current-session', text='source projection'
             WHERE message_id='message-legacy-session';
             UPDATE lcm_raw_messages
             SET message_id='message-current-session', content=NULL,
                 content_hash='{content_hash}', storage_kind='external',
                 payload_ref='{payload_ref}'
             WHERE message_id='message-legacy-session';
             INSERT INTO lcm_external_payloads(
                 payload_ref, provider, session_id, message_id, kind, content_hash,
                 byte_count, char_count
             ) VALUES('{payload_ref}', 'codex', 'legacy-session',
                      'message-current-session', 'message', '{content_hash}', 14, 14);"
        ),
    )
    .await;
    execute_sql(
        &target.sessions_db_path,
        &format!(
            "UPDATE lcm_raw_messages
             SET content=NULL, content_hash='{content_hash}', storage_kind='external',
                 payload_ref='{payload_ref}'
             WHERE message_id='message-current-session';
             INSERT INTO lcm_external_payloads(
                 payload_ref, provider, session_id, message_id, kind, content_hash,
                 byte_count, char_count
             ) VALUES('{payload_ref}', 'codex', 'current-session',
                      'message-current-session', 'message', '{content_hash}', 14, 14);"
        ),
    )
    .await;

    let options = fixture.options();
    let report = plan(&options).await.unwrap();
    let applied = apply(&options, &report.confirmation_token).await.unwrap();
    let variant_id = format!("consolidated/{}/message-current-session", fixture.source_id);
    let sessions = GlobalDb::open_read_only_at(
        &applied
            .destination_data_root
            .join(storage::SESSIONS_DB_FILENAME),
    )
    .await
    .unwrap();
    assert!(
        sessions
            .get_session_message("codex", &variant_id)
            .await
            .is_some()
    );
    assert!(
        sessions
            .lcm_load_raw_message("codex", &variant_id)
            .await
            .is_none()
    );
    let raw = sessions
        .lcm_load_raw_message("codex", "message-current-session")
        .await
        .unwrap();
    assert_eq!(raw.content_hash, content_hash);
    assert_eq!(raw.payload_ref.as_deref(), Some(payload_ref));
    let mut rows = sessions
        .conn()
        .query(
            "SELECT message_id FROM lcm_external_payloads WHERE payload_ref=?1",
            [payload_ref],
        )
        .await
        .unwrap();
    assert_eq!(
        rows.next()
            .await
            .unwrap()
            .unwrap()
            .get::<String>(0)
            .unwrap(),
        "message-current-session"
    );
    sessions.close();
}

#[tokio::test]
async fn distinct_external_content_variant_preserves_owner_expansion_and_retry() {
    let fixture = fixture().await;
    let source = layout_for_id(&fixture.project, &fixture.profile, &fixture.source_id).unwrap();
    let target = layout_for_id(&fixture.project, &fixture.profile, &fixture.target_id).unwrap();
    let mut source_ref = None;
    for (layout, session_id, old_message_id, content) in [
        (
            &source,
            "legacy-session",
            "message-legacy-session",
            "source external body",
        ),
        (
            &target,
            "current-session",
            "message-current-session",
            "target external body",
        ),
    ] {
        let payload = crate::sessions::lcm::payload::write_external_payload(
            &layout.data_root,
            "codex",
            session_id,
            "message-current-session",
            "message",
            content,
            None,
        )
        .unwrap();
        if session_id == "legacy-session" {
            source_ref = Some(payload.payload_ref.clone());
        }
        let db = GlobalDb::open_at_without_structured_backfill(&layout.sessions_db_path)
            .await
            .unwrap();
        crate::sessions::lcm::payload::upsert_payload_metadata(db.conn(), &payload)
            .await
            .unwrap();
        db.conn()
            .execute(
                "UPDATE session_messages
                 SET message_id='message-current-session', text=?1
                 WHERE provider='codex' AND message_id=?2",
                libsql::params![content, old_message_id],
            )
            .await
            .unwrap();
        db.conn()
            .execute(
                "UPDATE lcm_raw_messages
                 SET message_id='message-current-session', content=NULL,
                     content_hash=?1, storage_kind='external', payload_ref=?2
                 WHERE provider='codex' AND message_id=?3",
                libsql::params![payload.content_hash, payload.payload_ref, old_message_id],
            )
            .await
            .unwrap();
        db.checkpoint().await;
        db.close();
    }

    let options = fixture.options();
    let report = plan(&options).await.unwrap();
    let applied = apply(&options, &report.confirmation_token).await.unwrap();
    let variant_id = format!("consolidated/{}/message-current-session", fixture.source_id);
    let source_ref = source_ref.unwrap();
    let sessions = GlobalDb::open_read_only_at(
        &applied
            .destination_data_root
            .join(storage::SESSIONS_DB_FILENAME),
    )
    .await
    .unwrap();
    let raw = sessions
        .lcm_load_raw_message("codex", &variant_id)
        .await
        .unwrap();
    assert_eq!(raw.payload_ref.as_deref(), Some(source_ref.as_str()));
    let mut owners = sessions
        .conn()
        .query(
            "SELECT message_id FROM lcm_external_payloads WHERE payload_ref=?1",
            [source_ref.as_str()],
        )
        .await
        .unwrap();
    assert_eq!(
        owners
            .next()
            .await
            .unwrap()
            .unwrap()
            .get::<String>(0)
            .unwrap(),
        variant_id
    );
    drop(owners);
    let expanded = crate::sessions::lcm::payload::LcmStore::new(
        sessions.conn(),
        applied.destination_data_root.clone(),
    )
    .lcm_expand_payload("codex", "legacy-session", &source_ref, 0, 100)
    .await
    .unwrap();
    assert_eq!(expanded.content, "source external body");
    sessions.close();

    let retried = apply(&options, &report.confirmation_token).await.unwrap();
    assert_eq!(retried.state, ConsolidationState::Applied);
}

#[tokio::test]
async fn divergent_projection_and_raw_content_preserve_a_linked_source_variant() {
    let fixture = fixture().await;
    let source_sessions = fixture
        .profile
        .join("projects")
        .join(&fixture.source_id)
        .join(storage::SESSIONS_DB_FILENAME);
    let target_sessions = fixture
        .profile
        .join("projects")
        .join(&fixture.target_id)
        .join(storage::SESSIONS_DB_FILENAME);
    execute_sql(
        &source_sessions,
        "UPDATE session_messages
         SET message_id='message-current-session', text='source divergent projection',
             metadata_json='{\"parent_message_id\":\"message-current-session\"}'
         WHERE provider='codex' AND message_id='message-legacy-session';
         UPDATE lcm_raw_messages
         SET message_id='message-current-session',
             metadata_json='{\"parent_message_id\":\"message-current-session\"}'
         WHERE provider='codex' AND message_id='message-legacy-session';
         INSERT INTO turns(
             message_id, project_hash, session_id, model, timestamp, input_tokens,
             output_tokens, cost_usd, category
         ) VALUES(
             'message-current-session', 'project', 'legacy-session', 'model',
             1800000000, 1, 1, 0.0, 'task'
         );
         INSERT INTO session_messages(
             provider, message_id, session_id, role, timestamp, ordinal, text,
             kind, metadata_json
         ) VALUES(
             'codex', 'message-current-session:thinking', 'legacy-session',
             'assistant', 1800000001, 2, 'source thinking', 'reasoning',
             '{\"parent_message_id\":\"message-current-session\"}'
         );
         INSERT INTO lcm_raw_messages(
             provider, message_id, session_id, role, ordinal, timestamp, content,
             content_hash, storage_kind, snippet_text, index_text, metadata_json
         ) VALUES(
             'codex', 'message-current-session:thinking', 'legacy-session',
             'assistant', 2, 1800000001, 'source thinking', 'thinking-hash',
             'inline', 'source thinking', 'source thinking',
             '{\"parent_message_id\":\"message-current-session\"}'
         );
         INSERT INTO lcm_summary_nodes(
             node_id, provider, conversation_id, session_id, depth, summary_text,
             summary_hash, summary_token_count, source_token_count, created_at
         ) VALUES(
             'variant-summary', 'codex', 'source-conversation', 'legacy-session', 1,
             'variant summary', 'variant-summary-hash', 1, 1, 1800000002
         );
         INSERT INTO lcm_summary_sources(node_id, source_kind, source_id, ordinal)
         SELECT 'variant-summary', 'raw_message', CAST(store_id AS TEXT), 0
         FROM lcm_raw_messages WHERE message_id='message-current-session';",
    )
    .await;
    execute_sql(
        &target_sessions,
        "INSERT INTO session_messages(
             provider, message_id, session_id, role, timestamp, ordinal, text,
             kind, metadata_json
         ) VALUES(
             'codex', 'message-current-session:thinking', 'current-session',
             'assistant', 1800000001, 2, 'source thinking', 'reasoning',
             '{\"parent_message_id\":\"message-current-session\"}'
         );
         INSERT INTO lcm_raw_messages(
             provider, message_id, session_id, role, ordinal, timestamp, content,
             content_hash, storage_kind, snippet_text, index_text, metadata_json
         ) VALUES(
             'codex', 'message-current-session:thinking', 'current-session',
             'assistant', 2, 1800000001, 'source thinking', 'thinking-hash',
             'inline', 'source thinking', 'source thinking',
             '{\"parent_message_id\":\"message-current-session\"}'
         );",
    )
    .await;

    let options = fixture.options();
    let report = plan(&options).await.unwrap();
    assert_eq!(report.collisions.divergent_lcm_messages, 2);
    assert_eq!(report.collisions.divergent_lcm_session_ids, 2);
    assert_eq!(report.collisions.divergent_lcm_content_hashes, 1);
    assert_eq!(report.collisions.divergent_lcm_storage_kinds, 0);
    assert_eq!(report.collisions.divergent_lcm_payload_refs, 0);

    let applied = apply(&options, &report.confirmation_token).await.unwrap();
    let variant_id = format!("consolidated/{}/message-current-session", fixture.source_id);
    let sessions = GlobalDb::open_read_only_at(
        &applied
            .destination_data_root
            .join(storage::SESSIONS_DB_FILENAME),
    )
    .await
    .unwrap();
    assert_eq!(
        sessions
            .get_session_message("codex", "message-current-session")
            .await
            .unwrap()
            .text,
        "message from current-session"
    );
    let variant = sessions
        .get_session_message("codex", &variant_id)
        .await
        .unwrap();
    assert_eq!(variant.text, "source divergent projection");
    let metadata: serde_json::Value =
        serde_json::from_str(variant.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(metadata["parent_message_id"], variant_id);
    assert_eq!(
        metadata["consolidation_original_parent_message_id"],
        "message-current-session"
    );
    let raw_variant = sessions
        .lcm_load_raw_message("codex", &variant_id)
        .await
        .unwrap();
    assert_eq!(raw_variant.content, "message from legacy-session");
    let raw_metadata: serde_json::Value =
        serde_json::from_str(raw_variant.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(raw_metadata["parent_message_id"], variant_id);
    assert_eq!(
        raw_metadata["consolidation_original_parent_message_id"],
        "message-current-session"
    );
    let thinking_variant_id = format!("{variant_id}:thinking");
    let thinking = sessions
        .get_session_message("codex", &thinking_variant_id)
        .await
        .unwrap();
    let thinking_metadata: serde_json::Value =
        serde_json::from_str(thinking.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(thinking_metadata["parent_message_id"], variant_id);
    assert_eq!(
        thinking_metadata["consolidation_original_parent_message_id"],
        "message-current-session"
    );
    assert!(
        sessions
            .lcm_load_raw_message("codex", &thinking_variant_id)
            .await
            .is_none()
    );
    let thinking_raw = sessions
        .lcm_load_raw_message("codex", "message-current-session:thinking")
        .await
        .unwrap();
    let thinking_raw_metadata: serde_json::Value =
        serde_json::from_str(thinking_raw.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(
        thinking_raw_metadata["parent_message_id"],
        "message-current-session"
    );
    let mut turn_rows = sessions
        .conn()
        .query(
            "SELECT message_id FROM turns WHERE session_id='legacy-session'",
            (),
        )
        .await
        .unwrap();
    assert_eq!(
        turn_rows
            .next()
            .await
            .unwrap()
            .unwrap()
            .get::<String>(0)
            .unwrap(),
        variant_id
    );
    drop(turn_rows);
    let mut rows = sessions
        .conn()
        .query(
            "SELECT r.message_id
             FROM lcm_summary_sources s
             JOIN lcm_raw_messages r ON r.store_id=CAST(s.source_id AS INTEGER)
             WHERE s.node_id='variant-summary'",
            (),
        )
        .await
        .unwrap();
    assert_eq!(
        rows.next()
            .await
            .unwrap()
            .unwrap()
            .get::<String>(0)
            .unwrap(),
        variant_id
    );
    assert!(rows.next().await.unwrap().is_none());
    drop(rows);
    sessions.close();

    let retried = apply(&options, &report.confirmation_token).await.unwrap();
    assert_eq!(retried.state, ConsolidationState::Applied);
    let sessions = GlobalDb::open_read_only_at(
        &retried
            .destination_data_root
            .join(storage::SESSIONS_DB_FILENAME),
    )
    .await
    .unwrap();
    assert_eq!(sessions.session_message_count().await.unwrap(), 4);
    sessions.close();
}

#[tokio::test]
async fn indexed_message_family_materialization_handles_deep_and_wide_graph() {
    const DEPTH: usize = 128;
    const WIDTH: usize = 256;

    let fixture = fixture().await;
    let source = layout_for_id(&fixture.project, &fixture.profile, &fixture.source_id).unwrap();
    let target = layout_for_id(&fixture.project, &fixture.profile, &fixture.target_id).unwrap();
    execute_sql(
        &source.sessions_db_path,
        "UPDATE session_messages
         SET message_id='message-current-session', text='source divergent projection'
         WHERE provider='codex' AND message_id='message-legacy-session';",
    )
    .await;

    let mut family_sql = String::from(
        "INSERT OR IGNORE INTO sessions(provider, session_id, project_key, project_path)
         VALUES('codex', 'family-session', 'project', '/repo');",
    );
    let mut parent = "message-current-session".to_string();
    for depth in 0..DEPTH {
        let child = format!("family-depth-{depth}");
        family_sql.push_str(&format!(
            "INSERT INTO session_messages(
                 provider, message_id, session_id, role, ordinal, text, kind, metadata_json
             ) VALUES(
                 'codex', '{child}', 'family-session', 'assistant', {ordinal},
                 'depth {depth}', 'message', '{{\"parent_message_id\":\"{parent}\"}}'
             );",
            ordinal = depth + 2,
        ));
        parent = child;
    }
    for width in 0..WIDTH {
        let child = format!("family-wide-{width}");
        family_sql.push_str(&format!(
            "INSERT INTO session_messages(
                 provider, message_id, session_id, role, ordinal, text, kind, metadata_json
             ) VALUES(
                 'codex', '{child}', 'family-session', 'assistant', {ordinal},
                 'wide {width}', 'message',
                 '{{\"parent_message_id\":\"message-current-session\"}}'
             );",
            ordinal = DEPTH + width + 2,
        ));
    }
    execute_sql(&source.sessions_db_path, &family_sql).await;
    execute_sql(&target.sessions_db_path, &family_sql).await;
    sqlite::plan_session_offsets(&target.sessions_db_path, &source.sessions_db_path)
        .await
        .unwrap();

    let target_db = GlobalDb::open_at_without_structured_backfill(&target.sessions_db_path)
        .await
        .unwrap();
    target_db
        .conn()
        .execute(
            "ATTACH DATABASE ?1 AS source_input",
            libsql::params![source.sessions_db_path.to_string_lossy().to_string()],
        )
        .await
        .unwrap();
    sqlite::build_consolidation_message_map(
        target_db.conn(),
        "source_input",
        "main",
        &fixture.source_id,
    )
    .await
    .unwrap();

    let mut rows = target_db
        .conn()
        .query("SELECT COUNT(*) FROM consolidation_message_map", ())
        .await
        .unwrap();
    assert_eq!(
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        (1 + DEPTH + WIDTH) as i64
    );
    drop(rows);
    for original_id in [
        format!("family-depth-{}", DEPTH - 1),
        format!("family-wide-{}", WIDTH - 1),
    ] {
        let mut rows = target_db
            .conn()
            .query(
                "SELECT mapped_id FROM consolidation_message_map
                 WHERE provider='codex' AND original_id=?1",
                [original_id.as_str()],
            )
            .await
            .unwrap();
        assert_eq!(
            rows.next()
                .await
                .unwrap()
                .unwrap()
                .get::<String>(0)
                .unwrap(),
            format!("consolidated/{}/{original_id}", fixture.source_id)
        );
    }

    let family_plan_sql = format!(
        "{} SELECT COUNT(*) FROM variant_family",
        sqlite::session_variant_family_cte()
    );
    let family_plan = explain_query_plan(target_db.conn(), &family_plan_sql).await;
    assert!(
        family_plan
            .iter()
            .any(|detail| detail.contains("SEARCH edge USING")),
        "recursive family lookup must use the parent-edge primary key: {family_plan:?}"
    );
    assert!(
        family_plan
            .iter()
            .all(|detail| !detail.contains("session_messages")),
        "recursive family step must not rescan source session messages: {family_plan:?}"
    );

    let reserved_plan =
        explain_query_plan(target_db.conn(), sqlite::reserved_message_collision_sql()).await;
    assert!(
        reserved_plan
            .iter()
            .any(|detail| detail.contains("SEARCH r USING")),
        "reserved-reference lookup must use its primary key: {reserved_plan:?}"
    );

    let turn_lookup = sqlite::mapped_turn_message_id("s");
    let turn_plan_sql = format!(
        "SELECT {turn_lookup}
         FROM (SELECT 'message-current-session' AS message_id,
                      'legacy-session' AS session_id) s"
    );
    let turn_plan = explain_query_plan(target_db.conn(), &turn_plan_sql).await;
    assert!(
        turn_plan
            .iter()
            .any(|detail| detail.contains("SEARCH m USING")),
        "turn-owner lookup must use its primary key: {turn_plan:?}"
    );

    target_db
        .conn()
        .execute("DETACH DATABASE source_input", ())
        .await
        .unwrap();
    target_db.close();
}

#[tokio::test]
async fn numeric_and_boolean_parent_ids_expand_the_variant_family() {
    let fixture = fixture().await;
    let source = layout_for_id(&fixture.project, &fixture.profile, &fixture.source_id).unwrap();
    let target = layout_for_id(&fixture.project, &fixture.profile, &fixture.target_id).unwrap();
    execute_sql(
        &source.sessions_db_path,
        "UPDATE session_messages
         SET message_id='7', text='source divergent projection'
         WHERE provider='codex' AND message_id='message-legacy-session';",
    )
    .await;
    execute_sql(
        &target.sessions_db_path,
        "UPDATE session_messages SET message_id='7'
         WHERE provider='codex' AND message_id='message-current-session';",
    )
    .await;
    let family_sql =
        "INSERT OR IGNORE INTO sessions(provider, session_id, project_key, project_path)
         VALUES('codex', 'scalar-family', 'project', '/repo');
         INSERT INTO session_messages(
             provider, message_id, session_id, role, ordinal, text, kind, metadata_json
         ) VALUES(
             'codex', '1', 'scalar-family', 'assistant', 1, 'numeric child', 'message',
             '{\"parent_message_id\":7}'
         );
         INSERT INTO session_messages(
             provider, message_id, session_id, role, ordinal, text, kind, metadata_json
         ) VALUES(
             'codex', 'boolean-child', 'scalar-family', 'assistant', 2,
             'boolean child', 'message', '{\"parent_message_id\":true}'
         );";
    execute_sql(&source.sessions_db_path, family_sql).await;
    execute_sql(&target.sessions_db_path, family_sql).await;

    let options = fixture.options();
    let report = plan(&options).await.unwrap();
    let applied = apply(&options, &report.confirmation_token).await.unwrap();
    let sessions = GlobalDb::open_read_only_at(
        &applied
            .destination_data_root
            .join(storage::SESSIONS_DB_FILENAME),
    )
    .await
    .unwrap();
    let numeric_id = format!("consolidated/{}/1", fixture.source_id);
    let boolean_id = format!("consolidated/{}/boolean-child", fixture.source_id);
    let numeric = sessions
        .get_session_message("codex", &numeric_id)
        .await
        .unwrap();
    let boolean = sessions
        .get_session_message("codex", &boolean_id)
        .await
        .unwrap();
    let numeric_metadata: serde_json::Value =
        serde_json::from_str(numeric.metadata_json.as_deref().unwrap()).unwrap();
    let boolean_metadata: serde_json::Value =
        serde_json::from_str(boolean.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(
        numeric_metadata["parent_message_id"],
        format!("consolidated/{}/7", fixture.source_id)
    );
    assert_eq!(boolean_metadata["parent_message_id"], numeric_id);
    sessions.close();
}

#[tokio::test]
async fn synthetic_message_key_parent_reference_collision_fails_before_merge() {
    let fixture = fixture().await;
    let source = layout_for_id(&fixture.project, &fixture.profile, &fixture.source_id).unwrap();
    let synthetic = format!("consolidated/{}/message-current-session", fixture.source_id);
    let source_db = GlobalDb::open_at_without_structured_backfill(&source.sessions_db_path)
        .await
        .unwrap();
    source_db
        .conn()
        .execute(
            "UPDATE session_messages
             SET message_id='message-current-session', text='source divergent projection',
                 metadata_json=?1
             WHERE provider='codex' AND message_id='message-legacy-session'",
            [format!("{{\"parent_message_id\":\"{synthetic}\"}}")],
        )
        .await
        .unwrap();
    source_db.checkpoint().await;
    source_db.close();

    let options = fixture.options();
    let report = plan(&options).await.unwrap();
    let error = apply(&options, &report.confirmation_token)
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("synthetic consolidation message key collision"),
        "{error}"
    );
}

#[tokio::test]
async fn synthetic_message_key_collision_fails_before_merge() {
    let fixture = fixture().await;
    let source_sessions = fixture
        .profile
        .join("projects")
        .join(&fixture.source_id)
        .join(storage::SESSIONS_DB_FILENAME);
    let synthetic = format!("consolidated/{}/message-current-session", fixture.source_id);
    let source = GlobalDb::open_at_without_structured_backfill(&source_sessions)
        .await
        .unwrap();
    source
        .conn()
        .execute(
            "UPDATE session_messages
             SET message_id='message-current-session', text='source divergent projection'
             WHERE provider='codex' AND message_id='message-legacy-session'",
            (),
        )
        .await
        .unwrap();
    source
        .conn()
        .execute(
            "INSERT INTO session_messages(
                 provider, message_id, session_id, role, ordinal, text, kind
             ) VALUES('codex', ?1, 'legacy-session', 'user', 2, 'native collision', 'message')",
            [synthetic],
        )
        .await
        .unwrap();
    source.checkpoint().await;
    source.close();

    let options = fixture.options();
    let report = plan(&options).await.unwrap();
    let error = apply(&options, &report.confirmation_token)
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("synthetic consolidation message key collision"),
        "{error}"
    );
    assert_eq!(
        storage::read_repository_identity_marker(&fixture.project)
            .unwrap()
            .unwrap()
            .project_id,
        fixture.target_id
    );
}

#[tokio::test]
async fn ambiguous_cross_provider_turn_mapping_fails_closed() {
    let fixture = fixture().await;
    let source = layout_for_id(&fixture.project, &fixture.profile, &fixture.source_id).unwrap();
    let target = layout_for_id(&fixture.project, &fixture.profile, &fixture.target_id).unwrap();
    execute_sql(
        &source.sessions_db_path,
        "UPDATE session_messages
         SET message_id='shared-message', text='source codex'
         WHERE provider='codex' AND message_id='message-legacy-session';
         INSERT INTO sessions(provider, session_id, project_key, project_path)
         VALUES('claude', 'legacy-session', 'project', '/repo');
         INSERT INTO session_messages(
             provider, message_id, session_id, role, ordinal, text, kind
         ) VALUES('claude', 'shared-message', 'legacy-session', 'assistant', 1,
                  'source claude', 'message');
         INSERT INTO turns(
             message_id, project_hash, session_id, model, timestamp, input_tokens,
             output_tokens, cost_usd, category
         ) VALUES('shared-message', 'project', 'legacy-session', 'model',
                  1800000000, 1, 1, 0.0, 'task');",
    )
    .await;
    execute_sql(
        &target.sessions_db_path,
        "UPDATE session_messages SET message_id='shared-message'
         WHERE provider='codex' AND message_id='message-current-session';
         INSERT INTO sessions(provider, session_id, project_key, project_path)
         VALUES('claude', 'current-session', 'project', '/repo');
         INSERT INTO session_messages(
             provider, message_id, session_id, role, ordinal, text, kind
         ) VALUES('claude', 'shared-message', 'current-session', 'user', 1,
                  'target claude', 'message');",
    )
    .await;

    let options = fixture.options();
    let report = plan(&options).await.unwrap();
    let error = apply(&options, &report.confirmation_token)
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("source turn message mapping ambiguity collision"),
        "{error}"
    );
}

#[tokio::test]
async fn divergent_external_payload_identity_remains_a_hard_error() {
    let fixture = fixture().await;
    let source = layout_for_id(&fixture.project, &fixture.profile, &fixture.source_id).unwrap();
    let target = layout_for_id(&fixture.project, &fixture.profile, &fixture.target_id).unwrap();
    execute_sql(
        &source.sessions_db_path,
        "INSERT INTO lcm_external_payloads(
             payload_ref, provider, session_id, message_id, kind, content_hash,
             byte_count, char_count, metadata_json
         ) VALUES('shared-ref', 'codex', 'legacy-session', 'message-legacy-session',
                  'tool', 'source-hash', 10, 10, NULL);",
    )
    .await;
    execute_sql(
        &target.sessions_db_path,
        "INSERT INTO lcm_external_payloads(
             payload_ref, provider, session_id, message_id, kind, content_hash,
             byte_count, char_count, metadata_json
         ) VALUES('shared-ref', 'codex', 'current-session', 'message-current-session',
                  'tool', 'target-hash', 11, 11, NULL);",
    )
    .await;

    let options = fixture.options();
    let report = plan(&options).await.unwrap();
    let error = apply(&options, &report.confirmation_token)
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("divergent LCM external payload collision"),
        "{error}"
    );
}

#[tokio::test]
async fn divergent_summary_node_identity_remains_a_hard_error() {
    let fixture = fixture().await;
    let source = layout_for_id(&fixture.project, &fixture.profile, &fixture.source_id).unwrap();
    let target = layout_for_id(&fixture.project, &fixture.profile, &fixture.target_id).unwrap();
    for (path, session, text, hash) in [
        (
            &source.sessions_db_path,
            "legacy-session",
            "source summary",
            "source-hash",
        ),
        (
            &target.sessions_db_path,
            "current-session",
            "target summary",
            "target-hash",
        ),
    ] {
        let (db, _) = test_open(path).await;
        db.conn()
            .execute(
                "INSERT INTO lcm_summary_nodes(
                     node_id, provider, conversation_id, session_id, depth, summary_text,
                     summary_hash, summary_token_count, source_token_count, created_at
                 ) VALUES('shared-summary', 'codex', 'conversation', ?1, 1, ?2, ?3, 1, 1, 1800000002)",
                libsql::params![session, text, hash],
            )
            .await
            .unwrap();
        db.checkpoint().await.unwrap();
        db.close();
    }

    let options = fixture.options();
    let report = plan(&options).await.unwrap();
    let error = apply(&options, &report.confirmation_token)
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("divergent LCM summary node collision"),
        "{error}"
    );
}

#[tokio::test]
#[cfg(not(windows))]
async fn identity_survives_symlink_and_repository_move() {
    let fixture = fixture().await;
    let symlink = fixture.project.parent().unwrap().join("repo-symlink");
    std::os::unix::fs::symlink(&fixture.project, &symlink).unwrap();
    let mut options = fixture.options();
    options.project_root = symlink;
    let report = plan(&options).await.unwrap();
    let applied = apply(&options, &report.confirmation_token).await.unwrap();
    assert_eq!(
        storage::read_enrollment_marker(&fixture.project)
            .unwrap()
            .unwrap()
            .project_id,
        applied.destination_project_id
    );
    assert_eq!(
        storage::read_repository_identity_marker(&fixture.project)
            .unwrap()
            .unwrap()
            .project_id,
        applied.destination_project_id
    );

    let moved = fixture.project.parent().unwrap().join("repo-moved");
    fs::rename(&fixture.project, &moved).unwrap();
    let reopened = TraceDecay::open_read_only_with_options(
        &moved,
        TraceDecayOpenOptions {
            profile_root: Some(fixture.profile.clone()),
            global_db_path: Some(fixture.profile.join("global.db")),
        },
    )
    .await
    .unwrap();
    assert!(same_path(
        &reopened.store_layout().data_root,
        &applied.destination_data_root
    ));
}

#[tokio::test]
async fn untracked_branch_databases_with_mixed_case_extensions_are_recovered() {
    const SOURCE_ORPHAN_FACT: &str = "fact unique to the source orphan branch";
    const TARGET_ORPHAN_FACT: &str = "fact unique to the target orphan branch";
    let fixture = fixture().await;
    let source = layout_for_id(&fixture.project, &fixture.profile, &fixture.source_id).unwrap();
    let target = layout_for_id(&fixture.project, &fixture.profile, &fixture.target_id).unwrap();
    add_untracked_branch(&source, "orphan-source", SOURCE_ORPHAN_FACT).await;
    add_untracked_branch(&target, "orphan-target", TARGET_ORPHAN_FACT).await;
    fs::rename(
        target.data_root.join("branches/orphan-target.db"),
        target.data_root.join("branches/orphan-target.DB"),
    )
    .unwrap();

    let options = fixture.options();
    let planned = plan(&options).await.unwrap();
    assert_eq!(planned.source.graph_databases, 2);
    assert_eq!(planned.target.graph_databases, 2);
    assert_eq!(planned.source.branches, 2);
    assert_eq!(planned.target.branches, 2);

    let applied = apply(&options, &planned.confirmation_token).await.unwrap();
    let meta = branch_meta::load_branch_meta(&applied.destination_data_root).unwrap();
    let recovered = meta
        .branches
        .iter()
        .filter(|(name, _)| name.contains("orphan-source-") || name.contains("orphan-target-"))
        .map(|(name, entry)| {
            assert!(entry.gc_protected);
            (name.clone(), entry.db_file.clone())
        })
        .collect::<Vec<_>>();
    assert_eq!(recovered.len(), 2);
    assert!(recovered.iter().any(|(name, db_file)| {
        name.starts_with("recovered/orphan-target-") && db_file == "branches/orphan-target.DB"
    }));
    assert!(recovered.iter().any(|(name, db_file)| {
        name.starts_with(&format!(
            "consolidated/{}/recovered/orphan-source-",
            fixture.source_id
        )) && applied.destination_data_root.join(db_file).is_file()
    }));

    let gc = crate::branch::gc_dead_branch_stores(
        &fixture.project,
        &applied.destination_data_root,
        0,
        0,
    );
    assert!(gc.removed_tracked.is_empty());
    let reloaded = branch_meta::load_branch_meta(&applied.destination_data_root).unwrap();
    for (name, db_file) in recovered {
        assert!(reloaded.branches[&name].gc_protected);
        let path = applied.destination_data_root.join(db_file);
        assert!(path.is_file());
        let expected = if name.contains("orphan-source-") {
            SOURCE_ORPHAN_FACT
        } else {
            TARGET_ORPHAN_FACT
        };
        let (db, _) = test_open_read_only(&path).await;
        let facts = MemoryStore::new(db.conn())
            .list_facts(None, Some(0.0), 100)
            .await
            .unwrap();
        assert!(
            facts.iter().any(|fact| fact.content == expected),
            "recovered branch '{name}' lost its unique fact"
        );
        db.close();
    }
}

#[tokio::test]
async fn corrupt_untracked_branch_database_is_rejected_before_mutation() {
    let fixture = fixture().await;
    let source = layout_for_id(&fixture.project, &fixture.profile, &fixture.source_id).unwrap();
    add_untracked_branch(&source, "corrupt-orphan", "fact in corrupt orphan").await;
    let corrupt = source.data_root.join("branches/corrupt-orphan.db");
    let file = fs::OpenOptions::new().write(true).open(&corrupt).unwrap();
    file.set_len(fs::metadata(&corrupt).unwrap().len() / 2)
        .unwrap();
    drop(file);
    let before = full_tree_snapshot(&fixture.profile);

    let error = plan(&fixture.options()).await.unwrap_err();

    assert!(error.to_string().contains("quick_check"), "{error}");
    assert_eq!(
        full_tree_snapshot(&fixture.profile),
        before,
        "corrupt-input planning mutated the profile"
    );
}

#[tokio::test]
async fn branch_metadata_paths_cannot_escape_the_profile_shard() {
    let fixture = fixture().await;
    let source = layout_for_id(&fixture.project, &fixture.profile, &fixture.source_id).unwrap();
    let outside = fixture.profile.join("outside.db");
    fs::copy(&source.graph_db_path, &outside).unwrap();
    let meta = branch_meta::load_branch_meta(&source.data_root).unwrap();

    for db_file in [
        "../outside.db".to_string(),
        outside.to_string_lossy().to_string(),
    ] {
        let mut invalid = meta.clone();
        invalid.branches.insert(
            "unsafe".to_string(),
            BranchEntry {
                db_file,
                parent: Some(meta.default_branch.clone()),
                created_at: "0".to_string(),
                last_synced_at: "0".to_string(),
                gc_protected: false,
            },
        );
        fs::write(
            &source.branch_meta_path,
            serde_json::to_vec_pretty(&invalid).unwrap(),
        )
        .unwrap();
        let error = plan(&fixture.options()).await.unwrap_err();
        assert!(error.to_string().contains("store-relative path"), "{error}");
    }
}

#[tokio::test]
async fn a_third_matching_shard_is_rejected_as_ambiguous() {
    let fixture = fixture().await;
    create_shard(
        &fixture.profile,
        &fixture.project,
        "proj_third",
        "third fact",
        "third-session",
        false,
    )
    .await;
    let error = plan(&fixture.options()).await.unwrap_err();
    assert!(error.to_string().contains("ambiguous split-store identity"));
    assert!(error.to_string().contains("proj_third"));
}

#[tokio::test]
async fn overlapping_facts_merge_tags_metadata_and_feedback_without_duplication() {
    let fixture = fixture().await;
    add_fact_to_shard(
        &fixture,
        &fixture.source_id,
        "shared fact",
        "source-tag",
        json!({"source_only": true, "winner": "source"}),
        Some(FeedbackAction::Helpful),
    )
    .await;
    add_fact_to_shard(
        &fixture,
        &fixture.target_id,
        "shared fact",
        "target-tag",
        json!({"target_only": true, "winner": "target"}),
        Some(FeedbackAction::Unhelpful),
    )
    .await;

    let options = fixture.options();
    let planned = plan(&options).await.unwrap();
    assert_eq!(planned.collisions.fact_content_overlaps, 1);
    let applied = apply(&options, &planned.confirmation_token).await.unwrap();
    let graph_path = applied
        .destination_data_root
        .join(crate::config::DB_FILENAME);
    let (graph, _) = test_open_read_only(&graph_path).await;
    let store = MemoryStore::new(graph.conn());
    let facts = store.list_facts(None, Some(0.0), 100).await.unwrap();
    let shared = facts
        .iter()
        .find(|fact| fact.content == "shared fact")
        .unwrap();
    assert_eq!(facts.len(), 3);
    assert!(shared.tags.contains(&"source-tag".to_string()));
    assert!(shared.tags.contains(&"target-tag".to_string()));
    assert_eq!(shared.metadata["source_only"], true);
    assert_eq!(shared.metadata["target_only"], true);
    assert_eq!(shared.metadata["winner"], "target");
    assert_eq!(shared.helpful_count, 1);
    assert_eq!(shared.unhelpful_count, 1);
    assert_eq!(
        store
            .fact_trust_history(shared.fact_id)
            .await
            .unwrap()
            .len(),
        2
    );
    graph.close();
}

#[tokio::test]
async fn summary_raw_sources_follow_remapped_store_ids() {
    let fixture = fixture().await;
    let source = layout_for_id(&fixture.project, &fixture.profile, &fixture.source_id).unwrap();
    execute_sql(
        &source.sessions_db_path,
        "INSERT INTO lcm_summary_nodes(
             node_id, provider, conversation_id, session_id, depth, summary_text,
             summary_hash, summary_token_count, source_token_count, created_at
         ) VALUES(
             'source-summary', 'codex', 'source-conversation', 'legacy-session', 1,
             'summary', 'summary-hash', 1, 1, 1800000002
         );
         INSERT INTO lcm_summary_sources(node_id, source_kind, source_id, ordinal)
         SELECT 'source-summary', 'raw_message', CAST(store_id AS TEXT), 0
         FROM lcm_raw_messages WHERE message_id='message-legacy-session';",
    )
    .await;

    let options = fixture.options();
    let planned = plan(&options).await.unwrap();
    let applied = apply(&options, &planned.confirmation_token).await.unwrap();
    let sessions = GlobalDb::open_at_without_structured_backfill(
        &applied
            .destination_data_root
            .join(storage::SESSIONS_DB_FILENAME),
    )
    .await
    .unwrap();
    let mut rows = sessions
        .conn()
        .query(
            "SELECT r.message_id
             FROM lcm_summary_sources s
             JOIN lcm_raw_messages r ON r.store_id=CAST(s.source_id AS INTEGER)
             WHERE s.node_id='source-summary' AND s.source_kind='raw_message'",
            (),
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    assert_eq!(row.get::<String>(0).unwrap(), "message-legacy-session");
    assert!(rows.next().await.unwrap().is_none());
    drop(rows);
    sessions.close();
}

#[tokio::test]
async fn preexisting_destination_without_ledger_is_never_reused() {
    let fixture = fixture().await;
    let options = fixture.options();
    let planned = plan(&options).await.unwrap();
    fs::create_dir_all(&planned.destination_data_root).unwrap();
    fs::write(planned.destination_data_root.join("foreign"), b"foreign").unwrap();

    let error = apply(&options, &planned.confirmation_token)
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("already exists without this migration ledger")
    );
    assert!(!planned.ledger_path.exists());
    assert_eq!(
        storage::read_repository_identity_marker(&fixture.project)
            .unwrap()
            .unwrap()
            .project_id,
        fixture.target_id
    );
}

#[tokio::test]
async fn corrupt_retry_ledger_is_never_overwritten() {
    let fixture = fixture().await;
    let options = fixture.options();
    let planned = plan(&options).await.unwrap();
    fs::create_dir_all(planned.ledger_path.parent().unwrap()).unwrap();
    fs::write(&planned.ledger_path, b"{not-json").unwrap();

    let error = apply(&options, &planned.confirmation_token)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("ledger"));
    assert!(error.to_string().contains("corrupt"));
    assert_eq!(fs::read(&planned.ledger_path).unwrap(), b"{not-json");
    assert!(!planned.destination_data_root.exists());
}

#[test]
fn sqlite_family_backup_includes_wal_and_shm() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source.db");
    let target = temp.path().join("backup/target.db");
    fs::write(&source, b"db").unwrap();
    fs::write(sqlite_sidecar(&source, "-wal"), b"wal").unwrap();
    fs::write(sqlite_sidecar(&source, "-shm"), b"shm").unwrap();

    copy_sqlite_family_exact(&source, &target).unwrap();

    assert_eq!(fs::read(&target).unwrap(), b"db");
    assert_eq!(fs::read(sqlite_sidecar(&target, "-wal")).unwrap(), b"wal");
    assert_eq!(fs::read(sqlite_sidecar(&target, "-shm")).unwrap(), b"shm");
}

#[test]
fn atomic_copy_recovers_an_interrupted_temp_and_reopens_cleanly() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source.bin");
    let target = temp.path().join("backup/target.bin");
    let interrupted = target.with_extension(format!("tmp-{}", std::process::id()));
    fs::create_dir_all(interrupted.parent().unwrap()).unwrap();
    fs::write(&source, b"durable source bytes").unwrap();
    fs::write(&interrupted, b"partial").unwrap();

    copy_file_atomic(&source, &target).unwrap();

    assert_eq!(fs::read(&target).unwrap(), b"durable source bytes");
    assert!(!interrupted.exists());
}

#[test]
fn atomic_copy_preserves_read_only_files_without_losing_durability() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source.bin");
    let target = temp.path().join("backup/target.bin");
    fs::write(&source, b"read-only source bytes").unwrap();
    let original_permissions = fs::metadata(&source).unwrap().permissions();
    let mut permissions = original_permissions.clone();
    permissions.set_readonly(true);
    fs::set_permissions(&source, permissions).unwrap();

    copy_file_atomic(&source, &target).unwrap();
    files::copy_file_exact(&source, &target).unwrap();

    assert_eq!(fs::read(&target).unwrap(), b"read-only source bytes");
    assert!(fs::metadata(&target).unwrap().permissions().readonly());
    for path in [&source, &target] {
        fs::set_permissions(path, original_permissions.clone()).unwrap();
    }
}

#[tokio::test]
async fn current_schema_tables_have_an_explicit_consolidation_disposition() {
    let fixture = fixture().await;
    let graph = fixture
        .profile
        .join("projects")
        .join(&fixture.source_id)
        .join(crate::config::DB_FILENAME);
    let sessions = fixture
        .profile
        .join("projects")
        .join(&fixture.source_id)
        .join(storage::SESSIONS_DB_FILENAME);

    let unknown_graph = unknown_tables(&graph, graph_table_disposition).await;
    let unknown_sessions = unknown_tables(&sessions, session_table_disposition).await;

    assert!(
        unknown_graph.is_empty(),
        "graph schema tables need an explicit consolidation disposition: {unknown_graph:?}"
    );
    assert!(
        unknown_sessions.is_empty(),
        "session schema tables need an explicit consolidation disposition: {unknown_sessions:?}"
    );
}

async fn unknown_tables(path: &Path, classify: fn(&str) -> Option<&'static str>) -> Vec<String> {
    let (db, _) = test_open_read_only(path).await;
    let mut rows = db
        .conn()
        .query(
            "SELECT name FROM sqlite_schema
             WHERE type='table' AND name NOT LIKE 'sqlite_%'
             ORDER BY name",
            (),
        )
        .await
        .unwrap();
    let mut unknown = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        let name = row.get::<String>(0).unwrap();
        if classify(&name).is_none() {
            unknown.push(name);
        }
    }
    db.close();
    unknown
}

fn graph_table_disposition(table: &str) -> Option<&'static str> {
    match table {
        "memory_entities"
        | "memory_fact_entities"
        | "memory_fact_relations"
        | "memory_facts"
        | "memory_feedback_events"
        | "memory_oplog" => Some("merged"),
        "memory_bank_dirty" | "memory_banks" => Some("derived/rebuilt"),
        name if name == "memory_facts_fts" || name.starts_with("memory_facts_fts_") => {
            Some("derived/rebuilt")
        }
        // Code-graph tables are not flattened. Every source and target branch
        // database is copied intact into the destination branch topology.
        "edges" | "files" | "metadata" | "node_fingerprints" | "nodes" | "read_cache"
        | "redundancy_pairs" | "unresolved_refs" | "vectors" => Some("intentionally ignored"),
        name if name == "nodes_fts" || name.starts_with("nodes_fts_") => {
            Some("intentionally ignored")
        }
        _ => None,
    }
}

fn session_table_disposition(table: &str) -> Option<&'static str> {
    match table {
        "analytics_events"
        | "commit_sessions"
        | "dashboard_token_counts"
        | "git_correlation_meta"
        | "lcm_external_payloads"
        | "lcm_gc_marks"
        | "lcm_gc_meta"
        | "lcm_lifecycle_state"
        | "lcm_maintenance_debt"
        | "lcm_raw_messages"
        | "lcm_summary_nodes"
        | "lcm_summary_sources"
        | "parse_offsets"
        | "projects"
        | "savings_ledger"
        | "session_backfill_meta"
        | "session_git_spans"
        | "session_messages"
        | "session_schema_migrations"
        | "sessions"
        | "turns"
        | "workflow_agents"
        | "workflow_index_meta"
        | "workflow_runs" => Some("merged"),
        "code_projects" | "graph_scopes" | "project_aliases" | "store_artifacts"
        | "store_instances" => Some("rejected registry-only"),
        name if name == "lcm_raw_messages_fts"
            || name.starts_with("lcm_raw_messages_fts_")
            || name == "lcm_summary_nodes_fts"
            || name.starts_with("lcm_summary_nodes_fts_")
            || name == "session_messages_fts"
            || name.starts_with("session_messages_fts_") =>
        {
            Some("derived/rebuilt")
        }
        _ => None,
    }
}

async fn fixture() -> Fixture {
    let temp = TempDir::new().unwrap();
    let project = temp.path().join("repo");
    let profile = temp.path().join("profile");
    let source_id = "proj_legacy".to_string();
    let target_id = "proj_current".to_string();
    init_repo(&project);
    create_shard(
        &profile,
        &project,
        &source_id,
        "legacy durable fact",
        "legacy-session",
        true,
    )
    .await;
    create_shard(
        &profile,
        &project,
        &target_id,
        "current durable fact",
        "current-session",
        false,
    )
    .await;
    let global = GlobalDb::open_at_without_structured_backfill(&profile.join("global.db"))
        .await
        .unwrap();
    let git_common_dir = crate::worktree::git_common_dir(&project).unwrap();
    for project_id in [&source_id, &target_id] {
        global
            .upsert_code_project(
                project_id,
                &project,
                Some(&git_common_dir),
                None,
                Some("main"),
            )
            .await
            .unwrap();
        global
            .upsert_store_instance(StoreInstanceUpsert {
                store_id: format!("store:{project_id}:profile_sharded"),
                project_id: project_id.clone(),
                store_kind: "code_project".to_string(),
                storage_mode: "profile_sharded".to_string(),
                store_relpath: format!("projects/{project_id}"),
                manifest_relpath: Some(format!(
                    "projects/{project_id}/{}",
                    storage::STORE_MANIFEST_FILENAME
                )),
                last_verified_at: Some(1_800_000_000),
                last_write_at: Some(1_800_000_000),
            })
            .await
            .unwrap();
    }
    global
        .upsert_project_alias(&project, &target_id)
        .await
        .unwrap();
    global.checkpoint().await;
    global.close();
    storage::write_repository_identity_marker(&project, &target_id).unwrap();
    Fixture {
        _temp: temp,
        project,
        profile,
        source_id,
        target_id,
    }
}

async fn create_shard(
    profile: &Path,
    project: &Path,
    project_id: &str,
    fact_content: &str,
    session_id: &str,
    feedback: bool,
) {
    let layout = layout_for_id(project, profile, project_id).unwrap();
    fs::create_dir_all(&layout.data_root).unwrap();
    let (graph, _) = test_initialize(&layout.graph_db_path).await;
    let memory = MemoryStore::new(graph.conn());
    let outcome = memory
        .add_fact(
            AddFactRequest {
                content: fact_content.to_string(),
                category: MemoryCategory::Project,
                source: Some("consolidation-test".to_string()),
                tags: vec![project_id.to_string()],
                entities: vec!["TraceDecay".to_string()],
                trust: Some(0.8),
                metadata: json!({"project_id": project_id}),
            },
            0.5,
        )
        .await
        .unwrap();
    if feedback {
        memory
            .record_feedback_event(FeedbackRequest {
                fact_id: outcome.fact.unwrap().fact_id,
                action: FeedbackAction::Helpful,
                source: Some("consolidation-test".to_string()),
                note: Some("verified".to_string()),
            })
            .await
            .unwrap();
    }
    graph.checkpoint().await.unwrap();
    graph.close();

    let sessions = GlobalDb::open_at_without_structured_backfill(&layout.sessions_db_path)
        .await
        .unwrap();
    assert!(
        sessions
            .upsert_session(&SessionRecord {
                provider: "codex".to_string(),
                session_id: session_id.to_string(),
                project_key: project_id.to_string(),
                project_path: project.to_string_lossy().to_string(),
                title: Some(session_id.to_string()),
                started_at: Some(1_800_000_000),
                ended_at: Some(1_800_000_001),
                transcript_path: None,
                metadata_json: None,
                parent_session_id: None,
                is_subagent: false,
                agent_id: None,
                parent_tool_use_id: None,
            })
            .await
    );
    assert!(
        sessions
            .upsert_session_message(&SessionMessageRecord {
                provider: "codex".to_string(),
                message_id: format!("message-{session_id}"),
                session_id: session_id.to_string(),
                role: "user".to_string(),
                timestamp: Some(1_800_000_000),
                ordinal: 0,
                text: format!("message from {session_id}"),
                kind: Some("message".to_string()),
                model: None,
                tool_names: None,
                source_path: None,
                source_offset: None,
                metadata_json: None,
            })
            .await
    );
    sessions.checkpoint().await;
    sessions.close();

    branch_meta::save_branch_meta(&layout.data_root, &BranchMeta::new("main")).unwrap();
    fs::create_dir_all(layout.data_root.join("lcm-payloads")).unwrap();
    let payload_name = if feedback { "source.txt" } else { "target.txt" };
    fs::write(
        layout.data_root.join("lcm-payloads").join(payload_name),
        session_id,
    )
    .unwrap();
    storage::write_store_manifest(&layout).unwrap();
}

async fn add_fact_to_shard(
    fixture: &Fixture,
    project_id: &str,
    content: &str,
    tag: &str,
    metadata: serde_json::Value,
    feedback: Option<FeedbackAction>,
) {
    let layout = layout_for_id(&fixture.project, &fixture.profile, project_id).unwrap();
    let (graph, _) = test_open(&layout.graph_db_path).await;
    let memory = MemoryStore::new(graph.conn());
    let outcome = memory
        .add_fact(
            AddFactRequest {
                content: content.to_string(),
                category: MemoryCategory::Project,
                source: Some(project_id.to_string()),
                tags: vec![tag.to_string()],
                entities: vec!["TraceDecay".to_string()],
                trust: Some(0.8),
                metadata,
            },
            0.5,
        )
        .await
        .unwrap();
    if let Some(action) = feedback {
        memory
            .record_feedback_event(FeedbackRequest {
                fact_id: outcome.fact.unwrap().fact_id,
                action,
                source: Some(project_id.to_string()),
                note: Some("overlap".to_string()),
            })
            .await
            .unwrap();
    }
    graph.checkpoint().await.unwrap();
    graph.close();
}

async fn add_fact_relation_to_shard(fixture: &Fixture, project_id: &str) {
    let layout = layout_for_id(&fixture.project, &fixture.profile, project_id).unwrap();
    let (graph, _) = test_open(&layout.graph_db_path).await;
    let memory = MemoryStore::new(graph.conn());
    let source_fact_id = memory
        .list_facts(None, Some(0.0), 10)
        .await
        .unwrap()
        .into_iter()
        .next()
        .expect("fixture source fact")
        .fact_id;
    let target_fact_id = memory
        .add_fact(
            AddFactRequest {
                content: "relation target fact".to_string(),
                category: MemoryCategory::Project,
                source: Some("consolidation-test".to_string()),
                tags: vec!["relation".to_string()],
                entities: vec!["TraceDecay".to_string()],
                trust: Some(0.75),
                metadata: json!({"project_id": project_id}),
            },
            0.5,
        )
        .await
        .unwrap()
        .fact
        .expect("relation target fact should be stored")
        .fact_id;
    memory
        .upsert_fact_relation(
            source_fact_id,
            target_fact_id,
            FactRelationKind::Supports,
            0.9,
            "consolidation-test",
            json!({"evidence": "fixture"}),
        )
        .await
        .unwrap();
    graph.checkpoint().await.unwrap();
    graph.close();
}

fn add_branch_links(fixture: &Fixture, project_id: &str, count: usize) {
    let layout = layout_for_id(&fixture.project, &fixture.profile, project_id).unwrap();
    let mut meta = branch_meta::load_branch_meta(&layout.data_root).unwrap();
    let branches = layout.data_root.join("branches");
    fs::create_dir_all(&branches).unwrap();
    for index in 0..count {
        let name = format!("load-{index:03}");
        let relative = format!("branches/load-{index:03}.db");
        fs::copy(&layout.graph_db_path, layout.data_root.join(&relative)).unwrap();
        meta.add_branch(&name, &relative, "main");
    }
    branch_meta::save_branch_meta(&layout.data_root, &meta).unwrap();
}

async fn add_untracked_branch(layout: &StoreLayout, name: &str, fact_content: &str) {
    let branches = layout.data_root.join("branches");
    fs::create_dir_all(&branches).unwrap();
    let path = branches.join(format!("{name}.db"));
    fs::copy(&layout.graph_db_path, &path).unwrap();
    let (db, _) = test_open(&path).await;
    MemoryStore::new(db.conn())
        .add_fact(
            AddFactRequest {
                content: fact_content.to_string(),
                category: MemoryCategory::Project,
                source: Some("untracked-branch-test".to_string()),
                tags: vec![name.to_string()],
                entities: vec!["TraceDecay".to_string()],
                trust: Some(0.8),
                metadata: json!({"branch": name}),
            },
            0.5,
        )
        .await
        .unwrap();
    db.checkpoint().await.unwrap();
    db.close();
}

fn sqlite_family_bytes(path: &Path) -> u64 {
    [
        path.to_path_buf(),
        sqlite_sidecar(path, "-wal"),
        sqlite_sidecar(path, "-shm"),
    ]
    .into_iter()
    .filter_map(|member| fs::metadata(member).ok())
    .map(|metadata| metadata.len())
    .sum()
}

async fn execute_sql(path: &Path, sql: &str) {
    let (db, _) = test_open(path).await;
    db.conn().execute_batch(sql).await.unwrap();
    db.checkpoint().await.unwrap();
    db.close();
}

async fn rewrite_page_size(path: &Path, page_size: i64) {
    let (db, _) = test_open(path).await;
    db.checkpoint().await.unwrap();
    db.conn()
        .execute_batch(&format!(
            "PRAGMA journal_mode = DELETE; PRAGMA page_size = {page_size}; VACUUM;"
        ))
        .await
        .unwrap();
    db.close();
}

async fn database_page_size(path: &Path) -> i64 {
    let (db, _) = test_open_read_only(path).await;
    let mut rows = db.conn().query("PRAGMA page_size", ()).await.unwrap();
    let page_size = rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap();
    db.close();
    page_size
}

async fn explain_query_plan(conn: &libsql::Connection, sql: &str) -> Vec<String> {
    let mut rows = conn
        .query(&format!("EXPLAIN QUERY PLAN {sql}"), ())
        .await
        .unwrap();
    let mut details = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        details.push(row.get::<String>(3).unwrap());
    }
    details
}

fn init_repo(path: &Path) {
    fs::create_dir_all(path).unwrap();
    run_git(path, &["init"]);
    run_git(path, &["config", "user.email", "test@example.com"]);
    run_git(path, &["config", "user.name", "TraceDecay Test"]);
    fs::write(path.join("lib.rs"), "pub fn fixture() {}\n").unwrap();
    run_git(path, &["add", "."]);
    run_git(path, &["commit", "-m", "fixture"]);
}

fn run_git(path: &Path, args: &[&str]) {
    let status = Command::new(crate::git::git_program())
        .args(args)
        .current_dir(path)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} failed");
}
