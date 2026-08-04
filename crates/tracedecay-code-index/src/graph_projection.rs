//! Durable code-graph projection over the opaque graph database boundary.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tracedecay_application::CancellationSignal;
use tracedecay_domain::{
    CanonicalRelationEdgeV1, CodeGenerationId, CodeSearchChunkId, CodeSearchChunkV1,
    EdgeAuthorityV1, FileOccurrenceId, LanguageDescriptorRevision, RelationEdgeKindV1,
    RepositoryId, SourceFreshness, SymbolOccurrenceId,
};
use tracedecay_graph_db::{
    GraphCancellation, GraphDb, GraphDbError, GraphDbLocation, GraphDbOpenOptions, GraphDurability,
    GraphEntity, GraphEntityId, GraphFormatVersion, GraphLabel, GraphNamespace, GraphProjectionId,
    GraphProperty, GraphPropertyName, GraphRelation, GraphRelationId, GraphRelationKind,
    GraphSnapshot, GraphWatermark, NeverCancelled, ProjectionReplacement, SourceGeneration,
    TraversalRequest,
};

mod traversal;

use self::traversal::{FrontierPath, admit_frontier_path, best_frontier_path, compare_paths};

const GRAPH_FORMAT_VERSION: u32 = 2;
const CODE_NAMESPACE: &str = "code-graph";
const CODE_PROJECTION: &str = "code-generation";
const CURRENT_GENERATION_ENTITY: &str = "code-current-generation";
const CURRENT_GENERATION_PROPERTY: &str = "current-generation";
const PROJECTION_NODE_COUNT_PROPERTY: &str = "projection-node-count";
const SYMBOL_RECORD_PROPERTY: &str = "symbol-record";
const EDGE_RECORD_PROPERTY: &str = "edge-record";
const SYMBOL_LABEL: &str = "CodeSymbol";
const EDGE_LABEL: &str = "CodeRelationEvidence";
const SOURCE_EDGE_KIND: &str = "CodeRelationSource";
const TARGET_EDGE_KIND: &str = "CodeRelationTarget";

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum CodeGraphProjectionError {
    #[error("code graph contract violation: {0}")]
    Contract(String),
    #[error("code graph generation does not match")]
    GenerationMismatch,
    #[error("code graph operation cancelled")]
    Cancelled,
    #[error("code graph operation budget exhausted")]
    BudgetExhausted,
    #[error("code graph database conflict")]
    Conflict,
    #[error("code graph database reset required: {0}")]
    ResetRequired(String),
    #[error("code graph database is corrupt: {0}")]
    Corrupt(String),
    #[error("code graph database is unavailable: {0}")]
    Unavailable(String),
    #[error("code graph database durability is uncertain: {0}")]
    DurabilityUncertain(String),
    #[error("code graph database is closed")]
    Closed,
}

impl From<GraphDbError> for CodeGraphProjectionError {
    fn from(error: GraphDbError) -> Self {
        match error {
            GraphDbError::Cancelled => Self::Cancelled,
            GraphDbError::InvalidRequest { message } => Self::Contract(message),
            GraphDbError::Conflict => Self::Conflict,
            GraphDbError::BudgetExhausted => Self::BudgetExhausted,
            GraphDbError::ResetRequired { message } => Self::ResetRequired(message),
            GraphDbError::Corrupt { message } => Self::Corrupt(message),
            GraphDbError::Unavailable { message } => Self::Unavailable(message),
            GraphDbError::DurabilityUncertain { message } => Self::DurabilityUncertain(message),
            GraphDbError::Closed => Self::Closed,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CodeGraphSymbolBindingV1 {
    pub file: FileOccurrenceId,
    pub chunk: Option<CodeSearchChunkId>,
    pub language_descriptor_revision: LanguageDescriptorRevision,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct SymbolRecordV1 {
    occurrence: SymbolOccurrenceId,
    binding: Option<CodeGraphSymbolBindingV1>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodeGraphPathCandidateV1 {
    pub target: SymbolOccurrenceId,
    pub binding: CodeGraphSymbolBindingV1,
    pub path: Vec<CanonicalRelationEdgeV1>,
    pub weakest_authority: EdgeAuthorityV1,
    pub score_micros: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CodeGraphTraversalCoverageV1 {
    pub examined: u64,
    pub eligible: u64,
    pub excluded: u64,
    pub unknown: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodeGraphTraversalBatchV1 {
    pub candidates: Vec<CodeGraphPathCandidateV1>,
    pub coverage: CodeGraphTraversalCoverageV1,
}

pub trait CodeGraphProjectionPublisher {
    fn publish_code_graph(
        &self,
        generation: &CodeGenerationId,
        edges: &[CanonicalRelationEdgeV1],
        chunks: &[CodeSearchChunkV1],
        cancellation: &CancellationSignal,
    ) -> Result<GraphWatermark, CodeGraphProjectionError>;
}

#[derive(Clone)]
pub struct CodeGraphProjectionStore {
    database: GraphDb,
}

impl fmt::Debug for CodeGraphProjectionStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodeGraphProjectionStore")
            .finish_non_exhaustive()
    }
}

impl CodeGraphProjectionStore {
    pub fn memory(cancellation: &CancellationSignal) -> Result<Self, CodeGraphProjectionError> {
        Self::open_location(
            GraphDbLocation::Memory,
            GraphDurability::Memory,
            application_cancellation(cancellation),
        )
    }

    pub fn open(
        path: &Path,
        cancellation: &CancellationSignal,
    ) -> Result<Self, CodeGraphProjectionError> {
        Self::open_location(
            GraphDbLocation::Persistent(path.to_path_buf()),
            GraphDurability::Sync,
            application_cancellation(cancellation),
        )
    }

    fn open_location(
        location: GraphDbLocation,
        durability: GraphDurability,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Self, CodeGraphProjectionError> {
        let database = GraphDb::open(GraphDbOpenOptions {
            location,
            expected_format: GraphFormatVersion::new(GRAPH_FORMAT_VERSION)?,
            durability,
            cancellation,
        })?;
        Ok(Self { database })
    }

    pub fn evidence_reader(
        &self,
        generation: &CodeGenerationId,
        repository_id: Option<RepositoryId>,
        freshness: SourceFreshness,
        cancellation: &CancellationSignal,
    ) -> Result<CodeGraphEvidenceAdapterV1, CodeGraphProjectionError> {
        generation
            .validate()
            .map_err(|error| CodeGraphProjectionError::Contract(error.to_string()))?;
        validate_reader_metadata(repository_id.as_ref(), &freshness)?;
        let cancellation = application_cancellation(cancellation);
        let snapshot = Arc::new(self.database.snapshot()?);
        let current = read_current_generation(&snapshot, Arc::clone(&cancellation))?;
        if current.generation != *generation {
            return Err(CodeGraphProjectionError::GenerationMismatch);
        }
        Ok(CodeGraphEvidenceAdapterV1 {
            generation: generation.clone(),
            repository_id,
            freshness,
            snapshot,
            projection_node_count: current.projection_node_count,
            cancellation,
        })
    }

    pub fn close(&self) -> Result<(), CodeGraphProjectionError> {
        self.database.close().map_err(Into::into)
    }

    fn publish_with_cancellation(
        &self,
        generation: &CodeGenerationId,
        edges: &[CanonicalRelationEdgeV1],
        chunks: &[CodeSearchChunkV1],
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<GraphWatermark, CodeGraphProjectionError> {
        if cancellation.is_cancelled() {
            return Err(CodeGraphProjectionError::Cancelled);
        }
        let built = build_projection(generation, edges, chunks, Arc::clone(&cancellation))?;
        let watermark = built.watermark.clone();
        self.database.replace_projection(ProjectionReplacement {
            namespace: namespace()?,
            projection: projection()?,
            source_generation: source_generation(generation)?,
            next_watermark: built.watermark,
            entities: built.entities,
            relations: built.relations,
            cancellation,
        })?;
        Ok(watermark)
    }
}

impl CodeGraphProjectionPublisher for CodeGraphProjectionStore {
    fn publish_code_graph(
        &self,
        generation: &CodeGenerationId,
        edges: &[CanonicalRelationEdgeV1],
        chunks: &[CodeSearchChunkV1],
        cancellation: &CancellationSignal,
    ) -> Result<GraphWatermark, CodeGraphProjectionError> {
        self.publish_with_cancellation(
            generation,
            edges,
            chunks,
            application_cancellation(cancellation),
        )
    }
}

#[derive(Clone)]
pub struct CodeGraphEvidenceAdapterV1 {
    generation: CodeGenerationId,
    repository_id: Option<RepositoryId>,
    freshness: SourceFreshness,
    snapshot: Arc<GraphSnapshot>,
    projection_node_count: usize,
    cancellation: Arc<dyn GraphCancellation>,
}

impl fmt::Debug for CodeGraphEvidenceAdapterV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodeGraphEvidenceAdapterV1")
            .field("generation", &self.generation)
            .field("repository_id", &self.repository_id)
            .field("freshness", &self.freshness)
            .field("projection_node_count", &self.projection_node_count)
            .finish_non_exhaustive()
    }
}

impl CodeGraphEvidenceAdapterV1 {
    pub fn new(
        generation: CodeGenerationId,
        repository_id: Option<RepositoryId>,
        freshness: SourceFreshness,
        edges: &[CanonicalRelationEdgeV1],
        chunks: &[CodeSearchChunkV1],
    ) -> Result<Self, CodeGraphProjectionError> {
        let cancellation: Arc<dyn GraphCancellation> = Arc::new(NeverCancelled);
        let store = CodeGraphProjectionStore::open_location(
            GraphDbLocation::Memory,
            GraphDurability::Memory,
            Arc::clone(&cancellation),
        )?;
        store.publish_with_cancellation(&generation, edges, chunks, Arc::clone(&cancellation))?;
        validate_reader_metadata(repository_id.as_ref(), &freshness)?;
        let snapshot = Arc::new(store.database.snapshot()?);
        let current = read_current_generation(&snapshot, Arc::clone(&cancellation))?;
        Ok(Self {
            generation,
            repository_id,
            freshness,
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
    ) -> Result<CodeGraphTraversalBatchV1, CodeGraphProjectionError> {
        if generation != &self.generation {
            return Err(CodeGraphProjectionError::GenerationMismatch);
        }
        if self.cancellation.is_cancelled() {
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
            let Some(seed_record) = self.symbol_record(seed)? else {
                continue;
            };
            if seed_record.binding.is_none() {
                return Err(CodeGraphProjectionError::Corrupt(
                    "code graph authorized an unbound seed".to_owned(),
                ));
            }
            let adjacency = self.adjacency(seed, max_depth)?;
            self.traverse_seed(
                seed,
                max_depth,
                &admitted_kinds,
                &adjacency,
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
            namespace: namespace()?,
            start: symbol_entity_id(seed)?,
            relation_kinds: BTreeSet::new(),
            max_depth: graph_depth,
            max_visits: self.projection_node_count,
            max_results: self.projection_node_count,
            cancellation: Arc::clone(&self.cancellation),
        })?;
        let mut adjacency = BTreeMap::<SymbolOccurrenceId, Vec<CanonicalRelationEdgeV1>>::new();
        for visit in result.visits {
            if visit.depth % 2 == 0 {
                continue;
            }
            let entity = self
                .snapshot
                .entity(&namespace()?, &visit.entity, Arc::clone(&self.cancellation))?
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

    fn traverse_seed(
        &self,
        seed: &SymbolOccurrenceId,
        max_depth: u32,
        edge_kinds: &BTreeSet<RelationEdgeKindV1>,
        adjacency: &BTreeMap<SymbolOccurrenceId, Vec<CanonicalRelationEdgeV1>>,
        coverage: &mut CodeGraphTraversalCoverageV1,
        best_by_target: &mut BTreeMap<SymbolOccurrenceId, CodeGraphPathCandidateV1>,
    ) -> Result<(), CodeGraphProjectionError> {
        let mut frontiers = BTreeMap::from([(seed.clone(), vec![FrontierPath::seed()])]);
        let mut depths = BTreeMap::from([(seed.clone(), 0_usize)]);
        let mut queue = VecDeque::from([seed.clone()]);
        while let Some(source) = queue.pop_front() {
            if self.cancellation.is_cancelled() {
                return Err(CodeGraphProjectionError::Cancelled);
            }
            let depth = depths[&source];
            if depth >= max_depth as usize {
                continue;
            }
            let source_record = self.symbol_record(&source)?.ok_or_else(|| {
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
            let record = self.symbol_record(&target)?.ok_or_else(|| {
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
    ) -> Result<Option<SymbolRecordV1>, CodeGraphProjectionError> {
        let identity = symbol_entity_id(occurrence)?;
        let Some(entity) =
            self.snapshot
                .entity(&namespace()?, &identity, Arc::clone(&self.cancellation))?
        else {
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

struct BuiltProjection {
    watermark: GraphWatermark,
    entities: Vec<GraphEntity>,
    relations: Vec<GraphRelation>,
}

fn build_projection(
    generation: &CodeGenerationId,
    edges: &[CanonicalRelationEdgeV1],
    chunks: &[CodeSearchChunkV1],
    cancellation: Arc<dyn GraphCancellation>,
) -> Result<BuiltProjection, CodeGraphProjectionError> {
    generation
        .validate()
        .map_err(|error| CodeGraphProjectionError::Contract(error.to_string()))?;
    let mut bindings = BTreeMap::<SymbolOccurrenceId, CodeGraphSymbolBindingV1>::new();
    for chunk in chunks {
        if cancellation.is_cancelled() {
            return Err(CodeGraphProjectionError::Cancelled);
        }
        chunk
            .validate()
            .map_err(|error| CodeGraphProjectionError::Contract(error.to_string()))?;
        if chunk.anchor.generation_id != *generation {
            return Err(CodeGraphProjectionError::GenerationMismatch);
        }
        let Some(symbol) = chunk.anchor.symbol_occurrence_id.clone() else {
            continue;
        };
        let candidate = CodeGraphSymbolBindingV1 {
            file: chunk.anchor.file_occurrence_id.clone(),
            chunk: Some(chunk.id.clone()),
            language_descriptor_revision: chunk.language_descriptor_revision.clone(),
        };
        match bindings.entry(symbol) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(candidate);
            }
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                let current = entry.get_mut();
                if current.file != candidate.file
                    || current.language_descriptor_revision
                        != candidate.language_descriptor_revision
                {
                    return Err(CodeGraphProjectionError::Contract(
                        "one symbol occurrence has conflicting graph candidate bindings".to_owned(),
                    ));
                }
                if candidate.chunk < current.chunk {
                    current.chunk = candidate.chunk;
                }
            }
        }
    }

    let mut retained_edges = Vec::new();
    for edge in edges {
        if cancellation.is_cancelled() {
            return Err(CodeGraphProjectionError::Cancelled);
        }
        validate_edge(edge)?;
        if bindings.contains_key(&edge.from_occurrence) {
            retained_edges.push(edge.clone());
        }
    }
    retained_edges.sort_by(compare_edges);
    retained_edges.dedup();

    let mut occurrences: BTreeSet<_> = bindings.keys().cloned().collect();
    for edge in &retained_edges {
        occurrences.insert(edge.to_occurrence.clone());
    }
    let mut entities = Vec::with_capacity(occurrences.len() + retained_edges.len() + 1);
    for occurrence in occurrences {
        let record = SymbolRecordV1 {
            binding: bindings.get(&occurrence).cloned(),
            occurrence,
        };
        entities.push(symbol_entity(record)?);
    }
    for edge in &retained_edges {
        entities.push(edge_entity(edge)?);
    }
    let projection_node_count = entities.len().checked_add(1).ok_or_else(|| {
        CodeGraphProjectionError::Contract("code graph projection node count overflowed".to_owned())
    })?;
    entities.push(current_generation_entity(
        generation,
        projection_node_count,
    )?);

    let mut relations = Vec::with_capacity(retained_edges.len().saturating_mul(2));
    for edge in retained_edges {
        relations.push(source_relation(&edge)?);
        relations.push(target_relation(&edge)?);
    }
    Ok(BuiltProjection {
        watermark: GraphWatermark::new(stable_identity("watermark", generation.as_str()))?,
        entities,
        relations,
    })
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct CurrentGenerationV1 {
    generation: CodeGenerationId,
    projection_node_count: usize,
}

fn read_current_generation(
    snapshot: &GraphSnapshot,
    cancellation: Arc<dyn GraphCancellation>,
) -> Result<CurrentGenerationV1, CodeGraphProjectionError> {
    let identity = GraphEntityId::new(CURRENT_GENERATION_ENTITY)?;
    let entity = snapshot
        .entity(&namespace()?, &identity, cancellation)?
        .ok_or_else(|| {
            CodeGraphProjectionError::Unavailable(
                "code graph generation is not published".to_owned(),
            )
        })?;
    let generation = property_string(&entity, CURRENT_GENERATION_PROPERTY)?;
    let projection_node_count = property_string(&entity, PROJECTION_NODE_COUNT_PROPERTY)?
        .parse::<usize>()
        .map_err(|error| {
            CodeGraphProjectionError::Corrupt(format!(
                "code graph projection node count is invalid: {error}"
            ))
        })?;
    if projection_node_count == 0 {
        return Err(CodeGraphProjectionError::Corrupt(
            "code graph projection node count is zero".to_owned(),
        ));
    }
    Ok(CurrentGenerationV1 {
        generation: CodeGenerationId::new(generation)
            .map_err(|error| CodeGraphProjectionError::Corrupt(error.to_string()))?,
        projection_node_count,
    })
}

fn current_generation_entity(
    generation: &CodeGenerationId,
    projection_node_count: usize,
) -> Result<GraphEntity, CodeGraphProjectionError> {
    GraphEntity::new(
        GraphEntityId::new(CURRENT_GENERATION_ENTITY)?,
        BTreeSet::new(),
        BTreeMap::from([
            (
                GraphPropertyName::new(CURRENT_GENERATION_PROPERTY)?,
                GraphProperty::String(generation.as_str().to_owned()),
            ),
            (
                GraphPropertyName::new(PROJECTION_NODE_COUNT_PROPERTY)?,
                GraphProperty::String(projection_node_count.to_string()),
            ),
        ]),
    )
    .map_err(Into::into)
}

fn symbol_entity(record: SymbolRecordV1) -> Result<GraphEntity, CodeGraphProjectionError> {
    validate_symbol_record(&record)?;
    GraphEntity::new(
        symbol_entity_id(&record.occurrence)?,
        BTreeSet::from([GraphLabel::new(SYMBOL_LABEL)?]),
        BTreeMap::from([(
            GraphPropertyName::new(SYMBOL_RECORD_PROPERTY)?,
            GraphProperty::Bytes(serialize(&record)?),
        )]),
    )
    .map_err(Into::into)
}

fn edge_entity(edge: &CanonicalRelationEdgeV1) -> Result<GraphEntity, CodeGraphProjectionError> {
    GraphEntity::new(
        edge_entity_id(edge)?,
        BTreeSet::from([GraphLabel::new(EDGE_LABEL)?]),
        BTreeMap::from([(
            GraphPropertyName::new(EDGE_RECORD_PROPERTY)?,
            GraphProperty::Bytes(serialize(edge)?),
        )]),
    )
    .map_err(Into::into)
}

fn source_relation(
    edge: &CanonicalRelationEdgeV1,
) -> Result<GraphRelation, CodeGraphProjectionError> {
    GraphRelation::new(
        relation_id("source", edge)?,
        symbol_entity_id(&edge.from_occurrence)?,
        edge_entity_id(edge)?,
        GraphRelationKind::new(SOURCE_EDGE_KIND)?,
        BTreeMap::new(),
    )
    .map_err(Into::into)
}

fn target_relation(
    edge: &CanonicalRelationEdgeV1,
) -> Result<GraphRelation, CodeGraphProjectionError> {
    GraphRelation::new(
        relation_id("target", edge)?,
        edge_entity_id(edge)?,
        symbol_entity_id(&edge.to_occurrence)?,
        GraphRelationKind::new(TARGET_EDGE_KIND)?,
        BTreeMap::new(),
    )
    .map_err(Into::into)
}

fn symbol_entity_id(
    occurrence: &SymbolOccurrenceId,
) -> Result<GraphEntityId, CodeGraphProjectionError> {
    GraphEntityId::new(stable_identity("symbol", occurrence.as_str())).map_err(Into::into)
}

fn edge_entity_id(
    edge: &CanonicalRelationEdgeV1,
) -> Result<GraphEntityId, CodeGraphProjectionError> {
    GraphEntityId::new(stable_identity("edge", &hex::encode(serialize(edge)?))).map_err(Into::into)
}

fn relation_id(
    role: &str,
    edge: &CanonicalRelationEdgeV1,
) -> Result<GraphRelationId, CodeGraphProjectionError> {
    GraphRelationId::new(stable_identity(role, edge_entity_id(edge)?.as_str())).map_err(Into::into)
}

fn stable_identity(kind: &str, value: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(kind.as_bytes());
    digest.update([0]);
    digest.update(value.as_bytes());
    format!("{kind}:{}", hex::encode(digest.finalize()))
}

fn namespace() -> Result<GraphNamespace, CodeGraphProjectionError> {
    GraphNamespace::new(CODE_NAMESPACE).map_err(Into::into)
}

fn projection() -> Result<GraphProjectionId, CodeGraphProjectionError> {
    GraphProjectionId::new(CODE_PROJECTION).map_err(Into::into)
}

fn source_generation(
    generation: &CodeGenerationId,
) -> Result<SourceGeneration, CodeGraphProjectionError> {
    SourceGeneration::new(stable_identity("generation", generation.as_str())).map_err(Into::into)
}

fn serialize(value: &impl Serialize) -> Result<Vec<u8>, CodeGraphProjectionError> {
    serde_json::to_vec(value).map_err(|error| CodeGraphProjectionError::Contract(error.to_string()))
}

fn deserialize_property<T>(entity: &GraphEntity, name: &str) -> Result<T, CodeGraphProjectionError>
where
    T: for<'de> Deserialize<'de>,
{
    let property = entity
        .properties
        .get(&GraphPropertyName::new(name)?)
        .ok_or_else(|| {
            CodeGraphProjectionError::Corrupt(format!("code graph entity is missing {name}"))
        })?;
    let GraphProperty::Bytes(bytes) = property else {
        return Err(CodeGraphProjectionError::Corrupt(format!(
            "code graph entity {name} has the wrong type"
        )));
    };
    serde_json::from_slice(bytes)
        .map_err(|error| CodeGraphProjectionError::Corrupt(error.to_string()))
}

fn property_string(entity: &GraphEntity, name: &str) -> Result<String, CodeGraphProjectionError> {
    let property = entity
        .properties
        .get(&GraphPropertyName::new(name)?)
        .ok_or_else(|| {
            CodeGraphProjectionError::Corrupt(format!("code graph entity is missing {name}"))
        })?;
    let GraphProperty::String(value) = property else {
        return Err(CodeGraphProjectionError::Corrupt(format!(
            "code graph entity {name} has the wrong type"
        )));
    };
    Ok(value.clone())
}

fn has_label(entity: &GraphEntity, label: &str) -> bool {
    entity
        .labels
        .iter()
        .any(|candidate| candidate.as_str() == label)
}

fn validate_reader_metadata(
    repository_id: Option<&RepositoryId>,
    freshness: &SourceFreshness,
) -> Result<(), CodeGraphProjectionError> {
    if let Some(repository_id) = repository_id {
        repository_id
            .validate()
            .map_err(|error| CodeGraphProjectionError::Contract(error.to_string()))?;
    }
    freshness
        .source_namespace
        .validate()
        .map_err(|error| CodeGraphProjectionError::Contract(error.to_string()))?;
    freshness
        .source_instance
        .validate()
        .map_err(|error| CodeGraphProjectionError::Contract(error.to_string()))?;
    freshness
        .policy_revision
        .validate()
        .map_err(|error| CodeGraphProjectionError::Contract(error.to_string()))
}

fn validate_symbol_record(record: &SymbolRecordV1) -> Result<(), CodeGraphProjectionError> {
    record
        .occurrence
        .validate()
        .map_err(|error| CodeGraphProjectionError::Contract(error.to_string()))?;
    if let Some(binding) = &record.binding {
        binding
            .file
            .validate()
            .map_err(|error| CodeGraphProjectionError::Contract(error.to_string()))?;
        if let Some(chunk) = &binding.chunk {
            chunk
                .validate()
                .map_err(|error| CodeGraphProjectionError::Contract(error.to_string()))?;
        }
        binding
            .language_descriptor_revision
            .validate()
            .map_err(|error| CodeGraphProjectionError::Contract(error.to_string()))?;
    }
    Ok(())
}

fn validate_edge(edge: &CanonicalRelationEdgeV1) -> Result<(), CodeGraphProjectionError> {
    edge.from_occurrence
        .validate()
        .map_err(|error| CodeGraphProjectionError::Contract(error.to_string()))?;
    edge.to_occurrence
        .validate()
        .map_err(|error| CodeGraphProjectionError::Contract(error.to_string()))?;
    edge.evidence_span
        .validate()
        .map_err(|error| CodeGraphProjectionError::Contract(error.to_string()))
}

fn compare_edges(left: &CanonicalRelationEdgeV1, right: &CanonicalRelationEdgeV1) -> Ordering {
    (
        &left.from_occurrence,
        &left.to_occurrence,
        left.kind,
        left.authority,
        left.evidence_span.start_byte,
        left.evidence_span.end_byte,
    )
        .cmp(&(
            &right.from_occurrence,
            &right.to_occurrence,
            right.kind,
            right.authority,
            right.evidence_span.start_byte,
            right.evidence_span.end_byte,
        ))
}

#[derive(Clone, Debug)]
struct ApplicationCancellation(CancellationSignal);

impl GraphCancellation for ApplicationCancellation {
    fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
}

fn application_cancellation(cancellation: &CancellationSignal) -> Arc<dyn GraphCancellation> {
    Arc::new(ApplicationCancellation(cancellation.clone()))
}
