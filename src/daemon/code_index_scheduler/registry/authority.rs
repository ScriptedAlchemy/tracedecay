use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, RwLock};

use tracedecay_runtime_core::resident_memory::{
    DEFAULT_PROCESS_RESIDENT_MEMORY_LIMIT_V1, ProcessResidentMemoryV1,
};

use super::{
    CodeIndexCadenceTelemetryV1, CodeIndexSchedulerRegistryV1,
    GENERATION_PUBLICATION_CHANNEL_CAPACITY, bounded_daemon_admission_permits,
};
use crate::daemon::code_index_scheduler::SharedCodeIndexBytePoolV1;

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
            tokio::sync::broadcast::channel(GENERATION_PUBLICATION_CHANNEL_CAPACITY);
        Self {
            max_worktrees,
            resident_memory,
            byte_pool: Arc::new(SharedCodeIndexBytePoolV1::default()),
            mounted: Arc::new(tokio::sync::Mutex::new(BTreeMap::new())),
            mount_admission: Arc::new(tokio::sync::Semaphore::new(
                bounded_daemon_admission_permits(),
            )),
            background_reconcile_admission: Arc::new(tokio::sync::Semaphore::new(
                bounded_daemon_admission_permits(),
            )),
            generation_publications,
            cadence_telemetry: Arc::new(Mutex::new(CodeIndexCadenceTelemetryV1::default())),
            activations: Arc::new(Mutex::new(BTreeMap::new())),
            test_attribution_authorities: Arc::new(RwLock::new(BTreeMap::new())),
        }
    }

    #[cfg(test)]
    pub(in crate::daemon) fn resident_memory(&self) -> &Arc<ProcessResidentMemoryV1> {
        &self.resident_memory
    }
}
