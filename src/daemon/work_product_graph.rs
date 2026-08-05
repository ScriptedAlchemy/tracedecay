//! Root-owned Work product adapter over the registered project Grafeo handle.
//!
//! The adapter never discovers or opens a graph database. Project registration
//! resolves the exact store binding through the daemon-wide registry and
//! injects the resulting `Arc<GraphDb>` here.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use thiserror::Error;
use tracedecay_application::{
    WorkEvidenceExpansionV1, WorkEvidencePort, WorkProductApplicationError,
    WorkProductMutationReceiptV1, WorkTopologyCasRequestV1, WorkTopologyCommitV1, WorkTopologyPort,
    WorkTopologyPortError, WorkTopologyReadV1,
};
use tracedecay_domain::{
    TaskEvidenceLinkId, TaskId, WorkAuthority, WorkGraphVersionV1, WorkProductGraphV1,
    WorkTaskEvidenceCoverageV1, WorkTaskEvidenceV1, canonical_json_bytes, canonical_sha256,
};
use tracedecay_graph_db::{
    GraphDb, GraphDbError, GraphDbRegistration, GraphDbRegistry, GraphEntity, GraphEntityId,
    GraphIdempotencyKey, GraphLabel, GraphMutation, GraphNamespace, GraphProjectionId,
    GraphProjectionTelemetryRequest, GraphProperty, GraphPropertyName, GraphPublication,
    GraphPublicationInputDigest, GraphWatermark, GraphWriteBatch, NeverCancelled, SourceGeneration,
};

const GRAPH_NAMESPACE_DOMAIN: &str = "tracedecay.work.product.namespace.v1";
const GRAPH_PROJECTION: &str = "tracedecay.work.product.graph.v1";
const GRAPH_VERSION_LABEL: &str = "tracedecay_work_product_graph_version_v1";
const GRAPH_BYTES_PROPERTY: &str = "canonical_graph_json";
const GRAPH_WATERMARK_PREFIX: &str = "work-product-version-";
const GRAPH_SOURCE_PREFIX: &str = "work-product-source-";
const GRAPH_ENTITY_PREFIX: &str = "work-product-graph-";
const GRAPH_REGISTRATION_DEADLINE: Duration = Duration::from_secs(30);

#[derive(Debug, Error)]
pub(in crate::daemon) enum WorkGraphRegistrationError {
    #[error("registered project store has no canonical root")]
    MissingStoreRoot,
    #[error("Work graph identity is invalid: {0}")]
    InvalidIdentity(String),
    #[error("Work graph registry failed: {0}")]
    Graph(#[from] GraphDbError),
}

pub(in crate::daemon) struct RegisteredWorkGraphAdapter {
    registry: Arc<GraphDbRegistry>,
    registration: GraphDbRegistration,
    graph: Option<Arc<GraphDb>>,
    scope: WorkGraphScope,
    namespace: GraphNamespace,
    projection: GraphProjectionId,
    mutation_gate: Mutex<()>,
    lifecycle: Arc<WorkGraphLifecycleCancellation>,
}

pub(in crate::daemon) type RegisteredWorkProductService =
    tracedecay_application::WorkProductService<
        Arc<RegisteredWorkGraphAdapter>,
        Arc<RegisteredWorkGraphAdapter>,
    >;

#[derive(Clone, Debug, PartialEq, Eq)]
struct WorkGraphScope {
    project_id: String,
    repository_id: String,
    worktree_id: String,
}

#[derive(Debug, Default)]
struct WorkGraphLifecycleCancellation(AtomicBool);

impl WorkGraphLifecycleCancellation {
    fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }
}

impl tracedecay_graph_db::GraphCancellation for WorkGraphLifecycleCancellation {
    fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

impl WorkGraphScope {
    fn from_authority(authority: &WorkAuthority) -> Self {
        Self {
            project_id: authority.project_id().as_str().to_owned(),
            repository_id: authority.repository_id().as_str().to_owned(),
            worktree_id: authority.worktree_id().as_str().to_owned(),
        }
    }

    fn matches(&self, authority: &WorkAuthority) -> bool {
        self.project_id == authority.project_id().as_str()
            && self.repository_id == authority.repository_id().as_str()
            && self.worktree_id == authority.worktree_id().as_str()
    }
}

impl RegisteredWorkGraphAdapter {
    pub(in crate::daemon) fn resolve(
        registry: Arc<GraphDbRegistry>,
        database: &crate::global_db::RegisteredGlobalDb,
        authority: &WorkAuthority,
    ) -> Result<Arc<Self>, WorkGraphRegistrationError> {
        let store_root = database
            .runtime()
            .canonical_path()
            .parent()
            .ok_or(WorkGraphRegistrationError::MissingStoreRoot)?
            .to_path_buf();
        let deadline = registration_deadline()?;
        let lifecycle = Arc::new(WorkGraphLifecycleCancellation::default());
        let registration = GraphDbRegistration {
            binding: database.binding().clone(),
            verified_locator: database.runtime().verified_locator().clone(),
            store_root,
            cancellation: Arc::new(NeverCancelled),
            lifecycle_cancellation: lifecycle.clone(),
            deadline,
        };
        let graph = registry.resolve(registration.clone())?;
        let scope = WorkGraphScope::from_authority(authority);
        let namespace_digest = canonical_sha256(&(
            GRAPH_NAMESPACE_DOMAIN,
            &scope.project_id,
            &scope.repository_id,
            &scope.worktree_id,
        ))
        .map_err(|error| WorkGraphRegistrationError::InvalidIdentity(error.to_string()))?;
        let namespace = GraphNamespace::new(namespace_digest.as_str())?;
        let projection = GraphProjectionId::new(GRAPH_PROJECTION)?;
        Ok(Arc::new(Self {
            registry,
            registration,
            graph: Some(graph),
            scope,
            namespace,
            projection,
            mutation_gate: Mutex::new(()),
            lifecycle,
        }))
    }

    fn graph(&self) -> Result<&Arc<GraphDb>, GraphDbError> {
        self.graph.as_ref().ok_or(GraphDbError::Closed)
    }

    fn authorize_topology(&self, authority: &WorkAuthority) -> Result<(), WorkTopologyPortError> {
        if self.scope.matches(authority) {
            Ok(())
        } else {
            Err(WorkTopologyPortError::NotFoundOrNotAuthorized)
        }
    }

    fn authorize_evidence(
        &self,
        authority: &WorkAuthority,
    ) -> Result<(), WorkProductApplicationError> {
        if self.scope.matches(authority) {
            Ok(())
        } else {
            Err(WorkProductApplicationError::NotFoundOrNotAuthorized)
        }
    }

    fn mutation_guard(&self) -> Result<MutexGuard<'_, ()>, WorkTopologyPortError> {
        self.mutation_gate
            .lock()
            .map_err(|_| WorkTopologyPortError::Unavailable)
    }

    fn current_graph(&self) -> Result<WorkProductGraphV1, WorkTopologyPortError> {
        let telemetry = self
            .graph()
            .map_err(topology_error)?
            .projection_telemetry(GraphProjectionTelemetryRequest {
                namespace: self.namespace.clone(),
                projection: self.projection.clone(),
                cancellation: Arc::new(NeverCancelled),
            })
            .map_err(topology_error)?
            .ok_or(WorkTopologyPortError::NotFoundOrNotAuthorized)?;
        self.graph_at_watermark(&telemetry.watermark)
    }

    fn graph_at_watermark(
        &self,
        watermark: &GraphWatermark,
    ) -> Result<WorkProductGraphV1, WorkTopologyPortError> {
        let version = version_from_watermark(watermark)?;
        let entity = self
            .graph()
            .map_err(topology_error)?
            .entity(
                &self.namespace,
                &graph_entity_id(version)?,
                Arc::new(NeverCancelled),
            )
            .map_err(topology_error)?
            .ok_or(WorkTopologyPortError::Unavailable)?;
        let property = GraphPropertyName::new(GRAPH_BYTES_PROPERTY).map_err(topology_error)?;
        let Some(GraphProperty::Bytes(encoded)) = entity.properties.get(&property) else {
            return Err(WorkTopologyPortError::Unavailable);
        };
        let graph = serde_json::from_slice::<WorkProductGraphV1>(encoded)
            .map_err(|_| WorkTopologyPortError::Unavailable)?;
        if graph.version() != version || graph.validate().is_err() {
            return Err(WorkTopologyPortError::Unavailable);
        }
        Ok(graph)
    }

    fn replay_locked(
        &self,
        command_id: &tracedecay_domain::WorkCommandId,
        input_digest: &tracedecay_domain::ManifestDigest,
    ) -> Result<Option<WorkProductMutationReceiptV1>, WorkTopologyPortError> {
        let key = GraphIdempotencyKey::new(command_id.as_str()).map_err(topology_error)?;
        let Some(receipt) = self
            .graph()
            .map_err(topology_error)?
            .publication_receipt(&self.namespace, &key, Arc::new(NeverCancelled))
            .map_err(topology_error)?
        else {
            return Ok(None);
        };
        if receipt.input_digest.as_str() != input_digest.as_str() {
            return Err(WorkTopologyPortError::IdempotencyConflict);
        }
        let graph = self.graph_at_watermark(&receipt.commit.watermark)?;
        WorkProductMutationReceiptV1::new(graph, false, command_id.clone())
            .map(Some)
            .map_err(|_| WorkTopologyPortError::Unavailable)
    }

    fn publish(
        &self,
        request: &WorkTopologyCasRequestV1,
    ) -> Result<WorkProductMutationReceiptV1, WorkTopologyPortError> {
        let replacement = request.replacement();
        let version = replacement.version();
        let next_watermark = graph_watermark(version)?;
        let source_generation =
            SourceGeneration::new(format!("{GRAPH_SOURCE_PREFIX}{}", version.get()))
                .map_err(topology_error)?;
        let entity = graph_entity(replacement)?;
        let batch = GraphWriteBatch::new(
            self.namespace.clone(),
            self.projection.clone(),
            source_generation.clone(),
            next_watermark.clone(),
            vec![GraphMutation::UpsertEntity(entity)],
            Arc::new(NeverCancelled),
        )
        .map_err(topology_error)?;
        self.graph()
            .map_err(topology_error)?
            .publish(GraphPublication {
                namespace: self.namespace.clone(),
                idempotency_key: GraphIdempotencyKey::new(request.command_id().as_str())
                    .map_err(topology_error)?,
                input_digest: GraphPublicationInputDigest::new(request.input_digest().as_str())
                    .map_err(topology_error)?,
                source_generation,
                expected_watermark: request
                    .expected_version()
                    .map(graph_watermark)
                    .transpose()?,
                next_watermark,
                batch,
                cancellation: Arc::new(NeverCancelled),
            })
            .map_err(topology_error)?;
        WorkProductMutationReceiptV1::new(replacement.clone(), false, request.command_id().clone())
            .map_err(|_| WorkTopologyPortError::Unavailable)
    }

    fn evidence_graph(
        &self,
        authority: &WorkAuthority,
    ) -> Result<WorkProductGraphV1, WorkProductApplicationError> {
        self.authorize_evidence(authority)?;
        self.current_graph().map_err(|error| match error {
            WorkTopologyPortError::NotFoundOrNotAuthorized => {
                WorkProductApplicationError::NotFoundOrNotAuthorized
            }
            WorkTopologyPortError::VersionConflict
            | WorkTopologyPortError::IdempotencyConflict
            | WorkTopologyPortError::Unavailable => {
                WorkProductApplicationError::EvidenceUnavailable
            }
        })
    }
}

impl WorkTopologyPort for RegisteredWorkGraphAdapter {
    fn read(&self, authority: &WorkAuthority) -> Result<WorkTopologyReadV1, WorkTopologyPortError> {
        self.authorize_topology(authority)?;
        self.current_graph().map(WorkTopologyReadV1::Current)
    }

    fn compare_and_swap(
        &self,
        request: &WorkTopologyCasRequestV1,
    ) -> Result<WorkTopologyCommitV1, WorkTopologyPortError> {
        self.authorize_topology(request.authority())?;
        let _guard = self.mutation_guard()?;
        if let Some(receipt) = self.replay_locked(request.command_id(), request.input_digest())? {
            return Ok(WorkTopologyCommitV1::Replayed(receipt));
        }
        self.publish(request).map(WorkTopologyCommitV1::Committed)
    }

    fn replay(
        &self,
        authority: &WorkAuthority,
        command_id: &tracedecay_domain::WorkCommandId,
        input_digest: &tracedecay_domain::ManifestDigest,
    ) -> Result<Option<WorkProductMutationReceiptV1>, WorkTopologyPortError> {
        self.authorize_topology(authority)?;
        let _guard = self.mutation_guard()?;
        self.replay_locked(command_id, input_digest)
    }
}

impl WorkEvidencePort for RegisteredWorkGraphAdapter {
    fn task_evidence(
        &self,
        authority: &WorkAuthority,
        task_id: &TaskId,
        graph_version: WorkGraphVersionV1,
        limit: u32,
    ) -> Result<WorkTaskEvidenceV1, WorkProductApplicationError> {
        let graph = self.evidence_graph(authority)?;
        if graph.version() != graph_version {
            return Err(WorkProductApplicationError::EvidenceUnavailable);
        }
        let available_links = graph
            .evidence()
            .iter()
            .filter(|link| link.task_id() == task_id)
            .cloned()
            .collect::<Vec<_>>();
        let available = u32::try_from(available_links.len())
            .map_err(|_| WorkProductApplicationError::EvidenceUnavailable)?;
        let links = available_links
            .into_iter()
            .take(limit as usize)
            .collect::<Vec<_>>();
        let returned = u32::try_from(links.len())
            .map_err(|_| WorkProductApplicationError::EvidenceUnavailable)?;
        let coverage = if returned == available {
            WorkTaskEvidenceCoverageV1::Complete {
                returned,
                available,
            }
        } else {
            WorkTaskEvidenceCoverageV1::Partial {
                returned,
                available,
                unknowns: BTreeSet::from(["evidence_page_truncated".to_owned()]),
            }
        };
        WorkTaskEvidenceV1::new(task_id.clone(), graph_version, links, coverage)
            .map_err(|_| WorkProductApplicationError::EvidenceUnavailable)
    }

    fn expand(
        &self,
        authority: &WorkAuthority,
        task_id: &TaskId,
        link_id: &TaskEvidenceLinkId,
    ) -> Result<WorkEvidenceExpansionV1, WorkProductApplicationError> {
        let graph = self.evidence_graph(authority)?;
        let link = graph
            .evidence()
            .iter()
            .find(|link| link.task_id() == task_id && link.link_id() == link_id)
            .cloned()
            .ok_or(WorkProductApplicationError::EvidenceUnavailable)?;
        let content_handle = format!("retrieval-anchor:{}", link.anchor_id().as_str());
        WorkEvidenceExpansionV1::new(link, content_handle, false)
    }
}

impl Drop for RegisteredWorkGraphAdapter {
    fn drop(&mut self) {
        let Some(_graph) = self.graph.take() else {
            return;
        };
        drop(_graph);
        self.lifecycle.cancel();
        let Ok(deadline) = registration_deadline() else {
            tracing::warn!(
                event = "work_product_graph_close_failed",
                reason = "deadline_overflow"
            );
            return;
        };
        let mut registration = self.registration.clone();
        registration.cancellation = Arc::new(NeverCancelled);
        registration.deadline = deadline;
        if let Err(error) = self.registry.close(&registration) {
            tracing::warn!(
                event = "work_product_graph_close_failed",
                error = %error
            );
        }
    }
}

fn registration_deadline() -> Result<Instant, WorkGraphRegistrationError> {
    Instant::now()
        .checked_add(GRAPH_REGISTRATION_DEADLINE)
        .ok_or_else(|| {
            WorkGraphRegistrationError::InvalidIdentity(
                "graph registry deadline overflowed".to_owned(),
            )
        })
}

fn graph_entity(graph: &WorkProductGraphV1) -> Result<GraphEntity, WorkTopologyPortError> {
    let encoded = canonical_json_bytes(graph).map_err(|_| WorkTopologyPortError::Unavailable)?;
    GraphEntity::new(
        graph_entity_id(graph.version())?,
        BTreeSet::from([GraphLabel::new(GRAPH_VERSION_LABEL).map_err(topology_error)?]),
        BTreeMap::from([(
            GraphPropertyName::new(GRAPH_BYTES_PROPERTY).map_err(topology_error)?,
            GraphProperty::Bytes(encoded),
        )]),
    )
    .map_err(topology_error)
}

fn graph_entity_id(version: WorkGraphVersionV1) -> Result<GraphEntityId, WorkTopologyPortError> {
    GraphEntityId::new(format!("{GRAPH_ENTITY_PREFIX}{}", version.get())).map_err(topology_error)
}

fn graph_watermark(version: WorkGraphVersionV1) -> Result<GraphWatermark, WorkTopologyPortError> {
    GraphWatermark::new(format!("{GRAPH_WATERMARK_PREFIX}{}", version.get()))
        .map_err(topology_error)
}

fn version_from_watermark(
    watermark: &GraphWatermark,
) -> Result<WorkGraphVersionV1, WorkTopologyPortError> {
    watermark
        .as_str()
        .strip_prefix(GRAPH_WATERMARK_PREFIX)
        .and_then(|value| value.parse::<u64>().ok())
        .and_then(|value| WorkGraphVersionV1::new(value).ok())
        .ok_or(WorkTopologyPortError::Unavailable)
}

fn topology_error(error: GraphDbError) -> WorkTopologyPortError {
    match error {
        GraphDbError::Conflict => WorkTopologyPortError::VersionConflict,
        GraphDbError::Cancelled
        | GraphDbError::DeadlineExceeded
        | GraphDbError::InvalidRequest { .. }
        | GraphDbError::BudgetExhausted
        | GraphDbError::ResetRequired { .. }
        | GraphDbError::Corrupt { .. }
        | GraphDbError::Unavailable { .. }
        | GraphDbError::DurabilityUncertain { .. }
        | GraphDbError::Closed => WorkTopologyPortError::Unavailable,
    }
}
