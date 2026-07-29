use std::collections::BTreeSet;
use std::sync::Arc;

use url::Url;

use crate::{
    AdmittedRoot, AnalyzerCancellationPort, LspRequestId, SemanticProviderOutcome,
    SemanticProviderPort, SemanticRequest, SemanticResponse,
};

/// One extension set routed to one language-specific semantic provider.
pub struct LanguageSemanticRoute {
    extensions: BTreeSet<String>,
    provider: Arc<dyn SemanticProviderPort + Send + Sync>,
}

impl LanguageSemanticRoute {
    pub fn new(
        extensions: impl IntoIterator<Item = impl Into<String>>,
        provider: Arc<dyn SemanticProviderPort + Send + Sync>,
    ) -> Self {
        Self {
            extensions: extensions
                .into_iter()
                .map(Into::into)
                .map(|extension: String| extension.to_ascii_lowercase())
                .collect(),
            provider,
        }
    }
}

/// Routes file-scoped semantic requests by unique extension and fails closed
/// to the injected fallback for ambiguous or non-file requests.
pub struct PolyglotSemanticProvider {
    routes: Vec<LanguageSemanticRoute>,
    fallback: Arc<dyn SemanticProviderPort + Send + Sync>,
}

impl PolyglotSemanticProvider {
    pub fn new(
        routes: Vec<LanguageSemanticRoute>,
        fallback: Arc<dyn SemanticProviderPort + Send + Sync>,
    ) -> Self {
        Self { routes, fallback }
    }

    fn provider_for(
        &self,
        request: &SemanticRequest,
    ) -> &Arc<dyn SemanticProviderPort + Send + Sync> {
        let Some(extension) = semantic_request_extension(request) else {
            return &self.fallback;
        };
        let mut matching = self
            .routes
            .iter()
            .filter(|route| route.extensions.contains(&extension));
        let Some(route) = matching.next() else {
            return &self.fallback;
        };
        if matching.next().is_some() {
            return &self.fallback;
        }
        &route.provider
    }
}

impl SemanticProviderPort for PolyglotSemanticProvider {
    fn request(
        &self,
        root: &AdmittedRoot,
        request_id: &LspRequestId,
        request: &SemanticRequest,
    ) -> SemanticProviderOutcome<SemanticResponse> {
        self.provider_for(request)
            .request(root, request_id, request)
    }
}

fn semantic_request_extension(request: &SemanticRequest) -> Option<String> {
    let uri = Url::parse(request.document_uri()?).ok()?;
    if uri.scheme() != "file" {
        return None;
    }
    uri.to_file_path()
        .ok()?
        .extension()?
        .to_str()
        .map(str::to_ascii_lowercase)
}

/// Fans cancellation out to every injected semantic authority.
pub struct CompositeAnalyzerCancellation {
    authorities: Vec<Arc<dyn AnalyzerCancellationPort + Send + Sync>>,
}

impl CompositeAnalyzerCancellation {
    pub fn new(authorities: Vec<Arc<dyn AnalyzerCancellationPort + Send + Sync>>) -> Self {
        Self { authorities }
    }
}

impl AnalyzerCancellationPort for CompositeAnalyzerCancellation {
    fn cancel_upstream(&self, root: &AdmittedRoot, request_id: &LspRequestId) -> bool {
        self.authorities.iter().fold(false, |cancelled, authority| {
            authority.cancel_upstream(root, request_id) | cancelled
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct PendingProvider;

    impl SemanticProviderPort for PendingProvider {
        fn request(
            &self,
            _root: &AdmittedRoot,
            _request_id: &LspRequestId,
            _request: &SemanticRequest,
        ) -> SemanticProviderOutcome<SemanticResponse> {
            SemanticProviderOutcome::Pending
        }
    }

    struct UnavailableProvider;

    impl SemanticProviderPort for UnavailableProvider {
        fn request(
            &self,
            _root: &AdmittedRoot,
            _request_id: &LspRequestId,
            _request: &SemanticRequest,
        ) -> SemanticProviderOutcome<SemanticResponse> {
            SemanticProviderOutcome::Unavailable
        }
    }

    struct Cancellation {
        cancelled: bool,
    }

    impl AnalyzerCancellationPort for Cancellation {
        fn cancel_upstream(&self, _root: &AdmittedRoot, _request_id: &LspRequestId) -> bool {
            self.cancelled
        }
    }

    #[test]
    fn polyglot_provider_routes_unique_file_extensions_and_falls_back_on_ambiguity() {
        let routed: Arc<dyn SemanticProviderPort + Send + Sync> = Arc::new(PendingProvider);
        let fallback: Arc<dyn SemanticProviderPort + Send + Sync> = Arc::new(UnavailableProvider);
        let root = AdmittedRoot::new("file:///project");
        let request_id = LspRequestId::Number(1);
        let request = SemanticRequest::DocumentSymbols {
            document_uri: "file:///project/src/app.TS".to_string(),
        };

        let unique = PolyglotSemanticProvider::new(
            vec![LanguageSemanticRoute::new(["ts"], Arc::clone(&routed))],
            Arc::clone(&fallback),
        );
        assert!(matches!(
            unique.request(&root, &request_id, &request),
            SemanticProviderOutcome::Pending
        ));

        let ambiguous = PolyglotSemanticProvider::new(
            vec![
                LanguageSemanticRoute::new(["ts"], Arc::clone(&routed)),
                LanguageSemanticRoute::new(["TS"], routed),
            ],
            fallback,
        );
        assert!(matches!(
            ambiguous.request(&root, &request_id, &request),
            SemanticProviderOutcome::Unavailable
        ));
    }

    #[test]
    fn composite_cancellation_fans_out_and_preserves_any_success() {
        let cancellation = CompositeAnalyzerCancellation::new(vec![
            Arc::new(Cancellation { cancelled: false }),
            Arc::new(Cancellation { cancelled: true }),
        ]);

        assert!(cancellation.cancel_upstream(
            &AdmittedRoot::new("file:///project"),
            &LspRequestId::Number(2)
        ));
    }
}
