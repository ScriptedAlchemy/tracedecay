//! Production LSP semantic routing over retained language analyzers.
//!
//! Code-graph semantics must arrive through a current admitted application
//! query port. The retired SQLite graph facade is deliberately absent here;
//! until that port is injected, missing and ambiguous analyzer routes expose
//! the protocol's typed unavailable provider.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::runtime::Handle;
use tokio::sync::Mutex;
use tracedecay_lsp::analyzer::broker::DiagnosticBroker;
use tracedecay_lsp::analyzer::client::LspRefreshTimeouts;
use tracedecay_lsp::analyzer::{LanguageSemanticRoute, PolyglotSemanticProvider};
use tracedecay_lsp::{
    AdmittedRoot, LspAnalyzerCancellationAuthority, LspRequestId, SemanticCapability,
    SemanticProviderPort, UnavailableSemanticProvider,
};

use crate::errors::{Result, TraceDecayError};
use tracedecay_usecases::lsp_runtime::DaemonSemanticProviderAdapter;

#[derive(Clone)]
pub struct ProductionSemanticAuthorities {
    pub semantics: Arc<dyn SemanticProviderPort + Send + Sync>,
    pub cancellation: Arc<dyn LspAnalyzerCancellationAuthority>,
    pub analyzer_available: bool,
    pub semantic_capabilities: BTreeSet<SemanticCapability>,
}

/// Builds the concrete semantic and cancellation trait objects consumed by
/// the production LSP session factory.
///
/// Each installed analyzer remains bound to its declared document extensions.
/// No analyzer (or an ambiguous extension) resolves through the typed
/// unavailable fallback. Analyzer availability alone does not prove support
/// for any semantic method, so capabilities remain empty until negotiation or
/// a current application query port supplies exact evidence.
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
            if let Some(authority) = broker.semantic_authority_if_available(
                language,
                workspace_root.clone(),
                root_uri.clone(),
                timeouts,
            )? {
                routes.push((adapter, authority));
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
            analyzer_available: false,
            semantic_capabilities: BTreeSet::new(),
        });
    }

    let mut routes = Vec::with_capacity(upstream_routes.len());
    let mut cancellation: Vec<Arc<dyn LspAnalyzerCancellationAuthority>> =
        Vec::with_capacity(upstream_routes.len());
    for (adapter, upstream) in upstream_routes {
        let authority = DaemonSemanticProviderAdapter::shared_protocol(runtime.clone(), upstream);
        cancellation.push(authority.clone());
        routes.push(LanguageSemanticRoute::new(adapter.extensions, authority));
    }

    Ok(ProductionSemanticAuthorities {
        semantics: Arc::new(PolyglotSemanticProvider::new(routes, fallback)),
        cancellation: Arc::new(CompositeSemanticCancellation { cancellation }),
        analyzer_available: true,
        semantic_capabilities: BTreeSet::new(),
    })
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
