use super::{
    Commands, SilentReinstallAction, is_local_install_command,
    should_skip_agent_install_maintenance, should_skip_startup_maintenance,
    silent_reinstall_action,
};
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
        no_dashboard: false,
        automation: false,
        auto_apply: false,
    }));
    assert!(should_skip_startup_maintenance(&Commands::Reinstall));
    assert!(should_skip_startup_maintenance(&Commands::UpdatePlugin));
    assert!(should_skip_startup_maintenance(&Commands::Upgrade {
        no_heal: false,
        no_reinstall: false
    }));
    assert!(should_skip_startup_maintenance(&Commands::Update {
        no_heal: false,
        no_reinstall: false
    }));
    assert!(should_skip_startup_maintenance(&Commands::PostUpdate {
        no_heal: false,
        no_reinstall: false,
        lifecycle_lease_token: None,
        previous_daemon_state: None,
    }));
    assert!(should_skip_startup_maintenance(&Commands::Uninstall {
        agent: Some("kiro".to_string()),
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
fn tool_fallback_skips_network_and_agent_startup_maintenance() {
    let command = Commands::Tool {
        project: None,
        name: Some("message_search".to_string()),
        args: Vec::new(),
    };
    assert!(should_skip_startup_maintenance(&command));
    assert!(should_skip_agent_install_maintenance(&command));
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
        no_heal: false,
        no_reinstall: false
    }));
    assert!(should_skip_agent_install_maintenance(&Commands::Update {
        no_heal: false,
        no_reinstall: false
    }));
    assert!(should_skip_agent_install_maintenance(
        &Commands::PostUpdate {
            no_heal: false,
            no_reinstall: false,
            lifecycle_lease_token: None,
            previous_daemon_state: None,
        }
    ));
    // Also skip for uninstall (about to remove configs) and doctor (a
    // read-only diagnostic) — restoring the original #84 intent.
    assert!(should_skip_agent_install_maintenance(
        &Commands::Uninstall {
            agent: Some("cursor".to_string()),
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
fn post_update_full_reinstall_marker_advancement_suppresses_startup_reinstall() {
    // `post-update` ran the full tracked-agent install pass (see
    // `update_cmd::run_post_update_tasks`) and recorded it by advancing both
    // markers; the next ordinary command must not repeat that work via the
    // startup silent reinstall. The markers may only be advanced *after* the
    // full install pass — advancing them for a plugin-artifact-only refresh
    // would silently skip config-managed agents on minor/major bumps.
    let running = "6.1.0";
    let mut config = UserConfig {
        installed_agents: vec!["cursor".to_string()],
        previous_version: "6.0.0".to_string(),
        ..UserConfig::default()
    };

    assert!(config.mark_version_installed(running));
    assert_eq!(config.previous_version, running);
    assert_eq!(config.last_installed_version, running);
    assert_eq!(
        silent_reinstall_action(&config, running),
        SilentReinstallAction::Nothing
    );
    // Idempotent: a second post-update run has nothing left to record.
    assert!(!config.mark_version_installed(running));
}

#[test]
fn partial_reinstall_failure_leaves_startup_reinstall_pending() {
    // A partial failure in the post-update / silent reinstall pass must NOT
    // advance the version markers, so `silent_reinstall_action` still returns
    // `Reinstall` on the next startup — the self-healing retry path.
    let running = "6.1.0";
    let config = UserConfig {
        installed_agents: vec!["cursor".to_string()],
        previous_version: "6.0.0".to_string(),
        // markers deliberately left unadvanced (simulating a partial failure)
        ..UserConfig::default()
    };

    assert_eq!(
        silent_reinstall_action(&config, running),
        SilentReinstallAction::Reinstall
    );
}

#[test]
fn no_reinstall_marker_advancement_suppresses_startup_reinstall() {
    // `--no-reinstall` must be a durable opt-out for the running version, not a
    // one-command deferral: `run_post_update_tasks` advances both markers on the
    // skip path (without running the reinstall). This proves the effect — after
    // the markers advance, the next ordinary command's startup silent reinstall
    // returns `Nothing`, so it does NOT immediately undo the skip and reinstall
    // every agent anyway.
    let running = "6.1.0";
    let mut config = UserConfig {
        installed_agents: vec!["cursor".to_string()],
        previous_version: "6.0.0".to_string(),
        ..UserConfig::default()
    };

    // Pre-condition: without advancing markers the startup path WOULD reinstall.
    assert_eq!(
        silent_reinstall_action(&config, running),
        SilentReinstallAction::Reinstall
    );

    // The `--no-reinstall` path advances the markers instead of reinstalling.
    assert!(config.mark_version_installed(running));

    assert_eq!(
        silent_reinstall_action(&config, running),
        SilentReinstallAction::Nothing,
        "after --no-reinstall advances the markers, startup must not re-fire the reinstall"
    );
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
        no_dashboard: false,
        automation: false,
        auto_apply: false,
    };
    let global = Commands::Install {
        agent: Some("hermes".to_string()),
        local: false,
        no_dashboard: false,
        automation: false,
        auto_apply: false,
    };

    assert!(is_local_install_command(&local));
    assert!(!is_local_install_command(&global));
}

// These tests intentionally stay on pure parse/dispatch guard seams. Direct
// invocation of blocking or destructive run arms (serve/dashboard/upgrade,
// install mutations, status network paths, hooks that `process::exit`) is
// documented in docs/MAIN-RUN-DISPATCH-NOTE.md §5 and remains covered, where
// appropriate, by spawn-the-binary integration tests instead.
