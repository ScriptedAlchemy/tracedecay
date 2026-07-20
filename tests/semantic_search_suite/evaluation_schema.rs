use std::collections::BTreeMap;

use super::evaluation::{
    BenchmarkCacheStateV1, BenchmarkEvidenceRoleV1, BenchmarkKindV1, BenchmarkScaleV1,
    ComponentRevisionIdV1, EvidenceAnchorIdV1, GitCommitShaV1, HardwareClassIdV1,
    PROJECTION_MEASURED_REPETITIONS, PROJECTION_WARMUP_REPETITIONS, QUERY_MEASURED_REPETITIONS,
    QUERY_WARMUP_REPETITIONS, RepositoryRelativePathV1, SEMANTIC_BENCHMARK_RESULT_SCHEMA_VERSION,
    SEMANTIC_BENCHMARK_WORKLOAD_SCHEMA_VERSION, SemanticBenchmarkEvidenceAnchorV1,
    SemanticBenchmarkResultIdV1, SemanticBenchmarkResultV1, SemanticBenchmarkRevisionSetV1,
    SemanticBenchmarkSampleV1, SemanticBenchmarkWorkloadIdV1, SemanticBenchmarkWorkloadV1,
    Sha256DigestV1, WorkloadGroupIdV1,
};

fn digest(label: &str) -> Sha256DigestV1 {
    use sha2::Digest as _;

    let encoded = sha2::Sha256::digest(label.as_bytes());
    Sha256DigestV1::new(format!("sha256:{}", hex::encode(encoded))).unwrap()
}

fn anchor(role: BenchmarkEvidenceRoleV1, id: &str) -> SemanticBenchmarkEvidenceAnchorV1 {
    SemanticBenchmarkEvidenceAnchorV1 {
        anchor_id: EvidenceAnchorIdV1::new(id).unwrap(),
        role,
        artifact_path: RepositoryRelativePathV1::new(format!("evidence/{id}.json")).unwrap(),
        artifact_digest: digest(id),
        byte_len: 128,
    }
}

fn query_workload(
    scale: BenchmarkScaleV1,
    eligible_chunk_count: u64,
) -> SemanticBenchmarkWorkloadV1 {
    let mut strata = BTreeMap::new();
    strata.insert("python".to_string(), 20);
    strata.insert("rust".to_string(), 30);

    let mut workload = SemanticBenchmarkWorkloadV1 {
        schema_version: SEMANTIC_BENCHMARK_WORKLOAD_SCHEMA_VERSION,
        workload_id: SemanticBenchmarkWorkloadIdV1::new(match scale {
            BenchmarkScaleV1::Current => "query-current",
            BenchmarkScaleV1::TenX => "query-10x",
        })
        .unwrap(),
        workload_group_id: WorkloadGroupIdV1::new("query-benchmark").unwrap(),
        kind: BenchmarkKindV1::Query,
        scale,
        corpus_anchor: anchor(BenchmarkEvidenceRoleV1::CorpusDescriptor, "corpus"),
        query_set_anchor: Some(anchor(BenchmarkEvidenceRoleV1::QuerySet, "queries")),
        file_count: 500,
        eligible_chunk_count,
        query_count: 50,
        language_source_strata: strata,
        seed: 0x5eed,
        revisions: SemanticBenchmarkRevisionSetV1 {
            model: ComponentRevisionIdV1::new("model-r1").unwrap(),
            projection: ComponentRevisionIdV1::new("projection-r1").unwrap(),
            fusion: ComponentRevisionIdV1::new("fusion-r1").unwrap(),
        },
        hardware_class: HardwareClassIdV1::new("linux-x86_64-16c-64g").unwrap(),
        hardware_manifest_anchor: anchor(BenchmarkEvidenceRoleV1::HardwareManifest, "hardware"),
        runtime_manifest_anchor: anchor(BenchmarkEvidenceRoleV1::RuntimeManifest, "runtime"),
        cache_state: BenchmarkCacheStateV1::Warm,
        concurrency: 1,
        warmup_repetitions: QUERY_WARMUP_REPETITIONS,
        measured_repetitions: QUERY_MEASURED_REPETITIONS,
        digest: digest("placeholder"),
    };
    workload.digest = workload.compute_digest().unwrap();
    workload
}

fn projection_workload() -> SemanticBenchmarkWorkloadV1 {
    let mut workload = SemanticBenchmarkWorkloadV1 {
        schema_version: SEMANTIC_BENCHMARK_WORKLOAD_SCHEMA_VERSION,
        workload_id: SemanticBenchmarkWorkloadIdV1::new("projection-current").unwrap(),
        workload_group_id: WorkloadGroupIdV1::new("projection-benchmark").unwrap(),
        kind: BenchmarkKindV1::Projection,
        scale: BenchmarkScaleV1::Current,
        corpus_anchor: anchor(
            BenchmarkEvidenceRoleV1::CorpusDescriptor,
            "projection-corpus",
        ),
        query_set_anchor: None,
        file_count: 500,
        eligible_chunk_count: 12_000,
        query_count: 0,
        language_source_strata: BTreeMap::new(),
        seed: 0x5eed,
        revisions: SemanticBenchmarkRevisionSetV1 {
            model: ComponentRevisionIdV1::new("model-r1").unwrap(),
            projection: ComponentRevisionIdV1::new("projection-r1").unwrap(),
            fusion: ComponentRevisionIdV1::new("fusion-r1").unwrap(),
        },
        hardware_class: HardwareClassIdV1::new("linux-x86_64-16c-64g").unwrap(),
        hardware_manifest_anchor: anchor(
            BenchmarkEvidenceRoleV1::HardwareManifest,
            "projection-hardware",
        ),
        runtime_manifest_anchor: anchor(
            BenchmarkEvidenceRoleV1::RuntimeManifest,
            "projection-runtime",
        ),
        cache_state: BenchmarkCacheStateV1::Cold,
        concurrency: 1,
        warmup_repetitions: PROJECTION_WARMUP_REPETITIONS,
        measured_repetitions: PROJECTION_MEASURED_REPETITIONS,
        digest: digest("placeholder"),
    };
    workload.digest = workload.compute_digest().unwrap();
    workload
}

fn sample(ordinal: u32) -> SemanticBenchmarkSampleV1 {
    SemanticBenchmarkSampleV1 {
        ordinal,
        wall_time_ns: 1_000 + u64::from(ordinal),
        cpu_time_ns: 900 + u64::from(ordinal),
        peak_rss_bytes: 64 * 1024 * 1024,
        bytes_read: 1024,
        bytes_written: 512,
        model_bytes: 32 * 1024 * 1024,
        vector_bytes: 8 * 1024 * 1024,
        cache_bytes: 1024 * 1024,
        candidates: 25,
        chunks_embedded: 0,
        chunks_reused: 0,
        chunks_deleted: 0,
        hydration_fetches: 5,
    }
}

fn result(workload: &SemanticBenchmarkWorkloadV1) -> SemanticBenchmarkResultV1 {
    let samples = (0..workload.measured_repetitions).map(sample).collect();
    let mut result = SemanticBenchmarkResultV1 {
        schema_version: SEMANTIC_BENCHMARK_RESULT_SCHEMA_VERSION,
        result_id: SemanticBenchmarkResultIdV1::new("result-v1").unwrap(),
        workload_id: workload.workload_id.clone(),
        workload_digest: workload.digest.clone(),
        code_revision: GitCommitShaV1::new("0123456789abcdef0123456789abcdef01234567").unwrap(),
        captured_at_unix_micros: 1_800_000_000_000_000,
        samples,
        evidence_anchors: vec![
            SemanticBenchmarkEvidenceAnchorV1 {
                artifact_digest: workload.digest.clone(),
                ..anchor(BenchmarkEvidenceRoleV1::WorkloadManifest, "workload")
            },
            anchor(BenchmarkEvidenceRoleV1::RawSamples, "raw-samples"),
            anchor(BenchmarkEvidenceRoleV1::CandidateList, "candidate-list"),
        ],
        digest: digest("placeholder"),
    };
    result.digest = result.compute_digest().unwrap();
    result
}

#[test]
fn valid_workload_result_and_evidence_anchors_round_trip() {
    let workload = query_workload(BenchmarkScaleV1::Current, 12_000);
    workload.validate().unwrap();

    let result = result(&workload);
    result.validate_against_workload(&workload).unwrap();

    let workload_json = serde_json::to_string(&workload).unwrap();
    let decoded_workload: SemanticBenchmarkWorkloadV1 =
        serde_json::from_str(&workload_json).unwrap();
    decoded_workload.validate().unwrap();
    assert_eq!(decoded_workload, workload);

    let result_json = serde_json::to_string(&result).unwrap();
    let decoded_result: SemanticBenchmarkResultV1 = serde_json::from_str(&result_json).unwrap();
    decoded_result.validate_against_workload(&workload).unwrap();
    assert_eq!(decoded_result, result);

    for forbidden in [
        "labels",
        "holdout",
        "tuning",
        "profile_claim",
        "promotion",
        "activation",
    ] {
        assert!(!workload_json.contains(forbidden));
        assert!(!result_json.contains(forbidden));
    }
}

#[test]
fn evidence_anchor_rejects_invalid_identity_path_digest_and_length() {
    assert!(EvidenceAnchorIdV1::new(" leading-space").is_err());
    assert!(RepositoryRelativePathV1::new("/absolute/result.json").is_err());
    assert!(RepositoryRelativePathV1::new("evidence/../labels.json").is_err());
    assert!(RepositoryRelativePathV1::new(r"evidence\result.json").is_err());
    assert!(Sha256DigestV1::new("sha256:abcd").is_err());
    assert!(Sha256DigestV1::new(format!("sha256:{}", "A".repeat(64))).is_err());

    let mut zero_length = anchor(BenchmarkEvidenceRoleV1::RawSamples, "raw");
    zero_length.byte_len = 0;
    assert!(zero_length.validate().is_err());

    let mut value =
        serde_json::to_value(anchor(BenchmarkEvidenceRoleV1::RawSamples, "strict-fields")).unwrap();
    value
        .as_object_mut()
        .unwrap()
        .insert("locked_labels".to_string(), serde_json::json!("forbidden"));
    assert!(serde_json::from_value::<SemanticBenchmarkEvidenceAnchorV1>(value).is_err());
}

#[test]
fn workload_validator_enforces_kind_contracts_strata_and_digest() {
    let projection = projection_workload();
    projection.validate().unwrap();

    let mut query = query_workload(BenchmarkScaleV1::Current, 12_000);
    query.warmup_repetitions = PROJECTION_WARMUP_REPETITIONS;
    query.digest = query.compute_digest().unwrap();
    assert!(query.validate().is_err());

    let mut projection_with_queries = projection_workload();
    projection_with_queries.query_count = 1;
    projection_with_queries.query_set_anchor = Some(anchor(
        BenchmarkEvidenceRoleV1::QuerySet,
        "unexpected-query",
    ));
    projection_with_queries
        .language_source_strata
        .insert("rust".to_string(), 1);
    projection_with_queries.digest = projection_with_queries.compute_digest().unwrap();
    assert!(projection_with_queries.validate().is_err());

    let mut mismatched_strata = query_workload(BenchmarkScaleV1::Current, 12_000);
    mismatched_strata
        .language_source_strata
        .insert("rust".to_string(), 31);
    mismatched_strata.digest = mismatched_strata.compute_digest().unwrap();
    assert!(mismatched_strata.validate().is_err());

    let mut tampered = query_workload(BenchmarkScaleV1::Current, 12_000);
    tampered.eligible_chunk_count += 1;
    assert!(tampered.validate().is_err());
}

#[test]
fn ten_x_validator_requires_exact_chunk_multiplier_and_matching_contract() {
    let current = query_workload(BenchmarkScaleV1::Current, 12_000);
    let ten_x = query_workload(BenchmarkScaleV1::TenX, 120_000);
    ten_x.validate_ten_x_against(&current).unwrap();

    let wrong_size = query_workload(BenchmarkScaleV1::TenX, 119_999);
    assert!(wrong_size.validate_ten_x_against(&current).is_err());

    let mut wrong_revision = query_workload(BenchmarkScaleV1::TenX, 120_000);
    wrong_revision.revisions.fusion = ComponentRevisionIdV1::new("fusion-r2").unwrap();
    wrong_revision.digest = wrong_revision.compute_digest().unwrap();
    assert!(wrong_revision.validate_ten_x_against(&current).is_err());
}

#[test]
fn result_validator_rejects_missing_duplicate_or_mismatched_evidence() {
    let workload = query_workload(BenchmarkScaleV1::Current, 12_000);

    let mut missing_raw = result(&workload);
    missing_raw
        .evidence_anchors
        .retain(|anchor| anchor.role != BenchmarkEvidenceRoleV1::RawSamples);
    missing_raw.digest = missing_raw.compute_digest().unwrap();
    assert!(missing_raw.validate_against_workload(&workload).is_err());

    let mut duplicate_path = result(&workload);
    let duplicate = duplicate_path.evidence_anchors[0].clone();
    duplicate_path.evidence_anchors.push(duplicate);
    duplicate_path.digest = duplicate_path.compute_digest().unwrap();
    assert!(duplicate_path.validate_against_workload(&workload).is_err());

    let mut non_contiguous = result(&workload);
    non_contiguous.samples[10].ordinal = 11;
    non_contiguous.digest = non_contiguous.compute_digest().unwrap();
    assert!(non_contiguous.validate_against_workload(&workload).is_err());

    let other = query_workload(BenchmarkScaleV1::Current, 24_000);
    assert!(result(&workload).validate_against_workload(&other).is_err());

    let mut tampered = result(&workload);
    tampered.samples[0].wall_time_ns += 1;
    assert!(tampered.validate_against_workload(&workload).is_err());
}
