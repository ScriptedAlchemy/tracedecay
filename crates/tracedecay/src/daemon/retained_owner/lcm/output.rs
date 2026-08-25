//! Typed projections from daemon session-temporal values to retained results.

use tracedecay_application::RetainedSurfaceExecutionErrorV1;
use tracedecay_application::retained_surfaces::{
    ClosedUtcIntervalV1, CompactLineageEdgeV1, HydrationStateResultV1, LcmContentRangeV1,
    LcmDescribeExternalPayloadV1, LcmDescribeSourceOverviewV1, LcmDescribeSummaryNodeV1,
    LcmDescriptionV1, LcmExpandQueryBudgetV1, LcmExpandQueryContextBlockV1, LcmExpandQueryMatchV1,
    LcmExpandQueryPaginationV1, LcmExpandQueryResultV1, LcmExpandQuerySynthesisPromptV1,
    LcmExpandedSourceV1, LcmExpansionV1, LcmGrepHitV1, LcmMessageV1, LcmRawMessageMetadataV1,
    LcmRawMessageOverviewV1, LcmRawMessageV1, LcmRetrievalOutcomeV1, LcmSourcePaginationV1,
    LcmSourceRefV1, LcmStorageKindV1, LcmSummaryNodeOverviewV1, LcmSummaryNodeV1,
    LcmTemporalFieldsV1, RetainedOutcomeStatusV1, SessionCoverageIntervalV1, SessionCoverageModeV1,
    SessionCoverageReasonV1, SessionCoverageRequestV1, SessionCoverageStateV1,
    SessionSourceCoverageV1 as RetainedSourceCoverageV1, TemporalExplanationV1,
    TemporalFreshnessV1, TemporalOmissionV1, TemporalWatermarksV1, ValidCoverageIntervalV1,
};
use tracedecay_domain::{
    CompactContextLineageEdgeV1, HydrationStateV1, SessionSourceCoverageIntervalV1,
    SessionSourceCoverageReasonV1, SessionSourceCoverageStateV1, SessionSourceCoverageV1,
    TemporalModeV1, ValidCoverageIntervalV1 as DomainValidCoverageIntervalV1,
};
use tracedecay_sessions::lcm::contracts::{
    LcmContentRange, LcmDataFreshness, LcmDescribeResponse, LcmExpandResponse, LcmRawMessage,
    LcmRawMessageMetadata, LcmRetrievalOutcome, LcmSourceRef, LcmStorageKind, LcmSummaryNode,
};
use tracedecay_sessions::runtime::SessionMessageSearchResult;
use tracedecay_sessions::runtime::lcm::{
    LCM_EXPAND_QUERY_SYNTHESIS_SYSTEM_PROMPT, LcmContentSlice, LcmExpandQueryBudget,
    LcmExpandQueryContextBlock, LcmExpandQueryMatch, LcmExpandQueryResponse,
    LcmExpandQuerySynthesisPrompt,
};
use tracedecay_temporal_query::context::OrderedTextContextAssembler;

use crate::daemon::session_retrieval::SessionTemporalMetadataView;

pub(super) fn temporal_fields(value: SessionTemporalMetadataView) -> LcmTemporalFieldsV1 {
    LcmTemporalFieldsV1 {
        anchors: value
            .anchors
            .into_iter()
            .map(|anchor| anchor.as_str().to_owned())
            .collect(),
        watermarks: TemporalWatermarksV1 {
            generation: value.watermarks.generation,
            source: value.watermarks.source,
            projection: value.watermarks.projection,
            index: value.watermarks.index,
            summary: value.watermarks.summary,
        },
        authorized_root: value.authorized_root,
        coverage: tracedecay_application::retained_surfaces::TemporalCoverageV1 {
            visible: value.coverage.visible,
            hidden: value.coverage.hidden,
            unknown: value.coverage.unknown,
            redacted: value.coverage.redacted,
        },
        source_coverage: value
            .source_coverage
            .into_iter()
            .map(source_coverage)
            .collect(),
        explanations: value
            .explanations
            .into_iter()
            .map(|item| TemporalExplanationV1 {
                anchor: item.anchor.as_str().to_owned(),
                summary: item.summary,
            })
            .collect(),
        omissions: value
            .omissions
            .into_iter()
            .map(|item| TemporalOmissionV1 {
                rank: item.rank,
                anchor: item.anchor.as_str().to_owned(),
                reason: hydration(item.reason),
            })
            .collect(),
        next_cursor: value.cursor,
    }
}

fn source_coverage(value: SessionSourceCoverageV1) -> RetainedSourceCoverageV1 {
    RetainedSourceCoverageV1 {
        source_id: value.source_id().as_str().to_owned(),
        observed_frontier: value.observed_frontier().value(),
        committed_frontier: value.committed_frontier().value(),
        target_watermark: value.target_watermark().value(),
        request: SessionCoverageRequestV1 {
            mode: coverage_mode(value.request().mode()),
        },
        covered_intervals: value
            .covered_intervals()
            .iter()
            .cloned()
            .map(coverage_interval)
            .collect(),
        missing_intervals: value
            .missing_intervals()
            .iter()
            .cloned()
            .map(coverage_interval)
            .collect(),
        state: coverage_state(value.state()),
        reason: coverage_reason(value.reason()),
    }
}

fn coverage_interval(value: SessionSourceCoverageIntervalV1) -> SessionCoverageIntervalV1 {
    SessionCoverageIntervalV1 {
        knowledge: closed_interval(value.knowledge),
        valid: match value.valid {
            DomainValidCoverageIntervalV1::Known(interval) => {
                ValidCoverageIntervalV1::Known(closed_interval(interval))
            }
            DomainValidCoverageIntervalV1::Unknown => ValidCoverageIntervalV1::Unknown,
        },
    }
}

fn closed_interval(value: tracedecay_domain::ClosedUtcIntervalV1) -> ClosedUtcIntervalV1 {
    ClosedUtcIntervalV1 {
        from_inclusive: value.from_inclusive().map(|value| value.0),
        through_inclusive: value.through_inclusive().map(|value| value.0),
    }
}

const fn coverage_mode(value: TemporalModeV1) -> SessionCoverageModeV1 {
    match value {
        TemporalModeV1::Current => SessionCoverageModeV1::Current,
        TemporalModeV1::AsOf { cutoff } => SessionCoverageModeV1::AsOf { cutoff: cutoff.0 },
        TemporalModeV1::Evolution => SessionCoverageModeV1::Evolution,
        TemporalModeV1::Forensic => SessionCoverageModeV1::Forensic,
    }
}

const fn coverage_state(value: SessionSourceCoverageStateV1) -> SessionCoverageStateV1 {
    match value {
        SessionSourceCoverageStateV1::Fresh => SessionCoverageStateV1::Fresh,
        SessionSourceCoverageStateV1::Stale => SessionCoverageStateV1::Stale,
        SessionSourceCoverageStateV1::Partial => SessionCoverageStateV1::Partial,
        SessionSourceCoverageStateV1::Locked => SessionCoverageStateV1::Locked,
        SessionSourceCoverageStateV1::Redacted => SessionCoverageStateV1::Redacted,
        SessionSourceCoverageStateV1::RetentionWithheld => {
            SessionCoverageStateV1::RetentionWithheld
        }
        SessionSourceCoverageStateV1::Unavailable => SessionCoverageStateV1::Unavailable,
    }
}

fn coverage_reason(value: &SessionSourceCoverageReasonV1) -> SessionCoverageReasonV1 {
    match value {
        SessionSourceCoverageReasonV1::CaughtUp => SessionCoverageReasonV1::CaughtUp,
        SessionSourceCoverageReasonV1::ProjectionBehindSource { lag } => {
            SessionCoverageReasonV1::ProjectionBehindSource { lag: *lag }
        }
        SessionSourceCoverageReasonV1::SourceBehindTarget { lag } => {
            SessionCoverageReasonV1::SourceBehindTarget { lag: *lag }
        }
        SessionSourceCoverageReasonV1::ProjectionAndSourceBehind {
            projection_lag,
            source_lag,
        } => SessionCoverageReasonV1::ProjectionAndSourceBehind {
            projection_lag: *projection_lag,
            source_lag: *source_lag,
        },
        SessionSourceCoverageReasonV1::Locked => SessionCoverageReasonV1::Locked,
        SessionSourceCoverageReasonV1::Redacted => SessionCoverageReasonV1::Redacted,
        SessionSourceCoverageReasonV1::RetentionWithheld => {
            SessionCoverageReasonV1::RetentionWithheld
        }
        SessionSourceCoverageReasonV1::Unavailable => SessionCoverageReasonV1::Unavailable,
    }
}

pub(super) fn sliced_message(
    result: SessionMessageSearchResult,
    slice: LcmContentSlice,
) -> LcmMessageV1 {
    let total_chars = result.message.text.chars().count();
    let offset = slice.offset.min(total_chars);
    let content = result
        .message
        .text
        .chars()
        .skip(offset)
        .take(slice.limit)
        .collect::<String>();
    let returned_chars = content.chars().count();
    LcmMessageV1 {
        provider: result.message.provider,
        message_id: result.message.message_id,
        session_id: result.message.session_id,
        store_id: None,
        role: result.message.role,
        ordinal: result.message.ordinal,
        timestamp: result.message.timestamp,
        content,
        content_range: LcmContentRangeV1 {
            offset: offset as u64,
            limit: slice.limit as u64,
            returned_chars: returned_chars as u64,
            total_chars: total_chars as u64,
            truncated: offset.saturating_add(returned_chars) < total_chars,
        },
        content_hash: None,
        storage_kind: LcmStorageKindV1::CanonicalOccurrence,
        payload_ref: None,
        legacy_source: false,
        legacy_truncated: false,
        metadata_json: result.message.metadata_json,
    }
}

pub(super) fn grep_hit(result: SessionMessageSearchResult, max_chars: usize) -> LcmGrepHitV1 {
    let snippet = result.message.text.chars().take(max_chars).collect();
    let summary = result.message.kind.as_deref() == Some("summary");
    let message_id = result.message.message_id;
    LcmGrepHitV1 {
        kind: if summary {
            "summary_node"
        } else {
            "raw_message"
        }
        .to_owned(),
        provider: result.message.provider,
        session_id: result.message.session_id,
        message_id: (!summary).then(|| message_id.clone()),
        node_id: summary.then_some(message_id),
        store_id: None,
        role: (!summary).then_some(result.message.role),
        snippet,
        score: result.score,
    }
}

pub(super) fn retrieval(value: LcmRetrievalOutcome) -> LcmRetrievalOutcomeV1 {
    match value {
        LcmRetrievalOutcome::Complete { freshness } => LcmRetrievalOutcomeV1::Complete {
            freshness: freshness_value(freshness),
        },
        LcmRetrievalOutcome::Partial { freshness, omitted } => LcmRetrievalOutcomeV1::Partial {
            freshness: freshness_value(freshness),
            omitted,
        },
        LcmRetrievalOutcome::Stale { freshness } => LcmRetrievalOutcomeV1::Stale {
            freshness: freshness_value(freshness),
        },
    }
}

const fn freshness_value(value: LcmDataFreshness) -> TemporalFreshnessV1 {
    match value {
        LcmDataFreshness::Fresh => TemporalFreshnessV1::Fresh,
        LcmDataFreshness::Stored { generation_lag } => {
            TemporalFreshnessV1::Stored { generation_lag }
        }
        LcmDataFreshness::Partial { generation_lag } => {
            TemporalFreshnessV1::Partial { generation_lag }
        }
    }
}

pub(super) fn description(value: LcmDescribeResponse) -> LcmDescriptionV1 {
    LcmDescriptionV1 {
        target: value.target,
        provider: value.provider,
        session_id: value.session_id,
        raw_message_count: value.raw_message_count,
        summary_node_count: value.summary_node_count,
        external_payload_count: value.external_payload_count,
        first_store_id: value.first_store_id,
        last_store_id: value.last_store_id,
        raw_messages: value
            .raw_messages
            .into_iter()
            .map(|message| LcmRawMessageOverviewV1 {
                message_id: message.message_id,
                store_id: message.store_id,
                role: message.role,
                storage_kind: storage_kind(message.storage_kind),
                payload_ref: message.payload_ref,
                content_preview: message.content_preview,
                content_range: content_range(message.content_range),
            })
            .collect(),
        summary_nodes: value
            .summary_nodes
            .into_iter()
            .map(|node| LcmSummaryNodeOverviewV1 {
                node_id: node.node_id,
                conversation_id: node.conversation_id,
                depth: node.depth,
                summary_preview: node.summary_preview,
                source_count: node.source_count,
                created_at: node.created_at,
            })
            .collect(),
        summary_node: value.summary_node.map(|node| LcmDescribeSummaryNodeV1 {
            node_id: node.node_id,
            conversation_id: node.conversation_id,
            depth: node.depth,
            summary_token_count: node.summary_token_count,
            source_token_count: node.source_token_count,
            source_time_start: node.source_time_start,
            source_time_end: node.source_time_end,
            expand_hint: node.expand_hint,
            metadata_json: node.metadata_json,
            created_at: node.created_at,
            source_count: node.source_count,
            children: node
                .children
                .into_iter()
                .map(|child| LcmDescribeSourceOverviewV1 {
                    source_kind: child.source_kind,
                    source_ref: source_ref(child.source_ref),
                    store_id: child.store_id,
                    node_id: child.node_id,
                    role: child.role,
                    storage_kind: child.storage_kind.map(storage_kind),
                    summary_token_count: child.summary_token_count,
                    source_token_count: child.source_token_count,
                    expand_hint: child.expand_hint,
                })
                .collect(),
        }),
        external_payload: value
            .external_payload
            .map(|payload| LcmDescribeExternalPayloadV1 {
                payload_ref: payload.payload_ref,
                provider: payload.provider,
                session_id: payload.session_id,
                message_id: payload.message_id,
                kind: payload.kind,
                content_hash: payload.content_hash,
                byte_count: payload.byte_count,
                char_count: payload.char_count,
                created_at: payload.created_at,
                metadata_json: payload.metadata_json,
                content_preview: payload.content_preview,
            }),
        session_token_estimate: value.session_token_estimate,
    }
}

pub(super) fn expansion(value: LcmExpandResponse) -> LcmExpansionV1 {
    LcmExpansionV1 {
        kind: value.kind,
        content: value.content,
        content_range: content_range(value.content_range),
        raw_message: value.raw_message.map(raw_message),
        raw_message_metadata: value.raw_message_metadata.map(raw_message_metadata),
        summary_node: value.summary_node.map(summary_node),
        summary_sources: value
            .summary_sources
            .into_iter()
            .map(|source| LcmExpandedSourceV1 {
                source_ref: source_ref(source.source_ref),
                state: hydration(source.state),
                content: source.content,
                content_range: source.content_range.map(content_range),
                content_truncated: source.content_truncated,
                raw_message: source.raw_message.map(raw_message),
                raw_message_metadata: source.raw_message_metadata.map(raw_message_metadata),
                summary_node: source
                    .summary_node
                    .map(|node| Box::new(summary_node(*node))),
            })
            .collect(),
        payload_ref: value.payload_ref,
        from_current_session: value.from_current_session,
        externalized_note: value.externalized_note,
        source_pagination: value.source_pagination.map(|page| LcmSourcePaginationV1 {
            source_limit: page.source_limit,
            returned_sources: page.returned_sources,
            total_sources: page.total_sources,
            has_more: page.has_more,
            remaining_sources: page.remaining_sources,
        }),
    }
}

pub(super) fn lineage(value: Vec<CompactContextLineageEdgeV1>) -> Vec<CompactLineageEdgeV1> {
    value
        .into_iter()
        .map(|edge| CompactLineageEdgeV1 {
            kind: edge.kind.as_str().to_owned(),
            subject_anchor_id: edge.subject_anchor_id.as_str().to_owned(),
            object_anchor_id: edge.object_anchor_id.as_str().to_owned(),
            knowledge_at: edge.knowledge_at.0,
            authority: edge.authority.as_str().to_owned(),
            authorized: edge.authorized,
            supporting_anchor_ids: edge
                .supporting_anchor_ids
                .into_iter()
                .map(|anchor| anchor.as_str().to_owned())
                .collect(),
        })
        .collect()
}

pub(super) fn expand_query_result(
    value: LcmExpandQueryResponse,
    status: RetainedOutcomeStatusV1,
    omitted: u64,
    provider: &str,
    session_id: &str,
    temporal: LcmTemporalFieldsV1,
) -> LcmExpandQueryResultV1 {
    LcmExpandQueryResultV1 {
        status,
        context_blocks: value
            .context_blocks
            .into_iter()
            .map(|block| LcmExpandQueryContextBlockV1 {
                kind: block.kind,
                node_id: block.node_id,
                source_ref: block.source_ref.map(source_ref),
                content: block.content,
                content_range: content_range(block.content_range),
                raw_message: block.raw_message.map(raw_message),
                summary_node: block.summary_node.map(summary_node),
            })
            .collect(),
        answer: value.answer,
        needs_synthesis: Some(value.needs_synthesis),
        prompt: Some(value.prompt),
        query: value.query,
        synthesis_prompt: value
            .synthesis_prompt
            .map(|prompt| LcmExpandQuerySynthesisPromptV1 {
                system: prompt.system,
                user: prompt.user,
                user_prompt_truncated_for_mcp: None,
            }),
        max_tokens: Some(value.max_tokens),
        context_max_tokens: Some(value.context_max_tokens),
        context_budget: Some(LcmExpandQueryBudgetV1 {
            requested_max_chars: value.context_budget.requested_max_chars,
            used_chars: value.context_budget.used_chars,
        }),
        context_truncated: Some(value.context_truncated),
        context_pagination: Some(
            value
                .context_pagination
                .into_iter()
                .map(|page| LcmExpandQueryPaginationV1 {
                    kind: page.kind,
                    node_id: page.node_id,
                    source_ref: page.source_ref.map(source_ref),
                    state: page.state.map(hydration),
                    next_content_offset: page.next_content_offset,
                    has_more: page.has_more,
                })
                .collect(),
        ),
        node_ids: Some(value.node_ids),
        matches: Some(
            value
                .matches
                .into_iter()
                .map(|item| LcmExpandQueryMatchV1 {
                    kind: item.kind,
                    node_id: item.node_id,
                    store_id: item.store_id,
                    snippet: item.snippet,
                })
                .collect(),
        ),
        omitted: Some(omitted),
        provider: Some(provider.to_owned()),
        session_id: Some(session_id.to_owned()),
        temporal: Some(temporal),
        mcp_response_truncated: None,
        contract_truncated: None,
        mcp_truncation_reason: None,
        prompt_truncated_for_mcp: None,
        query_truncated_for_mcp: None,
        error: None,
        service_status: None,
        capped_sessions: None,
    }
}

pub(super) const fn hydration(value: HydrationStateV1) -> HydrationStateResultV1 {
    match value {
        HydrationStateV1::Available => HydrationStateResultV1::Available,
        HydrationStateV1::RetainedButUnavailable => HydrationStateResultV1::RetainedButUnavailable,
        HydrationStateV1::Redacted => HydrationStateResultV1::Redacted,
        HydrationStateV1::Deleted => HydrationStateResultV1::Deleted,
        HydrationStateV1::RetentionExpired => HydrationStateResultV1::RetentionExpired,
        HydrationStateV1::Unauthorized => HydrationStateResultV1::Unauthorized,
        HydrationStateV1::Locked => HydrationStateResultV1::Locked,
        HydrationStateV1::UnverifiableLegacy => HydrationStateResultV1::UnverifiableLegacy,
    }
}

const fn storage_kind(value: LcmStorageKind) -> LcmStorageKindV1 {
    match value {
        LcmStorageKind::Inline => LcmStorageKindV1::Inline,
        LcmStorageKind::External => LcmStorageKindV1::External,
    }
}

const fn content_range(value: LcmContentRange) -> LcmContentRangeV1 {
    LcmContentRangeV1 {
        offset: value.offset,
        limit: value.limit,
        returned_chars: value.returned_chars,
        total_chars: value.total_chars,
        truncated: value.truncated,
    }
}

fn source_ref(value: LcmSourceRef) -> LcmSourceRefV1 {
    match value {
        LcmSourceRef::RawMessage { store_id } => LcmSourceRefV1::RawMessage { store_id },
        LcmSourceRef::SummaryNode { node_id } => LcmSourceRefV1::SummaryNode { node_id },
    }
}

fn raw_message(value: LcmRawMessage) -> LcmRawMessageV1 {
    LcmRawMessageV1 {
        provider: value.provider,
        message_id: value.message_id,
        session_id: value.session_id,
        store_id: value.store_id,
        role: value.role,
        ordinal: value.ordinal,
        timestamp: value.timestamp,
        content: value.content,
        content_hash: value.content_hash,
        storage_kind: storage_kind(value.storage_kind),
        payload_ref: value.payload_ref,
        legacy_source: value.legacy_source,
        legacy_truncated: value.legacy_truncated,
        metadata_json: value.metadata_json,
    }
}

fn raw_message_metadata(value: LcmRawMessageMetadata) -> LcmRawMessageMetadataV1 {
    LcmRawMessageMetadataV1 {
        provider: value.provider,
        message_id: value.message_id,
        session_id: value.session_id,
        store_id: value.store_id,
        role: value.role,
        ordinal: value.ordinal,
        timestamp: value.timestamp,
        content_hash: value.content_hash,
        storage_kind: storage_kind(value.storage_kind),
        payload_ref: value.payload_ref,
        legacy_source: value.legacy_source,
        legacy_truncated: value.legacy_truncated,
        metadata_json: value.metadata_json,
    }
}

fn summary_node(value: LcmSummaryNode) -> LcmSummaryNodeV1 {
    LcmSummaryNodeV1 {
        node_id: value.node_id,
        provider: value.provider,
        conversation_id: value.conversation_id,
        session_id: value.session_id,
        depth: value.depth,
        summary_text: value.summary_text,
        summary_hash: value.summary_hash,
        source_refs: value.source_refs.into_iter().map(source_ref).collect(),
        summary_token_count: value.summary_token_count,
        source_token_count: value.source_token_count,
        source_time_start: value.source_time_start,
        source_time_end: value.source_time_end,
        expand_hint: value.expand_hint,
        metadata_json: value.metadata_json,
        created_at: value.created_at,
    }
}

pub(super) fn assemble_query(
    prompt: &str,
    query: Option<&str>,
    max_tokens: usize,
    context_max_tokens: usize,
    sources: Vec<(&'static str, Option<String>, String)>,
) -> Result<(LcmExpandQueryResponse, usize), RetainedSurfaceExecutionErrorV1> {
    let source_count = sources.len();
    let mut context = OrderedTextContextAssembler::new(context_max_tokens);
    let mut context_truncated = false;
    let mut matches = Vec::new();
    let mut context_blocks = Vec::new();
    let mut node_ids = Vec::new();
    for (kind, node_id, source_content) in sources {
        let admitted = context.admit(&source_content);
        let Some(content) = admitted.content else {
            context_truncated |= admitted.truncated;
            break;
        };
        context_truncated |= admitted.truncated;
        if let Some(node_id) = &node_id
            && !node_ids.contains(node_id)
        {
            node_ids.push(node_id.clone());
        }
        matches.push(LcmExpandQueryMatch {
            kind: kind.to_owned(),
            node_id: node_id.clone(),
            store_id: None,
            snippet: content.clone(),
        });
        context_blocks.push(LcmExpandQueryContextBlock {
            kind: kind.to_owned(),
            node_id,
            source_ref: None,
            content,
            content_range: LcmContentRange {
                offset: 0,
                limit: admitted.limit,
                returned_chars: admitted.returned_chars,
                total_chars: admitted.total_chars,
                truncated: admitted.truncated,
            },
            raw_message: None,
            summary_node: None,
        });
    }
    let needs_synthesis = !context_blocks.is_empty();
    let synthesis_prompt = if needs_synthesis {
        Some(LcmExpandQuerySynthesisPrompt {
            system: LCM_EXPAND_QUERY_SYNTHESIS_SYSTEM_PROMPT.to_owned(),
            user: format!(
                "QUESTION:\n{prompt}\n\nEXPANDED CONTEXT:\n{}",
                serde_json::to_string(&context_blocks)
                    .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)?
            ),
        })
    } else {
        None
    };
    let dropped = source_count.saturating_sub(context_blocks.len());
    Ok((
        LcmExpandQueryResponse {
            answer: (!needs_synthesis)
                .then(|| "No matching LCM context found in the current session.".to_owned()),
            needs_synthesis,
            prompt: prompt.to_owned(),
            query: query.map(str::to_owned),
            synthesis_prompt,
            max_tokens,
            context_max_tokens,
            context_budget: LcmExpandQueryBudget {
                requested_max_chars: context_max_tokens,
                used_chars: context.used_chars(),
            },
            context_truncated,
            context_pagination: Vec::new(),
            node_ids,
            matches,
            context_blocks,
        },
        dropped,
    ))
}

pub(super) fn merge_temporal(
    target: &mut SessionTemporalMetadataView,
    incoming: SessionTemporalMetadataView,
) -> bool {
    if target
        .authorized_root
        .as_ref()
        .zip(incoming.authorized_root.as_ref())
        .is_some_and(|(left, right)| left != right)
    {
        return false;
    }
    if target.authorized_root.is_none() {
        target.authorized_root = incoming.authorized_root;
    }
    for anchor in incoming.anchors {
        if !target.anchors.contains(&anchor) {
            target.anchors.push(anchor);
        }
    }
    target.watermarks.generation = target
        .watermarks
        .generation
        .max(incoming.watermarks.generation);
    target.watermarks.source = target.watermarks.source.max(incoming.watermarks.source);
    target.watermarks.projection = target
        .watermarks
        .projection
        .max(incoming.watermarks.projection);
    target.watermarks.index = target.watermarks.index.max(incoming.watermarks.index);
    target.watermarks.summary = target.watermarks.summary.max(incoming.watermarks.summary);
    target.coverage.visible = target
        .coverage
        .visible
        .saturating_add(incoming.coverage.visible);
    target.coverage.hidden = target
        .coverage
        .hidden
        .saturating_add(incoming.coverage.hidden);
    target.coverage.unknown = target
        .coverage
        .unknown
        .saturating_add(incoming.coverage.unknown);
    target.coverage.redacted = target
        .coverage
        .redacted
        .saturating_add(incoming.coverage.redacted);
    if incoming.cursor.is_some() {
        target.cursor = incoming.cursor;
    }
    for item in incoming.explanations {
        if !target.explanations.contains(&item) {
            target.explanations.push(item);
        }
    }
    for item in incoming.omissions {
        if !target.omissions.contains(&item) {
            target.omissions.push(item);
        }
    }
    true
}
