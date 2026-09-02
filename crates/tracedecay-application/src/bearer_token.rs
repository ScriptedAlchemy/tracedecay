//! Shared bearer-secret handling for application handoff tokens.
//!
//! One validation and zeroization authority for every bearer secret accepted
//! at the daemon boundary, so token families cannot drift on length/character
//! rules and no secret survives release in freed memory.

use std::fmt;
use std::hint::black_box;
use std::mem;

use tracedecay_domain::{DomainError, ManifestDigest, canonical_sha256};

/// A validated bearer secret: 32–512 bytes, trimmed, and control-free.
///
/// Never serialized, never printed; the owned buffer is overwritten before
/// release. Digesting is domain-separated so distinct token families derived
/// from the same secret can never collide.
pub(crate) struct BearerTokenSecret {
    secret: String,
}

/// The presented secret failed bearer validation. Deliberately carries no
/// detail: nothing about a rejected secret may leak into errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct InvalidBearerToken;

impl BearerTokenSecret {
    pub(crate) fn new(secret: String) -> Result<Self, InvalidBearerToken> {
        let byte_len = secret.len();
        if !(32..=512).contains(&byte_len)
            || secret.trim() != secret
            || secret.chars().any(char::is_control)
        {
            // Reject before taking ownership semantics: the rejected buffer is
            // still zeroized on drop below.
            let _rejected = Self { secret };
            return Err(InvalidBearerToken);
        }
        Ok(Self { secret })
    }

    /// Canonical domain-separated digest of the secret. The hashed shape is
    /// byte-identical to the previous per-family implementations:
    /// `canonical_sha256(&(domain, &secret))`.
    pub(crate) fn digest(&self, domain: &'static str) -> Result<ManifestDigest, DomainError> {
        canonical_sha256(&(domain, &self.secret))
    }
}

impl fmt::Debug for BearerTokenSecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("BearerTokenSecret([REDACTED])")
    }
}

impl Drop for BearerTokenSecret {
    fn drop(&mut self) {
        // `into_bytes` moves the allocation without copying, so zeroing the
        // returned buffer zeroes the secret's actual bytes.
        let mut bytes = mem::take(&mut self.secret).into_bytes();
        bytes.fill(0);
        black_box(&bytes);
    }
}
