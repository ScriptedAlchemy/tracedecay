//! Production LSP semantic routing over retained language analyzers.
//!
//! Code-graph semantics must arrive through a current admitted application
//! query port. The retired `SQLite` graph facade is deliberately absent here;
//! until that port is injected, missing and ambiguous analyzer routes expose
//! the protocol's typed unavailable provider.

use std::path::PathBuf;
use std::sync::Arc;

use futures_util::StreamExt;
use futures_util::stream::FuturesUnordered;
use serde_json::Value;
use tokio::sync::Mutex;
use tracedecay_daemon_protocol::MAX_LSP_FRAME_BYTES;
use tracedecay_lsp::analyzer::broker::{DiagnosticBroker, StdioLspSemanticAuthority};
use tracedecay_lsp::analyzer::client::LspRefreshTimeouts;
use tracedecay_lsp::analyzer::{LanguageSemanticRoute, PolyglotSemanticProvider};
use tracedecay_lsp::{
    AdmittedRoot, LspAnalyzerCancellationAuthority, LspRequestId, LspRuntimeFailure,
    LspRuntimeFuture, LspSemanticOperationOutcome, LspSemanticRequest, LspSemanticRequestAuthority,
    SemanticProviderAdapter, SemanticProviderPort, UnavailableSemanticProvider,
    UpstreamCapabilities,
};

use tracedecay_domain::errors::{Result, TraceDecayError};

use super::UpstreamCapabilityInitializationAuthority;
use crate::lsp_support::{analyzer_runtime_config_error, native_lsp_runtime};

#[derive(Clone)]
pub struct ProductionSemanticAuthorities {
    pub semantics: Arc<dyn SemanticProviderPort + Send + Sync>,
    pub cancellation: Arc<dyn LspAnalyzerCancellationAuthority>,
    pub upstream_capability_initializer: Arc<dyn UpstreamCapabilityInitializationAuthority>,
}

/// Builds the concrete semantic and cancellation trait objects consumed by
/// the production LSP session factory.
///
/// Each installed analyzer remains bound to its declared document extensions.
/// Missing or ambiguous document extensions remain unavailable. Workspace
/// symbols fan out to the retained language authorities; only an analyzer that
/// advertises the operation can contribute a complete result.
///
/// Analyzer processes stay unstarted at project open. An actual LSP session
/// initializes its retained shared client and uses that standard response as
/// the upstream capability authority.
#[tracing::instrument(name = "lsp_runtime.semantic_authorities", level = "trace", skip_all)]
pub async fn production_semantic_authorities(
    diagnostic_broker: Arc<Mutex<DiagnosticBroker>>,
    languages: &[String],
    workspace_root: PathBuf,
    root_uri: impl Into<String>,
    timeouts: LspRefreshTimeouts,
) -> Result<ProductionSemanticAuthorities> {
    let root_uri = root_uri.into();
    let upstream_routes = {
        let mut broker = diagnostic_broker.lock().await;
        let mut routes = Vec::new();
        for language in languages {
            let adapter = broker
                .adapter_for(language)
                .ok_or_else(|| TraceDecayError::Config {
                    message: format!("no LSP adapter registered for language '{language}'"),
                })?;
            if let Some(authority) = broker
                .semantic_authority_if_available(
                    language,
                    workspace_root.clone(),
                    root_uri.clone(),
                    timeouts,
                )
                .await
                .map_err(analyzer_runtime_config_error)?
            {
                routes.push((language, adapter, authority));
            }
        }
        routes
    };

    let fallback: Arc<dyn SemanticProviderPort + Send + Sync> =
        Arc::new(UnavailableSemanticProvider);
    if upstream_routes.is_empty() {
        return Ok(ProductionSemanticAuthorities {
            semantics: fallback,
            cancellation: Arc::new(UnavailableSemanticCancellation),
            upstream_capability_initializer: Arc::new(UnavailableUpstreamCapabilities),
        });
    }

    let mut routes = Vec::with_capacity(upstream_routes.len());
    let mut cancellation: Vec<Arc<dyn LspAnalyzerCancellationAuthority>> =
        Vec::with_capacity(upstream_routes.len());
    let mut initializers = Vec::with_capacity(upstream_routes.len());
    for (_, adapter, upstream) in upstream_routes {
        initializers.push(Arc::clone(&upstream));
        let authority = SemanticProviderAdapter::shared(native_lsp_runtime(), upstream);
        cancellation.push(authority.clone());
        routes.push(LanguageSemanticRoute::new(adapter.extensions, authority));
    }
    let workspace_symbols = SemanticProviderAdapter::shared(
        native_lsp_runtime(),
        Arc::new(WorkspaceSymbolAuthority {
            authorities: initializers.clone(),
        }),
    );
    cancellation.push(workspace_symbols.clone());

    Ok(ProductionSemanticAuthorities {
        semantics: Arc::new(PolyglotSemanticProvider::new(routes, workspace_symbols)),
        cancellation: Arc::new(CompositeSemanticCancellation { cancellation }),
        upstream_capability_initializer: Arc::new(CompositeUpstreamCapabilities { initializers }),
    })
}

/// One admitted operation owns all language futures. The existing semantic
/// adapter retains its single result and supplies capacity/cancellation; this
/// authority adds no independent request registry or polling cache.
struct WorkspaceSymbolAuthority {
    authorities: Vec<Arc<StdioLspSemanticAuthority>>,
}

impl LspSemanticRequestAuthority for WorkspaceSymbolAuthority {
    fn start(
        &self,
        root: AdmittedRoot,
        request_id: LspRequestId,
        request: LspSemanticRequest,
    ) -> LspRuntimeFuture<LspSemanticOperationOutcome> {
        // Polyglot routing also reaches this fallback for unknown or ambiguous
        // document extensions. Never fan a document into unrelated analyzers.
        if request.method() != "workspace/symbol" {
            return Box::pin(async { LspSemanticOperationOutcome::Unavailable });
        }
        let mut pending: FuturesUnordered<_> = self
            .authorities
            .iter()
            .enumerate()
            .map(|(index, authority)| {
                let operation = authority.start(root.clone(), request_id.clone(), request.clone());
                async move { (index, operation.await) }
            })
            .collect();
        let authorities = self.authorities.clone();
        Box::pin(async move {
            // Completion order must not reorder configured language routes.
            // Keep only one transport frame's worth of aggregate results;
            // the session also checks the final JSON-RPC envelope size.
            let mut symbols = vec![Vec::new(); authorities.len()];
            let mut result_bytes: usize = 0;
            let mut supported = false;
            while let Some((index, outcome)) = pending.next().await {
                let failure = match outcome {
                    LspSemanticOperationOutcome::Complete(Value::Array(current)) => {
                        match serde_json::to_vec(&current) {
                            Ok(encoded) => {
                                result_bytes = result_bytes.saturating_add(encoded.len());
                                if result_bytes <= MAX_LSP_FRAME_BYTES {
                                    supported = true;
                                    symbols[index] = current;
                                    continue;
                                }
                                LspSemanticOperationOutcome::Partial {
                                    value: Value::Null,
                                    coverage: "workspace-symbol-response-too-large".to_owned(),
                                    detail: None,
                                }
                            }
                            Err(_) => LspSemanticOperationOutcome::Partial {
                                value: Value::Null,
                                coverage: "analyzer-invalid-response".to_owned(),
                                detail: Some(
                                    LspSemanticOperationOutcome::ANALYZER_INVALID_RESPONSE_DETAIL,
                                ),
                            },
                        }
                    }
                    LspSemanticOperationOutcome::Complete(Value::Null) => {
                        supported = true;
                        continue;
                    }
                    LspSemanticOperationOutcome::Unsupported => continue,
                    LspSemanticOperationOutcome::Partial { .. }
                    | LspSemanticOperationOutcome::Unavailable => outcome,
                    LspSemanticOperationOutcome::Complete(_)
                    | LspSemanticOperationOutcome::RenameCandidate(_) => {
                        LspSemanticOperationOutcome::Partial {
                            value: Value::Null,
                            coverage: "analyzer-invalid-response".to_owned(),
                            detail: Some(
                                LspSemanticOperationOutcome::ANALYZER_INVALID_RESPONSE_DETAIL,
                            ),
                        }
                    }
                };
                // A failed member cannot become a successful empty response,
                // and its peers must not continue after the fan-out settles.
                for authority in &authorities {
                    authority.cancel_request(&root, &request_id);
                }
                return failure;
            }
            if supported {
                LspSemanticOperationOutcome::Complete(Value::Array(
                    symbols.into_iter().flatten().collect(),
                ))
            } else {
                LspSemanticOperationOutcome::Unavailable
            }
        })
    }

    fn cancel_request(&self, root: &AdmittedRoot, request_id: &LspRequestId) -> bool {
        self.authorities.iter().fold(false, |cancelled, authority| {
            authority.cancel_request(root, request_id) | cancelled
        })
    }
}

struct UnavailableUpstreamCapabilities;

impl UpstreamCapabilityInitializationAuthority for UnavailableUpstreamCapabilities {
    fn initialize_upstream_capabilities(
        &self,
    ) -> LspRuntimeFuture<std::result::Result<UpstreamCapabilities, LspRuntimeFailure>> {
        Box::pin(async { Ok(UpstreamCapabilities::default()) })
    }
}

struct CompositeUpstreamCapabilities {
    initializers: Vec<Arc<StdioLspSemanticAuthority>>,
}

impl UpstreamCapabilityInitializationAuthority for CompositeUpstreamCapabilities {
    fn initialize_upstream_capabilities(
        &self,
    ) -> LspRuntimeFuture<std::result::Result<UpstreamCapabilities, LspRuntimeFailure>> {
        let initializers = self.initializers.clone();
        Box::pin(async move {
            let mut capabilities = UpstreamCapabilities::default();
            for initializer in initializers {
                // An analyzer that will not start is absence, not failed
                // admission. The same state a project with no analyzer at all
                // resolves through `UnavailableUpstreamCapabilities`. Failing
                // the composite instead made one unstartable analyzer a
                // permanent gateway outage: `StdioLspSemanticAuthority` records
                // a failed start as terminal, so every later session in the
                // daemon's life also refused, and the whole LSP surface
                // (graph-backed projections and managed diagnostics included)
                // answered `Unavailable`. A rustup host without the
                // `rust-analyzer` component is exactly that shape: the proxy is
                // on PATH, so the language routes, and only the spawn fails.
                let Ok(current) = initializer.upstream_capabilities().await else {
                    continue;
                };
                capabilities.supports_diagnostics |= current.supports_diagnostics;
                capabilities.semantic.extend(current.semantic);
            }
            Ok(capabilities)
        })
    }
}

struct UnavailableSemanticCancellation;

impl LspAnalyzerCancellationAuthority for UnavailableSemanticCancellation {
    fn cancel_request(&self, _root: &AdmittedRoot, _request_id: &LspRequestId) -> bool {
        false
    }
}

struct CompositeSemanticCancellation {
    cancellation: Vec<Arc<dyn LspAnalyzerCancellationAuthority>>,
}

impl LspAnalyzerCancellationAuthority for CompositeSemanticCancellation {
    fn cancel_request(&self, root: &AdmittedRoot, request_id: &LspRequestId) -> bool {
        self.cancellation
            .iter()
            .fold(false, |cancelled, authority| {
                authority.cancel_request(root, request_id) | cancelled
            })
    }
}

#[cfg(test)]
mod tests;
