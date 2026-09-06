use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;

use grafeo_common::types::Value;
use grafeo_engine::GrafeoDB;

use crate::schema::{
    ENTITY_ID_PROPERTY, ENTITY_LABEL, RELATION_ID_PROPERTY, RELATION_LABEL,
    entity_projection_label, relation_projection_label,
};
use crate::state::{labeled_projection_nodes, latest_projection, load_entity, load_relation};
use crate::{
    GraphBudgetKind, GraphCancellation, GraphDb, GraphDbError, GraphEntity, GraphEntityId,
    GraphNamespace, GraphProjectionId, GraphRelation, GraphRelationId, GraphSnapshot,
    GraphWatermark, SourceGeneration,
};

const MAX_PROJECTION_PAGE_ITEMS: usize = 100_000;

/// The label pair and identity property that name one ordered identity domain:
/// the projection-scoped owner label, the record label that separates entities
/// from relations within it, and the property each record files its identity
/// under.
#[derive(Clone, Copy)]
pub(crate) struct IdentityScope<'a> {
    pub(crate) owner_label: &'a str,
    pub(crate) record_label: &'a str,
    pub(crate) identity_property: &'a str,
}

#[derive(Clone)]
pub struct GraphProjectionReadRequest {
    pub namespace: GraphNamespace,
    pub projection: GraphProjectionId,
    pub after_entity: Option<GraphEntityId>,
    pub after_relation: Option<GraphRelationId>,
    pub max_entities: usize,
    pub max_relations: usize,
    pub cancellation: Arc<dyn GraphCancellation>,
}

impl fmt::Debug for GraphProjectionReadRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GraphProjectionReadRequest")
            .field("namespace", &self.namespace)
            .field("projection", &self.projection)
            .field("after_entity", &self.after_entity)
            .field("after_relation", &self.after_relation)
            .field("max_entities", &self.max_entities)
            .field("max_relations", &self.max_relations)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct GraphProjectionPage {
    pub entities: Vec<GraphEntity>,
    pub relations: Vec<GraphRelation>,
    pub next_entity: Option<GraphEntityId>,
    pub next_relation: Option<GraphRelationId>,
}

#[derive(Clone)]
pub struct GraphProjectionTelemetryRequest {
    pub namespace: GraphNamespace,
    pub projection: GraphProjectionId,
    pub cancellation: Arc<dyn GraphCancellation>,
}

impl fmt::Debug for GraphProjectionTelemetryRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GraphProjectionTelemetryRequest")
            .field("namespace", &self.namespace)
            .field("projection", &self.projection)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GraphProjectionTelemetry {
    pub source_generation: SourceGeneration,
    pub watermark: GraphWatermark,
    pub commit_sequence: u64,
    pub entity_count: u64,
    pub relation_count: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GraphProjectionLabelPage {
    pub entities: Vec<GraphEntity>,
    pub total_entities: u64,
}

impl GraphDb {
    #[hotpath::measure(label = "graph_db.projection.read", impl_type = "GraphDb")]
    pub fn read_projection(
        &self,
        request: GraphProjectionReadRequest,
    ) -> Result<GraphProjectionPage, GraphDbError> {
        let guard = self.read_database(request.cancellation.as_ref())?;
        let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
        self.ensure_projection_readable(&request.namespace, &request.projection)?;
        let page = read_projection(self, database, request)?;
        crate::hotpath_observe::record_counts(page.entities.len(), page.relations.len(), 0, 0);
        crate::hotpath_observe::record_hydration_source(
            crate::hotpath_observe::HydrationSource::Live,
        );
        Ok(page)
    }

    #[hotpath::measure(label = "graph_db.projection.telemetry", impl_type = "GraphDb")]
    pub fn projection_telemetry(
        &self,
        request: GraphProjectionTelemetryRequest,
    ) -> Result<Option<GraphProjectionTelemetry>, GraphDbError> {
        let guard = self.read_database(request.cancellation.as_ref())?;
        let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
        self.ensure_projection_readable(&request.namespace, &request.projection)?;
        let telemetry = projection_telemetry(self, database, request)?;
        if let Some(telemetry) = &telemetry {
            crate::hotpath_observe::record_counts(
                usize::try_from(telemetry.entity_count).unwrap_or(usize::MAX),
                usize::try_from(telemetry.relation_count).unwrap_or(usize::MAX),
                0,
                0,
            );
        }
        crate::hotpath_observe::record_hydration_source(
            crate::hotpath_observe::HydrationSource::Live,
        );
        Ok(telemetry)
    }
}

impl GraphSnapshot {
    pub fn read_projection(
        &self,
        request: GraphProjectionReadRequest,
    ) -> Result<GraphProjectionPage, GraphDbError> {
        let page = self.database.read_projection(request)?;
        crate::hotpath_observe::record_hydration_source(
            crate::hotpath_observe::HydrationSource::Snapshot,
        );
        Ok(page)
    }

    pub fn projection_telemetry(
        &self,
        request: GraphProjectionTelemetryRequest,
    ) -> Result<Option<GraphProjectionTelemetry>, GraphDbError> {
        let telemetry = self.database.projection_telemetry(request)?;
        crate::hotpath_observe::record_hydration_source(
            crate::hotpath_observe::HydrationSource::Snapshot,
        );
        Ok(telemetry)
    }
}

fn projection_telemetry(
    handle: &GraphDb,
    database: &GrafeoDB,
    request: GraphProjectionTelemetryRequest,
) -> Result<Option<GraphProjectionTelemetry>, GraphDbError> {
    check_cancelled(request.cancellation.as_ref())?;
    let Some(projection) = latest_projection(database, &request.namespace, &request.projection)?
    else {
        return Ok(None);
    };
    let entity_count = count_labeled_nodes(
        handle,
        database,
        IdentityScope {
            owner_label: &entity_projection_label(&request.namespace, &request.projection),
            record_label: ENTITY_LABEL,
            identity_property: ENTITY_ID_PROPERTY,
        },
        request.cancellation.as_ref(),
    )?;
    let relation_count = count_labeled_nodes(
        handle,
        database,
        IdentityScope {
            owner_label: &relation_projection_label(&request.namespace, &request.projection),
            record_label: RELATION_LABEL,
            identity_property: RELATION_ID_PROPERTY,
        },
        request.cancellation.as_ref(),
    )?;
    check_cancelled(request.cancellation.as_ref())?;
    Ok(Some(GraphProjectionTelemetry {
        source_generation: projection.commit.source_generation,
        watermark: projection.commit.watermark,
        commit_sequence: projection.commit.sequence,
        entity_count,
        relation_count,
    }))
}

fn read_projection(
    handle: &GraphDb,
    database: &GrafeoDB,
    request: GraphProjectionReadRequest,
) -> Result<GraphProjectionPage, GraphDbError> {
    check_cancelled(request.cancellation.as_ref())?;
    if request.max_entities == 0 && request.max_relations == 0 {
        return Err(GraphDbError::budget_exhausted_count(
            GraphBudgetKind::Read,
            MAX_PROJECTION_PAGE_ITEMS,
        ));
    }
    validate_optional_page_limit(request.max_entities)?;
    validate_optional_page_limit(request.max_relations)?;

    let (entities, next_entity) = read_entity_page(handle, database, &request)?;
    check_cancelled(request.cancellation.as_ref())?;
    let (relations, next_relation) = read_relation_page(handle, database, &request)?;
    check_cancelled(request.cancellation.as_ref())?;
    Ok(GraphProjectionPage {
        entities,
        relations,
        next_entity,
        next_relation,
    })
}

fn read_entity_page(
    handle: &GraphDb,
    database: &GrafeoDB,
    request: &GraphProjectionReadRequest,
) -> Result<(Vec<GraphEntity>, Option<GraphEntityId>), GraphDbError> {
    if request.max_entities == 0 {
        return Ok((Vec::new(), None));
    }
    authenticate_entity_cursor(database, request)?;
    let owner_label = entity_projection_label(&request.namespace, &request.projection);
    let identities = query_identity_page(
        handle,
        database,
        IdentityScope {
            owner_label: &owner_label,
            record_label: ENTITY_LABEL,
            identity_property: ENTITY_ID_PROPERTY,
        },
        request.after_entity.as_ref().map(GraphEntityId::as_str),
        request.max_entities.saturating_add(1),
        request.cancellation.as_ref(),
    )?;
    let has_more = identities.len() > request.max_entities;
    let mut entities = Vec::with_capacity(identities.len().min(request.max_entities));
    for identity in identities.into_iter().take(request.max_entities) {
        check_cancelled(request.cancellation.as_ref())?;
        let identity = GraphEntityId::new(identity)
            .map_err(|error| persisted_identity_error("entity", error))?;
        let stored = load_entity(database, &request.namespace, &identity)?.ok_or_else(|| {
            GraphDbError::Corrupt {
                message: "projection query returned an unindexed entity".to_owned(),
            }
        })?;
        if stored.projection != request.projection {
            return Err(GraphDbError::Corrupt {
                message: "projection entity index does not match ownership".to_owned(),
            });
        }
        entities.push(stored.entity);
    }
    let next = has_more
        .then(|| entities.last().map(|entity| entity.identity.clone()))
        .flatten();
    Ok((entities, next))
}

fn read_relation_page(
    handle: &GraphDb,
    database: &GrafeoDB,
    request: &GraphProjectionReadRequest,
) -> Result<(Vec<GraphRelation>, Option<GraphRelationId>), GraphDbError> {
    if request.max_relations == 0 {
        return Ok((Vec::new(), None));
    }
    authenticate_relation_cursor(database, request)?;
    let owner_label = relation_projection_label(&request.namespace, &request.projection);
    let identities = query_identity_page(
        handle,
        database,
        IdentityScope {
            owner_label: &owner_label,
            record_label: RELATION_LABEL,
            identity_property: RELATION_ID_PROPERTY,
        },
        request.after_relation.as_ref().map(GraphRelationId::as_str),
        request.max_relations.saturating_add(1),
        request.cancellation.as_ref(),
    )?;
    let has_more = identities.len() > request.max_relations;
    let mut relations = Vec::with_capacity(identities.len().min(request.max_relations));
    for identity in identities.into_iter().take(request.max_relations) {
        check_cancelled(request.cancellation.as_ref())?;
        let identity = GraphRelationId::new(identity)
            .map_err(|error| persisted_identity_error("relation", error))?;
        let stored = load_relation(database, &request.namespace, &identity)?.ok_or_else(|| {
            GraphDbError::Corrupt {
                message: "projection query returned an unindexed relation".to_owned(),
            }
        })?;
        if stored.projection != request.projection {
            return Err(GraphDbError::Corrupt {
                message: "projection relation index does not match ownership".to_owned(),
            });
        }
        relations.push(stored.relation);
    }
    let next = has_more
        .then(|| relations.last().map(|relation| relation.identity.clone()))
        .flatten();
    Ok((relations, next))
}

fn authenticate_entity_cursor(
    database: &GrafeoDB,
    request: &GraphProjectionReadRequest,
) -> Result<(), GraphDbError> {
    let Some(cursor) = &request.after_entity else {
        return Ok(());
    };
    let stored = load_entity(database, &request.namespace, cursor)?;
    if stored.is_some_and(|stored| stored.projection == request.projection) {
        Ok(())
    } else {
        Err(GraphDbError::invalid(
            "entity cursor does not belong to the requested projection",
        ))
    }
}

fn authenticate_relation_cursor(
    database: &GrafeoDB,
    request: &GraphProjectionReadRequest,
) -> Result<(), GraphDbError> {
    let Some(cursor) = &request.after_relation else {
        return Ok(());
    };
    let stored = load_relation(database, &request.namespace, cursor)?;
    if stored.is_some_and(|stored| stored.projection == request.projection) {
        Ok(())
    } else {
        Err(GraphDbError::invalid(
            "relation cursor does not belong to the requested projection",
        ))
    }
}

/// The `limit` smallest identities after `after`, in ascending order.
///
/// This reads the store directly rather than issuing
/// `MATCH (n:owner:record) ... ORDER BY ... LIMIT`. GQL resolves each label in
/// that pattern by exact name, and a compacted generation files a multi-label
/// node under one fused composite key, so the pattern matches nothing there and
/// the page would come back silently empty. [`labeled_projection_nodes`] reads
/// through the composite instead.
///
/// Pages are answered from a cached ordered identity index rather than by
/// rescanning the projection, so paging N identities costs one O(N log N) build
/// plus O(log N + limit) per page instead of the O(N) scan *per page* — an
/// O(N^2) catalog warm — this used to run. See
/// [`crate::projection_identity_index`]. A projection too large to index falls
/// back to the bounded streaming scan below.
#[hotpath::measure(label = "graph_db.projection.identity_index.seek")]
fn query_identity_page(
    handle: &GraphDb,
    database: &GrafeoDB,
    scope: IdentityScope<'_>,
    after: Option<&str>,
    limit: usize,
    cancellation: &dyn GraphCancellation,
) -> Result<Vec<String>, GraphDbError> {
    check_cancelled(cancellation)?;
    if limit == 0 {
        return Ok(Vec::new());
    }
    let page =
        match handle
            .inner
            .identity_indexes
            .ordered_identities(database, scope, cancellation)?
        {
            Some(index) => index.page(after, limit),
            None => streaming_identity_page(database, scope, after, limit, cancellation)?,
        };
    check_cancelled(cancellation)?;
    Ok(page)
}

/// The pre-index page scan, kept for projections whose identities exceed the
/// index's retention budget. Bounded in memory by `limit` rather than by the
/// projection's size.
fn streaming_identity_page(
    database: &GrafeoDB,
    scope: IdentityScope<'_>,
    after: Option<&str>,
    limit: usize,
    cancellation: &dyn GraphCancellation,
) -> Result<Vec<String>, GraphDbError> {
    let IdentityScope {
        owner_label,
        record_label,
        identity_property,
    } = scope;
    let nodes = labeled_projection_nodes(database, owner_label, record_label)?;
    let store = database.graph_store();
    let mut page: BTreeSet<String> = BTreeSet::new();
    for node in nodes {
        check_cancelled(cancellation)?;
        let Some(record) = store.get_node(node) else {
            continue;
        };
        let identity = record
            .get_property(identity_property)
            .and_then(Value::as_str)
            .ok_or_else(|| GraphDbError::Corrupt {
                message: format!("projection query returned a non-string `{identity_property}`"),
            })?;
        if after.is_some_and(|after| identity <= after) {
            continue;
        }
        if page.len() == limit {
            if page
                .last()
                .is_some_and(|widest| identity >= widest.as_str())
            {
                continue;
            }
            page.pop_last();
        }
        page.insert(identity.to_owned());
    }
    Ok(page.into_iter().collect())
}

/// How many nodes carry both `owner_label` and `record_label`.
///
/// Answered from the same ordered index the pages are served from, so a count
/// alongside a page scans the projection once rather than twice.
fn count_labeled_nodes(
    handle: &GraphDb,
    database: &GrafeoDB,
    scope: IdentityScope<'_>,
    cancellation: &dyn GraphCancellation,
) -> Result<u64, GraphDbError> {
    check_cancelled(cancellation)?;
    let count =
        match handle
            .inner
            .identity_indexes
            .ordered_identities(database, scope, cancellation)?
        {
            Some(index) => index.node_count(),
            None => {
                labeled_projection_nodes(database, scope.owner_label, scope.record_label)?.len()
            }
        };
    let count = u64::try_from(count).map_err(|_| GraphDbError::Corrupt {
        message: "projection count query returned a negative cardinality".to_owned(),
    })?;
    check_cancelled(cancellation)?;
    Ok(count)
}

fn validate_optional_page_limit(limit: usize) -> Result<(), GraphDbError> {
    if limit > MAX_PROJECTION_PAGE_ITEMS {
        Err(GraphDbError::budget_exhausted_count(
            GraphBudgetKind::Read,
            MAX_PROJECTION_PAGE_ITEMS,
        ))
    } else {
        Ok(())
    }
}

fn check_cancelled(cancellation: &dyn GraphCancellation) -> Result<(), GraphDbError> {
    if cancellation.is_cancelled() {
        Err(GraphDbError::Cancelled)
    } else {
        Ok(())
    }
}

fn persisted_identity_error(description: &str, error: GraphDbError) -> GraphDbError {
    GraphDbError::Corrupt {
        message: format!("invalid persisted {description} identity: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use super::{IdentityScope, query_identity_page, streaming_identity_page};
    use crate::schema::{ENTITY_ID_PROPERTY, ENTITY_LABEL, entity_projection_label};
    use crate::{
        GraphDbLeaseV1, GraphDbLocation, GraphDbOpenOptions, GraphDbOwner, GraphDurability,
        GraphEntity, GraphEntityId, GraphFormatVersion, GraphMutation, GraphNamespace,
        GraphProjectionId, GraphWatermark, GraphWriteBatch, NeverCancelled, SourceGeneration,
    };

    const PAGE: usize = 1_024;

    fn scope(owner_label: &str) -> IdentityScope<'_> {
        IdentityScope {
            owner_label,
            record_label: ENTITY_LABEL,
            identity_property: ENTITY_ID_PROPERTY,
        }
    }

    fn memory_db() -> GraphDbLeaseV1 {
        GraphDbOwner::open(GraphDbOpenOptions {
            location: GraphDbLocation::Memory,
            expected_format: GraphFormatVersion::new(2).unwrap(),
            durability: GraphDurability::Memory,
            cancellation: Arc::new(NeverCancelled),
        })
        .unwrap()
        .issue_lease()
        .unwrap()
    }

    /// Publishes `count` entities into `projection`, in batches, so the probe
    /// can reach production width without one enormous write batch.
    fn publish_entities(db: &GraphDbLeaseV1, projection: &str, count: usize, batch_size: usize) {
        for (batch, start) in (0..count).step_by(batch_size).enumerate() {
            let mutations = (start..(start + batch_size).min(count))
                .map(|index| {
                    GraphMutation::UpsertEntity(
                        GraphEntity::new(
                            // Zero-padded so lexicographic order is stable and
                            // the identities look like real chunk ids.
                            GraphEntityId::new(format!("chunk-{index:09}")).unwrap(),
                            BTreeSet::new(),
                            BTreeMap::new(),
                        )
                        .unwrap(),
                    )
                })
                .collect::<Vec<_>>();
            db.apply_unverified(
                GraphWriteBatch::new(
                    GraphNamespace::new("project").unwrap(),
                    GraphProjectionId::new(projection).unwrap(),
                    SourceGeneration::new(format!("{projection}-generation-{batch}")).unwrap(),
                    GraphWatermark::new(format!("{projection}-watermark-{batch}")).unwrap(),
                    mutations,
                    Arc::new(NeverCancelled),
                )
                .unwrap(),
            )
            .unwrap();
        }
    }

    /// Pages the whole projection through one identity path and returns the
    /// per-page wall times in order.
    fn page_through(db: &GraphDbLeaseV1, projection: &str, indexed: bool) -> Vec<Duration> {
        let guard = db.read_guard().unwrap();
        let database = guard.as_ref().unwrap();
        let owner_label = entity_projection_label(
            &GraphNamespace::new("project").unwrap(),
            &GraphProjectionId::new(projection).unwrap(),
        );
        let cancellation = NeverCancelled;
        let mut after: Option<String> = None;
        let mut timings = Vec::new();
        loop {
            let started = Instant::now();
            let page = if indexed {
                query_identity_page(
                    db,
                    database,
                    scope(&owner_label),
                    after.as_deref(),
                    PAGE,
                    &cancellation,
                )
                .unwrap()
            } else {
                streaming_identity_page(
                    database,
                    scope(&owner_label),
                    after.as_deref(),
                    PAGE,
                    &cancellation,
                )
                .unwrap()
            };
            timings.push(started.elapsed());
            let Some(last) = page.last().cloned() else {
                break;
            };
            after = Some(last);
            if page.len() < PAGE {
                break;
            }
        }
        timings
    }

    fn total(timings: &[Duration]) -> Duration {
        timings.iter().sum()
    }

    /// Both identity paths must return byte-identical pages: same ordering,
    /// same exclusive cursor, same limit. The indexed path is only a faster
    /// way to answer the question the scan answered.
    #[test]
    fn indexed_and_streaming_identity_pages_agree() {
        let db = memory_db();
        publish_entities(&db, "agree", 300, 300);
        let guard = db.read_guard().unwrap();
        let database = guard.as_ref().unwrap();
        let owner_label = entity_projection_label(
            &GraphNamespace::new("project").unwrap(),
            &GraphProjectionId::new("agree").unwrap(),
        );
        let cancellation = NeverCancelled;

        for after in [
            None,
            Some("chunk-000000000"),
            Some("chunk-000000123"),
            Some("zzz"),
        ] {
            for limit in [1usize, 7, 64, 1_000] {
                let indexed = query_identity_page(
                    &db,
                    database,
                    scope(&owner_label),
                    after,
                    limit,
                    &cancellation,
                )
                .unwrap();
                let streamed = streaming_identity_page(
                    database,
                    scope(&owner_label),
                    after,
                    limit,
                    &cancellation,
                )
                .unwrap();
                assert_eq!(indexed, streamed, "after={after:?} limit={limit}");
            }
        }
    }

    /// A write after the index was built must be visible to the next page:
    /// the database write claim invalidates the cached index.
    #[test]
    fn writes_after_an_index_build_are_visible_to_the_next_page() {
        let db = memory_db();
        publish_entities(&db, "invalidate", 8, 8);

        let before = {
            let guard = db.read_guard().unwrap();
            let database = guard.as_ref().unwrap();
            query_identity_page(
                &db,
                database,
                scope(&entity_projection_label(
                    &GraphNamespace::new("project").unwrap(),
                    &GraphProjectionId::new("invalidate").unwrap(),
                )),
                None,
                100,
                &NeverCancelled,
            )
            .unwrap()
        };
        assert_eq!(before.len(), 8);

        publish_entities(&db, "invalidate", 16, 16);

        let guard = db.read_guard().unwrap();
        let database = guard.as_ref().unwrap();
        let after = query_identity_page(
            &db,
            database,
            scope(&entity_projection_label(
                &GraphNamespace::new("project").unwrap(),
                &GraphProjectionId::new("invalidate").unwrap(),
            )),
            None,
            100,
            &NeverCancelled,
        )
        .unwrap();

        assert_eq!(after.len(), 16, "stale index served a pre-write page");
    }

    /// Page-read cost probe at production width.
    ///
    /// The streaming path rescans the whole projection per page, so its total
    /// grows with pages^2; the indexed path pays one build and then seeks, so
    /// its total is linear in pages. Prints both so the catalog-warm
    /// projection can be read off a measurement instead of an extrapolation.
    #[test]
    #[ignore = "diagnostic probe, run explicitly"]
    fn projection_page_read_cost_scaling_probe() {
        const ENTITIES: usize = 100_000;

        let db = memory_db();
        let build_started = Instant::now();
        publish_entities(&db, "probe", ENTITIES, 10_000);
        let publish_elapsed = build_started.elapsed();

        let streaming = page_through(&db, "probe", false);
        let indexed = page_through(&db, "probe", true);
        // Second pass: the index is already warm, so this isolates seek cost.
        let indexed_warm = page_through(&db, "probe", true);

        println!("entities={ENTITIES} page={PAGE} publish={publish_elapsed:?}");
        println!(
            "streaming: pages={} total={:?} first={:?} last={:?}",
            streaming.len(),
            total(&streaming),
            streaming.first().unwrap(),
            streaming.last().unwrap(),
        );
        println!(
            "indexed:   pages={} total={:?} first(build)={:?} last={:?}",
            indexed.len(),
            total(&indexed),
            indexed.first().unwrap(),
            indexed.last().unwrap(),
        );
        println!(
            "indexed warm: pages={} total={:?} first={:?} last={:?}",
            indexed_warm.len(),
            total(&indexed_warm),
            indexed_warm.first().unwrap(),
            indexed_warm.last().unwrap(),
        );
        println!(
            "speedup: {:.1}x total, {:.1}x steady-state page",
            total(&streaming).as_secs_f64() / total(&indexed).as_secs_f64(),
            streaming.last().unwrap().as_secs_f64() / indexed_warm.last().unwrap().as_secs_f64(),
        );

        assert_eq!(streaming.len(), indexed.len());
        assert!(
            total(&indexed) < total(&streaming),
            "indexed paging must beat the rescan it replaced"
        );
    }
}
