#[cfg(test)]
use super::compression_policy::DEFAULT_INCREMENTAL_MAX_DEPTH;
pub(super) use super::compression_policy::threshold_pressure;
pub use super::compression_policy::{
    AssemblyCapInput, OverflowRecoveryCapInput, bounded_leaf_chunk_len,
    effective_assembly_token_cap, effective_leaf_chunk_tokens, has_eligible_backlog,
    overflow_recovery_assembly_cap, progress_leaf_chunk_len,
};
pub use super::compression_policy::{
    CondensationCandidateDecision, CondensationDecision, CondensationPolicy,
    CondensationSkipReason, condensation_candidate_decision, incremental_max_depth_limit,
};
use super::compression_policy::{forced_overflow_pressure, source_token_count};
use super::summarizer::CompressionSummarizerAdapter;
use super::{
    LCM_COMPRESSION_BOUNDARY_COOLDOWN_SECONDS, LCM_DEFAULT_SUMMARY_FAN_IN, LcmCompressionRequest,
    LcmLifecycleState, LcmMaintenanceDebt, LcmPreflightRequest, LcmRawMessage,
    LcmSessionBoundaryRequest,
};

#[derive(Debug, Clone, Copy)]
pub struct PreflightDecisionInput<'a> {
    pub request: &'a LcmPreflightRequest,
    pub frontier: &'a LcmLifecycleState,
    pub backlog: &'a [LcmRawMessage],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreflightDecision {
    pub should_compress: bool,
    pub reason: &'static str,
}

#[derive(Debug, Clone, Copy)]
pub struct CompressionPlanInput<'a> {
    pub request: &'a LcmCompressionRequest,
    pub backlog: &'a [LcmRawMessage],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompressionPlan {
    pub selected_backlog: Vec<LcmRawMessage>,
    pub forced_overflow_recovery: bool,
    pub leaf_chunk_tokens: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoundaryTransitionDecision {
    Ignore,
    CarryOver { old_session_id: String },
    StartCooldown { boundary_skip_at: i64 },
}

#[derive(Debug, Clone, Copy)]
pub struct CondensationDecisionInput<'a> {
    pub has_backlog: bool,
    pub summary_fan_in: Option<usize>,
    pub incremental_max_depth: Option<i64>,
    pub summarizer: &'a CompressionSummarizerAdapter,
}

pub fn boundary_transition_decision(
    request: &LcmSessionBoundaryRequest,
    now: i64,
) -> BoundaryTransitionDecision {
    let old_session_id = request.old_session_id.as_deref().unwrap_or("");
    let is_compression_boundary = request.boundary_reason.as_deref() == Some("compression")
        && !old_session_id.is_empty()
        && old_session_id != request.session_id;
    if !is_compression_boundary {
        return BoundaryTransitionDecision::Ignore;
    }
    if request.bound_session_id.as_deref() == Some(old_session_id) {
        return BoundaryTransitionDecision::CarryOver {
            old_session_id: old_session_id.to_string(),
        };
    }
    BoundaryTransitionDecision::StartCooldown {
        boundary_skip_at: request.boundary_skip_at.unwrap_or(now),
    }
}

pub fn cooldown_active(boundary_skip_at: Option<i64>, now: i64) -> bool {
    match boundary_skip_at {
        Some(boundary_skip_at) => {
            now - boundary_skip_at < LCM_COMPRESSION_BOUNDARY_COOLDOWN_SECONDS
        }
        None => false,
    }
}

pub fn condensation_policy_decision(input: CondensationDecisionInput<'_>) -> CondensationDecision {
    if input.has_backlog {
        return CondensationDecision::Skip(CondensationSkipReason::BacklogPresent);
    }
    if input.summarizer.persisted_summary_invocation().is_none() {
        return CondensationDecision::Skip(CondensationSkipReason::AuxiliarySummarizer);
    }
    CondensationDecision::QueryCandidates(CondensationPolicy {
        fan_in: input
            .summary_fan_in
            .filter(|fan_in| *fan_in > 1)
            .unwrap_or(LCM_DEFAULT_SUMMARY_FAN_IN),
        incremental_max_depth: incremental_max_depth_limit(input.incremental_max_depth),
    })
}

pub fn preflight_decision(input: PreflightDecisionInput<'_>) -> PreflightDecision {
    if forced_overflow_pressure(
        input.request.current_tokens,
        input.request.max_assembly_tokens,
    ) {
        return PreflightDecision {
            should_compress: true,
            reason: "forced_overflow_pressure",
        };
    }

    if frontier_has_maintenance_debt(input.frontier) {
        return PreflightDecision {
            should_compress: true,
            reason: "maintenance_debt_ready",
        };
    }

    if threshold_pressure(input.request.current_tokens, input.request.threshold_tokens) {
        if input.backlog.is_empty() {
            return PreflightDecision {
                should_compress: false,
                reason: "threshold_no_eligible_backlog",
            };
        }
        let leaf_chunk_tokens = effective_leaf_chunk_tokens(
            input.request.leaf_chunk_tokens,
            input.request.dynamic_leaf_chunk_enabled,
            input.request.dynamic_leaf_chunk_max,
            source_token_count(input.backlog),
        );
        if has_eligible_backlog(input.backlog, leaf_chunk_tokens) {
            return PreflightDecision {
                should_compress: true,
                reason: "threshold_backlog_ready",
            };
        }
        return PreflightDecision {
            should_compress: false,
            reason: "threshold_no_eligible_backlog",
        };
    }

    PreflightDecision {
        should_compress: false,
        reason: "no_compression_needed",
    }
}

pub fn compression_plan(input: CompressionPlanInput<'_>) -> CompressionPlan {
    let forced_overflow_recovery = should_force_overflow_recovery(input.request);
    let leaf_chunk_tokens = effective_leaf_chunk_tokens(
        input.request.leaf_chunk_tokens,
        input.request.dynamic_leaf_chunk_enabled,
        input.request.dynamic_leaf_chunk_max,
        source_token_count(input.backlog),
    );
    let selected_len = progress_leaf_chunk_len(
        input.backlog,
        leaf_chunk_tokens,
        input.request.max_source_messages,
    );
    CompressionPlan {
        selected_backlog: input.backlog[..selected_len].to_vec(),
        forced_overflow_recovery,
        leaf_chunk_tokens,
    }
}

pub fn frontier_has_maintenance_debt(frontier: &LcmLifecycleState) -> bool {
    frontier
        .maintenance_debt
        .iter()
        .any(|debt| matches!(debt, LcmMaintenanceDebt::RawBacklog { .. }))
}

fn should_force_overflow_recovery(request: &LcmCompressionRequest) -> bool {
    forced_overflow_pressure(request.current_tokens, request.max_assembly_tokens)
}

#[cfg(test)]
mod tests {
    use super::super::LcmSummarizerMode;
    use super::*;

    fn boundary_request() -> LcmSessionBoundaryRequest {
        LcmSessionBoundaryRequest {
            provider: "cursor".to_string(),
            session_id: "session-b".to_string(),
            old_session_id: Some("session-a".to_string()),
            boundary_reason: Some("compression".to_string()),
            bound_session_id: Some("session-z".to_string()),
            boundary_skip_at: None,
        }
    }

    #[test]
    fn boundary_transition_ignores_non_compression_boundaries() {
        let mut request = boundary_request();
        request.boundary_reason = Some("manual".to_string());
        assert_eq!(
            boundary_transition_decision(&request, 1_000),
            BoundaryTransitionDecision::Ignore
        );

        request.boundary_reason = Some("compression".to_string());
        request.old_session_id = Some(request.session_id.clone());
        assert_eq!(
            boundary_transition_decision(&request, 1_000),
            BoundaryTransitionDecision::Ignore
        );
    }

    #[test]
    fn boundary_transition_carries_over_matching_bound_session() {
        let mut request = boundary_request();
        request.bound_session_id = request.old_session_id.clone();
        assert_eq!(
            boundary_transition_decision(&request, 1_000),
            BoundaryTransitionDecision::CarryOver {
                old_session_id: "session-a".to_string(),
            }
        );
    }

    #[test]
    fn boundary_transition_starts_cooldown_for_skipped_carry_over() {
        let request = boundary_request();
        assert_eq!(
            boundary_transition_decision(&request, 1_000),
            BoundaryTransitionDecision::StartCooldown {
                boundary_skip_at: 1_000,
            }
        );

        let mut explicit = boundary_request();
        explicit.boundary_skip_at = Some(777);
        assert_eq!(
            boundary_transition_decision(&explicit, 1_000),
            BoundaryTransitionDecision::StartCooldown {
                boundary_skip_at: 777,
            }
        );
    }

    #[test]
    fn cooldown_active_holds_for_sixty_seconds_then_expires() {
        let now = 10_000;
        assert!(cooldown_active(Some(now), now));
        assert!(cooldown_active(
            Some(now - (LCM_COMPRESSION_BOUNDARY_COOLDOWN_SECONDS - 1)),
            now,
        ));
        assert!(!cooldown_active(
            Some(now - LCM_COMPRESSION_BOUNDARY_COOLDOWN_SECONDS),
            now,
        ));
        assert!(!cooldown_active(None, now));
    }

    #[test]
    fn condensation_policy_uses_defaults_for_regular_summarizers() {
        let summarizer = CompressionSummarizerAdapter::from_mode(LcmSummarizerMode::Fake {
            summary_text: "summary".to_string(),
        });
        assert_eq!(
            condensation_policy_decision(CondensationDecisionInput {
                has_backlog: false,
                summary_fan_in: Some(1),
                incremental_max_depth: None,
                summarizer: &summarizer,
            }),
            CondensationDecision::QueryCandidates(CondensationPolicy {
                fan_in: LCM_DEFAULT_SUMMARY_FAN_IN,
                incremental_max_depth: DEFAULT_INCREMENTAL_MAX_DEPTH,
            })
        );
    }

    #[test]
    fn condensation_policy_honors_overrides_and_negative_depth() {
        let summarizer = CompressionSummarizerAdapter::from_mode(LcmSummarizerMode::Provided {
            summary_text: "summary".to_string(),
            route: None,
        });
        assert_eq!(
            condensation_policy_decision(CondensationDecisionInput {
                has_backlog: false,
                summary_fan_in: Some(6),
                incremental_max_depth: Some(-1),
                summarizer: &summarizer,
            }),
            CondensationDecision::QueryCandidates(CondensationPolicy {
                fan_in: 6,
                incremental_max_depth: i64::MAX,
            })
        );
    }

    #[test]
    fn condensation_policy_skips_backlog_and_auxiliary_modes() {
        let persisted = CompressionSummarizerAdapter::from_mode(LcmSummarizerMode::Fake {
            summary_text: "summary".to_string(),
        });
        assert_eq!(
            condensation_policy_decision(CondensationDecisionInput {
                has_backlog: true,
                summary_fan_in: None,
                incremental_max_depth: None,
                summarizer: &persisted,
            }),
            CondensationDecision::Skip(CondensationSkipReason::BacklogPresent)
        );

        let auxiliary = CompressionSummarizerAdapter::from_mode(LcmSummarizerMode::HermesAuxiliary);
        assert_eq!(
            condensation_policy_decision(CondensationDecisionInput {
                has_backlog: false,
                summary_fan_in: None,
                incremental_max_depth: None,
                summarizer: &auxiliary,
            }),
            CondensationDecision::Skip(CondensationSkipReason::AuxiliarySummarizer)
        );
    }

    #[test]
    fn condensation_candidate_decision_requires_fan_in_children() {
        assert_eq!(
            condensation_candidate_decision(2, 3),
            CondensationCandidateDecision::SkipNotEnoughCandidates
        );
        assert_eq!(
            condensation_candidate_decision(3, 3),
            CondensationCandidateDecision::Condense
        );
    }

    #[test]
    fn preflight_forced_overflow_beats_maintenance_debt() {
        let request = LcmPreflightRequest {
            provider: "cursor".to_string(),
            session_id: "session-1".to_string(),
            messages: Vec::new(),
            current_tokens: Some(12),
            threshold_tokens: None,
            max_assembly_tokens: Some(8),
            leaf_chunk_tokens: None,
            max_source_messages: None,
            summary_fan_in: None,
            incremental_max_depth: None,
            fresh_tail_count: None,
            dynamic_leaf_chunk_enabled: None,
            dynamic_leaf_chunk_max: None,
            context_length: None,
            reserve_tokens_floor: None,
            ignore_session_patterns: Vec::new(),
            stateless_session_patterns: Vec::new(),
        };
        let frontier = LcmLifecycleState {
            provider: "cursor".to_string(),
            conversation_id: "session-1".to_string(),
            current_session_id: "session-1".to_string(),
            current_frontier_store_id: None,
            last_finalized_session_id: None,
            last_finalized_frontier_store_id: None,
            maintenance_debt: vec![LcmMaintenanceDebt::RawBacklog {
                from_store_id: 1,
                to_store_id: 1,
            }],
        };
        let backlog = vec![LcmRawMessage {
            provider: "cursor".to_string(),
            message_id: "m1".to_string(),
            session_id: "session-1".to_string(),
            store_id: 1,
            role: "assistant".to_string(),
            ordinal: 1,
            timestamp: Some(1),
            content: "123456".to_string(),
            content_hash: "hash".to_string(),
            storage_kind: super::super::LcmStorageKind::Inline,
            payload_ref: None,
            legacy_source: false,
            legacy_truncated: false,
            metadata_json: None,
        }];

        let decision = preflight_decision(PreflightDecisionInput {
            request: &request,
            frontier: &frontier,
            backlog: &backlog,
        });

        assert!(decision.should_compress);
        assert_eq!(decision.reason, "forced_overflow_pressure");
    }
}
