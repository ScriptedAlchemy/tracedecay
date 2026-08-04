use std::{fmt::Debug, fs, path::PathBuf};

use serde::{Serialize, de::DeserializeOwned};

use super::*;

fn round_trip<T>(value: T)
where
    T: Debug + DeserializeOwned + PartialEq + Serialize,
{
    let bytes = serde_json::to_vec(&value).expect("serialize DTO");
    assert_eq!(
        serde_json::from_slice::<T>(&bytes).expect("deserialize DTO"),
        value
    );
}

fn provenance() -> CopiedSnapshotProvenance {
    CopiedSnapshotProvenance {
        authority_identity: "store:project:example".to_owned(),
        staging_root: PathBuf::from("/private/staging"),
        canonical_path: PathBuf::from("/private/staging/snapshot.db"),
        byte_len: 17,
        content_digest: format!("sha256:{}", "1".repeat(64)),
        file_identity: SnapshotFileIdentity::Unix {
            device: 1,
            inode: 2,
            links: 1,
        },
    }
}

#[test]
fn every_request_command_cursor_and_error_variant_round_trips() {
    round_trip(Request {
        protocol_version: PROTOCOL_VERSION,
        request_id: "request-1".to_owned(),
        database: CopiedDatabase {
            path: PathBuf::from("/private/staging/snapshot.db"),
            kind: DatabaseKind::CopiedSnapshot,
            provenance: provenance(),
        },
        command: Command::Metadata,
    });
    round_trip(DatabaseKind::CopiedSnapshot);
    for identity in [
        SnapshotFileIdentity::Unix {
            device: 1,
            inode: 2,
            links: 3,
        },
        SnapshotFileIdentity::Windows {
            volume_serial: 1,
            file_index: 2,
            links: 3,
        },
        SnapshotFileIdentity::Unsupported,
    ] {
        round_trip(identity);
    }
    for command in [
        Command::Metadata,
        Command::Schema,
        Command::ForeignKeys,
        Command::PageSize,
        Command::JournalMode,
        Command::Integrity {
            check: IntegrityCheck::Quick,
        },
        Command::Integrity {
            check: IntegrityCheck::Full,
        },
        Command::SessionStoreCount {
            family: SessionStoreFamily::Observation,
            table: SessionStoreTable::Observations,
        },
        Command::SessionStoreSchema {
            family: SessionStoreFamily::Transcript,
            table: SessionStoreTable::Sessions,
        },
        Command::SessionStorePage {
            family: SessionStoreFamily::Lcm,
            table: SessionStoreTable::LcmRawMessages,
            cursor: Some(SessionStoreCursor::LcmRawMessages { store_id: 1 }),
            limit: 1,
        },
    ] {
        round_trip(command);
    }
    for family in [
        SessionStoreFamily::Observation,
        SessionStoreFamily::Transcript,
        SessionStoreFamily::Lcm,
        SessionStoreFamily::Temporal,
        SessionStoreFamily::Summary,
        SessionStoreFamily::Fact,
        SessionStoreFamily::Diagnostics,
        SessionStoreFamily::Configuration,
    ] {
        round_trip(family);
    }
    for table in [
        SessionStoreTable::Observations,
        SessionStoreTable::SourceCursors,
        SessionStoreTable::Sessions,
        SessionStoreTable::SessionMessages,
        SessionStoreTable::SessionSchemaMigrations,
        SessionStoreTable::LcmRawMessages,
        SessionStoreTable::SessionTemporalSchemaMigrations,
        SessionStoreTable::SessionTemporalGenerations,
        SessionStoreTable::SessionTemporalObservationEffects,
        SessionStoreTable::SessionTemporalProjectionReceipts,
        SessionStoreTable::SessionOccurrences,
        SessionStoreTable::SessionLogicalCopyEdges,
        SessionStoreTable::SessionAssertions,
        SessionStoreTable::SessionSummaryNodes,
        SessionStoreTable::SessionSummarySources,
        SessionStoreTable::SessionSummarySuccessors,
        SessionStoreTable::MemoryV2Facts,
        SessionStoreTable::MemoryV2CurrentFacts,
        SessionStoreTable::MemoryV2Assertions,
        SessionStoreTable::MemoryV2LineageEvents,
        SessionStoreTable::RetrievalAnchors,
        SessionStoreTable::GenerationDiagnostics,
        SessionStoreTable::DiagnosticGenerationPublications,
        SessionStoreTable::ConfigurationRevisions,
        SessionStoreTable::ConfigurationEntries,
        SessionStoreTable::ConfigurationMutationReceipts,
        SessionStoreTable::ConfigurationAuditEvents,
    ] {
        round_trip(table);
    }
    for cursor in [
        SessionStoreCursor::Observations { sequence: 1 },
        SessionStoreCursor::SourceCursors {
            source_json: "source".to_owned(),
            scope_json: "scope".to_owned(),
        },
        SessionStoreCursor::Sessions {
            provider: "codex".to_owned(),
            session_id: "session".to_owned(),
        },
        SessionStoreCursor::SessionMessages {
            provider: "codex".to_owned(),
            session_id: "session".to_owned(),
            ordinal: 1,
            message_id: "message".to_owned(),
        },
        SessionStoreCursor::SessionSchemaMigrations {
            name: "migration".to_owned(),
        },
        SessionStoreCursor::LcmRawMessages { store_id: 1 },
        SessionStoreCursor::SessionTemporalSchemaMigrations {
            name: "migration".to_owned(),
        },
        SessionStoreCursor::SessionTemporalGenerations {
            session_id: "session".to_owned(),
            generation: 1,
        },
        SessionStoreCursor::SessionTemporalObservationEffects {
            observation_sequence: 1,
        },
        SessionStoreCursor::SessionTemporalProjectionReceipts {
            session_id: "session".to_owned(),
            generation: 1,
            batch_ordinal: 0,
        },
        SessionStoreCursor::SessionOccurrences {
            session_id: "session".to_owned(),
            generation: 1,
            occurrence_id: "occurrence".to_owned(),
        },
        SessionStoreCursor::SessionLogicalCopyEdges {
            session_id: "session".to_owned(),
            generation: 1,
            occurrence_id: "occurrence".to_owned(),
            copied_from_occurrence_id: "origin".to_owned(),
        },
        SessionStoreCursor::SessionAssertions {
            session_id: "session".to_owned(),
            generation: 1,
            assertion_id: "assertion".to_owned(),
        },
        SessionStoreCursor::SessionSummaryNodes {
            summary_id: "summary".to_owned(),
        },
        SessionStoreCursor::SessionSummarySources {
            summary_id: "summary".to_owned(),
            source_ordinal: 0,
        },
        SessionStoreCursor::SessionSummarySuccessors {
            predecessor_summary_id: "summary".to_owned(),
            successor_summary_id: "successor".to_owned(),
        },
        SessionStoreCursor::MemoryV2Facts {
            fact_id: "fact".to_owned(),
            owner_kind: "project".to_owned(),
            project_id: "project".to_owned(),
        },
        SessionStoreCursor::MemoryV2CurrentFacts {
            fact_id: "fact".to_owned(),
            owner_kind: "project".to_owned(),
            project_id: "project".to_owned(),
        },
        SessionStoreCursor::MemoryV2Assertions {
            assertion_id: "assertion".to_owned(),
            fact_id: "fact".to_owned(),
            owner_kind: "project".to_owned(),
            project_id: "project".to_owned(),
        },
        SessionStoreCursor::MemoryV2LineageEvents { event_sequence: 1 },
        SessionStoreCursor::RetrievalAnchors {
            anchor_id: "anchor".to_owned(),
        },
        SessionStoreCursor::GenerationDiagnostics {
            diagnostic_anchor: "diagnostic".to_owned(),
        },
        SessionStoreCursor::DiagnosticGenerationPublications {
            generation_id: "generation".to_owned(),
        },
        SessionStoreCursor::ConfigurationRevisions {
            revision_id: "revision".to_owned(),
        },
        SessionStoreCursor::ConfigurationEntries {
            revision_id: "revision".to_owned(),
            key: "key".to_owned(),
            layer_kind: "layer".to_owned(),
            layer_id: "layer-1".to_owned(),
        },
        SessionStoreCursor::ConfigurationMutationReceipts {
            receipt_id: "receipt".to_owned(),
        },
        SessionStoreCursor::ConfigurationAuditEvents {
            event_id: "event".to_owned(),
        },
    ] {
        round_trip(cursor);
    }
    for code in [
        ErrorCode::RequestTooLarge,
        ErrorCode::InvalidRequest,
        ErrorCode::UnsupportedProtocolVersion,
        ErrorCode::InvalidPath,
        ErrorCode::InvalidSnapshotProvenance,
        ErrorCode::RefusedLiveProfile,
        ErrorCode::OpenFailed,
        ErrorCode::ReadOnlyInvariant,
        ErrorCode::InvalidStoreFamily,
        ErrorCode::InvalidPageCursor,
        ErrorCode::InvalidPageLimit,
        ErrorCode::ResultLimitExceeded,
        ErrorCode::InvalidSqliteValue,
        ErrorCode::InvalidSqliteHeader,
        ErrorCode::SqliteFailure,
    ] {
        round_trip(code);
    }
}

#[test]
fn every_session_journal_and_response_result_variant_round_trips() {
    let column = SessionStoreColumn {
        ordinal: 0,
        name: "id".to_owned(),
        declared_type: "TEXT".to_owned(),
        not_null: true,
        default_value: None,
        primary_key_ordinal: 1,
    };
    let foreign_key = SessionStoreForeignKey {
        id: 0,
        sequence: 0,
        referenced_table: "parent".to_owned(),
        from_column: "parent_id".to_owned(),
        to_column: Some("id".to_owned()),
        on_update: "NO ACTION".to_owned(),
        on_delete: "CASCADE".to_owned(),
        match_kind: "NONE".to_owned(),
    };
    let journal = JournalModeMetadata {
        source_header: SourceHeaderJournalMode {
            read_version: 2,
            write_version: 2,
            mode: SourceJournalMode::Wal,
        },
        mode: EffectiveJournalMode::Delete,
        immutable_effective_mode: EffectiveJournalMode::Delete,
        normalization: JournalModeNormalization::WalSourceImmutableDelete,
    };
    for mode in [SourceJournalMode::Rollback, SourceJournalMode::Wal] {
        round_trip(mode);
    }
    round_trip(EffectiveJournalMode::Delete);
    for normalization in [
        JournalModeNormalization::RollbackSourceImmutableDelete,
        JournalModeNormalization::WalSourceImmutableDelete,
    ] {
        round_trip(normalization);
    }
    for kind in [
        SchemaObjectKind::Table,
        SchemaObjectKind::Index,
        SchemaObjectKind::Trigger,
        SchemaObjectKind::View,
    ] {
        round_trip(kind);
    }
    for output in [
        Output::Metadata(Metadata {
            canonical_path: PathBuf::from("/private/staging/snapshot.db"),
            query_only: true,
            immutable: true,
            sqlite_version: "3.0.0".to_owned(),
            compile_options: vec!["ENABLE_FTS5".to_owned()],
        }),
        Output::Schema(SchemaMetadata {
            schema_version: 1,
            user_version: 2,
            objects: vec![SchemaObject {
                kind: SchemaObjectKind::Table,
                name: "nodes".to_owned(),
                table_name: "nodes".to_owned(),
                sql: Some("CREATE TABLE nodes".to_owned()),
            }],
        }),
        Output::ForeignKeys { enabled: true },
        Output::PageSize { bytes: 4096 },
        Output::JournalMode(journal),
        Output::Integrity(IntegrityReport {
            check: IntegrityCheck::Full,
            findings: vec!["ok".to_owned()],
        }),
        Output::SessionStoreCount(SessionStoreCount {
            family: SessionStoreFamily::Observation,
            table: SessionStoreTable::Observations,
            row_count: Some(1),
        }),
        Output::SessionStoreSchema(SessionStoreSchema {
            family: SessionStoreFamily::Transcript,
            table: SessionStoreTable::Sessions,
            exists: true,
            columns: vec![column],
            foreign_keys: vec![foreign_key],
        }),
        Output::SessionStorePage(SessionStorePage {
            family: SessionStoreFamily::Observation,
            table: SessionStoreTable::Observations,
            order_columns: vec!["sequence".to_owned()],
            digest_algorithm: ROW_DIGEST_ALGORITHM.to_owned(),
            rows: vec![SessionStoreRow::Observations {
                sequence: 1,
                observation_id: "observation".to_owned(),
                payload_digest: "payload".to_owned(),
                row_digest: "row".to_owned(),
            }],
            next_cursor: None,
        }),
    ] {
        round_trip(output);
    }
    for row in session_rows() {
        round_trip(row);
    }
    round_trip(Response {
        protocol_version: PROTOCOL_VERSION,
        request_id: Some("request-1".to_owned()),
        verified_snapshot: Some(VerifiedCopiedSnapshot {
            authority_identity: "store:project:example".to_owned(),
            canonical_path: PathBuf::from("/private/staging/snapshot.db"),
            byte_len: 17,
            content_digest: format!("sha256:{}", "1".repeat(64)),
            file_identity: SnapshotFileIdentity::Unsupported,
        }),
        outcome: ResponseOutcome::Error {
            error: ErrorPayload::new(ErrorCode::InvalidRequest, "invalid request"),
        },
    });
    round_trip(Response {
        protocol_version: PROTOCOL_VERSION,
        request_id: Some("request-2".to_owned()),
        verified_snapshot: None,
        outcome: ResponseOutcome::Ok {
            output: Output::PageSize { bytes: 4096 },
        },
    });
}

fn session_rows() -> [SessionStoreRow; 27] {
    [
        SessionStoreRow::Observations {
            sequence: 1,
            observation_id: "observation".to_owned(),
            payload_digest: "payload".to_owned(),
            row_digest: "row".to_owned(),
        },
        SessionStoreRow::SourceCursors {
            source_json: "source".to_owned(),
            scope_json: "scope".to_owned(),
            row_digest: "row".to_owned(),
        },
        SessionStoreRow::Sessions {
            provider: "codex".to_owned(),
            session_id: "session".to_owned(),
            row_digest: "row".to_owned(),
        },
        SessionStoreRow::SessionMessages {
            provider: "codex".to_owned(),
            session_id: "session".to_owned(),
            ordinal: 1,
            message_id: "message".to_owned(),
            row_digest: "row".to_owned(),
        },
        SessionStoreRow::SessionSchemaMigrations {
            name: "migration".to_owned(),
            version: 1,
            row_digest: "row".to_owned(),
        },
        SessionStoreRow::LcmRawMessages {
            store_id: 1,
            provider: "codex".to_owned(),
            session_id: "session".to_owned(),
            ordinal: 1,
            message_id: "message".to_owned(),
            content_hash: "content".to_owned(),
            row_digest: "row".to_owned(),
        },
        SessionStoreRow::SessionTemporalSchemaMigrations {
            name: "migration".to_owned(),
            version: 1,
            row_digest: "row".to_owned(),
        },
        SessionStoreRow::SessionTemporalGenerations {
            session_id: "session".to_owned(),
            generation: 1,
            state: "ready".to_owned(),
            row_digest: "row".to_owned(),
        },
        SessionStoreRow::SessionTemporalObservationEffects {
            observation_id: "observation".to_owned(),
            observation_sequence: 1,
            session_id: "session".to_owned(),
            effect_digest: "effect".to_owned(),
            row_digest: "row".to_owned(),
        },
        SessionStoreRow::SessionTemporalProjectionReceipts {
            session_id: "session".to_owned(),
            generation: 1,
            batch_ordinal: 0,
            batch_digest: "batch".to_owned(),
            row_digest: "row".to_owned(),
        },
        SessionStoreRow::SessionOccurrences {
            session_id: "session".to_owned(),
            generation: 1,
            occurrence_id: "occurrence".to_owned(),
            role: "user".to_owned(),
            row_digest: "row".to_owned(),
        },
        SessionStoreRow::SessionLogicalCopyEdges {
            session_id: "session".to_owned(),
            generation: 1,
            occurrence_id: "occurrence".to_owned(),
            copied_from_occurrence_id: "origin".to_owned(),
            row_digest: "row".to_owned(),
        },
        SessionStoreRow::SessionAssertions {
            session_id: "session".to_owned(),
            generation: 1,
            assertion_id: "assertion".to_owned(),
            assertion_kind: "supersedes".to_owned(),
            row_digest: "row".to_owned(),
        },
        SessionStoreRow::SessionSummaryNodes {
            summary_id: "summary".to_owned(),
            session_id: "session".to_owned(),
            summary_anchor_id: "anchor".to_owned(),
            row_digest: "row".to_owned(),
        },
        SessionStoreRow::SessionSummarySources {
            summary_id: "summary".to_owned(),
            source_ordinal: 0,
            source_kind: "anchor".to_owned(),
            row_digest: "row".to_owned(),
        },
        SessionStoreRow::SessionSummarySuccessors {
            predecessor_summary_id: "summary".to_owned(),
            successor_summary_id: "successor".to_owned(),
            row_digest: "row".to_owned(),
        },
        SessionStoreRow::MemoryV2Facts {
            fact_id: "fact".to_owned(),
            owner_kind: "project".to_owned(),
            project_id: "project".to_owned(),
            identity_json: "{}".to_owned(),
            row_digest: "row".to_owned(),
        },
        SessionStoreRow::MemoryV2CurrentFacts {
            fact_id: "fact".to_owned(),
            owner_kind: "project".to_owned(),
            project_id: "project".to_owned(),
            payload_access: "eligible".to_owned(),
            projection_state: "ready".to_owned(),
            row_digest: "row".to_owned(),
        },
        SessionStoreRow::MemoryV2Assertions {
            assertion_id: "assertion".to_owned(),
            fact_id: "fact".to_owned(),
            owner_kind: "project".to_owned(),
            project_id: "project".to_owned(),
            row_digest: "row".to_owned(),
        },
        SessionStoreRow::MemoryV2LineageEvents {
            event_sequence: 1,
            event_id: "event".to_owned(),
            fact_id: "fact".to_owned(),
            row_digest: "row".to_owned(),
        },
        SessionStoreRow::RetrievalAnchors {
            anchor_id: "anchor".to_owned(),
            projection_generation: "generation".to_owned(),
            row_digest: "row".to_owned(),
        },
        SessionStoreRow::GenerationDiagnostics {
            diagnostic_anchor: "diagnostic".to_owned(),
            generation_id: "generation".to_owned(),
            severity: "error".to_owned(),
            record_state: "current".to_owned(),
            row_digest: "row".to_owned(),
        },
        SessionStoreRow::DiagnosticGenerationPublications {
            generation_id: "generation".to_owned(),
            record_state: "current".to_owned(),
            row_digest: "row".to_owned(),
        },
        SessionStoreRow::ConfigurationRevisions {
            revision_id: "revision".to_owned(),
            snapshot_id: "snapshot".to_owned(),
            operation_kind: "bootstrap".to_owned(),
            row_digest: "row".to_owned(),
        },
        SessionStoreRow::ConfigurationEntries {
            revision_id: "revision".to_owned(),
            key: "key".to_owned(),
            layer_kind: "layer".to_owned(),
            layer_id: "layer-1".to_owned(),
            row_digest: "row".to_owned(),
        },
        SessionStoreRow::ConfigurationMutationReceipts {
            receipt_id: "receipt".to_owned(),
            result_revision_id: "revision".to_owned(),
            activation_status: "activated".to_owned(),
            row_digest: "row".to_owned(),
        },
        SessionStoreRow::ConfigurationAuditEvents {
            event_id: "event".to_owned(),
            operation_kind: "mutate".to_owned(),
            base_revision_id: "revision".to_owned(),
            row_digest: "row".to_owned(),
        },
    ]
}

#[test]
fn dto_envelopes_and_tagged_variants_reject_unknown_fields() {
    let request = Request {
        protocol_version: PROTOCOL_VERSION,
        request_id: "request-1".to_owned(),
        database: CopiedDatabase {
            path: PathBuf::from("/private/staging/snapshot.db"),
            kind: DatabaseKind::CopiedSnapshot,
            provenance: provenance(),
        },
        command: Command::Metadata,
    };
    let mut unknown_request = serde_json::to_value(&request).expect("serialize request");
    unknown_request["unexpected"] = serde_json::json!(true);
    assert!(serde_json::from_value::<Request>(unknown_request).is_err());
    let mut unknown_provenance = serde_json::to_value(&request).expect("serialize request");
    unknown_provenance["database"]["provenance"]["unexpected"] = serde_json::json!(true);
    assert!(serde_json::from_value::<Request>(unknown_provenance).is_err());
    assert_eq!(
        decode_request_value(serde_json::json!({
            "protocol_version": PROTOCOL_VERSION,
            "request_id": "request-1",
            "database": request.database,
            "command": { "type": "metadata", "unexpected": true },
        }))
        .expect_err("unknown command fields must be rejected")
        .code,
        ErrorCode::InvalidRequest
    );
    for value in [
        serde_json::json!({ "type": "page_size", "bytes": 4096, "unexpected": true }),
        serde_json::json!({
            "table": "observations", "sequence": 1, "observation_id": "observation",
            "payload_digest": "payload", "row_digest": "row", "unexpected": true
        }),
    ] {
        assert!(serde_json::from_value::<Output>(value.clone()).is_err());
        assert!(serde_json::from_value::<SessionStoreRow>(value).is_err());
    }
    assert!(
        serde_json::from_value::<ErrorPayload>(serde_json::json!({
            "code": "invalid_request", "message": "bad request", "unexpected": true
        }))
        .is_err()
    );
    for response in [
        serde_json::json!({
            "protocol_version": PROTOCOL_VERSION, "request_id": "request-1",
            "status": "ok", "output": { "type": "page_size", "bytes": 4096 },
            "unexpected": true
        }),
        serde_json::json!({
            "protocol_version": PROTOCOL_VERSION, "request_id": "request-1",
            "status": "ok", "error": { "code": "invalid_request", "message": "wrong" }
        }),
    ] {
        assert!(serde_json::from_value::<Response>(response).is_err());
    }
}

#[test]
fn semantic_validation_rejects_invalid_closed_commands_before_io() {
    let valid = Request {
        protocol_version: PROTOCOL_VERSION,
        request_id: "request-1".to_owned(),
        database: CopiedDatabase {
            path: PathBuf::from("/private/staging/snapshot.db"),
            kind: DatabaseKind::CopiedSnapshot,
            provenance: provenance(),
        },
        command: Command::SessionStorePage {
            family: SessionStoreFamily::Transcript,
            table: SessionStoreTable::Sessions,
            cursor: Some(SessionStoreCursor::Sessions {
                provider: "codex".to_owned(),
                session_id: "session-1".to_owned(),
            }),
            limit: 1,
        },
    };
    assert!(validate_request(&valid).is_ok());
    for (command, code) in [
        (
            Command::SessionStoreCount {
                family: SessionStoreFamily::Lcm,
                table: SessionStoreTable::Observations,
            },
            ErrorCode::InvalidStoreFamily,
        ),
        (
            Command::SessionStorePage {
                family: SessionStoreFamily::Observation,
                table: SessionStoreTable::Observations,
                cursor: Some(SessionStoreCursor::LcmRawMessages { store_id: 1 }),
                limit: 1,
            },
            ErrorCode::InvalidPageCursor,
        ),
        (
            Command::SessionStorePage {
                family: SessionStoreFamily::Observation,
                table: SessionStoreTable::Observations,
                cursor: None,
                limit: MAX_SESSION_STORE_PAGE_SIZE + 1,
            },
            ErrorCode::InvalidPageLimit,
        ),
    ] {
        assert_eq!(validate_command(&command).unwrap_err().code, code);
    }
    let mut invalid = provenance();
    invalid.authority_identity.clear();
    assert_eq!(
        validate_copied_snapshot_provenance(&invalid)
            .expect_err("empty authority identity must be rejected")
            .code,
        ErrorCode::InvalidSnapshotProvenance
    );
    invalid = provenance();
    invalid.content_digest = "sha256:ABC".to_owned();
    assert_eq!(
        validate_copied_snapshot_provenance(&invalid)
            .expect_err("noncanonical content digest must be rejected")
            .code,
        ErrorCode::InvalidSnapshotProvenance
    );
}

#[test]
fn canonical_row_digest_frames_every_sqlite_value_type() {
    let digest = || {
        let mut hasher = CanonicalRowHasher::new();
        hasher.update_null();
        hasher.update_integer(-7);
        hasher.update_real(1.5);
        hasher.update_text("東京".as_bytes());
        hasher.update_blob(&[0, 1, 2]);
        hasher.finish()
    };
    let first = digest();
    assert!(is_canonical_sha256_digest(&first));
    assert_eq!(digest(), first);
    let mut left = CanonicalRowHasher::new();
    left.update_text(b"ab");
    left.update_text(b"c");
    let mut right = CanonicalRowHasher::new();
    right.update_text(b"a");
    right.update_text(b"bc");
    assert_ne!(left.finish(), right.finish());
}

#[test]
fn metadata_identity_uses_the_supported_platform_shape() {
    let directory = tempfile::tempdir().expect("create temporary directory");
    let path = directory.path().join("snapshot.db");
    fs::write(&path, b"sealed").expect("write temporary snapshot");
    let identity = SnapshotFileIdentity::from_metadata(
        &fs::metadata(&path).expect("read temporary snapshot metadata"),
    );
    #[cfg(unix)]
    assert!(matches!(identity, SnapshotFileIdentity::Unix { .. }));
    #[cfg(not(unix))]
    assert_eq!(identity, SnapshotFileIdentity::Unsupported);
}
