#![allow(
    clippy::result_large_err,
    reason = "the sealed problem envelope is the canonical pre-admission boundary contract"
)]

use std::future::Future;
use std::pin::Pin;

use tracedecay_domain::{CodeGenerationId, TemporalModeV1, UtcMicros};
use tracedecay_policy::authorization::SourceAuthorizationEvaluator;

use crate::authorization::{AuthorizationAdmission, AuthorizationPort, AuthorizationService};
use crate::context::RequestContext;
use crate::handlers::ApplicationOperation;
use crate::result::{
    ApplicationProblem, ApplicationResult, AuthorityReceipt, RetryDirective, SafeDiagnostic,
};

use super::callable_code::{
    CallableCodeOperationKind, CallableCodeOperations, CodeFacetRecord, CodeFacetRequest,
    CodeHierarchyRequest, CodeImpactRequest, CodeImplementationsRequest, CodeNavigationRequest,
    CodeQueryPage, CodeRelationRequest, CodeSignatureRequest, CodeSymbolSearchRequest,
    CodeTimelineRecord, CodeTimelineRequest, ExactOccurrenceRecord, ExactOccurrenceRequest,
    LexicalOccurrenceRecord, ModuleApiRequest, PhraseSearchRequest, QualifiedNameRequest,
    SourceMetadataRecord, SourceMetadataRequest, ValidatedCodeQueryRequest,
};
use super::service::{evidence_envelope_with_async_publication_recheck, problem_envelope};
use super::{
    RetrievalPortContext, RetrievalPortOutcome, SymbolPrimitiveRecord, SymbolRelationRecord,
    TypeHierarchyRecord,
};

/// The `scope.generation` value that asks for the latest complete generation
/// instead of pinning an exact one.
///
/// Every callable-code surface requires an explicit generation identity, so a
/// caller with no generation in hand has nothing valid to send. This sentinel
/// is that caller's entry point, and it is exported so the published tool
/// schemas can name it rather than leaving it as folklore.
pub const UNPINNED_LATEST_GENERATION_SENTINEL: &str = "code-generation:unpinned-latest.v1";

pub type CallableCodeQueryFuture<'a, T> =
    Pin<Box<dyn Future<Output = RetrievalPortOutcome<CodeQueryPage<T>>> + Send + 'a>>;
pub type CallableCodeAuthorizationFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Typed application port over the existing exact, lexical, and graph
/// kernels. Implementations select the requested immutable generation and
/// delegate one method to its owning kernel; this trait contains no planner,
/// parser, index, fallback synthesis, or transport dispatch.
pub trait CallableCodeQueryPort: Send + Sync {
    fn exact_occurrence<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a ExactOccurrenceRequest,
    ) -> CallableCodeQueryFuture<'a, ExactOccurrenceRecord>;

    fn phrase_search<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a PhraseSearchRequest,
    ) -> CallableCodeQueryFuture<'a, LexicalOccurrenceRecord>;

    fn symbol_search<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a CodeSymbolSearchRequest,
    ) -> CallableCodeQueryFuture<'a, SymbolPrimitiveRecord>;

    fn qualified_name<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a QualifiedNameRequest,
    ) -> CallableCodeQueryFuture<'a, SymbolPrimitiveRecord>;

    fn signature_search<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a CodeSignatureRequest,
    ) -> CallableCodeQueryFuture<'a, SymbolPrimitiveRecord>;

    fn implementations<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a CodeImplementationsRequest,
    ) -> CallableCodeQueryFuture<'a, SymbolRelationRecord>;

    fn type_hierarchy<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a CodeHierarchyRequest,
    ) -> CallableCodeQueryFuture<'a, TypeHierarchyRecord>;

    fn callers<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a CodeRelationRequest,
    ) -> CallableCodeQueryFuture<'a, SymbolRelationRecord>;

    fn callees<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a CodeRelationRequest,
    ) -> CallableCodeQueryFuture<'a, SymbolRelationRecord>;

    fn impact<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a CodeImpactRequest,
    ) -> CallableCodeQueryFuture<'a, SymbolPrimitiveRecord>;

    fn module_api<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a ModuleApiRequest,
    ) -> CallableCodeQueryFuture<'a, SymbolPrimitiveRecord>;

    fn source_metadata<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a SourceMetadataRequest,
    ) -> CallableCodeQueryFuture<'a, SourceMetadataRecord>;

    fn facets<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a CodeFacetRequest,
    ) -> CallableCodeQueryFuture<'a, CodeFacetRecord>;

    fn timeline<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a CodeTimelineRequest,
    ) -> CallableCodeQueryFuture<'a, CodeTimelineRecord>;

    fn declaration<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a CodeNavigationRequest,
    ) -> CallableCodeQueryFuture<'a, SymbolPrimitiveRecord>;

    fn definition<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a CodeNavigationRequest,
    ) -> CallableCodeQueryFuture<'a, SymbolPrimitiveRecord>;

    fn type_definition<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a CodeNavigationRequest,
    ) -> CallableCodeQueryFuture<'a, SymbolPrimitiveRecord>;

    fn references<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a CodeNavigationRequest,
    ) -> CallableCodeQueryFuture<'a, SymbolRelationRecord>;
}

/// Opaque authorization admission retained across one callable-code read.
///
/// Canonical source authorization keeps its full proof. A production route
/// that already resolved exact project/source access may instead retain its
/// route-owned receipt without reconstructing policy inputs.
#[derive(Clone, Debug)]
pub enum CallableCodeAuthorizationAdmission {
    Source(Box<AuthorizationAdmission>),
    Routed(AuthorityReceipt),
}

impl CallableCodeAuthorizationAdmission {
    pub fn receipt(&self) -> &AuthorityReceipt {
        match self {
            Self::Source(admission) => admission.receipt(),
            Self::Routed(receipt) => receipt,
        }
    }
}

/// Authorization boundary for callable-code application reads.
pub trait CallableCodeAuthorizationPort: Send + Sync {
    fn admit<'a>(
        &'a self,
        context: &'a RequestContext,
        operation: &'a ApplicationOperation,
        observed_at: UtcMicros,
    ) -> CallableCodeAuthorizationFuture<
        'a,
        Result<CallableCodeAuthorizationAdmission, ApplicationProblem>,
    >;

    fn recheck_publication<'a>(
        &'a self,
        context: &'a RequestContext,
        operation: &'a ApplicationOperation,
        admission: &'a CallableCodeAuthorizationAdmission,
        observed_at: UtcMicros,
    ) -> CallableCodeAuthorizationFuture<'a, Result<AuthorityReceipt, ApplicationProblem>>;
}

impl<P, E> CallableCodeAuthorizationPort for AuthorizationService<P, E>
where
    P: AuthorizationPort + Send + Sync,
    E: SourceAuthorizationEvaluator + Send + Sync,
{
    fn admit<'a>(
        &'a self,
        context: &'a RequestContext,
        operation: &'a ApplicationOperation,
        observed_at: UtcMicros,
    ) -> CallableCodeAuthorizationFuture<
        'a,
        Result<CallableCodeAuthorizationAdmission, ApplicationProblem>,
    > {
        Box::pin(async move {
            AuthorizationService::admit(self, context, operation, observed_at)
                .map(|admission| CallableCodeAuthorizationAdmission::Source(Box::new(admission)))
        })
    }

    fn recheck_publication<'a>(
        &'a self,
        context: &'a RequestContext,
        operation: &'a ApplicationOperation,
        admission: &'a CallableCodeAuthorizationAdmission,
        observed_at: UtcMicros,
    ) -> CallableCodeAuthorizationFuture<'a, Result<AuthorityReceipt, ApplicationProblem>> {
        Box::pin(async move {
            let CallableCodeAuthorizationAdmission::Source(admission) = admission else {
                return Err(ApplicationProblem::not_found_or_not_authorized(
                    RetryDirective::Never,
                ));
            };
            AuthorizationService::recheck_publication(
                self,
                context,
                operation,
                admission,
                observed_at,
            )
        })
    }
}

pub struct CallableCodeQueryService<P, A> {
    port: P,
    authorization: A,
    operations: CallableCodeOperations,
}

macro_rules! callable_code_service_method {
    ($name:ident, $kind:ident, $request:ty, $item:ty, $port_method:ident) => {
        pub async fn $name(
            &self,
            context: &RequestContext,
            request: $request,
            observed_at: UtcMicros,
        ) -> ApplicationResult<CodeQueryPage<$item>> {
            let operation = self.operations.get(CallableCodeOperationKind::$kind);
            if request.validate().is_err() {
                return problem_envelope(context, operation, invalid_code_query_problem());
            }
            let admission = match self
                .authorization
                .admit(context, operation, observed_at)
                .await
            {
                Ok(admission) => admission,
                Err(problem) => return problem_envelope(context, operation, problem),
            };
            let outcome = self
                .port
                .$port_method(
                    RetrievalPortContext {
                        request: context,
                        operation,
                    },
                    &request,
                )
                .await;
            if let Err(problem) = validate_code_query_outcome(
                &outcome,
                &request.scope.generation,
                request.meta.page.page_size,
            ) {
                return problem_envelope(context, operation, problem);
            }
            evidence_envelope_with_async_publication_recheck(
                context,
                operation,
                admission.receipt(),
                outcome,
                observed_at,
                |finished_at| {
                    self.authorization.recheck_publication(
                        context,
                        operation,
                        &admission,
                        finished_at,
                    )
                },
            )
            .await
        }
    };
}

impl<P, A> CallableCodeQueryService<P, A>
where
    P: CallableCodeQueryPort,
    A: CallableCodeAuthorizationPort,
{
    pub fn new(port: P, authorization: A, operations: CallableCodeOperations) -> Self {
        Self {
            port,
            authorization,
            operations,
        }
    }

    callable_code_service_method!(
        exact_occurrence,
        ExactOccurrence,
        ExactOccurrenceRequest,
        ExactOccurrenceRecord,
        exact_occurrence
    );
    callable_code_service_method!(
        phrase_search,
        PhraseSearch,
        PhraseSearchRequest,
        LexicalOccurrenceRecord,
        phrase_search
    );
    callable_code_service_method!(
        symbol_search,
        SymbolSearch,
        CodeSymbolSearchRequest,
        SymbolPrimitiveRecord,
        symbol_search
    );
    callable_code_service_method!(
        qualified_name,
        QualifiedName,
        QualifiedNameRequest,
        SymbolPrimitiveRecord,
        qualified_name
    );
    callable_code_service_method!(
        signature_search,
        SignatureSearch,
        CodeSignatureRequest,
        SymbolPrimitiveRecord,
        signature_search
    );
    callable_code_service_method!(
        implementations,
        Implementations,
        CodeImplementationsRequest,
        SymbolRelationRecord,
        implementations
    );
    callable_code_service_method!(
        type_hierarchy,
        TypeHierarchy,
        CodeHierarchyRequest,
        TypeHierarchyRecord,
        type_hierarchy
    );
    callable_code_service_method!(
        callers,
        Callers,
        CodeRelationRequest,
        SymbolRelationRecord,
        callers
    );
    callable_code_service_method!(
        callees,
        Callees,
        CodeRelationRequest,
        SymbolRelationRecord,
        callees
    );
    callable_code_service_method!(
        impact,
        Impact,
        CodeImpactRequest,
        SymbolPrimitiveRecord,
        impact
    );
    callable_code_service_method!(
        module_api,
        ModuleApi,
        ModuleApiRequest,
        SymbolPrimitiveRecord,
        module_api
    );
    callable_code_service_method!(
        source_metadata,
        SourceMetadata,
        SourceMetadataRequest,
        SourceMetadataRecord,
        source_metadata
    );
    callable_code_service_method!(facets, Facets, CodeFacetRequest, CodeFacetRecord, facets);
    callable_code_service_method!(
        timeline,
        Timeline,
        CodeTimelineRequest,
        CodeTimelineRecord,
        timeline
    );
    callable_code_service_method!(
        declaration,
        Declaration,
        CodeNavigationRequest,
        SymbolPrimitiveRecord,
        declaration
    );
    callable_code_service_method!(
        definition,
        Definition,
        CodeNavigationRequest,
        SymbolPrimitiveRecord,
        definition
    );
    callable_code_service_method!(
        type_definition,
        TypeDefinition,
        CodeNavigationRequest,
        SymbolPrimitiveRecord,
        type_definition
    );
    callable_code_service_method!(
        references,
        References,
        CodeNavigationRequest,
        SymbolRelationRecord,
        references
    );
}

fn validate_code_query_outcome<T>(
    outcome: &RetrievalPortOutcome<CodeQueryPage<T>>,
    requested_generation: &CodeGenerationId,
    requested_page_size: u32,
) -> Result<(), ApplicationProblem> {
    let evidence = outcome.evidence();
    let unpinned = requested_generation.as_str() == UNPINNED_LATEST_GENERATION_SENTINEL;
    if evidence.temporal.requested_mode != TemporalModeV1::Current {
        return Err(invalid_code_query_outcome_problem());
    }
    if let Some(page) = &evidence.payload {
        let source_generation = evidence
            .temporal
            .source_generation
            .as_ref()
            .ok_or_else(stale_code_query_problem)?;
        let generation_matches_request = if unpinned {
            page.generation.as_str() != UNPINNED_LATEST_GENERATION_SENTINEL
                && source_generation == &page.generation
        } else {
            &page.generation == requested_generation && source_generation == requested_generation
        };
        if !generation_matches_request {
            return Err(stale_code_query_problem());
        }
        if page.validate().is_err() {
            return Err(invalid_code_query_outcome_problem());
        }
        let returned = page.items.len() as u64;
        let cursor_state_valid = match (&page.next_cursor, evidence.page.expires_at) {
            (Some(_), Some(expires_at)) => expires_at.0 > evidence.finished_at.0,
            (None, None) => true,
            _ => false,
        };
        if returned > u64::from(requested_page_size)
            || (page.next_cursor.is_some() && page.total == Some(returned))
            || !cursor_state_valid
            || evidence.page.returned != returned
            || evidence.coverage.returned != returned
            || evidence.page.total != page.total
            || evidence.page.cursor != page.next_cursor
        {
            return Err(invalid_code_query_outcome_problem());
        }
    } else {
        match evidence.temporal.source_generation.as_ref() {
            Some(generation)
                if unpinned && generation.as_str() != UNPINNED_LATEST_GENERATION_SENTINEL => {}
            Some(generation) if !unpinned && generation == requested_generation => {}
            None if matches!(
                outcome,
                RetrievalPortOutcome::Cancelled(_)
                    | RetrievalPortOutcome::TimedOut(_)
                    | RetrievalPortOutcome::Failed(_)
                    | RetrievalPortOutcome::Unavailable(_)
            ) => {}
            _ => return Err(stale_code_query_problem()),
        }
    }
    Ok(())
}

fn stale_code_query_problem() -> ApplicationProblem {
    ApplicationProblem::stale(
        SafeDiagnostic::new(
            "application.code-query.generation-mismatch",
            "The code-intelligence result does not belong to the requested index generation.",
        )
        .expect("static safe diagnostic is valid"),
    )
}

fn invalid_code_query_outcome_problem() -> ApplicationProblem {
    ApplicationProblem::unavailable(
        SafeDiagnostic::new(
            "application.code-query.invalid-port-evidence",
            "The callable code-intelligence result could not be verified.",
        )
        .expect("static safe diagnostic is valid"),
    )
}

fn invalid_code_query_problem() -> ApplicationProblem {
    ApplicationProblem::InvalidRequest {
        diagnostic: SafeDiagnostic::new(
            "application.code-query.invalid-request",
            "The callable code-intelligence request is invalid.",
        )
        .expect("static safe diagnostic is valid"),
        retry: RetryDirective::Never,
        legal_actions: Vec::new(),
    }
}
