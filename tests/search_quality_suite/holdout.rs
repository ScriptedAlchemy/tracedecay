//! Holdout seal metadata checks for the contract-only packet.
//!
//! Development runs validate only the opaque locator and seal digest metadata.
//! They never open holdout label bytes. Locked evaluation loads labels from a
//! direct local filesystem path outside this harness.

use crate::evaluation::HoldoutSealV1;

/// Development-visible state of the sealed holdout locator metadata.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HoldoutSealStatus {
    /// The metadata is valid and the payload remains external.
    AuthorizedStoreOnly,
    /// The opaque locator or seal metadata is malformed.
    InvalidMetadata,
}

/// Validates metadata without opening holdout label bytes.
pub(crate) fn seal_status(seal: &HoldoutSealV1) -> HoldoutSealStatus {
    if seal.validate().is_ok() {
        HoldoutSealStatus::AuthorizedStoreOnly
    } else {
        HoldoutSealStatus::InvalidMetadata
    }
}
