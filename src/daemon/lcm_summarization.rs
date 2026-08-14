use std::time::Duration;

use serde_json::Value;
use tracedecay_domain::CanonicalObservationEnvelopeV1;

use crate::db::engine::params;
use crate::global_db::RegisteredGlobalDb;
use tracedecay_sessions::runtime::lcm::{LcmError, LcmSummaryRequest};

mod provider_capabilities;

use provider_capabilities::{
    NativeSummaryCandidate, authoritative_summarizer, native_summary_recognizers,
};

pub(super) struct AuthoritativeSummary {
    pub(super) text: String,
    pub(super) route: String,
}

pub(super) async fn resolve_authoritative_summary(
    database: &RegisteredGlobalDb,
    provider: &str,
    session_id: &str,
    request: LcmSummaryRequest,
    timeout: Duration,
) -> Result<AuthoritativeSummary, SummaryResolutionError> {
    if let Some(summary) = native_summary_evidence(database, provider, session_id).await? {
        return Ok(summary);
    }
    generate_provider_summary(provider, request, timeout).await
}

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
pub(super) async fn native_summary_evidence(
    database: &RegisteredGlobalDb,
    provider: &str,
    session_id: &str,
) -> Result<Option<AuthoritativeSummary>, LcmError> {
    let snapshot = database
        .read_snapshot()
        .await
        .map_err(|error| LcmError::Db(error.to_string()))?;
    let mut rows = snapshot
        .query(
            "SELECT message_id, text, kind, metadata_json
             FROM session_messages
             WHERE provider = ?1 AND session_id = ?2
               AND length(trim(text)) > 0
             ORDER BY ordinal DESC, message_id DESC
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
        ));
    }
    drop(rows);
    let recognizers = native_summary_recognizers(provider);
    for (message_id, text, kind, metadata_json) in candidates {
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
            return Ok(Some(AuthoritativeSummary {
                text,
                route: route.to_string(),
            }));
        }
    }
    Ok(None)
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
