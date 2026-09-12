//! Managed test-run projection over the daemon operation event stream.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex as StdMutex};

use tracedecay_contracts::{OperationTermination, PageRequest, now_micros};
use tracedecay_domain::{ContentDigest, UtcMicros};
use tracedecay_lsp::{
    AdmittedRoot, ContextCoverage, ContextExpansionOutcome, ContextProjectionChange,
    ContextProjectionKind, ContextProjectionOutcome, ContextProjectionRegistration,
    LspRuntimeFailure, LspRuntimeFuture, MAX_CONTEXT_PROJECTION_ITEMS, TRACEDECAY_CONTEXT_REVISION,
};
use tracedecay_session_memory::response_handles::{micros_to_seconds, store_response_handle};

use super::context_projection::test_run_projection;
use super::feedback_source::context_expansion_envelope_for_test_run;
use super::registered_authority::{LspFeedbackProjectionScopePort, RegisteredProjectLspAuthority};
use super::{
    LSP_TEST_RUN_EXPANSION_HANDLE_SCHEMA_VERSION, LSP_TEST_RUN_EXPANSION_TTL_MICROS,
    LspFeedbackProjectionScope, ProjectionChangeQueue, StoredLspTestRunExpansionV1,
};
use crate::operation_stream::{
    CanonicalManagedTestRunReader, ManagedTestRunCurrentScope, ManagedTestRunReadOutcome,
    ManagedTestRunSnapshot, ManagedTestRunStaleReason, ManagedTestRunUnavailableReason,
    operation_event_authority,
};

/// Real test execution result projection owner. Feedback impact owns affected
/// test identities; execution results remain in their separate canonical
/// owner and are not copied into the feedback publication ledger.
pub trait LspTestRunProjectionPort: Send + Sync {
    fn snapshot(
        &self,
        root: AdmittedRoot,
        document_uri: Option<String>,
        document_content_digest: Option<ContentDigest>,
    ) -> LspRuntimeFuture<ContextProjectionOutcome>;

    fn expand(
        &self,
        _root: AdmittedRoot,
        _stored_record: String,
    ) -> LspRuntimeFuture<ContextExpansionOutcome> {
        Box::pin(async { ContextExpansionOutcome::Denied })
    }

    fn poll_changes(
        &self,
        _root: &AdmittedRoot,
        _subscriptions: &BTreeSet<ContextProjectionRegistration>,
    ) -> Vec<ContextProjectionChange> {
        Vec::new()
    }
}

#[derive(Clone)]
pub(crate) struct OperationEventTestRunProjection {
    reader: CanonicalManagedTestRunReader,
    project: Arc<RegisteredProjectLspAuthority>,
    current_scopes: Arc<StdMutex<BTreeMap<String, CachedTestRunScope>>>,
    observed_revisions: Arc<StdMutex<BTreeMap<String, String>>>,
    changes: ProjectionChangeQueue,
}

#[derive(Clone)]
pub(super) struct CachedTestRunScope {
    /// The client's spelling of the document, echoed on every projection and
    /// change it receives. `current` carries the retained document identity.
    document_uri: Option<String>,
    current: ManagedTestRunCurrentScope,
    projection: LspFeedbackProjectionScope,
}

#[derive(Clone)]
pub(super) struct LspTestRunExpansionContext {
    operation_id: String,
    operation_generation: u64,
    operation_completed: u64,
    operation_total: Option<u64>,
    operation_termination: Option<OperationTermination>,
    available_results: usize,
    result_offset: usize,
    page_size: u32,
}

impl OperationEventTestRunProjection {
    pub(crate) fn new(
        reader: CanonicalManagedTestRunReader,
        project: Arc<RegisteredProjectLspAuthority>,
    ) -> Self {
        Self {
            reader,
            project,
            current_scopes: Arc::new(StdMutex::new(BTreeMap::new())),
            observed_revisions: Arc::new(StdMutex::new(BTreeMap::new())),
            changes: ProjectionChangeQueue::default(),
        }
    }

    #[hotpath::measure(label = "usecases.lsp.test_run.store_expansion")]
    pub(super) fn store_expansion(
        &self,
        root: &AdmittedRoot,
        document_uri: Option<&str>,
        scope: &LspFeedbackProjectionScope,
        stable_id: String,
        context: LspTestRunExpansionContext,
    ) -> Result<String, LspRuntimeFailure> {
        let LspTestRunExpansionContext {
            operation_id,
            operation_generation,
            operation_completed,
            operation_total,
            operation_termination,
            available_results,
            result_offset,
            page_size,
        } = context;
        let issued_at = now_micros();
        let record = StoredLspTestRunExpansionV1 {
            schema_version: LSP_TEST_RUN_EXPANSION_HANDLE_SCHEMA_VERSION,
            revision: TRACEDECAY_CONTEXT_REVISION,
            root_uri: root.uri().to_owned(),
            document_uri: document_uri.map(str::to_owned),
            stable_id,
            scope_digest: self
                .project
                .feedback
                .scope()
                .scope_digest
                .as_str()
                .to_owned(),
            identity: scope.projection_identity(),
            generation: scope.generation,
            operation_id,
            operation_generation,
            operation_completed,
            operation_total,
            operation_termination,
            available_results,
            issued_at,
            expires_at: UtcMicros(
                issued_at
                    .0
                    .saturating_add(LSP_TEST_RUN_EXPANSION_TTL_MICROS),
            ),
            result_offset,
            page_size,
        };
        let content = serde_json::to_string(&record)
            .map_err(|_| LspRuntimeFailure::new("test-run-expansion-handle-invalid"))?;
        store_response_handle(
            &self.project.project_root,
            &content,
            micros_to_seconds(issued_at),
        )
        .map(|stored| stored.handle)
        .map_err(|_| LspRuntimeFailure::new("test-run-expansion-handle-store-failed"))
    }
}

pub(crate) fn lsp_test_result_port(
    project: Arc<RegisteredProjectLspAuthority>,
) -> Arc<dyn LspTestRunProjectionPort> {
    Arc::new(OperationEventTestRunProjection::new(
        CanonicalManagedTestRunReader::new(operation_event_authority()),
        project,
    ))
}

impl LspTestRunProjectionPort for OperationEventTestRunProjection {
    fn snapshot(
        &self,
        root: AdmittedRoot,
        document_uri: Option<String>,
        document_content_digest: Option<ContentDigest>,
    ) -> LspRuntimeFuture<ContextProjectionOutcome> {
        let projection = self.clone();
        Box::pin(hotpath::future!(
            async move {
                let mut scope = match projection
                    .project
                    .resolve(root.clone(), document_uri.clone())
                    .await
                {
                    Ok(scope) => scope,
                    Err(error) => {
                        return ContextProjectionOutcome::Deferred {
                            reason: error.class().to_owned(),
                        };
                    }
                };
                if let Err(reason) =
                    bind_test_run_document_content(&mut scope, document_content_digest)
                {
                    return ContextProjectionOutcome::Deferred {
                        reason: reason.to_owned(),
                    };
                }
                let retained_document_uri = match projection.project.retained_document_uri(&scope) {
                    Ok(uri) => uri,
                    Err(error) => {
                        return ContextProjectionOutcome::Deferred {
                            reason: error.class().to_owned(),
                        };
                    }
                };
                let current = ManagedTestRunCurrentScope {
                    root_uri: root.uri().to_owned(),
                    head_commit_id: Some(scope.head_commit_id.clone()),
                    code_generation_id: Some(scope.code_generation_id.clone()),
                    document_uri: retained_document_uri,
                    document_content_digest: scope.document_content_digest.clone(),
                };
                let scope_key = current_scope_key(root.uri(), document_uri.as_deref());
                projection
                    .current_scopes
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(
                        scope_key.clone(),
                        CachedTestRunScope {
                            document_uri: document_uri.clone(),
                            current: current.clone(),
                            projection: scope.clone(),
                        },
                    );
                let page = match PageRequest::first(MAX_CONTEXT_PROJECTION_ITEMS as u32) {
                    Ok(page) => page,
                    Err(_) => {
                        return ContextProjectionOutcome::Deferred {
                            reason: "managed-test-run-page-invalid".to_owned(),
                        };
                    }
                };
                match projection.reader.latest_current_page(&current, &page).await {
                    ManagedTestRunReadOutcome::Current(snapshot) => {
                        projection
                            .observed_revisions
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .insert(scope_key, test_run_source_revision(&snapshot));
                        let expansion_context = LspTestRunExpansionContext {
                            operation_id: snapshot.operation_id.to_string(),
                            operation_generation: snapshot.generation,
                            operation_completed: snapshot.completed,
                            operation_total: snapshot.total,
                            operation_termination: snapshot.termination,
                            available_results: snapshot.available_results,
                            result_offset: snapshot.result_offset,
                            page_size: 1,
                        };
                        let has_bounded_results = snapshot.next_cursor.is_some();
                        let mut outcome = test_run_projection(
                            root.clone(),
                            document_uri.clone(),
                            current.document_uri.as_deref(),
                            scope.clone(),
                            snapshot,
                        );
                        if let ContextProjectionOutcome::Ready(envelope) = &mut outcome {
                            for (index, item) in envelope.items.iter_mut().enumerate() {
                                item.retrieval_handle = match projection.store_expansion(
                                    &root,
                                    document_uri.as_deref(),
                                    &scope,
                                    item.stable_id.clone(),
                                    LspTestRunExpansionContext {
                                        result_offset: expansion_context
                                            .result_offset
                                            .saturating_add(index),
                                        ..expansion_context.clone()
                                    },
                                ) {
                                    Ok(handle) => Some(handle),
                                    Err(error) => {
                                        return ContextProjectionOutcome::Deferred {
                                            reason: error.class().to_owned(),
                                        };
                                    }
                                };
                            }
                            if has_bounded_results && !envelope.items.is_empty() {
                                envelope.retrieval_handle = match projection.store_expansion(
                                    &root,
                                    document_uri.as_deref(),
                                    &scope,
                                    format!("{}.__remaining__", expansion_context.operation_id),
                                    LspTestRunExpansionContext {
                                        result_offset: expansion_context
                                            .result_offset
                                            .saturating_add(envelope.items.len()),
                                        page_size: MAX_CONTEXT_PROJECTION_ITEMS as u32,
                                        ..expansion_context
                                    },
                                ) {
                                    Ok(handle) => Some(handle),
                                    Err(error) => {
                                        return ContextProjectionOutcome::Deferred {
                                            reason: error.class().to_owned(),
                                        };
                                    }
                                };
                            }
                        }
                        outcome
                    }
                    ManagedTestRunReadOutcome::Unavailable(
                        ManagedTestRunUnavailableReason::FrontierExpired,
                    ) => ContextProjectionOutcome::Deferred {
                        reason: "managed-test-run-frontier-expired".to_owned(),
                    },
                    ManagedTestRunReadOutcome::Unavailable(
                        ManagedTestRunUnavailableReason::RetainedHeadUnbound,
                    ) => ContextProjectionOutcome::Deferred {
                        reason: "managed-test-run-head-unbound".to_owned(),
                    },
                    ManagedTestRunReadOutcome::Unavailable(
                        ManagedTestRunUnavailableReason::RetainedCodeGenerationUnbound,
                    ) => ContextProjectionOutcome::Deferred {
                        reason: "managed-test-run-code-generation-unbound".to_owned(),
                    },
                    ManagedTestRunReadOutcome::Unavailable(
                        ManagedTestRunUnavailableReason::CurrentDocumentUnbound
                        | ManagedTestRunUnavailableReason::RetainedDocumentUnbound,
                    ) => ContextProjectionOutcome::Deferred {
                        reason: "managed-test-run-document-content-unbound".to_owned(),
                    },
                    ManagedTestRunReadOutcome::Unavailable(
                        ManagedTestRunUnavailableReason::CurrentHeadUnbound
                        | ManagedTestRunUnavailableReason::CurrentCodeGenerationUnbound,
                    ) => ContextProjectionOutcome::Deferred {
                        reason: "managed-test-run-current-identity-unbound".to_owned(),
                    },
                    ManagedTestRunReadOutcome::Stale(ManagedTestRunStaleReason::SourceIdentity) => {
                        ContextProjectionOutcome::Deferred {
                            reason: "managed-test-run-source-identity-stale".to_owned(),
                        }
                    }
                    ManagedTestRunReadOutcome::Stale(
                        ManagedTestRunStaleReason::DocumentContent,
                    ) => ContextProjectionOutcome::Deferred {
                        reason: "managed-test-run-document-content-stale".to_owned(),
                    },
                    ManagedTestRunReadOutcome::Unavailable(
                        ManagedTestRunUnavailableReason::AuthorityFailure,
                    ) => ContextProjectionOutcome::Failed {
                        reason: "managed-test-run-projection-failed".to_owned(),
                    },
                }
            },
            label = "usecases.lsp.test_run.snapshot"
        ))
    }

    fn expand(
        &self,
        root: AdmittedRoot,
        stored_record: String,
    ) -> LspRuntimeFuture<ContextExpansionOutcome> {
        self.expand_stored(root, stored_record)
    }

    #[hotpath::measure(label = "usecases.lsp.test_run.poll_changes")]
    fn poll_changes(
        &self,
        root: &AdmittedRoot,
        subscriptions: &BTreeSet<ContextProjectionRegistration>,
    ) -> Vec<ContextProjectionChange> {
        let registration = ContextProjectionRegistration {
            kind: ContextProjectionKind::test_run_results(),
            revision: TRACEDECAY_CONTEXT_REVISION,
        };
        if !subscriptions.contains(&registration) {
            return Vec::new();
        }
        let scopes = self
            .current_scopes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .filter(|scope| scope.current.root_uri == root.uri())
            .cloned()
            .collect::<Vec<_>>();
        for cached in scopes {
            let current = cached.current;
            let Some(ManagedTestRunReadOutcome::Current(snapshot)) =
                self.reader.try_latest_current(&current)
            else {
                continue;
            };
            let key = current_scope_key(root.uri(), cached.document_uri.as_deref());
            let source_revision = test_run_source_revision(&snapshot);
            let changed = {
                let mut observed = self
                    .observed_revisions
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                match observed.insert(key, source_revision.clone()) {
                    Some(previous) => previous != source_revision,
                    None => false,
                }
            };
            if !changed {
                continue;
            }
            let ContextProjectionOutcome::Ready(envelope) = test_run_projection(
                root.clone(),
                cached.document_uri,
                current.document_uri.as_deref(),
                cached.projection,
                snapshot,
            ) else {
                continue;
            };
            self.changes.offer(
                source_revision,
                ContextProjectionChange {
                    root_uri: envelope.root_uri,
                    document_uri: envelope.document_uri,
                    kind: envelope.kind,
                    generation: envelope.generation,
                    identity: envelope.identity,
                    freshness: envelope.freshness,
                    producer_state: envelope.producer_state,
                    coverage: envelope.coverage,
                    revision: envelope.revision,
                    retrieval_handle: envelope.retrieval_handle,
                },
            );
        }
        self.changes.snapshot(root, subscriptions)
    }
}

pub(super) fn current_scope_key(root_uri: &str, document_uri: Option<&str>) -> String {
    format!("{root_uri}\u{0}{}", document_uri.unwrap_or_default())
}

pub(super) fn test_run_source_revision(snapshot: &ManagedTestRunSnapshot) -> String {
    format!("{}:{}", snapshot.operation_id, snapshot.source_revision)
}

/// A retained managed test run is evidence about saved source. An LSP overlay
/// may reuse it only while its exact bytes still match that saved document.
pub(super) fn bind_test_run_document_content(
    scope: &mut LspFeedbackProjectionScope,
    overlay_digest: Option<ContentDigest>,
) -> Result<(), &'static str> {
    let Some(overlay_digest) = overlay_digest else {
        return Ok(());
    };
    let Some(saved_digest) = scope.document_content_digest.as_ref() else {
        return Err("managed-test-run-document-content-unbound");
    };
    if saved_digest != &overlay_digest {
        return Err("managed-test-run-document-content-stale");
    }
    scope.document_content_digest = Some(overlay_digest);
    Ok(())
}

impl OperationEventTestRunProjection {
    pub(super) fn expand_stored(
        &self,
        root: AdmittedRoot,
        stored_record: String,
    ) -> LspRuntimeFuture<ContextExpansionOutcome> {
        let projection = self.clone();
        Box::pin(hotpath::future!(
            async move {
                let Ok(record) =
                    serde_json::from_str::<StoredLspTestRunExpansionV1>(&stored_record)
                else {
                    return ContextExpansionOutcome::Denied;
                };
                let observed_at = now_micros();
                if record.schema_version != LSP_TEST_RUN_EXPANSION_HANDLE_SCHEMA_VERSION
                    || record.revision != TRACEDECAY_CONTEXT_REVISION
                    || record.root_uri != root.uri()
                    || record.issued_at >= record.expires_at
                    || observed_at < record.issued_at
                    || observed_at >= record.expires_at
                    || record.page_size == 0
                    || record.page_size > MAX_CONTEXT_PROJECTION_ITEMS as u32
                {
                    return ContextExpansionOutcome::Denied;
                }
                let scope = match projection
                    .project
                    .resolve(root.clone(), record.document_uri.clone())
                    .await
                {
                    Ok(scope) => scope,
                    Err(_) => return ContextExpansionOutcome::Denied,
                };
                if projection.project.feedback.scope().scope_digest.as_str() != record.scope_digest
                {
                    return ContextExpansionOutcome::Denied;
                }
                if scope.generation != record.generation
                    || scope.projection_identity() != record.identity
                {
                    return ContextExpansionOutcome::Ready(
                        context_expansion_envelope_for_test_run(
                            record,
                            ContextCoverage::Partial,
                            None,
                            Some("stale-generation".to_owned()),
                            None,
                        ),
                    );
                }
                let Ok(retained_document_uri) = projection.project.retained_document_uri(&scope)
                else {
                    return ContextExpansionOutcome::Denied;
                };
                let current = ManagedTestRunCurrentScope {
                    root_uri: root.uri().to_owned(),
                    head_commit_id: Some(scope.head_commit_id.clone()),
                    code_generation_id: Some(scope.code_generation_id.clone()),
                    document_uri: retained_document_uri,
                    document_content_digest: scope.document_content_digest.clone(),
                };
                let ManagedTestRunReadOutcome::Current(snapshot) =
                    projection.reader.latest_current(&current).await
                else {
                    return ContextExpansionOutcome::Denied;
                };
                if snapshot.operation_id.to_string() != record.operation_id
                    || snapshot.generation != record.operation_generation
                    || snapshot.completed != record.operation_completed
                    || snapshot.total != record.operation_total
                    || snapshot.termination != record.operation_termination
                    || snapshot.results.len() != record.available_results
                {
                    return ContextExpansionOutcome::Ready(
                        context_expansion_envelope_for_test_run(
                            record,
                            ContextCoverage::Partial,
                            None,
                            Some("stale-generation".to_owned()),
                            None,
                        ),
                    );
                }
                let end = record
                    .result_offset
                    .saturating_add(record.page_size as usize)
                    .min(snapshot.results.len());
                if record.result_offset >= snapshot.results.len() {
                    return ContextExpansionOutcome::Denied;
                }
                let results = snapshot.results[record.result_offset..end]
                    .iter()
                    .map(|result| {
                        serde_json::json!({
                            "test": result.test,
                            "passed": result.passed,
                        })
                    })
                    .collect::<Vec<_>>();
                let result_offset = record.result_offset;
                let next_retrieval_handle = if end < snapshot.results.len() {
                    match projection.store_expansion(
                        &root,
                        record.document_uri.as_deref(),
                        &scope,
                        record.stable_id.clone(),
                        LspTestRunExpansionContext {
                            operation_id: record.operation_id.clone(),
                            operation_generation: record.operation_generation,
                            operation_completed: record.operation_completed,
                            operation_total: record.operation_total,
                            operation_termination: record.operation_termination,
                            available_results: record.available_results,
                            result_offset: end,
                            page_size: MAX_CONTEXT_PROJECTION_ITEMS as u32,
                        },
                    ) {
                        Ok(handle) => Some(handle),
                        Err(_) => return ContextExpansionOutcome::Denied,
                    }
                } else {
                    None
                };
                let coverage = if next_retrieval_handle.is_some() {
                    ContextCoverage::Partial
                } else {
                    ContextCoverage::Complete
                };
                let omission_reason = next_retrieval_handle
                    .as_ref()
                    .map(|_| "bounded-projection-items".to_owned());
                ContextExpansionOutcome::Ready(context_expansion_envelope_for_test_run(
                    record,
                    coverage,
                    Some(serde_json::json!({
                        "results": results,
                        "result_offset": result_offset,
                        "available_results": snapshot.results.len(),
                        "next_retrieval_handle": next_retrieval_handle,
                    })),
                    omission_reason,
                    next_retrieval_handle,
                ))
            },
            label = "usecases.lsp.test_run.expand"
        ))
    }
}
