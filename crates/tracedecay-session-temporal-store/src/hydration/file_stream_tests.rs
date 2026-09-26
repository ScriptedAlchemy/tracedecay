//! File-backed hydration through the production backend: a payload near the
//! per-payload limit must be proven before any chunk crosses the sink and must
//! stream with chunk-sized transient memory, and a source that drifts under
//! the reader or is cancelled mid-stream must settle typed. The descriptor is
//! the one resolution would hand the reader; everything from the open onward
//! is the production read. Replacement between proof and emission is owned
//! and tested by `tracedecay_lcm::payload` itself.
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
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};
use std::thread;
use std::time::Duration;

use sha2::{Digest, Sha256};
use tempfile::tempdir;
use tracedecay_global_db::tests::harness::{HostAdmissionScope, HostAdmissionTestRuntimeV1};
use tracedecay_runtime_core::db::DatabaseEngineReadSnapshot;
use tracedecay_temporal_query::execution::ExecutionControl;
use tracedecay_temporal_query::execution::{ReadBudgetAccounting, TemporalPortError};

use super::{HydrationError, PayloadDescriptor, PayloadSource, SessionTemporalHydrationAdapter};

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

impl RegisteredRead {
    fn adapter(&self) -> SessionTemporalHydrationAdapter<'_> {
        SessionTemporalHydrationAdapter::for_registered_snapshot(&self.read, &self.storage_root)
    }
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
    let adapter = read.adapter();
    let control = ExecutionControl::default();
    let descriptor = external_descriptor(&content);

    let mut sink = HashingSink::default();
    let (result, peak_bytes) = measure_peak_live_bytes(adapter.read_descriptor(
        &control,
        &descriptor,
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
async fn payload_file_rewritten_or_removed_under_the_reader_is_refused() {
    for remove_instead_of_replace in [false, true] {
        let dir = tempdir().expect("temporary directory");
        let runtime = HostAdmissionTestRuntimeV1::profile(dir.path())
            .await
            .expect("registered profile runtime");
        let read = registered_read(&runtime).await;
        let content = payload_bytes(3 * CHUNK_BYTES);
        let payload_path = seed_payload_file(&read.storage_root, &content);
        let adapter = read.adapter();
        let control = ExecutionControl::default();
        let descriptor = external_descriptor(&content);

        let mut intact = HashingSink::default();
        adapter
            .read_descriptor(
                &control,
                &descriptor,
                MAX_PAYLOAD_BYTES,
                CHUNK_BYTES,
                &mut |chunk| intact.write(chunk),
            )
            .await
            .expect("intact payload hydrates");
        assert_eq!(intact.bytes, content.len());

        if remove_instead_of_replace {
            fs::remove_file(&payload_path).expect("remove proven payload");
        } else {
            // Same length and valid UTF-8, so only the content proof tells it apart.
            let replacement = content.iter().rev().copied().collect::<Vec<_>>();
            fs::write(&payload_path, replacement).expect("rewrite payload under the reader");
        }
        let mut sink = HashingSink::default();
        let outcome = adapter
            .read_descriptor(
                &control,
                &descriptor,
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
            "no chunk may cross the sink from a source that drifted from its descriptor \
             (remove={remove_instead_of_replace})"
        );
    }
}

#[tokio::test]
async fn cancellation_after_the_first_chunk_settles_typed_and_releases_the_source() {
    let dir = tempdir().expect("temporary directory");
    let runtime = HostAdmissionTestRuntimeV1::profile(dir.path())
        .await
        .expect("registered profile runtime");
    let read = registered_read(&runtime).await;
    let content = payload_bytes(4 * CHUNK_BYTES);
    let payload_path = seed_payload_file(&read.storage_root, &content);
    let adapter = read.adapter();
    let descriptor = external_descriptor(&content);

    // Cancelled by the sink after the first chunk: the next emission
    // checkpoint stops the stream and reports the cancellation, not a payload
    // fault, and exactly one chunk crossed.
    let control = ExecutionControl::default();
    let cancel = control.clone();
    let mut chunks = 0_usize;
    assert_eq!(
        adapter
            .read_descriptor(
                &control,
                &descriptor,
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

    // The interrupted run retained nothing: the file is still exactly the
    // seeded payload, and a fresh hydration proves and emits all of it.
    assert_eq!(fs::read(&payload_path).expect("payload file"), content);
    let mut sink = HashingSink::default();
    adapter
        .read_descriptor(
            &ExecutionControl::default(),
            &descriptor,
            MAX_PAYLOAD_BYTES,
            CHUNK_BYTES,
            &mut |chunk| sink.write(chunk),
        )
        .await
        .expect("fresh hydration after interrupted run");
    assert_eq!(sink.bytes, content.len());
    assert_eq!(hex(&sink.hasher.finalize()), hex(&Sha256::digest(&content)));
}

/// A work budget that runs out while the proof is still hashing windows is a
/// typed budget interruption, not a payload fault, and emits nothing. Four
/// checkpoints precede the first proof window (one in the adapter, one in the
/// backend, two in the LCM open) and the proof then checkpoints once per
/// 64 KiB window of the 1 MiB payload, so a budget of eight is exhausted at
/// the fifth window, any budget in 5..=20 trips inside the proof.
#[tokio::test]
async fn work_budget_exhausted_during_file_proof_is_typed_and_emits_nothing() {
    let dir = tempdir().expect("temporary directory");
    let runtime = HostAdmissionTestRuntimeV1::profile(dir.path())
        .await
        .expect("registered profile runtime");
    let read = registered_read(&runtime).await;
    let content = payload_bytes(PAYLOAD_BYTES);
    seed_payload_file(&read.storage_root, &content);
    let adapter = read.adapter();
    let control = ExecutionControl::default().with_work_limit(8);

    let mut sink = HashingSink::default();
    assert_eq!(
        adapter
            .read_descriptor(
                &control,
                &external_descriptor(&content),
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
