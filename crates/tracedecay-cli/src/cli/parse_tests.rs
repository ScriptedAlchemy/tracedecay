use super::{Cli, Commands, DaemonAction, ProfileStorageAction, RemoteAction};
use clap::{Parser, error::ErrorKind};

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
fn workflow_command_binds_one_closed_typed_operation() {
    let cli = Cli::try_parse_from([
        "tracedecay",
        "workflow",
        "register-definition",
        "--request-file",
        "workflow.json",
        "--project",
        "/tmp/project",
        "--json",
    ])
    .expect("Workflow command should parse");

    let Some(Commands::Workflow { invocation }) = cli.command else {
        panic!("unexpected Workflow command");
    };
    assert_eq!(
        invocation.operation,
        tracedecay_api::WorkflowOperation::RegisterDefinition
    );
    assert_eq!(
        invocation.request_file,
        std::path::Path::new("workflow.json")
    );
    assert_eq!(invocation.project.as_deref(), Some("/tmp/project"));
    assert!(invocation.json);
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
    let error = match Cli::try_parse_from(["tracedecay", "reinstall", "--local"]) {
        Ok(_) => panic!("--local without --agent must fail admission"),
        Err(error) => error,
    };
    assert_eq!(error.kind(), ErrorKind::MissingRequiredArgument);
}

#[test]
fn daemon_remote_tls_listener_requires_the_complete_triple() {
    for action in ["run", "install-service"] {
        let complete = Cli::try_parse_from([
            "tracedecay",
            "daemon",
            action,
            "--remote-listen",
            "192.0.2.10:7443",
            "--remote-tls-cert",
            "/run/tracedecay/remote.crt",
            "--remote-tls-key",
            "/run/tracedecay/remote.key",
        ])
        .unwrap_or_else(|error| panic!("complete daemon {action} TLS listener: {error}"));
        let (listen, certificate, private_key) = match complete.command {
            Some(Commands::Daemon {
                action:
                    DaemonAction::Run {
                        remote_listen: Some(listen),
                        remote_tls_cert: Some(certificate),
                        remote_tls_key: Some(private_key),
                        ..
                    }
                    | DaemonAction::InstallService {
                        remote_listen: Some(listen),
                        remote_tls_cert: Some(certificate),
                        remote_tls_key: Some(private_key),
                        ..
                    },
            }) => (listen, certificate, private_key),
            _ => panic!("daemon {action} did not bind the TLS listener"),
        };
        assert_eq!(listen.to_string(), "192.0.2.10:7443");
        assert_eq!(certificate, "/run/tracedecay/remote.crt");
        assert_eq!(private_key, "/run/tracedecay/remote.key");

        let partial = match Cli::try_parse_from([
            "tracedecay",
            "daemon",
            action,
            "--remote-listen",
            "192.0.2.10:7443",
        ]) {
            Ok(_) => panic!("partial daemon {action} TLS configuration must fail admission"),
            Err(error) => error,
        };
        assert_eq!(partial.kind(), ErrorKind::MissingRequiredArgument);
    }
}

#[test]
fn init_accepts_short_and_long_path_flag_like_dashboard_does() {
    let short = Cli::try_parse_from(["tracedecay", "init", "-p", "/tmp/project"])
        .expect("init -p PATH should parse");
    assert!(matches!(
        short.command,
        Some(Commands::Init {
            path: None,
            path_flag,
            ..
        }) if path_flag.as_deref() == Some("/tmp/project")
    ));

    let long = Cli::try_parse_from(["tracedecay", "init", "--path", "/tmp/project"])
        .expect("init --path PATH should parse");
    assert!(matches!(
        long.command,
        Some(Commands::Init {
            path: None,
            path_flag,
            ..
        }) if path_flag.as_deref() == Some("/tmp/project")
    ));

    let positional = Cli::try_parse_from(["tracedecay", "init", "/tmp/project"])
        .expect("init PATH should still parse positionally");
    assert!(matches!(
        positional.command,
        Some(Commands::Init {
            path,
            path_flag: None,
            ..
        }) if path.as_deref() == Some("/tmp/project")
    ));

    // `Cli` does not derive `Debug`, so match directly instead of
    // `.expect_err(...)`.
    let conflict =
        match Cli::try_parse_from(["tracedecay", "init", "/tmp/project", "--path", "/tmp/other"]) {
            Ok(_) => panic!("init PATH and --path together should be rejected as a conflict"),
            Err(error) => error,
        };
    assert_eq!(conflict.kind(), ErrorKind::ArgumentConflict);
}

#[test]
fn brokered_index_commands_refuse_flags_the_daemon_never_honors_at_parse_time() {
    for argv in [
        &["tracedecay", "init", "--skip-folder", "vendor"][..],
        &["tracedecay", "init", "--include-folder", "dist/generated"][..],
        &["tracedecay", "sync", "--skip-folder", "vendor"][..],
        &["tracedecay", "sync", "--include-folder", "dist/generated"][..],
        &["tracedecay", "sync", "--doctor"][..],
    ] {
        let error = match Cli::try_parse_from(argv) {
            Ok(_) => panic!("{argv:?} must be a usage error"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), ErrorKind::UnknownArgument, "{argv:?}");
        assert_eq!(error.exit_code(), 2, "{argv:?}");
    }
    assert!(matches!(
        Cli::try_parse_from(["tracedecay", "sync", "/tmp/project", "--verbose"])
            .expect("sync PATH --verbose parses")
            .command,
        Some(Commands::Sync { path, verbose: true, .. }) if path.as_deref() == Some("/tmp/project")
    ));
}

#[test]
fn bench_is_not_a_subcommand() {
    let error = match Cli::try_parse_from(["tracedecay", "bench", "--json"]) {
        Ok(_) => panic!("`tracedecay bench` must be a usage error"),
        Err(error) => error,
    };
    assert_eq!(error.kind(), ErrorKind::InvalidSubcommand);
    assert_eq!(error.exit_code(), 2);
    assert!(matches!(
        Cli::try_parse_from(["tracedecay", "gain", "--json"])
            .expect("a sibling diagnostics command parses")
            .command,
        Some(Commands::Gain { json: true, .. })
    ));
}

#[test]
fn reset_project_store_requires_one_project_selector_at_parse_time() {
    let missing = match Cli::try_parse_from(["tracedecay", "storage", "reset-project-store"]) {
        Ok(_) => panic!("reset-project-store without a selector must be a usage error"),
        Err(error) => error,
    };
    assert_eq!(missing.kind(), ErrorKind::MissingRequiredArgument);
    assert_eq!(missing.exit_code(), 2);
    for (flag, value, expected) in [
        (
            "--project-root",
            "/tmp/project",
            (Some("/tmp/project"), None),
        ),
        (
            "--project-id",
            "proj_0123456789abcdef",
            (None, Some("proj_0123456789abcdef")),
        ),
    ] {
        let parsed =
            Cli::try_parse_from(["tracedecay", "storage", "reset-project-store", flag, value])
                .expect("one selector parses")
                .command;
        let Some(Commands::Storage {
            action:
                ProfileStorageAction::ResetProjectStore {
                    project_root,
                    project_id,
                },
        }) = parsed
        else {
            panic!("{flag} must parse as reset-project-store");
        };
        assert_eq!((project_root.as_deref(), project_id.as_deref()), expected);
    }
}

#[test]
fn sessions_refresh_never_falls_back_to_the_current_directory() {
    let selectors = [
        "--session-id",
        "session.refresh",
        "--provider",
        "cursor",
        "--source",
        "4",
        "--target",
        "9",
    ];
    for (action, extra) in [
        ("begin", &[][..]),
        ("status", &["--handle", "refresh.abc"][..]),
        ("cancel", &["--handle", "refresh.abc"][..]),
    ] {
        let base: Vec<&str> = ["tracedecay", "sessions", "refresh", action]
            .into_iter()
            .chain(selectors)
            .chain(extra.iter().copied())
            .collect();
        let error = match Cli::try_parse_from(base.clone()) {
            Ok(_) => panic!("refresh {action} must require a project or profile selector"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), ErrorKind::MissingRequiredArgument);

        let mut with_project = base;
        with_project.extend(["--project-path", "/repo/tracedecay"]);
        assert!(
            Cli::try_parse_from(with_project).is_ok(),
            "refresh {action} with an explicit project selector must parse"
        );
    }
}

#[test]
fn remote_replay_parses_request_file_and_optional_trust_root() {
    let cli = Cli::try_parse_from([
        "tracedecay",
        "remote",
        "replay",
        "--endpoint",
        "https://brain.example/remote/",
        "--credential-file",
        "cred.bin",
        "--trust-root-file",
        "root.pem",
        "--timeout-secs",
        "45",
        "--request-file",
        "-",
        "--json",
    ])
    .expect("remote replay should parse");

    let Some(Commands::Remote {
        action: RemoteAction::Replay { authority },
    }) = cli.command
    else {
        panic!("unexpected remote replay command");
    };
    assert_eq!(authority.endpoint, "https://brain.example/remote/");
    assert_eq!(authority.credential_file, std::path::Path::new("cred.bin"));
    assert_eq!(
        authority.trust_root_file.as_deref(),
        Some(std::path::Path::new("root.pem"))
    );
    assert_eq!(authority.timeout_secs, 45);
    assert_eq!(authority.request_file, std::path::Path::new("-"));
    assert!(authority.json);
}

/// The `tracedecay …` invocations listed under an `Examples:` heading, with
/// `\`-continued lines joined and the trailing two-space description cut.
fn help_text_examples(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut continues = false;
    for line in text.lines() {
        let (body, next_continues) = match line.trim_end().strip_suffix('\\') {
            Some(body) => (body.trim_end(), true),
            None => (line, false),
        };
        match lines.last_mut() {
            Some(last) if continues => {
                last.push(' ');
                last.push_str(body.trim_start());
            }
            _ => lines.push(body.to_owned()),
        }
        continues = next_continues;
    }
    let mut in_examples = false;
    let mut examples = Vec::new();
    for line in &lines {
        if line.trim_end().ends_with("Examples:") {
            in_examples = true;
            continue;
        }
        in_examples &= line.starts_with("  ");
        if let Some(example) = line.trim_start().strip_prefix("tracedecay ")
            && in_examples
        {
            let invocation = example.split("  ").next().unwrap_or_default();
            examples.push(format!("tracedecay {invocation}"));
        }
    }
    examples
}

fn collect_help_examples(
    command: &clap::Command,
    path: &mut Vec<String>,
    found: &mut Vec<(String, String)>,
) {
    path.push(command.get_name().to_owned());
    for help in [command.get_after_help(), command.get_after_long_help()]
        .into_iter()
        .flatten()
    {
        for example in help_text_examples(&help.to_string()) {
            found.push((path.join(" "), example));
        }
    }
    for subcommand in command.get_subcommands() {
        collect_help_examples(subcommand, path, found);
    }
    path.pop();
}

fn parser_rejection(owner: &str, example: &str) -> Option<String> {
    let argv = shell_words::split(example).expect("example is shell-quoted");
    let error = Cli::try_parse_from(&argv).err()?;
    match error.kind() {
        ErrorKind::DisplayHelp
        | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
        | ErrorKind::DisplayVersion => None,
        kind => Some(format!("{owner}: `{example}`: {kind}")),
    }
}

#[test]
fn every_help_example_is_accepted_by_the_parser() {
    let mut found = Vec::new();
    collect_help_examples(
        &<Cli as clap::CommandFactory>::command(),
        &mut Vec::new(),
        &mut found,
    );
    assert!(
        found
            .iter()
            .any(|(owner, example)| owner == "tracedecay work"
                && example == "tracedecay work create --request-file create.json --json"),
        "{found:#?}"
    );

    let rejected: Vec<String> = found
        .iter()
        .filter_map(|(owner, example)| parser_rejection(owner, example))
        .collect();
    assert_eq!(rejected, Vec::<String>::new());
}
