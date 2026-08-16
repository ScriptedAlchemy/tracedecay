use std::collections::{BTreeMap, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use grafeo_core::graph::Direction;
use tracedecay_store::runtime::GraphRecoveredGenerationDigestV1;

use crate::generation::verify_recovered_generation;
use crate::lease::{
    GenerationLocator, VerifiedGenerationLease, VerifiedGraphSnapshot, VerifiedTraversalResult,
    VerifiedTraversalVisit,
};
use crate::recovery::{
    checkpoint_recovered_database, is_database_fault, open_recovered_database,
    quarantine_transition_failure, requarantine_after_failed_checkpoint_verification,
    set_projection_quarantine,
};
use crate::schema::{NAMESPACE_PROPERTY, relation_kind_from_type, required_string};
use crate::state::{
    latest_projection, load_entity_by_node, load_relation, load_relation_by_edge,
    projection_entities_checked, projection_relations_checked,
};
use crate::{
    GraphBudgetKind, GraphCancellation, GraphCommit, GraphDb, GraphDbError, GraphEntityRef,
    GraphGenerationManifest, GraphGenerationRelation, GraphMutation, GraphNamespace,
    GraphRelationRef, GraphTraversalDirection, GraphWriteBatch, TraversalRequest, mutation,
};

impl GraphDb {
    #[cfg(any(feature = "test-helpers", feature = "eval-helpers"))]
    pub(crate) fn verify_generation_in_place(
        &self,
        manifest: &GraphGenerationManifest,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<GraphRecoveredGenerationDigestV1, GraphDbError> {
        check()?;
        let guard = self.read_guard()?;
        let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
        verify_recovered_generation(database, manifest, None, check)
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
        let recovered = verify_recovered_generation(database, manifest, Some(expected), check)?;
        Ok((commit, recovered))
    }

    pub(crate) fn apply_generation_unverified(
        &self,
        manifest: &GraphGenerationManifest,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<GraphCommit, GraphDbError> {
        check()?;
        manifest.validate_checked(check)?;
        let physical_namespace = manifest.physical_namespace()?;
        let dependency_namespaces = self.require_exact_dependencies(manifest)?;
        let mut endpoint_namespaces = mutation::RelationEndpointNamespaces::new();
        let mut mutations = Vec::with_capacity(
            manifest
                .entities
                .len()
                .checked_add(manifest.relations.len())
                .ok_or_else(|| GraphDbError::invalid("graph generation mutation count overflow"))?,
        );
        for entity in &manifest.entities {
            check()?;
            mutations.push(GraphMutation::UpsertEntity(entity.clone()));
        }
        for relation in &manifest.relations {
            check()?;
            endpoint_namespaces.insert(
                relation.identity.clone(),
                (
                    endpoint_namespace(
                        manifest,
                        &physical_namespace,
                        &dependency_namespaces,
                        &relation.from.projection,
                    )?,
                    endpoint_namespace(
                        manifest,
                        &physical_namespace,
                        &dependency_namespaces,
                        &relation.to.projection,
                    )?,
                ),
            );
            mutations.push(GraphMutation::UpsertRelation(relation.storage_relation()?));
        }
        let batch = GraphWriteBatch::new_canonical_checked(
            physical_namespace.clone(),
            manifest.projection.projection.clone(),
            manifest.source_generation.clone(),
            manifest.watermark.clone(),
            mutations,
            check,
        )?;
        let digest = batch.canonical_digest_checked(check)?;
        let _snapshot_gate = self.inner.snapshot_gate.write();
        let guard = self.write_guard()?;
        let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
        if let Some(existing) = latest_projection(
            database,
            &physical_namespace,
            &manifest.projection.projection,
        )? {
            let locator =
                GenerationLocator::new(manifest.projection.clone(), manifest.generation.clone());
            let was_collected = self
                .inner
                .verified_generations
                .read()
                .map_err(|_| {
                    GraphDbError::unavailable("verified graph generation state lock is poisoned")
                })?
                .collected
                .contains(&locator);
            if !was_collected {
                verify_recovered_generation(database, manifest, None, check)?;
                let mut verified = self.inner.verified_generations.write().map_err(|_| {
                    GraphDbError::unavailable("verified graph generation state lock is poisoned")
                })?;
                verified
                    .stored
                    .insert(locator, generation_dependency_locators(manifest));
                return Ok(existing.commit);
            }
            let mut verified = self.inner.verified_generations.write().map_err(|_| {
                GraphDbError::unavailable("verified graph generation state lock is poisoned")
            })?;
            verified.collected.remove(&locator);
        }
        let mut state = self.state_write_guard()?;
        let commit = self.apply_locked(
            database,
            &mut state,
            batch,
            mutation::CommitMetadata {
                digest,
                generation_dependency_digest: Some(manifest.dependency_closure_digest(check)?),
                publication_record: None,
            },
            &endpoint_namespaces,
            check,
        )?;
        {
            let mut verified = self.inner.verified_generations.write().map_err(|_| {
                GraphDbError::unavailable("verified graph generation state lock is poisoned")
            })?;
            verified.stored.insert(
                GenerationLocator::new(manifest.projection.clone(), manifest.generation.clone()),
                generation_dependency_locators(manifest),
            );
        }
        check()?;
        Ok(commit)
    }

    pub(crate) fn reopen_and_verify_generation(
        &self,
        manifest: &GraphGenerationManifest,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<(GraphCommit, GraphRecoveredGenerationDigestV1), GraphDbError> {
        self.reopen_and_verify_generation_digest(manifest, None, check)
    }

    pub(crate) fn reopen_and_verify_existing_generation(
        &self,
        manifest: &GraphGenerationManifest,
        expected: &GraphRecoveredGenerationDigestV1,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<(GraphCommit, GraphRecoveredGenerationDigestV1), GraphDbError> {
        self.reopen_and_verify_generation_digest(manifest, Some(expected), check)
    }

    fn reopen_and_verify_generation_digest(
        &self,
        manifest: &GraphGenerationManifest,
        expected: Option<&GraphRecoveredGenerationDigestV1>,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<(GraphCommit, GraphRecoveredGenerationDigestV1), GraphDbError> {
        check()?;
        let physical_namespace = manifest.physical_namespace()?;
        let projection = manifest.projection.projection.clone();
        let quarantine_key = (physical_namespace.clone(), projection.clone());
        let _snapshot_gate = self.inner.snapshot_gate.write();
        let mut database_guard = self
            .inner
            .database
            .write()
            .map_err(|_| GraphDbError::unavailable("graph database write lock is poisoned"))?;
        self.ensure_available()?;
        check()?;
        let mut state_guard = self.state_write_guard()?;
        let mut quarantined_guard = self
            .inner
            .quarantined_projections
            .write()
            .map_err(|_| GraphDbError::unavailable("graph quarantine lock is poisoned"))?;
        let reopen = self.inner.reopen.clone().ok_or_else(|| {
            GraphDbError::invalid(
                "recovered generation verification requires a persistent graph database",
            )
        })?;
        let database = database_guard.take().ok_or(GraphDbError::Closed)?;
        if let Err(error) = database.close() {
            self.inner.poisoned.store(true, Ordering::Release);
            return Err(GraphDbError::DurabilityUncertain {
                message: format!(
                    "Grafeo close failed before recovered generation verification: {error}"
                ),
            });
        }
        let (mut recovered, mut recovered_state, mut quarantined) =
            match open_recovered_database(&reopen) {
                Ok(recovered) => recovered,
                Err(error) => {
                    self.inner.poisoned.store(true, Ordering::Release);
                    return Err(error);
                }
            };
        if let Err(error) = check() {
            *state_guard = recovered_state;
            *quarantined_guard = quarantined;
            *database_guard = Some(recovered);
            return Err(error);
        }
        let commit = match latest_projection(&recovered, &physical_namespace, &projection) {
            Ok(Some(projection)) => projection.commit,
            Ok(None) => {
                let error = GraphDbError::GenerationMismatch {
                    namespace: manifest.projection.namespace.to_string(),
                    projection: projection.to_string(),
                    generation: manifest.generation.to_string(),
                    message: "recovered generation is missing".to_owned(),
                };
                *state_guard = recovered_state;
                *quarantined_guard = quarantined;
                *database_guard = Some(recovered);
                drop(quarantined_guard);
                drop(state_guard);
                drop(database_guard);
                drop(_snapshot_gate);
                self.quarantine_generation(manifest)?;
                return Err(error);
            }
            Err(error) => {
                if is_database_fault(&error) {
                    self.inner.poisoned.store(true, Ordering::Release);
                }
                *state_guard = recovered_state;
                *quarantined_guard = quarantined;
                *database_guard = Some(recovered);
                return Err(error);
            }
        };
        let mut verified = match verify_recovered_generation(&recovered, manifest, expected, check)
        {
            Ok(verified) => verified,
            Err(error @ GraphDbError::GenerationMismatch { .. }) => {
                *state_guard = recovered_state;
                *quarantined_guard = quarantined;
                *database_guard = Some(recovered);
                drop(quarantined_guard);
                drop(state_guard);
                drop(database_guard);
                drop(_snapshot_gate);
                self.quarantine_generation(manifest)?;
                return Err(error);
            }
            Err(error) => {
                if is_database_fault(&error) {
                    self.inner.poisoned.store(true, Ordering::Release);
                }
                *state_guard = recovered_state;
                *quarantined_guard = quarantined;
                *database_guard = Some(recovered);
                return Err(error);
            }
        };
        if quarantined.contains(&quarantine_key) {
            if let Err(error) =
                set_projection_quarantine(&recovered, &physical_namespace, &projection, false)
                    .and_then(|()| crate::runtime::sync_wal(&recovered))
            {
                self.inner.poisoned.store(true, Ordering::Release);
                *state_guard = recovered_state;
                *quarantined_guard = quarantined;
                *database_guard = Some(recovered);
                return Err(quarantine_transition_failure(
                    "clear recovered generation quarantine",
                    error,
                ));
            }
            (recovered, recovered_state, quarantined) =
                match checkpoint_recovered_database(recovered, &reopen) {
                    Ok(recovered) => recovered,
                    Err(error) => {
                        self.inner.poisoned.store(true, Ordering::Release);
                        return Err(error);
                    }
                };
            if quarantined.contains(&quarantine_key) {
                self.inner.poisoned.store(true, Ordering::Release);
                *state_guard = recovered_state;
                *quarantined_guard = quarantined;
                *database_guard = Some(recovered);
                return Err(GraphDbError::DurabilityUncertain {
                    message: "recovered generation quarantine remained after checkpoint".to_owned(),
                });
            }
            verified = match verify_recovered_generation(&recovered, manifest, expected, check) {
                Ok(verified) => verified,
                Err(error) => {
                    (recovered, recovered_state, quarantined) =
                        match requarantine_after_failed_checkpoint_verification(
                            recovered,
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
                    return Err(error);
                }
            };
        }
        *state_guard = recovered_state;
        *quarantined_guard = quarantined;
        *database_guard = Some(recovered);
        Ok((commit, verified))
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
        let _snapshot_gate = self.inner.snapshot_gate.write();
        let guard = self.write_guard()?;
        let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
        let relation_ids = projection_relations_checked(database, &namespace, &projection, check)?
            .into_iter()
            .map(|relation| relation.relation.identity)
            .collect::<std::collections::BTreeSet<_>>();
        let entity_ids = projection_entities_checked(database, &namespace, &projection, check)?
            .into_iter()
            .map(|entity| entity.entity.identity)
            .collect::<std::collections::BTreeSet<_>>();
        let mut mutations = Vec::with_capacity(
            relation_ids
                .len()
                .checked_add(entity_ids.len())
                .ok_or_else(|| GraphDbError::invalid("graph deletion mutation count overflow"))?,
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
        let mut state = self.state_write_guard()?;
        self.apply_locked(
            database,
            &mut state,
            batch,
            mutation::CommitMetadata::for_digest(digest),
            &mutation::RelationEndpointNamespaces::new(),
            check,
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
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::Arc;

    use tempfile::TempDir;

    use crate::generation::{
        recovered_generation_enumerations, reset_recovered_generation_enumerations,
    };
    use crate::{
        GraphDbError, GraphDbLocation, GraphDbOpenOptions, GraphDbOwner, GraphDurability,
        GraphEntity, GraphEntityId, GraphFormatVersion, GraphGenerationDependency,
        GraphGenerationId, GraphGenerationManifest, GraphIdempotencyKey, GraphNamespace,
        GraphProjectionId, GraphProjectionIdentity, GraphWatermark, NeverCancelled,
        SourceGeneration,
    };

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

    #[test]
    fn recovered_generation_rejects_stale_persisted_source_generation() {
        let temp = TempDir::new().unwrap();
        let (_owner, database) = persistent_database(&temp);
        database
            .apply_generation_unverified(&manifest("source:old", "watermark:one"), &|| Ok(()))
            .unwrap();

        assert!(matches!(
            database
                .reopen_and_verify_generation(&manifest("source:new", "watermark:one"), &|| Ok(())),
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

        assert!(matches!(
            database
                .reopen_and_verify_generation(&manifest("source:one", "watermark:new"), &|| Ok(())),
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
            database.reopen_and_verify_generation(&changed, &|| Ok(())),
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

        reset_recovered_generation_enumerations();
        database
            .reopen_and_verify_generation(&manifest, &|| Ok(()))
            .unwrap();

        assert_eq!(recovered_generation_enumerations(), 1);
        owner.close().unwrap();
    }
}
