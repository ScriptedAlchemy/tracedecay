//! `tracedecay_similar` and `tracedecay_redundancy`: the clone-family reads.

use serde_json::Value;
use tracedecay_contracts::graph_tool::{GraphToolCompletionV1, GraphToolResultV1};
use tracedecay_contracts::retrieval::{
    RedundancyScopeV1, RedundancySurfaceRequestV1, SimilarCoverageV1, SimilarFamilyV1,
    SimilarMatchClassV1, SimilarOccurrenceV1, SimilarResultV1, SimilarSurfaceRequestV1,
    SimilarTargetV1,
};
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_domain::{CursorBindingV1, decode_bound_cursor, encode_bound_cursor};

use crate::McpToolContext;
use crate::handlers::support::decode_primitive_request;
use crate::tool_errors::cursor_refusal;

use super::graph_tool_completion;

#[tracing::instrument(name = "mcp.graph.similar.total", level = "trace", skip_all)]
pub async fn compute_similar(
    ctx: &McpToolContext<'_>,
    args: Value,
) -> Result<GraphToolCompletionV1> {
    let request: SimilarSurfaceRequestV1 = decode_primitive_request(&args, "tracedecay_similar")?;
    // The target is bound through the source it resolves to, inside the
    // clone cursor: a range and the occurrence it names page one family.
    let cursor_binding = CursorBindingV1::builder("similar")
        .parameter("match_classes", &request.match_classes)
        .parameter("result_limit", &request.result_limit)
        .build()
        .map_err(|error| TraceDecayError::Config {
            message: format!("failed to bind tracedecay_similar cursor: {error}"),
        })?;
    let project_id = request.project_id;
    let repository_id = request.repository_id;
    let target = match request.target {
        SimilarTargetV1::SymbolOccurrence {
            symbol_occurrence_id,
        } => tracedecay_query::code_search::CodeIndexSimilarTargetV1::SymbolOccurrence(
            symbol_occurrence_id,
        ),
        SimilarTargetV1::SourceRange { path, span } => {
            tracedecay_query::code_search::CodeIndexSimilarTargetV1::SourceRange { path, span }
        }
    };
    let match_classes = request
        .match_classes
        .iter()
        .map(|class| match class {
            SimilarMatchClassV1::ConservativeExact => {
                tracedecay_code_index::clones::CloneNormalizationClassV1::Conservative
            }
            SimilarMatchClassV1::RenameNormalizedExact => {
                tracedecay_code_index::clones::CloneNormalizationClassV1::Rename
            }
        })
        .collect();
    let cursor = request
        .cursor
        .as_deref()
        .map(|encoded| decode_bound_cursor(&cursor_binding, encoded))
        .transpose()
        .map_err(|mismatch| cursor_refusal(&mismatch))?;
    let executor = ctx.code_index_similar_executor().ok_or_else(|| {
        clone_lane_unavailable_error(
            "similarity",
            tracedecay_query::code_search::CodeIndexSearchUnavailableReasonV1::CapabilityUnavailable,
        )
    })?;
    let similar = match executor(tracedecay_query::code_search::CodeIndexSimilarRequestV1 {
        project_root: ctx.project_root().to_path_buf(),
        target,
        match_classes,
        result_limit: request.result_limit as usize,
        work_limit: request.work_limit as usize,
        cursor,
        authority: ctx.code_index_search_authority().cloned(),
        deadline: ctx.deadline().cloned(),
        cancellation: ctx.cancellation().cloned(),
    })
    .await
    {
        tracedecay_query::code_search::CodeIndexSimilarOutcomeV1::Complete(similar) => *similar,
        tracedecay_query::code_search::CodeIndexSimilarOutcomeV1::NotFound => {
            return Err(TraceDecayError::project_route(
                "application_surface_not_found_or_not_authorized",
                false,
                "the selected source has no body in the verified clone index",
            ));
        }
        tracedecay_query::code_search::CodeIndexSimilarOutcomeV1::Unavailable(reason) => {
            return Err(clone_lane_unavailable_error("similarity", reason));
        }
    };
    if similar.source.occurrence.project_id != project_id
        || similar.source.occurrence.repository_id != repository_id
    {
        return Err(TraceDecayError::project_route(
            "application_surface_not_found_or_not_authorized",
            false,
            "the selected source is outside the authorized repository scope",
        ));
    }
    let source = similar_occurrence(&similar.source.occurrence);
    let mut touched_files = vec![source.path.clone()];
    let mut complete = true;
    let families = similar
        .exact_groups
        .into_iter()
        .map(|group| -> Result<SimilarFamilyV1> {
            complete &= group.complete;
            let match_class = match group.key.class {
                tracedecay_code_index::clones::CloneNormalizationClassV1::Conservative => {
                    SimilarMatchClassV1::ConservativeExact
                }
                tracedecay_code_index::clones::CloneNormalizationClassV1::Rename => {
                    SimilarMatchClassV1::RenameNormalizedExact
                }
            };
            let members = group
                .members
                .into_iter()
                .filter(|member| {
                    member.occurrence.project_id == project_id
                        && member.occurrence.repository_id == repository_id
                })
                .map(|member| {
                    let occurrence = similar_occurrence(&member.occurrence);
                    touched_files.push(occurrence.path.clone());
                    occurrence
                })
                .collect::<Vec<_>>();
            let next_cursor = group
                .next_cursor
                .as_ref()
                .map(|position| encode_bound_cursor(&cursor_binding, position))
                .transpose()
                .map_err(|error| TraceDecayError::Config {
                    message: format!("failed to encode tracedecay_similar cursor: {error}"),
                })?;
            Ok(SimilarFamilyV1 {
                match_class,
                normalization_revision: group.key.normalization_revision,
                family_digest: group.key.digest,
                representative_payload_digest: similar.source.payload.payload_digest.clone(),
                member_count: members.len(),
                members,
                complete: group.complete,
                next_cursor,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    touched_files.sort();
    touched_files.dedup();
    let coverage = match similar.source.occurrence.eligibility {
        tracedecay_code_index::clones::CloneBodyEligibilityV1::Eligible if complete => {
            SimilarCoverageV1::Complete
        }
        tracedecay_code_index::clones::CloneBodyEligibilityV1::Eligible => {
            SimilarCoverageV1::Partial
        }
        tracedecay_code_index::clones::CloneBodyEligibilityV1::ExcludedTooSmall {
            minimum_tokens,
        } => SimilarCoverageV1::ExcludedTooSmall { minimum_tokens },
        tracedecay_code_index::clones::CloneBodyEligibilityV1::ExcludedTooLarge {
            maximum_tokens,
            maximum_bytes,
        } => SimilarCoverageV1::ExcludedTooLarge {
            maximum_tokens,
            maximum_bytes,
        },
        tracedecay_code_index::clones::CloneBodyEligibilityV1::ExcludedIncompleteTokenization => {
            SimilarCoverageV1::ExcludedIncompleteTokenization
        }
    };
    let result = SimilarResultV1 {
        source_generation: source.source_generation.clone(),
        source,
        families,
        coverage,
        freshness: None,
    };
    Ok(graph_tool_completion(
        GraphToolResultV1::Similar(result),
        touched_files,
    ))
}

/// The one clone-family unavailable wire shape. `tracedecay_similar` and
/// `tracedecay_redundancy` read the same executor vocabulary, so they report
/// the same `reason_code` and the same retry verdict; only the human lane name
/// in `detail` differs.
///
/// Deliberately does **not** re-emit retired opaque tokens
/// (`verified-code-similarity-unavailable` /
/// `verified-code-redundancy-unavailable`). Those never shipped on master; the
/// shared `reason.as_str()` vocabulary is the sole consumer-visible code.
fn clone_lane_unavailable_error(
    lane: &str,
    reason: tracedecay_query::code_search::CodeIndexSearchUnavailableReasonV1,
) -> TraceDecayError {
    if reason == tracedecay_query::code_search::CodeIndexSearchUnavailableReasonV1::InvalidRequest {
        return TraceDecayError::project_route(
            "application_surface_invalid_request",
            false,
            format!("the maintained clone {lane} lane refused the request as invalid"),
        );
    }
    TraceDecayError::ProjectRoute {
        reason_code: reason.as_str().to_owned(),
        retryable: reason.is_retryable(),
        detail: format!(
            "the maintained clone {lane} lane is unavailable: {}",
            reason.as_str()
        ),
        typed_detail: None,
    }
}

#[tracing::instrument(name = "mcp.graph.redundancy.total", level = "trace", skip_all)]
pub async fn compute_redundancy(
    ctx: &McpToolContext<'_>,
    args: Value,
) -> Result<GraphToolCompletionV1> {
    let request: RedundancySurfaceRequestV1 =
        decode_primitive_request(&args, "tracedecay_redundancy")?;
    let cursor_binding = CursorBindingV1::builder("redundancy")
        .parameter("match_classes", &request.match_classes)
        .parameter("scope", &request.scope)
        .parameter("include_generated_paths", &request.include_generated_paths)
        .parameter("family_limit", &request.family_limit)
        .parameter("member_limit", &request.member_limit)
        .build()
        .map_err(|error| TraceDecayError::Config {
            message: format!("failed to bind tracedecay_redundancy cursor: {error}"),
        })?;
    let cursor = request
        .cursor
        .as_deref()
        .map(|encoded| decode_bound_cursor::<String>(&cursor_binding, encoded))
        .transpose()
        .map_err(|mismatch| cursor_refusal(&mismatch))?;
    if request.project_id != ctx.admitted_scope().project_id
        || request.repository_id != ctx.admitted_scope().repository_id
    {
        return Err(TraceDecayError::project_route(
            "application_surface_not_found_or_not_authorized",
            false,
            "the selected repository is outside the authorized repository scope",
        ));
    }
    let match_classes = request
        .match_classes
        .iter()
        .map(|class| match class {
            SimilarMatchClassV1::ConservativeExact => {
                tracedecay_code_index::clones::CloneNormalizationClassV1::Conservative
            }
            SimilarMatchClassV1::RenameNormalizedExact => {
                tracedecay_code_index::clones::CloneNormalizationClassV1::Rename
            }
        })
        .collect();
    let scope = match request.scope {
        RedundancyScopeV1::Repository => {
            tracedecay_query::code_search::CodeIndexRedundancyScopeV1::Repository
        }
        RedundancyScopeV1::Path { path } => {
            tracedecay_query::code_search::CodeIndexRedundancyScopeV1::Path(path)
        }
        RedundancyScopeV1::PullRequest {
            provider,
            pull_request_id,
            head_commit_id,
            mut changed_paths,
        } => {
            changed_paths.sort();
            changed_paths.dedup();
            let pull_request_id = tracedecay_domain::feedback::GitHubPullRequestIdV1::new(
                pull_request_id,
            )
            .map_err(|error| TraceDecayError::Config {
                message: format!("invalid arguments for tracedecay_redundancy: {error}"),
            })?;
            tracedecay_query::code_search::CodeIndexRedundancyScopeV1::PullRequest {
                provider,
                pull_request_id,
                head_commit_id,
                changed_paths,
            }
        }
    };
    let executor = ctx.code_index_redundancy_executor().ok_or_else(|| {
        clone_lane_unavailable_error(
            "family",
            tracedecay_query::code_search::CodeIndexSearchUnavailableReasonV1::CapabilityUnavailable,
        )
    })?;
    let mut outcome = executor(tracedecay_query::code_search::CodeIndexRedundancyQueryV1 {
        project_root: ctx.project_root().to_path_buf(),
        project_id: request.project_id,
        repository_id: request.repository_id,
        match_classes,
        scope,
        include_generated_paths: request.include_generated_paths,
        family_limit: request.family_limit as usize,
        member_limit: request.member_limit as usize,
        work_limit: request.work_limit as usize,
        cursor,
        authority: ctx.code_index_search_authority().cloned(),
        deadline: ctx.deadline().cloned(),
        cancellation: ctx.cancellation().cloned(),
    })
    .await
    .map_err(|reason| clone_lane_unavailable_error("family", reason))?;
    outcome.next_cursor = outcome
        .next_cursor
        .as_ref()
        .map(|position| encode_bound_cursor(&cursor_binding, position))
        .transpose()
        .map_err(|error| TraceDecayError::Config {
            message: format!("failed to encode tracedecay_redundancy cursor: {error}"),
        })?;
    let mut touched_files = outcome
        .families
        .iter()
        .flat_map(|group| group.family.members.iter())
        .map(|member| member.path.clone())
        .collect::<Vec<_>>();
    touched_files.sort();
    touched_files.dedup();
    Ok(graph_tool_completion(
        GraphToolResultV1::Redundancy(outcome),
        touched_files,
    ))
}

fn similar_occurrence(
    occurrence: &tracedecay_code_index::clones::CloneBodyOccurrenceV1,
) -> SimilarOccurrenceV1 {
    SimilarOccurrenceV1 {
        project_id: occurrence.project_id.clone(),
        repository_id: occurrence.repository_id.clone(),
        worktree_id: occurrence.worktree_id.clone(),
        source_generation: occurrence.source_generation.clone(),
        snapshot_digest: occurrence.snapshot_digest.clone(),
        symbol_occurrence_id: occurrence.symbol_occurrence_id.clone(),
        path: occurrence.path.clone(),
        body_span: occurrence.body_span,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn clone_lanes_report_one_unavailable_wire_protocol() {
        use tracedecay_query::code_search::CodeIndexSearchUnavailableReasonV1 as Reason;

        for (reason, code, retryable) in [
            (
                Reason::CapabilityUnavailable,
                "code_index_unavailable",
                false,
            ),
            (Reason::AuthorityUnavailable, "authority_unavailable", false),
            (
                Reason::LinkedWorktreeDisabled,
                "linked_worktree_disabled",
                false,
            ),
            (Reason::Cancelled, "cancelled", true),
            (Reason::TimedOut, "timed_out", true),
            (
                Reason::CapacityUnavailable,
                "search_capacity_unavailable",
                true,
            ),
            (
                Reason::GenerationUnavailable,
                "generation_unavailable",
                true,
            ),
            (Reason::GenerationUnverified, "generation_unverified", true),
            (
                Reason::CorruptionResetRequired,
                "index_corruption_reset_required",
                false,
            ),
            (Reason::Internal, "search_failed", false),
        ] {
            for lane in ["similarity", "family"] {
                let error = clone_lane_unavailable_error(lane, reason);
                assert_eq!(
                    error
                        .project_route_context()
                        .expect("clone lane failures are typed project-route errors"),
                    (
                        code,
                        retryable,
                        format!("the maintained clone {lane} lane is unavailable: {code}").as_str(),
                    )
                );
            }
        }
        // A request the executor refuses is the caller's to correct, not an
        // unavailable lane.
        assert_eq!(
            clone_lane_unavailable_error("similarity", Reason::InvalidRequest)
                .project_route_context(),
            Some((
                "application_surface_invalid_request",
                false,
                "the maintained clone similarity lane refused the request as invalid",
            ))
        );
    }

    #[tokio::test]
    async fn similar_unavailable_wire_preserves_reason_and_retryability() {
        let temp = tempfile::tempdir().expect("temp root");
        let admitted = crate::tool_context::tests::scope("similar-unavailable");
        let project = crate::tool_context::tests::project_bundle(temp.path(), &admitted, None);
        let authority = tracedecay_query::code_search::CodeIndexSearchAuthorityV1 {
            principal: tracedecay_domain::PrincipalId::new("principal.similar-unavailable")
                .expect("principal"),
            authorization_revision: tracedecay_domain::AuthorizationRevision::new(
                "revision.similar-unavailable",
            )
            .expect("revision"),
        };

        for (reason, reason_code, retryable) in [
            (
                tracedecay_query::code_search::CodeIndexSearchUnavailableReasonV1::GenerationUnavailable,
                "generation_unavailable",
                true,
            ),
            (
                tracedecay_query::code_search::CodeIndexSearchUnavailableReasonV1::CorruptionResetRequired,
                "index_corruption_reset_required",
                false,
            ),
        ] {
            let executor: tracedecay_query::code_search::CodeIndexSimilarExecutor =
                std::sync::Arc::new(move |_| {
                    Box::pin(async move {
                        tracedecay_query::code_search::CodeIndexSimilarOutcomeV1::Unavailable(reason)
                    })
                });
            let code_index =
                crate::AdmittedCodeIndex::new(&authority, None, Some(&executor), None, None)
                    .expect("similar executor admits");
            let ctx = crate::McpToolContext::bind(crate::McpToolBinding {
                project: &project,
                request: crate::McpRequestAuthoritiesV1 {
                    code_index: Some(code_index),
                    ..crate::McpRequestAuthoritiesV1::default()
                },
            })
            .expect("admitted similar binding");
            let result = compute_similar(
                &ctx,
                json!({
                    "project_id": admitted.project_id,
                    "repository_id": admitted.repository_id,
                    "target": {
                        "kind": "symbol_occurrence",
                        "symbol_occurrence_id": "symbol.similar-unavailable",
                    },
                    "match_classes": ["conservative_exact"],
                    "result_limit": 10,
                    "work_limit": 20,
                }),
            )
            .await;
            let Err(error) = result else {
                panic!("unavailable similar executor must remain a transport failure");
            };
            let response =
                crate::tool_error_response(json!(1), "tracedecay_similar", &error);
            let wire: Value = serde_json::from_str(&crate::serialize_response_line(&response))
                .expect("JSON-RPC response");

            assert_eq!(wire["error"]["data"]["reason_code"], reason_code);
            assert_eq!(wire["error"]["data"]["retryable"], retryable);
        }
    }

    #[tokio::test]
    async fn redundancy_unavailable_wire_preserves_reason_and_retryability() {
        let temp = tempfile::tempdir().expect("temp root");
        let admitted = crate::tool_context::tests::scope("redundancy-unavailable");
        let project = crate::tool_context::tests::project_bundle(temp.path(), &admitted, None);
        let authority = tracedecay_query::code_search::CodeIndexSearchAuthorityV1 {
            principal: tracedecay_domain::PrincipalId::new("principal.redundancy-unavailable")
                .expect("principal"),
            authorization_revision: tracedecay_domain::AuthorizationRevision::new(
                "revision.redundancy-unavailable",
            )
            .expect("revision"),
        };

        for (reason, reason_code, retryable) in [
            (
                tracedecay_query::code_search::CodeIndexSearchUnavailableReasonV1::GenerationUnavailable,
                "generation_unavailable",
                true,
            ),
            (
                tracedecay_query::code_search::CodeIndexSearchUnavailableReasonV1::CorruptionResetRequired,
                "index_corruption_reset_required",
                false,
            ),
        ] {
            let executor: tracedecay_query::code_search::CodeIndexRedundancyExecutor =
                std::sync::Arc::new(move |_| {
                    Box::pin(async move { Err(reason) })
                });
            let code_index =
                crate::AdmittedCodeIndex::new(&authority, None, None, Some(&executor), None)
                    .expect("redundancy executor admits");
            let ctx = crate::McpToolContext::bind(crate::McpToolBinding {
                project: &project,
                request: crate::McpRequestAuthoritiesV1 {
                    code_index: Some(code_index),
                    ..crate::McpRequestAuthoritiesV1::default()
                },
            })
            .expect("admitted redundancy binding");
            let result = compute_redundancy(
                &ctx,
                json!({
                    "project_id": admitted.project_id,
                    "repository_id": admitted.repository_id,
                    "match_classes": ["conservative_exact"],
                    "scope": {"kind": "repository"},
                    "include_generated_paths": false,
                    "family_limit": 10,
                    "member_limit": 10,
                    "work_limit": 20,
                }),
            )
            .await;
            let Err(error) = result else {
                panic!("unavailable redundancy executor must remain a transport failure");
            };
            let response =
                crate::tool_error_response(json!(1), "tracedecay_redundancy", &error);
            let wire: Value = serde_json::from_str(&crate::serialize_response_line(&response))
                .expect("JSON-RPC response");

            assert_eq!(wire["error"]["data"]["reason_code"], reason_code);
            assert_eq!(wire["error"]["data"]["retryable"], retryable);
            assert_ne!(
                wire["error"]["data"]["reason_code"],
                "verified-code-redundancy-unavailable"
            );
        }
    }

    #[tokio::test]
    async fn missing_clone_lane_executors_emit_shared_capability_unavailable() {
        let temp = tempfile::tempdir().expect("temp root");
        let admitted = crate::tool_context::tests::scope("clone-missing-executor");
        let project = crate::tool_context::tests::project_bundle(temp.path(), &admitted, None);
        let authority = tracedecay_query::code_search::CodeIndexSearchAuthorityV1 {
            principal: tracedecay_domain::PrincipalId::new("principal.clone-missing-executor")
                .expect("principal"),
            authorization_revision: tracedecay_domain::AuthorizationRevision::new(
                "revision.clone-missing-executor",
            )
            .expect("revision"),
        };
        let shared_code = tracedecay_query::code_search::CodeIndexSearchUnavailableReasonV1::CapabilityUnavailable
            .as_str();

        // Admit the sibling lane so the request is authorized, then omit the
        // lane under test, the Codex P2 gap (opaque missing-executor tokens).
        let similar_stub: tracedecay_query::code_search::CodeIndexSimilarExecutor =
            std::sync::Arc::new(|_| {
                Box::pin(async {
                    tracedecay_query::code_search::CodeIndexSimilarOutcomeV1::NotFound
                })
            });
        let redundancy_only =
            crate::AdmittedCodeIndex::new(&authority, None, Some(&similar_stub), None, None)
                .expect("similar executor admits without redundancy");
        let redundancy_ctx = crate::McpToolContext::bind(crate::McpToolBinding {
            project: &project,
            request: crate::McpRequestAuthoritiesV1 {
                code_index: Some(redundancy_only),
                ..crate::McpRequestAuthoritiesV1::default()
            },
        })
        .expect("admitted similar-only binding");
        let redundancy_err = compute_redundancy(
            &redundancy_ctx,
            json!({
                "project_id": admitted.project_id,
                "repository_id": admitted.repository_id,
                "match_classes": ["conservative_exact"],
                "scope": {"kind": "repository"},
                "include_generated_paths": false,
                "family_limit": 10,
                "member_limit": 10,
                "work_limit": 20,
            }),
        )
        .await
        .expect_err("missing redundancy executor must be typed unavailable");
        let redundancy_wire: Value = serde_json::from_str(&crate::serialize_response_line(
            &crate::tool_error_response(json!(1), "tracedecay_redundancy", &redundancy_err),
        ))
        .expect("JSON-RPC response");
        assert_eq!(redundancy_wire["error"]["data"]["reason_code"], shared_code);
        assert_eq!(redundancy_wire["error"]["data"]["retryable"], false);
        assert_ne!(
            redundancy_wire["error"]["data"]["reason_code"],
            "verified-code-redundancy-unavailable"
        );

        let redundancy_stub: tracedecay_query::code_search::CodeIndexRedundancyExecutor =
            std::sync::Arc::new(|_| {
                Box::pin(async {
                    Err(tracedecay_query::code_search::CodeIndexSearchUnavailableReasonV1::Internal)
                })
            });
        let similar_only =
            crate::AdmittedCodeIndex::new(&authority, None, None, Some(&redundancy_stub), None)
                .expect("redundancy executor admits without similar");
        let similar_ctx = crate::McpToolContext::bind(crate::McpToolBinding {
            project: &project,
            request: crate::McpRequestAuthoritiesV1 {
                code_index: Some(similar_only),
                ..crate::McpRequestAuthoritiesV1::default()
            },
        })
        .expect("admitted redundancy-only binding");
        let similar_err = compute_similar(
            &similar_ctx,
            json!({
                "project_id": admitted.project_id,
                "repository_id": admitted.repository_id,
                "target": {
                    "kind": "symbol_occurrence",
                    "symbol_occurrence_id": "symbol.clone-missing-executor",
                },
                "match_classes": ["conservative_exact"],
                "result_limit": 10,
                "work_limit": 20,
            }),
        )
        .await
        .expect_err("missing similar executor must be typed unavailable");
        let similar_wire: Value = serde_json::from_str(&crate::serialize_response_line(
            &crate::tool_error_response(json!(1), "tracedecay_similar", &similar_err),
        ))
        .expect("JSON-RPC response");
        assert_eq!(similar_wire["error"]["data"]["reason_code"], shared_code);
        assert_eq!(similar_wire["error"]["data"]["retryable"], false);
        assert_ne!(
            similar_wire["error"]["data"]["reason_code"],
            "verified-code-similarity-unavailable"
        );
    }
}
