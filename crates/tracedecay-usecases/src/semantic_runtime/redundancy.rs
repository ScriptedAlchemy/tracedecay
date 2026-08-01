//! Generation-bound semantic-vector adapter for redundancy classification.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use tracedecay_domain::{
    CodeGenerationId, CodeSearchChunkGrainV1, EmbeddingMetricV1, ManifestDigest, canonical_sha256,
};

use crate::config::retrieval::SemanticCompatibilityPinsV1;
use crate::store::vector_generations::DatabaseVectorGenerationStoreV1;
use tracedecay_code_index::production::CodeIndexPublishedGenerationV1;
use tracedecay_runtime_core::db::Database;

use super::{CommittedRetrievalProfileStateV1, project_semantic_generation_pointer};

const SEMANTIC_DISTANCE_SCALE: f64 = 1_000_000_000.0;
const MAX_COSINE_DISTANCE_MICROS: i64 = 2_000_000_000;

#[derive(Clone, Debug, PartialEq)]
pub struct SemanticRedundancyVectorV1 {
    pub file_path: String,
    pub qualified_name: String,
    pub values: Vec<f32>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SemanticRedundancyGenerationV1 {
    pub vector_generation: String,
    pub source_generation: String,
    pub projection_key: String,
    pub profile: SemanticRedundancyProfileV1,
    pub vectors: Vec<SemanticRedundancyVectorV1>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticRedundancyProfileV1 {
    pub scope_digest: String,
    pub accepted_profile_digest: String,
    pub calibration_profile_id: String,
    pub calibration_digest: String,
    pub redundancy_profile_digest: String,
    pub maximum_distance_micros: i64,
}

impl SemanticRedundancyProfileV1 {
    pub(crate) fn distance_micros(&self, cosine: f64) -> Option<i64> {
        if !cosine.is_finite() || !(-1.0..=1.0).contains(&cosine) {
            return None;
        }
        let scaled = ((1.0 - cosine) * SEMANTIC_DISTANCE_SCALE).round();
        (scaled >= 0.0 && scaled <= MAX_COSINE_DISTANCE_MICROS as f64).then_some(scaled as i64)
    }

    pub fn accepts(&self, cosine: f64) -> Option<i64> {
        let distance = self.distance_micros(cosine)?;
        (distance <= self.maximum_distance_micros).then_some(distance)
    }

    /// Half-width, in normalized-vector coordinate units, of the smallest
    /// window that still contains every pair this profile could accept.
    ///
    /// For unit vectors `u`, `v` we have `‖u − v‖² = 2(1 − cos)`, and every
    /// single coordinate obeys `|u_k − v_k| ≤ ‖u − v‖`. A pair is accepted only
    /// when `round((1 − cos)·SCALE) ≤ maximum_distance_micros`, which requires
    /// `(1 − cos) ≤ (maximum_distance_micros + 0.5)/SCALE` (the `+0.5` bounds
    /// the rounding). Substituting yields a per-coordinate bound of
    /// `sqrt(2·(maximum_distance_micros + 0.5)/SCALE)`.
    ///
    /// Sorting normalized vectors by any one coordinate and comparing only
    /// entries within this half-width therefore excludes **no** acceptable pair
    /// (perfect recall): the returned window is a necessary condition on every
    /// accepted pair, never a sufficient one, so callers must still re-check
    /// [`accepts`] on each surviving candidate. A tiny epsilon is added for
    /// floating-point slack; the value saturates at `2.0` (a window that spans
    /// the whole normalized range, i.e. no pruning) for permissive profiles.
    pub fn cosine_projection_window(&self) -> f64 {
        let allowed = (self.maximum_distance_micros as f64 + 0.5) / SEMANTIC_DISTANCE_SCALE;
        if allowed <= 0.0 {
            return 0.0;
        }
        ((2.0 * allowed).sqrt() + 1e-9).min(2.0)
    }
}

#[derive(Clone)]
struct SemanticRedundancyAuthorityV1 {
    scope_digest: ManifestDigest,
    accepted_profile_digest: ManifestDigest,
    pins: SemanticCompatibilityPinsV1,
    calibration_digest: ManifestDigest,
    redundancy_profile_digest: ManifestDigest,
}

struct RetainedProjectGenerationsV1 {
    latest: CodeGenerationId,
    generations: BTreeMap<CodeGenerationId, CodeIndexPublishedGenerationV1>,
}

fn retained_generations() -> &'static Mutex<BTreeMap<PathBuf, RetainedProjectGenerationsV1>> {
    static GENERATIONS: OnceLock<Mutex<BTreeMap<PathBuf, RetainedProjectGenerationsV1>>> =
        OnceLock::new();
    GENERATIONS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn retained_authorities() -> &'static Mutex<BTreeMap<PathBuf, SemanticRedundancyAuthorityV1>> {
    static AUTHORITIES: OnceLock<Mutex<BTreeMap<PathBuf, SemanticRedundancyAuthorityV1>>> =
        OnceLock::new();
    AUTHORITIES.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// Retain the immutable code-generation bindings needed to interpret active
/// vector rows. The semantic schedule hook calls this before enqueueing a
/// generation; reads still admit only the atomically current vector/source
/// identity.
pub(crate) fn register_project_semantic_redundancy_generation(
    project_root: PathBuf,
    generation: CodeIndexPublishedGenerationV1,
) {
    let current =
        project_semantic_generation_pointer(&project_root).map(|pointer| pointer.source_generation);
    let incoming = generation.manifest().generation_id.clone();
    let mut retained = retained_generations()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let project = retained
        .entry(project_root)
        .or_insert_with(|| RetainedProjectGenerationsV1 {
            latest: incoming.clone(),
            generations: BTreeMap::new(),
        });
    project.latest = incoming.clone();
    project
        .generations
        .retain(|source, _| current.as_ref() == Some(source) || source == &incoming);
    project.generations.insert(incoming, generation);
}

pub fn register_project_semantic_redundancy_authority(
    project_root: PathBuf,
    committed: &CommittedRetrievalProfileStateV1,
) -> bool {
    let Some(authority) = redundancy_authority_from_committed(committed) else {
        retained_authorities()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&project_root);
        return false;
    };
    retained_authorities()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(project_root, authority);
    true
}

pub fn unregister_project_semantic_redundancy_authority(project_root: &Path) {
    retained_authorities()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(project_root);
}

pub(crate) fn unregister_project_semantic_redundancy_generation(project_root: &Path) {
    retained_generations()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(project_root);
    unregister_project_semantic_redundancy_authority(project_root);
}

/// Read only a complete active cosine generation whose source identity equals
/// the semantic runtime's atomically current pointer.
pub async fn project_semantic_redundancy_generation(
    project_root: &Path,
    database: &Database,
) -> Option<SemanticRedundancyGenerationV1> {
    let pointer = project_semantic_generation_pointer(project_root)?;
    let authority = retained_authorities()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(project_root)
        .cloned()?;
    if pointer.generation != authority.pins.vector_generation_id
        || pointer.projection_key != *authority.pins.projection.projection_key()
    {
        return None;
    }
    let source_generation = pointer.source_generation;
    let code = {
        let retained = retained_generations()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let project = retained.get(project_root)?;
        if project.latest != source_generation {
            return None;
        }
        project.generations.get(&source_generation).cloned()?
    };
    let vectors = DatabaseVectorGenerationStoreV1::read_active_generation(database)
        .await
        .ok()??;
    if vectors.source_generation() != &source_generation
        || vectors.generation_id() != &authority.pins.vector_generation_id
        || vectors.embedding_key() != &authority.pins.projection
        || vectors.embedding_key().embedding_key().metric != EmbeddingMetricV1::Cosine
    {
        return None;
    }

    let chunks = code
        .chunks()
        .chunks()
        .iter()
        .map(|chunk| (&chunk.id, chunk))
        .collect::<HashMap<_, _>>();
    let files = code
        .snapshot()
        .files
        .iter()
        .map(|file| (&file.file_occurrence_id, file.logical_path.as_str()))
        .collect::<HashMap<_, _>>();
    let symbols = code
        .symbols()
        .symbols
        .iter()
        .map(|symbol| (&symbol.occurrence, symbol.qualified_name.as_str()))
        .collect::<HashMap<_, _>>();
    let mut admitted = Vec::new();
    for (chunk_id, vector) in vectors.vectors() {
        let chunk = chunks.get(chunk_id)?;
        if vector.source_generation != source_generation
            || &vector.projection_key != vectors.projection_key()
            || vector.chunk_digest != chunk.content_digest
        {
            return None;
        }
        if !matches!(
            chunk.anchor.grain,
            CodeSearchChunkGrainV1::SymbolBody | CodeSearchChunkGrainV1::SymbolMember
        ) {
            continue;
        }
        let symbol = chunk.anchor.symbol_occurrence_id.as_ref()?;
        admitted.push(SemanticRedundancyVectorV1 {
            file_path: (*files.get(&chunk.anchor.file_occurrence_id)?).to_owned(),
            qualified_name: (*symbols.get(symbol)?).to_owned(),
            values: vector.values.clone(),
        });
    }
    Some(SemanticRedundancyGenerationV1 {
        vector_generation: vectors.generation_id().as_digest().as_str().to_owned(),
        source_generation: source_generation.as_str().to_owned(),
        projection_key: vectors.projection_key().profile_digest.as_str().to_owned(),
        profile: SemanticRedundancyProfileV1 {
            scope_digest: authority.scope_digest.as_str().to_owned(),
            accepted_profile_digest: authority.accepted_profile_digest.as_str().to_owned(),
            calibration_profile_id: authority
                .pins
                .calibration
                .calibration_profile_id
                .as_str()
                .to_owned(),
            calibration_digest: authority.calibration_digest.as_str().to_owned(),
            redundancy_profile_digest: authority.redundancy_profile_digest.as_str().to_owned(),
            maximum_distance_micros: authority.pins.calibration.maximum_distance_micros,
        },
        vectors: admitted,
    })
}

fn redundancy_authority_from_committed(
    committed: &CommittedRetrievalProfileStateV1,
) -> Option<SemanticRedundancyAuthorityV1> {
    committed.scope.validate().ok()?;
    let accepted = committed.state.active();
    let pins = accepted.compatibility().semantic.as_ref()?;
    let activation = committed.current_activation.as_ref()?;
    if &activation.compatibility != pins
        || activation.receipt.activated_generation != pins.vector_generation_id
        || accepted
            .profile()
            .calibrations
            .get(&tracedecay_domain::RetrieverKind::Semantic)
            != Some(&pins.calibration.calibration_profile_id)
        || pins.calibration.projection_key != *pins.projection.projection_key()
        || pins.calibration.vector_generation != pins.vector_generation_id
        || !(0..=MAX_COSINE_DISTANCE_MICROS).contains(&pins.calibration.maximum_distance_micros)
    {
        return None;
    }
    let calibration_digest = pins.calibration.canonical_digest().ok()?;
    let accepted_profile_digest = accepted.profile_digest().clone();
    let redundancy_profile_digest = canonical_sha256(&(
        "tracedecay.semantic-redundancy-profile.v1",
        &accepted_profile_digest,
        &calibration_digest,
        pins.calibration.maximum_distance_micros,
    ))
    .ok()?;
    Some(SemanticRedundancyAuthorityV1 {
        scope_digest: committed.scope.scope_digest.clone(),
        accepted_profile_digest,
        pins: pins.clone(),
        calibration_digest,
        redundancy_profile_digest,
    })
}
