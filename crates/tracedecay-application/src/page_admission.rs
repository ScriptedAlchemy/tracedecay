use std::future::Future;
use std::pin::Pin;

use thiserror::Error;
use tracedecay_domain::{ManifestDigest, UtcMicros};
use tracedecay_tool_catalog::BindingId;

use crate::{ApplicationWireOperation, PageRequest, RequestContext};

/// Everything an owning runtime must bind before an adapter may forward a page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageAdmissionRequest {
    binding_id: BindingId,
    operation: ApplicationWireOperation,
    context: RequestContext,
    observed_at: UtcMicros,
    operation_scope_digest: ManifestDigest,
    body_digest: ManifestDigest,
    page: PageRequest,
}

impl PageAdmissionRequest {
    pub fn new(
        binding_id: BindingId,
        operation: ApplicationWireOperation,
        context: RequestContext,
        observed_at: UtcMicros,
        operation_scope_digest: ManifestDigest,
        body_digest: ManifestDigest,
        page: PageRequest,
    ) -> Result<Self, PageAdmissionError> {
        context.validate().map_err(|_| PageAdmissionError::Denied)?;
        if context.admission_at(observed_at) != crate::RequestAdmission::Admitted {
            return Err(PageAdmissionError::Denied);
        }
        operation_scope_digest
            .validate()
            .map_err(|_| PageAdmissionError::InvalidRequest)?;
        body_digest
            .validate()
            .map_err(|_| PageAdmissionError::InvalidRequest)?;
        if operation_scope_digest != context.scope().scope_digest {
            return Err(PageAdmissionError::Denied);
        }
        Ok(Self {
            binding_id,
            operation,
            context,
            observed_at,
            operation_scope_digest,
            body_digest,
            page,
        })
    }

    pub fn binding_id(&self) -> &BindingId {
        &self.binding_id
    }

    pub const fn operation(&self) -> ApplicationWireOperation {
        self.operation
    }

    pub fn context(&self) -> &RequestContext {
        &self.context
    }

    pub const fn observed_at(&self) -> UtcMicros {
        self.observed_at
    }

    pub fn operation_scope_digest(&self) -> &ManifestDigest {
        &self.operation_scope_digest
    }

    pub fn body_digest(&self) -> &ManifestDigest {
        &self.body_digest
    }

    pub fn page(&self) -> &PageRequest {
        &self.page
    }
}

/// A page that can only be minted through [`PageAdmissionService`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdmittedPageRequest {
    request: PageAdmissionRequest,
}

impl AdmittedPageRequest {
    pub fn binding_id(&self) -> &BindingId {
        self.request.binding_id()
    }

    pub const fn operation(&self) -> ApplicationWireOperation {
        self.request.operation()
    }

    pub fn context(&self) -> &RequestContext {
        self.request.context()
    }

    pub const fn observed_at(&self) -> UtcMicros {
        self.request.observed_at()
    }

    pub fn operation_scope_digest(&self) -> &ManifestDigest {
        self.request.operation_scope_digest()
    }

    pub fn body_digest(&self) -> &ManifestDigest {
        self.request.body_digest()
    }

    pub fn page(&self) -> &PageRequest {
        self.request.page()
    }

    pub fn into_page(self) -> PageRequest {
        self.request.page
    }
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum PageAdmissionError {
    #[error("page request is not authorized for the authenticated request context")]
    Denied,
    #[error("page continuation no longer identifies the current owner snapshot")]
    Stale,
    #[error("page continuation authority is unavailable")]
    Unavailable,
    #[error("page request is invalid")]
    InvalidRequest,
    #[error("binding does not identify the requested application operation")]
    BindingMismatch,
    #[error("application operation has no page admission owner")]
    Unsupported,
}

pub type PageAdmissionFuture<'a> =
    Pin<Box<dyn Future<Output = Result<AdmittedPageRequest, PageAdmissionError>> + Send + 'a>>;

/// Private construction authority handed to one owner invocation.
///
/// Runtimes receive this only from [`PageAdmissionService`], after which they
/// may mint the admitted wrapper only on the successful authentic-owner path.
pub struct PageAdmissionSeal {
    _private: (),
}

impl PageAdmissionSeal {
    pub fn admit(self, request: PageAdmissionRequest) -> AdmittedPageRequest {
        AdmittedPageRequest { request }
    }
}

/// Application boundary implemented by operation-specific cursor owners.
pub trait PageAdmissionPort: Send + Sync {
    fn admit<'a>(
        &'a self,
        request: PageAdmissionRequest,
        seal: PageAdmissionSeal,
    ) -> PageAdmissionFuture<'a>;
}

/// Sole public constructor for an admitted page.
pub struct PageAdmissionService<P> {
    owner: P,
}

impl<P> PageAdmissionService<P>
where
    P: PageAdmissionPort,
{
    pub const fn new(owner: P) -> Self {
        Self { owner }
    }

    pub async fn admit(
        &self,
        request: PageAdmissionRequest,
    ) -> Result<AdmittedPageRequest, PageAdmissionError> {
        self.owner
            .admit(request, PageAdmissionSeal { _private: () })
            .await
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use tracedecay_domain::{
        ActorId, ManifestDigest, ProjectId, RepositoryId, UtcMicros, WorktreeId,
    };
    use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

    use super::*;
    use crate::{
        CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass,
        RequestId, ResolvedScope,
    };

    fn context() -> RequestContext {
        let observed_at = UtcMicros(1_000);
        let deadline = Deadline::new(UtcMicros(2_000)).unwrap();
        let scope = ResolvedScope::new(
            ProjectId::new("project.page-admission").unwrap(),
            RepositoryId::new("repository.page-admission").unwrap(),
            WorktreeId::new("worktree.page-admission").unwrap(),
            None,
        )
        .unwrap();
        let capability = CapabilityId::new("capability.page-admission").unwrap();
        let use_case = UseCaseId::new("use-case.page-admission").unwrap();
        let grant = CapabilityGrantSnapshot::new(
            CapabilityGrantId::new("grant.page-admission").unwrap(),
            1,
            ManifestDigest::new(format!("sha256:{}", "1".repeat(64))).unwrap(),
            ActorId::new("actor.page-admission").unwrap(),
            observed_at,
            UtcMicros(3_000),
            scope.clone(),
            BTreeSet::from([capability]),
            BTreeSet::from([use_case]),
            DisclosureClass::Evidence,
        )
        .unwrap();
        RequestContext::new(
            ActorId::new("actor.page-admission").unwrap(),
            scope,
            grant,
            RequestId::new("request.page-admission").unwrap(),
            deadline,
            CancellationContext::active("cancel.page-admission").unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn request_binds_context_scope_body_binding_and_operation() {
        let context = context();
        let binding_id = BindingId::new("binding.http.code-symbol-search.v1").unwrap();
        let body_digest = ManifestDigest::new(format!("sha256:{}", "2".repeat(64))).unwrap();
        let request = PageAdmissionRequest::new(
            binding_id.clone(),
            ApplicationWireOperation::CodeSymbolSearch,
            context.clone(),
            UtcMicros(1_000),
            context.scope().scope_digest.clone(),
            body_digest.clone(),
            PageRequest::first(25).unwrap(),
        )
        .unwrap();

        assert_eq!(request.binding_id(), &binding_id);
        assert_eq!(request.body_digest(), &body_digest);
        assert_eq!(request.page().page_size, 25);
        assert_eq!(
            request.operation_scope_digest(),
            &context.scope().scope_digest
        );
    }

    #[test]
    fn admission_rejects_a_scope_digest_from_another_context() {
        let context = context();
        let request = PageAdmissionRequest::new(
            BindingId::new("binding.http.code-symbol-search.v1").unwrap(),
            ApplicationWireOperation::CodeSymbolSearch,
            context,
            UtcMicros(1_000),
            ManifestDigest::new(format!("sha256:{}", "3".repeat(64))).unwrap(),
            ManifestDigest::new(format!("sha256:{}", "2".repeat(64))).unwrap(),
            PageRequest::first(25).unwrap(),
        );

        assert_eq!(request, Err(PageAdmissionError::Denied));
    }

    #[test]
    fn admission_rejects_an_elapsed_context() {
        let context = context();
        let request = PageAdmissionRequest::new(
            BindingId::new("binding.http.code-symbol-search.v1").unwrap(),
            ApplicationWireOperation::CodeSymbolSearch,
            context.clone(),
            context.deadline().expires_at,
            context.scope().scope_digest.clone(),
            ManifestDigest::new(format!("sha256:{}", "2".repeat(64))).unwrap(),
            PageRequest::first(25).unwrap(),
        );

        assert_eq!(request, Err(PageAdmissionError::Denied));
    }
}
