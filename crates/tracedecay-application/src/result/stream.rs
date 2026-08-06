use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

use crate::error::ApplicationContractError;

use super::{OperationReceipt, OperationTermination};

/// Opaque authenticated continuation reference for a bounded stream.
#[derive(Clone, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct ResumeToken(String);

impl ResumeToken {
    pub fn new(value: impl Into<String>) -> Result<Self, ApplicationContractError> {
        let value = value.into();
        if value.is_empty()
            || value.trim() != value
            || value.len() > 4096
            || value.chars().any(char::is_control)
        {
            return Err(ApplicationContractError::InvalidIdentifier {
                field: "stream resume token",
            });
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for ResumeToken {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

impl fmt::Display for ResumeToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Monotonic frontier retained by a resumable adapter.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StreamFrontier {
    pub next_sequence: u64,
    pub retained_from_sequence: u64,
    pub resume_token: Option<ResumeToken>,
}

/// Explicit loss signal. Consumers cannot continue as though omitted events
/// had been delivered.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StreamGap {
    pub first_missing_sequence: u64,
    pub last_missing_sequence: u64,
    pub frontier: StreamFrontier,
}

impl StreamGap {
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        if self.first_missing_sequence > self.last_missing_sequence
            || self.frontier.next_sequence <= self.last_missing_sequence
        {
            return Err(ApplicationContractError::InvalidRange {
                field: "stream gap",
            });
        }
        Ok(())
    }
}

/// Receipt-bearing terminal stream state.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StreamTermination {
    pub termination: OperationTermination,
    pub receipt: OperationReceipt,
}

impl StreamTermination {
    pub fn completed(receipt: OperationReceipt) -> Self {
        Self {
            termination: OperationTermination::Completed,
            receipt,
        }
    }

    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        self.receipt.validate()?;
        if self.termination != self.receipt.termination {
            return Err(ApplicationContractError::Inconsistent {
                field: "stream terminal receipt",
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum StreamEventKind<T> {
    Item(T),
    Progress { completed: u64, total: Option<u64> },
    Gap(StreamGap),
    Terminal(StreamTermination),
}

impl<T> StreamEventKind<T> {
    fn is_terminal(&self) -> bool {
        matches!(self, Self::Terminal(_))
    }
}

/// One ordered event independent of SSE, JSON-RPC, or terminal framing.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StreamEvent<T> {
    pub sequence: u64,
    pub kind: StreamEventKind<T>,
}

impl<T> StreamEvent<T> {
    pub fn item(sequence: u64, value: T) -> Result<Self, ApplicationContractError> {
        Ok(Self {
            sequence,
            kind: StreamEventKind::Item(value),
        })
    }

    pub fn terminal(
        sequence: u64,
        termination: StreamTermination,
    ) -> Result<Self, ApplicationContractError> {
        termination.validate()?;
        Ok(Self {
            sequence,
            kind: StreamEventKind::Terminal(termination),
        })
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum StreamValidationError {
    #[error("stream must publish exactly one terminal event")]
    MissingTerminal,
    #[error("stream sequence is not strictly contiguous")]
    NonContiguousSequence,
    #[error("stream published an event after its terminal receipt")]
    EventAfterTerminal,
    #[error("stream published more than one terminal event")]
    MultipleTerminalEvents,
    #[error("stream sequence overflowed")]
    SequenceOverflow,
    #[error("stream gap is invalid: {0}")]
    InvalidGap(String),
    #[error("stream terminal receipt is invalid: {0}")]
    InvalidTerminal(String),
}

/// Validate a bounded event sequence before an adapter renders it.
pub fn validate_stream<T>(events: &[StreamEvent<T>]) -> Result<(), StreamValidationError> {
    let mut terminal_seen = false;
    let mut expected = events.first().map(|event| event.sequence);

    for event in events {
        if terminal_seen {
            return Err(if event.kind.is_terminal() {
                StreamValidationError::MultipleTerminalEvents
            } else {
                StreamValidationError::EventAfterTerminal
            });
        }
        if Some(event.sequence) != expected {
            return Err(StreamValidationError::NonContiguousSequence);
        }
        if let StreamEventKind::Terminal(termination) = &event.kind {
            termination
                .validate()
                .map_err(|error| StreamValidationError::InvalidTerminal(error.to_string()))?;
            terminal_seen = true;
            continue;
        }
        if let StreamEventKind::Gap(gap) = &event.kind {
            if event.sequence != gap.first_missing_sequence {
                return Err(StreamValidationError::InvalidGap(
                    "event sequence does not match the first missing sequence".to_owned(),
                ));
            }
            let next_sequence = gap
                .last_missing_sequence
                .checked_add(1)
                .ok_or(StreamValidationError::SequenceOverflow)?;
            gap.validate()
                .map_err(|error| StreamValidationError::InvalidGap(error.to_string()))?;
            expected = Some(next_sequence);
        } else {
            expected = Some(
                event
                    .sequence
                    .checked_add(1)
                    .ok_or(StreamValidationError::SequenceOverflow)?,
            );
        }
    }

    if terminal_seen {
        Ok(())
    } else {
        Err(StreamValidationError::MissingTerminal)
    }
}
