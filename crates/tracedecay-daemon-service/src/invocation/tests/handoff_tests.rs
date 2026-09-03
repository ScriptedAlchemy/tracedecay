//! Closed daemon-wire coverage for handoff opens.

use super::*;

#[test]
fn handoff_open_wire_is_closed_project_scoped_and_debug_redacted() {
    let secret = "handoff-open-wire-secret-0000000000000001";
    let request = DaemonInvocationRequest::handoff_application(
        "request.handoff.wire",
        HandoffApplicationInvocationV1::OpenTaskHandoff(
            tracedecay_application::OpenTaskHandoffRequestV1 {
                token: secret.to_owned(),
                session_id: tracedecay_application::HandoffSessionId::new(
                    "lsp-session.handoff.wire",
                )
                .expect("session"),
            },
        ),
        UtcMicros(10),
        Deadline::new(UtcMicros(100)).expect("deadline"),
        CancellationContext::active("cancel.handoff.wire").expect("cancellation"),
    );

    assert_eq!(
        request.operation(),
        DaemonInvocationOperation::HandoffApplication
    );
    assert!(request.requires_project());
    assert_eq!(request.validate(), Ok(()));
    assert!(!format!("{request:?}").contains(secret));

    let encoded = serde_json::to_string(&request).expect("encode daemon handoff request");
    assert!(encoded.contains(secret));
    let decoded = parse_daemon_invocation_request(&encoded)
        .expect("recognize daemon handoff protocol")
        .expect("decode daemon handoff request");
    assert_eq!(
        decoded.operation(),
        DaemonInvocationOperation::HandoffApplication
    );
}
