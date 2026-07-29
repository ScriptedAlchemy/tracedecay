//! Checked protocol-id sequences for LSP connections and processes.
//!
//! These are deliberately not durable or global request identities. They mint
//! ids whose uniqueness requirement is exactly one live connection or one
//! process, and they refuse to wrap rather than reissue a used number.

use std::sync::atomic::{AtomicU64, Ordering};

/// A sequence refused to issue another id because it reached `u64::MAX`.
///
/// Exhaustion is a hard failure: reusing a protocol id would let a stale reply
/// resolve a live request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SequenceExhausted;

impl std::fmt::Display for SequenceExhausted {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("the process-wide identity sequence is exhausted")
    }
}

impl std::error::Error for SequenceExhausted {}

/// Counter for protocol ids whose uniqueness requirement is explicitly one
/// live connection, not global or persistent.
#[derive(Clone, Debug)]
pub struct ConnectionLocalRequestSequence {
    next: u64,
}

impl ConnectionLocalRequestSequence {
    pub const fn starting_at(first: u64) -> Self {
        Self { next: first }
    }

    pub fn next_number(&mut self) -> Result<u64, SequenceExhausted> {
        let current = self.next;
        self.next = self.next.checked_add(1).ok_or(SequenceExhausted)?;
        Ok(current)
    }

    pub fn next_string(&mut self, prefix: &str) -> Result<String, SequenceExhausted> {
        self.next_number()
            .map(|sequence| format!("{prefix}{sequence}"))
    }
}

/// Checked sequence for correlation ids whose complete lifetime is one process.
///
/// This is intentionally distinct from a minted global request identity:
/// callers may use it only when no id survives restart or crosses a process
/// boundary.
#[derive(Debug)]
pub struct ProcessLocalRequestSequence {
    next: AtomicU64,
}

impl ProcessLocalRequestSequence {
    pub const fn starting_at(first: u64) -> Self {
        Self {
            next: AtomicU64::new(first),
        }
    }

    pub fn next_number(&self) -> Result<u64, SequenceExhausted> {
        self.next
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_add(1)
            })
            .map_err(|_| SequenceExhausted)
    }

    pub fn next_string(&self, prefix: &str) -> Result<String, SequenceExhausted> {
        self.next_number()
            .map(|sequence| format!("{prefix}{sequence}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_local_sequence_never_wraps_to_a_duplicate() {
        let mut sequence = ConnectionLocalRequestSequence::starting_at(u64::MAX - 1);
        assert_eq!(sequence.next_number(), Ok(u64::MAX - 1));
        assert_eq!(sequence.next_number(), Err(SequenceExhausted));
        assert_eq!(sequence.next_number(), Err(SequenceExhausted));
    }

    #[test]
    fn process_local_sequence_never_wraps_to_a_duplicate() {
        let sequence = ProcessLocalRequestSequence::starting_at(u64::MAX - 1);
        assert_eq!(sequence.next_number(), Ok(u64::MAX - 1));
        assert_eq!(sequence.next_number(), Err(SequenceExhausted));
        assert_eq!(sequence.next_number(), Err(SequenceExhausted));
    }

    #[test]
    fn exhaustion_reports_the_retained_identity_failure_class() {
        assert_eq!(
            SequenceExhausted.to_string(),
            "the process-wide identity sequence is exhausted"
        );
    }
}
