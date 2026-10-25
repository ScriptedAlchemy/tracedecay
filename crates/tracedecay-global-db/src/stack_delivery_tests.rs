use tracedecay_runtime_core::db::engine::Executor;

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

#[asupersync::test]
async fn staged_rows_from_an_older_daemon_return_to_pending_on_open() {
    let harness = RegisteredGlobalDbHarness::open("github-stack-delivery-legacy-staging").await;
    let record = signal("signal.github-stack-legacy-staged", 10);
    harness
        .registered
        .append_github_stack_signal(
            record.clone(),
            vec![RECIPIENT.to_owned(), "actor.other".to_owned()],
        )
        .await
        .expect("append durable signal");
    harness
        .registered
        .acknowledge_github_stack_host_delivery(PROJECT_ID, &record.signal_id, "actor.other")
        .await
        .expect("settle one recipient");
    harness
        .registered
        .transact(async move |transaction| {
            transaction
                .execute_batch(
                    "DROP TRIGGER github_stack_delivery_recipients_settlement_guard;
                     UPDATE github_stack_delivery_recipients SET state = 'host_pending'
                     WHERE state = 'pending';",
                )
                .await
                .map_err(|error| error.to_string())
        })
        .await
        .expect("stage job")
        .expect("stage a row as the retired daemon did");

    let harness = harness.restart().await;
    assert_eq!(
        state(&harness, &record.signal_id, RECIPIENT).await,
        Some(GitHubStackDeliveryStateV1::Pending)
    );
    assert_eq!(
        state(&harness, &record.signal_id, "actor.other").await,
        Some(GitHubStackDeliveryStateV1::Settled),
        "another recipient's receipt is kept"
    );
    harness
        .registered
        .acknowledge_github_stack_host_delivery(PROJECT_ID, &record.signal_id, RECIPIENT)
        .await
        .expect("normalized row settles through the current guard");
}
