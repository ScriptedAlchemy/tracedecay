use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

use tracedecay_domain::canonical_sha256;
use tracedecay_graph_db::{
    GraphDbError, GraphGenerationManifest, GraphIdempotencyKey, GraphProjectionIdentity,
    VerifiedGraphSnapshot,
};
use tracedecay_runtime_core::store::memory::{
    ProjectMemoryGraphReconciliationScheduleV1, schedule_project_memory_graph_reconciliation,
};
use tracedecay_runtime_core::store_runtime::registry::StoreRuntimeKey;
use tracedecay_store::{
    FactReadControl, GraphPublicationInputDigestV1, GraphPublicationKeyV1, StoreShardIdV1,
    StoreShardScopeV1,
};

use super::super::{DaemonSessionRuntimeRegistryV1, Result, session_registry_error};

pub(in crate::daemon::store_runtime::session_registry) fn inline_graph_publication_input_digest(
    publication_key: &GraphPublicationKeyV1,
    manifest: &GraphGenerationManifest,
) -> std::result::Result<GraphPublicationInputDigestV1, GraphDbError> {
    let digest = canonical_sha256(&(
        "tracedecay.inline-graph-publication-input.v1",
        publication_key,
        manifest,
    ))
    .map_err(|error| GraphDbError::invalid(error.to_string()))?;
    GraphPublicationInputDigestV1::new(digest.as_str())
        .map_err(|error| GraphDbError::invalid(error.to_string()))
}

pub(in crate::daemon::store_runtime::session_registry) fn schedule_bound_memory_graph_reconciliation(
    database: &crate::db::Database,
) -> Result<()> {
    match schedule_project_memory_graph_reconciliation(database.clone()) {
        ProjectMemoryGraphReconciliationScheduleV1::Scheduled
        | ProjectMemoryGraphReconciliationScheduleV1::AlreadyScheduled => Ok(()),
        ProjectMemoryGraphReconciliationScheduleV1::Retiring => Err(session_registry_error(
            "schedule verified memory graph reconciliation",
            "memory graph reconciliation is fenced for coordinated retirement".to_owned(),
        )),
        ProjectMemoryGraphReconciliationScheduleV1::NotMounted => Err(session_registry_error(
            "schedule verified memory graph reconciliation",
            "writable memory database has no bound verified graph runtime".to_owned(),
        )),
        ProjectMemoryGraphReconciliationScheduleV1::LifecycleClosed => Err(session_registry_error(
            "schedule verified memory graph reconciliation",
            "memory graph reconciliation lifecycle is closed".to_owned(),
        )),
    }
}
use super::RetainedVerifiedGraphRuntimeV1;

impl RetainedVerifiedGraphRuntimeV1 {
    pub(crate) fn reconcile_verified_manifest(
        &self,
        manifest: &GraphGenerationManifest,
        idempotency_key: GraphIdempotencyKey,
    ) -> std::result::Result<VerifiedGraphSnapshot, GraphDbError> {
        self.publish_verified_manifest(
            manifest,
            idempotency_key,
            Arc::clone(&self.lifecycle_cancelled),
        )
    }
}

impl tracedecay_runtime_core::store_runtime::VerifiedGraphRuntimePortV1
    for RetainedVerifiedGraphRuntimeV1
{
    fn relational_binding(&self) -> &tracedecay_store::StoreRuntimeBindingV1 {
        &self.relational_binding
    }

    fn relational_verified_locator(&self) -> &tracedecay_store::VerifiedStoreLocatorV1 {
        &self.relational_verified_locator
    }

    fn cancel_reconciliation(&self) {
        self.lifecycle_cancelled.store(true, Ordering::Release);
    }

    fn close_reconciliation(&self) -> std::result::Result<(), GraphDbError> {
        RetainedVerifiedGraphRuntimeV1::close_reconciliation(self)
    }

    fn publish_verified_manifest(
        &self,
        manifest: &GraphGenerationManifest,
        idempotency_key: GraphIdempotencyKey,
        cancelled: Arc<AtomicBool>,
    ) -> std::result::Result<VerifiedGraphSnapshot, GraphDbError> {
        RetainedVerifiedGraphRuntimeV1::publish_verified_manifest(
            self,
            manifest,
            idempotency_key,
            cancelled,
        )
    }

    fn reconcile_verified_manifest(
        &self,
        manifest: &GraphGenerationManifest,
        idempotency_key: GraphIdempotencyKey,
    ) -> std::result::Result<VerifiedGraphSnapshot, GraphDbError> {
        RetainedVerifiedGraphRuntimeV1::reconcile_verified_manifest(self, manifest, idempotency_key)
    }

    fn verified_snapshot(
        &self,
        projection: &GraphProjectionIdentity,
        read_control: FactReadControl,
    ) -> std::result::Result<Option<VerifiedGraphSnapshot>, GraphDbError> {
        RetainedVerifiedGraphRuntimeV1::verified_snapshot(self, projection, read_control)
    }
}

impl DaemonSessionRuntimeRegistryV1 {
    pub(crate) async fn retain_memory_graph_runtime(
        &self,
        shard_id: StoreShardIdV1,
        database: Arc<crate::db::Database>,
    ) -> Result<RetainedVerifiedGraphRuntimeV1> {
        if !matches!(
            &shard_id.scope,
            StoreShardScopeV1::Project { .. } | StoreShardScopeV1::ProfileMemory
        ) || shard_id.brain_id != *self.identity.brain_id()
            || shard_id.profile_id != *self.identity.profile_id()
        {
            return Err(session_registry_error(
                "retain verified memory graph authority",
                "memory graph scope does not match the active profile authority".to_owned(),
            ));
        }
        if database.retained_runtime().binding().shard_id != shard_id {
            return Err(session_registry_error(
                "retain verified memory graph authority",
                "memory graph shard does not match the retained relational runtime".to_owned(),
            ));
        }
        let publication_storage = database.graph_publication_storage()?;
        let relational_binding = database.retained_runtime().binding().clone();
        let relational_verified_locator = database.retained_runtime().locator().verified().clone();
        let authority = self
            .registry
            .retain_graph_store(StoreRuntimeKey::new(shard_id, self.incarnation))
            .await
            .map_err(|failure| {
                session_registry_error(
                    "retain verified memory graph authority",
                    format!("{failure:?}"),
                )
            })?;
        Ok(RetainedVerifiedGraphRuntimeV1 {
            graph_registry: self.graph_registry.clone(),
            authority,
            publication_storage,
            relational_binding,
            relational_verified_locator,
            publication_gate: Mutex::new(()),
            lifecycle_cancelled: Arc::new(AtomicBool::new(false)),
        })
    }
}
