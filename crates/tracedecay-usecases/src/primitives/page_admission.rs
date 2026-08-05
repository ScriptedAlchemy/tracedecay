use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use tracedecay_application::retrieval::{PrimitiveFailure, PrimitiveFailureKind};
use tracedecay_application::{
    ApplicationWireOperation, OpaqueCursor, PageAdmissionError, PageAdmissionFuture,
    PageAdmissionPort, PageAdmissionRequest, PageAdmissionSeal, RequestAdmission, RequestContext,
    ResolvedScope,
};
use tracedecay_domain::{
    CodeGenerationId, CommitId, FileOccurrenceId, ManifestDigest, RetrievalGrainV1, SessionId,
    SignedCursorKeyRefV1, TemporalModeV1, UtcMicros, canonical_sha256,
};
use tracedecay_temporal_query::cursor::{StableSortKey, encode_cursor, verify_cursor};
use tracedecay_temporal_query::ports::{
    BindingDigest, KernelVersions, SessionCursorAuthenticator, TemporalExecutionSnapshot,
    TemporalSnapshotRequest, TemporalWatermarks,
};
use tracedecay_temporal_query::resolution::ValidatedAuthorization;
use tracedecay_tool_catalog::{CatalogContributionV1, SurfaceBindingV1};

use super::concrete::SymbolGraphCursorSnapshotAuthority;
use super::symbol_graph::SymbolGraphCursorPort;
use crate::diagnostics_publication::CodeIndexPublicationIdentityPortV1;
use crate::diagnostics_query::{DiagnosticQueryCoverage, DiagnosticQueryCursor, DiagnosticsQuery};
use crate::lsp_runtime::LspCodeIndexProjectionIdentityPort;
use crate::operation_stream::{
    CanonicalManagedTestRunReader, ManagedTestResultPageAdmission, ManagedTestRunCurrentScope,
};
use crate::tracedecay::TraceDecay;
use tracedecay_runtime_core::db::Database;

#[derive(Clone, Copy)]
pub(super) enum PrimitivePageOwner {
    SymbolGraph(&'static str),
    Diagnostics,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManagedTestRunCurrentIdentity {
    pub head_commit_id: CommitId,
    pub code_generation_id: CodeGenerationId,
}

pub type ManagedTestRunCurrentIdentityFuture<'a> = Pin<
    Box<
        dyn Future<
                Output = Result<
                    ManagedTestRunCurrentIdentity,
                    tracedecay_application::ApplicationContractError,
                >,
            > + Send
            + 'a,
    >,
>;

pub trait ManagedTestRunCurrentScopePort: Send + Sync {
    fn current_identity(&self) -> ManagedTestRunCurrentIdentityFuture<'_>;
}

#[derive(Clone, Debug)]
pub enum Pr12PrimitivePageOwner {
    SymbolGraph,
    Diagnostics(super::runtime::DiagnosticsPrimitiveRequest),
    TestResults,
}

pub(super) const fn declared_primitive_page_owner(
    operation: ApplicationWireOperation,
) -> Option<PrimitivePageOwner> {
    match operation {
        ApplicationWireOperation::CodeSymbolSearch => {
            Some(PrimitivePageOwner::SymbolGraph("search"))
        }
        ApplicationWireOperation::CodeSignatureSearch => {
            Some(PrimitivePageOwner::SymbolGraph("signature"))
        }
        ApplicationWireOperation::CodeImplementations => {
            Some(PrimitivePageOwner::SymbolGraph("implementations"))
        }
        ApplicationWireOperation::CodeTypeHierarchy => {
            Some(PrimitivePageOwner::SymbolGraph("hierarchy"))
        }
        ApplicationWireOperation::CodeCallers => Some(PrimitivePageOwner::SymbolGraph("callers")),
        ApplicationWireOperation::DiagnosticsRead => Some(PrimitivePageOwner::Diagnostics),
        _ => None,
    }
}

#[derive(Clone)]
pub struct SymbolGraphPageAdmissionAdapterV1<C> {
    catalog: Arc<[CatalogContributionV1]>,
    cursors: C,
}

pub struct ProjectSymbolGraphCursorSnapshotAuthority {
    key: SignedCursorKeyRefV1,
    configuration_digest: ManifestDigest,
    project_root: PathBuf,
    scope: ResolvedScope,
    code_index: Arc<dyn LspCodeIndexProjectionIdentityPort>,
}

impl ProjectSymbolGraphCursorSnapshotAuthority {
    pub(super) fn new(
        key: SignedCursorKeyRefV1,
        configuration_digest: ManifestDigest,
        project_root: PathBuf,
        scope: ResolvedScope,
        code_index: Arc<dyn LspCodeIndexProjectionIdentityPort>,
    ) -> Self {
        Self {
            key,
            configuration_digest,
            project_root,
            scope,
            code_index,
        }
    }
}

fn symbol_graph_snapshot_failure(code: &'static str, message: &'static str) -> PrimitiveFailure {
    PrimitiveFailure {
        kind: PrimitiveFailureKind::Unavailable,
        code: code.to_owned(),
        message: message.to_owned(),
    }
}

impl SymbolGraphCursorSnapshotAuthority for ProjectSymbolGraphCursorSnapshotAuthority {
    fn snapshot<'a>(
        &'a self,
        context: &'a RequestContext,
        lane: &'a str,
        body_digest: &'a ManifestDigest,
        _observed_at: UtcMicros,
    ) -> super::concrete::SymbolGraphCursorSnapshotFuture<'a> {
        Box::pin(async move {
            let graph_identity = self
                .code_index
                .current_identity(self.project_root.clone(), None)
                .await
                .map_err(|_| {
                    symbol_graph_snapshot_failure(
                        "application.symbol-graph.identity",
                        "could not read the current symbol-graph identity",
                    )
                })?
                .admit_for_scope(&self.scope)
                .map_err(|_| {
                    symbol_graph_snapshot_failure(
                        "application.symbol-graph.scope",
                        "the current symbol-graph identity did not match the admitted scope",
                    )
                })?;
            let graph_snapshot_digest = canonical_sha256(&(
                "tracedecay.symbol-graph.snapshot.v1",
                graph_identity.head_commit_id.as_str(),
                graph_identity.code_generation_id.as_str(),
                graph_identity.snapshot_digest.as_str(),
                graph_identity.invalidation_digest.as_str(),
                graph_identity.snapshot_content_digest.as_str(),
            ))
            .map_err(|_| {
                symbol_graph_snapshot_failure(
                    "application.symbol-graph.identity",
                    "could not derive the current symbol-graph snapshot digest",
                )
            })?;
            let request_digest = canonical_sha256(&(
                "tracedecay.symbol-graph.cursor.v1",
                context.actor(),
                context.grant().revision,
                &context.grant().digest,
                &context.grant().issuer,
                &context.grant().allowed_capabilities,
                &context.grant().allowed_use_cases,
                context.grant().disclosure,
                lane,
                body_digest.as_str(),
                graph_snapshot_digest.as_str(),
            ))
            .map_err(|_| {
                symbol_graph_snapshot_failure(
                    "application.symbol-graph.request",
                    "could not derive the symbol-graph cursor request digest",
                )
            })?;
            let request = TemporalSnapshotRequest::new(
                SessionId::new("session.daemon.primitive").map_err(|_| {
                    symbol_graph_snapshot_failure(
                        "application.symbol-graph.session",
                        "could not mint primitive session id",
                    )
                })?,
                context.scope().scope_digest.as_str(),
                request_digest.as_str(),
                context.grant().digest.as_str(),
                TemporalModeV1::Current,
                RetrievalGrainV1::Occurrence,
            )
            .map_err(|_| {
                symbol_graph_snapshot_failure(
                    "application.symbol-graph.snapshot",
                    "could not build temporal snapshot request",
                )
            })?;
            let watermark = graph_identity.generation.max(1);
            TemporalExecutionSnapshot::new_authorized(
                request,
                TemporalWatermarks {
                    generation: 1,
                    source: watermark,
                    projection: watermark,
                    index: watermark,
                    summary: watermark,
                },
                KernelVersions {
                    schema: 1,
                    ranking: 1,
                    configuration_digest: BindingDigest::new(
                        "configuration_digest",
                        canonical_sha256(&(
                            "tracedecay.symbol-graph.cursor-configuration.v1",
                            self.configuration_digest.as_str(),
                            graph_snapshot_digest.as_str(),
                        ))
                        .map_err(|_| {
                            symbol_graph_snapshot_failure(
                                "application.symbol-graph.configuration",
                                "could not bind the symbol-graph snapshot configuration",
                            )
                        })?
                        .as_str(),
                    )
                    .map_err(|_| {
                        symbol_graph_snapshot_failure(
                            "application.symbol-graph.configuration",
                            "invalid configuration digest",
                        )
                    })?,
                },
                Some(self.key.clone()),
                ValidatedAuthorization::Authorized,
            )
            .map_err(|_| {
                symbol_graph_snapshot_failure(
                    "application.symbol-graph.snapshot",
                    "could not authorize temporal snapshot",
                )
            })
        })
    }
}

impl<C> SymbolGraphPageAdmissionAdapterV1<C> {
    pub fn new(catalog: Arc<[CatalogContributionV1]>, cursors: C) -> Self {
        Self { catalog, cursors }
    }
}

impl<C> PageAdmissionPort for SymbolGraphPageAdmissionAdapterV1<C>
where
    C: SymbolGraphCursorPort,
{
    fn admit<'a>(
        &'a self,
        request: PageAdmissionRequest,
        seal: PageAdmissionSeal,
    ) -> PageAdmissionFuture<'a> {
        Box::pin(async move {
            let PrimitivePageOwner::SymbolGraph(lane) =
                declared_primitive_page_owner(request.operation())
                    .ok_or(PageAdmissionError::Unsupported)?
            else {
                return Err(PageAdmissionError::Unsupported);
            };
            let observed_at = request.observed_at();
            validate_catalog_page_request(&self.catalog, &request, observed_at)?;
            if let Some(cursor) = &request.page().cursor {
                self.cursors
                    .claim_page(
                        request.context(),
                        lane,
                        request.body_digest(),
                        Some(cursor),
                        observed_at,
                    )
                    .await
                    .map_err(|failure| match failure.kind {
                        PrimitiveFailureKind::InvalidRequest => PageAdmissionError::InvalidRequest,
                        PrimitiveFailureKind::NotFoundOrNotAuthorized => PageAdmissionError::Denied,
                        PrimitiveFailureKind::Stale => PageAdmissionError::Stale,
                        PrimitiveFailureKind::Unavailable => PageAdmissionError::Unavailable,
                    })?;
            }
            Ok(seal.admit(request))
        })
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn admit_from_owned_primitive_runtime<'a>(
    symbol: &'a SymbolGraphPageAdmissionAdapterV1<Arc<dyn SymbolGraphCursorPort>>,
    extended: &'a dyn super::runtime::Pr12ExtendedPrimitivePort,
    admitted_root_uri: &'a str,
    test_runs: &'a CanonicalManagedTestRunReader,
    test_run_scope: &'a dyn ManagedTestRunCurrentScopePort,
    request: PageAdmissionRequest,
    owner: Pr12PrimitivePageOwner,
    seal: PageAdmissionSeal,
) -> PageAdmissionFuture<'a> {
    match owner {
        Pr12PrimitivePageOwner::SymbolGraph => PageAdmissionPort::admit(symbol, request, seal),
        Pr12PrimitivePageOwner::Diagnostics(diagnostic) => Box::pin(async move {
            extended
                .admit_diagnostic_page(request, &diagnostic, seal)
                .await
        }),
        Pr12PrimitivePageOwner::TestResults => admit_test_result_page_from_owned_runtime(
            admitted_root_uri,
            test_runs,
            test_run_scope,
            request,
            seal,
        ),
    }
}

pub(super) fn admit_test_result_page_from_owned_runtime<'a>(
    admitted_root_uri: &'a str,
    test_runs: &'a CanonicalManagedTestRunReader,
    test_run_scope: &'a dyn ManagedTestRunCurrentScopePort,
    request: PageAdmissionRequest,
    seal: PageAdmissionSeal,
) -> PageAdmissionFuture<'a> {
    Box::pin(async move {
        let identity = test_run_scope
            .current_identity()
            .await
            .map_err(|_| PageAdmissionError::Unavailable)?;
        let current = ManagedTestRunCurrentScope {
            root_uri: admitted_root_uri.to_owned(),
            head_commit_id: Some(identity.head_commit_id),
            code_generation_id: Some(identity.code_generation_id),
            document_uri: None,
            document_content_digest: None,
        };
        let owner = ManagedTestResultPageAdmission::new(
            test_runs.clone(),
            current,
            request.binding_id().clone(),
            request.body_digest().clone(),
        );
        PageAdmissionPort::admit(&owner, request, seal).await
    })
}

#[derive(Clone)]
pub(super) struct AuthenticatedDiagnosticCursorAuthorityV1 {
    key: SignedCursorKeyRefV1,
    configuration_digest: ManifestDigest,
    authenticator: Arc<dyn SessionCursorAuthenticator>,
}

impl AuthenticatedDiagnosticCursorAuthorityV1 {
    pub(super) fn new(
        key: SignedCursorKeyRefV1,
        configuration_digest: ManifestDigest,
        authenticator: Arc<dyn SessionCursorAuthenticator>,
    ) -> Self {
        Self {
            key,
            configuration_digest,
            authenticator,
        }
    }

    fn snapshot(
        &self,
        context: &RequestContext,
        generation: &CodeGenerationId,
        lane: &str,
        body_digest: &ManifestDigest,
        observed_at: UtcMicros,
    ) -> Result<TemporalExecutionSnapshot, ()> {
        if context.validate().is_err()
            || context.admission_at(observed_at) != RequestAdmission::Admitted
        {
            return Err(());
        }
        let request_digest = canonical_sha256(&(
            "tracedecay.diagnostics.cursor.v1",
            context.actor(),
            context.grant().revision,
            &context.grant().digest,
            &context.grant().issuer,
            &context.grant().allowed_capabilities,
            &context.grant().allowed_use_cases,
            context.grant().disclosure,
            generation.as_str(),
            lane,
            body_digest.as_str(),
        ))
        .map_err(|_| ())?;
        let request = TemporalSnapshotRequest::new(
            SessionId::new("session.daemon.diagnostics").map_err(|_| ())?,
            context.scope().scope_digest.as_str(),
            request_digest.as_str(),
            context.grant().digest.as_str(),
            TemporalModeV1::Current,
            RetrievalGrainV1::Occurrence,
        )
        .map_err(|_| ())?;
        TemporalExecutionSnapshot::new_authorized(
            request,
            TemporalWatermarks {
                generation: 1,
                source: 1,
                projection: 1,
                index: 1,
                summary: 1,
            },
            KernelVersions {
                schema: 1,
                ranking: 1,
                configuration_digest: BindingDigest::new(
                    "configuration_digest",
                    self.configuration_digest.as_str(),
                )
                .map_err(|_| ())?,
            },
            Some(self.key.clone()),
            ValidatedAuthorization::Authorized,
        )
        .map_err(|_| ())
    }

    pub(super) fn decode(
        &self,
        encoded: &str,
        context: &RequestContext,
        generation: &CodeGenerationId,
        lane: &str,
        body_digest: &ManifestDigest,
        observed_at: UtcMicros,
    ) -> Result<DiagnosticQueryCursor, ()> {
        let snapshot = self.snapshot(context, generation, lane, body_digest, observed_at)?;
        let sort_key =
            verify_cursor(encoded, &snapshot, self.authenticator.as_ref()).map_err(|_| ())?;
        if sort_key.normalized_score_micros != 0 || sort_key.knowledge_at_micros != 0 {
            return Err(());
        }
        DiagnosticQueryCursor::decode(&format!("dq1:{}", sort_key.stable_id)).map_err(|_| ())
    }

    pub(super) fn encode(
        &self,
        cursor: &DiagnosticQueryCursor,
        context: &RequestContext,
        generation: &CodeGenerationId,
        lane: &str,
        body_digest: &ManifestDigest,
        observed_at: UtcMicros,
    ) -> Result<OpaqueCursor, ()> {
        let snapshot = self.snapshot(context, generation, lane, body_digest, observed_at)?;
        let encoded = encode_cursor(
            &snapshot,
            &StableSortKey {
                normalized_score_micros: 0,
                knowledge_at_micros: 0,
                stable_id: cursor.anchor().to_owned(),
            },
            self.authenticator.as_ref(),
        )
        .map_err(|_| ())?;
        OpaqueCursor::new(encoded).map_err(|_| ())
    }
}

#[derive(Clone)]
pub(super) enum DiagnosticPageLaneV1 {
    Workspace,
    File(FileOccurrenceId),
}

impl DiagnosticPageLaneV1 {
    fn as_str(&self) -> &str {
        match self {
            Self::Workspace => "workspace",
            Self::File(file) => file.as_str(),
        }
    }
}

pub(super) async fn resolve_diagnostic_page_owner(
    graph: &TraceDecay,
    database: &Database,
    code_index: &dyn LspCodeIndexProjectionIdentityPort,
    diagnostic_identity: &dyn CodeIndexPublicationIdentityPortV1,
    context: &RequestContext,
    request: &super::runtime::DiagnosticsPrimitiveRequest,
) -> Result<(CodeGenerationId, DiagnosticPageLaneV1), PageAdmissionError> {
    let identity = diagnostic_identity
        .resolve(graph.project_root().to_path_buf())
        .await
        .ok_or(PageAdmissionError::Unavailable)?;
    let scope = context.scope();
    if identity.repository() != &scope.repository_id
        || identity.worktree() != Some(&scope.worktree_id)
        || identity.reference() != scope.reference.as_ref()
    {
        return Err(PageAdmissionError::Stale);
    }
    let (document_path, lane) = match &request.scope {
        super::runtime::DiagnosticsPrimitiveScope::Workspace => {
            (None, DiagnosticPageLaneV1::Workspace)
        }
        super::runtime::DiagnosticsPrimitiveScope::File(path) => {
            let path =
                crate::diagnostics_publication::code_index_logical_path(graph.project_root(), path)
                    .ok_or(PageAdmissionError::Unavailable)?;
            let file = identity
                .file(&path)
                .map(|(file, _)| file.clone())
                .ok_or(PageAdmissionError::Unavailable)?;
            (Some(path), DiagnosticPageLaneV1::File(file))
        }
        super::runtime::DiagnosticsPrimitiveScope::Package(_) => {
            return Err(PageAdmissionError::Unsupported);
        }
    };
    let current_index = code_index
        .current_identity(graph.project_root().to_path_buf(), document_path)
        .await
        .map_err(|_| PageAdmissionError::Unavailable)?;
    if current_index.code_generation_id != *identity.generation_id() {
        return Err(PageAdmissionError::Stale);
    }
    let current = DiagnosticsQuery::new(database.conn())
        .current_generation()
        .await;
    let generation = current.generation.ok_or(PageAdmissionError::Unavailable)?;
    if !matches!(current.coverage, DiagnosticQueryCoverage::Complete) {
        return Err(PageAdmissionError::Unavailable);
    }
    if generation != *identity.generation_id() {
        return Err(PageAdmissionError::Stale);
    }
    Ok((generation, lane))
}

pub(super) struct DiagnosticPageAdmissionAdapterV1 {
    catalog: Arc<[CatalogContributionV1]>,
    authority: AuthenticatedDiagnosticCursorAuthorityV1,
    generation: CodeGenerationId,
    lane: DiagnosticPageLaneV1,
}

impl DiagnosticPageAdmissionAdapterV1 {
    pub(super) fn new(
        catalog: Arc<[CatalogContributionV1]>,
        authority: AuthenticatedDiagnosticCursorAuthorityV1,
        generation: CodeGenerationId,
        lane: DiagnosticPageLaneV1,
    ) -> Self {
        Self {
            catalog,
            authority,
            generation,
            lane,
        }
    }
}

impl PageAdmissionPort for DiagnosticPageAdmissionAdapterV1 {
    fn admit<'a>(
        &'a self,
        request: PageAdmissionRequest,
        seal: PageAdmissionSeal,
    ) -> PageAdmissionFuture<'a> {
        Box::pin(async move {
            if !matches!(
                declared_primitive_page_owner(request.operation()),
                Some(PrimitivePageOwner::Diagnostics)
            ) {
                return Err(PageAdmissionError::Unsupported);
            }
            validate_catalog_page_request(&self.catalog, &request, request.observed_at())?;
            if let Some(cursor) = &request.page().cursor {
                self.authority
                    .decode(
                        cursor.as_str(),
                        request.context(),
                        &self.generation,
                        self.lane.as_str(),
                        request.body_digest(),
                        request.observed_at(),
                    )
                    .map_err(|_| PageAdmissionError::Stale)?;
            }
            Ok(seal.admit(request))
        })
    }
}

fn validate_catalog_page_request(
    catalog: &[CatalogContributionV1],
    request: &PageAdmissionRequest,
    observed_at: UtcMicros,
) -> Result<(), PageAdmissionError> {
    request
        .context()
        .validate()
        .map_err(|_| PageAdmissionError::Denied)?;
    request
        .operation_scope_digest()
        .validate()
        .map_err(|_| PageAdmissionError::InvalidRequest)?;
    request
        .body_digest()
        .validate()
        .map_err(|_| PageAdmissionError::InvalidRequest)?;
    if request.operation_scope_digest() != &request.context().scope().scope_digest
        || request.context().admission_at(observed_at) != RequestAdmission::Admitted
    {
        return Err(PageAdmissionError::Denied);
    }

    let mut bindings = catalog
        .iter()
        .flat_map(CatalogContributionV1::bindings)
        .filter(|binding| binding.binding_id() == request.binding_id());
    let binding = bindings.next().ok_or(PageAdmissionError::BindingMismatch)?;
    if bindings.next().is_some() {
        return Err(PageAdmissionError::Unavailable);
    }
    validate_binding_operation(binding, request.operation())?;

    let mut capabilities = catalog
        .iter()
        .flat_map(CatalogContributionV1::capabilities)
        .filter(|capability| capability.capability_id() == binding.capability_id());
    let capability = capabilities.next().ok_or(PageAdmissionError::Unavailable)?;
    if capabilities.next().is_some() {
        return Err(PageAdmissionError::Unavailable);
    }
    if !capability.availability().is_callable()
        || capability
            .binding_ids()
            .binary_search(binding.binding_id())
            .is_err()
    {
        return Err(PageAdmissionError::Unsupported);
    }
    let pagination = capability
        .pagination()
        .ok_or(PageAdmissionError::Unsupported)?;
    if request.page().page_size > pagination.maximum_page_size() {
        return Err(PageAdmissionError::InvalidRequest);
    }
    if !request
        .context()
        .allows(capability.capability_id(), capability.use_case_id())
    {
        return Err(PageAdmissionError::Denied);
    }
    Ok(())
}

fn validate_binding_operation(
    binding: &SurfaceBindingV1,
    operation: ApplicationWireOperation,
) -> Result<(), PageAdmissionError> {
    if ApplicationWireOperation::from_catalog_name(binding.operation().as_str()) != Some(operation)
    {
        return Err(PageAdmissionError::BindingMismatch);
    }
    Ok(())
}
