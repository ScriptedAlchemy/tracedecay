use super::{
    Cli, CommandOutcome, CommandRuntime, HostBundleCliOptions, command_runtime, dispatch_command,
    run_startup_preamble,
};
use clap::Parser;
use std::iter;
use tracedecay_dashboard_api::cloud::ReleaseLookupError;
use tracedecay_domain::errors::Result;
use tracedecay_runtime_core::config::ProfileRoot;
use tracedecay_runtime_core::native_runtime;

/// Runs one command the way `run` does, on a native root with no Tokio
/// runtime in the process.
fn run_on_native_root(profile: &ProfileRoot, args: &[&str]) -> Result<CommandOutcome> {
    let cli = Cli::try_parse_from(iter::once("tracedecay").chain(args.iter().copied()))
        .expect("command must parse");
    let host_bundle = HostBundleCliOptions {
        component: cli.component,
        dry_run: cli.dry_run,
        yes: cli.yes,
        adopt: cli.adopt,
    };
    let command = cli.command.expect("subcommand must be present");
    assert_eq!(
        command_runtime(Some(&command)),
        CommandRuntime::Native,
        "{args:?}"
    );
    native_runtime::block_on(Box::pin(async {
        assert!(
            tokio::runtime::Handle::try_current().is_err(),
            "the native root must run without Tokio"
        );
        run_startup_preamble(profile, &command).await;
        dispatch_command(profile, command, host_bundle).await
    }))
    .expect("native root")
}

#[test]
fn client_families_complete_on_the_native_root_without_tokio() {
    crate::product_runtime::register_for_tests();
    let home = tempfile::tempdir().expect("temp home");
    let profile = ProfileRoot::under_home(home.path());
    std::fs::create_dir_all(
        tracedecay_automation_runtime::automation::managed_skills::managed_skill_root(
            profile.data_dir(),
        ),
    )
    .expect("managed skill root");
    let tokio_misses = native_runtime::tokio_runtime_missing_total();
    let parked_writer_waits = native_runtime::native_thread_writer_waits();

    for args in [
        &["tool"][..],
        &["channel"],
        &["automation", "skills", "list"],
    ] {
        let outcome = run_on_native_root(&profile, args);
        assert!(
            matches!(outcome, Ok(CommandOutcome::Success)),
            "{args:?}: {outcome:?}"
        );
    }
    for args in [&["status", "--json"][..], &["current-counter"], &["gain"]] {
        let error = run_on_native_root(&profile, args).expect_err("no daemon is running");
        assert!(
            tracedecay_daemon_identity::daemon_unreachable(&error),
            "{args:?}: {error}"
        );
    }
    let error = run_on_native_root(
        &profile,
        &["feedback-rollback", "dry-run", "--agent", "claude"],
    )
    .expect_err("no receipt to roll back");
    assert!(
        error.to_string().contains("no aggregate host receipt"),
        "{error}"
    );

    let doctor = native_runtime::block_on(Box::pin(tracedecay::doctor::run_doctor(
        &profile,
        tracedecay::doctor::AdmittedDoctorNetworkProbes {
            fetch_worldwide_total: || None,
            fetch_latest_version: || {
                Err(ReleaseLookupError::NetworkUnreachable {
                    detail: "offline test".to_owned(),
                })
            },
        },
        true,
    )))
    .expect("native root");
    assert!(doctor.is_ok(), "{doctor:?}");

    assert_eq!(
        native_runtime::tokio_runtime_missing_total(),
        tokio_misses,
        "a client command skipped work that needed Tokio"
    );
    assert_eq!(
        native_runtime::native_thread_writer_waits(),
        parked_writer_waits,
        "a client command parked a native worker on a storage write"
    );
}

#[test]
fn project_and_runtime_families_complete_on_the_native_root_without_tokio() {
    crate::product_runtime::register_for_tests();
    let home = tempfile::tempdir().expect("temp home");
    let profile = ProfileRoot::under_home(home.path());
    let project = tempfile::tempdir().expect("temp project");
    let project_arg = project.path().to_str().expect("UTF-8 project path");
    let work_request = project.path().join("work-request.json");
    std::fs::write(&work_request, r#"{"page_size":10}"#).expect("work request");
    let workflow_request = project.path().join("workflow-request.json");
    std::fs::write(&workflow_request, "{}").expect("workflow request");
    let work_request = work_request.to_str().expect("UTF-8 request path");
    let workflow_request = workflow_request.to_str().expect("UTF-8 request path");
    let tokio_misses = native_runtime::tokio_runtime_missing_total();
    let parked_writer_waits = native_runtime::native_thread_writer_waits();

    for args in [&["storage", "report"][..], &["lsp", "servers"]] {
        let outcome = run_on_native_root(&profile, args);
        assert!(
            matches!(outcome, Ok(CommandOutcome::Success)),
            "{args:?}: {outcome:?}"
        );
    }
    for args in [
        &["sync", project_arg][..],
        &["projects", "list"],
        &["branch", "list", "--path", project_arg],
        &["memory", "status", "--path", project_arg],
        &["list", "--all"],
        &[
            "work",
            "list-attempts",
            "--request-file",
            work_request,
            "--project",
            project_arg,
        ],
        &[
            "workflow",
            "list-definitions",
            "--request-file",
            workflow_request,
            "--project",
            project_arg,
        ],
        &["remote", "status"],
        &["dashboard", "--path", project_arg],
        &["serve", "--path", project_arg],
    ] {
        let error = run_on_native_root(&profile, args).expect_err("no daemon is running");
        assert!(
            tracedecay_daemon_identity::daemon_unreachable(&error),
            "{args:?}: {error}"
        );
    }
    let init = run_on_native_root(&profile, &["init", project_arg]).expect_err("no daemon");
    assert_eq!(
        init.project_route_context().map(|(code, _, _)| code),
        Some("code_index_scheduler_unavailable"),
        "{init}"
    );
    let stale = run_on_native_root(&profile, &["wipe", "--stale", "--yes"])
        .expect_err("stale stores come from the daemon");
    assert!(
        stale.to_string().contains("from the running daemon"),
        "{stale}"
    );
    std::fs::create_dir_all(profile.data_dir()).expect("profile root");
    let wipe = run_on_native_root(&profile, &["wipe", "--all", "--yes"]);
    assert!(matches!(wipe, Ok(CommandOutcome::Success)), "{wipe:?}");
    native_runtime::block_on(Box::pin(async {
        assert!(tokio::runtime::Handle::try_current().is_err());
        crate::commands::handle_no_command(&profile).await
    }))
    .expect("native root")
    .expect("bare tracedecay");

    assert_eq!(
        native_runtime::tokio_runtime_missing_total(),
        tokio_misses,
        "a project or runtime command skipped work that needed Tokio"
    );
    assert_eq!(
        native_runtime::native_thread_writer_waits(),
        parked_writer_waits,
        "a project or runtime command parked a native worker on a storage write"
    );
}
