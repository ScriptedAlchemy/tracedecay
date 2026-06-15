use serde_json::{json, Value};

use super::types::{
    LcmExtractionRequest, LcmExtractionResult, LcmSummarySourceMessage, LcmSummarySourceRange,
};

const EXTRACTION_PROMPT: &str = r"Extract decisions, commitments, outcomes, and rules from this conversation segment.

Format as a flat list of bullet points. Each bullet should be self-contained and understandable
without the surrounding conversation. Include:
- Decisions made (what was chosen, and why if stated)
- Commitments (who will do what)
- Outcomes (what happened as a result of an action)
- Rules or constraints discovered

Skip: greetings, meta-discussion, reasoning that led nowhere, repeated information.
If there is nothing worth extracting, respond with exactly: NOTHING_TO_EXTRACT

CONTENT:
{text}";

// Route-envelope contract with Hermes plugin:
// - `route`: caller-provided summarizer route string
// - `pre_compaction_extraction`: optional extraction result object
#[derive(Debug, serde::Deserialize)]
struct LcmProvidedSummaryRouteEnvelope {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    route: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pre_compaction_extraction: Option<LcmExtractionResult>,
}

pub(crate) fn build_extraction_request(
    session_id: &str,
    source_range: &LcmSummarySourceRange,
    source_messages: &[LcmSummarySourceMessage],
) -> Option<LcmExtractionRequest> {
    if source_messages.is_empty() {
        return None;
    }
    let serialized_messages = serialize_messages(source_messages);
    let prompt = EXTRACTION_PROMPT.replace("{text}", &serialized_messages);
    Some(LcmExtractionRequest {
        session_id: session_id.to_string(),
        source_range: source_range.clone(),
        prompt,
    })
}

pub(crate) fn split_summary_route(
    route: Option<&str>,
) -> (Option<String>, Option<LcmExtractionResult>) {
    let route = route.and_then(non_empty).map(str::to_string);
    let Some(route) = route else {
        return (None, None);
    };
    if !(route.starts_with('{') && route.ends_with('}')) {
        return (Some(route), None);
    }
    let parsed = serde_json::from_str::<LcmProvidedSummaryRouteEnvelope>(&route);
    if let Ok(envelope) = parsed {
        return (
            envelope
                .route
                .as_deref()
                .and_then(non_empty)
                .map(str::to_string),
            envelope.pre_compaction_extraction,
        );
    }
    (Some(route), None)
}

pub(crate) fn summary_metadata_extraction(
    extraction_result: Option<&LcmExtractionResult>,
    condensation: bool,
) -> Value {
    // Intentional divergence from upstream hermes-lcm: extraction output is persisted in
    // summary-node metadata. `LCM_EXTRACTION_OUTPUT_PATH` is accepted for parity, but no
    // markdown file is written by the Rust LCM pipeline.
    if condensation {
        return json!({ "status": "not_applicable" });
    }
    if let Some(extraction_result) = extraction_result {
        return serde_json::to_value(extraction_result)
            .unwrap_or_else(|_| json!({ "status": "invalid_result_contract" }));
    }
    json!({ "status": "not_requested" })
}

fn serialize_messages(messages: &[LcmSummarySourceMessage]) -> String {
    messages
        .iter()
        .map(|message| {
            format!(
                "[{}]: {}",
                message.role.to_ascii_uppercase(),
                message.content
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn non_empty(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}
