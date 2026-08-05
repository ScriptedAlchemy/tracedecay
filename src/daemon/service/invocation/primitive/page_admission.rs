//! Pre-use-case page admission for primitive-owned cursor families.

use super::*;

#[derive(Clone)]
struct PrimitivePageAdmissionOwner {
    dispatch: Arc<dyn Pr12PrimitiveDispatch>,
    owner: crate::application::primitives::Pr12PrimitivePageOwner,
}

impl tracedecay_application::PageAdmissionPort for PrimitivePageAdmissionOwner {
    fn admit<'a>(
        &'a self,
        request: tracedecay_application::PageAdmissionRequest,
        seal: tracedecay_application::PageAdmissionSeal,
    ) -> tracedecay_application::PageAdmissionFuture<'a> {
        if matches!(
            &self.owner,
            crate::application::primitives::Pr12PrimitivePageOwner::TestResults
        ) {
            self.dispatch.admit_page(request, seal)
        } else {
            self.dispatch
                .admit_primitive_page(request, self.owner.clone(), seal)
        }
    }
}

struct PrimitivePageAdmissionInput {
    request: tracedecay_application::PageAdmissionRequest,
    owner: crate::application::primitives::Pr12PrimitivePageOwner,
}

pub(super) async fn admit_primitive_page(
    dispatch: Arc<dyn Pr12PrimitiveDispatch>,
    binding_id: BindingId,
    surface_operation: crate::application_surface::ApplicationSurfaceOperation,
    primitive: &Pr12PrimitiveRequest,
    context: &RequestContext,
    observed_at: UtcMicros,
) -> Result<(), tracedecay_application::PageAdmissionError> {
    let Some(input) = primitive_page_admission_input(
        binding_id,
        surface_operation,
        primitive,
        context,
        observed_at,
    )?
    else {
        return Ok(());
    };
    tracedecay_application::PageAdmissionService::new(PrimitivePageAdmissionOwner {
        dispatch,
        owner: input.owner,
    })
    .admit(input.request)
    .await
    .map(|_| ())
}

fn primitive_page_admission_input(
    binding_id: BindingId,
    surface_operation: crate::application_surface::ApplicationSurfaceOperation,
    primitive: &Pr12PrimitiveRequest,
    context: &RequestContext,
    observed_at: UtcMicros,
) -> Result<Option<PrimitivePageAdmissionInput>, tracedecay_application::PageAdmissionError> {
    use crate::application::primitives::Pr12PrimitivePageOwner;
    use tracedecay_application::{ApplicationWireOperation, PageAdmissionError};

    let (page, owner, body_digest) = match (surface_operation, primitive) {
        (
            crate::application_surface::ApplicationSurfaceOperation::CodeSymbolSearch,
            Pr12PrimitiveRequest::SymbolSearch(request),
        ) => (
            request.meta.page.clone(),
            Pr12PrimitivePageOwner::SymbolGraph,
            crate::application::primitives::symbol_search_page_body_digest(request)
                .map_err(|_| PageAdmissionError::InvalidRequest)?,
        ),
        (
            crate::application_surface::ApplicationSurfaceOperation::CodeSignatureSearch,
            Pr12PrimitiveRequest::SignatureSearch(request),
        ) => (
            request.meta.page.clone(),
            Pr12PrimitivePageOwner::SymbolGraph,
            crate::application::primitives::signature_search_page_body_digest(request)
                .map_err(|_| PageAdmissionError::InvalidRequest)?,
        ),
        (
            crate::application_surface::ApplicationSurfaceOperation::CodeImplementations,
            Pr12PrimitiveRequest::Implementations(request),
        ) => (
            request.meta.page.clone(),
            Pr12PrimitivePageOwner::SymbolGraph,
            crate::application::primitives::implementations_page_body_digest(request)
                .map_err(|_| PageAdmissionError::InvalidRequest)?,
        ),
        (
            crate::application_surface::ApplicationSurfaceOperation::CodeTypeHierarchy,
            Pr12PrimitiveRequest::TypeHierarchy(request),
        ) => (
            request.meta.page.clone(),
            Pr12PrimitivePageOwner::SymbolGraph,
            crate::application::primitives::type_hierarchy_page_body_digest(request)
                .map_err(|_| PageAdmissionError::InvalidRequest)?,
        ),
        (
            crate::application_surface::ApplicationSurfaceOperation::CodeCallers,
            Pr12PrimitiveRequest::Callers(request),
        ) => (
            request.meta.page.clone(),
            Pr12PrimitivePageOwner::SymbolGraph,
            crate::application::primitives::callers_page_body_digest(request)
                .map_err(|_| PageAdmissionError::InvalidRequest)?,
        ),
        (
            crate::application_surface::ApplicationSurfaceOperation::DiagnosticsRead,
            Pr12PrimitiveRequest::DiagnosticsRead(request),
        ) => {
            let cursor = request
                .cursor
                .as_ref()
                .map(|cursor| tracedecay_application::OpaqueCursor::new(cursor.clone()))
                .transpose()
                .map_err(|_| PageAdmissionError::InvalidRequest)?;
            (
                PageRequest::new(request.maximum_diagnostics, cursor)
                    .map_err(|_| PageAdmissionError::InvalidRequest)?,
                Pr12PrimitivePageOwner::Diagnostics(request.clone()),
                crate::application::primitives::diagnostics_page_body_digest(request)
                    .map_err(|_| PageAdmissionError::InvalidRequest)?,
            )
        }
        (
            crate::application_surface::ApplicationSurfaceOperation::TestResults,
            Pr12PrimitiveRequest::RecentTestResults(page),
        ) => (
            page.clone(),
            Pr12PrimitivePageOwner::TestResults,
            canonical_sha256(page).map_err(|_| PageAdmissionError::InvalidRequest)?,
        ),
        (
            crate::application_surface::ApplicationSurfaceOperation::CodeSymbolSearch
            | crate::application_surface::ApplicationSurfaceOperation::CodeSignatureSearch
            | crate::application_surface::ApplicationSurfaceOperation::CodeImplementations
            | crate::application_surface::ApplicationSurfaceOperation::CodeTypeHierarchy
            | crate::application_surface::ApplicationSurfaceOperation::CodeCallers
            | crate::application_surface::ApplicationSurfaceOperation::DiagnosticsRead
            | crate::application_surface::ApplicationSurfaceOperation::TestResults,
            _,
        ) => return Err(PageAdmissionError::BindingMismatch),
        _ => return Ok(None),
    };
    let operation = ApplicationWireOperation::from_catalog_name(surface_operation.as_str())
        .ok_or(PageAdmissionError::Unsupported)?;
    let request = tracedecay_application::PageAdmissionRequest::new(
        binding_id,
        operation,
        context.clone(),
        observed_at,
        context.scope().scope_digest.clone(),
        body_digest,
        page,
    )?;
    Ok(Some(PrimitivePageAdmissionInput { request, owner }))
}
