//! At-rest LCM privacy rescan: legacy rows written under older detector
//! rules are re-scanned and remediated through the canonical ingest path.

use serde_json::{Value, json};
use tracedecay_domain::{
    ComponentVersion, PayloadReferenceV1, SanitizationReceiptId, SanitizationReceiptRefV1,
    SanitizationReceiptV1, SanitizerDispositionV1, SensitivityV1,
};
use tracedecay_lcm::retrieval_content::projected_content_hash;
use tracedecay_lcm::{payload, schema};
use tracedecay_privacy::{
    LCM_PAYLOAD_SANITIZER_VERSION_V1, lcm_payload_detector_revision, sanitize_lcm_payload_text,
};
use tracedecay_runtime_core::db::engine::params;
use tracedecay_sessions::runtime::SessionMessageRecord;

use crate::LcmPrivacyRescanOutcomeV1;
use crate::registered_lcm_privacy::LCM_PRIVACY_RESCAN_META_KEY;
use crate::tests::harness::RegisteredGlobalDbHarness;

fn secret() -> String {
    ["sk-at-rest-rescan-secret-", "1234567890abcdef"].concat()
}

/// A receipt exactly as an older binary bound it: the stored bytes are the
/// receipt's payload, accepted as non-sensitive, under the pinned sanitizer
/// contract, but the current detector rules never evaluated them.
pub(crate) fn legacy_receipt(content: &str) -> SanitizationReceiptV1 {
    let payload_reference = PayloadReferenceV1::for_payload(&Value::String(content.to_owned()))
        .expect("legacy payload reference");
    let sanitizer_version =
        ComponentVersion::new(LCM_PAYLOAD_SANITIZER_VERSION_V1).expect("pinned sanitizer contract");
    let receipt_id = SanitizationReceiptId::new(format!(
        "privacy.lcm-payload.v1.{}",
        payload_reference.digest().as_str()
    ))
    .expect("legacy receipt id");
    SanitizationReceiptV1::new(
        SanitizationReceiptRefV1::new(receipt_id, sanitizer_version).expect("legacy receipt ref"),
        SanitizerDispositionV1::Accepted,
        SensitivityV1::NonSensitive,
        Some(payload_reference),
    )
    .expect("legacy receipt")
}

async fn seed_session(harness: &RegisteredGlobalDbHarness, session_id: &str) {
    harness
        .registered
        .writer_connection()
        .expect("writer")
        .execute(
            "INSERT INTO sessions(provider, session_id, project_key, project_path)
             VALUES ('cursor', ?1, '/tmp/project', '/tmp/project')",
            params![session_id],
        )
        .await
        .expect("seed session");
}

/// Persists one inline message row exactly as an older
/// ingest could have: receipt-bound bytes the current rules never evaluated.
async fn seed_legacy_inline_row(
    harness: &RegisteredGlobalDbHarness,
    session_id: &str,
    message_id: &str,
    content: &str,
) {
    let metadata = json!({
        "fixture": "legacy-inline",
        "ingest_protection": { "sanitization_receipt": legacy_receipt(content) }
    })
    .to_string();
    let writer = harness.registered.writer_connection().expect("writer");
    writer
        .execute(
            "INSERT INTO lcm_raw_messages(
                provider, message_id, session_id, role, ordinal, timestamp,
                content, content_hash, storage_kind, payload_ref, metadata_json
             )
             VALUES ('cursor', ?1, ?2, 'user', 1, 10, ?3, ?4, 'inline', NULL, ?5)",
            params![
                message_id,
                session_id,
                content,
                projected_content_hash(content),
                metadata
            ],
        )
        .await
        .expect("seed legacy inline raw row");
}

/// Persists one external raw row whose at-rest payload file holds bytes the
/// current detector would redact.
async fn seed_legacy_external_row(
    harness: &RegisteredGlobalDbHarness,
    session_id: &str,
    message_id: &str,
    content: &str,
) -> String {
    let storage_root = harness
        .registered
        .db_path()
        .parent()
        .expect("registered database storage root")
        .to_path_buf();
    let mut rollback = payload::PayloadFileRollback::begin_cancellation_safe(&storage_root);
    let payload_ref = payload::write_external_payload_tracked(
        &storage_root,
        payload::ExternalPayloadWrite {
            provider: "cursor",
            session_id,
            message_id,
            kind: "tool_output",
            content,
            metadata_json: None,
        },
        &mut rollback,
    )
    .expect("write legacy external payload");
    let placeholder = format!(
        "[Externalized LCM ingest payload: kind=tool_output; field=content; chars={}; bytes={}; ref={}]",
        payload_ref.char_count, payload_ref.byte_count, payload_ref.payload_ref
    );
    let metadata = json!({
        "external_payload": true,
        "payload_ref": payload_ref.payload_ref,
        "kind": "tool_output",
        "byte_count": payload_ref.byte_count,
        "char_count": payload_ref.char_count,
        "sha256": payload_ref.content_hash,
        "ingest_protection": { "sanitization_receipt": legacy_receipt(content) }
    })
    .to_string();
    let transaction = harness
        .registered
        .begin_write_transaction()
        .await
        .expect("seed transaction");
    payload::upsert_payload_metadata(&transaction, &payload_ref)
        .await
        .expect("seed external payload metadata");
    transaction
        .execute(
            "INSERT INTO lcm_raw_messages(
                provider, message_id, session_id, role, ordinal, timestamp,
                content, content_hash, storage_kind, payload_ref, placeholder_text,
                metadata_json
             )
             VALUES ('cursor', ?1, ?2, 'user', 2, 20, NULL, ?3, 'external', ?4, ?5, ?6)",
            params![
                message_id,
                session_id,
                payload_ref.content_hash.as_str(),
                payload_ref.payload_ref.as_str(),
                placeholder.as_str(),
                metadata
            ],
        )
        .await
        .expect("seed legacy external raw row");
    transaction.commit().await.expect("commit seed");
    rollback.disarm();
    payload_ref.payload_ref
}

/// Persists one projection-landed row without a sanitization receipt: the
/// shape the bulk observation rebuild writes and the protect pass owns.
async fn seed_unreceipted_row(
    harness: &RegisteredGlobalDbHarness,
    session_id: &str,
    message_id: &str,
    content: &str,
) {
    let writer = harness.registered.writer_connection().expect("writer");
    writer
        .execute(
            "INSERT INTO lcm_raw_messages(
                provider, message_id, session_id, role, ordinal, timestamp,
                content, content_hash, storage_kind, payload_ref, metadata_json
             )
             VALUES ('cursor', ?1, ?2, 'assistant', 3, 30, ?3, ?4, 'inline', NULL, NULL)",
            params![
                message_id,
                session_id,
                content,
                projected_content_hash(content)
            ],
        )
        .await
        .expect("seed unreceipted raw row");
}

async fn count_rows_holding(harness: &RegisteredGlobalDbHarness, needle: &str) -> i64 {
    let snapshot = harness.registered.read_snapshot().await.expect("snapshot");
    let pattern = format!("%{needle}%");
    let mut rows = snapshot
        .query(
            "SELECT
                (SELECT COUNT(*) FROM lcm_raw_messages
                 WHERE COALESCE(content, '') LIKE ?1
                    OR snippet_text LIKE ?1
                    OR index_text LIKE ?1
                    OR COALESCE(metadata_json, '') LIKE ?1)",
            params![pattern],
        )
        .await
        .expect("count query");
    rows.next()
        .await
        .expect("count row")
        .expect("count present")
        .get(0)
        .expect("count value")
}

fn payload_dir_holds(storage_root: &std::path::Path, needle: &str) -> bool {
    let dir = payload::payload_dir(storage_root);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return false;
    };
    entries.filter_map(Result::ok).any(|entry| {
        std::fs::read(entry.path())
            .is_ok_and(|bytes| String::from_utf8_lossy(&bytes).contains(needle))
    })
}

#[asupersync::test]
async fn at_rest_rescan_remediates_legacy_rows_and_settles_watermark() {
    let harness = RegisteredGlobalDbHarness::open("lcm-privacy-rescan").await;
    let storage_root = harness
        .registered
        .db_path()
        .parent()
        .expect("registered database storage root")
        .to_path_buf();
    let session_id = "privacy-rescan-session";
    seed_session(&harness, session_id).await;

    // A message ingested through the current path stays untouched.
    harness
        .registered
        .lcm_ingest_raw_message(
            &storage_root,
            &SessionMessageRecord {
                provider: "cursor".to_owned(),
                message_id: "clean-message".to_owned(),
                session_id: session_id.to_owned(),
                role: "user".to_owned(),
                timestamp: Some(5),
                ordinal: 0,
                text: "the retry budget is three attempts".to_owned(),
                kind: None,
                model: None,
                tool_names: None,
                source_path: None,
                source_offset: None,
                metadata_json: None,
            },
        )
        .await
        .expect("ingest clean message");

    let inline_content = format!(
        "the deploy pipeline authenticates with api_key={}",
        secret()
    );
    let inline_sanitized = sanitize_lcm_payload_text(&inline_content).expect("evaluate fixture");
    assert_ne!(
        inline_sanitized.sanitized_text(),
        inline_content,
        "the fixture must be a value the current detector redacts"
    );
    seed_legacy_inline_row(&harness, session_id, "legacy-inline", &inline_content).await;

    let external_content = format!("captured tool output leaked api_key={}", secret());
    let old_payload_ref =
        seed_legacy_external_row(&harness, session_id, "legacy-external", &external_content).await;
    let old_payload_path = payload::payload_dir(&storage_root).join(&old_payload_ref);
    assert!(old_payload_path.is_file(), "seeded payload must be at rest");

    seed_unreceipted_row(
        &harness,
        session_id,
        "unreceipted-message",
        "projection-landed row without a receipt",
    )
    .await;

    assert!(count_rows_holding(&harness, &secret()).await > 0);
    assert!(payload_dir_holds(&storage_root, &secret()));

    let outcome = harness
        .registered
        .lcm_privacy_rescan_raw_messages()
        .await
        .expect("at-rest rescan");
    let LcmPrivacyRescanOutcomeV1::Completed(receipt) = outcome else {
        panic!("first rescan must complete a full pass: {outcome:?}");
    };
    assert_eq!(receipt.detector_revision, lcm_payload_detector_revision());
    assert_eq!(receipt.protected_rows, 1);
    assert_eq!(receipt.scanned_rows, 4);
    assert_eq!(receipt.clean_rows, 2);
    assert_eq!(receipt.remediated_rows, 2);
    assert_eq!(receipt.unavailable_payload_rows, 0);

    // The detector hit is gone from every at-rest surface: message rows and
    // the payload directory (the replaced payload file
    // is deleted, not merely superseded).
    assert_eq!(count_rows_holding(&harness, &secret()).await, 0);
    assert!(!payload_dir_holds(&storage_root, &secret()));
    assert!(
        !old_payload_path.exists(),
        "replaced payload must be deleted"
    );

    // The remediated inline row still serves through the verified raw-read
    // authority, with a fresh receipt and its provider metadata retained.
    let snapshot = harness.registered.read_snapshot().await.expect("snapshot");
    let remediated = schema::load_raw_message(&snapshot, "cursor", "legacy-inline")
        .await
        .expect("verified load of remediated row")
        .expect("remediated row still serves");
    assert!(
        remediated
            .content
            .contains("the deploy pipeline authenticates")
    );
    assert!(!remediated.content.contains(&secret()));
    let metadata: Value =
        serde_json::from_str(remediated.metadata_json.as_deref().expect("metadata"))
            .expect("metadata JSON");
    assert_eq!(metadata["fixture"], json!("legacy-inline"));
    assert_eq!(metadata["ingest_protection"]["redacted"], json!(true));

    // The unreceipted row now carries a receipt bound by the protect pass.
    let protected = schema::load_raw_message(&snapshot, "cursor", "unreceipted-message")
        .await
        .expect("verified load of protected row")
        .expect("protected row serves");
    assert_eq!(protected.content, "projection-landed row without a receipt");
    drop(snapshot);

    // A second request is answered by the watermark without scanning.
    assert_eq!(
        harness
            .registered
            .lcm_privacy_rescan_raw_messages()
            .await
            .expect("watermarked rescan"),
        LcmPrivacyRescanOutcomeV1::AlreadyCurrent
    );

    // A forced repeat pass (rule-refresh simulation: watermark cleared) finds
    // the remediated store clean and settles nothing further.
    let transaction = harness
        .registered
        .begin_write_transaction()
        .await
        .expect("watermark transaction");
    schema::clear_gc_meta(&transaction, LCM_PRIVACY_RESCAN_META_KEY)
        .await
        .expect("clear watermark");
    transaction.commit().await.expect("commit watermark clear");
    let outcome = harness
        .registered
        .lcm_privacy_rescan_raw_messages()
        .await
        .expect("repeat rescan");
    let LcmPrivacyRescanOutcomeV1::Completed(repeat) = outcome else {
        panic!("cleared watermark must force a full pass: {outcome:?}");
    };
    assert_eq!(repeat.scanned_rows, 4);
    assert_eq!(repeat.clean_rows, 4);
    assert_eq!(repeat.remediated_rows, 0);
    assert_eq!(repeat.protected_rows, 0);
}

#[asupersync::test]
async fn rescan_hydrates_canonical_inline_bodies() {
    let harness = RegisteredGlobalDbHarness::open("privacy-canonical-body").await;
    let storage_root = harness
        .registered
        .db_path()
        .parent()
        .expect("store root")
        .to_path_buf();
    let session_id = "canonical-body-session";
    seed_session(&harness, session_id).await;
    let text = "ordinary captured output\n".repeat(200);
    harness
        .registered
        .lcm_ingest_raw_message(
            &storage_root,
            &SessionMessageRecord {
                provider: "cursor".to_owned(),
                message_id: "large".to_owned(),
                session_id: session_id.to_owned(),
                role: "user".to_owned(),
                timestamp: Some(1),
                ordinal: 0,
                text: text.clone(),
                kind: None,
                model: None,
                tool_names: None,
                source_path: None,
                source_offset: None,
                metadata_json: None,
            },
        )
        .await
        .expect("ingest canonical body");
    let outcome = harness
        .registered
        .lcm_privacy_rescan_raw_messages()
        .await
        .expect("rescan valid canonical body");
    let LcmPrivacyRescanOutcomeV1::Completed(receipt) = outcome else {
        panic!("first scan must run")
    };
    assert_eq!(receipt.scanned_rows, 1);
    assert_eq!(receipt.clean_rows, 1);
    let snapshot = harness
        .registered
        .lcm_read_snapshot()
        .await
        .expect("snapshot");
    let message =
        tracedecay_lcm::raw::load_raw_message_by_identity(&snapshot, "cursor", session_id, "large")
            .await
            .expect("hydrate message")
            .expect("message present");
    assert_eq!(message.content, text);
}

#[asupersync::test]
async fn rescan_remediates_external_metadata_even_when_body_is_unavailable() {
    for unavailable in [false, true] {
        let harness = RegisteredGlobalDbHarness::open("privacy-external-metadata").await;
        let root = harness
            .registered
            .db_path()
            .parent()
            .expect("root")
            .to_path_buf();
        seed_session(&harness, "metadata-session").await;
        let body = "clean historical external body";
        let reference =
            seed_legacy_external_row(&harness, "metadata-session", "external", body).await;
        let path = payload::payload_dir(&root).join(&reference);
        let before = std::fs::read(&path).expect("payload");
        let writer = harness.registered.writer_connection().expect("writer");
        writer.execute("UPDATE lcm_raw_messages SET metadata_json = json_set(metadata_json, '$.provider_note', ?1) WHERE message_id = 'external'", params![format!("api_key={}", secret())]).await.expect("legacy metadata");
        if unavailable {
            std::fs::remove_file(&path).expect("offload fixture");
        }
        let result = harness
            .registered
            .lcm_privacy_rescan_raw_messages()
            .await
            .expect("rescan metadata");
        let LcmPrivacyRescanOutcomeV1::Completed(receipt) = result else {
            panic!("first scan")
        };
        assert_eq!(receipt.scanned_rows, 1);
        assert_eq!(receipt.remediated_rows, 1);
        assert_eq!(receipt.unavailable_payload_rows, u64::from(unavailable));
        let snapshot = harness
            .registered
            .lcm_read_snapshot()
            .await
            .expect("snapshot");
        let mut rows = snapshot.query("SELECT metadata_json, payload_ref FROM lcm_raw_messages WHERE message_id = 'external'", ()).await.expect("query");
        let row = rows.next().await.expect("row").expect("present");
        let metadata: String = row.get(0).expect("metadata");
        assert!(!metadata.contains(&secret()));
        let parsed: Value = serde_json::from_str(&metadata).expect("JSON");
        assert_eq!(
            parsed["ingest_protection"]["sanitization_receipt"],
            json!(legacy_receipt(body))
        );
        assert_eq!(row.get::<String>(1).expect("reference"), reference);
        if unavailable {
            assert!(!path.exists());
        } else {
            assert_eq!(std::fs::read(&path).expect("payload retained"), before);
        }
    }
}

#[asupersync::test]
async fn rescan_keeps_provider_metadata_when_reexternalizing_a_dirty_body() {
    let harness = RegisteredGlobalDbHarness::open("privacy-external-provider-metadata").await;
    seed_session(&harness, "provider-session").await;
    let mut body = (0..20_000)
        .map(|line| format!("captured tool output line {line}\n"))
        .collect::<String>();
    body.push_str(&format!("leaked api_key={}\n", secret()));
    let old_reference =
        seed_legacy_external_row(&harness, "provider-session", "offloaded", &body).await;
    let writer = harness.registered.writer_connection().expect("writer");
    writer
        .execute(
            "UPDATE lcm_raw_messages SET role = 'tool', metadata_json = json_set(metadata_json, '$.provider_note', 'reviewed') WHERE message_id = 'offloaded'",
            (),
        )
        .await
        .expect("provider metadata");

    let outcome = harness
        .registered
        .lcm_privacy_rescan_raw_messages()
        .await
        .expect("rescan dirty external body");
    let LcmPrivacyRescanOutcomeV1::Completed(receipt) = outcome else {
        panic!("first scan must run")
    };
    assert_eq!(receipt.remediated_rows, 1);

    let snapshot = harness
        .registered
        .lcm_read_snapshot()
        .await
        .expect("snapshot");
    let mut rows = snapshot
        .query(
            "SELECT metadata_json, payload_ref, storage_kind FROM lcm_raw_messages WHERE message_id = 'offloaded'",
            (),
        )
        .await
        .expect("query");
    let row = rows.next().await.expect("row").expect("present");
    let metadata: Value =
        serde_json::from_str(&row.get::<String>(0).expect("metadata")).expect("JSON");
    let reference: String = row.get(1).expect("reference");
    assert_eq!(row.get::<String>(2).expect("storage kind"), "external");
    assert_ne!(reference, old_reference);
    assert_eq!(metadata["provider_note"], json!("reviewed"));
    assert_eq!(metadata["external_payload"], json!(true));
    assert_eq!(metadata["payload_ref"], json!(reference));
    assert_eq!(metadata["ingest_protection"]["redacted"], json!(true));
}
