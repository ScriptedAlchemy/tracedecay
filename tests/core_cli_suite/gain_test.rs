use std::fs;

use tempfile::TempDir;
use tracedecay::host_admission::HostAdmissionTestRuntimeV1;

async fn open_isolated_runtime(tmp: &TempDir) -> HostAdmissionTestRuntimeV1 {
    HostAdmissionTestRuntimeV1::profile(tmp.path().join(".tracedecay"))
        .await
        .expect("registered profile runtime open")
}

#[tokio::test]
async fn record_and_query_savings_round_trip() {
    let tmp = TempDir::new().unwrap();
    let runtime = open_isolated_runtime(&tmp).await;

    let now: i64 = 1_715_000_000;
    runtime
        .record_savings_for_test("/proj/a", "tracedecay_context", 10_000, 500, now)
        .await;
    runtime
        .record_savings_for_test("/proj/a", "tracedecay_search", 2_000, 100, now + 60)
        .await;
    runtime
        .record_savings_for_test("/proj/b", "tracedecay_context", 5_000, 250, now + 120)
        .await;

    let total_a = runtime.sum_savings_for_test(Some("/proj/a"), 0).await;
    assert_eq!(total_a.saved_tokens, 11_400);
    assert_eq!(total_a.calls, 2);

    let total_all = runtime.sum_savings_for_test(None, 0).await;
    assert_eq!(total_all.saved_tokens, 16_150);
    assert_eq!(total_all.calls, 3);

    // Range filter: only entries after now+90 -> only the third one
    let recent = runtime.sum_savings_for_test(None, now + 90).await;
    assert_eq!(recent.calls, 1);
    assert_eq!(recent.saved_tokens, 4_750);
}

#[tokio::test]
async fn savings_history_buckets_by_day() {
    let tmp = TempDir::new().unwrap();
    let runtime = open_isolated_runtime(&tmp).await;

    // day1 = arbitrary epoch second; day2 = day1 + 86400 + 60s (crosses a UTC midnight)
    let day1 = 1_715_000_000;
    let day2 = day1 + 86_400 + 60;
    runtime
        .record_savings_for_test("/proj/a", "tracedecay_context", 1000, 100, day1)
        .await;
    runtime
        .record_savings_for_test("/proj/a", "tracedecay_context", 500, 50, day1 + 3600)
        .await;
    runtime
        .record_savings_for_test("/proj/a", "tracedecay_context", 800, 80, day2)
        .await;

    let history = runtime.savings_history_for_test(None, 0).await;
    assert_eq!(history.len(), 2);
    // Newest first
    assert_eq!(history[0].saved_tokens, 720); // day2: 800 - 80
    assert_eq!(history[1].saved_tokens, 1350); // day1: (1000-100) + (500-50)
}

#[tokio::test]
async fn savings_project_filters_canonicalize_read_side() {
    let tmp = TempDir::new().unwrap();
    let runtime = open_isolated_runtime(&tmp).await;
    let project_dir = tmp.path().join("project");
    fs::create_dir(&project_dir).unwrap();
    let canonical_project = HostAdmissionTestRuntimeV1::canonical_project_key(&project_dir);
    let project_with_trailing_slash = format!("{}/", project_dir.display());

    runtime
        .record_savings_for_test(&canonical_project, "tracedecay_context", 2000, 250, 86_400)
        .await;

    let total = runtime
        .sum_savings_for_test(Some(&project_with_trailing_slash), 0)
        .await;
    assert_eq!(total.saved_tokens, 1750);
    assert_eq!(total.calls, 1);

    let history = runtime
        .savings_history_for_test(Some(&project_with_trailing_slash), 0)
        .await;
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].saved_tokens, 1750);
    assert_eq!(history[0].calls, 1);
}

#[tokio::test]
async fn record_savings_canonicalizes_project_path_on_write() {
    let tmp = TempDir::new().unwrap();
    let runtime = open_isolated_runtime(&tmp).await;
    let project_dir = tmp.path().join("project");
    fs::create_dir(&project_dir).unwrap();
    let raw_project = format!("{}/", project_dir.display());
    let canonical_project = HostAdmissionTestRuntimeV1::canonical_project_key(&project_dir);

    runtime
        .record_savings_for_test(&raw_project, "tracedecay_context", 3000, 400, 86_400)
        .await;

    let total = runtime
        .sum_savings_for_test(Some(&canonical_project), 0)
        .await;
    assert_eq!(total.saved_tokens, 2600);
    assert_eq!(total.calls, 1);

    let history = runtime
        .savings_history_for_test(Some(&canonical_project), 0)
        .await;
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].saved_tokens, 2600);
    assert_eq!(history[0].calls, 1);
}
