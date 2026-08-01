//! Misc agent tests: generated-guidance content, healthchecks, helper
//! functions, detection, constants, and small utility behaviors.

use std::path::Path;

use crate::agent_test_support::*;
use tempfile::TempDir;
use tracedecay::agents::*;

#[test]
fn generated_guidance_prefers_resolved_active_project_store() {
    // The 16 model-invocable skills are now shared byte-for-byte across hosts
    // in `plugin/skills/`, so cursor and codex read the same source file.
    let shared_status = include_str!("../../plugin/skills/code-health/SKILL.md");
    let cursor_rule = include_str!("../../plugin/rules/tracedecay.mdc");

    for (name, guidance) in [
        ("cursor code-health", shared_status),
        ("codex code-health", shared_status),
    ] {
        assert!(
            guidance.contains("tracedecay_active_project"),
            "{name} should steer project identity checks through the active-project tool"
        );
        assert!(
            guidance.contains("tracedecay_storage_status"),
            "{name} should steer store checks through the storage-status tool"
        );
        assert!(
            guidance.contains("resolved active project store"),
            "{name} should describe resolved storage instead of repo-local DB probing"
        );
        assert!(
            !guidance.contains(".tracedecay/tracedecay.db"),
            "{name} must not tell agents to inspect the repo-local graph DB directly"
        );
    }

    assert!(
        cursor_rule.contains("resolved active project"),
        "the always-on Cursor rule should describe resolved active-project routing"
    );
    assert!(
        !cursor_rule.contains("when `.tracedecay/` exists"),
        "the always-on Cursor rule must not gate MCP usage on a repo-local marker"
    );
}

#[test]
fn generated_plugin_skill_descriptions_are_yaml_quoted() {
    // The shared skill set — one `plugin/skills/` tree for every host. Cursor's
    // workflow slugs are native commands now, not skills.
    for root in ["plugin/skills"] {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join(root);
        for entry in std::fs::read_dir(&root).unwrap() {
            let skill_path = entry.unwrap().path().join("SKILL.md");
            if !skill_path.is_file() {
                continue;
            }

            let skill = std::fs::read_to_string(&skill_path).unwrap();
            let description = skill
                .lines()
                .find(|line| line.starts_with("description: "))
                .unwrap_or_else(|| panic!("{} has no description", skill_path.display()));
            let value = description.strip_prefix("description: ").unwrap();
            assert!(
                valid_single_quoted_yaml_scalar(value),
                "{} description must be a valid single-quoted YAML scalar",
                skill_path.display()
            );
        }
    }
}

#[test]
fn generated_prompt_rules_do_not_hardcode_repo_local_graph_db() {
    // Hosts that render their own rule text, plus the shared renderer that
    // copilot/gemini/opencode/kimi/vibe delegate to.
    for (name, source) in [
        (
            "claude",
            include_str!("../../crates/tracedecay-agent-hosts/src/agents/claude.rs"),
        ),
        (
            "kiro",
            include_str!("../../crates/tracedecay-agent-hosts/src/agents/kiro.rs"),
        ),
        (
            "prompt_rules",
            include_str!("../../crates/tracedecay-agent-hosts/src/agents/prompt_rules.rs"),
        ),
    ] {
        assert!(
            !source.contains(".tracedecay/tracedecay.db"),
            "{name} generated guidance must not hardcode the repo-local graph DB path"
        );
        assert!(
            source.contains("tracedecay_active_project")
                && source.contains("tracedecay_storage_status"),
            "{name} generated guidance should point store questions to active-project/storage-status tools"
        );
    }
    for (name, source) in [
        (
            "copilot",
            include_str!("../../crates/tracedecay-agent-hosts/src/agents/copilot.rs"),
        ),
        (
            "gemini",
            include_str!("../../crates/tracedecay-agent-hosts/src/agents/gemini.rs"),
        ),
        (
            "kimi",
            include_str!("../../crates/tracedecay-agent-hosts/src/agents/kimi.rs"),
        ),
        (
            "opencode",
            include_str!("../../crates/tracedecay-agent-hosts/src/agents/opencode.rs"),
        ),
        (
            "vibe",
            include_str!("../../crates/tracedecay-agent-hosts/src/agents/vibe.rs"),
        ),
    ] {
        assert!(
            !source.contains(".tracedecay/tracedecay.db"),
            "{name} generated guidance must not hardcode the repo-local graph DB path"
        );
        assert!(
            source.contains("standard_prompt_rules"),
            "{name} should delegate prompt rules to the shared renderer"
        );
    }
}

#[test]
fn profile_storage_docs_do_not_overclaim_unimplemented_bundle_or_quota_support() {
    let docs = include_str!("../../docs/PROFILE-STORAGE-SUPPORT.md");
    assert!(
        !docs.contains("tracedecay support bundle --redact"),
        "profile-storage docs must not present support-bundle behavior as implemented"
    );
    assert!(
        !docs.contains("quota status"),
        "profile-storage docs must not claim quota status is emitted before quota support exists"
    );
}

#[test]
fn hermes_dashboard_wrapper_docs_reject_profile_project_defaults() {
    let wrapper = include_str!("../../dashboard/hermes-wrapper/plugin_api.py");
    assert!(
        wrapper.contains("Hermes homes and profiles never"),
        "Hermes dashboard docs should state the single-profile storage contract"
    );
    assert!(
        !wrapper.contains("configured `plugins.tracedecay.project_root` pin"),
        "Hermes dashboard docs must not advertise legacy project pins"
    );
}

// 5. Healthcheck with tempdir
// ---------------------------------------------------------------------------

#[test]
fn test_healthcheck_claude_clean_install() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);
    let ctx = make_install_ctx_with_real_bin(home);
    ClaudeIntegration.install(&ctx).unwrap();

    let mut dc = DoctorCounters::new();
    let hctx = HealthcheckContext {
        home: home.to_path_buf(),
        project_path: home.to_path_buf(),
    };
    ClaudeIntegration.healthcheck(&mut dc, &hctx);
    assert_eq!(dc.issues, 0, "clean Claude install should have no issues");
}

#[test]
fn test_healthcheck_gemini_clean_install() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);
    let ctx = make_install_ctx(home);
    GeminiIntegration.install(&ctx).unwrap();

    let mut dc = DoctorCounters::new();
    let hctx = HealthcheckContext {
        home: home.to_path_buf(),
        project_path: home.to_path_buf(),
    };
    GeminiIntegration.healthcheck(&mut dc, &hctx);
    assert_eq!(dc.issues, 0, "clean Gemini install should have no issues");
}

#[test]
fn test_healthcheck_opencode_clean_install() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);
    let ctx = make_install_ctx(home);
    OpenCodeIntegration.install(&ctx).unwrap();

    let mut dc = DoctorCounters::new();
    let hctx = HealthcheckContext {
        home: home.to_path_buf(),
        project_path: home.to_path_buf(),
    };
    OpenCodeIntegration.healthcheck(&mut dc, &hctx);
    assert_eq!(dc.issues, 0, "clean OpenCode install should have no issues");
}

#[test]
fn test_healthcheck_no_install_warns() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();

    // Healthcheck without installing should produce warnings (not crashes)
    let mut dc = DoctorCounters::new();
    let hctx = HealthcheckContext {
        home: home.to_path_buf(),
        project_path: home.to_path_buf(),
    };
    ClaudeIntegration.healthcheck(&mut dc, &hctx);
    // Should have issues (missing config files)
    assert!(
        dc.issues > 0 || dc.warnings > 0,
        "healthcheck on empty dir should report issues or warnings"
    );
}

#[test]
fn test_doctor_counters() {
    let mut dc = DoctorCounters::new();
    assert_eq!(dc.issues, 0);
    assert_eq!(dc.warnings, 0);

    dc.pass("this is fine");
    assert_eq!(dc.issues, 0);
    assert_eq!(dc.warnings, 0);

    dc.fail("something broke");
    assert_eq!(dc.issues, 1);
    assert_eq!(dc.warnings, 0);

    dc.warn("be careful");
    assert_eq!(dc.issues, 1);
    assert_eq!(dc.warnings, 1);

    dc.info("just info");
    assert_eq!(dc.issues, 1);
    assert_eq!(dc.warnings, 1);

    dc.fail("another failure");
    assert_eq!(dc.issues, 2);
    assert_eq!(dc.warnings, 1);
}

// ---------------------------------------------------------------------------
// 6. Helper function tests
// ---------------------------------------------------------------------------

#[test]
fn test_load_json_file_missing() {
    let val = load_json_file(Path::new("/nonexistent/file.json"));
    assert!(val.is_object());
    assert!(val.as_object().unwrap().is_empty());
}

#[test]
fn test_load_json_file_valid() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("test.json");
    std::fs::write(&path, r#"{"key": "value"}"#).unwrap();
    let val = load_json_file(&path);
    assert_eq!(val["key"], "value");
}

#[test]
fn test_load_json_file_invalid_returns_empty() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("bad.json");
    std::fs::write(&path, "not valid json").unwrap();
    let val = load_json_file(&path);
    assert!(val.is_object());
    assert!(val.as_object().unwrap().is_empty());
}

#[test]
fn test_load_json_file_strict_missing() {
    let result = load_json_file_strict(Path::new("/nonexistent/file.json"));
    assert!(result.is_ok());
    let val = result.unwrap();
    assert!(val.is_object());
    assert!(val.as_object().unwrap().is_empty());
}

#[test]
fn test_load_json_file_strict_empty_file() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("empty.json");
    std::fs::write(&path, "").unwrap();
    let result = load_json_file_strict(&path);
    assert!(result.is_ok());
    let val = result.unwrap();
    assert!(val.as_object().unwrap().is_empty());
}

#[test]
fn test_load_json_file_strict_whitespace_only() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("ws.json");
    std::fs::write(&path, "   \n  \t  ").unwrap();
    let result = load_json_file_strict(&path);
    assert!(result.is_ok());
}

#[test]
fn test_load_json_file_strict_invalid() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("bad.json");
    std::fs::write(&path, "not valid json").unwrap();
    assert!(load_json_file_strict(&path).is_err());
}

#[test]
fn test_load_json_file_strict_valid() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("good.json");
    std::fs::write(&path, r#"{"hello": "world"}"#).unwrap();
    let val = load_json_file_strict(&path).unwrap();
    assert_eq!(val["hello"], "world");
}

#[test]
fn test_backup_config_file() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("config.json");
    std::fs::write(&path, r#"{"original": true}"#).unwrap();
    let backup = backup_config_file(&path).unwrap();
    assert!(backup.is_some());
    let backup_path = backup.unwrap();
    assert!(backup_path.exists());
    // Verify backup content matches original
    let backup_content = std::fs::read_to_string(&backup_path).unwrap();
    assert_eq!(backup_content, r#"{"original": true}"#);
}

#[test]
fn test_backup_config_file_missing() {
    let result = backup_config_file(Path::new("/nonexistent/file.json")).unwrap();
    assert!(result.is_none());
}

#[test]
fn test_safe_write_json_file() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("output.json");
    let value = serde_json::json!({"hello": "world"});
    safe_write_json_file(&path, &value, None).unwrap();
    let content: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(content["hello"], "world");
}

#[test]
fn test_safe_write_json_file_creates_parent_dirs() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("deep/nested/dir/output.json");
    let value = serde_json::json!({"nested": true});
    safe_write_json_file(&path, &value, None).unwrap();
    assert!(path.exists());
}

#[test]
fn test_safe_write_json_file_overwrites_existing() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("existing.json");
    std::fs::write(&path, r#"{"old": true}"#).unwrap();
    let value = serde_json::json!({"new": true});
    safe_write_json_file(&path, &value, None).unwrap();
    let content: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(content["new"], true);
    assert!(content.get("old").is_none());
}

#[test]
fn test_write_json_file() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("write_test.json");
    let value = serde_json::json!({"test": 42});
    write_json_file(&path, &value).unwrap();
    let content: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(content["test"], 42);
}

#[test]
fn test_load_toml_file_missing() {
    let val = load_toml_file(Path::new("/nonexistent/file.toml")).unwrap();
    assert!(val.is_table());
    assert!(val.as_table().unwrap().is_empty());
}

#[test]
fn test_load_toml_file_valid() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("test.toml");
    std::fs::write(&path, "key = \"value\"\nnumber = 42\n").unwrap();
    let val = load_toml_file(&path).expect("valid TOML should parse as document");
    let table = val.as_table().expect("top-level should be a table");
    assert_eq!(table.get("key").and_then(|v| v.as_str()), Some("value"));
    assert_eq!(table.get("number").and_then(|v| v.as_integer()), Some(42));
}

#[test]
fn test_load_toml_file_invalid_returns_err() {
    // Bug #63: invalid TOML used to silently return an empty table, which let
    // install_mcp_server wipe out the user's config. Now it must surface an
    // error so the caller refuses to overwrite.
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("bad.toml");
    std::fs::write(&path, "{{{{not valid toml").unwrap();
    assert!(
        load_toml_file(&path).is_err(),
        "unparseable TOML must surface as error, not silently empty"
    );
}

#[test]
fn test_load_toml_file_empty_file_returns_empty_table() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("empty.toml");
    std::fs::write(&path, "").unwrap();
    let val = load_toml_file(&path).expect("empty file should be treated as empty table");
    assert!(val.as_table().unwrap().is_empty());
}

#[test]
fn test_write_toml_file() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("output.toml");
    let mut table = toml::map::Map::new();
    table.insert("key".to_string(), toml::Value::String("value".to_string()));
    let val = toml::Value::Table(table);
    write_toml_file(&path, &val).unwrap();
    assert!(path.exists());
    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.contains("key"));
    assert!(content.contains("value"));
}

#[test]
fn test_write_toml_file_backs_up_existing() {
    // Issue #63: overwriting an existing config must always leave a .bak copy
    // so the user can recover if anything goes wrong.
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("config.toml");
    let original = "preserved = \"keep me\"\n";
    std::fs::write(&path, original).unwrap();

    let mut table = toml::map::Map::new();
    table.insert(
        "new".to_string(),
        toml::Value::String("content".to_string()),
    );
    write_toml_file(&path, &toml::Value::Table(table)).unwrap();

    let backup = dir.path().join("config.toml.bak");
    assert!(
        backup.exists(),
        "write must create a .bak of the prior file"
    );
    assert_eq!(
        std::fs::read_to_string(&backup).unwrap(),
        original,
        "the backup must contain the exact previous bytes"
    );
}

#[test]
fn test_write_toml_file_no_backup_when_no_prior_file() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("fresh.toml");
    let mut table = toml::map::Map::new();
    table.insert("k".to_string(), toml::Value::String("v".to_string()));
    write_toml_file(&path, &toml::Value::Table(table)).unwrap();

    let backup = dir.path().join("fresh.toml.bak");
    assert!(
        !backup.exists(),
        "no backup should be created on first write"
    );
}

// ---------------------------------------------------------------------------
// JSONC helpers
// ---------------------------------------------------------------------------

#[test]
fn test_load_jsonc_file_missing() {
    let val = load_jsonc_file(Path::new("/nonexistent/file.jsonc"));
    assert!(val.is_object());
    assert!(val.as_object().unwrap().is_empty());
}

#[test]
fn test_load_jsonc_file_with_comments() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("test.jsonc");
    std::fs::write(
        &path,
        r#"{
        // This is a comment
        "key": "value", // trailing comment
        /* block comment */
        "number": 42,
    }"#,
    )
    .unwrap();
    let val = load_jsonc_file(&path);
    assert_eq!(val["key"], "value");
    assert_eq!(val["number"], 42);
}

#[test]
fn test_load_jsonc_file_strict_missing() {
    let result = load_jsonc_file_strict(Path::new("/nonexistent/file.jsonc"));
    assert!(result.is_ok());
}

#[test]
fn test_load_jsonc_file_strict_with_comments() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("test.jsonc");
    std::fs::write(
        &path,
        r#"{
        // comment
        "key": "value"
    }"#,
    )
    .unwrap();
    let val = load_jsonc_file_strict(&path).unwrap();
    assert_eq!(val["key"], "value");
}

#[test]
fn test_parse_jsonc() {
    let input = r#"{
        // line comment
        "a": 1,
        /* block */ "b": 2,
    }"#;
    let val = parse_jsonc(input);
    assert_eq!(val["a"], 1);
    assert_eq!(val["b"], 2);
}

// ---------------------------------------------------------------------------
// 7. is_detected / has_tracedecay tests
// ---------------------------------------------------------------------------

#[test]
fn test_is_detected_claude() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);
    assert!(!ClaudeIntegration.is_detected(home));
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    assert!(ClaudeIntegration.is_detected(home));
}

#[test]
fn test_is_detected_codex() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    assert!(!CodexIntegration.is_detected(home));
    std::fs::create_dir_all(home.join(".codex")).unwrap();
    assert!(CodexIntegration.is_detected(home));
}

#[test]
fn test_is_detected_gemini() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    assert!(!GeminiIntegration.is_detected(home));
    std::fs::create_dir_all(home.join(".gemini")).unwrap();
    assert!(GeminiIntegration.is_detected(home));
}

#[test]
fn test_is_detected_cursor() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    assert!(!CursorIntegration.is_detected(home));
    std::fs::create_dir_all(home.join(".cursor")).unwrap();
    assert!(CursorIntegration.is_detected(home));
}

#[test]
fn test_is_detected_opencode() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    assert!(!OpenCodeIntegration.is_detected(home));
    std::fs::create_dir_all(home.join(".config/opencode")).unwrap();
    assert!(OpenCodeIntegration.is_detected(home));
}

#[test]
fn test_is_detected_zed() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    assert!(!ZedIntegration.is_detected(home));
    #[cfg(target_os = "macos")]
    std::fs::create_dir_all(home.join("Library/Application Support/Zed")).unwrap();
    #[cfg(not(target_os = "macos"))]
    std::fs::create_dir_all(home.join(".config/zed")).unwrap();
    assert!(ZedIntegration.is_detected(home));
}

#[test]
fn test_is_detected_copilot() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    // Copilot is detected when either VS Code User dir or .copilot dir exists
    assert!(!CopilotIntegration.is_detected(home));
    std::fs::create_dir_all(home.join(".copilot")).unwrap();
    assert!(CopilotIntegration.is_detected(home));
}

#[test]
fn test_has_tracedecay_claude() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);
    // No config => false
    assert!(!ClaudeIntegration.has_tracedecay(home));

    // After install => true
    let ctx = make_install_ctx(home);
    ClaudeIntegration.install(&ctx).unwrap();
    assert!(ClaudeIntegration.has_tracedecay(home));

    // After uninstall => false
    ClaudeIntegration.uninstall(&ctx).unwrap();
    assert!(!ClaudeIntegration.has_tracedecay(home));
}

#[test]
fn test_has_tracedecay_gemini() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);
    assert!(!GeminiIntegration.has_tracedecay(home));

    let ctx = make_install_ctx(home);
    GeminiIntegration.install(&ctx).unwrap();
    assert!(GeminiIntegration.has_tracedecay(home));
}

#[test]
fn test_has_tracedecay_codex() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);
    assert!(!CodexIntegration.has_tracedecay(home));

    let ctx = make_install_ctx(home);
    CodexIntegration.install(&ctx).unwrap();
    assert!(
        codex_plugin_install_dir(home)
            .join(".codex-plugin/plugin.json")
            .exists()
    );
    assert!(
        CodexIntegration.has_tracedecay(home),
        "has_tracedecay should detect tracedecay after a clean install"
    );
}

#[test]
fn test_has_tracedecay_cursor() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);
    assert!(!CursorIntegration.has_tracedecay(home));

    let ctx = make_install_ctx(home);
    CursorIntegration.install(&ctx).unwrap();
    assert!(CursorIntegration.has_tracedecay(home));
}

#[test]
fn test_has_tracedecay_opencode() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);
    assert!(!OpenCodeIntegration.has_tracedecay(home));

    let ctx = make_install_ctx(home);
    OpenCodeIntegration.install(&ctx).unwrap();
    assert!(OpenCodeIntegration.has_tracedecay(home));
}

#[test]
fn test_has_tracedecay_copilot() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);
    assert!(!CopilotIntegration.has_tracedecay(home));

    let ctx = make_install_ctx(home);
    CopilotIntegration.install(&ctx).unwrap();
    assert!(CopilotIntegration.has_tracedecay(home));
}

// ---------------------------------------------------------------------------
// 10. Constants sanity
// ---------------------------------------------------------------------------

#[test]
fn test_tool_names_not_empty() {
    let names = tool_names();
    assert!(!names.is_empty());
    for name in &names {
        assert!(
            name.starts_with("tracedecay_"),
            "tool name should start with tracedecay_: {name}"
        );
    }
}

#[test]
fn test_read_only_tool_names_excludes_mutating_tools() {
    let read_only = read_only_tool_names();
    let read_only_set: std::collections::HashSet<&str> =
        read_only.iter().map(String::as_str).collect();
    let known_tools: std::collections::HashSet<String> = tool_names().into_iter().collect();
    assert!(!read_only.is_empty());

    for name in &read_only {
        assert!(
            known_tools.contains(name),
            "read-only tool should be a known MCP tool: {name}"
        );
    }

    for mutating in [
        "tracedecay_str_replace",
        "tracedecay_multi_str_replace",
        "tracedecay_insert_at",
        "tracedecay_ast_grep_rewrite",
        "tracedecay_replace_symbol",
        "tracedecay_insert_at_symbol",
        "tracedecay_move_symbol",
        "tracedecay_run_affected_tests",
        "tracedecay_session_start",
        "tracedecay_session_end",
        "tracedecay_fact_store",
        "tracedecay_fact_feedback",
    ] {
        assert!(
            !read_only_set.contains(mutating),
            "mutating tool should not be read-only: {mutating}"
        );
    }
}

#[test]
fn test_expected_tool_perms_not_empty() {
    let perms = expected_tool_perms();
    assert!(!perms.is_empty());
    for perm in &perms {
        assert!(
            perm.starts_with("mcp__tracedecay__"),
            "tool perm should start with mcp__tracedecay__: {perm}"
        );
    }
}

#[test]
fn test_tool_perms_match_tool_names() {
    let names = tool_names();
    let perms = expected_tool_perms();
    assert_eq!(
        names.len(),
        perms.len(),
        "tool_names and expected_tool_perms should have same length"
    );
    for name in &names {
        let expected_perm = format!("mcp__tracedecay__{name}");
        assert!(
            perms.contains(&expected_perm),
            "missing permission for tool {name}: expected {expected_perm}"
        );
    }
}

// ---------------------------------------------------------------------------
// 11. restore_config_backup
// ---------------------------------------------------------------------------

#[test]
fn test_restore_config_backup_restores_content() {
    let dir = TempDir::new().unwrap();
    let original_path = dir.path().join("config.json");
    let backup_path = dir.path().join("config.json.bak");

    // Create original and backup
    std::fs::write(&original_path, r#"{"version": 1}"#).unwrap();
    std::fs::write(&backup_path, r#"{"version": 1}"#).unwrap();

    // Corrupt the original
    std::fs::write(&original_path, "CORRUPTED").unwrap();

    // Restore from backup
    restore_config_backup(&original_path, &backup_path);

    let restored = std::fs::read_to_string(&original_path).unwrap();
    assert_eq!(
        restored, r#"{"version": 1}"#,
        "restored content should match the backup"
    );
}

#[test]
fn test_restore_config_backup_to_missing_original() {
    let dir = TempDir::new().unwrap();
    let original_path = dir.path().join("config.json");
    let backup_path = dir.path().join("config.json.bak");

    // Only create backup, not original
    std::fs::write(&backup_path, r#"{"saved": true}"#).unwrap();

    restore_config_backup(&original_path, &backup_path);

    assert!(
        original_path.exists(),
        "original should be created from backup"
    );
    let content = std::fs::read_to_string(&original_path).unwrap();
    assert_eq!(content, r#"{"saved": true}"#);
}

#[test]
fn test_restore_config_backup_missing_backup_does_not_panic() {
    let dir = TempDir::new().unwrap();
    let original_path = dir.path().join("config.json");
    let backup_path = dir.path().join("config.json.bak");

    std::fs::write(&original_path, "original").unwrap();

    // Restore with a nonexistent backup — should not panic
    restore_config_backup(&original_path, &backup_path);

    // Original should remain untouched since backup failed
    let content = std::fs::read_to_string(&original_path).unwrap();
    assert_eq!(content, "original");
}

// ---------------------------------------------------------------------------
// 12. which_tracedecay
// ---------------------------------------------------------------------------

#[test]
fn test_which_tracedecay_returns_some_or_none() {
    // which_tracedecay checks current_exe and PATH — we just verify it
    // doesn't panic and returns a sensible result.
    let result = which_tracedecay();
    // In a test environment, the current exe is the test runner, not tracedecay,
    // so it may return None (unless tracedecay is on PATH). Either way, no panic.
    if let Some(ref path) = result {
        assert!(!path.is_empty(), "path should not be empty if Some");
    }
    // Test passes regardless of Some or None — just ensures no panic.
}

// ---------------------------------------------------------------------------
// 13. home_dir
// ---------------------------------------------------------------------------

#[test]
fn test_home_dir_returns_some() {
    let result = home_dir();
    assert!(
        result.is_some(),
        "home_dir should return Some on most systems"
    );
    let home = result.unwrap();
    assert!(home.is_absolute(), "home dir should be an absolute path");
}

// ---------------------------------------------------------------------------
// 14. migrate_installed_agents
// ---------------------------------------------------------------------------

#[test]
fn test_migrate_installed_agents_skips_when_already_populated() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let mut config = tracedecay::user_config::UserConfig {
        installed_agents: vec!["claude".to_string()],
        ..Default::default()
    };

    // Should return immediately since installed_agents is non-empty
    migrate_installed_agents(home, &mut config);

    // The existing list should be unchanged
    assert_eq!(config.installed_agents, vec!["claude".to_string()]);
}

#[test]
fn test_migrate_installed_agents_detects_installed_agents() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let _agent_env = crate::common::AgentEnvLock::pin(home);

    // Install copilot so it can be detected
    let ctx = make_install_ctx(home);
    CopilotIntegration.install(&ctx).unwrap();

    let mut config = tracedecay::user_config::UserConfig::default();
    assert!(config.installed_agents.is_empty());

    // migrate will scan and detect copilot is installed
    // Note: save() will try to write to ~/.tracedecay/config.toml which may fail
    // in CI, but the function still populates installed_agents in memory.
    migrate_installed_agents(home, &mut config);

    assert!(
        config.installed_agents.contains(&"copilot".to_string()),
        "copilot should be detected, got: {:?}",
        config.installed_agents
    );
}

#[test]
fn test_migrate_installed_agents_empty_home_no_change() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let mut config = tracedecay::user_config::UserConfig::default();

    migrate_installed_agents(home, &mut config);

    // No agents installed in empty home, list should remain empty
    assert!(
        config.installed_agents.is_empty(),
        "installed_agents should remain empty when no agents detected"
    );
}

// ---------------------------------------------------------------------------
// 15. pick_integrations_interactive (no-agent-detected error path)
// ---------------------------------------------------------------------------

#[test]
fn test_pick_integrations_interactive_no_agents_detected() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();

    // Empty home — no agents detected
    let result = pick_integrations_interactive(home, &[]);
    assert!(
        result.is_err(),
        "pick_integrations_interactive should error when no agents detected"
    );

    let err_msg = format!("{}", result.unwrap_err());
    assert!(
        err_msg.contains("No supported agents detected"),
        "error should mention no agents detected, got: {err_msg}"
    );
}

#[test]
fn test_pick_integrations_interactive_single_uninstalled_agent() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();

    // Create only the .copilot dir so exactly one agent is detected
    std::fs::create_dir_all(home.join(".copilot")).unwrap();

    // Single detected agent that is NOT installed => fast path returns it directly
    let result = pick_integrations_interactive(home, &[]);
    assert!(
        result.is_ok(),
        "should succeed with single uninstalled agent"
    );
    let (to_install, to_uninstall) = result.unwrap();
    assert_eq!(to_install, vec!["copilot".to_string()]);
    assert!(to_uninstall.is_empty());
}

// ---------------------------------------------------------------------------
// 16. vscode_data_dir / copilot_cli_dir
// ---------------------------------------------------------------------------

#[test]
fn test_vscode_data_dir_is_under_home() {
    let home = Path::new("/fake/home");
    let dir = tracedecay::agents::vscode_data_dir(home);
    assert!(
        dir.starts_with("/fake/home"),
        "vscode_data_dir should be under home: {}",
        dir.display()
    );
}

#[test]
fn test_copilot_cli_dir_is_under_home() {
    let home = Path::new("/fake/home");
    let dir = tracedecay::agents::copilot_cli_dir(home);
    assert_eq!(
        dir,
        Path::new("/fake/home/.copilot"),
        "copilot_cli_dir should be home/.copilot"
    );
}

// ---------------------------------------------------------------------------
// 17. parse_jsonc edge cases
// ---------------------------------------------------------------------------

#[test]
fn test_parse_jsonc_empty_string() {
    let val = parse_jsonc("");
    assert!(val.is_object());
    assert!(val.as_object().unwrap().is_empty());
}

#[test]
fn test_parse_jsonc_only_comments() {
    let input = "// just a comment\n/* block */\n";
    let val = parse_jsonc(input);
    assert!(val.is_object());
    assert!(val.as_object().unwrap().is_empty());
}

#[test]
fn test_parse_jsonc_nested_comments() {
    let input = r#"{
        "a": "hello // not a comment",
        /* this is a real comment */
        "b": true
    }"#;
    let val = parse_jsonc(input);
    assert_eq!(val["a"].as_str().unwrap(), "hello // not a comment");
    assert_eq!(val["b"], true);
}

#[test]
fn test_parse_jsonc_trailing_comma_in_object() {
    let input = r#"{"a": 1, "b": 2,}"#;
    let val = parse_jsonc(input);
    assert_eq!(val["a"], 1);
    assert_eq!(val["b"], 2);
}

#[test]
fn test_parse_jsonc_trailing_comma_in_array() {
    let input = r#"{"arr": [1, 2, 3,]}"#;
    let val = parse_jsonc(input);
    let arr = val["arr"].as_array().unwrap();
    assert_eq!(arr.len(), 3);
}

// ---------------------------------------------------------------------------
// 18. backup + safe_write round-trip
// ---------------------------------------------------------------------------

#[test]
fn test_backup_and_safe_write_round_trip() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("roundtrip.json");

    // Create initial file
    let initial = serde_json::json!({"name": "tracedecay", "version": 1});
    safe_write_json_file(&path, &initial, None).unwrap();

    // Create backup
    let backup = backup_config_file(&path).unwrap();
    assert!(backup.is_some());
    let backup_path = backup.unwrap();

    // Overwrite with new content
    let updated = serde_json::json!({"name": "tracedecay", "version": 2});
    safe_write_json_file(&path, &updated, Some(&backup_path)).unwrap();

    // Verify new content
    let content: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(content["version"], 2);

    // Verify backup still has old content
    let backup_content: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&backup_path).unwrap()).unwrap();
    assert_eq!(backup_content["version"], 1);

    // Restore from backup
    restore_config_backup(&path, &backup_path);
    let restored: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(restored["version"], 1);
}
