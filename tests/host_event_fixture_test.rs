use serde_json::{Value, json};
use tracedecay::application::host_admission::{
    HostAdmissionAuthorities, HostAdmissionFacade, HostAdmissionOutcome, HostAdmissionScope,
};
use tracedecay::hooks::{codex_additional_context_json, cursor_session_start_json};

const FIXTURES: [(&str, &str); 5] = [
    (
        "codex",
        include_str!("fixtures/host_events/codex/baseline.json"),
    ),
    (
        "claude",
        include_str!("fixtures/host_events/claude/baseline.json"),
    ),
    (
        "cursor",
        include_str!("fixtures/host_events/cursor/baseline.json"),
    ),
    (
        "hermes",
        include_str!("fixtures/host_events/hermes/baseline.json"),
    ),
    (
        "kiro",
        include_str!("fixtures/host_events/kiro/baseline.json"),
    ),
];

#[test]
fn native_host_event_fixtures_cover_legal_redacted_outcomes() {
    let unavailable = HostAdmissionFacade::new(HostAdmissionAuthorities::default());

    for (provider, fixture) in FIXTURES {
        let document: Value = serde_json::from_str(fixture).expect("valid host fixture JSON");
        assert_eq!(document["schema_version"], 1, "{provider}");
        assert_eq!(document["provider"], provider, "{provider}");

        let cases = document["cases"].as_array().expect("fixture cases");
        assert_eq!(cases.len(), 4, "{provider}");
        let mut states = Vec::new();

        for case in cases {
            let state = case["state"].as_str().expect("state");
            states.push(state);
            assert!(case["request"].is_object(), "{provider}/{state}");
            assert_redacted(provider, state, case);

            let actual = &case["admission"];
            match state {
                "supported" => assert_eq!(
                    actual,
                    &serde_json::to_value(HostAdmissionOutcome::supported()).unwrap(),
                    "{provider}/{state}"
                ),
                "unavailable" => assert_eq!(
                    actual,
                    &serde_json::to_value(unavailable.probe(provider, HostAdmissionScope::Project))
                        .unwrap(),
                    "{provider}/{state}"
                ),
                "unknown" => assert_eq!(
                    actual,
                    &serde_json::to_value(
                        unavailable.probe(
                            case["admission_provider"]
                                .as_str()
                                .expect("unknown provider"),
                            HostAdmissionScope::Project,
                        )
                    )
                    .unwrap(),
                    "{provider}/{state}"
                ),
                "degraded" => assert_eq!(
                    actual,
                    &json!({
                        "status": "degraded",
                        "retryable": false,
                        "reason_code": "malformed_event",
                    }),
                    "{provider}/{state}"
                ),
                other => panic!("unexpected fixture state {other}"),
            }

            assert_legal_host_response(provider, state, &case["response"]);
        }

        states.sort_unstable();
        assert_eq!(
            states,
            ["degraded", "supported", "unavailable", "unknown"],
            "{provider}"
        );
    }
}

fn assert_legal_host_response(provider: &str, state: &str, response: &Value) {
    assert_eq!(response["exit_code"], 0, "{provider}/{state}");
    assert_eq!(response["stderr"], "", "{provider}/{state}");
    let stdout = response["stdout"].as_str().expect("stdout string");
    let expected = match provider {
        "codex" | "claude" => codex_additional_context_json("SessionStart", "<REDACTED_CONTEXT>"),
        "cursor" => cursor_session_start_json(None, "<REDACTED_CONTEXT>"),
        "hermes" | "kiro" => String::new(),
        other => panic!("unexpected provider {other}"),
    };
    if expected.is_empty() {
        assert_eq!(stdout, expected, "{provider}/{state}");
    } else {
        let actual: Value = serde_json::from_str(stdout).expect("legal JSON stdout");
        let expected: Value = serde_json::from_str(&expected).unwrap();
        assert_eq!(actual, expected, "{provider}/{state}");
    }
}

fn assert_redacted(provider: &str, state: &str, case: &Value) {
    let encoded = serde_json::to_string(case).unwrap();
    for forbidden in [
        "/home/",
        "C:\\\\Users\\",
        "api_key",
        "access_token",
        "secret",
        "hostname",
    ] {
        assert!(
            !encoded
                .to_ascii_lowercase()
                .contains(&forbidden.to_ascii_lowercase()),
            "{provider}/{state} contains forbidden data: {forbidden}"
        );
    }
}
