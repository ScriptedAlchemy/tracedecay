use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use super::fence::{capture_store_content_fence, capture_store_directory_fence};
use super::pages::walk_store_stats;
#[cfg(windows)]
use super::quarantine::classify_recovery_journal_probe;
use super::quarantine::{
    DurableDatabaseInventoryV1, DurableMemoryCheck, PendingQuarantineReceiptV1,
    QuarantineFinalizeOutcome, QuarantineKindV1, QuarantineRecoveryOutcome,
    QuarantineRegistryFenceV1, QuarantineStoreOutcome, RegisteredQuarantineDecisionV1,
    RegisteredQuarantineInventoryV1, RegisteredQuarantineRegistryStateV1,
    check_store_durable_memory, durable_check_scratch_root, durable_database_inventory,
    quarantine_candidate_namespace_available, quarantine_store_for_verified_collection,
    quarantine_store_for_verified_collection_controlled,
    read_registered_quarantine_intents_controlled, reconcile_existing_quarantine,
    reconcile_registered_quarantine_inventory_with_classified_hook,
    recover_existing_store_quarantine, recover_named_store_quarantine,
    recover_named_store_quarantine_controlled, recover_registered_quarantine_intent_controlled,
    reserve_quarantine_name_with_sequence,
};
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

/// Same shape as the production convenience caller
/// (`unbounded_collection_control`): a far-future monotonic deadline so
/// functional sweep assertions do not race a one-second wall clock.
fn functional_sweep_deadline() -> MonotonicDeadline {
    MonotonicDeadline::at(Instant::now() + Duration::from_hours(24))
}

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

mod pages;
mod quarantine;

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

#[tokio::test]
async fn empty_plan_retains_registered_live_source_when_exact_row_is_absent() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let (_runtime, db) = open_registered_db(&profile_root).await;
    let payload = b"absent row cannot authorize deleting contradictory live bytes";
    let (data_root, quarantine_path) = prepare_registered_quarantine(
        &db,
        &profile_root,
        "proj_registered_live_absent",
        "store_registered_live_absent",
        payload,
    )
    .await;
    let expected = capture_store_content_fence(&profile_root, &quarantine_path).unwrap();
    let StoreContentFence::Present(expected_inventory) = &expected else {
        panic!("fixture must capture an exact present-store fence");
    };
    let expected_root_identity = expected_inventory.root.clone();
    std::fs::rename(&quarantine_path, &data_root).unwrap();
    let transaction = db.begin_write_transaction().await.unwrap();
    assert_eq!(
        transaction
            .execute(
                "DELETE FROM store_instances WHERE store_id = ?1",
                tracedecay_runtime_core::db::engine::params!["store_registered_live_absent"],
            )
            .await
            .unwrap(),
        1
    );
    transaction.commit().await.unwrap();

    let (outcome, retired) =
        execute_registered_collection(&db, &CollectionPlan::default(), &profile_root)
            .await
            .unwrap();

    assert_eq!(retired, 0);
    assert_eq!(
        outcome,
        CollectionOutcome {
            errors: vec![CollectionFailure {
                store_id: "store_registered_live_absent".to_owned(),
                kind: CollectionFailureKind::RemoveFailed(CollectionMutationFailure {
                    operation: CollectionMutationOperation::ValidateRestoredStoreIdentity,
                    raw_os_error: None,
                    target_path: data_root.clone(),
                    expected_root_identity: Some(expected_root_identity),
                    classification: CollectionMutationFailureClassification::NonRetryable,
                }),
            }],
            recovery_receipts: vec![CollectionRecoveryReceipt {
                store_id: "store_registered_live_absent".to_owned(),
                original_path: data_root.clone(),
                quarantine_path: quarantine_path.clone(),
                actual_path: data_root.clone(),
                action: CollectionRecoveryAction::RetainedForRecovery,
            }],
            ..CollectionOutcome::default()
        }
    );
    assert_eq!(
        std::fs::read(data_root.join("payload.bin")).unwrap(),
        payload
    );
    assert_eq!(
        capture_store_content_fence(&profile_root, &data_root).unwrap(),
        expected
    );
    assert!(!quarantine_path.exists());
    assert_eq!(
        read_pending_quarantine_receipts(&profile_root).unwrap(),
        vec![PendingQuarantineReceiptV1 {
            quarantine_path: quarantine_path.clone(),
            actual_path: data_root,
            retirement_committed: false,
        }]
    );
}

#[test]
fn portable_inventory_other_profiles_progress_while_one_writer_is_paused() {
    let tmp = tempfile::TempDir::new().unwrap();
    let first_profile = tmp.path().join("first");
    let second_profile = tmp.path().join("second");
    for profile in [&first_profile, &second_profile] {
        std::fs::create_dir_all(profile.join("projects/proj_independent")).unwrap();
    }
    let first_projects = first_profile.join("projects");
    let signature =
        super::unregistered_page::portable_directory_signature(&first_projects).unwrap();
    let inventory = super::unregistered_page::portable_inventory_path(&first_profile, &signature);
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let first = std::thread::spawn(move || {
        let paused = AtomicBool::new(false);
        let mut scanned = 0;
        super::unregistered_page::advance_portable_inventory(
            &first_projects,
            &inventory,
            &signature,
            1,
            &mut scanned,
            &|| {
                if !paused.swap(true, Ordering::SeqCst) {
                    entered_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                }
                false
            },
        )
    });
    entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    let (finished_tx, finished_rx) = std::sync::mpsc::channel();
    let second = std::thread::spawn(move || {
        let result = super::unregistered_page::read_project_directory_page(
            &second_profile,
            None,
            1,
            &|| false,
        );
        finished_tx.send(result).unwrap();
    });
    let independent = finished_rx.recv_timeout(Duration::from_secs(1));
    // Always release and join the stalled writer before asserting so the
    // regression cannot strand a process-global lock on its failure path.
    release_tx.send(()).unwrap();
    assert_eq!(first.join().unwrap().unwrap(), Some(false));
    second.join().unwrap();
    let page = independent
        .expect("another profile must progress before the paused writer is released")
        .unwrap()
        .unwrap();
    assert!(matches!(
        page.entries.as_slice(),
        [super::unregistered_page::ProjectDirectoryWorkV1::Project(name)]
            if name == "proj_independent"
    ));
}

/// Every platform uses an append-only durable inventory. A cancelled admission
/// keeps its partial inventory, and
/// the next page advances that exact log instead of deleting/rebuilding it.
#[test]
fn unregistered_inventory_hydrates_another_writers_committed_suffix() {
    use std::io::Write;

    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path();
    for index in 0..17 {
        std::fs::create_dir_all(
            profile_root
                .join("projects")
                .join(format!("proj_writer_{index}")),
        )
        .unwrap();
    }
    let first =
        super::unregistered_page::read_project_directory_page(profile_root, None, 1, &|| false)
            .unwrap()
            .unwrap();
    let cursor = first.next_cursor.unwrap();
    let inventory = super::unregistered_page::portable_inventory_path(
        profile_root,
        cursor.split(':').nth(1).unwrap(),
    );
    let initial = std::fs::read_to_string(&inventory).unwrap();
    let foreign = (0..17)
        .map(|index| format!("proj_writer_{index}"))
        .find(|name| !initial.lines().any(|line| line == name))
        .unwrap();
    // Reproduce another process's durable append between writer admissions.
    let lock = tracedecay_runtime_core::storage::try_acquire_sidecar_lock(
        &tracedecay_runtime_core::storage::append_lock_path(&inventory),
    )
    .unwrap()
    .unwrap();
    let mut writer = std::fs::OpenOptions::new()
        .append(true)
        .open(&inventory)
        .unwrap();
    writeln!(writer, "{foreign}").unwrap();
    writer.sync_data().unwrap();
    drop(writer);
    drop(lock);
    let mut cursor = Some(cursor);
    let mut pages = 0;
    while let Some(saved) = cursor {
        let page = super::unregistered_page::read_project_directory_page(
            profile_root,
            Some(&saved),
            1,
            &|| false,
        )
        .unwrap()
        .unwrap();
        cursor = page.next_cursor;
        pages += 1;
        assert!(pages <= 18);
    }
    let log = std::fs::read_to_string(&inventory).unwrap();
    let records = log.lines().skip(1).collect::<Vec<_>>();
    assert_eq!(records.len(), 17);
    assert_eq!(
        records
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len(),
        17
    );
    assert_eq!(records.iter().filter(|name| **name == foreign).count(), 1);
}

#[test]
fn unregistered_inventory_restart_converges_without_repeating_records() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path();
    let count = 73;
    for index in 0..count {
        std::fs::create_dir_all(
            profile_root
                .join("projects")
                .join(format!("proj_restart_{index}")),
        )
        .unwrap();
    }
    let first =
        super::unregistered_page::read_project_directory_page(profile_root, None, 1, &|| false)
            .unwrap()
            .unwrap();
    let mut scanned = first.entries_scanned;
    let mut observed = std::collections::HashSet::new();
    for entry in first.entries {
        let super::unregistered_page::ProjectDirectoryWorkV1::Project(name) = entry else {
            panic!("unexpected quarantine")
        };
        assert!(observed.insert(name));
    }
    let saved = first.next_cursor.unwrap();
    let inventory = super::unregistered_page::portable_inventory_path(
        profile_root,
        saved.split(':').nth(1).unwrap(),
    );
    super::unregistered_page::forget_portable_inventory_builder_for_test(&inventory);
    let mut cursor = Some(saved);
    let mut pages = 0;
    while let Some(saved) = cursor {
        let page = super::unregistered_page::read_project_directory_page(
            profile_root,
            Some(&saved),
            1,
            &|| false,
        )
        .unwrap()
        .unwrap();
        scanned += page.entries_scanned;
        for entry in page.entries {
            let super::unregistered_page::ProjectDirectoryWorkV1::Project(name) = entry else {
                panic!("unexpected quarantine")
            };
            assert!(observed.insert(name), "restart repeated a committed record");
        }
        cursor = page.next_cursor;
        pages += 1;
        assert!(
            pages <= count + 3,
            "restart failed to converge within bounded hydration and replay"
        );
    }
    assert_eq!(observed.len(), count);
    // Directory + inventory reads cost 2N; the one restart hydrates and
    // replays only the eight records persisted by the first admission.
    assert_eq!(scanned, count * 2 + 16);
}

/// A crash while a first inventory header is being published must not turn the
/// cursor into a permanent configuration error. The next admission replaces
/// the uncommitted header before it recreates bounded inventory progress.
#[test]
fn portable_inventory_repairs_torn_header_before_restart_resume() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    for index in 0..16 {
        std::fs::create_dir_all(
            profile_root
                .join("projects")
                .join(format!("proj_header_recovery_{index}")),
        )
        .unwrap();
    }
    let cancellation = CancellationToken::new();
    let deadline = MonotonicDeadline::at(Instant::now() + Duration::from_secs(1));
    let interrupted = || cancellation.is_cancelled() || deadline.is_elapsed_at(Instant::now());
    let page =
        super::unregistered_page::read_project_directory_page(&profile_root, None, 1, &interrupted)
            .unwrap()
            .expect("first bounded page creates a resumable inventory");
    let cursor = page
        .next_cursor
        .expect("the first source slice remains incomplete");
    let inventory_path = super::unregistered_page::portable_inventory_path(
        &profile_root,
        cursor.split(':').nth(1).unwrap(),
    );
    std::fs::write(&inventory_path, b"v2:").unwrap();
    super::unregistered_page::forget_portable_inventory_builder_for_test(&inventory_path);

    let resumed = super::unregistered_page::read_project_directory_page(
        &profile_root,
        Some(&cursor),
        1,
        &interrupted,
    )
    .unwrap()
    .expect("a torn header is replaced before restart resume");

    assert_eq!(resumed.entries.len(), 1);
    assert!(resumed.next_cursor.is_some());
    let signature = cursor.split(':').nth(1).unwrap();
    assert!(
        std::fs::read(&inventory_path)
            .unwrap()
            .starts_with(format!("v2:{signature}\n").as_bytes()),
        "the recovered inventory must have a complete published header"
    );
}

/// A final append is committed only by its newline. After a restart, an
/// unterminated project id is discarded before hydration, so it cannot be
/// joined with a later append and hide the real project from the page.
#[test]
fn portable_inventory_truncates_torn_final_entry_before_restart_resume() {
    use std::io::Write;

    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    let project_ids = (0..32)
        .map(|index| format!("proj_torn_tail_{index}"))
        .collect::<Vec<_>>();
    for project_id in &project_ids {
        std::fs::create_dir_all(profile_root.join("projects").join(project_id)).unwrap();
    }
    let cancellation = CancellationToken::new();
    let deadline = functional_sweep_deadline();
    let interrupted = || cancellation.is_cancelled() || deadline.is_elapsed_at(Instant::now());
    let page =
        super::unregistered_page::read_project_directory_page(&profile_root, None, 1, &interrupted)
            .unwrap()
            .expect("first bounded page creates partial inventory");
    let cursor = page
        .next_cursor
        .expect("the inventory has unscanned source entries");
    let inventory_path = super::unregistered_page::portable_inventory_path(
        &profile_root,
        cursor.split(':').nth(1).unwrap(),
    );
    let inventory_before_torn_append = String::from_utf8(std::fs::read(&inventory_path).unwrap())
        .expect("the production inventory is UTF-8");
    let target = project_ids
        .iter()
        .find(|project_id| {
            !inventory_before_torn_append
                .lines()
                .skip(1)
                .any(|recorded| recorded == project_id.as_str())
        })
        .expect("the first bounded source slice does not contain every project")
        .clone();
    let torn = target[..target.len() - 1].to_owned();
    assert!(tracedecay_runtime_core::storage::validate_project_id(&torn).is_ok());
    let mut output = std::fs::OpenOptions::new()
        .append(true)
        .open(&inventory_path)
        .unwrap();
    output.write_all(torn.as_bytes()).unwrap();
    output.sync_data().unwrap();
    drop(output);
    super::unregistered_page::forget_portable_inventory_builder_for_test(&inventory_path);

    let _ = super::unregistered_page::read_project_directory_page(
        &profile_root,
        Some(&cursor),
        64,
        &interrupted,
    )
    .unwrap()
    .expect("restart resumes after trimming the torn final record");

    let recovered = String::from_utf8(std::fs::read(&inventory_path).unwrap()).unwrap();
    assert!(
        recovered
            .lines()
            .any(|recorded| recorded == target.as_str()),
        "the real project must be re-appended as its own record"
    );
    assert!(
        !recovered.contains(&format!("{torn}{target}")),
        "a torn record must never be joined with the subsequent append"
    );
    assert!(recovered.ends_with('\n'));
}

/// An already-elapsed deadline must not report a successful resume page.
/// Tail recovery may still truncate the unterminated suffix; the real
/// project is not fabricated onto the log.
#[test]
fn portable_inventory_elapsed_deadline_does_not_resume_torn_final_entry() {
    use std::io::Write;

    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    let project_ids = (0..32)
        .map(|index| format!("proj_torn_deadline_{index}"))
        .collect::<Vec<_>>();
    for project_id in &project_ids {
        std::fs::create_dir_all(profile_root.join("projects").join(project_id)).unwrap();
    }
    let cancellation = CancellationToken::new();
    let deadline = functional_sweep_deadline();
    let interrupted = || cancellation.is_cancelled() || deadline.is_elapsed_at(Instant::now());
    let page =
        super::unregistered_page::read_project_directory_page(&profile_root, None, 1, &interrupted)
            .unwrap()
            .expect("first bounded page creates partial inventory");
    let cursor = page
        .next_cursor
        .expect("the inventory has unscanned source entries");
    let inventory_path = super::unregistered_page::portable_inventory_path(
        &profile_root,
        cursor.split(':').nth(1).unwrap(),
    );
    let inventory_before_torn_append = String::from_utf8(std::fs::read(&inventory_path).unwrap())
        .expect("the production inventory is UTF-8");
    let target = project_ids
        .iter()
        .find(|project_id| {
            !inventory_before_torn_append
                .lines()
                .skip(1)
                .any(|recorded| recorded == project_id.as_str())
        })
        .expect("the first bounded source slice does not contain every project")
        .clone();
    let torn = target[..target.len() - 1].to_owned();
    assert!(tracedecay_runtime_core::storage::validate_project_id(&torn).is_ok());
    let mut output = std::fs::OpenOptions::new()
        .append(true)
        .open(&inventory_path)
        .unwrap();
    output.write_all(torn.as_bytes()).unwrap();
    output.sync_data().unwrap();
    drop(output);
    super::unregistered_page::forget_portable_inventory_builder_for_test(&inventory_path);

    let expired = MonotonicDeadline::at(Instant::now());
    let interrupted = || expired.is_elapsed_at(Instant::now());
    assert!(
        super::unregistered_page::read_project_directory_page(
            &profile_root,
            Some(&cursor),
            64,
            &interrupted,
        )
        .unwrap()
        .is_none(),
        "an already-elapsed deadline must not report a resumed page"
    );
    let recovered = String::from_utf8(std::fs::read(&inventory_path).unwrap()).unwrap();
    assert_eq!(
        recovered, inventory_before_torn_append,
        "interruption after tail recovery must leave only committed records"
    );
    assert!(
        !recovered
            .lines()
            .any(|recorded| recorded == target.as_str()),
        "interruption must not fabricate the repaired project record"
    );
    assert!(
        !recovered.contains(&format!("{torn}{target}")),
        "a torn record must never be joined with the subsequent append"
    );
}

/// The canonical sidecar writer lock is process-safe, rather than merely the
/// in-process builder map. A competing admission yields without touching the
/// log and a later admission resumes from the same durable boundary.
#[test]
fn portable_inventory_sidecar_writer_lock_serializes_concurrent_advances() {
    let tmp = tempfile::TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    let projects_dir = profile_root.join("projects");
    for index in 0..4 {
        std::fs::create_dir_all(projects_dir.join(format!("proj_writer_lock_{index}"))).unwrap();
    }
    let signature = super::unregistered_page::portable_directory_signature(&projects_dir).unwrap();
    let inventory = super::unregistered_page::portable_inventory_path(&profile_root, &signature);
    std::fs::create_dir_all(inventory.parent().unwrap()).unwrap();

    let writer_lock = tracedecay_runtime_core::storage::acquire_sidecar_lock_blocking(
        &tracedecay_runtime_core::storage::append_lock_path(&inventory),
    )
    .unwrap();
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let worker_profile_root = profile_root.clone();
    let worker = std::thread::spawn(move || {
        let cancellation = CancellationToken::new();
        let deadline = MonotonicDeadline::at(Instant::now() + Duration::from_secs(1));
        let interrupted = || cancellation.is_cancelled() || deadline.is_elapsed_at(Instant::now());
        started_tx.send(()).unwrap();
        super::unregistered_page::read_project_directory_page(
            &worker_profile_root,
            None,
            1,
            &interrupted,
        )
    });
    started_rx.recv().unwrap();
    let page = worker
        .join()
        .unwrap()
        .unwrap()
        .expect("a contending writer must return an incomplete retry page");
    assert!(page.entries.is_empty());
    assert_eq!(
        page.next_cursor,
        Some(format!("portable-v2:{signature}:0")),
        "a second process-equivalent writer must yield with an opaque retry cursor"
    );
    drop(writer_lock);

    let mut entries_scanned = 0usize;
    assert_eq!(
        super::unregistered_page::advance_portable_inventory(
            &projects_dir,
            &inventory,
            &signature,
            8,
            &mut entries_scanned,
            &|| false,
        )
        .unwrap(),
        Some(true)
    );
    let records = String::from_utf8(std::fs::read(&inventory).unwrap()).unwrap();
    assert!(records.ends_with('\n'));
    assert!(
        records
            .lines()
            .skip(1)
            .all(super::unregistered_page::portable_inventory_entry_is_valid)
    );
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

async fn prepare_registered_quarantine(
    db: &RegisteredGlobalDb,
    profile_root: &Path,
    project_id: &str,
    store_id: &str,
    payload: &[u8],
) -> (PathBuf, PathBuf) {
    let data_root = seed_store(
        db,
        profile_root,
        project_id,
        store_id,
        &profile_root.join("missing-project-root"),
        1_700_000_000,
    )
    .await;
    std::fs::write(data_root.join("payload.bin"), payload).unwrap();
    let row = db
        .try_list_store_instances_for_project(project_id)
        .await
        .unwrap()
        .into_iter()
        .find(|row| row.store_id == store_id)
        .unwrap();
    let expected = capture_store_content_fence(profile_root, &data_root).unwrap();
    let quarantine = quarantine_store_for_verified_collection_controlled(
        profile_root,
        &data_root,
        &expected,
        QuarantineKindV1::Registered,
        project_id,
        store_id,
        Some(QuarantineRegistryFenceV1 {
            store_relpath: row.store_relpath,
            created_at: row.created_at,
            last_write_at: row.last_write_at,
        }),
        unbounded_collection_control(),
    )
    .unwrap();
    let QuarantineStoreOutcome::Verified(quarantine) = quarantine else {
        panic!("fixture must reach a verified registered quarantine");
    };
    let quarantine_path = quarantine.quarantine_path().to_path_buf();
    drop(quarantine);
    (data_root, quarantine_path)
}
