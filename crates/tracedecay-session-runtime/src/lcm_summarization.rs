use std::time::Duration;

use serde_json::Value;
use tracedecay_domain::CanonicalObservationEnvelopeV1;

use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_lcm::{LcmError, LcmSummaryRequest, LcmSummarySourceRange};
use tracedecay_runtime_core::db::engine::{QueryExecutor, params};

mod cursor_agent;
mod provider_capabilities;

use provider_capabilities::{
    NativeSummaryCandidate, authoritative_summarizer, native_summary_recognizers,
};

pub(super) struct AuthoritativeSummary {
    pub(super) text: String,
    pub(super) route: String,
    pub(super) source_range: Option<LcmSummarySourceRange>,
}

pub(super) async fn resolve_authoritative_summary(
    database: &RegisteredGlobalDb,
    provider: &str,
    session_id: &str,
    request: LcmSummaryRequest,
    timeout: Duration,
    required_native_source_range: Option<&LcmSummarySourceRange>,
) -> Result<AuthoritativeSummary, SummaryResolutionError> {
    if let Some(summary) =
        native_summary_evidence(database, provider, session_id, Some(&request)).await?
        && required_native_source_range
            .is_none_or(|required| summary.source_range.as_ref() == Some(required))
    {
        return Ok(summary);
    }
    generate_provider_summary(provider, request, timeout).await
}

#[hotpath::measure(label = "daemon.lcm.summarize", future = true)]
async fn generate_provider_summary(
    provider: &str,
    request: LcmSummaryRequest,
    timeout: Duration,
) -> Result<AuthoritativeSummary, SummaryResolutionError> {
    let Some(summarizer) = authoritative_summarizer(provider) else {
        return Err(SummaryResolutionError::Unavailable(
            "authoritative_summarizer_unavailable",
        ));
    };
    summarizer.summarize(request, timeout).await
}

/// Scans a session's newest messages for evidence that the host itself already
/// produced an authoritative compaction summary.
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
    let mut rows = snapshot
        .query(
            "SELECT message.message_id, message.text, message.kind, message.metadata_json,
                    COALESCE(source_range.from_store_id, (
                        SELECT predecessor.store_id
                        FROM lcm_raw_messages AS predecessor
                        WHERE predecessor.provider = message.provider
                          AND predecessor.session_id = message.session_id
                          AND predecessor.store_id < raw.store_id
                        ORDER BY predecessor.store_id
                        LIMIT 1
                    )),
                    COALESCE(source_range.to_store_id, (
                        SELECT predecessor.store_id
                        FROM lcm_raw_messages AS predecessor
                        WHERE predecessor.provider = message.provider
                          AND predecessor.session_id = message.session_id
                          AND predecessor.store_id < raw.store_id
                        ORDER BY predecessor.store_id DESC
                        LIMIT 1
                    ))
             FROM session_messages AS message
             LEFT JOIN lcm_raw_messages AS raw
               ON raw.provider = message.provider
              AND raw.message_id = message.message_id
              AND raw.session_id = message.session_id
             LEFT JOIN lcm_raw_predecessor_ranges AS source_range
               ON source_range.provider = message.provider
              AND source_range.message_id = message.message_id
              AND source_range.session_id = message.session_id
             WHERE message.provider = ?1 AND message.session_id = ?2
               AND length(trim(message.text)) > 0
             ORDER BY message.ordinal DESC, message.message_id DESC
             LIMIT 512",
            params![provider, session_id],
        )
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
        ));
    }
    drop(rows);
    let recognizers = native_summary_recognizers(provider);
    for (message_id, text, kind, metadata_json, range_from, range_to) in candidates {
        let Some(metadata) = metadata_json
            .as_deref()
            .and_then(|metadata| serde_json::from_str::<Value>(metadata).ok())
        else {
            continue;
        };
        // Envelope decoding is best-effort rather than a gate: a provider that
        // records raw metadata instead of a canonical envelope still has to be
        // recognizable, so failure to decode leaves `envelope` empty and lets
        // the recognizers decide.
        let envelope =
            serde_json::from_value::<CanonicalObservationEnvelopeV1>(metadata.clone()).ok();
        let candidate = NativeSummaryCandidate {
            provider,
            message_id: &message_id,
            text: &text,
            kind: kind.as_deref(),
            metadata: &metadata,
            envelope: envelope.as_ref(),
        };
        let mut route = None;
        for recognizer in &recognizers {
            if recognizer.recognizes(&snapshot, &candidate).await? {
                route = Some(recognizer.route());
                break;
            }
        }
        if let Some(route) = route {
            let source_range = range_from.zip(range_to).map_or_else(
                || {
                    metadata
                        .get("tracedecay_lcm_source_range")
                        .cloned()
                        .and_then(|value| serde_json::from_value(value).ok())
                },
                |(from_store_id, to_store_id)| {
                    Some(LcmSummarySourceRange {
                        from_store_id,
                        to_store_id,
                    })
                },
            );
            if let Some(required_source) = required_source
                && !native_source_membership_is_exact(
                    &snapshot,
                    provider,
                    session_id,
                    source_range.as_ref(),
                    required_source,
                )
                .await?
            {
                continue;
            }
            return Ok(Some(AuthoritativeSummary {
                text,
                route: route.to_string(),
                source_range,
            }));
        }
    }
    Ok(None)
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
