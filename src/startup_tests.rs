use super::{
    agent_cmd::{
        hermes_profile_targets, hermes_selected_profile_targets, validate_hermes_profile_flags,
        validate_hermes_project_root_flag,
    },
    is_local_install_command, should_skip_agent_install_maintenance,
    should_skip_startup_maintenance, silent_reinstall_action, update_cmd, Commands,
    SilentReinstallAction,
};
use tempfile::TempDir;
use tracedecay::user_config::UserConfig;

#[test]
fn doctor_skips_startup_maintenance() {
    let command = Commands::Doctor {
        agent: Some("kiro".to_string()),
    };
    assert!(should_skip_startup_maintenance(&command));
}

#[test]
fn explicit_agent_config_commands_skip_startup_maintenance() {
    assert!(should_skip_startup_maintenance(&Commands::Install {
        agent: Some("kiro".to_string()),
        local: false,
        profile: None,
        all_profiles: false,
        project_root: None,
        no_dashboard: false,
        automation: false,
        auto_apply: false,
    }));
    assert!(should_skip_startup_maintenance(&Commands::Reinstall));
    assert!(should_skip_startup_maintenance(&Commands::UpdatePlugin));
    assert!(should_skip_startup_maintenance(&Commands::Upgrade {
        no_heal: false
    }));
    assert!(should_skip_startup_maintenance(&Commands::Update {
        no_heal: false
    }));
    assert!(should_skip_startup_maintenance(&Commands::PostUpdate {
        no_heal: false
    }));
    assert!(should_skip_startup_maintenance(&Commands::Uninstall {
        agent: Some("kiro".to_string()),
        profile: None,
        all_profiles: false,
    }));
}

#[test]
fn normal_commands_keep_startup_maintenance() {
    assert!(!should_skip_startup_maintenance(&Commands::Status {
        path: None,
        project_id: None,
        project_path: None,
        json: false,
        short: false,
        details: false,
        runtime: false,
    }));
}

#[test]
fn agent_install_maintenance_is_selective() {
    // Skip the implicit reinstall scan on the hot path (`serve`), on the
    // explicit install commands (they already install), and on per-call
    // tool invocations.
    assert!(should_skip_agent_install_maintenance(&Commands::Serve {
        path: None,
        timings: false,
    }));
    assert!(should_skip_agent_install_maintenance(&Commands::Install {
        agent: Some("cursor".to_string()),
        local: false,
        profile: None,
        all_profiles: false,
        project_root: None,
        no_dashboard: false,
        automation: false,
        auto_apply: false,
    }));
    assert!(should_skip_agent_install_maintenance(&Commands::Reinstall));
    // `update-plugin` promises byte-identical configs; the implicit
    // silent-reinstall prelude would rewrite them.
    assert!(should_skip_agent_install_maintenance(
        &Commands::UpdatePlugin
    ));
    assert!(should_skip_agent_install_maintenance(&Commands::Upgrade {
        no_heal: false
    }));
    assert!(should_skip_agent_install_maintenance(&Commands::Update {
        no_heal: false
    }));
    assert!(should_skip_agent_install_maintenance(
        &Commands::PostUpdate { no_heal: false }
    ));
    assert!(should_skip_agent_install_maintenance(&Commands::Tool {
        project: None,
        name: Some("message_search".to_string()),
        args: Vec::new(),
    }));

    // Also skip for uninstall (about to remove configs) and doctor (a
    // read-only diagnostic) — restoring the original #84 intent.
    assert!(should_skip_agent_install_maintenance(
        &Commands::Uninstall {
            agent: Some("cursor".to_string()),
            profile: None,
            all_profiles: false,
        }
    ));
    assert!(should_skip_agent_install_maintenance(&Commands::Doctor {
        agent: Some("cursor".to_string()),
    }));

    // Run maintenance for normal everyday command invocations so a binary
    // upgrade re-syncs agent config.
    assert!(!should_skip_agent_install_maintenance(&Commands::Init {
        path: None,
        skip_folders: Vec::new(),
        include_folders: Vec::new(),
    }));
    assert!(!should_skip_agent_install_maintenance(&Commands::Status {
        path: None,
        project_id: None,
        project_path: None,
        json: false,
        short: false,
        details: false,
        runtime: false,
    }));
}

#[test]
fn silent_reinstall_runs_after_minor_bump_without_post_update() {
    // An upgraded binary whose `post-update` never ran (or predates the
    // marker advancement) still triggers the reinstall pass.
    let config = UserConfig {
        installed_agents: vec!["cursor".to_string()],
        previous_version: "6.0.0".to_string(),
        ..UserConfig::default()
    };

    assert_eq!(
        silent_reinstall_action(&config, "6.1.0"),
        SilentReinstallAction::Reinstall
    );
}

#[test]
fn post_update_marker_advancement_prevents_duplicate_silent_reinstall() {
    // `post-update` refreshed the plugins and advanced the markers; the next
    // ordinary command must not repeat that work via silent reinstall.
    let running = "6.1.0";
    let mut config = UserConfig {
        installed_agents: vec!["cursor".to_string()],
        previous_version: "6.0.0".to_string(),
        ..UserConfig::default()
    };

    assert!(update_cmd::mark_running_version_installed(
        &mut config,
        running
    ));
    assert_eq!(config.previous_version, running);
    assert_eq!(config.last_installed_version, running);
    assert_eq!(
        silent_reinstall_action(&config, running),
        SilentReinstallAction::Nothing
    );
    // Idempotent: a second post-update run has nothing left to record.
    assert!(!update_cmd::mark_running_version_installed(
        &mut config,
        running
    ));
}

#[test]
fn patch_bump_only_advances_the_marker() {
    let config = UserConfig {
        installed_agents: vec!["cursor".to_string()],
        previous_version: "6.1.0".to_string(),
        last_installed_version: "6.1.0".to_string(),
        ..UserConfig::default()
    };

    assert_eq!(
        silent_reinstall_action(&config, "6.1.1"),
        SilentReinstallAction::AdvanceMarker
    );
}

#[test]
fn serve_skips_startup_maintenance() {
    // `tracedecay serve` is the MCP hot path with a 30 s client-side
    // `initialize` timeout (#84). Pre-serve maintenance work
    // (worldwide-counter flush, install-stale check, silent reinstall)
    // must NOT run on this path.
    assert!(should_skip_startup_maintenance(&Commands::Serve {
        path: None,
        timings: false,
    }));
}

#[test]
fn claude_and_kiro_hooks_skip_startup_maintenance() {
    // Claude and Kiro lifecycle hooks are agent-invoked hot-path
    // commands, exactly like the Cursor/Codex hooks already in the
    // skip-list. They must skip the synchronous `try_flush` network
    // round-trip (and the rest of the pre-command startup maintenance)
    // so they stay fast on every tool-use/prompt/stop event (#84).
    assert!(should_skip_startup_maintenance(&Commands::HookPreToolUse));
    assert!(should_skip_startup_maintenance(&Commands::HookPromptSubmit));
    assert!(should_skip_startup_maintenance(&Commands::HookStop));
    assert!(should_skip_startup_maintenance(
        &Commands::HookKiroPreToolUse
    ));
    assert!(should_skip_startup_maintenance(
        &Commands::HookKiroPromptSubmit
    ));
    assert!(should_skip_startup_maintenance(
        &Commands::HookKiroPostToolUse
    ));
}

#[test]
fn local_install_detection_tracks_dispatch_preamble_behavior() {
    let local = Commands::Install {
        agent: Some("hermes".to_string()),
        local: true,
        profile: Some("dev".to_string()),
        all_profiles: false,
        project_root: None,
        no_dashboard: false,
        automation: false,
        auto_apply: false,
    };
    let global = Commands::Install {
        agent: Some("hermes".to_string()),
        local: false,
        profile: Some("dev".to_string()),
        all_profiles: false,
        project_root: None,
        no_dashboard: false,
        automation: false,
        auto_apply: false,
    };

    assert!(is_local_install_command(&local));
    assert!(!is_local_install_command(&global));
}

#[test]
fn hermes_profile_flags_are_restricted_to_hermes() {
    let profile = Some("dev".to_string());
    let none_profile: Option<String> = None;

    let profile_err = validate_hermes_profile_flags(Some("cursor"), &profile, false)
        .expect_err("non-hermes --profile should fail");
    assert!(
        format!("{profile_err}").contains("`--profile` is only supported with `--agent hermes`")
    );

    let all_profiles_err = validate_hermes_profile_flags(Some("cursor"), &none_profile, true)
        .expect_err("non-hermes --all-profiles should fail");
    assert!(format!("{all_profiles_err}")
        .contains("`--all-profiles` is only supported with `--agent hermes`"));

    assert!(validate_hermes_profile_flags(Some("hermes"), &profile, false).is_ok());
    assert!(validate_hermes_profile_flags(Some("hermes"), &none_profile, true).is_ok());
}

#[test]
fn hermes_project_root_flag_requires_hermes_and_absolute_paths() {
    let temp = TempDir::new().expect("tempdir should exist");
    let absolute = temp.path().join("project-root");
    let absolute_str = absolute.to_string_lossy().to_string();
    let absolute_flag = Some(absolute_str.clone());

    let agent_err = validate_hermes_project_root_flag(Some("cursor"), &absolute_flag)
        .expect_err("non-hermes project-root should fail");
    assert!(
        format!("{agent_err}").contains("`--project-root` is only supported with `--agent hermes`")
    );

    let relative_flag = Some("relative/project".to_string());
    let relative_err = validate_hermes_project_root_flag(Some("hermes"), &relative_flag)
        .expect_err("relative project-root should fail");
    assert!(format!("{relative_err}").contains("`--project-root` must be an absolute path"));

    assert_eq!(
        validate_hermes_project_root_flag(Some("hermes"), &absolute_flag)
            .expect("absolute hermes project-root should pass"),
        Some(absolute)
    );
}

#[test]
fn hermes_profile_target_helpers_preserve_default_and_sorted_profiles() {
    let temp = TempDir::new().expect("tempdir should exist");
    let profiles_dir = temp.path().join(".hermes/profiles");
    std::fs::create_dir_all(profiles_dir.join("zeta")).expect("zeta profile dir");
    std::fs::create_dir_all(profiles_dir.join("alpha")).expect("alpha profile dir");
    std::fs::write(profiles_dir.join("README.txt"), "not a profile").expect("profile marker file");

    let all_targets = hermes_profile_targets(temp.path()).expect("profile discovery should work");
    assert_eq!(
        all_targets,
        vec![None, Some("alpha".to_string()), Some("zeta".to_string()),]
    );

    let selected = hermes_selected_profile_targets(temp.path(), &Some("dev".to_string()), false)
        .expect("explicit profile selection should not scan disk");
    assert_eq!(selected, vec![Some("dev".to_string())]);

    let selected_all = hermes_selected_profile_targets(temp.path(), &None, true)
        .expect("all-profiles selection should enumerate profiles");
    assert_eq!(selected_all, all_targets);
}

// These tests intentionally stay on pure parse/dispatch guard seams. Direct
// invocation of blocking or destructive run arms (serve/dashboard/upgrade,
// install mutations, status network paths, hooks that `process::exit`) is
// documented in docs/MAIN-RUN-DISPATCH-NOTE.md §5 and remains covered, where
// appropriate, by spawn-the-binary integration tests instead.
