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
        )
        .await?;
    }
    Ok(())
}

pub(crate) async fn handle_init(
    path: Option<String>,
    skip_folders: Vec<String>,
    include_folders: Vec<String>,
) -> tracedecay::errors::Result<()> {
    let project_path = tracedecay::config::resolve_path(path);
    let profile_root = tracedecay::storage::default_profile_root()?;
    if let Some(message) =
        tracedecay::project_registry::ephemeral_root_rejection(&project_path, &profile_root)
    {
        return Err(tracedecay::errors::TraceDecayError::Config { message });
    }
    let handshake = tracedecay::daemon::DaemonHandshake::for_current_client(
        Some(project_path.clone()),
        None,
        false,
        true,
    )?;
    #[cfg(unix)]
    let daemon_available = tracedecay::daemon::daemon_reachable();
    #[cfg(not(unix))]
    let daemon_available = true;

    handle_init_with_daemon_availability(
        project_path,
        skip_folders,
        include_folders,
        handshake,
        daemon_available,
    )
    .await
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

    let profile_root = handshake.client_identity.profile_root.clone();
    let lifecycle_lease = match tracedecay::lifecycle_lease::acquire_exclusive_for_profile(
        &profile_root,
        "init bootstrap",
    ) {
        Ok(lease) => lease,
        Err(lease_error) => {
            // A daemon may have acquired its shared lease after our availability
            // probe. Only retry the broker when a fresh, pre-request probe proves
            // it is now reachable; otherwise preserve the lifecycle error.
            #[cfg(unix)]
            if tracedecay::daemon::daemon_reachable() {
                return brokered_init(&project_path, &skip_folders, &include_folders, &handshake)
                    .await;
            }
            return Err(lease_error);
        }
    };
    let _database_scope = tracedecay::db::enter_maintenance_database_scope(
        &lifecycle_lease,
        &profile_root,
        "init bootstrap",
    )?;

    maintenance_bootstrap_init(
        &project_path,
        &skip_folders,
        &include_folders,
        &handshake,
        &lifecycle_lease,
    )
    .await
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
    // Init deliberately triggers a cold project open+index behind this single
    // status call. The default warming-retry grace is far tighter than a cold
    // open can take on a debug build or slow shared runner, which surfaced as
    // "daemon tracedecay_status timed out during read before deadline" failures
    // in CI. Give the bootstrap a generous budget so the client waits out the
    // background open instead of abandoning it just before it completes.
    let init_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(120);
    tracedecay::daemon::call_default_tool_awaiting_project_open(
        handshake,
        "tracedecay_status",
        serde_json::json!({"format": "json"}),
        init_deadline,
    )
    .await?;
    eprintln!(
        "initialized and indexed {} via daemon",
        project_path.display()
    );
    Ok(())
}

async fn maintenance_bootstrap_init(
    project_path: &Path,
    skip_folders: &[String],
    include_folders: &[String],
    handshake: &tracedecay::daemon::DaemonHandshake,
    lifecycle_lease: &tracedecay::lifecycle_lease::LifecycleLease,
) -> tracedecay::errors::Result<()> {
    if !project_path.is_dir() {
        return Err(tracedecay::errors::TraceDecayError::Config {
            message: format!(
                "project path is not a directory: {}",
                project_path.display()
            ),
        });
    }
    if !project_path.is_absolute() {
        return Err(tracedecay::errors::TraceDecayError::Config {
            message: format!("project path must be absolute: {}", project_path.display()),
        });
    }

    let open_options = tracedecay::tracedecay::TraceDecayOpenOptions {
        profile_root: Some(handshake.client_identity.profile_root.clone()),
        global_db_path: Some(handshake.client_identity.global_db_path.clone()),
    };
    if TraceDecay::try_initialized_store_layout_with_options(project_path, &open_options)
        .await?
        .is_some()
    {
        return Err(tracedecay::errors::TraceDecayError::Config {
            message: format!(
                "TraceDecay is already initialized at '{}'; use `tracedecay sync` to update the index",
                project_path.display()
            ),
        });
    }

    let mut cg =
        TraceDecay::init_with_exclusive_maintenance(project_path, open_options, lifecycle_lease)
            .await?;
    cg.add_skip_folders(skip_folders);
    cg.add_include_folders(include_folders);
    if let Err(error) = cg.index_all().await {
        cg.close();
        return Err(error);
    }
    let checkpoint_result = cg.checkpoint().await;
    cg.close();
    checkpoint_result?;
    eprintln!(
        "initialized and indexed {} under exclusive maintenance bootstrap",
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
        }
    }

    /// A directory guaranteed to sit outside `std::env::temp_dir()`, so the
    /// project/profile fixtures built inside it are never treated as
    /// isolated-test paths by `db::access::is_isolated_test_path` (which
    /// would let a direct open acquire the test-only authority escape hatch
    /// instead of exercising the fail-closed production path this test
    /// verifies). `env!("CARGO_MANIFEST_DIR")).join("target")` used to serve
    /// this purpose, but that only holds when the checkout itself lives
    /// outside the OS temp directory; a repo cloned under `/tmp` (as some
    /// sandboxed CI/dev environments do) breaks that assumption. Deriving the
    /// base from the running test binary's own on-disk location is robust
    /// regardless of where the checkout lives, because cargo (or any
    /// build-cache shim in front of it) never places build output inside the
    /// volatile system temp directory.
    fn checkout_tempdir() -> tempfile::TempDir {
        let exe = std::env::current_exe().expect("test binary has a current_exe path");
        let profile_dir = exe
            .parent() // .../target/<profile>/deps
            .and_then(Path::parent) // .../target/<profile>
            .expect("test binary sits under a cargo target profile directory")
            .to_path_buf();
        let base = profile_dir.join("commands-init-tests");
        std::fs::create_dir_all(&base).unwrap();
        tempfile::Builder::new()
            .prefix("daemonless-init-")
            .tempdir_in(base)
            .unwrap()
    }

    #[tokio::test(flavor = "current_thread")]
    async fn daemonless_init_uses_maintenance_authority_and_keeps_direct_open_fail_closed() {
        let temp = checkout_tempdir();
        let project = temp.path().join("project");
        let profile = temp.path().join("profile");
        std::fs::create_dir_all(project.join("src")).unwrap();
        std::fs::create_dir_all(project.join("ignored")).unwrap();
        gix::init(&project).unwrap();
        std::fs::write(project.join("src/main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(project.join("ignored/skip.rs"), "fn skipped() {}\n").unwrap();
        let handshake = test_handshake(&project, &profile);

        handle_init_with_daemon_availability(
            project.clone(),
            vec!["ignored".to_string()],
            vec!["src".to_string()],
            handshake.clone(),
            false,
        )
        .await
        .unwrap();

        let open_options = tracedecay::tracedecay::TraceDecayOpenOptions {
            profile_root: Some(profile.clone()),
            global_db_path: Some(profile.join("global.db")),
        };
        let direct_open_error =
            match TraceDecay::open_with_options(&project, open_options.clone()).await {
                Ok(cg) => {
                    cg.close();
                    panic!("ordinary direct open unexpectedly acquired writable authority");
                }
                Err(error) => error,
            };
        assert!(
            direct_open_error.to_string().contains(
                "configuration authority unavailable: a registered project session runtime is required"
            ),
            "unexpected direct-open error: {direct_open_error}"
        );

        let lifecycle = tracedecay::lifecycle_lease::acquire_exclusive_for_profile(
            &profile,
            "inspect daemonless init test",
        )
        .unwrap();
        let _database_scope = tracedecay::db::enter_maintenance_database_scope(
            &lifecycle,
            &profile,
            "inspect daemonless init test",
        )
        .unwrap();
        let cg = TraceDecay::open_with_exclusive_maintenance(&project, open_options, &lifecycle)
            .await
            .unwrap();
        let files = cg.get_all_files().await.unwrap();
        cg.close();
        assert!(
            files.iter().any(|file| file.path.ends_with("src/main.rs")),
            "included source file was not indexed: {files:?}"
        );
        assert!(
            files
                .iter()
                .all(|file| !file.path.ends_with("ignored/skip.rs")),
            "skipped folder was indexed: {files:?}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn brokered_init_retains_folder_option_error_before_sending_request() {
        let temp = checkout_tempdir();
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
        "sync completed via daemon for {}",
        resolved.project_path.display()
    );
    if doctor {
        tracedecay::doctor::run_doctor(None).await?;
    }
    Ok(())
}
