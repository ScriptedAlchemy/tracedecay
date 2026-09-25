//! Generated Cursor agent bundle contract.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;

fn assert_no_violations(rule_family: &str, violations: &[String]) {
    assert!(
        violations.is_empty(),
        "shared skill contract ({rule_family}) found {} violation(s):\n{}",
        violations.len(),
        violations.join("\n")
    );
}

#[test]
fn generated_cursor_agents_are_present_and_clean() {
    let agents: BTreeMap<&str, &str> =
        tracedecay_agent_hosts::agents::plugin_bundle::cursor_files()
            .into_iter()
            .filter(|(path, _)| path.starts_with("agents/"))
            .collect();
    for expected in [
        "automation-auditor.md",
        "change-risk-reviewer.md",
        "code-explorer.md",
        "code-health-auditor.md",
        "cross-host-integration-auditor.md",
        "runtime-storage-doctor.md",
        "session-historian.md",
        "usage-intelligence-analyst.md",
    ] {
        assert!(
            agents.contains_key(format!("agents/{expected}").as_str()),
            "generated Cursor agents missing {expected}"
        );
    }
    let mut violations = Vec::new();
    for (file, raw) in agents {
        if raw.contains('\r') || !raw.ends_with('\n') {
            violations.push(format!("{}: line-ending hygiene", file));
        }
        if file == "agents/session-historian.md" && raw.contains("after_store_id") {
            violations.push(format!(
                "{file}: must teach only opaque session continuation cursors"
            ));
        }
    }
    assert_no_violations("generated cursor agents", &violations);
}
