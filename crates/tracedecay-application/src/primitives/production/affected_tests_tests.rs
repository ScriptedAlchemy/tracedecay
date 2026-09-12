use std::collections::BTreeSet;
use std::sync::atomic::{AtomicUsize, Ordering};

use tracedecay_contracts::retrieval::{
    AffectedTestsRetrievalPort, PageRequest, ResultProjection, RetrievalOrder, RetrievalRequestMeta,
};
use tracedecay_contracts::{
    ApplicationOperation, CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot,
    Deadline, DisclosureClass, RequestContext, RequestId, ResultContractRef,
};
use tracedecay_domain::{
    ActorId, CodeGenerationId, ComponentVersion, ContentDigest, FileOccurrenceId,
    GenerationTestAttributionV1, ProjectId, ProviderEvaluationStateV1, RefId, RepositoryId,
    SessionCursorKeyIdV1, SessionCursorVersionV1, SymbolOccurrenceId,
    TestAttributionEvidenceClassV1, WorktreeId,
};
use tracedecay_tool_catalog::{CapabilityId, SchemaId, UseCaseId};

use super::super::concrete::SymbolGraphCursorSnapshotAuthority;
use super::super::runtime::StorageStatusHistoryPointV1;
use super::extended_primitive::{
    storage_status_history_path, update_storage_status_history_with_lock,
};
use super::*;
use std::path::PathBuf;
use tracedecay_code_index::provider::{
    GenerationProviderCoverageV1, GenerationProviderReadV1, GenerationTestAttributionJoinReadPort,
};
use tracedecay_code_index::test_attribution::{
    GenerationTestJoinCoverageV1, GenerationTestJoinDispositionV1,
    GenerationTestJoinPartialReasonV1, GenerationTestJoinRecordV1, GenerationTestJoinV1,
    TestAttributionJoinInputCoverageV1, TestAttributionOccurrenceV1, TestAttributionWatermarkV1,
};
use tracedecay_contracts::ResolvedScope;
use tracedecay_contracts::retrieval::{
    AffectedTestAttributionV1, AffectedTestsRequest, RetrievalPortContext,
};
use tracedecay_temporal_query::ports::InMemoryCursorAuthenticator;

struct AttributionFixture {
    calls: AtomicUsize,
    read: GenerationProviderReadV1<GenerationTestJoinV1>,
}

impl GenerationTestAttributionJoinReadPort for AttributionFixture {
    fn read_test_attribution(
        &self,
        _generation: &CodeGenerationId,
    ) -> GenerationProviderReadV1<GenerationTestJoinV1> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.read.clone()
    }
}

struct GenerationSwitchingFixture {
    current: CodeGenerationId,
    read: GenerationProviderReadV1<GenerationTestJoinV1>,
}

impl GenerationTestAttributionJoinReadPort for GenerationSwitchingFixture {
    fn read_test_attribution(
        &self,
        generation: &CodeGenerationId,
    ) -> GenerationProviderReadV1<GenerationTestJoinV1> {
        if generation == &self.current {
            self.read.clone()
        } else {
            GenerationProviderReadV1::new(
                ProviderEvaluationStateV1::Unavailable,
                GenerationProviderCoverageV1::Unavailable,
                None,
            )
            .expect("unavailable provider read")
        }
    }
}

fn generation(value: &str) -> CodeGenerationId {
    CodeGenerationId::new(value).expect("generation")
}

fn digest(value: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", value.to_string().repeat(64))).expect("digest")
}

fn content(value: char) -> ContentDigest {
    ContentDigest::new(format!("sha256:{}", value.to_string().repeat(64))).expect("content")
}

fn context(project_id: ProjectId) -> (RequestContext, ApplicationOperation, ResolvedScope) {
    let scope = ResolvedScope::new(
        project_id,
        RepositoryId::new("repository.affected-tests").expect("repository"),
        WorktreeId::new("worktree.affected-tests").expect("worktree"),
        Some(RefId::new("refs/heads/affected-tests").expect("reference")),
    )
    .expect("scope");
    let capability = CapabilityId::new("capability.affected-tests").expect("capability");
    let use_case = UseCaseId::new("use-case.affected-tests").expect("use case");
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new("grant.affected-tests").expect("grant"),
        1,
        digest('a'),
        ActorId::new("actor.affected-tests.issuer").expect("issuer"),
        UtcMicros(1),
        UtcMicros(10_000),
        scope.clone(),
        BTreeSet::from([capability.clone()]),
        BTreeSet::from([use_case.clone()]),
        DisclosureClass::Evidence,
    )
    .expect("grant");
    let request = RequestContext::new(
        ActorId::new("actor.affected-tests.requester").expect("actor"),
        scope.clone(),
        grant,
        RequestId::new("request.affected-tests").expect("request"),
        Deadline::new(UtcMicros(10_000)).expect("deadline"),
        CancellationContext::active("cancel.affected-tests").expect("cancellation"),
    )
    .expect("context");
    let operation = ApplicationOperation::new(
        capability,
        use_case,
        ResultContractRef::new(SchemaId::new("schema.affected-tests").expect("schema"), 1)
            .expect("contract"),
        true,
    );
    (request, operation, scope)
}

fn cursor_context(project: &str) -> RequestContext {
    let scope = ResolvedScope::new(
        ProjectId::new(project).expect("project"),
        RepositoryId::new("repository.diagnostics").expect("repository"),
        WorktreeId::new("worktree.diagnostics").expect("worktree"),
        Some(RefId::new("refs/heads/diagnostics").expect("reference")),
    )
    .expect("scope");
    let capability = CapabilityId::new("capability.diagnostics").expect("capability");
    let use_case = UseCaseId::new("use-case.diagnostics").expect("use case");
    let expires_at = UtcMicros(i64::MAX / 2);
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new("grant.diagnostics").expect("grant"),
        1,
        digest('a'),
        ActorId::new("actor.diagnostics.issuer").expect("issuer"),
        UtcMicros(1),
        expires_at,
        scope.clone(),
        BTreeSet::from([capability]),
        BTreeSet::from([use_case]),
        DisclosureClass::Evidence,
    )
    .expect("grant");
    RequestContext::new(
        ActorId::new("actor.diagnostics.requester").expect("actor"),
        scope,
        grant,
        RequestId::new("request.diagnostics").expect("request"),
        Deadline::new(expires_at).expect("deadline"),
        CancellationContext::active("cancel.diagnostics").expect("cancellation"),
    )
    .expect("context")
}

#[test]
fn diagnostic_cursor_binds_scope_generation_and_lane() {
    let key = SignedCursorKeyRefV1 {
        key_id: SessionCursorKeyIdV1::new("cursor.diagnostics").expect("key"),
        version: SessionCursorVersionV1::new(1).expect("version"),
    };
    let authenticator =
        InMemoryCursorAuthenticator::new(key.clone(), vec![7_u8; 32]).expect("authenticator");
    let authority = AuthenticatedDiagnosticCursorAuthorityV1 {
        key,
        configuration_digest: digest('c'),
        authenticator: Arc::new(authenticator),
    };
    let context = cursor_context("project.diagnostics");
    let current_generation = generation("generation.diagnostics.1");
    let query_cursor =
        DiagnosticQueryCursor::decode("dq1:anchor.diagnostic.1").expect("query cursor");
    let encoded = authority
        .encode(
            &query_cursor,
            &context,
            &current_generation,
            DIAGNOSTIC_CURSOR_LANE_WORKSPACE,
        )
        .expect("encode");

    assert_eq!(
        authority
            .decode(
                encoded.as_str(),
                &context,
                &current_generation,
                DIAGNOSTIC_CURSOR_LANE_WORKSPACE,
            )
            .expect("decode"),
        query_cursor
    );
    assert!(
        authority
            .decode(
                encoded.as_str(),
                &cursor_context("project.diagnostics.other"),
                &current_generation,
                DIAGNOSTIC_CURSOR_LANE_WORKSPACE,
            )
            .is_err()
    );
    assert!(
        authority
            .decode(
                encoded.as_str(),
                &context,
                &generation("generation.diagnostics.2"),
                DIAGNOSTIC_CURSOR_LANE_WORKSPACE,
            )
            .is_err()
    );
    assert!(
        authority
            .decode(
                encoded.as_str(),
                &context,
                &current_generation,
                "file.diagnostics",
            )
            .is_err()
    );
}

fn symbol_graph_context(request_id: RequestId) -> RequestContext {
    let scope = ResolvedScope::new(
        ProjectId::new("project.symbol-graph").expect("project"),
        RepositoryId::new("repository.symbol-graph").expect("repository"),
        WorktreeId::new("worktree.symbol-graph").expect("worktree"),
        Some(RefId::new("refs/heads/symbol-graph").expect("reference")),
    )
    .expect("scope");
    let capability = CapabilityId::new("capability.symbol-graph").expect("capability");
    let use_case = UseCaseId::new("use-case.symbol-graph").expect("use case");
    let expires_at = UtcMicros(i64::MAX / 2);
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new("grant.symbol-graph").expect("grant"),
        1,
        digest('a'),
        ActorId::new("actor.symbol-graph.issuer").expect("issuer"),
        UtcMicros(1),
        expires_at,
        scope.clone(),
        BTreeSet::from([capability]),
        BTreeSet::from([use_case]),
        DisclosureClass::Evidence,
    )
    .expect("grant");
    RequestContext::new(
        ActorId::new("actor.symbol-graph.requester").expect("actor"),
        scope,
        grant,
        request_id,
        Deadline::new(expires_at).expect("deadline"),
        CancellationContext::active("cancel.symbol-graph").expect("cancellation"),
    )
    .expect("context")
}

/// Stands in for the daemon-owned code index, publishing whichever
/// generation the test has most recently made current.
struct PublishedCodeIndexIdentity {
    scope: ResolvedScope,
    /// `None` models a sealed dirty-worktree generation: no commit's tree
    /// matches the indexed content.
    source_revision: Option<tracedecay_domain::CommitId>,
    published: Mutex<(CodeGenerationId, ManifestDigest)>,
}

struct RefusingCodeIndexIdentity;

impl LspCodeIndexProjectionIdentityPort for RefusingCodeIndexIdentity {
    fn current_identity(
        &self,
        _project_root: PathBuf,
        _document_relative_path: Option<String>,
    ) -> tracedecay_lsp::LspRuntimeFuture<
        Result<
            crate::lsp_runtime::LspCodeIndexProjectionIdentity,
            tracedecay_lsp::LspRuntimeFailure,
        >,
    > {
        Box::pin(async {
            Err(tracedecay_lsp::LspRuntimeFailure::new(
                "lsp-code-index-generation-unavailable",
            ))
        })
    }
}

impl PublishedCodeIndexIdentity {
    fn publish(&self, generation: &str, snapshot: char) {
        *self.published.lock().expect("published") = (
            CodeGenerationId::new(generation).expect("generation"),
            digest(snapshot),
        );
    }
}

impl LspCodeIndexProjectionIdentityPort for PublishedCodeIndexIdentity {
    fn current_identity(
        &self,
        _project_root: PathBuf,
        _document_relative_path: Option<String>,
    ) -> tracedecay_lsp::LspRuntimeFuture<
        Result<
            crate::lsp_runtime::LspCodeIndexProjectionIdentity,
            tracedecay_lsp::LspRuntimeFailure,
        >,
    > {
        let (code_generation_id, snapshot_digest) =
            self.published.lock().expect("published").clone();
        let identity = crate::lsp_runtime::LspCodeIndexProjectionIdentity {
            project: self.scope.project_id.clone(),
            repository: self.scope.repository_id.clone(),
            worktree: Some(self.scope.worktree_id.clone()),
            reference: self.scope.reference.clone(),
            head_commit_id: self.source_revision.clone(),
            source_revision: self.source_revision.clone(),
            code_generation_id,
            snapshot_digest,
            invalidation_digest: digest('e'),
            snapshot_content_digest: content('f'),
            document_file_occurrence_id: None,
            document_content_digest: None,
        };
        Box::pin(async move { Ok(identity) })
    }
}

fn symbol_graph_scope() -> ResolvedScope {
    ResolvedScope::new(
        ProjectId::new("project.symbol-graph").expect("project"),
        RepositoryId::new("repository.symbol-graph").expect("repository"),
        WorktreeId::new("worktree.symbol-graph").expect("worktree"),
        Some(RefId::new("refs/heads/symbol-graph").expect("reference")),
    )
    .expect("scope")
}

fn symbol_graph_cursor_authority(
    key: SignedCursorKeyRefV1,
) -> (
    Arc<PublishedCodeIndexIdentity>,
    ProjectSymbolGraphCursorSnapshotAuthority,
) {
    symbol_graph_cursor_authority_at(
        key,
        Some(tracedecay_domain::CommitId::new("a".repeat(40)).expect("commit")),
    )
}

fn symbol_graph_cursor_authority_at(
    key: SignedCursorKeyRefV1,
    source_revision: Option<tracedecay_domain::CommitId>,
) -> (
    Arc<PublishedCodeIndexIdentity>,
    ProjectSymbolGraphCursorSnapshotAuthority,
) {
    let scope = symbol_graph_scope();
    let code_index = Arc::new(PublishedCodeIndexIdentity {
        scope: scope.clone(),
        source_revision,
        published: Mutex::new((
            CodeGenerationId::new("generation.symbol-graph.code.11").expect("generation"),
            digest('d'),
        )),
    });
    let authority = ProjectSymbolGraphCursorSnapshotAuthority {
        key,
        configuration_digest: digest('c'),
        project_root: PathBuf::from("/symbol-graph"),
        scope,
        code_index: Arc::clone(&code_index) as Arc<dyn LspCodeIndexProjectionIdentityPort>,
    };
    (code_index, authority)
}

/// Production never mints a `sha256:`-prefixed request id, so a snapshot
/// bound to one could not be built, and a snapshot bound to the
/// correlation id could never be resumed by the next request. Both
/// contexts here carry ids minted by the real production surfaces.
#[tokio::test]
async fn symbol_graph_identity_refusal_names_the_runtime_failure() {
    let key = SignedCursorKeyRefV1 {
        key_id: SessionCursorKeyIdV1::new("cursor.symbol-graph").expect("key"),
        version: SessionCursorVersionV1::new(1).expect("version"),
    };
    let (_, mut authority) = symbol_graph_cursor_authority(key);
    authority.code_index = Arc::new(RefusingCodeIndexIdentity);
    let context = symbol_graph_context(
        tracedecay_contracts::request_identity::mint_global_request_id(
            tracedecay_contracts::request_identity::GlobalRequestSurface::McpFallback,
        )
        .expect("mcp fallback request id"),
    );

    let failure = authority
        .snapshot(&context, "search", now_observed())
        .await
        .expect_err("runtime refusal must remain typed");
    assert!(
        failure
            .message
            .contains("lsp-code-index-generation-unavailable"),
        "the public problem must name the underlying runtime refusal: {failure:?}"
    );
}

#[tokio::test]
async fn symbol_graph_cursors_resume_across_production_minted_request_ids() {
    let key = SignedCursorKeyRefV1 {
        key_id: SessionCursorKeyIdV1::new("cursor.symbol-graph").expect("key"),
        version: SessionCursorVersionV1::new(1).expect("version"),
    };
    let authenticator = Arc::new(
        InMemoryCursorAuthenticator::new(key.clone(), vec![9_u8; 32]).expect("authenticator"),
    );
    let (_code_index, snapshots) = symbol_graph_cursor_authority(key);
    let adapter = AuthenticatedSymbolGraphCursorAdapter::new(Arc::new(snapshots), authenticator);

    let issuing = symbol_graph_context(
        tracedecay_contracts::request_identity::mcp_connection_request_id(
            &serde_json::json!(1),
            "connection.symbol-graph",
        )
        .expect("mcp connection request id"),
    );
    let resuming = symbol_graph_context(
        tracedecay_contracts::request_identity::mint_global_request_id(
            tracedecay_contracts::request_identity::GlobalRequestSurface::McpFallback,
        )
        .expect("mcp fallback request id"),
    );
    assert_ne!(
        issuing.request_id().as_str(),
        resuming.request_id().as_str(),
        "each request carries its own correlation id"
    );

    let observed_at = now_observed();
    let claim = adapter
        .claim_page(&issuing, "search", None, observed_at)
        .await
        .expect("a production request must be able to claim a page");
    let cursor = adapter
        .finish_page(&issuing, "search", &claim, 3, 8, true, observed_at)
        .await
        .expect("a production request must be able to issue a page cursor")
        .expect("a page with more to serve mints a continuation");
    assert_eq!(
        adapter
            .claim_page(&resuming, "search", Some(&cursor), observed_at)
            .await
            .expect("the next production request must resume the page")
            .offset(),
        3
    );
    assert!(
        adapter
            .claim_page(&resuming, "callers", Some(&cursor), observed_at)
            .await
            .is_err(),
        "a cursor must not resume into another lane"
    );
}

/// The whole point of resolving the generation per request: once the code
/// index publishes a new generation, a cursor minted under the old one is
/// refused rather than quietly indexing into the replacement's rows.
#[tokio::test]
async fn a_cursor_minted_before_a_publication_does_not_resume_after_it() {
    let key = SignedCursorKeyRefV1 {
        key_id: SessionCursorKeyIdV1::new("cursor.symbol-graph").expect("key"),
        version: SessionCursorVersionV1::new(1).expect("version"),
    };
    let authenticator = Arc::new(
        InMemoryCursorAuthenticator::new(key.clone(), vec![9_u8; 32]).expect("authenticator"),
    );
    let (code_index, snapshots) = symbol_graph_cursor_authority(key);
    let adapter = AuthenticatedSymbolGraphCursorAdapter::new(Arc::new(snapshots), authenticator);
    let context = symbol_graph_context(
        tracedecay_contracts::request_identity::mint_global_request_id(
            tracedecay_contracts::request_identity::GlobalRequestSurface::McpFallback,
        )
        .expect("mcp fallback request id"),
    );

    let observed_at = now_observed();
    let claim = adapter
        .claim_page(&context, "search", None, observed_at)
        .await
        .expect("claim page");
    let cursor = adapter
        .finish_page(&context, "search", &claim, 3, 8, true, observed_at)
        .await
        .expect("finish page")
        .expect("continuation");
    assert_eq!(
        adapter
            .claim_page(&context, "search", Some(&cursor), observed_at)
            .await
            .expect("the unchanged generation still resumes")
            .offset(),
        3
    );

    code_index.publish("generation.symbol-graph.code.12", 'd');
    let failure = adapter
        .claim_page(&context, "search", Some(&cursor), observed_at)
        .await
        .expect_err("a superseded generation must refuse the cursor");
    assert_eq!(
        failure.kind,
        tracedecay_contracts::retrieval::PrimitiveFailureKind::Stale,
        "a cursor from a superseded generation is stale, not a different page"
    );

    // A re-index of the same commit republishes under the same generation
    // sequence with different content. The rows behind the cursor are still
    // gone, so the cursor must still be refused — the sequence is not the
    // whole identity.
    code_index.publish("generation.symbol-graph.code.11", '9');
    assert!(
        adapter
            .claim_page(&context, "search", Some(&cursor), observed_at)
            .await
            .is_err(),
        "a republication at the same sequence must not serve the old page-set"
    );
}

/// A dirty worktree seals a generation with no commit. Read-only graph
/// pages bind to that generation and content identity, so they are served
/// and resumable; the next publication still refuses the old cursor.
#[tokio::test]
async fn read_only_graph_pages_bind_to_a_sealed_dirty_worktree_generation() {
    let key = SignedCursorKeyRefV1 {
        key_id: SessionCursorKeyIdV1::new("cursor.symbol-graph").expect("key"),
        version: SessionCursorVersionV1::new(1).expect("version"),
    };
    let authenticator = Arc::new(
        InMemoryCursorAuthenticator::new(key.clone(), vec![9_u8; 32]).expect("authenticator"),
    );
    let (code_index, snapshots) = symbol_graph_cursor_authority_at(key, None);
    let adapter = AuthenticatedSymbolGraphCursorAdapter::new(Arc::new(snapshots), authenticator);
    let context = symbol_graph_context(
        tracedecay_contracts::request_identity::mint_global_request_id(
            tracedecay_contracts::request_identity::GlobalRequestSurface::McpFallback,
        )
        .expect("mcp fallback request id"),
    );

    let observed_at = now_observed();
    let claim = adapter
        .claim_page(&context, "search", None, observed_at)
        .await
        .expect("a sealed dirty generation serves read-only pages");
    let cursor = adapter
        .finish_page(&context, "search", &claim, 3, 8, true, observed_at)
        .await
        .expect("finish page")
        .expect("continuation");
    assert_eq!(
        adapter
            .claim_page(&context, "search", Some(&cursor), observed_at)
            .await
            .expect("the unchanged dirty generation still resumes")
            .offset(),
        3
    );

    code_index.publish("generation.symbol-graph.code.12", 'd');
    assert_eq!(
        adapter
            .claim_page(&context, "search", Some(&cursor), observed_at)
            .await
            .expect_err("a further edit supersedes the dirty generation")
            .kind,
        tracedecay_contracts::retrieval::PrimitiveFailureKind::Stale,
    );
}

#[test]
fn diagnostic_continuation_is_complete_coverage_not_partial_evidence() {
    let cursor = OpaqueCursor::new("opaque.diagnostics.next").expect("cursor");
    let outcome = diagnostics_result(
        generation("generation.diagnostics.1"),
        digest('b'),
        Vec::new(),
        2,
        Some(cursor.clone()),
        UtcMicros(100),
    );
    let RetrievalPortOutcome::Completed(evidence) = outcome else {
        panic!("bounded pagination must complete");
    };
    assert_eq!(
        evidence.coverage.completeness,
        CoverageCompleteness::Complete
    );
    assert_eq!(evidence.coverage.eligible, Some(2));
    assert_eq!(evidence.page.cursor, Some(PageCursor::from(cursor)));
    assert!(evidence.page.expires_at.is_some());
    assert!(!evidence.payload.expect("payload").findings_cleared);
}

fn request(generation: CodeGenerationId) -> AffectedTestsRequest {
    AffectedTestsRequest {
        symbol: SymbolOccurrenceId::new("symbol.source").expect("symbol"),
        generation,
        meta: RetrievalRequestMeta::current(
            PageRequest::first(100).expect("page"),
            ResultProjection::ReferencesOnly,
            RetrievalOrder::StableIdentity,
        ),
    }
}

fn complete_read(generation: CodeGenerationId) -> GenerationProviderReadV1<GenerationTestJoinV1> {
    let source = SymbolOccurrenceId::new("symbol.source").expect("source");
    let test = SymbolOccurrenceId::new("symbol.test").expect("test");
    let source_file = FileOccurrenceId::new("file.source").expect("source file");
    let test_file = FileOccurrenceId::new("file.test").expect("test file");
    let revision = ComponentVersion::new("test-attribution.v1").expect("revision");
    let attribution = GenerationTestAttributionV1 {
        generation_id: generation.clone(),
        source_revision: None,
        test_occurrence: test.clone(),
        covered_occurrences: vec![source.clone()],
        evidence_class: TestAttributionEvidenceClassV1::ConservativeDependencyCandidates,
        attribution_revision: revision.clone(),
    };
    let test_occurrence = TestAttributionOccurrenceV1 {
        occurrence_id: test.clone(),
        file_occurrence_id: test_file,
        content_digest: content('b'),
    };
    let source_occurrence = TestAttributionOccurrenceV1 {
        occurrence_id: source,
        file_occurrence_id: source_file,
        content_digest: content('c'),
    };
    let join = GenerationTestJoinV1 {
        generation_id: generation.clone(),
        code_snapshot_digest: digest('d'),
        code_content_identity: content('e'),
        test_watermark: TestAttributionWatermarkV1 {
            generation_id: generation,
            snapshot_digest: digest('d'),
            content_identity: content('e'),
            source_revision: None,
            attribution_revision: revision,
            evidence_digest: digest('f'),
            coverage: TestAttributionJoinInputCoverageV1::Complete,
        },
        records: vec![GenerationTestJoinRecordV1 {
            attribution,
            test_occurrence: Some(test_occurrence),
            covered_occurrences: vec![source_occurrence],
            disposition: GenerationTestJoinDispositionV1::Current {
                evidence_class: TestAttributionEvidenceClassV1::ConservativeDependencyCandidates,
            },
        }],
        coverage: GenerationTestJoinCoverageV1::Complete,
    };
    GenerationProviderReadV1::new(
        ProviderEvaluationStateV1::SupportedCompletedComplete,
        GenerationProviderCoverageV1::Complete {
            examined: 1,
            eligible: 1,
            excluded: 0,
        },
        Some(join),
    )
    .expect("provider read")
}

#[test]
fn exact_project_and_generation_route_canonical_attribution() {
    let project_id = ProjectId::new("project.affected-tests").expect("project");
    let generation = generation("generation.affected-tests.1");
    let authority = Arc::new(AttributionFixture {
        calls: AtomicUsize::new(0),
        read: complete_read(generation.clone()),
    });
    let port = TraceDecayAffectedTestsPortV1::from_binding(
        Some(project_id.clone()),
        generation.clone(),
        Some(authority.clone()),
    );
    let (context, operation, _) = context(project_id);

    let outcome = port.affected_tests(
        &RetrievalPortContext {
            request: &context,
            operation: &operation,
        },
        &request(generation.clone()),
    );

    assert_eq!(authority.calls.load(Ordering::Relaxed), 1);
    let RetrievalPortOutcome::Completed(evidence) = outcome else {
        panic!("exact current attribution must complete");
    };
    assert_eq!(evidence.temporal.source_generation, Some(generation));
    assert_eq!(evidence.temporal.watermark_digest, Some(digest('f')));
    assert_eq!(evidence.evidence_authorities.len(), 1);
    assert_eq!(
        evidence.evidence_authorities[0].source_kind,
        "test_attribution"
    );
    let payload = evidence.payload.expect("payload");
    assert_eq!(
        payload.tests,
        vec![SymbolOccurrenceId::new("symbol.test").expect("test")]
    );
    assert_eq!(
        payload.attributions,
        vec![AffectedTestAttributionV1 {
            test: SymbolOccurrenceId::new("symbol.test").expect("test"),
            evidence_class: TestAttributionEvidenceClassV1::ConservativeDependencyCandidates,
        }]
    );
}

#[test]
fn attribution_class_is_preserved_without_inference() {
    for evidence_class in [
        TestAttributionEvidenceClassV1::ObservedCoverageCandidates,
        TestAttributionEvidenceClassV1::PredictiveRankedCandidates,
    ] {
        let project_id = ProjectId::new("project.affected-tests").expect("project");
        let generation = generation("generation.affected-tests.1");
        let mut read = complete_read(generation.clone());
        let record = &mut read.evidence.as_mut().expect("join").records[0];
        record.attribution.evidence_class = evidence_class;
        record.disposition = GenerationTestJoinDispositionV1::Current { evidence_class };
        let port = TraceDecayAffectedTestsPortV1::from_binding(
            Some(project_id.clone()),
            generation.clone(),
            Some(Arc::new(AttributionFixture {
                calls: AtomicUsize::new(0),
                read,
            })),
        );
        let (context, operation, _) = context(project_id);

        let RetrievalPortOutcome::Completed(evidence) = port.affected_tests(
            &RetrievalPortContext {
                request: &context,
                operation: &operation,
            },
            &request(generation),
        ) else {
            panic!("current attribution must complete");
        };
        assert_eq!(
            evidence.payload.expect("payload").attributions[0].evidence_class,
            evidence_class
        );
    }
}

#[test]
fn unknown_attribution_remains_typed_partial() {
    let project_id = ProjectId::new("project.affected-tests").expect("project");
    let generation = generation("generation.affected-tests.1");
    let mut read = complete_read(generation.clone());
    read.provider_state = ProviderEvaluationStateV1::Partial;
    read.coverage = GenerationProviderCoverageV1::Partial {
        examined: 1,
        eligible: 0,
        excluded: 0,
        unknown: 1,
        capped: false,
    };
    let join = read.evidence.as_mut().expect("join");
    join.coverage = GenerationTestJoinCoverageV1::Partial {
        reasons: vec![GenerationTestJoinPartialReasonV1::UnknownUnsupported {
            test_occurrence: SymbolOccurrenceId::new("symbol.test").expect("test"),
        }],
    };
    join.records[0].attribution.evidence_class = TestAttributionEvidenceClassV1::UnknownUnsupported;
    join.records[0].disposition = GenerationTestJoinDispositionV1::UnknownUnsupported;
    let port = TraceDecayAffectedTestsPortV1::from_binding(
        Some(project_id.clone()),
        generation.clone(),
        Some(Arc::new(AttributionFixture {
            calls: AtomicUsize::new(0),
            read,
        })),
    );
    let (context, operation, _) = context(project_id);

    let RetrievalPortOutcome::Partial(evidence) = port.affected_tests(
        &RetrievalPortContext {
            request: &context,
            operation: &operation,
        },
        &request(generation),
    ) else {
        panic!("unknown attribution must stay partial");
    };
    let payload = evidence.payload.expect("payload");
    assert!(payload.tests.is_empty());
    assert_eq!(
        payload.attributions[0].evidence_class,
        TestAttributionEvidenceClassV1::UnknownUnsupported
    );
}

#[test]
fn absent_or_mismatched_authority_never_fabricates_complete_empty() {
    let project_id = ProjectId::new("project.affected-tests").expect("project");
    let expected_generation = generation("generation.affected-tests.1");
    let requested_generation = generation("generation.affected-tests.2");
    let port = TraceDecayAffectedTestsPortV1::from_binding(
        Some(project_id.clone()),
        expected_generation.clone(),
        None,
    );
    let (context, operation, _) = context(project_id);

    let outcome = port.affected_tests(
        &RetrievalPortContext {
            request: &context,
            operation: &operation,
        },
        &request(requested_generation),
    );

    assert!(matches!(outcome, RetrievalPortOutcome::Unavailable(_)));

    let authority = Arc::new(AttributionFixture {
        calls: AtomicUsize::new(0),
        read: complete_read(expected_generation.clone()),
    });
    let port = TraceDecayAffectedTestsPortV1::from_binding(
        Some(ProjectId::new("project.other").expect("other project")),
        expected_generation.clone(),
        Some(authority.clone()),
    );
    let outcome = port.affected_tests(
        &RetrievalPortContext {
            request: &context,
            operation: &operation,
        },
        &request(expected_generation),
    );

    assert!(matches!(outcome, RetrievalPortOutcome::Unavailable(_)));
    assert_eq!(authority.calls.load(Ordering::Relaxed), 0);
}

#[test]
fn port_routes_each_current_generation_instead_of_pinning_open_generation() {
    let project_id = ProjectId::new("project.affected-tests").expect("project");
    let opened_generation = generation("generation.affected-tests.1");
    let current_generation = generation("generation.affected-tests.2");
    let port = TraceDecayAffectedTestsPortV1::from_binding(
        Some(project_id.clone()),
        opened_generation,
        Some(Arc::new(GenerationSwitchingFixture {
            current: current_generation.clone(),
            read: complete_read(current_generation.clone()),
        })),
    );
    let (context, operation, _) = context(project_id);

    let outcome = port.affected_tests(
        &RetrievalPortContext {
            request: &context,
            operation: &operation,
        },
        &request(current_generation),
    );

    assert!(matches!(outcome, RetrievalPortOutcome::Completed(_)));
}

#[test]
fn storage_status_history_is_reloaded_from_durable_scope_file() {
    let directory = tempfile::tempdir().expect("history tempdir");
    let history_path = directory.path().join("storage-status-history-v1.json");
    let history_lock = Mutex::new(());
    let project_id = Some("project.storage-status".to_owned());
    let store_path = "/project/.tracedecay/graph.db".to_owned();

    let (first, first_coverage) = update_storage_status_history_with_lock(
        &history_lock,
        &history_path,
        project_id.clone(),
        store_path.clone(),
        4096,
        1,
    );
    assert_eq!(first.len(), 1);
    assert_eq!(first_coverage, "durable_project_store_history");
    assert!(history_path.is_file());

    let (second, second_coverage) = update_storage_status_history_with_lock(
        &history_lock,
        &history_path,
        project_id,
        store_path,
        8192,
        2,
    );
    assert_eq!(second.len(), 2);
    assert_eq!(second[0].database_bytes, 4096);
    assert_eq!(second[1].database_bytes, 8192);
    assert_eq!(second_coverage, "durable_project_store_history");
}

#[test]
fn storage_status_history_records_changes_not_reads() {
    let directory = tempfile::tempdir().expect("history tempdir");
    let history_path = directory.path().join("storage-status-history-v1.json");
    let history_lock = Mutex::new(());
    let project_id = Some("project.storage-status".to_owned());
    let store_path = "/project/.tracedecay/graph.db".to_owned();

    let (first, _) = update_storage_status_history_with_lock(
        &history_lock,
        &history_path,
        project_id.clone(),
        store_path.clone(),
        4096,
        1,
    );
    let (repeated, repeated_coverage) = update_storage_status_history_with_lock(
        &history_lock,
        &history_path,
        project_id.clone(),
        store_path.clone(),
        4096,
        2,
    );

    assert_eq!(first, repeated, "an unchanged store must read idempotently");
    assert_eq!(repeated.len(), 1);
    assert_eq!(repeated[0].observed_at, 1);
    assert_eq!(repeated_coverage, "durable_project_store_history");

    let (changed, _) = update_storage_status_history_with_lock(
        &history_lock,
        &history_path,
        project_id,
        store_path,
        8192,
        3,
    );
    assert_eq!(changed.len(), 2);
    assert_eq!(changed[1].database_bytes, 8192);
    assert_eq!(changed[1].observed_at, 3);
}

/// The history lock is process-global across every project's storage
/// status read. Contention must degrade to the current sample as a typed
/// bounded state; the prior blocking acquire convoyed every concurrent
/// status read behind one stalled history write, which is how a metadata
/// status tool timed out its admitted deadline on a busy profile.
///
/// The fixture owns its lock explicitly so it cannot place an unrelated
/// history test into the production singleton's contended state.
#[test]
fn storage_status_history_lock_contention_is_a_typed_bounded_state() {
    let directory = tempfile::tempdir().expect("history tempdir");
    let history_path = directory.path().join("storage-status-history-v1.json");
    let history_lock = Arc::new(Mutex::new(()));

    let held = history_lock.lock().expect("hold the fixture history lock");
    let reader_lock = Arc::clone(&history_lock);
    let (result_sender, result_receiver) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let result = update_storage_status_history_with_lock(
            &reader_lock,
            &history_path,
            Some("project.storage-status".to_owned()),
            "/project/.tracedecay/graph.db".to_owned(),
            4096,
            1,
        );
        let _ = result_sender.send(result);
    });

    // The old path parked here until the holder released the lock; the
    // bounded contract answers while the lock is provably still held.
    let (history, coverage) = result_receiver
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("a contended history read must answer without the lock");
    drop(held);
    reader.join().expect("join contended reader");

    assert_eq!(coverage, "current_sample_only_history_lock_contended");
    assert_eq!(
        history,
        vec![StorageStatusHistoryPointV1 {
            observed_at: 1,
            database_bytes: 4096,
        }],
        "contention reports exactly the live sample, never a partial file read"
    );
}

#[test]
fn storage_status_history_paths_are_store_scope_isolated() {
    let root = Path::new("/profile/projects/project.storage-status");
    let first = storage_status_history_path(
        root,
        Some("project.storage-status"),
        "/project/branches/main/graph.db",
    );
    let second = storage_status_history_path(
        root,
        Some("project.storage-status"),
        "/project/branches/topic/graph.db",
    );

    assert_ne!(first, second);
    assert_eq!(first.parent(), second.parent());
}

#[test]
fn invalid_storage_status_history_is_reset_without_claiming_full_history() {
    let directory = tempfile::tempdir().expect("history tempdir");
    let history_path = directory.path().join("storage-status-history-v1.json");
    let history_lock = Mutex::new(());
    std::fs::write(&history_path, b"{not-json").expect("invalid history");

    let (history, coverage) = update_storage_status_history_with_lock(
        &history_lock,
        &history_path,
        Some("project.storage-status".to_owned()),
        "/project/.tracedecay/graph.db".to_owned(),
        4096,
        1,
    );

    assert_eq!(history.len(), 1);
    assert_eq!(coverage, "durable_project_store_history_reset_invalid");
}
