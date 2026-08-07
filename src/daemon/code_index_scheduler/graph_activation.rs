use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use tracedecay_graph_db::{GraphCancellation, SealedGraphStateDigest};

use super::{
    CodeGraphProjectionError, CodeGraphServingAuthorityV1, CodeIndexProductionErrorV1,
    CodeIndexPublicationStoreErrorV1, CodeIndexSchedulerErrorV1, CodeIndexWorktreeSchedulerV1,
    DaemonCodeIndexPublicationStoreV1, DurablePublicationPointerV1, LatestCompleteCodeIndexV1,
};
use crate::code_index::graph_projection::CodeGraphProjectionStore;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CodeGraphReplayBindingV1 {
    pub generations_root: PathBuf,
    pub sealed_state_digest: SealedGraphStateDigest,
}

impl DaemonCodeIndexPublicationStoreV1 {
    pub(super) fn sealed_replay_binding(
        &self,
        generation_id: &tracedecay_domain::CodeGenerationId,
    ) -> Result<CodeGraphReplayBindingV1, CodeIndexPublicationStoreErrorV1> {
        let pointer_bytes = std::fs::read(&self.active_path).map_err(Self::unavailable)?;
        let pointer: DurablePublicationPointerV1 =
            serde_json::from_slice(&pointer_bytes).map_err(|error| {
                Self::unavailable(format!(
                    "active code-generation pointer is corrupt: {error}"
                ))
            })?;
        Self::validate_generation_file(&pointer.generation_file)?;
        if pointer.generation_id != generation_id.as_str() {
            return Err(Self::unavailable(
                "active code-generation pointer names a different generation",
            ));
        }
        let digest = pointer
            .state_digest
            .strip_prefix("sha256:")
            .ok_or_else(|| Self::unavailable("active code-generation digest is not sha256"))?;
        if pointer.generation_file != format!("generation-{digest}.json") {
            return Err(Self::unavailable(
                "active code-generation filename does not match its state digest",
            ));
        }
        Ok(CodeGraphReplayBindingV1 {
            generations_root: self.generations_root.clone(),
            sealed_state_digest: SealedGraphStateDigest::try_from(pointer.state_digest)
                .map_err(Self::unavailable)?,
        })
    }
}

impl CodeIndexWorktreeSchedulerV1 {
    pub(super) fn code_graph_replay_binding(
        &self,
        generation_id: &tracedecay_domain::CodeGenerationId,
    ) -> Result<CodeGraphReplayBindingV1, CodeIndexSchedulerErrorV1> {
        self.publication
            .sealed_replay_binding(generation_id)
            .map_err(|error| CodeIndexProductionErrorV1::Publication(error).into())
    }
}

struct SchedulerGraphCancellation(Arc<AtomicBool>);

impl GraphCancellation for SchedulerGraphCancellation {
    fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

impl LatestCompleteCodeIndexV1 {
    pub(super) fn activate_persistent_graph(
        &self,
        retained: crate::daemon::store_runtime::session_registry::RetainedCodeGraphRuntimeV1,
        cancellation: Arc<AtomicBool>,
    ) -> Result<(), CodeIndexSchedulerErrorV1> {
        let generation_id = self.generation.manifest().generation_id.clone();
        let authority = retained.authority();
        let snapshot = retained
            .publish_verified_snapshot(&self.generation, Arc::clone(&cancellation))
            .map_err(CodeGraphProjectionError::from)?;
        let store =
            CodeGraphProjectionStore::from_verified_snapshot(snapshot, generation_id.clone())?;
        let reader = store.evidence_reader_with_cancellation(
            &generation_id,
            Some(self.generation.snapshot().repository.clone()),
            self.source_freshness()
                .map_err(|error| CodeIndexSchedulerErrorV1::GraphActivation(error.to_string()))?,
            Arc::new(SchedulerGraphCancellation(cancellation)),
        )?;
        self.install_query_owners(reader, CodeGraphServingAuthorityV1::Persistent(authority))
            .map_err(|error| CodeIndexSchedulerErrorV1::GraphActivation(error.to_string()))?;
        let _ = self.generation.admitted_chunks();
        let _ = self.generation.test_attribution_authority();
        let _ = self.record_index();
        Ok(())
    }
}
