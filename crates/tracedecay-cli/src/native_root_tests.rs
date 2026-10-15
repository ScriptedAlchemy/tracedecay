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
    let command = Cli::try_parse_from(iter::once("tracedecay").chain(args.iter().copied()))
        .expect("command must parse")
        .command
        .expect("subcommand must be present");
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
        dispatch_command(profile, command, HostBundleCliOptions::default()).await
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
