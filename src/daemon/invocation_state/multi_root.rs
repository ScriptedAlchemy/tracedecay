//! Exact registered-root resolution for daemon multi-root execution.

use std::path::PathBuf;

use super::*;

pub(super) enum AuthorizedRootResolution {
    Denied,
    Unavailable(tracedecay_domain::ScopeUnavailableReasonV1),
}

pub(super) async fn resolve_authorized_root(
    store_administration: &StoreAdministration,
    database: &crate::global_db::RegisteredGlobalDb,
    root: &tracedecay_application::AuthorizedRoot,
) -> std::result::Result<PathBuf, AuthorizedRootResolution> {
    let scope = root.scope();
    let locator = root.locator().ok_or(AuthorizedRootResolution::Unavailable(
        tracedecay_domain::ScopeUnavailableReasonV1::AuthorityUnavailable,
    ))?;
    let profile_identity = store_administration.profile_identity().map_err(|_| {
        AuthorizedRootResolution::Unavailable(
            tracedecay_domain::ScopeUnavailableReasonV1::AuthorityUnavailable,
        )
    })?;
    if profile_identity.profile_id() != &locator.profile.profile_id {
        return Err(AuthorizedRootResolution::Unavailable(
            tracedecay_domain::ScopeUnavailableReasonV1::AuthorityUnavailable,
        ));
    }
    let registry_context = database
        .project_registry_context_by_id(locator.project_id.as_str())
        .await
        .map_err(|_| {
            AuthorizedRootResolution::Unavailable(
                tracedecay_domain::ScopeUnavailableReasonV1::StoreUnavailable,
            )
        })?
        .ok_or(AuthorizedRootResolution::Denied)?;
    if registry_context.project.project_id != locator.project_id.as_str()
        || !registry_context.stores.iter().any(|store| {
            store.store.project_id == locator.project_id.as_str()
                && store.store.store_id == locator.profile.store_id
        })
    {
        return Err(AuthorizedRootResolution::Denied);
    }
    let registered_root = PathBuf::from(registry_context.project.canonical_root);
    if !registered_root.is_absolute()
        || registered_root.canonicalize().ok().as_ref() != Some(&registered_root)
    {
        return Err(AuthorizedRootResolution::Unavailable(
            tracedecay_domain::ScopeUnavailableReasonV1::RootMissing,
        ));
    }
    let exact_root = locator.canonical_root.clone();
    if exact_root.canonicalize().ok().as_ref() != Some(&exact_root) {
        return Err(AuthorizedRootResolution::Unavailable(
            tracedecay_domain::ScopeUnavailableReasonV1::RootMissing,
        ));
    }
    tracedecay_usecases::context::RegisteredScopeResolver::resolve(
        &registered_root,
        &exact_root,
        &locator.project_id,
    )
    .map_err(|_| AuthorizedRootResolution::Denied)?;
    let exact_scope =
        project_open_owners::resolved_scope_for_project(&exact_root, &locator.project_id).map_err(
            |_| {
                AuthorizedRootResolution::Unavailable(
                    tracedecay_domain::ScopeUnavailableReasonV1::RootMissing,
                )
            },
        )?;
    if &exact_scope != scope {
        return Err(AuthorizedRootResolution::Denied);
    }
    Ok(exact_root)
}
