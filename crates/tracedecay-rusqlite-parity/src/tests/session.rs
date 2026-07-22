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
