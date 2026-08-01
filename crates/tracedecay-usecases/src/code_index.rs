//! Application adapter for the production Plan 25 code-index owner.
//!
//! The daemon/application request context remains the sole source of
//! cancellation and deadline state. This adapter does not decode controls
//! from client data or create a second publication authority.

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use tracedecay_application::RequestContext;

use crate::{
    application::context::{
        CancellationToken, RequestInterruption, application_request_interruption,
    },
    code_index::{
        production::{
            CodeIndexAtomicPublicationPort, CodeIndexExecutionControlV1,
            CodeIndexProductionConfigV1, CodeIndexProductionOpenErrorV1,
            CodeIndexProductionOwnerV1,
        },
        projection::CodeChunkProjectionSink,
    },
};

/// Production owner type exposed to daemon, CLI, MCP, and hook composition.
pub type ProductionCodeIndexOwnerV1<P, S> = CodeIndexProductionOwnerV1<P, S>;

/// Adapt one already-authorized application request to synchronous code-index
/// checkpoints. The owner checks this control before and after every bounded
/// extraction/chunking stage and before atomic publication.
pub struct RequestContextCodeIndexControlV1<'a> {
    context: &'a RequestContext,
    cancellation: &'a CancellationToken,
}

impl<'a> RequestContextCodeIndexControlV1<'a> {
    pub fn new(context: &'a RequestContext, cancellation: &'a CancellationToken) -> Self {
        Self {
            context,
            cancellation,
        }
    }
}

impl CodeIndexExecutionControlV1 for RequestContextCodeIndexControlV1<'_> {
    fn is_cancelled(&self) -> bool {
        matches!(
            application_request_interruption(self.context, self.cancellation),
            Some(RequestInterruption::Cancelled)
        )
    }

    fn is_deadline_exceeded(&self) -> bool {
        matches!(
            application_request_interruption(self.context, self.cancellation),
            Some(RequestInterruption::DeadlineExceeded)
        )
    }
}

/// Daemon-owned cancellation fence for one immutable snapshot build.
///
/// A scheduler captures the current epoch when it seals a snapshot. A later
/// filesystem hint advances the epoch and fairly cancels only that superseded
/// build; unrelated worktrees retain independent fences.
#[derive(Clone, Default)]
pub struct DaemonCodeIndexControlV1 {
    epoch: Arc<AtomicU64>,
    expected_epoch: u64,
    shutting_down: Arc<AtomicBool>,
}

impl DaemonCodeIndexControlV1 {
    pub fn new(epoch: Arc<AtomicU64>, shutting_down: Arc<AtomicBool>) -> Self {
        let expected_epoch = epoch.load(Ordering::Acquire);
        Self {
            epoch,
            expected_epoch,
            shutting_down,
        }
    }

    pub fn advance(epoch: &AtomicU64) {
        epoch.fetch_add(1, Ordering::AcqRel);
    }
}

impl CodeIndexExecutionControlV1 for DaemonCodeIndexControlV1 {
    fn is_cancelled(&self) -> bool {
        self.shutting_down.load(Ordering::Acquire)
            || self.epoch.load(Ordering::Acquire) != self.expected_epoch
    }

    fn is_deadline_exceeded(&self) -> bool {
        false
    }
}

/// Open the production code-index owner with only the established projection
/// and store-owned atomic-publication seams left injectable.
pub fn open_production_code_index_owner_v1<P, S>(
    config: CodeIndexProductionConfigV1,
    publication: P,
    projection: S,
) -> Result<ProductionCodeIndexOwnerV1<P, S>, CodeIndexProductionOpenErrorV1>
where
    P: CodeIndexAtomicPublicationPort,
    S: CodeChunkProjectionSink,
{
    CodeIndexProductionOwnerV1::new(config, publication, projection)
}
