//! Pure fact, memory, and lineage contracts.
//!
//! Facts are immutable assertions over receipt-bound payloads. Corrections,
//! trust changes, curation, and deletion are append-only lineage events; a
//! mutable current view is always a projection of that history.

mod fact;
mod lineage;

pub use fact::*;
pub use lineage::*;

use serde::Serialize;

use crate::research::{DomainError, canonical_sha256};

pub(crate) fn derive_memory_id(
    namespace: &'static str,
    value: &impl Serialize,
) -> Result<String, DomainError> {
    let digest = canonical_sha256(&(namespace, value))?;
    let encoded =
        crate::canonical_text::sha256_hex_body(digest.as_str(), "memory identity digest")?;
    Ok(format!("{namespace}.{encoded}"))
}
