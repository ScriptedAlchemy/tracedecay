//! Pre-cutover native adapters for branch- and snapshot-scoped code graphs.
//!
//! This module is deliberately self-contained. It consumes the canonical shard
//! and graph read contracts from `tracedecay-store`, but does not publish a root
//! dependency, register a runtime, or make the native path authoritative.

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
