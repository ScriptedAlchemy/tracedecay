//! File-backed hydration through the production backend: a payload near the
//! per-payload limit must be proven before any chunk crosses the sink and must
//! stream with chunk-sized transient memory, and a source that is replaced,
//! removed, or cancelled between proof and emission must settle typed.
//!
//! Peak memory is measured with a thread-local live-byte allocator. Tests run
//! on parallel threads, so the counter is per thread; the measured future is
//! driven by a thread-parking executor on the test thread so every allocation
//! it makes is attributed here.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::fs;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Wake, Waker};
use std::thread;
use std::time::Duration;

use sha2::{Digest, Sha256};
use tempfile::tempdir;
use tracedecay_domain::{RetrievalAnchorId, RetrievalGrainV1, SessionId, TemporalModeV1};
use tracedecay_global_db::tests::harness::{HostAdmissionScope, HostAdmissionTestRuntimeV1};
use tracedecay_runtime_core::db::DatabaseEngineReadSnapshot;
use tracedecay_temporal_query::ports::{
    BindingDigest, ExecutionControl, ExecutionLimits, KernelVersions, ReadBudgetAccounting,
    TemporalExecutionSnapshot, TemporalPortError, TemporalSnapshotRequest, TemporalWatermarks,
};
use tracedecay_temporal_query::resolution::ValidatedAuthorization;

use super::{
    BackendFuture, BoundedPayload, HydrationAuthorization, HydrationError, HydrationResolution,
    PayloadDescriptor, PayloadSource, SessionTemporalHydrationAdapter,
    SessionTemporalHydrationBackend, TemporalHydrationBackend,
};

/// Default per-payload limit; the fixture payload sits just under it.
const MAX_PAYLOAD_BYTES: usize = 1024 * 1024;
const CHUNK_BYTES: usize = 64 * 1024;
const PAYLOAD_BYTES: usize = MAX_PAYLOAD_BYTES - 1;
const PAYLOAD_REF: &str = "payload_stream_fixture.payload";

struct LiveBytesRecorder;

thread_local! {
    static LIVE_BYTES: Cell<usize> = const { Cell::new(0) };
    static PEAK_LIVE_BYTES: Cell<usize> = const { Cell::new(0) };
}

fn record_alloc(size: usize) {
    LIVE_BYTES.with(|live| {
        let next = live.get().saturating_add(size);
        live.set(next);
        PEAK_LIVE_BYTES.with(|peak| peak.set(peak.get().max(next)));
    });
}

fn record_dealloc(size: usize) {
    LIVE_BYTES.with(|live| live.set(live.get().saturating_sub(size)));
}

unsafe impl GlobalAlloc for LiveBytesRecorder {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record_alloc(layout.size());
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        record_dealloc(layout.size());
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record_alloc(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        record_dealloc(layout.size());
        record_alloc(new_size);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static TEST_ALLOCATOR: LiveBytesRecorder = LiveBytesRecorder;

struct ThreadWake(thread::Thread);

impl Wake for ThreadWake {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.unpark();
    }
}

/// Drives `future` on the current thread so its allocations land on this
/// thread's counters, and returns its output with the peak growth of live
/// bytes over the run.
fn measure_peak_live_bytes<F: Future>(future: F) -> (F::Output, usize) {
    let mut future = Box::pin(future);
    let waker = Waker::from(Arc::new(ThreadWake(thread::current())));
    let mut context = Context::from_waker(&waker);
    let start = LIVE_BYTES.with(Cell::get);
    PEAK_LIVE_BYTES.with(|peak| peak.set(start));
    let output = loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(output) => break output,
            Poll::Pending => thread::park_timeout(Duration::from_millis(10)),
        }
    };
    let peak = PEAK_LIVE_BYTES.with(Cell::get);
    (output, peak.saturating_sub(start))
}

fn hex(digest: &[u8]) -> String {
    use std::fmt::Write as _;
    digest.iter().fold(String::new(), |mut output, byte| {
        let _ = write!(output, "{byte:02x}");
        output
    })
}

fn payload_bytes(len: usize) -> Vec<u8> {
    (0..len).map(|index| b'a' + (index % 26) as u8).collect()
}

struct RegisteredRead {
    read: DatabaseEngineReadSnapshot,
    storage_root: PathBuf,
}

async fn registered_read(runtime: &HostAdmissionTestRuntimeV1) -> RegisteredRead {
    let database = runtime
        .registered_database(HostAdmissionScope::Profile)
        .expect("registered profile database");
    RegisteredRead {
        read: database.read_snapshot().await.expect("read snapshot"),
        storage_root: database
            .db_path()
            .parent()
            .expect("registered profile storage root")
            .to_path_buf(),
    }
}

/// Writes the fixture payload into the private LCM payload directory exactly
/// as the externalizer lays it out, and returns its path.
fn seed_payload_file(storage_root: &Path, content: &[u8]) -> PathBuf {
    let payload_dir = storage_root.join("lcm-payloads");
    fs::create_dir(&payload_dir).expect("payload directory");
    let payload_path = payload_dir.join(PAYLOAD_REF);
    fs::write(&payload_path, content).expect("external payload");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(&payload_dir, fs::Permissions::from_mode(0o700))
            .expect("private payload directory");
        fs::set_permissions(&payload_path, fs::Permissions::from_mode(0o600))
            .expect("private payload file");
    }
    payload_path
}

fn external_descriptor(content: &[u8]) -> PayloadDescriptor {
    PayloadDescriptor {
        source: PayloadSource::External {
            provider: "provider-1".to_string(),
            session_id: "session-1".to_string(),
            payload_ref: PAYLOAD_REF.to_string(),
            char_count: content.len(),
        },
        byte_count: content.len(),
        content_hash: hex(&Sha256::digest(content)),
    }
}

type Hook = Mutex<Option<Box<dyn FnOnce() + Send>>>;

fn run_hook(hook: &Hook) {
    if let Some(hook) = hook.lock().expect("hook").take() {
        hook();
    }
}

/// The production registered backend with only anchor resolution replaced by
/// a fixed external descriptor, so the proof and emission under test are the
/// real file-backed journey. `before_open` runs once as the adapter asks for
/// the payload; `after_open` runs once between the proof and the adapter's
/// emission.
struct ExternalPayloadBackend<'snapshot> {
    inner: SessionTemporalHydrationBackend<'snapshot>,
    descriptor: PayloadDescriptor,
    before_open: Hook,
    after_open: Hook,
}

impl<'snapshot> ExternalPayloadBackend<'snapshot> {
    fn new(read: &'snapshot RegisteredRead, content: &[u8]) -> Self {
        Self {
            inner: SessionTemporalHydrationBackend::new_registered(&read.read, &read.storage_root),
            descriptor: external_descriptor(content),
            before_open: Mutex::new(None),
            after_open: Mutex::new(None),
        }
    }

    fn with_before_open(self, hook: impl FnOnce() + Send + 'static) -> Self {
        *self.before_open.lock().expect("hook") = Some(Box::new(hook));
        self
    }

    fn with_after_open(self, hook: impl FnOnce() + Send + 'static) -> Self {
        *self.after_open.lock().expect("hook") = Some(Box::new(hook));
        self
    }
}

impl TemporalHydrationBackend for ExternalPayloadBackend<'_> {
    fn snapshot_is_stable(&self) -> bool {
        true
    }

    fn resolve_current<'a>(
        &'a self,
        _snapshot: &'a TemporalExecutionSnapshot,
        _anchor_id: &'a RetrievalAnchorId,
    ) -> BackendFuture<'a, HydrationResolution> {
        Box::pin(async move { Ok(HydrationResolution::Available(self.descriptor.clone())) })
    }

    fn open_bounded<'a>(
        &'a self,
        descriptor: &'a PayloadDescriptor,
        max_bytes: usize,
        control: &'a ExecutionControl,
    ) -> BackendFuture<'a, BoundedPayload> {
        Box::pin(async move {
            run_hook(&self.before_open);
            let payload = self
                .inner
                .open_bounded(descriptor, max_bytes, control)
                .await?;
            run_hook(&self.after_open);
            Ok(payload)
        })
    }
}

fn digest(byte: char) -> String {
    format!("sha256:{}", byte.to_string().repeat(64))
}

fn snapshot(control: ExecutionControl) -> TemporalExecutionSnapshot {
    let limits = ExecutionLimits::default();
    assert_eq!(limits.hydration_payload_bytes, MAX_PAYLOAD_BYTES);
    assert_eq!(limits.hydration_chunk_bytes, CHUNK_BYTES);
    TemporalExecutionSnapshot::new_authorized(
        TemporalSnapshotRequest::new(
            SessionId::new("session-1").expect("session"),
            digest('0'),
            digest('1'),
            digest('2'),
            TemporalModeV1::Current,
            RetrievalGrainV1::LogicalMessage,
        )
        .expect("request")
        .with_limits(limits)
        .with_execution_control(control),
        TemporalWatermarks {
            generation: 1,
            source: 2,
            projection: 3,
            index: 4,
            summary: 5,
        },
        KernelVersions {
            schema: 1,
            ranking: 1,
            configuration_digest: BindingDigest::new("configuration", digest('3')).expect("digest"),
        },
        None,
        ValidatedAuthorization::Authorized,
    )
    .expect("snapshot")
}

fn anchor() -> RetrievalAnchorId {
    RetrievalAnchorId::new("anchor-file").expect("anchor")
}

/// Sink that authenticates what it receives without buffering it, so the
/// measured region holds no consumer-side copy of the payload.
#[derive(Default)]
struct HashingSink {
    hasher: Sha256,
    bytes: usize,
    chunks: Vec<usize>,
}

impl HashingSink {
    fn write(&mut self, chunk: &[u8]) -> Result<(), HydrationError> {
        self.hasher.update(chunk);
        self.bytes += chunk.len();
        self.chunks.push(chunk.len());
        Ok(())
    }
}

#[tokio::test]
async fn near_limit_file_payload_streams_with_chunk_sized_peak_memory() {
    let dir = tempdir().expect("temporary directory");
    let runtime = HostAdmissionTestRuntimeV1::profile(dir.path())
        .await
        .expect("registered profile runtime");
    let read = registered_read(&runtime).await;
    let content = payload_bytes(PAYLOAD_BYTES);
    seed_payload_file(&read.storage_root, &content);
    let expected_hash = hex(&Sha256::digest(&content));
    let adapter =
        SessionTemporalHydrationAdapter::new(ExternalPayloadBackend::new(&read, &content));
    let snapshot = snapshot(ExecutionControl::default());
    assert_eq!(
        adapter.authorize(&snapshot, &anchor()).await,
        Ok(HydrationAuthorization::Authorized)
    );

    let mut sink = HashingSink::default();
    let (result, peak_bytes) = measure_peak_live_bytes(adapter.read_after_recheck(
        &snapshot,
        &anchor(),
        MAX_PAYLOAD_BYTES,
        CHUNK_BYTES,
        &mut |chunk| sink.write(chunk),
    ));
    result.expect("near-limit file payload hydrates");

    assert_eq!(sink.bytes, PAYLOAD_BYTES);
    assert_eq!(hex(&sink.hasher.finalize()), expected_hash);
    assert!(sink.chunks.iter().all(|chunk| *chunk <= CHUNK_BYTES));
    assert_eq!(sink.chunks.len(), PAYLOAD_BYTES.div_ceil(CHUNK_BYTES));
    // Whole-body buffering costs at least one payload (the pre-streaming read
    // held two: the verified read plus its bounded copy). Streaming holds one
    // proof window and one emission window, never both, plus hashing state.
    assert!(
        peak_bytes < PAYLOAD_BYTES / 4,
        "peak transient allocation {peak_bytes} B for a {PAYLOAD_BYTES} B payload is not \
         chunk-bounded (chunk {CHUNK_BYTES} B)"
    );
    eprintln!(
        "file hydration peak transient allocation: {peak_bytes} B for a {PAYLOAD_BYTES} B \
         payload in {CHUNK_BYTES} B chunks"
    );
}

#[tokio::test]
async fn payload_file_replaced_or_removed_between_proof_and_emission_is_refused() {
    for remove_instead_of_replace in [false, true] {
        let dir = tempdir().expect("temporary directory");
        let runtime = HostAdmissionTestRuntimeV1::profile(dir.path())
            .await
            .expect("registered profile runtime");
        let read = registered_read(&runtime).await;
        let content = payload_bytes(3 * CHUNK_BYTES);
        let payload_path = seed_payload_file(&read.storage_root, &content);
        let swap_path = payload_path.clone();
        let backend = ExternalPayloadBackend::new(&read, &content).with_after_open(move || {
            if remove_instead_of_replace {
                fs::remove_file(&swap_path).expect("remove proven payload");
            } else {
                let displaced = swap_path.with_extension("displaced");
                fs::rename(&swap_path, &displaced).expect("displace proven payload");
                // Same length and valid UTF-8, so only identity tells it apart.
                let replacement = payload_bytes(3 * CHUNK_BYTES)
                    .iter()
                    .rev()
                    .copied()
                    .collect::<Vec<_>>();
                fs::write(&swap_path, replacement).expect("write replacement payload");
            }
        });
        let adapter = SessionTemporalHydrationAdapter::new(backend);
        let snapshot = snapshot(ExecutionControl::default());

        let mut sink = HashingSink::default();
        let outcome = adapter
            .read_after_recheck(
                &snapshot,
                &anchor(),
                MAX_PAYLOAD_BYTES,
                CHUNK_BYTES,
                &mut |chunk| sink.write(chunk),
            )
            .await;
        assert_eq!(
            outcome,
            Err(HydrationError::Unavailable),
            "remove={remove_instead_of_replace}"
        );
        assert_eq!(
            sink.bytes, 0,
            "no chunk may cross the sink from a source replaced after its proof \
             (remove={remove_instead_of_replace})"
        );
    }
}

#[tokio::test]
async fn cancellation_during_file_proof_and_emission_settles_typed_and_releases_the_source() {
    let dir = tempdir().expect("temporary directory");
    let runtime = HostAdmissionTestRuntimeV1::profile(dir.path())
        .await
        .expect("registered profile runtime");
    let read = registered_read(&runtime).await;
    let content = payload_bytes(4 * CHUNK_BYTES);
    let payload_path = seed_payload_file(&read.storage_root, &content);

    // Cancelled after the adapter's pre-open checkpoint, as the backend is
    // asked for the payload: the proof's own checkpoint surfaces the
    // cancellation, no stream is produced, and no chunk is emitted.
    let control = ExecutionControl::default();
    let cancel = control.clone();
    let adapter = SessionTemporalHydrationAdapter::new(
        ExternalPayloadBackend::new(&read, &content).with_before_open(move || cancel.cancel()),
    );
    let snapshot_during_proof = snapshot(control);
    let mut sink = HashingSink::default();
    assert_eq!(
        adapter
            .read_after_recheck(
                &snapshot_during_proof,
                &anchor(),
                MAX_PAYLOAD_BYTES,
                CHUNK_BYTES,
                &mut |chunk| sink.write(chunk),
            )
            .await,
        Err(HydrationError::Interrupted(TemporalPortError::Cancelled))
    );
    assert_eq!(sink.bytes, 0);

    // Cancelled by the sink after the first chunk: the next emission
    // checkpoint stops the stream and reports the cancellation, not a payload
    // fault, and exactly one chunk crossed.
    let control = ExecutionControl::default();
    let cancel = control.clone();
    let adapter =
        SessionTemporalHydrationAdapter::new(ExternalPayloadBackend::new(&read, &content));
    let snapshot_during_emission = snapshot(control);
    let mut chunks = 0_usize;
    assert_eq!(
        adapter
            .read_after_recheck(
                &snapshot_during_emission,
                &anchor(),
                MAX_PAYLOAD_BYTES,
                CHUNK_BYTES,
                &mut |chunk| {
                    chunks += 1;
                    assert_eq!(chunk.len(), CHUNK_BYTES);
                    cancel.cancel();
                    Ok(())
                },
            )
            .await,
        Err(HydrationError::Interrupted(TemporalPortError::Cancelled))
    );
    assert_eq!(chunks, 1);

    // Neither interrupted run retained the source: the file is still exactly
    // the seeded payload, and a fresh hydration proves and emits all of it.
    assert_eq!(fs::read(&payload_path).expect("payload file"), content);
    let adapter =
        SessionTemporalHydrationAdapter::new(ExternalPayloadBackend::new(&read, &content));
    let snapshot = snapshot(ExecutionControl::default());
    let mut sink = HashingSink::default();
    adapter
        .read_after_recheck(
            &snapshot,
            &anchor(),
            MAX_PAYLOAD_BYTES,
            CHUNK_BYTES,
            &mut |chunk| sink.write(chunk),
        )
        .await
        .expect("fresh hydration after interrupted runs");
    assert_eq!(sink.bytes, content.len());
    assert_eq!(hex(&sink.hasher.finalize()), hex(&Sha256::digest(&content)));
}

/// A work budget that runs out while the proof is still hashing windows is a
/// typed budget interruption, not a payload fault, and emits nothing. Five
/// checkpoints precede the first proof window (two in the adapter, one in the
/// backend, two in the LCM open) and the proof then checkpoints once per
/// 64 KiB window of the 1 MiB payload, so a budget of eight is exhausted at
/// the fourth window — any budget in 6..=20 trips inside the proof.
#[tokio::test]
async fn work_budget_exhausted_during_file_proof_is_typed_and_emits_nothing() {
    let dir = tempdir().expect("temporary directory");
    let runtime = HostAdmissionTestRuntimeV1::profile(dir.path())
        .await
        .expect("registered profile runtime");
    let read = registered_read(&runtime).await;
    let content = payload_bytes(PAYLOAD_BYTES);
    seed_payload_file(&read.storage_root, &content);
    let adapter =
        SessionTemporalHydrationAdapter::new(ExternalPayloadBackend::new(&read, &content));
    let snapshot = snapshot(ExecutionControl::default().with_work_limit(8));

    let mut sink = HashingSink::default();
    assert_eq!(
        adapter
            .read_after_recheck(
                &snapshot,
                &anchor(),
                MAX_PAYLOAD_BYTES,
                CHUNK_BYTES,
                &mut |chunk| { sink.write(chunk) }
            )
            .await,
        Err(HydrationError::Interrupted(
            TemporalPortError::BudgetExceeded {
                resource: "work units",
                accounting: Some(ReadBudgetAccounting::consumed_with_more(8, 8)),
            }
        ))
    );
    assert_eq!(sink.bytes, 0);
}
