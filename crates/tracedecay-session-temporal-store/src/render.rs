//! DB-free LCM compatibility shaping after canonical temporal hydration.
//!
//! Truncation, offsets, and typed omissions are decided here so the registered
//! database adapters keep only snapshot, hydration, and transaction ownership.

use tracedecay_domain::HydrationStateV1;

use tracedecay_lcm::contracts::{
    LcmContentRange, LcmContentSlice, LcmError, LcmExpandResponse, LcmRawMessageOverview,
    LcmSourceRef,
};

#[derive(Debug)]
pub struct CanonicalLcmSourceHydration {
    pub source_ref: LcmSourceRef,
    pub state: HydrationStateV1,
    pub content: Option<String>,
}

#[derive(Debug)]
pub enum CanonicalLcmSourceHydrationError {
    Cardinality,
    Identity,
    InvalidContentState,
    PayloadIntegrity,
}

#[tracing::instrument(name = "session_temporal.hydrate.render", level = "trace", skip_all)]
pub fn apply_canonical_content(
    mut expansion: LcmExpandResponse,
    slice: LcmContentSlice,
    canonical_content: &str,
) -> Result<LcmExpandResponse, LcmError> {
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
    } else if let Some(metadata) = expansion.raw_message_metadata.take() {
        let mut raw = metadata.with_verified_content(canonical_content.to_string())?;
        raw.content.clone_from(&content);
        expansion.raw_message = Some(raw);
    }
    if let Some(summary) = expansion.summary_node.as_mut() {
        summary.summary_text = content;
    }
    Ok(expansion)
}

pub fn apply_canonical_description_content(
    messages: &mut Vec<LcmRawMessageOverview>,
    hydration: &[CanonicalLcmSourceHydration],
) -> Result<u64, CanonicalLcmSourceHydrationError> {
    if messages.len() != hydration.len() {
        return Err(CanonicalLcmSourceHydrationError::Cardinality);
    }
    let mut visible = Vec::with_capacity(messages.len());
    let mut omitted = 0;
    for (mut message, canonical) in messages.drain(..).zip(hydration) {
        if canonical.source_ref
            != (LcmSourceRef::RawMessage {
                store_id: message.store_id,
            })
        {
            return Err(CanonicalLcmSourceHydrationError::Identity);
        }
        match (canonical.state, canonical.content.as_deref()) {
            (HydrationStateV1::Available, Some(content)) => {
                let total_chars = content.chars().count() as u64;
                message.content_preview = content
                    .chars()
                    .take(tracedecay_lcm::MAX_DERIVED_SNIPPET_CHARS)
                    .collect();
                let returned_chars = message.content_preview.chars().count() as u64;
                message.content_range = LcmContentRange {
                    offset: 0,
                    limit: returned_chars,
                    returned_chars,
                    total_chars,
                    truncated: returned_chars < total_chars,
                };
                visible.push(message);
            }
            (HydrationStateV1::Available, None) | (_, Some(_)) => {
                return Err(CanonicalLcmSourceHydrationError::InvalidContentState);
            }
            (_, None) => omitted += 1,
        }
    }
    *messages = visible;
    Ok(omitted)
}

pub fn apply_canonical_summary_source_content(
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
            (HydrationStateV1::Available, Some(canonical_content)) => {
                let total_chars = canonical_content.chars().count();
                let offset = slice.offset.min(total_chars);
                let content = canonical_content
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
                } else if let Some(metadata) = source.raw_message_metadata.take() {
                    let mut raw = metadata
                        .with_verified_content(canonical_content.to_string())
                        .map_err(|_| CanonicalLcmSourceHydrationError::PayloadIntegrity)?;
                    raw.content.clone_from(&content);
                    source.raw_message = Some(raw);
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
                if let Some(raw) = source.raw_message.take() {
                    source.raw_message_metadata = Some(raw.into_metadata());
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
    use tracedecay_lcm::contracts::{LcmExpandedSummarySource, LcmRawMessage, LcmStorageKind};

    fn overview(store_id: i64) -> LcmRawMessageOverview {
        LcmRawMessageOverview {
            message_id: format!("message-{store_id}"),
            store_id,
            role: "assistant".to_owned(),
            storage_kind: LcmStorageKind::Inline,
            payload_ref: None,
            content_preview: "stale preview canary".to_owned(),
            content_range: LcmContentRange {
                offset: 0,
                limit: 20,
                returned_chars: 20,
                total_chars: 20,
                truncated: false,
            },
        }
    }

    #[test]
    fn canonical_description_counts_characters_and_omits_unavailable_previews() {
        let text = "界".repeat(tracedecay_lcm::MAX_DERIVED_SNIPPET_CHARS + 17);
        let mut messages = vec![overview(1), overview(2), overview(3)];
        let hydration = [
            CanonicalLcmSourceHydration {
                source_ref: LcmSourceRef::RawMessage { store_id: 1 },
                state: HydrationStateV1::Available,
                content: Some(text.clone()),
            },
            CanonicalLcmSourceHydration {
                source_ref: LcmSourceRef::RawMessage { store_id: 2 },
                state: HydrationStateV1::Redacted,
                content: None,
            },
            CanonicalLcmSourceHydration {
                source_ref: LcmSourceRef::RawMessage { store_id: 3 },
                state: HydrationStateV1::RetainedButUnavailable,
                content: None,
            },
        ];
        assert_eq!(
            apply_canonical_description_content(&mut messages, &hydration).unwrap(),
            2
        );
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].message_id, "message-1");
        assert_eq!(
            messages[0].content_preview,
            "界".repeat(tracedecay_lcm::MAX_DERIVED_SNIPPET_CHARS)
        );
        assert_eq!(
            messages[0].content_range.total_chars,
            text.chars().count() as u64
        );
        assert_eq!(
            messages[0].content_range.returned_chars,
            tracedecay_lcm::MAX_DERIVED_SNIPPET_CHARS as u64
        );
        assert!(messages[0].content_range.truncated);
    }

    #[test]
    fn canonical_description_rejects_mismatched_identity_and_content_state() {
        assert!(matches!(
            apply_canonical_description_content(&mut vec![overview(1)], &[]),
            Err(CanonicalLcmSourceHydrationError::Cardinality)
        ));
        let wrong_identity = CanonicalLcmSourceHydration {
            source_ref: LcmSourceRef::RawMessage { store_id: 2 },
            state: HydrationStateV1::Available,
            content: Some("content".to_owned()),
        };
        assert!(matches!(
            apply_canonical_description_content(&mut vec![overview(1)], &[wrong_identity]),
            Err(CanonicalLcmSourceHydrationError::Identity)
        ));
        for (state, content) in [
            (HydrationStateV1::Available, None),
            (
                HydrationStateV1::Unauthorized,
                Some("denied canary".to_owned()),
            ),
        ] {
            let hydration = CanonicalLcmSourceHydration {
                source_ref: LcmSourceRef::RawMessage { store_id: 1 },
                state,
                content,
            };
            assert!(matches!(
                apply_canonical_description_content(&mut vec![overview(1)], &[hydration]),
                Err(CanonicalLcmSourceHydrationError::InvalidContentState)
            ));
        }
    }

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
                metadata_json: None,
            }),
            raw_message_metadata: None,
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
            raw_message_metadata: None,
            summary_node: None,
            summary_sources: vec![source(1), source(2), source(3), source(4)],
            payload_ref: None,
            from_current_session: None,
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
            assert!(source.raw_message.is_none());
            assert!(source.raw_message_metadata.is_some());
            assert!(source.content_range.is_none());
            assert!(!source.content_truncated);
        }
    }

    #[test]
    fn canonical_raw_hydration_rejects_metadata_hash_mismatch() {
        let raw_message_metadata = source(1).raw_message.expect("raw fixture").into_metadata();
        let expansion = LcmExpandResponse {
            kind: "raw_message".to_string(),
            content: String::new(),
            content_range: LcmContentRange {
                offset: 0,
                limit: 32,
                returned_chars: 0,
                total_chars: 0,
                truncated: false,
            },
            raw_message: None,
            raw_message_metadata: Some(raw_message_metadata),
            summary_node: None,
            summary_sources: Vec::new(),
            payload_ref: None,
            from_current_session: None,
            source_pagination: None,
        };

        let result = apply_canonical_content(
            expansion,
            LcmContentSlice {
                offset: 0,
                limit: 32,
            },
            "canonical content",
        );

        assert!(matches!(result, Err(LcmError::PayloadIntegrityMismatch)));
    }
}
