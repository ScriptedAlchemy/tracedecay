use tracedecay_domain::ManifestDigest;
use tracedecay_policy::{
    CurationApplyDispositionV1, CurationApplyPolicyInputV1, CurationApplySubjectV1,
    CurationValidationDispositionV1, evaluate_curation_apply,
};

fn digest(byte: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).expect("digest")
}

#[test]
fn validated_curation_is_allowed_only_with_exact_evidence_and_validation_identities() {
    assert_eq!(
        evaluate_curation_apply(&CurationApplyPolicyInputV1 {
            subject: CurationApplySubjectV1::MemoryCurator,
            evidence_digest: Some(digest('a')),
            output_digest: digest('b'),
            validation: CurationValidationDispositionV1::Accepted,
            configuration_digest: digest('c'),
        })
        .expect("decision")
        .disposition,
        CurationApplyDispositionV1::Allow
    );
    assert_eq!(
        evaluate_curation_apply(&CurationApplyPolicyInputV1 {
            subject: CurationApplySubjectV1::MemoryCurator,
            evidence_digest: None,
            output_digest: digest('b'),
            validation: CurationValidationDispositionV1::Accepted,
            configuration_digest: digest('c'),
        })
        .expect("decision")
        .disposition,
        CurationApplyDispositionV1::Indeterminate
    );
}

#[test]
fn curation_with_no_candidate_is_not_applicable() {
    let decision = evaluate_curation_apply(&CurationApplyPolicyInputV1 {
        subject: CurationApplySubjectV1::SessionReflector,
        evidence_digest: Some(digest('a')),
        output_digest: digest('b'),
        validation: CurationValidationDispositionV1::NoCandidate,
        configuration_digest: digest('c'),
    })
    .expect("decision");

    assert_eq!(
        decision.disposition,
        CurationApplyDispositionV1::NotApplicable
    );
    assert!(!decision.allows_apply());
}
