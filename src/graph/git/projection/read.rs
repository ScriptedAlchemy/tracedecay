use super::*;

impl GitHealthProjectionStoreV1 {
    pub(crate) fn read(
        &self,
        binding: &GitHealthProjectionBindingV1,
        cancellation: &CancellationToken,
    ) -> GitHealthProjectionAvailabilityV1 {
        self.read_inner(binding, cancellation)
            .unwrap_or_else(|error| GitHealthProjectionAvailabilityV1::Unavailable {
                reason: error.unavailable_reason(),
            })
    }

    fn read_inner(
        &self,
        binding: &GitHealthProjectionBindingV1,
        cancellation: &CancellationToken,
    ) -> Result<GitHealthProjectionAvailabilityV1, GitHealthProjectionError> {
        cancellation_checkpoint(cancellation)?;
        let graph_cancellation: Arc<dyn GraphCancellation> =
            Arc::new(TokenCancellation(cancellation.clone()));
        let active_namespace = namespace(binding)?;
        let staging_namespace = staging_namespace(binding)?;
        let ready = self.read_state::<ReadyStateV1>(
            binding,
            &active_namespace,
            READY_ENTITY,
            true,
            Arc::clone(&graph_cancellation),
        )?;
        let working = self.read_state::<WorkingStateV1>(
            binding,
            &staging_namespace,
            WORKING_ENTITY,
            true,
            Arc::clone(&graph_cancellation),
        )?;
        if let Some(ready) = ready.as_ref() {
            self.authenticate_projection_commit(
                &active_namespace,
                &ready.source,
                ready.counters.batches_completed,
                Some(
                    ready
                        .counters
                        .commits_projected
                        .saturating_add(ready.counters.unique_paths)
                        .saturating_add(1),
                ),
                Arc::clone(&graph_cancellation),
            )?;
        }
        if let Some(working) = working.as_ref() {
            self.authenticate_projection_commit(
                &staging_namespace,
                &working.target,
                working.counters.batches_completed,
                working.complete.then(|| {
                    working
                        .counters
                        .commits_projected
                        .saturating_add(working.counters.unique_paths)
                        .saturating_add(1)
                }),
                Arc::clone(&graph_cancellation),
            )?;
        }
        let snapshot = ready
            .as_ref()
            .map(|ready| {
                self.authenticated_snapshot(
                    &active_namespace,
                    ready,
                    Arc::clone(&graph_cancellation),
                )
            })
            .transpose()?;
        if let Some(working) = working.as_ref()
            && ready
                .as_ref()
                .is_none_or(|ready| ready.source != working.target)
        {
            return Ok(snapshot.map_or_else(
                || GitHealthProjectionAvailabilityV1::Warming {
                    target: Some(working.target.clone()),
                },
                |snapshot| GitHealthProjectionAvailabilityV1::Refreshing {
                    snapshot,
                    target: working.target.clone(),
                },
            ));
        }
        let Some(snapshot) = snapshot else {
            return Ok(GitHealthProjectionAvailabilityV1::Warming {
                target: working.map(|working| working.target),
            });
        };
        if snapshot.source.binding == *binding {
            Ok(GitHealthProjectionAvailabilityV1::Ready { snapshot })
        } else {
            Ok(GitHealthProjectionAvailabilityV1::Stale {
                snapshot,
                reason: GitHealthProjectionUnavailableReasonV1::ScopeDrift,
            })
        }
    }

    fn authenticated_snapshot(
        &self,
        active_namespace: &tracedecay_graph_db::GraphNamespace,
        ready: &ReadyStateV1,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<GitHealthProjectionSnapshotV1, GitHealthProjectionError> {
        let entities = self.projection_entities(active_namespace, cancellation)?;
        authenticate_snapshot_entities(&entities, &ready.source, &ready.counters, READY_ENTITY)?;
        Ok(GitHealthProjectionSnapshotV1 {
            source: ready.source.clone(),
            commits_projected: ready.counters.commits_projected,
            batches_completed: ready.counters.batches_completed,
            churn_entries: ready.counters.unique_paths,
            coverage: ready.counters.coverage.clone(),
        })
    }

    pub(crate) fn read_churn_page(
        &self,
        binding: &GitHealthProjectionBindingV1,
        snapshot: &GitHealthProjectionSnapshotV1,
        after_cursor: Option<&GitHealthProjectionChurnCursorV1>,
        limit: usize,
        cancellation: &CancellationToken,
    ) -> Result<GitHealthProjectionChurnPageV1, GitHealthProjectionError> {
        if !storage_binding_matches(&snapshot.source.binding, binding) {
            return Err(GitHealthProjectionError::ScopeDrift);
        }
        if after_cursor.is_some_and(|cursor| {
            cursor.source != snapshot.source
                || cursor.batches_completed != snapshot.batches_completed
        }) {
            return Err(GitHealthProjectionError::SnapshotChanged);
        }
        cancellation_checkpoint(cancellation)?;
        let graph_cancellation: Arc<dyn GraphCancellation> =
            Arc::new(TokenCancellation(cancellation.clone()));
        let database = self.database.snapshot()?;
        let active_namespace = namespace(binding)?;
        self.authenticate_projection_source(
            &database,
            &active_namespace,
            &snapshot.source,
            snapshot.batches_completed,
            Some(
                snapshot
                    .commits_projected
                    .saturating_add(snapshot.churn_entries)
                    .saturating_add(1),
            ),
            Arc::clone(&graph_cancellation),
        )?;
        let page = database.read_projection(GraphProjectionReadRequest {
            namespace: active_namespace,
            projection: projection()?,
            after_entity: after_cursor
                .map(|cursor| GraphEntityId::new(&cursor.after_entity))
                .transpose()?,
            after_relation: None,
            max_entities: limit.min(PROJECTION_PAGE_SIZE),
            max_relations: 0,
            cancellation: graph_cancellation,
        })?;
        let file_label = std::collections::BTreeSet::from([GraphLabel::new(FILE_LABEL)?]);
        let mut entries = Vec::new();
        for entity in page.entities {
            if entity.labels == file_label {
                let (path, churn) = persistence::file_record_from_entity(&entity)?;
                entries.push(GitHealthProjectionChurnEntryV1 { path, churn });
            }
        }
        Ok(GitHealthProjectionChurnPageV1 {
            entries,
            next_cursor: page
                .next_entity
                .map(|identity| GitHealthProjectionChurnCursorV1 {
                    source: snapshot.source.clone(),
                    batches_completed: snapshot.batches_completed,
                    after_entity: identity.as_str().to_owned(),
                }),
        })
    }

    pub(super) fn authenticate_projection_commit(
        &self,
        projection_namespace: &tracedecay_graph_db::GraphNamespace,
        source: &GitHealthProjectionSourceV1,
        batches_completed: u64,
        expected_entities: Option<usize>,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<(), GitHealthProjectionError> {
        let database = self.database.snapshot()?;
        self.authenticate_projection_source(
            &database,
            projection_namespace,
            source,
            batches_completed,
            expected_entities,
            cancellation,
        )
    }

    fn authenticate_projection_source(
        &self,
        database: &GraphSnapshot,
        projection_namespace: &tracedecay_graph_db::GraphNamespace,
        source: &GitHealthProjectionSourceV1,
        batches_completed: u64,
        expected_entities: Option<usize>,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<(), GitHealthProjectionError> {
        let telemetry = database
            .projection_telemetry(GraphProjectionTelemetryRequest {
                namespace: projection_namespace.clone(),
                projection: projection()?,
                cancellation,
            })?
            .ok_or_else(|| {
                GitHealthProjectionError::Corrupt(
                    "persisted Git health state has no Grafeo projection commit".to_owned(),
                )
            })?;
        let expected_watermark = if batches_completed == 0 {
            format!("{}:initialize", source.projection_generation.as_str())
        } else {
            format!(
                "{}:{}",
                source.projection_generation.as_str(),
                batches_completed
            )
        };
        if telemetry.source_generation.as_str() != source.projection_generation.as_str()
            || telemetry.watermark.as_str() != expected_watermark
            || telemetry.relation_count != 0
            || expected_entities.is_some_and(|expected| {
                u64::try_from(expected).ok() != Some(telemetry.entity_count)
            })
        {
            return Err(GitHealthProjectionError::Corrupt(
                "Grafeo projection generation, watermark, or cardinality does not authenticate persisted Git health state"
                    .to_owned(),
            ));
        }
        Ok(())
    }
}
