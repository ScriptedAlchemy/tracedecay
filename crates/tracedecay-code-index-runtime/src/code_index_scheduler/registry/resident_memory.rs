//! Resident-memory authority construction for code-index scheduler registries.

use std::sync::Arc;

#[cfg(any(test, feature = "test-helpers"))]
use tracedecay_runtime_core::resident_memory::DEFAULT_PROCESS_RESIDENT_MEMORY_LIMIT_V1;
pub use tracedecay_runtime_core::resident_memory::ProcessResidentMemoryV1;

use super::CodeIndexSchedulerRegistryV1;

impl CodeIndexSchedulerRegistryV1 {
    #[cfg(any(test, feature = "test-helpers"))]
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn new(max_worktrees: usize) -> Self {
        Self::with_resident_memory_and_progress_producer_incarnation(
            max_worktrees,
            Arc::new(ProcessResidentMemoryV1::new(
                DEFAULT_PROCESS_RESIDENT_MEMORY_LIMIT_V1,
            )),
            1,
        )
    }

    // Reached from the `test-transport` dashboard and configuration fixture
    // runtimes, which compile without `cfg(test)`. The gate names those two
    // callers exactly: widening it to `test-helpers` would compile this into
    // builds where nothing calls it, which `-D warnings` rejects as dead code.
    #[cfg(any(test, feature = "test-transport"))]
    pub fn with_resident_memory(
        max_worktrees: usize,
        resident_memory: Arc<ProcessResidentMemoryV1>,
    ) -> Self {
        Self::with_resident_memory_and_progress_producer_incarnation(
            max_worktrees,
            resident_memory,
            1,
        )
    }

    /// Build a registry under one durable daemon epoch. The constructor name
    /// is retained for its invocation-state caller; individual producer
    /// incarnations are minted below this daemon authority for each scheduler.
    pub fn with_resident_memory_and_progress_producer_incarnation(
        max_worktrees: usize,
        resident_memory: Arc<ProcessResidentMemoryV1>,
        progress_daemon_incarnation: u64,
    ) -> Self {
        let (generation_publications, _) =
            tokio::sync::broadcast::channel(super::GENERATION_PUBLICATION_CHANNEL_CAPACITY);
        Self {
            max_worktrees,
            progress_daemon_incarnation: progress_daemon_incarnation.max(1),
            next_progress_producer_incarnation: Arc::new(std::sync::atomic::AtomicU64::new(1)),
            resident_memory,
            byte_pool: Arc::new(super::SharedCodeIndexBytePoolV1::default()),
            mounted: Arc::new(tokio::sync::Mutex::new(std::collections::BTreeMap::new())),
            retiring: Arc::new(tokio::sync::Mutex::new(std::collections::BTreeMap::new())),
            cold_mount_reservations: Arc::new(std::sync::Mutex::new(
                std::collections::BTreeMap::new(),
            )),
            background_reconcile_admission: Arc::new(tokio::sync::Semaphore::new(
                super::host_cpu_target(super::MAX_CONCURRENT_RECONCILE_WORKTREES),
            )),
            serving_generation_installation_tokens: Arc::new(std::sync::atomic::AtomicU64::new(1)),
            generation_publications,
            serving_seats: Arc::new(tokio::sync::watch::Sender::new(0)),
            cadence_telemetry: Arc::new(std::sync::Mutex::new(
                super::CodeIndexCadenceTelemetryV1::default(),
            )),
            activations: Arc::new(std::sync::Mutex::new(std::collections::BTreeMap::new())),
            test_attribution_authorities: Arc::new(std::sync::RwLock::new(
                std::collections::BTreeMap::new(),
            )),
        }
    }

    pub fn process_resident_memory(&self) -> Arc<ProcessResidentMemoryV1> {
        Arc::clone(&self.resident_memory)
    }
}
