use rusqlite::OptionalExtension;
use tempfile::TempDir;

use crate::schema_contract::invariants::test_fixture::authority_fixture;
use crate::tests::harness::open_registered_test_database_fixture;
use tracedecay_domain::errors::TraceDecayError;
use tracedecay_runtime_core::db::TestDatabaseRuntimeScope;

use super::reset_refused_observation_authority;

async fn install_registered_store(path: &std::path::Path) {
    let admitted =
        open_registered_test_database_fixture(path, TestDatabaseRuntimeScope::ProfileSessions)
            .await
            .expect("install the registered sessions schema");
    drop(admitted);
}

/// Replaces the canonical `observations` table with the pre-release
/// `idempotency_key` shape that admission refuses.
fn install_legacy_observation_shape(conn: &rusqlite::Connection) {
    conn.pragma_update(None, "foreign_keys", false)
        .expect("disable foreign keys for fixture seeding");
    conn.execute_batch(
        "DROP TABLE observations;
         CREATE TABLE observations (
            sequence INTEGER PRIMARY KEY AUTOINCREMENT,
            observation_id TEXT NOT NULL UNIQUE,
            idempotency_key TEXT NOT NULL UNIQUE,
            payload_digest TEXT NOT NULL,
            receipt_id TEXT NOT NULL,
            observation_json TEXT NOT NULL,
            committed_cursor_json TEXT NOT NULL,
            FOREIGN KEY(receipt_id) REFERENCES sanitization_receipts(receipt_id)
         );
         INSERT INTO observations
            (observation_id, idempotency_key, payload_digest, receipt_id,
             observation_json, committed_cursor_json)
         VALUES ('observation.legacy', 'idempotency.legacy', 'digest.legacy',
                 'receipt.legacy', '{}', '{}');",
    )
    .expect("install the refused pre-release observation shape");
}

fn seed_preserved_transcript_rows(conn: &rusqlite::Connection) {
    conn.execute_batch(
        "INSERT INTO sessions(provider, session_id, project_key, project_path)
         VALUES ('claude', 'session.fixture', 'project.fixture', '/project/fixture');
         INSERT INTO session_messages(provider, message_id, session_id, role, ordinal, text)
         VALUES ('claude', 'message.fixture', 'session.fixture', 'user', 0, 'projected output');",
    )
    .expect("seed transcript metadata and one projector-output message");
}

/// Seeds the session-temporal state a store that has ever projected carries:
/// an active generation frozen at a high projection frontier, the batch
/// receipt certifying that generation's occurrence/current/FTS coverage, one
/// occurrence, an applied relation receipt, and a running refresh operation.
/// Every guard trigger is satisfied on the way in — the generation walks
/// `building -> ready -> active` and the receipt lands while it is building —
/// so the fixture cannot be weaker than production.
fn seed_active_temporal_generation(conn: &rusqlite::Connection) {
    conn.execute_batch(
        "INSERT INTO retrieval_anchors
            (anchor_id, anchor_json, owner_json, projection_generation)
         VALUES ('anchor.fixture', '{}', '{}', 'generation.fixture');
         INSERT INTO session_temporal_generations
            (session_id, generation, state, frozen_watermarks_json, created_at)
         VALUES ('session.fixture', 1, 'building',
                 '{\"projection_frontier\":100}', 1);
         INSERT INTO session_temporal_projection_receipts
            (session_id, generation, batch_ordinal, batch_digest,
             frozen_watermarks_json, source_through, projection_through,
             occurrence_count, occurrence_digest, dimension_count, dimension_digest,
             copy_count, copy_digest, assertion_count, assertion_digest,
             supersession_count, supersession_digest, current_count, current_digest,
             fts_count, fts_digest, committed_at)
         VALUES ('session.fixture', 1, 0, 'digest.batch',
                 '{\"projection_frontier\":100}', 100, 100,
                 1, 'digest.occurrence', 0, 'digest.dimension', 0, 'digest.copy',
                 0, 'digest.assertion', 0, 'digest.supersession',
                 1, 'digest.current', 1, 'digest.fts', 2);
         UPDATE session_temporal_generations
            SET state = 'ready', ready_at = 3 WHERE session_id = 'session.fixture';
         UPDATE session_temporal_generations
            SET state = 'active', activated_at = 4 WHERE session_id = 'session.fixture';
         INSERT INTO session_occurrences
            (session_id, generation, occurrence_id, source_observation_id,
             source_provider, projection_output_ordinal, retrieval_anchor_id,
             role, knowledge_at, valid_time_json, evidence_json,
             sanitized_content_digest, sanitized_content_bytes, snippet_text, index_text)
         VALUES ('session.fixture', 1, 'occurrence.fixture', 'observation.legacy',
                 'claude', 0, 'anchor.fixture', 'user', 5,
                 '{\"kind\":\"unknown\"}', '{}',
                 '0000000000000000000000000000000000000000000000000000000000000000',
                 0, 'snippet', 'index');
         INSERT INTO session_current_entities
            (session_id, generation, entity_kind, entity_id, current_occurrence_id,
             coverage_json)
         VALUES ('session.fixture', 1, 'occurrence_anchor', 'anchor.fixture',
                 'occurrence.fixture', '{}');
         INSERT INTO session_relation_receipts
            (session_id, generation, scope_kind, scope_id, expected_graph_watermark,
             state, graph_watermark, created_at, applied_at)
         VALUES ('session.fixture', 1, 'profile_sessions', 'scope.fixture',
                 'watermark.fixture', 'applied', 'watermark.fixture', 6, 7);
         INSERT INTO session_refresh_operations
            (session_id, operation_id, request_digest, target_frontier_json,
             state, created_at, updated_at)
         VALUES ('session.fixture', 'operation.fixture', 'digest.request',
                 '{\"observed_through\":100,\"committed_through\":100}',
                 'running', 8, 8);",
    )
    .expect("seed a populated active temporal generation");
}

/// Mirrors the frontier predicate of
/// `pending_session_temporal_refresh_page_result`: an output-producing effect
/// past the active generation's frozen `projection_frontier`, for a session
/// with no running refresh operation. A retained generation at frontier 100
/// is exactly what would exclude re-ingested effects 1..=100 from the rebuild.
fn refresh_discovery_frontier(conn: &rusqlite::Connection, session_id: &str) -> Option<i64> {
    conn.query_row(
        "SELECT COALESCE(active.projection_frontier, 0)
         FROM session_temporal_observation_effects AS effect
         LEFT JOIN (
             SELECT session_id,
                    CAST(json_extract(frozen_watermarks_json, '$.projection_frontier')
                         AS INTEGER) AS projection_frontier
             FROM session_temporal_generations
             WHERE state = 'active'
         ) AS active ON active.session_id = effect.session_id
         WHERE NOT EXISTS (
             SELECT 1 FROM session_refresh_operations AS running
             WHERE running.session_id = effect.session_id AND running.state = 'running'
         )
           AND effect.output_count > 0
           AND effect.observation_sequence > COALESCE(active.projection_frontier, 0)
           AND effect.session_id = ?1
         GROUP BY effect.session_id",
        [session_id],
        |row| row.get::<_, i64>(0),
    )
    .optional()
    .unwrap()
}

fn foreign_key_violations(conn: &rusqlite::Connection) -> Vec<String> {
    let mut statement = conn.prepare("PRAGMA foreign_key_check").unwrap();
    let violations = statement
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>();
    violations.unwrap()
}

fn trigger_exists(conn: &rusqlite::Connection, trigger: &str) -> bool {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'trigger' AND name = ?1)",
        [trigger],
        |row| row.get::<_, bool>(0),
    )
    .unwrap()
}

fn scheme_migration_recorded(conn: &rusqlite::Connection) -> bool {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM global_schema_migrations WHERE migration = ?1)",
        [super::OBSERVATION_NATIVE_SOURCE_SCHEME_MIGRATION],
        |row| row.get::<_, bool>(0),
    )
    .unwrap()
}

fn table_exists(conn: &rusqlite::Connection, table: &str) -> bool {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
        [table],
        |row| row.get::<_, bool>(0),
    )
    .unwrap()
}

fn count(conn: &rusqlite::Connection, table: &str) -> i64 {
    conn.query_row(&format!("SELECT COUNT(*) FROM \"{table}\""), [], |row| {
        row.get::<_, i64>(0)
    })
    .unwrap()
}

/// Seeds one canonical-shape observation and cursor whose source names
/// `provider`, then removes the native-source scheme marker so the store looks
/// exactly like one written before the Cline/Roo/Kilo `ui_messages` source
/// existed. `source_key` is the observation's source key (`None` omits it).
fn seed_unmarked_native_source_rows(
    conn: &rusqlite::Connection,
    provider: &str,
    source_key: Option<&str>,
) {
    let source = match source_key {
        Some(key) => format!(
            r#"{{"provider":"{provider}","session_id":"session.fixture","source_key":"{key}"}}"#
        ),
        None => format!(r#"{{"provider":"{provider}","session_id":"session.fixture"}}"#),
    };
    let observation =
        format!(r#"{{"identity":{{"source":{source},"scope":{{"kind":"profile"}}}}}}"#);
    conn.pragma_update(None, "foreign_keys", false)
        .expect("disable foreign keys for fixture seeding");
    conn.execute_batch(
        "INSERT INTO sanitization_receipts
            (receipt_id, sanitizer_version, payload_digest, receipt_json)
         VALUES ('receipt.fixture', 'v1', 'digest.fixture', '{}');",
    )
    .expect("seed a receipt");
    conn.execute(
        "INSERT INTO observations
            (observation_id, payload_digest, receipt_id, observation_json,
             committed_cursor_json)
         VALUES ('observation.fixture', 'digest.fixture', 'receipt.fixture', ?1, '{}')",
        [&observation],
    )
    .expect("seed an observation");
    conn.execute(
        "INSERT INTO source_cursors(source_json, scope_json, cursor_json)
         VALUES (?1, '{\"kind\":\"profile\"}', '{}')",
        [&source],
    )
    .expect("seed a cursor");
    conn.execute(
        "DELETE FROM global_schema_migrations WHERE migration = ?1",
        [super::OBSERVATION_NATIVE_SOURCE_SCHEME_MIGRATION],
    )
    .expect("make the fixture an old-scheme store");
}

async fn reopen_registered_store(path: &std::path::Path) -> tracedecay_domain::errors::Result<()> {
    open_registered_test_database_fixture(path, TestDatabaseRuntimeScope::ProfileSessions)
        .await
        .map(drop)
}

/// The native-source scheme change only ever applied to Cline, Roo Code and
/// Kilo tasks. A populated store whose observations and cursors name none of
/// those hosts cannot double-count anything under the new scheme, so attach
/// enrolls it instead of demanding a reset that would discard derived history
/// for no reason. Rows are untouched. The rows are real committed Codex
/// observations (the same fixture the authority audit uses), because attach
/// audits every retained row after the shape check admits the store.
#[tokio::test]
async fn populated_store_without_cline_like_sources_enrolls_on_attach() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("sessions.db");
    install_registered_store(&database_path).await;
    {
        let raw = rusqlite::Connection::open(&database_path).unwrap();
        for index in 0..=super::super::schema::SOURCE_CURSOR_CENSUS_PAGE_ROWS {
            let (observation, cursor) =
                authority_fixture(u64::try_from(index).unwrap(), &format!("enroll-{index}"));
            let receipt = observation.receipt();
            let payload_digest = observation.payload_reference().digest().as_str().to_owned();
            raw.execute(
                "INSERT INTO sanitization_receipts
                    (receipt_id, sanitizer_version, payload_digest, receipt_json)
                 VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![
                    receipt.receipt().receipt_id().as_str(),
                    receipt.receipt().sanitizer_version().as_str(),
                    payload_digest.as_str(),
                    serde_json::to_string(receipt).unwrap()
                ],
            )
            .expect("seed a committed receipt");
            raw.execute(
                "INSERT INTO observations
                    (observation_id, payload_digest, receipt_id, observation_json,
                     committed_cursor_json)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![
                    observation.observation_id().as_str(),
                    payload_digest.as_str(),
                    receipt.receipt().receipt_id().as_str(),
                    serde_json::to_string(&observation).unwrap(),
                    serde_json::to_string(&cursor).unwrap()
                ],
            )
            .expect("seed a committed Codex observation");
            raw.execute(
                "INSERT INTO source_cursors(source_json, scope_json, cursor_json)
                 VALUES (?1, ?2, ?3)",
                rusqlite::params![
                    serde_json::to_string(cursor.source()).unwrap(),
                    serde_json::to_string(cursor.scope()).unwrap(),
                    serde_json::to_string(&cursor).unwrap()
                ],
            )
            .expect("seed the committed cursor");
        }
        raw.execute(
            "DELETE FROM global_schema_migrations WHERE migration = ?1",
            [super::OBSERVATION_NATIVE_SOURCE_SCHEME_MIGRATION],
        )
        .expect("make the fixture an old-scheme store");
        assert!(!scheme_migration_recorded(&raw));
    }

    reopen_registered_store(&database_path)
        .await
        .expect("a Codex-only old-scheme store must attach");

    let raw = rusqlite::Connection::open(&database_path).unwrap();
    assert!(
        scheme_migration_recorded(&raw),
        "attach must enroll the scheme for a store the change never applied to"
    );
    let expected_rows = super::super::schema::SOURCE_CURSOR_CENSUS_PAGE_ROWS + 1;
    assert_eq!(count(&raw, "observations"), expected_rows);
    assert_eq!(count(&raw, "source_cursors"), expected_rows);
    assert!(
        super::reset_refused_observation_authority(
            &mut rusqlite::Connection::open(&database_path).unwrap()
        )
        .is_err(),
        "an enrolled store is healthy and the scoped reset must refuse it"
    );
}

/// The observations scan is independently authoritative: a cursor can be
/// absent after a committed observation, and a Cline-like row beyond the
/// first bounded page must still refuse enrollment.
#[tokio::test]
async fn paged_census_finds_cline_observation_without_source_cursor() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("sessions.db");
    install_registered_store(&database_path).await;
    {
        let raw = rusqlite::Connection::open(&database_path).unwrap();
        raw.pragma_update(None, "foreign_keys", false)
            .expect("disable foreign keys for fixture seeding");
        raw.execute_batch(
            "INSERT INTO sanitization_receipts
                (receipt_id, sanitizer_version, payload_digest, receipt_json)
             VALUES ('receipt.census', 'v1', 'digest.census', '{}');",
        )
        .expect("seed census receipt");
        let rows = super::super::schema::OBSERVATION_SOURCE_CENSUS_PAGE_ROWS + 1;
        for index in 0..rows {
            let provider = if index + 1 == rows { "cline" } else { "codex" };
            let observation = format!(
                r#"{{"identity":{{"source":{{"provider":"{provider}","session_id":"session.{index}"}},"scope":{{"kind":"profile"}}}}}}"#
            );
            raw.execute(
                "INSERT INTO observations
                    (observation_id, payload_digest, receipt_id, observation_json,
                     committed_cursor_json)
                 VALUES (?1, 'digest.census', 'receipt.census', ?2, '{}')",
                rusqlite::params![format!("observation.census-{index}"), observation],
            )
            .expect("seed census observation");
        }
        raw.execute(
            "DELETE FROM global_schema_migrations WHERE migration = ?1",
            [super::OBSERVATION_NATIVE_SOURCE_SCHEME_MIGRATION],
        )
        .expect("make the fixture an old-scheme store");
        assert_eq!(count(&raw, "source_cursors"), 0);
    }

    let error = reopen_registered_store(&database_path)
        .await
        .expect_err("a paged Cline observation census must refuse admission");
    let (authority, reason) = error
        .reset_required_context()
        .unwrap_or_else(|| panic!("expected typed ResetRequired, got: {error}"));
    assert_eq!(authority, super::OBSERVATION_AUTHORITY);
    assert!(reason.contains("ui_messages.json"));

    let raw = rusqlite::Connection::open(&database_path).unwrap();
    assert!(!scheme_migration_recorded(&raw));
    assert_eq!(count(&raw, "source_cursors"), 0);
}

/// A store that did admit a Cline-like task under the combined `<task>` source
/// carries no record of which scheme wrote those rows, so it must still refuse
/// with the typed `ResetRequired` state naming the observation authority —
/// whether the host shows up as an observation provider or only as a cursor.
#[tokio::test]
async fn populated_store_with_cline_like_sources_still_refuses_without_the_marker() {
    for (provider, source_key) in [
        ("cline", None),
        ("roo-code", None),
        ("kilo", Some("task.fixture:ui_messages")),
    ] {
        let directory = TempDir::new().unwrap();
        let database_path = directory.path().join("sessions.db");
        install_registered_store(&database_path).await;
        {
            let raw = rusqlite::Connection::open(&database_path).unwrap();
            seed_unmarked_native_source_rows(&raw, provider, source_key);
        }

        let error = reopen_registered_store(&database_path)
            .await
            .expect_err("an old-scheme Cline-like store must refuse admission");
        let (authority, reason) = error.reset_required_context().unwrap_or_else(|| {
            panic!("expected the typed ResetRequired state for {provider}, got: {error}")
        });
        assert_eq!(authority, super::OBSERVATION_AUTHORITY);
        assert!(
            reason.contains("ui_messages.json"),
            "the refusal must name the scheme change for {provider}: {reason}"
        );
        let raw = rusqlite::Connection::open(&database_path).unwrap();
        assert!(
            !scheme_migration_recorded(&raw),
            "a refused {provider} store must not be enrolled behind the operator's back"
        );
    }
}

#[tokio::test]
async fn refused_observation_shape_resets_scoped_and_readmits() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("sessions.db");
    install_registered_store(&database_path).await;
    {
        let raw = rusqlite::Connection::open(&database_path).unwrap();
        seed_preserved_transcript_rows(&raw);
        install_legacy_observation_shape(&raw);
    }

    let refusal = match open_registered_test_database_fixture(
        &database_path,
        TestDatabaseRuntimeScope::ProfileSessions,
    )
    .await
    {
        Ok(_) => panic!("the pre-release observation shape must refuse admission"),
        Err(error) => error,
    };
    let (authority, reason) = refusal
        .reset_required_context()
        .unwrap_or_else(|| panic!("expected the typed ResetRequired state, got: {refusal}"));
    assert_eq!(authority, "observations");
    assert!(
        reason.contains("no sanctioned migration") || reason.contains("branch-local"),
        "the refusal must say why no migration exists: {reason}"
    );

    let report = {
        let mut raw = rusqlite::Connection::open(&database_path).unwrap();
        reset_refused_observation_authority(&mut raw)
            .expect("scoped reset of the refused authority")
    };
    assert!(
        report
            .reset_tables
            .iter()
            .any(|table| table == "observations"),
        "the refused table must be part of the reset: {report:?}"
    );
    assert_eq!(report.cleared_session_message_rows, 1);

    let readmitted = open_registered_test_database_fixture(
        &database_path,
        TestDatabaseRuntimeScope::ProfileSessions,
    )
    .await
    .expect("the reset store must readmit at the canonical schema");
    drop(readmitted);

    let raw = rusqlite::Connection::open(&database_path).unwrap();
    assert_eq!(
        count(&raw, "observations"),
        0,
        "the refused authority must be recreated empty"
    );
    let has_idempotency_column = raw
        .query_row(
            "SELECT EXISTS(
                SELECT 1 FROM pragma_table_xinfo('observations')
                WHERE name = 'idempotency_key'
             )",
            [],
            |row| row.get::<_, bool>(0),
        )
        .unwrap();
    assert!(
        !has_idempotency_column,
        "the recreated table must carry the canonical shape"
    );
    assert_eq!(
        count(&raw, "sessions"),
        1,
        "transcript metadata outside the refused authority must be preserved"
    );
    assert_eq!(
        count(&raw, "session_messages"),
        0,
        "recoverable projector output must be cleared with its provenance"
    );
    assert_eq!(
        count(&raw, "remote_deletion_tombstones"),
        0,
        "unrelated authorities must survive the scoped reset with their schema intact"
    );
}

/// A retained admission-refusal terminal names an observation row by id and
/// digest. After a scoped reset recreates the observation authority empty,
/// a leftover terminal would falsely suppress the re-ingested record whose
/// rewritten payload happens to match the stale refusal signature — so the
/// scoped reset must clear the refusal authority with the rest.
#[tokio::test]
async fn scoped_reset_clears_retained_admission_refusals() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("sessions.db");
    install_registered_store(&database_path).await;
    {
        let raw = rusqlite::Connection::open(&database_path).unwrap();
        raw.pragma_update(None, "foreign_keys", false)
            .expect("disable foreign keys for fixture seeding");
        raw.execute_batch(
            "INSERT INTO observation_admission_refusals
                (observation_id, refused_payload_digest, retained_payload_digest, refused_at)
             VALUES ('observation.legacy', 'digest.refused', 'digest.retained', 1);",
        )
        .expect("seed one retained admission refusal");
        install_legacy_observation_shape(&raw);
    }

    let report = {
        let mut raw = rusqlite::Connection::open(&database_path).unwrap();
        reset_refused_observation_authority(&mut raw)
            .expect("scoped reset of the refused authority")
    };
    assert!(
        report
            .reset_tables
            .iter()
            .any(|table| table == "observation_admission_refusals"),
        "the refusal authority must be part of the scoped reset: {report:?}"
    );

    let raw = rusqlite::Connection::open(&database_path).unwrap();
    assert!(
        table_exists(&raw, "observation_admission_refusals"),
        "the refusal authority must be recreated at the canonical shape"
    );
    assert_eq!(
        count(&raw, "observation_admission_refusals"),
        0,
        "a scoped reset must leave no stale refusal terminal that could \
         falsely suppress re-ingested records"
    );
}

#[tokio::test]
async fn healthy_observation_authority_refuses_the_scoped_reset() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("sessions.db");
    install_registered_store(&database_path).await;

    let mut raw = rusqlite::Connection::open(&database_path).unwrap();
    raw.execute(
        "INSERT INTO session_query_cursor_keys
            (key_id, key_version, key_material, created_at, retired_at)
         VALUES ('cursor-key-healthy', 1, ?1, 1, NULL)",
        rusqlite::params![vec![9_u8; 32]],
    )
    .unwrap();
    let error = reset_refused_observation_authority(&mut raw)
        .expect_err("a healthy authority must never be reset");
    assert!(
        matches!(
            &error,
            TraceDecayError::Config { message } if message.contains("not in a refused state")
        ),
        "unexpected error resetting a healthy authority: {error}"
    );
    assert_eq!(count(&raw, "session_query_cursor_keys"), 1);
    let unchanged_key: (String, Vec<u8>, Option<i64>) = raw
        .query_row(
            "SELECT key_id, key_material, retired_at FROM session_query_cursor_keys",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        unchanged_key,
        ("cursor-key-healthy".to_owned(), vec![9_u8; 32], None)
    );
    assert!(
        table_exists(&raw, "observations"),
        "a refused reset must mutate nothing"
    );
}

/// The session-temporal projection is projector output over the observation
/// stream, so it resets with the stream it projects. Refusing over it instead
/// made the reset unreachable on any store that had ever ingested; preserving
/// the generation while deleting its occurrences would be worse — the frozen
/// frontier of the retained active generation would exclude every re-ingested
/// effect from rebuild discovery, and its immutable batch receipt would go on
/// certifying occurrence, current-entity and FTS counts for rows that no
/// longer exist. So the generation, its receipts, its relation receipt and
/// its refresh operation are invalidated outright, and replay is rediscovered
/// from zero.
#[tokio::test]
async fn populated_temporal_generation_is_invalidated_and_replay_rediscovered() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("sessions.db");
    install_registered_store(&database_path).await;
    {
        let raw = rusqlite::Connection::open(&database_path).unwrap();
        seed_preserved_transcript_rows(&raw);
        install_legacy_observation_shape(&raw);
        seed_active_temporal_generation(&raw);
        raw.execute_batch(
            "INSERT INTO session_temporal_observation_effects
                (observation_id, observation_sequence, session_id, receipt_id,
                 effect_digest, output_count, recorded_at)
             VALUES ('observation.legacy', 1, 'session.fixture', 'receipt.legacy',
                     'digest.effect', 1, 1);",
        )
        .expect("seed one observation-derived temporal effect");
        assert_eq!(
            refresh_discovery_frontier(&raw, "session.fixture"),
            None,
            "the seeded store must start with the rebuild suppressed, or this \
             test proves nothing"
        );
    }

    let mut raw = rusqlite::Connection::open(&database_path).unwrap();
    let report = reset_refused_observation_authority(&mut raw)
        .expect("a populated session-temporal projection must not block the scoped reset");
    assert_eq!(
        report.cleared_derived_temporal_rows, 7,
        "every seeded projection row must be accounted for: {report:?}"
    );
    for table in [
        "session_temporal_generations",
        "session_temporal_projection_receipts",
        "session_relation_receipts",
        "session_refresh_operations",
        "session_occurrences",
        "session_current_entities",
        "session_temporal_observation_effects",
    ] {
        assert_eq!(
            count(&raw, table),
            0,
            "{table} must not survive the reset advertising coverage of deleted rows"
        );
    }
    assert!(
        foreign_key_violations(&raw).is_empty(),
        "the committed reset must be referentially coherent"
    );
    assert_eq!(
        count(&raw, "sessions"),
        1,
        "state outside the observation projection must be preserved"
    );

    // Re-ingest one native event under the rebuilt authority: the frontier
    // that used to be frozen at 100 must no longer exclude sequence 1.
    raw.execute_batch(
        "INSERT INTO sanitization_receipts
            (receipt_id, sanitizer_version, payload_digest, receipt_json)
         VALUES ('receipt.rebuilt', 'v1', 'digest.payload', '{}');
         INSERT INTO observations
            (observation_id, payload_digest, receipt_id, observation_json,
             committed_cursor_json)
         VALUES ('observation.rebuilt', 'digest.payload', 'receipt.rebuilt', '{}', '{}');
         INSERT INTO session_temporal_observation_effects
            (observation_id, observation_sequence, session_id, receipt_id,
             effect_digest, output_count, recorded_at)
         VALUES ('observation.rebuilt', 1, 'session.fixture', 'receipt.rebuilt',
                 'digest.effect', 1, 9);",
    )
    .expect("re-ingest one native event under the rebuilt authority");
    assert_eq!(
        refresh_discovery_frontier(&raw, "session.fixture"),
        Some(0),
        "the rebuilt stream must be rediscovered from zero, not excluded by the \
         frontier of the generation the reset invalidated"
    );
}

/// Seeds the anchor state one admitted observation leaves behind: its
/// exact-observation anchor bound through `observation_retrieval_anchors` and
/// `observation_projection_provenance`, the native-record alias resolving to
/// it, and a repository-capture anchor bound through
/// `observation_repository_provenance`.
fn seed_observation_bound_anchors(conn: &rusqlite::Connection) {
    conn.pragma_update(None, "foreign_keys", false)
        .expect("disable foreign keys for fixture seeding");
    conn.execute_batch(
        "INSERT INTO retrieval_anchors
            (anchor_id, anchor_json, owner_json, projection_generation)
         VALUES ('anchor.observation', '{}', '{\"kind\":\"profile\"}', 'generation.fixture'),
                ('anchor.capture', '{}', '{\"kind\":\"profile\"}', 'generation.fixture');
         INSERT INTO retrieval_anchor_aliases
            (owner_json, alias_kind, locator_digest, anchor_id)
         VALUES ('{\"kind\":\"profile\"}', '\"provider_record\"', '\"sha256:record\"',
                 'anchor.observation');
         INSERT INTO observation_retrieval_anchors (observation_id, anchor_id)
         VALUES ('observation.legacy', 'anchor.observation');
         INSERT INTO observation_repository_provenance
            (observation_id, availability_json, capture_json, retrieval_anchor_id, owner_json)
         VALUES ('observation.legacy', '{}', '{}', 'anchor.capture', '{\"kind\":\"profile\"}');
         INSERT INTO observation_projection_provenance
            (projector_version, observation_id, output_ordinal, retrieval_anchor_id,
             receipt_id, output_provider, output_message_id, output_digest, message_created)
         VALUES ('projector.v1', 'observation.legacy', 0, 'anchor.observation',
                 'receipt.legacy', 'claude', 'message.fixture', 'digest.output', 1);",
    )
    .expect("seed the anchors one admitted observation binds");
}

/// The anchors an admitted observation binds, and the native-record aliases
/// resolving to them, are re-derived by the next admission of the same
/// records — and verified field-for-field against whatever row already holds
/// the anchor id. A retained anchor whose transcript file was since replaced
/// (a new source generation) fails that verification as a storage collision
/// on every retry; a retained alias whose record was revised refuses it
/// deterministically. Either leaves the rebuild the reset promises undone, so
/// they go with the observation stream, while anchors preserved rows own
/// (here a summary anchor) stay, the immutability guards return, and the
/// committed store is referentially coherent.
#[tokio::test]
async fn observation_bound_anchors_and_aliases_reset_with_the_stream() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("sessions.db");
    install_registered_store(&database_path).await;
    {
        let raw = rusqlite::Connection::open(&database_path).unwrap();
        seed_preserved_transcript_rows(&raw);
        install_legacy_observation_shape(&raw);
        seed_active_temporal_generation(&raw);
        seed_observation_bound_anchors(&raw);
        raw.execute_batch(
            "INSERT INTO session_summary_nodes
                (summary_id, session_id, summary_anchor_id, summary_text,
                 index_text, source_horizon_json, created_at)
             VALUES ('summary.fixture', 'session.fixture', 'anchor.fixture',
                     'summary', 'index', '{}', 1);",
        )
        .expect("seed a preserved summary naming its own anchor");
        assert_eq!(count(&raw, "retrieval_anchors"), 3);
        assert_eq!(count(&raw, "retrieval_anchor_aliases"), 1);
    }

    let mut raw = rusqlite::Connection::open(&database_path).unwrap();
    let report =
        reset_refused_observation_authority(&mut raw).expect("scoped reset of an anchored store");
    assert_eq!(
        report.cleared_retrieval_anchor_rows, 3,
        "two observation-bound anchors and one alias must be accounted for: {report:?}"
    );
    assert_eq!(
        raw.query_row(
            "SELECT group_concat(anchor_id, ',') FROM retrieval_anchors",
            [],
            |row| row.get::<_, String>(0),
        )
        .unwrap(),
        "anchor.fixture",
        "only the anchor a preserved summary names may survive"
    );
    assert_eq!(
        count(&raw, "retrieval_anchor_aliases"),
        0,
        "no alias may keep resolving a native record to an anchor that is gone"
    );
    for trigger in [
        "retrieval_anchors_immutable_delete",
        "retrieval_anchor_aliases_immutable_delete",
        "retrieval_anchors_immutable_update",
    ] {
        assert!(
            trigger_exists(&raw, trigger),
            "{trigger} must be reinstalled before the reset commits"
        );
    }
    assert!(
        raw.execute("DELETE FROM retrieval_anchors", []).is_err(),
        "runtime immutability must be back in force"
    );
    assert!(
        foreign_key_violations(&raw).is_empty(),
        "the committed reset must be referentially coherent"
    );
    assert_eq!(count(&raw, "session_summary_nodes"), 1);

    // Re-admitting the same native record under a moved source generation
    // must now be able to write its anchor and alias afresh.
    raw.execute_batch(
        "INSERT INTO retrieval_anchors
            (anchor_id, anchor_json, owner_json, projection_generation)
         VALUES ('anchor.observation', '{\"generation\":2}', '{\"kind\":\"profile\"}',
                 'generation.fixture');
         INSERT INTO retrieval_anchor_aliases
            (owner_json, alias_kind, locator_digest, anchor_id)
         VALUES ('{\"kind\":\"profile\"}', '\"provider_record\"', '\"sha256:record\"',
                 'anchor.observation');",
    )
    .expect("the rebuilt authority must own the anchor identity again");
}

/// The rebuilt authority re-reads every native transcript only if nothing
/// tells it the corpus was already swept. The Codex history frontier and
/// corpus epoch, per-provider coverage verdicts, host discovery frontiers, and
/// queued discovery paths all live in `parse_offsets`; a surviving `complete`
/// verdict would leave the reset store empty forever while every read reports
/// a healthy, current, empty projection. The hook-analytics import cursor
/// shares the table but feeds `analytics_events`, so it stays.
#[tokio::test]
async fn native_source_scheduling_cursors_reset_with_the_stream() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("sessions.db");
    install_registered_store(&database_path).await;
    {
        let raw = rusqlite::Connection::open(&database_path).unwrap();
        install_legacy_observation_shape(&raw);
        raw.execute_batch(
            "INSERT INTO parse_offsets (file_path, byte_offset, mtime, file_id) VALUES
                ('host-coverage://codex/v1', 0, 2, 1),
                ('tracedecay-internal:codex-history-frontier:v2', 0, 0, 3),
                ('tracedecay-internal:codex-history-epoch:v2', -5565623358034642743,
                 -2743919526740859943, 1),
                ('tracedecay-internal:project-ingest-provider-frontier:v1', 11, 0, 1),
                ('host-frontier://kimi/discovery/v1', 0, 4, 0),
                ('host-discovery-queue://codex/v1/L2hvbWUvcm9sbG91dA', 1, 0, 1),
                ('hook_analytics:/project/.tracedecay/hook_analytics.jsonl', 4096, 7, 1);",
        )
        .expect("seed every parse-offset tenant");
    }

    let mut raw = rusqlite::Connection::open(&database_path).unwrap();
    let report = reset_refused_observation_authority(&mut raw)
        .expect("scoped reset of a store with scheduling cursors");
    assert_eq!(
        report.cleared_native_source_cursor_rows, 6,
        "every native-source scheduling cursor must be accounted for: {report:?}"
    );
    assert_eq!(
        raw.query_row(
            "SELECT group_concat(file_path, ',') FROM parse_offsets",
            [],
            |row| row.get::<_, String>(0),
        )
        .unwrap(),
        "hook_analytics:/project/.tracedecay/hook_analytics.jsonl",
        "only the analytics import cursor may survive the reset"
    );
}

/// A disposition (for example a redaction) recorded against an observation
/// anchor is preserved evidence the reset cannot rebind, so a store carrying
/// one refuses atomically instead of orphaning it.
#[tokio::test]
async fn anchor_dispositions_on_observation_anchors_refuse_atomically() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("sessions.db");
    install_registered_store(&database_path).await;
    {
        let raw = rusqlite::Connection::open(&database_path).unwrap();
        seed_preserved_transcript_rows(&raw);
        install_legacy_observation_shape(&raw);
        seed_observation_bound_anchors(&raw);
        raw.execute_batch(
            "INSERT INTO retrieval_anchor_dispositions
                (disposition_id, anchor_id, owner_json, state, superseded_by,
                 reason_class, effective_at, record_json)
             VALUES ('disposition.redacted', 'anchor.observation', '{\"kind\":\"profile\"}',
                     'redacted', NULL, 'redaction', 1, '{}');",
        )
        .expect("seed a redaction against the observation anchor");
    }

    let mut raw = rusqlite::Connection::open(&database_path).unwrap();
    let error = reset_refused_observation_authority(&mut raw)
        .expect_err("an anchor disposition the reset cannot rebind must refuse");
    assert!(
        matches!(
            &error,
            TraceDecayError::Config { message }
                if message.contains("retrieval_anchor_dispositions")
                    && message.contains("nothing was reset")
        ),
        "unexpected error for a preserved anchor disposition: {error}"
    );
    assert_eq!(count(&raw, "retrieval_anchors"), 2);
    assert_eq!(count(&raw, "retrieval_anchor_aliases"), 1);
    assert_eq!(count(&raw, "retrieval_anchor_dispositions"), 1);
    assert!(
        trigger_exists(&raw, "retrieval_anchors_immutable_delete"),
        "a rolled-back reset must leave the anchor guards in place"
    );
}

/// A scoped reset must never orphan preserved evidence. External payload
/// manifests are durable LCM publication metadata whose receipt lives in the
/// `sanitization_receipts` table the reset recreates empty, and they are not
/// reconstructible from the transcripts — so a store holding one refuses
/// atomically instead of having its external-payload metadata deleted to make
/// the reset succeed. Everything else preserved keeps its evidence, and
/// `PRAGMA foreign_key_check` proves it.
#[tokio::test]
async fn preserved_dependencies_refuse_atomically_and_stay_coherent() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("with-manifest.db");
    install_registered_store(&database_path).await;
    {
        let raw = rusqlite::Connection::open(&database_path).unwrap();
        seed_preserved_transcript_rows(&raw);
        install_legacy_observation_shape(&raw);
        seed_active_temporal_generation(&raw);
        raw.execute_batch(
            "INSERT INTO lcm_external_payloads
                (payload_ref, provider, session_id, message_id, kind, content_hash,
                 byte_count, char_count)
             VALUES ('payload.fixture', 'claude', 'session.fixture',
                     'message.fixture', 'text', 'digest.payload', 1, 1);
             INSERT INTO session_external_payload_manifests
                (payload_ref, session_id, payload_digest, manifest_json, receipt_id,
                 created_at)
             VALUES ('payload.fixture', 'session.fixture', 'digest.payload', '{}',
                     'receipt.legacy', 1);",
        )
        .expect("seed one external payload manifest");
    }

    let mut raw = rusqlite::Connection::open(&database_path).unwrap();
    let error = reset_refused_observation_authority(&mut raw)
        .expect_err("a preserved dependency with no safe treatment must refuse");
    assert!(
        matches!(
            &error,
            TraceDecayError::Config { message }
                if message.contains("session_external_payload_manifests")
                    && message.contains("nothing was reset")
        ),
        "unexpected error for a preserved dependency: {error}"
    );
    assert_eq!(
        count(&raw, "session_temporal_generations"),
        1,
        "a refused reset must mutate nothing"
    );
    assert_eq!(
        count(&raw, "session_external_payload_manifests"),
        1,
        "external-payload metadata must never be deleted to make a reset succeed"
    );

    // A store without that dependency resets, and everything preserved keeps
    // the evidence it needs.
    let preserved_path = directory.path().join("preserved.db");
    install_registered_store(&preserved_path).await;
    {
        let raw = rusqlite::Connection::open(&preserved_path).unwrap();
        seed_preserved_transcript_rows(&raw);
        install_legacy_observation_shape(&raw);
        seed_active_temporal_generation(&raw);
        raw.execute_batch(
            "INSERT INTO session_summary_nodes
                (summary_id, session_id, summary_anchor_id, summary_text,
                 index_text, source_horizon_json, created_at)
             VALUES ('summary.fixture', 'session.fixture', 'anchor.fixture',
                     'summary', 'index', '{}', 1);",
        )
        .expect("seed preserved LCM content");
    }
    let mut preserved = rusqlite::Connection::open(&preserved_path).unwrap();
    reset_refused_observation_authority(&mut preserved)
        .expect("scoped reset of a store with no unresolvable dependency");
    assert_eq!(
        count(&preserved, "session_summary_nodes"),
        1,
        "preserved LCM content must survive the scoped reset"
    );
    assert_eq!(
        count(&preserved, "retrieval_anchors"),
        1,
        "the anchors preserved summaries name must survive with them"
    );
    assert_eq!(
        count(&preserved, "session_summary_availability"),
        0,
        "per-generation availability verdicts go with the generation they judged"
    );
    assert!(
        foreign_key_violations(&preserved).is_empty(),
        "every preserved row must still resolve its evidence"
    );
}

/// The reset removes immutability triggers, deletes the projection, restores
/// the triggers and enrolls the new scheme inside one transaction. A failure
/// after the trigger removal and after the deletion — here the referential
/// integrity check, the last step before commit — must therefore leave the
/// store exactly as refused: no missing trigger, no partial deletion, and no
/// premature scheme enrollment that would let the old-scheme rows readmit.
#[tokio::test]
async fn a_failure_after_deletion_leaves_the_refused_store_unchanged() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("sessions.db");
    install_registered_store(&database_path).await;
    {
        let raw = rusqlite::Connection::open(&database_path).unwrap();
        seed_preserved_transcript_rows(&raw);
        install_legacy_observation_shape(&raw);
        seed_active_temporal_generation(&raw);
        raw.execute(
            "INSERT INTO session_query_cursor_keys
                (key_id, key_version, key_material, created_at, retired_at)
             VALUES ('cursor-key-reset-rollback', 1, ?1, 1, NULL)",
            rusqlite::params![vec![7_u8; 32]],
        )
        .unwrap();
        // Old-scheme rows: the enrollment marker this reset would add is
        // absent, so its premature appearance would be visible.
        raw.execute(
            "DELETE FROM global_schema_migrations WHERE migration = ?1",
            [super::OBSERVATION_NATIVE_SOURCE_SCHEME_MIGRATION],
        )
        .expect("make the fixture an old-scheme store");
        // A preserved summary naming an anchor that does not exist: the reset
        // cannot repair it, so its integrity check fails after everything else
        // in the transaction has already run.
        raw.execute_batch(
            "INSERT INTO session_summary_nodes
                (summary_id, session_id, summary_anchor_id, summary_text,
                 index_text, source_horizon_json, created_at)
             VALUES ('summary.orphan', 'session.fixture', 'anchor.missing',
                     'summary', 'index', '{}', 1);",
        )
        .expect("seed a preserved row the reset cannot make coherent");
    }

    let mut raw = rusqlite::Connection::open(&database_path).unwrap();
    let error = reset_refused_observation_authority(&mut raw)
        .expect_err("an incoherent result must never commit");
    assert!(
        matches!(
            &error,
            TraceDecayError::Config { message }
                if message.contains("retrieval_anchors") && message.contains("nothing was reset")
        ),
        "unexpected error for an unresolvable dependency: {error}"
    );

    // Reopen: the store is the refused one it was, whole.
    let mut reopened = rusqlite::Connection::open(&database_path).unwrap();
    for trigger in [
        "session_temporal_observation_effects_immutable_delete_v1",
        "session_temporal_generations_delete_guard_v1",
        "session_temporal_projection_receipts_immutable_delete_v1",
        "session_refresh_bindings_immutable_delete_v1",
    ] {
        assert!(
            trigger_exists(&reopened, trigger),
            "{trigger} must be back before the transaction that dropped it ends"
        );
    }
    assert_eq!(count(&reopened, "session_query_cursor_keys"), 1);
    let retained_key: (String, Vec<u8>, Option<i64>) = reopened
        .query_row(
            "SELECT key_id, key_material, retired_at FROM session_query_cursor_keys",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        retained_key,
        ("cursor-key-reset-rollback".to_owned(), vec![7_u8; 32], None)
    );
    assert_eq!(count(&reopened, "session_temporal_generations"), 1);
    assert_eq!(count(&reopened, "session_occurrences"), 1);
    assert_eq!(count(&reopened, "session_refresh_operations"), 1);
    assert!(
        !scheme_migration_recorded(&reopened),
        "a rolled-back reset must not enroll the new native-source scheme"
    );
    assert!(
        reset_refused_observation_authority(&mut reopened).is_err(),
        "the store must still be refused after a rolled-back reset"
    );
}

/// The reset's promise is that the rebuilt authority re-reads what it lost.
/// Row counts alone do not prove it: an ingestion cursor, an advance ledger
/// entry, or a projector checkpoint that outlived the reset would each silently
/// skip exactly the native events the rebuild depends on re-offering. Every
/// pre-reset cursor must therefore be refused after the reset — the tables come
/// back at the canonical shape and empty, the identical advance re-presents as
/// new rather than deduping against a survivor, and the temporal refresh query
/// discovers the re-ingested stream from zero instead of the frozen frontier.
#[tokio::test]
async fn pre_reset_cursors_are_refused_and_the_rebuilt_stream_is_rediscovered() {
    const ADVANCE: (&str, &str, &str) = (
        r#"{"provider":"claude"}"#,
        r#"{"project":"project.fixture"}"#,
        r#"{"through":100}"#,
    );

    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("sessions.db");
    install_registered_store(&database_path).await;
    {
        let raw = rusqlite::Connection::open(&database_path).unwrap();
        seed_preserved_transcript_rows(&raw);
        install_legacy_observation_shape(&raw);
        seed_active_temporal_generation(&raw);
        raw.execute(
            "INSERT INTO source_cursors(source_json, scope_json, cursor_json)
             VALUES (?1, ?2, ?3)",
            rusqlite::params![ADVANCE.0, ADVANCE.1, ADVANCE.2],
        )
        .expect("seed the pre-reset ingestion cursor");
        raw.execute(
            "INSERT INTO source_cursor_advances
                (source_json, scope_json, coverage_json, reason, receipt_id)
             VALUES (?1, ?2, ?3, 'admitted', 'receipt.legacy')",
            rusqlite::params![ADVANCE.0, ADVANCE.1, ADVANCE.2],
        )
        .expect("seed the pre-reset advance ledger entry");
        raw.execute_batch(
            "INSERT INTO observation_projection_checkpoints(projector_version, last_sequence)
             VALUES ('projector.v1', 100);",
        )
        .expect("seed the pre-reset projector checkpoint");
        assert_eq!(
            refresh_discovery_frontier(&raw, "session.fixture"),
            None,
            "the seeded store must start with the rebuild suppressed, or this \
             test proves nothing"
        );
    }

    let mut raw = rusqlite::Connection::open(&database_path).unwrap();
    let report =
        reset_refused_observation_authority(&mut raw).expect("scoped reset of a cursored store");
    for table in [
        "source_cursors",
        "source_cursor_advances",
        "observation_projection_checkpoints",
    ] {
        assert!(
            report.reset_tables.iter().any(|reset| reset == table),
            "{table} decides what gets re-offered and must be part of the reset: {report:?}"
        );
        assert!(
            table_exists(&raw, table),
            "{table} must be recreated at the canonical shape"
        );
        assert_eq!(
            count(&raw, table),
            0,
            "no pre-reset cursor may survive to skip the events the rebuild re-reads"
        );
    }

    // Re-ingest the native event the pre-reset advance already claimed to
    // cover, re-presenting that exact advance identity. A surviving row would
    // collide on the advance primary key; a refused one lets the rebuilt
    // authority record its own coverage.
    raw.execute_batch(
        "INSERT INTO sanitization_receipts
            (receipt_id, sanitizer_version, payload_digest, receipt_json)
         VALUES ('receipt.rebuilt', 'v1', 'digest.payload', '{}');
         INSERT INTO observations
            (observation_id, payload_digest, receipt_id, observation_json,
             committed_cursor_json)
         VALUES ('observation.rebuilt', 'digest.payload', 'receipt.rebuilt', '{}', '{}');
         INSERT INTO session_temporal_observation_effects
            (observation_id, observation_sequence, session_id, receipt_id,
             effect_digest, output_count, recorded_at)
         VALUES ('observation.rebuilt', 1, 'session.fixture', 'receipt.rebuilt',
                 'digest.effect', 1, 9);",
    )
    .expect("re-ingest one native event under the rebuilt authority");
    raw.execute(
        "INSERT INTO source_cursor_advances
            (source_json, scope_json, coverage_json, reason, receipt_id)
         VALUES (?1, ?2, ?3, 'admitted', 'receipt.rebuilt')",
        rusqlite::params![ADVANCE.0, ADVANCE.1, ADVANCE.2],
    )
    .expect("the rebuilt authority must be able to record the same coverage again");

    assert_eq!(
        refresh_discovery_frontier(&raw, "session.fixture"),
        Some(0),
        "the rebuilt stream must be rediscovered from zero, not excluded by the \
         frontier of the generation the reset invalidated"
    );
    assert!(
        foreign_key_violations(&raw).is_empty(),
        "the re-ingested stream must be referentially coherent"
    );
}

/// The host-observation journal attests the stream the reset destroys.
/// Leaving those receipts makes the next admission of the same observation
/// id a conflicting reuse. The writer ledger is installed only when the
/// runtime mounts a store; this offline fixture covers the receipt tables
/// the registered schema always carries.
#[tokio::test]
async fn host_observation_journal_resets_with_the_stream() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("sessions.db");
    install_registered_store(&database_path).await;
    {
        let raw = rusqlite::Connection::open(&database_path).unwrap();
        install_legacy_observation_shape(&raw);
        raw.execute_batch(
            "INSERT INTO external_source_states_v1 (
                binding_id, source_id, owner_kind, owner_id, definition_revision,
                definition_digest, binding_revision, binding_digest,
                source_frontier_digest, source_frontier_json,
                latest_source_receipt_digest
             ) VALUES (
                'binding.host', 'source.host-observation.codex', 'project',
                'project.fixture', 1, 'digest.definition', 1, 'digest.binding',
                'digest.frontier', '{}', 'digest.receipt'
             );
             INSERT INTO external_source_commit_receipts_v2 (
                binding_id, idempotency_key, request_digest, definition_revision,
                binding_revision, predecessor_frontier_digest,
                successor_frontier_digest, receipt_digest, receipt_json
             ) VALUES (
                'binding.host', 'sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
                'sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb',
                1, 1, 'digest.pred', 'digest.succ', 'digest.receipt', '{}'
             );",
        )
        .expect("seed a host-observation journal");
    }

    let mut raw = rusqlite::Connection::open(&database_path).unwrap();
    let report = reset_refused_observation_authority(&mut raw)
        .expect("scoped reset of a store with a host-observation journal");
    assert_eq!(
        report.cleared_external_source_rows, 2,
        "state and receipt must be accounted for: {report:?}"
    );
    assert_eq!(count(&raw, "external_source_states_v1"), 0);
    assert_eq!(count(&raw, "external_source_commit_receipts_v2"), 0);
}
