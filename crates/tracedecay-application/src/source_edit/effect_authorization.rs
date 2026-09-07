//! Durable source-edit effect inputs and authorization boundary.

use std::future::Future;
use std::pin::Pin;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracedecay_domain::configuration::ConfigurationRevisionId;
use tracedecay_domain::{
    ManifestDigest, PrivacyDomainId, RetrievalAnchorId, UtcMicros, canonical_sha256,
};

use super::{
    SourceEditKind, SourceEditRequest, source_edit_operation, source_edit_reconciliation_operation,
};
use crate::error::ApplicationContractError;
use crate::handlers::ApplicationOperation;
use crate::result::{ApplicationProblem, AuthorityReceipt, EffectId, IdempotencyKey};
use crate::{RequestAdmission, RequestContext, ResolvedScope};

const SOURCE_EDIT_EFFECT_REQUEST_DIGEST_DOMAIN_V1: &str =
    "tracedecay.application.source-edit-effect-request.v1";
const SOURCE_EDIT_RECONCILIATION_ATTEMPT_DIGEST_DOMAIN_V1: &str =
    "tracedecay.application.source-edit-reconciliation-attempt.v1";

/// Current sink evidence carried into a durable source-edit receipt.
///
/// The authority receipt is validated separately because it is refreshed at
/// admission and immediately before the effect. These digests bind the other
/// current authorities without persisting credentials or source text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceEditEffectProofV1 {
    pub policy_digest: ManifestDigest,
    pub configuration_revision_id: ConfigurationRevisionId,
    pub configuration_digest: ManifestDigest,
    pub catalog_revision: u32,
    pub catalog_digest: ManifestDigest,
    pub privacy_domain_id: PrivacyDomainId,
    pub privacy_key_epoch: u64,
    pub privacy_digest: ManifestDigest,
    pub external_proof: Option<RetrievalAnchorId>,
}

impl SourceEditEffectProofV1 {
    pub fn validate_for(
        &self,
        authority: &AuthorityReceipt,
    ) -> Result<(), ApplicationContractError> {
        self.policy_digest.validate()?;
        self.configuration_revision_id.validate()?;
        self.configuration_digest.validate()?;
        if self.catalog_revision == 0 {
            return Err(ApplicationContractError::ZeroValue {
                field: "source edit effect proof catalog revision",
            });
        }
        self.catalog_digest.validate()?;
        self.privacy_domain_id.validate()?;
        if self.privacy_key_epoch == 0 {
            return Err(ApplicationContractError::ZeroValue {
                field: "source edit effect proof privacy key epoch",
            });
        }
        self.privacy_digest.validate()?;
        self.external_proof
            .as_ref()
            .map_or(Ok(()), RetrievalAnchorId::validate)?;
        if self.policy_digest != authority.policy.digest {
            return Err(ApplicationContractError::Inconsistent {
                field: "source edit effect proof policy digest",
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceEditAuthorizationAdmissionV1 {
    pub receipt: AuthorityReceipt,
    pub proof: SourceEditEffectProofV1,
}

impl SourceEditAuthorizationAdmissionV1 {
    pub fn new(
        receipt: AuthorityReceipt,
        proof: SourceEditEffectProofV1,
        scope: &ResolvedScope,
    ) -> Result<Self, ApplicationContractError> {
        let admission = Self { receipt, proof };
        admission.validate_for(scope)?;
        Ok(admission)
    }

    pub fn validate_for(&self, scope: &ResolvedScope) -> Result<(), ApplicationContractError> {
        self.receipt.validate_for(scope)?;
        self.proof.validate_for(&self.receipt)
    }
}

/// Immutable, transport-neutral request for one preview or journaled edit.
///
/// `expected_state` is the caller-observed digest of every file the edit may
/// touch. The concrete edit authority independently captures those files and
/// rejects a mismatch before publishing its durable prepared journal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SourceEditEffectRequestV1 {
    pub context: RequestContext,
    pub authority: AuthorityReceipt,
    pub edit: SourceEditRequest,
    pub idempotency_key: IdempotencyKey,
    pub expected_state: ManifestDigest,
    pub proof: SourceEditEffectProofV1,
    pub observed_at: UtcMicros,
}

impl SourceEditEffectRequestV1 {
    pub fn input_digest(&self) -> Result<ManifestDigest, ApplicationContractError> {
        self.validate()?;
        Ok(canonical_sha256(&(
            SOURCE_EDIT_EFFECT_REQUEST_DIGEST_DOMAIN_V1,
            self.context.actor(),
            self.context.scope(),
            &self.edit,
            &self.idempotency_key,
            &self.expected_state,
            &self.proof.external_proof,
        ))?)
    }

    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        self.context.validate()?;
        self.authority.validate_for(self.context.scope())?;
        self.expected_state.validate()?;
        if let super::SourceEditRequest::RenameSymbol {
            binding, dry_run, ..
        } = &self.edit
        {
            match (*dry_run, binding.accepted_preview.as_ref()) {
                (true, None) => {}
                (false, Some(accepted)) if accepted.preview_digest == self.expected_state => {
                    accepted.validate()?;
                }
                (true, Some(_)) => {
                    return Err(ApplicationContractError::Inconsistent {
                        field: "rename preview acceptance on dry run",
                    });
                }
                (false, _) => {
                    return Err(ApplicationContractError::Inconsistent {
                        field: "rename exact accepted preview digest",
                    });
                }
            }
        }
        self.proof.validate_for(&self.authority)?;
        let operation = source_edit_operation(self.edit.kind())?;
        if self.context.admission_at(self.observed_at) != RequestAdmission::Admitted {
            return Err(ApplicationContractError::Inconsistent {
                field: "source edit request admission",
            });
        }
        if !self
            .context
            .allows(operation.capability_id(), operation.use_case_id())
        {
            return Err(ApplicationContractError::Inconsistent {
                field: "source edit request capability binding",
            });
        }
        let grant = self.context.grant();
        if self.authority.grant_id != grant.grant_id
            || self.authority.grant_revision != grant.revision
            || self.authority.grant_digest != grant.digest
        {
            return Err(ApplicationContractError::Inconsistent {
                field: "source edit request current grant",
            });
        }
        Ok(())
    }
}

/// Explicit conclusion supplied by an authorized reconciliation/inspection
/// operation. The concrete authority independently recaptures every candidate
/// file and accepts only an exact matching state digest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "disposition")]
pub enum SourceEditReconciliationDispositionV1 {
    ConfirmCommitted { committed_state: ManifestDigest },
    ConfirmRolledBack,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SourceEditReconciliationRequestV1 {
    pub context: RequestContext,
    pub authority: AuthorityReceipt,
    pub kind: SourceEditKind,
    pub effect_id: EffectId,
    pub idempotency_key: IdempotencyKey,
    pub attempt_idempotency_key: IdempotencyKey,
    pub input_digest: ManifestDigest,
    pub disposition: SourceEditReconciliationDispositionV1,
    pub proof: SourceEditEffectProofV1,
    pub observed_at: UtcMicros,
}

impl SourceEditReconciliationRequestV1 {
    pub fn attempt_input_digest(&self) -> Result<ManifestDigest, ApplicationContractError> {
        self.validate()?;
        Ok(canonical_sha256(&(
            SOURCE_EDIT_RECONCILIATION_ATTEMPT_DIGEST_DOMAIN_V1,
            self.context.actor(),
            self.context.scope(),
            self.kind,
            &self.effect_id,
            &self.idempotency_key,
            &self.attempt_idempotency_key,
            &self.input_digest,
            &self.disposition,
            &self.proof.external_proof,
        ))?)
    }

    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        self.context.validate()?;
        self.authority.validate_for(self.context.scope())?;
        self.input_digest.validate()?;
        if self.attempt_idempotency_key == self.idempotency_key {
            return Err(ApplicationContractError::Inconsistent {
                field: "source edit reconciliation attempt idempotency key",
            });
        }
        self.proof.validate_for(&self.authority)?;
        if let SourceEditReconciliationDispositionV1::ConfirmCommitted { committed_state } =
            &self.disposition
        {
            committed_state.validate()?;
        }
        let operation = source_edit_reconciliation_operation()?;
        if self.context.admission_at(self.observed_at) != RequestAdmission::Admitted
            || !self
                .context
                .allows(operation.capability_id(), operation.use_case_id())
        {
            return Err(ApplicationContractError::Inconsistent {
                field: "source edit reconciliation admission",
            });
        }
        let grant = self.context.grant();
        if self.authority.grant_id != grant.grant_id
            || self.authority.grant_revision != grant.revision
            || self.authority.grant_digest != grant.digest
        {
            return Err(ApplicationContractError::Inconsistent {
                field: "source edit reconciliation current grant",
            });
        }
        Ok(())
    }
}

pub type SourceEditAuthorizationFuture<'a> = Pin<
    Box<
        dyn Future<Output = Result<SourceEditAuthorizationAdmissionV1, ApplicationProblem>>
            + Send
            + 'a,
    >,
>;

/// Current source-edit authorization. Production adapters must reload their
/// policy/configuration authority for `recheck_effect`; retaining the
/// admission receipt alone is not a recheck.
pub trait SourceEditAuthorizationPort: Send + Sync {
    fn admit<'a>(
        &'a self,
        context: &'a RequestContext,
        operation: &'a ApplicationOperation,
        observed_at: UtcMicros,
    ) -> SourceEditAuthorizationFuture<'a>;

    fn recheck_effect<'a>(
        &'a self,
        context: &'a RequestContext,
        operation: &'a ApplicationOperation,
        admission: &'a SourceEditAuthorizationAdmissionV1,
        observed_at: UtcMicros,
    ) -> SourceEditAuthorizationFuture<'a>;
}
