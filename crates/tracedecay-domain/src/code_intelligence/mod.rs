//! Storage-neutral, runtime/store-free code-intelligence contracts for PR9
//! (Plan 25: Code Intelligence Indexing).
//!
//! These values are immutable logical records: no storage rows, no parser
//! acquisition, no runtime, no transport. Implementations live in
//! `src/code_index/` (root modules) and move to `crates/tracedecay-code-index`
//! unchanged only if the Plan 19 extraction gate approves a crate.
//!
//! Ownership: Plan 25 owns these code-specific contracts. Plan 15 owns the
//! shared retrieval kernel (`crate::retrieval`); Plan 35 owns
//! `GenerationDiagnosticV1` (`crate::diagnostics`, pr9/12 packet); Plan 36
//! owns native read-only Git semantics. This module stores only typed
//! references to those contracts.

pub mod graph;
pub mod identity;
pub mod index;
pub mod language;
pub mod search;

pub use graph::*;
pub use identity::*;
pub use index::*;
pub use language::*;
pub use search::*;

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
