use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, params};

use super::super::global_db_operation_error;

/// Marker proving `observations` was created with the canonical AUTOINCREMENT
/// DDL below; the authority schema contract's AUTOINCREMENT invariant consumes
/// it. It is recorded at creation, never by rewriting an existing table.
const OBSERVATION_SCHEMA_MIGRATION: &str = "observations-v2-canonical-autoincrement";

/// Marker proving `observations` was created by a binary that derives every
/// provider's id on `tracedecay.observation.v1`. Recorded at creation. An
/// existing store that already holds rows and lacks the marker keeps those
/// rows unread: admission reports [`OBSERVATIONS_PREDATE_UNIFIED_IDENTITY`]
/// instead of decoding them.
const OBSERVATION_UNIFIED_IDENTITY_MIGRATION: &str = "observations-unified-identity-v1";

/// The observation authority of a store written before the unified
/// observation identity. The store's other authorities stay admissible; its
/// session features are refused until the store is reset.
pub(crate) const OBSERVATIONS_PREDATE_UNIFIED_IDENTITY: crate::registered::RefusedAuthorityV1 =
    crate::registered::RefusedAuthorityV1::Shape {
        authority: "observations",
        reason: "observation rows predate the unified observation identity and cannot be read; reset the profile so ingestion can rebuild them from host transcripts",
    };

/// Marker proving every `observation_repository_provenance` row references
/// its repository capture through `observation_repository_captures` instead of
/// embedding it. One checkout state is shared by every observation taken under
/// it, so the embedded copy, held twice per row, in `capture_json` and inside
/// `availability_json.value`, repeated a handful of distinct captures hundreds
/// of thousands of times. Rows written before the marker are split in place on
/// the next schema admission.
const OBSERVATION_REPOSITORY_CAPTURE_DEDUPE_MIGRATION: &str =
    "observation-repository-captures-v1-shared";

/// Moves embedded repository captures into `observation_repository_captures`
/// and slims the rows that carried them. The immutability trigger is lifted
/// only for this rewrite; the released marker rows carry no capture id and are
/// left alone.
const REPOSITORY_CAPTURE_DEDUPE_SQL: &str = "
    DROP TRIGGER IF EXISTS observation_repository_provenance_immutable_update;
    INSERT OR IGNORE INTO observation_repository_captures (capture_id, capture_json)
    SELECT json_extract(capture_json, '$.capture_id'), json_extract(capture_json, '$.capture')
    FROM observation_repository_provenance
    WHERE json_type(capture_json, '$.capture') = 'object'
      AND json_extract(capture_json, '$.capture_id') IS NOT NULL;
    UPDATE observation_repository_provenance
    SET capture_json = json_remove(capture_json, '$.capture'),
        availability_json = json_remove(availability_json, '$.value.capture')
    WHERE json_type(capture_json, '$.capture') = 'object'
      AND json_extract(capture_json, '$.capture_id') IS NOT NULL;
    CREATE TRIGGER IF NOT EXISTS observation_repository_provenance_immutable_update
    BEFORE UPDATE ON observation_repository_provenance BEGIN
        SELECT RAISE(ABORT, 'observation repository provenance is immutable');
    END;";

const OBSERVATION_SCHEMA_OPERATION: &str = "ensure observation authority schema";

async fn observation_table_exists(
    conn: &impl QueryExecutor,
) -> tracedecay_domain::errors::Result<bool> {
    let mut rows = conn
        .query(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'observations'",
            (),
        )
        .await
        .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))?;
    rows.next()
        .await
        .map(|row| row.is_some())
        .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))
}

async fn migration_recorded(
    conn: &impl QueryExecutor,
    migration: &str,
) -> tracedecay_domain::errors::Result<bool> {
    let mut rows = conn
        .query(
            "SELECT 1 FROM global_schema_migrations WHERE migration = ?1",
            params![migration],
        )
        .await
        .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))?;
    rows.next()
        .await
        .map(|row| row.is_some())
        .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))
}

async fn observation_rows_exist(
    conn: &impl QueryExecutor,
) -> tracedecay_domain::errors::Result<bool> {
    let mut rows = conn
        .query("SELECT 1 FROM observations LIMIT 1", ())
        .await
        .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))?;
    rows.next()
        .await
        .map(|row| row.is_some())
        .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))
}

/// Canonical observation-authority DDL.
const OBSERVATION_AUTHORITY_SCHEMA_SQL: &str =
    "CREATE TABLE IF NOT EXISTS global_schema_migrations (
            migration TEXT PRIMARY KEY
        );
        CREATE TABLE IF NOT EXISTS sanitization_receipts (
            receipt_id TEXT PRIMARY KEY,
            sanitizer_version TEXT NOT NULL,
            payload_digest TEXT NOT NULL,
            receipt_json TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS observations (
            sequence INTEGER PRIMARY KEY AUTOINCREMENT,
            observation_id TEXT NOT NULL UNIQUE,
            payload_digest TEXT NOT NULL,
            receipt_id TEXT NOT NULL,
            observation_json TEXT NOT NULL,
            committed_cursor_json TEXT NOT NULL,
            FOREIGN KEY(receipt_id) REFERENCES sanitization_receipts(receipt_id)
        );
        CREATE TABLE IF NOT EXISTS remote_writer_fences (
            authority_key TEXT PRIMARY KEY,
            writer_fence_json TEXT NOT NULL CHECK(json_valid(writer_fence_json)),
            frontier_sequence INTEGER NOT NULL CHECK(frontier_sequence >= 0),
            updated_at INTEGER NOT NULL
        ) STRICT;
        CREATE TABLE IF NOT EXISTS remote_observation_events (
            event_id TEXT PRIMARY KEY,
            frame_digest TEXT NOT NULL,
            enrollment_id TEXT NOT NULL,
            enrollment_revision INTEGER NOT NULL CHECK(enrollment_revision > 0),
            node_id TEXT NOT NULL,
            policy_revision INTEGER NOT NULL CHECK(policy_revision > 0),
            capture_sequence INTEGER NOT NULL CHECK(capture_sequence > 0),
            previous_event_id TEXT REFERENCES remote_observation_events(event_id),
            observation_id TEXT NOT NULL UNIQUE REFERENCES observations(observation_id),
            writer_fence_json TEXT NOT NULL CHECK(json_valid(writer_fence_json)),
            captured_at INTEGER NOT NULL,
            idempotency_key TEXT NOT NULL UNIQUE,
            command_digest TEXT NOT NULL,
            UNIQUE(enrollment_id, node_id, capture_sequence)
        ) STRICT;
        CREATE TABLE IF NOT EXISTS observation_retrieval_anchors (
            observation_id TEXT PRIMARY KEY,
            anchor_id TEXT NOT NULL UNIQUE,
            FOREIGN KEY(observation_id) REFERENCES observations(observation_id),
            FOREIGN KEY(anchor_id) REFERENCES retrieval_anchors(anchor_id)
        );
        CREATE TABLE IF NOT EXISTS observation_repository_provenance (
            observation_id TEXT PRIMARY KEY,
            availability_json TEXT NOT NULL CHECK(json_valid(availability_json)),
            capture_json TEXT CHECK(capture_json IS NULL OR json_valid(capture_json)),
            retrieval_anchor_id TEXT UNIQUE,
            owner_json TEXT CHECK(owner_json IS NULL OR json_valid(owner_json)),
            CHECK((capture_json IS NULL) = (retrieval_anchor_id IS NULL)),
            CHECK((owner_json IS NULL) = (retrieval_anchor_id IS NULL)),
            FOREIGN KEY(observation_id) REFERENCES observations(observation_id),
            FOREIGN KEY(retrieval_anchor_id, owner_json)
                REFERENCES retrieval_anchors(anchor_id, owner_json)
        );
        CREATE TABLE IF NOT EXISTS observation_repository_captures (
            capture_id TEXT PRIMARY KEY,
            capture_json TEXT NOT NULL CHECK(json_valid(capture_json))
        );
        CREATE TRIGGER IF NOT EXISTS observation_retrieval_anchors_immutable_update
        BEFORE UPDATE ON observation_retrieval_anchors BEGIN
            SELECT RAISE(ABORT, 'observation retrieval anchor bindings are immutable');
        END;
        CREATE TRIGGER IF NOT EXISTS observation_retrieval_anchors_immutable_delete
        BEFORE DELETE ON observation_retrieval_anchors BEGIN
            SELECT RAISE(ABORT, 'observation retrieval anchor bindings are immutable');
        END;
        CREATE TRIGGER IF NOT EXISTS observation_repository_provenance_immutable_update
        BEFORE UPDATE ON observation_repository_provenance BEGIN
            SELECT RAISE(ABORT, 'observation repository provenance is immutable');
        END;
        CREATE TRIGGER IF NOT EXISTS observation_repository_provenance_immutable_delete
        BEFORE DELETE ON observation_repository_provenance BEGIN
            SELECT RAISE(ABORT, 'observation repository provenance is immutable');
        END;
        CREATE TABLE IF NOT EXISTS source_cursors (
            source_json TEXT NOT NULL,
            scope_json TEXT NOT NULL,
            cursor_json TEXT NOT NULL,
            PRIMARY KEY(source_json, scope_json)
        );
        CREATE TABLE IF NOT EXISTS source_cursor_advances (
            source_json TEXT NOT NULL,
            scope_json TEXT NOT NULL,
            coverage_json TEXT NOT NULL,
            reason TEXT NOT NULL,
            receipt_id TEXT,
            PRIMARY KEY(source_json, scope_json, coverage_json),
            FOREIGN KEY(receipt_id) REFERENCES sanitization_receipts(receipt_id)
        );
        CREATE TABLE IF NOT EXISTS observation_admission_refusals (
            observation_id TEXT NOT NULL,
            refused_payload_digest TEXT NOT NULL,
            retained_payload_digest TEXT NOT NULL,
            refused_at INTEGER NOT NULL,
            PRIMARY KEY(observation_id, refused_payload_digest),
            FOREIGN KEY(observation_id) REFERENCES observations(observation_id)
        );
        CREATE TRIGGER IF NOT EXISTS observation_admission_refusals_immutable_update
        BEFORE UPDATE ON observation_admission_refusals BEGIN
            SELECT RAISE(ABORT, 'observation admission refusals are immutable');
        END;
        CREATE TRIGGER IF NOT EXISTS observation_admission_refusals_immutable_delete
        BEFORE DELETE ON observation_admission_refusals BEGIN
            SELECT RAISE(ABORT, 'observation admission refusals are immutable');
        END;
        CREATE TABLE IF NOT EXISTS projection_queue (
            observation_id TEXT PRIMARY KEY,
            observation_sequence INTEGER NOT NULL UNIQUE,
            attempt_count INTEGER NOT NULL DEFAULT 0 CHECK(attempt_count >= 0),
            next_retry_at_micros INTEGER NOT NULL DEFAULT 0 CHECK(next_retry_at_micros >= 0),
            last_error TEXT,
            FOREIGN KEY(observation_id) REFERENCES observations(observation_id)
        );
        CREATE INDEX IF NOT EXISTS idx_retrieval_anchor_dispositions_release_due
            ON retrieval_anchor_dispositions(effective_at, sequence)
            WHERE state IN ('superseded', 'deleted');";

/// Installs the observation authority. Returns the refused authority, with
/// the unified-identity marker and every observation-row rewrite skipped, when
/// the store holds rows written before the unified identity.
pub async fn ensure_observation_schema(
    conn: &(impl Executor + Sync),
) -> tracedecay_domain::errors::Result<Option<crate::registered::RefusedAuthorityV1>> {
    let table_preexisted = observation_table_exists(conn).await?;
    tracedecay_runtime_core::db::retrieval_anchor_schema::install_retrieval_anchor_schema(
        conn,
        OBSERVATION_SCHEMA_OPERATION,
    )
    .await?;
    conn.execute_batch(OBSERVATION_AUTHORITY_SCHEMA_SQL)
        .await
        .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))?;
    if !table_preexisted {
        for migration in [
            OBSERVATION_SCHEMA_MIGRATION,
            OBSERVATION_UNIFIED_IDENTITY_MIGRATION,
        ] {
            conn.execute(
                "INSERT OR IGNORE INTO global_schema_migrations(migration) VALUES (?1)",
                params![migration],
            )
            .await
            .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))?;
        }
    } else if !migration_recorded(conn, OBSERVATION_UNIFIED_IDENTITY_MIGRATION).await? {
        if observation_rows_exist(conn).await? {
            return Ok(Some(OBSERVATIONS_PREDATE_UNIFIED_IDENTITY));
        }
        conn.execute(
            "INSERT OR IGNORE INTO global_schema_migrations(migration) VALUES (?1)",
            params![OBSERVATION_UNIFIED_IDENTITY_MIGRATION],
        )
        .await
        .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))?;
    }
    if !migration_recorded(conn, OBSERVATION_REPOSITORY_CAPTURE_DEDUPE_MIGRATION).await? {
        conn.execute_batch(REPOSITORY_CAPTURE_DEDUPE_SQL)
            .await
            .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))?;
        conn.execute(
            "INSERT OR IGNORE INTO global_schema_migrations(migration) VALUES (?1)",
            params![OBSERVATION_REPOSITORY_CAPTURE_DEDUPE_MIGRATION],
        )
        .await
        .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))?;
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_domain::errors::TraceDecayError;
    use tracedecay_domain::{
        ComponentVersion, DurableObservationV1, ObservationIdentityMaterialV1,
        ObservationOrderingDomainV1, ObservationScopeV1, ObservationSourceCursorV1,
        ObservationSourceGenerationV1, ObservationSourceIdentityV1, ObservationSourceRangeV1,
        PayloadReferenceV1, RetentionClass, SanitizationReceiptId, SanitizationReceiptRefV1,
        SanitizationReceiptV1, SanitizerDispositionV1, SensitivityV1, SessionId,
    };
    use tracedecay_runtime_core::db::engine::TestConnection;

    const PRE_UNIFIED_OBSERVATION_ID: &str =
        "sha256:efd99c7fd87f4ad156b40f16d982d18511ebfb708afc140f9f67e63e0c73f5ba";
    const PRE_UNIFIED_RECEIPT_ID: &str =
        "privacy.claude.v1.0000000000000000000000000000000000000000000000000000000000000000";
    const PRE_UNIFIED_OBSERVATION_JSON: &str = concat!(
        r#"{"observation_id":""#,
        "sha256:efd99c7fd87f4ad156b40f16d982d18511ebfb708afc140f9f67e63e0c73f5ba",
        r#"","idempotency_key":""#,
        "sha256:efd99c7fd87f4ad156b40f16d982d18511ebfb708afc140f9f67e63e0c73f5ba",
        r#"","identity":{"source":{"provider":"claude","session_id":"session.fixture"}},"#,
        r#""receipt":{"receipt_id":""#,
        "privacy.claude.v1.0000000000000000000000000000000000000000000000000000000000000000",
        r#""},"retention_class":"transcript.fixture","payload":{"message":"safe"}}"#
    );
    const FRESH_UNIFIED_OBSERVATION_ID: &str =
        "sha256:3fe143a02ab7fbca28e297944f24e997badda9ca9b1e40c7d2a6e4f965cebad5";

    async fn reopen(
        path: &std::path::Path,
    ) -> tracedecay_domain::errors::Result<(
        crate::RegisteredGlobalDbLeaseV1,
        crate::RegisteredGlobalDbOwnerV1,
    )> {
        crate::tests::harness::open_registered_test_database_fixture(
            path,
            tracedecay_runtime_core::db::TestDatabaseRuntimeScope::ProfileSessions,
        )
        .await
    }

    fn fresh_unified_observation() -> DurableObservationV1 {
        let payload = serde_json::json!({"message": "safe"});
        let material = ObservationIdentityMaterialV1::new(
            ObservationSourceIdentityV1::new(SessionId::new("session.fixture").unwrap()).unwrap(),
            ObservationScopeV1::Profile,
            ObservationSourceGenerationV1::new(7).unwrap(),
            ObservationSourceRangeV1::new(12, 34).unwrap(),
        )
        .unwrap();
        DurableObservationV1::new(
            material,
            SanitizationReceiptV1::new(
                SanitizationReceiptRefV1::new(
                    SanitizationReceiptId::new("receipt.fixture").unwrap(),
                    ComponentVersion::new("sanitizer.fixture.v1").unwrap(),
                )
                .unwrap(),
                SanitizerDispositionV1::Accepted,
                SensitivityV1::NonSensitive,
                Some(PayloadReferenceV1::for_payload(&payload).unwrap()),
            )
            .unwrap(),
            RetentionClass::new("transcript.fixture").unwrap(),
            payload,
        )
        .unwrap()
    }

    fn fresh_unified_cursor(observation: &DurableObservationV1) -> ObservationSourceCursorV1 {
        ObservationSourceCursorV1::for_ordering(
            observation.source().clone(),
            observation.scope().clone(),
            observation.identity().generation(),
            ObservationOrderingDomainV1::FileBytes,
            observation.identity().position().end(),
        )
        .unwrap()
    }

    /// A store written before the unified identity, holding a Claude row whose
    /// id was derived on `tracedecay.claude.observation.v1` and whose JSON
    /// still carries `idempotency_key`, is refused. The row is left unchanged.
    #[tokio::test]
    async fn pre_unified_identity_rows_require_a_typed_reset() {
        let directory = tempfile::TempDir::new().unwrap();
        let path = directory.path().join("sessions.db");
        drop(reopen(&path).await.unwrap());
        let conn = TestConnection::open(&path);
        conn.execute(
            "DELETE FROM global_schema_migrations WHERE migration = ?1",
            params!["observations-unified-identity-v1"],
        )
        .await
        .unwrap();
        conn.execute(
            "INSERT INTO sanitization_receipts
             (receipt_id, sanitizer_version, payload_digest, receipt_json)
             VALUES (?1, 'privacy.claude-record.v1', ?2, '{}')",
            params![PRE_UNIFIED_RECEIPT_ID, PRE_UNIFIED_OBSERVATION_ID],
        )
        .await
        .unwrap();
        conn.execute(
            "INSERT INTO observations
             (observation_id, payload_digest, receipt_id, observation_json, committed_cursor_json)
             VALUES (?1, ?1, ?2, ?3, '{}')",
            params![
                PRE_UNIFIED_OBSERVATION_ID,
                PRE_UNIFIED_RECEIPT_ID,
                PRE_UNIFIED_OBSERVATION_JSON
            ],
        )
        .await
        .unwrap();
        drop(conn);

        let (lease, owner) = reopen(&path)
            .await
            .expect("the store's other authorities stay admissible");
        for refusal in [lease.reset_required(), owner.reset_required()] {
            let Some(TraceDecayError::ResetRequired { authority, reason }) = refusal else {
                panic!("old observation rows must be a typed reset, got {refusal:?}");
            };
            assert_eq!(authority, "observations");
            assert_eq!(
                reason,
                "observation rows predate the unified observation identity and cannot be read; reset the profile so ingestion can rebuild them from host transcripts"
            );
        }
        drop((lease, owner));
        let conn = TestConnection::open(&path);
        let mut rows = conn
            .query(
                "SELECT COUNT(*) FROM global_schema_migrations WHERE migration = ?1",
                params![OBSERVATION_UNIFIED_IDENTITY_MIGRATION],
            )
            .await
            .unwrap();
        let recorded: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
        assert_eq!(
            recorded, 0,
            "a refused store never adopts the unified identity"
        );
        drop(rows);
        drop(conn);

        let conn = TestConnection::open(&path);
        let mut rows = conn
            .query(
                "SELECT observation_json FROM observations WHERE observation_id = ?1",
                params![PRE_UNIFIED_OBSERVATION_ID],
            )
            .await
            .unwrap();
        let stored: String = rows.next().await.unwrap().unwrap().get(0).unwrap();
        assert_eq!(stored, PRE_UNIFIED_OBSERVATION_JSON);
    }

    /// An empty store created before the marker has nothing to refuse. Admission
    /// records the marker and a later unified-identity row stays readable.
    #[tokio::test]
    async fn empty_pre_marker_observation_store_adopts_the_unified_identity() {
        let directory = tempfile::TempDir::new().unwrap();
        let path = directory.path().join("sessions.db");
        drop(reopen(&path).await.unwrap());
        let conn = TestConnection::open(&path);
        conn.execute(
            "DELETE FROM global_schema_migrations WHERE migration = ?1",
            params![OBSERVATION_UNIFIED_IDENTITY_MIGRATION],
        )
        .await
        .unwrap();
        drop(conn);

        drop(reopen(&path).await.unwrap());

        let conn = TestConnection::open(&path);
        let mut rows = conn
            .query(
                "SELECT COUNT(*) FROM global_schema_migrations WHERE migration = ?1",
                params![OBSERVATION_UNIFIED_IDENTITY_MIGRATION],
            )
            .await
            .unwrap();
        let recorded: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
        assert_eq!(recorded, 1);
    }

    /// A store this binary created keeps a unified-identity row and reads it
    /// back as that same id.
    #[tokio::test]
    async fn fresh_observation_store_accepts_unified_identity_rows() {
        let directory = tempfile::TempDir::new().unwrap();
        let path = directory.path().join("sessions.db");
        drop(reopen(&path).await.unwrap());
        let observation = fresh_unified_observation();
        assert_eq!(
            observation.observation_id().as_str(),
            FRESH_UNIFIED_OBSERVATION_ID
        );
        let observation_json = serde_json::to_string(&observation).unwrap();
        assert!(
            !observation_json.contains("idempotency_key"),
            "{observation_json}"
        );
        let cursor_json = serde_json::to_string(&fresh_unified_cursor(&observation)).unwrap();
        let receipt = observation.receipt();
        let conn = TestConnection::open(&path);
        conn.execute(
            "INSERT INTO sanitization_receipts
             (receipt_id, sanitizer_version, payload_digest, receipt_json)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                receipt.receipt().receipt_id().as_str(),
                receipt.receipt().sanitizer_version().as_str(),
                observation.payload_reference().digest().as_str(),
                serde_json::to_string(receipt).unwrap()
            ],
        )
        .await
        .unwrap();
        conn.execute(
            "INSERT INTO observations
             (observation_id, payload_digest, receipt_id, observation_json, committed_cursor_json)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                observation.observation_id().as_str(),
                observation.payload_reference().digest().as_str(),
                receipt.receipt().receipt_id().as_str(),
                observation_json.as_str(),
                cursor_json.as_str()
            ],
        )
        .await
        .unwrap();
        drop(conn);

        drop(reopen(&path).await.unwrap());

        let conn = TestConnection::open(&path);
        let mut rows = conn
            .query(
                "SELECT observation_json FROM observations WHERE observation_id = ?1",
                params![FRESH_UNIFIED_OBSERVATION_ID],
            )
            .await
            .unwrap();
        let stored: String = rows.next().await.unwrap().unwrap().get(0).unwrap();
        let decoded: DurableObservationV1 = serde_json::from_str(&stored).unwrap();
        assert_eq!(
            decoded.observation_id().as_str(),
            FRESH_UNIFIED_OBSERVATION_ID
        );
        assert!(!stored.contains("idempotency_key"));
    }

    /// A provenance row written before the shared-capture migration embeds the
    /// same capture twice; re-admission must split it into a shared row plus a
    /// slim reference and still hydrate the original documents for readers.
    #[tokio::test]
    async fn embedded_repository_captures_are_split_into_the_shared_table_on_admission() {
        let directory = tempfile::TempDir::new().unwrap();
        let fixture = crate::tests::harness::open_registered_test_fixture(
            &directory.path().join("sessions.db"),
            tracedecay_runtime_core::db::TestDatabaseRuntimeScope::ProfileSessions,
        )
        .await
        .unwrap();
        let transaction = fixture.database().begin_write_transaction().await.unwrap();
        ensure_observation_schema(&transaction).await.unwrap();
        let capture = serde_json::json!({
            "capture_id": "repository.capture.v1.fixture",
            "repository_id": "repository.fixture",
            "evidence": {"attached_ref": {"availability": "known", "value": "refs/heads/main"}},
            "captured_at": 7,
        });
        let embedded = |observation: &str| {
            serde_json::json!({
                "generation_id": "projection.fixture.v1",
                "capture_id": "repository.capture.v1.fixture",
                "capture": capture,
                "source_observation": observation,
            })
        };
        let released_marker = super::super::retention::PROVENANCE_RELEASED_MARKER;
        transaction
            .execute_batch("DROP TRIGGER observation_repository_provenance_immutable_update")
            .await
            .unwrap();
        // The provenance row points at an observation and an anchor, so seed
        // both before it.
        let mut observation_ids = Vec::new();
        for (index, label) in ["a", "b", "released"].into_iter().enumerate() {
            let (observation, cursor) =
                crate::schema_contract::invariants::test_fixture::authority_fixture(
                    index as u64,
                    label,
                );
            let receipt = observation.receipt();
            transaction
                .execute(
                    "INSERT INTO sanitization_receipts
                     (receipt_id, sanitizer_version, payload_digest, receipt_json)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![
                        receipt.receipt().receipt_id().as_str(),
                        receipt.receipt().sanitizer_version().as_str(),
                        observation.payload_reference().digest().as_str(),
                        serde_json::to_string(receipt).unwrap()
                    ],
                )
                .await
                .unwrap();
            transaction
                .execute(
                    "INSERT INTO observations
                     (observation_id, payload_digest, receipt_id, observation_json, committed_cursor_json)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        observation.observation_id().as_str(),
                        observation.payload_reference().digest().as_str(),
                        receipt.receipt().receipt_id().as_str(),
                        serde_json::to_string(&observation).unwrap(),
                        serde_json::to_string(&cursor).unwrap()
                    ],
                )
                .await
                .unwrap();
            let anchor_id = format!("anchor.{label}");
            transaction
                .execute(
                    "INSERT INTO retrieval_anchors
                     (anchor_id, anchor_json, owner_json, projection_generation)
                     VALUES (?1, '{}', '{}', 'projection.fixture.v1')",
                    params![anchor_id.as_str()],
                )
                .await
                .unwrap();
            let observation_id = observation.observation_id().as_str().to_owned();
            let (availability_json, capture_json) = if label == "released" {
                (released_marker.to_owned(), released_marker.to_owned())
            } else {
                let provenance = embedded(&observation_id);
                (
                    serde_json::json!({"availability": "known", "value": provenance}).to_string(),
                    provenance.to_string(),
                )
            };
            transaction
                .execute(
                    "INSERT INTO observation_repository_provenance
                     (observation_id, availability_json, capture_json, retrieval_anchor_id, owner_json)
                     VALUES (?1, ?2, ?3, ?4, '{}')",
                    params![
                        observation_id.as_str(),
                        availability_json,
                        capture_json,
                        anchor_id.as_str()
                    ],
                )
                .await
                .unwrap();
            observation_ids.push((label, observation_id));
        }
        let released_observation = observation_ids
            .iter()
            .find(|(label, _)| *label == "released")
            .map(|(_, id)| id.clone())
            .unwrap();
        transaction
            .execute(
                "DELETE FROM global_schema_migrations WHERE migration = ?1",
                params![OBSERVATION_REPOSITORY_CAPTURE_DEDUPE_MIGRATION],
            )
            .await
            .unwrap();

        ensure_observation_schema(&transaction).await.unwrap();

        let mut rows = transaction
            .query(
                &format!(
                    "SELECT repository.observation_id, repository.capture_json,
                            {}
                     FROM observation_repository_provenance AS repository
                     {}
                     ORDER BY repository.observation_id",
                    tracedecay_rusqlite_runtime::repository::REPOSITORY_PROVENANCE_HYDRATED_COLUMNS,
                    tracedecay_rusqlite_runtime::repository::REPOSITORY_PROVENANCE_CAPTURE_JOIN,
                ),
                (),
            )
            .await
            .unwrap();
        let mut seen = Vec::new();
        while let Some(row) = rows.next().await.unwrap() {
            let observation: String = row.get(0).unwrap();
            let slim_capture: String = row.get(1).unwrap();
            let hydrated_availability: String = row.get(2).unwrap();
            let hydrated_capture: String = row.get(3).unwrap();
            seen.push((
                observation,
                slim_capture,
                hydrated_availability,
                hydrated_capture,
            ));
        }
        assert_eq!(seen.len(), 3);
        for (observation, slim_capture, hydrated_availability, hydrated_capture) in &seen {
            if *observation == released_observation {
                assert_eq!(slim_capture, released_marker);
                assert_eq!(hydrated_availability, released_marker);
                assert_eq!(hydrated_capture, released_marker);
                continue;
            }
            let slim: serde_json::Value = serde_json::from_str(slim_capture).unwrap();
            assert!(
                slim.get("capture").is_none(),
                "{observation} still embeds its capture"
            );
            let expected = embedded(observation);
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(hydrated_capture).unwrap(),
                expected
            );
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(hydrated_availability).unwrap(),
                serde_json::json!({"availability": "known", "value": expected})
            );
        }
        let mut rows = transaction
            .query("SELECT COUNT(*) FROM observation_repository_captures", ())
            .await
            .unwrap();
        let captures: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
        assert_eq!(
            captures, 1,
            "two rows under one capture share a single stored copy"
        );
        assert!(
            migration_recorded(
                &transaction,
                OBSERVATION_REPOSITORY_CAPTURE_DEDUPE_MIGRATION
            )
            .await
            .unwrap()
        );
        transaction.rollback().await.unwrap();
    }
}
