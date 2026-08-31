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
use tracedecay_sessions::observation::ObservationCancellation;
use tracedecay_store::{
    FactReadControl, GraphPublicationInputDigestV1, GraphPublicationKeyV1, StoreShardIdV1,
    StoreShardScopeV1,
};

use super::super::{DaemonSessionRuntimeRegistryV1, Result, session_registry_error};

pub fn inline_graph_publication_input_digest(
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

pub fn schedule_bound_memory_graph_reconciliation(
    database: &tracedecay_runtime_core::db::Database,
) -> Result<()> {
    match schedule_project_memory_graph_reconciliation(database.clone()) {
        ProjectMemoryGraphReconciliationScheduleV1::Scheduled
        | ProjectMemoryGraphReconciliationScheduleV1::AlreadyScheduled => Ok(()),
        ProjectMemoryGraphReconciliationScheduleV1::Retiring => Err(session_registry_error(
            "schedule verified memory graph reconciliation",
            "memory graph reconciliation is fenced for runtime retirement".to_owned(),
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
    pub fn reconcile_verified_manifest(
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
    #[cfg(test)]
    pub(crate) async fn retain_memory_graph_runtime(
        &self,
        shard_id: StoreShardIdV1,
        database: tracedecay_runtime_core::db::DatabaseOwnerV1,
    ) -> Result<RetainedVerifiedGraphRuntimeV1> {
        Self::retain_memory_graph_runtime_for_task(
            self.identity.clone(),
            self.registry.clone(),
            self.graph_registry.clone(),
            Arc::clone(&self.graph_lifecycle_cancelled),
            self.incarnation,
            shard_id,
            database,
            ObservationCancellation::default(),
        )
        .await
        .map_err(|failure| failure.error)
    }

    pub(crate) async fn retain_memory_graph_runtime_for_task(
        identity: super::super::LocalProfileIdentityAuthorityV1,
        registry: tracedecay_runtime_core::store_runtime::registry::StoreRuntimeRegistry,
        graph_registry: tracedecay_graph_db::GraphDbRegistry,
        graph_lifecycle_cancelled: Arc<AtomicBool>,
        incarnation: tracedecay_store::StoreIncarnationV1,
        shard_id: StoreShardIdV1,
        database: tracedecay_runtime_core::db::DatabaseOwnerV1,
        cancellation: ObservationCancellation,
    ) -> std::result::Result<RetainedVerifiedGraphRuntimeV1, MemoryGraphRuntimeOpenFailureV1> {
        if !matches!(
            &shard_id.scope,
            StoreShardScopeV1::Project { .. } | StoreShardScopeV1::ProfileMemory
        ) || shard_id.brain_id != *identity.brain_id()
            || shard_id.profile_id != *identity.profile_id()
        {
            return Err(MemoryGraphRuntimeOpenFailureV1 {
                database,
                error: session_registry_error(
                    "retain verified memory graph authority",
                    "memory graph scope does not match the active profile authority".to_owned(),
                ),
            });
        }
        if database.registered_binding().shard_id != shard_id {
            return Err(MemoryGraphRuntimeOpenFailureV1 {
                database,
                error: session_registry_error(
                    "retain verified memory graph authority",
                    "memory graph shard does not match the retained relational runtime".to_owned(),
                ),
            });
        }
        let relational_binding = database.registered_binding().clone();
        let relational_verified_locator = database.registered_verified_locator().clone();
        let opened = super::graph_attachment::open_session_relation_owner_for_task(
            &registry,
            &graph_registry,
            &graph_lifecycle_cancelled,
            cancellation,
            incarnation,
            shard_id,
        )
        .await;
        let (graph, store_target) = match opened {
            Ok(opened) => opened,
            Err(error) => return Err(MemoryGraphRuntimeOpenFailureV1 { database, error }),
        };
        Ok(RetainedVerifiedGraphRuntimeV1 {
            graph_registry,
            database,
            graph,
            store_target: Mutex::new(Some(store_target)),
            relational_binding,
            relational_verified_locator,
            operation_admission: Mutex::new(super::MemoryGraphOperationAdmissionV1::Ready),
            publication_gate: Mutex::new(()),
            lifecycle_cancelled: Arc::new(AtomicBool::new(false)),
            registry_lifecycle_cancelled: graph_lifecycle_cancelled,
        })
    }
}

pub(crate) struct MemoryGraphRuntimeOpenFailureV1 {
    pub(crate) database: tracedecay_runtime_core::db::DatabaseOwnerV1,
    pub(crate) error: tracedecay_domain::errors::TraceDecayError,
}
