//! Server-local page admission for daemon-owned callable code queries.
//!
//! The request context reaches this module only after daemon authentication.
//! Continuation bytes stay opaque until the mounted prepared-query authority
//! authenticates their exact operation, scope, principal, body, page size,
//! authorization revision, and current-only temporal mode.

use std::sync::OnceLock;

use tracedecay_application::{
    ApplicationWireOperation, PageAdmissionError, PageAdmissionFuture, PageAdmissionPort,
    PageAdmissionRequest, PageAdmissionSeal,
};
use tracedecay_domain::TemporalModeV1;
use tracedecay_query::retrieval::{
    PreparedQueryErrorV1, route_authenticated_prepared_query_cursor,
};
use tracedecay_tool_catalog::{CapabilityManifestV1, CatalogSnapshotV1};

use super::CodeIndexSchedulerRegistryV1;
use super::queries::prepared_routing_bindings_for_context;

fn application_catalog() -> Result<&'static CatalogSnapshotV1, PageAdmissionError> {
    static CATALOG: OnceLock<Result<CatalogSnapshotV1, String>> = OnceLock::new();
    CATALOG
        .get_or_init(|| {
            crate::catalog_composition::build_application_catalog_snapshot()
                .map_err(|error| error.to_string())
        })
        .as_ref()
        .map_err(|_| PageAdmissionError::Unavailable)
}

pub(in crate::daemon) fn validate_catalog_page_request(
    request: &PageAdmissionRequest,
) -> Result<(), PageAdmissionError> {
    catalog_page_capability(application_catalog()?, request).map(|_| ())
}

fn catalog_page_capability<'a>(
    catalog: &'a CatalogSnapshotV1,
    request: &PageAdmissionRequest,
) -> Result<&'a CapabilityManifestV1, PageAdmissionError> {
    if request.operation_scope_digest() != &request.context().scope().scope_digest {
        return Err(PageAdmissionError::Denied);
    }
    let binding = catalog
        .binding(request.binding_id())
        .ok_or(PageAdmissionError::BindingMismatch)?;
    if ApplicationWireOperation::from_catalog_name(binding.operation().as_str())
        != Some(request.operation())
    {
        return Err(PageAdmissionError::BindingMismatch);
    }
    let capability = catalog
        .capability(binding.capability_id())
        .ok_or(PageAdmissionError::Unavailable)?;
    let pagination = capability
        .pagination()
        .ok_or(PageAdmissionError::Unsupported)?;
    if !capability.availability().is_callable() {
        return Err(PageAdmissionError::Unsupported);
    }
    if request.page().page_size > pagination.maximum_page_size() {
        return Err(PageAdmissionError::InvalidRequest);
    }
    if !request
        .context()
        .allows(capability.capability_id(), capability.use_case_id())
    {
        return Err(PageAdmissionError::Denied);
    }
    Ok(capability)
}

fn map_prepared_query_error(error: PreparedQueryErrorV1) -> PageAdmissionError {
    match error {
        PreparedQueryErrorV1::Invalid => PageAdmissionError::Denied,
        PreparedQueryErrorV1::Stale => PageAdmissionError::Stale,
        PreparedQueryErrorV1::Unavailable => PageAdmissionError::Unavailable,
    }
}

impl CodeIndexSchedulerRegistryV1 {
    async fn authenticate_callable_page(
        &self,
        request: &PageAdmissionRequest,
    ) -> Result<(), PageAdmissionError> {
        if !request.operation().is_callable_code() {
            return Err(PageAdmissionError::Unsupported);
        }
        validate_catalog_page_request(request)?;
        let authority = self
            .query_authority_for_scope(request.context().scope())
            .await
            .ok_or(PageAdmissionError::Unavailable)?;
        let Some(cursor) = request.page().cursor.as_ref() else {
            return Ok(());
        };
        let routing = prepared_routing_bindings_for_context(
            request.context(),
            TemporalModeV1::Current,
            request.operation().as_str(),
            request.body_digest().clone(),
            request.page().page_size,
        )
        .map_err(map_prepared_query_error)?;
        let generation = route_authenticated_prepared_query_cursor(
            authority.as_ref(),
            &routing,
            cursor.as_str(),
            request.observed_at(),
            None,
            |generation| generation,
        )
        .map_err(map_prepared_query_error)?;
        self.generation_for(request.context().scope(), &generation)
            .await
            .ok_or(PageAdmissionError::Stale)?;
        Ok(())
    }
}

impl PageAdmissionPort for CodeIndexSchedulerRegistryV1 {
    fn admit<'a>(
        &'a self,
        request: PageAdmissionRequest,
        seal: PageAdmissionSeal,
    ) -> PageAdmissionFuture<'a> {
        Box::pin(async move {
            self.authenticate_callable_page(&request).await?;
            Ok(seal.admit(request))
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use tracedecay_application::{
        CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass,
        PageRequest, RequestContext, RequestId, ResolvedScope,
    };
    use tracedecay_domain::{
        ActorId, ProjectId, RepositoryId, UtcMicros, WorktreeId, canonical_sha256,
    };
    use tracedecay_tool_catalog::{BindingId, CapabilityId};

    use super::*;

    fn page_request(
        catalog: &CatalogSnapshotV1,
        binding_id: BindingId,
        operation: ApplicationWireOperation,
        allowed_capability: Option<CapabilityId>,
    ) -> PageAdmissionRequest {
        let binding = catalog.binding(&binding_id).expect("catalog binding");
        let capability = catalog
            .capability(binding.capability_id())
            .expect("catalog capability");
        let scope = ResolvedScope::new(
            ProjectId::new("project.callable-page-admission").expect("project"),
            RepositoryId::new("repository.callable-page-admission").expect("repository"),
            WorktreeId::new("worktree.callable-page-admission").expect("worktree"),
            None,
        )
        .expect("scope");
        let observed_at = UtcMicros(100);
        let grant = CapabilityGrantSnapshot::new(
            CapabilityGrantId::new("grant.callable-page-admission").expect("grant"),
            1,
            canonical_sha256(&"grant.callable-page-admission").expect("grant digest"),
            ActorId::new("actor.callable-page-admission.issuer").expect("issuer"),
            UtcMicros(1),
            UtcMicros(1_000),
            scope.clone(),
            BTreeSet::from([
                allowed_capability.unwrap_or_else(|| capability.capability_id().clone())
            ]),
            BTreeSet::from([capability.use_case_id().clone()]),
            DisclosureClass::Evidence,
        )
        .expect("grant");
        let context = RequestContext::new(
            ActorId::new("actor.callable-page-admission").expect("actor"),
            scope,
            grant,
            RequestId::new("request.callable-page-admission").expect("request"),
            Deadline::new(UtcMicros(1_000)).expect("deadline"),
            CancellationContext::active("cancel.callable-page-admission").expect("cancellation"),
        )
        .expect("context");
        PageAdmissionRequest::new(
            binding_id,
            operation,
            context.clone(),
            observed_at,
            context.scope().scope_digest.clone(),
            canonical_sha256(&"body.callable-page-admission").expect("body digest"),
            PageRequest::first(1).expect("page"),
        )
        .expect("page admission request")
    }

    #[test]
    fn callable_paginated_bindings_are_derived_from_the_catalog() {
        let catalog = application_catalog().expect("canonical application catalog");
        let mut callable_bindings = BTreeMap::<&'static str, BTreeSet<BindingId>>::new();
        for capability in catalog.capabilities().filter(|capability| {
            capability.pagination().is_some() && capability.availability().is_callable()
        }) {
            for binding_id in capability.binding_ids() {
                let Some(binding) = catalog.binding(binding_id) else {
                    panic!("catalog capability binding must resolve");
                };
                let Some(operation) =
                    ApplicationWireOperation::from_catalog_name(binding.operation().as_str())
                else {
                    continue;
                };
                if operation.is_callable_code() {
                    callable_bindings
                        .entry(operation.as_str())
                        .or_default()
                        .insert(binding_id.clone());
                }
            }
        }

        let catalog_operations = callable_bindings.keys().copied().collect::<BTreeSet<_>>();
        let wire_operations = ApplicationWireOperation::ALL
            .into_iter()
            .filter(|operation| operation.is_callable_code())
            .map(ApplicationWireOperation::as_str)
            .collect::<BTreeSet<_>>();
        assert_eq!(catalog_operations, wire_operations);
        assert!(
            callable_bindings
                .values()
                .all(|bindings| !bindings.is_empty())
        );
        for (operation, binding_ids) in callable_bindings {
            let operation =
                ApplicationWireOperation::from_catalog_name(operation).expect("wire operation");
            for binding_id in binding_ids {
                let request = page_request(catalog, binding_id, operation, None);
                catalog_page_capability(catalog, &request).expect("callable page capability");
            }
        }
    }

    #[test]
    fn catalog_admission_fails_typed_for_mismatch_denial_and_unsupported_operation() {
        let catalog = application_catalog().expect("canonical application catalog");
        let callable = catalog
            .capabilities()
            .filter(|capability| capability.pagination().is_some())
            .flat_map(|capability| capability.binding_ids())
            .find_map(|binding_id| {
                let binding = catalog.binding(binding_id)?;
                let operation =
                    ApplicationWireOperation::from_catalog_name(binding.operation().as_str())?;
                operation
                    .is_callable_code()
                    .then(|| (binding_id.clone(), operation))
            })
            .expect("callable paginated binding");
        let mismatched_operation = ApplicationWireOperation::ALL
            .into_iter()
            .find(|operation| operation.is_callable_code() && *operation != callable.1)
            .expect("different callable operation");
        let mismatch = page_request(catalog, callable.0.clone(), mismatched_operation, None);
        assert_eq!(
            catalog_page_capability(catalog, &mismatch),
            Err(PageAdmissionError::BindingMismatch)
        );

        let denied = page_request(
            catalog,
            callable.0,
            callable.1,
            Some(CapabilityId::new("capability.denied-page").expect("denied capability")),
        );
        assert_eq!(
            catalog_page_capability(catalog, &denied),
            Err(PageAdmissionError::Denied)
        );

        let test_results = catalog
            .capabilities()
            .flat_map(|capability| capability.binding_ids())
            .find_map(|binding_id| {
                let binding = catalog.binding(binding_id)?;
                (ApplicationWireOperation::from_catalog_name(binding.operation().as_str())
                    == Some(ApplicationWireOperation::TestResults))
                .then(|| {
                    page_request(
                        catalog,
                        binding_id.clone(),
                        ApplicationWireOperation::TestResults,
                        None,
                    )
                })
            })
            .expect("managed test-result binding");
        catalog_page_capability(catalog, &test_results)
            .expect("managed test results are catalog-paginated");

        let unsupported = catalog
            .capabilities()
            .filter(|capability| capability.pagination().is_none())
            .flat_map(|capability| capability.binding_ids())
            .find_map(|binding_id| {
                let binding = catalog.binding(binding_id)?;
                let operation =
                    ApplicationWireOperation::from_catalog_name(binding.operation().as_str())?;
                Some(page_request(catalog, binding_id.clone(), operation, None))
            })
            .expect("non-paginated application binding");
        assert_eq!(
            catalog_page_capability(catalog, &unsupported),
            Err(PageAdmissionError::Unsupported)
        );
    }

    #[test]
    fn prepared_cursor_failures_preserve_typed_admission_states() {
        assert_eq!(
            map_prepared_query_error(PreparedQueryErrorV1::Invalid),
            PageAdmissionError::Denied
        );
        assert_eq!(
            map_prepared_query_error(PreparedQueryErrorV1::Stale),
            PageAdmissionError::Stale
        );
        assert_eq!(
            map_prepared_query_error(PreparedQueryErrorV1::Unavailable),
            PageAdmissionError::Unavailable
        );
    }
}
