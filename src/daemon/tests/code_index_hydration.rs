use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use tempfile::TempDir;
use tracedecay_domain::{
    AuthorizationRevision, CodeSearchChunkId, ComponentRevision, ExactClass,
    FreshnessCompatibilityV1, FreshnessVectorDigest, FusedCandidate, FusionProfileId,
    HydrationReceipt, HydrationRevision, LogicalEvidenceId, OccurrenceProvenance, PrincipalId,
    PrivacyDomainId, RankedCandidate, RefId, RepositoryId, RetrievalAnchorId, RetrievalBudget,
    RetrievalRequest, RetrievalScope, RetrievalSnapshot, SingleRootScopeV1, SourceFreshness,
    SourceInstanceKey, SourceNamespace, SourceOccurrenceId, TemporalModeV1, UtcMicros,
    VectorWatermark, WorktreeId,
};

use super::super::code_index_scheduler::{CodeIndexWorktreeSchedulerV1, SharedCodeIndexBytePoolV1};
use super::super::{code_index_search_display_binding, code_index_search_hydration_budget};
use tracedecay_query::code_search::CodeIndexSearchDisplayV1;
use tracedecay_query::retrieval::hydrate::{
    CanonicalLateHydration, HydrationAuthorizationV1, HydrationOutcomeV1,
    HydrationPreflightOutcomeV1, HydrationReadOutcomeV1, HydrationUnavailableV1,
    HydrationWorkPermitV1, LateHydrationSource,
};

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).expect("valid fixture identity")
}

fn request(max_hydration_bytes: u64) -> RetrievalRequest {
    RetrievalRequest {
        principal: id::<PrincipalId>("principal.code-index-hydration"),
        scope: RetrievalScope {
            privacy_domain: id::<PrivacyDomainId>("privacy.code-index-hydration"),
            root: SingleRootScopeV1 {
                repository: id::<RepositoryId>("repository.code-index-hydration"),
                worktree: Some(id::<WorktreeId>("worktree.code-index-hydration")),
                reference: Some(id::<RefId>("refs/heads/hydration")),
            },
        },
        temporal_mode: TemporalModeV1::Current,
        snapshot: RetrievalSnapshot {
            watermarks: VectorWatermark::default(),
            freshness_digest: FreshnessVectorDigest::new(format!("sha256:{}", "f".repeat(64)))
                .expect("valid freshness digest"),
            authorization_revision: id::<AuthorizationRevision>(
                "authorization.code-index-hydration",
            ),
            captured_at: UtcMicros(1),
        },
        profile_id: id::<FusionProfileId>("profile.code-index-hydration"),
        budget: RetrievalBudget {
            max_candidates_per_lane: 1,
            max_fused_candidates: 1,
            max_hydrated_results: 1,
            max_hydration_bytes,
            deadline_micros: None,
        },
    }
}

fn ranked() -> RankedCandidate {
    let anchor = id::<RetrievalAnchorId>("code-symbol:symbol.hydration");
    let freshness = SourceFreshness {
        source_namespace: id::<SourceNamespace>("ns.code.daemon"),
        source_instance: id::<SourceInstanceKey>("instance.code-index.daemon"),
        source_watermark: Some(1),
        projection_watermark: Some(1),
        observed_at: UtcMicros(1),
        source_generation: Some(1),
        generation_lag: Some(0),
        compatibility: FreshnessCompatibilityV1::Current,
        policy_revision: id::<ComponentRevision>("policy.code-index-hydration"),
    };
    RankedCandidate {
        candidate: FusedCandidate {
            anchor_id: anchor.clone(),
            logical_evidence_id: id::<LogicalEvidenceId>(anchor.as_str()),
            occurrences: vec![OccurrenceProvenance {
                source_occurrence_id: id::<SourceOccurrenceId>(
                    "code-chunk:generation.hydration:chunk.hydration",
                ),
                file_occurrence_id: Some(id("file.hydration")),
                retriever_evidence_anchor: id::<RetrievalAnchorId>("evidence.code-index-hydration"),
                source_namespace: freshness.source_namespace.clone(),
                repository_id: Some(id::<RepositoryId>("repository.code-index-hydration")),
                session_or_thread_id: None,
                logical_copy_cluster_id: None,
                logical_copy_evidence_anchor: None,
                evidence_role: tracedecay_domain::EvidenceRole::Primary,
                freshness,
            }],
            exact_class: ExactClass::Approximate,
            utility_micros: 1,
            contributions: Vec::new(),
            freshness: Vec::new(),
            decisions: Vec::new(),
        },
        final_ordinal: 0,
    }
}

fn display() -> CodeIndexSearchDisplayV1 {
    CodeIndexSearchDisplayV1 {
        name: "hydrate".to_owned(),
        qualified_name: "fixture::hydrate".to_owned(),
        kind: "function".to_owned(),
        path: "src/hydrate.rs".to_owned(),
    }
}

#[derive(Clone, Copy)]
enum FixtureMode {
    RevokeBeforeRead,
    ExceedBudget,
}

struct RecordingHydrationSource {
    mode: FixtureMode,
    authorization_checks: Arc<AtomicU32>,
    preflight_reads: Arc<AtomicU32>,
    payload_reads: Arc<AtomicU32>,
}

impl RecordingHydrationSource {
    fn authorize_now(&self) -> HydrationAuthorizationV1 {
        let check = self
            .authorization_checks
            .fetch_add(1, Ordering::SeqCst)
            .saturating_add(1);
        match self.mode {
            FixtureMode::RevokeBeforeRead if check >= 3 => HydrationAuthorizationV1::Denied,
            FixtureMode::RevokeBeforeRead | FixtureMode::ExceedBudget => {
                HydrationAuthorizationV1::Authorized
            }
        }
    }
}

impl LateHydrationSource<CodeIndexSearchDisplayV1> for RecordingHydrationSource {
    fn authorize(
        &mut self,
        _request: &RetrievalRequest,
        _candidate: &RankedCandidate,
    ) -> HydrationAuthorizationV1 {
        self.authorize_now()
    }

    fn preflight_authorized(
        &mut self,
        _request: &RetrievalRequest,
        _candidate: &RankedCandidate,
        _permit: &HydrationWorkPermitV1,
    ) -> HydrationPreflightOutcomeV1 {
        match self.authorize_now() {
            HydrationAuthorizationV1::Authorized => {
                self.preflight_reads.fetch_add(1, Ordering::SeqCst);
                HydrationPreflightOutcomeV1::Ready {
                    estimated_bytes: match self.mode {
                        FixtureMode::RevokeBeforeRead => 1,
                        FixtureMode::ExceedBudget => 2,
                    },
                }
            }
            HydrationAuthorizationV1::Denied
            | HydrationAuthorizationV1::Unavailable(HydrationUnavailableV1::AuthorityUnavailable) => {
                HydrationPreflightOutcomeV1::Unavailable(
                    HydrationUnavailableV1::AuthorityUnavailable,
                )
            }
            HydrationAuthorizationV1::Unavailable(reason) => {
                HydrationPreflightOutcomeV1::Unavailable(reason)
            }
        }
    }

    fn hydrate_authorized(
        &mut self,
        _request: &RetrievalRequest,
        candidate: &RankedCandidate,
        _permit: &HydrationWorkPermitV1,
    ) -> HydrationReadOutcomeV1<CodeIndexSearchDisplayV1> {
        match self.authorize_now() {
            HydrationAuthorizationV1::Authorized => {
                self.payload_reads.fetch_add(1, Ordering::SeqCst);
                let occurrence = &candidate.candidate.occurrences[0];
                HydrationReadOutcomeV1::Complete {
                    payload: display(),
                    receipt: HydrationReceipt {
                        anchor_id: candidate.candidate.anchor_id.clone(),
                        source_occurrence_id: occurrence.source_occurrence_id.clone(),
                        hydration_revision: id::<HydrationRevision>(
                            "hydration.code-index.display.v1",
                        ),
                        bytes_hydrated: 1,
                        authorized: true,
                        freshness: occurrence.freshness.clone(),
                    },
                }
            }
            HydrationAuthorizationV1::Denied
            | HydrationAuthorizationV1::Unavailable(HydrationUnavailableV1::AuthorityUnavailable) => {
                HydrationReadOutcomeV1::Unavailable(HydrationUnavailableV1::AuthorityUnavailable)
            }
            HydrationAuthorizationV1::Unavailable(reason) => {
                HydrationReadOutcomeV1::Unavailable(reason)
            }
        }
    }
}

#[test]
fn revocation_before_hydration_prevents_payload_read() {
    let authorization_checks = Arc::new(AtomicU32::new(0));
    let preflight_reads = Arc::new(AtomicU32::new(0));
    let payload_reads = Arc::new(AtomicU32::new(0));
    let mut source = RecordingHydrationSource {
        mode: FixtureMode::RevokeBeforeRead,
        authorization_checks: Arc::clone(&authorization_checks),
        preflight_reads: Arc::clone(&preflight_reads),
        payload_reads: Arc::clone(&payload_reads),
    };

    let request = request(1);
    let page = CanonicalLateHydration::new(&mut source)
        .hydrate(&request, &[ranked()], &request.budget)
        .expect("typed revocation outcome");

    assert_eq!(authorization_checks.load(Ordering::SeqCst), 3);
    assert_eq!(preflight_reads.load(Ordering::SeqCst), 1);
    assert_eq!(payload_reads.load(Ordering::SeqCst), 0);
    assert!(matches!(
        page.results[0].outcome,
        HydrationOutcomeV1::Unavailable(HydrationUnavailableV1::AuthorityUnavailable)
    ));
}

#[test]
fn hydration_budget_rejects_payload_read_after_preflight() {
    let authorization_checks = Arc::new(AtomicU32::new(0));
    let preflight_reads = Arc::new(AtomicU32::new(0));
    let payload_reads = Arc::new(AtomicU32::new(0));
    let mut source = RecordingHydrationSource {
        mode: FixtureMode::ExceedBudget,
        authorization_checks: Arc::clone(&authorization_checks),
        preflight_reads: Arc::clone(&preflight_reads),
        payload_reads: Arc::clone(&payload_reads),
    };

    let request = request(1);
    let page = CanonicalLateHydration::new(&mut source)
        .hydrate(&request, &[ranked()], &request.budget)
        .expect("typed budget outcome");

    assert_eq!(authorization_checks.load(Ordering::SeqCst), 2);
    assert_eq!(preflight_reads.load(Ordering::SeqCst), 1);
    assert_eq!(payload_reads.load(Ordering::SeqCst), 0);
    assert!(matches!(
        page.results[0].outcome,
        HydrationOutcomeV1::Unavailable(HydrationUnavailableV1::BudgetExceeded)
    ));
}

#[test]
fn accepted_semantic_budget_replaces_distinct_query_hydration_budget() {
    let query_budget = request(1).budget;
    let semantic_budget = RetrievalBudget {
        max_candidates_per_lane: 7,
        max_fused_candidates: 6,
        max_hydrated_results: 5,
        max_hydration_bytes: 4_096,
        deadline_micros: Some(3),
    };

    assert_eq!(
        code_index_search_hydration_budget(Some(&semantic_budget), &query_budget),
        semantic_budget
    );
    assert_eq!(
        code_index_search_hydration_budget(None, &query_budget),
        query_budget
    );
}

fn git(root: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(root)
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed");
}

#[test]
fn production_semantic_chunk_candidate_hydrates_from_frozen_generation() {
    let project = TempDir::new().expect("project");
    git(project.path(), &["init", "-q", "-b", "main"]);
    git(project.path(), &["config", "user.name", "TraceDecay Test"]);
    git(
        project.path(),
        &["config", "user.email", "tracedecay@example.invalid"],
    );
    std::fs::create_dir_all(project.path().join("src")).expect("source directory");
    std::fs::write(
        project.path().join("src/lib.rs"),
        "pub fn semantic_target() -> u32 { 7 }\n",
    )
    .expect("source");
    git(project.path(), &["add", "."]);
    git(project.path(), &["commit", "-qm", "fixture"]);

    let store = TempDir::new().expect("store");
    let mut scheduler = CodeIndexWorktreeSchedulerV1::open(
        tracedecay_domain::ProjectId::new("project.code-index-hydration").expect("valid project"),
        project.path(),
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    )
    .expect("scheduler");
    scheduler.reconcile_now().expect("publish generation");
    let latest = scheduler.latest_complete().expect("complete generation");
    let generation = latest.generation();
    let chunk = generation
        .chunks()
        .chunks()
        .iter()
        .find(|chunk| chunk.anchor.symbol_occurrence_id.is_some())
        .expect("symbol-backed production chunk");
    let chunk_id = CodeSearchChunkId::new(chunk.id.as_str().to_owned()).expect("chunk id");
    let anchor = RetrievalAnchorId::new(format!("code-chunk:{}", chunk_id.as_str()))
        .expect("semantic chunk anchor");
    let source_occurrence = SourceOccurrenceId::new(format!("code-chunk:{}", chunk_id.as_str()))
        .expect("semantic source occurrence");
    let freshness = tracedecay_query::retrieval::graph::production_code_index_freshness(
        generation.manifest().seal.sealed_at,
        ComponentRevision::new("policy.semantic.daemon.v1").expect("policy revision"),
    )
    .expect("freshness");
    let candidate = RankedCandidate {
        candidate: FusedCandidate {
            anchor_id: anchor.clone(),
            logical_evidence_id: LogicalEvidenceId::new(anchor.as_str().to_owned())
                .expect("logical evidence"),
            occurrences: vec![OccurrenceProvenance {
                source_occurrence_id: source_occurrence.clone(),
                file_occurrence_id: Some(chunk.anchor.file_occurrence_id.clone()),
                retriever_evidence_anchor: RetrievalAnchorId::new(format!(
                    "code-semantic:{}",
                    chunk_id.as_str()
                ))
                .expect("semantic evidence"),
                source_namespace: freshness.source_namespace.clone(),
                repository_id: Some(generation.snapshot().repository.clone()),
                session_or_thread_id: None,
                logical_copy_cluster_id: None,
                logical_copy_evidence_anchor: None,
                evidence_role: tracedecay_domain::EvidenceRole::Primary,
                freshness: freshness.clone(),
            }],
            exact_class: ExactClass::Approximate,
            utility_micros: 1,
            contributions: Vec::new(),
            freshness: vec![freshness],
            decisions: Vec::new(),
        },
        final_ordinal: 0,
    };
    let request = RetrievalRequest {
        principal: PrincipalId::new("principal.semantic-hydration").expect("principal"),
        scope: RetrievalScope {
            privacy_domain: generation.manifest().privacy_domain.clone(),
            root: SingleRootScopeV1 {
                repository: generation.snapshot().repository.clone(),
                worktree: generation.snapshot().worktree.clone(),
                reference: generation.snapshot().reference.clone(),
            },
        },
        temporal_mode: TemporalModeV1::Current,
        snapshot: RetrievalSnapshot {
            watermarks: VectorWatermark::default(),
            freshness_digest: FreshnessVectorDigest::new(
                generation.manifest().snapshot_digest.as_str(),
            )
            .expect("snapshot freshness"),
            authorization_revision: AuthorizationRevision::new("authorization.semantic-hydration")
                .expect("authorization revision"),
            captured_at: generation.manifest().seal.sealed_at,
        },
        profile_id: FusionProfileId::new("profile.semantic-hydration").expect("profile"),
        budget: RetrievalBudget {
            max_candidates_per_lane: 1,
            max_fused_candidates: 1,
            max_hydrated_results: 1,
            max_hydration_bytes: 65_536,
            deadline_micros: None,
        },
    };

    let display_paths =
        super::super::code_index_executor::CodeIndexDisplayPathIndexV1::for_generation(generation)
            .expect("display path index for the sealed generation");
    let (display, provenance) =
        code_index_search_display_binding(generation, &display_paths, &request, &candidate)
            .expect("frozen semantic chunk hydration");
    let symbol_occurrence = chunk
        .anchor
        .symbol_occurrence_id
        .as_ref()
        .expect("symbol occurrence");
    let symbol = generation
        .symbols()
        .symbols
        .iter()
        .find(|symbol| symbol.occurrence == *symbol_occurrence)
        .expect("indexed symbol");
    assert_eq!(display.qualified_name, symbol.qualified_name);
    assert_eq!(display.kind, symbol.kind);
    let declaring_file = generation
        .snapshot()
        .files
        .iter()
        .find(|file| file.logical_path == display.path)
        .expect("display path names a snapshot file");
    assert_eq!(
        declaring_file.disposition,
        tracedecay_domain::SnapshotFileDispositionV1::Present
    );
    assert_eq!(provenance.source_occurrence_id, source_occurrence);
    assert_eq!(
        candidate.candidate.occurrences[0].freshness,
        provenance.freshness
    );
    assert_eq!(
        candidate.candidate.occurrences[0].source_namespace,
        provenance.source_namespace
    );
    let rerank_symbol = crate::semantic_code::rerank_adapter::resolve_generation_chunk(
        generation,
        &format!("code-symbol:{}", symbol_occurrence.as_str()),
    )
    .expect("symbol rerank payload");
    let rerank_chunk = crate::semantic_code::rerank_adapter::resolve_generation_chunk(
        generation,
        &format!("code-chunk:{}", chunk_id.as_str()),
    )
    .expect("chunk rerank payload");
    assert_eq!(rerank_symbol.id, chunk.id);
    assert_eq!(rerank_chunk.id, chunk.id);
    assert_eq!(rerank_symbol.sanitized_text, rerank_chunk.sanitized_text);
}
