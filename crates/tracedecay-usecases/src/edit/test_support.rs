#![cfg(test)]

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicUsize, Ordering};

use tracedecay_application::{
    ApplicationOperation, AuthorityReceipt, CancellationContext, CancellationSignal,
    CapabilityGrantSnapshot, Deadline, DisclosureClass, IdempotencyKey, PolicyDecisionRef,
    RequestContext, RequestId, ResolvedScope, SourceEditAuthorizationFuture,
    SourceEditAuthorizationPort, SourceEditEffectProofV1, SourceEditEffectRequestV1,
    SourceEditKind, SourceEditReconciliationDispositionV1, SourceEditReconciliationRequestV1,
    SourceEditRequest, source_edit_operation, source_edit_reconciliation_operation,
};
use tracedecay_domain::{
    ActorId, ComponentVersion, ManifestDigest, ProjectId, RepositoryId, UtcMicros, WorktreeId,
};

use super::JOURNAL_VERSION;
use super::digest::effect_id;
use super::journal::{SourceEditDurableRequestV1, SourceEditJournalStateV1, SourceEditJournalV1};

pub(super) const SHA256_A: &str =
    "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
pub(super) const SHA256_B: &str =
    "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

pub(super) fn digest(value: &str) -> ManifestDigest {
    ManifestDigest::new(value).unwrap()
}

pub(super) fn fixture_request() -> SourceEditEffectRequestV1 {
    let operation = source_edit_operation(SourceEditKind::StrReplace).unwrap();
    let reconciliation_operation = source_edit_reconciliation_operation().unwrap();
    let scope = ResolvedScope::new(
        ProjectId::new("project.edit.fixture").unwrap(),
        RepositoryId::new("repository.edit.fixture").unwrap(),
        WorktreeId::new("worktree.edit.fixture").unwrap(),
        None,
    )
    .unwrap();
    let grant = CapabilityGrantSnapshot::new(
        tracedecay_application::CapabilityGrantId::new("grant.edit.fixture").unwrap(),
        1,
        digest(SHA256_A),
        ActorId::new("actor.edit.issuer").unwrap(),
        UtcMicros(1),
        UtcMicros(1_000),
        scope.clone(),
        BTreeSet::from([
            operation.capability_id().clone(),
            reconciliation_operation.capability_id().clone(),
        ]),
        BTreeSet::from([
            operation.use_case_id().clone(),
            reconciliation_operation.use_case_id().clone(),
        ]),
        DisclosureClass::Sensitive,
    )
    .unwrap();
    let context = RequestContext::new(
        ActorId::new("actor.edit.requester").unwrap(),
        scope,
        grant,
        RequestId::new("request.edit.fixture").unwrap(),
        Deadline::new(UtcMicros(900)).unwrap(),
        CancellationContext::active("cancel.edit.fixture").unwrap(),
    )
    .unwrap();
    let authority = AuthorityReceipt::from_context(
        &context,
        PolicyDecisionRef::new(
            "policy.edit.fixture",
            1,
            digest(SHA256_B),
            ComponentVersion::new("policy.edit.v1").unwrap(),
        )
        .unwrap(),
        UtcMicros(2),
    )
    .unwrap();
    SourceEditEffectRequestV1 {
        context,
        authority,
        edit: SourceEditRequest::StrReplace {
            path: "src/lib.rs".to_owned(),
            old_str: "old".to_owned(),
            new_str: "new".to_owned(),
            dry_run: false,
            verify: false,
        },
        idempotency_key: IdempotencyKey::new("source-edit.fixture").unwrap(),
        expected_state: digest(SHA256_A),
        proof: SourceEditEffectProofV1 {
            policy_digest: digest(SHA256_B),
            configuration_revision_id:
                tracedecay_domain::configuration::ConfigurationRevisionId::new(
                    "configuration.edit.fixture.v1",
                )
                .unwrap(),
            configuration_digest: digest(SHA256_A),
            catalog_revision: 1,
            catalog_digest: digest(SHA256_A),
            privacy_domain_id: tracedecay_domain::PrivacyDomainId::new("privacy.edit.fixture")
                .unwrap(),
            privacy_key_epoch: 1,
            privacy_digest: digest(SHA256_A),
            external_proof: None,
        },
        observed_at: UtcMicros(3),
    }
}

pub(super) fn fixture_journal(
    request: &SourceEditEffectRequestV1,
    state: SourceEditJournalStateV1,
) -> SourceEditJournalV1 {
    let operation = source_edit_operation(request.edit.kind()).unwrap();
    let input_digest = request.input_digest().unwrap();
    SourceEditJournalV1 {
        version: JOURNAL_VERSION,
        effect_id: effect_id(&request.idempotency_key, &input_digest).unwrap(),
        input_digest,
        expected_state: request.expected_state.clone(),
        predicted_state: None,
        candidate_files: vec!["src/lib.rs".to_owned()],
        recovery_files: Vec::new(),
        recovery_digest: None,
        request: SourceEditDurableRequestV1 {
            operation: operation.use_case_id().clone(),
            request_id: request.context.request_id().clone(),
            actor: request.context.actor().clone(),
            scope: request.context.scope().clone(),
            authority: request.authority.clone(),
            authority_proof: request.proof.clone(),
            idempotency_key: request.idempotency_key.clone(),
            deadline: request.context.deadline().clone(),
            started_at: request.observed_at,
            dry_run: request.edit.dry_run(),
            verification_requested: request.edit.verify(),
        },
        state,
    }
}

pub(super) fn fixture_reconciliation(
    request: &SourceEditEffectRequestV1,
    journal: &SourceEditJournalV1,
    disposition: SourceEditReconciliationDispositionV1,
) -> SourceEditReconciliationRequestV1 {
    SourceEditReconciliationRequestV1 {
        context: request.context.clone(),
        authority: request.authority.clone(),
        kind: request.edit.kind(),
        effect_id: journal.effect_id.clone(),
        idempotency_key: request.idempotency_key.clone(),
        attempt_idempotency_key: tracedecay_application::IdempotencyKey::new(
            "source-edit-reconciliation-attempt.fixture",
        )
        .unwrap(),
        input_digest: journal.input_digest.clone(),
        disposition,
        proof: request.proof.clone(),
        observed_at: UtcMicros(4),
    }
}

#[derive(Clone)]
pub(super) struct FixtureSourceEditAuthorization(
    pub(super) tracedecay_application::SourceEditAuthorizationAdmissionV1,
);

pub(super) fn fixture_authorization(
    request: &SourceEditEffectRequestV1,
) -> FixtureSourceEditAuthorization {
    FixtureSourceEditAuthorization(
        tracedecay_application::SourceEditAuthorizationAdmissionV1::new(
            request.authority.clone(),
            request.proof.clone(),
            request.context.scope(),
        )
        .unwrap(),
    )
}

impl SourceEditAuthorizationPort for FixtureSourceEditAuthorization {
    fn admit<'a>(
        &'a self,
        _context: &'a RequestContext,
        _operation: &'a ApplicationOperation,
        _observed_at: UtcMicros,
    ) -> SourceEditAuthorizationFuture<'a> {
        Box::pin(async move { Ok(self.0.clone()) })
    }

    fn recheck_effect<'a>(
        &'a self,
        _context: &'a RequestContext,
        _operation: &'a ApplicationOperation,
        _admission: &'a tracedecay_application::SourceEditAuthorizationAdmissionV1,
        _observed_at: UtcMicros,
    ) -> SourceEditAuthorizationFuture<'a> {
        Box::pin(async move { Ok(self.0.clone()) })
    }
}

pub(super) struct CancelBeforeEffectAuthorization {
    pub(super) admission: tracedecay_application::SourceEditAuthorizationAdmissionV1,
    pub(super) cancellation: CancellationSignal,
    pub(super) rechecks: AtomicUsize,
}

impl SourceEditAuthorizationPort for CancelBeforeEffectAuthorization {
    fn admit<'a>(
        &'a self,
        _context: &'a RequestContext,
        _operation: &'a ApplicationOperation,
        _observed_at: UtcMicros,
    ) -> SourceEditAuthorizationFuture<'a> {
        Box::pin(async move { Ok(self.admission.clone()) })
    }

    fn recheck_effect<'a>(
        &'a self,
        _context: &'a RequestContext,
        _operation: &'a ApplicationOperation,
        _admission: &'a tracedecay_application::SourceEditAuthorizationAdmissionV1,
        _observed_at: UtcMicros,
    ) -> SourceEditAuthorizationFuture<'a> {
        Box::pin(async move {
            if self.rechecks.fetch_add(1, Ordering::AcqRel) == 1 {
                assert!(self.cancellation.cancel(UtcMicros(4)));
            }
            Ok(self.admission.clone())
        })
    }
}
