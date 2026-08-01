//! Project-owned CI retention and code-anchor stores for PR13 production open.

use std::sync::Arc;

use tracedecay_application::RequestContext;
use tracedecay_application::feedback::{
    CI_FAILURE_LOCALIZE_CAPABILITY_ID_V1, CI_FAILURE_LOCALIZE_USE_CASE_ID_V1,
    CiFailureLocalizationRequestV1, FeedbackPortFuture,
};
use tracedecay_domain::canonical_sha256;
use tracedecay_domain::feedback::{
    CiCallerRelationV1, CiFailureCallerEvidenceV1, CiFailureCoverageV1,
    CiFailureGenerationEvidenceV1, CiFailureKindV1, CiFailureLocalizationStateV1,
    CiFailureSymbolEvidenceV1, CiFailureTestEvidenceV1, FeedbackScopeV1,
    MAX_CI_FAILURE_CALLER_EVIDENCE_V1, MAX_CI_FAILURE_TEST_EVIDENCE_V1,
};
use tracedecay_domain::{
    CanonicalObservationIdV1, CodeGenerationId, FileOccurrenceId, RetrievalAnchorId, SourceSpan,
    SymbolOccurrenceId,
};

use super::GitHubCiProviderRecordV1;
use super::production::{
    CiCodeAnchorStoreV1, CiExactCodeEvidenceV1, CiRetainedProviderObservationAuthorityV1,
    CiRetainedProviderObservationV1, CiRetainedProviderRecordV1,
};
use crate::advisory::context_allows_feedback_operation;
use crate::tracedecay::TraceDecay;
use tracedecay_runtime_core::db::Database;

const RETAINED_KEY_DOMAIN_V1: &str = "tracedecay.pr13.ci.retained-key.v1";
const RETAINED_KEY_PREFIX_V1: &str = "feedback.ci-failure.retained.v1.";
const MAX_RETAINED_BYTES_V1: usize = 4 * 1024 * 1024;

/// Durable CI retained-observation authority mirrored on the project graph DB.
#[derive(Clone)]
pub struct ProjectCiRetainedObservationStoreV1 {
    database: Database,
    scope: FeedbackScopeV1,
}

impl ProjectCiRetainedObservationStoreV1 {
    pub fn new(database: Database, scope: FeedbackScopeV1) -> Option<Self> {
        scope.validate().ok()?;
        Some(Self { database, scope })
    }

    fn key(&self, request: &CiFailureLocalizationRequestV1) -> Option<String> {
        if request.scope != self.scope {
            return None;
        }
        canonical_sha256(&(RETAINED_KEY_DOMAIN_V1, &request.scope, &request.run))
            .ok()
            .map(|digest| format!("{RETAINED_KEY_PREFIX_V1}{}", digest.as_str()))
    }

    fn observation_for(
        &self,
        context: &RequestContext,
        request: &CiFailureLocalizationRequestV1,
        record: &GitHubCiProviderRecordV1,
    ) -> Option<CiRetainedProviderObservationV1> {
        let digest = canonical_sha256(&(
            "tracedecay.pr13.ci.retained-observation.v1",
            &request.scope,
            &request.run,
            record.run_identity(),
        ))
        .ok()?;
        let observation_id = CanonicalObservationIdV1::new(digest.as_str().to_owned()).ok()?;
        let failure_anchor = match record.failed_annotation() {
            Some(annotation) => {
                let anchor_digest = canonical_sha256(&(
                    "tracedecay.pr13.ci.failure-anchor.v1",
                    &annotation.path,
                    annotation.start_line,
                    annotation.end_line,
                    &request.run,
                ))
                .ok()?;
                RetrievalAnchorId::new(format!(
                    "anchor.ci.failure.{}",
                    anchor_digest.as_str().trim_start_matches("sha256:")
                ))
                .ok()?
            }
            None => {
                let anchor_digest = canonical_sha256(&(
                    "tracedecay.pr13.ci.failure-anchor.job.v1",
                    &request.run,
                    record.failed_step().map(|step| step.number),
                ))
                .ok()?;
                RetrievalAnchorId::new(format!(
                    "anchor.ci.failure.{}",
                    anchor_digest.as_str().trim_start_matches("sha256:")
                ))
                .ok()?
            }
        };
        Some(CiRetainedProviderObservationV1 {
            observation_id,
            failure_anchor,
            provider_head_commit_id: request.scope.head_commit_id.clone(),
            failure_kind: CiFailureKindV1::Unknown,
            observed_at: context.grant().issued_at,
        })
    }
}

impl CiRetainedProviderObservationAuthorityV1 for ProjectCiRetainedObservationStoreV1 {
    fn load<'a>(
        &'a self,
        context: &'a RequestContext,
        request: &'a CiFailureLocalizationRequestV1,
    ) -> FeedbackPortFuture<'a, Option<CiRetainedProviderRecordV1>> {
        Box::pin(async move {
            if !context_allows_feedback_operation(
                context,
                &self.scope,
                CI_FAILURE_LOCALIZE_CAPABILITY_ID_V1,
                CI_FAILURE_LOCALIZE_USE_CASE_ID_V1,
            ) {
                return None;
            }
            let key = self.key(request)?;
            let encoded = self.database.get_metadata(&key).await.ok()??;
            if encoded.len() > MAX_RETAINED_BYTES_V1 {
                return None;
            }
            let record = serde_json::from_str::<CiRetainedProviderRecordV1>(&encoded).ok()?;
            record.validate_for(request).then_some(record)
        })
    }

    fn retain<'a>(
        &'a self,
        context: &'a RequestContext,
        request: &'a CiFailureLocalizationRequestV1,
        record: &'a GitHubCiProviderRecordV1,
        state: CiFailureLocalizationStateV1,
        coverage: CiFailureCoverageV1,
    ) -> FeedbackPortFuture<'a, Option<CiRetainedProviderObservationV1>> {
        Box::pin(async move {
            if !context_allows_feedback_operation(
                context,
                &self.scope,
                CI_FAILURE_LOCALIZE_CAPABILITY_ID_V1,
                CI_FAILURE_LOCALIZE_USE_CASE_ID_V1,
            ) {
                return None;
            }
            if !matches!(
                (state, coverage),
                (
                    CiFailureLocalizationStateV1::Complete | CiFailureLocalizationStateV1::Partial,
                    CiFailureCoverageV1::Complete | CiFailureCoverageV1::Partial
                )
            ) {
                return None;
            }
            let observation = self.observation_for(context, request, record)?;
            let retained = CiRetainedProviderRecordV1 {
                provider_record: record.clone(),
                observation: observation.clone(),
            };
            if !retained.validate_for(request) {
                return None;
            }
            let key = self.key(request)?;
            let encoded = serde_json::to_string(&retained).ok()?;
            if encoded.len() > MAX_RETAINED_BYTES_V1 {
                return None;
            }
            self.database.set_metadata(&key, &encoded).await.ok()?;
            Some(observation)
        })
    }
}

/// Graph-backed CI code-anchor resolver over the sealed project index.
#[derive(Clone)]
pub struct ProjectCiCodeAnchorStoreV1 {
    graph: Arc<TraceDecay>,
    scope: FeedbackScopeV1,
    code_index_identity:
        Option<Arc<dyn crate::diagnostics_publication::CodeIndexPublicationIdentityPortV1>>,
}

impl ProjectCiCodeAnchorStoreV1 {
    pub fn new(graph: Arc<TraceDecay>, scope: FeedbackScopeV1) -> Option<Self> {
        scope.validate().ok()?;
        Some(Self {
            graph,
            scope,
            code_index_identity: None,
        })
    }

    pub fn new_with_code_index_identity(
        graph: Arc<TraceDecay>,
        scope: FeedbackScopeV1,
        code_index_identity: Arc<
            dyn crate::diagnostics_publication::CodeIndexPublicationIdentityPortV1,
        >,
    ) -> Option<Self> {
        let mut store = Self::new(graph, scope)?;
        store.code_index_identity = Some(code_index_identity);
        Some(store)
    }
}

impl CiCodeAnchorStoreV1 for ProjectCiCodeAnchorStoreV1 {
    fn resolve<'a>(
        &'a self,
        context: &'a RequestContext,
        request: &'a CiFailureLocalizationRequestV1,
        record: &'a CiRetainedProviderRecordV1,
    ) -> FeedbackPortFuture<'a, Option<CiExactCodeEvidenceV1>> {
        Box::pin(async move {
            if !context_allows_feedback_operation(
                context,
                &self.scope,
                CI_FAILURE_LOCALIZE_CAPABILITY_ID_V1,
                CI_FAILURE_LOCALIZE_USE_CASE_ID_V1,
            ) || request.scope != self.scope
                || !record.validate_for(request)
                || record.provider_record.workflow_run.head_sha
                    != request.scope.head_commit_id.as_str()
                || record.provider_record.workflow_job.head_sha
                    != request.scope.head_commit_id.as_str()
                || record.provider_record.check_run.head_sha
                    != request.scope.head_commit_id.as_str()
            {
                return None;
            }
            let Some(annotation) = record.provider_record.failed_annotation() else {
                return Some(partial_code_evidence());
            };
            let Some(path) = canonical_project_relative_path(&annotation.path) else {
                return Some(partial_code_evidence());
            };
            let Some(synced_commit) = self.graph.last_synced_commit().await else {
                return Some(partial_code_evidence());
            };
            if synced_commit != request.scope.head_commit_id.as_str() {
                return Some(partial_code_evidence());
            }
            let Ok(mut nodes) = self.graph.get_nodes_by_file(&path).await else {
                return Some(partial_code_evidence());
            };
            // GitHub annotation lines are one-based; graph node spans retain
            // tree-sitter's zero-based rows.
            let graph_start_line = annotation.start_line.saturating_sub(1);
            let graph_end_line = annotation.end_line.saturating_sub(1);
            nodes.retain(|node| {
                node.start_line <= graph_start_line && node.end_line >= graph_end_line
            });
            nodes.sort_by(|left, right| {
                left.end_line
                    .saturating_sub(left.start_line)
                    .cmp(&right.end_line.saturating_sub(right.start_line))
                    .then_with(|| left.start_line.cmp(&right.start_line))
                    .then_with(|| left.id.cmp(&right.id))
            });
            let Some(symbol_node) = nodes.first() else {
                return Some(partial_code_evidence());
            };
            let Ok(source) = std::fs::read_to_string(self.graph.project_root().join(&path)) else {
                return Some(partial_code_evidence());
            };
            let Ok(Some(file_record)) = self.graph.db().get_file(&path).await else {
                return Some(partial_code_evidence());
            };
            if tracedecay_runtime_core::sync::content_hash(&source) != file_record.content_hash {
                return Some(partial_code_evidence());
            }
            let code_index_identity = if let Some(resolver) = self.code_index_identity.as_ref() {
                let Some(identity) = resolver
                    .resolve(self.graph.project_root().to_path_buf())
                    .await
                else {
                    return Some(partial_code_evidence());
                };
                if identity.source_revision() != Some(&request.scope.head_commit_id) {
                    return Some(partial_code_evidence());
                }
                Some(identity)
            } else {
                None
            };
            let Some(span) = source_span_for_annotation(
                &source,
                annotation.start_line,
                annotation.end_line,
                annotation.start_column,
                annotation.end_column,
            ) else {
                return Some(partial_code_evidence());
            };
            let file = if let Some(identity) = code_index_identity.as_ref() {
                let Some((file, digest)) = identity.file(&path) else {
                    return Some(partial_code_evidence());
                };
                if digest.as_str() != file_record.content_hash {
                    return Some(partial_code_evidence());
                }
                file.clone()
            } else {
                let Ok(file) = FileOccurrenceId::new(path) else {
                    return Some(partial_code_evidence());
                };
                file
            };
            let Ok(symbol) = SymbolOccurrenceId::new(symbol_node.id.clone()) else {
                return Some(partial_code_evidence());
            };

            let Ok(mut caller_nodes) = self.graph.get_callers(&symbol_node.id, 3).await else {
                return Some(partial_code_evidence());
            };
            caller_nodes.sort_by(|left, right| left.0.id.cmp(&right.0.id));
            caller_nodes.dedup_by(|left, right| left.0.id == right.0.id);
            let callers_truncated = caller_nodes.len() > MAX_CI_FAILURE_CALLER_EVIDENCE_V1;
            caller_nodes.truncate(MAX_CI_FAILURE_CALLER_EVIDENCE_V1);
            let callers = caller_nodes
                .iter()
                .filter_map(|(node, edge)| {
                    Some(CiFailureCallerEvidenceV1 {
                        retrieval_anchor_id: record.observation.failure_anchor.clone(),
                        caller_symbol: SymbolOccurrenceId::new(node.id.clone()).ok()?,
                        relation: if edge.target == symbol_node.id {
                            CiCallerRelationV1::DirectCall
                        } else {
                            CiCallerRelationV1::TransitiveCall
                        },
                    })
                })
                .collect::<Vec<_>>();

            let Ok(mut impacted) = self
                .graph
                .get_impact_radius_multi(std::slice::from_ref(&symbol_node.id), 3)
                .await
            else {
                return Some(partial_code_evidence());
            };
            impacted.sort_by(|left, right| left.id.cmp(&right.id));
            impacted.dedup_by(|left, right| left.id == right.id);
            let impacted_ids = impacted
                .iter()
                .map(|node| node.id.clone())
                .collect::<Vec<_>>();
            let Ok(test_ids) = self.graph.get_test_annotated_node_ids(&impacted_ids).await else {
                return Some(partial_code_evidence());
            };
            let mut test_ids = test_ids.into_iter().collect::<Vec<_>>();
            test_ids.sort();
            let tests_truncated = test_ids.len() > MAX_CI_FAILURE_TEST_EVIDENCE_V1;
            test_ids.truncate(MAX_CI_FAILURE_TEST_EVIDENCE_V1);
            let tests = test_ids
                .iter()
                .filter_map(|node_id| {
                    Some(CiFailureTestEvidenceV1 {
                        retrieval_anchor_id: record.observation.failure_anchor.clone(),
                        test_symbol: SymbolOccurrenceId::new(node_id.clone()).ok()?,
                    })
                })
                .collect::<Vec<_>>();
            let Ok(all_nodes) = self.graph.get_all_nodes().await else {
                return Some(partial_code_evidence());
            };
            let Ok(all_edges) = self.graph.get_all_edges().await else {
                return Some(partial_code_evidence());
            };
            let mut generation_nodes = all_nodes
                .iter()
                .map(|node| node.id.as_str())
                .collect::<Vec<_>>();
            generation_nodes.sort_unstable();
            let mut generation_edges = all_edges
                .iter()
                .map(|edge| {
                    (
                        edge.source.as_str(),
                        edge.target.as_str(),
                        edge.kind.as_str(),
                        edge.line,
                    )
                })
                .collect::<Vec<_>>();
            generation_edges.sort_unstable();
            let Ok(generation_digest) = canonical_sha256(&(
                "tracedecay.pr13.ci.graph-generation.v1",
                &request.scope.project_id,
                &request.scope.worktree_id,
                &request.scope.head_commit_id,
                &generation_nodes,
                &generation_edges,
                &symbol_node.id,
                &callers,
                &tests,
            )) else {
                return Some(partial_code_evidence());
            };
            let Some(generation_suffix) = generation_digest.as_str().strip_prefix("sha256:") else {
                return Some(partial_code_evidence());
            };
            let generation_id = if let Some(identity) = code_index_identity.as_ref() {
                identity.generation_id().clone()
            } else {
                let Ok(generation_id) =
                    CodeGenerationId::new(format!("generation.ci.graph.{generation_suffix}"))
                else {
                    return Some(partial_code_evidence());
                };
                generation_id
            };
            let partial = callers_truncated
                || tests_truncated
                || callers.len() != caller_nodes.len()
                || tests.len() != test_ids.len();
            Some(CiExactCodeEvidenceV1 {
                state: if partial {
                    CiFailureLocalizationStateV1::Partial
                } else {
                    CiFailureLocalizationStateV1::Complete
                },
                coverage: if partial {
                    CiFailureCoverageV1::Partial
                } else {
                    CiFailureCoverageV1::Complete
                },
                generation: Some(CiFailureGenerationEvidenceV1 {
                    generation_id,
                    retrieval_anchor_id: record.observation.failure_anchor.clone(),
                }),
                symbol: Some(CiFailureSymbolEvidenceV1 {
                    retrieval_anchor_id: record.observation.failure_anchor.clone(),
                    file,
                    span,
                    symbol,
                }),
                callers,
                tests,
            })
        })
    }
}

fn partial_code_evidence() -> CiExactCodeEvidenceV1 {
    CiExactCodeEvidenceV1 {
        state: CiFailureLocalizationStateV1::Partial,
        coverage: CiFailureCoverageV1::Partial,
        generation: None,
        symbol: None,
        callers: Vec::new(),
        tests: Vec::new(),
    }
}

fn canonical_project_relative_path(value: &str) -> Option<String> {
    let normalized = value.replace('\\', "/");
    if normalized.is_empty()
        || normalized.starts_with('/')
        || normalized.chars().any(char::is_control)
        || normalized
            .split('/')
            .any(|segment| segment.is_empty() || segment == "." || segment == "..")
    {
        return None;
    }
    Some(normalized)
}

fn source_span_for_annotation(
    source: &str,
    start_line: u32,
    end_line: u32,
    start_column: Option<u32>,
    end_column: Option<u32>,
) -> Option<SourceSpan> {
    if start_line == 0 || end_line < start_line {
        return None;
    }
    let mut line_starts = vec![0_usize];
    line_starts.extend(
        source
            .match_indices('\n')
            .map(|(index, _)| index.saturating_add(1)),
    );
    let start_index = usize::try_from(start_line.saturating_sub(1)).ok()?;
    let end_index = usize::try_from(end_line.saturating_sub(1)).ok()?;
    let line_start = *line_starts.get(start_index)?;
    let end_line_start = *line_starts.get(end_index)?;
    let end_line_limit = line_starts
        .get(end_index.saturating_add(1))
        .copied()
        .unwrap_or(source.len());
    let start_byte = line_column_offset(
        source,
        line_start,
        line_starts
            .get(start_index.saturating_add(1))
            .copied()
            .unwrap_or(source.len()),
        start_column.unwrap_or(1),
        false,
    )?;
    let end_byte = line_column_offset(
        source,
        end_line_start,
        end_line_limit,
        end_column.unwrap_or_else(|| {
            u32::try_from(source[end_line_start..end_line_limit].chars().count())
                .unwrap_or(u32::MAX)
        }),
        true,
    )?;
    let span = SourceSpan {
        start_byte: u64::try_from(start_byte).ok()?,
        end_byte: u64::try_from(end_byte).ok()?,
    };
    span.validate().ok()?;
    Some(span)
}

fn line_column_offset(
    source: &str,
    line_start: usize,
    line_limit: usize,
    column: u32,
    inclusive_end: bool,
) -> Option<usize> {
    if column == 0 || line_start > line_limit || line_limit > source.len() {
        return None;
    }
    let requested = usize::try_from(column.saturating_sub(1)).ok()?;
    let line = source.get(line_start..line_limit)?;
    let mut offsets = line
        .char_indices()
        .map(|(offset, _)| offset)
        .collect::<Vec<_>>();
    offsets.push(line.len());
    let base = *offsets.get(requested)?;
    if inclusive_end {
        Some(
            line_start.saturating_add(
                offsets
                    .get(requested.saturating_add(1))
                    .copied()
                    .unwrap_or(line.len()),
            ),
        )
    } else {
        Some(line_start.saturating_add(base))
    }
}
