use sha2::Digest;
use tracedecay_runtime_core::db::engine::params;

use crate::tests::harness::writer_telemetry;
use crate::{
    GitHubStackDeliveryStateV1, GitHubStackSignalAppendOutcomeV1, GitHubStackSignalRecordV1,
    MAX_GITHUB_STACK_ACTIVE_PENDING_V1, tests::harness::RegisteredGlobalDbHarness,
};

const PROJECT_ID: &str = "project.github-stack-delivery";
const SCOPE_DIGEST: &str = "sha256:github-stack-delivery-scope";
const RECIPIENT: &str = "actor.cursor-composer";

fn signal(signal_id: &str, observed_at_micros: i64) -> GitHubStackSignalRecordV1 {
    GitHubStackSignalRecordV1 {
        project_id: PROJECT_ID.to_owned(),
        signal_id: signal_id.to_owned(),
        scope_digest: SCOPE_DIGEST.to_owned(),
        repository_id: "repository.github-stack-delivery".to_owned(),
        watermark_id: "watermark.github-stack-delivery".to_owned(),
        observed_at_micros,
        signal_json: "{}".to_owned(),
    }
}

async fn state(
    harness: &RegisteredGlobalDbHarness,
    signal_id: &str,
    recipient: &str,
) -> Option<GitHubStackDeliveryStateV1> {
    harness
        .registered
        .github_stack_recipient_state(PROJECT_ID, signal_id, recipient)
        .await
        .expect("read recipient state")
}

#[asupersync::test]
async fn pending_delivery_survives_restart_and_settles_only_after_acknowledgement() {
    let harness = RegisteredGlobalDbHarness::open("github-stack-delivery-restart").await;
    let record = signal("signal.github-stack-restart", 10);
    harness
        .registered
        .append_github_stack_signal(record.clone(), vec![RECIPIENT.to_owned()])
        .await
        .expect("append durable signal");

    let harness = harness.restart().await;
    assert!(
        harness
            .registered
            .pending_github_stack_deliveries(PROJECT_ID, "sha256:wrong-scope", None)
            .await
            .expect("scope-isolated page")
            .is_empty()
    );
    assert!(
        harness
            .registered
            .pending_github_stack_deliveries(PROJECT_ID, SCOPE_DIGEST, Some("actor.other"))
            .await
            .expect("recipient-isolated page")
            .is_empty()
    );
    let expected = vec![crate::GitHubStackDeliveryRecordV1 {
        signal: record.clone(),
        recipient: RECIPIENT.to_owned(),
    }];
    for recipient in [None, Some(RECIPIENT)] {
        assert_eq!(
            harness
                .registered
                .pending_github_stack_deliveries(PROJECT_ID, SCOPE_DIGEST, recipient)
                .await
                .expect("recover pending delivery after restart"),
            expected,
            "listing {recipient:?} must not settle"
        );
    }
    assert_eq!(
        state(&harness, &record.signal_id, RECIPIENT).await,
        Some(GitHubStackDeliveryStateV1::Pending)
    );

    for _ in 0..2 {
        harness
            .registered
            .acknowledge_github_stack_host_delivery(PROJECT_ID, &record.signal_id, RECIPIENT)
            .await
            .expect("authenticated host acknowledgement is idempotent");
    }
    assert_eq!(
        state(&harness, &record.signal_id, RECIPIENT).await,
        Some(GitHubStackDeliveryStateV1::Settled)
    );
    assert!(
        harness
            .registered
            .pending_github_stack_deliveries(PROJECT_ID, SCOPE_DIGEST, None)
            .await
            .expect("settled rows leave the pending page")
            .is_empty()
    );
}

#[asupersync::test]
async fn saturation_is_per_recipient_and_defers_the_exact_overflow_recipient() {
    let harness = RegisteredGlobalDbHarness::open("github-stack-delivery-saturation").await;
    for index in 0..MAX_GITHUB_STACK_ACTIVE_PENDING_V1 {
        harness
            .registered
            .append_github_stack_signal(
                signal(
                    &format!("signal.github-stack-capacity.{index}"),
                    10 + index as i64,
                ),
                vec![RECIPIENT.to_owned()],
            )
            .await
            .expect("fill one recipient's bounded pending capacity");
    }

    let overflow = signal("signal.github-stack-overflow", 1_000);
    assert!(matches!(
        harness
            .registered
            .append_github_stack_signal(overflow.clone(), vec![RECIPIENT.to_owned()])
            .await
            .expect("retain bounded overflow"),
        GitHubStackSignalAppendOutcomeV1::Saturated {
            pending_count: MAX_GITHUB_STACK_ACTIVE_PENDING_V1,
            deferred_count: 1,
        }
    ));
    assert_eq!(
        state(&harness, &overflow.signal_id, RECIPIENT).await,
        Some(GitHubStackDeliveryStateV1::Deferred)
    );

    let other = signal("signal.github-stack-other-recipient", 1_001);
    harness
        .registered
        .append_github_stack_signal(other.clone(), vec!["actor.other".to_owned()])
        .await
        .expect("another recipient is not blocked by the full one");
    assert_eq!(
        state(&harness, &other.signal_id, "actor.other").await,
        Some(GitHubStackDeliveryStateV1::Pending)
    );
    assert_eq!(
        harness
            .registered
            .pending_github_stack_deliveries(PROJECT_ID, SCOPE_DIGEST, Some(RECIPIENT))
            .await
            .expect("recipient page")
            .len(),
        MAX_GITHUB_STACK_ACTIVE_PENDING_V1,
        "deferred overflow stays out of the pending page"
    );
}

#[asupersync::test]
async fn settling_a_recipient_promotes_only_its_oldest_deferred_row() {
    let harness = RegisteredGlobalDbHarness::open("github-stack-delivery-promotion").await;
    let both = vec![RECIPIENT.to_owned(), "actor.other".to_owned()];
    for index in 0..MAX_GITHUB_STACK_ACTIVE_PENDING_V1 {
        harness
            .registered
            .append_github_stack_signal(
                signal(
                    &format!("signal.github-stack-promotion.{index}"),
                    10 + index as i64,
                ),
                both.clone(),
            )
            .await
            .expect("fill pending capacity");
    }
    let older = signal("signal.github-stack-overflow-older", 1_000);
    let newer = signal("signal.github-stack-overflow-newer", 1_001);
    for overflow in [&newer, &older] {
        harness
            .registered
            .append_github_stack_signal(overflow.clone(), both.clone())
            .await
            .expect("retain deferred overflow");
    }

    harness
        .registered
        .acknowledge_github_stack_host_delivery(
            PROJECT_ID,
            "signal.github-stack-promotion.0",
            RECIPIENT,
        )
        .await
        .expect("settle one pending recipient");

    assert_eq!(
        state(&harness, &older.signal_id, RECIPIENT).await,
        Some(GitHubStackDeliveryStateV1::Pending)
    );
    assert_eq!(
        state(&harness, &newer.signal_id, RECIPIENT).await,
        Some(GitHubStackDeliveryStateV1::Deferred)
    );
    assert_eq!(
        state(&harness, &older.signal_id, "actor.other").await,
        Some(GitHubStackDeliveryStateV1::Deferred),
        "another recipient's backlog is untouched"
    );
}

async fn stored_signal_payload_bytes(harness: &RegisteredGlobalDbHarness, signal_id: &str) -> i64 {
    let snapshot = harness
        .registered
        .read_snapshot()
        .await
        .expect("read stored signal");
    let mut rows = snapshot
        .query(
            "SELECT length(signal_json) FROM github_stack_delivery_signals
             WHERE project_id = ?1 AND signal_id = ?2",
            params![PROJECT_ID, signal_id],
        )
        .await
        .expect("query stored payload size");
    rows.next()
        .await
        .expect("read stored payload size")
        .expect("signal identity remains after retirement")
        .get::<i64>(0)
        .expect("decode stored payload bytes")
}

#[asupersync::test]
async fn noncanonical_retirement_marker_cannot_reopen_an_expired_signal() {
    let harness = RegisteredGlobalDbHarness::open("github-stack-retirement-marker-shape").await;
    let record = signal("signal.github-stack-retirement-whitespace", 10);
    harness
        .registered
        .append_github_stack_signal(record.clone(), vec![RECIPIENT.to_owned()])
        .await
        .expect("append a real signal");
    harness
        .registered
        .acknowledge_github_stack_host_delivery(PROJECT_ID, &record.signal_id, RECIPIENT)
        .await
        .expect("settle the recipient");

    // The installed SQL trigger accepts equivalent JSON with whitespace.
    // The old byte-prefix decoder treated it as an unretired signal.
    let digest = hex::encode(sha2::Sha256::digest(record.signal_json.as_bytes()));
    let whitespace_marker = format!(
        "{{\n  \"__tracedecay_retired_stack_sha256_v1\": \"{digest}\"\n}}"
    );
    let writer = harness
        .registered
        .writer_connection()
        .expect("registered retirement-marker writer");
    writer
        .execute(
            "UPDATE github_stack_delivery_signals SET signal_json = ?3
             WHERE project_id = ?1 AND signal_id = ?2",
            params![PROJECT_ID, record.signal_id.as_str(), whitespace_marker.as_str()],
        )
        .await
        .expect("the terminal-only marker is allowed by SQLite");
    drop(writer);
    assert!(
        harness
            .registered
            .github_stack_signal(PROJECT_ID, &record.signal_id)
            .await
            .expect("a retired marker must never expand as a signal")
            .is_none(),
        "whitespace in a legitimate retirement marker must not expose it"
    );
    assert!(matches!(
        harness
            .registered
            .append_github_stack_signal(record.clone(), vec![RECIPIENT.to_owned()])
            .await
            .expect("the identical replay remains recognized"),
        GitHubStackSignalAppendOutcomeV1::Replayed { .. }
    ));
    let mut conflict = record.clone();
    conflict.signal_json = r#"{"different":true}"#.to_owned();
    assert!(
        harness
            .registered
            .append_github_stack_signal(conflict, vec![RECIPIENT.to_owned()])
            .await
            .expect_err("changed bytes must conflict with retained fingerprint")
            .contains("identity conflict")
    );

    let malformed = signal("signal.github-stack-malformed-marker", 11);
    harness
        .registered
        .append_github_stack_signal(malformed.clone(), vec![RECIPIENT.to_owned()])
        .await
        .expect("append a second signal");
    harness
        .registered
        .acknowledge_github_stack_host_delivery(PROJECT_ID, &malformed.signal_id, RECIPIENT)
        .await
        .expect("settle second recipient");
    let invalid_marker = format!(
        r#"{{"__tracedecay_retired_stack_sha256_v1":"{}"}}"#,
        "g".repeat(64)
    );
    let rejection = harness
        .registered
        .writer_connection()
        .expect("registered malformed-marker writer")
        .execute(
            "UPDATE github_stack_delivery_signals SET signal_json = ?3
             WHERE project_id = ?1 AND signal_id = ?2",
            params![PROJECT_ID, malformed.signal_id.as_str(), invalid_marker.as_str()],
        )
        .await
        .expect_err("the SQL guard must reject a non-hexadecimal fingerprint")
        .to_string();
    assert!(
        rejection.contains("GitHub stack delivery signals are immutable"),
        "{rejection}"
    );
    assert_eq!(
        harness
            .registered
            .github_stack_signal(PROJECT_ID, &malformed.signal_id)
            .await
            .expect("refused invalid marker cannot alter the original"),
        Some(malformed)
    );
}

#[asupersync::test]
async fn terminal_compaction_preserves_pending_peers_and_refuses_replay_collision() {
    let harness = RegisteredGlobalDbHarness::open("github-stack-terminal-compaction").await;
    let mut record = signal("signal.github-stack-old", 10);
    record.signal_json = format!(r#"{{"large":"{}"}}"#, "x".repeat(80_000));
    let recipients = vec![RECIPIENT.to_owned(), "actor.other".to_owned()];
    harness
        .registered
        .append_github_stack_signal(record.clone(), recipients.clone())
        .await
        .expect("durably append old signal");
    let original_size = stored_signal_payload_bytes(&harness, &record.signal_id).await;

    harness
        .registered
        .acknowledge_github_stack_host_delivery(PROJECT_ID, &record.signal_id, RECIPIENT)
        .await
        .expect("settle first recipient");
    assert_eq!(
        harness
            .registered
            .retire_github_stack_terminal_payloads_before(11)
            .await
            .expect("attempt cleanup with pending peer")
            .retired,
        0,
        "a pending peer retains its exact payload"
    );
    assert_eq!(
        harness
            .registered
            .github_stack_signal(PROJECT_ID, &record.signal_id)
            .await
            .expect("read live signal"),
        Some(record.clone())
    );
    harness
        .registered
        .record_github_stack_authorization_loss(PROJECT_ID, &record.signal_id, "actor.other")
        .await
        .expect("record permanently lost authorization");
    assert_eq!(
        harness
            .registered
            .retire_github_stack_terminal_payloads_before(10)
            .await
            .expect("respect inspection window")
            .retired,
        0,
        "the cutoff excludes the issue timestamp itself"
    );
    assert_eq!(
        harness
            .registered
            .retire_github_stack_terminal_payloads_before(11)
            .await
            .expect("retire fully terminal signal")
            .retired,
        1
    );
    assert!(
        stored_signal_payload_bytes(&harness, &record.signal_id).await < original_size / 100,
        "retire the large transport payload rather than moving it to another log"
    );
    assert!(
        harness
            .registered
            .github_stack_signal(PROJECT_ID, &record.signal_id)
            .await
            .expect("old expansion must expire")
            .is_none(),
        "a compact fingerprint must never be returned as expandable signal JSON"
    );
    assert_eq!(
        state(&harness, &record.signal_id, RECIPIENT).await,
        None,
        "retired recipient bookkeeping is no longer part of the active index"
    );
    assert!(matches!(
        harness
            .registered
            .append_github_stack_signal(record.clone(), recipients.clone())
            .await
            .expect("identical replay remains recognized"),
        GitHubStackSignalAppendOutcomeV1::Replayed { .. }
    ));
    let mut conflict = record.clone();
    conflict.signal_json = r#"{"different":true}"#.to_owned();
    assert!(
        harness
            .registered
            .append_github_stack_signal(conflict, recipients.clone())
            .await
            .is_err(),
        "retired signal identities reject mismatched payloads"
    );

    let restarted = harness.restart().await;
    assert!(matches!(
        restarted
            .registered
            .append_github_stack_signal(record, recipients)
            .await
            .expect("the small replay fingerprint survives restart"),
        GitHubStackSignalAppendOutcomeV1::Replayed { .. }
    ));
    assert_eq!(
        restarted
            .registered
            .pending_github_stack_deliveries(PROJECT_ID, SCOPE_DIGEST, None)
            .await
            .expect("retired data cannot redeliver"),
        vec![]
    );
}

#[asupersync::test]
async fn compaction_is_bounded_and_raw_sql_cannot_delete_unsettled_delivery() {
    let harness = RegisteredGlobalDbHarness::open("github-stack-bounded-retirement").await;
    let count = crate::MAX_GITHUB_STACK_RETIRE_BATCH_V1 + 3;
    for index in 0..count {
        let record = signal(
            &format!("signal.github-stack-batch.{index}"),
            index as i64 + 1,
        );
        harness
            .registered
            .append_github_stack_signal(record.clone(), vec![RECIPIENT.to_owned()])
            .await
            .expect("append batch signal");
        if index + 1 != count {
            harness
                .registered
                .acknowledge_github_stack_host_delivery(PROJECT_ID, &record.signal_id, RECIPIENT)
                .await
                .expect("settle eligible batch signal");
        }
    }
    let live = format!("signal.github-stack-batch.{}", count - 1);
    let pending_deletion = harness
        .registered
        .writer_connection()
        .expect("registered test writer")
        .execute(
            "DELETE FROM github_stack_delivery_recipients
             WHERE project_id = ?1 AND signal_id = ?2",
            params![PROJECT_ID, live.as_str()],
        )
        .await;
    assert!(
        pending_deletion
            .expect_err("the SQL guard protects active pending state")
            .to_string()
            .contains("GitHub stack delivery recipient is not retired"),
        "the trigger must reject deletion of an unsettled recipient"
    );

    let first = harness
        .registered
        .retire_github_stack_terminal_payloads_before(1000)
        .await
        .expect("bounded first page")
        .retired;
    assert_eq!(first, crate::MAX_GITHUB_STACK_RETIRE_BATCH_V1);
    let second = harness
        .registered
        .retire_github_stack_terminal_payloads_before(1000)
        .await
        .expect("next page")
        .retired;
    assert_eq!(second, 2, "pending signal is ineligible across pages");
    assert_eq!(
        state(&harness, &live, RECIPIENT).await,
        Some(GitHubStackDeliveryStateV1::Pending)
    );
    assert_eq!(
        harness
            .registered
            .retire_github_stack_terminal_payloads_before(1000)
            .await
            .expect("fully drained next page")
            .retired,
        0
    );
}

#[asupersync::test]
async fn retention_frontier_bounds_writer_work_and_resumes_past_deferred_backlog() {
    const BLOCKED: usize = 4_096;
    let harness = RegisteredGlobalDbHarness::open("github-stack-retirement-frontier").await;
    let writer = harness
        .registered
        .writer_connection()
        .expect("registered retention fixture writer");
    // One recipient's first 256 signals are pending and all later ones are
    // deferred. The terminal signal is beyond this old, ineligible prefix.
    writer
        .execute(
            "WITH RECURSIVE seq(n) AS (
                 SELECT 0 UNION ALL SELECT n + 1 FROM seq WHERE n < 4095
             )
             INSERT INTO github_stack_delivery_signals
                 (project_id, signal_id, scope_digest, repository_id,
                  watermark_id, observed_at_micros, signal_json)
             SELECT ?1, printf('signal.github-stack-prefix.%05d', n), ?2,
                    'repository.github-stack-delivery', 'watermark.github-stack-delivery',
                    n + 1, json_object('n', n) FROM seq",
            params![PROJECT_ID, SCOPE_DIGEST],
        )
        .await
        .expect("seed a large genuine signal table");
    writer
        .execute(
            "INSERT INTO github_stack_delivery_recipients
                 (project_id, signal_id, recipient, state)
             SELECT project_id, signal_id, ?1,
                    CASE WHEN observed_at_micros <= 256 THEN 'pending'
                         ELSE 'deferred' END
             FROM github_stack_delivery_signals WHERE project_id = ?2",
            params![RECIPIENT, PROJECT_ID],
        )
        .await
        .expect("seed a saturated recipient backlog");
    // The writer client must not hold the first registered attachment open
    // across the restart below.
    drop(writer);
    let late = signal("signal.github-stack-eligible-tail", 10_000);
    // The old recipient is saturated. Give the eligible tail an independently
    // admitted recipient so it can actually settle before retirement.
    let tail_recipient = "actor.terminal-tail";
    harness
        .registered
        .append_github_stack_signal(late.clone(), vec![tail_recipient.to_owned()])
        .await
        .expect("append terminal tail beyond the backlog");
    harness
        .registered
        .acknowledge_github_stack_host_delivery(PROJECT_ID, &late.signal_id, tail_recipient)
        .await
        .expect("settle eligible tail");

    let before = writer_telemetry(&harness.registered);
    let first = harness
        .registered
        .retire_github_stack_terminal_payloads_before(10_001)
        .await
        .expect("scan one bounded pending window");
    let after = writer_telemetry(&harness.registered);
    assert_eq!(
        after.transactions.commands - before.transactions.commands,
        1
    );
    let steps = after.sqlite_vm.vm_steps - before.sqlite_vm.vm_steps;
    assert_eq!(first.retired, 0);
    assert_eq!(
        first.inspected,
        crate::stack_delivery::MAX_GITHUB_STACK_RETIRE_SCAN_V1
    );
    assert!(
        !first.exhausted,
        "the deferred prefix cannot starve the tail"
    );
    assert!(
        (1..30_000).contains(&steps),
        "one idle writer slice must not examine the 4096-row deferred prefix: {steps} VM steps"
    );
    assert_eq!(
        harness
            .registered
            .github_stack_signal(PROJECT_ID, &late.signal_id)
            .await
            .unwrap(),
        Some(late.clone()),
        "the first bounded window must not skip ahead and compact the tail"
    );

    // The scan cursor is durable, not an in-process iterator. A restart must
    // resume past exactly the first 64 inspected rows and reach the tail.
    let restarted = harness.restart().await;
    let mut retired = 0;
    for _ in 0..(BLOCKED / crate::stack_delivery::MAX_GITHUB_STACK_RETIRE_SCAN_V1 + 3) {
        let page = restarted
            .registered
            .retire_github_stack_terminal_payloads_before(10_001)
            .await
            .expect("continue bounded retirement after restart");
        assert!(page.inspected <= crate::stack_delivery::MAX_GITHUB_STACK_RETIRE_SCAN_V1);
        assert!(page.retired <= crate::MAX_GITHUB_STACK_RETIRE_BATCH_V1);
        retired += page.retired;
        if retired != 0 {
            break;
        }
    }
    assert_eq!(retired, 1, "the terminal tail must eventually be reached");
    assert!(
        restarted
            .registered
            .github_stack_signal(PROJECT_ID, &late.signal_id)
            .await
            .unwrap()
            .is_none()
    );

    // On wrap, earlier signals that have since settled are reconsidered.
    let formerly_pending = "signal.github-stack-prefix.00000";
    restarted
        .registered
        .record_github_stack_authorization_loss(PROJECT_ID, formerly_pending, RECIPIENT)
        .await
        .expect("permanently settle an earlier signal");
    let mut saw_earlier = false;
    for _ in 0..(BLOCKED / crate::stack_delivery::MAX_GITHUB_STACK_RETIRE_SCAN_V1 + 3) {
        let page = restarted
            .registered
            .retire_github_stack_terminal_payloads_before(10_001)
            .await
            .expect("revisit old newly settled signals");
        assert!(page.inspected <= crate::stack_delivery::MAX_GITHUB_STACK_RETIRE_SCAN_V1);
        if restarted
            .registered
            .github_stack_signal(PROJECT_ID, formerly_pending)
            .await
            .unwrap()
            .is_none()
        {
            saw_earlier = true;
            break;
        }
    }
    assert!(
        saw_earlier,
        "wrap must not abandon later-settling old signals"
    );
}

#[asupersync::test]
async fn terminal_retirement_cannot_be_accelerated_past_the_issued_window() {
    let harness = RegisteredGlobalDbHarness::open("github-stack-terminal-freshness").await;
    let mut fresh = signal(
        "signal.github-stack-fresh",
        tracedecay_contracts::now_micros().0,
    );
    fresh.signal_json = r#"{"recent":true}"#.to_owned();
    harness
        .registered
        .append_github_stack_signal(fresh.clone(), vec![RECIPIENT.to_owned()])
        .await
        .expect("append current signal");
    harness
        .registered
        .acknowledge_github_stack_host_delivery(PROJECT_ID, &fresh.signal_id, RECIPIENT)
        .await
        .expect("settle current signal");
    assert_eq!(
        harness
            .registered
            .retire_github_stack_terminal_payloads_before(i64::MAX)
            .await
            .expect("future cutoff is conservatively clamped")
            .retired,
        0
    );
    assert_eq!(
        harness
            .registered
            .github_stack_signal(PROJECT_ID, &fresh.signal_id)
            .await
            .expect("recent expanded signal remains inspectable"),
        Some(fresh)
    );
}

#[asupersync::test]
async fn failed_terminal_recipient_retirement_rolls_back_payload_compaction() {
    let harness = RegisteredGlobalDbHarness::open("github-stack-retire-rollback").await;
    let record = signal("signal.github-stack-rollback", 10);
    harness
        .registered
        .append_github_stack_signal(record.clone(), vec![RECIPIENT.to_owned()])
        .await
        .expect("persist signal");
    harness
        .registered
        .acknowledge_github_stack_host_delivery(PROJECT_ID, &record.signal_id, RECIPIENT)
        .await
        .expect("settle signal before retention");

    harness
        .registered
        .writer_connection()
        .expect("registered test writer")
        .execute_batch(
            "CREATE TRIGGER github_stack_retirement_failure
             BEFORE DELETE ON github_stack_delivery_recipients
             BEGIN SELECT RAISE(ABORT, 'simulate interrupted retirement'); END;",
        )
        .await
        .expect("inject failure after payload update");
    let problem = harness
        .registered
        .retire_github_stack_terminal_payloads_before(11)
        .await
        .expect_err("recipient deletion must fail inside the transaction");
    assert!(
        problem.contains("simulate interrupted retirement"),
        "{problem}"
    );
    assert_eq!(
        harness
            .registered
            .github_stack_signal(PROJECT_ID, &record.signal_id)
            .await
            .expect("failed retirement preserves the full payload"),
        Some(record.clone())
    );
    assert_eq!(
        state(&harness, &record.signal_id, RECIPIENT).await,
        Some(GitHubStackDeliveryStateV1::Settled)
    );

    harness
        .registered
        .writer_connection()
        .expect("registered test writer")
        .execute_batch("DROP TRIGGER github_stack_retirement_failure")
        .await
        .expect("remove failure injection");
    assert_eq!(
        harness
            .registered
            .retire_github_stack_terminal_payloads_before(11)
            .await
            .expect("retry after failure")
            .retired,
        1
    );
    let restarted = harness.restart().await;
    assert!(
        restarted
            .registered
            .github_stack_signal(PROJECT_ID, &record.signal_id)
            .await
            .expect("retirement persisted across restart")
            .is_none()
    );
}
