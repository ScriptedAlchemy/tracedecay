//! Application adapter for the production Plan 25 code-index owner.
//!
//! The daemon/application request context remains the sole source of
//! cancellation and deadline state. This adapter does not decode controls
//! from client data or create a second publication authority.

use std::future::Future;
use std::pin::Pin;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use tracedecay_contracts::RequestContext;
use tracedecay_contracts::code_index_freshness::CodeIndexConvergenceParkedV1;
use tracedecay_domain::CodeGenerationId;

use tracedecay_code_index::{
    chunks::CodeIndexImportEvidenceV1,
    production::{
        CodeIndexAtomicPublicationPort, CodeIndexExecutionControlV1, CodeIndexProductionConfigV1,
        CodeIndexProductionOpenErrorV1, CodeIndexProductionOwnerV1,
    },
    projection::CodeChunkProjectionSink,
};
use tracedecay_runtime_core::cancellation::CancellationToken;
use tracedecay_runtime_core::resident_memory::{
    ResidentMemoryPressureStateV1, ResidentMemoryPressureV1,
};
use tracedecay_session_memory::context::{RequestInterruption, application_request_interruption};

/// Production owner type exposed to daemon, CLI, MCP, and hook composition.
pub type ProductionCodeIndexOwnerV1<P, S> = CodeIndexProductionOwnerV1<P, S>;

/// One admitted lazy-index request, pinned to the graph generation whose
/// verified parser evidence motivated it.
pub struct CodeIndexIgnoredDependencyAdmissionRequestV1<'a> {
    context: &'a RequestContext,
    source_generation: &'a CodeGenerationId,
    imports: &'a [CodeIndexImportEvidenceV1],
}

impl<'a> CodeIndexIgnoredDependencyAdmissionRequestV1<'a> {
    pub fn new(
        context: &'a RequestContext,
        source_generation: &'a CodeGenerationId,
        imports: &'a [CodeIndexImportEvidenceV1],
    ) -> Self {
        Self {
            context,
            source_generation,
            imports,
        }
    }

    pub const fn context(&self) -> &RequestContext {
        self.context
    }

    pub const fn source_generation(&self) -> &CodeGenerationId {
        self.source_generation
    }

    pub const fn imports(&self) -> &[CodeIndexImportEvidenceV1] {
        self.imports
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CodeIndexIgnoredDependencyAdmissionErrorV1 {
    Unavailable {
        detail: String,
    },
    ReadOnly,
    Cancelled,
    TimedOut,
    Stale {
        active_generation: CodeGenerationId,
    },
    /// No generation to admit against is coming until the park's
    /// remediation is applied; repeating the request cannot change that.
    Parked(CodeIndexConvergenceParkedV1),
}

pub type CodeIndexIgnoredDependencyAdmissionFutureV1<'a> = Pin<
    Box<
        dyn Future<Output = Result<CodeGenerationId, CodeIndexIgnoredDependencyAdmissionErrorV1>>
            + Send
            + 'a,
    >,
>;

/// Transport-neutral scheduling seam for parser-verified ignored dependency
/// imports. Implementations may advance the canonical code-index generation;
/// they may not return symbols directly.
pub trait CodeIndexIgnoredDependencyAdmissionPortV1: Send + Sync {
    fn admit<'a>(
        &'a self,
        request: CodeIndexIgnoredDependencyAdmissionRequestV1<'a>,
    ) -> CodeIndexIgnoredDependencyAdmissionFutureV1<'a>;
}

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
/// build; unrelated worktrees retain independent fences. When a resident-memory
/// cell is bound, the same checkpoints read the process: a build admitted
/// under the watermark must stop once live RSS crosses it, instead of
/// allocating until the cgroup kill line.
#[derive(Clone)]
pub struct DaemonCodeIndexControlV1 {
    epoch: Arc<AtomicU64>,
    expected_epoch: u64,
    shutting_down: Arc<AtomicBool>,
    pressure: Option<Arc<ResidentMemoryPressureV1>>,
    tripped: Arc<AtomicBool>,
}

impl Default for DaemonCodeIndexControlV1 {
    fn default() -> Self {
        Self::new(
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicBool::new(false)),
        )
    }
}

impl DaemonCodeIndexControlV1 {
    pub fn new(epoch: Arc<AtomicU64>, shutting_down: Arc<AtomicBool>) -> Self {
        let expected_epoch = epoch.load(Ordering::Acquire);
        Self {
            epoch,
            expected_epoch,
            shutting_down,
            pressure: None,
            tripped: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Read `pressure` at each checkpoint. A watermark crossing stays tripped
    /// for this attempt so a later low sample does not resume the same build.
    #[must_use]
    pub fn with_resident_memory(mut self, pressure: Arc<ResidentMemoryPressureV1>) -> Self {
        self.pressure = Some(pressure);
        self
    }

    pub fn advance(epoch: &AtomicU64) {
        epoch.fetch_add(1, Ordering::AcqRel);
    }

    /// The watermark ended this attempt. Shutdown and a newer source epoch
    /// keep their own identity.
    #[must_use]
    pub fn refused_by_resident_memory(&self) -> bool {
        self.tripped.load(Ordering::Acquire)
            && !self.shutting_down.load(Ordering::Acquire)
            && self.epoch.load(Ordering::Acquire) == self.expected_epoch
    }
}

impl CodeIndexExecutionControlV1 for DaemonCodeIndexControlV1 {
    fn is_cancelled(&self) -> bool {
        if self.shutting_down.load(Ordering::Acquire)
            || self.epoch.load(Ordering::Acquire) != self.expected_epoch
        {
            return true;
        }
        if self.tripped.load(Ordering::Acquire) {
            return true;
        }
        let Some(pressure) = &self.pressure else {
            return false;
        };
        // The maintenance sampler is slower than a worktree reconcile. Trusting
        // the last published state here is how a second linked worktree kept
        // allocating until the cgroup killer. Reclaimers stay off this thread:
        // the indexing pool's stack cannot host them.
        let state = pressure.sample_for_checkpoint();
        if matches!(state, ResidentMemoryPressureStateV1::OverBudget { .. }) {
            self.tripped.store(true, Ordering::Release);
            return true;
        }
        false
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

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    use tracedecay_code_index::production::CodeIndexExecutionControlV1;
    use tracedecay_runtime_core::resident_memory::{
        ProcessResidentSampleV1, RESIDENT_MEMORY_CHECKPOINT_SAMPLE_INTERVAL_V1,
        ResidentMemoryPressureV1,
    };

    use super::DaemonCodeIndexControlV1;

    #[test]
    fn a_checkpoint_stops_at_the_watermark_without_reclaiming() {
        let limit = NonZeroU64::new(100 * 1024 * 1024).expect("limit");
        let reads = Arc::new(AtomicU64::new(0));
        let sampled = Arc::clone(&reads);
        let ceiling = limit.get();
        let pressure = Arc::new(ResidentMemoryPressureV1::with_sampler(
            limit,
            Arc::new(move || {
                let read = sampled.fetch_add(1, Ordering::AcqRel);
                let bytes = if read == 0 { 1 } else { ceiling };
                Some(ProcessResidentSampleV1 {
                    resident_bytes: bytes,
                    unreclaimable_bytes: bytes,
                    swapped_bytes: 0,
                    cgroup_committed_bytes: None,
                })
            }),
        ));
        let reclaimed = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&reclaimed);
        let _reclaimer = pressure
            .register_pressure_reclaimer(
                0,
                Arc::new(move |_| {
                    flag.store(true, Ordering::Release);
                    0
                }),
            )
            .expect("register reclaimer");
        let epoch = Arc::new(AtomicU64::new(3));
        let shutting_down = Arc::new(AtomicBool::new(false));
        let control = DaemonCodeIndexControlV1::new(Arc::clone(&epoch), Arc::clone(&shutting_down))
            .with_resident_memory(pressure);

        assert!(
            !control.is_cancelled(),
            "a sample under the watermark must keep the build admitted"
        );
        assert!(!control.refused_by_resident_memory());
        std::thread::sleep(RESIDENT_MEMORY_CHECKPOINT_SAMPLE_INTERVAL_V1);
        assert!(
            control.is_cancelled(),
            "the next live sample over the watermark must stop the build"
        );
        assert!(control.refused_by_resident_memory());
        assert!(
            !reclaimed.load(Ordering::Acquire),
            "the indexing checkpoint must not run pressure reclaimers"
        );
        assert_eq!(epoch.load(Ordering::Acquire), 3);
        assert!(!shutting_down.load(Ordering::Acquire));
        assert!(!control.is_deadline_exceeded());
    }

    #[test]
    fn checkpoint_sampling_failure_preserves_known_pressure() {
        let limit = NonZeroU64::new(100 * 1024 * 1024).expect("limit");
        let pressure = Arc::new(ResidentMemoryPressureV1::with_sampler(
            limit,
            Arc::new(|| None),
        ));
        pressure.publish_observed_resident_bytes(limit.get());
        let control = DaemonCodeIndexControlV1::default().with_resident_memory(pressure);
        assert!(control.is_cancelled());
        assert!(control.refused_by_resident_memory());
    }
}
