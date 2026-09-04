use std::collections::BTreeSet;

use sha2::{Digest, Sha256};
use tracedecay_runtime_core::db::engine::{QueryExecutor, params};

use crate::configuration::FreshConfigurationStoreEvidence;
use crate::schema_contract::{
    invariant_trigger_names_for_tables, released_v3_invariant_triggers_intact,
    starts_with_ignore_ascii_case, validate_released_v3_temporal_projection_receipt_contract,
    validate_session_graph_publication_schema_contract, validate_session_temporal_schema_contract,
};
use crate::{global_db_operation_error, global_db_operation_message};

use super::{
    MIGRATION_NAME, OPERATION, RELEASED_SESSION_TEMPORAL_SCHEMA_VERSION,
    SESSION_TEMPORAL_AUTHORITY, SESSION_TEMPORAL_SCHEMA_VERSION, TEMPORAL_FTS_CONTRACTS,
    TEMPORAL_TABLE_COLUMNS,
};

const TEMPORAL_FTS_SHADOW_TABLES: &[&str] = &[
    "session_occurrences_fts_config",
    "session_occurrences_fts_content",
    "session_occurrences_fts_data",
    "session_occurrences_fts_docsize",
    "session_occurrences_fts_idx",
    "session_summary_nodes_fts_config",
    "session_summary_nodes_fts_content",
    "session_summary_nodes_fts_data",
    "session_summary_nodes_fts_docsize",
    "session_summary_nodes_fts_idx",
];

// Exact normalized CREATE TABLE authority published in v0.1.0-beta.37. The
// structural PRAGMA contract cannot observe CHECK expressions, so released-v3
// admission also pins every durable temporal table definition by digest.
const RELEASED_V3_TEMPORAL_TABLE_DIGESTS: &[(&str, &str)] = &[
    (
        "session_agents",
        "94d2e78d1ea2030560a21360e7ee6dc12a03d0e82ea14e2ddee08baec83bf367",
    ),
    (
        "session_assertion_supersession",
        "8bc0df1352864a9777c9a2dbaf33abd34f2c8fc74751a23f2b7c74348ec8f93e",
    ),
    (
        "session_assertions",
        "e4c4b5dba6ea971f33079249a724cbf76c2da7a9316c083adb42aaba29e4d963",
    ),
    (
        "session_current_entities",
        "ae412c1be5634a6667a3223f3e9687d72fd111e46c7b179d3814531727ebad89",
    ),
    (
        "session_derived_evidence",
        "d0b67a582d4c25bef3337dfc7f5150c2449bc6754cf63e4f56fe052f6342ad36",
    ),
    (
        "session_derived_evidence_members",
        "985db57f82cb2693097e4b779916316b05b0432e17e7fb30adedcb89d731402b",
    ),
    (
        "session_external_payload_manifests",
        "35941f805b9f94bd7acac50226e6bf181f3fd0f6578f3ba2779f300703cdee50",
    ),
    (
        "session_occurrences",
        "e1eda19c4d136c5d64480a562867dd2de6bd509a389df75f3269df3b3026c565",
    ),
    (
        "session_query_cursor_keys",
        "b50e9d3ea86a4c0fb675a3e2a7b814e00a38d6ab52d62b2423f9271cd3fab1b7",
    ),
    (
        "session_refresh_batch_bindings",
        "0ca52e44d29058dda06caeca4287653ef2238b0d2b4f45669ab19462443552ed",
    ),
    (
        "session_refresh_bindings",
        "542a3e5138627d7ffe8a97b50bb0f8f43d4b5e8806b2d2c62a597f5787122558",
    ),
    (
        "session_refresh_operations",
        "abceb5507d1470be9e552ae36a8f508e349200b8d3f34b905aafeadbd715f27f",
    ),
    (
        "session_refresh_progress",
        "b5a85a981f23b186c0106e160b22dba2db9a8e233232c51725fcb7eec101a6a1",
    ),
    (
        "session_refresh_receipts",
        "8a01bd241fb669833630b2ba7976da8296f7602768caeffddbb15e6a270f3039",
    ),
    (
        "session_relation_effect_journal",
        "73e372d47f338bae3e25d461c76f14e8b9b7a1606184993450fbe6a9965c7e12",
    ),
    (
        "session_relation_receipts",
        "867dc83c80264f4b13aeab7f1ac51572a88ee5d614739a701ebddbb8dcb84a80",
    ),
    (
        "session_summary_availability",
        "e1da9569afbb4bb2829404ec1deefccc35d21ce3c6a65c3ca28800cca0d8c79e",
    ),
    (
        "session_summary_nodes",
        "9cd68c6f2f0e4224db1a0cc107f562157c62942b2349320c573be85128a040e6",
    ),
    (
        "session_temporal_generations",
        "731f643fa5d08f7902ea3ae06fbc672cd52923acaaabffdb9985b2c806143c34",
    ),
    (
        "session_temporal_observation_effects",
        "614ca8fb3b21b2e0d8c08dc009c7e78ae1c55e87d52804a673bc84a8e52f375e",
    ),
    (
        "session_temporal_projection_receipts",
        "63f8c00ff8b62d060c57fe09046acd223b24a56ebf4cfa8a6f99eb2d938d23f1",
    ),
    (
        "session_temporal_schema_migrations",
        "8e9267a82387fa36ed23b4b339bb23baf0fd75d125b53e114851bfee2b515619",
    ),
    (
        "session_threads",
        "f94bfebac5f60f42dab193fe09348865fb416685d48a952093ac566d4c89ea98",
    ),
    (
        "session_turn_members",
        "9b4bc136a68ab8af12eb26bdfb832d7a09f68f0b2494cabeb3fd3b52e118f023",
    ),
    (
        "session_turns",
        "d67b106b4b594c91f5443e8a5f9cbee643349a495766adfb312b758c6b7bb1a5",
    ),
];

/// Read-only admission result for the final session-temporal schema.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SessionTemporalSchemaAdmission {
    /// The persisted schema and its objects exactly match the final contract.
    Current,
    /// The store carries the exact schema shipped through beta.37.
    ReleasedV3,
    /// The registered store is proven empty and may receive the final contract.
    Fresh,
}

/// Classifies a store without changing its schema or retained session state.
#[hotpath::measure(future = true, label = "session_temporal.schema.admit")]
pub(crate) async fn require_admissible_session_temporal_schema(
    conn: &impl QueryExecutor,
    fresh_store: Option<&FreshConfigurationStoreEvidence>,
) -> tracedecay_domain::errors::Result<SessionTemporalSchemaAdmission> {
    let version = schema_version(conn)
        .await
        .map_err(|error| session_temporal_reset_required(error.to_string()))?;
    match version {
        Some(SESSION_TEMPORAL_SCHEMA_VERSION) => {
            validate_current_session_temporal_schema(conn).await?;
            Ok(SessionTemporalSchemaAdmission::Current)
        }
        Some(RELEASED_SESSION_TEMPORAL_SCHEMA_VERSION) => {
            validate_released_v3_session_temporal_schema(conn).await?;
            Ok(SessionTemporalSchemaAdmission::ReleasedV3)
        }
        Some(version) => Err(session_temporal_reset_required(format!(
            "persisted schema version {version} does not match final version {SESSION_TEMPORAL_SCHEMA_VERSION}"
        ))),
        None if fresh_store.is_some() => Ok(SessionTemporalSchemaAdmission::Fresh),
        None => Err(session_temporal_reset_required(
            "a nonempty store does not carry the final schema marker",
        )),
    }
}

pub(super) async fn validate_current_session_temporal_schema(
    conn: &impl QueryExecutor,
) -> tracedecay_domain::errors::Result<()> {
    let tables = TEMPORAL_TABLE_COLUMNS
        .iter()
        .map(|(table, _)| *table)
        .filter(|table| !table.ends_with("_fts"))
        .collect::<Vec<_>>();
    validate_session_temporal_schema_contract(conn, &tables)
        .await
        .map_err(|error| session_temporal_reset_required(error.to_string()))?;
    validate_temporal_namespace_tables(conn)
        .await
        .map_err(|error| session_temporal_reset_required(error.to_string()))?;
    validate_session_graph_publication_schema_contract(conn)
        .await
        .map_err(|error| session_temporal_reset_required(error.to_string()))?;
    validate_temporal_fts_contracts(conn)
        .await
        .map_err(|error| session_temporal_reset_required(error.to_string()))?;
    validate_temporal_fts_match(conn)
        .await
        .map_err(|error| session_temporal_reset_required(error.to_string()))
}

pub(super) async fn validate_released_v3_session_temporal_schema(
    conn: &impl QueryExecutor,
) -> tracedecay_domain::errors::Result<()> {
    let tables = TEMPORAL_TABLE_COLUMNS
        .iter()
        .map(|(table, _)| *table)
        .filter(|table| {
            !table.ends_with("_fts")
                && *table != "session_temporal_projection_receipts"
                && *table != "session_relation_receipts"
        })
        .collect::<Vec<_>>();
    validate_session_temporal_schema_contract(conn, &tables)
        .await
        .map_err(|error| session_temporal_reset_required(error.to_string()))?;
    validate_released_v3_temporal_projection_receipt_contract(conn)
        .await
        .map_err(|error| session_temporal_reset_required(error.to_string()))?;
    validate_released_v3_temporal_table_definitions(conn).await?;
    if !released_v3_invariant_triggers_intact(conn)
        .await
        .map_err(|error| session_temporal_reset_required(error.to_string()))?
    {
        return Err(session_temporal_reset_required(
            "released v3 authority trigger contracts are absent or incompatible",
        ));
    }
    validate_released_v3_temporal_trigger_inventory(conn).await?;
    validate_temporal_namespace_tables(conn)
        .await
        .map_err(|error| session_temporal_reset_required(error.to_string()))?;
    validate_session_graph_publication_schema_contract(conn)
        .await
        .map_err(|error| session_temporal_reset_required(error.to_string()))?;
    validate_temporal_fts_contracts(conn)
        .await
        .map_err(|error| session_temporal_reset_required(error.to_string()))?;
    validate_temporal_fts_match(conn)
        .await
        .map_err(|error| session_temporal_reset_required(error.to_string()))
}

async fn validate_released_v3_temporal_table_definitions(
    conn: &impl QueryExecutor,
) -> tracedecay_domain::errors::Result<()> {
    let expected_tables = TEMPORAL_TABLE_COLUMNS
        .iter()
        .map(|(table, _)| *table)
        .filter(|table| !table.ends_with("_fts"))
        .collect::<BTreeSet<_>>();
    let contract_tables = RELEASED_V3_TEMPORAL_TABLE_DIGESTS
        .iter()
        .map(|(table, _)| *table)
        .collect::<BTreeSet<_>>();
    if contract_tables != expected_tables {
        return Err(global_db_operation_message(
            OPERATION,
            "released v3 CREATE TABLE authority is incomplete",
        ));
    }
    for (table, expected_digest) in RELEASED_V3_TEMPORAL_TABLE_DIGESTS {
        let mut rows = conn
            .query(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1",
                params![*table],
            )
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?
        else {
            return Err(session_temporal_reset_required(format!(
                "released v3 table '{table}' is missing"
            )));
        };
        let sql = row
            .get::<String>(0)
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let digest = hex::encode(Sha256::digest(normalize_schema_sql(&sql).as_bytes()));
        if digest != *expected_digest {
            return Err(session_temporal_reset_required(format!(
                "released v3 table '{table}' has an incompatible CREATE TABLE contract"
            )));
        }
    }
    Ok(())
}

async fn validate_released_v3_temporal_trigger_inventory(
    conn: &impl QueryExecutor,
) -> tracedecay_domain::errors::Result<()> {
    let temporal_tables = TEMPORAL_TABLE_COLUMNS
        .iter()
        .map(|(table, _)| *table)
        .collect::<Vec<_>>();
    let expected = invariant_trigger_names_for_tables(&temporal_tables)
        .into_iter()
        .collect::<BTreeSet<_>>();
    let temporal_tables = temporal_tables.into_iter().collect::<BTreeSet<_>>();
    let mut actual = BTreeSet::new();
    let mut rows = conn
        .query(
            "SELECT name, tbl_name FROM sqlite_master WHERE type = 'trigger' ORDER BY name",
            (),
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?
    {
        let name = row
            .get::<String>(0)
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let table = row
            .get::<String>(1)
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        if temporal_tables.contains(table.as_str()) {
            actual.insert(name);
        }
    }
    if actual.len() != expected.len()
        || actual
            .iter()
            .map(String::as_str)
            .ne(expected.iter().copied())
    {
        return Err(session_temporal_reset_required(
            "released v3 temporal trigger inventory is not exact",
        ));
    }
    Ok(())
}

async fn validate_temporal_namespace_tables(
    conn: &impl QueryExecutor,
) -> tracedecay_domain::errors::Result<()> {
    let expected = TEMPORAL_TABLE_COLUMNS
        .iter()
        .map(|(table, _)| *table)
        .chain(TEMPORAL_FTS_SHADOW_TABLES.iter().copied())
        .collect::<BTreeSet<_>>();
    let mut rows = conn
        .query(
            "SELECT name FROM sqlite_master
             WHERE type IN ('table', 'view') AND name NOT LIKE 'sqlite_%'
             ORDER BY name",
            (),
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?
    {
        let name = row
            .get::<String>(0)
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        if belongs_to_temporal_namespace(&name) && !expected.contains(name.as_str()) {
            return Err(global_db_operation_message(
                OPERATION,
                format!("unexpected session temporal table or view '{name}'"),
            ));
        }
    }
    Ok(())
}

fn belongs_to_temporal_namespace(name: &str) -> bool {
    [
        "session_agent_",
        "session_agents",
        "session_assertion",
        "session_current_entit",
        "session_derived_evidence",
        "session_external_payload",
        "session_logical_copy",
        "session_occurrence",
        "session_query_cursor",
        "session_refresh",
        "session_relation",
        "session_summary_availability",
        "session_summary_",
        "session_summary_nodes",
        "session_temporal",
        "session_thread",
        "session_turn",
    ]
    .iter()
    .any(|prefix| starts_with_ignore_ascii_case(name, prefix))
}

pub(super) fn session_temporal_reset_required(
    reason: impl Into<String>,
) -> tracedecay_domain::errors::TraceDecayError {
    tracedecay_domain::errors::TraceDecayError::reset_required(SESSION_TEMPORAL_AUTHORITY, reason)
}

pub(super) async fn validate_temporal_fts_contracts(
    conn: &impl QueryExecutor,
) -> tracedecay_domain::errors::Result<()> {
    for (table, expected_sql) in TEMPORAL_FTS_CONTRACTS {
        let mut rows = conn
            .query(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1",
                params![*table],
            )
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?
        else {
            return Err(global_db_operation_message(
                OPERATION,
                format!("temporal FTS table '{table}' is missing"),
            ));
        };
        let sql = row
            .get::<String>(0)
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        if normalize_fts_sql(&sql) != *expected_sql {
            return Err(global_db_operation_message(
                OPERATION,
                format!("table '{table}' has an incompatible temporal FTS contract"),
            ));
        }
    }
    Ok(())
}

fn normalize_fts_sql(sql: &str) -> String {
    normalize_schema_sql(sql)
}

fn normalize_schema_sql(sql: &str) -> String {
    sql.chars()
        .filter(|character| !character.is_whitespace() && *character != ';')
        .flat_map(char::to_lowercase)
        .collect::<String>()
        .replace("ifnotexists", "")
}

pub(super) async fn validate_temporal_fts_match(
    conn: &impl QueryExecutor,
) -> tracedecay_domain::errors::Result<()> {
    for (table, _) in TEMPORAL_FTS_CONTRACTS {
        conn.query(
            &format!("SELECT rowid FROM {table} WHERE {table} MATCH ?1 LIMIT 1"),
            params!["__tracedecay_temporal_fts_probe__"],
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    }
    Ok(())
}

async fn schema_version(
    conn: &impl QueryExecutor,
) -> tracedecay_domain::errors::Result<Option<i64>> {
    let mut tables = conn
        .query(
            "SELECT 1 FROM sqlite_master
             WHERE type = 'table' AND name = 'session_temporal_schema_migrations'",
            (),
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    if tables
        .next()
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?
        .is_none()
    {
        return Ok(None);
    }

    let mut rows = conn
        .query(
            "SELECT name, version FROM session_temporal_schema_migrations ORDER BY name",
            (),
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?
    else {
        return Err(global_db_operation_message(
            OPERATION,
            "session temporal schema marker is missing",
        ));
    };
    let name = row
        .get::<String>(0)
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    let version = row
        .get::<i64>(1)
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    if name != MIGRATION_NAME
        || rows
            .next()
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?
            .is_some()
    {
        return Err(global_db_operation_message(
            OPERATION,
            "session temporal schema marker is not the exact final singleton",
        ));
    }
    Ok(Some(version))
}
