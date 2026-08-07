use super::*;

impl CodeGraphEvidenceReader {
    #[cfg(feature = "test-helpers")]
    pub fn new(
        generation: CodeGenerationId,
        repository_id: Option<RepositoryId>,
        freshness: SourceFreshness,
        edges: &[CanonicalRelationEdgeV1],
        chunks: &[CodeSearchChunkV1],
    ) -> Result<Self, CodeGraphProjectionError> {
        Self::from_in_memory(generation, repository_id, freshness, edges, chunks)
    }

    #[cfg(feature = "eval-helpers")]
    pub fn new_for_evaluation(
        generation: CodeGenerationId,
        repository_id: Option<RepositoryId>,
        freshness: SourceFreshness,
        edges: &[CanonicalRelationEdgeV1],
        chunks: &[CodeSearchChunkV1],
    ) -> Result<Self, CodeGraphProjectionError> {
        Self::from_in_memory(generation, repository_id, freshness, edges, chunks)
    }

    #[cfg(any(feature = "test-helpers", feature = "eval-helpers"))]
    fn from_in_memory(
        generation: CodeGenerationId,
        repository_id: Option<RepositoryId>,
        freshness: SourceFreshness,
        edges: &[CanonicalRelationEdgeV1],
        chunks: &[CodeSearchChunkV1],
    ) -> Result<Self, CodeGraphProjectionError> {
        let cancellation: Arc<dyn GraphCancellation> = Arc::new(NeverCancelled);
        let application_cancellation = CancellationSignal::active("code-graph-memory")
            .map_err(|error| CodeGraphProjectionError::Contract(error.to_string()))?;
        let store = InMemoryCodeGraphProjectionBuilder::memory(&application_cancellation)?;
        store.publish_with_cancellation(&generation, edges, chunks, Arc::clone(&cancellation))?;
        validate_reader_metadata(repository_id.as_ref(), &freshness)?;
        let snapshot = store
            .snapshot
            .read()
            .map_err(|_| {
                CodeGraphProjectionError::Unavailable(
                    "code graph verified snapshot lock is poisoned".to_owned(),
                )
            })?
            .clone()
            .ok_or_else(|| {
                CodeGraphProjectionError::Unavailable(
                    "code graph generation is not published".to_owned(),
                )
            })?;
        let current =
            read_current_generation(&snapshot, &store.projection, Arc::clone(&cancellation))?;
        Ok(Self {
            generation,
            repository_id,
            freshness,
            projection: store.projection.clone(),
            snapshot,
            projection_node_count: current.projection_node_count,
            cancellation,
        })
    }

    pub fn generation(&self) -> &CodeGenerationId {
        &self.generation
    }

    pub fn repository_id(&self) -> Option<&RepositoryId> {
        self.repository_id.as_ref()
    }

    pub fn freshness(&self) -> &SourceFreshness {
        &self.freshness
    }

    pub fn traverse(
        &self,
        generation: &CodeGenerationId,
        seed_symbols: &[SymbolOccurrenceId],
        edge_kinds: &[RelationEdgeKindV1],
        max_depth: u32,
        request_cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<CodeGraphTraversalBatchV1, CodeGraphProjectionError> {
        let cancellation: Arc<dyn GraphCancellation> = Arc::new(CodeGraphReadCancellation {
            lifecycle: Arc::clone(&self.cancellation),
            request: request_cancellation,
        });
        if generation != &self.generation {
            return Err(CodeGraphProjectionError::GenerationMismatch);
        }
        if cancellation.is_cancelled() {
            return Err(CodeGraphProjectionError::Cancelled);
        }
        if max_depth == 0 {
            return Err(CodeGraphProjectionError::Contract(
                "code graph traversal depth must be positive".to_owned(),
            ));
        }
        let admitted_kinds: BTreeSet<_> = edge_kinds.iter().copied().collect();
        let mut best_by_target = BTreeMap::<SymbolOccurrenceId, CodeGraphPathCandidateV1>::new();
        let mut coverage = CodeGraphTraversalCoverageV1::default();
        for seed in seed_symbols {
            seed.validate()
                .map_err(|error| CodeGraphProjectionError::Contract(error.to_string()))?;
            let Some(seed_record) = self.symbol_record(seed, Arc::clone(&cancellation))? else {
                continue;
            };
            if seed_record.binding.is_none() {
                return Err(CodeGraphProjectionError::Corrupt(
                    "code graph authorized an unbound seed".to_owned(),
                ));
            }
            let adjacency = self.adjacency(seed, max_depth, Arc::clone(&cancellation))?;
            self.traverse_seed(
                seed,
                max_depth,
                &admitted_kinds,
                &adjacency,
                &cancellation,
                &mut coverage,
                &mut best_by_target,
            )?;
        }
        let mut candidates: Vec<_> = best_by_target.into_values().collect();
        candidates.sort_by(|left, right| {
            right
                .score_micros
                .cmp(&left.score_micros)
                .then_with(|| left.target.cmp(&right.target))
        });
        coverage.eligible = candidates.len() as u64;
        Ok(CodeGraphTraversalBatchV1 {
            candidates,
            coverage,
        })
    }

    fn adjacency(
        &self,
        seed: &SymbolOccurrenceId,
        max_depth: u32,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<BTreeMap<SymbolOccurrenceId, Vec<CanonicalRelationEdgeV1>>, CodeGraphProjectionError>
    {
        let graph_depth = usize::try_from(max_depth)
            .ok()
            .and_then(|depth| depth.checked_mul(2))
            .ok_or_else(|| {
                CodeGraphProjectionError::Contract(
                    "code graph traversal depth overflowed".to_owned(),
                )
            })?;
        let result = self.snapshot.traverse(TraversalRequest {
            namespace: self.projection.namespace.clone(),
            start: symbol_entity_id(seed)?,
            relation_kinds: BTreeSet::new(),
            direction: GraphTraversalDirection::Outgoing,
            max_depth: graph_depth,
            max_visits: self.projection_node_count,
            max_results: self.projection_node_count,
            cancellation: Arc::clone(&cancellation),
        })?;
        let mut adjacency = BTreeMap::<SymbolOccurrenceId, Vec<CanonicalRelationEdgeV1>>::new();
        for visit in result.visits {
            if visit.depth % 2 == 0 {
                continue;
            }
            let entity = self
                .snapshot
                .entity(&visit.entity, Arc::clone(&cancellation))?
                .ok_or_else(|| {
                    CodeGraphProjectionError::Corrupt(
                        "graph traversal referenced a missing edge entity".to_owned(),
                    )
                })?;
            if !has_label(&entity, EDGE_LABEL) {
                return Err(CodeGraphProjectionError::Corrupt(
                    "code graph alternation contains a non-edge entity".to_owned(),
                ));
            }
            let edge: CanonicalRelationEdgeV1 =
                deserialize_property(&entity, EDGE_RECORD_PROPERTY)?;
            validate_edge(&edge)?;
            if edge_entity_id(&edge)? != entity.identity {
                return Err(CodeGraphProjectionError::Corrupt(
                    "code graph edge identity does not match its payload".to_owned(),
                ));
            }
            adjacency
                .entry(edge.from_occurrence.clone())
                .or_default()
                .push(edge);
        }
        for edges in adjacency.values_mut() {
            edges.sort_by(compare_edges);
            edges.dedup();
        }
        Ok(adjacency)
    }

    #[allow(clippy::too_many_arguments)]
    fn traverse_seed(
        &self,
        seed: &SymbolOccurrenceId,
        max_depth: u32,
        edge_kinds: &BTreeSet<RelationEdgeKindV1>,
        adjacency: &BTreeMap<SymbolOccurrenceId, Vec<CanonicalRelationEdgeV1>>,
        cancellation: &Arc<dyn GraphCancellation>,
        coverage: &mut CodeGraphTraversalCoverageV1,
        best_by_target: &mut BTreeMap<SymbolOccurrenceId, CodeGraphPathCandidateV1>,
    ) -> Result<(), CodeGraphProjectionError> {
        let mut frontiers = BTreeMap::from([(seed.clone(), vec![FrontierPath::seed()])]);
        let mut depths = BTreeMap::from([(seed.clone(), 0_usize)]);
        let mut queue = VecDeque::from([seed.clone()]);
        while let Some(source) = queue.pop_front() {
            if cancellation.is_cancelled() {
                return Err(CodeGraphProjectionError::Cancelled);
            }
            let depth = depths[&source];
            if depth >= max_depth as usize {
                continue;
            }
            let source_record = self
                .symbol_record(&source, Arc::clone(cancellation))?
                .ok_or_else(|| {
                    CodeGraphProjectionError::Corrupt(
                        "code graph traversal reached a missing symbol entity".to_owned(),
                    )
                })?;
            if source_record.binding.is_none() {
                continue;
            }
            let prefixes = frontiers.get(&source).cloned().ok_or_else(|| {
                CodeGraphProjectionError::Corrupt(
                    "code graph symbol has no path frontier".to_owned(),
                )
            })?;
            for edge in adjacency.get(&source).into_iter().flatten() {
                coverage.examined = coverage.examined.saturating_add(prefixes.len() as u64);
                if !edge_kinds.contains(&edge.kind) {
                    coverage.excluded = coverage.excluded.saturating_add(prefixes.len() as u64);
                    continue;
                }
                let target_depth = depth + 1;
                if depths
                    .get(&edge.to_occurrence)
                    .is_some_and(|known| *known < target_depth)
                {
                    continue;
                }
                let is_new = !depths.contains_key(&edge.to_occurrence);
                if is_new {
                    depths.insert(edge.to_occurrence.clone(), target_depth);
                }
                for prefix in &prefixes {
                    admit_frontier_path(
                        frontiers.entry(edge.to_occurrence.clone()).or_default(),
                        prefix.extended(edge),
                    );
                }
                if is_new {
                    queue.push_back(edge.to_occurrence.clone());
                }
            }
        }
        for (target, paths) in frontiers {
            if paths.first().is_none_or(|path| path.segments.is_empty()) {
                continue;
            }
            let record = self
                .symbol_record(&target, Arc::clone(cancellation))?
                .ok_or_else(|| {
                    CodeGraphProjectionError::Corrupt(
                        "code graph path targets a missing symbol entity".to_owned(),
                    )
                })?;
            let Some(binding) = record.binding else {
                coverage.unknown = coverage.unknown.saturating_add(paths.len() as u64);
                continue;
            };
            let best = best_frontier_path(paths)?;
            let weakest_authority = best.weakest.ok_or_else(|| {
                CodeGraphProjectionError::Corrupt("code graph emitted an empty path".to_owned())
            })?;
            let candidate = CodeGraphPathCandidateV1 {
                target: target.clone(),
                binding,
                path: best.segments,
                weakest_authority,
                score_micros: best.score,
            };
            match best_by_target.entry(target) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(candidate);
                }
                std::collections::btree_map::Entry::Occupied(mut entry) => {
                    let current = entry.get();
                    if candidate.score_micros > current.score_micros
                        || (candidate.score_micros == current.score_micros
                            && compare_paths(&candidate.path, &current.path).is_lt())
                    {
                        entry.insert(candidate);
                    }
                }
            }
        }
        Ok(())
    }

    fn symbol_record(
        &self,
        occurrence: &SymbolOccurrenceId,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Option<SymbolRecordV1>, CodeGraphProjectionError> {
        let identity = symbol_entity_id(occurrence)?;
        let reference = GraphEntityRef::new(self.projection.clone(), identity.clone());
        let Some(entity) = self.snapshot.entity(&reference, cancellation)? else {
            return Ok(None);
        };
        if !has_label(&entity, SYMBOL_LABEL) {
            return Err(CodeGraphProjectionError::Corrupt(
                "code graph symbol identity has the wrong label".to_owned(),
            ));
        }
        let record: SymbolRecordV1 = deserialize_property(&entity, SYMBOL_RECORD_PROPERTY)?;
        validate_symbol_record(&record)?;
        if record.occurrence != *occurrence || symbol_entity_id(&record.occurrence)? != identity {
            return Err(CodeGraphProjectionError::Corrupt(
                "code graph symbol identity does not match its payload".to_owned(),
            ));
        }
        Ok(Some(record))
    }
}

struct CodeGraphReadCancellation {
    lifecycle: Arc<dyn GraphCancellation>,
    request: Arc<dyn GraphCancellation>,
}

impl GraphCancellation for CodeGraphReadCancellation {
    fn is_cancelled(&self) -> bool {
        self.lifecycle.is_cancelled() || self.request.is_cancelled()
    }
}
