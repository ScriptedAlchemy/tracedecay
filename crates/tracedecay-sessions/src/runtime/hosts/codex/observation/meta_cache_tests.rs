//! Cancellation and settlement contract for the shared Codex metadata cache.
//!
//! A metadata fill is owned by its own task, not by whichever request first
//! waited for it: cancelling a requester must never orphan an `in_flight`
//! claim, and every waiter on the same key must either receive the settled
//! result or elect a replacement fill.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tempfile::TempDir;
use tracedecay_domain::ObservationScopeV1;

use super::super::meta::{
    SessionMetaParseGate, install_session_meta_parse_gate_for_test,
    session_meta_read_count_for_test,
};
use super::*;
use crate::admission::HostAdmission;
use crate::admission::test_support::MemoryHostAdmission;
use crate::runtime::observation::jsonl_observation_admission::install_test_shared_jsonl_preparation_authority;

const SESSION_ID: &str = "meta-cache-session";

/// Every wait in this module is bounded so an orphaned claim fails the test
/// instead of hanging the suite.
const SETTLE_WITHIN: Duration = Duration::from_secs(20);

fn write_rollout(dir: &Path, name: &str, with_meta: bool) -> PathBuf {
    let cwd = dir.join("workspace");
    std::fs::create_dir_all(&cwd).unwrap();
    let path = dir.join(name);
    let mut lines = Vec::new();
    if with_meta {
        lines.push(json!({
            "timestamp": "2026-09-07T12:00:00.000Z",
            "type": "session_meta",
            "payload": {"id": SESSION_ID, "cwd": cwd}
        }));
    }
    lines.push(json!({
        "timestamp": "2026-09-07T12:00:01.000Z",
        "type": "event_msg",
        "payload": {
            "type": "item_completed",
            "item": {
                "type": "UserMessage",
                "id": "meta-cache-item",
                "content": [{"type": "text", "text": "Admit after the fill settles."}]
            }
        }
    }));
    std::fs::write(
        &path,
        lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n",
    )
    .unwrap();
    path
}

/// Releases the parse gate when dropped, so a failed assertion before the
/// explicit release reports instead of leaving a parked blocking parse that the
/// runtime's shutdown would wait on forever.
struct GateGuard(Arc<SessionMetaParseGate>);

impl std::ops::Deref for GateGuard {
    type Target = Arc<SessionMetaParseGate>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Drop for GateGuard {
    fn drop(&mut self) {
        self.0.release();
    }
}

fn fixture(name: &str, with_meta: bool) -> (TempDir, PathBuf, GateGuard) {
    install_test_shared_jsonl_preparation_authority();
    let tmp = TempDir::new().unwrap();
    let path = write_rollout(tmp.path(), name, with_meta);
    let gate = GateGuard(install_session_meta_parse_gate_for_test(&path));
    (tmp, path, gate)
}

fn cache_key(path: &Path) -> CodexMetaCacheKey {
    let canonical = std::fs::canonicalize(path).unwrap();
    CodexMetaCacheKey {
        identity: shared_jsonl_file_identity(&canonical).unwrap(),
        path: canonical,
    }
}

fn spawn_lookup(
    path: &Path,
    cancellation: ObservationCancellation,
) -> tokio::task::JoinHandle<TranscriptIngestResult<Arc<CodexMetaWithProvenance>>> {
    let path = path.to_path_buf();
    tokio::spawn(async move { shared_session_meta_with_provenance(&path, &cancellation).await })
}

/// Blocks until `count` parses of the fixture are parked at its gate.
async fn wait_parked(gate: &Arc<SessionMetaParseGate>, count: usize) {
    let gate = Arc::clone(gate);
    tokio::time::timeout(
        SETTLE_WITHIN,
        tokio::task::spawn_blocking(move || gate.wait_parked(count)),
    )
    .await
    .expect("a parse must reach the gate")
    .unwrap();
}

/// Waits until `count` lookups have registered as waiters behind the key's
/// live fill, so the test observes the in-flight path rather than a cache hit.
async fn wait_in_flight_waits(key: &CodexMetaCacheKey, count: usize) {
    tokio::time::timeout(SETTLE_WITHIN, async {
        while in_flight_waits_for_test(key) < count {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("a lookup must park behind the live fill");
}

#[tokio::test]
async fn cancelled_first_waiter_leaves_the_fill_owner_to_settle() {
    let (_tmp, path, gate) = fixture("cancelled-first-waiter.jsonl", true);
    let key = cache_key(&path);
    let reads_before = session_meta_read_count_for_test(&path);
    let waits_before = in_flight_waits_for_test(&key);

    let first = spawn_lookup(&path, ObservationCancellation::default());
    wait_parked(&gate, 1).await;
    assert!(lock_codex_meta_cache().in_flight.contains_key(&key));

    // The first waiter is torn down while the parse it elected is parked.
    first.abort();
    assert!(
        first
            .await
            .err()
            .expect("the aborted lookup never settles")
            .is_cancelled()
    );
    assert!(
        lock_codex_meta_cache().in_flight.contains_key(&key),
        "the fill owner outlives the request that elected it"
    );

    // A second lookup for the unchanged key parks behind the live fill rather
    // than looping behind an orphan claim or electing a second parse.
    let second = spawn_lookup(&path, ObservationCancellation::default());
    wait_in_flight_waits(&key, waits_before + 1).await;
    gate.release();
    let meta = tokio::time::timeout(SETTLE_WITHIN, second)
        .await
        .expect("the second lookup settles once the fill publishes")
        .unwrap()
        .unwrap();
    assert_eq!(meta.meta.session_id, SESSION_ID);
    assert_eq!(
        session_meta_read_count_for_test(&path) - reads_before,
        1,
        "the abandoned request's parse served the later waiter"
    );

    {
        let cache = lock_codex_meta_cache();
        assert!(!cache.in_flight.contains_key(&key));
        let entry = cache
            .entries
            .iter()
            .find(|entry| entry.key == key)
            .expect("the fill published its result");
        assert!(Arc::ptr_eq(&entry.meta, &meta));
        // The memory charge travelled with the blocking worker through the
        // abort and was shrunk to the measured size at settlement, not
        // released when the requester's scope was dropped.
        assert_eq!(
            entry
                ._memory
                .as_ref()
                .map(ProcessSharedMemoryReservationV1::reserved_bytes),
            Some(meta.retained_bytes())
        );
    }

    // Later callers hit the published entry, and the real admission
    // continuation runs through it with an unchanged source cursor.
    let third = shared_session_meta_with_provenance(&path, &ObservationCancellation::default())
        .await
        .unwrap();
    assert!(Arc::ptr_eq(&third, &meta));
    let admission = MemoryHostAdmission::default();
    let progress = try_admit_codex_jsonl_observations_for_profile_with_admission(
        &path,
        None,
        &[],
        &admission,
        None,
    )
    .await
    .unwrap();
    // The `session_meta` line and the user message are both durable frames.
    assert_eq!(progress.frames_persisted, 2);
    assert_eq!(
        progress.bytes_consumed,
        std::fs::metadata(&path).unwrap().len()
    );
    let cursor = admission
        .get_source_cursor(
            &codex_observation_source_v2(SESSION_ID).unwrap(),
            &ObservationScopeV1::Profile,
        )
        .await
        .unwrap()
        .expect("admission advanced the canonical cursor");
    assert_eq!(cursor.position(), progress.bytes_consumed);
    assert_eq!(
        session_meta_read_count_for_test(&path) - reads_before,
        1,
        "admission reused the cached metadata"
    );
}

#[tokio::test]
async fn waiter_cancellation_is_typed_and_leaves_the_fill_intact() {
    let (_tmp, path, gate) = fixture("waiter-cancellation.jsonl", true);
    let key = cache_key(&path);
    let waits_before = in_flight_waits_for_test(&key);

    let owner = spawn_lookup(&path, ObservationCancellation::default());
    wait_parked(&gate, 1).await;
    let cancellation = ObservationCancellation::default();
    let waiter = spawn_lookup(&path, cancellation.clone());
    wait_in_flight_waits(&key, waits_before + 1).await;

    // The waiter answers its own cancellation while the parse is still parked.
    cancellation.cancel();
    let error = tokio::time::timeout(SETTLE_WITHIN, waiter)
        .await
        .expect("a cancelled waiter must not wait out the parse")
        .unwrap()
        .err()
        .expect("the cancelled waiter fails typed");
    assert!(matches!(
        error,
        TranscriptIngestError::Cancelled { provider: PROVIDER }
    ));
    assert!(lock_codex_meta_cache().in_flight.contains_key(&key));

    gate.release();
    let meta = tokio::time::timeout(SETTLE_WITHIN, owner)
        .await
        .expect("the owner settles")
        .unwrap()
        .unwrap();
    assert_eq!(meta.meta.session_id, SESSION_ID);

    // An already-cancelled request never elects or joins a fill.
    let error = shared_session_meta_with_provenance(&path, &cancellation)
        .await
        .err()
        .expect("a cancelled request fails typed");
    assert!(matches!(
        error,
        TranscriptIngestError::Cancelled { provider: PROVIDER }
    ));
}

#[tokio::test]
async fn failed_fill_releases_its_claim_and_waiters_fail_typed() {
    let (_tmp, path, gate) = fixture("no-session-meta.jsonl", false);
    let key = cache_key(&path);
    let reads_before = session_meta_read_count_for_test(&path);
    let waits_before = in_flight_waits_for_test(&key);

    let first = spawn_lookup(&path, ObservationCancellation::default());
    wait_parked(&gate, 1).await;
    let second = spawn_lookup(&path, ObservationCancellation::default());
    wait_in_flight_waits(&key, waits_before + 1).await;
    gate.release();

    let (first, second) = tokio::time::timeout(SETTLE_WITHIN, async {
        (first.await.unwrap(), second.await.unwrap())
    })
    .await
    .expect("a failed fill must release its waiters");
    for outcome in [first, second] {
        assert!(matches!(
            outcome
                .err()
                .expect("a rollout without metadata fails typed"),
            TranscriptIngestError::InvalidSourceIdentity {
                provider: PROVIDER,
                ..
            }
        ));
    }
    // The waiter re-elected its own fill after the terminal failure instead of
    // spinning behind the failed claim; both parses reported the same typed
    // refusal.
    assert_eq!(session_meta_read_count_for_test(&path) - reads_before, 2);
    let cache = lock_codex_meta_cache();
    assert!(!cache.in_flight.contains_key(&key));
    assert!(cache.entries.iter().all(|entry| entry.key != key));
}

#[test]
fn late_claim_release_never_erases_a_replacement_owner() {
    let tmp = TempDir::new().unwrap();
    let path = write_rollout(tmp.path(), "late-claim-release.jsonl", true);
    let key = cache_key(&path);
    let stale = CodexMetaFillClaim {
        key: key.clone(),
        settled: Arc::new(Notify::new()),
    };
    let replacement = Arc::new(Notify::new());
    lock_codex_meta_cache()
        .in_flight
        .insert(key.clone(), Arc::clone(&replacement));

    drop(stale);

    let retained = lock_codex_meta_cache()
        .in_flight
        .remove(&key)
        .expect("the replacement owner's claim survives a stale release");
    assert!(Arc::ptr_eq(&retained, &replacement));
}

#[test]
fn torn_down_fill_task_releases_its_claim_without_publishing() {
    let (_tmp, path, gate) = fixture("torn-down-fill.jsonl", true);
    let key = cache_key(&path);
    let reads_before = session_meta_read_count_for_test(&path);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let lookup = runtime.spawn({
        let path = path.clone();
        async move {
            shared_session_meta_with_provenance(&path, &ObservationCancellation::default()).await
        }
    });
    gate.wait_parked(1);
    assert!(lock_codex_meta_cache().in_flight.contains_key(&key));

    // Shutting the runtime down tears the fill task down at its await point
    // while the blocking parse is still parked; the owner releases its claim.
    drop(lookup);
    runtime.shutdown_background();
    let released_by = std::time::Instant::now() + SETTLE_WITHIN;
    while lock_codex_meta_cache().in_flight.contains_key(&key) {
        assert!(
            std::time::Instant::now() < released_by,
            "a torn-down fill owner must release its claim"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    gate.release();

    // The orphaned worker's result is discarded rather than written into the
    // cache, and a fresh runtime fills the key with its own parse.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let meta = runtime
        .block_on(async {
            tokio::time::timeout(
                SETTLE_WITHIN,
                shared_session_meta_with_provenance(&path, &ObservationCancellation::default()),
            )
            .await
        })
        .expect("a fresh fill settles")
        .unwrap();
    assert_eq!(meta.meta.session_id, SESSION_ID);
    assert_eq!(
        session_meta_read_count_for_test(&path) - reads_before,
        2,
        "the replacement fill ran its own parse"
    );
    assert!(!lock_codex_meta_cache().in_flight.contains_key(&key));
    runtime.shutdown_background();
}
