//! Generation-bound semantic-vector adapter for redundancy classification.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use tracedecay_domain::configuration::ConfigurationRevisionId;
use tracedecay_domain::{
    CodeGenerationId, CodeSearchChunkGrainV1, EmbeddingMetricV1, ManifestDigest, canonical_sha256,
};

use crate::config::retrieval::SemanticCompatibilityPinsV1;
use tracedecay_code_index::production::CodeIndexPublishedGenerationV1;

use super::{
    CommittedRetrievalProfileStateV1, SemanticActivationReceiptV1,
    SemanticCurrentLinkedActivationV1, SemanticRetainedVectorGenerationsV1,
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
    activation: SemanticCurrentLinkedActivationV1,
    calibration_digest: ManifestDigest,
    redundancy_profile_digest: ManifestDigest,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedSemanticRedundancyAuthorityV1 {
    revision: ConfigurationRevisionId,
    roots: SemanticRetainedVectorGenerationsV1,
    authority: Option<SemanticRedundancyAuthorityV1>,
    /// Durable Ready receipt from the committed linked activation. Present
    /// only when [`authority`] validated that exact receipt; remount must
    /// reattach it onto the public Ready projection instead of synthesizing
    /// one from the scheduler pointer.
    activation_receipt: Option<SemanticActivationReceiptV1>,
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
    /// Last durable vector-readable source set observed by
    /// [`retain_project_semantic_code_sources`]. Until that maintenance
    /// proof runs, only `latest` is kept so publish supersession cannot
    /// accumulate every decoded generation for the life of the daemon.
    configured_sources: BTreeSet<CodeGenerationId>,
    generations: BTreeMap<CodeGenerationId, Arc<CodeIndexPublishedGenerationV1>>,
}

fn prune_retained_project_generations(project: &mut RetainedProjectGenerationsV1) {
    let latest = project.latest.clone();
    project
        .generations
        .retain(|source, _| source == &latest || project.configured_sources.contains(source));
}

struct SemanticProjectRedundancyStateV1 {
    revision: ConfigurationRevisionId,
    roots: SemanticRetainedVectorGenerationsV1,
    authority: Option<SemanticRedundancyAuthorityV1>,
    activation_receipt: Option<SemanticActivationReceiptV1>,
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
        .map(|authority| authority.activation.compatibility.clone())
}

/// Durable Ready receipt last installed for this project, if any.
///
/// Process-local only: unmount drops it. Restart remount must recommit the
/// exact receipt from the configuration store; a scheduler `Current`
/// pointer alone is not Ready.
pub fn project_semantic_activation_receipt(
    project_root: &Path,
) -> Option<SemanticActivationReceiptV1> {
    with_project_semantic_activation_receipt(project_root, |receipt| receipt)
}

pub(crate) fn with_project_semantic_activation_receipt<T>(
    project_root: &Path,
    reader: impl FnOnce(Option<SemanticActivationReceiptV1>) -> T,
) -> T {
    let activation = project_semantic_activation_gate(project_root);
    let _activation = activation
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let receipt = redundancy_states()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(project_root)
        .and_then(|state| state.activation_receipt.clone());
    reader(receipt)
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
    generation: Arc<CodeIndexPublishedGenerationV1>,
) {
    let incoming = generation.manifest().generation_id.clone();
    let mut retained = retained_generations()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let project = retained
        .entry(project_root)
        .or_insert_with(|| RetainedProjectGenerationsV1 {
            latest: incoming.clone(),
            configured_sources: BTreeSet::new(),
            generations: BTreeMap::new(),
        });
    project.latest = incoming.clone();
    project.generations.insert(incoming, generation);
    prune_retained_project_generations(project);
    record_retained_generation_count(&retained);
}

/// Prune process-local code-generation handles after the daemon retention
/// owner has completed its durable, revision-bound liveness proof.
///
/// This is a maintenance mutation, not a readable-source inventory operation.
/// Doctor and other diagnostic readers must never call it.
pub fn retain_project_semantic_code_sources(
    project_root: &Path,
    configured_sources: &BTreeSet<CodeGenerationId>,
) {
    let mut retained = retained_generations()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(project) = retained.get_mut(project_root) {
        project.configured_sources = configured_sources.clone();
        prune_retained_project_generations(project);
    }
    record_retained_generation_count(&retained);
}

pub fn project_semantic_retained_code_generation(
    project_root: &Path,
    source_generation: &CodeGenerationId,
) -> Option<Arc<CodeIndexPublishedGenerationV1>> {
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
    let authority = redundancy_authority_from_committed(committed);
    let activation_receipt = authority.as_ref().and_then(|_| {
        committed
            .current_activation
            .as_ref()
            .map(|activation| activation.receipt.clone())
    });
    PreparedSemanticRedundancyAuthorityV1 {
        revision: committed.state.configuration_revision().clone(),
        roots: SemanticRetainedVectorGenerationsV1::from_profile_state(&committed.state),
        authority,
        activation_receipt,
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
        activation_receipt: None,
    };
    commit_project_semantic_redundancy_authority(project_root, &prepared, false);
    true
}

/// Seat the known-empty retention authority for a project whose scope has no
/// published retrieval-profile state at all.
///
/// A fresh project store bootstraps its query-only retrieval profile lazily, on
/// the first semantic configuration operation, so project open frequently
/// resolves no committed state at all. Leaving the process-local record absent
/// makes every retention and Doctor read answer `None`, which means "this
/// project is not mounted" — indistinguishable from a mounted project whose
/// committed profile pins nothing. The truthful answer for a mounted project
/// without a profile is the known-empty set at the current configuration
/// revision.
///
/// No activation authority and no Ready receipt are installed: only a committed
/// linked activation can produce those. An existing record always wins, so this
/// can never demote a real committed authority to the empty projection.
pub fn commit_project_absent_semantic_roots(
    project_root: PathBuf,
    revision: ConfigurationRevisionId,
) -> bool {
    let activation = project_semantic_activation_gate(&project_root);
    let _activation = activation
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut states = redundancy_states()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if states.contains_key(&project_root) {
        return false;
    }
    states.insert(
        project_root,
        SemanticProjectRedundancyStateV1 {
            revision,
            roots: SemanticRetainedVectorGenerationsV1::default(),
            authority: None,
            activation_receipt: None,
        },
    );
    true
}

#[hotpath::measure(label = "usecases.semantic.commit_redundancy")]
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
        activation_receipt: prepared
            .activation_receipt
            .clone()
            .filter(|_| install_authority),
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
    let mut retained = retained_generations()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    retained.remove(project_root);
    record_retained_generation_count(&retained);
    drop(retained);
    redundancy_states()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(project_root);
    activation_gates()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(project_root);
}

#[inline(always)]
fn record_retained_generation_count(retained: &BTreeMap<PathBuf, RetainedProjectGenerationsV1>) {
    #[cfg(feature = "hotpath")]
    {
        let count = retained.values().fold(0_usize, |total, project| {
            total.saturating_add(project.generations.len())
        });
        hotpath::gauge!("usecases.semantic.retained_code_generations").set(count);
    }
    #[cfg(not(feature = "hotpath"))]
    {
        let _ = retained;
    }
}

#[cfg(feature = "hotpath")]
static REDUNDANCY_READS_ACTIVE: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Additive lifecycle guard for the active redundancy-read gauge. Drop runs
/// on completion, cancellation, and panic alike, so an abandoned read can
/// never leak an "active" count.
#[cfg(feature = "hotpath")]
struct RedundancyReadObservationV1;

#[cfg(feature = "hotpath")]
impl RedundancyReadObservationV1 {
    fn enter() -> Self {
        use std::sync::atomic::Ordering;
        let active = REDUNDANCY_READS_ACTIVE
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1);
        hotpath::gauge!("usecases.semantic.redundancy.reads_active").set(active);
        Self
    }
}

#[cfg(feature = "hotpath")]
impl Drop for RedundancyReadObservationV1 {
    fn drop(&mut self) {
        use std::sync::atomic::Ordering;
        let _ =
            REDUNDANCY_READS_ACTIVE.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |active| {
                active.checked_sub(1)
            });
        hotpath::gauge!("usecases.semantic.redundancy.reads_active")
            .set(REDUNDANCY_READS_ACTIVE.load(Ordering::Relaxed));
    }
}

/// Read only the exact complete cosine generation selected by committed pins.
#[hotpath::measure(label = "usecases.semantic.redundancy_generation", future = true)]
pub async fn project_semantic_redundancy_generation(
    project_root: &Path,
) -> Option<SemanticRedundancyGenerationV1> {
    #[cfg(feature = "hotpath")]
    let _active = RedundancyReadObservationV1::enter();
    let generation = read_project_semantic_redundancy_generation(project_root).await;
    crate::hotpath_observe::semantic_redundancy_read(generation.is_some());
    generation
}

async fn read_project_semantic_redundancy_generation(
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
        .active_vector_generation(&authority.activation.compatibility)
        .await?;
    if vectors.generation_id() != &authority.activation.compatibility.vector_generation_id
        || vectors.embedding_key() != &authority.activation.compatibility.projection
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
    let scanned = vectors.vectors().len();
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
    crate::hotpath_observe::semantic_redundancy_scan(scanned, admitted.len());
    Some(SemanticRedundancyGenerationV1 {
        vector_generation: vectors.generation_id().as_digest().as_str().to_owned(),
        source_generation: source_generation.as_str().to_owned(),
        projection_key: vectors.projection_key().profile_digest.as_str().to_owned(),
        profile: SemanticRedundancyProfileV1 {
            scope_digest: authority.scope_digest.as_str().to_owned(),
            accepted_profile_digest: authority.accepted_profile_digest.as_str().to_owned(),
            calibration_profile_id: authority
                .activation
                .compatibility
                .calibration
                .calibration_profile_id
                .as_str()
                .to_owned(),
            calibration_digest: authority.calibration_digest.as_str().to_owned(),
            redundancy_profile_digest: authority.redundancy_profile_digest.as_str().to_owned(),
            maximum_distance_micros: authority
                .activation
                .compatibility
                .calibration
                .maximum_distance_micros,
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
        activation: activation.clone(),
        calibration_digest,
        redundancy_profile_digest,
    })
}

#[cfg(test)]
mod tests {
    use super::project_semantic_activation_gate;
    use std::path::Path;
    use tracedecay_semantic_contracts::{SemanticFallbackReasonV1, SemanticGenerationPointerV1};

    /// End-to-end status coherence: a concurrent activation that mutates the
    /// receipt under the project activation gate is never interleaved into
    /// the public status snapshot. The reader waits behind the gate, then
    /// observes the post-mutation truth instead of pairing a stale Ready
    /// receipt with the newer registry state.
    #[tokio::test]
    async fn application_status_observes_activation_mutations_coherently() {
        use super::{SemanticProjectRedundancyStateV1, redundancy_states};
        use crate::semantic_runtime::{
            SemanticActivationCommandV1, SemanticActivationReceiptV1, SemanticActivationRequestV1,
            SemanticConfigurationPinV1, SemanticRetainedVectorGenerationsV1,
            SemanticRuntimeStateV1, project_semantic_application_status,
            register_project_semantic_runtime, unregister_project_semantic_runtime,
        };
        use tracedecay_domain::configuration::{ConfigurationRevisionId, ConfigurationSnapshotId};
        use tracedecay_domain::{
            CodeGenerationId, ManifestDigest, UtcMicros, VectorGenerationIdV1,
        };
        use tracedecay_semantic::{
            DaemonSemanticRuntimeHandleV1, PreparedSemanticRuntimeCommitV1, SemanticRuntimeWorkV1,
        };

        let project_root = Path::new("/tmp/tracedecay-activation-snapshot-gate").to_path_buf();
        let handle = DaemonSemanticRuntimeHandleV1::new(1, 8, 1 << 20).expect("handle");
        let generation = VectorGenerationIdV1::new(
            tracedecay_domain::canonical_sha256(&("redundancy.snapshot-gate.vector", 's'))
                .expect("vector generation digest"),
        );
        let published = SemanticGenerationPointerV1 {
            generation: generation.clone(),
            source_generation: CodeGenerationId::new("code-generation.snapshot-gate")
                .expect("source generation"),
            projection_key: tracedecay_semantic::session_pool::test_support::authority()
                .projection()
                .projection_key()
                .clone(),
        };
        handle.schedule(SemanticRuntimeWorkV1::new(
            CodeGenerationId::new("code-generation.snapshot-gate").expect("source generation"),
            1,
            move |_progress| async move {
                Ok(PreparedSemanticRuntimeCommitV1::new(move || async move {
                    Ok(published)
                }))
            },
        ));
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while handle.current().is_none() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("current generation published");
        register_project_semantic_runtime(project_root.clone(), handle);

        let pin = SemanticConfigurationPinV1 {
            revision_id: ConfigurationRevisionId::try_from(
                "configuration.revision.snapshot-gate".to_owned(),
            )
            .expect("configuration revision"),
            snapshot_id: ConfigurationSnapshotId::try_from(
                "configuration.snapshot.snapshot-gate".to_owned(),
            )
            .expect("configuration snapshot"),
            effective_behavior_digest: ManifestDigest::new(format!("sha256:{}", "e".repeat(64)))
                .expect("configuration digest"),
        };
        let receipt = SemanticActivationReceiptV1::issue(
            &SemanticActivationCommandV1::new(
                pin.clone(),
                SemanticActivationRequestV1::new(generation, None, None)
                    .expect("activation request"),
            )
            .expect("activation command"),
            UtcMicros(10),
        )
        .expect("durable activation receipt");
        redundancy_states()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                project_root.clone(),
                SemanticProjectRedundancyStateV1 {
                    revision: ConfigurationRevisionId::try_from(
                        "configuration.revision.snapshot-gate".to_owned(),
                    )
                    .expect("configuration revision"),
                    roots: SemanticRetainedVectorGenerationsV1::default(),
                    authority: None,
                    activation_receipt: Some(receipt.clone()),
                },
            );

        let ready = project_semantic_application_status(&project_root, Some(pin.clone()))
            .expect("mounted status");
        assert_eq!(
            ready.state,
            SemanticRuntimeStateV1::Current {
                receipt: receipt.clone()
            },
            "the durable receipt pairs with the current scheduler generation"
        );

        let gate = project_semantic_activation_gate(&project_root);
        let activation_guard = gate.lock().expect("activation gate");
        let (status_tx, status_rx) = std::sync::mpsc::channel();
        let reader_root = project_root.clone();
        let reader_pin = pin.clone();
        let reader = std::thread::spawn(move || {
            let status = project_semantic_application_status(&reader_root, Some(reader_pin));
            status_tx.send(status).expect("deliver raced status");
        });
        assert!(
            status_rx
                .recv_timeout(std::time::Duration::from_millis(200))
                .is_err(),
            "the status snapshot must wait for the concurrent activation's gate"
        );
        redundancy_states()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get_mut(&project_root)
            .expect("redundancy state")
            .activation_receipt = None;
        drop(activation_guard);
        let raced = status_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("status after the activation gate releases")
            .expect("mounted status");
        assert!(
            matches!(
                raced.state,
                SemanticRuntimeStateV1::Degraded {
                    reason: SemanticFallbackReasonV1::NotActivated,
                    ..
                }
            ),
            "the snapshot must observe the concurrent clearing, never a stale Ready pairing"
        );
        reader.join().expect("status reader thread");
        unregister_project_semantic_runtime(&project_root);
    }

    #[test]
    fn activation_receipt_snapshot_holds_the_project_gate_for_its_reader() {
        use std::sync::{Arc, Barrier};

        let project_root = Path::new("/fast/tmp/tracedecay-semantic-status-snapshot");
        let entered = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let reader = {
            let entered = Arc::clone(&entered);
            let release = Arc::clone(&release);
            std::thread::spawn(move || {
                super::with_project_semantic_activation_receipt(project_root, |_| {
                    entered.wait();
                    release.wait();
                });
            })
        };

        entered.wait();
        let activation = project_semantic_activation_gate(project_root);
        assert!(
            activation.try_lock().is_err(),
            "the receipt and scheduler projection must share one activation snapshot"
        );
        release.wait();
        reader.join().expect("activation snapshot reader");
    }

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

    #[test]
    fn project_unregistration_releases_retained_semantic_registries() {
        use super::{
            PreparedSemanticRedundancyAuthorityV1, activation_gates,
            commit_project_semantic_redundancy_authority,
            project_semantic_retained_vector_generations, redundancy_states, retained_generations,
            unregister_project_semantic_redundancy_generation,
        };
        use crate::semantic_runtime::SemanticRetainedVectorGenerationsV1;
        use std::sync::Arc;
        use tracedecay_domain::configuration::ConfigurationRevisionId;

        let project_root = Path::new("/tmp/tracedecay-retention-release-unmount");
        let prepared = PreparedSemanticRedundancyAuthorityV1 {
            revision: ConfigurationRevisionId::new("configuration.revision.retention-release")
                .expect("revision"),
            roots: SemanticRetainedVectorGenerationsV1::default(),
            authority: None,
            activation_receipt: None,
        };
        commit_project_semantic_redundancy_authority(project_root.to_path_buf(), &prepared, false);
        assert!(
            project_semantic_retained_vector_generations(project_root).is_some(),
            "commit must seat the process-local redundancy record"
        );

        let gate = project_semantic_activation_gate(project_root);
        let gate_probe = Arc::downgrade(&gate);
        drop(gate);

        unregister_project_semantic_redundancy_generation(project_root);

        assert!(
            redundancy_states()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(project_root)
                .is_none(),
            "project unmount must drop the process-local redundancy record"
        );
        assert!(
            !activation_gates()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .contains_key(project_root),
            "project unmount must drop the activation-gate registry entry"
        );
        assert!(
            !retained_generations()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .contains_key(project_root),
            "project unmount must drop retained code-generation handles"
        );
        assert!(
            gate_probe.upgrade().is_none(),
            "project unmount must drop the last strong activation-gate handle"
        );
    }
}
