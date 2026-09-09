use std::error::Error as _;

use crate::AutomationError;
use crate::text::truncate_chars_for_prompt;

#[test]
fn automation_error_preserves_port_source() {
    let error = AutomationError::port(
        "agent_task_backend",
        std::io::Error::other("backend disconnected"),
    );

    assert!(matches!(
        error,
        AutomationError::Port {
            port: "agent_task_backend",
            ..
        }
    ));
    assert_eq!(
        error.source().map(ToString::to_string).as_deref(),
        Some("backend disconnected")
    );
}

#[test]
fn prompt_truncation_counts_unicode_scalars() {
    assert_eq!(truncate_chars_for_prompt("a☺bc", 2), "a☺");
    assert_eq!(truncate_chars_for_prompt("a☺bc", 4), "a☺bc");
}
