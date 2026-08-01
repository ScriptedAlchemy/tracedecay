use super::{
    AutomationAction, AutomationConfigAction, AutomationConfigScope, AutomationRunAction,
    AutomationRunsAction, AutomationSkillsAction, AutomationSkillsInstallTarget, BranchAction, Cli,
    Commands, DaemonAction, FeedbackRollbackAction, HostBundleAction, LspAction, MemoryAction,
    MigrateAction, PostUpdateMode, SessionsAction, SessionsRefreshAction,
};
use clap::{Command, CommandFactory, Parser, error::ErrorKind};

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| value.to_string()).collect()
}

fn visible_subcommand_paths(command: &Command) -> Vec<Vec<String>> {
    fn collect(command: &Command, prefix: Vec<String>, paths: &mut Vec<Vec<String>>) {
        for subcommand in command.get_subcommands().filter(|sub| !sub.is_hide_set()) {
            let mut path = prefix.clone();
            path.push(subcommand.get_name().to_string());
            paths.push(path.clone());
            collect(subcommand, path, paths);
        }
    }

    let mut paths = Vec::new();
    collect(command, Vec::new(), &mut paths);
    paths
}

#[test]
fn visible_subcommands_accept_clap_help() {
    let command = Cli::command();
    for path in visible_subcommand_paths(&command) {
        if path == ["tool"] {
            continue;
        }

        let args = std::iter::once("tracedecay".to_string())
            .chain(path.iter().cloned())
            .chain(std::iter::once("--help".to_string()));
        let err = match Cli::try_parse_from(args) {
            Ok(_) => panic!(
                "`tracedecay {} --help` should short-circuit parsing",
                path.join(" ")
            ),
            Err(err) => err,
        };
        assert_eq!(
            err.kind(),
            ErrorKind::DisplayHelp,
            "`tracedecay {} --help` should display help",
            path.join(" ")
        );
    }
}

#[test]
fn every_visible_top_level_subcommand_ships_rich_help() {
    let command = Cli::command();
    for subcommand in command
        .get_subcommands()
        .filter(|sub| !sub.is_hide_set() && sub.get_name() != "help")
    {
        let name = subcommand.get_name();
        let long_about = subcommand
            .get_long_about()
            .map(ToString::to_string)
            .unwrap_or_default();
        assert!(
            long_about.len() >= 80,
            "`tracedecay {name}` needs a long_about paragraph (what it does and \
             when to use it); got {} chars",
            long_about.len()
        );
        let after_help = subcommand
            .get_after_help()
            .map(ToString::to_string)
            .unwrap_or_default();
        assert!(
            after_help.contains("Examples:"),
            "`tracedecay {name}` after_help must contain an `Examples:` section"
        );
        assert!(
            after_help.contains("tracedecay "),
            "`tracedecay {name}` examples must show real `tracedecay` invocations"
        );
        assert!(
            after_help.contains("Related:") || after_help.contains("Notes:"),
            "`tracedecay {name}` after_help must cross-reference related commands \
             or carry agent-relevant notes"
        );
    }
}

/// `dogfood` is how a source checkout reaches the live user environment
/// without cutting a release, so it must stay discoverable in `--help`. The
/// machine-invoked plumbing around it stays hidden.
#[test]
fn dogfood_is_discoverable_while_plumbing_stays_hidden() {
    let command = Cli::command();
    let visible: Vec<String> = command
        .get_subcommands()
        .filter(|sub| !sub.is_hide_set())
        .map(|sub| sub.get_name().to_string())
        .collect();
    assert!(
        visible.iter().any(|name| name == "dogfood"),
        "`dogfood` must appear in top-level help; visible: {visible:?}"
    );
    for hidden in ["post-update", "extract-worker", "hook-stop"] {
        assert!(
            !visible.iter().any(|name| name == hidden),
            "`{hidden}` is machine-invoked plumbing and must stay hidden"
        );
    }
}

#[test]
fn every_visible_nested_subcommand_has_a_purpose_line() {
    let command = Cli::command();
    for path in visible_subcommand_paths(&command) {
        if path.len() < 2 {
            continue;
        }
        let mut current = &command;
        for name in &path {
            current = current
                .find_subcommand(name)
                .unwrap_or_else(|| panic!("subcommand path {path:?} should resolve"));
        }
        let about = current
            .get_about()
            .map(ToString::to_string)
            .unwrap_or_default();
        assert!(
            about.trim().len() >= 10,
            "`tracedecay {}` needs a descriptive purpose line; got {about:?}",
            path.join(" ")
        );
    }
}

#[test]
fn top_level_help_teaches_the_tool_discovery_flow() {
    let command = Cli::command();
    let after_help = command
        .get_after_help()
        .map(ToString::to_string)
        .unwrap_or_default();
    for needle in [
        "tracedecay tool",
        "--help",
        "--args",
        "--json",
        "Quick start:",
    ] {
        assert!(
            after_help.contains(needle),
            "top-level after_help must teach the MCP tool discovery flow; missing {needle:?}"
        );
    }
}

#[test]
fn tool_command_preserves_trailing_help_and_reserved_args() {
    let cli = Cli::try_parse_from([
        "tracedecay",
        "tool",
        "--project",
        "/tmp/project",
        "search",
        "--help",
        "--json",
        "--args",
        r#"{"query":"foo"}"#,
        "@payload.json",
    ])
    .expect("tool command should parse");

    assert!(matches!(
        cli.command,
        Some(Commands::Tool { project, name, args })
            if project.as_deref() == Some("/tmp/project")
                && name.as_deref() == Some("search")
                && args
                    == vec![
                        "--help".to_string(),
                        "--json".to_string(),
                        "--args".to_string(),
                        r#"{"query":"foo"}"#.to_string(),
                        "@payload.json".to_string(),
                    ]
    ));
}

#[test]
fn claude_install_alias_dispatches_to_install_command() {
    let cli = Cli::try_parse_from([
        "tracedecay",
        "claude-install",
        "--agent",
        "hermes",
        "--no-dashboard",
    ])
    .expect("install alias should parse");

    assert!(matches!(
        cli.command,
        Some(Commands::Install {
            agent,
            local,
            no_dashboard,
            ..
        }) if agent.as_deref() == Some("hermes")
            && !local
            && no_dashboard
    ));
}

#[test]
fn removed_hermes_install_selectors_are_unknown_arguments() {
    for args in [
        vec![
            "tracedecay",
            "install",
            "--agent",
            "hermes",
            "--profile",
            "dev",
        ],
        vec![
            "tracedecay",
            "install",
            "--agent",
            "hermes",
            "--all-profiles",
        ],
        vec![
            "tracedecay",
            "install",
            "--agent",
            "hermes",
            "--project-root",
            "/tmp/project",
        ],
        vec![
            "tracedecay",
            "uninstall",
            "--agent",
            "hermes",
            "--profile",
            "dev",
        ],
        vec![
            "tracedecay",
            "uninstall",
            "--agent",
            "hermes",
            "--all-profiles",
        ],
    ] {
        let error = match Cli::try_parse_from(args.clone()) {
            Ok(_) => panic!("removed flag must fail: {args:?}"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), ErrorKind::UnknownArgument, "args: {args:?}");
    }
}

#[test]
fn update_plugins_alias_dispatches_to_update_plugin_command() {
    let cli = Cli::try_parse_from(["tracedecay", "update-plugins"])
        .expect("update-plugin alias should parse");

    assert!(matches!(
        cli.command,
        Some(Commands::UpdatePlugin {
            local: false,
            agent: None
        })
    ));
}

#[test]
fn update_upgrade_and_update_plugin_parse_to_distinct_commands() {
    let update = Cli::try_parse_from(["tracedecay", "update"]).expect("update should parse");
    let upgrade = Cli::try_parse_from(["tracedecay", "upgrade"]).expect("upgrade should parse");
    let update_plugin =
        Cli::try_parse_from(["tracedecay", "update-plugin"]).expect("update-plugin should parse");

    assert!(matches!(
        update.command,
        Some(Commands::Update {
            no_heal: false,
            no_reinstall: false
        })
    ));
    assert!(matches!(
        upgrade.command,
        Some(Commands::Upgrade {
            no_heal: false,
            no_reinstall: false
        })
    ));
    assert!(matches!(
        update_plugin.command,
        Some(Commands::UpdatePlugin {
            local: false,
            agent: None
        })
    ));
}

#[test]
fn project_local_lifecycle_commands_require_and_preserve_agent_scope() {
    let reinstall =
        Cli::try_parse_from(["tracedecay", "reinstall", "--local", "--agent", "opencode"]).unwrap();
    assert!(matches!(
        reinstall.command,
        Some(Commands::Reinstall {
            local: true,
            agent: Some(ref agent)
        }) if agent == "opencode"
    ));
    let update =
        Cli::try_parse_from(["tracedecay", "update-plugin", "--local", "--agent", "kimi"]).unwrap();
    assert!(matches!(
        update.command,
        Some(Commands::UpdatePlugin {
            local: true,
            agent: Some(ref agent)
        }) if agent == "kimi"
    ));
    let uninstall =
        Cli::try_parse_from(["tracedecay", "uninstall", "--local", "--agent", "roo-code"]).unwrap();
    assert!(matches!(
        uninstall.command,
        Some(Commands::Uninstall {
            local: true,
            agent: Some(ref agent)
        }) if agent == "roo-code"
    ));
    assert!(Cli::try_parse_from(["tracedecay", "reinstall", "--local"]).is_err());
}

#[test]
fn feedback_rollback_commands_parse_confirmation_and_state_paths() {
    let dry_run = Cli::try_parse_from([
        "tracedecay",
        "feedback-rollback",
        "dry-run",
        "--agent",
        "kimi",
    ])
    .unwrap();
    assert!(matches!(
        dry_run.command,
        Some(Commands::FeedbackRollback {
            action: FeedbackRollbackAction::DryRun { ref agent }
        }) if agent == "kimi"
    ));
    let apply = Cli::try_parse_from([
        "tracedecay",
        "feedback-rollback",
        "apply",
        "--agent",
        "opencode",
        "--state",
        ".tracedecay/feedback-opencode.json",
        "--yes",
    ])
    .unwrap();
    assert!(matches!(
        apply.command,
        Some(Commands::FeedbackRollback {
            action: FeedbackRollbackAction::Apply {
                ref agent,
                ref state,
                yes: true
            }
        }) if agent == "opencode" && state == ".tracedecay/feedback-opencode.json"
    ));
    assert!(
        Cli::try_parse_from([
            "tracedecay",
            "feedback-rollback",
            "restore",
            "--state",
            "state.json"
        ])
        .is_ok(),
        "confirmation is enforced by the handler so dry parsing remains inspectable"
    );
}

#[test]
fn host_bundle_recovery_commands_parse_agent_scope_and_quarantine() {
    let status = Cli::try_parse_from(["tracedecay", "host-bundle", "status"]).unwrap();
    assert!(matches!(
        status.command,
        Some(Commands::HostBundle {
            action: HostBundleAction::Status
        })
    ));
    let recover = Cli::try_parse_from([
        "tracedecay",
        "host-bundle",
        "recover",
        "--agent",
        "opencode",
        "--quarantine",
        "--yes",
    ])
    .unwrap();
    assert!(recover.yes);
    assert!(matches!(
        recover.command,
        Some(Commands::HostBundle {
            action: HostBundleAction::Recover {
                agent: Some(ref agent),
                quarantine: true,
            }
        }) if agent == "opencode"
    ));
    let all_hosts = Cli::try_parse_from(["tracedecay", "host-bundle", "recover", "--dry-run"])
        .expect("--dry-run needs no --component on the recovery verb");
    assert!(all_hosts.dry_run);
    assert!(matches!(
        all_hosts.command,
        Some(Commands::HostBundle {
            action: HostBundleAction::Recover {
                agent: None,
                quarantine: false,
            }
        })
    ));
}

#[test]
fn host_bundle_artifact_commands_parse_explicit_scope_and_confirmation() {
    let backup = Cli::try_parse_from([
        "tracedecay",
        "host-bundle",
        "artifact-backup",
        "--agent",
        "opencode",
        "--component",
        "agent",
        "--yes",
    ])
    .expect("artifact backup is an explicit host-component command");
    assert!(backup.yes);
    assert_eq!(backup.component, Some(super::HostBundleComponentArg::Agent));
    assert!(matches!(
        backup.command,
        Some(Commands::HostBundle {
            action: HostBundleAction::ArtifactBackup { ref agent }
        }) if agent == "opencode"
    ));

    let restore = Cli::try_parse_from([
        "tracedecay",
        "host-bundle",
        "artifact-restore",
        "--agent",
        "opencode",
        "--component",
        "agent",
        "--backup-id",
        "01010101010101010101010101010101",
        "--yes",
    ])
    .expect("artifact restore names its durable backup receipt");
    assert!(restore.yes);
    assert_eq!(
        restore.component,
        Some(super::HostBundleComponentArg::Agent)
    );
    assert!(matches!(
        restore.command,
        Some(Commands::HostBundle {
            action: HostBundleAction::ArtifactRestore {
                ref agent,
                ref backup_id,
            }
        }) if agent == "opencode" && backup_id == "01010101010101010101010101010101"
    ));
}

#[test]
fn update_and_post_update_parse_no_heal_flag() {
    let update = Cli::try_parse_from(["tracedecay", "update", "--no-heal"])
        .expect("update --no-heal should parse");
    let post_update = Cli::try_parse_from(["tracedecay", "post-update", "--no-heal"])
        .expect("post-update --no-heal should parse");
    let post_update_default = Cli::try_parse_from(["tracedecay", "post-update"])
        .expect("post-update should parse without --no-heal");
    let post_update_strict = Cli::try_parse_from(["tracedecay", "post-update", "--strict"])
        .expect("post-update --strict should parse");
    let post_update_forward_only = Cli::try_parse_from([
        "tracedecay",
        "post-update",
        "--mode",
        "dogfood-forward-only",
    ])
    .expect("typed dogfood forward-only mode should parse");

    assert!(matches!(
        update.command,
        Some(Commands::Update {
            no_heal: true,
            no_reinstall: false
        })
    ));
    assert!(matches!(
        post_update.command,
        Some(Commands::PostUpdate {
            no_heal: true,
            no_reinstall: false,
            lifecycle_lease_token: None,
            strict: false,
            mode: PostUpdateMode::Normal,
        })
    ));
    assert!(matches!(
        post_update_default.command,
        Some(Commands::PostUpdate {
            no_heal: false,
            no_reinstall: false,
            lifecycle_lease_token: None,
            strict: false,
            mode: PostUpdateMode::Normal,
        })
    ));
    assert!(matches!(
        post_update_strict.command,
        Some(Commands::PostUpdate { strict: true, .. })
    ));
    assert!(matches!(
        post_update_forward_only.command,
        Some(Commands::PostUpdate {
            mode: PostUpdateMode::DogfoodForwardOnly,
            ..
        })
    ));
}

#[test]
fn upgrade_parses_no_heal_flag() {
    let upgrade = Cli::try_parse_from(["tracedecay", "upgrade", "--no-heal"])
        .expect("upgrade --no-heal should parse");
    let upgrade_default =
        Cli::try_parse_from(["tracedecay", "upgrade"]).expect("upgrade should parse");

    assert!(matches!(
        upgrade.command,
        Some(Commands::Upgrade {
            no_heal: true,
            no_reinstall: false
        })
    ));
    assert!(matches!(
        upgrade_default.command,
        Some(Commands::Upgrade {
            no_heal: false,
            no_reinstall: false
        })
    ));
}

#[test]
fn upgrade_update_and_post_update_parse_no_reinstall_flag() {
    let upgrade = Cli::try_parse_from(["tracedecay", "upgrade", "--no-reinstall"])
        .expect("upgrade --no-reinstall should parse");
    let update = Cli::try_parse_from(["tracedecay", "update", "--no-reinstall"])
        .expect("update --no-reinstall should parse");
    let post_update = Cli::try_parse_from(["tracedecay", "post-update", "--no-reinstall"])
        .expect("post-update --no-reinstall should parse");

    assert!(matches!(
        upgrade.command,
        Some(Commands::Upgrade {
            no_heal: false,
            no_reinstall: true
        })
    ));
    assert!(matches!(
        update.command,
        Some(Commands::Update {
            no_heal: false,
            no_reinstall: true
        })
    ));
    assert!(matches!(
        post_update.command,
        Some(Commands::PostUpdate {
            no_heal: false,
            no_reinstall: true,
            lifecycle_lease_token: None,
            strict: false,
            mode: PostUpdateMode::Normal,
        })
    ));

    // --no-heal and --no-reinstall are independent and may combine.
    let both = Cli::try_parse_from(["tracedecay", "upgrade", "--no-heal", "--no-reinstall"])
        .expect("upgrade --no-heal --no-reinstall should parse");
    assert!(matches!(
        both.command,
        Some(Commands::Upgrade {
            no_heal: true,
            no_reinstall: true
        })
    ));
}

/// The full about text for one subcommand, so help assertions don't couple
/// to exact phrasing elsewhere in the global help output.
fn subcommand_about(name: &str) -> String {
    let command = Cli::command();
    let subcommand = command
        .find_subcommand(name)
        .unwrap_or_else(|| panic!("`{name}` subcommand should exist"));
    let about = subcommand
        .get_long_about()
        .or_else(|| subcommand.get_about())
        .map(ToString::to_string)
        .unwrap_or_default();
    format!("{} {about}", subcommand.get_about().unwrap_or_default())
}

#[test]
fn upgrade_help_states_refresh_runs_only_after_install() {
    let about = subcommand_about("upgrade").to_lowercase();

    // The distinction from `update`: install first, and no refresh (plugins,
    // daemon, health pass) on a no-op upgrade.
    assert!(about.contains("install"));
    assert!(about.contains("refresh"));
    assert!(about.contains("already up to date"));
}

#[test]
fn update_help_states_refresh_runs_even_when_current() {
    let about = subcommand_about("update").to_lowercase();

    // The distinction from `upgrade`: the refresh runs regardless of whether
    // a new binary was installed.
    assert!(about.contains("refresh"));
    assert!(about.contains("even when"));
}

#[test]
fn lsp_servers_command_parses_json_flag() {
    let cli = Cli::try_parse_from(["tracedecay", "lsp", "servers", "--json"])
        .expect("lsp servers should parse");

    assert!(matches!(
        cli.command,
        Some(Commands::Lsp {
            action: LspAction::Servers { json: true }
        })
    ));
}

#[test]
fn lsp_bridge_accepts_explicit_project_or_initialize_root() {
    let cli = Cli::try_parse_from([
        "tracedecay",
        "lsp",
        "bridge",
        "--stdio",
        "--project",
        "/workspace/project",
    ])
    .expect("lsp bridge should parse");

    assert!(matches!(
        cli.command,
        Some(Commands::Lsp {
            action: LspAction::Bridge {
                stdio: true,
                project,
            }
        }) if project.as_deref() == Some("/workspace/project")
    ));
    let initialize_routed = Cli::try_parse_from(["tracedecay", "lsp", "bridge", "--stdio"])
        .expect("initialize-routed LSP bridge should parse");
    assert!(matches!(
        initialize_routed.command,
        Some(Commands::Lsp {
            action: LspAction::Bridge {
                stdio: true,
                project: None,
            }
        })
    ));
}

#[test]
fn codex_install_automation_flag_parses_without_extra_knobs() {
    let cli = Cli::try_parse_from(["tracedecay", "install", "--agent", "codex", "--automation"])
        .expect("Codex automation install should parse");

    assert!(matches!(
        cli.command,
        Some(Commands::Install {
            agent,
            automation,
            ..
        }) if agent.as_deref() == Some("codex") && automation
    ));
}

#[test]
fn daemon_install_service_command_parses_socket_and_no_start() {
    let cli = Cli::try_parse_from([
        "tracedecay",
        "daemon",
        "install-service",
        "--socket",
        "/tmp/tracedecay.sock",
        "--no-start",
    ])
    .expect("daemon install-service should parse");

    assert!(matches!(
        cli.command,
        Some(Commands::Daemon {
            action: DaemonAction::InstallService { socket, no_start }
        }) if socket.as_deref() == Some("/tmp/tracedecay.sock") && no_start
    ));
}

#[test]
fn daemon_run_start_and_stop_commands_parse_lifecycle_options() {
    let run = Cli::try_parse_from([
        "tracedecay",
        "daemon",
        "run",
        "--profile-root",
        r"C:\Users\trace\AppData\Local\TraceDecay",
    ])
    .expect("daemon run profile root should parse");
    assert!(matches!(
        run.command,
        Some(Commands::Daemon {
            action: DaemonAction::Run {
                socket: None,
                profile_root: Some(profile_root),
            }
        }) if profile_root == r"C:\Users\trace\AppData\Local\TraceDecay"
    ));

    let start =
        Cli::try_parse_from(["tracedecay", "daemon", "start"]).expect("daemon start should parse");
    assert!(matches!(
        start.command,
        Some(Commands::Daemon {
            action: DaemonAction::Start
        })
    ));

    let stop =
        Cli::try_parse_from(["tracedecay", "daemon", "stop"]).expect("daemon stop should parse");
    assert!(matches!(
        stop.command,
        Some(Commands::Daemon {
            action: DaemonAction::Stop
        })
    ));
}

#[test]
fn status_and_branch_add_commands_dispatch_to_expected_variants() {
    let status = Cli::try_parse_from([
        "tracedecay",
        "status",
        "/tmp/project",
        "--json",
        "--short",
        "--details",
        "--runtime",
    ])
    .expect("status command should parse");
    assert!(matches!(
        status.command,
        Some(Commands::Status {
            path,
            project_id,
            project_path,
            json,
            short,
            details,
            runtime,
        }) if path.as_deref() == Some("/tmp/project")
            && project_id.is_none()
            && project_path.is_none()
            && json
            && short
            && details
            && runtime
    ));

    let branch = Cli::try_parse_from([
        "tracedecay",
        "branch",
        "add",
        "feature/dispatch-tests",
        "--path",
        "/tmp/project",
    ])
    .expect("branch add should parse");
    assert!(matches!(
        branch.command,
        Some(Commands::Branch {
            action: BranchAction::Add { name, path }
        }) if name.as_deref() == Some("feature/dispatch-tests")
            && path.as_deref() == Some("/tmp/project")
    ));
}

#[test]
fn branch_autotrack_enable_parses_poll_secs_and_path() {
    use super::BranchAutotrackAction;
    let cli = Cli::try_parse_from([
        "tracedecay",
        "branch",
        "autotrack",
        "enable",
        "--poll-secs",
        "120",
        "--path",
        "/tmp/project",
    ])
    .expect("branch autotrack enable should parse");
    assert!(matches!(
        cli.command,
        Some(Commands::Branch {
            action: BranchAction::Autotrack {
                action: BranchAutotrackAction::Enable { poll_secs, path }
            }
        }) if poll_secs == Some(120) && path.as_deref() == Some("/tmp/project")
    ));
}

#[test]
fn init_and_sync_parse_runtime_skip_and_include_folders() {
    let init = Cli::try_parse_from([
        "tracedecay",
        "init",
        "/tmp/project",
        "--skip-folder",
        "vendor",
        "dist",
        "--include-folder",
        "dist/generated",
    ])
    .expect("init skip/include folders should parse");
    assert!(matches!(
        init.command,
        Some(Commands::Init {
            path,
            skip_folders,
            include_folders,
        }) if path.as_deref() == Some("/tmp/project")
            && skip_folders == strings(&["vendor", "dist"])
            && include_folders == strings(&["dist/generated"])
    ));

    let sync = Cli::try_parse_from([
        "tracedecay",
        "sync",
        "/tmp/project",
        "--force",
        "--include-folder",
        "dist",
        "vendor/generated",
    ])
    .expect("sync include folders should parse");
    assert!(matches!(
        sync.command,
        Some(Commands::Sync {
            path,
            force,
            skip_folders,
            include_folders,
            ..
        }) if path.as_deref() == Some("/tmp/project")
            && force
            && skip_folders.is_empty()
            && include_folders == strings(&["dist", "vendor/generated"])
    ));
}

#[test]
fn init_and_sync_parse_repeated_include_folder_flags() {
    let init = Cli::try_parse_from([
        "tracedecay",
        "init",
        "/tmp/project",
        "--include-folder",
        "dist",
        "--include-folder",
        "vendor/generated",
    ])
    .expect("repeated init include folders should parse");
    assert!(matches!(
        init.command,
        Some(Commands::Init {
            path,
            include_folders,
            ..
        }) if path.as_deref() == Some("/tmp/project")
            && include_folders == strings(&["dist", "vendor/generated"])
    ));

    let sync = Cli::try_parse_from([
        "tracedecay",
        "sync",
        "/tmp/project",
        "--include-folder",
        "dist",
        "--include-folder",
        "vendor/generated",
    ])
    .expect("repeated sync include folders should parse");
    assert!(matches!(
        sync.command,
        Some(Commands::Sync {
            path,
            include_folders,
            ..
        }) if path.as_deref() == Some("/tmp/project")
            && include_folders == strings(&["dist", "vendor/generated"])
    ));
}

#[test]
fn memory_status_command_dispatches_to_expected_variant() {
    let cli = Cli::try_parse_from([
        "tracedecay",
        "memory",
        "status",
        "--json",
        "--path",
        "/tmp/project",
    ])
    .expect("memory status command should parse");

    assert!(matches!(
        cli.command,
        Some(Commands::Memory {
            action: MemoryAction::Status {
                json,
                path,
                project_id,
                project_path,
            }
        }) if json
            && path.as_deref() == Some("/tmp/project")
            && project_id.is_none()
            && project_path.is_none()
    ));
}

#[test]
fn automation_config_commands_parse_project_sidecar_flags() {
    let get = Cli::try_parse_from([
        "tracedecay",
        "automation",
        "config",
        "get",
        "--json",
        "--path",
        "/tmp/project",
    ])
    .expect("automation config get should parse");
    assert!(matches!(
        get.command,
        Some(Commands::Automation {
            action:
                AutomationAction::Config {
                    action:
                        AutomationConfigAction::Get {
                            scope: AutomationConfigScope::Project,
                            json,
                            path
                        }
                }
        }) if json && path.as_deref() == Some("/tmp/project")
    ));

    let explain = Cli::try_parse_from([
        "tracedecay",
        "automation",
        "config",
        "explain",
        "--json",
        "--scope",
        "global",
    ])
    .expect("automation config explain should parse");
    assert!(matches!(
        explain.command,
        Some(Commands::Automation {
            action:
                AutomationAction::Config {
                    action:
                        AutomationConfigAction::Explain {
                            scope: AutomationConfigScope::Global,
                            json,
                            path
                        }
                }
        }) if json && path.is_none()
    ));

    let enable = Cli::try_parse_from([
        "tracedecay",
        "automation",
        "config",
        "enable",
        "--scope",
        "global",
    ])
    .expect("automation config enable should parse");
    assert!(matches!(
        enable.command,
        Some(Commands::Automation {
            action:
                AutomationAction::Config {
                    action:
                        AutomationConfigAction::Enable {
                            scope: AutomationConfigScope::Global,
                            path
                        }
                }
        }) if path.is_none()
    ));

    let disable = Cli::try_parse_from(["tracedecay", "automation", "config", "disable"])
        .expect("automation config disable should parse");
    assert!(matches!(
        disable.command,
        Some(Commands::Automation {
            action:
                AutomationAction::Config {
                    action:
                        AutomationConfigAction::Disable {
                            scope: AutomationConfigScope::Project,
                            path
                        }
                }
        }) if path.is_none()
    ));

    let set = Cli::try_parse_from([
        "tracedecay",
        "automation",
        "config",
        "set",
        "--backend",
        "codex-app-server",
        "--host-mode",
        "delegated-host",
        "--timeout-secs",
        "120",
        "--scheduler-tick-secs",
        "30",
        "--auto-apply-memory-ops",
        "false",
        "--auto-enable-skills",
        "false",
        "--export-memory-digest",
        "false",
        "--memory-curator",
        "true",
        "--memory-curator-schedule",
        "manual",
        "--memory-curator-interval-secs",
        "900",
        "--memory-curator-cooldown-secs",
        "300",
        "--memory-curator-min-idle-secs",
        "120",
        "--memory-curator-stale-lock-secs",
        "3600",
        "--session-reflector",
        "true",
        "--session-reflector-schedule",
        "interval",
        "--session-reflector-interval-secs",
        "1800",
        "--session-reflector-cooldown-secs",
        "600",
        "--session-reflector-min-idle-secs",
        "60",
        "--session-reflector-stale-lock-secs",
        "7200",
        "--skill-writer",
        "true",
        "--skill-writer-schedule",
        "manual",
        "--skill-writer-interval-secs",
        "",
        "--skill-writer-cooldown-secs",
        "none",
    ])
    .expect("automation config set should parse");
    let Some(Commands::Automation {
        action:
            AutomationAction::Config {
                action:
                    AutomationConfigAction::Set {
                        scope,
                        backend,
                        host_mode,
                        timeout_secs,
                        scheduler_tick_secs,
                        auto_apply_memory_ops,
                        auto_enable_skills,
                        export_memory_digest,
                        memory_curator,
                        memory_curator_schedule,
                        memory_curator_interval_secs,
                        memory_curator_cooldown_secs,
                        memory_curator_min_idle_secs,
                        memory_curator_stale_lock_secs,
                        session_reflector,
                        session_reflector_schedule,
                        session_reflector_interval_secs,
                        session_reflector_cooldown_secs,
                        session_reflector_min_idle_secs,
                        session_reflector_stale_lock_secs,
                        skill_writer,
                        skill_writer_schedule,
                        skill_writer_interval_secs,
                        skill_writer_cooldown_secs,
                        skill_writer_min_idle_secs,
                        skill_writer_stale_lock_secs,
                        path,
                    },
            },
    }) = set.command
    else {
        panic!("automation config set should parse into Set action");
    };
    assert_eq!(scope, AutomationConfigScope::Project);
    assert_eq!(backend.as_deref(), Some("codex-app-server"));
    assert_eq!(host_mode.as_deref(), Some("delegated-host"));
    assert_eq!(timeout_secs, Some(120));
    assert_eq!(scheduler_tick_secs, Some(30));
    assert_eq!(auto_apply_memory_ops, Some(false));
    assert_eq!(auto_enable_skills, Some(false));
    assert_eq!(export_memory_digest, Some(false));
    assert_eq!(memory_curator, Some(true));
    assert_eq!(memory_curator_schedule.as_deref(), Some("manual"));
    assert_eq!(memory_curator_interval_secs.as_deref(), Some("900"));
    assert_eq!(memory_curator_cooldown_secs.as_deref(), Some("300"));
    assert_eq!(memory_curator_min_idle_secs.as_deref(), Some("120"));
    assert_eq!(memory_curator_stale_lock_secs.as_deref(), Some("3600"));
    assert_eq!(session_reflector, Some(true));
    assert_eq!(session_reflector_schedule.as_deref(), Some("interval"));
    assert_eq!(session_reflector_interval_secs.as_deref(), Some("1800"));
    assert_eq!(session_reflector_cooldown_secs.as_deref(), Some("600"));
    assert_eq!(session_reflector_min_idle_secs.as_deref(), Some("60"));
    assert_eq!(session_reflector_stale_lock_secs.as_deref(), Some("7200"));
    assert_eq!(skill_writer, Some(true));
    assert_eq!(skill_writer_schedule.as_deref(), Some("manual"));
    assert_eq!(skill_writer_interval_secs.as_deref(), Some(""));
    assert_eq!(skill_writer_cooldown_secs.as_deref(), Some("none"));
    assert!(skill_writer_min_idle_secs.is_none());
    assert!(skill_writer_stale_lock_secs.is_none());
    assert!(path.is_none());
}

#[test]
fn automation_run_memory_curation_parses_manual_flags() {
    let cli = Cli::try_parse_from([
        "tracedecay",
        "automation",
        "run",
        "memory-curation",
        "--max-clusters",
        "8",
        "--min-confidence",
        "0.7",
        "--path",
        "/tmp/project",
    ])
    .expect("automation memory-curation run should parse");

    assert!(matches!(
        cli.command,
        Some(Commands::Automation {
            action:
                AutomationAction::Run {
                    action:
                        AutomationRunAction::MemoryCuration {
                            max_clusters,
                            min_confidence,
                            path,
                        }
                }
        }) if max_clusters == 8
            && (min_confidence - 0.7).abs() < f64::EPSILON
            && path.as_deref() == Some("/tmp/project")
    ));
}

#[test]
fn automation_run_session_reflection_parses_manual_flags() {
    let cli = Cli::try_parse_from([
        "tracedecay",
        "automation",
        "run",
        "session-reflection",
        "--provider",
        "codex",
        "--query",
        "remember decisions",
        "--evidence-limit",
        "12",
        "--scope",
        "session",
        "--session-id",
        "session-123",
        "--include-summaries",
        "false",
        "--sort",
        "hybrid",
        "--source",
        "hermes",
        "--role",
        "assistant",
        "--start-time",
        "1715100000",
        "--end-time",
        "1715100100",
        "--path",
        "/tmp/project",
    ])
    .expect("automation session-reflection run should parse");

    assert!(matches!(
        cli.command,
        Some(Commands::Automation {
            action:
                AutomationAction::Run {
                    action:
                        AutomationRunAction::SessionReflection {
                            provider,
                            query,
                            evidence_limit,
                            scope,
                            session_id,
                            include_summaries,
                            sort,
                            source,
                            role,
                            start_time,
                            end_time,
                            path,
                        }
                }
        }) if provider == "codex"
            && query == "remember decisions"
            && evidence_limit == 12
            && scope == "session"
            && session_id.as_deref() == Some("session-123")
            && !include_summaries
            && sort == "hybrid"
            && source.as_deref() == Some("hermes")
            && role.as_deref() == Some("assistant")
            && start_time == Some(1_715_100_000)
            && end_time == Some(1_715_100_100)
            && path.as_deref() == Some("/tmp/project")
    ));
}

#[test]
fn automation_run_skill_writing_parses_manual_flags() {
    let cli = Cli::try_parse_from([
        "tracedecay",
        "automation",
        "run",
        "skill-writing",
        "--provider",
        "cursor",
        "--query",
        "workflow corrections",
        "--evidence-limit",
        "9",
        "--path",
        "/tmp/project",
    ])
    .expect("automation skill-writing run should parse");

    assert!(matches!(
        cli.command,
        Some(Commands::Automation {
            action:
                AutomationAction::Run {
                    action:
                        AutomationRunAction::SkillWriting {
                            provider,
                            query,
                            evidence_limit,
                            path,
                        }
                }
        }) if provider == "cursor"
            && query == "workflow corrections"
            && evidence_limit == 9
            && path.as_deref() == Some("/tmp/project")
    ));
}

#[test]
fn automation_run_skill_writing_defaults_to_all_providers() {
    let cli = Cli::try_parse_from(["tracedecay", "automation", "run", "skill-writing"])
        .expect("automation skill-writing run should parse with defaults");

    assert!(matches!(
        cli.command,
        Some(Commands::Automation {
            action:
                AutomationAction::Run {
                    action:
                        AutomationRunAction::SkillWriting { provider, .. }
                }
        }) if provider == "all"
    ));
}

#[test]
fn automation_runs_commands_parse_history_flags() {
    let list = Cli::try_parse_from([
        "tracedecay",
        "automation",
        "runs",
        "list",
        "--limit",
        "5",
        "--json",
        "--path",
        "/tmp/project",
    ])
    .expect("automation runs list should parse");

    assert!(matches!(
        list.command,
        Some(Commands::Automation {
            action:
                AutomationAction::Runs {
                    action:
                        AutomationRunsAction::List {
                            limit,
                            json,
                            path,
                        }
                }
        }) if limit == 5 && json && path.as_deref() == Some("/tmp/project")
    ));

    let view = Cli::try_parse_from([
        "tracedecay",
        "automation",
        "runs",
        "view",
        "run-123",
        "--json",
        "--path",
        "/tmp/project",
    ])
    .expect("automation runs view should parse");

    assert!(matches!(
        view.command,
        Some(Commands::Automation {
            action:
                AutomationAction::Runs {
                    action:
                        AutomationRunsAction::View { run_id, json, path }
                }
        }) if run_id == "run-123" && json && path.as_deref() == Some("/tmp/project")
    ));

    let artifact = Cli::try_parse_from([
        "tracedecay",
        "automation",
        "runs",
        "artifact",
        "run-123",
        "codex_handoff",
        "--json",
        "--path",
        "/tmp/project",
    ])
    .expect("automation runs artifact should parse");

    assert!(matches!(
        artifact.command,
        Some(Commands::Automation {
            action:
                AutomationAction::Runs {
                    action:
                        AutomationRunsAction::Artifact {
                            run_id,
                            kind,
                            json,
                            path
                        }
                }
        }) if run_id == "run-123"
            && kind == "codex_handoff"
            && json
            && path.as_deref() == Some("/tmp/project")
    ));
}

#[test]
fn automation_skills_commands_parse_lifecycle_flags() {
    let draft = Cli::try_parse_from([
        "tracedecay",
        "automation",
        "skills",
        "draft",
        "--id",
        "repo-hygiene",
        "--title",
        "Repository hygiene",
        "--summary",
        "Keep checks focused",
        "--category",
        "maintenance",
        "--body",
        "Run focused tests.",
        "--pinned",
    ])
    .expect("automation skills draft should parse");
    assert!(matches!(
        draft.command,
        Some(Commands::Automation {
            action:
                AutomationAction::Skills {
                    action:
                        AutomationSkillsAction::Draft {
                            id,
                            title,
                            summary,
                            category,
                            body,
                            pinned,
                        }
                }
        }) if id == "repo-hygiene"
            && title == "Repository hygiene"
            && summary == "Keep checks focused"
            && category == "maintenance"
            && body == "Run focused tests."
            && pinned
    ));

    let update = Cli::try_parse_from([
        "tracedecay",
        "automation",
        "skills",
        "update",
        "repo-hygiene",
        "--summary",
        "Updated",
        "--pinned",
        "false",
    ])
    .expect("automation skills update should parse");
    assert!(matches!(
        update.command,
        Some(Commands::Automation {
            action:
                AutomationAction::Skills {
                    action:
                        AutomationSkillsAction::Update {
                            id,
                            summary,
                            pinned,
                            ..
                        }
                }
        }) if id == "repo-hygiene"
            && summary.as_deref() == Some("Updated")
            && pinned == Some(false)
    ));

    let approve = Cli::try_parse_from([
        "tracedecay",
        "automation",
        "skills",
        "approve",
        "repo-hygiene",
    ])
    .expect("automation skills approve should parse");
    assert!(matches!(
        approve.command,
        Some(Commands::Automation {
            action:
                AutomationAction::Skills {
                    action: AutomationSkillsAction::Approve { id }
                }
        }) if id == "repo-hygiene"
    ));

    let install = Cli::try_parse_from([
        "tracedecay",
        "automation",
        "skills",
        "install",
        "--target",
        "cursor",
        "--output",
        "/tmp/plugin",
        "--json",
    ])
    .expect("automation skills install should parse");
    assert!(matches!(
        install.command,
        Some(Commands::Automation {
            action:
                AutomationAction::Skills {
                    action:
                        AutomationSkillsAction::Install {
                            target,
                            output,
                            plugin_artifact,
                            json,
                        }
                }
        }) if target == AutomationSkillsInstallTarget::Cursor
            && output == "/tmp/plugin"
            && !plugin_artifact
            && json
    ));

    let opencode_install = Cli::try_parse_from([
        "tracedecay",
        "automation",
        "skills",
        "install",
        "--target",
        "opencode",
        "--output",
        "/tmp/AGENTS.md",
    ])
    .expect("automation skills install should accept opencode alias");
    assert!(matches!(
        opencode_install.command,
        Some(Commands::Automation {
            action:
                AutomationAction::Skills {
                    action:
                        AutomationSkillsAction::Install {
                            target,
                            output,
                            plugin_artifact,
                            json,
                        }
                }
        }) if target == AutomationSkillsInstallTarget::OpenCode
            && output == "/tmp/AGENTS.md"
            && !plugin_artifact
            && !json
    ));

    let codex_artifact = Cli::try_parse_from([
        "tracedecay",
        "automation",
        "skills",
        "install",
        "--target",
        "codex",
        "--output",
        "/tmp/codex-plugin",
        "--plugin-artifact",
    ])
    .expect("automation skills install codex artifact should parse");
    assert!(matches!(
        codex_artifact.command,
        Some(Commands::Automation {
            action:
                AutomationAction::Skills {
                    action:
                        AutomationSkillsAction::Install {
                            target,
                            output,
                            plugin_artifact,
                            json,
                        }
                }
        }) if target == AutomationSkillsInstallTarget::Codex
            && output == "/tmp/codex-plugin"
            && plugin_artifact
            && !json
    ));
}

#[test]
fn project_selector_flags_parse_for_cli_read_surfaces() {
    let status =
        Cli::try_parse_from(["tracedecay", "status", "--project-id", "proj_123", "--json"])
            .expect("status project selector should parse");
    assert!(matches!(
        status.command,
        Some(Commands::Status {
            path,
            project_id,
            project_path,
            json,
            ..
        }) if path.is_none()
            && project_id.as_deref() == Some("proj_123")
            && project_path.is_none()
            && json
    ));

    let memory = Cli::try_parse_from([
        "tracedecay",
        "memory",
        "status",
        "--project-path",
        "/tmp/project",
    ])
    .expect("memory status project selector should parse");
    assert!(matches!(
        memory.command,
        Some(Commands::Memory {
            action:
                MemoryAction::Status {
                    path,
                    project_id,
                    project_path,
                    ..
                }
        }) if path.is_none()
            && project_id.is_none()
            && project_path.as_deref() == Some("/tmp/project")
    ));

    let sessions = Cli::try_parse_from([
        "tracedecay",
        "sessions",
        "search",
        "needle",
        "--project-id",
        "proj_123",
    ])
    .expect("sessions search project selector should parse");
    assert!(matches!(
        sessions.command,
        Some(Commands::Sessions {
            action: SessionsAction::Search(args)
        }) if args.project_id.as_deref() == Some("proj_123") && args.project_path.is_none()
    ));
}

#[test]
fn migrate_commands_parse_manifest_scaffolding_flags() {
    let consolidate = Cli::try_parse_from([
        "tracedecay",
        "migrate",
        "consolidate",
        "--project",
        "/tmp/project",
        "--profile-root",
        "/tmp/profile",
        "--source-project-id",
        "proj_old",
        "--target-project-id",
        "proj_current",
        "--apply",
        "--confirm-token",
        "confirm-123",
        "--json",
    ])
    .expect("migrate consolidate should parse");
    assert!(matches!(
        consolidate.command,
        Some(Commands::Migrate {
            action:
                MigrateAction::Consolidate {
                    project,
                    profile_root,
                    source_project_id,
                    target_project_id,
                    apply,
                    confirm_token,
                    json,
                }
        }) if project == "/tmp/project"
            && profile_root.as_deref() == Some("/tmp/profile")
            && source_project_id == "proj_old"
            && target_project_id == "proj_current"
            && apply
            && confirm_token.as_deref() == Some("confirm-123")
            && json
    ));

    let plan = Cli::try_parse_from([
        "tracedecay",
        "migrate",
        "plan",
        "--root",
        "/tmp/project",
        "--manifest",
        "/tmp/manifest.json",
        "--profile-root",
        "/tmp/profile",
        "--project-id",
        "proj_123",
        "--json",
    ])
    .expect("migrate plan should parse");
    assert!(matches!(
        plan.command,
        Some(Commands::Migrate {
            action:
                MigrateAction::Plan {
                    roots,
                    manifest,
                    profile_root,
                    project_id,
                    json,
                    ..
                }
        }) if roots == vec!["/tmp/project".to_string()]
            && manifest.as_deref() == Some("/tmp/manifest.json")
            && profile_root.as_deref() == Some("/tmp/profile")
            && project_id.as_deref() == Some("proj_123")
            && json
    ));

    let apply = Cli::try_parse_from([
        "tracedecay",
        "migrate",
        "apply",
        "--manifest",
        "/tmp/manifest.json",
        "--confirm-token",
        "confirm-mig_123",
    ])
    .expect("migrate apply should parse");
    assert!(matches!(
        apply.command,
        Some(Commands::Migrate {
            action:
                MigrateAction::Apply {
                    manifest,
                    confirm_token,
                }
        }) if manifest == "/tmp/manifest.json" && confirm_token == "confirm-mig_123"
    ));

    let verify = Cli::try_parse_from([
        "tracedecay",
        "migrate",
        "verify",
        "--manifest",
        "/tmp/manifest.json",
        "--json",
    ])
    .expect("migrate verify should parse");
    assert!(matches!(
        verify.command,
        Some(Commands::Migrate {
            action: MigrateAction::Verify { manifest, json }
        }) if manifest == "/tmp/manifest.json" && json
    ));
}

#[test]
fn migrate_reconstruct_apply_flag_parses() {
    let cli = Cli::try_parse_from([
        "tracedecay",
        "migrate",
        "reconstruct",
        "--profile-root",
        "/tmp/profile",
        "--apply",
        "--json",
    ])
    .expect("migrate reconstruct should parse");

    assert!(matches!(
        cli.command,
        Some(Commands::Migrate {
            action:
                MigrateAction::Reconstruct {
                    profile_root,
                    apply,
                    json,
                }
        }) if profile_root == "/tmp/profile" && apply && json
    ));
}

#[test]
fn migrate_export_requires_from_profile_flag() {
    let err = match Cli::try_parse_from([
        "tracedecay",
        "migrate",
        "export",
        "--project-id",
        "proj_123",
        "--to",
        "/tmp/exported",
    ]) {
        Ok(_) => panic!("migrate export should require --from-profile"),
        Err(err) => err,
    };

    assert_eq!(err.kind(), ErrorKind::MissingRequiredArgument);
}

#[test]
fn migrate_registry_gc_parses() {
    let cli = Cli::try_parse_from([
        "tracedecay",
        "migrate",
        "registry-gc",
        "--prefix",
        "/tmp",
        "--apply",
        "--json",
    ])
    .expect("migrate registry-gc should parse");

    assert!(matches!(
        cli.command,
        Some(Commands::Migrate {
            action:
                MigrateAction::RegistryGc {
                    prefix,
                    apply,
                    json,
                }
        }) if prefix.as_deref() == Some("/tmp") && apply && json
    ));
}

#[test]
fn migrate_storage_report_parses() {
    let cli = Cli::try_parse_from([
        "tracedecay",
        "migrate",
        "storage-report",
        "--profile-root",
        "/tmp/profile",
        "--json",
    ])
    .expect("migrate storage-report should parse");

    assert!(matches!(
        cli.command,
        Some(Commands::Migrate {
            action: MigrateAction::StorageReport {
                profile_root,
                project_id,
                project_root,
                json,
            }
        }) if profile_root.as_deref() == Some("/tmp/profile")
            && project_id.is_none()
            && project_root.is_none()
            && json
    ));
}

#[test]
fn migrate_storage_report_parses_targeted_project() {
    let cli = Cli::try_parse_from([
        "tracedecay",
        "migrate",
        "storage-report",
        "--profile-root",
        "/tmp/profile",
        "--project-id",
        "proj_a",
        "--project-root",
        "/repos/a",
    ])
    .expect("targeted migrate storage-report should parse");

    assert!(matches!(
        cli.command,
        Some(Commands::Migrate {
            action: MigrateAction::StorageReport {
                profile_root,
                project_id,
                project_root,
                json: false,
            }
        }) if profile_root.as_deref() == Some("/tmp/profile")
            && project_id.as_deref() == Some("proj_a")
            && project_root.as_deref() == Some("/repos/a")
    ));
}

#[test]
fn branch_remove_requires_a_branch_name() {
    let err = match Cli::try_parse_from(["tracedecay", "branch", "remove"]) {
        Ok(_) => panic!("branch remove should require a name"),
        Err(err) => err,
    };

    assert_eq!(err.kind(), ErrorKind::MissingRequiredArgument);
}

#[test]
fn parses_sessions_ingest_and_search_commands() {
    let ingest =
        Cli::try_parse_from(["tracedecay", "sessions", "ingest", "--provider", "cursor"]).unwrap();
    match ingest.command {
        Some(Commands::Sessions {
            action:
                SessionsAction::Ingest {
                    provider,
                    project_id,
                    project_path,
                },
        }) => {
            assert_eq!(provider.as_deref(), Some("cursor"));
            assert!(project_id.is_none());
            assert!(project_path.is_none());
        }
        _ => panic!("expected sessions ingest command"),
    }

    let search = Cli::try_parse_from([
        "tracedecay",
        "sessions",
        "search",
        "needle",
        "--provider",
        "codex",
        "--limit",
        "5",
    ])
    .unwrap();
    match search.command {
        Some(Commands::Sessions {
            action: SessionsAction::Search(args),
        }) => {
            assert_eq!(args.query, "needle");
            assert_eq!(args.provider.as_deref(), Some("codex"));
            assert_eq!(args.scope, "all");
            assert_eq!(args.message_type, "all");
            assert!(args.parent_session_id.is_none());
            assert_eq!(args.limit, 5);
            assert!(args.project_id.is_none());
            assert!(args.project_path.is_none());
            assert!(args.since.is_none());
            assert!(args.until.is_none());
            assert!(args.branch.is_none());
            assert!(args.worktree.is_none());
            assert!(args.commit.is_none());
        }
        _ => panic!("expected sessions search command"),
    }

    let time_filtered_search = Cli::try_parse_from([
        "tracedecay",
        "sessions",
        "search",
        "needle",
        "--since",
        "last hour",
        "--until",
        "2026-07-04T00:00:00Z",
    ])
    .unwrap();
    match time_filtered_search.command {
        Some(Commands::Sessions {
            action: SessionsAction::Search(args),
        }) => {
            assert_eq!(args.since.as_deref(), Some("last hour"));
            assert_eq!(args.until.as_deref(), Some("2026-07-04T00:00:00Z"));
        }
        _ => panic!("expected sessions search command"),
    }

    let all_provider_search =
        Cli::try_parse_from(["tracedecay", "sessions", "search", "needle"]).unwrap();
    match all_provider_search.command {
        Some(Commands::Sessions {
            action: SessionsAction::Search(args),
        }) => {
            assert_eq!(args.query, "needle");
            assert!(args.provider.is_none());
            assert_eq!(args.limit, 10);
        }
        _ => panic!("expected sessions search command"),
    }

    let filtered_search = Cli::try_parse_from([
        "tracedecay",
        "sessions",
        "search",
        "needle",
        "--scope",
        "subagents_only",
        "--message-type",
        "direct_user",
        "--parent-session-id",
        "parent-1",
    ])
    .unwrap();
    assert!(matches!(
        filtered_search.command,
        Some(Commands::Sessions {
            action: SessionsAction::Search(args)
        }) if args.scope == "subagents_only"
            && args.message_type == "direct_user"
            && args.parent_session_id.as_deref() == Some("parent-1")
    ));
}

#[test]
fn sessions_help_keeps_ingest_as_legacy_source_admission_only() {
    let command = Cli::command();
    let sessions = command
        .find_subcommand("sessions")
        .expect("sessions command");
    let long_about = sessions
        .get_long_about()
        .map(ToString::to_string)
        .unwrap_or_default();
    let after_help = sessions
        .get_after_help()
        .map(ToString::to_string)
        .unwrap_or_default();

    for needle in [
        "explicit legacy source-admission command",
        "canonical observation ingest",
        "leaves temporal projection to the durable scheduler",
        "owns no parallel temporal writer",
        "never invoked by a read",
    ] {
        assert!(
            long_about.contains(needle),
            "sessions help must retain the ingest cutover contract; missing {needle:?}"
        );
    }
    assert!(
        after_help.contains("deprecated ingest `--provider` option"),
        "sessions help must identify the retained ingest compatibility option"
    );
}

#[test]
fn sessions_refresh_parses_exact_lifecycle_selectors() {
    let begin = Cli::try_parse_from([
        "tracedecay",
        "sessions",
        "refresh",
        "begin",
        "--project-id",
        "project.tracedecay",
        "--session-id",
        "session.refresh",
        "--provider",
        "cursor",
        "--source",
        "4",
        "--target",
        "9",
        "--json",
    ])
    .expect("project-scoped refresh begin should parse");
    assert!(matches!(
        begin.command,
        Some(Commands::Sessions {
            action:
                SessionsAction::Refresh {
                    action: SessionsRefreshAction::Begin(args)
                }
        }) if args.selectors.project_id.as_deref() == Some("project.tracedecay")
            && args.selectors.project_path.is_none()
            && args.selectors.profile_id.is_none()
            && args.selectors.session_id == "session.refresh"
            && args.selectors.provider == "cursor"
            && args.selectors.source == 4
            && args.selectors.target == 9
            && args.json
    ));

    let status = Cli::try_parse_from([
        "tracedecay",
        "sessions",
        "refresh",
        "status",
        "--profile-id",
        "profile.primary",
        "--session-id",
        "session.refresh",
        "--provider",
        "cursor",
        "--source",
        "4",
        "--target",
        "9",
        "--handle",
        "refresh.abc",
    ])
    .expect("profile-scoped refresh status should parse");
    assert!(matches!(
        status.command,
        Some(Commands::Sessions {
            action:
                SessionsAction::Refresh {
                    action: SessionsRefreshAction::Status(args)
                }
        }) if args.selectors.project_id.is_none()
            && args.selectors.project_path.is_none()
            && args.selectors.profile_id.as_deref() == Some("profile.primary")
            && args.selectors.session_id == "session.refresh"
            && args.selectors.provider == "cursor"
            && args.selectors.source == 4
            && args.selectors.target == 9
            && args.handle == "refresh.abc"
            && !args.json
    ));

    let cancel = Cli::try_parse_from([
        "tracedecay",
        "sessions",
        "refresh",
        "cancel",
        "--project-path",
        "/repo/tracedecay",
        "--session-id",
        "session.refresh",
        "--provider",
        "cursor",
        "--source",
        "4",
        "--target",
        "9",
        "--operation-id",
        "refresh.abc",
        "--json",
    ])
    .expect("project-path refresh cancel should parse");
    assert!(matches!(
        cancel.command,
        Some(Commands::Sessions {
            action:
                SessionsAction::Refresh {
                    action: SessionsRefreshAction::Cancel(args)
                }
        }) if args.selectors.project_id.is_none()
            && args.selectors.project_path.as_deref() == Some("/repo/tracedecay")
            && args.selectors.profile_id.is_none()
            && args.handle == "refresh.abc"
            && args.json
    ));
}

#[test]
fn sessions_refresh_never_falls_back_to_the_current_directory() {
    for args in [
        vec![
            "tracedecay",
            "sessions",
            "refresh",
            "begin",
            "--session-id",
            "session.refresh",
            "--provider",
            "cursor",
            "--source",
            "4",
            "--target",
            "9",
        ],
        vec![
            "tracedecay",
            "sessions",
            "refresh",
            "status",
            "--session-id",
            "session.refresh",
            "--provider",
            "cursor",
            "--source",
            "4",
            "--target",
            "9",
            "--operation-id",
            "refresh.abc",
        ],
        vec![
            "tracedecay",
            "sessions",
            "refresh",
            "cancel",
            "--session-id",
            "session.refresh",
            "--provider",
            "cursor",
            "--source",
            "4",
            "--target",
            "9",
            "--operation-id",
            "refresh.abc",
        ],
    ] {
        let error = match Cli::try_parse_from(args.clone()) {
            Ok(_) => panic!("refresh must require a project or profile selector"),
            Err(error) => error,
        };
        assert_eq!(
            error.kind(),
            ErrorKind::MissingRequiredArgument,
            "args: {args:?}"
        );
    }
}

#[test]
fn sessions_refresh_help_lists_modes_and_exact_selectors() {
    let command = Cli::command();
    let refresh = command
        .find_subcommand("sessions")
        .and_then(|sessions| sessions.find_subcommand("refresh"))
        .expect("sessions refresh should be registered");
    let modes = refresh
        .get_subcommands()
        .map(|subcommand| subcommand.get_name())
        .collect::<Vec<_>>();
    assert_eq!(
        modes,
        ["start", "status", "join", "resume", "cancel", "begin"]
    );

    let begin = refresh
        .find_subcommand("begin")
        .expect("sessions refresh begin should be registered");
    let flags = begin
        .get_arguments()
        .filter_map(|argument| argument.get_long())
        .collect::<Vec<_>>();
    for selector in [
        "project-id",
        "project-path",
        "profile-id",
        "session-id",
        "provider",
        "source",
        "target",
        "json",
    ] {
        assert!(
            flags.contains(&selector),
            "refresh begin help should expose --{selector}"
        );
    }

    let target_help = begin
        .get_arguments()
        .find(|argument| argument.get_long() == Some("target"))
        .and_then(|argument| argument.get_help())
        .expect("refresh target help");
    assert!(target_help.to_string().contains("mode=current"));
    assert!(target_help.to_string().contains("grain=logical_message"));

    for mode in ["status", "cancel"] {
        let command = refresh
            .find_subcommand(mode)
            .unwrap_or_else(|| panic!("sessions refresh {mode} should be registered"));
        let handle = command
            .get_arguments()
            .find(|argument| argument.get_long() == Some("handle"))
            .expect("status/cancel should expose --handle");
        assert!(
            handle
                .get_visible_aliases()
                .is_some_and(|aliases| aliases.contains(&"operation-id")),
            "--operation-id should remain a visible deprecated alias"
        );
        assert!(
            handle
                .get_help()
                .is_some_and(|help| help.to_string().contains("daemon-local handle"))
        );
    }

    let status = refresh.find_subcommand("status").unwrap();
    assert!(
        status
            .get_about()
            .is_some_and(|about| about.to_string().contains("read-only"))
    );
    let status_handle = status
        .get_arguments()
        .find(|argument| argument.get_long() == Some("handle"))
        .and_then(|argument| argument.get_help())
        .expect("status handle help");
    for origin in ["start", "join", "resume", "begin"] {
        assert!(
            status_handle.to_string().contains(origin),
            "status handle help must identify {origin} as a handle origin"
        );
    }
    for mode in ["join", "resume"] {
        assert!(
            refresh
                .find_subcommand(mode)
                .and_then(|command| command.get_about())
                .is_some_and(|about| about.to_string().contains("opaque handle")),
            "{mode} help must explain that it returns an opaque handle"
        );
    }
}
