//! Byte-backed native-qualification acceptance contracts.
//!
//! Successful qualification requires a reviewed embedded production report.
//! Until then the empty packaged bytes produce typed unavailability. These tests
//! deliberately never fabricate native evidence: every negative case mutates a
//! genuine encoded qualification at the boundary it is intended to exercise.

use crate::{
    DirectEvaluationStatusV1, NativeQualificationExpectationsV1,
    NativeQualificationVectorGenerationRetentionV1, PackagedNativeQualificationErrorV1,
    PackagedNativeQualificationV1, load_packaged_native_qualification_from_bytes,
    packaged_native_qualification_bytes, qualified_default_activation_candidate,
};

const EVALUATED_PROFILE_ID: &str = "hybrid-conservative";
const NO_MATERIALIZATION_CHILD_ENV: &str = "TRACEDECAY_NATIVE_QUALIFICATION_NO_MATERIALIZATION";
const NO_MATERIALIZATION_TEST: &str =
    "native_qualification_tests::embedded_qualification_load_does_not_materialize_runtime_assets";

fn packaged_bytes() -> Vec<u8> {
    packaged_native_qualification_bytes().to_vec()
}

fn encoded_with(mutator: impl FnOnce(&mut PackagedNativeQualificationV1)) -> Vec<u8> {
    let mut qualification =
        serde_json::from_slice::<PackagedNativeQualificationV1>(&packaged_bytes())
            .expect("embedded qualification bytes must decode before a boundary mutation");
    mutator(&mut qualification);
    serde_json::to_vec(&qualification).expect("encode qualification boundary mutation")
}

fn expectations() -> NativeQualificationExpectationsV1 {
    let qualification = serde_json::from_slice::<PackagedNativeQualificationV1>(&packaged_bytes())
        .expect("embedded qualification bytes must decode before expected authority construction");
    NativeQualificationExpectationsV1::packaged_default(
        EVALUATED_PROFILE_ID.to_owned(),
        qualification.qualification_key.runtime,
        qualification.qualification_key.platform,
    )
    .expect("packaged workload/corpus metadata must construct independent expectations")
}

fn raw_output_digest(qualification: &PackagedNativeQualificationV1) -> String {
    tracedecay_domain::canonical_sha256(&(
        "tracedecay.search-eval.raw-output-evidence.v1",
        &qualification.portable_evidence.report.raw_outputs,
    ))
    .expect("rebind deliberately mutated raw evidence")
    .as_str()
    .to_owned()
}

fn assert_refusal(bytes: &[u8], expected: PackagedNativeQualificationErrorV1) {
    let expectations = expectations();
    let actual = load_packaged_native_qualification_from_bytes(bytes, &expectations)
        .expect_err("boundary mutation must not qualify a native activation candidate");
    assert_eq!(actual, expected);
}

#[test]
fn byte_backed_native_qualification_returns_the_embedded_report_and_material() {
    let expectations = expectations();
    let expected = qualified_default_activation_candidate(&expectations)
        .expect("embedded native qualification must qualify the selected profile");
    let actual = load_packaged_native_qualification_from_bytes(&packaged_bytes(), &expectations)
        .expect("the embedded qualification bytes must load without native generation");

    assert_eq!(actual.clone().into_parts(), expected.into_parts());
    assert_eq!(
        actual.into_parts().1.profile.profile_id.as_str(),
        EVALUATED_PROFILE_ID,
        "qualification must return material for the requested profile"
    );
}

#[test]
fn embedded_qualification_load_does_not_materialize_runtime_assets() {
    if std::env::var_os(NO_MATERIALIZATION_CHILD_ENV).is_some() {
        let candidate = qualified_default_activation_candidate(&expectations())
            .expect("embedded qualification must load through metadata only");
        assert_eq!(
            candidate.into_parts().1.profile.profile_id.as_str(),
            EVALUATED_PROFILE_ID
        );
        return;
    }

    let isolation = tempfile::tempdir().expect("isolated environment");
    let non_directory = isolation.path().join("not-a-directory");
    std::fs::write(&non_directory, b"not a directory").expect("non-directory temp root");
    let status = std::process::Command::new(std::env::current_exe().expect("test executable"))
        .args(["--exact", NO_MATERIALIZATION_TEST])
        .env(NO_MATERIALIZATION_CHILD_ENV, "1")
        .env("TMPDIR", &non_directory)
        .status()
        .expect("run isolated qualification loader");

    assert!(
        status.success(),
        "loading embedded qualification must not invoke the real tempfile-backed asset materializer"
    );
}

#[test]
fn byte_backed_native_qualification_explicitly_redacts_vector_generation_identity() {
    let qualification = serde_json::from_slice::<PackagedNativeQualificationV1>(&packaged_bytes())
        .expect("embedded qualification bytes must decode before portable identity inspection");

    assert_eq!(
        qualification.portable_evidence.vector_generation_retention,
        NativeQualificationVectorGenerationRetentionV1::RedactedForPortableQualification,
    );
    for output in &qualification.portable_evidence.report.raw_outputs {
        let resources = output
            .native_resources
            .as_ref()
            .expect("genuine package retains native resource evidence");
        for stage in resources.samples.values() {
            let crate::semantic_native::SemanticNativeStageResultV1::Complete(sample) = stage
            else {
                panic!("genuine package retains complete native resource evidence");
            };
            assert!(
                sample.provenance.vector_generation_id.is_none(),
                "portable package must not retain an evaluator-local vector generation"
            );
        }
    }
}

#[test]
fn byte_backed_native_qualification_rejects_retained_vector_generation_identity() {
    let bytes = encoded_with(|qualification| {
        let sample = qualification
            .portable_evidence
            .report
            .raw_outputs
            .iter_mut()
            .find_map(|output| output.native_resources.as_mut())
            .and_then(|resources| resources.samples.values_mut().next())
            .expect("genuine package retains a native resource sample");
        let crate::semantic_native::SemanticNativeStageResultV1::Complete(sample) = sample else {
            panic!("genuine package retains complete native resource evidence");
        };
        sample.provenance.vector_generation_id =
            Some("sha256:portable-qualification-mutation".to_owned());
        rebind_raw_output_digest(qualification);
    });

    assert_refusal(
        &bytes,
        PackagedNativeQualificationErrorV1::InvalidQualificationKey,
    );
}

#[test]
fn byte_backed_native_qualification_rejects_corrupt_bytes() {
    assert_refusal(
        b"not a native qualification document",
        PackagedNativeQualificationErrorV1::CorruptBytes,
    );
}

#[test]
fn byte_backed_native_qualification_rejects_an_unsupported_schema() {
    let bytes = encoded_with(|qualification| qualification.schema_version = u32::MAX);

    assert_refusal(
        &bytes,
        PackagedNativeQualificationErrorV1::UnsupportedSchema,
    );
}

#[test]
fn byte_backed_native_qualification_rejects_a_stale_workload() {
    let bytes = encoded_with(|qualification| {
        qualification.portable_evidence.report.workload_digest = "sha256:stale-workload".to_owned();
    });

    assert_refusal(&bytes, PackagedNativeQualificationErrorV1::StaleWorkload);
}

#[test]
fn byte_backed_native_qualification_rejects_a_stale_corpus() {
    let bytes = encoded_with(|qualification| {
        qualification.portable_evidence.report.corpus_digest = "sha256:stale-corpus".to_owned();
        qualification.qualification_key.evaluator.corpus_digest = "sha256:stale-corpus".to_owned();
    });

    assert_refusal(&bytes, PackagedNativeQualificationErrorV1::StaleCorpus);
}

#[test]
fn byte_backed_native_qualification_rejects_a_stale_execution_revision() {
    let bytes = encoded_with(|qualification| {
        qualification
            .portable_evidence
            .report
            .execution_contract
            .projection_revision = "retriever.semantic-flat.evaluation.stale".to_owned();
        qualification
            .qualification_key
            .evaluator
            .execution_contract
            .projection_revision = "retriever.semantic-flat.evaluation.stale".to_owned();
    });

    assert_refusal(
        &bytes,
        PackagedNativeQualificationErrorV1::StaleExecutionRevision,
    );
}

#[test]
fn byte_backed_native_qualification_rejects_a_model_mismatch() {
    let bytes = encoded_with(|qualification| {
        qualification
            .portable_evidence
            .report
            .execution_contract
            .model_revision = "model:wrong".to_owned();
        qualification
            .qualification_key
            .evaluator
            .execution_contract
            .model_revision = "model:wrong".to_owned();
    });

    assert_refusal(&bytes, PackagedNativeQualificationErrorV1::ModelMismatch);
}

#[test]
fn byte_backed_native_qualification_rejects_a_runtime_mismatch() {
    let bytes = encoded_with(|qualification| {
        qualification
            .portable_evidence
            .report
            .execution_contract
            .runtime_revision = "runtime:wrong".to_owned();
        qualification
            .qualification_key
            .evaluator
            .execution_contract
            .runtime_revision = "runtime:wrong".to_owned();
    });

    assert_refusal(&bytes, PackagedNativeQualificationErrorV1::RuntimeMismatch);
}

#[test]
fn byte_backed_native_qualification_rejects_incomplete_native_evidence() {
    let bytes = encoded_with(|qualification| {
        qualification.portable_evidence.report.raw_outputs[0].native_resources = None;
        qualification.portable_evidence.report.raw_output_digest = raw_output_digest(qualification);
    });

    assert_refusal(
        &bytes,
        PackagedNativeQualificationErrorV1::IncompleteNativeEvidence,
    );
}

#[test]
fn byte_backed_native_qualification_rejects_a_failed_qualification() {
    let bytes = encoded_with(|qualification| {
        qualification.portable_evidence.report.status = DirectEvaluationStatusV1::Fail;
    });

    assert_refusal(
        &bytes,
        PackagedNativeQualificationErrorV1::FailedQualification,
    );
}

#[test]
fn byte_backed_native_qualification_rejects_a_mutated_portable_model_key() {
    let bytes = encoded_with(|qualification| {
        qualification
            .qualification_key
            .runtime
            .model
            .runtime_backend = "fastembed-mutated".to_owned();
    });

    assert_refusal(&bytes, PackagedNativeQualificationErrorV1::ModelMismatch);
}

#[test]
fn byte_backed_native_qualification_rejects_a_mutated_search_index_key() {
    let bytes = encoded_with(|qualification| {
        qualification
            .qualification_key
            .runtime
            .search_index_key
            .schema_revision = "semantic-index.mutated.v1".to_owned();
    });

    assert_refusal(
        &bytes,
        PackagedNativeQualificationErrorV1::SearchIndexMismatch,
    );
}

#[test]
fn byte_backed_native_qualification_rejects_a_mutated_build_key() {
    let bytes = encoded_with(|qualification| {
        qualification.qualification_key.runtime.fusion_revision =
            tracedecay_domain::ComponentRevision::new("fusion.native.mutated.v1")
                .expect("valid mutated fusion revision");
    });

    assert_refusal(&bytes, PackagedNativeQualificationErrorV1::BuildMismatch);
}

#[test]
fn byte_backed_native_qualification_rejects_a_mutated_platform_key() {
    let bytes = encoded_with(|qualification| {
        qualification.qualification_key.platform.architecture = "mutated-architecture".to_owned();
    });

    assert_refusal(&bytes, PackagedNativeQualificationErrorV1::PlatformMismatch);
}

#[test]
fn byte_backed_native_qualification_rejects_a_missing_baseline_profile() {
    let bytes = encoded_with(|qualification| {
        qualification
            .portable_evidence
            .report
            .raw_outputs
            .retain(|output| output.profile_id != "query-fallback");
        rebind_raw_output_digest(qualification);
    });

    assert_refusal(
        &bytes,
        PackagedNativeQualificationErrorV1::IncompleteNativeEvidence,
    );
}

#[test]
fn byte_backed_native_qualification_rejects_a_missing_partition() {
    let bytes = encoded_with(|qualification| {
        qualification
            .portable_evidence
            .report
            .raw_outputs
            .retain(|output| {
                !(output.profile_id == EVALUATED_PROFILE_ID && output.partition == "validation")
            });
        rebind_raw_output_digest(qualification);
    });

    assert_refusal(
        &bytes,
        PackagedNativeQualificationErrorV1::IncompleteNativeEvidence,
    );
}

#[test]
fn byte_backed_native_qualification_rejects_a_missing_profile_aggregate() {
    let bytes = encoded_with(|qualification| {
        qualification
            .portable_evidence
            .report
            .profiles
            .retain(|profile| profile.profile_id != "query-fallback");
    });

    assert_refusal(
        &bytes,
        PackagedNativeQualificationErrorV1::InvalidRawOutputEvidence,
    );
}

#[test]
fn byte_backed_native_qualification_rejects_a_missing_query_stage() {
    let bytes = encoded_with(|qualification| {
        let output = qualification
            .portable_evidence
            .report
            .raw_outputs
            .iter_mut()
            .find(|output| output.profile_id == EVALUATED_PROFILE_ID)
            .expect("selected profile output");
        let native = output.queries[0]
            .native
            .as_mut()
            .expect("genuine native query evidence");
        native.exact_flat_oracle = crate::semantic_native::SemanticNativeStageResultV1::Pending {
            reason:
                crate::semantic_native::SemanticNativePendingReasonV1::SemanticGenerationUnavailable,
        };
        rebind_raw_output_digest(qualification);
    });

    assert_refusal(
        &bytes,
        PackagedNativeQualificationErrorV1::InvalidRawOutputEvidence,
    );
}

fn rebind_raw_output_digest(qualification: &mut PackagedNativeQualificationV1) {
    let digest = raw_output_digest(qualification);
    qualification.portable_evidence.report.raw_output_digest = digest.clone();
    qualification.qualification_key.evaluator.raw_output_digest = digest;
}
