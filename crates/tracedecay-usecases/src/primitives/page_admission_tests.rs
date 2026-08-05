use std::collections::{BTreeSet, HashSet};
use std::sync::Arc;

use tracedecay_application::{
    ApplicationWireOperation, CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot,
    Deadline, DisclosureClass, PageAdmissionError, PageAdmissionRequest, PageAdmissionService,
    PageRequest, RequestContext, RequestId, ResolvedScope, application_catalog_contributions,
};
use tracedecay_domain::{
    ActorId, CodeGenerationId, CommitId, ContentDigest, FileOccurrenceId, ManifestDigest,
    ProjectId, RepositoryId, SessionCursorKeyIdV1, SessionCursorVersionV1, SignedCursorKeyRefV1,
    UtcMicros, WorktreeId,
};
use tracedecay_temporal_query::ports::{InMemoryCursorAuthenticator, SessionCursorAuthenticator};
use tracedecay_tool_catalog::{
    BindingId, BindingSurface, CapabilityId, CatalogContributionV1, UseCaseId,
};

use super::page_admission::{
    AuthenticatedDiagnosticCursorAuthorityV1, DiagnosticPageAdmissionAdapterV1,
    DiagnosticPageLaneV1, ProjectSymbolGraphCursorSnapshotAuthority,
    SymbolGraphPageAdmissionAdapterV1, declared_primitive_page_owner,
};
use super::{AuthenticatedSymbolGraphCursorAdapter, SymbolGraphCursorPort};
use crate::diagnostics_query::DiagnosticQueryCursor;
use crate::lsp_runtime::{LspCodeIndexProjectionIdentity, LspCodeIndexProjectionIdentityPort};

struct StaticCodeIndexIdentity(LspCodeIndexProjectionIdentity);

impl LspCodeIndexProjectionIdentityPort for StaticCodeIndexIdentity {
    fn current_identity(
        &self,
        _project_root: std::path::PathBuf,
        _document_relative_path: Option<String>,
    ) -> tracedecay_lsp::LspRuntimeFuture<
        Result<LspCodeIndexProjectionIdentity, tracedecay_lsp::LspRuntimeFailure>,
    > {
        let identity = self.0.clone();
        Box::pin(async move { Ok(identity) })
    }
}

fn digest(byte: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).expect("digest")
}

fn catalog() -> Arc<[CatalogContributionV1]> {
    Arc::from(
        application_catalog_contributions()
            .expect("application catalog")
            .into_boxed_slice(),
    )
}

fn declared_binding(
    catalog: &[CatalogContributionV1],
    operation: ApplicationWireOperation,
) -> (BindingId, CapabilityId, UseCaseId) {
    let binding = catalog
        .iter()
        .flat_map(CatalogContributionV1::bindings)
        .find(|binding| {
            binding.surface() == BindingSurface::Http
                && ApplicationWireOperation::from_catalog_name(binding.operation().as_str())
                    == Some(operation)
        })
        .expect("declared HTTP binding");
    let capability = catalog
        .iter()
        .flat_map(CatalogContributionV1::capabilities)
        .find(|capability| capability.capability_id() == binding.capability_id())
        .expect("declared capability");
    (
        binding.binding_id().clone(),
        capability.capability_id().clone(),
        capability.use_case_id().clone(),
    )
}

fn context(capability: CapabilityId, use_case: UseCaseId) -> RequestContext {
    let scope = ResolvedScope::new(
        ProjectId::new("project.page-admission").expect("project"),
        RepositoryId::new("repository.page-admission").expect("repository"),
        WorktreeId::new("worktree.page-admission").expect("worktree"),
        None,
    )
    .expect("scope");
    let expires_at = UtcMicros(i64::MAX / 2);
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new("grant.page-admission").expect("grant"),
        1,
        digest('a'),
        ActorId::new("actor.page-admission.issuer").expect("issuer"),
        UtcMicros(1),
        expires_at,
        scope.clone(),
        BTreeSet::from([capability]),
        BTreeSet::from([use_case]),
        DisclosureClass::Evidence,
    )
    .expect("grant");
    RequestContext::new(
        ActorId::new("actor.page-admission").expect("actor"),
        scope,
        grant,
        RequestId::new("request.page-admission").expect("request"),
        Deadline::new(expires_at).expect("deadline"),
        CancellationContext::active("cancel.page-admission").expect("cancellation"),
    )
    .expect("context")
}

fn authenticator(key: &SignedCursorKeyRefV1) -> Arc<dyn SessionCursorAuthenticator + Send + Sync> {
    Arc::new(
        InMemoryCursorAuthenticator::new(key.clone(), vec![9_u8; 32])
            .expect("cursor authenticator"),
    )
}

fn key() -> SignedCursorKeyRefV1 {
    SignedCursorKeyRefV1 {
        key_id: SessionCursorKeyIdV1::new("cursor.page-admission").expect("key"),
        version: SessionCursorVersionV1::new(1).expect("version"),
    }
}

fn symbol_graph_cursors(
    context: &RequestContext,
    key: &SignedCursorKeyRefV1,
) -> Arc<dyn SymbolGraphCursorPort> {
    let scope = context.scope().clone();
    let identity = LspCodeIndexProjectionIdentity {
        repository: scope.repository_id.clone(),
        worktree: Some(scope.worktree_id.clone()),
        reference: scope.reference.clone(),
        source_revision: Some(CommitId::new("commit.page-admission").expect("commit")),
        code_generation_id: CodeGenerationId::new("generation.page-admission.graph.11")
            .expect("generation"),
        snapshot_digest: digest('b'),
        invalidation_digest: digest('f'),
        snapshot_content_digest: ContentDigest::new(format!("sha256:{}", "1".repeat(64)))
            .expect("content"),
        document_content_digest: None,
    };
    Arc::new(AuthenticatedSymbolGraphCursorAdapter::new(
        Arc::new(ProjectSymbolGraphCursorSnapshotAuthority::new(
            key.clone(),
            digest('c'),
            std::path::PathBuf::from("/project/page-admission"),
            scope,
            Arc::new(StaticCodeIndexIdentity(identity)),
        )),
        authenticator(key),
    ))
}

#[test]
fn every_catalog_declared_primitive_page_operation_has_an_authentic_owner() {
    let catalog = catalog();
    let bindings = catalog
        .iter()
        .flat_map(CatalogContributionV1::bindings)
        .map(|binding| (binding.binding_id(), binding))
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut declared = HashSet::new();
    for capability in catalog
        .iter()
        .flat_map(CatalogContributionV1::capabilities)
        .filter(|capability| capability.pagination().is_some())
    {
        for operation in capability
            .binding_ids()
            .iter()
            .filter_map(|binding_id| bindings.get(binding_id))
            .filter_map(|binding| {
                ApplicationWireOperation::from_catalog_name(binding.operation().as_str())
            })
            .filter(|operation| {
                operation.owner_kind() == tracedecay_application::ApplicationOwnerKind::Primitive
            })
        {
            declared.insert(operation);
            assert!(
                declared_primitive_page_owner(operation).is_some(),
                "{} declares pagination without an authentic owner",
                operation.as_str()
            );
        }
    }
    assert!(!declared.is_empty());
}

#[tokio::test]
async fn symbol_page_admission_uses_the_existing_authenticated_cursor_verifier() {
    let catalog = catalog();
    let operation = ApplicationWireOperation::CodeCallers;
    let (binding_id, capability, use_case) = declared_binding(&catalog, operation);
    let context = context(capability, use_case);
    let key = key();
    let cursors = symbol_graph_cursors(&context, &key);
    let observed_at = UtcMicros(2);
    let body_digest = digest('d');
    let claim = cursors
        .claim_page(&context, "callers", &body_digest, None, observed_at)
        .await
        .expect("claim page");
    let cursor = cursors
        .finish_page(
            &context,
            "callers",
            &body_digest,
            &claim,
            3,
            8,
            true,
            observed_at,
        )
        .await
        .expect("finish page")
        .expect("authentic cursor");
    let scope_digest = context.scope().scope_digest.clone();
    let cross_body = PageAdmissionRequest::new(
        binding_id.clone(),
        operation,
        context.clone(),
        observed_at,
        scope_digest.clone(),
        digest('e'),
        PageRequest::new(25, Some(cursor.clone())).expect("page"),
    )
    .expect("admission request");
    let rejected = PageAdmissionService::new(SymbolGraphPageAdmissionAdapterV1::new(
        Arc::clone(&catalog),
        Arc::clone(&cursors),
    ))
    .admit(cross_body)
    .await;
    assert_eq!(rejected, Err(PageAdmissionError::Denied));

    let request = PageAdmissionRequest::new(
        binding_id,
        operation,
        context,
        observed_at,
        scope_digest,
        body_digest,
        PageRequest::new(25, Some(cursor)).expect("page"),
    )
    .expect("admission request");
    let admitted =
        PageAdmissionService::new(SymbolGraphPageAdmissionAdapterV1::new(catalog, cursors))
            .admit(request)
            .await
            .expect("authenticated continuation");

    assert_eq!(admitted.operation(), operation);
    assert!(admitted.page().cursor.is_some());
}

#[tokio::test]
async fn diagnostic_page_admission_uses_generation_and_typed_lane() {
    let catalog = catalog();
    let operation = ApplicationWireOperation::DiagnosticsRead;
    let (binding_id, capability, use_case) = declared_binding(&catalog, operation);
    let context = context(capability, use_case);
    let key = key();
    let authority = AuthenticatedDiagnosticCursorAuthorityV1::new(
        key.clone(),
        digest('c'),
        authenticator(&key),
    );
    let generation = CodeGenerationId::new("generation.page-admission.1").expect("generation");
    let lane = FileOccurrenceId::new("file.page-admission").expect("file occurrence");
    let query_cursor =
        DiagnosticQueryCursor::decode("dq1:anchor.page-admission.1").expect("query cursor");
    let cursor = authority
        .encode(
            &query_cursor,
            &context,
            &generation,
            lane.as_str(),
            &digest('d'),
            UtcMicros(2),
        )
        .expect("authentic cursor");
    let scope_digest = context.scope().scope_digest.clone();
    let request = PageAdmissionRequest::new(
        binding_id,
        operation,
        context,
        UtcMicros(2),
        scope_digest,
        digest('d'),
        PageRequest::new(25, Some(cursor)).expect("page"),
    )
    .expect("admission request");
    let admitted = PageAdmissionService::new(DiagnosticPageAdmissionAdapterV1::new(
        catalog,
        authority,
        generation,
        DiagnosticPageLaneV1::File(lane),
    ))
    .admit(request)
    .await
    .expect("authenticated continuation");

    assert_eq!(admitted.operation(), operation);
}

#[tokio::test]
async fn binding_operation_mismatch_is_rejected_before_cursor_admission() {
    let catalog = catalog();
    let (binding_id, capability, use_case) =
        declared_binding(&catalog, ApplicationWireOperation::CodeCallers);
    let context = context(capability, use_case);
    let scope_digest = context.scope().scope_digest.clone();
    let request = PageAdmissionRequest::new(
        binding_id,
        ApplicationWireOperation::CodeSignatureSearch,
        context,
        UtcMicros(2),
        scope_digest,
        digest('d'),
        PageRequest::first(25).expect("page"),
    )
    .expect("admission request");
    let key = key();
    let cursors = symbol_graph_cursors(&context, &key);
    let result =
        PageAdmissionService::new(SymbolGraphPageAdmissionAdapterV1::new(catalog, cursors))
            .admit(request)
            .await;

    assert_eq!(result, Err(PageAdmissionError::BindingMismatch));
}
