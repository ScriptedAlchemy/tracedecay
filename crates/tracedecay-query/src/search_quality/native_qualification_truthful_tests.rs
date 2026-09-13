use super::*;

fn current_expectations(
    qualification: &PackagedNativeQualificationV1,
) -> NativeQualificationExpectationsV1 {
    NativeQualificationExpectationsV1::packaged_default(
        SEMANTIC_PROFILE.to_owned(),
        qualification.qualification_key.runtime.clone(),
        NativeQualificationPlatformV1::current(),
    )
    .expect("current qualification expectations")
}

#[test]
fn stale_packaged_pass_names_both_workload_digests() {
    let qualification = load_embedded_qualification().expect("reviewed qualification");
    let expectations = current_expectations(&qualification);
    let error = validate_qualification(&qualification, &expectations)
        .expect_err("superseded PASS must not activate");
    let failure = qualification_failure(
        error,
        Some(&qualification),
        embedded_qualification_bytes().ok(),
        &expectations,
    );

    assert!(matches!(
        failure,
        SemanticQualificationFailureV1::StaleWorkload {
            profile_id,
            packaged_workload_digest,
            current_workload_digest,
            ..
        } if profile_id == SEMANTIC_PROFILE
            && packaged_workload_digest
                == qualification.qualification_key.evaluator.workload_digest
            && current_workload_digest == expectations.workload_digest
    ));
}

#[test]
fn fresh_failed_report_is_failed_qualification() {
    let mut qualification = load_embedded_qualification().expect("reviewed qualification");
    let expectations = current_expectations(&qualification);
    qualification.portable_evidence.report.status = DirectEvaluationStatusV1::Fail;
    let error = validate_qualification(&qualification, &expectations)
        .expect_err("a genuine non-PASS must not activate");
    let failure = qualification_failure(error, Some(&qualification), None, &expectations);

    assert!(matches!(
        failure,
        SemanticQualificationFailureV1::FailedQualification {
            profile_id,
            workload_digest,
            ..
        } if profile_id == SEMANTIC_PROFILE
            && workload_digest == qualification.qualification_key.evaluator.workload_digest
    ));
}

#[test]
fn missing_evidence_is_distinct_from_stale_or_failed() {
    let qualification = load_embedded_qualification().expect("reviewed qualification");
    let expectations = current_expectations(&qualification);
    let failure = qualification_failure(
        PackagedNativeQualificationErrorV1::EmbeddedAssetUnavailable,
        None,
        None,
        &expectations,
    );

    assert!(matches!(
        failure,
        SemanticQualificationFailureV1::NoQualificationEvidence {
            evidence_digest: None,
            ..
        }
    ));
}

#[test]
fn digest_patching_does_not_create_qualification_provenance() {
    let mut qualification = load_embedded_qualification().expect("reviewed qualification");
    let expectations = current_expectations(&qualification);
    qualification.qualification_key.evaluator.workload_digest =
        expectations.workload_digest.clone();
    qualification.portable_evidence.report.workload_digest = expectations.workload_digest.clone();
    let bytes = serde_json::to_vec(&qualification).expect("patched qualification");
    let error = load_packaged_native_qualification_from_bytes(&bytes, &expectations)
        .expect_err("digest patching must not mint qualification");
    let failure = qualification_failure(error, Some(&qualification), Some(&bytes), &expectations);

    assert!(matches!(
        failure,
        SemanticQualificationFailureV1::NoQualificationEvidence {
            evidence_digest: Some(_),
            ..
        }
    ));
}

#[test]
fn wrong_profile_is_not_qualification_evidence() {
    let mut qualification = load_embedded_qualification().expect("reviewed qualification");
    let expectations = current_expectations(&qualification);
    qualification.qualification_key.evaluated_profile_id =
        super::super::evaluate::RERANK_PROFILE.to_owned();
    let bytes = serde_json::to_vec(&qualification).expect("wrong-profile qualification");
    let error = load_packaged_native_qualification_from_bytes(&bytes, &expectations)
        .expect_err("wrong profile must not activate");
    let failure = qualification_failure(error, Some(&qualification), Some(&bytes), &expectations);

    assert!(matches!(
        failure,
        SemanticQualificationFailureV1::NoQualificationEvidence { detail, .. }
            if detail.contains(super::super::evaluate::RERANK_PROFILE)
                && detail.contains(SEMANTIC_PROFILE)
    ));
}

#[test]
fn genuine_passing_package_passes_sha_and_provenance_validation() {
    let qualification = load_embedded_qualification().expect("reviewed qualification");

    assert_eq!(
        validate_document_bindings(&qualification),
        Ok(()),
        "the positive control is genuine PASS evidence; only its workload binding is stale"
    );
}
