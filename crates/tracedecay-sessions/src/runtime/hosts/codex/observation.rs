//! Codex rollout observation admission and canonical-envelope normalization.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, Weak};

use tokio::sync::Notify;
use tokio::sync::futures::OwnedNotified;

pub(super) use tracedecay_capture::codex::codex_native_record_id;
#[cfg(test)]
pub use tracedecay_capture::codex::normalize_codex_observation;
use tracedecay_capture::codex::{
    CodexObservationContext, codex_observation_record_supported,
    normalize_codex_observation_with_context,
};
use tracedecay_domain::canonical_text::encode_lowercase_hex;
use tracedecay_domain::{
    ObservationScopeV1, ObservationSourceIdentityV1, ProjectId, ProviderId, RetentionClass,
    SessionId,
};
use tracedecay_store::observation::ObservationCoverageReason;

use super::PROVIDER;
use super::context::CodexContextState;
use super::meta::{CodexMetaWithProvenance, session_meta_with_provenance};
use crate::admission::HostAdmission;
use crate::host_ports::unregistered_admission;
use crate::observation::ObservationCancellation;
use crate::runtime::jsonl_observation_admission::{
    JsonlFrameAdmission, JsonlObservationAdmissionRequest, PersistedCursorUpdate,
    SharedJsonlFileIdentity, admit_jsonl_observations, reserve_shared_jsonl_page,
    shared_jsonl_background_cpu, shared_jsonl_file_identity, shared_jsonl_preparation_capacity,
};
use crate::runtime::shared::TranscriptScopeMatcher;
use crate::runtime::source::{JsonlIoAccounting, TranscriptIngestError, TranscriptIngestResult};
use tracedecay_privacy::{ObservationRecordParseErrorV1, normalize_prepared_observation_record_v1};
use tracedecay_runtime_core::resident_memory::ProcessSharedMemoryReservationV1;

#[cfg(test)]
mod meta_cache_tests;
#[cfg(test)]
mod retired_source_tests;

const CODEX_OBSERVATION_RETENTION: &str = "retention.provider-observation";
pub const CODEX_HOOK_MAX_NEW_BYTES: u64 = crate::runtime::source::MAX_JSONL_RECORD_BYTES as u64;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct CodexMetaCacheKey {
    path: PathBuf,
    identity: SharedJsonlFileIdentity,
}

struct CachedCodexMeta {
    key: CodexMetaCacheKey,
    meta: Arc<CodexMetaWithProvenance>,
    _memory: Option<ProcessSharedMemoryReservationV1>,
}

#[derive(Default)]
struct CodexMetaCache {
    entries: VecDeque<CachedCodexMeta>,
    /// Keys with a fill in flight, each paired with the `Notify` its waiters
    /// park on. An entry is owned by exactly one [`CodexMetaFillClaim`].
    in_flight: HashMap<CodexMetaCacheKey, Arc<Notify>>,
}

/// Process-retained metadata cache. Every critical section is synchronous, and
/// a `std` mutex is what lets a fill claim release itself from `Drop`.
static CODEX_META_CACHE: OnceLock<Mutex<CodexMetaCache>> = OnceLock::new();

fn lock_codex_meta_cache() -> MutexGuard<'static, CodexMetaCache> {
    CODEX_META_CACHE
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// Owner of one in-flight metadata fill.
///
/// The fill runs on its own task, so the request that elected it may stop
/// waiting without orphaning the claim: the owner still settles the parse,
/// publishes or fails, and only then drops. Dropping, after publication, on a
/// terminal failure, or when the fill task itself is torn down, removes
/// exactly this claim and wakes every waiter, which re-checks the cache and
/// elects a new fill when nothing was published.
struct CodexMetaFillClaim {
    key: CodexMetaCacheKey,
    settled: Arc<Notify>,
}

impl CodexMetaFillClaim {
    fn publish(
        self,
        meta: Arc<CodexMetaWithProvenance>,
        memory: Option<ProcessSharedMemoryReservationV1>,
    ) {
        let mut cache = lock_codex_meta_cache();
        while cache.entries.len() >= shared_jsonl_preparation_capacity() {
            cache.entries.pop_front();
        }
        cache.entries.push_back(CachedCodexMeta {
            key: self.key.clone(),
            meta,
            _memory: memory,
        });
        drop(cache);
        // Dropping `self` releases the claim and wakes the waiters, who now
        // find the published entry.
    }
}

impl Drop for CodexMetaFillClaim {
    fn drop(&mut self) {
        let mut cache = lock_codex_meta_cache();
        // Only this fill's own claim is released: a late owner never erases a
        // replacement that waiters elected after it was torn down.
        if cache
            .in_flight
            .get(&self.key)
            .is_some_and(|settled| Arc::ptr_eq(settled, &self.settled))
        {
            cache.in_flight.remove(&self.key);
        }
        drop(cache);
        self.settled.notify_waiters();
    }
}

enum CodexMetaLookup {
    Hit(Arc<CodexMetaWithProvenance>),
    InFlight(OwnedNotified),
    Claimed(CodexMetaFillClaim),
}

#[cfg(test)]
static CODEX_META_IN_FLIGHT_WAITS: OnceLock<Mutex<HashMap<CodexMetaCacheKey, usize>>> =
    OnceLock::new();

#[cfg(test)]
fn record_in_flight_wait_for_test(key: &CodexMetaCacheKey) {
    let mut waits = CODEX_META_IN_FLIGHT_WAITS
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    *waits.entry(key.clone()).or_default() += 1;
}

#[cfg(all(test, unix))]
fn in_flight_waits_for_test(key: &CodexMetaCacheKey) -> usize {
    CODEX_META_IN_FLIGHT_WAITS
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(key)
        .copied()
        .unwrap_or_default()
}

fn lookup_codex_meta(key: &CodexMetaCacheKey) -> TranscriptIngestResult<CodexMetaLookup> {
    let mut cache = lock_codex_meta_cache();
    if let Some(index) = cache.entries.iter().position(|entry| entry.key == *key) {
        let entry = cache
            .entries
            .remove(index)
            .ok_or(TranscriptIngestError::InvalidFrameState { provider: PROVIDER })?;
        let meta = Arc::clone(&entry.meta);
        cache.entries.push_back(entry);
        return Ok(CodexMetaLookup::Hit(meta));
    }
    if let Some(settled) = cache.in_flight.get(key) {
        #[cfg(test)]
        record_in_flight_wait_for_test(key);
        // Created under the lock, so a settlement racing this lookup still
        // wakes the returned future.
        return Ok(CodexMetaLookup::InFlight(
            Arc::clone(settled).notified_owned(),
        ));
    }
    let settled = Arc::new(Notify::new());
    cache.in_flight.insert(key.clone(), Arc::clone(&settled));
    Ok(CodexMetaLookup::Claimed(CodexMetaFillClaim {
        key: key.clone(),
        settled,
    }))
}

/// Runs one admitted metadata fill to settlement.
///
/// The memory reservation travels inside the blocking closure: dropping this
/// task's `JoinHandle` does not stop started blocking work, so the charge is
/// released only when the parse itself settles, shrunk into the cache entry
/// on success, or dropped with the worker's result otherwise.
async fn fill_codex_session_meta(
    claim: CodexMetaFillClaim,
    error_path: PathBuf,
) -> TranscriptIngestResult<Arc<CodexMetaWithProvenance>> {
    let memory = reserve_shared_jsonl_page()?;
    let background_cpu = shared_jsonl_background_cpu()?;
    let parse_path = claim.key.path.clone();
    let (parsed, mut memory) = tokio::task::spawn_blocking(move || {
        let parsed = background_cpu.with_permit(|| session_meta_with_provenance(&parse_path));
        (parsed, memory)
    })
    .await
    .map_err(|_| TranscriptIngestError::BlockingScanTaskFailed { provider: PROVIDER })?;
    let parsed = Arc::new(parsed.ok_or(TranscriptIngestError::InvalidSourceIdentity {
        provider: PROVIDER,
        path: error_path,
    })?);
    if let Some(reservation) = &mut memory {
        reservation
            .shrink_to(parsed.retained_bytes())
            .map_err(|_| TranscriptIngestError::InvalidFrameState { provider: PROVIDER })?;
    }
    claim.publish(Arc::clone(&parsed), memory);
    Ok(parsed)
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CodexJsonlAdmissionProgress {
    pub bytes_consumed: u64,
    pub source_deferred: bool,
    pub frames_decoded: u64,
    pub frames_accepted: u64,
    pub frames_skipped: u64,
    /// Of `frames_skipped`, the frames refused from their raw bytes before any
    /// decode. Every one is a rollout record this scope never owned.
    pub frames_rejected_before_decode: u64,
    pub frames_refused: u64,
    pub frames_persisted: u64,
    /// This pass resumed from a durable source cursor instead of opening the
    /// rollout for the first time. With `frames_persisted == 0` it is the only
    /// evidence that separates an already-admitted rollout from an empty one,
    /// so a caller can report the replay as a duplicate rather than as a pass
    /// that captured nothing.
    pub resumed: bool,
    /// The committed source cursor's position once this pass settled.
    pub covered_through: u64,
    /// Bytes this pass's rollout scan read, by category.
    pub io: JsonlIoAccounting,
    /// Bytes reread before the resume offset to recover the cwd the first
    /// new record inherits.
    pub prior_context_bytes: u64,
}

/// Admit a Codex rollout for one exact project identity.
///
/// The scheduler supplies the already-resolved project id; each complete record
/// is routed by the rollout's current Codex cwd, including context reconstructed
/// before a resumed byte cursor.
pub async fn try_admit_codex_jsonl_observations_for_project(
    path: &Path,
    project_root: &Path,
    project_id: ProjectId,
    max_new_bytes: Option<u64>,
) -> TranscriptIngestResult<CodexJsonlAdmissionProgress> {
    let Some(admission) =
        unregistered_admission::create(unregistered_admission::Scope::Project(project_id.clone()))
    else {
        return Ok(CodexJsonlAdmissionProgress::default());
    };
    try_admit_codex_jsonl_observations_for_project_with_admission(
        path,
        project_root,
        project_id,
        admission.as_ref(),
        max_new_bytes,
    )
    .await
}

/// Project admission with authority prepared by the caller.
///
/// The project scheduler constructs this facade from its authoritative project
/// identity and may attach additional admission evidence before source routing.
pub async fn try_admit_codex_jsonl_observations_for_project_with_admission(
    path: &Path,
    project_root: &Path,
    project_id: ProjectId,
    admission: &dyn HostAdmission,
    max_new_bytes: Option<u64>,
) -> TranscriptIngestResult<CodexJsonlAdmissionProgress> {
    try_admit_codex_jsonl_observations_for_project_with_admission_and_cancellation(
        path,
        project_root,
        project_id,
        admission,
        max_new_bytes,
        &ObservationCancellation::default(),
    )
    .await
}

pub async fn try_admit_codex_jsonl_observations_for_project_with_admission_and_cancellation(
    path: &Path,
    project_root: &Path,
    project_id: ProjectId,
    admission: &dyn HostAdmission,
    max_new_bytes: Option<u64>,
    cancellation: &ObservationCancellation,
) -> TranscriptIngestResult<CodexJsonlAdmissionProgress> {
    try_admit_codex_jsonl_observations(
        path,
        CodexObservationAdmission::Project {
            root: project_root,
            project_id,
        },
        admission,
        max_new_bytes,
        None,
        cancellation,
    )
    .await
}

pub(crate) async fn try_admit_codex_jsonl_observations_for_project_window(
    path: &Path,
    project_root: &Path,
    project_id: ProjectId,
    admission: &dyn HostAdmission,
    max_new_bytes: u64,
    cancellation: &ObservationCancellation,
) -> TranscriptIngestResult<CodexJsonlAdmissionProgress> {
    try_admit_codex_jsonl_observations(
        path,
        CodexObservationAdmission::Project {
            root: project_root,
            project_id,
        },
        admission,
        Some(
            max_new_bytes
                .min(crate::runtime::jsonl_observation_admission::MAX_CAPTURE_WINDOW_BYTES),
        ),
        Some(crate::runtime::jsonl_observation_admission::MAX_CAPTURE_WINDOW),
        cancellation,
    )
    .await
}

/// Admit Codex records that are not attributable to any registered project.
///
/// A scheduler may constrain this pass to one session while it catches up a
/// profile-owned rollout.
pub async fn try_admit_codex_jsonl_observations_for_profile(
    path: &Path,
    session_id: Option<&str>,
    registered_roots: &[PathBuf],
    max_new_bytes: Option<u64>,
) -> TranscriptIngestResult<CodexJsonlAdmissionProgress> {
    let Some(admission) = unregistered_admission::create(unregistered_admission::Scope::Profile)
    else {
        return Ok(CodexJsonlAdmissionProgress::default());
    };
    try_admit_codex_jsonl_observations_for_profile_with_admission(
        path,
        session_id,
        registered_roots,
        admission.as_ref(),
        max_new_bytes,
    )
    .await
}

pub async fn try_admit_codex_jsonl_observations_for_profile_with_admission(
    path: &Path,
    session_id: Option<&str>,
    registered_roots: &[PathBuf],
    admission: &dyn HostAdmission,
    max_new_bytes: Option<u64>,
) -> TranscriptIngestResult<CodexJsonlAdmissionProgress> {
    try_admit_codex_jsonl_observations_for_profile_with_admission_and_cancellation(
        path,
        session_id,
        registered_roots,
        admission,
        max_new_bytes,
        &ObservationCancellation::default(),
    )
    .await
}

pub async fn try_admit_codex_jsonl_observations_for_profile_with_admission_and_cancellation(
    path: &Path,
    session_id: Option<&str>,
    registered_roots: &[PathBuf],
    admission: &dyn HostAdmission,
    max_new_bytes: Option<u64>,
    cancellation: &ObservationCancellation,
) -> TranscriptIngestResult<CodexJsonlAdmissionProgress> {
    try_admit_codex_jsonl_observations(
        path,
        CodexObservationAdmission::Profile {
            session_id,
            registered_roots,
        },
        admission,
        max_new_bytes,
        None,
        cancellation,
    )
    .await
}

pub(super) enum CodexObservationAdmission<'a> {
    Project {
        root: &'a Path,
        project_id: ProjectId,
    },
    Profile {
        session_id: Option<&'a str>,
        registered_roots: &'a [PathBuf],
    },
}

impl CodexObservationAdmission<'_> {
    pub(super) fn scope(&self) -> ObservationScopeV1 {
        match self {
            Self::Project { project_id, .. } => ObservationScopeV1::Project {
                project_id: project_id.clone(),
            },
            Self::Profile { .. } => ObservationScopeV1::Profile,
        }
    }

    /// Resolve this rollout's scope boundary once, so the per-record admission
    /// test does not re-resolve the git identity of an unchanging root.
    pub(super) fn scope_matcher(&self) -> TranscriptScopeMatcher {
        match self {
            Self::Project { root, .. } => TranscriptScopeMatcher::project(root),
            Self::Profile {
                registered_roots, ..
            } => TranscriptScopeMatcher::profile(registered_roots),
        }
    }

    pub(super) fn accepts_session(&self, session_id: &str) -> bool {
        !matches!(self, Self::Profile { session_id: Some(expected), .. } if *expected != session_id)
    }

    /// A project rollout's session row keys on the registered root, and a
    /// profile rollout's on its session cwd, while each record's location is
    /// the cwd in effect when it was written. That is the linked worktree the
    /// record ran in whenever it differs from the root, and a `turn_context`
    /// can move it mid-rollout. The source file the record was read from and
    /// the turn's model complete the projection context.
    fn observation_context<'b>(
        &'b self,
        session_cwd: &'b Path,
        record_cwd: &'b Path,
        transcript_path: &'b Path,
        model: Option<&'b str>,
    ) -> CodexObservationContext<'b> {
        let project_path = match self {
            Self::Project { root, .. } => *root,
            Self::Profile { .. } => session_cwd,
        };
        CodexObservationContext {
            project_path: Some(project_path),
            location_path: Some(record_cwd),
            transcript_path: Some(transcript_path),
            model,
        }
    }
}

#[derive(Clone)]
struct CodexAdmissionState {
    context: CodexContextState,
    scope_verdict: Option<bool>,
}

#[derive(Clone, Copy)]
struct CodexAdmissionContext<'a> {
    path: &'a Path,
    scope: &'a CodexObservationAdmission<'a>,
    admission: &'a dyn HostAdmission,
    meta: &'a super::meta::CodexMeta,
    native_thread_id: Option<&'a str>,
    cancellation: &'a ObservationCancellation,
}

fn replay_identity(session_id: &str, domain: &[u8]) -> String {
    use sha2::{Digest as _, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(domain);
    hasher.update([0]);
    hasher.update(session_id.as_bytes());
    format!("sha256:{}", encode_lowercase_hex(&hasher.finalize()))
}

pub fn codex_observation_source_v2(
    session_id: &str,
) -> TranscriptIngestResult<ObservationSourceIdentityV1> {
    Ok(ObservationSourceIdentityV1::for_provider_source(
        ProviderId::new(PROVIDER)?,
        SessionId::new(session_id.to_string())?,
        SessionId::new(replay_identity(
            session_id,
            b"tracedecay.codex.observation-source.v2",
        ))?,
    )?)
}

async fn shared_session_meta_with_provenance(
    path: &Path,
    cancellation: &ObservationCancellation,
) -> TranscriptIngestResult<Arc<CodexMetaWithProvenance>> {
    let identity_path = path.to_path_buf();
    let key = tokio::task::spawn_blocking(move || {
        let canonical = std::fs::canonicalize(&identity_path).map_err(|source| {
            TranscriptIngestError::ScanIo {
                operation: "resolve Codex session metadata identity",
                path: identity_path.clone(),
                source,
            }
        })?;
        let identity = shared_jsonl_file_identity(&canonical)?;
        Ok::<_, TranscriptIngestError>(CodexMetaCacheKey {
            path: canonical,
            identity,
        })
    })
    .await
    .map_err(|_| TranscriptIngestError::BlockingScanTaskFailed { provider: PROVIDER })??;
    let claim = loop {
        if cancellation.is_cancelled() {
            return Err(TranscriptIngestError::Cancelled { provider: PROVIDER });
        }
        match lookup_codex_meta(&key)? {
            CodexMetaLookup::Hit(meta) => return Ok(meta),
            CodexMetaLookup::InFlight(settled) => {
                tokio::select! {
                    () = settled => {}
                    () = cancellation.cancelled() => {
                        return Err(TranscriptIngestError::Cancelled { provider: PROVIDER });
                    }
                }
            }
            CodexMetaLookup::Claimed(claim) => break claim,
        }
    };
    // The fill is detached from this request: cancelling the requester leaves
    // the owner to settle for every other waiter on the same key.
    let fill = tokio::spawn(fill_codex_session_meta(claim, path.to_path_buf()));
    tokio::select! {
        settled = fill => {
            settled.map_err(|_| TranscriptIngestError::BlockingScanTaskFailed { provider: PROVIDER })?
        }
        () = cancellation.cancelled() => Err(TranscriptIngestError::Cancelled { provider: PROVIDER }),
    }
}

/// Serializes the read-cursor-then-admit window for one rollout in one scope.
///
/// The MCP hook route (`admit_codex_project_rollouts`) and the daemon's project
/// catch-up sweep (`ingest::project_provider::run_codex`) both reach
/// [`try_admit_codex_jsonl_observations`] for the same rollout under the same
/// scope, and both read the source cursor before they write. Interleaved, the
/// loser reads a cursor the winner has not published yet, re-reads frames the
/// winner has already committed, and re-submits the same observation ids with
/// its own independently captured repository provenance. The store's replay
/// verification refuses that second provenance as `observation repository
/// provenance collision`, which reaches the host as a retryable
/// `authority_write_failed` infrastructure error rather than the duplicate it
/// is. Serialized, the loser reads the advanced cursor, persists nothing, and
/// reports the `resumed` replay its caller renders as an exact duplicate.
///
/// The gate is per process. Cross-process writers still meet at the store's
/// own transaction, which is what the replay verification is there for.
type CodexAdmissionGate = Arc<tokio::sync::Mutex<()>>;
type CodexAdmissionGates =
    Mutex<HashMap<(ObservationScopeV1, PathBuf), Weak<tokio::sync::Mutex<()>>>>;

static CODEX_ADMISSION_GATES: OnceLock<CodexAdmissionGates> = OnceLock::new();

/// ponytail: linear sweep of live gates per acquisition; keyed eviction if a
/// scope ever admits enough rollouts at once for the sweep to show up.
fn codex_admission_gate(scope: &ObservationScopeV1, path: &Path) -> CodexAdmissionGate {
    let gates = CODEX_ADMISSION_GATES.get_or_init(|| Mutex::new(HashMap::new()));
    let mut gates = gates.lock().unwrap_or_else(PoisonError::into_inner);
    gates.retain(|_, gate| gate.strong_count() > 0);
    let key = (scope.clone(), path.to_path_buf());
    if let Some(gate) = gates.get(&key).and_then(Weak::upgrade) {
        return gate;
    }
    let gate = CodexAdmissionGate::new(tokio::sync::Mutex::new(()));
    gates.insert(key, Arc::downgrade(&gate));
    gate
}

async fn try_admit_codex_jsonl_observations(
    path: &Path,
    admission_scope: CodexObservationAdmission<'_>,
    admission: &dyn HostAdmission,
    max_new_bytes: Option<u64>,
    max_frames: Option<usize>,
    cancellation: &ObservationCancellation,
) -> TranscriptIngestResult<CodexJsonlAdmissionProgress> {
    if cancellation.is_cancelled() {
        return Ok(CodexJsonlAdmissionProgress {
            bytes_consumed: 0,
            source_deferred: true,
            ..CodexJsonlAdmissionProgress::default()
        });
    }
    let parsed_meta = shared_session_meta_with_provenance(path, cancellation).await?;
    let native_thread_id = parsed_meta.native_thread_id.clone();
    let meta = parsed_meta.meta.clone();
    if !admission_scope.accepts_session(&meta.session_id) {
        return Ok(CodexJsonlAdmissionProgress::default());
    }
    let canonical_source = codex_observation_source_v2(&meta.session_id)?;
    let context = CodexAdmissionContext {
        path,
        scope: &admission_scope,
        admission,
        meta: &meta,
        native_thread_id: native_thread_id.as_deref(),
        cancellation,
    };
    let scope = admission_scope.scope();
    // Held across the cursor read and the admit below: both are one pass over
    // this rollout, and a peer that interleaves between them re-submits what
    // this pass is about to commit.
    let gate = codex_admission_gate(&scope, path);
    let _admitting = gate.lock().await;
    admit_codex_jsonl_page(context, canonical_source, max_new_bytes, max_frames).await
}

async fn admit_codex_jsonl_page(
    context: CodexAdmissionContext<'_>,
    source: ObservationSourceIdentityV1,
    max_new_bytes: Option<u64>,
    max_frames: Option<usize>,
) -> TranscriptIngestResult<CodexJsonlAdmissionProgress> {
    let CodexAdmissionContext {
        path,
        scope: admission_scope,
        admission,
        meta,
        native_thread_id,
        cancellation,
    } = context;
    let scope = admission_scope.scope();
    // Resolving the scope runs git for the root. An unchanged rollout admits
    // no frame and must not pay it on every pass.
    let resolved_scope = OnceLock::new();
    let scope_matcher = || resolved_scope.get_or_init(|| admission_scope.scope_matcher());
    let mut request = JsonlObservationAdmissionRequest::new(
        PROVIDER,
        path,
        admission,
        source,
        scope.clone(),
        RetentionClass::new(CODEX_OBSERVATION_RETENTION)?,
    )
    .with_max_new_bytes(max_new_bytes)
    .with_persisted_cursor_update(PersistedCursorUpdate::Replace)
    .with_lazy_shared_frame_preparation()
    .with_cancellation(cancellation.clone());
    if let Some(max_frames) = max_frames {
        request = request.with_max_frames(max_frames);
    }
    let mut prior_context_bytes = 0;
    let progress = admit_jsonl_observations(
        request,
        |scan| {
            debug_assert!(
                scan.frame_bytes().all(|frame| !frame.is_empty()),
                "shared JSONL admission never exposes empty native frames"
            );
            // The prior context is rebuilt by reading the rollout before its
            // cursor; a scan with no new frame never consults it.
            let context = if scan.resumed && scan.frame_bytes().next().is_some() {
                let (context, read) =
                    CodexContextState::scan_prior(path, scan.generation, scan.start_offset, meta);
                prior_context_bytes = read;
                context
            } else {
                CodexContextState::from_meta(meta)
            };
            CodexAdmissionState {
                context,
                scope_verdict: None,
            }
        },
        |state, _bytes, range, _, prepared, hints| {
            let mut stable_record_id = None;
            let mut non_durable_reason = None;
            // Scope is consulted before the record is decoded, not after. A
            // rollout that belongs to another project answers the same verdict
            // for every one of its frames, and the old order paid a full JSON
            // decode per frame to reach it.
            let in_scope = *state
                .scope_verdict
                .get_or_insert_with(|| scope_matcher().accepts(Some(state.context.cwd.as_path())));
            if !in_scope && !hints.may_change_codex_context {
                return Ok(JsonlFrameAdmission::non_durable_before_decode(
                    ObservationCoverageReason::OutOfScope,
                ));
            }
            let Some(prepared) = prepared else {
                return Ok(JsonlFrameAdmission::needs_preparation());
            };
            let parsed = normalize_prepared_observation_record_v1(prepared, |native| {
                if state.context.observe_context_record(native, path, meta) {
                    // Only a context record can move the rollout's cwd,
                    // so the memoized verdict is dropped exactly when it
                    // can no longer be trusted.
                    state.scope_verdict = None;
                }
                if !*state.scope_verdict.get_or_insert_with(|| {
                    scope_matcher().accepts(Some(state.context.cwd.as_path()))
                }) {
                    non_durable_reason = Some(ObservationCoverageReason::OutOfScope);
                    return Err(ObservationRecordParseErrorV1::NormalizationFailed);
                }
                if !codex_observation_record_supported(native) {
                    non_durable_reason = Some(ObservationCoverageReason::UnsupportedFact);
                    return Err(ObservationRecordParseErrorV1::NormalizationFailed);
                }
                let record_id = codex_native_record_id(&meta.session_id, native)
                    .map_err(|_| ObservationRecordParseErrorV1::NormalizationFailed)?;
                let envelope = normalize_codex_observation_with_context(
                    native,
                    &meta.session_id,
                    native_thread_id,
                    record_id.clone(),
                    range,
                    admission_scope.observation_context(
                        meta.cwd.as_path(),
                        state.context.cwd.as_path(),
                        path,
                        state.context.model.as_deref(),
                    ),
                )?;
                stable_record_id = Some(record_id);
                Ok(envelope)
            });
            match parsed {
                Ok(parsed) => {
                    let record_id = stable_record_id
                        .ok_or(TranscriptIngestError::InvalidFrameState { provider: PROVIDER })?;
                    Ok(JsonlFrameAdmission::durable(parsed, record_id))
                }
                Err(_) => Ok(JsonlFrameAdmission::non_durable(
                    non_durable_reason.unwrap_or(ObservationCoverageReason::MalformedFrame),
                )),
            }
        },
    )
    .await?;
    Ok(CodexJsonlAdmissionProgress {
        bytes_consumed: progress.bytes_consumed,
        source_deferred: progress.source_deferred,
        frames_decoded: progress.frames_decoded,
        frames_accepted: progress.frames_accepted,
        frames_skipped: progress.frames_skipped,
        frames_rejected_before_decode: progress.frames_rejected_before_decode,
        frames_refused: progress.frames_refused,
        frames_persisted: progress.frames_persisted,
        resumed: progress.resumed,
        covered_through: progress.covered_through,
        io: progress.io,
        prior_context_bytes,
    })
}
