use std::collections::BTreeMap;

use serde::Serialize;
use thiserror::Error;

use crate::{
    CancellationContract, CatalogDigest, EffectClass, PaginationContract, StreamingContract,
};

pub const MCP_DISPATCH_CONTRACT_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum McpDispatchAvailability {
    Available,
    Unavailable {
        reason: McpDispatchUnavailableReason,
        retryable: bool,
    },
}

impl McpDispatchAvailability {
    pub const fn is_available(&self) -> bool {
        matches!(self, Self::Available)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum McpDispatchUnavailableReason {
    EffectJourneyUnverified,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum McpIdempotencyContract {
    NotProvided,
    Idempotent,
    KeyRequired,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "mode")]
pub enum McpInverseContract {
    NotApplicable,
    Unavailable { reason: McpInverseUnavailableReason },
    Tool { tool_name: String },
    SameTool { action: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum McpInverseUnavailableReason {
    NoVerifiedInverse,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum McpTerminalState {
    Completed,
    Cancelled,
    DeadlineExceeded,
    Denied,
    Failed,
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct McpDeadlineContractV1 {
    maximum_millis: u64,
}

impl McpDeadlineContractV1 {
    pub fn new(maximum_millis: u64) -> Result<Self, McpDispatchCatalogError> {
        if maximum_millis == 0 {
            return Err(McpDispatchCatalogError::InvalidDeadline);
        }
        Ok(Self { maximum_millis })
    }

    pub const fn maximum_millis(self) -> u64 {
        self.maximum_millis
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct McpDispatchContractInputV1 {
    pub tool_name: String,
    pub availability: McpDispatchAvailability,
    pub effect: EffectClass,
    pub deadline: McpDeadlineContractV1,
    pub idempotency: McpIdempotencyContract,
    pub inverse: McpInverseContract,
    pub cancellation: CancellationContract,
    pub terminal_states: Vec<McpTerminalState>,
    pub pagination: Option<PaginationContract>,
    pub streaming: Option<StreamingContract>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct McpDispatchContractV1 {
    tool_name: String,
    availability: McpDispatchAvailability,
    effect: EffectClass,
    read_only: bool,
    deadline: McpDeadlineContractV1,
    idempotency: McpIdempotencyContract,
    inverse: McpInverseContract,
    cancellation: CancellationContract,
    terminal_states: Vec<McpTerminalState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pagination: Option<PaginationContract>,
    #[serde(skip_serializing_if = "Option::is_none")]
    streaming: Option<StreamingContract>,
}

impl McpDispatchContractV1 {
    pub fn new(mut input: McpDispatchContractInputV1) -> Result<Self, McpDispatchCatalogError> {
        if input.tool_name.is_empty() {
            return Err(McpDispatchCatalogError::EmptyToolName);
        }
        if input.terminal_states.is_empty() {
            return Err(McpDispatchCatalogError::MissingTerminalStates {
                tool_name: input.tool_name,
            });
        }
        input.terminal_states.sort_unstable();
        if input
            .terminal_states
            .windows(2)
            .any(|states| states[0] == states[1])
        {
            return Err(McpDispatchCatalogError::DuplicateTerminalState {
                tool_name: input.tool_name,
            });
        }
        for required in [
            McpTerminalState::Completed,
            McpTerminalState::DeadlineExceeded,
            McpTerminalState::Denied,
            McpTerminalState::Failed,
            McpTerminalState::Unavailable,
        ] {
            if input.terminal_states.binary_search(&required).is_err() {
                return Err(McpDispatchCatalogError::IncompleteTerminalStates {
                    tool_name: input.tool_name,
                    missing: required,
                });
            }
        }
        let cancellable = matches!(input.cancellation, CancellationContract::Cooperative { .. });
        if input
            .terminal_states
            .binary_search(&McpTerminalState::Cancelled)
            .is_ok()
            != cancellable
        {
            return Err(McpDispatchCatalogError::InvalidCancellationTerminal {
                tool_name: input.tool_name,
            });
        }
        if input.effect.is_read_only() != matches!(input.inverse, McpInverseContract::NotApplicable)
        {
            return Err(McpDispatchCatalogError::InvalidInverse {
                tool_name: input.tool_name,
            });
        }
        Ok(Self {
            read_only: input.effect.is_read_only(),
            tool_name: input.tool_name,
            availability: input.availability,
            effect: input.effect,
            deadline: input.deadline,
            idempotency: input.idempotency,
            inverse: input.inverse,
            cancellation: input.cancellation,
            terminal_states: input.terminal_states,
            pagination: input.pagination,
            streaming: input.streaming,
        })
    }

    pub fn tool_name(&self) -> &str {
        &self.tool_name
    }

    pub const fn availability(&self) -> &McpDispatchAvailability {
        &self.availability
    }

    pub const fn effect(&self) -> EffectClass {
        self.effect
    }

    pub const fn read_only(&self) -> bool {
        self.read_only
    }

    pub const fn deadline(&self) -> McpDeadlineContractV1 {
        self.deadline
    }

    pub const fn idempotency(&self) -> McpIdempotencyContract {
        self.idempotency
    }

    pub const fn inverse(&self) -> &McpInverseContract {
        &self.inverse
    }

    pub const fn cancellation(&self) -> &CancellationContract {
        &self.cancellation
    }

    pub fn terminal_states(&self) -> &[McpTerminalState] {
        &self.terminal_states
    }

    pub const fn pagination(&self) -> Option<&PaginationContract> {
        self.pagination.as_ref()
    }

    pub const fn streaming(&self) -> Option<&StreamingContract> {
        self.streaming.as_ref()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpDispatchCatalogV1 {
    contracts: BTreeMap<String, McpDispatchContractV1>,
    fingerprint: CatalogDigest,
}

impl McpDispatchCatalogV1 {
    pub fn new(
        contracts: impl IntoIterator<Item = McpDispatchContractV1>,
    ) -> Result<Self, McpDispatchCatalogError> {
        let mut by_name = BTreeMap::new();
        for contract in contracts {
            let tool_name = contract.tool_name.clone();
            if by_name.insert(tool_name.clone(), contract).is_some() {
                return Err(McpDispatchCatalogError::DuplicateToolName { tool_name });
            }
        }
        if by_name.is_empty() {
            return Err(McpDispatchCatalogError::EmptyCatalog);
        }
        let canonical = serde_json::to_vec(&by_name)
            .map_err(|error| McpDispatchCatalogError::Serialization(error.to_string()))?;
        Ok(Self {
            contracts: by_name,
            fingerprint: CatalogDigest::sha256(canonical),
        })
    }

    pub const fn version(&self) -> u32 {
        MCP_DISPATCH_CONTRACT_VERSION
    }

    pub const fn fingerprint(&self) -> CatalogDigest {
        self.fingerprint
    }

    pub fn contract(&self, tool_name: &str) -> Option<&McpDispatchContractV1> {
        self.contracts.get(tool_name)
    }

    pub fn contracts(&self) -> impl ExactSizeIterator<Item = &McpDispatchContractV1> {
        self.contracts.values()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum McpDispatchCatalogError {
    #[error("MCP dispatch catalog cannot be empty")]
    EmptyCatalog,
    #[error("MCP dispatch deadline must be greater than zero")]
    InvalidDeadline,
    #[error("MCP dispatch tool name cannot be empty")]
    EmptyToolName,
    #[error("MCP dispatch tool '{tool_name}' has no terminal states")]
    MissingTerminalStates { tool_name: String },
    #[error("MCP dispatch tool '{tool_name}' repeats a terminal state")]
    DuplicateTerminalState { tool_name: String },
    #[error("MCP dispatch tool '{tool_name}' omits terminal state {missing:?}")]
    IncompleteTerminalStates {
        tool_name: String,
        missing: McpTerminalState,
    },
    #[error("MCP dispatch tool '{tool_name}' cancellation and terminal states disagree")]
    InvalidCancellationTerminal { tool_name: String },
    #[error("MCP dispatch tool '{tool_name}' has an inverse inconsistent with its effect")]
    InvalidInverse { tool_name: String },
    #[error("MCP dispatch tool '{tool_name}' is declared more than once")]
    DuplicateToolName { tool_name: String },
    #[error("MCP dispatch catalog fingerprint serialization failed: {0}")]
    Serialization(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CancellationPoint;

    fn contract(name: &str) -> McpDispatchContractV1 {
        McpDispatchContractV1::new(McpDispatchContractInputV1 {
            tool_name: name.to_owned(),
            availability: McpDispatchAvailability::Available,
            effect: EffectClass::Read,
            deadline: McpDeadlineContractV1::new(1_000).unwrap(),
            idempotency: McpIdempotencyContract::NotProvided,
            inverse: McpInverseContract::NotApplicable,
            cancellation: CancellationContract::cooperative(vec![
                CancellationPoint::BeforeAdmission,
            ])
            .unwrap(),
            terminal_states: vec![
                McpTerminalState::Completed,
                McpTerminalState::Cancelled,
                McpTerminalState::DeadlineExceeded,
                McpTerminalState::Denied,
                McpTerminalState::Failed,
                McpTerminalState::Unavailable,
            ],
            pagination: None,
            streaming: None,
        })
        .unwrap()
    }

    #[test]
    fn catalog_fingerprint_is_order_independent() {
        let first = McpDispatchCatalogV1::new([contract("b"), contract("a")]).unwrap();
        let second = McpDispatchCatalogV1::new([contract("a"), contract("b")]).unwrap();
        assert_eq!(first.fingerprint(), second.fingerprint());
    }

    #[test]
    fn read_contract_rejects_callable_inverse() {
        let mut input = McpDispatchContractInputV1 {
            tool_name: "read".to_owned(),
            availability: McpDispatchAvailability::Available,
            effect: EffectClass::Read,
            deadline: McpDeadlineContractV1::new(1_000).unwrap(),
            idempotency: McpIdempotencyContract::NotProvided,
            inverse: McpInverseContract::Tool {
                tool_name: "write".to_owned(),
            },
            cancellation: CancellationContract::NotCancellable,
            terminal_states: vec![
                McpTerminalState::Completed,
                McpTerminalState::DeadlineExceeded,
                McpTerminalState::Denied,
                McpTerminalState::Failed,
                McpTerminalState::Unavailable,
            ],
            pagination: None,
            streaming: None,
        };
        assert!(matches!(
            McpDispatchContractV1::new(input.clone()),
            Err(McpDispatchCatalogError::InvalidInverse { .. })
        ));
        input.effect = EffectClass::Administrative;
        assert!(McpDispatchContractV1::new(input).is_ok());
    }
}
