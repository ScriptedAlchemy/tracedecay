//! Runtime-port wiring for the capabilities the extracted crates invert.
//!
//! `tracedecay_sessions::host_ports` and the automation host-I/O bundle are
//! process-global `OnceLock` slots that must be filled before any transcript
//! ingest, host installer, or branch lock runs. [`register_runtime_ports`] is
//! the complete, idempotent wiring call that fills them. Every underlying
//! `register` is `OnceLock::set`, so repeated calls are safe and the first
//! registration wins.
//!
//! The hook runtime handle ([`HookRuntimeV1`]) is composed here from this
//! crate's adapters plus the one capability this crate cannot build: the
//! daemon client ([`DaemonClientPortsV1`] — the tool invoker and the hook
//! event notifier that talk to the daemon over its socket). The composition
//! root owns that client and hands it to [`register_runtime_ports`]; a root
//! that wants an explicit handle for a hook entry point builds it with
//! [`hook_runtime_with`] and never touches the slot. Project open reads the
//! registered client through [`hook_runtime`], which is a typed refusal in a
//! process whose composition root never registered.

use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::OnceLock;

use serde_json::Value;

use tracedecay_agent_hosts::ports::hook_runtime::{
    DaemonToolInvoker, HookEventNotifier, HookRuntimeV1,
};
use tracedecay_domain::errors::{Result, TraceDecayError};

/// The daemon-client half of the hook runtime handle: the two adapters that
/// need a daemon connection, handshake, and wire preamble, which only the
/// composition root composes.
#[derive(Clone, Copy)]
pub struct DaemonClientPortsV1 {
    pub daemon_tool: DaemonToolInvoker,
    pub event_notifier: HookEventNotifier,
}

static DAEMON_CLIENT_PORTS: OnceLock<DaemonClientPortsV1> = OnceLock::new();

/// Installs every runtime port. Idempotent; first call wins.
///
/// Call this as early as possible in a process: the slots below are read by
/// transcript ingest, agent-host installers, hooks, branch locking, and
/// project open, all of which fail closed when nothing registered.
#[hotpath::measure(label = "runtime_ports.register")]
pub fn register_runtime_ports(daemon_client: DaemonClientPortsV1) -> Result<()> {
    register_session_ports();
    register_agent_host_ports();
    // First registration wins, like every slot below.
    let _ = DAEMON_CLIENT_PORTS.set(daemon_client);
    Ok(())
}

/// Typed refusal for a process that reaches project open before its
/// composition root registered the runtime ports.
pub(crate) fn require_runtime_ports() -> Result<DaemonClientPortsV1> {
    DAEMON_CLIENT_PORTS
        .get()
        .copied()
        .ok_or_else(|| TraceDecayError::Config {
            message: "runtime ports are not registered: the composition root must call \
                      register_runtime_ports before a project opens"
                .to_owned(),
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
    host_ports::unregistered_admission::register(unregistered_admission);
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
    use tracedecay_automation_runtime::ports as automation_ports;

    automation_ports::codex_app_server::register(run_codex_app_server_prompt);
    automation_ports::session_store::register_canonical_project_key(
        tracedecay_global_db::RegisteredGlobalDb::canonical_project_key,
    );
}

/// The hook runtime handle for the registered daemon client, or a typed
/// refusal when no composition root registered one.
///
/// Project open publishes hook bindings through this handle. Hook entry
/// points hold an explicit handle from [`hook_runtime_with`] instead.
pub fn hook_runtime() -> Result<HookRuntimeV1> {
    Ok(hook_runtime_with(require_runtime_ports()?))
}

/// The hook runtime handle: every capability a hook path needs, as one `Copy`
/// value of this crate's adapters plus the supplied daemon client.
///
/// Built wherever a hook entry point starts rather than stored: the struct is
/// plain function pointers, so constructing it is free and there is no slot
/// for a second composition to lose. Two former slots are absent by design —
/// the memory-injection gate and the Cursor ingest ceiling were agent-hosts'
/// own function and constant round-tripped through the root, and their
/// readers now call them directly.
#[must_use]
pub fn hook_runtime_with(daemon_client: DaemonClientPortsV1) -> HookRuntimeV1 {
    HookRuntimeV1 {
        daemon_tool: daemon_client.daemon_tool,
        project_root_resolver: resolve_project_root_with_identity,
        scope_resolver: resolve_hook_scope,
        event_notifier: daemon_client.event_notifier,
        timing_gate: hook_timings_enabled,
        project_initialization_gate: crate::project::TraceDecay::is_initialized,
        store_layout_resolver: resolve_hook_store_layout,
    }
}

/// The daemon client a test process registers in place of a composition
/// root: the daemon tool answers a typed unavailable state and the event
/// notifier delivers nothing, because no daemon socket exists behind it.
///
/// Invariant: a test process only ever registers this fixture client or the
/// root's real one, and project open reads the handle solely to publish hook
/// bindings, which consults the scope resolver alone.
#[cfg(any(test, feature = "test-helpers"))]
#[must_use]
pub fn fixture_daemon_client_ports() -> DaemonClientPortsV1 {
    DaemonClientPortsV1 {
        daemon_tool: fixture_daemon_tool,
        event_notifier: fixture_notify_hook_event,
    }
}

#[cfg(any(test, feature = "test-helpers"))]
fn fixture_daemon_tool<'a>(
    _project_root: Option<&'a Path>,
    tool_name: &'a str,
    _arguments: Value,
    _require_project_identity: bool,
) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>> {
    Box::pin(async move {
        Err(TraceDecayError::Config {
            message: format!(
                "fixture daemon client: daemon tool '{tool_name}' has no daemon behind it"
            ),
        })
    })
}

#[cfg(any(test, feature = "test-helpers"))]
fn fixture_notify_hook_event(
    _project_root: &Path,
    _event: tracedecay_hooks::DaemonHookEvent,
) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
    Box::pin(async {})
}

#[hotpath::measure(label = "runtime_ports.codex_app_server")]
fn run_codex_app_server_prompt(
    prompt: &str,
    config: &tracedecay_automation_runtime::ports::codex_app_server::SummaryConfig,
    thread_source: &str,
    response_schema: Option<&Value>,
) -> std::result::Result<tracedecay_automation_runtime::ports::codex_app_server::Summary, String> {
    let config = tracedecay_sessions::runtime::codex_app_server::CodexAppServerSummaryConfig {
        codex_bin: config.codex_bin.clone(),
        model: config.model.clone(),
        timeout: config.timeout,
    };
    let result = if let Some(response_schema) = response_schema {
        tracedecay_sessions::runtime::codex_app_server::run_prompt_with_codex_app_server_response_schema(
            prompt,
            &config,
            thread_source,
            response_schema,
        )
    } else {
        tracedecay_sessions::runtime::codex_app_server::run_prompt_with_codex_app_server(
            prompt,
            &config,
            thread_source,
        )
    };
    result
        .map(
            |summary| tracedecay_automation_runtime::ports::codex_app_server::Summary {
                text: summary.text,
                model: summary.model,
            },
        )
        .map_err(|error| error.to_string())
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
) -> std::result::Result<tracedecay_contracts::ResolvedScope, String> {
    tracedecay_code_index_runtime::resolved_scope_for_project(project_root, project_id)
        .map_err(|error| error.to_string())
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
        crate::project::TraceDecay::resolve_store_layout_for_identity(project_root),
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
            register_runtime_ports(fixture_daemon_client_ports())
                .expect("runtime port registration");
        });
        pinned
    }

    /// YAML double-quoted scalars treat `\t`/`\U` as escapes. A Windows
    /// native path must be written so those separators survive as separators.
    fn hermes_project_root_yaml(project_root: &str) -> String {
        format!(
            "plugins:\n  tracedecay:\n    project_root: '{}'\n",
            project_root.replace('\'', "''")
        )
    }

    #[test]
    fn hermes_profile_pin_resolves_a_pinned_root_after_registration() {
        let _pinned = registered();
        let temp = tempfile::tempdir().expect("tempdir");
        let config = temp.path().join("config.yaml");
        let pinned = temp.path().join("pinned-project");
        // Single-quoted so a Windows path's backslashes are not read as YAML
        // escapes (`\a` -> BEL, `\t` -> TAB).
        std::fs::write(
            &config,
            hermes_project_root_yaml(&pinned.display().to_string()),
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

    /// The hook runtime is one explicit handle of adapters, so this is the
    /// single check that every hook capability this crate composes answers
    /// through it (here: the registered-identity gates for an unregistered
    /// checkout) instead of through a slot that may be empty.
    #[tokio::test]
    async fn the_hook_runtime_handle_answers_through_project_adapters() {
        let _pinned = registered();
        let runtime = hook_runtime().expect("registered daemon client");
        let unregistered = tempfile::tempdir().expect("tempdir");
        // The layout keeps the caller's spelling of the root, so hand it the
        // canonical form up front: macOS temp roots live behind the
        // `/var` -> `/private/var` symlink.
        let checkout = unregistered
            .path()
            .canonicalize()
            .expect("canonical checkout");

        assert!(!(runtime.project_initialization_gate)(&checkout));
        assert!((runtime.project_root_resolver)(&checkout).await.is_none());
        assert!(
            (runtime.timing_gate)(&checkout).is_none(),
            "an unregistered checkout has no published telemetry override"
        );
        let layout = (runtime.store_layout_resolver)(&checkout)
            .await
            .expect("the root resolves a canonical layout for any checkout");
        assert_eq!(layout.project_root, checkout);
        assert!(layout.identity.project_id.is_some());
    }
}
