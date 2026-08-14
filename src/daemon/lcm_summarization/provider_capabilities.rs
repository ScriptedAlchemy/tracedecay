//! Provider-keyed capabilities behind authoritative LCM summarization.
//!
//! Every place a host differs — how its own native compaction summary is
//! recognized, what corroborating evidence that recognition needs, the route
//! label the recognition records, and whether the host can be asked to produce
//! a summary on demand — is registered here as one capability implementation.
//! The scan in the parent module stays provider-neutral: it reads rows, hands
//! each one to the registered recognizers, and never names a provider.

use std::pin::Pin;
use std::time::Duration;

use serde_json::Value;
use tracedecay_domain::{CanonicalObservationEnvelopeV1, CanonicalObservationFactV1};

use crate::db::engine::{QueryExecutor, ReadSnapshot, params};
use tracedecay_sessions::runtime::lcm::{LcmError, LcmSummaryRequest};

use super::{AuthoritativeSummary, SummaryResolutionError};

/// One `session_messages` row offered to the recognizers.
///
/// Both views of the row are carried because providers disagree about what
/// their native summary looks like on disk: Codex records raw provider
/// metadata that never decodes as a canonical envelope, while Cursor and
/// Claude record canonical envelopes. `envelope` is therefore `None` on decode
/// failure rather than a reason to skip the row.
pub(super) struct NativeSummaryCandidate<'a> {
    pub(super) provider: &'a str,
    pub(super) message_id: &'a str,
    pub(super) text: &'a str,
    pub(super) kind: Option<&'a str>,
    pub(super) metadata: &'a Value,
    pub(super) envelope: Option<&'a CanonicalObservationEnvelopeV1>,
}

impl<'a> NativeSummaryCandidate<'a> {
    /// The payload of this row's `Compaction` fact, when the row decoded as a
    /// canonical envelope that carries one.
    fn compaction_summary(&self) -> Option<&'a Value> {
        self.envelope?.facts().iter().find_map(|fact| match fact {
            CanonicalObservationFactV1::Compaction {
                summary: Some(summary),
                ..
            } => Some(summary),
            _ => None,
        })
    }
}

type NativeSummaryRecognitionFuture<'a> =
    Pin<Box<dyn Future<Output = Result<bool, LcmError>> + Send + 'a>>;

/// One provider's rule for accepting a row as its own authoritative summary.
pub(super) trait NativeSummaryRecognizerV1: Sync {
    /// The route label recorded when this recognizer accepts a row.
    fn route(&self) -> &'static str;

    /// Whether this row is the provider's native compaction summary, including
    /// whatever corroborating lookup that provider's evidence demands.
    fn recognizes<'a>(
        &self,
        snapshot: &'a ReadSnapshot,
        candidate: &'a NativeSummaryCandidate<'a>,
    ) -> NativeSummaryRecognitionFuture<'a>;
}

/// Codex publishes its compaction summary as raw provider metadata, so this
/// recognizer reads `candidate.metadata` and ignores the envelope entirely.
/// The `summary_body` discriminator is what separates a usable plaintext
/// summary from an encrypted body that carries no recoverable text.
struct CodexNativeCompactionV1;

impl NativeSummaryRecognizerV1 for CodexNativeCompactionV1 {
    fn route(&self) -> &'static str {
        "codex_native_compaction"
    }

    fn recognizes<'a>(
        &self,
        _snapshot: &'a ReadSnapshot,
        candidate: &'a NativeSummaryCandidate<'a>,
    ) -> NativeSummaryRecognitionFuture<'a> {
        let recognized = candidate.kind == Some("summary")
            && candidate.metadata.get("source").and_then(Value::as_str)
                == Some("codex_context_compacted")
            && candidate.metadata.get("summary_body").and_then(Value::as_str) == Some("plaintext");
        Box::pin(async move { Ok(recognized) })
    }
}

/// Cursor stores the compacted text twice — once as the message body and once
/// as the compaction fact's summary. The row is authoritative only when the
/// two agree exactly, which is what proves the body was not truncated or
/// re-rendered on the way into the store.
struct CursorNativeCompactionV1;

impl NativeSummaryRecognizerV1 for CursorNativeCompactionV1 {
    fn route(&self) -> &'static str {
        "cursor_native_compaction"
    }

    fn recognizes<'a>(
        &self,
        _snapshot: &'a ReadSnapshot,
        candidate: &'a NativeSummaryCandidate<'a>,
    ) -> NativeSummaryRecognitionFuture<'a> {
        let recognized = candidate
            .compaction_summary()
            .and_then(Value::as_str)
            .is_some_and(|summary| summary == candidate.text);
        Box::pin(async move { Ok(recognized) })
    }
}

/// Claude's compaction summary flags itself in the compaction fact, but the
/// flags alone are not proof: the boundary record that produced the summary
/// must point back at this exact message. That pairing check is this
/// recognizer's corroboration query.
struct ClaudeNativeCompactionV1;

impl NativeSummaryRecognizerV1 for ClaudeNativeCompactionV1 {
    fn route(&self) -> &'static str {
        "claude_native_compaction"
    }

    fn recognizes<'a>(
        &self,
        snapshot: &'a ReadSnapshot,
        candidate: &'a NativeSummaryCandidate<'a>,
    ) -> NativeSummaryRecognitionFuture<'a> {
        Box::pin(async move {
            let Some(envelope) = candidate.envelope else {
                return Ok(false);
            };
            let Some(summary) = candidate.compaction_summary() else {
                return Ok(false);
            };
            if summary.get("isCompactSummary").and_then(Value::as_bool) != Some(true)
                || summary
                    .get("isVisibleInTranscriptOnly")
                    .and_then(Value::as_bool)
                    != Some(true)
            {
                return Ok(false);
            }
            claude_summary_pair_is_exact(
                snapshot,
                envelope,
                candidate.provider,
                candidate.message_id,
            )
            .await
        })
    }
}

/// The native-summary recognizer registry, in evaluation order.
///
/// The order is load-bearing and Codex must stay first. Recognition used to be
/// a chain of inline `provider ==` branches in which the Codex branch returned
/// *before* the row was decoded as a canonical envelope; a Codex row is raw
/// provider metadata and that decode always fails for it. Keying is per
/// provider, so in practice one scan only ever consults one entry, but the
/// order is written out rather than left implicit so the original precedence
/// survives if a provider ever registers more than one recognizer.
const NATIVE_SUMMARY_RECOGNIZERS: &[(&str, &dyn NativeSummaryRecognizerV1)] = &[
    ("codex", &CodexNativeCompactionV1),
    ("cursor", &CursorNativeCompactionV1),
    ("claude", &ClaudeNativeCompactionV1),
];

/// The recognizers registered for one provider, in evaluation order.
pub(super) fn native_summary_recognizers(
    provider: &str,
) -> Vec<&'static dyn NativeSummaryRecognizerV1> {
    NATIVE_SUMMARY_RECOGNIZERS
        .iter()
        .filter(|(id, _)| *id == provider)
        .map(|(_, recognizer)| *recognizer)
        .collect()
}

/// Whether the boundary record named by this summary points back at the
/// summary itself, which is what makes the pair authoritative rather than a
/// stray flagged message.
async fn claude_summary_pair_is_exact(
    snapshot: &impl QueryExecutor,
    summary: &CanonicalObservationEnvelopeV1,
    provider: &str,
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
             WHERE provider = ?1 AND message_id = ?2
               AND session_id = ?3
               AND kind IN ('compact_boundary', 'compaction')
             LIMIT 1",
            params![provider, boundary_id.as_str(), session_id.as_str()],
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

type AuthoritativeSummaryFuture =
    Pin<Box<dyn Future<Output = Result<AuthoritativeSummary, SummaryResolutionError>> + Send>>;

/// One provider's ability to be asked for a summary it has not already stored.
pub(super) trait AuthoritativeSummarizerV1: Sync {
    fn summarize(
        &self,
        request: LcmSummaryRequest,
        timeout: Duration,
    ) -> AuthoritativeSummaryFuture;
}

struct CodexAppServerSummarizerV1;

impl AuthoritativeSummarizerV1 for CodexAppServerSummarizerV1 {
    fn summarize(
        &self,
        request: LcmSummaryRequest,
        timeout: Duration,
    ) -> AuthoritativeSummaryFuture {
        Box::pin(codex_app_server_summary(request, timeout))
    }
}

async fn codex_app_server_summary(
    request: LcmSummaryRequest,
    timeout: Duration,
) -> Result<AuthoritativeSummary, SummaryResolutionError> {
    let mut config =
        tracedecay_sessions::runtime::codex_app_server::CodexAppServerSummaryConfig::from_env();
    config.timeout = config.timeout.min(timeout);
    let result = tokio::task::spawn_blocking(move || {
        tracedecay_sessions::runtime::codex_app_server::summarize_with_codex_app_server(
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

/// The on-demand summarizer registry. A provider absent from this table has no
/// authoritative summarizer, which is what keeps its frontier pending instead
/// of admitting a summary this daemon invented.
const AUTHORITATIVE_SUMMARIZERS: &[(&str, &dyn AuthoritativeSummarizerV1)] =
    &[("codex", &CodexAppServerSummarizerV1)];

/// The summarizer registered for one provider, if any.
pub(super) fn authoritative_summarizer(
    provider: &str,
) -> Option<&'static dyn AuthoritativeSummarizerV1> {
    AUTHORITATIVE_SUMMARIZERS
        .iter()
        .find(|(id, _)| *id == provider)
        .map(|(_, summarizer)| *summarizer)
}
