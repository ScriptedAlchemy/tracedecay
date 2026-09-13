use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::error::DomainError;

/// UTC timestamp represented as microseconds from the Unix epoch.
#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
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
        if self.start >= self.end {
            return Err(DomainError::InvalidTimeInterval);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_open_interval_rejects_zero_width() {
        assert_eq!(
            TimeInterval {
                start: UtcMicros(7),
                end: UtcMicros(7),
            }
            .validate(),
            Err(DomainError::InvalidTimeInterval)
        );
    }
}
