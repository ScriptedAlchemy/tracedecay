//! The concrete feedback LSP source: cycle runtime, diagnostic snapshots, and context projection authority.

use std::collections::BTreeSet;
use std::sync::Arc;

use serde::Serialize;
use tracedecay_contracts::feedback::{FeedbackDiagnosticsReadRequestV1, FeedbackExpandRequestV1};
use tracedecay_contracts::request_identity::{GlobalRequestSurface, mint_global_request_id};
use tracedecay_contracts::{
    AnchorExpandRequest, ApplicationOutcome, ApplicationResult, PageRequest, ResultProjection,
    RetrievalOrder, RetrievalRequestMeta, now_micros,
};
use tracedecay_domain::feedback::{
    FeedbackContentIdentityV1, FeedbackDiagnosticProducerV1, FeedbackFindingLifecycleV1,
    FeedbackFindingV1,
};
use tracedecay_domain::{CodeGenerationId, CommitId, ContentDigest, ManifestDigest, UtcMicros};
use tracedecay_lsp::{
    AdmittedRoot, CanonicalContextProjectionAuthority, CanonicalDiagnosticRefreshRequest,
    ContextCoverage, ContextExpansionEnvelope, ContextExpansionOutcome, ContextExpansionRequest,
    ContextExpansionScope, ContextFreshness, ContextProducerState, ContextProjectionChange,
    ContextProjectionEnvelope, ContextProjectionIdentity, ContextProjectionItem,
    ContextProjectionKind, ContextProjectionOutcome, ContextProjectionRegistration,
    ContextProjectionRequest, DiagnosticTrigger, FeedbackCycleRequest, FeedbackCycleRuntimePort,
    LspRequestId, LspRuntimeFailure, LspRuntimeFuture, MAX_CONTEXT_PROJECTION_ITEMS,
    MAX_CONTEXT_RETRIEVAL_HANDLE_BYTES, ManagedDiagnosticSnapshot, ManagedDiagnosticSnapshotPort,
    TRACEDECAY_CONTEXT_REVISION,
};
use tracedecay_session_memory::response_handles::{
    ResponseHandleLookup, micros_to_seconds, retrieve_response_handle, store_response_handle,
};

use super::context_projection::{
    advisory_finding_matches, advisory_projection_producer, advisory_projection_status,
    affected_test_projection, bounded_advisory_item_omissions, cycle_coverage, finding_item,
    finding_matches_document, impact_projection, producer_state_for_cycle,
    projection_omission_reasons,
};
use super::diagnostic_projection::LspFeedbackDiagnosticProjectionPort;
use super::managed_test_runs::LspTestRunProjectionPort;
use super::registered_authority::LspFeedbackProjectionScopePort;
use super::{
    CurrentFeedbackCycle, FindingContextTarget, LSP_CONTEXT_EXPANSION_HANDLE_SCHEMA_VERSION,
    LspFeedbackProjectionScope, ProjectionChangeQueue, StoredLspContextExpansionV1,
    StoredLspTestRunExpansionV1, incomplete_read_projection,
};
use crate::feedback::concrete::{ConcreteFeedbackOwner, FeedbackRuntime, ProjectFeedbackStore};
use crate::feedback::owner::{
    FeedbackReadInvocationResultV1, FeedbackReadOperationV1, FeedbackReadOwnerErrorV1,
};

/// Shared feedback source mounted as both `FeedbackCyclePort` and the managed
/// diagnostics/context authority in the daemon LSP runtime adapters.
#[derive(Clone)]
pub struct ConcreteFeedbackLspSource {
    runtime: Arc<FeedbackRuntime>,
    owner: Arc<ConcreteFeedbackOwner>,
    publications: ProjectFeedbackStore,
    cycle: Arc<dyn FeedbackCycleRuntimePort>,
    scope: Arc<dyn LspFeedbackProjectionScopePort>,
    diagnostic_projection: Arc<dyn LspFeedbackDiagnosticProjectionPort>,
    test_runs: Arc<dyn LspTestRunProjectionPort>,
    changes: ProjectionChangeQueue,
}

impl ConcreteFeedbackLspSource {
    pub fn new<F>(
        runtime: Arc<FeedbackRuntime>,
        cycle: F,
        scope: Arc<dyn LspFeedbackProjectionScopePort>,
        diagnostic_projection: Arc<dyn LspFeedbackDiagnosticProjectionPort>,
        test_runs: Arc<dyn LspTestRunProjectionPort>,
    ) -> Self
    where
        F: FnOnce(ProjectFeedbackStore) -> Arc<dyn FeedbackCycleRuntimePort>,
    {
        let owner = runtime.owner();
        let publications = runtime.publication_store();
        let cycle = cycle(publications.clone());
        Self {
            runtime,
            owner,
            publications,
            cycle,
            scope,
            diagnostic_projection,
            test_runs,
            changes: ProjectionChangeQueue::default(),
        }
    }

    /// The exact store clone supplied to the feedback cycle dedupe/publication
    /// boundary. Exposing it lets the daemon composition root prove both
    /// surfaces use one authority.
    pub fn publication_store(&self) -> ProjectFeedbackStore {
        self.publications.clone()
    }

    #[hotpath::measure(label = "usecases.lsp.changes.queue", future = true)]
    pub(super) async fn queue_feedback_changes(
        &self,
        request: &FeedbackCycleRequest,
    ) -> Result<CurrentFeedbackCycle, LspRuntimeFailure> {
        let current = self
            .current_cycle(
                AdmittedRoot::new(request.root_uri.clone()),
                Some(request.document_uri.clone()),
                None,
            )
            .await?;
        let identity = current.scope.projection_identity();
        let generation = current.scope.generation;
        let (source_revision, changes) = match current.result.as_ref() {
            Some(result) => {
                let cycle = &result.cycle;
                let aggregate_state = producer_state_for_cycle(cycle);
                let github =
                    advisory_projection_status(cycle, FeedbackDiagnosticProducerV1::GitHubReview);
                let ci =
                    advisory_projection_status(cycle, FeedbackDiagnosticProducerV1::CiLocalization);
                let proximity =
                    advisory_projection_status(cycle, FeedbackDiagnosticProducerV1::Proximity);
                (
                    cycle.result_id.as_str().to_owned(),
                    vec![
                        (
                            ContextProjectionKind::diagnostics(),
                            cycle_coverage(cycle),
                            aggregate_state,
                        ),
                        (
                            ContextProjectionKind::post_edit_impact(),
                            impact_projection(cycle).0,
                            aggregate_state,
                        ),
                        (
                            ContextProjectionKind::affected_tests(),
                            affected_test_projection(cycle).0,
                            aggregate_state,
                        ),
                        (ContextProjectionKind::github_review(), github.0, github.1),
                        (ContextProjectionKind::ci_failure_localization(), ci.0, ci.1),
                        (
                            ContextProjectionKind::agent_proximity(),
                            proximity.0,
                            proximity.1,
                        ),
                    ],
                )
            }
            // No cycle to name a revision with. Keying the notification on the
            // termination collapses a run of identical degraded reads into one
            // change instead of re-announcing the same absence per read.
            None => {
                let (coverage, producer_state) = incomplete_read_projection(current.termination);
                (
                    format!("incomplete-read.{producer_state:?}"),
                    [
                        ContextProjectionKind::diagnostics(),
                        ContextProjectionKind::post_edit_impact(),
                        ContextProjectionKind::affected_tests(),
                        ContextProjectionKind::github_review(),
                        ContextProjectionKind::ci_failure_localization(),
                        ContextProjectionKind::agent_proximity(),
                    ]
                    .map(|kind| (kind, coverage, producer_state))
                    .to_vec(),
                )
            }
        };
        for (kind, coverage, producer_state) in changes {
            self.changes.offer(
                source_revision.clone(),
                ContextProjectionChange {
                    root_uri: request.root_uri.clone(),
                    document_uri: Some(request.document_uri.clone()),
                    kind,
                    generation,
                    identity: identity.clone(),
                    freshness: ContextFreshness::Current,
                    producer_state,
                    coverage,
                    revision: TRACEDECAY_CONTEXT_REVISION,
                    retrieval_handle: None,
                },
            );
        }
        Ok(current)
    }

    #[hotpath::measure(label = "usecases.lsp.cycle.current", future = true)]
    pub(super) async fn current_cycle(
        &self,
        root: AdmittedRoot,
        document_uri: Option<String>,
        document_content_digest: Option<ContentDigest>,
    ) -> Result<CurrentFeedbackCycle, LspRuntimeFailure> {
        let mut scope = self.scope.resolve(root, document_uri).await?;
        if document_content_digest.is_some() {
            scope.document_content_digest = document_content_digest;
        }
        let observed_at = now_micros();
        let expires_at = self
            .runtime
            .request_expiry_at(observed_at)
            .map_err(|_| LspRuntimeFailure::new("feedback-request-expiry-unavailable"))?;
        let request_id = mint_global_request_id(GlobalRequestSurface::LspFeedbackDiagnostics)
            .map_err(|_| LspRuntimeFailure::new("feedback-request-identity-unavailable"))?;
        let handle = self
            .runtime
            .mint_diagnostics(
                request_id.as_str(),
                FeedbackDiagnosticsReadRequestV1 {
                    head_commit_id: scope.head_commit_id.clone(),
                },
                observed_at,
            )
            .map_err(|_| LspRuntimeFailure::new("feedback-request-mint-failed"))?;
        let result = self
            .owner
            .invoke(FeedbackReadOperationV1::Diagnostics, &handle, observed_at)
            .await
            .map_err(|_| LspRuntimeFailure::new("feedback-read-unavailable"))?;
        let FeedbackReadInvocationResultV1::Diagnostics(result) = result else {
            return Err(LspRuntimeFailure::new("feedback-read-kind-mismatch"));
        };
        let envelope = result.map_err(|_| LspRuntimeFailure::new("feedback-read-failed"))?;
        let ApplicationOutcome::Evidence(evidence) = envelope.outcome else {
            return Err(LspRuntimeFailure::new("feedback-read-outcome-invalid"));
        };
        // A read that did not complete, or that completed with no cycle to
        // report, is the ordinary state of a project that has ingested nothing
        // yet. Refusing it here denied every first-run project any context
        // projection at all, even though the projection envelope already
        // carries typed coverage and producer state for exactly this case.
        let termination = evidence.execution.termination;
        let Some(payload) = evidence.payload else {
            return Ok(CurrentFeedbackCycle {
                scope,
                result: None,
                termination,
                canonical_handle: handle,
                observed_at,
                expires_at,
            });
        };
        if !feedback_content_is_current(payload.cycle.content_identity.as_ref(), &scope) {
            return Err(LspRuntimeFailure::new("feedback-source-identity-stale"));
        }
        Ok(CurrentFeedbackCycle {
            scope,
            result: Some(payload),
            termination,
            canonical_handle: handle,
            observed_at,
            expires_at,
        })
    }

    #[hotpath::measure(label = "usecases.lsp.context.findings")]
    pub(super) fn current_finding_items<'a>(
        &self,
        target: FindingContextTarget<'_>,
        scope: &LspFeedbackProjectionScope,
        impact_target_file: Option<&tracedecay_domain::FileOccurrenceId>,
        kind: ContextProjectionKind,
        findings: impl Iterator<Item = &'a FeedbackFindingV1>,
        maximum_items: usize,
    ) -> Result<Vec<ContextProjectionItem>, LspRuntimeFailure> {
        let observed_at = now_micros();
        let expires_at = self
            .runtime
            .request_expiry_at(observed_at)
            .map_err(|_| LspRuntimeFailure::new("feedback-finding-expiry-unavailable"))?;
        findings
            .filter(|finding| finding_matches_document(finding, scope, impact_target_file))
            .filter_map(|finding| finding_item(finding).map(|item| (finding, item)))
            .take(maximum_items)
            .map(|(finding, item)| {
                let (canonical_operation, canonical_handle) = if let Some(anchor) =
                    finding.retrieval_anchor_id.as_ref()
                {
                    let page = PageRequest::first(MAX_CONTEXT_PROJECTION_ITEMS as u32)
                        .map_err(|_| LspRuntimeFailure::new("feedback-expand-page-invalid"))?;
                    let handle = self
                        .runtime
                        .mint_expand(
                            mint_global_request_id(GlobalRequestSurface::LspFeedbackExpand)
                                .map_err(|_| {
                                    LspRuntimeFailure::new("feedback-request-identity-unavailable")
                                })?
                                .as_str(),
                            FeedbackExpandRequestV1 {
                                finding_id: finding.finding_id.clone(),
                                expansion: AnchorExpandRequest {
                                    anchor: anchor.clone(),
                                    meta: RetrievalRequestMeta::current(
                                        page,
                                        ResultProjection::ReferencesOnly,
                                        RetrievalOrder::StableIdentity,
                                    ),
                                },
                            },
                            observed_at,
                        )
                        .map_err(|_| LspRuntimeFailure::new("feedback-expand-mint-failed"))?;
                    (FeedbackReadOperationV1::Expand, handle)
                } else {
                    let handle = self
                        .runtime
                        .mint_get(
                            mint_global_request_id(GlobalRequestSurface::LspFeedbackGet)
                                .map_err(|_| {
                                    LspRuntimeFailure::new("feedback-request-identity-unavailable")
                                })?
                                .as_str(),
                            finding.finding_id.clone(),
                            observed_at,
                        )
                        .map_err(|_| LspRuntimeFailure::new("feedback-get-mint-failed"))?;
                    (FeedbackReadOperationV1::Get, handle)
                };
                self.attach_context_handle(
                    target.root,
                    target.document_uri,
                    kind.clone(),
                    scope,
                    observed_at,
                    expires_at,
                    canonical_operation,
                    &canonical_handle,
                    item,
                )
            })
            .collect()
    }

    #[allow(clippy::too_many_arguments)]
    #[hotpath::measure(label = "usecases.lsp.context.store_handle")]
    pub(super) fn attach_context_handle(
        &self,
        root: &AdmittedRoot,
        document_uri: Option<&str>,
        kind: ContextProjectionKind,
        scope: &LspFeedbackProjectionScope,
        observed_at: UtcMicros,
        expires_at: UtcMicros,
        canonical_operation: FeedbackReadOperationV1,
        canonical_handle: &str,
        mut item: ContextProjectionItem,
    ) -> Result<ContextProjectionItem, LspRuntimeFailure> {
        let record = StoredLspContextExpansionV1 {
            schema_version: LSP_CONTEXT_EXPANSION_HANDLE_SCHEMA_VERSION,
            root_uri: root.uri().to_owned(),
            document_uri: document_uri.map(str::to_owned),
            kind,
            stable_id: item.stable_id.clone(),
            scope_digest: self.runtime.scope().scope_digest.as_str().to_owned(),
            identity: scope.projection_identity(),
            generation: scope.generation,
            issued_at: observed_at,
            expires_at,
            canonical_operation,
            canonical_handle: canonical_handle.to_owned(),
        };
        let content = serde_json::to_string(&record)
            .map_err(|_| LspRuntimeFailure::new("context-expansion-handle-invalid"))?;
        let stored = store_response_handle(
            self.runtime.project_root(),
            &content,
            micros_to_seconds(observed_at),
        )
        .map_err(|_| LspRuntimeFailure::new("context-expansion-handle-store-failed"))?;
        item.retrieval_handle = Some(stored.handle);
        Ok(item)
    }

    pub(super) async fn expand_context(
        &self,
        root: AdmittedRoot,
        request: ContextExpansionRequest,
    ) -> ContextExpansionOutcome {
        let observed_at = now_micros();
        let content = match retrieve_response_handle(
            self.runtime.project_root(),
            &request.retrieval_handle,
            micros_to_seconds(observed_at),
        ) {
            Ok(ResponseHandleLookup::Found(record)) => record.content,
            Ok(ResponseHandleLookup::Missing | ResponseHandleLookup::Expired { .. }) => {
                return ContextExpansionOutcome::Denied;
            }
            Err(_) => {
                return ContextExpansionOutcome::Failed {
                    reason: "context-expansion-handle-unavailable".to_owned(),
                };
            }
        };
        let record = match serde_json::from_str::<StoredLspContextExpansionV1>(&content) {
            Ok(record) => record,
            Err(_) => {
                return self.test_runs.expand(root, content).await;
            }
        };
        if self.runtime.request_expiry_at(record.issued_at).ok() != Some(record.expires_at) {
            return ContextExpansionOutcome::Denied;
        }
        if !valid_context_expansion_record(
            &record,
            &root,
            self.runtime.scope().scope_digest.as_str(),
            observed_at,
        ) {
            return ContextExpansionOutcome::Denied;
        }
        let current = match self.scope.resolve(root, record.document_uri.clone()).await {
            Ok(scope) => scope,
            Err(error)
                if matches!(
                    error.class(),
                    "registered-generation-not-current"
                        | "registered-head-unavailable"
                        | "current-generation-read-failed"
                        | "current-generation-unavailable"
                        | "current-generation-invalid"
                ) =>
            {
                return ContextExpansionOutcome::Ready(context_expansion_envelope(
                    record,
                    ContextCoverage::Partial,
                    None,
                    Some("scope-revalidation-unavailable".to_owned()),
                ));
            }
            Err(_) => return ContextExpansionOutcome::Denied,
        };
        if !context_expansion_scope_is_current(&record, &current) {
            return ContextExpansionOutcome::Ready(context_expansion_envelope(
                record,
                ContextCoverage::Partial,
                None,
                Some("stale-generation".to_owned()),
            ));
        }
        let invocation = match self
            .owner
            .invoke(
                record.canonical_operation,
                &record.canonical_handle,
                observed_at,
            )
            .await
        {
            Ok(invocation) => invocation,
            Err(FeedbackReadOwnerErrorV1::NotFoundOrNotAuthorized) => {
                return ContextExpansionOutcome::Denied;
            }
            Err(FeedbackReadOwnerErrorV1::Unavailable)
            | Err(FeedbackReadOwnerErrorV1::Contract(_)) => {
                return ContextExpansionOutcome::Ready(context_expansion_envelope(
                    record,
                    ContextCoverage::Partial,
                    None,
                    Some("canonical-feedback-unavailable".to_owned()),
                ));
            }
        };
        let Ok((complete, evidence)) =
            canonical_feedback_value(record.canonical_operation, invocation)
        else {
            return ContextExpansionOutcome::Failed {
                reason: "context-expansion-kind-mismatch".to_owned(),
            };
        };
        ContextExpansionOutcome::Ready(context_expansion_envelope(
            record,
            if complete {
                ContextCoverage::Complete
            } else {
                ContextCoverage::Partial
            },
            Some(evidence),
            (!complete).then(|| "canonical-feedback-partial".to_owned()),
        ))
    }
}

impl FeedbackCycleRuntimePort for ConcreteFeedbackLspSource {
    fn execute(
        &self,
        request: FeedbackCycleRequest,
    ) -> LspRuntimeFuture<Result<(), LspRuntimeFailure>> {
        let source = self.clone();
        Box::pin(hotpath::future!(
            async move {
                source.cycle.execute(request.clone()).await?;
                // Queueing is best-effort: the cycle above already succeeded, and
                // that is what `execute` reports.
                let _ = source.queue_feedback_changes(&request).await;
                Ok(())
            },
            label = "usecases.lsp.cycle.execute"
        ))
    }
}

impl ManagedDiagnosticSnapshotPort for ConcreteFeedbackLspSource {
    fn snapshot(
        &self,
        request: CanonicalDiagnosticRefreshRequest,
    ) -> LspRuntimeFuture<Result<ManagedDiagnosticSnapshot, LspRuntimeFailure>> {
        let source = self.clone();
        Box::pin(hotpath::future!(
            async move {
                let cycle_request = FeedbackCycleRequest {
                    root_uri: request.root.uri().to_owned(),
                    document_uri: request.document_uri.clone(),
                    trigger: DiagnosticTrigger::ExplicitDocumentDiagnostics,
                };
                source.cycle.execute(cycle_request.clone()).await?;
                let current = source.queue_feedback_changes(&cycle_request).await?;
                let scope = current.scope;
                crate::lsp_support::validate_managed_diagnostic_scope(&request, &scope)?;
                let Some(result) = current.result else {
                    return Err(LspRuntimeFailure::new("feedback-read-incomplete"));
                };
                let cycle = result.cycle;
                let expansion_handles = source
                    .current_finding_items(
                        FindingContextTarget {
                            root: &request.root,
                            document_uri: Some(&request.document_uri),
                        },
                        &scope,
                        cycle.impact.as_ref().map(|impact| &impact.target.file),
                        ContextProjectionKind::diagnostics(),
                        cycle.findings.iter(),
                        MAX_CONTEXT_PROJECTION_ITEMS,
                    )?
                    .into_iter()
                    .filter_map(|item| item.retrieval_handle.map(|handle| (item.stable_id, handle)))
                    .collect();
                let diagnostics = source
                    .diagnostic_projection
                    .project(
                        request.root,
                        request.document_uri,
                        scope.clone(),
                        cycle,
                        expansion_handles,
                    )
                    .await?;
                Ok(ManagedDiagnosticSnapshot {
                    generation: scope.generation,
                    code_generation_id: scope.code_generation_id.clone(),
                    snapshot_digest: scope.snapshot_digest.clone(),
                    authority_digest: crate::lsp_support::managed_diagnostic_authority_digest(
                        &scope,
                    )?,
                    diagnostics,
                })
            },
            label = "usecases.lsp.diagnostics.snapshot"
        ))
    }
}

impl CanonicalContextProjectionAuthority for ConcreteFeedbackLspSource {
    fn registrations(&self) -> Vec<ContextProjectionRegistration> {
        [
            ContextProjectionKind::diagnostics(),
            ContextProjectionKind::post_edit_impact(),
            ContextProjectionKind::affected_tests(),
            ContextProjectionKind::test_run_results(),
            ContextProjectionKind::github_review(),
            ContextProjectionKind::ci_failure_localization(),
            ContextProjectionKind::agent_proximity(),
        ]
        .into_iter()
        .map(|kind| ContextProjectionRegistration {
            kind,
            revision: TRACEDECAY_CONTEXT_REVISION,
        })
        .collect()
    }

    fn snapshot(
        &self,
        root: AdmittedRoot,
        _request_id: LspRequestId,
        request: ContextProjectionRequest,
    ) -> LspRuntimeFuture<ContextProjectionOutcome> {
        if request.kind == ContextProjectionKind::test_run_results() {
            let document_content_digest = request.document_content_digest().cloned();
            return self
                .test_runs
                .snapshot(root, request.document_uri, document_content_digest);
        }
        let source = self.clone();
        Box::pin(hotpath::future!(
            async move {
                let current = match source
                    .current_cycle(
                        root.clone(),
                        request.document_uri.clone(),
                        request.document_content_digest().cloned(),
                    )
                    .await
                {
                    Ok(result) => result,
                    Err(error) => {
                        return ContextProjectionOutcome::Deferred {
                            reason: error.class().to_owned(),
                        };
                    }
                };
                let CurrentFeedbackCycle {
                    scope,
                    result,
                    termination,
                    canonical_handle,
                    observed_at,
                    expires_at,
                } = current;
                let Some(result) = result else {
                    if request.kind != ContextProjectionKind::diagnostics()
                        && request.kind != ContextProjectionKind::post_edit_impact()
                        && request.kind != ContextProjectionKind::affected_tests()
                        && advisory_projection_producer(&request.kind).is_none()
                    {
                        return ContextProjectionOutcome::Unsupported;
                    }
                    let (coverage, producer_state) = incomplete_read_projection(termination);
                    return ContextProjectionOutcome::Ready(ContextProjectionEnvelope {
                        root_uri: root.uri().to_owned(),
                        document_uri: request.document_uri,
                        kind: request.kind,
                        generation: scope.generation,
                        identity: scope.projection_identity(),
                        freshness: ContextFreshness::Current,
                        producer_state,
                        coverage,
                        revision: TRACEDECAY_CONTEXT_REVISION,
                        items: Vec::new(),
                        omitted_count: 0,
                        omission_reasons: projection_omission_reasons(coverage, 0, producer_state),
                        retrieval_handle: None,
                    });
                };
                let cycle = result.cycle;
                let kind = request.kind.clone();
                let (coverage, mut items, omitted_count) = if request.kind
                    == ContextProjectionKind::diagnostics()
                {
                    let items = match source.current_finding_items(
                        FindingContextTarget {
                            root: &root,
                            document_uri: request.document_uri.as_deref(),
                        },
                        &scope,
                        cycle.impact.as_ref().map(|impact| &impact.target.file),
                        ContextProjectionKind::diagnostics(),
                        cycle.findings.iter(),
                        MAX_CONTEXT_PROJECTION_ITEMS,
                    ) {
                        Ok(items) => items,
                        Err(error) => {
                            return ContextProjectionOutcome::Deferred {
                                reason: error.class().to_owned(),
                            };
                        }
                    };
                    let active_findings = cycle
                        .findings
                        .iter()
                        .filter(|finding| {
                            finding_matches_document(
                                finding,
                                &scope,
                                cycle.impact.as_ref().map(|impact| &impact.target.file),
                            )
                        })
                        .filter(|finding| finding.lifecycle == FeedbackFindingLifecycleV1::Active)
                        .count();
                    let omitted_count = usize::try_from(cycle.omitted_findings)
                        .unwrap_or(usize::MAX)
                        .saturating_add(active_findings.saturating_sub(items.len()));
                    let coverage = match (cycle_coverage(&cycle), omitted_count) {
                        (ContextCoverage::Complete, 1..) => ContextCoverage::Partial,
                        (coverage, _) => coverage,
                    };
                    (coverage, items, omitted_count)
                } else if request.kind == ContextProjectionKind::post_edit_impact() {
                    impact_projection(&cycle)
                } else if request.kind == ContextProjectionKind::affected_tests() {
                    affected_test_projection(&cycle)
                } else if let Some(producer) = advisory_projection_producer(&request.kind) {
                    let projected_item_count = cycle
                        .findings
                        .iter()
                        .filter(|finding| {
                            finding_matches_document(
                                finding,
                                &scope,
                                cycle.impact.as_ref().map(|impact| &impact.target.file),
                            )
                        })
                        .filter(|finding| advisory_finding_matches(finding, producer))
                        .filter_map(finding_item)
                        .count();
                    let items = match source.current_finding_items(
                        FindingContextTarget {
                            root: &root,
                            document_uri: request.document_uri.as_deref(),
                        },
                        &scope,
                        cycle.impact.as_ref().map(|impact| &impact.target.file),
                        kind.clone(),
                        cycle
                            .findings
                            .iter()
                            .filter(|finding| advisory_finding_matches(finding, producer)),
                        MAX_CONTEXT_PROJECTION_ITEMS,
                    ) {
                        Ok(items) => items,
                        Err(error) => {
                            return ContextProjectionOutcome::Deferred {
                                reason: error.class().to_owned(),
                            };
                        }
                    };
                    // Cycle omissions are aggregate and carry no producer
                    // attribution. Surface that uncertainty through status,
                    // but count only the bounded items attributable to this
                    // projection so every advisory lane cannot claim the
                    // same omitted finding.
                    let omitted_count =
                        bounded_advisory_item_omissions(projected_item_count, items.len());
                    let coverage = match (
                        advisory_projection_status(&cycle, producer).0,
                        omitted_count,
                    ) {
                        (ContextCoverage::Complete, 1..) => ContextCoverage::Partial,
                        (coverage, _) => coverage,
                    };
                    (coverage, items, omitted_count)
                } else {
                    return ContextProjectionOutcome::Unsupported;
                };
                // Advisory finding items already carry per-finding canonical
                // expand/get handles from `current_finding_items`; replacing them
                // with a cycle-level diagnostics handle would discard the exact
                // authority that expansion must reauthorize.
                if kind != ContextProjectionKind::diagnostics()
                    && advisory_projection_producer(&kind).is_none()
                {
                    items = match items
                        .into_iter()
                        .map(|item| {
                            source.attach_context_handle(
                                &root,
                                request.document_uri.as_deref(),
                                kind.clone(),
                                &scope,
                                observed_at,
                                expires_at,
                                FeedbackReadOperationV1::Diagnostics,
                                &canonical_handle,
                                item,
                            )
                        })
                        .collect()
                    {
                        Ok(items) => items,
                        Err(error) => {
                            return ContextProjectionOutcome::Deferred {
                                reason: error.class().to_owned(),
                            };
                        }
                    };
                }
                let retrieval_handle = match source.attach_context_handle(
                    &root,
                    request.document_uri.as_deref(),
                    kind.clone(),
                    &scope,
                    observed_at,
                    expires_at,
                    FeedbackReadOperationV1::Diagnostics,
                    &canonical_handle,
                    ContextProjectionItem {
                        stable_id: "__projection__".to_owned(),
                        summary: String::new(),
                        retrieval_handle: None,
                    },
                ) {
                    Ok(item) => item.retrieval_handle,
                    Err(error) => {
                        return ContextProjectionOutcome::Deferred {
                            reason: error.class().to_owned(),
                        };
                    }
                };
                let mut producer_state = advisory_projection_producer(&kind)
                    .map(|producer| advisory_projection_status(&cycle, producer).1)
                    .unwrap_or_else(|| producer_state_for_cycle(&cycle));
                if coverage == ContextCoverage::Partial
                    && producer_state == ContextProducerState::Complete
                {
                    producer_state = ContextProducerState::Partial;
                }
                let mut omission_reasons =
                    projection_omission_reasons(coverage, omitted_count, producer_state);
                if advisory_projection_producer(&kind).is_some() && cycle.omitted_findings > 0 {
                    omission_reasons.retain(|reason| reason != "producer-partial");
                    omission_reasons.push("cycle-omissions-unattributed".to_owned());
                }
                ContextProjectionOutcome::Ready(ContextProjectionEnvelope {
                    root_uri: root.uri().to_owned(),
                    document_uri: request.document_uri,
                    kind,
                    generation: scope.generation,
                    identity: scope.projection_identity(),
                    freshness: ContextFreshness::Current,
                    producer_state,
                    coverage,
                    revision: TRACEDECAY_CONTEXT_REVISION,
                    items,
                    omitted_count,
                    omission_reasons,
                    retrieval_handle,
                })
            },
            label = "usecases.lsp.context.snapshot"
        ))
    }

    fn expand(
        &self,
        root: AdmittedRoot,
        _request_id: LspRequestId,
        request: ContextExpansionRequest,
    ) -> LspRuntimeFuture<ContextExpansionOutcome> {
        let source = self.clone();
        Box::pin(hotpath::future!(
            async move { source.expand_context(root, request).await },
            label = "usecases.lsp.context.expand"
        ))
    }

    fn poll_changes(
        &self,
        root: &AdmittedRoot,
        subscriptions: &BTreeSet<ContextProjectionRegistration>,
    ) -> Vec<ContextProjectionChange> {
        ordered_context_changes(
            self.changes.snapshot(root, subscriptions),
            self.test_runs.poll_changes(root, subscriptions),
        )
    }
}

/// Feedback-cycle changes describe the saved edit that triggered the cycle;
/// managed test-run changes are a distinct later execution result. Preserve
/// that production chronology after the bounded feedback lanes have been
/// ordered, rather than giving the test run an artificial earlier rank.
pub(super) fn ordered_context_changes(
    mut feedback_changes: Vec<ContextProjectionChange>,
    test_run_changes: Vec<ContextProjectionChange>,
) -> Vec<ContextProjectionChange> {
    feedback_changes.extend(test_run_changes);
    feedback_changes
}

pub(super) fn feedback_content_is_current(
    content_identity: Option<&FeedbackContentIdentityV1>,
    scope: &LspFeedbackProjectionScope,
) -> bool {
    matches!(
        content_identity,
        Some(FeedbackContentIdentityV1::SavedContent {
            generation_digest,
            file_digest,
        }) if generation_digest == &scope.snapshot_digest
            && scope
                .document_content_digest
                .as_ref()
                .is_none_or(|digest| digest.as_str() == file_digest.as_str())
    )
}

pub(super) fn valid_context_expansion_record(
    record: &StoredLspContextExpansionV1,
    root: &AdmittedRoot,
    scope_digest: &str,
    observed_at: UtcMicros,
) -> bool {
    record.schema_version == LSP_CONTEXT_EXPANSION_HANDLE_SCHEMA_VERSION
        && record.root_uri == root.uri()
        && record
            .document_uri
            .as_deref()
            .is_none_or(|uri| root.contains_document(uri))
        && record.kind.is_valid()
        && !record.stable_id.is_empty()
        && record.stable_id.len() <= MAX_CONTEXT_RETRIEVAL_HANDLE_BYTES
        && record.scope_digest == scope_digest
        && valid_projection_identity(&record.identity)
        && record.issued_at < record.expires_at
        && record.issued_at <= observed_at
        && observed_at < record.expires_at
        && matches!(
            record.canonical_operation,
            FeedbackReadOperationV1::Diagnostics
                | FeedbackReadOperationV1::Get
                | FeedbackReadOperationV1::Expand
        )
        && !record.canonical_handle.is_empty()
        && record.canonical_handle.len() <= MAX_CONTEXT_RETRIEVAL_HANDLE_BYTES
        && record
            .canonical_handle
            .bytes()
            .all(|byte| byte.is_ascii_graphic())
}

pub(super) fn context_expansion_scope_is_current(
    record: &StoredLspContextExpansionV1,
    current: &LspFeedbackProjectionScope,
) -> bool {
    current.projection_identity() == record.identity && current.generation == record.generation
}

pub(super) fn context_expansion_envelope(
    record: StoredLspContextExpansionV1,
    coverage: ContextCoverage,
    evidence: Option<serde_json::Value>,
    omission_reason: Option<String>,
) -> ContextExpansionEnvelope {
    ContextExpansionEnvelope {
        root_uri: record.root_uri,
        document_uri: record.document_uri,
        kind: record.kind,
        stable_id: record.stable_id,
        generation: record.generation,
        scope: ContextExpansionScope {
            scope_digest: record.scope_digest,
            identity: record.identity,
        },
        expires_at: record.expires_at.0,
        coverage,
        revision: TRACEDECAY_CONTEXT_REVISION,
        evidence,
        omission_reason,
        next_retrieval_handle: None,
    }
}

pub(super) fn context_expansion_envelope_for_test_run(
    record: StoredLspTestRunExpansionV1,
    coverage: ContextCoverage,
    evidence: Option<serde_json::Value>,
    omission_reason: Option<String>,
    next_retrieval_handle: Option<String>,
) -> ContextExpansionEnvelope {
    ContextExpansionEnvelope {
        root_uri: record.root_uri,
        document_uri: record.document_uri,
        kind: ContextProjectionKind::test_run_results(),
        stable_id: record.stable_id,
        generation: record.generation,
        scope: ContextExpansionScope {
            scope_digest: record.scope_digest,
            identity: record.identity,
        },
        expires_at: record.expires_at.0,
        coverage,
        revision: record.revision,
        evidence,
        omission_reason,
        next_retrieval_handle,
    }
}

pub(super) fn valid_projection_identity(identity: &ContextProjectionIdentity) -> bool {
    CommitId::new(identity.head_commit_id.clone()).is_ok()
        && CodeGenerationId::new(identity.code_generation_id.clone()).is_ok()
        && ManifestDigest::new(identity.snapshot_digest.clone()).is_ok()
        && ManifestDigest::new(identity.invalidation_digest.clone()).is_ok()
        && ContentDigest::new(identity.snapshot_content_digest.clone()).is_ok()
        && identity
            .document_content_digest
            .as_ref()
            .is_none_or(|digest| ContentDigest::new(digest.clone()).is_ok())
}

pub(super) fn canonical_feedback_value(
    operation: FeedbackReadOperationV1,
    invocation: FeedbackReadInvocationResultV1,
) -> Result<(bool, serde_json::Value), ()> {
    match (operation, invocation) {
        (
            FeedbackReadOperationV1::Diagnostics,
            FeedbackReadInvocationResultV1::Diagnostics(result),
        ) => canonical_application_value(result),
        (FeedbackReadOperationV1::Get, FeedbackReadInvocationResultV1::Get(result)) => {
            canonical_application_value(result)
        }
        (FeedbackReadOperationV1::Expand, FeedbackReadInvocationResultV1::Expand(result)) => {
            canonical_application_value(result)
        }
        _ => Err(()),
    }
}

pub(super) fn canonical_application_value<T: Serialize>(
    result: ApplicationResult<T>,
) -> Result<(bool, serde_json::Value), ()> {
    let complete = result.is_ok();
    serde_json::to_value(result)
        .map(|value| (complete, value))
        .map_err(|_| ())
}
