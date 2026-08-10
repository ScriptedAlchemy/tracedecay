#![allow(clippy::duplicate_mod, clippy::type_complexity)]
//! Anchor-retargeting acceptance coverage for the fact provenance contract:
//! moving refs, rewriting a branch, or removing a checkout must never retarget
//! retained commit/tree/blob or captured-state anchors, and unavailable git
//! state must surface a safe typed state instead of resolving against ambient
//! `HEAD`.
//!
//! The live provenance probe is crate-internal, so the real-git cases build
//! the capture from exact repository object ids through the public domain
//! contract, persist it through the authoritative observation store, mutate
//! the repository, and then resolve through the daemon admission facade.
//! Capture-side object-store hygiene and the ambient-`HEAD` fallback contract
//! are covered next to the probe in `src/repository_provenance.rs`.

use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;
use tracedecay::host_admission::HostAdmissionTestRuntimeV1;
use tracedecay_domain::research::{
    AttributionGap, DomainError, GitTruthManifest, LogSafeText, ResearchAnchorSubjectV1,
    ResearchAnchorTombstoneV1, ResearchBundleEnvelopeV1, ResearchBundleManifestV1,
    ResearchContextAnchorV1, RetrievalRecipeV1, SanitizationReceiptRefV1,
    SanitizationReceiptResolverV1, SanitizedTextRefV1,
};
use tracedecay_domain::{
    AccessPolicyDigest, AnchorDurabilityClass, AnchorSourceGenerationV2, BlobId,
    CanonicalObservationIdV1, CapabilityId, ClaudeByteRangeV1, ClaudeFileGenerationV1,
    ClaudeObservationIdentityMaterialV1, ClaudeSourceCursorV1, ClaudeSourceIdentityV1, CommitId,
    ComponentVersion, CoverageReportV1, DurableClaudeObservationV1, EvidenceAvailabilityV1,
    EvidenceClass, FactOwnerV1, GenerationBoundRepositoryProvenanceV1, ObservationScopeV1,
    PayloadAccessState, PayloadReferenceV1, PrivacyDomainBoundLocatorDigest, PrivacyDomainId,
    ProjectId, ProjectionGenerationId, RefId, RepositoryCaptureId, RepositoryEvidenceV1,
    RepositoryId, RepositoryProvenanceV1, RepositoryRemoteIdentityV1, ResolutionAuthorizationV1,
    RetentionClass, RetrievalAnchorId, RetrievalAnchorRecordV2, RetrievalAnchorRecordV2Parts,
    RetrievalAnchorTargetV2, SanitizationReceiptId, SanitizationReceiptV1, SanitizerDispositionV1,
    ScopeResolutionId, SensitivityV1, SessionId, TreeId, UtcMicros, VectorWatermark, WorktreeId,
};
use tracedecay_store::{
    AnchoredObservationWrite, ObservationPersistOutcome, ObservationStore, ObservationWrite,
    SESSION_MESSAGE_PROJECTOR_VERSION, build_observation_resolution_authorization_v1,
    build_observation_retrieval_anchor_v2,
};
use tracedecay_usecases::host_admission::HostAdmissionScope;
use tracedecay_usecases::memory::EvidenceAnchorResolver;

use crate::common::git_program;

#[allow(dead_code, clippy::duplicate_mod)]
#[path = "research_anchors/support.rs"]
mod support;

use support::valid_fixture;

const FIXTURE: &str = "tests/fixtures/v2/research-anchor-manifest.json";

#[derive(Debug)]
struct ResearchAnchorFixtureV1 {
    envelope: ResearchBundleEnvelopeV1,
    tombstones: Vec<ResearchAnchorTombstoneV1>,
    #[allow(dead_code)]
    sanitization_receipts: Vec<CaptureSanitizationReceiptV1>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
struct StrictResearchAnchorFixtureV1 {
    envelope: ResearchBundleEnvelopeV1,
    tombstones: Vec<ResearchAnchorTombstoneV1>,
    sanitization_receipts: Vec<CaptureSanitizationReceiptV1>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawResearchAnchorFixtureV1 {
    envelope: Value,
    tombstones: Vec<ResearchAnchorTombstoneV1>,
    sanitization_receipts: Vec<CaptureSanitizationReceiptV1>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureSanitizationReceiptV1 {
    receipt: SanitizationReceiptRefV1,
    value_sha256: BTreeSet<String>,
}

#[derive(Debug)]
struct CaptureReceiptResolver {
    bindings: BTreeMap<SanitizationReceiptRefV1, BTreeSet<String>>,
}

impl CaptureReceiptResolver {
    fn from_receipts(receipts: &[CaptureSanitizationReceiptV1]) -> Result<Self, String> {
        let mut bindings = BTreeMap::new();
        for evidence in receipts {
            if bindings
                .insert(evidence.receipt.clone(), evidence.value_sha256.clone())
                .is_some()
            {
                return Err("duplicate capture sanitization receipt".into());
            }
        }
        Ok(Self { bindings })
    }
}

// SAFETY: this fixture resolver accepts only receipt/value bindings whose exact-byte
// SHA-256 digests are recorded as capture evidence in the checked fixture.
unsafe impl SanitizationReceiptResolverV1 for CaptureReceiptResolver {
    fn verify_receipt_binding(
        &self,
        receipt: &SanitizationReceiptRefV1,
        value: &str,
    ) -> Result<(), DomainError> {
        let digest = hex::encode(Sha256::digest(value.as_bytes()));
        if self
            .bindings
            .get(receipt)
            .is_some_and(|digests| digests.contains(&digest))
        {
            Ok(())
        } else {
            Err(DomainError::UnsafeText {
                field: "capture sanitization receipt binding",
            })
        }
    }
}

const CAPTURED_AT_MICROS: i64 = 1_762_000_000_000_000;
const AUTHORIZATION_DIGEST: &str =
    "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

/// Real on-disk git checkout used to move, rewrite, and delete refs between
/// capture and resolution. The caller owns the enclosing `TempDir`.
struct GitCheckout {
    path: PathBuf,
}

impl GitCheckout {
    fn init(path: &Path) -> Self {
        fs::create_dir_all(path).unwrap();
        let checkout = Self {
            path: path.to_path_buf(),
        };
        checkout.git(&["init", "-q", "-b", "main"]);
        checkout.git(&["config", "user.name", "TraceDecay Test"]);
        checkout.git(&["config", "user.email", "tracedecay@example.invalid"]);
        checkout
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn git(&self, args: &[&str]) -> String {
        let output = Command::new(git_program())
            .args(args)
            .current_dir(&self.path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    fn commit(&self, contents: &str) -> String {
        fs::write(self.path.join("tracked.txt"), contents).unwrap();
        self.git(&["add", "--", "tracked.txt"]);
        self.git(&["commit", "-q", "-m", contents]);
        self.git(&["rev-parse", "HEAD"])
    }

    fn head_tree(&self) -> String {
        self.git(&["rev-parse", "HEAD^{tree}"])
    }

    fn tracked_blob(&self) -> String {
        self.git(&["rev-parse", "HEAD:tracked.txt"])
    }
}

struct PersistedCapture {
    observation_id: CanonicalObservationIdV1,
    anchor_id: RetrievalAnchorId,
    capture_id: RepositoryCaptureId,
    commit: CommitId,
    tree: TreeId,
}

fn opaque_id(prefix: &str, domain: &str, path: &Path) -> String {
    let digest = hex::encode(Sha256::digest(
        format!("anchor-retargeting.{domain}:{}", path.display()).as_bytes(),
    ));
    format!("{prefix}.{digest}")
}

fn locator_digest(domain: &str, path: &Path) -> PrivacyDomainBoundLocatorDigest {
    let digest = hex::encode(Sha256::digest(
        format!("anchor-retargeting.{domain}:{}", path.display()).as_bytes(),
    ));
    PrivacyDomainBoundLocatorDigest::new(format!("sha256:{digest}")).unwrap()
}

fn project_observation(project_id: &ProjectId, seed: &str) -> DurableClaudeObservationV1 {
    let scope = ObservationScopeV1::Project {
        project_id: project_id.clone(),
    };
    let source = ClaudeSourceIdentityV1::new(
        SessionId::new(format!("session.anchor-retargeting.{seed}")).unwrap(),
    )
    .unwrap();
    let payload = serde_json::json!({
        "kind": "assistant_message",
        "body": format!("retained evidence for {seed}"),
    });
    let payload_reference = PayloadReferenceV1::for_payload(&payload).unwrap();
    let receipt = SanitizationReceiptV1::new(
        SanitizationReceiptRefV1::new(
            SanitizationReceiptId::new(format!("receipt.anchor-retargeting.{seed}")).unwrap(),
            ComponentVersion::new("sanitizer.anchor-retargeting.v1").unwrap(),
        )
        .unwrap(),
        SanitizerDispositionV1::Accepted,
        SensitivityV1::NonSensitive,
        Some(payload_reference),
    )
    .unwrap();
    let identity = ClaudeObservationIdentityMaterialV1::new(
        source,
        scope,
        ClaudeFileGenerationV1::new(3).unwrap(),
        ClaudeByteRangeV1::new(0, 64).unwrap(),
    )
    .unwrap();
    DurableClaudeObservationV1::new(
        identity,
        receipt,
        RetentionClass::new("retention.anchor-retargeting").unwrap(),
        payload,
    )
    .unwrap()
}

/// Captures the checkout's exact current HEAD state through the domain
/// contract, the same evidence the crate-internal probe records post
/// sanitization: attached ref, HEAD commit, index tree, path identity, remote
/// identity, and dirty-state classification.
fn capture_head_state(
    checkout: &GitCheckout,
    project_id: &ProjectId,
    observation_id: &CanonicalObservationIdV1,
    projection_generation: &ProjectionGenerationId,
) -> (RepositoryId, GenerationBoundRepositoryProvenanceV1) {
    let commit = CommitId::new(checkout.git(&["rev-parse", "HEAD"])).unwrap();
    let tree = TreeId::new(checkout.head_tree()).unwrap();
    let repository_id =
        RepositoryId::new(opaque_id("repository", "repository", checkout.path())).unwrap();
    let worktree_id = WorktreeId::new(opaque_id("worktree", "worktree", checkout.path())).unwrap();
    let evidence = RepositoryEvidenceV1::new(
        EvidenceAvailabilityV1::Known(RefId::new("refs/heads/main").unwrap()),
        EvidenceAvailabilityV1::Known(commit),
        EvidenceAvailabilityV1::Known(tree),
        EvidenceAvailabilityV1::Known(locator_digest("path-identity", checkout.path())),
        RepositoryRemoteIdentityV1::Missing,
        EvidenceAvailabilityV1::Unknown,
    )
    .unwrap();
    let capture = RepositoryProvenanceV1::new(
        repository_id.clone(),
        Some(project_id.clone()),
        Some(worktree_id),
        locator_digest("canonical-root", checkout.path()),
        evidence,
        UtcMicros(CAPTURED_AT_MICROS),
    )
    .unwrap();
    let binding = GenerationBoundRepositoryProvenanceV1::new(
        projection_generation.clone(),
        capture,
        Some(observation_id.clone()),
    )
    .unwrap();
    (repository_id, binding)
}

async fn persist_head_capture(
    store: &impl ObservationStore,
    project_id: &ProjectId,
    seed: &str,
    checkout: &GitCheckout,
) -> PersistedCapture {
    let observation = project_observation(project_id, seed);
    let next_cursor = ClaudeSourceCursorV1::new(
        observation.source().clone(),
        observation.scope().clone(),
        observation.identity().generation(),
        observation.identity().position().end(),
    )
    .unwrap();
    let write = ObservationWrite::new(observation, None, next_cursor).unwrap();
    let observation = write.observation().clone();
    let projection_generation =
        ProjectionGenerationId::new(SESSION_MESSAGE_PROJECTOR_VERSION).unwrap();
    let authorization =
        build_observation_resolution_authorization_v1(&observation, "anchor-retargeting.v1")
            .unwrap();
    let retrieval_anchor = build_observation_retrieval_anchor_v2(
        &observation,
        projection_generation.clone(),
        UtcMicros(CAPTURED_AT_MICROS),
        authorization.clone(),
    )
    .unwrap();
    let (repository_id, binding) = capture_head_state(
        checkout,
        project_id,
        observation.observation_id(),
        &projection_generation,
    );
    let capture_id = binding.capture_id().clone();
    let evidence = binding.capture().evidence();
    let commit = evidence
        .head_commit()
        .value()
        .expect("captured HEAD commit")
        .clone();
    let tree = evidence
        .index_tree()
        .value()
        .expect("captured index tree")
        .clone();
    let repository_anchor = RetrievalAnchorRecordV2::new(RetrievalAnchorRecordV2Parts {
        target: RetrievalAnchorTargetV2::RepositoryCapture {
            repository_id,
            capture_id: capture_id.clone(),
            receipt: observation.receipt().receipt().clone(),
        },
        owner: observation.scope().clone(),
        aliases: vec![],
        occurred_at: None,
        ingested_at: UtcMicros(CAPTURED_AT_MICROS),
        evidence_class: EvidenceClass::Observed,
        source_generation: AnchorSourceGenerationV2::RepositoryCapture(capture_id.clone()),
        projection_generation: projection_generation.clone(),
        projection_watermark: VectorWatermark::default(),
        coverage: CoverageReportV1::default(),
        source_observations: vec![observation.observation_id().clone()],
        source_anchors: vec![],
        authorization,
        payload_access: PayloadAccessState::Eligible,
        retention_class: observation.retention_class().clone(),
        durability: AnchorDurabilityClass::DurableEvidence,
    })
    .unwrap();
    let write = AnchoredObservationWrite::new(write, retrieval_anchor, projection_generation)
        .unwrap()
        .with_repository_provenance_attachment(
            EvidenceAvailabilityV1::Known(binding),
            Some(repository_anchor.clone()),
        )
        .unwrap();
    match store.persist_observation(write).await.unwrap() {
        ObservationPersistOutcome::Committed(_) => {}
        other => panic!("provenance capture write must commit, got {other:?}"),
    }
    PersistedCapture {
        observation_id: observation.observation_id().clone(),
        anchor_id: repository_anchor.anchor_id().clone(),
        capture_id,
        commit,
        tree,
    }
}

async fn resolve_capture_anchor(
    runtime: &HostAdmissionTestRuntimeV1,
    project_id: &ProjectId,
    anchor_id: &RetrievalAnchorId,
) -> RetrievalAnchorRecordV2 {
    let facade = runtime.facade();
    facade
        .resolve_evidence_anchor(
            FactOwnerV1::Project {
                project_id: project_id.clone(),
            },
            anchor_id.clone(),
        )
        .await
        .unwrap()
        .into_record()
}

async fn project_runtime(
    tmp: &TempDir,
    project_root: &Path,
    project_id: &ProjectId,
) -> HostAdmissionTestRuntimeV1 {
    assert!(
        tracedecay::storage::write_repository_identity_marker(project_root, project_id.as_str())
            .unwrap()
    );
    tracedecay::storage::write_enrollment_marker(
        project_root,
        &tracedecay::storage::EnrollmentMarker {
            project_id: project_id.as_str().to_owned(),
            storage_mode: tracedecay::storage::StorageMode::ProfileSharded,
        },
    )
    .unwrap();
    HostAdmissionTestRuntimeV1::project(
        tmp.path().join("profile"),
        project_root,
        project_id.clone(),
    )
    .await
    .unwrap()
}

async fn reopen_project_runtime(
    tmp: &TempDir,
    project_root: &Path,
    project_id: &ProjectId,
) -> HostAdmissionTestRuntimeV1 {
    HostAdmissionTestRuntimeV1::project(
        tmp.path().join("profile"),
        project_root,
        project_id.clone(),
    )
    .await
    .unwrap()
}

fn assert_resolution_targets_original_capture(
    record: &RetrievalAnchorRecordV2,
    persisted: &PersistedCapture,
) {
    assert_eq!(record.anchor_id(), &persisted.anchor_id);
    let RetrievalAnchorTargetV2::RepositoryCapture { capture_id, .. } = record.target() else {
        panic!("retained anchor must keep targeting the repository capture");
    };
    assert_eq!(capture_id, &persisted.capture_id);
    assert!(
        matches!(
            record.source_generation(),
            AnchorSourceGenerationV2::RepositoryCapture(capture_id)
                if capture_id == &persisted.capture_id
        ),
        "source generation must stay bound to the original capture"
    );
    assert_eq!(record.payload_access(), PayloadAccessState::Eligible);
}

async fn assert_retained_head_state(
    store: &impl ObservationStore,
    persisted: &PersistedCapture,
    forbidden_commits: &[String],
) {
    let stored = store
        .get_observation(&persisted.observation_id)
        .await
        .unwrap()
        .expect("retained observation must survive ref mutation");
    let binding = stored
        .repository_provenance_attachment()
        .availability()
        .value()
        .expect("repository provenance must stay retained");
    assert_eq!(binding.capture_id(), &persisted.capture_id);
    let evidence = binding.capture().evidence();
    assert_eq!(
        evidence.head_commit(),
        &EvidenceAvailabilityV1::Known(persisted.commit.clone()),
        "retained evidence must keep the captured commit"
    );
    assert_eq!(
        evidence.index_tree(),
        &EvidenceAvailabilityV1::Known(persisted.tree.clone()),
        "retained evidence must keep the captured tree"
    );
    assert!(matches!(
        evidence.attached_ref(),
        EvidenceAvailabilityV1::Known(_)
    ));
    for forbidden in forbidden_commits {
        assert_ne!(
            evidence.head_commit().value().map(CommitId::as_str),
            Some(forbidden.as_str()),
            "retained evidence must never silently switch to the moved ref target"
        );
    }
}

#[tokio::test]
async fn moved_branch_ref_keeps_resolution_on_the_captured_commit() {
    let tmp = TempDir::new().unwrap();
    let checkout = GitCheckout::init(tmp.path());
    let commit_a = checkout.commit("initial evidence");
    let project_id = ProjectId::new("project.anchor-retargeting.move").unwrap();
    let runtime = project_runtime(&tmp, checkout.path(), &project_id).await;
    let store = runtime
        .observation_store(HostAdmissionScope::Project)
        .unwrap();
    let persisted = persist_head_capture(&store, &project_id, "move", &checkout).await;
    assert_eq!(persisted.commit.as_str(), commit_a);

    // Move the branch ref to commit B after the anchor was retained.
    let commit_b = checkout.commit("moved ref target");
    assert_ne!(commit_a, commit_b);
    assert_eq!(checkout.git(&["rev-parse", "refs/heads/main"]), commit_b);

    // Restart the store so resolution runs against reopened retained state.
    drop(runtime);
    let runtime = reopen_project_runtime(&tmp, checkout.path(), &project_id).await;
    let record = resolve_capture_anchor(&runtime, &project_id, &persisted.anchor_id).await;
    assert_resolution_targets_original_capture(&record, &persisted);
    let store = runtime
        .observation_store(HostAdmissionScope::Project)
        .unwrap();
    assert_retained_head_state(&store, &persisted, &[commit_b]).await;
}

#[tokio::test]
async fn force_rewritten_branch_keeps_resolution_on_the_captured_commit() {
    let tmp = TempDir::new().unwrap();
    let checkout = GitCheckout::init(tmp.path());
    let commit_a = checkout.commit("base evidence");
    let project_id = ProjectId::new("project.anchor-retargeting.rewrite").unwrap();
    let runtime = project_runtime(&tmp, checkout.path(), &project_id).await;
    let store = runtime
        .observation_store(HostAdmissionScope::Project)
        .unwrap();
    let persisted = persist_head_capture(&store, &project_id, "rewrite", &checkout).await;
    assert_eq!(persisted.commit.as_str(), commit_a);

    // Force-update the branch: amend replaces the tip with a rewritten object.
    fs::write(checkout.path().join("tracked.txt"), "rewritten evidence").unwrap();
    checkout.git(&["add", "--", "tracked.txt"]);
    checkout.git(&["commit", "-q", "--amend", "--no-edit"]);
    let rewritten = checkout.git(&["rev-parse", "refs/heads/main"]);
    assert_ne!(commit_a, rewritten);

    drop(runtime);
    let runtime = reopen_project_runtime(&tmp, checkout.path(), &project_id).await;
    let record = resolve_capture_anchor(&runtime, &project_id, &persisted.anchor_id).await;
    assert_resolution_targets_original_capture(&record, &persisted);
    let store = runtime
        .observation_store(HostAdmissionScope::Project)
        .unwrap();
    assert_retained_head_state(&store, &persisted, &[rewritten]).await;
}

#[tokio::test]
async fn deleted_ref_and_removed_checkout_never_resolve_against_ambient_head() {
    let tmp = TempDir::new().unwrap();
    let checkout_path = tmp.path().join("checkout");
    let checkout = GitCheckout::init(&checkout_path);
    let commit_a = checkout.commit("retained evidence");
    let project_id = ProjectId::new("project.anchor-retargeting.delete").unwrap();
    let runtime = project_runtime(&tmp, checkout.path(), &project_id).await;
    let store = runtime
        .observation_store(HostAdmissionScope::Project)
        .unwrap();
    let persisted = persist_head_capture(&store, &project_id, "delete", &checkout).await;
    assert_eq!(persisted.commit.as_str(), commit_a);

    // Delete the ref entirely, then remove the checkout.
    checkout.git(&["update-ref", "-d", "refs/heads/main"]);
    fs::remove_dir_all(&checkout_path).unwrap();
    assert!(!checkout_path.exists());

    // Whatever repository now occupies the path has its own ambient HEAD.
    let ambient = GitCheckout::init(&checkout_path);
    let ambient_head = ambient.commit("ambient replacement");
    assert_ne!(commit_a, ambient_head);

    drop(runtime);
    let runtime = reopen_project_runtime(&tmp, ambient.path(), &project_id).await;
    let record = resolve_capture_anchor(&runtime, &project_id, &persisted.anchor_id).await;
    assert_resolution_targets_original_capture(&record, &persisted);
    let store = runtime
        .observation_store(HostAdmissionScope::Project)
        .unwrap();
    assert_retained_head_state(&store, &persisted, &[ambient_head]).await;
}

#[test]
fn exact_commit_tree_and_blob_anchor_ids_track_objects_not_refs() {
    let tmp = TempDir::new().unwrap();
    let checkout = GitCheckout::init(tmp.path());
    let commit_a = checkout.commit("object evidence");
    let tree_a = checkout.head_tree();
    let blob_a = checkout.tracked_blob();
    let project_id = ProjectId::new("project.anchor-retargeting.objects").unwrap();
    let repository_id =
        RepositoryId::new(opaque_id("repository", "repository", checkout.path())).unwrap();
    let authorization = ResolutionAuthorizationV1 {
        resolved_scope_id: ScopeResolutionId::new("scope.anchor-retargeting").unwrap(),
        privacy_domain_id: PrivacyDomainId::new("privacy.anchor-retargeting").unwrap(),
        access_policy_digest: AccessPolicyDigest::new(AUTHORIZATION_DIGEST).unwrap(),
        capability_id: CapabilityId::new("capability.anchor-retargeting").unwrap(),
        canonical_request_digest: PrivacyDomainBoundLocatorDigest::new(AUTHORIZATION_DIGEST)
            .unwrap(),
    };
    let exact_anchor = |target: RetrievalAnchorTargetV2, payload_access| {
        RetrievalAnchorRecordV2::new(RetrievalAnchorRecordV2Parts {
            target,
            owner: ObservationScopeV1::Project {
                project_id: project_id.clone(),
            },
            aliases: vec![],
            occurred_at: None,
            ingested_at: UtcMicros(CAPTURED_AT_MICROS),
            evidence_class: EvidenceClass::Observed,
            source_generation: AnchorSourceGenerationV2::Unavailable,
            projection_generation: ProjectionGenerationId::new(SESSION_MESSAGE_PROJECTOR_VERSION)
                .unwrap(),
            projection_watermark: VectorWatermark::default(),
            coverage: CoverageReportV1::default(),
            source_observations: vec![],
            source_anchors: vec![],
            authorization: authorization.clone(),
            payload_access,
            retention_class: RetentionClass::new("retention.anchor-retargeting").unwrap(),
            durability: AnchorDurabilityClass::DurableEvidence,
        })
        .unwrap()
    };
    let commit_target = |commit: &str| RetrievalAnchorTargetV2::ExactRepositoryCommit {
        repository_id: repository_id.clone(),
        commit_id: CommitId::new(commit).unwrap(),
    };
    let commit_anchor = exact_anchor(commit_target(&commit_a), PayloadAccessState::Eligible);
    let tree_anchor = exact_anchor(
        RetrievalAnchorTargetV2::ExactRepositoryTree {
            repository_id: repository_id.clone(),
            tree_id: TreeId::new(&tree_a).unwrap(),
        },
        PayloadAccessState::Eligible,
    );
    let blob_anchor = exact_anchor(
        RetrievalAnchorTargetV2::ExactRepositoryBlob {
            repository_id: repository_id.clone(),
            blob_id: BlobId::new(&blob_a).unwrap(),
        },
        PayloadAccessState::Eligible,
    );
    let anchor_ids = (
        commit_anchor.anchor_id().clone(),
        tree_anchor.anchor_id().clone(),
        blob_anchor.anchor_id().clone(),
    );

    // Move the branch to a new commit; anchor identity is a pure function of
    // the owner and the exact retained object, so it cannot be rekeyed.
    let commit_b = checkout.commit("moved object target");
    assert_ne!(commit_a, commit_b);
    let rebuilt = exact_anchor(commit_target(&commit_a), PayloadAccessState::Eligible);
    assert_eq!(rebuilt.anchor_id(), &anchor_ids.0);
    assert!(matches!(
        rebuilt.target(),
        RetrievalAnchorTargetV2::ExactRepositoryCommit { commit_id, .. }
            if commit_id.as_str() == commit_a
    ));

    // An anchor derived from the moved ref names a different object entirely.
    let retargeted = exact_anchor(commit_target(&commit_b), PayloadAccessState::Eligible);
    assert_ne!(retargeted.anchor_id(), &anchor_ids.0);
    assert_ne!(retargeted.anchor_id(), &anchor_ids.1);
    assert_ne!(retargeted.anchor_id(), &anchor_ids.2);

    // Redaction and deletion update payload availability without changing the
    // owner-bound identity of the retained object.
    let redacted = exact_anchor(commit_target(&commit_a), PayloadAccessState::Redacted);
    let deleted = exact_anchor(commit_target(&commit_a), PayloadAccessState::Deleted);
    assert_eq!(redacted.anchor_id(), &anchor_ids.0);
    assert_eq!(deleted.anchor_id(), &anchor_ids.0);
    assert_eq!(redacted.payload_access(), PayloadAccessState::Redacted);
    assert_eq!(deleted.payload_access(), PayloadAccessState::Deleted);
}

#[test]
fn moved_rewritten_and_deleted_refs_never_retarget_frozen_manifest_anchors() {
    let baseline = valid_fixture();
    let baseline_git_anchors = git_anchor_commits(&baseline);
    assert_eq!(baseline_git_anchors.len(), 3);
    let baseline_catalog_digest = baseline
        .envelope
        .retrieval_catalog
        .compute_digest()
        .unwrap();

    // Refs are routing inputs only: moving the branch to another commit,
    // rewriting the branch tip, or deleting the ref in the frozen git snapshot
    // must never change what an already-recorded anchor means.
    #[allow(clippy::type_complexity)]
    let scenarios: [(&str, fn(&mut GitTruthManifest)); 3] = [
        ("moved ref", |snapshot| {
            snapshot.refs = vec![(
                RefId::new("ref-synthetic-branch-001").unwrap(),
                CommitId::new("commit-synthetic-moved-001").unwrap(),
            )];
        }),
        ("rewritten branch", |snapshot| {
            snapshot.head_commit = CommitId::new("commit-synthetic-rewritten-001").unwrap();
            snapshot.refs = vec![(
                RefId::new("ref-synthetic-branch-001").unwrap(),
                snapshot.head_commit.clone(),
            )];
        }),
        ("deleted ref", |snapshot| {
            snapshot.refs.clear();
        }),
    ];

    for (scenario, mutate) in scenarios {
        let mut drifted = valid_fixture();
        mutate(&mut drifted.envelope.manifest.git_snapshot);
        drifted.envelope.manifest.digest = drifted.envelope.manifest.compute_digest().unwrap();
        drifted
            .envelope
            .validate()
            .unwrap_or_else(|error| panic!("{scenario}: manifest must stay valid: {error:?}"));

        assert_eq!(
            git_anchor_commits(&drifted),
            baseline_git_anchors,
            "{scenario}: git anchors must keep their exact retained commits"
        );
        assert_eq!(
            drifted.envelope.retrieval_catalog.compute_digest().unwrap(),
            baseline_catalog_digest,
            "{scenario}: the frozen retrieval catalog must not move"
        );
        for anchor in &drifted.envelope.manifest.anchors {
            for retrieval_anchor in anchor.retrieval_anchors.iter() {
                let record = drifted
                    .envelope
                    .retrieval_catalog
                    .get(retrieval_anchor)
                    .unwrap_or_else(|| panic!("{scenario}: anchor must stay cataloged"));
                assert_eq!(record.anchor_id, *retrieval_anchor, "{scenario}");
                record.validate().unwrap();
            }
        }
        for tombstone in &drifted.tombstones {
            tombstone
                .validate_against(&drifted.envelope.retrieval_catalog)
                .unwrap();
        }
    }
}

fn git_anchor_commits(fixture: &ResearchAnchorFixtureV1) -> Vec<(String, Option<String>)> {
    fixture
        .envelope
        .manifest
        .anchors
        .iter()
        .filter_map(|anchor| match &anchor.subject {
            ResearchAnchorSubjectV1::Git(subject) => Some((
                anchor.entry_id.as_str().to_owned(),
                subject
                    .commit_id
                    .as_ref()
                    .map(|commit| commit.as_str().to_owned()),
            )),
            _ => None,
        })
        .collect()
}
