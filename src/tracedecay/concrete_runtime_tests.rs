use std::collections::BTreeSet;
use std::fs;
use std::sync::Arc;

use tempfile::TempDir;
use tracedecay_application::retrieval::{
    RetrievalOrder, RetrievalRequestMeta, SourceReadModeV1, SourceReadPortContext,
    SourceReadPortOutcome, SourceReadPrimitivePort, SourceReadPrimitiveRequest,
};
use tracedecay_application::{
    ApplicationOperation, CancellationContext, CancellationSignal, CapabilityGrantId,
    CapabilityGrantSnapshot, Deadline, DisclosureClass, PageRequest, RequestAdmission,
    RequestContext, RequestId, ResolvedScope, ResultContractRef, ResultProjection,
};
use tracedecay_domain::{
    ActorId, CodeGenerationId, ContentDigest, FileOccurrenceId, LanguageId, ManifestDigest,
    ProjectId, RefId, RepositoryId, SanitizedCodeFileV1, SnapshotFileDispositionV1, UtcMicros,
    WorktreeId,
};
use tracedecay_graph_db::NeverCancelled;
use tracedecay_tool_catalog::{CapabilityId, SchemaId, UseCaseId};
use tracedecay_usecases::graph::{
    CodeGraphProjectionReadPort, CodeGraphReadError, CodeGraphReadFuture, CodeGraphReadRequest,
    VerifiedCodeGraphRead,
};

use super::{TraceDecay, TraceDecayOpenOptions};
use tracedecay_usecases::primitives::SourceReadAdapter;

const NOW: UtcMicros = UtcMicros(1_000);

#[derive(Clone)]
struct FixtureCodeGraphProjection {
    scope: ResolvedScope,
    store: Arc<tracedecay_code_index::graph_projection::CodeGraphProjectionStore>,
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

#[tokio::test]
async fn source_reads_reuse_the_cross_session_cache() {
    let root = TempDir::new().expect("temporary project");
    fs::create_dir_all(root.path().join("src")).expect("source directory");
    let source = "pub fn first() {}\npub fn second() {}\n";
    fs::write(root.path().join("src/lib.rs"), source).expect("fixture source");
    let profile_root = root.path().join(".tracedecay-test-profile");
    let open_options = TraceDecayOpenOptions {
        profile_root: Some(profile_root.clone()),
        global_db_path: Some(profile_root.join("global.db")),
    };
    let lifecycle = tracedecay_runtime_core::lifecycle_lease::acquire_exclusive_for_profile(
        &profile_root,
        "source-read cache test",
    )
    .expect("exclusive lifecycle authority");
    let _database_scope = tracedecay_runtime_core::db::enter_maintenance_database_scope(
        &lifecycle,
        &profile_root,
        "source-read cache test",
    )
    .expect("maintenance database authority");
    let graph = Arc::new(
        TraceDecay::init_with_exclusive_maintenance(root.path(), open_options, &lifecycle)
            .await
            .expect("initialize graph"),
    );
    let (scope, context, operation) = application_context("source-read");
    let generation = CodeGenerationId::new("generation.source-read-cache.1").unwrap();
    let cancellation = CancellationSignal::active("cancel.source-read-cache-fixture").unwrap();
    let store = tracedecay_code_index::graph_projection::HermeticCodeGraphProjectionStore::memory(
        &cancellation,
    )
    .expect("hermetic graph projection");
    let files = [SanitizedCodeFileV1 {
        file_occurrence_id: FileOccurrenceId::try_from("file:src/lib.rs".to_owned()).unwrap(),
        logical_path: "src/lib.rs".to_owned(),
        language: Some(LanguageId::new("rust").unwrap()),
        content_digest: ContentDigest::new(format!("sha256:{}", "a".repeat(64))).unwrap(),
        disposition: SnapshotFileDispositionV1::Present,
    }];
    let symbols = tracedecay_code_index::lineage::GenerationSymbolIndexV1::new(
        generation.clone(),
        Vec::new(),
    )
    .expect("empty symbol index");
    store
        .publish_indexed_with_cancellation(
            &generation,
            &[],
            &[],
            &files,
            &symbols,
            Arc::new(NeverCancelled),
        )
        .expect("publish source-read fixture generation");
    let code_graph = Arc::new(FixtureCodeGraphProjection {
        scope: scope.clone(),
        store: Arc::new(
            store
                .verified_store(&generation)
                .expect("verified source-read fixture generation"),
        ),
    });
    let adapter = SourceReadAdapter::new(graph, code_graph, scope).expect("source adapter");
    let request = SourceReadPrimitiveRequest {
        file: "src/lib.rs".to_owned(),
        mode: SourceReadModeV1::Lines,
        lines: Some("2-2".to_owned()),
        include_symbols: false,
        meta: RetrievalRequestMeta::current(
            PageRequest::first(1).expect("page"),
            ResultProjection::Evidence,
            RetrievalOrder::SourcePosition,
        ),
    };

    let first = adapter
        .source_read(
            SourceReadPortContext {
                request: &context,
                operation: &operation,
                observed_at: NOW,
            },
            &request,
        )
        .await;
    let second = adapter
        .source_read(
            SourceReadPortContext {
                request: &context,
                operation: &operation,
                observed_at: NOW,
            },
            &request,
        )
        .await;

    let SourceReadPortOutcome::Completed { result: first, .. } = first else {
        panic!("first source read must complete");
    };
    let SourceReadPortOutcome::Completed { result: second, .. } = second else {
        panic!("cached source read must complete");
    };
    assert_eq!(first.body.as_deref(), Some("pub fn second() {}"));
    assert!(!first.unchanged);
    assert!(second.unchanged);
    assert!(second.body.is_none());
    assert_eq!(second.digest, first.digest);

    let invalid = SourceReadPrimitiveRequest {
        mode: SourceReadModeV1::Full,
        lines: Some("1-1".to_owned()),
        ..request
    };
    let outcome = adapter
        .source_read(
            SourceReadPortContext {
                request: &context,
                operation: &operation,
                observed_at: NOW,
            },
            &invalid,
        )
        .await;
    assert!(
        matches!(outcome, SourceReadPortOutcome::Failed { .. }),
        "production source reads must reject mode/range mismatches"
    );
}

fn application_context(suffix: &str) -> (ResolvedScope, RequestContext, ApplicationOperation) {
    let scope = ResolvedScope::new(
        ProjectId::new("project.retrieval-primitives").expect("project"),
        RepositoryId::new("repository.retrieval-primitives").expect("repository"),
        WorktreeId::new("worktree.retrieval-primitives").expect("worktree"),
        Some(RefId::new("refs/heads/retrieval-primitives").expect("reference")),
    )
    .expect("scope");
    let capability =
        CapabilityId::new(format!("capability.retrieval.{suffix}")).expect("capability");
    let use_case = UseCaseId::new(format!("use-case.retrieval.{suffix}")).expect("use case");
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new(format!("grant.retrieval.{suffix}")).expect("grant id"),
        1,
        ManifestDigest::new(format!("sha256:{}", "a".repeat(64))).expect("grant digest"),
        ActorId::new("actor.retrieval.issuer").expect("issuer"),
        UtcMicros(1),
        UtcMicros(10_000),
        scope.clone(),
        BTreeSet::from([capability.clone()]),
        BTreeSet::from([use_case.clone()]),
        DisclosureClass::Evidence,
    )
    .expect("grant");
    let context = RequestContext::new(
        ActorId::new("actor.retrieval.requester").expect("actor"),
        scope.clone(),
        grant,
        RequestId::new(format!("request.retrieval.{suffix}")).expect("request id"),
        Deadline::new(UtcMicros(10_000)).expect("deadline"),
        CancellationContext::active(format!("cancel.retrieval.{suffix}")).expect("cancellation"),
    )
    .expect("request context");
    let operation = ApplicationOperation::new(
        capability,
        use_case,
        ResultContractRef::new(
            SchemaId::new(format!("schema.retrieval.{suffix}")).expect("schema"),
            1,
        )
        .expect("result contract"),
        true,
    );
    (scope, context, operation)
}
