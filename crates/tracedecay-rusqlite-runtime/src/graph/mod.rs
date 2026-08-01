//! Native adapters for branch- and snapshot-scoped code graphs.
//!
//! These adapters are registered in production: the daemon's store-runtime
//! registry attaches every [`StoreShardScopeV1::Code`] shard through
//! [`GraphPhysicalAttachmentFactory`], which builds a [`GraphReaderExecutor`]
//! and, for mutable shards, a [`GraphMutationExecutor`]; see
//! `crates/tracedecay-runtime-core/src/store_runtime/registry/ports.rs`.
//!
//! The module stays self-contained in the sense that matters: it consumes the
//! canonical shard and graph contracts from `tracedecay-store` and depends on
//! nothing in the root crate.
//!
//! Read-operation coverage is uneven. Node, edge, file, and search reads are
//! constructed by production callers. `RuntimeReadOperationV1::GraphStats` is
//! implemented and covered by this crate's tests, but no production caller
//! constructs it yet — the live statistics path is still
//! `crates/tracedecay-runtime-core/src/db/stats.rs`, and the two share a
//! `display_language_for_path` mapping that must be kept in sync until one of
//! them wins.
//!
//! [`StoreShardScopeV1::Code`]: tracedecay_store::StoreShardScopeV1

mod attachment;
pub mod fixtures;
mod locator;
mod mutation;
mod read;

pub use attachment::{
    GraphDispatchError, GraphPhysicalAttachmentFactory, GraphPhysicalAttachmentParts,
    GraphPhysicalAttachmentPrepareError, GraphPhysicalAttachmentStartError,
    GraphRuntimePhysicalAttachment, GraphRuntimePhysicalSnapshot,
};
pub use locator::{
    CodeShardAccessV1, CodeShardLocatorError, CodeShardPhysicalLocator,
    CodeShardPhysicalLocatorFactory, GRAPH_DATABASE_FILENAME,
};
pub use mutation::{
    GraphEdgeMutationV1, GraphFileMutationV1, GraphFileReplacementV1, GraphMutationExecutor,
    GraphMutationPayloadV1,
};
pub use read::GraphReaderExecutor;

#[cfg(test)]
mod tests;
