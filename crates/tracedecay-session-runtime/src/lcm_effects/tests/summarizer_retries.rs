use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tracedecay_global_db::RegisteredGlobalDbLeaseV1;
use tracedecay_lcm::summary_convergence::next_candidate;
use tracedecay_runtime_core::db::engine::params;
use tracedecay_runtime_core::test_executable::write_executable_script;

use super::{ProjectSummarizerFixture, message, run_with_pinned_summarizer_binding, session};
use crate::lcm_summary_convergence::{
    LcmSummaryConvergenceDisposition, run_summary_convergence_page,
};
use crate::session_temporal_refresh_scheduler::registry::session_refresh_retry_delay;
use crate::session_temporal_refresh_scheduler::wake::SessionTemporalRefreshRetryClass;

async fn seed_session(database: &RegisteredGlobalDbLeaseV1, session_id: &str) {
    assert!(
        database
            .upsert_session(&session("cursor", session_id))
            .await
    );
    for ordinal in 1..=8 {
        let mut raw = message(session_id, ordinal);
        raw.message_id = format!("{session_id}-{ordinal}");
        database
            .lcm_ingest_raw_message(database.db_path().parent().unwrap(), &raw)
            .await
            .unwrap();
    }
}

struct RetryState {
    state: String,
    failure_code: Option<String>,
    failure_count: i64,
    next_attempt_at_ms: i64,
}

async fn retry_state(database: &RegisteredGlobalDbLeaseV1, session_id: &str) -> RetryState {
    let snapshot = database.read_snapshot().await.unwrap();
    let mut rows = snapshot
        .query(
            "SELECT state, failure_code, failure_count, next_attempt_at_ms
             FROM lcm_summary_convergence_queue
             WHERE provider = 'cursor' AND session_id = ?1",
            params![session_id],
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    RetryState {
        state: row.get(0).unwrap(),
        failure_code: row.get(1).unwrap(),
        failure_count: row.get(2).unwrap(),
        next_attempt_at_ms: row.get(3).unwrap(),
    }
}

#[test]
fn a_summarizer_timeout_is_counted_then_converges_under_the_same_binding() {
    run_with_pinned_summarizer_binding(async {
        let fixture = ProjectSummarizerFixture::open().await;
        let database = fixture.db();
        let session_id = "timeout-then-recovery";
        seed_session(&database, session_id).await;
        let executable = fixture.root.path().join("retrying-cursor-agent");
        let launches = fixture.root.path().join("summarizer-launches");
        write_executable_script(
            &executable,
            format!(
                "#!/bin/sh\nif [ ! -e '{}' ]; then\n  printf '%s\\n' first > '{}'\n  exec /bin/sleep 60\nfi\nprintf '%s\\n' retry >> '{}'\nprintf '%s\\n' 'summary produced by retry'\n",
                launches.display(), launches.display(), launches.display(),
            ),
        )
        .unwrap();
        fixture.pin_cursor_agent(&executable);
        let binding = crate::lcm_summarization::summarizer_binding_identity(&database).unwrap();

        // Model a session that already reached the bounded backoff plateau.
        // The real provider run below must increment, rather than erase, its
        // counted failures and start the next delay after that run settles.
        let transaction = database.begin_write_transaction().await.unwrap();
        transaction
            .execute(
                "UPDATE lcm_summary_convergence_queue
                 SET state = 'retryable', failure_code = 'deadline_exceeded',
                     failure_count = 6, next_attempt_at_ms = 0,
                     attempted_raw_store_id = newest_raw_store_id
                 WHERE provider = 'cursor' AND session_id = ?1",
                params![session_id],
            )
            .await
            .unwrap();
        transaction.commit().await.unwrap();
        let before_run_ms = i64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis(),
        )
        .unwrap();
        let failed = run_summary_convergence_page(database.clone(), 1)
            .await
            .unwrap();
        assert_eq!(failed.sessions.len(), 1);
        assert_eq!(failed.sessions[0].summary_nodes_created, 0);
        assert_eq!(
            failed.sessions[0].disposition,
            LcmSummaryConvergenceDisposition::Retryable {
                reason: "deadline_exceeded".to_owned()
            },
        );
        let retry = retry_state(&database, session_id).await;
        assert_eq!(retry.state, "retryable");
        assert_eq!(retry.failure_code.as_deref(), Some("deadline_exceeded"));
        assert_eq!(retry.failure_count, 7);
        let delay = session_refresh_retry_delay(SessionTemporalRefreshRetryClass::Deadline, 7);
        assert_eq!(
            session_refresh_retry_delay(SessionTemporalRefreshRetryClass::Deadline, u32::MAX),
            delay,
            "repeated failures stay within the existing backoff bound",
        );
        let minimum_run = Duration::from_secs(
            *tracedecay_domain::configuration::LCM_SUMMARIZER_TIMEOUT_SECS_RANGE.start(),
        );
        assert!(
            retry.next_attempt_at_ms
                >= before_run_ms + i64::try_from((minimum_run + delay).as_millis()).unwrap(),
            "the persisted retry deadline must follow provider settlement, not page admission",
        );
        assert!(
            failed
                .next_retry_delay
                .is_some_and(|remaining| remaining <= delay)
        );
        let snapshot = database.read_snapshot().await.unwrap();
        assert!(
            next_candidate(&snapshot, retry.next_attempt_at_ms - 1)
                .await
                .unwrap()
                .is_none()
        );
        let due = next_candidate(&snapshot, retry.next_attempt_at_ms)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(due.session_id, session_id);
        assert_eq!(due.failure_count, 7);
        drop(snapshot);
        let status = database
            .lcm_status("cursor", Some(session_id))
            .await
            .unwrap();
        assert_eq!(status.summary_node_count, 0);
        assert_eq!(status.summary_convergence.retryable_session_count, 1);
        assert_eq!(status.summary_convergence.unavailable_session_count, 0);
        assert_eq!(std::fs::read_to_string(&launches).unwrap(), "first\n");

        // Advance only this fixture's durable due time. Candidate selection,
        // provider execution, and publication still use the production page;
        // no wall-clock sleep or configuration change is needed for recovery.
        let transaction = database.begin_write_transaction().await.unwrap();
        transaction
            .execute(
                "UPDATE lcm_summary_convergence_queue SET next_attempt_at_ms = 0
                 WHERE provider = 'cursor' AND session_id = ?1",
                params![session_id],
            )
            .await
            .unwrap();
        transaction.commit().await.unwrap();
        let recovered = run_summary_convergence_page(database.clone(), 1)
            .await
            .unwrap();
        assert_eq!(recovered.parked_rows_requeued, 0);
        assert_eq!(recovered.sessions.len(), 1);
        assert!(recovered.sessions[0].summary_nodes_created > 0);
        assert_eq!(
            std::fs::read_to_string(&launches).unwrap(),
            "first\nretry\n"
        );
        assert_eq!(
            crate::lcm_summarization::summarizer_binding_identity(&database).unwrap(),
            binding
        );
        let status = database
            .lcm_status("cursor", Some(session_id))
            .await
            .unwrap();
        assert!(status.summary_node_count > 0);
        assert_eq!(status.summary_convergence.retryable_session_count, 0);
        assert_eq!(status.summary_convergence.unavailable_session_count, 0);
        assert!(status.lifecycle.current_frontier_store_id.is_some());
    });
}

#[test]
fn a_missing_configured_summarizer_remains_parked_without_retries() {
    run_with_pinned_summarizer_binding(async {
        let fixture = ProjectSummarizerFixture::open().await;
        let database = fixture.db();
        let session_id = "missing-configured-summarizer";
        seed_session(&database, session_id).await;
        fixture.pin_cursor_agent(&fixture.root.path().join("missing-cursor-agent"));

        let page = run_summary_convergence_page(database.clone(), 1)
            .await
            .unwrap();
        assert_eq!(page.sessions.len(), 1);
        assert_eq!(
            page.sessions[0].disposition,
            LcmSummaryConvergenceDisposition::Pending {
                reason: "cursor_agent_unavailable".to_owned(),
            }
        );
        assert_eq!(page.sessions[0].summary_nodes_created, 0);
        let state = retry_state(&database, session_id).await;
        assert_eq!(state.state, "unavailable");
        assert_eq!(
            state.failure_code.as_deref(),
            Some("cursor_agent_unavailable")
        );
        assert_eq!(state.failure_count, 0);
        assert_eq!(state.next_attempt_at_ms, 0);
        assert!(page.next_retry_delay.is_none());
        let repeated = run_summary_convergence_page(database.clone(), 1)
            .await
            .unwrap();
        assert!(repeated.sessions.is_empty());
        assert!(repeated.next_retry_delay.is_none());
    });
}
