use serde_json::{Value, json};

use super::types::{
    LcmExtractionRequest, LcmExtractionResult, LcmSummarySourceMessage, LcmSummarySourceRange,
};

const EXTRACTION_PROMPT: &str = r"Extract durable decisions, commitments, outcomes, and constraints from this conversation segment.

Return a flat list of self-contained bullet points that can be understood without the
surrounding conversation. Preserve exact paths, identifiers, citations, source references,
and redaction markers. Do not infer omitted or redacted content.

Skip greetings, meta-discussion, abandoned reasoning, and repeated information.
If there is nothing worth extracting, respond with exactly: NOTHING_TO_EXTRACT

CONTENT:
{text}";

pub fn build_extraction_request(
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

pub fn summary_metadata_extraction(
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
