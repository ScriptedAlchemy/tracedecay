use std::cmp::Ordering;
use std::collections::BTreeMap;

use tracedecay_runtime_core::db::engine::{Executor, params};
use serde::{Deserialize, Serialize};
use tracedecay_domain::{
    ClaudeSourceCursorV1, ClaudeSourceIdentityV1, DurableClaudeObservationV1, ObservationScopeV1,
    SanitizationReceiptV1,
};

use tracedecay_runtime_core::errors::Result;

use super::{db_error, db_message, projection, quote_identifier};

const SUMMARY_PUBLICATION_SANITIZER_VERSION: &str = "tracedecay.lcm-summary-publication.v1";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct FrozenPublicationReceipt {
    summary_id: String,
    disposition: String,
    published_at: i64,
    generation: i64,
    frozen_watermarks_json: String,
    source_horizon_json: String,
    publication_manifest_digest: String,
}

pub(super) async fn merge_observation_authority(conn: &impl Executor) -> Result<()> {
    seed_legacy_observation_backfill_watermarks(conn).await?;
    conn.execute_batch(
        "INSERT OR IGNORE INTO sanitization_receipts(
             receipt_id, sanitizer_version, payload_digest, receipt_json
         )
         SELECT receipt_id, sanitizer_version, payload_digest, receipt_json
         FROM source.sanitization_receipts;

         WITH target_frontier(last_sequence) AS (
             SELECT COALESCE(MAX(sequence), 0) FROM observations
         ), source_only AS (
             SELECT s.observation_id, s.payload_digest, s.receipt_id,
                    s.observation_json, s.committed_cursor_json,
                    ROW_NUMBER() OVER (ORDER BY s.sequence, s.observation_id) AS ordinal
             FROM source.observations AS s
             WHERE NOT EXISTS (
                 SELECT 1 FROM observations AS t
                 WHERE t.observation_id = s.observation_id
             )
         )
         INSERT INTO observations(
             sequence, observation_id, payload_digest, receipt_id,
             observation_json, committed_cursor_json
         )
         SELECT target_frontier.last_sequence + source_only.ordinal,
                source_only.observation_id, source_only.payload_digest,
                source_only.receipt_id, source_only.observation_json,
                source_only.committed_cursor_json
         FROM source_only CROSS JOIN target_frontier
         ORDER BY source_only.ordinal;

         INSERT OR IGNORE INTO retrieval_anchors(
             anchor_id, anchor_json, owner_json, projection_generation
         )
         SELECT anchor_id, anchor_json, owner_json, projection_generation
         FROM source.retrieval_anchors;

         INSERT OR IGNORE INTO observation_retrieval_anchors(observation_id, anchor_id)
         SELECT observation_id, anchor_id
         FROM source.observation_retrieval_anchors;

         INSERT OR IGNORE INTO retrieval_anchor_aliases(
             owner_json, alias_kind, locator_digest, anchor_id
         )
         SELECT owner_json, alias_kind, locator_digest, anchor_id
         FROM source.retrieval_anchor_aliases;

         INSERT OR IGNORE INTO observation_repository_provenance(
             observation_id, availability_json, capture_json,
             retrieval_anchor_id, owner_json
         )
         SELECT observation_id, availability_json, capture_json,
                retrieval_anchor_id, owner_json
         FROM source.observation_repository_provenance;",
    )
    .await
    .map_err(|error| db_error("merge_observation_authority", error))?;

    // Merged observations arrive above the target's frontier, and a source
    // whose own backfills had not converged brings no anchor or provenance
    // attachment for some of them. Clearing the completion markers re-arms the
    // target's resumable backfills, which continue from their retained
    // watermarks and so cover exactly the merged tail.
    for migration in [
        crate::root_seam::global_db::observation::OBSERVATION_ANCHOR_SCHEMA_MIGRATION,
        crate::root_seam::global_db::observation::OBSERVATION_PROVENANCE_SCHEMA_MIGRATION,
    ] {
        conn.execute(
            "DELETE FROM global_schema_migrations WHERE migration = ?1",
            params![migration],
        )
        .await
        .map_err(|error| db_error("merge_observation_authority", error))?;
    }

    merge_source_cursors(conn).await?;
    merge_source_cursor_advances(conn).await?;
    projection::merge(conn).await
}

/// Older completed backfills predate persisted watermarks. Preserve their
/// established target frontier before importing source rows and re-arming the
/// marker, so the resumed pass covers only the appended tail.
async fn seed_legacy_observation_backfill_watermarks(conn: &impl Executor) -> Result<()> {
    for migration in [
        crate::root_seam::global_db::observation::OBSERVATION_ANCHOR_SCHEMA_MIGRATION,
        crate::root_seam::global_db::observation::OBSERVATION_PROVENANCE_SCHEMA_MIGRATION,
    ] {
        conn.execute(
            "INSERT OR IGNORE INTO observation_backfill_watermarks(
                 migration, backfilled_through
             )
             SELECT migration, COALESCE((SELECT MAX(sequence) FROM observations), 0)
             FROM global_schema_migrations
             WHERE migration = ?1",
            params![migration],
        )
        .await
        .map_err(|error| db_error("seed legacy observation backfill watermark", error))?;
    }
    Ok(())
}

pub(in super::super) async fn verify_observation_merge(conn: &impl Executor) -> Result<()> {
    verify_observation_union(conn, "target_input", "source").await?;
    projection::verify(conn).await?;
    crate::root_seam::global_db::schema_stages::validate_observation_authority_connection(conn)
        .await
}

struct AuthorityUnionSpec {
    table: &'static str,
    identity: &'static str,
    scalar_columns: &'static str,
    representation_columns: &'static str,
    label: &'static str,
}

async fn verify_authority_table_union(
    conn: &impl Executor,
    target: &str,
    source: &str,
    spec: AuthorityUnionSpec,
) -> Result<()> {
    let AuthorityUnionSpec {
        table,
        identity,
        scalar_columns,
        representation_columns,
        label,
    } = spec;
    let target_schema = quote_identifier(target);
    let source_schema = quote_identifier(source);
    let table = quote_identifier(table);
    let scalar_differences = super::query_i64(
        conn,
        &format!(
            "WITH expected AS (
                 SELECT {scalar_columns} FROM {target_schema}.{table}
                 UNION SELECT {scalar_columns} FROM {source_schema}.{table}
             )
             SELECT
               (SELECT COUNT(*) FROM (
                    SELECT * FROM expected
                    EXCEPT SELECT {scalar_columns} FROM main.{table}
                ))
             + (SELECT COUNT(*) FROM (
                    SELECT {scalar_columns} FROM main.{table}
                    EXCEPT SELECT * FROM expected
                ))"
        ),
    )
    .await?;
    if scalar_differences != 0 {
        return Err(db_message(
            "verify_observation_authority_union",
            format!("destination {label} union differs from frozen inputs"),
        ));
    }

    // Duplicate identities were already compared as typed values and repaired to canonical JSON
    // during preflight. Raw representation equality remains exact for every non-overlap row.
    let representation_differences = super::query_i64(
        conn,
        &format!(
            "WITH expected AS (
                 SELECT {identity}, {representation_columns}
                 FROM {target_schema}.{table} AS target_row
                 WHERE NOT EXISTS (
                     SELECT 1 FROM {source_schema}.{table} AS source_row
                     WHERE source_row.{identity} = target_row.{identity}
                 )
                 UNION ALL
                 SELECT {identity}, {representation_columns}
                 FROM {source_schema}.{table} AS source_row
                 WHERE NOT EXISTS (
                     SELECT 1 FROM {target_schema}.{table} AS target_row
                     WHERE target_row.{identity} = source_row.{identity}
                 )
             ), actual AS (
                 SELECT {identity}, {representation_columns}
                 FROM main.{table} AS main_row
                 WHERE NOT (
                     EXISTS (
                         SELECT 1 FROM {target_schema}.{table} AS target_row
                         WHERE target_row.{identity} = main_row.{identity}
                     ) AND EXISTS (
                         SELECT 1 FROM {source_schema}.{table} AS source_row
                         WHERE source_row.{identity} = main_row.{identity}
                     )
                 )
             )
             SELECT
               (SELECT COUNT(*) FROM (SELECT * FROM expected EXCEPT SELECT * FROM actual))
             + (SELECT COUNT(*) FROM (SELECT * FROM actual EXCEPT SELECT * FROM expected))"
        ),
    )
    .await?;
    if representation_differences != 0 {
        return Err(db_message(
            "verify_observation_authority_union",
            format!("destination {label} representation differs from frozen inputs"),
        ));
    }
    Ok(())
}

pub(super) async fn verify_observation_union(
    conn: &impl Executor,
    target: &str,
    source: &str,
) -> Result<()> {
    verify_authority_table_union(
        conn,
        target,
        source,
        AuthorityUnionSpec {
            table: "sanitization_receipts",
            identity: "receipt_id",
            scalar_columns: "receipt_id, sanitizer_version, payload_digest",
            representation_columns: "receipt_json",
            label: "sanitization receipt",
        },
    )
    .await?;
    verify_authority_table_union(
        conn,
        target,
        source,
        AuthorityUnionSpec {
            table: "observations",
            identity: "observation_id",
            scalar_columns: "observation_id, payload_digest, receipt_id",
            representation_columns: "observation_json, committed_cursor_json",
            label: "observation",
        },
    )
    .await?;

    let target_schema = quote_identifier(target);
    let source_schema = quote_identifier(source);
    let advance_differences = super::query_i64(
        conn,
        &format!(
            "WITH expected AS (
                 SELECT source_json, scope_json, coverage_json, reason, receipt_id
                 FROM {target_schema}.source_cursor_advances
                 UNION
                 SELECT source_json, scope_json, coverage_json, reason, receipt_id
                 FROM {source_schema}.source_cursor_advances
             )
             SELECT
               (SELECT COUNT(*) FROM (
                    SELECT * FROM expected
                    EXCEPT SELECT source_json, scope_json, coverage_json, reason, receipt_id
                           FROM main.source_cursor_advances
                ))
             + (SELECT COUNT(*) FROM (
                    SELECT source_json, scope_json, coverage_json, reason, receipt_id
                    FROM main.source_cursor_advances
                    EXCEPT SELECT * FROM expected
                ))"
        ),
    )
    .await?;
    if advance_differences != 0 {
        return Err(db_message(
            "verify_observation_authority_union",
            "destination source cursor advance union differs from frozen inputs",
        ));
    }

    let mut expected_cursors = read_source_cursor_rows(conn, target).await?;
    for (key, source_value) in read_source_cursor_rows(conn, source).await? {
        match expected_cursors.get(&key) {
            None => {
                expected_cursors.insert(key, source_value);
            }
            Some((_, target_cursor)) => {
                let ordering = source_value.1.checked_cmp(target_cursor).map_err(|_| {
                    db_message(
                        "verify_observation_authority_union",
                        "frozen source cursor generations are not comparable",
                    )
                })?;
                if ordering == Ordering::Greater {
                    expected_cursors.insert(key, source_value);
                }
            }
        }
    }
    let expected_cursors = expected_cursors
        .into_iter()
        .map(|(key, (_, cursor))| (key, cursor))
        .collect::<BTreeMap<_, _>>();
    let actual_cursors = read_source_cursor_rows(conn, "main")
        .await?
        .into_iter()
        .map(|(key, (_, cursor))| (key, cursor))
        .collect::<BTreeMap<_, _>>();
    if actual_cursors != expected_cursors {
        return Err(db_message(
            "verify_observation_authority_union",
            "destination source cursor union differs from frozen inputs",
        ));
    }

    let queue_differences = super::query_i64(
        conn,
        "SELECT
            (SELECT COUNT(*) FROM (
                SELECT observation_id, sequence FROM observations
                EXCEPT SELECT observation_id, observation_sequence FROM projection_queue
            ))
          + (SELECT COUNT(*) FROM (
                SELECT observation_id, observation_sequence FROM projection_queue
                EXCEPT SELECT observation_id, sequence FROM observations
            ))",
    )
    .await?;
    if queue_differences != 0 {
        return Err(db_message(
            "verify_observation_authority_union",
            "destination observation projection queue differs from committed observations",
        ));
    }
    Ok(())
}

fn decode_authority<T: serde::de::DeserializeOwned>(
    json: &str,
    operation: &'static str,
) -> Result<T> {
    serde_json::from_str(json).map_err(|error| db_error(operation, error))
}

fn receipt_matches_columns(
    receipt: &SanitizationReceiptV1,
    receipt_id: &str,
    sanitizer_version: &str,
    payload_digest: &str,
) -> bool {
    receipt.receipt().receipt_id().as_str() == receipt_id
        && receipt.receipt().sanitizer_version().as_str() == sanitizer_version
        && receipt
            .payload()
            .map_or(payload_digest.is_empty(), |payload| {
                payload.digest().as_str() == payload_digest
            })
}

fn observation_matches_columns(
    observation: &DurableClaudeObservationV1,
    cursor: &ClaudeSourceCursorV1,
    observation_id: &str,
    payload_digest: &str,
    receipt_id: &str,
) -> bool {
    observation.observation_id().as_str() == observation_id
        && observation.payload_reference().digest().as_str() == payload_digest
        && observation.receipt().receipt().receipt_id().as_str() == receipt_id
        && cursor.source() == observation.source()
        && cursor.scope() == observation.scope()
        && cursor.generation() == observation.identity().generation()
        && cursor.byte_offset() == observation.identity().position().end()
}

async fn collect_receipt_repairs(conn: &impl Executor) -> Result<Vec<(String, String)>> {
    let mut receipt_rows = conn
        .query(
            "SELECT s.receipt_id, s.sanitizer_version, s.payload_digest, s.receipt_json,
                    t.sanitizer_version, t.payload_digest, t.receipt_json
             FROM source.sanitization_receipts AS s
             JOIN main.sanitization_receipts AS t USING(receipt_id)",
            (),
        )
        .await
        .map_err(|error| db_error("compare_duplicate_receipts", error))?;
    let mut receipt_repairs = Vec::new();
    while let Some(row) = receipt_rows
        .next()
        .await
        .map_err(|error| db_error("compare_duplicate_receipts", error))?
    {
        let receipt_id = row
            .get::<String>(0)
            .map_err(|error| db_error("compare_duplicate_receipts", error))?;
        let source_version = row
            .get::<String>(1)
            .map_err(|error| db_error("compare_duplicate_receipts", error))?;
        let source_digest = row
            .get::<String>(2)
            .map_err(|error| db_error("compare_duplicate_receipts", error))?;
        let source_json = row
            .get::<String>(3)
            .map_err(|error| db_error("compare_duplicate_receipts", error))?;
        let target_version = row
            .get::<String>(4)
            .map_err(|error| db_error("compare_duplicate_receipts", error))?;
        let target_digest = row
            .get::<String>(5)
            .map_err(|error| db_error("compare_duplicate_receipts", error))?;
        let target_json = row
            .get::<String>(6)
            .map_err(|error| db_error("compare_duplicate_receipts", error))?;
        if source_version != target_version || source_digest != target_digest {
            return Err(db_message(
                "merge_observation_authority",
                "sanitization receipt identity collision",
            ));
        }
        if source_version == SUMMARY_PUBLICATION_SANITIZER_VERSION {
            let source: FrozenPublicationReceipt =
                decode_authority(&source_json, "decode_duplicate_publication_receipt")?;
            let target: FrozenPublicationReceipt =
                decode_authority(&target_json, "decode_duplicate_publication_receipt")?;
            if source != target
                || target.disposition != "accepted"
                || target.generation <= 0
                || serde_json::from_str::<serde_json::Value>(&target.frozen_watermarks_json)
                    .is_err()
            {
                return Err(db_message(
                    "merge_observation_authority",
                    "summary publication receipt identity collision",
                ));
            }
            let canonical = serde_json::to_string(&target)
                .map_err(|error| db_error("canonicalize_duplicate_publication_receipt", error))?;
            if canonical != target_json {
                receipt_repairs.push((receipt_id, canonical));
            }
            continue;
        }

        let source = serde_json::from_str::<SanitizationReceiptV1>(&source_json);
        let target = serde_json::from_str::<SanitizationReceiptV1>(&target_json);
        match (source, target) {
            (Ok(source), Ok(target)) => {
                if source != target
                    || !receipt_matches_columns(
                        &source,
                        &receipt_id,
                        &source_version,
                        &source_digest,
                    )
                    || !receipt_matches_columns(
                        &target,
                        &receipt_id,
                        &target_version,
                        &target_digest,
                    )
                {
                    return Err(db_message(
                        "merge_observation_authority",
                        "sanitization receipt identity collision",
                    ));
                }
                let canonical = serde_json::to_string(&target)
                    .map_err(|error| db_error("canonicalize_duplicate_receipt", error))?;
                if canonical != target_json {
                    receipt_repairs.push((receipt_id, canonical));
                }
            }
            (Err(_), Err(_)) if source_json == target_json => {}
            _ => {
                return Err(db_message(
                    "merge_observation_authority",
                    "sanitization receipt identity collision",
                ));
            }
        }
    }
    drop(receipt_rows);
    Ok(receipt_repairs)
}

async fn collect_observation_repairs(
    conn: &impl Executor,
) -> Result<Vec<(String, String, String)>> {
    let mut observation_rows = conn
        .query(
            "SELECT s.observation_id, s.payload_digest, s.receipt_id,
                    s.observation_json, s.committed_cursor_json,
                    t.payload_digest, t.receipt_id,
                    t.observation_json, t.committed_cursor_json
             FROM source.observations AS s
             JOIN main.observations AS t USING(observation_id)",
            (),
        )
        .await
        .map_err(|error| db_error("compare_duplicate_observations", error))?;
    let mut observation_repairs = Vec::new();
    while let Some(row) = observation_rows
        .next()
        .await
        .map_err(|error| db_error("compare_duplicate_observations", error))?
    {
        let observation_id = row
            .get::<String>(0)
            .map_err(|error| db_error("compare_duplicate_observations", error))?;
        let source_digest = row
            .get::<String>(1)
            .map_err(|error| db_error("compare_duplicate_observations", error))?;
        let source_receipt = row
            .get::<String>(2)
            .map_err(|error| db_error("compare_duplicate_observations", error))?;
        let source_observation_json = row
            .get::<String>(3)
            .map_err(|error| db_error("compare_duplicate_observations", error))?;
        let source_cursor_json = row
            .get::<String>(4)
            .map_err(|error| db_error("compare_duplicate_observations", error))?;
        let target_digest = row
            .get::<String>(5)
            .map_err(|error| db_error("compare_duplicate_observations", error))?;
        let target_receipt = row
            .get::<String>(6)
            .map_err(|error| db_error("compare_duplicate_observations", error))?;
        let target_observation_json = row
            .get::<String>(7)
            .map_err(|error| db_error("compare_duplicate_observations", error))?;
        let target_cursor_json = row
            .get::<String>(8)
            .map_err(|error| db_error("compare_duplicate_observations", error))?;
        let source_observation: DurableClaudeObservationV1 =
            decode_authority(&source_observation_json, "decode_duplicate_observation")?;
        let target_observation: DurableClaudeObservationV1 =
            decode_authority(&target_observation_json, "decode_duplicate_observation")?;
        let source_cursor: ClaudeSourceCursorV1 =
            decode_authority(&source_cursor_json, "decode_duplicate_observation_cursor")?;
        let target_cursor: ClaudeSourceCursorV1 =
            decode_authority(&target_cursor_json, "decode_duplicate_observation_cursor")?;
        if source_digest != target_digest
            || source_receipt != target_receipt
            || source_observation != target_observation
            || source_cursor != target_cursor
            || !observation_matches_columns(
                &source_observation,
                &source_cursor,
                &observation_id,
                &source_digest,
                &source_receipt,
            )
            || !observation_matches_columns(
                &target_observation,
                &target_cursor,
                &observation_id,
                &target_digest,
                &target_receipt,
            )
        {
            return Err(db_message(
                "merge_observation_authority",
                "observation identity collision",
            ));
        }
        let canonical_observation = serde_json::to_string(&target_observation)
            .map_err(|error| db_error("canonicalize_duplicate_observation", error))?;
        let canonical_cursor = serde_json::to_string(&target_cursor)
            .map_err(|error| db_error("canonicalize_duplicate_observation_cursor", error))?;
        if canonical_observation != target_observation_json
            || canonical_cursor != target_cursor_json
        {
            observation_repairs.push((observation_id, canonical_observation, canonical_cursor));
        }
    }
    drop(observation_rows);
    Ok(observation_repairs)
}

async fn canonicalize_equivalent_duplicate_authority(conn: &impl Executor) -> Result<()> {
    let receipt_repairs = collect_receipt_repairs(conn).await?;
    let observation_repairs = collect_observation_repairs(conn).await?;
    if receipt_repairs.is_empty() && observation_repairs.is_empty() {
        return Ok(());
    }
    crate::root_seam::global_db::schema_stages::begin_observation_authority_canonical_repair(conn)
        .await?;
    for (receipt_id, receipt_json) in receipt_repairs {
        conn.execute(
            "UPDATE sanitization_receipts SET receipt_json = ?2 WHERE receipt_id = ?1",
            params![receipt_id, receipt_json],
        )
        .await
        .map_err(|error| db_error("canonicalize_duplicate_receipt", error))?;
    }
    for (observation_id, observation_json, cursor_json) in observation_repairs {
        conn.execute(
            "UPDATE observations
             SET observation_json = ?2, committed_cursor_json = ?3
             WHERE observation_id = ?1",
            params![observation_id, observation_json, cursor_json],
        )
        .await
        .map_err(|error| db_error("canonicalize_duplicate_observation", error))?;
    }
    crate::root_seam::global_db::schema_stages::finish_observation_authority_canonical_repair(conn)
        .await
}

pub(in super::super) async fn preflight_observation_merge(conn: &impl Executor) -> Result<()> {
    canonicalize_equivalent_duplicate_authority(conn).await?;

    if super::query_i64(
        conn,
        "SELECT COUNT(*)
         FROM source.source_cursor_advances AS source
         JOIN main.source_cursor_advances AS target USING(
             source_json, scope_json, coverage_json
         )
         WHERE source.reason IS NOT target.reason
            OR source.receipt_id IS NOT target.receipt_id",
    )
    .await?
        != 0
    {
        return Err(db_message(
            "merge_source_cursor_advances",
            "source cursor advance identity collision",
        ));
    }

    let target_cursors = read_source_cursor_rows(conn, "target_input").await?;
    let source_cursors = read_source_cursor_rows(conn, "source").await?;
    for (key, (_, source_cursor)) in &source_cursors {
        if let Some((_, target_cursor)) = target_cursors.get(key) {
            source_cursor.checked_cmp(target_cursor).map_err(|_| {
                db_message(
                    "merge_source_cursors",
                    "cursor generations are not comparable",
                )
            })?;
        }
    }

    projection::preflight(conn).await
}

async fn merge_source_cursors(conn: &impl Executor) -> Result<()> {
    let target_rows = read_source_cursor_rows(conn, "main").await?;
    let source_rows = read_source_cursor_rows(conn, "source").await?;
    for ((source_json, scope_json), (cursor_json, source_cursor)) in source_rows {
        let replace = match target_rows.get(&(source_json.clone(), scope_json.clone())) {
            None => true,
            Some((_, target_cursor)) => {
                matches!(
                    source_cursor.checked_cmp(target_cursor).map_err(|_| {
                        db_message(
                            "merge_source_cursors",
                            "cursor generations are not comparable",
                        )
                    })?,
                    Ordering::Greater
                )
            }
        };
        if replace {
            conn.execute(
                "INSERT INTO source_cursors(source_json, scope_json, cursor_json)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(source_json, scope_json) DO UPDATE SET
                     cursor_json = excluded.cursor_json",
                params![source_json, scope_json, cursor_json],
            )
            .await
            .map_err(|error| db_error("merge_source_cursors", error))?;
        }
    }
    Ok(())
}

async fn merge_source_cursor_advances(conn: &impl Executor) -> Result<()> {
    conn.execute_batch(
        "INSERT OR IGNORE INTO source_cursor_advances(
             source_json, scope_json, coverage_json, reason, receipt_id
         )
         SELECT source_json, scope_json, coverage_json, reason, receipt_id
         FROM source.source_cursor_advances;",
    )
    .await
    .map_err(|error| db_error("merge_source_cursor_advances", error))
}

async fn read_source_cursor_rows(
    conn: &impl Executor,
    schema: &str,
) -> Result<BTreeMap<(String, String), (String, ClaudeSourceCursorV1)>> {
    let sql = format!(
        "SELECT source_json, scope_json, cursor_json
         FROM {}.source_cursors ORDER BY source_json, scope_json",
        quote_identifier(schema)
    );
    let mut rows = conn
        .query(&sql, ())
        .await
        .map_err(|error| db_error("read_source_cursor_rows", error))?;
    let mut cursors = BTreeMap::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| db_error("read_source_cursor_rows", error))?
    {
        let source_json = row
            .get::<String>(0)
            .map_err(|error| db_error("read_source_cursor_rows", error))?;
        let scope_json = row
            .get::<String>(1)
            .map_err(|error| db_error("read_source_cursor_rows", error))?;
        let cursor_json = row
            .get::<String>(2)
            .map_err(|error| db_error("read_source_cursor_rows", error))?;
        let cursor = decode_source_cursor(&source_json, &scope_json, &cursor_json)?;
        cursors.insert((source_json, scope_json), (cursor_json, cursor));
    }
    Ok(cursors)
}

fn decode_source_cursor(
    source_json: &str,
    scope_json: &str,
    cursor_json: &str,
) -> Result<ClaudeSourceCursorV1> {
    let source = serde_json::from_str::<ClaudeSourceIdentityV1>(source_json)
        .map_err(|error| db_error("decode_source_cursor", error))?;
    let scope = serde_json::from_str::<ObservationScopeV1>(scope_json)
        .map_err(|error| db_error("decode_source_cursor", error))?;
    let cursor = serde_json::from_str::<ClaudeSourceCursorV1>(cursor_json)
        .map_err(|error| db_error("decode_source_cursor", error))?;
    if cursor.source() != &source || cursor.scope() != &scope {
        return Err(db_message(
            "decode_source_cursor",
            "cursor authority does not match its storage key",
        ));
    }
    Ok(cursor)
}
