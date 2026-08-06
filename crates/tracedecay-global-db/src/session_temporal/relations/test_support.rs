use std::sync::Arc;

use sha2::{Digest, Sha256};
use tracedecay_domain::MessageOccurrenceIdV1;
use tracedecay_graph_db::{GraphDbOwner, NeverCancelled};

use super::SessionRelationGraphStore;

pub(crate) fn memory_relation_store() -> SessionRelationGraphStore {
    let owner = GraphDbOwner::memory(Arc::new(NeverCancelled)).expect("memory relation graph");
    SessionRelationGraphStore::new(owner.handle())
}

/// Derives a canonical `sha256:`-tagged occurrence identity from a readable
/// test seed so fixtures satisfy production identity validation.
pub(crate) fn occurrence_id_for_test(seed: &str) -> MessageOccurrenceIdV1 {
    let digest = Sha256::digest(seed.as_bytes());
    let mut value = String::with_capacity(71);
    value.push_str("sha256:");
    for byte in digest {
        value.push_str(&format!("{byte:02x}"));
    }
    MessageOccurrenceIdV1::new(value).expect("derived occurrence identity")
}
