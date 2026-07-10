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
use crate::db::Database;
use crate::memory::store::MemoryStore;
use crate::memory::types::{AddFactRequest, FeedbackAction, FeedbackRequest, MemoryCategory};
use crate::sessions::{SessionMessageRecord, SessionRecord};
use crate::tracedecay::{TraceDecay, TraceDecayOpenOptions};

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
    Directory {
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
        if metadata.is_dir() {
            #[cfg(unix)]
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            snapshot.insert(
                relative,
                TreeSnapshotEntry::Directory {
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
    assert!(!fixture.profile.join("lifecycle.lock").exists());
    assert_eq!(
        storage::read_repository_identity_marker(&fixture.project)
            .unwrap()
            .unwrap()
            .project_id,
        fixture.target_id
    );
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
    assert_eq!(sqlite::count_rows(&graph, "memory_facts").await.unwrap(), 2);
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
    assert_eq!(sqlite::count_rows(&graph, "memory_facts").await.unwrap(), 2);
    assert_eq!(sqlite::count_rows(&sessions, "sessions").await.unwrap(), 2);
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
    let (graph, _) = Database::open(&graph_path).await.unwrap();
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
async fn identity_survives_symlink_and_repository_move() {
    let fixture = fixture().await;
    let symlink = fixture.project.parent().unwrap().join("repo-symlink");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&fixture.project, &symlink).unwrap();
    #[cfg(windows)]
    std::os::windows::fs::symlink_dir(&fixture.project, &symlink).unwrap();
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
    let (graph, _) = Database::open_read_only(&graph_path).await.unwrap();
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
    let sessions = GlobalDb::open_at(
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
    let (db, _) = Database::open_read_only(path).await.unwrap();
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
    let (graph, _) = Database::initialize(&layout.graph_db_path).await.unwrap();
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

    let sessions = GlobalDb::open_at(&layout.sessions_db_path).await.unwrap();
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
    let (graph, _) = Database::open(&layout.graph_db_path).await.unwrap();
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

fn add_branch_links(fixture: &Fixture, project_id: &str, count: usize) {
    let layout = layout_for_id(&fixture.project, &fixture.profile, project_id).unwrap();
    let mut meta = branch_meta::load_branch_meta(&layout.data_root).unwrap();
    let branches = layout.data_root.join("branches");
    fs::create_dir_all(&branches).unwrap();
    for index in 0..count {
        let name = format!("load-{index:03}");
        let relative = format!("branches/load-{index:03}.db");
        fs::hard_link(&layout.graph_db_path, layout.data_root.join(&relative)).unwrap();
        meta.add_branch(&name, &relative, "main");
    }
    branch_meta::save_branch_meta(&layout.data_root, &meta).unwrap();
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
    let (db, _) = Database::open(path).await.unwrap();
    db.conn().execute_batch(sql).await.unwrap();
    db.checkpoint().await.unwrap();
    db.close();
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
