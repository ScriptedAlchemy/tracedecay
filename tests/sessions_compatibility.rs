use tracedecay::sessions::lcm::security::should_externalize;
use tracedecay::sessions::{SessionMessageType, SessionProvider, SessionSearchScope};

#[test]
fn root_session_paths_reexport_sessions_crate_values() {
    let _: tracedecay_sessions::SessionProvider = SessionProvider::Codex;
    let _: tracedecay_sessions::SessionSearchScope = SessionSearchScope::All;
    let _: tracedecay_sessions::SessionMessageType = SessionMessageType::All;
    assert_eq!(
        should_externalize("tool", None, "data:text/plain;base64,aaaa"),
        tracedecay_sessions::lcm::security::should_externalize(
            "tool",
            None,
            "data:text/plain;base64,aaaa"
        )
    );
}
