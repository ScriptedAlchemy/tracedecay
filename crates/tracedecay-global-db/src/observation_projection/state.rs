use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap};

use tracedecay_domain::{CanonicalObservationIdV1, DurableObservationV1, PayloadDigestV1};
use tracedecay_store::{
    EDITED_FILES_KEY, ObservationProjection, ProjectionCheckpoint, ProjectionStoreError,
    ProjectionStoreResult, SESSION_MESSAGE_PROJECTOR_VERSION, SESSION_MESSAGE_PROJECTOR_VERSION_V4,
    SessionMessageProjection, SessionMessageRecord, SessionRecord, message_output_digest,
};

use tracedecay_lcm::raw::stored_message_record_select_columns;
use tracedecay_lcm::retrieval_content::projected_content_hash;
use tracedecay_lcm::{LcmError, LcmStorageKind};
use tracedecay_runtime_core::db::Database;
use tracedecay_runtime_core::db::engine::{Executor, IntoParams, QueryExecutor, Row, params};
use tracedecay_sessions::runtime::shared::durable_project_path_key;
use tracedecay_sessions::runtime::store_access::{
    message_record_from_row, session_record_from_row,
};

use super::apply::{derive_projection_with_alias, verify_provenance};

pub(super) fn storage(
    operation: &'static str,
    source: impl std::error::Error + Send + Sync + 'static,
) -> ProjectionStoreError {
    ProjectionStoreError::Storage {
        operation,
        source: Box::new(source),
    }
}

pub(super) fn storage_message(
    operation: &'static str,
    message: impl Into<String>,
) -> ProjectionStoreError {
    storage(operation, std::io::Error::other(message.into()))
}

pub(super) fn decode_sequence(value: i64, operation: &'static str) -> ProjectionStoreResult<u64> {
    u64::try_from(value).map_err(|_| storage_message(operation, "negative observation sequence"))
}

pub(super) fn decode_observation_row(
    row: &Row,
    operation: &'static str,
) -> ProjectionStoreResult<(u64, DurableObservationV1)> {
    let sequence = decode_sequence(
        row.get::<i64>(0)
            .map_err(|error| storage(operation, error))?,
        operation,
    )?;
    let observation_json = row
        .get::<String>(1)
        .map_err(|error| storage(operation, error))?;
    let observation = serde_json::from_str(&observation_json)
        .map_err(|error| storage("decode queued observation", error))?;
    Ok((sequence, observation))
}

pub(super) async fn read_observation(
    conn: &impl QueryExecutor,
    observation_id: &CanonicalObservationIdV1,
) -> ProjectionStoreResult<Option<(u64, DurableObservationV1)>> {
    let mut rows = conn
        .query(
            "SELECT sequence, observation_json FROM observations WHERE observation_id = ?1",
            params![observation_id.as_str()],
        )
        .await
        .map_err(|error| storage("read queued observation", error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage("read queued observation", error))?
    else {
        return Ok(None);
    };
    decode_observation_row(&row, "read queued observation").map(Some)
}

pub(super) async fn read_checkpoint(
    conn: &impl QueryExecutor,
) -> ProjectionStoreResult<ProjectionCheckpoint> {
    let mut rows = conn
        .query(
            "SELECT last_sequence FROM observation_projection_checkpoints
             WHERE projector_version = ?1",
            params![SESSION_MESSAGE_PROJECTOR_VERSION],
        )
        .await
        .map_err(|error| storage("read projector checkpoint", error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage("read projector checkpoint", error))?
    else {
        return Ok(ProjectionCheckpoint::new(0));
    };
    let sequence = decode_sequence(
        row.get::<i64>(0)
            .map_err(|error| storage("read projector checkpoint", error))?,
        "read projector checkpoint",
    )?;
    Ok(ProjectionCheckpoint::new(sequence))
}

pub(super) async fn write_checkpoint(
    conn: &impl Executor,
    sequence: u64,
) -> ProjectionStoreResult<ProjectionCheckpoint> {
    let sequence_i64 =
        i64::try_from(sequence).map_err(|_| ProjectionStoreError::SequenceOverflow(sequence))?;
    conn.execute(
        "INSERT INTO observation_projection_checkpoints (projector_version, last_sequence)
         VALUES (?1, ?2)
         ON CONFLICT(projector_version) DO UPDATE SET last_sequence = excluded.last_sequence",
        params![SESSION_MESSAGE_PROJECTOR_VERSION, sequence_i64],
    )
    .await
    .map_err(|error| storage("write projector checkpoint", error))?;
    Ok(ProjectionCheckpoint::new(sequence))
}

pub(super) async fn queued_sequence(
    conn: &impl QueryExecutor,
    observation_id: &CanonicalObservationIdV1,
) -> ProjectionStoreResult<Option<u64>> {
    let mut rows = conn
        .query(
            "SELECT observation_sequence FROM projection_queue WHERE observation_id = ?1",
            params![observation_id.as_str()],
        )
        .await
        .map_err(|error| storage("read projection queue", error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage("read projection queue", error))?
    else {
        return Ok(None);
    };
    decode_sequence(
        row.get::<i64>(0)
            .map_err(|error| storage("read projection queue", error))?,
        "read projection queue",
    )
    .map(Some)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ProjectionRetryState {
    pub(super) attempt_count: u32,
    pub(super) next_retry_at_micros: i64,
    pub(super) last_error: Option<String>,
}

pub(super) async fn projection_retry_state(
    conn: &impl QueryExecutor,
    observation_id: &CanonicalObservationIdV1,
) -> ProjectionStoreResult<Option<ProjectionRetryState>> {
    let mut rows = conn
        .query(
            "SELECT attempt_count, next_retry_at_micros, last_error
             FROM projection_queue WHERE observation_id = ?1",
            params![observation_id.as_str()],
        )
        .await
        .map_err(|error| storage("read projection retry state", error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage("read projection retry state", error))?
    else {
        return Ok(None);
    };
    let attempt_count = row
        .get::<i64>(0)
        .map_err(|error| storage("read projection retry state", error))?;
    let attempt_count = u32::try_from(attempt_count).map_err(|_| {
        storage_message(
            "read projection retry state",
            "projection retry attempt count is outside the supported range",
        )
    })?;
    let next_retry_at_micros = row
        .get::<i64>(1)
        .map_err(|error| storage("read projection retry state", error))?;
    if next_retry_at_micros < 0 {
        return Err(storage_message(
            "read projection retry state",
            "projection retry timestamp is negative",
        ));
    }
    let last_error = row
        .get::<Option<String>>(2)
        .map_err(|error| storage("read projection retry state", error))?;
    Ok(Some(ProjectionRetryState {
        attempt_count,
        next_retry_at_micros,
        last_error,
    }))
}

/// Queue rows one re-arm transaction touches.
pub(crate) const REARM_PROJECTION_RETRY_BATCH_ROWS: i64 = 1_024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct RearmedProjectionRetries {
    pub(crate) rows: u64,
    pub(crate) batches: u64,
}

/// A projection retry deadline paces re-attempts within the store mount that
/// observed the failure. A fresh mount re-arms every queued projection for
/// immediate replay so restart recovery drains commit-before-ack work instead
/// of waiting out a dead process's backoff. Attempt counts and last errors
/// persist, so a projection that fails again resumes its escalating delay from
/// the recorded attempt history.
///
/// The queue grows with session history, so the re-arm walks it in rowid
/// order, `batch_rows` deferred rows per short writer transaction: no single
/// statement or transaction scales with the queue, and foreground writes
/// interleave between batches. Rows queued after the walk starts belong to
/// this mount and are left alone.
#[hotpath::measure(
    future = true,
    label = "global_db.observation_projection.rearm_retries"
)]
pub(crate) async fn rearm_queued_projection_retries(
    database: &Database,
    batch_rows: i64,
) -> ProjectionStoreResult<RearmedProjectionRetries> {
    let mut rearmed = RearmedProjectionRetries::default();
    let Some(last_rowid) = query_optional_i64(
        &database.read_connection(),
        "SELECT MAX(rowid) FROM projection_queue",
        params![],
    )
    .await?
    else {
        return Ok(rearmed);
    };
    let mut after_rowid = 0_i64;
    while let Some((batch_end, rows)) =
        rearm_projection_retry_batch(database, after_rowid, last_rowid, batch_rows).await?
    {
        rearmed.rows += rows;
        rearmed.batches += 1;
        after_rowid = batch_end;
    }
    Ok(rearmed)
}

/// Re-arms the next `batch_rows` deferred rows after `after_rowid` in one
/// writer transaction and returns the last rowid it covered.
#[hotpath::measure(
    future = true,
    label = "global_db.observation_projection.rearm_retries.batch"
)]
async fn rearm_projection_retry_batch(
    database: &Database,
    after_rowid: i64,
    last_rowid: i64,
    batch_rows: i64,
) -> ProjectionStoreResult<Option<(i64, u64)>> {
    const OPERATION: &str = "rearm queued projection retries";
    let transaction = database
        .begin_write_transaction(OPERATION)
        .await
        .map_err(|error| storage(OPERATION, error))?;
    let Some(batch_end) = query_optional_i64(
        &transaction,
        "SELECT MAX(rowid) FROM (
            SELECT rowid FROM projection_queue
            WHERE rowid > ?1 AND rowid <= ?2 AND next_retry_at_micros > 0
            ORDER BY rowid LIMIT ?3
         )",
        params![after_rowid, last_rowid, batch_rows],
    )
    .await?
    else {
        transaction
            .rollback()
            .await
            .map_err(|error| storage(OPERATION, error))?;
        return Ok(None);
    };
    let rows = transaction
        .execute(
            "UPDATE projection_queue SET next_retry_at_micros = 0
             WHERE rowid > ?1 AND rowid <= ?2 AND next_retry_at_micros > 0",
            params![after_rowid, batch_end],
        )
        .await
        .map_err(|error| storage(OPERATION, error))?;
    transaction
        .commit()
        .await
        .map_err(|error| storage(OPERATION, error))?;
    Ok(Some((batch_end, rows)))
}

async fn query_optional_i64(
    conn: &impl QueryExecutor,
    sql: &str,
    params: impl IntoParams,
) -> ProjectionStoreResult<Option<i64>> {
    const OPERATION: &str = "read projection retry rearm window";
    let mut rows = conn
        .query(sql, params)
        .await
        .map_err(|error| storage(OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage(OPERATION, error))?
    else {
        return Ok(None);
    };
    row.get::<Option<i64>>(0)
        .map_err(|error| storage(OPERATION, error))
}

pub(super) async fn schedule_projection_retry(
    conn: &impl Executor,
    observation_id: &CanonicalObservationIdV1,
    attempt_count: u32,
    next_retry_at_micros: i64,
    last_error: &str,
) -> ProjectionStoreResult<()> {
    let updated = conn
        .execute(
            "UPDATE projection_queue
             SET attempt_count = ?2, next_retry_at_micros = ?3, last_error = ?4
             WHERE observation_id = ?1",
            params![
                observation_id.as_str(),
                i64::from(attempt_count),
                next_retry_at_micros,
                last_error,
            ],
        )
        .await
        .map_err(|error| storage("schedule projection retry", error))?;
    if updated == 1 {
        Ok(())
    } else {
        Err(ProjectionStoreError::NotQueued)
    }
}

pub(super) async fn consume_projection_queue_item(
    conn: &impl Executor,
    observation_id: &CanonicalObservationIdV1,
) -> ProjectionStoreResult<()> {
    conn.execute(
        "DELETE FROM projection_queue WHERE observation_id = ?1",
        params![observation_id.as_str()],
    )
    .await
    .map_err(|error| storage("consume projection queue item", error))?;
    Ok(())
}

pub(super) async fn read_session(
    conn: &impl QueryExecutor,
    provider: &str,
    session_id: &str,
) -> ProjectionStoreResult<Option<SessionRecord>> {
    let mut rows = conn
        .query(
            "SELECT provider, session_id, project_key, project_path, title, started_at, ended_at,
                    transcript_path, metadata_json, parent_session_id, is_subagent, agent_id,
                    parent_tool_use_id
             FROM sessions WHERE provider = ?1 AND session_id = ?2",
            params![provider, session_id],
        )
        .await
        .map_err(|error| storage("read projected session", error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage("read projected session", error))?
    else {
        return Ok(None);
    };
    session_record_from_row(&row)
        .map(Some)
        .map_err(|error| storage("decode projected session", error.source))
}

pub(super) async fn read_message(
    conn: &impl QueryExecutor,
    provider: &str,
    message_id: &str,
) -> ProjectionStoreResult<Option<SessionMessageRecord>> {
    let sql = format!(
        "SELECT {}
         FROM lcm_raw_messages AS message WHERE provider = ?1 AND message_id = ?2",
        stored_message_record_select_columns("message")
    );
    let mut rows = conn
        .query(&sql, params![provider, message_id])
        .await
        .map_err(|error| storage("read projected message", error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage("read projected message", error))?
    else {
        return Ok(None);
    };
    message_record_from_row(&row, 0)
        .map(Some)
        .map_err(|error| storage("decode projected message", error.source))
}

fn output_owner_lookup_sql(select_expr: &str, ordering: &str) -> String {
    format!(
        "(
                    SELECT {select_expr}
                    FROM observation_projection_provenance AS provenance
                    JOIN observations AS observation
                      ON observation.observation_id = provenance.observation_id
                    WHERE provenance.projector_version = groups.projector_version
                      AND provenance.output_provider = groups.output_provider
                      AND provenance.output_message_id = groups.output_message_id
                    ORDER BY observation.sequence {ordering}, provenance.observation_id {ordering}
                    LIMIT 1
                )"
    )
}

/// The one definition of the projected-output ownership aggregation that
/// populates `temp.observation_projection_output_state`: for every
/// `(projector_version, output_provider, output_message_id)` group in the
/// (optionally filtered) provenance authority it derives the canonical owner
/// (newest row when the projector owns the output, oldest otherwise), the
/// newest owner row and its sequence, and the group's ownership counts.
/// Whole-cache initialization and per-output re-aggregation both render
/// their statement from this single spelling so the aggregation cannot
/// drift; `provenance_filter` scopes only the grouped rows (the correlated
/// owner lookups constrain themselves to each group's exact key).
fn output_state_aggregation_sql(provenance_filter: &str) -> String {
    let newest_id = output_owner_lookup_sql("provenance.observation_id", "DESC");
    let oldest_id = output_owner_lookup_sql("provenance.observation_id", "ASC");
    let newest_sequence = output_owner_lookup_sql("observation.sequence", "DESC");
    format!(
        "INSERT INTO temp.observation_projection_output_state (
            projector_version, output_provider, output_message_id,
            canonical_observation_id, latest_observation_id, latest_sequence,
            projector_owned, owner_count
         )
         SELECT groups.projector_version, groups.output_provider, groups.output_message_id,
                CASE WHEN groups.projector_owned = 1 THEN {newest_id} ELSE {oldest_id} END,
                {newest_id},
                {newest_sequence},
                groups.projector_owned, groups.owner_count
         FROM (
            SELECT projector_version, output_provider, output_message_id,
                   MAX(message_created) AS projector_owned,
                   COUNT(*) AS owner_count
            FROM observation_projection_provenance
            {provenance_filter}
            GROUP BY projector_version, output_provider, output_message_id
         ) AS groups"
    )
}

/// Re-aggregates the ownership cache for one exact output from the
/// provenance authority: the output's cached row is removed and rebuilt
/// through the canonical aggregation ([`output_state_aggregation_sql`]), so
/// convergence paths (e.g. collided-provenance reconciliation) share the
/// initialization's single definition.
pub(super) async fn reaggregate_output_state_for_output(
    conn: &impl Executor,
    output_provider: &str,
    output_message_id: &str,
) -> ProjectionStoreResult<()> {
    conn.execute(
        "DELETE FROM temp.observation_projection_output_state
         WHERE projector_version = ?1
           AND output_provider = ?2 AND output_message_id = ?3",
        params![
            SESSION_MESSAGE_PROJECTOR_VERSION,
            output_provider,
            output_message_id,
        ],
    )
    .await
    .map_err(|error| storage("reset collided projection output state", error))?;
    conn.execute(
        &output_state_aggregation_sql(
            "WHERE projector_version = ?1
               AND output_provider = ?2 AND output_message_id = ?3",
        ),
        params![
            SESSION_MESSAGE_PROJECTOR_VERSION,
            output_provider,
            output_message_id,
        ],
    )
    .await
    .map_err(|error| storage("reaggregate collided projection output state", error))?;
    Ok(())
}

pub(super) struct ProjectionOutputOwner {
    pub(super) sequence: u64,
    pub(super) observation: DurableObservationV1,
}

pub(super) struct ProjectionOutputState {
    pub(super) latest: ProjectionOutputOwner,
    pub(super) canonical: DurableObservationV1,
    pub(super) projector_owned: bool,
    pub(super) owner_count: u64,
}

async fn projection_cache_tokens(conn: &impl Executor) -> ProjectionStoreResult<(i64, i64)> {
    let mut version_rows = conn
        .query("PRAGMA data_version", ())
        .await
        .map_err(|error| storage("read projection cache data version", error))?;
    let data_version = version_rows
        .next()
        .await
        .map_err(|error| storage("read projection cache data version", error))?
        .ok_or_else(|| storage_message("read projection cache data version", "no row"))?
        .get::<i64>(0)
        .map_err(|error| storage("read projection cache data version", error))?;
    drop(version_rows);
    let mut rowid_rows = conn
        .query(
            "SELECT COALESCE(MAX(rowid), 0) FROM observation_projection_provenance",
            (),
        )
        .await
        .map_err(|error| storage("read projection cache provenance rowid", error))?;
    let provenance_rowid = rowid_rows
        .next()
        .await
        .map_err(|error| storage("read projection cache provenance rowid", error))?
        .ok_or_else(|| storage_message("read projection cache provenance rowid", "no row"))?
        .get::<i64>(0)
        .map_err(|error| storage("read projection cache provenance rowid", error))?;
    Ok((data_version, provenance_rowid))
}

pub(super) async fn ensure_projection_output_state_cache(
    conn: &impl Executor,
) -> ProjectionStoreResult<()> {
    conn.execute_batch(
        "CREATE TEMP TABLE IF NOT EXISTS observation_projection_output_state (
            projector_version TEXT NOT NULL,
            output_provider TEXT NOT NULL,
            output_message_id TEXT NOT NULL,
            canonical_observation_id TEXT NOT NULL,
            latest_observation_id TEXT NOT NULL,
            latest_sequence INTEGER NOT NULL CHECK(latest_sequence >= 0),
            projector_owned INTEGER NOT NULL CHECK(projector_owned IN (0, 1)),
            owner_count INTEGER NOT NULL CHECK(owner_count > 0),
            PRIMARY KEY(projector_version, output_provider, output_message_id)
        ) WITHOUT ROWID;
        CREATE TEMP TABLE IF NOT EXISTS observation_projection_output_state_meta (
            initialized INTEGER PRIMARY KEY CHECK(initialized = 1),
            data_version INTEGER NOT NULL CHECK(data_version >= 0),
            provenance_rowid INTEGER NOT NULL CHECK(provenance_rowid >= 0)
        ) WITHOUT ROWID;",
    )
    .await
    .map_err(|error| storage("create projection output state cache", error))?;
    let (data_version, provenance_rowid) = projection_cache_tokens(conn).await?;

    let mut rows = conn
        .query(
            "SELECT data_version, provenance_rowid
             FROM temp.observation_projection_output_state_meta
             WHERE initialized = 1",
            (),
        )
        .await
        .map_err(|error| storage("read projection output state cache", error))?;
    let cached = rows
        .next()
        .await
        .map_err(|error| storage("read projection output state cache", error))?
        .map(|row| -> ProjectionStoreResult<(i64, i64)> {
            Ok((
                row.get(0)
                    .map_err(|error| storage("read projection output state cache", error))?,
                row.get(1)
                    .map_err(|error| storage("read projection output state cache", error))?,
            ))
        })
        .transpose()?;
    drop(rows);
    if let Some((stored_version, stored_rowid)) = cached
        && (stored_rowid == provenance_rowid || stored_version == data_version)
    {
        // `data_version` moves when any other connection commits, including
        // session rows that do not touch provenance. Rebuilding here scans the
        // whole ownership table once per queued observation. This writer keeps
        // the temp rows current itself; a foreign provenance insert changes
        // `MAX(rowid)` and still rebuilds.
        if stored_version != data_version || stored_rowid != provenance_rowid {
            conn.execute(
                "UPDATE temp.observation_projection_output_state_meta
                 SET data_version = ?1, provenance_rowid = ?2
                 WHERE initialized = 1",
                params![data_version, provenance_rowid],
            )
            .await
            .map_err(|error| storage("refresh projection cache token", error))?;
        }
        return Ok(());
    }

    conn.execute_batch(
        "DELETE FROM temp.observation_projection_output_state;
         DELETE FROM temp.observation_projection_output_state_meta;",
    )
    .await
    .map_err(|error| storage("initialize projection output state cache", error))?;
    conn.execute(&output_state_aggregation_sql(""), ())
        .await
        .map_err(|error| storage("initialize projection output state cache", error))?;
    conn.execute(
        "INSERT INTO temp.observation_projection_output_state_meta(
            initialized, data_version, provenance_rowid
         ) VALUES (1, ?1, ?2)",
        params![data_version, provenance_rowid],
    )
    .await
    .map_err(|error| storage("record projection cache data version", error))?;

    let mut rows = conn
        .query(
            "SELECT
                (SELECT COUNT(*) FROM observation_projection_provenance),
                (SELECT COALESCE(SUM(owner_count), 0)
                 FROM temp.observation_projection_output_state)",
            (),
        )
        .await
        .map_err(|error| storage("verify projection output state cache", error))?;
    let row = rows
        .next()
        .await
        .map_err(|error| storage("verify projection output state cache", error))?
        .ok_or_else(|| storage_message("verify projection output state cache", "no row"))?;
    let provenance_count = row
        .get::<i64>(0)
        .map_err(|error| storage("verify projection output state cache", error))?;
    let cached_count = row
        .get::<i64>(1)
        .map_err(|error| storage("verify projection output state cache", error))?;
    if provenance_count != cached_count {
        return Err(storage_message(
            "verify projection output state cache",
            "provenance aggregate mismatch",
        ));
    }
    Ok(())
}

pub(super) async fn read_output_state(
    conn: &impl QueryExecutor,
    projection: &SessionMessageProjection,
) -> ProjectionStoreResult<Option<ProjectionOutputState>> {
    let message = projection.message();
    let mut rows = conn
        .query(
            "SELECT state.latest_sequence, latest.observation_json,
                    canonical.observation_json, state.projector_owned, state.owner_count,
                    state.latest_observation_id, state.canonical_observation_id
             FROM temp.observation_projection_output_state AS state
             JOIN observations AS latest
               ON latest.observation_id = state.latest_observation_id
             JOIN observations AS canonical
               ON canonical.observation_id = state.canonical_observation_id
             WHERE state.projector_version = ?1
               AND state.output_provider = ?2
               AND state.output_message_id = ?3",
            params![
                SESSION_MESSAGE_PROJECTOR_VERSION,
                message.provider.as_str(),
                message.message_id.as_str()
            ],
        )
        .await
        .map_err(|error| storage("read projection output state", error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage("read projection output state", error))?
    else {
        return Ok(None);
    };
    let latest_sequence = decode_sequence(
        row.get::<i64>(0)
            .map_err(|error| storage("read projection output state", error))?,
        "read projection output state",
    )?;
    let latest_json = row
        .get::<String>(1)
        .map_err(|error| storage("read projection output state", error))?;
    let latest: DurableObservationV1 = serde_json::from_str(&latest_json)
        .map_err(|error| storage("decode latest projection output owner", error))?;
    let latest_observation_id: String = row
        .get(5)
        .map_err(|error| storage("read projection output state", error))?;
    let canonical_observation_id: String = row
        .get(6)
        .map_err(|error| storage("read projection output state", error))?;
    let canonical = if latest_observation_id == canonical_observation_id {
        latest.clone()
    } else {
        serde_json::from_str(
            &row.get::<String>(2)
                .map_err(|error| storage("read projection output state", error))?,
        )
        .map_err(|error| storage("decode canonical projection output owner", error))?
    };
    let projector_owned = row
        .get::<i64>(3)
        .map_err(|error| storage("read projection output state", error))?
        != 0;
    let owner_count = decode_sequence(
        row.get::<i64>(4)
            .map_err(|error| storage("read projection output state", error))?,
        "read projection output state",
    )?;
    if owner_count == 0 {
        return Err(storage_message(
            "read projection output state",
            "empty ownership aggregate",
        ));
    }
    Ok(Some(ProjectionOutputState {
        latest: ProjectionOutputOwner {
            sequence: latest_sequence,
            observation: latest,
        },
        canonical,
        projector_owned,
        owner_count,
    }))
}

pub(super) async fn has_other_projector_output_owner(
    conn: &impl QueryExecutor,
    projection: &SessionMessageProjection,
) -> ProjectionStoreResult<bool> {
    let message = projection.message();
    let mut rows = conn
        .query(
            "SELECT 1 FROM observation_projection_provenance
             WHERE output_provider = ?1 AND output_message_id = ?2
               AND projector_version <> ?3 AND projector_version <> ?4
             LIMIT 1",
            params![
                message.provider.as_str(),
                message.message_id.as_str(),
                SESSION_MESSAGE_PROJECTOR_VERSION,
                SESSION_MESSAGE_PROJECTOR_VERSION_V4,
            ],
        )
        .await
        .map_err(|error| storage("read cross-projector output owners", error))?;
    Ok(rows
        .next()
        .await
        .map_err(|error| storage("read cross-projector output owners", error))?
        .is_some())
}

async fn message_projection(
    conn: &impl QueryExecutor,
    observation: &DurableObservationV1,
    provider: &str,
    message_id: &str,
) -> ProjectionStoreResult<SessionMessageProjection> {
    if let Some(projection) = super::apply::derive_projection(observation)?
        .messages()
        .find(|projection| {
            projection.message().provider == provider
                && projection.message().message_id == message_id
        })
        .cloned()
    {
        return Ok(projection);
    }
    derive_projection_with_alias(conn, observation)
        .await?
        .messages()
        .find(|projection| {
            projection.message().provider == provider
                && projection.message().message_id == message_id
        })
        .cloned()
        .ok_or(ProjectionStoreError::ProvenanceCollision)
}

/// Session row the output verification compares against.
///
/// The projection-row batch loads sessions from message rows it found. A
/// missing or relocated message therefore has no batch entry even when the
/// expected session row is durable. That absence is not `row_missing`; the
/// single-output path's [`read_session`] is the authority for it.
pub(in super::super) async fn load_verified_session<'a>(
    conn: &impl QueryExecutor,
    rows: &'a ProjectionRowsBatch,
    provider: &str,
    session_id: &str,
) -> ProjectionStoreResult<Option<Cow<'a, SessionRecord>>> {
    if let Some(session) = rows.session(provider, session_id) {
        return Ok(Some(Cow::Borrowed(session)));
    }
    Ok(read_session(conn, provider, session_id)
        .await?
        .map(Cow::Owned))
}

pub(in super::super) async fn verify_projection_rows(
    conn: &impl QueryExecutor,
    projection: &SessionMessageProjection,
) -> ProjectionStoreResult<()> {
    let session = projection.session();
    let actual_session = read_session(conn, &session.provider, &session.session_id).await?;
    let message = projection.message();
    let actual_message = read_message(conn, &message.provider, &message.message_id).await?;
    verify_projection_rows_from_records(
        conn,
        projection,
        actual_session.as_ref(),
        actual_message.as_ref(),
    )
    .await
}

pub(in super::super) async fn verify_projection_rows_from_records(
    conn: &impl QueryExecutor,
    projection: &SessionMessageProjection,
    actual_session: Option<&SessionRecord>,
    actual_message: Option<&SessionMessageRecord>,
) -> ProjectionStoreResult<()> {
    let session = projection.session();
    // Re-derived sessions still carry the observation's host spelling. Apply the
    // same ingest-boundary normalization used by apply_session so macOS firmlink
    // expansions (/var -> /private/var) and user symlink families compare equal
    // to the persisted canonical row without putting FS probing into reconcile.
    let expected = canonicalize_session_project_paths(session);
    let session_conflict = actual_session.map_or(Some("row_missing"), |actual| {
        reconcile_session_rows_detailed(&canonicalize_session_project_paths(actual), &expected)
            .err()
            .map(SessionReconcileConflict::field)
    });
    if let Some(field) = session_conflict {
        return Err(ProjectionStoreError::SessionOutputCollision {
            provider: session.provider.clone(),
            session_id: session.session_id.clone(),
            field,
        });
    }
    let message = projection.message();
    let compatible = match actual_message {
        Some(actual) => {
            stored_row_matches(actual, message)?
                || protected_message_rows_compatible(conn, actual, message).await?
        }
        None => false,
    };
    if !compatible {
        return Err(ProjectionStoreError::OutputCollision {
            provider: message.provider.clone(),
            message_id: message.message_id.clone(),
        });
    }
    Ok(())
}

pub(super) fn same_projection_lineage(
    candidate: &DurableObservationV1,
    owner: &DurableObservationV1,
) -> bool {
    (candidate.source() == owner.source() && candidate.scope() == owner.scope())
        || tracedecay_domain::prove_cline_native_source_transition(owner, candidate).is_some()
}

pub(super) async fn verify_output_state(
    conn: &impl QueryExecutor,
    state: &ProjectionOutputState,
    projection: &SessionMessageProjection,
) -> ProjectionStoreResult<()> {
    if state.owner_count == 0 {
        return Err(ProjectionStoreError::ProvenanceCollision);
    }
    let message = projection.message();
    let owner_projection = message_projection(
        conn,
        &state.canonical,
        &message.provider,
        &message.message_id,
    )
    .await?;
    verify_provenance(conn, &owner_projection).await?;
    verify_projection_rows(conn, &owner_projection).await
}

/// Requested `(output_provider, output_message_id)` keys carried by one
/// batched authority statement. A page of audited outputs is answered in a
/// bounded number of round trips instead of two per projected message, while
/// each statement stays far below the runtime's per-query row admission.
const OUTPUT_AUTHORITY_BATCH_KEYS: usize = 256;

/// The canonical projection owner one output resolved to.
///
/// `canonical_observation_id` is retained alongside the decoded observation so
/// a caller that already derived that exact observation's projection can reuse
/// its own derivation instead of re-deriving the identical result.
#[derive(Debug)]
pub(in super::super) struct ProjectionOutputAuthority {
    pub(in super::super) canonical_observation_id: String,
    pub(in super::super) canonical: DurableObservationV1,
}

/// The storage columns of one projected message row. Not part of the output
/// digest; current provenance still authorizes them because they are derived
/// from the same observation.
pub(in super::super) struct ProjectionStorageColumns {
    pub(in super::super) session_id: String,
    pub(in super::super) storage_kind: String,
    pub(in super::super) content: String,
    pub(in super::super) content_hash: String,
    pub(in super::super) snippet_text: String,
    pub(in super::super) index_text: String,
}

pub(in super::super) struct ProjectionRowsBatch {
    sessions: HashMap<(String, String), SessionRecord>,
    messages: HashMap<(String, String), SessionMessageRecord>,
    storage_columns: HashMap<(String, String), ProjectionStorageColumns>,
}

impl ProjectionRowsBatch {
    pub(in super::super) fn session(
        &self,
        provider: &str,
        session_id: &str,
    ) -> Option<&SessionRecord> {
        self.sessions
            .get(&(provider.to_owned(), session_id.to_owned()))
    }

    pub(in super::super) fn message(
        &self,
        provider: &str,
        message_id: &str,
    ) -> Option<&SessionMessageRecord> {
        self.messages
            .get(&(provider.to_owned(), message_id.to_owned()))
    }

    pub(in super::super) fn storage_columns(
        &self,
        provider: &str,
        message_id: &str,
    ) -> Option<&ProjectionStorageColumns> {
        self.storage_columns
            .get(&(provider.to_owned(), message_id.to_owned()))
    }
}

pub(in super::super) async fn read_projection_rows_batch(
    conn: &impl QueryExecutor,
    outputs: &BTreeSet<(String, String)>,
) -> ProjectionStoreResult<ProjectionRowsBatch> {
    let mut messages = HashMap::with_capacity(outputs.len());
    let mut storage_columns = HashMap::with_capacity(outputs.len());
    let requested_keys = outputs.iter().collect::<Vec<_>>();
    for chunk in requested_keys.chunks(OUTPUT_AUTHORITY_BATCH_KEYS) {
        let requested = serde_json::to_string(
            &chunk
                .iter()
                .map(|(provider, message_id)| {
                    serde_json::json!({ "provider": provider, "message_id": message_id })
                })
                .collect::<Vec<_>>(),
        )
        .map_err(|error| storage("encode projected message request", error))?;
        let sql = format!(
            "SELECT {}, message.storage_kind, COALESCE(message.content, ''),
                    message.content_hash, message.snippet_text, message.index_text
             FROM json_each(?1) AS requested
             CROSS JOIN lcm_raw_messages AS message
             WHERE message.provider = json_extract(requested.value, '$.provider')
               AND message.message_id = json_extract(requested.value, '$.message_id')",
            stored_message_record_select_columns("message")
        );
        let mut rows = conn
            .query(&sql, params![requested.as_str()])
            .await
            .map_err(|error| storage("read projected messages", error))?;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| storage("read projected messages", error))?
        {
            let message = message_record_from_row(&row, 0)
                .map_err(|error| storage("decode projected messages", error.source))?;
            let decode = |index: i32| {
                row.get::<String>(index)
                    .map_err(|error| storage("decode projected message storage", error))
            };
            let columns = ProjectionStorageColumns {
                session_id: message.session_id.clone(),
                storage_kind: decode(13)?,
                content: decode(14)?,
                content_hash: decode(15)?,
                snippet_text: decode(16)?,
                index_text: decode(17)?,
            };
            let key = (message.provider.clone(), message.message_id.clone());
            storage_columns.insert(key.clone(), columns);
            messages.insert(key, message);
        }
    }

    let session_keys = messages
        .values()
        .map(|message| (message.provider.clone(), message.session_id.clone()))
        .collect::<BTreeSet<_>>();
    let mut sessions = HashMap::with_capacity(session_keys.len());
    let requested_keys = session_keys.iter().collect::<Vec<_>>();
    for chunk in requested_keys.chunks(OUTPUT_AUTHORITY_BATCH_KEYS) {
        let requested = serde_json::to_string(
            &chunk
                .iter()
                .map(|(provider, session_id)| {
                    serde_json::json!({ "provider": provider, "session_id": session_id })
                })
                .collect::<Vec<_>>(),
        )
        .map_err(|error| storage("encode projected session request", error))?;
        let mut rows = conn
            .query(
                "SELECT session.provider, session.session_id, session.project_key,
                        session.project_path, session.title, session.started_at,
                        session.ended_at, session.transcript_path, session.metadata_json,
                        session.parent_session_id, session.is_subagent, session.agent_id,
                        session.parent_tool_use_id
                 FROM json_each(?1) AS requested
                 CROSS JOIN sessions AS session
                 WHERE session.provider = json_extract(requested.value, '$.provider')
                   AND session.session_id = json_extract(requested.value, '$.session_id')",
                params![requested.as_str()],
            )
            .await
            .map_err(|error| storage("read projected sessions", error))?;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| storage("read projected sessions", error))?
        {
            let session = session_record_from_row(&row)
                .map_err(|error| storage("decode projected sessions", error.source))?;
            sessions.insert(
                (session.provider.clone(), session.session_id.clone()),
                session,
            );
        }
    }

    Ok(ProjectionRowsBatch {
        sessions,
        messages,
        storage_columns,
    })
}

/// The batched ownership resolution behind [`read_output_authorities`].
///
/// The grouped aggregate (`MAX(message_created)`, `COUNT(*)`) and the owner
/// selection both reuse [`output_owner_lookup_sql`], the one definition of the
/// projector's ordering, so the batched path cannot drift from the per-output
/// and whole-cache spellings. A requested key whose group is absent, whose
/// ownership aggregate is NULL or empty, or whose canonical owner has no
/// `observations` row simply yields no row: the caller maps that absence to
/// [`ProjectionStoreError::ProvenanceCollision`], exactly as the single-output
/// reads did.
fn output_authority_batch_sql() -> String {
    let newest_id = output_owner_lookup_sql("provenance.observation_id", "DESC");
    let oldest_id = output_owner_lookup_sql("provenance.observation_id", "ASC");
    format!(
        "WITH groups AS (
            SELECT provenance.projector_version AS projector_version,
                   provenance.output_provider AS output_provider,
                   provenance.output_message_id AS output_message_id,
                   MAX(provenance.message_created) AS projector_owned,
                   COUNT(*) AS owner_count
            FROM json_each(?2) AS requested
            CROSS JOIN observation_projection_provenance AS provenance
              INDEXED BY idx_observation_projection_provenance_output
            WHERE provenance.projector_version = ?1
              AND provenance.output_provider =
                    json_extract(requested.value, '$.provider')
              AND provenance.output_message_id =
                    json_extract(requested.value, '$.message_id')
            GROUP BY provenance.projector_version, provenance.output_provider,
                     provenance.output_message_id
         ),
         owners AS (
            SELECT groups.output_provider AS output_provider,
                   groups.output_message_id AS output_message_id,
                   CASE WHEN groups.projector_owned = 1 THEN {newest_id} ELSE {oldest_id} END
                     AS canonical_observation_id
            FROM groups
            WHERE groups.projector_owned IS NOT NULL AND groups.owner_count > 0
         )
         SELECT owners.output_provider, owners.output_message_id,
                observation.observation_id, observation.observation_json
         FROM owners
         JOIN observations AS observation
           ON observation.observation_id = owners.canonical_observation_id"
    )
}

/// Resolves the canonical projection owner for a whole set of outputs.
///
/// Keys are deduplicated by the caller's [`BTreeSet`], so one requested key
/// never multiplies a group's `COUNT(*)` through the `json_each` join.
#[cfg_attr(
    feature = "hotpath",
    hotpath::measure(label = "global_db.observation_state.batch.authority")
)]
pub(in super::super) async fn read_output_authorities(
    conn: &impl QueryExecutor,
    outputs: &BTreeSet<(String, String)>,
) -> ProjectionStoreResult<HashMap<(String, String), ProjectionOutputAuthority>> {
    let mut resolved = HashMap::with_capacity(outputs.len());
    if outputs.is_empty() {
        return Ok(resolved);
    }
    let sql = output_authority_batch_sql();
    let requested_keys = outputs.iter().collect::<Vec<_>>();
    for chunk in requested_keys.chunks(OUTPUT_AUTHORITY_BATCH_KEYS) {
        let requested = serde_json::to_string(
            &chunk
                .iter()
                .map(|(provider, message_id)| {
                    serde_json::json!({ "provider": provider, "message_id": message_id })
                })
                .collect::<Vec<_>>(),
        )
        .map_err(|error| storage("encode projection output authority request", error))?;
        let mut rows = conn
            .query(
                &sql,
                params![SESSION_MESSAGE_PROJECTOR_VERSION, requested.as_str()],
            )
            .await
            .map_err(|error| storage("read projection output authority", error))?;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| storage("read projection output authority", error))?
        {
            let provider = row
                .get::<String>(0)
                .map_err(|error| storage("read projection output authority", error))?;
            let message_id = row
                .get::<String>(1)
                .map_err(|error| storage("read projection output authority", error))?;
            let canonical_observation_id = row
                .get::<String>(2)
                .map_err(|error| storage("read canonical projection output authority", error))?;
            let observation_json = row
                .get::<String>(3)
                .map_err(|error| storage("read canonical projection output authority", error))?;
            let canonical = serde_json::from_str(&observation_json)
                .map_err(|error| storage("decode canonical projection output authority", error))?;
            resolved.insert(
                (provider, message_id),
                ProjectionOutputAuthority {
                    canonical_observation_id,
                    canonical,
                },
            );
        }
    }
    Ok(resolved)
}

/// Verifies one projected output against an already-resolved authority.
///
/// `derived` lets a caller that just derived some observation's projection hand
/// it back: when that observation *is* the canonical owner, the owner
/// projection is the same value [`message_projection`] would re-derive from the
/// same connection, so it is reused instead of re-queried. Any other owner
/// re-derives exactly as before.
#[cfg_attr(
    feature = "hotpath",
    hotpath::measure(label = "global_db.observation_state.verify.resolved_authority")
)]
pub(in super::super) async fn resolve_output_projection(
    conn: &impl QueryExecutor,
    authority: &ProjectionOutputAuthority,
    derived: Option<(&str, &ObservationProjection)>,
    projection: &SessionMessageProjection,
) -> ProjectionStoreResult<SessionMessageProjection> {
    let message = projection.message();
    let owner_projection = match derived {
        Some((observation_id, effect)) if observation_id == authority.canonical_observation_id => {
            effect
                .messages()
                .find(|candidate| {
                    candidate.message().provider == message.provider
                        && candidate.message().message_id == message.message_id
                })
                .cloned()
                .ok_or(ProjectionStoreError::ProvenanceCollision)?
        }
        _ => {
            message_projection(
                conn,
                &authority.canonical,
                &message.provider,
                &message.message_id,
            )
            .await?
        }
    };
    Ok(owner_projection)
}

/// Normalize projection rows through the same authority as runtime session writes
/// and project-scoped reads. Host/display spellings are not durable identity.
/// Reconciliation remains pure over the normalized stored strings.
pub(super) fn canonicalize_session_project_paths(session: &SessionRecord) -> SessionRecord {
    let canonical = durable_project_path_key(&session.project_path);
    let mut normalized = session.clone();
    if session.project_key == session.project_path {
        normalized.project_key.clone_from(&canonical);
    }
    normalized.project_path = canonical;
    normalized
}

/// Reconcile two stored session rows into one merged row using pure
/// string/shape logic only. Project-path family identity is resolved earlier,
/// at the apply-side ingest boundary ([`canonicalize_session_project_paths`]),
/// so this function, reached from the verify/audit and rebuild paths as well
/// as apply, never touches the filesystem and stays reproducible from stored
/// evidence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct SessionReconcileConflict(&'static str);

impl SessionReconcileConflict {
    pub(super) const fn field(self) -> &'static str {
        self.0
    }
}

pub(super) fn reconcile_session_rows_detailed(
    actual: &SessionRecord,
    expected: &SessionRecord,
) -> Result<SessionRecord, SessionReconcileConflict> {
    if actual.provider != expected.provider || actual.session_id != expected.session_id {
        return Err(SessionReconcileConflict("identity"));
    }
    let project_key = if actual.project_key == expected.project_key {
        actual.project_key.clone()
    } else if actual.project_key == "user" {
        expected.project_key.clone()
    } else if expected.project_key == "user" {
        actual.project_key.clone()
    } else if actual.project_key == actual.project_path
        && actual.project_path == expected.project_path
    {
        expected.project_key.clone()
    } else if expected.project_key == expected.project_path
        && expected.project_path == actual.project_path
    {
        actual.project_key.clone()
    } else {
        return Err(SessionReconcileConflict("project_key"));
    };
    let project_path = if actual.project_path == expected.project_path {
        actual.project_path.clone()
    } else if actual.project_path == actual.project_key {
        expected.project_path.clone()
    } else if expected.project_path == expected.project_key {
        actual.project_path.clone()
    } else {
        return Err(SessionReconcileConflict("project_path"));
    };
    Ok(SessionRecord {
        provider: actual.provider.clone(),
        session_id: actual.session_id.clone(),
        project_key,
        project_path,
        title: actual.title.clone().or_else(|| expected.title.clone()),
        started_at: actual
            .started_at
            .into_iter()
            .chain(expected.started_at)
            .min(),
        ended_at: actual.ended_at.into_iter().chain(expected.ended_at).max(),
        transcript_path: reconcile_optional(
            "transcript_path",
            actual.transcript_path.as_ref(),
            expected.transcript_path.as_ref(),
        )?,
        metadata_json: reconcile_metadata(
            actual.metadata_json.as_ref(),
            expected.metadata_json.as_ref(),
        )?,
        parent_session_id: reconcile_optional(
            "parent_session_id",
            actual.parent_session_id.as_ref(),
            expected.parent_session_id.as_ref(),
        )?,
        is_subagent: actual.is_subagent || expected.is_subagent,
        agent_id: reconcile_optional(
            "agent_id",
            actual.agent_id.as_ref(),
            expected.agent_id.as_ref(),
        )?,
        parent_tool_use_id: reconcile_optional(
            "parent_tool_use_id",
            actual.parent_tool_use_id.as_ref(),
            expected.parent_tool_use_id.as_ref(),
        )?,
    })
}

fn reconcile_optional<T: Clone + Eq>(
    field: &'static str,
    actual: Option<&T>,
    expected: Option<&T>,
) -> Result<Option<T>, SessionReconcileConflict> {
    match (actual, expected) {
        (Some(actual), Some(expected)) if actual != expected => {
            Err(SessionReconcileConflict(field))
        }
        (Some(actual), _) => Ok(Some(actual.clone())),
        (_, Some(expected)) => Ok(Some(expected.clone())),
        (None, None) => Ok(None),
    }
}

fn reconcile_metadata(
    actual: Option<&String>,
    expected: Option<&String>,
) -> Result<Option<String>, SessionReconcileConflict> {
    let (Some(actual), Some(expected)) = (actual, expected) else {
        return reconcile_optional("metadata_json", actual, expected);
    };
    if actual == expected {
        return Ok(Some(actual.clone()));
    }
    let mut actual: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(actual).map_err(|_| SessionReconcileConflict("metadata_json"))?;
    let expected: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(expected).map_err(|_| SessionReconcileConflict("metadata_json"))?;
    for (key, expected_value) in expected {
        match actual.get_mut(&key) {
            None => {
                actual.insert(key, expected_value);
            }
            Some(actual_value) if *actual_value == expected_value => {}
            Some(actual_value) if key == "usage" => {
                if let Some(merged) = reconcile_usage(actual_value, &expected_value) {
                    *actual_value = merged;
                }
            }
            // Each record contributes its own file-edit entries; the session
            // row keeps the union in first-seen order (re-applying a record
            // adds nothing).
            Some(serde_json::Value::Array(actual_files)) if key == EDITED_FILES_KEY => {
                if let serde_json::Value::Array(expected_files) = expected_value {
                    for file in expected_files {
                        if !actual_files.contains(&file) {
                            actual_files.push(file);
                        }
                    }
                }
            }
            // Host ingest keeps the first annotation (`merge_session_metadata`).
            // A later observation's source, cwd, or hook label is not a different
            // session. Session identity stays on provider and session id.
            Some(_) => {}
        }
    }
    serde_json::to_string(&actual)
        .map(Some)
        .map_err(|_| SessionReconcileConflict("metadata_json"))
}

fn reconcile_usage(
    actual: &serde_json::Value,
    expected: &serde_json::Value,
) -> Option<serde_json::Value> {
    if actual == expected {
        return Some(actual.clone());
    }
    match (actual, expected) {
        (serde_json::Value::Number(actual), serde_json::Value::Number(expected)) => Some(
            serde_json::Value::from(actual.as_u64()?.max(expected.as_u64()?)),
        ),
        (serde_json::Value::Object(actual), serde_json::Value::Object(expected)) => {
            let mut merged = actual.clone();
            for (key, expected_value) in expected {
                match merged.get_mut(key) {
                    None => {
                        merged.insert(key.clone(), expected_value.clone());
                    }
                    Some(actual_value) => {
                        *actual_value = reconcile_usage(actual_value, expected_value)?;
                    }
                }
            }
            Some(serde_json::Value::Object(merged))
        }
        _ => None,
    }
}

/// The message row the projector stores for `message`: its sanitized body
/// and protected metadata beside the projection's session columns.
pub(in super::super) fn projected_stored_message(
    message: &SessionMessageRecord,
) -> ProjectionStoreResult<SessionMessageRecord> {
    tracedecay_lcm::raw::projection_stored_message(message).map_err(|error| match error {
        LcmError::SanitizationRefused {
            reason,
            quarantined,
        } => ProjectionStoreError::SanitizationRefused {
            reason,
            quarantined,
        },
        error => storage("derive projected message row", error),
    })
}

/// Provenance digest of one projected output: the canonical output digest over
/// the row the projector stores, so a stored row is always digestible into the
/// provenance that pairs with it.
pub(in super::super) fn stored_output_digest(
    projection: &SessionMessageProjection,
) -> ProjectionStoreResult<PayloadDigestV1> {
    message_output_digest(
        projection.session(),
        &projected_stored_message(projection.message())?,
        projection.output_ordinal(),
    )
}

/// Whether `actual` is exactly the row the projector stores for `message`.
///
/// A Hermes body is written by the Hermes LCM turn authority rather than the
/// projector, so a Hermes row is compared on its session columns alone. A
/// body the sanitizer refuses has no projected row to match.
pub(in super::super) fn stored_row_matches(
    actual: &SessionMessageRecord,
    message: &SessionMessageRecord,
) -> ProjectionStoreResult<bool> {
    if message.provider == "hermes" {
        return Ok(same_session_columns(actual, message));
    }
    match projected_stored_message(message) {
        Ok(expected) => Ok(*actual == expected),
        Err(ProjectionStoreError::SanitizationRefused { .. }) => Ok(false),
        Err(error) => Err(error),
    }
}

fn same_session_columns(actual: &SessionMessageRecord, expected: &SessionMessageRecord) -> bool {
    actual.provider == expected.provider
        && actual.message_id == expected.message_id
        && actual.session_id == expected.session_id
        && actual.role == expected.role
        && actual.timestamp == expected.timestamp
        && actual.ordinal == expected.ordinal
        && actual.kind == expected.kind
        && actual.model == expected.model
        && actual.tool_names == expected.tool_names
        && actual.source_path == expected.source_path
        && actual.source_offset == expected.source_offset
}

/// Whether a row the transcript ingest wrote (externalized or with its own
/// protected metadata) is a protected rendering of `expected`.
pub(super) async fn protected_message_rows_compatible(
    conn: &impl QueryExecutor,
    actual: &SessionMessageRecord,
    expected: &SessionMessageRecord,
) -> ProjectionStoreResult<bool> {
    if stored_row_matches(actual, expected)? || !same_session_columns(actual, expected) {
        return Ok(false);
    }
    // A row that fails its own receipt is not a protected rendering of this
    // projection. Callers treat that as an ordinary output mismatch and, when
    // current provenance uniquely owns the output, rewrite it. A database
    // fault is still a fault.
    let raw =
        match tracedecay_lcm::schema::load_raw_message(conn, &actual.provider, &actual.message_id)
            .await
        {
            Ok(Some(raw)) => raw,
            Ok(None) | Err(LcmError::PayloadIntegrityMismatch) => return Ok(false),
            Err(error) => return Err(storage("read protected projection output", error)),
        };
    Ok(match raw.storage_kind {
        LcmStorageKind::Inline => tracedecay_privacy::sanitize_lcm_payload_text(&expected.text)
            .is_ok_and(|protected| raw.content == protected.sanitized_text()),
        LcmStorageKind::External => {
            raw.content_hash == projected_content_hash(&expected.text) && raw.payload_ref.is_some()
        }
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod reconcile_tests {
    use std::collections::BTreeSet;

    use crate::tests::harness::RegisteredGlobalDbHarness;
    use tracedecay_domain::{
        CanonicalObservationEnvelopeV1, ComponentVersion, ObservationId,
        ObservationIdentityMaterialV1, ObservationOrderingDomainV1, ObservationScopeV1,
        ObservationSourceGenerationV1, ObservationSourceIdentityV1, ObservationSourceRangeV1,
        PayloadReferenceV1, RetentionClass, SanitizationReceiptId, SanitizationReceiptRefV1,
        SanitizationReceiptV1, SanitizerDispositionV1, SensitivityV1,
    };
    #[cfg(unix)]
    use tracedecay_runtime_core::db::engine::params;
    use tracedecay_store::{
        ObservationProjection, ProjectionStoreError, SessionMessageRecord, SessionRecord,
    };

    use super::{
        canonicalize_session_project_paths, load_verified_session, read_projection_rows_batch,
        reconcile_session_rows_detailed, verify_projection_rows_from_records,
    };

    fn record(project_path: &str) -> SessionRecord {
        SessionRecord {
            provider: "codex".to_owned(),
            session_id: "session-family".to_owned(),
            project_key: project_path.to_owned(),
            project_path: project_path.to_owned(),
            title: None,
            started_at: Some(1),
            ended_at: Some(2),
            transcript_path: None,
            metadata_json: None,
            parent_session_id: None,
            is_subagent: false,
            agent_id: None,
            parent_tool_use_id: None,
        }
    }

    /// Each record's file-edit rollup joins the session row's array: the union
    /// keeps every distinct edit, a re-applied record adds nothing, and the
    /// other first-annotation-wins keys are untouched.
    #[test]
    fn edited_files_rollups_union_across_records() {
        let with_metadata = |metadata: serde_json::Value| SessionRecord {
            metadata_json: Some(metadata.to_string()),
            ..record("/work/project")
        };
        let first_edit = serde_json::json!({"path": "/work/a.rs", "edited_at_micros": 1_000});
        let second_edit = serde_json::json!({"path": "/work/b.rs", "edited_at_micros": 2_000});
        let later_a = serde_json::json!({"path": "/work/a.rs", "edited_at_micros": 3_000});
        let actual = with_metadata(serde_json::json!({
            "source": "claude_transcript",
            "edited_files": [first_edit.clone()]
        }));
        let expected = with_metadata(serde_json::json!({
            "source": "other",
            "edited_files": [second_edit.clone(), first_edit.clone(), later_a.clone()]
        }));

        let merged = reconcile_session_rows_detailed(&actual, &expected).unwrap();
        let metadata: serde_json::Value =
            serde_json::from_str(merged.metadata_json.as_deref().unwrap()).unwrap();
        assert_eq!(
            metadata["edited_files"],
            serde_json::json!([first_edit, second_edit, later_a]),
            "distinct edits of one path are separate events, duplicates collapse"
        );
        assert_eq!(metadata["source"], "claude_transcript");

        let again = reconcile_session_rows_detailed(&merged, &expected).unwrap();
        assert_eq!(again.metadata_json, merged.metadata_json);

        let no_edits = with_metadata(serde_json::json!({"source": "other"}));
        let merged = reconcile_session_rows_detailed(&no_edits, &actual).unwrap();
        let metadata: serde_json::Value =
            serde_json::from_str(merged.metadata_json.as_deref().unwrap()).unwrap();
        assert_eq!(metadata["edited_files"].as_array().unwrap().len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_family_roots_reconcile_after_ingest_normalization() {
        let tmp = tempfile::TempDir::new().unwrap();
        let real = tmp.path().join("fast-projects").join("repo");
        std::fs::create_dir_all(&real).unwrap();
        let alias_parent = tmp.path().join("home-projects");
        std::os::unix::fs::symlink(tmp.path().join("fast-projects"), &alias_parent).unwrap();
        let aliased = alias_parent.join("repo");

        // The apply-side ingest boundary resolves each family spelling to the
        // canonical on-disk form; reconciliation itself is pure string logic.
        let normalized_alias =
            canonicalize_session_project_paths(&record(&aliased.to_string_lossy()));
        let normalized_real = canonicalize_session_project_paths(&record(&real.to_string_lossy()));
        assert_eq!(normalized_alias.project_path, normalized_real.project_path);
        assert_eq!(normalized_alias.project_key, normalized_real.project_key);
        assert_ne!(
            normalized_alias.project_path,
            aliased.to_string_lossy(),
            "user symlink families must converge away from the alias spelling"
        );

        let merged = reconcile_session_rows_detailed(&normalized_alias, &normalized_real)
            .expect("normalized symlink families naming one directory must reconcile");
        assert_eq!(merged.project_path, normalized_real.project_path);
        assert_eq!(merged.project_key, normalized_real.project_key);

        // Symmetric: order must not change the merged identity.
        let merged_reversed =
            reconcile_session_rows_detailed(&normalized_real, &normalized_alias).unwrap();
        assert_eq!(merged_reversed.project_path, merged.project_path);

        // The audit path stays pure: two live family spellings that were never
        // normalized at ingest do not silently merge via filesystem probing.
        assert!(
            reconcile_session_rows_detailed(
                &record(&aliased.to_string_lossy()),
                &record(&real.to_string_lossy()),
            )
            .is_err(),
            "reconcile must not canonicalize; family identity is an ingest concern"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn existing_registered_session_alias_is_reconciled_and_persisted() {
        let tmp = tempfile::TempDir::new().unwrap();
        let real = tmp.path().join("project");
        std::fs::create_dir_all(&real).unwrap();
        let alias = tmp.path().join("project-alias");
        std::os::unix::fs::symlink(&real, &alias).unwrap();
        let harness = RegisteredGlobalDbHarness::open("existing-session-alias").await;
        let project_id = tracedecay_domain::ProjectId::new("project.fixture").unwrap();
        let mut original = record(&alias.to_string_lossy());
        original.project_key = project_id.as_str().to_owned();
        assert!(harness.registered.upsert_session(&original).await);
        // Model a persisted host spelling from the earlier projection writer.
        let transaction = harness.registered.begin_write_transaction().await.unwrap();
        transaction
            .execute(
                "UPDATE sessions SET project_path = ?1 WHERE provider = ?2 AND session_id = ?3",
                params![
                    original.project_path.as_str(),
                    original.provider.as_str(),
                    original.session_id.as_str()
                ],
            )
            .await
            .unwrap();
        transaction.commit().await.unwrap();

        let mut expected = original.clone();
        expected.project_path = real.to_string_lossy().into_owned();
        let transaction = harness.registered.begin_write_transaction().await.unwrap();
        super::super::apply::apply_session(&transaction, &expected)
            .await
            .unwrap();
        transaction.commit().await.unwrap();
        let persisted = harness
            .registered
            .get_session(&original.provider, &original.session_id)
            .await
            .unwrap();
        assert_eq!(persisted.project_key, project_id.as_str());
        assert_eq!(
            persisted.project_path,
            super::durable_project_path_key(&expected.project_path)
        );

        let other = tmp.path().join("other-project");
        std::fs::create_dir_all(&other).unwrap();
        expected.project_path = other.to_string_lossy().into_owned();
        let transaction = harness.registered.begin_write_transaction().await.unwrap();
        assert!(matches!(
            super::super::apply::apply_session(&transaction, &expected).await,
            Err(
                tracedecay_store::ProjectionStoreError::SessionOutputCollision {
                    field: "project_path",
                    ..
                }
            )
        ));
        transaction.rollback().await.unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_temp_firmlink_projection_matches_runtime_session_identity() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project = tmp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let host = project.to_string_lossy().into_owned();
        assert!(
            host.starts_with("/var/"),
            "expected macOS temp host spelling under /var, got {host}"
        );
        let normalized = canonicalize_session_project_paths(&record(&host));
        assert_eq!(
            normalized.project_path,
            tracedecay_sessions::runtime::shared::durable_project_path_key(&host),
            "projection and runtime session writes must use the same durable identity"
        );
        assert_eq!(normalized.project_key, normalized.project_path);
    }

    /// The Windows analogue of the firmlink case: `canonicalize` yields
    /// `\\?\D:\...`, which no host reports and no search key carries.
    #[cfg(windows)]
    #[test]
    fn windows_verbatim_spelling_is_not_written_into_project_paths() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project = tmp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let host = project.to_string_lossy().into_owned();
        assert!(
            !host.starts_with(r"\\?\"),
            "expected a plain host spelling from the temp root, got {host}"
        );
        let normalized = canonicalize_session_project_paths(&record(&host));
        assert!(
            !normalized.project_path.starts_with(r"\\?\"),
            "verbatim prefix leaked into the stored project path: {}",
            normalized.project_path
        );
        assert_eq!(normalized.project_key, normalized.project_path);

        let verbatim = format!(r"\\?\{host}");
        let from_verbatim = canonicalize_session_project_paths(&record(&verbatim));
        assert_eq!(
            from_verbatim.project_path, normalized.project_path,
            "both spellings of one directory must converge on the plain form"
        );
    }

    #[test]
    fn genuinely_different_roots_still_refuse_to_reconcile() {
        let tmp = tempfile::TempDir::new().unwrap();
        let first = tmp.path().join("first");
        let second = tmp.path().join("second");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        assert!(
            reconcile_session_rows_detailed(
                &record(&first.to_string_lossy()),
                &record(&second.to_string_lossy()),
            )
            .is_err(),
            "distinct directories must never merge"
        );
    }

    #[test]
    fn session_reconcile_conflict_names_the_field_without_exposing_its_value() {
        let mut actual = record("/project");
        actual.transcript_path = Some("/private/old-transcript.jsonl".to_owned());
        let mut expected = record("/project");
        expected.transcript_path = Some("/private/new-transcript.jsonl".to_owned());

        let conflict = reconcile_session_rows_detailed(&actual, &expected)
            .expect_err("different transcript identities must not merge");

        assert_eq!(conflict.field(), "transcript_path");
    }

    #[test]
    fn session_metadata_keeps_stored_annotations_and_merges_usage() {
        let mut stored = record("/project");
        stored.metadata_json = Some(
            r#"{"source":"cursor_transcript","cursor_session_cwd":"/project","cursor_session_worktree":"/project-wt","usage":{"input_tokens":1}}"#
                .to_owned(),
        );
        let mut projected = record("/project");
        projected.metadata_json = Some(
            r#"{"source":"cursor_composer","cursor_session_cwd":"/project","cursor_session_worktree":"/project","usage":{"input_tokens":4},"cursor_source":"cursor"}"#
                .to_owned(),
        );

        let merged = reconcile_session_rows_detailed(&stored, &projected)
            .expect("annotation disagreement must not be a session collision");
        let value: serde_json::Value =
            serde_json::from_str(merged.metadata_json.as_deref().unwrap()).unwrap();
        assert_eq!(value["source"], "cursor_transcript");
        assert_eq!(value["cursor_session_worktree"], "/project-wt");
        assert_eq!(value["cursor_source"], "cursor");
        assert_eq!(value["usage"]["input_tokens"], 4);
    }

    #[test]
    fn unmergeable_usage_keeps_the_stored_counter() {
        let mut stored = record("/project");
        stored.metadata_json = Some(r#"{"usage":{"input_tokens":1}}"#.to_owned());
        let mut projected = record("/project");
        projected.metadata_json = Some(r#"{"usage":"not-a-counter"}"#.to_owned());

        let merged = reconcile_session_rows_detailed(&stored, &projected)
            .expect("an unmergeable counter must not block the session");
        let value: serde_json::Value =
            serde_json::from_str(merged.metadata_json.as_deref().unwrap()).unwrap();
        assert_eq!(value["usage"]["input_tokens"], 1);
    }

    #[tokio::test]
    async fn missing_message_is_an_output_collision_not_a_missing_session() {
        let mut fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../tests/fixtures/provider_normalization/codex/agent_message.expected_envelope.json"
        ))
        .unwrap();
        fixture["stable_record_id"] =
            serde_json::Value::String("record.missing-message".to_owned());
        fixture["relations"]["session_id"] =
            serde_json::Value::String("session.missing-message".to_owned());
        fixture["relations"]["thread_id"] =
            serde_json::Value::String("session.missing-message".to_owned());
        fixture["relations"]["message_id"] =
            serde_json::Value::String("record.missing-message".to_owned());
        let envelope: CanonicalObservationEnvelopeV1 = serde_json::from_value(fixture).unwrap();
        let source = ObservationSourceIdentityV1::for_provider(
            envelope.provider().clone(),
            envelope.relations().session_id().clone(),
        )
        .unwrap();
        let payload = serde_json::to_value(&envelope).unwrap();
        let receipt = SanitizationReceiptV1::new(
            SanitizationReceiptRefV1::new(
                SanitizationReceiptId::new("receipt.missing-message").unwrap(),
                ComponentVersion::new("sanitizer.missing-message.v1").unwrap(),
            )
            .unwrap(),
            SanitizerDispositionV1::Accepted,
            SensitivityV1::NonSensitive,
            Some(PayloadReferenceV1::for_payload(&payload).unwrap()),
        )
        .unwrap();
        let observation = tracedecay_domain::DurableObservationV1::new(
            ObservationIdentityMaterialV1::for_native_record(
                source,
                ObservationScopeV1::Profile,
                ObservationSourceGenerationV1::new(1).unwrap(),
                ObservationSourceRangeV1::new(0, 100).unwrap(),
                ObservationOrderingDomainV1::FileBytes,
                ObservationId::new("record.missing-message").unwrap(),
            )
            .unwrap(),
            receipt,
            RetentionClass::new("retention.missing-message").unwrap(),
            payload,
        )
        .unwrap();
        let session = SessionRecord {
            provider: "codex".to_owned(),
            session_id: "session.missing-message".to_owned(),
            project_key: "user".to_owned(),
            project_path: "user".to_owned(),
            title: None,
            started_at: Some(1),
            ended_at: Some(2),
            transcript_path: None,
            metadata_json: None,
            parent_session_id: None,
            is_subagent: false,
            agent_id: None,
            parent_tool_use_id: None,
        };
        let message = SessionMessageRecord {
            provider: "codex".to_owned(),
            message_id: "record.missing-message".to_owned(),
            session_id: "session.missing-message".to_owned(),
            role: "assistant".to_owned(),
            timestamp: Some(1),
            ordinal: 0,
            text: "The billing pipeline regression is fixed.".to_owned(),
            kind: None,
            model: None,
            tool_names: None,
            source_path: None,
            source_offset: None,
            metadata_json: None,
        };
        let projection = ObservationProjection::for_message(&observation, session, message)
            .unwrap()
            .message()
            .expect("explicit message projection")
            .clone();
        let message = projection.message();
        let session = projection.session();
        assert_eq!(message.provider, "codex");
        assert_eq!(message.message_id, "record.missing-message");
        assert_eq!(session.session_id, "session.missing-message");
        let outputs = BTreeSet::from([(message.provider.clone(), message.message_id.clone())]);

        let harness = RegisteredGlobalDbHarness::open("missing-message-collision").await;
        let absent = harness.registered.read_snapshot().await.unwrap();
        let batch = read_projection_rows_batch(&absent, &outputs).await.unwrap();
        assert!(batch.message("codex", "record.missing-message").is_none());
        assert!(
            load_verified_session(&absent, &batch, "codex", "session.missing-message")
                .await
                .unwrap()
                .is_none()
        );
        let missing_session = verify_projection_rows_from_records(&absent, &projection, None, None)
            .await
            .expect_err("a projection with no stored session is a session collision");
        assert!(matches!(
            missing_session,
            ProjectionStoreError::SessionOutputCollision {
                field: "row_missing",
                ..
            }
        ));

        assert!(harness.registered.upsert_session(session).await);
        let present = harness.registered.read_snapshot().await.unwrap();
        let batch = read_projection_rows_batch(&present, &outputs)
            .await
            .unwrap();
        assert!(
            batch.session("codex", "session.missing-message").is_none(),
            "the message-keyed batch still does not see a session the message row never named"
        );
        let loaded = load_verified_session(&present, &batch, "codex", "session.missing-message")
            .await
            .unwrap()
            .expect("the durable session row is not missing");
        let missing_message = verify_projection_rows_from_records(
            &present,
            &projection,
            Some(loaded.as_ref()),
            batch.message("codex", "record.missing-message"),
        )
        .await
        .expect_err("a missing message with a live session is an output collision");
        match missing_message {
            ProjectionStoreError::OutputCollision {
                provider,
                message_id,
            } => {
                assert_eq!(provider, "codex");
                assert_eq!(message_id, "record.missing-message");
            }
            other => panic!("missing message classified as {other}"),
        }
    }
}
