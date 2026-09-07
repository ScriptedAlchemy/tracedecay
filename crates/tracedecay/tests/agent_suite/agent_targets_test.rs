use tracedecay_automation_runtime::automation::agent_targets::{
    install_codex_managed_agents, remove_managed_agents,
};

/// The production host I/O bundle the Codex installer hands `agent_targets`.
fn host_io() -> tracedecay_automation_runtime::automation::host_io::HostIo {
    tracedecay_agent_hosts::host_io()
}

#[test]
fn codex_managed_agents_export_to_user_agents_dir() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path();

    let summary = install_codex_managed_agents(&host_io(), home).unwrap();

    let canonical = host_io().codex_agent_files();
    assert!(!canonical.is_empty());
    assert_eq!(summary.exported_count, canonical.len());
    assert_eq!(summary.output, home.join(".codex/agents"));
    let exported: std::collections::BTreeSet<&str> = summary
        .exported
        .iter()
        .map(|entry| entry.id.as_str())
        .collect();
    assert_eq!(
        exported,
        canonical
            .iter()
            .map(|file| file
                .relative
                .strip_prefix("tracedecay-")
                .unwrap()
                .strip_suffix(".toml")
                .unwrap())
            .collect(),
        "the Codex plugin lifecycle must materialize every bundled specialist"
    );
    for file in canonical {
        let installed = home.join(".codex/agents").join(file.relative);
        assert_eq!(std::fs::read_to_string(&installed).unwrap(), file.contents);
        let document: toml::Value = toml::from_str(file.contents).unwrap();
        assert_eq!(
            document["name"].as_str(),
            file.relative.strip_suffix(".toml")
        );
    }
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

    let installed = install_codex_managed_agents(&host_io(), home).unwrap();
    remove_managed_agents(&host_io(), &home.join(".codex/agents")).unwrap();

    for entry in installed.exported {
        assert!(
            !entry.path.exists(),
            "{} must be removed",
            entry.path.display()
        );
    }
    assert!(home.join(".codex/agents/user-agent.toml").is_file());
}
