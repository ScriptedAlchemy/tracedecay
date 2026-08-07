use std::collections::BTreeSet;

use tracedecay_domain::configuration::{
    AccessRuleId, AuthorityRef, CONFIGURATION_SETTING_KEYS_V1, CapabilityResolutionContextV1,
    ConfigurationGrantId, ConfigurationGrantReceiptId, ConfigurationIdempotencyKey,
    ConfigurationMutationEffectV1, ConfigurationMutationGrantReceiptV1,
    ConfigurationMutationOperationV1, ConfigurationMutationSinkV1, ConfigurationRevisionId,
    ConfigurationSettlementAuthorityV1, ConfigurationValueV1, CredentialKindV1,
    CredentialReferenceId, CredentialReferenceMetadataV1, RuleEffect, SEMANTIC_RUNTIME_SETTING_KEY,
    ScopeAccessRule, ScopeAccessSubjectV1, ScopeSourceBinding, SettingKey, SourceBindingId,
    SourceKindV1, UserProfileId, WorktreePlacementModeV1, resolve_restrictive_capabilities,
    safe_work_topology_policy_v1,
};
use tracedecay_domain::feedback::PROXIMITY_RISK_THRESHOLD_SETTING_KEY_V1;
use tracedecay_domain::{
    AccessPolicyDigest, ActorId, CapabilityId, LocatorDigest, ManifestDigest, ProjectId, UtcMicros,
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
    let reference = CredentialReferenceMetadataV1 {
        reference_id: id::<CredentialReferenceId>("credential.reference"),
        kind: CredentialKindV1::ApiToken,
        reference_digest: digest('c'),
        operation_digest: digest('d'),
        settlement_authority: ConfigurationSettlementAuthorityV1 {
            policy_epoch: 1,
            policy_digest: id::<AccessPolicyDigest>(&format!("sha256:{}", "e".repeat(64))),
            revalidated_at: UtcMicros(42),
        },
        created_at: UtcMicros(42),
        effective_deadline_at: UtcMicros(84),
        rotation: 1,
    };
    reference.validate().unwrap();

    let encoded = serde_json::to_value(reference).unwrap();
    assert!(encoded.get("value").is_none());
    assert!(encoded.get("plaintext").is_none());
    assert!(encoded.get("secret").is_none());
    assert!(encoded.get("reference_digest").is_some());
    assert!(encoded.get("operation_digest").is_some());
}

#[test]
fn final_configuration_inventory_is_canonical_and_uses_typed_scalar_values() {
    assert!(!CONFIGURATION_SETTING_KEYS_V1.is_empty());
    assert!(CONFIGURATION_SETTING_KEYS_V1.contains(&SEMANTIC_RUNTIME_SETTING_KEY));
    assert!(
        CONFIGURATION_SETTING_KEYS_V1.contains(&PROXIMITY_RISK_THRESHOLD_SETTING_KEY_V1),
        "the proximity threshold must be available through the canonical registry"
    );
    let mut unique = BTreeSet::new();
    for key in CONFIGURATION_SETTING_KEYS_V1 {
        assert!(
            unique.insert(*key),
            "duplicate configuration setting key: {key}"
        );
        SettingKey::new(*key).expect("configuration key must be canonical");
        assert_ne!(*key, "root_dir", "path metadata is not durable authority");
    }
    for value in [
        ConfigurationValueV1::Boolean(true),
        ConfigurationValueV1::Unsigned(1),
        ConfigurationValueV1::StringList(vec!["src/**".to_owned()]),
    ] {
        value
            .validate()
            .expect("scalar setting uses an existing canonical value form");
    }
}

fn mutation_receipt() -> ConfigurationMutationGrantReceiptV1 {
    ConfigurationMutationGrantReceiptV1::issue(
        id::<ConfigurationGrantReceiptId>("configuration.grant-receipt.fixture"),
        id::<ConfigurationGrantId>("configuration.grant.fixture"),
        id::<ActorId>("actor.fixture"),
        ConfigurationMutationOperationV1::DirectMutation,
        digest('d'),
        id::<ConfigurationRevisionId>("configuration.revision.fixture"),
        7,
        AccessPolicyDigest::new(format!("sha256:{}", "e".repeat(64))).unwrap(),
        ConfigurationMutationSinkV1::ConfigurationStore,
        ConfigurationMutationEffectV1::CommitConfigurationRevision,
        Some(ConfigurationIdempotencyKey::new("configuration.idempotency.fixture").unwrap()),
        UtcMicros(10),
        UtcMicros(20),
    )
    .unwrap()
}

#[test]
fn mutation_receipt_rejects_expiry_and_binding_replay() {
    let receipt = mutation_receipt();
    assert!(
        receipt
            .validate_for(
                &receipt.actor_id,
                ConfigurationMutationOperationV1::DirectMutation,
                &receipt.scope_digest,
                &receipt.expected_configuration_revision,
                ConfigurationMutationSinkV1::ConfigurationStore,
                ConfigurationMutationEffectV1::CommitConfigurationRevision,
                UtcMicros(19),
            )
            .is_ok()
    );
    assert!(
        receipt
            .validate_for(
                &receipt.actor_id,
                ConfigurationMutationOperationV1::CredentialWrite,
                &receipt.scope_digest,
                &receipt.expected_configuration_revision,
                ConfigurationMutationSinkV1::CredentialStore,
                ConfigurationMutationEffectV1::WriteCredentialReference,
                UtcMicros(19),
            )
            .is_err()
    );
    assert!(
        receipt
            .validate_for(
                &receipt.actor_id,
                ConfigurationMutationOperationV1::DirectMutation,
                &receipt.scope_digest,
                &receipt.expected_configuration_revision,
                ConfigurationMutationSinkV1::ConfigurationStore,
                ConfigurationMutationEffectV1::CommitConfigurationRevision,
                UtcMicros(20),
            )
            .is_err()
    );
}

#[test]
fn mutation_receipt_digest_rejects_a_swapped_direct_idempotency_key() {
    let mut receipt = mutation_receipt();
    receipt.idempotency_key =
        Some(ConfigurationIdempotencyKey::new("configuration.idempotency.tampered").unwrap());

    assert!(matches!(
        receipt.validate(),
        Err(tracedecay_domain::DomainError::DigestMismatch)
    ));
}

#[test]
fn mutation_receipt_rejects_tampered_policy_or_scope() {
    let receipt = mutation_receipt();
    let mut tampered = serde_json::to_value(&receipt).unwrap();
    tampered["policy_epoch"] = serde_json::json!(8);
    assert!(
        serde_json::from_value::<ConfigurationMutationGrantReceiptV1>(tampered)
            .unwrap()
            .validate()
            .is_err()
    );

    let mut tampered = receipt;
    tampered.scope_digest = digest('f');
    assert!(tampered.validate().is_err());
}
