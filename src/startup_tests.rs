use super::{
    AnalyticsAction, CommandFamily, Commands, DAEMON_CPU_THREADS_ENV,
    DEFAULT_MAX_DAEMON_CPU_THREADS, DaemonAction, HostBundleCliOptions, MAX_ASYNC_WORKER_THREADS,
    MAX_BLOCKING_THREADS, PackageHookAction, PostUpdateMode, RAYON_NUM_THREADS_ENV,
    ScoopPackageHookAction, SilentReinstallAction, StderrTracingDefault, async_worker_threads,
    daemon_cpu_threads_from, is_daemon_run, is_extract_worker, is_local_install_command,
    should_skip_agent_install_maintenance, should_skip_startup_maintenance,
    silent_reinstall_action, stderr_tracing_default, validate_host_bundle_options,
};
use std::path::PathBuf;
use tracedecay::user_config::UserConfig;

/// `wipe` destroys deployed state, so it takes the same `--yes` acceptance as
/// the lifecycle mutations. Without it a scripted wipe had to feed `go!`
/// through a pipe on stdin, and `--yes` was rejected outright.
#[test]
fn wipe_accepts_the_global_confirmation_flag() {
    let command = Commands::Wipe { all: true };
    let options = HostBundleCliOptions {
        component: None,
        dry_run: false,
        yes: true,
        adopt: false,
    };
    validate_host_bundle_options(&command, CommandFamily::for_command(&command), &options)
        .expect("wipe --yes must be accepted");
}

/// `wipe` owns no host component and has no preview, so the other two global
/// lifecycle flags stay rejected on it.
#[test]
fn wipe_still_rejects_component_and_dry_run() {
    let command = Commands::Wipe { all: false };
    let family = CommandFamily::for_command(&command);
    let dry_run = HostBundleCliOptions {
        component: None,
        dry_run: true,
        yes: false,
        adopt: false,
    };
    assert!(validate_host_bundle_options(&command, family, &dry_run).is_err());
}

/// The confirmation flag must not leak onto the other project commands, which
/// have nothing to confirm.
#[test]
fn sibling_project_commands_still_reject_the_confirmation_flag() {
    let command = Commands::List { all: false };
    let options = HostBundleCliOptions {
        component: None,
        dry_run: false,
        yes: true,
        adopt: false,
    };
    assert!(
        validate_host_bundle_options(&command, CommandFamily::for_command(&command), &options)
            .is_err()
    );
}

#[test]
fn async_runtime_bounds_parallel_allocators() {
    assert!((1..=MAX_ASYNC_WORKER_THREADS).contains(&async_worker_threads()));
    assert_eq!(MAX_ASYNC_WORKER_THREADS, 16);
    assert_eq!(MAX_BLOCKING_THREADS, 32);
}

#[test]
fn daemon_cpu_pool_is_bounded_by_default_and_operator_tunable() {
    assert_eq!(
        daemon_cpu_threads_from(96, None).unwrap(),
        DEFAULT_MAX_DAEMON_CPU_THREADS
    );
    assert_eq!(daemon_cpu_threads_from(8, None).unwrap(), 8);
    assert_eq!(
        daemon_cpu_threads_from(96, Some((DAEMON_CPU_THREADS_ENV, "32"))).unwrap(),
        32
    );
    assert_eq!(
        daemon_cpu_threads_from(96, Some((RAYON_NUM_THREADS_ENV, "24"))).unwrap(),
        24
    );
    assert_eq!(
        daemon_cpu_threads_from(96, Some((RAYON_NUM_THREADS_ENV, "0"))).unwrap(),
        DEFAULT_MAX_DAEMON_CPU_THREADS
    );
    assert_eq!(
        daemon_cpu_threads_from(96, Some((RAYON_NUM_THREADS_ENV, "invalid"))).unwrap(),
        DEFAULT_MAX_DAEMON_CPU_THREADS
    );
    assert!(
        daemon_cpu_threads_from(96, Some((DAEMON_CPU_THREADS_ENV, "0")))
            .unwrap_err()
            .contains(DAEMON_CPU_THREADS_ENV)
    );
}

#[test]
fn only_foreground_daemon_installs_the_global_cpu_pool() {
    let daemon = Commands::Daemon {
        action: DaemonAction::Run {
            socket: None,
            profile_root: None,
        },
    };
    assert!(is_daemon_run(Some(&daemon)));
    assert!(!is_daemon_run(Some(&Commands::Monitor)));
    assert!(!is_daemon_run(None));
}

#[test]
fn extraction_workers_bypass_the_async_runtime() {
    assert!(is_extract_worker(Some(&Commands::ExtractWorker)));
    assert!(!is_extract_worker(None));
}

#[test]
fn representative_commands_route_to_their_dispatch_family() {
    let cases = [
        (
            Commands::Init {
                path: None,
                skip_folders: Vec::new(),
                include_folders: Vec::new(),
            },
            CommandFamily::Project,
        ),
        (
            Commands::Tool {
                project: None,
                name: Some("status".to_string()),
                args: Vec::new(),
            },
            CommandFamily::Runtime,
        ),
        (
            Commands::Reinstall {
                local: false,
                agent: None,
            },
            CommandFamily::Agent,
        ),
        (Commands::HookStop, CommandFamily::Hook),
        (Commands::Dogfood, CommandFamily::Update),
        (
            Commands::PackageHook {
                action: PackageHookAction::Scoop {
                    action: ScoopPackageHookAction::Prepare {
                        package_id: "tracedecay".to_string(),
                        state_file: PathBuf::from("scoop-state.json"),
                    },
                },
            },
            CommandFamily::Update,
        ),
        (Commands::DisableUploadCounter, CommandFamily::Configuration),
        (Commands::Monitor, CommandFamily::Diagnostics),
        (
            Commands::Analytics {
                action: AnalyticsAction::Sync,
            },
            CommandFamily::Knowledge,
        ),
    ];

    for (command, expected) in cases {
        assert_eq!(CommandFamily::for_command(&command), expected);
    }
}

#[test]
fn hook_commands_default_to_a_silent_stderr_subscriber() {
    for command in [
        Commands::HookStop,
        Commands::HookCursorPostToolUse,
        Commands::HookCodexSessionStart,
    ] {
        assert_eq!(
            stderr_tracing_default(Some(&command)),
            StderrTracingDefault::Silent,
            "hook stderr belongs to the host and must stay quiet by default"
        );
    }
}

#[test]
fn non_hook_commands_keep_warnings_on_stderr() {
    assert_eq!(
        stderr_tracing_default(Some(&Commands::Monitor)),
        StderrTracingDefault::Warn
    );
    assert_eq!(stderr_tracing_default(None), StderrTracingDefault::Warn);
}

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
    assert!(should_skip_startup_maintenance(&Commands::Reinstall {
        local: false,
        agent: None,
    }));
    assert!(should_skip_startup_maintenance(&Commands::UpdatePlugin {
        local: false,
        agent: None,
    }));
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
        strict: false,
        mode: PostUpdateMode::Normal,
    }));
    assert!(should_skip_startup_maintenance(&Commands::PackageHook {
        action: PackageHookAction::Scoop {
            action: ScoopPackageHookAction::Restore {
                package_id: "tracedecay-beta".to_string(),
                state_file: PathBuf::from("scoop-state.json"),
            },
        },
    }));
    assert!(should_skip_startup_maintenance(&Commands::Uninstall {
        agent: Some("kiro".to_string()),
        local: false,
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
    assert!(should_skip_agent_install_maintenance(
        &Commands::Reinstall {
            local: false,
            agent: None,
        }
    ));
    // `update-plugin` promises byte-identical configs; the implicit
    // silent-reinstall prelude would rewrite them.
    assert!(should_skip_agent_install_maintenance(
        &Commands::UpdatePlugin {
            local: false,
            agent: None,
        }
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
            strict: false,
            mode: PostUpdateMode::Normal,
        }
    ));
    // Also skip for uninstall (about to remove configs) and doctor (a
    // read-only diagnostic) — restoring the original #84 intent.
    assert!(should_skip_agent_install_maintenance(
        &Commands::Uninstall {
            agent: Some("cursor".to_string()),
            local: false,
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
fn numeric_beta_prerelease_bump_only_advances_the_marker() {
    // beta.2 → beta.10 is newer, but same minor — no agent reinstall.
    let config = UserConfig {
        installed_agents: vec!["cursor".to_string()],
        previous_version: "0.0.18-beta.2".to_string(),
        last_installed_version: "0.0.18-beta.2".to_string(),
        ..UserConfig::default()
    };

    assert_eq!(
        silent_reinstall_action(&config, "0.0.18-beta.10"),
        SilentReinstallAction::AdvanceMarker
    );
}

#[test]
fn beta_minor_bump_triggers_reinstall_and_cross_channel_stays_quiet() {
    // 1.2.x → 1.3.x is a SemVer minor bump on the same beta channel.
    let beta_minor = UserConfig {
        installed_agents: vec!["cursor".to_string()],
        previous_version: "1.2.3-beta.2".to_string(),
        last_installed_version: "1.2.3-beta.2".to_string(),
        ..UserConfig::default()
    };
    assert_eq!(
        silent_reinstall_action(&beta_minor, "1.3.0-beta.1"),
        SilentReinstallAction::Reinstall
    );

    // Stable ↔ beta never counts as a minor transition for silent reinstall.
    let cross_channel = UserConfig {
        installed_agents: vec!["cursor".to_string()],
        previous_version: "1.2.3".to_string(),
        last_installed_version: "1.2.3".to_string(),
        ..UserConfig::default()
    };
    assert_eq!(
        silent_reinstall_action(&cross_channel, "1.3.0-beta.1"),
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
