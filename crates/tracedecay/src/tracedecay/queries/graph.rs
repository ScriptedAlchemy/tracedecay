use std::sync::Arc;

use tracedecay_graph_query::{
    CodeGraphProjectionReadPort, CodeGraphReadAdmissionPort, CodeGraphSourceAuthorityPort,
    CodeGraphSourceBindFuture, CodeGraphSourceBindRequest, SourceReadContext,
    VerifiedGraphQueryFuture, VerifiedGraphQueryPort, VerifiedGraphQueryRequest,
    open_verified_graph_query,
};

struct BoundCodeGraphSourceAuthority {
    source: SourceReadContext,
}

impl CodeGraphSourceAuthorityPort for BoundCodeGraphSourceAuthority {
    fn bind<'a>(
        &'a self,
        _request: CodeGraphSourceBindRequest<'a>,
    ) -> CodeGraphSourceBindFuture<'a> {
        let source = self.source.clone();
        Box::pin(async move { Ok(source) })
    }
}

/// Root adapter: closes over admission, projection, and the admitted project
/// source authority. `open` never names [`crate::tracedecay::TraceDecay`].
pub(crate) struct AdmittedVerifiedGraphQueryPort {
    admission: Arc<dyn CodeGraphReadAdmissionPort>,
    projection: Arc<dyn CodeGraphProjectionReadPort>,
    source_authority: Option<Arc<dyn CodeGraphSourceAuthorityPort>>,
}

impl AdmittedVerifiedGraphQueryPort {
    pub(crate) fn new(
        admission: Arc<dyn CodeGraphReadAdmissionPort>,
        projection: Arc<dyn CodeGraphProjectionReadPort>,
        source: Option<SourceReadContext>,
    ) -> Self {
        Self {
            admission,
            projection,
            source_authority: source.map(|source| {
                Arc::new(BoundCodeGraphSourceAuthority { source })
                    as Arc<dyn CodeGraphSourceAuthorityPort>
            }),
        }
    }
}

impl VerifiedGraphQueryPort for AdmittedVerifiedGraphQueryPort {
    fn open<'a>(&'a self, request: VerifiedGraphQueryRequest<'a>) -> VerifiedGraphQueryFuture<'a> {
        Box::pin(open_verified_graph_query(
            &*self.admission,
            &*self.projection,
            request,
            self.source_authority.as_deref(),
        ))
    }
}

#[cfg(test)]
pub(crate) fn admitted_verified_graph_query_port(
    admission: Arc<dyn CodeGraphReadAdmissionPort>,
    projection: Arc<dyn CodeGraphProjectionReadPort>,
) -> Arc<dyn VerifiedGraphQueryPort> {
    admitted_verified_graph_query_port_with_source(admission, projection, None)
}

pub(crate) fn admitted_verified_graph_query_port_with_source(
    admission: Arc<dyn CodeGraphReadAdmissionPort>,
    projection: Arc<dyn CodeGraphProjectionReadPort>,
    source: Option<SourceReadContext>,
) -> Arc<dyn VerifiedGraphQueryPort> {
    Arc::new(AdmittedVerifiedGraphQueryPort::new(
        admission, projection, source,
    ))
}
