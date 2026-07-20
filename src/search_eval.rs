//! Canonical Plan 15 search-quality fixture validator and comparison harness.
//!
//! The checked-in PR9 packet is contract-only. This module validates all
//! fixture bytes and emits a typed `blocked` result when the authorized
//! locked-quality artifact is unavailable. Holdout capability bytes are never
//! opened until a locked-quality run manifest has passed every pre-reveal
//! check.

pub mod holdout;

use self::holdout::{
    HoldoutAuthorityError, HoldoutAuthorityStoreV1, HoldoutEnvelopePayloadV1,
    HoldoutRegistryRecordV1,
};
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tracedecay_domain::{
    AcceptedPr9CandidateEvidenceDigest, AcceptedPr9CandidateEvidenceV1,
    AgentAdjudicatedLabelProvenanceV1, AuthorizationCanaryV1, ContextSpanV1, DecisionRecordDigest,
    DecisionRecordV1, EvalOutcomeV1, EvalPartitionV1, EvalQueryV1, EvaluationContractError,
    EvaluationFixtureBundleV1, EvaluationTaskV1, EvidenceBatchDigest, EvidenceBatchId,
    EvidenceBatchV1, EvidenceClaimId, EvidenceIndexDigest, EvidenceIndexEntryV1, EvidenceIndexId,
    EvidenceIndexV1, ExactAdmissionOracleV1, FixtureAuthorityV1, FixtureContentDigest,
    FixtureFileDigestV1, FixtureManifestDigest, FixtureManifestV1, HoldoutAccessReceiptV1,
    HoldoutLabelAuthorityV1, HoldoutSealDigest, HoldoutSealV1, LabelSetDigest, LabelSetId,
    LabelSetV1, QueryFamilyV1, QueryWorkloadV1, RelevanceJudgmentV1, RunManifestDigest,
    RunManifestV1, SavedCandidateSetV1, TemporalEventV1, WorkloadDigest,
};

const ZERO_DIGEST: &str = "sha256:0000000000000000000000000000000000000000000000000000000000000000";
const REPORT_DIGEST_DOMAIN: &str = "tracedecay.search-quality-report.v1";
pub const PR9_ACCEPTANCE_DECISION_EXPRESSION: &str = "pr9_candidate_matrix_complete && bound_label_authority in [deterministic,human_authoritative,agent_adjudicated] && durable_run_bound_receipt";

#[derive(Debug, Error)]
pub enum SearchEvalError {
    #[error("read {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("write {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("parse {path}: {source}")]
    Parse {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("serialize evaluation artifact: {0}")]
    Serialize(serde_json::Error),
    #[error(transparent)]
    Contract(#[from] EvaluationContractError),
    #[error(transparent)]
    HoldoutAuthority(#[from] holdout::HoldoutAuthorityError),
    #[error("invalid run manifest: {0}")]
    InvalidRun(String),
    #[error("immutable run output violation: {0}")]
    ImmutableOutput(String),
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ValidationSummary {
    pub command: &'static str,
    pub status: &'static str,
    pub authority: FixtureAuthorityV1,
    pub fixture_manifest_digest: FixtureManifestDigest,
    pub run_manifest_digest: RunManifestDigest,
    pub workload_digest: WorkloadDigest,
    pub development_label_digest: LabelSetDigest,
    pub holdout_seal_digest: HoldoutSealDigest,
    pub query_count: usize,
    pub development_query_count: usize,
    pub sealed_holdout_query_count: usize,
}

#[derive(Clone, Debug)]
pub struct CompareOptions {
    pub fixture_root: PathBuf,
    pub run_manifest: Option<PathBuf>,
    pub output_root: PathBuf,
    /// Opaque locator resolved only through the private holdout authority
    /// store after the frozen run passes pre-reveal validation.
    pub holdout_capability: Option<String>,
    pub holdout_profile_root: Option<PathBuf>,
    pub holdout_seal: Option<PathBuf>,
    pub saved_candidates: Option<PathBuf>,
    pub required_outcome: Option<EvalOutcomeV1>,
}

#[derive(Clone, Debug)]
pub struct SealHoldoutOptions {
    pub fixture_root: PathBuf,
    pub run_manifest: Option<PathBuf>,
    pub labels_path: PathBuf,
    pub profile_root: Option<PathBuf>,
    pub label_authority: HoldoutLabelAuthorityV1,
    pub signed_by: tracedecay_domain::DecisionOwnerId,
    pub trust_root_id: String,
    pub signed_at_unix: u64,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct SealHoldoutResult {
    pub labels: HoldoutRegistryRecordV1,
    pub envelope: HoldoutRegistryRecordV1,
    pub seal: HoldoutSealV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RequiredExternalArtifactV1 {
    pub kind: String,
    pub locator: String,
    pub digest: HoldoutSealDigest,
    pub signature_locator: String,
    pub reason: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SavedCandidateAblationSummaryV1 {
    pub disabled_lane: String,
    pub retained_candidate_lists: usize,
    pub retained_candidates: usize,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct CompareResult {
    pub command: &'static str,
    pub outcome: EvalOutcomeV1,
    pub required_outcome: Option<EvalOutcomeV1>,
    pub requirement_satisfied: bool,
    pub rationale: String,
    pub run_id: String,
    pub run_revision: u32,
    pub run_manifest_digest: RunManifestDigest,
    pub blocked_on: Vec<RequiredExternalArtifactV1>,
    pub saved_candidate_ablations: Vec<SavedCandidateAblationSummaryV1>,
    pub report_path: PathBuf,
    pub evidence_index_path: PathBuf,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct EvaluationReportV1 {
    schema_revision: u32,
    run_id: tracedecay_domain::RunId,
    run_revision: u32,
    run_manifest_digest: RunManifestDigest,
    fixture_manifest_digest: FixtureManifestDigest,
    outcome: EvalOutcomeV1,
    rationale: String,
    no_access_before_lock: bool,
    blocked_on: Vec<RequiredExternalArtifactV1>,
    saved_candidate_ablations: Vec<SavedCandidateAblationSummaryV1>,
    decision: DecisionRecordV1,
    #[serde(default)]
    supersedes_report_digest: Option<FixtureContentDigest>,
    report_digest: FixtureContentDigest,
}

#[derive(Serialize)]
struct EvaluationReportDigestInput<'a> {
    domain: &'static str,
    schema_revision: u32,
    run_id: &'a tracedecay_domain::RunId,
    run_revision: u32,
    run_manifest_digest: &'a RunManifestDigest,
    fixture_manifest_digest: &'a FixtureManifestDigest,
    outcome: EvalOutcomeV1,
    rationale: &'a str,
    no_access_before_lock: bool,
    blocked_on: &'a [RequiredExternalArtifactV1],
    saved_candidate_ablations: &'a [SavedCandidateAblationSummaryV1],
    decision: &'a DecisionRecordV1,
    supersedes_report_digest: &'a Option<FixtureContentDigest>,
}

impl EvaluationReportV1 {
    fn compute_digest(&self) -> Result<FixtureContentDigest, SearchEvalError> {
        let input = EvaluationReportDigestInput {
            domain: REPORT_DIGEST_DOMAIN,
            schema_revision: self.schema_revision,
            run_id: &self.run_id,
            run_revision: self.run_revision,
            run_manifest_digest: &self.run_manifest_digest,
            fixture_manifest_digest: &self.fixture_manifest_digest,
            outcome: self.outcome,
            rationale: &self.rationale,
            no_access_before_lock: self.no_access_before_lock,
            blocked_on: &self.blocked_on,
            saved_candidate_ablations: &self.saved_candidate_ablations,
            decision: &self.decision,
            supersedes_report_digest: &self.supersedes_report_digest,
        };
        let bytes = serde_json::to_vec(&input).map_err(SearchEvalError::Serialize)?;
        FixtureContentDigest::new(sha256_bytes(&bytes)).map_err(SearchEvalError::Contract)
    }

    fn verify_digest(&self) -> Result<(), SearchEvalError> {
        if self.compute_digest()? == self.report_digest {
            Ok(())
        } else {
            Err(SearchEvalError::InvalidRun(
                "prior report digest does not verify".to_string(),
            ))
        }
    }
}

struct ValidatedFixtures {
    bundle: EvaluationFixtureBundleV1,
    manifest_file_digest: FixtureManifestDigest,
}

/// The private, immutable payload opened only after a run-bound reveal capability
/// succeeds. It deliberately carries no path or key material; the enclosing
/// authority envelope binds its exact bytes and may carry an exchange signature.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct SealedHoldoutLabelSetV1 {
    schema_revision: u32,
    label_authority: HoldoutLabelAuthorityV1,
    #[serde(default)]
    agent_adjudication: Option<AgentAdjudicatedLabelProvenanceV1>,
    judgments: Vec<RelevanceJudgmentV1>,
}

struct LoadedSavedCandidates {
    saved: SavedCandidateSetV1,
    ablations: Vec<SavedCandidateAblationSummaryV1>,
}

struct LockedQualityEvaluation {
    outcome: EvalOutcomeV1,
    rationale: String,
    accepted_evidence: Option<AcceptedPr9CandidateEvidenceV1>,
}

pub fn validate_fixture_root(fixture_root: &Path) -> Result<ValidationSummary, SearchEvalError> {
    let fixtures = load_validated_fixtures(fixture_root)?;
    Ok(validation_summary(&fixtures))
}

pub fn seal_holdout_labels(
    options: &SealHoldoutOptions,
) -> Result<SealHoldoutResult, SearchEvalError> {
    let fixtures = load_validated_fixtures(&options.fixture_root)?;
    let run = options
        .run_manifest
        .as_deref()
        .map(read_json)
        .transpose()?
        .unwrap_or_else(|| fixtures.bundle.run.clone());
    if !fixtures
        .bundle
        .manifest
        .decision_owners
        .contains(&options.signed_by)
    {
        return Err(SearchEvalError::InvalidRun(
            "holdout seal signer is not a frozen decision owner".to_string(),
        ));
    }
    let bytes = fs::read(&options.labels_path).map_err(|source| SearchEvalError::Read {
        path: options.labels_path.clone(),
        source,
    })?;
    let labels: SealedHoldoutLabelSetV1 =
        serde_json::from_slice(&bytes).map_err(|source| SearchEvalError::Parse {
            path: options.labels_path.clone(),
            source,
        })?;
    validate_sealed_holdout_labels(&labels, &options.label_authority, &run, &fixtures)?;
    let store = match options.profile_root.as_deref() {
        Some(root) => HoldoutAuthorityStoreV1::open_at(root)?,
        None => HoldoutAuthorityStoreV1::open_default()?,
    };
    if let Some(provenance) = &labels.agent_adjudication {
        store.verify_agent_adjudication(provenance, options.signed_at_unix)?;
    }
    let labels_record = store.import_sealed_labels(&bytes, options.signed_at_unix)?;
    let seal_digest = HoldoutSealDigest::new(sha256_bytes(&bytes))?;
    let envelope = store.sign_envelope(HoldoutEnvelopePayloadV1 {
        schema_revision: 1,
        labels_locator: labels_record.locator.clone(),
        labels_content_digest: labels_record.content_digest.clone(),
        seal_digest: seal_digest.clone(),
        label_authority: options.label_authority,
        signed_by: options.signed_by.clone(),
        trust_root_id: options.trust_root_id.clone(),
        signed_at_unix: options.signed_at_unix,
    })?;
    Ok(SealHoldoutResult {
        seal: HoldoutSealV1 {
            locator: labels_record.locator.clone(),
            seal_digest,
            labels_content_digest: Some(labels_record.content_digest.clone()),
            label_authority: Some(options.label_authority),
            signed_envelope_digest: envelope.content_digest.clone(),
            signature_locator: envelope.locator.clone(),
            access_policy: tracedecay_domain::HoldoutAccessPolicyV1::SealedRevealRequiresReceipt,
            reveal_contract: "Locked labels may be revealed only after a run is frozen, through a least-privilege capability with a durable receipt."
                .to_string(),
            schema_revision: 1,
        },
        labels: labels_record,
        envelope,
    })
}

pub fn compare(options: &CompareOptions) -> Result<CompareResult, SearchEvalError> {
    let fixtures = load_validated_fixtures(&options.fixture_root)?;
    let run = if let Some(path) = &options.run_manifest {
        read_json(path)?
    } else {
        fixtures.bundle.run.clone()
    };
    let holdout_seal = options.holdout_seal.as_deref().map(read_json).transpose()?;
    validate_run_bindings_with_seal(&run, &fixtures, holdout_seal.as_ref())?;
    validate_single_path_component(run.run_id.as_str(), "run id")?;

    let loaded_saved_candidates = options
        .saved_candidates
        .as_deref()
        .map(|path| load_saved_candidate_ablations(path, &run, &fixtures.bundle.workload))
        .transpose()?;

    // A checked-in contract-only corpus may be elevated only by an explicitly
    // locked run carrying private saved candidates and a reveal capability.
    // Development runs still stop before touching label-bearing artifacts.
    let (outcome, rationale, blocked_on, accepted_evidence) = if run.authority
        != FixtureAuthorityV1::LockedQuality
    {
        (
            EvalOutcomeV1::Blocked,
            "authoritative locked-quality labels are unavailable; the checked-in contract-only packet cannot produce a quality conclusion"
                .to_string(),
            vec![required_holdout_artifact(
                &fixtures.bundle.manifest.holdout_seal,
            )],
            None,
        )
    } else {
        let saved_candidates = loaded_saved_candidates.as_ref().ok_or_else(|| {
            SearchEvalError::InvalidRun(
                "locked-quality comparison requires frozen PR9 saved candidates".to_string(),
            )
        })?;
        let capability_locator = options.holdout_capability.as_deref().ok_or_else(|| {
            SearchEvalError::InvalidRun(
                "locked-quality run has no holdout reveal capability".to_string(),
            )
        })?;
        let store = match options.holdout_profile_root.as_deref() {
            Some(root) => HoldoutAuthorityStoreV1::open_at(root)?,
            None => HoldoutAuthorityStoreV1::open_default()?,
        };
        let evaluation = evaluate_locked_quality(
            &store,
            capability_locator,
            &fixtures,
            &run,
            &saved_candidates.saved,
            holdout_seal
                .as_ref()
                .unwrap_or(&fixtures.bundle.manifest.holdout_seal),
            current_unix_seconds()?,
        )?;
        (
            evaluation.outcome,
            evaluation.rationale,
            Vec::new(),
            evaluation.accepted_evidence,
        )
    };

    let saved_candidate_set_digest = loaded_saved_candidates
        .as_ref()
        .map(|loaded| loaded.saved.digest.clone());
    let saved_candidate_ablations = loaded_saved_candidates
        .as_ref()
        .map(|loaded| loaded.ablations.clone())
        .unwrap_or_default();

    let revision_dir = revision_directory(&options.output_root, &run);
    let supersedes_report_digest =
        validate_append_only_revision(&options.output_root, &run, &revision_dir)?;
    let decision = if let Some(accepted) = &accepted_evidence {
        accepted.decision.clone()
    } else {
        let mut decision = DecisionRecordV1 {
            run_id: run.run_id.clone(),
            outcome,
            rationale: rationale.clone(),
            decided_by: fixtures.bundle.manifest.decision_owners[0].clone(),
            saved_candidate_set_digest,
            evidence_batches: Vec::new(),
            digest: DecisionRecordDigest::new(ZERO_DIGEST)?,
        };
        decision.digest = decision.compute_digest()?;
        decision.validate()?;
        decision.verify_digest()?;
        decision
    };

    let mut report = EvaluationReportV1 {
        schema_revision: 1,
        run_id: run.run_id.clone(),
        run_revision: run.revision,
        run_manifest_digest: run.digest.clone(),
        fixture_manifest_digest: fixtures.manifest_file_digest.clone(),
        outcome,
        rationale: rationale.clone(),
        no_access_before_lock: true,
        blocked_on: blocked_on.clone(),
        saved_candidate_ablations: saved_candidate_ablations.clone(),
        decision,
        supersedes_report_digest,
        report_digest: FixtureContentDigest::new(ZERO_DIGEST)?,
    };
    report.report_digest = report.compute_digest()?;
    report.verify_digest()?;

    let report_bytes = pretty_json(&report)?;
    let report_artifact = FixtureFileDigestV1 {
        path: "report.json".to_string(),
        byte_len: report_bytes.len() as u64,
        digest: FixtureContentDigest::new(sha256_bytes(&report_bytes))?,
    };
    let accepted_evidence_bytes = accepted_evidence.as_ref().map(pretty_json).transpose()?;
    let accepted_evidence_artifact = match accepted_evidence_bytes.as_deref() {
        Some(bytes) => Some(FixtureFileDigestV1 {
            path: "accepted-pr9-candidate-evidence.json".to_string(),
            byte_len: bytes.len() as u64,
            digest: FixtureContentDigest::new(sha256_bytes(bytes))?,
        }),
        None => None,
    };
    let evidence_index = build_evidence_index(
        &fixtures,
        &run,
        outcome,
        &blocked_on,
        report_artifact,
        accepted_evidence_artifact,
    )?;
    let evidence_index_bytes = pretty_json(&evidence_index)?;
    let run_manifest_bytes = pretty_json(&run)?;
    write_immutable_revision(
        &revision_dir,
        &report_bytes,
        &evidence_index_bytes,
        &run_manifest_bytes,
        accepted_evidence_bytes.as_deref(),
    )?;

    let required_outcome = options.required_outcome;
    Ok(CompareResult {
        command: "compare",
        outcome,
        required_outcome,
        requirement_satisfied: required_outcome.is_none_or(|required| required == outcome),
        rationale,
        run_id: run.run_id.to_string(),
        run_revision: run.revision,
        run_manifest_digest: run.digest,
        blocked_on,
        saved_candidate_ablations,
        report_path: revision_dir.join("report.json"),
        evidence_index_path: revision_dir.join("evidence-index.json"),
    })
}

fn evaluate_locked_quality(
    store: &HoldoutAuthorityStoreV1,
    capability_locator: &str,
    fixtures: &ValidatedFixtures,
    run: &RunManifestV1,
    saved_candidates: &SavedCandidateSetV1,
    holdout_seal: &HoldoutSealV1,
    now_unix: u64,
) -> Result<LockedQualityEvaluation, SearchEvalError> {
    run.validate_pre_reveal(&fixtures.bundle.manifest, &fixtures.bundle.workload)?;
    saved_candidates.validate_pr9_baseline_for_run(run, &fixtures.bundle.workload)?;
    if run.decision_expression != PR9_ACCEPTANCE_DECISION_EXPRESSION {
        return Err(SearchEvalError::InvalidRun(
            "locked PR9 run does not use the supported executable acceptance expression"
                .to_string(),
        ));
    }
    let declared_label_authority = holdout_seal.label_authority.clone().ok_or_else(|| {
        SearchEvalError::InvalidRun(
            "locked-quality seal has no declared label authority".to_string(),
        )
    })?;
    let (labels, receipt) = store.evaluate_locked_labels(
        capability_locator,
        run,
        holdout_seal,
        &fixtures.bundle.manifest.decision_owners,
        now_unix,
        |bytes| parse_sealed_holdout_labels(bytes, &declared_label_authority, run, fixtures),
    )?;
    if let Some(provenance) = &labels.agent_adjudication {
        store.verify_agent_adjudication(provenance, now_unix)?;
    }

    let acceptance_rationale = match labels.label_authority {
        HoldoutLabelAuthorityV1::Deterministic => "owner-bound deterministic sealed labels, a run-bound reveal capability, durable receipt, and frozen exact/lexical/graph candidates validated for this locked run"
            .to_string(),
        HoldoutLabelAuthorityV1::HumanAuthoritative => "owner-bound human-authoritative sealed labels, a run-bound reveal capability, durable receipt, and frozen exact/lexical/graph candidates validated for this locked run"
            .to_string(),
        HoldoutLabelAuthorityV1::AgentAdjudicated => "owner-bound user-delegated agent-adjudicated sealed labels, two independent blinded judgments or a distinct tie-break adjudication, a run-bound reveal capability, durable receipt, and frozen exact/lexical/graph candidates validated for this locked run"
            .to_string(),
    };
    let accepted_evidence =
        build_accepted_pr9_evidence(run, saved_candidates, receipt, &acceptance_rationale)?;
    store
        .validate_accepted_pr9_evidence(
            &accepted_evidence,
            run,
            &fixtures.bundle.workload,
            holdout_seal,
            &fixtures.bundle.manifest.decision_owners,
        )
        .map_err(SearchEvalError::Contract)?;
    Ok(LockedQualityEvaluation {
        outcome: EvalOutcomeV1::Accepted,
        rationale: acceptance_rationale,
        accepted_evidence: Some(accepted_evidence),
    })
}

fn parse_sealed_holdout_labels(
    bytes: &[u8],
    declared_label_authority: &HoldoutLabelAuthorityV1,
    run: &RunManifestV1,
    fixtures: &ValidatedFixtures,
) -> Result<SealedHoldoutLabelSetV1, HoldoutAuthorityError> {
    let labels = serde_json::from_slice(bytes).map_err(|error| {
        HoldoutAuthorityError::InvalidMetadata(format!(
            "sealed holdout labels are not a valid v1 packet: {error}"
        ))
    })?;
    validate_sealed_holdout_labels(&labels, declared_label_authority, run, fixtures)
        .map_err(|error| HoldoutAuthorityError::InvalidMetadata(error.to_string()))?;
    Ok(labels)
}

fn validate_sealed_holdout_labels(
    labels: &SealedHoldoutLabelSetV1,
    declared_label_authority: &HoldoutLabelAuthorityV1,
    run: &RunManifestV1,
    fixtures: &ValidatedFixtures,
) -> Result<(), EvaluationContractError> {
    if labels.schema_revision != 1 {
        return Err(EvaluationContractError::InvalidIdentity {
            field: "sealed holdout label schema revision",
        });
    }
    if &labels.label_authority != declared_label_authority {
        return Err(EvaluationContractError::HoldoutAccessViolation(
            "sealed holdout label authority differs from the immutable envelope".to_string(),
        ));
    }
    if labels.judgments.is_empty() {
        return Err(EvaluationContractError::Empty {
            field: "sealed holdout label judgments",
        });
    }
    match labels.label_authority {
        HoldoutLabelAuthorityV1::AgentAdjudicated => {
            let provenance = labels.agent_adjudication.as_ref().ok_or_else(|| {
                EvaluationContractError::CoverageViolation(
                    "agent-adjudicated labels require bound adjudication provenance".to_string(),
                )
            })?;
            if !provenance.is_sealable()? {
                return Err(EvaluationContractError::CoverageViolation(
                    "agent-adjudicated labels are not in a sealable terminal state".to_string(),
                ));
            }
            if !fixtures
                .bundle
                .manifest
                .decision_owners
                .contains(&provenance.delegated_by)
            {
                return Err(EvaluationContractError::HoldoutAccessViolation(
                    "agent adjudication was not delegated by a frozen decision owner".to_string(),
                ));
            }
            if provenance.final_label_set_digest.as_ref()
                != Some(&sealed_holdout_label_set_digest(&labels.judgments)?)
            {
                return Err(EvaluationContractError::DigestMismatch {
                    field: "agent-adjudicated final label set",
                });
            }
        }
        HoldoutLabelAuthorityV1::Deterministic | HoldoutLabelAuthorityV1::HumanAuthoritative => {
            if labels.agent_adjudication.is_some() {
                return Err(EvaluationContractError::CoverageViolation(
                    "non-agent holdout labels cannot carry agent adjudication provenance"
                        .to_string(),
                ));
            }
        }
    }

    const NO_RESULT_FAMILIES: [QueryFamilyV1; 4] = [
        QueryFamilyV1::ExpectedNoResult,
        QueryFamilyV1::FalseExactHardNegative,
        QueryFamilyV1::WrongScopeNearMatch,
        QueryFamilyV1::AuthorizationCanary,
    ];
    let execution_order: BTreeSet<_> = run.execution_order.iter().collect();
    let task_ids: BTreeSet<_> = fixtures
        .bundle
        .tasks
        .iter()
        .map(|task| &task.task_id)
        .collect();
    let mut covered_queries = BTreeSet::new();
    let mut judgment_ids = BTreeSet::new();
    let mut judgment_pairs = BTreeSet::new();
    for judgment in &labels.judgments {
        if judgment.labeler.is_empty()
            || judgment.labeler_provenance.is_empty()
            || judgment.adjudication.is_empty()
        {
            return Err(EvaluationContractError::Empty {
                field: "sealed holdout judgment provenance/adjudication",
            });
        }
        if judgment
            .valid_until_unix_micros
            .is_some_and(|until| until < judgment.valid_from_unix_micros)
        {
            return Err(EvaluationContractError::CoverageViolation(
                "sealed holdout judgment validity interval is reversed".to_string(),
            ));
        }
        if !judgment_ids.insert(&judgment.judgment_id) {
            return Err(EvaluationContractError::Duplicate {
                field: "sealed holdout judgment id",
            });
        }
        if !judgment_pairs.insert((&judgment.query_id, &judgment.document_id)) {
            return Err(EvaluationContractError::Duplicate {
                field: "sealed holdout judgment query/document",
            });
        }
        let query = fixtures
            .bundle
            .workload
            .query(&judgment.query_id)
            .ok_or_else(|| {
                EvaluationContractError::CoverageViolation(
                    "sealed holdout judgment references an unknown query".to_string(),
                )
            })?;
        if query.partition != EvalPartitionV1::SealedHoldout
            || !execution_order.contains(&judgment.query_id)
        {
            return Err(EvaluationContractError::PartitionViolation(
                "sealed holdout judgment is outside the frozen run".to_string(),
            ));
        }
        if fixtures
            .bundle
            .manifest
            .document(&judgment.document_id)
            .is_none()
        {
            return Err(EvaluationContractError::CoverageViolation(
                "sealed holdout judgment references an unknown document".to_string(),
            ));
        }
        if judgment
            .task_oracle
            .as_ref()
            .is_some_and(|task_id| !task_ids.contains(task_id))
        {
            return Err(EvaluationContractError::CoverageViolation(
                "sealed holdout judgment references an unknown task".to_string(),
            ));
        }
        if NO_RESULT_FAMILIES.contains(&query.family) && judgment.grade.is_relevant() {
            return Err(EvaluationContractError::PartitionViolation(
                "sealed holdout no-result query carries a relevant judgment".to_string(),
            ));
        }
        covered_queries.insert(&judgment.query_id);
    }
    if covered_queries != execution_order {
        return Err(EvaluationContractError::CoverageViolation(
            "sealed holdout labels must cover every frozen run query".to_string(),
        ));
    }
    Ok(())
}

pub fn sealed_holdout_label_set_digest(
    judgments: &[RelevanceJudgmentV1],
) -> Result<LabelSetDigest, EvaluationContractError> {
    #[derive(Serialize)]
    struct DigestInput<'a> {
        domain: &'static str,
        judgments: &'a [RelevanceJudgmentV1],
    }

    let bytes = serde_json::to_vec(&DigestInput {
        domain: "tracedecay.sealed-holdout-label-set.v1",
        judgments,
    })
    .map_err(|error| EvaluationContractError::CanonicalSerialization(error.to_string()))?;
    LabelSetDigest::new(sha256_bytes(&bytes))
}

fn build_accepted_pr9_evidence(
    run: &RunManifestV1,
    saved_candidates: &SavedCandidateSetV1,
    receipt: HoldoutAccessReceiptV1,
    rationale: &str,
) -> Result<AcceptedPr9CandidateEvidenceV1, SearchEvalError> {
    let mut batch = EvidenceBatchV1 {
        batch_id: EvidenceBatchId::new(format!("batch-{}-revision-{}", run.run_id, run.revision))?,
        run_id: run.run_id.clone(),
        scope: run.scope,
        workload_digest: saved_candidates.workload_digest.clone(),
        candidate_lists: saved_candidates.candidate_lists.clone(),
        holdout_receipts: vec![receipt.clone()],
        digest: EvidenceBatchDigest::new(ZERO_DIGEST)?,
    };
    batch.digest = batch.compute_digest()?;
    let mut decision = DecisionRecordV1 {
        run_id: run.run_id.clone(),
        outcome: EvalOutcomeV1::Accepted,
        rationale: rationale.to_string(),
        decided_by: receipt.signed_by,
        saved_candidate_set_digest: Some(saved_candidates.digest.clone()),
        evidence_batches: vec![batch.digest.clone()],
        digest: DecisionRecordDigest::new(ZERO_DIGEST)?,
    };
    decision.digest = decision.compute_digest()?;
    let mut accepted = AcceptedPr9CandidateEvidenceV1 {
        schema_revision: 1,
        decision,
        evidence_batches: vec![batch],
        saved_candidates: saved_candidates.clone(),
        digest: AcceptedPr9CandidateEvidenceDigest::new(ZERO_DIGEST)?,
    };
    accepted.digest = accepted.compute_digest()?;
    Ok(accepted)
}

fn load_validated_fixtures(fixture_root: &Path) -> Result<ValidatedFixtures, SearchEvalError> {
    let manifest_path = fixture_root.join("fixture-manifest-v1.json");
    let manifest: FixtureManifestV1 = read_json(&manifest_path)?;
    let holdout_seal: HoldoutSealV1 = read_json(&fixture_root.join("locked-judgments-v1.json"))?;
    if holdout_seal != manifest.holdout_seal {
        return Err(SearchEvalError::InvalidRun(
            "locked-judgments-v1.json differs from the fixture manifest seal".to_string(),
        ));
    }
    if fixture_root
        .join("holdout/judgments-holdout-v1.jsonl")
        .exists()
    {
        return Err(SearchEvalError::InvalidRun(
            "holdout labels must not be present in the checked-in fixture root".to_string(),
        ));
    }

    verify_manifest_files(fixture_root, &manifest)?;
    let queries = read_jsonl::<EvalQueryV1>(&fixture_root.join("queries-v1.jsonl"))?;
    let mut workload = QueryWorkloadV1 {
        revision: 1,
        queries,
        digest: WorkloadDigest::new(ZERO_DIGEST)?,
    };
    workload.digest = workload.compute_digest()?;
    let judgments =
        read_jsonl::<RelevanceJudgmentV1>(&fixture_root.join("judgments-development-v1.jsonl"))?;
    let mut development_labels = LabelSetV1 {
        label_set_id: LabelSetId::new("labels-development-v1")?,
        revision: 1,
        partition: EvalPartitionV1::Development,
        judgments,
        digest: LabelSetDigest::new(ZERO_DIGEST)?,
    };
    development_labels.digest = development_labels.compute_digest()?;

    let run: RunManifestV1 = read_json(&fixture_root.join("run-contract-v1.json"))?;
    let evidence_index: EvidenceIndexV1 = read_json(&fixture_root.join("evidence-index.json"))?;
    let bundle = EvaluationFixtureBundleV1 {
        manifest,
        workload,
        snapshots: read_jsonl(&fixture_root.join("snapshots-v1.jsonl"))?,
        temporal_events: read_jsonl::<TemporalEventV1>(
            &fixture_root.join("temporal-events-v1.jsonl"),
        )?,
        context_spans: read_jsonl::<ContextSpanV1>(&fixture_root.join("context-spans-v1.jsonl"))?,
        tasks: read_jsonl::<EvaluationTaskV1>(&fixture_root.join("tasks-v1.jsonl"))?,
        authorization_canaries: read_jsonl::<AuthorizationCanaryV1>(
            &fixture_root.join("authorization-canaries-v1.jsonl"),
        )?,
        exact_admission_oracles: read_jsonl::<ExactAdmissionOracleV1>(
            &fixture_root.join("exact-admission-oracles-v1.jsonl"),
        )?,
        contamination_partitions: read_json(
            &fixture_root.join("contamination-partitions-v1.json"),
        )?,
        development_labels,
        run,
        evidence_index,
    };
    bundle.validate()?;
    validate_no_result_labels(&bundle)?;
    validate_partition_counts(&bundle)?;

    let manifest_file_digest =
        FixtureManifestDigest::new(sha256_file(&manifest_path)?.0.as_str().to_string())?;
    validate_run_bindings_raw(&bundle.run, &bundle.manifest, &manifest_file_digest, None)?;
    Ok(ValidatedFixtures {
        bundle,
        manifest_file_digest,
    })
}

fn validation_summary(fixtures: &ValidatedFixtures) -> ValidationSummary {
    let development_query_count = fixtures.bundle.workload.development_queries().count();
    let sealed_holdout_query_count = fixtures.bundle.workload.sealed_holdout_queries().count();
    ValidationSummary {
        command: "validate",
        status: "valid",
        authority: fixtures.bundle.manifest.authority,
        fixture_manifest_digest: fixtures.manifest_file_digest.clone(),
        run_manifest_digest: fixtures.bundle.run.digest.clone(),
        workload_digest: fixtures.bundle.workload.digest.clone(),
        development_label_digest: fixtures.bundle.development_labels.digest.clone(),
        holdout_seal_digest: fixtures.bundle.manifest.holdout_seal.seal_digest.clone(),
        query_count: fixtures.bundle.workload.queries.len(),
        development_query_count,
        sealed_holdout_query_count,
    }
}

fn validate_run_bindings_with_seal(
    run: &RunManifestV1,
    fixtures: &ValidatedFixtures,
    holdout_seal: Option<&HoldoutSealV1>,
) -> Result<(), SearchEvalError> {
    run.validate_against_workload(&fixtures.bundle.workload)?;
    let computed = run.compute_digest()?;
    if run.digest != computed {
        return Err(SearchEvalError::InvalidRun(format!(
            "run manifest digest must be {computed}"
        )));
    }
    validate_run_bindings_raw(
        run,
        &fixtures.bundle.manifest,
        &fixtures.manifest_file_digest,
        holdout_seal,
    )
}

fn validate_run_bindings_raw(
    run: &RunManifestV1,
    manifest: &FixtureManifestV1,
    manifest_file_digest: &FixtureManifestDigest,
    holdout_seal: Option<&HoldoutSealV1>,
) -> Result<(), SearchEvalError> {
    if &run.fixture_manifest_digest != manifest_file_digest {
        return Err(SearchEvalError::InvalidRun(
            "run manifest does not bind the fixture-manifest bytes".to_string(),
        ));
    }
    let expected_seal = holdout_seal.unwrap_or(&manifest.holdout_seal);
    if run.holdout_seal_digest != expected_seal.seal_digest {
        return Err(SearchEvalError::InvalidRun(
            "run manifest does not bind the holdout seal".to_string(),
        ));
    }
    if run.artifact_files != manifest.artifact_files {
        return Err(SearchEvalError::InvalidRun(
            "run manifest artifact list differs from the fixture manifest".to_string(),
        ));
    }
    let workload = manifest_artifact(manifest, "queries-v1.jsonl")?;
    let labels = manifest_artifact(manifest, "judgments-development-v1.jsonl")?;
    if run.workload_file_digest != workload.digest
        || run.development_label_file_digest != labels.digest
    {
        return Err(SearchEvalError::InvalidRun(
            "run manifest workload or development-label digest drifted".to_string(),
        ));
    }
    Ok(())
}

fn manifest_artifact<'a>(
    manifest: &'a FixtureManifestV1,
    path: &str,
) -> Result<&'a FixtureFileDigestV1, SearchEvalError> {
    manifest
        .artifact_files
        .iter()
        .find(|artifact| artifact.path == path)
        .ok_or_else(|| {
            SearchEvalError::InvalidRun(format!("fixture manifest omits required {path}"))
        })
}

fn verify_manifest_files(
    fixture_root: &Path,
    manifest: &FixtureManifestV1,
) -> Result<(), SearchEvalError> {
    manifest.validate()?;
    for document in &manifest.corpus {
        let (digest, byte_len) = sha256_file(&fixture_root.join(&document.snapshot_path))?;
        if digest != document.content_digest || byte_len != document.byte_len {
            return Err(SearchEvalError::InvalidRun(format!(
                "corpus artifact {} drifted from its manifest digest or length",
                document.snapshot_path
            )));
        }
    }
    for artifact in &manifest.artifact_files {
        let (digest, byte_len) = sha256_file(&fixture_root.join(&artifact.path))?;
        if digest != artifact.digest || byte_len != artifact.byte_len {
            return Err(SearchEvalError::InvalidRun(format!(
                "fixture artifact {} drifted from its manifest digest or length",
                artifact.path
            )));
        }
    }
    Ok(())
}

fn validate_partition_counts(bundle: &EvaluationFixtureBundleV1) -> Result<(), SearchEvalError> {
    for partition in &bundle.manifest.partitions {
        let actual = bundle
            .workload
            .queries
            .iter()
            .filter(|query| query.partition == partition.partition)
            .count();
        if actual != partition.query_count as usize {
            return Err(SearchEvalError::InvalidRun(format!(
                "{} partition declares {} queries but contains {actual}",
                partition.partition.as_str(),
                partition.query_count
            )));
        }
    }
    Ok(())
}

fn validate_no_result_labels(bundle: &EvaluationFixtureBundleV1) -> Result<(), SearchEvalError> {
    const NO_RESULT_FAMILIES: [QueryFamilyV1; 4] = [
        QueryFamilyV1::ExpectedNoResult,
        QueryFamilyV1::FalseExactHardNegative,
        QueryFamilyV1::WrongScopeNearMatch,
        QueryFamilyV1::AuthorizationCanary,
    ];
    for judgment in &bundle.development_labels.judgments {
        let query = bundle.workload.query(&judgment.query_id).ok_or_else(|| {
            SearchEvalError::InvalidRun(format!(
                "judgment {} references an unknown query",
                judgment.judgment_id
            ))
        })?;
        if NO_RESULT_FAMILIES.contains(&query.family) && judgment.grade.is_relevant() {
            return Err(SearchEvalError::InvalidRun(format!(
                "no-result query {} carries a relevant judgment",
                query.query_id
            )));
        }
    }
    Ok(())
}

fn required_holdout_artifact(seal: &HoldoutSealV1) -> RequiredExternalArtifactV1 {
    RequiredExternalArtifactV1 {
        kind: "sealed_holdout_labels".to_string(),
        locator: seal.locator.clone(),
        digest: seal.seal_digest.clone(),
        signature_locator: seal.signature_locator.clone(),
        reason: "an owner-bound, independently adjudicated locked-quality label artifact and reveal capability are required"
            .to_string(),
    }
}

fn load_saved_candidate_ablations(
    path: &Path,
    run: &RunManifestV1,
    workload: &QueryWorkloadV1,
) -> Result<LoadedSavedCandidates, SearchEvalError> {
    let saved: SavedCandidateSetV1 = read_json(path)?;
    saved.validate_for_run(run, workload)?;
    let lanes: BTreeSet<_> = saved
        .candidate_lists
        .iter()
        .map(|list| list.lane.clone())
        .collect();
    let ablations = lanes
        .into_iter()
        .map(|lane| {
            let retained = saved.ablate_lanes(std::slice::from_ref(&lane))?;
            Ok(SavedCandidateAblationSummaryV1 {
                disabled_lane: lane.to_string(),
                retained_candidate_lists: retained.len(),
                retained_candidates: retained.iter().map(|list| list.candidates.len()).sum(),
            })
        })
        .collect::<Result<Vec<_>, EvaluationContractError>>()
        .map_err(SearchEvalError::Contract)?;
    Ok(LoadedSavedCandidates { saved, ablations })
}

fn build_evidence_index(
    fixtures: &ValidatedFixtures,
    run: &RunManifestV1,
    outcome: EvalOutcomeV1,
    blocked_on: &[RequiredExternalArtifactV1],
    report_artifact: FixtureFileDigestV1,
    accepted_evidence_artifact: Option<FixtureFileDigestV1>,
) -> Result<EvidenceIndexV1, SearchEvalError> {
    let mut immutable_result_anchors = vec![report_artifact.digest.to_string()];
    immutable_result_anchors.extend(blocked_on.iter().map(|artifact| artifact.locator.clone()));
    let mut aggregate_artifacts = vec![report_artifact];
    if let Some(accepted_evidence_artifact) = accepted_evidence_artifact {
        immutable_result_anchors.push(accepted_evidence_artifact.digest.to_string());
        aggregate_artifacts.push(accepted_evidence_artifact);
    }
    let entry = EvidenceIndexEntryV1 {
        claim_id: EvidenceClaimId::new(format!(
            "claim-{}-revision-{}-outcome",
            run.run_id, run.revision
        ))?,
        claim: format!(
            "Run {} revision {} ended with terminal outcome {}.",
            run.run_id,
            run.revision,
            outcome.as_str()
        ),
        fixture_artifacts: std::iter::once("fixture-manifest-v1.json".to_string())
            .chain(
                fixtures
                    .bundle
                    .manifest
                    .artifact_files
                    .iter()
                    .map(|artifact| artifact.path.clone()),
            )
            .chain([
                "locked-judgments-v1.json".to_string(),
                "run-manifest.json".to_string(),
            ])
            .collect(),
        run_manifest_digest: run.digest.clone(),
        aggregate_artifacts,
        immutable_result_anchors,
        acceptance_authority: outcome == EvalOutcomeV1::Accepted,
    };
    let mut index = EvidenceIndexV1 {
        index_id: EvidenceIndexId::new(format!(
            "evidence-index-{}-revision-{}",
            run.run_id, run.revision
        ))?,
        revision: run.revision,
        authority: run.authority,
        entries: vec![entry],
        digest: EvidenceIndexDigest::new(ZERO_DIGEST)?,
    };
    index.digest = index.compute_digest()?;
    index.validate()?;
    index.verify_digest()?;
    Ok(index)
}

fn revision_directory(output_root: &Path, run: &RunManifestV1) -> PathBuf {
    output_root
        .join(run.run_id.as_str())
        .join(format!("revision-{:08}", run.revision))
}

fn validate_single_path_component(value: &str, field: &str) -> Result<(), SearchEvalError> {
    if value == "."
        || value == ".."
        || value
            .chars()
            .any(|character| matches!(character, '/' | '\\'))
    {
        return Err(SearchEvalError::InvalidRun(format!(
            "{field} must be a single filesystem component"
        )));
    }
    Ok(())
}

fn validate_append_only_revision(
    output_root: &Path,
    run: &RunManifestV1,
    revision_dir: &Path,
) -> Result<Option<FixtureContentDigest>, SearchEvalError> {
    if revision_dir.exists() {
        return Err(SearchEvalError::ImmutableOutput(format!(
            "{} already exists and cannot be overwritten",
            revision_dir.display()
        )));
    }
    let run_root = output_root.join(run.run_id.as_str());
    if run.revision == 1 {
        if run_root.exists()
            && fs::read_dir(&run_root)
                .map_err(|source| SearchEvalError::Read {
                    path: run_root.clone(),
                    source,
                })?
                .next()
                .is_some()
        {
            return Err(SearchEvalError::ImmutableOutput(format!(
                "{} already contains append-only run revisions",
                run_root.display()
            )));
        }
        return Ok(None);
    }
    let previous_path = run_root
        .join(format!("revision-{:08}", run.revision - 1))
        .join("report.json");
    let previous: EvaluationReportV1 = read_json(&previous_path)?;
    previous.verify_digest()?;
    if previous.run_id != run.run_id || previous.run_revision + 1 != run.revision {
        return Err(SearchEvalError::ImmutableOutput(
            "previous report does not form a contiguous revision chain".to_string(),
        ));
    }
    Ok(Some(previous.report_digest))
}

fn write_immutable_revision(
    revision_dir: &Path,
    report: &[u8],
    evidence_index: &[u8],
    run_manifest: &[u8],
    accepted_evidence: Option<&[u8]>,
) -> Result<(), SearchEvalError> {
    let parent = revision_dir.parent().ok_or_else(|| {
        SearchEvalError::ImmutableOutput("revision output has no parent".to_string())
    })?;
    fs::create_dir_all(parent).map_err(|source| SearchEvalError::Write {
        path: parent.to_path_buf(),
        source,
    })?;
    let file_name = revision_dir
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            SearchEvalError::ImmutableOutput("revision output name is not UTF-8".to_string())
        })?;
    let pending = parent.join(format!("{file_name}.pending-{}", std::process::id()));
    if pending.exists() {
        return Err(SearchEvalError::ImmutableOutput(format!(
            "pending output {} already exists",
            pending.display()
        )));
    }
    fs::create_dir(&pending).map_err(|source| SearchEvalError::Write {
        path: pending.clone(),
        source,
    })?;
    let write_result = (|| {
        write_synced(&pending.join("report.json"), report)?;
        write_synced(&pending.join("evidence-index.json"), evidence_index)?;
        write_synced(&pending.join("run-manifest.json"), run_manifest)?;
        if let Some(accepted_evidence) = accepted_evidence {
            write_synced(
                &pending.join("accepted-pr9-candidate-evidence.json"),
                accepted_evidence,
            )?;
        }
        fs::rename(&pending, revision_dir).map_err(|source| SearchEvalError::Write {
            path: revision_dir.to_path_buf(),
            source,
        })
    })();
    if write_result.is_err() {
        let _ = fs::remove_dir_all(&pending);
    }
    write_result
}

fn write_synced(path: &Path, bytes: &[u8]) -> Result<(), SearchEvalError> {
    let mut file = File::create(path).map_err(|source| SearchEvalError::Write {
        path: path.to_path_buf(),
        source,
    })?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|source| SearchEvalError::Write {
            path: path.to_path_buf(),
            source,
        })
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, SearchEvalError> {
    let bytes = fs::read(path).map_err(|source| SearchEvalError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    serde_json::from_slice(&bytes).map_err(|source| SearchEvalError::Parse {
        path: path.to_path_buf(),
        source,
    })
}

fn read_jsonl<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Vec<T>, SearchEvalError> {
    let text = fs::read_to_string(path).map_err(|source| SearchEvalError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str(line).map_err(|source| SearchEvalError::Parse {
                path: path.to_path_buf(),
                source,
            })
        })
        .collect()
}

fn pretty_json<T: Serialize>(value: &T) -> Result<Vec<u8>, SearchEvalError> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(SearchEvalError::Serialize)?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn sha256_file(path: &Path) -> Result<(FixtureContentDigest, u64), SearchEvalError> {
    let bytes = fs::read(path).map_err(|source| SearchEvalError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let digest = FixtureContentDigest::new(sha256_bytes(&bytes))?;
    Ok((digest, bytes.len() as u64))
}

fn sha256_bytes(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity("sha256:".len() + digest.len() * 2);
    encoded.push_str("sha256:");
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut encoded, "{byte:02x}");
    }
    encoded
}

fn current_unix_seconds() -> Result<u64, SearchEvalError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|error| {
            SearchEvalError::InvalidRun(format!("system clock precedes epoch: {error}"))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search_eval::holdout::{
        AgentDelegationPayloadV1, AgentJudgmentArtifactV1, DecisionOwnerKeySpecV1,
        HoldoutEnvelopePayloadV1,
    };
    use tempfile::tempdir;
    use tracedecay_domain::{
        AgentAdjudicationStateV1, CandidateListV1, EvalRunScopeV1, HoldoutLabelAuthorityV1,
        HoldoutRevealCapabilityV1, JudgmentId, LabelEvidenceRoleV1, RelevanceGradeV1,
        RetrieverLaneId, RunId, SavedCandidateSetDigest,
    };

    #[test]
    fn locked_supported_label_authorities_produce_receipted_accepted_evidence() {
        for authority in [
            HoldoutLabelAuthorityV1::HumanAuthoritative,
            HoldoutLabelAuthorityV1::AgentAdjudicated,
        ] {
            assert_locked_authority_accepted(authority);
        }
    }

    fn assert_locked_authority_accepted(label_authority: HoldoutLabelAuthorityV1) {
        let fixture_root =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/search_quality");
        let mut fixtures = load_validated_fixtures(&fixture_root).unwrap();
        let now_unix = current_unix_seconds().unwrap();
        let profile = tempdir().unwrap();
        let store = HoldoutAuthorityStoreV1::open_at(profile.path()).unwrap();
        let owner = fixtures.bundle.manifest.decision_owners[0].clone();
        let trust_root_id = "test-root-v1";

        store
            .generate_decision_owner_key(
                DecisionOwnerKeySpecV1 {
                    owner_id: owner.clone(),
                    trust_root_id: trust_root_id.to_string(),
                    not_before_unix: now_unix.saturating_sub(1),
                    not_after_unix: now_unix + 60,
                    rotation_epoch: 1,
                },
                now_unix,
            )
            .unwrap();

        let mut run = fixtures.bundle.run.clone();
        run.run_id = RunId::new("run-test-locked-acceptance-v1").unwrap();
        run.scope = EvalRunScopeV1::Locked;
        run.authority = FixtureAuthorityV1::LockedQuality;
        run.decision_expression = PR9_ACCEPTANCE_DECISION_EXPRESSION.to_string();
        run.execution_order = fixtures
            .bundle
            .workload
            .sealed_holdout_queries()
            .map(|query| query.query_id.clone())
            .collect();

        let judgments = run
            .execution_order
            .iter()
            .enumerate()
            .map(|(index, query_id)| RelevanceJudgmentV1 {
                judgment_id: JudgmentId::new(format!("judgment-locked-{index:03}")).unwrap(),
                query_id: query_id.clone(),
                document_id: fixtures.bundle.manifest.corpus[0].document_id.clone(),
                symbol: None,
                grade: RelevanceGradeV1::NotRelevant,
                evidence_role: LabelEvidenceRoleV1::Primary,
                valid_from_unix_micros: 1,
                valid_until_unix_micros: None,
                supersedes_judgment_id: None,
                logical_copy_group: None,
                forbidden_anchor_ids: Vec::new(),
                abstention_oracle: false,
                task_oracle: None,
                labeler: "test-human-labeler".to_string(),
                labeler_provenance: "test-independent-double-label".to_string(),
                adjudication: "test-adjudicated".to_string(),
                correction_revision: 0,
                note: None,
            })
            .collect::<Vec<_>>();
        let final_label_set_digest = sealed_holdout_label_set_digest(&judgments).unwrap();
        let agent_adjudication = (label_authority == HoldoutLabelAuthorityV1::AgentAdjudicated)
            .then(|| {
                let packet = store
                    .import_blinded_packet(b"{\"schema_revision\":1,\"queries\":[]}", now_unix)
                    .unwrap();
                let delegation = store
                    .sign_agent_delegation(
                        AgentDelegationPayloadV1 {
                            schema_revision: 1,
                            delegated_by: owner.clone(),
                            blinded_packet_digest: packet.content_digest.clone(),
                            signed_at_unix: now_unix,
                        },
                        trust_root_id,
                    )
                    .unwrap();
                let import_judgment = |id: &str, instance: &str| {
                    let artifact = AgentJudgmentArtifactV1 {
                        schema_revision: 1,
                        independent_judgment_id: JudgmentId::new(id).unwrap(),
                        adjudicator_instance_id: instance.to_string(),
                        adjudicator_model: "sol".to_string(),
                        adjudicator_version: "gpt-5.6-sol".to_string(),
                        judged_at_unix: now_unix,
                        blinded_packet_digest: packet.content_digest.clone(),
                        label_set_digest: final_label_set_digest.clone(),
                        judgments: judgments.clone(),
                    };
                    let record = store.import_agent_judgment(&artifact).unwrap();
                    artifact.provenance(record.content_digest)
                };
                AgentAdjudicatedLabelProvenanceV1 {
                    delegated_by: owner.clone(),
                    blinded_packet_digest: packet.content_digest.clone(),
                    signed_delegation_digest: delegation.content_digest,
                    final_label_set_digest: Some(final_label_set_digest.clone()),
                    state: AgentAdjudicationStateV1::Agreement,
                    independent_judgments: vec![
                        import_judgment("judgment-agent-a", "sol-instance-a"),
                        import_judgment("judgment-agent-b", "sol-instance-b"),
                    ],
                    separate_adjudication: None,
                }
            });
        let labels = SealedHoldoutLabelSetV1 {
            schema_revision: 1,
            label_authority,
            agent_adjudication,
            judgments,
        };
        let labels_bytes = serde_json::to_vec(&labels).unwrap();
        let labels_record = store.import_sealed_labels(&labels_bytes, now_unix).unwrap();
        let seal_digest = HoldoutSealDigest::new(sha256_bytes(&labels_bytes)).unwrap();
        let envelope = store
            .sign_envelope(HoldoutEnvelopePayloadV1 {
                schema_revision: 1,
                labels_locator: labels_record.locator.clone(),
                labels_content_digest: labels_record.content_digest.clone(),
                seal_digest: seal_digest.clone(),
                label_authority,
                signed_by: owner.clone(),
                trust_root_id: trust_root_id.to_string(),
                signed_at_unix: now_unix,
            })
            .unwrap();
        let seal = HoldoutSealV1 {
            locator: labels_record.locator.clone(),
            seal_digest: seal_digest.clone(),
            labels_content_digest: Some(labels_record.content_digest),
            label_authority: Some(label_authority),
            signed_envelope_digest: envelope.content_digest,
            signature_locator: envelope.locator,
            access_policy: tracedecay_domain::HoldoutAccessPolicyV1::SealedRevealRequiresReceipt,
            reveal_contract: "test signed run-bound reveal".to_string(),
            schema_revision: 1,
        };

        fixtures.bundle.manifest.authority = FixtureAuthorityV1::LockedQuality;
        fixtures.bundle.manifest.holdout_seal = seal.clone();
        run.holdout_seal_digest = seal_digest;
        run.digest = run.compute_digest().unwrap();
        fixtures.bundle.run = run.clone();

        let capability = store
            .sign_reveal_capability(
                HoldoutRevealCapabilityV1 {
                    schema_revision: 1,
                    labels_locator: seal.locator.clone(),
                    envelope_locator: seal.signature_locator.clone(),
                    seal_digest: seal.seal_digest.clone(),
                    run_id: run.run_id.clone(),
                    run_manifest_digest: run.digest.clone(),
                    revealed_by: owner,
                    operation: "evaluate_locked_quality_v1".to_string(),
                    not_before_unix: now_unix.saturating_sub(1),
                    expires_at_unix: now_unix + 30,
                },
                trust_root_id,
                now_unix,
            )
            .unwrap();
        let candidate_lists = run
            .execution_order
            .iter()
            .flat_map(|query_id| {
                ["exact", "lexical", "graph"].map(|lane| CandidateListV1 {
                    query_id: query_id.clone(),
                    lane: RetrieverLaneId::new(lane).unwrap(),
                    candidates: Vec::new(),
                })
            })
            .collect();
        let mut saved_candidates = SavedCandidateSetV1 {
            schema_revision: 1,
            run_id: run.run_id.clone(),
            run_manifest_digest: run.digest.clone(),
            scope: EvalRunScopeV1::Locked,
            workload_digest: fixtures.bundle.workload.digest.clone(),
            candidate_lists,
            digest: SavedCandidateSetDigest::new(ZERO_DIGEST).unwrap(),
        };
        saved_candidates.digest = saved_candidates.compute_digest().unwrap();

        let evaluation = evaluate_locked_quality(
            &store,
            &capability.locator,
            &fixtures,
            &run,
            &saved_candidates,
            &fixtures.bundle.manifest.holdout_seal,
            now_unix,
        )
        .unwrap();
        assert_eq!(evaluation.outcome, EvalOutcomeV1::Accepted);
        let accepted = evaluation.accepted_evidence.as_ref().unwrap();
        assert_eq!(accepted.evidence_batches.len(), 1);
        assert_eq!(accepted.evidence_batches[0].holdout_receipts.len(), 1);
        store
            .validate_accepted_pr9_evidence(
                accepted,
                &run,
                &fixtures.bundle.workload,
                &seal,
                &fixtures.bundle.manifest.decision_owners,
            )
            .unwrap();
    }
}
