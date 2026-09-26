use std::collections::{BTreeMap, BTreeSet};

use tracedecay_domain::configuration::{
    AccessRuleId, AuthorityRef, CandidateDispositionV1, CapabilityResolutionContextV1,
    ConfigurationCandidateV1, ConfigurationGrantId, ConfigurationGrantReceiptId,
    ConfigurationIdempotencyKey, ConfigurationLayerIdV1, ConfigurationMutationEffectV1,
    ConfigurationMutationGrantReceiptV1, ConfigurationMutationOperationV1,
    ConfigurationMutationSinkV1, ConfigurationRevisionId, ConfigurationSnapshotV1,
    ConfigurationValueV1, ProtectedChange, RuleEffect, SOURCE_BINDINGS_SETTING_KEY,
    ScopeAccessRule, ScopeAccessSubjectV1, ScopeSourceBinding, SettingKey, SourceBindingId,
    SourceKindV1, UserProfileId, resolve_restrictive_capabilities,
};
use tracedecay_domain::{
    AccessPolicyDigest, ActorId, CapabilityId, LocatorDigest, ProjectId, UtcMicros,
};

use tracedecay_domain::test_fixtures::id;

use tracedecay_domain::test_fixtures::digest;

fn locator_digest(byte: char) -> LocatorDigest {
    LocatorDigest::new(format!("sha256:{}", byte.to_string().repeat(64)))
        .expect("fixture digest is canonical")
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

/// A bound source whose id sorts before an existing binding lands in
/// canonical order, so the resulting snapshot validates.
#[test]
fn bind_source_inserts_in_canonical_binding_order() {
    let project = AuthorityRef::Project(id::<ProjectId>("project.bind-order"));
    let daemon = ScopeSourceBinding::new(
        id::<SourceBindingId>("binding.tracedecay-daemon.project-open"),
        SourceKindV1::Cursor,
        locator_digest('a'),
        project.clone(),
    )
    .unwrap();
    let github = ScopeSourceBinding::new(
        id::<SourceBindingId>("binding.github.rust-lang-log"),
        SourceKindV1::GitHub,
        locator_digest('b'),
        project,
    )
    .unwrap();
    let key = SettingKey::new(SOURCE_BINDINGS_SETTING_KEY).unwrap();
    let snapshot = ConfigurationSnapshotV1::new(
        BTreeMap::from([(
            key.clone(),
            ConfigurationValueV1::SourceBindings(vec![daemon.clone()]),
        )]),
        BTreeMap::from([(
            key.clone(),
            vec![ConfigurationCandidateV1 {
                layer: ConfigurationLayerIdV1::Default,
                revision_id: id::<ConfigurationRevisionId>("configuration.revision.bind-base"),
                disposition: CandidateDispositionV1::Winning,
                safe_reason: None,
            }],
        )]),
    )
    .unwrap();

    let bound = snapshot
        .apply_protected_change(
            &ProtectedChange::BindSource(github.clone()),
            &id::<ConfigurationRevisionId>("configuration.revision.bind-order"),
        )
        .expect("an out-of-order binding id still binds");

    assert_eq!(
        bound.effective_values.get(&key),
        Some(&ConfigurationValueV1::SourceBindings(vec![github, daemon]))
    );
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
                ConfigurationMutationOperationV1::ProtectedApply,
                &receipt.scope_digest,
                &receipt.expected_configuration_revision,
                ConfigurationMutationSinkV1::ConfigurationStore,
                ConfigurationMutationEffectV1::CommitConfigurationRevision,
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
