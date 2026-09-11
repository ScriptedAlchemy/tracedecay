use tracedecay_domain::configuration::{ConfigurationRevisionId, UserProfileId};
use tracedecay_domain::{ActorId, ManifestDigest, ProjectId};
use tracedecay_policy::{
    CurationApplyAuthorityV1, CurationApplyDispositionV1, CurationApplyPolicyInputV1,
    CurationApplySubjectV1, CurationValidationDispositionV1, evaluate_curation_apply,
};

fn digest(byte: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).expect("digest")
}

fn input(
    subject: CurationApplySubjectV1,
    evidence_digest: Option<ManifestDigest>,
    validation: CurationValidationDispositionV1,
) -> CurationApplyPolicyInputV1 {
    CurationApplyPolicyInputV1 {
        authority: CurationApplyAuthorityV1 {
            actor_id: ActorId::new(match subject {
                CurationApplySubjectV1::MemoryCurator => "automation:memory-curator",
                CurationApplySubjectV1::SessionReflector => "automation:session-reflector",
                CurationApplySubjectV1::SkillWriter => "automation:skill-writer",
            })
            .expect("actor"),
            project_id: Some(ProjectId::new("project.curation").expect("project")),
            profile_id: UserProfileId::new("profile.curation").expect("profile"),
            configuration_revision_id: ConfigurationRevisionId::new("config.curation.v1")
                .expect("configuration revision"),
        },
        subject,
        evidence_digest,
        output_digest: digest('b'),
        validation,
        configuration_digest: digest('c'),
    }
}

#[test]
fn subject_actor_mismatch_is_a_typed_denial() {
    let mut input = input(
        CurationApplySubjectV1::MemoryCurator,
        Some(digest('a')),
        CurationValidationDispositionV1::Accepted,
    );
    input.authority.actor_id = ActorId::new("automation:skill-writer").expect("actor");

    assert_eq!(
        evaluate_curation_apply(&input)
            .expect("decision")
            .disposition,
        CurationApplyDispositionV1::Deny
    );
}

#[test]
fn validated_curation_is_allowed_only_with_exact_evidence_and_validation_identities() {
    assert_eq!(
        evaluate_curation_apply(&input(
            CurationApplySubjectV1::MemoryCurator,
            Some(digest('a')),
            CurationValidationDispositionV1::Accepted,
        ))
        .expect("decision")
        .disposition,
        CurationApplyDispositionV1::Allow
    );
    assert_eq!(
        evaluate_curation_apply(&input(
            CurationApplySubjectV1::MemoryCurator,
            None,
            CurationValidationDispositionV1::Accepted,
        ))
        .expect("decision")
        .disposition,
        CurationApplyDispositionV1::Indeterminate
    );
}

#[test]
fn curation_with_no_candidate_is_not_applicable() {
    let decision = evaluate_curation_apply(&input(
        CurationApplySubjectV1::SessionReflector,
        Some(digest('a')),
        CurationValidationDispositionV1::NoCandidate,
    ))
    .expect("decision");

    assert_eq!(
        decision.disposition,
        CurationApplyDispositionV1::NotApplicable
    );
    assert!(!decision.allows_apply());
}

#[test]
fn decision_binds_exact_authority_and_configuration_revision() {
    let input = input(
        CurationApplySubjectV1::SkillWriter,
        Some(digest('a')),
        CurationValidationDispositionV1::Accepted,
    );
    let decision = evaluate_curation_apply(&input).expect("decision");

    assert_eq!(decision.authority, input.authority);
}
