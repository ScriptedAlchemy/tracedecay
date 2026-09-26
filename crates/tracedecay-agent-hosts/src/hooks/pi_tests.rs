use std::path::Path;
use std::time::Instant;

use serde_json::Value;
use tracedecay_runtime_core::config::ProfileRoot;

use super::{HOOK_ANALYTICS_FILENAME, TestDaemonHookActionGuard, dispatch_pi_event};

fn read_analytics_rows(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

#[tokio::test]
async fn pi_lifecycle_events_record_under_pi_and_land_their_session() {
    let project = tempfile::tempdir().unwrap();
    let profile_dir = tempfile::tempdir().unwrap();
    let project_root = project.path().canonicalize().unwrap();
    let profile_root = profile_dir.path().canonicalize().unwrap();
    let profile = ProfileRoot::new(&profile_root);
    tracedecay_runtime_core::storage::pin_fixture_repository_identity(
        &project_root,
        "proj_pi_hook",
    )
    .unwrap();
    let layout =
        tracedecay_runtime_core::storage::resolve_layout(&project_root, &profile_root).unwrap();
    std::fs::create_dir_all(&layout.data_root).unwrap();
    let daemon = TestDaemonHookActionGuard::install([
        serde_json::json!({ "user_scope": false, "messages_upserted": 6 }),
        serde_json::json!({ "user_scope": false, "messages_upserted": 1 }),
    ]);
    let runtime = crate::ports::hook_runtime::crate_test_runtime(profile.clone());

    for hook_name in ["session_start", "agent_end"] {
        let event = serde_json::json!({
            "hook_event_name": hook_name,
            "id": format!("event-{hook_name}"),
            "session_id": "pi-session-1",
            "cwd": project_root,
        })
        .to_string();
        dispatch_pi_event(&runtime, &event, &project_root, Instant::now()).await;
    }

    let rows = read_analytics_rows(&layout.data_root.join(HOOK_ANALYTICS_FILENAME));
    for hook_name in ["session_start", "agent_end"] {
        assert!(
            rows.iter().any(|row| row["event"] == "hook_invoked"
                && row["agent"] == "pi"
                && row["hook_name"] == hook_name),
            "missing Pi {hook_name} invocation: {rows:?}"
        );
    }
    assert!(
        rows.iter()
            .all(|row| row["agent"] != "other" && row["hook_name"] != "pi_event"),
        "Pi hooks must not fold into the shared other host: {rows:?}"
    );

    let ingests = daemon
        .calls()
        .into_iter()
        .filter(|(_, args)| args["action"] == "ingest_transcript")
        .collect::<Vec<_>>();
    assert_eq!(ingests.len(), 2, "each session boundary lands its session");
    for (root, args) in &ingests {
        assert_eq!(root.as_deref(), Some(project_root.as_path()));
        assert_eq!(args["provider"], "pi");
        assert_eq!(args["user_scope"], false);
        let event: Value = serde_json::from_str(args["event_json"].as_str().unwrap()).unwrap();
        assert_eq!(event["session_id"], "pi-session-1");
    }
}
