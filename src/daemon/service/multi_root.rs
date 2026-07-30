//! Daemon façade over canonical multi-root CAS and query authorities.

use rusqlite::Connection;
use tracedecay_application::{
    AuthorizedMultiRootQueryService, AuthorizedScopeSet, MultiRootQueryError, MultiRootQueryPageV1,
    MultiRootQueryPort, MultiRootQueryRequestV1,
};
use tracedecay_domain::ScopeSetRevision;
use tracedecay_rusqlite_runtime::repository::{
    AuthorizedScopeSetExecutor, AuthorizedScopeSetStoreError,
};
use tracedecay_store::runtime::ScopeSetCasOutcomeV1;

use super::invocation::DaemonInvocationService;

impl DaemonInvocationService {
    /// Persist a canonical scope set under the daemon's already-open store
    /// authority. The caller retains connection and migration ownership.
    pub(crate) fn compare_and_swap_scope_set(
        &self,
        connection: &mut Connection,
        expected_revision: Option<ScopeSetRevision>,
        next: &AuthorizedScopeSet,
    ) -> Result<ScopeSetCasOutcomeV1, AuthorizedScopeSetStoreError> {
        AuthorizedScopeSetExecutor::compare_and_swap(connection, expected_revision, next)
    }

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
