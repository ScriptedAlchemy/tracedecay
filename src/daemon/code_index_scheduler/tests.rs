use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use tempfile::TempDir;
use tracedecay_application::{
    CallableCodeOperationKind, CallableCodeQueryPort, CancellationContext, CapabilityGrantSnapshot,
    CodeQueryScope, CodeRelationRequest, Deadline, DisclosureClass, ExactOccurrenceRequest,
    PageRequest, PhraseSearchRequest, RequestContext, RequestId, ResolvedScope, ResultProjection,
    RetrievalOrder, RetrievalPortContext, RetrievalPortOutcome, RetrievalRequestMeta,
    callable_code_operation,
};
use tracedecay_domain::{
    ActorId, EphemeralSanitizedQueryViewV1, ManifestDigest, ProjectId, QueryNormalizationRevision,
    RefId, RepositoryId, SanitizerRevision, UtcMicros, WorktreeId,
};

#[cfg(feature = "semantic-fastembed")]
use crate::application::semantic_runtime::{ProductionSemanticRuntimeV1, current_query_factory};
#[cfg(feature = "semantic-fastembed")]
use crate::config::SemanticResourceCeilings;
#[cfg(feature = "semantic-fastembed")]
use crate::db::{Database, DatabaseAuthority};
#[cfg(feature = "semantic-fastembed")]
use crate::semantic_code::{
    CatalogedFastEmbedModelV1, DaemonSemanticRuntimeHandleV1, FastEmbedModelCatalogV1,
    ModelLifecycleErrorV1, ModelMemberSourceV1, SemanticModelLifecycleOwnerV1,
    production_fastembed_catalog,
};
#[cfg(feature = "semantic-fastembed")]
use crate::store::vector_generations::DatabaseVectorGenerationStoreV1;

use super::{
    CodeIndexReconcileOutcomeV1, CodeIndexSchedulerRegistryV1, CodeIndexWorktreeSchedulerV1,
    SharedCodeIndexBytePoolV1,
};

struct GitFixture {
    root: TempDir,
}

impl GitFixture {
    fn new(files: &[(&str, &str)]) -> Self {
        let root = TempDir::new().expect("fixture root");
        git(root.path(), &["init", "-q"]);
        git(root.path(), &["config", "user.name", "TraceDecay Test"]);
        git(
            root.path(),
            &["config", "user.email", "tracedecay@example.invalid"],
        );
        for (path, source) in files {
            write(root.path(), path, source);
        }
        git(root.path(), &["add", "."]);
        git(root.path(), &["commit", "-qm", "fixture"]);
        Self { root }
    }

    fn path(&self) -> &Path {
        self.root.path()
    }

    fn edit(&self, path: &str, source: &str) {
        write(self.path(), path, source);
    }
}

fn git(root: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(root)
        .args(args)
        .status()
        .expect("run git fixture command");
    assert!(status.success(), "git fixture command failed: {args:?}");
}

fn write(root: &Path, path: &str, source: &str) {
    let path = root.join(path);
    std::fs::create_dir_all(path.parent().expect("source parent")).expect("create source parent");
    std::fs::write(path, source).expect("write fixture source");
}

fn scheduler(
    fixture: &GitFixture,
    store_root: PathBuf,
    bytes: Arc<SharedCodeIndexBytePoolV1>,
) -> CodeIndexWorktreeSchedulerV1 {
    CodeIndexWorktreeSchedulerV1::open(fixture.path(), store_root, bytes)
        .expect("open worktree scheduler")
}

fn published(outcome: CodeIndexReconcileOutcomeV1) -> super::CodeIndexPublishEvidenceV1 {
    match outcome {
        CodeIndexReconcileOutcomeV1::Published(evidence) => evidence,
        CodeIndexReconcileOutcomeV1::Noop(_) => panic!("expected a published generation"),
    }
}

fn application_context(
    operation: &tracedecay_application::ApplicationOperation,
    repository: RepositoryId,
    worktree: WorktreeId,
) -> RequestContext {
    let scope = ResolvedScope::new(
        ProjectId::new("project.code-index.fixture").expect("project id"),
        repository,
        worktree,
        Some(RefId::new("refs/heads/main").expect("ref id")),
    )
    .expect("resolved scope");
    let grant = CapabilityGrantSnapshot::new(
        tracedecay_application::CapabilityGrantId::new("grant.code-index.fixture")
            .expect("grant id"),
        1,
        ManifestDigest::new(format!("sha256:{}", "a".repeat(64))).expect("grant digest"),
        ActorId::new("actor.code-index.issuer").expect("issuer"),
        UtcMicros(1),
        UtcMicros(i64::MAX),
        scope.clone(),
        BTreeSet::from([operation.capability_id().clone()]),
        BTreeSet::from([operation.use_case_id().clone()]),
        DisclosureClass::Evidence,
    )
    .expect("grant");
    RequestContext::new(
        ActorId::new("actor.code-index.requester").expect("actor"),
        scope,
        grant,
        RequestId::new("request.code-index.fixture").expect("request id"),
        Deadline::new(UtcMicros(i64::MAX)).expect("deadline"),
        CancellationContext::active("cancel.code-index.fixture").expect("cancellation"),
    )
    .expect("request context")
}

fn query_meta() -> RetrievalRequestMeta {
    RetrievalRequestMeta::current(
        PageRequest::first(16).expect("page"),
        ResultProjection::Evidence,
        RetrievalOrder::Relevance,
    )
}

#[test]
fn saved_edit_incremental_publish() {
    let fixture = GitFixture::new(&[(
        "src/lib.rs",
        "pub fn alpha() -> u32 { 1 }\npub fn beta() -> u32 { 2 }\n",
    )]);
    let store = TempDir::new().expect("store root");
    let bytes = Arc::new(SharedCodeIndexBytePoolV1::default());
    let mut scheduler = scheduler(&fixture, store.path().to_path_buf(), bytes);

    let first = published(scheduler.reconcile_now().expect("initial publish"));
    fixture.edit(
        "src/lib.rs",
        "pub fn alpha() -> u32 { 10 }\npub fn beta() -> u32 { 2 }\n",
    );
    scheduler.notify_path(fixture.path().join("src/lib.rs"));
    let second = published(scheduler.reconcile_now().expect("incremental publish"));

    assert_ne!(first.generation_id, second.generation_id);
    assert_eq!(second.incremental_parse_files, 1);
    assert!(second.changed_ranges > 0);
    let latest = scheduler.latest_complete().expect("latest generation");
    assert!(!latest.exact().expect("exact lane").is_empty());
    assert!(!latest.lexical().is_empty());
    assert!(
        !latest.graph_edges().is_empty() || !latest.graph_abstentions().is_empty(),
        "graph lane must remain explicitly queryable"
    );
    let owners = latest
        .production_query_owners()
        .expect("production exact/lexical/graph owners connect");
    let _ = owners.exact;
    let _ = owners.lexical;
    let _ = owners.graph;
}

#[test]
fn duplicate_save_and_overflow_equals_clean_scan() {
    let fixture = GitFixture::new(&[
        ("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n"),
        ("src/other.rs", "pub fn other() -> u32 { 2 }\n"),
    ]);
    let store = TempDir::new().expect("store root");
    let bytes = Arc::new(SharedCodeIndexBytePoolV1::default());
    let mut hinted = scheduler(&fixture, store.path().join("hinted"), Arc::clone(&bytes));
    let mut clean = scheduler(&fixture, store.path().join("clean"), bytes);
    published(hinted.reconcile_now().expect("hinted baseline"));
    published(clean.reconcile_now().expect("clean baseline"));

    fixture.edit("src/lib.rs", "pub fn alpha() -> u32 { 3 }\n");
    let path = fixture.path().join("src/lib.rs");
    hinted.notify_path(path.clone());
    hinted.notify_path(path);
    hinted.notify_overflow();

    let hinted_publish = published(hinted.reconcile_now().expect("hinted reconcile"));
    let clean_publish = published(clean.reconcile_now().expect("clean reconcile"));
    assert_eq!(
        hinted_publish.snapshot_content_identity,
        clean_publish.snapshot_content_identity
    );
    assert_eq!(hinted_publish.lane_digest, clean_publish.lane_digest);
    assert!(hinted_publish.overflow_reconciled);
}

#[test]
fn cross_worktree_byte_reuse_without_identity_alias() {
    let first = GitFixture::new(&[("src/lib.rs", "pub fn shared() -> u32 { 7 }\n")]);
    let second = GitFixture::new(&[("src/lib.rs", "pub fn shared() -> u32 { 7 }\n")]);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(2);

    let mut first_scheduler = registry
        .open_worktree(first.path(), store.path().join("first"))
        .expect("first scheduler");
    let mut second_scheduler = registry
        .open_worktree(second.path(), store.path().join("second"))
        .expect("second scheduler");
    let first_publish = published(first_scheduler.reconcile_now().expect("first publish"));
    let second_publish = published(second_scheduler.reconcile_now().expect("second publish"));

    assert!(registry.byte_pool_stats().reused >= 1);
    assert_ne!(first_publish.repository_id, second_publish.repository_id);
    assert_ne!(
        first_publish.file_occurrence_ids, second_publish.file_occurrence_ids,
        "shared bytes must never alias repository/worktree occurrence identity"
    );
}

#[test]
fn one_symbol_unrelated_work_skip() {
    let fixture = GitFixture::new(&[(
        "src/lib.rs",
        "pub fn alpha() -> u32 { 1 }\n\npub fn unrelated() -> u32 { 99 }\n",
    )]);
    let store = TempDir::new().expect("store root");
    let bytes = Arc::new(SharedCodeIndexBytePoolV1::default());
    let mut scheduler = scheduler(&fixture, store.path().to_path_buf(), bytes);
    published(scheduler.reconcile_now().expect("baseline"));

    fixture.edit(
        "src/lib.rs",
        "pub fn alpha() -> u32 { 2 }\n\npub fn unrelated() -> u32 { 99 }\n",
    );
    scheduler.notify_path(fixture.path().join("src/lib.rs"));
    let changed = published(scheduler.reconcile_now().expect("one-symbol publish"));

    assert_eq!(changed.reextracted_files, 1);
    assert!(changed.changed_chunks > 0);
    assert!(
        changed.reused_chunks > 0,
        "unrelated symbol chunks must skip projection work"
    );
}

#[test]
fn content_noop_suppresses_publication() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let store = TempDir::new().expect("store root");
    let bytes = Arc::new(SharedCodeIndexBytePoolV1::default());
    let mut scheduler = scheduler(&fixture, store.path().to_path_buf(), bytes);
    let first = published(scheduler.reconcile_now().expect("baseline publish"));

    match scheduler.reconcile_now().expect("content noop") {
        CodeIndexReconcileOutcomeV1::Noop(evidence) => {
            assert_eq!(
                evidence.snapshot_content_identity, first.snapshot_content_identity,
                "unchanged content must reuse the sealed snapshot identity"
            );
        }
        CodeIndexReconcileOutcomeV1::Published(_) => {
            panic!("identical content must not publish a new generation")
        }
    }
    let _owners = scheduler
        .latest_complete()
        .expect("active generation")
        .production_query_owners()
        .expect("owners remain connected after content no-op");
}

#[test]
fn superseding_notifies_publish_only_latest_content() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let store = TempDir::new().expect("store root");
    let bytes = Arc::new(SharedCodeIndexBytePoolV1::default());
    let mut live = scheduler(&fixture, store.path().join("live"), Arc::clone(&bytes));
    let mut clean = scheduler(&fixture, store.path().join("clean"), bytes);
    published(live.reconcile_now().expect("live baseline"));
    published(clean.reconcile_now().expect("clean baseline"));

    fixture.edit("src/lib.rs", "pub fn alpha() -> u32 { 2 }\n");
    live.notify_path(fixture.path().join("src/lib.rs"));
    fixture.edit("src/lib.rs", "pub fn alpha() -> u32 { 3 }\n");
    live.notify_path(fixture.path().join("src/lib.rs"));
    live.notify_overflow();

    let superseded = published(live.reconcile_now().expect("superseded reconcile"));
    let expected = published(clean.reconcile_now().expect("clean latest reconcile"));
    assert_eq!(
        superseded.snapshot_content_identity, expected.snapshot_content_identity,
        "fair supersession must publish only the latest reconciled content"
    );
    assert_eq!(superseded.lane_digest, expected.lane_digest);
    assert!(superseded.overflow_reconciled);
}

#[test]
fn production_query_owners_bind_exact_lexical_and_graph_lanes() {
    let fixture = GitFixture::new(&[(
        "src/lib.rs",
        "pub fn caller() { callee(); }\npub fn callee() {}\n",
    )]);
    let store = TempDir::new().expect("store root");
    let bytes = Arc::new(SharedCodeIndexBytePoolV1::default());
    let mut scheduler = scheduler(&fixture, store.path().to_path_buf(), bytes);
    published(scheduler.reconcile_now().expect("publish"));
    let owners = scheduler
        .latest_complete()
        .expect("latest generation")
        .production_query_owners()
        .expect("connect production query owners");
    assert!(
        std::mem::size_of_val(&owners.exact) > 0
            && std::mem::size_of_val(&owners.lexical) > 0
            && std::mem::size_of_val(&owners.graph) > 0,
        "exact/lexical/graph production owners must be concrete lane values"
    );
}

#[test]
fn restart_restores_complete_generation_and_content_noop() {
    let fixture = GitFixture::new(&[(
        "src/lib.rs",
        "pub fn caller() { callee(); }\npub fn callee() {}\n",
    )]);
    let store = TempDir::new().expect("store root");
    let bytes = Arc::new(SharedCodeIndexBytePoolV1::default());
    let first = {
        let mut scheduler = scheduler(&fixture, store.path().to_path_buf(), Arc::clone(&bytes));
        published(scheduler.reconcile_now().expect("initial publish"))
    };

    let mut restarted = scheduler(&fixture, store.path().to_path_buf(), bytes);
    let restored = restarted
        .latest_complete()
        .expect("restart restores active generation");
    assert_eq!(
        restored.generation.manifest().generation_id,
        first.generation_id
    );
    restored
        .production_query_owners()
        .expect("restored generation reconnects all query owners");
    match restarted.reconcile_now().expect("restart reconciliation") {
        CodeIndexReconcileOutcomeV1::Noop(evidence) => {
            assert_eq!(
                evidence.snapshot_content_identity,
                first.snapshot_content_identity
            );
        }
        CodeIndexReconcileOutcomeV1::Published(_) => {
            panic!("restart with identical content must not republish")
        }
    }
}

#[test]
fn restart_rejects_corrupt_sealed_generation() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let store = TempDir::new().expect("store root");
    {
        let mut scheduler = scheduler(
            &fixture,
            store.path().to_path_buf(),
            Arc::new(SharedCodeIndexBytePoolV1::default()),
        );
        published(scheduler.reconcile_now().expect("initial publish"));
    }
    let pointer: super::DurablePublicationPointerV1 = serde_json::from_slice(
        &std::fs::read(store.path().join("active-code-generation-v1.json"))
            .expect("read active pointer"),
    )
    .expect("decode active pointer");
    let generation_path = store
        .path()
        .join("code-generations-v1")
        .join(pointer.generation_file);
    let mut bytes = std::fs::read(&generation_path).expect("read sealed generation");
    let middle = bytes.len() / 2;
    bytes[middle] ^= 0x01;
    std::fs::write(&generation_path, bytes).expect("corrupt sealed generation");

    let result = CodeIndexWorktreeSchedulerV1::open(
        fixture.path(),
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );
    assert!(
        result.is_err(),
        "corrupt sealed state must fail project open"
    );
}

#[test]
fn restart_rejects_pointer_generation_mismatch() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let store = TempDir::new().expect("store root");
    {
        let mut scheduler = scheduler(
            &fixture,
            store.path().to_path_buf(),
            Arc::new(SharedCodeIndexBytePoolV1::default()),
        );
        published(scheduler.reconcile_now().expect("initial publish"));
    }
    let pointer_path = store.path().join("active-code-generation-v1.json");
    let mut pointer: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&pointer_path).expect("read active pointer"))
            .expect("decode active pointer");
    pointer["generation_id"] = serde_json::Value::String("generation.mismatched".to_owned());
    std::fs::write(
        &pointer_path,
        serde_json::to_vec(&pointer).expect("encode mismatched pointer"),
    )
    .expect("write mismatched pointer");

    let result = CodeIndexWorktreeSchedulerV1::open(
        fixture.path(),
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );
    assert!(
        result.is_err(),
        "pointer/generation mismatch must fail project open"
    );
}

#[tokio::test]
async fn daemon_owned_per_worktree_scheduler_reconciles_saved_edits() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(1);
    assert!(
        registry
            .mount_worktree(fixture.path(), store.path().to_path_buf(), None)
            .await
            .expect("mount daemon-owned scheduler")
    );

    let first = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Some(generation) = registry.latest_generation_id(fixture.path()).await {
                break generation;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("initial generation published");

    fixture.edit("src/lib.rs", "pub fn alpha() -> u32 { 2 }\n");
    assert!(
        registry
            .notify_path(fixture.path(), fixture.path().join("src/lib.rs"))
            .await
    );
    let second = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Some(generation) = registry.latest_generation_id(fixture.path()).await
                && generation != first
            {
                break generation;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("saved edit generation published");

    assert_ne!(first, second);
    registry.shutdown().await;
}

#[cfg(feature = "semantic-fastembed")]
#[tokio::test(flavor = "multi_thread")]
async fn configured_jina_lifecycle_publishes_and_restores_semantic_generation() {
    struct PreparedJinaFixture {
        root: PathBuf,
    }

    impl ModelMemberSourceV1 for PreparedJinaFixture {
        fn fetch_member(
            &self,
            model: &CatalogedFastEmbedModelV1,
            upstream_path: &str,
            destination: &Path,
        ) -> Result<(), ModelLifecycleErrorV1> {
            let member = model
                .members
                .values()
                .find(|member| member.upstream_path == upstream_path)
                .ok_or(ModelLifecycleErrorV1::DownloadFailed)?;
            std::fs::copy(self.root.join(&member.path), destination)
                .map(|_| ())
                .map_err(|_| ModelLifecycleErrorV1::DownloadFailed)
        }
    }

    let Some(fixture_root) = std::env::var_os("TRACEDECAY_DISTRIBUTION_FASTEMBED_FIXTURE")
        .map(PathBuf::from)
        .filter(|path| path.is_dir())
    else {
        eprintln!(
            "skipping configured Jina integration; prepare fixture and set \
             TRACEDECAY_DISTRIBUTION_FASTEMBED_FIXTURE"
        );
        return;
    };

    let lifecycle_root = TempDir::new().expect("lifecycle root");
    let catalog: FastEmbedModelCatalogV1 = production_fastembed_catalog();
    let lifecycle = Arc::new(
        SemanticModelLifecycleOwnerV1::open(
            lifecycle_root.path(),
            catalog,
            Arc::new(PreparedJinaFixture { root: fixture_root }),
        )
        .expect("Jina lifecycle"),
    );
    lifecycle
        .select_model(Some(crate::config::DEFAULT_FASTEMBED_MODEL_ID), true)
        .expect("select configured Jina model");
    lifecycle
        .acquire_blocking_for_tests()
        .expect("install configured Jina fixture");

    let project = GitFixture::new(&[(
        "src/lib.rs",
        "pub fn semantic_bridge() -> &'static str { \"ready\" }\n",
    )]);
    let code_store = TempDir::new().expect("code store");
    let mut scheduler = scheduler(
        &project,
        code_store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );
    published(scheduler.reconcile_now().expect("publish code generation"));
    let latest = scheduler.latest_complete().expect("latest code generation");

    let database_root = TempDir::new().expect("database root");
    let database_path = database_root.path().join("project.db");
    let authority =
        DatabaseAuthority::acquire_test(&database_path, "Jina semantic bridge integration")
            .expect("database authority");
    let database = Arc::new(
        Database::initialize(&database_path, &authority)
            .await
            .expect("project database")
            .0,
    );
    let handle = DaemonSemanticRuntimeHandleV1::new(1, 64, 2 << 30).expect("semantic handle");
    let runtime = ProductionSemanticRuntimeV1::new(
        handle.clone(),
        Arc::clone(&database),
        Arc::clone(&lifecycle),
        SemanticResourceCeilings {
            max_model_bytes: 1024 * 1024 * 1024,
            max_tokenizer_bytes: 64 * 1024 * 1024,
            max_resident_bytes: 2 * 1024 * 1024 * 1024,
            max_threads: 1,
            max_concurrent_sessions: 1,
            max_batch_size: 4,
            max_sequence_length: 512,
            load_deadline_ms: 180_000,
        },
    );

    assert!(runtime.schedule_saved_generation(&latest.generation));
    latest
        .production_query_owners()
        .expect("ordinary lanes remain callable during Jina startup");
    tokio::time::timeout(Duration::from_mins(3), async {
        while handle.current().is_none() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("Jina projection became atomically current");
    let current = handle.current().expect("current semantic pointer");
    assert!(current_query_factory(&handle).is_some());
    let store = DatabaseVectorGenerationStoreV1::open(database.as_ref())
        .await
        .expect("vector store");
    assert_eq!(
        store.active_generation_id().await.expect("active vector"),
        Some(current.generation.clone())
    );

    let restarted_handle =
        DaemonSemanticRuntimeHandleV1::new(1, 64, 2 << 30).expect("restarted handle");
    let restarted = ProductionSemanticRuntimeV1::new(
        restarted_handle.clone(),
        database,
        lifecycle,
        SemanticResourceCeilings {
            max_model_bytes: 1024 * 1024 * 1024,
            max_tokenizer_bytes: 64 * 1024 * 1024,
            max_resident_bytes: 2 * 1024 * 1024 * 1024,
            max_threads: 1,
            max_concurrent_sessions: 1,
            max_batch_size: 4,
            max_sequence_length: 512,
            load_deadline_ms: 180_000,
        },
    );
    assert!(
        restarted
            .restore_current(&latest.generation)
            .await
            .expect("restore current generation")
    );
    assert_eq!(restarted_handle.current(), Some(current));
    assert!(current_query_factory(&restarted_handle).is_some());
}

#[tokio::test]
async fn callable_application_operations_consume_exact_lexical_and_graph_owners() {
    let fixture = GitFixture::new(&[(
        "src/lib.rs",
        "pub fn caller() { callee(); }\npub fn callee() {}\n",
    )]);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(1);
    registry
        .mount_worktree(fixture.path(), store.path().to_path_buf(), None)
        .await
        .expect("mount daemon-owned scheduler");
    let generation = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Some(generation) = registry.latest_generation_id(fixture.path()).await {
                break generation;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("initial generation published");
    let latest = registry
        .generation_for(&generation)
        .await
        .expect("mounted generation");
    let repository = latest.generation.snapshot().repository.clone();
    let worktree = latest
        .generation
        .snapshot()
        .worktree
        .clone()
        .expect("worktree identity");
    let scope = CodeQueryScope::new(generation.clone(), None).expect("query scope");

    let exact_operation =
        callable_code_operation(CallableCodeOperationKind::ExactOccurrence).expect("operation");
    let exact_context = application_context(&exact_operation, repository.clone(), worktree.clone());
    let exact_request =
        ExactOccurrenceRequest::new("caller", None, scope.clone(), query_meta()).expect("exact");
    let exact = registry
        .exact_occurrence(
            RetrievalPortContext {
                request: &exact_context,
                operation: &exact_operation,
            },
            &exact_request,
        )
        .await;
    match exact {
        RetrievalPortOutcome::Completed(evidence) => assert!(
            !evidence.payload.expect("exact page").items.is_empty(),
            "exact operation must return production lane evidence"
        ),
        outcome => panic!("expected completed exact operation, got {outcome:?}"),
    }

    let lexical_operation =
        callable_code_operation(CallableCodeOperationKind::PhraseSearch).expect("operation");
    let lexical_context =
        application_context(&lexical_operation, repository.clone(), worktree.clone());
    let query = EphemeralSanitizedQueryViewV1::sanitize(
        "callee",
        SanitizerRevision::new("sanitizer.query.fixture").expect("sanitizer"),
        QueryNormalizationRevision::new("normalization.query.fixture").expect("normalization"),
    )
    .expect("query");
    let lexical_request = PhraseSearchRequest::new(
        query,
        vec!["callee".to_owned()],
        scope.clone(),
        query_meta(),
    )
    .expect("lexical");
    let lexical = registry
        .phrase_search(
            RetrievalPortContext {
                request: &lexical_context,
                operation: &lexical_operation,
            },
            &lexical_request,
        )
        .await;
    match lexical {
        RetrievalPortOutcome::Completed(evidence) => assert!(
            !evidence.payload.expect("lexical page").items.is_empty(),
            "lexical operation must return production lane evidence"
        ),
        outcome => panic!("expected completed lexical operation, got {outcome:?}"),
    }

    let caller = latest
        .generation
        .symbols()
        .symbols
        .iter()
        .find(|record| record.qualified_name.ends_with("caller"))
        .expect("caller symbol")
        .occurrence
        .as_str()
        .to_owned();
    let graph_operation =
        callable_code_operation(CallableCodeOperationKind::Callees).expect("operation");
    let graph_context = application_context(&graph_operation, repository, worktree);
    let graph_request = CodeRelationRequest {
        node_id: caller,
        maximum_depth: 2,
        resolve_trait_dispatch: false,
        scope,
        meta: query_meta(),
    };
    let graph = registry
        .callees(
            RetrievalPortContext {
                request: &graph_context,
                operation: &graph_operation,
            },
            &graph_request,
        )
        .await;
    match graph {
        RetrievalPortOutcome::Completed(evidence) => assert!(
            !evidence.payload.expect("graph page").items.is_empty(),
            "graph operation must return production lane evidence"
        ),
        outcome => panic!("expected completed graph operation, got {outcome:?}"),
    }

    registry.shutdown().await;
}
