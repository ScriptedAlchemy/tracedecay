use tracedecay_sqlite_parity_protocol::{
    CanonicalRowHasher, Command, Output, ROW_DIGEST_ALGORITHM, SessionStoreCount,
    SessionStoreFamily, SessionStoreRow, SessionStoreTable,
};

use super::support::{execute, fixture};

#[test]
fn session_counts_schema_and_keyset_pages_cover_every_closed_table() {
    let fixture = fixture();
    assert_eq!(
        execute(
            &fixture.path,
            Command::SessionStoreCount {
                family: SessionStoreFamily::Observation,
                table: SessionStoreTable::Observations,
            },
        ),
        Output::SessionStoreCount(SessionStoreCount {
            family: SessionStoreFamily::Observation,
            table: SessionStoreTable::Observations,
            row_count: Some(2),
        })
    );
    let Output::SessionStoreSchema(message_schema) = execute(
        &fixture.path,
        Command::SessionStoreSchema {
            family: SessionStoreFamily::Transcript,
            table: SessionStoreTable::SessionMessages,
        },
    ) else {
        panic!("session-store schema output expected");
    };
    assert!(message_schema.exists);
    assert_eq!(message_schema.columns[0].name, "provider");
    assert_eq!(message_schema.foreign_keys.len(), 2);
    assert!(
        message_schema
            .foreign_keys
            .iter()
            .all(|key| key.referenced_table == "sessions" && key.on_delete == "CASCADE")
    );

    let Output::SessionStorePage(first_page) = execute(
        &fixture.path,
        Command::SessionStorePage {
            family: SessionStoreFamily::Observation,
            table: SessionStoreTable::Observations,
            cursor: None,
            limit: 1,
        },
    ) else {
        panic!("session-store page output expected");
    };
    assert_eq!(first_page.order_columns, ["sequence"]);
    assert_eq!(first_page.digest_algorithm, ROW_DIGEST_ALGORITHM);
    assert!(matches!(
        &first_page.rows[0],
        SessionStoreRow::Observations {
            sequence: 1,
            observation_id,
            payload_digest,
            row_digest,
        } if observation_id == "observation-1"
            && payload_digest == "digest-1"
            && row_digest.starts_with("sha256:")
    ));
    let mut expected_digest = CanonicalRowHasher::new();
    expected_digest.update_integer(1);
    expected_digest.update_text(b"observation-1");
    expected_digest.update_text(b"digest-1");
    expected_digest.update_text(b"receipt");
    expected_digest.update_text(b"{}");
    expected_digest.update_text(b"{}");
    assert!(matches!(
        &first_page.rows[0],
        SessionStoreRow::Observations { row_digest, .. }
            if row_digest == &expected_digest.finish()
    ));
    let Output::SessionStorePage(second_page) = execute(
        &fixture.path,
        Command::SessionStorePage {
            family: SessionStoreFamily::Observation,
            table: SessionStoreTable::Observations,
            cursor: first_page.next_cursor,
            limit: 1,
        },
    ) else {
        panic!("second session-store page output expected");
    };
    assert!(matches!(
        &second_page.rows[0],
        SessionStoreRow::Observations {
            sequence: 2,
            observation_id,
            ..
        } if observation_id == "observation-2"
    ));
    assert!(second_page.next_cursor.is_none());

    for (table, expected) in [
        (SessionStoreTable::SourceCursors, "source_cursors"),
        (SessionStoreTable::Sessions, "sessions"),
        (SessionStoreTable::SessionMessages, "session_messages"),
        (
            SessionStoreTable::SessionSchemaMigrations,
            "session_schema_migrations",
        ),
        (SessionStoreTable::LcmRawMessages, "lcm_raw_messages"),
        (
            SessionStoreTable::SessionTemporalSchemaMigrations,
            "session_temporal_schema_migrations",
        ),
        (
            SessionStoreTable::SessionTemporalGenerations,
            "session_temporal_generations",
        ),
        (
            SessionStoreTable::SessionTemporalObservationEffects,
            "session_temporal_observation_effects",
        ),
        (
            SessionStoreTable::SessionTemporalProjectionReceipts,
            "session_temporal_projection_receipts",
        ),
        (SessionStoreTable::SessionOccurrences, "session_occurrences"),
        (SessionStoreTable::SessionAssertions, "session_assertions"),
        (
            SessionStoreTable::SessionSummaryNodes,
            "session_summary_nodes",
        ),
        (SessionStoreTable::MemoryV2Facts, "memory_v2_facts"),
        (
            SessionStoreTable::MemoryV2CurrentFacts,
            "memory_v2_current_facts",
        ),
        (
            SessionStoreTable::MemoryV2Assertions,
            "memory_v2_assertions",
        ),
        (
            SessionStoreTable::MemoryV2LineageEvents,
            "memory_v2_lineage_events",
        ),
        (SessionStoreTable::RetrievalAnchors, "retrieval_anchors"),
        (
            SessionStoreTable::GenerationDiagnostics,
            "generation_diagnostics",
        ),
        (
            SessionStoreTable::DiagnosticGenerationPublications,
            "diagnostic_generation_publications",
        ),
        (
            SessionStoreTable::ConfigurationRevisions,
            "configuration_revisions",
        ),
        (
            SessionStoreTable::ConfigurationEntries,
            "configuration_entries",
        ),
        (
            SessionStoreTable::ConfigurationMutationReceipts,
            "configuration_mutation_receipts",
        ),
        (
            SessionStoreTable::ConfigurationAuditEvents,
            "configuration_audit_events",
        ),
    ] {
        let Output::SessionStorePage(page) = execute(
            &fixture.path,
            Command::SessionStorePage {
                family: table.family(),
                table,
                cursor: None,
                limit: 10,
            },
        ) else {
            panic!("session-store page expected for {table:?}");
        };
        assert!(!page.rows.is_empty(), "fixture row missing for {table:?}");
        assert_eq!(
            serde_json::to_value(&page.rows[0]).unwrap()["table"],
            expected
        );
    }
}

#[test]
fn projection_receipt_pages_walk_the_composite_generation_keyset() {
    let fixture = fixture();
    let Output::SessionStorePage(first_page) = execute(
        &fixture.path,
        Command::SessionStorePage {
            family: SessionStoreFamily::Temporal,
            table: SessionStoreTable::SessionTemporalProjectionReceipts,
            cursor: None,
            limit: 1,
        },
    ) else {
        panic!("projection-receipt page output expected");
    };
    assert_eq!(
        first_page.order_columns,
        ["session_id", "generation", "batch_ordinal"]
    );
    assert!(matches!(
        &first_page.rows[0],
        SessionStoreRow::SessionTemporalProjectionReceipts {
            session_id,
            generation: 1,
            batch_ordinal: 0,
            batch_digest,
            row_digest,
        } if session_id == "session-1"
            && batch_digest == "batch-0"
            && row_digest.starts_with("sha256:")
    ));
    assert!(first_page.next_cursor.is_some());

    let Output::SessionStorePage(second_page) = execute(
        &fixture.path,
        Command::SessionStorePage {
            family: SessionStoreFamily::Temporal,
            table: SessionStoreTable::SessionTemporalProjectionReceipts,
            cursor: first_page.next_cursor,
            limit: 1,
        },
    ) else {
        panic!("second projection-receipt page output expected");
    };
    assert!(matches!(
        &second_page.rows[0],
        SessionStoreRow::SessionTemporalProjectionReceipts {
            batch_ordinal: 1,
            batch_digest,
            ..
        } if batch_digest == "batch-1"
    ));
    assert!(second_page.next_cursor.is_none());

    let Output::SessionStoreCount(count) = execute(
        &fixture.path,
        Command::SessionStoreCount {
            family: SessionStoreFamily::Summary,
            table: SessionStoreTable::SessionSummaryNodes,
        },
    ) else {
        panic!("summary-node count output expected");
    };
    assert_eq!(count.row_count, Some(2));
}

#[test]
fn occurrence_pages_walk_the_generation_keyset() {
    let fixture = fixture();
    let Output::SessionStorePage(first_page) = execute(
        &fixture.path,
        Command::SessionStorePage {
            family: SessionStoreFamily::Temporal,
            table: SessionStoreTable::SessionOccurrences,
            cursor: None,
            limit: 1,
        },
    ) else {
        panic!("occurrence page output expected");
    };
    assert_eq!(
        first_page.order_columns,
        ["session_id", "generation", "occurrence_id"]
    );
    assert!(matches!(
        &first_page.rows[0],
        SessionStoreRow::SessionOccurrences {
            session_id,
            generation: 1,
            occurrence_id,
            role,
            row_digest,
        } if session_id == "session-1"
            && occurrence_id == "occurrence-1"
            && role == "user"
            && row_digest.starts_with("sha256:")
    ));
    assert!(first_page.next_cursor.is_some());

    let Output::SessionStorePage(second_page) = execute(
        &fixture.path,
        Command::SessionStorePage {
            family: SessionStoreFamily::Temporal,
            table: SessionStoreTable::SessionOccurrences,
            cursor: first_page.next_cursor,
            limit: 1,
        },
    ) else {
        panic!("second occurrence page output expected");
    };
    assert!(matches!(
        &second_page.rows[0],
        SessionStoreRow::SessionOccurrences {
            occurrence_id,
            role,
            ..
        } if occurrence_id == "occurrence-2" && role == "assistant"
    ));
    assert!(second_page.next_cursor.is_none());
}

#[test]
fn assertion_pages_walk_the_generation_keyset_with_digest_oracle() {
    let fixture = fixture();
    let first = single_row_page(&fixture.path, SessionStoreTable::SessionAssertions, None);
    assert_eq!(
        first.order_columns,
        ["session_id", "generation", "assertion_id"]
    );
    let mut oracle = CanonicalRowHasher::new();
    oracle.update_text(b"session-1");
    oracle.update_integer(1);
    oracle.update_text(b"assertion-1");
    oracle.update_text(b"supersedes");
    oracle.update_text(b"anchor-1");
    oracle.update_text(b"anchor-2");
    oracle.update_integer(1);
    oracle.update_text(br#"{"kind":"unknown"}"#);
    oracle.update_text(b"{}");
    assert!(matches!(
        &first.rows[0],
        SessionStoreRow::SessionAssertions {
            session_id,
            generation: 1,
            assertion_id,
            assertion_kind,
            row_digest,
        } if session_id == "session-1"
            && assertion_id == "assertion-1"
            && assertion_kind == "supersedes"
            && row_digest == &oracle.finish()
    ));
    let cursor = first.next_cursor.clone().expect("assertion cursor");
    let second = single_row_page(
        &fixture.path,
        SessionStoreTable::SessionAssertions,
        Some(cursor),
    );
    assert!(matches!(
        &second.rows[0],
        SessionStoreRow::SessionAssertions {
            assertion_id,
            assertion_kind,
            ..
        } if assertion_id == "assertion-2" && assertion_kind == "annotates"
    ));
    assert!(second.next_cursor.is_none());
}

#[test]
fn summary_node_pages_walk_the_identifier_keyset() {
    let fixture = fixture();
    let first = single_row_page(&fixture.path, SessionStoreTable::SessionSummaryNodes, None);
    assert_eq!(first.order_columns, ["summary_id"]);
    let (SessionStoreRow::SessionSummaryNodes { summary_id, .. }, Some(cursor)) =
        (&first.rows[0], first.next_cursor.clone())
    else {
        panic!("first summary-node page expected with cursor");
    };
    assert_eq!(summary_id, "summary-1");
    let second = single_row_page(
        &fixture.path,
        SessionStoreTable::SessionSummaryNodes,
        Some(cursor),
    );
    assert!(matches!(
        &second.rows[0],
        SessionStoreRow::SessionSummaryNodes { summary_id, .. } if summary_id == "summary-2"
    ));
    assert!(second.next_cursor.is_none());
}

#[test]
fn source_cursor_pages_walk_the_composite_keyset_with_digest_oracle() {
    let fixture = fixture();
    let first = single_row_page(&fixture.path, SessionStoreTable::SourceCursors, None);
    assert_eq!(first.order_columns, ["source_json", "scope_json"]);
    let mut oracle = CanonicalRowHasher::new();
    oracle.update_text(br#"{"source":"a"}"#);
    oracle.update_text(br#"{"scope":"1"}"#);
    oracle.update_text(br#"{"cursor":"1"}"#);
    assert!(matches!(
        &first.rows[0],
        SessionStoreRow::SourceCursors {
            source_json,
            scope_json,
            row_digest,
        } if source_json == r#"{"source":"a"}"#
            && scope_json == r#"{"scope":"1"}"#
            && row_digest == &oracle.finish()
    ));
    let cursor = first.next_cursor.clone().expect("source-cursor cursor");
    let second = single_row_page(
        &fixture.path,
        SessionStoreTable::SourceCursors,
        Some(cursor),
    );
    assert!(matches!(
        &second.rows[0],
        SessionStoreRow::SourceCursors { scope_json, .. }
            if scope_json == r#"{"scope":"2"}"#
    ));
    assert!(second.next_cursor.is_none());
}

#[test]
fn retrieval_anchor_pages_walk_the_identifier_keyset_with_digest_oracle() {
    let fixture = fixture();
    let first = single_row_page(&fixture.path, SessionStoreTable::RetrievalAnchors, None);
    assert_eq!(first.order_columns, ["anchor_id"]);
    let mut oracle = CanonicalRowHasher::new();
    oracle.update_text(b"anchor-1");
    oracle.update_text(b"{}");
    oracle.update_text(b"{}");
    oracle.update_text(b"generation-1");
    assert!(matches!(
        &first.rows[0],
        SessionStoreRow::RetrievalAnchors {
            anchor_id,
            projection_generation,
            row_digest,
        } if anchor_id == "anchor-1"
            && projection_generation == "generation-1"
            && row_digest == &oracle.finish()
    ));
    let cursor = first.next_cursor.clone().expect("retrieval-anchor cursor");
    let second = single_row_page(
        &fixture.path,
        SessionStoreTable::RetrievalAnchors,
        Some(cursor),
    );
    assert!(matches!(
        &second.rows[0],
        SessionStoreRow::RetrievalAnchors { anchor_id, .. } if anchor_id == "anchor-2"
    ));
    assert!(second.next_cursor.is_none());
}

#[test]
fn memory_v2_fact_pages_walk_the_owner_keyset() {
    let fixture = fixture();
    let first = single_row_page(&fixture.path, SessionStoreTable::MemoryV2Facts, None);
    assert_eq!(first.order_columns, ["fact_id", "owner_kind", "project_id"]);
    assert!(matches!(
        &first.rows[0],
        SessionStoreRow::MemoryV2Facts {
            fact_id,
            owner_kind,
            project_id,
            identity_json,
            row_digest,
        } if fact_id == "fact-1"
            && owner_kind == "project"
            && project_id == "proj"
            && identity_json == "{}"
            && row_digest.starts_with("sha256:")
    ));
    let cursor = first.next_cursor.clone().expect("memory-v2-fact cursor");
    let second = single_row_page(
        &fixture.path,
        SessionStoreTable::MemoryV2Facts,
        Some(cursor),
    );
    assert!(matches!(
        &second.rows[0],
        SessionStoreRow::MemoryV2Facts { fact_id, .. } if fact_id == "fact-2"
    ));
    assert!(second.next_cursor.is_none());
}

#[test]
fn memory_v2_current_fact_pages_walk_the_owner_keyset() {
    let fixture = fixture();
    let first = single_row_page(&fixture.path, SessionStoreTable::MemoryV2CurrentFacts, None);
    assert_eq!(first.order_columns, ["fact_id", "owner_kind", "project_id"]);
    assert!(matches!(
        &first.rows[0],
        SessionStoreRow::MemoryV2CurrentFacts {
            fact_id,
            payload_access,
            projection_state,
            ..
        } if fact_id == "fact-1"
            && payload_access == "eligible"
            && projection_state == "ready"
    ));
    let cursor = first
        .next_cursor
        .clone()
        .expect("memory-v2-current-fact cursor");
    let second = single_row_page(
        &fixture.path,
        SessionStoreTable::MemoryV2CurrentFacts,
        Some(cursor),
    );
    assert!(matches!(
        &second.rows[0],
        SessionStoreRow::MemoryV2CurrentFacts {
            fact_id,
            payload_access,
            ..
        } if fact_id == "fact-2" && payload_access == "redacted"
    ));
    assert!(second.next_cursor.is_none());
}

#[test]
fn memory_v2_assertion_pages_walk_the_composite_keyset() {
    let fixture = fixture();
    let first = single_row_page(&fixture.path, SessionStoreTable::MemoryV2Assertions, None);
    assert_eq!(
        first.order_columns,
        ["assertion_id", "fact_id", "owner_kind", "project_id"]
    );
    assert!(matches!(
        &first.rows[0],
        SessionStoreRow::MemoryV2Assertions {
            assertion_id,
            fact_id,
            row_digest,
            ..
        } if assertion_id == "assertion-1"
            && fact_id == "fact-1"
            && row_digest.starts_with("sha256:")
    ));
    let cursor = first
        .next_cursor
        .clone()
        .expect("memory-v2-assertion cursor");
    let second = single_row_page(
        &fixture.path,
        SessionStoreTable::MemoryV2Assertions,
        Some(cursor),
    );
    assert!(matches!(
        &second.rows[0],
        SessionStoreRow::MemoryV2Assertions { assertion_id, .. }
            if assertion_id == "assertion-2"
    ));
    assert!(second.next_cursor.is_none());
}

#[test]
fn memory_v2_lineage_event_pages_walk_the_sequence_keyset() {
    let fixture = fixture();
    let first = single_row_page(
        &fixture.path,
        SessionStoreTable::MemoryV2LineageEvents,
        None,
    );
    assert_eq!(first.order_columns, ["event_sequence"]);
    assert!(matches!(
        &first.rows[0],
        SessionStoreRow::MemoryV2LineageEvents {
            event_sequence: 1,
            event_id,
            fact_id,
            ..
        } if event_id == "event-1" && fact_id == "fact-1"
    ));
    let cursor = first.next_cursor.clone().expect("memory-v2-lineage cursor");
    let second = single_row_page(
        &fixture.path,
        SessionStoreTable::MemoryV2LineageEvents,
        Some(cursor),
    );
    assert!(matches!(
        &second.rows[0],
        SessionStoreRow::MemoryV2LineageEvents {
            event_sequence: 2,
            event_id,
            ..
        } if event_id == "event-2"
    ));
    assert!(second.next_cursor.is_none());
}

#[test]
fn generation_diagnostic_pages_walk_the_anchor_keyset() {
    let fixture = fixture();
    let first = single_row_page(
        &fixture.path,
        SessionStoreTable::GenerationDiagnostics,
        None,
    );
    assert_eq!(first.order_columns, ["diagnostic_anchor"]);
    assert!(matches!(
        &first.rows[0],
        SessionStoreRow::GenerationDiagnostics {
            diagnostic_anchor,
            generation_id,
            severity,
            record_state,
            row_digest,
        } if diagnostic_anchor == "diagnostic-1"
            && generation_id == "generation-1"
            && severity == "error"
            && record_state == "current"
            && row_digest.starts_with("sha256:")
    ));
    let cursor = first
        .next_cursor
        .clone()
        .expect("generation-diagnostic cursor");
    let second = single_row_page(
        &fixture.path,
        SessionStoreTable::GenerationDiagnostics,
        Some(cursor),
    );
    assert!(matches!(
        &second.rows[0],
        SessionStoreRow::GenerationDiagnostics {
            diagnostic_anchor,
            severity,
            ..
        } if diagnostic_anchor == "diagnostic-2" && severity == "warning"
    ));
    assert!(second.next_cursor.is_none());
}

#[test]
fn diagnostic_publication_pages_walk_the_generation_keyset_with_digest_oracle() {
    let fixture = fixture();
    let first = single_row_page(
        &fixture.path,
        SessionStoreTable::DiagnosticGenerationPublications,
        None,
    );
    assert_eq!(first.order_columns, ["generation_id"]);
    let mut oracle = CanonicalRowHasher::new();
    oracle.update_text(b"generation-1");
    oracle.update_text(b"superseded");
    oracle.update_text(b"generation-2");
    oracle.update_integer(1);
    assert!(matches!(
        &first.rows[0],
        SessionStoreRow::DiagnosticGenerationPublications {
            generation_id,
            record_state,
            row_digest,
        } if generation_id == "generation-1"
            && record_state == "superseded"
            && row_digest == &oracle.finish()
    ));
    let cursor = first
        .next_cursor
        .clone()
        .expect("diagnostic-publication cursor");
    let second = single_row_page(
        &fixture.path,
        SessionStoreTable::DiagnosticGenerationPublications,
        Some(cursor),
    );
    assert!(matches!(
        &second.rows[0],
        SessionStoreRow::DiagnosticGenerationPublications {
            generation_id,
            record_state,
            ..
        } if generation_id == "generation-2" && record_state == "current"
    ));
    assert!(second.next_cursor.is_none());
}

#[test]
fn configuration_revision_pages_walk_the_identifier_keyset_with_digest_oracle() {
    let fixture = fixture();
    let first = single_row_page(
        &fixture.path,
        SessionStoreTable::ConfigurationRevisions,
        None,
    );
    assert_eq!(first.order_columns, ["revision_id"]);
    let mut oracle = CanonicalRowHasher::new();
    oracle.update_text(b"revision-1");
    oracle.update_null();
    oracle.update_text(b"snapshot-1");
    oracle.update_text(b"behavior-1");
    oracle.update_text(b"provenance-1");
    oracle.update_text(b"actor");
    oracle.update_text(b"bootstrap");
    oracle.update_integer(1);
    assert!(matches!(
        &first.rows[0],
        SessionStoreRow::ConfigurationRevisions {
            revision_id,
            snapshot_id,
            operation_kind,
            row_digest,
        } if revision_id == "revision-1"
            && snapshot_id == "snapshot-1"
            && operation_kind == "bootstrap"
            && row_digest == &oracle.finish()
    ));
    let cursor = first
        .next_cursor
        .clone()
        .expect("configuration-revision cursor");
    let second = single_row_page(
        &fixture.path,
        SessionStoreTable::ConfigurationRevisions,
        Some(cursor),
    );
    assert!(matches!(
        &second.rows[0],
        SessionStoreRow::ConfigurationRevisions {
            revision_id,
            operation_kind,
            ..
        } if revision_id == "revision-2" && operation_kind == "mutate"
    ));
    assert!(second.next_cursor.is_none());
}

#[test]
fn configuration_entry_pages_walk_the_composite_layer_keyset() {
    let fixture = fixture();
    let first = single_row_page(&fixture.path, SessionStoreTable::ConfigurationEntries, None);
    assert_eq!(
        first.order_columns,
        ["revision_id", "key", "layer_kind", "layer_id"]
    );
    assert!(matches!(
        &first.rows[0],
        SessionStoreRow::ConfigurationEntries {
            revision_id,
            key,
            layer_kind,
            layer_id,
            row_digest,
        } if revision_id == "revision-1"
            && key == "key-1"
            && layer_kind == "layer"
            && layer_id == "layer-1"
            && row_digest.starts_with("sha256:")
    ));
    let cursor = first
        .next_cursor
        .clone()
        .expect("configuration-entry cursor");
    let second = single_row_page(
        &fixture.path,
        SessionStoreTable::ConfigurationEntries,
        Some(cursor),
    );
    assert!(matches!(
        &second.rows[0],
        SessionStoreRow::ConfigurationEntries { layer_id, .. } if layer_id == "layer-2"
    ));
    assert!(second.next_cursor.is_none());
}

#[test]
fn configuration_receipt_pages_walk_the_identifier_keyset() {
    let fixture = fixture();
    let first = single_row_page(
        &fixture.path,
        SessionStoreTable::ConfigurationMutationReceipts,
        None,
    );
    assert_eq!(first.order_columns, ["receipt_id"]);
    assert!(matches!(
        &first.rows[0],
        SessionStoreRow::ConfigurationMutationReceipts {
            receipt_id,
            result_revision_id,
            activation_status,
            ..
        } if receipt_id == "receipt-1"
            && result_revision_id == "revision-2"
            && activation_status == "activated"
    ));
    let cursor = first
        .next_cursor
        .clone()
        .expect("configuration-receipt cursor");
    let second = single_row_page(
        &fixture.path,
        SessionStoreTable::ConfigurationMutationReceipts,
        Some(cursor),
    );
    assert!(matches!(
        &second.rows[0],
        SessionStoreRow::ConfigurationMutationReceipts {
            receipt_id,
            activation_status,
            ..
        } if receipt_id == "receipt-2" && activation_status == "noop"
    ));
    assert!(second.next_cursor.is_none());
}

#[test]
fn configuration_audit_event_pages_walk_the_identifier_keyset() {
    let fixture = fixture();
    let first = single_row_page(
        &fixture.path,
        SessionStoreTable::ConfigurationAuditEvents,
        None,
    );
    assert_eq!(first.order_columns, ["event_id"]);
    assert!(matches!(
        &first.rows[0],
        SessionStoreRow::ConfigurationAuditEvents {
            event_id,
            operation_kind,
            base_revision_id,
            ..
        } if event_id == "event-1"
            && operation_kind == "mutate"
            && base_revision_id == "revision-1"
    ));
    let cursor = first
        .next_cursor
        .clone()
        .expect("configuration-audit cursor");
    let second = single_row_page(
        &fixture.path,
        SessionStoreTable::ConfigurationAuditEvents,
        Some(cursor),
    );
    assert!(matches!(
        &second.rows[0],
        SessionStoreRow::ConfigurationAuditEvents {
            event_id,
            operation_kind,
            ..
        } if event_id == "event-2" && operation_kind == "denied"
    ));
    assert!(second.next_cursor.is_none());
}

/// Guards against a silent canonical-digest subset: a page query that drops or
/// reorders a physical column still changes row digests but would otherwise pass
/// every value assertion. Requiring the SELECT column count to equal
/// `PRAGMA table_info` for every closed table forecloses that regression at once.
#[test]
fn every_page_query_selects_the_full_physical_column_set() {
    let fixture = fixture();
    let connection = rusqlite::Connection::open(&fixture.path).expect("open fixture");
    for table in ALL_SESSION_STORE_TABLES {
        let spec = crate::closed_sql::session_table_spec(table);
        let (sql, _params) = crate::closed_sql::session_page_query(table, None, 1);
        let statement = connection.prepare(sql).expect("prepare page query");
        let selected = i64::try_from(statement.column_count()).expect("column count fits i64");
        let physical: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info(?1)",
                [spec.identifier],
                |row| row.get(0),
            )
            .expect("pragma table_info count");
        assert_eq!(
            selected, physical,
            "page query for {table:?} must select every physical column"
        );
    }
}

fn single_row_page(
    path: &std::path::Path,
    table: SessionStoreTable,
    cursor: Option<tracedecay_sqlite_parity_protocol::SessionStoreCursor>,
) -> tracedecay_sqlite_parity_protocol::SessionStorePage {
    let Output::SessionStorePage(page) = execute(
        path,
        Command::SessionStorePage {
            family: table.family(),
            table,
            cursor,
            limit: 1,
        },
    ) else {
        panic!("session-store page expected for {table:?}");
    };
    page
}

const ALL_SESSION_STORE_TABLES: [SessionStoreTable; 24] = [
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
    SessionStoreTable::SessionAssertions,
    SessionStoreTable::SessionSummaryNodes,
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
];
