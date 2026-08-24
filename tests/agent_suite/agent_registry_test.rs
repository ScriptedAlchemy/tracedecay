//! Agent registry and agent-trait tests (availability, ids, names).

use tracedecay::agents::*;

// ---------------------------------------------------------------------------
// 1. Registry tests
// ---------------------------------------------------------------------------

#[test]
fn test_available_integrations() {
    let ids = available_integrations();
    assert!(ids.contains(&"claude"));
    assert!(ids.contains(&"copilot"));
    assert!(ids.contains(&"codex"));
    assert!(ids.contains(&"gemini"));
    assert!(ids.contains(&"opencode"));
    assert!(ids.contains(&"cursor"));
    assert!(ids.contains(&"hermes"));
    assert!(ids.contains(&"zed"));
    assert!(ids.contains(&"cline"));
    assert!(ids.contains(&"roo-code"));
    assert!(ids.contains(&"antigravity"));
    assert!(ids.contains(&"kilo"));
    assert!(ids.contains(&"kiro"));
    assert!(ids.contains(&"kimi"));
    assert!(ids.contains(&"vibe"));
}

#[test]
fn test_hermes_registry_entry() {
    let ids = available_integrations();
    assert!(ids.contains(&"hermes"));

    let agent = get_integration("hermes").unwrap();
    assert_eq!(agent.id(), "hermes");
    assert_eq!(agent.name(), "Hermes");
}

#[test]
fn test_get_integration_valid() {
    for id in &[
        "claude",
        "opencode",
        "codex",
        "gemini",
        "copilot",
        "cursor",
        "hermes",
        "zed",
        "cline",
        "roo-code",
        "antigravity",
        "kilo",
        "kiro",
        "kimi",
        "vibe",
    ] {
        let agent = get_integration(id).unwrap();
        assert_eq!(agent.id(), *id);
    }
}

#[test]
fn test_get_integration_invalid() {
    assert!(get_integration("nonexistent").is_err());
    assert!(get_integration("").is_err());
    assert!(get_integration("CLAUDE").is_err()); // case-sensitive
}

// ---------------------------------------------------------------------------
// 2. Agent trait tests (name/id)
// ---------------------------------------------------------------------------

#[test]
fn test_agent_names_and_ids() {
    for agent in all_integrations() {
        assert!(!agent.name().is_empty(), "agent name should not be empty");
        assert!(!agent.id().is_empty(), "agent id should not be empty");
    }
}

#[test]
fn test_agent_names_are_human_readable() {
    // Names should have at least one space or capital letter (human-readable, not slug)
    let expected_names: Vec<(&str, &str)> = vec![
        ("claude", "Claude Code"),
        ("copilot", "GitHub Copilot"),
        ("codex", "Codex CLI"),
        ("gemini", "Gemini CLI"),
        ("hermes", "Hermes"),
        ("opencode", "OpenCode"),
        ("cursor", "Cursor"),
        ("zed", "Zed"),
        ("cline", "Cline"),
        ("roo-code", "Roo Code"),
        ("antigravity", "Antigravity"),
        ("kilo", "Kilo CLI"),
        ("kiro", "Kiro"),
        ("kimi", "Kimi CLI"),
        ("vibe", "Mistral Vibe"),
    ];
    for (id, expected_name) in expected_names {
        let agent = get_integration(id).unwrap();
        assert_eq!(agent.name(), expected_name, "name mismatch for agent {id}");
    }
}

// ---------------------------------------------------------------------------
