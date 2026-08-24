use std::collections::{BTreeMap, BTreeSet};

use tracedecay_domain::configuration::{
    CandidateDispositionV1, ConfigurationCandidateV1, ConfigurationLayerIdV1,
};

use super::*;

fn consent(
    granted_at: i64,
    expires_at: i64,
    categories: &[WorkExpertiseCategoryV1],
) -> WorkExpertiseConsentV1 {
    WorkExpertiseConsentV1 {
        schema_version: WorkExpertiseConsentV1::SCHEMA_VERSION,
        enabled: true,
        granted_at: Some(UtcMicros(granted_at)),
        expires_at: Some(UtcMicros(expires_at)),
        allowed_categories: categories.iter().copied().collect(),
    }
}

fn snapshot(
    user: WorkExpertiseConsentV1,
    project: WorkExpertiseConsentV1,
) -> WorkExpertiseConsentSnapshotV1 {
    let revision_id = ConfigurationRevisionId::new("configuration-revision.expertise.1")
        .expect("valid revision id");
    let user_key =
        SettingKey::new(USER_WORK_EXPERTISE_CONSENT_SETTING_KEY).expect("valid user setting key");
    let project_key = SettingKey::new(PROJECT_WORK_EXPERTISE_CONSENT_SETTING_KEY)
        .expect("valid project setting key");
    let candidate = ConfigurationCandidateV1 {
        layer: ConfigurationLayerIdV1::Default,
        revision_id: revision_id.clone(),
        disposition: CandidateDispositionV1::Defaulted,
        safe_reason: None,
    };
    let configuration = ConfigurationSnapshotV1::new(
        BTreeMap::from([
            (
                user_key.clone(),
                ConfigurationValueV1::WorkExpertiseConsent(user),
            ),
            (
                project_key.clone(),
                ConfigurationValueV1::WorkExpertiseConsent(project),
            ),
        ]),
        BTreeMap::from([
            (user_key, vec![candidate.clone()]),
            (project_key, vec![candidate]),
        ]),
    )
    .expect("valid configuration snapshot");
    WorkExpertiseConsentSnapshotV1::from_configuration(revision_id, configuration)
        .expect("valid expertise snapshot")
}

#[test]
fn expertise_requires_explicit_user_and_project_consent() {
    let authority = snapshot(
        WorkExpertiseConsentV1::disabled(),
        WorkExpertiseConsentV1::disabled(),
    );
    let categories = BTreeSet::from([WorkExpertiseCategoryV1::Language]);
    let authorization = authority
        .authorization(&categories, UtcMicros(10))
        .expect("typed unavailable authorization");
    let WorkExpertiseAuthorizationV1::Unavailable {
        reasons,
        legal_actions,
        ..
    } = authorization
    else {
        panic!("disabled consent must be unavailable");
    };
    assert!(reasons.contains(&WorkExpertiseUnavailableReasonV1::UserConsentDisabled));
    assert!(reasons.contains(&WorkExpertiseUnavailableReasonV1::ProjectConsentDisabled));
    assert!(legal_actions.contains(&WorkExpertiseLegalActionV1::GrantUserConsent));
    assert!(legal_actions.contains(&WorkExpertiseLegalActionV1::GrantProjectConsent));
}

#[test]
fn expertise_uses_category_intersection_and_earliest_expiry() {
    let authority = snapshot(
        consent(
            1,
            100,
            &[
                WorkExpertiseCategoryV1::Language,
                WorkExpertiseCategoryV1::Testing,
            ],
        ),
        consent(1, 80, &[WorkExpertiseCategoryV1::Language]),
    );
    let categories = BTreeSet::from([WorkExpertiseCategoryV1::Language]);
    let authorization = authority
        .authorization(&categories, UtcMicros(10))
        .expect("available authorization");
    assert!(matches!(
        authorization,
        WorkExpertiseAuthorizationV1::Available {
            categories: authorized,
            expires_at: UtcMicros(80),
            durability: WorkExpertiseContextDurabilityV1::EphemeralOnly,
            ..
        } if authorized == categories
    ));
}
