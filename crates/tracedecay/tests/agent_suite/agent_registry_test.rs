//! Agent registry lookup rejection.

use tracedecay_agent_hosts::agents::*;
use tracedecay_domain::errors::TraceDecayError;

#[test]
fn test_get_integration_invalid() {
    assert_eq!(get_integration("claude").unwrap().id(), "claude");
    // Lookup is case-sensitive.
    for id in ["nonexistent", "", "CLAUDE"] {
        let Err(error) = get_integration(id) else {
            panic!("{id:?} must not resolve to an integration");
        };
        let TraceDecayError::Config { message } = &error else {
            panic!("unexpected refusal kind for {id:?}: {error:?}");
        };
        assert!(
            message.starts_with(&format!("unknown agent: \"{id}\". Available agents: "))
                && message.contains("claude"),
            "unexpected refusal for {id:?}: {message}"
        );
    }
}
