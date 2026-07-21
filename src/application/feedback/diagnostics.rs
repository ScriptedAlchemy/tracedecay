//! Generation-bound bridge from the diagnostic-store port to application
//! provider ports.
//!
//! The adapter only reads sanitized clean-generation records. It deliberately
//! has no overlay write path and does not retain a local diagnostic cache.

use tracedecay_application::FreshnessState;
use tracedecay_application::diagnostics::{
    CurrentDiagnosticsRequest, DiagnosticProviderPort, DiagnosticProviderResult,
    DiagnosticProviderState, GenerationDiagnosticHistoryPort, GenerationDiagnosticHistoryRequest,
    ProviderSourceIdentity,
};
use tracedecay_domain::GenerationDiagnosticV1;
use tracedecay_store::DiagnosticStore;

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

impl<S> DiagnosticProviderPort for DiagnosticStoreFeedbackProvider<S>
where
    S: DiagnosticStore,
{
    fn current_diagnostics<'a>(
        &'a self,
        _context: &'a tracedecay_application::RequestContext,
        request: &'a CurrentDiagnosticsRequest,
    ) -> tracedecay_application::DiagnosticProviderFuture<'a, Vec<GenerationDiagnosticV1>> {
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
                Ok(records) => provider_result(
                    request.identity.clone(),
                    DiagnosticProviderState::SupportedComplete,
                    Some(records),
                ),
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
        _context: &'a tracedecay_application::RequestContext,
        request: &'a GenerationDiagnosticHistoryRequest,
    ) -> tracedecay_application::DiagnosticProviderFuture<'a, Vec<GenerationDiagnosticV1>> {
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
                Ok(records) => provider_result(
                    request.identity.clone(),
                    DiagnosticProviderState::SupportedComplete,
                    Some(
                        records
                            .into_iter()
                            .filter(|record| record.file_occurrence_id == request.file)
                            .collect(),
                    ),
                ),
                Err(_) => provider_result(
                    request.identity.clone(),
                    DiagnosticProviderState::Unavailable,
                    None,
                ),
            }
        })
    }
}

fn provider_result<T>(
    identity: tracedecay_application::DiagnosticProviderIdentity,
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
    identity: &tracedecay_application::DiagnosticProviderIdentity,
) -> DiagnosticProviderState {
    if identity.freshness.state == FreshnessState::Stale {
        DiagnosticProviderState::Stale
    } else {
        DiagnosticProviderState::Unavailable
    }
}
