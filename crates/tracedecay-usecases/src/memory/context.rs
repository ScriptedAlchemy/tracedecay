//! Trusted operation identity for memory use cases.

use serde::Serialize;
use tracedecay_domain::{ActorId, FactOwnerV1, ProvenanceId, canonical_sha256};

use tracedecay_sessions::runtime::source::canonical_framed_sha256;

use super::error::MemoryApplicationError;
use crate::request_identity::{GlobalOpaqueIdentityKind, mint_global_opaque_id};

/// Trusted daemon-issued identity for one memory operation. The raw
/// JSON-RPC identifier is never retained: it is domain-separated and hashed
/// with owner and action before it reaches the fact authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemoryOperationContext {
    operation_id: ProvenanceId,
    actor: Option<ActorId>,
}

impl MemoryOperationContext {
    pub fn from_logical_effect<T: Serialize>(
        owner: &FactOwnerV1,
        action: &str,
        logical_effect: &T,
        actor: Option<ActorId>,
    ) -> Result<Self, MemoryApplicationError> {
        let owner = validated_owner(owner)?;
        validate_operation_component(action, "memory operation action")?;
        validate_actor(actor.as_ref())?;
        let effect_digest =
            canonical_sha256(logical_effect).map_err(|_| MemoryApplicationError::InvalidInput {
                invariant: "memory logical effect identity",
            })?;
        let digest = canonical_framed_sha256(
            b"tracedecay.memory.operation.effect",
            &[
                owner.as_bytes(),
                action.as_bytes(),
                effect_digest.as_str().as_bytes(),
            ],
        );
        let operation_id =
            ProvenanceId::new(format!("memory-operation.effect.{digest}")).map_err(|_| {
                MemoryApplicationError::InvalidInput {
                    invariant: "derived memory operation identity",
                }
            })?;
        Ok(Self {
            operation_id,
            actor,
        })
    }

    /// Derives an operation identity from a trusted transport request ID.
    /// Retriable writes must use [`Self::from_logical_effect`] instead.
    pub fn from_request_id(
        owner: &FactOwnerV1,
        action: &str,
        request_id: &str,
        actor: Option<ActorId>,
    ) -> Result<Self, MemoryApplicationError> {
        let owner = validated_owner(owner)?;
        validate_operation_component(action, "memory operation action")?;
        validate_operation_component(request_id, "memory request identity")?;
        validate_actor(actor.as_ref())?;
        let digest = canonical_framed_sha256(
            b"tracedecay.memory.operation.request",
            &[owner.as_bytes(), action.as_bytes(), request_id.as_bytes()],
        );
        let operation_id = ProvenanceId::new(format!("memory-operation.request.{digest}"))
            .map_err(|_| MemoryApplicationError::InvalidInput {
                invariant: "derived memory operation identity",
            })?;
        Ok(Self {
            operation_id,
            actor,
        })
    }

    /// Use only for direct non-retriable core calls without a daemon request
    /// identity. Retriable transports must use [`Self::from_logical_effect`].
    pub fn generated(
        owner: &FactOwnerV1,
        action: &str,
        actor: Option<ActorId>,
    ) -> Result<Self, MemoryApplicationError> {
        let raw =
            mint_global_opaque_id(GlobalOpaqueIdentityKind::MemoryOperation).map_err(|_| {
                MemoryApplicationError::InvalidInput {
                    invariant: "generated memory operation identity",
                }
            })?;
        Self::from_request_id(owner, action, &raw, actor)
    }

    pub fn operation_id(&self) -> &ProvenanceId {
        &self.operation_id
    }

    pub fn actor(&self) -> Option<&ActorId> {
        self.actor.as_ref()
    }
}

fn validated_owner(owner: &FactOwnerV1) -> Result<String, MemoryApplicationError> {
    owner.validate()?;
    Ok(match owner {
        FactOwnerV1::Profile => "profile".to_owned(),
        FactOwnerV1::Project { project_id } => format!("project:{}", project_id.as_str()),
    })
}

fn validate_actor(actor: Option<&ActorId>) -> Result<(), MemoryApplicationError> {
    actor.map_or(Ok(()), |actor| {
        actor
            .validate()
            .map_err(|_| MemoryApplicationError::InvalidInput {
                invariant: "memory operation actor",
            })
    })
}

pub(super) fn validate_operation_component(
    value: &str,
    invariant: &'static str,
) -> Result<(), MemoryApplicationError> {
    if value.is_empty() || value.len() > 512 || value.chars().any(char::is_control) {
        return Err(MemoryApplicationError::InvalidInput { invariant });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_identity_digest_matches_canonical_framed_sha256() {
        let context = MemoryOperationContext::from_request_id(
            &FactOwnerV1::Profile,
            "feedback",
            "fixture-feedback-mcp",
            None,
        )
        .unwrap();

        assert_eq!(
            context.operation_id().as_str(),
            "memory-operation.request.5882a5798c5caa7cfd90d3af658b7ad2226e146ef4926ebce0f9ab280c882363"
        );
    }

    #[test]
    fn logical_effect_identity_is_stable_and_content_bound() {
        let first = MemoryOperationContext::from_logical_effect(
            &FactOwnerV1::Profile,
            "add",
            &serde_json::json!({"content": "same fact"}),
            None,
        )
        .unwrap();
        let replay = MemoryOperationContext::from_logical_effect(
            &FactOwnerV1::Profile,
            "add",
            &serde_json::json!({"content": "same fact"}),
            None,
        )
        .unwrap();
        let distinct = MemoryOperationContext::from_logical_effect(
            &FactOwnerV1::Profile,
            "add",
            &serde_json::json!({"content": "different fact"}),
            None,
        )
        .unwrap();

        assert_eq!(first, replay);
        assert_ne!(first, distinct);
        assert!(
            first
                .operation_id()
                .as_str()
                .starts_with("memory-operation.effect.")
        );
    }
}
