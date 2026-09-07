use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};

use tracedecay_domain::{CanonicalObservationIdV1, DurableObservationV1};
use tracedecay_store::{
    ObservationProjection, ProjectionCheckpoint, ProjectionStoreError, ProjectionStoreResult,
    SESSION_MESSAGE_PROJECTOR_VERSION, SESSION_MESSAGE_PROJECTOR_VERSION_V4,
    SessionMessageProjection, SessionMessageRecord, SessionRecord,
};

use tracedecay_lcm::retrieval_content::projected_content_hash;
use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, Row, params};
use tracedecay_runtime_core::path_safety::{canonicalize_existing_prefix, plain_host_path};

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

/// A projection retry deadline paces re-attempts within the store mount that
/// observed the failure. A fresh mount re-arms every queued projection for
/// immediate replay so restart recovery drains commit-before-ack work on its
/// first catch-up pass instead of waiting out a dead process's backoff.
/// Attempt counts and last errors persist, so a projection that fails again
/// resumes its escalating delay from the recorded attempt history.
pub(crate) async fn rearm_queued_projection_retries(
    conn: &impl Executor,
) -> ProjectionStoreResult<u64> {
    conn.execute(
        "UPDATE projection_queue SET next_retry_at_micros = 0 WHERE next_retry_at_micros > 0",
        params![],
    )
    .await
    .map_err(|error| storage("rearm queued projection retries", error))
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
    macro_rules! cell {
        ($index:literal) => {
            row.get($index)
                .map_err(|error| storage("decode projected session", error))?
        };
        ($index:literal, $ty:ty) => {
            row.get::<$ty>($index)
                .map_err(|error| storage("decode projected session", error))?
        };
    }
    Ok(Some(SessionRecord {
        provider: cell!(0),
        session_id: cell!(1),
        project_key: cell!(2),
        project_path: cell!(3),
        title: cell!(4),
        started_at: cell!(5),
        ended_at: cell!(6),
        transcript_path: cell!(7),
        metadata_json: cell!(8),
        parent_session_id: cell!(9),
        is_subagent: cell!(10, i64) != 0,
        agent_id: cell!(11),
        parent_tool_use_id: cell!(12),
    }))
}

pub(super) async fn read_message(
    conn: &impl QueryExecutor,
    provider: &str,
    message_id: &str,
) -> ProjectionStoreResult<Option<SessionMessageRecord>> {
    let mut rows = conn
        .query(
            "SELECT provider, message_id, session_id, role, timestamp, ordinal, text, kind,
                    model, tool_names, source_path, source_offset, metadata_json
             FROM session_messages WHERE provider = ?1 AND message_id = ?2",
            params![provider, message_id],
        )
        .await
        .map_err(|error| storage("read projected message", error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage("read projected message", error))?
    else {
        return Ok(None);
    };
    macro_rules! cell {
        ($index:literal) => {
            row.get($index)
                .map_err(|error| storage("decode projected message", error))?
        };
    }
    Ok(Some(SessionMessageRecord {
        provider: cell!(0),
        message_id: cell!(1),
        session_id: cell!(2),
        role: cell!(3),
        timestamp: cell!(4),
        ordinal: cell!(5),
        text: cell!(6),
        kind: cell!(7),
        model: cell!(8),
        tool_names: cell!(9),
        source_path: cell!(10),
        source_offset: cell!(11),
        metadata_json: cell!(12),
    }))
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
            data_version INTEGER NOT NULL CHECK(data_version >= 0)
        ) WITHOUT ROWID;",
    )
    .await
    .map_err(|error| storage("create projection output state cache", error))?;
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

    let mut rows = conn
        .query(
            "SELECT 1 FROM temp.observation_projection_output_state_meta
             WHERE initialized = 1 AND data_version = ?1",
            params![data_version],
        )
        .await
        .map_err(|error| storage("read projection output state cache", error))?;
    let initialized = rows
        .next()
        .await
        .map_err(|error| storage("read projection output state cache", error))?
        .is_some();
    drop(rows);
    if initialized {
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
        "INSERT INTO temp.observation_projection_output_state_meta(initialized, data_version)
         VALUES (1, ?1)",
        params![data_version],
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
    if !actual_session.is_some_and(|actual| session_rows_compatible(actual, &expected)) {
        return Err(ProjectionStoreError::OutputCollision {
            provider: session.provider.clone(),
            message_id: format!("session:{}", session.session_id),
        });
    }
    let message = projection.message();
    let compatible = match actual_message {
        Some(actual) => {
            actual == message || protected_message_rows_compatible(conn, actual, message).await?
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
    candidate.source() == owner.source() && candidate.scope() == owner.scope()
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

pub(in super::super) struct ProjectionRowsBatch {
    sessions: HashMap<(String, String), SessionRecord>,
    messages: HashMap<(String, String), SessionMessageRecord>,
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
}

pub(in super::super) async fn read_projection_rows_batch(
    conn: &impl QueryExecutor,
    outputs: &BTreeSet<(String, String)>,
) -> ProjectionStoreResult<ProjectionRowsBatch> {
    let mut messages = HashMap::with_capacity(outputs.len());
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
        let mut rows = conn
            .query(
                "SELECT message.provider, message.message_id, message.session_id,
                        message.role, message.timestamp, message.ordinal, message.text,
                        message.kind, message.model, message.tool_names, message.source_path,
                        message.source_offset, message.metadata_json
                 FROM json_each(?1) AS requested
                 CROSS JOIN session_messages AS message
                 WHERE message.provider = json_extract(requested.value, '$.provider')
                   AND message.message_id = json_extract(requested.value, '$.message_id')",
                params![requested.as_str()],
            )
            .await
            .map_err(|error| storage("read projected messages", error))?;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| storage("read projected messages", error))?
        {
            macro_rules! cell {
                ($index:literal) => {
                    row.get($index)
                        .map_err(|error| storage("decode projected messages", error))?
                };
            }
            let message = SessionMessageRecord {
                provider: cell!(0),
                message_id: cell!(1),
                session_id: cell!(2),
                role: cell!(3),
                timestamp: cell!(4),
                ordinal: cell!(5),
                text: cell!(6),
                kind: cell!(7),
                model: cell!(8),
                tool_names: cell!(9),
                source_path: cell!(10),
                source_offset: cell!(11),
                metadata_json: cell!(12),
            };
            messages.insert(
                (message.provider.clone(), message.message_id.clone()),
                message,
            );
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
            macro_rules! cell {
                ($index:literal) => {
                    row.get($index)
                        .map_err(|error| storage("decode projected sessions", error))?
                };
                ($index:literal, $ty:ty) => {
                    row.get::<$ty>($index)
                        .map_err(|error| storage("decode projected sessions", error))?
                };
            }
            let session = SessionRecord {
                provider: cell!(0),
                session_id: cell!(1),
                project_key: cell!(2),
                project_path: cell!(3),
                title: cell!(4),
                started_at: cell!(5),
                ended_at: cell!(6),
                transcript_path: cell!(7),
                metadata_json: cell!(8),
                parent_session_id: cell!(9),
                is_subagent: cell!(10, i64) != 0,
                agent_id: cell!(11),
                parent_tool_use_id: cell!(12),
            };
            sessions.insert(
                (session.provider.clone(), session.session_id.clone()),
                session,
            );
        }
    }

    Ok(ProjectionRowsBatch { sessions, messages })
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

pub(super) fn session_rows_compatible(actual: &SessionRecord, expected: &SessionRecord) -> bool {
    reconcile_session_rows(actual, expected).is_some()
}

/// Session project-path normalization boundary. Resolves a session row's
/// project path to its canonical on-disk form so that symlinked family roots
/// (e.g. `/home/zack/projects` vs `/fast/projects`) converge to a single
/// spelling before they are persisted or compared.
///
/// Called from [`apply_session`](super::apply) at ingest, and from verify/rebuild
/// on the re-derived expected row so host spellings match the persisted form.
/// [`reconcile_session_rows`] itself stays pure string/shape logic with no
/// filesystem access. macOS `/var` firmlink expansions are collapsed back to the
/// public `/var/...` spelling inside [`canonical_project_path`].
pub(super) fn canonicalize_session_project_paths(session: &SessionRecord) -> SessionRecord {
    let Some(canonical) = canonical_project_path(&session.project_path) else {
        return session.clone();
    };
    let mut normalized = session.clone();
    // A path-shaped key must track the canonical path so downstream pure
    // reconciliation keeps recognizing the two spellings as one family root.
    if session.project_key == session.project_path {
        normalized.project_key.clone_from(&canonical);
    }
    normalized.project_path = canonical;
    normalized
}

thread_local! {
    static CANONICAL_PROJECT_PATH_CACHE: RefCell<HashMap<String, Option<String>>> =
        RefCell::new(HashMap::new());
}

/// Resolve a project-path string to its canonical on-disk form, returning
/// `Some` only when the path exists and its canonical spelling differs. Non
/// paths and vanished paths yield `None`, so identity is only widened by
/// verifiable filesystem evidence.
///
/// Canonicalization is memoized per distinct `project_path` for the
/// current thread so a drain or rebuild transaction does not re-walk the
/// filesystem for the same family root on every session write.
///
/// On macOS, canonicalization expands firmlinks such as `/var` ->
/// `/private/var`. Prefer the stable public `/var/...` spelling (same policy as
/// [`tracedecay_sessions::runtime::git_correlation::normalize_worktree`]) so host-reported
/// temp/project roots are not rewritten into a form that breaks search keys and
/// authority verify against the original observation path. On Windows,
/// canonicalization returns the `\\?\D:\...` verbatim form; no host reports a
/// project root that way, so the same policy spells it plainly
/// ([`plain_host_path`]).
fn canonical_project_path(path: &str) -> Option<String> {
    CANONICAL_PROJECT_PATH_CACHE.with(|cache| {
        if let Some(cached) = cache.borrow().get(path) {
            return cached.clone();
        }
        let computed = compute_canonical_project_path(path);
        cache.borrow_mut().insert(path.to_owned(), computed.clone());
        computed
    })
}

fn compute_canonical_project_path(path: &str) -> Option<String> {
    let path_ref = std::path::Path::new(path);
    if !path_ref.exists() {
        return None;
    }
    let canonical = plain_host_path(&canonicalize_existing_prefix(path_ref)?);
    let mut canonical = canonical.to_string_lossy().into_owned();
    if let Some(stripped) = canonical.strip_prefix("/private/var/") {
        canonical = format!("/var/{stripped}");
    }
    (canonical != path).then_some(canonical)
}

/// Reconcile two stored session rows into one merged row using pure
/// string/shape logic only. Project-path family identity is resolved earlier,
/// at the apply-side ingest boundary ([`canonicalize_session_project_paths`]),
/// so this function — reached from the verify/audit and rebuild paths as well
/// as apply — never touches the filesystem and stays reproducible from stored
/// evidence.
pub(super) fn reconcile_session_rows(
    actual: &SessionRecord,
    expected: &SessionRecord,
) -> Option<SessionRecord> {
    if actual.provider != expected.provider || actual.session_id != expected.session_id {
        return None;
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
        return None;
    };
    let project_path = if actual.project_path == expected.project_path {
        actual.project_path.clone()
    } else if actual.project_path == actual.project_key {
        expected.project_path.clone()
    } else if expected.project_path == expected.project_key {
        actual.project_path.clone()
    } else {
        return None;
    };
    Some(SessionRecord {
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
            actual.transcript_path.as_ref(),
            expected.transcript_path.as_ref(),
        )
        .ok()?,
        metadata_json: reconcile_metadata(
            actual.metadata_json.as_ref(),
            expected.metadata_json.as_ref(),
        )
        .ok()?,
        parent_session_id: reconcile_optional(
            actual.parent_session_id.as_ref(),
            expected.parent_session_id.as_ref(),
        )
        .ok()?,
        is_subagent: actual.is_subagent || expected.is_subagent,
        agent_id: reconcile_optional(actual.agent_id.as_ref(), expected.agent_id.as_ref()).ok()?,
        parent_tool_use_id: reconcile_optional(
            actual.parent_tool_use_id.as_ref(),
            expected.parent_tool_use_id.as_ref(),
        )
        .ok()?,
    })
}

#[derive(Debug)]
struct ReconcileConflict;

fn reconcile_optional<T: Clone + Eq>(
    actual: Option<&T>,
    expected: Option<&T>,
) -> Result<Option<T>, ReconcileConflict> {
    match (actual, expected) {
        (Some(actual), Some(expected)) if actual != expected => Err(ReconcileConflict),
        (Some(actual), _) => Ok(Some(actual.clone())),
        (_, Some(expected)) => Ok(Some(expected.clone())),
        (None, None) => Ok(None),
    }
}

fn reconcile_metadata(
    actual: Option<&String>,
    expected: Option<&String>,
) -> Result<Option<String>, ReconcileConflict> {
    let (Some(actual), Some(expected)) = (actual, expected) else {
        return reconcile_optional(actual, expected);
    };
    if actual == expected {
        return Ok(Some(actual.clone()));
    }
    let mut actual: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(actual).map_err(|_| ReconcileConflict)?;
    let expected: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(expected).map_err(|_| ReconcileConflict)?;
    for (key, expected_value) in expected {
        match actual.get_mut(&key) {
            None => {
                actual.insert(key, expected_value);
            }
            Some(actual_value) if *actual_value == expected_value => {}
            Some(actual_value) if key == "usage" => {
                *actual_value =
                    reconcile_usage(actual_value, &expected_value).ok_or(ReconcileConflict)?;
            }
            Some(_) if key == "source" => {}
            Some(_) => return Err(ReconcileConflict),
        }
    }
    serde_json::to_string(&actual)
        .map(Some)
        .map_err(|_| ReconcileConflict)
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

pub(super) async fn protected_message_rows_compatible(
    conn: &impl QueryExecutor,
    actual: &SessionMessageRecord,
    expected: &SessionMessageRecord,
) -> ProjectionStoreResult<bool> {
    if actual.provider != expected.provider
        || actual.message_id != expected.message_id
        || actual.session_id != expected.session_id
        || actual.role != expected.role
        || actual.timestamp != expected.timestamp
        || actual.ordinal != expected.ordinal
        || actual.kind != expected.kind
        || actual.model != expected.model
        || actual.tool_names != expected.tool_names
        || actual.source_path != expected.source_path
        || actual.source_offset != expected.source_offset
    {
        return Ok(false);
    }
    let Some(metadata) = actual
        .metadata_json
        .as_deref()
        .and_then(|encoded| serde_json::from_str::<serde_json::Value>(encoded).ok())
    else {
        return Ok(false);
    };
    let Some(payload_ref) = metadata
        .get("payload_ref")
        .and_then(serde_json::Value::as_str)
    else {
        return Ok(false);
    };
    let expected_hash = projected_content_hash(&expected.text);
    if metadata
        .get("external_payload")
        .and_then(serde_json::Value::as_bool)
        != Some(true)
        || metadata.get("sha256").and_then(serde_json::Value::as_str)
            != Some(expected_hash.as_str())
        || !actual.text.contains(payload_ref)
    {
        return Ok(false);
    }
    let mut rows = conn
        .query(
            "SELECT CAST(COALESCE(content, '') AS TEXT),
                    CAST(COALESCE(content_hash, '') AS TEXT),
                    CAST(COALESCE(storage_kind, '') AS TEXT),
                    CAST(COALESCE(payload_ref, '') AS TEXT)
             FROM lcm_raw_messages
             WHERE provider = ?1 AND message_id = ?2
             LIMIT 2",
            params![actual.provider.as_str(), actual.message_id.as_str()],
        )
        .await
        .map_err(|error| storage("read protected projection output", error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage("read protected projection output", error))?
    else {
        return Ok(false);
    };
    let compatible = row
        .get::<String>(0)
        .map_err(|error| storage("read protected projection output", error))?
        .is_empty()
        && row
            .get::<String>(1)
            .map_err(|error| storage("read protected projection output", error))?
            == expected_hash
        && row
            .get::<String>(2)
            .map_err(|error| storage("read protected projection output", error))?
            == "external"
        && row
            .get::<String>(3)
            .map_err(|error| storage("read protected projection output", error))?
            == payload_ref;
    if rows
        .next()
        .await
        .map_err(|error| storage("read protected projection output", error))?
        .is_some()
    {
        return Ok(false);
    }
    Ok(compatible)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod reconcile_tests {
    use tracedecay_store::SessionRecord;

    use super::canonicalize_session_project_paths;
    use super::reconcile_session_rows;

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

        let merged = reconcile_session_rows(&normalized_alias, &normalized_real)
            .expect("normalized symlink families naming one directory must reconcile");
        assert_eq!(merged.project_path, normalized_real.project_path);
        assert_eq!(merged.project_key, normalized_real.project_key);

        // Symmetric: order must not change the merged identity.
        let merged_reversed = reconcile_session_rows(&normalized_real, &normalized_alias).unwrap();
        assert_eq!(merged_reversed.project_path, merged.project_path);

        // The audit path stays pure: two live family spellings that were never
        // normalized at ingest do not silently merge via filesystem probing.
        assert!(
            reconcile_session_rows(
                &record(&aliased.to_string_lossy()),
                &record(&real.to_string_lossy()),
            )
            .is_none(),
            "reconcile must not canonicalize; family identity is an ingest concern"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_temp_firmlink_spelling_is_preserved_at_ingest() {
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
            normalized.project_path, host,
            "macOS /var firmlink expansion must not rewrite host-facing project paths"
        );
        assert_eq!(normalized.project_key, host);
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

    /// Host-independent half of the same invariant: whatever `canonicalize`
    /// returns, the stored spelling never carries the verbatim prefix.
    #[test]
    fn stored_project_paths_never_carry_the_verbatim_prefix() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project = tmp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let normalized = canonicalize_session_project_paths(&record(&project.to_string_lossy()));
        assert!(!normalized.project_path.starts_with(r"\\?\"));
        assert!(!normalized.project_key.starts_with(r"\\?\"));
    }

    #[test]
    fn genuinely_different_roots_still_refuse_to_reconcile() {
        let tmp = tempfile::TempDir::new().unwrap();
        let first = tmp.path().join("first");
        let second = tmp.path().join("second");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        assert!(
            reconcile_session_rows(
                &record(&first.to_string_lossy()),
                &record(&second.to_string_lossy()),
            )
            .is_none(),
            "distinct directories must never merge"
        );
    }
}
