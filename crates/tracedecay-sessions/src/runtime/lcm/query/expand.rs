use std::path::Path;

use super::*;

pub(crate) async fn expand(
    conn: &(impl QueryExecutor + ?Sized),
    storage_root: &Path,
    request: LcmExpandRequest,
) -> Result<LcmExpandResponse, LcmError> {
    match request.target {
        LcmExpandTarget::RawMessage { store_id } => {
            let raw = raw::load_raw_message_by_store_id(conn, store_id).await?;
            // Raw store_id expansion works across sessions like hermes-lcm
            // `lcm_expand` store_id mode (grep scope=all -> expand the hit),
            // but stays provider-scoped: providers are a TraceDecay concept
            // with no Hermes equivalent.
            if raw.provider != request.provider {
                return Err(LcmError::SummarySourceNotOwnedBySession);
            }
            let from_current_session = raw.session_id == request.session_id;
            let externalized_ref = raw.payload_ref.clone();
            let (raw, range) = raw_message_with_sliced_content(raw, request.content_slice);
            let content = raw.content.clone();
            let payload_ref = if from_current_session {
                None
            } else {
                externalized_ref.clone()
            };
            Ok(LcmExpandResponse {
                kind: "raw_message".to_string(),
                content,
                content_range: range,
                raw_message: Some(raw),
                summary_node: None,
                summary_sources: Vec::new(),
                payload_ref,
                from_current_session: Some(from_current_session),
                externalized_note: None,
                source_pagination: None,
            })
        }
        LcmExpandTarget::SummaryNode { node_id } => {
            let expansion =
                dag::expand_summary_node(conn, &request.provider, &request.session_id, &node_id)
                    .await?;
            let (summary, range) =
                summary_node_with_sliced_text(expansion.summary, request.content_slice);
            let content = summary.summary_text.clone();
            let (sources, source_pagination) = paginate_summary_sources(
                expansion.sources,
                request.source_offset,
                request.source_limit,
            );
            let summary_sources = slice_summary_sources(sources, request.content_slice);
            Ok(LcmExpandResponse {
                kind: "summary_node".to_string(),
                content,
                content_range: range,
                raw_message: None,
                summary_node: Some(summary),
                summary_sources,
                payload_ref: None,
                from_current_session: None,
                externalized_note: None,
                source_pagination: Some(source_pagination),
            })
        }
        LcmExpandTarget::ExternalPayload { payload_ref } => {
            let slice = request.content_slice.unwrap_or(LcmContentSlice {
                offset: 0,
                limit: usize::MAX,
            });
            let expansion = payload::expand_payload(
                conn,
                storage_root,
                &request.provider,
                &request.session_id,
                &payload_ref,
                slice.offset,
                slice.limit,
            )
            .await?;
            let range = LcmContentRange {
                offset: expansion.offset,
                limit: slice.limit as u64,
                returned_chars: expansion.char_count,
                total_chars: expansion.total_char_count,
                truncated: expansion.offset > 0
                    || expansion.offset.saturating_add(expansion.char_count)
                        < expansion.total_char_count,
            };
            Ok(LcmExpandResponse {
                kind: "external_payload".to_string(),
                content: expansion.content,
                content_range: range,
                raw_message: None,
                summary_node: None,
                summary_sources: Vec::new(),
                payload_ref: Some(expansion.payload_ref),
                from_current_session: None,
                externalized_note: None,
                source_pagination: None,
            })
        }
    }
}

/// True when a context block is machine noise rather than readable text: a
/// base64/hex thinking-signature blob or comparable binary-ish payload. Such
/// blocks never help answer a query and only waste the synthesis budget, so
/// expand-query assembly skips them.
///
/// Deterministic and cheap: prose never contains an unbroken ~160-char run of
/// pure base64/hex-alphabet characters, whereas signature blobs are exactly
/// that. Whitespace-delimited tokens are checked so ordinary URLs or paths
/// (which contain separators well under the threshold) are never misclassified.
pub(super) fn is_noise_block_content(content: &str) -> bool {
    const NOISE_TOKEN_MIN_CHARS: usize = 160;
    let trimmed = content.trim();
    if trimmed.len() < NOISE_TOKEN_MIN_CHARS {
        return false;
    }
    trimmed
        .split_whitespace()
        .any(|token| token_is_signature_blob(token, NOISE_TOKEN_MIN_CHARS))
}

fn token_is_signature_blob(token: &str, min_chars: usize) -> bool {
    if token.chars().count() < min_chars {
        return false;
    }
    // Base64 / base64url / hex alphabet only. A real word or path this long
    // would contain characters outside this set (spaces are already split off).
    let mut has_lower = false;
    let mut has_upper = false;
    let mut has_digit = false;
    for c in token.chars() {
        if !(c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=' | '-' | '_')) {
            return false;
        }
        has_lower |= c.is_ascii_lowercase();
        has_upper |= c.is_ascii_uppercase();
        has_digit |= c.is_ascii_digit();
    }
    // Require real base64 entropy (mixed case and digits). A monotonous run of
    // one repeated character — long padding, ASCII art, a giant single-case
    // word — is not a signature blob and must not be dropped as noise.
    has_lower && has_upper && has_digit
}

pub(super) fn expand_query_match_from_hit(hit: &LcmGrepHit) -> LcmExpandQueryMatch {
    LcmExpandQueryMatch {
        kind: hit.kind.clone(),
        node_id: hit.node_id.clone(),
        store_id: hit.store_id,
        snippet: hit.snippet.clone(),
    }
}

pub(super) fn expand_query_synthesis_prompt(
    prompt: &str,
    context_blocks: &[LcmExpandQueryContextBlock],
    context_truncated: bool,
) -> LcmExpandQuerySynthesisPrompt {
    let system = LCM_EXPAND_QUERY_SYNTHESIS_SYSTEM_PROMPT.to_string();
    let context_json = serde_json::to_string_pretty(context_blocks).unwrap_or_else(|_| "[]".into());
    let truncation_note = if context_truncated {
        "\n\nNOTE: Some LCM context was truncated; pagination metadata is included in the tool response."
    } else {
        ""
    };
    let user = format!("QUESTION:\n{prompt}\n\nEXPANDED CONTEXT:\n{context_json}{truncation_note}");
    LcmExpandQuerySynthesisPrompt { system, user }
}
