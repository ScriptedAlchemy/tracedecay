//! Trusted operation identity for V1-facing memory use cases.

use tracedecay_domain::{ActorId, FactOwnerV1, ProvenanceId};

use crate::sessions::source::canonical_framed_sha256;

use super::error::MemoryApplicationError;
use crate::request_identity::{GlobalOpaqueIdentityKind, mint_global_opaque_id};

/// Trusted daemon-issued identity for one V1-facing operation. The raw
/// JSON-RPC identifier is never retained: it is domain-separated and hashed
/// with owner and action before it reaches the fact authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemoryOperationContext {
    operation_id: ProvenanceId,
    actor: Option<ActorId>,
}

impl MemoryOperationContext {
    pub fn from_trusted_request_id(
        owner: &FactOwnerV1,
        action: &str,
        request_id: &str,
        actor: Option<ActorId>,
    ) -> Result<Self, MemoryApplicationError> {
        owner.validate()?;
        validate_operation_component(action, "memory operation action")?;
        validate_operation_component(request_id, "memory request identity")?;
        if let Some(actor) = &actor {
            actor
                .validate()
                .map_err(|_| MemoryApplicationError::InvalidCompatibilityInput {
                    invariant: "memory operation actor",
                })?;
        }
        let owner = match owner {
            FactOwnerV1::Profile => "profile".to_owned(),
            FactOwnerV1::Project { project_id } => format!("project:{}", project_id.as_str()),
        };
        let digest = canonical_framed_sha256(
            b"tracedecay.memory.operation.v1",
            &[owner.as_bytes(), action.as_bytes(), request_id.as_bytes()],
        );
        let operation_id =
            ProvenanceId::new(format!("memory-operation.v1.{digest}")).map_err(|_| {
                MemoryApplicationError::InvalidCompatibilityInput {
                    invariant: "derived memory operation identity",
                }
            })?;
        Ok(Self {
            operation_id,
            actor,
        })
    }

    /// Use only for direct non-retriable core calls without a daemon request
    /// identity. Retriable transports must use [`Self::from_trusted_request_id`].
    pub fn generated(
        owner: &FactOwnerV1,
        action: &str,
        actor: Option<ActorId>,
    ) -> Result<Self, MemoryApplicationError> {
        let raw =
            mint_global_opaque_id(GlobalOpaqueIdentityKind::MemoryOperation).map_err(|_| {
                MemoryApplicationError::InvalidCompatibilityInput {
                    invariant: "generated memory operation identity",
                }
            })?;
        Self::from_trusted_request_id(owner, action, &raw, actor)
    }

    pub fn operation_id(&self) -> &ProvenanceId {
        &self.operation_id
    }

    pub fn actor(&self) -> Option<&ActorId> {
        self.actor.as_ref()
    }
}

pub(super) fn validate_operation_component(
    value: &str,
    invariant: &'static str,
) -> Result<(), MemoryApplicationError> {
    if value.is_empty() || value.len() > 512 || value.chars().any(char::is_control) {
        return Err(MemoryApplicationError::InvalidCompatibilityInput { invariant });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_identity_digest_matches_canonical_framed_sha256() {
        let context = MemoryOperationContext::from_trusted_request_id(
            &FactOwnerV1::Profile,
            "feedback",
            "fixture-feedback-mcp",
            None,
        )
        .unwrap();

        assert_eq!(
            context.operation_id().as_str(),
            "memory-operation.v1.178353d02133a655ee53c04806709a086671ac1e7a364969759cb3be8b810a4b"
        );
    }
}
