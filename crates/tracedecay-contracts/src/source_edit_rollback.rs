use serde::Serialize;
use tracedecay_domain::{ManifestDigest, UtcMicros, canonical_sha256};
use tracedecay_tool_catalog::{CapabilityId, SchemaId, SchemaRef, UseCaseId};

use crate::error::ApplicationContractError;
use crate::handlers::ApplicationOperation;
use crate::result::{AuthorityReceipt, EffectId, IdempotencyKey, ResultContractRef};
use crate::source_edit::SourceEditEffectProofV1;
use crate::{RequestAdmission, RequestContext};

const SOURCE_EDIT_ROLLBACK_REQUEST_DIGEST_DOMAIN_V1: &str =
    "tracedecay.application.source-edit-rollback-request.v1";

/// Request to restore the exact private preimages retained for one completed
/// source edit. Callers identify the original effect and its public digests;
/// source bytes remain confined to the server-side rollback record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SourceEditRollbackRequestV1 {
    pub context: RequestContext,
    pub authority: AuthorityReceipt,
    pub effect_id: EffectId,
    pub original_idempotency_key: IdempotencyKey,
    pub idempotency_key: IdempotencyKey,
    pub original_input_digest: ManifestDigest,
    pub expected_state: ManifestDigest,
    pub proof: SourceEditEffectProofV1,
    pub observed_at: UtcMicros,
}

impl SourceEditRollbackRequestV1 {
    pub fn input_digest(&self) -> Result<ManifestDigest, ApplicationContractError> {
        self.validate()?;
        Ok(canonical_sha256(&(
            SOURCE_EDIT_ROLLBACK_REQUEST_DIGEST_DOMAIN_V1,
            self.context.actor(),
            self.context.scope(),
            &self.effect_id,
            &self.original_idempotency_key,
            &self.idempotency_key,
            &self.original_input_digest,
            &self.expected_state,
            &self.proof.external_proof,
        ))?)
    }

    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        self.context.validate()?;
        self.authority.validate_for(self.context.scope())?;
        self.original_input_digest.validate()?;
        self.expected_state.validate()?;
        if self.idempotency_key == self.original_idempotency_key {
            return Err(ApplicationContractError::Inconsistent {
                field: "source edit rollback idempotency key",
            });
        }
        self.proof.validate_for(&self.authority)?;
        let operation = source_edit_rollback_operation()?;
        if self.context.admission_at(self.observed_at) != RequestAdmission::Admitted
            || !self
                .context
                .allows(operation.capability_id(), operation.use_case_id())
        {
            return Err(ApplicationContractError::Inconsistent {
                field: "source edit rollback admission",
            });
        }
        let grant = self.context.grant();
        if self.authority.grant_id != grant.grant_id
            || self.authority.grant_revision != grant.revision
            || self.authority.grant_digest != grant.digest
        {
            return Err(ApplicationContractError::Inconsistent {
                field: "source edit rollback current grant",
            });
        }
        Ok(())
    }
}

pub fn source_edit_rollback_operation() -> Result<ApplicationOperation, ApplicationContractError> {
    let result_schema = source_edit_rollback_schema("result")?;
    Ok(ApplicationOperation::new(
        CapabilityId::new("capability.application.source-edit.rollback")?,
        UseCaseId::new("use-case.application.source-edit.rollback")?,
        ResultContractRef::from_schema(&result_schema),
        true,
    ))
}

pub(crate) fn source_edit_rollback_schema(
    suffix: &str,
) -> Result<SchemaRef, ApplicationContractError> {
    Ok(SchemaRef::new(
        SchemaId::new(format!("schema.application.source-edit.rollback.{suffix}"))?,
        1,
    )?)
}
