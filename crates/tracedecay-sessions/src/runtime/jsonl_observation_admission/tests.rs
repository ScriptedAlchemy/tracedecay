//! Cover-past contract tests for the shared JSONL admission seam.
//!
//! The seam has exactly two dispositions for an admitted frame that fails:
//!
//! 1. A deterministic content refusal covers past the frame with a durable
//!    `AdmissionRefused` coverage row so the stream converges.
//! 2. Everything else — store commit/read-back failures, unbound authorities,
//!    retryable races — is a typed [`TranscriptIngestError::HostAdmission`]
//!    block: the frontier does not advance and no coverage is written over a
//!    record whose durable fate is unknown.
//!
//! Exact duplicates (same identity + digest) are idempotent no-op receipts on
//! the persist path and never reach either disposition.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::json;
use tracedecay_domain::{
    ObservationScopeV1, ObservationSourceCursorV1, ObservationSourceIdentityV1, ProviderId,
    SessionId,
};
use tracedecay_store::ParseOffset;
use tracedecay_store::observation::{
    CursorAdvanceOutcome, ObservationCoverageReason, ObservationCursorAdvance,
};

use crate::admission::test_support::MemoryHostAdmission;
use crate::admission::{
    AdmissionFuture, HostAdmission, HostAdmissionOutcome, HostProjectionDrainOutcome,
};
use crate::observation::{
    CaptureObservationOutcome, CaptureObservationRequest, ObservationCancellation,
};
use crate::runtime::codex::try_admit_codex_jsonl_observations_for_profile_with_admission;
use crate::runtime::source::TranscriptIngestError;

/// Wraps [`MemoryHostAdmission`] so a test can script the capture verdict and
/// observe every cover-past cursor write the seam attempts.
#[derive(Default)]
struct SeamSpyAdmission {
    inner: MemoryHostAdmission,
    scripted_capture_error: Mutex<Option<HostAdmissionOutcome>>,
    report_no_cursor: AtomicBool,
    cover_past_advances: Mutex<Vec<ObservationCursorAdvance>>,
}

impl SeamSpyAdmission {
    fn script_capture_error(&self, outcome: HostAdmissionOutcome) {
        *self.scripted_capture_error.lock().unwrap() = Some(outcome);
    }

    fn cover_past_advances(&self) -> Vec<ObservationCursorAdvance> {
        self.cover_past_advances.lock().unwrap().clone()
    }
}

impl HostAdmission for SeamSpyAdmission {
    fn capture_observation<'a>(
        &'a self,
        request: CaptureObservationRequest,
    ) -> AdmissionFuture<'a, CaptureObservationOutcome> {
        Box::pin(async move {
            if let Some(outcome) = *self.scripted_capture_error.lock().unwrap() {
                return Err(outcome);
            }
            self.inner.capture_observation(request).await
        })
    }

    fn advance_non_durable_source_cursor<'a>(
        &'a self,
        advance: ObservationCursorAdvance,
        cancellation: ObservationCancellation,
    ) -> AdmissionFuture<'a, CursorAdvanceOutcome> {
        self.cover_past_advances
            .lock()
            .unwrap()
            .push(advance.clone());
        self.inner
            .advance_non_durable_source_cursor(advance, cancellation)
    }

    fn get_source_cursor<'a>(
        &'a self,
        source: &'a ObservationSourceIdentityV1,
        scope: &'a ObservationScopeV1,
    ) -> AdmissionFuture<'a, Option<ObservationSourceCursorV1>> {
        if self.report_no_cursor.load(Ordering::SeqCst) {
            return Box::pin(async { Ok(None) });
        }
        self.inner.get_source_cursor(source, scope)
    }

    fn drain_projection_queue<'a>(
        &'a self,
        provider: &'a str,
        scope: &'a ObservationScopeV1,
        cancellation: &'a ObservationCancellation,
        max: usize,
    ) -> AdmissionFuture<'a, HostProjectionDrainOutcome> {
        self.inner
            .drain_projection_queue(provider, scope, cancellation, max)
    }

    fn has_session_message<'a>(
        &'a self,
        scope: &'a ObservationScopeV1,
        provider: &'a str,
        message_id: &'a str,
    ) -> AdmissionFuture<'a, bool> {
        self.inner.has_session_message(scope, provider, message_id)
    }

    fn get_parse_offset<'a>(
        &'a self,
        scope: &'a ObservationScopeV1,
        path: &'a str,
    ) -> AdmissionFuture<'a, Option<ParseOffset>> {
        self.inner.get_parse_offset(scope, path)
    }

    fn advance_parse_offset<'a>(
        &'a self,
        scope: &'a ObservationScopeV1,
        path: &'a str,
        offset: ParseOffset,
    ) -> AdmissionFuture<'a, ()> {
        self.inner.advance_parse_offset(scope, path, offset)
    }
}

const SESSION_ID: &str = "seam-contract-session";

/// Two durable Codex records: the session meta and one user message.
fn write_rollout(path: &Path, cwd: &Path) -> u64 {
    let lines = [
        json!({
            "timestamp": "2026-01-01T00:00:00.000Z",
            "type": "session_meta",
            "payload": {
                "id": SESSION_ID,
                "cwd": cwd,
                "model": "gpt-5.5"
            }
        }),
        json!({
            "timestamp": "2026-01-01T00:00:01.000Z",
            "type": "event_msg",
            "payload": {
                "type": "user_message",
                "message": "seam contract message"
            }
        }),
    ];
    let contents = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    std::fs::write(path, &contents).unwrap();
    u64::try_from(contents.len()).unwrap()
}

fn rollout_fixture() -> (tempfile::TempDir, PathBuf, u64) {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path().join("workspace");
    std::fs::create_dir_all(&cwd).unwrap();
    let path = temp.path().join("rollout.jsonl");
    let len = write_rollout(&path, &cwd);
    (temp, path, len)
}

async fn stored_cursor(spy: &SeamSpyAdmission) -> Option<ObservationSourceCursorV1> {
    let source = ObservationSourceIdentityV1::for_provider(
        ProviderId::new("codex").unwrap(),
        SessionId::new(SESSION_ID).unwrap(),
    )
    .unwrap();
    spy.get_source_cursor(&source, &ObservationScopeV1::Profile)
        .await
        .unwrap()
}

#[tokio::test]
async fn commit_failures_block_typed_and_never_cover_past() {
    for reason in [
        "observation_commit_failed",
        "authority_write_failed",
        "observation_persisted_value_unavailable",
    ] {
        let (_temp, path, _len) = rollout_fixture();
        let spy = SeamSpyAdmission::default();
        spy.script_capture_error(HostAdmissionOutcome::degraded(reason));

        let error = try_admit_codex_jsonl_observations_for_profile_with_admission(
            &path,
            None,
            &[],
            &spy,
            None,
        )
        .await
        .expect_err("a commit failure must block the source instead of covering past it");

        match error {
            TranscriptIngestError::HostAdmission {
                provider: "codex",
                reason: surfaced,
                retryable: false,
            } => assert_eq!(surfaced, reason),
            other => panic!("commit failure must stay a typed admission block, got {other:?}"),
        }
        assert!(
            spy.cover_past_advances().is_empty(),
            "{reason}: no coverage may be written over an uncommitted record"
        );
        assert!(spy.inner.observations().is_empty());
        assert!(
            stored_cursor(&spy).await.is_none(),
            "{reason}: the source frontier must not advance"
        );
    }
}

#[tokio::test]
async fn retryable_admission_failures_keep_their_own_verdict() {
    let (_temp, path, _len) = rollout_fixture();
    let spy = SeamSpyAdmission::default();
    spy.script_capture_error(HostAdmissionOutcome::retained_backpressured(
        "cursor_conflict",
    ));

    let error =
        try_admit_codex_jsonl_observations_for_profile_with_admission(&path, None, &[], &spy, None)
            .await
            .expect_err("a retryable race must surface for another pass");

    assert!(
        matches!(
            error,
            TranscriptIngestError::HostAdmission {
                provider: "codex",
                reason: "cursor_conflict",
                retryable: true,
            }
        ),
        "retryable races must not be laundered into a terminal record verdict: {error:?}"
    );
    assert!(spy.cover_past_advances().is_empty());
    assert!(stored_cursor(&spy).await.is_none());
}

#[tokio::test]
async fn content_refusals_cover_past_so_the_stream_converges() {
    let (_temp, path, len) = rollout_fixture();
    let spy = SeamSpyAdmission::default();
    spy.script_capture_error(HostAdmissionOutcome::degraded(
        "invalid_observation_contract",
    ));

    let progress =
        try_admit_codex_jsonl_observations_for_profile_with_admission(&path, None, &[], &spy, None)
            .await
            .expect("deterministic content refusals must not block the source");

    assert_eq!(progress.bytes_consumed, len);
    let advances = spy.cover_past_advances();
    assert_eq!(advances.len(), 2, "both refused frames must be covered");
    for advance in &advances {
        assert_eq!(
            advance.reason(),
            ObservationCoverageReason::AdmissionRefused
        );
    }
    let cursor = stored_cursor(&spy)
        .await
        .expect("coverage must advance the frontier");
    assert_eq!(cursor.position(), len);

    let replay =
        try_admit_codex_jsonl_observations_for_profile_with_admission(&path, None, &[], &spy, None)
            .await
            .expect("a covered stream must converge");
    assert_eq!(replay.bytes_consumed, 0);
    assert_eq!(
        spy.cover_past_advances().len(),
        2,
        "converged coverage must not be re-written"
    );
}

#[tokio::test]
async fn exact_duplicates_are_idempotent_no_op_receipts() {
    let (_temp, path, len) = rollout_fixture();
    let spy = SeamSpyAdmission::default();

    try_admit_codex_jsonl_observations_for_profile_with_admission(&path, None, &[], &spy, None)
        .await
        .expect("initial admission must persist both records");
    assert_eq!(spy.inner.observations().len(), 2);
    assert!(spy.cover_past_advances().is_empty());
    let committed = stored_cursor(&spy)
        .await
        .expect("persist must advance the frontier");
    assert_eq!(committed.position(), len);

    // Replay the whole file against the already-durable rows, the state a
    // lost or stale frontier read produces: every frame is an exact
    // duplicate (same identity + digest) and must be a silent no-op receipt.
    spy.report_no_cursor.store(true, Ordering::SeqCst);
    let replay =
        try_admit_codex_jsonl_observations_for_profile_with_admission(&path, None, &[], &spy, None)
            .await
            .expect("exact duplicates must be idempotent no-op receipts");
    spy.report_no_cursor.store(false, Ordering::SeqCst);

    assert_eq!(replay.bytes_consumed, len);
    assert_eq!(spy.inner.observations().len(), 2, "no duplicate rows");
    assert!(
        spy.cover_past_advances().is_empty(),
        "an exact duplicate is not an admission refusal and writes no coverage"
    );
    let unchanged = stored_cursor(&spy)
        .await
        .expect("frontier must remain durable");
    assert_eq!(
        unchanged, committed,
        "an exact duplicate replay performs no extra cursor write"
    );
}
