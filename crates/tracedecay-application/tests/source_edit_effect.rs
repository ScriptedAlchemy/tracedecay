mod common;

use tracedecay_application::{
    EffectId, IdempotencyKey, RenamePreviewAcceptanceV1, RenameSymbolBindingV1,
    SourceEditEffectProofV1, SourceEditEffectRequestV1, SourceEditKind,
    SourceEditReconciliationDispositionV1, SourceEditReconciliationRequestV1, SourceEditRequest,
    source_edit_operation, source_edit_reconciliation_operation,
};
use tracedecay_domain::configuration::ConfigurationRevisionId;
use tracedecay_domain::{PrivacyDomainId, UtcMicros};

fn request() -> SourceEditEffectRequestV1 {
    let operation = source_edit_operation(SourceEditKind::StrReplace).unwrap();
    let context = common::context(&operation);
    SourceEditEffectRequestV1 {
        authority: common::authority(&context),
        context,
        edit: SourceEditRequest::StrReplace {
            path: "src/lib.rs".to_owned(),
            old_str: "old".to_owned(),
            new_str: "new".to_owned(),
            dry_run: false,
            verify: true,
        },
        idempotency_key: IdempotencyKey::new("source-edit.fixture").unwrap(),
        expected_state: common::digest(common::SHA256_A),
        proof: SourceEditEffectProofV1 {
            policy_digest: common::digest(common::SHA256_B),
            configuration_revision_id: common::id::<ConfigurationRevisionId>(
                "configuration.revision.source-edit.fixture",
            ),
            configuration_digest: common::digest(common::SHA256_A),
            catalog_revision: 1,
            catalog_digest: common::digest(common::SHA256_A),
            privacy_domain_id: common::id::<PrivacyDomainId>("privacy.source-edit.fixture"),
            privacy_key_epoch: 1,
            privacy_digest: common::digest(common::SHA256_A),
            external_proof: None,
        },
        observed_at: UtcMicros(3),
    }
}

#[test]
fn source_edit_effect_requires_the_exact_current_grant() {
    let mut request = request();
    request.authority.grant_revision += 1;

    assert!(request.validate().is_err());
}

#[test]
fn source_edit_effect_rejects_zero_catalog_revision() {
    let mut request = request();
    request.proof.catalog_revision = 0;

    assert!(request.validate().is_err());
}

#[test]
fn source_edit_effect_rejects_zero_privacy_key_epoch() {
    let mut request = request();
    request.proof.privacy_key_epoch = 0;

    assert!(request.validate().is_err());
}

#[test]
fn rename_apply_rejects_a_stale_or_missing_preview_digest() {
    let mut request = request();
    let operation = source_edit_operation(SourceEditKind::RenameSymbol).unwrap();
    request.context = common::context(&operation);
    request.authority = common::authority(&request.context);
    request.edit = SourceEditRequest::RenameSymbol {
        binding: RenameSymbolBindingV1 {
            node_id: "node.fixture".to_owned(),
            qualified_name: "crate::old".to_owned(),
            kind: "function".to_owned(),
            file: "src/lib.rs".to_owned(),
            old_name: "old".to_owned(),
            accepted_preview: None,
        },
        new_name: "new".to_owned(),
        dry_run: false,
        verify: true,
    };
    assert!(request.validate().is_err());

    if let SourceEditRequest::RenameSymbol { binding, .. } = &mut request.edit {
        binding.accepted_preview = Some(RenamePreviewAcceptanceV1 {
            preview_id: common::digest(common::SHA256_A),
            preview_digest: common::digest(common::SHA256_B),
            plan_digest: common::digest(common::SHA256_A),
            repository_revision: Some("0123456789abcdef".to_owned()),
            graph_revision: common::digest(common::SHA256_B),
        });
    } else {
        unreachable!("rename test request");
    }
    assert!(request.validate().is_err());

    let SourceEditRequest::RenameSymbol { binding, .. } = &mut request.edit else {
        unreachable!("rename test request");
    };
    binding.accepted_preview.as_mut().unwrap().preview_digest = request.expected_state.clone();
    assert!(request.validate().is_ok());
}

#[test]
fn idempotency_digest_excludes_volatile_revalidation_evidence() {
    let request = request();
    let expected = request.input_digest().unwrap();
    let mut revalidated = request.clone();
    revalidated.observed_at = UtcMicros(4);
    revalidated.authority.revalidated_at = UtcMicros(4);
    revalidated.proof.configuration_digest = common::digest(common::SHA256_B);
    revalidated.proof.catalog_digest = common::digest(common::SHA256_B);
    revalidated.proof.privacy_digest = common::digest(common::SHA256_B);

    assert_eq!(revalidated.input_digest().unwrap(), expected);

    revalidated.expected_state = common::digest(common::SHA256_B);
    assert_ne!(revalidated.input_digest().unwrap(), expected);
}

#[test]
fn reconciliation_requires_its_distinct_current_capability() {
    let effect = request();
    let input_digest = effect.input_digest().unwrap();
    let operation = source_edit_reconciliation_operation().unwrap();
    let context = common::context(&operation);
    let request = SourceEditReconciliationRequestV1 {
        authority: common::authority(&context),
        context,
        kind: SourceEditKind::StrReplace,
        effect_id: EffectId::new("effect.source-edit.fixture").unwrap(),
        idempotency_key: effect.idempotency_key.clone(),
        attempt_idempotency_key: IdempotencyKey::new("reconcile-attempt.fixture").unwrap(),
        input_digest,
        disposition: SourceEditReconciliationDispositionV1::ConfirmRolledBack,
        proof: SourceEditEffectProofV1 {
            policy_digest: common::digest(common::SHA256_B),
            configuration_revision_id: common::id::<ConfigurationRevisionId>(
                "configuration.revision.source-edit.fixture",
            ),
            configuration_digest: common::digest(common::SHA256_A),
            catalog_revision: 1,
            catalog_digest: common::digest(common::SHA256_A),
            privacy_domain_id: common::id::<PrivacyDomainId>("privacy.source-edit.fixture"),
            privacy_key_epoch: 1,
            privacy_digest: common::digest(common::SHA256_A),
            external_proof: None,
        },
        observed_at: UtcMicros(3),
    };

    assert!(request.validate().is_ok());
    let attempt_digest = request.attempt_input_digest().unwrap();
    let mut changed_disposition = request.clone();
    changed_disposition.disposition = SourceEditReconciliationDispositionV1::ConfirmCommitted {
        committed_state: common::digest(common::SHA256_B),
    };
    assert_ne!(
        changed_disposition.attempt_input_digest().unwrap(),
        attempt_digest
    );

    let mut reused_original_key = request.clone();
    reused_original_key.attempt_idempotency_key = reused_original_key.idempotency_key.clone();
    assert!(reused_original_key.validate().is_err());

    let mut wrong_capability = request;
    wrong_capability.context = effect.context;
    wrong_capability.authority = effect.authority;
    assert!(wrong_capability.validate().is_err());
}
