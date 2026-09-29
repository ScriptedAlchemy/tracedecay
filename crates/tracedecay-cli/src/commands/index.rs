use std::path::{Path, PathBuf};
use tracedecay_runtime_core::config::ProfileRoot;

use tracedecay_project::project::TraceDecay;

use tracedecay_contracts::graph_tool::GraphToolResultV1;
use tracedecay_contracts::retrieval::{
    AdminCliRegistryEmptyV1, AdminCliResultV1, AdminCliSurfaceRequestV1, AdminSyncAdmissionV1,
    AdminSyncResultV1,
};
use tracedecay_tool_catalog::ApplicationSurfaceOperation;

use super::daemon::admin_cli_result;

/// True when the profile registry has zero registered projects, i.e. the user
/// has not run `tracedecay init` anywhere yet. An unreadable registry is not
/// a fresh install: the offer to initialize is only made on a known-empty one.
async fn is_fresh_install(profile: &ProfileRoot) -> bool {
    matches!(
        admin_cli_result(profile, None, AdminCliSurfaceRequestV1::RegistryEmpty {}).await,
        Ok(AdminCliResultV1::RegistryEmpty(AdminCliRegistryEmptyV1 {
            empty: true
        }))
    )
}

/// When invoked with no subcommand, offer to create the index if none exists.
pub(crate) async fn handle_no_command(
    profile: &ProfileRoot,
) -> tracedecay_domain::errors::Result<()> {
    let project_path = tracedecay_configuration::resolve_path(None);
    if TraceDecay::has_initialized_store_with_options(
        &project_path,
        &tracedecay_project::project::TraceDecayOpenOptions::for_profile(profile),
    )
    .await
    {
        // Already initialized, show help via clap
        let _ = <crate::cli::Cli as clap::CommandFactory>::command().print_help();
        eprintln!();
        return Ok(());
    }
    if is_fresh_install(profile).await {
        eprintln!("\x1b[1;36mWelcome to tracedecay!\x1b[0m");
        eprintln!(
            "Looks like a new installation. To get started, run \x1b[1mtracedecay init\x1b[0m \
             in your project root."
        );
        eprintln!();
    }
    // Bare `tracedecay` is ambiguous (help vs init) and creating a store here
    // is how phantom indexes used to appear. Present the next command and
    // return; do not read stdin on a terminal or a pipe.
    eprintln!(
        "No TraceDecay index found at '{}'. Skipping index creation (run `tracedecay init`).",
        project_path.display()
    );
    Ok(())
}

#[hotpath::measure(label = "cli.init.run", future = true)]
pub(crate) async fn handle_init(
    profile: &ProfileRoot,
    path: Option<String>,
    adopt_project: Option<String>,
    fresh: bool,
    assume_yes: bool,
) -> tracedecay_domain::errors::Result<()> {
    let project_path = tracedecay_configuration::resolve_path(path);
    let profile_root = profile.data_dir().to_path_buf();
    if let Some(message) =
        tracedecay_global_db::ephemeral_root_rejection(&project_path, &profile_root)
    {
        return Err(tracedecay_domain::errors::TraceDecayError::Config { message });
    }
    let adoption = moved_store_adoption_request(adopt_project, fresh, assume_yes)?;
    let mut handshake = tracedecay::daemon::handshake_for_current_client(
        profile,
        Some(project_path.clone()),
        None,
        false,
        true,
    )?;
    handshake.moved_store_adoption = adoption;
    let daemon_available = init_daemon_available(profile);

    let project_path_for_remedy = project_path.clone();
    handle_init_with_daemon_availability(profile, project_path, handshake, daemon_available)
        .await
        .map_err(|error| annotate_reset_required_init_error(error, &project_path_for_remedy))
}

/// Whether a daemon is accepting connections for this profile.
///
/// A connectable endpoint is the whole precondition: `brokered_init` carries
/// its own 120 s bootstrap deadline, so a daemon that has not finished
/// answering initialize within the one-second reachability probe is still the
/// daemon this init must broker through. Requiring the identity proof here
/// refused cold starts on CPU-constrained hosts and told the operator to start
/// a daemon that was already running.
///
/// This resolves through the daemon-control authority on every platform rather
/// than a `cfg` split. The unix socket and the Windows loopback authority are
/// both behind `daemon_socket_connectable`, so assuming availability wherever
/// the transport differs would let init proceed on Windows without the
/// scheduler it then requires.
fn init_daemon_available(profile: &ProfileRoot) -> bool {
    tracedecay_daemon_control::daemon_socket_connectable(profile)
}

/// Maps explicit `tracedecay init` flags to the adoption request the daemon
/// honors. Only init escalates past `Never`: `--adopt-project` names the
/// project, bare `--yes` confirms a unique candidate, `--fresh` opts out of
/// adoption entirely, and no flags means candidates are offered in a typed
/// refusal instead of silently remapped.
fn moved_store_adoption_request(
    adopt_project: Option<String>,
    fresh: bool,
    assume_yes: bool,
) -> tracedecay_domain::errors::Result<tracedecay_project::project::MovedStoreAdoption> {
    if fresh && adopt_project.is_some() {
        return Err(tracedecay_domain::errors::TraceDecayError::Config {
            message: "--fresh mints a new project identity and contradicts --adopt-project; \
                      pass exactly one"
                .to_owned(),
        });
    }
    Ok(match (adopt_project, fresh, assume_yes) {
        (Some(project_id), _, _) => {
            tracedecay_project::project::MovedStoreAdoption::AdoptNamed(project_id)
        }
        (None, true, _) => tracedecay_project::project::MovedStoreAdoption::Never,
        (None, false, true) => tracedecay_project::project::MovedStoreAdoption::AdoptUnique,
        (None, false, false) => tracedecay_project::project::MovedStoreAdoption::OfferCandidates,
    })
}

/// A refused store surfaces from init as the typed ResetRequired state. The
/// remedy is the scoped operator reset, not manual directory removal, so init
/// names the exact command for this project instead of leaving the raw
/// refusal as the last word.
fn annotate_reset_required_init_error(
    error: tracedecay_domain::errors::TraceDecayError,
    project_path: &Path,
) -> tracedecay_domain::errors::TraceDecayError {
    if error.reset_required_context().is_none() {
        return error;
    }
    let project_path = project_path.to_string_lossy();
    let reset_command = shell_words::join([
        "tracedecay",
        "storage",
        "reset-project-store",
        "--project-root",
        project_path.as_ref(),
        "--yes",
    ]);
    let init_command = shell_words::join(["tracedecay", "init", project_path.as_ref()]);
    tracedecay_domain::errors::TraceDecayError::Config {
        message: format!(
            "{error}\n\nthis store cannot be opened until it is reset; run:\n  \
             {reset_command}\n\
             then re-run `{init_command}`, sessions re-ingest from the \
             preserved transcripts"
        ),
    }
}

async fn handle_init_with_daemon_availability(
    profile: &ProfileRoot,
    project_path: PathBuf,
    handshake: tracedecay_daemon_protocol::DaemonHandshake,
    daemon_available: bool,
) -> tracedecay_domain::errors::Result<()> {
    if daemon_available {
        return brokered_init(profile, &project_path, &handshake).await;
    }
    Err(tracedecay_domain::errors::TraceDecayError::project_route(
        "code_index_scheduler_unavailable",
        true,
        "project initialization requires the daemon-owned code-index scheduler; start the daemon and retry",
    ))
}

async fn brokered_init(
    profile: &ProfileRoot,
    project_path: &Path,
    handshake: &tracedecay_daemon_protocol::DaemonHandshake,
) -> tracedecay_domain::errors::Result<()> {
    // Init deliberately triggers a cold project open behind this single
    // status call. The default warming-retry grace is far tighter than a cold
    // open can take on a debug build or slow shared runner, which surfaced as
    // "daemon tracedecay_status timed out during read before deadline" failures
    // in CI. Give the bootstrap a generous budget so the client waits out the
    // background open instead of abandoning it just before it completes.
    let init_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(120);
    tracedecay::daemon::call_default_tool_awaiting_project_open(
        profile,
        handshake,
        "tracedecay_status",
        serde_json::json!({"format": "json", "admission_only": true}),
        init_deadline,
    )
    .await?;
    // Admission alone does not start indexing: the code-index scheduler mounts
    // on first demand, and the background full-server upgrade that would
    // eventually demand it does not survive a daemon restart. Request the
    // reconciliation explicitly so the message below reports something that
    // actually happened.
    let reconcile = admin_sync(profile, handshake.clone(), init_deadline).await;
    let reconcile = match reconcile {
        Err(error) => {
            if !code_index_reconciliation_is_optional(project_path, &error).await {
                return Err(error);
            }
            eprintln!(
                "initialized {}; code indexing is unavailable for this non-Git project",
                project_path.display()
            );
            return Ok(());
        }
        Ok(reconcile) => reconcile,
    };
    match reconcile.status {
        // `queued` means the daemon accepted the reconcile demand into its
        // pre-mount queue. Init's confirmation names that request
        // (`requested`), not the internal queue noun.
        AdminSyncAdmissionV1::Queued => eprintln!(
            "initialized {}; daemon code-index reconciliation requested",
            project_path.display()
        ),
        AdminSyncAdmissionV1::NotApplicable => eprintln!(
            "initialized {}; code indexing does not apply to this non-Git project",
            project_path.display()
        ),
    }
    Ok(())
}

/// Asks the project's owner for the operator's code-index reconcile.
async fn admin_sync(
    profile: &ProfileRoot,
    handshake: tracedecay_daemon_protocol::DaemonHandshake,
    deadline: tokio::time::Instant,
) -> tracedecay_domain::errors::Result<AdminSyncResultV1> {
    match crate::tool_command::owner_operation_result(
        profile,
        handshake,
        ApplicationSurfaceOperation::AdminSync,
        serde_json::json!({}),
        deadline,
    )
    .await?
    {
        GraphToolResultV1::AdminSync(result) => Ok(result),
        _ => Err(tracedecay_domain::errors::TraceDecayError::project_route(
            "owner_result_mismatch",
            false,
            "the project owner answered tracedecay_admin_sync with another operation's result",
        )),
    }
}

async fn code_index_reconciliation_is_optional(
    project_path: &Path,
    error: &tracedecay_domain::errors::TraceDecayError,
) -> bool {
    if !error
        .to_string()
        .contains("code_index_scheduler_unavailable")
    {
        return false;
    }
    let deadline = tracedecay_runtime_core::cancellation::MonotonicDeadline::at(
        std::time::Instant::now() + std::time::Duration::from_secs(2),
    );
    matches!(
        tracedecay_runtime_core::git_discovery::discover_repository_identity(
            project_path,
            deadline,
            &tracedecay_runtime_core::cancellation::CancellationToken::new(),
        )
        .await,
        tracedecay_runtime_core::git_discovery::GitRepositoryIdentityOutcome::NotRepository
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod daemon_precondition_tests {
    use std::path::Path;

    pub(super) struct SocketEnvGuard {
        previous: Option<std::ffi::OsString>,
    }

    impl SocketEnvGuard {
        pub(super) fn set(value: &Path) -> Self {
            let previous = std::env::var_os(tracedecay_daemon_protocol::SOCKET_ENV);
            unsafe {
                std::env::set_var(tracedecay_daemon_protocol::SOCKET_ENV, value);
            }
            Self { previous }
        }
    }

    impl Drop for SocketEnvGuard {
        fn drop(&mut self) {
            unsafe {
                match self.previous.take() {
                    Some(previous) => {
                        std::env::set_var(tracedecay_daemon_protocol::SOCKET_ENV, previous);
                    }
                    None => std::env::remove_var(tracedecay_daemon_protocol::SOCKET_ENV),
                }
            }
        }
    }

    /// Init's daemon precondition is a probe on every platform, not a `cfg`.
    ///
    /// This test is deliberately not gated to unix. On Windows the endpoint is
    /// the loopback authority rather than a socket file, and a profile that
    /// has no authority record has no daemon to broker through, so the answer
    /// must be `false` there exactly as it is on unix. Hardcoding availability
    /// off-unix let init run past the scheduler it then requires, and that
    /// regression is only observable from a test the Windows shard compiles.
    #[test]
    fn init_daemon_availability_is_probed_on_every_platform() {
        let profile = tempfile::TempDir::new().expect("temp profile");
        let _socket = SocketEnvGuard::set(&profile.path().join("absent.sock"));

        assert!(
            !super::init_daemon_available(&tracedecay_runtime_core::config::ProfileRoot::new(
                profile.path()
            )),
            "an endpoint with no listener must not count as an available daemon"
        );
    }
}

#[cfg(all(test, unix))]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod init_bootstrap_tests {
    use super::*;

    fn test_handshake(
        project_path: &Path,
        profile_root: &Path,
    ) -> tracedecay_daemon_protocol::DaemonHandshake {
        tracedecay_daemon_protocol::DaemonHandshake {
            project_path: Some(project_path.to_path_buf()),
            scope_prefix: None,
            timings: false,
            allow_init: true,
            allow_initialize_root_routing: false,
            client_identity: tracedecay_daemon_protocol::DaemonClientIdentity {
                profile_root: profile_root.to_path_buf(),
                global_db_path: profile_root.join("global.db"),
            },
            client_version: env!("CARGO_PKG_VERSION").to_string(),
            client_instance_id: "commands-init-test".to_string(),
            tool_list_changed_capable: false,
            catalog_version: String::new(),
            moved_store_adoption: tracedecay_project::project::MovedStoreAdoption::Never,
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn daemonless_init_refuses_without_the_code_index_scheduler() {
        let temp = tempfile::TempDir::new().unwrap();
        let project = temp.path().join("project");
        let profile = temp.path().join("profile");
        std::fs::create_dir_all(&project).unwrap();
        let handshake = test_handshake(&project, &profile);

        let error = handle_init_with_daemon_availability(
            &ProfileRoot::new(&profile),
            project.clone(),
            handshake,
            false,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(
                error.project_route_context(),
                Some(("code_index_scheduler_unavailable", true, _))
            ),
            "unexpected daemonless init error: {error}"
        );
        assert!(
            !profile.exists(),
            "scheduler refusal must not initialize a local store"
        );
    }

    /// A worktree parked on a corrupt publication authority refuses the
    /// reconcile; the operator reads its cause and remedy as whole fields,
    /// not folded into one sentence cut at the diagnostic bound.
    #[test]
    fn a_parked_admin_sync_refusal_prints_as_fields() {
        let parked = tracedecay_contracts::code_index_freshness::CodeIndexConvergenceParkedV1 {
            reason: format!("the publication authority is corrupt: {}", "x".repeat(600)),
            blocked_reason: None,
            remediation: "run `tracedecay daemon restart`".to_owned(),
            parked_at_micros: 1,
            observed_passes: 1,
            retries_on_wake: false,
        };
        let detail = parked
            .publication_authority_corrupt_error()
            .project_route_typed_detail()
            .cloned()
            .expect("a parked refusal carries its typed detail");
        let refusal = tracedecay_mcp::application_output::tool_result::ApplicationRefusal {
            operation: ApplicationSurfaceOperation::AdminSync,
            binding_id: tracedecay_tool_catalog::BindingId::new("binding.cli.admin_sync.v1")
                .unwrap(),
            problem: tracedecay_contracts::ApplicationProblemEnvelope::new(
                tracedecay_contracts::ResultContractRef::new(
                    tracedecay_tool_catalog::SchemaId::new(
                        "schema.application.primitive.admin-sync.result",
                    )
                    .unwrap(),
                    1,
                )
                .unwrap(),
                tracedecay_contracts::RequestId::new("request.cli.admin-sync").unwrap(),
                tracedecay_contracts::ApplicationProblem::from_detail(detail),
            )
            .unwrap(),
        };

        assert_eq!(
            crate::commands::process_error_text(refusal.into_error()),
            format!(
                "project route error (application.code-index.parked)\n\
                 Parked cause: the publication authority is corrupt: {}\n\
                 Parked remedy: run `tracedecay daemon restart`\n\
                 Retries on wake: false",
                "x".repeat(600)
            )
        );
    }

    #[tokio::test]
    async fn only_non_git_scheduler_unavailability_is_optional_during_init() {
        let temp = tempfile::TempDir::new().unwrap();
        let non_git = temp.path().join("non-git");
        let git = temp.path().join("git");
        let nested_git = git.join("nested");
        std::fs::create_dir_all(&non_git).unwrap();
        std::fs::create_dir_all(&nested_git).unwrap();
        let initialized = std::process::Command::new(
            tracedecay_runtime_core::git::try_git_program()
                .expect("absolute git executable should resolve"),
        )
        .args(["init", "-q"])
        .current_dir(&git)
        .status()
        .unwrap();
        assert!(initialized.success());
        let unavailable = tracedecay_domain::errors::TraceDecayError::Config {
            message: "daemon tool call failed: code_index_scheduler_unavailable: admin sync was not accepted"
                .to_string(),
        };
        let unrelated = tracedecay_domain::errors::TraceDecayError::Config {
            message: "daemon tool call failed: storage unavailable".to_string(),
        };

        assert!(code_index_reconciliation_is_optional(&non_git, &unavailable).await);
        assert!(!code_index_reconciliation_is_optional(&git, &unavailable).await);
        assert!(!code_index_reconciliation_is_optional(&nested_git, &unavailable).await);
        assert!(!code_index_reconciliation_is_optional(&non_git, &unrelated).await);
    }

    #[test]
    fn reset_remedy_commands_survive_paths_with_spaces_and_apostrophes() {
        let error = annotate_reset_required_init_error(
            tracedecay_domain::errors::TraceDecayError::reset_required(
                "project-fixture",
                "incompatible persisted shape",
            ),
            Path::new("/repo/it's an example"),
        );

        let text = error.to_string();
        assert!(text.contains("this store cannot be opened until it is reset"));
        let reset_command = text
            .split_once("run:\n  ")
            .expect("reset command must be named")
            .1;
        let (reset_command, then_part) = reset_command
            .split_once("\n")
            .expect("reset command ends before the init remedy");
        assert_eq!(
            shell_words::split(reset_command).unwrap(),
            [
                "tracedecay",
                "storage",
                "reset-project-store",
                "--project-root",
                "/repo/it's an example",
                "--yes",
            ]
        );
        let init_command = then_part
            .split_once("`tracedecay init ")
            .map(|(_, tail)| tail)
            .and_then(|tail| tail.split_once('`').map(|(command, _)| command))
            .expect("init remedy must be named");
        assert_eq!(
            shell_words::split(&format!("tracedecay init {init_command}")).unwrap(),
            ["tracedecay", "init", "/repo/it's an example"]
        );
    }
}

#[hotpath::measure(label = "cli.sync.run", future = true)]
pub(crate) async fn handle_sync(
    profile: &ProfileRoot,
    path: Option<String>,
    verbose: bool,
) -> tracedecay_domain::errors::Result<()> {
    let resolved = super::scope::resolve_project_scope(
        profile,
        tracedecay_configuration::resolve_path_with_discovery(profile, path),
    )
    .await?;
    let handshake = super::daemon::client_handshake(profile, Some(&resolved.project_path))?;
    let deadline = tokio::time::Instant::now() + crate::tool_command::tool_command_deadline()?;
    let result = admin_sync(profile, handshake, deadline).await?;
    if verbose {
        eprintln!("{}", serde_json::to_string_pretty(&result)?);
    }
    match result.status {
        AdminSyncAdmissionV1::Queued => eprintln!(
            "code-index reconciliation queued via daemon for {}",
            resolved.project_path.display()
        ),
        AdminSyncAdmissionV1::NotApplicable => eprintln!(
            "code indexing does not apply to {}",
            resolved.project_path.display()
        ),
    }
    Ok(())
}
