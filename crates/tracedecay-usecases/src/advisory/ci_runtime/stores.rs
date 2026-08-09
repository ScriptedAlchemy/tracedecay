//! Project-owned CI retention and code-anchor stores for advisory production open.

use std::sync::Arc;

use tracedecay_application::RequestContext;
use tracedecay_application::feedback::{
    CI_FAILURE_LOCALIZE_CAPABILITY_ID_V1, CI_FAILURE_LOCALIZE_USE_CASE_ID_V1,
    CiFailureLocalizationRequestV1, FeedbackPortFuture,
};
use tracedecay_domain::feedback::{
    CiCallerRelationV1, CiFailureCallerEvidenceV1, CiFailureCoverageV1,
    CiFailureGenerationEvidenceV1, CiFailureKindV1, CiFailureLocalizationStateV1,
    CiFailureSymbolEvidenceV1, CiFailureTestEvidenceV1, FeedbackScopeV1,
    MAX_CI_FAILURE_CALLER_EVIDENCE_V1, MAX_CI_FAILURE_TEST_EVIDENCE_V1,
};
use tracedecay_domain::{CanonicalObservationIdV1, ContentDigest, RetrievalAnchorId, SourceSpan};
use tracedecay_domain::{RelationEdgeKindV1, canonical_sha256};

use super::GitHubCiProviderRecordV1;
use super::production::{
    CiCodeAnchorStoreV1, CiExactCodeEvidenceV1, CiRetainedProviderObservationAuthorityV1,
    CiRetainedProviderObservationV1, CiRetainedProviderRecordV1,
};
use crate::advisory::context_allows_feedback_operation;
use crate::graph::{CodeGraphProjectionReadPort, CodeGraphReadRequest, request_graph_cancellation};
use crate::tracedecay::TraceDecay;
use tracedecay_runtime_core::db::Database;

const RETAINED_KEY_DOMAIN_V1: &str = "tracedecay.advisory.ci.retained-key.v1";
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
            "tracedecay.advisory.ci.retained-observation.v1",
            &request.scope,
            &request.run,
            record.run_identity(),
        ))
        .ok()?;
        let observation_id = CanonicalObservationIdV1::new(digest.as_str().to_owned()).ok()?;
        let failure_anchor = match record.failed_annotation() {
            Some(annotation) => {
                let anchor_digest = canonical_sha256(&(
                    "tracedecay.advisory.ci.failure-anchor.v1",
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
                    "tracedecay.advisory.ci.failure-anchor.job.v1",
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
    code_graph: Arc<dyn CodeGraphProjectionReadPort>,
    scope: FeedbackScopeV1,
    code_index_identity:
        Option<Arc<dyn crate::diagnostics_publication::CodeIndexPublicationIdentityPortV1>>,
}

impl ProjectCiCodeAnchorStoreV1 {
    pub fn new(
        graph: Arc<TraceDecay>,
        scope: FeedbackScopeV1,
        code_graph: Arc<dyn CodeGraphProjectionReadPort>,
    ) -> Option<Self> {
        scope.validate().ok()?;
        Some(Self {
            graph,
            code_graph,
            scope,
            code_index_identity: None,
        })
    }

    pub fn new_with_code_index_identity(
        graph: Arc<TraceDecay>,
        scope: FeedbackScopeV1,
        code_graph: Arc<dyn CodeGraphProjectionReadPort>,
        code_index_identity: Arc<
            dyn crate::diagnostics_publication::CodeIndexPublicationIdentityPortV1,
        >,
    ) -> Option<Self> {
        let mut store = Self::new(graph, scope, code_graph)?;
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
            let cancellation = request_graph_cancellation(context);
            let Ok(verified) = self
                .code_graph
                .open(CodeGraphReadRequest::new(
                    context,
                    context.grant().issued_at,
                    Arc::clone(&cancellation),
                ))
                .await
            else {
                return Some(partial_code_evidence());
            };
            let Ok(reader) = verified.reader_with_cancellation(
                context,
                context.grant().issued_at,
                Arc::clone(&cancellation),
            ) else {
                return Some(partial_code_evidence());
            };
            let Ok(Some(file_record)) =
                reader.file_by_logical_path(&path, Arc::clone(&cancellation))
            else {
                return Some(partial_code_evidence());
            };
            let Ok(source) = std::fs::read_to_string(self.graph.project_root().join(&path)) else {
                return Some(partial_code_evidence());
            };
            let Some(source_digest) =
                normalized_content_digest(&tracedecay_runtime_core::sync::content_hash(&source))
            else {
                return Some(partial_code_evidence());
            };
            if source_digest != file_record.content_digest {
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
                if digest != &file_record.content_digest {
                    return Some(partial_code_evidence());
                }
                file.clone()
            } else {
                file_record.file_occurrence_id.clone()
            };
            let Ok(mut symbols) =
                reader.symbols_in_logical_file(&path, 100_000, Arc::clone(&cancellation))
            else {
                return Some(partial_code_evidence());
            };
            symbols.retain(|symbol| {
                symbol
                    .binding
                    .as_ref()
                    .and_then(|binding| binding.source_span)
                    .is_some_and(|candidate| {
                        candidate.start_byte <= span.start_byte
                            && candidate.end_byte >= span.end_byte
                    })
            });
            symbols.sort_by(|left, right| {
                let left_span = left
                    .binding
                    .as_ref()
                    .and_then(|binding| binding.source_span);
                let right_span = right
                    .binding
                    .as_ref()
                    .and_then(|binding| binding.source_span);
                left_span
                    .map(|span| span.end_byte.saturating_sub(span.start_byte))
                    .cmp(&right_span.map(|span| span.end_byte.saturating_sub(span.start_byte)))
                    .then_with(|| left.occurrence.cmp(&right.occurrence))
            });
            let Some(symbol_summary) = symbols.first() else {
                return Some(partial_code_evidence());
            };
            let symbol = symbol_summary.occurrence.clone();
            let Ok(impact) = reader.impact(
                std::slice::from_ref(&symbol),
                &[RelationEdgeKindV1::Calls],
                3,
                100_000,
                100_000,
                Arc::clone(&cancellation),
            ) else {
                return Some(partial_code_evidence());
            };
            let callers_truncated = impact.impacted.len() > MAX_CI_FAILURE_CALLER_EVIDENCE_V1;
            let callers = impact
                .impacted
                .iter()
                .take(MAX_CI_FAILURE_CALLER_EVIDENCE_V1)
                .map(|impacted| CiFailureCallerEvidenceV1 {
                    retrieval_anchor_id: record.observation.failure_anchor.clone(),
                    caller_symbol: impacted.summary.occurrence.clone(),
                    relation: if impacted.depth == 1 {
                        CiCallerRelationV1::DirectCall
                    } else {
                        CiCallerRelationV1::TransitiveCall
                    },
                })
                .collect::<Vec<_>>();
            let test_symbols = impact
                .impacted
                .iter()
                .filter(|impacted| {
                    impacted
                        .summary
                        .metadata
                        .as_ref()
                        .is_some_and(|metadata| metadata.kind.eq_ignore_ascii_case("test"))
                })
                .collect::<Vec<_>>();
            let tests_truncated = test_symbols.len() > MAX_CI_FAILURE_TEST_EVIDENCE_V1;
            let tests = test_symbols
                .into_iter()
                .take(MAX_CI_FAILURE_TEST_EVIDENCE_V1)
                .map(|impacted| CiFailureTestEvidenceV1 {
                    retrieval_anchor_id: record.observation.failure_anchor.clone(),
                    test_symbol: impacted.summary.occurrence.clone(),
                })
                .collect::<Vec<_>>();
            let generation_id = if let Some(identity) = code_index_identity.as_ref() {
                if identity.generation_id() != reader.generation() {
                    return Some(partial_code_evidence());
                }
                identity.generation_id().clone()
            } else {
                reader.generation().clone()
            };
            let partial = callers_truncated || tests_truncated || !impact.complete;
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

fn normalized_content_digest(value: &str) -> Option<ContentDigest> {
    if value.starts_with("sha256:") {
        ContentDigest::new(value.to_owned()).ok()
    } else {
        ContentDigest::new(format!("sha256:{value}")).ok()
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
