//! Daemon façade over canonical multi-root CAS and query authorities.

use tracedecay_application::{
    AuthorizedMultiRootQueryService, AuthorizedScopeSet, MultiRootQueryError, MultiRootQueryPageV1,
    MultiRootQueryPort, MultiRootQueryRequestV1,
};
use tracedecay_rusqlite_runtime::repository::{
    AuthorizedScopeSetSqliteStorage, AuthorizedScopeSetStoreError,
};
use tracedecay_store::runtime::ScopeSetCasOutcomeV1;

use super::invocation::DaemonInvocationService;

impl DaemonInvocationService {
    /// Admit an exact canonical scope set through the registered store. A
    /// concurrent idempotent publication is accepted only after re-reading the
    /// stored set; conflicts never authorize the proposed request object.
    pub(crate) fn persist_exact_scope_set(
        &self,
        storage: &AuthorizedScopeSetSqliteStorage,
        next: &AuthorizedScopeSet,
    ) -> Result<Option<AuthorizedScopeSet>, AuthorizedScopeSetStoreError> {
        if let Some(current) = storage.read(next.scope_set_id())? {
            return Ok((current == *next).then_some(current));
        }
        match storage.compare_and_swap(None, next)? {
            ScopeSetCasOutcomeV1::Applied(_) | ScopeSetCasOutcomeV1::Conflict { .. } => {}
        }
        Ok(storage
            .read(next.scope_set_id())?
            .filter(|stored| stored == next))
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
