//! Composition-root registration for every process-global runtime port.
//!
//! The crate split left several capabilities inverted behind `OnceLock` slots
//! in the extracted crates: `tracedecay_sessions::host_ports`,
//! `tracedecay_agent_hosts::ports`, and
//! `tracedecay_runtime_core::ports::branch_admin_recovery`. Each slot has a
//! conservative default so an unwired process still runs — it just does less
//! (no LCM redaction, no memory injection, zero turn costs, a daemon that
//! reports itself unavailable).
//!
//! Only the composition root can fill them, and it must do so before any
//! transcript ingest, host installer, hook, or branch lock runs. That is what
//! [`register_runtime_ports`] is: the single, idempotent, root-owned wiring
//! call. Both process entry paths invoke it — `src/main.rs` for every CLI and
//! daemon invocation, and the daemon session-registry constructor for embedded
//! and integration-test runtimes that never pass through `main`.
//!
//! Every underlying `register` is `OnceLock::set`, so repeated calls are safe
//! and the first registration wins.

use std::future::Future;
use std::path::Path;
use std::pin::Pin;

use serde_json::Value;

use crate::errors::Result;

/// Installs every root-owned runtime port. Idempotent; first call wins.
///
/// Call this as early as possible in a process: the slots below are read by
/// transcript ingest, agent-host installers, hooks, and branch locking, all of
/// which fail quietly (or fail closed) when the root never registered.
pub fn register_runtime_ports() {
    register_session_ports();
    register_agent_host_ports();
    crate::agents::register_mcp_tool_catalog_ports();
    crate::automation::register_runtime_ports();
    crate::dashboard::register_runtime_ports();
    crate::branch::register_branch_admin_recovery_gate();
}

// ---------------------------------------------------------------------------
// tracedecay_sessions::host_ports
// ---------------------------------------------------------------------------

fn register_session_ports() {
    use tracedecay_sessions::host_ports;

    host_ports::lcm_redaction::register(lcm_redaction_policy);
    host_ports::hermes_profile_pin::register(
        tracedecay_agent_hosts::agents::hermes::read_config_pinned_project_root,
    );
    host_ports::session_review::register(crate::hooks::schedule_user_session_review);
    host_ports::unregistered_admission::register(unregistered_admission);
}

/// Owner-configured LCM redaction policy, read from the user profile.
///
/// Redaction is irreversible, so this is strictly opt-in: the profile default
/// is "disabled with no patterns", which reproduces the port's own unwired
/// default.
fn lcm_redaction_policy() -> tracedecay_sessions::host_ports::LcmRedactionPolicy {
    let config = crate::user_config::UserConfig::load();
    tracedecay_sessions::host_ports::LcmRedactionPolicy {
        enabled: config.lcm_sensitive_redaction_enabled,
        patterns: config.lcm_sensitive_redaction_patterns,
    }
}

/// Builds an admission facade with no durable authority behind it.
///
/// The standalone Codex entry points walk a rollout and count what they *would*
/// admit; every capture through this facade fails closed because no registered
/// database is attached.
fn unregistered_admission(
    scope: tracedecay_sessions::host_ports::unregistered_admission::Scope,
) -> Box<dyn tracedecay_sessions::admission::HostAdmission> {
    use crate::application::host_admission::{HostAdmissionAuthorities, HostAdmissionFacade};
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

    ports::pricing::register(crate::accounting::pricing::cost_of_turn);
    ports::hook_runtime::register_daemon_tool_invoker(daemon_tool_json);
    ports::hook_runtime::register_memory_injection_gate(
        crate::hooks::memory_inject::memory_injection_enabled,
    );
    ports::hook_runtime::register_cursor_catch_up_ingest_max_bytes(
        cursor_catch_up_ingest_max_bytes,
    );
}

/// Fn-pointer shim over the root's async daemon tool call.
///
/// The port is a plain `fn` returning a boxed future so the extracted crate
/// needs no async-trait machinery.
fn daemon_tool_json<'a>(
    project_root: Option<&'a Path>,
    tool_name: &'a str,
    arguments: Value,
) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>> {
    Box::pin(crate::hooks::daemon_tool_json(
        project_root,
        tool_name,
        arguments,
    ))
}

fn cursor_catch_up_ingest_max_bytes() -> u64 {
    crate::hooks::CURSOR_CATCH_UP_INGEST_MAX_BYTES
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
        ONCE.call_once(register_runtime_ports);
        pinned
    }

    #[test]
    fn lcm_redaction_policy_is_resolvable_after_registration() {
        let _pinned = registered();
        // Unwired, `resolve()` always answers the inert default and the owner's
        // opt-in is silently ignored — LCM raw payloads keep their secrets.
        // Write the opt-in into the pinned profile and require it to arrive.
        let path = crate::user_config::config_path().expect("pinned profile config path");
        std::fs::write(
            &path,
            "lcm_sensitive_redaction_enabled = true\n\
             lcm_sensitive_redaction_patterns = [\"authorization\"]\n",
        )
        .expect("write pinned user config");

        assert_eq!(
            tracedecay_sessions::host_ports::lcm_redaction::resolve(),
            tracedecay_sessions::host_ports::LcmRedactionPolicy {
                enabled: true,
                patterns: vec!["authorization".to_owned()],
            },
            "registered provider must carry the owner's redaction opt-in to LCM ingest"
        );
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

    #[test]
    fn memory_injection_gate_matches_the_root_reader() {
        let _pinned = registered();
        assert_eq!(
            tracedecay_agent_hosts::ports::hook_runtime::memory_injection_enabled(),
            crate::hooks::memory_inject::memory_injection_enabled(),
            "registered gate must back the memory-injection port"
        );
    }

    #[test]
    fn cursor_catch_up_ceiling_is_bounded_after_registration() {
        let _pinned = registered();
        let ceiling =
            tracedecay_agent_hosts::ports::hook_runtime::cursor_catch_up_ingest_max_bytes();
        assert_ne!(
            ceiling,
            u64::MAX,
            "an unwired ceiling reads as u64::MAX and silences the doctor check"
        );
        assert_eq!(ceiling, crate::hooks::CURSOR_CATCH_UP_INGEST_MAX_BYTES);
    }

    #[test]
    fn pricing_port_returns_the_root_price_table_answer() {
        let _pinned = registered();
        // Pick a model the bundled table prices; the port and the root reader
        // must agree, and a priced model must not read as zero.
        let model = "claude-sonnet-4-20250514";
        let port = tracedecay_agent_hosts::ports::pricing::cost_of_turn(model, 1_000_000, 0, 0, 0);
        let root = crate::accounting::pricing::cost_of_turn(model, 1_000_000, 0, 0, 0);
        assert!((port - root).abs() < f64::EPSILON);
        assert!(
            port > 0.0,
            "a priced model must not report a zero turn cost through the port"
        );
    }
}
