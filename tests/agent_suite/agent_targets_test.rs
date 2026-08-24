use tracedecay_agent_hosts::automation::agent_targets::{
    install_codex_managed_agents, remove_managed_agents,
};

const EXPECTED_AGENT_IDS: &[&str] = &[
    "code-explorer",
    "code-health-auditor",
    "session-historian",
    "runtime-storage-doctor",
    "cross-host-integration-auditor",
    "change-risk-reviewer",
    "usage-intelligence-analyst",
    "automation-auditor",
];

#[test]
fn codex_managed_agents_export_to_user_agents_dir() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path();

    let summary = install_codex_managed_agents(home).unwrap();

    assert_eq!(summary.exported_count, EXPECTED_AGENT_IDS.len());
    assert_eq!(summary.output, home.join(".codex/agents"));
    let exported: std::collections::BTreeSet<&str> = summary
        .exported
        .iter()
        .map(|entry| entry.id.as_str())
        .collect();
    assert_eq!(
        exported,
        EXPECTED_AGENT_IDS.iter().copied().collect(),
        "the Codex plugin lifecycle must materialize every bundled specialist"
    );
    assert!(
        home.join(".codex/agents/tracedecay-code-explorer.toml")
            .is_file()
    );
    assert!(
        std::fs::read_to_string(home.join(".codex/agents/tracedecay-code-explorer.toml"))
            .unwrap()
            .contains("name = \"tracedecay-code-explorer\"")
    );
    assert!(
        home.join(".codex/agents/.tracedecay-managed-agents.json")
            .is_file()
    );
}

#[test]
fn managed_agent_removal_uses_manifest_and_preserves_user_files() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path();
    std::fs::create_dir_all(home.join(".codex/agents")).unwrap();
    std::fs::write(home.join(".codex/agents/user-agent.toml"), "not tracedecay").unwrap();

    install_codex_managed_agents(home).unwrap();
    remove_managed_agents(&home.join(".codex/agents")).unwrap();

    assert!(
        !home
            .join(".codex/agents/tracedecay-code-explorer.toml")
            .exists()
    );
    assert!(home.join(".codex/agents/user-agent.toml").is_file());
}
