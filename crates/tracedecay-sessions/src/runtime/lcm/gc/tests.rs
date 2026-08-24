use std::fs;

use crate::runtime::lcm::schema;
use crate::runtime::lcm::util::{self, file_mtime_seconds};
use tracedecay_runtime_core::db::engine::{Connection, TestConnection, TransactionBehavior};

use super::pending_delete::{PENDING_PAYLOAD_DELETE_ERROR_PREFIX, pending_payload_delete_key};
use super::*;

const PROVIDER: &str = "cursor";
const PRIMARY_REF: &str =
    "payload_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.payload";
const SECONDARY_REF: &str =
    "payload_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb.payload";

async fn drain_pending_payload_deletes(
    conn: &Connection,
    storage_root: &Path,
) -> Result<PayloadDeleteDrain, LcmError> {
    let transaction = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let drain = drain_pending_payload_deletes_in_transaction(&transaction, storage_root).await?;
    transaction.commit().await?;
    Ok(drain)
}

async fn drain_pending_payload_delete(
    conn: &Connection,
    storage_root: &Path,
    payload_ref: &str,
) -> Result<Option<u64>, LcmError> {
    let transaction = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let removed =
        drain_pending_payload_delete_in_transaction(&transaction, storage_root, payload_ref)
            .await?;
    transaction.commit().await?;
    Ok(removed)
}

struct TestStore {
    _temp: tempfile::TempDir,
    storage_root: PathBuf,
    conn: TestConnection,
}

async fn test_store() -> Result<TestStore, String> {
    let temp = tempfile::tempdir().map_err(|err| format!("create tempdir: {err}"))?;
    let storage_root = temp.path().to_path_buf();
    let conn = TestConnection::open(&storage_root.join("sessions.db"));
    ensure_gc_test_schema(&conn).await?;
    Ok(TestStore {
        _temp: temp,
        storage_root,
        conn,
    })
}

async fn ensure_gc_test_schema(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS sessions (
            provider TEXT NOT NULL,
            session_id TEXT NOT NULL,
            project_key TEXT NOT NULL,
            project_path TEXT NOT NULL,
            title TEXT,
            started_at INTEGER,
            PRIMARY KEY(provider, session_id)
        );
        CREATE TABLE IF NOT EXISTS session_messages (
            provider TEXT NOT NULL,
            message_id TEXT NOT NULL,
            session_id TEXT NOT NULL,
            role TEXT NOT NULL,
            timestamp INTEGER,
            ordinal INTEGER NOT NULL,
            text TEXT NOT NULL,
            metadata_json TEXT,
            PRIMARY KEY(provider, message_id),
            FOREIGN KEY(provider, session_id)
                REFERENCES sessions(provider, session_id) ON DELETE CASCADE
        );",
    )
    .await
    .map_err(|err| format!("create gc test sessions table: {err}"))?;
    schema::ensure_lcm_schema(conn)
        .await
        .map_err(|err| format!("ensure lcm schema: {err}"))?;
    Ok(())
}

async fn insert_session(
    conn: &Connection,
    storage_root: &Path,
    session_id: &str,
) -> Result<(), String> {
    let project_key = storage_root.to_string_lossy().to_string();
    conn.execute(
        "INSERT INTO sessions (provider, session_id, project_key, project_path, title, started_at)
         VALUES (?1, ?2, ?3, ?3, ?4, 1)
         ON CONFLICT(provider, session_id) DO NOTHING",
        params![PROVIDER, session_id, project_key, session_id],
    )
    .await
    .map_err(|err| format!("insert session {session_id}: {err}"))?;
    Ok(())
}

struct RawMessage<'a> {
    session_id: &'a str,
    message_id: &'a str,
    storage_kind: &'a str,
    payload_ref: Option<&'a str>,
    content: Option<&'a str>,
    snippet_text: &'a str,
    index_text: &'a str,
    metadata_json: Option<&'a str>,
}

async fn insert_raw_message(conn: &Connection, message: RawMessage<'_>) -> Result<(), String> {
    conn.execute(
        "INSERT INTO lcm_raw_messages (
            provider, message_id, session_id, role, ordinal, timestamp,
            content, content_hash, storage_kind, payload_ref, snippet_text,
            index_text, legacy_source, legacy_truncated, metadata_json
         ) VALUES (?1, ?2, ?3, 'assistant', 1, 2, ?4, ?5, ?6, ?7, ?8, ?9, 0, 0, ?10)",
        params![
            PROVIDER,
            message.message_id,
            message.session_id,
            util::opt_text(message.content),
            format!("{}-hash", message.message_id),
            message.storage_kind,
            message.payload_ref,
            message.snippet_text,
            message.index_text,
            message.metadata_json
        ],
    )
    .await
    .map_err(|err| format!("insert raw message {}: {err}", message.message_id))?;
    Ok(())
}

async fn seed_payload(
    store: &TestStore,
    message_id: &str,
    content: &str,
) -> Result<String, String> {
    insert_session(&store.conn, &store.storage_root, "session-a").await?;
    let payload_ref = payload::write_external_payload(
        &store.storage_root,
        PROVIDER,
        "session-a",
        message_id,
        "message",
        content,
        None,
    )
    .map_err(|err| err.to_string())?;
    payload::upsert_payload_metadata(&store.conn, &payload_ref)
        .await
        .map_err(|err| err.to_string())?;
    let placeholder = format!(
        "[externalized payload: bytes={} ref={}; content]",
        content.len(),
        payload_ref.payload_ref
    );
    insert_raw_message(
        &store.conn,
        RawMessage {
            session_id: "session-a",
            message_id,
            storage_kind: "external",
            payload_ref: Some(&payload_ref.payload_ref),
            content: None,
            snippet_text: &placeholder,
            index_text: &placeholder,
            metadata_json: Some(&placeholder),
        },
    )
    .await?;
    Ok(payload_ref.payload_ref)
}

fn payload_path(store: &TestStore, payload_ref: &str) -> PathBuf {
    payload::payload_dir(&store.storage_root).join(payload_ref)
}

async fn drop_raw_reference(store: &TestStore, payload_ref: &str) -> Result<(), String> {
    store
        .conn
        .execute(
            "DELETE FROM lcm_raw_messages WHERE payload_ref = ?1",
            params![payload_ref],
        )
        .await
        .map_err(|err| format!("drop raw reference: {err}"))?;
    Ok(())
}

async fn insert_gc_mark(
    store: &TestStore,
    payload_ref: &str,
    state: &str,
    first_seen_at: i64,
) -> Result<(), String> {
    store
        .conn
        .execute(
            "INSERT INTO lcm_gc_marks(payload_ref, state, first_seen_at, updated_at)
             VALUES (?1, ?2, ?3, ?3)",
            params![payload_ref, state, first_seen_at],
        )
        .await
        .map_err(|err| format!("insert gc mark: {err}"))?;
    Ok(())
}

#[test]
fn tombstone_helper_rewrites_all_live_prefixes() {
    let cases = [
        (
            format!("[externalized payload: bytes=12 ref={PRIMARY_REF}; note=body]"),
            format!("[gc'd externalized payload: bytes=12 ref={PRIMARY_REF}; note=body]"),
        ),
        (
            format!("[externalized lcm ingest payload: bytes=12 ref={PRIMARY_REF}; note=body]"),
            format!("[gc'd externalized payload: bytes=12 ref={PRIMARY_REF}; note=body]"),
        ),
        (
            format!("[externalized tool output: bytes=12 ref={PRIMARY_REF}; note=body]"),
            format!("[gc'd externalized tool output: bytes=12 ref={PRIMARY_REF}; note=body]"),
        ),
    ];
    for (input, expected) in cases {
        assert_eq!(tombstone_placeholder_in_text(&input, PRIMARY_REF), expected);
    }
}

#[test]
fn tombstone_helper_rewrites_repeated_refs_and_is_idempotent() {
    let input = format!(
        "one [externalized payload: bytes=12 ref={PRIMARY_REF}; a] two [externalized tool output: bytes=8 ref={PRIMARY_REF}; b]"
    );
    let expected = format!(
        "one [gc'd externalized payload: bytes=12 ref={PRIMARY_REF}; a] two [gc'd externalized tool output: bytes=8 ref={PRIMARY_REF}; b]"
    );
    assert_eq!(tombstone_placeholder_in_text(&input, PRIMARY_REF), expected);
    assert_eq!(
        tombstone_placeholder_in_text(&expected, PRIMARY_REF),
        expected
    );
}

#[tokio::test]
async fn referenced_payload_refs_ignores_tombstoned_placeholders() -> Result<(), String> {
    let store = test_store().await?;
    insert_session(&store.conn, &store.storage_root, "session-a").await?;
    let live = format!("prefix [externalized payload: bytes=12 ref={PRIMARY_REF}; marker] suffix");
    let tombstoned =
        format!("prefix [gc'd externalized payload: bytes=12 ref={SECONDARY_REF}; marker] suffix");
    insert_raw_message(
        &store.conn,
        RawMessage {
            session_id: "session-a",
            message_id: "message-1",
            storage_kind: "inline",
            payload_ref: None,
            content: Some(&live),
            snippet_text: &live,
            index_text: &live,
            metadata_json: None,
        },
    )
    .await?;
    insert_raw_message(
        &store.conn,
        RawMessage {
            session_id: "session-a",
            message_id: "message-2",
            storage_kind: "inline",
            payload_ref: None,
            content: Some(&tombstoned),
            snippet_text: &tombstoned,
            index_text: &tombstoned,
            metadata_json: None,
        },
    )
    .await?;

    let refs = referenced_payload_refs(&store.conn, PROVIDER, Some("session-a"))
        .await
        .map_err(|err| err.to_string())?;
    assert_eq!(refs, BTreeSet::from([PRIMARY_REF.to_string()]));
    assert!(text_has_tombstoned_payload_ref(&tombstoned, SECONDARY_REF));
    Ok(())
}

#[tokio::test]
async fn delete_external_payload_aborts_when_still_referenced() -> Result<(), String> {
    let store = test_store().await?;
    let payload_ref = seed_payload(&store, "message-1", "body to delete").await?;
    let Err(err) = payload::delete_external_payload(
        &store.conn,
        &store.storage_root,
        &payload_ref,
        &payload::DeleteOpts::default(),
    )
    .await
    else {
        return Err("live payload must not be deleted".to_string());
    };
    assert_eq!(err, LcmError::StillReferenced);
    assert!(
        payload::load_payload_metadata(&store.conn, &payload_ref)
            .await
            .is_ok()
    );
    assert!(
        payload::payload_dir(&store.storage_root)
            .join(&payload_ref)
            .is_file()
    );
    Ok(())
}

#[tokio::test]
async fn delete_external_payload_applies_db_then_file_and_is_idempotent() -> Result<(), String> {
    let store = test_store().await?;
    let payload_ref = seed_payload(&store, "message-1", "body to delete").await?;
    drop_raw_reference(&store, &payload_ref).await?;
    insert_gc_mark(&store, &payload_ref, "unreferenced", 1).await?;

    let outcome = payload::delete_external_payload(
        &store.conn,
        &store.storage_root,
        &payload_ref,
        &payload::DeleteOpts::default(),
    )
    .await
    .map_err(|err| err.to_string())?;
    assert!(outcome.metadata_row_existed);
    assert!(outcome.file_existed);
    assert!(outcome.file_removed);
    assert!(outcome.bytes_freed > 0);
    assert!(
        payload::load_payload_metadata(&store.conn, &payload_ref)
            .await
            .is_err()
    );
    assert!(
        gc_mark(&store.conn, &payload_ref)
            .await
            .map_err(|err| err.to_string())?
            .is_none(),
        "ordinary deletion must clear the payload's GC mark"
    );
    assert!(!payload_path(&store, &payload_ref).exists());

    let second = payload::delete_external_payload(
        &store.conn,
        &store.storage_root,
        &payload_ref,
        &payload::DeleteOpts::default(),
    )
    .await
    .map_err(|err| err.to_string())?;
    assert_eq!(second, payload::DeleteOutcome::default());
    Ok(())
}

#[tokio::test]
async fn payload_delete_rollback_preserves_metadata_and_file() -> Result<(), String> {
    let store = test_store().await?;
    let payload_ref = seed_payload(&store, "message-1", "body to preserve").await?;
    drop_raw_reference(&store, &payload_ref).await?;
    insert_gc_mark(&store, &payload_ref, "unreferenced", 1).await?;

    let transaction = store
        .conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .map_err(|err| err.to_string())?;
    let prepared = payload::delete_external_payload_in_transaction(
        &transaction,
        &store.storage_root,
        &payload_ref,
        &payload::DeleteOpts::default(),
    )
    .await
    .map_err(|err| err.to_string())?;
    assert!(
        prepared.pending_removal_bytes.is_some(),
        "the transaction should stage deletion"
    );
    assert!(
        payload_path(&store, &payload_ref).is_file(),
        "the file must remain until commit"
    );
    transaction
        .rollback()
        .await
        .map_err(|err| err.to_string())?;

    assert!(payload_path(&store, &payload_ref).is_file());
    assert!(
        payload::load_payload_metadata(&store.conn, &payload_ref)
            .await
            .is_ok()
    );
    assert!(
        gc_mark(&store.conn, &payload_ref)
            .await
            .map_err(|err| err.to_string())?
            .is_some(),
        "rollback must restore the payload's GC mark"
    );
    assert!(
        schema::get_gc_meta(&store.conn, &pending_payload_delete_key(&payload_ref))
            .await
            .map_err(|err| err.to_string())?
            .is_none()
    );
    Ok(())
}

#[tokio::test]
async fn committed_payload_delete_tombstone_recovers_unlink() -> Result<(), String> {
    let store = test_store().await?;
    let payload_ref = seed_payload(&store, "message-1", "body to reap").await?;
    drop_raw_reference(&store, &payload_ref).await?;

    let transaction = store
        .conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .map_err(|err| err.to_string())?;
    payload::delete_external_payload_in_transaction(
        &transaction,
        &store.storage_root,
        &payload_ref,
        &payload::DeleteOpts::default(),
    )
    .await
    .map_err(|err| err.to_string())?;
    transaction.commit().await.map_err(|err| err.to_string())?;

    assert!(payload_path(&store, &payload_ref).is_file());
    assert!(
        payload::load_payload_metadata(&store.conn, &payload_ref)
            .await
            .is_err()
    );
    let removed = drain_pending_payload_delete(&store.conn, &store.storage_root, &payload_ref)
        .await
        .map_err(|err| err.to_string())?;
    assert!(removed.is_some());
    assert!(!payload_path(&store, &payload_ref).exists());
    assert!(
        schema::get_gc_meta(&store.conn, &pending_payload_delete_key(&payload_ref))
            .await
            .map_err(|err| err.to_string())?
            .is_none()
    );
    Ok(())
}

#[tokio::test]
async fn committed_payload_delete_drain_failure_returns_pending_then_retries() -> Result<(), String>
{
    let store = test_store().await?;
    let payload_ref = seed_payload(&store, "message-1", "body to retry").await?;
    drop_raw_reference(&store, &payload_ref).await?;

    let transaction = store
        .conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .map_err(|err| err.to_string())?;
    let prepared = payload::delete_external_payload_in_transaction(
        &transaction,
        &store.storage_root,
        &payload_ref,
        &payload::DeleteOpts::default(),
    )
    .await
    .map_err(|err| err.to_string())?;
    transaction.commit().await.map_err(|err| err.to_string())?;

    let mut outcome = prepared.outcome;
    payload::reconcile_committed_payload_drain(
        &mut outcome,
        &payload_ref,
        Err(LcmError::Io(
            "injected post-commit drain failure".to_string(),
        )),
    );
    assert!(outcome.metadata_row_existed);
    assert!(!outcome.file_removed);
    assert_eq!(outcome.bytes_freed, 0);
    assert!(payload_path(&store, &payload_ref).is_file());
    assert!(
        schema::get_gc_meta(&store.conn, &pending_payload_delete_key(&payload_ref))
            .await
            .map_err(|err| err.to_string())?
            .is_some()
    );

    let removed = drain_pending_payload_delete(&store.conn, &store.storage_root, &payload_ref)
        .await
        .map_err(|err| err.to_string())?;
    assert!(removed.is_some());
    assert!(!payload_path(&store, &payload_ref).exists());
    Ok(())
}

#[tokio::test]
async fn corrupt_tombstone_does_not_block_healthy_pending_delete() -> Result<(), String> {
    let store = test_store().await?;
    let dir = payload::payload_dir(&store.storage_root);
    fs::create_dir_all(&dir).map_err(|err| err.to_string())?;
    fs::write(dir.join(SECONDARY_REF), b"healthy pending payload")
        .map_err(|err| err.to_string())?;
    let (hash, bytes, chars) =
        payload::payload_file_fingerprint(&dir, SECONDARY_REF).map_err(|err| err.to_string())?;
    stage_payload_delete(&store.conn, SECONDARY_REF, Some(&hash), bytes, chars)
        .await
        .map_err(|err| err.to_string())?;
    schema::set_gc_meta(
        &store.conn,
        &pending_payload_delete_key(PRIMARY_REF),
        "{not-json",
    )
    .await
    .map_err(|err| err.to_string())?;

    let drain = drain_pending_payload_deletes(&store.conn, &store.storage_root)
        .await
        .map_err(|err| err.to_string())?;
    assert_eq!(drain.outcomes.removed.count, 1);
    assert_eq!(drain.outcomes.failed.count, 1);
    assert!(!dir.join(SECONDARY_REF).exists());
    assert!(
        schema::get_gc_meta(&store.conn, &pending_payload_delete_key(SECONDARY_REF))
            .await
            .map_err(|err| err.to_string())?
            .is_none()
    );
    assert!(
        schema::get_gc_meta(&store.conn, &pending_payload_delete_key(PRIMARY_REF))
            .await
            .map_err(|err| err.to_string())?
            .is_some()
    );
    assert_eq!(
        schema::get_gc_meta(&store.conn, "last_gc_status")
            .await
            .map_err(|err| err.to_string())?
            .as_deref(),
        Some("partial")
    );
    let error = schema::get_gc_meta(&store.conn, "last_error")
        .await
        .map_err(|err| err.to_string())?
        .ok_or_else(|| "missing pending-delete diagnostic".to_string())?;
    assert!(error.starts_with(PENDING_PAYLOAD_DELETE_ERROR_PREFIX));
    assert!(error.len() <= 1_024);
    let status = schema::get_gc_meta(&store.conn, "last_gc_status")
        .await
        .map_err(|err| err.to_string())?
        .ok_or_else(|| "missing GC status diagnostic".to_string())?;
    assert_eq!(status, "partial");

    let retry = drain_pending_payload_deletes(&store.conn, &store.storage_root)
        .await
        .map_err(|err| err.to_string())?;
    assert_eq!(retry.outcomes.failed.count, 1);
    Ok(())
}

#[tokio::test]
async fn committed_orphan_tombstone_preserves_same_size_replacement() -> Result<(), String> {
    let store = test_store().await?;
    let dir = payload::payload_dir(&store.storage_root);
    fs::create_dir_all(&dir).map_err(|err| err.to_string())?;
    let path = dir.join(PRIMARY_REF);
    fs::write(&path, b"orphan-a").map_err(|err| err.to_string())?;
    let mtime = file_mtime_seconds(&fs::symlink_metadata(&path).map_err(|err| err.to_string())?);
    let cfg = LcmGcConfig {
        grace_seconds: LcmGcConfig::MIN_GRACE_SECONDS,
        backup_before_reap: false,
        ..Default::default()
    }
    .normalized();

    let transaction = store
        .conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .map_err(|err| err.to_string())?;
    let mut report = run_payload_gc_in_transaction(
        &transaction,
        &store.storage_root,
        PROVIDER,
        None,
        &cfg,
        true,
        mtime + LcmGcConfig::MIN_GRACE_SECONDS as i64,
    )
    .await
    .map_err(|err| err.to_string())?;
    assert_eq!(report.orphans.count, 1);
    transaction.commit().await.map_err(|err| err.to_string())?;

    fs::write(&path, b"replace!").map_err(|err| err.to_string())?;
    let drain = drain_pending_payload_deletes(&store.conn, &store.storage_root)
        .await
        .map_err(|err| err.to_string())?;
    assert!(drain.removed_bytes(PRIMARY_REF).is_none());
    finalize_gc_report(&store.conn, &mut report, drain)
        .await
        .map_err(|err| err.to_string())?;
    assert_eq!(report.totals.files, 0);
    assert_eq!(report.totals.bytes, 0);
    assert_eq!(fs::read(&path).map_err(|err| err.to_string())?, b"replace!");
    assert!(
        schema::get_gc_meta(&store.conn, &pending_payload_delete_key(PRIMARY_REF))
            .await
            .map_err(|err| err.to_string())?
            .is_none(),
        "the stale tombstone must not poison later opens"
    );
    drain_pending_payload_deletes(&store.conn, &store.storage_root)
        .await
        .map_err(|err| err.to_string())?;
    Ok(())
}

#[tokio::test]
async fn delete_external_payload_db_only_leaves_orphan_for_crash_convergence() -> Result<(), String>
{
    let store = test_store().await?;
    let payload_ref = seed_payload(&store, "message-1", "body to delete").await?;
    drop_raw_reference(&store, &payload_ref).await?;

    let outcome = payload::delete_external_payload(
        &store.conn,
        &store.storage_root,
        &payload_ref,
        &payload::DeleteOpts {
            rewrite_placeholders: true,
            remove_file: false,
            verify_hash: false,
        },
    )
    .await
    .map_err(|err| err.to_string())?;
    assert!(outcome.metadata_row_existed);
    assert!(outcome.file_existed);
    assert!(!outcome.file_removed);
    assert_eq!(outcome.bytes_freed, 0);
    assert!(payload_path(&store, &payload_ref).is_file());

    let file_mtime = file_mtime_seconds(
        &fs::symlink_metadata(payload_path(&store, &payload_ref)).map_err(|err| err.to_string())?,
    );
    let cfg = LcmGcConfig {
        grace_seconds: LcmGcConfig::MIN_GRACE_SECONDS,
        backup_before_reap: false,
        ..Default::default()
    }
    .normalized();
    let report = run_payload_gc_with_apply(
        &store.conn,
        &store.storage_root,
        PROVIDER,
        None,
        &cfg,
        true,
        file_mtime + LcmGcConfig::MIN_GRACE_SECONDS as i64,
    )
    .await
    .map_err(|err| err.to_string())?;
    assert_eq!(report.orphans.count, 1);
    assert_eq!(report.totals.files, 1);
    assert!(!payload_path(&store, &payload_ref).exists());
    Ok(())
}

#[tokio::test]
async fn delete_external_payload_hash_gate_preserves_corrupted_payload() -> Result<(), String> {
    let store = test_store().await?;
    let payload_ref = seed_payload(&store, "message-1", "trusted body").await?;
    drop_raw_reference(&store, &payload_ref).await?;
    fs::write(payload_path(&store, &payload_ref), b"tampered body")
        .map_err(|err| err.to_string())?;

    let Err(err) = payload::delete_external_payload(
        &store.conn,
        &store.storage_root,
        &payload_ref,
        &payload::DeleteOpts::default(),
    )
    .await
    else {
        return Err("corrupted payload must not be reaped".to_string());
    };
    assert_eq!(err, LcmError::PayloadIntegrityMismatch);
    assert!(
        payload::load_payload_metadata(&store.conn, &payload_ref)
            .await
            .is_ok()
    );
    assert!(payload_path(&store, &payload_ref).is_file());
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn delete_external_payload_rejects_symlink_payload_at_hash_gate() -> Result<(), String> {
    let store = test_store().await?;
    let payload_ref = seed_payload(&store, "message-1", "trusted body").await?;
    drop_raw_reference(&store, &payload_ref).await?;
    let path = payload_path(&store, &payload_ref);
    fs::remove_file(&path).map_err(|err| err.to_string())?;
    let outside = store.storage_root.join("outside-payload-body.txt");
    fs::write(&outside, b"trusted body").map_err(|err| err.to_string())?;
    std::os::unix::fs::symlink(&outside, &path).map_err(|err| err.to_string())?;

    let Err(err) = payload::delete_external_payload(
        &store.conn,
        &store.storage_root,
        &payload_ref,
        &payload::DeleteOpts::default(),
    )
    .await
    else {
        return Err("symlink payload must be rejected before DB mutation".to_string());
    };
    assert_eq!(err, LcmError::InvalidPayloadRef);
    assert!(
        payload::load_payload_metadata(&store.conn, &payload_ref)
            .await
            .is_ok()
    );
    assert!(
        fs::symlink_metadata(&path)
            .map_err(|err| err.to_string())?
            .file_type()
            .is_symlink()
    );
    Ok(())
}

#[tokio::test]
async fn delete_external_payload_rejects_invalid_refs() -> Result<(), String> {
    let temp = tempfile::tempdir().map_err(|err| format!("create tempdir: {err}"))?;
    let conn = TestConnection::open(&temp.path().join("sessions.db"));
    for invalid in [
        "",
        ".",
        "..",
        "../evil",
        "/etc/passwd",
        "payload_../x.payload",
    ] {
        let Err(err) = payload::delete_external_payload(
            &conn,
            temp.path(),
            invalid,
            &payload::DeleteOpts::default(),
        )
        .await
        else {
            return Err("invalid ref should fail before path access".to_string());
        };
        assert_eq!(err, LcmError::InvalidPayloadRef, "invalid ref {invalid}");
    }
    Ok(())
}

#[tokio::test]
async fn gc_on_store_without_payload_dir_reports_empty_run() -> Result<(), String> {
    let store = test_store().await?;
    assert!(!payload::payload_dir(&store.storage_root).exists());
    let cfg = LcmGcConfig {
        backup_before_reap: false,
        ..Default::default()
    }
    .normalized();
    for apply in [false, true] {
        let report = run_payload_gc_with_apply(
            &store.conn,
            &store.storage_root,
            PROVIDER,
            None,
            &cfg,
            apply,
            1_000,
        )
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(report.orphans.count, 0);
        assert_eq!(report.unreferenced.count, 0);
        assert_eq!(report.missing.count, 0);
        assert_eq!(report.totals.files, 0);
        assert!(report.errors.is_empty());
    }
    assert!(!payload::payload_dir(&store.storage_root).exists());
    Ok(())
}

#[tokio::test]
async fn gc_reports_missing_payloads_when_payload_dir_was_deleted() -> Result<(), String> {
    let store = test_store().await?;
    let payload_ref = seed_payload(&store, "msg-1", "externalized content").await?;
    std::fs::remove_dir_all(payload::payload_dir(&store.storage_root))
        .map_err(|err| format!("remove payload dir: {err}"))?;
    let cfg = LcmGcConfig {
        backup_before_reap: false,
        ..Default::default()
    }
    .normalized();
    let report = run_payload_gc_with_apply(
        &store.conn,
        &store.storage_root,
        PROVIDER,
        None,
        &cfg,
        false,
        1_000,
    )
    .await
    .map_err(|err| err.to_string())?;
    assert_eq!(report.missing.count, 1);
    assert_eq!(report.missing.refs, vec![payload_ref]);
    assert!(report.errors.is_empty());
    assert!(!payload::payload_dir(&store.storage_root).exists());
    Ok(())
}

#[tokio::test]
async fn unreferenced_payload_two_scan_reaps_after_grace() -> Result<(), String> {
    let store = test_store().await?;
    let payload_ref = seed_payload(&store, "message-1", "body to delete").await?;
    drop_raw_reference(&store, &payload_ref).await?;
    let cfg = LcmGcConfig {
        grace_seconds: LcmGcConfig::MIN_GRACE_SECONDS,
        backup_before_reap: false,
        ..Default::default()
    }
    .normalized();
    let first = run_payload_gc_with_apply(
        &store.conn,
        &store.storage_root,
        PROVIDER,
        None,
        &cfg,
        true,
        1_000,
    )
    .await
    .map_err(|err| err.to_string())?;
    assert_eq!(first.unreferenced.count, 0);
    assert!(
        payload::load_payload_metadata(&store.conn, &payload_ref)
            .await
            .is_ok()
    );

    let second = run_payload_gc_with_apply(
        &store.conn,
        &store.storage_root,
        PROVIDER,
        None,
        &cfg,
        true,
        1_000 + LcmGcConfig::MIN_GRACE_SECONDS as i64,
    )
    .await
    .map_err(|err| err.to_string())?;
    assert_eq!(second.unreferenced.count, 1);
    assert!(
        payload::load_payload_metadata(&store.conn, &payload_ref)
            .await
            .is_err()
    );
    assert!(
        !payload::payload_dir(&store.storage_root)
            .join(&payload_ref)
            .exists()
    );
    Ok(())
}

#[tokio::test]
async fn session_scoped_unreferenced_payload_reaps_after_grace() -> Result<(), String> {
    let store = test_store().await?;
    let payload_ref = seed_payload(&store, "message-1", "body to delete").await?;
    drop_raw_reference(&store, &payload_ref).await?;
    let cfg = LcmGcConfig {
        grace_seconds: LcmGcConfig::MIN_GRACE_SECONDS,
        backup_before_reap: false,
        ..Default::default()
    }
    .normalized();
    let first = run_payload_gc_with_apply(
        &store.conn,
        &store.storage_root,
        PROVIDER,
        Some("session-a"),
        &cfg,
        true,
        1_000,
    )
    .await
    .map_err(|err| err.to_string())?;
    assert_eq!(first.unreferenced.count, 0);
    assert!(
        payload::load_payload_metadata(&store.conn, &payload_ref)
            .await
            .is_ok()
    );

    let second = run_payload_gc_with_apply(
        &store.conn,
        &store.storage_root,
        PROVIDER,
        Some("session-a"),
        &cfg,
        true,
        1_000 + LcmGcConfig::MIN_GRACE_SECONDS as i64,
    )
    .await
    .map_err(|err| err.to_string())?;
    assert_eq!(second.unreferenced.count, 1);
    assert!(
        payload::load_payload_metadata(&store.conn, &payload_ref)
            .await
            .is_err()
    );
    assert!(
        !payload::payload_dir(&store.storage_root)
            .join(&payload_ref)
            .exists()
    );
    Ok(())
}

#[tokio::test]
async fn run_payload_gc_dry_run_does_not_mutate() -> Result<(), String> {
    let store = test_store().await?;
    let payload_ref = seed_payload(&store, "message-1", "body to delete").await?;
    drop_raw_reference(&store, &payload_ref).await?;
    insert_gc_mark(&store, &payload_ref, "unreferenced", 1).await?;
    let cfg = LcmGcConfig {
        grace_seconds: LcmGcConfig::MIN_GRACE_SECONDS,
        backup_before_reap: false,
        ..Default::default()
    }
    .normalized();
    let report = run_payload_gc(
        &store.conn,
        &store.storage_root,
        PROVIDER,
        None,
        &cfg,
        1_000,
    )
    .await
    .map_err(|err| err.to_string())?;
    assert_eq!(report.status, "dry_run");
    assert_eq!(report.unreferenced.count, 1);
    assert!(
        payload::load_payload_metadata(&store.conn, &payload_ref)
            .await
            .is_ok()
    );
    assert!(
        payload::payload_dir(&store.storage_root)
            .join(&payload_ref)
            .is_file()
    );
    Ok(())
}

#[tokio::test]
async fn orphan_phase_honors_mtime_grace_then_reaps() -> Result<(), String> {
    let store = test_store().await?;
    let payload_ref = seed_payload(&store, "message-1", "orphan body").await?;
    drop_raw_reference(&store, &payload_ref).await?;
    store
        .conn
        .execute(
            "DELETE FROM lcm_external_payloads WHERE payload_ref = ?1",
            params![payload_ref.as_str()],
        )
        .await
        .map_err(|err| err.to_string())?;
    let file_mtime = file_mtime_seconds(
        &fs::symlink_metadata(payload_path(&store, &payload_ref)).map_err(|err| err.to_string())?,
    );
    let cfg = LcmGcConfig {
        grace_seconds: LcmGcConfig::MIN_GRACE_SECONDS,
        backup_before_reap: false,
        ..Default::default()
    }
    .normalized();
    let report = run_payload_gc(
        &store.conn,
        &store.storage_root,
        PROVIDER,
        None,
        &cfg,
        file_mtime + LcmGcConfig::MIN_GRACE_SECONDS as i64 - 1,
    )
    .await
    .map_err(|err| err.to_string())?;
    assert_eq!(report.orphans.count, 0);
    assert!(payload_path(&store, &payload_ref).is_file());

    let report = run_payload_gc_with_apply(
        &store.conn,
        &store.storage_root,
        PROVIDER,
        None,
        &cfg,
        true,
        file_mtime + LcmGcConfig::MIN_GRACE_SECONDS as i64,
    )
    .await
    .map_err(|err| err.to_string())?;
    assert_eq!(report.orphans.count, 1);
    assert_eq!(report.totals.files, 1);
    assert!(!payload_path(&store, &payload_ref).exists());
    Ok(())
}

#[tokio::test]
async fn missing_metadata_defaults_to_report_only_and_opt_in_tombstones_after_window()
-> Result<(), String> {
    let store = test_store().await?;
    let payload_ref = seed_payload(&store, "message-1", "missing body").await?;
    fs::remove_file(payload_path(&store, &payload_ref)).map_err(|err| err.to_string())?;
    let cfg = LcmGcConfig {
        reap_missing_enabled: false,
        reap_missing_after: 10,
        backup_before_reap: false,
        ..Default::default()
    }
    .normalized();
    let first = run_payload_gc_with_apply(
        &store.conn,
        &store.storage_root,
        PROVIDER,
        None,
        &cfg,
        true,
        100,
    )
    .await
    .map_err(|err| err.to_string())?;
    assert_eq!(first.missing.count, 1);
    let later = run_payload_gc_with_apply(
        &store.conn,
        &store.storage_root,
        PROVIDER,
        None,
        &cfg,
        true,
        1_000,
    )
    .await
    .map_err(|err| err.to_string())?;
    assert_eq!(later.missing.count, 1);
    assert!(
        payload::load_payload_metadata(&store.conn, &payload_ref)
            .await
            .is_ok()
    );

    let cfg = LcmGcConfig {
        reap_missing_enabled: true,
        reap_missing_after: 10,
        backup_before_reap: false,
        ..Default::default()
    }
    .normalized();
    let marked = run_payload_gc_with_apply(
        &store.conn,
        &store.storage_root,
        PROVIDER,
        None,
        &cfg,
        true,
        2_000,
    )
    .await
    .map_err(|err| err.to_string())?;
    assert_eq!(marked.missing.count, 1);
    assert!(
        payload::load_payload_metadata(&store.conn, &payload_ref)
            .await
            .is_ok()
    );

    let reaped = run_payload_gc_with_apply(
        &store.conn,
        &store.storage_root,
        PROVIDER,
        None,
        &cfg,
        true,
        2_010,
    )
    .await
    .map_err(|err| err.to_string())?;
    assert_eq!(reaped.missing.count, 1);
    assert!(
        payload::load_payload_metadata(&store.conn, &payload_ref)
            .await
            .is_err()
    );
    let refs = referenced_payload_refs(&store.conn, PROVIDER, None)
        .await
        .map_err(|err| err.to_string())?;
    assert!(!refs.contains(&payload_ref));
    Ok(())
}

#[tokio::test]
async fn missing_metadata_clears_mark_when_file_reappears() -> Result<(), String> {
    let store = test_store().await?;
    let payload_ref = seed_payload(&store, "message-1", "restored body").await?;
    fs::remove_file(payload_path(&store, &payload_ref)).map_err(|err| err.to_string())?;
    let cfg = LcmGcConfig {
        reap_missing_enabled: true,
        reap_missing_after: 10,
        backup_before_reap: false,
        ..Default::default()
    }
    .normalized();
    run_payload_gc_with_apply(
        &store.conn,
        &store.storage_root,
        PROVIDER,
        None,
        &cfg,
        true,
        100,
    )
    .await
    .map_err(|err| err.to_string())?;
    assert_eq!(
        gc_mark(&store.conn, &payload_ref)
            .await
            .map_err(|err| err.to_string())?
            .map(|mark| mark.0),
        Some("missing".to_string())
    );

    fs::write(payload_path(&store, &payload_ref), b"restored body")
        .map_err(|err| err.to_string())?;
    let report = run_payload_gc_with_apply(
        &store.conn,
        &store.storage_root,
        PROVIDER,
        None,
        &cfg,
        true,
        1_000,
    )
    .await
    .map_err(|err| err.to_string())?;

    assert_eq!(report.missing.count, 0);
    assert!(
        gc_mark(&store.conn, &payload_ref)
            .await
            .map_err(|err| err.to_string())?
            .is_none()
    );
    assert!(
        payload::load_payload_metadata(&store.conn, &payload_ref)
            .await
            .is_ok()
    );
    Ok(())
}

#[tokio::test]
async fn run_payload_gc_isolates_corrupted_ref_errors_while_reaping_orphans() -> Result<(), String>
{
    let store = test_store().await?;
    let corrupted_ref = seed_payload(&store, "message-1", "trusted body").await?;
    drop_raw_reference(&store, &corrupted_ref).await?;
    fs::write(payload_path(&store, &corrupted_ref), b"tampered body")
        .map_err(|err| err.to_string())?;
    insert_gc_mark(&store, &corrupted_ref, "unreferenced", 1).await?;

    let orphan_a =
        "payload_1111111111111111111111111111111111111111111111111111111111111111.payload";
    let orphan_b =
        "payload_2222222222222222222222222222222222222222222222222222222222222222.payload";
    fs::write(payload_path(&store, orphan_a), b"orphan-a").map_err(|err| err.to_string())?;
    fs::write(payload_path(&store, orphan_b), b"orphan-b").map_err(|err| err.to_string())?;
    let orphan_a_mtime = file_mtime_seconds(
        &fs::symlink_metadata(payload_path(&store, orphan_a)).map_err(|err| err.to_string())?,
    );
    let orphan_b_mtime = file_mtime_seconds(
        &fs::symlink_metadata(payload_path(&store, orphan_b)).map_err(|err| err.to_string())?,
    );
    let newest_orphan_mtime = orphan_a_mtime.max(orphan_b_mtime);
    let cfg = LcmGcConfig {
        grace_seconds: LcmGcConfig::MIN_GRACE_SECONDS,
        backup_before_reap: false,
        ..Default::default()
    }
    .normalized();
    let report = run_payload_gc_with_apply(
        &store.conn,
        &store.storage_root,
        PROVIDER,
        None,
        &cfg,
        true,
        newest_orphan_mtime + LcmGcConfig::MIN_GRACE_SECONDS as i64,
    )
    .await
    .map_err(|err| err.to_string())?;

    assert_eq!(report.orphans.count, 2);
    assert_eq!(report.errors.len(), 1);
    assert_eq!(report.errors[0].payload_ref, corrupted_ref);
    assert_eq!(report.errors[0].kind, "integrity_mismatch");
    assert_eq!(
        schema::get_gc_meta(&store.conn, "last_gc_status")
            .await
            .map_err(|err| err.to_string())?
            .as_deref(),
        Some("partial")
    );
    assert!(
        payload::load_payload_metadata(&store.conn, &corrupted_ref)
            .await
            .is_ok()
    );
    assert!(!payload_path(&store, orphan_a).exists());
    assert!(!payload_path(&store, orphan_b).exists());
    Ok(())
}

#[tokio::test]
async fn unreadable_payload_path_never_reaps_live_metadata() -> Result<(), String> {
    let store = test_store().await?;
    let payload_ref = seed_payload(&store, "message-1", "live metadata").await?;
    let path = payload_path(&store, &payload_ref);
    fs::remove_file(&path).map_err(|err| err.to_string())?;
    fs::create_dir(&path).map_err(|err| err.to_string())?;
    insert_gc_mark(&store, &payload_ref, "missing", 1).await?;
    let cfg = LcmGcConfig {
        reap_missing_enabled: true,
        reap_missing_after: 10,
        backup_before_reap: false,
        ..Default::default()
    }
    .normalized();

    let report = run_payload_gc_with_apply(
        &store.conn,
        &store.storage_root,
        PROVIDER,
        None,
        &cfg,
        true,
        100,
    )
    .await
    .map_err(|err| err.to_string())?;

    assert_eq!(report.totals.rows_deleted, 0);
    assert!(
        report
            .errors
            .iter()
            .any(|error| error.payload_ref == payload_ref && error.kind == "payload_stat_failed")
    );
    assert!(
        payload::load_payload_metadata(&store.conn, &payload_ref)
            .await
            .is_ok()
    );
    assert!(path.is_dir());
    Ok(())
}

#[test]
fn committed_delete_quarantine_preserves_rename_replacement() -> Result<(), String> {
    let temp = tempfile::tempdir().map_err(|err| err.to_string())?;
    let dir = payload::payload_dir(temp.path());
    fs::create_dir(&dir).map_err(|err| err.to_string())?;
    let path = dir.join(PRIMARY_REF);
    let original = b"original payload";
    fs::write(&path, original).map_err(|err| err.to_string())?;
    let expected_hash = util::sha256_hex(original);

    let removal = payload::remove_committed_payload_file_with(
        temp.path(),
        PRIMARY_REF,
        Some(&expected_hash),
        original.len() as u64,
        Some(original.len() as u64),
        |original_path, _quarantine| {
            fs::write(original_path, b"replacement payload")
                .map_err(|err| LcmError::Io(err.to_string()))
        },
    )
    .map_err(|err| err.to_string())?;

    assert!(matches!(
        removal,
        payload::CommittedPayloadRemoval::Removed(bytes) if bytes == original.len() as u64
    ));
    assert_eq!(
        fs::read(&path).map_err(|err| err.to_string())?,
        b"replacement payload"
    );
    Ok(())
}

#[test]
fn committed_delete_quarantine_restores_in_place_rewrite() -> Result<(), String> {
    let temp = tempfile::tempdir().map_err(|err| err.to_string())?;
    let dir = payload::payload_dir(temp.path());
    fs::create_dir(&dir).map_err(|err| err.to_string())?;
    let path = dir.join(PRIMARY_REF);
    let original = b"original payload";
    fs::write(&path, original).map_err(|err| err.to_string())?;
    let expected_hash = util::sha256_hex(original);

    let removal = payload::remove_committed_payload_file_with(
        temp.path(),
        PRIMARY_REF,
        Some(&expected_hash),
        original.len() as u64,
        Some(original.len() as u64),
        |_original_path, quarantine| {
            fs::write(quarantine, b"rewritten payload").map_err(|err| LcmError::Io(err.to_string()))
        },
    )
    .map_err(|err| err.to_string())?;

    assert!(matches!(
        removal,
        payload::CommittedPayloadRemoval::ReplacementPreserved
    ));
    assert_eq!(
        fs::read(&path).map_err(|err| err.to_string())?,
        b"rewritten payload"
    );
    Ok(())
}

#[test]
fn committed_delete_requires_exact_hash_byte_and_char_sizes() -> Result<(), String> {
    let original = "héllo 雪";
    let expected_hash = util::sha256_hex(original.as_bytes());
    let expected_bytes = original.len() as u64;
    let expected_chars = original.chars().count() as u64;
    for (hash, bytes, chars) in [
        ("wrong".to_string(), expected_bytes, expected_chars),
        (expected_hash.clone(), expected_bytes + 1, expected_chars),
        (expected_hash.clone(), expected_bytes, expected_chars + 1),
    ] {
        let temp = tempfile::tempdir().map_err(|err| err.to_string())?;
        let dir = payload::payload_dir(temp.path());
        fs::create_dir(&dir).map_err(|err| err.to_string())?;
        let path = dir.join(PRIMARY_REF);
        fs::write(&path, original).map_err(|err| err.to_string())?;

        let removal = payload::remove_committed_payload_file(
            temp.path(),
            PRIMARY_REF,
            Some(&hash),
            bytes,
            Some(chars),
        )
        .map_err(|err| err.to_string())?;

        assert!(matches!(
            removal,
            payload::CommittedPayloadRemoval::ReplacementPreserved
        ));
        assert_eq!(
            fs::read_to_string(&path).map_err(|err| err.to_string())?,
            original
        );
    }
    Ok(())
}

#[test]
fn committed_delete_has_no_digest_or_size_fallback() -> Result<(), String> {
    let temp = tempfile::tempdir().map_err(|err| err.to_string())?;
    let dir = payload::payload_dir(temp.path());
    fs::create_dir(&dir).map_err(|err| err.to_string())?;
    let path = dir.join(PRIMARY_REF);
    fs::write(&path, b"preserve").map_err(|err| err.to_string())?;

    let removal = payload::remove_committed_payload_file(temp.path(), PRIMARY_REF, None, 8, None)
        .map_err(|err| err.to_string())?;

    assert!(matches!(
        removal,
        payload::CommittedPayloadRemoval::ReplacementPreserved
    ));
    assert_eq!(fs::read(&path).map_err(|err| err.to_string())?, b"preserve");
    Ok(())
}

#[test]
fn committed_delete_retry_succeeds_after_same_id_content_restore() -> Result<(), String> {
    let temp = tempfile::tempdir().map_err(|err| err.to_string())?;
    let dir = payload::payload_dir(temp.path());
    fs::create_dir(&dir).map_err(|err| err.to_string())?;
    let path = dir.join(PRIMARY_REF);
    let original = b"original";
    let expected_hash = util::sha256_hex(original);
    fs::write(&path, original).map_err(|err| err.to_string())?;

    let first = payload::remove_committed_payload_file_with(
        temp.path(),
        PRIMARY_REF,
        Some(&expected_hash),
        original.len() as u64,
        Some(original.len() as u64),
        |_original_path, quarantine| {
            fs::write(quarantine, b"mutated!").map_err(|err| LcmError::Io(err.to_string()))
        },
    )
    .map_err(|err| err.to_string())?;
    assert!(matches!(
        first,
        payload::CommittedPayloadRemoval::ReplacementPreserved
    ));

    fs::write(&path, original).map_err(|err| err.to_string())?;
    let retry = payload::remove_committed_payload_file(
        temp.path(),
        PRIMARY_REF,
        Some(&expected_hash),
        original.len() as u64,
        Some(original.len() as u64),
    )
    .map_err(|err| err.to_string())?;
    assert!(matches!(
        retry,
        payload::CommittedPayloadRemoval::Removed(bytes) if bytes == original.len() as u64
    ));
    assert!(!path.exists());
    Ok(())
}

// ---------------------------------------------------------------------------
// SQL batching regression coverage.
//
// These tests measure *work*: how many round trips a GC path issues, how many
// rows those round trips visit, and which rows survive. Nothing here inspects
// statement text, so a query rewrite that preserves the work a pass does keeps
// the gate green, while a regression back to per-row SQL breaks it. Elapsed
// time is never asserted: a set-sized workload costing a fixed number of round
// trips is a property of the access pattern, not of the machine.
// ---------------------------------------------------------------------------

/// Counts the work forwarded through it: one tick per round trip, plus the rows
/// each query actually returned. It never retains statement text.
#[derive(Default)]
struct WorkCounter {
    round_trips: std::cell::Cell<usize>,
    rows_visited: std::cell::Cell<usize>,
}

impl WorkCounter {
    fn round_trips(&self) -> usize {
        self.round_trips.get()
    }

    fn rows_visited(&self) -> usize {
        self.rows_visited.get()
    }

    fn tick(&self) {
        self.round_trips
            .set(self.round_trips.get().saturating_add(1));
    }

    fn add_rows(&self, rows: usize) {
        self.rows_visited
            .set(self.rows_visited.get().saturating_add(rows));
    }
}

/// Transparent `Executor` wrapper that counts the work it forwards.
struct CountingExecutor<'a, E: ?Sized> {
    inner: &'a E,
    counter: &'a WorkCounter,
}

impl<E: QueryExecutor + ?Sized> QueryExecutor for CountingExecutor<'_, E> {
    async fn query<P>(
        &self,
        sql: &str,
        params: P,
    ) -> tracedecay_runtime_core::db::engine::Result<tracedecay_runtime_core::db::engine::Rows>
    where
        P: tracedecay_runtime_core::db::engine::IntoParams,
    {
        use tracedecay_runtime_core::db::engine::{Row, Rows, Value};

        self.counter.tick();
        let mut rows = self.inner.query(sql, params).await?;
        // Drain and replay so the row count is measured, not estimated. The
        // replayed `Rows` is indistinguishable to the caller: same column
        // names, same values, same order.
        let columns = (0..rows.column_count())
            .map(|index| rows.column_name(index).unwrap_or_default().to_string())
            .collect::<Vec<_>>();
        let mut replay = Vec::new();
        while let Some(row) = rows.next().await? {
            let mut values = Vec::new();
            let mut column = 0_i32;
            while let Ok(value) = row.get::<Value>(column) {
                values.push(value);
                column += 1;
            }
            replay.push(Row::from_values(values));
        }
        self.counter.add_rows(replay.len());
        Ok(Rows::from_parts(columns, replay))
    }
}

impl<E: Executor + ?Sized> Executor for CountingExecutor<'_, E> {
    async fn execute<P>(
        &self,
        sql: &str,
        params: P,
    ) -> tracedecay_runtime_core::db::engine::Result<u64>
    where
        P: tracedecay_runtime_core::db::engine::IntoParams,
    {
        self.counter.tick();
        self.inner.execute(sql, params).await
    }

    async fn execute_batch(&self, sql: &str) -> tracedecay_runtime_core::db::engine::Result<()> {
        self.counter.tick();
        self.inner.execute_batch(sql).await
    }
}

fn batch_ref(index: usize) -> String {
    format!("payload_batch_{index:04}.payload")
}

/// Stages `count` pending-delete tombstones whose payloads exist on disk and
/// own no metadata row, then drains them under a work counter. Returns the
/// round trips the drain cost.
async fn drain_round_trips_for_tombstones(count: usize) -> Result<usize, String> {
    let store = test_store().await?;
    let dir = payload::payload_dir(&store.storage_root);
    fs::create_dir_all(&dir).map_err(|err| err.to_string())?;

    let mut refs = Vec::new();
    for index in 0..count {
        let payload_ref = batch_ref(index);
        fs::write(dir.join(&payload_ref), format!("body {index}").as_bytes())
            .map_err(|err| err.to_string())?;
        let (hash, bytes, chars) =
            payload::payload_file_fingerprint(&dir, &payload_ref).map_err(|err| err.to_string())?;
        stage_payload_delete(&store.conn, &payload_ref, Some(&hash), bytes, chars)
            .await
            .map_err(|err| err.to_string())?;
        refs.push(payload_ref);
    }

    let counter = WorkCounter::default();
    let transaction = store
        .conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .map_err(|err| err.to_string())?;
    let drain = {
        let counting = CountingExecutor {
            inner: &transaction,
            counter: &counter,
        };
        drain_pending_payload_deletes_in_transaction(&counting, &store.storage_root)
            .await
            .map_err(|err| err.to_string())?
    };
    transaction.commit().await.map_err(|err| err.to_string())?;

    // The drain must actually have done its job, or a "cheap" round-trip count
    // would be measuring a no-op.
    assert_eq!(drain.outcomes.removed.count, count);
    assert_eq!(drain.outcomes.failed.count, 0);
    for payload_ref in &refs {
        assert!(
            !dir.join(payload_ref).exists(),
            "{payload_ref} still on disk"
        );
        assert!(
            schema::get_gc_meta(&store.conn, &pending_payload_delete_key(payload_ref))
                .await
                .map_err(|err| err.to_string())?
                .is_none(),
            "{payload_ref} tombstone not cleared"
        );
    }
    Ok(counter.round_trips())
}

/// M11: the pending-delete drain probes `lcm_external_payloads` once for the
/// whole tombstone set, not once per tombstone.
///
/// Measured, not read off the SQL: drain two set sizes and compare the round
/// trips. Each extra tombstone still costs its own tombstone clear, and that
/// marginal is pinned below; a per-tombstone existence probe would raise it by
/// one and fail the gate, whatever the statements happen to say.
#[tokio::test]
async fn pending_delete_drain_probes_metadata_once_for_the_whole_set() -> Result<(), String> {
    /// Round trips one additional tombstone adds: the `gc_meta` clear that
    /// retires that tombstone. The batched existence probe is *not* here — it
    /// is paid once for the whole drain.
    const PER_TOMBSTONE_ROUND_TRIPS: usize = 1;
    const SMALL: usize = 2;
    const LARGE: usize = 8;

    let small = drain_round_trips_for_tombstones(SMALL).await?;
    let large = drain_round_trips_for_tombstones(LARGE).await?;

    assert_eq!(
        large - small,
        (LARGE - SMALL) * PER_TOMBSTONE_ROUND_TRIPS,
        "drain cost {small} round trips for {SMALL} tombstones and {large} for {LARGE}: \
         the per-tombstone marginal is not {PER_TOMBSTONE_ROUND_TRIPS}, so something in the \
         loop is still issuing its own query"
    );
    Ok(())
}

/// M11 equivalence: a tombstone whose metadata row is still present must be
/// preserved (not unlinked) and a tombstone whose row is gone must be reaped,
/// in the same drain — the batched probe must not conflate the two.
#[tokio::test]
async fn pending_delete_drain_batches_mixed_metadata_presence() -> Result<(), String> {
    let store = test_store().await?;
    let dir = payload::payload_dir(&store.storage_root);
    fs::create_dir_all(&dir).map_err(|err| err.to_string())?;

    // Tombstone A: metadata row still present -> preserved.
    let live_ref = seed_payload(&store, "message-live", "live body").await?;
    let (live_hash, live_bytes, live_chars) =
        payload::payload_file_fingerprint(&dir, &live_ref).map_err(|err| err.to_string())?;
    stage_payload_delete(
        &store.conn,
        &live_ref,
        Some(&live_hash),
        live_bytes,
        live_chars,
    )
    .await
    .map_err(|err| err.to_string())?;

    // Tombstone B: no metadata row -> removed.
    let dead_ref = batch_ref(99);
    fs::write(dir.join(&dead_ref), b"dead body").map_err(|err| err.to_string())?;
    let (dead_hash, dead_bytes, dead_chars) =
        payload::payload_file_fingerprint(&dir, &dead_ref).map_err(|err| err.to_string())?;
    stage_payload_delete(
        &store.conn,
        &dead_ref,
        Some(&dead_hash),
        dead_bytes,
        dead_chars,
    )
    .await
    .map_err(|err| err.to_string())?;

    let drain = drain_pending_payload_deletes(&store.conn, &store.storage_root)
        .await
        .map_err(|err| err.to_string())?;

    assert_eq!(drain.outcomes.preserved.refs, [live_ref.clone()]);
    assert_eq!(drain.outcomes.removed.refs, [dead_ref.clone()]);
    assert!(dir.join(&live_ref).is_file(), "live payload was unlinked");
    assert!(!dir.join(&dead_ref).exists(), "dead payload survived");
    Ok(())
}

/// Seeds `count` payloads that are on disk with metadata rows, carry no live
/// reference, and already hold an aged `unreferenced` GC mark, so one apply pass
/// reaps all of them.
async fn seed_reapable_payloads(store: &TestStore, count: usize) -> Result<Vec<String>, String> {
    let mut refs = Vec::new();
    for index in 0..count {
        let payload_ref =
            seed_payload(store, &format!("message-{index}"), &format!("body {index}")).await?;
        drop_raw_reference(store, &payload_ref).await?;
        insert_gc_mark(store, &payload_ref, "unreferenced", 0).await?;
        refs.push(payload_ref);
    }
    Ok(refs)
}

/// Reaps `count` aged, unreferenced payloads in one apply pass under a work
/// counter, asserting every one of them was actually reaped, and returns the
/// round trips the pass cost.
async fn unreferenced_reap_round_trips(count: usize) -> Result<usize, String> {
    let store = test_store().await?;
    let refs = seed_reapable_payloads(&store, count).await?;
    let cfg = LcmGcConfig {
        grace_seconds: LcmGcConfig::MIN_GRACE_SECONDS,
        backup_before_reap: false,
        max_batch_size: 64,
        ..Default::default()
    }
    .normalized();

    let counter = WorkCounter::default();
    let transaction = store
        .conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .map_err(|err| err.to_string())?;
    let report = {
        let counting = CountingExecutor {
            inner: &transaction,
            counter: &counter,
        };
        run_payload_gc_in_transaction(
            &counting,
            &store.storage_root,
            PROVIDER,
            None,
            &cfg,
            true,
            1_000_000,
        )
        .await
        .map_err(|err| err.to_string())?
    };
    transaction.commit().await.map_err(|err| err.to_string())?;

    assert_eq!(report.unreferenced.count, count);
    for payload_ref in &refs {
        assert!(
            payload::load_payload_metadata(&store.conn, payload_ref)
                .await
                .is_err(),
            "{payload_ref} metadata survived"
        );
    }
    Ok(counter.round_trips())
}

/// M1: the reference-closure scan is hoisted out of the reap loop, so it costs
/// the pass a fixed amount however many payloads the batch reaps.
///
/// Measured as a marginal, not read off the SQL: reap two batch sizes and
/// compare. Each extra payload still pays for the work that is irreducibly its
/// own — its metadata read, its placeholder sweep, its row deletes. What must
/// *not* be in the marginal is a pass-level query; if a reference-closure scan
/// creeps back into the loop the marginal rises and this fails, whatever the
/// statement text looks like.
///
/// The reap loop prepares each delete and clears the whole batch's GC marks in
/// one bounded statement afterwards, so the mark delete is not in the marginal
/// either. That is a batch-only property: `delete_external_payload_in_transaction`
/// still clears a single payload's own mark, and no batching removes that —
/// `delete_external_payload_applies_db_then_file_and_is_idempotent` gates it.
#[tokio::test]
async fn unreferenced_reap_scans_reference_closure_once_for_the_batch() -> Result<(), String> {
    /// Round trips one additional reaped payload adds, measured. It covers the
    /// work that is irreducibly that payload's own: loading its metadata row,
    /// its residual-placeholder sweep, its metadata-row delete, and its
    /// pending-delete tombstone write. Neither a reference-closure scan nor a
    /// GC-mark delete is in there — both are paid once for the batch, and that
    /// is what this test guards.
    const PER_PAYLOAD_ROUND_TRIPS: usize = 4;
    const SMALL: usize = 2;
    const LARGE: usize = 8;

    let small = unreferenced_reap_round_trips(SMALL).await?;
    let large = unreferenced_reap_round_trips(LARGE).await?;

    assert_eq!(
        large - small,
        (LARGE - SMALL) * PER_PAYLOAD_ROUND_TRIPS,
        "reap cost {small} round trips for {SMALL} payloads and {large} for {LARGE}: \
         the per-payload marginal is not {PER_PAYLOAD_ROUND_TRIPS}, so the reference-closure \
         scan, the GC-mark delete, or another batch-level statement is back inside the \
         per-payload loop"
    );
    Ok(())
}

/// M1 equivalence: a payload that *is* still referenced must still abort with
/// `StillReferenced` even when it shares a cached closure with payloads that
/// were reaped earlier in the same batch.
#[tokio::test]
async fn shared_reference_closure_still_rejects_a_referenced_payload() -> Result<(), String> {
    let store = test_store().await?;
    let reaped = seed_payload(&store, "message-reaped", "reap me").await?;
    drop_raw_reference(&store, &reaped).await?;
    let kept = seed_payload(&store, "message-kept", "keep me").await?;

    let transaction = store
        .conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .map_err(|err| err.to_string())?;
    let mut cache = payload::ReferencedClosureCache::default();
    payload::prepare_external_payload_delete_in_transaction_with_cache(
        &transaction,
        &store.storage_root,
        &reaped,
        &payload::DeleteOpts::default(),
        &mut cache,
    )
    .await
    .map_err(|err| err.to_string())?;
    let still_referenced = payload::prepare_external_payload_delete_in_transaction_with_cache(
        &transaction,
        &store.storage_root,
        &kept,
        &payload::DeleteOpts::default(),
        &mut cache,
    )
    .await;
    transaction.commit().await.map_err(|err| err.to_string())?;

    assert!(
        matches!(still_referenced, Err(LcmError::StillReferenced)),
        "referenced payload was not rejected"
    );
    assert!(
        payload::load_payload_metadata(&store.conn, &kept)
            .await
            .is_ok(),
        "referenced payload metadata was deleted"
    );
    assert!(
        payload::load_payload_metadata(&store.conn, &reaped)
            .await
            .is_err(),
        "unreferenced payload was not deleted"
    );
    Ok(())
}

/// Tombstones `PRIMARY_REF` in a store holding one live placeholder plus
/// `decoys` inline-prose rows that merely name the ref, and returns how many
/// rows the delete's queries visited.
///
/// The payload deliberately has no metadata row, which is the state the
/// missing-metadata reap and the crash-recovery path both operate in. That
/// keeps the live-reference closure scan — a different, deliberately broad
/// query this PR does not touch — out of the measurement, so what is counted
/// is the residual-placeholder sweep's own selectivity.
async fn residual_sweep_rows_visited(decoys: usize) -> Result<usize, String> {
    let store = test_store().await?;
    insert_session(&store.conn, &store.storage_root, "session-a").await?;
    let live = format!("[externalized tool output: bytes=4 ref={PRIMARY_REF}; out]");
    insert_raw_message(
        &store.conn,
        RawMessage {
            session_id: "session-a",
            message_id: "message-live",
            storage_kind: "inline",
            payload_ref: None,
            content: Some(&live),
            snippet_text: &live,
            index_text: &live,
            metadata_json: Some(&live),
        },
    )
    .await?;

    for index in 0..decoys {
        let prose = format!("the operator mentioned {PRIMARY_REF} in note {index}");
        insert_raw_message(
            &store.conn,
            RawMessage {
                session_id: "session-a",
                message_id: &format!("message-decoy-{index}"),
                storage_kind: "inline",
                payload_ref: None,
                content: Some(&prose),
                snippet_text: &prose,
                index_text: &prose,
                metadata_json: Some(&prose),
            },
        )
        .await?;
    }

    let counter = WorkCounter::default();
    let transaction = store
        .conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .map_err(|err| err.to_string())?;
    {
        let counting = CountingExecutor {
            inner: &transaction,
            counter: &counter,
        };
        payload::delete_external_payload_in_transaction(
            &counting,
            &store.storage_root,
            PRIMARY_REF,
            &payload::DeleteOpts {
                rewrite_placeholders: true,
                remove_file: false,
                verify_hash: false,
            },
        )
        .await
        .map_err(|err| err.to_string())?;
    }
    transaction.commit().await.map_err(|err| err.to_string())?;

    // The sweep must still have tombstoned the row that needed it, or a low
    // row count would only mean the prefilter matched nothing at all.
    let mut rows = store
        .conn
        .query(
            "SELECT snippet_text FROM lcm_raw_messages WHERE message_id = 'message-live'",
            (),
        )
        .await
        .map_err(|err| err.to_string())?;
    let row = rows
        .next()
        .await
        .map_err(|err| err.to_string())?
        .ok_or_else(|| "tombstoned row vanished".to_string())?;
    let snippet: String = row.get(0).map_err(|err| err.to_string())?;
    drop(rows);
    assert!(
        text_has_tombstoned_payload_ref(&snippet, PRIMARY_REF),
        "sweep did not tombstone the live placeholder: {snippet}"
    );

    Ok(counter.rows_visited())
}

/// M2: the residual-placeholder sweep prefilters on live-prefix + ref rather
/// than a bare `%ref%`, so its cost is set by the rows that can actually be
/// rewritten, not by every row that happens to name the ref.
///
/// Measured as rows visited, not as `LIKE` terms counted in the statement text:
/// a bare `%ref%` prefilter pulls inline prose that merely mentions the ref
/// back into the sweep, so its row count grows with the decoys. The narrowed
/// prefilter excludes them and the row count stays flat.
#[tokio::test]
async fn residual_placeholder_sweep_prefilters_on_live_prefixes() -> Result<(), String> {
    let without_decoys = residual_sweep_rows_visited(0).await?;
    let with_decoys = residual_sweep_rows_visited(32).await?;

    assert_eq!(
        with_decoys, without_decoys,
        "sweep visited {without_decoys} rows with no decoys and {with_decoys} with 32 of them: \
         the prefilter is matching rows it can never rewrite, which is what a bare `%ref%` \
         pattern does"
    );
    Ok(())
}

/// M2 equivalence: the narrowed prefilter must rewrite exactly the rows the bare
/// `%ref%` form rewrote — live placeholders in every text column, plus the
/// stored `payload_ref` — and must leave inline prose that merely mentions the
/// ref, and already-tombstoned placeholders, untouched.
#[tokio::test]
async fn narrowed_prefilter_rewrites_the_same_rows() -> Result<(), String> {
    let store = test_store().await?;
    let payload_ref = seed_payload(&store, "message-live", "body to tombstone").await?;

    let live = format!("[externalized tool output: bytes=4 ref={payload_ref}; out]");
    let already_gcd = format!("[gc'd externalized payload: bytes=4 ref={payload_ref}; gone]");
    let prose = format!("the operator mentioned {payload_ref} in a note");
    insert_raw_message(
        &store.conn,
        RawMessage {
            session_id: "session-a",
            message_id: "message-other-live",
            storage_kind: "inline",
            payload_ref: None,
            content: Some(&live),
            snippet_text: &live,
            index_text: &live,
            metadata_json: Some(&live),
        },
    )
    .await?;
    insert_raw_message(
        &store.conn,
        RawMessage {
            session_id: "session-a",
            message_id: "message-already-gcd",
            storage_kind: "inline",
            payload_ref: None,
            content: Some(&already_gcd),
            snippet_text: &already_gcd,
            index_text: &already_gcd,
            metadata_json: Some(&already_gcd),
        },
    )
    .await?;
    insert_raw_message(
        &store.conn,
        RawMessage {
            session_id: "session-a",
            message_id: "message-prose",
            storage_kind: "inline",
            payload_ref: None,
            content: Some(&prose),
            snippet_text: &prose,
            index_text: &prose,
            metadata_json: Some(&prose),
        },
    )
    .await?;

    let transaction = store
        .conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .map_err(|err| err.to_string())?;
    payload::delete_external_payload_in_transaction(
        &transaction,
        &store.storage_root,
        &payload_ref,
        &payload::DeleteOpts {
            rewrite_placeholders: true,
            remove_file: false,
            verify_hash: false,
        },
    )
    .await
    .map_err(|err| err.to_string())?;
    transaction.commit().await.map_err(|err| err.to_string())?;

    let mut rows = store
        .conn
        .query(
            "SELECT message_id, storage_kind, payload_ref, snippet_text, index_text
             FROM lcm_raw_messages ORDER BY message_id",
            (),
        )
        .await
        .map_err(|err| err.to_string())?;
    let mut seen = Vec::new();
    while let Some(row) = rows.next().await.map_err(|err| err.to_string())? {
        let message_id: String = row.get(0).map_err(|err| err.to_string())?;
        let storage_kind: String = row.get(1).map_err(|err| err.to_string())?;
        let stored_ref: Option<String> = row.get(2).unwrap_or(None);
        let snippet: String = row.get(3).map_err(|err| err.to_string())?;
        let index_text: String = row.get(4).map_err(|err| err.to_string())?;
        seen.push((message_id, storage_kind, stored_ref, snippet, index_text));
    }
    drop(rows);

    for (message_id, storage_kind, stored_ref, snippet, index_text) in seen {
        match message_id.as_str() {
            "message-live" => {
                assert_eq!(storage_kind, "inline", "external row was not inlined");
                assert_eq!(stored_ref, None, "stored payload_ref was not cleared");
                assert!(text_has_tombstoned_payload_ref(&snippet, &payload_ref));
                assert!(text_has_tombstoned_payload_ref(&index_text, &payload_ref));
            }
            "message-other-live" => {
                assert!(
                    text_has_tombstoned_payload_ref(&snippet, &payload_ref),
                    "live tool-output placeholder was not tombstoned: {snippet}"
                );
                assert!(text_has_tombstoned_payload_ref(&index_text, &payload_ref));
            }
            "message-already-gcd" => {
                assert_eq!(snippet, already_gcd, "already-tombstoned row was rewritten");
                assert_eq!(index_text, already_gcd);
            }
            "message-prose" => {
                assert_eq!(snippet, prose, "inline prose was rewritten");
                assert_eq!(index_text, prose);
            }
            other => return Err(format!("unexpected row {other}")),
        }
    }
    Ok(())
}
