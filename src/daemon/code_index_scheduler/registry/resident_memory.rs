//! Resident-memory authority construction for code-index scheduler registries.

use std::sync::Arc;

#[cfg(test)]
use tracedecay_runtime_core::resident_memory::DEFAULT_PROCESS_RESIDENT_MEMORY_LIMIT_V1;
pub(super) use tracedecay_runtime_core::resident_memory::ProcessResidentMemoryV1;

use super::CodeIndexSchedulerRegistryV1;

impl CodeIndexSchedulerRegistryV1 {
    #[cfg(test)]
    pub fn new(max_worktrees: usize) -> Self {
        Self::with_resident_memory(
            max_worktrees,
            Arc::new(ProcessResidentMemoryV1::new(
                DEFAULT_PROCESS_RESIDENT_MEMORY_LIMIT_V1,
            )),
        )
    }

    pub fn with_resident_memory(
        max_worktrees: usize,
        resident_memory: Arc<ProcessResidentMemoryV1>,
    ) -> Self {
        let (generation_publications, _) =
            tokio::sync::broadcast::channel(super::GENERATION_PUBLICATION_CHANNEL_CAPACITY);
        Self {
            max_worktrees,
            resident_memory,
            byte_pool: Arc::new(super::SharedCodeIndexBytePoolV1::default()),
            mounted: Arc::new(tokio::sync::Mutex::new(std::collections::BTreeMap::new())),
            retiring: Arc::new(tokio::sync::Mutex::new(std::collections::BTreeMap::new())),
            cold_mount_reservations: Arc::new(std::sync::Mutex::new(
                std::collections::BTreeMap::new(),
            )),
            background_reconcile_admission: Arc::new(tokio::sync::Semaphore::new(
                super::bounded_daemon_admission_permits(),
            )),
            serving_generation_installation_tokens: Arc::new(std::sync::atomic::AtomicU64::new(1)),
            generation_publications,
            cadence_telemetry: Arc::new(std::sync::Mutex::new(
                super::CodeIndexCadenceTelemetryV1::default(),
            )),
            activations: Arc::new(std::sync::Mutex::new(std::collections::BTreeMap::new())),
            test_attribution_authorities: Arc::new(std::sync::RwLock::new(
                std::collections::BTreeMap::new(),
            )),
        }
    }

    #[cfg(test)]
    pub(in crate::daemon) fn resident_memory(&self) -> &Arc<ProcessResidentMemoryV1> {
        &self.resident_memory
    }
}
