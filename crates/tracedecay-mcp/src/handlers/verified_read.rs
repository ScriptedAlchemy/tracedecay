//! The verified-graph read a dispatch table opens through the composition
//! root.
//!
//! Every graph-backed tool funnels through one admission open the root owns:
//! it binds the caller's request identity, deadline, and cancellation to the
//! exact admitted scope and reports a stale serving seat back to the dispatch
//! boundary. The tables in this crate never open a graph themselves; they
//! name the catalog operation and hand it to that funnel.

use std::future::Future;
use std::pin::Pin;

use tracedecay_code_index::graph_projection::CodeGraphReadinessRequirement;
use tracedecay_contracts::ApplicationOperation;
use tracedecay_contracts::retrieval::catalog::primitive_read_operation;
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_graph_query::VerifiedGraphQuery;

/// One verified-graph read a handler asks the funnel to open: the catalog
/// operation it is admitted under and the residency its answer needs.
#[derive(Clone, Debug)]
pub struct VerifiedGraphRead {
    pub operation: ApplicationOperation,
    pub readiness: CodeGraphReadinessRequirement,
}

impl VerifiedGraphRead {
    /// A read admitted under `operation` that waits for the full catalog.
    #[must_use]
    pub fn catalog(operation: ApplicationOperation) -> Self {
        Self {
            operation,
            readiness: CodeGraphReadinessRequirement::Catalog,
        }
    }

    /// A read answered from the engine alone, which therefore does not wait
    /// for a released catalog to warm again.
    #[must_use]
    pub fn engine_only(self) -> Self {
        Self {
            readiness: CodeGraphReadinessRequirement::Engine,
            ..self
        }
    }
}

/// One admitted verified-graph open in flight.
pub type VerifiedGraphOpenFuture<'a> =
    Pin<Box<dyn Future<Output = Result<VerifiedGraphQuery>> + Send + 'a>>;

/// The root's verified-graph admission funnel, lent to a dispatch table for
/// one tool call. Opening is lazy: a handler that answers without the graph
/// never pays for admission.
pub type VerifiedGraphOpen<'a> =
    dyn Fn(VerifiedGraphRead) -> VerifiedGraphOpenFuture<'a> + Sync + 'a;

/// Resolves the primitive read operation the catalog registers as
/// `operation_name`, the identity a verified open is admitted under. The read
/// waits for the full catalog unless the handler narrows it.
pub fn verified_read_operation(operation_name: &str) -> Result<VerifiedGraphRead> {
    let operation = primitive_read_operation(operation_name)
        .map_err(|error| TraceDecayError::Config {
            message: format!("invalid graph read operation: {error}"),
        })?
        .ok_or_else(|| TraceDecayError::Config {
            message: format!("unregistered graph read operation: {operation_name}"),
        })?;
    Ok(VerifiedGraphRead::catalog(operation))
}
