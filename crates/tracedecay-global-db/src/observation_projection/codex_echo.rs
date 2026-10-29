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
use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, params};
use tracedecay_store::{
    ProjectionStoreResult, SESSION_MESSAGE_PROJECTOR_VERSION, SessionMessageRecord,
};

use super::apply::retire_replaced_codex_output;
use super::state::{read_observation, storage, storage_message};

const CODEX: &str = "codex";
const OPERATION: &str = "retire Codex response echoes";

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
