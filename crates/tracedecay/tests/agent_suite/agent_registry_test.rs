//! Agent registry lookup rejection.

use tracedecay_agent_hosts::agents::*;

#[test]
fn test_get_integration_invalid() {
    assert!(get_integration("nonexistent").is_err());
    assert!(get_integration("").is_err());
    assert!(get_integration("CLAUDE").is_err()); // case-sensitive
}
