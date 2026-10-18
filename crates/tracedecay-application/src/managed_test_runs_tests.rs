use std::collections::BTreeMap;

use tracedecay_contracts::feedback::TestResultProjectionV1;
use tracedecay_contracts::{
    Deadline, OperationBudgetUsage, OperationReceipt, OperationTermination, RequestId,
};
use tracedecay_domain::{CodeGenerationId, CommitId, ContentDigest, UtcMicros};
use tracedecay_global_db::tests::harness::RegisteredGlobalDbHarness;
use tracedecay_global_db::{ManagedTestRunOutcomeV1, ManagedTestRunStartV1};

use super::{
    CanonicalManagedTestRunReader, ManagedTestRunCurrentScope, ManagedTestRunReadOutcome,
    ManagedTestRunSnapshot, ManagedTestRunStaleReason, ManagedTestRunUnavailableReason,
};
use crate::operation_stream::{OperationEventAuthority, OperationId};

const ROOT: &str = "file:///workspace";
const DOCUMENT: &str = "file:///workspace/src/lib.rs";

fn head() -> CommitId {
    CommitId::new("0123456789abcdef0123456789abcdef01234567").expect("head commit")
}

fn generation() -> CodeGenerationId {
    CodeGenerationId::new("generation.test.current").expect("code generation")
}

fn digest(fill: &str) -> ContentDigest {
    ContentDigest::new(format!("sha256:{}", fill.repeat(64))).expect("content digest")
}

fn deadline() -> Deadline {
    Deadline::new(UtcMicros(i64::MAX)).expect("deadline")
}

fn start(operation_id: &str, started_at: i64) -> ManagedTestRunStartV1 {
    ManagedTestRunStartV1 {
        operation_id: operation_id.to_owned(),
        root_uri: ROOT.to_owned(),
        session_id: None,
        head_commit_id: Some(head()),
        code_generation_id: Some(generation()),
        document_content_digests: BTreeMap::from([(DOCUMENT.to_owned(), digest("a"))]),
        started_at: UtcMicros(started_at),
        requested_tests: 3,
    }
}

fn outcome(started_at: i64) -> ManagedTestRunOutcomeV1 {
    ManagedTestRunOutcomeV1 {
        receipt: OperationReceipt::completed(
            UtcMicros(started_at),
            UtcMicros(started_at + 1),
            deadline(),
            OperationBudgetUsage::default(),
        )
        .expect("receipt"),
        exit_code: Some(101),
        results: vec![
            TestResultProjectionV1 {
                test: "suite::passes".to_owned(),
                passed: true,
            },
            TestResultProjectionV1 {
                test: "suite::fails".to_owned(),
                passed: false,
            },
        ],
        ignored: 1,
    }
}

fn current() -> ManagedTestRunCurrentScope {
    ManagedTestRunCurrentScope {
        root_uri: ROOT.to_owned(),
        head_commit_id: Some(head()),
        code_generation_id: Some(generation()),
        document_uri: Some(DOCUMENT.to_owned()),
        document_content_digest: Some(digest("a")),
    }
}

fn settled_snapshot(operation_id: &str, started_at: i64) -> ManagedTestRunSnapshot {
    let outcome = outcome(started_at);
    ManagedTestRunSnapshot {
        operation_id: operation_id.to_owned(),
        head_commit_id: Some(head()),
        code_generation_id: Some(generation()),
        document_content_digests: BTreeMap::from([(DOCUMENT.to_owned(), digest("a"))]),
        deadline: deadline(),
        results: outcome.results,
        completed: 3,
        total: Some(3),
        termination: Some(OperationTermination::Completed),
    }
}

/// A fresh event authority is what the live stream holds after a daemon
/// restart or once it evicted the run: nothing.
#[tokio::test]
async fn a_settled_run_reads_back_after_the_live_stream_forgot_it() {
    let harness = RegisteredGlobalDbHarness::open("managed-test-run-reader-settled").await;
    harness
        .registered
        .record_managed_test_run_start(&start("run-settled", 1_000))
        .await
        .expect("record start");
    harness
        .registered
        .record_managed_test_run_outcome("run-settled", &outcome(1_000))
        .await
        .expect("record outcome");
    let harness = harness.restart().await;
    let reader = CanonicalManagedTestRunReader::new(
        harness.registered.clone(),
        OperationEventAuthority::default(),
    );

    assert_eq!(
        reader.latest_current(&current()).await,
        ManagedTestRunReadOutcome::Current(settled_snapshot("run-settled", 1_000))
    );
}

#[tokio::test]
async fn an_executing_run_reports_live_progress_until_its_outcome_is_recorded() {
    let harness = RegisteredGlobalDbHarness::open("managed-test-run-reader-live").await;
    let store = harness.registered.clone();
    let events = OperationEventAuthority::default();
    let request_id = RequestId::new("request.test-run.reader-live").expect("request id");
    let operation_id = OperationId::from_request(request_id.clone()).to_string();
    let live_deadline = Deadline::new(UtcMicros(i64::MAX - 1)).expect("deadline");
    let emitter = events
        .begin_managed_test_run(ROOT.to_owned(), request_id, live_deadline.clone())
        .await
        .expect("managed test run");
    store
        .record_managed_test_run_start(&start(&operation_id, 1_000))
        .await
        .expect("record start");
    emitter.progress(2, Some(3)).await.expect("progress");
    let reader = CanonicalManagedTestRunReader::new(store.clone(), events);

    assert_eq!(
        reader.latest_current(&current()).await,
        ManagedTestRunReadOutcome::Current(ManagedTestRunSnapshot {
            deadline: live_deadline,
            results: Vec::new(),
            completed: 2,
            termination: None,
            ..settled_snapshot(&operation_id, 1_000)
        })
    );

    store
        .record_managed_test_run_outcome(&operation_id, &outcome(1_000))
        .await
        .expect("record outcome");
    emitter
        .terminal(outcome(1_000).receipt)
        .await
        .expect("terminal receipt");
    assert_eq!(
        reader.latest_current(&current()).await,
        ManagedTestRunReadOutcome::Current(settled_snapshot(&operation_id, 1_000))
    );
}

#[tokio::test]
async fn an_unsettled_run_nothing_executes_is_abandoned_not_reported_running() {
    let harness = RegisteredGlobalDbHarness::open("managed-test-run-reader-abandoned").await;
    let store = harness.registered.clone();
    store
        .record_managed_test_run_start(&start("run-settled", 1_000))
        .await
        .expect("record older start");
    store
        .record_managed_test_run_outcome("run-settled", &outcome(1_000))
        .await
        .expect("record older outcome");
    store
        .record_managed_test_run_start(&start("run-abandoned", 2_000))
        .await
        .expect("record newer start");
    let reader = CanonicalManagedTestRunReader::new(store, OperationEventAuthority::default());

    assert_eq!(
        reader.latest_current(&current()).await,
        ManagedTestRunReadOutcome::Unavailable(ManagedTestRunUnavailableReason::Abandoned)
    );
}

#[tokio::test]
async fn a_root_without_a_recorded_run_is_unrecorded() {
    let harness = RegisteredGlobalDbHarness::open("managed-test-run-reader-unrecorded").await;
    let reader = CanonicalManagedTestRunReader::new(
        harness.registered.clone(),
        OperationEventAuthority::default(),
    );

    assert_eq!(
        reader.latest_current(&current()).await,
        ManagedTestRunReadOutcome::Unavailable(ManagedTestRunUnavailableReason::Unrecorded)
    );
}

#[tokio::test]
async fn a_recorded_run_is_refused_once_its_source_or_document_moved_on() {
    let harness = RegisteredGlobalDbHarness::open("managed-test-run-reader-drift").await;
    let store = harness.registered.clone();
    store
        .record_managed_test_run_start(&start("run-drift", 1_000))
        .await
        .expect("record start");
    store
        .record_managed_test_run_outcome("run-drift", &outcome(1_000))
        .await
        .expect("record outcome");
    let reader = CanonicalManagedTestRunReader::new(store, OperationEventAuthority::default());

    for (moved, expected) in [
        (
            ManagedTestRunCurrentScope {
                head_commit_id: Some(
                    CommitId::new("fedcba9876543210fedcba9876543210fedcba98")
                        .expect("changed head"),
                ),
                ..current()
            },
            ManagedTestRunReadOutcome::Stale(ManagedTestRunStaleReason::SourceIdentity),
        ),
        (
            ManagedTestRunCurrentScope {
                code_generation_id: Some(
                    CodeGenerationId::new("generation.test.changed").expect("changed generation"),
                ),
                ..current()
            },
            ManagedTestRunReadOutcome::Stale(ManagedTestRunStaleReason::SourceIdentity),
        ),
        (
            ManagedTestRunCurrentScope {
                document_content_digest: Some(digest("b")),
                ..current()
            },
            ManagedTestRunReadOutcome::Stale(ManagedTestRunStaleReason::DocumentContent),
        ),
        (
            ManagedTestRunCurrentScope {
                document_uri: Some("file:///workspace/src/other.rs".to_owned()),
                ..current()
            },
            ManagedTestRunReadOutcome::Unavailable(
                ManagedTestRunUnavailableReason::RetainedDocumentUnbound,
            ),
        ),
        (
            ManagedTestRunCurrentScope {
                document_content_digest: None,
                ..current()
            },
            ManagedTestRunReadOutcome::Unavailable(
                ManagedTestRunUnavailableReason::CurrentDocumentUnbound,
            ),
        ),
    ] {
        assert_eq!(reader.latest_current(&moved).await, expected, "{moved:?}");
    }
}
