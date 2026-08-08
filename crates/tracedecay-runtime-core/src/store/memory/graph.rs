use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde::Serialize;
use sha2::{Digest, Sha256};
use tracedecay_domain::{FactAssertionId, FactId, FactOwnerV1, RetrievalAnchorId};
use tracedecay_graph_db::{
    GraphEntity, GraphEntityId, GraphLabel, GraphNamespace, GraphProjectionId,
    GraphProjectionReadRequest, GraphProjectionTelemetryRequest, GraphRelation, GraphRelationId,
    GraphRelationKind, GraphTraversalDirection, GraphWatermark, MAX_VERIFIED_GENERATION_ENTITIES,
    MAX_VERIFIED_GENERATION_RELATIONS, NeverCancelled, ProjectionReplacement, SourceGeneration,
    TraversalRequest,
};
use tracedecay_store::{
    FactStoreError, FactStoreResult, ProjectMemoryFactIdV1, ProjectMemoryGraphPageV1,
    ProjectMemoryGraphQueryV1, ProjectMemoryGraphRelationKindV1, ProjectMemoryGraphRelationV1,
    ProjectMemoryGraphTargetV1, ProjectMemoryLegacyEntityTargetV1, ProjectMemoryResult,
};

use crate::db::Database;
use crate::db::engine::params;

use super::envelope::finish_read_snapshot;
use super::primitives::{OwnerKey, row_i64, row_string, storage_error, storage_message};
use super::projection::load_project_memory_projections_tx;

const OPERATION: &str = "project memory relation graph";
const PROJECTION: &str = "project-memory-relations";
const SUPPORTS: &str = "memory-supports";
const CONTRADICTS: &str = "memory-contradicts";
const SUPERSEDES: &str = "memory-supersedes";
const DERIVED_FROM: &str = "memory-derived-from";
const MENTIONS: &str = "memory-mentions";
const ACTIVE_ASSERTION: &str = "memory-active-assertion";
const EVIDENCE_ANCHOR: &str = "memory-evidence-anchor";
const MAX_PROJECTION_PUBLICATION_ATTEMPTS: usize = 4;

#[derive(Clone, Debug, Serialize)]
struct SourceRelation {
    source: String,
    target: String,
    kind: String,
}

#[derive(Clone, Debug, Serialize)]
struct MemoryGraphSource {
    owner: String,
    entities: Vec<String>,
    relations: Vec<SourceRelation>,
}

enum ProjectionPublicationOutcome {
    Current,
    Conflict,
}

pub(super) async fn project_memory_graph(
    db: &Database,
    query: ProjectMemoryGraphQueryV1,
) -> ProjectMemoryResult<ProjectMemoryGraphPageV1> {
    let graph = db.memory_relation_graph().ok_or_else(|| {
        storage_message(
            OPERATION,
            "registered memory relation graph authority is unavailable",
        )
    })?;
    let owner = query.owner().clone();
    let namespace = namespace(&owner)?;
    let projection = GraphProjectionId::new(PROJECTION).map_err(graph_error)?;
    let mut published_watermark = None;
    for _ in 0..MAX_PROJECTION_PUBLICATION_ATTEMPTS {
        let source = load_source(db, &owner).await?;
        let (watermark, entities, relations) = build_projection(&source)?;
        let graph_for_projection = Arc::clone(&graph);
        let namespace_for_projection = namespace.clone();
        let projection_for_projection = projection.clone();
        let watermark_for_projection = watermark.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            let cancellation: Arc<dyn tracedecay_graph_db::GraphCancellation> =
                Arc::new(NeverCancelled);
            let current =
                graph_for_projection.projection_telemetry(GraphProjectionTelemetryRequest {
                    namespace: namespace_for_projection.clone(),
                    projection: projection_for_projection.clone(),
                    cancellation: Arc::clone(&cancellation),
                })?;
            if current.as_ref().map(|state| &state.watermark) == Some(&watermark_for_projection) {
                return Ok::<_, tracedecay_graph_db::GraphDbError>(
                    ProjectionPublicationOutcome::Current,
                );
            }
            let expected = current.map(|state| state.watermark);
            match graph_for_projection.replace_projection_unverified_if_current(
                ProjectionReplacement {
                    namespace: namespace_for_projection,
                    projection: projection_for_projection,
                    source_generation: SourceGeneration::new(format!(
                        "project-memory:{}",
                        watermark_for_projection.as_str()
                    ))?,
                    next_watermark: watermark_for_projection,
                    entities,
                    relations,
                    cancellation,
                },
                expected.as_ref(),
            ) {
                Ok(_) => Ok(ProjectionPublicationOutcome::Current),
                Err(tracedecay_graph_db::GraphDbError::Conflict) => {
                    Ok(ProjectionPublicationOutcome::Conflict)
                }
                Err(error) => Err(error),
            }
        })
        .await
        .map_err(|error| storage_error(OPERATION, error))?
        .map_err(graph_error)?;
        if matches!(outcome, ProjectionPublicationOutcome::Conflict) {
            continue;
        }
        let verified_source = load_source(db, &owner).await?;
        if source_watermark(&verified_source)? == watermark {
            published_watermark = Some(watermark);
            break;
        }
    }
    let published_watermark = published_watermark
        .ok_or_else(|| graph_error(tracedecay_graph_db::GraphDbError::Conflict))?;

    let max_relations = query.max_relations();
    let roots = query.roots().to_vec();
    let hydration_roots = roots.clone();
    let graph_for_read = Arc::clone(&graph);
    let namespace_for_read = namespace.clone();
    let projection_for_read = projection.clone();
    let page = tokio::task::spawn_blocking(move || {
        let cancellation: Arc<dyn tracedecay_graph_db::GraphCancellation> =
            Arc::new(NeverCancelled);
        let snapshot = graph_for_read.snapshot()?;
        let max_page = max_relations
            .checked_add(1)
            .ok_or(tracedecay_graph_db::GraphDbError::BudgetExhausted)?;
        let relations = if roots.is_empty() {
            let projection_page = snapshot.read_projection(GraphProjectionReadRequest {
                namespace: namespace_for_read,
                projection: projection_for_read,
                after_entity: None,
                after_relation: None,
                max_entities: 0,
                max_relations: max_page,
                cancellation,
            })?;
            if projection_page.next_relation.is_some() {
                return Err(tracedecay_graph_db::GraphDbError::BudgetExhausted);
            }
            projection_page.relations
        } else {
            let relation_kinds = relation_kinds()?;
            let mut accepted = BTreeSet::new();
            for root in roots {
                let start = fact_entity_id(&root)?;
                let result = snapshot.traverse(TraversalRequest {
                    namespace: namespace_for_read.clone(),
                    start,
                    relation_kinds: relation_kinds.clone(),
                    direction: GraphTraversalDirection::Both,
                    max_depth: max_relations,
                    max_visits: max_page,
                    max_results: max_page,
                    cancellation: Arc::clone(&cancellation),
                })?;
                if result.visits.len() == max_page {
                    return Err(tracedecay_graph_db::GraphDbError::BudgetExhausted);
                }
                accepted.extend(result.visits.into_iter().map(|visit| visit.entity));
            }
            let starts = accepted.iter().cloned().collect::<Vec<_>>();
            let batches = snapshot.outgoing_relations(
                &namespace_for_read,
                &starts,
                &relation_kinds,
                max_page,
                cancellation,
            )?;
            let mut seen = BTreeSet::new();
            batches
                .into_iter()
                .flatten()
                .filter(|relation| {
                    accepted.contains(&relation.from)
                        && accepted.contains(&relation.to)
                        && seen.insert(relation.identity.clone())
                })
                .collect::<Vec<_>>()
        };
        if relations.len() > max_relations {
            return Err(tracedecay_graph_db::GraphDbError::BudgetExhausted);
        }
        Ok::<_, tracedecay_graph_db::GraphDbError>(relations)
    })
    .await
    .map_err(|error| storage_error(OPERATION, error))?
    .map_err(graph_error)?;

    let hydrated = hydrate_page(db, owner.clone(), &hydration_roots, page).await?;
    if source_watermark(&load_source(db, &owner).await?)? != published_watermark {
        return Err(graph_error(tracedecay_graph_db::GraphDbError::Conflict).into());
    }
    Ok(hydrated)
}

async fn load_source(db: &Database, owner: &FactOwnerV1) -> FactStoreResult<MemoryGraphSource> {
    let key = OwnerKey::new(owner)?;
    let transaction = db
        .begin_memory_read_transaction(OPERATION)
        .await
        .map_err(|error| storage_error(OPERATION, error))?;
    let result = async {
        let mut entities = Vec::new();
        let mut rows = transaction
            .query(
                "SELECT fact_id
                 FROM memory_v2_facts
                 WHERE owner_kind = ?1 AND project_id = ?2
                 ORDER BY fact_id",
                params![key.kind, key.project_id.as_str()],
            )
            .await
            .map_err(|error| storage_error(OPERATION, error))?;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| storage_error(OPERATION, error))?
        {
            push_source_entity(
                &mut entities,
                fact_entity_id_from_str(&row_string(&row, 0, OPERATION)?)?,
            )?;
        }
        drop(rows);
        let mut relations = Vec::new();
        let mut rows = transaction
            .query(
                "SELECT source_fact_id, target_fact_id, relation
                 FROM memory_v2_fact_relations
                 WHERE owner_kind = ?1 AND project_id = ?2
                 ORDER BY source_fact_id, target_fact_id, relation",
                params![key.kind, key.project_id.as_str()],
            )
            .await
            .map_err(|error| storage_error(OPERATION, error))?;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| storage_error(OPERATION, error))?
        {
            push_source_relation(
                &mut relations,
                SourceRelation {
                    source: fact_entity_id_from_str(&row_string(&row, 0, OPERATION)?)?,
                    target: fact_entity_id_from_str(&row_string(&row, 1, OPERATION)?)?,
                    kind: explicit_relation_kind(&row_string(&row, 2, OPERATION)?)?.to_owned(),
                },
            )?;
        }
        drop(rows);
        let mut rows = transaction
            .query(
                "SELECT fact_id, active_assertion_id
                 FROM memory_v2_current_facts
                 WHERE owner_kind = ?1 AND project_id = ?2
                   AND active_assertion_id IS NOT NULL
                 ORDER BY fact_id, active_assertion_id",
                params![key.kind, key.project_id.as_str()],
            )
            .await
            .map_err(|error| storage_error(OPERATION, error))?;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| storage_error(OPERATION, error))?
        {
            let fact = row_string(&row, 0, OPERATION)?;
            let assertion = row_string(&row, 1, OPERATION)?;
            push_source_relation(
                &mut relations,
                SourceRelation {
                    source: fact_entity_id_from_str(&fact)?,
                    target: assertion_entity_id_from_str(&fact, &assertion)?,
                    kind: ACTIVE_ASSERTION.to_owned(),
                },
            )?;
        }
        drop(rows);
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
                 WHERE assertion_evidence.owner_kind = ?1
                   AND assertion_evidence.project_id = ?2
                 ORDER BY assertion_evidence.fact_id, assertion_evidence.assertion_id,
                          assertion_evidence.ordinal",
                params![key.kind, key.project_id.as_str()],
            )
            .await
            .map_err(|error| storage_error(OPERATION, error))?;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| storage_error(OPERATION, error))?
        {
            let fact = row_string(&row, 0, OPERATION)?;
            let assertion = row_string(&row, 1, OPERATION)?;
            push_source_relation(
                &mut relations,
                SourceRelation {
                    source: assertion_entity_id_from_str(&fact, &assertion)?,
                    target: anchor_entity_id_from_str(&row_string(&row, 2, OPERATION)?)?,
                    kind: EVIDENCE_ANCHOR.to_owned(),
                },
            )?;
        }
        drop(rows);
        let mut rows = transaction
            .query(
                "SELECT mappings.fact_id, links.entity_id
                 FROM memory_v2_facts AS mappings
                 JOIN memory_facts AS legacy ON legacy.canonical_fact_id = mappings.fact_id
                 JOIN memory_fact_entities AS links ON links.fact_id = legacy.fact_id
                 WHERE mappings.owner_kind = ?1 AND mappings.project_id = ?2
                 ORDER BY mappings.fact_id, links.entity_id",
                params![key.kind, key.project_id.as_str()],
            )
            .await
            .map_err(|error| storage_error(OPERATION, error))?;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| storage_error(OPERATION, error))?
        {
            push_source_relation(
                &mut relations,
                SourceRelation {
                    source: fact_entity_id_from_str(&row_string(&row, 0, OPERATION)?)?,
                    target: entity_entity_id(row_i64(&row, 1, OPERATION)?)?,
                    kind: MENTIONS.to_owned(),
                },
            )?;
        }
        relations.sort_by(|left, right| {
            (&left.source, &left.target, &left.kind).cmp(&(
                &right.source,
                &right.target,
                &right.kind,
            ))
        });
        relations.dedup_by(|left, right| {
            left.source == right.source && left.target == right.target && left.kind == right.kind
        });
        Ok(MemoryGraphSource {
            owner: key.json,
            entities,
            relations,
        })
    }
    .await;
    finish_read_snapshot(transaction, result).await
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
    relations: &mut Vec<SourceRelation>,
    relation: SourceRelation,
) -> FactStoreResult<()> {
    if relations.len() >= MAX_VERIFIED_GENERATION_RELATIONS {
        return Err(storage_message(
            OPERATION,
            "canonical memory topology exceeds native graph relation capacity",
        ));
    }
    relations.push(relation);
    Ok(())
}

fn build_projection(
    source: &MemoryGraphSource,
) -> FactStoreResult<(GraphWatermark, Vec<GraphEntity>, Vec<GraphRelation>)> {
    let watermark = source_watermark(source)?;
    let mut entity_ids = source
        .entities
        .iter()
        .map(|identity| GraphEntityId::new(identity.clone()).map_err(graph_error))
        .collect::<FactStoreResult<BTreeSet<_>>>()?;
    let mut relations = Vec::with_capacity(source.relations.len());
    for relation in &source.relations {
        let from = GraphEntityId::new(relation.source.clone()).map_err(graph_error)?;
        let to = GraphEntityId::new(relation.target.clone()).map_err(graph_error)?;
        insert_projection_entity(&mut entity_ids, from.clone())?;
        insert_projection_entity(&mut entity_ids, to.clone())?;
        let relation_digest = hex::encode(Sha256::digest(
            format!(
                "{}\0{}\0{}",
                relation.source, relation.target, relation.kind
            )
            .as_bytes(),
        ));
        relations.push(
            GraphRelation::new(
                GraphRelationId::new(format!("memory-relation:{relation_digest}"))
                    .map_err(graph_error)?,
                from,
                to,
                GraphRelationKind::new(relation.kind.clone()).map_err(graph_error)?,
                BTreeMap::new(),
            )
            .map_err(graph_error)?,
        );
    }
    let entities = entity_ids
        .into_iter()
        .map(|identity| {
            GraphEntity::new(
                identity.clone(),
                BTreeSet::from([GraphLabel::new(label_for_entity(identity.as_str())?)?]),
                BTreeMap::new(),
            )
        })
        .collect::<Result<Vec<_>, _>>()
        .map_err(graph_error)?;
    Ok((watermark, entities, relations))
}

fn insert_projection_entity(
    entities: &mut BTreeSet<GraphEntityId>,
    entity: GraphEntityId,
) -> FactStoreResult<()> {
    if !entities.contains(&entity) && entities.len() >= MAX_VERIFIED_GENERATION_ENTITIES {
        return Err(storage_message(
            OPERATION,
            "canonical memory topology exceeds native graph entity capacity",
        ));
    }
    entities.insert(entity);
    Ok(())
}

fn source_watermark(source: &MemoryGraphSource) -> FactStoreResult<GraphWatermark> {
    let encoded = serde_json::to_vec(source).map_err(|error| storage_error(OPERATION, error))?;
    GraphWatermark::new(format!(
        "memory-relations:{}",
        hex::encode(Sha256::digest(encoded))
    ))
    .map_err(graph_error)
}

async fn hydrate_page(
    db: &Database,
    owner: FactOwnerV1,
    roots: &[FactId],
    relations: Vec<GraphRelation>,
) -> FactStoreResult<ProjectMemoryGraphPageV1> {
    let mut fact_ids = roots.iter().cloned().collect::<BTreeSet<_>>();
    let mut projected = Vec::with_capacity(relations.len());
    for relation in relations {
        let source = parse_target(&owner, relation.from.as_str())?;
        let target = parse_target(&owner, relation.to.as_str())?;
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
    let result = load_project_memory_projections_tx(
        &transaction,
        &owner,
        &fact_ids.into_iter().collect::<Vec<_>>(),
    )
    .await
    .and_then(|facts| ProjectMemoryGraphPageV1::new(owner, facts, projected));
    finish_read_snapshot(transaction, result).await
}

fn namespace(owner: &FactOwnerV1) -> FactStoreResult<GraphNamespace> {
    let encoded = serde_json::to_vec(owner).map_err(|error| storage_error(OPERATION, error))?;
    GraphNamespace::new(format!(
        "project-memory:{}",
        hex::encode(Sha256::digest(encoded))
    ))
    .map_err(graph_error)
}

fn relation_kinds() -> Result<BTreeSet<GraphRelationKind>, tracedecay_graph_db::GraphDbError> {
    [
        SUPPORTS,
        CONTRADICTS,
        SUPERSEDES,
        DERIVED_FROM,
        MENTIONS,
        ACTIVE_ASSERTION,
        EVIDENCE_ANCHOR,
    ]
    .into_iter()
    .map(GraphRelationKind::new)
    .collect()
}

fn explicit_relation_kind(value: &str) -> FactStoreResult<&'static str> {
    match value {
        "supports" => Ok(SUPPORTS),
        "contradicts" => Ok(CONTRADICTS),
        "supersedes" => Ok(SUPERSEDES),
        "derived_from" => Ok(DERIVED_FROM),
        _ => Err(storage_message(
            OPERATION,
            "unknown canonical memory relation kind",
        )),
    }
}

fn public_relation_kind(value: &str) -> FactStoreResult<ProjectMemoryGraphRelationKindV1> {
    match value {
        SUPPORTS => Ok(ProjectMemoryGraphRelationKindV1::Supports),
        CONTRADICTS => Ok(ProjectMemoryGraphRelationKindV1::Contradicts),
        SUPERSEDES => Ok(ProjectMemoryGraphRelationKindV1::Supersedes),
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

fn label_for_entity(identity: &str) -> Result<&'static str, tracedecay_graph_db::GraphDbError> {
    if identity.starts_with("memory-fact:") {
        Ok("memory-fact-reference")
    } else if identity.starts_with("memory-entity:") {
        Ok("memory-entity-reference")
    } else if identity.starts_with("memory-assertion:") {
        Ok("memory-assertion-reference")
    } else if identity.starts_with("memory-anchor:") {
        Ok("retrieval-anchor-reference")
    } else {
        Err(tracedecay_graph_db::GraphDbError::invalid(
            "unknown memory relation entity identity",
        ))
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

fn entity_entity_id(entity_id: i64) -> FactStoreResult<String> {
    if entity_id <= 0 {
        return Err(FactStoreError::InvalidLegacyFactId {
            legacy_fact_id: entity_id,
        });
    }
    Ok(format!("memory-entity:{entity_id}"))
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
        let entity_id = encoded
            .parse::<i64>()
            .map_err(|error| storage_error(OPERATION, error))?;
        return Ok(ProjectMemoryGraphTargetV1::Entity(
            ProjectMemoryLegacyEntityTargetV1::new(owner.clone(), entity_id)?,
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

fn graph_error(error: tracedecay_graph_db::GraphDbError) -> FactStoreError {
    storage_error(OPERATION, error)
}
