use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use sha2::{Digest, Sha256};
use tracedecay_domain::{
    FactAssertionId, FactCurationActionV1, FactId, FactLineageEventKindV1, FactLineageEventV1,
    FactOwnerV1, FactPayloadV1, FactRelationKindV1, ProjectMemoryGraphRelationKindV1,
    RetrievalAnchorId,
};
use tracedecay_graph_db::{
    GraphBudgetKind, GraphEntityId, GraphIdempotencyKey, GraphNamespace, GraphProjectionId,
    GraphProjectionIdentity, GraphProjectionReadRequest, GraphRelationKind, GraphRelationRef,
    GraphTraversalDirection, MAX_VERIFIED_GENERATION_ENTITIES, MAX_VERIFIED_GENERATION_RELATIONS,
    TraversalRequest,
};
use tracedecay_store::{
    FactReadControl, FactStoreError, FactStoreResult, ProjectMemoryEntityIdV1,
    ProjectMemoryFactIdV1, ProjectMemoryFactProjectionV1, ProjectMemoryGraphPageV1,
    ProjectMemoryGraphQueryV1, ProjectMemoryGraphRelationV1, ProjectMemoryGraphTargetV1,
    StoreShardScopeV1,
};

use crate::db::Database;
use crate::db::engine::params;

use super::envelope::finish_read_snapshot;
use super::graph_manifest::{MemoryGraphSource, SourceRelation, build_manifest, source_watermark};
use super::primitives::{
    OwnerKey, row_optional_string, row_string, storage_error, storage_message,
};
use super::projection::load_project_memory_projections_controlled_tx;

const OPERATION: &str = "project memory relation graph";
const PROJECTION: &str = "project-memory-relations";
const CONTRADICTS: &str = "memory-contradicts";
const SUPERSEDES: &str = "memory-supersedes";
const SUPPORTS: &str = "memory-supports";
const DERIVED_FROM: &str = "memory-derived-from";
const MENTIONS: &str = "memory-mentions";
const ACTIVE_ASSERTION: &str = "memory-active-assertion";
const EVIDENCE_ANCHOR: &str = "memory-evidence-anchor";
pub(super) struct ProjectedRelation {
    pub(super) source: GraphEntityId,
    pub(super) target: GraphEntityId,
    pub(super) kind: GraphRelationKind,
}

struct SharedGraphCancellation(FactReadControl);

impl tracedecay_graph_db::GraphCancellation for SharedGraphCancellation {
    fn is_cancelled(&self) -> bool {
        self.0.interrupted()
    }
}

#[derive(Default)]
struct SourceLoadMeasurement {
    rows: u64,
    bytes: u64,
}

impl SourceLoadMeasurement {
    fn record_row(&mut self, values: &[&str]) -> FactStoreResult<()> {
        self.rows = self.rows.checked_add(1).ok_or_else(|| {
            storage_message(
                OPERATION,
                "project memory reconciliation source row counter overflowed",
            )
        })?;
        for value in values {
            let bytes = u64::try_from(value.len()).map_err(|_| {
                storage_message(
                    OPERATION,
                    "project memory reconciliation source byte counter overflowed",
                )
            })?;
            self.bytes = self.bytes.checked_add(bytes).ok_or_else(|| {
                storage_message(
                    OPERATION,
                    "project memory reconciliation source byte counter overflowed",
                )
            })?;
        }
        Ok(())
    }
}

pub(super) async fn project_memory_graph(
    db: &Database,
    query: ProjectMemoryGraphQueryV1,
    read_control: &FactReadControl,
) -> FactStoreResult<ProjectMemoryGraphPageV1> {
    let owner = query.owner().clone();
    let fact_runtime =
        super::runtime::retained_fact_runtime(db)?.ok_or(FactStoreError::GraphUnavailable)?;
    super::runtime::validate_owner_binding(fact_runtime.binding(), &owner, OPERATION)?;
    let runtime = db
        .memory_graph_runtime()
        .ok_or(FactStoreError::GraphUnavailable)?;
    let namespace = namespace(&owner)?;
    let projection =
        GraphProjectionId::new(PROJECTION).map_err(|error| graph_error(&owner, error))?;
    let projection_identity = GraphProjectionIdentity::new(namespace.clone(), projection.clone());
    ensure_not_cancelled(read_control)?;
    let source = load_source(db, &owner, Some(read_control), None).await?;
    let expected_watermark = source_watermark(&owner, &source, Some(read_control))?;
    let expected_manifest = build_manifest(
        &owner,
        projection_identity.clone(),
        &source,
        expected_watermark.clone(),
        Some(read_control),
    )?;
    let expected_generation = expected_manifest.generation;
    ensure_source_read_active(Some(read_control))?;
    let runtime_for_read = Arc::clone(&runtime);
    let control_for_snapshot = read_control.clone();
    let projection_for_snapshot = projection_identity.clone();
    let verified_snapshot = tokio::task::spawn_blocking(move || {
        runtime_for_read.verified_snapshot(&projection_for_snapshot, control_for_snapshot)
    })
    .await
    .map_err(|error| storage_error(OPERATION, error))?
    .map_err(|error| graph_error(&owner, error))?
    .ok_or(FactStoreError::GraphUnavailable)?;
    if verified_snapshot.projection() != &projection_identity
        || verified_snapshot.generation() != &expected_generation
    {
        return Err(FactStoreError::GraphConflict);
    }

    let max_relations = query.max_relations();
    let roots = query.roots().to_vec();
    let hydration_roots = roots.clone();
    let snapshot_for_read = verified_snapshot;
    let control_for_read = read_control.clone();
    let namespace_for_read = namespace.clone();
    let projection_for_read = projection.clone();
    let projection_identity_for_read = projection_identity;
    let page = tokio::task::spawn_blocking(move || {
        let cancellation: Arc<dyn tracedecay_graph_db::GraphCancellation> =
            Arc::new(SharedGraphCancellation(control_for_read));
        let max_page = max_relations.checked_add(1).ok_or_else(|| {
            tracedecay_graph_db::GraphDbError::budget_exhausted_count(
                GraphBudgetKind::Read,
                max_relations,
            )
        })?;
        let relations = if roots.is_empty() {
            let projection_page =
                snapshot_for_read.read_projection(GraphProjectionReadRequest {
                    namespace: namespace_for_read,
                    projection: projection_for_read,
                    after_entity: None,
                    after_relation: None,
                    max_entities: 0,
                    max_relations: max_page,
                    cancellation,
                })?;
            if projection_page.next_relation.is_some() {
                return Err(tracedecay_graph_db::GraphDbError::budget_exhausted_count(
                    GraphBudgetKind::Read,
                    max_relations,
                ));
            }
            projection_page
                .relations
                .into_iter()
                .map(|relation| ProjectedRelation {
                    source: relation.from,
                    target: relation.to,
                    kind: relation.kind,
                })
                .collect()
        } else {
            let relation_kinds = relation_kinds()?;
            let relation_sentinel = max_relations.checked_add(1).ok_or_else(|| {
                tracedecay_graph_db::GraphDbError::budget_exhausted_count(
                    GraphBudgetKind::Read,
                    max_relations,
                )
            })?;
            let entity_sentinel = relation_sentinel.checked_add(1).ok_or_else(|| {
                tracedecay_graph_db::GraphDbError::budget_exhausted_count(
                    GraphBudgetKind::Read,
                    max_relations,
                )
            })?;
            let mut accepted_entities = BTreeSet::new();
            for root in roots {
                let start = fact_entity_id(&root)?;
                let result = snapshot_for_read.traverse(TraversalRequest {
                    namespace: namespace_for_read.clone(),
                    start,
                    relation_kinds: relation_kinds.clone(),
                    direction: GraphTraversalDirection::Both,
                    max_depth: relation_sentinel,
                    max_visits: entity_sentinel,
                    max_results: entity_sentinel,
                    cancellation: Arc::clone(&cancellation),
                })?;
                if result.visits.len() == entity_sentinel {
                    return Err(tracedecay_graph_db::GraphDbError::budget_exhausted_count(
                        GraphBudgetKind::Read,
                        max_relations,
                    ));
                }
                accepted_entities
                    .extend(result.visits.into_iter().map(|visit| visit.entity.identity));
            }
            let starts = accepted_entities.iter().cloned().collect::<Vec<_>>();
            let relation_ids = snapshot_for_read.outgoing_relation_ids(
                &starts,
                &relation_kinds,
                relation_sentinel,
                Arc::clone(&cancellation),
            )?;
            let relation_ids = relation_ids.into_iter().flatten().collect::<BTreeSet<_>>();
            let mut relations = Vec::with_capacity(relation_ids.len());
            for relation_id in relation_ids {
                let reference =
                    GraphRelationRef::new(projection_identity_for_read.clone(), relation_id);
                let relation = snapshot_for_read
                    .relation(&reference, Arc::clone(&cancellation))?
                    .ok_or(tracedecay_graph_db::GraphDbError::Conflict)?;
                relations.push(ProjectedRelation {
                    source: relation.from.identity,
                    target: relation.to.identity,
                    kind: relation.kind,
                });
            }
            validate_rooted_relations(&accepted_entities, relations, max_relations)?
        };
        if relations.len() > max_relations {
            return Err(tracedecay_graph_db::GraphDbError::budget_exhausted_count(
                GraphBudgetKind::Read,
                max_relations,
            ));
        }
        Ok::<_, tracedecay_graph_db::GraphDbError>(relations)
    })
    .await
    .map_err(|error| storage_error(OPERATION, error))?
    .map_err(|error| graph_error(&owner, error))?;

    ensure_not_cancelled(read_control)?;
    let hydrated = hydrate_page(db, owner.clone(), &hydration_roots, page, read_control).await?;
    if source_watermark(
        &owner,
        &load_source(db, &owner, Some(read_control), None).await?,
        Some(read_control),
    )? != expected_watermark
    {
        return Err(FactStoreError::GraphConflict);
    }
    Ok(hydrated)
}

pub(super) fn schedule_project_memory_graph_reconciliation(
    db: Database,
) -> super::ProjectMemoryGraphReconciliationScheduleV1 {
    if db.memory_graph_runtime().is_none() {
        return super::ProjectMemoryGraphReconciliationScheduleV1::NotMounted;
    }
    match db.schedule_memory_graph_reconciliation(|weak_db| async move {
        let Some(db) = weak_db.upgrade() else {
            return true;
        };
        match reconcile_project_memory_graph_pass(&db).await {
            Ok(()) => true,
            Err(error) => {
                tracing::warn!(
                    error_kind = reconciliation_error_kind(&error),
                    "project memory graph reconciliation remains pending"
                );
                false
            }
        }
    }) {
        crate::db::MemoryGraphReconciliationTaskScheduleV1::Scheduled => {
            super::ProjectMemoryGraphReconciliationScheduleV1::Scheduled
        }
        crate::db::MemoryGraphReconciliationTaskScheduleV1::AlreadyScheduled => {
            super::ProjectMemoryGraphReconciliationScheduleV1::AlreadyScheduled
        }
        crate::db::MemoryGraphReconciliationTaskScheduleV1::Retiring => {
            super::ProjectMemoryGraphReconciliationScheduleV1::Retiring
        }
        crate::db::MemoryGraphReconciliationTaskScheduleV1::Closed => {
            super::ProjectMemoryGraphReconciliationScheduleV1::LifecycleClosed
        }
    }
}

async fn reconcile_project_memory_graph_pass(db: &Database) -> FactStoreResult<()> {
    let _pass = db
        .begin_project_memory_reconciliation_pass()
        .map_err(|counter| {
            storage_message(
                OPERATION,
                format!("project memory reconciliation telemetry overflowed: {counter}"),
            )
        })?;
    let owner = bound_owner(db)?;
    let runtime = db
        .memory_graph_runtime()
        .ok_or(FactStoreError::GraphUnavailable)?;
    let fact_runtime =
        super::runtime::retained_fact_runtime(db)?.ok_or(FactStoreError::GraphUnavailable)?;
    super::runtime::validate_owner_binding(fact_runtime.binding(), &owner, OPERATION)?;
    let projection = GraphProjectionIdentity::new(
        namespace(&owner)?,
        GraphProjectionId::new(PROJECTION).map_err(|error| graph_error(&owner, error))?,
    );
    let source = load_source(db, &owner, None, Some(db)).await?;
    let watermark = source_watermark(&owner, &source, None)?;
    let manifest = build_manifest(&owner, projection.clone(), &source, watermark.clone(), None)?;
    let expected_generation = manifest.generation.clone();
    let idempotency_key =
        GraphIdempotencyKey::new(format!("publish:{}", expected_generation.as_str()))
            .map_err(|error| graph_error(&owner, error))?;
    db.project_memory_reconciliation_telemetry()
        .record_publication_attempt()
        .map_err(|counter| {
            storage_message(
                OPERATION,
                format!("project memory reconciliation telemetry overflowed: {counter}"),
            )
        })?;
    let runtime_for_reconciliation = Arc::clone(&runtime);
    let snapshot = match tokio::task::spawn_blocking(move || {
        runtime_for_reconciliation.reconcile_verified_manifest(&manifest, idempotency_key)
    })
    .await
    .map_err(|error| storage_error(OPERATION, error))?
    {
        Ok(snapshot) => snapshot,
        Err(error) => {
            let mapped = graph_error(&owner, error);
            if matches!(mapped, FactStoreError::GraphConflict)
                && verified_head_matches_expected(
                    &runtime,
                    &projection,
                    &expected_generation,
                    &owner,
                )
                .await?
            {
                return finish_reconciliation_watermark(db, &owner, watermark).await;
            }
            return Err(mapped);
        }
    };
    if snapshot.projection() != &projection || snapshot.generation() != &expected_generation {
        if verified_head_matches_expected(&runtime, &projection, &expected_generation, &owner)
            .await?
        {
            return finish_reconciliation_watermark(db, &owner, watermark).await;
        }
        return Err(FactStoreError::GraphConflict);
    }
    finish_reconciliation_watermark(db, &owner, watermark).await
}

async fn finish_reconciliation_watermark(
    db: &Database,
    owner: &FactOwnerV1,
    watermark: tracedecay_graph_db::GraphWatermark,
) -> FactStoreResult<()> {
    if source_watermark(owner, &load_source(db, owner, None, Some(db)).await?, None)? != watermark
        && !db.memory_graph_reconciliation_pending()
    {
        return Err(FactStoreError::GraphConflict);
    }
    Ok(())
}

async fn verified_head_matches_expected(
    runtime: &Arc<dyn crate::store_runtime::VerifiedGraphRuntimePortV1>,
    projection: &GraphProjectionIdentity,
    expected_generation: &tracedecay_graph_db::GraphGenerationId,
    owner: &FactOwnerV1,
) -> FactStoreResult<bool> {
    let runtime = Arc::clone(runtime);
    let projection_for_read = projection.clone();
    let snapshot = tokio::task::spawn_blocking(move || {
        runtime.verified_snapshot(
            &projection_for_read,
            FactReadControl::new(Arc::new(|| false)),
        )
    })
    .await
    .map_err(|error| storage_error(OPERATION, error))?
    .map_err(|error| graph_error(owner, error))?;
    Ok(snapshot.is_some_and(|snapshot| {
        snapshot.projection() == projection && snapshot.generation() == expected_generation
    }))
}

pub(super) async fn publish_project_memory_graph_after_write(db: Database) {
    match reconcile_project_memory_graph_pass(&db).await {
        Ok(()) => {}
        Err(error) => {
            tracing::warn!(
                error_kind = reconciliation_error_kind(&error),
                "project memory graph publication after write remains pending"
            );
            schedule_project_memory_graph_reconciliation(db);
        }
    }
}

fn bound_owner(db: &Database) -> FactStoreResult<FactOwnerV1> {
    match &db.retained_runtime().binding().shard_id.scope {
        StoreShardScopeV1::ProfileMemory => Ok(FactOwnerV1::Profile),
        StoreShardScopeV1::Project { project_id } => Ok(FactOwnerV1::Project {
            project_id: project_id.clone(),
        }),
        _ => Err(FactStoreError::GraphUnavailable),
    }
}

const fn reconciliation_error_kind(error: &FactStoreError) -> &'static str {
    match error {
        FactStoreError::GraphConflict => "conflict",
        FactStoreError::GraphUnavailable => "unavailable",
        FactStoreError::GraphCancelled => "cancelled",
        FactStoreError::GraphBudgetExhausted => "budget_exhausted",
        FactStoreError::GraphDeadlineExceeded => "deadline_exceeded",
        FactStoreError::GraphResetRequired { .. } => "reset_required",
        FactStoreError::OwnerMismatch => "owner_mismatch",
        FactStoreError::Storage { .. } => "storage",
        _ => "canonical_source",
    }
}

pub(super) fn validate_rooted_relations(
    accepted_entities: &BTreeSet<GraphEntityId>,
    relations: Vec<ProjectedRelation>,
    max_relations: usize,
) -> Result<Vec<ProjectedRelation>, tracedecay_graph_db::GraphDbError> {
    if relations.iter().any(|relation| {
        !accepted_entities.contains(&relation.source)
            || !accepted_entities.contains(&relation.target)
    }) {
        return Err(tracedecay_graph_db::GraphDbError::Conflict);
    }
    if relations.len() > max_relations {
        return Err(tracedecay_graph_db::GraphDbError::budget_exhausted_count(
            GraphBudgetKind::Read,
            max_relations,
        ));
    }
    Ok(relations)
}

fn ensure_not_cancelled(read_control: &FactReadControl) -> FactStoreResult<()> {
    if read_control.interrupted() {
        return Err(FactStoreError::GraphCancelled);
    }
    Ok(())
}

fn ensure_source_read_active(read_control: Option<&FactReadControl>) -> FactStoreResult<()> {
    if read_control.is_some_and(FactReadControl::interrupted) {
        return Err(FactStoreError::ReadCancelled);
    }
    Ok(())
}

async fn load_source(
    db: &Database,
    owner: &FactOwnerV1,
    read_control: Option<&FactReadControl>,
    telemetry_database: Option<&Database>,
) -> FactStoreResult<MemoryGraphSource> {
    ensure_source_read_active(read_control)?;
    let key = OwnerKey::new(owner)?;
    let transaction = db
        .begin_memory_read_transaction(OPERATION)
        .await
        .map_err(|error| storage_error(OPERATION, error))?;
    let mut source_load = SourceLoadMeasurement::default();
    let result = async {
        let mut entities = Vec::new();
        let mut all_fact_ids = BTreeSet::new();
        let mut fact_ids = BTreeSet::new();
        let mut active_assertions = BTreeMap::new();
        let mut rows = transaction
            .query(
                "SELECT fact_id
                 FROM memory_v2_facts
                 WHERE owner_kind = ?1 AND project_id = ?2 AND owner_json = ?3
                 ORDER BY fact_id",
                params![key.kind, key.project_id.as_str(), key.json.as_str()],
            )
            .await
            .map_err(|error| storage_error(OPERATION, error))?;
        ensure_source_read_active(read_control)?;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| storage_error(OPERATION, error))?
        {
            ensure_source_read_active(read_control)?;
            let fact_id = row_string(&row, 0, OPERATION)?;
            source_load.record_row(&[fact_id.as_str()])?;
            let fact_id = FactId::new(fact_id)?;
            fact_id
                .validate_owner(owner)
                .map_err(|_| FactStoreError::OwnerMismatch)?;
            if !all_fact_ids.insert(fact_id) {
                return Err(storage_message(
                    OPERATION,
                    "canonical memory source contains a duplicate fact identity",
                ));
            }
        }
        drop(rows);
        let mut rows = transaction
            .query(
                "SELECT current_facts.fact_id, current_facts.active_assertion_id
                 FROM memory_v2_current_facts AS current_facts
                 JOIN memory_v2_facts AS facts
                   ON facts.fact_id = current_facts.fact_id
                  AND facts.owner_kind = current_facts.owner_kind
                  AND facts.project_id = current_facts.project_id
                 WHERE current_facts.owner_kind = ?1 AND current_facts.project_id = ?2
                   AND facts.owner_json = ?3
                   AND current_facts.payload_access = 'eligible'
                 ORDER BY current_facts.fact_id",
                params![key.kind, key.project_id.as_str(), key.json.as_str()],
            )
            .await
            .map_err(|error| storage_error(OPERATION, error))?;
        ensure_source_read_active(read_control)?;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| storage_error(OPERATION, error))?
        {
            ensure_source_read_active(read_control)?;
            let fact_id = row_string(&row, 0, OPERATION)?;
            let active_assertion = row_optional_string(&row, 1, OPERATION)?;
            if let Some(assertion) = active_assertion.as_deref() {
                source_load.record_row(&[fact_id.as_str(), assertion])?;
            } else {
                source_load.record_row(&[fact_id.as_str()])?;
            }
            let fact_id = FactId::new(fact_id)?;
            ensure_projected_fact_exists(&all_fact_ids, owner, &fact_id)?;
            let active_assertion = active_assertion.ok_or(FactStoreError::PayloadAccessMismatch)?;
            push_source_entity(&mut entities, fact_entity_id_from_str(fact_id.as_str())?)?;
            active_assertions.insert(fact_id.clone(), active_assertion);
            fact_ids.insert(fact_id);
        }
        drop(rows);
        let mut relations = BTreeSet::new();
        let mut rows = transaction
            .query(
                "SELECT fact_id, event_json
                 FROM memory_v2_lineage_events
                 WHERE owner_kind = ?1 AND project_id = ?2
                   AND json_extract(event_json, '$.kind.kind') = 'curated'
                   AND json_extract(event_json, '$.kind.action.kind') IN (
                       'contradicted_by', 'superseded_by', 'merged_into', 'linked'
                   )
                 ORDER BY fact_id, event_sequence",
                params![key.kind, key.project_id.as_str()],
            )
            .await
            .map_err(|error| storage_error(OPERATION, error))?;
        ensure_source_read_active(read_control)?;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| storage_error(OPERATION, error))?
        {
            ensure_source_read_active(read_control)?;
            let stored_fact_id = row_string(&row, 0, OPERATION)?;
            let event_json = row_string(&row, 1, OPERATION)?;
            source_load.record_row(&[stored_fact_id.as_str(), event_json.as_str()])?;
            let stored_fact_id = FactId::new(stored_fact_id)?;
            let event = serde_json::from_str::<FactLineageEventV1>(&event_json)
                .map_err(|error| storage_error(OPERATION, error))?;
            if event.owner() != owner || event.fact_id() != &stored_fact_id {
                return Err(storage_message(
                    OPERATION,
                    "canonical lineage event does not match its owner-scoped storage key",
                ));
            }
            let (source_fact_id, target_fact_id, kind, evidence_visible) = match event.kind() {
                FactLineageEventKindV1::Curated {
                    action: FactCurationActionV1::ContradictedBy { fact_id },
                    ..
                } => (&stored_fact_id, fact_id, CONTRADICTS, true),
                FactLineageEventKindV1::Curated {
                    action:
                        FactCurationActionV1::SupersededBy { fact_id }
                        | FactCurationActionV1::MergedInto { fact_id },
                    ..
                } => (fact_id, &stored_fact_id, SUPERSEDES, true),
                FactLineageEventKindV1::Curated {
                    action: FactCurationActionV1::Linked { relation },
                    ..
                } => {
                    if relation.owner() != owner || relation.source_fact_id() != &stored_fact_id {
                        return Err(storage_message(
                            OPERATION,
                            "canonical linked relation does not match its lineage authority",
                        ));
                    }
                    let mut evidence_visible = true;
                    for evidence_fact_id in relation.evidence_fact_ids() {
                        ensure_projected_fact_exists(&all_fact_ids, owner, evidence_fact_id)?;
                        evidence_visible &= fact_ids.contains(evidence_fact_id);
                    }
                    let kind = match relation.kind() {
                        FactRelationKindV1::Supports => SUPPORTS,
                        FactRelationKindV1::Contradicts => CONTRADICTS,
                        FactRelationKindV1::Supersedes => SUPERSEDES,
                        FactRelationKindV1::DerivedFrom => DERIVED_FROM,
                    };
                    (
                        relation.source_fact_id(),
                        relation.target_fact_id(),
                        kind,
                        evidence_visible,
                    )
                }
                _ => {
                    return Err(storage_message(
                        OPERATION,
                        "canonical lineage relation query returned an unsupported event",
                    ));
                }
            };
            ensure_projected_fact_exists(&all_fact_ids, owner, source_fact_id)?;
            ensure_projected_fact_exists(&all_fact_ids, owner, target_fact_id)?;
            if !evidence_visible
                || !fact_ids.contains(source_fact_id)
                || !fact_ids.contains(target_fact_id)
            {
                continue;
            }
            push_source_relation(
                &mut relations,
                SourceRelation {
                    source: fact_entity_id_from_str(source_fact_id.as_str())?,
                    target: fact_entity_id_from_str(target_fact_id.as_str())?,
                    kind: kind.to_owned(),
                },
            )?;
        }
        drop(rows);
        for (fact, assertion) in &active_assertions {
            ensure_source_read_active(read_control)?;
            push_source_relation(
                &mut relations,
                SourceRelation {
                    source: fact_entity_id_from_str(fact.as_str())?,
                    target: assertion_entity_id_from_str(fact.as_str(), assertion)?,
                    kind: ACTIVE_ASSERTION.to_owned(),
                },
            )?;
        }
        let mut rows = transaction
            .query(
                "SELECT assertion_evidence.fact_id, assertion_evidence.assertion_id,
                        evidence.anchor_id
                 FROM memory_v2_assertion_evidence AS assertion_evidence
                 JOIN memory_v2_evidence AS evidence
                   ON evidence.evidence_id = assertion_evidence.evidence_id
                  AND evidence.fact_id = assertion_evidence.fact_id
                  AND evidence.owner_kind = assertion_evidence.owner_kind
                  AND evidence.project_id = assertion_evidence.project_id
                 JOIN memory_v2_current_facts AS current_facts
                   ON current_facts.fact_id = assertion_evidence.fact_id
                  AND current_facts.owner_kind = assertion_evidence.owner_kind
                  AND current_facts.project_id = assertion_evidence.project_id
                  AND current_facts.active_assertion_id = assertion_evidence.assertion_id
                 WHERE assertion_evidence.owner_kind = ?1
                   AND assertion_evidence.project_id = ?2
                   AND current_facts.payload_access = 'eligible'
                 ORDER BY assertion_evidence.fact_id, assertion_evidence.assertion_id,
                          assertion_evidence.ordinal",
                params![key.kind, key.project_id.as_str()],
            )
            .await
            .map_err(|error| storage_error(OPERATION, error))?;
        ensure_source_read_active(read_control)?;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| storage_error(OPERATION, error))?
        {
            ensure_source_read_active(read_control)?;
            let fact = row_string(&row, 0, OPERATION)?;
            let assertion = row_string(&row, 1, OPERATION)?;
            let anchor = row_string(&row, 2, OPERATION)?;
            source_load.record_row(&[fact.as_str(), assertion.as_str(), anchor.as_str()])?;
            let fact = FactId::new(fact)?;
            ensure_projected_fact_exists(&fact_ids, owner, &fact)?;
            push_source_relation(
                &mut relations,
                SourceRelation {
                    source: assertion_entity_id_from_str(fact.as_str(), &assertion)?,
                    target: anchor_entity_id_from_str(&anchor)?,
                    kind: EVIDENCE_ANCHOR.to_owned(),
                },
            )?;
        }
        drop(rows);
        let mut rows = transaction
            .query(
                "SELECT current_facts.fact_id, payloads.payload_json
                 FROM memory_v2_current_facts AS current_facts
                 LEFT JOIN memory_v2_assertion_payloads AS payloads
                   ON payloads.assertion_id = current_facts.active_assertion_id
                  AND payloads.fact_id = current_facts.fact_id
                  AND payloads.owner_kind = current_facts.owner_kind
                  AND payloads.project_id = current_facts.project_id
                 WHERE current_facts.owner_kind = ?1 AND current_facts.project_id = ?2
                   AND current_facts.payload_access = 'eligible'
                 ORDER BY current_facts.fact_id",
                params![key.kind, key.project_id.as_str()],
            )
            .await
            .map_err(|error| storage_error(OPERATION, error))?;
        ensure_source_read_active(read_control)?;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| storage_error(OPERATION, error))?
        {
            ensure_source_read_active(read_control)?;
            let fact = row_string(&row, 0, OPERATION)?;
            let payload_json = row_optional_string(&row, 1, OPERATION)?;
            if let Some(payload_json) = payload_json.as_deref() {
                source_load.record_row(&[fact.as_str(), payload_json])?;
            } else {
                source_load.record_row(&[fact.as_str()])?;
            }
            let fact = FactId::new(fact)?;
            ensure_projected_fact_exists(&fact_ids, owner, &fact)?;
            let payload_json = payload_json.ok_or(FactStoreError::PayloadAccessMismatch)?;
            let payload = serde_json::from_str::<FactPayloadV1>(&payload_json)
                .map_err(|error| storage_error(OPERATION, error))?;
            for entity in payload.entities() {
                ensure_source_read_active(read_control)?;
                let target = ProjectMemoryEntityIdV1::new(owner.clone(), entity.clone())?;
                push_source_relation(
                    &mut relations,
                    SourceRelation {
                        source: fact_entity_id_from_str(fact.as_str())?,
                        target: entity_entity_id(&target),
                        kind: MENTIONS.to_owned(),
                    },
                )?;
            }
        }
        ensure_source_read_active(read_control)?;
        Ok(MemoryGraphSource {
            owner: key.json,
            entities,
            relations,
        })
    }
    .await;
    let source_result = finish_read_snapshot(transaction, result).await;
    let telemetry_result = if let Some(telemetry_database) = telemetry_database {
        telemetry_database
            .project_memory_reconciliation_telemetry()
            .record_source_load(source_load.rows, source_load.bytes)
            .map_err(|counter| {
                storage_message(
                    OPERATION,
                    format!("project memory reconciliation telemetry overflowed: {counter}"),
                )
            })
    } else {
        Ok(())
    };
    match (source_result, telemetry_result) {
        (Ok(source), Ok(())) => Ok(source),
        (Ok(_), Err(telemetry_error)) => Err(telemetry_error),
        (Err(source_error), Ok(())) => Err(source_error),
        (Err(source_error), Err(telemetry_error)) => Err(storage_message(
            OPERATION,
            format!(
                "{source_error}; reconciliation source telemetry also failed: {telemetry_error}"
            ),
        )),
    }
}

fn ensure_projected_fact_exists(
    facts: &BTreeSet<FactId>,
    owner: &FactOwnerV1,
    fact_id: &FactId,
) -> FactStoreResult<()> {
    fact_id
        .validate_owner(owner)
        .map_err(|_| FactStoreError::OwnerMismatch)?;
    if !facts.contains(fact_id) {
        return Err(FactStoreError::FactNotFound {
            fact_id: fact_id.clone(),
        });
    }
    Ok(())
}

#[cfg(test)]
pub(in crate::store::memory) async fn relation_kinds_from_canonical_source_for_test(
    db: &Database,
    owner: &FactOwnerV1,
    read_control: &FactReadControl,
) -> FactStoreResult<BTreeSet<FactRelationKindV1>> {
    load_source(db, owner, Some(read_control), None)
        .await?
        .relations
        .iter()
        .filter_map(|relation| match relation.kind.as_str() {
            SUPPORTS => Some(Ok(FactRelationKindV1::Supports)),
            CONTRADICTS => Some(Ok(FactRelationKindV1::Contradicts)),
            SUPERSEDES => Some(Ok(FactRelationKindV1::Supersedes)),
            DERIVED_FROM => Some(Ok(FactRelationKindV1::DerivedFrom)),
            MENTIONS | ACTIVE_ASSERTION | EVIDENCE_ANCHOR => None,
            _ => Some(Err(storage_message(
                OPERATION,
                "canonical source contains an unknown relation kind",
            ))),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use serde_json::json;
    use tempfile::{TempDir, tempdir};
    use tracedecay_domain::{
        Confidence, FactCategoryV1, FactId, FactOwnerV1, ProvenanceId, UtcMicros,
    };
    use tracedecay_store::{
        FactCommitOutcome, FactReadControl, FactStore, FactStoreError, FactWriteControl,
    };

    use super::*;
    use crate::db::engine::params;
    use crate::db::{Database, DatabaseAuthority, TestDatabaseRuntimeMode};
    use crate::store::memory::DatabaseFactStore;
    use crate::store::memory::crud::{initial_batch, sanitize_payload};

    async fn database(label: &str) -> (TempDir, Database) {
        let directory = tempdir().expect("create graph telemetry fixture directory");
        let path = directory.path().join(format!("{label}.db"));
        let authority = DatabaseAuthority::acquire_test(&path, "graph telemetry test authority")
            .expect("acquire graph telemetry fixture authority");
        let (database, _) = Database::publish_profile_memory_test_runtime(
            &path,
            &authority,
            TestDatabaseRuntimeMode::Initialize,
        )
        .await
        .expect("publish graph telemetry fixture runtime");
        (directory, database)
    }

    fn write_control() -> FactWriteControl {
        FactWriteControl::new(Arc::new(|| false), Arc::new(|| true))
    }

    async fn seed_source_fact(database: &Database, label: &str) -> FactId {
        let sanitized = sanitize_payload(
            &format!("canonical {label} source fact"),
            FactCategoryV1::General,
            &[],
            &[],
            &json!({"fixture": label}),
            None,
        )
        .expect("sanitize graph telemetry fixture payload")
        .expect("graph telemetry fixture remains durable");
        let batch = initial_batch(
            &FactOwnerV1::Profile,
            &ProvenanceId::new(format!("graph.telemetry.{label}.seed"))
                .expect("graph telemetry fixture operation id"),
            sanitized.payload,
            sanitized.access,
            Confidence::new(0.8).expect("graph telemetry fixture confidence"),
            None,
            UtcMicros(1_000_000),
        )
        .expect("create graph telemetry fixture batch");
        let fact_id = batch.fact_id().clone();
        let outcome = DatabaseFactStore::new(database)
            .commit_fact(batch, &write_control())
            .await
            .expect("commit graph telemetry fixture fact");
        assert!(matches!(outcome, FactCommitOutcome::Committed(_)));
        fact_id
    }

    #[tokio::test]
    async fn cancelled_source_load_records_materialized_source_work() {
        let (_directory, database) = database("cancelled-source-telemetry").await;
        seed_source_fact(&database, "cancelled-source-telemetry").await;
        let observer = database.project_memory_reconciliation_telemetry_observer();
        let before = observer.snapshot();
        let checks = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&checks);
        let control = FactReadControl::new(Arc::new(move || {
            observed.fetch_add(1, Ordering::AcqRel) >= 3
        }));

        let error = load_source(
            &database,
            &FactOwnerV1::Profile,
            Some(&control),
            Some(&database),
        )
        .await
        .expect_err("source read must stop after materializing its first fact row");
        assert!(matches!(error, FactStoreError::ReadCancelled));
        assert_eq!(checks.load(Ordering::Acquire), 4);

        let cancelled = observer.snapshot();
        assert!(cancelled.source_rows_loaded > before.source_rows_loaded);
        assert!(cancelled.source_bytes_loaded > before.source_bytes_loaded);
    }

    #[tokio::test]
    async fn failed_source_load_records_materialized_source_work() {
        let (_directory, database) = database("failed-source-telemetry").await;
        let fact_id = seed_source_fact(&database, "failed-source-telemetry").await;
        let transaction = database
            .begin_memory_write_transaction(OPERATION)
            .await
            .expect("begin source corruption transaction");
        assert_eq!(
            transaction
                .execute(
                    "UPDATE memory_v2_current_facts
                     SET active_assertion_id = NULL
                     WHERE fact_id = ?1",
                    params![fact_id.as_str()],
                )
                .await
                .expect("clear canonical assertion reference"),
            1
        );
        transaction
            .commit()
            .await
            .expect("commit source corruption transaction");
        let observer = database.project_memory_reconciliation_telemetry_observer();
        let before = observer.snapshot();

        let error = load_source(&database, &FactOwnerV1::Profile, None, Some(&database))
            .await
            .expect_err("missing canonical assertion must fail source loading");
        assert!(matches!(error, FactStoreError::PayloadAccessMismatch));

        let failed = observer.snapshot();
        assert!(failed.source_rows_loaded > before.source_rows_loaded);
        assert!(failed.source_bytes_loaded > before.source_bytes_loaded);
    }
}

fn push_source_entity(entities: &mut Vec<String>, entity: String) -> FactStoreResult<()> {
    if entities.len() >= MAX_VERIFIED_GENERATION_ENTITIES {
        return Err(storage_message(
            OPERATION,
            "canonical memory facts exceed native graph entity capacity",
        ));
    }
    entities.push(entity);
    Ok(())
}

fn push_source_relation(
    relations: &mut BTreeSet<SourceRelation>,
    relation: SourceRelation,
) -> FactStoreResult<()> {
    if !relations.contains(&relation) && relations.len() >= MAX_VERIFIED_GENERATION_RELATIONS {
        return Err(storage_message(
            OPERATION,
            "canonical memory topology exceeds native graph relation capacity",
        ));
    }
    relations.insert(relation);
    Ok(())
}

async fn hydrate_page(
    db: &Database,
    owner: FactOwnerV1,
    roots: &[FactId],
    relations: Vec<ProjectedRelation>,
    read_control: &FactReadControl,
) -> FactStoreResult<ProjectMemoryGraphPageV1> {
    let mut fact_ids = roots.iter().cloned().collect::<BTreeSet<_>>();
    let mut projected = Vec::with_capacity(relations.len());
    for relation in relations {
        ensure_not_cancelled(read_control)?;
        let source = parse_target(&owner, relation.source.as_str())?;
        let target = parse_target(&owner, relation.target.as_str())?;
        if let ProjectMemoryGraphTargetV1::Fact(fact) = &source {
            fact_ids.insert(fact.fact_id().clone());
        } else if let ProjectMemoryGraphTargetV1::Assertion { fact_id, .. } = &source {
            fact_ids.insert(fact_id.clone());
        }
        if let ProjectMemoryGraphTargetV1::Fact(fact) = &target {
            fact_ids.insert(fact.fact_id().clone());
        } else if let ProjectMemoryGraphTargetV1::Assertion { fact_id, .. } = &target {
            fact_ids.insert(fact_id.clone());
        }
        projected.push(ProjectMemoryGraphRelationV1::new(
            &owner,
            source,
            target,
            public_relation_kind(relation.kind.as_str())?,
        )?);
    }
    let transaction = db
        .begin_memory_read_transaction(OPERATION)
        .await
        .map_err(|error| storage_error(OPERATION, error))?;
    let result = load_project_memory_projections_controlled_tx(
        &transaction,
        &owner,
        &fact_ids.into_iter().collect::<Vec<_>>(),
        read_control,
    )
    .await
    .and_then(|facts| {
        if facts
            .iter()
            .any(|fact| matches!(fact, ProjectMemoryFactProjectionV1::Unavailable(_)))
        {
            return Err(FactStoreError::PayloadAccessMismatch);
        }
        ProjectMemoryGraphPageV1::new(owner, facts, projected)
    });
    finish_read_snapshot(transaction, result).await
}

#[cfg(test)]
pub(in crate::store::memory) async fn hydrate_roots_from_canonical_source_for_test(
    db: &Database,
    owner: FactOwnerV1,
    roots: &[FactId],
    read_control: &FactReadControl,
) -> FactStoreResult<ProjectMemoryGraphPageV1> {
    hydrate_page(db, owner, roots, Vec::new(), read_control).await
}

fn namespace(owner: &FactOwnerV1) -> FactStoreResult<GraphNamespace> {
    let encoded = serde_json::to_vec(owner).map_err(|error| storage_error(OPERATION, error))?;
    GraphNamespace::new(format!(
        "project-memory:{}",
        hex::encode(Sha256::digest(encoded))
    ))
    .map_err(|error| graph_error(owner, error))
}

fn relation_kinds() -> Result<BTreeSet<GraphRelationKind>, tracedecay_graph_db::GraphDbError> {
    [
        CONTRADICTS,
        SUPERSEDES,
        SUPPORTS,
        DERIVED_FROM,
        MENTIONS,
        ACTIVE_ASSERTION,
        EVIDENCE_ANCHOR,
    ]
    .into_iter()
    .map(GraphRelationKind::new)
    .collect()
}

fn public_relation_kind(value: &str) -> FactStoreResult<ProjectMemoryGraphRelationKindV1> {
    match value {
        CONTRADICTS => Ok(ProjectMemoryGraphRelationKindV1::Contradicts),
        SUPERSEDES => Ok(ProjectMemoryGraphRelationKindV1::Supersedes),
        SUPPORTS => Ok(ProjectMemoryGraphRelationKindV1::Supports),
        DERIVED_FROM => Ok(ProjectMemoryGraphRelationKindV1::DerivedFrom),
        MENTIONS => Ok(ProjectMemoryGraphRelationKindV1::Mentions),
        ACTIVE_ASSERTION => Ok(ProjectMemoryGraphRelationKindV1::ActiveAssertion),
        EVIDENCE_ANCHOR => Ok(ProjectMemoryGraphRelationKindV1::EvidenceAnchor),
        _ => Err(storage_message(
            OPERATION,
            "unknown projected memory relation kind",
        )),
    }
}

fn fact_entity_id(fact_id: &FactId) -> Result<GraphEntityId, tracedecay_graph_db::GraphDbError> {
    GraphEntityId::new(format!(
        "memory-fact:{}",
        hex::encode(fact_id.as_str().as_bytes())
    ))
}

fn fact_entity_id_from_str(value: &str) -> FactStoreResult<String> {
    FactId::new(value.to_owned())?;
    Ok(format!("memory-fact:{}", hex::encode(value.as_bytes())))
}

fn assertion_entity_id_from_str(fact: &str, assertion: &str) -> FactStoreResult<String> {
    FactId::new(fact.to_owned())?;
    FactAssertionId::new(assertion.to_owned())?;
    Ok(format!(
        "memory-assertion:{}:{}",
        hex::encode(fact.as_bytes()),
        hex::encode(assertion.as_bytes())
    ))
}

fn entity_entity_id(entity: &ProjectMemoryEntityIdV1) -> String {
    format!("memory-entity:{}", hex::encode(entity.entity().as_bytes()))
}

fn anchor_entity_id_from_str(value: &str) -> FactStoreResult<String> {
    RetrievalAnchorId::new(value.to_owned())?;
    Ok(format!("memory-anchor:{}", hex::encode(value.as_bytes())))
}

fn parse_target(
    owner: &FactOwnerV1,
    identity: &str,
) -> FactStoreResult<ProjectMemoryGraphTargetV1> {
    if let Some(encoded) = identity.strip_prefix("memory-fact:") {
        let fact_id = FactId::new(decode_identity(encoded)?)?;
        return Ok(ProjectMemoryGraphTargetV1::Fact(
            ProjectMemoryFactIdV1::new(owner.clone(), fact_id)?,
        ));
    }
    if let Some(encoded) = identity.strip_prefix("memory-entity:") {
        return Ok(ProjectMemoryGraphTargetV1::Entity(
            ProjectMemoryEntityIdV1::new(owner.clone(), decode_identity(encoded)?)?,
        ));
    }
    if let Some(encoded) = identity.strip_prefix("memory-assertion:") {
        let (fact, assertion) = encoded
            .split_once(':')
            .ok_or_else(|| storage_message(OPERATION, "malformed assertion graph identity"))?;
        return Ok(ProjectMemoryGraphTargetV1::Assertion {
            owner: owner.clone(),
            fact_id: FactId::new(decode_identity(fact)?)?,
            assertion_id: FactAssertionId::new(decode_identity(assertion)?)?,
        });
    }
    if let Some(encoded) = identity.strip_prefix("memory-anchor:") {
        return Ok(ProjectMemoryGraphTargetV1::RetrievalAnchor {
            owner: owner.clone(),
            anchor_id: RetrievalAnchorId::new(decode_identity(encoded)?)?,
        });
    }
    Err(storage_message(
        OPERATION,
        "malformed memory graph entity identity",
    ))
}

fn decode_identity(value: &str) -> FactStoreResult<String> {
    let bytes = hex::decode(value).map_err(|error| storage_error(OPERATION, error))?;
    String::from_utf8(bytes).map_err(|error| storage_error(OPERATION, error))
}

pub(super) fn graph_error(
    owner: &FactOwnerV1,
    error: tracedecay_graph_db::GraphDbError,
) -> FactStoreError {
    match error {
        tracedecay_graph_db::GraphDbError::Conflict => FactStoreError::GraphConflict,
        tracedecay_graph_db::GraphDbError::Cancelled => FactStoreError::GraphCancelled,
        tracedecay_graph_db::GraphDbError::BudgetExhausted { .. } => {
            FactStoreError::GraphBudgetExhausted
        }
        tracedecay_graph_db::GraphDbError::DeadlineExceeded => {
            FactStoreError::GraphDeadlineExceeded
        }
        tracedecay_graph_db::GraphDbError::Unavailable { .. }
        | tracedecay_graph_db::GraphDbError::Closed => FactStoreError::GraphUnavailable,
        tracedecay_graph_db::GraphDbError::ResetRequired { message } => {
            FactStoreError::GraphResetRequired {
                owner: owner.clone(),
                reason: message,
            }
        }
        other => storage_error(OPERATION, other),
    }
}
