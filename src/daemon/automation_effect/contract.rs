//! Shared canonical digest and contract-error projection.

use serde::Serialize;
use tracedecay_domain::{ManifestDigest, canonical_sha256};

use crate::errors::{Result, TraceDecayError};

pub(super) fn digest(value: &impl Serialize) -> Result<ManifestDigest> {
    canonical_sha256(value).map_err(contract_error)
}

pub(super) fn contract_error(error: impl std::fmt::Display) -> TraceDecayError {
    TraceDecayError::Config {
        message: format!("automation application contract is invalid: {error}"),
    }
}
