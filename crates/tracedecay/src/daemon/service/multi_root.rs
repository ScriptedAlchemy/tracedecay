//! Daemon façade over canonical multi-root CAS and query authorities.

use tracedecay_application::{
    AuthorizedMultiRootQueryService, MultiRootQueryError, MultiRootQueryPageV1, MultiRootQueryPort,
    MultiRootQueryRequestV1,
};

use super::invocation::DaemonInvocationService;

impl DaemonInvocationService {
    /// Execute one federated query after transport admission has supplied the
    /// exact contexts and frozen root generations.
    pub(crate) fn execute_multi_root_query<P, Q, T>(
        &self,
        port: P,
        request: MultiRootQueryRequestV1<Q>,
    ) -> Result<MultiRootQueryPageV1<T>, MultiRootQueryError>
    where
        P: MultiRootQueryPort<Q, T>,
        T: Clone,
    {
        AuthorizedMultiRootQueryService::new(port).execute(request)
    }
}
