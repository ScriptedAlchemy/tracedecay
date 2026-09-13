use serde::{Deserialize, Serialize};

use super::error::DomainError;

/// UTC timestamp represented as microseconds from the Unix epoch.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct UtcMicros(pub i64);

/// Closed half-open occurrence interval.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(deny_unknown_fields)]
pub struct TimeInterval {
    pub start: UtcMicros,
    pub end: UtcMicros,
}

impl TimeInterval {
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.start > self.end {
            return Err(DomainError::InvalidTimeInterval);
        }
        Ok(())
    }
}
