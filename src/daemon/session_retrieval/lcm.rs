//! LCM describe and expansion over the canonical session retrieval owner.

use serde_json::json;
use sha2::{Digest, Sha256};
use tracedecay_application::RequestContext;
use tracedecay_domain::{
    HydrationStateV1, RetrievalAnchorId, RetrievalGrainV1, SessionId, TemporalModeV1,
};
use tracedecay_sessions::lcm::contracts::{LcmDataFreshness, LcmRetrievalOutcome};
use tracedecay_sessions::runtime::lcm::{
    LcmContentSlice, LcmDescribeRequest, LcmDescribeTarget, LcmExpandRequest, LcmExpandTarget,
};
use tracedecay_temporal_query::TemporalKernelResult;
use tracedecay_temporal_query::context::ContextBudget;
use tracedecay_temporal_query::ranking::DiversityLimits;
use tracedecay_usecases::session::{
    SessionDataFreshness, SessionRequestBinding, SessionRetrievalOutcome, SessionRetrievalScope,
    SessionTemporalExecutionError, SessionTemporalQuery,
};

use super::contract::{
    LcmDescribeServiceCommand, LcmDescribeServiceOutcome, LcmExpandServiceCommand,
    LcmExpandServiceOutcome, SessionRetrievalExplanationView, SessionRetrievalOmissionView,
    SessionRetrievalStoreScope, SessionRetrievalUnavailable, SessionRetrievalUnavailableReason,
    SessionTemporalMetadataView, SessionTemporalWatermarksView,
};
use super::{
    APPLICATION_RETRIEVAL_MAX_BYTES, DaemonSessionRetrievalService, MESSAGE_SEARCH_MAX_BYTES,
    message_search_digest,
};

impl DaemonSessionRetrievalService {
    fn lcm_authorization_binding(&self, provider: &str) -> String {
        format!(
            "sha256:{}",
            hex::encode(message_search_digest(
                b"tracedecay.mcp.lcm.authorization.v1\0",
                &self.root.identity,
                Some(provider),
            ))
        )
    }

    fn lcm_binding(
        &self,
        kind: &str,
        provider: &str,
        session_id: &SessionId,
        target: &str,
        grain: RetrievalGrainV1,
        content_slice: Option<LcmContentSlice>,
        source_limit: Option<usize>,
    ) -> String {
        let encoded = json!({
            "version": 1,
            "kind": kind,
            "provider": provider,
            "session_id": session_id.as_str(),
            "target": target,
            "grain": grain.as_str(),
            "content_offset": content_slice.map(|slice| slice.offset),
            "content_limit": content_slice.map(|slice| slice.limit),
            "source_limit": source_limit,
            "authorization": self.lcm_authorization_binding(provider),
        })
        .to_string();
        format!("sha256:{}", hex::encode(Sha256::digest(encoded.as_bytes())))
    }

    fn lcm_temporal_view(&self, result: &TemporalKernelResult) -> SessionTemporalMetadataView {
        let watermarks = result.snapshot.watermarks();
        SessionTemporalMetadataView {
            anchors: result
                .ranked
                .iter()
                .map(|ranked| ranked.anchor_id.clone())
                .collect(),
            watermarks: SessionTemporalWatermarksView {
                generation: watermarks.generation,
                source: watermarks.source,
                projection: watermarks.projection,
                index: watermarks.index,
                summary: watermarks.summary,
            },
            coverage: result.coverage,
            source_coverage: result
                .snapshot
                .source_coverage()
                .map(|receipt| receipt.sources().to_vec())
                .unwrap_or_default(),
            cursor: result.next_cursor.clone(),
            explanations: result
                .ranked
                .iter()
                .map(|ranked| SessionRetrievalExplanationView {
                    anchor: ranked.anchor_id.clone(),
                    summary: format!(
                        "temporal rank {} at {}",
                        ranked.normalized_score_micros, ranked.knowledge_at_micros
                    ),
                })
                .collect(),
            omissions: result
                .hydrated
                .iter()
                .filter(|hydrated| hydrated.state() != HydrationStateV1::Available)
                .map(|hydrated| SessionRetrievalOmissionView {
                    rank: hydrated.rank(),
                    anchor: hydrated.anchor_id().clone(),
                    reason: hydrated.state(),
                })
                .collect(),
            authorized_root: self.root.authorized_root.clone(),
        }
    }

    fn lcm_direct_query(
        &self,
        session_id: SessionId,
        provider: &str,
        grain: RetrievalGrainV1,
        temporal_mode: TemporalModeV1,
        retrieval_scope: SessionRetrievalScope,
        direct_anchor: Option<RetrievalAnchorId>,
        binding: String,
    ) -> Option<SessionTemporalQuery> {
        let query = SessionTemporalQuery::new(
            session_id,
            Some(provider.to_string()),
            "",
            None,
            temporal_mode,
            grain,
            1,
            DiversityLimits::unbounded(),
            ContextBudget {
                max_bytes: APPLICATION_RETRIEVAL_MAX_BYTES,
                max_tokens: APPLICATION_RETRIEVAL_MAX_BYTES / 4,
                estimator_version: "words-v1".to_string(),
            },
        )
        .ok()?
        .with_retrieval_scope(retrieval_scope)
        // The direct query resolves exactly one anchor and the caller then
        // slices its content, so the multi-MiB defaults buy nothing — and the
        // admitted binding rejects them outright.
        .with_execution_limits(super::admitted_execution_limits(1))
        .with_compatibility_filter_digest(binding);
        Some(match direct_anchor {
            Some(anchor_id) => query.with_direct_anchor(anchor_id),
            None => query,
        })
    }

    pub(super) async fn execute_lcm_describe_admitted(
        &self,
        context: &RequestContext,
        binding: &SessionRequestBinding,
        command: LcmDescribeServiceCommand,
    ) -> LcmDescribeServiceOutcome {
        if command.store_scope() != self.root.store_scope {
            return LcmDescribeServiceOutcome::WrongScope;
        }
        let executor = match self.registered_execution() {
            Ok(executor) => executor,
            Err(error) => {
                return describe_execution_error(
                    error,
                    self.empty_temporal(),
                    self.root.store_scope,
                );
            }
        };
        let target = command.target().clone();
        let direct_result = executor
            .resolve_lcm_describe_target(command.provider(), command.session_id(), &target)
            .await;
        let direct = match direct_result {
            Ok(direct) => direct,
            Err(error) => {
                return describe_execution_error(
                    error,
                    self.empty_temporal(),
                    self.root.store_scope,
                );
            }
        };
        let retrieval_binding = self.lcm_binding(
            "describe",
            command.provider(),
            command.session_id(),
            &lcm_describe_target_key(&target),
            command.grain(),
            None,
            None,
        );
        let temporal_mode = if direct.is_some() {
            TemporalModeV1::Current
        } else {
            TemporalModeV1::Forensic
        };
        let Some(query) = self.lcm_direct_query(
            command.session_id().clone(),
            command.provider(),
            command.grain(),
            temporal_mode,
            SessionRetrievalScope::Session(command.session_id().clone()),
            direct.as_ref().map(|direct| direct.anchor_id.clone()),
            retrieval_binding,
        ) else {
            return LcmDescribeServiceOutcome::Denied;
        };
        let outcome = self
            .execute_temporal_query_with_context(
                context,
                binding,
                query,
                "grant.application.lcm-describe",
            )
            .await;
        let (result, retrieval) = match outcome {
            SessionRetrievalOutcome::Complete {
                mut items,
                freshness,
            } => (
                items.pop(),
                LcmRetrievalOutcome::complete(lcm_data_freshness(freshness)),
            ),
            SessionRetrievalOutcome::Partial {
                mut items,
                freshness,
                omitted,
            } => {
                // A describe is a point read over a deliberately single-item
                // page, so a pagination-only partial (zero genuine omissions)
                // still yields a complete description: the rendered counts
                // come from whole-table aggregates, not from the page.
                let retrieval = if omitted == 0 {
                    LcmRetrievalOutcome::complete(lcm_data_freshness(freshness))
                } else {
                    LcmRetrievalOutcome::partial(lcm_data_freshness(freshness), omitted)
                };
                let Some(result) = items.pop() else {
                    return LcmDescribeServiceOutcome::Partial {
                        description: None,
                        temporal: self.empty_temporal(),
                        grain: command.grain(),
                        state: None,
                        lineage: Vec::new(),
                        retrieval,
                    };
                };
                (Some(result), retrieval)
            }
            SessionRetrievalOutcome::CompleteZero { freshness } if direct.is_none() => (
                None,
                LcmRetrievalOutcome::complete(lcm_data_freshness(freshness)),
            ),
            SessionRetrievalOutcome::CompleteZero { .. } => {
                return LcmDescribeServiceOutcome::Deleted;
            }
            terminal => {
                return describe_retrieval_outcome(
                    terminal,
                    command.grain(),
                    self.empty_temporal(),
                    self.root.store_scope,
                );
            }
        };
        let state = match (direct.as_ref(), result.as_ref()) {
            (Some(direct), Some(result)) => match hydration_state(result, &direct.anchor_id) {
                Some(HydrationStateV1::Available) => HydrationStateV1::Available,
                Some(state) => return describe_hydration_state(state),
                None => {
                    return LcmDescribeServiceOutcome::Unavailable(
                        SessionRetrievalUnavailable::without_worker(
                            SessionRetrievalUnavailableReason::HydrationUnavailable,
                        ),
                    );
                }
            },
            (Some(_), None) => return LcmDescribeServiceOutcome::Deleted,
            (None, _) => HydrationStateV1::Available,
        };
        let request = LcmDescribeRequest {
            provider: command.provider().to_string(),
            session_id: command.session_id().as_str().to_string(),
            target,
        };
        let rendered = executor
            .render_lcm_describe(
                request,
                result
                    .as_ref()
                    .map(|result| result.snapshot.request().execution_control()),
            )
            .await;
        let description = match rendered {
            Ok(description) => description,
            Err(error) => {
                return describe_execution_error(
                    error,
                    self.empty_temporal(),
                    self.root.store_scope,
                );
            }
        };
        let temporal = result.as_ref().map_or_else(
            || self.empty_temporal(),
            |result| self.lcm_temporal_view(result),
        );
        let lineage = result.map_or_else(Vec::new, |result| result.lineage);
        match retrieval {
            LcmRetrievalOutcome::Complete { .. } => LcmDescribeServiceOutcome::Complete {
                description,
                temporal,
                grain: command.grain(),
                state,
                lineage,
                retrieval,
            },
            LcmRetrievalOutcome::Partial { .. } => LcmDescribeServiceOutcome::Partial {
                description: Some(description),
                temporal,
                grain: command.grain(),
                state: Some(state),
                lineage,
                retrieval,
            },
            LcmRetrievalOutcome::Stale { .. } => LcmDescribeServiceOutcome::Stale {
                temporal,
                retrieval,
            },
        }
    }

    fn lcm_expand_target_key(target: &LcmExpandTarget) -> String {
        match target {
            LcmExpandTarget::RawMessage { store_id } => format!("raw:{store_id}"),
            LcmExpandTarget::SummaryNode { node_id } => format!("summary:{node_id}"),
            LcmExpandTarget::ExternalPayload { payload_ref } => {
                format!("payload:{payload_ref}")
            }
        }
    }

    pub(super) async fn execute_lcm_expand_admitted(
        &self,
        context: &RequestContext,
        binding: &SessionRequestBinding,
        command: LcmExpandServiceCommand,
    ) -> LcmExpandServiceOutcome {
        if command.store_scope() != self.root.store_scope {
            return LcmExpandServiceOutcome::WrongScope;
        }
        let executor = match self.registered_execution() {
            Ok(executor) => executor,
            Err(error) => {
                return expand_execution_error(error, self.empty_temporal(), self.root.store_scope);
            }
        };
        let target = command.target().clone();
        let direct_result = executor
            .resolve_lcm_expand_target(command.provider(), command.session_id(), &target)
            .await;
        let direct = match direct_result {
            Ok(direct) => direct,
            Err(SessionTemporalExecutionError::Deleted) if command.cursor().is_some() => {
                return LcmExpandServiceOutcome::Denied;
            }
            Err(error) => {
                return expand_execution_error(error, self.empty_temporal(), self.root.store_scope);
            }
        };
        let retrieval_binding = self.lcm_binding(
            "expand",
            command.provider(),
            command.session_id(),
            &Self::lcm_expand_target_key(&target),
            command.grain(),
            Some(command.content_slice()),
            command.source_limit(),
        );
        let retrieval_scope = if matches!(&target, LcmExpandTarget::RawMessage { .. })
            && direct.owner_session_id.as_str() != command.session_id().as_str()
        {
            SessionRetrievalScope::AllSessionsInAuthorizedRoot
        } else {
            SessionRetrievalScope::Session(command.session_id().clone())
        };
        let Some(query) = self.lcm_direct_query(
            command.session_id().clone(),
            command.provider(),
            command.grain(),
            TemporalModeV1::Current,
            retrieval_scope,
            Some(direct.anchor_id.clone()),
            retrieval_binding.clone(),
        ) else {
            return LcmExpandServiceOutcome::Denied;
        };
        let outcome = self
            .execute_temporal_query_with_context(
                context,
                binding,
                query,
                "grant.application.lcm-expand",
            )
            .await;
        let (result, retrieval) = match outcome {
            SessionRetrievalOutcome::Complete {
                mut items,
                freshness,
            } => match items.pop() {
                Some(result) => (
                    result,
                    LcmRetrievalOutcome::complete(lcm_data_freshness(freshness)),
                ),
                None => return LcmExpandServiceOutcome::Deleted,
            },
            SessionRetrievalOutcome::Partial {
                mut items,
                freshness,
                omitted,
            } => {
                let retrieval =
                    LcmRetrievalOutcome::partial(lcm_data_freshness(freshness), omitted);
                let Some(result) = items.pop() else {
                    return LcmExpandServiceOutcome::Partial {
                        expansion: None,
                        temporal: self.empty_temporal(),
                        grain: command.grain(),
                        state: None,
                        retrieval,
                    };
                };
                (result, retrieval)
            }
            SessionRetrievalOutcome::CompleteZero { .. } => {
                return LcmExpandServiceOutcome::Deleted;
            }
            terminal => {
                return expand_retrieval_outcome(
                    terminal,
                    command.grain(),
                    self.empty_temporal(),
                    self.root.store_scope,
                );
            }
        };
        let external_content = if let LcmExpandTarget::ExternalPayload { payload_ref } = &target {
            let Ok(max_bytes) = usize::try_from(MESSAGE_SEARCH_MAX_BYTES) else {
                return LcmExpandServiceOutcome::Unavailable(
                    SessionRetrievalUnavailable::without_worker(
                        SessionRetrievalUnavailableReason::HydrationUnavailable,
                    ),
                );
            };
            match executor
                .hydrate_lcm_external_payload(
                    &result.snapshot,
                    &direct.anchor_id,
                    command.provider(),
                    command.session_id(),
                    payload_ref,
                    max_bytes,
                )
                .await
            {
                Ok(content) => Some(content),
                Err(error) => {
                    return expand_execution_error(
                        error,
                        self.lcm_temporal_view(&result),
                        self.root.store_scope,
                    );
                }
            }
        } else {
            None
        };
        let canonical_content = if let Some(content) = external_content.as_deref() {
            Some(content)
        } else {
            match hydration_state(&result, &direct.anchor_id) {
                Some(HydrationStateV1::Available) => result
                    .hydrated
                    .iter()
                    .find(|hydrated| hydrated.anchor_id() == &direct.anchor_id)
                    .and_then(|hydrated| hydrated.content())
                    .and_then(|content| std::str::from_utf8(content).ok()),
                Some(state) => return expand_hydration_state(state),
                None => None,
            }
        };
        let Some(canonical_content) = canonical_content else {
            return LcmExpandServiceOutcome::Unavailable(
                SessionRetrievalUnavailable::without_worker(
                    SessionRetrievalUnavailableReason::HydrationUnavailable,
                ),
            );
        };
        let source_offset = match command.cursor() {
            Some(cursor) => match executor
                .decode_lcm_source_cursor(&result.snapshot, &retrieval_binding, cursor)
                .await
            {
                Ok(offset) => offset,
                Err(error) => {
                    return expand_execution_error(
                        error,
                        self.empty_temporal(),
                        self.root.store_scope,
                    );
                }
            },
            None => 0,
        };
        let request = LcmExpandRequest {
            provider: command.provider().to_string(),
            session_id: command.session_id().as_str().to_string(),
            target,
            content_slice: Some(command.content_slice()),
            source_offset,
            source_limit: command.source_limit(),
        };
        let rendered = executor
            .render_lcm_expand(
                request,
                canonical_content,
                result.snapshot.request().execution_control(),
            )
            .await;
        let mut expansion = match rendered {
            Ok(expansion) => expansion,
            Err(error) => {
                return expand_execution_error(error, self.empty_temporal(), self.root.store_scope);
            }
        };
        if let Err(error) = executor
            .hydrate_lcm_summary_sources(
                &result.snapshot,
                command.provider(),
                command.session_id(),
                command.content_slice(),
                &mut expansion,
            )
            .await
        {
            return expand_execution_error(error, self.empty_temporal(), self.root.store_scope);
        }
        let mut temporal = self.lcm_temporal_view(&result);
        if let Some(offset) = expansion
            .source_pagination
            .as_ref()
            .and_then(|pagination| pagination.next_source_offset)
        {
            match executor
                .encode_lcm_source_cursor(&result.snapshot, &retrieval_binding, offset)
                .await
            {
                Ok(cursor) => temporal.cursor = Some(cursor),
                Err(error) => {
                    return expand_execution_error(
                        error,
                        self.empty_temporal(),
                        self.root.store_scope,
                    );
                }
            }
        }
        let summary_source_omitted = u64::try_from(
            expansion
                .summary_sources
                .iter()
                .filter(|source| source.state != HydrationStateV1::Available)
                .count(),
        )
        .unwrap_or(u64::MAX);
        let retrieval = match retrieval {
            LcmRetrievalOutcome::Complete { freshness } if summary_source_omitted > 0 => {
                LcmRetrievalOutcome::partial(freshness, summary_source_omitted)
            }
            LcmRetrievalOutcome::Partial { freshness, omitted } => LcmRetrievalOutcome::partial(
                freshness,
                omitted.saturating_add(summary_source_omitted),
            ),
            retrieval => retrieval,
        };
        match retrieval {
            LcmRetrievalOutcome::Complete { .. } => LcmExpandServiceOutcome::Complete {
                expansion,
                temporal,
                grain: command.grain(),
                state: HydrationStateV1::Available,
                retrieval,
            },
            LcmRetrievalOutcome::Partial { .. } => LcmExpandServiceOutcome::Partial {
                expansion: Some(expansion),
                temporal,
                grain: command.grain(),
                state: Some(HydrationStateV1::Available),
                retrieval,
            },
            LcmRetrievalOutcome::Stale { .. } => LcmExpandServiceOutcome::Stale {
                temporal,
                retrieval,
            },
        }
    }
}

fn lcm_describe_target_key(target: &LcmDescribeTarget) -> String {
    match target {
        LcmDescribeTarget::Session => "session".to_string(),
        LcmDescribeTarget::SummaryNode { node_id } => format!("summary:{node_id}"),
        LcmDescribeTarget::ExternalPayload { payload_ref } => format!("payload:{payload_ref}"),
    }
}

const fn lcm_data_freshness(freshness: SessionDataFreshness) -> LcmDataFreshness {
    match freshness {
        SessionDataFreshness::Fresh => LcmDataFreshness::Fresh,
        SessionDataFreshness::Stored { generation_lag } => {
            LcmDataFreshness::Stored { generation_lag }
        }
        SessionDataFreshness::Partial { generation_lag } => {
            LcmDataFreshness::Partial { generation_lag }
        }
    }
}

fn hydration_state(
    result: &TemporalKernelResult,
    anchor_id: &RetrievalAnchorId,
) -> Option<HydrationStateV1> {
    result
        .hydrated
        .iter()
        .find(|hydrated| hydrated.anchor_id() == anchor_id)
        .map(tracedecay_temporal_query::TemporalHydratedResult::state)
}

fn describe_hydration_state(state: HydrationStateV1) -> LcmDescribeServiceOutcome {
    match state {
        HydrationStateV1::Locked => LcmDescribeServiceOutcome::Locked,
        HydrationStateV1::Redacted => LcmDescribeServiceOutcome::Redacted,
        HydrationStateV1::Deleted | HydrationStateV1::RetentionExpired => {
            LcmDescribeServiceOutcome::Deleted
        }
        HydrationStateV1::Unauthorized => LcmDescribeServiceOutcome::Denied,
        HydrationStateV1::Available
        | HydrationStateV1::RetainedButUnavailable
        | HydrationStateV1::UnverifiableLegacy => {
            LcmDescribeServiceOutcome::Unavailable(SessionRetrievalUnavailable::without_worker(
                SessionRetrievalUnavailableReason::HydrationUnavailable,
            ))
        }
    }
}

fn expand_hydration_state(state: HydrationStateV1) -> LcmExpandServiceOutcome {
    match state {
        HydrationStateV1::Locked => LcmExpandServiceOutcome::Locked,
        HydrationStateV1::Redacted => LcmExpandServiceOutcome::Redacted,
        HydrationStateV1::Deleted | HydrationStateV1::RetentionExpired => {
            LcmExpandServiceOutcome::Deleted
        }
        HydrationStateV1::Unauthorized => LcmExpandServiceOutcome::Denied,
        HydrationStateV1::Available
        | HydrationStateV1::RetainedButUnavailable
        | HydrationStateV1::UnverifiableLegacy => {
            LcmExpandServiceOutcome::Unavailable(SessionRetrievalUnavailable::without_worker(
                SessionRetrievalUnavailableReason::HydrationUnavailable,
            ))
        }
    }
}

fn describe_execution_error(
    error: SessionTemporalExecutionError,
    temporal: SessionTemporalMetadataView,
    store_scope: SessionRetrievalStoreScope,
) -> LcmDescribeServiceOutcome {
    match error {
        SessionTemporalExecutionError::Locked => LcmDescribeServiceOutcome::Locked,
        SessionTemporalExecutionError::Redacted => LcmDescribeServiceOutcome::Redacted,
        SessionTemporalExecutionError::Deleted => LcmDescribeServiceOutcome::Deleted,
        SessionTemporalExecutionError::WrongScope => LcmDescribeServiceOutcome::WrongScope,
        SessionTemporalExecutionError::Denied => LcmDescribeServiceOutcome::Denied,
        SessionTemporalExecutionError::ResetRequired => {
            LcmDescribeServiceOutcome::ResetRequired { store_scope }
        }
        SessionTemporalExecutionError::BudgetExhausted => {
            LcmDescribeServiceOutcome::BudgetExhausted
        }
        SessionTemporalExecutionError::Cancelled => LcmDescribeServiceOutcome::Cancelled,
        SessionTemporalExecutionError::Stale { generation_lag } => {
            LcmDescribeServiceOutcome::Stale {
                temporal,
                retrieval: LcmRetrievalOutcome::stale(LcmDataFreshness::Stored { generation_lag }),
            }
        }
        SessionTemporalExecutionError::Unavailable
        | SessionTemporalExecutionError::Empty { .. }
        | SessionTemporalExecutionError::Kernel(_) => {
            LcmDescribeServiceOutcome::Unavailable(SessionRetrievalUnavailable::without_worker(
                SessionRetrievalUnavailableReason::TemporalStoreUnavailable,
            ))
        }
    }
}

fn expand_execution_error(
    error: SessionTemporalExecutionError,
    temporal: SessionTemporalMetadataView,
    store_scope: SessionRetrievalStoreScope,
) -> LcmExpandServiceOutcome {
    match error {
        SessionTemporalExecutionError::Locked => LcmExpandServiceOutcome::Locked,
        SessionTemporalExecutionError::Redacted => LcmExpandServiceOutcome::Redacted,
        SessionTemporalExecutionError::Deleted => LcmExpandServiceOutcome::Deleted,
        SessionTemporalExecutionError::WrongScope => LcmExpandServiceOutcome::WrongScope,
        SessionTemporalExecutionError::Denied => LcmExpandServiceOutcome::Denied,
        SessionTemporalExecutionError::ResetRequired => {
            LcmExpandServiceOutcome::ResetRequired { store_scope }
        }
        SessionTemporalExecutionError::BudgetExhausted => LcmExpandServiceOutcome::BudgetExhausted,
        SessionTemporalExecutionError::Cancelled => LcmExpandServiceOutcome::Cancelled,
        SessionTemporalExecutionError::Stale { generation_lag } => LcmExpandServiceOutcome::Stale {
            temporal,
            retrieval: LcmRetrievalOutcome::stale(LcmDataFreshness::Stored { generation_lag }),
        },
        SessionTemporalExecutionError::Unavailable
        | SessionTemporalExecutionError::Empty { .. }
        | SessionTemporalExecutionError::Kernel(_) => {
            LcmExpandServiceOutcome::Unavailable(SessionRetrievalUnavailable::without_worker(
                SessionRetrievalUnavailableReason::TemporalStoreUnavailable,
            ))
        }
    }
}

pub(super) fn describe_retrieval_outcome(
    outcome: SessionRetrievalOutcome<TemporalKernelResult>,
    grain: RetrievalGrainV1,
    temporal: SessionTemporalMetadataView,
    store_scope: SessionRetrievalStoreScope,
) -> LcmDescribeServiceOutcome {
    match outcome {
        SessionRetrievalOutcome::WrongScope => LcmDescribeServiceOutcome::WrongScope,
        SessionRetrievalOutcome::Locked => LcmDescribeServiceOutcome::Locked,
        SessionRetrievalOutcome::Redacted => LcmDescribeServiceOutcome::Redacted,
        SessionRetrievalOutcome::Deleted => LcmDescribeServiceOutcome::Deleted,
        SessionRetrievalOutcome::Denied => LcmDescribeServiceOutcome::Denied,
        SessionRetrievalOutcome::ResetRequired => {
            LcmDescribeServiceOutcome::ResetRequired { store_scope }
        }
        SessionRetrievalOutcome::BudgetExhausted => LcmDescribeServiceOutcome::BudgetExhausted,
        SessionRetrievalOutcome::CursorManifestLimitExceeded { .. } => {
            LcmDescribeServiceOutcome::BudgetExhausted
        }
        SessionRetrievalOutcome::Cancelled => LcmDescribeServiceOutcome::Cancelled,
        SessionRetrievalOutcome::Stale { freshness } => LcmDescribeServiceOutcome::Stale {
            temporal,
            retrieval: LcmRetrievalOutcome::stale(lcm_data_freshness(freshness)),
        },
        SessionRetrievalOutcome::Partial {
            freshness, omitted, ..
        } => LcmDescribeServiceOutcome::Partial {
            description: None,
            temporal,
            grain,
            state: None,
            lineage: Vec::new(),
            retrieval: LcmRetrievalOutcome::partial(lcm_data_freshness(freshness), omitted),
        },
        SessionRetrievalOutcome::Unavailable
        | SessionRetrievalOutcome::Complete { .. }
        | SessionRetrievalOutcome::CompleteZero { .. } => {
            LcmDescribeServiceOutcome::Unavailable(SessionRetrievalUnavailable::without_worker(
                SessionRetrievalUnavailableReason::TemporalStoreUnavailable,
            ))
        }
    }
}

pub(super) fn expand_retrieval_outcome(
    outcome: SessionRetrievalOutcome<TemporalKernelResult>,
    grain: RetrievalGrainV1,
    temporal: SessionTemporalMetadataView,
    store_scope: SessionRetrievalStoreScope,
) -> LcmExpandServiceOutcome {
    match outcome {
        SessionRetrievalOutcome::WrongScope => LcmExpandServiceOutcome::WrongScope,
        SessionRetrievalOutcome::Locked => LcmExpandServiceOutcome::Locked,
        SessionRetrievalOutcome::Redacted => LcmExpandServiceOutcome::Redacted,
        SessionRetrievalOutcome::Deleted => LcmExpandServiceOutcome::Deleted,
        SessionRetrievalOutcome::Denied => LcmExpandServiceOutcome::Denied,
        SessionRetrievalOutcome::ResetRequired => {
            LcmExpandServiceOutcome::ResetRequired { store_scope }
        }
        SessionRetrievalOutcome::BudgetExhausted => LcmExpandServiceOutcome::BudgetExhausted,
        SessionRetrievalOutcome::CursorManifestLimitExceeded { .. } => {
            LcmExpandServiceOutcome::BudgetExhausted
        }
        SessionRetrievalOutcome::Cancelled => LcmExpandServiceOutcome::Cancelled,
        SessionRetrievalOutcome::Stale { freshness } => LcmExpandServiceOutcome::Stale {
            temporal,
            retrieval: LcmRetrievalOutcome::stale(lcm_data_freshness(freshness)),
        },
        SessionRetrievalOutcome::Partial {
            freshness, omitted, ..
        } => LcmExpandServiceOutcome::Partial {
            expansion: None,
            temporal,
            grain,
            state: None,
            retrieval: LcmRetrievalOutcome::partial(lcm_data_freshness(freshness), omitted),
        },
        SessionRetrievalOutcome::Unavailable
        | SessionRetrievalOutcome::Complete { .. }
        | SessionRetrievalOutcome::CompleteZero { .. } => {
            LcmExpandServiceOutcome::Unavailable(SessionRetrievalUnavailable::without_worker(
                SessionRetrievalUnavailableReason::TemporalStoreUnavailable,
            ))
        }
    }
}
