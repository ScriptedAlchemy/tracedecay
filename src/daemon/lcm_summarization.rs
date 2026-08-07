use std::time::Duration;

use serde_json::Value;
use tracedecay_domain::{CanonicalObservationEnvelopeV1, CanonicalObservationFactV1};

use crate::db::engine::{QueryExecutor, params};
use crate::global_db::RegisteredGlobalDb;
use crate::sessions::lcm::{LcmError, LcmSummaryRequest};

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
    match provider {
        "codex" => {
            let mut config =
                crate::sessions::codex_app_server::CodexAppServerSummaryConfig::from_env();
            config.timeout = config.timeout.min(timeout);
            let result = tokio::task::spawn_blocking(move || {
                crate::sessions::codex_app_server::summarize_with_codex_app_server(
                    &request, &config,
                )
            })
            .await
            .map_err(|_| SummaryResolutionError::Unavailable("codex_app_server_unavailable"))?
            .map_err(|_| SummaryResolutionError::Unavailable("codex_app_server_unavailable"))?;
            Ok(AuthoritativeSummary {
                text: result.text,
                route: result.model.map_or_else(
                    || "codex_app_server".to_string(),
                    |model| format!("codex_app_server:{model}"),
                ),
            })
        }
        _ => Err(SummaryResolutionError::Unavailable(
            "authoritative_summarizer_unavailable",
        )),
    }
}

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
    for (message_id, text, kind, metadata_json) in candidates {
        let Some(metadata) = metadata_json
            .as_deref()
            .and_then(|metadata| serde_json::from_str::<Value>(metadata).ok())
        else {
            continue;
        };
        if provider == "codex"
            && kind.as_deref() == Some("summary")
            && metadata.get("source").and_then(Value::as_str) == Some("codex_context_compacted")
            && metadata.get("summary_body").and_then(Value::as_str) == Some("plaintext")
        {
            return Ok(Some(AuthoritativeSummary {
                text,
                route: "codex_native_compaction".to_string(),
            }));
        }
        let Ok(envelope) =
            serde_json::from_value::<CanonicalObservationEnvelopeV1>(metadata.clone())
        else {
            continue;
        };
        let Some(native_summary) = envelope.facts().iter().find_map(|fact| match fact {
            CanonicalObservationFactV1::Compaction {
                summary: Some(summary),
                ..
            } => Some(summary),
            _ => None,
        }) else {
            continue;
        };
        if provider == "cursor" && native_summary.as_str() == Some(text.as_str()) {
            return Ok(Some(AuthoritativeSummary {
                text,
                route: "cursor_native_compaction".to_string(),
            }));
        }
        if provider == "claude"
            && native_summary
                .get("isCompactSummary")
                .and_then(Value::as_bool)
                == Some(true)
            && native_summary
                .get("isVisibleInTranscriptOnly")
                .and_then(Value::as_bool)
                == Some(true)
            && claude_summary_pair_is_exact(&snapshot, &envelope, &message_id).await?
        {
            return Ok(Some(AuthoritativeSummary {
                text,
                route: "claude_native_compaction".to_string(),
            }));
        }
    }
    Ok(None)
}

async fn claude_summary_pair_is_exact(
    snapshot: &impl QueryExecutor,
    summary: &CanonicalObservationEnvelopeV1,
    summary_message_id: &str,
) -> Result<bool, LcmError> {
    let Some(boundary_id) = summary.relations().parent_message_id() else {
        return Ok(false);
    };
    let Some(summary_id) = summary.relations().message_id() else {
        return Ok(false);
    };
    let session_id = summary.relations().session_id();
    if summary_id.as_str() != summary_message_id {
        return Ok(false);
    }
    let mut rows = snapshot
        .query(
            "SELECT metadata_json
             FROM session_messages
             WHERE provider = 'claude' AND message_id = ?1
               AND session_id = ?2
               AND kind IN ('compact_boundary', 'compaction')
             LIMIT 1",
            params![boundary_id.as_str(), session_id.as_str()],
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
    let metadata = row
        .get::<Option<String>>(0)
        .map_err(|error| LcmError::Db(error.to_string()))?;
    let Some(boundary) = metadata
        .as_deref()
        .and_then(|metadata| serde_json::from_str::<CanonicalObservationEnvelopeV1>(metadata).ok())
    else {
        return Ok(false);
    };
    let anchor = boundary.facts().iter().find_map(|fact| match fact {
        CanonicalObservationFactV1::Compaction {
            summary: Some(metadata),
            ..
        } => metadata
            .pointer("/preservedSegment/anchorUuid")
            .and_then(Value::as_str),
        _ => None,
    });
    Ok(anchor == Some(summary_id.as_str()))
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
