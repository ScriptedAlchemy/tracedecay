//! Versioned, bounded Remote Brain query-composition wire contract.
//!
//! This contract deliberately transports only composition evidence. Concrete
//! product query records remain owned by their established application API;
//! a remote response cannot smuggle an untyped JSON payload around those
//! contracts.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use tracedecay_domain::{RemoteRepositoryScopeV1, UtcMicros};

use super::composition::{ExpectedRemoteShardV1, RemoteQueryCompositionV1, ShardCoverageStateV1};
use super::protocol::RemoteProtocolBodyV1;
use crate::ApplicationContractError;

pub const REMOTE_QUERY_SCHEMA_REVISION_V1: u16 = 1;
pub const MAX_REMOTE_QUERY_PAGE_SIZE_V1: u16 = 100;
pub const MAX_REMOTE_QUERY_EXPECTED_SHARDS_V1: usize = 64;
pub const MAX_REMOTE_QUERY_CURSOR_BYTES_V1: usize = 512;

/// Bounded continuation metadata. The cursor is opaque to the caller and
/// bound by the serving owner to exact identity/generation inventory.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteQueryPageBoundsV1 {
    pub page_size: u16,
    pub cursor: Option<String>,
}

impl RemoteQueryPageBoundsV1 {
    pub fn new(page_size: u16, cursor: Option<String>) -> Result<Self, ApplicationContractError> {
        let value = Self { page_size, cursor };
        value.validate()?;
        Ok(value)
    }

    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        if self.page_size == 0 || self.page_size > MAX_REMOTE_QUERY_PAGE_SIZE_V1 {
            return Err(ApplicationContractError::InvalidRange {
                field: "remote query page size",
            });
        }
        if let Some(cursor) = &self.cursor
            && (cursor.is_empty()
                || cursor.len() > MAX_REMOTE_QUERY_CURSOR_BYTES_V1
                || cursor.trim() != cursor
                || cursor.chars().any(char::is_control))
        {
            return Err(ApplicationContractError::InvalidIdentifier {
                field: "remote query cursor",
            });
        }
        Ok(())
    }
}

/// Query only for authenticated composition/coverage of one exact repository
/// scope and explicitly expected immutable shard generations.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteQueryRequestV1 {
    pub schema_revision: u16,
    pub scope: RemoteRepositoryScopeV1,
    pub expected_shards: BTreeSet<ExpectedRemoteShardV1>,
    pub page: RemoteQueryPageBoundsV1,
}

impl RemoteQueryRequestV1 {
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        if self.schema_revision != REMOTE_QUERY_SCHEMA_REVISION_V1 {
            return Err(ApplicationContractError::Inconsistent {
                field: "remote query schema revision",
            });
        }
        self.scope.validate()?;
        self.page.validate()?;
        if self.expected_shards.is_empty()
            || self.expected_shards.len() > MAX_REMOTE_QUERY_EXPECTED_SHARDS_V1
        {
            return Err(ApplicationContractError::InvalidRange {
                field: "remote query expected shard inventory",
            });
        }
        for shard in &self.expected_shards {
            for (field, value) in [
                ("remote query Brain identity", shard.brain_id.as_str()),
                ("remote query shard identity", shard.shard_id.as_str()),
                (
                    "remote query generation identity",
                    shard.generation_id.as_str(),
                ),
            ] {
                if value.is_empty()
                    || value.len() > 512
                    || value.trim() != value
                    || value.chars().any(char::is_control)
                {
                    return Err(ApplicationContractError::InvalidIdentifier { field });
                }
            }
        }
        Ok(())
    }
}

impl RemoteProtocolBodyV1 for RemoteQueryRequestV1 {
    fn validate_remote_protocol_body(
        &self,
        _sent_at: UtcMicros,
    ) -> Result<(), ApplicationContractError> {
        self.validate()
    }
}

/// A wire-distinct marker that proves an authorized shard supplied a complete
/// query value. It must not collapse to JSON `null`, which is reserved for a
/// denied, partial, or unavailable contribution with no disclosable value.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteQueryCompleteValueV1 {
    pub complete_value_present: bool,
}

impl RemoteQueryCompleteValueV1 {
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        if !self.complete_value_present {
            return Err(ApplicationContractError::Inconsistent {
                field: "remote complete query value marker",
            });
        }
        Ok(())
    }
}

/// Canonical Remote Brain composition response. Per-shard `null` means no
/// disclosed value; a complete value is the explicit object above.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteQueryResultV1 {
    pub composition: RemoteQueryCompositionV1<RemoteQueryCompleteValueV1>,
}

impl RemoteQueryResultV1 {
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        for contribution in &self.composition.contributions {
            contribution.validate()?;
            if let Some(value) = &contribution.value {
                value.validate()?;
            }
            if contribution.coverage == ShardCoverageStateV1::Complete
                && contribution.value.is_none()
            {
                return Err(ApplicationContractError::Inconsistent {
                    field: "remote complete query value",
                });
            }
        }
        Ok(())
    }
}
