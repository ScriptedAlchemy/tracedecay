use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use tracedecay_domain::{
    AdmittedEmbeddingProjectionKeyV1, CodeGenerationId, CodeSearchChunkId, ManifestDigest,
    ProjectionKeyV1, VectorGenerationIdV1,
};
use tracedecay_graph_db::{
    GraphCancellation, GraphDb, GraphProjectionTelemetryRequest, GraphProperty, GraphSnapshot,
    GraphWatermark, VectorMetric,
};

use super::{
    PreparedVectorGenerationV1, VectorGenerationBuildIdV1, VectorGenerationPlanV1,
    VectorGenerationPublicationV1, VectorGenerationStateMachineV1, VectorGenerationStoreErrorV1,
    VectorProjectionCheckpointV1,
};

mod native_records;
mod persistence;
mod reclaim;
mod transitions;

pub use reclaim::VectorGenerationReclaimReceiptV1;

use native_records::{
    read_build_records, read_cataloged_generation_records, read_generation_metadata,
    read_state_metadata,
};
use persistence::{
    check_cancelled, generation_label, graph_namespace, graph_projection, map_graph_error,
    measured_resident_bytes, normalized_vector_score, required_string, resident_size_overflow,
    search_vector_property, storage_error, vector_metric,
};

pub const SEMANTIC_VECTOR_GRAPH_PROJECTION: &str = "tracedecay.semantic-vector.graph";
const VECTOR_PROPERTY: &str = "vector";
const CHUNK_ID_PROPERTY: &str = "chunk_id";
const GENERATION_ID_PROPERTY: &str = "generation_id";
const MAX_RESIDENT_VECTOR_ROWS: usize = 100_000;

pub struct GraphVectorGenerationStoreV1 {
    graph: Arc<GraphDb>,
}

pub struct SemanticVectorGraphSearchRequestV1 {
    pub generation_id: VectorGenerationIdV1,
    pub embedding_key: AdmittedEmbeddingProjectionKeyV1,
    pub source_generation: CodeGenerationId,
    pub source_manifest_digest: ManifestDigest,
    pub query: Vec<f32>,
    pub limit: usize,
    pub cancellation: Arc<dyn GraphCancellation>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SemanticVectorGraphMatchV1 {
    pub chunk_id: CodeSearchChunkId,
    pub distance: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SemanticVectorGraphSearchResultV1 {
    pub generation_id: VectorGenerationIdV1,
    pub matches: Vec<SemanticVectorGraphMatchV1>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SemanticHybridLexicalCandidateV1 {
    pub chunk_id: CodeSearchChunkId,
    pub score: f64,
}

pub struct SemanticHybridGraphSearchRequestV1 {
    pub vector: SemanticVectorGraphSearchRequestV1,
    pub lexical: Vec<SemanticHybridLexicalCandidateV1>,
    pub vector_weight: f64,
    pub lexical_weight: f64,
    pub limit: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SemanticHybridGraphMatchV1 {
    pub chunk_id: CodeSearchChunkId,
    pub vector_distance: Option<f64>,
    pub lexical_score: Option<f64>,
    pub combined_score: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SemanticHybridGraphSearchResultV1 {
    pub generation_id: VectorGenerationIdV1,
    pub matches: Vec<SemanticHybridGraphMatchV1>,
}

pub struct ActiveVectorGenerationPublicationGuardV1 {
    _snapshot: GraphSnapshot,
    watermark: GraphWatermark,
    generation_id: VectorGenerationIdV1,
    projection_key: ProjectionKeyV1,
    source_generation: CodeGenerationId,
    source_manifest_digest: ManifestDigest,
    embedding_key: AdmittedEmbeddingProjectionKeyV1,
}

impl ActiveVectorGenerationPublicationGuardV1 {
    pub fn watermark(&self) -> &GraphWatermark {
        &self.watermark
    }

    pub fn generation_id(&self) -> &VectorGenerationIdV1 {
        &self.generation_id
    }

    pub fn projection_key(&self) -> &ProjectionKeyV1 {
        &self.projection_key
    }

    pub fn source_generation(&self) -> &CodeGenerationId {
        &self.source_generation
    }

    pub fn source_manifest_digest(&self) -> &ManifestDigest {
        &self.source_manifest_digest
    }

    pub fn embedding_key(&self) -> &AdmittedEmbeddingProjectionKeyV1 {
        &self.embedding_key
    }
}

/// Active generation plus the monotonic record revision that made it current.
#[derive(Clone, Debug)]
pub struct ActiveGraphVectorGenerationSnapshotV1 {
    revision: u64,
    generation: super::PublishedVectorGenerationV1,
}

impl ActiveGraphVectorGenerationSnapshotV1 {
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    pub fn generation(&self) -> &super::PublishedVectorGenerationV1 {
        &self.generation
    }

    pub fn into_generation(self) -> super::PublishedVectorGenerationV1 {
        self.generation
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActiveVectorResidentPlanV1 {
    pub watermark: GraphWatermark,
    pub generation_id: VectorGenerationIdV1,
    pub retained_bytes: u64,
    pub hydration_peak_bytes: u64,
}

pub struct ResidentVectorGenerationV1 {
    pub generation_id: VectorGenerationIdV1,
    pub projection_key: ProjectionKeyV1,
    pub source_generation: CodeGenerationId,
    pub source_manifest_digest: ManifestDigest,
    pub rows: Vec<ResidentVectorRowV1>,
    pub retained_bytes: u64,
}

pub struct ResidentVectorRowV1 {
    pub chunk_id: CodeSearchChunkId,
    pub values: Box<[f32]>,
}

impl GraphVectorGenerationStoreV1 {
    pub fn open(
        graph: Arc<GraphDb>,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Self, VectorGenerationStoreErrorV1> {
        let store = Self { graph };
        check_cancelled(cancellation.as_ref())?;
        let snapshot = store.graph.snapshot().map_err(map_graph_error)?;
        let exists = snapshot
            .projection_telemetry(GraphProjectionTelemetryRequest {
                namespace: graph_namespace()?,
                projection: graph_projection()?,
                cancellation: Arc::clone(&cancellation),
            })
            .map_err(map_graph_error)?
            .is_some();
        drop(snapshot);
        if !exists {
            let mut initial = VectorGenerationStateMachineV1::new();
            match store.initialize_state(&mut initial, Arc::clone(&cancellation)) {
                Ok(_) | Err(VectorGenerationStoreErrorV1::ConcurrentMutation) => {}
                Err(error) => return Err(error),
            }
        }
        store.verify_existing_state(cancellation)?;
        Ok(store)
    }

    fn verify_existing_state(
        &self,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<(), VectorGenerationStoreErrorV1> {
        check_cancelled(cancellation.as_ref())?;
        let snapshot = self.graph.snapshot().map_err(map_graph_error)?;
        let metadata = read_state_metadata(&snapshot, Arc::clone(&cancellation))?;
        let active = metadata.active_generation;
        let records = active
            .as_ref()
            .map(|generation| {
                read_cataloged_generation_records(&snapshot, generation, Arc::clone(&cancellation))
            })
            .transpose()?
            .flatten();
        if active.is_some() && records.is_none() {
            return Err(VectorGenerationStoreErrorV1::Corrupt(
                "active semantic vector generation records are missing".to_owned(),
            ));
        }
        if let Some(records) = records {
            let rows = u64::try_from(records.generation.vectors().len()).map_err(storage_error)?;
            if rows != metadata.active_row_count {
                return Err(VectorGenerationStoreErrorV1::Corrupt(
                    "active semantic vector generation measures are inconsistent".to_owned(),
                ));
            }
        }
        drop(snapshot);
        check_cancelled(cancellation.as_ref())?;
        Ok(())
    }

    pub async fn begin_generation(
        &self,
        plan: VectorGenerationPlanV1,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<VectorGenerationBuildIdV1, VectorGenerationStoreErrorV1> {
        self.begin_generation_records(plan, false, cancellation)
    }

    pub async fn rebuild_generation(
        &self,
        plan: VectorGenerationPlanV1,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<VectorGenerationBuildIdV1, VectorGenerationStoreErrorV1> {
        self.begin_generation_records(plan, true, cancellation)
    }

    pub async fn cancel_generation(
        &self,
        build_id: &VectorGenerationBuildIdV1,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<bool, VectorGenerationStoreErrorV1> {
        self.cancel_generation_records(build_id, cancellation)
    }

    pub async fn commit_batch(
        &self,
        build_id: &VectorGenerationBuildIdV1,
        expected_checkpoint: Option<&VectorProjectionCheckpointV1>,
        prepared: PreparedVectorGenerationV1,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<VectorProjectionCheckpointV1, VectorGenerationStoreErrorV1> {
        self.commit_batch_records(build_id, expected_checkpoint, prepared, cancellation)
    }

    pub async fn publish_generation(
        &self,
        build_id: &VectorGenerationBuildIdV1,
        expected_active_generation: Option<&VectorGenerationIdV1>,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<VectorGenerationPublicationV1, VectorGenerationStoreErrorV1> {
        self.publish_generation_records(build_id, expected_active_generation, cancellation)
    }

    pub async fn activate_generation(
        &self,
        generation_id: &VectorGenerationIdV1,
        expected_active_generation: Option<&VectorGenerationIdV1>,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<VectorGenerationPublicationV1, VectorGenerationStoreErrorV1> {
        self.activate_generation_records(generation_id, expected_active_generation, cancellation)
    }

    pub async fn deactivate_generation(
        &self,
        expected_active_generation: Option<&VectorGenerationIdV1>,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<(), VectorGenerationStoreErrorV1> {
        self.deactivate_generation_records(expected_active_generation, cancellation)
    }

    pub async fn active_generation_id(
        &self,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Option<VectorGenerationIdV1>, VectorGenerationStoreErrorV1> {
        let snapshot = self.graph.snapshot().map_err(map_graph_error)?;
        read_state_metadata(&snapshot, cancellation).map(|metadata| metadata.active_generation)
    }

    /// Read the active immutable generation together with the monotonic
    /// record revision that made it current. Callers holding the revision can
    /// later verify currency without re-reading vector payloads.
    pub async fn active_generation_snapshot_for(
        &self,
        embedding_key: &AdmittedEmbeddingProjectionKeyV1,
        source_generation: &CodeGenerationId,
        source_manifest_digest: &ManifestDigest,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Option<ActiveGraphVectorGenerationSnapshotV1>, VectorGenerationStoreErrorV1> {
        let snapshot = self.graph.snapshot().map_err(map_graph_error)?;
        let metadata = read_state_metadata(&snapshot, Arc::clone(&cancellation))?;
        let Some(active) = metadata.active_generation.as_ref() else {
            return Ok(None);
        };
        let records = read_cataloged_generation_records(&snapshot, active, cancellation)?
            .ok_or_else(|| {
                VectorGenerationStoreErrorV1::Corrupt(
                    "active semantic vector generation records are missing".to_owned(),
                )
            })?;
        let generation = records.generation;
        if generation.embedding_key() != embedding_key
            || generation.source_generation() != source_generation
            || generation.source_manifest_digest() != source_manifest_digest
        {
            return Ok(None);
        }
        Ok(Some(ActiveGraphVectorGenerationSnapshotV1 {
            revision: metadata.revision,
            generation,
        }))
    }

    /// Return the active immutable generation only when every query-facing
    /// projection and source identity matches exactly.
    pub async fn active_generation_for(
        &self,
        embedding_key: &AdmittedEmbeddingProjectionKeyV1,
        source_generation: &CodeGenerationId,
        source_manifest_digest: &ManifestDigest,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Option<super::PublishedVectorGenerationV1>, VectorGenerationStoreErrorV1> {
        Ok(self
            .active_generation_snapshot_for(
                embedding_key,
                source_generation,
                source_manifest_digest,
                cancellation,
            )
            .await?
            .map(ActiveGraphVectorGenerationSnapshotV1::into_generation))
    }

    /// True while `revision` still names the record state that activated
    /// `generation_id`. Any later vector mutation retires the revision.
    pub async fn active_snapshot_is_current(
        &self,
        revision: u64,
        generation_id: &VectorGenerationIdV1,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<bool, VectorGenerationStoreErrorV1> {
        let snapshot = self.graph.snapshot().map_err(map_graph_error)?;
        let metadata = read_state_metadata(&snapshot, cancellation)?;
        Ok(metadata.revision == revision
            && metadata.active_generation.as_ref() == Some(generation_id))
    }

    pub async fn staged_checkpoint(
        &self,
        build_id: &VectorGenerationBuildIdV1,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Option<VectorProjectionCheckpointV1>, VectorGenerationStoreErrorV1> {
        let snapshot = self.graph.snapshot().map_err(map_graph_error)?;
        read_build_records(&snapshot, build_id, cancellation)
            .map(|records| records.map(|records| records.staged.checkpoint))
    }

    pub async fn active_generation(
        &self,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Option<super::PublishedVectorGenerationV1>, VectorGenerationStoreErrorV1> {
        let snapshot = self.graph.snapshot().map_err(map_graph_error)?;
        let metadata = read_state_metadata(&snapshot, Arc::clone(&cancellation))?;
        metadata
            .active_generation
            .as_ref()
            .map(|generation| {
                read_cataloged_generation_records(&snapshot, generation, cancellation)
            })
            .transpose()
            .map(|records| records.flatten().map(|records| records.generation))
    }

    pub async fn generation(
        &self,
        generation_id: &VectorGenerationIdV1,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Option<super::PublishedVectorGenerationV1>, VectorGenerationStoreErrorV1> {
        let snapshot = self.graph.snapshot().map_err(map_graph_error)?;
        read_cataloged_generation_records(&snapshot, generation_id, cancellation)
            .map(|records| records.map(|records| records.generation))
    }

    pub async fn search_active_vectors(
        &self,
        request: SemanticVectorGraphSearchRequestV1,
    ) -> Result<SemanticVectorGraphSearchResultV1, VectorGenerationStoreErrorV1> {
        check_cancelled(request.cancellation.as_ref())?;
        let snapshot = self.graph.snapshot().map_err(map_graph_error)?;
        self.search_active_vectors_in_snapshot(&snapshot, request)
    }

    fn search_active_vectors_in_snapshot(
        &self,
        snapshot: &GraphSnapshot,
        request: SemanticVectorGraphSearchRequestV1,
    ) -> Result<SemanticVectorGraphSearchResultV1, VectorGenerationStoreErrorV1> {
        let metadata = read_state_metadata(snapshot, Arc::clone(&request.cancellation))?;
        if metadata.active_generation.as_ref() != Some(&request.generation_id) {
            return Err(VectorGenerationStoreErrorV1::StaleActiveGeneration);
        }
        let generation = read_generation_metadata(
            snapshot,
            &request.generation_id,
            Arc::clone(&request.cancellation),
        )?
        .ok_or(VectorGenerationStoreErrorV1::IncompatibleBaseGeneration)?;
        if generation.embedding_key != request.embedding_key
            || generation.source_generation != request.source_generation
            || generation.source_manifest_digest != request.source_manifest_digest
        {
            return Err(VectorGenerationStoreErrorV1::BatchIdentityMismatch);
        }
        let entities = self.generation_entities(
            snapshot,
            &request.generation_id,
            Arc::clone(&request.cancellation),
        )?;
        if u64::try_from(entities.len()).map_err(storage_error)? != metadata.active_row_count {
            return Err(VectorGenerationStoreErrorV1::Corrupt(
                "active semantic vector row count does not match its committed state".to_owned(),
            ));
        }
        let embedding = request.embedding_key.embedding_key();
        let expected_dimension = usize::try_from(embedding.dimensions).map_err(storage_error)?;
        if request.query.len() != expected_dimension
            || request.query.iter().any(|value| !value.is_finite())
        {
            return Err(VectorGenerationStoreErrorV1::InvalidPlan(
                "semantic vector query shape does not match its projection".to_owned(),
            ));
        }
        let mut matches = Vec::with_capacity(entities.len());
        for entity in entities {
            check_cancelled(request.cancellation.as_ref())?;
            if required_string(&entity, GENERATION_ID_PROPERTY)?
                != request.generation_id.as_digest().as_str()
            {
                return Err(VectorGenerationStoreErrorV1::Corrupt(
                    "semantic vector search returned a foreign generation row".to_owned(),
                ));
            }
            let chunk_id = CodeSearchChunkId::try_from(
                required_string(&entity, CHUNK_ID_PROPERTY)?.to_owned(),
            )
            .map_err(storage_error)?;
            let vector = match entity
                .properties
                .get(&search_vector_property(&request.generation_id)?)
            {
                Some(GraphProperty::Vector(vector)) => vector,
                Some(_) => {
                    return Err(VectorGenerationStoreErrorV1::Corrupt(
                        "semantic vector property has the wrong type".to_owned(),
                    ));
                }
                None => {
                    return Err(VectorGenerationStoreErrorV1::Corrupt(
                        "semantic vector property is missing".to_owned(),
                    ));
                }
            };
            if vector.dimension != expected_dimension
                || vector.metric != vector_metric(embedding.metric)
            {
                return Err(VectorGenerationStoreErrorV1::Corrupt(
                    "semantic vector shape does not match its projection".to_owned(),
                ));
            }
            matches.push(SemanticVectorGraphMatchV1 {
                chunk_id,
                distance: exact_vector_distance(
                    vector_metric(embedding.metric),
                    &request.query,
                    &vector.values,
                )?,
            });
        }
        matches.sort_by(|left, right| {
            left.distance
                .total_cmp(&right.distance)
                .then_with(|| left.chunk_id.cmp(&right.chunk_id))
        });
        matches.truncate(request.limit);
        Ok(SemanticVectorGraphSearchResultV1 {
            generation_id: request.generation_id,
            matches,
        })
    }

    pub async fn search_active_hybrid(
        &self,
        request: SemanticHybridGraphSearchRequestV1,
    ) -> Result<SemanticHybridGraphSearchResultV1, VectorGenerationStoreErrorV1> {
        if request.limit == 0
            || !request.vector_weight.is_finite()
            || !request.lexical_weight.is_finite()
            || request.vector_weight < 0.0
            || request.lexical_weight < 0.0
            || request.vector_weight + request.lexical_weight <= 0.0
            || request
                .lexical
                .iter()
                .any(|candidate| !candidate.score.is_finite() || candidate.score < 0.0)
        {
            return Err(VectorGenerationStoreErrorV1::InvalidPlan(
                "semantic hybrid search weights, scores, and limit must be finite and positive"
                    .to_owned(),
            ));
        }
        let cancellation = Arc::clone(&request.vector.cancellation);
        let generation_id = request.vector.generation_id.clone();
        let snapshot = self.graph.snapshot().map_err(map_graph_error)?;
        let vector = self.search_active_vectors_in_snapshot(&snapshot, request.vector)?;
        let eligible =
            self.generation_chunk_ids(&snapshot, &generation_id, Arc::clone(&cancellation))?;
        let lexical_max = request
            .lexical
            .iter()
            .filter(|candidate| eligible.contains(&candidate.chunk_id))
            .map(|candidate| candidate.score)
            .max_by(f64::total_cmp)
            .unwrap_or(0.0);
        let mut fused = BTreeMap::<CodeSearchChunkId, SemanticHybridGraphMatchV1>::new();
        for candidate in vector.matches {
            let score = normalized_vector_score(candidate.distance);
            fused.insert(
                candidate.chunk_id.clone(),
                SemanticHybridGraphMatchV1 {
                    chunk_id: candidate.chunk_id,
                    vector_distance: Some(candidate.distance),
                    lexical_score: None,
                    combined_score: request.vector_weight * score,
                },
            );
        }
        for candidate in request.lexical {
            if !eligible.contains(&candidate.chunk_id) {
                continue;
            }
            let normalized = if lexical_max == 0.0 {
                0.0
            } else {
                candidate.score / lexical_max
            };
            let entry = fused.entry(candidate.chunk_id.clone()).or_insert_with(|| {
                SemanticHybridGraphMatchV1 {
                    chunk_id: candidate.chunk_id.clone(),
                    vector_distance: None,
                    lexical_score: None,
                    combined_score: 0.0,
                }
            });
            if entry
                .lexical_score
                .is_none_or(|prior| candidate.score > prior)
            {
                if let Some(prior) = entry.lexical_score {
                    entry.combined_score -= request.lexical_weight
                        * if lexical_max == 0.0 {
                            0.0
                        } else {
                            prior / lexical_max
                        };
                }
                entry.lexical_score = Some(candidate.score);
                entry.combined_score += request.lexical_weight * normalized;
            }
        }
        check_cancelled(cancellation.as_ref())?;
        let mut matches = fused.into_values().collect::<Vec<_>>();
        matches.sort_by(|left, right| {
            right
                .combined_score
                .total_cmp(&left.combined_score)
                .then_with(|| left.chunk_id.cmp(&right.chunk_id))
        });
        matches.truncate(request.limit);
        Ok(SemanticHybridGraphSearchResultV1 {
            generation_id,
            matches,
        })
    }

    fn generation_chunk_ids(
        &self,
        snapshot: &GraphSnapshot,
        generation_id: &VectorGenerationIdV1,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<BTreeSet<CodeSearchChunkId>, VectorGenerationStoreErrorV1> {
        let entities =
            self.generation_entities(snapshot, generation_id, Arc::clone(&cancellation))?;
        let mut chunks = BTreeSet::new();
        for entity in entities {
            check_cancelled(cancellation.as_ref())?;
            if required_string(&entity, GENERATION_ID_PROPERTY)?
                != generation_id.as_digest().as_str()
            {
                return Err(VectorGenerationStoreErrorV1::Corrupt(
                    "semantic vector search row names a foreign generation".to_owned(),
                ));
            }
            let chunk_id = CodeSearchChunkId::try_from(
                required_string(&entity, CHUNK_ID_PROPERTY)?.to_owned(),
            )
            .map_err(storage_error)?;
            if !chunks.insert(chunk_id) {
                return Err(VectorGenerationStoreErrorV1::Corrupt(
                    "semantic vector generation contains duplicate search chunks".to_owned(),
                ));
            }
        }
        Ok(chunks)
    }

    fn generation_entities(
        &self,
        snapshot: &GraphSnapshot,
        generation_id: &VectorGenerationIdV1,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Vec<tracedecay_graph_db::GraphEntity>, VectorGenerationStoreErrorV1> {
        let label = generation_label(generation_id)?;
        let records = read_cataloged_generation_records(snapshot, generation_id, cancellation)?
            .ok_or(VectorGenerationStoreErrorV1::IncompatibleBaseGeneration)?;
        if records.generation.vectors().len() > MAX_RESIDENT_VECTOR_ROWS {
            return Err(VectorGenerationStoreErrorV1::Unavailable(format!(
                "semantic vector generation exceeds the resident row ceiling of {MAX_RESIDENT_VECTOR_ROWS}"
            )));
        }
        Ok(records
            .entities
            .into_values()
            .filter(|entity| entity.labels.contains(&label))
            .collect())
    }

    pub fn acquire_active_generation_publication_guard(
        &self,
        expected_watermark: &GraphWatermark,
        expected_generation: &VectorGenerationIdV1,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<ActiveVectorGenerationPublicationGuardV1, VectorGenerationStoreErrorV1> {
        check_cancelled(cancellation.as_ref())?;
        let snapshot = self.graph.snapshot().map_err(map_graph_error)?;
        let metadata = read_state_metadata(&snapshot, Arc::clone(&cancellation))?;
        if &metadata.watermark != expected_watermark
            || metadata.active_generation.as_ref() != Some(expected_generation)
        {
            return Err(VectorGenerationStoreErrorV1::StaleActiveGeneration);
        }
        let records = read_cataloged_generation_records(
            &snapshot,
            expected_generation,
            Arc::clone(&cancellation),
        )?
        .ok_or_else(|| {
            VectorGenerationStoreErrorV1::Corrupt(
                "active semantic vector generation records are missing".to_owned(),
            )
        })?;
        if u64::try_from(records.generation.vectors().len()).map_err(storage_error)?
            != metadata.active_row_count
        {
            return Err(VectorGenerationStoreErrorV1::Corrupt(
                "active semantic vector row count does not match its committed state".to_owned(),
            ));
        }
        check_cancelled(cancellation.as_ref())?;
        Ok(ActiveVectorGenerationPublicationGuardV1 {
            _snapshot: snapshot,
            watermark: metadata.watermark,
            generation_id: expected_generation.clone(),
            projection_key: records.generation.projection_key().clone(),
            source_generation: records.generation.source_generation().clone(),
            source_manifest_digest: records.generation.source_manifest_digest().clone(),
            embedding_key: records.generation.embedding_key().clone(),
        })
    }

    pub async fn active_resident_plan(
        &self,
        expected_generation: &VectorGenerationIdV1,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Option<ActiveVectorResidentPlanV1>, VectorGenerationStoreErrorV1> {
        check_cancelled(cancellation.as_ref())?;
        let snapshot = self.graph.snapshot().map_err(map_graph_error)?;
        let metadata = read_state_metadata(&snapshot, Arc::clone(&cancellation))?;
        if metadata.active_generation.as_ref() != Some(expected_generation) {
            return Ok(None);
        }
        let generation =
            read_generation_metadata(&snapshot, expected_generation, Arc::clone(&cancellation))?
                .ok_or_else(|| {
                    VectorGenerationStoreErrorV1::Corrupt(
                        "active semantic vector generation metadata is missing".to_owned(),
                    )
                })?;
        let rows =
            self.generation_entities(&snapshot, expected_generation, Arc::clone(&cancellation))?;
        let row_count = u64::try_from(rows.len()).map_err(storage_error)?;
        if row_count != metadata.active_row_count {
            return Err(VectorGenerationStoreErrorV1::Corrupt(
                "active semantic vector row count does not match its committed state".to_owned(),
            ));
        }
        let dimensions = u64::from(generation.embedding_key.embedding_key().dimensions);
        let vector_bytes = dimensions
            .checked_mul(u64::try_from(size_of::<f32>()).map_err(storage_error)?)
            .ok_or_else(resident_size_overflow)?;
        let per_row = u64::try_from(size_of::<ResidentVectorRowV1>())
            .map_err(storage_error)?
            .checked_add(1_024)
            .and_then(|bytes| bytes.checked_add(vector_bytes))
            .ok_or_else(resident_size_overflow)?;
        let retained_bytes = row_count
            .checked_mul(per_row)
            .ok_or_else(resident_size_overflow)?;
        let hydration_peak_bytes = retained_bytes
            .checked_mul(2)
            .and_then(|bytes| {
                row_count
                    .checked_mul(4_096)
                    .and_then(|overhead| bytes.checked_add(overhead))
            })
            .ok_or_else(resident_size_overflow)?;
        drop(snapshot);
        check_cancelled(cancellation.as_ref())?;
        Ok(Some(ActiveVectorResidentPlanV1 {
            watermark: metadata.watermark,
            generation_id: expected_generation.clone(),
            retained_bytes,
            hydration_peak_bytes,
        }))
    }

    pub async fn read_resident_generation_for(
        &self,
        plan: &ActiveVectorResidentPlanV1,
        embedding_key: &AdmittedEmbeddingProjectionKeyV1,
        source_generation: &CodeGenerationId,
        source_manifest_digest: &ManifestDigest,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Option<ResidentVectorGenerationV1>, VectorGenerationStoreErrorV1> {
        check_cancelled(cancellation.as_ref())?;
        let snapshot = self.graph.snapshot().map_err(map_graph_error)?;
        let metadata = read_state_metadata(&snapshot, Arc::clone(&cancellation))?;
        if metadata.watermark != plan.watermark {
            return Ok(None);
        }
        let generation_id = &plan.generation_id;
        let Some(generation) =
            read_generation_metadata(&snapshot, generation_id, Arc::clone(&cancellation))?
        else {
            return Ok(None);
        };
        if metadata.active_generation.as_ref() != Some(generation_id)
            || &generation.embedding_key != embedding_key
            || &generation.source_generation != source_generation
            || &generation.source_manifest_digest != source_manifest_digest
        {
            return Ok(None);
        }
        let entities =
            self.generation_entities(&snapshot, generation_id, Arc::clone(&cancellation))?;
        if u64::try_from(entities.len()).map_err(storage_error)? != metadata.active_row_count {
            return Err(VectorGenerationStoreErrorV1::Corrupt(
                "active semantic vector row count does not match its committed state".to_owned(),
            ));
        }
        let expected_dimension =
            usize::try_from(embedding_key.embedding_key().dimensions).map_err(storage_error)?;
        let expected_metric = vector_metric(embedding_key.embedding_key().metric);
        let vector_property = search_vector_property(generation_id)?;
        let mut rows = Vec::with_capacity(entities.len());
        for entity in entities {
            check_cancelled(cancellation.as_ref())?;
            if required_string(&entity, GENERATION_ID_PROPERTY)?
                != generation_id.as_digest().as_str()
            {
                return Err(VectorGenerationStoreErrorV1::Corrupt(
                    "resident vector row names a foreign generation".to_owned(),
                ));
            }
            let vector = match entity.properties.get(&vector_property) {
                Some(GraphProperty::Vector(vector)) => vector,
                Some(_) => {
                    return Err(VectorGenerationStoreErrorV1::Corrupt(
                        "resident vector property has the wrong type".to_owned(),
                    ));
                }
                None => {
                    return Err(VectorGenerationStoreErrorV1::Corrupt(
                        "resident vector property is missing".to_owned(),
                    ));
                }
            };
            if vector.dimension != expected_dimension || vector.metric != expected_metric {
                return Err(VectorGenerationStoreErrorV1::Corrupt(
                    "resident vector shape does not match its projection".to_owned(),
                ));
            }
            rows.push(ResidentVectorRowV1 {
                chunk_id: CodeSearchChunkId::try_from(
                    required_string(&entity, CHUNK_ID_PROPERTY)?.to_owned(),
                )
                .map_err(storage_error)?,
                values: vector.values.clone().into_boxed_slice(),
            });
        }
        rows.sort_by(|left, right| left.chunk_id.cmp(&right.chunk_id));
        if rows
            .windows(2)
            .any(|pair| pair[0].chunk_id == pair[1].chunk_id)
        {
            return Err(VectorGenerationStoreErrorV1::Corrupt(
                "resident vector generation contains duplicate chunks".to_owned(),
            ));
        }
        let retained_bytes = measured_resident_bytes(&rows)?;
        drop(snapshot);
        check_cancelled(cancellation.as_ref())?;
        Ok(Some(ResidentVectorGenerationV1 {
            generation_id: generation_id.clone(),
            projection_key: generation.projection_key,
            source_generation: source_generation.clone(),
            source_manifest_digest: source_manifest_digest.clone(),
            rows,
            retained_bytes,
        }))
    }
}

fn exact_vector_distance(
    metric: VectorMetric,
    query: &[f32],
    document: &[f32],
) -> Result<f64, VectorGenerationStoreErrorV1> {
    if query.is_empty()
        || query.len() != document.len()
        || query.iter().chain(document).any(|value| !value.is_finite())
    {
        return Err(VectorGenerationStoreErrorV1::Corrupt(
            "semantic vector exact-flat inputs are invalid".to_owned(),
        ));
    }
    let distance = match metric {
        VectorMetric::Cosine => {
            let (mut dot, mut query_norm, mut document_norm) = (0.0_f64, 0.0_f64, 0.0_f64);
            for (&query_value, &document_value) in query.iter().zip(document) {
                let query_value = f64::from(query_value);
                let document_value = f64::from(document_value);
                dot += query_value * document_value;
                query_norm += query_value * query_value;
                document_norm += document_value * document_value;
            }
            if query_norm == 0.0 || document_norm == 0.0 {
                return Err(VectorGenerationStoreErrorV1::Corrupt(
                    "cosine distance is undefined for a zero-norm vector".to_owned(),
                ));
            }
            1.0 - (dot / (query_norm.sqrt() * document_norm.sqrt())).clamp(-1.0, 1.0)
        }
        VectorMetric::DotProduct => -query
            .iter()
            .zip(document)
            .map(|(&left, &right)| f64::from(left) * f64::from(right))
            .sum::<f64>(),
        VectorMetric::Euclidean => query
            .iter()
            .zip(document)
            .map(|(&left, &right)| {
                let delta = f64::from(left) - f64::from(right);
                delta * delta
            })
            .sum::<f64>()
            .sqrt(),
    };
    if distance.is_finite() {
        Ok(distance)
    } else {
        Err(VectorGenerationStoreErrorV1::Corrupt(
            "semantic vector exact-flat distance is not finite".to_owned(),
        ))
    }
}
