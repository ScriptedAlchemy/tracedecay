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
        (
            SessionStoreTable::SessionLogicalCopyEdges,
            "session_logical_copy_edges",
        ),
        (SessionStoreTable::SessionAssertions, "session_assertions"),
        (
            SessionStoreTable::SessionSummaryNodes,
            "session_summary_nodes",
        ),
        (
            SessionStoreTable::SessionSummarySources,
            "session_summary_sources",
        ),
        (
            SessionStoreTable::SessionSummarySuccessors,
            "session_summary_successors",
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
fn logical_copy_edge_pages_walk_the_composite_occurrence_keyset() {
    let fixture = fixture();
    let Output::SessionStorePage(first_page) = execute(
        &fixture.path,
        Command::SessionStorePage {
            family: SessionStoreFamily::Temporal,
            table: SessionStoreTable::SessionLogicalCopyEdges,
            cursor: None,
            limit: 1,
        },
    ) else {
        panic!("logical-copy-edge page output expected");
    };
    assert_eq!(
        first_page.order_columns,
        [
            "session_id",
            "generation",
            "occurrence_id",
            "copied_from_occurrence_id"
        ]
    );
    assert!(matches!(
        &first_page.rows[0],
        SessionStoreRow::SessionLogicalCopyEdges {
            session_id,
            generation: 1,
            occurrence_id,
            copied_from_occurrence_id,
            row_digest,
        } if session_id == "session-1"
            && occurrence_id == "occurrence-2"
            && copied_from_occurrence_id == "occurrence-1"
            && row_digest.starts_with("sha256:")
    ));
    assert!(first_page.next_cursor.is_some());

    let Output::SessionStorePage(second_page) = execute(
        &fixture.path,
        Command::SessionStorePage {
            family: SessionStoreFamily::Temporal,
            table: SessionStoreTable::SessionLogicalCopyEdges,
            cursor: first_page.next_cursor,
            limit: 1,
        },
    ) else {
        panic!("second logical-copy-edge page output expected");
    };
    assert!(matches!(
        &second_page.rows[0],
        SessionStoreRow::SessionLogicalCopyEdges {
            occurrence_id,
            ..
        } if occurrence_id == "occurrence-3"
    ));
    assert!(second_page.next_cursor.is_none());

    let Output::SessionStoreCount(count) = execute(
        &fixture.path,
        Command::SessionStoreCount {
            family: SessionStoreFamily::Temporal,
            table: SessionStoreTable::SessionAssertions,
        },
    ) else {
        panic!("assertion count output expected");
    };
    assert_eq!(count.row_count, Some(1));
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
fn summary_source_pages_walk_the_ordinal_keyset_with_digest_oracle() {
    let fixture = fixture();
    let first = single_row_page(
        &fixture.path,
        SessionStoreTable::SessionSummarySources,
        None,
    );
    assert_eq!(first.order_columns, ["summary_id", "source_ordinal"]);
    assert!(matches!(
        &first.rows[0],
        SessionStoreRow::SessionSummarySources {
            summary_id,
            source_ordinal: 0,
            source_kind,
            ..
        } if summary_id == "summary-1" && source_kind == "anchor"
    ));
    let mut oracle = CanonicalRowHasher::new();
    oracle.update_text(b"summary-1");
    oracle.update_integer(0);
    oracle.update_text(b"anchor");
    oracle.update_text(b"anchor-1");
    oracle.update_null();
    assert!(matches!(
        &first.rows[0],
        SessionStoreRow::SessionSummarySources { row_digest, .. }
            if row_digest == &oracle.finish()
    ));
    let cursor = first.next_cursor.clone().expect("summary-source cursor");
    let second = single_row_page(
        &fixture.path,
        SessionStoreTable::SessionSummarySources,
        Some(cursor),
    );
    assert!(matches!(
        &second.rows[0],
        SessionStoreRow::SessionSummarySources {
            source_ordinal: 1,
            source_kind,
            ..
        } if source_kind == "summary"
    ));
    assert!(second.next_cursor.is_none());
}

#[test]
fn summary_successor_pages_walk_the_composite_keyset_with_digest_oracle() {
    let fixture = fixture();
    let first = single_row_page(
        &fixture.path,
        SessionStoreTable::SessionSummarySuccessors,
        None,
    );
    assert_eq!(
        first.order_columns,
        ["predecessor_summary_id", "successor_summary_id"]
    );
    let mut oracle = CanonicalRowHasher::new();
    oracle.update_text(b"summary-1");
    oracle.update_text(b"summary-2");
    oracle.update_integer(1);
    assert!(matches!(
        &first.rows[0],
        SessionStoreRow::SessionSummarySuccessors {
            predecessor_summary_id,
            successor_summary_id,
            row_digest,
        } if predecessor_summary_id == "summary-1"
            && successor_summary_id == "summary-2"
            && row_digest == &oracle.finish()
    ));
    let cursor = first.next_cursor.clone().expect("summary-successor cursor");
    let second = single_row_page(
        &fixture.path,
        SessionStoreTable::SessionSummarySuccessors,
        Some(cursor),
    );
    assert!(matches!(
        &second.rows[0],
        SessionStoreRow::SessionSummarySuccessors {
            successor_summary_id,
            ..
        } if successor_summary_id == "summary-3"
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

const ALL_SESSION_STORE_TABLES: [SessionStoreTable; 15] = [
    SessionStoreTable::Observations,
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
];
