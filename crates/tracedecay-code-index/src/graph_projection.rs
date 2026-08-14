//! Durable code-graph projection over the opaque graph database boundary.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_application::CancellationSignal;
use tracedecay_domain::{
    CanonicalRelationEdgeV1, CodeGenerationId, CodeSearchChunkId, CodeSearchChunkV1,
    EdgeAuthorityV1, FileOccurrenceId, LanguageDescriptorRevision, RelationEdgeKindV1,
    RepositoryId, SourceFreshness, SourceSpan, SymbolOccurrenceId, canonical_sha256,
};
use tracedecay_graph_db::{
    GraphCancellation, GraphDbError, GraphEntity, GraphEntityId, GraphEntityRef, GraphGenerationId,
    GraphGenerationManifest, GraphGenerationRelation, GraphIdempotencyKey, GraphLabel,
    GraphNamespace, GraphProjectionId, GraphProjectionIdentity, GraphProjectorRevision,
    GraphProperty, GraphPropertyName, GraphRelationId, GraphRelationKind, GraphTraversalDirection,
    SourceGeneration, TraversalRequest, VerifiedGraphSnapshot,
};
#[cfg(any(feature = "test-helpers", feature = "eval-helpers"))]
use tracedecay_graph_db::{GraphWatermark, NeverCancelled};

mod builder;
mod interactive;
mod reader;
mod schema;
mod traversal;

pub use self::builder::build_published_code_graph_manifest_checked;
use self::builder::{ProductionCodeGraphInputs, build_projection};
use self::interactive::InteractiveCatalog;
pub use self::interactive::{
    CodeGraphDegreeRankingV1, CodeGraphEdgeKindCountsV1, CodeGraphImpactBatchV1,
    CodeGraphImpactedSymbolV1, CodeGraphInteractiveReader, CodeGraphPathSearchV1,
    CodeGraphSemanticEdgeV1, CodeGraphSymbolDegreesV1, CodeGraphSymbolPageV1,
    CodeGraphSymbolSummaryV1,
};
use self::schema::{
    SYMBOL_LABEL, SYMBOL_RECORD_PROPERTY, deserialize_property, has_label, serialize,
    stable_identity,
};
use self::traversal::{FrontierPath, admit_frontier_path, best_frontier_path, compare_paths};
use crate::lineage::LineageSymbolRecordV1;

#[cfg(any(feature = "test-helpers", feature = "eval-helpers"))]
const CODE_NAMESPACE: &str = "code-graph";
const CODE_PROJECTION: &str = "code-generation";
const CURRENT_GENERATION_ENTITY: &str = "code-current-generation";
const CURRENT_GENERATION_PROPERTY: &str = "current-generation";
const PROJECTION_NODE_COUNT_PROPERTY: &str = "projection-node-count";
const CHUNK_RECORD_PROPERTY: &str = "chunk-record";
const EDGE_RECORD_PROPERTY: &str = "edge-record";
const CHUNK_LABEL: &str = "CodeChunk";
const EDGE_LABEL: &str = "CodeRelationEvidence";
const FILE_SYMBOL_EDGE_KIND: &str = "CodeFileContainsSymbol";
const CHUNK_SYMBOL_EDGE_KIND: &str = "CodeChunkDescribesSymbol";
const SOURCE_EDGE_KIND: &str = "CodeRelationSource";
const TARGET_EDGE_KIND: &str = "CodeRelationTarget";
pub const CODE_GRAPH_PROJECTOR_REVISION: &str = "code-graph-projector.v4";

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
    #[error("code graph operation deadline exceeded")]
    DeadlineExceeded,
    #[error("code graph database conflict")]
    Conflict,
    #[error(
        "code graph projection `{namespace}/{projection}` is quarantined after recovery mismatch: {message}"
    )]
    ProjectionMismatch {
        namespace: String,
        projection: String,
        message: String,
    },
    #[error(
        "code graph generation `{namespace}/{projection}/{generation}` is quarantined after recovery mismatch: {message}"
    )]
    RecoveredGenerationMismatch {
        namespace: String,
        projection: String,
        generation: String,
        message: String,
    },
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
            GraphDbError::BudgetExhausted { .. } => Self::BudgetExhausted,
            GraphDbError::DeadlineExceeded => Self::DeadlineExceeded,
            GraphDbError::ProjectionMismatch {
                namespace,
                projection,
                message,
            } => Self::ProjectionMismatch {
                namespace,
                projection,
                message,
            },
            GraphDbError::GenerationMismatch {
                namespace,
                projection,
                generation,
                message,
            } => Self::RecoveredGenerationMismatch {
                namespace,
                projection,
                generation,
                message,
            },
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
    pub logical_path: Option<String>,
    pub source_span: Option<SourceSpan>,
    pub chunk: Option<CodeSearchChunkId>,
    pub language_descriptor_revision: LanguageDescriptorRevision,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct SymbolRecordV1 {
    occurrence: SymbolOccurrenceId,
    binding: Option<CodeGraphSymbolBindingV1>,
    metadata: Option<LineageSymbolRecordV1>,
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

#[derive(Clone)]
pub struct CodeGraphProjectionStore {
    snapshot: Arc<VerifiedGraphSnapshot>,
    projection: GraphProjectionIdentity,
    generation: CodeGenerationId,
    /// Generation-pinned name/kind catalog, built lazily on first interactive
    /// read and shared by every reader cloned from this store. The catalog is
    /// derived from the verified snapshot and dies with it, so it is a cache
    /// of the projection authority rather than a parallel authority.
    interactive_catalog: Arc<RwLock<Option<Arc<InteractiveCatalog>>>>,
}

impl fmt::Debug for CodeGraphProjectionStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodeGraphProjectionStore")
            .finish_non_exhaustive()
    }
}

impl CodeGraphProjectionStore {
    pub fn from_verified_snapshot(
        snapshot: VerifiedGraphSnapshot,
        generation: CodeGenerationId,
    ) -> Result<Self, CodeGraphProjectionError> {
        let projection = snapshot.projection().clone();
        let expected = code_graph_generation_id(
            &generation,
            &GraphProjectorRevision::try_from(CODE_GRAPH_PROJECTOR_REVISION.to_owned())?,
        )?;
        if snapshot.generation() != &expected {
            return Err(CodeGraphProjectionError::GenerationMismatch);
        }
        Ok(Self {
            snapshot: Arc::new(snapshot),
            projection,
            generation,
            interactive_catalog: Arc::new(RwLock::new(None)),
        })
    }

    pub fn evidence_reader(
        &self,
        generation: &CodeGenerationId,
        repository_id: Option<RepositoryId>,
        freshness: SourceFreshness,
        cancellation: &CancellationSignal,
    ) -> Result<CodeGraphEvidenceReader, CodeGraphProjectionError> {
        self.evidence_reader_with_cancellation(
            generation,
            repository_id,
            freshness,
            application_cancellation(cancellation),
        )
    }

    pub fn evidence_reader_with_cancellation(
        &self,
        generation: &CodeGenerationId,
        repository_id: Option<RepositoryId>,
        freshness: SourceFreshness,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<CodeGraphEvidenceReader, CodeGraphProjectionError> {
        if generation != &self.generation {
            return Err(CodeGraphProjectionError::GenerationMismatch);
        }
        generation
            .validate()
            .map_err(|error| CodeGraphProjectionError::Contract(error.to_string()))?;
        validate_reader_metadata(repository_id.as_ref(), &freshness)?;
        let snapshot = Arc::clone(&self.snapshot);
        let current =
            read_current_generation(&snapshot, &self.projection, Arc::clone(&cancellation))?;
        if current.generation != *generation {
            return Err(CodeGraphProjectionError::GenerationMismatch);
        }
        Ok(CodeGraphEvidenceReader {
            generation: generation.clone(),
            repository_id,
            freshness,
            projection: self.projection.clone(),
            snapshot,
            projection_node_count: current.projection_node_count,
            cancellation,
        })
    }

    /// The published generation this store is pinned to.
    pub fn generation(&self) -> &CodeGenerationId {
        &self.generation
    }

    pub fn interactive_reader(
        &self,
        generation: &CodeGenerationId,
        cancellation: &CancellationSignal,
    ) -> Result<CodeGraphInteractiveReader, CodeGraphProjectionError> {
        self.interactive_reader_with_cancellation(
            generation,
            application_cancellation(cancellation),
        )
    }

    pub fn interactive_reader_with_cancellation(
        &self,
        generation: &CodeGenerationId,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<CodeGraphInteractiveReader, CodeGraphProjectionError> {
        if generation != &self.generation {
            return Err(CodeGraphProjectionError::GenerationMismatch);
        }
        generation
            .validate()
            .map_err(|error| CodeGraphProjectionError::Contract(error.to_string()))?;
        let snapshot = Arc::clone(&self.snapshot);
        let current =
            read_current_generation(&snapshot, &self.projection, Arc::clone(&cancellation))?;
        if current.generation != *generation {
            return Err(CodeGraphProjectionError::GenerationMismatch);
        }
        Ok(CodeGraphInteractiveReader::assemble(
            generation.clone(),
            self.projection.clone(),
            snapshot,
            current.projection_node_count,
            cancellation,
            Arc::clone(&self.interactive_catalog),
        ))
    }
}

/// Mutable in-memory publisher reserved for hermetic tests and evaluations.
/// Persistent daemon publication never uses this type.
#[derive(Clone)]
#[cfg(any(feature = "test-helpers", feature = "eval-helpers"))]
struct InMemoryCodeGraphProjectionBuilder {
    snapshot: Arc<RwLock<Option<Arc<VerifiedGraphSnapshot>>>>,
    projection: GraphProjectionIdentity,
}

#[cfg(any(feature = "test-helpers", feature = "eval-helpers"))]
impl InMemoryCodeGraphProjectionBuilder {
    pub fn memory(cancellation: &CancellationSignal) -> Result<Self, CodeGraphProjectionError> {
        if cancellation.is_cancelled() {
            return Err(CodeGraphProjectionError::Cancelled);
        }
        Ok(Self {
            snapshot: Arc::new(RwLock::new(None)),
            projection: code_graph_projection_identity(default_namespace()?)?,
        })
    }

    #[cfg(feature = "test-helpers")]
    pub fn publish_code_graph(
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

    pub fn publish_with_cancellation(
        &self,
        generation: &CodeGenerationId,
        edges: &[CanonicalRelationEdgeV1],
        chunks: &[CodeSearchChunkV1],
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<GraphWatermark, CodeGraphProjectionError> {
        if cancellation.is_cancelled() {
            return Err(CodeGraphProjectionError::Cancelled);
        }
        let revision = GraphProjectorRevision::try_from(CODE_GRAPH_PROJECTOR_REVISION.to_owned())?;
        let manifest = build_code_graph_manifest(
            self.projection.clone(),
            generation,
            edges,
            chunks,
            &revision,
            Arc::clone(&cancellation),
        )?;
        let watermark = manifest.watermark.clone();
        let snapshot = VerifiedGraphSnapshot::memory(manifest, cancellation)?;
        *self.snapshot.write().map_err(|_| {
            CodeGraphProjectionError::Unavailable(
                "code graph verified snapshot lock is poisoned".to_owned(),
            )
        })? = Some(Arc::new(snapshot));
        Ok(watermark)
    }

    /// Publishes a hermetic generation that also carries its file snapshot and
    /// symbol index, so the interactive reader can resolve qualified names and
    /// kinds. [`Self::publish_with_cancellation`] publishes edges and chunks
    /// alone, which leaves every symbol without metadata and therefore
    /// unresolvable by name — the shape integration fixtures need.
    #[cfg(feature = "test-helpers")]
    pub fn publish_indexed_with_cancellation(
        &self,
        generation: &CodeGenerationId,
        edges: &[CanonicalRelationEdgeV1],
        chunks: &[CodeSearchChunkV1],
        files: &[tracedecay_domain::SanitizedCodeFileV1],
        symbols: &crate::lineage::GenerationSymbolIndexV1,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<GraphWatermark, CodeGraphProjectionError> {
        if cancellation.is_cancelled() {
            return Err(CodeGraphProjectionError::Cancelled);
        }
        if symbols.generation_id != *generation {
            return Err(CodeGraphProjectionError::GenerationMismatch);
        }
        let revision = GraphProjectorRevision::try_from(CODE_GRAPH_PROJECTOR_REVISION.to_owned())?;
        let check = {
            let cancellation = Arc::clone(&cancellation);
            move || {
                if cancellation.is_cancelled() {
                    Err(GraphDbError::Cancelled)
                } else {
                    Ok(())
                }
            }
        };
        let manifest = build_code_graph_manifest_inputs_checked(
            self.projection.clone(),
            generation,
            edges,
            chunks,
            Some(ProductionCodeGraphInputs {
                files,
                symbols,
                imports: &[],
            }),
            &revision,
            &check,
        )?;
        let watermark = manifest.watermark.clone();
        let snapshot = VerifiedGraphSnapshot::memory(manifest, cancellation)?;
        *self.snapshot.write().map_err(|_| {
            CodeGraphProjectionError::Unavailable(
                "code graph verified snapshot lock is poisoned".to_owned(),
            )
        })? = Some(Arc::new(snapshot));
        Ok(watermark)
    }

    #[cfg(feature = "test-helpers")]
    pub fn verified_store(
        &self,
        generation: &CodeGenerationId,
    ) -> Result<CodeGraphProjectionStore, CodeGraphProjectionError> {
        let snapshot = self
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
        CodeGraphProjectionStore::from_verified_snapshot(
            snapshot.as_ref().clone(),
            generation.clone(),
        )
    }

    #[cfg(feature = "test-helpers")]
    pub fn evidence_reader(
        &self,
        generation: &CodeGenerationId,
        repository_id: Option<RepositoryId>,
        freshness: SourceFreshness,
        cancellation: &CancellationSignal,
    ) -> Result<CodeGraphEvidenceReader, CodeGraphProjectionError> {
        let snapshot = self
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
        CodeGraphProjectionStore::from_verified_snapshot(
            snapshot.as_ref().clone(),
            generation.clone(),
        )?
        .evidence_reader(generation, repository_id, freshness, cancellation)
    }
}

#[derive(Clone)]
#[cfg(feature = "test-helpers")]
pub struct HermeticCodeGraphProjectionStore {
    inner: InMemoryCodeGraphProjectionBuilder,
}

#[cfg(feature = "test-helpers")]
impl HermeticCodeGraphProjectionStore {
    pub fn memory(cancellation: &CancellationSignal) -> Result<Self, CodeGraphProjectionError> {
        InMemoryCodeGraphProjectionBuilder::memory(cancellation).map(|inner| Self { inner })
    }

    pub fn publish_code_graph(
        &self,
        generation: &CodeGenerationId,
        edges: &[CanonicalRelationEdgeV1],
        chunks: &[CodeSearchChunkV1],
        cancellation: &CancellationSignal,
    ) -> Result<GraphWatermark, CodeGraphProjectionError> {
        self.inner
            .publish_code_graph(generation, edges, chunks, cancellation)
    }

    pub fn publish_with_cancellation(
        &self,
        generation: &CodeGenerationId,
        edges: &[CanonicalRelationEdgeV1],
        chunks: &[CodeSearchChunkV1],
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<GraphWatermark, CodeGraphProjectionError> {
        self.inner
            .publish_with_cancellation(generation, edges, chunks, cancellation)
    }

    /// Publishes a hermetic generation carrying its file snapshot and symbol
    /// index, so interactive reads can resolve symbols by qualified name and
    /// kind the way an activated production generation does.
    pub fn publish_indexed_with_cancellation(
        &self,
        generation: &CodeGenerationId,
        edges: &[CanonicalRelationEdgeV1],
        chunks: &[CodeSearchChunkV1],
        files: &[tracedecay_domain::SanitizedCodeFileV1],
        symbols: &crate::lineage::GenerationSymbolIndexV1,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<GraphWatermark, CodeGraphProjectionError> {
        self.inner.publish_indexed_with_cancellation(
            generation,
            edges,
            chunks,
            files,
            symbols,
            cancellation,
        )
    }

    pub fn verified_store(
        &self,
        generation: &CodeGenerationId,
    ) -> Result<CodeGraphProjectionStore, CodeGraphProjectionError> {
        self.inner.verified_store(generation)
    }

    pub fn evidence_reader(
        &self,
        generation: &CodeGenerationId,
        repository_id: Option<RepositoryId>,
        freshness: SourceFreshness,
        cancellation: &CancellationSignal,
    ) -> Result<CodeGraphEvidenceReader, CodeGraphProjectionError> {
        self.inner
            .evidence_reader(generation, repository_id, freshness, cancellation)
    }
}

#[derive(Clone)]
pub struct CodeGraphEvidenceReader {
    generation: CodeGenerationId,
    repository_id: Option<RepositoryId>,
    freshness: SourceFreshness,
    projection: GraphProjectionIdentity,
    snapshot: Arc<VerifiedGraphSnapshot>,
    projection_node_count: usize,
    cancellation: Arc<dyn GraphCancellation>,
}

impl fmt::Debug for CodeGraphEvidenceReader {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodeGraphEvidenceReader")
            .field("generation", &self.generation)
            .field("repository_id", &self.repository_id)
            .field("freshness", &self.freshness)
            .field("projection_node_count", &self.projection_node_count)
            .finish_non_exhaustive()
    }
}

pub fn code_graph_generation_id(
    generation: &CodeGenerationId,
    projector_revision: &GraphProjectorRevision,
) -> Result<GraphGenerationId, CodeGraphProjectionError> {
    generation
        .validate()
        .map_err(|error| CodeGraphProjectionError::Contract(error.to_string()))?;
    let digest = canonical_sha256(&(
        "tracedecay.code-graph-generation.v1",
        generation,
        projector_revision,
    ))
    .map_err(|error| CodeGraphProjectionError::Contract(error.to_string()))?;
    GraphGenerationId::new(format!("code-graph:{}", digest.as_str())).map_err(Into::into)
}

pub fn code_graph_idempotency_key(
    generation: &CodeGenerationId,
    projector_revision: &GraphProjectorRevision,
) -> Result<GraphIdempotencyKey, CodeGraphProjectionError> {
    let graph_generation = code_graph_generation_id(generation, projector_revision)?;
    GraphIdempotencyKey::new(format!("publish:{}", graph_generation.as_str())).map_err(Into::into)
}

pub fn code_graph_projection_identity(
    namespace: GraphNamespace,
) -> Result<GraphProjectionIdentity, CodeGraphProjectionError> {
    Ok(GraphProjectionIdentity::new(namespace, projection()?))
}

pub fn build_code_graph_manifest(
    projection: GraphProjectionIdentity,
    generation: &CodeGenerationId,
    edges: &[CanonicalRelationEdgeV1],
    chunks: &[CodeSearchChunkV1],
    projector_revision: &GraphProjectorRevision,
    cancellation: Arc<dyn GraphCancellation>,
) -> Result<GraphGenerationManifest, CodeGraphProjectionError> {
    let check = || {
        if cancellation.is_cancelled() {
            Err(GraphDbError::Cancelled)
        } else {
            Ok(())
        }
    };
    build_code_graph_manifest_checked(
        projection,
        generation,
        edges,
        chunks,
        projector_revision,
        &check,
    )
}

pub fn build_code_graph_manifest_checked(
    projection: GraphProjectionIdentity,
    generation: &CodeGenerationId,
    edges: &[CanonicalRelationEdgeV1],
    chunks: &[CodeSearchChunkV1],
    projector_revision: &GraphProjectorRevision,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<GraphGenerationManifest, CodeGraphProjectionError> {
    build_code_graph_manifest_inputs_checked(
        projection,
        generation,
        edges,
        chunks,
        None,
        projector_revision,
        check,
    )
}

fn build_code_graph_manifest_inputs_checked(
    projection: GraphProjectionIdentity,
    generation: &CodeGenerationId,
    edges: &[CanonicalRelationEdgeV1],
    chunks: &[CodeSearchChunkV1],
    production: Option<ProductionCodeGraphInputs<'_>>,
    projector_revision: &GraphProjectorRevision,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<GraphGenerationManifest, CodeGraphProjectionError> {
    check()?;
    if projection.projection != self::projection()? {
        return Err(CodeGraphProjectionError::Contract(
            "code graph projection identity uses a foreign projector".to_owned(),
        ));
    }
    let built = build_projection(&projection, generation, edges, chunks, production, check)?;
    GraphGenerationManifest::new_checked(
        projection,
        code_graph_generation_id(generation, projector_revision)?,
        source_generation(generation)?,
        built.watermark,
        vec![],
        built.entities,
        built.relations,
        check,
    )
    .map_err(Into::into)
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct CurrentGenerationV1 {
    generation: CodeGenerationId,
    projection_node_count: usize,
}

fn read_current_generation(
    snapshot: &VerifiedGraphSnapshot,
    projection: &GraphProjectionIdentity,
    cancellation: Arc<dyn GraphCancellation>,
) -> Result<CurrentGenerationV1, CodeGraphProjectionError> {
    let identity = GraphEntityId::new(CURRENT_GENERATION_ENTITY)?;
    let entity = snapshot
        .entity(
            &GraphEntityRef::new(projection.clone(), identity),
            cancellation,
        )?
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
    projection: &GraphProjectionIdentity,
    edge: &CanonicalRelationEdgeV1,
) -> Result<GraphGenerationRelation, CodeGraphProjectionError> {
    GraphGenerationRelation::new(
        relation_id("source", edge)?,
        GraphEntityRef::new(projection.clone(), symbol_entity_id(&edge.from_occurrence)?),
        GraphEntityRef::new(projection.clone(), edge_entity_id(edge)?),
        GraphRelationKind::new(SOURCE_EDGE_KIND)?,
        BTreeMap::new(),
    )
    .map_err(Into::into)
}

fn target_relation(
    projection: &GraphProjectionIdentity,
    edge: &CanonicalRelationEdgeV1,
) -> Result<GraphGenerationRelation, CodeGraphProjectionError> {
    GraphGenerationRelation::new(
        relation_id("target", edge)?,
        GraphEntityRef::new(projection.clone(), edge_entity_id(edge)?),
        GraphEntityRef::new(projection.clone(), symbol_entity_id(&edge.to_occurrence)?),
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

#[cfg(any(feature = "test-helpers", feature = "eval-helpers"))]
fn default_namespace() -> Result<GraphNamespace, CodeGraphProjectionError> {
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
    if let Some(metadata) = &record.metadata {
        builder::validate_symbol_metadata(metadata, &record.occurrence)?;
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

/// Loads and revalidates one symbol record by occurrence. `Ok(None)` means the
/// occurrence has no entity in this generation; every payload mismatch is a
/// typed corruption, never a silent miss.
fn load_symbol_record(
    snapshot: &VerifiedGraphSnapshot,
    projection: &GraphProjectionIdentity,
    occurrence: &SymbolOccurrenceId,
    cancellation: Arc<dyn GraphCancellation>,
) -> Result<Option<SymbolRecordV1>, CodeGraphProjectionError> {
    let identity = symbol_entity_id(occurrence)?;
    let reference = GraphEntityRef::new(projection.clone(), identity.clone());
    let Some(entity) = snapshot.entity(&reference, cancellation)? else {
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

/// A read is cancelled when either the store lifecycle or the individual
/// request asks for it.
struct CodeGraphReadCancellation {
    lifecycle: Arc<dyn GraphCancellation>,
    request: Arc<dyn GraphCancellation>,
}

impl GraphCancellation for CodeGraphReadCancellation {
    fn is_cancelled(&self) -> bool {
        self.lifecycle.is_cancelled() || self.request.is_cancelled()
    }
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
