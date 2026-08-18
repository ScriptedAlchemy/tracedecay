use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

use tracedecay::tracedecay::TraceDecay;

use super::daemon::daemon_tool_json;

/// True when the global DB has zero registered projects (or can't be opened
/// at all) — i.e. the user has not run `tracedecay init` anywhere yet.
async fn is_fresh_install() -> bool {
    daemon_tool_json(
        None,
        "tracedecay_admin_cli",
        serde_json::json!({ "action": "registry_empty" }),
    )
    .await
    .ok()
    .and_then(|value| value.get("empty").and_then(serde_json::Value::as_bool))
    .unwrap_or(false)
}

/// When invoked with no subcommand, offer to create the index if none exists.
pub(crate) async fn handle_no_command() -> tracedecay::errors::Result<()> {
    let project_path = tracedecay::config::resolve_path(None);
    if TraceDecay::has_initialized_store(&project_path).await {
        // Already initialized — show help via clap
        let _ = <crate::cli::Cli as clap::CommandFactory>::command().print_help();
        eprintln!();
        return Ok(());
    }
    if is_fresh_install().await {
        eprintln!("\x1b[1;36mWelcome to tracedecay!\x1b[0m");
        eprintln!(
            "Looks like a new installation. To get started, run \x1b[1mtracedecay init\x1b[0m \
             in your project root."
        );
        eprintln!();
    }
    if !io::stdin().is_terminal() {
        eprintln!(
            "No TraceDecay index found at '{}'. Non-interactive: skipping index creation (run `tracedecay init`).",
            project_path.display()
        );
        return Ok(());
    }
    eprint!(
        "No TraceDecay index found at '{}'. Create one now? [Y/n] ",
        project_path.display()
    );
    io::stderr().flush().ok();
    let mut answer = String::new();
    io::stdin().lock().read_line(&mut answer).map_err(|e| {
        tracedecay::errors::TraceDecayError::Config {
            message: format!("failed to read stdin: {}", e),
        }
    })?;
    let answer = answer.trim();
    if answer.is_empty() || answer.eq_ignore_ascii_case("y") {
        handle_init(
            Some(project_path.to_string_lossy().into_owned()),
            Vec::new(),
            Vec::new(),
            None,
            false,
            false,
        )
        .await?;
    }
    Ok(())
}

pub(crate) async fn handle_init(
    path: Option<String>,
    skip_folders: Vec<String>,
    include_folders: Vec<String>,
    adopt_project: Option<String>,
    fresh: bool,
    assume_yes: bool,
) -> tracedecay::errors::Result<()> {
    let project_path = tracedecay::config::resolve_path(path);
    let profile_root = tracedecay::storage::default_profile_root()?;
    if let Some(message) =
        tracedecay::project_registry::ephemeral_root_rejection(&project_path, &profile_root)
    {
        return Err(tracedecay::errors::TraceDecayError::Config { message });
    }
    let adoption = moved_store_adoption_request(adopt_project, fresh, assume_yes)?;
    let mut handshake = tracedecay::daemon::DaemonHandshake::for_current_client(
        Some(project_path.clone()),
        None,
        false,
        true,
    )?;
    handshake.moved_store_adoption = adoption;
    #[cfg(unix)]
    let daemon_available = tracedecay::daemon::daemon_reachable();
    #[cfg(not(unix))]
    let daemon_available = true;

    let project_path_for_remedy = project_path.clone();
    handle_init_with_daemon_availability(
        project_path,
        skip_folders,
        include_folders,
        handshake,
        daemon_available,
    )
    .await
    .map_err(|error| annotate_reset_required_init_error(error, &project_path_for_remedy))
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
) -> tracedecay::errors::Result<tracedecay::tracedecay::MovedStoreAdoption> {
    if fresh && adopt_project.is_some() {
        return Err(tracedecay::errors::TraceDecayError::Config {
            message: "--fresh mints a new project identity and contradicts --adopt-project; \
                      pass exactly one"
                .to_owned(),
        });
    }
    Ok(match (adopt_project, fresh, assume_yes) {
        (Some(project_id), _, _) => {
            tracedecay::tracedecay::MovedStoreAdoption::AdoptNamed(project_id)
        }
        (None, true, _) => tracedecay::tracedecay::MovedStoreAdoption::Never,
        (None, false, true) => tracedecay::tracedecay::MovedStoreAdoption::AdoptUnique,
        (None, false, false) => tracedecay::tracedecay::MovedStoreAdoption::OfferCandidates,
    })
}

/// A refused store surfaces from init as the typed ResetRequired state. The
/// remedy is the scoped operator reset, not manual directory removal, so init
/// names the exact command for this project instead of leaving the raw
/// refusal as the last word.
fn annotate_reset_required_init_error(
    error: tracedecay::errors::TraceDecayError,
    project_path: &Path,
) -> tracedecay::errors::TraceDecayError {
    let is_reset_required = match &error {
        tracedecay::errors::TraceDecayError::ResetRequired { .. } => true,
        // Daemon-brokered opens serialize the typed state over JSON-RPC; the
        // schema-shape refusal text is the stable marker that survives it.
        other => other.to_string().contains("shape this binary creates"),
    };
    if !is_reset_required {
        return error;
    }
    tracedecay::errors::TraceDecayError::Config {
        message: format!(
            "{error}\n\nthis store cannot be opened until it is reset; run:\n  \
             tracedecay storage reset-project-store --project-root {} --yes\n\
             then re-run `tracedecay init {}` — sessions re-ingest from the \
             preserved transcripts",
            project_path.display(),
            project_path.display()
        ),
    }
}

async fn handle_init_with_daemon_availability(
    project_path: PathBuf,
    skip_folders: Vec<String>,
    include_folders: Vec<String>,
    handshake: tracedecay::daemon::DaemonHandshake,
    daemon_available: bool,
) -> tracedecay::errors::Result<()> {
    if daemon_available {
        return brokered_init(&project_path, &skip_folders, &include_folders, &handshake).await;
    }
    Err(tracedecay::errors::TraceDecayError::project_route(
        "code_index_scheduler_unavailable",
        true,
        "project initialization requires the daemon-owned code-index scheduler; start the daemon and retry",
    ))
}

async fn brokered_init(
    project_path: &Path,
    skip_folders: &[String],
    include_folders: &[String],
    handshake: &tracedecay::daemon::DaemonHandshake,
) -> tracedecay::errors::Result<()> {
    if !skip_folders.is_empty() || !include_folders.is_empty() {
        return Err(tracedecay::errors::TraceDecayError::Config {
            message: "brokered init does not yet support --skip-folders/--include-folders; configure tracedecay.toml first".to_string(),
        });
    }
    // Init deliberately triggers a cold project open behind this single
    // status call. The default warming-retry grace is far tighter than a cold
    // open can take on a debug build or slow shared runner, which surfaced as
    // "daemon tracedecay_status timed out during read before deadline" failures
    // in CI. Give the bootstrap a generous budget so the client waits out the
    // background open instead of abandoning it just before it completes.
    let init_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(120);
    tracedecay::daemon::call_default_tool_awaiting_project_open(
        handshake,
        "tracedecay_status",
        serde_json::json!({"format": "json", "admission_only": true}),
        init_deadline,
    )
    .await?;
    eprintln!(
        "initialized {}; daemon code-index reconciliation requested",
        project_path.display()
    );
    Ok(())
}

#[cfg(all(test, unix))]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod init_bootstrap_tests {
    use super::*;

    fn test_handshake(
        project_path: &Path,
        profile_root: &Path,
    ) -> tracedecay::daemon::DaemonHandshake {
        tracedecay::daemon::DaemonHandshake {
            project_path: Some(project_path.to_path_buf()),
            scope_prefix: None,
            timings: false,
            allow_init: true,
            allow_initialize_root_routing: false,
            client_identity: tracedecay::client_identity::DaemonClientIdentity {
                profile_root: profile_root.to_path_buf(),
                global_db_path: profile_root.join("global.db"),
            },
            client_version: env!("CARGO_PKG_VERSION").to_string(),
            client_instance_id: "commands-init-test".to_string(),
            tool_list_changed_capable: false,
            catalog_version: String::new(),
            moved_store_adoption: tracedecay::tracedecay::MovedStoreAdoption::Never,
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
            project.clone(),
            Vec::new(),
            Vec::new(),
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

    #[tokio::test(flavor = "current_thread")]
    async fn brokered_init_retains_folder_option_error_before_sending_request() {
        let temp = tempfile::TempDir::new().unwrap();
        let project = temp.path().join("project");
        let profile = temp.path().join("profile");
        std::fs::create_dir_all(&project).unwrap();
        let handshake = test_handshake(&project, &profile);

        let error = handle_init_with_daemon_availability(
            project,
            vec!["generated".to_string()],
            Vec::new(),
            handshake,
            true,
        )
        .await
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("brokered init does not yet support --skip-folders/--include-folders"),
            "unexpected brokered-init error: {error}"
        );
        assert!(
            !profile.exists(),
            "brokered rejection must not open a local store"
        );
    }
}

pub(crate) async fn handle_sync(
    path: Option<String>,
    force: bool,
    skip_folders: Vec<String>,
    include_folders: Vec<String>,
    doctor: bool,
    verbose: bool,
) -> tracedecay::errors::Result<()> {
    if !skip_folders.is_empty() || !include_folders.is_empty() {
        return Err(tracedecay::errors::TraceDecayError::Config {
            message: "brokered sync does not yet support --skip-folders/--include-folders; update tracedecay.toml first".to_string(),
        });
    }
    let resolved =
        super::scope::resolve_project_scope(tracedecay::config::resolve_path_with_discovery(path))
            .await?;
    let handshake = tracedecay::daemon::DaemonHandshake::for_current_client(
        Some(resolved.project_path.clone()),
        None,
        false,
        false,
    )?;
    let result = tracedecay::daemon::call_default_tool(
        &handshake,
        "tracedecay_admin_sync",
        serde_json::json!({"force": force}),
    )
    .await?;
    if verbose {
        eprintln!(
            "{}",
            serde_json::to_string_pretty(&result).unwrap_or_default()
        );
    }
    eprintln!(
        "code-index reconciliation queued via daemon for {}",
        resolved.project_path.display()
    );
    if doctor {
        tracedecay::doctor::run_doctor().await?;
    }
    Ok(())
}
