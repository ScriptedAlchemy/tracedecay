//! DB-free LCM compatibility shaping after canonical temporal hydration.
//!
//! Truncation, offsets, and typed omissions are decided here so the registered
//! database adapters keep only snapshot, hydration, and transaction ownership.

use tracedecay_domain::HydrationStateV1;

use super::contracts::{LcmContentRange, LcmContentSlice, LcmExpandResponse, LcmSourceRef};

#[derive(Debug)]
pub(crate) struct CanonicalLcmSourceHydration {
    pub(crate) source_ref: LcmSourceRef,
    pub(crate) state: HydrationStateV1,
    pub(crate) content: Option<String>,
}

#[derive(Debug)]
pub(crate) enum CanonicalLcmSourceHydrationError {
    Cardinality,
    Identity,
    InvalidContentState,
}

pub(crate) fn apply_canonical_content(
    mut expansion: LcmExpandResponse,
    slice: LcmContentSlice,
    canonical_content: &str,
) -> LcmExpandResponse {
    let total_chars = canonical_content.chars().count();
    let offset = slice.offset.min(total_chars);
    let content = canonical_content
        .chars()
        .skip(offset)
        .take(slice.limit)
        .collect::<String>();
    let returned_chars = content.chars().count();

    expansion.content.clone_from(&content);
    expansion.content_range.offset = offset as u64;
    expansion.content_range.limit = slice.limit as u64;
    expansion.content_range.returned_chars = returned_chars as u64;
    expansion.content_range.total_chars = total_chars as u64;
    expansion.content_range.truncated =
        offset > 0 || offset.saturating_add(returned_chars) < total_chars;
    if let Some(raw) = expansion.raw_message.as_mut() {
        raw.content.clone_from(&content);
    }
    if let Some(summary) = expansion.summary_node.as_mut() {
        summary.summary_text = content;
    }
    expansion
}

pub(crate) fn apply_canonical_summary_source_content(
    expansion: &mut LcmExpandResponse,
    slice: LcmContentSlice,
    hydration: &[CanonicalLcmSourceHydration],
) -> Result<(), CanonicalLcmSourceHydrationError> {
    if expansion.summary_sources.len() != hydration.len() {
        return Err(CanonicalLcmSourceHydrationError::Cardinality);
    }
    for (source, canonical) in expansion.summary_sources.iter_mut().zip(hydration) {
        if source.source_ref != canonical.source_ref {
            return Err(CanonicalLcmSourceHydrationError::Identity);
        }
        source.state = canonical.state;
        match (canonical.state, canonical.content.as_deref()) {
            (HydrationStateV1::Available, Some(content)) => {
                let total_chars = content.chars().count();
                let offset = slice.offset.min(total_chars);
                let content = content
                    .chars()
                    .skip(offset)
                    .take(slice.limit)
                    .collect::<String>();
                let returned_chars = content.chars().count();
                let range = LcmContentRange {
                    offset: offset as u64,
                    limit: slice.limit as u64,
                    returned_chars: returned_chars as u64,
                    total_chars: total_chars as u64,
                    truncated: offset > 0 || offset.saturating_add(returned_chars) < total_chars,
                };
                source.content.clone_from(&content);
                source.content_truncated = range.truncated;
                source.content_range = Some(range);
                if let Some(raw) = source.raw_message.as_mut() {
                    raw.content.clone_from(&content);
                }
                if let Some(summary) = source.summary_node.as_mut() {
                    summary.summary_text.clone_from(&content);
                }
            }
            (HydrationStateV1::Available, None) | (_, Some(_)) => {
                return Err(CanonicalLcmSourceHydrationError::InvalidContentState);
            }
            (_, None) => {
                source.content.clear();
                source.content_range = None;
                source.content_truncated = false;
                if let Some(raw) = source.raw_message.as_mut() {
                    raw.content.clear();
                }
                if let Some(summary) = source.summary_node.as_mut() {
                    summary.summary_text.clear();
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::session::lcm::contracts::{
        LcmExpandedSummarySource, LcmRawMessage, LcmStorageKind,
    };

    fn source(store_id: i64) -> LcmExpandedSummarySource {
        LcmExpandedSummarySource {
            source_ref: LcmSourceRef::RawMessage { store_id },
            state: HydrationStateV1::Available,
            content: String::new(),
            content_range: None,
            content_truncated: false,
            raw_message: Some(LcmRawMessage {
                provider: "cursor".to_string(),
                message_id: format!("message-{store_id}"),
                session_id: "session".to_string(),
                store_id,
                role: "assistant".to_string(),
                ordinal: store_id,
                timestamp: None,
                content: "legacy projection poison".to_string(),
                content_hash: "hash".to_string(),
                storage_kind: LcmStorageKind::Inline,
                payload_ref: None,
                legacy_source: false,
                legacy_truncated: false,
                metadata_json: None,
            }),
            summary_node: None,
        }
    }

    #[test]
    fn canonical_summary_source_content_preserves_order_and_typed_omissions() {
        let mut expansion = LcmExpandResponse {
            kind: "summary_node".to_string(),
            content: "summary".to_string(),
            content_range: LcmContentRange {
                offset: 0,
                limit: 7,
                returned_chars: 7,
                total_chars: 7,
                truncated: false,
            },
            raw_message: None,
            summary_node: None,
            summary_sources: vec![source(1), source(2), source(3), source(4)],
            payload_ref: None,
            from_current_session: None,
            externalized_note: None,
            source_pagination: None,
        };
        let hydration = vec![
            CanonicalLcmSourceHydration {
                source_ref: LcmSourceRef::RawMessage { store_id: 1 },
                state: HydrationStateV1::Available,
                content: Some("available source".to_string()),
            },
            CanonicalLcmSourceHydration {
                source_ref: LcmSourceRef::RawMessage { store_id: 2 },
                state: HydrationStateV1::Redacted,
                content: None,
            },
            CanonicalLcmSourceHydration {
                source_ref: LcmSourceRef::RawMessage { store_id: 3 },
                state: HydrationStateV1::Unauthorized,
                content: None,
            },
            CanonicalLcmSourceHydration {
                source_ref: LcmSourceRef::RawMessage { store_id: 4 },
                state: HydrationStateV1::Deleted,
                content: None,
            },
        ];

        apply_canonical_summary_source_content(
            &mut expansion,
            LcmContentSlice {
                offset: 0,
                limit: 9,
            },
            &hydration,
        )
        .expect("matching canonical source hydration");

        assert_eq!(
            expansion
                .summary_sources
                .iter()
                .map(|source| source.source_ref.clone())
                .collect::<Vec<_>>(),
            hydration
                .iter()
                .map(|source| source.source_ref.clone())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            expansion.summary_sources[0].state,
            HydrationStateV1::Available
        );
        assert_eq!(expansion.summary_sources[0].content, "available");
        assert_eq!(
            expansion.summary_sources[0]
                .raw_message
                .as_ref()
                .unwrap()
                .content,
            "available"
        );
        assert!(expansion.summary_sources[0].content_truncated);
        for (source, state) in expansion.summary_sources[1..].iter().zip([
            HydrationStateV1::Redacted,
            HydrationStateV1::Unauthorized,
            HydrationStateV1::Deleted,
        ]) {
            assert_eq!(source.state, state);
            assert!(source.content.is_empty());
            assert!(source.raw_message.as_ref().unwrap().content.is_empty());
            assert!(source.content_range.is_none());
            assert!(!source.content_truncated);
        }
    }
}
