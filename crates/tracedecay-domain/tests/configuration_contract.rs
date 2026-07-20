use std::collections::BTreeSet;

use tracedecay_domain::configuration::{
    AccessRuleId, AuthorityRef, CapabilityResolutionContextV1, CredentialKindV1,
    CredentialReferenceId, CredentialReferenceMetadataV1, RuleEffect, ScopeAccessRule,
    ScopeAccessSubjectV1, ScopeSourceBinding, SourceBindingId, SourceKindV1, UserProfileId,
    WorktreePlacementModeV1, resolve_restrictive_capabilities, safe_work_topology_policy_v1,
};
use tracedecay_domain::{
    ActorId, CapabilityId, LocatorDigest, ManifestDigest, ProjectId, UtcMicros,
};

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    <T as TryFrom<String>>::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).expect("fixture id is canonical")
}

fn digest(byte: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64)))
        .expect("fixture digest is canonical")
}

fn locator_digest(byte: char) -> LocatorDigest {
    LocatorDigest::new(format!("sha256:{}", byte.to_string().repeat(64)))
        .expect("fixture digest is canonical")
}

#[test]
fn safe_topology_default_is_restrictive_and_digest_stable() {
    let policy = safe_work_topology_policy_v1();
    policy.validate().expect("safe default must validate");

    assert_eq!(
        policy.placement,
        WorktreePlacementModeV1::ExistingWorktreeOnly
    );
    assert!(policy.roots.is_empty());
    assert!(!policy.cross_merge.allow_cross_repository);
    assert_eq!(
        policy.cross_merge.default_mode,
        tracedecay_domain::configuration::CrossMergeModeV1::Disabled
    );
    assert_eq!(
        policy.compute_digest().unwrap(),
        policy.compute_digest().unwrap()
    );
}

#[test]
fn projectless_hermes_binding_cannot_be_reused_for_other_source_kinds() {
    let binding = ScopeSourceBinding::new(
        id::<SourceBindingId>("binding.hermes"),
        SourceKindV1::Hermes,
        locator_digest('a'),
        AuthorityRef::ProjectlessHermes(id::<UserProfileId>("profile.hermes")),
    )
    .expect("Hermes may bind to a user profile");
    binding.validate().unwrap();

    let invalid = ScopeSourceBinding::new(
        id::<SourceBindingId>("binding.cursor"),
        SourceKindV1::Cursor,
        locator_digest('b'),
        AuthorityRef::ProjectlessHermes(id::<UserProfileId>("profile.hermes")),
    );
    assert!(invalid.is_err(), "only projectless Hermes is representable");
}

#[test]
fn deny_rules_union_before_allow_rules_intersect() {
    let read = id::<CapabilityId>("capability.read");
    let write = id::<CapabilityId>("capability.write");
    let authority = AuthorityRef::Project(id::<ProjectId>("project.fixture"));
    let subject = ScopeAccessSubjectV1 {
        actor: Some(id::<ActorId>("actor.fixture")),
        operation: None,
        source_kind: Some(SourceKindV1::Hermes),
    };
    let allow = ScopeAccessRule::new(
        id::<AccessRuleId>("rule.allow"),
        subject.clone(),
        authority.clone(),
        BTreeSet::from([read.clone(), write.clone()]),
        RuleEffect::Allow,
        None,
    )
    .unwrap();
    let deny = ScopeAccessRule::new(
        id::<AccessRuleId>("rule.deny"),
        subject.clone(),
        authority.clone(),
        BTreeSet::from([write.clone()]),
        RuleEffect::Deny,
        None,
    )
    .unwrap();

    let result = resolve_restrictive_capabilities(
        BTreeSet::from([read.clone(), write]),
        &[allow, deny],
        &CapabilityResolutionContextV1 {
            actor: id::<ActorId>("actor.fixture"),
            operation: None,
            source_kind: SourceKindV1::Hermes,
            authority,
            evaluated_at: UtcMicros(1),
        },
    )
    .unwrap();

    assert_eq!(result.effective, BTreeSet::from([read]));
}

#[test]
fn credential_metadata_has_no_plaintext_value_surface() {
    let reference = CredentialReferenceMetadataV1::new(
        id::<CredentialReferenceId>("credential.reference"),
        CredentialKindV1::ApiToken,
        digest('c'),
        UtcMicros(42),
        1,
    )
    .unwrap();

    let encoded = serde_json::to_value(reference).unwrap();
    assert!(encoded.get("value").is_none());
    assert!(encoded.get("plaintext").is_none());
    assert!(encoded.get("secret").is_none());
    assert!(encoded.get("reference_digest").is_some());
}
