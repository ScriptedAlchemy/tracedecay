//! LSP session and framing wire vocabulary.
//!
//! These are the daemon-wire identities, request-id sequences, and
//! Content-Length frame poll/send outcomes exchanged between the LSP bridge,
//! the invocation client, and the daemon. They are data: no overlay, no
//! analyzer, no session actor, and no stdio pump.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

/// Hard JSON-RPC payload limit for one bridged frame.
pub const MAX_LSP_FRAME_BYTES: usize = 4 * 1024 * 1024;

/// An opaque LSP JSON-RPC payload. Framing adapters remove and restore the
/// `Content-Length` envelope; the bridge never parses the JSON body.
pub type LspFrame = Vec<u8>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FramePoll {
    Frame(LspFrame),
    Pending,
    Closed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrameSend {
    Sent,
    Backpressured,
    Closed,
}

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
    #[hotpath::skip]
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
    #[hotpath::skip]
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

/// Maximum roots admitted into one exact workspace-folder set.
/// A client may only admit the bounded root set authorized for this session.
/// Keeping this small also bounds federated provider fan-out before any graph
/// or analyzer operation is started.
pub const MAX_LSP_WORKSPACE_ROOTS: usize = 8;

/// Wire-shape failure for [`LspSessionId::new`] or [`LspSessionCredential::new`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LspSessionIdentityError {
    InvalidSessionId,
    InvalidCredential,
}

/// Opaque daemon-assigned LSP session identity.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct LspSessionId(String);

impl LspSessionId {
    pub fn new(value: impl Into<String>) -> Result<Self, LspSessionIdentityError> {
        let value = value.into();
        if value.is_empty() || value.len() > 128 {
            return Err(LspSessionIdentityError::InvalidSessionId);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Opaque credential minted by the daemon admission authority.
#[derive(Clone, Eq, PartialEq)]
pub struct LspSessionCredential(Vec<u8>);

impl LspSessionCredential {
    pub fn new(value: impl Into<Vec<u8>>) -> Result<Self, LspSessionIdentityError> {
        let value = value.into();
        if value.len() < 16 || value.len() > 256 {
            return Err(LspSessionIdentityError::InvalidCredential);
        }
        Ok(Self(value))
    }

    /// Returns credential material only to an authenticated daemon wire
    /// adapter. Presentation code must never log or render this value.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for LspSessionCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("LspSessionCredential([redacted])")
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LspSessionAccess {
    session_id: LspSessionId,
    credential: LspSessionCredential,
}

impl LspSessionAccess {
    pub fn new(session_id: LspSessionId, credential: LspSessionCredential) -> Self {
        Self {
            session_id,
            credential,
        }
    }

    pub fn session_id(&self) -> &LspSessionId {
        &self.session_id
    }

    /// Returns the opaque credential only to the authenticated daemon
    /// invocation service.
    pub fn credential(&self) -> &LspSessionCredential {
        &self.credential
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
