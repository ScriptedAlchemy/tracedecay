//! Production LSP semantic routing over retained language analyzers.
//!
//! Code-graph semantics must arrive through a current admitted application
//! query port. The retired `SQLite` graph facade is deliberately absent here;
//! until that port is injected, missing and ambiguous analyzer routes expose
//! the protocol's typed unavailable provider.

use std::path::PathBuf;
use std::sync::Arc;

use tokio::runtime::Handle;
use tokio::sync::Mutex;
use tracedecay_lsp::analyzer::broker::{DiagnosticBroker, StdioLspSemanticAuthority};
use tracedecay_lsp::analyzer::client::LspRefreshTimeouts;
use tracedecay_lsp::analyzer::{LanguageSemanticRoute, PolyglotSemanticProvider};
use tracedecay_lsp::{
    AdmittedRoot, LspAnalyzerCancellationAuthority, LspRequestId, LspRuntimeFailure,
    LspRuntimeFuture, SemanticProviderPort, UnavailableSemanticProvider, UpstreamCapabilities,
};

use tracedecay_domain::errors::{Result, TraceDecayError};

use super::{DaemonSemanticProviderAdapter, UpstreamCapabilityInitializationAuthority};
use crate::lsp_support::analyzer_runtime_config_error;

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
/// No analyzer (or an ambiguous extension) resolves through the typed
/// unavailable fallback, and that fallback serves nothing, so the no-analyzer
/// route reports no semantic capability at all.
///
/// Analyzer processes stay unstarted at project open. An actual LSP session
/// initializes its retained shared client and uses that standard response as
/// the upstream capability authority.
#[hotpath::measure(label = "lsp_runtime.semantic_authorities", future = true)]
pub async fn production_semantic_authorities(
    runtime: Handle,
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
        let authority = DaemonSemanticProviderAdapter::shared_protocol(runtime.clone(), upstream);
        cancellation.push(authority.clone());
        routes.push(LanguageSemanticRoute::new(adapter.extensions, authority));
    }

    Ok(ProductionSemanticAuthorities {
        semantics: Arc::new(PolyglotSemanticProvider::new(routes, fallback)),
        cancellation: Arc::new(CompositeSemanticCancellation { cancellation }),
        upstream_capability_initializer: Arc::new(CompositeUpstreamCapabilities { initializers }),
    })
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
                // admission — the same state a project with no analyzer at all
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
