//! Pure fact, memory, and lineage contracts.
//!
//! Facts are immutable assertions over receipt-bound payloads. Corrections,
//! trust changes, curation, and deletion are append-only lineage events; a
//! mutable current view is always a projection of that history.

mod fact;
mod lineage;
mod relation;

pub use fact::{
    FactAssertionKindV1, FactAssertionV1, FactCategoryV1, FactEvidenceRefV1,
    FactEvidenceRelationV1, FactIdentityMaterialV1, FactIdentitySourceV1, FactOwnerV1,
    FactPayloadV1,
};
pub use lineage::{FactCurationActionV1, FactLineageEventKindV1, FactLineageEventV1};
pub use relation::{
    FactRelationKindV1, FactRelationProvenanceV1, FactRelationV1, ProjectMemoryGraphRelationKindV1,
};

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
