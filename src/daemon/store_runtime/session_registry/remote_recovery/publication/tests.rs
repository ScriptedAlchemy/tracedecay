use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use tracedecay_runtime_core::storage::{
    DurableAtomicWriteFaultForTest, with_durable_atomic_write_fault_for_test,
};

use super::mounted_identity::validate_existing_mounted_identity;
use super::quarantine::{
    activate_remote_restore_quarantine, activated_fence_matches_preserved_authority,
    complete_remote_restore_quarantine, install_remote_restore_quarantine,
    lock_project_sessions_for_replacement, reject_unbound_retained_rollback,
    remote_restore_quarantine_active, remote_restore_quarantine_blocks_open,
};
use super::{
    FailedPublicationDispositionV1, RestorePublicationV1, completed_remote_restore,
    failed_publication_disposition, mark_remote_restore_rollback_required,
    quarantine_sqlite_sidecars, read_remote_restore_quarantine,
    remote_restore_activated_open_identity, restore_retained_rollback_over_unverified_destination,
    retain_interrupted_rollback, rollback_required, sqlite_sidecar,
    validate_completed_remote_restore,
};

fn sqlite_with_marker(path: &Path, marker: &str) {
    let connection = rusqlite::Connection::open(path).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE marker (value TEXT NOT NULL);
             CREATE TABLE observations (id INTEGER PRIMARY KEY);
             CREATE TABLE remote_observation_events (id INTEGER PRIMARY KEY);
             CREATE TABLE remote_writer_fences (id INTEGER PRIMARY KEY);",
        )
        .unwrap();
    connection
        .execute("INSERT INTO marker (value) VALUES (?1)", [marker])
        .unwrap();
}

fn marker(path: &Path) -> String {
    rusqlite::Connection::open(path)
        .unwrap()
        .query_row("SELECT value FROM marker", (), |row| row.get(0))
        .unwrap()
}

#[tokio::test]
async fn stale_mounted_runtime_rejects_the_replacement_identity() {
    let harness = crate::global_db::tests::harness::RegisteredGlobalDbHarness::open(
        "remote-restore-stale-mounted-runtime",
    )
    .await;
    let mounted_identity = harness
        .registered
        .runtime()
        .opened_file_identity()
        .expect("mounted runtime identity");
    let replacement_root = tempfile::tempdir().expect("replacement database root");
    let replacement = replacement_root.path().join("replacement.sqlite3");
    sqlite_with_marker(&replacement, "replacement");
    let replacement_identity =
        tracedecay_runtime_core::db::sqlite_generation_identity(&replacement)
            .expect("replacement identity");
    assert_ne!(mounted_identity, replacement_identity);

    validate_existing_mounted_identity(
        &harness.registered,
        &harness.registered.binding().shard_id,
        harness.registered.binding().incarnation,
        mounted_identity,
        harness.registered.db_path(),
    )
    .expect("the exact mounted identity remains reusable");
    validate_existing_mounted_identity(
        &harness.registered,
        &harness.registered.binding().shard_id,
        harness.registered.binding().incarnation,
        mounted_identity,
        &replacement,
    )
    .expect_err("the same opened inode cannot satisfy a foreign destination binding");
    let error = validate_existing_mounted_identity(
        &harness.registered,
        &harness.registered.binding().shard_id,
        harness.registered.binding().incarnation,
        replacement_identity,
        harness.registered.db_path(),
    )
    .expect_err("a stale mounted inode cannot satisfy replacement convergence");
    assert!(error.to_string().contains("mounted identity"));
}

#[tokio::test]
async fn empty_mount_gate_excludes_an_old_inode_mount_until_replacement_finishes() {
    let mounted = Arc::new(tokio::sync::Mutex::new(BTreeMap::new()));
    let replacement_guard = lock_project_sessions_for_replacement(&mounted).await;
    let started = Arc::new(tokio::sync::Notify::new());
    let contender = tokio::spawn({
        let mounted = Arc::clone(&mounted);
        let started = Arc::clone(&started);
        async move {
            started.notify_one();
            let _old_inode_mount = mounted.lock().await;
        }
    });
    started.notified().await;
    tokio::task::yield_now().await;
    assert!(
        !contender.is_finished(),
        "an empty-map mount must remain excluded throughout physical replacement"
    );

    drop(replacement_guard);
    contender
        .await
        .expect("excluded mount resumes after replacement");
}

#[test]
fn unbound_retained_rollback_is_never_adopted_as_authority() {
    let temporary = tempfile::tempdir().expect("unbound rollback fixture");
    let rollback = temporary.path().join("sessions.remote-restore.rollback");
    sqlite_with_marker(&rollback, "schema-valid-foreign");

    let error = reject_unbound_retained_rollback(&rollback)
        .expect_err("schema validity cannot replace durable pre-publication identity proof");
    assert!(
        error
            .to_string()
            .contains("no matching durable pre-publication fence")
    );
}

#[test]
fn failed_publication_remounts_only_exact_old_or_verified_new_identity() {
    assert_eq!(
        failed_publication_disposition(Some(11), 11, 22),
        FailedPublicationDispositionV1::RemountRolledBack
    );
    assert_eq!(
        failed_publication_disposition(Some(22), 11, 22),
        FailedPublicationDispositionV1::FinishPublished
    );
    assert_eq!(
        failed_publication_disposition(Some(33), 11, 22),
        FailedPublicationDispositionV1::RestoreRetainedRollback(Some(33))
    );
    assert_eq!(
        failed_publication_disposition(None, 11, 22),
        FailedPublicationDispositionV1::RestoreRetainedRollback(None)
    );
}

#[test]
fn unverified_destination_is_quarantined_before_retained_rollback_is_restored() {
    let temporary = tempfile::tempdir().unwrap();
    let destination = temporary.path().join("sessions.sqlite3");
    let rollback = temporary.path().join("sessions.remote-restore.rollback");
    sqlite_with_marker(&destination, "unverified");
    sqlite_with_marker(&rollback, "retained");
    std::fs::write(sqlite_sidecar(&destination, "wal"), b"foreign wal").unwrap();
    std::fs::write(sqlite_sidecar(&destination, "shm"), b"foreign shm").unwrap();
    let destination_identity =
        tracedecay_runtime_core::db::sqlite_generation_identity(&destination).unwrap();
    let rollback_identity =
        tracedecay_runtime_core::db::sqlite_generation_identity(&rollback).unwrap();

    restore_retained_rollback_over_unverified_destination(
        &destination,
        &rollback,
        Some(destination_identity),
        Some(rollback_identity),
    )
    .unwrap();

    assert_eq!(marker(&destination), "retained");
    assert!(!rollback.exists());
    let quarantine = rollback.with_extension("unverified.sqlite3");
    assert_eq!(marker(&quarantine), "unverified");
    assert!(!sqlite_sidecar(&destination, "wal").exists());
    assert!(!sqlite_sidecar(&destination, "shm").exists());
    assert_eq!(
        std::fs::read(sqlite_sidecar(&quarantine, "wal")).unwrap(),
        b"foreign wal"
    );
    assert_eq!(
        std::fs::read(sqlite_sidecar(&quarantine, "shm")).unwrap(),
        b"foreign shm"
    );
}

#[test]
fn interrupted_quarantine_resumes_from_a_missing_destination() {
    let temporary = tempfile::tempdir().unwrap();
    let destination = temporary.path().join("sessions.sqlite3");
    let rollback = temporary.path().join("sessions.remote-restore.rollback");
    let quarantine = rollback.with_extension("unverified.sqlite3");
    sqlite_with_marker(&rollback, "retained");
    sqlite_with_marker(&quarantine, "unverified");

    restore_retained_rollback_over_unverified_destination(&destination, &rollback, None, None)
        .unwrap();

    assert_eq!(marker(&destination), "retained");
    assert_eq!(marker(&quarantine), "unverified");
    assert!(!rollback.exists());
}

#[test]
fn missing_rollback_leaves_an_unverified_destination_quarantined() {
    let temporary = tempfile::tempdir().unwrap();
    let destination = temporary.path().join("sessions.sqlite3");
    let rollback = temporary.path().join("sessions.remote-restore.rollback");
    sqlite_with_marker(&destination, "unverified");
    let destination_identity =
        tracedecay_runtime_core::db::sqlite_generation_identity(&destination).unwrap();
    let staging = temporary.path().join("sessions.remote-restore.staging");
    install_remote_restore_quarantine(&destination, &staging, &rollback, 11, 22).unwrap();

    restore_retained_rollback_over_unverified_destination(
        &destination,
        &rollback,
        Some(destination_identity),
        None,
    )
    .expect_err("a pre-publication foreign destination has no retained rollback");

    assert!(!destination.exists());
    let quarantine = rollback.with_extension("unverified.sqlite3");
    assert_eq!(marker(&quarantine), "unverified");
    let fence = read_remote_restore_quarantine(&destination)
        .unwrap()
        .expect("durable quarantine fence");
    assert_eq!(fence.expected_rollback_identity, 11);
    assert_eq!(fence.expected_published_identity, 22);
}

#[test]
fn interrupted_exchange_promotes_the_exact_old_staging_to_rollback() {
    let temporary = tempfile::tempdir().unwrap();
    let destination = temporary.path().join("sessions.sqlite3");
    let staging = temporary.path().join("sessions.remote-restore.staging");
    let rollback = temporary.path().join("sessions.remote-restore.rollback");
    sqlite_with_marker(&destination, "published");
    sqlite_with_marker(&staging, "retained-old");
    let old_identity = tracedecay_runtime_core::db::sqlite_generation_identity(&staging).unwrap();
    let new_identity =
        tracedecay_runtime_core::db::sqlite_generation_identity(&destination).unwrap();
    install_remote_restore_quarantine(
        &destination,
        &staging,
        &rollback,
        old_identity,
        new_identity,
    )
    .unwrap();
    let fence = read_remote_restore_quarantine(&destination)
        .unwrap()
        .expect("quarantine fence");

    retain_interrupted_rollback(&fence, &rollback).unwrap();

    assert_eq!(marker(&destination), "published");
    assert_eq!(marker(&rollback), "retained-old");
    assert!(!staging.exists());
}

#[test]
fn rollback_intent_survives_a_sidecar_first_crash_cut() {
    let temporary = tempfile::tempdir().unwrap();
    let destination = temporary.path().join("sessions.sqlite3");
    let staging = temporary.path().join("sessions.remote-restore.staging");
    let rollback = temporary.path().join("sessions.remote-restore.rollback");
    sqlite_with_marker(&destination, "published");
    sqlite_with_marker(&rollback, "retained-old");
    std::fs::write(sqlite_sidecar(&destination, "wal"), b"attach wal").unwrap();
    std::fs::write(sqlite_sidecar(&destination, "shm"), b"attach shm").unwrap();
    let old_identity = tracedecay_runtime_core::db::sqlite_generation_identity(&rollback).unwrap();
    let new_identity =
        tracedecay_runtime_core::db::sqlite_generation_identity(&destination).unwrap();
    install_remote_restore_quarantine(
        &destination,
        &staging,
        &rollback,
        old_identity,
        new_identity,
    )
    .unwrap();
    mark_remote_restore_rollback_required(&destination, &rollback, old_identity, new_identity)
        .unwrap();
    let retained_new = destination.with_extension(format!(
        "remote-restore-rejected-{new_identity:016x}.sqlite3"
    ));

    quarantine_sqlite_sidecars(&destination, &retained_new).unwrap();

    let fence = read_remote_restore_quarantine(&destination)
        .unwrap()
        .expect("rollback fence survives crash cut");
    assert!(rollback_required(&fence));
    assert!(remote_restore_quarantine_active(&destination).unwrap());
    assert!(sqlite_sidecar(&retained_new, "wal").exists());
    assert!(sqlite_sidecar(&retained_new, "shm").exists());
    assert_eq!(marker(&destination), "published");
    assert_eq!(marker(&rollback), "retained-old");
}

#[test]
fn activated_restore_marker_allows_exact_cold_mount_and_rejects_foreign_replacement() {
    let temporary = tempfile::tempdir().unwrap();
    let destination = temporary.path().join("sessions.sqlite3");
    let staging = temporary.path().join("sessions.remote-restore.staging");
    let rollback = temporary.path().join("sessions.remote-restore.rollback");
    sqlite_with_marker(&destination, "published");
    let published_identity =
        tracedecay_runtime_core::db::sqlite_generation_identity(&destination).unwrap();
    install_remote_restore_quarantine(&destination, &staging, &rollback, 11, published_identity)
        .unwrap();

    complete_remote_restore_quarantine(&destination, RestorePublicationV1::Published).unwrap();

    let marker = read_remote_restore_quarantine(&destination)
        .unwrap()
        .expect("durable terminal marker");
    assert_eq!(
        completed_remote_restore(&marker),
        Some(RestorePublicationV1::Published)
    );
    assert!(remote_restore_quarantine_active(&destination).unwrap());
    assert!(remote_restore_quarantine_blocks_open(&destination).unwrap());

    activate_remote_restore_quarantine(&destination, RestorePublicationV1::Published).unwrap();

    assert!(!remote_restore_quarantine_active(&destination).unwrap());
    assert!(!remote_restore_quarantine_blocks_open(&destination).unwrap());
    assert_eq!(
        remote_restore_activated_open_identity(&destination).unwrap(),
        Some(published_identity)
    );

    let foreign = temporary.path().join("foreign.sqlite3");
    sqlite_with_marker(&foreign, "foreign");
    std::fs::remove_file(&destination).unwrap();
    std::fs::rename(&foreign, &destination).unwrap();
    let terminal = read_remote_restore_quarantine(&destination)
        .unwrap()
        .expect("terminal marker remains after foreign replacement");
    assert!(
        validate_completed_remote_restore(
            &destination,
            &terminal,
            RestorePublicationV1::Published,
        )
        .is_err()
    );
    assert!(remote_restore_quarantine_blocks_open(&destination).unwrap());
    remote_restore_activated_open_identity(&destination)
        .expect_err("foreign runtime identity remains fenced");

    let next_staging = temporary.path().join("next.remote-restore.staging");
    let next_rollback = temporary.path().join("next.remote-restore.rollback");
    install_remote_restore_quarantine(&destination, &next_staging, &next_rollback, 33, 44).unwrap();
    let next = read_remote_restore_quarantine(&destination)
        .unwrap()
        .expect("next active restore replaces terminal marker");
    assert_eq!(next.rollback, next_rollback);
    assert!(remote_restore_quarantine_active(&destination).unwrap());
}

#[test]
fn repeat_restore_temp_sync_failure_preserves_the_exact_activated_authority() {
    let temporary = tempfile::tempdir().unwrap();
    let destination = temporary.path().join("sessions.sqlite3");
    let staging = temporary.path().join("sessions.remote-restore.staging");
    let rollback = temporary.path().join("sessions.remote-restore.rollback");
    let unrelated = temporary.path().join("unrelated.sqlite3");
    sqlite_with_marker(&destination, "preserved");
    sqlite_with_marker(&unrelated, "unrelated");
    let preserved_identity =
        tracedecay_runtime_core::db::sqlite_generation_identity(&destination).unwrap();
    install_remote_restore_quarantine(&destination, &staging, &rollback, 11, preserved_identity)
        .unwrap();
    complete_remote_restore_quarantine(&destination, RestorePublicationV1::Published).unwrap();
    activate_remote_restore_quarantine(&destination, RestorePublicationV1::Published).unwrap();

    let next_staging = temporary.path().join("next.remote-restore.staging");
    let next_rollback = temporary.path().join("next.remote-restore.rollback");
    with_durable_atomic_write_fault_for_test(DurableAtomicWriteFaultForTest::AfterTempSync, || {
        install_remote_restore_quarantine(
            &destination,
            &next_staging,
            &next_rollback,
            preserved_identity,
            44,
        )
    })
    .expect_err("repeat restore fence installation fault");

    let retained = read_remote_restore_quarantine(&destination)
        .unwrap()
        .expect("prior activated authority remains durable");
    assert!(activated_fence_matches_preserved_authority(
        &destination,
        &retained,
        preserved_identity,
    ));
    assert_eq!(marker(&destination), "preserved");
    assert_eq!(marker(&unrelated), "unrelated");
}

#[test]
fn failed_phase_transition_preserves_the_previous_active_fence() {
    let temporary = tempfile::tempdir().unwrap();
    let destination = temporary.path().join("sessions.sqlite3");
    let staging = temporary.path().join("sessions.remote-restore.staging");
    let rollback = temporary.path().join("sessions.remote-restore.rollback");
    sqlite_with_marker(&destination, "published");
    install_remote_restore_quarantine(&destination, &staging, &rollback, 11, 22).unwrap();

    with_durable_atomic_write_fault_for_test(DurableAtomicWriteFaultForTest::AfterRename, || {
        mark_remote_restore_rollback_required(&destination, &rollback, 11, 22)
    })
    .expect_err("rollback transition fault");

    let publishing = read_remote_restore_quarantine(&destination)
        .unwrap()
        .expect("immutable publishing fence remains");
    assert!(!rollback_required(&publishing));
    assert!(remote_restore_quarantine_active(&destination).unwrap());

    mark_remote_restore_rollback_required(&destination, &rollback, 11, 22).unwrap();
    complete_remote_restore_quarantine(&destination, RestorePublicationV1::Published)
        .expect_err("rollback-required restore cannot publish");
    with_durable_atomic_write_fault_for_test(DurableAtomicWriteFaultForTest::AfterRename, || {
        complete_remote_restore_quarantine(&destination, RestorePublicationV1::RolledBack)
    })
    .expect_err("terminal transition fault");

    let rollback_required_marker = read_remote_restore_quarantine(&destination)
        .unwrap()
        .expect("rollback-required marker remains");
    assert!(rollback_required(&rollback_required_marker));
    assert!(remote_restore_quarantine_active(&destination).unwrap());

    complete_remote_restore_quarantine(&destination, RestorePublicationV1::RolledBack).unwrap();
    with_durable_atomic_write_fault_for_test(DurableAtomicWriteFaultForTest::AfterRename, || {
        activate_remote_restore_quarantine(&destination, RestorePublicationV1::RolledBack)
    })
    .expect_err("activation transition fault");
    let terminal = read_remote_restore_quarantine(&destination)
        .unwrap()
        .expect("terminal marker remains after activation fault");
    assert_eq!(
        completed_remote_restore(&terminal),
        Some(RestorePublicationV1::RolledBack)
    );
    assert!(remote_restore_quarantine_active(&destination).unwrap());
}
