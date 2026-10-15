//! Codex records one conversational message twice in a rollout: a
//! `response_item` message, and an `event_msg` with the same text in the same
//! turn (`item_completed` `UserMessage`, or the legacy `user_message` and
//! `agent_message`). Capture keeps both records. Projection keeps the event,
//! which carries Codex's item identity, and retires the `response_item` row
//! directly before it in the session.

use serde::Deserialize;
use tracedecay_domain::{
    CanonicalObservationEnvelopeV1, CanonicalObservationIdV1, DurableObservationV1, ObservationId,
};
use tracedecay_runtime_core::db::Database;
use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, params};
use tracedecay_runtime_core::db::schema_version;
use tracedecay_store::{
    ProjectionStoreResult, SESSION_MESSAGE_PROJECTOR_VERSION, SessionMessageRecord,
};

use super::apply::retire_replaced_codex_output;
use super::state::{
    ensure_projection_output_state_cache, read_observation, storage, storage_message,
};

const CODEX: &str = "codex";
const OPERATION: &str = "retire Codex response echoes";
/// Recorded once the echoes a store projected earlier are retired.
const CONVERGENCE: &str = "codex-response-echo-retirement-v1";
const SESSION_PAGE: usize = 64;

struct CodexRecord {
    native_kind: String,
    turn: Option<ObservationId>,
}

fn codex_record(observation: &DurableObservationV1) -> Option<CodexRecord> {
    let envelope = CanonicalObservationEnvelopeV1::deserialize(observation.payload()).ok()?;
    Some(CodexRecord {
        native_kind: envelope.native_record_kind().to_owned(),
        turn: envelope.relations().turn_id().cloned(),
    })
}

/// Retires the `response_item` echo of `message` when `observation` is the
/// Codex event that recorded it.
pub(super) async fn retire_codex_response_echo(
    conn: &impl Executor,
    observation: &DurableObservationV1,
    message: &SessionMessageRecord,
) -> ProjectionStoreResult<()> {
    if message.provider != CODEX || message.kind.as_deref() != Some("message") {
        return Ok(());
    }
    let Some(event) = codex_record(observation) else {
        return Ok(());
    };
    retire_echo_of(conn, &event, &message.message_id).await
}

async fn retire_echo_of(
    conn: &impl Executor,
    event: &CodexRecord,
    message_id: &str,
) -> ProjectionStoreResult<()> {
    if event.native_kind != "event_msg" {
        return Ok(());
    }
    let Some(echo) = preceding_response_echo(conn, event, message_id).await? else {
        return Ok(());
    };
    retire_replaced_codex_output(conn, CODEX, &echo).await
}

/// The row directly before the event row `message_id` in its session, when it
/// is the same message (role, kind, content) recorded by a `response_item` of
/// the event's turn. Either record may predate turn ids.
async fn preceding_response_echo(
    conn: &impl QueryExecutor,
    event: &CodexRecord,
    message_id: &str,
) -> ProjectionStoreResult<Option<String>> {
    let mut rows = conn
        .query(
            "SELECT previous.message_id, provenance.observation_id
             FROM lcm_raw_messages AS current
             JOIN lcm_raw_messages AS previous ON previous.store_id = (
                 SELECT candidate.store_id FROM lcm_raw_messages AS candidate
                 WHERE candidate.provider = current.provider
                   AND candidate.session_id = current.session_id
                   AND candidate.source_path IS current.source_path
                   AND candidate.ordinal < current.ordinal
                 ORDER BY candidate.ordinal DESC
                 LIMIT 1
             )
             JOIN observation_projection_provenance AS provenance
               ON provenance.projector_version = ?3
              AND provenance.output_provider = previous.provider
              AND provenance.output_message_id = previous.message_id
             WHERE current.provider = ?1 AND current.message_id = ?2
               AND current.kind = 'message' AND previous.kind = 'message'
               AND previous.role = current.role
               AND previous.content_hash = current.content_hash",
            params![CODEX, message_id, SESSION_MESSAGE_PROJECTOR_VERSION],
        )
        .await
        .map_err(|error| storage(OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage(OPERATION, error))?
    else {
        return Ok(None);
    };
    let echo = row
        .get::<String>(0)
        .map_err(|error| storage(OPERATION, error))?;
    let owner = row
        .get::<String>(1)
        .map_err(|error| storage(OPERATION, error))?;
    drop(rows);
    let owner = CanonicalObservationIdV1::new(owner)
        .map_err(|_| storage_message(OPERATION, "invalid echo owner id"))?;
    let Some(response) = read_observation(conn, &owner)
        .await?
        .and_then(|(_, owner)| codex_record(&owner))
    else {
        return Ok(None);
    };
    let same_turn = match (&event.turn, &response.turn) {
        (Some(event), Some(response)) => event == response,
        _ => true,
    };
    Ok((response.native_kind == "response_item" && same_turn).then_some(echo))
}

/// Retires the echoes a store projected before projection retired them
/// itself, one page of Codex sessions per transaction, once per store.
pub(crate) async fn converge_codex_response_echoes(
    database: &Database,
) -> ProjectionStoreResult<()> {
    let mut after = String::new();
    loop {
        let transaction = database
            .begin_write_transaction(OPERATION)
            .await
            .map_err(|error| storage(OPERATION, error))?;
        let converged = !lcm_raw_messages_exist(&transaction).await?
            || schema_version::read_version(&transaction, CONVERGENCE)
                .await
                .map_err(|error| storage(OPERATION, error))?
                .is_some();
        if converged {
            return transaction
                .commit()
                .await
                .map_err(|error| storage(OPERATION, error));
        }
        ensure_projection_output_state_cache(&transaction).await?;
        let sessions = codex_session_page(&transaction, &after).await?;
        for session_id in &sessions {
            retire_session_echoes(&transaction, session_id).await?;
        }
        let last_page = sessions.len() < SESSION_PAGE;
        if last_page {
            schema_version::record_version(&transaction, CONVERGENCE, 1)
                .await
                .map_err(|error| storage(OPERATION, error))?;
        }
        transaction
            .commit()
            .await
            .map_err(|error| storage(OPERATION, error))?;
        match sessions.into_iter().last() {
            Some(last) if !last_page => after = last,
            _ => return Ok(()),
        }
    }
}

async fn lcm_raw_messages_exist(conn: &impl QueryExecutor) -> ProjectionStoreResult<bool> {
    let mut rows = conn
        .query(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'lcm_raw_messages'",
            (),
        )
        .await
        .map_err(|error| storage(OPERATION, error))?;
    Ok(rows
        .next()
        .await
        .map_err(|error| storage(OPERATION, error))?
        .is_some())
}

async fn codex_session_page(
    conn: &impl QueryExecutor,
    after: &str,
) -> ProjectionStoreResult<Vec<String>> {
    let mut rows = conn
        .query(
            "SELECT DISTINCT session_id FROM lcm_raw_messages
             WHERE provider = ?1 AND session_id > ?2
             ORDER BY session_id
             LIMIT ?3",
            params![CODEX, after, SESSION_PAGE as i64],
        )
        .await
        .map_err(|error| storage(OPERATION, error))?;
    let mut sessions = Vec::with_capacity(SESSION_PAGE);
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage(OPERATION, error))?
    {
        sessions.push(
            row.get::<String>(0)
                .map_err(|error| storage(OPERATION, error))?,
        );
    }
    Ok(sessions)
}

/// Every message row of `session_id` directly preceded by the same message is
/// a candidate; [`retire_echo_of`] keeps only an event preceded by its echo.
async fn retire_session_echoes(
    conn: &impl Executor,
    session_id: &str,
) -> ProjectionStoreResult<()> {
    let mut rows = conn
        .query(
            "SELECT paired.message_id, provenance.observation_id
             FROM (
                 SELECT provider, message_id, kind, role, content_hash,
                        LAG(kind) OVER sequence AS previous_kind,
                        LAG(role) OVER sequence AS previous_role,
                        LAG(content_hash) OVER sequence AS previous_hash
                 FROM lcm_raw_messages
                 WHERE provider = ?1 AND session_id = ?2
                 WINDOW sequence AS (PARTITION BY source_path ORDER BY ordinal)
             ) AS paired
             JOIN observation_projection_provenance AS provenance
               ON provenance.projector_version = ?3
              AND provenance.output_provider = paired.provider
              AND provenance.output_message_id = paired.message_id
             WHERE paired.kind = 'message' AND paired.previous_kind = 'message'
               AND paired.previous_role = paired.role
               AND paired.previous_hash = paired.content_hash",
            params![CODEX, session_id, SESSION_MESSAGE_PROJECTOR_VERSION],
        )
        .await
        .map_err(|error| storage(OPERATION, error))?;
    let mut candidates = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage(OPERATION, error))?
    {
        candidates.push((
            row.get::<String>(0)
                .map_err(|error| storage(OPERATION, error))?,
            row.get::<String>(1)
                .map_err(|error| storage(OPERATION, error))?,
        ));
    }
    drop(rows);
    for (message_id, owner) in candidates {
        let owner = CanonicalObservationIdV1::new(owner)
            .map_err(|_| storage_message(OPERATION, "invalid event owner id"))?;
        let Some(event) = read_observation(conn, &owner)
            .await?
            .and_then(|(_, owner)| codex_record(&owner))
        else {
            continue;
        };
        retire_echo_of(conn, &event, &message_id).await?;
    }
    Ok(())
}
