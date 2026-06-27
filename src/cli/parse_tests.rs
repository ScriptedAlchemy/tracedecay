use super::{
    AutomationAction, AutomationConfigAction, AutomationConfigScope, AutomationRunAction,
    AutomationRunsAction, AutomationSkillsAction, AutomationSkillsInstallTarget, BranchAction, Cli,
    Commands, DaemonAction, LspAction, MemoryAction, MigrateAction, SessionsAction,
};
use clap::{error::ErrorKind, Command, CommandFactory, Parser};

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
        "--profile",
        "dev",
        "--project-root",
        "/tmp/project",
        "--no-dashboard",
    ])
    .expect("install alias should parse");

    assert!(matches!(
        cli.command,
        Some(Commands::Install {
            agent,
            local,
            profile,
            all_profiles,
            project_root,
            no_dashboard,
            ..
        }) if agent.as_deref() == Some("hermes")
            && !local
            && profile.as_deref() == Some("dev")
            && !all_profiles
            && project_root.as_deref() == Some("/tmp/project")
            && no_dashboard
    ));
}

#[test]
fn update_plugins_alias_dispatches_to_update_plugin_command() {
    let cli = Cli::try_parse_from(["tracedecay", "update-plugins"])
        .expect("update-plugin alias should parse");

    assert!(matches!(cli.command, Some(Commands::UpdatePlugin)));
}

#[test]
fn update_upgrade_and_update_plugin_parse_to_distinct_commands() {
    let update = Cli::try_parse_from(["tracedecay", "update"]).expect("update should parse");
    let upgrade = Cli::try_parse_from(["tracedecay", "upgrade"]).expect("upgrade should parse");
    let update_plugin =
        Cli::try_parse_from(["tracedecay", "update-plugin"]).expect("update-plugin should parse");

    assert!(matches!(update.command, Some(Commands::Update)));
    assert!(matches!(upgrade.command, Some(Commands::Upgrade)));
    assert!(matches!(
        update_plugin.command,
        Some(Commands::UpdatePlugin)
    ));
}

#[test]
fn update_help_describes_refresh_scope() {
    let help = Cli::command().render_long_help().to_string();

    assert!(help.contains("update"));
    assert!(help.contains("Refresh the tracedecay binary, generated plugins, and daemon"));
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
        "--model",
        "gpt-test",
        "--timeout-secs",
        "120",
        "--scheduler-tick-secs",
        "30",
        "--max-tokens",
        "4096",
        "--temperature",
        "0.2",
        "--require-dashboard-approval",
        "true",
        "--auto-apply-memory-ops",
        "false",
        "--auto-enable-skills",
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
                        model,
                        timeout_secs,
                        scheduler_tick_secs,
                        max_tokens,
                        temperature,
                        require_dashboard_approval,
                        auto_apply_memory_ops,
                        auto_enable_skills,
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
    assert_eq!(model.as_deref(), Some("gpt-test"));
    assert_eq!(timeout_secs, Some(120));
    assert_eq!(scheduler_tick_secs, Some(30));
    assert_eq!(max_tokens.as_deref(), Some("4096"));
    assert_eq!(temperature.as_deref(), Some("0.2"));
    assert_eq!(require_dashboard_approval, Some(true));
    assert_eq!(auto_apply_memory_ops, Some(false));
    assert_eq!(auto_enable_skills, Some(false));
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
fn automation_run_memory_curation_parses_manual_dry_run_flags() {
    let cli = Cli::try_parse_from([
        "tracedecay",
        "automation",
        "run",
        "memory-curation",
        "--dry-run",
        "true",
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
                            dry_run,
                            max_clusters,
                            min_confidence,
                            path,
                        }
                }
        }) if dry_run
            && max_clusters == 8
            && (min_confidence - 0.7).abs() < f64::EPSILON
            && path.as_deref() == Some("/tmp/project")
    ));
}

#[test]
fn automation_run_session_reflection_parses_manual_dry_run_flags() {
    let cli = Cli::try_parse_from([
        "tracedecay",
        "automation",
        "run",
        "session-reflection",
        "--dry-run",
        "true",
        "--provider",
        "codex",
        "--query",
        "remember decisions",
        "--evidence-limit",
        "12",
        "--storage-scope",
        "hermes_profile",
        "--hermes-home",
        "/tmp/hermes-profile",
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
                            dry_run,
                            provider,
                            query,
                            evidence_limit,
                            storage_scope,
                            hermes_home,
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
        }) if dry_run
            && provider == "codex"
            && query == "remember decisions"
            && evidence_limit == 12
            && storage_scope == "hermes_profile"
            && hermes_home.as_deref() == Some("/tmp/hermes-profile")
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
fn automation_run_skill_writing_parses_manual_dry_run_flags() {
    let cli = Cli::try_parse_from([
        "tracedecay",
        "automation",
        "run",
        "skill-writing",
        "--dry-run",
        "true",
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
                            dry_run,
                            provider,
                            query,
                            evidence_limit,
                            path,
                        }
                }
        }) if dry_run
            && provider == "cursor"
            && query == "workflow corrections"
            && evidence_limit == 9
            && path.as_deref() == Some("/tmp/project")
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
            action:
                SessionsAction::Search {
                    project_id,
                    project_path,
                    ..
                }
        }) if project_id.as_deref() == Some("proj_123") && project_path.is_none()
    ));
}

#[test]
fn migrate_commands_parse_manifest_scaffolding_flags() {
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
fn install_conflicting_profile_flags_fail_during_parse() {
    let err = match Cli::try_parse_from([
        "tracedecay",
        "install",
        "--agent",
        "hermes",
        "--profile",
        "dev",
        "--all-profiles",
    ]) {
        Ok(_) => panic!("conflicting profile flags should fail"),
        Err(err) => err,
    };

    assert_eq!(err.kind(), ErrorKind::ArgumentConflict);
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
            action:
                SessionsAction::Search {
                    query,
                    provider,
                    limit,
                    project_id,
                    project_path,
                },
        }) => {
            assert_eq!(query, "needle");
            assert_eq!(provider.as_deref(), Some("codex"));
            assert_eq!(limit, 5);
            assert!(project_id.is_none());
            assert!(project_path.is_none());
        }
        _ => panic!("expected sessions search command"),
    }

    let all_provider_search =
        Cli::try_parse_from(["tracedecay", "sessions", "search", "needle"]).unwrap();
    match all_provider_search.command {
        Some(Commands::Sessions {
            action:
                SessionsAction::Search {
                    query,
                    provider,
                    limit,
                    ..
                },
        }) => {
            assert_eq!(query, "needle");
            assert!(provider.is_none());
            assert_eq!(limit, 10);
        }
        _ => panic!("expected sessions search command"),
    }
}
