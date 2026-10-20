use super::*;

const SESSION_ID: &str = "codex-pinned-session";
const SHIPPED_V2_SOURCE: &str = r#"{"provider":"codex","session_id":"codex-pinned-session","source_key":"sha256:1a03b304c22f4282bb1ce885d69e3c8f2c0e53d1a81320ca4b8df677234d216b"}"#;

fn source(rollout: &str) -> ObservationSourceIdentityV1 {
    CodexRolloutFile::of(Path::new(rollout), SESSION_ID)
        .source(SESSION_ID)
        .unwrap()
}

#[test]
fn single_rollout_sessions_keep_the_shipped_v2_source_bytes() {
    for rollout in [
        "/home/.codex/sessions/2026/01/01/rollout-2026-01-01T00-00-00-codex-pinned-session.jsonl",
        "/home/.codex/archived_sessions/rollout-2026-01-01T00-00-00-codex-pinned-session.jsonl",
        "/tmp/rollout.jsonl",
        "/tmp/rollout-2026-01-01T00-00-00-another-thread_codex-pinned-session.jsonl",
        "/tmp/rollout-2026-01-01T00-00-00-codex-pinned-session_codex-pinned-session.jsonl",
        "/tmp/rollout-2026-01-01T00-00-00-codex-pinned-session_.jsonl",
    ] {
        assert_eq!(
            serde_json::to_string(&source(rollout)).unwrap(),
            SHIPPED_V2_SOURCE,
            "{rollout}"
        );
    }
    assert_eq!(
        serde_json::to_string(&codex_observation_source_v2(SESSION_ID).unwrap()).unwrap(),
        SHIPPED_V2_SOURCE
    );
}

#[test]
fn each_revert_rollout_of_a_thread_is_its_own_source() {
    let original = source("/s/rollout-2026-01-01T00-00-00-codex-pinned-session.jsonl");
    let first_revert =
        source("/s/rollout-2026-01-02T00-00-00-codex-pinned-session_rollout-a.jsonl");
    let second_revert =
        source("/s/rollout-2026-01-03T00-00-00-codex-pinned-session_rollout-b.jsonl");
    let archived_first_revert =
        source("/a/rollout-2026-01-02T00-00-00-codex-pinned-session_rollout-a.jsonl");

    assert_ne!(first_revert, original);
    assert_ne!(second_revert, original);
    assert_ne!(first_revert, second_revert);
    assert_eq!(first_revert, archived_first_revert);
    assert_eq!(first_revert.session_id().as_str(), SESSION_ID);
}
