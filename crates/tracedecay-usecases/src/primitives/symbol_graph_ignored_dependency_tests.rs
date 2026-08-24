use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use sha2::{Digest, Sha256};
use tracedecay_application::retrieval::{
    ExactSymbolRequest, PrimitiveFailureKind, ResultProjection, RetrievalOrder,
    RetrievalRequestMeta, SymbolGraphPortContext, SymbolGraphPortOutcome, SymbolGraphPrimitivePort,
    SymbolGraphScope, SymbolSearchPrimitiveRequest,
};
use tracedecay_application::{
    ApplicationOperation, CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot,
    Deadline, DisclosureClass, PageRequest, RequestAdmission, RequestContext, RequestId,
    ResolvedScope, ResultContractRef,
};
use tracedecay_code_index::chunks::CodeIndexImportEvidenceV1;
use tracedecay_code_index::graph_projection::{
    CODE_GRAPH_PROJECTOR_REVISION, CodeGraphProjectionStore, CodeGraphSymbolBindingV1,
    build_code_graph_manifest, code_graph_projection_identity,
};
use tracedecay_code_index::lineage::LineageSymbolRecordV1;
use tracedecay_domain::{
    ActorId, CodeGenerationId, ContentDigest, EphemeralSanitizedQueryViewV1, FileIdentityDigest,
    FileOccurrenceId, LanguageDescriptorRevision, LanguageId, ManifestDigest, ProjectId,
    QueryNormalizationRevision, RefId, RepositoryId, RetrievalGrainV1, SanitizedCodeFileV1,
    SanitizerRevision, SessionId, SnapshotFileDispositionV1, SourceSpan, SymbolIdentityDigest,
    SymbolOccurrenceId, TemporalModeV1, UtcMicros, WorktreeId,
};
use tracedecay_graph_db::{
    GraphEntity, GraphEntityId, GraphEntityRef, GraphGenerationManifest, GraphGenerationRelation,
    GraphLabel, GraphNamespace, GraphProjectorRevision, GraphProperty, GraphPropertyName,
    GraphRelationId, GraphRelationKind, NeverCancelled, VerifiedGraphSnapshot,
};
use tracedecay_temporal_query::ports::{
    BindingDigest, KernelVersions, TemporalExecutionSnapshot, TemporalSnapshotRequest,
    TemporalWatermarks,
};
use tracedecay_temporal_query::resolution::ValidatedAuthorization;
use tracedecay_tool_catalog::{CapabilityId, SchemaId, UseCaseId};

use super::symbol_graph::{
    CanonicalSymbolGraphAdapter, SymbolGraphCursorFuture, SymbolGraphCursorPort,
    SymbolGraphPageClaim,
};
use crate::code_index::{
    CodeIndexIgnoredDependencyAdmissionErrorV1, CodeIndexIgnoredDependencyAdmissionFutureV1,
    CodeIndexIgnoredDependencyAdmissionPortV1, CodeIndexIgnoredDependencyAdmissionRequestV1,
};
use crate::graph::{
    CodeGraphProjectionReadPort, CodeGraphReadError, CodeGraphReadFuture, CodeGraphReadRequest,
    VerifiedCodeGraphRead,
};

mod edge_cases;

const NOW: UtcMicros = UtcMicros(1_000);

#[derive(Clone)]
struct FixtureCodeGraphProjection {
    scope: ResolvedScope,
    store: Arc<CodeGraphProjectionStore>,
}

impl CodeGraphProjectionReadPort for FixtureCodeGraphProjection {
    fn open<'a>(&'a self, request: CodeGraphReadRequest<'a>) -> CodeGraphReadFuture<'a> {
        Box::pin(async move {
            if request.context.scope() != &self.scope {
                return Err(CodeGraphReadError::Denied);
            }
            if request.cancellation.is_cancelled() {
                return Err(CodeGraphReadError::Cancelled);
            }
            match request.context.admission_at(request.observed_at) {
                RequestAdmission::Admitted => {}
                RequestAdmission::Cancelled => return Err(CodeGraphReadError::Cancelled),
                RequestAdmission::TimedOut => return Err(CodeGraphReadError::TimedOut),
            }
            VerifiedCodeGraphRead::new(self.scope.clone(), Arc::clone(&self.store))
        })
    }
}

#[derive(Clone)]
struct FixtureCursor {
    snapshot: TemporalExecutionSnapshot,
    source_generation: CodeGenerationId,
}

impl SymbolGraphCursorPort for FixtureCursor {
    fn claim_page<'a>(
        &'a self,
        _context: &'a RequestContext,
        _lane: &'a str,
        _cursor: Option<&'a tracedecay_application::OpaqueCursor>,
        _observed_at: UtcMicros,
    ) -> SymbolGraphCursorFuture<'a, SymbolGraphPageClaim> {
        Box::pin(async move {
            Ok(SymbolGraphPageClaim::new(
                self.snapshot.clone(),
                self.source_generation.clone(),
                0,
            ))
        })
    }

    fn finish_page<'a>(
        &'a self,
        _context: &'a RequestContext,
        _lane: &'a str,
        _claim: &'a SymbolGraphPageClaim,
        _next_offset: usize,
        _total: usize,
        _has_more: bool,
        _observed_at: UtcMicros,
    ) -> SymbolGraphCursorFuture<'a, Option<tracedecay_application::OpaqueCursor>> {
        Box::pin(async { Ok(None) })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RecordedAdmission {
    context: RequestContext,
    source_generation: CodeGenerationId,
    imports: Vec<CodeIndexImportEvidenceV1>,
}

#[derive(Clone)]
struct RecordingIgnoredDependencyAdmission {
    calls: Arc<Mutex<Vec<RecordedAdmission>>>,
    response: Result<CodeGenerationId, CodeIndexIgnoredDependencyAdmissionErrorV1>,
}

impl RecordingIgnoredDependencyAdmission {
    fn new(response: Result<CodeGenerationId, CodeIndexIgnoredDependencyAdmissionErrorV1>) -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            response,
        }
    }

    fn calls(&self) -> Vec<RecordedAdmission> {
        self.calls.lock().expect("recording lock").clone()
    }
}

impl CodeIndexIgnoredDependencyAdmissionPortV1 for RecordingIgnoredDependencyAdmission {
    fn admit<'a>(
        &'a self,
        request: CodeIndexIgnoredDependencyAdmissionRequestV1<'a>,
    ) -> CodeIndexIgnoredDependencyAdmissionFutureV1<'a> {
        Box::pin(async move {
            self.calls
                .lock()
                .expect("recording lock")
                .push(RecordedAdmission {
                    context: request.context().clone(),
                    source_generation: request.source_generation().clone(),
                    imports: request.imports().to_vec(),
                });
            self.response.clone()
        })
    }
}

#[tokio::test]
async fn exact_symbol_generation_advance_returns_stale_retry_without_same_call_symbol() {
    let fixture = fixture();
    let scheduler = Arc::new(RecordingIgnoredDependencyAdmission::new(Ok(
        next_generation(),
    )));
    let adapter = adapter(&fixture, Some(scheduler.clone()));

    let outcome = adapter
        .exact_symbol(
            port_context(&fixture),
            &exact_request("ExternalWidget", true, Some("src/client")),
        )
        .await;

    assert_failure(
        outcome,
        PrimitiveFailureKind::Stale,
        "application.symbol-graph.ignored-dependency-generation-advanced",
    );
    let calls = scheduler.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].context, fixture.context);
    assert_eq!(calls[0].source_generation, fixture.generation);
    assert_eq!(calls[0].imports, vec![fixture.expected_import.clone()]);
}

#[tokio::test]
async fn exact_symbol_absent_scheduler_fails_closed_without_legacy_support_gap() {
    let fixture = fixture();
    let adapter = adapter(&fixture, None);

    let outcome = adapter
        .exact_symbol(
            port_context(&fixture),
            &exact_request("ExternalWidget", true, Some("src/client")),
        )
        .await;

    assert_failure(
        outcome,
        PrimitiveFailureKind::Unavailable,
        "application.symbol-graph.ignored-dependency-scheduler-unavailable",
    );
}

#[tokio::test]
async fn exact_symbol_scheduler_failures_remain_typed_and_never_become_support_gaps() {
    for (error, expected_kind, expected_code) in scheduler_error_cases() {
        let fixture = fixture();
        let scheduler = Arc::new(RecordingIgnoredDependencyAdmission::new(Err(error)));
        let outcome = adapter(&fixture, Some(scheduler.clone()))
            .exact_symbol(
                port_context(&fixture),
                &exact_request("ExternalWidget", true, Some("src/client")),
            )
            .await;

        assert_failure(outcome, expected_kind, expected_code);
        assert_eq!(scheduler.calls().len(), 1);
    }
}

#[tokio::test]
async fn exact_symbol_never_schedules_without_opt_in_or_after_a_positive_exact_match() {
    let fixture = fixture();
    let scheduler = Arc::new(RecordingIgnoredDependencyAdmission::new(Ok(
        next_generation(),
    )));
    let adapter = adapter(&fixture, Some(scheduler.clone()));

    let empty = adapter
        .exact_symbol(
            port_context(&fixture),
            &exact_request("ExternalWidget", false, Some("src/client")),
        )
        .await;
    assert_completed_names(empty, &[]);

    let positive = adapter
        .exact_symbol(
            port_context(&fixture),
            &exact_request("Widget", true, Some("src/client")),
        )
        .await;
    assert_completed_names(positive, &["Widget"]);
    assert!(scheduler.calls().is_empty());
}

#[tokio::test]
async fn symbol_search_opt_in_reuses_the_exact_symbol_scheduler_boundary() {
    let fixture = fixture();
    let scheduler = Arc::new(RecordingIgnoredDependencyAdmission::new(Ok(
        next_generation(),
    )));
    let adapter = adapter(&fixture, Some(scheduler.clone()));
    let request = search_request("ExternalWidget", true, Some("src/client"));

    let outcome = adapter
        .symbol_search(port_context(&fixture), &request)
        .await;

    assert_failure(
        outcome,
        PrimitiveFailureKind::Stale,
        "application.symbol-graph.ignored-dependency-generation-advanced",
    );
    let calls = scheduler.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].context, fixture.context);
    assert_eq!(calls[0].source_generation, fixture.generation);
    assert_eq!(calls[0].imports, vec![fixture.expected_import.clone()]);
}

#[tokio::test]
async fn symbol_search_absent_scheduler_fails_closed_without_legacy_support_gap() {
    let fixture = fixture();
    let adapter = adapter(&fixture, None);
    let request = search_request("ExternalWidget", true, Some("src/client"));

    let outcome = adapter
        .symbol_search(port_context(&fixture), &request)
        .await;

    assert_failure(
        outcome,
        PrimitiveFailureKind::Unavailable,
        "application.symbol-graph.ignored-dependency-scheduler-unavailable",
    );
}

#[tokio::test]
async fn symbol_search_scheduler_failures_remain_typed_and_never_become_support_gaps() {
    for (error, expected_kind, expected_code) in scheduler_error_cases() {
        let fixture = fixture();
        let scheduler = Arc::new(RecordingIgnoredDependencyAdmission::new(Err(error)));
        let request = search_request("ExternalWidget", true, Some("src/client"));

        let outcome = adapter(&fixture, Some(scheduler.clone()))
            .symbol_search(port_context(&fixture), &request)
            .await;

        assert_failure(outcome, expected_kind, expected_code);
        assert_eq!(scheduler.calls().len(), 1);
    }
}

#[tokio::test]
async fn symbol_search_never_schedules_without_opt_in_or_after_a_positive_match() {
    let fixture = fixture();
    let scheduler = Arc::new(RecordingIgnoredDependencyAdmission::new(Ok(
        next_generation(),
    )));
    let adapter = adapter(&fixture, Some(scheduler.clone()));
    let no_opt_in = search_request("ExternalWidget", false, Some("src/client"));
    let positive = search_request("Widget", true, Some("src/client"));

    let empty = adapter
        .symbol_search(port_context(&fixture), &no_opt_in)
        .await;
    assert_completed_names(empty, &[]);

    let matched = adapter
        .symbol_search(port_context(&fixture), &positive)
        .await;
    assert_completed_names(matched, &["Widget"]);
    assert!(scheduler.calls().is_empty());
}

struct Fixture {
    scope: ResolvedScope,
    context: RequestContext,
    operation: ApplicationOperation,
    generation: CodeGenerationId,
    graph: Arc<dyn CodeGraphProjectionReadPort>,
    cursor: FixtureCursor,
    expected_import: CodeIndexImportEvidenceV1,
}

fn fixture() -> Fixture {
    let (scope, context, operation) = application_context();
    let generation = current_generation();
    let (store, expected_import) = projection_fixture(&generation);
    let graph: Arc<dyn CodeGraphProjectionReadPort> = Arc::new(FixtureCodeGraphProjection {
        scope: scope.clone(),
        store,
    });
    let cursor = FixtureCursor {
        snapshot: cursor_snapshot(&scope, &context),
        source_generation: generation.clone(),
    };
    Fixture {
        scope,
        context,
        operation,
        generation,
        graph,
        cursor,
        expected_import,
    }
}

fn adapter(
    fixture: &Fixture,
    scheduler: Option<Arc<RecordingIgnoredDependencyAdmission>>,
) -> CanonicalSymbolGraphAdapter<FixtureCursor> {
    let scheduler = scheduler.map(|scheduler| {
        let port: Arc<dyn CodeIndexIgnoredDependencyAdmissionPortV1> = scheduler;
        port
    });
    CanonicalSymbolGraphAdapter::new(fixture.graph.clone(), fixture.cursor.clone(), scheduler)
}

fn port_context(fixture: &Fixture) -> SymbolGraphPortContext<'_> {
    SymbolGraphPortContext {
        request: &fixture.context,
        operation: &fixture.operation,
        observed_at: NOW,
    }
}

fn exact_request(name: &str, lazy: bool, path_prefix: Option<&str>) -> ExactSymbolRequest {
    ExactSymbolRequest {
        name: name.to_owned(),
        scope: SymbolGraphScope {
            path_prefix: path_prefix.map(str::to_owned),
        },
        lazy_index_ignored_dependencies: lazy,
        meta: request_meta(),
    }
}

fn search_request(
    query: &str,
    lazy: bool,
    path_prefix: Option<&str>,
) -> SymbolSearchPrimitiveRequest {
    SymbolSearchPrimitiveRequest {
        query: sanitized_query(query),
        scope: SymbolGraphScope {
            path_prefix: path_prefix.map(str::to_owned),
        },
        lazy_index_ignored_dependencies: lazy,
        meta: request_meta(),
    }
}

fn scheduler_error_cases() -> Vec<(
    CodeIndexIgnoredDependencyAdmissionErrorV1,
    PrimitiveFailureKind,
    &'static str,
)> {
    vec![
        (
            CodeIndexIgnoredDependencyAdmissionErrorV1::Unavailable {
                detail: "scheduler offline".to_owned(),
            },
            PrimitiveFailureKind::Unavailable,
            "application.symbol-graph.ignored-dependency-scheduler-unavailable",
        ),
        (
            CodeIndexIgnoredDependencyAdmissionErrorV1::ReadOnly,
            PrimitiveFailureKind::Unavailable,
            "application.symbol-graph.ignored-dependency-read-only",
        ),
        (
            CodeIndexIgnoredDependencyAdmissionErrorV1::Cancelled,
            PrimitiveFailureKind::Unavailable,
            "application.symbol-graph.ignored-dependency-cancelled",
        ),
        (
            CodeIndexIgnoredDependencyAdmissionErrorV1::TimedOut,
            PrimitiveFailureKind::Unavailable,
            "application.symbol-graph.ignored-dependency-timed-out",
        ),
        (
            CodeIndexIgnoredDependencyAdmissionErrorV1::Stale {
                active_generation: next_generation(),
            },
            PrimitiveFailureKind::Stale,
            "application.symbol-graph.ignored-dependency-generation-stale",
        ),
    ]
}

fn request_meta() -> RetrievalRequestMeta {
    RetrievalRequestMeta::current(
        PageRequest::first(20).expect("page"),
        ResultProjection::Evidence,
        RetrievalOrder::StableIdentity,
    )
}

fn sanitized_query(query: &str) -> EphemeralSanitizedQueryViewV1 {
    EphemeralSanitizedQueryViewV1::sanitize(
        query.to_owned(),
        SanitizerRevision::new("sanitizer.symbol-graph-test").expect("sanitizer"),
        QueryNormalizationRevision::new("normalization.symbol-graph-test").expect("normalization"),
    )
    .expect("query")
}

fn assert_failure<T>(
    outcome: SymbolGraphPortOutcome<T>,
    expected_kind: PrimitiveFailureKind,
    expected_code: &str,
) {
    let SymbolGraphPortOutcome::Failed { failure, .. } = outcome else {
        panic!("lazy indexing must fail with a typed retry outcome, never a support gap")
    };
    assert_eq!(failure.kind, expected_kind);
    assert_eq!(failure.code, expected_code);
    assert_ne!(failure.code, "ignored-dependency-lazy-index");
}

fn assert_completed_names(
    outcome: SymbolGraphPortOutcome<tracedecay_application::retrieval::SymbolPrimitiveRecord>,
    expected: &[&str],
) {
    let SymbolGraphPortOutcome::Completed { page, .. } = outcome else {
        panic!("ordinary exact lookup must remain complete")
    };
    assert!(page.support_gaps.is_empty());
    assert_eq!(
        page.items
            .iter()
            .map(|item| item.name.as_str())
            .collect::<Vec<_>>(),
        expected
    );
}

fn current_generation() -> CodeGenerationId {
    CodeGenerationId::new("generation.symbol-graph.ignored-dependency.1").expect("generation")
}

fn next_generation() -> CodeGenerationId {
    CodeGenerationId::new("generation.symbol-graph.ignored-dependency.2").expect("generation")
}

fn application_context() -> (ResolvedScope, RequestContext, ApplicationOperation) {
    let scope = ResolvedScope::new(
        ProjectId::new("project.symbol-graph-ignored-dependency").expect("project"),
        RepositoryId::new("repository.symbol-graph-ignored-dependency").expect("repository"),
        WorktreeId::new("worktree.symbol-graph-ignored-dependency").expect("worktree"),
        Some(RefId::new("refs/heads/symbol-graph-ignored-dependency").expect("reference")),
    )
    .expect("scope");
    let capability =
        CapabilityId::new("capability.symbol-graph-ignored-dependency").expect("capability");
    let use_case = UseCaseId::new("use-case.symbol-graph-ignored-dependency").expect("use case");
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new("grant.symbol-graph-ignored-dependency").expect("grant"),
        1,
        digest::<ManifestDigest>('a'),
        ActorId::new("actor.symbol-graph-issuer").expect("issuer"),
        UtcMicros(1),
        UtcMicros(10_000),
        scope.clone(),
        BTreeSet::from([capability.clone()]),
        BTreeSet::from([use_case.clone()]),
        DisclosureClass::Evidence,
    )
    .expect("grant");
    let context = RequestContext::new(
        ActorId::new("actor.symbol-graph-requester").expect("actor"),
        scope.clone(),
        grant,
        RequestId::new("request.symbol-graph-ignored-dependency").expect("request"),
        Deadline::new(UtcMicros(9_000)).expect("deadline"),
        CancellationContext::active("cancel.symbol-graph-ignored-dependency")
            .expect("cancellation"),
    )
    .expect("context");
    let operation = ApplicationOperation::new(
        capability,
        use_case,
        ResultContractRef::new(
            SchemaId::new("schema.symbol-graph-ignored-dependency").expect("schema"),
            1,
        )
        .expect("result contract"),
        true,
    );
    (scope, context, operation)
}

fn cursor_snapshot(scope: &ResolvedScope, context: &RequestContext) -> TemporalExecutionSnapshot {
    let request = TemporalSnapshotRequest::new(
        SessionId::new("session.symbol-graph-ignored-dependency").expect("session"),
        scope.scope_digest.as_str(),
        format!("sha256:{}", "b".repeat(64)),
        context.grant().digest.as_str(),
        TemporalModeV1::Current,
        RetrievalGrainV1::Occurrence,
    )
    .expect("snapshot request");
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
                format!("sha256:{}", "c".repeat(64)),
            )
            .expect("configuration digest"),
        },
        None,
        ValidatedAuthorization::Authorized,
    )
    .expect("snapshot")
}

fn projection_fixture(
    generation: &CodeGenerationId,
) -> (Arc<CodeGraphProjectionStore>, CodeIndexImportEvidenceV1) {
    let (manifest, expected) = projection_manifest(generation);
    (store_for_manifest(manifest, generation), expected)
}

fn projection_manifest(
    generation: &CodeGenerationId,
) -> (GraphGenerationManifest, CodeIndexImportEvidenceV1) {
    let revision = GraphProjectorRevision::try_from(CODE_GRAPH_PROJECTOR_REVISION.to_owned())
        .expect("projector revision");
    let projection =
        code_graph_projection_identity(GraphNamespace::new("code-graph").expect("graph namespace"))
            .expect("projection");
    let mut manifest = build_code_graph_manifest(
        projection.clone(),
        generation,
        &[],
        &[],
        &revision,
        Arc::new(NeverCancelled),
    )
    .expect("base manifest");

    let client_file = file("file.client", "src/client/app.ts");
    let vendor_file = file("file.vendor", "aaa/generated.ts");
    let expected = import(
        &client_file,
        "external-widget",
        "ExternalWidget",
        "type",
        "bare_module",
        10,
    );
    let imports = vec![
        expected.clone(),
        import(
            &client_file,
            "external-widget-secondary",
            "ExternalWidget",
            "type",
            "bare_module",
            15,
        ),
        import(
            &client_file,
            "external-widget",
            "ExternalWidget",
            "value",
            "bare_module",
            20,
        ),
        import(
            &client_file,
            "./ExternalWidget",
            "ExternalWidget",
            "type",
            "project_relative",
            30,
        ),
        import(
            &vendor_file,
            "external-widget",
            "ExternalWidget",
            "type",
            "bare_module",
            40,
        ),
    ];

    manifest.entities.push(file_entity(&client_file));
    manifest.entities.push(file_entity(&vendor_file));
    for row in &imports {
        manifest.entities.push(import_entity(row));
        manifest
            .relations
            .push(file_import_relation(&projection, row));
    }
    manifest.entities.push(symbol_entity(&client_file));
    let projection_node_count = manifest.entities.len();
    let current = manifest
        .entities
        .iter_mut()
        .find(|entity| entity.identity.as_str() == "code-current-generation")
        .expect("current generation entity");
    current.properties.insert(
        GraphPropertyName::new("projection-node-count").expect("property"),
        GraphProperty::String(projection_node_count.to_string()),
    );

    let manifest = GraphGenerationManifest::new(
        manifest.projection,
        manifest.generation,
        manifest.source_generation,
        manifest.watermark,
        manifest.dependencies,
        manifest.entities,
        manifest.relations,
    )
    .expect("fixture manifest");
    (manifest, expected)
}

fn store_for_manifest(
    manifest: GraphGenerationManifest,
    generation: &CodeGenerationId,
) -> Arc<CodeGraphProjectionStore> {
    let snapshot =
        VerifiedGraphSnapshot::memory(manifest, Arc::new(NeverCancelled)).expect("snapshot");
    let store = CodeGraphProjectionStore::from_verified_snapshot(snapshot, generation.clone())
        .expect("verified store");
    Arc::new(store)
}

fn file(occurrence: &str, logical_path: &str) -> SanitizedCodeFileV1 {
    SanitizedCodeFileV1 {
        file_occurrence_id: FileOccurrenceId::try_from(occurrence.to_owned()).expect("file"),
        logical_path: logical_path.to_owned(),
        language: Some(LanguageId::new("typescript").expect("language")),
        content_digest: digest::<ContentDigest>('d'),
        disposition: SnapshotFileDispositionV1::Present,
    }
}

fn import(
    file: &SanitizedCodeFileV1,
    module_specifier: &str,
    imported_name: &str,
    namespace: &str,
    module_kind: &str,
    start_byte: u64,
) -> CodeIndexImportEvidenceV1 {
    serde_json::from_value(serde_json::json!({
        "logical_path": file.logical_path,
        "file_occurrence_id": file.file_occurrence_id,
        "module_specifier": module_specifier,
        "imported_name": imported_name,
        "local_name": imported_name,
        "namespace": namespace,
        "module_kind": module_kind,
        "span": { "start_byte": start_byte, "end_byte": start_byte + 1 },
        "start_line": start_byte,
        "start_column": 0,
    }))
    .expect("import evidence")
}

fn file_entity(file: &SanitizedCodeFileV1) -> GraphEntity {
    GraphEntity::new(
        GraphEntityId::new(stable_identity("file", file.file_occurrence_id.as_str()))
            .expect("file entity id"),
        BTreeSet::from([GraphLabel::new("CodeFile").expect("file label")]),
        BTreeMap::from([(
            GraphPropertyName::new("file-record").expect("file property"),
            GraphProperty::Bytes(serde_json::to_vec(file).expect("file record")),
        )]),
    )
    .expect("file entity")
}

fn import_entity(import: &CodeIndexImportEvidenceV1) -> GraphEntity {
    GraphEntity::new(
        import_entity_id(import),
        BTreeSet::from([GraphLabel::new("CodeImport").expect("import label")]),
        BTreeMap::from([(
            GraphPropertyName::new("import-record").expect("import property"),
            GraphProperty::Bytes(serde_json::to_vec(import).expect("import record")),
        )]),
    )
    .expect("import entity")
}

fn file_import_relation(
    projection: &tracedecay_graph_db::GraphProjectionIdentity,
    import: &CodeIndexImportEvidenceV1,
) -> GraphGenerationRelation {
    let import_id = import_entity_id(import);
    GraphGenerationRelation::new(
        GraphRelationId::new(stable_identity(
            "file-import",
            &format!(
                "{}\0{}",
                import.file_occurrence_id.as_str(),
                import_id.as_str()
            ),
        ))
        .expect("relation id"),
        GraphEntityRef::new(
            projection.clone(),
            GraphEntityId::new(stable_identity("file", import.file_occurrence_id.as_str()))
                .expect("file entity id"),
        ),
        GraphEntityRef::new(projection.clone(), import_id),
        GraphRelationKind::new("CodeFileContainsImport").expect("relation kind"),
        BTreeMap::new(),
    )
    .expect("file import relation")
}

fn import_entity_id(import: &CodeIndexImportEvidenceV1) -> GraphEntityId {
    GraphEntityId::new(stable_identity(
        "import",
        &hex::encode(serde_json::to_vec(import).expect("import record")),
    ))
    .expect("import entity id")
}

#[derive(Serialize)]
struct SymbolRecordFixture {
    occurrence: SymbolOccurrenceId,
    binding: Option<CodeGraphSymbolBindingV1>,
    metadata: Option<LineageSymbolRecordV1>,
}

fn symbol_entity(file: &SanitizedCodeFileV1) -> GraphEntity {
    let occurrence = SymbolOccurrenceId::new("symbol.fixture.Widget").expect("symbol");
    let record = SymbolRecordFixture {
        occurrence: occurrence.clone(),
        binding: Some(CodeGraphSymbolBindingV1 {
            file: file.file_occurrence_id.clone(),
            logical_path: Some(file.logical_path.clone()),
            source_span: Some(SourceSpan {
                start_byte: 0,
                end_byte: 6,
            }),
            chunk: None,
            language_descriptor_revision: LanguageDescriptorRevision::new("language.typescript.v1")
                .expect("language revision"),
        }),
        metadata: Some(LineageSymbolRecordV1 {
            occurrence: occurrence.clone(),
            identity: digest::<SymbolIdentityDigest>('e'),
            qualified_name: "client::Widget".to_owned(),
            simple_name: "Widget".to_owned(),
            kind: "struct".to_owned(),
            visibility: "public".to_owned(),
            branches: 0,
            loops: 0,
            max_nesting: 0,
            line_span: 1,
            start_line: 0,
            signature: None,
            skip_test_coverage: false,
            file_identity: digest::<FileIdentityDigest>('f'),
            content_digest: digest::<ContentDigest>('1'),
        }),
    };
    GraphEntity::new(
        GraphEntityId::new(stable_identity("symbol", occurrence.as_str()))
            .expect("symbol entity id"),
        BTreeSet::from([GraphLabel::new("CodeSymbol").expect("symbol label")]),
        BTreeMap::from([(
            GraphPropertyName::new("symbol-record").expect("symbol property"),
            GraphProperty::Bytes(serde_json::to_vec(&record).expect("symbol record")),
        )]),
    )
    .expect("symbol entity")
}

fn stable_identity(kind: &str, value: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(kind.as_bytes());
    digest.update([0]);
    digest.update(value.as_bytes());
    format!("{kind}:{}", hex::encode(digest.finalize()))
}

fn digest<T>(byte: char) -> T
where
    T: TryFrom<String>,
    <T as TryFrom<String>>::Error: std::fmt::Debug,
{
    T::try_from(format!("sha256:{}", byte.to_string().repeat(64))).expect("digest")
}
