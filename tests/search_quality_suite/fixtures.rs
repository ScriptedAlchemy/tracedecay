//! Loading and integrity verification of the committed search-quality
//! fixtures under `tests/fixtures/search_quality/`.
//!
//! Every loader validates against the typed schemas
//! (`crates/tracedecay-domain/src/evaluation.rs`) and every digest is
//! recomputed from the committed bytes at load time: a fixture whose
//! recorded digests disagree with its own files fails loudly instead of
//! silently drifting (the same recomputed-metrics convention as the context
//! and redundancy evals).

use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::evaluation::{
    EvalPartitionV1, EvalQueryV1, EvaluationContractError, EvaluationFixtureBundleV1,
    FixtureContentDigest, FixtureManifestV1, HoldoutSealV1, LabelSetId, LabelSetV1, QueryFamilyV1,
    QueryWorkloadV1, RelevanceJudgmentV1, WorkloadDigest,
};

/// Absolute path of the committed fixture root.
pub(crate) fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/search_quality")
}

pub(crate) fn corpus_root() -> PathBuf {
    fixture_root().join("corpus")
}

/// Algorithm-tagged (`sha256:<hex>`) digest of raw bytes.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity("sha256:".len() + digest.len() * 2);
    encoded.push_str("sha256:");
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut encoded, "{byte:02x}").expect("write hex");
    }
    encoded
}

pub(crate) fn sha256_file(path: &Path) -> (FixtureContentDigest, u64) {
    let bytes = fs::read(path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
    let digest = FixtureContentDigest::new(sha256_hex(&bytes)).expect("valid sha256 digest");
    (digest, bytes.len() as u64)
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> T {
    let text =
        fs::read_to_string(path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|err| panic!("parse {}: {err}", path.display()))
}

fn read_jsonl<T: serde::de::DeserializeOwned>(path: &Path) -> Vec<T> {
    let text =
        fs::read_to_string(path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .enumerate()
        .map(|(index, line)| {
            serde_json::from_str(line)
                .unwrap_or_else(|err| panic!("parse {} line {}: {err}", path.display(), index + 1))
        })
        .collect()
}

/// Loads and validates the frozen fixture manifest.
pub(crate) fn load_manifest() -> FixtureManifestV1 {
    let manifest: FixtureManifestV1 = read_json(&fixture_root().join("fixture-manifest-v1.json"));
    manifest.validate().expect("fixture manifest validates");
    manifest
}

/// Recomputes every corpus snapshot digest from the committed bytes and
/// compares it (and the byte length) against the manifest.
pub(crate) fn verify_corpus_digests(manifest: &FixtureManifestV1) {
    for document in &manifest.corpus {
        let path = fixture_root().join(&document.snapshot_path);
        let (digest, byte_len) = sha256_file(&path);
        assert_eq!(
            digest, document.content_digest,
            "corpus snapshot {} drifted from its manifest digest",
            document.snapshot_path
        );
        assert_eq!(
            byte_len, document.byte_len,
            "corpus snapshot {} byte length drifted",
            document.snapshot_path
        );
    }
}

/// Recomputes the workload and development-label artifact digests.
pub(crate) fn verify_artifact_digests(manifest: &FixtureManifestV1) {
    for artifact in &manifest.artifact_files {
        let path = fixture_root().join(&artifact.path);
        let (digest, byte_len) = sha256_file(&path);
        assert_eq!(
            digest, artifact.digest,
            "fixture artifact {} drifted from its manifest digest",
            artifact.path
        );
        assert_eq!(
            byte_len, artifact.byte_len,
            "fixture artifact {} byte length drifted",
            artifact.path
        );
    }
}

/// Parses `queries-v1.jsonl` into the typed workload, computes its canonical
/// digest, and validates it.
pub(crate) fn load_workload() -> QueryWorkloadV1 {
    let queries: Vec<EvalQueryV1> = read_jsonl(&fixture_root().join("queries-v1.jsonl"));
    let mut workload = QueryWorkloadV1 {
        revision: 1,
        queries,
        digest: WorkloadDigest::new(
            "sha256:0000000000000000000000000000000000000000000000000000000000000000",
        )
        .unwrap(),
    };
    workload.digest = workload
        .compute_digest()
        .expect("workload digest computable");
    workload.validate().expect("workload validates");
    workload.verify_digest().expect("workload digest verifies");
    workload
}

/// Parses `judgments-development-v1.jsonl` into the typed development label
/// set. The sealed holdout labels are never parsed by this harness.
pub(crate) fn load_development_labels() -> LabelSetV1 {
    let judgments: Vec<RelevanceJudgmentV1> =
        read_jsonl(&fixture_root().join("judgments-development-v1.jsonl"));
    let mut labels = LabelSetV1 {
        label_set_id: LabelSetId::new("labels-development-v1").unwrap(),
        revision: 1,
        partition: EvalPartitionV1::Development,
        judgments,
        digest: crate::evaluation::LabelSetDigest::new(
            "sha256:0000000000000000000000000000000000000000000000000000000000000000",
        )
        .unwrap(),
    };
    labels.digest = labels.compute_digest().expect("label digest computable");
    labels.validate().expect("development labels validate");
    labels.verify_digest().expect("label digest verifies");
    labels
}

/// Loads the sealed holdout locator (`locked-judgments-v1.json`) and checks
/// it agrees with the manifest's seal.
pub(crate) fn load_holdout_seal(manifest: &FixtureManifestV1) -> HoldoutSealV1 {
    let seal: HoldoutSealV1 = read_json(&fixture_root().join("locked-judgments-v1.json"));
    seal.validate().expect("holdout seal validates");
    assert_eq!(
        seal, manifest.holdout_seal,
        "the committed holdout locator must match the manifest seal"
    );
    seal
}

/// Loads every checked-in Plan 15 artifact into one pure cross-validation
/// bundle. No retrieval is executed and the sealed holdout locator is never
/// opened.
pub(crate) fn load_fixture_bundle() -> EvaluationFixtureBundleV1 {
    let manifest = load_manifest();
    let _holdout_seal = load_holdout_seal(&manifest);
    let workload = load_workload();
    let development_labels = load_development_labels();
    let snapshots = read_jsonl(&fixture_root().join("snapshots-v1.jsonl"));
    let temporal_events = read_jsonl(&fixture_root().join("temporal-events-v1.jsonl"));
    let context_spans = read_jsonl(&fixture_root().join("context-spans-v1.jsonl"));
    let tasks = read_jsonl(&fixture_root().join("tasks-v1.jsonl"));
    let authorization_canaries =
        read_jsonl(&fixture_root().join("authorization-canaries-v1.jsonl"));
    let exact_admission_oracles =
        read_jsonl(&fixture_root().join("exact-admission-oracles-v1.jsonl"));
    let contamination_partitions =
        read_json(&fixture_root().join("contamination-partitions-v1.json"));
    let run = read_json(&fixture_root().join("run-contract-v1.json"));
    let evidence_index = read_json(&fixture_root().join("evidence-index.json"));

    EvaluationFixtureBundleV1 {
        manifest,
        workload,
        snapshots,
        temporal_events,
        context_spans,
        tasks,
        authorization_canaries,
        exact_admission_oracles,
        contamination_partitions,
        development_labels,
        run,
        evidence_index,
    }
}

/// Recomputes all raw-byte fixture digests and both canonical runtime
/// artifact digests.
pub(crate) fn verify_fixture_bundle_digests(bundle: &EvaluationFixtureBundleV1) {
    verify_corpus_digests(&bundle.manifest);
    verify_artifact_digests(&bundle.manifest);
    let (manifest_digest, _) = sha256_file(&fixture_root().join("fixture-manifest-v1.json"));
    assert_eq!(
        manifest_digest.as_str(),
        bundle.run.fixture_manifest_digest.as_str(),
        "run manifest does not pin the committed fixture manifest bytes"
    );
    let workload_artifact = bundle
        .manifest
        .artifact_files
        .iter()
        .find(|artifact| artifact.path == "queries-v1.jsonl")
        .expect("manifest declares queries-v1.jsonl");
    let labels_artifact = bundle
        .manifest
        .artifact_files
        .iter()
        .find(|artifact| artifact.path == "judgments-development-v1.jsonl")
        .expect("manifest declares judgments-development-v1.jsonl");
    assert_eq!(
        bundle.run.workload_file_digest, workload_artifact.digest,
        "run workload digest differs from the fixture manifest"
    );
    assert_eq!(
        bundle.run.development_label_file_digest, labels_artifact.digest,
        "run development-label digest differs from the fixture manifest"
    );
    assert_eq!(
        bundle.run.artifact_files, bundle.manifest.artifact_files,
        "run artifact list differs from the fixture manifest"
    );
    assert_eq!(
        bundle.run.compute_digest().expect("run digest computable"),
        bundle.run.digest,
        "run manifest canonical digest drifted"
    );
    assert_eq!(
        bundle
            .evidence_index
            .compute_digest()
            .expect("evidence-index digest computable"),
        bundle.evidence_index.digest,
        "evidence-index canonical digest drifted"
    );
}

/// Families whose labels must declare no relevant document.
const NO_RESULT_FAMILIES: [QueryFamilyV1; 4] = [
    QueryFamilyV1::ExpectedNoResult,
    QueryFamilyV1::FalseExactHardNegative,
    QueryFamilyV1::WrongScopeNearMatch,
    QueryFamilyV1::AuthorizationCanary,
];

/// Cross-validates the development labels against the workload and manifest:
/// every judgment references a development query and a corpus document, and
/// no-result families carry no relevant grades.
pub(crate) fn validate_labels_against_workload(
    labels: &LabelSetV1,
    workload: &QueryWorkloadV1,
    manifest: &FixtureManifestV1,
) -> Result<(), EvaluationContractError> {
    for judgment in &labels.judgments {
        let query = workload.query(&judgment.query_id).ok_or_else(|| {
            EvaluationContractError::CoverageViolation(format!(
                "judgment references unknown query {}",
                judgment.query_id
            ))
        })?;
        if query.partition != EvalPartitionV1::Development {
            return Err(EvaluationContractError::PartitionViolation(format!(
                "development label set judges {} query {}",
                query.partition.as_str(),
                query.query_id
            )));
        }
        if manifest.document(&judgment.document_id).is_none() {
            return Err(EvaluationContractError::CoverageViolation(format!(
                "judgment references unknown corpus document {}",
                judgment.document_id
            )));
        }
        if NO_RESULT_FAMILIES.contains(&query.family) && judgment.grade.is_relevant() {
            return Err(EvaluationContractError::PartitionViolation(format!(
                "no-result family query {} must not carry a relevant judgment",
                query.query_id
            )));
        }
    }
    Ok(())
}

/// Every query's contamination groups and forbidden canary documents must be
/// declared by the manifest.
pub(crate) fn validate_workload_against_manifest(
    workload: &QueryWorkloadV1,
    manifest: &FixtureManifestV1,
) -> Result<(), EvaluationContractError> {
    for query in &workload.queries {
        for group in &query.contamination_groups {
            if !manifest.contamination_groups.contains(group) {
                return Err(EvaluationContractError::CoverageViolation(format!(
                    "query {} declares undeclared contamination group {group}",
                    query.query_id
                )));
            }
        }
        for forbidden in &query.forbidden_document_ids {
            if manifest.document(forbidden).is_none() {
                return Err(EvaluationContractError::CoverageViolation(format!(
                    "query {} forbids unknown corpus document {forbidden}",
                    query.query_id
                )));
            }
        }
    }
    Ok(())
}
