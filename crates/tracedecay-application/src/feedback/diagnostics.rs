//! Generation-bound bridge from the diagnostic-store port to application
//! provider ports.
//!
//! The adapter only reads sanitized clean-generation records. It deliberately
//! has no overlay write path and does not retain a local diagnostic cache.

use tracedecay_contracts::FreshnessState;
use tracedecay_contracts::diagnostics::{
    CurrentDiagnosticsRequest, DiagnosticProviderPort, DiagnosticProviderResult,
    DiagnosticProviderState, GenerationDiagnosticHistoryPort, GenerationDiagnosticHistoryRequest,
    ProviderSourceIdentity,
};
use tracedecay_domain::{CodeGenerationId, GenerationDiagnosticV1, RetrievalAnchorId};
use tracedecay_store::{
    DiagnosticPublicationReceiptV1, DiagnosticStore, DiagnosticStoreResult,
    SanitizedCleanDiagnosticSnapshotV1,
};

use crate::diagnostics_store::DiagnosticsStore;
use crate::lsp_runtime::LspFeedbackDiagnosticRecordPort;
use tracedecay_runtime_core::db::Database;

/// Owned adapter that lets long-lived feedback runtimes reuse the canonical
/// diagnostics store without retaining a borrowed database connection.
#[derive(Clone)]
pub struct DatabaseDiagnosticStore {
    database: Database,
}

impl DatabaseDiagnosticStore {
    pub fn new(database: Database) -> Self {
        Self { database }
    }
}

impl LspFeedbackDiagnosticRecordPort for DatabaseDiagnosticStore {
    fn diagnostic_by_anchor(
        &self,
        anchor: RetrievalAnchorId,
    ) -> tracedecay_lsp::LspRuntimeFuture<
        Result<Option<GenerationDiagnosticV1>, tracedecay_lsp::LspRuntimeFailure>,
    > {
        let database = self.database.clone();
        Box::pin(async move {
            DiagnosticsStore::new(database)
                .diagnostic_by_anchor(&anchor)
                .await
                .map_err(|_| {
                    tracedecay_lsp::LspRuntimeFailure::new("diagnostic-anchor-read-failed")
                })
        })
    }
}

impl DiagnosticStore for DatabaseDiagnosticStore {
    async fn publish_clean_diagnostics(
        &self,
        snapshot: SanitizedCleanDiagnosticSnapshotV1,
    ) -> DiagnosticStoreResult<DiagnosticPublicationReceiptV1> {
        DiagnosticsStore::new(self.database.clone())
            .publish_clean_diagnostics(snapshot)
            .await
    }

    async fn current_diagnostic_generation(
        &self,
    ) -> DiagnosticStoreResult<Option<CodeGenerationId>> {
        DiagnosticsStore::new(self.database.clone())
            .current_diagnostic_generation()
            .await
    }

    #[hotpath::measure(label = "usecases.diagnostics.for_generation", future = true)]
    async fn diagnostics_for_generation(
        &self,
        generation: &CodeGenerationId,
    ) -> DiagnosticStoreResult<Vec<GenerationDiagnosticV1>> {
        let records = DiagnosticsStore::new(self.database.clone())
            .diagnostics_for_generation(generation)
            .await?;
        crate::hotpath_observe::feedback_query(records.len());
        Ok(records)
    }

    async fn diagnostics_for_publication(
        &self,
        generation: &CodeGenerationId,
        publication_revision: u64,
    ) -> DiagnosticStoreResult<Vec<GenerationDiagnosticV1>> {
        DiagnosticsStore::new(self.database.clone())
            .diagnostics_for_publication(generation, publication_revision)
            .await
    }

    #[hotpath::measure(label = "usecases.diagnostics.current", future = true)]
    async fn current_diagnostics(
        &self,
        generation: &CodeGenerationId,
    ) -> DiagnosticStoreResult<Vec<GenerationDiagnosticV1>> {
        let records = DiagnosticsStore::new(self.database.clone())
            .current_diagnostics(generation)
            .await?;
        crate::hotpath_observe::feedback_query(records.len());
        Ok(records)
    }

    #[hotpath::measure(label = "usecases.diagnostics.current_file", future = true)]
    async fn current_diagnostics_for_file(
        &self,
        generation: &CodeGenerationId,
        file_occurrence_id: &tracedecay_domain::FileOccurrenceId,
    ) -> DiagnosticStoreResult<Vec<GenerationDiagnosticV1>> {
        DiagnosticsStore::new(self.database.clone())
            .current_diagnostics_for_file(generation, file_occurrence_id)
            .await
    }

    async fn stale_diagnostics(
        &self,
        generation: &CodeGenerationId,
    ) -> DiagnosticStoreResult<Vec<GenerationDiagnosticV1>> {
        DiagnosticsStore::new(self.database.clone())
            .stale_diagnostics(generation)
            .await
    }

    #[hotpath::measure(label = "usecases.diagnostics.by_anchor", future = true)]
    async fn diagnostic_by_anchor(
        &self,
        anchor: &RetrievalAnchorId,
    ) -> DiagnosticStoreResult<Option<GenerationDiagnosticV1>> {
        DiagnosticsStore::new(self.database.clone())
            .diagnostic_by_anchor(anchor)
            .await
    }

    async fn diagnostic_supersession_chain(
        &self,
        anchor: &RetrievalAnchorId,
    ) -> DiagnosticStoreResult<Vec<GenerationDiagnosticV1>> {
        DiagnosticsStore::new(self.database.clone())
            .diagnostic_supersession_chain(anchor)
            .await
    }

    async fn supersede_diagnostic_generation(
        &self,
        prior_generation: &CodeGenerationId,
        successor_generation: &CodeGenerationId,
    ) -> DiagnosticStoreResult<u64> {
        DiagnosticsStore::new(self.database.clone())
            .supersede_diagnostic_generation(prior_generation, successor_generation)
            .await
    }
}

/// Concrete adapter over the existing diagnostic-store read port. It is kept
/// independent of daemon composition so a daemon can bind its own admitted
/// analyzer/provider construction without introducing a second store.
pub struct DiagnosticStoreFeedbackProvider<S> {
    store: S,
}

impl<S> DiagnosticStoreFeedbackProvider<S> {
    pub fn new(store: S) -> Self {
        Self { store }
    }
}

impl<S> DiagnosticStoreFeedbackProvider<S>
where
    S: DiagnosticStore,
{
    /// Selects the admitted provider identities represented by the exact
    /// current stored publication for this document. An empty result means no
    /// publication has yet established producer provenance.
    pub(crate) async fn current_publication_providers(
        &self,
        providers: &[tracedecay_contracts::DiagnosticProviderIdentity],
        input: &tracedecay_domain::feedback::FeedbackEvaluationInputV1,
    ) -> Result<Vec<tracedecay_contracts::DiagnosticProviderIdentity>, ()> {
        let Some(first) = providers.first() else {
            return Ok(Vec::new());
        };
        let ProviderSourceIdentity::CleanGeneration { generation } = &first.source else {
            return Ok(Vec::new());
        };
        let current = self
            .store
            .current_diagnostic_generation()
            .await
            .map_err(|_| ())?;
        if current.as_ref() != Some(generation) {
            return Ok(Vec::new());
        }
        let records = self
            .store
            .current_diagnostics_for_file(generation, &first.document.file)
            .await
            .map_err(|_| ())?;
        let selected = providers
            .iter()
            .filter(|provider| {
                records.iter().any(|record| {
                    record_matches_provider(record, provider)
                        && record.repository == provider.scope.repository_id
                        && record.worktree.as_ref() == Some(&provider.scope.worktree_id)
                        && record.reference.as_ref() == provider.scope.reference.as_ref()
                        && record.file_occurrence_id == provider.document.file
                        && record.content_digest == provider.document.content_digest
                        && record
                            .source_revision
                            .as_ref()
                            .is_none_or(|revision| revision == &input.request.scope.head_commit_id)
                })
            })
            .cloned()
            .collect::<Vec<_>>();
        Ok(selected)
    }
}

impl<S> DiagnosticProviderPort for DiagnosticStoreFeedbackProvider<S>
where
    S: DiagnosticStore,
{
    fn current_diagnostics<'a>(
        &'a self,
        _context: &'a tracedecay_contracts::RequestContext,
        request: &'a CurrentDiagnosticsRequest,
    ) -> tracedecay_contracts::DiagnosticProviderFuture<'a, Vec<GenerationDiagnosticV1>> {
        Box::pin(async move {
            if request.validate().is_err() {
                return provider_result(
                    request.identity.clone(),
                    DiagnosticProviderState::Unavailable,
                    None,
                );
            }
            let ProviderSourceIdentity::CleanGeneration { generation } = &request.identity.source
            else {
                return provider_result(
                    request.identity.clone(),
                    DiagnosticProviderState::Unsupported,
                    None,
                );
            };
            let current = match self.store.current_diagnostic_generation().await {
                Ok(Some(current)) if current == *generation => current,
                Ok(Some(_) | None) => {
                    return provider_result(
                        request.identity.clone(),
                        stale_or_unavailable(&request.identity),
                        None,
                    );
                }
                Err(_) => {
                    return provider_result(
                        request.identity.clone(),
                        DiagnosticProviderState::Unavailable,
                        None,
                    );
                }
            };
            match self
                .store
                .current_diagnostics_for_file(&current, &request.identity.document.file)
                .await
            {
                Ok(records) => {
                    let records = records
                        .into_iter()
                        .filter(|record| record_matches_provider(record, &request.identity))
                        .collect::<Vec<_>>();
                    if records.is_empty() {
                        provider_result(
                            request.identity.clone(),
                            DiagnosticProviderState::Unavailable,
                            None,
                        )
                    } else {
                        provider_result(
                            request.identity.clone(),
                            DiagnosticProviderState::SupportedComplete,
                            Some(records),
                        )
                    }
                }
                Err(_) => provider_result(
                    request.identity.clone(),
                    DiagnosticProviderState::Unavailable,
                    None,
                ),
            }
        })
    }
}

impl<S> GenerationDiagnosticHistoryPort for DiagnosticStoreFeedbackProvider<S>
where
    S: DiagnosticStore,
{
    fn diagnostics_for_generation<'a>(
        &'a self,
        _context: &'a tracedecay_contracts::RequestContext,
        request: &'a GenerationDiagnosticHistoryRequest,
    ) -> tracedecay_contracts::DiagnosticProviderFuture<'a, Vec<GenerationDiagnosticV1>> {
        Box::pin(async move {
            if request.validate().is_err() {
                return provider_result(
                    request.identity.clone(),
                    DiagnosticProviderState::Unavailable,
                    None,
                );
            }
            match self
                .store
                .diagnostics_for_generation(&request.generation)
                .await
            {
                Ok(records) => {
                    let records = records
                        .into_iter()
                        .filter(|record| {
                            record.file_occurrence_id == request.file
                                && record_matches_provider(record, &request.identity)
                        })
                        .collect::<Vec<_>>();
                    if records.is_empty() {
                        provider_result(
                            request.identity.clone(),
                            DiagnosticProviderState::Unavailable,
                            None,
                        )
                    } else {
                        provider_result(
                            request.identity.clone(),
                            DiagnosticProviderState::SupportedComplete,
                            Some(records),
                        )
                    }
                }
                Err(_) => provider_result(
                    request.identity.clone(),
                    DiagnosticProviderState::Unavailable,
                    None,
                ),
            }
        })
    }
}

fn record_matches_provider(
    record: &GenerationDiagnosticV1,
    identity: &tracedecay_contracts::DiagnosticProviderIdentity,
) -> bool {
    record.provenance.producer == identity.producer.provider
        && record.provenance.analyzer_revision == identity.producer.analyzer_revision
        && record.provenance.configuration_revision == identity.configuration.revision
}

fn provider_result<T>(
    identity: tracedecay_contracts::DiagnosticProviderIdentity,
    state: DiagnosticProviderState,
    payload: Option<T>,
) -> DiagnosticProviderResult<T> {
    DiagnosticProviderResult::new(identity.clone(), state, payload).unwrap_or_else(|_| {
        // A malformed caller identity cannot be made valid by this
        // translation boundary. Preserve it only as an unavailable result so
        // the application adapter can reject it without panicking.
        DiagnosticProviderResult {
            identity,
            state: DiagnosticProviderState::Unavailable,
            payload: None,
        }
    })
}

fn stale_or_unavailable(
    identity: &tracedecay_contracts::DiagnosticProviderIdentity,
) -> DiagnosticProviderState {
    if identity.freshness.state == FreshnessState::Stale {
        DiagnosticProviderState::Stale
    } else {
        DiagnosticProviderState::Unavailable
    }
}
