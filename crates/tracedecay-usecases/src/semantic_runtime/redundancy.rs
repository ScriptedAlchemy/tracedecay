//! Generation-bound semantic-vector adapter for redundancy classification.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use tracedecay_domain::configuration::ConfigurationRevisionId;
use tracedecay_domain::{
    CodeGenerationId, CodeSearchChunkGrainV1, EmbeddingMetricV1, ManifestDigest, canonical_sha256,
};

use crate::config::retrieval::SemanticCompatibilityPinsV1;
use tracedecay_code_index::production::CodeIndexPublishedGenerationV1;

use super::{
    CommittedRetrievalProfileStateV1, SemanticRetainedVectorGenerationsV1,
    project_semantic_production_runtime,
};

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

#[derive(Clone, Debug, PartialEq, Eq)]
struct SemanticRedundancyAuthorityV1 {
    scope_digest: ManifestDigest,
    accepted_profile_digest: ManifestDigest,
    pins: SemanticCompatibilityPinsV1,
    calibration_digest: ManifestDigest,
    redundancy_profile_digest: ManifestDigest,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedSemanticRedundancyAuthorityV1 {
    revision: ConfigurationRevisionId,
    roots: SemanticRetainedVectorGenerationsV1,
    authority: Option<SemanticRedundancyAuthorityV1>,
}

impl PreparedSemanticRedundancyAuthorityV1 {
    pub fn has_active_authority(&self) -> bool {
        self.authority.is_some()
    }

    pub fn configuration_revision(&self) -> &ConfigurationRevisionId {
        &self.revision
    }
}

struct RetainedProjectGenerationsV1 {
    latest: CodeGenerationId,
    generations: BTreeMap<CodeGenerationId, CodeIndexPublishedGenerationV1>,
}

struct SemanticProjectRedundancyStateV1 {
    revision: ConfigurationRevisionId,
    roots: SemanticRetainedVectorGenerationsV1,
    authority: Option<SemanticRedundancyAuthorityV1>,
}

fn retained_generations() -> &'static Mutex<BTreeMap<PathBuf, RetainedProjectGenerationsV1>> {
    static GENERATIONS: OnceLock<Mutex<BTreeMap<PathBuf, RetainedProjectGenerationsV1>>> =
        OnceLock::new();
    GENERATIONS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn redundancy_states() -> &'static Mutex<BTreeMap<PathBuf, SemanticProjectRedundancyStateV1>> {
    static STATES: OnceLock<Mutex<BTreeMap<PathBuf, SemanticProjectRedundancyStateV1>>> =
        OnceLock::new();
    STATES.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn activation_gates() -> &'static Mutex<BTreeMap<PathBuf, std::sync::Arc<Mutex<()>>>> {
    static GATES: OnceLock<Mutex<BTreeMap<PathBuf, std::sync::Arc<Mutex<()>>>>> = OnceLock::new();
    GATES.get_or_init(|| Mutex::new(BTreeMap::new()))
}

pub fn project_semantic_activation_gate(project_root: &Path) -> std::sync::Arc<Mutex<()>> {
    activation_gates()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .entry(project_root.to_path_buf())
        .or_insert_with(|| std::sync::Arc::new(Mutex::new(())))
        .clone()
}

/// Exact semantic compatibility selected by the committed retrieval profile.
///
/// The process-local embedding handle is only an observed cache and must not
/// select a generation independently of this durable activation projection.
pub fn project_committed_semantic_pins(project_root: &Path) -> Option<SemanticCompatibilityPinsV1> {
    let activation = project_semantic_activation_gate(project_root);
    let _activation = activation
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    redundancy_states()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(project_root)
        .and_then(|state| state.authority.as_ref())
        .map(|authority| authority.pins.clone())
}

pub fn project_semantic_retained_vector_generations(
    project_root: &Path,
) -> Option<SemanticRetainedVectorGenerationsV1> {
    let activation = project_semantic_activation_gate(project_root);
    let _activation = activation
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    redundancy_states()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(project_root)
        .map(|state| state.roots.clone())
}

pub fn project_semantic_redundancy_revision(
    project_root: &Path,
) -> Option<ConfigurationRevisionId> {
    let activation = project_semantic_activation_gate(project_root);
    let _activation = activation
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    redundancy_states()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(project_root)
        .map(|state| state.revision.clone())
}

/// Retain the immutable code-generation bindings needed to interpret active
/// vector rows. The semantic schedule hook calls this before enqueueing a
/// generation; reads remain selected by committed compatibility pins.
pub(crate) fn register_project_semantic_redundancy_generation(
    project_root: PathBuf,
    generation: CodeIndexPublishedGenerationV1,
) {
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
    project.generations.insert(incoming, generation);
}

/// Prune process-local code-generation handles after the daemon retention
/// owner has completed its durable, revision-bound liveness proof.
///
/// This is a maintenance mutation, not a readable-source inventory operation.
/// Doctor and other diagnostic readers must never call it.
pub fn retain_project_semantic_code_sources(
    project_root: &Path,
    configured_sources: &std::collections::BTreeSet<CodeGenerationId>,
) {
    let mut retained = retained_generations()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(project) = retained.get_mut(project_root) {
        let latest = project.latest.clone();
        project
            .generations
            .retain(|source, _| source == &latest || configured_sources.contains(source));
    }
}

pub fn project_semantic_retained_code_generation(
    project_root: &Path,
    source_generation: &CodeGenerationId,
) -> Option<CodeIndexPublishedGenerationV1> {
    retained_generations()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(project_root)?
        .generations
        .get(source_generation)
        .cloned()
}

pub fn prepare_project_semantic_redundancy_authority(
    committed: &CommittedRetrievalProfileStateV1,
) -> PreparedSemanticRedundancyAuthorityV1 {
    PreparedSemanticRedundancyAuthorityV1 {
        revision: committed.state.configuration_revision().clone(),
        roots: SemanticRetainedVectorGenerationsV1::from_profile_state(&committed.state),
        authority: redundancy_authority_from_committed(committed),
    }
}

pub fn commit_project_initial_semantic_roots(
    project_root: PathBuf,
    state: &crate::config::retrieval::RetrievalProfileStateV1,
) -> bool {
    if !state.audit().is_empty() || state.active().compatibility().semantic.is_some() {
        return false;
    }
    let prepared = PreparedSemanticRedundancyAuthorityV1 {
        revision: state.configuration_revision().clone(),
        roots: SemanticRetainedVectorGenerationsV1::from_profile_state(state),
        authority: None,
    };
    commit_project_semantic_redundancy_authority(project_root, &prepared, false);
    true
}

pub fn commit_project_semantic_redundancy_authority(
    project_root: PathBuf,
    prepared: &PreparedSemanticRedundancyAuthorityV1,
    install_authority: bool,
) {
    let activation = project_semantic_activation_gate(&project_root);
    let _activation = activation
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    commit_project_semantic_redundancy_authority_under_gate(
        project_root,
        prepared,
        install_authority,
    );
}

pub fn commit_project_semantic_redundancy_authority_under_gate(
    project_root: PathBuf,
    prepared: &PreparedSemanticRedundancyAuthorityV1,
    install_authority: bool,
) {
    let state = SemanticProjectRedundancyStateV1 {
        revision: prepared.revision.clone(),
        roots: prepared.roots.clone(),
        authority: prepared.authority.clone().filter(|_| install_authority),
    };
    redundancy_states()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(project_root, state);
}

pub(crate) fn unregister_project_semantic_redundancy_generation(project_root: &Path) {
    let activation = project_semantic_activation_gate(project_root);
    let _activation = activation
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    retained_generations()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(project_root);
    redundancy_states()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(project_root);
    activation_gates()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(project_root);
}

/// Read only the exact complete cosine generation selected by committed pins.
pub async fn project_semantic_redundancy_generation(
    project_root: &Path,
) -> Option<SemanticRedundancyGenerationV1> {
    let authority = {
        let activation = project_semantic_activation_gate(project_root);
        let _activation = activation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        redundancy_states()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(project_root)
            .and_then(|state| state.authority.clone())?
    };
    let vectors = project_semantic_production_runtime(project_root)?
        .active_vector_generation(&authority.pins)
        .await?;
    if vectors.generation_id() != &authority.pins.vector_generation_id
        || vectors.embedding_key() != &authority.pins.projection
        || vectors.embedding_key().embedding_key().metric != EmbeddingMetricV1::Cosine
    {
        return None;
    }
    let source_generation = vectors.source_generation().clone();
    let code = retained_generations()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(project_root)?
        .generations
        .get(&source_generation)
        .cloned()?;

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

#[cfg(test)]
mod tests {
    use super::project_semantic_activation_gate;
    use std::path::Path;

    #[test]
    fn activation_in_one_project_does_not_block_another_projects_reads() {
        let project_a = project_semantic_activation_gate(Path::new("/project-a"));
        let project_b = project_semantic_activation_gate(Path::new("/project-b"));
        let _activation_a = project_a.lock().expect("project A activation gate");

        assert!(
            project_b.try_lock().is_ok(),
            "project B redundancy reads must not share project A's activation gate"
        );
        assert!(
            project_a.try_lock().is_err(),
            "the exact project gate still serializes its own activation and reads"
        );
    }
}
