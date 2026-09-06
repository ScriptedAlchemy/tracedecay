//! Composition-root registration for every process-global runtime port.
//!
//! The crate split left several capabilities inverted behind `OnceLock` slots
//! in the extracted crates: `tracedecay_sessions::host_ports` and
//! `tracedecay_agent_hosts::ports`. Each slot has a
//! conservative default so an unwired process still runs — it just does less
//! (no memory injection, zero turn costs, a daemon that reports itself
//! unavailable).
//!
//! Only the composition root can fill them, and it must do so before any
//! transcript ingest, host installer, hook, or branch lock runs. That is what
//! [`register_runtime_ports`] is: the complete, idempotent, root-owned wiring
//! call. There is no partial form: an earlier split that installed everything
//! *except* the agent-host MCP tool catalog let any installer reached on that
//! path write an empty tool permission set with no error, so the catalog is
//! now read directly from `tracedecay-mcp` by the crate that needs it and is
//! not wired here at all. Composition-root registry wrappers (`join_standalone_session_registry`, session-runtime
//! shutdown, host admission) invoke the complete form for embedded and
//! integration-test runtimes that never pass through `main`. The extracted
//! store-runtime crate never calls this.
//!
//! Every underlying `register` is `OnceLock::set`, so repeated calls are safe
//! and the first registration wins.

use std::future::Future;
use std::path::Path;
use std::pin::Pin;

use serde_json::Value;

use tracedecay_domain::errors::Result;

/// Installs every root-owned runtime port. Idempotent; first call wins.
///
/// Call this as early as possible in a process: the slots below are read by
/// transcript ingest, agent-host installers, hooks, and branch locking, all of
/// which fail quietly (or fail closed) when the root never registered.
#[hotpath::measure(label = "runtime_ports.register")]
pub fn register_runtime_ports() -> Result<()> {
    register_session_ports();
    register_agent_host_ports();
    Ok(())
}

/// Adapts the root catalog composer to the code-index runtime's provider seam.
pub(crate) fn compose_application_catalog_snapshot() -> std::result::Result<
    tracedecay_tool_catalog::CatalogSnapshotV1,
    tracedecay_code_index_runtime::ApplicationCatalogSnapshotErrorV1,
> {
    crate::catalog_composition::build_application_catalog_snapshot().map_err(|error| {
        tracedecay_code_index_runtime::ApplicationCatalogSnapshotErrorV1::new(error.to_string())
    })
}

// ---------------------------------------------------------------------------
// tracedecay_sessions::host_ports
// ---------------------------------------------------------------------------

fn register_session_ports() {
    use tracedecay_sessions::host_ports;

    host_ports::hermes_profile_pin::register(
        tracedecay_agent_hosts::agents::hermes::read_config_pinned_project_root,
    );
    host_ports::session_review::register(schedule_user_session_review);
    host_ports::unregistered_admission::register(unregistered_admission);
}

fn schedule_user_session_review<'a>(
    provider: &'a str,
    session_id: Option<&'a str>,
) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
    Box::pin(hotpath::future!(
        tracedecay_agent_hosts::hooks::schedule_user_session_review(provider, session_id),
        label = "runtime_ports.session_review"
    ))
}

/// Builds an admission facade with no durable authority behind it.
///
/// The standalone Codex entry points walk a rollout and count what they *would*
/// admit; every capture through this facade fails closed because no registered
/// database is attached.
fn unregistered_admission(
    scope: tracedecay_sessions::host_ports::unregistered_admission::Scope,
) -> Box<dyn tracedecay_sessions::admission::HostAdmission> {
    use tracedecay_host_admission::{HostAdmissionAuthorities, HostAdmissionFacade};
    use tracedecay_sessions::host_ports::unregistered_admission::Scope;

    let authorities = match scope {
        Scope::Project(project_id) => {
            HostAdmissionAuthorities::unregistered_for_project(project_id)
        }
        Scope::Profile => HostAdmissionAuthorities::unregistered_for_profile(),
    };
    Box::new(HostAdmissionFacade::new(authorities))
}

// ---------------------------------------------------------------------------
// tracedecay_agent_hosts::ports
// ---------------------------------------------------------------------------

fn register_agent_host_ports() {
    use tracedecay_agent_hosts::ports;
    use tracedecay_automation_runtime::ports as automation_ports;

    tracedecay_agent_hosts::register_automation_host_io();
    automation_ports::codex_app_server::register(run_codex_app_server_prompt);
    automation_ports::session_store::register_canonical_project_key(
        tracedecay_global_db::RegisteredGlobalDb::canonical_project_key,
    );
    // One handle, built here and installed whole: a hook path either has every
    // root capability or the process reports a bootstrap failure. Two former
    // slots are absent by design — the memory-injection gate and the Cursor
    // ingest ceiling were agent-hosts' own function and constant round-tripped
    // through the root, and their readers now call them directly.
    ports::hook_runtime::install(ports::hook_runtime::HookRuntimeV1 {
        daemon_tool: daemon_tool_json,
        project_root_resolver: resolve_project_root_with_identity,
        scope_resolver: resolve_hook_scope,
        event_notifier: notify_hook_event,
        timing_gate: hook_timings_enabled,
        project_initialization_gate: crate::tracedecay::TraceDecay::is_initialized,
        store_layout_resolver: resolve_hook_store_layout,
    });
}

#[hotpath::measure(label = "runtime_ports.codex_app_server")]
fn run_codex_app_server_prompt(
    prompt: &str,
    config: &tracedecay_automation_runtime::ports::codex_app_server::SummaryConfig,
    thread_source: &str,
) -> std::result::Result<tracedecay_automation_runtime::ports::codex_app_server::Summary, String> {
    let config = tracedecay_sessions::runtime::codex_app_server::CodexAppServerSummaryConfig {
        codex_bin: config.codex_bin.clone(),
        model: config.model.clone(),
        timeout: config.timeout,
    };
    tracedecay_sessions::runtime::codex_app_server::run_prompt_with_codex_app_server(
        prompt,
        &config,
        thread_source,
    )
    .map(
        |summary| tracedecay_automation_runtime::ports::codex_app_server::Summary {
            text: summary.text,
            model: summary.model,
        },
    )
    .map_err(|error| error.to_string())
}

/// Fn-pointer shim over the root's async daemon tool call.
///
/// The port is a plain `fn` returning a boxed future so the extracted crate
/// needs no async-trait machinery.
fn daemon_tool_json<'a>(
    project_root: Option<&'a Path>,
    tool_name: &'a str,
    arguments: Value,
    require_project_identity: bool,
) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>> {
    Box::pin(hotpath::future!(
        async move {
            let handshake = crate::daemon::handshake_for_current_client(
                project_root.map(Path::to_path_buf),
                None,
                false,
                require_project_identity,
            )?;
            let result = crate::daemon::call_default_tool(&handshake, tool_name, arguments).await?;
            crate::daemon::tool_json_payload(&result, tool_name)
        },
        label = "runtime_ports.daemon_tool"
    ))
}

fn resolve_project_root_with_identity(
    start: &Path,
) -> Pin<Box<dyn Future<Output = Option<std::path::PathBuf>> + Send + '_>> {
    Box::pin(hotpath::future!(
        crate::config::discover_project_root_with_identity(start),
        label = "runtime_ports.resolve_project_root"
    ))
}

#[hotpath::measure(label = "runtime_ports.resolve_hook_scope")]
fn resolve_hook_scope(
    project_root: &Path,
    project_id: &tracedecay_domain::ProjectId,
) -> std::result::Result<tracedecay_application::ResolvedScope, String> {
    tracedecay_code_index_runtime::resolved_scope_for_project(project_root, project_id)
        .map_err(|error| error.to_string())
}

fn notify_hook_event(
    project_root: &Path,
    event: tracedecay_hooks::DaemonHookEvent,
) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
    Box::pin(hotpath::future!(
        async move {
            let _ = crate::daemon::notify_hook_event(project_root, event).await;
        },
        label = "runtime_ports.notify_hook"
    ))
}

fn hook_timings_enabled(project_root: &Path) -> Option<bool> {
    crate::config::cached_telemetry_config(project_root)
        .ok()
        .map(|telemetry| telemetry.timings)
}

fn resolve_hook_store_layout(
    project_root: &Path,
) -> Pin<Box<dyn Future<Output = Result<tracedecay_runtime_core::storage::StoreLayout>> + Send + '_>>
{
    Box::pin(hotpath::future!(
        crate::tracedecay::TraceDecay::resolve_store_layout_for_identity(project_root),
        label = "runtime_ports.resolve_store_layout"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Registration is process-global and every slot is a `OnceLock`, so the
    /// whole suite shares one installation. Doing it once here keeps the
    /// assertions below independent of test order.
    ///
    /// The returned guard pins profile discovery at an empty tempdir: several
    /// of these adapters read the owner's real profile, which no test may
    /// touch.
    fn registered() -> crate::config::PinnedUserDataDir {
        static ONCE: std::sync::Once = std::sync::Once::new();
        let pinned = crate::config::PinnedUserDataDir::new();
        ONCE.call_once(|| {
            register_runtime_ports().expect("runtime port registration");
        });
        pinned
    }

    #[test]
    fn hermes_profile_pin_resolves_a_pinned_root_after_registration() {
        let _pinned = registered();
        let temp = tempfile::tempdir().expect("tempdir");
        let config = temp.path().join("config.yaml");
        let pinned = temp.path().join("pinned-project");
        std::fs::write(
            &config,
            format!(
                "plugins:\n  tracedecay:\n    project_root: \"{}\"\n",
                pinned.display()
            ),
        )
        .expect("write hermes profile config");

        // Unwired this reads `None`, which makes legacy Hermes state stores
        // skip rather than attribute to the pinned root.
        assert_eq!(
            tracedecay_sessions::host_ports::hermes_profile_pin::resolve(&config),
            Some(pinned.display().to_string()),
            "registered resolver must back the hermes profile pin port"
        );
    }

    #[test]
    fn unregistered_admission_factory_builds_both_scopes() {
        let _pinned = registered();
        use tracedecay_sessions::host_ports::unregistered_admission::{Scope, create};

        assert!(
            create(Scope::Profile).is_some(),
            "profile-scoped unregistered admission must be constructible"
        );
        let project_id = tracedecay_domain::ProjectId::new("project.runtime-ports-test")
            .expect("valid project id");
        assert!(
            create(Scope::Project(project_id)).is_some(),
            "project-scoped unregistered admission must be constructible"
        );
    }

    /// The whole hook runtime arrives as one handle, so this is the single
    /// assertion that the composition root is complete for every hook path.
    #[test]
    fn the_hook_runtime_handle_is_installed_after_registration() {
        let _pinned = registered();
        assert!(
            tracedecay_agent_hosts::ports::hook_runtime::installed().is_some(),
            "register_runtime_ports must install the hook runtime handle"
        );
    }

    /// The tool catalog is no longer wired here at all: host installers read
    /// it from its owning crate, so it is readable with no registration and an
    /// unavailable catalog is an error rather than an empty tool set.
    #[test]
    fn the_advertised_tool_catalog_needs_no_registration() {
        let _pinned = registered();
        let tools = tracedecay_agent_hosts::ports::mcp_tools::advertised_tools()
            .expect("the advertised tool catalog");
        assert!(!tools.is_empty());
    }

    #[test]
    fn pricing_reader_uses_the_shared_all_provider_table() {
        let _pinned = registered();
        let model = "claude-sonnet-4-6";
        let cost = tracedecay_agent_hosts::ports::pricing::cost_of_turn(
            "claude", model, 1_000_000, 0, 0, 0,
        );
        assert!(cost.is_some_and(|cost| cost > 0.0));
    }
}
