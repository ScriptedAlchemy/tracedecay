//! The registered store's own identity, used as the Work product owner
//! authority.
//!
//! `AuthorizedWorkProductScopeV1` is deliberately never accepted from a
//! request. This adapter resolves it from two facts the runtime already
//! proved and neither the caller nor this module can restate:
//!
//! * the owner is the brain and profile the registered store is *bound* to
//!   (`StoreRuntimeBindingV1::shard_id`), so a request cannot name a different
//!   profile's Work product by asking for it; and
//! * a selected relation scope is authorized only when it is the scope the
//!   request context already resolved, so a request scoped to one project
//!   cannot select another project's relations.
//!
//! Anything else is refused as not-authorized. There is no partial
//! authorization: a selection naming two projects where the context resolved
//! one is refused whole, because narrowing it silently would answer a
//! different question than the caller asked.

use tracedecay_application::{
    AuthorizedWorkProductScopeV1, RequestContext, WorkProductOwnerAuthorizationErrorV1,
    WorkProductOwnerAuthorizationPortV1, WorkProductSelectionScopeV1, WorkRelationScopeV1,
};
use tracedecay_domain::UtcMicros;

use crate::work::WorkSqliteStorage;

impl WorkProductOwnerAuthorizationPortV1 for WorkSqliteStorage {
    fn authorize_scope(
        &self,
        context: &RequestContext,
        selection: &WorkProductSelectionScopeV1,
        _observed_at: UtcMicros,
    ) -> Result<AuthorizedWorkProductScopeV1, WorkProductOwnerAuthorizationErrorV1> {
        let shard = &self.handle.binding().shard_id;
        if !selection_is_within_resolved_scope(context, selection) {
            return Err(WorkProductOwnerAuthorizationErrorV1::NotAuthorized);
        }
        AuthorizedWorkProductScopeV1::new(
            shard.brain_id.clone(),
            shard.profile_id.clone(),
            selection.clone(),
        )
        .map_err(|_| WorkProductOwnerAuthorizationErrorV1::Unavailable)
    }
}

fn selection_is_within_resolved_scope(
    context: &RequestContext,
    selection: &WorkProductSelectionScopeV1,
) -> bool {
    let resolved = context.scope();
    match selection {
        // An explicit no-Git selection asserts no repository relation at all,
        // so there is nothing for the resolved scope to authorize beyond the
        // grant the context already carries.
        WorkProductSelectionScopeV1::ProfileOwnedNoGit => true,
        WorkProductSelectionScopeV1::Relations { relation_scopes } => {
            !relation_scopes.is_empty()
                && relation_scopes.iter().all(|scope| match scope {
                    WorkRelationScopeV1::Project { project_id } => {
                        *project_id == resolved.project_id
                    }
                    WorkRelationScopeV1::Repository {
                        project_id,
                        repository_id,
                    } => {
                        *project_id == resolved.project_id
                            && *repository_id == resolved.repository_id
                    }
                })
        }
    }
}
