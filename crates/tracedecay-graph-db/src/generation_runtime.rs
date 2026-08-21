use std::collections::{BTreeMap, HashSet, VecDeque};
use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use grafeo_core::graph::Direction;
use parking_lot::{RwLockUpgradableReadGuard, RwLockWriteGuard as ParkingRwLockWriteGuard};
use tracedecay_store::runtime::GraphRecoveredGenerationDigestV1;

use crate::generation::verify_recovered_generation;
use crate::lease::{
    GenerationLocator, VerifiedGenerationLease, VerifiedGraphSnapshot, VerifiedTraversalResult,
    VerifiedTraversalVisit,
};
use crate::projection::graph_properties_live_bytes;
use crate::recovery::{
    checkpoint_recovered_database, is_database_fault, open_recovered_database,
    quarantine_transition_failure, requarantine_after_failed_checkpoint_verification,
    set_projection_quarantine,
};
use crate::runtime::{GraphBatchPlan, PreparedGraphBatch};
use crate::schema::{NAMESPACE_PROPERTY, relation_kind_from_type, required_string};
use crate::state::{
    latest_projection, load_entity_by_node, load_relation, load_relation_by_edge,
    projection_entities_checked, projection_relations_checked,
};
use crate::{
    GraphBudgetKind, GraphCancellation, GraphCommit, GraphDb, GraphDbError, GraphEntityRef,
    GraphGenerationManifest, GraphGenerationRelation, GraphIdempotencyKey, GraphMutation,
    GraphNamespace, GraphRelationRef, GraphTraversalDirection, GraphWriteBatch,
    MAX_VERIFIED_GENERATION_BATCH_LIVE_BYTES, MAX_VERIFIED_GENERATION_BATCH_MUTATIONS,
    TraversalRequest, mutation,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GenerationStagePageKind {
    Entities,
    Relations,
}

impl GenerationStagePageKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Entities => "entities",
            Self::Relations => "relations",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct GenerationStagePage {
    ordinal: usize,
    kind: GenerationStagePageKind,
    range: Range<usize>,
    live_bytes: usize,
}

impl GenerationStagePage {
    fn mutation_count(&self) -> usize {
        self.range.end - self.range.start
    }

    fn live_bytes(&self) -> usize {
        self.live_bytes
    }
}

struct GenerationStageContext {
    locator: GenerationLocator,
    physical_namespace: GraphNamespace,
    dependency_namespaces: BTreeMap<crate::GraphProjectionIdentity, GraphNamespace>,
    dependency_digest: tracedecay_store::runtime::GraphDependencyGenerationClosureDigestV1,
}

impl GraphDb {
    #[cfg(any(feature = "test-helpers", feature = "eval-helpers"))]
    pub(crate) fn verify_generation_in_place(
        &self,
        manifest: &GraphGenerationManifest,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<GraphRecoveredGenerationDigestV1, GraphDbError> {
        check()?;
        let expected = manifest.expected_recovered_digest(check)?;
        let guard = self.read_guard()?;
        let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
        verify_recovered_generation(database, manifest, &expected, check)
    }

    pub(crate) fn verify_existing_generation(
        &self,
        manifest: &GraphGenerationManifest,
        expected: &GraphRecoveredGenerationDigestV1,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<(GraphCommit, GraphRecoveredGenerationDigestV1), GraphDbError> {
        check()?;
        let physical_namespace = manifest.physical_namespace()?;
        let guard = self.read_guard()?;
        let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
        let commit = latest_projection(
            database,
            &physical_namespace,
            &manifest.projection.projection,
        )?
        .ok_or_else(|| {
            GraphDbError::unavailable(
                "metadata-only graph replay has no complete native generation rows",
            )
        })?
        .commit;
        let recovered = verify_recovered_generation(database, manifest, expected, check)?;
        Ok((commit, recovered))
    }

    /// Stages one generation in bounded, durably receipted native pages.
    ///
    /// Entity pages precede relation pages, so every local endpoint exists
    /// before its edge is staged. A final empty batch binds dependency
    /// metadata only after every page receipt is present. None of these rows
    /// become serveable until the caller's recovered-digest proof and
    /// relational verified-head compare-and-swap succeed.
    #[cfg(any(test, feature = "test-helpers", feature = "eval-helpers"))]
    pub(crate) fn apply_generation_unverified(
        &self,
        manifest: &GraphGenerationManifest,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<GraphCommit, GraphDbError> {
        let expected = manifest.expected_recovered_digest(check)?;
        self.apply_generation_unverified_with_digest(manifest, &expected, check)
    }

    pub(crate) fn apply_generation_unverified_with_digest(
        &self,
        manifest: &GraphGenerationManifest,
        expected: &GraphRecoveredGenerationDigestV1,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<GraphCommit, GraphDbError> {
        check()?;
        manifest.validate_checked(check)?;
        let context = GenerationStageContext {
            locator: GenerationLocator::new(
                manifest.projection.clone(),
                manifest.generation.clone(),
            ),
            physical_namespace: manifest.physical_namespace()?,
            dependency_namespaces: self.require_exact_dependencies(manifest)?,
            dependency_digest: manifest.dependency_closure_digest(check)?,
        };
        if let Some(commit) = self.reseat_complete_staged_generation(manifest, &context)? {
            return Ok(commit);
        }
        let pages = generation_stage_pages(manifest)?;
        for (index, page) in pages.iter().enumerate() {
            check()?;
            self.apply_generation_stage_page_with_context(
                manifest,
                expected,
                &context,
                index.checked_sub(1).and_then(|prior| pages.get(prior)),
                page,
                check,
            )?;
            // This is the exact cancellation boundary: the page transaction
            // and receipt are durable, while no verified lease/head exists.
            check()?;
        }
        self.finalize_staged_generation(manifest, expected, &context, pages.last(), check)
    }

    fn reseat_complete_staged_generation(
        &self,
        manifest: &GraphGenerationManifest,
        context: &GenerationStageContext,
    ) -> Result<Option<GraphCommit>, GraphDbError> {
        let _snapshot_gate = self.inner.snapshot_gate.upgradable_read();
        let existing = {
            let guard = self.read_guard()?;
            let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
            latest_projection(
                database,
                &context.physical_namespace,
                &manifest.projection.projection,
            )?
        };
        let Some(existing) = existing else {
            return Ok(None);
        };
        if existing.commit.source_generation != manifest.source_generation
            || existing.commit.watermark != manifest.watermark
            || existing.commit.generation_dependency_digest.as_ref()
                != Some(&context.dependency_digest)
        {
            return Ok(None);
        }
        let mut verified = self.inner.verified_generations.write().map_err(|_| {
            GraphDbError::unavailable("verified graph generation state lock is poisoned")
        })?;
        verified.collected.remove(&context.locator);
        verified.stored.insert(
            context.locator.clone(),
            generation_dependency_locators(manifest),
        );
        Ok(Some(existing.commit))
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_generation_stage_page_with_context(
        &self,
        manifest: &GraphGenerationManifest,
        expected: &GraphRecoveredGenerationDigestV1,
        context: &GenerationStageContext,
        predecessor: Option<&GenerationStagePage>,
        page: &GenerationStagePage,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<GraphCommit, GraphDbError> {
        let (idempotency_key, input_digest) =
            generation_stage_page_receipt(manifest, expected, page)?;
        self.run_gated_batch(
            check,
            |database| {
                if let Some(existing) = crate::state::publication(
                    database,
                    &context.physical_namespace,
                    &idempotency_key,
                )? {
                    if existing.input_digest == input_digest
                        && existing.commit.source_generation == manifest.source_generation
                        && existing.commit.watermark == manifest.watermark
                    {
                        return Ok(GraphBatchPlan::Settled(existing.commit, ()));
                    }
                    return Err(GraphDbError::Conflict);
                }
                if let Some(predecessor) = predecessor {
                    let (prior_key, prior_input) =
                        generation_stage_page_receipt(manifest, expected, predecessor)?;
                    let prior = crate::state::publication(
                        database,
                        &context.physical_namespace,
                        &prior_key,
                    )?
                    .ok_or_else(|| {
                        GraphDbError::unavailable(
                            "graph generation stage predecessor is not applied",
                        )
                    })?;
                    if prior.input_digest != prior_input {
                        return Err(GraphDbError::Conflict);
                    }
                } else if latest_projection(
                    database,
                    &context.physical_namespace,
                    &manifest.projection.projection,
                )?
                .is_some()
                {
                    return Err(GraphDbError::Conflict);
                }
                let (batch, endpoint_namespaces) =
                    prepare_generation_stage_batch(manifest, context, page, check)?;
                let digest = batch.canonical_digest_checked(check)?;
                Ok(GraphBatchPlan::Apply(
                    PreparedGraphBatch {
                        batch,
                        metadata: mutation::CommitMetadata {
                            digest: digest.clone(),
                            generation_dependency_digest: None,
                            publication_record: Some((
                                idempotency_key.clone(),
                                digest,
                                input_digest.clone(),
                            )),
                        },
                        endpoint_namespaces,
                    },
                    (),
                ))
            },
            |_database, commit, ()| Ok(commit),
        )
    }

    fn finalize_staged_generation(
        &self,
        manifest: &GraphGenerationManifest,
        expected: &GraphRecoveredGenerationDigestV1,
        context: &GenerationStageContext,
        last_page: Option<&GenerationStagePage>,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<GraphCommit, GraphDbError> {
        let (idempotency_key, input_digest) =
            generation_stage_finalization_receipt(manifest, expected)?;
        self.run_gated_batch(
            check,
            |database| {
                if let Some(existing) = crate::state::publication(
                    database,
                    &context.physical_namespace,
                    &idempotency_key,
                )? {
                    if existing.input_digest == input_digest
                        && existing.commit.source_generation == manifest.source_generation
                        && existing.commit.watermark == manifest.watermark
                        && existing.commit.generation_dependency_digest.as_ref()
                            == Some(&context.dependency_digest)
                    {
                        return Ok(GraphBatchPlan::Settled(existing.commit, ()));
                    }
                    return Err(GraphDbError::Conflict);
                }
                if let Some(last_page) = last_page {
                    let (last_key, last_input) =
                        generation_stage_page_receipt(manifest, expected, last_page)?;
                    let last = crate::state::publication(
                        database,
                        &context.physical_namespace,
                        &last_key,
                    )?
                    .ok_or_else(|| {
                        GraphDbError::unavailable("graph generation final page is not applied")
                    })?;
                    if last.input_digest != last_input {
                        return Err(GraphDbError::Conflict);
                    }
                }
                let batch = GraphWriteBatch::new_canonical_checked(
                    context.physical_namespace.clone(),
                    manifest.projection.projection.clone(),
                    manifest.source_generation.clone(),
                    manifest.watermark.clone(),
                    Vec::new(),
                    check,
                )?;
                let digest = batch.canonical_digest_checked(check)?;
                Ok(GraphBatchPlan::Apply(
                    PreparedGraphBatch {
                        batch,
                        metadata: mutation::CommitMetadata {
                            digest: digest.clone(),
                            generation_dependency_digest: Some(context.dependency_digest.clone()),
                            publication_record: Some((
                                idempotency_key.clone(),
                                digest,
                                input_digest.clone(),
                            )),
                        },
                        endpoint_namespaces: mutation::RelationEndpointNamespaces::new(),
                    },
                    (),
                ))
            },
            |_database, commit, ()| {
                let mut verified = self.inner.verified_generations.write().map_err(|_| {
                    GraphDbError::unavailable("verified graph generation state lock is poisoned")
                })?;
                verified.collected.remove(&context.locator);
                verified.stored.insert(
                    context.locator.clone(),
                    generation_dependency_locators(manifest),
                );
                Ok(commit)
            },
        )
    }

    pub(crate) fn reopen_and_verify_existing_generation(
        &self,
        manifest: &GraphGenerationManifest,
        expected: &GraphRecoveredGenerationDigestV1,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<(GraphCommit, GraphRecoveredGenerationDigestV1), GraphDbError> {
        check()?;
        let physical_namespace = manifest.physical_namespace()?;
        let projection = manifest.projection.projection.clone();
        let quarantine_key = (physical_namespace.clone(), projection.clone());
        let reopen = self.inner.reopen.clone().ok_or_else(|| {
            GraphDbError::invalid(
                "recovered generation verification requires a persistent graph database",
            )
        })?;
        // The exclusive snapshot-gate claim covers only the physical
        // close/reopen swap. The recovered-digest proof below is read-only,
        // so it runs behind an upgradable claim: snapshot readers proceed
        // while every writer still queues behind this guard, keeping the
        // reopened rows stable for the digest.
        let snapshot_gate = self.inner.snapshot_gate.write();
        {
            let mut database_guard =
                self.inner.database.write().map_err(|_| {
                    GraphDbError::unavailable("graph database write lock is poisoned")
                })?;
            self.ensure_available()?;
            check()?;
            let mut state_guard = self.state_write_guard()?;
            let mut quarantined_guard = self
                .inner
                .quarantined_projections
                .write()
                .map_err(|_| GraphDbError::unavailable("graph quarantine lock is poisoned"))?;
            let database = database_guard.take().ok_or(GraphDbError::Closed)?;
            if let Err(error) = database.close() {
                self.inner.poisoned.store(true, Ordering::Release);
                return Err(GraphDbError::DurabilityUncertain {
                    message: format!(
                        "Grafeo close failed before recovered generation verification: {error}"
                    ),
                });
            }
            let (recovered, recovered_state, quarantined) = match open_recovered_database(&reopen) {
                Ok(recovered) => recovered,
                Err(error) => {
                    self.inner.poisoned.store(true, Ordering::Release);
                    return Err(error);
                }
            };
            *state_guard = recovered_state;
            *quarantined_guard = quarantined;
            *database_guard = Some(recovered);
        }
        let snapshot_gate = ParkingRwLockWriteGuard::downgrade_to_upgradable(snapshot_gate);
        check()?;
        let commit = {
            let guard = self.read_guard()?;
            let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
            match latest_projection(database, &physical_namespace, &projection) {
                Ok(Some(existing)) => existing.commit,
                Ok(None) => {
                    let error = GraphDbError::GenerationMismatch {
                        namespace: manifest.projection.namespace.to_string(),
                        projection: projection.to_string(),
                        generation: manifest.generation.to_string(),
                        message: "recovered generation is missing".to_owned(),
                    };
                    drop(guard);
                    drop(snapshot_gate);
                    self.quarantine_generation(manifest)?;
                    return Err(error);
                }
                Err(error) => {
                    if is_database_fault(&error) {
                        self.inner.poisoned.store(true, Ordering::Release);
                    }
                    return Err(error);
                }
            }
        };
        let verified = {
            let guard = self.read_guard()?;
            let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
            match verify_recovered_generation(database, manifest, expected, check) {
                Ok(verified) => verified,
                Err(error @ GraphDbError::GenerationMismatch { .. }) => {
                    drop(guard);
                    drop(snapshot_gate);
                    self.quarantine_generation(manifest)?;
                    return Err(error);
                }
                Err(error) => {
                    if is_database_fault(&error) {
                        self.inner.poisoned.store(true, Ordering::Release);
                    }
                    return Err(error);
                }
            }
        };
        let was_quarantined = self
            .inner
            .quarantined_projections
            .read()
            .map_err(|_| GraphDbError::unavailable("graph quarantine lock is poisoned"))?
            .contains(&quarantine_key);
        if !was_quarantined {
            return Ok((commit, verified));
        }
        // Quarantine repair: the exclusive claim covers only the durable
        // marker clear and the checkpoint transition (both rewrite the
        // database file). The re-verification afterwards is read-only again,
        // so the gate downgrades back to upgradable and snapshot readers are
        // admitted while the repaired rows stream through the proof.
        let write_gate = RwLockUpgradableReadGuard::upgrade(snapshot_gate);
        {
            let mut database_guard =
                self.inner.database.write().map_err(|_| {
                    GraphDbError::unavailable("graph database write lock is poisoned")
                })?;
            let mut state_guard = self.state_write_guard()?;
            let mut quarantined_guard = self
                .inner
                .quarantined_projections
                .write()
                .map_err(|_| GraphDbError::unavailable("graph quarantine lock is poisoned"))?;
            {
                let database = database_guard.as_ref().ok_or(GraphDbError::Closed)?;
                if let Err(error) =
                    set_projection_quarantine(database, &physical_namespace, &projection, false)
                        .and_then(|()| crate::runtime::sync_wal(database))
                {
                    self.inner.poisoned.store(true, Ordering::Release);
                    return Err(quarantine_transition_failure(
                        "clear recovered generation quarantine",
                        error,
                    ));
                }
            }
            let database = database_guard.take().ok_or(GraphDbError::Closed)?;
            let (recovered, recovered_state, quarantined) =
                match checkpoint_recovered_database(database, &reopen) {
                    Ok(recovered) => recovered,
                    Err(error) => {
                        self.inner.poisoned.store(true, Ordering::Release);
                        return Err(error);
                    }
                };
            let still_quarantined = quarantined.contains(&quarantine_key);
            *state_guard = recovered_state;
            *quarantined_guard = quarantined;
            *database_guard = Some(recovered);
            if still_quarantined {
                self.inner.poisoned.store(true, Ordering::Release);
                return Err(GraphDbError::DurabilityUncertain {
                    message: "recovered generation quarantine remained after checkpoint".to_owned(),
                });
            }
        }
        let snapshot_gate = ParkingRwLockWriteGuard::downgrade_to_upgradable(write_gate);
        let verify_result = {
            let guard = self.read_guard()?;
            let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
            verify_recovered_generation(database, manifest, expected, check)
        };
        match verify_result {
            Ok(verified) => Ok((commit, verified)),
            Err(error) => {
                // Restoring the durable quarantine marker rewrites the file,
                // so the failure path re-takes the exclusive claim.
                let _write_gate = RwLockUpgradableReadGuard::upgrade(snapshot_gate);
                let mut database_guard = self.inner.database.write().map_err(|_| {
                    GraphDbError::unavailable("graph database write lock is poisoned")
                })?;
                let mut state_guard = self.state_write_guard()?;
                let mut quarantined_guard =
                    self.inner.quarantined_projections.write().map_err(|_| {
                        GraphDbError::unavailable("graph quarantine lock is poisoned")
                    })?;
                let database = database_guard.take().ok_or(GraphDbError::Closed)?;
                let (recovered, recovered_state, quarantined) =
                    match requarantine_after_failed_checkpoint_verification(
                        database,
                        &reopen,
                        &physical_namespace,
                        &projection,
                        &error,
                    ) {
                        Ok(recovered) => recovered,
                        Err(restore_error) => {
                            self.inner.poisoned.store(true, Ordering::Release);
                            return Err(restore_error);
                        }
                    };
                if is_database_fault(&error) {
                    self.inner.poisoned.store(true, Ordering::Release);
                }
                *state_guard = recovered_state;
                *quarantined_guard = quarantined;
                *database_guard = Some(recovered);
                Err(error)
            }
        }
    }

    pub(crate) fn install_verified_generation(
        &self,
        lease: std::sync::Arc<VerifiedGenerationLease>,
    ) -> Result<Option<std::sync::Arc<VerifiedGenerationLease>>, GraphDbError> {
        let mut state = self.inner.verified_generations.write().map_err(|_| {
            GraphDbError::unavailable("verified graph generation state lock is poisoned")
        })?;
        state.install(lease)
    }

    pub(crate) fn remember_verified_generation(
        &self,
        lease: &std::sync::Arc<VerifiedGenerationLease>,
    ) -> Result<(), GraphDbError> {
        let mut state = self.inner.verified_generations.write().map_err(|_| {
            GraphDbError::unavailable("verified graph generation state lock is poisoned")
        })?;
        state.remember(lease)
    }

    pub(crate) fn delete_generation_contents(
        &self,
        locator: &GenerationLocator,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<(), GraphDbError> {
        check()?;
        let namespace = locator.physical_namespace()?;
        let commit = {
            let guard = self.read_guard()?;
            let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
            latest_projection(database, &namespace, &locator.projection.projection)?
                .map(|projection| projection.commit)
        };
        let Some(commit) = commit else {
            let mut state = self.inner.verified_generations.write().map_err(|_| {
                GraphDbError::unavailable("verified graph generation state lock is poisoned")
            })?;
            state.known.remove(locator);
            state.quarantined.remove(locator);
            state.stored.remove(locator);
            state.retiring.remove(locator);
            state.collected.insert(locator.clone());
            return Ok(());
        };
        self.delete_projection_checked(
            namespace,
            locator.projection.projection.clone(),
            commit.source_generation,
            commit.watermark,
            check,
        )?;
        let mut state = self.inner.verified_generations.write().map_err(|_| {
            GraphDbError::unavailable("verified graph generation state lock is poisoned")
        })?;
        state.known.remove(locator);
        state.quarantined.remove(locator);
        state.stored.remove(locator);
        state.retiring.remove(locator);
        state.collected.insert(locator.clone());
        Ok(())
    }

    fn delete_projection_checked(
        &self,
        namespace: GraphNamespace,
        projection: crate::GraphProjectionId,
        source_generation: crate::SourceGeneration,
        watermark: crate::GraphWatermark,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<GraphCommit, GraphDbError> {
        check()?;
        self.run_gated_batch(
            check,
            |database| {
                let relation_ids =
                    projection_relations_checked(database, &namespace, &projection, check)?
                        .into_iter()
                        .map(|relation| relation.relation.identity)
                        .collect::<std::collections::BTreeSet<_>>();
                let entity_ids =
                    projection_entities_checked(database, &namespace, &projection, check)?
                        .into_iter()
                        .map(|entity| entity.entity.identity)
                        .collect::<std::collections::BTreeSet<_>>();
                let mut mutations = Vec::with_capacity(
                    relation_ids
                        .len()
                        .checked_add(entity_ids.len())
                        .ok_or_else(|| {
                            GraphDbError::invalid("graph deletion mutation count overflow")
                        })?,
                );
                for identity in relation_ids {
                    check()?;
                    mutations.push(GraphMutation::DeleteRelation(identity));
                }
                for identity in entity_ids {
                    check()?;
                    mutations.push(GraphMutation::DeleteEntity(identity));
                }
                let batch = GraphWriteBatch::new_canonical_checked(
                    namespace,
                    projection,
                    source_generation,
                    watermark,
                    mutations,
                    check,
                )?;
                let digest = batch.canonical_digest_checked(check)?;
                Ok(GraphBatchPlan::Apply(
                    PreparedGraphBatch {
                        batch,
                        metadata: mutation::CommitMetadata::for_digest(digest),
                        endpoint_namespaces: mutation::RelationEndpointNamespaces::new(),
                    },
                    (),
                ))
            },
            |_database, commit, ()| Ok(commit),
        )
    }

    pub(crate) fn generation_relation(
        &self,
        snapshot: &VerifiedGraphSnapshot,
        reference: &GraphRelationRef,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Option<GraphGenerationRelation>, GraphDbError> {
        if cancellation.is_cancelled() {
            return Err(GraphDbError::Cancelled);
        }
        let namespace_projection = snapshot.namespace_projection_map()?;
        let guard = self.read_database(cancellation.as_ref())?;
        let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
        let relation_lease = snapshot.lease_for_projection(&reference.projection)?;
        let relation_namespace = relation_lease.locator.physical_namespace()?;
        let Some(stored) = load_relation(database, &relation_namespace, &reference.identity)?
        else {
            return Ok(None);
        };
        let edge = database
            .graph_store()
            .get_edge(stored.edge)
            .ok_or_else(|| GraphDbError::Corrupt {
                message: "verified generation relation edge is missing".to_owned(),
            })?;
        let from = typed_entity_ref(database, edge.src, &namespace_projection)?;
        let to = typed_entity_ref(database, edge.dst, &namespace_projection)?;
        if cancellation.is_cancelled() {
            return Err(GraphDbError::Cancelled);
        }
        GraphGenerationRelation::new(
            stored.relation.identity,
            from,
            to,
            stored.relation.kind,
            stored.relation.properties,
        )
        .map(Some)
    }

    pub(crate) fn traverse_generation(
        &self,
        snapshot: &VerifiedGraphSnapshot,
        request: TraversalRequest,
    ) -> Result<VerifiedTraversalResult, GraphDbError> {
        if request.cancellation.is_cancelled() {
            return Err(GraphDbError::Cancelled);
        }
        if request.max_visits == 0 {
            return Err(GraphDbError::budget_exhausted_count(
                GraphBudgetKind::Read,
                request.max_visits,
            ));
        }
        if request.max_results == 0 {
            return Ok(VerifiedTraversalResult { visits: Vec::new() });
        }
        let namespace_projection = snapshot.namespace_projection_map()?;
        let guard = self.read_database(request.cancellation.as_ref())?;
        let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
        let head_namespace = crate::generation::physical_namespace(
            &snapshot.projection().namespace,
            &snapshot.projection().projection,
            snapshot.generation(),
        )?;
        let start = crate::state::load_entity(database, &head_namespace, &request.start)?
            .ok_or_else(|| GraphDbError::invalid("traversal start entity does not exist"))?;
        let store = database.graph_store();
        let mut queue = VecDeque::from([(start.node, 0_usize, None)]);
        let mut discovered = HashSet::from([start.node]);
        let mut visits = Vec::new();
        while let Some((node, depth, via_relation)) = queue.pop_front() {
            if request.cancellation.is_cancelled() {
                return Err(GraphDbError::Cancelled);
            }
            if visits.len() >= request.max_visits {
                return Err(GraphDbError::budget_exhausted_count(
                    GraphBudgetKind::Read,
                    request.max_visits,
                ));
            }
            visits.push(VerifiedTraversalVisit {
                entity: typed_entity_ref(database, node, &namespace_projection)?,
                depth,
                via_relation,
            });
            if visits.len() >= request.max_results || depth >= request.max_depth {
                continue;
            }
            let directions: &[Direction] = match request.direction {
                GraphTraversalDirection::Outgoing => &[Direction::Outgoing],
                GraphTraversalDirection::Incoming => &[Direction::Incoming],
                GraphTraversalDirection::Both => &[Direction::Outgoing, Direction::Incoming],
            };
            let mut adjacent = Vec::new();
            for direction in directions {
                for (neighbor, edge_id) in store.edges_from(node, *direction) {
                    if request.cancellation.is_cancelled() {
                        return Err(GraphDbError::Cancelled);
                    }
                    let edge = store
                        .get_edge(edge_id)
                        .ok_or_else(|| GraphDbError::Corrupt {
                            message: "verified traversal relation edge is missing".to_owned(),
                        })?;
                    let kind = relation_kind_from_type(edge.edge_type.as_str())?;
                    if !request.relation_kinds.is_empty() && !request.relation_kinds.contains(&kind)
                    {
                        continue;
                    }
                    let relation_namespace = GraphNamespace::new(required_string(
                        edge.get_property(NAMESPACE_PROPERTY),
                        "verified traversal relation namespace",
                    )?)
                    .map_err(|error| GraphDbError::Corrupt {
                        message: format!(
                            "verified traversal relation namespace is invalid: {error}"
                        ),
                    })?;
                    let Some(relation_projection) =
                        namespace_projection.get(&relation_namespace).cloned()
                    else {
                        continue;
                    };
                    let stored = load_relation_by_edge(database, edge_id)?.ok_or_else(|| {
                        GraphDbError::Corrupt {
                            message: "verified traversal edge has no typed relation locator"
                                .to_owned(),
                        }
                    })?;
                    let neighbor_entity = load_entity_by_node(database, neighbor)?;
                    let Some(entity_projection) = namespace_projection
                        .get(&neighbor_entity.namespace)
                        .cloned()
                    else {
                        continue;
                    };
                    let entity =
                        GraphEntityRef::new(entity_projection, neighbor_entity.entity.identity);
                    adjacent.push((
                        GraphRelationRef::new(relation_projection, stored.relation.identity),
                        entity,
                        neighbor,
                    ));
                }
            }
            adjacent.sort_by(|left, right| (&left.0, &left.1).cmp(&(&right.0, &right.1)));
            adjacent.dedup_by(|left, right| left.0 == right.0 && left.1 == right.1);
            for (relation, _, neighbor) in adjacent {
                if discovered.insert(neighbor) {
                    let next_depth = depth.checked_add(1).ok_or_else(|| {
                        GraphDbError::budget_exhausted_count(
                            GraphBudgetKind::Read,
                            request.max_depth,
                        )
                    })?;
                    queue.push_back((neighbor, next_depth, Some(relation)));
                }
            }
        }
        Ok(VerifiedTraversalResult { visits })
    }

    pub(crate) fn verified_generation(
        &self,
        locator: &GenerationLocator,
    ) -> Result<Option<std::sync::Arc<VerifiedGenerationLease>>, GraphDbError> {
        let state = self.inner.verified_generations.read().map_err(|_| {
            GraphDbError::unavailable("verified graph generation state lock is poisoned")
        })?;
        if state.quarantined.contains(locator) {
            return Err(GraphDbError::GenerationMismatch {
                namespace: locator.projection.namespace.to_string(),
                projection: locator.projection.projection.to_string(),
                generation: locator.generation.to_string(),
                message: "generation remains quarantined after recovery mismatch".to_owned(),
            });
        }
        if state.retiring.contains(locator) || state.collected.contains(locator) {
            return Err(GraphDbError::Conflict);
        }
        Ok(state.known.get(locator).and_then(std::sync::Weak::upgrade))
    }

    fn require_exact_dependencies(
        &self,
        manifest: &GraphGenerationManifest,
    ) -> Result<BTreeMap<crate::GraphProjectionIdentity, GraphNamespace>, GraphDbError> {
        let state = self.inner.verified_generations.read().map_err(|_| {
            GraphDbError::unavailable("verified graph generation state lock is poisoned")
        })?;
        let mut namespaces = BTreeMap::new();
        for dependency in &manifest.dependencies {
            let locator = GenerationLocator::new(
                dependency.projection.clone(),
                dependency.generation.clone(),
            );
            if state.retiring.contains(&locator) || state.collected.contains(&locator) {
                return Err(GraphDbError::Conflict);
            }
            let Some(head) = state.known.get(&locator).and_then(std::sync::Weak::upgrade) else {
                return Err(GraphDbError::Conflict);
            };
            namespaces.insert(
                dependency.projection.clone(),
                head.locator.physical_namespace()?,
            );
        }
        Ok(namespaces)
    }

    pub(crate) fn quarantine_generation(
        &self,
        manifest: &GraphGenerationManifest,
    ) -> Result<(), GraphDbError> {
        let locator =
            GenerationLocator::new(manifest.projection.clone(), manifest.generation.clone());
        let physical_namespace = locator.physical_namespace()?;
        let _snapshot_gate = self.inner.snapshot_gate.write();
        let mut database_guard = self
            .inner
            .database
            .write()
            .map_err(|_| GraphDbError::unavailable("graph database write lock is poisoned"))?;
        let mut format_state = self.state_write_guard()?;
        let mut projection_quarantine = self
            .inner
            .quarantined_projections
            .write()
            .map_err(|_| GraphDbError::unavailable("graph quarantine lock is poisoned"))?;
        let reopen = self.inner.reopen.clone().ok_or_else(|| {
            GraphDbError::invalid("generation quarantine requires a persistent graph database")
        })?;
        {
            let database = database_guard.as_ref().ok_or(GraphDbError::Closed)?;
            crate::recovery::set_projection_quarantine(
                database,
                &physical_namespace,
                &manifest.projection.projection,
                true,
            )
            .and_then(|()| crate::runtime::sync_wal(database))
            .inspect_err(|_| {
                self.inner.poisoned.store(true, Ordering::Release);
            })?;
        }
        let database = database_guard.take().ok_or(GraphDbError::Closed)?;
        let (recovered, recovered_state, recovered_quarantine) =
            crate::recovery::checkpoint_recovered_database(database, &reopen).inspect_err(
                |_| {
                    self.inner.poisoned.store(true, Ordering::Release);
                },
            )?;
        if !recovered_quarantine.contains(&(
            physical_namespace.clone(),
            manifest.projection.projection.clone(),
        )) {
            self.inner.poisoned.store(true, Ordering::Release);
            return Err(GraphDbError::DurabilityUncertain {
                message: "generation quarantine disappeared after durable checkpoint".to_owned(),
            });
        }
        *format_state = recovered_state;
        *projection_quarantine = recovered_quarantine;
        *database_guard = Some(recovered);
        let mut state = self.inner.verified_generations.write().map_err(|_| {
            GraphDbError::unavailable("verified graph generation state lock is poisoned")
        })?;
        state.quarantine(locator);
        Ok(())
    }
}

fn typed_entity_ref(
    database: &grafeo_engine::GrafeoDB,
    node: grafeo_common::types::NodeId,
    namespace_projection: &BTreeMap<GraphNamespace, crate::GraphProjectionIdentity>,
) -> Result<GraphEntityRef, GraphDbError> {
    let stored = load_entity_by_node(database, node)?;
    let projection = namespace_projection
        .get(&stored.namespace)
        .cloned()
        .ok_or_else(|| GraphDbError::Corrupt {
            message: "verified graph entity escapes snapshot dependency closure".to_owned(),
        })?;
    Ok(GraphEntityRef::new(projection, stored.entity.identity))
}

fn generation_stage_pages(
    manifest: &GraphGenerationManifest,
) -> Result<Vec<GenerationStagePage>, GraphDbError> {
    let mut pages = Vec::new();
    append_generation_stage_pages(
        &mut pages,
        GenerationStagePageKind::Entities,
        manifest.entities.len(),
        |index| generation_entity_live_bytes(&manifest.entities[index]),
    )?;
    append_generation_stage_pages(
        &mut pages,
        GenerationStagePageKind::Relations,
        manifest.relations.len(),
        |index| generation_relation_live_bytes(&manifest.relations[index]),
    )?;
    Ok(pages)
}

fn generation_entity_live_bytes(entity: &crate::GraphEntity) -> Result<usize, GraphDbError> {
    entity
        .labels
        .iter()
        .try_fold(entity.identity.as_str().len(), |bytes, label| {
            bytes
                .checked_add(label.as_str().len())
                .ok_or_else(stage_live_bytes_exhausted)
        })?
        .checked_add(graph_properties_live_bytes(&entity.properties)?)
        .ok_or_else(stage_live_bytes_exhausted)
}

fn generation_relation_live_bytes(
    relation: &GraphGenerationRelation,
) -> Result<usize, GraphDbError> {
    [
        relation.identity.as_str().len(),
        relation.from.identity.as_str().len(),
        relation.from.projection.namespace.as_str().len(),
        relation.from.projection.projection.as_str().len(),
        relation.to.identity.as_str().len(),
        relation.to.projection.namespace.as_str().len(),
        relation.to.projection.projection.as_str().len(),
        relation.kind.as_str().len(),
        graph_properties_live_bytes(&relation.properties)?,
    ]
    .into_iter()
    .try_fold(0usize, |bytes, next| {
        bytes
            .checked_add(next)
            .ok_or_else(stage_live_bytes_exhausted)
    })
}

fn stage_live_bytes_exhausted() -> GraphDbError {
    GraphDbError::budget_exhausted_count(
        GraphBudgetKind::Write,
        MAX_VERIFIED_GENERATION_BATCH_LIVE_BYTES,
    )
}

fn append_generation_stage_pages(
    pages: &mut Vec<GenerationStagePage>,
    kind: GenerationStagePageKind,
    count: usize,
    property_bytes: impl Fn(usize) -> Result<usize, GraphDbError>,
) -> Result<(), GraphDbError> {
    let mut start = 0usize;
    let mut live_bytes = 0usize;
    for index in 0..count {
        let next_bytes = property_bytes(index)?;
        if next_bytes > MAX_VERIFIED_GENERATION_BATCH_LIVE_BYTES {
            return Err(GraphDbError::budget_exhausted_count(
                GraphBudgetKind::Write,
                MAX_VERIFIED_GENERATION_BATCH_LIVE_BYTES,
            ));
        }
        let page_is_full = index - start == MAX_VERIFIED_GENERATION_BATCH_MUTATIONS;
        let bytes_would_overflow = live_bytes
            .checked_add(next_bytes)
            .is_none_or(|bytes| bytes > MAX_VERIFIED_GENERATION_BATCH_LIVE_BYTES);
        if index > start && (page_is_full || bytes_would_overflow) {
            pages.push(GenerationStagePage {
                ordinal: pages.len(),
                kind,
                range: start..index,
                live_bytes,
            });
            start = index;
            live_bytes = 0;
        }
        live_bytes = live_bytes.checked_add(next_bytes).ok_or_else(|| {
            GraphDbError::budget_exhausted_count(
                GraphBudgetKind::Write,
                MAX_VERIFIED_GENERATION_BATCH_LIVE_BYTES,
            )
        })?;
    }
    if start < count {
        pages.push(GenerationStagePage {
            ordinal: pages.len(),
            kind,
            range: start..count,
            live_bytes,
        });
    }
    Ok(())
}

fn generation_stage_page_receipt(
    manifest: &GraphGenerationManifest,
    expected: &GraphRecoveredGenerationDigestV1,
    page: &GenerationStagePage,
) -> Result<(GraphIdempotencyKey, String), GraphDbError> {
    let digest = tracedecay_domain::canonical_sha256(&(
        "tracedecay.graph-generation-native-page.v1",
        &manifest.projection,
        &manifest.generation,
        &manifest.source_generation,
        &manifest.watermark,
        expected.as_str(),
        page.ordinal,
        page.kind.as_str(),
        page.range.start,
        page.range.end,
        page.live_bytes,
    ))
    .map_err(|error| GraphDbError::invalid(error.to_string()))?;
    Ok((
        GraphIdempotencyKey::new(format!("generation-page:{}", digest.as_str()))?,
        digest.as_str().to_owned(),
    ))
}

fn generation_stage_finalization_receipt(
    manifest: &GraphGenerationManifest,
    expected: &GraphRecoveredGenerationDigestV1,
) -> Result<(GraphIdempotencyKey, String), GraphDbError> {
    let digest = tracedecay_domain::canonical_sha256(&(
        "tracedecay.graph-generation-native-finalization.v1",
        &manifest.projection,
        &manifest.generation,
        &manifest.source_generation,
        &manifest.watermark,
        expected.as_str(),
    ))
    .map_err(|error| GraphDbError::invalid(error.to_string()))?;
    Ok((
        GraphIdempotencyKey::new(format!("generation-finalize:{}", digest.as_str()))?,
        digest.as_str().to_owned(),
    ))
}

fn prepare_generation_stage_batch(
    manifest: &GraphGenerationManifest,
    context: &GenerationStageContext,
    page: &GenerationStagePage,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<(GraphWriteBatch, mutation::RelationEndpointNamespaces), GraphDbError> {
    if page.mutation_count() > MAX_VERIFIED_GENERATION_BATCH_MUTATIONS
        || page.live_bytes() > MAX_VERIFIED_GENERATION_BATCH_LIVE_BYTES
    {
        return Err(GraphDbError::budget_exhausted_count(
            GraphBudgetKind::Write,
            MAX_VERIFIED_GENERATION_BATCH_LIVE_BYTES,
        ));
    }
    let mut endpoint_namespaces = mutation::RelationEndpointNamespaces::new();
    let mutations = match page.kind {
        GenerationStagePageKind::Entities => manifest
            .entities
            .get(page.range.clone())
            .ok_or(GraphDbError::Conflict)?
            .iter()
            .map(|entity| {
                check()?;
                Ok(GraphMutation::UpsertEntity(entity.clone()))
            })
            .collect::<Result<Vec<_>, GraphDbError>>()?,
        GenerationStagePageKind::Relations => manifest
            .relations
            .get(page.range.clone())
            .ok_or(GraphDbError::Conflict)?
            .iter()
            .map(|relation| {
                check()?;
                endpoint_namespaces.insert(
                    relation.identity.clone(),
                    (
                        endpoint_namespace(
                            manifest,
                            &context.physical_namespace,
                            &context.dependency_namespaces,
                            &relation.from.projection,
                        )?,
                        endpoint_namespace(
                            manifest,
                            &context.physical_namespace,
                            &context.dependency_namespaces,
                            &relation.to.projection,
                        )?,
                    ),
                );
                Ok(GraphMutation::UpsertRelation(relation.storage_relation()?))
            })
            .collect::<Result<Vec<_>, GraphDbError>>()?,
    };
    let batch = GraphWriteBatch::new_canonical_checked(
        context.physical_namespace.clone(),
        manifest.projection.projection.clone(),
        manifest.source_generation.clone(),
        manifest.watermark.clone(),
        mutations,
        check,
    )?;
    Ok((batch, endpoint_namespaces))
}

fn endpoint_namespace(
    manifest: &GraphGenerationManifest,
    candidate_namespace: &GraphNamespace,
    dependencies: &BTreeMap<crate::GraphProjectionIdentity, GraphNamespace>,
    projection: &crate::GraphProjectionIdentity,
) -> Result<GraphNamespace, GraphDbError> {
    if projection == &manifest.projection {
        return Ok(candidate_namespace.clone());
    }
    dependencies
        .get(projection)
        .cloned()
        .ok_or_else(|| GraphDbError::invalid("relation endpoint dependency is not verified"))
}

fn generation_dependency_locators(manifest: &GraphGenerationManifest) -> Vec<GenerationLocator> {
    manifest
        .dependencies
        .iter()
        .map(|dependency| {
            GenerationLocator::new(dependency.projection.clone(), dependency.generation.clone())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::Arc;
    use std::sync::mpsc;
    use std::time::Duration;

    use tempfile::TempDir;
    use tracedecay_store::runtime::GraphRecoveredGenerationDigestV1;

    use crate::generation::{
        manifest_canonicalizations, recovered_generation_enumerations,
        reset_manifest_canonicalizations, reset_recovered_generation_enumerations,
    };
    use crate::projection::{batch_canonicalizations, reset_batch_canonicalizations};
    use crate::{
        GraphDbError, GraphDbLocation, GraphDbOpenOptions, GraphDbOwner, GraphDurability,
        GraphEntity, GraphEntityId, GraphFormatVersion, GraphGenerationDependency,
        GraphGenerationId, GraphGenerationManifest, GraphIdempotencyKey, GraphNamespace,
        GraphProjectionId, GraphProjectionIdentity, GraphProperty, GraphPropertyName,
        GraphWatermark, MAX_VERIFIED_GENERATION_BATCH_LIVE_BYTES,
        MAX_VERIFIED_GENERATION_BATCH_MUTATIONS, NeverCancelled, SourceGeneration,
    };

    use super::{GenerationLocator, generation_stage_pages};

    fn manifest(source: &str, watermark: &str) -> GraphGenerationManifest {
        GraphGenerationManifest::new(
            GraphProjectionIdentity::new(
                GraphNamespace::new("recovered-commit").unwrap(),
                GraphProjectionId::new("metadata").unwrap(),
            ),
            GraphGenerationId::new("generation").unwrap(),
            SourceGeneration::new(source).unwrap(),
            GraphWatermark::new(watermark).unwrap(),
            vec![],
            vec![],
            vec![],
        )
        .unwrap()
    }

    fn persistent_database(temp: &TempDir) -> (GraphDbOwner, crate::GraphDbLeaseV1) {
        let owner = GraphDbOwner::open(GraphDbOpenOptions {
            location: GraphDbLocation::Persistent(temp.path().join("commit-metadata.grafeo")),
            expected_format: GraphFormatVersion::current(),
            durability: GraphDurability::WalSync,
            cancellation: Arc::new(NeverCancelled),
        })
        .unwrap();
        let database = owner.issue_lease().unwrap();
        (owner, database)
    }

    fn sealed_digest(
        manifest: &GraphGenerationManifest,
    ) -> tracedecay_store::runtime::GraphRecoveredGenerationDigestV1 {
        manifest.expected_recovered_digest(&|| Ok(())).unwrap()
    }

    #[test]
    fn recovered_generation_rejects_stale_persisted_source_generation() {
        let temp = TempDir::new().unwrap();
        let (_owner, database) = persistent_database(&temp);
        database
            .apply_generation_unverified(&manifest("source:old", "watermark:one"), &|| Ok(()))
            .unwrap();

        let changed = manifest("source:new", "watermark:one");
        assert!(matches!(
            database.reopen_and_verify_existing_generation(
                &changed,
                &sealed_digest(&changed),
                &|| { Ok(()) }
            ),
            Err(GraphDbError::GenerationMismatch { .. })
        ));
    }

    #[test]
    fn recovered_generation_rejects_stale_persisted_watermark() {
        let temp = TempDir::new().unwrap();
        let (_owner, database) = persistent_database(&temp);
        database
            .apply_generation_unverified(&manifest("source:one", "watermark:old"), &|| Ok(()))
            .unwrap();

        let changed = manifest("source:one", "watermark:new");
        assert!(matches!(
            database.reopen_and_verify_existing_generation(
                &changed,
                &sealed_digest(&changed),
                &|| { Ok(()) }
            ),
            Err(GraphDbError::GenerationMismatch { .. })
        ));
    }

    #[test]
    fn recovered_generation_rejects_stale_persisted_dependency_metadata() {
        let temp = TempDir::new().unwrap();
        let (_owner, database) = persistent_database(&temp);
        let original = manifest("source:one", "watermark:one");
        database
            .apply_generation_unverified(&original, &|| Ok(()))
            .unwrap();
        let mut changed = original;
        changed.dependencies.push(GraphGenerationDependency::new(
            GraphProjectionIdentity::new(
                GraphNamespace::new("dependency").unwrap(),
                GraphProjectionId::new("metadata").unwrap(),
            ),
            GraphGenerationId::new("dependency-generation").unwrap(),
            GraphIdempotencyKey::new("dependency-publication").unwrap(),
        ));

        assert!(matches!(
            database.reopen_and_verify_existing_generation(
                &changed,
                &sealed_digest(&changed),
                &|| { Ok(()) }
            ),
            Err(GraphDbError::GenerationMismatch { .. })
        ));
    }

    #[test]
    fn persistent_generation_reopen_enumerates_large_projection_once() {
        let temp = TempDir::new().unwrap();
        let owner = GraphDbOwner::open(GraphDbOpenOptions {
            location: GraphDbLocation::Persistent(temp.path().join("one-pass.grafeo")),
            expected_format: GraphFormatVersion::current(),
            durability: GraphDurability::WalSync,
            cancellation: Arc::new(NeverCancelled),
        })
        .unwrap();
        let database = owner.issue_lease().unwrap();
        let manifest = GraphGenerationManifest::new(
            GraphProjectionIdentity::new(
                GraphNamespace::new("one-pass").unwrap(),
                GraphProjectionId::new("large").unwrap(),
            ),
            GraphGenerationId::new("generation-large").unwrap(),
            SourceGeneration::new("source-large").unwrap(),
            GraphWatermark::new("watermark-large").unwrap(),
            vec![],
            (0..5_000)
                .map(|index| {
                    GraphEntity::new(
                        GraphEntityId::new(format!("entity:{index:05}")).unwrap(),
                        BTreeSet::new(),
                        BTreeMap::new(),
                    )
                    .unwrap()
                })
                .collect(),
            vec![],
        )
        .unwrap();
        database
            .apply_generation_unverified(&manifest, &|| Ok(()))
            .unwrap();

        let sealed = sealed_digest(&manifest);
        reset_recovered_generation_enumerations();
        database
            .reopen_and_verify_existing_generation(&manifest, &sealed, &|| Ok(()))
            .unwrap();

        assert_eq!(recovered_generation_enumerations(), 1);
        owner.close().unwrap();
    }

    fn large_persistent_generation(
        temp: &TempDir,
        namespace: &str,
    ) -> (GraphDbOwner, crate::GraphDbLeaseV1, GraphGenerationManifest) {
        let owner = GraphDbOwner::open(GraphDbOpenOptions {
            location: GraphDbLocation::Persistent(temp.path().join(format!("{namespace}.grafeo"))),
            expected_format: GraphFormatVersion::current(),
            durability: GraphDurability::WalSync,
            cancellation: Arc::new(NeverCancelled),
        })
        .unwrap();
        let database = owner.issue_lease().unwrap();
        let manifest = large_manifest(namespace);
        reset_batch_canonicalizations();
        database
            .apply_generation_unverified(&manifest, &|| Ok(()))
            .unwrap();
        assert_eq!(
            batch_canonicalizations(),
            3,
            "two bounded row pages plus final metadata bind are hashed once each"
        );
        (owner, database, manifest)
    }

    fn large_manifest(namespace: &str) -> GraphGenerationManifest {
        GraphGenerationManifest::new(
            GraphProjectionIdentity::new(
                GraphNamespace::new(namespace).unwrap(),
                GraphProjectionId::new("large").unwrap(),
            ),
            GraphGenerationId::new("generation-large").unwrap(),
            SourceGeneration::new("source-large").unwrap(),
            GraphWatermark::new("watermark-large").unwrap(),
            vec![],
            (0..5_000)
                .map(|index| {
                    GraphEntity::new(
                        GraphEntityId::new(format!("entity:{index:05}")).unwrap(),
                        BTreeSet::new(),
                        BTreeMap::new(),
                    )
                    .unwrap()
                })
                .collect(),
            vec![],
        )
        .unwrap()
    }

    fn foreign_recovered_digest() -> GraphRecoveredGenerationDigestV1 {
        GraphRecoveredGenerationDigestV1::new(format!("sha256:{}", "0".repeat(64))).unwrap()
    }

    #[test]
    fn reopen_verification_streams_digest_without_blocking_snapshot_readers() {
        let temp = TempDir::new().unwrap();
        let (owner, database, manifest) = large_persistent_generation(&temp, "reader-admission");

        let (start_tx, start_rx) = mpsc::channel::<()>();
        let (done_tx, done_rx) = mpsc::channel::<()>();
        let reader = {
            let database = database.clone();
            std::thread::spawn(move || {
                start_rx.recv().unwrap();
                drop(database.snapshot().unwrap());
                done_tx.send(()).unwrap();
            })
        };
        let gate = Arc::clone(&database.inner.snapshot_gate);
        let admitted = Cell::new(0usize);
        let refused = Cell::new(0usize);
        let signalled = Cell::new(false);
        let reader_completed = Cell::new(None::<bool>);
        let check = || {
            if recovered_generation_enumerations() == 0 {
                return Ok(());
            }
            if gate.try_read().is_some() {
                admitted.set(admitted.get() + 1);
            } else {
                refused.set(refused.get() + 1);
            }
            if !signalled.get() {
                signalled.set(true);
                start_tx.send(()).unwrap();
                reader_completed.set(Some(done_rx.recv_timeout(Duration::from_secs(30)).is_ok()));
            }
            Ok(())
        };
        let sealed = sealed_digest(&manifest);
        reset_recovered_generation_enumerations();
        database
            .reopen_and_verify_existing_generation(&manifest, &sealed, &check)
            .unwrap();
        reader.join().unwrap();

        assert!(
            admitted.get() > 0,
            "the recovered-digest stream must run outside the exclusive snapshot gate"
        );
        assert_eq!(
            refused.get(),
            0,
            "no part of the recovered-digest stream may hold the snapshot gate exclusively"
        );
        assert_eq!(
            reader_completed.get(),
            Some(true),
            "a concurrent snapshot reader must complete while the digest streams"
        );
        owner.close().unwrap();
    }

    #[test]
    fn sealed_digest_reopen_skips_manifest_recanonicalization() {
        let temp = TempDir::new().unwrap();
        let (owner, database, manifest) = large_persistent_generation(&temp, "sealed-reuse");
        let sealed = sealed_digest(&manifest);

        reset_recovered_generation_enumerations();
        reset_manifest_canonicalizations();
        let (_, recovered) = database
            .reopen_and_verify_existing_generation(&manifest, &sealed, &|| Ok(()))
            .unwrap();
        assert_eq!(recovered, sealed);
        assert_eq!(
            manifest_canonicalizations(),
            0,
            "the sealed digest replaces every full-manifest re-stream during hydrate"
        );
        assert_eq!(
            recovered_generation_enumerations(),
            1,
            "the recovered-digest proof still streams the reopened rows"
        );
        owner.close().unwrap();
    }

    #[test]
    fn foreign_sealed_digest_still_fails_recovered_proof_then_repairs() {
        let temp = TempDir::new().unwrap();
        let (owner, database, manifest) = large_persistent_generation(&temp, "sealed-mismatch");
        let sealed = sealed_digest(&manifest);

        assert!(matches!(
            database.reopen_and_verify_existing_generation(
                &manifest,
                &foreign_recovered_digest(),
                &|| Ok(())
            ),
            Err(GraphDbError::GenerationMismatch { .. })
        ));

        // The mismatch quarantined the generation. Hydrating with the exact
        // sealed digest clears the durable marker through the checkpoint
        // transition and re-verifies the repaired rows; that second (repair)
        // enumeration must also admit snapshot readers instead of holding
        // the gate exclusively.
        let (start_tx, start_rx) = mpsc::channel::<()>();
        let (done_tx, done_rx) = mpsc::channel::<()>();
        let reader = {
            let database = database.clone();
            std::thread::spawn(move || {
                start_rx.recv().unwrap();
                drop(database.snapshot().unwrap());
                done_tx.send(()).unwrap();
            })
        };
        let gate = Arc::clone(&database.inner.snapshot_gate);
        let admitted = Cell::new(0usize);
        let refused = Cell::new(0usize);
        let signalled = Cell::new(false);
        let reader_completed = Cell::new(None::<bool>);
        let check = || {
            // The repair path enumerates twice: the pre-repair proof, then
            // the post-checkpoint re-verification. Sample the gate during
            // the repair enumeration specifically.
            if recovered_generation_enumerations() < 2 {
                return Ok(());
            }
            if gate.try_read().is_some() {
                admitted.set(admitted.get() + 1);
            } else {
                refused.set(refused.get() + 1);
            }
            if !signalled.get() {
                signalled.set(true);
                start_tx.send(()).unwrap();
                reader_completed.set(Some(done_rx.recv_timeout(Duration::from_secs(30)).is_ok()));
            }
            Ok(())
        };
        reset_recovered_generation_enumerations();
        let (_, recovered) = database
            .reopen_and_verify_existing_generation(&manifest, &sealed, &check)
            .unwrap();
        reader.join().unwrap();
        assert_eq!(recovered, sealed);
        assert_eq!(
            recovered_generation_enumerations(),
            2,
            "quarantine repair re-verifies the checkpointed rows"
        );
        assert!(
            admitted.get() > 0,
            "the repair re-verification must run outside the exclusive snapshot gate"
        );
        assert_eq!(
            refused.get(),
            0,
            "no part of the repair re-verification may hold the snapshot gate exclusively"
        );
        assert_eq!(
            reader_completed.get(),
            Some(true),
            "a concurrent snapshot reader must complete while the repair digest streams"
        );
        owner.close().unwrap();
    }

    #[test]
    fn retry_admission_apply_then_reopen_streams_rows_once() {
        let temp = TempDir::new().unwrap();
        let (owner, database, manifest) = large_persistent_generation(&temp, "retry-admission");
        let sealed = sealed_digest(&manifest);
        let first = database
            .apply_generation_unverified_with_digest(&manifest, &sealed, &|| Ok(()))
            .unwrap();

        // The live retry admission sequence is publish's apply-then-reopen.
        // The re-seat apply is bookkeeping only; the mandatory close/reopen
        // recovered-digest proof is the one and only row stream.
        reset_recovered_generation_enumerations();
        reset_manifest_canonicalizations();
        reset_batch_canonicalizations();
        let reapplied = database
            .apply_generation_unverified_with_digest(&manifest, &sealed, &|| Ok(()))
            .unwrap();
        let (_, recovered) = database
            .reopen_and_verify_existing_generation(&manifest, &sealed, &|| Ok(()))
            .unwrap();

        assert_eq!(reapplied.sequence, first.sequence);
        assert_eq!(reapplied.digest, first.digest);
        assert_eq!(recovered, sealed);
        assert_eq!(
            batch_canonicalizations(),
            0,
            "a retry admission must not rebuild or hash the canonical batch"
        );
        assert_eq!(
            manifest_canonicalizations(),
            0,
            "a retry admission must not re-canonicalize the manifest"
        );
        assert_eq!(
            recovered_generation_enumerations(),
            1,
            "the whole retry admission streams the rows exactly once, at the reopen proof"
        );
        owner.close().unwrap();
    }

    #[test]
    fn second_generation_apply_writes_instead_of_reseating_prior() {
        let temp = TempDir::new().unwrap();
        let (owner, database, manifest_a) = large_persistent_generation(&temp, "two-generations");
        let mut entities_b = manifest_a.entities.clone();
        entities_b.push(
            GraphEntity::new(
                GraphEntityId::new("entity:only-in-b").unwrap(),
                BTreeSet::new(),
                BTreeMap::new(),
            )
            .unwrap(),
        );
        let manifest_b = GraphGenerationManifest::new(
            manifest_a.projection.clone(),
            GraphGenerationId::new("generation-b").unwrap(),
            SourceGeneration::new("source-b").unwrap(),
            GraphWatermark::new("watermark-b").unwrap(),
            vec![],
            entities_b,
            vec![],
        )
        .unwrap();

        reset_batch_canonicalizations();
        let commit_b = database
            .apply_generation_unverified(&manifest_b, &|| Ok(()))
            .unwrap();
        assert_eq!(
            batch_canonicalizations(),
            3,
            "a different generation must stage two bounded pages and its final metadata bind"
        );
        assert_eq!(commit_b.source_generation.as_str(), "source-b");
        assert_eq!(commit_b.watermark.as_str(), "watermark-b");

        // B's rows are really stored: the close/reopen recovered-digest
        // proof over B's generation (including the entity A never had)
        // seats B's sealed digest.
        let sealed_b = sealed_digest(&manifest_b);
        let (reopened_b, recovered_b) = database
            .reopen_and_verify_existing_generation(&manifest_b, &sealed_b, &|| Ok(()))
            .unwrap();
        assert_eq!(recovered_b, sealed_b);
        assert_eq!(reopened_b.source_generation.as_str(), "source-b");

        // A same-generation retry of A still takes the cheap re-seat.
        reset_batch_canonicalizations();
        let retried_a = database
            .apply_generation_unverified(&manifest_a, &|| Ok(()))
            .unwrap();
        assert_eq!(batch_canonicalizations(), 0);
        assert_eq!(retried_a.source_generation.as_str(), "source-large");
        assert_ne!(commit_b.digest, retried_a.digest);
        assert_ne!(
            commit_b.source_generation, retried_a.source_generation,
            "generation B's commit must never alias generation A's"
        );
        owner.close().unwrap();
    }

    #[test]
    fn divergent_identity_apply_falls_through_to_write() {
        let temp = TempDir::new().unwrap();
        let (_owner, database) = persistent_database(&temp);
        database
            .apply_generation_unverified(&manifest("source:one", "watermark:one"), &|| Ok(()))
            .unwrap();

        // Same projection and generation id (same physical namespace), but a
        // different stored identity: the re-seat bind must refuse the cheap
        // return and fall through to a real apply instead of fail-closing
        // the apply with a verification mismatch.
        let divergent = manifest("source:two", "watermark:two");
        reset_batch_canonicalizations();
        let commit = database
            .apply_generation_unverified(&divergent, &|| Ok(()))
            .unwrap();
        assert_eq!(
            batch_canonicalizations(),
            1,
            "a divergent identity must write, not re-seat"
        );
        assert_eq!(commit.source_generation.as_str(), "source:two");
        assert_eq!(commit.watermark.as_str(), "watermark:two");

        // Retrying the now-stored identity takes the cheap re-seat.
        reset_batch_canonicalizations();
        let retried = database
            .apply_generation_unverified(&divergent, &|| Ok(()))
            .unwrap();
        assert_eq!(batch_canonicalizations(), 0);
        assert_eq!(retried.source_generation.as_str(), "source:two");
    }

    #[test]
    fn generation_stage_pages_bound_mutations_and_live_property_bytes() {
        let property = "x".repeat(1024 * 1024);
        let manifest = GraphGenerationManifest::new(
            GraphProjectionIdentity::new(
                GraphNamespace::new("bounded-stage").unwrap(),
                GraphProjectionId::new("properties").unwrap(),
            ),
            GraphGenerationId::new("generation-bounded").unwrap(),
            SourceGeneration::new("source-bounded").unwrap(),
            GraphWatermark::new("watermark-bounded").unwrap(),
            vec![],
            (0..33)
                .map(|index| {
                    GraphEntity::new(
                        GraphEntityId::new(format!("entity:{index:02}")).unwrap(),
                        BTreeSet::new(),
                        BTreeMap::from([(
                            GraphPropertyName::new("payload").unwrap(),
                            GraphProperty::String(property.clone()),
                        )]),
                    )
                    .unwrap()
                })
                .collect(),
            vec![],
        )
        .unwrap();

        let pages = generation_stage_pages(&manifest).unwrap();

        assert!(
            pages.len() > 1,
            "the property-byte ceiling must split this input"
        );
        assert!(pages.iter().all(|page| {
            page.mutation_count() <= MAX_VERIFIED_GENERATION_BATCH_MUTATIONS
                && page.live_bytes() <= MAX_VERIFIED_GENERATION_BATCH_LIVE_BYTES
        }));
        assert_eq!(
            pages
                .iter()
                .map(|page| page.mutation_count())
                .sum::<usize>(),
            manifest.entities.len() + manifest.relations.len()
        );
    }

    #[test]
    fn interruption_between_pages_is_hidden_and_exact_retry_resumes() {
        let manifest = large_manifest("interrupted-stage-resume");
        let locator =
            GenerationLocator::new(manifest.projection.clone(), manifest.generation.clone());

        // Rebuild this generation in a fresh database and cancel as soon as
        // the first native receipt becomes readable. The transaction itself
        // never observes cancellation after commit; the next explicit page
        // boundary does, leaving an exact durable resume point.
        let second_temp = TempDir::new().unwrap();
        let (second_owner, second_database) = persistent_database(&second_temp);
        let pages = generation_stage_pages(&manifest).unwrap();
        let sealed = sealed_digest(&manifest);
        let physical_namespace = manifest.physical_namespace().unwrap();
        let (first_page_key, _) =
            super::generation_stage_page_receipt(&manifest, &sealed, &pages[0]).unwrap();
        let cancel_after_first_page = || {
            let Ok(database_guard) = second_database.inner.database.try_read() else {
                return Ok(());
            };
            let Some(database) = database_guard.as_ref() else {
                return Err(GraphDbError::Closed);
            };
            if crate::state::publication(database, &physical_namespace, &first_page_key)?.is_some()
            {
                Err(GraphDbError::Cancelled)
            } else {
                Ok(())
            }
        };
        assert_eq!(
            second_database.apply_generation_unverified_with_digest(
                &manifest,
                &sealed,
                &cancel_after_first_page,
            ),
            Err(GraphDbError::Cancelled)
        );

        assert!(
            second_database
                .verified_generation(&locator)
                .unwrap()
                .is_none()
        );
        assert!(
            !second_database
                .inner
                .verified_generations
                .read()
                .unwrap()
                .stored
                .contains_key(&locator),
            "a partial physical stage must not become serveable or retained as complete"
        );

        reset_batch_canonicalizations();
        let resumed = second_database
            .apply_generation_unverified_with_digest(&manifest, &sealed, &|| Ok(()))
            .unwrap();
        assert_eq!(
            batch_canonicalizations(),
            2,
            "the exact first-page receipt must skip rebuilding that page on resume"
        );
        let (_, recovered) = second_database
            .reopen_and_verify_existing_generation(&manifest, &sealed, &|| Ok(()))
            .unwrap();
        assert_eq!(
            recovered, sealed,
            "the resumed rows must prove the sealed digest"
        );

        reset_batch_canonicalizations();
        let exact = second_database
            .apply_generation_unverified_with_digest(&manifest, &sealed, &|| Ok(()))
            .unwrap();
        assert_eq!(exact.sequence, resumed.sequence);
        assert_eq!(batch_canonicalizations(), 0);
        second_owner.close().unwrap();
    }
}
