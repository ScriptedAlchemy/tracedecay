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

use tracedecay_contracts::ApplicationOperation;
use tracedecay_contracts::retrieval::catalog::primitive_read_operation;
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_graph_query::VerifiedGraphQuery;

/// One admitted verified-graph open in flight.
pub type VerifiedGraphOpenFuture<'a> =
    Pin<Box<dyn Future<Output = Result<VerifiedGraphQuery>> + Send + 'a>>;

/// The root's verified-graph admission funnel, lent to a dispatch table for
/// one tool call. Opening is lazy: a handler that answers without the graph
/// never pays for admission.
pub type VerifiedGraphOpen<'a> =
    dyn Fn(ApplicationOperation) -> VerifiedGraphOpenFuture<'a> + Sync + 'a;

/// Resolves the primitive read operation the catalog registers as
/// `operation_name`, the identity a verified open is admitted under.
pub fn verified_read_operation(operation_name: &str) -> Result<ApplicationOperation> {
    primitive_read_operation(operation_name)
        .map_err(|error| TraceDecayError::Config {
            message: format!("invalid graph read operation: {error}"),
        })?
        .ok_or_else(|| TraceDecayError::Config {
            message: format!("unregistered graph read operation: {operation_name}"),
        })
}
