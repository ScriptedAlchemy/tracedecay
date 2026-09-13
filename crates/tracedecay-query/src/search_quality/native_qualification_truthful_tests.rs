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

/// Lift the shipped evidence into the current asset schema without touching a
/// single measurement.
///
/// The checked-in bytes are a genuine `qualify-native` PASS whose envelope
/// predates methodology 2. Raising only the envelope keeps every document
/// binding, digest, and per-query score exactly as the evaluator wrote it, so
/// these denials still run against real evidence rather than a hand-built
/// lookalike. It does not make that evidence activatable: its workload binding
/// is still superseded, which is what the stale denial below asserts.
fn shipped_evidence_under_current_schema() -> PackagedNativeQualificationV1 {
    let mut value = serde_json::from_slice::<serde_json::Value>(
        embedded_qualification_bytes().expect("embedded qualification bytes"),
    )
    .expect("shipped qualification json");
    let envelope = value.as_object_mut().expect("qualification envelope");
    envelope.insert(
        "schema_version".to_owned(),
        serde_json::json!(PACKAGED_NATIVE_QUALIFICATION_SCHEMA_VERSION),
    );
    envelope.insert(
        "methodology_version".to_owned(),
        serde_json::json!(QUALIFICATION_METHODOLOGY_VERSION),
    );
    let report = value["portable_evidence"]["report"]
        .as_object_mut()
        .expect("report object");
    report.insert(
        "methodology_version".to_owned(),
        serde_json::json!(QUALIFICATION_METHODOLOGY_VERSION),
    );
    report.insert("paired_effects".to_owned(), serde_json::json!([]));
    serde_json::from_value(value).expect("shipped evidence under the current schema")
}

fn failure_for(
    qualification: &PackagedNativeQualificationV1,
    expectations: &NativeQualificationExpectationsV1,
) -> SemanticQualificationFailureV1 {
    let bytes = serde_json::to_vec(qualification).expect("qualification bytes");
    let error = validate_qualification(qualification, expectations)
        .expect_err("this fixture must not activate");
    qualification_failure(error, Some(qualification), Some(&bytes), expectations)
}

#[test]
fn stale_packaged_pass_names_both_workload_digests() {
    let qualification = shipped_evidence_under_current_schema();
    let expectations = current_expectations(&qualification);
    let failure = failure_for(&qualification, &expectations);

    assert!(
        matches!(
            &failure,
            SemanticQualificationFailureV1::StaleWorkload {
                profile_id,
                packaged_workload_digest,
                current_workload_digest,
                ..
            } if profile_id == SEMANTIC_PROFILE
                && *packaged_workload_digest
                    == qualification.qualification_key.evaluator.workload_digest
                && *current_workload_digest == expectations.workload_digest
        ),
        "a superseded PASS must name both workload digests: {failure:?}"
    );
}

#[test]
fn fresh_failed_report_is_failed_qualification() {
    let mut qualification = shipped_evidence_under_current_schema();
    let expectations = current_expectations(&qualification);
    qualification.portable_evidence.report.status = DirectEvaluationStatusV1::Fail;
    qualification.portable_evidence.report.workload_digest = expectations.workload_digest.clone();
    qualification.qualification_key.evaluator.workload_digest =
        expectations.workload_digest.clone();
    let failure = failure_for(&qualification, &expectations);

    assert!(
        matches!(
            &failure,
            SemanticQualificationFailureV1::FailedQualification {
                profile_id,
                workload_digest,
                ..
            } if profile_id == SEMANTIC_PROFILE
                && *workload_digest == expectations.workload_digest
        ),
        "a genuine non-PASS on the current workload must be named as failed: {failure:?}"
    );
}

#[test]
fn missing_evidence_is_distinct_from_stale_or_failed() {
    let qualification = shipped_evidence_under_current_schema();
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
    let mut qualification = shipped_evidence_under_current_schema();
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
    let mut qualification = shipped_evidence_under_current_schema();
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
    let qualification = shipped_evidence_under_current_schema();

    assert_eq!(
        validate_document_bindings(&qualification),
        Ok(()),
        "the positive control is genuine PASS evidence; only its workload binding is stale"
    );
}

/// The two decision-rule refusals are their own states, not a shrug at absent
/// evidence: the bytes exist, a real run produced them, and they answer a
/// question this build no longer asks.
#[test]
fn superseded_schema_and_methodology_are_their_own_states() {
    let qualification = shipped_evidence_under_current_schema();
    let expectations = current_expectations(&qualification);

    let shipped_schema = PackagedVersionProbeV1::read(
        embedded_qualification_bytes().expect("embedded qualification bytes"),
    )
    .expect("shipped schema version")
    .schema_version;
    let schema_failure = packaged_native_qualification_failure(
        PackagedNativeQualificationErrorV1::UnsupportedSchema,
        &expectations,
    );
    assert!(
        matches!(
            &schema_failure,
            SemanticQualificationFailureV1::SupersededSchema {
                profile_id,
                packaged_schema_version,
                current_schema_version,
                evidence_digest: Some(_),
                ..
            } if profile_id == SEMANTIC_PROFILE
                && *packaged_schema_version == shipped_schema
                && *current_schema_version == PACKAGED_NATIVE_QUALIFICATION_SCHEMA_VERSION
        ),
        "superseded schema must name both versions and the evidence it refused: {schema_failure:?}"
    );

    let mut superseded = shipped_evidence_under_current_schema();
    superseded.methodology_version = QUALIFICATION_METHODOLOGY_VERSION - 1;
    superseded.portable_evidence.report.methodology_version =
        QUALIFICATION_METHODOLOGY_VERSION - 1;
    let bytes = serde_json::to_vec(&superseded).expect("superseded methodology bytes");
    let error = load_packaged_native_qualification_from_bytes(&bytes, &expectations)
        .expect_err("evidence scored under an earlier rule must not activate");
    assert_eq!(
        error,
        PackagedNativeQualificationErrorV1::UnsupportedMethodology
    );
    let methodology_failure =
        qualification_failure(error, Some(&superseded), Some(&bytes), &expectations);
    assert!(
        matches!(
            &methodology_failure,
            SemanticQualificationFailureV1::SupersededMethodology {
                profile_id,
                packaged_methodology_version,
                current_methodology_version,
                ..
            } if profile_id == SEMANTIC_PROFILE
                && *packaged_methodology_version == QUALIFICATION_METHODOLOGY_VERSION - 1
                && *current_methodology_version == QUALIFICATION_METHODOLOGY_VERSION
        ),
        "superseded methodology must be distinct from absent evidence: {methodology_failure:?}"
    );
}
