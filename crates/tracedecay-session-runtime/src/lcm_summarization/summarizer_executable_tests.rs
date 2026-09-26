//! Guards that on-demand summarization launches a host CLI only through the
//! configured `lcm.summarizer_executables.v1` binding.
//!
//! A trap `cursor-agent`/`codex` sits first on `PATH` and records every launch.
//! With the isolated profile and no configured executable the request must
//! settle on the typed unconfigured reason and the trap must stay silent.

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use tracedecay_domain::ProjectId;
use tracedecay_domain::configuration::{LcmSummarizerExecutableV1, LcmSummarizerExecutablesV1};
use tracedecay_global_db::tests::harness::{
    RegisteredGlobalDbHarness, RegisteredGlobalDbTestRuntime,
};
use tracedecay_lcm::{LcmSummaryRequest, LcmSummarySourceMessage, LcmSummarySourceRange};

use super::{
    CODEX_APP_SERVER_UNCONFIGURED, CURSOR_AGENT_UNCONFIGURED, SUMMARIZER_CONFIGURATION_UNAVAILABLE,
    SummaryResolutionError, generate_provider_summary, summarizer_executables,
};

/// `PATH` is process-wide, so the guards below take turns owning it.
static PATH_OWNER: Mutex<()> = Mutex::new(());

/// Puts trap executables first on `PATH` for the guard's lifetime. Each trap
/// appends its name to `launches` so a spawn is observable after the fact.
struct TrapPath {
    previous: Option<OsString>,
    launches: PathBuf,
    _directory: tempfile::TempDir,
    _owner: MutexGuard<'static, ()>,
}

impl TrapPath {
    fn install(names: &[&str]) -> Self {
        let owner = PATH_OWNER
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let directory = tempfile::tempdir().unwrap();
        let launches = directory.path().join("launches.log");
        for name in names {
            let trap = directory.path().join(name);
            std::fs::write(
                &trap,
                format!(
                    "#!/bin/sh\nprintf '%s\\n' \"$0\" >> '{}'\nexit 0\n",
                    launches.display()
                ),
            )
            .unwrap();
            let mut permissions = std::fs::metadata(&trap).unwrap().permissions();
            std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o700);
            std::fs::set_permissions(&trap, permissions).unwrap();
        }
        let previous = std::env::var_os("PATH");
        let mut path = directory.path().as_os_str().to_owned();
        if let Some(rest) = &previous {
            path.push(":");
            path.push(rest);
        }
        // SAFETY: tests serialize process-environment access through the
        // shared TraceDecay environment lock; the guard restores PATH on drop.
        unsafe { std::env::set_var("PATH", &path) };
        Self {
            previous,
            launches,
            _directory: directory,
            _owner: owner,
        }
    }

    fn launches(&self) -> String {
        std::fs::read_to_string(&self.launches).unwrap_or_default()
    }
}

impl Drop for TrapPath {
    fn drop(&mut self) {
        // SAFETY: see `install`.
        unsafe {
            match self.previous.take() {
                Some(previous) => std::env::set_var("PATH", previous),
                None => std::env::remove_var("PATH"),
            }
        }
    }
}

fn summary_request(provider: &str) -> LcmSummaryRequest {
    LcmSummaryRequest {
        provider: provider.to_owned(),
        session_id: "guard-session".to_owned(),
        focus_topic: None,
        prompt: "summarize".to_owned(),
        source_range: LcmSummarySourceRange {
            from_store_id: 1,
            to_store_id: 2,
        },
        source_messages: vec![LcmSummarySourceMessage {
            store_id: 1,
            role: "user".to_owned(),
            content: "hello".to_owned(),
        }],
        extraction_request: None,
    }
}

fn unavailable_reason(error: SummaryResolutionError) -> &'static str {
    match error {
        SummaryResolutionError::Unavailable(reason) => reason,
        SummaryResolutionError::Storage(error) => {
            panic!("expected a typed unavailable reason, got storage error {error}")
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn unconfigured_summarizers_report_typed_state_and_spawn_nothing() {
    let trap = TrapPath::install(&["cursor-agent", "codex"]);
    let harness = RegisteredGlobalDbHarness::open("lcm-summarizer-executable-guard").await;
    let database = harness.registered.clone();

    // A profile-sessions shard has no project configuration: every provider
    // is unconfigured there by construction.
    assert_eq!(
        summarizer_executables(&database).ok(),
        Some(LcmSummarizerExecutablesV1::unconfigured())
    );

    let cursor = generate_provider_summary(
        &database,
        "cursor",
        &summary_request("cursor"),
        Duration::from_secs(5),
    )
    .await
    .err()
    .map(unavailable_reason);
    assert_eq!(cursor, Some(CURSOR_AGENT_UNCONFIGURED));

    let codex = generate_provider_summary(
        &database,
        "codex",
        &summary_request("codex"),
        Duration::from_secs(5),
    )
    .await
    .err()
    .map(unavailable_reason);
    assert_eq!(codex, Some(CODEX_APP_SERVER_UNCONFIGURED));

    assert_eq!(
        trap.launches(),
        "",
        "an unconfigured summarizer must never reach a PATH-resolved host CLI"
    );
    // The traps are reachable through PATH, so silence above is the setting
    // refusing, not a missing binary.
    let resolved = std::process::Command::new("cursor-agent").status().unwrap();
    assert!(resolved.success());
    assert!(trap.launches().contains("cursor-agent"));
}

#[tokio::test(flavor = "multi_thread")]
async fn project_shard_without_a_published_pin_is_unavailable_not_ambient() {
    let trap = TrapPath::install(&["cursor-agent"]);
    let root = tempfile::tempdir().unwrap();
    let project_id = ProjectId::new("project.lcm-summarizer-guard".to_owned()).unwrap();
    let runtime = RegisteredGlobalDbTestRuntime::project(
        root.path().join("profile"),
        root.path().join("project"),
        project_id,
    )
    .await
    .unwrap();
    let database = runtime.project_database_arc().unwrap();

    let reason = generate_provider_summary(
        &database,
        "cursor",
        &summary_request("cursor"),
        Duration::from_secs(5),
    )
    .await
    .err()
    .map(unavailable_reason);
    assert_eq!(reason, Some(SUMMARIZER_CONFIGURATION_UNAVAILABLE));
    assert_eq!(trap.launches(), "");
}

#[tokio::test(flavor = "multi_thread")]
async fn configured_executable_is_launched_instead_of_the_path_binary() {
    let trap = TrapPath::install(&["cursor-agent"]);
    let root = tempfile::tempdir().unwrap();
    let project_root = root.path().join("project");
    let project_id = ProjectId::new("project.lcm-summarizer-configured".to_owned()).unwrap();
    let runtime = RegisteredGlobalDbTestRuntime::project(
        root.path().join("profile"),
        &project_root,
        project_id.clone(),
    )
    .await
    .unwrap();
    let database = runtime.project_database_arc().unwrap();

    let configured = root.path().join("bin").join("cursor-agent");
    std::fs::create_dir_all(configured.parent().unwrap()).unwrap();
    std::fs::write(
        &configured,
        "#!/bin/sh\nprintf '%s\\n' 'configured summary text'\n",
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&configured).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o700);
    std::fs::set_permissions(&configured, permissions).unwrap();
    tracedecay_configuration::test_support::pin_lcm_summarizer_executables(
        project_id,
        &project_root,
        LcmSummarizerExecutablesV1 {
            cursor_agent: LcmSummarizerExecutableV1::configured(configured.clone()).unwrap(),
            codex: LcmSummarizerExecutableV1::Unconfigured,
        },
    )
    .unwrap();

    let summary = generate_provider_summary(
        &database,
        "cursor",
        &summary_request("cursor"),
        Duration::from_secs(5),
    )
    .await
    .ok()
    .map(|summary| (summary.text, summary.route));
    assert_eq!(
        summary,
        Some((
            "configured summary text".to_owned(),
            "cursor_agent".to_owned()
        ))
    );
    assert_eq!(
        trap.launches(),
        "",
        "the PATH binary must stay untouched while a configured executable exists"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn configured_model_reaches_the_summarizer_in_a_private_workspace() {
    let _trap = TrapPath::install(&[]);
    let root = tempfile::tempdir().unwrap();
    let project_root = root.path().join("project");
    let project_id = ProjectId::new("project.lcm-summarizer-tuning".to_owned()).unwrap();
    let runtime = RegisteredGlobalDbTestRuntime::project(
        root.path().join("profile"),
        &project_root,
        project_id.clone(),
    )
    .await
    .unwrap();
    let database = runtime.project_database_arc().unwrap();

    let configured = root.path().join("bin").join("cursor-agent");
    let argv = root.path().join("argv.log");
    std::fs::create_dir_all(configured.parent().unwrap()).unwrap();
    std::fs::write(
        &configured,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nprintf '%s\\n' 'tuned summary text'\n",
            argv.display()
        ),
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&configured).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o700);
    std::fs::set_permissions(&configured, permissions).unwrap();
    tracedecay_configuration::test_support::pin_lcm_summarizer_executables(
        project_id,
        &project_root,
        LcmSummarizerExecutablesV1 {
            cursor_agent: LcmSummarizerExecutableV1::configured_with(
                configured,
                Some("configured-summary-model".to_owned()),
                Some(5),
            )
            .unwrap(),
            codex: LcmSummarizerExecutableV1::Unconfigured,
        },
    )
    .unwrap();

    let summary = generate_provider_summary(
        &database,
        "cursor",
        &summary_request("cursor"),
        Duration::from_secs(30),
    )
    .await
    .ok()
    .map(|summary| summary.text);
    assert_eq!(summary.as_deref(), Some("tuned summary text"));

    let argv = std::fs::read_to_string(&argv).unwrap();
    let args: Vec<&str> = argv.lines().collect();
    let value_after = |flag: &str| {
        args.iter()
            .position(|arg| *arg == flag)
            .and_then(|index| args.get(index + 1).copied())
    };
    assert_eq!(value_after("--model"), Some("configured-summary-model"));
    let workspace = PathBuf::from(value_after("--workspace").unwrap());
    assert_ne!(workspace, std::env::temp_dir());
    assert!(
        !workspace.exists(),
        "the per-run summary workspace must be removed after the run"
    );
}
