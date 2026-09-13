use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};

use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tracedecay_application::code_index::open_production_code_index_owner_v1;
#[cfg(all(feature = "semantic-fastembed", not(windows)))]
use tracedecay_application::semantic_runtime::{
    RetainedSemanticVectorGraphV1, SemanticRuntimeFuture, SemanticVectorGraphErrorV1,
    SemanticVectorGraphProviderV1,
};
use tracedecay_code_index_retention::code_index_generations::DurablePublicationPointerV1;
use tracedecay_contracts::{
    CancellationContext, CapabilityGrantSnapshot, Deadline, DisclosureClass, OpaqueCursor,
    PageRequest, RequestContext, RequestId, ResolvedScope, ResultProjection, RetrievalOrder,
    RetrievalRequestMeta,
};
use tracedecay_domain::{
    ActorId, AuthorizationRevision, CalibrationProfileId, ChunkerRevision, CodeGenerationId,
    ComponentRevision, DiversityPolicy, ExactAdmissionRuleRevision, FusionProfile, ManifestDigest,
    PolicyRevisionId, PrincipalId, PrivacyDomainId, ProjectId, QueryNormalizationRevision, RefId,
    RelationEdgeKindV1, RepositoryId, RerankPolicy, RetrievalAnchorId, RetrievalBudget,
    RetrievalCursorKeyId, RetrieverKind, SanitizerRevision, ScoreDomainCalibrationV1,
    ScoreDomainId, UtcMicros, WorktreeId,
};
#[cfg(all(feature = "semantic-fastembed", not(windows)))]
use tracedecay_graph_db::NeverCancelled;
use tracedecay_query::retrieval::{
    QueryAuthorityV1,
    fusion::RetrievalCursorKeyringV1,
    lexical::LexicalRoutingV1,
    ports::RetrievalExecutionControl,
    rerank::{
        AdmittedNativeRerankExecutorV1, DeterministicLocalRerankExecutorV1, LocalRerankFailureV1,
        LocalRerankInputV1, LocalRerankPermitV1,
    },
};

use crate::code_index_scheduler::{
    CodeIndexHintPolicyV1, CodeIndexReconcileOutcomeV1, CodeIndexSchedulerRegistryV1,
    CodeIndexWorktreeSchedulerV1, SharedCodeIndexBytePoolV1,
};

#[cfg(feature = "hotpath-alloc")]
#[global_allocator]
static HOTPATH_ALLOCATOR: hotpath::CountingAllocator = hotpath::CountingAllocator::new();

mod branch_publication_tests;
mod noop_reconcile_tests;
mod publication_store;
mod reconcile;
mod retained_configuration_tests;
mod search_permit_release;
mod semantic_schedule_order_tests;
mod serving;

/// Base directory for fixture temporary roots, resolved through every symlink.
///
/// macOS puts `TempDir` under `/var/folders/...`, and `/var` is a symlink to
/// `/private/var`. Production canonicalizes a project root before it hashes
/// the code-index scope and before it decides whether a dependency escaped the
/// worktree, so a fixture path that still carries the symlink names a
/// different scope than the one the scheduler writes and reads. Create the
/// fixture inside the canonical temporary directory so every path taken from
/// it is already canonical.
fn decode_hex(encoded: &str) -> Vec<u8> {
    (0..encoded.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&encoded[index..index + 2], 16).expect("hex byte"))
        .collect()
}

fn canonical_temp_root() -> std::path::PathBuf {
    let base = std::env::temp_dir();
    base.canonicalize().unwrap_or(base)
}

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
        let root = TempDir::new_in(canonical_temp_root()).expect("fixture root");
        git(root.path(), &["init", "-q", "-b", "main"]);
        git(root.path(), &["config", "user.name", "TraceDecay Test"]);
        git(
            root.path(),
            &["config", "user.email", "tracedecay@example.invalid"],
        );
        // `git commit` runs `git maintenance run --auto`, and with the default
        // `maintenance.autoDetach` that child outlives the commit we waited on.
        // It touches `.git` (its own lock, `gc.log`) after `git commit` has
        // already returned, which races `from_template`'s directory walk: an
        // entry listed by `read_dir` can be gone by the time it is copied, and
        // the fixture fails with a bare `NotFound`. Fixtures need no
        // maintenance at all, so switch it off in the repository itself; the
        // setting is inherited by every copy taken from this template.
        git(root.path(), &["config", "maintenance.auto", "false"]);
        git(root.path(), &["config", "gc.auto", "0"]);
        // These fixtures are byte-exact corpora: a clean scan captures HEAD's
        // blobs while an incremental reconcile reads the files Git wrote into
        // the worktree, and the two must agree byte for byte. The Windows
        // runner's global `core.autocrlf=true` would make `rebase`/`switch`
        // rewrite the LF sources as CRLF, so newline conversion is pinned off
        // in the repository itself; every copy of the template inherits it.
        git(root.path(), &["config", "core.autocrlf", "false"]);
        for (path, source) in files {
            write(root.path(), path, source);
        }
        git(root.path(), &["add", "."]);
        git(root.path(), &["commit", "-qm", "fixture"]);
        Self { root }
    }

    fn from_template(template: &Path) -> Self {
        let root = TempDir::new_in(canonical_temp_root()).expect("fixture root");
        copy_dir_recursive(template, root.path());
        Self { root }
    }

    fn path(&self) -> &Path {
        self.root.path()
    }

    fn edit(&self, path: &str, source: &str) {
        write(self.path(), path, source);
    }

    fn remove(&self, path: &str) {
        std::fs::remove_file(self.path().join(path)).expect("remove fixture source");
    }

    fn commit_all(&self, message: &str) {
        git(self.path(), &["add", "-A"]);
        git(self.path(), &["commit", "-qm", message]);
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
        let source = entry.path();
        let destination = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_recursive(&source, &destination);
        } else {
            // `file_type` is the un-followed type, so a symlink lands here and
            // `fs::copy` would resolve it; name the entry either way so a
            // failure identifies the exact template path rather than reporting
            // a bare errno.
            std::fs::copy(&source, &destination).unwrap_or_else(|error| {
                panic!(
                    "copy fixture template file '{}' to '{}': {error}",
                    source.display(),
                    destination.display()
                )
            });
        }
    }
}

fn git(root: &Path, args: &[&str]) {
    let status = Command::new(
        tracedecay_runtime_core::git::try_git_program()
            .expect("absolute git executable should resolve"),
    )
    .current_dir(root)
    .args(args)
    .status()
    .expect("run git fixture command");
    assert!(status.success(), "git fixture command failed: {args:?}");
}

fn git_stdout(root: &Path, args: &[&str]) -> String {
    let output = Command::new(
        tracedecay_runtime_core::git::try_git_program()
            .expect("absolute git executable should resolve"),
    )
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

fn replace_scheduler_policy_revision(scheduler: &mut CodeIndexWorktreeSchedulerV1, revision: &str) {
    let mut config = scheduler.production_config.clone();
    config.policy_revision = PolicyRevisionId::new(revision).expect("policy revision");
    scheduler.owner = open_production_code_index_owner_v1(
        config.clone(),
        scheduler.publication.clone(),
        super::DaemonProjectionSinkV1,
    )
    .expect("open reconfigured production owner")
    .with_physical_artifact_pool(scheduler.byte_pool.physical_artifacts.clone());
    scheduler.production_config = config;
}

fn replace_scheduler_chunker_revision(
    scheduler: &mut CodeIndexWorktreeSchedulerV1,
    revision: &str,
) {
    let mut config = scheduler.production_config.clone();
    config.chunker_revision = ChunkerRevision::new(revision).expect("chunker revision");
    scheduler.owner = open_production_code_index_owner_v1(
        config.clone(),
        scheduler.publication.clone(),
        super::DaemonProjectionSinkV1,
    )
    .expect("open reconfigured production owner")
    .with_physical_artifact_pool(scheduler.byte_pool.physical_artifacts.clone());
    scheduler.production_config = config;
}

fn build_progress_snapshot(
    scheduler: &CodeIndexWorktreeSchedulerV1,
) -> Arc<tracedecay_contracts::code_index_freshness::CodeIndexBuildProgressV1> {
    scheduler
        .build_progress_slot()
        .read()
        .expect("build progress slot")
        .snapshot()
        .expect("published build progress")
}

fn progress_snapshot_for_generation(
    generation_id: &CodeGenerationId,
    committed_pages: u64,
) -> tracedecay_contracts::code_index_freshness::CodeIndexBuildProgressV1 {
    tracedecay_contracts::code_index_freshness::CodeIndexBuildProgressV1 {
        generation_id: generation_id.as_str().to_owned(),
        daemon_incarnation: 1,
        producer_incarnation: 1,
        progress_epoch: 0,
        sealed_source_digest: format!("sha256:{}", "a".repeat(64)),
        phase: tracedecay_contracts::code_index_freshness::CodeIndexBuildPhaseV1::BulkCommit,
        committed_pages,
        committed_chunks: committed_pages,
        committed_imports: 0,
        committed_payload_bytes: committed_pages,
        completed_files: committed_pages,
        total_files: 100,
        completed_lexical_units: committed_pages,
        total_lexical_units: 100,
        current_batch_pages: 0,
        current_batch_payload_bytes: 0,
        elapsed_micros: 1,
        last_commit_latency_micros: None,
        files_per_second: None,
        lexical_units_per_second: None,
        estimated_remaining_seconds: None,
        last_progress_micros: 1,
        blocked_reason: None,
    }
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
    use tracedecay_code_index_retention::code_index_generations::durable_generation_index_digest;

    let pointer_path = store_root.join("active-code-generation-v1.json");
    let mut pointer: tracedecay_code_index_retention::code_index_generations::DurablePublicationPointerV1 =
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

fn rewrite_active_rust_extractor_revision(store_root: &Path, revision: &str) {
    use tracedecay_code_index::{
        capabilities::expected_seal_digest,
        languages::{LanguageRegistry, StaticLanguageRegistry},
    };
    use tracedecay_code_index_retention::code_index_generations::{
        DurablePublicationPointerV1, durable_generation_index_digest,
    };
    use tracedecay_domain::{CodeGenerationManifestV1, ExtractorRevision, LanguageId};

    let pointer_path = store_root.join("active-code-generation-v1.json");
    let mut pointer: DurablePublicationPointerV1 =
        serde_json::from_slice(&std::fs::read(&pointer_path).expect("read publication pointer"))
            .expect("decode publication pointer");
    let generation_path = store_root
        .join("code-generations-v1")
        .join(&pointer.generation_file);
    let mut envelope: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&generation_path).expect("read generation manifest"))
            .expect("decode generation envelope");
    let mut manifest: CodeGenerationManifestV1 =
        serde_json::from_value(envelope["generation"]["manifest"].clone())
            .expect("decode generation identity manifest");
    let rust = LanguageId::new("rust").expect("Rust language id");
    let mut descriptor = StaticLanguageRegistry::new()
        .descriptor(&rust)
        .expect("compiled Rust descriptor")
        .clone();
    descriptor.extractor_revision =
        ExtractorRevision::new(revision).expect("historical extractor revision");
    let historical_registry = StaticLanguageRegistry::try_from_descriptors(vec![descriptor])
        .expect("historical snapshot registry");
    manifest.registry_revision = historical_registry.registry_revision();
    manifest.extractor_revisions = historical_registry
        .descriptors()
        .iter()
        .map(|descriptor| {
            (
                descriptor.language.clone(),
                descriptor.extractor_revision.clone(),
            )
        })
        .collect();
    manifest.seal.expected_digest =
        expected_seal_digest(&manifest).expect("reseal historical manifest identity");
    envelope["generation"]["manifest"] =
        serde_json::to_value(manifest).expect("encode historical manifest identity");

    let generation_bytes =
        serde_json::to_vec(&envelope["generation"]).expect("encode generation payload");
    envelope["state_digest"] = serde_json::Value::String(format!(
        "sha256:{}",
        Sha256::digest(&generation_bytes).iter().fold(
            String::with_capacity(64),
            |mut encoded, byte| {
                write!(&mut encoded, "{byte:02x}").expect("encode generation digest");
                encoded
            }
        )
    ));
    let envelope_bytes = serde_json::to_vec(&envelope).expect("encode generation envelope");
    std::fs::write(&generation_path, &envelope_bytes).expect("write historical generation");
    let state_digest = format!(
        "sha256:{}",
        Sha256::digest(&envelope_bytes).iter().fold(
            String::with_capacity(64),
            |mut encoded, byte| {
                write!(&mut encoded, "{byte:02x}").expect("encode envelope digest");
                encoded
            }
        )
    );
    pointer.state_digest = state_digest.clone();
    let active = pointer
        .generation_index
        .iter_mut()
        .find(|entry| entry.generation_id == pointer.generation_id)
        .expect("active generation index entry");
    active.state_digest = state_digest;
    active.size_bytes = envelope_bytes.len() as u64;
    active.text_artifact = None;
    pointer.generation_index_digest = Some(
        durable_generation_index_digest(
            &pointer.generation_index,
            pointer.generation_index_truncated,
        )
        .expect("digest rewritten generation index"),
    );
    std::fs::write(
        pointer_path,
        serde_json::to_vec(&pointer).expect("encode publication pointer"),
    )
    .expect("write publication pointer");
}

/// Unchanged-file count for the committed one-file-edit capture. Defaults to
/// 1,000; the 10k acceptance measurement sets `TRACEDECAY_CAPTURE_CORPUS_FILES`.
fn committed_capture_corpus_files() -> usize {
    match std::env::var("TRACEDECAY_CAPTURE_CORPUS_FILES") {
        Ok(value) => value
            .parse()
            .expect("TRACEDECAY_CAPTURE_CORPUS_FILES must be a positive integer"),
        Err(std::env::VarError::NotPresent) => 1_000,
        Err(std::env::VarError::NotUnicode(_)) => {
            panic!("TRACEDECAY_CAPTURE_CORPUS_FILES must contain valid Unicode")
        }
    }
}

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
    mode: tracedecay_code_index_retention::code_index_generations::CodeGenerationRetentionModeV1,
    now_secs: i64,
    completed_at: UtcMicros,
) -> Result<
    tracedecay_code_index_retention::code_index_generations::ScopeRootRetentionReportV1,
    tracedecay_code_index_retention::code_index_generations::CodeGenerationRetentionErrorV1,
> {
    use tracedecay_code_index_retention::code_index_generations::{
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
            .map(|root| {
                tracedecay_code_index_retention::code_index_generations::code_index_scope_hash(root)
            })
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

struct MixedAnchorReverseRerankExecutorV1;

impl AdmittedNativeRerankExecutorV1 for MixedAnchorReverseRerankExecutorV1 {
    fn artifact_manifest_digest(&self) -> &ManifestDigest {
        static DIGEST: OnceLock<ManifestDigest> = OnceLock::new();
        DIGEST.get_or_init(|| {
            ManifestDigest::new(format!("sha256:{}", "a".repeat(64))).expect("artifact digest")
        })
    }
}

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

impl RetrievalExecutionControl for ReadyRerankControlV1 {
    fn elapsed_micros(&self) -> u64 {
        0
    }

    fn is_cancelled(&self) -> bool {
        false
    }
}

struct CancelledRerankControlV1;

impl RetrievalExecutionControl for CancelledRerankControlV1 {
    fn elapsed_micros(&self) -> u64 {
        0
    }

    fn is_cancelled(&self) -> bool {
        true
    }
}

struct ReadySemanticControlV1;

impl RetrievalExecutionControl for ReadySemanticControlV1 {
    fn is_cancelled(&self) -> bool {
        false
    }

    fn elapsed_micros(&self) -> u64 {
        0
    }
}

fn application_context(
    operation: &tracedecay_contracts::ApplicationOperation,
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
        tracedecay_contracts::CapabilityGrantId::new("grant.code-index.fixture").expect("grant id"),
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

fn install_verified_graph_store(latest: &super::LatestCompleteCodeIndexV1) {
    install_verified_graph_store_on_text(&latest.text_generation_handle(), latest);
}

fn install_verified_graph_store_on_text(
    text: &super::LatestCodeTextGenerationV1,
    latest: &super::LatestCompleteCodeIndexV1,
) {
    let generation = latest.generation.manifest().generation_id.clone();
    let cancellation =
        tracedecay_contracts::CancellationSignal::active("cancel.callable-graph-projection")
            .expect("graph cancellation");
    let publisher =
        tracedecay_code_index::graph_projection::HermeticCodeGraphProjectionStore::memory(
            &cancellation,
        )
        .expect("graph publisher");
    publisher
        .publish_indexed_with_cancellation(
            &generation,
            latest.generation.edges(),
            latest.generation.chunks().chunks(),
            &latest.generation.snapshot().files,
            latest.generation.symbols(),
            Arc::new(tracedecay_graph_db::NeverCancelled),
        )
        .expect("publish indexed graph");
    let graph_store = Arc::new(
        publisher
            .verified_store(&generation)
            .expect("verified graph"),
    );
    graph_store
        .warm_interactive_catalog_with_cancellation(Arc::new(tracedecay_graph_db::NeverCancelled))
        .expect("warm graph catalog");
    let graph_reader = graph_store
        .evidence_reader_with_cancellation(
            &generation,
            Some(latest.generation.snapshot().repository.clone()),
            latest.source_freshness().expect("source freshness"),
            Arc::new(tracedecay_graph_db::NeverCancelled),
        )
        .expect("graph reader");
    text.install_graph_serving(
        graph_reader,
        Some(graph_store),
        super::CodeGraphServingAuthorityV1::Memory,
    )
    .expect("install interactive graph serving");
}

fn query_authority(privacy_domain: PrivacyDomainId) -> Arc<QueryAuthorityV1> {
    query_authority_with_candidate_cap(privacy_domain, 32)
}

fn query_authority_with_candidate_cap(
    privacy_domain: PrivacyDomainId,
    max_candidates_per_lane: u32,
) -> Arc<QueryAuthorityV1> {
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
        // Approximate candidates calibrate only within a declared score
        // domain, so the profile must cover every raw-score domain the
        // daemon lanes emit.
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
        minimum_calibrated_feature_micros: BTreeMap::new(),
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
            max_candidates_per_lane,
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

fn active_text_artifact_path(store_root: &Path) -> PathBuf {
    let pointer: serde_json::Value = serde_json::from_slice(
        &std::fs::read(store_root.join("active-code-generation-v1.json"))
            .expect("read durable pointer"),
    )
    .expect("parse durable pointer");
    let active_generation = pointer["generation_id"]
        .as_str()
        .expect("active generation id");
    let entry = pointer["generation_index"]
        .as_array()
        .expect("durable generation index")
        .iter()
        .find(|entry| entry["generation_id"] == active_generation)
        .expect("active generation index entry");
    let artifact_file = entry["text_artifact"]["artifact_file"]
        .as_str()
        .expect("attached text artifact descriptor");
    store_root
        .join("code-text-artifacts-v1")
        .join(artifact_file)
}

fn rewrite_active_text_artifact_format_revision(store_root: &Path, revision: u64) -> PathBuf {
    use tracedecay_code_index_retention::code_index_generations::{
        DurablePublicationPointerV1, durable_generation_index_digest,
    };

    let pointer_path = store_root.join("active-code-generation-v1.json");
    let mut pointer: DurablePublicationPointerV1 = serde_json::from_slice(
        &std::fs::read(&pointer_path).expect("read durable publication pointer"),
    )
    .expect("decode durable publication pointer");
    let entry = pointer
        .generation_index
        .iter_mut()
        .find(|entry| entry.generation_id == pointer.generation_id)
        .expect("active generation entry");
    let descriptor = entry
        .text_artifact
        .as_mut()
        .expect("active text artifact descriptor");
    let old_path = store_root
        .join("code-text-artifacts-v1")
        .join(&descriptor.artifact_file);
    {
        let connection = rusqlite::Connection::open(&old_path).expect("open published artifact");
        let revision = i64::try_from(revision).expect("artifact revision fits SQLite INTEGER");
        assert_eq!(
            connection
                .execute("UPDATE artifact_state SET format_revision = ?1", [revision],)
                .expect("rewrite artifact format revision"),
            1
        );
    }
    let artifact_bytes = std::fs::read(&old_path).expect("read rewritten artifact");
    let artifact_hex = Sha256::digest(&artifact_bytes).iter().fold(
        String::with_capacity(64),
        |mut artifact_hex, byte| {
            write!(&mut artifact_hex, "{byte:02x}").expect("write digest hex");
            artifact_hex
        },
    );
    let artifact_file = format!("text-artifact-{artifact_hex}.bin");
    let rewritten_path = old_path
        .parent()
        .expect("artifact root")
        .join(&artifact_file);
    std::fs::rename(&old_path, &rewritten_path).expect("rename rewritten artifact");
    descriptor.artifact_file = artifact_file;
    descriptor.artifact_digest =
        ManifestDigest::new(format!("sha256:{artifact_hex}")).expect("rewritten artifact digest");
    descriptor.artifact_size_bytes =
        u64::try_from(artifact_bytes.len()).expect("artifact length fits u64");
    pointer.generation_index_digest = Some(
        durable_generation_index_digest(
            &pointer.generation_index,
            pointer.generation_index_truncated,
        )
        .expect("rewritten generation index digest"),
    );
    std::fs::write(
        pointer_path,
        serde_json::to_vec(&pointer).expect("encode rewritten publication pointer"),
    )
    .expect("write rewritten publication pointer");
    rewritten_path
}

/// Build the core-authority search policy with caller lexical routing.
fn routed_core_search_request(
    query: &str,
    lexical_routing: LexicalRoutingV1,
) -> super::query_runtime::QuerySearchExecutionRequestV1 {
    let mut request = core_search_request(query);
    request.lexical_routing = lexical_routing;
    request
}

/// The qualified symbol name behind each composed candidate, in rank order;
/// `None` for file-grain chunk anchors.
fn ranked_symbol_names(
    executed: &super::query_runtime::ExecutedQuerySearchV1,
    latest: &super::LatestCompleteCodeIndexV1,
) -> Vec<Option<String>> {
    let symbols = latest.generation.symbols();
    executed
        .authorized
        .fallback
        .ordered_candidates
        .iter()
        .map(|ranked| {
            let anchor = ranked.candidate.anchor_id.as_str();
            let occurrence = anchor.strip_prefix("code-symbol:")?;
            let symbol = symbols
                .symbols
                .iter()
                .find(|symbol| symbol.occurrence.as_str() == occurrence)
                .unwrap_or_else(|| panic!("anchor {anchor} names a published symbol"));
            Some(symbol.qualified_name.clone())
        })
        .collect()
}

fn ranks_symbol(names: &[Option<String>], suffix: &str) -> bool {
    names.iter().flatten().any(|name| name.ends_with(suffix))
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
            lexical_routing: LexicalRoutingV1::query_only(),
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
    let latest = wait_for_live_complete_generation(&registry, fixture.path()).await;
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

/// Flip the last hexadecimal character of a durable identifier so the result
/// stays format-valid while naming something else.
fn flip_trailing_hex(value: &str) -> String {
    let mut renamed = value.to_owned();
    let last = renamed.pop().expect("identifier has a trailing character");
    renamed.push(if last == '0' { '1' } else { '0' });
    renamed
}

/// Rewrite the durable publication pointer exactly as a convergence
/// republication leaves it between its publish and its seat: the active
/// generation id advances to an unseated successor, replacing the
/// predecessor's index entry (same-revision entries are replaced, not
/// appended), while the sealed artifact and Git evidence stay in place.
/// `drift_content` additionally flips the sealed source content identity,
/// staging a successor sealed from different bytes.
fn advance_pointer_to_unseated_successor(scoped_store_root: &Path, drift_content: bool) {
    use tracedecay_code_index_retention::code_index_generations::durable_generation_index_digest;

    let pointer_path = scoped_store_root.join("active-code-generation-v1.json");
    let mut pointer: DurablePublicationPointerV1 =
        serde_json::from_slice(&std::fs::read(&pointer_path).expect("read active pointer"))
            .expect("decode active pointer");
    let predecessor_id = pointer.generation_id.clone();
    pointer.generation_id = flip_trailing_hex(&predecessor_id);
    if drift_content {
        pointer.snapshot_content_identity = flip_trailing_hex(&pointer.snapshot_content_identity);
    }
    let active_entry = pointer
        .generation_index
        .iter_mut()
        .find(|entry| entry.generation_id == predecessor_id)
        .expect("active generation index entry");
    active_entry.generation_id = pointer.generation_id.clone();
    active_entry.snapshot_content_identity = pointer.snapshot_content_identity.clone();
    pointer.generation_index_digest = Some(
        durable_generation_index_digest(
            &pointer.generation_index,
            pointer.generation_index_truncated,
        )
        .expect("digest successor publication index"),
    );
    std::fs::write(
        &pointer_path,
        serde_json::to_vec(&pointer).expect("encode successor pointer"),
    )
    .expect("write successor pointer");
}

/// Rewrite one fixture source with bytes of the same length and hand it back
/// its previous mtime, the way `rsync -a`, `cp --preserve`, `touch -d`, and
/// restore tools do. Every `(path, len, mtime)` tuple the stat signature hashes
/// is unchanged afterwards, so only content can tell the two states apart.
fn rewrite_preserving_stat(fixture: &GitFixture, path: &str, source: &str) {
    let absolute = fixture.path().join(path);
    let before = std::fs::metadata(&absolute).expect("source metadata before rewrite");
    assert_eq!(
        before.len(),
        source.len() as u64,
        "the rewrite must keep the byte length so stat metadata cannot see it"
    );
    std::fs::write(&absolute, source).expect("rewrite fixture source");
    filetime::set_file_mtime(
        &absolute,
        filetime::FileTime::from_system_time(before.modified().expect("source mtime")),
    )
    .expect("restore the source mtime");
    let after = std::fs::metadata(&absolute).expect("source metadata after rewrite");
    assert_eq!(after.len(), before.len());
    assert_eq!(
        after.modified().expect("restored mtime"),
        before.modified().expect("original mtime"),
        "the rewrite must hand the file its previous mtime back"
    );
}

fn served_lexical_texts(scheduler: &CodeIndexWorktreeSchedulerV1, needle: &str) -> Vec<String> {
    scheduler
        .latest_complete()
        .expect("served generation")
        .lexical()
        .iter()
        .filter(|chunk| chunk.sanitized_text.as_str().contains(needle))
        .map(|chunk| chunk.sanitized_text.as_str().to_owned())
        .collect()
}

/// Wait until no owner pass is running for `project_root`.
///
/// A serving seat is published from inside a pass, so every seat wait returns
/// while the worker still owns `reconcile_in_progress` and has post-seat work
/// left — semantic scheduling, receipts, graph steps. A test that samples one
/// of those effects immediately after a seat wait races the pass that produces
/// it. This is the barrier for "the pass that seated is finished", and it is a
/// failure bound only: a worker that never finishes panics with a diagnostic.
async fn wait_for_quiescent_owner_pass(
    registry: &CodeIndexSchedulerRegistryV1,
    project_root: &Path,
) {
    let deadline = Instant::now() + SERVING_SEAT_FAILURE_CEILING;
    while registry.reconcile_in_progress_for_test(project_root).await {
        assert!(
            Instant::now() <= deadline,
            "the owner pass for {} never finished",
            project_root.display()
        );
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

/// Hold the background worker out of a new pass, then wait for the in-flight
/// pass to finish, and keep the admission permit.
///
/// Winning the permit only proves no *new* pass can start. The worker releases
/// it after source reconciliation but keeps its `reconcile_in_progress` guard
/// through text seating, so the permit is routinely free while a pass runs and
/// that pass still moves the serving seat, its witness, and the pending wake
/// under a test that already believes the worker is parked. With the permit
/// held the pass counter is monotone to zero, so this settles once and stays
/// settled for the rest of the test.
async fn quiesced_background_reconcile_admission(
    registry: &CodeIndexSchedulerRegistryV1,
    project_root: &Path,
) -> tokio::sync::OwnedSemaphorePermit {
    let admission = registry
        .background_reconcile_admission()
        .acquire_owned()
        .await
        .expect("hold background worker at its dequeue point");
    wait_for_quiescent_owner_pass(registry, project_root).await;
    admission
}

/// In-process semantic-vector graph fixture: the canonical isolated semantic
/// evaluation graph stands in for the daemon-retained code-graph runtime, so
/// publish/restore flows exercise the same verified staging/publication
/// machinery the production provider resolves.
#[cfg(all(feature = "semantic-fastembed", not(windows)))]
struct IsolatedSemanticVectorGraphProviderV1 {
    graph:
        Arc<tracedecay_application::store::vector_generations::IsolatedSemanticEvaluationGraphV1>,
    current: tracedecay_domain::CodeGenerationId,
    generation_reads: std::sync::atomic::AtomicUsize,
}

#[cfg(all(feature = "semantic-fastembed", not(windows)))]
impl IsolatedSemanticVectorGraphProviderV1 {
    fn new(
        generation: &tracedecay_code_index::production::CodeIndexPublishedGenerationV1,
    ) -> Arc<Self> {
        let graph =
            tracedecay_application::store::vector_generations::isolated_semantic_evaluation_graph(
                &[generation],
                Arc::new(NeverCancelled),
            )
            .expect("open isolated semantic evaluation graph");
        Arc::new(Self {
            graph,
            current: generation.manifest().generation_id.clone(),
            generation_reads: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    fn generation_reads(&self) -> usize {
        self.generation_reads
            .load(std::sync::atomic::Ordering::Acquire)
    }
}

#[cfg(all(feature = "semantic-fastembed", not(windows)))]
impl SemanticVectorGraphProviderV1 for IsolatedSemanticVectorGraphProviderV1 {
    fn graph_for_generation<'a>(
        &'a self,
        generation: &'a tracedecay_code_index::production::CodeIndexPublishedGenerationV1,
    ) -> SemanticRuntimeFuture<'a, Result<RetainedSemanticVectorGraphV1, SemanticVectorGraphErrorV1>>
    {
        self.generation_reads
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
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

const CALLER_STAR: usize = 2_000;
const CALLER_STAR_FILES: usize = 8;
const CALLER_PAGE: u32 = 10;

fn caller_star_sources() -> Vec<(String, String)> {
    let per_file = CALLER_STAR / CALLER_STAR_FILES;
    let mut mods = String::from("pub fn hub() {}\n");
    let mut files = Vec::with_capacity(CALLER_STAR_FILES.saturating_add(1));
    for file_idx in 0..CALLER_STAR_FILES {
        let _ = writeln!(mods, "mod callers_{file_idx:02};");
        let mut body = String::new();
        for local in 0..per_file {
            let index = file_idx * per_file + local;
            let _ = writeln!(body, "pub fn caller_{index:04}() {{ crate::hub(); }}");
        }
        files.push((format!("src/callers_{file_idx:02}.rs"), body));
    }
    files.insert(0, ("src/lib.rs".to_owned(), mods));
    files
}

fn callers_page_meta(page_size: u32, cursor: Option<OpaqueCursor>) -> RetrievalRequestMeta {
    RetrievalRequestMeta::current(
        PageRequest::new(page_size, cursor).expect("callers page"),
        ResultProjection::Evidence,
        RetrievalOrder::Relevance,
    )
}

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

/// Failure ceiling for a positive serving wait. This is not a scheduling
/// budget: the waiter still blocks on the seating signal, and a seat that
/// arrives at any time before the ceiling succeeds. The bound exists only so
/// a worktree that can never seat fails with a diagnostic instead of hanging.
const SERVING_SEAT_FAILURE_CEILING: Duration = Duration::from_mins(2);

async fn serving_seat_wait_diagnostic(
    registry: &CodeIndexSchedulerRegistryV1,
    path: &Path,
    last_serving: Option<&CodeGenerationId>,
    last_generation: Option<&CodeGenerationId>,
) -> String {
    let current_serving = registry
        .latest_complete_serving_for_test(path)
        .await
        .map(|latest| latest.generation.manifest().generation_id.clone());
    let current_generation = registry.latest_generation_id(path).await;
    let mounted = match registry.mounted_code_scope(path).await {
        Some(scope) => format!(
            "mounted repo={} worktree={} shutting_down={}",
            scope.repository_id,
            scope.worktree_id,
            scope
                .shutting_down
                .load(std::sync::atomic::Ordering::Acquire)
        ),
        None => "unmounted".to_owned(),
    };
    format!(
        "serving seat never arrived for worktree {}; last observed serving={:?} generation={:?}; registry serving={:?} generation={:?} {mounted}",
        path.display(),
        last_serving.map(CodeGenerationId::as_str),
        last_generation.map(CodeGenerationId::as_str),
        current_serving.as_ref().map(CodeGenerationId::as_str),
        current_generation.as_ref().map(CodeGenerationId::as_str),
    )
}

/// Wait until `probe` observes a serving seat for `path`.
///
/// Checks the current slot before subscribing so a seat that arrived before
/// this waiter exists is not missed, then waits on
/// [`CodeIndexSchedulerRegistryV1::subscribe_serving_seats`]. A seat can also
/// land between that first probe and subscribe; the loop re-reads the slot
/// before blocking on the next wake.
///
/// [`CodeIndexSchedulerRegistryV1::subscribe_serving_generation_changes`]
/// wakes on per-worktree seating, including restored mounts that emit no new
/// registry-wide seat count. That subscribe returns `None` until the worktree
/// is mounted, so the loop re-attempts it each iteration until it returns
/// `Some`. A waiter that starts before mount still observes
/// [`CodeIndexSchedulerRegistryV1::subscribe_serving_seats`].
///
/// `watch::Sender::subscribe()` marks the current value seen, so a seat that
/// lands between subscribe and the first `changed()` is invisible unless we
/// re-probe after subscribe and before `changed()`.
///
/// `ceiling` is a failure bound only. The wait is still signal-driven; a
/// test must not pass because the ceiling elapsed.
async fn wait_until_serving_seat<T, F, Fut>(
    registry: &CodeIndexSchedulerRegistryV1,
    path: &Path,
    ceiling: Duration,
    mut probe: F,
) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let wait = async {
        if let Some(value) = probe().await {
            return value;
        }
        let mut seats = registry.subscribe_serving_seats();
        let mut per_worktree = None;
        loop {
            if per_worktree.is_none() {
                per_worktree = registry.subscribe_serving_generation_changes(path).await;
            }
            // watch::Sender::subscribe() marks the current value seen, so a
            // seat that landed between this subscribe and the wait below is
            // missed unless we re-probe before changed().
            if let Some(value) = probe().await {
                return value;
            }
            match per_worktree.as_mut() {
                Some(changes) => {
                    tokio::select! {
                        result = seats.changed() => {
                            result.expect("the seating channel stays open while the registry lives");
                        }
                        result = changes.changed() => {
                            result.expect("the per-worktree serving channel stays open while the owner lives");
                        }
                    }
                }
                None => {
                    seats
                        .changed()
                        .await
                        .expect("the seating channel stays open while the registry lives");
                }
            }
        }
    };
    match tokio::time::timeout(ceiling, wait).await {
        Ok(value) => value,
        Err(_) => {
            let last_serving = registry
                .latest_complete_serving_for_test(path)
                .await
                .map(|latest| latest.generation.manifest().generation_id.clone());
            let last_generation = registry.latest_generation_id(path).await;
            panic!(
                "{}",
                serving_seat_wait_diagnostic(
                    registry,
                    path,
                    last_serving.as_ref(),
                    last_generation.as_ref(),
                )
                .await
            )
        }
    }
}

/// Wait until the registry-mounted worktree seats its first generation.
///
/// [`CodeIndexSchedulerRegistryV1::latest_generation_id`] answers from a
/// serving or text seat. The text lane publishes
/// [`CodeIndexSchedulerRegistryV1::subscribe_serving_generation_changes`];
/// the earlier note that text can seat without a wake applied only to the
/// registry-wide [`CodeIndexSchedulerRegistryV1::subscribe_serving_seats`]
/// counter. Callers that need the complete serving generation must use
/// [`wait_for_live_complete_generation`].
async fn wait_for_initial_generation(
    registry: &CodeIndexSchedulerRegistryV1,
    path: &Path,
) -> tracedecay_domain::CodeGenerationId {
    wait_until_serving_seat(registry, path, SERVING_SEAT_FAILURE_CEILING, || {
        registry.latest_generation_id(path)
    })
    .await
}

/// Publication now broadcasts as soon as reconcile publishes, before graph
/// seating. Callers that need the complete serving generation must wait for
/// that seat, not only the publication event.
///
/// Probe the serving slot only. `latest_complete_fresh` opens git (advancing
/// `.git/index` mtime) and may post a freshness wake, which is exactly the
/// false-stale / extra-receipt failure the post-reconcile witness exists to
/// prevent.
async fn wait_for_live_complete_generation(
    registry: &CodeIndexSchedulerRegistryV1,
    path: &Path,
) -> super::LatestCompleteCodeIndexV1 {
    wait_until_serving_seat(registry, path, SERVING_SEAT_FAILURE_CEILING, || {
        registry.latest_complete_serving_for_test(path)
    })
    .await
}

/// Historical 10 ms / 5 s poll of the serving slot. Kept only so the
/// regression can prove it misses a seat that arrives after the deadline.
async fn wait_for_live_complete_generation_by_polling(
    registry: &CodeIndexSchedulerRegistryV1,
    path: &Path,
) -> super::LatestCompleteCodeIndexV1 {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(latest) = registry.latest_complete_serving_for_test(path).await {
            return latest;
        }
        assert!(Instant::now() <= deadline, "live generation");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn wait_for_dashboard_ready(registry: &CodeIndexSchedulerRegistryV1, path: &Path) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let ready = registry
                .dashboard_freshness(path)
                .await
                .is_some_and(|freshness| {
                    freshness.staleness_state.as_deref() == Some("fresh")
                        && freshness.coverage == "complete"
                });
            if ready {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("dashboard reaches fresh complete state");
}

/// Wait until exact/lexical text serving is seated for `path`.
///
/// Publication broadcast and [`CodeIndexSchedulerRegistryV1::latest_generation_id`]
/// stay behind optional graph seating. Unpinned exact/lexical queries resolve
/// through the text owner, so that slot is the typed receipt this wait joins.
/// The text lane publishes the per-worktree serving-generation watch, so
/// [`wait_until_serving_seat`] blocks on that signal rather than sampling.
async fn wait_for_queryable_text_generation(
    registry: &CodeIndexSchedulerRegistryV1,
    path: &Path,
) -> super::LatestCodeTextGenerationV1 {
    wait_until_serving_seat(registry, path, SERVING_SEAT_FAILURE_CEILING, || async {
        registry.latest_text_serving_for_root(path).await
    })
    .await
}

/// The generation id of the text-current seat.
///
/// Tests whose assertions only need exact/lexical text serving (a saved edit
/// was reconciled, a newer generation is current, a scope resolves) must wait
/// on this rather than on [`CodeIndexSchedulerRegistryV1::latest_generation_id`]:
/// that resolver prefers the graph-bearing serving slot and keeps answering
/// the previous generation until optional graph activation of the successor
/// finishes, so a text-only test waiting on it was really waiting on graph
/// seating (issue #917).
async fn wait_for_queryable_text_generation_id(
    registry: &CodeIndexSchedulerRegistryV1,
    path: &Path,
) -> tracedecay_domain::CodeGenerationId {
    wait_for_queryable_text_generation(registry, path)
        .await
        .metadata()
        .manifest()
        .generation_id
        .clone()
}

/// Wait until the text owner seats a generation distinct from `previous`.
///
/// Same signal as [`wait_for_queryable_text_generation`], narrowed to a
/// successor: the seat that replaces `previous` is itself a per-worktree
/// serving-generation change.
async fn wait_for_queryable_text_generation_change(
    registry: &CodeIndexSchedulerRegistryV1,
    path: &Path,
    previous: &tracedecay_domain::CodeGenerationId,
) -> super::LatestCodeTextGenerationV1 {
    wait_until_serving_seat(registry, path, SERVING_SEAT_FAILURE_CEILING, || async {
        registry
            .latest_text_serving_for_root(path)
            .await
            .filter(|text| &text.metadata().manifest().generation_id != previous)
    })
    .await
}

/// Wait until the mounted worktree seats a generation distinct from `previous`.
///
/// Same signal as [`wait_for_initial_generation`]: the successor may appear as
/// a text seat, and the text lane publishes the per-worktree serving-generation
/// watch.
async fn wait_for_generation_change(
    registry: &CodeIndexSchedulerRegistryV1,
    path: &Path,
    previous: &tracedecay_domain::CodeGenerationId,
) -> tracedecay_domain::CodeGenerationId {
    wait_until_serving_seat(registry, path, SERVING_SEAT_FAILURE_CEILING, || async {
        registry
            .latest_generation_id(path)
            .await
            .filter(|generation| generation != previous)
    })
    .await
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
