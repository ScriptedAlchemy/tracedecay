//! Storage-neutral, runtime/store-free code-intelligence contracts.
//!
//! These values are immutable logical records: no storage rows, no parser
//! acquisition, no runtime, no transport. Implementations live in
//! `src/code_index/` (root modules) and `crates/tracedecay-code-index`.
//!
//! This module stores only typed references to the shared retrieval kernel
//! (`crate::retrieval`), `GenerationDiagnosticV1` (`crate::diagnostics`),
//! and native read-only Git semantics.

pub mod graph;
pub mod identity;
pub mod index;
pub mod language;
pub mod search;
mod vector_contract;

pub use graph::*;
pub use identity::*;
pub use index::*;
pub use language::*;
pub use search::*;
pub use vector_contract::*;

#[cfg(test)]
mod tests {
    use super::NodeKind;

    #[test]
    fn protobuf_node_kinds_are_unconditional_domain_vocabulary() {
        let kinds = [
            (NodeKind::ProtoMessage, "proto_message"),
            (NodeKind::ProtoService, "proto_service"),
            (NodeKind::ProtoRpc, "proto_rpc"),
        ];

        for (kind, wire_name) in kinds {
            assert_eq!(kind.as_str(), wire_name);
            assert_eq!(NodeKind::from_str(wire_name), Some(kind));
        }
    }
}
