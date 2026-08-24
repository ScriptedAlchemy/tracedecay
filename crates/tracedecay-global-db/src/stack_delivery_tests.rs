use crate::{
    GitHubStackDeliveryKeyV1, GitHubStackDeliveryStateV1, GitHubStackSignalAppendOutcomeV1,
    GitHubStackSignalRecordV1, MAX_GITHUB_STACK_ACTIVE_PENDING_V1,
    tests::harness::RegisteredGlobalDbHarness,
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

#[tokio::test]
async fn host_pending_delivery_survives_restart_and_settles_only_after_acknowledgement() {
    let harness = RegisteredGlobalDbHarness::open("github-stack-delivery-restart").await;
    let record = signal("signal.github-stack-restart", 10);
    harness
        .registered
        .append_github_stack_signal(record.clone(), vec![RECIPIENT.to_owned()])
        .await
        .expect("append durable signal");
    let pending = harness
        .registered
        .pending_github_stack_deliveries(PROJECT_ID, 1)
        .await
        .expect("read pending delivery");
    let key = GitHubStackDeliveryKeyV1::from(
        pending
            .first()
            .expect("exact pending recipient before host handoff"),
    );
    harness
        .registered
        .publish_github_stack_deliveries(PROJECT_ID, &record.watermark_id, &[key])
        .await
        .expect("publish host handoff");
    assert_eq!(
        harness
            .registered
            .github_stack_recipient_state(PROJECT_ID, &record.signal_id, RECIPIENT)
            .await
            .expect("read pre-restart state"),
        Some(GitHubStackDeliveryStateV1::HostPending)
    );

    let harness = harness.restart().await;
    assert!(
        harness
            .registered
            .pending_host_github_stack_deliveries(PROJECT_ID, "sha256:wrong-scope", 1)
            .await
            .expect("scope-isolated host page")
            .is_empty()
    );
    let host_pending = harness
        .registered
        .pending_host_github_stack_deliveries(PROJECT_ID, SCOPE_DIGEST, 1)
        .await
        .expect("recover host-pending handoff after restart");
    assert_eq!(
        host_pending,
        vec![crate::GitHubStackDeliveryRecordV1 {
            signal: record.clone(),
            recipient: RECIPIENT.to_owned()
        }]
    );

    harness
        .registered
        .acknowledge_github_stack_host_delivery(PROJECT_ID, &record.signal_id, RECIPIENT)
        .await
        .expect("authenticated host acknowledgement");
    assert_eq!(
        harness
            .registered
            .github_stack_recipient_state(PROJECT_ID, &record.signal_id, RECIPIENT)
            .await
            .expect("read settled state"),
        Some(GitHubStackDeliveryStateV1::Settled)
    );
}

#[tokio::test]
async fn saturation_defers_but_retains_the_exact_overflow_recipient() {
    let harness = RegisteredGlobalDbHarness::open("github-stack-delivery-saturation").await;
    let recipients = (0..MAX_GITHUB_STACK_ACTIVE_PENDING_V1)
        .map(|index| format!("actor.cursor-composer.{index}"))
        .collect();
    assert!(matches!(
        harness
            .registered
            .append_github_stack_signal(signal("signal.github-stack-capacity", 10), recipients)
            .await
            .expect("fill bounded pending capacity"),
        GitHubStackSignalAppendOutcomeV1::Appended {
            pending_count: MAX_GITHUB_STACK_ACTIVE_PENDING_V1,
            deferred_count: 0,
        }
    ));

    let overflow = signal("signal.github-stack-overflow", 11);
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
        harness
            .registered
            .github_stack_recipient_state(PROJECT_ID, &overflow.signal_id, RECIPIENT)
            .await
            .expect("read retained overflow binding"),
        Some(GitHubStackDeliveryStateV1::Deferred)
    );
}
