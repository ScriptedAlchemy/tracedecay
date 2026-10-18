use std::path::Path;

use super::*;

#[tracing::instrument(name = "sessions.lcm.expand", level = "trace", skip_all)]
pub async fn expand(
    conn: &(impl QueryExecutor + ?Sized),
    storage_root: &Path,
    request: LcmExpandRequest,
) -> Result<LcmExpandResponse, LcmError> {
    match request.target {
        LcmExpandTarget::CanonicalOccurrence { message_id } => {
            let raw = raw::load_raw_message_by_identity(
                conn,
                &request.provider,
                &request.session_id,
                &message_id,
            )
            .await?
            .ok_or(LcmError::SummarySourceNotOwnedBySession)?;
            let (raw, range) = raw_message_with_sliced_content(raw, request.content_slice);
            let content = raw.content.clone();
            Ok(LcmExpandResponse {
                kind: "raw_message".to_string(),
                content,
                content_range: range,
                raw_message: Some(raw),
                raw_message_metadata: None,
                summary_node: None,
                summary_sources: Vec::new(),
                payload_ref: None,
                from_current_session: Some(true),
                source_pagination: None,
            })
        }
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
                raw_message_metadata: None,
                summary_node: None,
                summary_sources: Vec::new(),
                payload_ref,
                from_current_session: Some(from_current_session),
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
                raw_message_metadata: None,
                summary_node: Some(summary),
                summary_sources,
                payload_ref: None,
                from_current_session: None,
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
                raw_message_metadata: None,
                summary_node: None,
                summary_sources: Vec::new(),
                payload_ref: Some(expansion.payload_ref),
                from_current_session: None,
                source_pagination: None,
            })
        }
    }
}
