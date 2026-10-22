use std::time::Instant;

use serde_json::Value;
use tracedecay_runtime_core::config::ProfileRoot;

use super::{TestDaemonHookActionGuard, dispatch_pi_event};
use crate::hooks::analytics::record_native_hook_invoked_parsed;
use tracedecay_domain::NativeHostIdentityV1;

/// The Pi row itself is recorded by the shared native-event handler before
/// dispatch; the replay suite asserts it through the real binary.
#[tokio::test]
async fn pi_session_boundaries_land_their_session() {
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

    for hook_name in ["session_start", "agent_end", "turn_start"] {
        let event = serde_json::json!({
            "hook_event_name": hook_name,
            "id": format!("event-{hook_name}"),
            "session_id": "pi-session-1",
            "cwd": project_root,
        })
        .to_string();
        let parsed: Value = serde_json::from_str(&event).unwrap();
        let telemetry = record_native_hook_invoked_parsed(
            &runtime,
            Some(&project_root),
            NativeHostIdentityV1::Pi,
            hook_name,
            &event,
            &parsed,
        );
        dispatch_pi_event(&runtime, &event, &project_root, &telemetry, Instant::now()).await;
    }

    let ingests = daemon
        .calls()
        .into_iter()
        .filter(|(_, args)| args["action"] == "ingest_transcript")
        .collect::<Vec<_>>();
    assert_eq!(
        ingests.len(),
        2,
        "each session boundary, and only a boundary, lands its session"
    );
    for (root, args) in &ingests {
        assert_eq!(root.as_deref(), Some(project_root.as_path()));
        assert_eq!(args["provider"], "pi");
        assert_eq!(args["user_scope"], false);
        let event: Value = serde_json::from_str(args["event_json"].as_str().unwrap()).unwrap();
        assert_eq!(event["session_id"], "pi-session-1");
    }
}
