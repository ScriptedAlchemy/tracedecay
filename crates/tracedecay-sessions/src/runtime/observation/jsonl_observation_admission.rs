use rayon::prelude::*;
use std::borrow::Cow;
use std::collections::{HashMap, HashSet, VecDeque};
use std::num::NonZeroU64;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use tokio::sync::Notify;

use tracedecay_domain::{
    CanonicalObservationIdV1, ObservationId, ObservationIdentityMaterialV1,
    ObservationOrderingDomainV1, ObservationScopeV1, ObservationSourceCursorV1,
    ObservationSourceGenerationV1, ObservationSourceIdentityV1, RetentionClass,
    SanitizationReceiptV1,
};
use tracedecay_store::observation::{
    ObservationCoverageReason, ObservationCursorAdvance, ObservationIdentityCollisionDispositionV1,
};

use crate::admission::{
    HostAdmission, HostAdmissionOutcome, HostAdmissionRecovery, HostAdmissionStatus,
    is_admission_cancellation,
};
use crate::observation::{
    CaptureObservationOutcome, CaptureObservationRequest, ObservationCancellation,
};
use crate::runtime::SessionMessageRecord;
use crate::runtime::shared::StoredCursor;
use crate::runtime::snapshot_observation::host_admission_error;
use crate::runtime::source::{
    JsonlFrameDeferral, JsonlIoAccounting, JsonlResumeState, MAX_JSONL_RECORD_BYTES,
    ParsedTranscript, RawJsonlRecord, RawJsonlSkippedRange, RawJsonlSkippedReason,
    TranscriptIngestError, TranscriptIngestResult, preflight_strict_jsonl,
    try_stream_new_jsonl_raw_strict_with_resume,
};
#[cfg(test)]
use tracedecay_private_fs::background_cpu::install_process_background_cpu;
use tracedecay_private_fs::background_cpu::{ProcessBackgroundCpuV1, process_background_cpu};
use tracedecay_runtime_core::privacy::{
    ObservationRecordParseErrorV1, ParsedObservationRecordV1, PreparedObservationRecordV1,
    prepare_observation_record_v1,
};
use tracedecay_runtime_core::resident_memory::{
    ProcessResidentMemoryV1, ProcessSharedMemoryReservationV1, ResidentMemoryComponentIdV1,
};

#[derive(Clone, Copy)]
pub(in crate::runtime) enum PersistedCursorUpdate {
    Replace,
    Monotonic,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum SharedJsonlFramePreparation {
    None,
    Lazy,
    EagerWhenPinned,
}

impl From<bool> for SharedJsonlFramePreparation {
    fn from(prepare_frames: bool) -> Self {
        if prepare_frames {
            Self::EagerWhenPinned
        } else {
            Self::None
        }
    }
}

/// How one flush persists: the retention class the frames are captured under
/// and whether the durable cursor may move backwards. Both are decided once per
/// admission and always travel together, so they pass as one value.
#[derive(Clone, Copy)]
struct FlushPolicy<'policy> {
    retention_class: &'policy RetentionClass,
    persisted_cursor_update: PersistedCursorUpdate,
}

pub(in crate::runtime) struct JsonlObservationAdmissionRequest<'request> {
    provider: &'static str,
    path: &'request Path,
    admission: &'request dyn HostAdmission,
    source: ObservationSourceIdentityV1,
    scope: ObservationScopeV1,
    retention_class: RetentionClass,
    max_new_bytes: Option<u64>,
    required_start_cursor: Option<Option<ObservationSourceCursorV1>>,
    max_end_offset: Option<u64>,
    persisted_cursor_update: PersistedCursorUpdate,
    cancellation: ObservationCancellation,
    shared_frame_preparation: SharedJsonlFramePreparation,
}

impl<'request> JsonlObservationAdmissionRequest<'request> {
    pub(in crate::runtime) fn new(
        provider: &'static str,
        path: &'request Path,
        admission: &'request dyn HostAdmission,
        source: ObservationSourceIdentityV1,
        scope: ObservationScopeV1,
        retention_class: RetentionClass,
    ) -> Self {
        Self {
            provider,
            path,
            admission,
            source,
            scope,
            retention_class,
            max_new_bytes: None,
            required_start_cursor: None,
            max_end_offset: None,
            persisted_cursor_update: PersistedCursorUpdate::Monotonic,
            cancellation: ObservationCancellation::default(),
            shared_frame_preparation: SharedJsonlFramePreparation::None,
        }
    }

    pub(in crate::runtime) fn with_max_new_bytes(mut self, max_new_bytes: Option<u64>) -> Self {
        self.max_new_bytes = max_new_bytes;
        self
    }

    /// Require this admission to start from the cursor observed by its caller.
    /// A concurrent winner changes the cursor into a no-op so the caller can
    /// refresh any external watermark before deciding what the next bytes mean.
    pub(in crate::runtime) fn with_required_start_cursor(
        mut self,
        required_start_cursor: Option<ObservationSourceCursorV1>,
    ) -> Self {
        self.required_start_cursor = Some(required_start_cursor);
        self
    }

    /// Bound this admission to an absolute source offset.
    pub(in crate::runtime) fn with_max_end_offset(mut self, max_end_offset: u64) -> Self {
        self.max_end_offset = Some(max_end_offset);
        self
    }

    pub(in crate::runtime) fn with_persisted_cursor_update(
        mut self,
        persisted_cursor_update: PersistedCursorUpdate,
    ) -> Self {
        self.persisted_cursor_update = persisted_cursor_update;
        self
    }

    pub(in crate::runtime) fn with_cancellation(
        mut self,
        cancellation: ObservationCancellation,
    ) -> Self {
        self.cancellation = cancellation;
        self
    }

    pub(in crate::runtime) fn with_lazy_shared_frame_preparation(mut self) -> Self {
        self.shared_frame_preparation = SharedJsonlFramePreparation::Lazy;
        self
    }
}

pub(in crate::runtime) enum JsonlFrameAdmission {
    Durable {
        parsed_record: ParsedObservationRecordV1,
        native_record_id: ObservationId,
        identity_collision_retry: bool,
        skip_if_observation_exists: Option<CanonicalObservationIdV1>,
    },
    NonDurable {
        reason: ObservationCoverageReason,
        /// The verdict was reached without decoding the frame. Kept because
        /// the cost of a skipped frame is not the skip: it is whether the
        /// frame was parsed first.
        before_decode: bool,
    },
    NeedsPreparation,
}

impl JsonlFrameAdmission {
    pub(in crate::runtime) fn durable(
        parsed_record: ParsedObservationRecordV1,
        native_record_id: ObservationId,
    ) -> Self {
        let mut admission =
            Self::durable_with_identity_collision_retry(parsed_record, native_record_id);
        if let Self::Durable {
            identity_collision_retry,
            ..
        } = &mut admission
        {
            *identity_collision_retry = false;
        }
        admission
    }

    /// Marks a frame whose provider proved that its primary stable identity
    /// was derived without a native record id and may therefore be retried
    /// once with the provider's positional collision fallback.
    pub(in crate::runtime) fn durable_with_identity_collision_retry(
        parsed_record: ParsedObservationRecordV1,
        native_record_id: ObservationId,
    ) -> Self {
        Self::Durable {
            parsed_record,
            native_record_id,
            identity_collision_retry: true,
            skip_if_observation_exists: None,
        }
    }

    pub(in crate::runtime) fn durable_unless_observation_exists(
        parsed_record: ParsedObservationRecordV1,
        native_record_id: ObservationId,
        observation_id: CanonicalObservationIdV1,
    ) -> Self {
        Self::Durable {
            parsed_record,
            native_record_id,
            identity_collision_retry: false,
            skip_if_observation_exists: Some(observation_id),
        }
    }

    pub(in crate::runtime) fn non_durable(reason: ObservationCoverageReason) -> Self {
        Self::NonDurable {
            reason,
            before_decode: false,
        }
    }

    /// A frame refused without ever being decoded.
    ///
    /// A normalizer may only answer this when the reason is a property the
    /// frame's bytes cannot change. Codex scope is the one such reason today:
    /// it belongs to the session cwd, and only a `session_meta` or
    /// `turn_context` line can move that cwd, so a frame that provably names
    /// neither inherits a verdict that is already settled.
    ///
    /// Deciding this early is not reason-preserving, and deliberately so. A
    /// frame that is *also* malformed would have been covered as
    /// `MalformedFrame` had it been decoded; refused early it is covered as
    /// `OutOfScope`. That is the intended reading — a rollout this scope does
    /// not own is not this pass's to report structural health on — and it is
    /// what makes the skipped decode observable in production state rather
    /// than only in a counter.
    pub(in crate::runtime) fn non_durable_before_decode(reason: ObservationCoverageReason) -> Self {
        Self::NonDurable {
            reason,
            before_decode: true,
        }
    }

    #[hotpath::skip]
    pub(in crate::runtime) const fn needs_preparation() -> Self {
        Self::NeedsPreparation
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(in crate::runtime) struct JsonlObservationAdmissionProgress {
    pub bytes_consumed: u64,
    pub source_deferred: bool,
    pub frames_decoded: u64,
    pub frames_accepted: u64,
    pub frames_skipped: u64,
    /// Of `frames_skipped`, how many were refused from their raw bytes before
    /// being decoded. The aggregate cannot separate a skip that cost a decode
    /// and two structural walks from one that cost a tokenize.
    pub frames_rejected_before_decode: u64,
    pub frames_refused: u64,
    pub frames_persisted: u64,
    pub io: crate::runtime::source::JsonlIoAccounting,
}

#[derive(Clone, Copy)]
pub(in crate::runtime) struct JsonlObservationScan<'scan> {
    pub resumed: bool,
    /// True when a prior cursor existed but the scan restarted at offset 0
    /// (truncate/rename replacement). Callers use this to keep projected
    /// message ids distinct across file generations.
    pub replacement_rescan: bool,
    pub start_offset: u64,
    pub generation: u64,
    pub mtime: u64,
    frames: &'scan [SharedJsonlFrame],
}

impl JsonlObservationScan<'_> {
    /// Raw frames in this exact bounded admission page. Host-specific state
    /// may use this view for decisions that require pairing two native records
    /// without issuing a second file read or retaining the page bytes.
    pub fn frame_bytes(&self) -> impl Iterator<Item = &[u8]> {
        self.frames.iter().map(|frame| frame.bytes.as_ref())
    }
}

#[derive(Clone, Copy)]
struct JsonlCheckpoint {
    offset: u64,
    end_offset: u64,
    resume_fingerprint: u64,
}

impl JsonlCheckpoint {
    #[hotpath::skip]
    const fn new(offset: u64, end_offset: u64, resume_fingerprint: u64) -> Self {
        Self {
            offset,
            end_offset,
            resume_fingerprint,
        }
    }
}

/// Bounded persist window: flush consecutive durables before this many
/// frames so one `persist_observations` call stays a scan-sized batch.
const MAX_CAPTURE_WINDOW: usize = 256;
const MAX_CAPTURE_WINDOW_BYTES: u64 = 4 * 1024 * 1024;
pub(crate) const SHARED_JSONL_PAGE_MAX_NEW_BYTES: u64 = MAX_JSONL_RECORD_BYTES as u64 + 1;
// One page reads at most 16 MiB. In the adversarial dense-value shape, that
// encoding can hold roughly eight million JSON values across many individually
// valid records. Charging 32 retained bytes per encoded byte covers serde
// value slots, object/map nodes, decoded strings, raw frames, and bounded page
// bookkeeping before any of those allocations are built. The two extra raw
// byte multiples retain the source frames and parser/reorder bookkeeping. The reservation is
// shrunk to the structural meter once transient parser allocations are gone.
const SHARED_JSONL_WORKER_RESERVATION_BYTES: u64 = 544 * 1024 * 1024;

#[derive(Clone)]
struct SharedJsonlPreparationAuthority {
    memory: Arc<ProcessResidentMemoryV1>,
    component: ResidentMemoryComponentIdV1,
}

static SHARED_JSONL_PREPARATION_AUTHORITY: OnceLock<SharedJsonlPreparationAuthority> =
    OnceLock::new();

/// Mount the process-wide JSONL page-preparation authority.
///
/// The first successful install wins. Later calls — including concurrent
/// `OnceLock::set` losers and fixtures that carry a distinct
/// [`ProcessResidentMemoryV1`] Arc — are no-ops. `InvalidFrameState` is a
/// frame-parse failure, not "another caller already mounted preparation".
/// Treating a second installer as a frame error poisons every later
/// host-admission fixture in the same process (the `mcp_suite` cascade).
pub(crate) fn install_shared_jsonl_preparation_authority(
    memory: Arc<ProcessResidentMemoryV1>,
) -> TranscriptIngestResult<()> {
    let authority = SharedJsonlPreparationAuthority {
        memory,
        component: ResidentMemoryComponentIdV1::new("sessions.codex.prepared-pages")
            .map_err(|_| TranscriptIngestError::InvalidFrameState { provider: "codex" })?,
    };
    let _ = SHARED_JSONL_PREPARATION_AUTHORITY.set(authority);
    Ok(())
}

pub(crate) fn shared_jsonl_preparation_workers() -> usize {
    process_background_cpu().map_or(1, |authority| {
        shared_jsonl_preparation_workers_from(authority.width().get())
    })
}

const fn shared_jsonl_preparation_workers_from(installed_width: usize) -> usize {
    // `Ord::max` is not const-stable, so this cannot delegate to it while the
    // function stays `const`.
    if installed_width > 1 {
        installed_width
    } else {
        1
    }
}

pub(crate) fn shared_jsonl_preparation_capacity() -> usize {
    let cpu_width = shared_jsonl_preparation_workers();
    let Some(authority) = SHARED_JSONL_PREPARATION_AUTHORITY.get() else {
        return 1;
    };
    let memory = authority.memory.snapshot();
    shared_jsonl_preparation_capacity_from(cpu_width, memory.limit_bytes, memory.used_bytes)
}

fn shared_jsonl_max_preparation_capacity() -> usize {
    let cpu_width = shared_jsonl_preparation_workers();
    let Some(authority) = SHARED_JSONL_PREPARATION_AUTHORITY.get() else {
        return 1;
    };
    let memory_width =
        authority.memory.snapshot().limit_bytes / SHARED_JSONL_WORKER_RESERVATION_BYTES;
    cpu_width
        .min(memory_width.min(usize::MAX as u64) as usize)
        .max(1)
}

fn shared_jsonl_preparation_capacity_from(
    cpu_width: usize,
    memory_limit_bytes: u64,
    memory_used_bytes: u64,
) -> usize {
    let available_bytes = memory_limit_bytes.saturating_sub(memory_used_bytes);
    let memory_width = available_bytes / SHARED_JSONL_WORKER_RESERVATION_BYTES;
    cpu_width
        .min(memory_width.min(usize::MAX as u64) as usize)
        .max(1)
}

const fn shared_jsonl_speculative_capacity_from(total_capacity: usize) -> usize {
    // Speculative cursor-zero work must never consume the final page
    // reservation. Exact cursor demand (including append and reduced-budget
    // windows) needs one slot that cannot be pinned behind unrelated prefetch.
    total_capacity.saturating_sub(1)
}

pub(in crate::runtime) fn shared_jsonl_background_cpu()
-> TranscriptIngestResult<Arc<ProcessBackgroundCpuV1>> {
    process_background_cpu().ok_or(TranscriptIngestError::BackgroundResourceUnavailable {
        provider: "codex",
        resource: "process background CPU authority",
    })
}

pub(in crate::runtime) fn reserve_shared_jsonl_page()
-> TranscriptIngestResult<Option<ProcessSharedMemoryReservationV1>> {
    reserve_shared_jsonl_bytes(
        SHARED_JSONL_WORKER_RESERVATION_BYTES,
        "shared JSONL page resident-memory capacity",
    )
}

pub(crate) fn reserve_shared_jsonl_bytes(
    bytes: u64,
    resource: &'static str,
) -> TranscriptIngestResult<Option<ProcessSharedMemoryReservationV1>> {
    let Some(authority) = SHARED_JSONL_PREPARATION_AUTHORITY.get() else {
        return Err(TranscriptIngestError::BackgroundResourceUnavailable {
            provider: "codex",
            resource: "process resident-memory authority",
        });
    };
    let bytes = NonZeroU64::new(bytes)
        .ok_or(TranscriptIngestError::InvalidFrameState { provider: "codex" })?;
    authority
        .memory
        .reserve_process_shared(authority.component, bytes)
        .map(Some)
        .map_err(|_| {
            hotpath::gauge!("jsonl_shared_backpressure_memory").inc(1.0);
            TranscriptIngestError::BackgroundResourceUnavailable {
                provider: "codex",
                resource,
            }
        })
}

#[cfg(test)]
pub(in crate::runtime) fn install_test_shared_jsonl_preparation_authority() {
    use std::num::NonZeroU64;
    use std::num::NonZeroUsize;
    use tracedecay_runtime_core::resident_memory::ProcessResidentMemoryV1;

    static MEMORY: OnceLock<Arc<ProcessResidentMemoryV1>> = OnceLock::new();
    let memory = Arc::clone(MEMORY.get_or_init(|| {
        Arc::new(ProcessResidentMemoryV1::new(
            NonZeroU64::new(32 * 1024 * 1024 * 1024).unwrap(),
        ))
    }));
    install_process_background_cpu(NonZeroUsize::new(48).unwrap()).unwrap();
    install_shared_jsonl_preparation_authority(memory).unwrap();
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct SharedJsonlPageKey {
    path: PathBuf,
    position: u64,
    generation: u64,
    max_new_bytes: Option<u64>,
    resume: Option<(u64, u64, u64)>,
    preparation: SharedJsonlFramePreparation,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(in crate::runtime) struct SharedJsonlFileIdentity {
    len: u64,
    modified_nanos: u128,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    changed_secs: i64,
    #[cfg(unix)]
    changed_nanos: i64,
}

#[derive(Clone, Copy)]
pub(in crate::runtime) struct JsonlFrameHints {
    pub may_change_codex_context: bool,
    pub identity_collision_retry: bool,
}

fn jsonl_frame_hints(bytes: &[u8]) -> JsonlFrameHints {
    const TOKEN_LEN: usize = 12;
    const SESSION_META: &[u8; TOKEN_LEN] = b"session_meta";
    const TURN_CONTEXT: &[u8; TOKEN_LEN] = b"turn_context";
    // A JSON escape can spell either discriminator without its literal bytes.
    // Treat every escaped frame as a candidate so this remains a one-sided
    // conservative hint; false positives cost normalization, never coverage.
    let may_change_codex_context = bytes.contains(&b'\\')
        || (bytes.len() >= TOKEN_LEN
            && bytes.windows(TOKEN_LEN).any(|window| {
                window == SESSION_META.as_slice() || window == TURN_CONTEXT.as_slice()
            }));
    JsonlFrameHints {
        may_change_codex_context,
        identity_collision_retry: false,
    }
}

struct SharedJsonlFrame {
    offset: u64,
    end_offset: u64,
    resume_fingerprint: u64,
    bytes: Arc<[u8]>,
    prepared:
        tokio::sync::OnceCell<Result<PreparedObservationRecordV1, ObservationRecordParseErrorV1>>,
    hints: JsonlFrameHints,
}

struct SharedJsonlPage {
    frames: Vec<SharedJsonlFrame>,
    lazy_preparation: tokio::sync::Mutex<()>,
    skipped: Vec<RawJsonlSkippedRange>,
    start_offset: u64,
    read_through: u64,
    file_identity: u64,
    new_cursor: StoredCursor,
    replacement_generation: bool,
    deferred: Option<JsonlFrameDeferral>,
    io: JsonlIoAccounting,
    retained_bytes: u64,
    _prepared_bytes: Option<SharedJsonlPreparedBytesGuard>,
    lazy_prepared_bytes: Mutex<Vec<SharedJsonlPreparedBytesGuard>>,
    lazy_memory: Mutex<Vec<ProcessSharedMemoryReservationV1>>,
    _memory: Option<ProcessSharedMemoryReservationV1>,
}

struct CachedSharedJsonlPage {
    key: SharedJsonlPageKey,
    identity: SharedJsonlFileIdentity,
    page: Arc<SharedJsonlPage>,
    speculative: bool,
}

#[derive(Default)]
struct SharedJsonlPageCache {
    pages: VecDeque<CachedSharedJsonlPage>,
    in_flight: HashMap<SharedJsonlPageKey, Arc<SharedJsonlInFlight>>,
    speculative_in_flight: HashSet<SharedJsonlPageKey>,
    retained_bytes: u64,
}

#[derive(Debug)]
struct SharedJsonlInFlight {
    notify: Arc<Notify>,
    abandoned: std::sync::atomic::AtomicBool,
}

impl SharedJsonlInFlight {
    fn new() -> Self {
        Self {
            notify: Arc::new(Notify::new()),
            abandoned: std::sync::atomic::AtomicBool::new(false),
        }
    }
}

struct SharedJsonlInFlightGuard {
    state: Arc<SharedJsonlInFlight>,
    armed: bool,
}

impl SharedJsonlInFlightGuard {
    fn new(state: Arc<SharedJsonlInFlight>) -> Self {
        Self { state, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for SharedJsonlInFlightGuard {
    fn drop(&mut self) {
        if self.armed {
            self.state
                .abandoned
                .store(true, std::sync::atomic::Ordering::Release);
            self.state.notify.notify_waiters();
        }
    }
}

fn discard_abandoned_shared_jsonl_in_flight(cache: &mut SharedJsonlPageCache) {
    let abandoned = cache
        .in_flight
        .iter()
        .filter(|(_, state)| state.abandoned.load(std::sync::atomic::Ordering::Acquire))
        .map(|(key, _)| key.clone())
        .collect::<Vec<_>>();
    if abandoned.is_empty() {
        return;
    }
    for key in abandoned {
        cache.in_flight.remove(&key);
        cache.speculative_in_flight.remove(&key);
    }
    hotpath::gauge!("jsonl_shared_pages_in_flight").set(cache.in_flight.len() as f64);
}

fn reserve_shared_jsonl_speculative_slot(
    cache: &mut SharedJsonlPageCache,
    key: &SharedJsonlPageKey,
    capacity: usize,
) -> bool {
    let cached = cache.pages.iter().filter(|page| page.speculative).count();
    if cached.saturating_add(cache.speculative_in_flight.len()) >= capacity {
        return false;
    }
    cache.speculative_in_flight.insert(key.clone())
}

static SHARED_JSONL_PAGE_CACHE: OnceLock<tokio::sync::Mutex<SharedJsonlPageCache>> =
    OnceLock::new();
static SHARED_JSONL_PATH_PINS: OnceLock<Mutex<HashMap<PathBuf, usize>>> = OnceLock::new();
static SHARED_JSONL_PREPARED_BYTES: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
static SHARED_JSONL_PEAK_PREPARED_BYTES: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
#[cfg(test)]
static SHARED_JSONL_BUILD_OBSERVERS: OnceLock<
    Mutex<HashMap<PathBuf, std::sync::Weak<SharedJsonlBuildObservation>>>,
> = OnceLock::new();
#[cfg(test)]
static SHARED_JSONL_ACTIVE_FRAME_PREPARATIONS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
#[cfg(test)]
static SHARED_JSONL_PEAK_FRAME_PREPARATIONS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
#[cfg(test)]
static SHARED_JSONL_TOTAL_FRAME_PREPARATIONS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
#[cfg(test)]
static SHARED_JSONL_FRAME_PREPARATIONS_BY_FILE: OnceLock<Mutex<HashMap<u64, usize>>> =
    OnceLock::new();
#[cfg(test)]
static SHARED_JSONL_WAITER_REGISTRATIONS: OnceLock<Mutex<HashMap<PathBuf, usize>>> =
    OnceLock::new();
#[cfg(test)]
static SHARED_JSONL_BUILD_GATES: OnceLock<Mutex<HashMap<PathBuf, Arc<std::sync::Barrier>>>> =
    OnceLock::new();

#[cfg(test)]
#[derive(Default)]
struct SharedJsonlBuildObservation {
    active: std::sync::atomic::AtomicUsize,
    peak: std::sync::atomic::AtomicUsize,
}

#[cfg(test)]
struct SharedJsonlBuildObserver {
    observation: Arc<SharedJsonlBuildObservation>,
    paths: Vec<PathBuf>,
}

#[cfg(test)]
impl SharedJsonlBuildObserver {
    fn for_paths(paths: &[PathBuf]) -> Self {
        let mut unique_paths = Vec::with_capacity(paths.len());
        for path in paths {
            if !unique_paths.contains(path) {
                unique_paths.push(path.clone());
            }
        }
        let observation = Arc::new(SharedJsonlBuildObservation::default());
        let observers = SHARED_JSONL_BUILD_OBSERVERS.get_or_init(|| Mutex::new(HashMap::new()));
        let mut observers = observers.lock().unwrap_or_else(PoisonError::into_inner);
        for path in &unique_paths {
            assert!(
                observers
                    .get(path)
                    .and_then(std::sync::Weak::upgrade)
                    .is_none(),
                "a shared JSONL path may have only one active build observer"
            );
            observers.insert(path.clone(), Arc::downgrade(&observation));
        }
        drop(observers);
        Self {
            observation,
            paths: unique_paths,
        }
    }

    fn active(&self) -> usize {
        self.observation
            .active
            .load(std::sync::atomic::Ordering::Acquire)
    }

    fn peak(&self) -> usize {
        self.observation
            .peak
            .load(std::sync::atomic::Ordering::Acquire)
    }
}

#[cfg(test)]
impl Drop for SharedJsonlBuildObserver {
    fn drop(&mut self) {
        let observers = SHARED_JSONL_BUILD_OBSERVERS.get_or_init(|| Mutex::new(HashMap::new()));
        let mut observers = observers.lock().unwrap_or_else(PoisonError::into_inner);
        for path in &self.paths {
            let belongs_to_observer = observers
                .get(path)
                .and_then(std::sync::Weak::upgrade)
                .is_some_and(|observation| Arc::ptr_eq(&observation, &self.observation));
            if belongs_to_observer {
                observers.remove(path);
            }
        }
    }
}

#[cfg(test)]
struct SharedJsonlBuildGuard {
    observation: Option<Arc<SharedJsonlBuildObservation>>,
}

#[cfg(test)]
impl SharedJsonlBuildGuard {
    fn enter(path: &Path) -> Self {
        use std::sync::atomic::Ordering;

        let observers = SHARED_JSONL_BUILD_OBSERVERS.get_or_init(|| Mutex::new(HashMap::new()));
        let mut observers = observers.lock().unwrap_or_else(PoisonError::into_inner);
        let observation = observers.get(path).and_then(std::sync::Weak::upgrade);
        if observation.is_none() {
            observers.remove(path);
        }
        drop(observers);
        if let Some(observation) = &observation {
            let active = observation.active.fetch_add(1, Ordering::AcqRel) + 1;
            observation.peak.fetch_max(active, Ordering::AcqRel);
        }
        Self { observation }
    }
}

#[cfg(test)]
impl Drop for SharedJsonlBuildGuard {
    fn drop(&mut self) {
        use std::sync::atomic::Ordering;

        if let Some(observation) = &self.observation {
            observation.active.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

#[cfg(test)]
struct SharedJsonlFramePreparationGuard;

#[cfg(test)]
impl SharedJsonlFramePreparationGuard {
    fn enter(file_identity: u64) -> Self {
        use std::sync::atomic::Ordering;

        let active = SHARED_JSONL_ACTIVE_FRAME_PREPARATIONS.fetch_add(1, Ordering::AcqRel) + 1;
        SHARED_JSONL_PEAK_FRAME_PREPARATIONS.fetch_max(active, Ordering::AcqRel);
        SHARED_JSONL_TOTAL_FRAME_PREPARATIONS.fetch_add(1, Ordering::AcqRel);
        let preparations =
            SHARED_JSONL_FRAME_PREPARATIONS_BY_FILE.get_or_init(|| Mutex::new(HashMap::new()));
        let mut preparations = preparations.lock().unwrap_or_else(PoisonError::into_inner);
        let count = preparations.entry(file_identity).or_default();
        *count = count.saturating_add(1);
        Self
    }
}

#[cfg(test)]
fn shared_jsonl_frame_preparations_for_test(file_identity: u64) -> usize {
    SHARED_JSONL_FRAME_PREPARATIONS_BY_FILE
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&file_identity)
        .copied()
        .unwrap_or_default()
}

#[cfg(test)]
impl Drop for SharedJsonlFramePreparationGuard {
    fn drop(&mut self) {
        SHARED_JSONL_ACTIVE_FRAME_PREPARATIONS.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    }
}

struct SharedJsonlPreparationWaitGuard;

impl SharedJsonlPreparationWaitGuard {
    fn new() -> Self {
        hotpath::gauge!("jsonl_shared_prep_waiting").inc(1.0);
        Self
    }
}

impl Drop for SharedJsonlPreparationWaitGuard {
    fn drop(&mut self) {
        hotpath::gauge!("jsonl_shared_prep_waiting").dec(1.0);
    }
}

struct SharedJsonlPreparationActiveGuard;

impl Drop for SharedJsonlPreparationActiveGuard {
    fn drop(&mut self) {
        hotpath::gauge!("jsonl_shared_prep_active").dec(1.0);
    }
}

struct SharedJsonlQueuedPathGuard;

impl SharedJsonlQueuedPathGuard {
    fn new() -> Self {
        hotpath::gauge!("jsonl_shared_generation_paths_queued").inc(1.0);
        hotpath::gauge!("jsonl_shared_generation_paths_total").inc(1.0);
        Self
    }
}

impl Drop for SharedJsonlQueuedPathGuard {
    fn drop(&mut self) {
        hotpath::gauge!("jsonl_shared_generation_paths_queued").dec(1.0);
        hotpath::gauge!("jsonl_shared_generation_paths_completed").inc(1.0);
    }
}

struct SharedJsonlPreparedBytesGuard {
    bytes: u64,
}

impl SharedJsonlPreparedBytesGuard {
    fn new(bytes: u64) -> Self {
        use std::sync::atomic::Ordering;

        let current = SHARED_JSONL_PREPARED_BYTES
            .fetch_add(bytes, Ordering::AcqRel)
            .saturating_add(bytes);
        let previous_peak = SHARED_JSONL_PEAK_PREPARED_BYTES.fetch_max(current, Ordering::AcqRel);
        hotpath::gauge!("jsonl_shared_prepared_bytes_current").inc(bytes as f64);
        if current > previous_peak {
            hotpath::gauge!("jsonl_shared_prepared_bytes_peak").set(current as f64);
        }
        Self { bytes }
    }
}

impl Drop for SharedJsonlPreparedBytesGuard {
    fn drop(&mut self) {
        SHARED_JSONL_PREPARED_BYTES.fetch_sub(self.bytes, std::sync::atomic::Ordering::AcqRel);
        hotpath::gauge!("jsonl_shared_prepared_bytes_current").dec(self.bytes as f64);
    }
}

#[derive(Debug)]
pub(crate) struct SharedJsonlPathPin {
    paths: Vec<PathBuf>,
    prefetch_started: std::sync::atomic::AtomicBool,
    cancelled: Arc<std::sync::atomic::AtomicBool>,
    prefetches: Mutex<Vec<tokio::task::AbortHandle>>,
}

pub(crate) fn pin_shared_jsonl_paths(paths: &[PathBuf]) -> SharedJsonlPathPin {
    let mut canonical = Vec::with_capacity(paths.len());
    for path in paths {
        let Ok(path) = std::fs::canonicalize(path) else {
            continue;
        };
        if !canonical.contains(&path) {
            canonical.push(path);
        }
    }
    let pins = SHARED_JSONL_PATH_PINS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut pins = pins.lock().unwrap_or_else(PoisonError::into_inner);
    for path in &canonical {
        let count = pins.entry(path.clone()).or_default();
        *count = count.saturating_add(1);
    }
    SharedJsonlPathPin {
        paths: canonical,
        prefetch_started: std::sync::atomic::AtomicBool::new(false),
        cancelled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        prefetches: Mutex::new(Vec::new()),
    }
}

impl SharedJsonlPathPin {
    pub(crate) fn start_prefetches(&self, paths: &[PathBuf]) {
        if self
            .prefetch_started
            .swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            return;
        }
        self.prefetches
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend(start_shared_jsonl_page_prefetch_with_cancellation(
                paths,
                Some(Arc::clone(&self.cancelled)),
            ));
    }
}

impl Drop for SharedJsonlPathPin {
    fn drop(&mut self) {
        self.cancelled
            .store(true, std::sync::atomic::Ordering::Release);
        for prefetch in self
            .prefetches
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .drain(..)
        {
            prefetch.abort();
        }
        let pins = SHARED_JSONL_PATH_PINS.get_or_init(|| Mutex::new(HashMap::new()));
        let mut pins = pins.lock().unwrap_or_else(PoisonError::into_inner);
        for path in &self.paths {
            if let Some(count) = pins.get_mut(path) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    pins.remove(path);
                }
            }
        }
    }
}

fn shared_jsonl_path_is_pinned(path: &Path) -> bool {
    SHARED_JSONL_PATH_PINS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .contains_key(path)
}

pub(in crate::runtime) fn shared_jsonl_file_identity(
    path: &Path,
) -> TranscriptIngestResult<SharedJsonlFileIdentity> {
    let metadata = std::fs::metadata(path).map_err(|source| TranscriptIngestError::ScanIo {
        operation: "stat shared JSONL page",
        path: path.to_path_buf(),
        source,
    })?;
    let modified_nanos = metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |value| value.as_nanos());
    Ok(SharedJsonlFileIdentity {
        len: metadata.len(),
        modified_nanos,
        #[cfg(unix)]
        device: metadata.dev(),
        #[cfg(unix)]
        inode: metadata.ino(),
        #[cfg(unix)]
        changed_secs: metadata.ctime(),
        #[cfg(unix)]
        changed_nanos: metadata.ctime_nsec(),
    })
}

struct SharedJsonlBuildOptions {
    prepare_frames: bool,
    background_cpu: Arc<ProcessBackgroundCpuV1>,
    memory: Option<ProcessSharedMemoryReservationV1>,
    cancellation: Option<Arc<std::sync::atomic::AtomicBool>>,
}

fn build_shared_jsonl_page(
    path: PathBuf,
    previous: StoredCursor,
    max_new_bytes: Option<u64>,
    resume_state: Option<JsonlResumeState>,
    options: SharedJsonlBuildOptions,
) -> TranscriptIngestResult<Arc<SharedJsonlPage>> {
    let SharedJsonlBuildOptions {
        prepare_frames,
        background_cpu,
        mut memory,
        cancellation,
    } = options;
    #[cfg(test)]
    let build_gate = {
        SHARED_JSONL_BUILD_GATES
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&path)
            .cloned()
    };
    #[cfg(test)]
    let _build_guard = SharedJsonlBuildGuard::enter(&path);
    #[cfg(test)]
    if let Some(gate) = build_gate {
        gate.wait();
    }
    let raw = {
        let _permit = if let Some(permit) = background_cpu.try_acquire() {
            permit
        } else {
            hotpath::gauge!("jsonl_shared_backpressure_cpu").inc(1.0);
            let waiting = SharedJsonlPreparationWaitGuard::new();
            let permit = background_cpu.acquire();
            drop(waiting);
            permit
        };
        hotpath::gauge!("jsonl_shared_prep_active").inc(1.0);
        let _active = SharedJsonlPreparationActiveGuard;
        try_stream_new_jsonl_raw_strict_with_resume(
            &path,
            previous,
            max_new_bytes,
            MAX_JSONL_RECORD_BYTES,
            resume_state,
        )?
    };
    #[cfg(test)]
    let preparation_file_identity = raw.file_identity;
    if cancellation
        .as_ref()
        .is_some_and(|cancelled| cancelled.load(std::sync::atomic::Ordering::Acquire))
    {
        return Err(TranscriptIngestError::Cancelled { provider: "codex" });
    }
    let prepare = |frame: RawJsonlRecord| {
        let prepare_frame = || {
            if cancellation
                .as_ref()
                .is_some_and(|cancelled| cancelled.load(std::sync::atomic::Ordering::Acquire))
            {
                return Err(TranscriptIngestError::Cancelled { provider: "codex" });
            }
            let range =
                tracedecay_domain::ObservationSourceRangeV1::new(frame.offset, frame.end_offset)?;
            let bytes: Arc<[u8]> = frame.bytes.into();
            let hints = jsonl_frame_hints(bytes.as_ref());
            let prepared = tokio::sync::OnceCell::new();
            if prepare_frames {
                #[cfg(test)]
                let _frame_preparation =
                    SharedJsonlFramePreparationGuard::enter(preparation_file_identity);
                let result = prepare_observation_record_v1(
                    bytes.as_ref(),
                    range,
                    ObservationOrderingDomainV1::FileBytes,
                );
                prepared
                    .set(result)
                    .map_err(|_| TranscriptIngestError::InvalidFrameState { provider: "jsonl" })?;
            }
            Ok(SharedJsonlFrame {
                offset: frame.offset,
                end_offset: frame.end_offset,
                resume_fingerprint: frame.resume_fingerprint,
                bytes,
                prepared,
                hints,
            })
        };
        let _permit = if let Some(permit) = background_cpu.try_acquire() {
            permit
        } else {
            hotpath::gauge!("jsonl_shared_backpressure_cpu").inc(1.0);
            let waiting = SharedJsonlPreparationWaitGuard::new();
            let permit = background_cpu.acquire();
            drop(waiting);
            permit
        };
        hotpath::gauge!("jsonl_shared_prep_active").inc(1.0);
        let _active = SharedJsonlPreparationActiveGuard;
        prepare_frame()
    };
    let frames = if prepare_frames {
        raw.frames
            .into_par_iter()
            .map(prepare)
            .collect::<TranscriptIngestResult<Vec<_>>>()
    } else {
        raw.frames
            .into_iter()
            .map(prepare)
            .collect::<TranscriptIngestResult<Vec<_>>>()
    };
    let frames = frames?;
    if prepare_frames && !frames.is_empty() {
        hotpath::gauge!("jsonl_shared_frames_prepared")
            .inc(frames.len().min(u32::MAX as usize) as f64);
    }
    let retained_container_bytes = std::mem::size_of::<SharedJsonlPage>()
        .saturating_add(std::mem::size_of::<CachedSharedJsonlPage>())
        .saturating_add(std::mem::size_of::<SharedJsonlPageKey>())
        .saturating_add(crate::runtime::source::path_byte_len(&path))
        .saturating_add(
            frames
                .capacity()
                .saturating_mul(std::mem::size_of::<SharedJsonlFrame>()),
        )
        .saturating_add(
            raw.skipped
                .capacity()
                .saturating_mul(std::mem::size_of::<RawJsonlSkippedRange>()),
        );
    let retained_container_bytes = u64::try_from(retained_container_bytes)
        .map_err(|_| TranscriptIngestError::InvalidFrameState { provider: "codex" })?;
    let retained_bytes = frames.iter().try_fold(
        retained_container_bytes,
        |total, frame| -> TranscriptIngestResult<u64> {
            let raw_bytes = u64::try_from(frame.bytes.len())
                .map_err(|_| TranscriptIngestError::InvalidFrameState { provider: "codex" })?;
            total
                .checked_add(raw_bytes)
                .and_then(|total| {
                    total.checked_add(
                        frame
                            .prepared
                            .get()
                            .and_then(|prepared| prepared.as_ref().ok())
                            .map_or(0, PreparedObservationRecordV1::retained_bytes),
                    )
                })
                .ok_or(TranscriptIngestError::InvalidFrameState { provider: "codex" })
        },
    )?;
    if let Some(reservation) = &mut memory {
        reservation
            .shrink_to(retained_bytes)
            .map_err(|_| TranscriptIngestError::InvalidFrameState { provider: "codex" })?;
    }
    let prepared_bytes = prepare_frames.then(|| SharedJsonlPreparedBytesGuard::new(retained_bytes));
    hotpath::gauge!("jsonl_shared_page_read_bytes").inc(
        raw.io
            .identity_window_bytes
            .saturating_add(raw.io.prefix_validation_bytes)
            .saturating_add(raw.io.snapshot_hash_bytes)
            .saturating_add(raw.io.scan_payload_read_bytes) as f64,
    );
    Ok(Arc::new(SharedJsonlPage {
        frames,
        lazy_preparation: tokio::sync::Mutex::new(()),
        skipped: raw.skipped,
        start_offset: raw.start_offset,
        read_through: raw.read_through,
        file_identity: raw.file_identity,
        new_cursor: raw.new_cursor,
        replacement_generation: raw.replacement_generation,
        deferred: raw.deferred,
        io: raw.io,
        retained_bytes,
        _prepared_bytes: prepared_bytes,
        lazy_prepared_bytes: Mutex::new(Vec::new()),
        lazy_memory: Mutex::new(Vec::new()),
        _memory: memory,
    }))
}

async fn prepare_shared_jsonl_window(
    page: &SharedJsonlPage,
    start: usize,
    cancellation: &ObservationCancellation,
    provider: &'static str,
) -> TranscriptIngestResult<()> {
    let background_cpu = shared_jsonl_background_cpu()?;
    prepare_shared_jsonl_window_with_background_cpu(
        page,
        start,
        cancellation,
        provider,
        background_cpu,
    )
    .await
}

async fn prepare_shared_jsonl_window_with_background_cpu(
    page: &SharedJsonlPage,
    start: usize,
    cancellation: &ObservationCancellation,
    provider: &'static str,
    background_cpu: Arc<ProcessBackgroundCpuV1>,
) -> TranscriptIngestResult<()> {
    if cancellation.is_cancelled() {
        return Err(TranscriptIngestError::Cancelled { provider });
    }
    let _preparation = tokio::select! {
        biased;
        () = cancellation.cancelled() => {
            return Err(TranscriptIngestError::Cancelled { provider });
        }
        preparation = page.lazy_preparation.lock() => preparation,
    };
    if cancellation.is_cancelled() {
        return Err(TranscriptIngestError::Cancelled { provider });
    }
    let mut jobs = Vec::new();
    let mut job_bytes = 0_u64;
    for (index, frame) in page.frames.iter().enumerate().skip(start) {
        if index > start && frame.hints.may_change_codex_context {
            break;
        }
        if frame.prepared.get().is_none() {
            let frame_bytes = u64::try_from(frame.bytes.len())
                .map_err(|_| TranscriptIngestError::InvalidFrameState { provider })?;
            if !jobs.is_empty() && job_bytes.saturating_add(frame_bytes) > MAX_CAPTURE_WINDOW_BYTES
            {
                break;
            }
            let range =
                tracedecay_domain::ObservationSourceRangeV1::new(frame.offset, frame.end_offset)?;
            jobs.push((index, Arc::clone(&frame.bytes), range));
            job_bytes = job_bytes.saturating_add(frame_bytes);
        }
        if frame.hints.may_change_codex_context
            || index.saturating_sub(start) + 1 >= MAX_CAPTURE_WINDOW
        {
            break;
        }
    }
    if jobs.is_empty() {
        return Ok(());
    }
    let mut preparation_memory = reserve_shared_jsonl_bytes(
        SHARED_JSONL_WORKER_RESERVATION_BYTES,
        "lazy shared JSONL preparation resident-memory capacity",
    )?;
    let task_cancellation = cancellation.clone();
    #[cfg(test)]
    let preparation_file_identity = page.file_identity;
    let prepared = tokio::task::spawn_blocking(move || {
        jobs.into_par_iter()
            .map(|(index, bytes, range)| {
                if task_cancellation.is_cancelled() {
                    return Err(TranscriptIngestError::Cancelled { provider });
                }
                let _permit = if let Some(permit) = background_cpu.try_acquire() {
                    permit
                } else {
                    hotpath::gauge!("jsonl_shared_backpressure_cpu").inc(1.0);
                    let waiting = SharedJsonlPreparationWaitGuard::new();
                    let permit = background_cpu
                        .acquire_cancellable(task_cancellation.cancellation_flag())
                        .ok_or(TranscriptIngestError::Cancelled { provider })?;
                    drop(waiting);
                    permit
                };
                if task_cancellation.is_cancelled() {
                    return Err(TranscriptIngestError::Cancelled { provider });
                }
                hotpath::gauge!("jsonl_shared_prep_active").inc(1.0);
                let _active = SharedJsonlPreparationActiveGuard;
                #[cfg(test)]
                let _frame_preparation =
                    SharedJsonlFramePreparationGuard::enter(preparation_file_identity);
                Ok((
                    index,
                    prepare_observation_record_v1(
                        bytes.as_ref(),
                        range,
                        ObservationOrderingDomainV1::FileBytes,
                    ),
                ))
            })
            .collect::<TranscriptIngestResult<Vec<_>>>()
    })
    .await
    .map_err(|_| TranscriptIngestError::BlockingScanTaskFailed { provider })??;
    if cancellation.is_cancelled() {
        return Err(TranscriptIngestError::Cancelled { provider });
    }
    let prepared_count = prepared.len();
    let newly_prepared_bytes = prepared.iter().fold(0_u64, |total, (_, prepared)| {
        total.saturating_add(
            prepared
                .as_ref()
                .map_or(0, PreparedObservationRecordV1::retained_bytes),
        )
    });
    if let Some(reservation) = &mut preparation_memory {
        reservation
            .shrink_to(newly_prepared_bytes)
            .map_err(|_| TranscriptIngestError::InvalidFrameState { provider })?;
    }
    for (index, prepared) in prepared {
        page.frames[index]
            .prepared
            .set(prepared)
            .map_err(|_| TranscriptIngestError::InvalidFrameState { provider })?;
    }
    if let Some(reservation) = preparation_memory
        && reservation.reserved_bytes() != 0
    {
        page.lazy_memory
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(reservation);
    }
    let mut lazy_prepared_bytes = page
        .lazy_prepared_bytes
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let retained_bytes = newly_prepared_bytes.saturating_add(
        if lazy_prepared_bytes.is_empty() && page._prepared_bytes.is_none() {
            page.retained_bytes
        } else {
            0
        },
    );
    if retained_bytes != 0 {
        lazy_prepared_bytes.push(SharedJsonlPreparedBytesGuard::new(retained_bytes));
    }
    hotpath::gauge!("jsonl_shared_frames_prepared")
        .inc(prepared_count.min(u32::MAX as usize) as f64);
    Ok(())
}

const fn preparation_failure_reason(
    error: ObservationRecordParseErrorV1,
) -> ObservationCoverageReason {
    match error {
        ObservationRecordParseErrorV1::Empty => ObservationCoverageReason::BlankFrame,
        ObservationRecordParseErrorV1::TooLarge
        | ObservationRecordParseErrorV1::CanonicalEnvelopeTooLarge => {
            ObservationCoverageReason::OversizedFrame
        }
        _ => ObservationCoverageReason::MalformedFrame,
    }
}

#[derive(Default)]
struct SharedJsonlCancellation {
    blocking: Option<Arc<std::sync::atomic::AtomicBool>>,
    operation: Option<ObservationCancellation>,
}

#[cfg(test)]
async fn shared_jsonl_page(
    path: &Path,
    previous: StoredCursor,
    max_new_bytes: Option<u64>,
    resume_state: Option<JsonlResumeState>,
    preparation: impl Into<SharedJsonlFramePreparation>,
) -> TranscriptIngestResult<(Arc<SharedJsonlPage>, bool)> {
    shared_jsonl_page_with_cancellation(
        path,
        previous,
        max_new_bytes,
        resume_state,
        preparation,
        SharedJsonlCancellation::default(),
        false,
    )
    .await
}

async fn shared_jsonl_page_with_cancellation(
    path: &Path,
    previous: StoredCursor,
    max_new_bytes: Option<u64>,
    resume_state: Option<JsonlResumeState>,
    preparation: impl Into<SharedJsonlFramePreparation>,
    cancellation: SharedJsonlCancellation,
    speculative: bool,
) -> TranscriptIngestResult<(Arc<SharedJsonlPage>, bool)> {
    let preparation = preparation.into();
    let SharedJsonlCancellation {
        blocking: cancellation,
        operation: operation_cancellation,
    } = cancellation;
    let identity_path = path.to_path_buf();
    let (canonical_path, identity) = tokio::task::spawn_blocking(move || {
        let canonical_path = std::fs::canonicalize(&identity_path).map_err(|source| {
            TranscriptIngestError::ScanIo {
                operation: "resolve shared JSONL page identity",
                path: identity_path.clone(),
                source,
            }
        })?;
        Ok::<_, TranscriptIngestError>((
            canonical_path,
            shared_jsonl_file_identity(&identity_path)?,
        ))
    })
    .await
    .map_err(|_| TranscriptIngestError::BlockingScanTaskFailed { provider: "jsonl" })??;
    // Preparation policy is part of the page key. Codex prefetch and demand
    // both select `Lazy`, so a pinned page stays raw until ordered admission
    // proves which frames can affect scope. Explicit eager consumers retain
    // their own cache entry and cannot make the scope-first path pay a decode.
    let prepare_frames_eagerly = preparation == SharedJsonlFramePreparation::EagerWhenPinned
        && shared_jsonl_path_is_pinned(&canonical_path);
    let key = SharedJsonlPageKey {
        path: canonical_path,
        position: previous.position,
        generation: previous.file_id,
        max_new_bytes,
        resume: resume_state
            .map(|resume| (resume.generation, resume.file_identity, resume.fingerprint)),
        preparation,
    };
    let cache_lock = SHARED_JSONL_PAGE_CACHE.get_or_init(tokio::sync::Mutex::default);
    let in_flight = loop {
        let mut cache = cache_lock.lock().await;
        discard_abandoned_shared_jsonl_in_flight(&mut cache);
        if let Some(index) = cache
            .pages
            .iter()
            .position(|entry| entry.key == key && entry.identity == identity)
        {
            let mut cached = cache
                .pages
                .remove(index)
                .ok_or(TranscriptIngestError::InvalidFrameState { provider: "jsonl" })?;
            if !speculative {
                cached.speculative = false;
            }
            let page = Arc::clone(&cached.page);
            cache.pages.push_back(cached);
            hotpath::gauge!("jsonl_shared_page_hits").inc(1.0);
            return Ok((page, true));
        }
        if let Some(in_flight) = cache.in_flight.get(&key) {
            hotpath::gauge!("jsonl_shared_page_waits").inc(1.0);
            let notified = Arc::clone(&in_flight.notify).notified_owned();
            tokio::pin!(notified);
            notified.as_mut().enable();
            #[cfg(test)]
            {
                let registrations =
                    SHARED_JSONL_WAITER_REGISTRATIONS.get_or_init(|| Mutex::new(HashMap::new()));
                let mut registrations =
                    registrations.lock().unwrap_or_else(PoisonError::into_inner);
                let count = registrations.entry(key.path.clone()).or_default();
                *count = count.saturating_add(1);
            }
            drop(cache);
            if let Some(cancellation) = &operation_cancellation {
                tokio::select! {
                    () = notified.as_mut() => {}
                    () = tokio::time::sleep(std::time::Duration::from_millis(10)) => {
                        if cancellation.is_cancelled() {
                            return Err(TranscriptIngestError::Cancelled { provider: "jsonl" });
                        }
                    }
                }
            } else {
                notified.as_mut().await;
            }
            continue;
        }
        if speculative
            && !reserve_shared_jsonl_speculative_slot(
                &mut cache,
                &key,
                shared_jsonl_speculative_capacity_from(shared_jsonl_max_preparation_capacity()),
            )
        {
            hotpath::gauge!("jsonl_shared_backpressure_memory").inc(1.0);
            return Err(TranscriptIngestError::BackgroundResourceUnavailable {
                provider: "codex",
                resource: "shared JSONL speculative preparation capacity",
            });
        }
        let in_flight = Arc::new(SharedJsonlInFlight::new());
        cache.in_flight.insert(key.clone(), Arc::clone(&in_flight));
        hotpath::gauge!("jsonl_shared_page_misses").inc(1.0);
        hotpath::gauge!("jsonl_shared_pages_in_flight").set(cache.in_flight.len() as f64);
        break in_flight;
    };
    let mut in_flight_guard = SharedJsonlInFlightGuard::new(in_flight);
    hotpath::gauge!("jsonl_shared_page_reads").inc(1.0);
    if !speculative {
        let mut cache = cache_lock.lock().await;
        let mut index = 0;
        while index < cache.pages.len() {
            let superseded_prefetch = cache.pages[index].speculative
                && cache.pages[index].key.path == key.path
                && cache.pages[index].key != key;
            if superseded_prefetch {
                if let Some(evicted) = cache.pages.remove(index) {
                    cache.retained_bytes = cache
                        .retained_bytes
                        .saturating_sub(evicted.page.retained_bytes);
                }
            } else {
                index += 1;
            }
        }
    }
    let memory = match reserve_shared_jsonl_page() {
        Ok(memory) => memory,
        Err(error) => {
            let mut cache = cache_lock.lock().await;
            let notify = cache.in_flight.remove(&key);
            cache.speculative_in_flight.remove(&key);
            hotpath::gauge!("jsonl_shared_pages_in_flight").set(cache.in_flight.len() as f64);
            in_flight_guard.disarm();
            drop(cache);
            if let Some(notify) = notify {
                notify.notify.notify_waiters();
            }
            return Err(error);
        }
    };
    let background_cpu = match shared_jsonl_background_cpu() {
        Ok(authority) => authority,
        Err(error) => {
            let mut cache = cache_lock.lock().await;
            let notify = cache.in_flight.remove(&key);
            cache.speculative_in_flight.remove(&key);
            hotpath::gauge!("jsonl_shared_pages_in_flight").set(cache.in_flight.len() as f64);
            in_flight_guard.disarm();
            drop(cache);
            if let Some(notify) = notify {
                notify.notify.notify_waiters();
            }
            return Err(error);
        }
    };
    let scan_path = path.to_path_buf();
    let page = tokio::task::spawn_blocking(move || {
        build_shared_jsonl_page(
            scan_path,
            previous,
            max_new_bytes,
            resume_state,
            SharedJsonlBuildOptions {
                prepare_frames: prepare_frames_eagerly,
                background_cpu,
                memory,
                cancellation,
            },
        )
    })
    .await
    .map_err(|_| TranscriptIngestError::BlockingScanTaskFailed { provider: "jsonl" })
    .and_then(|result| result);
    let mut cache = cache_lock.lock().await;
    let notify = cache.in_flight.remove(&key);
    cache.speculative_in_flight.remove(&key);
    hotpath::gauge!("jsonl_shared_pages_in_flight").set(cache.in_flight.len() as f64);
    in_flight_guard.disarm();
    let page = match page {
        Ok(page) => page,
        Err(error) => {
            drop(cache);
            if let Some(notify) = notify {
                notify.notify.notify_waiters();
            }
            return Err(error);
        }
    };
    let retained_bytes = page.retained_bytes;
    let workers = shared_jsonl_preparation_workers();
    if page._memory.is_some() {
        while cache.pages.len() >= workers {
            let Some(index) = cache
                .pages
                .iter()
                .position(|entry| !shared_jsonl_path_is_pinned(&entry.key.path))
            else {
                break;
            };
            let Some(evicted) = cache.pages.remove(index) else {
                break;
            };
            cache.retained_bytes = cache
                .retained_bytes
                .saturating_sub(evicted.page.retained_bytes);
        }
        let fits = cache.pages.len() < workers;
        if fits {
            cache.retained_bytes = cache.retained_bytes.saturating_add(retained_bytes);
            cache.pages.push_back(CachedSharedJsonlPage {
                key,
                identity,
                page: Arc::clone(&page),
                speculative,
            });
        }
    }
    hotpath::gauge!("jsonl_shared_reorder_window_depth").set(cache.pages.len() as f64);
    drop(cache);
    if let Some(notify) = notify {
        notify.notify.notify_waiters();
    }
    Ok((page, false))
}

/// Starts one generation-owned prepared window. Page tasks run ahead of
/// ordered source admission; the canonical admission call waits on the exact
/// in-flight page it needs and preserves per-source cursor order.
#[cfg(test)]
pub(crate) fn start_shared_jsonl_page_prefetch(paths: &[PathBuf]) -> Vec<tokio::task::AbortHandle> {
    start_shared_jsonl_page_prefetch_with_cancellation(paths, None)
}

fn start_shared_jsonl_page_prefetch_with_cancellation(
    paths: &[PathBuf],
    cancellation: Option<Arc<std::sync::atomic::AtomicBool>>,
) -> Vec<tokio::task::AbortHandle> {
    let workers = shared_jsonl_speculative_capacity_from(shared_jsonl_preparation_capacity());
    let mut prefetches = Vec::with_capacity(workers.min(paths.len()));
    for path in paths.iter().take(workers) {
        let path = path.clone();
        let queued = SharedJsonlQueuedPathGuard::new();
        let cancellation = cancellation.clone();
        let task = tokio::spawn(async move {
            let _queued = queued;
            if let Err(error) = shared_jsonl_page_with_cancellation(
                &path,
                StoredCursor::default(),
                Some(SHARED_JSONL_PAGE_MAX_NEW_BYTES),
                None,
                SharedJsonlFramePreparation::Lazy,
                SharedJsonlCancellation {
                    blocking: cancellation,
                    operation: None,
                },
                true,
            )
            .await
            {
                if matches!(error, TranscriptIngestError::Cancelled { .. }) {
                    return;
                }
                hotpath::gauge!("jsonl_shared_generation_retries").inc(1.0);
                tracing::debug!(
                    provider = "codex",
                    error = %error,
                    "shared JSONL prefetch deferred to canonical admission"
                );
            }
        });
        prefetches.push(task.abort_handle());
    }
    prefetches
}

struct DurableJsonlFrame {
    checkpoint: JsonlCheckpoint,
    range: tracedecay_domain::ObservationSourceRangeV1,
    parsed_record: ParsedObservationRecordV1,
    native_record_id: ObservationId,
    skip_if_observation_exists: Option<CanonicalObservationIdV1>,
    bytes: Arc<[u8]>,
    fallback_prepared: Option<PreparedObservationRecordV1>,
    fallback_hints: JsonlFrameHints,
    identity_collision_disposition: ObservationIdentityCollisionDispositionV1,
}

struct PendingAdmissionWindow<'window, State> {
    expected_cursor: &'window mut Option<ObservationSourceCursorV1>,
    frames: &'window mut Vec<DurableJsonlFrame>,
    bytes: &'window mut u64,
    start_state: &'window mut Option<State>,
    progress: &'window mut JsonlObservationAdmissionProgress,
}

enum CaptureWindowError {
    ScalarFallback(HostAdmissionRecovery),
    Ingest(TranscriptIngestError),
}

impl From<TranscriptIngestError> for CaptureWindowError {
    fn from(error: TranscriptIngestError) -> Self {
        Self::Ingest(error)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DurableFrameDisposition {
    Persisted,
    Refused,
    AlreadyDurable,
}

struct ActiveAdmission<'request> {
    provider: &'static str,
    admission: &'request dyn HostAdmission,
    source: ObservationSourceIdentityV1,
    scope: ObservationScopeV1,
    generation: ObservationSourceGenerationV1,
    file_identity: u64,
    cancellation: ObservationCancellation,
}

impl ActiveAdmission<'_> {
    fn cursor_at(
        &self,
        end_offset: u64,
        resume_fingerprint: u64,
    ) -> TranscriptIngestResult<ObservationSourceCursorV1> {
        Ok(ObservationSourceCursorV1::for_ordering(
            self.source.clone(),
            self.scope.clone(),
            self.generation,
            ObservationOrderingDomainV1::FileBytes,
            end_offset,
        )?
        .with_resume_checkpoint(self.file_identity, resume_fingerprint))
    }

    #[hotpath::skip]
    async fn advance_coverage(
        &self,
        expected_cursor: &mut Option<ObservationSourceCursorV1>,
        checkpoint: JsonlCheckpoint,
        reason: ObservationCoverageReason,
        receipt: Option<SanitizationReceiptV1>,
    ) -> TranscriptIngestResult<()> {
        let range = tracedecay_domain::ObservationSourceRangeV1::new(
            checkpoint.offset,
            checkpoint.end_offset,
        )?;
        let advance = match receipt {
            Some(receipt) => ObservationCursorAdvance::for_ordering_with_sanitization_receipt(
                self.source.clone(),
                self.scope.clone(),
                self.generation,
                ObservationOrderingDomainV1::FileBytes,
                expected_cursor.clone(),
                range,
                reason,
                receipt,
            ),
            None => ObservationCursorAdvance::for_ordering(
                self.source.clone(),
                self.scope.clone(),
                self.generation,
                ObservationOrderingDomainV1::FileBytes,
                expected_cursor.clone(),
                range,
                reason,
            ),
        }
        .map_err(|_| TranscriptIngestError::InvalidFrameState {
            provider: self.provider,
        })?
        .with_resume_checkpoint(self.file_identity, checkpoint.resume_fingerprint);
        hotpath::gauge!("jsonl_admission_coverage_frames").inc(1.0);
        hotpath::gauge!("jsonl_admission_writer_submits").inc(1.0);
        self.admission
            .advance_non_durable_source_cursor(advance, self.cancellation.clone())
            .await
            .map_err(|outcome| {
                if is_admission_cancellation(&outcome, &self.cancellation) {
                    TranscriptIngestError::Cancelled {
                        provider: self.provider,
                    }
                } else if outcome.retryable {
                    // A retryable advance failure — a cursor CAS lost to a
                    // peer ingestor, a still-mounting write authority — says
                    // nothing about the record; wrapping it as NonDurable
                    // laundered the admission's own verdict into a terminal
                    // non-retryable disposition.
                    host_admission_error(self.provider, outcome)
                } else {
                    TranscriptIngestError::NonDurableRecord {
                        provider: self.provider,
                        offset: checkpoint.offset,
                        end_offset: checkpoint.end_offset,
                        reason: outcome
                            .reason_code
                            .unwrap_or("non_durable_cursor_advance_failed"),
                    }
                }
            })?;
        *expected_cursor =
            Some(self.cursor_at(checkpoint.end_offset, checkpoint.resume_fingerprint)?);
        Ok(())
    }

    fn capture_request(
        &self,
        expected_cursor: Option<ObservationSourceCursorV1>,
        frame: DurableJsonlFrame,
        retention_class: &RetentionClass,
    ) -> TranscriptIngestResult<CaptureObservationRequest> {
        let identity = ObservationIdentityMaterialV1::for_native_record(
            self.source.clone(),
            self.scope.clone(),
            self.generation,
            frame.range,
            ObservationOrderingDomainV1::FileBytes,
            frame.native_record_id,
        )?;
        CaptureObservationRequest::new(
            frame.parsed_record,
            identity,
            expected_cursor,
            retention_class.clone(),
            self.cancellation.clone(),
        )
        .map_err(|_| TranscriptIngestError::InvalidFrameState {
            provider: self.provider,
        })
        .map(|request| {
            request
                .with_resume_checkpoint(self.file_identity, frame.checkpoint.resume_fingerprint)
                .with_identity_collision_disposition(frame.identity_collision_disposition)
        })
    }

    #[hotpath::skip]
    async fn apply_capture_result(
        &self,
        expected_cursor: &mut Option<ObservationSourceCursorV1>,
        checkpoint: JsonlCheckpoint,
        result: Result<CaptureObservationOutcome, HostAdmissionOutcome>,
        persisted_cursor_update: PersistedCursorUpdate,
    ) -> TranscriptIngestResult<DurableFrameDisposition> {
        match result {
            Ok(CaptureObservationOutcome::Persisted { .. })
            | Ok(CaptureObservationOutcome::AcceptedForReplay { .. }) => {
                let should_update = match persisted_cursor_update {
                    PersistedCursorUpdate::Replace => true,
                    PersistedCursorUpdate::Monotonic => {
                        expected_cursor.as_ref().is_none_or(|cursor| {
                            cursor.generation() != self.generation
                                || cursor.position() < checkpoint.end_offset
                        })
                    }
                };
                if should_update {
                    *expected_cursor =
                        Some(self.cursor_at(checkpoint.end_offset, checkpoint.resume_fingerprint)?);
                }
                Ok(DurableFrameDisposition::Persisted)
            }
            Ok(CaptureObservationOutcome::Rejected { receipt, .. }) => {
                self.advance_coverage(
                    expected_cursor,
                    checkpoint,
                    ObservationCoverageReason::SanitizerRejected,
                    Some(receipt),
                )
                .await?;
                Ok(DurableFrameDisposition::Refused)
            }
            Ok(CaptureObservationOutcome::Quarantined { receipt, .. }) => {
                self.advance_coverage(
                    expected_cursor,
                    checkpoint,
                    ObservationCoverageReason::SanitizerQuarantined,
                    Some(receipt),
                )
                .await?;
                Ok(DurableFrameDisposition::Refused)
            }
            // Deterministic content refusals re-fail identically forever;
            // advance coverage with a durable typed reason so the stream
            // converges instead of re-reporting the same records every sweep.
            Err(outcome)
                if is_deterministic_content_refusal(&outcome)
                    && !self.cancellation.is_cancelled() =>
            {
                tracing::warn!(
                    provider = self.provider,
                    offset = checkpoint.offset,
                    reason = outcome.reason_code.unwrap_or("host_admission_refused"),
                    "admission refused a record; covering past it"
                );
                self.advance_coverage(
                    expected_cursor,
                    checkpoint,
                    if outcome.reason_code == Some("observation_identity_collision") {
                        ObservationCoverageReason::ObservationIdentityCollision
                    } else {
                        ObservationCoverageReason::AdmissionRefused
                    },
                    None,
                )
                .await?;
                Ok(DurableFrameDisposition::Refused)
            }
            Err(outcome) => {
                if outcome.status == HostAdmissionStatus::Backpressured {
                    hotpath::gauge!("jsonl_admission_backpressure_writer").inc(1.0);
                }
                if is_admission_cancellation(&outcome, &self.cancellation) {
                    Err(TranscriptIngestError::Cancelled {
                        provider: self.provider,
                    })
                } else {
                    // Everything else says nothing about the record's
                    // content: commit/read-back failures
                    // (`observation_commit_failed`,
                    // `authority_write_failed`,
                    // `observation_persisted_value_unavailable`), unbound
                    // authorities, and retryable races keep the admission
                    // authority's own verdict as a typed block. The frontier
                    // must not advance over a record whose durable fate is
                    // unknown — the persist may already have committed and
                    // advanced the source cursor, so a cover-past write here
                    // would stack a second, conflicting cursor advance on
                    // every frame.
                    Err(host_admission_error(self.provider, outcome))
                }
            }
        }
    }

    #[hotpath::skip]
    async fn capture(
        &self,
        expected_cursor: &mut Option<ObservationSourceCursorV1>,
        frame: DurableJsonlFrame,
        retention_class: &RetentionClass,
        persisted_cursor_update: PersistedCursorUpdate,
    ) -> TranscriptIngestResult<DurableFrameDisposition> {
        let checkpoint = frame.checkpoint;
        let existing_receipt = match frame.skip_if_observation_exists.as_ref() {
            Some(observation_id) => self
                .admission
                .observation_receipt(
                    self.provider,
                    &self.scope,
                    observation_id,
                    &self.cancellation,
                )
                .await
                .map_err(|outcome| host_admission_error(self.provider, outcome))?,
            None => None,
        };
        if let Some(receipt) = existing_receipt {
            self.advance_coverage(
                expected_cursor,
                checkpoint,
                ObservationCoverageReason::DuplicateObservation,
                Some(receipt),
            )
            .await?;
            return Ok(DurableFrameDisposition::AlreadyDurable);
        }
        let result = self
            .capture_attempt(expected_cursor.clone(), frame, retention_class)
            .await?;
        self.apply_capture_result(expected_cursor, checkpoint, result, persisted_cursor_update)
            .await
    }

    #[hotpath::skip]
    async fn capture_attempt(
        &self,
        expected_cursor: Option<ObservationSourceCursorV1>,
        frame: DurableJsonlFrame,
        retention_class: &RetentionClass,
    ) -> TranscriptIngestResult<Result<CaptureObservationOutcome, HostAdmissionOutcome>> {
        crate::runtime::pipeline_metrics::record_capture_single();
        hotpath::gauge!("jsonl_admission_batch_frames").inc(1.0);
        hotpath::gauge!("jsonl_admission_batch_bytes").inc(frame.bytes.len() as f64);
        hotpath::gauge!("jsonl_admission_writer_submits").inc(1.0);
        let request = self.capture_request(expected_cursor, frame, retention_class)?;
        Ok(self.admission.capture_observation(request).await)
    }

    #[hotpath::skip]
    async fn capture_window(
        &self,
        expected_cursor: &mut Option<ObservationSourceCursorV1>,
        frames: Vec<DurableJsonlFrame>,
        retention_class: &RetentionClass,
        persisted_cursor_update: PersistedCursorUpdate,
        progress: &mut JsonlObservationAdmissionProgress,
    ) -> Result<(), CaptureWindowError> {
        if frames.is_empty() {
            return Ok(());
        }
        if frames
            .iter()
            .any(|frame| frame.skip_if_observation_exists.is_some())
        {
            for frame in frames {
                match self
                    .capture(
                        expected_cursor,
                        frame,
                        retention_class,
                        persisted_cursor_update,
                    )
                    .await?
                {
                    DurableFrameDisposition::Persisted => {
                        progress.frames_accepted = progress.frames_accepted.saturating_add(1);
                        progress.frames_persisted = progress.frames_persisted.saturating_add(1);
                    }
                    DurableFrameDisposition::Refused => {
                        progress.frames_refused = progress.frames_refused.saturating_add(1);
                    }
                    DurableFrameDisposition::AlreadyDurable => {
                        progress.frames_skipped = progress.frames_skipped.saturating_add(1);
                    }
                }
            }
            return Ok(());
        }
        crate::runtime::pipeline_metrics::record_capture_window(frames.len());
        let batch_bytes = frames.iter().try_fold(0_u64, |total, frame| {
            let bytes = u64::try_from(frame.bytes.len())
                .map_err(|_| TranscriptIngestError::InvalidFrameState { provider: "codex" })?;
            total
                .checked_add(bytes)
                .ok_or(TranscriptIngestError::InvalidFrameState { provider: "codex" })
        })?;
        hotpath::gauge!("jsonl_admission_batch_frames").inc(frames.len() as f64);
        hotpath::gauge!("jsonl_admission_batch_bytes").inc(batch_bytes as f64);
        hotpath::gauge!("jsonl_admission_writer_submits").inc(1.0);
        let mut batch_expected = expected_cursor.clone();
        let mut requests = Vec::with_capacity(frames.len());
        let mut checkpoints = Vec::with_capacity(frames.len());
        for frame in frames {
            checkpoints.push(frame.checkpoint);
            let next_expected = Some(self.cursor_at(
                frame.checkpoint.end_offset,
                frame.checkpoint.resume_fingerprint,
            )?);
            requests.push(self.capture_request(batch_expected, frame, retention_class)?);
            batch_expected = next_expected;
        }
        match self.admission.capture_observations(requests).await {
            Ok(outcomes) => {
                if outcomes.len() != checkpoints.len() {
                    return Err(CaptureWindowError::Ingest(
                        TranscriptIngestError::InvalidFrameState {
                            provider: self.provider,
                        },
                    ));
                }
                for (checkpoint, outcome) in checkpoints.into_iter().zip(outcomes) {
                    match self
                        .apply_capture_result(
                            expected_cursor,
                            checkpoint,
                            Ok(outcome),
                            persisted_cursor_update,
                        )
                        .await?
                    {
                        DurableFrameDisposition::Persisted => {
                            progress.frames_accepted = progress.frames_accepted.saturating_add(1);
                            progress.frames_persisted = progress.frames_persisted.saturating_add(1);
                        }
                        DurableFrameDisposition::Refused => {
                            progress.frames_refused = progress.frames_refused.saturating_add(1);
                        }
                        DurableFrameDisposition::AlreadyDurable => {
                            progress.frames_skipped = progress.frames_skipped.saturating_add(1);
                        }
                    }
                }
                Ok(())
            }
            Err(outcome) => {
                if outcome.status == HostAdmissionStatus::Backpressured {
                    hotpath::gauge!("jsonl_admission_backpressure_writer").inc(1.0);
                }
                if let Some(recovery) = outcome.recovery {
                    match recovery {
                        HostAdmissionRecovery::BatchRequiresScalarFallback(_)
                        | HostAdmissionRecovery::DeterministicContentRefusal => {
                            return Err(CaptureWindowError::ScalarFallback(recovery));
                        }
                    }
                }
                if is_admission_cancellation(&outcome, &self.cancellation) {
                    Err(CaptureWindowError::Ingest(
                        TranscriptIngestError::Cancelled {
                            provider: self.provider,
                        },
                    ))
                } else {
                    Err(CaptureWindowError::Ingest(host_admission_error(
                        self.provider,
                        outcome,
                    )))
                }
            }
        }
    }
}

#[hotpath::measure(label = "sessions.observation.admit_jsonl", future = true)]
pub(in crate::runtime) async fn admit_jsonl_observations<State: Clone>(
    request: JsonlObservationAdmissionRequest<'_>,
    initialize: impl FnOnce(JsonlObservationScan) -> State,
    mut normalize: impl FnMut(
        &mut State,
        &[u8],
        tracedecay_domain::ObservationSourceRangeV1,
        u64,
        Option<PreparedObservationRecordV1>,
        JsonlFrameHints,
    ) -> TranscriptIngestResult<JsonlFrameAdmission>,
) -> TranscriptIngestResult<JsonlObservationAdmissionProgress> {
    let JsonlObservationAdmissionRequest {
        provider,
        path,
        admission,
        source,
        scope,
        retention_class,
        mut max_new_bytes,
        required_start_cursor,
        max_end_offset,
        persisted_cursor_update,
        cancellation,
        shared_frame_preparation,
    } = request;
    if cancellation.is_cancelled() {
        return Err(TranscriptIngestError::Cancelled { provider });
    }
    let mut expected_cursor =
        admission
            .get_source_cursor(&source, &scope)
            .await
            .map_err(|outcome| {
                if is_admission_cancellation(&outcome, &cancellation) {
                    TranscriptIngestError::Cancelled { provider }
                } else {
                    TranscriptIngestError::InvalidFrameState { provider }
                }
            })?;
    if cancellation.is_cancelled() {
        return Err(TranscriptIngestError::Cancelled { provider });
    }
    if required_start_cursor
        .as_ref()
        .is_some_and(|required| required != &expected_cursor)
    {
        return Ok(JsonlObservationAdmissionProgress {
            source_deferred: true,
            ..JsonlObservationAdmissionProgress::default()
        });
    }
    let previous = expected_cursor
        .as_ref()
        .map_or(StoredCursor::default(), |cursor| StoredCursor {
            position: cursor.position(),
            mtime: 0,
            file_id: cursor.generation().generation_id(),
        });
    if let Some(max_end_offset) = max_end_offset {
        let remaining = max_end_offset.saturating_sub(previous.position);
        if remaining == 0 {
            return Ok(JsonlObservationAdmissionProgress {
                source_deferred: true,
                ..JsonlObservationAdmissionProgress::default()
            });
        }
        max_new_bytes = Some(max_new_bytes.map_or(remaining, |limit| limit.min(remaining)));
    }
    let resume_state = expected_cursor.as_ref().and_then(|cursor| {
        Some(JsonlResumeState {
            generation: cursor.generation().generation_id(),
            file_identity: cursor.file_identity()?,
            fingerprint: cursor.resume_fingerprint()?,
        })
    });
    let had_expected_cursor = expected_cursor.is_some();
    let (raw, shared_page_hit) = shared_jsonl_page_with_cancellation(
        path,
        previous,
        max_new_bytes,
        resume_state,
        shared_frame_preparation,
        SharedJsonlCancellation {
            blocking: None,
            operation: Some(cancellation.clone()),
        },
        false,
    )
    .await?;
    let mut progress = JsonlObservationAdmissionProgress {
        bytes_consumed: raw.read_through.saturating_sub(raw.start_offset),
        source_deferred: raw.deferred.is_some(),
        io: if shared_page_hit {
            JsonlIoAccounting::default()
        } else {
            raw.io
        },
        ..JsonlObservationAdmissionProgress::default()
    };
    let total_frames = raw.frames.len();
    let retained_frame_bytes = raw.retained_bytes;
    tracing::debug!(
        event = "transcript_admission_batch",
        phase = "capturing",
        provider,
        transcript = %transcript_log_identity(path),
        total_frames,
        retained_frame_bytes,
        bytes_consumed = progress.bytes_consumed,
        source_deferred = progress.source_deferred,
        "transcript admission batch started"
    );
    if cancellation.is_cancelled() {
        return Err(TranscriptIngestError::Cancelled { provider });
    }
    let generation = ObservationSourceGenerationV1::new(raw.new_cursor.file_id)?;
    let mut state = initialize(JsonlObservationScan {
        resumed: had_expected_cursor && raw.start_offset > 0,
        // Derived from the scanned generation rather than this batch's first
        // offset, so a rewrite that spans several batches keeps namespacing
        // ids past the batch that started at the file head.
        replacement_rescan: raw.replacement_generation,
        start_offset: raw.start_offset,
        generation: raw.new_cursor.file_id,
        mtime: raw.new_cursor.mtime,
        frames: &raw.frames,
    });
    let active = ActiveAdmission {
        provider,
        admission,
        source,
        scope,
        generation,
        file_identity: raw.file_identity,
        cancellation,
    };
    let mut skipped = raw.skipped.iter().copied().peekable();
    let mut pending: Vec<DurableJsonlFrame> = Vec::new();
    let mut pending_bytes = 0_u64;
    let mut pending_start_state: Option<State> = None;

    #[hotpath::skip]
    async fn flush_pending<State: Clone>(
        active: &ActiveAdmission<'_>,
        window: PendingAdmissionWindow<'_, State>,
        policy: FlushPolicy<'_>,
        normalize: &mut impl FnMut(
            &mut State,
            &[u8],
            tracedecay_domain::ObservationSourceRangeV1,
            u64,
            Option<PreparedObservationRecordV1>,
            JsonlFrameHints,
        ) -> TranscriptIngestResult<JsonlFrameAdmission>,
    ) -> TranscriptIngestResult<()> {
        let PendingAdmissionWindow {
            expected_cursor,
            frames: pending,
            bytes: pending_bytes,
            start_state: pending_start_state,
            progress,
        } = window;
        let frames = std::mem::take(pending);
        *pending_bytes = 0;
        if frames.is_empty() {
            return Ok(());
        }
        let mut fallback_state =
            pending_start_state
                .take()
                .ok_or(TranscriptIngestError::InvalidFrameState {
                    provider: active.provider,
                })?;
        let backups = frames
            .iter()
            .map(|frame| {
                (
                    frame.checkpoint,
                    frame.range,
                    Arc::clone(&frame.bytes),
                    frame.fallback_prepared.clone(),
                    frame.fallback_hints,
                )
            })
            .collect::<Vec<_>>();
        match active
            .capture_window(
                expected_cursor,
                frames,
                policy.retention_class,
                policy.persisted_cursor_update,
                progress,
            )
            .await
        {
            Ok(()) => Ok(()),
            Err(CaptureWindowError::ScalarFallback(_recovery)) => {
                for (checkpoint, range, bytes, prepared, hints) in backups {
                    if active.cancellation.is_cancelled() {
                        return Err(TranscriptIngestError::Cancelled {
                            provider: active.provider,
                        });
                    }
                    let frame_start_state = fallback_state.clone();
                    match normalize(
                        &mut fallback_state,
                        bytes.as_ref(),
                        range,
                        checkpoint.offset,
                        prepared.clone(),
                        hints,
                    )? {
                        JsonlFrameAdmission::Durable {
                            parsed_record,
                            native_record_id,
                            identity_collision_retry,
                            skip_if_observation_exists,
                        } => {
                            let frame = DurableJsonlFrame {
                                checkpoint,
                                range,
                                parsed_record,
                                native_record_id,
                                skip_if_observation_exists,
                                bytes: Arc::clone(&bytes),
                                fallback_prepared: None,
                                fallback_hints: hints,
                                identity_collision_disposition: if identity_collision_retry {
                                    ObservationIdentityCollisionDispositionV1::RetryWithAlternateIdentity
                                } else {
                                    ObservationIdentityCollisionDispositionV1::SettleTerminal
                                },
                            };
                            let disposition = if frame.skip_if_observation_exists.is_some() {
                                active
                                    .capture(
                                        expected_cursor,
                                        frame,
                                        policy.retention_class,
                                        policy.persisted_cursor_update,
                                    )
                                    .await?
                            } else {
                                let first = active
                                    .capture_attempt(
                                        expected_cursor.clone(),
                                        frame,
                                        policy.retention_class,
                                    )
                                    .await?;
                                match first {
                                    Err(outcome)
                                        if identity_collision_retry
                                            && matches!(
                                                &outcome,
                                                HostAdmissionOutcome {
                                                    reason_code: Some(
                                                        "observation_identity_collision"
                                                    ),
                                                    retryable: false,
                                                    ..
                                                }
                                            ) =>
                                    {
                                        // The provider explicitly proved that the
                                        // primary identity had no native record
                                        // id. Re-normalize from the frame's input
                                        // state so provider carry is applied once,
                                        // then try exactly one positional identity.
                                        let mut retry_state = frame_start_state;
                                        let mut retry_hints = hints;
                                        retry_hints.identity_collision_retry = true;
                                        let retry = normalize(
                                            &mut retry_state,
                                            bytes.as_ref(),
                                            range,
                                            checkpoint.offset,
                                            prepared,
                                            retry_hints,
                                        )?;
                                        let JsonlFrameAdmission::Durable {
                                            parsed_record,
                                            native_record_id,
                                            ..
                                        } = retry
                                        else {
                                            return Err(TranscriptIngestError::InvalidFrameState {
                                                provider: active.provider,
                                            });
                                        };
                                        let disposition = active
                                            .capture(
                                                expected_cursor,
                                                DurableJsonlFrame {
                                                    checkpoint,
                                                    range,
                                                    parsed_record,
                                                    native_record_id,
                                                    skip_if_observation_exists: None,
                                                    bytes,
                                                    fallback_prepared: None,
                                                    fallback_hints: retry_hints,
                                                    identity_collision_disposition:
                                                        ObservationIdentityCollisionDispositionV1::SettleTerminal,
                                                },
                                                policy.retention_class,
                                                policy.persisted_cursor_update,
                                            )
                                            .await?;
                                        fallback_state = retry_state;
                                        disposition
                                    }
                                    result => {
                                        active
                                            .apply_capture_result(
                                                expected_cursor,
                                                checkpoint,
                                                result,
                                                policy.persisted_cursor_update,
                                            )
                                            .await?
                                    }
                                }
                            };
                            match disposition {
                                DurableFrameDisposition::Persisted => {
                                    progress.frames_accepted =
                                        progress.frames_accepted.saturating_add(1);
                                    progress.frames_persisted =
                                        progress.frames_persisted.saturating_add(1);
                                }
                                DurableFrameDisposition::Refused => {
                                    progress.frames_refused =
                                        progress.frames_refused.saturating_add(1);
                                }
                                DurableFrameDisposition::AlreadyDurable => {
                                    progress.frames_skipped =
                                        progress.frames_skipped.saturating_add(1);
                                }
                            }
                        }
                        JsonlFrameAdmission::NonDurable {
                            reason,
                            before_decode,
                        } => {
                            active
                                .advance_coverage(expected_cursor, checkpoint, reason, None)
                                .await?;
                            crate::runtime::pipeline_metrics::record_frame_skipped(reason);
                            progress.frames_skipped = progress.frames_skipped.saturating_add(1);
                            if before_decode {
                                progress.frames_rejected_before_decode =
                                    progress.frames_rejected_before_decode.saturating_add(1);
                            }
                        }
                        JsonlFrameAdmission::NeedsPreparation => {
                            return Err(TranscriptIngestError::InvalidFrameState {
                                provider: active.provider,
                            });
                        }
                    }
                }
                Ok(())
            }
            Err(CaptureWindowError::Ingest(error)) => Err(error),
        }
    }

    for (frame_index, frame) in raw.frames.iter().enumerate() {
        if active.cancellation.is_cancelled() {
            return Err(TranscriptIngestError::Cancelled { provider });
        }
        if frame_index % 256 == 0 {
            tracing::trace!(
                event = "transcript_admission_batch",
                phase = "capturing",
                provider,
                transcript = %transcript_log_identity(path),
                completed_frames = frame_index,
                total_frames,
                source_offset = frame.offset,
                "transcript admission batch progress"
            );
        }
        while skipped
            .peek()
            .is_some_and(|skipped| skipped.offset < frame.offset)
        {
            if active.cancellation.is_cancelled() {
                return Err(TranscriptIngestError::Cancelled { provider });
            }
            flush_pending(
                &active,
                PendingAdmissionWindow {
                    expected_cursor: &mut expected_cursor,
                    frames: &mut pending,
                    bytes: &mut pending_bytes,
                    start_state: &mut pending_start_state,
                    progress: &mut progress,
                },
                FlushPolicy {
                    retention_class: &retention_class,
                    persisted_cursor_update,
                },
                &mut normalize,
            )
            .await?;
            let skipped = skipped
                .next()
                .ok_or(TranscriptIngestError::InvalidFrameState { provider })?;
            active
                .advance_coverage(
                    &mut expected_cursor,
                    JsonlCheckpoint::new(
                        skipped.offset,
                        skipped.end_offset,
                        skipped.resume_fingerprint,
                    ),
                    skipped_reason(skipped.reason),
                    None,
                )
                .await?;
            crate::runtime::pipeline_metrics::record_frame_skipped(skipped_reason(skipped.reason));
            progress.frames_skipped = progress.frames_skipped.saturating_add(1);
        }
        if active.cancellation.is_cancelled() {
            return Err(TranscriptIngestError::Cancelled { provider });
        }

        let range =
            tracedecay_domain::ObservationSourceRangeV1::new(frame.offset, frame.end_offset)?;
        let checkpoint =
            JsonlCheckpoint::new(frame.offset, frame.end_offset, frame.resume_fingerprint);
        let frame_start_state = state.clone();
        let mut frame_state = frame_start_state.clone();
        let cached_prepared = frame.prepared.get().cloned();
        let mut admission = match cached_prepared {
            Some(Err(error)) => JsonlFrameAdmission::non_durable(preparation_failure_reason(error)),
            prepared => normalize(
                &mut frame_state,
                &frame.bytes,
                range,
                frame.offset,
                prepared.and_then(Result::ok),
                frame.hints,
            )?,
        };
        if matches!(admission, JsonlFrameAdmission::NeedsPreparation) {
            frame_state = frame_start_state.clone();
            prepare_shared_jsonl_window(&raw, frame_index, &active.cancellation, provider).await?;
            admission = match frame
                .prepared
                .get()
                .cloned()
                .ok_or(TranscriptIngestError::InvalidFrameState { provider })?
            {
                Ok(prepared) => normalize(
                    &mut frame_state,
                    &frame.bytes,
                    range,
                    frame.offset,
                    Some(prepared),
                    frame.hints,
                )?,
                Err(error) => JsonlFrameAdmission::non_durable(preparation_failure_reason(error)),
            };
        }
        if matches!(
            admission,
            JsonlFrameAdmission::Durable { .. }
                | JsonlFrameAdmission::NonDurable {
                    before_decode: false,
                    ..
                }
        ) {
            progress.frames_decoded = progress.frames_decoded.saturating_add(1);
        }
        state = frame_state;
        let (parsed_record, native_record_id, identity_collision_retry, skip_if_observation_exists) =
            match admission {
                JsonlFrameAdmission::Durable {
                    parsed_record,
                    native_record_id,
                    identity_collision_retry,
                    skip_if_observation_exists,
                } => (
                    parsed_record,
                    native_record_id,
                    identity_collision_retry,
                    skip_if_observation_exists,
                ),
                JsonlFrameAdmission::NonDurable {
                    reason,
                    before_decode,
                } => {
                    flush_pending(
                        &active,
                        PendingAdmissionWindow {
                            expected_cursor: &mut expected_cursor,
                            frames: &mut pending,
                            bytes: &mut pending_bytes,
                            start_state: &mut pending_start_state,
                            progress: &mut progress,
                        },
                        FlushPolicy {
                            retention_class: &retention_class,
                            persisted_cursor_update,
                        },
                        &mut normalize,
                    )
                    .await?;
                    active
                        .advance_coverage(&mut expected_cursor, checkpoint, reason, None)
                        .await?;
                    crate::runtime::pipeline_metrics::record_frame_skipped(reason);
                    progress.frames_skipped = progress.frames_skipped.saturating_add(1);
                    if before_decode {
                        progress.frames_rejected_before_decode =
                            progress.frames_rejected_before_decode.saturating_add(1);
                    }
                    continue;
                }
                JsonlFrameAdmission::NeedsPreparation => {
                    return Err(TranscriptIngestError::InvalidFrameState { provider });
                }
            };
        let frame_bytes = u64::try_from(frame.bytes.len())
            .map_err(|_| TranscriptIngestError::InvalidFrameState { provider })?;
        if !pending.is_empty()
            && pending_bytes.saturating_add(frame_bytes) > MAX_CAPTURE_WINDOW_BYTES
        {
            flush_pending(
                &active,
                PendingAdmissionWindow {
                    expected_cursor: &mut expected_cursor,
                    frames: &mut pending,
                    bytes: &mut pending_bytes,
                    start_state: &mut pending_start_state,
                    progress: &mut progress,
                },
                FlushPolicy {
                    retention_class: &retention_class,
                    persisted_cursor_update,
                },
                &mut normalize,
            )
            .await?;
        }
        pending.push(DurableJsonlFrame {
            checkpoint,
            range,
            parsed_record,
            native_record_id,
            skip_if_observation_exists,
            bytes: Arc::clone(&frame.bytes),
            fallback_prepared: frame
                .prepared
                .get()
                .and_then(|prepared| prepared.as_ref().ok())
                .cloned(),
            fallback_hints: frame.hints,
            identity_collision_disposition: if identity_collision_retry {
                ObservationIdentityCollisionDispositionV1::RetryWithAlternateIdentity
            } else {
                ObservationIdentityCollisionDispositionV1::SettleTerminal
            },
        });
        if pending.len() == 1 {
            pending_start_state = Some(frame_start_state);
        }
        pending_bytes = pending_bytes.saturating_add(frame_bytes);
        if pending.len() >= MAX_CAPTURE_WINDOW {
            flush_pending(
                &active,
                PendingAdmissionWindow {
                    expected_cursor: &mut expected_cursor,
                    frames: &mut pending,
                    bytes: &mut pending_bytes,
                    start_state: &mut pending_start_state,
                    progress: &mut progress,
                },
                FlushPolicy {
                    retention_class: &retention_class,
                    persisted_cursor_update,
                },
                &mut normalize,
            )
            .await?;
        }
    }

    flush_pending(
        &active,
        PendingAdmissionWindow {
            expected_cursor: &mut expected_cursor,
            frames: &mut pending,
            bytes: &mut pending_bytes,
            start_state: &mut pending_start_state,
            progress: &mut progress,
        },
        FlushPolicy {
            retention_class: &retention_class,
            persisted_cursor_update,
        },
        &mut normalize,
    )
    .await?;

    if !active.cancellation.is_cancelled() {
        for skipped in skipped {
            active
                .advance_coverage(
                    &mut expected_cursor,
                    JsonlCheckpoint::new(
                        skipped.offset,
                        skipped.end_offset,
                        skipped.resume_fingerprint,
                    ),
                    skipped_reason(skipped.reason),
                    None,
                )
                .await?;
            crate::runtime::pipeline_metrics::record_frame_skipped(skipped_reason(skipped.reason));
            progress.frames_skipped = progress.frames_skipped.saturating_add(1);
        }
    } else {
        return Err(TranscriptIngestError::Cancelled { provider });
    }
    tracing::debug!(
        event = "transcript_admission_batch",
        phase = "complete",
        provider,
        transcript = %transcript_log_identity(path),
        total_frames,
        retained_frame_bytes,
        bytes_consumed = progress.bytes_consumed,
        source_deferred = progress.source_deferred,
        "transcript admission batch finished"
    );
    crate::runtime::pipeline_metrics::record_admission_progress(
        progress.frames_decoded,
        progress.frames_accepted,
        progress.frames_skipped,
        progress.frames_rejected_before_decode,
        progress.frames_refused,
        progress.frames_persisted,
    );
    Ok(progress)
}

/// Non-retryable admission failures that are verdicts about the record's
/// content. Only these may be covered past: they re-fail identically on every
/// sweep, so a durable typed refusal reason is what lets the stream converge.
/// Identity collisions retain their exact reason; other deterministic content
/// refusals use `AdmissionRefused`. Every other failure — store commit/read-back failures,
/// unbound authorities, retryable races — says nothing about the record and
/// must surface as a typed block instead of writing coverage over a commit
/// that never landed (or one that already landed and advanced the cursor).
fn is_deterministic_content_refusal(outcome: &HostAdmissionOutcome) -> bool {
    matches!(
        outcome.recovery,
        Some(HostAdmissionRecovery::DeterministicContentRefusal)
    ) || (!outcome.retryable
        && matches!(
            outcome.reason_code,
            Some("invalid_observation_contract" | "privacy_boundary_failed")
        ))
}

/// Log identity for a transcript file. Transcript paths sit under the
/// operator's home directory and name real sessions, so ingest logs carry the
/// basename only rather than persisting an absolute path into the daemon log.
fn transcript_log_identity(path: &Path) -> Cow<'_, str> {
    path.file_name()
        .map_or(Cow::Borrowed("<unnamed>"), |name| name.to_string_lossy())
}

fn skipped_reason(reason: RawJsonlSkippedReason) -> ObservationCoverageReason {
    match reason {
        RawJsonlSkippedReason::Whitespace => ObservationCoverageReason::BlankFrame,
        RawJsonlSkippedReason::Oversized => ObservationCoverageReason::OversizedFrame,
    }
}

pub(in crate::runtime) fn namespace_replacement_message_ids(
    messages: &mut [SessionMessageRecord],
    generation: u64,
) {
    for message in messages {
        message.message_id = format!("{}:generation:{generation}", message.message_id);
    }
}

pub(in crate::runtime) fn preflight_and_parse_new(
    provider: &'static str,
    path: &Path,
    prev: StoredCursor,
    max_new_bytes: Option<u64>,
    parse_new: impl FnOnce() -> Option<ParsedTranscript>,
) -> TranscriptIngestResult<Option<ParsedTranscript>> {
    preflight_strict_jsonl(provider, path, prev, max_new_bytes)?;
    Ok(parse_new())
}

#[cfg(test)]
mod tests;
