use std::time::Duration;

use serde_json::Value;
use tracedecay_domain::CanonicalObservationEnvelopeV1;
use tracedecay_domain::configuration::LcmSummarizerExecutablesV1;

use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_lcm::raw::{LcmPredecessorRangeState, predecessor_range_state};
use tracedecay_lcm::{LcmError, LcmSummaryRequest, LcmSummarySourceRange};
use tracedecay_runtime_core::db::{
    DatabaseEngineReadSnapshot,
    engine::{QueryExecutor, params},
};
use tracedecay_store::StoreShardScopeV1;

mod cursor_agent;
mod provider_capabilities;
// The summarizer fixtures are `#!/bin/sh` executables found through a
// `:`-joined PATH.
#[cfg(all(test, unix))]
mod summarizer_executable_tests;

#[cfg(test)]
use provider_capabilities::{CODEX_APP_SERVER_UNCONFIGURED, CURSOR_AGENT_UNCONFIGURED};
use provider_capabilities::{
    NativeSummaryCandidate, authoritative_summarizer, native_summary_recognizers,
};

/// Reason reported when a project shard has no published configuration pin,
/// so its summarizer binding cannot be read at all.
const SUMMARIZER_CONFIGURATION_UNAVAILABLE: &str = "summarizer_configuration_unavailable";

pub(super) struct AuthoritativeSummary {
    pub(super) text: String,
    pub(super) route: String,
    /// Provenance of the summarized interval. A summary whose interval is
    /// absent carries why, a session's genuinely-first message has no
    /// predecessor, an owed-but-missing interval is unavailable, so the
    /// caller can refuse instead of publishing provenance-free evidence.
    pub(super) source_range: LcmPredecessorRangeState,
}

/// Borrows the pending response's summary request: native-evidence hits and
/// providers without a summarizer never copy the source messages, and the
/// caller keeps the pending response intact for the unavailable result.
pub(super) async fn resolve_authoritative_summary(
    database: &RegisteredGlobalDb,
    provider: &str,
    session_id: &str,
    request: &LcmSummaryRequest,
    timeout: Duration,
    required_native_source_range: Option<&LcmSummarySourceRange>,
) -> Result<AuthoritativeSummary, SummaryResolutionError> {
    if let Some(summary) =
        native_summary_evidence(database, provider, session_id, Some(request)).await?
        && required_native_source_range
            .is_none_or(|required| summary.source_range.interval() == Some(required))
    {
        return Ok(summary);
    }
    generate_provider_summary(database, provider, request, timeout).await
}

#[hotpath::measure(label = "daemon.lcm.summarize", future = true)]
async fn generate_provider_summary(
    database: &RegisteredGlobalDb,
    provider: &str,
    request: &LcmSummaryRequest,
    timeout: Duration,
) -> Result<AuthoritativeSummary, SummaryResolutionError> {
    let Some(summarizer) = authoritative_summarizer(provider) else {
        return Err(SummaryResolutionError::Unavailable(
            "authoritative_summarizer_unavailable",
        ));
    };
    // The binding is read before the summarizer runs, so an unconfigured or
    // unreadable setting is a typed pending reason and never a spawn.
    let executables = summarizer_executables(database)?;
    // Provider summarizers run on a blocking thread and need an owned request.
    summarizer
        .summarize(request.clone(), timeout, &executables)
        .await
}

/// The summarizer executables configured for the shard `database` serves.
///
/// Project shards read the daemon-published pin for their registered project.
/// Profile-wide shards have no project configuration authority, so every
/// provider is unconfigured there and their sessions stay pending.
fn summarizer_executables(
    database: &RegisteredGlobalDb,
) -> Result<LcmSummarizerExecutablesV1, SummaryResolutionError> {
    match &database.binding().shard_id.scope {
        StoreShardScopeV1::Project { project_id }
        | StoreShardScopeV1::ProjectSessions { project_id }
        | StoreShardScopeV1::Code { project_id, .. } => {
            tracedecay_configuration::lcm_summarizer_executables_for_project(project_id).map_err(
                |error| {
                    tracing::debug!(
                        project_id = project_id.as_str(),
                        %error,
                        "LCM summarizer binding is unavailable for this project shard"
                    );
                    SummaryResolutionError::Unavailable(SUMMARIZER_CONFIGURATION_UNAVAILABLE)
                },
            )
        }
        StoreShardScopeV1::Profile
        | StoreShardScopeV1::ProfileMemory
        | StoreShardScopeV1::ProfileSessions
        | StoreShardScopeV1::RemoteNode { .. } => Ok(LcmSummarizerExecutablesV1::unconfigured()),
    }
}

/// The summarizer binding retained convergence currently runs under for this
/// shard, as the durable identity parked sessions settle against.
///
/// An unpublished pin is a binding of its own, so its publication counts as a
/// change exactly like configuring an executable does.
pub(super) fn summarizer_binding_identity(
    database: &RegisteredGlobalDb,
) -> Result<String, LcmError> {
    match summarizer_executables(database) {
        Ok(executables) => serde_json::to_string(&executables)
            .map_err(|error| LcmError::Db(format!("encode LCM summarizer binding: {error}"))),
        Err(SummaryResolutionError::Unavailable(reason)) => Ok(reason.to_owned()),
        Err(SummaryResolutionError::Storage(error)) => Err(error),
    }
}

/// Finds evidence that the host itself already produced an authoritative
/// compaction summary. Required retained pages use the persisted predecessor
/// range for an exact indexed lookup; an unbound status read inspects only the
/// newest bounded candidate window.
///
/// The scan is provider-neutral: it decodes each row once and offers it to the
/// recognizers registered for this provider, which own every provider-specific
/// recognition rule, corroboration query, and route label.
#[hotpath::measure(label = "daemon.lcm.evidence", future = true)]
pub(super) async fn native_summary_evidence(
    database: &RegisteredGlobalDb,
    provider: &str,
    session_id: &str,
    required_source: Option<&LcmSummaryRequest>,
) -> Result<Option<AuthoritativeSummary>, LcmError> {
    let snapshot = database
        .read_snapshot()
        .await
        .map_err(|error| LcmError::Db(error.to_string()))?;
    let (candidate_sql, candidate_params) = if let Some(required) = required_source {
        (
            format!(
                "SELECT message.message_id, COALESCE(message.content, message.placeholder_text, ''), message.kind,
                    message.metadata_json, source_range.from_store_id, source_range.to_store_id,
                    message.store_id, {MESSAGE_ENVELOPE_COLUMN}
             FROM lcm_raw_predecessor_ranges AS source_range
             JOIN lcm_raw_messages AS message
               ON message.provider = source_range.provider
              AND message.message_id = source_range.message_id
              AND message.session_id = source_range.session_id
             WHERE source_range.provider = ?1 AND source_range.session_id = ?2
               AND source_range.to_store_id = ?3
               AND length(trim(COALESCE(message.content, message.placeholder_text, ''))) > 0
             ORDER BY message.store_id, message.message_id
             LIMIT 2"
            ),
            params![provider, session_id, required.source_range.to_store_id,],
        )
    } else {
        (
            format!(
                "SELECT message.message_id, COALESCE(message.content, message.placeholder_text, ''), message.kind,
                    message.metadata_json, source_range.from_store_id, source_range.to_store_id,
                    message.store_id, {MESSAGE_ENVELOPE_COLUMN}
             FROM lcm_raw_messages AS message
             LEFT JOIN lcm_raw_predecessor_ranges AS source_range
               ON source_range.provider = message.provider
              AND source_range.message_id = message.message_id
              AND source_range.session_id = message.session_id
             WHERE message.provider = ?1 AND message.session_id = ?2
               AND length(trim(COALESCE(message.content, message.placeholder_text, ''))) > 0
             ORDER BY message.ordinal DESC, message.message_id DESC
             LIMIT 512"
            ),
            params![provider, session_id],
        )
    };
    let mut rows = snapshot
        .query(&candidate_sql, candidate_params)
        .await
        .map_err(|error| LcmError::Db(error.to_string()))?;
    let mut candidates = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| LcmError::Db(error.to_string()))?
    {
        candidates.push((
            row.get::<String>(0)
                .map_err(|error| LcmError::Db(error.to_string()))?,
            row.get::<String>(1)
                .map_err(|error| LcmError::Db(error.to_string()))?,
            row.get::<Option<String>>(2)
                .map_err(|error| LcmError::Db(error.to_string()))?,
            row.get::<Option<String>>(3)
                .map_err(|error| LcmError::Db(error.to_string()))?,
            row.get::<Option<i64>>(4)
                .map_err(|error| LcmError::Db(error.to_string()))?,
            row.get::<Option<i64>>(5)
                .map_err(|error| LcmError::Db(error.to_string()))?,
            row.get::<Option<i64>>(6)
                .map_err(|error| LcmError::Db(error.to_string()))?,
            row.get::<Option<String>>(7)
                .map_err(|error| LcmError::Db(error.to_string()))?,
        ));
    }
    drop(rows);
    let recognizers = native_summary_recognizers(provider);
    let mut previous_native_store_id = None;
    let mut matched = None;
    for (message_id, text, kind, metadata_json, range_from, range_to, store_id, envelope_json) in
        candidates.into_iter().rev()
    {
        let metadata = parse_message_metadata(metadata_json.as_deref());
        let envelope = decode_message_envelope(envelope_json.as_deref())?;
        let candidate = NativeSummaryCandidate {
            provider,
            message_id: &message_id,
            text: &text,
            kind: kind.as_deref(),
            metadata: &metadata,
            envelope: envelope.as_deref(),
        };
        let mut route = None;
        for recognizer in &recognizers {
            if recognizer.recognizes(&snapshot, &candidate).await? {
                route = Some(recognizer.route());
                break;
            }
        }
        if let Some(route) = route {
            let explicit_range = metadata
                .get("tracedecay_lcm_source_range")
                .cloned()
                .and_then(|value| serde_json::from_value(value).ok());
            let source_range = if let Some(required) = required_source {
                let from_store_id = required.source_range.from_store_id;
                let starts_at_first_raw = range_from == Some(from_store_id);
                let starts_at_native_summary = if starts_at_first_raw {
                    true
                } else {
                    native_store_is_recognized(&snapshot, provider, session_id, from_store_id)
                        .await?
                };
                // A bound page reaches this row through its persisted range,
                // so an interval that does not bind the required one is not a
                // missing range: it is evidence that does not cover the page.
                explicit_range
                    .or_else(|| starts_at_native_summary.then(|| required.source_range.clone()))
                    .map_or(LcmPredecessorRangeState::Unavailable, |interval| {
                        LcmPredecessorRangeState::Interval(interval)
                    })
            } else if let Some(interval) = explicit_range {
                LcmPredecessorRangeState::Interval(interval)
            } else if let (Some(from_store_id), Some(to_store_id)) =
                (previous_native_store_id.or(range_from), range_to)
            {
                LcmPredecessorRangeState::Interval(LcmSummarySourceRange {
                    from_store_id,
                    to_store_id,
                })
            } else if let Some(store_id) = store_id {
                // No persisted interval: ask the range authority whether this
                // row is its session's first conversational message or is
                // owed an interval it does not have.
                predecessor_range_state(&snapshot, provider, session_id, store_id).await?
            } else {
                // The recognized row is not in the raw authority at all, so
                // no interval can be derived for it.
                LcmPredecessorRangeState::Unavailable
            };
            previous_native_store_id = store_id.or(previous_native_store_id);
            if let Some(required_source) = required_source
                && !native_source_membership_is_exact(
                    &snapshot,
                    provider,
                    session_id,
                    source_range.interval(),
                    required_source,
                )
                .await?
            {
                continue;
            }
            matched = Some(AuthoritativeSummary {
                text,
                route: route.to_string(),
                source_range,
            });
        }
    }
    Ok(matched)
}

async fn native_store_is_recognized(
    snapshot: &DatabaseEngineReadSnapshot,
    provider: &str,
    session_id: &str,
    store_id: i64,
) -> Result<bool, LcmError> {
    let mut rows = snapshot
        .query(
            &format!(
                "SELECT message.message_id, COALESCE(message.content, message.placeholder_text, ''), message.kind,
                        message.metadata_json, {MESSAGE_ENVELOPE_COLUMN}
             FROM lcm_raw_messages AS message
             WHERE message.provider = ?1 AND message.session_id = ?2
               AND message.store_id = ?3
             LIMIT 1"
            ),
            params![provider, session_id, store_id],
        )
        .await
        .map_err(|error| LcmError::Db(error.to_string()))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| LcmError::Db(error.to_string()))?
    else {
        return Ok(false);
    };
    let message_id = row
        .get::<String>(0)
        .map_err(|error| LcmError::Db(error.to_string()))?;
    let text = row
        .get::<String>(1)
        .map_err(|error| LcmError::Db(error.to_string()))?;
    let kind = row
        .get::<Option<String>>(2)
        .map_err(|error| LcmError::Db(error.to_string()))?;
    let metadata = parse_message_metadata(
        row.get::<Option<String>>(3)
            .map_err(|error| LcmError::Db(error.to_string()))?
            .as_deref(),
    );
    let envelope = decode_message_envelope(
        row.get::<Option<String>>(4)
            .map_err(|error| LcmError::Db(error.to_string()))?
            .as_deref(),
    )?;
    drop(rows);
    let candidate = NativeSummaryCandidate {
        provider,
        message_id: &message_id,
        text: &text,
        kind: kind.as_deref(),
        metadata: &metadata,
        envelope: envelope.as_deref(),
    };
    for recognizer in native_summary_recognizers(provider) {
        if recognizer.recognizes(snapshot, &candidate).await? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The canonical envelope of the newest observation projected into the
/// `message` row. Message metadata does not embed it: the observation row is
/// its only copy. Rows no observation projected (direct transcript ingest)
/// select NULL.
pub(super) const MESSAGE_ENVELOPE_COLUMN: &str =
    "(SELECT json_extract(observation.observation_json, '$.payload')
      FROM observation_projection_provenance AS provenance
      JOIN observations AS observation
        ON observation.observation_id = provenance.observation_id
      WHERE provenance.output_provider = message.provider
        AND provenance.output_message_id = message.message_id
      ORDER BY observation.sequence DESC
      LIMIT 1)";

/// Stored message metadata, or `Null` when the row has none or it is not JSON.
fn parse_message_metadata(metadata: Option<&str>) -> Value {
    metadata
        .and_then(|metadata| serde_json::from_str(metadata).ok())
        .unwrap_or(Value::Null)
}

/// A projected row's observation payload is a validated canonical envelope, so
/// one that does not decode is corruption, never an unrecognized row.
pub(super) fn decode_message_envelope(
    envelope: Option<&str>,
) -> Result<Option<Box<CanonicalObservationEnvelopeV1>>, LcmError> {
    envelope
        .map(|envelope| {
            serde_json::from_str(envelope)
                .map(Box::new)
                .map_err(|error| {
                    LcmError::Db(format!(
                        "message observation envelope decode failed: {error}"
                    ))
                })
        })
        .transpose()
}

async fn native_source_membership_is_exact(
    snapshot: &impl QueryExecutor,
    provider: &str,
    session_id: &str,
    native_range: Option<&LcmSummarySourceRange>,
    required: &LcmSummaryRequest,
) -> Result<bool, LcmError> {
    if native_range != Some(&required.source_range) || required.source_messages.is_empty() {
        return Ok(false);
    }
    let limit = i64::try_from(required.source_messages.len().saturating_add(1))
        .map_err(|_| LcmError::Db("native source membership limit overflow".to_string()))?;
    let mut rows = snapshot
        .query(
            "SELECT store_id
             FROM lcm_raw_messages
             WHERE provider = ?1 AND session_id = ?2
               AND store_id BETWEEN ?3 AND ?4
             ORDER BY store_id
             LIMIT ?5",
            params![
                provider,
                session_id,
                required.source_range.from_store_id,
                required.source_range.to_store_id,
                limit,
            ],
        )
        .await
        .map_err(|error| LcmError::Db(error.to_string()))?;
    let mut actual = Vec::with_capacity(required.source_messages.len().saturating_add(1));
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| LcmError::Db(error.to_string()))?
    {
        actual.push(
            row.get::<i64>(0)
                .map_err(|error| LcmError::Db(error.to_string()))?,
        );
    }
    Ok(actual
        == required
            .source_messages
            .iter()
            .map(|message| message.store_id)
            .collect::<Vec<_>>())
}

pub(super) enum SummaryResolutionError {
    Storage(LcmError),
    Unavailable(&'static str),
}

impl From<LcmError> for SummaryResolutionError {
    fn from(error: LcmError) -> Self {
        Self::Storage(error)
    }
}

#[cfg(test)]
mod decode_message_envelope_tests {
    use super::decode_message_envelope;

    #[test]
    fn corrupt_observation_envelope_is_typed() {
        let error = decode_message_envelope(Some(r#"{"not": "an envelope"}"#))
            .expect_err("a corrupt observation payload must not read as unrecognized");
        assert!(
            error
                .to_string()
                .contains("message observation envelope decode failed"),
            "typed envelope failure: {error}"
        );
    }

    #[test]
    fn unprojected_row_has_no_envelope() {
        assert!(decode_message_envelope(None).unwrap().is_none());
    }
}
