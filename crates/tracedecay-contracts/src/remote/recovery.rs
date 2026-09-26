//! Application-facing promotion and rejoin contracts.
//!
//! Physical locators and authority storage are intentionally absent. Adapters
//! may present these records but cannot infer confirmation or promotion.

use serde::{Deserialize, Serialize};
use tracedecay_domain::UtcMicros;

use crate::error::ApplicationContractError;

use super::protocol::RemoteProtocolBodyV1;

mod service;

pub use service::{
    REMOTE_PROMOTION_USE_CASE_ID_V1, RemoteRecoveryCallerV1, RemoteRecoveryCommittedV1,
    RemoteRecoveryControlPortV1, RemoteRecoveryInterruptionV1, RemoteRecoveryOperationErrorV1,
    RemoteRecoveryOperationPortV1, RemoteRecoveryOperationReceiptV1, RemoteRecoveryProtocolOwnerV1,
    RemoteRecoveryTerminationV1, remote_promotion_result_contract_v1,
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RecoveryAuthorityExpectationV1 {
    pub brain_id: String,
    pub shard_id: String,
    pub generation_id: String,
    pub authority_node_id: String,
    pub placement_revision: u64,
    pub authority_epoch: u64,
}

impl RecoveryAuthorityExpectationV1 {
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        for (field, value) in [
            ("recovery brain id", self.brain_id.as_str()),
            ("recovery shard id", self.shard_id.as_str()),
            ("recovery generation id", self.generation_id.as_str()),
            (
                "recovery authority node id",
                self.authority_node_id.as_str(),
            ),
        ] {
            validate_identifier(field, value)?;
        }
        for (field, value) in [
            ("recovery placement revision", self.placement_revision),
            ("recovery authority epoch", self.authority_epoch),
        ] {
            if value == 0 {
                return Err(ApplicationContractError::ZeroValue { field });
            }
        }
        Ok(())
    }

    pub fn matches_writer(&self, writer: &tracedecay_domain::RemoteWriterFenceV1) -> bool {
        self.brain_id == writer.brain_id.as_str()
            && self.shard_id == writer.shard_id.as_str()
            && self.generation_id == writer.generation_id.as_str()
            && self.authority_node_id == writer.authority_node_id.as_str()
            && self.placement_revision == writer.placement_revision.get()
            && self.authority_epoch == writer.authority_epoch.0
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PromotionPreviewV1 {
    pub preview_id: String,
    pub expected: RecoveryAuthorityExpectationV1,
    pub replacement_epoch: u64,
    pub replacement_placement_revision: u64,
    pub required_sink_ids: Vec<String>,
    pub expires_at_micros: i64,
}

impl PromotionPreviewV1 {
    pub fn validate(&self, now_micros: i64) -> Result<(), ApplicationContractError> {
        validate_identifier("promotion preview id", &self.preview_id)?;
        self.expected.validate()?;
        if self.replacement_epoch <= self.expected.authority_epoch {
            return Err(ApplicationContractError::Inconsistent {
                field: "promotion replacement epoch",
            });
        }
        if self.replacement_placement_revision <= self.expected.placement_revision {
            return Err(ApplicationContractError::Inconsistent {
                field: "promotion replacement placement revision",
            });
        }
        if self.required_sink_ids.is_empty() {
            return Err(ApplicationContractError::Inconsistent {
                field: "promotion durable sinks",
            });
        }
        for sink in &self.required_sink_ids {
            validate_identifier("promotion durable sink id", sink)?;
        }
        if now_micros >= self.expires_at_micros {
            return Err(ApplicationContractError::InvalidRange {
                field: "promotion preview expiry",
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PromotionConfirmationV1 {
    pub preview_id: String,
    pub expected_authority_epoch: u64,
    pub expected_placement_revision: u64,
    pub expires_at_micros: i64,
}

impl PromotionConfirmationV1 {
    pub fn validate(&self, now_micros: i64) -> Result<(), ApplicationContractError> {
        validate_identifier("promotion preview id", &self.preview_id)?;
        if self.expected_authority_epoch == 0
            || self.expected_placement_revision == 0
            || now_micros >= self.expires_at_micros
        {
            return Err(ApplicationContractError::Inconsistent {
                field: "promotion confirmation",
            });
        }
        Ok(())
    }
}

impl RemoteProtocolBodyV1 for PromotionConfirmationV1 {
    fn validate_remote_protocol_body(
        &self,
        sent_at: UtcMicros,
    ) -> Result<(), ApplicationContractError> {
        self.validate(sent_at.0)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PromotionCasReceiptV1 {
    pub receipt_id: String,
    pub preview_id: String,
    pub previous_epoch: u64,
    pub installed_epoch: u64,
    pub installed_placement_revision: u64,
    pub installed_sink_ids: Vec<String>,
    pub published_frontier_sequence: u64,
    pub old_authority_fenced: bool,
}

impl PromotionCasReceiptV1 {
    pub fn validate_against(
        &self,
        preview: &PromotionPreviewV1,
    ) -> Result<(), ApplicationContractError> {
        validate_identifier("promotion receipt id", &self.receipt_id)?;
        if self.preview_id != preview.preview_id
            || self.previous_epoch != preview.expected.authority_epoch
            || self.installed_epoch != preview.replacement_epoch
            || self.installed_placement_revision != preview.replacement_placement_revision
            || !self.old_authority_fenced
        {
            return Err(ApplicationContractError::Inconsistent {
                field: "promotion receipt",
            });
        }
        if preview
            .required_sink_ids
            .iter()
            .any(|required| !self.installed_sink_ids.contains(required))
        {
            return Err(ApplicationContractError::Inconsistent {
                field: "promotion installed sinks",
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum AuthorityRejoinStateV1 {
    CurrentAuthority,
    FencedReadOnly {
        observed_higher_epoch: u64,
    },
    ReseedRequired {
        observed_higher_epoch: u64,
    },
    ReseedPreviewed {
        preview_id: String,
        observed_higher_epoch: u64,
    },
    Reseeding,
    RejoinedReadOnly,
}

impl AuthorityRejoinStateV1 {
    pub fn may_accept_writes(&self) -> bool {
        matches!(self, Self::CurrentAuthority)
    }
}

/// Delegates to the crate-shared bounded-identifier validator in
/// [`crate::identity`] instead of re-implementing the same empty/trim/
/// control-character/length checks locally. Recovery identifiers keep their
/// existing 512-byte bound.
fn validate_identifier(field: &'static str, value: &str) -> Result<(), ApplicationContractError> {
    crate::identity::validate_identifier(value, field, 512)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expectation() -> RecoveryAuthorityExpectationV1 {
        RecoveryAuthorityExpectationV1 {
            brain_id: "brain.remote".into(),
            shard_id: "shard.profile".into(),
            generation_id: "generation.7".into(),
            authority_node_id: "node.authority".into(),
            placement_revision: 4,
            authority_epoch: 8,
        }
    }

    #[test]
    fn promotion_must_advance_epoch_and_placement() {
        let preview = PromotionPreviewV1 {
            preview_id: "promotion.1".into(),
            expected: expectation(),
            replacement_epoch: 8,
            replacement_placement_revision: 5,
            required_sink_ids: vec!["writer".into()],
            expires_at_micros: 20,
        };
        assert!(preview.validate(10).is_err());
    }

    #[test]
    fn old_authority_stays_read_only_after_rejoin() {
        for state in [
            AuthorityRejoinStateV1::FencedReadOnly {
                observed_higher_epoch: 9,
            },
            AuthorityRejoinStateV1::ReseedRequired {
                observed_higher_epoch: 9,
            },
            AuthorityRejoinStateV1::RejoinedReadOnly,
        ] {
            assert!(!state.may_accept_writes());
        }
    }

    #[test]
    fn promotion_confirmation_requires_exact_expectations() {
        let mut promotion = PromotionConfirmationV1 {
            preview_id: "promotion.1".into(),
            expected_authority_epoch: 8,
            expected_placement_revision: 4,
            expires_at_micros: 20,
        };
        assert!(promotion.validate(10).is_ok());
        assert!(promotion.validate(20).is_err());
        promotion.expected_placement_revision = 0;
        assert!(promotion.validate(10).is_err());
    }
}
