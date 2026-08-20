use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use tempfile::TempDir;
use tracedecay_application::retrieval::{
    CodeFacetDimension, CodeFacetRequest, CodeNavigationRequest, CodeTimelineRequest,
};
use tracedecay_application::{
    CallableCodeOperationKind, CallableCodeQueryPort, CancellationContext, CapabilityGrantSnapshot,
    CodeQueryScope, CodeRelationRequest, CodeSymbolSearchRequest, Deadline, DisclosureClass,
    ExactOccurrenceRequest, OmissionReason, OpaqueCursor, PageRequest, PhraseSearchRequest,
    QualifiedNameRequest, RequestContext, RequestId, ResolvedScope, ResultProjection,
    RetrievalOrder, RetrievalPortContext, RetrievalPortOutcome, RetrievalRequestMeta,
    SourceMetadataRequest, callable_code_operation,
};
use tracedecay_domain::{
    ActorId, AuthorizationRevision, CalibrationProfileId, CodeGenerationId, CommitId,
    ComponentRevision, DiversityPolicy, EphemeralSanitizedQueryViewV1, ExactAdmissionRuleRevision,
    ExactClass, FreshnessVectorDigest, FusedCandidate, FusionProfile, LogicalEvidenceId,
    ManifestDigest, OptionalStagePublicStatus, PrincipalId, PrivacyDomainId, ProjectId,
    QueryNormalizationRevision, RankedCandidate, RefId, RelationEdgeKindV1, RepositoryId,
    RerankPolicy, RetrievalAnchorId, RetrievalBudget, RetrievalCursorKeyId, RetrievalRequest,
    RetrievalScope, RetrievalSnapshot, RetrieverKind, SanitizerRevision, ScoreDomainCalibrationV1,
    ScoreDomainId, SensitivityLevelV1, SingleRootScopeV1, TemporalModeV1, UtcMicros,
    VectorWatermark, WorktreeId,
};

#[cfg(feature = "semantic-fastembed")]
use crate::config::SemanticResourceCeilings;
#[cfg(feature = "semantic-fastembed")]
use crate::db::{Database, DatabaseAuthority, TestDatabaseRuntimeMode};
#[cfg(feature = "semantic-fastembed")]
use crate::semantic_code::{
    CatalogedFastEmbedModelV1, DaemonSemanticRuntimeHandleV1, FastEmbedModelCatalogV1,
    ModelLifecycleErrorV1, ModelMemberSourceV1, SemanticModelLifecycleOwnerV1,
    production_fastembed_catalog,
};
#[cfg(feature = "semantic-fastembed")]
use tracedecay_graph_db::NeverCancelled;
#[cfg(feature = "semantic-fastembed")]
use tracedecay_usecases::semantic_runtime::{
    ProductionSemanticRuntimeV1, RetainedSemanticVectorGraphV1, SemanticRuntimeFuture,
    SemanticVectorGraphErrorV1, SemanticVectorGraphProviderV1,
};

use super::registry::{
    ColdMountOpenEventV1, ServingGenerationInstallationOutcomeV1,
    ServingGenerationRollbackOutcomeV1,
};
use super::{
    CodeIndexCadenceOutcomeV1, CodeIndexCadenceTriggerV1, CodeIndexReconcileOutcomeV1,
    CodeIndexSchedulerRegistryV1, CodeIndexWorktreeSchedulerV1, GenerationDecodeAdmissionV1,
    SharedCodeIndexBytePoolV1,
};
use crate::semantic_code::rerank_adapter::GenerationBoundCodeRerankViewsV1;
use tracedecay_query::retrieval::QueryAuthorityV1;
use tracedecay_query::retrieval::fusion::RetrievalCursorKeyringV1;
use tracedecay_query::retrieval::rerank::{
    BoundedRerankRuntimeV1, DeterministicLocalRerankExecutorV1, LocalRerankFailureV1,
    LocalRerankInputV1, LocalRerankPermitV1, RerankExecutionControlV1,
};

mod semantic_schedule_order_tests;

struct GitFixture {
    root: TempDir,
}

/// Most scheduler tests share this one-file lib fixture. Build the git repo
/// once per process and filesystem-copy it so each test avoids five fresh
/// `git` subprocesses.
const ALPHA_LIB_V1: &[(&str, &str)] = &[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")];
const RETAINED_REVISION_0: &[(&str, &str)] =
    &[("src/lib.rs", "pub fn retained_revision() -> usize { 0 }\n")];

impl GitFixture {
    fn new(files: &[(&str, &str)]) -> Self {
        if files == ALPHA_LIB_V1 {
            return Self::from_template(alpha_lib_v1_template());
        }
        if files == RETAINED_REVISION_0 {
            return Self::from_template(retained_revision_0_template());
        }
        Self::build_fresh(files)
    }

    fn build_fresh(files: &[(&str, &str)]) -> Self {
        let root = TempDir::new().expect("fixture root");
        git(root.path(), &["init", "-q", "-b", "main"]);
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

    fn from_template(template: &Path) -> Self {
        let root = TempDir::new().expect("fixture root");
        copy_dir_recursive(template, root.path());
        Self { root }
    }

    fn path(&self) -> &Path {
        self.root.path()
    }

    fn edit(&self, path: &str, source: &str) {
        write(self.path(), path, source);
    }
}

fn alpha_lib_v1_template() -> &'static Path {
    static TEMPLATE: OnceLock<TempDir> = OnceLock::new();
    TEMPLATE
        .get_or_init(|| GitFixture::build_fresh(ALPHA_LIB_V1).root)
        .path()
}

fn retained_revision_0_template() -> &'static Path {
    static TEMPLATE: OnceLock<TempDir> = OnceLock::new();
    TEMPLATE
        .get_or_init(|| GitFixture::build_fresh(RETAINED_REVISION_0).root)
        .path()
}

fn copy_dir_recursive(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).expect("create fixture copy root");
    for entry in std::fs::read_dir(src).expect("read fixture template") {
        let entry = entry.expect("fixture template entry");
        let file_type = entry.file_type().expect("fixture template entry type");
        let destination = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_recursive(&entry.path(), &destination);
        } else {
            std::fs::copy(entry.path(), &destination).expect("copy fixture template file");
        }
    }
}

fn git(root: &Path, args: &[&str]) {
    let status = Command::new(crate::git::git_program())
        .current_dir(root)
        .args(args)
        .status()
        .expect("run git fixture command");
    assert!(status.success(), "git fixture command failed: {args:?}");
}

fn git_stdout(root: &Path, args: &[&str]) -> String {
    let output = Command::new(crate::git::git_program())
        .current_dir(root)
        .args(args)
        .output()
        .expect("run git fixture command");
    assert!(
        output.status.success(),
        "git fixture command failed: {args:?}"
    );
    String::from_utf8(output.stdout)
        .expect("git fixture stdout")
        .trim()
        .to_owned()
}

fn write(root: &Path, path: &str, source: &str) {
    let path = root.join(path);
    std::fs::create_dir_all(path.parent().expect("source parent")).expect("create source parent");
    std::fs::write(path, source).expect("write fixture source");
}

fn test_project_id() -> ProjectId {
    ProjectId::new("project.code-index-tests").expect("valid test project identity")
}

fn scheduler(
    fixture: &GitFixture,
    store_root: PathBuf,
    bytes: Arc<SharedCodeIndexBytePoolV1>,
) -> CodeIndexWorktreeSchedulerV1 {
    CodeIndexWorktreeSchedulerV1::open(test_project_id(), fixture.path(), store_root, bytes)
        .expect("open worktree scheduler")
}

fn published(outcome: CodeIndexReconcileOutcomeV1) -> super::CodeIndexPublishEvidenceV1 {
    match outcome {
        CodeIndexReconcileOutcomeV1::Published(evidence) => evidence,
        CodeIndexReconcileOutcomeV1::Noop(evidence) => {
            panic!("expected a published generation, got noop {evidence:?}")
        }
    }
}

fn retention_generations(
    fixture: &GitFixture,
    store_root: &Path,
    count: usize,
) -> Vec<CodeGenerationId> {
    let mut scheduler = scheduler(
        fixture,
        store_root.to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );
    // Every seeded revision must carry content no earlier revision published.
    // A store is seeded per scope while the fixture worktree is shared, so a
    // per-call `0..count` sequence replayed the same bytes for the second scope
    // and the scheduler correctly no-op'd instead of sealing a new generation.
    static SEEDED_REVISION: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(1);
    let mut generations = Vec::with_capacity(count);
    for revision in 0..count {
        if revision > 0 {
            let revision = SEEDED_REVISION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            fixture.edit(
                "src/lib.rs",
                &format!("pub fn retained_revision() -> usize {{ {revision} }}\n"),
            );
            scheduler.notify_hook_paths([PathBuf::from("src/lib.rs")]);
        }
        generations.push(
            published(
                scheduler
                    .reconcile_now()
                    .expect("publish retention fixture generation"),
            )
            .generation_id,
        );
    }
    generations
}

fn remove_historical_pointer_entries(store_root: &Path) {
    use crate::retention::code_index_generations::durable_generation_index_digest;

    let pointer_path = store_root.join("active-code-generation-v1.json");
    let mut pointer: crate::retention::code_index_generations::DurablePublicationPointerV1 =
        serde_json::from_slice(&std::fs::read(&pointer_path).expect("read publication pointer"))
            .expect("decode publication pointer");
    pointer
        .generation_index
        .retain(|entry| entry.generation_id == pointer.generation_id);
    pointer.generation_index_truncated = true;
    pointer.generation_index_digest = Some(
        durable_generation_index_digest(
            &pointer.generation_index,
            pointer.generation_index_truncated,
        )
        .expect("digest active-only publication index"),
    );
    std::fs::write(
        pointer_path,
        serde_json::to_vec(&pointer).expect("encode legacy publication pointer"),
    )
    .expect("write legacy publication pointer");
}

#[test]
fn code_generation_retention_preserves_every_pointer_addressable_generation() {
    use crate::retention::code_index_generations::{
        CodeGenerationRetentionModeV1, DEFAULT_SUPERSEDED_GENERATION_FLOOR,
        run_code_generation_retention,
    };

    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn retained_revision() -> usize { 0 }\n")]);
    let store = TempDir::new().expect("store root");
    let generations = retention_generations(&fixture, store.path(), 5);

    let report = run_code_generation_retention(
        store.path(),
        &BTreeSet::new(),
        DEFAULT_SUPERSEDED_GENERATION_FLOOR,
        CodeGenerationRetentionModeV1::Apply,
        UtcMicros(49),
    )
    .expect("apply retention");

    assert!(report.plan.collectable_generations.is_empty());
    assert!(report.deleted_generations.is_empty());
    let reopened = scheduler(
        &fixture,
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );
    for generation in generations {
        assert!(
            reopened
                .publication
                .load_generation(&generation)
                .expect("read pointer-addressable generation")
                .is_some(),
            "retention must preserve every generation still named by the pointer"
        );
    }
}

#[test]
fn bounded_pointer_history_collects_evicted_clean_and_dirty_generations() {
    use crate::retention::code_index_generations::{
        CodeGenerationRetentionModeV1, DEFAULT_SUPERSEDED_GENERATION_FLOOR,
        DurablePublicationPointerV1, MAX_DURABLE_GENERATION_INDEX_BYTES_V1,
        MAX_DURABLE_GENERATION_INDEX_ENTRIES_V1, run_code_generation_retention,
    };

    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn retained_revision() -> usize { 0 }\n")]);
    let store = TempDir::new().expect("store root");
    let generations = retention_generations(
        &fixture,
        store.path(),
        MAX_DURABLE_GENERATION_INDEX_ENTRIES_V1 + 3,
    );
    let pointer: DurablePublicationPointerV1 = serde_json::from_slice(
        &std::fs::read(store.path().join("active-code-generation-v1.json"))
            .expect("read bounded publication pointer"),
    )
    .expect("decode bounded publication pointer");

    assert!(pointer.generation_index_truncated);
    assert_eq!(
        pointer.generation_index.len(),
        MAX_DURABLE_GENERATION_INDEX_ENTRIES_V1
    );
    assert!(
        pointer
            .generation_index
            .iter()
            .map(|entry| entry.size_bytes)
            .sum::<u64>()
            <= MAX_DURABLE_GENERATION_INDEX_BYTES_V1
    );
    assert!(
        pointer
            .generation_index
            .iter()
            .any(|entry| entry.source_reference.is_none()),
        "dirty snapshots must consume the same retained-history budget"
    );

    let report = run_code_generation_retention(
        store.path(),
        &BTreeSet::new(),
        DEFAULT_SUPERSEDED_GENERATION_FLOOR,
        CodeGenerationRetentionModeV1::Apply,
        UtcMicros(50),
    )
    .expect("collect generations evicted from bounded history");
    assert_eq!(
        report.deleted_generations.len(),
        generations.len() - pointer.generation_index.len()
    );

    let reopened = scheduler(
        &fixture,
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );
    let retained = pointer
        .generation_index
        .iter()
        .map(|entry| entry.generation_id.as_str())
        .collect::<BTreeSet<_>>();
    for generation in &generations {
        assert_eq!(
            reopened
                .publication
                .load_generation(generation)
                .expect("read bounded generation")
                .is_some(),
            retained.contains(generation.as_str()),
            "only pointer-addressable generations may survive collection"
        );
    }
    assert_eq!(
        std::fs::read_dir(store.path().join("code-generations-v1"))
            .expect("list retained generations")
            .count(),
        pointer.generation_index.len()
    );
}

#[test]
fn code_generation_retention_dry_run_reports_without_deleting() {
    use crate::retention::code_index_generations::{
        CodeGenerationRetentionModeV1, DEFAULT_SUPERSEDED_GENERATION_FLOOR,
        run_code_generation_retention,
    };

    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn retained_revision() -> usize { 0 }\n")]);
    let store = TempDir::new().expect("store root");
    let generations = retention_generations(&fixture, store.path(), 5);
    remove_historical_pointer_entries(store.path());

    let report = run_code_generation_retention(
        store.path(),
        &BTreeSet::new(),
        DEFAULT_SUPERSEDED_GENERATION_FLOOR,
        CodeGenerationRetentionModeV1::DryRun,
        UtcMicros(50),
    )
    .expect("plan retention");

    assert_eq!(report.plan.superseded_generations.len(), 4);
    assert_eq!(report.plan.collectable_generations.len(), 4);
    assert_eq!(
        report
            .plan
            .collectable_generations
            .iter()
            .map(|generation| generation.generation_id.clone())
            .collect::<BTreeSet<_>>(),
        generations[..4].iter().cloned().collect()
    );
    println!(
        "dry_run superseded_count={} superseded_bytes={} collectable_count={} collectable_bytes={} deleted_count={}",
        report.plan.superseded_generations.len(),
        report.plan.superseded_generation_bytes(),
        report.plan.collectable_generations.len(),
        report.plan.collectable_generation_bytes(),
        report.deleted_generations.len(),
    );
    assert_eq!(report.deleted_generations.len(), 0);
    assert!(report.receipt.is_none());
    assert!(
        store
            .path()
            .join("code-generations-v1")
            .join(&report.plan.collectable_generations[0].generation_file)
            .is_file()
    );
}

#[test]
fn code_generation_retention_never_sweeps_vector_readable_source() {
    use crate::retention::code_index_generations::{
        CodeGenerationRetentionModeV1, DEFAULT_SUPERSEDED_GENERATION_FLOOR,
        run_code_generation_retention,
    };

    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn retained_revision() -> usize { 0 }\n")]);
    let store = TempDir::new().expect("store root");
    let generations = retention_generations(&fixture, store.path(), 6);
    remove_historical_pointer_entries(store.path());
    let vector_readable = BTreeSet::from([generations[0].clone()]);

    let report = run_code_generation_retention(
        store.path(),
        &vector_readable,
        DEFAULT_SUPERSEDED_GENERATION_FLOOR,
        CodeGenerationRetentionModeV1::Apply,
        UtcMicros(60),
    )
    .expect("apply retention");

    let vector_generation = report
        .plan
        .superseded_generations
        .iter()
        .find(|generation| generation.generation_id == generations[0])
        .expect("vector-readable generation was inventoried");
    assert!(
        store
            .path()
            .join("code-generations-v1")
            .join(&vector_generation.generation_file)
            .is_file(),
        "a generation named by retained_readable_sources must survive the sweep"
    );
    assert!(
        report
            .deleted_generations
            .iter()
            .all(|generation| generation.generation_id != generations[0])
    );
    assert_eq!(report.deleted_generations.len(), 4);
    assert_eq!(
        report
            .deleted_generations
            .iter()
            .map(|generation| generation.generation_id.clone())
            .collect::<BTreeSet<_>>(),
        generations[1..generations.len() - 1]
            .iter()
            .cloned()
            .collect()
    );
}

#[test]
fn code_generation_retention_emits_durable_reclaim_receipt() {
    use crate::retention::code_index_generations::{
        CodeGenerationRetentionModeV1, DEFAULT_SUPERSEDED_GENERATION_FLOOR,
        observe_code_generation_retention, run_code_generation_retention,
    };

    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn retained_revision() -> usize { 0 }\n")]);
    let store = TempDir::new().expect("store root");
    retention_generations(&fixture, store.path(), 5);
    remove_historical_pointer_entries(store.path());

    let report = run_code_generation_retention(
        store.path(),
        &BTreeSet::new(),
        DEFAULT_SUPERSEDED_GENERATION_FLOOR,
        CodeGenerationRetentionModeV1::Apply,
        UtcMicros(70),
    )
    .expect("apply retention");

    let receipt = report.receipt.expect("applied reclaim receipt");
    assert_eq!(receipt.deleted_generations.len(), 4);
    assert_eq!(
        receipt.reclaimed_bytes,
        receipt
            .deleted_generations
            .iter()
            .map(|generation| generation.size_bytes)
            .sum::<u64>()
    );
    assert!(
        store
            .path()
            .join("code-generation-retention-receipts-v1")
            .join(format!("receipt-{}.json", receipt.receipt_digest))
            .is_file()
    );
    let observed =
        observe_code_generation_retention(store.path()).expect("observe retained generations");
    assert_eq!(observed.superseded_generation_count, 0);
    assert_eq!(observed.superseded_generation_bytes, 0);
}

// --- Code-index scope-root reconciliation ----------------------------------
//
// Generation retention above operates inside one
// `code-index-v1/<sha256(canonical_project_root)>/` scope. These tests cover the
// pass that reconciles the *scopes themselves* against the live project roots,
// which is the only thing that can reach a scope whose root no longer exists.

const EIGHT_DAYS_SECS: i64 = 8 * 24 * 60 * 60;

fn unix_now_secs() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock after the unix epoch")
            .as_secs(),
    )
    .expect("unix seconds fit i64")
}

/// Publish `count` sealed generations into the scope `code_index_root` would use
/// for `canonical_root`, and return that scope directory.
fn seeded_scope(
    fixture: &GitFixture,
    code_index_root: &Path,
    canonical_root: &Path,
    count: usize,
) -> PathBuf {
    let scope = super::scoped_code_index_store_root(code_index_root, canonical_root);
    std::fs::create_dir_all(&scope).expect("create code-index scope root");
    retention_generations(fixture, &scope, count);
    scope
}

fn execute_scope_retention_with_test_binding_cleanup(
    store_root: &Path,
    live_roots: &BTreeSet<PathBuf>,
    minimum_stranding_age_secs: i64,
    mode: crate::retention::code_index_generations::CodeGenerationRetentionModeV1,
    now_secs: i64,
    completed_at: UtcMicros,
) -> Result<
    crate::retention::code_index_generations::ScopeRootRetentionReportV1,
    crate::retention::code_index_generations::CodeGenerationRetentionErrorV1,
> {
    use crate::retention::code_index_generations::{
        CodeGenerationRetentionModeV1, ScopeRootAuthorityReceiptV1,
        ScopeRootBindingCleanupReplayV1, ScopeRootCandidateBindingV1, ScopeRootLivenessProofV1,
        complete_scope_root_binding_cleanup, execute_scope_root_retention,
        plan_scope_root_retention, plan_scope_root_retention_with_liveness_proof,
        prepare_scope_root_binding_cleanup, recover_scope_root_retention,
    };

    if mode == CodeGenerationRetentionModeV1::Apply {
        recover_scope_root_retention(store_root)?;
    }
    let observed =
        plan_scope_root_retention(store_root, live_roots, minimum_stranding_age_secs, now_secs)?;
    let source_scope = tracedecay_store::StoreShardIdV1::project(
        tracedecay_domain::BrainId::new("brain.scope-test").expect("test brain"),
        tracedecay_domain::UserProfileId::new("profile.scope-test").expect("test profile"),
        tracedecay_domain::ProjectId::new("project.scope-test").expect("test project"),
    );
    let candidate = observed
        .collectable_scopes
        .first()
        .map_or_else(|| "0".repeat(64), |scope| scope.scope_hash.clone());
    let receipt = |revision: &str, digit: char| ScopeRootAuthorityReceiptV1 {
        revision: revision.to_owned(),
        terminal_count: 1,
        digest: format!("sha256:{}", digit.to_string().repeat(64)),
    };
    let proof = ScopeRootLivenessProofV1::new(
        live_roots
            .iter()
            .map(|root| crate::retention::code_index_generations::code_index_scope_hash(root))
            .collect(),
        receipt("registry", '1'),
        receipt("git", '2'),
        receipt("mount", '3'),
        receipt("config", '4'),
        receipt("vector", '5'),
        receipt("dependency", '6'),
        ScopeRootCandidateBindingV1 {
            scope_hash: candidate.clone(),
            source_scope: source_scope.clone(),
            vector_census_revision: "vector".to_owned(),
            live: false,
        },
    )?;
    let plan = if observed.collectable_scopes.is_empty() {
        observed
    } else {
        plan_scope_root_retention_with_liveness_proof(
            store_root,
            proof.clone(),
            minimum_stranding_age_secs,
            now_secs,
        )?
    };
    if !plan.collectable_scopes.is_empty() {
        prepare_scope_root_binding_cleanup(
            store_root,
            &plan,
            &candidate,
            &source_scope,
            &proof,
            completed_at,
        )?;
    }
    let report =
        execute_scope_root_retention(store_root, plan, &proof, mode, now_secs, completed_at)?;
    if !report.collected_scopes.is_empty() {
        complete_scope_root_binding_cleanup(
            store_root,
            &ScopeRootBindingCleanupReplayV1 {
                scope_hash: candidate,
                source_scope,
                liveness_proof: proof,
            },
        )?;
    }
    Ok(report)
}

#[test]
fn stranded_code_index_scope_is_collected_while_its_live_sibling_is_untouched() {
    use crate::retention::code_index_generations::{
        CodeGenerationRetentionModeV1, DEFAULT_STRANDED_SCOPE_MINIMUM_AGE_SECS,
    };

    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn retained_revision() -> usize { 0 }\n")]);
    let code_index = TempDir::new().expect("code-index root");
    let live_root = fixture.path().to_path_buf();
    let deleted_worktree = fixture.path().join(".claude/worktrees/agent-deadbeef");

    let live_scope = seeded_scope(&fixture, code_index.path(), &live_root, 2);
    let stranded_scope = seeded_scope(&fixture, code_index.path(), &deleted_worktree, 2);
    let live_roots = BTreeSet::from([live_root]);

    let report = execute_scope_retention_with_test_binding_cleanup(
        code_index.path(),
        &live_roots,
        DEFAULT_STRANDED_SCOPE_MINIMUM_AGE_SECS,
        CodeGenerationRetentionModeV1::Apply,
        unix_now_secs() + EIGHT_DAYS_SECS,
        UtcMicros(90),
    )
    .expect("reconcile code-index scope roots");

    assert_eq!(report.collected_scopes.len(), 1);
    assert_eq!(report.plan.live_scope_count, 1);
    assert!(
        !stranded_scope.exists(),
        "a scope whose canonical project root is gone must be collected"
    );
    assert!(
        live_scope.join("active-code-generation-v1.json").is_file(),
        "reconciliation must never touch a scope a live root names"
    );
    let receipt = report.receipt.expect("durable reconciliation receipt");
    assert!(receipt.reclaimed_bytes > 0);
    assert_eq!(
        receipt.reclaimed_bytes,
        report.plan.collectable_scope_bytes()
    );
    assert!(
        code_index
            .path()
            .join("code-index-scope-retention-receipts-v1")
            .join(format!("receipt-{}.json", receipt.receipt_digest))
            .is_file(),
        "collection must leave a durable receipt outside the collected scope"
    );
}

#[test]
fn code_index_scope_matching_a_live_worktree_is_never_collected() {
    use crate::retention::code_index_generations::{
        DEFAULT_STRANDED_SCOPE_MINIMUM_AGE_SECS, plan_scope_root_retention,
    };

    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn retained_revision() -> usize { 0 }\n")]);
    let code_index = TempDir::new().expect("code-index root");
    let primary = fixture.path().to_path_buf();
    let linked_worktree = fixture.path().join(".claude/worktrees/agent-live");

    seeded_scope(&fixture, code_index.path(), &primary, 1);
    seeded_scope(&fixture, code_index.path(), &linked_worktree, 1);
    let live_roots = BTreeSet::from([primary, linked_worktree]);

    let plan = plan_scope_root_retention(
        code_index.path(),
        &live_roots,
        DEFAULT_STRANDED_SCOPE_MINIMUM_AGE_SECS,
        unix_now_secs() + EIGHT_DAYS_SECS,
    )
    .expect("plan scope reconciliation");

    assert_eq!(plan.live_scope_count, 2);
    assert_eq!(
        plan.stranded_scope_count(),
        0,
        "a linked worktree is a live canonical root, not a stranded scope"
    );
    assert_eq!(plan.stranded_scope_bytes(), 0);
}

#[test]
fn code_index_scope_with_a_pending_generation_journal_is_refused() {
    use crate::retention::code_index_generations::{
        CodeGenerationRetentionModeV1, DEFAULT_STRANDED_SCOPE_MINIMUM_AGE_SECS,
        StrandedScopeRefusalV1,
    };

    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn retained_revision() -> usize { 0 }\n")]);
    let code_index = TempDir::new().expect("code-index root");
    let live_root = fixture.path().to_path_buf();
    let deleted_worktree = fixture.path().join(".claude/worktrees/agent-interrupted");

    seeded_scope(&fixture, code_index.path(), &live_root, 1);
    let stranded_scope = seeded_scope(&fixture, code_index.path(), &deleted_worktree, 2);
    // An unfinished generation-retention journal inside the scope. Recovering it
    // belongs to that scope's own owner; collecting the scope would destroy the
    // evidence recovery needs.
    std::fs::write(
        stranded_scope.join(".code-generation-retention-transaction-v1.json"),
        b"{}",
    )
    .expect("seed pending generation journal");
    let live_roots = BTreeSet::from([live_root]);

    let report = execute_scope_retention_with_test_binding_cleanup(
        code_index.path(),
        &live_roots,
        DEFAULT_STRANDED_SCOPE_MINIMUM_AGE_SECS,
        CodeGenerationRetentionModeV1::Apply,
        unix_now_secs() + EIGHT_DAYS_SECS,
        UtcMicros(91),
    )
    .expect("reconcile code-index scope roots");

    assert!(report.collected_scopes.is_empty());
    assert!(report.receipt.is_none());
    assert!(report.plan.collectable_scopes.is_empty());
    assert_eq!(report.plan.refused_scopes.len(), 1);
    assert_eq!(
        report.plan.refused_scopes[0].refusal,
        StrandedScopeRefusalV1::PendingGenerationRetention
    );
    assert!(
        stranded_scope
            .join("active-code-generation-v1.json")
            .is_file(),
        "a scope mid-transaction must survive reconciliation untouched"
    );
    assert!(
        report.plan.stranded_scope_bytes() > 0,
        "a refused scope is still unreachable storage and must be reported"
    );
}

#[test]
fn freshly_stranded_code_index_scope_is_retained_until_the_age_gate_passes() {
    use crate::retention::code_index_generations::{
        CodeGenerationRetentionModeV1, DEFAULT_STRANDED_SCOPE_MINIMUM_AGE_SECS,
    };

    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn retained_revision() -> usize { 0 }\n")]);
    let code_index = TempDir::new().expect("code-index root");
    let live_root = fixture.path().to_path_buf();
    let just_removed = fixture.path().join(".claude/worktrees/agent-just-removed");

    seeded_scope(&fixture, code_index.path(), &live_root, 1);
    let stranded_scope = seeded_scope(&fixture, code_index.path(), &just_removed, 1);
    let live_roots = BTreeSet::from([live_root]);

    let report = execute_scope_retention_with_test_binding_cleanup(
        code_index.path(),
        &live_roots,
        DEFAULT_STRANDED_SCOPE_MINIMUM_AGE_SECS,
        CodeGenerationRetentionModeV1::Apply,
        unix_now_secs(),
        UtcMicros(92),
    )
    .expect("reconcile code-index scope roots");

    assert!(report.collected_scopes.is_empty());
    assert_eq!(report.plan.retained_immature_scopes.len(), 1);
    assert!(
        stranded_scope.exists(),
        "a scope stranded moments ago may still belong to a worktree being moved"
    );
    assert_eq!(
        report.plan.stranded_scope_count(),
        1,
        "immaturity delays collection; it never hides the bytes"
    );
}

#[test]
fn scope_reconciliation_refuses_to_collect_without_a_proven_live_root_set() {
    use crate::retention::code_index_generations::{
        CodeGenerationRetentionModeV1, DEFAULT_STRANDED_SCOPE_MINIMUM_AGE_SECS,
        plan_scope_root_retention,
    };

    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn retained_revision() -> usize { 0 }\n")]);
    let code_index = TempDir::new().expect("code-index root");
    let scope = seeded_scope(&fixture, code_index.path(), fixture.path(), 2);
    let unproven = BTreeSet::new();

    // "The registry could not be read" and "this profile has no live roots" are
    // indistinguishable at this layer, and one of those readings deletes the
    // whole store. The planner refuses rather than choosing.
    let planned = plan_scope_root_retention(
        code_index.path(),
        &unproven,
        DEFAULT_STRANDED_SCOPE_MINIMUM_AGE_SECS,
        unix_now_secs() + EIGHT_DAYS_SECS,
    );
    let applied = execute_scope_retention_with_test_binding_cleanup(
        code_index.path(),
        &unproven,
        DEFAULT_STRANDED_SCOPE_MINIMUM_AGE_SECS,
        CodeGenerationRetentionModeV1::Apply,
        unix_now_secs() + EIGHT_DAYS_SECS,
        UtcMicros(93),
    );

    assert!(planned.is_err());
    assert!(applied.is_err());
    assert!(
        scope.join("active-code-generation-v1.json").is_file(),
        "an empty live-root set must never mean every scope is stranded"
    );
}

/// The retention census must stay reachable on stores whose generations are
/// individually larger than any byte budget worth calling cheap. Directory
/// entries are what the census costs; bytes are not.
#[test]
fn oversized_generations_still_produce_a_complete_retention_finding() {
    use crate::retention::code_index_generations::{
        DEFAULT_SUPERSEDED_GENERATION_FLOOR, GenerationDigestVerificationV1,
        plan_code_generation_retention_with_verification,
    };
    use tracedecay_application::doctor::DoctorCoverageCompletenessV1;
    use tracedecay_application::storage::{
        CodeGenerationRetentionRecordV1, StorageByteSizeV1, StoreKeyV1,
        code_generation_retention_finding,
    };

    const ONE_GIB: u64 = 1024 * 1024 * 1024;

    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn retained_revision() -> usize { 0 }\n")]);
    let store = TempDir::new().expect("store root");
    retention_generations(&fixture, store.path(), 4);
    // Sparse growth: the manifest prefix each generation is read through is
    // untouched, only the on-disk size a byte budget would have measured.
    for entry in std::fs::read_dir(store.path().join("code-generations-v1"))
        .expect("list sealed generations")
    {
        let entry = entry.expect("sealed generation entry");
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(entry.path())
            .expect("open sealed generation");
        file.set_len(ONE_GIB).expect("grow sealed generation");
    }
    let pointer_path = store.path().join("active-code-generation-v1.json");
    let mut pointer: crate::retention::code_index_generations::DurablePublicationPointerV1 =
        serde_json::from_slice(&std::fs::read(&pointer_path).expect("read publication pointer"))
            .expect("decode publication pointer");
    // Every generation stays pointer-addressable — this test is about census
    // cost at scale, not about a collectable backlog — so each index entry's
    // recorded size must match its sparsely grown file.
    for entry in &mut pointer.generation_index {
        entry.size_bytes = ONE_GIB;
    }
    pointer.generation_index_digest = Some(
        crate::retention::code_index_generations::durable_generation_index_digest(
            &pointer.generation_index,
            pointer.generation_index_truncated,
        )
        .expect("digest sparse publication index"),
    );
    std::fs::write(
        pointer_path,
        serde_json::to_vec(&pointer).expect("encode sparse publication pointer"),
    )
    .expect("write sparse publication pointer");

    let plan = plan_code_generation_retention_with_verification(
        store.path(),
        &BTreeSet::new(),
        DEFAULT_SUPERSEDED_GENERATION_FLOOR,
        GenerationDigestVerificationV1::MetadataOnly,
    )
    .expect("metadata-only census must not depend on re-hashing gigabytes");

    assert_eq!(plan.superseded_generations.len(), 3);
    assert!(
        plan.superseded_generation_bytes() >= 3 * ONE_GIB,
        "the census must report the real footprint, not a budgeted subset"
    );

    let record = CodeGenerationRetentionRecordV1 {
        store: StoreKeyV1::new("code-index-v1").expect("valid store key"),
        superseded_generation_count: plan.superseded_generations.len() as u64,
        superseded_generation_bytes: StorageByteSizeV1(plan.superseded_generation_bytes()),
        collectable_generation_count: plan.collectable_generations.len() as u64,
        collectable_generation_bytes: StorageByteSizeV1(plan.collectable_generation_bytes()),
        stranded_scope_count: 0,
        stranded_scope_bytes: StorageByteSizeV1(0),
    };
    let finding =
        code_generation_retention_finding(&record, DoctorCoverageCompletenessV1::Complete)
            .expect("the retention finding must be producible at this size");

    assert!(
        finding.finding().coverage().is_complete(),
        "a byte budget must not downgrade coverage the census actually achieved"
    );
    assert!(finding.finding().state().is_healthy_complete());
}

struct MixedAnchorReverseRerankExecutorV1;

impl DeterministicLocalRerankExecutorV1 for MixedAnchorReverseRerankExecutorV1 {
    fn planned_model_invocations(
        &self,
        _candidate_count: u32,
    ) -> Result<u32, LocalRerankFailureV1> {
        Ok(1)
    }

    fn rerank(
        &self,
        _policy: &RerankPolicy,
        inputs: &[LocalRerankInputV1<'_>],
        _permit: LocalRerankPermitV1,
    ) -> Result<Vec<RetrievalAnchorId>, LocalRerankFailureV1> {
        Ok(inputs
            .iter()
            .rev()
            .map(|input| input.candidate.candidate.anchor_id.clone())
            .collect())
    }
}

struct ReadyRerankControlV1;

impl RerankExecutionControlV1 for ReadyRerankControlV1 {
    fn elapsed_micros(&self) -> u64 {
        0
    }

    fn is_cancelled(&self) -> bool {
        false
    }
}

fn application_context(
    operation: &tracedecay_application::ApplicationOperation,
    repository: RepositoryId,
    worktree: WorktreeId,
) -> RequestContext {
    let scope = ResolvedScope::new(
        test_project_id(),
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

fn query_authority(privacy_domain: PrivacyDomainId) -> Arc<QueryAuthorityV1> {
    let id = |value: &str| value.to_owned();
    let profile = FusionProfile {
        profile_id: id("profile.code-index.fixture")
            .try_into()
            .expect("profile id"),
        evaluation_result_anchor: id("evaluation.code-index.fixture")
            .try_into()
            .expect("evaluation anchor"),
        calibrations: RetrieverKind::QUERY_FALLBACK_LANES
            .into_iter()
            .map(|lane| {
                (
                    lane,
                    CalibrationProfileId::new(format!(
                        "calibration.{}.code-index.fixture",
                        lane.as_str()
                    ))
                    .expect("calibration id"),
                )
            })
            .collect(),
        // Plan 15 pipeline step 7: approximate candidates calibrate only within
        // a declared score domain, so the profile must cover every raw-score
        // domain the daemon lanes emit.
        score_domain_calibrations: [
            (
                RetrieverKind::ExactLiteral,
                tracedecay_query::retrieval::QUERY_EXACT_SCORE_DOMAIN_V1,
            ),
            (
                RetrieverKind::Lexical,
                tracedecay_query::retrieval::QUERY_LEXICAL_SCORE_DOMAIN_V1,
            ),
            (
                RetrieverKind::Graph,
                tracedecay_query::retrieval::QUERY_GRAPH_SCORE_DOMAIN_V1,
            ),
        ]
        .into_iter()
        .map(|(lane, domain)| {
            let score_domain = ScoreDomainId::new(domain).expect("score domain id");
            (
                score_domain.clone(),
                ScoreDomainCalibrationV1 {
                    calibration_profile_id: CalibrationProfileId::new(format!(
                        "calibration.{}.code-index.fixture",
                        lane.as_str()
                    ))
                    .expect("calibration id"),
                    score_domain,
                    raw_min_micros: 0,
                    raw_max_micros: 1_000_000,
                },
            )
        })
        .collect(),
        weights_micros: [
            (RetrieverKind::ExactLiteral, 1_000_000),
            (RetrieverKind::Lexical, 500_000),
            (RetrieverKind::Graph, 250_000),
        ]
        .into_iter()
        .collect(),
        diversity_policy_id: id("diversity.code-index.fixture")
            .try_into()
            .expect("diversity id"),
        rerank_policy_id: None,
        retrieval_budget: RetrievalBudget {
            max_candidates_per_lane: 32,
            max_fused_candidates: 32,
            max_hydrated_results: 32,
            max_hydration_bytes: 32 * 65_536,
            deadline_micros: None,
        },
    };
    let diversity = DiversityPolicy {
        policy_id: profile.diversity_policy_id.clone(),
        evaluation_result_anchor: Some(profile.evaluation_result_anchor.clone()),
        per_source_namespace: None,
        per_source_instance: None,
        per_repository: None,
        per_file: None,
        per_session_or_thread: None,
        per_copy_cluster: None,
        per_evidence_role: None,
    };
    let keyring = RetrievalCursorKeyringV1::new(
        privacy_domain,
        RetrievalCursorKeyId::new("retrieval-key.code-index.fixture").expect("cursor key id"),
        1,
        vec![7_u8; 32],
        1_000_000,
    )
    .expect("cursor keyring");
    Arc::new(
        QueryAuthorityV1::new(
            profile,
            diversity,
            ComponentRevision::new("ranking.code-index.fixture").expect("ranking revision"),
            keyring,
        )
        .expect("query authority"),
    )
}

async fn mount_query_authority(
    registry: &CodeIndexSchedulerRegistryV1,
    project_root: &Path,
    context: &RequestContext,
    privacy_domain: PrivacyDomainId,
) {
    registry
        .mount_query_authority(
            project_root,
            context.scope(),
            query_authority(privacy_domain),
        )
        .await
        .expect("mount query authority");
}

#[test]
fn semantic_mcp_reasons_bind_runtime_state_and_exact_source_generation() {
    let latest =
        tracedecay_domain::CodeGenerationId::new("generation.latest").expect("latest generation");
    let stale =
        tracedecay_domain::CodeGenerationId::new("generation.stale").expect("stale generation");
    let _vector = tracedecay_domain::VectorGenerationIdV1::new(
        tracedecay_domain::canonical_sha256(&"semantic-mcp-vector").expect("vector digest"),
    );

    assert_eq!(
        super::queries::semantic_mcp_reason(None, &latest, None),
        "semantic_runtime_unavailable"
    );
    assert_eq!(
        super::queries::semantic_mcp_reason(Some(&stale), &latest, None),
        "semantic_generation_stale"
    );
    assert_eq!(
        super::queries::semantic_mcp_reason(Some(&latest), &latest, None),
        "calibration_unavailable"
    );
    for (state, reason) in [
        (
            tracedecay_usecases::semantic_runtime::SemanticRuntimeStateV1::Indexing {
                completed_units: 1,
                total_units: 2,
            },
            "semantic_indexing",
        ),
        (
            tracedecay_usecases::semantic_runtime::SemanticRuntimeStateV1::Degraded {
                active_generation: None,
                reason:
                    tracedecay_usecases::semantic_runtime::SemanticFallbackReasonV1::RuntimeFailure,
            },
            "semantic_degraded",
        ),
        (
            tracedecay_usecases::semantic_runtime::SemanticRuntimeStateV1::Failed {
                model_id: "model.fixture".to_owned(),
                artifact_digest: format!("sha256:{}", "a".repeat(64)),
                detail: "fixture failure".to_owned(),
                retryable: true,
            },
            "semantic_failed",
        ),
    ] {
        assert_eq!(
            super::queries::semantic_mcp_reason(None, &latest, Some(&state)),
            reason
        );
    }
}

#[test]
fn capture_sanitizes_code_and_propagates_scan_evidence() {
    let secret = ["sk", "-test-", "1234567890abcdef"].concat();
    let source = format!("pub const TOKEN: &str = \"{secret}\";\n");
    let fixture = GitFixture::new(&[("src/lib.rs", &source)]);
    let store = TempDir::new().expect("store root");
    let mut first_scheduler = scheduler(
        &fixture,
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );

    published(
        first_scheduler
            .reconcile_now()
            .expect("publish sanitized code"),
    );
    let latest = first_scheduler
        .latest_complete()
        .expect("latest generation");
    let snapshot = latest.generation.snapshot();

    assert_eq!(
        snapshot.sanitizer_revision.as_str(),
        crate::privacy::CODE_SOURCE_SANITIZER_VERSION_V1
    );
    assert!(
        snapshot
            .sanitization_receipts
            .iter()
            .all(|receipt| { receipt.as_str().starts_with("privacy.code-source.v1.") })
    );
    assert!(
        latest
            .generation
            .chunks()
            .chunks()
            .iter()
            .all(|chunk| { !chunk.sanitized_text.as_str().contains(&secret) })
    );
    assert!(
        latest
            .generation
            .chunks()
            .chunks()
            .iter()
            .any(|chunk| chunk.sensitivity.level == SensitivityLevelV1::Redacted)
    );

    drop(latest);
    drop(first_scheduler);
    let restarted = scheduler(
        &fixture,
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );
    let restored = restarted
        .latest_complete()
        .expect("restart restores sanitized generation");
    assert!(
        restored
            .generation
            .chunks()
            .chunks()
            .iter()
            .all(|chunk| !chunk.sanitized_text.as_str().contains(&secret))
    );
    assert!(
        restored
            .generation
            .snapshot()
            .sanitization_receipts
            .iter()
            .all(|receipt| receipt.as_str().starts_with("privacy.code-source.v1."))
    );
}

#[tokio::test]
async fn registry_feeds_publications_and_bounded_freshness_reads() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(1);
    let mut publications = registry.subscribe_generation_publications();

    registry
        .mount_worktree(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("mount worktree");
    let initial = tokio::time::timeout(Duration::from_secs(2), publications.recv())
        .await
        .expect("initial publication timeout")
        .expect("initial publication event");
    assert_eq!(
        initial.project_root,
        fixture.path().canonicalize().expect("canonical fixture")
    );

    let freshness = registry
        .dashboard_freshness(fixture.path())
        .await
        .expect("dashboard freshness");
    assert_eq!(
        freshness.latest_generation_id.as_deref(),
        Some(initial.generation_id.as_str())
    );
    assert!(freshness.last_reconcile_micros.is_some());
    assert_eq!(freshness.staleness_state.as_deref(), Some("fresh"));
    assert_eq!(freshness.hook_hint_count, Some(0));

    fixture.edit("src/lib.rs", "pub fn alpha() -> u32 { 2 }\n");
    assert!(
        registry
            .notify_hook_paths(fixture.path(), &["src/lib.rs".to_owned()])
            .await
    );
    let changed = tokio::time::timeout(Duration::from_secs(2), publications.recv())
        .await
        .expect("changed publication timeout")
        .expect("changed publication event");
    assert_ne!(changed.generation_id, initial.generation_id);
}

/// The generation-publication broadcast carries only verified publishes —
/// generations that crossed the durable publication compare-and-swap, the
/// verified graph snapshot publish, and the serving swap. A restart that
/// restores a retained generation is a `Noop` apply and must reach the
/// serving slot silently: the post-mount query-authority waiter re-reads the
/// serving slot for restores and trusts this bus only for verified publishes.
#[tokio::test]
async fn restart_remount_serves_the_retained_generation_without_republishing() {
    let fixture = GitFixture::new(ALPHA_LIB_V1);
    let store = TempDir::new().expect("store root");
    let first = CodeIndexSchedulerRegistryV1::new(1);
    first
        .mount_worktree(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("mount worktree");
    let sealed = wait_for_initial_generation(&first, fixture.path()).await;
    first.shutdown().await;

    let restarted = CodeIndexSchedulerRegistryV1::new(1);
    // Subscribed before the remount. The per-worktree worker is serial, so a
    // broadcast wrongly emitted by the restore-era passes would sit in this
    // receiver ahead of the edit-triggered publication received below.
    let mut publications = restarted.subscribe_generation_publications();
    restarted
        .mount_worktree(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("remount worktree over the retained store");
    let restored = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(generation) = restarted.latest_generation_id(fixture.path()).await {
                break generation;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("restart serves a generation");
    assert_eq!(
        restored, sealed,
        "the restart serves the retained generation, not a rebuilt one"
    );

    fixture.edit("src/lib.rs", "pub fn alpha() -> u32 { 2 }\n");
    assert!(
        restarted
            .notify_hook_paths(fixture.path(), &["src/lib.rs".to_owned()])
            .await
    );
    let first_broadcast = tokio::time::timeout(Duration::from_secs(5), publications.recv())
        .await
        .expect("post-restart publication timeout")
        .expect("post-restart publication event");
    assert_ne!(
        first_broadcast.generation_id, sealed,
        "the retained restore stays silent; the first broadcast is the rebuilt generation"
    );
    restarted.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scheduler_notifications_remain_nonblocking_while_reconcile_is_busy() {
    let fixture = GitFixture::new(ALPHA_LIB_V1);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(1);
    registry
        .mount_worktree(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("mount worktree");
    wait_for_initial_generation(&registry, fixture.path()).await;

    let scheduler = registry
        .scheduler_handle(fixture.path())
        .await
        .expect("scheduler handle");
    let (locked_tx, locked_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let blocker = std::thread::spawn(move || {
        let _scheduler = scheduler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        locked_tx.send(()).expect("announce scheduler lock");
        release_rx.recv().expect("release scheduler lock");
    });
    locked_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("scheduler lock acquisition");

    let notification_registry = registry.clone();
    let project_root = fixture.path().to_path_buf();
    let notification = tokio::spawn(async move {
        notification_registry
            .notify_hook_paths(&project_root, &["src/lib.rs".to_owned()])
            .await
    });
    let notified = tokio::time::timeout(Duration::from_millis(100), notification)
        .await
        .expect("scheduler notification must not wait for the reconcile lock")
        .expect("notification task");
    assert!(notified);

    let registry_read = tokio::time::timeout(
        Duration::from_millis(100),
        registry.scheduler_handle(fixture.path()),
    )
    .await;
    assert!(
        registry_read.is_ok(),
        "scheduler notification must not retain the mounted-worktree registry lock"
    );

    release_tx.send(()).expect("release scheduler");
    blocker.join().expect("scheduler blocker");
    registry.shutdown().await;
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
fn generation_bound_rerank_authorizes_mixed_symbol_and_chunk_anchors() {
    let fixture = GitFixture::new(&[(
        "src/lib.rs",
        "pub fn alpha() -> u32 { 1 }\npub fn beta() -> u32 { alpha() }\n",
    )]);
    let store = TempDir::new().expect("store root");
    let mut scheduler = scheduler(
        &fixture,
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );
    published(scheduler.reconcile_now().expect("initial publish"));
    let latest = scheduler.latest_complete().expect("latest generation");
    let symbol_chunk = latest
        .generation
        .chunks()
        .chunks()
        .iter()
        .find(|chunk| chunk.anchor.symbol_occurrence_id.is_some())
        .expect("symbol chunk");
    let symbol = symbol_chunk
        .anchor
        .symbol_occurrence_id
        .as_ref()
        .expect("symbol occurrence");
    let chunk = latest
        .generation
        .chunks()
        .chunks()
        .iter()
        .find(|chunk| chunk.id != symbol_chunk.id)
        .unwrap_or(symbol_chunk);
    let anchors = [
        RetrievalAnchorId::new(format!("code-symbol:{}", symbol.as_str())).expect("symbol anchor"),
        RetrievalAnchorId::new(format!("code-chunk:{}", chunk.id.as_str())).expect("chunk anchor"),
    ];
    let candidates = anchors
        .iter()
        .enumerate()
        .map(|(ordinal, anchor)| RankedCandidate {
            candidate: FusedCandidate {
                anchor_id: anchor.clone(),
                logical_evidence_id: LogicalEvidenceId::new(anchor.as_str().to_owned())
                    .expect("logical evidence"),
                occurrences: Vec::new(),
                exact_class: ExactClass::Approximate,
                utility_micros: 2 - ordinal as u64,
                contributions: Vec::new(),
                freshness: Vec::new(),
                decisions: Vec::new(),
            },
            final_ordinal: ordinal as u32,
        })
        .collect::<Vec<_>>();
    let request = RetrievalRequest {
        principal: PrincipalId::new("principal.rerank-mixed").expect("principal"),
        scope: RetrievalScope {
            privacy_domain: latest.generation.manifest().privacy_domain.clone(),
            root: SingleRootScopeV1 {
                repository: latest.generation.snapshot().repository.clone(),
                worktree: latest.generation.snapshot().worktree.clone(),
                reference: latest.generation.snapshot().reference.clone(),
            },
        },
        temporal_mode: TemporalModeV1::Current,
        snapshot: RetrievalSnapshot {
            watermarks: VectorWatermark::default(),
            freshness_digest: FreshnessVectorDigest::new(format!("sha256:{}", "f".repeat(64)))
                .expect("freshness digest"),
            authorization_revision: AuthorizationRevision::new("authorization.rerank-mixed.v1")
                .expect("authorization revision"),
            captured_at: UtcMicros(1),
        },
        profile_id: "profile.rerank-mixed.v1"
            .to_owned()
            .try_into()
            .expect("profile"),
        budget: RetrievalBudget {
            max_candidates_per_lane: 8,
            max_fused_candidates: 8,
            max_hydrated_results: 8,
            max_hydration_bytes: 65_536,
            deadline_micros: None,
        },
    };
    let query = EphemeralSanitizedQueryViewV1::sanitize(
        "alpha",
        SanitizerRevision::new("sanitizer.rerank-mixed.v1").expect("sanitizer"),
        QueryNormalizationRevision::new("normalization.rerank-mixed.v1").expect("normalization"),
    )
    .expect("query");
    let policy = RerankPolicy {
        policy_id: "rerank.mixed.v1".to_owned().try_into().expect("policy"),
        evaluation_result_anchor: RetrievalAnchorId::new("evaluation.rerank-mixed.v1")
            .expect("evaluation"),
        max_candidates: 2,
        max_input_bytes: u64::MAX,
        max_input_tokens: u64::MAX,
        max_work_units: 2,
        max_model_invocations: 1,
        deadline_micros: None,
    };
    let mut views = GenerationBoundCodeRerankViewsV1::new(&latest.generation, &query);
    let outcome = BoundedRerankRuntimeV1::new(&mut views, &MixedAnchorReverseRerankExecutorV1)
        .rerank(&request, &policy, &candidates, &ReadyRerankControlV1);

    assert_eq!(outcome.public_status, OptionalStagePublicStatus::Complete);
    assert_eq!(
        outcome
            .ordered_candidates
            .iter()
            .map(|candidate| candidate.candidate.anchor_id.clone())
            .collect::<Vec<_>>(),
        anchors.into_iter().rev().collect::<Vec<_>>()
    );
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
    assert_eq!(hinted_publish._lane_digest, clean_publish._lane_digest);
    assert!(hinted_publish.overflow_reconciled);
}

#[test]
fn cross_worktree_byte_reuse_without_identity_alias() {
    let first = GitFixture::new(&[("src/lib.rs", "pub fn shared() -> u32 { 7 }\n")]);
    let linked_root = TempDir::new().expect("linked worktree root");
    let linked = linked_root.path().join("linked");
    let linked_arg = linked.to_str().expect("linked worktree path");
    git(
        first.path(),
        &["worktree", "add", "-q", "-b", "linked", linked_arg, "main"],
    );
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(2);
    let project_id = ProjectId::new("project.linked-worktrees").expect("valid project");

    let mut first_scheduler = registry
        .open_worktree(project_id.clone(), first.path(), store.path().join("first"))
        .expect("first scheduler");
    let mut second_scheduler = registry
        .open_worktree(project_id.clone(), &linked, store.path().join("second"))
        .expect("second scheduler");
    let first_publish = published(first_scheduler.reconcile_now().expect("first publish"));
    let second_publish = published(second_scheduler.reconcile_now().expect("second publish"));
    let first_generation = first_scheduler
        .latest_complete()
        .expect("first generation")
        .generation;
    let second_generation = second_scheduler
        .latest_complete()
        .expect("second generation")
        .generation;
    let reuse = registry.byte_pool_stats();

    assert!(reuse.reused >= 1, "sanitized source bytes must be shared");
    assert!(
        reuse.parse_chunk_reused >= 1,
        "matching parse/chunk artifacts must be physically shared"
    );
    assert_eq!(first_publish.repository_id, second_publish.repository_id);
    assert_eq!(first_generation.manifest().project_id, project_id);
    assert_eq!(second_generation.manifest().project_id, project_id);
    assert_ne!(
        first_generation.snapshot().worktree,
        second_generation.snapshot().worktree
    );
    assert_eq!(
        first_publish.snapshot_content_identity,
        second_publish.snapshot_content_identity
    );
    assert_ne!(
        first_publish._file_occurrence_ids, second_publish._file_occurrence_ids,
        "shared artifacts must never alias worktree occurrence identity"
    );
    assert_ne!(first_publish.generation_id, second_publish.generation_id);
    assert_ne!(
        first_generation.manifest().snapshot_digest,
        second_generation.manifest().snapshot_digest
    );
    assert_ne!(
        first_generation.capability().manifest_digest,
        second_generation.capability().manifest_digest,
        "authorization identity remains generation-local"
    );
    assert_ne!(
        first_generation.projection().publication_digest(),
        second_generation.projection().publication_digest(),
        "publication identity remains generation-local"
    );

    git(&linked, &["mv", "src/lib.rs", "src/renamed.rs"]);
    second_scheduler.notify_path(linked.join("src/lib.rs"));
    second_scheduler.notify_path(linked.join("src/renamed.rs"));
    published(
        second_scheduler
            .reconcile_now()
            .expect("renamed linked-worktree publish"),
    );
    let after_rename = registry.byte_pool_stats();
    assert_eq!(
        after_rename.parse_chunk_reused, reuse.parse_chunk_reused,
        "same content at a new logical path must not reuse path-bound parse/chunk artifacts"
    );

    write(&linked, "src/renamed.rs", "pub fn shared() -> u32 { 8 }\n");
    second_scheduler.notify_path(linked.join("src/renamed.rs"));
    published(
        second_scheduler
            .reconcile_now()
            .expect("edited linked-worktree publish"),
    );
    let after_edit = registry.byte_pool_stats();
    assert_eq!(
        after_edit.parse_chunk_reused, after_rename.parse_chunk_reused,
        "changed source content must not reuse the prior parse/chunk artifact"
    );
    assert_eq!(
        first_scheduler
            .latest_complete()
            .expect("first worktree remains current")
            .generation
            .manifest()
            .generation_id,
        first_publish.generation_id,
        "editing one linked worktree must not invalidate its sibling"
    );
}

#[tokio::test]
async fn existing_path_remount_rejects_foreign_project_identity() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn owned() {}\n")]);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(1);
    registry
        .mount_worktree(
            ProjectId::new("project.remount.owner").expect("valid owner project"),
            fixture.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("mount owning project");

    let error = registry
        .mount_worktree(
            ProjectId::new("project.remount.foreign").expect("valid foreign project"),
            fixture.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect_err("same path must reject a foreign project");

    assert!(matches!(
        error,
        super::CodeIndexSchedulerErrorV1::Identity(message)
            if message.contains("different project identity")
    ));
    registry.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_same_root_mounts_keep_one_canonical_owner() {
    let fixture = GitFixture::new(&[("src/main.rs", "fn main() {}\n")]);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(4);
    let barrier = Arc::new(tokio::sync::Barrier::new(4));
    let mut tasks = Vec::new();
    for _ in 0..4 {
        let registry = registry.clone();
        let root = fixture.path().to_path_buf();
        let store_root = store.path().to_path_buf();
        let barrier = Arc::clone(&barrier);
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            registry
                .mount_worktree(test_project_id(), &root, store_root, None)
                .await
        }));
    }

    let mut mounted = 0;
    let mut reused = 0;
    for task in tasks {
        if task
            .await
            .expect("mount task joins")
            .expect("mount succeeds")
        {
            mounted += 1;
        } else {
            reused += 1;
        }
    }
    assert_eq!(mounted, 1, "one caller must install the canonical owner");
    assert_eq!(reused, 3, "same-root racers must reuse the canonical owner");
    assert_eq!(
        registry.mounted.lock().await.len(),
        1,
        "the registry must retain exactly one owner after the mount race"
    );

    registry.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn paused_cold_mount_rejects_a_root_retiring_before_final_commit() {
    let fixture = GitFixture::new(&[("src/main.rs", "fn main() {}\n")]);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(2);
    let root = fixture.path().canonicalize().expect("canonical root");
    let (cold_commit_entered, release_cold_commit) = registry
        .pause_next_cold_mount_before_final_commit(root.clone())
        .await;
    let cold_registry = registry.clone();
    let cold_root = fixture.path().to_path_buf();
    let cold_store = store.path().to_path_buf();
    let cold_mount = tokio::spawn(async move {
        cold_registry
            .mount_worktree(test_project_id(), &cold_root, cold_store, None)
            .await
    });

    cold_commit_entered
        .await
        .expect("first cold mount must pause before its final owner commit");

    let roots = BTreeSet::from([root.clone()]);
    assert!(
        !registry
            .retire_project_roots_with_deadline(&roots, Duration::from_millis(25))
            .await,
        "retirement must wait for the paused exact cold reservation"
    );
    assert_eq!(registry.retiring_owner_count().await, 0);

    release_cold_commit
        .send(())
        .expect("release paused cold mount final commit");
    let cold_error = cold_mount
        .await
        .expect("paused cold mount joins")
        .expect_err("a retiring root must reject a stale cold mount final commit");
    assert!(matches!(
        cold_error,
        super::CodeIndexSchedulerErrorV1::Identity(message)
            if message.contains("still retiring")
    ));
    let retiring = registry.retiring.lock().await;
    let mounted = registry.mounted.lock().await;
    assert!(!retiring.contains_key(&root));
    assert!(!mounted.contains_key(&root));
    drop(mounted);
    drop(retiring);

    assert!(
        registry
            .retire_project_roots_with_deadline(&roots, Duration::from_secs(2))
            .await,
        "the completed retired cold reservation must release"
    );
    assert_eq!(registry.retiring_owner_count().await, 0);
    assert!(
        !registry.mounted.lock().await.contains_key(&root),
        "the rejected cold mount must not leave a replacement worker"
    );

    registry.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn retirement_parks_the_incumbent_while_a_same_root_remount_waits_on_its_scheduler() {
    let fixture = GitFixture::new(&[("src/main.rs", "fn main() {}\n")]);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(2);
    let root = fixture.path().canonicalize().expect("canonical root");
    assert!(
        registry
            .mount_worktree(
                test_project_id(),
                fixture.path(),
                store.path().to_path_buf(),
                None,
            )
            .await
            .expect("incumbent mount succeeds")
    );
    let scheduler = registry
        .scheduler_handle(&root)
        .await
        .expect("incumbent scheduler");
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let (release_scheduler_tx, release_scheduler_rx) = std::sync::mpsc::channel();
    let held_scheduler = Arc::clone(&scheduler);
    let lock_thread = std::thread::spawn(move || {
        let scheduler = held_scheduler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let wake = Arc::clone(&scheduler.wake);
        held_tx.send(wake).expect("signal held scheduler");
        release_scheduler_rx.recv().expect("release held scheduler");
    });
    let wake = held_rx.recv().expect("scheduler lock must be held");
    wake.notify_one();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let reconciling = registry.reconcile_in_progress_for_test(&root).await;
            if reconciling {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("incumbent worker blocks in its reconcile pass");

    let replacement_entered = registry
        .observe_next_existing_semantic_schedule_replacement(root.clone())
        .await;
    let remount_registry = registry.clone();
    let remount_root = fixture.path().to_path_buf();
    let remount_store = store.path().to_path_buf();
    let remount = tokio::spawn(async move {
        remount_registry
            .mount_worktree(test_project_id(), &remount_root, remount_store, None)
            .await
    });
    replacement_entered
        .await
        .expect("same-root remount reaches its semantic replacement");

    let roots = BTreeSet::from([root.clone()]);
    let retirement = registry
        .retire_project_roots_with_deadline(&roots, Duration::from_millis(25))
        .await;
    let parked_before_retry = {
        let retiring = registry.retiring.lock().await;
        let mounted = registry.mounted.lock().await;
        (retiring.contains_key(&root), mounted.contains_key(&root))
    };

    release_scheduler_tx
        .send(())
        .expect("release incumbent scheduler");
    lock_thread.join().expect("held scheduler thread joins");
    let remount = remount.await.expect("same-root remount joins");
    let drained = registry
        .retire_project_roots_with_deadline(&roots, Duration::from_secs(2))
        .await;
    let no_owner_remains = {
        let retiring = registry.retiring.lock().await;
        let mounted = registry.mounted.lock().await;
        !retiring.contains_key(&root) && !mounted.contains_key(&root)
    };
    registry.shutdown().await;

    assert!(
        !retirement,
        "retirement must reach and park the incumbent instead of waiting on the remount's registry locks"
    );
    assert!(matches!(
        remount,
        Err(super::CodeIndexSchedulerErrorV1::Identity(message))
            if message.contains("retired while semantic schedule update waited")
    ));
    assert_eq!(
        parked_before_retry,
        (true, false),
        "the retiring incumbent stays parked and the stale remount never installs a replacement"
    );
    assert!(
        drained,
        "the parked incumbent must drain after its scheduler releases"
    );
    assert!(
        no_owner_remains,
        "the refused remount must not leave a mounted or retiring orphan"
    );
}

#[test]
fn empty_generation_restart_preserves_project_identity() {
    // A file with a compiled language descriptor (so the snapshot has something
    // extractable and the reconcile reaches a publish) whose content yields no
    // symbols, so the sealed generation is chunk-empty. `# fixture` used to be
    // that file, but the markdown extractor now chunks headings, which made
    // this fixture produce a non-empty generation and stopped exercising the
    // empty-generation restore this test exists for.
    let fixture = GitFixture::new(&[("README.md", "")]);
    let store = TempDir::new().expect("store root");
    let project_id = ProjectId::new("project.empty-restart").expect("valid project");
    let mut scheduler = CodeIndexWorktreeSchedulerV1::open(
        project_id.clone(),
        fixture.path(),
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    )
    .expect("open scheduler");
    published(scheduler.reconcile_now().expect("publish empty generation"));
    let generation = scheduler.latest_complete().expect("published generation");
    assert!(generation.generation().chunks().chunks().is_empty());
    assert_eq!(generation.generation().manifest().project_id, project_id);
    drop(generation);
    drop(scheduler);

    let reopened = CodeIndexWorktreeSchedulerV1::open(
        project_id.clone(),
        fixture.path(),
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    )
    .expect("reopen scheduler");
    assert_eq!(
        reopened
            .latest_complete()
            .expect("restored generation")
            .generation()
            .manifest()
            .project_id,
        project_id
    );
    drop(reopened);

    let mut foreign = CodeIndexWorktreeSchedulerV1::open(
        ProjectId::new("project.empty-restart.foreign").expect("valid foreign project"),
        fixture.path(),
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    )
    .expect("foreground open defers sealed identity validation");
    let error = foreign
        .activate_or_reconcile()
        .expect_err("persisted generation must reject a foreign project");
    assert!(matches!(
        error,
        super::CodeIndexSchedulerErrorV1::Identity(message)
            if message.contains("different project/worktree identity")
    ));
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
    assert_eq!(superseded._lane_digest, expected._lane_digest);
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
    let latest = scheduler.latest_complete().expect("latest generation");
    let owners = latest
        .production_query_owners()
        .expect("connect production query owners");
    assert!(
        std::mem::size_of_val(&owners.exact) > 0
            && std::mem::size_of_val(&owners.lexical) > 0
            && std::mem::size_of_val(&owners.graph) > 0,
        "exact/lexical/graph production owners must be concrete lane values"
    );
    let same_generation = scheduler.latest_complete().expect("same latest generation");
    assert!(
        Arc::ptr_eq(&latest.query_owners, &same_generation.query_owners),
        "repeated queries must reuse generation-bound query projections"
    );
    assert!(
        same_generation.query_owners.get().is_some(),
        "the shared query projection cache must remain populated"
    );
}

/// The lexical projection build inside `install_query_owners` honors the
/// caller's `RetrievalBudget` verbatim: `Some(0)` is a set deadline that
/// expires immediately as a typed `BudgetExceeded` refusal with nothing
/// memoized, while `None` is unset and keeps the crate build fallback so the
/// same generation still builds and memoizes its owners.
#[test]
fn query_owner_build_honors_set_budget_and_keeps_crate_fallback_when_unset() {
    let fixture = GitFixture::new(&[(
        "src/lib.rs",
        "pub fn caller() { callee(); }\npub fn callee() {}\n",
    )]);
    let store = TempDir::new().expect("store root");
    let bytes = Arc::new(SharedCodeIndexBytePoolV1::default());
    let mut scheduler = scheduler(&fixture, store.path().to_path_buf(), bytes);
    published(scheduler.reconcile_now().expect("publish"));
    let latest = scheduler.latest_complete().expect("latest generation");
    let graph_reader = || {
        crate::code_index::graph_projection::CodeGraphEvidenceReader::new(
            latest.generation().manifest().generation_id.clone(),
            Some(latest.generation().snapshot().repository.clone()),
            latest.source_freshness().expect("source freshness"),
            latest.generation().edges(),
            latest.generation().chunks().chunks(),
        )
        .expect("memory graph reader")
    };

    let expired = RetrievalBudget {
        max_candidates_per_lane: 1,
        max_fused_candidates: 1,
        max_hydrated_results: 1,
        max_hydration_bytes: 1,
        deadline_micros: Some(0),
    };
    let refusal = latest.install_query_owners(
        graph_reader(),
        super::CodeGraphServingAuthorityV1::Memory,
        &expired,
    );
    match refusal {
        Err(tracedecay_query::retrieval::RetrievalPortError::BudgetExceeded) => {}
        Err(error) => {
            panic!("an already-expired build budget is a typed budget refusal: {error:?}")
        }
        Ok(_) => panic!("Some(0) is a set deadline and must expire immediately"),
    }
    assert!(
        latest.query_owners.get().is_none(),
        "a refused build must not be memoized as serving owners"
    );

    let unset = RetrievalBudget {
        deadline_micros: None,
        ..expired
    };
    let owners = latest
        .install_query_owners(
            graph_reader(),
            super::CodeGraphServingAuthorityV1::Memory,
            &unset,
        )
        .expect("an unset deadline keeps the crate build fallback");
    let repeat = latest
        .production_query_owners()
        .expect("built owners are memoized for serving");
    assert!(
        Arc::ptr_eq(&owners, &repeat),
        "every reader must share the one built owner set"
    );
}

/// The generation record index must answer every point lookup exactly as the
/// linear `.iter().find(..)` scans it replaced, including misses, and must be
/// built once per generation rather than once per query.
#[test]
fn generation_record_index_matches_linear_scan_lookups() {
    use std::fmt::Write as _;

    let mut sources = Vec::new();
    for file in 0..24 {
        let mut body = String::new();
        for symbol in 0..6 {
            write!(
                body,
                "pub fn caller_{file}_{symbol}() {{ callee_{file}_{symbol}(); }}\n\
                 pub fn callee_{file}_{symbol}() {{}}\n"
            )
            .expect("write to a string never fails");
        }
        sources.push((format!("src/module_{file}.rs"), body));
    }
    let files = sources
        .iter()
        .map(|(path, body)| (path.as_str(), body.as_str()))
        .collect::<Vec<_>>();
    let fixture = GitFixture::new(&files);
    let store = TempDir::new().expect("store root");
    let bytes = Arc::new(SharedCodeIndexBytePoolV1::default());
    let mut scheduler = scheduler(&fixture, store.path().to_path_buf(), bytes);
    published(scheduler.reconcile_now().expect("publish"));
    let latest = scheduler.latest_complete().expect("latest generation");

    let generation = latest.generation();
    let snapshot_files = &generation.snapshot().files;
    let chunks = generation.chunks().chunks();
    let symbols = &generation.symbols().symbols;
    let edges = generation.edges();
    assert!(
        !snapshot_files.is_empty() && !chunks.is_empty() && !symbols.is_empty(),
        "fixture must publish files, chunks, and symbols to compare"
    );

    let index = latest.record_index();

    for file in snapshot_files {
        let expected = snapshot_files
            .iter()
            .position(|candidate| candidate.file_occurrence_id == file.file_occurrence_id);
        assert_eq!(
            index.file_position(&file.file_occurrence_id),
            expected,
            "indexed file lookup must match the linear scan"
        );
    }

    for chunk in chunks {
        let expected = chunks.iter().position(|candidate| candidate.id == chunk.id);
        assert_eq!(
            index.chunk_position(&chunk.id),
            expected,
            "indexed chunk lookup must match the linear scan"
        );
    }

    for record in symbols {
        let expected = symbols
            .iter()
            .position(|candidate| candidate.occurrence == record.occurrence);
        assert_eq!(
            index.symbol_position(&record.occurrence),
            expected,
            "indexed symbol lookup must match the linear scan"
        );
    }

    let facet_rows = index.kind_facet_rows();
    let expected_facet_rows = symbols
        .iter()
        .enumerate()
        .filter_map(|(symbol_position, symbol)| {
            let chunk = chunks.iter().find(|candidate| {
                candidate.anchor.symbol_occurrence_id.as_ref() == Some(&symbol.occurrence)
            })?;
            let file_position = snapshot_files.iter().position(|candidate| {
                candidate.file_occurrence_id == chunk.anchor.file_occurrence_id
            })?;
            Some((symbol_position, file_position))
        })
        .collect::<Vec<_>>();
    assert_eq!(
        facet_rows, expected_facet_rows,
        "indexed facet rows must match the resolvable linear symbol/file joins"
    );

    for selector in symbols
        .iter()
        .take(12)
        .map(|symbol| symbol.qualified_name.as_str())
    {
        let expected = symbols
            .iter()
            .enumerate()
            .filter(|(_, symbol)| symbol.qualified_name == selector)
            .map(|(position, _)| position)
            .collect::<Vec<_>>();
        assert_eq!(
            index.qualified_name_positions(symbols, selector),
            expected,
            "indexed qualified-name lookup must match the linear scan"
        );
    }

    for selector in symbols
        .iter()
        .take(12)
        .map(|symbol| symbol.qualified_name.rsplit("::").next().expect("segment"))
    {
        let expected = symbols
            .iter()
            .enumerate()
            .filter(|(_, symbol)| symbol.qualified_name.rsplit("::").next() == Some(selector))
            .map(|(position, _)| position)
            .collect::<Vec<_>>();
        assert_eq!(
            index.last_segment_positions(symbols, selector),
            expected,
            "indexed last-segment lookup must match the linear scan"
        );
    }

    for chunk in chunks {
        let Some(symbol) = chunk.anchor.symbol_occurrence_id.as_ref() else {
            continue;
        };
        let file = &chunk.anchor.file_occurrence_id;
        let expected_by_symbol = chunks
            .iter()
            .position(|candidate| candidate.anchor.symbol_occurrence_id.as_ref() == Some(symbol));
        assert_eq!(
            index.chunk_position_for_symbol(symbol),
            expected_by_symbol,
            "indexed symbol-anchored chunk lookup must match the linear scan"
        );
        let expected_by_pair = chunks.iter().position(|candidate| {
            &candidate.anchor.file_occurrence_id == file
                && candidate.anchor.symbol_occurrence_id.as_ref() == Some(symbol)
        });
        assert_eq!(
            index.chunk_position_for_file_symbol(file, symbol),
            expected_by_pair,
            "indexed file+symbol chunk lookup must match the linear scan"
        );
    }

    let incident_symbols = edges
        .iter()
        .flat_map(|edge| [edge.from_occurrence.clone(), edge.to_occurrence.clone()])
        .collect::<BTreeSet<_>>();
    for symbol in &incident_symbols {
        for reverse in [false, true] {
            let expected = edges
                .iter()
                .enumerate()
                .filter(|(_, edge)| {
                    if reverse {
                        &edge.to_occurrence == symbol
                    } else {
                        &edge.from_occurrence == symbol
                    }
                })
                .map(|(position, _)| position)
                .collect::<Vec<_>>();
            assert_eq!(
                index.incident_edge_positions(symbol, reverse),
                expected.as_slice(),
                "indexed adjacency must match the linear edge scan in order"
            );
        }
    }

    let missing_file = tracedecay_domain::FileOccurrenceId::new("absent-file-occurrence")
        .expect("valid file occurrence id");
    let missing_chunk =
        tracedecay_domain::CodeSearchChunkId::new("absent-chunk").expect("valid chunk id");
    let missing_symbol = tracedecay_domain::SymbolOccurrenceId::new("absent-symbol-occurrence")
        .expect("valid symbol occurrence id");
    assert_eq!(index.file_position(&missing_file), None);
    assert_eq!(index.chunk_position(&missing_chunk), None);
    assert_eq!(index.symbol_position(&missing_symbol), None);
    assert_eq!(index.chunk_position_for_symbol(&missing_symbol), None);
    assert_eq!(
        index.chunk_position_for_file_symbol(&missing_file, &missing_symbol),
        None
    );
    assert!(
        index
            .incident_edge_positions(&missing_symbol, false)
            .is_empty()
    );
    assert!(
        index
            .incident_edge_positions(&missing_symbol, true)
            .is_empty()
    );

    let same_generation = scheduler.latest_complete().expect("same latest generation");
    assert!(
        Arc::ptr_eq(&latest.record_index, &same_generation.record_index),
        "repeated queries must reuse the generation-bound record index"
    );
    assert!(
        same_generation.record_index.get().is_some(),
        "the shared record index must stay built across queries"
    );
}

#[tokio::test]
async fn core_query_profile_composes_live_code_index_lanes() {
    let fixture = GitFixture::new(&[("src/main.rs", "fn main() {}\n")]);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(1);
    registry
        .mount_worktree(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("mount daemon-owned scheduler");
    wait_for_initial_generation(&registry, fixture.path()).await;
    let latest = registry
        .latest_complete_fresh(fixture.path())
        .await
        .expect("live generation");
    let snapshot = latest.generation.snapshot();
    let scope = ResolvedScope::new(
        test_project_id(),
        snapshot.repository.clone(),
        snapshot.worktree.clone().expect("worktree id"),
        snapshot.reference.clone(),
    )
    .expect("resolved scope");
    let authority = query_authority(latest.generation.manifest().privacy_domain.clone());
    registry
        .mount_query_authority(fixture.path(), &scope, authority)
        .await
        .expect("mount core query authority");

    let request = super::query_runtime::QuerySearchExecutionRequestV1::new(
        "main",
        super::query_runtime::QuerySearchExecutionPolicyV1 {
            principal: PrincipalId::new("principal.core-query.fixture").expect("principal"),
            authorization_revision: AuthorizationRevision::new("authorization.core-query.fixture")
                .expect("authorization revision"),
            sanitizer_revision: SanitizerRevision::new(
                tracedecay_query::retrieval::QUERY_SANITIZER_REVISION_V1,
            )
            .expect("sanitizer revision"),
            normalization_revision: QueryNormalizationRevision::new(
                tracedecay_query::retrieval::QUERY_NORMALIZATION_REVISION_V1,
            )
            .expect("normalization revision"),
            exact_rule_revision: ExactAdmissionRuleRevision::new(
                tracedecay_query::retrieval::QUERY_EXACT_RULE_REVISION_V1,
            )
            .expect("exact rules revision"),
            lexical_profile_revision: ComponentRevision::new(
                tracedecay_query::retrieval::QUERY_LEXICAL_PROFILE_REVISION_V1,
            )
            .expect("lexical profile revision"),
            lexical_score_domain: ScoreDomainId::new(
                tracedecay_query::retrieval::QUERY_LEXICAL_SCORE_DOMAIN_V1,
            )
            .expect("lexical score domain"),
            fuzzy_budget: tracedecay_query::retrieval::lexical::MAX_FUZZY_TERM_EXPANSIONS_V1,
            graph_edge_kinds: vec![RelationEdgeKindV1::Calls],
            graph_max_depth: 1,
            page_size: 10,
            cursor: None,
        },
    );
    let executed = registry
        .execute_query_search(&scope, request)
        .await
        .expect("core query composes live lanes");
    assert!(
        !executed.authorized.fallback.ordered_candidates.is_empty(),
        "live main symbol is returned"
    );
    assert!(
        !executed.served_stale,
        "a ready generation serves the fresh path and is never marked stale"
    );
    registry.shutdown().await;
}

#[tokio::test]
async fn query_authority_lookup_preserves_real_mount_identity_isolation() {
    let primary = GitFixture::new(&[("src/main.rs", "fn main() {}\n")]);
    let linked_root = TempDir::new().expect("linked worktree root");
    let linked = linked_root.path().join("linked");
    let linked_arg = linked.to_str().expect("linked worktree path");
    git(
        primary.path(),
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "linked-query",
            linked_arg,
            "main",
        ],
    );
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(2);
    for root in [primary.path(), linked.as_path()] {
        registry
            .mount_worktree(test_project_id(), root, store.path().to_path_buf(), None)
            .await
            .expect("mount real sibling worktree");
    }

    let scope_for = |root: &Path| {
        let identity =
            super::identity::IndexingIdentityV1::resolve(root).expect("mounted worktree identity");
        ResolvedScope::new(
            test_project_id(),
            identity.repository_id().clone(),
            identity.worktree_id().clone(),
            identity.head_ref().cloned(),
        )
        .expect("resolved scope")
    };
    let primary_scope = scope_for(primary.path());
    let linked_scope = scope_for(&linked);
    assert_eq!(primary_scope.repository_id, linked_scope.repository_id);
    assert_ne!(primary_scope.worktree_id, linked_scope.worktree_id);

    registry
        .mount_query_authority(
            &linked,
            &linked_scope,
            query_authority(
                PrivacyDomainId::new("privacy.query-authority-linked")
                    .expect("linked privacy domain"),
            ),
        )
        .await
        .expect("mount linked query authority");
    assert!(!registry.has_query_authority_for_scope(&primary_scope).await);
    assert!(registry.has_query_authority_for_scope(&linked_scope).await);

    registry
        .mount_query_authority(
            primary.path(),
            &primary_scope,
            query_authority(
                PrivacyDomainId::new("privacy.query-authority-primary")
                    .expect("primary privacy domain"),
            ),
        )
        .await
        .expect("mount primary query authority");
    assert!(registry.has_query_authority_for_scope(&primary_scope).await);
    assert!(registry.has_query_authority_for_scope(&linked_scope).await);

    registry.shutdown().await;
}

/// Build the core-authority search policy shared by the
/// stale-while-revalidate tests.
fn core_search_request(query: &str) -> super::query_runtime::QuerySearchExecutionRequestV1 {
    super::query_runtime::QuerySearchExecutionRequestV1::new(
        query,
        super::query_runtime::QuerySearchExecutionPolicyV1 {
            principal: PrincipalId::new("principal.stale-serving.fixture").expect("principal"),
            authorization_revision: AuthorizationRevision::new(
                "authorization.stale-serving.fixture",
            )
            .expect("authorization revision"),
            sanitizer_revision: SanitizerRevision::new(
                tracedecay_query::retrieval::QUERY_SANITIZER_REVISION_V1,
            )
            .expect("sanitizer revision"),
            normalization_revision: QueryNormalizationRevision::new(
                tracedecay_query::retrieval::QUERY_NORMALIZATION_REVISION_V1,
            )
            .expect("normalization revision"),
            exact_rule_revision: ExactAdmissionRuleRevision::new(
                tracedecay_query::retrieval::QUERY_EXACT_RULE_REVISION_V1,
            )
            .expect("exact rules revision"),
            lexical_profile_revision: ComponentRevision::new(
                tracedecay_query::retrieval::QUERY_LEXICAL_PROFILE_REVISION_V1,
            )
            .expect("lexical profile revision"),
            lexical_score_domain: ScoreDomainId::new(
                tracedecay_query::retrieval::QUERY_LEXICAL_SCORE_DOMAIN_V1,
            )
            .expect("lexical score domain"),
            fuzzy_budget: tracedecay_query::retrieval::lexical::MAX_FUZZY_TERM_EXPANSIONS_V1,
            graph_edge_kinds: vec![RelationEdgeKindV1::Calls],
            graph_max_depth: 1,
            page_size: 10,
            cursor: None,
        },
    )
}

/// Mount one worktree, publish an initial generation, and mount the core
/// query authority for its exact scope.
async fn mounted_core_query_worktree(
    fixture: &GitFixture,
    store: &TempDir,
) -> (CodeIndexSchedulerRegistryV1, ResolvedScope) {
    mounted_core_query_worktree_in(CodeIndexSchedulerRegistryV1::new(1), fixture, store).await
}

/// The same worktree as [`mounted_core_query_worktree`], mounted into a
/// registry whose background-reconcile admission is bounded to one permit so a
/// test can occupy it and hold the worker at its dequeue point.
async fn mounted_core_query_worktree_with_one_permit(
    fixture: &GitFixture,
    store: &TempDir,
) -> (CodeIndexSchedulerRegistryV1, ResolvedScope) {
    mounted_core_query_worktree_in(
        CodeIndexSchedulerRegistryV1::with_background_reconcile_permits(1, 1),
        fixture,
        store,
    )
    .await
}

async fn mounted_core_query_worktree_in(
    registry: CodeIndexSchedulerRegistryV1,
    fixture: &GitFixture,
    store: &TempDir,
) -> (CodeIndexSchedulerRegistryV1, ResolvedScope) {
    registry
        .mount_worktree(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("mount daemon-owned scheduler");
    wait_for_initial_generation(&registry, fixture.path()).await;
    let latest = registry
        .latest_complete_fresh(fixture.path())
        .await
        .expect("live generation");
    let snapshot = latest.generation.snapshot();
    let scope = ResolvedScope::new(
        test_project_id(),
        snapshot.repository.clone(),
        snapshot.worktree.clone().expect("worktree id"),
        snapshot.reference.clone(),
    )
    .expect("resolved scope");
    mount_core_query_authority(&registry, fixture.path(), &scope, &latest).await;
    (registry, scope)
}

/// Mount the core query authority for one exact scope against an
/// already-mounted worktree. The authority slot is keyed by the scope digest,
/// so remounting under a different reference is exactly what a daemon does when
/// it opens a project whose HEAD has moved since the retained generation was
/// sealed.
async fn mount_core_query_authority(
    registry: &CodeIndexSchedulerRegistryV1,
    project_root: &Path,
    scope: &ResolvedScope,
    latest: &super::LatestCompleteCodeIndexV1,
) {
    let authority = query_authority(latest.generation.manifest().privacy_domain.clone());
    registry
        .mount_query_authority(project_root, scope, authority)
        .await
        .expect("mount core query authority");
}

/// The same repository and worktree under a reference the admitted scope has
/// already moved past — the shape every restored generation has after the
/// ordinary commit/branch-then-restart cycle.
fn moved_reference_scope(scope: &ResolvedScope) -> ResolvedScope {
    ResolvedScope::new(
        scope.project_id.clone(),
        scope.repository_id.clone(),
        scope.worktree_id.clone(),
        Some(RefId::new("refs/heads/moved-after-seal").expect("moved reference")),
    )
    .expect("moved scope")
}

/// The defect this covers: during any generation rebuild the ready gate
/// abstains, and search used to collapse into `GenerationUnavailable` for the
/// whole window while callers/grep/context kept serving. Holding the scheduler
/// mutex reproduces exactly that window — the background worker owns the
/// scheduler, so the ready gate's `try_lock` abstains — while the last complete
/// generation stays in `serving_generation`.
// Holding the scheduler guard across the awaits is the scenario, not an
// oversight: it is how this test occupies the rebuild window that the fallback
// exists to serve through. The guard is released before shutdown.
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn search_serves_the_last_complete_generation_while_the_scheduler_rebuilds() {
    let fixture = GitFixture::new(&[("src/main.rs", "fn main() {}\n")]);
    let store = TempDir::new().expect("store root");
    let (registry, scope) = mounted_core_query_worktree(&fixture, &store).await;

    // Baseline: the ready path, byte-for-byte, before anything is degraded.
    let fresh = registry
        .execute_query_search(&scope, core_search_request("main"))
        .await
        .expect("ready generation serves the fresh path");
    assert!(!fresh.served_stale);
    let fresh_generation = fresh.generation.clone();
    let fresh_candidates = fresh.authorized.fallback.ordered_candidates.clone();
    assert!(!fresh_candidates.is_empty(), "live main symbol is returned");

    // Enter the rebuild window: the scheduler is owned elsewhere, so the ready
    // gate cannot admit a current generation.
    let scheduler = {
        let mounted = registry.mounted.lock().await;
        Arc::clone(
            &mounted
                .get(&fixture.path().canonicalize().expect("canonical root"))
                .expect("mounted worktree")
                .scheduler,
        )
    };
    let held = scheduler
        .lock()
        .expect("hold the scheduler as a rebuild would");
    assert!(
        registry
            .latest_complete_ready_for_scope(&scope)
            .await
            .is_none(),
        "the ready gate abstains for the whole rebuild window"
    );
    assert!(
        registry
            .latest_complete_serving_for_scope(&scope)
            .await
            .is_some(),
        "the last complete generation is still held and needs no re-read"
    );

    let stale = registry
        .execute_query_search(&scope, core_search_request("main"))
        .await
        .expect("search keeps serving through the rebuild instead of failing");
    assert!(
        stale.served_stale,
        "a fallback answer must be reported stale, never as current"
    );
    assert_eq!(
        stale.generation, fresh_generation,
        "the stale answer names the complete generation that actually answered"
    );
    assert_eq!(
        stale.authorized.fallback.ordered_candidates, fresh_candidates,
        "serving stale changes only the coverage marker, not ranking identity"
    );

    // The coverage marker the executor derives from this flag.
    let coverage = tracedecay_query::code_search::CodeIndexSearchCoverageV1::fused_stale(
        stale.generation.as_str(),
        &tracedecay_query::code_search::CodeIndexSemanticStatusV1::Complete,
    );
    assert!(coverage.any_servable(), "a stale answer is still servable");
    assert!(
        coverage.is_degraded(),
        "a stale answer says recall is partial"
    );
    assert_eq!(
        coverage.exact,
        tracedecay_query::code_search::CodeIndexLaneStatusV1::Stale {
            generation: fresh_generation.as_str().to_owned(),
        }
    );

    drop(held);
    registry.shutdown().await;
}

/// The resolution-order defect: asking the ready gate first made every query
/// queue on the single-flight decode of the generation being activated, so a
/// query holding a perfectly servable generation still paid an O(store) sweep
/// before it could reach the stale fallback. Await-new must never preempt
/// serve-old.
///
/// This occupies the decode barrier exactly as activation of a new generation
/// does — pinned slot empty, one decode in flight — and deliberately leaves the
/// scheduler mutex FREE, so the ready gate is admitted and would park inside it.
#[tokio::test]
async fn search_never_awaits_an_in_flight_decode_while_a_generation_is_servable() {
    let fixture = GitFixture::new(&[("src/main.rs", "fn main() {}\n")]);
    let store = TempDir::new().expect("store root");
    let (registry, scope) = mounted_core_query_worktree(&fixture, &store).await;

    // Baseline: the ready path admits, byte-for-byte, before anything degrades.
    let fresh = registry
        .execute_query_search(&scope, core_search_request("main"))
        .await
        .expect("ready generation serves the fresh path");
    assert!(!fresh.served_stale);
    let fresh_generation = fresh.generation.clone();
    let fresh_candidates = fresh.authorized.fallback.ordered_candidates.clone();
    assert!(!fresh_candidates.is_empty(), "live main symbol is returned");

    let scheduler = {
        let mounted = registry.mounted.lock().await;
        Arc::clone(
            &mounted
                .get(&fixture.path().canonicalize().expect("canonical root"))
                .expect("mounted worktree")
                .scheduler,
        )
    };
    let (held_decode, decodes_before) = {
        let scheduler = scheduler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (
            scheduler.hold_active_decode(),
            scheduler.sealed_decode_count(),
        )
    };
    assert!(
        registry
            .latest_complete_serving_for_scope(&scope)
            .await
            .is_some(),
        "the last complete generation is still held and needs no decode"
    );

    let stale = tokio::time::timeout(
        Duration::from_secs(30),
        registry.execute_query_search(&scope, core_search_request("main")),
    )
    .await
    .expect("a servable generation must never queue on the decode barrier")
    .expect("search serves through the activation window");
    assert!(
        stale.served_stale,
        "a fallback answer must be reported stale, never as current"
    );
    assert_eq!(
        stale.generation, fresh_generation,
        "the stale answer names the complete generation that actually answered"
    );
    assert_eq!(
        stale.authorized.fallback.ordered_candidates, fresh_candidates,
        "serving stale changes only the coverage marker, not ranking identity"
    );
    assert_eq!(
        scheduler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .sealed_decode_count(),
        decodes_before,
        "the serving path must not enter the sealed decode at all"
    );

    drop(held_decode);
    registry.shutdown().await;
}

/// Fail-closed: the fallback serves a *retained complete* generation, never a
/// missing one. With no mounted worktree neither resolver can produce one, and
/// the typed fail-fast is preserved rather than degraded into an empty answer.
#[tokio::test]
async fn search_fails_fast_when_no_complete_generation_exists() {
    let fixture = GitFixture::new(&[("src/main.rs", "fn main() {}\n")]);
    let store = TempDir::new().expect("store root");
    let (registry, scope) = mounted_core_query_worktree(&fixture, &store).await;

    let empty = CodeIndexSchedulerRegistryV1::new(1);
    assert!(
        empty
            .latest_complete_serving_for_scope(&scope)
            .await
            .is_none(),
        "an unmounted scope has no retained generation to serve"
    );
    // `ExecutedQuerySearchV1` intentionally omits `Debug` (it carries the
    // sanitized query), so assert on the error arm directly.
    match empty
        .execute_query_search(&scope, core_search_request("main"))
        .await
    {
        Err(super::query_runtime::QuerySearchExecutionErrorV1::GenerationUnavailable) => {}
        Err(other) => panic!("expected the typed fail-fast, got {other:?}"),
        Ok(_) => panic!("absent generations must not be degraded into a stale answer"),
    }

    empty.shutdown().await;
    registry.shutdown().await;
}

/// The live outage this covers: a scope's branch label moves — a restored
/// generation was sealed before a `git switch`, or a retained route scope
/// pinned the label that was live at project open — while the worktree the
/// daemon is serving stays byte-identical. The label is not checkout
/// identity: the ready ladder has already verified the generation against
/// the live worktree, so the exact worktree's own graph must keep serving as
/// current instead of degrading to stale (queries) or `Unavailable` (graph
/// reads and the runtime census, which have no stale arm and were orphaned
/// until the route reopened).
#[tokio::test]
async fn moved_reference_label_still_serves_the_exact_worktree_as_current() {
    let fixture = GitFixture::new(&[("src/main.rs", "fn main() {}\n")]);
    let store = TempDir::new().expect("store root");
    let (registry, scope) = mounted_core_query_worktree(&fixture, &store).await;

    // Baseline under the sealed reference: the ready path, byte-for-byte.
    let fresh = registry
        .execute_query_search(&scope, core_search_request("main"))
        .await
        .expect("ready generation serves the fresh path");
    assert!(!fresh.served_stale);
    let fresh_generation = fresh.generation.clone();
    let fresh_candidates = fresh.authorized.fallback.ordered_candidates.clone();
    assert!(!fresh_candidates.is_empty(), "live main symbol is returned");

    // The scope's label moves. Nothing about the worktree changes, and the
    // daemon mounts the authority for the *new* scope exactly as project open
    // does.
    let moved = moved_reference_scope(&scope);
    let latest = registry
        .latest_complete_fresh(fixture.path())
        .await
        .expect("retained generation");
    mount_core_query_authority(&registry, fixture.path(), &moved, &latest).await;

    let ready = registry
        .latest_complete_ready_for_scope(&moved)
        .await
        .expect("a moved label must not orphan the exact worktree's ready generation");
    assert_eq!(
        ready.generation.manifest().generation_id,
        fresh_generation,
        "the ready gate serves the generation verified against this worktree"
    );
    // Attribution is generation-bound, not scope-bound: the served generation
    // still names its own sealed reference, so the answer is attributed to the
    // revision that produced it rather than to the scope that asked.
    assert_ne!(
        ready.generation.snapshot().reference,
        moved.reference,
        "the served generation keeps its own sealed reference"
    );

    let answered = registry
        .execute_query_search(&moved, core_search_request("main"))
        .await
        .expect("search survives a moved reference label instead of failing");
    assert!(
        !answered.served_stale,
        "a byte-identical worktree is current regardless of the label the scope carries"
    );
    assert_eq!(
        answered.generation, fresh_generation,
        "the answer names the complete generation that actually answered"
    );
    assert_eq!(
        answered.authorized.fallback.ordered_candidates, fresh_candidates,
        "the label move changes nothing about ranking identity"
    );

    // The grep/context/callers ladder survives the same move.
    let ladder = registry
        .latest_complete_fresh_for_scope(&moved)
        .await
        .expect("the callable-code ladder also serves through a moved reference");
    assert_eq!(ladder.generation.manifest().generation_id, fresh_generation);

    // The root-scope ready gate behind graph reads and the runtime census —
    // the arms with no stale fallback — must not be orphaned either. Seating
    // races the publication event, so the gate is polled bounded.
    let decoded = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(decoded) = registry
                .latest_complete_ready_decoded_for_root_scope(fixture.path(), &moved)
                .await
            {
                break decoded;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("graph reads and the census survive a moved reference label");
    assert_eq!(
        decoded.generation.manifest().generation_id,
        fresh_generation
    );

    registry.shutdown().await;
}

/// A graph publication `Conflict` is a lifecycle or compare-and-swap race
/// (a runtime mid-close/retire, a concurrent publisher, a superseded head),
/// never evidence about the sealed payload. Classifying it terminal turned
/// one race into a permanent outage: the seat pass gave up stale serving,
/// every later reconcile hit the same race, and the route answered
/// `generation_unverified` until the daemon restarted.
#[test]
fn graph_publication_conflict_re_arms_activation_instead_of_orphaning_serving() {
    use crate::code_index::graph_projection::CodeGraphProjectionError;

    assert!(
        super::CodeIndexSchedulerErrorV1::GraphProjection(CodeGraphProjectionError::Conflict)
            .is_retryable_activation(),
        "a publication conflict leaves the sealed artifact intact and must retry with backoff"
    );
    assert!(
        !super::CodeIndexSchedulerErrorV1::GraphProjection(CodeGraphProjectionError::Corrupt(
            "sealed payload mismatch".to_owned()
        ))
        .is_retryable_activation(),
        "payload corruption stays terminal so reconcile can rebuild"
    );
}

/// The serving gates are relaxed on `reference` only. A different repository
/// or a different worktree is a different checkout identity and must stay
/// unservable, or an answer would be mis-attributed rather than merely old.
#[tokio::test]
async fn serving_arms_still_refuse_a_different_worktree_identity() {
    let fixture = GitFixture::new(&[("src/main.rs", "fn main() {}\n")]);
    let store = TempDir::new().expect("store root");
    let (registry, scope) = mounted_core_query_worktree(&fixture, &store).await;

    let foreign = ResolvedScope::new(
        scope.project_id.clone(),
        scope.repository_id.clone(),
        WorktreeId::new("worktree.some-other-checkout").expect("worktree id"),
        scope.reference.clone(),
    )
    .expect("foreign scope");
    assert!(
        registry
            .latest_complete_serving_for_scope(&foreign)
            .await
            .is_none(),
        "a different worktree identity never inherits a retained generation"
    );
    assert!(
        registry
            .latest_complete_fresh_for_scope(&foreign)
            .await
            .is_none(),
        "the callable-code ladder refuses a different worktree identity too"
    );
    assert!(
        registry
            .latest_complete_ready_for_scope(&foreign)
            .await
            .is_none(),
        "the ready gate refuses a different worktree identity"
    );
    assert!(
        registry
            .latest_complete_ready_decoded_for_root_scope(fixture.path(), &foreign)
            .await
            .is_none(),
        "graph reads and the census refuse a different worktree identity truthfully"
    );

    registry.shutdown().await;
}

#[tokio::test]
async fn foreign_serving_generation_replacement_rejects_stale_rollback_token() {
    let fixture = GitFixture::new(&[("src/main.rs", "fn main() {}\n")]);
    let store = TempDir::new().expect("store root");
    let (registry, _scope) = mounted_core_query_worktree(&fixture, &store).await;
    let original = registry
        .serving_code_scope(fixture.path())
        .await
        .and_then(|scope| scope.serving_generation)
        .expect("initial retained generation");
    let original_id = original.manifest().generation_id.clone();
    let ServingGenerationInstallationOutcomeV1::Installed(original_installation) = registry
        .install_exact_serving_generation(fixture.path(), &original)
        .await
    else {
        panic!("the initial serving generation must admit one exact owner")
    };

    fixture.edit(
        "src/main.rs",
        "fn main() { refreshed(); }\nfn refreshed() {}\n",
    );
    git(fixture.path(), &["add", "src/main.rs"]);
    git(
        fixture.path(),
        &["commit", "-qm", "refresh retained generation"],
    );
    assert!(
        registry
            .notify_path(fixture.path(), fixture.path().join("src/main.rs"))
            .await,
        "the mounted worktree must accept a refresh hint"
    );
    let newer = wait_for_generation_change(&registry, fixture.path(), &original_id).await;

    assert_eq!(
        registry
            .retire_owned_serving_generation(fixture.path(), original_installation)
            .await,
        ServingGenerationRollbackOutcomeV1::NoMatch,
        "a foreign replacement must invalidate the original installation token"
    );
    assert_eq!(
        registry.latest_generation_id(fixture.path()).await,
        Some(newer.clone())
    );
    let newer_generation = registry
        .serving_code_scope(fixture.path())
        .await
        .and_then(|scope| scope.serving_generation)
        .expect("newer retained generation");
    let ServingGenerationInstallationOutcomeV1::Installed(newer_installation) = registry
        .install_exact_serving_generation(fixture.path(), &newer_generation)
        .await
    else {
        panic!("the newer serving generation must admit a fresh owner")
    };
    assert_eq!(
        registry
            .retire_owned_serving_generation(fixture.path(), newer_installation)
            .await,
        ServingGenerationRollbackOutcomeV1::Cleared
    );
    assert!(
        registry
            .latest_generation_id(fixture.path())
            .await
            .is_none(),
        "the exact failed generation must be retired"
    );

    registry.shutdown().await;
}

#[tokio::test]
async fn committed_serving_generation_installation_preserves_the_exact_generation() {
    let fixture = GitFixture::new(&[("src/main.rs", "fn main() {}\n")]);
    let store = TempDir::new().expect("store root");
    let (registry, _scope) = mounted_core_query_worktree(&fixture, &store).await;
    let generation = registry
        .serving_code_scope(fixture.path())
        .await
        .and_then(|scope| scope.serving_generation)
        .expect("initial retained generation");
    let generation_id = generation.manifest().generation_id.clone();
    let ServingGenerationInstallationOutcomeV1::Installed(installation) = registry
        .install_exact_serving_generation(fixture.path(), &generation)
        .await
    else {
        panic!("the exact serving generation must admit an installation token")
    };
    assert_eq!(
        registry
            .commit_serving_generation_installation(fixture.path(), installation)
            .await,
        ServingGenerationRollbackOutcomeV1::Cleared
    );
    assert_eq!(
        registry.latest_generation_id(fixture.path()).await,
        Some(generation_id),
        "committing metadata ownership must retain the exact serving generation"
    );
    registry.shutdown().await;
}

#[tokio::test]
async fn abandoned_serving_generation_installation_releases_the_exact_replay_claim() {
    let fixture = GitFixture::new(&[("src/main.rs", "fn main() {}\n")]);
    let store = TempDir::new().expect("store root");
    let (registry, _scope) = mounted_core_query_worktree(&fixture, &store).await;
    let generation = registry
        .serving_code_scope(fixture.path())
        .await
        .and_then(|scope| scope.serving_generation)
        .expect("initial retained generation");
    let generation_id = generation.manifest().generation_id.clone();
    let ServingGenerationInstallationOutcomeV1::Installed(abandoned) = registry
        .install_exact_serving_generation(fixture.path(), &generation)
        .await
    else {
        panic!("the initial serving generation must admit one exact owner")
    };

    drop(abandoned);

    let ServingGenerationInstallationOutcomeV1::Installed(replay) = registry
        .install_exact_serving_generation(fixture.path(), &generation)
        .await
    else {
        panic!("dropping an unfinished installation must release the replay claim")
    };
    assert_eq!(
        registry
            .commit_serving_generation_installation(fixture.path(), replay)
            .await,
        ServingGenerationRollbackOutcomeV1::Cleared
    );
    assert_eq!(
        registry.latest_generation_id(fixture.path()).await,
        Some(generation_id),
        "releasing an abandoned claim must never clear the serving generation"
    );

    registry.shutdown().await;
}

#[tokio::test]
async fn cancelled_serving_generation_installation_releases_the_exact_replay_claim() {
    let fixture = GitFixture::new(&[("src/main.rs", "fn main() {}\n")]);
    let store = TempDir::new().expect("store root");
    let (registry, _scope) = mounted_core_query_worktree(&fixture, &store).await;
    let generation = registry
        .serving_code_scope(fixture.path())
        .await
        .and_then(|scope| scope.serving_generation)
        .expect("initial retained generation");
    let generation_id = generation.manifest().generation_id.clone();
    let task_registry = registry.clone();
    let task_root = fixture.path().to_path_buf();
    let task_generation = Arc::clone(&generation);
    let (claimed, claimed_observed) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let ServingGenerationInstallationOutcomeV1::Installed(_installation) = task_registry
            .install_exact_serving_generation(&task_root, &task_generation)
            .await
        else {
            panic!("the initial serving generation must admit one exact owner")
        };
        claimed.send(()).expect("publish exact installation claim");
        std::future::pending::<()>().await;
        drop(_installation);
    });
    claimed_observed
        .await
        .expect("installation task must hold the claim before cancellation");
    task.abort();
    let _ = task.await;

    let ServingGenerationInstallationOutcomeV1::Installed(replay) = registry
        .install_exact_serving_generation(fixture.path(), &generation)
        .await
    else {
        panic!("cancelling an installation task must release the exact replay claim")
    };
    assert_eq!(
        registry
            .commit_serving_generation_installation(fixture.path(), replay)
            .await,
        ServingGenerationRollbackOutcomeV1::Cleared
    );
    assert_eq!(
        registry.latest_generation_id(fixture.path()).await,
        Some(generation_id),
        "cancelling the claim owner must leave its serving generation intact"
    );

    registry.shutdown().await;
}

/// The second half of the outage: search resolves its generation without ever
/// running the freshness ladder, so when both arms came up empty nothing
/// requested the reconcile that would remedy it — the typed failure repeated
/// forever. Search must now ask for its own remedy, exactly once per due
/// window, and must still never reconcile inline or park.
// Holding the scheduler guard across the awaits is the scenario: it is how this
// test occupies the window where neither arm can resolve. Released before
// shutdown.
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn search_requests_one_background_reconcile_when_nothing_is_servable() {
    let fixture = GitFixture::new(&[("src/main.rs", "fn main() {}\n")]);
    let store = TempDir::new().expect("store root");
    let (registry, scope) = mounted_core_query_worktree_with_one_permit(&fixture, &store).await;

    // Occupy the single background-reconcile permit so the worker parks at its
    // dequeue point and the pending wake stays observable for the whole test.
    let admission = registry
        .background_reconcile_admission()
        .acquire_owned()
        .await
        .expect("background reconcile admission");

    let scheduler = {
        let mounted = registry.mounted.lock().await;
        Arc::clone(
            &mounted
                .get(&fixture.path().canonicalize().expect("canonical root"))
                .expect("mounted worktree")
                .scheduler,
        )
    };
    // Nothing retained and the scheduler owned elsewhere: both arms come up
    // empty, which is the exact None/None admission that used to be silent.
    registry.clear_serving_generation_for_scope(&scope).await;
    let held = scheduler
        .lock()
        .expect("hold the scheduler as a rebuild would");
    registry.clear_pending_wake_for_scope(&scope).await;

    match registry
        .execute_query_search(&scope, core_search_request("main"))
        .await
    {
        Err(super::query_runtime::QuerySearchExecutionErrorV1::GenerationUnverified) => {}
        Err(other) => panic!("expected the typed unverified state, got {other:?}"),
        Ok(_) => panic!("absent generations must not be degraded into an answer"),
    }
    let stamped = registry
        .pending_wake_micros_for_scope(&scope)
        .await
        .expect("mounted worktree");
    assert_ne!(
        stamped, 0,
        "a search that resolved to nothing must request the rebuild that remedies it"
    );

    // Repeated failing searches inside the same due window must not storm the
    // worker: the outstanding wake already is the remedy they would ask for.
    for _ in 0..4 {
        let _ = registry
            .execute_query_search(&scope, core_search_request("main"))
            .await;
        assert!(
            !registry.request_query_background_reconcile(&scope).await,
            "an outstanding wake must debounce every further admission"
        );
        assert_eq!(
            registry
                .pending_wake_micros_for_scope(&scope)
                .await
                .expect("mounted worktree"),
            stamped,
            "the pending arrival must keep the first admission's instant"
        );
    }

    // A fresh due window (the worker claimed the wake) admits exactly one more.
    registry.clear_pending_wake_for_scope(&scope).await;
    assert!(
        registry.request_query_background_reconcile(&scope).await,
        "a new due window admits one request"
    );
    assert!(
        !registry.request_query_background_reconcile(&scope).await,
        "and only one"
    );

    drop(held);
    // Release the worker before shutdown: it is parked on this permit, and
    // `shutdown` joins its task.
    drop(admission);
    registry.shutdown().await;
}

#[tokio::test]
async fn dashboard_freshness_projects_the_mounted_scheduler_generation() {
    let fixture = GitFixture::new(&[("src/main.rs", "fn main() {}\n")]);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(1);
    registry
        .mount_worktree(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("mount daemon-owned scheduler");
    wait_for_initial_generation(&registry, fixture.path()).await;

    let projected = registry
        .dashboard_freshness(fixture.path())
        .await
        .expect("mounted scheduler projection");
    let latest = registry
        .latest_complete_fresh(fixture.path())
        .await
        .expect("latest generation");

    assert_eq!(
        projected.latest_generation_id.as_deref(),
        Some(latest.generation.manifest().generation_id.as_str())
    );
    assert_eq!(projected.staleness_state.as_deref(), Some("fresh"));
    assert_eq!(projected.coverage, "complete");
}

/// A dashboard status view reports the last execution-owned scheduler state; it
/// must not run the freshness ladder, wake a worker, or publish an out-of-band
/// source change merely because an operator opened the view.
#[tokio::test]
async fn dashboard_freshness_does_not_reconcile_an_out_of_band_change() {
    let fixture = GitFixture::new(&[("src/main.rs", "fn main() { println!(\"v1\"); }\n")]);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(1);
    registry
        .mount_worktree(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("mount daemon-owned scheduler");
    let initial = wait_for_initial_generation(&registry, fixture.path()).await;

    fixture.edit("src/main.rs", "fn main() { println!(\"v2\"); }\n");
    git(fixture.path(), &["commit", "-qam", "out-of-band"]);

    let projected = registry
        .dashboard_freshness(fixture.path())
        .await
        .expect("mounted scheduler projection");

    assert_eq!(
        projected.latest_generation_id.as_deref(),
        Some(initial.as_str()),
        "a status read must not publish the changed source generation"
    );
    registry.shutdown().await;
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
fn restored_generation_abstains_and_schedules_background_truth() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let store = TempDir::new().expect("store root");
    let bytes = Arc::new(SharedCodeIndexBytePoolV1::default());
    {
        let mut scheduler = scheduler(&fixture, store.path().to_path_buf(), Arc::clone(&bytes));
        published(scheduler.reconcile_now().expect("initial publish"));
    }

    // Simulate a generation sealed WITHOUT a restore-time freshness witness (an
    // older daemon, or a witness that never landed). With no witness the restore
    // must fail closed: unproven bytes are not request-admissible until the
    // background worker reconciles against gix truth.
    std::fs::remove_file(store.path().join("freshness_witness.v1"))
        .expect("remove restore-time freshness witness");

    let mut restarted = scheduler(&fixture, store.path().to_path_buf(), bytes);
    assert!(
        restarted
            .latest_complete_ready_for_query()
            .expect("ready check")
            .is_none(),
        "restored bytes are not request-admissible before current truth is proven"
    );
    assert_eq!(
        restarted.pending_hint_count(),
        None,
        "the ready check schedules one overflow reconcile for the background worker"
    );
    let first_wake_epoch = restarted.epoch.load(std::sync::atomic::Ordering::Acquire);
    assert!(
        restarted
            .latest_complete_ready_for_query()
            .expect("repeat ready check")
            .is_none()
    );
    assert!(
        restarted.epoch.load(std::sync::atomic::Ordering::Acquire) > first_wake_epoch,
        "an earlier failed wake cannot strand a retained overflow marker"
    );

    let outcome = restarted.reconcile_now().expect("background truth");
    assert!(matches!(outcome, CodeIndexReconcileOutcomeV1::Noop(_)));
    assert!(
        restarted
            .latest_complete_ready_for_query()
            .expect("ready check")
            .is_some(),
        "the unchanged restored generation becomes request-admissible after reconciliation"
    );
}

#[test]
fn exact_source_readiness_abstains_when_a_file_is_added_inside_freshness_window() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let store = TempDir::new().expect("store root");
    let mut scheduler = scheduler(
        &fixture,
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );
    published(scheduler.reconcile_now().expect("initial publish"));
    assert!(
        scheduler
            .latest_complete_ready_for_exact_source_with(
                GenerationDecodeAdmissionV1::AlreadyDecoded,
            )
            .expect("exact source readiness")
            .is_some()
    );

    fixture.edit("src/added.rs", "pub fn added() {}\n");

    assert!(
        scheduler
            .latest_complete_ready_for_exact_source_with(
                GenerationDecodeAdmissionV1::AlreadyDecoded,
            )
            .expect("exact source readiness after file add")
            .is_none(),
        "workspace completeness must abstain before the added file is published"
    );
}

#[test]
fn unchanged_reopen_with_witness_skips_full_reconcile() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let store = TempDir::new().expect("store root");
    let bytes = Arc::new(SharedCodeIndexBytePoolV1::default());
    let baseline = {
        let mut scheduler = scheduler(&fixture, store.path().to_path_buf(), Arc::clone(&bytes));
        published(scheduler.reconcile_now().expect("initial publish"))
    };
    assert!(
        store.path().join("freshness_witness.v1").is_file(),
        "a successful reconcile persists the restore-time freshness witness"
    );

    // Reopen against the same store with the worktree unchanged. Foreground
    // open remains unverified and decode-free; the retained owner then uses the
    // witness to activate the exact sealed generation without a source read.
    let mut reopened = scheduler(&fixture, store.path().to_path_buf(), bytes);
    assert!(
        !reopened.verified_against_source(),
        "foreground open cannot claim freshness before background proof"
    );
    assert_eq!(
        reopened.sealed_decode_count(),
        0,
        "foreground open must not decode sealed bytes"
    );
    assert!(
        matches!(
            reopened
                .activate_or_reconcile()
                .expect("retained activation"),
            CodeIndexReconcileOutcomeV1::Noop(_)
        ),
        "the matching frontier activates the sealed generation without rebuilding"
    );
    assert!(
        reopened
            .ensure_fresh_for_query()
            .expect("freshness ladder runs")
            .is_none(),
        "background frontier verification establishes the ordinary freshness clocks"
    );
    let served = reopened
        .latest_complete_ready_for_query()
        .expect("ready check")
        .expect("witness-verified restore serves immediately");
    assert_eq!(
        served.generation.manifest().generation_id,
        baseline.generation_id,
        "the witness-verified reopen serves the sealed generation"
    );
}

#[test]
fn edited_reopen_forces_full_reconcile_when_witness_mismatches() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let store = TempDir::new().expect("store root");
    let bytes = Arc::new(SharedCodeIndexBytePoolV1::default());
    let baseline = {
        let mut scheduler = scheduler(&fixture, store.path().to_path_buf(), Arc::clone(&bytes));
        published(scheduler.reconcile_now().expect("initial publish"))
    };

    // A working-tree edit changes the tier-2 stat signature, so the witness no
    // longer matches. The reopen must fail closed and fully reconcile the change
    // rather than serve the now-stale sealed generation.
    fixture.edit("src/lib.rs", "pub fn alpha() -> u32 { 2 }\n");

    let mut reopened = scheduler(&fixture, store.path().to_path_buf(), bytes);
    assert!(
        !reopened.verified_against_source(),
        "a changed worktree must never be adopted as verified from a stale witness"
    );
    let outcome = reopened
        .ensure_fresh_for_query()
        .expect("freshness ladder runs")
        .expect("a witness mismatch forces a reconcile");
    assert_ne!(
        published(outcome).generation_id,
        baseline.generation_id,
        "the edited source is captured in a freshly published generation"
    );
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

    let mut reopened = CodeIndexWorktreeSchedulerV1::open(
        test_project_id(),
        fixture.path(),
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    )
    .expect("foreground open defers sealed validation");
    assert_eq!(
        reopened.sealed_decode_count(),
        0,
        "foreground open must not inspect corrupt sealed bytes"
    );
    assert!(
        reopened.activate_or_reconcile().is_err(),
        "retained activation must reject corrupt sealed state"
    );
    assert!(
        reopened.latest_complete_already_decoded().is_none(),
        "corrupt sealed state never becomes serving state"
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

    let mut reopened = CodeIndexWorktreeSchedulerV1::open(
        test_project_id(),
        fixture.path(),
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    )
    .expect("foreground open defers sealed validation");
    assert!(
        reopened.activate_or_reconcile().is_err(),
        "pointer/generation mismatch must fail retained activation"
    );
    assert!(
        reopened.latest_complete_already_decoded().is_none(),
        "a mismatched pointer never becomes serving state"
    );
}

#[tokio::test]
async fn daemon_owned_per_worktree_scheduler_reconciles_saved_edits() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(1);
    assert!(
        registry
            .mount_worktree(
                test_project_id(),
                fixture.path(),
                store.path().to_path_buf(),
                None,
            )
            .await
            .expect("mount daemon-owned scheduler")
    );
    let first = wait_for_initial_generation(&registry, fixture.path()).await;

    fixture.edit("src/lib.rs", "pub fn alpha() -> u32 { 2 }\n");
    assert!(
        registry
            .notify_path(fixture.path(), fixture.path().join("src/lib.rs"))
            .await
    );
    let second = wait_for_generation_change(&registry, fixture.path(), &first).await;

    assert_ne!(first, second);
    registry.shutdown().await;
}

/// A slow freshness reconcile in one worktree must not serialize another
/// worktree's query. `latest_complete_fresh` clones the per-worktree handle
/// under a short map lock and drops the registry guard before reconciling, so
/// holding one scheduler's lock never blocks the registry map for others.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn worktree_queries_do_not_serialize_on_slow_reconcile() {
    let slow = GitFixture::new(&[("src/lib.rs", "pub fn slow() -> u32 { 1 }\n")]);
    let fast = GitFixture::new(&[("src/lib.rs", "pub fn fast() -> u32 { 2 }\n")]);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(2);
    for fixture in [&slow, &fast] {
        assert!(
            registry
                .mount_worktree(
                    test_project_id(),
                    fixture.path(),
                    store.path().to_path_buf(),
                    None,
                )
                .await
                .expect("mount worktree")
        );
    }
    // Let both workers publish their initial generation so neither scheduler is
    // mid-reconcile when the test grabs a lock.
    for fixture in [&slow, &fast] {
        let _ = wait_for_initial_generation(&registry, fixture.path()).await;
    }

    // Hold the slow worktree's scheduler lock on a dedicated thread to model a
    // long in-flight reconcile that cannot complete until the test releases it.
    let slow_handle = registry
        .scheduler_handle(slow.path())
        .await
        .expect("slow scheduler handle");
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let lock_thread = std::thread::spawn(move || {
        let _guard = slow_handle
            .lock()
            .unwrap_or_else(|_| panic!("slow scheduler lock"));
        held_tx.send(()).expect("signal slow lock held");
        let _ = release_rx.recv();
    });
    held_rx.recv().expect("slow scheduler lock acquired");

    // A freshness query on the slow worktree now blocks on its scheduler lock.
    // Under the old design it would hold the registry map lock while blocked,
    // starving every other worktree's query.
    let slow_registry = registry.clone();
    let slow_path = slow.path().to_path_buf();
    let slow_query =
        tokio::spawn(async move { slow_registry.latest_complete_fresh(&slow_path).await });
    // Let the slow query enter its blocking reconcile section (acquire and drop
    // the map lock, then park on the scheduler lock) before the fast query runs.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // The fast worktree's query must complete within a bounded time even while
    // the slow worktree's reconcile is stuck holding its scheduler lock.
    let fast_result = tokio::time::timeout(
        Duration::from_secs(2),
        registry.latest_complete_fresh(fast.path()),
    )
    .await
    .expect("fast worktree query is not serialized behind the slow reconcile");
    assert!(
        fast_result.is_some(),
        "fast worktree serves its generation while the slow worktree reconcile is in flight"
    );

    // Release the slow lock and let its query drain before shutting down.
    release_tx.send(()).expect("release slow lock");
    lock_thread.join().expect("slow lock thread joins");
    let _ = slow_query.await;
    registry.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn busy_worktree_serves_last_complete_generation_without_waiting() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn ready() -> u32 { 1 }\n")]);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(1);
    registry
        .mount_worktree(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("mount worktree");
    let expected = wait_for_initial_generation(&registry, fixture.path()).await;
    let scheduler = registry
        .scheduler_handle(fixture.path())
        .await
        .expect("scheduler handle");
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let lock_thread = std::thread::spawn(move || {
        let _guard = scheduler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        held_tx.send(()).expect("signal scheduler lock held");
        let _ = release_rx.recv();
    });
    held_rx.recv().expect("scheduler lock acquired");

    let result = tokio::time::timeout(
        Duration::from_millis(250),
        registry.latest_complete_fresh(fixture.path()),
    )
    .await;
    release_tx.send(()).expect("release scheduler lock");
    lock_thread.join().expect("scheduler lock thread joins");

    let latest = result
        .expect("foreground query must not wait for an in-flight refresh")
        .expect("last complete generation remains queryable");
    assert_eq!(
        latest.generation.manifest().generation_id,
        expected,
        "foreground query must preserve the unchanged complete generation"
    );
    registry.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_signals_code_index_worker_without_taking_busy_scheduler_lock() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn busy() -> u32 { 1 }\n")]);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(1);
    registry
        .mount_worktree(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("mount worktree");
    // Let the mount-time reconcile finish first. Until it does, the background
    // worker owns the scheduler lock itself, and shutdown joining a worker that
    // is *already* blocked acquiring that lock is a different wait than the one
    // under test — this test is about shutdown never taking the lock on its own
    // behalf.
    wait_for_initial_generation(&registry, fixture.path()).await;
    let scheduler = registry
        .scheduler_handle(fixture.path())
        .await
        .expect("scheduler handle");
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let lock_thread = std::thread::spawn(move || {
        let _guard = scheduler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        held_tx.send(()).expect("signal scheduler lock held");
        std::thread::sleep(Duration::from_millis(750));
    });
    held_rx.recv().expect("scheduler lock acquired");

    let started = std::time::Instant::now();
    registry.shutdown().await;
    let elapsed = started.elapsed();
    lock_thread.join().expect("scheduler lock holder joins");

    assert!(
        elapsed < Duration::from_millis(250),
        "shutdown waited {elapsed:?} for a synchronous scheduler lock instead of signalling its cooperative cancellation token"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn project_retirement_retains_blocked_worker_owner_until_retry_joins_it() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn busy() -> u32 { 1 }\n")]);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(1);
    registry
        .mount_worktree(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("mount worktree");
    wait_for_initial_generation(&registry, fixture.path()).await;
    let scheduler = registry
        .scheduler_handle(fixture.path())
        .await
        .expect("scheduler handle");
    let wake = {
        let scheduler = scheduler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Arc::clone(&scheduler.wake)
    };
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let lock_thread = std::thread::spawn(move || {
        let _guard = scheduler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        held_tx.send(()).expect("signal scheduler lock held");
        release_rx.recv().expect("release scheduler lock");
    });
    held_rx.recv().expect("scheduler lock acquired");
    fixture.edit("src/lib.rs", "pub fn busy() -> u32 { 2 }\n");
    wake.notify_one();
    tokio::time::sleep(Duration::from_millis(50)).await;
    let roots = [fixture.path().canonicalize().expect("canonical root")]
        .into_iter()
        .collect();

    assert!(
        !registry
            .retire_project_roots_with_deadline(&roots, Duration::from_millis(25))
            .await,
        "blocked writer must report settling"
    );
    assert_eq!(registry.retiring_owner_count().await, 1);
    release_tx.send(()).expect("release writer");
    lock_thread.join().expect("writer joins");
    assert!(
        registry
            .retire_project_roots_with_deadline(&roots, Duration::from_secs(2))
            .await,
        "retry must join the retained owner"
    );
    assert_eq!(registry.retiring_owner_count().await, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn simultaneous_cold_mounts_admit_exactly_one_worktree_owner() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn race() -> u32 { 1 }\n")]);
    let store = TempDir::new().expect("store root");
    // Keep both newly-created workers parked at their dequeue point. The test
    // counts mount admissions before either worker can coalesce its initial
    // wake, so a duplicate cold owner cannot hide behind worker scheduling.
    let registry = CodeIndexSchedulerRegistryV1::with_background_reconcile_permits(2, 0);
    let callers = 2;
    registry.install_cold_mount_admission_barrier(fixture.path(), callers);
    let start = Arc::new(tokio::sync::Barrier::new(callers + 1));
    let mounts = (0..callers)
        .map(|_| {
            let registry = registry.clone();
            let project_root = fixture.path().to_path_buf();
            let store_root = store.path().to_path_buf();
            let start = Arc::clone(&start);
            tokio::spawn(async move {
                start.wait().await;
                registry
                    .mount_worktree(test_project_id(), &project_root, store_root, None)
                    .await
                    .expect("cold mount")
            })
        })
        .collect::<Vec<_>>();
    start.wait().await;

    let mut created_owners = 0;
    for mount in mounts {
        if mount.await.expect("mount task joins") {
            created_owners += 1;
        }
    }
    let mounted_worktrees = registry.memory_stats().await.mounted_worktrees;

    // Close the shared admission and join the retained owner before asserting,
    // so a failing run cannot leave a detached worker behind the test.
    registry.shutdown().await;

    assert_eq!(
        created_owners, 1,
        "one root must admit one cold owner; a second true result creates a detached runtime"
    );
    assert_eq!(
        mounted_worktrees, 1,
        "the retained mount table must contain the one owner that was admitted"
    );
}

// Each caller begins from the same empty pending slot. Holding the scheduler
// lock forces every request onto the BusyFollowUp path, where the registry—not
// the worker's later wake coalescing—is solely responsible for one admission.
#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn concurrent_query_admissions_claim_one_pending_wake_before_worker_coalescing() {
    let fixture = GitFixture::new(&[("src/main.rs", "fn main() {}\n")]);
    let store = TempDir::new().expect("store root");
    let (registry, scope) = mounted_core_query_worktree_with_one_permit(&fixture, &store).await;
    let admission = registry
        .background_reconcile_admission()
        .acquire_owned()
        .await
        .expect("background reconcile admission");
    let scheduler = {
        let mounted = registry.mounted.lock().await;
        Arc::clone(
            &mounted
                .get(&fixture.path().canonicalize().expect("canonical root"))
                .expect("mounted worktree")
                .scheduler,
        )
    };
    registry.clear_serving_generation_for_scope(&scope).await;
    registry.clear_pending_wake_for_scope(&scope).await;
    let held = scheduler
        .lock()
        .expect("hold the scheduler as a rebuild would");

    let callers = 8;
    registry.install_query_admission_barrier(&scope, callers);
    let start = Arc::new(tokio::sync::Barrier::new(callers + 1));
    let requests = (0..callers)
        .map(|_| {
            let registry = registry.clone();
            let scope = scope.clone();
            let start = Arc::clone(&start);
            tokio::spawn(async move {
                start.wait().await;
                registry.request_query_background_reconcile(&scope).await
            })
        })
        .collect::<Vec<_>>();
    start.wait().await;

    let mut admitted = 0;
    for request in requests {
        if request.await.expect("query admission task joins") {
            admitted += 1;
        }
    }
    let stamped = registry
        .pending_wake_micros_for_scope(&scope)
        .await
        .expect("mounted worktree");

    drop(held);
    registry.shutdown().await;
    drop(admission);

    assert_ne!(
        stamped, 0,
        "the one winning admission records its pending wake"
    );
    assert_eq!(
        admitted, 1,
        "the registry must atomically claim one query admission before worker wake coalescing"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelled_cold_mount_holds_its_reservation_until_blocking_open_finishes() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn cancel() {}\n")]);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::with_background_reconcile_permits(1, 0);
    registry.install_cold_mount_open_gate(fixture.path());

    let leader = {
        let registry = registry.clone();
        let project_root = fixture.path().to_path_buf();
        let store_root = store.path().to_path_buf();
        tokio::spawn(async move {
            registry
                .mount_worktree(test_project_id(), &project_root, store_root, None)
                .await
        })
    };
    registry
        .wait_for_cold_mount_open_events(fixture.path(), 1)
        .await;

    let follower = {
        let registry = registry.clone();
        let project_root = fixture.path().to_path_buf();
        let store_root = store.path().to_path_buf();
        tokio::spawn(async move {
            registry
                .mount_worktree(test_project_id(), &project_root, store_root, None)
                .await
        })
    };
    registry.wait_for_cold_mount_follower(fixture.path()).await;
    leader.abort();
    assert!(
        leader.await.is_err(),
        "caller cancellation joins as cancelled"
    );

    registry.release_cold_mount_open_gate(fixture.path());
    assert!(
        follower
            .await
            .expect("follower mount task joins")
            .expect("follower retries after the detached open settles"),
        "the follower becomes the one canonical owner after the cancelled caller's open ends"
    );
    let events = registry.cold_mount_open_events(fixture.path());
    let first_finished = events
        .iter()
        .position(|event| *event == ColdMountOpenEventV1::Finished)
        .expect("first blocking open finished");
    let second_started = events
        .iter()
        .enumerate()
        .filter(|(_, event)| **event == ColdMountOpenEventV1::Started)
        .nth(1)
        .map(|(index, _)| index)
        .expect("retry opens after the detached open settles");

    registry.shutdown().await;

    assert!(
        first_finished < second_started,
        "a cancelled caller must retain its reservation until its detached blocking open has finished"
    );
}

#[tokio::test]
async fn failed_cold_mount_releases_its_reservation_for_retry() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn retry() {}\n")]);
    let registry = CodeIndexSchedulerRegistryV1::new(1);
    let bad_store_root = TempDir::new().expect("bad store root");
    let bad_store = bad_store_root.path().join("not-a-directory");
    std::fs::write(&bad_store, "not a directory").expect("write failing store path");

    assert!(
        registry
            .mount_worktree(test_project_id(), fixture.path(), bad_store, None)
            .await
            .is_err(),
        "the first cold open fails through the typed scheduler error"
    );
    let retry_store = TempDir::new().expect("retry store root");
    assert!(
        registry
            .mount_worktree(
                test_project_id(),
                fixture.path(),
                retry_store.path().to_path_buf(),
                None,
            )
            .await
            .expect("failed cold mount must release its exact reservation"),
        "retry owns the root after the failed open finishes"
    );

    registry.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn distinct_cold_mounts_respect_capacity_before_opening() {
    let first = GitFixture::new(&[("src/lib.rs", "pub fn first() {}\n")]);
    let second = GitFixture::new(&[("src/lib.rs", "pub fn second() {}\n")]);
    let third = GitFixture::new(&[("src/lib.rs", "pub fn third() {}\n")]);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::with_background_reconcile_permits(2, 0);
    registry.install_cold_mount_open_gate(first.path());
    registry.install_cold_mount_open_gate(second.path());
    registry.install_cold_mount_open_observer(third.path());

    let first_mount = {
        let registry = registry.clone();
        let project_root = first.path().to_path_buf();
        let store_root = store.path().to_path_buf();
        tokio::spawn(async move {
            registry
                .mount_worktree(test_project_id(), &project_root, store_root, None)
                .await
        })
    };
    registry
        .wait_for_cold_mount_open_events(first.path(), 1)
        .await;
    let second_mount = {
        let registry = registry.clone();
        let project_root = second.path().to_path_buf();
        let store_root = store.path().to_path_buf();
        tokio::spawn(async move {
            registry
                .mount_worktree(test_project_id(), &project_root, store_root, None)
                .await
        })
    };
    registry
        .wait_for_cold_mount_open_events(second.path(), 1)
        .await;

    let capacity = registry
        .mount_worktree(
            test_project_id(),
            third.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect_err("the N+1 distinct root must be refused before opening");
    registry.release_cold_mount_open_gate(first.path());
    registry.release_cold_mount_open_gate(second.path());
    assert!(
        first_mount
            .await
            .expect("first mount task joins")
            .expect("first mount"),
        "first reserved root mounts"
    );
    assert!(
        second_mount
            .await
            .expect("second mount task joins")
            .expect("second mount"),
        "second reserved root mounts"
    );

    registry.shutdown().await;

    assert!(matches!(
        capacity,
        super::CodeIndexSchedulerErrorV1::Identity(message)
            if message.contains("capacity is exhausted")
    ));
    assert!(
        registry.cold_mount_open_events(third.path()).is_empty(),
        "the rejected N+1 root never starts its expensive blocking open"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn foreign_wake_keeps_pending_arrival_when_query_claim_is_released() {
    let fixture = GitFixture::new(&[("src/main.rs", "fn main() {}\n")]);
    let store = TempDir::new().expect("store root");
    let (registry, scope) = mounted_core_query_worktree_with_one_permit(&fixture, &store).await;
    let admission = registry
        .background_reconcile_admission()
        .acquire_owned()
        .await
        .expect("hold background worker at its dequeue point");
    registry.clear_pending_wake_for_scope(&scope).await;
    registry.install_query_claim_gate(&scope);

    let request = {
        let registry = registry.clone();
        let scope = scope.clone();
        tokio::spawn(async move { registry.request_query_background_reconcile(&scope).await })
    };
    registry.wait_for_query_claim(&scope).await;
    assert!(
        registry
            .notify_path(fixture.path(), fixture.path().join("src/main.rs"))
            .await,
        "foreign hint wake is accepted for the mounted root"
    );
    registry.release_query_claim(&scope);
    assert!(
        !request.await.expect("query admission task joins"),
        "the fresh query declines its own reconcile after the foreign wake arrives"
    );
    let stamped = registry
        .pending_wake_micros_for_scope(&scope)
        .await
        .expect("mounted worktree");

    drop(admission);
    registry.shutdown().await;

    assert_ne!(
        stamped, 0,
        "dropping a query-owned claim must not erase a foreign coalesced wake"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn foreign_wake_arriving_during_query_claim_drop_is_retained() {
    let fixture = GitFixture::new(&[("src/main.rs", "fn main() {}\n")]);
    let store = TempDir::new().expect("store root");
    let (registry, scope) = mounted_core_query_worktree_with_one_permit(&fixture, &store).await;
    let admission = registry
        .background_reconcile_admission()
        .acquire_owned()
        .await
        .expect("hold background worker at its dequeue point");
    registry.clear_pending_wake_for_scope(&scope).await;
    registry.install_query_claim_gate(&scope);
    registry.install_pending_wake_drop_gate(&scope).await;

    let request = {
        let registry = registry.clone();
        let scope = scope.clone();
        tokio::spawn(async move { registry.request_query_background_reconcile(&scope).await })
    };
    registry.wait_for_query_claim(&scope).await;
    registry.release_query_claim(&scope);
    registry.wait_for_pending_wake_claim_drop(&scope).await;

    let foreign_wake = {
        let registry = registry.clone();
        let project_root = fixture.path().to_path_buf();
        let changed_path = fixture.path().join("src/main.rs");
        tokio::spawn(async move { registry.notify_path(&project_root, changed_path).await })
    };
    registry.wait_for_foreign_pending_wake_attempt(&scope).await;
    registry.release_pending_wake_claim_drop(&scope).await;

    assert!(
        !request.await.expect("query admission task joins"),
        "the rejected query releases its own claimed marker"
    );
    assert!(
        foreign_wake.await.expect("foreign wake task joins"),
        "foreign hint wake is accepted after the claim release"
    );
    let stamped = registry
        .pending_wake_micros_for_scope(&scope)
        .await
        .expect("mounted worktree");

    drop(admission);
    registry.shutdown().await;

    assert_ne!(
        stamped, 0,
        "a foreign wake that contended at the former owner/marker gap remains pending"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shutdown_after_outer_mount_check_refuses_reservation_before_open() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn shutdown_race() {}\n")]);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::with_background_reconcile_permits(1, 0);
    registry.install_cold_mount_post_check_gate(fixture.path());
    registry.install_cold_mount_open_observer(fixture.path());

    let mount = {
        let registry = registry.clone();
        let project_root = fixture.path().to_path_buf();
        let store_root = store.path().to_path_buf();
        tokio::spawn(async move {
            registry
                .mount_worktree(test_project_id(), &project_root, store_root, None)
                .await
        })
    };
    registry
        .wait_for_cold_mount_post_check(fixture.path())
        .await;
    registry.shutdown().await;
    registry.release_cold_mount_post_check(fixture.path());

    let error = mount
        .await
        .expect("mount task joins")
        .expect_err("shutdown closes cold-mount admission before reservation");
    assert!(matches!(
        error,
        super::CodeIndexSchedulerErrorV1::Identity(message)
            if message.contains("shutting down")
    ));
    assert!(
        registry.cold_mount_open_events(fixture.path()).is_empty(),
        "a caller paused before reservation never starts an open after shutdown closes admission"
    );
    assert_eq!(registry.memory_stats().await.mounted_worktrees, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shutdown_waits_for_and_fences_a_cold_mount_open() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn shutdown() {}\n")]);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::with_background_reconcile_permits(1, 0);
    registry.install_cold_mount_open_gate(fixture.path());
    let mount = {
        let registry = registry.clone();
        let project_root = fixture.path().to_path_buf();
        let store_root = store.path().to_path_buf();
        tokio::spawn(async move {
            registry
                .mount_worktree(test_project_id(), &project_root, store_root, None)
                .await
        })
    };
    registry
        .wait_for_cold_mount_open_events(fixture.path(), 1)
        .await;
    let mut cancelled = registry
        .subscribe_cold_mount_cancellation(fixture.path())
        .expect("cold mount reservation");
    let shutdown = {
        let registry = registry.clone();
        tokio::spawn(async move { registry.shutdown().await })
    };
    let _ = cancelled.changed().await;
    assert!(
        !shutdown.is_finished(),
        "shutdown waits for the in-flight blocking open after fencing it"
    );
    registry.release_cold_mount_open_gate(fixture.path());
    let mount_error = mount
        .await
        .expect("mount task joins")
        .expect_err("shutdown fences publication after the open ends");
    shutdown.await.expect("shutdown task joins");

    assert!(matches!(
        mount_error,
        super::CodeIndexSchedulerErrorV1::Identity(message)
            if message.contains("shutting down")
    ));
    assert_eq!(registry.memory_stats().await.mounted_worktrees, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn retirement_waits_for_and_fences_an_exact_cold_mount_open() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn retire() {}\n")]);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::with_background_reconcile_permits(1, 0);
    registry.install_cold_mount_open_gate(fixture.path());
    let mount = {
        let registry = registry.clone();
        let project_root = fixture.path().to_path_buf();
        let store_root = store.path().to_path_buf();
        tokio::spawn(async move {
            registry
                .mount_worktree(test_project_id(), &project_root, store_root, None)
                .await
        })
    };
    registry
        .wait_for_cold_mount_open_events(fixture.path(), 1)
        .await;
    let mut cancelled = registry
        .subscribe_cold_mount_cancellation(fixture.path())
        .expect("cold mount reservation");
    let roots = BTreeSet::from([fixture.path().canonicalize().expect("canonical root")]);
    let retirement = {
        let registry = registry.clone();
        tokio::spawn(async move {
            registry
                .retire_project_roots_with_deadline(&roots, Duration::from_secs(2))
                .await
        })
    };
    let _ = cancelled.changed().await;
    registry.release_cold_mount_open_gate(fixture.path());
    let mount_error = mount
        .await
        .expect("mount task joins")
        .expect_err("retirement fences publication after the open ends");
    assert!(
        retirement.await.expect("retirement task joins"),
        "retirement joins the cancelled cold-open reservation"
    );
    assert_eq!(registry.memory_stats().await.mounted_worktrees, 0);
    assert!(
        registry
            .mount_worktree(
                test_project_id(),
                fixture.path(),
                store.path().to_path_buf(),
                None,
            )
            .await
            .expect("retired reservation is released after its open joins"),
        "a new owner may mount only after retirement completes"
    );
    registry.shutdown().await;

    assert!(matches!(
        mount_error,
        super::CodeIndexSchedulerErrorV1::Identity(message)
            if message.contains("retiring")
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn background_reconciles_respect_a_single_admission_permit() {
    // A bound of ONE serializes all worktrees: while the first worker holds the
    // sole permit (blocked on its scheduler lock), the second cannot start.
    let first = GitFixture::new(&[("src/lib.rs", "pub fn first() -> u32 { 1 }\n")]);
    let second = GitFixture::new(&[("src/lib.rs", "pub fn second() -> u32 { 1 }\n")]);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::with_background_reconcile_permits(2, 1);
    for fixture in [&first, &second] {
        registry
            .mount_worktree(
                test_project_id(),
                fixture.path(),
                store.path().to_path_buf(),
                None,
            )
            .await
            .expect("mount worktree");
    }
    let first_generation = wait_for_initial_generation(&registry, first.path()).await;
    let second_generation = wait_for_initial_generation(&registry, second.path()).await;

    let first_handle = registry
        .scheduler_handle(first.path())
        .await
        .expect("first scheduler");
    let first_wake = {
        let scheduler = first_handle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Arc::clone(&scheduler.wake)
    };
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let lock_thread = std::thread::spawn(move || {
        let _guard = first_handle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        held_tx.send(()).expect("signal first lock held");
        let _ = release_rx.recv();
    });
    held_rx.recv().expect("first scheduler lock acquired");

    first.edit("src/lib.rs", "pub fn first() -> u32 { 2 }\n");
    first_wake.notify_one();
    tokio::time::sleep(Duration::from_millis(100)).await;

    second.edit("src/lib.rs", "pub fn second() -> u32 { 2 }\n");
    assert!(
        registry
            .notify_path(second.path(), second.path().join("src/lib.rs"))
            .await
    );
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(
        registry.latest_generation_id(second.path()).await,
        Some(second_generation.clone()),
        "with a single permit a second worktree must wait behind the first"
    );

    release_tx.send(()).expect("release first scheduler");
    lock_thread.join().expect("first lock thread joins");
    let _ = wait_for_generation_change(&registry, first.path(), &first_generation).await;
    let _ = wait_for_generation_change(&registry, second.path(), &second_generation).await;
    registry.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn distinct_stores_reconcile_in_parallel_under_bounded_admission() {
    // With two permits, hold the FIRST worktree's scheduler lock so its worker
    // takes one permit and blocks mid-reconcile (an in-flight reconcile analog).
    // The SECOND worktree, writing to a different path-scoped store, must still
    // acquire the remaining permit and publish — proving distinct stores are NOT
    // serialized behind one another. (Same-store exclusion — that one worktree
    // never runs two overlapping reconciles — is structural, from its single
    // worker plus per-scheduler `Mutex`, and is covered by
    // `scheduler_notifications_release_registry_while_reconcile_is_busy`.)
    let first = GitFixture::new(&[("src/lib.rs", "pub fn first() -> u32 { 1 }\n")]);
    let second = GitFixture::new(&[("src/lib.rs", "pub fn second() -> u32 { 1 }\n")]);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::with_background_reconcile_permits(2, 2);
    for fixture in [&first, &second] {
        registry
            .mount_worktree(
                test_project_id(),
                fixture.path(),
                store.path().to_path_buf(),
                None,
            )
            .await
            .expect("mount worktree");
    }
    let first_generation = wait_for_initial_generation(&registry, first.path()).await;
    let second_generation = wait_for_initial_generation(&registry, second.path()).await;

    let first_handle = registry
        .scheduler_handle(first.path())
        .await
        .expect("first scheduler");
    let first_wake = {
        let scheduler = first_handle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Arc::clone(&scheduler.wake)
    };
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let lock_thread = std::thread::spawn(move || {
        let _guard = first_handle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        held_tx.send(()).expect("signal first lock held");
        let _ = release_rx.recv();
    });
    held_rx.recv().expect("first scheduler lock acquired");

    // Wake the first worker: it takes one of the two permits, then blocks on the
    // held scheduler lock. Its own store cannot advance while blocked, but it
    // occupies exactly one permit.
    first.edit("src/lib.rs", "pub fn first() -> u32 { 2 }\n");
    first_wake.notify_one();
    tokio::time::sleep(Duration::from_millis(100)).await;

    // The second worktree — a distinct path-scoped store — must proceed on the
    // remaining permit and publish a new generation without the first releasing.
    // (Note: the first scheduler lock is deliberately held here, so we must NOT
    // query the first worktree via `latest_generation_id`, which would block on
    // that lock while holding the registry map lock.)
    second.edit("src/lib.rs", "pub fn second() -> u32 { 2 }\n");
    assert!(
        registry
            .notify_path(second.path(), second.path().join("src/lib.rs"))
            .await
    );
    let advanced_second =
        wait_for_generation_change(&registry, second.path(), &second_generation).await;
    assert_ne!(
        advanced_second, second_generation,
        "a distinct store must reconcile in parallel, not serialize behind the first"
    );

    // Release the first worktree and confirm it, too, reconciles the pending edit
    // once its lock frees — it was blocked, never starved.
    release_tx.send(()).expect("release first scheduler");
    lock_thread.join().expect("first lock thread joins");
    let advanced_first =
        wait_for_generation_change(&registry, first.path(), &first_generation).await;
    assert_ne!(
        advanced_first, first_generation,
        "the first worktree reconciles its pending edit once its lock is released"
    );
    registry.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn poisoned_scheduler_lock_does_not_retire_the_background_worker() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(1);
    registry
        .mount_worktree(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("mount worktree");
    let initial = wait_for_initial_generation(&registry, fixture.path()).await;
    let scheduler = registry
        .scheduler_handle(fixture.path())
        .await
        .expect("scheduler");
    let poison = Arc::clone(&scheduler);
    assert!(
        std::thread::spawn(move || {
            let _guard = poison.lock().expect("unpoisoned scheduler");
            panic!("fixture poison");
        })
        .join()
        .is_err()
    );

    fixture.edit("src/lib.rs", "pub fn alpha() -> u32 { 2 }\n");
    scheduler
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .notify_path(fixture.path().join("src/lib.rs"));
    let _ = wait_for_generation_change(&registry, fixture.path(), &initial).await;
    registry.shutdown().await;
}

/// In-process semantic-vector graph fixture: the canonical isolated semantic
/// evaluation graph stands in for the daemon-retained code-graph runtime, so
/// publish/restore flows exercise the same verified staging/publication
/// machinery the production provider resolves.
#[cfg(feature = "semantic-fastembed")]
struct IsolatedSemanticVectorGraphProviderV1 {
    graph: Arc<tracedecay_usecases::store::vector_generations::IsolatedSemanticEvaluationGraphV1>,
    current: tracedecay_domain::CodeGenerationId,
}

#[cfg(feature = "semantic-fastembed")]
impl IsolatedSemanticVectorGraphProviderV1 {
    fn new(
        generation: &tracedecay_code_index::production::CodeIndexPublishedGenerationV1,
    ) -> Arc<Self> {
        let graph =
            tracedecay_usecases::store::vector_generations::isolated_semantic_evaluation_graph(
                &[generation],
                Arc::new(NeverCancelled),
            )
            .expect("open isolated semantic evaluation graph");
        Arc::new(Self {
            graph,
            current: generation.manifest().generation_id.clone(),
        })
    }
}

#[cfg(feature = "semantic-fastembed")]
impl SemanticVectorGraphProviderV1 for IsolatedSemanticVectorGraphProviderV1 {
    fn graph_for_generation<'a>(
        &'a self,
        generation: &'a tracedecay_code_index::production::CodeIndexPublishedGenerationV1,
    ) -> SemanticRuntimeFuture<'a, Result<RetainedSemanticVectorGraphV1, SemanticVectorGraphErrorV1>>
    {
        Box::pin(async move {
            self.graph
                .retained(&generation.manifest().generation_id)
                .map_err(|error| SemanticVectorGraphErrorV1::Rejected(error.to_string()))
        })
    }

    fn graph_for_current(
        &self,
    ) -> SemanticRuntimeFuture<'_, Result<RetainedSemanticVectorGraphV1, SemanticVectorGraphErrorV1>>
    {
        Box::pin(async move {
            self.graph
                .retained(&self.current)
                .map_err(|error| SemanticVectorGraphErrorV1::Rejected(error.to_string()))
        })
    }
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
        Database::publish_test_runtime(
            &database_path,
            &authority,
            TestDatabaseRuntimeMode::Initialize,
        )
        .await
        .expect("project database")
        .0,
    );
    let handle = DaemonSemanticRuntimeHandleV1::new(1, 64, 2 << 30).expect("semantic handle");
    let vector_graph = IsolatedSemanticVectorGraphProviderV1::new(&latest.generation);
    let runtime = ProductionSemanticRuntimeV1::new(
        handle.clone(),
        Arc::clone(&database),
        Arc::clone(&vector_graph) as Arc<dyn SemanticVectorGraphProviderV1>,
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
    assert!(
        handle
            .query_factory(
                &current.source_generation,
                &current.generation,
                &current.projection_key,
            )
            .is_some()
    );

    let restarted_handle =
        DaemonSemanticRuntimeHandleV1::new(1, 64, 2 << 30).expect("restarted handle");
    let restarted = ProductionSemanticRuntimeV1::new(
        restarted_handle.clone(),
        database,
        Arc::clone(&vector_graph) as Arc<dyn SemanticVectorGraphProviderV1>,
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
            .restore_current(&latest.generation, &current.generation)
            .await
            .expect("restore current generation")
    );
    assert_eq!(restarted_handle.current(), Some(current.clone()));
    assert!(
        restarted_handle
            .query_factory(
                &current.source_generation,
                &current.generation,
                &current.projection_key,
            )
            .is_some()
    );
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
        .mount_worktree(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("mount daemon-owned scheduler");
    let generation = wait_for_initial_generation(&registry, fixture.path()).await;
    let latest = registry
        .latest_complete_fresh(fixture.path())
        .await
        .expect("mounted generation");
    assert_eq!(latest.generation.manifest().generation_id, generation);
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
    mount_query_authority(
        &registry,
        fixture.path(),
        &exact_context,
        latest.generation.manifest().privacy_domain.clone(),
    )
    .await;
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
    let exact_repeat = registry
        .exact_occurrence(
            RetrievalPortContext {
                request: &exact_context,
                operation: &exact_operation,
            },
            &exact_request,
        )
        .await;
    assert!(
        exact.evidence().finished_at > latest.generation.manifest().seal.sealed_at,
        "query completion time must not reuse the generation seal time"
    );
    assert_eq!(
        serde_json::to_vec(&exact.evidence().payload).expect("serialize exact payload"),
        serde_json::to_vec(&exact_repeat.evidence().payload)
            .expect("serialize repeated exact payload"),
        "same generation and request produce byte-stable production query payload"
    );
    match exact {
        RetrievalPortOutcome::Completed(evidence) => {
            let page = evidence.payload.expect("exact page");
            assert_eq!(page.generation, generation);
            assert!(
                !page.items.is_empty(),
                "exact operation must return production lane evidence"
            );
        }
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
        Vec::new(),
        0,
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
        RetrievalPortOutcome::Completed(evidence) => {
            let page = evidence.payload.expect("lexical page");
            assert_eq!(page.generation, generation);
            assert!(
                !page.items.is_empty(),
                "lexical operation must return production lane evidence"
            );
        }
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
    let graph_context = application_context(&graph_operation, repository.clone(), worktree.clone());
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
        RetrievalPortOutcome::Completed(evidence) => {
            let page = evidence.payload.expect("graph page");
            assert_eq!(page.generation, generation);
            assert!(
                !page.items.is_empty(),
                "graph operation must return production lane evidence"
            );
        }
        outcome => panic!("expected completed graph operation, got {outcome:?}"),
    }

    let qualified_name = latest
        .generation
        .symbols()
        .symbols
        .iter()
        .find(|record| record.qualified_name.ends_with("callee"))
        .expect("callee symbol")
        .qualified_name
        .clone();
    let qualified_operation =
        callable_code_operation(CallableCodeOperationKind::QualifiedName).expect("operation");
    let qualified_context =
        application_context(&qualified_operation, repository.clone(), worktree.clone());
    let qualified_request = QualifiedNameRequest {
        qualified_name,
        scope: graph_request.scope.clone(),
        meta: query_meta(),
    };
    let qualified = registry
        .qualified_name(
            RetrievalPortContext {
                request: &qualified_context,
                operation: &qualified_operation,
            },
            &qualified_request,
        )
        .await;
    assert_eq!(
        qualified
            .evidence()
            .payload
            .as_ref()
            .expect("qualified page")
            .items
            .len(),
        1
    );

    let file = latest.generation.snapshot().files[0]
        .file_occurrence_id
        .clone();
    let metadata_operation =
        callable_code_operation(CallableCodeOperationKind::SourceMetadata).expect("operation");
    let metadata_context = application_context(&metadata_operation, repository, worktree);
    let metadata_request =
        SourceMetadataRequest::new(vec![file], graph_request.scope.clone(), query_meta())
            .expect("metadata request");
    let metadata = registry
        .source_metadata(
            RetrievalPortContext {
                request: &metadata_context,
                operation: &metadata_operation,
            },
            &metadata_request,
        )
        .await;
    let metadata_page = metadata.evidence().payload.as_ref().expect("metadata page");
    assert_eq!(metadata_page.items[0].path, "src/lib.rs");
    assert_eq!(metadata_page.items[0].language.as_deref(), Some("rust"));

    let facets_operation =
        callable_code_operation(CallableCodeOperationKind::Facets).expect("operation");
    let facets_context = application_context(
        &facets_operation,
        latest.generation.snapshot().repository.clone(),
        latest
            .generation
            .snapshot()
            .worktree
            .clone()
            .expect("worktree"),
    );
    let facets = registry
        .facets(
            RetrievalPortContext {
                request: &facets_context,
                operation: &facets_operation,
            },
            &CodeFacetRequest {
                dimension: CodeFacetDimension::Kind,
                scope: graph_request.scope.clone(),
                meta: query_meta(),
            },
        )
        .await;
    assert!(
        !facets
            .evidence()
            .payload
            .as_ref()
            .expect("facet page")
            .items
            .is_empty()
    );

    let timeline_operation =
        callable_code_operation(CallableCodeOperationKind::Timeline).expect("operation");
    let timeline_context = application_context(
        &timeline_operation,
        latest.generation.snapshot().repository.clone(),
        latest
            .generation
            .snapshot()
            .worktree
            .clone()
            .expect("worktree"),
    );
    let timeline = registry
        .timeline(
            RetrievalPortContext {
                request: &timeline_context,
                operation: &timeline_operation,
            },
            &CodeTimelineRequest {
                scope: graph_request.scope.clone(),
                meta: query_meta(),
            },
        )
        .await;
    assert_eq!(
        timeline
            .evidence()
            .payload
            .as_ref()
            .expect("timeline page")
            .items
            .len(),
        1
    );

    let callee = latest
        .generation
        .symbols()
        .symbols
        .iter()
        .find(|record| record.qualified_name.ends_with("callee"))
        .expect("callee")
        .occurrence
        .as_str()
        .to_owned();
    let references_operation =
        callable_code_operation(CallableCodeOperationKind::References).expect("operation");
    let references_context = application_context(
        &references_operation,
        latest.generation.snapshot().repository.clone(),
        latest
            .generation
            .snapshot()
            .worktree
            .clone()
            .expect("worktree"),
    );
    let references = registry
        .references(
            RetrievalPortContext {
                request: &references_context,
                operation: &references_operation,
            },
            &CodeNavigationRequest {
                node_id: callee,
                scope: graph_request.scope.clone(),
                meta: query_meta(),
            },
        )
        .await;
    assert!(
        !references
            .evidence()
            .payload
            .as_ref()
            .expect("references page")
            .items
            .is_empty()
    );

    registry.shutdown().await;
}

// ---------------------------------------------------------------------------
// Worktree-aware incremental indexing: identity, gix classification, the
// hook-driven + lazy-reconcile freshness ladder.
// ---------------------------------------------------------------------------

use super::CodeIndexHintPolicyV1;
use super::classification::{WorktreeChangeClassV1, WorktreeChangeClassificationV1};

fn scheduler_with_policy(
    fixture: &GitFixture,
    store_root: PathBuf,
    bytes: Arc<SharedCodeIndexBytePoolV1>,
    policy: CodeIndexHintPolicyV1,
) -> CodeIndexWorktreeSchedulerV1 {
    CodeIndexWorktreeSchedulerV1::open_with_policy(
        test_project_id(),
        fixture.path(),
        store_root,
        bytes,
        policy,
    )
    .expect("open worktree scheduler with policy")
}

/// gix status classification keeps committed/staged/unstaged/untracked/deleted
/// dispositions distinct and truthful.
#[test]
fn classification_distinguishes_staged_unstaged_untracked_and_deleted() {
    let fixture = GitFixture::new(&[
        ("src/a.rs", "pub fn a() -> u32 { 1 }\n"),
        ("src/b.rs", "pub fn b() -> u32 { 2 }\n"),
        ("src/d.rs", "pub fn d() -> u32 { 4 }\n"),
    ]);
    // Staged modification.
    fixture.edit("src/a.rs", "pub fn a() -> u32 { 10 }\n");
    git(fixture.path(), &["add", "src/a.rs"]);
    // Unstaged modification.
    fixture.edit("src/b.rs", "pub fn b() -> u32 { 20 }\n");
    // Untracked new file.
    write(fixture.path(), "src/c.rs", "pub fn c() -> u32 { 3 }\n");
    // Unstaged deletion.
    std::fs::remove_file(fixture.path().join("src/d.rs")).expect("remove d");

    let repository = gix::open(fixture.path()).expect("open gix");
    let classification = WorktreeChangeClassificationV1::classify(&repository).expect("classify");

    assert_eq!(
        classification.class_of("src/a.rs"),
        Some(WorktreeChangeClassV1::StagedModified)
    );
    assert_eq!(
        classification.class_of("src/b.rs"),
        Some(WorktreeChangeClassV1::UnstagedModified)
    );
    assert_eq!(
        classification.class_of("src/c.rs"),
        Some(WorktreeChangeClassV1::Untracked)
    );
    assert_eq!(
        classification.class_of("src/d.rs"),
        Some(WorktreeChangeClassV1::UnstagedDeleted)
    );

    let deleted = classification.deleted_paths();
    assert!(
        deleted.contains("src/d.rs"),
        "deletion is a tombstone candidate"
    );

    let candidates = classification.candidate_paths();
    assert!(candidates.contains("src/a.rs"));
    assert!(candidates.contains("src/b.rs"));
    assert!(candidates.contains("src/c.rs"));
    assert!(
        !candidates.contains("src/d.rs"),
        "a deleted path is never a present-content candidate"
    );
}

/// A filesystem rename is reconciled as the truthful delete-plus-add pair and
/// produces the same final code lanes as a fresh scan of the renamed tree.
#[test]
fn rename_reconciliation_matches_clean_scan() {
    let fixture = GitFixture::new(&[
        ("src/old.rs", "pub fn renamed_symbol() -> u32 { 7 }\n"),
        ("src/keep.rs", "pub fn keep_symbol() -> u32 { 1 }\n"),
    ]);
    let store = TempDir::new().expect("store root");
    let bytes = Arc::new(SharedCodeIndexBytePoolV1::default());
    let mut incremental = scheduler(
        &fixture,
        store.path().join("incremental"),
        Arc::clone(&bytes),
    );
    published(incremental.reconcile_now().expect("baseline publish"));

    std::fs::rename(
        fixture.path().join("src/old.rs"),
        fixture.path().join("src/new.rs"),
    )
    .expect("rename source file");
    let classification =
        WorktreeChangeClassificationV1::classify(&gix::open(fixture.path()).expect("open gix"))
            .expect("classify rename");
    assert_eq!(
        classification.class_of("src/old.rs"),
        Some(WorktreeChangeClassV1::UnstagedDeleted)
    );
    assert_eq!(
        classification.class_of("src/new.rs"),
        Some(WorktreeChangeClassV1::Untracked)
    );

    incremental.notify_path(fixture.path().join("src/old.rs"));
    incremental.notify_path(fixture.path().join("src/new.rs"));
    let renamed = published(
        incremental
            .reconcile_now()
            .expect("incremental rename reconcile"),
    );
    let mut clean = scheduler(&fixture, store.path().join("clean"), bytes);
    let clean = published(clean.reconcile_now().expect("clean renamed-tree scan"));

    assert_eq!(
        renamed.snapshot_content_identity, clean.snapshot_content_identity,
        "rename reconciliation must capture the same final tree as a clean scan"
    );
    assert_eq!(
        renamed._lane_digest, clean._lane_digest,
        "rename reconciliation must publish byte-identical code lanes"
    );
    let latest = incremental.latest_complete().expect("renamed generation");
    assert!(
        latest
            .generation()
            .snapshot()
            .files
            .iter()
            .any(|file| file.logical_path == "src/new.rs")
    );
    assert!(
        latest
            .generation()
            .snapshot()
            .files
            .iter()
            .all(|file| file.logical_path != "src/old.rs")
    );
}

/// A staged-only edit (index differs from HEAD while the worktree matches the
/// staged bytes) is real indexing work and converges with a fresh scan.
#[test]
fn index_only_reconciliation_matches_clean_scan() {
    let fixture = GitFixture::new(&[
        ("src/lib.rs", "pub fn staged_symbol() -> u32 { 1 }\n"),
        ("src/keep.rs", "pub fn keep_symbol() -> u32 { 2 }\n"),
    ]);
    let store = TempDir::new().expect("store root");
    let bytes = Arc::new(SharedCodeIndexBytePoolV1::default());
    let mut incremental = scheduler(
        &fixture,
        store.path().join("incremental"),
        Arc::clone(&bytes),
    );
    published(incremental.reconcile_now().expect("baseline publish"));

    fixture.edit("src/lib.rs", "pub fn staged_symbol() -> u32 { 10 }\n");
    git(fixture.path(), &["add", "src/lib.rs"]);
    let classification =
        WorktreeChangeClassificationV1::classify(&gix::open(fixture.path()).expect("open gix"))
            .expect("classify staged-only edit");
    assert_eq!(
        classification.class_of("src/lib.rs"),
        Some(WorktreeChangeClassV1::StagedModified)
    );
    assert_eq!(classification.changes().len(), 1);

    let staged = published(
        incremental
            .reconcile_now()
            .expect("incremental staged-only reconcile"),
    );
    let mut clean = scheduler(&fixture, store.path().join("clean"), bytes);
    let clean = published(clean.reconcile_now().expect("clean staged-tree scan"));

    assert_eq!(staged.reextracted_files, 1);
    assert_eq!(
        staged.snapshot_content_identity, clean.snapshot_content_identity,
        "staged-only reconciliation must capture the same final tree as a clean scan"
    );
    assert_eq!(
        staged._lane_digest, clean._lane_digest,
        "staged-only reconciliation must publish byte-identical code lanes"
    );
}

/// Deleting a tracked file tombstones its prior chunks: the next published
/// generation must not carry any chunk anchored to the removed file, while an
/// untouched sibling's chunks survive unchanged.
#[test]
fn deleting_a_file_tombstones_its_prior_chunks() {
    let fixture = GitFixture::new(&[
        ("src/keep.rs", "pub fn keep_marker_symbol() -> u32 { 1 }\n"),
        ("src/gone.rs", "pub fn gone_marker_symbol() -> u32 { 2 }\n"),
    ]);
    let store = TempDir::new().expect("store root");
    let bytes = Arc::new(SharedCodeIndexBytePoolV1::default());
    let mut scheduler = scheduler(&fixture, store.path().to_path_buf(), bytes);

    let baseline = published(scheduler.reconcile_now().expect("baseline publish"));
    let gone_occurrences: BTreeSet<_> = {
        let latest = scheduler.latest_complete().expect("baseline generation");
        latest
            .lexical()
            .iter()
            .filter(|chunk| chunk.sanitized_text.as_str().contains("gone_marker_symbol"))
            .map(|chunk| chunk.anchor.file_occurrence_id.clone())
            .collect()
    };
    assert!(
        !gone_occurrences.is_empty(),
        "baseline must index the file that will be deleted"
    );

    // Delete the tracked file out of band (an unstaged deletion) and reconcile.
    std::fs::remove_file(fixture.path().join("src/gone.rs")).expect("remove gone.rs");
    scheduler.notify_path(fixture.path().join("src/gone.rs"));
    let after = published(scheduler.reconcile_now().expect("post-deletion publish"));

    assert_ne!(
        baseline.generation_id, after.generation_id,
        "removing indexed content must publish a new generation"
    );
    assert!(
        after.changed_chunks > 0,
        "a deletion must register as changed (tombstoned) chunk work"
    );
    assert!(
        !after
            ._file_occurrence_ids
            .iter()
            .any(|occurrence| gone_occurrences.contains(occurrence)),
        "the deleted file's occurrence must be absent from the new generation"
    );

    let latest = scheduler
        .latest_complete()
        .expect("post-deletion generation");
    assert!(
        latest
            .lexical()
            .iter()
            .all(|chunk| !chunk.sanitized_text.as_str().contains("gone_marker_symbol")),
        "no surviving chunk may carry the deleted file's content"
    );
    assert!(
        latest
            .lexical()
            .iter()
            .any(|chunk| chunk.sanitized_text.as_str().contains("keep_marker_symbol")),
        "an untouched sibling's chunks must survive the deletion"
    );
}

/// A host after-file-edit hook delivers its exact touched paths into the
/// incremental queue and the subsequent reconcile publishes the edit.
#[test]
fn hook_hint_delivers_exact_paths_and_schedules_batch() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let store = TempDir::new().expect("store root");
    let bytes = Arc::new(SharedCodeIndexBytePoolV1::default());
    let mut scheduler = scheduler(&fixture, store.path().to_path_buf(), bytes);
    published(scheduler.reconcile_now().expect("baseline"));

    fixture.edit("src/lib.rs", "pub fn alpha() -> u32 { 2 }\n");
    let edited = fixture.path().join("src/lib.rs");
    scheduler.notify_hook_paths([edited.clone()]);

    assert!(
        scheduler.pending_hint_paths().contains(&edited),
        "the exact hook path is enqueued as a hint"
    );

    let publish = published(scheduler.reconcile_now().expect("hook-scheduled batch"));
    assert!(publish.changed_chunks > 0, "the hinted edit is indexed");
    assert!(
        scheduler.pending_hint_paths().is_empty(),
        "reconciliation drains the hint queue"
    );
}

/// With no filesystem watcher, a raw out-of-band file write is still caught by
/// the tier-2 bounded-staleness reconcile at query admission.
#[test]
fn threshold_expiry_reconciles_out_of_band_write_without_watcher() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let store = TempDir::new().expect("store root");
    let bytes = Arc::new(SharedCodeIndexBytePoolV1::default());
    let policy = CodeIndexHintPolicyV1 {
        staleness_threshold: Duration::ZERO,
    };
    let mut scheduler = scheduler_with_policy(&fixture, store.path().to_path_buf(), bytes, policy);
    let baseline = published(scheduler.reconcile_now().expect("baseline"));

    // No hook, no watcher: a raw editor/rsync write.
    fixture.edit("src/lib.rs", "pub fn alpha() -> u32 { 2 }\n");

    let reconciled = scheduler
        .ensure_fresh_for_query()
        .expect("freshness ladder runs");
    assert!(
        reconciled.is_some(),
        "an elapsed staleness bound reconciles at admission"
    );

    let served = scheduler
        .latest_complete()
        .expect("served generation")
        .generation
        .snapshot()
        .content_identity
        .clone();
    assert_ne!(
        served, baseline.snapshot_content_identity,
        "the out-of-band write is reflected in the served generation"
    );
}

/// Tier-2 cheapness: when the staleness bound has elapsed but nothing on disk
/// changed, the query-path freshness check must NOT run a full read+hash
/// reconcile. The stat-level prefilter resets the clock and reports no work, so
/// a quiet repository is never re-hashed every threshold on the query path.
#[test]
fn threshold_expiry_without_change_skips_full_reconcile() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let store = TempDir::new().expect("store root");
    let bytes = Arc::new(SharedCodeIndexBytePoolV1::default());
    let policy = CodeIndexHintPolicyV1 {
        staleness_threshold: Duration::ZERO,
    };
    let mut scheduler = scheduler_with_policy(&fixture, store.path().to_path_buf(), bytes, policy);
    let baseline = published(scheduler.reconcile_now().expect("baseline"));

    // The staleness bound has elapsed (ZERO), but with no disk change the stat
    // prefilter must short-circuit before any capture and report no reconcile.
    let reconciled = scheduler
        .ensure_fresh_for_query()
        .expect("freshness ladder runs");
    assert!(
        reconciled.is_none(),
        "an unchanged tree past the staleness bound must not reconcile"
    );

    let served = scheduler
        .latest_complete()
        .expect("served generation")
        .generation
        .manifest()
        .generation_id
        .clone();
    assert_eq!(
        served, baseline.generation_id,
        "no new generation is published when nothing changed on disk"
    );
}

#[test]
fn ready_query_expiry_defers_even_an_unchanged_tree() {
    let fixture = GitFixture::new(ALPHA_LIB_V1);
    let store = TempDir::new().expect("store root");
    let bytes = Arc::new(SharedCodeIndexBytePoolV1::default());
    let policy = CodeIndexHintPolicyV1 {
        staleness_threshold: Duration::ZERO,
    };
    let mut scheduler = scheduler_with_policy(&fixture, store.path().to_path_buf(), bytes, policy);
    published(scheduler.reconcile_now().expect("baseline"));

    assert!(
        scheduler
            .latest_complete_ready_for_query()
            .expect("ready query")
            .is_none(),
        "latency-sensitive admission must abstain instead of scanning the worktree"
    );
    assert_eq!(
        scheduler.pending_hint_count(),
        None,
        "ready admission schedules an overflow reconcile in the background"
    );
}

/// A git operation from another process (commit here stands in for pull/rebase)
/// is detected instantly by the tier-1 .git-metadata check, without waiting for
/// the staleness bound and without any watcher.
#[test]
fn git_op_in_another_process_detected_via_metadata() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let store = TempDir::new().expect("store root");
    let bytes = Arc::new(SharedCodeIndexBytePoolV1::default());
    // Long staleness bound so only tier-1 (git metadata) can fire.
    let policy = CodeIndexHintPolicyV1 {
        staleness_threshold: Duration::from_hours(1),
    };
    let mut scheduler = scheduler_with_policy(&fixture, store.path().to_path_buf(), bytes, policy);
    let baseline = published(scheduler.reconcile_now().expect("baseline"));

    // Another process commits a change.
    fixture.edit("src/lib.rs", "pub fn alpha() -> u32 { 2 }\n");
    git(fixture.path(), &["commit", "-qam", "external"]);

    let reconciled = scheduler
        .ensure_fresh_for_query()
        .expect("freshness ladder runs");
    assert!(
        reconciled.is_some(),
        "a .git-metadata change reconciles before the bound"
    );

    let served = scheduler
        .latest_complete()
        .expect("served generation")
        .generation
        .snapshot()
        .content_identity
        .clone();
    assert_ne!(
        served, baseline.snapshot_content_identity,
        "the external git change is reflected in the served generation"
    );
}

/// When HEAD moves between indexing and query, the served generation is
/// refreshed to the new revision while its repository/worktree identity is never
/// mixed with another checkout's.
#[test]
fn identity_move_reconciles_and_never_mixes_identity() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let store = TempDir::new().expect("store root");
    let bytes = Arc::new(SharedCodeIndexBytePoolV1::default());
    let policy = CodeIndexHintPolicyV1 {
        staleness_threshold: Duration::from_hours(1),
    };
    let mut scheduler = scheduler_with_policy(&fixture, store.path().to_path_buf(), bytes, policy);
    published(scheduler.reconcile_now().expect("baseline"));

    let repo_before = scheduler.identity().repository_id().clone();
    let worktree_before = scheduler.identity().worktree_id().clone();
    let commit_before = scheduler.identity().head_commit().cloned();

    // HEAD moves under the same worktree.
    fixture.edit("src/lib.rs", "pub fn alpha() -> u32 { 2 }\n");
    git(fixture.path(), &["commit", "-qam", "move-head"]);

    let reconciled = scheduler
        .ensure_fresh_for_query()
        .expect("freshness ladder runs");
    assert!(reconciled.is_some(), "a HEAD move reconciles at admission");

    // Tier-3 backstop: identity re-resolved to the new revision.
    let commit_after = scheduler.identity().head_commit().cloned();
    assert_ne!(
        commit_before, commit_after,
        "the resolved source revision advances with HEAD"
    );
    // Structural identity is never mixed across the move.
    assert_eq!(scheduler.identity().repository_id(), &repo_before);
    assert_eq!(scheduler.identity().worktree_id(), &worktree_before);

    let served = scheduler.latest_complete().expect("served generation");
    assert_eq!(
        &served.generation.snapshot().repository,
        &repo_before,
        "the served generation is attributed to its exact repository identity"
    );
    assert_eq!(
        served.generation.snapshot().worktree.as_ref(),
        Some(&worktree_before),
        "the served generation is attributed to its exact worktree identity"
    );
}

/// Re-reconciling identical final content in a fresh store (same worktree)
/// yields the byte-identical published chunk lane, proving publication output
/// is a pure function of content identity and independent of edit history.
#[test]
fn reparse_matches_full_parse_chunks() {
    let fixture = GitFixture::new(&[(
        "src/lib.rs",
        "pub fn alpha() -> u32 { 1 }\npub fn beta() -> u32 { 2 }\n",
    )]);
    let store = TempDir::new().expect("store root");
    let bytes = Arc::new(SharedCodeIndexBytePoolV1::default());

    // Sequential-edit scheduler: baseline then two edits, each reconciled.
    let mut sequential = scheduler(
        &fixture,
        store.path().join("sequential"),
        Arc::clone(&bytes),
    );
    published(sequential.reconcile_now().expect("sequential baseline"));

    fixture.edit(
        "src/lib.rs",
        "pub fn alpha() -> u32 { 10 }\npub fn beta() -> u32 { 2 }\n",
    );
    published(sequential.reconcile_now().expect("sequential edit 1"));

    let final_source = "pub fn alpha() -> u32 { 10 }\npub fn beta() -> u32 { 20 }\n";
    fixture.edit("src/lib.rs", final_source);
    let second = published(sequential.reconcile_now().expect("sequential edit 2"));

    // Fresh-store scheduler over the identical final content in the SAME
    // worktree, so chunk identity (repository/worktree-bound) still matches.
    let mut full = scheduler(&fixture, store.path().join("full"), bytes);
    let full_publish = published(full.reconcile_now().expect("full parse"));

    assert_eq!(
        second.snapshot_content_identity, full_publish.snapshot_content_identity,
        "identical final content yields identical snapshot identity"
    );
    assert_eq!(
        second._lane_digest, full_publish._lane_digest,
        "sequential-edit and fresh-store reconcile produce byte-identical chunk lanes"
    );
}

// ---------------------------------------------------------------------------
// Query-admission serving generation: an unpinned query resolves through the
// freshness ladder to the latest complete generation; an explicit caller pin
// is served generation-bound and read-only, bypassing freshness entirely.
// ---------------------------------------------------------------------------

fn canonical_fixture_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// Wait for the registry-mounted worktree to publish its first generation.
/// Prefers the existing generation-publication broadcast over sleep polling.
async fn wait_for_initial_generation(
    registry: &CodeIndexSchedulerRegistryV1,
    path: &Path,
) -> tracedecay_domain::CodeGenerationId {
    if let Some(generation) = registry.latest_generation_id(path).await {
        return generation;
    }
    let mut publications = registry.subscribe_generation_publications();
    if let Some(generation) = registry.latest_generation_id(path).await {
        return generation;
    }
    let canonical = canonical_fixture_path(path);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let event = publications.recv().await.expect("generation publication");
            if event.project_root == canonical {
                break event.generation_id;
            }
        }
    })
    .await
    .expect("initial generation published")
}

/// Wait until the mounted worktree publishes a generation distinct from `previous`.
async fn wait_for_generation_change(
    registry: &CodeIndexSchedulerRegistryV1,
    path: &Path,
    previous: &tracedecay_domain::CodeGenerationId,
) -> tracedecay_domain::CodeGenerationId {
    if let Some(generation) = registry.latest_generation_id(path).await
        && &generation != previous
    {
        return generation;
    }
    let mut publications = registry.subscribe_generation_publications();
    if let Some(generation) = registry.latest_generation_id(path).await
        && &generation != previous
    {
        return generation;
    }
    let canonical = canonical_fixture_path(path);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let event = publications.recv().await.expect("generation publication");
            if event.project_root == canonical && &event.generation_id != previous {
                break event.generation_id;
            }
        }
    })
    .await
    .expect("changed generation published")
}

#[tokio::test]
async fn semantic_mcp_abstention_uses_freshest_sealed_generation() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(1);
    registry
        .mount_worktree(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("mount scheduler");
    let initial = wait_for_initial_generation(&registry, fixture.path()).await;

    let first = registry.semantic_mcp_abstention(fixture.path()).await;
    assert_eq!(first.code_generation.as_deref(), Some(initial.as_str()));
    assert_eq!(first.reason, "semantic_runtime_unavailable");

    fixture.edit("src/lib.rs", "pub fn alpha() -> u32 { 2 }\n");
    git(fixture.path(), &["commit", "-qam", "external"]);
    // Query admission schedules the reconcile instead of running it inline, so
    // the out-of-band commit lands on the background worker. The abstention
    // still reports the freshest *sealed* generation; it just no longer forces
    // the rebuild that seals it onto whichever request arrived first.
    let _ = registry.semantic_mcp_abstention(fixture.path()).await;
    wait_for_generation_change(&registry, fixture.path(), &initial).await;
    let refreshed = registry.semantic_mcp_abstention(fixture.path()).await;
    assert_ne!(refreshed.code_generation.as_deref(), Some(initial.as_str()));
    assert_eq!(
        refreshed.code_generation.as_deref(),
        registry
            .latest_generation_id(fixture.path())
            .await
            .as_ref()
            .map(tracedecay_domain::CodeGenerationId::as_str)
    );
    assert_eq!(refreshed.reason, "semantic_runtime_unavailable");
    registry.shutdown().await;
}

#[tokio::test]
async fn freshness_failure_does_not_serve_a_stale_complete_generation() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(1);
    registry
        .mount_worktree(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("mount scheduler");
    wait_for_initial_generation(&registry, fixture.path()).await;

    let git_dir = fixture.path().join(".git");
    let unavailable_git_dir = fixture.path().join(".git-unavailable");
    std::fs::rename(&git_dir, &unavailable_git_dir).expect("hide git authority");
    let latest = registry.latest_complete_fresh(fixture.path()).await;
    std::fs::rename(&unavailable_git_dir, &git_dir).expect("restore git authority");

    assert!(
        latest.is_none(),
        "failed freshness resolution must fail closed instead of serving stale data"
    );
    registry.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn expired_query_does_not_wait_for_a_busy_scheduler() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(1);
    registry
        .mount_worktree(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("mount scheduler");
    let generation = wait_for_initial_generation(&registry, fixture.path()).await;
    let latest = registry
        .latest_complete_fresh(fixture.path())
        .await
        .expect("latest generation");
    let operation =
        callable_code_operation(CallableCodeOperationKind::ExactOccurrence).expect("operation");
    let context = application_context(
        &operation,
        latest.generation.snapshot().repository.clone(),
        latest
            .generation
            .snapshot()
            .worktree
            .clone()
            .expect("worktree identity"),
    )
    .with_deadline(Deadline::new(UtcMicros(1)).expect("expired deadline"));
    let request = ExactOccurrenceRequest::new(
        "alpha",
        None,
        CodeQueryScope::new(generation, None).expect("scope"),
        query_meta(),
    )
    .expect("request");

    let scheduler = registry
        .scheduler_handle(fixture.path())
        .await
        .expect("scheduler");
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let lock_thread = std::thread::spawn(move || {
        let _guard = scheduler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        held_tx.send(()).expect("signal scheduler lock held");
        std::thread::sleep(Duration::from_millis(300));
    });
    held_rx.recv().expect("scheduler lock acquired");

    let started = std::time::Instant::now();
    let outcome = registry
        .exact_occurrence(
            RetrievalPortContext {
                request: &context,
                operation: &operation,
            },
            &request,
        )
        .await;
    let elapsed = started.elapsed();
    lock_thread.join().expect("scheduler lock thread joins");

    assert!(matches!(outcome, RetrievalPortOutcome::Unavailable(_)));
    assert!(
        elapsed < Duration::from_millis(100),
        "expired query waited {elapsed:?} for scheduler work"
    );
    registry.shutdown().await;
}

#[test]
fn same_content_head_move_publishes_new_source_identity() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let store = TempDir::new().expect("store root");
    let mut scheduler = scheduler(
        &fixture,
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );
    let initial = published(scheduler.reconcile_now().expect("initial reconcile"));
    let initial_generation = scheduler.latest_complete().expect("initial generation");
    assert_eq!(
        initial_generation.generation.snapshot().source_revision,
        Some(
            CommitId::new(git_stdout(fixture.path(), &["rev-parse", "HEAD"]))
                .expect("initial commit id")
        )
    );

    git(
        fixture.path(),
        &["commit", "--allow-empty", "-qm", "same tree"],
    );
    let moved_head =
        CommitId::new(git_stdout(fixture.path(), &["rev-parse", "HEAD"])).expect("moved HEAD");
    let refreshed = published(scheduler.reconcile_now().expect("HEAD reconcile"));
    let served = scheduler.latest_complete().expect("refreshed generation");

    assert_ne!(refreshed.generation_id, initial.generation_id);
    assert_eq!(
        refreshed.snapshot_content_identity, initial.snapshot_content_identity,
        "same tree content remains physically reusable"
    );
    assert_eq!(
        served.generation.snapshot().source_revision,
        Some(moved_head)
    );
    assert_eq!(
        served
            .generation
            .snapshot()
            .reference
            .as_ref()
            .map(RefId::as_str),
        Some("refs/heads/main")
    );
}

#[tokio::test]
async fn unpinned_query_resolves_exact_admitted_worktree_scope() {
    let left = GitFixture::new(&[("src/lib.rs", "pub fn left_only() {}\n")]);
    let right = GitFixture::new(&[("src/lib.rs", "pub fn right_only() {}\n")]);
    let (first, target, target_literal) = if left.path().canonicalize().expect("left root")
        < right.path().canonicalize().expect("right root")
    {
        (&left, &right, "right_only")
    } else {
        (&right, &left, "left_only")
    };
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(2);
    registry
        .mount_worktree(
            ProjectId::new("project.unpinned.first").expect("valid project"),
            first.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("mount first worktree");
    registry
        .mount_worktree(
            test_project_id(),
            target.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("mount target worktree");
    wait_for_initial_generation(&registry, first.path()).await;
    let target_generation = wait_for_initial_generation(&registry, target.path()).await;
    let target_latest = registry
        .latest_complete_fresh(target.path())
        .await
        .expect("target generation");
    let repository = target_latest.generation.snapshot().repository.clone();
    let worktree = target_latest
        .generation
        .snapshot()
        .worktree
        .clone()
        .expect("target worktree identity");
    let operation =
        callable_code_operation(CallableCodeOperationKind::ExactOccurrence).expect("operation");
    let context = application_context(&operation, repository, worktree);
    mount_query_authority(
        &registry,
        target.path(),
        &context,
        target_latest.generation.manifest().privacy_domain.clone(),
    )
    .await;
    let scope =
        CodeQueryScope::new(super::queries::unpinned_latest_generation(), None).expect("scope");
    let request = ExactOccurrenceRequest::new(target_literal, None, scope, query_meta())
        .expect("exact request");

    let outcome = registry
        .exact_occurrence(
            RetrievalPortContext {
                request: &context,
                operation: &operation,
            },
            &request,
        )
        .await;
    let served = match outcome {
        RetrievalPortOutcome::Completed(evidence) => {
            let page = evidence.payload.expect("exact page");
            assert!(!page.items.is_empty(), "target-only symbol is returned");
            page.generation
        }
        other => panic!("expected completed scoped query, got {other:?}"),
    };
    assert_eq!(served, target_generation);
    registry.shutdown().await;
}

#[tokio::test]
async fn unpinned_cursor_continues_on_its_immutable_generation() {
    let fixture = GitFixture::new(&[(
        "src/lib.rs",
        "mod a { pub fn shared() {} }\nmod b { pub fn shared() {} }\nmod c { pub fn shared() {} }\n",
    )]);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(1);
    registry
        .mount_worktree(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("mount worktree");
    wait_for_initial_generation(&registry, fixture.path()).await;
    let initial = registry
        .latest_complete_fresh(fixture.path())
        .await
        .expect("initial generation");
    let operation =
        callable_code_operation(CallableCodeOperationKind::ExactOccurrence).expect("operation");
    let context = application_context(
        &operation,
        initial.generation.snapshot().repository.clone(),
        initial
            .generation
            .snapshot()
            .worktree
            .clone()
            .expect("worktree identity"),
    );
    mount_query_authority(
        &registry,
        fixture.path(),
        &context,
        initial.generation.manifest().privacy_domain.clone(),
    )
    .await;
    let scope =
        CodeQueryScope::new(super::queries::unpinned_latest_generation(), None).expect("scope");
    let first_request = ExactOccurrenceRequest::new(
        "shared",
        None,
        scope.clone(),
        RetrievalRequestMeta::current(
            PageRequest::first(1).expect("first page"),
            ResultProjection::Evidence,
            RetrievalOrder::Relevance,
        ),
    )
    .expect("first request");
    let first = registry
        .exact_occurrence(
            RetrievalPortContext {
                request: &context,
                operation: &operation,
            },
            &first_request,
        )
        .await;
    let first_page = match first {
        RetrievalPortOutcome::Completed(evidence) => evidence.payload.expect("first page"),
        other => panic!("expected first page, got {other:?}"),
    };
    let cursor = first_page.next_cursor.clone().expect("continuation cursor");
    let original_generation = first_page.generation.clone();

    // The envelope prefix names the cursor wire revision and is bumped whenever
    // that contract changes (it is `ccq2.` today). Take it from the cursor the
    // production path just minted rather than pinning a literal here: this test
    // is about expiry tampering, not about which revision is current.
    let (prefix, encoded) = cursor
        .as_str()
        .split_once('.')
        .expect("callable cursor revision prefix");
    let mut tampered: serde_json::Value =
        serde_json::from_slice(&hex::decode(encoded).expect("cursor hex")).expect("cursor JSON");
    tampered["payload"]["expires_at"] = serde_json::json!(0);
    let tampered = OpaqueCursor::new(format!(
        "{prefix}.{}",
        hex::encode(serde_json::to_vec(&tampered).expect("tampered cursor JSON"))
    ))
    .expect("tampered cursor");
    let tampered_request = ExactOccurrenceRequest::new(
        "shared",
        None,
        scope.clone(),
        RetrievalRequestMeta::current(
            PageRequest::new(1, Some(tampered)).expect("tampered continuation page"),
            ResultProjection::Evidence,
            RetrievalOrder::Relevance,
        ),
    )
    .expect("tampered continuation request");
    let tampered_outcome = registry
        .exact_occurrence(
            RetrievalPortContext {
                request: &context,
                operation: &operation,
            },
            &tampered_request,
        )
        .await;
    let RetrievalPortOutcome::Unavailable(tampered_evidence) = tampered_outcome else {
        panic!("tampered cursor must be rejected");
    };
    assert_eq!(
        tampered_evidence.omissions[0].reason,
        OmissionReason::Failed,
        "MAC verification must precede expiry and other binding diagnostics"
    );

    fixture.edit(
        "src/lib.rs",
        "mod a { pub fn shared() {} }\nmod b { pub fn shared() {} }\nmod c { pub fn shared() {} }\npub fn unrelated() {}\n",
    );
    git(fixture.path(), &["commit", "-qam", "refresh"]);
    // Serve-old-first: the freshness ladder answers from the retained
    // generation and only *requests* the rebuild, so the new generation
    // arrives from the background worker rather than from this call.
    let _requested = registry
        .latest_complete_fresh(fixture.path())
        .await
        .expect("retained generation stays servable while the rebuild runs");
    let refreshed =
        wait_for_generation_change(&registry, fixture.path(), &original_generation).await;
    assert_ne!(refreshed, original_generation);

    let continuation_request = ExactOccurrenceRequest::new(
        "shared",
        None,
        scope,
        RetrievalRequestMeta::current(
            PageRequest::new(1, Some(cursor)).expect("continuation page"),
            ResultProjection::Evidence,
            RetrievalOrder::Relevance,
        ),
    )
    .expect("continuation request");
    let continuation = registry
        .exact_occurrence(
            RetrievalPortContext {
                request: &context,
                operation: &operation,
            },
            &continuation_request,
        )
        .await;
    let continuation_page = match continuation {
        RetrievalPortOutcome::Completed(evidence) => evidence.payload.expect("continuation page"),
        other => panic!("expected continuation page, got {other:?}"),
    };
    assert_eq!(continuation_page.generation, original_generation);
    assert_eq!(continuation_page.items.len(), 1);
    registry.shutdown().await;
}

#[tokio::test]
async fn pinned_generation_from_another_worktree_is_unavailable() {
    let owner = GitFixture::new(&[("src/lib.rs", "pub fn owner_only() {}\n")]);
    let requester = GitFixture::new(&[("src/lib.rs", "pub fn requester_only() {}\n")]);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(2);
    registry
        .mount_worktree(
            ProjectId::new("project.pinned.owner").expect("valid project"),
            owner.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("mount owner worktree");
    registry
        .mount_worktree(
            test_project_id(),
            requester.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("mount requester worktree");
    let owner_generation = wait_for_initial_generation(&registry, owner.path()).await;
    wait_for_initial_generation(&registry, requester.path()).await;
    let requester_latest = registry
        .latest_complete_fresh(requester.path())
        .await
        .expect("requester generation");
    let operation =
        callable_code_operation(CallableCodeOperationKind::ExactOccurrence).expect("operation");
    let context = application_context(
        &operation,
        requester_latest.generation.snapshot().repository.clone(),
        requester_latest
            .generation
            .snapshot()
            .worktree
            .clone()
            .expect("requester worktree identity"),
    );
    let scope = CodeQueryScope::new(owner_generation, None).expect("scope");
    let request =
        ExactOccurrenceRequest::new("owner_only", None, scope, query_meta()).expect("request");

    assert!(matches!(
        registry
            .exact_occurrence(
                RetrievalPortContext {
                    request: &context,
                    operation: &operation,
                },
                &request,
            )
            .await,
        RetrievalPortOutcome::Unavailable(_)
    ));
    registry.shutdown().await;
}

#[tokio::test]
async fn symbol_search_is_generation_bound_and_uses_mounted_authority() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() {}\n")]);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(1);
    registry
        .mount_worktree(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("mount worktree");
    let generation = wait_for_initial_generation(&registry, fixture.path()).await;
    let latest = registry
        .latest_complete_fresh(fixture.path())
        .await
        .expect("latest generation");
    let operation =
        callable_code_operation(CallableCodeOperationKind::SymbolSearch).expect("operation");
    let context = application_context(
        &operation,
        latest.generation.snapshot().repository.clone(),
        latest
            .generation
            .snapshot()
            .worktree
            .clone()
            .expect("worktree identity"),
    );
    mount_query_authority(
        &registry,
        fixture.path(),
        &context,
        latest.generation.manifest().privacy_domain.clone(),
    )
    .await;
    let query = EphemeralSanitizedQueryViewV1::sanitize(
        "alpha",
        SanitizerRevision::new("sanitizer.query.fixture").expect("sanitizer"),
        QueryNormalizationRevision::new("normalization.query.fixture").expect("normalization"),
    )
    .expect("query");
    let request = CodeSymbolSearchRequest {
        query,
        scope: CodeQueryScope::new(generation.clone(), None).expect("scope"),
        meta: query_meta(),
    };

    let outcome = registry
        .symbol_search(
            RetrievalPortContext {
                request: &context,
                operation: &operation,
            },
            &request,
        )
        .await;
    let RetrievalPortOutcome::Completed(evidence) = outcome else {
        panic!("mounted symbol-search authority must complete, got {outcome:?}");
    };
    let page = evidence.payload.expect("symbol-search page");
    assert_eq!(page.generation, generation);
    assert!(
        page.items.iter().any(|symbol| symbol.name == "alpha"),
        "implemented symbol search must return the indexed symbol"
    );
    registry.shutdown().await;
}

/// A query that pins no explicit generation (the reserved unpinned sentinel)
/// resolves its serving generation through the three-tier freshness ladder. An
/// out-of-band git commit after indexing is caught by the tier-1 `.git`
/// metadata check at query admission, so the unpinned query serves the freshly
/// reconciled latest generation rather than the stale one indexed at mount.
#[tokio::test]
async fn unpinned_query_serves_freshness_resolved_latest_generation() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(1);
    registry
        .mount_worktree(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("mount daemon-owned scheduler");
    let initial = wait_for_initial_generation(&registry, fixture.path()).await;

    // Capture the stable repository/worktree identity from the generation
    // indexed at mount, before the out-of-band change moves HEAD.
    let latest = registry
        .latest_complete_fresh(fixture.path())
        .await
        .expect("mounted generation");
    assert_eq!(latest.generation.manifest().generation_id, initial);
    let repository = latest.generation.snapshot().repository.clone();
    let worktree = latest
        .generation
        .snapshot()
        .worktree
        .clone()
        .expect("worktree identity");

    // Another process commits a change. Nothing notifies the scheduler, and no
    // filesystem watcher exists, so `latest_complete` still reports `initial`
    // until a freshness check runs at query admission.
    fixture.edit("src/lib.rs", "pub fn alpha() -> u32 { 2 }\n");
    git(fixture.path(), &["commit", "-qam", "external"]);
    assert_eq!(
        registry.latest_generation_id(fixture.path()).await,
        Some(initial.clone()),
        "an out-of-band commit is not reflected until the freshness ladder runs"
    );

    // Query admission runs the ladder's *checks* inline but never its rebuild:
    // the reconcile is handed to the background worker so no request pays
    // O(store) for it. The commit therefore lands a moment later, not on the
    // first query, and the unpinned query then serves it.
    let _ = registry.latest_complete_fresh(fixture.path()).await;
    wait_for_generation_change(&registry, fixture.path(), &initial).await;

    // An unpinned query resolves the serving generation through the ladder.
    let operation =
        callable_code_operation(CallableCodeOperationKind::ExactOccurrence).expect("operation");
    let context = application_context(&operation, repository, worktree);
    mount_query_authority(
        &registry,
        fixture.path(),
        &context,
        latest.generation.manifest().privacy_domain.clone(),
    )
    .await;
    let scope =
        CodeQueryScope::new(super::queries::unpinned_latest_generation(), None).expect("scope");
    let request =
        ExactOccurrenceRequest::new("alpha", None, scope, query_meta()).expect("exact request");
    let outcome = registry
        .exact_occurrence(
            RetrievalPortContext {
                request: &context,
                operation: &operation,
            },
            &request,
        )
        .await;

    let served = match outcome {
        RetrievalPortOutcome::Completed(evidence) => {
            evidence.payload.expect("exact page").generation
        }
        other => panic!("expected a completed unpinned query, got {other:?}"),
    };
    assert_ne!(
        served, initial,
        "the unpinned query serves the freshness-resolved latest generation, not the stale one"
    );
    assert_eq!(
        Some(served),
        registry.latest_generation_id(fixture.path()).await,
        "query admission reconciled the out-of-band commit into the served generation"
    );

    registry.shutdown().await;
}

/// An explicit caller-pinned generation is served generation-bound and
/// read-only: the freshness ladder is bypassed, so an out-of-band commit after
/// indexing never mutates the served generation and never triggers a reconcile.
#[tokio::test]
async fn pinned_query_bypasses_freshness_resolution() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(1);
    registry
        .mount_worktree(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("mount daemon-owned scheduler");
    let initial = wait_for_initial_generation(&registry, fixture.path()).await;

    let latest = registry
        .latest_complete_fresh(fixture.path())
        .await
        .expect("mounted generation");
    assert_eq!(latest.generation.manifest().generation_id, initial);
    let repository = latest.generation.snapshot().repository.clone();
    let worktree = latest
        .generation
        .snapshot()
        .worktree
        .clone()
        .expect("worktree identity");

    // The same out-of-band commit as the unpinned case: it would be caught by
    // the tier-1 metadata check *if* the freshness ladder ran.
    fixture.edit("src/lib.rs", "pub fn alpha() -> u32 { 2 }\n");
    git(fixture.path(), &["commit", "-qam", "external"]);

    // The caller pins the exact generation indexed at mount.
    let operation =
        callable_code_operation(CallableCodeOperationKind::ExactOccurrence).expect("operation");
    let context = application_context(&operation, repository, worktree);
    mount_query_authority(
        &registry,
        fixture.path(),
        &context,
        latest.generation.manifest().privacy_domain.clone(),
    )
    .await;
    let scope = CodeQueryScope::new(initial.clone(), None).expect("scope");
    let request =
        ExactOccurrenceRequest::new("alpha", None, scope, query_meta()).expect("exact request");
    let outcome = registry
        .exact_occurrence(
            RetrievalPortContext {
                request: &context,
                operation: &operation,
            },
            &request,
        )
        .await;

    let served = match outcome {
        RetrievalPortOutcome::Completed(evidence) => {
            evidence.payload.expect("exact page").generation
        }
        other => panic!("expected a completed pinned query, got {other:?}"),
    };
    assert_eq!(
        served, initial,
        "a pinned query serves exactly the requested generation, bypassing freshness"
    );
    assert_eq!(
        registry.latest_generation_id(fixture.path()).await,
        Some(initial),
        "the pinned read never triggers a reconcile of the out-of-band commit"
    );

    registry.shutdown().await;
}

/// End-to-end proof that the diagnostics identity split is closed.
///
/// The compiler pillar publishes through its real production entry point
/// (`publish_compiler_diagnostics_through_code_index_v1`, the exact call the
/// `tracedecay_diagnose` handler makes) with identity resolved from the real
/// mounted `CodeIndexSchedulerRegistryV1`. The published records are then driven
/// through the real `DiagnosticsStoreLspFeedbackProjection` against a feedback
/// cycle whose impact target carries the same registry-minted identity.
///
/// Before the identity was unified, the producer minted
/// `FileOccurrenceId::new("src/lib.rs")` and its own
/// `generation.diagnostics.compiler.<digest>`, so the projection refused every
/// record with `ImpactTargetFileMismatch` / `GenerationMismatch` and LSP
/// Problems stayed empty. This test asserts admission, not refusal.
#[tokio::test]
async fn compiler_diagnostics_published_under_registry_identity_are_admitted_by_the_lsp_projection()
{
    use std::collections::{BTreeMap, BTreeSet};

    use crate::diagnostics_publication::{
        CodeIndexPublicationIdentityPortV1, CompilerDiagnosticPublicationOutcomeV1,
        publish_compiler_diagnostics_through_code_index_v1,
    };
    use crate::diagnostics_store::DiagnosticsStore;
    use tracedecay_domain::feedback::{
        FeedbackCycleId, FeedbackCycleResultV1, FeedbackCycleTerminationV1,
        FeedbackDiagnosticClassificationV1, FeedbackDiagnosticProducerV1,
        FeedbackDiagnosticProjectionV1, FeedbackDurabilityV1, FeedbackFindingId,
        FeedbackFindingLifecycleV1, FeedbackFindingV1, FeedbackImpactStateV1, FeedbackImpactV1,
        FeedbackResultId, FeedbackScopeV1, FeedbackTargetV1, ProviderEvaluationStateV1,
    };
    use tracedecay_domain::{ComponentVersion, ContentDigest, DiagnosticSeverityV1, SourceSpan};
    use tracedecay_lsp::{AdmittedRoot, DiagnosticSource, LspRuntimeFailure, LspRuntimeFuture};
    use tracedecay_usecases::lsp_runtime::{
        DiagnosticsStoreLspFeedbackProjection, LspCodeIndexProjectionIdentityPort,
        LspFeedbackDiagnosticProjectionPort, LspFeedbackDocumentSnapshot,
        LspFeedbackDocumentSnapshotPort, LspFeedbackProjectionScope,
    };

    struct FixedDocument(String);

    impl LspFeedbackDocumentSnapshotPort for FixedDocument {
        fn snapshot(
            &self,
            _root: AdmittedRoot,
            _document_uri: String,
        ) -> LspRuntimeFuture<Result<LspFeedbackDocumentSnapshot, LspRuntimeFailure>> {
            let text = self.0.clone();
            Box::pin(async move { Ok(LspFeedbackDocumentSnapshot { text }) })
        }
    }

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        <T as TryFrom<String>>::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).expect("valid fixture identity")
    }

    let source = "pub fn alpha() -> u32 {\n    let value: u32 = \"nope\";\n    value\n}\n";
    let fixture = GitFixture::new(&[("src/lib.rs", source)]);
    let store_root = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(1);
    assert!(
        registry
            .mount_worktree(
                test_project_id(),
                fixture.path(),
                store_root.path().to_path_buf(),
                None,
            )
            .await
            .expect("mount daemon-owned scheduler")
    );
    wait_for_initial_generation(&registry, fixture.path()).await;

    // The one mint: file identity and generation both come from the registry.
    let identity =
        CodeIndexPublicationIdentityPortV1::resolve(&registry, fixture.path().to_path_buf())
            .await
            .expect("code-index generation identity");
    let (indexed_file, indexed_digest) = identity
        .file("src/lib.rs")
        .expect("code index contains the fixture source");
    let indexed_file = indexed_file.clone();
    let indexed_digest = indexed_digest.clone();
    assert!(
        indexed_file.as_str().starts_with("file.daemon."),
        "registry must mint daemon file identity, got {}",
        indexed_file.as_str()
    );
    let generation = identity.generation_id().clone();

    let database_root = TempDir::new().expect("database root");
    let database_path = database_root.path().join("diagnostics.db");
    let authority = crate::db::DatabaseAuthority::acquire_test(
        &database_path,
        "diagnostics identity admission test",
    )
    .expect("database authority");
    crate::daemon::store_runtime::register_registered_schema_installer();
    let (database, _guard) = crate::db::Database::publish_test_runtime(
        &database_path,
        &authority,
        crate::db::TestDatabaseRuntimeMode::Initialize,
    )
    .await
    .expect("open diagnostics database");

    let parsed = crate::diagnose::parse_cargo_output(
        "error[E0308]: mismatched types\n  --> src/lib.rs:2:22\n",
    );
    assert_eq!(parsed.len(), 1, "fixture cargo output must parse");

    let outcome = {
        let store = DiagnosticsStore::new(database.clone());
        publish_compiler_diagnostics_through_code_index_v1(
            fixture.path(),
            Some(&registry as &dyn CodeIndexPublicationIdentityPortV1),
            &store,
            &parsed,
            ComponentVersion::new("analyzer.tracedecay-diagnose.test".to_owned())
                .expect("analyzer revision"),
            ComponentVersion::new("configuration.tracedecay-diagnose.v1".to_owned())
                .expect("configuration revision"),
        )
        .await
    };
    let CompilerDiagnosticPublicationOutcomeV1::Published {
        generation: published_generation,
        report,
        unresolved,
    } = outcome
    else {
        panic!("compiler publication did not reach the store: {outcome:?}");
    };
    assert!(unresolved.is_empty(), "unexpected skips: {unresolved:?}");
    assert_eq!(report.inserted, 1);
    assert!(report.rejected.is_empty());
    assert_eq!(
        published_generation, generation,
        "records must publish under the code-index generation"
    );

    let record = {
        let store = DiagnosticsStore::new(database.clone());
        store
            .current_records(&generation)
            .await
            .expect("read published records")
            .pop()
            .expect("one published record")
    };
    assert_eq!(record.file_occurrence_id, indexed_file);
    assert_eq!(record.content_digest, indexed_digest);

    // The saved-edit cycle's impact target is minted by the same authority, so
    // the projection's identity comparison can succeed.
    let head_commit = git_stdout(fixture.path(), &["rev-parse", "HEAD"]);
    let projection_identity = LspCodeIndexProjectionIdentityPort::current_identity(
        &registry,
        fixture.path().to_path_buf(),
        Some("src/lib.rs".to_owned()),
    )
    .await
    .expect("lsp code-index projection identity");
    let document_file_occurrence_id = projection_identity
        .document_file_occurrence_id
        .clone()
        .expect("document file occurrence identity");
    let document_content_digest: ContentDigest = projection_identity
        .document_content_digest
        .clone()
        .expect("document content digest");
    assert_eq!(document_file_occurrence_id, indexed_file);
    assert_eq!(document_content_digest, indexed_digest);

    let finding = FeedbackFindingV1 {
        finding_id: id::<FeedbackFindingId>("finding.diagnostics.admission"),
        classification: FeedbackDiagnosticClassificationV1::New,
        lifecycle: FeedbackFindingLifecycleV1::Active,
        retrieval_anchor_id: Some(record.diagnostic_anchor.clone()),
        provider_state: ProviderEvaluationStateV1::SupportedCompletedComplete,
        safe_bounded_preview: None,
        diagnostic_projection: None,
    };
    let cycle = FeedbackCycleResultV1 {
        result_id: id::<FeedbackResultId>("result.diagnostics.admission"),
        cycle_id: id::<FeedbackCycleId>("cycle.diagnostics.admission"),
        scope: FeedbackScopeV1 {
            project_id: test_project_id(),
            repository_id: id(record.repository.as_str()),
            worktree_id: id(record
                .worktree
                .as_ref()
                .expect("worktree identity")
                .as_str()),
            branch_ref: "refs/heads/main".to_owned(),
            head_commit_id: id(&head_commit),
        },
        content_identity: None,
        durability: FeedbackDurabilityV1::Durable,
        policy_digest: projection_identity.snapshot_digest.clone(),
        configuration_digest: projection_identity.invalidation_digest.clone(),
        termination: FeedbackCycleTerminationV1::Clean,
        provider_states: vec![ProviderEvaluationStateV1::SupportedCompletedComplete],
        advisory_provider_states: Vec::new(),
        baseline_states: Vec::new(),
        impact: Some(FeedbackImpactV1 {
            target: FeedbackTargetV1 {
                file: indexed_file.clone(),
                span: None,
                symbol: None,
                generation_id: Some(generation.clone()),
            },
            affected_files: vec![indexed_file.clone()],
            affected_callers: Vec::new(),
            affected_tests: Vec::new(),
            evidence_anchors: Vec::new(),
            state: FeedbackImpactStateV1::Partial,
            affected_tests_state: FeedbackImpactStateV1::Partial,
        }),
        impact_state: Some(FeedbackImpactStateV1::Partial),
        affected_tests_state: Some(FeedbackImpactStateV1::Partial),
        findings: vec![finding],
        total_findings: 1,
        returned_findings: 1,
        omitted_findings: 0,
        advisory_only: false,
    };
    let advisory_cycle = FeedbackCycleResultV1 {
        advisory_provider_states: vec![
            tracedecay_domain::feedback::FeedbackAdvisoryProviderStateV1 {
                producer: FeedbackDiagnosticProducerV1::GitHubReview,
                state: ProviderEvaluationStateV1::SupportedCompletedComplete,
            },
        ],
        findings: vec![FeedbackFindingV1 {
            finding_id: id("finding.github.direct-projection"),
            classification: FeedbackDiagnosticClassificationV1::Unknown,
            lifecycle: FeedbackFindingLifecycleV1::Active,
            retrieval_anchor_id: Some(id("anchor.github.evidence-only")),
            provider_state: ProviderEvaluationStateV1::SupportedCompletedComplete,
            safe_bounded_preview: None,
            diagnostic_projection: Some(FeedbackDiagnosticProjectionV1 {
                file: indexed_file.clone(),
                span: SourceSpan {
                    start_byte: 0,
                    end_byte: 2,
                },
                symbol: None,
                code: "github-review".to_owned(),
                severity: DiagnosticSeverityV1::Information,
                safe_bounded_message: "Unresolved GitHub review comment".to_owned(),
                producer: FeedbackDiagnosticProducerV1::GitHubReview,
                code_description_uri: Some(
                    "https://github.com/ScriptedAlchemy/tracedecay/pull/13#discussion_r1"
                        .to_owned(),
                ),
            }),
        }],
        total_findings: 1,
        returned_findings: 1,
        omitted_findings: 0,
        ..cycle.clone()
    };

    let document_uri = url::Url::from_file_path(fixture.path().join("src/lib.rs"))
        .expect("document uri")
        .to_string();
    let root_uri = url::Url::from_file_path(fixture.path())
        .expect("root uri")
        .to_string();
    let projection = DiagnosticsStoreLspFeedbackProjection::new(
        Arc::new(
            tracedecay_usecases::feedback::diagnostics::DatabaseDiagnosticStore::new(
                database.clone(),
            ),
        ),
        Arc::new(FixedDocument(source.to_owned())),
    );
    let published = projection
        .project(
            AdmittedRoot::new(root_uri.clone()),
            document_uri.clone(),
            LspFeedbackProjectionScope {
                head_commit_id: id(&head_commit),
                code_generation_id: generation.clone(),
                snapshot_digest: projection_identity.snapshot_digest.clone(),
                invalidation_digest: projection_identity.invalidation_digest.clone(),
                snapshot_content_digest: projection_identity.snapshot_content_digest.clone(),
                document_file_occurrence_id: Some(document_file_occurrence_id.clone()),
                document_content_digest: Some(document_content_digest.clone()),
                document_relative_path: Some("src/lib.rs".to_owned()),
                generation: 1,
            },
            cycle,
            BTreeMap::new(),
        )
        .await
        .expect("projection succeeds");

    assert_eq!(
        published.len(),
        1,
        "the published compiler record must be admitted, not skipped"
    );
    assert_eq!(published[0].uri, document_uri);
    assert_eq!(published[0].code.as_deref(), Some("E0308"));
    assert_eq!(
        published[0].source,
        DiagnosticSource::TraceDecay,
        "the compiler pillar must name its own producer source"
    );
    let sources: BTreeSet<_> = published.iter().map(|entry| entry.source).collect();
    assert!(sources.iter().all(|source| source.is_tracedecay()));

    let advisory = projection
        .project(
            AdmittedRoot::new(root_uri),
            document_uri,
            LspFeedbackProjectionScope {
                head_commit_id: id(&head_commit),
                code_generation_id: generation,
                snapshot_digest: projection_identity.snapshot_digest,
                invalidation_digest: projection_identity.invalidation_digest,
                snapshot_content_digest: projection_identity.snapshot_content_digest,
                document_file_occurrence_id: Some(document_file_occurrence_id),
                document_content_digest: Some(document_content_digest),
                document_relative_path: Some("src/lib.rs".to_owned()),
                generation: 2,
            },
            advisory_cycle,
            BTreeMap::new(),
        )
        .await
        .expect("advisory projection succeeds");
    assert_eq!(
        advisory.len(),
        1,
        "a bounded advisory code projection must not require its evidence anchor in the diagnostic store"
    );
    assert_eq!(advisory[0].source, DiagnosticSource::TraceDecayGitHub);
    assert_eq!(advisory[0].code.as_deref(), Some("github-review"));

    registry.shutdown().await;
}

/// A producer with no reachable code-index authority publishes nothing and says
/// so. The former behaviour minted a repository-relative file identity, which
/// the LSP projection could only refuse.
#[tokio::test]
async fn compiler_publication_without_a_resolver_is_named_not_guessed() {
    use crate::diagnostics_publication::{
        CompilerDiagnosticPublicationOutcomeV1, publish_compiler_diagnostics_through_code_index_v1,
    };
    use crate::diagnostics_store::DiagnosticsStore;
    use tracedecay_domain::ComponentVersion;

    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let database_root = TempDir::new().expect("database root");
    let database_path = database_root.path().join("diagnostics.db");
    crate::daemon::store_runtime::register_registered_schema_installer();
    let authority =
        crate::db::DatabaseAuthority::acquire_test(&database_path, "diagnostics absent resolver")
            .expect("database authority");
    let (database, _guard) = crate::db::Database::publish_test_runtime(
        &database_path,
        &authority,
        crate::db::TestDatabaseRuntimeMode::Initialize,
    )
    .await
    .expect("open diagnostics database");
    let store = DiagnosticsStore::new(database.clone());

    let parsed = crate::diagnose::parse_cargo_output(
        "error[E0308]: mismatched types\n  --> src/lib.rs:1:1\n",
    );
    let outcome = publish_compiler_diagnostics_through_code_index_v1(
        fixture.path(),
        None,
        &store,
        &parsed,
        ComponentVersion::new("analyzer.tracedecay-diagnose.test".to_owned())
            .expect("analyzer revision"),
        ComponentVersion::new("configuration.tracedecay-diagnose.v1".to_owned())
            .expect("configuration revision"),
    )
    .await;
    assert_eq!(
        outcome,
        CompilerDiagnosticPublicationOutcomeV1::CodeIndexIdentityUnavailable
    );
}

/// A real branch switch under one worktree reconciles to the same immutable
/// publication inputs and code lanes as a clean scan of the switched-to branch.
#[test]
fn real_branch_switch_reconcile_matches_clean_scan() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    git(fixture.path(), &["switch", "-q", "-c", "feature"]);
    fixture.edit("src/lib.rs", "pub fn alpha() -> u32 { 2 }\n");
    git(fixture.path(), &["commit", "-qam", "feature"]);
    git(fixture.path(), &["switch", "-q", "main"]);

    let store = TempDir::new().expect("store root");
    let bytes = Arc::new(SharedCodeIndexBytePoolV1::default());
    let mut live = scheduler(&fixture, store.path().join("live"), Arc::clone(&bytes));
    published(live.reconcile_now().expect("main baseline"));
    let main_snapshot = live
        .latest_complete()
        .expect("main generation")
        .generation()
        .snapshot()
        .clone();

    git(fixture.path(), &["switch", "-q", "feature"]);
    let switched = published(live.reconcile_now().expect("branch-switch reconcile"));
    let switched_generation = live.latest_complete().expect("switched generation");

    let mut clean = scheduler(&fixture, store.path().join("clean"), bytes);
    let clean_publish = published(clean.reconcile_now().expect("clean feature scan"));
    let clean_generation = clean.latest_complete().expect("clean generation");

    assert_eq!(
        switched.snapshot_content_identity,
        clean_publish.snapshot_content_identity
    );
    assert_eq!(switched._lane_digest, clean_publish._lane_digest);
    assert_eq!(
        main_snapshot.reference.as_ref().map(RefId::as_str),
        Some("refs/heads/main")
    );
    assert_eq!(
        switched_generation
            .generation()
            .snapshot()
            .reference
            .as_ref()
            .map(RefId::as_str),
        Some("refs/heads/feature")
    );
    assert_ne!(
        switched_generation.generation().snapshot().source_revision,
        main_snapshot.source_revision
    );
    assert_eq!(
        switched_generation.generation().snapshot().reference,
        clean_generation.generation().snapshot().reference
    );
    assert_eq!(
        switched_generation.generation().snapshot().source_revision,
        clean_generation.generation().snapshot().source_revision
    );
}

/// A real Git rebase reconciles the rewritten branch tip to the same immutable
/// publication inputs and code lanes as a clean scan of the rebased checkout.
#[test]
fn real_rebase_reconcile_matches_clean_scan() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    git(fixture.path(), &["switch", "-q", "-c", "feature"]);
    write(
        fixture.path(),
        "src/feature.rs",
        "pub fn feature() -> u32 { 2 }\n",
    );
    git(fixture.path(), &["add", "."]);
    git(fixture.path(), &["commit", "-qm", "feature"]);

    let store = TempDir::new().expect("store root");
    let bytes = Arc::new(SharedCodeIndexBytePoolV1::default());
    let mut live = scheduler(&fixture, store.path().join("live"), Arc::clone(&bytes));
    published(live.reconcile_now().expect("pre-rebase baseline"));
    let pre_rebase_revision = live
        .latest_complete()
        .expect("pre-rebase generation")
        .generation()
        .snapshot()
        .source_revision
        .clone();

    git(fixture.path(), &["switch", "-q", "main"]);
    write(
        fixture.path(),
        "src/main_only.rs",
        "pub fn main_only() -> u32 { 3 }\n",
    );
    git(fixture.path(), &["add", "."]);
    git(fixture.path(), &["commit", "-qm", "advance main"]);
    git(fixture.path(), &["switch", "-q", "feature"]);
    git(fixture.path(), &["rebase", "-q", "main"]);

    let rebased = published(live.reconcile_now().expect("rebase reconcile"));
    let rebased_generation = live.latest_complete().expect("rebased generation");

    let mut clean = scheduler(&fixture, store.path().join("clean"), bytes);
    let clean_publish = published(clean.reconcile_now().expect("clean rebased scan"));
    let clean_generation = clean.latest_complete().expect("clean generation");

    assert_eq!(
        rebased.snapshot_content_identity,
        clean_publish.snapshot_content_identity
    );
    assert_eq!(rebased._lane_digest, clean_publish._lane_digest);
    assert_eq!(
        rebased_generation
            .generation()
            .snapshot()
            .reference
            .as_ref()
            .map(RefId::as_str),
        Some("refs/heads/feature")
    );
    assert_ne!(
        rebased_generation.generation().snapshot().source_revision,
        pre_rebase_revision
    );
    assert_eq!(
        rebased_generation.generation().snapshot().reference,
        clean_generation.generation().snapshot().reference
    );
    assert_eq!(
        rebased_generation.generation().snapshot().source_revision,
        clean_generation.generation().snapshot().source_revision
    );
}

/// Restoring a sealed generation must keep queries non-blocking AND schedule a
/// verification reconcile. Open-time clocks alone must not suppress cadence.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mount_with_retained_generation_verifies_cadence_promptly() {
    let fixture = GitFixture::new(RETAINED_REVISION_0);
    let store = TempDir::new().expect("store root");
    let bytes = Arc::new(SharedCodeIndexBytePoolV1::default());
    let scoped_store = super::scoped_code_index_store_root(
        store.path(),
        &fixture.path().canonicalize().expect("canonical fixture"),
    );
    let first_generation = {
        let mut scheduler = scheduler(&fixture, scoped_store, Arc::clone(&bytes));
        published(scheduler.reconcile_now().expect("seed generation")).generation_id
    };

    // Out-of-band edit after the sealed generation was written.
    fixture.edit("src/lib.rs", "pub fn retained_revision() -> usize { 1 }\n");
    git(fixture.path(), &["commit", "-qam", "stale-after-seal"]);

    let registry = CodeIndexSchedulerRegistryV1::new(1);
    registry
        .mount_worktree(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("mount with retained generation");

    // The retained generation is not queryable until its freshness frontier is
    // proved. The mount wake must verify against gix and publish the new content.
    let refreshed = wait_for_generation_change(&registry, fixture.path(), &first_generation).await;
    assert_ne!(refreshed, first_generation);

    let receipt = wait_for_event_to_ready(&registry).await;
    assert!(
        matches!(receipt.outcome, CodeIndexCadenceOutcomeV1::Published { .. }),
        "stale retained generation must publish a refreshed generation"
    );
    registry.shutdown().await;
}

/// Content-identical reconcile after mount verification emits a no-op
/// event-to-ready receipt (zero projection work) rather than a silent discard.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mount_verification_noop_emits_event_to_ready_receipt() {
    let fixture = GitFixture::new(ALPHA_LIB_V1);
    let store = TempDir::new().expect("store root");
    let bytes = Arc::new(SharedCodeIndexBytePoolV1::default());
    let scoped_store = super::scoped_code_index_store_root(
        store.path(),
        &fixture.path().canonicalize().expect("canonical fixture"),
    );
    {
        let mut scheduler = scheduler(&fixture, scoped_store.clone(), Arc::clone(&bytes));
        published(scheduler.reconcile_now().expect("seed generation"));
    }
    // Exercise the mount-time verification of a restored generation that carries
    // NO freshness witness (an older seal, or a witness that never landed): the
    // mount must still schedule a verification pass that emits a no-op receipt.
    // The witness-present fast path (mount skips the reconcile) is covered by
    // `witness_verified_mount_skips_reconcile`.
    std::fs::remove_file(scoped_store.join("freshness_witness.v1"))
        .expect("remove restore-time freshness witness");

    let registry = CodeIndexSchedulerRegistryV1::new(1);
    registry
        .mount_worktree(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("mount retained");
    let receipt = wait_for_event_to_ready(&registry).await;
    assert!(
        receipt.is_noop(),
        "unchanged retained content must emit a no-op event-to-ready receipt"
    );
    // The mount wake is an observed arrival, so queue wait and total latency are
    // both measurable, and the dequeue stamp keeps them distinct measurements.
    let queue_delay = receipt
        .queue_delay_micros()
        .expect("mount wake arrival is observed");
    let event_to_ready = receipt
        .event_to_ready_micros()
        .expect("mount wake arrival is observed");
    assert!(
        queue_delay <= event_to_ready,
        "queue wait ({queue_delay}) cannot exceed event-to-ready ({event_to_ready})"
    );
    assert_eq!(
        event_to_ready,
        queue_delay + receipt.service_micros(),
        "event-to-ready must decompose into queue wait plus service time"
    );
    assert_eq!(receipt.trigger, CodeIndexCadenceTriggerV1::Mount);

    // The read model is reachable from the production registry surface.
    let read_model = registry.cadence_read_model();
    assert!(read_model.retained_count >= 1);
    assert!(
        read_model.capacity >= super::cadence::P99_MINIMUM_SAMPLES,
        "ring must be able to hold a p99-eligible population"
    );
    assert_eq!(read_model.latency_sample_count, read_model.retained_count);
    assert_eq!(read_model.arrival_unavailable_count, 0);
    assert!(
        !read_model.event_to_ready_micros.p99.is_available(),
        "p99 must stay unavailable until 100 samples are retained"
    );
    assert!(
        registry
            .latest_generation_id(fixture.path())
            .await
            .is_some(),
        "the verified generation becomes serving state"
    );
    registry.shutdown().await;
}

/// A mount whose restored generation is proved current by its freshness witness
/// activates that generation without rebuilding it.
#[tokio::test(flavor = "multi_thread")]
async fn witness_verified_mount_activates_without_rebuild() {
    let fixture = GitFixture::new(ALPHA_LIB_V1);
    let store = TempDir::new().expect("store root");
    let bytes = Arc::new(SharedCodeIndexBytePoolV1::default());
    let scoped_store = super::scoped_code_index_store_root(
        store.path(),
        &fixture.path().canonicalize().expect("canonical fixture"),
    );
    let seeded = {
        let mut scheduler = scheduler(&fixture, scoped_store.clone(), Arc::clone(&bytes));
        published(scheduler.reconcile_now().expect("seed generation")).generation_id
    };
    assert!(
        scoped_store.join("freshness_witness.v1").is_file(),
        "seeding a generation persists its restore-time freshness witness"
    );

    let registry = CodeIndexSchedulerRegistryV1::new(1);
    registry
        .mount_worktree(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("mount retained");

    // The retained owner decodes and proves the frontier in background. A no-op
    // receipt makes that activation observable while the generation identity
    // proves no replacement was published.
    let receipt = wait_for_event_to_ready(&registry).await;
    assert!(
        receipt.is_noop(),
        "a matching witness activates the retained generation without rebuilding"
    );
    assert_eq!(
        registry.latest_generation_id(fixture.path()).await,
        Some(seeded),
        "the witness-verified mount serves the sealed generation without rebuilding"
    );
    registry.shutdown().await;
}

/// A retained generation is not serving until its persistent graph replay
/// activates. A typed replay failure leaves the slot empty while the ordinary
/// refresh remains pending.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_cold_mount_graph_replay_never_seats_retained_generation() {
    let fixture = GitFixture::new(ALPHA_LIB_V1);
    let store = TempDir::new().expect("store root");
    let bytes = Arc::new(SharedCodeIndexBytePoolV1::default());
    let scoped_store = super::scoped_code_index_store_root(
        store.path(),
        &fixture.path().canonicalize().expect("canonical fixture"),
    );
    let scope = {
        let mut scheduler = scheduler(&fixture, scoped_store, bytes);
        published(scheduler.reconcile_now().expect("seed generation"));
        let latest = scheduler.latest_complete().expect("seeded generation");
        let snapshot = latest.generation.snapshot();
        ResolvedScope::new(
            test_project_id(),
            snapshot.repository.clone(),
            snapshot.worktree.clone().expect("worktree id"),
            snapshot.reference.clone(),
        )
        .expect("resolved scope")
    };

    let profile = TempDir::new().expect("profile root");
    let profile_root = profile.path().join("profile");
    let project_id = test_project_id();
    crate::storage::pin_fixture_repository_identity(fixture.path(), project_id.as_str())
        .expect("project enrollment");
    let identity =
        crate::daemon::profile_identity::load_or_create(&profile_root).expect("profile identity");
    let _database_scope =
        crate::db::enter_daemon_database_scope(&profile_root, 93, "failed cold-mount graph replay")
            .expect("daemon database scope");
    let graph_runtime = Arc::new(
        crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1::open(
            identity,
        )
        .await
        .expect("graph runtime registry"),
    );
    let _writable_project_database = graph_runtime
        .project_memory(project_id.clone(), [fixture.path().to_path_buf()])
        .await
        .expect("initialize writable project database");
    let read_only_project_database = Arc::new(
        graph_runtime
            .project_memory_read_only(project_id.clone(), [fixture.path().to_path_buf()])
            .await
            .expect("read-only project database"),
    );
    assert!(
        read_only_project_database
            .graph_publication_storage()
            .is_err(),
        "the fixture must refuse persistent graph publication"
    );

    let registry = CodeIndexSchedulerRegistryV1::with_background_reconcile_permits(1, 1);
    let admission = registry
        .background_reconcile_admission()
        .acquire_owned()
        .await
        .expect("hold background activation");
    registry
        .mount_worktree_with_graph_runtime(
            project_id,
            fixture.path(),
            store.path().to_path_buf(),
            None,
            Arc::clone(&graph_runtime),
            read_only_project_database,
        )
        .await
        .expect("mount retained generation");

    let scheduler = {
        let mounted = registry.mounted.lock().await;
        Arc::clone(
            &mounted
                .get(&fixture.path().canonicalize().expect("canonical root"))
                .expect("mounted worktree")
                .scheduler,
        )
    };
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let lock_thread = std::thread::spawn(move || {
        let _guard = scheduler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        held_tx.send(()).expect("signal scheduler held");
        let _ = release_rx.recv();
    });
    held_rx.recv().expect("scheduler lock acquired");
    drop(admission);

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if registry.pending_wake_micros_for_scope(&scope).await == Some(0) {
            break;
        }
        assert!(
            std::time::Instant::now() <= deadline,
            "worker did not dequeue the retained graph replay"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    release_tx.send(()).expect("release scheduler");
    lock_thread.join().expect("join scheduler holder");

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if registry
            .pending_wake_micros_for_scope(&scope)
            .await
            .is_some_and(|pending| pending != 0)
        {
            break;
        }
        assert!(
            std::time::Instant::now() <= deadline,
            "failed graph replay did not restore its pending retry arrival"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    assert_eq!(
        registry.latest_generation_id(fixture.path()).await,
        None,
        "a retained generation cannot serve after persistent graph replay fails"
    );
    registry.shutdown().await;
    graph_runtime
        .shutdown_memory_graph_reconciliation_tasks()
        .await
        .expect("join graph reconciliation tasks");
}

/// A retained seal is only a candidate for activation. If source-authority
/// verification fails after the seal decodes, the worker must not copy that
/// unverified generation into the serving slot. The failed arrival remains
/// pending for a later hint, while queries fail fast with the typed unverified
/// state.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_retained_activation_never_installs_unverified_serving_state() {
    let fixture = GitFixture::new(ALPHA_LIB_V1);
    let store = TempDir::new().expect("store root");
    let bytes = Arc::new(SharedCodeIndexBytePoolV1::default());
    let scoped_store = super::scoped_code_index_store_root(
        store.path(),
        &fixture.path().canonicalize().expect("canonical fixture"),
    );
    let scope = {
        let mut scheduler = scheduler(&fixture, scoped_store, bytes);
        published(scheduler.reconcile_now().expect("seed generation"));
        let latest = scheduler.latest_complete().expect("seeded generation");
        let snapshot = latest.generation.snapshot();
        ResolvedScope::new(
            test_project_id(),
            snapshot.repository.clone(),
            snapshot.worktree.clone().expect("worktree id"),
            snapshot.reference.clone(),
        )
        .expect("resolved scope")
    };

    let registry = CodeIndexSchedulerRegistryV1::with_background_reconcile_permits(1, 1);
    let admission = registry
        .background_reconcile_admission()
        .acquire_owned()
        .await
        .expect("hold background activation");
    registry
        .mount_worktree(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("mount retained generation");

    // Hold the scheduler after the worker dequeues the mount arrival. This
    // makes the attempted activation observable through the existing pending
    // arrival: zero while the attempt owns it, restored after the failure.
    let scheduler = {
        let mounted = registry.mounted.lock().await;
        Arc::clone(
            &mounted
                .get(&fixture.path().canonicalize().expect("canonical root"))
                .expect("mounted worktree")
                .scheduler,
        )
    };
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let lock_thread = std::thread::spawn(move || {
        let _guard = scheduler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        held_tx.send(()).expect("signal scheduler held");
        let _ = release_rx.recv();
    });
    held_rx.recv().expect("scheduler lock acquired");

    let git_dir = fixture.path().join(".git");
    let unavailable_git_dir = fixture.path().join(".git.activation-unavailable");
    std::fs::rename(&git_dir, &unavailable_git_dir).expect("make Git authority unavailable");
    drop(admission);

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if registry.pending_wake_micros_for_scope(&scope).await == Some(0) {
            break;
        }
        assert!(
            std::time::Instant::now() <= deadline,
            "worker did not dequeue the retained activation"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    release_tx.send(()).expect("release scheduler");
    lock_thread.join().expect("join scheduler holder");

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if registry
            .pending_wake_micros_for_scope(&scope)
            .await
            .is_some_and(|pending| pending != 0)
        {
            break;
        }
        assert!(
            std::time::Instant::now() <= deadline,
            "failed activation did not restore its pending retry arrival"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    assert_eq!(
        registry.latest_generation_id(fixture.path()).await,
        None,
        "a retained generation cannot serve after its activation fails"
    );
    match tokio::time::timeout(
        Duration::from_millis(250),
        registry.execute_query_search(&scope, core_search_request("alpha")),
    )
    .await
    .expect("an unavailable generation must not block its query")
    {
        Err(super::query_runtime::QuerySearchExecutionErrorV1::GenerationUnverified) => {}
        Err(other) => panic!("expected the typed unverified state, got {other:?}"),
        Ok(_) => panic!("failed activation must not degrade into a stale answer"),
    }

    // A later real hint retries the retained owner. Restoring Git truth lets
    // the retry prove and activate the exact retained generation.
    std::fs::rename(&unavailable_git_dir, &git_dir).expect("restore Git authority");
    assert!(
        registry.notify_hook_overflow(fixture.path()).await,
        "restored worktree accepts a retry hint"
    );
    let receipt = wait_for_event_to_ready(&registry).await;
    assert!(
        receipt.is_noop(),
        "retry verifies the unchanged retained seal"
    );
    assert!(
        registry
            .latest_generation_id(fixture.path())
            .await
            .is_some(),
        "successful retry installs the verified retained generation"
    );
    registry.shutdown().await;
}

/// A retryable graph-activation failure of an already-sealed complete
/// generation must retry activation of that exact immutable artifact with
/// backoff. It must not fall through into reconcile and seal a duplicate
/// generation, even when the worktree has changed and overflow hints keep
/// waking the worker.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retryable_activation_failure_retries_the_sealed_generation_without_resealing() {
    let fixture = GitFixture::new(ALPHA_LIB_V1);
    let store = TempDir::new().expect("store root");
    let bytes = Arc::new(SharedCodeIndexBytePoolV1::default());
    let scoped_store = super::scoped_code_index_store_root(
        store.path(),
        &fixture.path().canonicalize().expect("canonical fixture"),
    );
    let sealed_worktree_id = {
        let mut scheduler = scheduler(&fixture, scoped_store.clone(), bytes);
        published(scheduler.reconcile_now().expect("seed generation"));
        scheduler
            .latest_complete()
            .expect("seeded generation")
            .generation
            .snapshot()
            .worktree
            .clone()
            .expect("seeded worktree id")
    };
    // Change the worktree so a reconcile pass would seal a brand-new
    // generation if the worker fell through after the activation failure.
    fixture.edit("src/extra.rs", "pub fn extra() -> u32 { 2 }\n");
    let generation_files = |scoped_store: &Path| -> usize {
        std::fs::read_dir(scoped_store.join("code-generations-v1")).map_or(0, |entries| {
            entries
                .filter_map(Result::ok)
                .filter(|entry| {
                    entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with("generation-")
                })
                .count()
        })
    };
    assert_eq!(generation_files(&scoped_store), 1);

    super::graph_activation::set_injected_activation_failures(&sealed_worktree_id, usize::MAX);
    let registry = CodeIndexSchedulerRegistryV1::with_background_reconcile_permits(1, 1);
    registry
        .mount_worktree(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("mount retained generation");

    // Keep waking the worker while activation stays failing; each pass must
    // hold the sealed artifact instead of rebuilding.
    for _ in 0..5 {
        let _ = registry.notify_hook_overflow(fixture.path()).await;
        tokio::time::sleep(Duration::from_millis(120)).await;
    }
    assert_eq!(
        generation_files(&scoped_store),
        1,
        "a retryable activation failure must not seal a duplicate generation"
    );
    assert_eq!(
        registry.latest_generation_id(fixture.path()).await,
        None,
        "a generation that never activated must not serve"
    );

    // Clearing the injected failure lets the scheduled backoff retry activate
    // the exact sealed artifact and resume ordinary refresh.
    super::graph_activation::set_injected_activation_failures(&sealed_worktree_id, 0);
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        if registry
            .latest_generation_id(fixture.path())
            .await
            .is_some()
        {
            break;
        }
        assert!(
            std::time::Instant::now() <= deadline,
            "the backoff retry did not activate the sealed generation"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    registry.shutdown().await;
}

/// Busy admission preserves the prior generation and schedules a follow-up wake
/// so serve-during-refresh cannot leave the index stale indefinitely.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn busy_admission_schedules_follow_up_cadence_wake() {
    let fixture = GitFixture::new(ALPHA_LIB_V1);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(1);
    registry
        .mount_worktree(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("mount");
    let expected = wait_for_initial_generation(&registry, fixture.path()).await;
    let before_receipts = registry.event_to_ready_receipts().len();

    let scheduler = registry
        .scheduler_handle(fixture.path())
        .await
        .expect("scheduler handle");
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let lock_thread = std::thread::spawn(move || {
        let _guard = scheduler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        held_tx.send(()).expect("signal held");
        let _ = release_rx.recv();
    });
    held_rx.recv().expect("lock acquired");

    let latest = tokio::time::timeout(
        Duration::from_millis(250),
        registry.latest_complete_fresh(fixture.path()),
    )
    .await
    .expect("must not wait on busy lock")
    .expect("prior generation served");
    assert_eq!(latest.generation.manifest().generation_id, expected);

    release_tx.send(()).expect("release");
    lock_thread.join().expect("join");

    // Follow-up wake must produce another cadence receipt after the lock frees.
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    loop {
        let receipts = registry.event_to_ready_receipts();
        if receipts.len() > before_receipts
            && receipts.iter().any(|receipt| {
                receipt.trigger == CodeIndexCadenceTriggerV1::BusyFollowUp
                    || receipt.trigger == CodeIndexCadenceTriggerV1::Mount
                    || receipt.trigger == CodeIndexCadenceTriggerV1::QueryAdmission
            })
        {
            break;
        }
        assert!(
            std::time::Instant::now() <= deadline,
            "busy follow-up wake did not produce a cadence receipt"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    registry.shutdown().await;
}

async fn wait_for_event_to_ready(
    registry: &CodeIndexSchedulerRegistryV1,
) -> super::CodeIndexEventToReadyReceiptV1 {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(receipt) = registry.latest_event_to_ready_receipt() {
            return receipt;
        }
        assert!(
            std::time::Instant::now() <= deadline,
            "timed out waiting for event-to-ready receipt"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The installed Plan 26 observability lane must persist one canonical index
/// lifecycle observation when a reconcile publishes a generation, and the
/// retrieval-pipeline families when a query composition completes, all in the
/// one project observation store.
#[tokio::test]
async fn installed_observability_lane_records_index_and_retrieval_observations() {
    let _pin = crate::config::PinnedUserDataDir::new();
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let store = TempDir::new().expect("store root");
    let (registry, scope) = mounted_core_query_worktree(&fixture, &store).await;

    let runtime = tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime::project(
        tracedecay_runtime_core::storage::default_profile_root().expect("profile root"),
        fixture.path(),
        scope.project_id.clone(),
    )
    .await
    .expect("registered runtime");
    let database = runtime.project_database_arc().expect("project database");
    let producer = Arc::new(
        tracedecay_usecases::observability::BoundedObservabilityProducerV1::start(
            database.clone(),
            tracedecay_usecases::observability::ObservabilityProducerIdentityV1 {
                authorized_scope_ref: scope.project_id.as_str().to_owned(),
                process_boot_id: "boot:code-index-observability".to_owned(),
                producer_revision: "code-index-observability-test.v1".to_owned(),
                configuration_revision: "code-index-observability-config.v1".to_owned(),
                policy_revision: "code-index-observability-policy.v1".to_owned(),
            },
            64,
        )
        .expect("bounded producer"),
    );
    registry
        .install_index_observability(
            fixture.path(),
            super::observability::CodeIndexObservabilityV1::new(
                database.clone(),
                Arc::clone(&producer),
            ),
        )
        .await
        .expect("install observability lane");

    // A reconcile after installation publishes a new generation and must leave
    // one canonical index lifecycle observation beside the cadence receipt.
    let initial = registry
        .latest_generation_id(fixture.path())
        .await
        .expect("initial generation");
    fixture.edit("src/lib.rs", "pub fn alpha() -> u32 { 2 }\n");
    assert!(
        registry
            .notify_path(fixture.path(), fixture.path().join("src/lib.rs"))
            .await
    );
    let _ = wait_for_generation_change(&registry, fixture.path(), &initial).await;

    // One real query composition through the mounted authority carries the
    // retrieval-pipeline families through the bounded producer.
    let _executed = registry
        .execute_query_search(&scope, core_search_request("alpha"))
        .await
        .expect("query search");

    registry.shutdown().await;
    producer.shutdown().await.expect("flush producer");

    let port =
        tracedecay_usecases::observability::RegisteredObservabilityPortV1::new(database.as_ref());
    let observability_query =
        |event_kinds: Vec<String>| tracedecay_application::ObservabilityQueryV1 {
            authorized_scope_ref: scope.project_id.as_str().to_owned(),
            event_kinds,
            horizon: tracedecay_application::ObservabilityHorizonV1 {
                since_micros: 0,
                until_micros: i64::MAX,
            },
            after_watermark: None,
            limit: 256,
        };
    let index_page = tracedecay_application::ObservabilityQueryPort::query(
        &port,
        observability_query(vec!["index.measurement.observed.v1".to_owned()]),
    )
    .await
    .expect("index lifecycle events");
    assert!(
        index_page.events.iter().any(|event| matches!(
            &event.payload,
            tracedecay_domain::ObservabilityPayloadV1::Index(observation)
                if observation.outcome == tracedecay_domain::IndexOutcomeV1::Published
                    && observation.kind
                        == tracedecay_domain::IndexObservationKindV1::Publication
        )),
        "a published reconcile must leave a canonical publication observation"
    );

    let retrieval_page = tracedecay_application::ObservabilityQueryPort::query(
        &port,
        observability_query(vec![
            "retrieval.planner.decided.v1".to_owned(),
            "retrieval.synthesis.completed.v1".to_owned(),
            "retrieval.source.observed.v1".to_owned(),
        ]),
    )
    .await
    .expect("retrieval pipeline events");
    let planner = retrieval_page
        .events
        .iter()
        .find_map(|event| match &event.payload {
            tracedecay_domain::ObservabilityPayloadV1::RetrievalPlanner(planner) => {
                Some(planner.clone())
            }
            _ => None,
        })
        .expect("one planner observation per composition");
    assert_eq!(
        planner.requested_lanes,
        vec!["exact_literal", "lexical", "graph"],
        "the observation reflects the lanes the composition actually ran"
    );
    assert!(
        retrieval_page.events.iter().any(|event| matches!(
            &event.payload,
            tracedecay_domain::ObservabilityPayloadV1::RetrievalSynthesis(_)
        )),
        "the composition's synthesis observation must be persisted"
    );
}
